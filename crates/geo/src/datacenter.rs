//! 数据中心网段（`system-design.md` §4.2 第 5 步「可选数据中心 CIDR 表」，丢弃原因 `datacenter_ip`）。
//!
//! 云厂商公开的机器可读网段清单（`brief-p1.md`「原始素材」）：AWS `ip-ranges.json`、Google Cloud `cloud.json`、
//! Oracle Cloud `public_ip_ranges.json`、DigitalOcean 与 Linode 的 geofeed（RFC 8805 CSV）。Azure 的 Service Tags
//! 文件名每周变化、没有固定地址，不在默认清单里，需要时用 `RIVU_DATACENTER_SOURCES` 指向自己镜像的文件。
//! 默认清单见 `rivu_common::config::DatacenterConfig::DEFAULT_SOURCES`。
//!
//! - [`parse_list`]：自动识别格式。JSON 取所有形如 `地址/前缀` 的字符串值；文本与 CSV 取每行第一个字段；
//!   `#` 开头的行与无法解析的行跳过。
//! - [`DcRanges`]：把前缀转换成排好序、合并过的区间，二分查找；IPv4 映射的 IPv6 地址按 IPv4 查。
//! - [`DcDb`]：`ArcSwap` 持有当前的区间集合，按文件修改时间热加载；未加载时一律「不是数据中心」。
//! - [`refresh`]：下载全部来源，**全部成功**才原子替换本地文件（部分来源失败时保留上一份完整的清单）。
//!
//! 默认关闭（`RIVU_DATACENTER_FILTER=off`）：不少 VPN 与企业代理的出口在云上，打开后这部分真实访问会被丢弃。

use std::fmt::Write as _;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result, ensure};
use arc_swap::ArcSwapOption;
use parking_lot::Mutex;
use rivu_common::config::redact_url;
use serde::Serialize;
use tokio::sync::watch;
use tokio::task::JoinHandle;

use crate::refresh::HttpClient;

/// 单个来源的下载上限：AWS 的 JSON 约 2 MB，留足余量。
pub const MAX_SOURCE_BYTES: u64 = 64 << 20;

/// 一条网段。
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Prefix {
    pub addr: IpAddr,
    pub len: u8,
}

impl Prefix {
    /// 解析 `地址/前缀`，主机位清零。没有 `/` 的单个地址不算（清单里的单地址极少，且容易误把别的字段当成地址）。
    pub fn parse(s: &str) -> Option<Prefix> {
        let (addr, len) = s.trim().split_once('/')?;
        let addr: IpAddr = addr.parse().ok()?;
        let len: u8 = len.parse().ok()?;
        let max = if addr.is_ipv4() { 32 } else { 128 };
        if len > max {
            return None;
        }
        let addr = match addr {
            IpAddr::V4(a) => IpAddr::V4(Ipv4Addr::from(u32::from(a) & mask32(len))),
            IpAddr::V6(a) => IpAddr::V6(Ipv6Addr::from(u128::from(a) & mask128(len))),
        };
        Some(Prefix { addr, len })
    }
}

impl std::fmt::Display for Prefix {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", self.addr, self.len)
    }
}

fn mask32(len: u8) -> u32 {
    if len == 0 {
        0
    } else {
        u32::MAX << (32 - u32::from(len))
    }
}

fn mask128(len: u8) -> u128 {
    if len == 0 {
        0
    } else {
        u128::MAX << (128 - u32::from(len))
    }
}

/// 从一个来源的内容里取出全部网段。返回 `(网段, 跳过的行数)`；JSON 不计跳过。
pub fn parse_list(bytes: &[u8]) -> (Vec<Prefix>, usize) {
    let text = String::from_utf8_lossy(bytes);
    let trimmed = text.trim_start_matches('\u{feff}').trim_start();
    if (trimmed.starts_with('{') || trimmed.starts_with('['))
        && let Ok(v) = serde_json::from_str::<serde_json::Value>(trimmed)
    {
        let mut out = Vec::new();
        walk_json(&v, &mut out);
        return (out, 0);
    }
    let mut out = Vec::new();
    let mut skipped = 0;
    for line in trimmed.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let first = line
            .split(|c: char| c == ',' || c.is_whitespace())
            .next()
            .unwrap_or("");
        match Prefix::parse(first) {
            Some(p) => out.push(p),
            None => skipped += 1,
        }
    }
    (out, skipped)
}

fn walk_json(v: &serde_json::Value, out: &mut Vec<Prefix>) {
    match v {
        serde_json::Value::String(s) if s.contains('/') => {
            if let Some(p) = Prefix::parse(s) {
                out.push(p);
            }
        }
        serde_json::Value::Array(a) => a.iter().for_each(|x| walk_json(x, out)),
        serde_json::Value::Object(o) => o.values().for_each(|x| walk_json(x, out)),
        _ => {}
    }
}

/// 合并后的区间集合（闭区间），二分查找。
#[derive(Clone, Debug, Default)]
pub struct DcRanges {
    v4: Vec<(u32, u32)>,
    v6: Vec<(u128, u128)>,
    prefixes: usize,
}

fn merge<T: Ord + Copy>(mut r: Vec<(T, T)>, next: impl Fn(T) -> Option<T>) -> Vec<(T, T)> {
    r.sort_unstable();
    let mut out: Vec<(T, T)> = Vec::with_capacity(r.len());
    for (s, e) in r {
        if let Some(last) = out.last_mut() {
            // 重叠或紧挨着（上一段的终点 + 1 == 这一段的起点）就合并
            if s <= last.1 || next(last.1) == Some(s) {
                if e > last.1 {
                    last.1 = e;
                }
                continue;
            }
        }
        out.push((s, e));
    }
    out
}

fn find<T: Ord + Copy>(ranges: &[(T, T)], x: T) -> bool {
    // 最后一个起点 <= x 的区间
    let i = ranges.partition_point(|&(s, _)| s <= x);
    i > 0 && x <= ranges[i - 1].1
}

impl DcRanges {
    pub fn from_prefixes(prefixes: &[Prefix]) -> DcRanges {
        let mut v4 = Vec::new();
        let mut v6 = Vec::new();
        for p in prefixes {
            match p.addr {
                IpAddr::V4(a) => {
                    let s = u32::from(a);
                    v4.push((s, s | !mask32(p.len)));
                }
                IpAddr::V6(a) => {
                    let s = u128::from(a);
                    v6.push((s, s | !mask128(p.len)));
                }
            }
        }
        DcRanges {
            v4: merge(v4, |x| x.checked_add(1)),
            v6: merge(v6, |x| x.checked_add(1)),
            prefixes: prefixes.len(),
        }
    }

    pub fn contains(&self, ip: IpAddr) -> bool {
        match ip {
            IpAddr::V4(a) => find(&self.v4, u32::from(a)),
            IpAddr::V6(a) => match a.to_ipv4_mapped() {
                Some(v4) => find(&self.v4, u32::from(v4)),
                None => find(&self.v6, u128::from(a)),
            },
        }
    }

    /// 来源里的网段条数（去重前）。
    pub fn prefixes(&self) -> usize {
        self.prefixes
    }

    /// 合并后的区间数 `(IPv4, IPv6)`。
    pub fn ranges(&self) -> (usize, usize) {
        (self.v4.len(), self.v6.len())
    }
}

/// `/readyz` 报告用的状态。
#[derive(Clone, Debug, Default, Serialize)]
pub struct DcStatus {
    pub loaded: bool,
    pub prefixes: usize,
    /// 文件头里记录的下载时间（RFC 3339）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fetched_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
}

/// 当前生效的数据中心网段，热加载。
#[derive(Default)]
pub struct DcDb {
    ranges: ArcSwapOption<DcRanges>,
    status: Mutex<DcStatus>,
    loaded_mtime: Mutex<Option<SystemTime>>,
}

impl DcDb {
    pub fn new() -> DcDb {
        DcDb::default()
    }

    /// 未加载时恒为 false（宁可放过，不误杀）。
    pub fn contains(&self, ip: IpAddr) -> bool {
        self.ranges.load().as_ref().is_some_and(|r| r.contains(ip))
    }

    pub fn status(&self) -> DcStatus {
        self.status.lock().clone()
    }

    /// 直接换上一组网段（测试与装配用）。
    pub fn replace(&self, ranges: DcRanges, fetched_at: Option<String>) {
        let mut st = self.status.lock();
        st.loaded = true;
        st.prefixes = ranges.prefixes();
        st.fetched_at = fetched_at;
        st.last_error = None;
        self.ranges.store(Some(Arc::new(ranges)));
    }

    /// 文件修改时间变了（或从没加载过）就重新加载；返回是否加载了。文件不存在返回 `Ok(false)`。
    pub fn reload_if_changed(&self, path: &Path) -> Result<bool> {
        let mtime = match std::fs::metadata(path) {
            Ok(m) => m.modified().ok(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(e) => return Err(e).with_context(|| format!("读取 {} 失败", path.display())),
        };
        if mtime.is_some() && *self.loaded_mtime.lock() == mtime {
            return Ok(false);
        }
        let result = std::fs::read(path)
            .with_context(|| format!("读取 {} 失败", path.display()))
            .and_then(|bytes| {
                let (prefixes, _) = parse_list(&bytes);
                ensure!(!prefixes.is_empty(), "{} 里没有网段", path.display());
                Ok((prefixes, fetched_at(&bytes)))
            });
        match result {
            Ok((prefixes, fetched)) => {
                self.replace(DcRanges::from_prefixes(&prefixes), fetched);
                *self.loaded_mtime.lock() = mtime;
                Ok(true)
            }
            Err(e) => {
                self.status.lock().last_error = Some(format!("{e:#}"));
                Err(e)
            }
        }
    }

    /// 每 `interval` 检查一次文件修改时间并热加载（第一次立即检查）。
    pub fn spawn_reloader(
        self: &Arc<Self>,
        path: PathBuf,
        interval: Duration,
        mut stop: watch::Receiver<bool>,
    ) -> JoinHandle<()> {
        let db = Arc::clone(self);
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(interval);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                tokio::select! {
                    () = async { let _ = stop.wait_for(|s| *s).await; } => break,
                    _ = ticker.tick() => {
                        let (worker, path) = (Arc::clone(&db), path.clone());
                        match tokio::task::spawn_blocking(move || worker.reload_if_changed(&path)).await {
                            Ok(Ok(true)) => tracing::info!(prefixes = db.status().prefixes, "数据中心网段已加载"),
                            Ok(Ok(false)) => {}
                            Ok(Err(e)) => tracing::warn!(error = %format!("{e:#}"), "数据中心网段加载失败，继续使用旧清单"),
                            Err(e) => tracing::error!(error = %e, "数据中心网段热加载任务异常"),
                        }
                    }
                }
            }
        })
    }
}

const FETCHED_AT: &str = "# fetched_at: ";

fn fetched_at(bytes: &[u8]) -> Option<String> {
    String::from_utf8_lossy(bytes)
        .lines()
        .take_while(|l| l.starts_with('#'))
        .find_map(|l| l.strip_prefix(FETCHED_AT).map(str::to_owned))
}

/// 一次刷新的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefreshReport {
    /// 去重后的网段数。
    pub prefixes: usize,
    /// 每个来源取到的网段数，顺序同 `sources`。
    pub per_source: Vec<usize>,
}

/// 下载全部来源，合并去重后原子写入 `path`。任何一个来源失败或没有取到网段都中止，保留旧文件。
pub async fn refresh(http: &HttpClient, sources: &[String], path: &Path) -> Result<RefreshReport> {
    ensure!(!sources.is_empty(), "没有配置数据中心网段的来源");
    let mut all = Vec::new();
    let mut per_source = Vec::with_capacity(sources.len());
    let mut header = String::from("# Rivu 数据中心网段：datacenter_refresh 生成，不要手工编辑。\n");
    let _ = writeln!(header, "{FETCHED_AT}{}", jiff::Timestamp::now());
    for url in sources {
        let bytes = http.get_bytes(url, MAX_SOURCE_BYTES).await?;
        let (prefixes, skipped) = parse_list(&bytes);
        ensure!(
            !prefixes.is_empty(),
            "{} 里没有可识别的网段（跳过了 {skipped} 行）",
            redact_url(url)
        );
        let _ = writeln!(
            header,
            "# source: {} prefixes={}",
            redact_url(url),
            prefixes.len()
        );
        per_source.push(prefixes.len());
        all.extend(prefixes);
    }
    all.sort_unstable();
    all.dedup();
    let mut body = header;
    for p in &all {
        let _ = writeln!(body, "{p}");
    }
    let dir = path.parent().context("数据中心网段文件没有上级目录")?;
    tokio::fs::create_dir_all(dir)
        .await
        .with_context(|| format!("创建 {} 失败", dir.display()))?;
    let tmp = path.with_extension(format!("{}.tmp", std::process::id()));
    tokio::fs::write(&tmp, body.as_bytes())
        .await
        .with_context(|| format!("写入 {} 失败", tmp.display()))?;
    if let Err(e) = tokio::fs::rename(&tmp, path).await {
        let _ = tokio::fs::remove_file(&tmp).await;
        return Err(e).with_context(|| format!("替换 {} 失败", path.display()));
    }
    Ok(RefreshReport {
        prefixes: all.len(),
        per_source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn prefix_parse_masks_host_bits_and_rejects_junk() {
        assert_eq!(
            Prefix::parse("10.1.2.3/8").unwrap().to_string(),
            "10.0.0.0/8"
        );
        assert_eq!(
            Prefix::parse(" 2600:1f18::1/32 ").unwrap().to_string(),
            "2600:1f18::/32"
        );
        assert_eq!(Prefix::parse("0.0.0.0/0").unwrap().len, 0);
        for bad in [
            "10.0.0.0",
            "10.0.0.0/33",
            "::/129",
            "us-east-1",
            "a/b",
            "10.0.0.0/-1",
        ] {
            assert!(Prefix::parse(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn parses_aws_gcp_oracle_json_and_geofeed_csv() {
        let aws = br#"{"syncToken":"1","createDate":"2026-10-01-00-00-00",
            "prefixes":[{"ip_prefix":"3.5.140.0/22","region":"ap-northeast-2","service":"AMAZON","network_border_group":"ap-northeast-2"}],
            "ipv6_prefixes":[{"ipv6_prefix":"2600:1f14::/35","region":"us-west-2","service":"EC2"}]}"#;
        let (p, _) = parse_list(aws);
        assert_eq!(p.len(), 2);
        let gcp = br#"{"prefixes":[{"ipv4Prefix":"34.1.208.0/20","service":"Google Cloud","scope":"africa-south1"},{"ipv6Prefix":"2600:1900:8000::/44"}]}"#;
        assert_eq!(parse_list(gcp).0.len(), 2);
        let oracle = br#"{"regions":[{"region":"us-phoenix-1","cidrs":[{"cidr":"129.146.0.0/21","tags":["OCI"]}]}]}"#;
        assert_eq!(
            parse_list(oracle).0,
            vec![Prefix::parse("129.146.0.0/21").unwrap()]
        );
        let geofeed = "# comment\n5.101.96.0/21,NL,NL-NH,Amsterdam,\n2a03:b0c0::/32,NL,,,\nnot-a-cidr,US,,,\n\n";
        let (p, skipped) = parse_list(geofeed.as_bytes());
        assert_eq!((p.len(), skipped), (2, 1));
        let plain = b"\xef\xbb\xbf1.2.3.0/24\n1.2.4.0/24 trailing\n";
        assert_eq!(parse_list(plain).0.len(), 2);
    }

    #[test]
    fn ranges_merge_and_lookup_at_the_edges() {
        let ps: Vec<Prefix> = [
            "10.0.0.0/24",
            "10.0.1.0/24",   // 紧挨着：合并
            "10.0.0.128/25", // 被包含
            "192.168.0.0/16",
            "2001:db8::/32",
            "255.255.255.255/32",
        ]
        .iter()
        .map(|s| Prefix::parse(s).unwrap())
        .collect();
        let r = DcRanges::from_prefixes(&ps);
        assert_eq!(r.ranges(), (3, 1));
        assert_eq!(r.prefixes(), 6);
        for yes in [
            "10.0.0.0",
            "10.0.1.255",
            "192.168.255.255",
            "255.255.255.255",
            "2001:db8:ffff::1",
            "::ffff:10.0.0.5",
        ] {
            assert!(r.contains(ip(yes)), "{yes}");
        }
        for no in [
            "9.255.255.255",
            "10.0.2.0",
            "192.169.0.0",
            "2001:db9::",
            "::1",
            "0.0.0.0",
        ] {
            assert!(!r.contains(ip(no)), "{no}");
        }
        assert!(
            DcRanges::from_prefixes(&[Prefix::parse("0.0.0.0/0").unwrap()]).contains(ip("8.8.8.8"))
        );
    }

    #[test]
    fn db_is_empty_until_loaded_and_hot_reloads_by_mtime() {
        let dir = std::env::temp_dir().join(format!("rivu-dc-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("ranges.txt");
        let db = DcDb::new();
        assert!(!db.contains(ip("3.5.140.1")));
        assert!(!db.reload_if_changed(&path).unwrap(), "文件不存在");
        std::fs::write(
            &path,
            format!("{FETCHED_AT}2026-10-27T00:00:00Z\n3.5.140.0/22\n"),
        )
        .unwrap();
        assert!(db.reload_if_changed(&path).unwrap());
        assert!(db.contains(ip("3.5.140.1")));
        assert_eq!(
            db.status().fetched_at.as_deref(),
            Some("2026-10-27T00:00:00Z")
        );
        assert!(!db.reload_if_changed(&path).unwrap(), "没变");
        // 坏文件：保留旧清单，记录错误
        std::thread::sleep(Duration::from_millis(20));
        std::fs::write(&path, "# empty\n").unwrap();
        let _ = filetime_bump(&path);
        assert!(db.reload_if_changed(&path).is_err());
        assert!(db.contains(ip("3.5.140.1")));
        assert!(db.status().last_error.is_some());
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// 某些文件系统的修改时间精度只有秒：把修改时间往后挪，保证「变了」。
    fn filetime_bump(path: &Path) -> std::io::Result<()> {
        let f = std::fs::File::options().write(true).open(path)?;
        f.set_modified(SystemTime::now() + Duration::from_secs(5))
    }
}
