//! 从 `sessions` 表重建最近活跃的会话（`system-design.md` §4.6「预热」第 2 步）。
//!
//! 只恢复一个 key：`last_visitor_id`（会话最后一个事件的当天 ID）；别名无法恢复，也不需要（§4.6「失败语义」）。
//! 先用子查询找出 `last_seen_at` 在窗口内的会话，再把这些会话在所有 part 里的行合并：
//! 只在外层按 `last_seen_at` 过滤会漏掉旧 part 里的 `started_at` 与入口页。
//! 调用方用 `rivu_writer` 的客户端（该账号只多了 `sessions` 的 SELECT）。

use clickhouse::{Client, Row};
use serde::Deserialize;

/// 一个会话合并后的状态。
#[derive(Row, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct WarmSession {
    pub site_id: u64,
    pub session_id: u64,
    pub last_visitor_id: u64,
    pub ingest_partition: u8,
    pub started_at_ms: i64,
    pub last_seen_ms: i64,
    /// 会话第一个 pageview 的 pathname；没有 pageview 时为空串。
    pub entry_pathname: String,
    pub referrer: String,
    pub referrer_source: String,
    pub channel: String,
    pub utm_source: String,
    pub utm_medium: String,
    pub utm_campaign: String,
    pub utm_content: String,
    pub utm_term: String,
    pub click_id_param: String,
    pub country_code: [u8; 2],
    pub region_code: String,
    pub city_geoname_id: u32,
    pub device: String,
    pub browser: String,
    pub browser_version: String,
    pub os: String,
    pub os_version: String,
}

/// `last_seen_at >= since_ms` 的会话。会话最长 7 天（§10 Q7），所以只看 `start_date` 最近 8 天的分区。
pub async fn recent_sessions(
    client: &Client,
    since_ms: i64,
    max_execution_secs: u64,
) -> Result<Vec<WarmSession>, clickhouse::error::Error> {
    client
        .query(
            "SELECT site_id,
                    session_id,
                    argMaxMerge(last_visitor_id) AS last_visitor_id,
                    any(ingest_partition) AS ingest_partition,
                    toUnixTimestamp64Milli(min(started_at)) AS started_at_ms,
                    toUnixTimestamp64Milli(max(last_seen_at)) AS last_seen_ms,
                    argMinIfMerge(entry_page).2 AS entry_pathname,
                    any(referrer) AS referrer,
                    toString(any(referrer_source)) AS referrer_source,
                    toString(any(channel)) AS channel,
                    any(utm_source) AS utm_source,
                    any(utm_medium) AS utm_medium,
                    any(utm_campaign) AS utm_campaign,
                    any(utm_content) AS utm_content,
                    any(utm_term) AS utm_term,
                    toString(any(click_id_param)) AS click_id_param,
                    CAST(any(country_code), 'FixedString(2)') AS country_code,
                    toString(any(region_code)) AS region_code,
                    any(city_geoname_id) AS city_geoname_id,
                    toString(any(device)) AS device,
                    toString(any(browser)) AS browser,
                    toString(any(browser_version)) AS browser_version,
                    toString(any(os)) AS os,
                    toString(any(os_version)) AS os_version
             FROM sessions
             WHERE start_date >= toDate(fromUnixTimestamp64Milli({since:Int64}, 'UTC')) - 8
               AND (site_id, session_id) IN (
                   SELECT site_id, session_id FROM sessions
                   WHERE start_date >= toDate(fromUnixTimestamp64Milli({since:Int64}, 'UTC')) - 8
                     AND last_seen_at >= fromUnixTimestamp64Milli({since:Int64}, 'UTC'))
             GROUP BY site_id, session_id",
        )
        .param("since", since_ms)
        .with_setting("max_execution_time", max_execution_secs.to_string())
        .with_setting("log_comment", "rivu:session_warmup")
        .fetch_all::<WarmSession>()
        .await
}
