//! 图表注释端点（`/api/internal/sites/{site}/annotations`）的端到端测试：真实的 PostgreSQL，HTTP 走真实的本地 TCP 端口
//! （与 `site_lifecycle.rs` 同一套装配）。
//!
//! 缺少环境变量时跳过并打印原因（同 `internal_api.rs`）：
//!   RIVU_TEST_DATABASE_URL     有 DDL 与写权限的 PostgreSQL 连接串（rivu_migrator）
//!   RIVU_TEST_CLICKHOUSE_URL   ClickHouse HTTP 地址（迁移与公开看板的查询要用）
//! 可选：RIVU_TEST_CH_{MIGRATOR,READER}_{USER,PASSWORD}、RIVU_TEST_CH_DATABASE。
//!
//! 用例：
//! - `annotation_crud_and_validation`：增删改查、按日期筛选、文本与日期的校验、别的站点的 id；
//! - `annotation_limit_per_site`：每站 500 条，删掉一条后又能加；
//! - `annotation_permissions_for_every_role`：组织四种角色、站点访客两种角色、别的站点的访客、非成员、没有会话；
//!   作者信息只按成员页的可见范围给出；站点删除后 404，恢复后注释原样回来；
//! - `public_and_shared_dashboards_do_not_return_annotations`：公开站点与共享链接调不到注释，入口信息与查询结果里也没有。

use std::net::SocketAddr;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use clickhouse::Client;
use jiff::Timestamp;
use rivu::dash_api::{self, DashApi, DashLimits};
use rivu::{migrate, public};
use rivu_auth::password::hash_blocking;
use rivu_common::config::{ChAccount, Config, Env, Role, Secret};
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

    fn patch(&self, path: &str, body: &Value) -> Rb<'_> {
        self.json_rb("PATCH", path, body)
    }

    fn delete(&self, path: &str) -> Rb<'_> {
        self.rb("DELETE", path, Vec::new())
            .header("Content-Type", "application/json")
            .header("Origin", &self.origin)
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
    email: String,
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
        email,
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

async fn site_key(pg: &PgPool, site: u64) -> String {
    sqlx::query_scalar("SELECT site_key FROM sites WHERE id = $1")
        .bind(site as i64)
        .fetch_one(pg)
        .await
        .unwrap()
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

fn list_path(site: u64) -> String {
    format!("/api/internal/sites/{site}/annotations")
}

fn item_path(site: u64, id: impl std::fmt::Display) -> String {
    format!("/api/internal/sites/{site}/annotations/{id}")
}

fn texts(v: &Value) -> Vec<String> {
    v["annotations"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["text"].as_str().unwrap().to_owned())
        .collect()
}

// ------------------------------------------------------------------ 用例

#[tokio::test]
async fn annotation_crud_and_validation() {
    let Some(env) = test_env("annotation_crud_and_validation") else {
        return;
    };
    let pg = pg(&env).await;
    let http = start(&env, &pg).await;
    let org = make_org(&pg).await;
    let owner = user(&pg, &http, Some((org, "owner"))).await;
    let site = make_site(&pg, org, "Asia/Shanghai").await;
    let other_site = make_site(&pg, org, "UTC").await;
    let lp = list_path(site);

    // 空列表
    let v = expect(&http.get(&lp).cookie(&owner.cookie).send().await, 200);
    assert_eq!(v, json!({"annotations": [], "max_annotations": 500}));

    // 添加：trim；响应带作者、mine、时间
    let r = http
        .post(
            &lp,
            &json!({"date": "2026-11-02", "text": "  Released v2.0  "}),
        )
        .cookie(&owner.cookie)
        .send()
        .await;
    let a = expect(&r, 201)["annotation"].clone();
    assert_eq!(a["date"], "2026-11-02");
    assert_eq!(a["text"], "Released v2.0");
    assert_eq!(a["mine"], true);
    assert_eq!(a["author"]["email"], owner.email);
    assert_eq!(a["author"]["name"], "Test User");
    assert!(a["created_at"].as_str().unwrap().ends_with('Z'));
    let first = a["id"].as_i64().unwrap();
    for (date, text) in [("2026-10-15", "发布 v1.9"), ("2026-11-10", "Blog post")] {
        expect(
            &http
                .post(&lp, &json!({"date": date, "text": text}))
                .cookie(&owner.cookie)
                .send()
                .await,
            201,
        );
    }

    // 列表按日期排；from、to 含两端
    let v = expect(&http.get(&lp).cookie(&owner.cookie).send().await, 200);
    assert_eq!(texts(&v), ["发布 v1.9", "Released v2.0", "Blog post"]);
    let v = expect(
        &http
            .get(&format!("{lp}?from=2026-11-02&to=2026-11-10"))
            .cookie(&owner.cookie)
            .send()
            .await,
        200,
    );
    assert_eq!(texts(&v), ["Released v2.0", "Blog post"]);
    let v = expect(
        &http
            .get(&format!("{lp}?to=2026-11-01"))
            .cookie(&owner.cookie)
            .send()
            .await,
        200,
    );
    assert_eq!(texts(&v), ["发布 v1.9"]);
    for q in [
        "from=2026-11-10&to=2026-11-02",
        "from=2026-13-01",
        "to=yesterday",
    ] {
        expect_err(
            &http
                .get(&format!("{lp}?{q}"))
                .cookie(&owner.cookie)
                .send()
                .await,
            400,
            "invalid_request",
        );
    }

    // 修改：只改文本、只改日期、两样都改；空请求 400
    let ip = item_path(site, first);
    let v = expect(
        &http
            .patch(&ip, &json!({"text": "Released v2.0.1"}))
            .cookie(&owner.cookie)
            .send()
            .await,
        200,
    );
    assert_eq!(v["annotation"]["text"], "Released v2.0.1");
    assert_eq!(v["annotation"]["date"], "2026-11-02");
    let v = expect(
        &http
            .patch(&ip, &json!({"date": "2026-11-03"}))
            .cookie(&owner.cookie)
            .send()
            .await,
        200,
    );
    assert_eq!(v["annotation"]["text"], "Released v2.0.1");
    assert_eq!(v["annotation"]["date"], "2026-11-03");
    let v = expect(
        &http
            .patch(&ip, &json!({"date": "2026-11-04", "text": "v2"}))
            .cookie(&owner.cookie)
            .send()
            .await,
        200,
    );
    assert_eq!(v["annotation"]["text"], "v2");
    assert_eq!(v["annotation"]["date"], "2026-11-04");
    expect_err(
        &http
            .patch(&ip, &json!({}))
            .cookie(&owner.cookie)
            .send()
            .await,
        400,
        "invalid_request",
    );

    // 取值校验（添加与修改同一套）
    let long = "x".repeat(256);
    let bad_bodies = [
        json!({"date": "2026-11-02", "text": ""}),
        json!({"date": "2026-11-02", "text": "   "}),
        json!({"date": "2026-11-02", "text": long}),
        json!({"date": "2026-11-02", "text": "line\nbreak"}),
        json!({"date": "2026-11-02", "text": "bell\u{7}"}),
        json!({"date": "2026-11-31", "text": "ok"}),
        json!({"date": "2026-11-02T00:00:00Z", "text": "ok"}),
        json!({"date": "1999-12-31", "text": "ok"}),
        json!({"date": "2026-11-02"}),
        json!({"date": "2026-11-02", "text": "ok", "site_id": 1}),
    ];
    for body in &bad_bodies {
        expect_err(
            &http.post(&lp, body).cookie(&owner.cookie).send().await,
            400,
            "invalid_request",
        );
    }
    for body in [
        json!({"text": ""}),
        json!({"text": "x".repeat(256)}),
        json!({"date": "2026-02-30"}),
        json!({"color": "red"}),
    ] {
        expect_err(
            &http.patch(&ip, &body).cookie(&owner.cookie).send().await,
            400,
            "invalid_request",
        );
    }
    // 255 个字符（不是字节）可以
    expect(
        &http
            .post(
                &lp,
                &json!({"date": "2026-11-05", "text": "注".repeat(255)}),
            )
            .cookie(&owner.cookie)
            .send()
            .await,
        201,
    );

    // 别的站点、不存在、不是整数的 id：404
    for path in [
        item_path(other_site, first),
        item_path(site, 999_999_999),
        item_path(site, "abc"),
    ] {
        expect_err(
            &http
                .patch(&path, &json!({"text": "x"}))
                .cookie(&owner.cookie)
                .send()
                .await,
            404,
            "not_found",
        );
        expect_err(
            &http.delete(&path).cookie(&owner.cookie).send().await,
            404,
            "not_found",
        );
    }

    // 删除：204，再删 404，列表里没有了
    expect(&http.delete(&ip).cookie(&owner.cookie).send().await, 204);
    expect_err(
        &http.delete(&ip).cookie(&owner.cookie).send().await,
        404,
        "not_found",
    );
    let v = expect(&http.get(&lp).cookie(&owner.cookie).send().await, 200);
    assert!(!texts(&v).contains(&"v2".to_owned()));

    // 其他方法 405
    let r = http
        .patch(&lp, &json!({}))
        .cookie(&owner.cookie)
        .send()
        .await;
    assert_eq!(r.status, 405);
}

#[tokio::test]
async fn annotation_limit_per_site() {
    let Some(env) = test_env("annotation_limit_per_site") else {
        return;
    };
    let pg = pg(&env).await;
    let http = start(&env, &pg).await;
    let org = make_org(&pg).await;
    let editor = user(&pg, &http, Some((org, "editor"))).await;
    let site = make_site(&pg, org, "UTC").await;
    let other = make_site(&pg, org, "UTC").await;
    // 499 条直接写库，再加一条软删除的（不计数）
    sqlx::query(
        "INSERT INTO annotations (site_id, date, text)
         SELECT $1, DATE '2026-01-01' + (n % 300), 'note ' || n FROM generate_series(1, 499) n",
    )
    .bind(site as i64)
    .execute(&pg)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO annotations (site_id, date, text, deleted_at) VALUES ($1, '2026-01-01', 'gone', now())",
    )
    .bind(site as i64)
    .execute(&pg)
    .await
    .unwrap();
    let lp = list_path(site);
    let body = json!({"date": "2026-11-20", "text": "the 500th"});
    let v = expect(
        &http.post(&lp, &body).cookie(&editor.cookie).send().await,
        201,
    );
    let last = v["annotation"]["id"].as_i64().unwrap();
    expect_err(
        &http.post(&lp, &body).cookie(&editor.cookie).send().await,
        409,
        "limit_reached",
    );
    let v = expect(&http.get(&lp).cookie(&editor.cookie).send().await, 200);
    assert_eq!(v["annotations"].as_array().unwrap().len(), 500);
    // 上限按站点算
    expect(
        &http
            .post(&list_path(other), &body)
            .cookie(&editor.cookie)
            .send()
            .await,
        201,
    );
    // 删掉一条后又能加
    expect(
        &http
            .delete(&item_path(site, last))
            .cookie(&editor.cookie)
            .send()
            .await,
        204,
    );
    expect(
        &http.post(&lp, &body).cookie(&editor.cookie).send().await,
        201,
    );
}

#[tokio::test]
async fn annotation_permissions_for_every_role() {
    let Some(env) = test_env("annotation_permissions_for_every_role") else {
        return;
    };
    let pg = pg(&env).await;
    let http = start(&env, &pg).await;
    let org = make_org(&pg).await;
    let other_org = make_org(&pg).await;
    let site = make_site(&pg, org, "UTC").await;
    let sibling = make_site(&pg, org, "UTC").await;
    let owner = user(&pg, &http, Some((org, "owner"))).await;
    let admin = user(&pg, &http, Some((org, "admin"))).await;
    let editor = user(&pg, &http, Some((org, "editor"))).await;
    let viewer = user(&pg, &http, Some((org, "viewer"))).await;
    let guest_editor = guest(&pg, &http, site, "editor").await;
    let guest_viewer = guest(&pg, &http, site, "viewer").await;
    let sibling_guest = guest(&pg, &http, sibling, "editor").await;
    let outsider = user(&pg, &http, Some((other_org, "owner"))).await;
    let lp = list_path(site);

    // (名字, cookie, 列表状态, 写状态)
    let cases: [(&str, &Actor, u16, u16); 8] = [
        ("owner", &owner, 200, 201),
        ("admin", &admin, 200, 201),
        ("editor", &editor, 200, 201),
        ("viewer", &viewer, 200, 403),
        ("guest editor", &guest_editor, 200, 201),
        ("guest viewer", &guest_viewer, 200, 403),
        ("guest of another site", &sibling_guest, 404, 404),
        ("outsider", &outsider, 404, 404),
    ];
    let seed = expect(
        &http
            .post(&lp, &json!({"date": "2026-11-01", "text": "seed"}))
            .cookie(&owner.cookie)
            .send()
            .await,
        201,
    )["annotation"]["id"]
        .as_i64()
        .unwrap();
    for (name, a, list_status, write_status) in cases {
        let r = http.get(&lp).cookie(&a.cookie).send().await;
        assert_eq!(r.status, list_status, "{name} 列表：{}", r.body);
        let r = http
            .post(
                &lp,
                &json!({"date": "2026-11-02", "text": format!("by {name}")}),
            )
            .cookie(&a.cookie)
            .send()
            .await;
        assert_eq!(r.status, write_status, "{name} 添加：{}", r.body);
        let (patch_status, delete_status) = match write_status {
            201 => (200, 204),
            s => (s, s),
        };
        let mine = if write_status == 201 {
            r.json()["annotation"]["id"].as_i64().unwrap()
        } else {
            seed
        };
        // 改别人的（seed 是 owner 的）也按同一权限
        let r = http
            .patch(
                &item_path(site, seed),
                &json!({"text": format!("seed edited by {name}")}),
            )
            .cookie(&a.cookie)
            .send()
            .await;
        assert_eq!(r.status, patch_status, "{name} 修改：{}", r.body);
        let r = http
            .delete(&item_path(site, mine))
            .cookie(&a.cookie)
            .send()
            .await;
        assert_eq!(r.status, delete_status, "{name} 删除：{}", r.body);
        if write_status == 403 {
            assert_eq!(r.json()["error"], "forbidden");
        }
    }
    // 没有会话 401
    expect_err(&http.get(&lp).send().await, 401, "unauthorized");
    expect_err(
        &http
            .post(&lp, &json!({"date": "2026-11-02", "text": "anon"}))
            .send()
            .await,
        401,
        "unauthorized",
    );

    // 作者信息：组织成员看得到成员与访客的名称与邮箱；站点访客只看得到自己的
    let by_editor = expect(
        &http
            .post(&lp, &json!({"date": "2026-11-03", "text": "editor note"}))
            .cookie(&editor.cookie)
            .send()
            .await,
        201,
    )["annotation"]["id"]
        .as_i64()
        .unwrap();
    let by_guest = expect(
        &http
            .post(&lp, &json!({"date": "2026-11-04", "text": "guest note"}))
            .cookie(&guest_editor.cookie)
            .send()
            .await,
        201,
    )["annotation"]["id"]
        .as_i64()
        .unwrap();
    let find = |v: &Value, id: i64| {
        v["annotations"]
            .as_array()
            .unwrap()
            .iter()
            .find(|a| a["id"] == id)
            .cloned()
            .unwrap()
    };
    let as_viewer = expect(&http.get(&lp).cookie(&viewer.cookie).send().await, 200);
    assert_eq!(find(&as_viewer, by_editor)["author"]["email"], editor.email);
    assert_eq!(find(&as_viewer, by_editor)["mine"], false);
    assert_eq!(
        find(&as_viewer, by_guest)["author"]["email"],
        guest_editor.email
    );
    let as_guest = expect(
        &http.get(&lp).cookie(&guest_viewer.cookie).send().await,
        200,
    );
    assert_eq!(find(&as_guest, by_editor)["author"], Value::Null);
    assert_eq!(find(&as_guest, by_guest)["author"], Value::Null);
    let as_author = expect(
        &http.get(&lp).cookie(&guest_editor.cookie).send().await,
        200,
    );
    assert_eq!(find(&as_author, by_guest)["mine"], true);
    assert_eq!(
        find(&as_author, by_guest)["author"]["email"],
        guest_editor.email
    );
    // 作者离开组织后，成员页里没有他，注释也不再显示作者；注释本身保留
    sqlx::query(
        "UPDATE memberships SET deleted_at = now() WHERE organization_id = $1 AND user_id = $2",
    )
    .bind(org)
    .bind(editor.id)
    .execute(&pg)
    .await
    .unwrap();
    let v = expect(&http.get(&lp).cookie(&owner.cookie).send().await, 200);
    assert_eq!(find(&v, by_editor)["author"], Value::Null);
    assert_eq!(find(&v, by_editor)["text"], "editor note");
    // 账号硬删除时 created_by 置空
    sqlx::query("DELETE FROM auth_sessions WHERE user_id = $1")
        .bind(editor.id)
        .execute(&pg)
        .await
        .unwrap();
    sqlx::query("DELETE FROM memberships WHERE user_id = $1")
        .bind(editor.id)
        .execute(&pg)
        .await
        .unwrap();
    sqlx::query("DELETE FROM users WHERE id = $1")
        .bind(editor.id)
        .execute(&pg)
        .await
        .unwrap();
    let created_by: Option<i64> =
        sqlx::query_scalar("SELECT created_by FROM annotations WHERE id = $1")
            .bind(by_editor)
            .fetch_one(&pg)
            .await
            .unwrap();
    assert_eq!(created_by, None);

    // 站点删除后谁都 404；恢复后注释原样回来
    sqlx::query("UPDATE sites SET deleted_at = now() WHERE id = $1")
        .bind(site as i64)
        .execute(&pg)
        .await
        .unwrap();
    for a in [&owner, &guest_viewer] {
        expect_err(
            &http.get(&lp).cookie(&a.cookie).send().await,
            404,
            "not_found",
        );
    }
    expect_err(
        &http
            .post(&lp, &json!({"date": "2026-11-02", "text": "x"}))
            .cookie(&owner.cookie)
            .send()
            .await,
        404,
        "not_found",
    );
    sqlx::query("UPDATE sites SET deleted_at = NULL WHERE id = $1")
        .bind(site as i64)
        .execute(&pg)
        .await
        .unwrap();
    let v = expect(&http.get(&lp).cookie(&owner.cookie).send().await, 200);
    assert!(texts(&v).contains(&"guest note".to_owned()));
}

#[tokio::test]
async fn public_and_shared_dashboards_do_not_return_annotations() {
    let Some(env) = test_env("public_and_shared_dashboards_do_not_return_annotations") else {
        return;
    };
    let pg = pg(&env).await;
    let http = start(&env, &pg).await;
    let org = make_org(&pg).await;
    let other_org = make_org(&pg).await;
    let owner = user(&pg, &http, Some((org, "owner"))).await;
    let outsider = user(&pg, &http, Some((other_org, "owner"))).await;
    let site = make_site(&pg, org, "UTC").await;
    let key = site_key(&pg, site).await;
    sqlx::query("UPDATE sites SET is_public = true WHERE id = $1")
        .bind(site as i64)
        .execute(&pg)
        .await
        .unwrap();
    const SECRET: &str = "launch-note-should-stay-private";
    expect(
        &http
            .post(
                &list_path(site),
                &json!({"date": "2026-11-19", "text": SECRET}),
            )
            .cookie(&owner.cookie)
            .send()
            .await,
        201,
    );
    let link = expect(
        &http
            .post(
                &format!("/api/internal/sites/{site}/shared-links"),
                &json!({}),
            )
            .cookie(&owner.cookie)
            .send()
            .await,
        201,
    );
    let slug = link["link"]["slug"].as_str().unwrap().to_owned();

    // 公开站点：匿名（用 id 或 site_key）401；登录了但不是成员 404
    for seg in [site.to_string(), key.clone()] {
        let p = format!("/api/internal/sites/{seg}/annotations");
        expect_err(&http.get(&p).send().await, 401, "unauthorized");
        expect_err(
            &http.get(&p).cookie(&outsider.cookie).send().await,
            404,
            "not_found",
        );
        // 共享链接：带链接头也调不到
        expect_err(
            &http.get(&p).header("X-Rivu-Share", &slug).send().await,
            401,
            "unauthorized",
        );
    }

    // 入口信息与看板查询里都没有注释
    let bodies = [
        http.get(&format!("/api/internal/public/{key}"))
            .send()
            .await,
        http.get(&format!("/api/internal/shared/{slug}"))
            .send()
            .await,
        http.post(
            &format!("/api/internal/sites/{key}/query"),
            &json!({"metrics": ["visitors"], "date_range": "30d", "dimensions": ["time:day"],
                    "include": {"time_labels": true}}),
        )
        .send()
        .await,
        http.post(
            &format!("/api/internal/sites/{key}/query"),
            &json!({"metrics": ["visitors"], "date_range": "30d"}),
        )
        .header("X-Rivu-Share", &slug)
        .send()
        .await,
    ];
    for r in &bodies {
        assert_eq!(r.status, 200, "{}", r.body);
        assert!(!r.body.contains(SECRET), "{}", r.body);
        assert!(!r.body.contains("annotation"), "{}", r.body);
    }
}
