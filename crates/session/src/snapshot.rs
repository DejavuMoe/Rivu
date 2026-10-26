//! 会话关停快照（`system-design.md` §4.6「预热」第 1 步）：优雅关停时把全部会话（含别名 key）写进
//! `${RIVU_DATA_DIR}/sessions.snapshot`，启动时快照不超过 30 分钟、格式一致就直接加载。
//!
//! 文件格式（小端）：
//!
//! ```text
//! magic "RIVUSESS" | version u32 | layout_hash u64 | written_at_ms i64 | count u64
//! | count 个会话 | fnv1a64(以上全部字节) u64
//! ```
//!
//! 版本门控借鉴 Plausible（R02 §4.1）：`version` 或 `layout_hash`（[`LAYOUT`] 的哈希）不一致就不加载，
//! 宁可从 ClickHouse 重建，也不按旧结构解读。字段有任何增删改，要同时改 [`LAYOUT`]。
//! 文件先写到同目录的临时文件再改名，进程在写的中途退出不会留下半个快照。

use std::fmt;
use std::io::{self, Write};
use std::path::Path;

use rivu_domain::SessionAttrs;

use crate::assigner::{MAX_KEYS, SessionRecord};

const MAGIC: &[u8; 8] = b"RIVUSESS";
/// 格式版本。
pub const VERSION: u32 = 1;
/// 记录的结构描述，改字段时一并修改（它的哈希写进文件头）。
pub const LAYOUT: &str = "site_id:u64 session_id:u64 keys:[u64] started_at_ms:i64 last_seen_ms:i64 \
partition:u8 entry:opt<str> referrer:str referrer_source:str channel:str utm_source:str utm_medium:str \
utm_campaign:str utm_content:str utm_term:str click_id_param:str country_code:[u8;2] region_code:str \
city_geoname_id:u32 device:str browser:str browser_version:str os:str os_version:str";

/// 快照的默认文件名（放在 `RIVU_DATA_DIR` 下）。
pub const FILE_NAME: &str = "sessions.snapshot";

/// 快照最长可用 30 分钟：更旧的快照里的会话都已超时（§4.6）。
pub const MAX_AGE_MS: i64 = 30 * 60 * 1000;

const HEADER_LEN: usize = 8 + 4 + 8 + 8 + 8;

#[derive(Debug, PartialEq, Eq)]
pub enum SnapshotError {
    Io(String),
    BadMagic,
    Version(u32),
    Layout,
    Checksum,
    Truncated,
    /// 快照写于 `age_ms` 毫秒之前，超过了 [`MAX_AGE_MS`]（或时间在未来）。
    Stale {
        age_ms: i64,
    },
    Invalid(&'static str),
}

impl fmt::Display for SnapshotError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "读写快照失败：{e}"),
            Self::BadMagic => f.write_str("不是会话快照文件"),
            Self::Version(v) => write!(f, "快照格式版本 {v} 与当前版本 {VERSION} 不一致"),
            Self::Layout => f.write_str("快照的结构哈希与当前结构不一致"),
            Self::Checksum => f.write_str("快照校验和不符"),
            Self::Truncated => f.write_str("快照文件不完整"),
            Self::Stale { age_ms } => write!(f, "快照写于 {} 秒之前，已过期", age_ms / 1000),
            Self::Invalid(what) => write!(f, "快照内容非法：{what}"),
        }
    }
}

impl std::error::Error for SnapshotError {}

/// FNV-1a 64：结构哈希与文件校验和（只防截断与磁盘损坏，不防篡改：文件在本机数据目录里）。
fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

fn put_str(out: &mut Vec<u8>, s: &str) {
    out.extend_from_slice(&(s.len() as u32).to_le_bytes());
    out.extend_from_slice(s.as_bytes());
}

fn attr_strs(a: &SessionAttrs) -> [&str; 15] {
    [
        &a.referrer,
        &a.referrer_source,
        &a.channel,
        &a.utm_source,
        &a.utm_medium,
        &a.utm_campaign,
        &a.utm_content,
        &a.utm_term,
        &a.click_id_param,
        &a.region_code,
        &a.device,
        &a.browser,
        &a.browser_version,
        &a.os,
        &a.os_version,
    ]
}

/// 编码成文件内容。
pub fn encode(records: &[SessionRecord], written_at_ms: i64) -> Vec<u8> {
    let mut out = Vec::with_capacity(HEADER_LEN + records.len() * 160 + 8);
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&VERSION.to_le_bytes());
    out.extend_from_slice(&fnv1a64(LAYOUT.as_bytes()).to_le_bytes());
    out.extend_from_slice(&written_at_ms.to_le_bytes());
    out.extend_from_slice(&(records.len() as u64).to_le_bytes());
    for r in records {
        out.extend_from_slice(&r.site_id.to_le_bytes());
        out.extend_from_slice(&r.session_id.to_le_bytes());
        out.push(r.keys.len().min(MAX_KEYS) as u8);
        for k in r.keys.iter().take(MAX_KEYS) {
            out.extend_from_slice(&k.to_le_bytes());
        }
        out.extend_from_slice(&r.started_at_ms.to_le_bytes());
        out.extend_from_slice(&r.last_seen_ms.to_le_bytes());
        out.push(r.partition);
        match &r.entry {
            Some(p) => {
                out.push(1);
                put_str(&mut out, p);
            }
            None => out.push(0),
        }
        let a = &r.attrs;
        for (i, s) in attr_strs(a).into_iter().enumerate() {
            put_str(&mut out, s);
            // LAYOUT 的顺序：click_id_param 之后是 country_code，region_code 之后是 city_geoname_id
            if i == 8 {
                out.extend_from_slice(&a.country_code);
            }
            if i == 9 {
                out.extend_from_slice(&a.city_geoname_id.to_le_bytes());
            }
        }
    }
    let sum = fnv1a64(&out);
    out.extend_from_slice(&sum.to_le_bytes());
    out
}

struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], SnapshotError> {
        let end = self.pos.checked_add(n).ok_or(SnapshotError::Truncated)?;
        let s = self
            .buf
            .get(self.pos..end)
            .ok_or(SnapshotError::Truncated)?;
        self.pos = end;
        Ok(s)
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], SnapshotError> {
        let mut a = [0u8; N];
        a.copy_from_slice(self.take(N)?);
        Ok(a)
    }

    fn u8(&mut self) -> Result<u8, SnapshotError> {
        Ok(self.take(1)?[0])
    }

    fn u32(&mut self) -> Result<u32, SnapshotError> {
        Ok(u32::from_le_bytes(self.array()?))
    }

    fn u64(&mut self) -> Result<u64, SnapshotError> {
        Ok(u64::from_le_bytes(self.array()?))
    }

    fn i64(&mut self) -> Result<i64, SnapshotError> {
        Ok(i64::from_le_bytes(self.array()?))
    }

    fn string(&mut self) -> Result<String, SnapshotError> {
        let len = self.u32()? as usize;
        let bytes = self.take(len)?;
        String::from_utf8(bytes.to_vec()).map_err(|_| SnapshotError::Invalid("字符串不是 UTF-8"))
    }
}

/// 解码文件内容，返回 `(写入时刻, 会话)`。
pub fn decode(bytes: &[u8]) -> Result<(i64, Vec<SessionRecord>), SnapshotError> {
    if bytes.len() < HEADER_LEN + 8 {
        return Err(if bytes.starts_with(MAGIC) || bytes.len() < MAGIC.len() {
            SnapshotError::Truncated
        } else {
            SnapshotError::BadMagic
        });
    }
    if &bytes[..8] != MAGIC {
        return Err(SnapshotError::BadMagic);
    }
    let (body, sum) = bytes.split_at(bytes.len() - 8);
    let mut r = Reader { buf: body, pos: 8 };
    let version = r.u32()?;
    if version != VERSION {
        return Err(SnapshotError::Version(version));
    }
    if r.u64()? != fnv1a64(LAYOUT.as_bytes()) {
        return Err(SnapshotError::Layout);
    }
    let mut sum_bytes = [0u8; 8];
    sum_bytes.copy_from_slice(sum);
    if u64::from_le_bytes(sum_bytes) != fnv1a64(body) {
        return Err(SnapshotError::Checksum);
    }
    let written_at = r.i64()?;
    let count = r.u64()?;
    // 每条记录至少 50 字节：数量与文件大小对不上就是坏文件，不按它预分配。
    if count > (body.len() / 50) as u64 {
        return Err(SnapshotError::Invalid("会话数与文件大小不符"));
    }
    let mut records = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let site_id = r.u64()?;
        let session_id = r.u64()?;
        let nkeys = usize::from(r.u8()?);
        if nkeys == 0 || nkeys > MAX_KEYS {
            return Err(SnapshotError::Invalid("key 个数"));
        }
        let keys = (0..nkeys).map(|_| r.u64()).collect::<Result<Vec<_>, _>>()?;
        let started_at_ms = r.i64()?;
        let last_seen_ms = r.i64()?;
        let partition = r.u8()?;
        let entry = match r.u8()? {
            0 => None,
            1 => Some(r.string()?),
            _ => return Err(SnapshotError::Invalid("入口页标记")),
        };
        let mut s = || r.string();
        let mut a = SessionAttrs {
            referrer: s()?,
            referrer_source: s()?,
            channel: s()?,
            utm_source: s()?,
            utm_medium: s()?,
            utm_campaign: s()?,
            utm_content: s()?,
            utm_term: s()?,
            click_id_param: s()?,
            ..SessionAttrs::default()
        };
        a.country_code = r.array()?;
        a.region_code = r.string()?;
        a.city_geoname_id = r.u32()?;
        a.device = r.string()?;
        a.browser = r.string()?;
        a.browser_version = r.string()?;
        a.os = r.string()?;
        a.os_version = r.string()?;
        records.push(SessionRecord {
            site_id,
            session_id,
            keys,
            started_at_ms,
            last_seen_ms,
            entry,
            attrs: a,
            partition,
        });
    }
    if r.pos != body.len() {
        return Err(SnapshotError::Invalid("记录之后还有多余的字节"));
    }
    Ok((written_at, records))
}

/// 写快照：先写同目录的 `<文件名>.tmp` 并 fsync，再改名覆盖。
pub fn write_file(
    path: &Path,
    records: &[SessionRecord],
    written_at_ms: i64,
) -> Result<(), SnapshotError> {
    let io = |e: io::Error| SnapshotError::Io(e.to_string());
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(io)?;
    }
    let tmp = path.with_extension("snapshot.tmp");
    {
        let mut f = std::fs::File::create(&tmp).map_err(io)?;
        f.write_all(&encode(records, written_at_ms)).map_err(io)?;
        f.sync_all().map_err(io)?;
    }
    std::fs::rename(&tmp, path).map_err(io)
}

/// 读快照：文件不存在返回 `Ok(None)`；过期（超过 [`MAX_AGE_MS`] 或写入时刻在未来 1 分钟以上）、
/// 格式不一致或损坏返回错误。不删除文件，由调用方处理。
pub fn read_file(path: &Path, now_ms: i64) -> Result<Option<Vec<SessionRecord>>, SnapshotError> {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(SnapshotError::Io(e.to_string())),
    };
    let (written_at, records) = decode(&bytes)?;
    let age_ms = now_ms.saturating_sub(written_at);
    if !(-60_000..=MAX_AGE_MS).contains(&age_ms) {
        return Err(SnapshotError::Stale { age_ms });
    }
    Ok(Some(records))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Vec<SessionRecord> {
        vec![
            SessionRecord {
                site_id: 7,
                session_id: 0xdead_beef,
                keys: vec![1, 2, 3],
                started_at_ms: 1_000,
                last_seen_ms: 5_000,
                entry: Some("/文档".into()),
                attrs: SessionAttrs {
                    referrer: "news.example.org/x".into(),
                    referrer_source: "Hacker News".into(),
                    channel: "Referral".into(),
                    utm_source: "hn".into(),
                    utm_term: "rust".into(),
                    click_id_param: "gclid".into(),
                    country_code: *b"CN",
                    region_code: "CN-31".into(),
                    city_geoname_id: 1_796_236,
                    device: "desktop".into(),
                    browser: "Firefox".into(),
                    browser_version: "140.0".into(),
                    os: "Linux".into(),
                    os_version: String::new(),
                    ..SessionAttrs::default()
                },
                partition: 31,
            },
            SessionRecord {
                site_id: 8,
                session_id: 42,
                keys: vec![9],
                started_at_ms: -1,
                last_seen_ms: i64::MAX,
                entry: None,
                attrs: SessionAttrs::default(),
                partition: 0,
            },
        ]
    }

    #[test]
    fn roundtrip_keeps_every_field() {
        let bytes = encode(&sample(), 123);
        assert_eq!(decode(&bytes).unwrap(), (123, sample()));
        assert_eq!(decode(&encode(&[], 5)).unwrap(), (5, vec![]));
    }

    #[test]
    fn rejects_other_versions_layouts_and_damage() {
        let good = encode(&sample(), 1);
        let mut v2 = good.clone();
        v2[8] = 2;
        assert_eq!(decode(&v2), Err(SnapshotError::Version(2)));
        let mut layout = good.clone();
        layout[12] ^= 1;
        assert_eq!(decode(&layout), Err(SnapshotError::Layout));
        let mut flipped = good.clone();
        let mid = good.len() / 2;
        flipped[mid] ^= 0x40;
        assert_eq!(decode(&flipped), Err(SnapshotError::Checksum));
        assert_eq!(
            decode(b"not a snapshot at all, definitely not"),
            Err(SnapshotError::BadMagic)
        );
        assert_eq!(decode(&good[..20]), Err(SnapshotError::Truncated));
        // 截掉一半：校验和对不上
        assert!(decode(&good[..good.len() / 2]).is_err());
    }

    #[test]
    fn file_roundtrip_and_staleness() {
        let dir = std::env::temp_dir().join(format!("rivu-snap-{}", std::process::id()));
        let path = dir.join(FILE_NAME);
        assert_eq!(read_file(&path, 0).unwrap(), None, "没有文件");
        write_file(&path, &sample(), 1_000_000).unwrap();
        assert!(!path.with_extension("snapshot.tmp").exists());
        assert_eq!(
            read_file(&path, 1_000_000 + MAX_AGE_MS).unwrap(),
            Some(sample())
        );
        assert_eq!(
            read_file(&path, 1_000_000 + MAX_AGE_MS + 1),
            Err(SnapshotError::Stale {
                age_ms: MAX_AGE_MS + 1
            })
        );
        assert!(matches!(
            read_file(&path, 1_000_000 - 120_000),
            Err(SnapshotError::Stale { .. })
        ));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
