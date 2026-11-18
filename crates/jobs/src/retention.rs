//! `retention_purge`：按保留期删除原始数据（`system-design.md` §5.6，`platform-and-ops.md` §6）。
//!
//! - 每个站点保留最近 N 个本地日（含今天）：N = 站点的 `site_settings.retention_days`，没有设置时取
//!   `RIVU_RETENTION_DAYS`（默认 731）；站点设置不会超过全局值。截止时刻是站点时区里「今天 − (N − 1)」的 00:00。
//! - `events` 按 `ts`、`sessions` 按 `started_at` 过期，到首个缺失当前时区原生汇总标记的本地日为止。
//!   会话还须覆盖最后活动日。软删除站点、无法锁定或无法验证的站点保留 raw。`ingest_counters` 有独立 TTL。
//! - 整月过期且确认分区内所有行均可删除时 `DROP PARTITION`；部分可删除时执行按站点截止时刻的轻量删除。
//! - 进程内定时（每 [`INTERVAL`]），`rivu_maintainer` 账号；重复执行没有副作用。

use std::time::Duration;

use anyhow::{Context, Result};
use clickhouse::Client;
use jiff::civil::Date;
use jiff::tz::TimeZone;
use jiff::{Timestamp, ToSpan};
use rivu_query::{SiteContext, TenantScope, rollup};
use sqlx::{PgConnection, PgPool};
use tokio::sync::watch;
use tokio::task::JoinHandle;

use crate::stats_delete::{TABLES, Table};

/// 执行周期。
pub const INTERVAL: Duration = Duration::from_secs(3600);

/// 各站点的截止时刻（UTC 毫秒，早于它的原始数据删除）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cutoffs {
    pub sites: Vec<(u64, i64)>,
    /// 计算过期积压时，未知站点按全局保留期、UTC 再提前一天。实际删除必须有站点级覆盖证明。
    pub default_ms: i64,
}

impl Cutoffs {
    fn min(&self) -> i64 {
        self.sites
            .iter()
            .map(|(_, c)| *c)
            .chain([self.default_ms])
            .min()
            .unwrap_or(self.default_ms)
    }

    fn max(&self) -> i64 {
        self.sites
            .iter()
            .map(|(_, c)| *c)
            .chain([self.default_ms])
            .max()
            .unwrap_or(self.default_ms)
    }
}

fn cutoff_ms(tz: &TimeZone, days: u32, now: Timestamp) -> Result<i64> {
    let today = now.to_zoned(tz.clone()).date();
    let first: Date = today
        .checked_sub(i64::from(days.max(1) - 1).days())
        .context("日期超出范围")?;
    Ok(first
        .to_zoned(tz.clone())
        .context("本地日期换算失败")?
        .timestamp()
        .as_millisecond())
}

/// 算出各站点的截止时刻。`sites`：`(id, 时区, 站点级保留天数)`。
pub fn cutoffs(
    global_days: u32,
    sites: &[(u64, String, Option<i32>)],
    now: Timestamp,
) -> Result<Cutoffs> {
    let mut out = Vec::with_capacity(sites.len());
    for (id, tz_name, own) in sites {
        let tz = TimeZone::get(tz_name).unwrap_or(TimeZone::UTC);
        let days = own
            .and_then(|d| u32::try_from(d).ok())
            .map_or(global_days, |d| d.min(global_days));
        out.push((*id, cutoff_ms(&tz, days, now)?));
    }
    Ok(Cutoffs {
        sites: out,
        default_ms: cutoff_ms(&TimeZone::UTC, global_days, now)? - 86_400_000,
    })
}

/// `toYYYYMM` 分区值对应的 UTC 月份 `[起点, 下月起点)`（毫秒）。
pub fn month_bounds(partition: u32) -> Result<(i64, i64)> {
    let year = i16::try_from(partition / 100).context("分区值无效")?;
    let month = i8::try_from(partition % 100).context("分区值无效")?;
    let start = Date::new(year, month, 1).context("分区值无效")?;
    let next = start.checked_add(1.month()).context("日期超出范围")?;
    let ms =
        |d: Date| -> Result<i64> { Ok(d.to_zoned(TimeZone::UTC)?.timestamp().as_millisecond()) };
    Ok((ms(start)?, ms(next)?))
}

/// 一个分区要做什么。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Drop,
    /// 可能有过期行：先查，有才删。
    Delete,
    Keep,
}

pub fn action_for(partition: u32, c: &Cutoffs) -> Result<Action> {
    let (start, end) = month_bounds(partition)?;
    Ok(if end <= c.min() {
        Action::Drop
    } else if start < c.max() {
        Action::Delete
    } else {
        Action::Keep
    })
}

const PROVEN: &str =
    "transform(site_id, {ids:Array(UInt64)}, {cuts:Array(Int64)}, toInt64('-9223372036854775808'))";
const POLICY: &str =
    "transform(site_id, {policy_ids:Array(UInt64)}, {policy_cuts:Array(Int64)}, {dflt:Int64})";

async fn purge_table(
    ch: &Client,
    database: &str,
    table: Table,
    policy: &Cutoffs,
    proven: &[(u64, i64)],
) -> Result<(u64, u64)> {
    let parts: Vec<String> = ch
        .query("SELECT DISTINCT partition FROM system.parts WHERE database = ? AND table = ? AND active ORDER BY partition")
        .bind(database)
        .bind(table.name)
        .fetch_all()
        .await
        .with_context(|| format!("列出 {} 的分区失败", table.name))?;
    let (ids, cuts): (Vec<_>, Vec<_>) = proven.iter().copied().unzip();
    let (policy_ids, policy_cuts): (Vec<_>, Vec<_>) = policy.sites.iter().copied().unzip();
    let (mut dropped, mut deleted, mut blocked) = (0, 0, 0);
    let col = table.time_column;
    let eligible = format!("toUnixTimestamp64Milli({col}) < {PROVEN}");
    for p in parts {
        let Ok(partition) = p.parse::<u32>() else {
            continue;
        };
        let action = action_for(partition, policy)?;
        if action == Action::Keep {
            continue;
        }
        let (rows, eligible_rows, overdue): (u64, u64, u64) = ch
            .query(&format!(
                "SELECT count(), countIf({eligible}),
                        countIf(toUnixTimestamp64Milli({col}) < {POLICY} AND NOT ({eligible}))
                 FROM {} WHERE {} = {partition}",
                table.name, table.partition_expr
            ))
            .param("ids", &ids)
            .param("cuts", &cuts)
            .param("policy_ids", &policy_ids)
            .param("policy_cuts", &policy_cuts)
            .param("dflt", policy.default_ms)
            .fetch_one()
            .await
            .with_context(|| format!("检查 {}:{partition} 的过期行失败", table.name))?;
        blocked += overdue;
        if eligible_rows == 0 {
            continue;
        }
        if action == Action::Drop && rows == eligible_rows {
            // 分区值是本模块从系统表读出并解析过的整数
            ch.query(&format!(
                "ALTER TABLE {} DROP PARTITION {partition}",
                table.name
            ))
            .execute()
            .await
            .with_context(|| format!("删除分区 {}:{partition} 失败", table.name))?;
            metrics::counter!("rivu_retention_partitions_total", "table" => table.name, "action" => "drop")
                    .increment(1);
            tracing::info!(table = table.name, partition, "过期分区已删除");
            dropped += 1;
        } else {
            ch.query(&format!(
                "DELETE FROM {} IN PARTITION {partition} WHERE {eligible}",
                table.name
            ))
            .param("ids", &ids)
            .param("cuts", &cuts)
            .with_setting("lightweight_deletes_sync", "1")
            .execute()
            .await
            .with_context(|| format!("删除 {}:{partition} 的过期行失败", table.name))?;
            metrics::counter!("rivu_retention_partitions_total", "table" => table.name, "action" => "delete")
                    .increment(1);
            tracing::info!(table = table.name, partition, "分区里的过期行已删除");
            deleted += 1;
        }
    }
    metrics::gauge!("rivu_retention_blocked_rows", "table" => table.name).set(blocked as f64);
    if blocked > 0 {
        tracing::warn!(
            table = table.name,
            blocked,
            "保留已过期但未证明可清理的原始行"
        );
    }
    Ok((dropped, deleted))
}

fn local_day(tz: &TimeZone, ms: i64) -> Result<Date> {
    Ok(Timestamp::from_millisecond(ms)?.to_zoned(tz.clone()).date())
}

async fn proven_cutoffs(
    pg: &mut PgConnection,
    ch: &Client,
    id: u64,
    tz: TimeZone,
    requested: i64,
    now: Timestamp,
) -> Result<Option<(i64, i64)>> {
    let (first_event, last_event): (Option<i64>, Option<i64>) = ch
        .query(
            "SELECT minOrNull(toUnixTimestamp64Milli(ts)), maxOrNull(toUnixTimestamp64Milli(ts))
                FROM events WHERE site_id = ? AND ts < fromUnixTimestamp64Milli(?, 'UTC')",
        )
        .bind(id)
        .bind(requested)
        .fetch_one()
        .await?;
    let (first_session, last_session): (Option<i64>, Option<i64>) = ch
        .query("SELECT minOrNull(toUnixTimestamp64Milli(started_at)),
                       maxOrNull(toUnixTimestamp64Milli(greatest(started_at, last_seen_at)))
                FROM sessions WHERE site_id = ? AND started_at < fromUnixTimestamp64Milli(?, 'UTC')")
        .bind(id)
        .bind(requested)
        .fetch_one()
        .await?;
    let Some(first) = first_event.into_iter().chain(first_session).min() else {
        return Ok(None);
    };
    let last = last_event
        .into_iter()
        .chain(last_session)
        .max()
        .unwrap_or(first);
    let mut day = local_day(&tz, first)?;
    let through = local_day(&tz, last)?;
    let settled = crate::rollup::last_buildable(&tz, now)?;
    let tz_name = tz.iana_name().context("站点时区缺少 IANA 名称")?;
    let marks: Vec<(String, bool)> = ch
        .query(
            "SELECT toString(day), tz = ? FROM rollup_site FINAL
                WHERE site_id = ? AND origin = 'native' AND import_id = 0",
        )
        .bind(tz_name)
        .bind(id)
        .fetch_all()
        .await?;
    if marks.iter().any(|(_, matches_tz)| !matches_tz) {
        tracing::warn!(site_id = id, "存在旧时区的汇总标记，暂停清理");
        return Ok(None);
    }
    let marks: std::collections::HashSet<_> = marks.into_iter().map(|(d, _)| d).collect();
    let mut repaired = false;
    while day <= through && day <= settled {
        if !marks.contains(&day.to_string()) {
            let start = day.to_zoned(tz.clone())?.timestamp();
            // 旧版按时间前缀清理。用最早仍在的 raw 证明整个构建窗口（含七天 UTC 会话回看）在幸存范围内。
            let lookback = start.to_zoned(TimeZone::UTC).date().checked_sub(7.days())?;
            let lookback_ms = lookback
                .to_zoned(TimeZone::UTC)?
                .timestamp()
                .as_millisecond();
            if !first_event.is_some_and(|ms| ms <= lookback_ms)
                || !first_session.is_some_and(|ms| ms <= lookback_ms)
            {
                break;
            }
            let erased: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM pending_stats_deletions
                  WHERE site_id = $1 AND scope = 'date_range' AND status = 'done'
                    AND date_from <= $2::date AND date_to >= $3::date)",
            )
            .bind(i64::try_from(id)?)
            .bind(day.tomorrow()?.to_string())
            .bind(lookback.yesterday()?.to_string())
            .fetch_one(&mut *pg)
            .await?;
            if erased {
                break;
            }
            if repaired {
                // shortcut: 每站点每轮只补一天，积压清空前保留用于证明可补建的 raw。
                return Ok(None);
            }
            let site = SiteContext::new(id, tz.clone());
            rollup::build_day(
                ch,
                &TenantScope::new(id),
                &site,
                day,
                u64::try_from(now.as_millisecond())?,
                now,
            )
            .await
            .map_err(|e| anyhow::anyhow!("补建 {day} 失败：{e}"))?;
            let marked: u64 = ch
                .query("SELECT countIf(tz = ?) FROM rollup_site FINAL
                        WHERE site_id = ? AND origin = 'native' AND import_id = 0 AND day = toDate(?)")
                .bind(tz_name)
                .bind(id)
                .bind(day.to_string())
                .fetch_one()
                .await?;
            anyhow::ensure!(marked == 1, "未读到 {day} 补建后的原生汇总标记");
            tracing::info!(site_id = id, %day, "已补建原生汇总");
            repaired = true;
        }
        day = day.tomorrow()?;
    }
    if day > through {
        return Ok(Some((requested, requested)));
    }
    tracing::warn!(site_id = id, first_uncovered_day = %day, "原生汇总前缀不足，限制原始数据清理");
    let boundary = day.to_zoned(tz)?.timestamp().as_millisecond();
    let events = requested.min(boundary);
    // 同一会话的所有物理行使用同一个开始时间；任一行跨界，就保留这一会话的所有聚合状态。
    let first_crossing: Option<i64> = ch
        .query(
            "SELECT minOrNull(toUnixTimestamp64Milli(started_at)) FROM sessions
                WHERE site_id = ? AND started_at < fromUnixTimestamp64Milli(?, 'UTC')
                 AND last_seen_at >= fromUnixTimestamp64Milli(?, 'UTC')",
        )
        .bind(id)
        .bind(events)
        .bind(boundary)
        .fetch_one()
        .await?;
    Ok(Some((events, first_crossing.unwrap_or(events))))
}

/// 一轮清理：返回删除的分区数与执行了轻量删除的分区数。
pub async fn run_once(
    pg: &PgPool,
    ch: &Client,
    database: &str,
    global_days: u32,
) -> Result<(u64, u64)> {
    // 与时区 PATCH 和 rollup worker 的时区切换互斥，要求它们也持有同一站点行锁。
    let mut tx = pg.begin().await?;
    let sites = rivu_storage_pg::sites::lock_retention_sites(&mut tx).await?;
    let now = Timestamp::now();
    let settings = sites
        .iter()
        .map(|s| Ok((u64::try_from(s.id)?, s.timezone.clone(), s.retention_days)))
        .collect::<Result<Vec<_>>>()?;
    let policy = cutoffs(global_days, &settings, now)?;
    let (mut events, mut sessions) = (Vec::new(), Vec::new());
    for (site, &(id, requested)) in sites.iter().zip(&policy.sites) {
        if site.paused {
            continue;
        }
        let result = async {
            let tz = TimeZone::get(&site.timezone)?;
            proven_cutoffs(&mut tx, ch, id, tz, requested, now).await
        }
        .await;
        match result {
            Ok(Some((event_cut, session_cut))) => {
                events.push((id, event_cut));
                sessions.push((id, session_cut));
            }
            Ok(None) => {}
            Err(e) => {
                tracing::error!(site_id = id, error = %format!("{e:#}"), "无法证明汇总覆盖，保留原始数据")
            }
        }
    }
    let (mut dropped, mut deleted) = (0, 0);
    // events 与 sessions；ingest_counters 有自己的表级 TTL
    for (table, proven) in [(TABLES[0], events), (TABLES[1], sessions)] {
        let (d, l) = purge_table(ch, database, table, &policy, &proven).await?;
        dropped += d;
        deleted += l;
    }
    tx.commit().await?;
    Ok((dropped, deleted))
}

/// 启动 `retention_purge`：启动后先跑一轮，之后每 [`INTERVAL`] 一轮。失败只记日志。
pub fn spawn_retention_purge(
    pg: PgPool,
    ch: Client,
    database: String,
    global_days: u32,
    mut stop: watch::Receiver<bool>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(INTERVAL);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                () = crate::stopped(&mut stop) => break,
                _ = ticker.tick() => {}
            }
            match run_once(&pg, &ch, &database, global_days).await {
                Ok((0, 0)) => {}
                Ok((dropped, deleted)) => {
                    tracing::info!(dropped, deleted, "retention_purge 完成");
                }
                Err(e) => {
                    tracing::error!(error = %format!("{e:#}"), "retention_purge 失败，下个周期重试");
                }
            }
        }
    })
}

/// 注册指标说明。
pub fn describe_metrics() {
    metrics::describe_counter!(
        "rivu_retention_partitions_total",
        "retention_purge 处理的分区数（action = drop：整个分区过期；delete：分区里有过期行）"
    );
    metrics::describe_gauge!(
        "rivu_retention_blocked_rows",
        "最近一轮检查中已过期但尚未证明可以删除的原始行数（sessions 计物理聚合状态行）"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(s: &str) -> i64 {
        s.parse::<Timestamp>().unwrap().as_millisecond()
    }

    #[test]
    fn cutoffs_follow_site_timezone_and_retention() {
        let now: Timestamp = "2026-10-28T12:00:00Z".parse().unwrap();
        let sites = vec![
            (1, "Asia/Shanghai".to_owned(), None),
            (2, "UTC".to_owned(), Some(30)),
            // 站点设置超过全局：按全局
            (3, "UTC".to_owned(), Some(5000)),
        ];
        let c = cutoffs(731, &sites, now).unwrap();
        // 上海 10-28 往前 730 天 = 2024-10-28 00:00 +08:00
        assert_eq!(c.sites[0], (1, ms("2024-10-28T00:00:00+08:00")));
        assert_eq!(c.sites[1], (2, ms("2026-09-29T00:00:00Z")));
        assert_eq!(c.sites[2], (3, ms("2024-10-28T00:00:00Z")));
        // 默认：UTC 的截止再提前一天，比任何时区都早，只会多留
        assert_eq!(c.default_ms, ms("2024-10-27T00:00:00Z"));
        assert!(c.default_ms <= c.min());
    }

    #[test]
    fn partitions_are_dropped_only_when_wholly_expired() {
        let c = Cutoffs {
            sites: vec![
                (1, ms("2024-10-28T00:00:00Z")),
                (2, ms("2026-09-29T00:00:00Z")),
            ],
            default_ms: ms("2024-10-27T10:00:00Z"),
        };
        assert_eq!(
            month_bounds(202602).unwrap(),
            (ms("2026-02-01T00:00:00Z"), ms("2026-03-01T00:00:00Z"))
        );
        assert_eq!(action_for(202409, &c).unwrap(), Action::Drop);
        // 默认截止落在 2024-10 里：不能整个删
        assert_eq!(action_for(202410, &c).unwrap(), Action::Delete);
        // 站点 2 只留 30 天：2026-08 的行对它过期
        assert_eq!(action_for(202608, &c).unwrap(), Action::Delete);
        assert_eq!(action_for(202609, &c).unwrap(), Action::Delete);
        assert_eq!(action_for(202610, &c).unwrap(), Action::Keep);
    }
}
