//! 站点删除的恢复窗口、恢复端点、按日期范围删除统计数据，以及 `ch_delete_batch` 的端到端测试：真实的 PostgreSQL
//! 与 ClickHouse，HTTP 部分走真实的本地 TCP 端口（与 `org_admin.rs` 同一套装配）。
//!
//! 缺少环境变量时跳过并打印原因（同 `internal_api.rs`）：
//!   RIVU_TEST_DATABASE_URL     有 DDL 与写权限的 PostgreSQL 连接串（rivu_migrator）
//!   RIVU_TEST_CLICKHOUSE_URL   ClickHouse HTTP 地址
//! 可选：RIVU_TEST_CH_{MIGRATOR,WRITER,READER,MAINTAINER}_{USER,PASSWORD}、RIVU_TEST_CH_DATABASE。
//!
//! 用例：
//! - `site_delete_and_restore`：权限（非成员 404、低角色 403）、删除写入意图、已删除列表、恢复与意图取消、
//!   恢复后再删除（部分唯一索引）、窗口已过或清除已开始时 409；
//! - `stats_deletion_requests`：日期范围的校验、权限、列表顺序；
//! - `plausible_imports_over_http`：上传导出、后台导入完成、带代码的失败、同一站点一次只跑一个导入、删除；
//! - `purge_deletes_exactly_the_targeted_rows`：两个站点、多个日期与分区的 events / sessions / ingest_counters，
//!   按日期范围删除（站点时区 +8，跨 UTC 月界）与整站删除只删目标行；`partitions_done` 断点续跑；重复执行幂等。

use std::collections::{BTreeMap, HashMap};
use std::net::SocketAddr;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use clickhouse::Client;
use jiff::civil::Date;
use jiff::tz::TimeZone;
use jiff::{Timestamp, ToSpan};
use rivu::dash_api::{self, DashApi, DashLimits};
use rivu::{migrate, public};
use rivu_auth::password::hash_blocking;
use rivu_common::config::{ChAccount, Config, Env, Role, Secret};
use rivu_storage_ch::rows::{EventRow, IngestCounterRow, insert_events, insert_ingest_counters};
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

    fn upload(&self, path: &str, body: Vec<u8>) -> Rb<'_> {
        self.rb("POST", path, body)
            .header("Content-Type", "application/zip")
            .header("Origin", &self.origin)
    }

    fn post(&self, path: &str, body: &Value) -> Rb<'_> {
        self.json_rb("POST", path, body)
    }

    fn delete_with(&self, path: &str, body: &Value) -> Rb<'_> {
        self.json_rb("DELETE", path, body)
    }
}

async fn start(env: &TestEnv, pg: &PgPool) -> Http {
    start_with_maintainer(env, pg, &env.ch_url).await
}

async fn start_with_maintainer(env: &TestEnv, pg: &PgPool, write_url: &str) -> Http {
    let cfg = config(env);
    let reader: Client =
        rivu_storage_ch::client(&env.ch_url, &account("reader"), Some(&ch_database()));
    let now: Timestamp = "2026-03-12T12:00:00Z".parse().unwrap();
    let dash = DashApi::new(&cfg, pg.clone(), reader, None)
        .expect("装配内部端点")
        .with_limits(loose())
        .with_maintainer(rivu_storage_ch::client(
            write_url,
            &account("maintainer"),
            Some(&ch_database()),
        ))
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

struct Actor {
    id: i64,
    cookie: String,
}

async fn actor(pg: &PgPool, http: &Http, org: &Org, role: &str) -> Actor {
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
    rivu_storage_pg::users::upsert_member(pg, org.id, id, role)
        .await
        .unwrap();
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

struct World {
    org: Org,
    owner: Actor,
    admin: Actor,
    editor: Actor,
    viewer: Actor,
    outsider: Actor,
}

async fn world(pg: &PgPool, http: &Http) -> World {
    let org = make_org(pg).await;
    let other = make_org(pg).await;
    World {
        owner: actor(pg, http, &org, "owner").await,
        admin: actor(pg, http, &org, "admin").await,
        editor: actor(pg, http, &org, "editor").await,
        viewer: actor(pg, http, &org, "viewer").await,
        outsider: actor(pg, http, &other, "owner").await,
        org,
    }
}

async fn make_site(pg: &PgPool, org_id: i64, name: &str, timezone: &str) -> u64 {
    let hostnames = vec!["example.test".to_owned(), "b.example.test".to_owned()];
    rivu_storage_pg::sites::create_site_in_org(
        pg,
        org_id,
        &NewSite {
            name,
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

/// 站点的站点删除意图（未取消的那条，或最新的一条）：(status, not_before 距现在的秒数, requested_by)。
async fn site_intent(pg: &PgPool, site_id: u64) -> Option<(String, f64, Option<i64>)> {
    sqlx::query_as(
        "SELECT status, EXTRACT(EPOCH FROM (not_before - now()))::float8, requested_by
           FROM pending_stats_deletions WHERE site_id = $1 AND scope = 'site'
          ORDER BY (status <> 'cancelled') DESC, id DESC LIMIT 1",
    )
    .bind(site_id as i64)
    .fetch_optional(pg)
    .await
    .unwrap()
}

fn listed(list: &Value, site_id: u64) -> Option<Value> {
    list["sites"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["id"] == site_id)
        .cloned()
}

// ------------------------------------------------------------------ 删除与恢复

#[tokio::test]
async fn site_delete_and_restore() {
    let Some(env) = test_env("site_delete_and_restore") else {
        return;
    };
    let pg = pg(&env).await;
    let http = start(&env, &pg).await;
    let w = world(&pg, &http).await;
    let name = unique("Site");
    let site = make_site(&pg, w.org.id, &name, "UTC").await;
    let site_path = format!("/api/internal/sites/{site}");
    let restore_path = format!("{site_path}/restore");
    let deleted_path = format!("/api/internal/orgs/{}/deleted-sites", w.org.id);

    // 删除：editor 403，outsider 404，名称不符 400
    let body = json!({"confirm": name});
    expect_err(
        &http
            .delete_with(&site_path, &body)
            .cookie(&w.editor.cookie)
            .send()
            .await,
        403,
        "forbidden",
    );
    expect(
        &http
            .delete_with(&site_path, &body)
            .cookie(&w.outsider.cookie)
            .send()
            .await,
        404,
    );
    expect_err(
        &http
            .delete_with(&site_path, &json!({"confirm": "nope"}))
            .cookie(&w.admin.cookie)
            .send()
            .await,
        400,
        "invalid_request",
    );
    expect(
        &http
            .delete_with(&site_path, &body)
            .cookie(&w.admin.cookie)
            .send()
            .await,
        204,
    );
    // 同一事务写入了意图：pending、约 7 天后、记录请求者
    let (status, secs, by) = site_intent(&pg, site).await.expect("站点删除意图");
    assert_eq!(status, "pending");
    assert!(
        (6.9 * 86400.0..=7.0 * 86400.0 + 60.0).contains(&secs),
        "not_before 应在 7 天后：{secs}"
    );
    assert_eq!(by, Some(w.admin.id));
    // 站点从列表与设置里消失
    let sites = expect(
        &http
            .get("/api/internal/sites")
            .cookie(&w.owner.cookie)
            .send()
            .await,
        200,
    );
    assert!(listed(&sites, site).is_none());
    expect(
        &http
            .get(&format!("{site_path}/settings"))
            .cookie(&w.owner.cookie)
            .send()
            .await,
        404,
    );

    // 已删除列表：editor / viewer 403，非成员 404，owner 看得到
    for who in [&w.editor, &w.viewer] {
        expect_err(
            &http.get(&deleted_path).cookie(&who.cookie).send().await,
            403,
            "forbidden",
        );
    }
    expect(
        &http
            .get(&deleted_path)
            .cookie(&w.outsider.cookie)
            .send()
            .await,
        404,
    );
    let list = expect(
        &http.get(&deleted_path).cookie(&w.owner.cookie).send().await,
        200,
    );
    let item = listed(&list, site).expect("已删除列表里有这个站点");
    assert_eq!(item["name"], name.as_str());
    assert_eq!(item["domain"], "example.test");
    assert!(item["site_key"].as_str().is_some_and(|k| k.len() == 12));
    assert!(
        item["deleted_at"]
            .as_str()
            .is_some_and(|t| t.ends_with('Z'))
    );
    assert!(item["purge_at"].as_str().unwrap() > item["deleted_at"].as_str().unwrap());

    // 恢复：outsider 404、editor 403、不存在 404
    expect(
        &http
            .post(&restore_path, &json!({}))
            .cookie(&w.outsider.cookie)
            .send()
            .await,
        404,
    );
    expect_err(
        &http
            .post(&restore_path, &json!({}))
            .cookie(&w.editor.cookie)
            .send()
            .await,
        403,
        "forbidden",
    );
    expect(
        &http
            .post("/api/internal/sites/999999999999/restore", &json!({}))
            .cookie(&w.owner.cookie)
            .send()
            .await,
        404,
    );
    let restored = expect(
        &http
            .post(&restore_path, &json!({}))
            .cookie(&w.admin.cookie)
            .send()
            .await,
        200,
    );
    assert_eq!(restored["site"]["id"], site);
    assert_eq!(restored["site"]["name"], name.as_str());
    assert_eq!(restored["site"]["role"], "admin");
    assert_eq!(restored["site"]["access"], "member");
    assert_eq!(site_intent(&pg, site).await.unwrap().0, "cancelled");
    let sites = expect(
        &http
            .get("/api/internal/sites")
            .cookie(&w.owner.cookie)
            .send()
            .await,
        200,
    );
    assert!(listed(&sites, site).is_some(), "恢复后重新出现在站点列表里");
    let list = expect(
        &http.get(&deleted_path).cookie(&w.owner.cookie).send().await,
        200,
    );
    assert!(listed(&list, site).is_none());
    // 没有被删除的站点不能恢复（404）
    expect(
        &http
            .post(&restore_path, &json!({}))
            .cookie(&w.owner.cookie)
            .send()
            .await,
        404,
    );

    // 恢复后可以再删除（旧意图已取消，部分唯一索引不挡）
    expect(
        &http
            .delete_with(&site_path, &body)
            .cookie(&w.owner.cookie)
            .send()
            .await,
        204,
    );
    assert_eq!(site_intent(&pg, site).await.unwrap().0, "pending");
    // 窗口已过：409，且不再出现在已删除列表里
    sqlx::query(
        "UPDATE pending_stats_deletions SET not_before = now() - interval '1 second'
          WHERE site_id = $1 AND scope = 'site' AND status = 'pending'",
    )
    .bind(site as i64)
    .execute(&pg)
    .await
    .unwrap();
    expect_err(
        &http
            .post(&restore_path, &json!({}))
            .cookie(&w.owner.cookie)
            .send()
            .await,
        409,
        "conflict",
    );
    let list = expect(
        &http.get(&deleted_path).cookie(&w.owner.cookie).send().await,
        200,
    );
    assert!(listed(&list, site).is_none());
    // 清除已经开始（running，租约在未来）：同样 409
    sqlx::query(
        "UPDATE pending_stats_deletions SET status = 'running', not_before = now() + interval '1 hour'
          WHERE site_id = $1 AND scope = 'site' AND status = 'pending'",
    )
    .bind(site as i64)
    .execute(&pg)
    .await
    .unwrap();
    expect_err(
        &http
            .post(&restore_path, &json!({}))
            .cookie(&w.owner.cookie)
            .send()
            .await,
        409,
        "conflict",
    );
    // 站点仍然是删除状态
    let deleted: bool =
        sqlx::query_scalar("SELECT deleted_at IS NOT NULL FROM sites WHERE id = $1")
            .bind(site as i64)
            .fetch_one(&pg)
            .await
            .unwrap();
    assert!(deleted);
    // 不再让 ch_delete_batch 碰这条测试数据
    sqlx::query("UPDATE pending_stats_deletions SET status = 'failed' WHERE site_id = $1 AND scope = 'site' AND status = 'running'")
        .bind(site as i64)
        .execute(&pg)
        .await
        .unwrap();
}

// ------------------------------------------------------------------ 日期范围删除请求

// ------------------------------------------------------------------ Plausible 导入

/// 只有 `imported_visitors` 的最小导出（其余族可以缺）。
fn visitors_zip() -> Vec<u8> {
    use std::io::Write;
    let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    zip.start_file(
        "imported_visitors_20260601_20260603.csv",
        zip::write::SimpleFileOptions::default(),
    )
    .unwrap();
    zip.write_all(
        b"\"date\",\"visitors\",\"pageviews\",\"bounces\",\"visits\",\"visit_duration\"\n\
          \"2026-06-01\",10,20,5,12,600\n\"2026-06-02\",8,15,4,9,300\n\"2026-06-03\",5,7,2,6,120\n",
    )
    .unwrap();
    zip.finish().unwrap().into_inner()
}

/// 轮询列表直到这次导入不再是 `importing`。
async fn settled(http: &Http, cookie: &str, path: &str, id: i64) -> Value {
    for _ in 0..300 {
        let list = expect(&http.get(path).cookie(cookie).send().await, 200);
        let row = list["imports"]
            .as_array()
            .unwrap()
            .iter()
            .find(|i| i["id"] == id)
            .cloned()
            .expect("列表里有这次导入");
        if row["status"] != "importing" {
            return row;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("导入 #{id} 30 秒内没有结束");
}

#[tokio::test]
async fn plausible_imports_over_http() {
    let Some(env) = test_env("plausible_imports_over_http") else {
        return;
    };
    let _serial = IMPORT_TESTS.lock().await;
    let pg = pg(&env).await;
    let http = start(&env, &pg).await;
    let w = world(&pg, &http).await;
    let site = make_site(&pg, w.org.id, &unique("Imported"), "UTC").await;
    let path = format!("/api/internal/sites/{site}/imports");
    let upload = format!("{path}?file_name=..%2F..%2Fblog_export.zip");

    // 权限：组织 admin+
    expect_err(
        &http.get(&path).cookie(&w.editor.cookie).send().await,
        403,
        "forbidden",
    );
    expect_err(
        &http
            .upload(&upload, visitors_zip())
            .cookie(&w.viewer.cookie)
            .send()
            .await,
        403,
        "forbidden",
    );
    expect_err(
        &http.get(&path).cookie(&w.outsider.cookie).send().await,
        404,
        "not_found",
    );
    // 表单能发的类型被 CSRF 判定拒绝
    let r = http
        .rb("POST", &upload, visitors_zip())
        .header("Content-Type", "text/plain")
        .header("Origin", &http.origin)
        .cookie(&w.admin.cookie)
        .send()
        .await;
    assert_eq!(r.status, 403, "{}", r.body);

    let list = expect(&http.get(&path).cookie(&w.admin.cookie).send().await, 200);
    assert_eq!(list["imports"], json!([]));
    assert_eq!(list["native_from"], Value::Null, "站点还没有数据");

    // A timezone change that already holds the site lock wins over import registration.
    let mut tx = pg.begin().await.unwrap();
    assert!(
        rivu_storage_pg::sites::lock_timezone(&mut tx, site, false)
            .await
            .unwrap()
            .is_some()
    );
    sqlx::query("UPDATE sites SET timezone = 'Asia/Taipei' WHERE id = $1")
        .bind(site as i64)
        .execute(&mut *tx)
        .await
        .unwrap();
    let request = http
        .upload(&upload, visitors_zip())
        .cookie(&w.admin.cookie)
        .send();
    tokio::pin!(request);
    let waiting = tokio::time::timeout(Duration::from_millis(150), &mut request).await;
    tx.commit().await.unwrap();
    assert!(
        waiting.is_err(),
        "upload completed before the timezone transaction committed"
    );
    let v = expect(&request.await, 202);
    assert_eq!(v["import"]["status"], "importing");
    assert_eq!(v["import"]["file_name"], "blog_export.zip", "路径部分去掉");
    let id = v["import"]["id"].as_i64().unwrap();
    let done = settled(&http, &w.admin.cookie, &path, id).await;
    assert_eq!(done["status"], "completed", "{done}");
    assert_eq!(done["start_date"], "2026-06-01");
    assert_eq!(done["end_date"], "2026-06-03");
    assert_eq!(done["imported_by"], "Test User");
    assert_eq!(done["error_code"], Value::Null);
    let timezone: String =
        rivu_storage_ch::client(&env.ch_url, &account("reader"), Some(&ch_database()))
            .query("SELECT tz FROM rollup_site FINAL WHERE site_id = ? AND import_id = ? LIMIT 1")
            .bind(site)
            .bind(id as u64)
            .fetch_one()
            .await
            .unwrap();
    assert_eq!(timezone, "Asia/Taipei");

    // 失败带代码：不是 zip、日期与已有导入重叠
    for (body, code) in [
        (b"not a zip".to_vec(), "invalid_zip"),
        (visitors_zip(), "overlap"),
    ] {
        let v = expect(
            &http
                .upload(&upload, body)
                .cookie(&w.owner.cookie)
                .send()
                .await,
            202,
        );
        let row = settled(
            &http,
            &w.owner.cookie,
            &path,
            v["import"]["id"].as_i64().unwrap(),
        )
        .await;
        assert_eq!(row["status"], "failed", "{row}");
        assert_eq!(row["error_code"], code, "{row}");
    }
    expect_err(
        &http
            .upload(&upload, Vec::new())
            .cookie(&w.admin.cookie)
            .send()
            .await,
        400,
        "invalid_request",
    );

    // 一个站点同时只有一个进行中的导入；进行中的不能删
    let running = rivu_storage_pg::imports::create(
        &mut pg.acquire().await.unwrap(),
        site,
        "plausible_csv",
        "x.zip",
        None,
    )
    .await
    .unwrap()
    .expect("建记录")
    .id;
    expect_err(
        &http
            .json_rb(
                "PATCH",
                &format!("/api/internal/sites/{site}"),
                &json!({"timezone": "UTC"}),
            )
            .cookie(&w.admin.cookie)
            .send()
            .await,
        409,
        "timezone_import_conflict",
    );
    expect_err(
        &http
            .upload(&upload, visitors_zip())
            .cookie(&w.admin.cookie)
            .send()
            .await,
        409,
        "import_running",
    );
    expect_err(
        &http
            .delete_with(&format!("{path}/{running}"), &json!({}))
            .cookie(&w.admin.cookie)
            .send()
            .await,
        409,
        "import_running",
    );
    rivu_storage_pg::imports::fail(
        &mut pg.acquire().await.unwrap(),
        running,
        "interrupted",
        "test",
    )
    .await
    .unwrap();

    // 模拟恢复置为失败后，已接受的 INSERT 才落表。
    let maintainer =
        rivu_storage_ch::client(&env.ch_url, &account("maintainer"), Some(&ch_database()));
    rivu_query::rollup::insert_import(
        &maintainer,
        &rivu_query::TenantScope::new(site),
        "rollup_events",
        "Asia/Taipei",
        running as u64,
        &[rivu_query::rollup::ImportRow {
            day: "2026-06-01".parse().unwrap(),
            dims: vec!["pageview".to_owned()],
            m: rivu_query::rollup::Measures {
                events: 20,
                ..Default::default()
            },
        }],
    )
    .await
    .unwrap();

    // 删除已完成的记录，只清理属于该批次的汇总行。
    let r = http
        .delete_with(&format!("{path}/{id}"), &json!({}))
        .cookie(&w.admin.cookie)
        .send()
        .await;
    expect(&r, 204);
    assert_eq!(imported_rows(&maintainer, "rollup_events", site).await, 1);
    assert_eq!(
        rivu_storage_pg::imports::completed_range(&pg, site)
            .await
            .unwrap(),
        None
    );
    let list = expect(&http.get(&path).cookie(&w.admin.cookie).send().await, 200);
    for row in list["imports"].as_array().unwrap() {
        assert_eq!(row["status"], "failed");
        let r = http
            .delete_with(&format!("{path}/{}", row["id"]), &json!({}))
            .cookie(&w.admin.cookie)
            .send()
            .await;
        expect(&r, 204);
    }
    assert_eq!(imported_rows(&maintainer, "rollup_events", site).await, 0);
    assert!(
        rivu_storage_pg::imports::list(&pg, site)
            .await
            .unwrap()
            .is_empty()
    );
    expect_err(
        &http
            .delete_with(&format!("{path}/{id}"), &json!({}))
            .cookie(&w.admin.cookie)
            .send()
            .await,
        404,
        "not_found",
    );
}

static IMPORT_TESTS: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

struct InsertGate {
    url: String,
    pending: tokio::sync::oneshot::Receiver<()>,
    release: tokio::sync::oneshot::Sender<()>,
    _proxy: tokio::task::JoinSet<std::io::Result<()>>,
}

/// Forward to real ClickHouse, withholding only the first INSERT response after the server commits it.
async fn gate_first_insert(env: &TestEnv) -> InsertGate {
    let target = url::Url::parse(&env.ch_url).unwrap();
    assert_eq!(
        target.scheme(),
        "http",
        "the test proxy requires local HTTP"
    );
    let upstream = target.socket_addrs(|| Some(80)).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let (notice, pending) = tokio::sync::oneshot::channel();
    let (release, wait) = tokio::sync::oneshot::channel();
    let mut proxy = tokio::task::JoinSet::new();
    proxy.spawn(async move {
        let mut gate = Some((notice, wait));
        loop {
            let (mut downstream, _) = listener.accept().await?;
            let mut header = Vec::new();
            loop {
                header.push(downstream.read_u8().await?);
                if header.ends_with(b"\r\n\r\n") {
                    break;
                }
            }
            let header = std::str::from_utf8(&header).unwrap();
            let insert = header
                .lines()
                .next()
                .unwrap()
                .contains("query=INSERT+INTO+");
            let mut server = TcpStream::connect(upstream.as_slice()).await?;
            for line in header.lines().take_while(|line| !line.is_empty()) {
                if !line.to_ascii_lowercase().starts_with("connection:") {
                    server.write_all(line.as_bytes()).await?;
                    server.write_all(b"\r\n").await?;
                }
            }
            server.write_all(b"Connection: close\r\n\r\n").await?;
            let (mut request, mut reply) = downstream.split();
            let (mut result, mut body) = server.split();
            let upload = async {
                tokio::io::copy(&mut request, &mut body).await?;
                body.shutdown().await
            };
            let download = async {
                if insert && let Some((notice, wait)) = gate.take() {
                    let mut response = Vec::new();
                    result.read_to_end(&mut response).await?;
                    assert!(
                        response.starts_with(b"HTTP/1.1 200"),
                        "{}",
                        String::from_utf8_lossy(&response)
                    );
                    let _ = notice.send(());
                    let _ = wait.await;
                    reply.write_all(&response).await?;
                } else {
                    tokio::io::copy(&mut result, &mut reply).await?;
                }
                reply.shutdown().await
            };
            tokio::pin!(upload);
            tokio::select! {
                result = download => result?,
                result = &mut upload => result?,
            }
        }
    });
    InsertGate {
        url,
        pending,
        release,
        _proxy: proxy,
    }
}

async fn wait_for_insert(gate: &mut InsertGate) {
    tokio::time::timeout(Duration::from_secs(15), &mut gate.pending)
        .await
        .expect("the first real INSERT must reach ClickHouse")
        .unwrap();
}

fn cli(env: &TestEnv, site_key: &str, action: &str) -> tokio::process::Command {
    let maintainer = account("maintainer");
    let reader = account("reader");
    let mut cmd = tokio::process::Command::new(env!("CARGO_BIN_EXE_rivu"));
    cmd.kill_on_drop(true)
        .args(["import", action, "--site", site_key])
        .env("RIVU_DATABASE_URL", &env.pg_url)
        .env("RIVU_CLICKHOUSE_URL", &env.ch_url)
        .env("RIVU_CH_DATABASE", ch_database())
        .env("RIVU_CH_READER_USER", reader.user)
        .env("RIVU_CH_READER_PASSWORD", reader.password.expose())
        .env("RIVU_CH_MAINTAINER_USER", maintainer.user)
        .env("RIVU_CH_MAINTAINER_PASSWORD", maintainer.password.expose());
    cmd
}

async fn cli_output(cmd: &mut tokio::process::Command) -> std::process::Output {
    tokio::time::timeout(Duration::from_secs(15), cmd.output())
        .await
        .expect("CLI must exit")
        .unwrap()
}

async fn import_row(pg: &PgPool, site: u64, status: &str) -> rivu_storage_pg::imports::ImportRow {
    let row = rivu_storage_pg::imports::list(pg, site)
        .await
        .unwrap()
        .remove(0);
    assert_eq!(row.status, status, "{row:?}");
    row
}

async fn imported_rows(ch: &Client, table: &str, site: u64) -> u64 {
    ch.query(&format!(
        "SELECT count() FROM {table} WHERE site_id = ? AND origin = 'import'"
    ))
    .bind(site)
    .fetch_one()
    .await
    .unwrap()
}

#[tokio::test]
async fn cli_import_delete_refuses_an_active_real_zip() {
    let Some(env) = test_env("cli_import_delete_refuses_an_active_real_zip") else {
        return;
    };
    let _serial = IMPORT_TESTS.lock().await;
    let pg = pg(&env).await;
    let site = make_site(&pg, make_org(&pg).await.id, &unique("CLI import"), "UTC").await;
    let key: String = sqlx::query_scalar("SELECT site_key FROM sites WHERE id = $1")
        .bind(site as i64)
        .fetch_one(&pg)
        .await
        .unwrap();
    let ch = rivu_storage_ch::client(&env.ch_url, &account("maintainer"), Some(&ch_database()));
    let mut gate = gate_first_insert(&env).await;
    let proxy_env = TestEnv {
        ch_url: gate.url.clone(),
        ..env.clone()
    };
    struct ZipFile(std::path::PathBuf);
    impl Drop for ZipFile {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }
    let zip = ZipFile(std::env::temp_dir().join(format!("rivu-import-{}.zip", Uuid::new_v4())));
    std::fs::write(&zip.0, visitors_zip()).unwrap();
    let mut child = cli(&proxy_env, &key, "plausible")
        .arg(&zip.0)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    tokio::select! {
        () = wait_for_insert(&mut gate) => {},
        status = child.wait() => {
            assert!(status.is_ok(), "{status:?}");
            panic!("import exited before its first insert: {:?}", child.wait_with_output().await.unwrap());
        }
    }
    let id = import_row(&pg, site, "importing").await.id.to_string();
    assert_eq!(imported_rows(&ch, "rollup_events", site).await, 3);
    assert!(child.try_wait().unwrap().is_none());

    let out = cli_output(cli(&env, &key, "delete").args(["--id", &id])).await;
    assert!(!out.status.success(), "delete accepted a live import");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("import_running"),
        "{out:?}"
    );
    assert_eq!(imported_rows(&ch, "rollup_events", site).await, 3);
    assert_eq!(imported_rows(&ch, "rollup_site", site).await, 0);
    assert_eq!(
        rivu_jobs::import_plausible::recover_interrupted(&pg, &ch)
            .await
            .unwrap(),
        0,
        "recovery must also skip the live CLI"
    );

    gate.release.send(()).unwrap();
    let out = tokio::time::timeout(Duration::from_secs(15), child.wait_with_output())
        .await
        .unwrap()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    assert_eq!(import_row(&pg, site, "completed").await.id.to_string(), id);
    let out = cli_output(&mut cli(&env, &key, "list")).await;
    assert!(out.status.success(), "{out:?}");
    assert!(String::from_utf8_lossy(&out.stdout).contains(&format!("{id}\tcompleted\t")));
    let out = cli_output(cli(&env, &key, "plausible").arg(&zip.0)).await;
    assert!(
        !out.status.success(),
        "a completed import must reject a duplicate ZIP"
    );
    assert_eq!(
        import_row(&pg, site, "failed").await.error_code.as_deref(),
        Some("overlap")
    );
    let out = cli_output(cli(&env, &key, "delete").args(["--id", &id])).await;
    assert!(out.status.success(), "{out:?}");
    assert_eq!(
        rivu_storage_pg::imports::list(&pg, site)
            .await
            .unwrap()
            .len(),
        1
    );
    assert_eq!(imported_rows(&ch, "rollup_events", site).await, 0);
    assert_eq!(imported_rows(&ch, "rollup_site", site).await, 0);
}

#[tokio::test]
async fn import_recovery_skips_live_upload_and_recovers_disconnected_owner() {
    let Some(mut env) =
        test_env("import_recovery_skips_live_upload_and_recovers_disconnected_owner")
    else {
        return;
    };
    let _serial = IMPORT_TESTS.lock().await;
    let mut pg_url = url::Url::parse(&env.pg_url).unwrap();
    pg_url.query_pairs_mut().append_pair(
        "options",
        "-c idle_in_transaction_session_timeout=3s -c transaction_timeout=4s",
    );
    // Migrate before enabling the deliberately short timeouts on the application connections.
    migrate_once(&env).await;
    env.pg_url = pg_url.to_string();
    let pg = pg(&env).await;
    let timeouts: (String, String) = sqlx::query_as(
        "SELECT current_setting('idle_in_transaction_session_timeout'), current_setting('transaction_timeout')",
    )
    .fetch_one(&pg)
    .await
    .unwrap();
    assert_eq!(timeouts, ("3s".into(), "4s".into()));
    let ch = rivu_storage_ch::client(&env.ch_url, &account("maintainer"), Some(&ch_database()));
    let mut gate = gate_first_insert(&env).await;
    let http = start_with_maintainer(&env, &pg, &gate.url).await;
    let org = make_org(&pg).await;
    let admin = actor(&pg, &http, &org, "admin").await;
    let site = make_site(&pg, org.id, &unique("Active upload"), "UTC").await;
    let site_path = format!("/api/internal/sites/{site}");
    let path = format!("{site_path}/imports");
    let id = expect(
        &http
            .upload(&path, visitors_zip())
            .cookie(&admin.cookie)
            .send()
            .await,
        202,
    )["import"]["id"]
        .as_i64()
        .unwrap();
    wait_for_insert(&mut gate).await;
    tokio::time::sleep(Duration::from_millis(4200)).await;
    let recover = || rivu_jobs::import_plausible::recover_interrupted(&pg, &ch);
    assert_eq!(recover().await.unwrap(), 0, "live upload was recovered");
    assert_eq!(import_row(&pg, site, "importing").await.id, id);
    assert_eq!(imported_rows(&ch, "rollup_events", site).await, 3);
    expect_err(
        &http
            .json_rb("PATCH", &site_path, &json!({"timezone": "Asia/Taipei"}))
            .cookie(&admin.cookie)
            .send()
            .await,
        409,
        "timezone_import_conflict",
    );

    let terminated: bool = sqlx::query_scalar(
        "SELECT pg_terminate_backend(pid, 3000)
        FROM pg_locks WHERE locktype = 'advisory' AND granted AND objsubid = 2
          AND classid::bigint = ($1::bigint >> 32) AND objid::bigint = ($1::bigint & 4294967295)",
    )
    .bind(id)
    .fetch_one(&pg)
    .await
    .unwrap();
    assert!(terminated);
    let (a, b) = tokio::try_join!(recover(), recover()).unwrap();
    assert_eq!(a + b, 1, "only one recovery must claim the orphan");
    assert_eq!(
        import_row(&pg, site, "failed").await.error_code.as_deref(),
        Some("interrupted")
    );
    assert_eq!(imported_rows(&ch, "rollup_events", site).await, 0);

    // The delayed success cannot make the old worker write the next table or complete the PG row.
    let _ = gate.release.send(());
    tokio::time::sleep(Duration::from_millis(1200)).await;
    let row = settled(&http, &admin.cookie, &path, id).await;
    assert_eq!(row["error_code"], "interrupted");
    assert_eq!(imported_rows(&ch, "rollup_site", site).await, 0);
    assert_eq!(recover().await.unwrap(), 0);
}

#[tokio::test]
async fn stats_deletion_requests() {
    let Some(env) = test_env("stats_deletion_requests") else {
        return;
    };
    let pg = pg(&env).await;
    let http = start(&env, &pg).await;
    let w = world(&pg, &http).await;
    let name = unique("Stats");
    // 时钟固定在 2026-03-12T12:00Z；站点在 UTC+14，本地已经是 2026-03-13
    let site = make_site(&pg, w.org.id, &name, "Pacific/Kiritimati").await;
    let path = format!("/api/internal/sites/{site}/stats-deletions");
    let ok = json!({"from": "2026-03-01", "to": "2026-03-13", "confirm": name});

    expect_err(
        &http.post(&path, &ok).cookie(&w.editor.cookie).send().await,
        403,
        "forbidden",
    );
    expect_err(
        &http.get(&path).cookie(&w.viewer.cookie).send().await,
        403,
        "forbidden",
    );
    expect(
        &http
            .post(&path, &ok)
            .cookie(&w.outsider.cookie)
            .send()
            .await,
        404,
    );
    expect(
        &http.get(&path).cookie(&w.outsider.cookie).send().await,
        404,
    );

    for bad in [
        json!({"from": "2026-03-01", "to": "2026-03-13", "confirm": "nope"}),
        json!({"from": "2026-03-05", "to": "2026-03-01", "confirm": name}),
        json!({"from": "2026-03-01", "to": "2026-03-14", "confirm": name}),
        json!({"from": "2026-3-1", "to": "2026-03-02", "confirm": name}),
        json!({"from": "2026-02-30", "to": "2026-03-02", "confirm": name}),
        json!({"from": "2024-03-12", "to": "2026-03-13", "confirm": name}),
        json!({"from": "2026-03-01", "confirm": name}),
    ] {
        let r = http.post(&path, &bad).cookie(&w.admin.cookie).send().await;
        assert_eq!(r.status, 400, "{bad}: {}", r.body);
    }
    let first = expect(
        &http.post(&path, &ok).cookie(&w.admin.cookie).send().await,
        201,
    );
    assert_eq!(first["deletion"]["from"], "2026-03-01");
    assert_eq!(first["deletion"]["to"], "2026-03-13");
    assert!(first["deletion"]["completed_at"].is_null());
    let second = expect(
        &http
            .post(
                &path,
                &json!({"from": " 2026-01-10 ", "to": "2026-01-10", "confirm": name}),
            )
            .cookie(&w.owner.cookie)
            .send()
            .await,
        201,
    );
    // 列表：新的在前，只有日期范围
    let list = expect(&http.get(&path).cookie(&w.owner.cookie).send().await, 200);
    let rows = list["deletions"].as_array().unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["id"], second["deletion"]["id"]);
    assert_eq!(rows[1]["id"], first["deletion"]["id"]);
    for r in rows {
        assert!(
            ["pending", "running", "done"].contains(&r["status"].as_str().unwrap()),
            "{r}"
        );
        assert!(r["created_at"].as_str().is_some());
    }
    let requested_by: Vec<Option<i64>> = sqlx::query_scalar(
        "SELECT requested_by FROM pending_stats_deletions WHERE site_id = $1 ORDER BY id",
    )
    .bind(site as i64)
    .fetch_all(&pg)
    .await
    .unwrap();
    assert_eq!(requested_by, vec![Some(w.admin.id), Some(w.owner.id)]);
}

// ------------------------------------------------------------------ ClickHouse 清除

/// 一个站点的测试数据：每个本地时刻一个会话（一个 pageview）和一个计数分钟。
fn site_rows(site_id: u64, instants: &[Timestamp]) -> (Vec<EventRow>, Vec<IngestCounterRow>) {
    let mut events = Vec::new();
    let mut counters = Vec::new();
    for (i, t) in instants.iter().enumerate() {
        let ts = OffsetDateTime::from_unix_timestamp(t.as_second()).unwrap();
        let session_id = (site_id << 16) | i as u64;
        events.push(EventRow {
            site_id,
            ts,
            event_id: Uuid::now_v7(),
            name: "pageview".into(),
            visitor_id: session_id ^ 0xABCD,
            session_id,
            session_start: ts,
            hostname: "example.test".into(),
            pathname: "/".into(),
            entry_pathname: "/".into(),
            ingest_partition: 0,
            referrer: String::new(),
            referrer_source: String::new(),
            channel: String::new(),
            utm_source: String::new(),
            utm_medium: String::new(),
            utm_campaign: String::new(),
            utm_content: String::new(),
            utm_term: String::new(),
            click_id_param: String::new(),
            country_code: *b"CN",
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
        });
        counters.push(IngestCounterRow {
            site_id,
            minute: OffsetDateTime::from_unix_timestamp(t.as_second() / 60 * 60).unwrap(),
            metric: "accepted".into(),
            script_version: 1,
            value: 1,
        });
    }
    (events, counters)
}

/// 每张表按站点本地日期的行数。
async fn per_day(ch: &Client, site_id: u64, tz: &str) -> BTreeMap<String, [u64; 3]> {
    let mut out: BTreeMap<String, [u64; 3]> = BTreeMap::new();
    for (i, (table, col)) in [
        ("events", "ts"),
        ("sessions", "started_at"),
        ("ingest_counters", "minute"),
    ]
    .into_iter()
    .enumerate()
    {
        let rows: Vec<(String, u64)> = ch
            .query(&format!(
                "SELECT toString(toDate({col}, {{tz:String}})) AS d, count() FROM {table}
                  WHERE site_id = {{site:UInt64}} GROUP BY d ORDER BY d"
            ))
            .param("tz", tz)
            .param("site", site_id)
            .fetch_all()
            .await
            .expect("按日计数");
        for (d, n) in rows {
            out.entry(d).or_default()[i] = n;
        }
    }
    out
}

async fn intent_state(pg: &PgPool, id: i64) -> (String, Vec<String>) {
    sqlx::query_as("SELECT status, partitions_done FROM pending_stats_deletions WHERE id = $1")
        .bind(id)
        .fetch_one(pg)
        .await
        .unwrap()
}

async fn date_range_intent(
    pg: &PgPool,
    site_id: u64,
    from: Date,
    to: Date,
    done: &[String],
) -> i64 {
    let (start, end) =
        rivu_query::resolve::local_dates_utc_ms(&TimeZone::get("Asia/Shanghai").unwrap(), from, to)
            .unwrap();
    // 一条语句写入（含预置的 partitions_done），并发的 run_batch 不会看到中间状态
    sqlx::query_scalar(
        "INSERT INTO pending_stats_deletions
            (site_id, scope, date_from, date_to, partitions_done, timezone, utc_start_ms, utc_end_ms)
         VALUES ($1, 'date_range', $2::date, $3::date, $4, 'Asia/Shanghai', $5, $6) RETURNING id",
    )
    .bind(site_id as i64)
    .bind(from.to_string())
    .bind(to.to_string())
    .bind(done)
    .bind(start)
    .bind(end)
    .fetch_one(pg)
    .await
    .unwrap()
}

async fn purge(pg: &PgPool, ch: &Client) {
    rivu_jobs::stats_delete::run_batch(pg, ch)
        .await
        .expect("ch_delete_batch");
}

#[tokio::test]
async fn purge_deletes_exactly_the_targeted_rows() {
    let Some(env) = test_env("purge_deletes_exactly_the_targeted_rows") else {
        return;
    };
    let pg = pg(&env).await;
    let db = ch_database();
    let writer = rivu_storage_ch::client(&env.ch_url, &account("writer"), Some(&db));
    let maintainer = rivu_storage_ch::client(&env.ch_url, &account("maintainer"), Some(&db));
    let dedup = rivu_storage_ch::rows::supports_deduplicate_insert(&writer)
        .await
        .expect("查询 deduplicate_insert");

    let org = make_org(&pg).await;
    const TZ: &str = "Asia/Shanghai";
    let a = make_site(&pg, org.id, &unique("A"), TZ).await;
    let b = make_site(&pg, org.id, &unique("B"), TZ).await;

    // 本地日期：上月最后一天 d1、本月 1 日 d2、2 日 d3、3 日 d4（都在 ingest_counters 的 90 天 TTL 内）
    let tz = TimeZone::get(TZ).unwrap();
    let today = Timestamp::now().to_zoned(tz.clone()).date();
    let d2 = today.first_of_month();
    let d1 = d2.checked_sub(1.day()).unwrap();
    let d3 = d2.checked_add(1.day()).unwrap();
    let d4 = d2.checked_add(2.days()).unwrap();
    let at = |d: Date, h: i8, m: i8| d.at(h, m, 0, 0).to_zoned(tz.clone()).unwrap().timestamp();
    // d2 00:30 本地 = 上一天 16:30 UTC：落在上一个 UTC 月的分区里，必须按本地日期删掉
    let instants = [
        at(d1, 12, 0),
        at(d1, 23, 59),
        at(d2, 0, 30),
        at(d2, 12, 0),
        at(d3, 12, 0),
        at(d3, 23, 30),
        at(d4, 0, 0),
        at(d4, 12, 0),
    ];
    for (site, tag) in [(a, "a"), (b, "b")] {
        let (events, counters) = site_rows(site, &instants);
        insert_events(&writer, &events, &format!("purge-it:{site}:{tag}:e"), dedup)
            .await
            .expect("写入 events");
        insert_ingest_counters(
            &writer,
            &counters,
            &format!("purge-it:{site}:{tag}:c"),
            dedup,
        )
        .await
        .expect("写入 ingest_counters");
    }
    let day = |d: Date| d.to_string();
    let full = BTreeMap::from([
        (day(d1), [2, 2, 2]),
        (day(d2), [2, 2, 2]),
        (day(d3), [2, 2, 2]),
        (day(d4), [2, 2, 2]),
    ]);
    assert_eq!(per_day(&maintainer, a, TZ).await, full);
    assert_eq!(per_day(&maintainer, b, TZ).await, full);

    // (a) 日期范围 d2..=d3：只删 A 的这两天
    let range = date_range_intent(&pg, a, d2, d3, &[]).await;
    purge(&pg, &maintainer).await;
    let (status, done) = intent_state(&pg, range).await;
    assert_eq!(status, "done");
    let months: Vec<u32> = {
        let m = |d: Date| u32::try_from(i32::from(d.year()) * 100 + i32::from(d.month())).unwrap();
        let mut v = vec![m(d1), m(d2)];
        v.dedup();
        v
    };
    for table in ["events", "sessions", "ingest_counters"] {
        for m in &months {
            assert!(
                done.contains(&format!("{table}:{m}")),
                "{done:?} 缺 {table}:{m}"
            );
        }
    }
    let after_range = BTreeMap::from([(day(d1), [2, 2, 2]), (day(d4), [2, 2, 2])]);
    assert_eq!(per_day(&maintainer, a, TZ).await, after_range);
    assert_eq!(per_day(&maintainer, b, TZ).await, full, "别的站点不受影响");

    // 幂等：同一范围再执行一次，结果不变
    let again = date_range_intent(&pg, a, d2, d3, &[]).await;
    purge(&pg, &maintainer).await;
    assert_eq!(intent_state(&pg, again).await.0, "done");
    assert_eq!(per_day(&maintainer, a, TZ).await, after_range);

    // 断点续跑：d4 的 events 分区已记为完成 → 跳过它，只删 sessions 与 ingest_counters
    let m4 = u32::try_from(i32::from(d4.year()) * 100 + i32::from(d4.month())).unwrap();
    let resumed = date_range_intent(&pg, a, d4, d4, &[format!("events:{m4}")]).await;
    purge(&pg, &maintainer).await;
    assert_eq!(intent_state(&pg, resumed).await.0, "done");
    assert_eq!(
        per_day(&maintainer, a, TZ).await,
        BTreeMap::from([(day(d1), [2, 2, 2]), (day(d4), [2, 0, 0])])
    );
    // 再来一条完整的：剩下的 events 也删掉
    let rerun = date_range_intent(&pg, a, d4, d4, &[]).await;
    purge(&pg, &maintainer).await;
    assert_eq!(intent_state(&pg, rerun).await.0, "done");
    assert_eq!(
        per_day(&maintainer, a, TZ).await,
        BTreeMap::from([(day(d1), [2, 2, 2])])
    );
    assert_eq!(per_day(&maintainer, b, TZ).await, full);

    // (b) 整站：恢复窗口 0 天 → 立即到期，删除 A 的全部数据
    assert!(
        rivu_storage_pg::stats_deletions::soft_delete_site(&pg, a, None, 0)
            .await
            .unwrap()
    );
    purge(&pg, &maintainer).await;
    let (status, done): (String, Vec<String>) = sqlx::query_as(
        "SELECT status, partitions_done FROM pending_stats_deletions WHERE site_id = $1 AND scope = 'site'",
    )
    .bind(a as i64)
    .fetch_one(&pg)
    .await
    .unwrap();
    assert_eq!(status, "done");
    let m1 = u32::try_from(i32::from(d1.year()) * 100 + i32::from(d1.month())).unwrap();
    assert!(done.contains(&format!("events:{m1}")), "{done:?}");
    assert!(
        per_day(&maintainer, a, TZ).await.is_empty(),
        "A 的数据应全部删除"
    );
    assert_eq!(per_day(&maintainer, b, TZ).await, full, "B 不受影响");

    // 清理 B（尽力而为）
    for table in ["events", "sessions", "ingest_counters"] {
        let _ = maintainer
            .query(&format!(
                "DELETE FROM {table} WHERE site_id = {{site:UInt64}}"
            ))
            .param("site", b)
            .execute()
            .await;
    }
}
