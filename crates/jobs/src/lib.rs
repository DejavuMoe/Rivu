//! 后台任务：`salts_rotate`、`geoip_refresh`、`ch_delete_batch`（[`stats_delete`]）、`rollup_build`（[`rollup`]）与 `ch_monitor`（[`ch_monitor`]），
//! 直接在进程内定时执行（`platform-and-ops.md` §6）。
//!
//! 写盐需要 PG 写权限，所以只有 `worker` 和 `all` 角色运行（`Role::runs_jobs`），collector 不运行。
//! `geoip_refresh` 见 [`geoip`]：下载并原子替换 GeoIP 库，collector 只读共享目录、自己热加载。
//! `datacenter_refresh` 见 [`datacenter`]：同样的方式维护数据中心网段清单（只在开启过滤时运行）。

pub mod ch_monitor;
pub mod datacenter;
pub mod geoip;
pub mod rollup;
pub mod stats_delete;

pub use ch_monitor::spawn_ch_monitor;
pub use datacenter::spawn_datacenter_refresh;
pub use geoip::{GeoRefreshJob, GeoRefreshSnapshot, GeoRefreshState, spawn_geoip_refresh};
pub use rollup::spawn_rollup_build;
pub use stats_delete::{StatsDeleteKick, spawn_stats_delete};

use std::time::Duration;

use anyhow::Result;
use jiff::civil::Date;
use jiff::{Span, Timestamp};
use sqlx::PgPool;
use tokio::sync::watch;
use tokio::task::JoinHandle;

/// `salts_rotate` 的执行周期：每小时。盐只需要「提前一天存在」，所以不必精确地卡在 00:05 UTC。
pub const SALTS_ROTATE_INTERVAL: Duration = Duration::from_secs(3600);

/// 一次 `salts_rotate` 的结果。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RotateReport {
    /// 新生成的盐的天数。
    pub created: u64,
    /// 删除的盐的行数。
    pub deleted: u64,
}

fn shift(day: Date, days: i64) -> Date {
    day.saturating_add(Span::new().days(days))
}

/// 必须存在的盐：UTC 的 `today-1`、`today`、`today+1`。
///
/// 站点本地日期相对 UTC 日期在 −1 到 +1 天内（`platform-and-ops.md` §6）。
pub fn required_days(today_utc: Date) -> [Date; 3] {
    [shift(today_utc, -1), today_utc, shift(today_utc, 1)]
}

/// 早于这一天的盐可以删除：`today-2`（多留一天覆盖迟到事件与 `prev_visitor_id`）。
pub fn keep_from(today_utc: Date) -> Date {
    shift(today_utc, -2)
}

/// 当前的 UTC 日期。
pub fn utc_today() -> Date {
    Timestamp::now().to_zoned(jiff::tz::TimeZone::UTC).date()
}

/// `salts_rotate`：确保 `today-1`、`today`、`today+1` 的盐存在，删除 `day < today-2` 的盐。幂等。
pub async fn salts_rotate(pool: &PgPool, today_utc: Date) -> Result<RotateReport> {
    let created = rivu_storage_pg::salts::ensure_salts(pool, &required_days(today_utc)).await?;
    let deleted = rivu_storage_pg::salts::delete_salts_before(pool, keep_from(today_utc)).await?;
    Ok(RotateReport { created, deleted })
}

/// 启动 `salts_rotate` 的定时任务：每 [`SALTS_ROTATE_INTERVAL`] 一次，直到收到停止信号。
/// `run_now` 为 true 时启动后立即执行一次，否则第一次在一个周期之后（调用方已经自己跑过一次时用）。
/// 失败只记日志，下个周期重试。
pub fn spawn_salts_rotation(
    pool: PgPool,
    mut stop: watch::Receiver<bool>,
    run_now: bool,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let first = if run_now {
            tokio::time::Instant::now()
        } else {
            tokio::time::Instant::now() + SALTS_ROTATE_INTERVAL
        };
        let mut ticker = tokio::time::interval_at(first, SALTS_ROTATE_INTERVAL);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                () = stopped(&mut stop) => break,
                _ = ticker.tick() => run_once(&pool).await,
            }
        }
    })
}

/// 执行一次并记录结果；失败不向上传播。
pub async fn run_once(pool: &PgPool) {
    match salts_rotate(pool, utc_today()).await {
        Ok(r) => tracing::info!(
            created = r.created,
            deleted = r.deleted,
            "salts_rotate 完成"
        ),
        Err(e) => {
            tracing::error!(error = %format!("{e:#}"), "salts_rotate 失败，下个周期重试");
        }
    }
}

/// 等到停止信号（发送端被丢弃也算停止）。`watch::Ref` 不是 `Send`，不能带过 await，所以包一层。
async fn stopped(rx: &mut watch::Receiver<bool>) {
    let _ = rx.wait_for(|s| *s).await;
}

#[cfg(test)]
mod tests {
    use jiff::civil::date;

    use super::*;

    #[test]
    fn required_days_are_yesterday_today_tomorrow() {
        assert_eq!(
            required_days(date(2026, 3, 1)),
            [date(2026, 2, 28), date(2026, 3, 1), date(2026, 3, 2)]
        );
        // 跨年、闰日
        assert_eq!(
            required_days(date(2026, 12, 31)),
            [date(2026, 12, 30), date(2026, 12, 31), date(2027, 1, 1)]
        );
        assert_eq!(required_days(date(2028, 3, 1))[0], date(2028, 2, 29));
    }

    #[test]
    fn salts_older_than_two_days_are_deletable() {
        let today = date(2026, 3, 1);
        assert_eq!(keep_from(today), date(2026, 2, 27));
        // 保留窗口 today-2 ..= today+1 是 4 天（§4.5）；必须存在的 3 天都在窗口内
        assert!(required_days(today).iter().all(|d| *d >= keep_from(today)));
    }
}
