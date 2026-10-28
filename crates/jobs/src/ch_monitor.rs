//! `ch_monitor`：ClickHouse 的 parts、merge 与 mutation 积压（`system-design.md` §7「需要观察的指标」，
//! `platform-and-ops.md` §10）。
//!
//! 进程内定时（每 [`INTERVAL`]），用 `rivu_maintainer` 账号读 `system.parts`、`system.merges`、
//! `system.mutations`（只看 Rivu 的库）。导出按表的 gauge，不带站点标签：
//!
//! - `rivu_ch_parts{table}`：活跃 part 数；`rivu_ch_partition_parts_max{table}`：单个分区里最多的活跃 part 数
//!   （ClickHouse 按分区计数，超过 `parts_to_delay_insert` 开始拖慢插入，超过 `parts_to_throw_insert` 拒绝插入）；
//! - `rivu_ch_rows{table}`、`rivu_ch_bytes{table}`：行数与磁盘占用（压缩后）；
//! - `rivu_ch_merges{table}`：正在进行的 merge；
//! - `rivu_ch_mutations_pending{table}`、`rivu_ch_mutation_age_seconds{table}`：未完成的 mutation（轻量删除也是）
//!   与其中最老的已等待时间。
//!
//! 单个分区的 part 数达到 [`PARTS_WARN`]、或 mutation 等待超过 [`MUTATION_WARN`] 时打一条 `warn`，
//! 回落之后再次越过才会再打。

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use anyhow::{Context, Result};
use clickhouse::Client;
use tokio::sync::watch;
use tokio::task::JoinHandle;

/// 采样周期。
pub const INTERVAL: Duration = Duration::from_secs(30);

/// 单个分区的活跃 part 数告警线（25.8 的 `parts_to_delay_insert` 默认 1000）。
pub const PARTS_WARN: u64 = 300;

/// mutation 等待时间告警线。
pub const MUTATION_WARN: Duration = Duration::from_secs(3600);

/// 一张表的一次采样。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TableStats {
    pub parts: u64,
    pub partition_parts_max: u64,
    pub rows: u64,
    pub bytes: u64,
    pub merges: u64,
    pub mutations_pending: u64,
    pub mutation_age_secs: u64,
}

/// 采样一次：Rivu 库里每张有数据或有活动的表。
pub async fn sample(ch: &Client, database: &str) -> Result<BTreeMap<String, TableStats>> {
    let mut out: BTreeMap<String, TableStats> = BTreeMap::new();
    let parts: Vec<(String, u64, u64, u64, u64)> = ch
        .query(
            "SELECT table, sum(n), max(n), sum(r), sum(b) FROM ( \
               SELECT table, partition_id, count() AS n, sum(rows) AS r, sum(bytes_on_disk) AS b \
               FROM system.parts WHERE database = ? AND active GROUP BY table, partition_id) \
             GROUP BY table",
        )
        .bind(database)
        .fetch_all()
        .await
        .context("读取 system.parts 失败")?;
    for (table, n, max, rows, bytes) in parts {
        let t = out.entry(table).or_default();
        t.parts = n;
        t.partition_parts_max = max;
        t.rows = rows;
        t.bytes = bytes;
    }
    let merges: Vec<(String, u64)> = ch
        .query("SELECT table, count() FROM system.merges WHERE database = ? GROUP BY table")
        .bind(database)
        .fetch_all()
        .await
        .context("读取 system.merges 失败")?;
    for (table, n) in merges {
        out.entry(table).or_default().merges = n;
    }
    let mutations: Vec<(String, u64, u64)> = ch
        .query(
            "SELECT table, count(), toUInt64(greatest(max(dateDiff('second', create_time, now())), 0)) \
             FROM system.mutations WHERE database = ? AND NOT is_done GROUP BY table",
        )
        .bind(database)
        .fetch_all()
        .await
        .context("读取 system.mutations 失败")?;
    for (table, n, age) in mutations {
        let t = out.entry(table).or_default();
        t.mutations_pending = n;
        t.mutation_age_secs = age;
    }
    Ok(out)
}

/// 越线告警的状态：同一张表回落之前只告警一次。
#[derive(Default)]
pub struct Alerts {
    parts: BTreeSet<String>,
    mutations: BTreeSet<String>,
}

impl Alerts {
    /// 返回这一轮新越线的 `(表, 种类)`。
    pub fn update(&mut self, stats: &BTreeMap<String, TableStats>) -> Vec<(String, &'static str)> {
        let mut fired = Vec::new();
        let mut check =
            |set: &mut BTreeSet<String>, kind: &'static str, over: &dyn Fn(&TableStats) -> bool| {
                for (table, s) in stats {
                    if over(s) {
                        if set.insert(table.clone()) {
                            fired.push((table.clone(), kind));
                        }
                    } else {
                        set.remove(table);
                    }
                }
                set.retain(|t| stats.contains_key(t));
            };
        check(&mut self.parts, "parts", &|s| {
            s.partition_parts_max >= PARTS_WARN
        });
        check(&mut self.mutations, "mutation", &|s| {
            s.mutation_age_secs >= MUTATION_WARN.as_secs()
        });
        fired
    }
}

fn export(stats: &BTreeMap<String, TableStats>, previous: &BTreeSet<String>) {
    for (table, s) in stats {
        let t = table.clone();
        metrics::gauge!("rivu_ch_parts", "table" => t.clone()).set(s.parts as f64);
        metrics::gauge!("rivu_ch_partition_parts_max", "table" => t.clone())
            .set(s.partition_parts_max as f64);
        metrics::gauge!("rivu_ch_rows", "table" => t.clone()).set(s.rows as f64);
        metrics::gauge!("rivu_ch_bytes", "table" => t.clone()).set(s.bytes as f64);
        metrics::gauge!("rivu_ch_merges", "table" => t.clone()).set(s.merges as f64);
        metrics::gauge!("rivu_ch_mutations_pending", "table" => t.clone())
            .set(s.mutations_pending as f64);
        metrics::gauge!("rivu_ch_mutation_age_seconds", "table" => t)
            .set(s.mutation_age_secs as f64);
    }
    // 上一轮有、这一轮没有的表（数据删空了）：归零，不留旧值
    for table in previous.iter().filter(|t| !stats.contains_key(*t)) {
        for name in [
            "rivu_ch_parts",
            "rivu_ch_partition_parts_max",
            "rivu_ch_rows",
            "rivu_ch_bytes",
            "rivu_ch_merges",
            "rivu_ch_mutations_pending",
            "rivu_ch_mutation_age_seconds",
        ] {
            metrics::gauge!(name, "table" => table.clone()).set(0.0);
        }
    }
}

/// 启动 `ch_monitor`：启动后立即采样一次，之后每 [`INTERVAL`] 一次。失败只记日志。
pub fn spawn_ch_monitor(
    ch: Client,
    database: String,
    mut stop: watch::Receiver<bool>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(INTERVAL);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut alerts = Alerts::default();
        let mut previous: BTreeSet<String> = BTreeSet::new();
        loop {
            tokio::select! {
                () = crate::stopped(&mut stop) => break,
                _ = ticker.tick() => {}
            }
            match sample(&ch, &database).await {
                Ok(stats) => {
                    export(&stats, &previous);
                    for (table, kind) in alerts.update(&stats) {
                        let s = &stats[&table];
                        match kind {
                            "parts" => tracing::warn!(
                                table = %table,
                                partition_parts_max = s.partition_parts_max,
                                merges = s.merges,
                                "ClickHouse 单个分区的活跃 part 过多，merge 跟不上写入；继续增长会拖慢并最终拒绝插入"
                            ),
                            _ => tracing::warn!(
                                table = %table,
                                pending = s.mutations_pending,
                                age_secs = s.mutation_age_secs,
                                "ClickHouse 有长时间未完成的 mutation（删除）"
                            ),
                        }
                    }
                    previous = stats.into_keys().collect();
                }
                Err(e) => {
                    tracing::warn!(error = %format!("{e:#}"), "ch_monitor 采样失败，下个周期重试");
                }
            }
        }
    })
}

/// 注册指标说明。
pub fn describe_metrics() {
    metrics::describe_gauge!("rivu_ch_parts", "ClickHouse 表的活跃 part 数");
    metrics::describe_gauge!(
        "rivu_ch_partition_parts_max",
        "ClickHouse 表里单个分区最多的活跃 part 数（插入限流按分区计）"
    );
    metrics::describe_gauge!("rivu_ch_rows", "ClickHouse 表的行数（活跃 part）");
    metrics::describe_gauge!(
        "rivu_ch_bytes",
        metrics::Unit::Bytes,
        "ClickHouse 表的磁盘占用（压缩后，活跃 part）"
    );
    metrics::describe_gauge!("rivu_ch_merges", "ClickHouse 表正在进行的 merge 数");
    metrics::describe_gauge!(
        "rivu_ch_mutations_pending",
        "ClickHouse 表未完成的 mutation 数（含轻量删除）"
    );
    metrics::describe_gauge!(
        "rivu_ch_mutation_age_seconds",
        metrics::Unit::Seconds,
        "ClickHouse 表最老的未完成 mutation 已等待的时间"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stats(parts: u64, age: u64) -> BTreeMap<String, TableStats> {
        BTreeMap::from([(
            "events".to_owned(),
            TableStats {
                partition_parts_max: parts,
                mutation_age_secs: age,
                ..Default::default()
            },
        )])
    }

    #[test]
    fn alerts_fire_once_per_crossing() {
        let mut a = Alerts::default();
        assert!(a.update(&stats(10, 0)).is_empty());
        assert_eq!(
            a.update(&stats(PARTS_WARN, 0)),
            vec![("events".to_owned(), "parts")]
        );
        assert!(
            a.update(&stats(PARTS_WARN + 50, 0)).is_empty(),
            "仍在线上：不重复告警"
        );
        assert!(a.update(&stats(20, 0)).is_empty());
        assert_eq!(a.update(&stats(PARTS_WARN, 0)).len(), 1, "回落后再次越线");
        assert_eq!(
            a.update(&stats(PARTS_WARN, 7200)),
            vec![("events".to_owned(), "mutation")]
        );
        // 表消失（删空）：状态清掉
        assert!(a.update(&BTreeMap::new()).is_empty());
        assert_eq!(a.update(&stats(PARTS_WARN, 0)).len(), 1);
    }
}
