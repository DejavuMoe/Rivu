//! `rollup_build`：构建日汇总层（`system-design.md` §5.5、§6.4；表与口径见 [`rivu_query::rollup`]）。
//!
//! - 进程内定时（`RIVU_ROLLUP_INTERVAL_SECS`，默认 10 分钟），worker / all 运行，用 `rivu_maintainer` 账号。
//! - 每个站点从「已构建的最后一天 + 1」（没有构建过：最早事件那天）建到「结束已超过 [`SETTLE`] 的最后一个本地日」，
//!   一轮每站点最多 [`MAX_DAYS_PER_RUN`] 天，下一轮接着建。
//! - 站点改了时区：先删除它全部的原生汇总行，再按新时区从头建。
//! - 多个进程同时构建同一天是安全的：同键的行按 `version` 取最新，内容相同。
//! - 迟于 [`SETTLE`] 才写入的事件不会补进已构建的天（写入积压超过这个时间时会少算，§6.4「已知近似」）。

use std::time::Duration;

use anyhow::{Context, Result};
use clickhouse::Client;
use jiff::Timestamp;
use jiff::civil::Date;
use jiff::tz::TimeZone;
use rivu_query::rollup::{self, BuildState};
use rivu_query::{SiteContext, TenantScope};
use sqlx::PgPool;
use tokio::sync::watch;
use tokio::task::JoinHandle;

/// 一天结束后等多久再构建它：覆盖接入管线的批量与排队延迟。
pub const SETTLE: Duration = Duration::from_secs(2 * 3600);

/// 一轮里一个站点最多构建的天数（首次构建有历史数据的站点时分多轮完成）。
pub const MAX_DAYS_PER_RUN: usize = 92;

/// 可以构建的最后一个本地日：`now - SETTLE` 所在本地日的前一天。
pub fn last_buildable(tz: &TimeZone, now: Timestamp) -> Result<Date> {
    let settled = now.checked_sub(SETTLE).context("时间超出范围")?;
    settled
        .to_zoned(tz.clone())
        .date()
        .yesterday()
        .context("日期超出范围")
}

/// 这一轮要构建的天（升序）。
pub fn days_to_build(state: &BuildState, last: Date) -> Vec<Date> {
    let start = match (state.built_to, state.first_day) {
        (Some(b), _) if !state.tz_changed => match b.tomorrow() {
            Ok(d) => d,
            Err(_) => return Vec::new(),
        },
        (_, Some(first)) => first,
        (_, None) => return Vec::new(),
    };
    let mut out = Vec::new();
    let mut d = start;
    while d <= last && out.len() < MAX_DAYS_PER_RUN {
        out.push(d);
        match d.tomorrow() {
            Ok(n) => d = n,
            Err(_) => break,
        }
    }
    out
}

/// 构建一个站点，返回这一轮构建的天数。
pub async fn build_site(ch: &Client, site_id: u64, tz_name: &str, now: Timestamp) -> Result<usize> {
    let tz = TimeZone::get(tz_name).unwrap_or(TimeZone::UTC);
    let scope = TenantScope::new(site_id);
    let state = rollup::build_state(ch, &scope, &tz)
        .await
        .map_err(|e| anyhow::anyhow!("读取构建进度失败：{e}"))?;
    if state.tz_changed {
        rollup::delete(ch, &scope, None, true)
            .await
            .map_err(|e| anyhow::anyhow!("删除旧时区的汇总行失败：{e}"))?;
        tracing::info!(site_id, timezone = tz_name, "站点时区已变更，汇总层重建");
    }
    let days = days_to_build(&state, last_buildable(&tz, now)?);
    let site = SiteContext::new(site_id, tz);
    let version = u64::try_from(now.as_millisecond()).unwrap_or(0);
    for day in &days {
        rollup::build_day(ch, &scope, &site, *day, version, now)
            .await
            .map_err(|e| anyhow::anyhow!("构建 {day} 失败：{e}"))?;
    }
    Ok(days.len())
}

/// 一轮 `rollup_build`：全部未删除的站点。单个站点失败只记日志，不影响其他站点。返回构建的天数。
pub async fn run_once(pg: &PgPool, ch: &Client) -> Result<usize> {
    let sites = rivu_storage_pg::sites::load_sites(pg).await?;
    let now = Timestamp::now();
    let mut built = 0;
    for s in sites {
        match build_site(ch, s.id, &s.timezone, now).await {
            Ok(n) => {
                built += n;
                metrics::counter!("rivu_rollup_days_built_total").increment(n as u64);
            }
            Err(e) => {
                metrics::counter!("rivu_rollup_failures_total").increment(1);
                tracing::error!(site_id = s.id, error = %format!("{e:#}"), "汇总层构建失败，下个周期重试");
            }
        }
    }
    Ok(built)
}

/// 启动 `rollup_build` 的定时任务：启动后先跑一轮，之后每 `interval` 一轮。
pub fn spawn_rollup_build(
    pg: PgPool,
    ch: Client,
    interval: Duration,
    mut stop: watch::Receiver<bool>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                () = crate::stopped(&mut stop) => break,
                _ = ticker.tick() => {}
            }
            match run_once(&pg, &ch).await {
                Ok(0) => {}
                Ok(n) => tracing::info!(days = n, "rollup_build 完成"),
                Err(e) => {
                    tracing::error!(error = %format!("{e:#}"), "rollup_build 失败，下个周期重试");
                }
            }
        }
    })
}

/// 注册指标说明。
pub fn describe_metrics() {
    metrics::describe_counter!(
        "rivu_rollup_days_built_total",
        "rollup_build 构建的站点日数"
    );
    metrics::describe_counter!(
        "rivu_rollup_failures_total",
        "rollup_build 里构建失败的站点次数"
    );
}

#[cfg(test)]
mod tests {
    use jiff::civil::date;

    use super::*;

    fn state(built_to: Option<Date>, first_day: Option<Date>, tz_changed: bool) -> BuildState {
        BuildState {
            built_to,
            tz_changed,
            first_day,
        }
    }

    #[test]
    fn last_buildable_waits_for_the_day_to_settle() {
        let tz = TimeZone::get("Asia/Shanghai").unwrap();
        // 上海 10-28 01:30：减去 2 小时是 10-27 23:30，前一天 10-26
        let now: Timestamp = "2026-10-28T01:30:00+08:00".parse().unwrap();
        assert_eq!(last_buildable(&tz, now).unwrap(), date(2026, 10, 26));
        let now: Timestamp = "2026-10-28T02:00:00+08:00".parse().unwrap();
        assert_eq!(last_buildable(&tz, now).unwrap(), date(2026, 10, 27));
    }

    #[test]
    fn days_continue_after_the_last_built_day() {
        let last = date(2026, 10, 27);
        assert_eq!(
            days_to_build(
                &state(Some(date(2026, 10, 25)), Some(date(2026, 1, 1)), false),
                last
            ),
            vec![date(2026, 10, 26), date(2026, 10, 27)]
        );
        assert!(days_to_build(&state(Some(last), Some(date(2026, 1, 1)), false), last).is_empty());
        // 没有事件：什么也不建
        assert!(days_to_build(&state(None, None, false), last).is_empty());
        // 首次构建与时区变更都从最早事件那天开始，一轮最多 MAX_DAYS_PER_RUN 天
        let first = date(2025, 1, 1);
        let days = days_to_build(&state(None, Some(first), false), last);
        assert_eq!(days.len(), MAX_DAYS_PER_RUN);
        assert_eq!(days[0], first);
        let days = days_to_build(&state(Some(last), Some(first), true), last);
        assert_eq!(days[0], first);
    }
}
