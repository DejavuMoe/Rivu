//! `datacenter_refresh`：下载云厂商的网段清单，合并成 `${RIVU_DATA_DIR}/datacenter/ranges.txt`
//! （[`rivu_geo::datacenter::refresh`]）。只在 `RIVU_DATACENTER_FILTER=on` 时运行；collector 按文件修改时间自己热加载。
//!
//! 调度：本地文件不存在或已超过一个周期（24 小时）就立即下载，否则在「文件修改时间 + 周期」时下载，
//! 频繁重启不会每次都去请求各家的清单。失败 1 小时后重试，期间沿用旧文件。

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use rivu_common::config::DatacenterConfig;
use rivu_geo::HttpClient;
use rivu_geo::datacenter::{DcDb, refresh};
use tokio::sync::watch;
use tokio::task::JoinHandle;

/// 失败后的重试间隔。
const RETRY_AFTER: Duration = Duration::from_secs(3600);

/// 距下一次下载还要等多久：文件不存在、读不到修改时间或已过期为 0。
pub fn first_delay(path: &Path, interval: Duration, now: SystemTime) -> Duration {
    let Some(mtime) = std::fs::metadata(path).ok().and_then(|m| m.modified().ok()) else {
        return Duration::ZERO;
    };
    let age = now.duration_since(mtime).unwrap_or(Duration::ZERO);
    interval.saturating_sub(age)
}

/// 启动 `datacenter_refresh`。`db` 是本进程持有的网段表（`all` 角色），下载成功后立即热加载。
pub fn spawn_datacenter_refresh(
    cfg: DatacenterConfig,
    db: Option<Arc<DcDb>>,
    mut stop: watch::Receiver<bool>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let http = match HttpClient::new() {
            Ok(h) => h,
            Err(e) => {
                tracing::error!(error = %format!("{e:#}"), "datacenter_refresh 无法创建 HTTP 客户端，任务退出");
                return;
            }
        };
        let mut wait = first_delay(
            &cfg.path,
            DatacenterConfig::REFRESH_INTERVAL,
            SystemTime::now(),
        );
        loop {
            tokio::select! {
                () = async { let _ = stop.wait_for(|s| *s).await; } => break,
                () = tokio::time::sleep(wait) => {}
            }
            wait = match refresh(&http, &cfg.sources, &cfg.path).await {
                Ok(r) => {
                    tracing::info!(prefixes = r.prefixes, per_source = ?r.per_source, "datacenter_refresh 完成");
                    metrics::counter!("rivu_datacenter_refresh_total", "result" => "updated")
                        .increment(1);
                    if let Some(db) = &db {
                        let (db, path) = (Arc::clone(db), cfg.path.clone());
                        if let Ok(Err(e)) =
                            tokio::task::spawn_blocking(move || db.reload_if_changed(&path)).await
                        {
                            tracing::warn!(error = %format!("{e:#}"), "加载新下载的数据中心网段失败");
                        }
                    }
                    DatacenterConfig::REFRESH_INTERVAL
                }
                Err(e) => {
                    tracing::warn!(error = %format!("{e:#}"), "datacenter_refresh 失败，1 小时后重试，期间沿用旧清单");
                    metrics::counter!("rivu_datacenter_refresh_total", "result" => "failed")
                        .increment(1);
                    RETRY_AFTER
                }
            };
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_delay_waits_for_the_rest_of_the_interval() {
        let dir = std::env::temp_dir().join(format!("rivu-dcjob-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("ranges.txt");
        let day = Duration::from_secs(86_400);
        assert_eq!(
            first_delay(&path, day, SystemTime::now()),
            Duration::ZERO,
            "没有文件"
        );
        std::fs::write(&path, "1.2.3.0/24\n").unwrap();
        let mtime = std::fs::metadata(&path).unwrap().modified().unwrap();
        let d = first_delay(&path, day, mtime + Duration::from_secs(3600));
        assert_eq!(d, Duration::from_secs(23 * 3600));
        assert_eq!(
            first_delay(&path, day, mtime + 2 * day),
            Duration::ZERO,
            "过期"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }
}
