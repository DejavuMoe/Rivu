//! 接入诊断的读取：按时间桶、指标与脚本版本聚合 `ingest_counters`（`system-design.md` §5.4）。
//!
//! 表的排序键是 `(site_id, minute, metric, script_version)`，查询带 `site_id` 等值与 `minute` 范围，
//! 只读排序键前缀；`SummingMergeTree` 的同键行可能还没合并，所以一律 `sum(value)`。
//! 所有值走参数绑定。调用方用 `rivu_reader` 的客户端，并传入该档位的 `max_execution_time`。

use clickhouse::{Client, Row};
use serde::Deserialize;

/// 一个 `(时间桶, 指标, 脚本版本)` 分组的合计。
#[derive(Row, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct CounterGroup {
    /// 桶起点（Unix 秒，是 `step_secs` 的整数倍）。
    pub bucket: u32,
    /// `accepted` 或 `dropped_<原因>`。
    pub metric_name: String,
    pub script_version: u16,
    pub total: u64,
    /// 分组内最后一个有数据的分钟（Unix 秒）。
    pub last_minute: u32,
}

/// 站点在 `[from, to)`（Unix 秒）内按 `step_secs` 取整的分组合计。
pub async fn counter_groups(
    client: &Client,
    site_id: u64,
    from: u32,
    to: u32,
    step_secs: u32,
    max_execution_secs: u64,
    log_comment: &str,
) -> Result<Vec<CounterGroup>, clickhouse::error::Error> {
    client
        .query(
            "SELECT toUInt32(intDiv(toUnixTimestamp(minute), {step:UInt32}) * {step:UInt32}) AS bucket,
                    toString(metric) AS metric_name,
                    script_version,
                    sum(value) AS total,
                    max(toUnixTimestamp(minute)) AS last_minute
             FROM ingest_counters
             WHERE site_id = {site:UInt64}
               AND minute >= toDateTime({from:UInt32}, 'UTC')
               AND minute < toDateTime({to:UInt32}, 'UTC')
             GROUP BY bucket, metric_name, script_version
             ORDER BY bucket, metric_name, script_version",
        )
        .param("step", step_secs)
        .param("site", site_id)
        .param("from", from)
        .param("to", to)
        .with_setting("max_execution_time", max_execution_secs.to_string())
        .with_setting("log_comment", log_comment)
        .fetch_all::<CounterGroup>()
        .await
}

/// 站点从 `from`（Unix 秒）起最后一个有 `accepted` 的分钟（Unix 秒）；没有返回 `None`。
pub async fn last_accepted_minute(
    client: &Client,
    site_id: u64,
    from: u32,
    max_execution_secs: u64,
    log_comment: &str,
) -> Result<Option<u32>, clickhouse::error::Error> {
    let ts: u32 = client
        .query(
            "SELECT max(toUnixTimestamp(minute))
             FROM ingest_counters
             WHERE site_id = {site:UInt64}
               AND minute >= toDateTime({from:UInt32}, 'UTC')
               AND metric = 'accepted'",
        )
        .param("site", site_id)
        .param("from", from)
        .with_setting("max_execution_time", max_execution_secs.to_string())
        .with_setting("log_comment", log_comment)
        .fetch_one()
        .await?;
    // 空集的 max 是 0（1970-01-01）
    Ok((ts > 0).then_some(ts))
}
