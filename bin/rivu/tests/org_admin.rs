//! 看板的组织管理端点（成员、邀请、站点访客、自己的 API key、邀请页）的端到端测试：真实的 PostgreSQL，
//! 真实的本地 TCP 端口，路由由 `public::router_full` 装配（与 `internal_api.rs` 同一条路径）。
//!
//! 缺少环境变量时跳过并打印原因（同 `internal_api.rs`）：
//!   RIVU_TEST_DATABASE_URL     有 DDL 与写权限的 PostgreSQL 连接串（rivu_migrator）
//!   RIVU_TEST_CLICKHOUSE_URL   ClickHouse HTTP 地址（只用来装配读客户端，本文件不查询）
//!
//! 用例：
//! - `members_*`：列表（viewer 与 admin 的视角）、改角色与移除的角色矩阵、自己 403、最后一名 owner 409、并发互降、
//!   移除成员连带吊销 API key；
//! - `invitations_*`：邀请 → 查询 → 接受（新用户登录、已有账号的会话）、邮箱不符、过期与撤销、过期后重新邀请、限流、日志里没有秘密；
//! - `guests_*`：站点访客的邀请、接受、移除；
//! - `api_keys_*`：创建（全部站点、指定站点、别的组织的站点）、只列自己的、吊销别人的 404、创建出来的 key 能被鉴权查到；
//! - `csrf_*`：缺 Origin 的写请求被拒。

use std::net::SocketAddr;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use clickhouse::Client;
use jiff::Timestamp;
use rivu::dash_api::{self, DashApi, DashLimits};
use rivu::{migrate, public};
use rivu_auth::key::hash_key;
use rivu_auth::password::hash_blocking;
use rivu_common::config::{ChAccount, Config, Env, Role, Secret};
use rivu_storage_pg::members::{self, MemberChange};
use rivu_storage_pg::sites::NewSite;
use serde_json::{Value, json};
use sqlx::PgPool;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::OnceCell;

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
        // 服务端在读完请求体之前就回了 4xx 并关了连接：写入可能被对端重置，响应照样要读
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

    fn patch(&self, path: &str, body: &Value) -> Rb<'_> {
        self.json_rb("PATCH", path, body.to_string().into_bytes())
    }

    fn delete(&self, path: &str) -> Rb<'_> {
        self.json_rb("DELETE", path, Vec::new())
    }
}

async fn start(env: &TestEnv, pg: &PgPool, limits: DashLimits) -> (Http, Config) {
    init_logging();
    let cfg = config(env);
    let reader: Client =
        rivu_storage_ch::client(&env.ch_url, &account("reader"), Some(&ch_database()));
    let now: Timestamp = "2026-03-12T12:00:00Z".parse().unwrap();
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

// ------------------------------------------------------------------ 日志

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

impl LogBuf {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
    }
}

/// 全进程唯一的日志订阅者（TRACE，含 sqlx 与 hyper 的），写进内存。
fn init_logging() -> LogBuf {
    static LOGS: OnceLock<LogBuf> = OnceLock::new();
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

/// 一个登录过的用户。
#[derive(Clone)]
struct Actor {
    id: i64,
    email: String,
    cookie: String,
}

async fn make_user(pg: &PgPool) -> (i64, String) {
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
    (id, email)
}

async fn login_cookie(http: &Http, email: &str) -> String {
    let r = http
        .post(
            "/api/internal/login",
            &json!({"email": email, "password": PASSWORD}),
        )
        .send()
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
    r.cookie_pair("rivu_session").expect("会话 cookie")
}

/// 建用户（可选加入组织）并登录。
async fn actor(pg: &PgPool, http: &Http, member_of: Option<(&Org, &str)>) -> Actor {
    let (id, email) = make_user(pg).await;
    if let Some((org, role)) = member_of {
        rivu_storage_pg::users::upsert_member(pg, org.id, id, role)
            .await
            .unwrap();
    }
    let cookie = login_cookie(http, &email).await;
    Actor { id, email, cookie }
}

/// 一个组织与它的各种角色（两名 owner，方便测「还有另一名 owner」）。
struct World {
    org: Org,
    owner: Actor,
    owner2: Actor,
    admin: Actor,
    editor: Actor,
    viewer: Actor,
    outsider: Actor,
}

async fn world(pg: &PgPool, http: &Http) -> World {
    let org = make_org(pg).await;
    let other = make_org(pg).await;
    let owner = actor(pg, http, Some((&org, "owner"))).await;
    let owner2 = actor(pg, http, Some((&org, "owner"))).await;
    let admin = actor(pg, http, Some((&org, "admin"))).await;
    let editor = actor(pg, http, Some((&org, "editor"))).await;
    let viewer = actor(pg, http, Some((&org, "viewer"))).await;
    let outsider = actor(pg, http, Some((&other, "owner"))).await;
    World {
        org,
        owner,
        owner2,
        admin,
        editor,
        viewer,
        outsider,
    }
}

struct Site {
    id: u64,
    key: String,
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
    Site {
        id: created.id,
        key: created.site_key,
    }
}

// ------------------------------------------------------------------ 断言与路径

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

fn members_path(org: &Org) -> String {
    format!("/api/internal/orgs/{}/members", org.id)
}

fn member_path(org: &Org, user_id: i64) -> String {
    format!("/api/internal/orgs/{}/members/{user_id}", org.id)
}

fn invitations_path(org: &Org) -> String {
    format!("/api/internal/orgs/{}/invitations", org.id)
}

fn keys_path(org: &Org) -> String {
    format!("/api/internal/orgs/{}/api-keys", org.id)
}

async fn get_members(http: &Http, org: &Org, cookie: &str) -> Value {
    expect(
        &http.get(&members_path(org)).cookie(cookie).send().await,
        200,
    )
}

fn member_role_in(list: &Value, user_id: i64) -> Option<String> {
    list["members"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["user_id"] == user_id)
        .map(|m| m["role"].as_str().unwrap().to_owned())
}

/// 邀请（组织成员）：返回 `(邀请 id, 令牌)`。
async fn invite(http: &Http, org: &Org, cookie: &str, email: &str, role: &str) -> (i64, String) {
    let r = http
        .post(
            &invitations_path(org),
            &json!({"email": email, "role": role}),
        )
        .cookie(cookie)
        .send()
        .await;
    let v = expect(&r, 201);
    (
        v["invitation"]["id"].as_i64().unwrap(),
        token_of(v["url"].as_str().unwrap()),
    )
}

fn token_of(url: &str) -> String {
    let prefix = format!("{HTTP_ORIGIN}/invite/");
    let t = url
        .strip_prefix(&prefix)
        .unwrap_or_else(|| panic!("链接形状不对：{url}"));
    assert_eq!(t.len(), 43, "令牌是 43 个 base64url 字符");
    t.to_owned()
}

fn invite_info_path(token: &str) -> String {
    format!("/api/internal/invitations/{token}")
}

fn accept_path(token: &str) -> String {
    format!("/api/internal/invitations/{token}/accept")
}

async fn owner_count(pg: &PgPool, org: &Org) -> i64 {
    sqlx::query_scalar(
        "SELECT count(*) FROM memberships WHERE organization_id = $1 AND role = 'owner' AND deleted_at IS NULL",
    )
    .bind(org.id)
    .fetch_one(pg)
    .await
    .unwrap()
}

// ------------------------------------------------------------------ 成员

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn members_list_as_viewer_and_admin() {
    let Some(env) = test_env("members_list") else {
        return;
    };
    migrate_once(&env).await;
    let pg = rivu_storage_pg::connect(&env.pg_url, 6).await.unwrap();
    let (http, _) = start(&env, &pg, loose()).await;
    let w = world(&pg, &http).await;

    // 没有会话 401；非成员与不存在的组织、不是数字的组织 404
    expect_err(
        &http.get(&members_path(&w.org)).send().await,
        401,
        "unauthorized",
    );
    expect_err(
        &http
            .get(&members_path(&w.org))
            .cookie(&w.outsider.cookie)
            .send()
            .await,
        404,
        "not_found",
    );
    expect_err(
        &http
            .get("/api/internal/orgs/999999999/members")
            .cookie(&w.owner.cookie)
            .send()
            .await,
        404,
        "not_found",
    );
    expect_err(
        &http
            .get("/api/internal/orgs/abc/members")
            .cookie(&w.owner.cookie)
            .send()
            .await,
        404,
        "not_found",
    );

    // 一条待处理的邀请与一个访客，列表里都要有
    let site = make_site(&pg, &w.org, "Docs").await;
    let (inv_id, _) = invite(
        &http,
        &w.org,
        &w.admin.cookie,
        &format!("{}@example.test", unique("pending")),
        "editor",
    )
    .await;
    let guest = make_user(&pg).await;
    sqlx::query("INSERT INTO site_memberships (site_id, user_id, role) VALUES ($1, $2, 'viewer')")
        .bind(site.id as i64)
        .bind(guest.0)
        .execute(&pg)
        .await
        .unwrap();

    // viewer 的视角：能看，不能管理；`me` 标出自己；按角色排序
    let v = get_members(&http, &w.org, &w.viewer.cookie).await;
    assert_eq!(v["can_manage"], false);
    assert_eq!(v["my_role"], "viewer");
    let members = v["members"].as_array().unwrap();
    assert_eq!(members.len(), 5);
    let roles: Vec<&str> = members
        .iter()
        .map(|m| m["role"].as_str().unwrap())
        .collect();
    assert_eq!(roles, ["owner", "owner", "admin", "editor", "viewer"]);
    let me: Vec<i64> = members
        .iter()
        .filter(|m| m["me"] == true)
        .map(|m| m["user_id"].as_i64().unwrap())
        .collect();
    assert_eq!(me, [w.viewer.id]);
    let first = &members[0];
    assert!(first["joined_at"].as_str().unwrap().ends_with('Z'));
    assert!(first["email"].is_string());
    // 登录过的用户有 last_active_at
    assert!(first["last_active_at"].is_string());
    assert_eq!(v["invitations"].as_array().unwrap().len(), 1);
    assert_eq!(v["invitations"][0]["id"], inv_id);
    assert_eq!(v["invitations"][0]["role"], "editor");
    assert_eq!(v["invitations"][0]["site"], Value::Null);
    assert!(v["invitations"][0]["expires_at"].is_string());
    assert_eq!(v["guests"].as_array().unwrap().len(), 1);
    assert_eq!(v["guests"][0]["user_id"], guest.0);
    assert_eq!(v["guests"][0]["site"]["id"], site.id);
    assert_eq!(v["guests"][0]["site"]["name"], "Docs");
    assert_eq!(v["guests"][0]["role"], "viewer");

    // admin 的视角
    let v = get_members(&http, &w.org, &w.admin.cookie).await;
    assert_eq!(v["can_manage"], true);
    assert_eq!(v["my_role"], "admin");

    // 删掉的成员不在列表里
    sqlx::query(
        "UPDATE memberships SET deleted_at = now() WHERE organization_id = $1 AND user_id = $2",
    )
    .bind(w.org.id)
    .bind(w.editor.id)
    .execute(&pg)
    .await
    .unwrap();
    let v = get_members(&http, &w.org, &w.owner.cookie).await;
    assert_eq!(v["members"].as_array().unwrap().len(), 4);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn members_role_change_and_removal_rules() {
    let Some(env) = test_env("members_rules") else {
        return;
    };
    migrate_once(&env).await;
    let pg = rivu_storage_pg::connect(&env.pg_url, 6).await.unwrap();
    let (http, _) = start(&env, &pg, loose()).await;
    let w = world(&pg, &http).await;
    let patch = |actor: &Actor, target: i64, body: Value| {
        let http = http.clone();
        let cookie = actor.cookie.clone();
        let path = member_path(&w.org, target);
        async move { http.patch(&path, &body).cookie(&cookie).send().await }
    };

    // viewer、editor 写：403；没有会话 401
    expect_err(
        &patch(&w.viewer, w.editor.id, json!({"role": "viewer"})).await,
        403,
        "forbidden",
    );
    expect_err(
        &http
            .delete(&member_path(&w.org, w.viewer.id))
            .cookie(&w.editor.cookie)
            .send()
            .await,
        403,
        "forbidden",
    );
    expect_err(
        &http
            .patch(&member_path(&w.org, w.editor.id), &json!({"role": "admin"}))
            .send()
            .await,
        401,
        "unauthorized",
    );
    // 非成员：404（与组织不存在相同）
    expect_err(
        &patch(&w.outsider, w.editor.id, json!({"role": "viewer"})).await,
        404,
        "not_found",
    );

    // 自己：403（admin 与 owner 都一样），改与移除都是
    for who in [&w.admin, &w.owner] {
        expect_err(
            &patch(who, who.id, json!({"role": "viewer"})).await,
            403,
            "forbidden",
        );
        expect_err(
            &http
                .delete(&member_path(&w.org, who.id))
                .cookie(&who.cookie)
                .send()
                .await,
            403,
            "forbidden",
        );
    }

    // 目标不是成员：404；角色取值不对：400
    expect_err(
        &patch(&w.admin, w.outsider.id, json!({"role": "viewer"})).await,
        404,
        "not_found",
    );
    expect_err(
        &http
            .delete(&member_path(&w.org, w.outsider.id))
            .cookie(&w.admin.cookie)
            .send()
            .await,
        404,
        "not_found",
    );
    expect_err(
        &patch(&w.admin, w.editor.id, json!({"role": "root"})).await,
        400,
        "invalid_request",
    );
    expect_err(
        &patch(&w.admin, w.editor.id, json!({})).await,
        400,
        "invalid_request",
    );

    // admin 不能碰 owner（改、移除），也不能把别人升成 owner
    expect_err(
        &patch(&w.admin, w.owner.id, json!({"role": "viewer"})).await,
        403,
        "forbidden",
    );
    expect_err(
        &http
            .delete(&member_path(&w.org, w.owner.id))
            .cookie(&w.admin.cookie)
            .send()
            .await,
        403,
        "forbidden",
    );
    expect_err(
        &patch(&w.admin, w.editor.id, json!({"role": "owner"})).await,
        403,
        "forbidden",
    );
    let v = get_members(&http, &w.org, &w.owner.cookie).await;
    assert_eq!(member_role_in(&v, w.owner.id).as_deref(), Some("owner"));
    assert_eq!(member_role_in(&v, w.editor.id).as_deref(), Some("editor"));

    // admin 可以在 admin、editor、viewer 之间改，响应是更新后的成员
    let v = expect(
        &patch(&w.admin, w.editor.id, json!({"role": "viewer"})).await,
        200,
    );
    assert_eq!(v["user_id"], w.editor.id);
    assert_eq!(v["role"], "viewer");
    assert_eq!(v["me"], false);
    let v = expect(
        &patch(&w.admin, w.editor.id, json!({"role": "admin"})).await,
        200,
    );
    assert_eq!(v["role"], "admin");

    // owner 可以升 owner，也可以降另一名 owner（还剩他自己）
    let v = expect(
        &patch(&w.owner, w.editor.id, json!({"role": "owner"})).await,
        200,
    );
    assert_eq!(v["role"], "owner");
    assert_eq!(owner_count(&pg, &w.org).await, 3);
    expect(
        &patch(&w.owner, w.editor.id, json!({"role": "editor"})).await,
        200,
    );
    expect(
        &patch(&w.owner, w.owner2.id, json!({"role": "admin"})).await,
        200,
    );
    assert_eq!(owner_count(&pg, &w.org).await, 1);
    // 现在 owner2 是 admin：不能再碰 owner（他的会话仍有效，角色是现查的）
    expect_err(
        &patch(&w.owner2, w.owner.id, json!({"role": "viewer"})).await,
        403,
        "forbidden",
    );

    // 移除：admin 移除 viewer 成功，再移除 404
    expect(
        &http
            .delete(&member_path(&w.org, w.viewer.id))
            .cookie(&w.admin.cookie)
            .send()
            .await,
        204,
    );
    expect_err(
        &http
            .delete(&member_path(&w.org, w.viewer.id))
            .cookie(&w.admin.cookie)
            .send()
            .await,
        404,
        "not_found",
    );
    // 被移除的人不再是成员：看不到这个组织
    expect_err(
        &http
            .get(&members_path(&w.org))
            .cookie(&w.viewer.cookie)
            .send()
            .await,
        404,
        "not_found",
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn members_last_owner_is_kept() {
    let Some(env) = test_env("members_last_owner") else {
        return;
    };
    migrate_once(&env).await;
    let pg = rivu_storage_pg::connect(&env.pg_url, 6).await.unwrap();
    let (http, _) = start(&env, &pg, loose()).await;
    let w = world(&pg, &http).await;

    // 经 HTTP 够不到「最后一名 owner」：自己 403，admin 碰不了 owner，另一名 owner 在时数目 ≥ 2。
    // 所以这里直接测存储层的事务规则：放行一切判定（闭包恒真）时，仍然 409。
    // 先把 owner2 降成 admin，只剩 owner 一名 owner
    expect(
        &http
            .patch(&member_path(&w.org, w.owner2.id), &json!({"role": "admin"}))
            .cookie(&w.owner.cookie)
            .send()
            .await,
        200,
    );
    let demote = members::change_role(&pg, w.org.id, w.admin.id, w.owner.id, "admin", |_, _| true)
        .await
        .unwrap();
    assert_eq!(demote, MemberChange::LastOwner);
    let remove = members::remove_member(&pg, w.org.id, w.admin.id, w.owner.id, |_, _| true)
        .await
        .unwrap();
    assert_eq!(remove, MemberChange::LastOwner);
    assert_eq!(owner_count(&pg, &w.org).await, 1);
    let v = get_members(&http, &w.org, &w.owner.cookie).await;
    assert_eq!(member_role_in(&v, w.owner.id).as_deref(), Some("owner"));
    // 判定闭包拒绝：Denied，且没有任何改动
    let denied = members::change_role(&pg, w.org.id, w.admin.id, w.editor.id, "viewer", |_, _| {
        false
    })
    .await
    .unwrap();
    assert_eq!(denied, MemberChange::Denied);
    // 目标或调用者不是成员：NotMember
    let none = members::remove_member(&pg, w.org.id, w.admin.id, w.outsider.id, |_, _| true)
        .await
        .unwrap();
    assert_eq!(none, MemberChange::NotMember);

    // 并发互降：两名 owner 同时降对方，组织行锁让其中一个先完成，另一个按锁内的最新角色被拒（403），永远剩一名 owner
    expect(
        &http
            .patch(&member_path(&w.org, w.owner2.id), &json!({"role": "owner"}))
            .cookie(&w.owner.cookie)
            .send()
            .await,
        200,
    );
    assert_eq!(owner_count(&pg, &w.org).await, 2);
    let a = http
        .patch(
            &member_path(&w.org, w.owner2.id),
            &json!({"role": "viewer"}),
        )
        .cookie(&w.owner.cookie)
        .send();
    let b = http
        .patch(&member_path(&w.org, w.owner.id), &json!({"role": "viewer"}))
        .cookie(&w.owner2.cookie)
        .send();
    let (ra, rb) = tokio::join!(a, b);
    let mut statuses = [ra.status, rb.status];
    statuses.sort_unstable();
    assert_eq!(statuses, [200, 403], "{} / {}", ra.body, rb.body);
    assert_eq!(owner_count(&pg, &w.org).await, 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn members_removal_revokes_their_api_keys() {
    let Some(env) = test_env("members_remove_keys") else {
        return;
    };
    migrate_once(&env).await;
    let pg = rivu_storage_pg::connect(&env.pg_url, 6).await.unwrap();
    let (http, _) = start(&env, &pg, loose()).await;
    let w = world(&pg, &http).await;

    // editor 在这个组织建一把 key；另一个组织里也有他的一把（不能被连带吊销）
    let made = expect(
        &http
            .post(&keys_path(&w.org), &json!({"name": "BI export"}))
            .cookie(&w.editor.cookie)
            .send()
            .await,
        201,
    );
    let key = made["key"].as_str().unwrap().to_owned();
    let hash = hash_key(&key);
    assert!(
        rivu_storage_pg::api_keys::find_active_by_hash(&pg, &hash)
            .await
            .unwrap()
            .is_some()
    );
    let other = make_org(&pg).await;
    rivu_storage_pg::users::upsert_member(&pg, other.id, w.editor.id, "viewer")
        .await
        .unwrap();
    let other_made = expect(
        &http
            .post(&keys_path(&other), &json!({"name": "elsewhere"}))
            .cookie(&w.editor.cookie)
            .send()
            .await,
        201,
    );
    let other_hash = hash_key(other_made["key"].as_str().unwrap());

    expect(
        &http
            .delete(&member_path(&w.org, w.editor.id))
            .cookie(&w.admin.cookie)
            .send()
            .await,
        204,
    );
    assert!(
        rivu_storage_pg::api_keys::find_active_by_hash(&pg, &hash)
            .await
            .unwrap()
            .is_none()
    );
    // 把他加回来：旧 key 不会复活，列表里也没有
    rivu_storage_pg::users::upsert_member(&pg, w.org.id, w.editor.id, "editor")
        .await
        .unwrap();
    assert!(
        rivu_storage_pg::api_keys::find_active_by_hash(&pg, &hash)
            .await
            .unwrap()
            .is_none(),
        "移除成员必须同时吊销他的 key"
    );
    let v = expect(
        &http
            .get(&keys_path(&w.org))
            .cookie(&w.editor.cookie)
            .send()
            .await,
        200,
    );
    assert_eq!(v["keys"].as_array().unwrap().len(), 0);
    // 别的组织里的 key 不受影响
    assert!(
        rivu_storage_pg::api_keys::find_active_by_hash(&pg, &other_hash)
            .await
            .unwrap()
            .is_some()
    );
}

// ------------------------------------------------------------------ 邀请

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn invitations_create_lookup_and_accept_as_new_user() {
    let Some(env) = test_env("invitations_new_user") else {
        return;
    };
    migrate_once(&env).await;
    let pg = rivu_storage_pg::connect(&env.pg_url, 6).await.unwrap();
    let (http, _) = start(&env, &pg, loose()).await;
    let logs = init_logging();
    let w = world(&pg, &http).await;
    let email = format!("{}@Example.Test", unique("Newcomer"));
    let new_password = "Brand-new-secret-pass-77";

    // 角色与输入校验
    let invite_with = |actor: &Actor, body: Value| {
        let http = http.clone();
        let cookie = actor.cookie.clone();
        let path = invitations_path(&w.org);
        async move { http.post(&path, &body).cookie(&cookie).send().await }
    };
    expect_err(
        &invite_with(
            &w.viewer,
            json!({"email": "a@example.test", "role": "viewer"}),
        )
        .await,
        403,
        "forbidden",
    );
    expect_err(
        &invite_with(
            &w.editor,
            json!({"email": "a@example.test", "role": "viewer"}),
        )
        .await,
        403,
        "forbidden",
    );
    expect_err(
        &invite_with(
            &w.outsider,
            json!({"email": "a@example.test", "role": "viewer"}),
        )
        .await,
        404,
        "not_found",
    );
    for bad in [
        json!({"email": "not-an-email", "role": "viewer"}),
        json!({"email": "a@example.test", "role": "owner"}),
        json!({"email": "a@example.test", "role": "root"}),
        json!({"email": "a@example.test"}),
        json!({"email": "a@example.test", "role": "viewer", "site_role": "viewer"}),
        json!({"email": "a@example.test", "site_id": 1}),
    ] {
        expect_err(&invite_with(&w.admin, bad).await, 400, "invalid_request");
    }
    // 已经是成员：409
    expect_err(
        &invite_with(&w.admin, json!({"email": w.viewer.email, "role": "viewer"})).await,
        409,
        "conflict",
    );

    // 邀请：邮箱规范化为小写，响应带一次性链接
    let r = invite_with(
        &w.admin,
        json!({"email": format!("  {email}  "), "role": "editor"}),
    )
    .await;
    let v = expect(&r, 201);
    let inv = &v["invitation"];
    assert_eq!(inv["email"], email.to_lowercase());
    assert_eq!(inv["role"], "editor");
    assert_eq!(inv["site"], Value::Null);
    assert_eq!(inv["site_role"], Value::Null);
    assert_eq!(inv["invited_by"], "Test User");
    let token = token_of(v["url"].as_str().unwrap());
    // 库里只存摘要
    let stored: Vec<u8> = sqlx::query_scalar("SELECT token_hash FROM invitations WHERE id = $1")
        .bind(inv["id"].as_i64().unwrap())
        .fetch_one(&pg)
        .await
        .unwrap();
    assert_eq!(stored, rivu_auth::session::hash_token(&token).to_vec());
    // 同一个邮箱再邀请：409
    expect_err(
        &invite_with(&w.admin, json!({"email": email, "role": "viewer"})).await,
        409,
        "conflict",
    );

    // 查询：不需要登录
    let v = expect(&http.get(&invite_info_path(&token)).send().await, 200);
    assert_eq!(v["email"], email.to_lowercase());
    assert_eq!(v["organization"]["id"], w.org.id);
    assert_eq!(v["role"], "editor");
    assert_eq!(v["site"], Value::Null);
    assert_eq!(v["inviter"], "Test User");
    assert_eq!(v["account_exists"], false);
    assert_eq!(v["signed_in_as"], Value::Null);
    assert!(v["expires_at"].is_string());
    // 带会话时告诉页面当前登录的是谁
    let v = expect(
        &http
            .get(&invite_info_path(&token))
            .cookie(&w.viewer.cookie)
            .send()
            .await,
        200,
    );
    assert_eq!(v["signed_in_as"], w.viewer.email);
    // 令牌形状不对或不存在：同样的 404
    for bad in ["short".to_owned(), "x".repeat(43), "=".repeat(43)] {
        expect_err(
            &http.get(&invite_info_path(&bad)).send().await,
            404,
            "not_found",
        );
    }

    // 接受：新用户要给密码（12 到 128 个字符）
    let accept = |body: Value| {
        let http = http.clone();
        let path = accept_path(&token);
        async move { http.post(&path, &body).send().await }
    };
    expect_err(&accept(json!({})).await, 400, "invalid_request");
    expect_err(
        &accept(json!({"password": "short"})).await,
        400,
        "invalid_request",
    );
    let r = accept(json!({"name": "  Newcomer  ", "password": new_password})).await;
    let v = expect(&r, 200);
    assert_eq!(v["organization_id"], w.org.id);
    assert_eq!(v["site_id"], Value::Null);
    // 直接登录：会话 cookie 有效，是组织的 editor，不是实例管理员
    let cookie = r.cookie_pair("rivu_session").expect("接受邀请后登录");
    let me = expect(
        &http.get("/api/internal/me").cookie(&cookie).send().await,
        200,
    );
    assert_eq!(me["user"]["email"], email.to_lowercase());
    assert_eq!(me["user"]["name"], "Newcomer");
    assert_eq!(me["user"]["is_instance_admin"], false);
    assert_eq!(me["organizations"][0]["id"], w.org.id);
    assert_eq!(me["organizations"][0]["role"], "editor");
    // 他出现在成员列表里，邀请从待处理里消失
    let list = get_members(&http, &w.org, &w.owner.cookie).await;
    assert_eq!(list["invitations"].as_array().unwrap().len(), 0);
    assert!(
        list["members"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["email"] == email.to_lowercase() && m["role"] == "editor")
    );
    // 一次性：再看、再接受、撤销都是 404
    expect_err(
        &http.get(&invite_info_path(&token)).send().await,
        404,
        "not_found",
    );
    expect_err(
        &accept(json!({"password": new_password})).await,
        404,
        "not_found",
    );
    // 日志里没有令牌、密码、key 之类的秘密
    let text = logs.text();
    for secret in [token.as_str(), new_password, email.to_lowercase().as_str()] {
        assert!(!text.contains(secret), "日志里不应出现 {secret}");
    }
    assert!(text.contains("接受邀请"), "写操作要有 info 日志");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn invitations_accept_as_existing_user() {
    let Some(env) = test_env("invitations_existing") else {
        return;
    };
    migrate_once(&env).await;
    let pg = rivu_storage_pg::connect(&env.pg_url, 6).await.unwrap();
    let (http, _) = start(&env, &pg, loose()).await;
    let w = world(&pg, &http).await;
    let existing = actor(&pg, &http, None).await;
    let (_, token) = invite(&http, &w.org, &w.owner.cookie, &existing.email, "viewer").await;

    let v = expect(&http.get(&invite_info_path(&token)).send().await, 200);
    assert_eq!(v["account_exists"], true);
    let post = |cookie: Option<&str>, body: Value| {
        let http = http.clone();
        let path = accept_path(&token);
        let cookie = cookie.map(str::to_owned);
        async move {
            let rb = http.post(&path, &body);
            match cookie {
                Some(c) => rb.cookie(&c).send().await,
                None => rb.send().await,
            }
        }
    };
    // 已有账号但没有会话：401（即便带了密码也不创建、不登录）
    expect_err(
        &post(None, json!({"password": "whatever-password-123"})).await,
        401,
        "unauthorized",
    );
    // 登录的是别人：403 email_mismatch
    expect_err(
        &post(Some(&w.editor.cookie), json!({})).await,
        403,
        "email_mismatch",
    );
    // 邀请仍然待处理
    expect(&http.get(&invite_info_path(&token)).send().await, 200);
    // 本人的会话：成功；不再签发新的会话 cookie
    let r = post(Some(&existing.cookie), json!({})).await;
    let v = expect(&r, 200);
    assert_eq!(v["organization_id"], w.org.id);
    assert_eq!(r.cookie_pair("rivu_session"), None);
    let list = get_members(&http, &w.org, &existing.cookie).await;
    assert_eq!(
        member_role_in(&list, existing.id).as_deref(),
        Some("viewer")
    );
    expect_err(
        &post(Some(&existing.cookie), json!({})).await,
        404,
        "not_found",
    );

    // 邀请已经是（别的途径加入的）成员的人：接受时 409，邀请保持待处理
    let late = actor(&pg, &http, None).await;
    let (_, token2) = invite(&http, &w.org, &w.owner.cookie, &late.email, "viewer").await;
    rivu_storage_pg::users::upsert_member(&pg, w.org.id, late.id, "editor")
        .await
        .unwrap();
    expect_err(
        &http
            .post(&accept_path(&token2), &json!({}))
            .cookie(&late.cookie)
            .send()
            .await,
        409,
        "conflict",
    );
    // 被禁用的账号：404（他的会话同时失效，不会落到 401，也不会被当成没有账号而重建）
    let off = actor(&pg, &http, None).await;
    let (_, token3) = invite(&http, &w.org, &w.owner.cookie, &off.email, "viewer").await;
    sqlx::query("UPDATE users SET disabled_at = now() WHERE id = $1")
        .bind(off.id)
        .execute(&pg)
        .await
        .unwrap();
    expect_err(
        &http
            .post(&accept_path(&token3), &json!({}))
            .cookie(&off.cookie)
            .send()
            .await,
        404,
        "not_found",
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn invitations_revoked_and_expired_tokens_are_not_found() {
    let Some(env) = test_env("invitations_expiry") else {
        return;
    };
    migrate_once(&env).await;
    let pg = rivu_storage_pg::connect(&env.pg_url, 6).await.unwrap();
    let (http, _) = start(&env, &pg, loose()).await;
    let w = world(&pg, &http).await;
    let email = format!("{}@example.test", unique("later"));

    // 撤销：viewer 不行；admin 行；之后查询与接受都是 404，再撤销也是 404
    let (id, token) = invite(&http, &w.org, &w.admin.cookie, &email, "viewer").await;
    let path = format!("{}/{id}", invitations_path(&w.org));
    expect_err(
        &http.delete(&path).cookie(&w.viewer.cookie).send().await,
        403,
        "forbidden",
    );
    expect_err(
        &http.delete(&path).cookie(&w.outsider.cookie).send().await,
        404,
        "not_found",
    );
    expect(
        &http.delete(&path).cookie(&w.admin.cookie).send().await,
        204,
    );
    expect_err(
        &http.get(&invite_info_path(&token)).send().await,
        404,
        "not_found",
    );
    expect_err(
        &http
            .post(
                &accept_path(&token),
                &json!({"password": "Some-password-123456"}),
            )
            .send()
            .await,
        404,
        "not_found",
    );
    expect_err(
        &http.delete(&path).cookie(&w.admin.cookie).send().await,
        404,
        "not_found",
    );
    // 撤销后可以立刻重新邀请
    let (id2, token2) = invite(&http, &w.org, &w.admin.cookie, &email, "viewer").await;
    assert_ne!(token, token2);

    // 过期：查询、接受 404，列表里没有，撤销 404
    sqlx::query("UPDATE invitations SET expires_at = now() - interval '1 hour' WHERE id = $1")
        .bind(id2)
        .execute(&pg)
        .await
        .unwrap();
    expect_err(
        &http.get(&invite_info_path(&token2)).send().await,
        404,
        "not_found",
    );
    expect_err(
        &http
            .post(
                &accept_path(&token2),
                &json!({"password": "Some-password-123456"}),
            )
            .send()
            .await,
        404,
        "not_found",
    );
    let list = get_members(&http, &w.org, &w.owner.cookie).await;
    assert_eq!(list["invitations"].as_array().unwrap().len(), 0);
    expect_err(
        &http
            .delete(&format!("{}/{id2}", invitations_path(&w.org)))
            .cookie(&w.admin.cookie)
            .send()
            .await,
        404,
        "not_found",
    );
    // 过期之后重新邀请同一个邮箱：成功（旧的过期邀请被软删除，不会撞部分唯一索引）
    let (id3, token3) = invite(&http, &w.org, &w.admin.cookie, &email, "editor").await;
    assert_ne!(id3, id2);
    let deleted: bool =
        sqlx::query_scalar("SELECT deleted_at IS NOT NULL FROM invitations WHERE id = $1")
            .bind(id2)
            .fetch_one(&pg)
            .await
            .unwrap();
    assert!(deleted, "过期的待处理邀请要被清掉");
    let v = expect(&http.get(&invite_info_path(&token3)).send().await, 200);
    assert_eq!(v["role"], "editor");

    // 组织已删除：404
    sqlx::query("UPDATE organizations SET deleted_at = now() WHERE id = $1")
        .bind(w.org.id)
        .execute(&pg)
        .await
        .unwrap();
    expect_err(
        &http.get(&invite_info_path(&token3)).send().await,
        404,
        "not_found",
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn invitations_pages_are_rate_limited_per_ip() {
    let Some(env) = test_env("invitations_rate_limit") else {
        return;
    };
    migrate_once(&env).await;
    let pg = rivu_storage_pg::connect(&env.pg_url, 6).await.unwrap();
    let limits = DashLimits {
        view_per_min: 3,
        view_burst: 3,
        ..loose()
    };
    let (http, _) = start(&env, &pg, limits).await;
    let token = "x".repeat(43);
    for _ in 0..3 {
        expect_err(
            &http
                .get(&invite_info_path(&token))
                .ip("198.51.100.7")
                .send()
                .await,
            404,
            "not_found",
        );
    }
    let r = http
        .get(&invite_info_path(&token))
        .ip("198.51.100.7")
        .send()
        .await;
    expect_err(&r, 429, "rate_limited");
    assert!(
        r.headers
            .iter()
            .any(|(k, _)| k.eq_ignore_ascii_case("retry-after"))
    );
    // 接受端点共用同一个桶；别的 IP 不受影响
    expect_err(
        &http
            .post(&accept_path(&token), &json!({}))
            .ip("198.51.100.7")
            .send()
            .await,
        429,
        "rate_limited",
    );
    expect_err(
        &http
            .get(&invite_info_path(&token))
            .ip("198.51.100.8")
            .send()
            .await,
        404,
        "not_found",
    );
}

// ------------------------------------------------------------------ 站点访客

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn guests_invite_accept_and_remove() {
    let Some(env) = test_env("guests") else {
        return;
    };
    migrate_once(&env).await;
    let pg = rivu_storage_pg::connect(&env.pg_url, 6).await.unwrap();
    let (http, _) = start(&env, &pg, loose()).await;
    let w = world(&pg, &http).await;
    let site = make_site(&pg, &w.org, "Docs").await;
    let other_org = make_org(&pg).await;
    let foreign = make_site(&pg, &other_org, "Foreign").await;
    let email = format!("{}@example.test", unique("guest"));
    let guest_invite = |actor: &Actor, body: Value| {
        let http = http.clone();
        let cookie = actor.cookie.clone();
        let path = invitations_path(&w.org);
        async move { http.post(&path, &body).cookie(&cookie).send().await }
    };

    // 角色：viewer 403；别的组织的站点与不存在的站点 404；取值不对 400
    expect_err(
        &guest_invite(
            &w.viewer,
            json!({"email": email, "site_id": site.id, "site_role": "viewer"}),
        )
        .await,
        403,
        "forbidden",
    );
    for site_id in [foreign.id as i64, i64::MAX] {
        expect_err(
            &guest_invite(
                &w.admin,
                json!({"email": email, "site_id": site_id, "site_role": "viewer"}),
            )
            .await,
            404,
            "not_found",
        );
    }
    expect_err(
        &guest_invite(
            &w.admin,
            json!({"email": email, "site_id": site.id, "site_role": "admin"}),
        )
        .await,
        400,
        "invalid_request",
    );
    // 已是组织成员：409
    expect_err(
        &guest_invite(
            &w.admin,
            json!({"email": w.viewer.email, "site_id": site.id, "site_role": "viewer"}),
        )
        .await,
        409,
        "conflict",
    );

    // 邀请 → 查询 → 接受（新用户）
    let v = expect(
        &guest_invite(
            &w.admin,
            json!({"email": email, "site_id": site.id, "site_role": "editor"}),
        )
        .await,
        201,
    );
    assert_eq!(v["invitation"]["role"], Value::Null);
    assert_eq!(v["invitation"]["site"]["id"], site.id);
    assert_eq!(v["invitation"]["site"]["name"], "Docs");
    assert_eq!(v["invitation"]["site_role"], "editor");
    let token = token_of(v["url"].as_str().unwrap());
    // 同一个邮箱同一个站点再邀请：409
    expect_err(
        &guest_invite(
            &w.admin,
            json!({"email": email, "site_id": site.id, "site_role": "viewer"}),
        )
        .await,
        409,
        "conflict",
    );
    let info = expect(&http.get(&invite_info_path(&token)).send().await, 200);
    assert_eq!(info["role"], Value::Null);
    assert_eq!(info["site"]["name"], "Docs");
    assert_eq!(info["site_role"], "editor");
    let r = http
        .post(
            &accept_path(&token),
            &json!({"password": "Guest-secret-password-9"}),
        )
        .send()
        .await;
    let v = expect(&r, 200);
    assert_eq!(v["organization_id"], w.org.id);
    assert_eq!(v["site_id"], site.id);
    let guest_cookie = r.cookie_pair("rivu_session").unwrap();
    // 访客能看到这个站点，但不是组织成员（看不到成员页、不能建 key）
    let sites = expect(
        &http
            .get("/api/internal/sites")
            .cookie(&guest_cookie)
            .send()
            .await,
        200,
    );
    assert_eq!(sites["sites"][0]["access"], "guest");
    assert_eq!(sites["sites"][0]["role"], "editor");
    expect_err(
        &http
            .get(&members_path(&w.org))
            .cookie(&guest_cookie)
            .send()
            .await,
        404,
        "not_found",
    );
    expect_err(
        &http
            .post(&keys_path(&w.org), &json!({"name": "nope"}))
            .cookie(&guest_cookie)
            .send()
            .await,
        404,
        "not_found",
    );
    // 已是该站点访客再被邀请：409
    expect_err(
        &guest_invite(
            &w.admin,
            json!({"email": email, "site_id": site.id, "site_role": "viewer"}),
        )
        .await,
        409,
        "conflict",
    );

    // 列表里有他
    let list = get_members(&http, &w.org, &w.viewer.cookie).await;
    let guests = list["guests"].as_array().unwrap();
    assert_eq!(guests.len(), 1);
    assert_eq!(guests[0]["email"], email);
    assert_eq!(guests[0]["site"]["id"], site.id);
    let guest_id = guests[0]["id"].as_i64().unwrap();

    // 移除：viewer 403、别的组织 404、admin 204、再移除 404；他随即看不到站点
    let path = |org: &Org, id: i64| format!("/api/internal/orgs/{}/guests/{id}", org.id);
    expect_err(
        &http
            .delete(&path(&w.org, guest_id))
            .cookie(&w.viewer.cookie)
            .send()
            .await,
        403,
        "forbidden",
    );
    expect_err(
        &http
            .delete(&path(&other_org, guest_id))
            .cookie(&w.outsider.cookie)
            .send()
            .await,
        404,
        "not_found",
    );
    expect(
        &http
            .delete(&path(&w.org, guest_id))
            .cookie(&w.admin.cookie)
            .send()
            .await,
        204,
    );
    expect_err(
        &http
            .delete(&path(&w.org, guest_id))
            .cookie(&w.admin.cookie)
            .send()
            .await,
        404,
        "not_found",
    );
    let sites = expect(
        &http
            .get("/api/internal/sites")
            .cookie(&guest_cookie)
            .send()
            .await,
        200,
    );
    assert_eq!(sites["sites"].as_array().unwrap().len(), 0);

    // 站点删除后：待处理的访客邀请 404，也不再出现在列表里
    let email2 = format!("{}@example.test", unique("guest"));
    let v = expect(
        &guest_invite(
            &w.admin,
            json!({"email": email2, "site_id": site.id, "site_role": "viewer"}),
        )
        .await,
        201,
    );
    let token2 = token_of(v["url"].as_str().unwrap());
    rivu_storage_pg::sites::soft_delete_site(&pg, site.id)
        .await
        .unwrap();
    expect_err(
        &http.get(&invite_info_path(&token2)).send().await,
        404,
        "not_found",
    );
    expect_err(
        &http
            .post(
                &accept_path(&token2),
                &json!({"password": "Guest-secret-password-9"}),
            )
            .send()
            .await,
        404,
        "not_found",
    );
    let list = get_members(&http, &w.org, &w.owner.cookie).await;
    assert_eq!(list["invitations"].as_array().unwrap().len(), 0);
}

// ------------------------------------------------------------------ API key

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn api_keys_create_list_and_revoke_own_keys() {
    let Some(env) = test_env("api_keys") else {
        return;
    };
    migrate_once(&env).await;
    let pg = rivu_storage_pg::connect(&env.pg_url, 6).await.unwrap();
    let (http, cfg) = start(&env, &pg, loose()).await;
    let w = world(&pg, &http).await;
    let docs = make_site(&pg, &w.org, "Docs").await;
    let blog = make_site(&pg, &w.org, "Blog").await;
    let other_org = make_org(&pg).await;
    let foreign = make_site(&pg, &other_org, "Foreign").await;
    let create = |actor: &Actor, body: Value| {
        let http = http.clone();
        let cookie = actor.cookie.clone();
        let path = keys_path(&w.org);
        async move { http.post(&path, &body).cookie(&cookie).send().await }
    };

    // 没有会话 401；非成员 404
    expect_err(
        &http.get(&keys_path(&w.org)).send().await,
        401,
        "unauthorized",
    );
    expect_err(
        &http
            .get(&keys_path(&w.org))
            .cookie(&w.outsider.cookie)
            .send()
            .await,
        404,
        "not_found",
    );
    expect_err(
        &create(&w.outsider, json!({"name": "x"})).await,
        404,
        "not_found",
    );

    // 校验：名称、站点、过期天数
    for bad in [
        json!({"name": "   "}),
        json!({"name": "x".repeat(101)}),
        json!({"name": "k", "sites": []}),
        json!({"name": "k", "sites": [foreign.key]}),
        json!({"name": "k", "sites": [docs.key, foreign.key]}),
        json!({"name": "k", "sites": ["abcdefghijkl"]}),
        json!({"name": "k", "expires_in_days": 7}),
        json!({"name": "k", "expires_in_days": 0}),
    ] {
        expect_err(&create(&w.viewer, bad).await, 400, "invalid_request");
    }
    // 站点已删除的也算无效
    let gone = make_site(&pg, &w.org, "Gone").await;
    rivu_storage_pg::sites::soft_delete_site(&pg, gone.id)
        .await
        .unwrap();
    expect_err(
        &create(&w.viewer, json!({"name": "k", "sites": [gone.key]})).await,
        400,
        "invalid_request",
    );

    // 全部站点（viewer 也可以有 key）
    let v = expect(
        &create(&w.viewer, json!({"name": "  BI export  "})).await,
        201,
    );
    let all_key = v["key"].as_str().unwrap().to_owned();
    assert!(all_key.starts_with("rivu_") && all_key.len() == 48);
    let item = &v["item"];
    assert_eq!(item["name"], "BI export");
    assert_eq!(item["scopes"], json!(["stats:read:*"]));
    assert_eq!(item["sites"], json!([]));
    assert_eq!(item["key_prefix"], &all_key[..12]);
    assert_eq!(item["expires_at"], Value::Null);
    assert_eq!(item["last_used_at"], Value::Null);
    // 创建出来的 key 能被鉴权查到（身份是创建者在这个组织里的角色，权限限定在 scope）
    let rec = rivu_storage_pg::api_keys::find_active_by_hash(&pg, &hash_key(&all_key))
        .await
        .unwrap()
        .expect("新 key 有效");
    assert_eq!(rec.organization_id, w.org.id);
    assert_eq!(rec.user_id, w.viewer.id);
    assert_eq!(rec.role, "viewer");
    assert_eq!(rec.scopes, ["stats:read:*"]);

    // 指定站点（去重）与 30 天过期
    let v = expect(
        &create(
            &w.viewer,
            json!({"name": "two sites", "sites": [docs.key, blog.key, docs.key], "expires_in_days": 30}),
        )
        .await,
        201,
    );
    let item = &v["item"];
    assert_eq!(
        item["scopes"],
        json!([
            format!("stats:read:{}", docs.key),
            format!("stats:read:{}", blog.key)
        ])
    );
    assert_eq!(
        item["sites"],
        json!([
            {"site_key": docs.key, "name": "Docs"},
            {"site_key": blog.key, "name": "Blog"}
        ])
    );
    let days: f64 = sqlx::query_scalar(
        "SELECT (extract(epoch FROM (expires_at - now())) / 86400.0)::double precision FROM api_keys WHERE id = $1",
    )
    .bind(item["id"].as_i64().unwrap())
    .fetch_one(&pg)
    .await
    .unwrap();
    assert!((29.9..30.1).contains(&days), "约 30 天后过期：{days}");
    let picked_id = item["id"].as_i64().unwrap();
    let picked_key = v["key"].as_str().unwrap().to_owned();
    let rec = rivu_storage_pg::api_keys::find_active_by_hash(&pg, &hash_key(&picked_key))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(rec.scopes.len(), 2);

    // 列表：只有自己的，新的在前，带限额；明文与哈希不出现
    let owner_made = expect(&create(&w.owner, json!({"name": "owner's"})).await, 201);
    let owner_key_id = owner_made["item"]["id"].as_i64().unwrap();
    let r = http
        .get(&keys_path(&w.org))
        .cookie(&w.viewer.cookie)
        .send()
        .await;
    let v = expect(&r, 200);
    let keys = v["keys"].as_array().unwrap();
    assert_eq!(keys.len(), 2);
    assert_eq!(keys[0]["id"], picked_id);
    assert_eq!(keys[1]["name"], "BI export");
    assert!(keys.iter().all(|k| k["id"] != owner_key_id));
    assert!(!r.body.contains(&all_key) && !r.body.contains(&picked_key));
    assert_eq!(v["limits"]["hourly"], cfg.api_rate.hourly_default);
    assert_eq!(
        v["limits"]["burst"],
        cfg.api_rate.burst.min(cfg.api_rate.hourly_default)
    );
    assert_eq!(v["limits"]["concurrency"], cfg.api_rate.max_concurrency);
    // 组织自己的小时配额优先，突发不超过它
    sqlx::query("UPDATE organizations SET api_hourly_limit = 30 WHERE id = $1")
        .bind(w.org.id)
        .execute(&pg)
        .await
        .unwrap();
    let v = expect(
        &http
            .get(&keys_path(&w.org))
            .cookie(&w.viewer.cookie)
            .send()
            .await,
        200,
    );
    assert_eq!(v["limits"]["hourly"], 30);
    assert_eq!(v["limits"]["burst"], cfg.api_rate.burst.min(30));
    // 已过期的 key 仍然列出（由页面标注）
    sqlx::query("UPDATE api_keys SET expires_at = now() - interval '1 day' WHERE id = $1")
        .bind(picked_id)
        .execute(&pg)
        .await
        .unwrap();
    let v = expect(
        &http
            .get(&keys_path(&w.org))
            .cookie(&w.viewer.cookie)
            .send()
            .await,
        200,
    );
    assert_eq!(v["keys"].as_array().unwrap().len(), 2);

    // 吊销别人的 key：404（key 仍然有效）；吊销自己的：204，之后查不到，再吊销 404
    let key_path = |id: i64| format!("{}/{id}", keys_path(&w.org));
    expect_err(
        &http
            .delete(&key_path(owner_key_id))
            .cookie(&w.viewer.cookie)
            .send()
            .await,
        404,
        "not_found",
    );
    // 管理员也不能吊销别人的 key（只管自己的）
    expect_err(
        &http
            .delete(&key_path(owner_key_id))
            .cookie(&w.admin.cookie)
            .send()
            .await,
        404,
        "not_found",
    );
    let owner_hash = hash_key(owner_made["key"].as_str().unwrap());
    assert!(
        rivu_storage_pg::api_keys::find_active_by_hash(&pg, &owner_hash)
            .await
            .unwrap()
            .is_some()
    );
    let own_id = v["keys"]
        .as_array()
        .unwrap()
        .iter()
        .find(|k| k["name"] == "BI export")
        .unwrap()["id"]
        .as_i64()
        .unwrap();
    expect(
        &http
            .delete(&key_path(own_id))
            .cookie(&w.viewer.cookie)
            .send()
            .await,
        204,
    );
    assert!(
        rivu_storage_pg::api_keys::find_active_by_hash(&pg, &hash_key(&all_key))
            .await
            .unwrap()
            .is_none()
    );
    expect_err(
        &http
            .delete(&key_path(own_id))
            .cookie(&w.viewer.cookie)
            .send()
            .await,
        404,
        "not_found",
    );
    expect_err(
        &http
            .delete(&format!("{}/abc", keys_path(&w.org)))
            .cookie(&w.viewer.cookie)
            .send()
            .await,
        404,
        "not_found",
    );
    // 别的组织里同 id 的路径也不行（key 属于哪个组织看库，不看路径）
    let other_member = actor(&pg, &http, Some((&other_org, "viewer"))).await;
    expect_err(
        &http
            .delete(&format!(
                "/api/internal/orgs/{}/api-keys/{owner_key_id}",
                other_org.id
            ))
            .cookie(&other_member.cookie)
            .send()
            .await,
        404,
        "not_found",
    );
}

// ------------------------------------------------------------------ CSRF

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn csrf_rejects_mutations_without_origin() {
    let Some(env) = test_env("org_admin_csrf") else {
        return;
    };
    migrate_once(&env).await;
    let pg = rivu_storage_pg::connect(&env.pg_url, 6).await.unwrap();
    let (http, _) = start(&env, &pg, loose()).await;
    let w = world(&pg, &http).await;
    let (inv_id, token) = invite(
        &http,
        &w.org,
        &w.owner.cookie,
        &format!("{}@example.test", unique("csrf")),
        "viewer",
    )
    .await;
    let made = expect(
        &http
            .post(&keys_path(&w.org), &json!({"name": "k"}))
            .cookie(&w.owner.cookie)
            .send()
            .await,
        201,
    );
    let key_id = made["item"]["id"].as_i64().unwrap();

    let key_path = format!("{}/{key_id}", keys_path(&w.org));
    let inv_path = format!("{}/{inv_id}", invitations_path(&w.org));
    let requests = [
        http.post(
            &invitations_path(&w.org),
            &json!({"email": "x@example.test", "role": "viewer"}),
        ),
        http.patch(
            &member_path(&w.org, w.viewer.id),
            &json!({"role": "editor"}),
        ),
        http.delete(&member_path(&w.org, w.viewer.id)),
        http.delete(&inv_path),
        http.delete(&format!("/api/internal/orgs/{}/guests/1", w.org.id)),
        http.post(&keys_path(&w.org), &json!({"name": "k2"})),
        http.delete(&key_path),
        http.post(
            &accept_path(&token),
            &json!({"password": "Some-password-123456"}),
        ),
    ];
    for rb in requests {
        let (method, path) = (rb.method, rb.path.clone());
        // 缺 Origin
        let r = rb.remove("Origin").cookie(&w.owner.cookie).send().await;
        let m = format!("{method} {path} 缺 Origin");
        assert_eq!(r.status, 403, "{m}：{}", r.body);
        assert_eq!(r.json()["error"], "csrf_rejected", "{m}");
    }
    // 跨站的 Origin 同样被拒
    let r = http
        .post(
            &invitations_path(&w.org),
            &json!({"email": "x@example.test", "role": "viewer"}),
        )
        .header("Origin", "https://evil.example")
        .cookie(&w.owner.cookie)
        .send()
        .await;
    expect_err(&r, 403, "csrf_rejected");
    // 什么都没有发生
    let list = get_members(&http, &w.org, &w.owner.cookie).await;
    assert_eq!(
        member_role_in(&list, w.viewer.id).as_deref(),
        Some("viewer")
    );
    assert_eq!(list["invitations"].as_array().unwrap().len(), 1);
    expect(&http.get(&invite_info_path(&token)).send().await, 200);
    // 读请求不要求 Origin
    expect(
        &http
            .get(&keys_path(&w.org))
            .cookie(&w.owner.cookie)
            .send()
            .await,
        200,
    );
}
