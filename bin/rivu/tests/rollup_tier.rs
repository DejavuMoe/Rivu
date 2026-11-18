//! 日汇总层对照原始层（`system-design.md` §6.4 选层）：同一份数据、同一个请求，强制用汇总层（`with_rollup(0)`）
//! 与只用原始层的结果必须一致。
//!
//! 数据由确定性的随机生成器产生，经真实管线写入 ClickHouse，然后用 `rivu_query::rollup::build_day`
//! （`rivu_maintainer` 账号）构建全部的天。汇总层的两个已知近似在数据与请求里都避开了：
//! - 会话都不跨本地午夜（访问在 06:00–19:00 开始，最长约 3 小时）；
//! - `sources` 族只按部分维度分组时，同一访客同一天有来源不同的两个会话会在 `visitors` 上多算——这一族的请求不带 `visitors`
//!   （其余族的维度对同一个人恒定：每个人固定一个 IP 与 UA）。
//!
//! 还验证：区间的后半段没有构建时，前半段走汇总层、后半段走原始层（`meta.tiers` 两者都有），结果仍一致；
//! 不兼容的请求（事件级过滤、小时粒度、`percentage`）只走原始层；删除汇总行后回到原始层。
//!
//! 需要真实的 PostgreSQL 和 ClickHouse，缺变量时跳过（同 `query_differential.rs`）。

use std::sync::Arc;
use std::time::Duration;

use clickhouse::Client;
use jiff::Timestamp;
use jiff::civil::{Date, date};
use jiff::tz::TimeZone;
use rand::rngs::StdRng;
use rand::seq::IndexedRandom;
use rand::{Rng, SeedableRng};
use rivu::migrate;
use rivu_common::config::{BufferConfig, ChAccount, Config, Env, Role, Secret};
use rivu_geo::GeoDb;
use rivu_ingest::pipeline::{Outcome, Pipeline, RequestCtx, SaltCache, SiteSnapshot};
use rivu_query::plan::Tier;
use rivu_query::result::{MetricValue, QueryResult};
use rivu_query::rollup;
use rivu_query::{ExecOptions, QuerySpec, SiteContext, TenantScope, execute};
use rivu_storage_pg::sites::NewSite;
use rivu_ua::UaParser;
use rivu_writer::{BatchWriter, ChSink, WriterOptions};
use serde_json::{Value, json};

const CASES: usize = 300;
const TZ: &str = "Asia/Shanghai";

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

fn ch_database() -> String {
    var("RIVU_TEST_CH_DATABASE").unwrap_or_else(|| "rivu".into())
}

const UAS: [&str; 4] = [
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/16.5 Safari/605.1.15 Ddg/17.2",
    "Mozilla/5.0 (Linux; Android 12; Pixel 6) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/99.0.4844.84 Mobile Safari/537.36 Brave/1.40.128",
    "Mozilla/5.0 (X11; Linux x86_64; rv:126.0) Gecko/20100101 Firefox/126.0",
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36",
];
const IPS: [&str; 4] = [
    "89.160.20.128",
    "81.2.69.142",
    "175.16.199.1",
    "203.0.113.50",
];
const PAGES: [&str; 6] = [
    "/",
    "/pricing",
    "/docs",
    "/docs/intro",
    "/blog/a",
    "/contact",
];
const CUSTOM: [&str; 2] = ["Signup", "Download"];
const REFERRERS: [Option<&str>; 5] = [
    None,
    None,
    Some("https://www.google.com/search?q=rivu"),
    Some("https://t.co/abc123"),
    Some("https://blog.example.org/post/1"),
];
const UTMS: [Option<&str>; 3] = [
    None,
    None,
    Some("?utm_source=newsletter&utm_medium=email&utm_campaign=spring"),
];

/// 数据覆盖的本地日。
const FIRST: Date = date(2026, 6, 1);
const LAST: Date = date(2026, 6, 21);

struct Ev {
    person: usize,
    ts_ms: i64,
    name: &'static str,
    path: &'static str,
    referrer: Option<&'static str>,
    utm: Option<&'static str>,
    e: Option<u32>,
    sd: Option<u8>,
}

fn at(tz: &TimeZone, d: Date, h: i8, m: i8, s: i8) -> i64 {
    d.at(h, m, s, 0)
        .to_zoned(tz.clone())
        .unwrap()
        .timestamp()
        .as_millisecond()
}

fn generate(rng: &mut StdRng, tz: &TimeZone, persons: usize) -> Vec<Ev> {
    let days = (LAST - FIRST).get_days() as i64 + 1;
    let mut out = Vec::new();
    for person in 0..persons {
        for _ in 0..rng.random_range(3..10) {
            let day = FIRST
                .checked_add(jiff::Span::new().days(rng.random_range(0..days)))
                .unwrap();
            let mut t = at(
                tz,
                day,
                rng.random_range(6..19),
                rng.random_range(0..60),
                rng.random_range(0..60),
            );
            for k in 0..rng.random_range(1..7) {
                let r = rng.random_range(0..100);
                let (name, e, sd) = if k == 0 || r < 55 {
                    ("pageview", None, None)
                } else if r < 80 {
                    (
                        "engagement",
                        Some(rng.random_range(0..60_000u32)),
                        Some(rng.random_range(0..=100u8)),
                    )
                } else {
                    (*CUSTOM.choose(rng).unwrap(), None, None)
                };
                out.push(Ev {
                    person,
                    ts_ms: t,
                    name,
                    path: PAGES.choose(rng).unwrap(),
                    referrer: *REFERRERS.choose(rng).unwrap(),
                    utm: *UTMS.choose(rng).unwrap(),
                    e,
                    sd,
                });
                // 会话内的间隔不超过 25 分钟，偶尔超过 30 分钟（同一天里的新会话）
                t += if rng.random_bool(0.1) {
                    rng.random_range(1_900_000..2_400_000)
                } else {
                    rng.random_range(5_000..1_500_000)
                };
            }
        }
    }
    out.sort_by_key(|e| e.ts_ms);
    let mut last = i64::MIN;
    for e in &mut out {
        if e.ts_ms <= last {
            e.ts_ms = last + 1;
        }
        last = e.ts_ms;
    }
    out
}

fn body(key: &str, e: &Ev) -> Value {
    let url = if e.name == "engagement" {
        format!("https://example.test{}", e.path)
    } else {
        format!("https://example.test{}{}", e.path, e.utm.unwrap_or(""))
    };
    let mut b = json!({"v": 1, "sv": 1, "s": key, "n": e.name, "u": url});
    if e.name == "engagement" {
        b["e"] = json!(e.e);
        b["sd"] = json!(e.sd);
    } else if let Some(r) = e.referrer {
        b["r"] = json!(r);
    }
    b
}

// ---- 请求生成 -------------------------------------------------------------------------------

/// 每个族：可选的维度、允许的指标。
fn families() -> Vec<(&'static [&'static str], &'static [&'static str])> {
    const SESSION: &[&str] = &[
        "visitors",
        "visits",
        "pageviews",
        "events",
        "views_per_visit",
        "bounce_rate",
        "visit_duration",
        "engaged_duration",
        "time_on_page",
        "scroll_depth",
    ];
    const NO_VISITORS: &[&str] = &[
        "visits",
        "pageviews",
        "events",
        "views_per_visit",
        "bounce_rate",
        "visit_duration",
        "engaged_duration",
        "time_on_page",
        "scroll_depth",
    ];
    vec![
        (&[], SESSION),
        (
            &["event:page"],
            &[
                "visitors",
                "visits",
                "pageviews",
                "events",
                "time_on_page",
                "scroll_depth",
            ],
        ),
        (&["visit:entry_page"], SESSION),
        (&["visit:exit_page"], SESSION),
        (
            &[
                "visit:channel",
                "visit:source",
                "visit:referrer",
                "visit:utm_source",
                "visit:utm_campaign",
            ],
            NO_VISITORS,
        ),
        (&["visit:country", "visit:region", "visit:city"], SESSION),
        (&["visit:device"], SESSION),
        (&["visit:browser", "visit:browser_version"], SESSION),
        (&["visit:os", "visit:os_version"], SESSION),
        (
            &["event:name"],
            &["visitors", "visits", "pageviews", "events"],
        ),
    ]
}

fn filter_for(rng: &mut StdRng, dim: &str) -> Option<Value> {
    let (op, vals): (&str, Vec<Value>) = match dim {
        "visit:country" => ("is", vec![json!("SE"), json!("GB")]),
        "visit:city" => ("is_not", vec![json!(2694762)]),
        "visit:channel" => ("is_not", vec![json!("Direct")]),
        "visit:source" => ("contains", vec![json!("o")]),
        "visit:entry_page" | "visit:exit_page" => ("contains", vec![json!("docs")]),
        "visit:device" => ("is", vec![json!("Desktop")]),
        "visit:browser" => ("is_not", vec![json!("Firefox")]),
        "visit:os" => ("contains", vec![json!("i")]),
        _ => return None,
    };
    let leaf = json!([op, dim, vals]);
    Some(if rng.random_bool(0.2) {
        json!(["not", leaf])
    } else {
        leaf
    })
}

fn spec(rng: &mut StdRng) -> Value {
    let fams = families();
    let (dims_pool, metrics_pool) = fams.choose(rng).unwrap();
    let mut dims: Vec<&str> = dims_pool
        .iter()
        .filter(|_| rng.random_bool(0.5))
        .take(2)
        .copied()
        .collect();
    if dims.is_empty() && !dims_pool.is_empty() {
        dims.push(dims_pool[0]);
    }
    let mut metrics: Vec<&str> = metrics_pool
        .iter()
        .filter(|_| rng.random_bool(0.4))
        .copied()
        .collect();
    if metrics.is_empty() {
        metrics.push(metrics_pool[0]);
    }
    // exit_rate 只能配恰好一个 event:page 维度
    if dims == ["event:page"] && rng.random_bool(0.3) {
        metrics.push("exit_rate");
    }
    let with_time = !metrics.contains(&"exit_rate") && rng.random_bool(0.4);
    let mut all_dims: Vec<Value> = dims.iter().map(|d| json!(d)).collect();
    if with_time {
        all_dims.insert(
            0,
            json!(*["time:day", "time:week", "time:month"].choose(rng).unwrap()),
        );
    }
    let from = FIRST
        .checked_add(jiff::Span::new().days(rng.random_range(-3..15)))
        .unwrap();
    let to = from
        .checked_add(jiff::Span::new().days(rng.random_range(0..20)))
        .unwrap();
    let mut s = json!({
        "metrics": metrics,
        "date_range": [from.to_string(), to.to_string()],
        "dimensions": all_dims,
        "pagination": {"limit": 1000},
    });
    if let Some(d) = dims.choose(rng)
        && rng.random_bool(0.4)
        && let Some(f) = filter_for(rng, d)
    {
        s["filters"] = json!([f]);
    }
    if rng.random_bool(0.25) {
        s["include"] = json!({"compare": "previous_period"});
    }
    s
}

// ---- 比较 -----------------------------------------------------------------------------------

fn same(a: &MetricValue, b: &MetricValue) -> bool {
    match (a, b) {
        (MetricValue::Int(x), MetricValue::Int(y)) => x == y,
        _ => {
            let (x, y) = (a.as_f64(), b.as_f64());
            (x - y).abs() <= 1e-9 * x.abs().max(y.abs()).max(1.0)
        }
    }
}

fn diff(got: &QueryResult, want: &QueryResult) -> Option<String> {
    if got.results.len() != want.results.len() {
        return Some(format!(
            "行数 {} ≠ {}",
            got.results.len(),
            want.results.len()
        ));
    }
    for (i, (g, w)) in got.results.iter().zip(&want.results).enumerate() {
        if g.dimensions != w.dimensions {
            return Some(format!(
                "第 {i} 行维度 {:?} ≠ {:?}",
                g.dimensions, w.dimensions
            ));
        }
        if !g.metrics.iter().zip(&w.metrics).all(|(a, b)| same(a, b)) {
            return Some(format!(
                "第 {i} 行 {:?} 指标 {:?} ≠ {:?}",
                g.dimensions, g.metrics, w.metrics
            ));
        }
        match (&g.comparison, &w.comparison) {
            (None, None) => {}
            (Some(a), Some(b)) if a.metrics.iter().zip(&b.metrics).all(|(x, y)| same(x, y)) => {}
            _ => return Some(format!("第 {i} 行对比期不同")),
        }
    }
    let cmp = |r: &QueryResult| {
        serde_json::to_value(&r.comparison_results)
            .unwrap_or(Value::Null)
            .to_string()
    };
    if cmp(got) != cmp(want) {
        return Some("comparison_results 不同".into());
    }
    None
}

async fn run(
    reader: &Client,
    site: &SiteContext,
    spec: &Value,
    now: Timestamp,
    rollup: bool,
) -> QueryResult {
    let mut opts = ExecOptions::new(15).with_now(now).with_route("rollup_test");
    if rollup {
        opts = opts.with_rollup(0);
    }
    execute(
        reader,
        &TenantScope::new(site.site_id),
        site,
        QuerySpec::parse(spec).expect("合法请求"),
        opts,
    )
    .await
    .unwrap_or_else(|e| panic!("执行失败：{e}\n{spec}"))
}

async fn cleanup(migrator: &Client, site_id: u64) {
    let mut tables = vec!["events", "sessions", "ingest_counters"];
    tables.extend(rollup::Family::ALL.iter().map(|f| f.table()));
    for table in tables {
        let _ = migrator
            .query(&format!(
                "ALTER TABLE {table} DELETE WHERE site_id = {{site:UInt64}}"
            ))
            .param("site", site_id)
            .with_setting("mutations_sync", "1")
            .execute()
            .await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rollup_tier_matches_the_raw_tier() {
    let (Some(pg_url), Some(ch_url)) = (
        var("RIVU_TEST_DATABASE_URL"),
        var("RIVU_TEST_CLICKHOUSE_URL"),
    ) else {
        println!(
            "跳过 rollup_tier_matches_the_raw_tier：需要设置 RIVU_TEST_DATABASE_URL 和 RIVU_TEST_CLICKHOUSE_URL"
        );
        return;
    };
    let migrator = account("migrator");
    let cfg = Config::from_env(
        Role::Migrate,
        &Env::from_pairs([
            ("RIVU_DATABASE_URL", pg_url.as_str()),
            ("RIVU_CLICKHOUSE_URL", ch_url.as_str()),
            ("RIVU_CH_DATABASE", ch_database().as_str()),
            ("RIVU_CH_MIGRATOR_USER", migrator.user.as_str()),
            ("RIVU_CH_MIGRATOR_PASSWORD", migrator.password.expose()),
        ]),
    )
    .expect("测试配置");
    migrate::apply(&cfg, false).await.expect("migrate");

    // ---- 站点、盐、管线 ----------------------------------------------------------------------
    let pg = rivu_storage_pg::connect(&pg_url, 4).await.expect("连接 PG");
    let created = rivu_storage_pg::sites::create_site(
        &pg,
        &NewSite {
            name: "rollup-tier",
            timezone: TZ,
            hostnames: &["example.test".to_owned()],
        },
    )
    .await
    .expect("建站");
    let tz = TimeZone::get(TZ).unwrap();
    let days: Vec<Date> = {
        let mut v = Vec::new();
        let mut d = date(2026, 5, 25);
        while d <= date(2026, 6, 30) {
            v.push(d);
            d = d.tomorrow().unwrap();
        }
        v
    };
    rivu_storage_pg::salts::ensure_salts(&pg, &days)
        .await
        .expect("ensure_salts");
    let snapshot = Arc::new(SiteSnapshot::new());
    snapshot.reload(&pg).await.expect("站点快照");
    let salts = Arc::new(SaltCache::new());
    salts
        .reload(&pg, date(2026, 5, 25), date(2026, 6, 30))
        .await
        .expect("加载盐");
    let geo = Arc::new(GeoDb::with_manual_attribution(Some("test"), None));
    let mmdb = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../crates/geo/tests/data/test-data/GeoIP2-City-Test.mmdb");
    assert!(geo.reload_if_changed(&mmdb).expect("加载测试库"));
    let sink = Arc::new(ChSink::new(rivu_storage_ch::writer_client(
        &ch_url,
        &account("writer"),
        &ch_database(),
    )));
    let mut wopts = WriterOptions::new(BufferConfig {
        flush_interval: Duration::from_millis(100),
        max_rows: 500,
        max_bytes: 8 << 20,
        channel_capacity: 100_000,
    });
    wopts.counter_flush_interval = Duration::from_secs(10);
    let writer = BatchWriter::spawn(Arc::clone(&sink), wopts);
    let pipeline = Pipeline::new(
        Arc::clone(&snapshot),
        Arc::clone(&salts),
        writer.handle(),
        vec!["127.0.0.1/32".parse().unwrap()],
        Arc::new(UaParser::embedded().expect("uap-core")),
        Arc::clone(&geo),
    );

    let mut rng = StdRng::seed_from_u64(0x0000_11D0_2026);
    let events = generate(&mut rng, &tz, 30);
    for e in &events {
        let ctx = RequestCtx {
            peer: "127.0.0.1".parse().unwrap(),
            xff: vec![IPS[e.person % IPS.len()].to_owned()],
            user_agent: UAS[(e.person / IPS.len()) % UAS.len()].to_owned(),
            now_ms: e.ts_ms,
        };
        let out = pipeline.process(body(&created.site_key, e).to_string().as_bytes(), ctx);
        assert!(
            matches!(out, Outcome::Accepted)
                || (e.name == "engagement"
                    && matches!(out, Outcome::Dropped(rivu_domain::DropReason::NoSession))),
            "意外的管线结果 {out:?}"
        );
    }
    drop(pipeline);
    writer.shutdown().await.expect("关停写入器");

    let reader = rivu_storage_ch::client(&ch_url, &account("reader"), Some(&ch_database()));
    let maintainer = rivu_storage_ch::client(&ch_url, &account("maintainer"), Some(&ch_database()));
    let admin = rivu_storage_ch::client(&ch_url, &migrator, Some(&ch_database()));
    let scope = TenantScope::new(created.id);
    let site = SiteContext::new(created.id, tz.clone());
    let now: Timestamp = "2026-07-10T12:00:00+08:00".parse().unwrap();

    // ---- 构建：先只建前半段 --------------------------------------------------------------------
    let state = rollup::build_state(&maintainer, &scope, &tz).await.unwrap();
    assert_eq!(state.built_to, None);
    assert!(state.first_day.is_some_and(|d| d >= FIRST));
    let half = date(2026, 6, 10);
    let mut d = date(2026, 5, 28);
    while d <= half {
        rollup::build_day(&maintainer, &scope, &site, d, 1, now)
            .await
            .unwrap_or_else(|e| panic!("构建 {d}：{e}"));
        d = d.tomorrow().unwrap();
    }
    let state = rollup::build_state(&maintainer, &scope, &tz).await.unwrap();
    assert_eq!(state.built_to, Some(half));
    assert!(!state.tz_changed);

    // 跨越已构建与未构建的区间：两层都用到
    let s = json!({"metrics": ["visitors", "visits", "pageviews", "bounce_rate"], "date_range": ["2026-06-01", "2026-06-21"], "dimensions": ["time:day", "visit:country"]});
    let raw = run(&reader, &site, &s, now, false).await;
    let mixed = run(&reader, &site, &s, now, true).await;
    assert_eq!(raw.meta.tiers, vec![Tier::Raw]);
    assert_eq!(mixed.meta.tiers, vec![Tier::Raw, Tier::Rollup]);
    assert!(raw.results.len() > 10, "数据太少：{}", raw.results.len());
    if let Some(m) = diff(&mixed, &raw) {
        panic!("汇总层 + 原始层与原始层不一致：{m}\n{s}");
    }

    // ---- 建完全部，随机请求 -------------------------------------------------------------------
    let mut d = half.tomorrow().unwrap();
    while d <= date(2026, 7, 1) {
        rollup::build_day(&maintainer, &scope, &site, d, 2, now)
            .await
            .unwrap_or_else(|e| panic!("构建 {d}：{e}"));
        d = d.tomorrow().unwrap();
    }
    // 重复构建同一天是安全的
    rollup::build_day(&maintainer, &scope, &site, date(2026, 6, 5), 3, now)
        .await
        .unwrap();

    let mut rng = StdRng::seed_from_u64(0xBEEF);
    let (mut used_rollup, mut failures) = (0usize, Vec::new());
    for _ in 0..CASES {
        let s = spec(&mut rng);
        let raw = run(&reader, &site, &s, now, false).await;
        let got = run(&reader, &site, &s, now, true).await;
        if got.meta.tiers.contains(&Tier::Rollup) {
            used_rollup += 1;
        }
        if let Some(m) = diff(&got, &raw) {
            failures.push(format!("{m}\n  {s}"));
        }
    }
    println!("随机请求 {CASES} 个，用到汇总层的 {used_rollup} 个");
    assert!(
        failures.is_empty(),
        "{} 个请求不一致：\n{}",
        failures.len(),
        failures
            .iter()
            .take(10)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );
    assert!(used_rollup > CASES * 9 / 10, "大多数请求应当走汇总层");

    // ---- 不兼容的请求只走原始层 -----------------------------------------------------------------
    for s in [
        json!({"metrics": ["visitors"], "date_range": ["2026-06-01", "2026-06-07"], "filters": [["is", "event:page", ["/docs"]]]}),
        json!({"metrics": ["visitors"], "date_range": ["2026-06-01", "2026-06-02"], "dimensions": ["time:hour"]}),
        json!({"metrics": ["visitors", "percentage"], "date_range": ["2026-06-01", "2026-06-07"], "dimensions": ["visit:device"]}),
        json!({"metrics": ["visitors"], "date_range": ["2026-06-01", "2026-06-07"], "dimensions": ["visit:country"], "filters": [["is", "visit:device", ["Desktop"]]]}),
    ] {
        let got = run(&reader, &site, &s, now, true).await;
        assert_eq!(got.meta.tiers, vec![Tier::Raw], "{s}");
    }

    // ---- 保留期：原始数据删掉之后，兼容查询由汇总层给出同样的结果 ------------------------------------
    let keep = json!({"metrics": ["visitors", "visits", "pageviews", "bounce_rate"], "date_range": ["2026-06-01", "2026-06-21"], "dimensions": ["time:day", "visit:device"]});
    let page_filter = json!({"metrics": ["visitors"], "date_range": ["2026-06-01", "2026-06-21"], "filters": [["is", "event:page", ["/docs"]]]});
    let before = run(&reader, &site, &keep, now, false).await;
    let mut tx = pg.begin().await.unwrap();
    rivu_storage_pg::sites::update_site(
        &mut tx,
        created.id,
        &rivu_storage_pg::sites::SitePatch {
            retention_days: Some(Some(30)),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    // 真实的「现在」是 2026-10 之后：这个站点 6 月的数据都过了 30 天
    let (_, deleted) = rivu_jobs::retention::run_once(&pg, &maintainer, &ch_database(), 731)
        .await
        .unwrap();
    assert!(deleted > 0, "应当有分区执行了删除");
    let left: u64 = admin
        .query("SELECT count() FROM events WHERE site_id = ?")
        .bind(created.id)
        .fetch_one()
        .await
        .unwrap();
    assert_eq!(left, 0, "过期的原始事件已删除");
    // 查询按 now = 07-10 计：保留 30 天时 06-11 之前只在汇总层里，不看行数阈值也走汇总层
    let mut kept = site.clone();
    kept.retention_days = Some(30);
    let got = execute(
        &reader,
        &scope,
        &kept,
        QuerySpec::parse(&keep).unwrap(),
        ExecOptions::new(15).with_now(now).with_rollup(u64::MAX),
    )
    .await
    .unwrap();
    assert_eq!(got.meta.tiers, vec![Tier::Rollup]);
    assert!(got.meta.warnings.is_empty(), "{:?}", got.meta.warnings);
    if let Some(m) = diff(&got, &before) {
        panic!("保留期之外由汇总层回答的结果与删除前的原始层不一致：{m}");
    }
    // 不兼容的查询：那些天没有数据，带警告
    let got = run(&reader, &kept, &page_filter, now, true).await;
    assert!(
        got.meta
            .warnings
            .iter()
            .any(|w| w.code == "range_beyond_raw_retention"),
        "{:?}",
        got.meta.warnings
    );
    // ---- 删除汇总行之后回到原始层 ---------------------------------------------------------------
    rollup::delete(
        &maintainer,
        &scope,
        Some((date(2026, 6, 1), date(2026, 6, 30))),
        false,
    )
    .await
    .unwrap();
    let state = rollup::build_state(&maintainer, &scope, &tz).await.unwrap();
    assert_eq!(state.built_to, Some(date(2026, 7, 1)));
    rollup::delete(&maintainer, &scope, None, true)
        .await
        .unwrap();
    let state = rollup::build_state(&maintainer, &scope, &tz).await.unwrap();
    assert_eq!(state.built_to, None);
    let s = json!({"metrics": ["visitors"], "date_range": ["2026-06-01", "2026-06-21"]});
    assert_eq!(
        run(&reader, &site, &s, now, true).await.meta.tiers,
        vec![Tier::Raw]
    );

    cleanup(&admin, created.id).await;
}
