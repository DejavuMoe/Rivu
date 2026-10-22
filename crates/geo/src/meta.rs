//! `<库文件名>.meta.json`：记录库的来源与校验头（`system-design.md` §4.4「文件与元数据」）。
//!
//! meta 里不放任何凭据：来源标识里的 URL 已去掉用户信息和查询串。

use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::attribution::StoredAttribution;

/// meta 文件名后缀。
pub const META_SUFFIX: &str = ".meta.json";

/// 库文件旁边的 meta 路径：`geoip.mmdb` -> `geoip.mmdb.meta.json`。
pub fn meta_path(db_path: &Path) -> PathBuf {
    let mut name = db_path.file_name().unwrap_or_default().to_os_string();
    name.push(META_SUFFIX);
    db_path.with_file_name(name)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Meta {
    /// 来源类别：`dbip-lite` | `maxmind-geolite2` | `custom`
    pub source: String,
    /// 来源标识（含版本或去凭据的 URL）。与当前配置不一致时视为没有本地副本。
    pub source_id: String,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    /// 解压后的 mmdb 内容的 SHA-256（小写十六进制）。
    pub sha256: String,
    /// RFC 3339，UTC。
    pub downloaded_at: String,
    /// 最近一次与服务器确认（200 或 304）的时间。
    pub checked_at: String,
    pub build_epoch: u64,
    pub database_type: String,
    pub attribution: Option<StoredAttribution>,
}

/// 读取 meta；不存在、读不出或解析失败都返回 `None`（等同于手工放置的文件）。
pub fn read(db_path: &Path) -> Option<Meta> {
    let bytes = std::fs::read(meta_path(db_path)).ok()?;
    match serde_json::from_slice(&bytes) {
        Ok(m) => Some(m),
        Err(e) => {
            tracing::warn!(error = %e, "geoip meta.json 无法解析，按没有 meta 处理");
            None
        }
    }
}

/// 原子写 meta：同目录临时文件，fsync 后 rename 覆盖。
pub fn write(db_path: &Path, meta: &Meta) -> Result<()> {
    let target = meta_path(db_path);
    let mut tmp_name = target.file_name().unwrap_or_default().to_os_string();
    tmp_name.push(format!(".{}.tmp", std::process::id()));
    let tmp = target.with_file_name(tmp_name);
    let result = (|| -> Result<()> {
        let mut f = std::fs::File::create(&tmp).context("创建 meta 临时文件失败")?;
        f.write_all(&serde_json::to_vec_pretty(meta)?)?;
        f.write_all(b"\n")?;
        f.sync_all().context("fsync meta 失败")?;
        drop(f);
        std::fs::rename(&tmp, &target).context("替换 meta 失败")?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Meta {
        Meta {
            source: "dbip-lite".into(),
            source_id: "dbip-lite:country".into(),
            etag: Some("\"abc\"".into()),
            last_modified: Some("Thu, 01 Oct 2026 06:29:26 GMT".into()),
            sha256: "00".repeat(32),
            downloaded_at: "2026-10-22T00:00:00Z".into(),
            checked_at: "2026-10-22T00:00:00Z".into(),
            build_epoch: 1_790_000_000,
            database_type: "DBIP-Country-Lite".into(),
            attribution: None,
        }
    }

    #[test]
    fn path_sits_next_to_the_database() {
        assert_eq!(
            meta_path(Path::new("/var/lib/rivu/geoip/geoip.mmdb")),
            Path::new("/var/lib/rivu/geoip/geoip.mmdb.meta.json")
        );
    }

    #[test]
    fn write_then_read_roundtrip_and_overwrite() {
        let dir = std::env::temp_dir().join(format!("rivu-geo-meta-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("geoip.mmdb");
        assert_eq!(read(&db), None);
        let m = sample();
        write(&db, &m).unwrap();
        assert_eq!(read(&db), Some(m.clone()));
        let mut m2 = m;
        m2.etag = None;
        write(&db, &m2).unwrap();
        assert_eq!(read(&db), Some(m2));
        // 垃圾内容按没有 meta 处理
        std::fs::write(meta_path(&db), b"{not json").unwrap();
        assert_eq!(read(&db), None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
