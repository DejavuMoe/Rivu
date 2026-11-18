//! `rollup_build`：构建日汇总层（`system-design.md` §5.5、§6.4；表与口径见 [`rivu_query::rollup`]）。
//!
//! - 进程内定时（`RIVU_ROLLUP_INTERVAL_SECS`，默认 10 分钟），用 `rivu_maintainer` 账号。
//! - 每个站点从「已构建的最后一天 + 1」（没有构建过：最早事件那天）建到「结束已超过 [`SETTLE`] 的最后一个本地日」，
//!   一轮每站点最多 [`MAX_DAYS_PER_RUN`] 天，下一轮接着建。
//! - 站点改了时区：确认原始层覆盖旧汇总后，删除原生汇总并按新时区从头建。
//! - 设置更新与构建共享 PG 的站点行锁。
//! - 迟于 [`SETTLE`] 才写入的事件不会补进已构建的天（写入积压超过这个时间时会少算，§6.4「已知近似」）。

use std::time::Duration;

use anyhow::{Context, Result};
use clickhouse::Client;
use jiff::civil::Date;
use jiff::tz::TimeZone;
use jiff::{Timestamp, ToSpan};
use rivu_common::config::MIN_RETENTION_DAYS;
use rivu_query::rollup::{self, BuildState};
use rivu_query::{SiteContext, TenantScope};
use rivu_storage_pg::{imports, sites, stats_deletions};
use sqlx::{PgConnection, PgPool};
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

pub const TIMEZONE_REBUILD_UNPROVEN: &str = "timezone_rebuild_unproven";
pub const TIMEZONE_IMPORT_CONFLICT: &str = "timezone_import_conflict";

/// Called under the site row lock before changing its timezone in PostgreSQL.
pub async fn timezone_change_conflict(
    db: &mut PgConnection,
    ch: &Client,
    site_id: u64,
    current: &str,
    next: &str,
    now: Timestamp,
) -> Result<Option<&'static str>> {
    let Ok(old_tz) = TimeZone::get(current) else {
        return Ok(Some(TIMEZONE_REBUILD_UNPROVEN));
    };
    let new_tz = TimeZone::get(next).context("新站点时区无效")?;
    let scope = TenantScope::new(site_id);
    let state = rollup::build_state(ch, &scope, &old_tz)
        .await
        .map_err(|e| anyhow::anyhow!("读取构建进度失败：{e}"))?;
    // An earlier rebuild may have deleted some families and left its marker for retry.
    if state.tz_changed {
        return Ok(Some(TIMEZONE_REBUILD_UNPROVEN));
    }
    rebuild_conflict(db, ch, &scope, &new_tz, now, &state).await
}

async fn rebuild_conflict(
    db: &mut PgConnection,
    ch: &Client,
    scope: &TenantScope,
    tz: &TimeZone,
    now: Timestamp,
    state: &BuildState,
) -> Result<Option<&'static str>> {
    let first = rollup::first_native_day(ch, scope)
        .await
        .map_err(|e| anyhow::anyhow!("读取原生汇总边界失败：{e}"))?;
    let rebuild_since = if let Some(first) = first {
        // A raw starting point is needed for the builder, but cannot prove input coverage.
        let (Some((marker_day, marker_tz)), Some(raw), Some(raw_ms)) =
            (&state.first_marker, state.first_day, state.first_event_ms)
        else {
            return Ok(Some(TIMEZONE_REBUILD_UNPROVEN));
        };
        if *marker_day != first {
            return Ok(Some(TIMEZONE_REBUILD_UNPROVEN));
        }
        let Ok(old_tz) = TimeZone::get(marker_tz) else {
            return Ok(Some(TIMEZONE_REBUILD_UNPROVEN));
        };
        let (_, old_end) = rivu_query::resolve::local_dates_utc_ms(&old_tz, first, first)
            .map_err(|e| anyhow::anyhow!("换算旧汇总日期失败：{e}"))?;
        if raw_ms >= old_end {
            return Ok(Some(TIMEZONE_REBUILD_UNPROVEN));
        }

        // Family rows have no timezone. Bound every IANA offset (including transitions).
        const OFFSET: Duration = Duration::from_secs(26 * 3600);
        let earliest = first
            .to_zoned(TimeZone::UTC)?
            .timestamp()
            .checked_sub(OFFSET)?;
        let new_day = earliest.to_zoned(tz.clone()).date();
        let (start, _) = rivu_query::resolve::local_dates_utc_ms(tz, new_day, new_day)
            .map_err(|e| anyhow::anyhow!("换算重建日期失败：{e}"))?;
        let session_day = Timestamp::from_millisecond(start)?
            .to_zoned(TimeZone::UTC)
            .date()
            .checked_sub(rollup::SESSION_LOOKBACK_DAYS.days())?;
        // Any supported retention cutoff is <= now - (MIN - 1) days + two offsets.
        // Leave an extra day between that cutoff and the inputs to the rebuild.
        let retained = Duration::from_secs(u64::from(MIN_RETENTION_DAYS - 2) * 86_400);
        let latest_cutoff = now.checked_sub(retained)?.checked_add(OFFSET * 2)?;
        if session_day.to_zoned(TimeZone::UTC)?.timestamp() < latest_cutoff {
            return Ok(Some(TIMEZONE_REBUILD_UNPROVEN));
        }
        // Rebuild starts at the first raw event. Allow for its session scan and the
        // timezone in which any earlier explicit erase was requested.
        Some(
            raw.min(first)
                .checked_sub((rollup::SESSION_LOOKBACK_DAYS + 4).days())?
                .to_string(),
        )
    } else {
        None
    };
    if stats_deletions::blocks_timezone_change(db, scope.site_id(), rebuild_since).await? {
        return Ok(Some(TIMEZONE_REBUILD_UNPROVEN));
    }
    // The rebuild starts at the first raw event in the target timezone.
    if import_blocks_build(db, scope.site_id(), tz, state.first_event_ms).await? {
        return Ok(Some(TIMEZONE_IMPORT_CONFLICT));
    }
    Ok(None)
}

async fn import_blocks_build(
    db: &mut PgConnection,
    site_id: u64,
    tz: &TimeZone,
    first_event_ms: Option<i64>,
) -> Result<bool> {
    let native_from = first_event_ms
        .map(Timestamp::from_millisecond)
        .transpose()?
        .map(|ts| ts.to_zoned(tz.clone()).date().to_string());
    imports::blocks_timezone_change(db, site_id, native_from).await
}

/// 构建一个站点，返回这一轮构建的天数。
pub async fn build_site(pg: &PgPool, ch: &Client, site_id: u64) -> Result<usize> {
    let mut tx = sites::begin_maintenance(pg).await?;
    let Some(tz_name) = sites::lock_timezone(&mut tx, site_id, false).await? else {
        return Ok(0);
    };
    if !stats_deletions::maintenance_fences(&mut tx, Some(site_id))
        .await?
        .is_empty()
    {
        return Ok(0);
    }
    let now = Timestamp::now();
    let tz = TimeZone::get(&tz_name).unwrap_or(TimeZone::UTC);
    let scope = TenantScope::new(site_id);
    let state = rollup::build_state(ch, &scope, &tz)
        .await
        .map_err(|e| anyhow::anyhow!("读取构建进度失败：{e}"))?;
    if state.tz_changed {
        if let Some(code) = rebuild_conflict(&mut tx, ch, &scope, &tz, now, &state).await? {
            anyhow::bail!("{code}: 无法安全重建旧时区汇总");
        }
        rollup::delete(ch, &scope, None, true)
            .await
            .map_err(|e| anyhow::anyhow!("删除旧时区的汇总行失败：{e}"))?;
        tracing::info!(site_id, timezone = tz_name, "站点时区已变更，汇总层重建");
    }
    let days = days_to_build(&state, last_buildable(&tz, now)?);
    if !state.tz_changed
        && state.built_to.is_none()
        && !days.is_empty()
        && import_blocks_build(&mut tx, site_id, &tz, state.first_event_ms).await?
    {
        anyhow::bail!("{TIMEZONE_IMPORT_CONFLICT}: 导入与首次原生汇总冲突");
    }
    let site = SiteContext::new(site_id, tz);
    let version = u64::try_from(now.as_millisecond()).unwrap_or(0);
    for day in &days {
        rollup::build_day(ch, &scope, &site, *day, version, now)
            .await
            .map_err(|e| anyhow::anyhow!("构建 {day} 失败：{e}"))?;
    }
    tx.commit().await?;
    Ok(days.len())
}

/// 一轮 `rollup_build`：全部未删除的站点。单个站点失败只记日志，不影响其他站点。返回构建的天数。
pub async fn run_once(pg: &PgPool, ch: &Client) -> Result<usize> {
    let sites = rivu_storage_pg::sites::load_sites(pg).await?;
    let mut built = 0;
    for s in sites {
        match build_site(pg, ch, s.id).await {
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
            first_event_ms: None,
            first_marker: None,
            native_from: first_day,
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
