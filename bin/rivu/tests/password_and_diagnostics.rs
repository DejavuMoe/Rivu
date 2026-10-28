//! 修改密码（`POST /api/internal/me/password`）与接入诊断（`GET /api/internal/sites/{site}/diagnostics`）的端到端测试：
//! 真实的 PostgreSQL 与 ClickHouse，真实的本地 TCP 端口，路由由 `public::router_full` 装配（与 `org_admin.rs` 同一条路径）。
//!
//! 缺少环境变量时跳过并打印原因：
//!   RIVU_TEST_DATABASE_URL     有 DDL 与写权限的 PostgreSQL 连接串（rivu_migrator）
//!   RIVU_TEST_CLICKHOUSE_URL   ClickHouse HTTP 地址
//!
//! 用例：
//! - `password_*`：会话与 CSRF、当前密码不对、新密码太短或超长、与当前相同、成功后其他会话全部失效且签发新会话、改后新旧密码登录、
//!   `(用户, IP)` 限流；
//! - `diagnostics_*`：直接写 `ingest_counters` 后按 1h / 24h / 7d 聚合（桶对齐、已接收与已丢弃、丢弃原因、脚本版本、最后事件）、
//!   站点之间互不可见、`dropped_unknown_site` 不计入、范围外的行不计入、安静站点的最后事件回溯、权限矩阵。

use std::net::SocketAddr;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use clickhouse::Client;
use jiff::Timestamp;
use rivu::dash_api::{self, DashApi, DashLimits};
use rivu::{migrate, public};
use rivu_auth::password::hash_blocking;
use rivu_common::config::{ChAccount, Config, Env, Role, Secret};
use rivu_storage_ch::rows::IngestCounterRow;
use rivu_storage_pg::sites::NewSite;
use serde_json::{Value, json};
use sqlx::PgPool;
use time::OffsetDateTime;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::OnceCell;

const PASSWORD: &str = "correct horse battery";
const NEW_PASSWORD: &str = "another long passphrase";
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

/// 放得很宽的限流：不是在测限流的用例用它。
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

    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    /// 可以原样放进请求 `Cookie` 头的 `name=value`。
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

    fn remove(mut self, k: &str) -> Self {
        self.headers.retain(|(hk, _)| !hk.eq_ignore_ascii_case(k));
        self
    }

    fn cookie(self, c: &str) -> Self {
        self.header("Cookie", c)
    }

    fn ip(self, ip: &str) -> Self {
        self.header("X-Forwarded-For", ip)
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

    /// 带 JSON 的 `Content-Type` 与同源的 `Origin`（CSRF 判定要的）。
    fn json_rb(&self, method: &'static str, path: &str, body: Vec<u8>) -> Rb<'_> {
        self.rb(method, path, body)
            .header("Content-Type", "application/json")
            .header("Origin", &self.origin)
    }

    fn post(&self, path: &str, body: &Value) -> Rb<'_> {
        self.json_rb("POST", path, body.to_string().into_bytes())
    }
}

/// `now` 固定的服务（诊断的时间桶按它对齐）。
async fn start(env: &TestEnv, pg: &PgPool, limits: DashLimits, now: Timestamp) -> (Http, Config) {
    let cfg = config(env);
    let reader: Client =
        rivu_storage_ch::client(&env.ch_url, &account("reader"), Some(&ch_database()));
    let dash = DashApi::new(&cfg, pg.clone(), reader, None)
        .expect("装配内部端点")
        .with_limits(limits)
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
    (
        Http {
            addr,
            origin: cfg.base_url.clone(),
        },
        cfg,
    )
}

// ------------------------------------------------------------------ 组织、用户、站点

struct Org {
    id: i64,
}

async fn make_org(pg: &PgPool) -> Org {
    let slug = unique("org");
    let id: i64 =
        sqlx::query_scalar("INSERT INTO organizations (name, slug) VALUES ($1, $1) RETURNING id")
            .bind(&slug)
            .fetch_one(pg)
            .await
            .unwrap();
    Org { id }
}

fn password_hash() -> &'static str {
    static H: OnceLock<String> = OnceLock::new();
    H.get_or_init(|| hash_blocking(PASSWORD).unwrap())
}

async fn make_user(pg: &PgPool, member_of: Option<(&Org, &str)>) -> (i64, String) {
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
    if let Some((org, role)) = member_of {
        rivu_storage_pg::users::upsert_member(pg, org.id, id, role)
            .await
            .unwrap();
    }
    (id, email)
}

async fn login(http: &Http, email: &str, password: &str) -> Resp {
    http.post(
        "/api/internal/login",
        &json!({"email": email, "password": password}),
    )
    .send()
    .await
}

async fn login_cookie(http: &Http, email: &str) -> String {
    let r = login(http, email, PASSWORD).await;
    assert_eq!(r.status, 200, "{}", r.body);
    r.cookie_pair("rivu_session").expect("会话 cookie")
}

struct Site {
    id: u64,
}

async fn make_site(pg: &PgPool, org: &Org, name: &str) -> Site {
    let hostnames = vec!["example.test".to_owned()];
    let created = rivu_storage_pg::sites::create_site_in_org(
        pg,
        org.id,
        &NewSite {
            name,
            timezone: "UTC",
            hostnames: &hostnames,
        },
    )
    .await
    .expect("建站");
    Site { id: created.id }
}

fn expect(r: &Resp, status: u16) -> Value {
    assert_eq!(r.status, status, "响应：{}", r.body);
    if r.body.trim().is_empty() {
        Value::Null
    } else {
        r.json()
    }
}

/// 断言错误响应：状态码、`error` 码。
fn expect_err(r: &Resp, status: u16, code: &str) {
    let v = expect(r, status);
    assert_eq!(v["error"], code, "响应：{}", r.body);
}

// ------------------------------------------------------------------ 修改密码

const PW: &str = "/api/internal/me/password";

fn change(http: &Http, cookie: &str, current: &str, new: &str) -> impl Future<Output = Resp> {
    http.post(PW, &json!({"current": current, "new": new}))
        .cookie(cookie)
        .send()
}

async fn me_status(http: &Http, cookie: &str) -> u16 {
    http.get("/api/internal/me")
        .cookie(cookie)
        .send()
        .await
        .status
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn password_change_validates_and_revokes_other_sessions() {
    let Some(env) = test_env("password_change") else {
        return;
    };
    migrate_once(&env).await;
    let pg = rivu_storage_pg::connect(&env.pg_url, 6).await.unwrap();
    let (http, _) = start(&env, &pg, loose(), Timestamp::now()).await;
    let (_, email) = make_user(&pg, None).await;
    let a = login_cookie(&http, &email).await;
    let b = login_cookie(&http, &email).await;

    // 没有会话 401；缺 Origin 或不是 JSON 的请求被 CSRF 拒绝
    let r = http
        .post(PW, &json!({"current": PASSWORD, "new": NEW_PASSWORD}))
        .send()
        .await;
    expect_err(&r, 401, "unauthorized");
    let r = http
        .post(PW, &json!({"current": PASSWORD, "new": NEW_PASSWORD}))
        .cookie(&a)
        .remove("Origin")
        .send()
        .await;
    expect_err(&r, 403, "csrf_rejected");
    let r = http
        .post(PW, &json!({"current": PASSWORD, "new": NEW_PASSWORD}))
        .cookie(&a)
        .header("Content-Type", "text/plain")
        .send()
        .await;
    expect_err(&r, 403, "csrf_rejected");
    // 只允许 POST
    let r = http.get(PW).cookie(&a).send().await;
    assert_eq!(r.status, 405);

    // 请求体不对
    let r = http
        .post(PW, &json!({"current": PASSWORD}))
        .cookie(&a)
        .send()
        .await;
    expect_err(&r, 400, "invalid_request");

    // 当前密码不对（新密码合法）
    let r = change(&http, &a, "not the password!", NEW_PASSWORD).await;
    expect_err(&r, 400, "wrong_password");
    // 超长的当前密码同样判错
    let r = change(&http, &a, &"x".repeat(200), NEW_PASSWORD).await;
    expect_err(&r, 400, "wrong_password");
    // 新密码太短、太长
    let r = change(&http, &a, PASSWORD, "short").await;
    expect_err(&r, 400, "invalid_new_password");
    let r = change(&http, &a, PASSWORD, &"y".repeat(129)).await;
    expect_err(&r, 400, "invalid_new_password");
    // 与当前密码相同
    let r = change(&http, &a, PASSWORD, PASSWORD).await;
    expect_err(&r, 400, "same_password");

    // 失败的尝试什么都没改：两个会话都还在，旧密码仍能登录
    assert_eq!(me_status(&http, &a).await, 200);
    assert_eq!(me_status(&http, &b).await, 200);
    assert_eq!(login(&http, &email, PASSWORD).await.status, 200);

    // 成功：200 `{}`，签发新会话，旧会话全部失效
    let r = change(&http, &a, PASSWORD, NEW_PASSWORD).await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.json(), json!({}));
    assert!(
        r.header("cache-control")
            .is_some_and(|v| v.contains("no-store"))
    );
    let fresh = r.cookie_pair("rivu_session").expect("新会话的 cookie");
    assert_ne!(fresh, a);
    assert_eq!(me_status(&http, &a).await, 401, "发起请求的旧会话也已撤销");
    assert_eq!(me_status(&http, &b).await, 401, "其他会话已撤销");
    assert_eq!(me_status(&http, &fresh).await, 200, "新会话可用");
    let sessions = expect(
        &http
            .get("/api/internal/sessions")
            .cookie(&fresh)
            .send()
            .await,
        200,
    );
    let list = sessions["sessions"].as_array().unwrap();
    assert_eq!(list.len(), 1, "只剩新会话：{sessions}");
    assert_eq!(list[0]["current"], true);

    // 旧密码不能再登录，新密码可以
    expect_err(
        &login(&http, &email, PASSWORD).await,
        401,
        "invalid_credentials",
    );
    assert_eq!(login(&http, &email, NEW_PASSWORD).await.status, 200);
    // 新会话可以接着再改（当前密码已是新的）
    let r = change(&http, &fresh, NEW_PASSWORD, PASSWORD).await;
    assert_eq!(r.status, 200, "{}", r.body);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn password_change_is_rate_limited_per_user_and_ip() {
    let Some(env) = test_env("password_rate") else {
        return;
    };
    migrate_once(&env).await;
    let pg = rivu_storage_pg::connect(&env.pg_url, 6).await.unwrap();
    let limits = DashLimits {
        login_attempts: 3,
        ..loose()
    };
    let (http, _) = start(&env, &pg, limits, Timestamp::now()).await;
    let (_, email) = make_user(&pg, None).await;
    let (_, other_email) = make_user(&pg, None).await;
    let cookie = login_cookie(&http, &email).await;
    let other = login_cookie(&http, &other_email).await;

    for _ in 0..3 {
        let r = http
            .post(
                PW,
                &json!({"current": "wrong password!!", "new": NEW_PASSWORD}),
            )
            .cookie(&cookie)
            .send()
            .await;
        expect_err(&r, 400, "wrong_password");
    }
    // 第 4 次：429，带 Retry-After；即使这次的当前密码是对的
    let r = change(&http, &cookie, PASSWORD, NEW_PASSWORD).await;
    expect_err(&r, 429, "rate_limited");
    assert!(r.header("retry-after").is_some());
    assert_eq!(
        me_status(&http, &cookie).await,
        200,
        "被限流的请求什么都没改"
    );
    // 别的 IP 是另一个桶
    let r = http
        .post(
            PW,
            &json!({"current": "wrong password!!", "new": NEW_PASSWORD}),
        )
        .cookie(&cookie)
        .ip("198.51.100.9")
        .send()
        .await;
    expect_err(&r, 400, "wrong_password");
    // 别的用户不受影响
    let r = change(&http, &other, PASSWORD, NEW_PASSWORD).await;
    assert_eq!(r.status, 200, "{}", r.body);
}

// ------------------------------------------------------------------ 接入诊断

fn diag_path(site: impl std::fmt::Display, range: Option<&str>) -> String {
    match range {
        Some(r) => format!("/api/internal/sites/{site}/diagnostics?range={r}"),
        None => format!("/api/internal/sites/{site}/diagnostics"),
    }
}

fn counter(site: &Site, minute: i64, metric: &str, sv: u16, value: u64) -> IngestCounterRow {
    IngestCounterRow {
        site_id: site.id,
        minute: OffsetDateTime::from_unix_timestamp(minute).unwrap(),
        metric: metric.into(),
        script_version: sv,
        value,
    }
}

async fn put_counters(env: &TestEnv, rows: &[IngestCounterRow]) {
    let client = rivu_storage_ch::writer_client(&env.ch_url, &account("writer"), &ch_database());
    rivu_storage_ch::rows::insert_ingest_counters(&client, rows, &unique("diag-test"), false)
        .await
        .expect("写入 ingest_counters");
}

async fn cleanup(env: &TestEnv, site_ids: &[u64]) {
    let admin = rivu_storage_ch::client(&env.ch_url, &account("migrator"), Some(&ch_database()));
    for site_id in site_ids {
        let _ = admin
            .query("ALTER TABLE ingest_counters DELETE WHERE site_id = {site:UInt64}")
            .param("site", *site_id)
            .execute()
            .await;
    }
}

fn iso(secs: i64) -> String {
    Timestamp::from_second(secs).unwrap().to_string()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn diagnostics_aggregate_counters_per_site() {
    let Some(env) = test_env("diagnostics_aggregate") else {
        return;
    };
    migrate_once(&env).await;
    let pg = rivu_storage_pg::connect(&env.pg_url, 6).await.unwrap();
    // 「现在」：最近一个 6 小时边界之后的 3 小时 30 分 30 秒（在 90 天的 TTL 之内；各档的桶边界都在预期位置）
    let real = Timestamp::now().as_second();
    let t0 = real.div_euclid(21_600) * 21_600;
    let now_secs = t0 + 3 * 3600 + 30 * 60 + 30;
    let now = Timestamp::from_second(now_secs).unwrap();
    let minute = t0 + 3 * 3600 + 30 * 60; // 「现在」所在的分钟
    let (http, _) = start(&env, &pg, loose(), now).await;

    let org = make_org(&pg).await;
    let a = make_site(&pg, &org, "diag-a").await;
    let b = make_site(&pg, &org, "diag-b").await;
    let quiet = make_site(&pg, &org, "diag-quiet").await;
    let empty = make_site(&pg, &org, "diag-empty").await;

    let mut rows = vec![
        // 最后一个分钟桶：已接收 10、机器人 2
        counter(&a, minute, "accepted", 5, 10),
        counter(&a, minute, "dropped_bot", 5, 2),
        // 前一分钟：已接收 4（版本 5）、主机名不在白名单 3（版本 4）
        counter(&a, minute - 60, "accepted", 5, 4),
        counter(&a, minute - 60, "dropped_hostname_not_allowed", 4, 3),
        // 上一小时（90 分钟前）：已接收 6（版本 4）
        counter(&a, minute - 90 * 60, "accepted", 4, 6),
        // 30 小时前：只在 7d 里
        counter(&a, minute - 30 * 3600, "accepted", 4, 7),
        // 不属于任何站点的丢弃（正常记在 site_id = 0；这里故意写到站点上，验证被排除）
        counter(&a, minute, "dropped_unknown_site", 5, 100),
        // 别的站点：不应出现在 a 的结果里
        counter(&b, minute, "accepted", 9, 1000),
        counter(&b, minute, "dropped_bot", 9, 500),
        // 安静的站点：3 天前才有过事件
        counter(&quiet, minute - 3 * 86_400, "accepted", 2, 5),
    ];
    // 版本 5 的最后出现：最新一分钟；版本 4 的最后出现：前一分钟
    rows.sort_by_key(|r| r.minute);
    put_counters(&env, &rows).await;

    let (_, owner_email) = make_user(&pg, Some((&org, "owner"))).await;
    let owner = login_cookie(&http, &owner_email).await;
    let get = |site: &Site, range: Option<&str>| {
        let path = diag_path(site.id, range);
        let owner = owner.clone();
        let http = http.clone();
        async move { expect(&http.get(&path).cookie(&owner).send().await, 200) }
    };

    // ---- 1h：60 个 1 分钟桶
    let v = get(&a, Some("1h")).await;
    assert_eq!(v["range"], "1h");
    assert_eq!(v["step_seconds"], 60);
    let buckets = v["buckets"].as_array().unwrap();
    assert_eq!(buckets.len(), 60);
    assert_eq!(buckets[59]["t"], iso(minute));
    assert_eq!(buckets[59]["accepted"], 10);
    assert_eq!(buckets[59]["dropped"], 2, "unknown_site 不计入");
    assert_eq!(buckets[58]["t"], iso(minute - 60));
    assert_eq!(buckets[58]["accepted"], 4);
    assert_eq!(buckets[58]["dropped"], 3);
    assert_eq!(buckets[0]["t"], iso(minute - 59 * 60));
    assert_eq!(buckets[0]["accepted"], 0);
    assert_eq!(v["totals"], json!({"accepted": 14, "dropped": 5}));
    assert_eq!(
        v["reasons"],
        json!([
            {"reason": "hostname_not_allowed", "count": 3},
            {"reason": "bot", "count": 2},
        ])
    );
    assert_eq!(
        v["versions"],
        json!([
            {"version": 5, "events": 16, "last_seen": iso(minute)},
            {"version": 4, "events": 3, "last_seen": iso(minute - 60)},
        ])
    );
    assert_eq!(v["last_event_at"], iso(minute));
    assert_eq!(v["partial_last"], true);

    // ---- 24h（默认）：24 个 1 小时桶，最后一个是「现在」所在的小时
    let v = get(&a, None).await;
    assert_eq!(v["range"], "24h");
    let buckets = v["buckets"].as_array().unwrap();
    assert_eq!(buckets.len(), 24);
    assert_eq!(buckets[23]["t"], iso(t0 + 3 * 3600));
    assert_eq!(buckets[23]["accepted"], 14);
    assert_eq!(buckets[23]["dropped"], 5);
    assert_eq!(buckets[22]["t"], iso(t0 + 2 * 3600));
    assert_eq!(buckets[22]["accepted"], 6);
    assert_eq!(v["totals"], json!({"accepted": 20, "dropped": 5}));
    assert_eq!(v["versions"][1]["events"], 9);

    // ---- 7d：28 个 6 小时桶
    let v = get(&a, Some("7d")).await;
    let buckets = v["buckets"].as_array().unwrap();
    assert_eq!(buckets.len(), 28);
    assert_eq!(buckets[27]["t"], iso(t0));
    assert_eq!(buckets[27]["accepted"], 20);
    assert_eq!(v["totals"], json!({"accepted": 27, "dropped": 5}));

    // ---- 站点隔离：b 只有自己的数字
    let v = get(&b, Some("1h")).await;
    assert_eq!(v["totals"], json!({"accepted": 1000, "dropped": 500}));
    assert_eq!(v["reasons"], json!([{"reason": "bot", "count": 500}]));
    assert_eq!(v["versions"].as_array().unwrap().len(), 1);
    assert_eq!(v["versions"][0]["version"], 9);

    // ---- 安静的站点：范围内没有数据，最后事件向前回溯
    let v = get(&quiet, Some("1h")).await;
    assert_eq!(v["totals"], json!({"accepted": 0, "dropped": 0}));
    assert_eq!(v["last_event_at"], iso(minute - 3 * 86_400));
    assert!(v["versions"].as_array().unwrap().is_empty());
    // ---- 从没有过数据的站点
    let v = get(&empty, Some("7d")).await;
    assert_eq!(v["totals"], json!({"accepted": 0, "dropped": 0}));
    assert!(v["last_event_at"].is_null());
    assert!(v["reasons"].as_array().unwrap().is_empty());
    assert_eq!(v["buckets"].as_array().unwrap().len(), 28);

    // ---- 站点可以用 site_key 或数字 id 引用；范围参数不合法 400
    let r = http
        .get(&diag_path(a.id, Some("30d")))
        .cookie(&owner)
        .send()
        .await;
    expect_err(&r, 400, "invalid_request");

    cleanup(&env, &[a.id, b.id, quiet.id, empty.id]).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn diagnostics_permissions() {
    let Some(env) = test_env("diagnostics_permissions") else {
        return;
    };
    migrate_once(&env).await;
    let pg = rivu_storage_pg::connect(&env.pg_url, 6).await.unwrap();
    let (http, _) = start(&env, &pg, loose(), Timestamp::now()).await;
    let org = make_org(&pg).await;
    let other = make_org(&pg).await;
    let site = make_site(&pg, &org, "diag-perm").await;
    let path = diag_path(site.id, Some("1h"));

    let mut cookies = Vec::new();
    for role in ["owner", "admin", "editor", "viewer"] {
        let (_, email) = make_user(&pg, Some((&org, role))).await;
        cookies.push((role, login_cookie(&http, &email).await));
    }
    let (_, outsider_email) = make_user(&pg, Some((&other, "owner"))).await;
    let outsider = login_cookie(&http, &outsider_email).await;
    let guest = |role: &'static str| {
        let pg = pg.clone();
        let http = http.clone();
        let site_id = site.id;
        async move {
            let (id, email) = make_user(&pg, None).await;
            sqlx::query(
                "INSERT INTO site_memberships (site_id, user_id, role) VALUES ($1, $2, $3)",
            )
            .bind(site_id as i64)
            .bind(id)
            .bind(role)
            .execute(&pg)
            .await
            .unwrap();
            login_cookie(&http, &email).await
        }
    };

    // 没有会话 401
    expect_err(&http.get(&path).send().await, 401, "unauthorized");
    for (role, cookie) in &cookies {
        let r = http.get(&path).cookie(cookie).send().await;
        match *role {
            "viewer" => expect_err(&r, 403, "forbidden"),
            _ => {
                let v = expect(&r, 200);
                assert_eq!(v["buckets"].as_array().unwrap().len(), 60, "{role}");
            }
        }
    }
    // 站点访客：editor 可以，viewer 不行（有效角色）
    let g = guest("editor").await;
    expect(&http.get(&path).cookie(&g).send().await, 200);
    let g = guest("viewer").await;
    expect_err(&http.get(&path).cookie(&g).send().await, 403, "forbidden");
    // 看不到的站点与不存在的站点是同一个 404
    expect_err(
        &http.get(&path).cookie(&outsider).send().await,
        404,
        "not_found",
    );
    expect_err(
        &http
            .get(&diag_path(u64::MAX / 2, Some("1h")))
            .cookie(&cookies[0].1)
            .send()
            .await,
        404,
        "not_found",
    );
    expect_err(
        &http
            .get(&diag_path("abc", Some("1h")))
            .cookie(&cookies[0].1)
            .send()
            .await,
        404,
        "not_found",
    );
    // 只允许 GET
    let r = http
        .post(&path, &json!({}))
        .cookie(&cookies[0].1)
        .send()
        .await;
    assert_eq!(r.status, 405);
}
