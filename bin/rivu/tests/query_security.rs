//! 查询内核的安全测试（真实的 ClickHouse）：恶意取值只当数据，不会被解释成 SQL；站点之间互不可见；
//! 正则长度上限在数据库侧也生效。
//!
//! 数据直接以 `EventRow` 写入（绕过管线，这样路径里可以放任意字符串），会话由 `mv_sessions` 生成。
//! 缺少 `RIVU_TEST_CLICKHOUSE_URL` 时跳过。可选变量同 `e2e_lite.rs`。

use jiff::Timestamp;
use jiff::tz::TimeZone;
use rivu_common::config::{ChAccount, Secret};
use rivu_query::{
    ExecOptions, QueryError, QueryResult, QuerySpec, SiteContext, TenantScope, execute,
};
use rivu_storage_ch::rows::{EventRow, insert_events};
use serde_json::{Value, json};
use time::OffsetDateTime;

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

fn database() -> String {
    var("RIVU_TEST_CH_DATABASE").unwrap_or_else(|| "rivu".into())
}

/// 引号、反斜杠、`{x:String}` 形式的文本、注释、换行、制表符、占位符、Unicode。
const NASTY: [&str; 10] = [
    "/'; DROP TABLE events; --",
    "/back\\slash\\\\'\\",
    "/a'b\"c`d",
    "/{x:String}",
    "/{p0:Array(String)} {site:UInt64}",
    "/* c */ OR 1=1 --",
    "/line1\nline2\ttab\r",
    "/? ?? ?fields",
    "/') OR site_id != 0 OR ('1'='1",
    "/日本語 😀 \u{202e}evil",
];

fn row(site_id: u64, i: u64, path: &str) -> EventRow {
    let ts_ms = 1_773_100_800_000i64 + (i as i64) * 60_000; // 2026-03-10 00:00 UTC 起，每分钟一个
    let ts = OffsetDateTime::from_unix_timestamp_nanos(i128::from(ts_ms) * 1_000_000).unwrap();
    EventRow {
        site_id,
        ts,
        name: "pageview".into(),
        visitor_id: 1000 + i,
        session_id: 5000 + i,
        session_start: ts,
        hostname: "example.test".into(),
        pathname: path.into(),
        entry_pathname: path.into(),
        referrer_source: path.into(),
        utm_term: path.into(),
        ..EventRow::default()
    }
}

async fn run(
    reader: &clickhouse::Client,
    site_id: u64,
    spec: Value,
) -> Result<QueryResult, QueryError> {
    let spec = QuerySpec::parse(&spec)?;
    execute(
        reader,
        &TenantScope::new(site_id),
        &SiteContext::new(site_id, TimeZone::UTC),
        spec,
        ExecOptions::new(10)
            .with_now("2026-03-11T00:00:00Z".parse::<Timestamp>().unwrap())
            .with_route("security-test"),
    )
    .await
}

fn count(r: &QueryResult) -> u64 {
    match r.results[0].metrics[0] {
        rivu_query::result::MetricValue::Int(n) => n,
        v => panic!("{v:?}"),
    }
}

#[tokio::test]
async fn hostile_values_are_data_and_sites_are_isolated() {
    let Some(ch_url) = var("RIVU_TEST_CLICKHOUSE_URL") else {
        println!(
            "跳过 hostile_values_are_data_and_sites_are_isolated：需要设置 RIVU_TEST_CLICKHOUSE_URL"
        );
        return;
    };
    let db = database();
    let writer = rivu_storage_ch::writer_client(&ch_url, &account("writer"), &db);
    let reader = rivu_storage_ch::client(&ch_url, &account("reader"), Some(&db));
    let admin = rivu_storage_ch::client(&ch_url, &account("migrator"), Some(&db));

    let base = 8_000_000_000u64 + (std::process::id() as u64 % 1_000_000) * 10;
    let (site_a, site_b) = (base, base + 1);
    // 两个站点各写同样的一批：带恶意字符串的路径，加两条普通路径
    let mut paths: Vec<String> = NASTY.iter().map(|s| (*s).to_owned()).collect();
    paths.push("/q'uote".into());
    paths.push("/Plain".into());
    for (k, site) in [site_a, site_b].into_iter().enumerate() {
        let rows: Vec<EventRow> = paths
            .iter()
            .enumerate()
            .map(|(i, p)| row(site, i as u64, p))
            .collect();
        insert_events(&writer, &rows, &format!("sec-{base}-{k}"), false)
            .await
            .expect("写入");
    }
    let total = paths.len() as u64;

    let day = json!(["2026-03-10", "2026-03-10"]);
    // 不加过滤：本站点的全部事件，不含另一个站点的
    let all = run(
        &reader,
        site_a,
        json!({"metrics": ["visitors", "pageviews"], "date_range": day}),
    )
    .await
    .unwrap();
    assert_eq!(count(&all), total, "只看到本站点的事件");
    let other = run(
        &reader,
        site_b,
        json!({"metrics": ["visitors"], "date_range": day}),
    )
    .await
    .unwrap();
    assert_eq!(count(&other), total);
    // 一个不存在的站点：什么都看不到
    let none = run(
        &reader,
        base + 7,
        json!({"metrics": ["visitors", "visits"], "date_range": day}),
    )
    .await
    .unwrap();
    assert_eq!(count(&none), 0);

    for p in NASTY {
        // is：恰好命中这一条（取值被当成数据逐字比较）
        for dim in [
            "event:page",
            "visit:source",
            "visit:utm_term",
            "visit:entry_page",
        ] {
            let r = run(
                &reader,
                site_a,
                json!({
                    "metrics": ["visitors", "visits", "pageviews"], "date_range": day,
                    "filters": [["is", dim, [p]]]
                }),
            )
            .await
            .unwrap_or_else(|e| panic!("is {dim} {p:?}: {e}"));
            assert_eq!(count(&r), 1, "is {dim} {p:?}");
            // is_not：其余全部
            let r = run(
                &reader,
                site_a,
                json!({
                    "metrics": ["visitors"], "date_range": day,
                    "filters": [["is_not", dim, [p]]]
                }),
            )
            .await
            .unwrap();
            assert_eq!(count(&r), total - 1, "is_not {dim} {p:?}");
        }
        // contains：子串（取值本身）；通配符：整串匹配（取值里没有 `*`，所以就是逐字相等）
        for op in ["contains", "matches_wildcard"] {
            let r = run(
                &reader,
                site_a,
                json!({
                    "metrics": ["visitors"], "date_range": day,
                    "filters": [[op, "event:page", [p]]]
                }),
            )
            .await
            .unwrap_or_else(|e| panic!("{op} {p:?}: {e}"));
            assert_eq!(count(&r), 1, "{op} {p:?}");
        }
        // 作为维度回显：逐字往返
        let r = run(
            &reader,
            site_a,
            json!({
                "metrics": ["visitors"], "date_range": day, "dimensions": ["event:page"],
                "filters": [["is", "event:page", [p]]]
            }),
        )
        .await
        .unwrap();
        assert_eq!(r.results.len(), 1);
        assert_eq!(r.results[0].dimensions, [p]);
    }

    // 不区分大小写（仅 ASCII）与展示文案
    let r = run(
        &reader,
        site_a,
        json!({
            "metrics": ["visitors"], "date_range": day,
            "filters": [["is", "event:page", ["/PLAIN"], {"case_sensitive": false}]]
        }),
    )
    .await
    .unwrap();
    assert_eq!(count(&r), 1);
    let r = run(
        &reader,
        site_a,
        json!({
            "metrics": ["visitors"], "date_range": day,
            "filters": [["is", "event:page", ["/PLAIN"]]]
        }),
    )
    .await
    .unwrap();
    assert_eq!(count(&r), 0);

    // 正则：用户的正则只在 Hyperscan 里当模式匹配
    let r = run(
        &reader,
        site_a,
        json!({
            "metrics": ["visitors"], "date_range": day,
            "filters": [["matches", "event:page", ["^/q.uote$", "^/back.slash"]]]
        }),
    )
    .await
    .unwrap();
    assert_eq!(count(&r), 2);
    // 取反的通配符
    let r = run(
        &reader,
        site_a,
        json!({
            "metrics": ["visitors"], "date_range": day,
            "filters": [["matches_wildcard_not", "event:page", ["/*"]]]
        }),
    )
    .await
    .unwrap();
    assert_eq!(count(&r), 0, "所有路径都以 / 开头");

    // 校验限制在到达数据库之前就生效
    let eleven: Vec<String> = (0..11).map(|i| format!("a{i}")).collect();
    let err = run(
        &reader,
        site_a,
        json!({
            "metrics": ["visitors"], "date_range": day,
            "filters": [["matches", "event:page", eleven]]
        }),
    )
    .await
    .unwrap_err();
    assert!(matches!(err, QueryError::Validation { .. }), "{err}");
    let err = run(
        &reader,
        site_a,
        json!({
            "metrics": ["visitors"], "date_range": day,
            "filters": [["matches", "event:page", ["a".repeat(257)]]]
        }),
    )
    .await
    .unwrap_err();
    assert!(matches!(err, QueryError::Validation { .. }), "{err}");

    // 数据库侧的纵深防御：`rivu_reader` 的 profile 限制了 Hyperscan 正则长度（256 字节）
    let too_long = "a".repeat(300);
    let res = reader
        .query("SELECT multiMatchAny('x', {r:Array(String)})")
        .param("r", vec![too_long])
        .fetch_one::<u8>()
        .await;
    assert!(
        res.is_err(),
        "profile 里的 max_hyperscan_regexp_length 应当拒绝 300 字节的正则"
    );

    // 表还在，数据没被动过
    let n: u64 = reader
        .query("SELECT count() FROM events WHERE site_id = {s:UInt64}")
        .param("s", site_a)
        .fetch_one()
        .await
        .unwrap();
    assert_eq!(n, total);

    for site in [site_a, site_b] {
        for table in ["events", "sessions"] {
            let _ = admin
                .query(&format!(
                    "ALTER TABLE {table} DELETE WHERE site_id = {{s:UInt64}}"
                ))
                .param("s", site)
                .execute()
                .await;
        }
    }
}

#[tokio::test]
async fn limits_and_scope_errors_are_reported_without_touching_the_database() {
    // 校验错误不需要数据库：用一个指向不存在地址的客户端也应当得到校验错误
    let reader = clickhouse::Client::default().with_url("http://127.0.0.1:1");
    let err = run(
        &reader,
        1,
        json!({
            "metrics": ["conversion_rate"], "date_range": "7d"
        }),
    )
    .await
    .unwrap_err();
    assert_eq!(err.as_validation().unwrap().0, "metrics[0]");
    assert_eq!(err.http_status_hint(), 400);
    // 范围与站点上下文不一致：内部错误（调用方的 bug），不查库
    let spec = QuerySpec::parse(&json!({"metrics": ["visitors"], "date_range": "7d"})).unwrap();
    let err = execute(
        &reader,
        &TenantScope::new(1),
        &SiteContext::new(2, TimeZone::UTC),
        spec,
        ExecOptions::new(5),
    )
    .await
    .unwrap_err();
    assert!(matches!(err, QueryError::Internal(_)), "{err}");
    assert_eq!(err.http_status_hint(), 500);
}
