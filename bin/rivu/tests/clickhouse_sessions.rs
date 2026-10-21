//! 集成测试：迁移 -> 写入一个会话的 3 个事件 -> 校验 mv_sessions 的聚合结果 -> 校验去重 token。
//!
//! 需要真实的 PostgreSQL 和 ClickHouse。缺少环境变量时跳过并打印原因：
//!   RIVU_TEST_DATABASE_URL     有 DDL 权限的 PostgreSQL 连接串（rivu_migrator）
//!   RIVU_TEST_CLICKHOUSE_URL   ClickHouse HTTP 地址
//! 可选（默认值是 deploy/compose.dev.yml 里的开发账号）：
//!   RIVU_TEST_CH_{MIGRATOR,WRITER,READER}_{USER,PASSWORD}
//!   RIVU_TEST_CH_DATABASE      默认 rivu

use std::collections::HashMap;

use clickhouse::{Client, Row};
use rivu::migrate;
use rivu_common::config::{ChAccount, Config, Env, Role, Secret};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

/// 与 `events` 表的全部列一一对应（官方 crate 默认校验：没有 DEFAULT 的列必须都出现）。
#[derive(Row, Serialize, Clone)]
struct EventRow {
    site_id: u64,
    #[serde(with = "clickhouse::serde::time::datetime64::millis")]
    ts: OffsetDateTime,
    #[serde(with = "clickhouse::serde::uuid")]
    event_id: Uuid,
    name: String,
    visitor_id: u64,
    session_id: u64,
    #[serde(with = "clickhouse::serde::time::datetime64::millis")]
    session_start: OffsetDateTime,
    ingest_partition: u8,
    hostname: String,
    pathname: String,
    entry_pathname: String,
    referrer: String,
    referrer_source: String,
    channel: String,
    utm_source: String,
    utm_medium: String,
    utm_campaign: String,
    utm_content: String,
    utm_term: String,
    click_id_param: String,
    country_code: [u8; 2],
    region_code: String,
    city_geoname_id: u32,
    device: String,
    browser: String,
    browser_version: String,
    os: String,
    os_version: String,
    interactive: bool,
    props: HashMap<String, String>,
    scroll_depth: u8,
    engagement_ms: u32,
    revenue_amount: Option<i64>,
    revenue_currency: String,
}

#[derive(Row, Deserialize, Debug, PartialEq)]
struct SessionAgg {
    pageviews: u64,
    events: u64,
    interactive_events: u64,
    engagement_ms: u64,
    entry: (String, String),
    exit: (String, String),
}

fn var(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

fn account(role: &str) -> ChAccount {
    let upper = role.to_uppercase();
    ChAccount {
        user: var(&format!("RIVU_TEST_CH_{upper}_USER")).unwrap_or_else(|| format!("rivu_{role}")),
        password: Secret::new(
            var(&format!("RIVU_TEST_CH_{upper}_PASSWORD"))
                .unwrap_or_else(|| format!("rivu_dev_{role}")),
        ),
    }
}

fn client(url: &str, role: &str) -> Client {
    rivu_storage_ch::client(url, &account(role), Some("rivu"))
}

fn event(
    site_id: u64,
    session_id: u64,
    base: OffsetDateTime,
    offset_s: i64,
    name: &str,
    pathname: &str,
    engagement_ms: u32,
) -> EventRow {
    EventRow {
        site_id,
        ts: base + time::Duration::seconds(offset_s),
        event_id: Uuid::now_v7(),
        name: name.into(),
        visitor_id: session_id ^ 0xABCD,
        session_id,
        session_start: base,
        hostname: "example.test".into(),
        pathname: pathname.into(),
        entry_pathname: "/".into(),
        ingest_partition: 0,
        referrer: String::new(),
        referrer_source: String::new(),
        channel: String::new(),
        utm_source: String::new(),
        utm_medium: String::new(),
        utm_campaign: String::new(),
        utm_content: String::new(),
        utm_term: String::new(),
        click_id_param: String::new(),
        country_code: *b"CN",
        region_code: "CN-31".into(),
        city_geoname_id: 0,
        device: "desktop".into(),
        browser: "Firefox".into(),
        browser_version: "130.0".into(),
        os: "Linux".into(),
        os_version: String::new(),
        interactive: true,
        props: HashMap::new(),
        scroll_depth: 255,
        engagement_ms,
        revenue_amount: None,
        revenue_currency: String::new(),
    }
}

/// 与 `rivu_storage_ch` 的写入约定一致：显式 async_insert=0、稳定的去重 token、
/// 显式开启 MV 去重。`deduplicate_insert` 是 26.2 才有的设置，只在服务端支持时才设置。
async fn insert_batch(
    writer: &Client,
    rows: &[EventRow],
    token: &str,
    has_deduplicate_insert: bool,
) {
    let mut insert = writer
        .insert::<EventRow>("events")
        .await
        .expect("开始 INSERT")
        .with_setting("async_insert", "0")
        .with_setting("insert_deduplicate", "1")
        .with_setting("insert_deduplication_token", token)
        .with_setting("deduplicate_blocks_in_dependent_materialized_views", "1");
    if has_deduplicate_insert {
        insert = insert.with_setting("deduplicate_insert", "enable");
    }
    for row in rows {
        insert.write(row).await.expect("写入行");
    }
    insert.end().await.expect("结束 INSERT");
}

async fn session_aggregates(reader: &Client, site_id: u64) -> Vec<SessionAgg> {
    // 内层按完整排序键 GROUP BY：合并 AggregatingMergeTree 里尚未合并的部分行
    reader
        .query(
            "SELECT sum(pageviews) AS pageviews, sum(events) AS events, \
                    sum(interactive_events) AS interactive_events, sum(engagement_ms) AS engagement_ms, \
                    argMinIfMerge(entry_page) AS entry, argMaxIfMerge(exit_page) AS exit \
             FROM rivu.sessions WHERE site_id = {site:UInt64} \
             GROUP BY site_id, start_date, visitor_id, session_id",
        )
        .param("site", site_id)
        .fetch_all::<SessionAgg>()
        .await
        .expect("查询 sessions")
}

async fn count(client: &Client, table: &str, site_id: u64) -> u64 {
    client
        .query(&format!(
            "SELECT count() FROM rivu.{table} WHERE site_id = {{site:UInt64}}"
        ))
        .param("site", site_id)
        .fetch_one::<u64>()
        .await
        .expect("count")
}

#[tokio::test]
async fn session_mv_aggregates_and_dedups() {
    let (Some(pg_url), Some(ch_url)) = (
        var("RIVU_TEST_DATABASE_URL"),
        var("RIVU_TEST_CLICKHOUSE_URL"),
    ) else {
        println!(
            "跳过 session_mv_aggregates_and_dedups：需要设置 RIVU_TEST_DATABASE_URL 和 RIVU_TEST_CLICKHOUSE_URL"
        );
        return;
    };

    // 1. 迁移（也等于在测试里再验证一次幂等）
    let migrator = account("migrator");
    let env = Env::from_pairs([
        ("RIVU_DATABASE_URL", pg_url.as_str()),
        ("RIVU_CLICKHOUSE_URL", ch_url.as_str()),
        (
            "RIVU_CH_DATABASE",
            var("RIVU_TEST_CH_DATABASE")
                .unwrap_or_else(|| "rivu".into())
                .as_str(),
        ),
        ("RIVU_CH_MIGRATOR_USER", migrator.user.as_str()),
        ("RIVU_CH_MIGRATOR_PASSWORD", migrator.password.expose()),
    ]);
    let config = Config::from_env(Role::Migrate, &env).expect("测试配置");
    migrate::apply(&config, false).await.expect("migrate");
    let second = migrate::apply(&config, false).await.expect("migrate again");
    assert!(second.is_empty(), "第二次迁移不应有待执行项: {second:?}");

    let admin = client(&ch_url, "migrator");
    let writer = client(&ch_url, "writer");
    let reader = client(&ch_url, "reader");

    let has_deduplicate_insert: u64 = admin
        .query("SELECT count() FROM system.settings WHERE name = 'deduplicate_insert'")
        .fetch_one()
        .await
        .expect("查询 system.settings");
    let has_deduplicate_insert = has_deduplicate_insert > 0;
    println!("服务端支持 deduplicate_insert: {has_deduplicate_insert}");

    // 2. 一个会话：pageview / -> pageview /pricing -> engagement（/pricing，停留 7000 ms）
    let now = OffsetDateTime::now_utc();
    let base = now
        .replace_nanosecond((now.nanosecond() / 1_000_000) * 1_000_000)
        .unwrap();
    let nanos = now.unix_timestamp_nanos();
    let site_id = 9_000_000_000_000_000_000u64 + (nanos % 1_000_000_000_000) as u64;
    let session_id = (nanos as u64) | 1;
    let rows = vec![
        event(site_id, session_id, base, 0, "pageview", "/", 0),
        event(site_id, session_id, base, 10, "pageview", "/pricing", 0),
        event(
            site_id,
            session_id,
            base,
            20,
            "engagement",
            "/pricing",
            7000,
        ),
    ];

    insert_batch(
        &writer,
        &rows,
        &format!("it:{site_id}:1"),
        has_deduplicate_insert,
    )
    .await;

    let expected = SessionAgg {
        pageviews: 2,
        events: 2,
        interactive_events: 0,
        engagement_ms: 7000,
        entry: ("example.test".into(), "/".into()),
        exit: ("example.test".into(), "/pricing".into()),
    };
    let sessions = session_aggregates(&reader, site_id).await;
    assert_eq!(sessions, vec![expected], "首次写入后的会话聚合");
    assert_eq!(count(&reader, "events", site_id).await, 3);
    assert_eq!(
        count(&reader, "sessions", site_id).await,
        1,
        "MV 只应产生 1 行部分聚合"
    );

    // 3. 同一个 token 重放同一批数据：events 和 sessions 都不应重复计数
    insert_batch(
        &writer,
        &rows,
        &format!("it:{site_id}:1"),
        has_deduplicate_insert,
    )
    .await;
    assert_eq!(
        count(&reader, "events", site_id).await,
        3,
        "重放后 events 行数不变"
    );
    assert_eq!(
        count(&reader, "sessions", site_id).await,
        1,
        "重放后 sessions 的部分聚合行数不变（MV 去重生效）"
    );
    let sessions = session_aggregates(&reader, site_id).await;
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].pageviews, 2, "重放后 pageviews 不变");
    assert_eq!(sessions[0].events, 2, "重放后 events 不变");
    assert_eq!(sessions[0].engagement_ms, 7000, "重放后 engagement_ms 不变");

    // 4. 对照：换一个 token 就是新的一批，应该被计入（证明上面的断言不是因为什么都没写进去）
    insert_batch(
        &writer,
        &rows,
        &format!("it:{site_id}:2"),
        has_deduplicate_insert,
    )
    .await;
    assert_eq!(count(&reader, "events", site_id).await, 6);
    let sessions = session_aggregates(&reader, site_id).await;
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].pageviews, 4);
    assert_eq!(sessions[0].events, 4);
    assert_eq!(sessions[0].engagement_ms, 14000);

    // 清理测试数据（尽力而为；mutation 异步执行）
    for table in ["events", "sessions"] {
        let _ = admin
            .query(&format!(
                "ALTER TABLE rivu.{table} DELETE WHERE site_id = {{site:UInt64}}"
            ))
            .param("site", site_id)
            .execute()
            .await;
    }
}
