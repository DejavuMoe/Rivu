//! 保存的分群（`/api/internal/sites/{site}/segments`）与分群展开的端到端测试：真实的 PostgreSQL 与 ClickHouse，
//! HTTP 走真实的本地 TCP 端口（与 `annotations.rs`、`consolidated.rs` 同一套装配，另外挂上公开查询 API）。
//!
//! 缺少环境变量时跳过并打印原因（同 `internal_api.rs`）：
//!   RIVU_TEST_DATABASE_URL     有 DDL 与写权限的 PostgreSQL 连接串（rivu_migrator）
//!   RIVU_TEST_CLICKHOUSE_URL   ClickHouse HTTP 地址
//! 可选：RIVU_TEST_CH_{MIGRATOR,WRITER,READER}_{USER,PASSWORD}、RIVU_TEST_CH_DATABASE。
//!
//! 用例：
//! - `segment_crud_and_validation`：增删改查、名称与过滤的校验（错误带字段路径）、规范化、重名、别的站点的 id；
//! - `segment_limit_per_site`：每站 100 个，删掉一个后又能加；
//! - `segment_permissions_for_every_role`：组织四种角色、站点访客两种角色、别的站点的访客、非成员、没有会话、
//!   跨站请求、站点删除；
//! - `shared_links_bind_segments_and_block_their_deletion`：建链接时绑定分群（只认本站点未删除的分群）、列表带分群、
//!   有未吊销的链接（含已过期的）时不能删分群，吊销后能删；
//! - `segment_queries_equal_inlined_filters`：看板查询、共享链接、公开查询 API 与 `rivu query` 里的分群，
//!   结果与把分群的过滤直接写进查询完全相同；`is_not`、`or` 里的分群；别的站点的、已删除的分群 400；改了分群立即生效。

use std::collections::HashMap;
use std::net::SocketAddr;
use std::process::Stdio;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use clickhouse::Client;
use jiff::Timestamp;
use rivu::apikey_cmd::issue_key;
use rivu::cli::ApikeyCreateArgs;
use rivu::dash_api::{self, DashApi, DashLimits};
use rivu::query_api::{self, QueryApi};
use rivu::{api_auth, migrate, public};
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

/// 看板的内部端点加上公开查询 API（与 `rivu serve` 一样合并在同一个路由里），固定「现在」。
async fn start(env: &TestEnv, pg: &PgPool) -> Http {
    let cfg = config(env);
    let reader: Client =
        rivu_storage_ch::client(&env.ch_url, &account("reader"), Some(&ch_database()));
    let now: Timestamp = "2026-11-20T12:00:00Z".parse().unwrap();
    let dash = DashApi::new(&cfg, pg.clone(), reader.clone(), None)
        .expect("装配内部端点")
        .with_limits(loose())
        .with_clock(move || now);
    let guard = api_auth::build_guard(&cfg, pg.clone());
    let api = QueryApi::new(guard, pg.clone(), reader, None, cfg.query_timeouts.api_secs)
        .with_clock(move || now);
    let routes = dash_api::router(Arc::new(dash)).merge(query_api::router(Arc::new(api)));
    let app = public::router_full(None, None, Some(routes));
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

async fn make_org(pg: &PgPool) -> (i64, String) {
    let slug = unique("org");
    let id =
        sqlx::query_scalar("INSERT INTO organizations (name, slug) VALUES ($1, $1) RETURNING id")
            .bind(&slug)
            .fetch_one(pg)
            .await
            .unwrap();
    (id, slug)
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

async fn make_site(pg: &PgPool, org_id: i64) -> (u64, String) {
    let hostnames = vec![format!("{}.example.test", unique("site"))];
    let name = hostnames[0].clone();
    let created = rivu_storage_pg::sites::create_site_in_org(
        pg,
        org_id,
        &NewSite {
            name: &name,
            timezone: "UTC",
            hostnames: &hostnames,
        },
    )
    .await
    .expect("建站");
    (created.id, created.site_key)
}

fn expect(r: &Resp, status: u16) -> Value {
    assert_eq!(r.status, status, "响应：{}", r.body);
    if r.body.trim().is_empty() {
        Value::Null
    } else {
        r.json()
    }
}

fn expect_err(r: &Resp, status: u16, code: &str) -> Value {
    let v = expect(r, status);
    assert_eq!(v["error"], code, "响应：{}", r.body);
    v
}

/// 400 `invalid_query`，第一条错误的字段路径。
fn expect_path(r: &Resp, path: &str) {
    let v = expect_err(r, 400, "invalid_query");
    assert_eq!(v["errors"][0]["path"], path, "响应：{}", r.body);
}

fn list_path(site: u64) -> String {
    format!("/api/internal/sites/{site}/segments")
}

fn item_path(site: u64, id: impl std::fmt::Display) -> String {
    format!("/api/internal/sites/{site}/segments/{id}")
}

fn names(v: &Value) -> Vec<String> {
    v["segments"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["name"].as_str().unwrap().to_owned())
        .collect()
}

async fn add(http: &Http, actor: &Actor, site: u64, name: &str, filters: Value) -> i64 {
    let r = http
        .post(&list_path(site), &json!({"name": name, "filters": filters}))
        .cookie(&actor.cookie)
        .send()
        .await;
    expect(&r, 201)["segment"]["id"].as_i64().unwrap()
}

// ------------------------------------------------------------------ 用例

#[tokio::test]
async fn segment_crud_and_validation() {
    let Some(env) = test_env("segment_crud_and_validation") else {
        return;
    };
    let pg = pg(&env).await;
    let http = start(&env, &pg).await;
    let (org, _) = make_org(&pg).await;
    let owner = user(&pg, &http, Some((org, "owner"))).await;
    let (site, _) = make_site(&pg, org).await;
    let (other_site, _) = make_site(&pg, org).await;
    let lp = list_path(site);

    let v = expect(&http.get(&lp).cookie(&owner.cookie).send().await, 200);
    assert_eq!(v, json!({"segments": [], "max_segments": 100}));

    // 添加：名称 trim；过滤按查询的语法规范化（整数取值变成字符串，大小写选项保留），原样存进库里
    let r = http
        .post(
            &lp,
            &json!({"name": "  Mobile from search  ", "filters": [
                ["is", "visit:device", ["Mobile"]],
                ["or", [["is", "visit:source", ["Google"]], ["contains", "visit:referrer", ["bing"], {"case_sensitive": false}]]],
                ["is", "visit:city", [2950159]]
            ]}),
        )
        .cookie(&owner.cookie)
        .send()
        .await;
    let s = expect(&r, 201)["segment"].clone();
    let normalized = json!([
        ["is", "visit:device", ["Mobile"]],
        ["or", [["is", "visit:source", ["Google"]], ["contains", "visit:referrer", ["bing"], {"case_sensitive": false}]]],
        ["is", "visit:city", ["2950159"]]
    ]);
    assert_eq!(s["name"], "Mobile from search");
    assert_eq!(s["filters"], normalized);
    assert!(s["created_at"].as_str().unwrap().ends_with('Z'));
    assert!(s["updated_at"].as_str().unwrap().ends_with('Z'));
    let first = s["id"].as_i64().unwrap();
    let stored: String =
        sqlx::query_scalar::<_, String>("SELECT filter::text FROM segments WHERE id = $1")
            .bind(first)
            .fetch_one(&pg)
            .await
            .unwrap();
    assert_eq!(serde_json::from_str::<Value>(&stored).unwrap(), normalized);
    let owner_col: Option<i64> =
        sqlx::query_scalar("SELECT owner_user_id FROM segments WHERE id = $1")
            .bind(first)
            .fetch_one(&pg)
            .await
            .unwrap();
    assert_eq!(owner_col, None, "分群按站点共享");
    add(
        &http,
        &owner,
        site,
        "德国访客",
        json!([["is", "visit:country", ["DE"]]]),
    )
    .await;
    add(
        &http,
        &owner,
        site,
        "blog readers",
        json!([["contains", "event:page", ["/blog"]]]),
    )
    .await;

    // 列表按名称排（不区分大小写）
    let v = expect(&http.get(&lp).cookie(&owner.cookie).send().await, 200);
    assert_eq!(
        names(&v),
        ["blog readers", "Mobile from search", "德国访客"]
    );

    // 重名（不区分大小写，trim 之后）
    expect_err(
        &http
            .post(&lp, &json!({"name": " MOBILE FROM SEARCH", "filters": [["is", "visit:country", ["SE"]]]}))
            .cookie(&owner.cookie)
            .send()
            .await,
        409,
        "conflict",
    );
    // 别的站点可以同名
    add(
        &http,
        &owner,
        other_site,
        "Mobile from search",
        json!([["is", "visit:country", ["SE"]]]),
    )
    .await;

    // 名称的校验
    for bad in [
        json!(""),
        json!("   "),
        json!("x".repeat(101)),
        json!("a\nb"),
        json!(7),
    ] {
        let r = http
            .post(
                &lp,
                &json!({"name": bad, "filters": [["is", "visit:country", ["SE"]]]}),
            )
            .cookie(&owner.cookie)
            .send()
            .await;
        assert_eq!(r.status, 400, "{bad}: {}", r.body);
    }
    // 过滤的校验：与查询同一个解析器与规则，错误带字段路径
    for (filters, path) in [
        (json!([]), "filters"),
        (json!("visit:country"), "filters"),
        (json!([["is", "visit:nope", ["x"]]]), "filters[0][1]"),
        (json!([["is", "visit:country", []]]), "filters[0][2]"),
        (json!([["is", "time:day", ["2026-11-01"]]]), "filters[0][1]"),
        (json!([["is", "segment", [first]]]), "filters[0][1]"),
        (
            json!([["not", ["is", "segment", [first]]]]),
            "filters[0][1][1]",
        ),
        (
            json!([["has_done", ["is", "event:name", ["Signup"]]]]),
            "filters[0][0]",
        ),
        (
            json!([
                ["is", "visit:country", ["DE"]],
                ["matches", "event:page", ["("]]
            ]),
            "filters[1][2][0]",
        ),
        (
            json!([
                ["is", "event:goal", ["x"]],
                ["contains", "event:goal", ["x"]]
            ]),
            "filters[1][0]",
        ),
    ] {
        let r = http
            .post(&lp, &json!({"name": unique("bad"), "filters": filters}))
            .cookie(&owner.cookie)
            .send()
            .await;
        expect_path(&r, path);
    }
    for body in [
        json!({"name": "no filters"}),
        json!({"filters": [["is", "visit:country", ["SE"]]]}),
        json!({"name": "x", "filters": [["is", "visit:country", ["SE"]]], "owner": 1}),
    ] {
        let r = http.post(&lp, &body).cookie(&owner.cookie).send().await;
        assert_eq!(r.status, 400, "{body}: {}", r.body);
    }

    // 修改：只改名、只改过滤；空请求 400；大小写不同的同一名称可以改；与别的分群重名 409
    let ip = item_path(site, first);
    let v = expect(
        &http
            .patch(&ip, &json!({"name": "Mobile visitors from search"}))
            .cookie(&owner.cookie)
            .send()
            .await,
        200,
    );
    assert_eq!(v["segment"]["name"], "Mobile visitors from search");
    assert_eq!(v["segment"]["filters"], normalized);
    let v = expect(
        &http
            .patch(
                &ip,
                &json!({"filters": [["is", "visit:device", ["Tablet"]]]}),
            )
            .cookie(&owner.cookie)
            .send()
            .await,
        200,
    );
    assert_eq!(v["segment"]["name"], "Mobile visitors from search");
    assert_eq!(
        v["segment"]["filters"],
        json!([["is", "visit:device", ["Tablet"]]])
    );
    expect(
        &http
            .patch(&ip, &json!({"name": "MOBILE visitors from search"}))
            .cookie(&owner.cookie)
            .send()
            .await,
        200,
    );
    expect_err(
        &http
            .patch(&ip, &json!({"name": "德国访客"}))
            .cookie(&owner.cookie)
            .send()
            .await,
        409,
        "conflict",
    );
    expect_err(
        &http
            .patch(&ip, &json!({}))
            .cookie(&owner.cookie)
            .send()
            .await,
        400,
        "invalid_request",
    );
    expect_path(
        &http
            .patch(&ip, &json!({"filters": [["is", "segment", [first]]]}))
            .cookie(&owner.cookie)
            .send()
            .await,
        "filters[0][1]",
    );
    // 不存在的、别的站点的、不是整数的 id：404
    let foreign = add(
        &http,
        &owner,
        other_site,
        "foreign",
        json!([["is", "visit:country", ["SE"]]]),
    )
    .await;
    for id in [foreign.to_string(), "999999999".into(), "abc".into()] {
        expect_err(
            &http
                .patch(&item_path(site, &id), &json!({"name": "x"}))
                .cookie(&owner.cookie)
                .send()
                .await,
            404,
            "not_found",
        );
        expect_err(
            &http
                .delete(&item_path(site, &id))
                .cookie(&owner.cookie)
                .send()
                .await,
            404,
            "not_found",
        );
    }

    // 删除：204，之后改、删都是 404；名称可以再用
    let r = http.delete(&ip).cookie(&owner.cookie).send().await;
    assert_eq!(r.status, 204, "{}", r.body);
    expect_err(
        &http.delete(&ip).cookie(&owner.cookie).send().await,
        404,
        "not_found",
    );
    expect_err(
        &http
            .patch(&ip, &json!({"name": "again"}))
            .cookie(&owner.cookie)
            .send()
            .await,
        404,
        "not_found",
    );
    add(
        &http,
        &owner,
        site,
        "mobile visitors from search",
        json!([["is", "visit:country", ["SE"]]]),
    )
    .await;
    let v = expect(&http.get(&lp).cookie(&owner.cookie).send().await, 200);
    assert_eq!(
        names(&v),
        ["blog readers", "mobile visitors from search", "德国访客"]
    );
}

#[tokio::test]
async fn segment_limit_per_site() {
    let Some(env) = test_env("segment_limit_per_site") else {
        return;
    };
    let pg = pg(&env).await;
    let http = start(&env, &pg).await;
    let (org, _) = make_org(&pg).await;
    let owner = user(&pg, &http, Some((org, "owner"))).await;
    let (site, _) = make_site(&pg, org).await;
    // 99 个直接写库，第 100 个走接口
    sqlx::query(
        "INSERT INTO segments (site_id, name, filter)
         SELECT $1, 'seg ' || g, '[[\"is\",\"visit:country\",[\"DE\"]]]'::jsonb FROM generate_series(1, 99) g",
    )
    .bind(site as i64)
    .execute(&pg)
    .await
    .unwrap();
    // 已删除的分群不算
    sqlx::query(
        "INSERT INTO segments (site_id, name, filter, deleted_at)
         VALUES ($1, 'gone', '[[\"is\",\"visit:country\",[\"DE\"]]]'::jsonb, now())",
    )
    .bind(site as i64)
    .execute(&pg)
    .await
    .unwrap();
    let last = add(
        &http,
        &owner,
        site,
        "the 100th",
        json!([["is", "visit:country", ["SE"]]]),
    )
    .await;
    let r = http
        .post(
            &list_path(site),
            &json!({"name": "one too many", "filters": [["is", "visit:country", ["SE"]]]}),
        )
        .cookie(&owner.cookie)
        .send()
        .await;
    expect_err(&r, 409, "limit_reached");
    let v = expect(
        &http
            .get(&list_path(site))
            .cookie(&owner.cookie)
            .send()
            .await,
        200,
    );
    assert_eq!(v["segments"].as_array().unwrap().len(), 100);
    assert_eq!(v["max_segments"], 100);
    let r = http
        .delete(&item_path(site, last))
        .cookie(&owner.cookie)
        .send()
        .await;
    assert_eq!(r.status, 204);
    add(
        &http,
        &owner,
        site,
        "one too many",
        json!([["is", "visit:country", ["SE"]]]),
    )
    .await;
}

#[tokio::test]
async fn segment_permissions_for_every_role() {
    let Some(env) = test_env("segment_permissions_for_every_role") else {
        return;
    };
    let pg = pg(&env).await;
    let http = start(&env, &pg).await;
    let (org, _) = make_org(&pg).await;
    let (other_org, _) = make_org(&pg).await;
    let (site, _) = make_site(&pg, org).await;
    let (sibling, _) = make_site(&pg, org).await;
    let owner = user(&pg, &http, Some((org, "owner"))).await;
    let admin = user(&pg, &http, Some((org, "admin"))).await;
    let editor = user(&pg, &http, Some((org, "editor"))).await;
    let viewer = user(&pg, &http, Some((org, "viewer"))).await;
    let guest_editor = guest(&pg, &http, site, "editor").await;
    let guest_viewer = guest(&pg, &http, site, "viewer").await;
    let sibling_guest = guest(&pg, &http, sibling, "editor").await;
    let outsider = user(&pg, &http, Some((other_org, "owner"))).await;
    let seed = add(
        &http,
        &owner,
        site,
        "seed",
        json!([["is", "visit:country", ["DE"]]]),
    )
    .await;

    // (角色, 能读, 能写, 看不见站点)
    let cases: [(&str, &Actor, bool, bool); 8] = [
        ("owner", &owner, true, true),
        ("admin", &admin, true, true),
        ("editor", &editor, true, true),
        ("viewer", &viewer, true, false),
        ("guest editor", &guest_editor, true, true),
        ("guest viewer", &guest_viewer, true, false),
        ("guest of another site", &sibling_guest, false, false),
        ("member of another org", &outsider, false, false),
    ];
    for (label, actor, can_read, can_write) in cases {
        let r = http
            .get(&list_path(site))
            .cookie(&actor.cookie)
            .send()
            .await;
        if can_read {
            let v = expect(&r, 200);
            assert!(
                v["segments"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|s| s["id"] == seed),
                "{label}"
            );
        } else {
            expect_err(&r, 404, "not_found");
        }
        let post = http
            .post(
                &list_path(site),
                &json!({"name": format!("by {label}"), "filters": [["is", "visit:country", ["SE"]]]}),
            )
            .cookie(&actor.cookie)
            .send()
            .await;
        let patch = http
            .patch(
                &item_path(site, seed),
                &json!({"name": format!("seed renamed by {label}")}),
            )
            .cookie(&actor.cookie)
            .send()
            .await;
        let victim = add(
            &http,
            &owner,
            site,
            &format!("victim for {label}"),
            json!([["is", "visit:country", ["SE"]]]),
        )
        .await;
        let delete = http
            .delete(&item_path(site, victim))
            .cookie(&actor.cookie)
            .send()
            .await;
        match (can_read, can_write) {
            (_, true) => {
                assert_eq!(post.status, 201, "{label}: {}", post.body);
                assert_eq!(patch.status, 200, "{label}: {}", patch.body);
                assert_eq!(delete.status, 204, "{label}: {}", delete.body);
            }
            (true, false) => {
                for r in [&post, &patch, &delete] {
                    expect_err(r, 403, "forbidden");
                }
            }
            (false, false) => {
                for r in [&post, &patch, &delete] {
                    expect_err(r, 404, "not_found");
                }
            }
        }
    }

    // 没有会话 401
    for r in [
        http.get(&list_path(site)).send().await,
        http.post(
            &list_path(site),
            &json!({"name": "x", "filters": [["is", "visit:country", ["SE"]]]}),
        )
        .send()
        .await,
        http.patch(&item_path(site, seed), &json!({"name": "x"}))
            .send()
            .await,
        http.delete(&item_path(site, seed)).send().await,
    ] {
        assert_eq!(r.status, 401, "{}", r.body);
    }
    // 跨站请求（Origin 不对）：403，写不进去
    let r = http
        .post(
            &list_path(site),
            &json!({"name": "csrf", "filters": [["is", "visit:country", ["SE"]]]}),
        )
        .header("Origin", "https://evil.example")
        .cookie(&owner.cookie)
        .send()
        .await;
    assert_eq!(r.status, 403, "{}", r.body);
    let v = expect(
        &http
            .get(&list_path(site))
            .cookie(&owner.cookie)
            .send()
            .await,
        200,
    );
    assert!(!names(&v).contains(&"csrf".to_owned()));

    // 站点删除：所有人 404；恢复后分群原样回来
    sqlx::query("UPDATE sites SET deleted_at = now() WHERE id = $1")
        .bind(site as i64)
        .execute(&pg)
        .await
        .unwrap();
    expect_err(
        &http
            .get(&list_path(site))
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
    let v = expect(
        &http
            .get(&list_path(site))
            .cookie(&owner.cookie)
            .send()
            .await,
        200,
    );
    assert!(names(&v).iter().any(|n| n.starts_with("seed renamed by")));
}

fn links_path(site: u64) -> String {
    format!("/api/internal/sites/{site}/shared-links")
}

#[tokio::test]
async fn shared_links_bind_segments_and_block_their_deletion() {
    let Some(env) = test_env("shared_links_bind_segments_and_block_their_deletion") else {
        return;
    };
    let pg = pg(&env).await;
    let http = start(&env, &pg).await;
    let (org, _) = make_org(&pg).await;
    let owner = user(&pg, &http, Some((org, "owner"))).await;
    let (site, _) = make_site(&pg, org).await;
    let (other_site, _) = make_site(&pg, org).await;
    let seg = add(
        &http,
        &owner,
        site,
        "Sweden",
        json!([["is", "visit:country", ["SE"]]]),
    )
    .await;
    let foreign = add(
        &http,
        &owner,
        other_site,
        "Foreign",
        json!([["is", "visit:country", ["SE"]]]),
    )
    .await;
    let gone = add(
        &http,
        &owner,
        site,
        "Gone",
        json!([["is", "visit:country", ["SE"]]]),
    )
    .await;
    assert_eq!(
        http.delete(&item_path(site, gone))
            .cookie(&owner.cookie)
            .send()
            .await
            .status,
        204
    );

    // 绑定分群建链接；响应与列表都带分群的 id 和名称，没绑定的是 null
    let r = http
        .post(
            &links_path(site),
            &json!({"name": "Board", "segment_id": seg}),
        )
        .cookie(&owner.cookie)
        .send()
        .await;
    let link = expect(&r, 201)["link"].clone();
    assert_eq!(link["segment"], json!({"id": seg, "name": "Sweden"}));
    let bound = link["id"].as_i64().unwrap();
    let r = http
        .post(
            &links_path(site),
            &json!({"segment_id": seg, "expires_at": "2099-01-01T00:00:00Z"}),
        )
        .cookie(&owner.cookie)
        .send()
        .await;
    let expiring = expect(&r, 201)["link"]["id"].as_i64().unwrap();
    let r = http
        .post(&links_path(site), &json!({}))
        .cookie(&owner.cookie)
        .send()
        .await;
    let plain = expect(&r, 201)["link"].clone();
    assert_eq!(plain["segment"], Value::Null);
    // 别的站点的、已删除的、不存在的分群：400，链接没建出来
    for bad in [foreign, gone, 999_999_999] {
        let r = http
            .post(&links_path(site), &json!({"segment_id": bad}))
            .cookie(&owner.cookie)
            .send()
            .await;
        expect_err(&r, 400, "invalid_request");
    }
    let v = expect(
        &http
            .get(&links_path(site))
            .cookie(&owner.cookie)
            .send()
            .await,
        200,
    );
    let links = v["links"].as_array().unwrap();
    assert_eq!(links.len(), 3, "{v}");
    let by_id = |id: i64| links.iter().find(|l| l["id"] == id).unwrap().clone();
    assert_eq!(
        by_id(bound)["segment"],
        json!({"id": seg, "name": "Sweden"})
    );
    assert_eq!(by_id(plain["id"].as_i64().unwrap())["segment"], Value::Null);
    // 改名后列表跟着变
    expect(
        &http
            .patch(&item_path(site, seg), &json!({"name": "Swedish visitors"}))
            .cookie(&owner.cookie)
            .send()
            .await,
        200,
    );
    let v = expect(
        &http
            .get(&links_path(site))
            .cookie(&owner.cookie)
            .send()
            .await,
        200,
    );
    let renamed = v["links"]
        .as_array()
        .unwrap()
        .iter()
        .find(|l| l["id"] == bound)
        .unwrap()["segment"]
        .clone();
    assert_eq!(renamed, json!({"id": seg, "name": "Swedish visitors"}));

    // 有链接在用：409 segment_in_use，带链接的 id 和名称；已过期但没吊销的链接也算
    sqlx::query("UPDATE shared_links SET expires_at = now() - interval '1 day' WHERE id = $1")
        .bind(expiring)
        .execute(&pg)
        .await
        .unwrap();
    let r = http
        .delete(&item_path(site, seg))
        .cookie(&owner.cookie)
        .send()
        .await;
    let v = expect_err(&r, 409, "segment_in_use");
    assert_eq!(
        v["links"],
        json!([{"id": bound, "name": "Board"}, {"id": expiring, "name": null}])
    );
    // 吊销一个还不够；两个都吊销之后能删
    let r = http
        .delete(&format!("{}/{bound}", links_path(site)))
        .cookie(&owner.cookie)
        .send()
        .await;
    assert_eq!(r.status, 204, "{}", r.body);
    let v = expect_err(
        &http
            .delete(&item_path(site, seg))
            .cookie(&owner.cookie)
            .send()
            .await,
        409,
        "segment_in_use",
    );
    assert_eq!(v["links"], json!([{"id": expiring, "name": null}]));
    let r = http
        .delete(&format!("{}/{expiring}", links_path(site)))
        .cookie(&owner.cookie)
        .send()
        .await;
    assert_eq!(r.status, 204, "{}", r.body);
    let r = http
        .delete(&item_path(site, seg))
        .cookie(&owner.cookie)
        .send()
        .await;
    assert_eq!(r.status, 204, "{}", r.body);
}

// ------------------------------------------------------------------ 查询里的分群

/// 一个会话：`(开始时刻 UTC, 国家, 来源, 依次浏览的页面)`；每个会话一个访客。
type Visit = (&'static str, [u8; 2], &'static str, &'static [&'static str]);

fn visit_rows(site: u64, session_id: u64, (start, country, source, pages): Visit) -> Vec<EventRow> {
    let secs = start.parse::<Timestamp>().expect("时刻").as_second();
    let base = OffsetDateTime::from_unix_timestamp(secs).expect("时刻");
    pages
        .iter()
        .enumerate()
        .map(|(i, page)| EventRow {
            site_id: site,
            ts: base + time::Duration::seconds(30 * i as i64),
            event_id: Uuid::now_v7(),
            name: "pageview".into(),
            visitor_id: session_id ^ 0x5eed,
            session_id,
            session_start: base,
            ingest_partition: 0,
            hostname: "example.test".into(),
            pathname: (*page).into(),
            entry_pathname: pages[0].into(),
            referrer: String::new(),
            referrer_source: source.into(),
            channel: String::new(),
            utm_source: String::new(),
            utm_medium: String::new(),
            utm_campaign: String::new(),
            utm_content: String::new(),
            utm_term: String::new(),
            click_id_param: String::new(),
            country_code: country,
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
    insert_events(&writer, &rows, &format!("segments-it:{site}"), dedup)
        .await
        .expect("写入 events");
}

/// 结果里除了请求回显的过滤与站点之外的部分（分群的请求回显 `["is","segment",[id]]`，展开后的请求回显展开后的过滤）。
fn without_filters_echo(mut v: Value) -> Value {
    let q = v["query"].as_object_mut().expect("结果带 query");
    q.remove("filters");
    // 公开 API 的回显带 `site`（必填），看板的可以省略
    q.remove("site");
    v
}

fn visitors(v: &Value) -> u64 {
    v["results"][0]["metrics"][0].as_u64().unwrap_or(0)
}

/// `rivu query`（真正的二进制）：spec 从 stdin 读，结果打印在 stdout。
async fn rivu_query(env: &TestEnv, spec: &Value) -> (bool, String, String) {
    let reader = account("reader");
    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_rivu"))
        .kill_on_drop(true)
        .arg("query")
        .env("RIVU_DATABASE_URL", &env.pg_url)
        .env("RIVU_CLICKHOUSE_URL", &env.ch_url)
        .env("RIVU_CH_DATABASE", ch_database())
        .env("RIVU_CH_READER_USER", reader.user)
        .env("RIVU_CH_READER_PASSWORD", reader.password.expose())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("启动 rivu query");
    let mut stdin = child.stdin.take().unwrap();
    stdin.write_all(spec.to_string().as_bytes()).await.unwrap();
    drop(stdin);
    let out = tokio::time::timeout(Duration::from_secs(30), child.wait_with_output())
        .await
        .expect("rivu query 要退出")
        .unwrap();
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn segment_queries_equal_inlined_filters() {
    let Some(env) = test_env("segment_queries_equal_inlined_filters") else {
        return;
    };
    let pg = pg(&env).await;
    let http = start(&env, &pg).await;
    let (org, org_slug) = make_org(&pg).await;
    let owner = user(&pg, &http, Some((org, "owner"))).await;
    let viewer = user(&pg, &http, Some((org, "viewer"))).await;
    let (site, key) = make_site(&pg, org).await;
    let (other_site, other_key) = make_site(&pg, org).await;
    // 5 个会话；分群「来自搜索、看过 /pricing 的德国访客」命中 1 和 5
    seed(
        &env,
        site,
        &[
            ("2026-11-10T10:00:00Z", *b"DE", "Google", &["/", "/pricing"]),
            ("2026-11-10T11:00:00Z", *b"DE", "Google", &["/blog"]),
            ("2026-11-10T12:00:00Z", *b"SE", "Google", &["/pricing"]),
            ("2026-11-11T09:00:00Z", *b"DE", "", &["/pricing"]),
            (
                "2026-11-12T09:00:00Z",
                *b"DE",
                "Bing",
                &["/pricing", "/signup"],
            ),
        ],
    )
    .await;
    seed(
        &env,
        other_site,
        &[("2026-11-10T10:00:00Z", *b"DE", "Google", &["/pricing"])],
    )
    .await;
    let seg_filters = json!([
        ["is", "visit:country", ["DE"]],
        ["is", "visit:source", ["Google", "Bing"]],
        ["contains", "event:page", ["/pricing"]]
    ]);
    let seg = add(
        &http,
        &owner,
        site,
        "German searchers on pricing",
        seg_filters.clone(),
    )
    .await;
    let foreign = add(
        &http,
        &owner,
        other_site,
        "Foreign",
        json!([["is", "visit:country", ["DE"]]]),
    )
    .await;
    let inlined = json!(["and", seg_filters]);

    let range = json!(["2026-11-01", "2026-11-19"]);
    let shapes = [
        json!({"metrics": ["visitors", "visits", "pageviews", "bounce_rate", "visit_duration"], "date_range": range}),
        json!({"metrics": ["visitors", "pageviews"], "dimensions": ["visit:source"], "date_range": range}),
        json!({"metrics": ["pageviews", "visitors"], "dimensions": ["event:page"], "date_range": range}),
        json!({"metrics": ["visitors"], "dimensions": ["time:day"], "date_range": range, "include": {"time_labels": true}}),
    ];
    let pairs = [
        (json!([["is", "segment", [seg]]]), json!([inlined.clone()])),
        (
            json!([["is", "visit:country", ["DE"]], ["is", "segment", [seg]]]),
            json!([["is", "visit:country", ["DE"]], inlined.clone()]),
        ),
        (
            json!([["is_not", "segment", [seg]]]),
            json!([["not", inlined.clone()]]),
        ),
        (
            json!([[
                "or",
                [["is", "segment", [seg]], ["is", "visit:country", ["SE"]]]
            ]]),
            json!([["or", [inlined.clone(), ["is", "visit:country", ["SE"]]]]]),
        ),
    ];
    let q = |site_id: u64| format!("/api/internal/sites/{site_id}/query");
    let api_key = issue_key(
        &pg,
        &ApikeyCreateArgs {
            org: org_slug.clone(),
            user: Some(owner.email.clone()),
            name: "segments-e2e".into(),
            scopes: vec!["stats:read".into()],
            expires: None,
        },
    )
    .await
    .expect("创建 key")
    .plaintext;
    let api = |spec: Value| {
        http.post(query_api::QUERY_PATH, &spec)
            .header("Authorization", &format!("Bearer {api_key}"))
    };

    for shape in &shapes {
        for (with_segment, with_filters) in &pairs {
            let mut a = shape.clone();
            a["filters"] = with_segment.clone();
            let mut b = shape.clone();
            b["filters"] = with_filters.clone();
            // 看板（viewer 也能应用分群）
            let ra = http.post(&q(site), &a).cookie(&viewer.cookie).send().await;
            let rb = http.post(&q(site), &b).cookie(&viewer.cookie).send().await;
            let (va, vb) = (expect(&ra, 200), expect(&rb, 200));
            assert_eq!(
                va["query"]["filters"],
                stringify_ints(with_segment.clone()),
                "回显保留分群引用"
            );
            assert_eq!(
                without_filters_echo(va.clone()),
                without_filters_echo(vb.clone()),
                "{a}"
            );
            // 公开查询 API
            let mut a_api = a.clone();
            a_api["site"] = json!(key);
            let mut b_api = b.clone();
            b_api["site"] = json!(key);
            let (pa, pb) = (api(a_api.clone()).send().await, api(b_api).send().await);
            let (pa, pb) = (expect(&pa, 200), expect(&pb, 200));
            assert_eq!(
                without_filters_echo(pa.clone()),
                without_filters_echo(pb),
                "{a_api}"
            );
            assert_eq!(
                without_filters_echo(pa.clone()),
                without_filters_echo(va.clone()),
                "{a_api}"
            );
        }
    }
    // 结果确实被过滤了：分群里是会话 1 和 5，两个访客
    let mut overview = shapes[0].clone();
    overview["filters"] = json!([["is", "segment", [seg]]]);
    let v = expect(
        &http
            .post(&q(site), &overview)
            .cookie(&owner.cookie)
            .send()
            .await,
        200,
    );
    assert_eq!(visitors(&v), 2, "{v}");
    // `is_not` 就是 `["not", 分群的过滤]`：分群里有事件级条件（这里是页面），取反也按事件判定，
    // 所以 1 和 5 浏览的其他页面仍然算（5 个访客都有不匹配的事件），与直接写 `not` 相同
    overview["filters"] = json!([["is_not", "segment", [seg]]]);
    let v = expect(
        &http
            .post(&q(site), &overview)
            .cookie(&owner.cookie)
            .send()
            .await,
        200,
    );
    assert_eq!(visitors(&v), 5, "{v}");
    // 只有会话级条件的分群，取反就是「不在分群里的访客」
    let sessions_only = add(
        &http,
        &owner,
        site,
        "German searchers",
        json!([
            ["is", "visit:country", ["DE"]],
            ["is", "visit:source", ["Google", "Bing"]]
        ]),
    )
    .await;
    overview["filters"] = json!([["is_not", "segment", [sessions_only]]]);
    let v = expect(
        &http
            .post(&q(site), &overview)
            .cookie(&owner.cookie)
            .send()
            .await,
        200,
    );
    assert_eq!(visitors(&v), 2, "{v}");

    // `rivu query`：与公开 API 同一个结果
    let mut cli_spec = shapes[0].clone();
    cli_spec["site"] = json!(key);
    cli_spec["filters"] = json!([["is", "segment", [seg]]]);
    let (ok, stdout, stderr) = rivu_query(&env, &cli_spec).await;
    assert!(ok, "rivu query 失败：{stderr}");
    let cli: Value = serde_json::from_str(&stdout).unwrap_or_else(|e| panic!("{e}: {stdout}"));
    let via_api = expect(&api(cli_spec.clone()).send().await, 200);
    assert_eq!(
        without_filters_echo(cli.clone()),
        without_filters_echo(via_api)
    );
    assert_eq!(visitors(&cli), 2);
    // 别的站点的分群：`rivu query` 失败，消息说明是未知分群
    cli_spec["filters"] = json!([["is", "segment", [foreign]]]);
    let (ok, _, stderr) = rivu_query(&env, &cli_spec).await;
    assert!(!ok);
    assert!(stderr.contains("unknown segment"), "{stderr}");

    // 别的站点的分群、不存在的分群：400，路径指向那个 id（看板与公开 API 一样）
    for bad in [foreign, 999_999_999] {
        let mut s = shapes[0].clone();
        s["filters"] = json!([
            ["is", "visit:country", ["DE"]],
            ["is", "segment", [seg, bad]]
        ]);
        expect_path(
            &http.post(&q(site), &s).cookie(&owner.cookie).send().await,
            "filters[1][2][1]",
        );
        s["site"] = json!(key);
        expect_path(&api(s).send().await, "filters[1][2][1]");
    }
    // 分群属于被查询的站点才认：查别的站点时引用本站点的分群 400
    let mut s = shapes[0].clone();
    s["site"] = json!(other_key);
    s["filters"] = json!([["is", "segment", [seg]]]);
    expect_path(&api(s).send().await, "filters[0][2][0]");

    // 共享链接绑定分群：通过链接看到的结果与看板里应用分群相同
    let r = http
        .post(&links_path(site), &json!({"segment_id": seg}))
        .cookie(&owner.cookie)
        .send()
        .await;
    let slug = expect(&r, 201)["link"]["slug"].as_str().unwrap().to_owned();
    let mut plain = shapes[1].clone();
    plain["filters"] = json!([]);
    let mut applied = shapes[1].clone();
    applied["filters"] = json!([["is", "segment", [seg]]]);
    let shared = expect(&http.post(&q(site), &plain).share(&slug).send().await, 200);
    let session = expect(
        &http
            .post(&q(site), &applied)
            .cookie(&owner.cookie)
            .send()
            .await,
        200,
    );
    assert_eq!(
        shared, session,
        "共享链接与看板走同一条展开路径，连回显都相同"
    );
    assert_eq!(
        shared["query"]["filters"],
        json!([["is", "segment", [seg.to_string()]]])
    );

    // 改了分群的过滤：下一次查询立即生效（每次查询现读，不缓存）
    expect(
        &http
            .patch(
                &item_path(site, seg),
                &json!({"filters": [["is", "visit:country", ["SE"]]]}),
            )
            .cookie(&owner.cookie)
            .send()
            .await,
        200,
    );
    overview["filters"] = json!([["is", "segment", [seg]]]);
    let v = expect(
        &http
            .post(&q(site), &overview)
            .cookie(&owner.cookie)
            .send()
            .await,
        200,
    );
    assert_eq!(visitors(&v), 1, "{v}");
    let mut via_link = shapes[0].clone();
    via_link["filters"] = json!([]);
    let v = expect(
        &http.post(&q(site), &via_link).share(&slug).send().await,
        200,
    );
    assert_eq!(visitors(&v), 1, "{v}");

    // 分群被删（绕过接口改库）：看板里引用它 400；绑定它的链接 404（fail-closed）
    sqlx::query("UPDATE segments SET deleted_at = now() WHERE id = $1")
        .bind(seg)
        .execute(&pg)
        .await
        .unwrap();
    expect_path(
        &http
            .post(&q(site), &overview)
            .cookie(&owner.cookie)
            .send()
            .await,
        "filters[0][2][0]",
    );
    assert_eq!(
        http.post(&q(site), &via_link)
            .share(&slug)
            .send()
            .await
            .status,
        404
    );
}

/// 回显是规范化后的过滤：整数取值变成字符串。
fn stringify_ints(v: Value) -> Value {
    match v {
        Value::Number(n) => Value::String(n.to_string()),
        Value::Array(a) => Value::Array(a.into_iter().map(stringify_ints).collect()),
        other => other,
    }
}
