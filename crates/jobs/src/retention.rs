//! `retention_purge`：按保留期删除原始数据（`system-design.md` §5.6，`platform-and-ops.md` §6）。
//!
//! - 每个站点保留最近 N 个本地日（含今天）：N = 站点的 `site_settings.retention_days`，没有设置时取
//!   `RIVU_RETENTION_DAYS`（默认 731）；站点设置不会超过全局值。截止时刻是站点时区里「今天 − (N − 1)」的 00:00。
//! - `events` 按 `ts`、`sessions` 按开始时间 `started_at`；日汇总层与 `ingest_counters`（表级 TTL 90 天）不在这里处理。
//! - 每个月分区：整月都早于所有站点的截止时刻 → `DROP PARTITION`；与截止时刻交叠且确实有过期行 →
//!   一条轻量删除，按站点取各自的截止时刻（`transform(site_id, …)`）；其余不动。所以平时每天至多一两条 mutation。
//! - 进程内定时（每 [`INTERVAL`]），`rivu_maintainer` 账号；重复执行没有副作用。

use std::time::Duration;

use anyhow::{Context, Result};
use clickhouse::Client;
use jiff::civil::Date;
use jiff::tz::TimeZone;
use jiff::{Timestamp, ToSpan};
use rivu_storage_pg::{sites, stats_deletions};
use sqlx::PgPool;
use tokio::sync::watch;
use tokio::task::JoinHandle;

use crate::stats_delete::{TABLES, Table};

/// 执行周期。
pub const INTERVAL: Duration = Duration::from_secs(3600);

/// 各站点的截止时刻（UTC 毫秒，早于它的原始数据删除）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cutoffs {
    pub sites: Vec<(u64, i64)>,
    /// 不在列表里的站点（已经彻底删除的站点留下的行）：全局保留期按 UTC 算再提前一天，比任何时区的截止都早，只会多留不会多删。
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

const EXPIRED: &str = "fromUnixTimestamp64Milli(transform(site_id, {ids:Array(UInt64)}, {cuts:Array(Int64)}, {dflt:Int64}), 'UTC') AND site_id NOT IN {fenced:Array(UInt64)}";

async fn purge_table(
    ch: &Client,
    database: &str,
    table: Table,
    c: &Cutoffs,
    fenced: &[u64],
) -> Result<(u64, u64)> {
    let parts: Vec<String> = ch
        .query("SELECT DISTINCT partition FROM system.parts WHERE database = ? AND table = ? AND active ORDER BY partition")
        .bind(database)
        .bind(table.name)
        .fetch_all()
        .await
        .with_context(|| format!("列出 {} 的分区失败", table.name))?;
    let ids: Vec<u64> = c.sites.iter().map(|(id, _)| *id).collect();
    let cuts: Vec<i64> = c.sites.iter().map(|(_, ms)| *ms).collect();
    let (mut dropped, mut deleted) = (0, 0);
    for p in parts {
        let Ok(partition) = p.parse::<u32>() else {
            continue;
        };
        match action_for(partition, c)? {
            Action::Keep => {}
            Action::Drop if fenced.is_empty() => {
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
            }
            Action::Drop | Action::Delete => {
                let col = table.time_column;
                let exists: Vec<u8> = ch
                    .query(&format!(
                        "SELECT 1 FROM {} WHERE {} = {partition} AND {col} < {EXPIRED} LIMIT 1",
                        table.name, table.partition_expr
                    ))
                    .param("ids", &ids)
                    .param("cuts", &cuts)
                    .param("dflt", c.default_ms)
                    .param("fenced", fenced)
                    .fetch_all()
                    .await
                    .with_context(|| format!("检查 {}:{partition} 的过期行失败", table.name))?;
                if exists.is_empty() {
                    continue;
                }
                ch.query(&format!(
                    "DELETE FROM {} IN PARTITION {partition} WHERE {col} < {EXPIRED}",
                    table.name
                ))
                .param("ids", &ids)
                .param("cuts", &cuts)
                .param("dflt", c.default_ms)
                .param("fenced", fenced)
                .execute()
                .await
                .with_context(|| format!("删除 {}:{partition} 的过期行失败", table.name))?;
                metrics::counter!("rivu_retention_partitions_total", "table" => table.name, "action" => "delete")
                    .increment(1);
                tracing::info!(table = table.name, partition, "分区里的过期行已删除");
                deleted += 1;
            }
        }
    }
    Ok((dropped, deleted))
}

/// 一轮清理：返回删除的分区数与执行了轻量删除的分区数。
pub async fn run_once(
    pg: &PgPool,
    ch: &Client,
    database: &str,
    global_days: u32,
) -> Result<(u64, u64)> {
    let mut tx = sites::begin_maintenance(pg).await?;
    stats_deletions::lock_retention(&mut tx, false).await?;
    let sites = sites::retention_settings(&mut tx).await?;
    let fenced = stats_deletions::maintenance_fences(&mut tx, None).await?;
    let c = cutoffs(global_days, &sites, Timestamp::now())?;
    let ch = ch.clone().with_setting("lightweight_deletes_sync", "2");
    let (mut dropped, mut deleted) = (0, 0);
    // events 与 sessions；ingest_counters 有自己的表级 TTL
    for table in TABLES.into_iter().filter(|t| t.name != "ingest_counters") {
        let (d, l) = purge_table(&ch, database, table, &c, &fenced).await?;
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
