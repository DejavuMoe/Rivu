//! 多站点汇总视图（`GET /api/internal/orgs/{org_id}/consolidated`）的端到端测试：真实的 PostgreSQL 与 ClickHouse，
//! HTTP 走真实的本地 TCP 端口（与 `annotations.rs` 同一套装配）。
//!
//! 缺少环境变量时跳过并打印原因（同 `internal_api.rs`）：
//!   RIVU_TEST_DATABASE_URL     有 DDL 与写权限的 PostgreSQL 连接串（rivu_migrator）
//!   RIVU_TEST_CLICKHOUSE_URL   ClickHouse HTTP 地址
//! 可选：RIVU_TEST_CH_{MIGRATOR,WRITER,READER}_{USER,PASSWORD}、RIVU_TEST_CH_DATABASE。
//!
//! 用例：
//! - `sums_and_weighted_averages_across_time_zones`：两个时区不同的站点（UTC、Asia/Tokyo）写入已知的会话，核对合计
//!   （访客、访问、浏览量相加，跳出率与访问时长按访问加权）、按本地日的趋势、对比期、按月，以及与单站查询逐站一致；
//! - `only_readable_sites_are_included`：站点访客只看到自己的站点，已删除的站点不算，非成员、没有站点的成员、
//!   已删除的组织都是 404，没有会话 401，参数校验；
//! - `site_count_is_capped`：超过 100 个站点时只汇总前 100 个并标明。

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use clickhouse::Client;
use jiff::Timestamp;
use rivu::dash_api::{self, DashApi, DashLimits};
use rivu::{migrate, public};
use rivu_auth::password::hash_blocking;
use rivu_common::config::{ChAccount, Config, Env, Role, Secret};
use rivu_storage_ch::rows::{EventRow, insert_events};
use rivu_storage_pg::sites::NewSite;
use serde_json::{Value, json};
use sqlx::PgPool;
use time::OffsetDateTime;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::OnceCell;
use uuid::Uuid;

const PASSWORD: &str = "correct horse battery";
const HTTP_ORIGIN: &str = "http://127.0.0.1:8080";
const BROWSER_UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36";

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

#[derive(Clone)]
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

async fn pg(env: &TestEnv) -> PgPool {
    migrate_once(env).await;
    rivu_storage_pg::connect(&env.pg_url, 8)
        .await
        .expect("连接 PostgreSQL")
}

fn unique(prefix: &str) -> String {
    format!("{prefix}-{:08x}", rand::random::<u32>())
}

fn config(env: &TestEnv) -> Config {
    let reader = account("reader");
    let pairs: Vec<(String, String)> = vec![
        ("RIVU_DATABASE_URL".into(), env.pg_url.clone()),
        ("RIVU_CLICKHOUSE_URL".into(), env.ch_url.clone()),
        ("RIVU_CH_DATABASE".into(), ch_database()),
        ("RIVU_CH_READER_USER".into(), reader.user.clone()),
        (
            "RIVU_CH_READER_PASSWORD".into(),
            reader.password.expose().to_owned(),
        ),
        ("RIVU_TRUSTED_PROXIES".into(), "127.0.0.0/8".into()),
        ("RIVU_BASE_URL".into(), HTTP_ORIGIN.into()),
    ];
    Config::from_env(Role::Admin, &Env::from_pairs(pairs)).expect("测试配置")
}

fn loose() -> DashLimits {
    DashLimits {
        login_attempts: 100_000,
        login_ip_attempts: 100_000,
        login_window: Duration::from_secs(60),
        view_per_min: 1_000_000,
        view_burst: 100_000,
        password_per_min: 100_000,
        account_fail_alert: u32::MAX,
        global_fail_alert: u32::MAX,
    }
}

// ------------------------------------------------------------------ 真实 HTTP

struct Resp {
    status: u16,
    headers: Vec<(String, String)>,
    body: String,
}

impl Resp {
    fn json(&self) -> Value {
        serde_json::from_str(&self.body)
            .unwrap_or_else(|e| panic!("响应体不是 JSON（{e}）：{}", self.body))
    }

    fn cookie_pair(&self, name: &str) -> Option<String> {
        self.headers
            .iter()
            .filter(|(k, _)| k.eq_ignore_ascii_case("set-cookie"))
            .map(|(_, v)| v.as_str())
            .find(|c| c.starts_with(&format!("{name}=")))
            .map(|c| c.split(';').next().unwrap().to_owned())
    }
}

#[derive(Clone)]
struct Http {
    addr: SocketAddr,
    origin: String,
}

struct Rb<'a> {
    http: &'a Http,
    method: &'static str,
    path: String,
    body: Vec<u8>,
    headers: Vec<(String, String)>,
}

impl Rb<'_> {
    fn header(mut self, k: &str, v: &str) -> Self {
        self.headers.retain(|(hk, _)| !hk.eq_ignore_ascii_case(k));
        self.headers.push((k.to_owned(), v.to_owned()));
        self
    }

    fn cookie(self, c: &str) -> Self {
        self.header("Cookie", c)
    }

    async fn send(self) -> Resp {
        let mut stream = TcpStream::connect(self.http.addr).await.unwrap();
        let mut req = format!(
            "{} {} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nContent-Length: {}\r\n",
            self.method,
            self.path,
            self.body.len()
        );
        for (k, v) in &self.headers {
            req.push_str(&format!("{k}: {v}\r\n"));
        }
        req.push_str("\r\n");
        let mut bytes = req.into_bytes();
        bytes.extend_from_slice(&self.body);
        let _ = stream.write_all(&bytes).await;
        let mut raw = Vec::new();
        let mut chunk = [0u8; 8192];
        loop {
            match stream.read(&mut chunk).await {
                Ok(0) | Err(_) => break,
                Ok(n) => raw.extend_from_slice(&chunk[..n]),
            }
        }
        assert!(
            !raw.is_empty(),
            "{} {}：没有收到任何响应",
            self.method,
            self.path
        );
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
}

impl Http {
    fn rb(&self, method: &'static str, path: &str, body: Vec<u8>) -> Rb<'_> {
        Rb {
            http: self,
            method,
            path: path.to_owned(),
            body,
            headers: vec![
                ("User-Agent".into(), BROWSER_UA.into()),
                ("X-Forwarded-For".into(), "203.0.113.1".into()),
            ],
        }
    }

    fn get(&self, path: &str) -> Rb<'_> {
        self.rb("GET", path, Vec::new())
    }

    fn json_rb(&self, method: &'static str, path: &str, body: &Value) -> Rb<'_> {
        self.rb(method, path, body.to_string().into_bytes())
            .header("Content-Type", "application/json")
            .header("Origin", &self.origin)
    }

    fn post(&self, path: &str, body: &Value) -> Rb<'_> {
        self.json_rb("POST", path, body)
    }
}

async fn start(env: &TestEnv, pg: &PgPool) -> Http {
    let cfg = config(env);
    let reader: Client =
        rivu_storage_ch::client(&env.ch_url, &account("reader"), Some(&ch_database()));
    let now: Timestamp = "2026-11-20T12:00:00Z".parse().unwrap();
    let dash = DashApi::new(&cfg, pg.clone(), reader, None)
        .expect("装配内部端点")
        .with_limits(loose())
        .with_clock(move || now);
    let app = public::router_full(None, None, Some(dash_api::router(Arc::new(dash))));
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
    Http {
        addr,
        origin: cfg.base_url.clone(),
    }
}

// ------------------------------------------------------------------ 组织、用户、站点

async fn make_org(pg: &PgPool) -> i64 {
    let slug = unique("org");
    sqlx::query_scalar("INSERT INTO organizations (name, slug) VALUES ($1, $1) RETURNING id")
        .bind(&slug)
        .fetch_one(pg)
        .await
        .unwrap()
}

fn password_hash() -> &'static str {
    static H: OnceLock<String> = OnceLock::new();
    H.get_or_init(|| hash_blocking(PASSWORD).unwrap())
}

struct Actor {
    id: i64,
    cookie: String,
}

/// 建一个账号并登录；`org_role` 给出时同时成为该组织的成员。
async fn user(pg: &PgPool, http: &Http, org_role: Option<(i64, &str)>) -> Actor {
    let email = format!("{}@example.test", unique("user"));
    let id = rivu_storage_pg::users::create_user(
        pg,
        &rivu_storage_pg::users::NewUser {
            email: &email,
            name: Some("Test User"),
            password_hash: password_hash(),
            is_instance_admin: false,
        },
    )
    .await
    .unwrap();
    if let Some((org, role)) = org_role {
        rivu_storage_pg::users::upsert_member(pg, org, id, role)
            .await
            .unwrap();
    }
    let r = http
        .post(
            "/api/internal/login",
            &json!({"email": email, "password": PASSWORD}),
        )
        .send()
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
    Actor {
        id,
        cookie: r.cookie_pair("rivu_session").expect("会话 cookie"),
    }
}

async fn guest(pg: &PgPool, http: &Http, site: u64, role: &str) -> Actor {
    let a = user(pg, http, None).await;
    sqlx::query("INSERT INTO site_memberships (site_id, user_id, role) VALUES ($1, $2, $3)")
        .bind(site as i64)
        .bind(a.id)
        .bind(role)
        .execute(pg)
        .await
        .unwrap();
    a
}

async fn make_site(pg: &PgPool, org_id: i64, timezone: &str) -> u64 {
    let hostnames = vec![format!("{}.example.test", unique("site"))];
    let name = hostnames[0].clone();
    rivu_storage_pg::sites::create_site_in_org(
        pg,
        org_id,
        &NewSite {
            name: &name,
            timezone,
            hostnames: &hostnames,
        },
    )
    .await
    .expect("建站")
    .id
}

fn expect(r: &Resp, status: u16) -> Value {
    assert_eq!(r.status, status, "响应：{}", r.body);
    if r.body.trim().is_empty() {
        Value::Null
    } else {
        r.json()
    }
}

fn expect_err(r: &Resp, status: u16, code: &str) {
    let v = expect(r, status);
    assert_eq!(v["error"], code, "响应：{}", r.body);
}

fn path(org: i64, query: &str) -> String {
    format!("/api/internal/orgs/{org}/consolidated{query}")
}

/// 一个会话：`(开始时刻 UTC, 每次浏览距开始的秒数)`。
type Visit = (&'static str, &'static [i64]);

fn visit_rows(site: u64, session_id: u64, (start, offsets): Visit) -> Vec<EventRow> {
    let secs = start.parse::<Timestamp>().expect("时刻").as_second();
    let base = OffsetDateTime::from_unix_timestamp(secs).expect("时刻");
    offsets
        .iter()
        .enumerate()
        .map(|(i, off)| EventRow {
            site_id: site,
            ts: base + time::Duration::seconds(*off),
            event_id: Uuid::now_v7(),
            name: "pageview".into(),
            visitor_id: session_id ^ 0x5eed,
            session_id,
            session_start: base,
            ingest_partition: 0,
            hostname: "example.test".into(),
            pathname: format!("/p{i}"),
            entry_pathname: "/p0".into(),
            referrer: String::new(),
            referrer_source: String::new(),
            channel: String::new(),
            utm_source: String::new(),
            utm_medium: String::new(),
            utm_campaign: String::new(),
            utm_content: String::new(),
            utm_term: String::new(),
            click_id_param: String::new(),
            country_code: *b"DE",
            region_code: String::new(),
            city_geoname_id: 0,
            device: "desktop".into(),
            browser: "Firefox".into(),
            browser_version: "130.0".into(),
            os: "Linux".into(),
            os_version: String::new(),
            interactive: true,
            props: HashMap::new(),
            scroll_depth: 255,
            engagement_ms: 0,
            revenue_amount: None,
            revenue_currency: String::new(),
        })
        .collect()
}

async fn seed(env: &TestEnv, site: u64, visits: &[Visit]) {
    let writer = rivu_storage_ch::client(&env.ch_url, &account("writer"), Some(&ch_database()));
    let dedup = rivu_storage_ch::rows::supports_deduplicate_insert(&writer)
        .await
        .expect("查询 deduplicate_insert");
    let rows: Vec<EventRow> = visits
        .iter()
        .enumerate()
        .flat_map(|(i, v)| visit_rows(site, site * 1000 + i as u64 + 1, *v))
        .collect();
    insert_events(&writer, &rows, &format!("consolidated-it:{site}"), dedup)
        .await
        .expect("写入 events");
}

fn close(a: &Value, b: f64) -> bool {
    (a.as_f64().unwrap() - b).abs() < 1e-9
}

fn assert_metrics(v: &Value, visitors: u64, visits: u64, pageviews: u64, bounce: f64, dur: f64) {
    assert_eq!(v["visitors"], visitors, "{v}");
    assert_eq!(v["visits"], visits, "{v}");
    assert_eq!(v["pageviews"], pageviews, "{v}");
    assert!(close(&v["bounce_rate"], bounce), "bounce_rate {v}");
    assert!(close(&v["visit_duration"], dur), "visit_duration {v}");
}

fn site_row(v: &Value, id: u64) -> Value {
    v["sites"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["id"] == id)
        .cloned()
        .unwrap_or_else(|| panic!("站点 {id} 不在结果里：{v}"))
}

fn site_ids(v: &Value) -> Vec<u64> {
    let mut ids: Vec<u64> = v["sites"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["id"].as_u64().unwrap())
        .collect();
    ids.sort_unstable();
    ids
}

fn sorted(mut ids: Vec<u64>) -> Vec<u64> {
    ids.sort_unstable();
    ids
}

// ------------------------------------------------------------------ 用例

#[tokio::test]
async fn sums_and_weighted_averages_across_time_zones() {
    let Some(env) = test_env("sums_and_weighted_averages_across_time_zones") else {
        return;
    };
    let pg = pg(&env).await;
    let http = start(&env, &pg).await;
    let org = make_org(&pg).await;
    let owner = user(&pg, &http, Some((org, "owner"))).await;
    let a = make_site(&pg, org, "UTC").await;
    let b = make_site(&pg, org, "Asia/Tokyo").await;
    // A（UTC）：11-10 两个会话（3 次浏览 120 秒；1 次浏览，跳出），11-11 一个（2 次浏览 30 秒）；
    // 对比期（10-17 到 10-31）里一个跳出的会话
    seed(
        &env,
        a,
        &[
            ("2026-11-10T10:00:00Z", &[0, 60, 120]),
            ("2026-11-10T12:00:00Z", &[0]),
            ("2026-11-11T09:00:00Z", &[0, 30]),
            ("2026-10-20T09:00:00Z", &[0]),
        ],
    )
    .await;
    // B（UTC+9）：11-10T20:00Z 是东京的 11-11 05:00（跳出）；11-10T01:00Z 是东京的 11-10 10:00（2 次浏览 60 秒）
    seed(
        &env,
        b,
        &[
            ("2026-11-10T20:00:00Z", &[0]),
            ("2026-11-10T01:00:00Z", &[0, 60]),
        ],
    )
    .await;

    let v = expect(
        &http
            .get(&path(
                org,
                "?from=2026-11-01&to=2026-11-15&compare=previous_period",
            ))
            .cookie(&owner.cookie)
            .send()
            .await,
        200,
    );
    assert_eq!(v["organization"]["id"], org);
    assert_eq!(v["date_range"], json!(["2026-11-01", "2026-11-15"]));
    assert_eq!(v["interval"], "day");
    assert_eq!(v["compare"], "previous_period");
    assert_eq!(v["meta"]["visitors"], "sum_of_sites");
    assert!(v["meta"]["note"].as_str().unwrap().contains("sum"));
    assert_eq!(v["meta"]["sites_total"], 2);
    assert_eq!(v["meta"]["truncated"], false);
    assert_eq!(site_ids(&v), sorted(vec![a, b]));

    // 每个站点自己的数字
    let ra = site_row(&v, a);
    assert_eq!(ra["timezone"], "UTC");
    assert_metrics(&ra["totals"], 3, 3, 6, 1.0 / 3.0, 50.0);
    assert_metrics(&ra["comparison_totals"], 1, 1, 1, 1.0, 0.0);
    let rb = site_row(&v, b);
    assert_eq!(rb["timezone"], "Asia/Tokyo");
    assert!(rb["range"]["start"].as_str().unwrap().contains("+09:00"));
    assert_metrics(&rb["totals"], 2, 2, 3, 0.5, 30.0);
    // 合计：相加与按访问加权
    assert_metrics(&v["totals"], 5, 5, 9, 0.4, 42.0);
    assert_metrics(&v["comparison_totals"], 1, 1, 1, 1.0, 0.0);

    // 趋势：各站点的本地日，按日期标签相加；没有数据的天是 0
    let trend = v["trend"].as_array().unwrap();
    assert_eq!(trend.len(), 15);
    assert_eq!(trend[0]["date"], "2026-11-01");
    assert_eq!(trend[14]["date"], "2026-11-15");
    let day = |d: &str| trend.iter().find(|t| t["date"] == d).unwrap().clone();
    assert_metrics(&day("2026-11-10"), 3, 3, 6, 1.0 / 3.0, 60.0);
    assert_metrics(&day("2026-11-11"), 2, 2, 3, 0.5, 15.0);
    assert_metrics(&day("2026-11-09"), 0, 0, 0, 0.0, 0.0);

    // 与看板单站查询逐站一致（同一个查询内核）
    let keys = [
        "visitors",
        "visits",
        "pageviews",
        "bounce_rate",
        "visit_duration",
    ];
    for site in [a, b] {
        let r = expect(
            &http
                .post(
                    &format!("/api/internal/sites/{site}/query"),
                    &json!({"metrics": keys, "date_range": ["2026-11-01", "2026-11-15"]}),
                )
                .cookie(&owner.cookie)
                .send()
                .await,
            200,
        );
        let m = &r["results"][0]["metrics"];
        let row = site_row(&v, site);
        for (i, k) in keys.iter().enumerate() {
            assert!(
                close(&row["totals"][*k], m[i].as_f64().unwrap()),
                "{site} {k}: {row} vs {m}"
            );
        }
    }

    // 按月：10 月只有 A 的一个会话
    let v = expect(
        &http
            .get(&path(org, "?from=2026-10-01&to=2026-11-15&interval=month"))
            .cookie(&owner.cookie)
            .send()
            .await,
        200,
    );
    let trend = v["trend"].as_array().unwrap();
    assert_eq!(
        trend.iter().map(|t| t["date"].clone()).collect::<Vec<_>>(),
        [json!("2026-10-01"), json!("2026-11-01")]
    );
    assert_metrics(&trend[0], 1, 1, 1, 1.0, 0.0);
    assert_metrics(&trend[1], 5, 5, 9, 0.4, 42.0);
    assert!(v.get("comparison_totals").is_none());

    // 关键词范围也能用（测试时钟是 2026-11-20）
    let v = expect(
        &http
            .get(&path(org, "?range=30d"))
            .cookie(&owner.cookie)
            .send()
            .await,
        200,
    );
    assert_eq!(v["date_range"], "30d");
    assert_metrics(&v["totals"], 5, 5, 9, 0.4, 42.0);
}

#[tokio::test]
async fn only_readable_sites_are_included() {
    let Some(env) = test_env("only_readable_sites_are_included") else {
        return;
    };
    let pg = pg(&env).await;
    let http = start(&env, &pg).await;
    let org = make_org(&pg).await;
    let empty_org = make_org(&pg).await;
    let other_org = make_org(&pg).await;
    let a = make_site(&pg, org, "UTC").await;
    let b = make_site(&pg, org, "Europe/Berlin").await;
    let gone = make_site(&pg, org, "UTC").await;
    let elsewhere = make_site(&pg, other_org, "UTC").await;
    sqlx::query("UPDATE sites SET deleted_at = now() WHERE id = $1")
        .bind(gone as i64)
        .execute(&pg)
        .await
        .unwrap();
    seed(&env, a, &[("2026-11-10T10:00:00Z", &[0, 60])]).await;
    seed(&env, b, &[("2026-11-10T10:00:00Z", &[0])]).await;
    seed(&env, gone, &[("2026-11-10T10:00:00Z", &[0, 1, 2, 3])]).await;
    let viewer = user(&pg, &http, Some((org, "viewer"))).await;
    let guest_a = guest(&pg, &http, a, "viewer").await;
    let empty_member = user(&pg, &http, Some((empty_org, "owner"))).await;
    let outsider = user(&pg, &http, Some((other_org, "owner"))).await;
    let q = "?from=2026-11-01&to=2026-11-15";

    // 组织成员：全部未删除的站点，不含已删除的
    let v = expect(
        &http.get(&path(org, q)).cookie(&viewer.cookie).send().await,
        200,
    );
    assert_eq!(site_ids(&v), sorted(vec![a, b]));
    assert_eq!(v["totals"]["pageviews"], 3);

    // 站点访客：只有自己被授权的站点
    let v = expect(
        &http.get(&path(org, q)).cookie(&guest_a.cookie).send().await,
        200,
    );
    assert_eq!(site_ids(&v), [a]);
    assert_eq!(v["totals"]["pageviews"], 2);
    assert_eq!(v["meta"]["sites_total"], 1);

    // 404：别的组织（访客、非成员）、没有站点的组织、不存在的组织、不是数字的 id
    for (who, org_seg) in [
        (&guest_a, other_org.to_string()),
        (&outsider, org.to_string()),
        (&empty_member, empty_org.to_string()),
        (&viewer, "999999999".to_owned()),
        (&viewer, "abc".to_owned()),
    ] {
        expect_err(
            &http
                .get(&format!("/api/internal/orgs/{org_seg}/consolidated"))
                .cookie(&who.cookie)
                .send()
                .await,
            404,
            "not_found",
        );
    }
    // 别的组织的成员能看自己的组织
    let v = expect(
        &http
            .get(&path(other_org, q))
            .cookie(&outsider.cookie)
            .send()
            .await,
        200,
    );
    assert_eq!(site_ids(&v), [elsewhere]);

    // 没有会话 401
    expect_err(&http.get(&path(org, q)).send().await, 401, "unauthorized");

    // 参数校验
    for bad in [
        "?range=realtime",
        "?range=7d&from=2026-11-01&to=2026-11-02",
        "?from=2026-11-01",
        "?interval=week",
        "?compare=yes",
    ] {
        expect_err(
            &http
                .get(&path(org, bad))
                .cookie(&viewer.cookie)
                .send()
                .await,
            400,
            "invalid_request",
        );
    }
    // 查询内核拒绝的取值也是 400（错误体沿用单站查询）
    for bad in ["?range=yesterday", "?from=2026-11-31&to=2026-12-01"] {
        let r = http
            .get(&path(org, bad))
            .cookie(&viewer.cookie)
            .send()
            .await;
        assert_eq!(r.status, 400, "{bad}: {}", r.body);
    }
    // 只允许 GET
    let r = http
        .post(&path(org, ""), &json!({}))
        .cookie(&viewer.cookie)
        .send()
        .await;
    assert_eq!(r.status, 405);

    // 组织删除后 404
    sqlx::query("UPDATE organizations SET deleted_at = now() WHERE id = $1")
        .bind(org)
        .execute(&pg)
        .await
        .unwrap();
    expect_err(
        &http.get(&path(org, q)).cookie(&viewer.cookie).send().await,
        404,
        "not_found",
    );
}

#[tokio::test]
async fn site_count_is_capped() {
    let Some(env) = test_env("site_count_is_capped") else {
        return;
    };
    let pg = pg(&env).await;
    let http = start(&env, &pg).await;
    let org = make_org(&pg).await;
    let owner = user(&pg, &http, Some((org, "owner"))).await;
    for _ in 0..101 {
        make_site(&pg, org, "UTC").await;
    }
    let v = expect(
        &http
            .get(&path(org, "?range=7d"))
            .cookie(&owner.cookie)
            .send()
            .await,
        200,
    );
    assert_eq!(v["sites"].as_array().unwrap().len(), 100);
    assert_eq!(v["meta"]["sites_total"], 101);
    assert_eq!(v["meta"]["sites_included"], 100);
    assert_eq!(v["meta"]["max_sites"], 100);
    assert_eq!(v["meta"]["truncated"], true);
    assert_eq!(v["trend"].as_array().unwrap().len(), 7);
}
