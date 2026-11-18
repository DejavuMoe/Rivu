//! 看板的内部端点（`/api/internal/*`：首次初始化、登录与会话、站点与查询、共享链接、公开看板）的端到端测试：
//! 真实的 PostgreSQL 与 ClickHouse，真实的本地 TCP 端口，路由由 `public::router_full` 装配（与 `rivu serve` 同一条路径）。
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
//! - `site_admin_*`：站点管理端点（建站、设置、白名单、屏蔽规则、共享链接、软删除；角色、404/403、CSRF）；
//! - `cookie_attributes_in_https_mode`、`secrets_never_reach_the_logs_and_metrics_are_recorded`、
//!   `unknown_accounts_cost_as_much_as_wrong_passwords`（统计方式粗测）。

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use clickhouse::Client;
use jiff::civil::{Date, Weekday, date};
use jiff::tz::TimeZone;
use jiff::{Timestamp, ToSpan};
use rivu::dash_api::{self, DashApi, DashLimits};
use rivu::{migrate, public, user_cmd};
use rivu_auth::password::hash_blocking;
use rivu_common::config::{BufferConfig, ChAccount, Config, Env, Role, Secret};
use rivu_common::migration::{self, Store};
use rivu_geo::GeoDb;
use rivu_ingest::pipeline::{Outcome, Pipeline, RequestCtx, SaltCache, SiteSnapshot};
use rivu_query::rollup;
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

/// 看板端点用的配置。默认 http 开发模式（`RIVU_BASE_URL=http://127.0.0.1:8080`），信任回环代理（测试用 `X-Forwarded-For` 区分客户端）。
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
    /// 响应体原样的字节（二进制响应用，如导出的 zip）。
    bytes: Vec<u8>,
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
            bytes: raw[split..].to_vec(),
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

    /// 带 JSON 请求体的 `PATCH`（同样带 CSRF 要的头）。
    fn patch(&self, path: &str, body: &Value) -> Rb<'_> {
        self.rb("PATCH", path, body.to_string().into_bytes())
            .header("Content-Type", "application/json")
            .header("Origin", &self.origin)
    }

    /// 带 JSON 请求体的 `DELETE`（删站点要 `confirm`）。
    fn delete_with(&self, path: &str, body: &Value) -> Rb<'_> {
        self.rb("DELETE", path, body.to_string().into_bytes())
            .header("Content-Type", "application/json")
            .header("Origin", &self.origin)
    }
}

struct Server {
    http: Http,
    geo: Arc<GeoDb>,
    reader: Client,
}

async fn start(env: &TestEnv, pg: &PgPool, config: &Config, limits: Option<DashLimits>) -> Server {
    start_at(env, pg, config, limits, NOW.parse().unwrap()).await
}

async fn start_at(
    env: &TestEnv,
    pg: &PgPool,
    config: &Config,
    limits: Option<DashLimits>,
    now: Timestamp,
) -> Server {
    init_logging();
    let geo = Arc::new(GeoDb::with_manual_attribution(
        Some("MaxMind test database"),
        Some("https://github.com/maxmind/MaxMind-DB"),
    ));
    let mmdb = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../crates/geo/tests/data/test-data/GeoIP2-City-Test.mmdb");
    assert!(geo.reload_if_changed(&mmdb).expect("加载测试库"));
    let reader = rivu_storage_ch::client(&env.ch_url, &account("reader"), Some(&ch_database()));
    let mut dash = DashApi::new(config, pg.clone(), reader.clone(), Some(Arc::clone(&geo)))
        .expect("装配内部端点")
        .with_clock(move || now);
    if let Some(limits) = limits {
        dash = dash.with_limits(limits);
    }
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
        goals: Vec::new(),
        retention_days: None,
        imported: None,
        completed_import_ids: Vec::new(),
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
        Some(loose()),
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
    let server = start(&env, &iso.pool, &config(&env, &[]), Some(loose())).await;
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
        None,
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
    let server = start(&env, &pg, &config(&env, &[]), Some(loose())).await;
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
        "/api/internal/sites/1/members",
        "/api/internal",
    ] {
        assert_eq!(http.get(path).send().await.status, 404, "{path}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn disabled_or_deleted_users_lose_their_sessions_immediately() {
    let Some(env) = test_env("disabled_users") else {
        return;
    };
    migrate_once(&env).await;
    let pg = rivu_storage_pg::connect(&env.pg_url, 4).await.unwrap();
    let server = start(&env, &pg, &config(&env, &[]), Some(loose())).await;
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
    let server = start(&env, &pg, &config(&env, &[]), Some(loose())).await;
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
    let server = start(&env, &pg, &config(&env, &[]), None).await;
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
    let server = start(&env, &pg, &config(&env, &[]), Some(limits)).await;
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
    let server = start(&env, &pg, &config(&env, &[]), Some(loose())).await;
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

    // 属性键列表：与查询同样的鉴权；这批事件没有属性，返回空列表；别的组织的站点 404
    let keys_path = format!("/api/internal/sites/{}/property-keys", site_a.id);
    let keys_spec = json!({"metrics": ["events"], "date_range": "all"});
    let r = http
        .post(&keys_path, &keys_spec)
        .cookie(&c_viewer)
        .send()
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.json(), json!({"keys": []}));
    assert_eq!(r.header("cache-control"), Some("no-store"));
    let r = http
        .post(
            &format!("/api/internal/sites/{}/property-keys", site_b.id),
            &keys_spec,
        )
        .cookie(&c_owner)
        .send()
        .await;
    assert_eq!(r.status, 404);

    // CSV 导出：editor+（viewer 403），数字与直接查询一致
    let export_path = format!("/api/internal/sites/{}/export", site_a.id);
    let export_body = json!({"date_range": ["2026-03-09", "2026-03-11"]});
    let r = http
        .post(&export_path, &export_body)
        .cookie(&c_viewer)
        .send()
        .await;
    assert_eq!(r.status, 403, "{}", r.body);
    let r = http
        .post(&export_path, &export_body)
        .cookie(&c_outsider)
        .send()
        .await;
    assert_eq!(r.status, 404, "{}", r.body);
    let r = http
        .post(&export_path, &json!({"date_range": "realtime"}))
        .cookie(&c_owner)
        .send()
        .await;
    assert_eq!(r.status, 400, "{}", r.body);
    let r = http
        .post(&export_path, &export_body)
        .cookie(&c_owner)
        .send()
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.header("content-type"), Some("application/zip"));
    assert_eq!(r.header("cache-control"), Some("no-store"));
    let disposition = r.header("content-disposition").unwrap_or_default();
    assert!(
        disposition.starts_with("attachment; filename=\"rivu-")
            && disposition.ends_with("-2026-03-09-2026-03-11.zip\""),
        "{disposition}"
    );
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(r.bytes.clone())).expect("zip");
    let mut read = |name: &str| {
        let mut s = String::new();
        std::io::Read::read_to_string(&mut zip.by_name(name).expect(name), &mut s).unwrap();
        s
    };
    // 趋势：三天都在（没有数据的天记 0），列与请求的指标一致
    let trend = read("visitors.csv");
    let lines: Vec<&str> = trend.lines().collect();
    assert_eq!(
        lines[0],
        "\u{feff}date,visitors,visits,pageviews,views_per_visit,bounce_rate,visit_duration"
    );
    assert_eq!(lines.len(), 4, "{trend}");
    // 国家明细与直接查询相同
    let want: Value = serde_json::from_str(
        &direct(
            &server,
            &site_a,
            &json!({"metrics": ["visitors", "visits", "pageviews", "bounce_rate", "visit_duration"],
                    "dimensions": ["visit:country"], "date_range": ["2026-03-09", "2026-03-11"],
                    "pagination": {"limit": 10000}}),
        )
        .await,
    )
    .unwrap();
    let countries = read("countries.csv");
    let mut got = countries.lines();
    assert_eq!(
        got.next(),
        Some("\u{feff}country,visitors,visits,pageviews,bounce_rate,visit_duration")
    );
    let rows = want["results"].as_array().unwrap();
    assert!(!rows.is_empty());
    for (line, row) in got.zip(rows) {
        let cols: Vec<&str> = line.split(',').collect();
        assert_eq!(cols[0], row["dimensions"][0], "{line}");
        assert_eq!(cols[1], row["metrics"][0].to_string(), "{line}");
        assert_eq!(cols[3], row["metrics"][2].to_string(), "{line}");
    }
    for f in [
        "sources.csv",
        "pages.csv",
        "entry_pages.csv",
        "browser_versions.csv",
        "custom_props.csv",
        "conversions.csv",
    ] {
        assert!(!read(f).is_empty(), "{f}");
    }
    assert_eq!(
        read("custom_props.csv"),
        "\u{feff}property,value,visitors,events\n"
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
    // 目标维度已支持：站点没有目标时没有行
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.json()["results"], json!([]), "{}", r.body);
    let r = http
        .post(
            &query_path(site_a.id),
            &json!({"metrics": ["visitors"], "date_range": "day", "dimensions": ["event:props:plan"]}),
        )
        .cookie(&c_owner)
        .send()
        .await;
    // 属性维度已支持：这批事件没有 plan 属性，全部归为 (none)
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(
        r.json()["results"][0]["dimensions"],
        json!(["(none)"]),
        "{}",
        r.body
    );

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
    let server = start(&env, &pg, &config(&env, &[]), Some(loose())).await;
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn public_and_shared_access_is_rate_limited() {
    let Some(env) = test_env("public_rate_limit") else {
        return;
    };
    migrate_once(&env).await;
    let pg = rivu_storage_pg::connect(&env.pg_url, 6).await.unwrap();
    // 生产装配将默认突发 60 限制为每分钟配额 1：初始只有一个令牌，每分钟补一个。
    let server = start(
        &env,
        &pg,
        &config(&env, &[("RIVU_PUBLIC_VIEW_RATE_PER_MIN", "1")]),
        None,
    )
    .await;
    let http = &server.http;
    let org = make_org(&pg).await;
    let member = make_user(&pg, Some((&org, "owner"))).await;
    let public = make_site(&pg, &org, true).await;
    let private = make_site(&pg, &org, false).await;
    let spec = json!({"metrics": ["visitors"], "date_range": "day"});

    // 公开看板入口、查询与实时共用一个 (IP, 站点) 桶。
    let info_path = format!("/api/internal/public/{}", public.key);
    let r = http.get(&info_path).ip("192.0.2.10").send().await;
    assert_eq!(r.status, 200, "{}", r.body);
    let r = http
        .post(&query_path(public.id), &spec)
        .ip("192.0.2.10")
        .send()
        .await;
    assert_eq!(r.status, 429, "{}", r.body);
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
        (1..=60).contains(&retry),
        "1 次/分钟：Retry-After 是到下一个令牌的秒数（向上取整），不超过 60：{retry}"
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
    let r = http.get(&link_info).ip("192.0.2.20").send().await;
    assert_eq!(r.status, 200, "{}", r.body);
    let r = http
        .post(&query_path(private.id), &spec)
        .share(&link.slug)
        .ip("192.0.2.20")
        .send()
        .await;
    assert_eq!(r.status, 429, "{}", r.body);
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
    let missing = format!("/api/internal/shared/{}", "g".repeat(43));
    for status in [404, 429] {
        let r = http.get(&missing).ip("192.0.2.30").send().await;
        assert_eq!(r.status, status, "{}", r.body);
    }

    // 密码端点也消耗共享访问桶。用默认访问配额，才能单独验证密码桶的 5 次 / 分钟。
    let password_server = start(&env, &pg, &config(&env, &[]), None).await;
    let http = &password_server.http;
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
        Some(loose()),
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
        Some(loose()),
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
        Some(loose()),
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
    let server = start(&env, &pg, &config(&env, &[]), Some(loose())).await;
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

// ------------------------------------------------------------------ 站点管理端点

/// 断言状态码并返回 JSON 体（204 等没有体的返回 `Null`）。
fn expect(r: &Resp, status: u16) -> Value {
    assert_eq!(r.status, status, "响应：{}", r.body);
    if r.body.trim().is_empty() {
        Value::Null
    } else {
        r.json()
    }
}

/// 断言错误响应：状态码、`error` 码；返回 `message`。
fn expect_err(r: &Resp, status: u16, code: &str) -> String {
    let v = expect(r, status);
    assert_eq!(v["error"], code, "响应：{}", r.body);
    v["message"].as_str().unwrap_or_default().to_owned()
}

fn site_path(site: impl std::fmt::Display, tail: &str) -> String {
    format!("/api/internal/sites/{site}{tail}")
}

async fn guest_of(pg: &PgPool, site: &Site, role: &str) -> User {
    let u = make_user(pg, None).await;
    sqlx::query("INSERT INTO site_memberships (site_id, user_id, role) VALUES ($1, $2, $3)")
        .bind(site.id as i64)
        .bind(u.id)
        .bind(role)
        .execute(pg)
        .await
        .unwrap();
    u
}

/// 一个组织、四种组织角色、一个外人（别的组织的 owner）。
struct Cast {
    org: Org,
    owner: String,
    admin: String,
    editor: String,
    viewer: String,
    outsider: String,
}

async fn cast(pg: &PgPool, http: &Http) -> Cast {
    let org = make_org(pg).await;
    let other = make_org(pg).await;
    let mut cookies = Vec::new();
    for role in ["owner", "admin", "editor", "viewer"] {
        let u = make_user(pg, Some((&org, role))).await;
        cookies.push(login_cookie(http, &u.email).await);
    }
    let outsider = make_user(pg, Some((&other, "owner"))).await;
    let mut it = cookies.into_iter();
    Cast {
        org,
        owner: it.next().unwrap(),
        admin: it.next().unwrap(),
        editor: it.next().unwrap(),
        viewer: it.next().unwrap(),
        outsider: login_cookie(http, &outsider.email).await,
    }
}

async fn create_site_via_api(http: &Http, cookie: &str, org_id: i64, body: &Value) -> Resp {
    http.post(&format!("/api/internal/orgs/{org_id}/sites"), body)
        .cookie(cookie)
        .send()
        .await
}

async fn add_rollup_event(ch: &Client, site_id: u64, day: Date) {
    add_rollup_event_at(ch, site_id, at(day, 23, 30, 0)).await;
}

async fn add_rollup_event_at(ch: &Client, site_id: u64, event_ms: i64) {
    ch.query(
        "INSERT INTO events
         (site_id, ts, event_id, name, visitor_id, session_id, session_start,
          hostname, pathname, entry_pathname)
         SELECT ?, fromUnixTimestamp64Milli(?, 'UTC'), generateUUIDv4(), 'pageview', 1, 1,
                fromUnixTimestamp64Milli(?, 'UTC'), 'example.test', '/rollup', '/rollup'",
    )
    .bind(site_id)
    .bind(event_ms)
    .bind(event_ms)
    .execute()
    .await
    .unwrap();
}

async fn rollup_rows(ch: &Client, site_id: u64) -> Vec<u64> {
    let mut counts = Vec::new();
    for table in rollup::Family::TABLES {
        counts.push(
            ch.query(&format!(
                "SELECT count() FROM {table} WHERE site_id = ? AND origin = 'native'"
            ))
            .bind(site_id)
            .fetch_one()
            .await
            .unwrap(),
        );
    }
    counts
}

#[tokio::test]
async fn legacy_range_erase_preserves_raw_after_timezone_change() {
    let Some(env) = test_env("legacy_range_erase_preserves_raw_after_timezone_change") else {
        return;
    };
    migrate_once(&env).await;
    let iso = isolated(&env).await;
    let pg = &iso.pool;
    // ClickHouse is shared with other tests, even when the PG schema is isolated.
    sqlx::query("SELECT setval(pg_get_serial_sequence('sites', 'id'), $1, false)")
        .bind(10_000_000_000_i64 + i64::from(rand::random::<u32>()))
        .execute(pg)
        .await
        .unwrap();
    let now: Timestamp = "2026-11-18T00:00:00Z".parse().unwrap();
    let server = start_at(&env, pg, &config(&env, &[]), Some(loose()), now).await;
    let org = make_org(pg).await;
    let owner = make_user(pg, Some((&org, "owner"))).await;
    let cookie = login_cookie(&server.http, &owner.email).await;
    let site = make_site(pg, &org, false).await;
    let admin = rivu_storage_ch::client(&env.ch_url, &account("migrator"), Some(&ch_database()));
    let ch = rivu_storage_ch::client(&env.ch_url, &account("maintainer"), Some(&ch_database()));
    let before = [
        at(date(2026, 10, 8), 23, 30, 0),
        at(date(2026, 10, 9), 23, 30, 0),
    ];
    for ms in before {
        add_rollup_event_at(&admin, site.id, ms).await;
    }
    let intent: i64 = sqlx::query_scalar(
        "INSERT INTO pending_stats_deletions (site_id, scope, date_from, date_to)
         VALUES ($1, 'date_range', '2026-10-09', '2026-10-09') RETURNING id",
    )
    .bind(site.id as i64)
    .fetch_one(pg)
    .await
    .unwrap();
    let path = site_path(site.id, "/stats-deletions");
    let body = json!({"from": "2026-10-09", "to": "2026-10-09", "confirm": "internal-api-e2e"});
    let queued = server.http.post(&path, &body).cookie(&cookie).send().await;
    expect(&queued, 201);
    let frozen = queued.json()["deletion"]["id"].as_i64().unwrap();
    let bounds: (String, i64, i64) = sqlx::query_as(
        "SELECT timezone, utc_start_ms, utc_end_ms FROM pending_stats_deletions WHERE id = $1",
    )
    .bind(frozen)
    .fetch_one(pg)
    .await
    .unwrap();
    assert_eq!(
        bounds,
        (
            "UTC".into(),
            at(date(2026, 10, 9), 0, 0, 0),
            at(date(2026, 10, 10), 0, 0, 0)
        )
    );
    sqlx::query("UPDATE sites SET timezone = 'Asia/Taipei' WHERE id = $1")
        .bind(site.id as i64)
        .execute(pg)
        .await
        .unwrap();
    let claims = rivu_storage_pg::stats_deletions::claim_due(pg, 100)
        .await
        .unwrap();
    for (id, reason) in [(intent, "缺少入队时"), (frozen, "时区不一致")] {
        let claim = claims.iter().find(|claim| claim.id == id).unwrap();
        assert!(
            rivu_jobs::stats_delete::process(pg, &ch, claim)
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(raw_event_times(&ch, site.id).await, before);
        let (status, checkpoints, error): (String, Vec<String>, String) = sqlx::query_as(
            "SELECT status, partitions_done, last_error FROM pending_stats_deletions WHERE id = $1",
        )
        .bind(id)
        .fetch_one(pg)
        .await
        .unwrap();
        assert_eq!(status, "failed");
        assert!(checkpoints.is_empty());
        assert!(error.contains(reason), "{error}");
    }
    let listed = server.http.get(&path).cookie(&cookie).send().await;
    expect(&listed, 200);
    let listed = listed.json();
    let deletions = listed["deletions"].as_array().unwrap();
    assert_eq!(deletions.len(), 2);
    for item in deletions {
        assert_eq!(item["status"], "failed");
        assert_eq!(item["error_code"], "stats_deletion_timezone_unproven");
    }
    // An operator verified UTC and resubmits via the normal authorized endpoint.
    expect(
        &server
            .http
            .patch(&site_path(site.id, ""), &json!({"timezone": "UTC"}))
            .cookie(&cookie)
            .send()
            .await,
        200,
    );
    let resubmitted = server.http.post(&path, &body).cookie(&cookie).send().await;
    expect(&resubmitted, 201);
    let id = resubmitted.json()["deletion"]["id"].as_i64().unwrap();
    let claims = rivu_storage_pg::stats_deletions::claim_due(pg, 100)
        .await
        .unwrap();
    let claim = claims.iter().find(|claim| claim.id == id).unwrap();
    assert!(
        rivu_jobs::stats_delete::process(pg, &ch, claim)
            .await
            .unwrap()
            .is_some()
    );
    assert_eq!(raw_event_times(&ch, site.id).await, [before[0]]);
    cleanup(&env, &[site.id]).await;
    iso.drop().await;
}

async fn raw_event_times(ch: &Client, site_id: u64) -> Vec<i64> {
    ch.query("SELECT toUnixTimestamp64Milli(ts) FROM events WHERE site_id = ? ORDER BY ts")
        .bind(site_id)
        .fetch_all()
        .await
        .unwrap()
}

// Forward to the real server until the test sees the mutation registered, then
// return a gateway timeout while ClickHouse is still waiting for its merges.
async fn mutation_timeout_proxy(
    env: &TestEnv,
) -> (
    Client,
    tokio::sync::watch::Sender<bool>,
    tokio::task::JoinHandle<()>,
) {
    let url = url::Url::parse(&env.ch_url).unwrap();
    assert_eq!(url.scheme(), "http");
    let upstream = format!(
        "{}:{}",
        url.host_str().unwrap(),
        url.port_or_known_default().unwrap()
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_url = format!("http://{}", listener.local_addr().unwrap());
    let (timeout, rx) = tokio::sync::watch::channel(false);
    let task = tokio::spawn(async move {
        let mut connections = tokio::task::JoinSet::new();
        loop {
            let (mut downstream, _) = listener.accept().await.unwrap();
            let upstream = upstream.clone();
            let mut rx = rx.clone();
            connections.spawn(async move {
                let mut upstream = TcpStream::connect(upstream).await.unwrap();
                tokio::select! {
                    _ = tokio::io::copy_bidirectional(&mut downstream, &mut upstream) => {}
                    _ = async { rx.wait_for(|timed_out| *timed_out).await.unwrap(); } => {
                        let message = "upstream mutation timed out";
                        let reply = format!("HTTP/1.1 504 Gateway Timeout\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{message}", message.len());
                        let _ = downstream.write_all(reply.as_bytes()).await;
                    }
                }
            });
        }
    });
    let client = rivu_storage_ch::client(&proxy_url, &account("maintainer"), Some(&ch_database()));
    (client, timeout, task)
}

async fn pending_rollup_page_deletes(ch: &Client, site_id: u64) -> u64 {
    ch.query(
        "SELECT count() FROM system.mutations WHERE database = currentDatabase()
               AND table = 'rollup_pages' AND NOT is_done AND command LIKE ?",
    )
    .bind(format!("%site_id = _CAST({site_id}, 'UInt64')%"))
    .fetch_one()
    .await
    .unwrap()
}

#[tokio::test]
async fn timed_out_erase_fences_maintenance_until_retry_completes() {
    use rivu_storage_pg::stats_deletions::MAX_ATTEMPTS;

    // This test pauses a whole ClickHouse table and runs retention. Use dedicated databases.
    if var("RIVU_TEST_ERASE_TIMEOUT").as_deref() != Some("1") {
        println!("跳过：需要独立双库及 RIVU_TEST_ERASE_TIMEOUT=1");
        return;
    }
    let env = test_env("timed_out_erase_fences_maintenance_until_retry_completes").unwrap();
    migrate_once(&env).await;
    let pg = rivu_storage_pg::connect(&env.pg_url, 6).await.unwrap();
    let now = Timestamp::now();
    let today = now.to_zoned(TimeZone::UTC).date();
    let old = today.checked_sub(40.days()).unwrap();
    let erased = today.checked_sub(5.days()).unwrap();
    let kept = erased.tomorrow().unwrap();
    let server = start_at(&env, &pg, &config(&env, &[]), Some(loose()), now).await;
    let org = make_org(&pg).await;
    let owner = make_user(&pg, Some((&org, "owner"))).await;
    let cookie = login_cookie(&server.http, &owner.email).await;
    let site = make_site(&pg, &org, false).await;
    let other = make_site(&pg, &org, false).await;
    let db = ch_database();
    let admin = rivu_storage_ch::client(&env.ch_url, &account("migrator"), Some(&db));
    let ch = rivu_storage_ch::client(&env.ch_url, &account("maintainer"), Some(&db));
    let scope = TenantScope::new(site.id);
    for day in [old, erased, kept] {
        add_rollup_event(&admin, site.id, day).await;
    }
    add_rollup_event(&admin, other.id, old).await;
    sqlx::query("UPDATE site_settings SET retention_days = 30 WHERE site_id IN ($1, $2)")
        .bind(site.id as i64)
        .bind(other.id as i64)
        .execute(&pg)
        .await
        .unwrap();
    rollup::build_day(
        &ch,
        &scope,
        &SiteContext::new(site.id, TimeZone::UTC),
        erased,
        1,
        now,
    )
    .await
    .unwrap();
    let date = erased.to_string();
    let queued = server
        .http
        .post(
            &site_path(site.id, "/stats-deletions"),
            &json!({"from": date, "to": date, "confirm": "internal-api-e2e"}),
        )
        .cookie(&cookie)
        .send()
        .await;
    expect(&queued, 201);
    let id = queued.json()["deletion"]["id"].as_i64().unwrap();
    sqlx::query("UPDATE pending_stats_deletions SET attempts = $2 WHERE id = $1")
        .bind(id)
        .bind(MAX_ATTEMPTS - 1)
        .execute(&pg)
        .await
        .unwrap();
    let (proxied, timeout, proxy) = mutation_timeout_proxy(&env).await;
    admin
        .query("SYSTEM STOP MERGES rollup_pages")
        .execute()
        .await
        .unwrap();
    let erase = tokio::spawn({
        let pg = pg.clone();
        async move { rivu_jobs::stats_delete::run_batch(&pg, &proxied).await }
    });
    let site_id = site.id;
    let observed = tokio::time::timeout(Duration::from_secs(15), async {
        while pending_rollup_page_deletes(&ch, site_id).await == 0 {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await;
    timeout.send(true).unwrap();
    let result = tokio::time::timeout(Duration::from_secs(10), erase).await;
    proxy.abort();
    // Join captures panics so the test can resume merges before surfacing a failure.
    let checks = tokio::spawn({
        let pg = pg.clone();
        let ch = ch.clone();
        let db = db.clone();
        async move {
            tokio::time::timeout(Duration::from_secs(20), async {
                observed.expect("ClickHouse never registered the synchronous mutation");
                assert_eq!(result.unwrap().unwrap().unwrap(), 1);
                assert!(pending_rollup_page_deletes(&ch, site_id).await > 0);
                let (status, error, checkpoints): (String, String, Vec<String>) = sqlx::query_as(
                    "SELECT status, last_error, partitions_done FROM pending_stats_deletions WHERE id = $1",
                )
                .bind(id)
                .fetch_one(&pg)
                .await
                .unwrap();
                assert_eq!(status, "failed");
                assert!(error.contains("upstream mutation timed out"), "{error}");
                assert!(checkpoints.iter().any(|key| key.starts_with("events:")));
                sqlx::query("SELECT id FROM sites WHERE id = $1 FOR UPDATE NOWAIT")
                    .bind(site.id as i64)
                    .execute(&pg)
                    .await
                    .unwrap();
                let rollups = rollup_rows(&ch, site.id).await;
                assert!(rollups[0] > 0, "rollup_pages has not been erased yet");
                let built = rivu_jobs::rollup::build_site(&pg, &ch, site.id)
                    .await
                    .unwrap();
                assert_eq!(built, 0);
                assert_eq!(rollup_rows(&ch, site.id).await, rollups);
                expect_err(
                    &server
                        .http
                        .patch(&site_path(site.id, ""), &json!({"timezone": "Asia/Taipei"}))
                        .cookie(&cookie)
                        .send()
                        .await,
                    409,
                    "timezone_rebuild_unproven",
                );
                expect(
                    &server
                        .http
                        .patch(
                            &site_path(site.id, ""),
                            &json!({"timezone": "UTC", "name": "Same Zone"}),
                        )
                        .cookie(&cookie)
                        .send()
                        .await,
                    200,
                );
                rivu_jobs::retention::run_once(&pg, &ch, &db, 731)
                    .await
                    .unwrap();
                assert_eq!(
                    raw_event_times(&ch, site.id).await,
                    [at(old, 23, 30, 0), at(kept, 23, 30, 0)]
                );
                assert!(raw_event_times(&ch, other.id).await.is_empty());
                assert!(pending_rollup_page_deletes(&ch, site_id).await > 0);
            })
            .await
        }
    });
    let checks = checks.await;
    admin
        .query("SYSTEM START MERGES rollup_pages")
        .execute()
        .await
        .unwrap();
    checks.unwrap().unwrap();
    sqlx::query(
        "UPDATE pending_stats_deletions SET status = 'running', not_before = now() WHERE id = $1",
    )
    .bind(id)
    .execute(&pg)
    .await
    .unwrap();
    assert_eq!(
        rivu_jobs::stats_delete::run_batch(&pg, &ch).await.unwrap(),
        1
    );
    let status: String =
        sqlx::query_scalar("SELECT status FROM pending_stats_deletions WHERE id = $1")
            .bind(id)
            .fetch_one(&pg)
            .await
            .unwrap();
    assert_eq!(status, "done");
    assert_eq!(pending_rollup_page_deletes(&ch, site_id).await, 0);
    rivu_jobs::retention::run_once(&pg, &ch, &db, 731)
        .await
        .unwrap();
    assert_eq!(raw_event_times(&ch, site.id).await, [at(kept, 23, 30, 0)]);
    assert!(
        rivu_jobs::rollup::build_site(&pg, &ch, site.id)
            .await
            .unwrap()
            > 0
    );
    assert_eq!(
        rivu_jobs::rollup::build_site(&pg, &ch, site.id)
            .await
            .unwrap(),
        0
    );
    let kept_rollup: Vec<String> = ch
        .query(
            "SELECT toString(day) FROM rollup_site WHERE site_id = ? AND visitors > 0 ORDER BY day",
        )
        .bind(site.id)
        .fetch_all()
        .await
        .unwrap();
    assert_eq!(kept_rollup, [kept.to_string()]);
    rollup::delete(&ch, &scope, None, false).await.unwrap();
    cleanup(&env, &[site.id, other.id]).await;
}

#[tokio::test]
async fn range_erase_preserves_unrebuilt_native_history() {
    use rivu_storage_pg::stats_deletions;

    let Some(env) = test_env("range_erase_preserves_unrebuilt_native_history") else {
        return;
    };
    migrate_once(&env).await;
    let pg = rivu_storage_pg::connect(&env.pg_url, 6).await.unwrap();
    let now: Timestamp = "2026-11-18T00:00:00Z".parse().unwrap();
    let cfg = config(&env, &[]);
    let server = start_at(&env, &pg, &cfg, Some(loose()), now).await;
    let org = make_org(&pg).await;
    let owner = make_user(&pg, Some((&org, "owner"))).await;
    let cookie = login_cookie(&server.http, &owner.email).await;
    let site = make_site(&pg, &org, false).await;
    let admin = rivu_storage_ch::client(&env.ch_url, &account("migrator"), Some(&ch_database()));
    let ch = rivu_storage_ch::client(&env.ch_url, &account("maintainer"), Some(&ch_database()));
    let day: Date = "2026-10-09".parse().unwrap();
    let scope = TenantScope::new(site.id);
    add_rollup_event(&admin, site.id, day).await;
    rollup::build_day(
        &ch,
        &scope,
        &SiteContext::new(site.id, TimeZone::UTC),
        day,
        1,
        now,
    )
    .await
    .unwrap();
    let before = rollup_rows(&server.reader, site.id).await;
    assert!(before[0] > 0 && before[before.len() - 1] > 0);
    sqlx::query("UPDATE sites SET timezone = 'Asia/Taipei' WHERE id = $1")
        .bind(site.id as i64)
        .execute(&pg)
        .await
        .unwrap();

    let response = server
        .http
        .post(
            &site_path(site.id, "/stats-deletions"),
            &json!({"from": day.to_string(), "to": day.to_string(), "confirm": "internal-api-e2e"}),
        )
        .cookie(&cookie)
        .send()
        .await;
    // Supply valid Taipei bounds to reach the worker's independent rollup check.
    let taipei = TimeZone::get("Asia/Taipei").unwrap();
    if response.status == 409 {
        stats_deletions::create_date_range(
            &mut pg.acquire().await.unwrap(),
            site.id,
            &day.to_string(),
            &day.to_string(),
            "Asia/Taipei",
            rivu_query::resolve::local_dates_utc_ms(&taipei, day, day).unwrap(),
            None,
        )
        .await
        .unwrap();
    }
    let claimed = stats_deletions::claim_due(&pg, 100).await.unwrap();
    let intent = claimed.iter().find(|i| i.site_id == site.id).unwrap();
    let result = rivu_jobs::stats_delete::process(&pg, &ch, intent).await;
    assert_eq!(rollup_rows(&server.reader, site.id).await, before);
    expect_err(&response, 409, "stats_deletion_timezone_unproven");
    assert!(result.unwrap().is_none());
    let (partitions, error): (Vec<String>, String) = sqlx::query_as(
        "SELECT partitions_done, last_error FROM pending_stats_deletions WHERE id = $1",
    )
    .bind(intent.id)
    .fetch_one(&pg)
    .await
    .unwrap();
    assert!(partitions.is_empty());
    assert!(error.contains("原生汇总日期的时区"), "{error}");

    ch.clone()
        .with_setting("lightweight_deletes_sync", "2")
        .query("DELETE FROM rollup_site WHERE site_id = ?")
        .bind(site.id)
        .execute()
        .await
        .unwrap();
    assert!(
        !rivu_jobs::stats_delete::can_erase_dates(&server.reader, site.id, &taipei)
            .await
            .unwrap(),
        "orphaned native families have no timezone proof"
    );
    assert!(
        stats_deletions::soft_delete_site(&pg, site.id, None, 0)
            .await
            .unwrap()
    );
    let claimed = stats_deletions::claim_due(&pg, 100).await.unwrap();
    let whole = claimed
        .iter()
        .find(|i| i.site_id == site.id && i.scope == "site")
        .unwrap();
    assert!(
        rivu_jobs::stats_delete::process(&pg, &ch, whole)
            .await
            .unwrap()
            .is_some()
    );
    assert!(
        rollup_rows(&server.reader, site.id)
            .await
            .into_iter()
            .all(|n| n == 0)
    );
    cleanup(&env, &[site.id]).await;
}

#[tokio::test]
async fn timezone_rebuild_rejects_import_overlap() {
    let Some(env) = test_env("timezone_rebuild_rejects_import_overlap") else {
        return;
    };
    migrate_once(&env).await;
    let pg = rivu_storage_pg::connect(&env.pg_url, 6).await.unwrap();
    let now = Timestamp::now();
    let import_day = now
        .to_zoned(TimeZone::UTC)
        .date()
        .checked_sub(3.days())
        .unwrap();
    let native_day = import_day.tomorrow().unwrap();
    let cfg = config(&env, &[("RIVU_ROLLUP_MIN_ROWS", "0")]);
    let server = start_at(&env, &pg, &cfg, Some(loose()), now).await;
    let http = &server.http;
    let org = make_org(&pg).await;
    let owner = make_user(&pg, Some((&org, "owner"))).await;
    let cookie = login_cookie(http, &owner.email).await;
    let site = make_site(&pg, &org, false).await;
    let scope = TenantScope::new(site.id);
    let admin = rivu_storage_ch::client(&env.ch_url, &account("migrator"), Some(&ch_database()));
    let ch = rivu_storage_ch::client(&env.ch_url, &account("maintainer"), Some(&ch_database()));
    add_rollup_event_at(&admin, site.id, at(native_day, 0, 30, 0)).await;
    rollup::build_day(
        &ch,
        &scope,
        &SiteContext::new(site.id, TimeZone::UTC),
        native_day,
        1,
        now,
    )
    .await
    .unwrap();
    let zip = {
        use std::io::Write;
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        zip.start_file(
            format!("imported_visitors_{import_day}_{import_day}.csv"),
            zip::write::SimpleFileOptions::default(),
        )
        .unwrap();
        writeln!(
            zip,
            "date,visitors,pageviews,bounces,visits,visit_duration\n{import_day},1,17,0,1,0"
        )
        .unwrap();
        zip.finish().unwrap().into_inner()
    };
    let (_, summary) =
        rivu_jobs::import_plausible::import_file(&pg, &ch, site.id, zip, "export.zip", None)
            .await
            .unwrap();
    assert_eq!((summary.start, summary.end), (import_day, import_day));
    let pageviews = |day: Date| {
        let day = day.to_string();
        http.post(
            &query_path(site.id),
            &json!({"metrics": ["pageviews"], "date_range": [day, day]}),
        )
        .cookie(&cookie)
        .send()
    };
    assert_eq!(
        expect(&pageviews(import_day).await, 200)["results"][0]["metrics"],
        json!([17])
    );
    assert_eq!(
        expect(&pageviews(native_day).await, 200)["results"][0]["metrics"],
        json!([1])
    );
    let response = http
        .patch(
            &site_path(site.id, ""),
            &json!({
                "timezone": "America/Los_Angeles", "name": "must not commit", "week_starts_on": 0,
            }),
        )
        .cookie(&cookie)
        .send()
        .await;
    let view = rivu_storage_pg::sites::admin_view(&pg, site.id)
        .await
        .unwrap()
        .unwrap();
    let before = rollup_rows(&server.reader, site.id).await;
    // Exercise a timezone already persisted by a legacy process.
    sqlx::query("UPDATE sites SET timezone = 'America/Los_Angeles' WHERE id = $1")
        .bind(site.id as i64)
        .execute(&pg)
        .await
        .unwrap();
    let result = rivu_jobs::rollup::build_site(&pg, &ch, site.id).await;
    let metrics_after_build =
        expect(&pageviews(import_day).await, 200)["results"][0]["metrics"].clone();
    assert_ne!(
        metrics_after_build,
        json!([18]),
        "import and shifted native PV were added together"
    );
    assert_eq!(rollup_rows(&server.reader, site.id).await, before);
    expect_err(&response, 409, "timezone_import_conflict");
    assert_eq!(
        (
            view.timezone.as_str(),
            view.name.as_str(),
            view.week_starts_on
        ),
        ("UTC", "internal-api-e2e", 1)
    );
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("timezone_import_conflict")
    );
    rollup::delete(&ch, &scope, None, true).await.unwrap();
    let first_build = rivu_jobs::rollup::build_site(&pg, &ch, site.id)
        .await
        .unwrap_err();
    assert!(first_build.to_string().contains("timezone_import_conflict"));
    sqlx::query("UPDATE sites SET timezone = 'UTC' WHERE id = $1")
        .bind(site.id as i64)
        .execute(&pg)
        .await
        .unwrap();
    assert_eq!(
        expect(&pageviews(import_day).await, 200)["results"][0]["metrics"],
        json!([17])
    );
    expect(
        &http
            .patch(&site_path(site.id, ""), &json!({"timezone": "Asia/Taipei"}))
            .cookie(&cookie)
            .send()
            .await,
        200,
    );
    assert!(
        rivu_jobs::rollup::build_site(&pg, &ch, site.id)
            .await
            .unwrap()
            > 0
    );
    assert_eq!(
        expect(&pageviews(import_day).await, 200)["results"][0]["metrics"],
        json!([17])
    );
    assert_eq!(
        expect(&pageviews(native_day).await, 200)["results"][0]["metrics"],
        json!([1])
    );

    rollup::delete(&ch, &scope, None, false).await.unwrap();
    cleanup(&env, &[site.id]).await;
}

#[tokio::test]
async fn range_erase_waits_for_rebuild_past_pg_idle_timeout() {
    use rivu_storage_pg::stats_deletions;
    use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

    let Some(env) = test_env("range_erase_waits_for_rebuild_past_pg_idle_timeout") else {
        return;
    };
    migrate_once(&env).await;
    let pg = rivu_storage_pg::connect(&env.pg_url, 6).await.unwrap();
    let options = env.pg_url.parse::<PgConnectOptions>().unwrap().options([
        ("idle_in_transaction_session_timeout", "3s"),
        ("transaction_timeout", "4s"),
    ]);
    let maintenance_pg = PgPoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .unwrap();
    let now = Timestamp::now();
    let day = now
        .to_zoned(TimeZone::UTC)
        .date()
        .checked_sub(4.days())
        .unwrap();
    let cfg = config(&env, &[]);
    let server = start_at(&env, &pg, &cfg, Some(loose()), now).await;
    let org = make_org(&pg).await;
    let owner = make_user(&pg, Some((&org, "owner"))).await;
    let cookie = login_cookie(&server.http, &owner.email).await;
    let site = make_site(&pg, &org, false).await;
    let scope = TenantScope::new(site.id);
    let admin = rivu_storage_ch::client(&env.ch_url, &account("migrator"), Some(&ch_database()));
    let ch = rivu_storage_ch::client(&env.ch_url, &account("maintainer"), Some(&ch_database()));
    add_rollup_event(&admin, site.id, day).await;
    rollup::build_day(
        &ch,
        &scope,
        &SiteContext::new(site.id, TimeZone::UTC),
        day,
        1,
        now,
    )
    .await
    .unwrap();
    expect(
        &server
            .http
            .patch(&site_path(site.id, ""), &json!({"timezone": "Asia/Taipei"}))
            .cookie(&cookie)
            .send()
            .await,
        200,
    );
    admin
        .query("SYSTEM STOP MERGES rollup_pages")
        .execute()
        .await
        .unwrap();
    let rebuild = tokio::spawn({
        let ch = ch.clone();
        let pg = maintenance_pg.clone();
        async move { rivu_jobs::rollup::build_site(&pg, &ch, site.id).await }
    });
    let blocked = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let count: u64 = ch
                .query(
                    "SELECT count() FROM system.mutations WHERE database = currentDatabase()
                   AND table = 'rollup_pages' AND NOT is_done AND command LIKE ?",
                )
                .bind(format!("%site_id = _CAST({}, 'UInt64')%", site.id))
                .fetch_one()
                .await
                .unwrap();
            if count > 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await;
    let erase = async {
        let shifted = day.tomorrow().unwrap().to_string();
        server
            .http
            .post(
                &site_path(site.id, "/stats-deletions"),
                &json!({
                    "from": shifted, "to": shifted, "confirm": "internal-api-e2e",
                }),
            )
            .cookie(&cookie)
            .send()
            .await
    };
    tokio::pin!(erase);
    let early = if blocked.is_ok() {
        tokio::time::timeout(Duration::from_millis(4500), &mut erase)
            .await
            .ok()
    } else {
        None
    };
    admin
        .query("SYSTEM START MERGES rollup_pages")
        .execute()
        .await
        .unwrap();
    assert!(blocked.is_ok(), "no pending ClickHouse mutation");
    assert!(
        early.is_none(),
        "same-site erase finished before ClickHouse resumed: {:?}",
        early.map(|r| r.status)
    );
    assert!(
        tokio::time::timeout(Duration::from_secs(30), rebuild)
            .await
            .unwrap()
            .unwrap()
            .unwrap()
            > 0
    );
    expect(
        &tokio::time::timeout(Duration::from_secs(10), erase)
            .await
            .unwrap(),
        201,
    );
    let claimed = stats_deletions::claim_due(&pg, 100).await.unwrap();
    let intent = claimed.iter().find(|i| i.site_id == site.id).unwrap();
    rivu_jobs::stats_delete::process(&pg, &ch, intent)
        .await
        .unwrap();
    let status: String =
        sqlx::query_scalar("SELECT status FROM pending_stats_deletions WHERE id = $1")
            .bind(intent.id)
            .fetch_one(&pg)
            .await
            .unwrap();
    assert_eq!(status, "done");
    let pg_timeouts: (String, String) = sqlx::query_as(
        "SELECT current_setting('idle_in_transaction_session_timeout'), current_setting('transaction_timeout')",
    ).fetch_one(&maintenance_pg).await.unwrap();
    assert_eq!(pg_timeouts, ("3s".into(), "4s".into()));
    rollup::delete(&ch, &scope, None, false).await.unwrap();
    cleanup(&env, &[site.id]).await;
}

#[tokio::test]
async fn site_admin_timezone_preserves_native_history() {
    use rivu_jobs::rollup::{TIMEZONE_REBUILD_UNPROVEN, build_site};

    let Some(env) = test_env("site_admin_timezone_preserves_native_history") else {
        return;
    };
    migrate_once(&env).await;
    let pg = rivu_storage_pg::connect(&env.pg_url, 6).await.unwrap();
    let now = Timestamp::now();
    let today = now.to_zoned(TimeZone::UTC).date();
    let cfg = config(&env, &[("RIVU_ROLLUP_MIN_ROWS", "0")]);
    let server = start_at(&env, &pg, &cfg, Some(loose()), now).await;
    let http = &server.http;
    let org = make_org(&pg).await;
    let owner = make_user(&pg, Some((&org, "owner"))).await;
    let cookie = login_cookie(http, &owner.email).await;
    let admin = rivu_storage_ch::client(&env.ch_url, &account("migrator"), Some(&ch_database()));
    let ch = rivu_storage_ch::client(&env.ch_url, &account("maintainer"), Some(&ch_database()));
    let sync_ch = ch.clone().with_setting("lightweight_deletes_sync", "2");
    let patch = |id, body: Value| http.patch(&site_path(id, ""), &body).cookie(&cookie).send();
    let query = |id, day: Date| {
        let day = day.to_string();
        http.post(
            &query_path(id),
            &json!({"metrics": ["pageviews", "visits"], "date_range": [day, day]}),
        )
        .cookie(&cookie)
        .send()
    };

    let archive = make_site(&pg, &org, false).await;
    let archive_scope = TenantScope::new(archive.id);
    let oldest = today.checked_sub(40.days()).unwrap();
    let kept = today.checked_sub(3.days()).unwrap();
    add_rollup_event(&admin, archive.id, oldest).await;
    add_rollup_event(&admin, archive.id, kept).await;
    rollup::build_day(
        &ch,
        &archive_scope,
        &SiteContext::new(archive.id, TimeZone::UTC),
        oldest,
        1,
        now,
    )
    .await
    .unwrap();
    let old_query = expect(&query(archive.id, oldest).await, 200);
    assert_eq!(old_query["results"][0]["metrics"], json!([1, 1]));
    assert_eq!(old_query["meta"]["tiers"], json!(["rollup"]));
    let native = rollup_rows(&server.reader, archive.id).await;

    for (day, raw_left) in [(oldest, 1u64), (kept, 0u64)] {
        for (table, col) in [("events", "ts"), ("sessions", "started_at")] {
            sync_ch
                .query(&format!(
                    "DELETE FROM {table} WHERE site_id = ? AND toDate({col}) = toDate(?)"
                ))
                .bind(archive.id)
                .bind(day.to_string())
                .execute()
                .await
                .unwrap();
            let count: u64 = server
                .reader
                .query(&format!("SELECT count() FROM {table} WHERE site_id = ?"))
                .bind(archive.id)
                .fetch_one()
                .await
                .unwrap();
            assert_eq!(count, raw_left, "{table}");
        }
        expect_err(
            &patch(
                archive.id,
                json!({"timezone": "Asia/Taipei", "name": "must not commit", "week_starts_on": 0}),
            )
            .await,
            409,
            TIMEZONE_REBUILD_UNPROVEN,
        );
        let view = rivu_storage_pg::sites::admin_view(&pg, archive.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(view.timezone, "UTC");
        assert_eq!(view.name, "internal-api-e2e");
        assert_eq!(view.week_starts_on, 1);
        let response = expect(&query(archive.id, oldest).await, 200);
        assert_eq!(response["results"], old_query["results"]);
        assert_eq!(response["meta"]["tiers"], old_query["meta"]["tiers"]);
    }
    expect(
        &patch(archive.id, json!({"timezone": "UTC", "week_starts_on": 0})).await,
        200,
    );
    expect(&patch(archive.id, json!({"name": "new name"})).await, 200);

    // Simulate a timezone update already committed by a legacy process.
    sqlx::query("UPDATE sites SET timezone = 'Asia/Taipei' WHERE id = $1")
        .bind(archive.id as i64)
        .execute(&pg)
        .await
        .unwrap();
    let err = build_site(&pg, &ch, archive.id).await.unwrap_err();
    assert!(
        err.to_string().contains(TIMEZONE_REBUILD_UNPROVEN),
        "{err:#}"
    );
    assert_eq!(rollup_rows(&server.reader, archive.id).await, native);

    let edge = make_site(&pg, &org, false).await;
    let day = today.checked_sub(24.days()).unwrap();
    add_rollup_event(&admin, edge.id, day).await;
    rollup::build_day(
        &ch,
        &TenantScope::new(edge.id),
        &SiteContext::new(edge.id, TimeZone::UTC),
        day,
        1,
        now,
    )
    .await
    .unwrap();
    // Events for this day are still within 30 days. The session scan reaches further back.
    expect_err(
        &patch(edge.id, json!({"timezone": "Asia/Taipei"})).await,
        409,
        TIMEZONE_REBUILD_UNPROVEN,
    );

    let recent = make_site(&pg, &org, false).await;
    let scope = TenantScope::new(recent.id);
    for (table, dims) in [
        ("rollup_site", vec![]),
        ("rollup_pages", vec!["/imported".into()]),
    ] {
        rollup::insert_import(
            &ch,
            &scope,
            table,
            "UTC",
            1,
            &[rollup::ImportRow {
                day: oldest,
                dims,
                m: rollup::Measures {
                    pageviews: 17,
                    ..Default::default()
                },
            }],
        )
        .await
        .unwrap();
    }
    // Import-only sites may change timezone.
    expect(
        &patch(recent.id, json!({"timezone": "Europe/London"})).await,
        200,
    );
    expect(&patch(recent.id, json!({"timezone": "UTC"})).await, 200);
    add_rollup_event(&admin, recent.id, kept).await;
    assert!(build_site(&pg, &ch, recent.id).await.unwrap() > 0);
    assert_eq!(
        expect(&query(recent.id, kept).await, 200)["results"][0]["metrics"],
        json!([1, 1])
    );
    expect(
        &patch(recent.id, json!({"timezone": "Asia/Taipei"})).await,
        200,
    );

    let new_tz = TimeZone::get("Asia/Taipei").unwrap();
    // Recreate a partial family cleanup, and fail the next attempt with a read-only client.
    sync_ch
        .query("DELETE FROM rollup_pages WHERE site_id = ? AND origin = 'native'")
        .bind(recent.id)
        .execute()
        .await
        .unwrap();
    assert!(build_site(&pg, &server.reader, recent.id).await.is_err());
    expect_err(
        &http
            .post(
                &site_path(recent.id, "/stats-deletions"),
                &json!({
                    "from": kept.to_string(), "to": kept.to_string(), "confirm": "internal-api-e2e",
                }),
            )
            .cookie(&cookie)
            .send()
            .await,
        409,
        "stats_deletion_timezone_unproven",
    );
    assert!(
        rollup::build_state(&server.reader, &scope, &new_tz)
            .await
            .unwrap()
            .tz_changed
    );
    expect_err(
        &patch(recent.id, json!({"timezone": "UTC"})).await,
        409,
        TIMEZONE_REBUILD_UNPROVEN,
    );
    let async_default = ch.clone().with_setting("lightweight_deletes_sync", "0");
    assert!(build_site(&pg, &async_default, recent.id).await.unwrap() > 0);
    assert!(
        !rollup::build_state(&server.reader, &scope, &new_tz)
            .await
            .unwrap()
            .tz_changed
    );
    let shifted = expect(&query(recent.id, kept.tomorrow().unwrap()).await, 200);
    assert_eq!(shifted["results"][0]["metrics"], json!([1, 1]));
    assert_eq!(shifted["meta"]["tiers"], json!(["rollup"]));
    for table in ["rollup_site", "rollup_pages"] {
        let pages: u64 = server.reader
            .query(&format!("SELECT sum(pageviews) FROM {table} FINAL WHERE site_id = ? AND origin = 'import' AND import_id = 1 AND day = toDate(?)"))
            .bind(recent.id)
            .bind(oldest.to_string())
            .fetch_one()
            .await
            .unwrap();
        assert_eq!(pages, 17, "{table}");
    }

    let erase_day = kept.tomorrow().unwrap().to_string();
    expect(
        &http
            .post(
                &site_path(recent.id, "/stats-deletions"),
                &json!({"from": erase_day, "to": erase_day, "confirm": "internal-api-e2e"}),
            )
            .cookie(&cookie)
            .send()
            .await,
        201,
    );
    expect_err(
        &patch(recent.id, json!({"timezone": "UTC"})).await,
        409,
        TIMEZONE_REBUILD_UNPROVEN,
    );
    sqlx::query("DELETE FROM pending_stats_deletions WHERE site_id = $1")
        .bind(recent.id as i64)
        .execute(&pg)
        .await
        .unwrap();

    for id in [archive.id, edge.id, recent.id] {
        rollup::delete(&ch, &TenantScope::new(id), None, false)
            .await
            .unwrap();
    }
    cleanup(&env, &[archive.id, edge.id, recent.id]).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn site_admin_create_and_settings() {
    let Some(env) = test_env("site_admin_settings") else {
        return;
    };
    migrate_once(&env).await;
    let pg = rivu_storage_pg::connect(&env.pg_url, 6).await.unwrap();
    let cfg = config(&env, &[]);
    let server = start(&env, &pg, &cfg, Some(loose())).await;
    let http = &server.http;
    let c = cast(&pg, http).await;
    let org_id = c.org.id;

    // ---- 建站：角色与可见性
    let body = json!({"domain": "example.com", "timezone": "UTC"});
    expect_err(
        &http
            .post(&format!("/api/internal/orgs/{org_id}/sites"), &body)
            .send()
            .await,
        401,
        "unauthorized",
    );
    for (who, cookie) in [("viewer", &c.viewer), ("editor", &c.editor)] {
        let m = expect_err(
            &create_site_via_api(http, cookie, org_id, &body).await,
            403,
            "forbidden",
        );
        assert_eq!(m, "insufficient role", "{who}");
    }
    // 不是该组织成员：404，与组织不存在相同
    expect_err(
        &create_site_via_api(http, &c.outsider, org_id, &body).await,
        404,
        "not_found",
    );
    expect_err(
        &create_site_via_api(http, &c.admin, i64::MAX, &body).await,
        404,
        "not_found",
    );
    let r = http
        .post("/api/internal/orgs/abc/sites", &body)
        .cookie(&c.admin)
        .send()
        .await;
    expect_err(&r, 404, "not_found");

    // 域名规范化：scheme、www、端口、路径、大小写都去掉；名称缺省 = 域名
    let v = expect(
        &create_site_via_api(
            http,
            &c.admin,
            org_id,
            &json!({"domain": "  HTTPS://WWW.Example.com.:8443/a/b?q=1#f ", "timezone": "Asia/Shanghai"}),
        )
        .await,
        201,
    );
    let s = &v["site"];
    assert_eq!(s["name"], "example.com");
    assert_eq!(s["timezone"], "Asia/Shanghai");
    assert_eq!(s["is_public"], false);
    assert_eq!(s["role"], "admin");
    assert_eq!(s["access"], "member");
    assert_eq!(s["organization"]["id"], org_id);
    assert_eq!(s["organization"]["slug"], c.org.slug);
    let site_id = s["id"].as_u64().unwrap();
    let key = s["site_key"].as_str().unwrap().to_owned();
    assert_eq!(key.len(), 12);
    let settings = expect(
        &http
            .get(&site_path(site_id, "/settings"))
            .cookie(&c.admin)
            .send()
            .await,
        200,
    );
    assert_eq!(settings["hostnames"].as_array().unwrap().len(), 1);
    assert_eq!(settings["hostnames"][0]["pattern"], "example.com");
    // 与 `GET /sites` 同形
    let list = expect(
        &http
            .get("/api/internal/sites")
            .cookie(&c.admin)
            .send()
            .await,
        200,
    );
    let listed = list["sites"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["id"] == site_id)
        .expect("新站点在列表里")
        .clone();
    assert_eq!(&listed, s);

    // 自定义名称（trim）；owner 也可以建
    let v = expect(
        &create_site_via_api(
            http,
            &c.owner,
            org_id,
            &json!({"domain": "blog.example.org", "name": "  My Blog ", "timezone": "UTC"}),
        )
        .await,
        201,
    );
    assert_eq!(v["site"]["name"], "My Blog");
    assert_eq!(v["site"]["role"], "owner");

    // 400：时区、域名、名称
    for (body, prefix) in [
        (
            json!({"domain": "a.com", "timezone": "Mars/Base"}),
            "the timezone",
        ),
        (json!({"domain": "a.com", "timezone": ""}), "the timezone"),
        (
            json!({"domain": "*.a.com", "timezone": "UTC"}),
            "the domain",
        ),
        (json!({"domain": "", "timezone": "UTC"}), "the domain"),
        (
            json!({"domain": "bad domain.com", "timezone": "UTC"}),
            "the domain",
        ),
        (
            json!({"domain": "www.www.a.com", "timezone": "UTC"}),
            "the domain",
        ),
        (
            json!({"domain": "a.com", "name": "x".repeat(101), "timezone": "UTC"}),
            "the name",
        ),
        (
            json!({"domain": "a.com", "name": "a\u{7}b", "timezone": "UTC"}),
            "the name",
        ),
    ] {
        let m = expect_err(
            &create_site_via_api(http, &c.admin, org_id, &body).await,
            400,
            "invalid_request",
        );
        assert!(m.starts_with(prefix), "{body} -> {m}");
    }
    expect_err(
        &create_site_via_api(http, &c.admin, org_id, &json!({"domain": "a.com"})).await,
        400,
        "invalid_request",
    );

    // ---- 读设置：角色
    let want_geo = server.geo.capability().map(|c| c.as_str());
    let st = expect(
        &http
            .get(&site_path(site_id, "/settings"))
            .cookie(&c.editor)
            .send()
            .await,
        200,
    );
    assert_eq!(st["site"]["id"], site_id);
    assert_eq!(st["site"]["site_key"], key);
    assert_eq!(st["site"]["name"], "example.com");
    assert_eq!(st["site"]["is_public"], false);
    assert!(st["site"]["created_at"].as_str().unwrap().ends_with('Z'));
    assert_eq!(
        st["settings"],
        json!({"week_starts_on": 1, "collect_enabled": true, "tracker": {"hash": false, "engagement": true, "outbound": false, "downloads": false, "forms": false, "tagged": true},
               "ingest_rate_limit_per_min": null, "geo_level": "city", "retention_days": null})
    );
    assert_eq!(
        st["limits"]["shield_max_rules"],
        cfg.shield_max_rules_per_site
    );
    assert_eq!(
        st["limits"]["ingest_rate_limit_default_per_min"],
        json!(cfg.ingest_rate_limit_per_min)
    );
    assert!(
        st["limits"]["ingest_rate_limit_max_per_min"]
            .as_u64()
            .unwrap()
            >= 1_000_000
    );
    assert_eq!(st["geo_capability"], json!(want_geo));
    expect_err(
        &http
            .get(&site_path(site_id, "/settings"))
            .cookie(&c.viewer)
            .send()
            .await,
        403,
        "forbidden",
    );
    expect_err(
        &http
            .get(&site_path(site_id, "/settings"))
            .cookie(&c.outsider)
            .send()
            .await,
        404,
        "not_found",
    );
    expect_err(
        &http.get(&site_path(site_id, "/settings")).send().await,
        401,
        "unauthorized",
    );
    expect_err(
        &http
            .get(&site_path("nosuchsite1", "/settings"))
            .cookie(&c.admin)
            .send()
            .await,
        404,
        "not_found",
    );
    // 站点访客：editor 能读设置，写 403；viewer 读也 403
    let site = Site {
        id: site_id,
        key: key.clone(),
        org_id,
    };
    let ge = guest_of(&pg, &site, "editor").await;
    let gv = guest_of(&pg, &site, "viewer").await;
    let ge_cookie = login_cookie(http, &ge.email).await;
    let gv_cookie = login_cookie(http, &gv.email).await;
    expect(
        &http
            .get(&site_path(&key, "/settings"))
            .cookie(&ge_cookie)
            .send()
            .await,
        200,
    );
    expect_err(
        &http
            .get(&site_path(&key, "/settings"))
            .cookie(&gv_cookie)
            .send()
            .await,
        403,
        "forbidden",
    );
    expect_err(
        &http
            .patch(&site_path(&key, ""), &json!({"name": "x"}))
            .cookie(&ge_cookie)
            .send()
            .await,
        403,
        "forbidden",
    );

    // ---- PATCH：权限
    for (cookie, status) in [(&c.viewer, 403), (&c.editor, 403), (&c.outsider, 404)] {
        let r = http
            .patch(&site_path(site_id, ""), &json!({"name": "hijack"}))
            .cookie(cookie)
            .send()
            .await;
        assert_eq!(r.status, status, "{}", r.body);
    }
    expect_err(
        &http
            .patch(&site_path(site_id, ""), &json!({"name": "hijack"}))
            .send()
            .await,
        401,
        "unauthorized",
    );
    expect_err(
        &http
            .patch(&site_path(site_id, ""), &json!({"is_public": true}))
            .cookie(&c.editor)
            .send()
            .await,
        403,
        "forbidden",
    );

    // ---- PATCH：每个字段都能往返
    let patch = |body: Value| {
        let cookie = c.admin.clone();
        let path = site_path(site_id, "");
        async move { http.patch(&path, &body).cookie(&cookie).send().await }
    };
    let v = expect(&patch(json!({"name": "  Renamed  "})).await, 200);
    assert_eq!(v["site"]["name"], "Renamed");
    let v = expect(&patch(json!({"timezone": "Europe/Berlin"})).await, 200);
    assert_eq!(v["site"]["timezone"], "Europe/Berlin");
    let v = expect(&patch(json!({"week_starts_on": 0})).await, 200);
    assert_eq!(v["settings"]["week_starts_on"], 0);
    let v = expect(&patch(json!({"collect_enabled": false})).await, 200);
    assert_eq!(v["settings"]["collect_enabled"], false);
    let v = expect(&patch(json!({"geo_level": "country"})).await, 200);
    assert_eq!(v["settings"]["geo_level"], "country");
    // 保留期：不超过全局（limits.retention_days_max），null 恢复全局
    assert_eq!(v["limits"]["retention_days_max"], 731);
    let v = expect(&patch(json!({"retention_days": 90})).await, 200);
    assert_eq!(v["settings"]["retention_days"], 90);
    let v = expect(&patch(json!({"retention_days": null})).await, 200);
    assert_eq!(v["settings"]["retention_days"], Value::Null);
    // 只改一个字段不影响别的
    assert_eq!(v["site"]["name"], "Renamed");
    assert_eq!(v["settings"]["week_starts_on"], 0);
    assert_eq!(v["settings"]["collect_enabled"], false);
    // 限速：给值、再用 null 恢复默认
    let v = expect(
        &patch(json!({"ingest_rate_limit_per_min": 5000})).await,
        200,
    );
    assert_eq!(v["settings"]["ingest_rate_limit_per_min"], 5000);
    let v = expect(&patch(json!({"name": "Renamed2"})).await, 200);
    assert_eq!(
        v["settings"]["ingest_rate_limit_per_min"], 5000,
        "没提到的字段不被重置"
    );
    let v = expect(
        &patch(json!({"ingest_rate_limit_per_min": null})).await,
        200,
    );
    assert_eq!(v["settings"]["ingest_rate_limit_per_min"], Value::Null);
    // 公开：之后匿名的公开入口可达
    expect_err(
        &http
            .get(&format!("/api/internal/public/{key}"))
            .send()
            .await,
        404,
        "not_found",
    );
    let v = expect(&patch(json!({"is_public": true})).await, 200);
    assert_eq!(v["site"]["is_public"], true);
    expect(
        &http
            .get(&format!("/api/internal/public/{key}"))
            .send()
            .await,
        200,
    );
    expect(&patch(json!({"is_public": false})).await, 200);
    expect_err(
        &http
            .get(&format!("/api/internal/public/{key}"))
            .send()
            .await,
        404,
        "not_found",
    );

    // tracker 只覆盖给出的键，保留 tracker 里的其他键（含不认识的）
    sqlx::query("UPDATE site_settings SET tracker = '{\"other\": 1, \"nested\": {\"a\": true}}' WHERE site_id = $1")
        .bind(site_id as i64)
        .execute(&pg)
        .await
        .unwrap();
    let tracker = || async {
        let t: String =
            sqlx::query_scalar("SELECT tracker::text FROM site_settings WHERE site_id = $1")
                .bind(site_id as i64)
                .fetch_one(&pg)
                .await
                .unwrap();
        serde_json::from_str::<Value>(&t).unwrap()
    };
    let v = expect(&patch(json!({"tracker": {"hash": true}})).await, 200);
    assert_eq!(v["settings"]["tracker"]["hash"], true);
    assert_eq!(
        tracker().await,
        json!({"other": 1, "nested": {"a": true}, "hash": true})
    );
    let v = expect(
        &patch(json!({"tracker": {"hash": false, "outbound": true, "engagement": false}})).await,
        200,
    );
    assert_eq!(
        v["settings"]["tracker"],
        json!({"hash": false, "engagement": false, "outbound": true, "downloads": false,
               "forms": false, "tagged": true})
    );
    assert_eq!(
        tracker().await,
        json!({"other": 1, "nested": {"a": true}, "hash": false, "outbound": true, "engagement": false})
    );
    expect(&patch(json!({"tracker": {"hash": true}})).await, 200);
    // 未知键、非布尔、空对象：400
    for bad in [
        json!({"tracker": {"nope": true}}),
        json!({"tracker": {"hash": "yes"}}),
        json!({"tracker": {}}),
    ] {
        expect_err(&patch(bad.clone()).await, 400, "invalid_request");
    }

    // 采集侧读到的是同一份数据（collector 的站点快照来源）
    let row = rivu_storage_pg::sites::find_site_by_key(&pg, &key)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (
            row.timezone.as_str(),
            row.collect_enabled,
            row.geo_level.as_str(),
            row.tracker.hash,
            row.ingest_rate_limit_per_min
        ),
        ("Europe/Berlin", false, "country", true, None)
    );
    let week: i16 =
        sqlx::query_scalar("SELECT week_starts_on FROM site_settings WHERE site_id = $1")
            .bind(site_id as i64)
            .fetch_one(&pg)
            .await
            .unwrap();
    assert_eq!(week, 0);

    // 老数据没有 site_settings 行：GET 给列默认值，PATCH 补行
    sqlx::query("DELETE FROM site_settings WHERE site_id = $1")
        .bind(site_id as i64)
        .execute(&pg)
        .await
        .unwrap();
    let st = expect(
        &http
            .get(&site_path(site_id, "/settings"))
            .cookie(&c.admin)
            .send()
            .await,
        200,
    );
    assert_eq!(
        st["settings"],
        json!({"week_starts_on": 1, "collect_enabled": true, "tracker": {"hash": false, "engagement": true, "outbound": false, "downloads": false, "forms": false, "tagged": true},
               "ingest_rate_limit_per_min": null, "geo_level": "city", "retention_days": null})
    );
    let v = expect(&patch(json!({"week_starts_on": 6})).await, 200);
    assert_eq!(v["settings"]["week_starts_on"], 6);
    assert_eq!(v["settings"]["collect_enabled"], true);

    // 400：未知字段、空对象、取值越界、类型错
    let max = st["limits"]["ingest_rate_limit_max_per_min"]
        .as_u64()
        .unwrap();
    for bad in [
        json!({"nope": 1}),
        json!({"name": "ok", "site_key": "x"}),
        json!({}),
        json!({"name": ""}),
        json!({"name": "   "}),
        json!({"name": "x".repeat(101)}),
        json!({"timezone": "Not/AZone"}),
        json!({"week_starts_on": 7}),
        json!({"week_starts_on": -1}),
        json!({"week_starts_on": "1"}),
        json!({"ingest_rate_limit_per_min": 0}),
        json!({"ingest_rate_limit_per_min": -5}),
        json!({"ingest_rate_limit_per_min": max + 1}),
        json!({"ingest_rate_limit_per_min": "many"}),
        json!({"geo_level": "planet"}),
        json!({"retention_days": 29}),
        json!({"retention_days": 732}),
        json!({"is_public": "yes"}),
    ] {
        expect_err(&patch(bad.clone()).await, 400, "invalid_request");
    }
    let r = http
        .rb("PATCH", &site_path(site_id, ""), b"not json".to_vec())
        .header("Content-Type", "application/json")
        .header("Origin", &http.origin)
        .cookie(&c.admin)
        .send()
        .await;
    expect_err(&r, 400, "invalid_request");
    // 失败的 PATCH 不改任何东西
    let st = expect(
        &http
            .get(&site_path(site_id, "/settings"))
            .cookie(&c.admin)
            .send()
            .await,
        200,
    );
    assert_eq!(st["site"]["name"], "Renamed2");
    assert_eq!(st["settings"]["week_starts_on"], 6);
    // 边界值可以
    expect(&patch(json!({"ingest_rate_limit_per_min": max})).await, 200);
    expect(&patch(json!({"ingest_rate_limit_per_min": 1})).await, 200);
    // 没有 PATCH 以外的写方法
    assert_eq!(
        http.post(&site_path(site_id, ""), &json!({}))
            .cookie(&c.admin)
            .send()
            .await
            .status,
        405
    );

    // ---- 白名单
    let hp = site_path(site_id, "/hostnames");
    let add = |pattern: &str| {
        let cookie = c.admin.clone();
        let hp = hp.clone();
        let body = json!({"pattern": pattern});
        async move { http.post(&hp, &body).cookie(&cookie).send().await }
    };
    let v = expect(&add(" Blog.Example.COM ").await, 201);
    assert_eq!(v["hostname"]["pattern"], "blog.example.com");
    assert!(v["hostname"]["created_at"].as_str().unwrap().ends_with('Z'));
    expect_err(&add("blog.example.com").await, 409, "conflict");
    expect_err(&add("example.com").await, 409, "conflict");
    for bad in [
        "www.example.com",
        "*.www.example.com",
        "",
        "a b.com",
        "a.*.com",
        "http://x.com",
        "例え.jp",
    ] {
        expect_err(&add(bad).await, 400, "invalid_request");
    }
    expect(&add("*.example.com").await, 201);
    expect_err(
        &http
            .post(&hp, &json!({"pattern": "x.example.com"}))
            .cookie(&c.editor)
            .send()
            .await,
        403,
        "forbidden",
    );
    expect_err(
        &http
            .post(&hp, &json!({"pattern": "x.example.com"}))
            .cookie(&c.outsider)
            .send()
            .await,
        404,
        "not_found",
    );
    let hosts = || async {
        let st = expect(
            &http
                .get(&site_path(site_id, "/settings"))
                .cookie(&c.admin)
                .send()
                .await,
            200,
        );
        st["hostnames"]
            .as_array()
            .unwrap()
            .iter()
            .map(|h| h["pattern"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>()
    };
    // 按 created_at 升序：第一个是建站时的主域名
    assert_eq!(
        hosts().await,
        ["example.com", "blog.example.com", "*.example.com"]
    );
    // 删除：角色、404、URL 解码后的精确匹配
    expect_err(
        &http
            .delete(&format!("{hp}/blog.example.com"))
            .cookie(&c.editor)
            .send()
            .await,
        403,
        "forbidden",
    );
    expect(
        &http
            .delete(&format!("{hp}/blog.example.com"))
            .cookie(&c.admin)
            .send()
            .await,
        204,
    );
    expect_err(
        &http
            .delete(&format!("{hp}/blog.example.com"))
            .cookie(&c.admin)
            .send()
            .await,
        404,
        "not_found",
    );
    expect_err(
        &http
            .delete(&format!("{hp}/nothere.example.com"))
            .cookie(&c.admin)
            .send()
            .await,
        404,
        "not_found",
    );
    expect(
        &http
            .delete(&format!("{hp}/%2A.example.com"))
            .cookie(&c.admin)
            .send()
            .await,
        204,
    );
    assert_eq!(hosts().await, ["example.com"]);
    // 删掉的可以再加回来；允许删到空（fail-closed 由前端提示）
    expect(&add("blog.example.com").await, 201);
    expect(
        &http
            .delete(&format!("{hp}/blog.example.com"))
            .cookie(&c.admin)
            .send()
            .await,
        204,
    );
    expect(
        &http
            .delete(&format!("{hp}/example.com"))
            .cookie(&c.admin)
            .send()
            .await,
        204,
    );
    assert!(hosts().await.is_empty());
    let row = rivu_storage_pg::sites::find_site_by_key(&pg, &key)
        .await
        .unwrap()
        .unwrap();
    assert!(row.hostnames.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn site_admin_shield_and_shared_links() {
    let Some(env) = test_env("site_admin_shield_links") else {
        return;
    };
    migrate_once(&env).await;
    let pg = rivu_storage_pg::connect(&env.pg_url, 6).await.unwrap();
    let cfg = config(&env, &[("RIVU_SHIELD_MAX_RULES_PER_SITE", "4")]);
    let server = start(&env, &pg, &cfg, Some(loose())).await;
    let http = &server.http;
    let c = cast(&pg, http).await;
    let site = make_site(&pg, &c.org, false).await;
    let other = make_site(&pg, &c.org, false).await;
    let ge = guest_of(&pg, &site, "editor").await;
    let ge_cookie = login_cookie(http, &ge.email).await;
    let sp = site_path(site.id, "/shield");

    // ---- 屏蔽规则
    let add = |body: Value| {
        let cookie = c.admin.clone();
        let sp = sp.clone();
        async move { http.post(&sp, &body).cookie(&cookie).send().await }
    };
    let v = expect(
        &add(json!({"type": "ip", "action": "deny", "value": " 203.0.113.9 "})).await,
        201,
    );
    let rule = &v["rule"];
    assert_eq!(rule["type"], "ip");
    assert_eq!(rule["action"], "deny");
    assert_eq!(rule["value"], "203.0.113.9");
    assert_eq!(rule["description"], Value::Null);
    assert_eq!(rule["id"].as_str().unwrap().len(), 36);
    assert!(rule["created_at"].as_str().unwrap().ends_with('Z'));
    let ip_rule_id = rule["id"].as_str().unwrap().to_owned();
    // 规范化后的值：cidr 主机位清零、国家码大写、备注 trim
    let v = expect(
        &add(json!({"type": "cidr", "action": "deny", "value": "10.1.2.3/8"})).await,
        201,
    );
    assert_eq!(v["rule"]["value"], "10.0.0.0/8");
    let v = expect(
        &add(
            json!({"type": "country", "action": "allow", "value": "cn", "description": "  home  "}),
        )
        .await,
        201,
    );
    assert_eq!(v["rule"]["value"], "CN");
    assert_eq!(v["rule"]["action"], "allow");
    assert_eq!(v["rule"]["description"], "home");
    // 重复（同类型同取值，不论动作）409 conflict
    expect_err(
        &add(json!({"type": "ip", "action": "allow", "value": "203.0.113.9"})).await,
        409,
        "conflict",
    );
    expect_err(
        &add(json!({"type": "country", "action": "deny", "value": "CN"})).await,
        409,
        "conflict",
    );
    // 400：类型、动作、取值、备注
    for bad in [
        json!({"type": "nope", "action": "deny", "value": "x"}),
        json!({"type": "ip", "action": "block", "value": "1.2.3.4"}),
        json!({"type": "ip", "action": "deny", "value": "999.1.1.1"}),
        json!({"type": "ip", "action": "deny", "value": "10.0.0.0/8"}),
        json!({"type": "cidr", "action": "deny", "value": "10.0.0.0"}),
        json!({"type": "country", "action": "deny", "value": "CHN"}),
        json!({"type": "page", "action": "deny", "value": "no-slash"}),
        json!({"type": "hostname", "action": "deny", "value": "bad host"}),
        json!({"type": "ip", "action": "deny", "value": "1.2.3.4", "description": "d".repeat(201)}),
    ] {
        let m = expect_err(&add(bad.clone()).await, 400, "invalid_request");
        assert!(!m.is_empty(), "{bad}");
    }
    let m = expect_err(
        &add(json!({"type": "ip", "action": "deny", "value": "zzz"})).await,
        400,
        "invalid_request",
    );
    assert!(m.starts_with("the value"), "{m}");
    // 已满：第 4 条是最后一条，第 5 条 409 limit_reached；重复项在满了之后仍报 conflict
    expect(
        &add(json!({"type": "page", "action": "deny", "value": "/admin/*"})).await,
        201,
    );
    let m = expect_err(
        &add(json!({"type": "page", "action": "deny", "value": "/other"})).await,
        409,
        "limit_reached",
    );
    assert!(m.contains('4'), "{m}");
    expect_err(
        &add(json!({"type": "page", "action": "deny", "value": "/admin/*"})).await,
        409,
        "conflict",
    );

    // 列表：editor 能读（降序，最新的在前），viewer 403，写 403
    let list = expect(&http.get(&sp).cookie(&c.editor).send().await, 200);
    assert_eq!(list["max_rules"], 4);
    let rules = list["rules"].as_array().unwrap();
    assert_eq!(rules.len(), 4);
    let kinds: Vec<&str> = rules.iter().map(|r| r["type"].as_str().unwrap()).collect();
    assert_eq!(kinds, ["page", "country", "cidr", "ip"]);
    assert_eq!(rules[1]["description"], "home");
    expect(&http.get(&sp).cookie(&ge_cookie).send().await, 200);
    expect_err(
        &http.get(&sp).cookie(&c.viewer).send().await,
        403,
        "forbidden",
    );
    expect_err(
        &http.get(&sp).cookie(&c.outsider).send().await,
        404,
        "not_found",
    );
    expect_err(&http.get(&sp).send().await, 401, "unauthorized");
    for cookie in [&c.editor, &c.viewer, &ge_cookie] {
        let r = http
            .post(
                &sp,
                &json!({"type": "ip", "action": "deny", "value": "198.51.100.1"}),
            )
            .cookie(cookie)
            .send()
            .await;
        expect_err(&r, 403, "forbidden");
        expect_err(
            &http
                .delete(&format!("{sp}/{ip_rule_id}"))
                .cookie(cookie)
                .send()
                .await,
            403,
            "forbidden",
        );
    }
    expect_err(
        &http
            .post(
                &sp,
                &json!({"type": "ip", "action": "deny", "value": "198.51.100.1"}),
            )
            .cookie(&c.outsider)
            .send()
            .await,
        404,
        "not_found",
    );
    assert_eq!(
        rivu_storage_pg::shield::list_rules(&pg, site.id)
            .await
            .unwrap()
            .len(),
        4
    );
    // 删除：软删除；再删 404；非 UUID 404；别的站点的规则 id 404
    expect_err(
        &http
            .delete(&format!("{sp}/not-a-uuid"))
            .cookie(&c.admin)
            .send()
            .await,
        404,
        "not_found",
    );
    expect_err(
        &http
            .delete(&site_path(other.id, &format!("/shield/{ip_rule_id}")))
            .cookie(&c.admin)
            .send()
            .await,
        404,
        "not_found",
    );
    expect(
        &http
            .delete(&format!("{sp}/{ip_rule_id}"))
            .cookie(&c.admin)
            .send()
            .await,
        204,
    );
    expect_err(
        &http
            .delete(&format!("{sp}/{ip_rule_id}"))
            .cookie(&c.admin)
            .send()
            .await,
        404,
        "not_found",
    );
    assert_eq!(
        rivu_storage_pg::shield::list_rules(&pg, site.id)
            .await
            .unwrap()
            .len(),
        3
    );
    // 软删除后同值规则可以再加（腾出了名额）
    expect(
        &add(json!({"type": "ip", "action": "deny", "value": "203.0.113.9"})).await,
        201,
    );

    // ---- 共享链接
    let lp = site_path(site.id, "/shared-links");
    let create = |cookie: &str, body: Value| {
        let lp = lp.clone();
        let cookie = cookie.to_owned();
        async move { http.post(&lp, &body).cookie(&cookie).send().await }
    };
    let editor_id: i64 = sqlx::query_scalar(
        "SELECT m.user_id FROM memberships m JOIN users u ON u.id = m.user_id
         WHERE m.organization_id = $1 AND m.role = 'editor'",
    )
    .bind(c.org.id)
    .fetch_one(&pg)
    .await
    .unwrap();
    let v = expect(&create(&c.editor, json!({})).await, 201);
    let plain = &v["link"];
    assert_eq!(plain["name"], Value::Null);
    assert_eq!(plain["has_password"], false);
    assert_eq!(plain["expires_at"], Value::Null);
    assert_eq!(plain["slug"].as_str().unwrap().len(), 43);
    assert_eq!(plain["created_by"]["id"], editor_id);
    assert!(
        plain["created_by"]["email"]
            .as_str()
            .unwrap()
            .ends_with("@example.test")
    );
    assert!(plain["created_at"].as_str().unwrap().ends_with('Z'));
    let plain_slug = plain["slug"].as_str().unwrap().to_owned();
    let plain_id = plain["id"].as_i64().unwrap();
    let info = expect(
        &http
            .get(&format!("/api/internal/shared/{plain_slug}"))
            .send()
            .await,
        200,
    );
    assert_eq!(info["authorized"], true);

    // 名称、密码、过期时间；密码走 argon2id，可以用它换访问凭证
    let v = expect(
        &create(
            &c.admin,
            json!({"name": "  Weekly report ", "password": PASSWORD, "expires_at": "2099-01-01T08:00:00+08:00"}),
        )
        .await,
        201,
    );
    let pw = &v["link"];
    assert_eq!(pw["name"], "Weekly report");
    assert_eq!(pw["has_password"], true);
    assert!(
        pw["expires_at"]
            .as_str()
            .unwrap()
            .starts_with("2099-01-01T00:00:00")
    );
    let pw_slug = pw["slug"].as_str().unwrap().to_owned();
    let pw_id = pw["id"].as_i64().unwrap();
    let hash: String = sqlx::query_scalar("SELECT password_hash FROM shared_links WHERE id = $1")
        .bind(pw_id)
        .fetch_one(&pg)
        .await
        .unwrap();
    assert!(hash.starts_with("$argon2id$"));
    let info = expect(
        &http
            .get(&format!("/api/internal/shared/{pw_slug}"))
            .send()
            .await,
        200,
    );
    assert_eq!(
        (
            info["password_required"].as_bool(),
            info["authorized"].as_bool()
        ),
        (Some(true), Some(false))
    );
    let r = http
        .post(
            &format!("/api/internal/shared/{pw_slug}/auth"),
            &json!({"password": PASSWORD}),
        )
        .send()
        .await;
    expect(&r, 200);
    let m = expect_err(
        &create(&c.admin, json!({"password": "short-pw-11"})).await,
        400,
        "invalid_request",
    );
    assert!(m.contains("password"), "{m}");
    expect_err(
        &create(&c.admin, json!({"password": "p".repeat(129)})).await,
        400,
        "invalid_request",
    );
    expect(
        &create(&c.admin, json!({"password": "p".repeat(12)})).await,
        201,
    );
    expect(
        &create(&c.admin, json!({"password": "p".repeat(128)})).await,
        201,
    );
    let m = expect_err(
        &create(&c.admin, json!({"expires_at": "2020-01-01T00:00:00Z"})).await,
        400,
        "invalid_request",
    );
    assert_eq!(m, "the expires_at must be in the future");
    for bad in [
        json!({"expires_at": "tomorrow"}),
        json!({"expires_at": "2099-01-01"}),
        json!({"expires_at": 5}),
    ] {
        expect_err(&create(&c.admin, bad).await, 400, "invalid_request");
    }
    let m = expect_err(
        &create(&c.admin, json!({"name": "n".repeat(101)})).await,
        400,
        "invalid_request",
    );
    assert!(m.starts_with("the name"), "{m}");
    // 角色：viewer 与站点访客不能建（访客读列表可以），外人 404
    expect_err(&create(&c.viewer, json!({})).await, 403, "forbidden");
    expect_err(&create(&ge_cookie, json!({})).await, 403, "forbidden");
    expect_err(&create(&c.outsider, json!({})).await, 404, "not_found");

    // 列表：未吊销的（含已过期的），按创建时间降序，带 slug 与 has_password
    let expired = user_cmd::issue_link(&pg, &site.key, None, Some("1m"))
        .await
        .unwrap();
    sqlx::query("UPDATE shared_links SET expires_at = now() - interval '1 hour' WHERE id = $1")
        .bind(expired.id)
        .execute(&pg)
        .await
        .unwrap();
    let list = expect(&http.get(&lp).cookie(&c.editor).send().await, 200);
    let links = list["links"].as_array().unwrap();
    let ids: Vec<i64> = links.iter().map(|l| l["id"].as_i64().unwrap()).collect();
    assert!(
        ids.windows(2).all(|w| w[0] > w[1]),
        "按创建时间降序：{ids:?}"
    );
    assert_eq!(ids.len(), 5);
    let by_id = |id: i64| links.iter().find(|l| l["id"] == id).unwrap();
    assert_eq!(by_id(plain_id)["slug"], plain_slug);
    assert_eq!(by_id(pw_id)["name"], "Weekly report");
    assert_eq!(by_id(pw_id)["has_password"], true);
    assert_eq!(
        by_id(expired.id)["created_by"],
        Value::Null,
        "命令行建的没有创建者"
    );
    assert!(
        by_id(expired.id)["expires_at"].is_string(),
        "已过期的也返回"
    );
    expect(&http.get(&lp).cookie(&ge_cookie).send().await, 200);
    expect_err(
        &http.get(&lp).cookie(&c.viewer).send().await,
        403,
        "forbidden",
    );
    expect_err(
        &http.get(&lp).cookie(&c.outsider).send().await,
        404,
        "not_found",
    );

    // 撤销：角色、别的站点的链接 404、吊销后入口 404
    let other_link = user_cmd::issue_link(&pg, &other.key, None, None)
        .await
        .unwrap();
    let del = |cookie: &str, id: i64| {
        let p = format!("{lp}/{id}");
        let cookie = cookie.to_owned();
        async move { http.delete(&p).cookie(&cookie).send().await }
    };
    expect_err(&del(&c.viewer, plain_id).await, 403, "forbidden");
    expect_err(&del(&ge_cookie, plain_id).await, 403, "forbidden");
    expect_err(&del(&c.outsider, plain_id).await, 404, "not_found");
    expect_err(&del(&c.editor, other_link.id).await, 404, "not_found");
    expect(
        &http
            .get(&format!("/api/internal/shared/{}", other_link.slug))
            .send()
            .await,
        200,
    );
    expect_err(
        &http
            .delete(&format!("{lp}/abc"))
            .cookie(&c.editor)
            .send()
            .await,
        404,
        "not_found",
    );
    expect(&del(&c.editor, plain_id).await, 204);
    expect_err(
        &http
            .get(&format!("/api/internal/shared/{plain_slug}"))
            .send()
            .await,
        404,
        "not_found",
    );
    expect_err(&del(&c.editor, plain_id).await, 404, "not_found");
    expect(&del(&c.admin, pw_id).await, 204);
    expect_err(
        &http
            .get(&format!("/api/internal/shared/{pw_slug}"))
            .send()
            .await,
        404,
        "not_found",
    );
    let list = expect(&http.get(&lp).cookie(&c.admin).send().await, 200);
    let ids: Vec<i64> = list["links"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l["id"].as_i64().unwrap())
        .collect();
    assert!(!ids.contains(&plain_id) && !ids.contains(&pw_id));
    assert_eq!(ids.len(), 3);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn site_admin_delete_site_and_csrf() {
    let Some(env) = test_env("site_admin_delete_csrf") else {
        return;
    };
    migrate_once(&env).await;
    let pg = rivu_storage_pg::connect(&env.pg_url, 6).await.unwrap();
    let server = start(&env, &pg, &config(&env, &[]), Some(loose())).await;
    let http = &server.http;
    let c = cast(&pg, http).await;

    let v = expect(
        &create_site_via_api(
            http,
            &c.admin,
            c.org.id,
            &json!({"domain": "to-delete.example.com", "name": "To Delete", "timezone": "UTC"}),
        )
        .await,
        201,
    );
    let id = v["site"]["id"].as_u64().unwrap();
    let key = v["site"]["site_key"].as_str().unwrap().to_owned();
    let site = Site {
        id,
        key: key.clone(),
        org_id: c.org.id,
    };
    let sp = site_path(id, "");
    let confirm = |s: &str| json!({"confirm": s});
    // 先公开并建一个共享链接，删除后两者都要 404
    expect(
        &http
            .patch(&sp, &json!({"is_public": true}))
            .cookie(&c.admin)
            .send()
            .await,
        200,
    );
    let link = user_cmd::issue_link(&pg, &key, None, None).await.unwrap();
    let spec = &specs()[0].1;
    expect(
        &http
            .post(&query_path(id), spec)
            .cookie(&c.admin)
            .send()
            .await,
        200,
    );
    expect(
        &http
            .get(&format!("/api/internal/public/{key}"))
            .send()
            .await,
        200,
    );

    // CSRF：每个新的写路由在缺 Origin、别的源、非 JSON 类型时 403 csrf_rejected，且什么都没改
    let rule_id = "00000000-0000-0000-0000-000000000000";
    let routes: Vec<(&'static str, String, Value)> = vec![
        (
            "POST",
            format!("/api/internal/orgs/{}/sites", c.org.id),
            json!({"domain": "csrf.example.com", "timezone": "UTC"}),
        ),
        ("PATCH", sp.clone(), json!({"name": "csrf"})),
        ("DELETE", sp.clone(), confirm("To Delete")),
        (
            "POST",
            site_path(id, "/hostnames"),
            json!({"pattern": "csrf.example.com"}),
        ),
        (
            "DELETE",
            site_path(id, "/hostnames/to-delete.example.com"),
            json!({}),
        ),
        (
            "POST",
            site_path(id, "/shield"),
            json!({"type": "ip", "action": "deny", "value": "1.2.3.4"}),
        ),
        (
            "DELETE",
            site_path(id, &format!("/shield/{rule_id}")),
            json!({}),
        ),
        ("POST", site_path(id, "/shared-links"), json!({})),
        (
            "DELETE",
            site_path(id, &format!("/shared-links/{}", link.id)),
            json!({}),
        ),
    ];
    for (method, path, body) in &routes {
        let mk = || {
            http.rb(method, path, body.to_string().into_bytes())
                .header("Content-Type", "application/json")
                .header("Origin", &http.origin)
                .cookie(&c.admin)
        };
        for (what, rb) in [
            ("缺 Origin", mk().remove("Origin")),
            ("别的源", mk().header("Origin", "http://evil.example")),
            ("表单类型", mk().header("Content-Type", "text/plain")),
        ] {
            let r = rb.send().await;
            assert_eq!(r.status, 403, "{method} {path} {what}：{}", r.body);
            assert_eq!(r.json()["error"], "csrf_rejected", "{method} {path} {what}");
        }
    }
    let st = expect(
        &http
            .get(&site_path(id, "/settings"))
            .cookie(&c.admin)
            .send()
            .await,
        200,
    );
    assert_eq!(st["site"]["name"], "To Delete");
    assert_eq!(st["hostnames"].as_array().unwrap().len(), 1);
    assert_eq!(
        expect(
            &http
                .get(&site_path(id, "/shield"))
                .cookie(&c.admin)
                .send()
                .await,
            200
        )["rules"],
        json!([])
    );
    assert_eq!(
        expect(
            &http
                .get(&site_path(id, "/shared-links"))
                .cookie(&c.admin)
                .send()
                .await,
            200
        )["links"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let sites_now: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM sites WHERE organization_id = $1 AND deleted_at IS NULL",
    )
    .bind(c.org.id)
    .fetch_one(&pg)
    .await
    .unwrap();
    assert_eq!(sites_now, 1, "CSRF 被拒的建站请求没有建出站点");

    // 删除：角色、确认名称
    for (cookie, status) in [(&c.viewer, 403), (&c.editor, 403), (&c.outsider, 404)] {
        let r = http
            .delete_with(&sp, &confirm("To Delete"))
            .cookie(cookie)
            .send()
            .await;
        assert_eq!(r.status, status, "{}", r.body);
    }
    expect_err(
        &http.delete_with(&sp, &confirm("To Delete")).send().await,
        401,
        "unauthorized",
    );
    for wrong in ["", "to delete", "To Delete ", "To", "example.com"] {
        let m = expect_err(
            &http
                .delete_with(&sp, &confirm(wrong))
                .cookie(&c.admin)
                .send()
                .await,
            400,
            "invalid_request",
        );
        assert!(m.contains("confirm"), "{m}");
    }
    expect_err(
        &http
            .delete_with(&sp, &json!({}))
            .cookie(&c.admin)
            .send()
            .await,
        400,
        "invalid_request",
    );
    expect_err(
        &http.delete(&sp).cookie(&c.admin).send().await,
        400,
        "invalid_request",
    );
    expect(
        &http
            .get(&site_path(id, "/settings"))
            .cookie(&c.admin)
            .send()
            .await,
        200,
    );

    // 删除后：站点从列表消失，设置、统计、公开入口、共享链接、写端点全部 404
    expect(
        &http
            .delete_with(&sp, &confirm("To Delete"))
            .cookie(&c.admin)
            .send()
            .await,
        204,
    );
    let list = expect(
        &http
            .get("/api/internal/sites")
            .cookie(&c.admin)
            .send()
            .await,
        200,
    );
    assert!(
        list["sites"]
            .as_array()
            .unwrap()
            .iter()
            .all(|s| s["id"] != id)
    );
    expect_err(
        &http
            .get(&site_path(id, "/settings"))
            .cookie(&c.admin)
            .send()
            .await,
        404,
        "not_found",
    );
    expect_err(
        &http
            .post(&query_path(id), spec)
            .cookie(&c.admin)
            .send()
            .await,
        404,
        "not_found",
    );
    expect_err(
        &http
            .post(&query_path(&key), spec)
            .cookie(&c.admin)
            .send()
            .await,
        404,
        "not_found",
    );
    expect_err(
        &http
            .get(&format!("/api/internal/public/{key}"))
            .send()
            .await,
        404,
        "not_found",
    );
    expect_err(
        &http.post(&query_path(id), spec).send().await,
        404,
        "not_found",
    );
    expect_err(
        &http
            .get(&format!("/api/internal/shared/{}", link.slug))
            .send()
            .await,
        404,
        "not_found",
    );
    expect_err(
        &http
            .get(&site_path(id, "/shield"))
            .cookie(&c.admin)
            .send()
            .await,
        404,
        "not_found",
    );
    expect_err(
        &http
            .patch(&sp, &json!({"name": "zombie"}))
            .cookie(&c.admin)
            .send()
            .await,
        404,
        "not_found",
    );
    expect_err(
        &http
            .delete_with(&sp, &confirm("To Delete"))
            .cookie(&c.admin)
            .send()
            .await,
        404,
        "not_found",
    );
    expect_err(
        &http
            .post(
                &site_path(id, "/hostnames"),
                &json!({"pattern": "z.example.com"}),
            )
            .cookie(&c.admin)
            .send()
            .await,
        404,
        "not_found",
    );
    // 软删除：行还在，采集侧的站点快照里没有它
    let (deleted, name): (bool, String) =
        sqlx::query_as("SELECT deleted_at IS NOT NULL, name FROM sites WHERE id = $1")
            .bind(id as i64)
            .fetch_one(&pg)
            .await
            .unwrap();
    assert!(deleted);
    assert_eq!(name, "To Delete");
    assert!(
        rivu_storage_pg::sites::find_site_by_key(&pg, &site.key)
            .await
            .unwrap()
            .is_none()
    );
}

// ================================================================== 目标

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn goals_crud_permissions_and_goal_queries() {
    let Some(env) = test_env("goals") else {
        return;
    };
    migrate_once(&env).await;
    let pg = rivu_storage_pg::connect(&env.pg_url, 6).await.unwrap();
    let server = start(&env, &pg, &config(&env, &[]), Some(loose())).await;
    let http = &server.http;
    let c = cast(&pg, http).await;
    let site = make_site(&pg, &c.org, false).await;
    let other = make_site(&pg, &c.org, false).await;
    let ge = guest_of(&pg, &site, "editor").await;
    let ge_cookie = login_cookie(http, &ge.email).await;
    let gv = guest_of(&pg, &site, "viewer").await;
    let gv_cookie = login_cookie(http, &gv.email).await;
    let gp = site_path(site.id, "/goals");

    // ---- 添加：owner、admin、editor、站点访客 editor 都可以（site.content.write）
    let add = |cookie: &str, body: Value| {
        let cookie = cookie.to_owned();
        let gp = gp.clone();
        async move { http.post(&gp, &body).cookie(&cookie).send().await }
    };
    let v = expect(
        &add(
            &c.owner,
            json!({"kind": "page", "page_path": " /pricing ", "display_name": " Pricing "}),
        )
        .await,
        201,
    );
    let g = &v["goal"];
    assert_eq!(g["kind"], "page");
    assert_eq!(g["page_path"], "/pricing");
    assert_eq!(g["event_name"], Value::Null);
    assert_eq!(g["display_name"], "Pricing");
    assert_eq!(g["label"], "Pricing");
    assert!(g["created_at"].as_str().unwrap().ends_with('Z'));
    let pricing_id = g["id"].as_i64().unwrap();
    let v = expect(
        &add(&c.admin, json!({"kind": "page", "page_path": "/**"})).await,
        201,
    );
    assert_eq!(v["goal"]["page_path"], "/*", "连续的 * 合并");
    assert_eq!(v["goal"]["label"], "/*");
    let v = expect(
        &add(
            &c.editor,
            json!({"kind": "event", "event_name": "Signup", "display_name": null}),
        )
        .await,
        201,
    );
    let signup_id = v["goal"]["id"].as_i64().unwrap();
    assert_eq!(v["goal"]["label"], "Signup");
    expect(
        &add(
            &ge_cookie,
            json!({"kind": "event", "event_name": "Download"}),
        )
        .await,
        201,
    );
    // viewer、站点访客 viewer 403；外人 404；没有会话 401
    for cookie in [&c.viewer, &gv_cookie] {
        expect_err(
            &add(cookie, json!({"kind": "event", "event_name": "X"})).await,
            403,
            "forbidden",
        );
        expect_err(
            &http
                .delete(&format!("{gp}/{signup_id}"))
                .cookie(cookie)
                .send()
                .await,
            403,
            "forbidden",
        );
        expect_err(&http.get(&gp).cookie(cookie).send().await, 403, "forbidden");
    }
    expect_err(
        &add(&c.outsider, json!({"kind": "event", "event_name": "X"})).await,
        404,
        "not_found",
    );
    expect_err(&http.get(&gp).send().await, 401, "unauthorized");
    // 重复（同类同值）、同名（标签撞了）409 conflict
    expect_err(
        &add(
            &c.owner,
            json!({"kind": "event", "event_name": "Signup", "display_name": "Other"}),
        )
        .await,
        409,
        "conflict",
    );
    expect_err(
        &add(&c.owner, json!({"kind": "event", "event_name": "Pricing"})).await,
        409,
        "conflict",
    );
    // 400：类型、缺字段、多字段、事件名、页面通配、显示名、滚动深度与属性条件（不在本期）
    for bad in [
        json!({"kind": "scroll", "page_path": "/a"}),
        json!({"kind": "event"}),
        json!({"kind": "page"}),
        json!({"kind": "event", "event_name": "A", "page_path": "/a"}),
        json!({"kind": "page", "page_path": "/a", "event_name": "A"}),
        json!({"kind": "event", "event_name": "x".repeat(121)}),
        json!({"kind": "event", "event_name": "a\u{1}b"}),
        json!({"kind": "event", "event_name": "engagement"}),
        json!({"kind": "page", "page_path": "no-slash"}),
        json!({"kind": "page", "page_path": "/a b"}),
        json!({"kind": "page", "page_path": format!("/{}", "a".repeat(250))}),
        json!({"kind": "event", "event_name": "A", "display_name": "d".repeat(101)}),
        json!({"kind": "event", "event_name": "A", "props": {"plan": "pro"}}),
    ] {
        let r = add(&c.owner, bad.clone()).await;
        assert_eq!(r.status, 400, "{bad}: {}", r.body);
    }

    // ---- 列表：editor 与站点访客 editor 能读，按创建顺序
    let list = expect(&http.get(&gp).cookie(&ge_cookie).send().await, 200);
    assert_eq!(list["max_goals"], 100);
    let labels: Vec<&str> = list["goals"]
        .as_array()
        .unwrap()
        .iter()
        .map(|g| g["label"].as_str().unwrap())
        .collect();
    assert_eq!(labels, ["Pricing", "/*", "Signup", "Download"]);
    expect(&http.get(&gp).cookie(&c.editor).send().await, 200);

    // ---- 查询：目标从 PG 现读；目标维度、目标过滤与转化率（数据：6 人 × 3 天各浏览 / 与 /pricing，
    //      3 月 12 日又有 3 人只浏览 /；每日访客之和 = 18 + 3 = 21）
    ingest(&env, &pg, &[&site], &server.geo).await;
    let range = json!(["2026-03-09", "2026-03-12"]);
    let spec = json!({"metrics": ["visitors", "events", "conversion_rate"],
                      "dimensions": ["event:goal"], "date_range": range});
    // 能看看板的人（viewer、站点访客 viewer）都能拿到目标名字：它们随结果行返回
    for cookie in [&c.viewer, &gv_cookie, &c.owner] {
        let v = expect(
            &http
                .post(&query_path(site.id), &spec)
                .cookie(cookie)
                .send()
                .await,
            200,
        );
        let rows: Vec<(String, i64, i64, f64)> = v["results"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| {
                (
                    r["dimensions"][0].as_str().unwrap().to_owned(),
                    r["metrics"][0].as_i64().unwrap(),
                    r["metrics"][1].as_i64().unwrap(),
                    r["metrics"][2].as_f64().unwrap(),
                )
            })
            .collect();
        // 没有转化的目标（Signup、Download）不出现
        assert_eq!(rows.len(), 2, "{rows:?}");
        assert_eq!(rows[0], ("/*".to_owned(), 21, 39, 1.0));
        assert_eq!(
            (rows[1].0.as_str(), rows[1].1, rows[1].2),
            ("Pricing", 18, 18)
        );
        assert!((rows[1].3 - 18.0 / 21.0).abs() < 1e-12, "{rows:?}");
        assert!(
            v["meta"]["metric_semantics"]["conversion_rate"]
                .as_str()
                .unwrap()
                .contains("event:goal")
        );
    }
    // 目标过滤（按标签与按 id 相同）：分母去掉目标叶子
    for value in [json!("Pricing"), json!(pricing_id), json!("pricing")] {
        let cs = !matches!(&value, Value::String(s) if s == "pricing");
        let spec = json!({"metrics": ["visitors", "conversion_rate"], "date_range": range,
                          "filters": [["is", "event:goal", [value], {"case_sensitive": cs}]]});
        let v = expect(
            &http
                .post(&query_path(site.id), &spec)
                .cookie(&c.viewer)
                .send()
                .await,
            200,
        );
        assert_eq!(v["results"][0]["metrics"][0], 18, "{spec}");
        let cr = v["results"][0]["metrics"][1].as_f64().unwrap();
        assert!((cr - 18.0 / 21.0).abs() < 1e-12, "{spec}: {cr}");
    }
    // 校验：转化率要有目标维度或过滤；目标过滤只认 is / is_not
    let r = http
        .post(
            &query_path(site.id),
            &json!({"metrics": ["conversion_rate"], "date_range": range}),
        )
        .cookie(&c.owner)
        .send()
        .await;
    assert_eq!(r.status, 400, "{}", r.body);

    // ---- 删除：软删除，立即从查询里消失；再删 404；非整数 id 404；别的站点 404
    expect(
        &http
            .delete(&format!("{gp}/{pricing_id}"))
            .cookie(&ge_cookie)
            .send()
            .await,
        204,
    );
    expect_err(
        &http
            .delete(&format!("{gp}/{pricing_id}"))
            .cookie(&c.owner)
            .send()
            .await,
        404,
        "not_found",
    );
    expect_err(
        &http
            .delete(&format!("{gp}/abc"))
            .cookie(&c.owner)
            .send()
            .await,
        404,
        "not_found",
    );
    expect_err(
        &http
            .delete(&site_path(other.id, &format!("/goals/{signup_id}")))
            .cookie(&c.owner)
            .send()
            .await,
        404,
        "not_found",
    );
    let v = expect(
        &http
            .post(&query_path(site.id), &spec)
            .cookie(&c.owner)
            .send()
            .await,
        200,
    );
    assert_eq!(v["results"].as_array().unwrap().len(), 1, "{v}");
    assert_eq!(v["results"][0]["dimensions"][0], "/*");
    // 同一事件名删除后可以再加
    expect(
        &add(&c.owner, json!({"kind": "page", "page_path": "/pricing"})).await,
        201,
    );
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM goals WHERE site_id = $1")
        .bind(site.id as i64)
        .fetch_one(&pg)
        .await
        .unwrap();
    assert_eq!(n, 5, "软删除的行仍在");

    cleanup(&env, &[site.id, other.id]).await;
}
