//! 更换登录邮箱（`/api/internal/me/email`、`/api/email-change/{token}`、`rivu user set-email`）的端到端测试：
//! 真实的 PostgreSQL，真实的本地 TCP 端口，路由由 `public::router_full` 装配（与 `rivu serve` 同一条路径），
//! 邮件进内存收件箱（`Mailer::memory`）。
//!
//! 缺少环境变量时跳过并打印原因：
//!   RIVU_TEST_DATABASE_URL     有 DDL 与写权限的 PostgreSQL 连接串（rivu_migrator）
//!   RIVU_TEST_CLICKHOUSE_URL   ClickHouse HTTP 地址（迁移要用）
//!
//! 用例：
//! - `link_*`：发到新邮箱的链接确认后才改；保留发起确认的本人会话、撤销其余会话；旧邮箱收到遮住新邮箱的通知；
//!   已确认的周报邮箱与已开的站点跟到新邮箱、待确认的作废；用过、过期、被替换、取消、旧邮箱已变、改密码之后的链接统一 404；
//!   确认时新邮箱已被占用 409；不带会话确认时撤销全部会话；
//! - `request_*`：会话与 CSRF、新邮箱格式、与当前相同、当前密码不对、已被占用、按 `(用户, IP)` 限流；
//! - `without_mail_*`：没配置 SMTP 时立即生效，保留当前会话，作废周报待确认的链接；
//! - `cli_*`：`rivu user set-email`。

use std::net::SocketAddr;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use rivu::dash_api::{self, DashApi, DashLimits};
use rivu::{migrate, public, user_cmd};
use rivu_auth::password::hash_blocking;
use rivu_common::config::{ChAccount, Config, Env, Role, Secret};
use rivu_mail::{Mail, Mailer, Outbox};
use rivu_storage_pg::sites::NewSite;
use serde_json::{Value, json};
use sqlx::PgPool;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::OnceCell;

const PASSWORD: &str = "correct horse battery";
const HTTP_ORIGIN: &str = "http://127.0.0.1:8080";
const BROWSER_UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36";
const ME_EMAIL: &str = "/api/internal/me/email";

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

    fn json_rb(&self, method: &'static str, path: &str, body: &Value) -> Rb<'_> {
        self.rb(method, path, body.to_string().into_bytes())
            .header("Content-Type", "application/json")
            .header("Origin", &self.origin)
    }

    fn post(&self, path: &str, body: &Value) -> Rb<'_> {
        self.json_rb("POST", path, body)
    }

    fn delete(&self, path: &str) -> Rb<'_> {
        self.json_rb("DELETE", path, &json!({}))
    }
}

/// 起服务；`mail` 为真时用内存收件箱发信（配置了 SMTP），否则没有发信器。
async fn start(env: &TestEnv, pg: &PgPool, limits: DashLimits, mail: bool) -> (Http, Outbox) {
    let cfg = config(env);
    let reader = rivu_storage_ch::client(&env.ch_url, &account("reader"), Some(&ch_database()));
    let mut dash = DashApi::new(&cfg, pg.clone(), reader, None)
        .expect("装配内部端点")
        .with_limits(limits);
    let outbox = if mail {
        let (mailer, outbox) = Mailer::memory("Rivu <rivu@example.test>");
        dash = dash.with_mailer(mailer);
        outbox
    } else {
        Outbox::default()
    };
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
        outbox,
    )
}

// ------------------------------------------------------------------ 数据

fn password_hash() -> &'static str {
    static H: OnceLock<String> = OnceLock::new();
    H.get_or_init(|| hash_blocking(PASSWORD).unwrap())
}

fn fresh_email() -> String {
    format!("{}@example.test", unique("mail"))
}

async fn make_user(pg: &PgPool) -> (i64, String) {
    let email = fresh_email();
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

/// 一个组织、一个站点，用户是 viewer；周报邮箱已确认，这个站点开着周报。返回站点 id。
async fn subscribe_weekly(pg: &PgPool, user_id: i64, email: &str) -> i64 {
    let slug = unique("org");
    let org: i64 =
        sqlx::query_scalar("INSERT INTO organizations (name, slug) VALUES ($1, $1) RETURNING id")
            .bind(&slug)
            .fetch_one(pg)
            .await
            .unwrap();
    rivu_storage_pg::users::upsert_member(pg, org, user_id, "viewer")
        .await
        .unwrap();
    let hostnames = vec!["example.test".to_owned()];
    let site = rivu_storage_pg::sites::create_site_in_org(
        pg,
        org,
        &NewSite {
            name: "email-change-e2e",
            timezone: "UTC",
            hostnames: &hostnames,
        },
    )
    .await
    .unwrap();
    let site_id = site.id as i64;
    sqlx::query(
        "INSERT INTO weekly_report_emails (user_id, email, lang, confirmed_at)
         VALUES ($1, $2, 'en', now() - interval '1 day')",
    )
    .bind(user_id)
    .bind(email)
    .execute(pg)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO weekly_report_subscriptions (user_id, site_id, email, lang, confirmed_at)
         VALUES ($1, $2, $3, 'en', now() - interval '1 day')",
    )
    .bind(user_id)
    .bind(site_id)
    .bind(email)
    .execute(pg)
    .await
    .unwrap();
    site_id
}

async fn login(http: &Http, email: &str) -> Resp {
    http.post(
        "/api/internal/login",
        &json!({"email": email, "password": PASSWORD}),
    )
    .send()
    .await
}

async fn login_cookie(http: &Http, email: &str) -> String {
    let r = login(http, email).await;
    assert_eq!(r.status, 200, "{}", r.body);
    r.cookie_pair("rivu_session").expect("会话 cookie")
}

async fn me_status(http: &Http, cookie: &str) -> u16 {
    http.get("/api/internal/me")
        .cookie(cookie)
        .send()
        .await
        .status
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

async fn request(http: &Http, cookie: &str, new_email: &str, password: &str) -> Resp {
    http.post(
        ME_EMAIL,
        &json!({"new_email": new_email, "current_password": password}),
    )
    .cookie(cookie)
    .send()
    .await
}

/// 发起更换，返回确认邮件里的令牌和那封邮件。新邮箱按规范化（小写）后的地址核对。
async fn request_link(
    http: &Http,
    outbox: &Outbox,
    cookie: &str,
    new_email: &str,
    lang: &str,
) -> (String, Mail) {
    let r = http
        .post(
            ME_EMAIL,
            &json!({"new_email": new_email, "current_password": PASSWORD, "lang": lang}),
        )
        .cookie(cookie)
        .send()
        .await;
    let v = expect(&r, 202);
    let new_email = new_email.to_lowercase();
    assert_eq!(v["status"], "pending");
    assert_eq!(v["pending"]["new_email"], new_email);
    assert!(v["pending"]["expires_at"].as_str().unwrap().ends_with('Z'));
    let mut mails = outbox.take();
    assert_eq!(mails.len(), 1, "{mails:?}");
    let mail = mails.remove(0);
    assert_eq!(mail.to, new_email);
    let prefix = format!("{HTTP_ORIGIN}/email-change/confirm/");
    let line = mail
        .body
        .lines()
        .find(|l| l.contains(&prefix))
        .unwrap_or_else(|| panic!("没有确认链接：{}", mail.body));
    let token = line[line.find(&prefix).unwrap() + prefix.len()..].to_owned();
    assert!(rivu_auth::session::is_well_formed(&token), "{token}");
    let html = mail.html.as_deref().expect("HTML 部分");
    assert!(html.contains(&format!("href=\"{prefix}{token}\"")));
    (token, mail)
}

fn token_path(token: &str) -> String {
    format!("/api/email-change/{token}")
}

async fn check_link(http: &Http, token: &str) -> Resp {
    http.get(&token_path(token)).send().await
}

async fn confirm_link(http: &Http, token: &str, cookie: Option<&str>) -> Resp {
    let mut rb = http.post(&token_path(token), &json!({}));
    if let Some(c) = cookie {
        rb = rb.cookie(c);
    }
    rb.send().await
}

/// 统一的 404：两个链接端点、各种失败，响应完全相同。
async fn assert_link_dead(http: &Http, token: &str) {
    for r in [
        check_link(http, token).await,
        confirm_link(http, token, None).await,
    ] {
        assert_eq!(r.status, 404, "{}", r.body);
        assert_eq!(
            r.json(),
            json!({"error": "not_found", "message": "not found"})
        );
        assert_eq!(r.header("referrer-policy"), Some("no-referrer"));
    }
}

async fn wait_for_mail(outbox: &Outbox) -> Vec<Mail> {
    for _ in 0..100 {
        let mails = outbox.take();
        if !mails.is_empty() {
            return mails;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("等不到邮件");
}

async fn user_email(pg: &PgPool, user_id: i64) -> String {
    sqlx::query_scalar("SELECT email::text FROM users WHERE id = $1")
        .bind(user_id)
        .fetch_one(pg)
        .await
        .unwrap()
}

// ------------------------------------------------------------------ 用例

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn link_changes_email_keeps_current_session_and_moves_weekly_reports() {
    let Some(env) = test_env("link_happy_path") else {
        return;
    };
    migrate_once(&env).await;
    let pg = rivu_storage_pg::connect(&env.pg_url, 6).await.unwrap();
    let (http, outbox) = start(&env, &pg, loose(), true).await;
    let (user_id, old) = make_user(&pg).await;
    let site_id = subscribe_weekly(&pg, user_id, &old).await;
    let a = login_cookie(&http, &old).await;
    let b = login_cookie(&http, &old).await;

    let v = expect(&http.get(ME_EMAIL).cookie(&a).send().await, 200);
    assert_eq!(
        v,
        json!({"email": old, "confirm_by_email": true, "pending": null})
    );

    // 新邮箱写成大写也按规范化后的处理
    let new = fresh_email();
    let (token, mail) = request_link(&http, &outbox, &a, &new.to_uppercase(), "zh").await;
    assert_eq!(mail.subject, "确认 Rivu 的新登录邮箱");
    assert!(mail.body.contains("24 小时"), "{}", mail.body);
    let v = expect(&http.get(ME_EMAIL).cookie(&a).send().await, 200);
    assert_eq!(v["email"], old);
    assert_eq!(v["pending"]["new_email"], new);

    // 确认之前什么都没变：旧邮箱照常登录，两个会话都在
    assert_eq!(user_email(&pg, user_id).await, old);
    assert_eq!(login(&http, &new).await.status, 401);
    assert_eq!(me_status(&http, &b).await, 200);

    // 打开确认页：只读
    let r = check_link(&http, &token).await;
    assert_eq!(expect(&r, 200), json!({"email": new}));
    assert_eq!(r.header("referrer-policy"), Some("no-referrer"));
    assert_eq!(r.header("cache-control"), Some("no-store"));
    assert_eq!(expect(&check_link(&http, &token).await, 200)["email"], new);
    assert_eq!(user_email(&pg, user_id).await, old);

    // 确认（在发起更换的那个浏览器里）：保留这一个会话，其余撤销
    let r = confirm_link(&http, &token, Some(&a)).await;
    assert_eq!(expect(&r, 200), json!({"email": new, "signed_in": true}));
    assert_eq!(r.header("referrer-policy"), Some("no-referrer"));
    assert_eq!(user_email(&pg, user_id).await, new);
    assert_eq!(me_status(&http, &a).await, 200);
    assert_eq!(me_status(&http, &b).await, 401);
    let me = expect(&http.get("/api/internal/me").cookie(&a).send().await, 200);
    assert_eq!(me["user"]["email"], new);

    // 旧邮箱收到通知，新邮箱遮住
    let mails = wait_for_mail(&outbox).await;
    assert_eq!(mails.len(), 1);
    assert_eq!(mails[0].to, old);
    assert_eq!(mails[0].subject, "Rivu 登录邮箱已更改");
    let masked = format!("{}***@example.test", &new[..1]);
    assert!(mails[0].body.contains(&masked), "{}", mails[0].body);
    assert!(!mails[0].body.contains(&new));
    assert!(mails[0].html.is_some());

    // 新邮箱能登录，旧邮箱不能
    assert_eq!(login(&http, &new).await.status, 200);
    assert_eq!(login(&http, &old).await.status, 401);

    // 链接只能用一次
    assert_link_dead(&http, &token).await;

    // 周报：新邮箱算已确认，已开的站点跟过去
    let (email, confirmed): (String, bool) = sqlx::query_as(
        "SELECT email, confirmed_at IS NOT NULL FROM weekly_report_emails WHERE user_id = $1",
    )
    .bind(user_id)
    .fetch_one(&pg)
    .await
    .unwrap();
    assert_eq!((email.as_str(), confirmed), (new.as_str(), true));
    let sub: String = sqlx::query_scalar(
        "SELECT email FROM weekly_report_subscriptions WHERE user_id = $1 AND site_id = $2",
    )
    .bind(user_id)
    .bind(site_id)
    .fetch_one(&pg)
    .await
    .unwrap();
    assert_eq!(sub, new);
    let weekly = expect(
        &http
            .get("/api/internal/account/weekly-report")
            .cookie(&a)
            .send()
            .await,
        200,
    );
    assert_eq!(weekly["email"], new);
    assert_eq!(weekly["email_status"], "confirmed");
    let site = weekly["sites"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["id"] == json!(site_id))
        .expect("站点在列表里");
    assert_eq!(site["subscribed"], true);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn link_without_session_signs_out_everywhere() {
    let Some(env) = test_env("link_no_session") else {
        return;
    };
    migrate_once(&env).await;
    let pg = rivu_storage_pg::connect(&env.pg_url, 6).await.unwrap();
    let (http, outbox) = start(&env, &pg, loose(), true).await;
    let (user_id, old) = make_user(&pg).await;
    let (_, other) = make_user(&pg).await;
    let a = login_cookie(&http, &old).await;
    let b = login_cookie(&http, &old).await;
    let stranger = login_cookie(&http, &other).await;
    // 周报邮箱还在待确认：一条发给旧邮箱、还没点的链接
    let site_id = subscribe_weekly(&pg, user_id, &old).await;
    sqlx::query(
        "UPDATE weekly_report_emails SET confirmed_at = NULL, token_hash = $2,
                token_expires_at = now() + interval '1 hour' WHERE user_id = $1",
    )
    .bind(user_id)
    .bind(vec![9u8; 32])
    .execute(&pg)
    .await
    .unwrap();
    let new = fresh_email();
    let (token, mail) = request_link(&http, &outbox, &a, &new, "en").await;
    assert_eq!(mail.subject, "Confirm your new Rivu sign-in email");

    // 别人的会话不算本人：照样撤销本人的全部会话，别人的会话不受影响
    let r = confirm_link(&http, &token, Some(&stranger)).await;
    assert_eq!(expect(&r, 200), json!({"email": new, "signed_in": false}));
    assert_eq!(user_email(&pg, user_id).await, new);
    assert_eq!(me_status(&http, &a).await, 401);
    assert_eq!(me_status(&http, &b).await, 401);
    assert_eq!(me_status(&http, &stranger).await, 200);
    let mails = wait_for_mail(&outbox).await;
    assert_eq!(mails[0].to, old);
    assert_eq!(mails[0].subject, "Your Rivu sign-in email was changed");
    // 待确认的周报邮箱作废，回到未确认，不会因为换了登录邮箱就算确认；订阅行保留但暂停
    let (pending, confirmed, email): (bool, bool, String) = sqlx::query_as(
        "SELECT token_hash IS NOT NULL, confirmed_at IS NOT NULL, email FROM weekly_report_emails
         WHERE user_id = $1",
    )
    .bind(user_id)
    .fetch_one(&pg)
    .await
    .unwrap();
    assert_eq!(
        (pending, confirmed, email.as_str()),
        (false, false, old.as_str())
    );
    let sub: String = sqlx::query_scalar(
        "SELECT email FROM weekly_report_subscriptions WHERE user_id = $1 AND site_id = $2",
    )
    .bind(user_id)
    .bind(site_id)
    .fetch_one(&pg)
    .await
    .unwrap();
    assert_eq!(sub, old);

    // 从没用过周报的账号：确认更换不会凭空建一条周报邮箱确认
    let (other_id, other_old) = make_user(&pg).await;
    let c = login_cookie(&http, &other_old).await;
    let (token, _) = request_link(&http, &outbox, &c, &fresh_email(), "en").await;
    expect(&confirm_link(&http, &token, None).await, 200);
    wait_for_mail(&outbox).await;
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM weekly_report_emails WHERE user_id = $1")
        .bind(other_id)
        .fetch_one(&pg)
        .await
        .unwrap();
    assert_eq!(n, 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn link_is_invalid_after_expiry_replacement_cancel_or_account_changes() {
    let Some(env) = test_env("link_invalid") else {
        return;
    };
    migrate_once(&env).await;
    let pg = rivu_storage_pg::connect(&env.pg_url, 6).await.unwrap();
    let (http, outbox) = start(&env, &pg, loose(), true).await;
    let (user_id, old) = make_user(&pg).await;
    let a = login_cookie(&http, &old).await;

    // 格式不对、不存在
    assert_link_dead(&http, "short").await;
    assert_link_dead(&http, &"A".repeat(43)).await;

    // 过期
    let (token, _) = request_link(&http, &outbox, &a, &fresh_email(), "en").await;
    sqlx::query(
        "UPDATE email_changes SET expires_at = now() - interval '1 second' WHERE user_id = $1",
    )
    .bind(user_id)
    .execute(&pg)
    .await
    .unwrap();
    assert_link_dead(&http, &token).await;
    let v = expect(&http.get(ME_EMAIL).cookie(&a).send().await, 200);
    assert_eq!(v["pending"], Value::Null, "过期的不算待确认");

    // 新的请求替换旧的
    let (first, _) = request_link(&http, &outbox, &a, &fresh_email(), "en").await;
    let second_email = fresh_email();
    let (second, _) = request_link(&http, &outbox, &a, &second_email, "en").await;
    assert_link_dead(&http, &first).await;
    assert_eq!(
        expect(&check_link(&http, &second).await, 200)["email"],
        second_email
    );

    // 取消
    let r = http.delete(ME_EMAIL).cookie(&a).send().await;
    assert_eq!(r.status, 204, "{}", r.body);
    assert_link_dead(&http, &second).await;
    assert_eq!(
        expect(&http.get(ME_EMAIL).cookie(&a).send().await, 200)["pending"],
        Value::Null
    );

    // 发起之后账号邮箱被运维改了：链接作废
    let (token, _) = request_link(&http, &outbox, &a, &fresh_email(), "en").await;
    let interim = fresh_email();
    user_cmd::set_email(&pg, &old, &interim).await.unwrap();
    assert_link_dead(&http, &token).await;
    let a = login_cookie(&http, &interim).await;

    // 改密码作废待确认的更换
    let (token, _) = request_link(&http, &outbox, &a, &fresh_email(), "en").await;
    let r = http
        .post(
            "/api/internal/me/password",
            &json!({"current": PASSWORD, "new": "another long passphrase"}),
        )
        .cookie(&a)
        .send()
        .await;
    let a = r.cookie_pair("rivu_session").expect("新会话");
    expect(&r, 200);
    assert_link_dead(&http, &token).await;
    sqlx::query("UPDATE users SET password_hash = $2 WHERE id = $1")
        .bind(user_id)
        .bind(password_hash())
        .execute(&pg)
        .await
        .unwrap();

    // 确认时新邮箱已被别人注册：409，链接随之作废，邮箱不变
    let target = fresh_email();
    let (token, _) = request_link(&http, &outbox, &a, &target, "en").await;
    rivu_storage_pg::users::create_user(
        &pg,
        &rivu_storage_pg::users::NewUser {
            email: &target,
            name: None,
            password_hash: password_hash(),
            is_instance_admin: false,
        },
    )
    .await
    .unwrap();
    expect_err(
        &confirm_link(&http, &token, Some(&a)).await,
        409,
        "email_taken",
    );
    assert_link_dead(&http, &token).await;
    assert_eq!(user_email(&pg, user_id).await, interim);
    assert_eq!(me_status(&http, &a).await, 200, "没改成就不撤销会话");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn request_validates_input_password_and_uniqueness() {
    let Some(env) = test_env("request_validation") else {
        return;
    };
    migrate_once(&env).await;
    let pg = rivu_storage_pg::connect(&env.pg_url, 6).await.unwrap();
    let (http, outbox) = start(&env, &pg, loose(), true).await;
    let (user_id, email) = make_user(&pg).await;
    let (_, taken) = make_user(&pg).await;
    let a = login_cookie(&http, &email).await;
    let body = json!({"new_email": fresh_email(), "current_password": PASSWORD});

    // 没有会话、CSRF、方法
    expect_err(
        &http.post(ME_EMAIL, &body).send().await,
        401,
        "unauthorized",
    );
    expect_err(&http.get(ME_EMAIL).send().await, 401, "unauthorized");
    let r = http
        .post(ME_EMAIL, &body)
        .cookie(&a)
        .remove("Origin")
        .send()
        .await;
    expect_err(&r, 403, "csrf_rejected");
    let r = http
        .json_rb("PUT", ME_EMAIL, &json!({}))
        .cookie(&a)
        .send()
        .await;
    assert_eq!(r.status, 405);
    // 请求体不对
    let r = http
        .post(ME_EMAIL, &json!({"new_email": fresh_email()}))
        .cookie(&a)
        .send()
        .await;
    expect_err(&r, 400, "invalid_request");
    let r = http
        .post(
            ME_EMAIL,
            &json!({"new_email": fresh_email(), "current_password": PASSWORD, "lang": "fr"}),
        )
        .cookie(&a)
        .send()
        .await;
    expect_err(&r, 400, "invalid_request");

    expect_err(
        &request(&http, &a, "not-an-email", PASSWORD).await,
        400,
        "invalid_email",
    );
    expect_err(
        &request(&http, &a, &email.to_uppercase(), PASSWORD).await,
        400,
        "same_email",
    );
    expect_err(
        &request(&http, &a, &fresh_email(), "not the password!").await,
        400,
        "wrong_password",
    );
    // 已被占用：密码不对时先报密码（不凭会话探测别人的邮箱）
    expect_err(
        &request(&http, &a, &taken, "not the password!").await,
        400,
        "wrong_password",
    );
    let r = request(&http, &a, &taken.to_uppercase(), PASSWORD).await;
    expect_err(&r, 409, "email_taken");
    assert_eq!(
        r.json()["message"],
        "an account with this email address already exists"
    );

    // 失败的请求什么都没改，也没发信
    assert!(outbox.take().is_empty());
    assert_eq!(user_email(&pg, user_id).await, email);
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM email_changes WHERE user_id = $1")
        .bind(user_id)
        .fetch_one(&pg)
        .await
        .unwrap();
    assert_eq!(n, 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn request_is_rate_limited_like_password_change() {
    let Some(env) = test_env("request_rate") else {
        return;
    };
    migrate_once(&env).await;
    let pg = rivu_storage_pg::connect(&env.pg_url, 6).await.unwrap();
    let limits = DashLimits {
        login_attempts: 3,
        ..loose()
    };
    let (http, _outbox) = start(&env, &pg, limits, true).await;
    let (_, email) = make_user(&pg).await;
    let a = login_cookie(&http, &email).await;
    for _ in 0..3 {
        expect_err(
            &request(&http, &a, &fresh_email(), "not the password!").await,
            400,
            "wrong_password",
        );
    }
    let r = request(&http, &a, &fresh_email(), PASSWORD).await;
    expect_err(&r, 429, "rate_limited");
    assert!(r.header("retry-after").is_some());
    // 桶按 (用户, IP)：换个 IP 不受影响
    let r = http
        .post(
            ME_EMAIL,
            &json!({"new_email": fresh_email(), "current_password": PASSWORD}),
        )
        .cookie(&a)
        .ip("198.51.100.7")
        .send()
        .await;
    assert_eq!(r.status, 202, "{}", r.body);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn without_mail_the_change_applies_immediately() {
    let Some(env) = test_env("without_mail") else {
        return;
    };
    migrate_once(&env).await;
    let pg = rivu_storage_pg::connect(&env.pg_url, 6).await.unwrap();
    let (http, _) = start(&env, &pg, loose(), false).await;
    let (user_id, old) = make_user(&pg).await;
    let site_id = subscribe_weekly(&pg, user_id, &old).await;
    // 一条发给旧邮箱、还没确认的周报链接
    sqlx::query(
        "UPDATE weekly_report_emails SET confirmed_at = NULL, token_hash = $2,
                token_expires_at = now() + interval '1 hour' WHERE user_id = $1",
    )
    .bind(user_id)
    .bind(vec![7u8; 32])
    .execute(&pg)
    .await
    .unwrap();
    let a = login_cookie(&http, &old).await;
    let b = login_cookie(&http, &old).await;
    let v = expect(&http.get(ME_EMAIL).cookie(&a).send().await, 200);
    assert_eq!(v["confirm_by_email"], false);

    expect_err(
        &request(&http, &a, &fresh_email(), "not the password!").await,
        400,
        "wrong_password",
    );
    let new = fresh_email();
    let r = request(&http, &a, &new, PASSWORD).await;
    let v = expect(&r, 200);
    assert_eq!(v["status"], "changed");
    assert_eq!(v["user"]["id"], user_id);
    assert_eq!(v["user"]["email"], new);
    assert_eq!(user_email(&pg, user_id).await, new);
    assert_eq!(me_status(&http, &a).await, 200, "当前会话保留");
    assert_eq!(me_status(&http, &b).await, 401, "其他会话撤销");
    assert_eq!(login(&http, &new).await.status, 200);
    assert_eq!(login(&http, &old).await.status, 401);

    // 周报：待确认的链接作废；新邮箱没验证过，不算已确认，订阅行保留但暂停
    let (token_cleared, email): (bool, String) = sqlx::query_as(
        "SELECT token_hash IS NULL, email FROM weekly_report_emails WHERE user_id = $1",
    )
    .bind(user_id)
    .fetch_one(&pg)
    .await
    .unwrap();
    assert!(token_cleared);
    assert_eq!(email, old);
    let weekly = expect(
        &http
            .get("/api/internal/account/weekly-report")
            .cookie(&a)
            .send()
            .await,
        200,
    );
    assert_eq!(weekly["email_status"], "unconfirmed");
    let subscribed: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT FROM weekly_report_subscriptions WHERE user_id = $1 AND site_id = $2)",
    )
    .bind(user_id)
    .bind(site_id)
    .fetch_one(&pg)
    .await
    .unwrap();
    assert!(subscribed);

    // 已被占用
    let (_, taken) = make_user(&pg).await;
    expect_err(
        &request(&http, &a, &taken, PASSWORD).await,
        409,
        "email_taken",
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cli_set_email_changes_without_confirmation_and_signs_out_everywhere() {
    let Some(env) = test_env("cli_set_email") else {
        return;
    };
    migrate_once(&env).await;
    let pg = rivu_storage_pg::connect(&env.pg_url, 6).await.unwrap();
    let (http, outbox) = start(&env, &pg, loose(), true).await;
    let (user_id, old) = make_user(&pg).await;
    let (_, taken) = make_user(&pg).await;
    let a = login_cookie(&http, &old).await;
    let (token, _) = request_link(&http, &outbox, &a, &fresh_email(), "en").await;

    let new = fresh_email();
    user_cmd::set_email(&pg, &format!("  {}  ", old.to_uppercase()), &new)
        .await
        .unwrap();
    assert_eq!(user_email(&pg, user_id).await, new);
    assert_eq!(me_status(&http, &a).await, 401, "全部会话撤销");
    assert_link_dead(&http, &token).await;
    assert!(outbox.take().is_empty(), "运维改邮箱不发信");
    assert_eq!(login(&http, &new).await.status, 200);

    let err = user_cmd::set_email(&pg, &new, &taken).await.unwrap_err();
    assert!(format!("{err:#}").contains("已被别的账号使用"), "{err:#}");
    let err = user_cmd::set_email(&pg, &new, &new).await.unwrap_err();
    assert!(format!("{err:#}").contains("相同"), "{err:#}");
    let err = user_cmd::set_email(&pg, &old, &fresh_email())
        .await
        .unwrap_err();
    assert!(format!("{err:#}").contains("找不到用户"), "{err:#}");
    assert!(user_cmd::set_email(&pg, &new, "nope").await.is_err());
    assert_eq!(user_email(&pg, user_id).await, new);
}
