//! 看板的内部端点（`/api/internal/*`：首次初始化、登录与会话、站点与查询、共享链接、公开看板）的端到端测试：
//! 真实的 PostgreSQL 与 ClickHouse，真实的本地 TCP 端口，路由由 `public::router_full` 装配（与 `rivu all` 同一条路径）。
//!
//! 缺少环境变量时跳过并打印原因（同 `public_api.rs`）：
//!   RIVU_TEST_DATABASE_URL     有 DDL 与写权限的 PostgreSQL 连接串（rivu_migrator）
//!   RIVU_TEST_CLICKHOUSE_URL   ClickHouse HTTP 地址
//!
//! 首次初始化要求 `users` 为空，而共享的开发库里到处是别的测试建的用户，所以 setup 相关的用例在**独立的 schema** 里跑：
//! 建一个临时 schema（`search_path` 指向它）、把 PG 迁移应用进去、用完 `DROP SCHEMA ... CASCADE`。
//!
//! 用例：
//! - `setup_*`：只能执行一次、需要令牌、并发只有一个成功、CLI 等价物；
//! - `session_lifecycle`：登录、cookie 属性、会话列表、撤销、登出、登出所有设备、轮换、续期节流、过期；
//! - `disabled_or_deleted_users_lose_their_sessions_immediately`；
//! - `csrf_*`、`login_is_rate_limited_*`；
//! - `session_queries_equal_direct_execution_and_site_access_is_uniform`、`shared_links_*`、
//!   `public_and_shared_access_is_rate_limited`；
//! - `cookie_attributes_in_https_mode`、`secrets_never_reach_the_logs_and_metrics_are_recorded`、
//!   `unknown_accounts_cost_as_much_as_wrong_passwords`（统计方式粗测）。

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use clickhouse::Client;
use jiff::Timestamp;
use jiff::civil::{Date, Weekday, date};
use jiff::tz::TimeZone;
use rivu::dash_api::{self, DashApi, DashLimits};
use rivu::{migrate, public, user_cmd};
use rivu_auth::password::hash_blocking;
use rivu_common::config::{BufferConfig, ChAccount, Config, Env, Role, Secret};
use rivu_common::migration::{self, Store};
use rivu_geo::GeoDb;
use rivu_ingest::pipeline::{Outcome, Pipeline, RequestCtx, SaltCache, SiteSnapshot};
use rivu_query::{ExecOptions, QuerySpec, SiteContext, TenantScope, execute};
use rivu_storage_pg::sites::NewSite;
use rivu_ua::UaParser;
use rivu_writer::{BatchWriter, ChSink, WriterOptions};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::postgres::PgPoolOptions;
use sqlx::{AssertSqlSafe, PgPool};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::OnceCell;

const HOST: &str = "example.test";
/// 处理函数与直接调用共用的「现在」：数据全在它之前。
const NOW: &str = "2026-03-12T12:00:00Z";
const PASSWORD: &str = "correct horse battery";
const SETUP_TOKEN: &str = "setup-token-0123456789";
const HTTP_ORIGIN: &str = "http://127.0.0.1:8080";
const BROWSER_UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36";

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

/// api 角色的配置。默认 http 开发模式（`RIVU_BASE_URL=http://127.0.0.1:8080`），信任回环代理（测试用 `X-Forwarded-For` 区分客户端）。
fn config(env: &TestEnv, extra: &[(&str, &str)]) -> Config {
    let reader = account("reader");
    let mut pairs: Vec<(String, String)> = vec![
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
    for (k, v) in extra {
        pairs.retain(|(pk, _)| pk != k);
        pairs.push(((*k).into(), (*v).into()));
    }
    Config::from_env(Role::Api, &Env::from_pairs(pairs)).expect("测试配置")
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

// ------------------------------------------------------------------ 独立 schema

struct Iso {
    pool: PgPool,
    admin: PgPool,
    schema: String,
}

impl Iso {
    async fn count(&self, table: &str) -> i64 {
        sqlx::query_scalar(AssertSqlSafe(format!("SELECT count(*) FROM {table}")))
            .fetch_one(&self.pool)
            .await
            .unwrap()
    }

    async fn drop(self) {
        self.pool.close().await;
        let _ = sqlx::query(AssertSqlSafe(format!(
            "DROP SCHEMA IF EXISTS {} CASCADE",
            self.schema
        )))
        .execute(&self.admin)
        .await;
    }
}

/// 一个空的临时 schema，里面应用了全部 PG 迁移。
async fn isolated(env: &TestEnv) -> Iso {
    let admin = rivu_storage_pg::connect(&env.pg_url, 2).await.unwrap();
    let schema = format!("t_internal_{:08x}", rand::random::<u32>());
    sqlx::query(AssertSqlSafe(format!("CREATE SCHEMA {schema}")))
        .execute(&admin)
        .await
        .unwrap();
    let path = schema.clone();
    let pool = PgPoolOptions::new()
        .max_connections(8)
        .after_connect(move |conn, _| {
            let path = path.clone();
            Box::pin(async move {
                sqlx::query(AssertSqlSafe(format!("SET search_path TO {path}, public")))
                    .execute(conn)
                    .await?;
                Ok(())
            })
        })
        .connect(&env.pg_url)
        .await
        .unwrap();
    // 不走 `apply_migration`：它会往 `schema_migrations` 里记账，而 search_path 里的 public 已经有那张表（开发库自己的记录）
    let mut migrations = migration::embedded().expect("迁移");
    migrations.sort_by_key(|m| m.version);
    for m in migrations.iter().filter(|m| m.store == Store::Postgres) {
        let mut tx = pool.begin().await.unwrap();
        sqlx::raw_sql(AssertSqlSafe(m.sql.clone()))
            .execute(&mut *tx)
            .await
            .unwrap_or_else(|e| panic!("在临时 schema 里应用 {} 失败：{e:#}", m.file));
        tx.commit().await.unwrap();
    }
    Iso {
        pool,
        admin,
        schema,
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

    fn all(&self, name: &str) -> Vec<&str> {
        self.headers
            .iter()
            .filter(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
            .collect()
    }

    fn json(&self) -> Value {
        serde_json::from_str(&self.body)
            .unwrap_or_else(|e| panic!("响应体不是 JSON（{e}）：{}", self.body))
    }

    /// `Set-Cookie` 里名为 `name` 的那条的完整文本。
    fn set_cookie(&self, name: &str) -> Option<&str> {
        self.all("set-cookie")
            .into_iter()
            .find(|c| c.starts_with(&format!("{name}=")))
    }

    /// 可以原样放进请求 `Cookie` 头的 `name=value`。
    fn cookie_pair(&self, name: &str) -> Option<String> {
        self.set_cookie(name)
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

    fn share(self, slug: &str) -> Self {
        self.header("X-Rivu-Share", slug)
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
        // 服务端在读完请求体之前就回了 4xx 并关了连接（CSRF、无权限）：写入可能被对端重置，响应照样要读
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
    fn post(&self, path: &str, body: &Value) -> Rb<'_> {
        self.rb("POST", path, body.to_string().into_bytes())
            .header("Content-Type", "application/json")
            .header("Origin", &self.origin)
    }

    fn delete(&self, path: &str) -> Rb<'_> {
        self.rb("DELETE", path, Vec::new())
            .header("Content-Type", "application/json")
            .header("Origin", &self.origin)
    }
}

struct Server {
    http: Http,
    geo: Arc<GeoDb>,
    reader: Client,
}

async fn start(env: &TestEnv, pg: &PgPool, config: &Config, limits: DashLimits) -> Server {
    init_logging();
    let geo = Arc::new(GeoDb::with_manual_attribution(
        Some("MaxMind test database"),
        Some("https://github.com/maxmind/MaxMind-DB"),
    ));
    let mmdb = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../crates/geo/tests/data/test-data/GeoIP2-City-Test.mmdb");
    assert!(geo.reload_if_changed(&mmdb).expect("加载测试库"));
    let reader = rivu_storage_ch::client(&env.ch_url, &account("reader"), Some(&ch_database()));
    let now: Timestamp = NOW.parse().unwrap();
    let dash = DashApi::new(config, pg.clone(), reader.clone(), Some(Arc::clone(&geo)))
        .expect("装配内部端点")
        .with_limits(limits)
        .with_clock(move || now);
    let app = public::router_full(
        Role::Api,
        None,
        None,
        Some(dash_api::router(Arc::new(dash))),
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
        http: Http {
            addr,
            origin: config.base_url.clone(),
        },
        geo,
        reader,
    }
}

// ------------------------------------------------------------------ 组织、用户、站点

struct Org {
    id: i64,
    slug: String,
}

async fn make_org(pg: &PgPool) -> Org {
    let slug = unique("org");
    let id: i64 =
        sqlx::query_scalar("INSERT INTO organizations (name, slug) VALUES ($1, $1) RETURNING id")
            .bind(&slug)
            .fetch_one(pg)
            .await
            .unwrap();
    Org { id, slug }
}

struct User {
    id: i64,
    email: String,
}

/// 建用户（密码 [`PASSWORD`]），可选加入组织。
async fn make_user(pg: &PgPool, member_of: Option<(&Org, &str)>) -> User {
    let email = format!("{}@example.test", unique("user"));
    let hash = hash_blocking(PASSWORD).unwrap();
    let id = rivu_storage_pg::users::create_user(
        pg,
        &rivu_storage_pg::users::NewUser {
            email: &email,
            name: Some("Test User"),
            password_hash: &hash,
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
    User { id, email }
}

#[derive(Clone)]
struct Site {
    id: u64,
    key: String,
    org_id: i64,
}

async fn make_site(pg: &PgPool, org: &Org, public: bool) -> Site {
    let hostnames = vec![HOST.to_owned()];
    let created = rivu_storage_pg::sites::create_site(
        pg,
        &NewSite {
            name: "internal-api-e2e",
            timezone: "UTC",
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
    Site {
        id: created.id,
        key: created.site_key,
        org_id: org.id,
    }
}

async fn login(http: &Http, email: &str, password: &str, ip: &str) -> Resp {
    http.post(
        "/api/internal/login",
        &json!({"email": email, "password": password}),
    )
    .ip(ip)
    .send()
    .await
}

/// 登录并返回 `Cookie` 头的值（http 开发模式的 cookie 名）。
async fn login_cookie(http: &Http, email: &str) -> String {
    let r = login(http, email, PASSWORD, "203.0.113.1").await;
    assert_eq!(r.status, 200, "{}", r.body);
    r.cookie_pair("rivu_session").expect("会话 cookie")
}

// ------------------------------------------------------------------ 经管线写入已知事件

struct Ev {
    ts_ms: i64,
    person: usize,
    name: &'static str,
    path: &'static str,
}

fn at(d: Date, h: i8, m: i8, s: i8) -> i64 {
    d.at(h, m, s, 0)
        .to_zoned(TimeZone::UTC)
        .unwrap()
        .timestamp()
        .as_millisecond()
}

/// 6 个人 × 3 天（2026-03-09 至 11）各一个会话，再加「现在」（3 月 12 日 12:00）之前的三个页面浏览：
/// 11:58 的两个人（在最近 5 分钟内）与 11:50 的一个人（不在）。
fn known_events() -> Vec<Ev> {
    let mut out = Vec::new();
    for person in 0..6usize {
        for day in [9i8, 10, 11] {
            let base = at(date(2026, 3, day), 9 + person as i8, 0, 0);
            out.push(Ev {
                ts_ms: base,
                person,
                name: "pageview",
                path: "/",
            });
            out.push(Ev {
                ts_ms: base + 60_000,
                person,
                name: "pageview",
                path: "/pricing",
            });
        }
    }
    for (person, h, m, s) in [(0usize, 11i8, 58i8, 0i8), (1, 11, 58, 1), (2, 11, 50, 0)] {
        out.push(Ev {
            ts_ms: at(date(2026, 3, 12), h, m, s),
            person,
            name: "pageview",
            path: "/",
        });
    }
    out.sort_by_key(|e| e.ts_ms);
    out
}

async fn ingest(env: &TestEnv, pg: &PgPool, sites: &[&Site], geo: &Arc<GeoDb>) {
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
    static PARSER: std::sync::OnceLock<Arc<UaParser>> = std::sync::OnceLock::new();
    let ua = Arc::clone(PARSER.get_or_init(|| Arc::new(UaParser::embedded().expect("uap-core"))));
    let pipeline = Pipeline::new(
        snapshot,
        salts,
        writer.handle(),
        vec!["127.0.0.1/32".parse().unwrap()],
        ua,
        Arc::clone(geo),
    );
    for site in sites {
        for e in known_events() {
            let body = json!({"v": 1, "sv": 1, "s": site.key, "n": e.name,
                              "u": format!("https://{HOST}{}", e.path)});
            let out = pipeline.process(
                body.to_string().as_bytes(),
                RequestCtx {
                    peer: "127.0.0.1".parse().unwrap(),
                    xff: vec![IPS[e.person % IPS.len()].to_owned()],
                    user_agent: UAS[e.person % UAS.len()].to_owned(),
                    now_ms: e.ts_ms,
                },
            );
            assert_eq!(out, Outcome::Accepted, "{} {}", e.name, e.path);
        }
    }
    drop(pipeline);
    writer.shutdown().await.expect("关停写入器");
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

/// 与处理函数相同的站点上下文与「现在」，直接调 `rivu_query::execute`；返回结果的 JSON 文本。
async fn direct(server: &Server, site: &Site, spec: &Value) -> String {
    let ctx = SiteContext {
        site_id: site.id,
        site_key: Some(site.key.clone()),
        timezone: TimeZone::UTC,
        week_starts_on: Weekday::Monday,
        geo_attribution: server.geo.attribution(),
        geo_capability: server.geo.capability(),
    };
    let result = execute(
        &server.reader,
        &TenantScope::new(site.id),
        &ctx,
        QuerySpec::parse(spec).expect("合法的 QuerySpec"),
        ExecOptions::new(10)
            .with_now(NOW.parse().unwrap())
            .with_route("internal-api-test"),
    )
    .await
    .expect("直接执行");
    serde_json::to_string(&result).unwrap()
}

fn specs() -> Vec<(&'static str, Value)> {
    let range = json!(["2026-03-09", "2026-03-11"]);
    vec![
        (
            "overview",
            json!({"metrics": ["visitors", "visits", "pageviews", "bounce_rate"], "date_range": range}),
        ),
        (
            "按页面",
            json!({"metrics": ["visitors", "pageviews"], "dimensions": ["event:page"],
                   "date_range": range, "order_by": [["pageviews", "desc"]],
                   "include": {"total_rows": true}}),
        ),
        (
            "国家与设备（meta 带地理署名）",
            json!({"metrics": ["visitors", "visits"], "dimensions": ["visit:country", "visit:device"],
                   "date_range": range}),
        ),
        (
            "时间序列加对比",
            json!({"metrics": ["pageviews"], "dimensions": ["time:day"],
                   "date_range": ["2026-03-10", "2026-03-11"],
                   "include": {"time_labels": true, "compare": "previous_period"}}),
        ),
    ]
}

fn query_path(site: impl std::fmt::Display) -> String {
    format!("/api/internal/sites/{site}/query")
}

fn realtime_path(site: impl std::fmt::Display) -> String {
    format!("/api/internal/sites/{site}/realtime")
}

fn visitors(v: &Value) -> i64 {
    v["results"][0]["metrics"][0].as_i64().unwrap()
}

// ================================================================== 首次初始化

fn setup_body(email: &str, password: &str, token: Option<&str>) -> Value {
    let mut b = json!({"email": email, "password": password, "org_name": "Acme Analytics"});
    if let Some(t) = token {
        b["token"] = json!(t);
    }
    b
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn setup_runs_once_and_needs_the_token() {
    let Some(env) = test_env("setup_runs_once_and_needs_the_token") else {
        return;
    };
    migrate_once(&env).await;
    let iso = isolated(&env).await;
    let server = start(
        &env,
        &iso.pool,
        &config(&env, &[("RIVU_SETUP_TOKEN", SETUP_TOKEN)]),
        loose(),
    )
    .await;
    let http = &server.http;
    let path = "/api/internal/setup";

    // CSRF 在最前面：没有 Origin 的请求，连令牌都不看
    let r = http
        .post(
            path,
            &setup_body("a@example.test", PASSWORD, Some(SETUP_TOKEN)),
        )
        .remove("Origin")
        .send()
        .await;
    assert_eq!(
        (r.status, r.json()["error"].as_str()),
        (403, Some("csrf_rejected"))
    );

    // 令牌：缺失、错误、长度相同的错误、前缀、多一个字符，都是同一个 403，什么都没有创建
    let same_len = "x".repeat(SETUP_TOKEN.len());
    let prefix = &SETUP_TOKEN[..SETUP_TOKEN.len() - 1];
    let longer = format!("{SETUP_TOKEN}!");
    let mut bodies = Vec::new();
    for token in [
        None,
        Some(""),
        Some("wrong"),
        Some(same_len.as_str()),
        Some(prefix),
        Some(longer.as_str()),
    ] {
        let r = http
            .post(path, &setup_body("a@example.test", PASSWORD, token))
            .send()
            .await;
        assert_eq!(r.status, 403, "{token:?}：{}", r.body);
        assert!(r.set_cookie("rivu_session").is_none());
        bodies.push(r.body);
    }
    assert!(
        bodies.windows(2).all(|w| w[0] == w[1]),
        "错误令牌的响应都相同"
    );
    assert_eq!(iso.count("users").await, 0);

    // 令牌对了，输入不合法：400，仍然什么都没有创建
    for (name, body) in [
        (
            "密码太短",
            setup_body("a@example.test", "short", Some(SETUP_TOKEN)),
        ),
        (
            "邮箱不合法",
            setup_body("not-an-email", PASSWORD, Some(SETUP_TOKEN)),
        ),
        (
            "组织名为空",
            json!({"email": "a@example.test", "password": PASSWORD, "org_name": "  ", "token": SETUP_TOKEN}),
        ),
        (
            "缺字段",
            json!({"email": "a@example.test", "token": SETUP_TOKEN}),
        ),
    ] {
        let r = http.post(path, &body).send().await;
        assert_eq!(r.status, 400, "{name}：{}", r.body);
        assert_eq!(r.json()["error"], "invalid_request", "{name}");
        assert!(!r.body.contains(PASSWORD), "{name}：错误里不回显密码");
    }
    assert_eq!(iso.count("users").await, 0);
    assert_eq!(iso.count("organizations").await, 0);

    // 成功：201，直接登录
    let r = http
        .post(
            path,
            &setup_body("  Admin@Example.TEST ", PASSWORD, Some(SETUP_TOKEN)),
        )
        .send()
        .await;
    assert_eq!(r.status, 201, "{}", r.body);
    let v = r.json();
    assert_eq!(v["user"]["email"], "admin@example.test", "邮箱规范化");
    assert_eq!(v["user"]["is_instance_admin"], true);
    assert_eq!(v["organization"]["role"], "owner");
    assert_eq!(v["organization"]["slug"], "acme-analytics");
    let cookie = r.cookie_pair("rivu_session").expect("已登录");

    let me = http.get("/api/internal/me").cookie(&cookie).send().await;
    assert_eq!(me.status, 200, "{}", me.body);
    let me = me.json();
    assert_eq!(me["user"]["is_instance_admin"], true);
    assert_eq!(me["organizations"][0]["slug"], "acme-analytics");
    assert_eq!(me["organizations"][0]["role"], "owner");

    // 库里：一个用户、一个组织、一条 owner 成员关系；密码是 argon2id
    assert_eq!(iso.count("users").await, 1);
    assert_eq!(iso.count("organizations").await, 1);
    let (hash, admin): (String, bool) =
        sqlx::query_as("SELECT password_hash, is_instance_admin FROM users")
            .fetch_one(&iso.pool)
            .await
            .unwrap();
    assert!(
        hash.starts_with("$argon2id$v=19$m=19456,t=2,p=1$"),
        "{hash}"
    );
    assert!(admin);
    let role: String = sqlx::query_scalar("SELECT role FROM memberships")
        .fetch_one(&iso.pool)
        .await
        .unwrap();
    assert_eq!(role, "owner");

    // 已初始化：再来一律 404（令牌对不对都一样，不让外部探测；响应与任何不存在的路径同样是 404）
    let a = http
        .post(
            path,
            &setup_body("b@example.test", PASSWORD, Some(SETUP_TOKEN)),
        )
        .send()
        .await;
    let b = http
        .post(
            path,
            &setup_body("b@example.test", PASSWORD, Some("wrong-token-wrong-token")),
        )
        .send()
        .await;
    let c = http.post(path, &json!({})).send().await;
    assert_eq!((a.status, b.status, c.status), (404, 404, 404));
    assert_eq!((&a.body, &b.body), (&c.body, &c.body));
    assert_eq!(iso.count("users").await, 1);

    // CLI 等价物同样拒绝
    let err = user_cmd::setup_with(&iso.pool, "c@example.test", PASSWORD, "Other", None)
        .await
        .unwrap_err();
    assert!(format!("{err:#}").contains("已经初始化"), "{err:#}");
    assert_eq!(iso.count("users").await, 1);
    iso.drop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn setup_without_a_configured_token_needs_none_and_concurrent_attempts_create_one_user() {
    let Some(env) = test_env("setup_concurrent") else {
        return;
    };
    migrate_once(&env).await;
    let iso = isolated(&env).await;
    let server = start(&env, &iso.pool, &config(&env, &[]), loose()).await;
    let mut tasks = Vec::new();
    for i in 0..8 {
        let http = server.http.clone();
        tasks.push(tokio::spawn(async move {
            http.post(
                "/api/internal/setup",
                &setup_body(&format!("admin{i}@example.test"), PASSWORD, None),
            )
            .ip(&format!("198.51.100.{i}"))
            .send()
            .await
            .status
        }));
    }
    let mut statuses = Vec::new();
    for t in tasks {
        statuses.push(t.await.unwrap());
    }
    assert_eq!(
        statuses.iter().filter(|s| **s == 201).count(),
        1,
        "{statuses:?}"
    );
    assert_eq!(
        statuses.iter().filter(|s| **s == 404).count(),
        7,
        "{statuses:?}"
    );
    assert_eq!(iso.count("users").await, 1);
    assert_eq!(iso.count("organizations").await, 1);
    assert_eq!(iso.count("memberships").await, 1);
    iso.drop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn setup_attempts_are_rate_limited_per_ip() {
    let Some(env) = test_env("setup_rate_limit") else {
        return;
    };
    migrate_once(&env).await;
    let iso = isolated(&env).await;
    let server = start(
        &env,
        &iso.pool,
        &config(&env, &[("RIVU_SETUP_TOKEN", SETUP_TOKEN)]),
        DashLimits::default(),
    )
    .await;
    let attempt = |ip: &'static str| {
        let http = server.http.clone();
        async move {
            http.post(
                "/api/internal/setup",
                &setup_body("a@example.test", PASSWORD, Some("guess-guess-guess-guess")),
            )
            .ip(ip)
            .send()
            .await
        }
    };
    for i in 0..5 {
        assert_eq!(attempt("198.51.100.7").await.status, 403, "第 {i} 次猜令牌");
    }
    let r = attempt("198.51.100.7").await;
    assert_eq!(r.status, 429, "{}", r.body);
    assert!(r.header("retry-after").is_some());
    // 另一个 IP 不受影响
    assert_eq!(attempt("198.51.100.8").await.status, 403);
    iso.drop().await;
}

// ================================================================== 登录与会话

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn session_lifecycle() {
    let Some(env) = test_env("session_lifecycle") else {
        return;
    };
    migrate_once(&env).await;
    let pg = rivu_storage_pg::connect(&env.pg_url, 6).await.unwrap();
    let server = start(&env, &pg, &config(&env, &[]), loose()).await;
    let http = &server.http;
    let org = make_org(&pg).await;
    let user = make_user(&pg, Some((&org, "editor"))).await;
    let other = make_user(&pg, Some((&org, "viewer"))).await;

    // 统一的失败：错误密码、不存在的账号、被禁用的账号、不合法的邮箱，响应逐字节相同，且没有 cookie
    let wrong = login(http, &user.email, "wrong password here", "203.0.113.1").await;
    let unknown = login(http, "nobody-at-all@example.test", PASSWORD, "203.0.113.1").await;
    let garbage = login(http, "not an email", PASSWORD, "203.0.113.1").await;
    let too_long = login(http, &user.email, &"x".repeat(5000), "203.0.113.1").await;
    for r in [&wrong, &unknown, &garbage, &too_long] {
        assert_eq!(r.status, 401);
        assert_eq!(r.body, wrong.body);
        assert!(r.all("set-cookie").is_empty());
        assert_eq!(r.header("cache-control"), Some("no-store"));
    }
    assert_eq!(wrong.json()["error"], "invalid_credentials");

    // 登录成功：cookie 属性（http 开发模式：没有 Secure 与 __Host- 前缀）
    let r = http
        .post(
            "/api/internal/login",
            &json!({"email": user.email.to_uppercase(), "password": PASSWORD}),
        )
        .send()
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.json()["user"]["email"], user.email);
    assert!(r.json()["user"].get("password_hash").is_none());
    let set = r
        .set_cookie("rivu_session")
        .expect("会话 cookie")
        .to_owned();
    let (pair, attrs) = set.split_once("; ").unwrap();
    assert_eq!(
        attrs, "Max-Age=1209600; Path=/; HttpOnly; SameSite=Lax",
        "{set}"
    );
    let token = pair.strip_prefix("rivu_session=").unwrap();
    assert_eq!(token.len(), 43);
    let cookie1 = pair.to_owned();

    // 库里只有令牌的 SHA-256，没有明文
    let stored: Vec<u8> =
        sqlx::query_scalar("SELECT token_hash FROM auth_sessions WHERE user_id = $1")
            .bind(user.id)
            .fetch_one(&pg)
            .await
            .unwrap();
    assert_eq!(stored, Sha256::digest(token.as_bytes()).to_vec());
    let leaked: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM auth_sessions WHERE device_label LIKE '%' || $1 || '%'",
    )
    .bind(token)
    .fetch_one(&pg)
    .await
    .unwrap();
    assert_eq!(leaked, 0);

    // 第二个设备：客户端给标签
    let r2 = http
        .post(
            "/api/internal/login",
            &json!({"email": user.email, "password": PASSWORD, "device_label": "Pixel 6"}),
        )
        .header(
            "User-Agent",
            "Mozilla/5.0 (X11; Linux x86_64; rv:126.0) Gecko/20100101 Firefox/126.0",
        )
        .send()
        .await;
    let cookie2 = r2.cookie_pair("rivu_session").unwrap();
    assert_ne!(cookie1, cookie2);

    let list = http
        .get("/api/internal/sessions")
        .cookie(&cookie1)
        .send()
        .await;
    assert_eq!(list.status, 200, "{}", list.body);
    let sessions = list.json()["sessions"].as_array().unwrap().clone();
    assert_eq!(sessions.len(), 2);
    let labels: Vec<_> = sessions
        .iter()
        .map(|s| s["device_label"].as_str().unwrap())
        .collect();
    assert!(
        labels.contains(&"Chrome on Windows"),
        "{labels:?}（没给标签时由 UA 归纳）"
    );
    assert!(labels.contains(&"Pixel 6"), "{labels:?}");
    let current: Vec<_> = sessions.iter().filter(|s| s["current"] == true).collect();
    assert_eq!(current.len(), 1);
    assert_eq!(
        current[0]["device_label"], "Chrome on Windows",
        "当前会话标记：cookie1 是第一个设备"
    );
    for s in &sessions {
        for k in ["created_at", "last_used_at", "expires_at"] {
            assert!(s[k].as_str().unwrap().ends_with('Z'), "{s}");
        }
        assert!(s.get("token_hash").is_none() && s.get("token").is_none());
    }
    let other_id = sessions.iter().find(|s| s["current"] == false).unwrap()["id"]
        .as_i64()
        .unwrap();
    let my_id = current[0]["id"].as_i64().unwrap();

    // 撤销别的设备的会话；别人的会话与不存在的 id 是同一个 404
    let foreign_cookie = login_cookie(http, &other.email).await;
    let foreign_id = http
        .get("/api/internal/sessions")
        .cookie(&foreign_cookie)
        .send()
        .await
        .json()["sessions"][0]["id"]
        .as_i64()
        .unwrap();
    let a = http
        .delete(&format!("/api/internal/sessions/{foreign_id}"))
        .cookie(&cookie1)
        .send()
        .await;
    let b = http
        .delete("/api/internal/sessions/999999999")
        .cookie(&cookie1)
        .send()
        .await;
    let c = http
        .delete("/api/internal/sessions/not-a-number")
        .cookie(&cookie1)
        .send()
        .await;
    assert_eq!((a.status, b.status, c.status), (404, 404, 404));
    assert_eq!((&a.body, &a.body), (&b.body, &c.body));
    assert_eq!(
        http.get("/api/internal/me")
            .cookie(&foreign_cookie)
            .send()
            .await
            .status,
        200,
        "别人的会话没被动"
    );
    let r = http
        .delete(&format!("/api/internal/sessions/{other_id}"))
        .cookie(&cookie1)
        .send()
        .await;
    assert_eq!(r.status, 204);
    assert_eq!(
        http.get("/api/internal/me")
            .cookie(&cookie2)
            .send()
            .await
            .status,
        401,
        "被撤销的设备立即失效"
    );
    assert_eq!(
        http.get("/api/internal/me")
            .cookie(&cookie1)
            .send()
            .await
            .status,
        200
    );

    // 登录时请求里带着旧会话：旧会话被撤销（轮换，不复用令牌）
    let r = http
        .post(
            "/api/internal/login",
            &json!({"email": user.email, "password": PASSWORD}),
        )
        .cookie(&cookie1)
        .send()
        .await;
    let cookie3 = r.cookie_pair("rivu_session").unwrap();
    assert_eq!(
        http.get("/api/internal/me")
            .cookie(&cookie1)
            .send()
            .await
            .status,
        401
    );
    assert_eq!(
        http.get("/api/internal/me")
            .cookie(&cookie3)
            .send()
            .await
            .status,
        200
    );
    let my_id_now = http
        .get("/api/internal/sessions")
        .cookie(&cookie3)
        .send()
        .await
        .json()["sessions"][0]["id"]
        .as_i64()
        .unwrap();
    assert_ne!(my_id_now, my_id);

    // 续期节流：10 分钟内不写库、不刷新 cookie；超过 10 分钟才续期并刷新 cookie
    let last_used = |id: i64| {
        let pg = pg.clone();
        async move {
            sqlx::query_scalar::<_, String>(
                "SELECT last_used_at::text FROM auth_sessions WHERE id = $1",
            )
            .bind(id)
            .fetch_one(&pg)
            .await
            .unwrap()
        }
    };
    sqlx::query(
        "UPDATE auth_sessions SET last_used_at = now() - interval '2 minutes' WHERE id = $1",
    )
    .bind(my_id_now)
    .execute(&pg)
    .await
    .unwrap();
    let before = last_used(my_id_now).await;
    let r = http.get("/api/internal/me").cookie(&cookie3).send().await;
    assert_eq!(r.status, 200);
    assert!(
        r.set_cookie("rivu_session").is_none(),
        "10 分钟内不刷新 cookie"
    );
    assert_eq!(last_used(my_id_now).await, before, "10 分钟内不写库");
    sqlx::query(
        "UPDATE auth_sessions SET last_used_at = now() - interval '11 minutes',
                expires_at = now() + interval '13 days' WHERE id = $1",
    )
    .bind(my_id_now)
    .execute(&pg)
    .await
    .unwrap();
    let r = http.get("/api/internal/me").cookie(&cookie3).send().await;
    assert_eq!(r.status, 200);
    let refreshed = r
        .set_cookie("rivu_session")
        .expect("超过 10 分钟：刷新 cookie");
    assert_eq!(
        refreshed,
        set.replace(token, cookie3.strip_prefix("rivu_session=").unwrap())
    );
    let renewed: bool = sqlx::query_scalar(
        "SELECT last_used_at > now() - interval '1 minute' AND expires_at > now() + interval '13 days 23 hours'
         FROM auth_sessions WHERE id = $1",
    )
    .bind(my_id_now)
    .fetch_one(&pg)
    .await
    .unwrap();
    assert!(renewed, "last_used_at 与 expires_at 都已续期（滑动 14 天）");
    assert!(
        http.get("/api/internal/me")
            .cookie(&cookie3)
            .send()
            .await
            .set_cookie("rivu_session")
            .is_none()
    );

    // 登出：204，清 cookie（Max-Age=0），会话立即失效；没有会话的登出同样成功（幂等）
    let r = http
        .post("/api/internal/logout", &json!({}))
        .cookie(&cookie3)
        .send()
        .await;
    assert_eq!(r.status, 204);
    let cleared = r.set_cookie("rivu_session").unwrap();
    assert!(
        cleared.contains("Max-Age=0")
            && cleared.contains("HttpOnly")
            && cleared.contains("SameSite=Lax"),
        "{cleared}"
    );
    assert_eq!(
        http.get("/api/internal/me")
            .cookie(&cookie3)
            .send()
            .await
            .status,
        401
    );
    let r = http.post("/api/internal/logout", &json!({})).send().await;
    assert_eq!(r.status, 204);

    // 退出所有设备：全部会话失效，别的用户的不受影响
    let c_a = login_cookie(http, &user.email).await;
    let c_b = login_cookie(http, &user.email).await;
    assert_eq!(
        http.post("/api/internal/logout-all", &json!({}))
            .send()
            .await
            .status,
        401,
        "要求会话"
    );
    let r = http
        .post("/api/internal/logout-all", &json!({}))
        .cookie(&c_a)
        .send()
        .await;
    assert_eq!(r.status, 204);
    for c in [&c_a, &c_b] {
        assert_eq!(
            http.get("/api/internal/me").cookie(c).send().await.status,
            401
        );
    }
    assert_eq!(
        http.get("/api/internal/me")
            .cookie(&foreign_cookie)
            .send()
            .await
            .status,
        200
    );

    // 过期的会话与格式不对的 cookie
    let c = login_cookie(http, &user.email).await;
    sqlx::query(
        "UPDATE auth_sessions SET expires_at = now() - interval '1 minute' WHERE user_id = $1",
    )
    .bind(user.id)
    .execute(&pg)
    .await
    .unwrap();
    assert_eq!(
        http.get("/api/internal/me").cookie(&c).send().await.status,
        401
    );
    for bad in [
        "rivu_session=".to_owned(),
        "rivu_session=abc".to_owned(),
        format!("rivu_session={}", "A".repeat(43)),
        "other=1".to_owned(),
    ] {
        assert_eq!(
            http.get("/api/internal/me")
                .cookie(&bad)
                .send()
                .await
                .status,
            401,
            "{bad}"
        );
    }
    // 需要会话的端点，没有会话一律 401
    for path in [
        "/api/internal/me",
        "/api/internal/sites",
        "/api/internal/sessions",
    ] {
        let r = http.get(path).send().await;
        assert_eq!(r.status, 401, "{path}");
        assert_eq!(r.json()["error"], "unauthorized");
    }
    // 方法与未实现的内部端点
    let r = http.get("/api/internal/login").send().await;
    assert_eq!((r.status, r.header("allow")), (405, Some("POST")));
    assert!(
        r.header("access-control-allow-origin").is_none(),
        "不开 CORS"
    );
    let r = http
        .rb("OPTIONS", "/api/internal/login", Vec::new())
        .send()
        .await;
    assert_eq!(r.status, 405);
    assert!(r.header("access-control-allow-origin").is_none());
    for path in [
        "/api/internal/orgs",
        "/api/internal/sites/1/settings",
        "/api/internal",
    ] {
        assert_eq!(http.get(path).send().await.status, 501, "{path}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn disabled_or_deleted_users_lose_their_sessions_immediately() {
    let Some(env) = test_env("disabled_users") else {
        return;
    };
    migrate_once(&env).await;
    let pg = rivu_storage_pg::connect(&env.pg_url, 4).await.unwrap();
    let server = start(&env, &pg, &config(&env, &[]), loose()).await;
    let http = &server.http;
    let org = make_org(&pg).await;
    let user = make_user(&pg, Some((&org, "owner"))).await;
    let c1 = login_cookie(http, &user.email).await;
    let c2 = login_cookie(http, &user.email).await;
    assert_eq!(
        http.get("/api/internal/me").cookie(&c1).send().await.status,
        200
    );

    // 禁用：下一个请求就失效（不等任何清理任务），并且不能再登录（与错误密码同一个响应）
    assert!(
        rivu_storage_pg::users::set_disabled(&pg, &user.email, true)
            .await
            .unwrap()
    );
    for c in [&c1, &c2] {
        assert_eq!(
            http.get("/api/internal/me").cookie(c).send().await.status,
            401
        );
        assert_eq!(
            http.get("/api/internal/sites")
                .cookie(c)
                .send()
                .await
                .status,
            401
        );
    }
    let r = login(http, &user.email, PASSWORD, "203.0.113.1").await;
    let wrong = login(http, &user.email, "wrong password here", "203.0.113.1").await;
    assert_eq!(
        (r.status, &r.body),
        (401, &wrong.body),
        "被禁用与密码错误不可区分"
    );
    let active: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM auth_sessions WHERE user_id = $1 AND revoked_at IS NULL",
    )
    .bind(user.id)
    .fetch_one(&pg)
    .await
    .unwrap();
    assert_eq!(active, 0, "禁用时同一个事务里撤销全部会话");

    // 重新启用：可以登录，但被撤销的会话不会复活
    assert!(
        rivu_storage_pg::users::set_disabled(&pg, &user.email, false)
            .await
            .unwrap()
    );
    assert_eq!(
        http.get("/api/internal/me").cookie(&c1).send().await.status,
        401
    );
    let c3 = login_cookie(http, &user.email).await;
    assert_eq!(
        http.get("/api/internal/me").cookie(&c3).send().await.status,
        200
    );

    // 只改 disabled_at（绕过 set_disabled）也立即生效：读取路径自己校验用户状态
    sqlx::query("UPDATE users SET disabled_at = now() WHERE id = $1")
        .bind(user.id)
        .execute(&pg)
        .await
        .unwrap();
    assert_eq!(
        http.get("/api/internal/me").cookie(&c3).send().await.status,
        401
    );
    sqlx::query("UPDATE users SET disabled_at = NULL WHERE id = $1")
        .bind(user.id)
        .execute(&pg)
        .await
        .unwrap();
    assert_eq!(
        http.get("/api/internal/me").cookie(&c3).send().await.status,
        200
    );
    // 删除（软删除）同样
    sqlx::query("UPDATE users SET deleted_at = now() WHERE id = $1")
        .bind(user.id)
        .execute(&pg)
        .await
        .unwrap();
    assert_eq!(
        http.get("/api/internal/me").cookie(&c3).send().await.status,
        401
    );
    assert_eq!(
        login(http, &user.email, PASSWORD, "203.0.113.1")
            .await
            .status,
        401
    );
    sqlx::query("UPDATE users SET deleted_at = NULL WHERE id = $1")
        .bind(user.id)
        .execute(&pg)
        .await
        .unwrap();

    // 组织成员规则：不能降级最后一名 owner
    let err = rivu_storage_pg::users::upsert_member(&pg, org.id, user.id, "viewer")
        .await
        .unwrap_err();
    assert!(format!("{err:#}").contains("最后一名 owner"), "{err:#}");
    let second = make_user(&pg, Some((&org, "owner"))).await;
    rivu_storage_pg::users::upsert_member(&pg, org.id, user.id, "viewer")
        .await
        .unwrap();
    let c4 = login_cookie(http, &user.email).await;
    let me = http.get("/api/internal/me").cookie(&c4).send().await.json();
    assert_eq!(me["organizations"][0]["role"], "viewer");
    let _ = second;
    // 用户列表带组织与角色
    let listed = rivu_storage_pg::users::list_users(&pg).await.unwrap();
    let row = listed.iter().find(|u| u.id == user.id).unwrap();
    assert_eq!(row.memberships, vec![format!("{}:viewer", org.slug)]);
    assert!(!row.disabled);
}

// ================================================================== CSRF 与限流

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn csrf_rejects_cross_origin_and_non_json_state_changing_requests() {
    let Some(env) = test_env("csrf") else {
        return;
    };
    migrate_once(&env).await;
    let pg = rivu_storage_pg::connect(&env.pg_url, 4).await.unwrap();
    let server = start(&env, &pg, &config(&env, &[]), loose()).await;
    let http = &server.http;
    let org = make_org(&pg).await;
    let user = make_user(&pg, Some((&org, "owner"))).await;
    let site = make_site(&pg, &org, false).await;
    let cookie = login_cookie(http, &user.email).await;
    let sessions_before = http
        .get("/api/internal/sessions")
        .cookie(&cookie)
        .send()
        .await
        .json()["sessions"]
        .as_array()
        .unwrap()
        .len();

    let login_body = json!({"email": user.email, "password": PASSWORD});
    let q = json!({"metrics": ["visitors"], "date_range": "day"});
    // 每个会改变状态的端点（含用 POST 的查询）：缺 Origin、别的源、别的端口与协议、null、表单类型、跨站声明，全部 403
    type Mk = Box<dyn Fn(&Http) -> Rb<'_>>;
    let endpoints: Vec<(&str, Mk)> = vec![
        (
            "login",
            Box::new({
                let b = login_body.clone();
                move |h| h.post("/api/internal/login", &b)
            }),
        ),
        (
            "logout",
            Box::new(|h| {
                h.post("/api/internal/logout", &json!({}))
                    .cookie("rivu_session=x")
            }),
        ),
        (
            "logout-all",
            Box::new({
                let c = cookie.clone();
                move |h| h.post("/api/internal/logout-all", &json!({})).cookie(&c)
            }),
        ),
        (
            "session delete",
            Box::new({
                let c = cookie.clone();
                move |h| h.delete("/api/internal/sessions/1").cookie(&c)
            }),
        ),
        (
            "query",
            Box::new({
                let c = cookie.clone();
                let p = query_path(site.id);
                let q = q.clone();
                move |h| h.post(&p, &q).cookie(&c)
            }),
        ),
        (
            "shared auth",
            Box::new(|h| {
                h.post(
                    &format!("/api/internal/shared/{}/auth", "a".repeat(43)),
                    &json!({"password": "x"}),
                )
            }),
        ),
        (
            "setup",
            Box::new(|h| h.post("/api/internal/setup", &json!({}))),
        ),
    ];
    for (name, mk) in &endpoints {
        let variants: Vec<(&str, Rb<'_>)> = vec![
            ("没有 Origin", mk(http).remove("Origin")),
            (
                "别的源",
                mk(http).header("Origin", "https://evil.example.net"),
            ),
            (
                "同主机别的端口",
                mk(http).header("Origin", "http://127.0.0.1:9999"),
            ),
            (
                "同主机别的协议",
                mk(http).header("Origin", "https://127.0.0.1:8080"),
            ),
            (
                "localhost 不等于 127.0.0.1",
                mk(http).header("Origin", "http://localhost:8080"),
            ),
            ("Origin: null", mk(http).header("Origin", "null")),
            (
                "表单类型",
                mk(http).header("Content-Type", "application/x-www-form-urlencoded"),
            ),
            ("text/plain", mk(http).header("Content-Type", "text/plain")),
            ("没有 Content-Type", mk(http).remove("Content-Type")),
            ("跨站声明", mk(http).header("Sec-Fetch-Site", "cross-site")),
        ];
        for (what, rb) in variants {
            let r = rb.send().await;
            assert_eq!(r.status, 403, "{name}：{what}：{}", r.body);
            assert_eq!(r.json()["error"], "csrf_rejected", "{name}：{what}");
            assert!(r.all("set-cookie").is_empty(), "{name}：{what}");
        }
    }
    // 被拒绝的请求没有副作用：会话没有变多，登录没有发生
    let sessions_after = http
        .get("/api/internal/sessions")
        .cookie(&cookie)
        .send()
        .await
        .json()["sessions"]
        .as_array()
        .unwrap()
        .len();
    assert_eq!(sessions_before, sessions_after);
    assert_eq!(
        http.get("/api/internal/me")
            .cookie(&cookie)
            .send()
            .await
            .status,
        200,
        "没有被登出"
    );

    // 同源的 JSON 请求通过（Content-Type 带参数、Origin 与 base url 同源）
    let r = http
        .post("/api/internal/logout", &json!({}))
        .header("Content-Type", "application/json; charset=utf-8")
        .send()
        .await;
    assert_eq!(r.status, 204);
    // GET 不受影响：别的 Origin 也能读自己的响应头（但没有 CORS 头，浏览器不会把响应交给别的源）
    let r = http
        .get("/api/internal/me")
        .cookie(&cookie)
        .header("Origin", "https://evil.example.net")
        .send()
        .await;
    assert_eq!(r.status, 200);
    assert!(r.header("access-control-allow-origin").is_none());
    cleanup(&env, &[site.id]).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn login_is_rate_limited_per_ip_and_per_account() {
    let Some(env) = test_env("login_rate_limit") else {
        return;
    };
    migrate_once(&env).await;
    let pg = rivu_storage_pg::connect(&env.pg_url, 4).await.unwrap();
    let server = start(&env, &pg, &config(&env, &[]), DashLimits::default()).await;
    let http = &server.http;
    let org = make_org(&pg).await;
    let victim = make_user(&pg, Some((&org, "owner"))).await;
    let bystander = make_user(&pg, Some((&org, "viewer"))).await;

    // (账号, IP) 组合：同一来源对同一账号 5 次错误后第 6 次 429，之后连正确的密码也 429；
    // 账号大小写不同算同一个账号
    for i in 0..5 {
        let email = if i % 2 == 0 {
            victim.email.to_uppercase()
        } else {
            victim.email.clone()
        };
        let r = login(http, &email, "wrong password here", "198.51.100.10").await;
        assert_eq!(r.status, 401, "第 {i} 次");
    }
    let blocked = login(http, &victim.email, PASSWORD, "198.51.100.10").await;
    assert_eq!(blocked.status, 429, "{}", blocked.body);
    let retry: u64 = blocked.header("retry-after").unwrap().parse().unwrap();
    assert!((1..=60).contains(&retry), "{retry}");
    assert_eq!(blocked.json()["error"], "rate_limited");
    assert!(blocked.all("set-cookie").is_empty());
    // 同一个 IP 的另一个账号：不受影响（组合桶，且 IP 桶还有余量）
    assert_eq!(
        login(http, &bystander.email, PASSWORD, "198.51.100.10")
            .await
            .status,
        200
    );

    // 锁号回归：攻击者从 5 个不同 IP 各打满受害者的 5 次错误密码（共 25 次失败），
    // 真用户从自己的 IP 仍然能登录（没有单纯按账号的阻断桶）
    for ip in 0..5 {
        for i in 0..5 {
            let r = login(
                http,
                &victim.email,
                "wrong password here",
                &format!("198.51.100.{}", 20 + ip),
            )
            .await;
            assert_eq!(r.status, 401, "攻击 IP {ip} 第 {i} 次：{}", r.body);
        }
        // 每个攻击 IP 自己的组合桶打满后才 429
        assert_eq!(
            login(
                http,
                &victim.email,
                "wrong password here",
                &format!("198.51.100.{}", 20 + ip)
            )
            .await
            .status,
            429
        );
    }
    let real = login(http, &victim.email, PASSWORD, "198.51.100.99").await;
    assert_eq!(real.status, 200, "真用户被攻击者锁住了：{}", real.body);

    // 不存在的账号：同样的限流，同样的响应（限流本身不泄露账号是否存在）
    let ghost = format!("ghost-{}@example.test", unique("g"));
    for i in 0..5 {
        let r = login(http, &ghost, "wrong password here", "198.51.100.50").await;
        assert_eq!(r.status, 401, "第 {i} 次");
    }
    let ghost_blocked = login(http, &ghost, PASSWORD, "198.51.100.50").await;
    assert_eq!(ghost_blocked.status, 429);
    assert_eq!(ghost_blocked.json()["error"], blocked.json()["error"]);
    assert_eq!(ghost_blocked.json()["message"], blocked.json()["message"]);

    // 每 IP 桶：同一个 IP 对 20 个不同账号（每个只试一次，组合桶不触发）后第 21 次 429，正确的密码也一样
    for i in 0..20 {
        let r = login(
            http,
            &format!("spray{i}-{}@example.test", unique("s")),
            "wrong password here",
            "198.51.100.60",
        )
        .await;
        assert_eq!(r.status, 401, "第 {i} 个账号");
    }
    let sprayed = login(http, &bystander.email, PASSWORD, "198.51.100.60").await;
    assert_eq!(sprayed.status, 429, "{}", sprayed.body);
    assert!(sprayed.header("retry-after").is_some());
    // 别的 IP 不受影响
    assert_eq!(
        login(http, &bystander.email, PASSWORD, "198.51.100.61")
            .await
            .status,
        200
    );
}

#[tokio::test(flavor = "current_thread")]
async fn login_failure_alerts_count_but_never_block() {
    let Some(env) = test_env("login_failure_alerts") else {
        return;
    };
    migrate_once(&env).await;
    let buf = init_logging();
    let recorder = metrics_exporter_prometheus::PrometheusBuilder::new().build_recorder();
    let metrics = recorder.handle();
    let _metrics_guard = metrics::set_default_local_recorder(&recorder);
    let pg = rivu_storage_pg::connect(&env.pg_url, 4).await.unwrap();
    // 账号阈值 3、全局阈值 6；阻断桶放宽：只看告警
    let limits = DashLimits {
        account_fail_alert: 3,
        global_fail_alert: 6,
        ..loose()
    };
    let server = start(&env, &pg, &config(&env, &[]), limits).await;
    let http = &server.http;
    let org = make_org(&pg).await;
    let victim = make_user(&pg, Some((&org, "owner"))).await;
    let ghost = format!("alert-ghost-{}@example.test", unique("g"));

    // 受害者 5 次失败：第 3 次达到账号阈值告警一次，之后同一窗口不再重复；全程都是 401，没有 429
    for i in 0..5 {
        let r = login(http, &victim.email, "wrong password here", "203.0.113.211").await;
        assert_eq!(r.status, 401, "第 {i} 次：{}", r.body);
    }
    let text = metrics.render();
    assert_eq!(
        sample(&text, "rivu_auth_login_alerts_total{kind=\"account\"}"),
        Some(1.0),
        "{text}"
    );
    assert_eq!(
        sample(&text, "rivu_auth_login_alerts_total{kind=\"global\"}"),
        None,
        "5 次失败还没到全局阈值：{text}"
    );
    // 不存在的账号也计数；全局第 6 次失败触发全局告警，之后不再重复
    for i in 0..3 {
        let r = login(http, &ghost, "wrong password here", "203.0.113.212").await;
        assert_eq!(r.status, 401, "第 {i} 次：{}", r.body);
    }
    let text = metrics.render();
    assert_eq!(
        sample(&text, "rivu_auth_login_alerts_total{kind=\"global\"}"),
        Some(1.0),
        "{text}"
    );
    assert_eq!(
        sample(&text, "rivu_auth_login_alerts_total{kind=\"account\"}"),
        Some(2.0),
        "不存在的账号第 3 次失败也达到账号阈值：{text}"
    );
    // 超过阈值之后真用户仍然能登录（只告警不阻断）
    let ok = login(http, &victim.email, PASSWORD, "203.0.113.213").await;
    assert_eq!(ok.status, 200, "{}", ok.body);

    // 告警日志不带邮箱与 IP
    let logs = String::from_utf8_lossy(&buf.0.lock().unwrap()).into_owned();
    assert!(logs.contains("达到告警阈值"), "应有告警日志");
    for (what, secret) in [
        ("受害者邮箱", victim.email.as_str()),
        ("不存在账号的邮箱", ghost.as_str()),
        ("客户端 IP", "203.0.113.211"),
    ] {
        assert!(!logs.contains(secret), "日志里出现了{what}");
    }
}

// ================================================================== 看板查询与站点访问

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn session_queries_equal_direct_execution_and_site_access_is_uniform() {
    let Some(env) = test_env("dashboard_queries") else {
        return;
    };
    migrate_once(&env).await;
    let pg = rivu_storage_pg::connect(&env.pg_url, 6).await.unwrap();
    let server = start(&env, &pg, &config(&env, &[]), loose()).await;
    let http = &server.http;

    let org_a = make_org(&pg).await;
    let org_b = make_org(&pg).await;
    let owner = make_user(&pg, Some((&org_a, "owner"))).await;
    let viewer = make_user(&pg, Some((&org_a, "viewer"))).await;
    let guest = make_user(&pg, None).await;
    let outsider = make_user(&pg, Some((&org_b, "owner"))).await;
    let admin = make_user(&pg, None).await;
    sqlx::query("UPDATE users SET is_instance_admin = true WHERE id = $1")
        .bind(admin.id)
        .execute(&pg)
        .await
        .unwrap();
    let site_a = make_site(&pg, &org_a, false).await;
    let site_pub = make_site(&pg, &org_a, true).await;
    let site_b = make_site(&pg, &org_b, false).await;
    sqlx::query("INSERT INTO site_memberships (site_id, user_id, role) VALUES ($1, $2, 'viewer')")
        .bind(site_a.id as i64)
        .bind(guest.id)
        .execute(&pg)
        .await
        .unwrap();
    ingest(&env, &pg, &[&site_a, &site_pub, &site_b], &server.geo).await;

    let c_owner = login_cookie(http, &owner.email).await;
    let c_viewer = login_cookie(http, &viewer.email).await;
    let c_guest = login_cookie(http, &guest.email).await;
    let c_outsider = login_cookie(http, &outsider.email).await;
    let c_admin = login_cookie(http, &admin.email).await;

    // 会话访问的结果与直接 execute 逐字节相同；路径可以是数字 id，也可以是 site_key；
    // 请求体里的 site 可省略、可等于路径的站点
    for (name, spec) in specs() {
        let want = direct(&server, &site_a, &spec).await;
        for (who, cookie) in [
            ("owner", &c_owner),
            ("viewer", &c_viewer),
            ("站点访客", &c_guest),
        ] {
            for path in [query_path(site_a.id), query_path(&site_a.key)] {
                let r = http.post(&path, &spec).cookie(cookie).send().await;
                assert_eq!(r.status, 200, "{name}：{who}：{}", r.body);
                assert_eq!(r.body, want, "{name}：{who}：{path}");
                assert_eq!(r.header("cache-control"), Some("no-store"));
                assert_eq!(r.header("content-type"), Some("application/json"));
            }
        }
        let mut with_site = spec.clone();
        with_site["site"] = json!(site_a.key);
        let want_with_site = direct(&server, &site_a, &with_site).await;
        let r = http
            .post(&query_path(site_a.id), &with_site)
            .cookie(&c_owner)
            .send()
            .await;
        assert_eq!(
            (r.status, r.body.as_str()),
            (200, want_with_site.as_str()),
            "{name}：带 site"
        );
    }
    // meta 带地理署名
    let geo_spec = &specs()[2].1;
    let r = http
        .post(&query_path(site_a.id), geo_spec)
        .cookie(&c_owner)
        .send()
        .await;
    assert!(
        r.json()["meta"]["geo_attribution"]["text"].is_string(),
        "{}",
        r.body
    );

    // 请求体里的 site 与路径不一致：404（与「站点不存在」同一个响应）
    let spec = &specs()[0].1;
    let mut wrong_site = spec.clone();
    wrong_site["site"] = json!(site_pub.key);
    let mismatch = http
        .post(&query_path(site_a.id), &wrong_site)
        .cookie(&c_owner)
        .send()
        .await;
    let unknown = http
        .post(&query_path("zzzzzzzzzzzz"), spec)
        .cookie(&c_owner)
        .send()
        .await;
    assert_eq!((mismatch.status, unknown.status), (404, 404));
    assert_eq!(mismatch.body, unknown.body);

    // 无权的站点、不存在的站点、不存在的 id、不合法的路径段：404，响应逐字节相同
    let denied: Vec<(&str, Resp)> = vec![
        (
            "别的组织的站点",
            http.post(&query_path(site_b.id), spec)
                .cookie(&c_owner)
                .send()
                .await,
        ),
        (
            "别的组织的站点（key）",
            http.post(&query_path(&site_b.key), spec)
                .cookie(&c_owner)
                .send()
                .await,
        ),
        (
            "外人访问站点 A",
            http.post(&query_path(site_a.id), spec)
                .cookie(&c_outsider)
                .send()
                .await,
        ),
        (
            "实例管理员没有隐式权限",
            http.post(&query_path(site_a.id), spec)
                .cookie(&c_admin)
                .send()
                .await,
        ),
        (
            "站点访客看不到同组织的别的站点",
            http.post(&query_path(site_b.id), spec)
                .cookie(&c_guest)
                .send()
                .await,
        ),
        (
            "不存在的 key",
            http.post(&query_path("abcdefghijkl"), spec)
                .cookie(&c_owner)
                .send()
                .await,
        ),
        (
            "不存在的 id",
            http.post(&query_path(999_999_999u64), spec)
                .cookie(&c_owner)
                .send()
                .await,
        ),
        (
            "不合法的路径段",
            http.post(&query_path("not-a-site"), spec)
                .cookie(&c_owner)
                .send()
                .await,
        ),
        (
            "匿名访问非公开站点",
            http.post(&query_path(site_a.id), spec).send().await,
        ),
        (
            "失效的 cookie",
            http.post(&query_path(site_a.id), spec)
                .cookie("rivu_session=AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA")
                .send()
                .await,
        ),
    ];
    for (what, r) in &denied {
        assert_eq!(r.status, 404, "{what}：{}", r.body);
        assert_eq!(r.body, denied[0].1.body, "{what}");
        assert_eq!(r.json()["error"], "not_found");
    }
    // 实时同样
    for (what, r) in [
        (
            "realtime 无权",
            http.get(&realtime_path(site_b.id))
                .cookie(&c_owner)
                .send()
                .await,
        ),
        (
            "realtime 不存在",
            http.get(&realtime_path(999_999_999u64))
                .cookie(&c_owner)
                .send()
                .await,
        ),
        (
            "realtime 匿名非公开",
            http.get(&realtime_path(site_a.id)).send().await,
        ),
    ] {
        assert_eq!(r.status, 404, "{what}");
        assert_eq!(r.body, denied[0].1.body, "{what}");
    }

    // 公开站点：匿名可读，结果与直接执行相同；非公开（改回去）立刻 404
    let want_pub = direct(&server, &site_pub, spec).await;
    let r = http.post(&query_path(site_pub.id), spec).send().await;
    assert_eq!(
        (r.status, r.body.as_str()),
        (200, want_pub.as_str()),
        "{}",
        r.body
    );
    let r = http.post(&query_path(&site_pub.key), spec).send().await;
    assert_eq!(r.body, want_pub);
    // 登录了但没有权限的人访问公开站点：按公开处理
    let r = http
        .post(&query_path(site_pub.id), spec)
        .cookie(&c_outsider)
        .send()
        .await;
    assert_eq!((r.status, r.body.as_str()), (200, want_pub.as_str()));
    // 公开站点的入口信息
    let info = http
        .get(&format!("/api/internal/public/{}", site_pub.key))
        .send()
        .await;
    assert_eq!(info.status, 200, "{}", info.body);
    assert_eq!(info.json()["site"]["id"], site_pub.id);
    assert_eq!(info.json()["site"]["timezone"], "UTC");
    let nope = http
        .get(&format!("/api/internal/public/{}", site_a.key))
        .send()
        .await;
    assert_eq!((nope.status, &nope.body), (404, &denied[0].1.body));
    sqlx::query("UPDATE sites SET is_public = false WHERE id = $1")
        .bind(site_pub.id as i64)
        .execute(&pg)
        .await
        .unwrap();
    assert_eq!(
        http.post(&query_path(site_pub.id), spec)
            .send()
            .await
            .status,
        404
    );
    assert_eq!(
        http.get(&format!("/api/internal/public/{}", site_pub.key))
            .send()
            .await
            .status,
        404
    );
    sqlx::query("UPDATE sites SET is_public = true WHERE id = $1")
        .bind(site_pub.id as i64)
        .execute(&pg)
        .await
        .unwrap();

    // 站点或组织被删除：404
    sqlx::query("UPDATE sites SET deleted_at = now() WHERE id = $1")
        .bind(site_pub.id as i64)
        .execute(&pg)
        .await
        .unwrap();
    assert_eq!(
        http.post(&query_path(site_pub.id), spec)
            .send()
            .await
            .status,
        404
    );
    sqlx::query("UPDATE sites SET deleted_at = NULL WHERE id = $1")
        .bind(site_pub.id as i64)
        .execute(&pg)
        .await
        .unwrap();

    // 查询错误与公开 API 一致：400 带路径、非法 JSON
    let bad = json!({"metrics": ["visitors"], "date_range": "day", "bogus": 1});
    let r = http
        .post(&query_path(site_a.id), &bad)
        .cookie(&c_owner)
        .send()
        .await;
    assert_eq!(r.status, 400, "{}", r.body);
    assert_eq!(r.json()["error"], "invalid_query");
    assert_eq!(r.json()["errors"][0]["path"], "bogus");
    let r = http
        .rb("POST", &query_path(site_a.id), b"{not json".to_vec())
        .header("Content-Type", "application/json")
        .header("Origin", HTTP_ORIGIN)
        .cookie(&c_owner)
        .send()
        .await;
    assert_eq!(r.status, 400);
    let r = http
        .post(
            &query_path(site_a.id),
            &json!({"metrics": ["visitors"], "date_range": "day", "dimensions": ["event:goal"]}),
        )
        .cookie(&c_owner)
        .send()
        .await;
    assert_eq!(r.status, 400, "不支持的维度是校验错误：{}", r.body);

    // 实时：最近 5 分钟的访客（11:58 两个人在窗口内，11:50 的那个不在）；等价于 date_range=realtime 的 visitors 查询
    let rt_spec = json!({"metrics": ["visitors"], "date_range": "realtime"});
    let want_rt = direct(&server, &site_a, &rt_spec).await;
    for cookie in [&c_owner, &c_viewer, &c_guest] {
        let r = http
            .get(&realtime_path(site_a.id))
            .cookie(cookie)
            .send()
            .await;
        assert_eq!(r.status, 200, "{}", r.body);
        assert_eq!(r.body, want_rt);
        assert_eq!(visitors(&r.json()), 2, "{}", r.body);
        assert_eq!(r.header("cache-control"), Some("no-store"));
    }
    assert_eq!(
        http.post(&query_path(site_a.id), &rt_spec)
            .cookie(&c_owner)
            .send()
            .await
            .body,
        want_rt,
        "POST 查询里的 realtime 同样可用"
    );
    // 公开站点的实时：匿名可读
    assert_eq!(
        visitors(
            &http
                .get(&realtime_path(site_pub.key.as_str()))
                .send()
                .await
                .json()
        ),
        2
    );
    // 方法：realtime 只有 GET，query 只有 POST
    let r = http
        .post(&realtime_path(site_a.id), &json!({}))
        .cookie(&c_owner)
        .send()
        .await;
    assert_eq!((r.status, r.header("allow")), (405, Some("GET, HEAD")));
    let r = http
        .get(&query_path(site_a.id))
        .cookie(&c_owner)
        .send()
        .await;
    assert_eq!((r.status, r.header("allow")), (405, Some("POST")));

    // 站点列表：只含可见的站点，按组织；带有效角色；站点访客的 access 是 guest
    let sites = |c: String| async move { http.get("/api/internal/sites").cookie(&c).send().await };
    let r = sites(c_owner.clone()).await;
    assert_eq!(r.status, 200, "{}", r.body);
    let list = r.json()["sites"].as_array().unwrap().clone();
    let keys: Vec<_> = list
        .iter()
        .map(|s| s["site_key"].as_str().unwrap().to_owned())
        .collect();
    assert!(
        keys.contains(&site_a.key) && keys.contains(&site_pub.key) && !keys.contains(&site_b.key),
        "{keys:?}"
    );
    let entry = list.iter().find(|s| s["site_key"] == site_a.key).unwrap();
    assert_eq!(entry["role"], "owner");
    assert_eq!(entry["access"], "member");
    assert_eq!(entry["timezone"], "UTC");
    assert_eq!(entry["is_public"], false);
    assert_eq!(entry["organization"]["slug"], org_a.slug);
    assert_eq!(entry["id"], site_a.id);
    assert!(
        entry.as_object().unwrap().keys().all(|k| [
            "id",
            "site_key",
            "name",
            "timezone",
            "is_public",
            "organization",
            "role",
            "access"
        ]
        .contains(&k.as_str())),
        "最小化的字段：{entry}"
    );
    let list_viewer = sites(c_viewer.clone()).await.json();
    assert_eq!(
        list_viewer["sites"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["site_key"] == site_a.key)
            .unwrap()["role"],
        "viewer"
    );
    let list_guest = sites(c_guest.clone()).await.json();
    let guest_sites = list_guest["sites"].as_array().unwrap();
    assert_eq!(guest_sites.len(), 1, "{guest_sites:?}");
    assert_eq!(
        (
            guest_sites[0]["role"].as_str(),
            guest_sites[0]["access"].as_str()
        ),
        (Some("viewer"), Some("guest"))
    );
    assert!(
        sites(c_admin.clone()).await.json()["sites"]
            .as_array()
            .unwrap()
            .is_empty(),
        "实例管理员看不到别人的站点"
    );
    // 有效角色取较高者：viewer 同时是站点访客 editor -> editor
    sqlx::query("INSERT INTO site_memberships (site_id, user_id, role) VALUES ($1, $2, 'editor')")
        .bind(site_a.id as i64)
        .bind(viewer.id)
        .execute(&pg)
        .await
        .unwrap();
    let list_viewer = sites(c_viewer.clone()).await.json();
    assert_eq!(
        list_viewer["sites"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["site_key"] == site_a.key)
            .unwrap()["role"],
        "editor"
    );

    // me：用户与组织、角色
    let me = http
        .get("/api/internal/me")
        .cookie(&c_viewer)
        .send()
        .await
        .json();
    assert_eq!(me["user"]["email"], viewer.email);
    assert_eq!(me["user"]["is_instance_admin"], false);
    assert_eq!(me["organizations"].as_array().unwrap().len(), 1);
    assert_eq!(me["organizations"][0]["role"], "viewer");
    assert!(me["user"].get("password_hash").is_none());
    let me_admin = http
        .get("/api/internal/me")
        .cookie(&c_admin)
        .send()
        .await
        .json();
    assert_eq!(me_admin["user"]["is_instance_admin"], true);
    assert!(me_admin["organizations"].as_array().unwrap().is_empty());

    cleanup(&env, &[site_a.id, site_pub.id, site_b.id]).await;
    let _ = (site_b.org_id, org_b.slug);
}

// ================================================================== 共享链接

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn shared_links_passwords_expiry_revocation_scope_and_segments() {
    let Some(env) = test_env("shared_links") else {
        return;
    };
    migrate_once(&env).await;
    let pg = rivu_storage_pg::connect(&env.pg_url, 6).await.unwrap();
    let server = start(&env, &pg, &config(&env, &[]), loose()).await;
    let http = &server.http;

    let org = make_org(&pg).await;
    let member = make_user(&pg, Some((&org, "owner"))).await;
    let site = make_site(&pg, &org, false).await;
    let other_site = make_site(&pg, &org, false).await;
    ingest(&env, &pg, &[&site, &other_site], &server.geo).await;
    let spec = &specs()[0].1;
    let want = direct(&server, &site, spec).await;
    let slug_of = |l: &user_cmd::IssuedLink| l.slug.clone();
    let share_path = |slug: &str| format!("/api/internal/shared/{slug}");

    // 无密码的链接：入口信息带站点；带头的查询与实时可读；结果与直接执行相同；站点本身不是公开的
    let open = user_cmd::issue_link(&pg, &site.key, None, None)
        .await
        .unwrap();
    assert!(!open.has_password);
    let slug = slug_of(&open);
    assert_eq!(slug.len(), 43);
    let stored: String = sqlx::query_scalar("SELECT slug FROM shared_links WHERE id = $1")
        .bind(open.id)
        .fetch_one(&pg)
        .await
        .unwrap();
    assert_eq!(stored, slug);
    let info = http.get(&share_path(&slug)).send().await;
    assert_eq!(info.status, 200, "{}", info.body);
    let v = info.json();
    assert_eq!(
        (v["password_required"].as_bool(), v["authorized"].as_bool()),
        (Some(false), Some(true))
    );
    assert_eq!(v["site"]["site_key"], site.key);
    let r = http
        .post(&query_path(site.id), spec)
        .share(&slug)
        .send()
        .await;
    assert_eq!(
        (r.status, r.body.as_str()),
        (200, want.as_str()),
        "{}",
        r.body
    );
    assert_eq!(
        visitors(
            &http
                .get(&realtime_path(&site.key))
                .share(&slug)
                .send()
                .await
                .json()
        ),
        2
    );
    // 没有头就是匿名：非公开站点 404
    assert_eq!(
        http.post(&query_path(site.id), spec).send().await.status,
        404
    );

    // 链接只对它的站点有效；别的站点、未知 slug、格式不对的 slug：同一个 404
    let not_found = http.post(&query_path(site.id), spec).send().await;
    for (what, r) in [
        (
            "别的站点",
            http.post(&query_path(other_site.id), spec)
                .share(&slug)
                .send()
                .await,
        ),
        (
            "别的站点（key）",
            http.post(&query_path(&other_site.key), spec)
                .share(&slug)
                .send()
                .await,
        ),
        (
            "未知 slug",
            http.post(&query_path(site.id), spec)
                .share(&"a".repeat(43))
                .send()
                .await,
        ),
        (
            "格式不对的 slug",
            http.post(&query_path(site.id), spec)
                .share("short")
                .send()
                .await,
        ),
        (
            "空 slug",
            http.post(&query_path(site.id), spec).share("").send().await,
        ),
        (
            "入口：未知",
            http.get(&share_path(&"b".repeat(43))).send().await,
        ),
        ("入口：格式不对", http.get(&share_path("x")).send().await),
    ] {
        assert_eq!(r.status, 404, "{what}：{}", r.body);
        assert_eq!(r.body, not_found.body, "{what}");
    }

    // 共享链接不能访问 me / sites / sessions / 公开入口 / 删会话，即使带着头
    for path in [
        "/api/internal/me",
        "/api/internal/sites",
        "/api/internal/sessions",
    ] {
        let r = http.get(path).share(&slug).send().await;
        assert_eq!(r.status, 401, "{path}");
    }
    assert_eq!(
        http.get(&format!("/api/internal/public/{}", site.key))
            .share(&slug)
            .send()
            .await
            .status,
        404,
        "站点不是公开的"
    );

    // 有密码的链接
    let locked = user_cmd::issue_link(&pg, &site.key, Some("link password 123"), None)
        .await
        .unwrap();
    assert!(locked.has_password);
    let lslug = slug_of(&locked);
    let hash: String = sqlx::query_scalar("SELECT password_hash FROM shared_links WHERE id = $1")
        .bind(locked.id)
        .fetch_one(&pg)
        .await
        .unwrap();
    assert!(hash.starts_with("$argon2id$"), "{hash}");
    let info = http.get(&share_path(&lslug)).send().await.json();
    assert_eq!(
        (
            info["password_required"].as_bool(),
            info["authorized"].as_bool()
        ),
        (Some(true), Some(false))
    );
    assert!(info.get("site").is_none(), "没通过密码不泄露站点");
    let r = http
        .post(&query_path(site.id), spec)
        .share(&lslug)
        .send()
        .await;
    assert_eq!(
        (r.status, r.json()["error"].as_str()),
        (401, Some("password_required"))
    );
    // 错误的密码：401，不签发凭证
    for pw in ["wrong", "link password 124", ""] {
        let r = http
            .post(
                &format!("{}/auth", share_path(&lslug)),
                &json!({"password": pw}),
            )
            .send()
            .await;
        assert_eq!(r.status, 401, "{pw:?}：{}", r.body);
        assert_eq!(r.json()["error"], "invalid_password");
        assert!(r.all("set-cookie").is_empty());
    }
    let grants: i64 =
        sqlx::query_scalar("SELECT count(*) FROM shared_link_grants WHERE shared_link_id = $1")
            .bind(locked.id)
            .fetch_one(&pg)
            .await
            .unwrap();
    assert_eq!(grants, 0);
    // 正确的密码：签发绑定链接的 12 小时 HttpOnly cookie
    let r = http
        .post(
            &format!("{}/auth", share_path(&lslug)),
            &json!({"password": "link password 123"}),
        )
        .send()
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
    let name = format!("rivu_share_{}", locked.id);
    let set = r.set_cookie(&name).expect("访问凭证 cookie");
    assert!(
        set.ends_with("; Max-Age=43200; Path=/; HttpOnly; SameSite=Lax"),
        "{set}"
    );
    let grant = r.cookie_pair(&name).unwrap();
    let grant_token = grant.strip_prefix(&format!("{name}=")).unwrap().to_owned();
    let stored: Vec<u8> =
        sqlx::query_scalar("SELECT token_hash FROM shared_link_grants WHERE shared_link_id = $1")
            .bind(locked.id)
            .fetch_one(&pg)
            .await
            .unwrap();
    assert_eq!(
        stored,
        Sha256::digest(grant_token.as_bytes()).to_vec(),
        "库里只有摘要"
    );
    assert_eq!(r.json()["site"]["site_key"], site.key);
    // 带凭证：入口显示已授权，查询通过
    let info = http
        .get(&share_path(&lslug))
        .cookie(&grant)
        .send()
        .await
        .json();
    assert_eq!(info["authorized"], true);
    assert_eq!(info["site"]["id"], site.id);
    let r = http
        .post(&query_path(site.id), spec)
        .share(&lslug)
        .cookie(&grant)
        .send()
        .await;
    assert_eq!(
        (r.status, r.body.as_str()),
        (200, want.as_str()),
        "{}",
        r.body
    );
    // 凭证只绑定这个链接：拿给另一个有密码的链接用，不行（换名字也不行）
    let locked2 = user_cmd::issue_link(&pg, &site.key, Some("link password 123"), None)
        .await
        .unwrap();
    let l2slug = slug_of(&locked2);
    let r = http
        .post(&query_path(site.id), spec)
        .share(&l2slug)
        .cookie(&grant)
        .send()
        .await;
    assert_eq!(r.status, 401);
    let renamed = format!("rivu_share_{}={grant_token}", locked2.id);
    let r = http
        .post(&query_path(site.id), spec)
        .share(&l2slug)
        .cookie(&renamed)
        .send()
        .await;
    assert_eq!(r.status, 401, "令牌库里是按 (链接, 摘要) 查的");
    // 凭证只对站点有效：别的站点 404
    assert_eq!(
        http.post(&query_path(other_site.id), spec)
            .share(&lslug)
            .cookie(&grant)
            .send()
            .await
            .status,
        404
    );
    // 凭证不是会话：不能访问 me
    assert_eq!(
        http.get("/api/internal/me")
            .cookie(&grant)
            .share(&lslug)
            .send()
            .await
            .status,
        401
    );
    // 过期的凭证
    sqlx::query("UPDATE shared_link_grants SET expires_at = now() - interval '1 second' WHERE shared_link_id = $1").bind(locked.id).execute(&pg).await.unwrap();
    assert_eq!(
        http.post(&query_path(site.id), spec)
            .share(&lslug)
            .cookie(&grant)
            .send()
            .await
            .status,
        401
    );
    // 无密码的链接调用 auth：直接授权，不签发凭证
    let r = http
        .post(
            &format!("{}/auth", share_path(&slug)),
            &json!({"password": "anything"}),
        )
        .send()
        .await;
    assert_eq!(r.status, 200);
    assert!(r.all("set-cookie").is_empty());

    // 吊销：立即 404，凭证一并删除
    let r = http
        .post(
            &format!("{}/auth", share_path(&lslug)),
            &json!({"password": "link password 123"}),
        )
        .send()
        .await;
    let grant = r.cookie_pair(&name).unwrap();
    assert_eq!(
        http.post(&query_path(site.id), spec)
            .share(&lslug)
            .cookie(&grant)
            .send()
            .await
            .status,
        200
    );
    assert!(
        rivu_storage_pg::shared_links::revoke(&pg, locked.id)
            .await
            .unwrap()
    );
    assert!(
        !rivu_storage_pg::shared_links::revoke(&pg, locked.id)
            .await
            .unwrap(),
        "重复吊销返回 false"
    );
    assert_eq!(
        http.post(&query_path(site.id), spec)
            .share(&lslug)
            .cookie(&grant)
            .send()
            .await
            .status,
        404
    );
    assert_eq!(http.get(&share_path(&lslug)).send().await.status, 404);
    assert_eq!(
        http.post(
            &format!("{}/auth", share_path(&lslug)),
            &json!({"password": "link password 123"})
        )
        .send()
        .await
        .status,
        404
    );
    let grants: i64 =
        sqlx::query_scalar("SELECT count(*) FROM shared_link_grants WHERE shared_link_id = $1")
            .bind(locked.id)
            .fetch_one(&pg)
            .await
            .unwrap();
    assert_eq!(grants, 0);

    // 过期：404（用 --expires 建，再把过期时间改到过去）
    let expiring = user_cmd::issue_link(&pg, &site.key, None, Some("7d"))
        .await
        .unwrap();
    assert!(expiring.expires_at.is_some());
    let eslug = slug_of(&expiring);
    assert_eq!(
        http.post(&query_path(site.id), spec)
            .share(&eslug)
            .send()
            .await
            .status,
        200
    );
    sqlx::query("UPDATE shared_links SET expires_at = now() - interval '1 second' WHERE id = $1")
        .bind(expiring.id)
        .execute(&pg)
        .await
        .unwrap();
    assert_eq!(
        http.post(&query_path(site.id), spec)
            .share(&eslug)
            .send()
            .await
            .status,
        404
    );
    assert_eq!(http.get(&share_path(&eslug)).send().await.status, 404);

    // 站点被删除或组织被删除：链接失效
    let l3 = user_cmd::issue_link(&pg, &other_site.key, None, None)
        .await
        .unwrap();
    let s3 = slug_of(&l3);
    assert_eq!(
        http.post(&query_path(other_site.id), spec)
            .share(&s3)
            .send()
            .await
            .status,
        200
    );
    sqlx::query("UPDATE sites SET deleted_at = now() WHERE id = $1")
        .bind(other_site.id as i64)
        .execute(&pg)
        .await
        .unwrap();
    assert_eq!(
        http.post(&query_path(other_site.id), spec)
            .share(&s3)
            .send()
            .await
            .status,
        404
    );
    sqlx::query("UPDATE sites SET deleted_at = NULL WHERE id = $1")
        .bind(other_site.id as i64)
        .execute(&pg)
        .await
        .unwrap();

    // 绑定 segment：强制叠加它的过滤（瑞典的访客），与「请求自己带同样的过滤」逐字节相同，且与不带过滤的结果不同
    let seg_id: i64 = sqlx::query_scalar("INSERT INTO segments (site_id, name, filter) VALUES ($1, 'sweden', $2::text::jsonb) RETURNING id")
        .bind(site.id as i64)
        .bind(r#"["is","visit:country",["SE"]]"#)
        .fetch_one(&pg)
        .await
        .unwrap();
    let seg_link = user_cmd::issue_link(&pg, &site.key, None, None)
        .await
        .unwrap();
    sqlx::query("UPDATE shared_links SET segment_id = $1 WHERE id = $2")
        .bind(seg_id)
        .bind(seg_link.id)
        .execute(&pg)
        .await
        .unwrap();
    let seg_slug = slug_of(&seg_link);
    let mut filtered = spec.clone();
    filtered["filters"] = json!([["is", "visit:country", ["SE"]]]);
    let want_filtered = direct(&server, &site, &filtered).await;
    assert_ne!(want_filtered, want, "测试数据里瑞典访客只是一部分");
    let r = http
        .post(&query_path(site.id), spec)
        .share(&seg_slug)
        .send()
        .await;
    assert_eq!(
        (r.status, r.body.as_str()),
        (200, want_filtered.as_str()),
        "{}",
        r.body
    );
    // 请求自己的过滤也在，segment 的过滤叠加在后面（隐式 AND）：请求要英国，叠加瑞典，结果为空
    let mut contradictory = spec.clone();
    contradictory["filters"] = json!([["is", "visit:country", ["GB"]]]);
    let r = http
        .post(&query_path(site.id), &contradictory)
        .share(&seg_slug)
        .send()
        .await;
    assert_eq!(r.status, 200);
    let rows = r.json()["results"].as_array().unwrap().clone();
    assert!(rows.is_empty() || rows[0]["metrics"][0] == 0, "{}", r.body);
    // 实时也受约束：窗口内的两个访客，瑞典的只有一个（person 0）
    let rt = http
        .get(&realtime_path(site.id))
        .share(&seg_slug)
        .send()
        .await
        .json();
    assert_eq!(visitors(&rt), 1, "{rt}");
    // 登录的成员带着链接头：按链接（segment 约束仍然生效），文档 §4.2 的顺序
    let c_member = login_cookie(http, &member.email).await;
    let r = http
        .post(&query_path(site.id), spec)
        .share(&seg_slug)
        .cookie(&c_member)
        .send()
        .await;
    assert_eq!(
        r.body, want_filtered,
        "带链接头的请求按链接判定，即使同时登录了"
    );
    // 带无效链接头的成员：404，不退回去用会话（否则同一个链接 URL 会因调用者是否登录而得到不同的响应）
    let r = http
        .post(&query_path(site.id), spec)
        .share(&"c".repeat(43))
        .cookie(&c_member)
        .send()
        .await;
    assert_eq!(r.status, 404);
    // 不带头的成员：会话，完整结果
    assert_eq!(
        http.post(&query_path(site.id), spec)
            .cookie(&c_member)
            .send()
            .await
            .body,
        want
    );
    // segment 被删除：链接 fail-closed，404，而不是把未过滤的数据放出去
    sqlx::query("UPDATE segments SET deleted_at = now() WHERE id = $1")
        .bind(seg_id)
        .execute(&pg)
        .await
        .unwrap();
    assert_eq!(
        http.post(&query_path(site.id), spec)
            .share(&seg_slug)
            .send()
            .await
            .status,
        404
    );
    // segment 的过滤读不懂：500，同样不放行
    sqlx::query(
        "UPDATE segments SET deleted_at = NULL, filter = '[\"nope\"]'::jsonb WHERE id = $1",
    )
    .bind(seg_id)
    .execute(&pg)
    .await
    .unwrap();
    assert_eq!(
        http.post(&query_path(site.id), spec)
            .share(&seg_slug)
            .send()
            .await
            .status,
        500
    );

    // 链接管理（CLI 背后的存储函数）：列表含吊销标记
    let rows = rivu_storage_pg::shared_links::list(&pg, &site.key)
        .await
        .unwrap();
    assert!(
        rows.iter()
            .any(|r| r.id == locked.id && r.revoked && r.has_password)
    );
    assert!(
        rows.iter()
            .any(|r| r.id == open.id && !r.revoked && !r.has_password)
    );
    assert!(
        user_cmd::issue_link(&pg, "abcdefghijkl", None, None)
            .await
            .is_err(),
        "站点不存在"
    );
    assert!(
        user_cmd::issue_link(&pg, &site.key, Some("short"), None)
            .await
            .is_err(),
        "密码太短"
    );
    assert!(
        user_cmd::issue_link(&pg, &site.key, None, Some("yesterday"))
            .await
            .is_err()
    );
    cleanup(&env, &[site.id, other_site.id]).await;
}

/// 反复发请求直到出现 429（至多 20 次）；返回那个 429 响应。
async fn until_429<F, Fut>(mut send: F) -> Resp
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Resp>,
{
    for _ in 0..20 {
        let r = send().await;
        if r.status == 429 {
            return r;
        }
    }
    panic!("连续 20 次请求都没有被限流");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn public_and_shared_access_is_rate_limited() {
    let Some(env) = test_env("public_rate_limit") else {
        return;
    };
    migrate_once(&env).await;
    let pg = rivu_storage_pg::connect(&env.pg_url, 6).await.unwrap();
    let server = start(&env, &pg, &config(&env, &[]), DashLimits::default()).await;
    let http = &server.http;
    let org = make_org(&pg).await;
    let member = make_user(&pg, Some((&org, "owner"))).await;
    let public = make_site(&pg, &org, true).await;
    let private = make_site(&pg, &org, false).await;
    let spec = json!({"metrics": ["visitors"], "date_range": "day"});

    // 公开看板：按 (IP, 站点)，突发 60（默认 300 次/分钟），之后 429 + Retry-After；另一个 IP、另一个站点各有自己的桶
    // 突发用只读库的入口信息来耗（快，补充的令牌可以忽略）；入口、查询与实时共用同一个 (IP, 站点) 桶
    let info_path = format!("/api/internal/public/{}", public.key);
    for i in 0..60 {
        let r = http.get(&info_path).ip("192.0.2.10").send().await;
        assert_eq!(r.status, 200, "第 {i} 次：{}", r.body);
    }
    // 满 60 个令牌耗尽后，请求的间隔内可能补回 1 个令牌（每 0.2 秒一个）：在前几个请求内必须出现 429
    let r = until_429(|| {
        http.post(&query_path(public.id), &spec)
            .ip("192.0.2.10")
            .send()
    })
    .await;
    assert_eq!(
        http.get(&realtime_path(public.id))
            .ip("192.0.2.10")
            .send()
            .await
            .status,
        429
    );
    assert_eq!(
        http.get(&info_path).ip("192.0.2.10").send().await.status,
        429
    );
    assert_eq!(r.json()["error"], "rate_limited");
    let retry: u64 = r.header("retry-after").unwrap().parse().unwrap();
    assert!(
        (1..=2).contains(&retry),
        "300 次/分钟 = 每 0.2 秒一个令牌，Retry-After 向上取整为 1 秒：{retry}"
    );
    assert_eq!(
        http.post(&query_path(public.id), &spec)
            .ip("192.0.2.11")
            .send()
            .await
            .status,
        200
    );
    let other_public = make_site(&pg, &org, true).await;
    assert_eq!(
        http.post(&query_path(other_public.id), &spec)
            .ip("192.0.2.10")
            .send()
            .await
            .status,
        200
    );
    // 非公开站点的 404 不消耗也不受公开桶影响（同一个 IP）
    assert_eq!(
        http.post(&query_path(private.id), &spec)
            .ip("192.0.2.10")
            .send()
            .await
            .status,
        404
    );
    // 登录的成员不走公开桶（按会话判定，看板档不限流）
    let cookie = login_cookie(http, &member.email).await;
    for _ in 0..12 {
        assert_eq!(
            http.post(&query_path(public.id), &spec)
                .ip("192.0.2.10")
                .cookie(&cookie)
                .send()
                .await
                .status,
            200
        );
    }

    // 共享链接：按 (IP, 链接)
    let link = user_cmd::issue_link(&pg, &private.key, None, None)
        .await
        .unwrap();
    let link2 = user_cmd::issue_link(&pg, &private.key, None, None)
        .await
        .unwrap();
    let link_info = format!("/api/internal/shared/{}", link.slug);
    for i in 0..60 {
        let r = http.get(&link_info).ip("192.0.2.20").send().await;
        assert_eq!(r.status, 200, "第 {i} 次：{}", r.body);
    }
    let r = until_429(|| {
        http.post(&query_path(private.id), &spec)
            .share(&link.slug)
            .ip("192.0.2.20")
            .send()
    })
    .await;
    assert!(r.header("retry-after").is_some());
    assert_eq!(
        http.post(&query_path(private.id), &spec)
            .share(&link.slug)
            .ip("192.0.2.21")
            .send()
            .await
            .status,
        200
    );
    assert_eq!(
        http.post(&query_path(private.id), &spec)
            .share(&link2.slug)
            .ip("192.0.2.20")
            .send()
            .await
            .status,
        200,
        "另一个链接有自己的桶"
    );
    // 乱猜 slug 也要消耗配额（限流在查库之前）
    let guess = "g".repeat(43);
    for _ in 0..60 {
        assert_eq!(
            http.get(&format!("/api/internal/shared/{guess}"))
                .ip("192.0.2.30")
                .send()
                .await
                .status,
            404
        );
    }
    until_429(|| {
        http.get(&format!("/api/internal/shared/{guess}"))
            .ip("192.0.2.30")
            .send()
    })
    .await;

    // 共享链接的密码尝试：5 次 / 分钟（按 IP 加链接），超限后连正确的密码也 429
    let locked = user_cmd::issue_link(&pg, &private.key, Some("link password 123"), None)
        .await
        .unwrap();
    let auth = |pw: &'static str, ip: &'static str| {
        let path = format!("/api/internal/shared/{}/auth", locked.slug);
        async move {
            http.post(&path, &json!({"password": pw}))
                .ip(ip)
                .send()
                .await
        }
    };
    for i in 0..5 {
        assert_eq!(auth("wrong", "192.0.2.40").await.status, 401, "第 {i} 次");
    }
    let r = auth("link password 123", "192.0.2.40").await;
    assert_eq!(r.status, 429, "{}", r.body);
    assert!(r.all("set-cookie").is_empty());
    assert_eq!(
        auth("link password 123", "192.0.2.41").await.status,
        200,
        "另一个 IP 不受影响"
    );
    cleanup(&env, &[public.id, private.id, other_public.id]).await;
}

// ================================================================== cookie、日志、指标、时序

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cookie_attributes_in_https_mode() {
    let Some(env) = test_env("https_cookies") else {
        return;
    };
    migrate_once(&env).await;
    let pg = rivu_storage_pg::connect(&env.pg_url, 4).await.unwrap();
    let origin = "https://stats.example.test";
    let server = start(
        &env,
        &pg,
        &config(&env, &[("RIVU_BASE_URL", origin)]),
        loose(),
    )
    .await;
    let http = &server.http;
    assert_eq!(http.origin, origin);
    let org = make_org(&pg).await;
    let user = make_user(&pg, Some((&org, "owner"))).await;
    let site = make_site(&pg, &org, false).await;

    // http 的 Origin 在 https 的部署里被拒绝
    let r = http
        .post(
            "/api/internal/login",
            &json!({"email": user.email, "password": PASSWORD}),
        )
        .header("Origin", HTTP_ORIGIN)
        .send()
        .await;
    assert_eq!(r.status, 403);

    let r = login(http, &user.email, PASSWORD, "203.0.113.1").await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert!(
        r.set_cookie("rivu_session").is_none(),
        "https 模式用 __Host- 前缀的名字"
    );
    let set = r
        .set_cookie("__Host-rivu_session")
        .expect("会话 cookie")
        .to_owned();
    let (pair, attrs) = set.split_once("; ").unwrap();
    assert_eq!(
        attrs, "Max-Age=1209600; Path=/; HttpOnly; SameSite=Lax; Secure",
        "{set}"
    );
    assert!(!set.contains("Domain"));
    let cookie = pair.to_owned();
    assert_eq!(
        http.get("/api/internal/me")
            .cookie(&cookie)
            .send()
            .await
            .status,
        200
    );
    // 不带前缀的同值 cookie 不被认
    let plain = cookie.replace("__Host-", "");
    assert_eq!(
        http.get("/api/internal/me")
            .cookie(&plain)
            .send()
            .await
            .status,
        401
    );
    // 登出清 cookie：属性与签发时一致（带 Secure），浏览器才会接受对 __Host- cookie 的删除
    let r = http
        .post("/api/internal/logout", &json!({}))
        .cookie(&cookie)
        .send()
        .await;
    let cleared = r.set_cookie("__Host-rivu_session").unwrap();
    assert_eq!(
        cleared,
        "__Host-rivu_session=; Max-Age=0; Expires=Thu, 01 Jan 1970 00:00:00 GMT; Path=/; HttpOnly; SameSite=Lax; Secure"
    );

    // 共享链接的访问凭证 cookie 同样
    let link = user_cmd::issue_link(&pg, &site.key, Some("link password 123"), None)
        .await
        .unwrap();
    let r = http
        .post(
            &format!("/api/internal/shared/{}/auth", link.slug),
            &json!({"password": "link password 123"}),
        )
        .send()
        .await;
    let name = format!("__Host-rivu_share_{}", link.id);
    let set = r.set_cookie(&name).expect("访问凭证");
    assert!(
        set.ends_with("; Max-Age=43200; Path=/; HttpOnly; SameSite=Lax; Secure"),
        "{set}"
    );

    // RIVU_COOKIE_SECURE=off 在 https 地址下也能关（反向代理之后的特殊部署）：不带 Secure 与前缀
    let server = start(
        &env,
        &pg,
        &config(
            &env,
            &[("RIVU_BASE_URL", origin), ("RIVU_COOKIE_SECURE", "off")],
        ),
        loose(),
    )
    .await;
    let r = login(&server.http, &user.email, PASSWORD, "203.0.113.1").await;
    assert!(
        r.set_cookie("rivu_session")
            .is_some_and(|c| !c.contains("Secure")),
        "{:?}",
        r.headers
    );
}

/// 把 tracing 输出写进内存。
#[derive(Clone, Default)]
struct LogBuf(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for LogBuf {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for LogBuf {
    type Writer = LogBuf;

    fn make_writer(&'a self) -> LogBuf {
        self.clone()
    }
}

/// 全进程唯一的日志订阅者（TRACE，含 sqlx 与 hyper 的），写进内存。每个起服务的用例都会先调用它，
/// 所以任何用例里产生的日志都在里面。
fn init_logging() -> LogBuf {
    static LOGS: std::sync::OnceLock<LogBuf> = std::sync::OnceLock::new();
    LOGS.get_or_init(|| {
        let buf = LogBuf::default();
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::TRACE)
            .with_ansi(false)
            .with_writer(buf.clone())
            .finish();
        tracing::subscriber::set_global_default(subscriber).expect("全局订阅者只设置一次");
        buf
    })
    .clone()
}

fn sample(text: &str, series: &str) -> Option<f64> {
    text.lines()
        .find_map(|l| l.strip_prefix(series)?.trim().parse().ok())
}

#[tokio::test(flavor = "current_thread")]
async fn secrets_never_reach_the_logs_and_metrics_are_recorded() {
    let Some(env) = test_env("logs_and_metrics") else {
        return;
    };
    migrate_once(&env).await;
    let iso = isolated(&env).await;

    // 日志：整个测试进程共用一个全局订阅者（见 `init_logging`；线程级订阅者会被别的线程先登记的 callsite 缓存挡掉）。
    // 下面的秘密值都是本用例独有的，所以「全进程的日志里都没有它们」是更强的断言。
    let buf = init_logging();
    // 指标 recorder：current_thread 运行时里服务端任务与测试同线程，指标都落进它
    let recorder = metrics_exporter_prometheus::PrometheusBuilder::new().build_recorder();
    let metrics = recorder.handle();
    let _metrics_guard = metrics::set_default_local_recorder(&recorder);

    let server = start(
        &env,
        &iso.pool,
        &config(&env, &[("RIVU_SETUP_TOKEN", SETUP_TOKEN)]),
        loose(),
    )
    .await;
    let http = &server.http;
    let pw_setup = "Sup3r-secret-setup-password";
    let pw_wrong = "Wr0ng-guess-password-123";
    let pw_link = "L1nk-secret-password-456";

    // 流程：初始化、错误与正确的登录、建站与共享链接、错误与正确的链接密码、用凭证查询、登出
    let r = http
        .post(
            "/api/internal/setup",
            &setup_body("admin@example.test", pw_setup, Some(SETUP_TOKEN)),
        )
        .send()
        .await;
    assert_eq!(r.status, 201, "{}", r.body);
    let setup_cookie = r.cookie_pair("rivu_session").unwrap();
    let r = login(http, "admin@example.test", pw_wrong, "203.0.113.5").await;
    assert_eq!(r.status, 401);
    let r = login(http, "ghost@example.test", pw_wrong, "203.0.113.5").await;
    assert_eq!(r.status, 401);
    let r = login(http, "admin@example.test", pw_setup, "203.0.113.5").await;
    let login_cookie = r.cookie_pair("rivu_session").unwrap();
    let org_id: i64 = sqlx::query_scalar("SELECT id FROM organizations")
        .fetch_one(&iso.pool)
        .await
        .unwrap();
    let hostnames = vec![HOST.to_owned()];
    let created = rivu_storage_pg::sites::create_site(
        &iso.pool,
        &NewSite {
            name: "logs",
            timezone: "UTC",
            hostnames: &hostnames,
        },
    )
    .await
    .unwrap();
    sqlx::query("UPDATE sites SET organization_id = $1 WHERE id = $2")
        .bind(org_id)
        .bind(created.id as i64)
        .execute(&iso.pool)
        .await
        .unwrap();
    let link = user_cmd::issue_link(&iso.pool, &created.site_key, Some(pw_link), None)
        .await
        .unwrap();
    let auth_path = format!("/api/internal/shared/{}/auth", link.slug);
    assert_eq!(
        http.post(&auth_path, &json!({"password": pw_wrong}))
            .send()
            .await
            .status,
        401
    );
    let r = http
        .post(&auth_path, &json!({"password": pw_link}))
        .send()
        .await;
    assert_eq!(r.status, 200);
    let grant_cookie = r.cookie_pair(&format!("rivu_share_{}", link.id)).unwrap();
    let spec = json!({"metrics": ["visitors"], "date_range": "day"});
    let r = http
        .post(&query_path(created.id), &spec)
        .share(&link.slug)
        .cookie(&grant_cookie)
        .send()
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
    let r = http
        .post(&query_path(created.id), &spec)
        .cookie(&login_cookie)
        .send()
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
    let _ = http
        .post("/api/internal/logout", &json!({}))
        .cookie(&login_cookie)
        .send()
        .await;
    let _ = http
        .post(
            "/api/internal/setup",
            &setup_body("x@example.test", pw_setup, Some(SETUP_TOKEN)),
        )
        .send()
        .await;

    // 日志里没有任何敏感值：密码、令牌、cookie 值、setup 令牌、共享链接 slug 与密码哈希
    let logs = String::from_utf8_lossy(&buf.0.lock().unwrap()).into_owned();
    assert!(
        logs.contains("首次初始化完成") && logs.contains("登录成功"),
        "确实抓到了日志（{} 字节）：{logs}",
        logs.len()
    );
    let token_of = |c: &str| c.split_once('=').unwrap().1.to_owned();
    let hash: String = sqlx::query_scalar("SELECT password_hash FROM users")
        .fetch_one(&iso.pool)
        .await
        .unwrap();
    for (what, secret) in [
        ("setup 的密码", pw_setup.to_owned()),
        ("错误的密码", pw_wrong.to_owned()),
        ("链接密码", pw_link.to_owned()),
        ("setup 令牌", SETUP_TOKEN.to_owned()),
        ("setup 会话令牌", token_of(&setup_cookie)),
        ("登录会话令牌", token_of(&login_cookie)),
        ("链接访问凭证", token_of(&grant_cookie)),
        ("共享链接 slug", link.slug.clone()),
        ("密码哈希", hash[..40].to_owned()),
        ("cookie 头", "rivu_session=".to_owned()),
        ("Set-Cookie 的值", "Max-Age=1209600".to_owned()),
        ("邮箱", "admin@example.test".to_owned()),
        ("客户端 IP", "203.0.113.5".to_owned()),
    ] {
        assert!(
            !logs.contains(&secret),
            "日志里出现了{what}：{:?}",
            logs.lines()
                .filter(|l| l.contains(&secret))
                .take(3)
                .collect::<Vec<_>>()
        );
    }
    assert!(
        !logs.to_lowercase().contains("authorization"),
        "也没有 Authorization 头"
    );

    // 指标：登录成功/失败计数、内部查询耗时（route 区分）、被拒绝的限流不在这里
    // 活跃会话数：维护任务启动时立即刷新一次（之后每分钟），并清理已撤销的会话行（已登出的那个）
    assert_eq!(
        iso.count("auth_sessions").await,
        2,
        "setup 与登录各一个会话行（登录的那个已登出、已撤销）"
    );
    let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
    let maintenance = dash_api::spawn_maintenance(iso.pool.clone(), stop_rx);
    tokio::time::sleep(Duration::from_millis(500)).await;
    stop_tx.send(true).unwrap();
    maintenance.await.unwrap();
    let active: i64 = 1; // setup 的会话；登录的那个已登出，`/setup` 再次调用没有建会话
    assert_eq!(
        iso.count("auth_sessions").await,
        active,
        "已撤销与过期的行被清理"
    );
    let text = metrics.render();
    assert_eq!(
        sample(&text, "rivu_login_total{result=\"success\"}"),
        Some(1.0),
        "{text}"
    );
    assert_eq!(
        sample(&text, "rivu_login_total{result=\"failure\"}"),
        Some(2.0),
        "{text}"
    );
    let seconds_for = |route: &str| {
        text.lines().any(|l| {
            l.starts_with("rivu_query_seconds") && l.contains(&format!("route=\"{route}\""))
        })
    };
    assert!(
        seconds_for("shared.query") && seconds_for("internal.query"),
        "{text}"
    );
    let requests = |route: &str, status: u16| -> Option<f64> {
        text.lines()
            .filter(|l| {
                l.starts_with("rivu_api_requests_total")
                    && l.contains(&format!("route=\"{route}\""))
                    && l.contains(&format!("status=\"{status}\""))
            })
            .find_map(|l| l.rsplit(' ').next()?.trim().parse().ok())
    };
    assert_eq!(
        sample(&text, "rivu_auth_active_sessions"),
        Some(active as f64),
        "{text}"
    );
    assert_eq!(requests("internal.login", 401), Some(2.0), "{text}");
    assert_eq!(requests("internal.login", 200), Some(1.0), "{text}");
    assert_eq!(requests("internal.setup", 201), Some(1.0), "{text}");
    assert_eq!(requests("internal.setup", 404), Some(1.0), "{text}");
    iso.drop().await;
}

fn median(v: &mut [f64]) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[v.len() / 2]
}

/// 统计方式粗测：不存在的账号、错误的密码、被禁用的账号，登录耗时同量级（都做一次 argon2 校验）。
/// 打印中位数、均值与比值（`--nocapture` 可见）；断言只卡「同量级」，不卡微小的差别。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unknown_accounts_cost_as_much_as_wrong_passwords() {
    let Some(env) = test_env("login_timing") else {
        return;
    };
    migrate_once(&env).await;
    let pg = rivu_storage_pg::connect(&env.pg_url, 4).await.unwrap();
    let server = start(&env, &pg, &config(&env, &[]), loose()).await;
    let http = &server.http;
    let org = make_org(&pg).await;
    let user = make_user(&pg, Some((&org, "owner"))).await;
    let disabled = make_user(&pg, Some((&org, "viewer"))).await;
    rivu_storage_pg::users::set_disabled(&pg, &disabled.email, true)
        .await
        .unwrap();

    let timed = |email: String| async move {
        let t = Instant::now();
        let r = login(http, &email, "wrong password here", "203.0.113.1").await;
        assert_eq!(r.status, 401);
        t.elapsed().as_secs_f64() * 1000.0
    };
    // 预热（连接池、argon2 的首次内存分配）
    for _ in 0..3 {
        timed(user.email.clone()).await;
        timed("warm-up@example.test".into()).await;
    }
    let (mut wrong, mut unknown, mut off) = (Vec::new(), Vec::new(), Vec::new());
    for i in 0..30 {
        // 交替进行，让系统噪声同时落在三组上
        wrong.push(timed(user.email.clone()).await);
        unknown.push(timed(format!("nobody-{i}@example.test")).await);
        off.push(timed(disabled.email.clone()).await);
    }
    let mean = |v: &[f64]| v.iter().sum::<f64>() / v.len() as f64;
    let (mw, mu, mo) = (mean(&wrong), mean(&unknown), mean(&off));
    let (dw, du, dd) = (median(&mut wrong), median(&mut unknown), median(&mut off));
    println!(
        "登录耗时（毫秒，30 次交替）：错误密码 中位 {dw:.1} 均值 {mw:.1}；不存在的账号 中位 {du:.1} 均值 {mu:.1}；被禁用 中位 {dd:.1} 均值 {mo:.1}；\
         中位数比 不存在/错误密码 = {:.3}，被禁用/错误密码 = {:.3}",
        du / dw,
        dd / dw
    );
    assert!(dw > 5.0, "确实做了 argon2 运算：{dw:.1} ms");
    for (name, d) in [("不存在的账号", du), ("被禁用的账号", dd)] {
        let ratio = d / dw;
        assert!(
            (0.7..1.4).contains(&ratio),
            "{name}与错误密码的中位耗时应当同量级：{d:.1} ms 对 {dw:.1} ms"
        );
    }
}
