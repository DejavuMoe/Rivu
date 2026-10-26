//! 公开查询 API（`POST /api/v1/query`、`GET /api/v1/query/schema.json`）的端到端测试：
//! 真实的 PostgreSQL 与 ClickHouse，真实的本地 TCP 端口，路由由 `public::router_full` 装配（与 `rivu all` 同一条路径）。
//!
//! 缺少环境变量时跳过并打印原因（同 `e2e_lite.rs`）：
//!   RIVU_TEST_DATABASE_URL     有 DDL 与写权限的 PostgreSQL 连接串（rivu_migrator）
//!   RIVU_TEST_CLICKHOUSE_URL   ClickHouse HTTP 地址
//! 可选：RIVU_TEST_CH_{MIGRATOR,WRITER,READER}_{USER,PASSWORD}、RIVU_TEST_CH_DATABASE。
//!
//! 用例：
//! 1. `http_results_equal_direct_execution`：经管线写入一组已知事件（固定的历史时间，两个站点：UTC 与
//!    Asia/Kathmandu + 周日起始），若干 QuerySpec 的 HTTP 响应与直接调用 `rivu_query::execute` 的结果
//!    **逐字节**相等（同一个「现在」、同一份站点上下文），并核对 `log_comment` 带 route / site / org / api_key_id；
//! 2. `errors_follow_the_documented_chain`：400 / 401 / 403 / 404 / 501 与 schema.json；
//! 3. `hourly_quota_exhaustion_is_429_with_headers`：组织小时配额用尽后 429 + `Retry-After` + `X-RateLimit-*`。

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use clickhouse::Client;
use jiff::Timestamp;
use jiff::civil::{Date, Weekday, date};
use jiff::tz::TimeZone;
use rivu::apikey_cmd::issue_key;
use rivu::cli::ApikeyCreateArgs;
use rivu::query_api::{self, QueryApi};
use rivu::{api_auth, migrate, public};
use rivu_common::config::{BufferConfig, ChAccount, Config, Env, Role, Secret};
use rivu_geo::GeoDb;
use rivu_ingest::pipeline::{Outcome, Pipeline, RequestCtx, SaltCache, SiteSnapshot};
use rivu_query::{ExecOptions, QuerySpec, SiteContext, TenantScope, execute};
use rivu_storage_pg::sites::NewSite;
use rivu_ua::UaParser;
use rivu_writer::{BatchWriter, ChSink, WriterOptions};
use serde_json::{Value, json};
use sqlx::PgPool;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::OnceCell;

const HOST: &str = "example.test";
/// 处理函数与直接调用共用的「现在」：数据全在它之前。
const NOW: &str = "2026-03-12T12:00:00Z";

const UAS: [&str; 7] = [
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/118.0.5993.771 YaBrowser/23.11.2.771 Yowser/2.5 Safari/537.36",
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/16.5 Safari/605.1.15 Ddg/17.2",
    "Mozilla/5.0 (iPhone; CPU iPhone OS 17_2_1 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.2 Mobile/15E148 Safari/604.1 Ddg/17.2",
    "Mozilla/5.0 (iPad; CPU OS 12_5_5 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/12.0 EdgiOS/46.3.26 Mobile/15E148 Safari/605.1.15",
    "Mozilla/5.0 (Linux; Android 12; Pixel 6) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/99.0.4844.84 Mobile Safari/537.36 Brave/1.40.128",
    "Mozilla/5.0 (X11; Linux x86_64; rv:126.0) Gecko/20100101 Firefox/126.0",
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36",
];
/// MaxMind 官方测试库里的地址（瑞典、英国、中国、日本）与一个没有位置的文档地址。
const IPS: [&str; 5] = [
    "89.160.20.128",
    "81.2.69.142",
    "175.16.199.1",
    "2001:218::1",
    "203.0.113.50",
];

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

struct TestEnv {
    pg_url: String,
    ch_url: String,
}

fn test_env(test: &str) -> Option<TestEnv> {
    let (Some(pg_url), Some(ch_url)) = (
        var("RIVU_TEST_DATABASE_URL"),
        var("RIVU_TEST_CLICKHOUSE_URL"),
    ) else {
        println!("跳过 {test}：需要设置 RIVU_TEST_DATABASE_URL 和 RIVU_TEST_CLICKHOUSE_URL");
        return None;
    };
    Some(TestEnv { pg_url, ch_url })
}

async fn migrate_once(env: &TestEnv) {
    static DONE: OnceCell<()> = OnceCell::const_new();
    DONE.get_or_init(|| async {
        let migrator = account("migrator");
        let cfg_env = Env::from_pairs([
            ("RIVU_DATABASE_URL", env.pg_url.as_str()),
            ("RIVU_CLICKHOUSE_URL", env.ch_url.as_str()),
            ("RIVU_CH_DATABASE", ch_database().as_str()),
            ("RIVU_CH_MIGRATOR_USER", migrator.user.as_str()),
            ("RIVU_CH_MIGRATOR_PASSWORD", migrator.password.expose()),
        ]);
        let config = Config::from_env(Role::Migrate, &cfg_env).expect("测试配置");
        migrate::apply(&config, false).await.expect("migrate");
    })
    .await;
}

fn api_config(env: &TestEnv) -> Config {
    let reader = account("reader");
    let pairs = [
        ("RIVU_DATABASE_URL", env.pg_url.clone()),
        ("RIVU_CLICKHOUSE_URL", env.ch_url.clone()),
        ("RIVU_CH_DATABASE", ch_database()),
        ("RIVU_CH_READER_USER", reader.user.clone()),
        (
            "RIVU_CH_READER_PASSWORD",
            reader.password.expose().to_owned(),
        ),
    ];
    Config::from_env(Role::Api, &Env::from_pairs(pairs)).expect("测试配置")
}

fn unique(prefix: &str) -> String {
    format!("{prefix}-{:08x}", rand::random::<u32>())
}

fn ua_parser() -> Arc<UaParser> {
    static PARSER: std::sync::OnceLock<Arc<UaParser>> = std::sync::OnceLock::new();
    Arc::clone(PARSER.get_or_init(|| Arc::new(UaParser::embedded().expect("uap-core 规则"))))
}

async fn cleanup(env: &TestEnv, site_ids: &[u64]) {
    let admin = rivu_storage_ch::client(&env.ch_url, &account("migrator"), Some(&ch_database()));
    for site_id in site_ids {
        for table in ["events", "sessions", "ingest_counters"] {
            let _ = admin
                .query(&format!(
                    "ALTER TABLE {table} DELETE WHERE site_id = {{site:UInt64}}"
                ))
                .param("site", *site_id)
                .execute()
                .await;
        }
    }
}

// ------------------------------------------------------------------ 组织、用户、站点、key

struct Org {
    id: i64,
    slug: String,
    email: String,
}

/// 建组织、用户与成员关系（直接 SQL：用户管理还没有 CLI）。
async fn make_org(pg: &PgPool) -> Org {
    let slug = unique("org");
    let email = format!("{}@example.test", unique("user"));
    let org_id: i64 =
        sqlx::query_scalar("INSERT INTO organizations (name, slug) VALUES ($1, $1) RETURNING id")
            .bind(&slug)
            .fetch_one(pg)
            .await
            .unwrap();
    let user_id: i64 = sqlx::query_scalar(
        "INSERT INTO users (email, password_hash) VALUES ($1, 'x') RETURNING id",
    )
    .bind(&email)
    .fetch_one(pg)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO memberships (organization_id, user_id, role) VALUES ($1, $2, 'owner')",
    )
    .bind(org_id)
    .bind(user_id)
    .execute(pg)
    .await
    .unwrap();
    Org {
        id: org_id,
        slug,
        email,
    }
}

#[derive(Clone)]
struct Site {
    id: u64,
    key: String,
    tz: &'static str,
    week_start: Weekday,
}

async fn make_site(
    pg: &PgPool,
    org: &Org,
    tz: &'static str,
    week_start: Weekday,
    public: bool,
) -> Site {
    let hostnames = vec![HOST.to_owned()];
    let created = rivu_storage_pg::sites::create_site(
        pg,
        &NewSite {
            name: "public-api-e2e",
            timezone: tz,
            hostnames: &hostnames,
        },
    )
    .await
    .expect("建站");
    sqlx::query("UPDATE sites SET organization_id = $1, is_public = $2 WHERE id = $3")
        .bind(org.id)
        .bind(public)
        .bind(created.id as i64)
        .execute(pg)
        .await
        .unwrap();
    sqlx::query("UPDATE site_settings SET week_starts_on = $1 WHERE site_id = $2")
        .bind(i16::from(week_start.to_sunday_zero_offset()))
        .bind(created.id as i64)
        .execute(pg)
        .await
        .unwrap();
    Site {
        id: created.id,
        key: created.site_key,
        tz,
        week_start,
    }
}

struct Issued {
    id: i64,
    key: String,
}

async fn new_key(pg: &PgPool, org: &Org, scopes: &[&str]) -> Issued {
    let issued = issue_key(
        pg,
        &ApikeyCreateArgs {
            org: org.slug.clone(),
            user: Some(org.email.clone()),
            name: "public-api-e2e".into(),
            scopes: scopes.iter().map(|s| (*s).to_owned()).collect(),
            expires: None,
        },
    )
    .await
    .expect("创建 key");
    Issued {
        id: issued.id,
        key: issued.plaintext,
    }
}

// ------------------------------------------------------------------ 真实 HTTP

struct Resp {
    status: u16,
    headers: Vec<(String, String)>,
    body: String,
}

impl Resp {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    fn json(&self) -> Value {
        serde_json::from_str(&self.body)
            .unwrap_or_else(|e| panic!("响应体不是 JSON（{e}）：{}", self.body))
    }
}

#[derive(Clone, Copy)]
struct Http {
    addr: SocketAddr,
}

impl Http {
    async fn request(
        &self,
        method: &str,
        path: &str,
        key: Option<&str>,
        extra: &[&str],
        body: &[u8],
    ) -> Resp {
        let mut stream = TcpStream::connect(self.addr).await.unwrap();
        let mut req = format!(
            "{method} {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nContent-Length: {}\r\n",
            body.len()
        );
        if let Some(k) = key {
            req.push_str(&format!("Authorization: Bearer {k}\r\n"));
        }
        for h in extra {
            req.push_str(h);
            req.push_str("\r\n");
        }
        req.push_str("\r\n");
        stream.write_all(req.as_bytes()).await.unwrap();
        stream.write_all(body).await.unwrap();
        let mut raw = Vec::new();
        stream.read_to_end(&mut raw).await.unwrap();
        let split = raw
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
            .map_or(raw.len(), |i| i + 4);
        let head = String::from_utf8_lossy(&raw[..split]).into_owned();
        let mut lines = head.lines();
        let status = lines
            .next()
            .unwrap()
            .split_whitespace()
            .nth(1)
            .unwrap()
            .parse()
            .unwrap();
        Resp {
            status,
            headers: lines
                .filter_map(|l| l.split_once(':'))
                .map(|(k, v)| (k.trim().to_owned(), v.trim().to_owned()))
                .collect(),
            body: String::from_utf8_lossy(&raw[split..]).into_owned(),
        }
    }

    async fn query(&self, key: Option<&str>, spec: &Value) -> Resp {
        self.request(
            "POST",
            query_api::QUERY_PATH,
            key,
            &["Content-Type: application/json"],
            spec.to_string().as_bytes(),
        )
        .await
    }

    async fn query_raw(&self, key: Option<&str>, body: &str) -> Resp {
        self.request(
            "POST",
            query_api::QUERY_PATH,
            key,
            &["Content-Type: application/json"],
            body.as_bytes(),
        )
        .await
    }
}

struct Server {
    http: Http,
    geo: Arc<GeoDb>,
    reader: Client,
}

/// 起一个只含公开查询 API 的 api 角色路由（与 `rivu all` 同一条 `router_full` 路径），固定「现在」。
async fn start_server(env: &TestEnv, pg: &PgPool) -> Server {
    let config = api_config(env);
    let guard = api_auth::build_guard(&config, pg.clone());
    let geo = Arc::new(GeoDb::with_manual_attribution(
        Some("MaxMind test database"),
        Some("https://github.com/maxmind/MaxMind-DB"),
    ));
    let mmdb = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../crates/geo/tests/data/test-data/GeoIP2-City-Test.mmdb");
    assert!(geo.reload_if_changed(&mmdb).expect("加载测试库"));
    let reader = rivu_storage_ch::client(&env.ch_url, &account("reader"), Some(&ch_database()));
    let now: Timestamp = NOW.parse().unwrap();
    let api = QueryApi::new(
        guard,
        pg.clone(),
        reader.clone(),
        Some(Arc::clone(&geo)),
        config.query_timeouts.api_secs,
    )
    .with_clock(move || now);
    let app = public::router_full(
        Role::Api,
        None,
        None,
        Some(query_api::router(Arc::new(api))),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .unwrap()
    });
    Server {
        http: Http { addr },
        geo,
        reader,
    }
}

// ------------------------------------------------------------------ 经管线写入已知事件

struct Ev {
    ts_ms: i64,
    person: usize,
    name: &'static str,
    path: &'static str,
    referrer: Option<&'static str>,
    utm: Option<&'static str>,
    e: Option<u32>,
    sd: Option<u8>,
}

fn at(d: Date, h: i8, m: i8, s: i8) -> i64 {
    d.at(h, m, s, 0)
        .to_zoned(TimeZone::UTC)
        .unwrap()
        .timestamp()
        .as_millisecond()
}

/// 8 个人 × 3 天（2026-03-09 至 11）：每天各一个会话 `/` → `/pricing` → engagement（偶数的人再有一个 Signup），
/// 再加 3 月 12 日 11:58 的两个页面浏览（给 `realtime` 用）。UTC 时间，对两个站点相同。
fn known_events() -> Vec<Ev> {
    let mut out = Vec::new();
    for person in 0..8usize {
        for day in [9i8, 10, 11] {
            let base = at(date(2026, 3, day), 9 + person as i8, 0, 0);
            out.push(Ev {
                ts_ms: base,
                person,
                name: "pageview",
                path: "/",
                referrer: [
                    None,
                    Some("https://www.google.com/search?q=rivu"),
                    Some("https://t.co/abc"),
                ][person % 3],
                utm: (person % 4 == 3).then_some("?utm_source=newsletter&utm_medium=email"),
                e: None,
                sd: None,
            });
            out.push(Ev {
                ts_ms: base + 60_000,
                person,
                name: "pageview",
                path: "/pricing",
                referrer: None,
                utm: None,
                e: None,
                sd: None,
            });
            out.push(Ev {
                ts_ms: base + 90_000,
                person,
                name: "engagement",
                path: "/pricing",
                referrer: None,
                utm: None,
                e: Some(5000 + 1000 * person as u32),
                sd: Some(40 + person as u8 * 5),
            });
            if person % 2 == 0 {
                out.push(Ev {
                    ts_ms: base + 120_000,
                    person,
                    name: "Signup",
                    path: "/pricing",
                    referrer: None,
                    utm: None,
                    e: None,
                    sd: None,
                });
            }
        }
    }
    for person in 0..2usize {
        out.push(Ev {
            ts_ms: at(date(2026, 3, 12), 11, 58, person as i8),
            person,
            name: "pageview",
            path: "/",
            referrer: None,
            utm: None,
            e: None,
            sd: None,
        });
    }
    out.sort_by_key(|e| e.ts_ms);
    out
}

fn body_for(site: &Site, e: &Ev) -> Value {
    let url = format!(
        "https://{HOST}{}{}",
        e.path,
        if e.name == "engagement" {
            ""
        } else {
            e.utm.unwrap_or("")
        }
    );
    let mut body = json!({"v": 1, "sv": 1, "s": site.key, "n": e.name, "u": url});
    if e.name == "engagement" {
        body["e"] = json!(e.e.unwrap());
        body["sd"] = json!(e.sd.unwrap());
    } else if let Some(r) = e.referrer {
        body["r"] = json!(r);
    }
    body
}

/// 把 `known_events` 经真实管线写进这些站点（固定的历史时间，所以先备好那几天的盐），返回每个站点被接受的事件数。
async fn ingest(env: &TestEnv, pg: &PgPool, sites: &[Site], geo: &Arc<GeoDb>) -> Vec<usize> {
    let mut days = Vec::new();
    let mut d = date(2026, 3, 6);
    while d <= date(2026, 3, 14) {
        days.push(d);
        d = d.tomorrow().unwrap();
    }
    rivu_storage_pg::salts::ensure_salts(pg, &days)
        .await
        .expect("ensure_salts");
    let snapshot = Arc::new(SiteSnapshot::new());
    snapshot.reload(pg).await.expect("加载站点快照");
    let salts = Arc::new(SaltCache::new());
    salts
        .reload(pg, date(2026, 3, 5), date(2026, 3, 15))
        .await
        .expect("加载盐");

    let mut wopts = WriterOptions::new(BufferConfig {
        flush_interval: Duration::from_millis(100),
        max_rows: 500,
        max_bytes: 8 << 20,
        channel_capacity: 100_000,
    });
    wopts.counter_flush_interval = Duration::from_secs(10);
    let writer = BatchWriter::spawn(
        Arc::new(ChSink::new(rivu_storage_ch::writer_client(
            &env.ch_url,
            &account("writer"),
            &ch_database(),
        ))),
        wopts,
    );
    let pipeline = Pipeline::new(
        snapshot,
        salts,
        writer.handle(),
        vec!["127.0.0.1/32".parse().unwrap()],
        ua_parser(),
        Arc::clone(geo),
    );
    let events = known_events();
    let mut accepted = Vec::new();
    for site in sites {
        let mut n = 0;
        for e in &events {
            let out = pipeline.process(
                body_for(site, e).to_string().as_bytes(),
                RequestCtx {
                    peer: "127.0.0.1".parse().unwrap(),
                    xff: vec![IPS[e.person % IPS.len()].to_owned()],
                    user_agent: UAS[e.person % UAS.len()].to_owned(),
                    now_ms: e.ts_ms,
                },
            );
            assert_eq!(out, Outcome::Accepted, "{} {}", e.name, e.path);
            n += 1;
        }
        accepted.push(n);
    }
    drop(pipeline);
    writer.shutdown().await.expect("关停写入器");
    accepted
}

// ------------------------------------------------------------------ 直接执行（对照）

/// 与处理函数相同的站点上下文与「现在」，直接调 `rivu_query::execute`；返回结果的 JSON 文本。
async fn direct(server: &Server, site: &Site, spec: &Value) -> (String, Value) {
    let ctx = SiteContext {
        site_id: site.id,
        site_key: Some(site.key.clone()),
        timezone: TimeZone::get(site.tz).unwrap(),
        week_starts_on: site.week_start,
        geo_attribution: server.geo.attribution(),
        geo_capability: server.geo.capability(),
        goals: Vec::new(),
    };
    let result = execute(
        &server.reader,
        &TenantScope::new(site.id),
        &ctx,
        QuerySpec::parse(spec).expect("合法的 QuerySpec"),
        ExecOptions::new(20)
            .with_now(NOW.parse().unwrap())
            .with_route("public-api-test"),
    )
    .await
    .expect("直接执行");
    (
        serde_json::to_string(&result).unwrap(),
        serde_json::to_value(&result).unwrap(),
    )
}

/// 与 `rivu run` 相同的桶配置的 Prometheus recorder（只装在当前线程）。
fn metrics_recorder() -> (
    metrics_exporter_prometheus::PrometheusRecorder,
    metrics_exporter_prometheus::PrometheusHandle,
) {
    let recorder = metrics_exporter_prometheus::PrometheusBuilder::new()
        .set_buckets_for_metric(
            metrics_exporter_prometheus::Matcher::Full("rivu_query_seconds".to_owned()),
            &query_api::QUERY_SECONDS_BUCKETS,
        )
        .unwrap()
        .build_recorder();
    let handle = recorder.handle();
    (recorder, handle)
}

/// 取 Prometheus 文本里某条序列（`name{labels}`）的值。
fn sample(text: &str, series: &str) -> Option<f64> {
    text.lines()
        .find_map(|l| l.strip_prefix(series)?.trim().parse().ok())
}

fn rows(v: &Value) -> usize {
    v["results"].as_array().map_or(0, Vec::len)
}

/// 一个等价性用例：名称、站点序号、请求（不含 `site`）、结果至少应有的行数。
struct Case {
    name: &'static str,
    site: usize,
    spec: Value,
    min_rows: usize,
}

fn cases() -> Vec<Case> {
    let mar = |a: &str, b: &str| json!([format!("2026-03-{a}"), format!("2026-03-{b}")]);
    vec![
        Case {
            name: "overview",
            site: 0,
            spec: json!({"metrics": ["visitors", "visits", "pageviews", "bounce_rate", "visit_duration", "views_per_visit"],
                         "date_range": mar("09", "11")}),
            min_rows: 1,
        },
        Case {
            name: "按页面：排序、总行数",
            site: 0,
            spec: json!({"metrics": ["visitors", "pageviews"], "dimensions": ["event:page"],
                         "date_range": mar("09", "11"), "order_by": [["pageviews", "desc"]],
                         "include": {"total_rows": true}}),
            min_rows: 2,
        },
        Case {
            name: "国家与设备（meta 带地理署名）",
            site: 0,
            spec: json!({"metrics": ["visitors", "visits"], "dimensions": ["visit:country", "visit:device"],
                         "date_range": mar("09", "11")}),
            min_rows: 2,
        },
        Case {
            name: "过滤：设备 + 页面",
            site: 0,
            spec: json!({"metrics": ["pageviews", "visitors", "bounce_rate"], "dimensions": ["visit:source"],
                         "filters": [["is", "visit:device", ["desktop", "mobile"]], ["contains", "event:page", ["pric"]]],
                         "date_range": mar("09", "11")}),
            min_rows: 1,
        },
        Case {
            name: "过滤：or 组合与 percentage",
            site: 0,
            spec: json!({"metrics": ["visitors", "percentage"], "dimensions": ["visit:channel"],
                         "filters": [["or", [["is", "visit:country", ["SE"]], ["is", "visit:country", ["GB"]]]]],
                         "date_range": mar("09", "11")}),
            min_rows: 1,
        },
        Case {
            name: "compare：维度行带 comparison",
            site: 0,
            spec: json!({"metrics": ["visitors", "pageviews"], "dimensions": ["event:page"],
                         "date_range": mar("10", "11"), "include": {"compare": "previous_period"}}),
            min_rows: 2,
        },
        Case {
            name: "compare：时间序列，comparison_results 与对比标签",
            site: 0,
            spec: json!({"metrics": ["pageviews", "visits"], "dimensions": ["time:day"],
                         "date_range": mar("10", "11"),
                         "include": {"time_labels": true, "compare": "previous_period"}}),
            min_rows: 2,
        },
        Case {
            name: "time_labels：小时桶",
            site: 0,
            spec: json!({"metrics": ["visitors"], "dimensions": ["time:hour"],
                         "date_range": mar("10", "10"), "include": {"time_labels": true}}),
            min_rows: 8,
        },
        Case {
            name: "入口页",
            site: 0,
            spec: json!({"metrics": ["visits", "bounce_rate"], "dimensions": ["visit:entry_page"],
                         "date_range": mar("09", "11")}),
            min_rows: 1,
        },
        Case {
            name: "退出率（按页面）",
            site: 0,
            spec: json!({"metrics": ["exit_rate", "pageviews"], "dimensions": ["event:page"],
                         "date_range": mar("09", "11")}),
            min_rows: 2,
        },
        Case {
            name: "分页与总行数",
            site: 0,
            spec: json!({"metrics": ["pageviews"], "dimensions": ["event:page"], "date_range": mar("09", "11"),
                         "order_by": [["pageviews", "desc"]],
                         "pagination": {"limit": 1, "offset": 1}, "include": {"total_rows": true}}),
            min_rows: 1,
        },
        Case {
            name: "自定义事件",
            site: 0,
            spec: json!({"metrics": ["events", "visitors"], "dimensions": ["event:name"],
                         "date_range": mar("09", "11")}),
            min_rows: 2,
        },
        Case {
            name: "realtime（固定的现在）",
            site: 0,
            spec: json!({"metrics": ["visitors"], "date_range": "realtime"}),
            min_rows: 1,
        },
        Case {
            name: "相对范围 7d + 日桶",
            site: 0,
            spec: json!({"metrics": ["pageviews"], "dimensions": ["time:day"], "date_range": "7d",
                         "include": {"time_labels": true}}),
            min_rows: 3,
        },
        Case {
            name: "Kathmandu：周桶（周日起始）与时间标签",
            site: 1,
            spec: json!({"metrics": ["visitors", "pageviews"], "dimensions": ["time:week"],
                         "date_range": "28d", "include": {"time_labels": true}}),
            min_rows: 1,
        },
        Case {
            name: "Kathmandu：本月 + 日桶 + 对比",
            site: 1,
            spec: json!({"metrics": ["pageviews", "visits", "visit_duration"], "dimensions": ["time:day"],
                         "date_range": "month", "include": {"time_labels": true, "compare": "previous_period"}}),
            min_rows: 3,
        },
        Case {
            name: "Kathmandu：engagement 指标",
            site: 1,
            spec: json!({"metrics": ["time_on_page", "scroll_depth", "engaged_duration"], "dimensions": ["event:page"],
                         "date_range": mar("09", "11")}),
            min_rows: 1,
        },
    ]
}

// ------------------------------------------------------------------ 用例 1

#[tokio::test]
async fn http_results_equal_direct_execution() {
    let Some(env) = test_env("http_results_equal_direct_execution") else {
        return;
    };
    migrate_once(&env).await;
    let pg = rivu_storage_pg::connect(&env.pg_url, 8)
        .await
        .expect("连接 PG");
    let org = make_org(&pg).await;
    let sites = [
        make_site(&pg, &org, "UTC", Weekday::Monday, false).await,
        make_site(&pg, &org, "Asia/Kathmandu", Weekday::Sunday, false).await,
    ];
    let key = new_key(&pg, &org, &["stats:read"]).await;
    // 本线程的 recorder（单线程运行时：服务端任务与测试在同一线程，指标都落进它）
    let (recorder, metrics) = metrics_recorder();
    let _guard = metrics::set_default_local_recorder(&recorder);
    let server = start_server(&env, &pg).await;
    let accepted = ingest(&env, &pg, &sites, &server.geo).await;
    println!("经管线写入：每站 {accepted:?} 个事件");

    let mut checked = 0;
    for case in cases() {
        let site = &sites[case.site];
        let mut spec = case.spec.clone();
        spec["site"] = json!(site.key);

        let (direct_text, direct_value) = direct(&server, site, &spec).await;
        let r = server.http.query(Some(&key.key), &spec).await;
        assert_eq!(r.status, 200, "{}：{}", case.name, r.body);
        assert_eq!(
            r.header("content-type"),
            Some("application/json"),
            "{}",
            case.name
        );
        assert_eq!(r.header("cache-control"), Some("no-store"), "{}", case.name);
        assert!(r.header("x-ratelimit-limit").is_some(), "{}", case.name);
        assert_eq!(r.header("access-control-allow-origin"), None, "不开 CORS");
        // HTTP 层没有改变语义：解析后逐字段相等，序列化后逐字节相等
        let got = r.json();
        assert_eq!(
            got, direct_value,
            "{}：HTTP 与直接执行的结果必须逐字段相等",
            case.name
        );
        assert_eq!(r.body, direct_text, "{}：逐字节相等", case.name);
        // 防止「空对空」的假通过
        assert!(
            rows(&got) >= case.min_rows,
            "{}：行数 {} < {}\n{}",
            case.name,
            rows(&got),
            case.min_rows,
            r.body
        );
        // 站点设置确实来自站点：时区回显
        assert_eq!(got["query"]["time_zone"], site.tz, "{}", case.name);
        checked += 1;
    }
    println!("{checked} 个 QuerySpec 的 HTTP 响应与直接执行逐字节相等");

    // ---- 已知答案（数据是确定的）-------------------------------------------------------------
    let site = &sites[0];
    let r = server
        .http
        .query(
            Some(&key.key),
            &json!({"site": site.key, "metrics": ["pageviews", "visits", "events"],
                    "date_range": ["2026-03-09", "2026-03-11"]}),
        )
        .await
        .json();
    // 8 人 × 3 天 × 2 次页面浏览；24 个会话；events 不含 engagement：48 + 12 个 Signup
    assert_eq!(r["results"][0]["metrics"], json!([48, 24, 60]), "{r}");
    assert_eq!(r["query"]["time_zone"], "UTC");

    let r = server
        .http
        .query(
            Some(&key.key),
            &json!({"site": site.key, "metrics": ["visitors"], "dimensions": ["visit:country"],
                    "date_range": ["2026-03-09", "2026-03-11"]}),
        )
        .await
        .json();
    assert_eq!(
        r["meta"]["geo_attribution"]["source"], "custom",
        "含位置维度时回显署名：{r}"
    );
    assert_eq!(r["meta"]["geo_capability"], "city");

    let r = server
        .http
        .query(
            Some(&key.key),
            &json!({"site": sites[1].key, "metrics": ["visitors"], "dimensions": ["time:week"],
                    "date_range": "28d", "include": {"time_labels": true}}),
        )
        .await
        .json();
    assert_eq!(r["query"]["time_zone"], "Asia/Kathmandu");
    let labels = r["meta"]["time_labels"].as_array().unwrap();
    let first: Date = labels[0].as_str().unwrap().parse().unwrap();
    assert_eq!(
        first.weekday(),
        Weekday::Sunday,
        "周起始日来自站点设置：{labels:?}"
    );

    // ---- Prometheus：每个请求计一次，查询耗时直方图每次执行计一次 --------------------------------
    let text = metrics.render();
    let executed = (checked + 3) as f64;
    assert_eq!(
        sample(
            &text,
            "rivu_api_requests_total{route=\"api.query\",status=\"200\"}"
        ),
        Some(executed),
        "{text}"
    );
    assert_eq!(
        sample(&text, "rivu_query_seconds_count{route=\"api.query\"}"),
        Some(executed),
        "{text}"
    );
    assert!(
        sample(
            &text,
            "rivu_query_seconds_bucket{route=\"api.query\",le=\"0.005\"}"
        )
        .is_some(),
        "固定桶的直方图（不是 summary）：{text}"
    );
    assert!(!text.contains(&key.key), "指标里没有 key 明文");
    assert!(!text.contains(&sites[0].key), "指标不带站点标签");

    // ---- log_comment：route / site / org / api_key_id，不含明文 key ---------------------------
    let admin = rivu_storage_ch::client(&env.ch_url, &account("migrator"), Some(&ch_database()));
    match admin.query("SYSTEM FLUSH LOGS").execute().await {
        Ok(()) => {
            let want = format!(
                "SELECT count() FROM system.query_log \
                 WHERE type = 'QueryFinish' AND event_time > now() - INTERVAL 10 MINUTE \
                   AND JSONExtractString(log_comment, 'route') = 'api.query' \
                   AND JSONExtractUInt(log_comment, 'site') = {} \
                   AND JSONExtractUInt(log_comment, 'org') = {} \
                   AND JSONExtractUInt(log_comment, 'api_key_id') = {}",
                site.id, org.id, key.id
            );
            let n: u64 = admin.query(&want).fetch_one().await.expect("查 query_log");
            assert!(
                n > 0,
                "system.query_log 里应有带 route/site/org/api_key_id 的记录"
            );
            let leaked: u64 = admin
                .query(
                    "SELECT count() FROM system.query_log \
                     WHERE event_time > now() - INTERVAL 10 MINUTE \
                       AND JSONExtractString(log_comment, 'route') = 'api.query' \
                       AND position(log_comment, 'rivu_') > 0",
                )
                .fetch_one()
                .await
                .unwrap();
            assert_eq!(leaked, 0, "log_comment 不含 key 明文");
            println!(
                "log_comment 校验通过：{n} 条 api.query 记录（site={}, org={}, api_key_id={}）",
                site.id, org.id, key.id
            );
        }
        Err(e) => println!("跳过 log_comment 校验：当前账号不能 FLUSH LOGS（{e}）"),
    }

    cleanup(&env, &[sites[0].id, sites[1].id]).await;
}

// ------------------------------------------------------------------ 用例 2

#[tokio::test]
async fn errors_follow_the_documented_chain() {
    let Some(env) = test_env("errors_follow_the_documented_chain") else {
        return;
    };
    migrate_once(&env).await;
    let pg = rivu_storage_pg::connect(&env.pg_url, 8)
        .await
        .expect("连接 PG");
    let org = make_org(&pg).await;
    let other = make_org(&pg).await;
    let site = make_site(&pg, &org, "UTC", Weekday::Monday, false).await;
    let second = make_site(&pg, &org, "UTC", Weekday::Monday, false).await;
    let foreign = make_site(&pg, &other, "UTC", Weekday::Monday, false).await;
    let foreign_public = make_site(&pg, &other, "UTC", Weekday::Monday, true).await;
    let key = new_key(&pg, &org, &["stats:read"]).await;
    let narrow = new_key(&pg, &org, &[&format!("stats:read:{}", site.key)]).await;
    let sites_only = new_key(&pg, &org, &["sites:read"]).await;
    let (recorder, metrics) = metrics_recorder();
    let _guard = metrics::set_default_local_recorder(&recorder);
    let server = start_server(&env, &pg).await;
    let http = server.http;

    let good = |site_key: &str| json!({"site": site_key, "metrics": ["visitors"], "date_range": ["2026-03-09", "2026-03-11"]});

    // ---- 200：自己的站点；别的组织的公开站点可读统计 -----------------------------------------
    let r = http.query(Some(&key.key), &good(&site.key)).await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.header("cache-control"), Some("no-store"));
    let r = http.query(Some(&key.key), &good(&foreign_public.key)).await;
    assert_eq!(
        r.status, 200,
        "公开站点的统计对任何有效 key 可读：{}",
        r.body
    );

    // ---- 400 -------------------------------------------------------------------------------
    let cases: Vec<(&str, String, &str, &str)> = vec![
        ("非法 JSON", "{\"site\": ".into(), "", "not valid JSON"),
        ("空体", String::new(), "", "not valid JSON"),
        (
            "未知字段",
            json!({"site": site.key, "metrics": ["visitors"], "date_range": "day", "bogus": 1}).to_string(),
            "bogus",
            "unknown field",
        ),
        (
            "未知指标",
            json!({"site": site.key, "metrics": ["nope"], "date_range": "day"}).to_string(),
            "metrics[0]",
            "",
        ),
        (
            "非法过滤：取值不是数组",
            json!({"site": site.key, "metrics": ["visitors"], "date_range": "day",
                   "filters": [["is", "event:page", "/"]]})
            .to_string(),
            "filters[0]",
            "",
        ),
        (
            "非法过滤：未知操作符",
            json!({"site": site.key, "metrics": ["visitors"], "date_range": "day",
                   "filters": [["is", "visit:device", ["desktop"]], ["bogus_op", "event:page", ["/"]]]})
            .to_string(),
            "filters[1]",
            "",
        ),
        (
            "校验错误（站点判定之后）：日期范围反了",
            json!({"site": site.key, "metrics": ["visitors"],
                   "date_range": ["2026-03-11", "2026-03-09"]})
            .to_string(),
            "date_range",
            "",
        ),
        (
            "校验错误（站点判定之后）：正则过长",
            json!({"site": site.key, "metrics": ["visitors"], "date_range": "day",
                   "filters": [["matches", "event:page", ["a".repeat(300)]]]})
            .to_string(),
            "filters",
            "",
        ),
        (
            "缺少 site",
            json!({"metrics": ["visitors"], "date_range": "day"}).to_string(),
            "site",
            "required",
        ),
    ];
    for (name, body, path_prefix, message) in &cases {
        let r = http.query_raw(Some(&key.key), body).await;
        assert_eq!(r.status, 400, "{name}：{}", r.body);
        assert_eq!(r.header("cache-control"), Some("no-store"), "{name}");
        assert!(r.header("x-ratelimit-remaining").is_some(), "{name}");
        let v = r.json();
        assert_eq!(v["error"], "invalid_query", "{name}");
        let errors = v["errors"]
            .as_array()
            .unwrap_or_else(|| panic!("{name}：{v}"));
        assert_eq!(errors.len(), 1, "{name}");
        let path = errors[0]["path"].as_str().unwrap();
        let msg = errors[0]["message"].as_str().unwrap();
        assert!(
            if path_prefix.is_empty() {
                path.is_empty()
            } else {
                path.starts_with(path_prefix)
            },
            "{name}：path={path:?} 应以 {path_prefix:?} 开头；{v}"
        );
        assert!(msg.contains(message), "{name}：{msg}");
        assert_eq!(v["message"], msg, "{name}");
        println!("400 {name}：path={path:?} message={msg:?}");
    }

    // ---- 401 -------------------------------------------------------------------------------
    let never_issued = rivu_auth::key::generate().plaintext().to_owned();
    let mut bodies_401 = Vec::new();
    for (name, k) in [
        ("无 key", None),
        ("错 key（格式对、未发放）", Some(never_issued.as_str())),
        ("格式错", Some("rivu_nope")),
    ] {
        let r = http.query(k, &good(&site.key)).await;
        assert_eq!(r.status, 401, "{name}");
        assert_eq!(r.header("www-authenticate"), Some("Bearer realm=\"rivu\""));
        assert_eq!(r.header("cache-control"), Some("no-store"));
        bodies_401.push(r.body);
    }
    assert!(
        bodies_401.windows(2).all(|w| w[0] == w[1]),
        "三种 401 的响应体相同"
    );
    // 非 Bearer 方案
    let r = http
        .request(
            "POST",
            query_api::QUERY_PATH,
            None,
            &[&format!("Authorization: Basic {}", key.key)],
            b"{}",
        )
        .await;
    assert_eq!(r.status, 401);
    // 无 key 时，坏请求体也是 401（先鉴权后解析）
    assert_eq!(http.query_raw(None, "not json").await.status, 401);

    // ---- 403 -------------------------------------------------------------------------------
    let r = http.query(Some(&sites_only.key), &good(&site.key)).await;
    assert_eq!(r.status, 403, "{}", r.body);
    assert_eq!(r.json()["error"], "forbidden");
    assert!(r.json()["message"].as_str().unwrap().contains("stats:read"));
    assert_eq!(r.header("cache-control"), Some("no-store"));
    // 403 先于请求体校验：缺 scope 的 key 发坏请求也是 403
    assert_eq!(
        http.query_raw(Some(&sites_only.key), "not json")
            .await
            .status,
        403
    );

    // ---- 404：完全相同的响应 -------------------------------------------------------------------
    let unknown = http.query(Some(&key.key), &good("zzzzzzzzzzzz")).await;
    let foreign_resp = http.query(Some(&key.key), &good(&foreign.key)).await;
    let out_of_scope = http.query(Some(&narrow.key), &good(&second.key)).await;
    let malformed = http.query(Some(&key.key), &good("../etc/passwd")).await;
    let overlong = http.query(Some(&key.key), &good(&"a".repeat(1000))).await;
    for (name, r) in [
        ("不存在", &unknown),
        ("别的组织", &foreign_resp),
        ("scope 只含另一个站点", &out_of_scope),
        ("格式非法", &malformed),
        ("超长", &overlong),
    ] {
        assert_eq!(r.status, 404, "{name}：{}", r.body);
        assert_eq!(r.body, unknown.body, "{name}：404 体与「不存在」完全相同");
        assert_eq!(r.header("cache-control"), Some("no-store"), "{name}");
        assert_eq!(
            r.header("content-type"),
            unknown.header("content-type"),
            "{name}"
        );
    }
    println!("404 体：{}", unknown.body);
    // 带站点限定 scope 的 key 查自己被授权的站点：200
    assert_eq!(
        http.query(Some(&narrow.key), &good(&site.key)).await.status,
        200
    );

    // ---- 其他方法、其余 /api/* ---------------------------------------------------------------
    let r = http
        .request("GET", query_api::QUERY_PATH, Some(&key.key), &[], b"")
        .await;
    assert_eq!(r.status, 405);
    assert_eq!(r.header("allow"), Some("POST"));
    let r = http
        .request("OPTIONS", query_api::QUERY_PATH, Some(&key.key), &[], b"")
        .await;
    assert_eq!(
        r.header("access-control-allow-origin"),
        None,
        "不开 CORS，预检不应答"
    );
    let r = http
        .request("GET", "/api/stats/overview", Some(&key.key), &[], b"")
        .await;
    assert_eq!(r.status, 501, "其余 /api/* 仍是 501");
    let r = http
        .request("POST", "/api/internal/sites/1/query", None, &[], b"{}")
        .await;
    assert_eq!(r.status, 501);

    // ---- schema.json ---------------------------------------------------------------------
    let r = http
        .request("GET", query_api::SCHEMA_PATH, None, &[], b"")
        .await;
    assert_eq!(r.status, 200, "无需鉴权");
    assert_eq!(r.header("content-type"), Some("application/json"));
    assert_eq!(r.header("cache-control"), Some("public, max-age=3600"));
    assert_eq!(r.header("x-ratelimit-limit"), None, "不消耗配额");
    let etag = r.header("etag").expect("ETag").to_owned();
    let schema = r.json();
    assert_eq!(schema, rivu_query::schema_json());
    assert!(schema["properties"]["metrics"].is_object(), "{schema}");
    assert!(
        schema["$defs"].is_object() && schema["$defs"]["Metric"].is_object(),
        "含 metrics 等定义"
    );
    let r = http
        .request(
            "GET",
            query_api::SCHEMA_PATH,
            None,
            &[&format!("If-None-Match: {etag}")],
            b"",
        )
        .await;
    assert_eq!(r.status, 304);
    assert_eq!(r.body, "");

    // ---- rivu_api_requests_total{route,status}：401 / 403 / 404 / 400 / 501 都被计数 ------------------
    let text = metrics.render();
    let count = |route: &str, status: u16| {
        sample(
            &text,
            &format!("rivu_api_requests_total{{route=\"{route}\",status=\"{status}\"}}"),
        )
    };
    assert_eq!(count("api.query", 200), Some(3.0), "{text}");
    assert_eq!(count("api.query", 400), Some(cases.len() as f64), "{text}");
    assert_eq!(count("api.query", 401), Some(5.0), "{text}");
    assert_eq!(count("api.query", 403), Some(2.0), "{text}");
    assert_eq!(count("api.query", 404), Some(5.0), "{text}");
    assert_eq!(count("api.query", 405), Some(2.0), "{text}");
    assert_eq!(count("api.query_schema", 200), Some(1.0), "{text}");
    assert_eq!(count("api.query_schema", 304), Some(1.0), "{text}");
    assert_eq!(count("api.not_implemented", 501), Some(2.0), "{text}");
    // 只有走到 execute 的请求计入耗时：3 次 200（含别的组织公开站点的那次）+ 校验错误发生在 build 阶段的 2 次
    assert_eq!(
        sample(&text, "rivu_query_seconds_count{route=\"api.query\"}"),
        Some(5.0),
        "{text}"
    );
    assert_eq!(
        sample(
            &text,
            "rivu_query_errors_total{route=\"api.query\",kind=\"validation\"}"
        ),
        Some(2.0),
        "{text}"
    );

    cleanup(&env, &[site.id, second.id, foreign.id, foreign_public.id]).await;
}

// ------------------------------------------------------------------ 用例 3

#[tokio::test]
async fn hourly_quota_exhaustion_is_429_with_headers() {
    let Some(env) = test_env("hourly_quota_exhaustion_is_429_with_headers") else {
        return;
    };
    migrate_once(&env).await;
    let pg = rivu_storage_pg::connect(&env.pg_url, 8)
        .await
        .expect("连接 PG");
    let org = make_org(&pg).await;
    sqlx::query("UPDATE organizations SET api_hourly_limit = 3 WHERE id = $1")
        .bind(org.id)
        .execute(&pg)
        .await
        .unwrap();
    let site = make_site(&pg, &org, "UTC", Weekday::Monday, false).await;
    let k1 = new_key(&pg, &org, &["stats:read"]).await;
    let k2 = new_key(&pg, &org, &["stats:read"]).await;
    let server = start_server(&env, &pg).await;
    let http = server.http;
    let spec = json!({"site": site.key, "metrics": ["visitors"], "date_range": ["2026-03-09", "2026-03-11"]});

    // 前三次放行（第三次是 400 也要计数：用错误请求试探同样消耗配额），剩余额度递减
    let r = http.query(Some(&k1.key), &spec).await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.header("x-ratelimit-limit"), Some("3"));
    assert_eq!(r.header("x-ratelimit-remaining"), Some("2"));
    let r = http.query(Some(&k2.key), &spec).await;
    assert_eq!(r.status, 200);
    assert_eq!(
        r.header("x-ratelimit-remaining"),
        Some("1"),
        "同组织两把 key 共享配额"
    );
    let r = http.query_raw(Some(&k1.key), "not json").await;
    assert_eq!(r.status, 400);
    assert_eq!(r.header("x-ratelimit-remaining"), Some("0"));

    // 超限：429 + Retry-After + X-RateLimit-*，体里有 retry_after
    let r = http.query(Some(&k1.key), &spec).await;
    assert_eq!(r.status, 429, "{}", r.body);
    assert_eq!(r.header("retry-after"), Some("3600"));
    assert_eq!(r.header("x-ratelimit-limit"), Some("3"));
    assert_eq!(r.header("x-ratelimit-remaining"), Some("0"));
    let reset: u64 = r
        .header("x-ratelimit-reset")
        .expect("X-RateLimit-Reset")
        .parse()
        .unwrap();
    assert!((1..=3600).contains(&reset), "{reset}");
    assert_eq!(r.header("cache-control"), Some("no-store"));
    let v = r.json();
    assert_eq!(v["error"], "rate_limited");
    assert_eq!(v["retry_after"], 3600);
    println!(
        "429：{} Retry-After={:?} X-RateLimit-Reset={reset}",
        r.body,
        r.header("retry-after")
    );
    // 另一个组织不受影响
    let other = make_org(&pg).await;
    let other_site = make_site(&pg, &other, "UTC", Weekday::Monday, false).await;
    let ok = new_key(&pg, &other, &["stats:read"]).await;
    let r = http
        .query(
            Some(&ok.key),
            &json!({"site": other_site.key, "metrics": ["visitors"], "date_range": "day"}),
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
    // schema.json 不计入配额，也不受限流影响
    let r = http
        .request("GET", query_api::SCHEMA_PATH, None, &[], b"")
        .await;
    assert_eq!(r.status, 200);

    cleanup(&env, &[site.id, other_site.id]).await;
}
