//! `retention_purge`：按保留期删除原始数据（`system-design.md` §5.6，`platform-and-ops.md` §6）。
//!
//! - 每个站点保留最近 N 个本地日（含今天）：N = 站点的 `site_settings.retention_days`，没有设置时取
//!   `RIVU_RETENTION_DAYS`（默认 731）；站点设置不会超过全局值。截止时刻是站点时区里「今天 − (N − 1)」的 00:00。
//! - 从首个已验证的本地日到下一未覆盖日，原生标记给出 UTC 半开区间。`events` 按 `ts`、
//!   `sessions` 按 `started_at` 与策略取交集，会话上界还需减去生产最长会话时长，
//!   且物理状态行的最早输入必须已知且不早于已验证区间。
//! - 仅轻量删除符合站点和时间条件的过期行。软删除、无法锁定或验证的站点保留 raw。`ingest_counters` 有独立 TTL。
//! - 进程内定时（每 [`INTERVAL`]），`rivu_maintainer` 账号；重复执行没有副作用。

use std::time::Duration;

use anyhow::{Context, Result};
use clickhouse::Client;
use jiff::civil::Date;
use jiff::tz::TimeZone;
use jiff::{Timestamp, ToSpan};
use rivu_session::assigner::SessionConfig;
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
    /// 计算过期积压时，未知站点按全局保留期、UTC 再提前一天。实际删除必须有站点级覆盖证明。
    pub default_ms: i64,
}

impl Cutoffs {
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
    local_midnight_ms(first, tz).context("本地日期换算失败")
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

fn month_start_ms(partition: u32) -> Result<i64> {
    let year = i16::try_from(partition / 100).context("分区值无效")?;
    let month = i8::try_from(partition % 100).context("分区值无效")?;
    local_midnight_ms(Date::new(year, month, 1)?, &TimeZone::UTC)
}

fn local_midnight_ms(day: Date, tz: &TimeZone) -> Result<i64> {
    Ok(day.to_zoned(tz.clone())?.timestamp().as_millisecond())
}

struct ProvenRange {
    site_id: u64,
    start_ms: i64,
    end_ms: i64,
}

const PROVEN_START: &str =
    "transform(site_id, {ids:Array(UInt64)}, {starts:Array(Int64)}, toInt64(0))";
const PROVEN_END: &str = "transform(site_id, {ids:Array(UInt64)}, {ends:Array(Int64)}, toInt64(0))";
const POLICY: &str =
    "transform(site_id, {policy_ids:Array(UInt64)}, {policy_cuts:Array(Int64)}, {dflt:Int64})";

async fn purge_table(
    ch: &Client,
    database: &str,
    table: Table,
    policy: &Cutoffs,
    proven: &[ProvenRange],
) -> Result<u64> {
    let parts: Vec<String> = ch
        .query("SELECT DISTINCT partition FROM system.parts WHERE database = ? AND table = ? AND active ORDER BY partition")
        .bind(database)
        .bind(table.name)
        .fetch_all()
        .await
        .with_context(|| format!("列出 {} 的分区失败", table.name))?;
    let ids: Vec<_> = proven.iter().map(|p| p.site_id).collect();
    let starts: Vec<_> = proven.iter().map(|p| p.start_ms).collect();
    let ends: Vec<_> = proven.iter().map(|p| p.end_ms).collect();
    let (policy_ids, policy_cuts): (Vec<_>, Vec<_>) = policy.sites.iter().copied().unzip();
    let (mut deleted, mut blocked) = (0, 0);
    let ms = format!("toUnixTimestamp64Milli({})", table.time_column);
    let session_lower = if table.name == "sessions" {
        format!(
            " AND first_input_ms != {} AND first_input_ms >= {PROVEN_START}",
            i64::MIN
        )
    } else {
        String::new()
    };
    let eligible = format!(
        "has({{ids:Array(UInt64)}}, site_id) AND {ms} >= {PROVEN_START} AND {ms} < {PROVEN_END}{session_lower}"
    );
    let max_cutoff = policy.max();
    for p in parts {
        let Ok(partition) = p.parse::<u32>() else {
            continue;
        };
        if month_start_ms(partition)? >= max_cutoff {
            continue;
        }
        let (eligible_rows, overdue): (u64, u64) = ch
            .query(&format!(
                "SELECT countIf({eligible}), countIf({ms} < {POLICY} AND NOT ({eligible}))
                 FROM {} WHERE {} = {partition}",
                table.name, table.partition_expr
            ))
            .param("ids", &ids)
            .param("starts", &starts)
            .param("ends", &ends)
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
        ch.query(&format!(
            "DELETE FROM {} IN PARTITION {partition} WHERE {eligible}",
            table.name
        ))
        .param("ids", &ids)
        .param("starts", &starts)
        .param("ends", &ends)
        .with_setting("lightweight_deletes_sync", "1")
        .execute()
        .await
        .with_context(|| format!("删除 {}:{partition} 的过期行失败", table.name))?;
        metrics::counter!("rivu_retention_partitions_total", "table" => table.name, "action" => "delete")
            .increment(1);
        tracing::info!(table = table.name, partition, "分区里的过期行已删除");
        deleted += 1;
    }
    metrics::gauge!("rivu_retention_blocked_rows", "table" => table.name).set(blocked as f64);
    if blocked > 0 {
        tracing::warn!(
            table = table.name,
            blocked,
            "保留已过期但未证明可清理的原始行"
        );
    }
    Ok(deleted)
}

/// 从第一条原生标记开始，逐个本地日核验；缺口后的标记不延伸此前的连续区间。
async fn proven_range(
    ch: &Client,
    id: u64,
    tz: &TimeZone,
    requested: i64,
    now: Timestamp,
    lifetime_ms: i64,
) -> Result<Option<(i64, i64)>> {
    let tz_name = tz.iana_name().context("站点时区缺少 IANA 名称")?;
    let marks: Vec<(String, bool)> = ch
        .query(
            "SELECT toString(day), tz = ? FROM rollup_site FINAL
                WHERE site_id = ? AND origin = 'native' AND import_id = 0 ORDER BY day",
        )
        .bind(tz_name)
        .bind(id)
        .fetch_all()
        .await?;
    if marks.iter().any(|(_, matches_tz)| !matches_tz) {
        tracing::warn!(site_id = id, "存在旧时区的汇总标记，暂停清理");
        return Ok(None);
    }
    let Some((first, _)) = marks.first() else {
        return Ok(None);
    };
    let first: Date = first.parse()?;
    let settled = crate::rollup::last_buildable(tz, now)?;
    let mut expected = first;
    for (day, _) in marks {
        if expected > settled || day.parse::<Date>()? != expected {
            break;
        }
        expected = expected.tomorrow()?;
    }
    let start = local_midnight_ms(first, tz)?;
    let end = local_midnight_ms(expected, tz)?;
    if expected <= settled && end - lifetime_ms < requested {
        tracing::warn!(site_id = id, first_uncovered_day = %expected, "原生汇总覆盖有缺口，限制原始数据清理");
    }
    Ok((start < end).then_some((start, end)))
}

/// 一轮清理，返回执行了轻量删除的分区数。
pub async fn run_once(pg: &PgPool, ch: &Client, database: &str, global_days: u32) -> Result<u64> {
    // 站点设置在清理期间固定，ClickHouse writer 仍可写入。
    let mut tx = rivu_storage_pg::sites::begin_maintenance(pg).await?;
    let sites = rivu_storage_pg::sites::lock_retention_sites(&mut tx).await?;
    let now = Timestamp::now();
    let settings = sites
        .iter()
        .map(|s| Ok((u64::try_from(s.id)?, s.timezone.clone(), s.retention_days)))
        .collect::<Result<Vec<_>>>()?;
    let policy = cutoffs(global_days, &settings, now)?;
    let lifetime_ms = SessionConfig::default().max_duration_ms;
    let (mut events, mut sessions) = (Vec::new(), Vec::new());
    for (site, &(id, requested)) in sites.iter().zip(&policy.sites) {
        if site.paused {
            continue;
        }
        let result = async {
            let tz = TimeZone::get(&site.timezone)?;
            proven_range(ch, id, &tz, requested, now, lifetime_ms).await
        }
        .await;
        match result {
            Ok(Some((start_ms, end_ms))) => {
                // mv_sessions 按 session_start 记录同一会话的所有状态，生产续期允许恰好达到 lifetime。
                for (ranges, end) in [(&mut events, end_ms), (&mut sessions, end_ms - lifetime_ms)]
                {
                    let end_ms = end.min(requested);
                    if start_ms < end_ms {
                        ranges.push(ProvenRange {
                            site_id: id,
                            start_ms,
                            end_ms,
                        });
                    }
                }
            }
            Ok(None) => {}
            Err(e) => {
                tracing::error!(site_id = id, error = %format!("{e:#}"), "无法证明汇总覆盖，保留原始数据")
            }
        }
    }
    let mut deleted = 0;
    // ingest_counters 有自己的表级 TTL。
    for (table, proven) in [(TABLES[0], events), (TABLES[1], sessions)] {
        deleted += purge_table(ch, database, table, &policy, &proven).await?;
    }
    tx.commit().await?;
    Ok(deleted)
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
                Ok(0) => {}
                Ok(deleted) => {
                    tracing::info!(deleted, "retention_purge 完成");
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
        "retention_purge 执行轻量删除的分区数（action = delete）"
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
    }

    #[test]
    fn local_midnight_spans_a_short_dst_day() {
        let tz = TimeZone::get("America/New_York").unwrap();
        let day = Date::new(2026, 3, 8).unwrap();
        assert_eq!(
            local_midnight_ms(day, &tz).unwrap(),
            ms("2026-03-08T05:00:00Z")
        );
        assert_eq!(
            local_midnight_ms(day.tomorrow().unwrap(), &tz).unwrap(),
            ms("2026-03-09T04:00:00Z")
        );
    }
}
