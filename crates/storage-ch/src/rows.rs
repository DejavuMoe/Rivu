//! 写入 ClickHouse 的行结构体与批量 INSERT。
//!
//! 约定（`system-design.md` §4.7、核实报告 #12/#13/E1/E2）：
//! - 显式 `async_insert=0`：26.2 起服务端默认开启；
//! - 每批一个稳定的 `insert_deduplication_token`，重试时不变；token 要逐批用 `with_setting` 传入；
//! - 显式 `deduplicate_blocks_in_dependent_materialized_views=1`：25.8 默认是 0，
//!   重放时 `events` 去重了而 `sessions` 会再写一遍；
//! - `deduplicate_insert` 是 26.2 才有的设置，只在服务端支持时才设（发送未知设置会被拒绝）。

use std::collections::HashMap;

use anyhow::{Context, Result};
use clickhouse::{Client, Row};
use serde::Serialize;
use time::OffsetDateTime;
use uuid::Uuid;

/// 与 `events` 表的全部列一一对应（官方 crate 默认校验：没有 DEFAULT 的列必须都出现）。
#[derive(Row, Serialize, Clone, Debug)]
pub struct EventRow {
    pub site_id: u64,
    #[serde(with = "clickhouse::serde::time::datetime64::millis")]
    pub ts: OffsetDateTime,
    #[serde(with = "clickhouse::serde::uuid")]
    pub event_id: Uuid,
    pub name: String,
    pub visitor_id: u64,
    pub session_id: u64,
    #[serde(with = "clickhouse::serde::time::datetime64::millis")]
    pub session_start: OffsetDateTime,
    pub ingest_partition: u8,
    pub hostname: String,
    pub pathname: String,
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
    pub interactive: bool,
    pub props: HashMap<String, String>,
    pub scroll_depth: u8,
    pub engagement_ms: u32,
    // 旧列没有显式 DEFAULT，ClickHouse 客户端校验要求写入行包含它们，采集端始终填空值。
    pub revenue_amount: Option<i64>,
    pub revenue_currency: String,
}

impl EventRow {
    /// 序列化后大小的粗略估算（字节），写缓冲用它判断「8 MB」触发。
    /// 固定部分按各数值列的宽度，加上每个字符串的长度前缀与内容。
    pub fn approx_bytes(&self) -> usize {
        // 数值、日期、UUID、bool、FixedString(2) 等定长列
        const FIXED: usize = 8 + 8 + 16 + 8 + 8 + 8 + 1 + 2 + 4 + 1 + 1 + 4 + 8 + 1 + 4 + 1;
        // 每个 String 的长度前缀（varint，按 1 字节计）
        const STRINGS: usize = 19;
        let text = self.name.len()
            + self.hostname.len()
            + self.pathname.len()
            + self.entry_pathname.len()
            + self.referrer.len()
            + self.referrer_source.len()
            + self.channel.len()
            + self.utm_source.len()
            + self.utm_medium.len()
            + self.utm_campaign.len()
            + self.utm_content.len()
            + self.utm_term.len()
            + self.click_id_param.len()
            + self.region_code.len()
            + self.device.len()
            + self.browser.len()
            + self.browser_version.len()
            + self.os.len()
            + self.os_version.len()
            + self.revenue_currency.len();
        let props: usize = self.props.iter().map(|(k, v)| k.len() + v.len() + 2).sum();
        FIXED + STRINGS + text + props + 1
    }
}

impl Default for EventRow {
    /// 空事件：除了「事件级」列取协议默认值（`interactive = true`、`scroll_depth = 255` 表示无），
    /// 其余都是零值。测试和构造行时用 `..Default::default()` 省去全部列。
    fn default() -> Self {
        EventRow {
            site_id: 0,
            ts: OffsetDateTime::UNIX_EPOCH,
            event_id: Uuid::nil(),
            name: String::new(),
            visitor_id: 0,
            session_id: 0,
            session_start: OffsetDateTime::UNIX_EPOCH,
            ingest_partition: 0,
            hostname: String::new(),
            pathname: String::new(),
            entry_pathname: String::new(),
            referrer: String::new(),
            referrer_source: String::new(),
            channel: String::new(),
            utm_source: String::new(),
            utm_medium: String::new(),
            utm_campaign: String::new(),
            utm_content: String::new(),
            utm_term: String::new(),
            click_id_param: String::new(),
            country_code: [0, 0],
            region_code: String::new(),
            city_geoname_id: 0,
            device: String::new(),
            browser: String::new(),
            browser_version: String::new(),
            os: String::new(),
            os_version: String::new(),
            interactive: true,
            props: HashMap::new(),
            scroll_depth: 255,
            engagement_ms: 0,
            revenue_amount: None,
            revenue_currency: String::new(),
        }
    }
}
/// `ingest_counters` 的一行（`system-design.md` §5.4）。`minute` 已按分钟取整。
#[derive(Row, Serialize, Clone, Debug, PartialEq, Eq)]
pub struct IngestCounterRow {
    pub site_id: u64,
    #[serde(with = "clickhouse::serde::time::datetime")]
    pub minute: OffsetDateTime,
    pub metric: String,
    pub script_version: u16,
    pub value: u64,
}

/// 服务端是否有 `deduplicate_insert` 设置（26.2 起才有）。
pub async fn supports_deduplicate_insert(client: &Client) -> Result<bool> {
    let n: u64 = client
        .query("SELECT count() FROM system.settings WHERE name = 'deduplicate_insert'")
        .fetch_one()
        .await
        .context("查询 system.settings 失败")?;
    Ok(n > 0)
}

/// 写入一批行。同一个 `token` 重放同一批数据时，ClickHouse 会跳过重复的 block（含 MV 的下游表）。
async fn insert_rows<T>(
    client: &Client,
    table: &str,
    rows: &[T],
    token: &str,
    has_deduplicate_insert: bool,
) -> Result<()>
where
    T: for<'a> Row<Value<'a> = T> + Serialize,
{
    if rows.is_empty() {
        return Ok(());
    }
    let mut insert = client
        .insert::<T>(table)
        .await
        .with_context(|| format!("开始 INSERT INTO {table} 失败"))?
        .with_setting("async_insert", "0")
        .with_setting("insert_deduplicate", "1")
        .with_setting("insert_deduplication_token", token)
        .with_setting("deduplicate_blocks_in_dependent_materialized_views", "1");
    if has_deduplicate_insert {
        insert = insert.with_setting("deduplicate_insert", "enable");
    }
    for row in rows {
        insert
            .write(row)
            .await
            .with_context(|| format!("写入 {table} 的行失败"))?;
    }
    insert
        .end()
        .await
        .with_context(|| format!("结束 INSERT INTO {table} 失败"))
}

/// 写入一批 `events`（`sessions` 由 `mv_sessions` 在同一个 INSERT 内生成）。
pub async fn insert_events(
    client: &Client,
    rows: &[EventRow],
    token: &str,
    has_deduplicate_insert: bool,
) -> Result<()> {
    insert_rows(client, "events", rows, token, has_deduplicate_insert).await
}

/// 写入一批 `ingest_counters`。
pub async fn insert_ingest_counters(
    client: &Client,
    rows: &[IngestCounterRow],
    token: &str,
    has_deduplicate_insert: bool,
) -> Result<()> {
    insert_rows(
        client,
        "ingest_counters",
        rows,
        token,
        has_deduplicate_insert,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn approx_bytes_grows_with_content() {
        let base = EventRow::default();
        let long = EventRow {
            pathname: "/".repeat(1000),
            props: HashMap::from([("plan".to_owned(), "pro".to_owned())]),
            ..EventRow::default()
        };
        assert!(long.approx_bytes() >= base.approx_bytes() + 999 + 9);
        // 空行也有固定开销，不会是 0
        assert!(base.approx_bytes() > 100);
    }

    #[test]
    fn default_row_uses_protocol_defaults() {
        let r = EventRow::default();
        assert!(r.interactive);
        assert_eq!(r.scroll_depth, 255);
        assert_eq!(r.country_code, [0, 0]);
    }
}
