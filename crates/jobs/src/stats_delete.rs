//! `ch_delete_batch`：执行 `pending_stats_deletions` 里到期的统计数据删除意图（`platform-and-ops.md` §6）。
//!
//! - 进程内定时（`RIVU_STATS_DELETE_INTERVAL_SECS`，默认 5 分钟），worker / all 运行；all 里看板写入日期范围意图后
//!   会立即唤醒一次（[`StatsDeleteKick`]）。多个进程同时运行是安全的：领取用 `FOR UPDATE SKIP LOCKED`，
//!   领取后 `not_before` 作为租约（见 `rivu_storage_pg::stats_deletions::claim_due`）。
//! - ClickHouse 侧用 `rivu_maintainer` 账号（`ALTER DELETE` 权限）对 `events`、`sessions`、`ingest_counters`
//!   逐分区执行轻量删除 `DELETE FROM t IN PARTITION p WHERE site_id = … [AND 时间条件]`；每完成一个分区记入
//!   `partitions_done`，崩溃后跳过已完成的分区继续；重复执行同一条删除没有副作用（幂等）。
//! - `scope='site'`：删除该站点在三张表全部分区里的行（分区列表按 `site_id` 现查）。
//! - `scope='date_range'`：日期是站点本地日期，按站点当前时区换算成 UTC 半开区间 `[start, end)`
//!   （与查询引擎的整日区间同一套换算，[`rivu_query::resolve::local_dates_utc_ms`]）：
//!   `events` 按 `ts`；`ingest_counters` 按 `minute`；`sessions` 删除**开始时间**（`started_at`）落在区间内的会话——
//!   跨过区间边界的会话按开始时间归属，与看板「会话属于开始那天」的口径一致。
//! - 失败：记 `last_error`，指数退避后重试，`MAX_ATTEMPTS` 次后置为 `failed`。
//!
//! 日志只带意图 id、站点 id 与分区，不带其他内容。

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clickhouse::Client;
use jiff::civil::Date;
use jiff::tz::TimeZone;
use jiff::{Timestamp, ToSpan};
use rivu_storage_pg::stats_deletions::{self, ClaimedDeletion};
use sqlx::PgPool;
use tokio::sync::{Notify, watch};
use tokio::task::JoinHandle;

/// 一轮最多领取的意图条数（领完再领，直到没有到期的）。
const CLAIM_BATCH: i64 = 4;

/// 看板写入日期范围意图后唤醒 `ch_delete_batch`（只在同一进程里运行 jobs 时有效，即 `rivu all`）。
pub type StatsDeleteKick = Arc<Notify>;

/// 删除涉及的一张表：表名、分区表达式、按时间删除时比较的列。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Table {
    pub name: &'static str,
    partition_expr: &'static str,
    time_column: &'static str,
}

/// 三张表（都是 `PARTITION BY toYYYYMM(...)`，迁移见 `migrations/clickhouse/`）。
pub const TABLES: [Table; 3] = [
    Table {
        name: "events",
        partition_expr: "toYYYYMM(ts)",
        time_column: "ts",
    },
    Table {
        name: "sessions",
        // start_date = toDate(session_start)（UTC），started_at = min(session_start)：两者落在同一个月
        partition_expr: "toYYYYMM(start_date)",
        time_column: "started_at",
    },
    Table {
        name: "ingest_counters",
        partition_expr: "toYYYYMM(minute)",
        time_column: "minute",
    },
];

/// 一个待删除的分区。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Target {
    pub table: Table,
    /// `toYYYYMM` 的值，例如 `202610`。
    pub partition: u32,
}

impl Target {
    /// 记入 `partitions_done` 的键：`<表>:<分区>`。
    pub fn key(&self) -> String {
        format!("{}:{}", self.table.name, self.partition)
    }
}

/// 删除的范围。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// 整个站点。
    Site,
    /// UTC 毫秒半开区间 `[start_ms, end_ms)`。
    Range { start_ms: i64, end_ms: i64 },
}

/// UTC 半开区间 `[start_ms, end_ms)` 覆盖的 `toYYYYMM` 分区（升序）。空区间返回空。
pub fn months_between(start_ms: i64, end_ms: i64) -> Result<Vec<u32>> {
    if end_ms <= start_ms {
        return Ok(Vec::new());
    }
    let month_of = |ms: i64| -> Result<Date> {
        let d = Timestamp::from_millisecond(ms)
            .context("时间超出范围")?
            .to_zoned(TimeZone::UTC)
            .date();
        Ok(d.first_of_month())
    };
    let first = month_of(start_ms)?;
    let last = month_of(end_ms - 1)?;
    let mut out = Vec::new();
    let mut m = first;
    while m <= last {
        out.push(
            u32::try_from(i32::from(m.year()) * 100 + i32::from(m.month()))
                .context("年份超出范围")?,
        );
        m = m.checked_add(1.month()).context("月份超出范围")?;
    }
    Ok(out)
}

/// 意图的删除范围：`date_range` 按站点时区（查不到或无效时按 UTC）换算成 UTC 区间。
pub fn scope_of(intent: &ClaimedDeletion) -> Result<Scope> {
    match intent.scope.as_str() {
        "site" => Ok(Scope::Site),
        "date_range" => {
            let (Some(from), Some(to)) = (&intent.date_from, &intent.date_to) else {
                bail!("date_range 意图缺少日期");
            };
            let from: Date = from.parse().context("date_from 无效")?;
            let to: Date = to.parse().context("date_to 无效")?;
            let tz = intent
                .timezone
                .as_deref()
                .and_then(|n| TimeZone::get(n).ok())
                .unwrap_or(TimeZone::UTC);
            let (start_ms, end_ms) = rivu_query::resolve::local_dates_utc_ms(&tz, from, to)
                .map_err(|e| anyhow::anyhow!("日期换算失败：{e}"))?;
            Ok(Scope::Range { start_ms, end_ms })
        }
        other => bail!("未知的删除范围 `{other}`"),
    }
}

/// 要删除的分区：整个站点按 `site_id` 现查各表的分区；日期范围按月份推算（三张表相同）。
pub async fn plan(ch: &Client, site_id: u64, scope: Scope) -> Result<Vec<Target>> {
    let mut out = Vec::new();
    match scope {
        Scope::Site => {
            for table in TABLES {
                let parts: Vec<u32> = ch
                    .query(&format!(
                        "SELECT DISTINCT {} AS p FROM {} WHERE site_id = {{site:UInt64}} ORDER BY p",
                        table.partition_expr, table.name
                    ))
                    .param("site", site_id)
                    .fetch_all()
                    .await
                    .with_context(|| format!("列出 {} 的分区失败", table.name))?;
                out.extend(
                    parts
                        .into_iter()
                        .map(|partition| Target { table, partition }),
                );
            }
        }
        Scope::Range { start_ms, end_ms } => {
            let months = months_between(start_ms, end_ms)?;
            for table in TABLES {
                out.extend(months.iter().map(|&partition| Target { table, partition }));
            }
        }
    }
    Ok(out)
}

/// 对一个分区执行轻量删除（幂等：没有匹配行或分区不存在时什么也不做）。
pub async fn delete_partition(ch: &Client, site_id: u64, scope: Scope, t: Target) -> Result<()> {
    // 分区值是本模块算出的整数，直接写进语句；站点与时间走参数
    let mut sql = format!(
        "DELETE FROM {} IN PARTITION {} WHERE site_id = {{site:UInt64}}",
        t.table.name, t.partition
    );
    if let Scope::Range { .. } = scope {
        let col = t.table.time_column;
        sql.push_str(&format!(
            " AND {col} >= fromUnixTimestamp64Milli({{start:Int64}}, 'UTC') AND {col} < fromUnixTimestamp64Milli({{end:Int64}}, 'UTC')"
        ));
    }
    let mut q = ch.query(&sql).param("site", site_id);
    if let Scope::Range { start_ms, end_ms } = scope {
        q = q.param("start", start_ms).param("end", end_ms);
    }
    q.execute()
        .await
        .with_context(|| format!("删除 {} 失败", t.key()))
}

/// 执行一条已领取的意图：跳过 `partitions_done` 里的分区，逐个删除并记录，全部完成后置为 `done`。
pub async fn process(pg: &PgPool, ch: &Client, intent: &ClaimedDeletion) -> Result<usize> {
    let scope = scope_of(intent)?;
    let targets = plan(ch, intent.site_id, scope).await?;
    let mut deleted = 0;
    for t in targets {
        let key = t.key();
        if intent.partitions_done.contains(&key) {
            continue;
        }
        delete_partition(ch, intent.site_id, scope, t).await?;
        stats_deletions::mark_partition_done(pg, intent.id, &key).await?;
        metrics::counter!("rivu_stats_delete_partitions_total", "table" => t.table.name)
            .increment(1);
        tracing::info!(
            deletion_id = intent.id,
            site_id = intent.site_id,
            partition = %key,
            "统计数据分区已删除"
        );
        deleted += 1;
    }
    stats_deletions::mark_done(pg, intent.id).await?;
    Ok(deleted)
}

/// 一轮 `ch_delete_batch`：反复领取到期意图并执行，直到没有到期的。返回处理的意图条数。
/// 单条失败记 `last_error` 并退避，不影响其他意图。
pub async fn run_batch(pg: &PgPool, ch: &Client) -> Result<usize> {
    let mut handled = 0;
    loop {
        let claimed = stats_deletions::claim_due(pg, CLAIM_BATCH).await?;
        if claimed.is_empty() {
            return Ok(handled);
        }
        for intent in &claimed {
            handled += 1;
            let scope = intent.scope.clone();
            match process(pg, ch, intent).await {
                Ok(partitions) => {
                    metrics::counter!("rivu_stats_deletions_total", "scope" => scope, "result" => "done")
                        .increment(1);
                    tracing::info!(
                        deletion_id = intent.id,
                        site_id = intent.site_id,
                        partitions,
                        "统计数据删除完成"
                    );
                }
                Err(e) => {
                    let msg = format!("{e:#}");
                    let failed = stats_deletions::mark_attempt_failed(
                        pg,
                        intent.id,
                        intent.attempts,
                        &msg,
                        stats_deletions::backoff_secs(intent.attempts),
                    )
                    .await?;
                    let result = if failed { "failed" } else { "retry" };
                    metrics::counter!("rivu_stats_deletions_total", "scope" => scope, "result" => result)
                        .increment(1);
                    tracing::error!(
                        deletion_id = intent.id,
                        site_id = intent.site_id,
                        attempts = intent.attempts,
                        failed,
                        error = %msg,
                        "统计数据删除失败"
                    );
                }
            }
        }
    }
}

/// 启动 `ch_delete_batch` 的定时任务：启动后先跑一轮，之后每 `interval` 一轮，`kick` 被通知时提前跑一轮。
pub fn spawn_stats_delete(
    pg: PgPool,
    ch: Client,
    interval: Duration,
    kick: StatsDeleteKick,
    mut stop: watch::Receiver<bool>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                () = crate::stopped(&mut stop) => break,
                _ = ticker.tick() => {}
                () = kick.notified() => {}
            }
            match run_batch(&pg, &ch).await {
                Ok(0) => {}
                Ok(n) => tracing::info!(intents = n, "ch_delete_batch 完成"),
                Err(e) => {
                    tracing::error!(error = %format!("{e:#}"), "ch_delete_batch 失败，下个周期重试");
                }
            }
        }
    })
}

/// 注册指标说明。
pub fn describe_metrics() {
    metrics::describe_counter!(
        "rivu_stats_deletions_total",
        "统计数据删除意图的执行结果（scope = site | date_range，result = done | retry | failed）"
    );
    metrics::describe_counter!(
        "rivu_stats_delete_partitions_total",
        "ch_delete_batch 完成轻量删除的 ClickHouse 分区数（按表）"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(s: &str) -> i64 {
        s.parse::<Timestamp>().unwrap().as_millisecond()
    }

    #[test]
    fn months_cover_the_half_open_range() {
        assert_eq!(
            months_between(ms("2026-09-30T16:00:00Z"), ms("2026-10-02T16:00:00Z")).unwrap(),
            vec![202609, 202610]
        );
        // 结束是开区间：恰好在月初的结束不包含那个月
        assert_eq!(
            months_between(ms("2026-10-05T00:00:00Z"), ms("2026-11-01T00:00:00Z")).unwrap(),
            vec![202610]
        );
        assert_eq!(
            months_between(ms("2025-11-15T00:00:00Z"), ms("2026-02-01T00:00:01Z")).unwrap(),
            vec![202511, 202512, 202601, 202602]
        );
        assert!(months_between(5, 5).unwrap().is_empty());
    }

    #[test]
    fn date_range_scope_uses_the_site_timezone() {
        let intent = ClaimedDeletion {
            id: 1,
            site_id: 7,
            scope: "date_range".into(),
            date_from: Some("2026-10-01".into()),
            date_to: Some("2026-10-01".into()),
            timezone: Some("Asia/Shanghai".into()),
            partitions_done: Vec::new(),
            attempts: 1,
        };
        assert_eq!(
            scope_of(&intent).unwrap(),
            Scope::Range {
                start_ms: ms("2026-09-30T16:00:00Z"),
                end_ms: ms("2026-10-01T16:00:00Z"),
            }
        );
        // 时区查不到：按 UTC
        let utc = ClaimedDeletion {
            timezone: None,
            ..intent.clone()
        };
        assert_eq!(
            scope_of(&utc).unwrap(),
            Scope::Range {
                start_ms: ms("2026-10-01T00:00:00Z"),
                end_ms: ms("2026-10-02T00:00:00Z"),
            }
        );
        let site = ClaimedDeletion {
            scope: "site".into(),
            ..intent
        };
        assert_eq!(scope_of(&site).unwrap(), Scope::Site);
    }

    #[test]
    fn partition_keys_name_the_table() {
        assert_eq!(
            Target {
                table: TABLES[1],
                partition: 202610
            }
            .key(),
            "sessions:202610"
        );
    }
}
