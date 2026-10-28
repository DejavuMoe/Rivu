//! 屏蔽规则、采集限流、垃圾引荐与 API key 的集成测试（连真实 PostgreSQL 与 ClickHouse）。
//!
//! 缺少环境变量时跳过并打印原因（同 `e2e_lite.rs`）：
//!   RIVU_TEST_DATABASE_URL     有 DDL 与写权限的 PostgreSQL 连接串（rivu_migrator）
//!   RIVU_TEST_CLICKHOUSE_URL   ClickHouse HTTP 地址
//! 可选：RIVU_TEST_CH_{MIGRATOR,WRITER,READER}_{USER,PASSWORD}、RIVU_TEST_CH_DATABASE。
//!
//! 三个用例：
//! 1. 建站与规则 → 管线处理一组事件 → ClickHouse `ingest_counters` 里 accepted 与各 `dropped_*` 逐项等于预期，
//!    `events` 行数等于 accepted；另外验证每站规则上限在并发添加下不超限；
//! 2. API key 全链路：创建 → 鉴权（401）→ 限流（429）→ 作用域（403）→ 站点权限（404）→ 吊销与失效；
//! 3. 命令行入口（`shield add|list|remove`、`apikey create|list|revoke`）的校验与副作用。

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::extract::{Path, State};
use axum::http::{HeaderMap, Request, header};
use axum::response::Json;
use axum::routing::get;
use clickhouse::Client;
use rivu::apikey_cmd::issue_key;
use rivu::cli::{
    ApikeyAction, ApikeyCreateArgs, ApikeyListArgs, ApikeyRevokeArgs, ShieldAction,
    ShieldActionKind, ShieldAddArgs, ShieldListArgs, ShieldRemoveArgs, ShieldType,
};
use rivu::{api_auth, apikey_cmd, migrate, shield_cmd};
use rivu_auth::key::{generate, hash_key};
use rivu_auth::{
    ApiError, ApiGuard, AuthContext, PgStore, Required, Resource, ScopeAction, protect,
};
use rivu_common::config::{BufferConfig, ChAccount, Config, Env, Role, Secret};
use rivu_domain::DropReason;
use rivu_geo::GeoDb;
use rivu_ingest::pipeline::{
    Outcome, Pipeline, RequestCtx, SaltCache, SiteSnapshot, salt_window, utc_today,
};
use rivu_ingest::shield::{RuleAction, RuleType, normalize_value};
use rivu_storage_pg::api_keys::NewApiKey;
use rivu_storage_pg::shield::{AddOutcome, NewRule};
use rivu_storage_pg::sites::NewSite;
use rivu_ua::UaParser;
use rivu_writer::{BatchWriter, ChSink, WriterOptions};
use serde_json::{Value, json};
use sqlx::PgPool;
use tokio::sync::{OnceCell, Semaphore};
use tower::ServiceExt;

const HOST: &str = "example.test";
const CHROME_WIN: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36";
const GOOGLEBOT: &str = "Mozilla/5.0 (compatible; Googlebot/2.1; +http://www.google.com/bot.html)";
/// MaxMind 官方测试库里的地址：`89.160.20.128` 是瑞典，`81.2.69.142` 是英国。
const SE_IP: &str = "89.160.20.128";
const GB_IP: &str = "81.2.69.142";

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

fn reader_client(url: &str) -> Client {
    rivu_storage_ch::client(url, &account("reader"), Some(&ch_database()))
}

fn admin_client(url: &str) -> Client {
    rivu_storage_ch::client(url, &account("migrator"), Some(&ch_database()))
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

/// 查询与看板端点用的配置：PG 用测试连接串，限流与规则上限可以按用例覆盖。
fn api_config(env: &TestEnv, extra: &[(&str, &str)]) -> Config {
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
    ];
    pairs.extend(
        extra
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned())),
    );
    Config::from_env(Role::Admin, &Env::from_pairs(pairs)).expect("测试配置")
}

fn unique(prefix: &str) -> String {
    format!("{prefix}-{:08x}", rand::random::<u32>())
}

async fn make_site(pg: &PgPool, name: &str) -> (u64, String) {
    let hostnames = vec![HOST.to_owned()];
    let created = rivu_storage_pg::sites::create_site(
        pg,
        &NewSite {
            name,
            timezone: "UTC",
            hostnames: &hostnames,
        },
    )
    .await
    .expect("建站");
    (created.id, created.site_key)
}

/// 规范化后添加一条规则。
async fn add_rule(
    pg: &PgPool,
    site_id: u64,
    kind: RuleType,
    action: RuleAction,
    value: &str,
    max: usize,
) -> AddOutcome {
    let value = normalize_value(kind, value).expect("规则取值");
    rivu_storage_pg::shield::add_rule(
        pg,
        &NewRule {
            site_id,
            kind: kind.as_str(),
            action: action.as_str(),
            value: &value,
            description: None,
            created_by: None,
        },
        max,
    )
    .await
    .expect("add_rule")
}

async fn counter_sums(client: &Client, site_id: u64) -> BTreeMap<String, u64> {
    #[derive(clickhouse::Row, serde::Deserialize)]
    struct R {
        metric: String,
        total: u64,
    }
    client
        .query(
            "SELECT metric, sum(value) AS total FROM ingest_counters \
             WHERE site_id = {site:UInt64} GROUP BY metric",
        )
        .param("site", site_id)
        .fetch_all::<R>()
        .await
        .expect("ingest_counters")
        .into_iter()
        .map(|r| (r.metric, r.total))
        .collect()
}

async fn event_count(client: &Client, site_id: u64) -> u64 {
    client
        .query("SELECT count() FROM events WHERE site_id = {site:UInt64}")
        .param("site", site_id)
        .fetch_one::<u64>()
        .await
        .expect("count")
}

async fn cleanup(admin: &Client, site_id: u64) {
    for table in ["events", "sessions", "ingest_counters"] {
        let _ = admin
            .query(&format!(
                "ALTER TABLE {table} DELETE WHERE site_id = {{site:UInt64}}"
            ))
            .param("site", site_id)
            .execute()
            .await;
    }
}

// ------------------------------------------------------------------ 用例 1：规则、限流、垃圾引荐

/// 一次发送：事件体、客户端 IP、UA、期望结果。
struct Send<'a> {
    site_key: &'a str,
    body: Value,
    ip: &'a str,
    ua: &'a str,
    want: Outcome,
}

fn pv(site_key: &str, url: &str) -> Value {
    json!({"v": 1, "s": site_key, "n": "pageview", "u": url})
}

#[tokio::test]
async fn rules_rate_limit_and_spam_counters_match_the_expectations() {
    let Some(env) = test_env("rules_rate_limit_and_spam_counters_match_the_expectations") else {
        return;
    };
    migrate_once(&env).await;
    let pg = rivu_storage_pg::connect(&env.pg_url, 8)
        .await
        .expect("连接 PG");

    // ---- 站点与规则 ------------------------------------------------------------------------
    let (site1, key1) = make_site(&pg, "rules-e2e").await;
    let (site2, key2) = make_site(&pg, "limit-e2e").await;
    sqlx::query("UPDATE site_settings SET ingest_rate_limit_per_min = 5 WHERE site_id = $1")
        .bind(site2 as i64)
        .execute(&pg)
        .await
        .expect("设置站点限额");

    let max = 500;
    let rules = [
        (RuleType::Ip, RuleAction::Deny, "203.0.113.50"),
        (RuleType::Cidr, RuleAction::Deny, "198.51.100.0/24"),
        (RuleType::Ip, RuleAction::Allow, "198.51.100.7"),
        (RuleType::Page, RuleAction::Deny, "/admin*"),
        (RuleType::Page, RuleAction::Allow, "/admin/public*"),
        (RuleType::Country, RuleAction::Deny, "se"),
    ];
    for (kind, action, value) in rules {
        let out = add_rule(&pg, site1, kind, action, value, max).await;
        assert!(
            matches!(out, AddOutcome::Added(_)),
            "{kind} {value}: {out:?}"
        );
    }
    // 同类型同取值：重复（不论动作）；规范化后相同也算重复
    assert_eq!(
        add_rule(
            &pg,
            site1,
            RuleType::Ip,
            RuleAction::Allow,
            "203.0.113.50",
            max
        )
        .await,
        AddOutcome::Duplicate
    );
    assert_eq!(
        add_rule(
            &pg,
            site1,
            RuleType::Cidr,
            RuleAction::Deny,
            "198.51.100.77/24",
            max
        )
        .await,
        AddOutcome::Duplicate,
        "198.51.100.77/24 规范化成 198.51.100.0/24"
    );
    let listed = rivu_storage_pg::shield::list_rules(&pg, site1)
        .await
        .expect("list");
    assert_eq!(listed.len(), rules.len());

    // ---- 管线 ------------------------------------------------------------------------------
    let today = utc_today();
    let days: Vec<_> = (-1i64..=1)
        .map(|d| today.saturating_add(jiff::Span::new().days(d)))
        .collect();
    rivu_storage_pg::salts::ensure_salts(&pg, &days)
        .await
        .expect("ensure_salts");
    let sites = Arc::new(SiteSnapshot::new());
    sites.reload(&pg).await.expect("加载站点快照");
    let salts = Arc::new(SaltCache::new());
    let (from, to) = salt_window(today);
    salts.reload(&pg, from, to).await.expect("加载盐");

    let geo = Arc::new(GeoDb::new());
    let mmdb = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../crates/geo/tests/data/test-data/GeoIP2-City-Test.mmdb");
    assert!(geo.reload_if_changed(&mmdb).expect("加载测试库"));

    let mut opts = WriterOptions::new(BufferConfig {
        flush_interval: Duration::from_millis(100),
        max_rows: 1000,
        max_bytes: 8 << 20,
        channel_capacity: 10_000,
    });
    opts.counter_flush_interval = Duration::from_secs(10);
    let writer = BatchWriter::spawn(
        Arc::new(ChSink::new(rivu_storage_ch::writer_client(
            &env.ch_url,
            &account("writer"),
            &ch_database(),
        ))),
        opts,
    );
    let pipeline = Pipeline::new(
        Arc::clone(&sites),
        Arc::clone(&salts),
        writer.handle(),
        vec!["127.0.0.1/32".parse().unwrap()],
        Arc::new(UaParser::embedded().expect("uap-core")),
        geo,
    );

    let url = |path: &str| format!("https://{HOST}{path}");
    let dropped = |r: DropReason| Outcome::Dropped(r);
    let ips2: Vec<String> = (0..8).map(|i| format!("203.0.113.{}", 100 + i)).collect();
    let mut sends: Vec<Send> = vec![
        // 正常
        Send {
            site_key: &key1,
            body: pv(&key1, &url("/")),
            ip: "203.0.113.9",
            ua: CHROME_WIN,
            want: Outcome::Accepted,
        },
        // IP 与 CIDR 规则（含 allow 例外与边界）
        Send {
            site_key: &key1,
            body: pv(&key1, &url("/")),
            ip: "203.0.113.50",
            ua: CHROME_WIN,
            want: dropped(DropReason::IpBlocked),
        },
        Send {
            site_key: &key1,
            body: pv(&key1, &url("/")),
            ip: "198.51.100.20",
            ua: CHROME_WIN,
            want: dropped(DropReason::IpBlocked),
        },
        Send {
            site_key: &key1,
            body: pv(&key1, &url("/")),
            ip: "198.51.100.0",
            ua: CHROME_WIN,
            want: dropped(DropReason::IpBlocked),
        },
        Send {
            site_key: &key1,
            body: pv(&key1, &url("/")),
            ip: "198.51.100.255",
            ua: CHROME_WIN,
            want: dropped(DropReason::IpBlocked),
        },
        Send {
            site_key: &key1,
            body: pv(&key1, &url("/")),
            ip: "198.51.100.7",
            ua: CHROME_WIN,
            want: Outcome::Accepted,
        },
        Send {
            site_key: &key1,
            body: pv(&key1, &url("/")),
            ip: "198.51.101.0",
            ua: CHROME_WIN,
            want: Outcome::Accepted,
        },
        Send {
            site_key: &key1,
            body: pv(&key1, &url("/")),
            ip: "198.51.99.255",
            ua: CHROME_WIN,
            want: Outcome::Accepted,
        },
        Send {
            site_key: &key1,
            body: pv(&key1, &url("/")),
            ip: "::ffff:203.0.113.50",
            ua: CHROME_WIN,
            want: dropped(DropReason::IpBlocked),
        },
        // 页面规则
        Send {
            site_key: &key1,
            body: pv(&key1, &url("/admin/x")),
            ip: "203.0.113.9",
            ua: CHROME_WIN,
            want: dropped(DropReason::PageBlocked),
        },
        Send {
            site_key: &key1,
            body: pv(&key1, &url("/admin")),
            ip: "203.0.113.9",
            ua: CHROME_WIN,
            want: dropped(DropReason::PageBlocked),
        },
        Send {
            site_key: &key1,
            body: pv(&key1, &url("/admin/public/x")),
            ip: "203.0.113.9",
            ua: CHROME_WIN,
            want: Outcome::Accepted,
        },
        Send {
            site_key: &key1,
            body: pv(&key1, &url("/Admin")),
            ip: "203.0.113.9",
            ua: CHROME_WIN,
            want: Outcome::Accepted,
        },
        // 国家规则（地理之后）
        Send {
            site_key: &key1,
            body: pv(&key1, &url("/")),
            ip: SE_IP,
            ua: CHROME_WIN,
            want: dropped(DropReason::CountryBlocked),
        },
        Send {
            site_key: &key1,
            body: pv(&key1, &url("/")),
            ip: GB_IP,
            ua: CHROME_WIN,
            want: Outcome::Accepted,
        },
        // 垃圾引荐、机器人、hostname
        Send {
            site_key: &key1,
            body: json!({"v":1,"s":key1,"n":"pageview","u":url("/"),"r":"https://semalt.com/x"}),
            ip: "203.0.113.9",
            ua: CHROME_WIN,
            want: dropped(DropReason::SpamReferrer),
        },
        Send {
            site_key: &key1,
            body: json!({"v":1,"s":key1,"n":"pageview","u":url("/"),"r":"https://www.google.com/"}),
            ip: "203.0.113.9",
            ua: CHROME_WIN,
            want: Outcome::Accepted,
        },
        Send {
            site_key: &key1,
            body: pv(&key1, &url("/")),
            ip: "203.0.113.9",
            ua: GOOGLEBOT,
            want: dropped(DropReason::Bot),
        },
        Send {
            site_key: &key1,
            body: pv(&key1, "https://other.test/"),
            ip: "203.0.113.9",
            ua: CHROME_WIN,
            want: dropped(DropReason::HostnameNotAllowed),
        },
        // engagement：规则同样适用；正常页面的 engagement 被接受
        Send {
            site_key: &key1,
            body: json!({"v":1,"s":key1,"n":"engagement","u":url("/admin/x"),"e":3000,"sd":10}),
            ip: "203.0.113.9",
            ua: CHROME_WIN,
            want: dropped(DropReason::PageBlocked),
        },
        Send {
            site_key: &key1,
            body: json!({"v":1,"s":key1,"n":"engagement","u":url("/"),"e":3000,"sd":10}),
            ip: "203.0.113.9",
            ua: CHROME_WIN,
            want: Outcome::Accepted,
        },
        Send {
            site_key: &key1,
            body: json!({"v":1,"s":key1,"n":"engagement","u":url("/"),"e":3000,"sd":10}),
            ip: "203.0.113.50",
            ua: CHROME_WIN,
            want: dropped(DropReason::IpBlocked),
        },
    ];
    // 站点 2（每分钟 5 个）：同一分钟内 8 个，前 5 个放行，后 3 个 rate_limited
    for (i, ip) in ips2.iter().enumerate() {
        let want = if i < 5 {
            Outcome::Accepted
        } else {
            dropped(DropReason::RateLimited)
        };
        sends.push(Send {
            site_key: &key2,
            body: pv(&key2, &url("/")),
            ip,
            ua: CHROME_WIN,
            want,
        });
    }

    let mut expected: BTreeMap<(u64, String), u64> = BTreeMap::new();
    for s in &sends {
        let site = if s.site_key == key1 { site1 } else { site2 };
        let now_ms = i64::try_from(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis(),
        )
        .unwrap();
        let got = pipeline.process(
            s.body.to_string().as_bytes(),
            RequestCtx {
                peer: "127.0.0.1".parse().unwrap(),
                xff: vec![s.ip.to_owned()],
                user_agent: s.ua.to_owned(),
                now_ms,
            },
        );
        assert_eq!(got, s.want, "{} {} {}", s.ip, s.body, s.ua);
        let metric = match &s.want {
            Outcome::Accepted => "accepted".to_owned(),
            Outcome::Dropped(r) => r.metric().to_owned(),
            other => panic!("没有预期之外的结果：{other:?}"),
        };
        *expected.entry((site, metric)).or_insert(0) += 1;
    }
    drop(pipeline);
    writer.shutdown().await.expect("关停写入器");

    // ---- 对账 ------------------------------------------------------------------------------
    let reader = reader_client(&env.ch_url);
    for (site, name) in [(site1, "站点 1"), (site2, "站点 2")] {
        let want: BTreeMap<String, u64> = expected
            .iter()
            .filter(|((s, _), _)| *s == site)
            .map(|((_, m), n)| (m.clone(), *n))
            .collect();
        let got = counter_sums(&reader, site).await;
        println!("{name} ingest_counters：{got:?}");
        assert_eq!(got, want, "{name}：accepted 与各 dropped 逐项相等");
        assert_eq!(
            event_count(&reader, site).await,
            want["accepted"],
            "{name}：events 行数 = accepted"
        );
    }
    // 用例里真的覆盖了每一种新原因（防止「全 0 对全 0」的假通过）
    let site1_counts = counter_sums(&reader, site1).await;
    for reason in [
        DropReason::IpBlocked,
        DropReason::PageBlocked,
        DropReason::CountryBlocked,
        DropReason::SpamReferrer,
        DropReason::Bot,
        DropReason::HostnameNotAllowed,
    ] {
        assert!(
            site1_counts.get(reason.metric()).copied().unwrap_or(0) >= 1,
            "{reason}"
        );
    }
    assert_eq!(
        counter_sums(&reader, site2).await["dropped_rate_limited"],
        3
    );

    let admin = admin_client(&env.ch_url);
    cleanup(&admin, site1).await;
    cleanup(&admin, site2).await;
}

#[tokio::test]
async fn shield_rule_cap_holds_under_concurrent_adds_and_removal_frees_a_slot() {
    let Some(env) =
        test_env("shield_rule_cap_holds_under_concurrent_adds_and_removal_frees_a_slot")
    else {
        return;
    };
    migrate_once(&env).await;
    let pg = rivu_storage_pg::connect(&env.pg_url, 12)
        .await
        .expect("连接 PG");
    let (site, key) = make_site(&pg, "cap-e2e").await;
    let max = 5;
    for i in 0..3 {
        let out = add_rule(
            &pg,
            site,
            RuleType::Page,
            RuleAction::Deny,
            &format!("/p{i}"),
            max,
        )
        .await;
        assert!(matches!(out, AddOutcome::Added(_)));
    }
    // 8 个并发添加，只剩 2 个名额：恰好 2 个成功，6 个达到上限
    let mut tasks = Vec::new();
    for i in 0..8 {
        let pg = pg.clone();
        tasks.push(tokio::spawn(async move {
            add_rule(
                &pg,
                site,
                RuleType::Ip,
                RuleAction::Deny,
                &format!("203.0.113.{i}"),
                max,
            )
            .await
        }));
    }
    let mut added = Vec::new();
    let mut limited = 0;
    for t in tasks {
        match t.await.unwrap() {
            AddOutcome::Added(id) => added.push(id),
            AddOutcome::LimitReached { limit } => {
                assert_eq!(limit, max);
                limited += 1;
            }
            other => panic!("{other:?}"),
        }
    }
    assert_eq!((added.len(), limited), (2, 6));
    let rules = rivu_storage_pg::shield::list_rules(&pg, site)
        .await
        .unwrap();
    assert_eq!(rules.len(), max);

    // 到上限后继续添加被拒；重复项仍然报告「重复」（先查重，再数，再插）
    assert_eq!(
        add_rule(&pg, site, RuleType::Country, RuleAction::Deny, "CN", max).await,
        AddOutcome::LimitReached { limit: max }
    );
    assert_eq!(
        add_rule(&pg, site, RuleType::Page, RuleAction::Allow, "/p0", max).await,
        AddOutcome::Duplicate
    );
    // 软删除一条：腾出名额，且同取值可以再次添加（部分唯一索引只看未删除行）
    assert!(
        rivu_storage_pg::shield::remove_rule_by_id(&pg, site, &added[0])
            .await
            .unwrap()
    );
    assert!(
        !rivu_storage_pg::shield::remove_rule_by_id(&pg, site, &added[0])
            .await
            .unwrap(),
        "已删除的不能再删"
    );
    assert!(
        rivu_storage_pg::shield::remove_rule_by_id(&pg, site + 1_000_000, &added[1])
            .await
            .map(|removed| !removed)
            .unwrap(),
        "规则属于别的站点时不能删"
    );
    assert!(
        rivu_storage_pg::shield::remove_rule_by_id(&pg, site, "not-a-uuid")
            .await
            .is_err()
    );
    assert!(matches!(
        add_rule(&pg, site, RuleType::Country, RuleAction::Deny, "CN", max).await,
        AddOutcome::Added(_)
    ));
    assert!(
        rivu_storage_pg::shield::remove_rule_by_value(&pg, site, "country", "CN")
            .await
            .unwrap()
    );
    // 站点已删除：规则不再出现在快照里
    sqlx::query("UPDATE sites SET deleted_at = now() WHERE id = $1")
        .bind(site as i64)
        .execute(&pg)
        .await
        .unwrap();
    let all = rivu_storage_pg::shield::load_all_rules(&pg).await.unwrap();
    assert!(all.iter().all(|r| r.site_id != site));
    assert_eq!(
        add_rule(&pg, site, RuleType::Country, RuleAction::Deny, "US", max).await,
        AddOutcome::SiteNotFound
    );
    let snapshot_rows = rivu_storage_pg::sites::load_sites(&pg).await.unwrap();
    assert!(snapshot_rows.iter().all(|s| s.site_key != key));
}

// ------------------------------------------------------------------ 用例 2：API key 全链路

struct Org {
    id: i64,
    slug: String,
    user_id: i64,
    email: String,
}

/// 建组织、用户与成员关系（直接 SQL：用户管理还没有 CLI）。
async fn make_org(pg: &PgPool, role: &str) -> Org {
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
    sqlx::query("INSERT INTO memberships (organization_id, user_id, role) VALUES ($1, $2, $3)")
        .bind(org_id)
        .bind(user_id)
        .bind(role)
        .execute(pg)
        .await
        .unwrap();
    Org {
        id: org_id,
        slug,
        user_id,
        email,
    }
}

async fn site_in_org(pg: &PgPool, org: &Org, public: bool) -> (u64, String) {
    let (id, key) = make_site(pg, "api-e2e").await;
    sqlx::query("UPDATE sites SET organization_id = $1, is_public = $2 WHERE id = $3")
        .bind(org.id)
        .bind(public)
        .bind(id as i64)
        .execute(pg)
        .await
        .unwrap();
    (id, key)
}

async fn new_key(pg: &PgPool, org: &Org, scopes: &[&str], expires_unix: Option<i64>) -> String {
    let k = generate();
    let scopes: Vec<String> = scopes.iter().map(|s| (*s).to_owned()).collect();
    rivu_storage_pg::api_keys::create_key(
        pg,
        &NewApiKey {
            organization_id: org.id,
            user_id: org.user_id,
            name: "e2e",
            key_prefix: k.prefix(),
            key_hash: k.hash(),
            scopes: &scopes,
            expires_at_unix: expires_unix,
        },
    )
    .await
    .unwrap();
    k.plaintext().to_owned()
}

type Guard = Arc<ApiGuard<PgStore>>;

async fn stats_handler(
    State(guard): State<Guard>,
    ctx: AuthContext,
    Path(site_key): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let site = guard
        .authorize_site(&ctx, &site_key, Resource::Stats, ScopeAction::Read)
        .await?;
    Ok(Json(json!({"site_id": site.id, "org": ctx.org_id})))
}

async fn slow_handler(State(gate): State<Arc<Semaphore>>) -> &'static str {
    let _p = gate.acquire().await.unwrap();
    "ok"
}

struct Resp {
    status: u16,
    headers: HeaderMap,
    body: String,
}

async fn call(app: &Router, path: &str, key: Option<&str>) -> Resp {
    let mut req = Request::builder().uri(path);
    if let Some(k) = key {
        req = req.header(header::AUTHORIZATION, format!("Bearer {k}"));
    }
    let resp = app
        .clone()
        .oneshot(req.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = resp.status().as_u16();
    let headers = resp.headers().clone();
    let body =
        String::from_utf8(to_bytes(resp.into_body(), 1 << 20).await.unwrap().to_vec()).unwrap();
    Resp {
        status,
        headers,
        body,
    }
}

fn hdr<'a>(r: &'a Resp, name: &str) -> Option<&'a str> {
    r.headers.get(name).and_then(|v| v.to_str().ok())
}

async fn last_used(pg: &PgPool, key: &str) -> Option<String> {
    sqlx::query_scalar("SELECT last_used_at::text FROM api_keys WHERE key_hash = $1")
        .bind(hash_key(key).as_slice())
        .fetch_one(pg)
        .await
        .unwrap()
}

#[tokio::test]
async fn api_key_chain_end_to_end() {
    let Some(env) = test_env("api_key_chain_end_to_end") else {
        return;
    };
    migrate_once(&env).await;
    let pg = rivu_storage_pg::connect(&env.pg_url, 8)
        .await
        .expect("连接 PG");
    // 并发上限 2、排队 300 ms；小时配额与突发用默认值（10000 / 60），个别组织用 api_hourly_limit 覆盖
    let config = api_config(
        &env,
        &[
            ("RIVU_API_MAX_CONCURRENCY", "2"),
            ("RIVU_API_QUEUE_TIMEOUT_MS", "300"),
        ],
    );
    let guard = api_auth::build_guard(&config, pg.clone());

    let org = make_org(&pg, "owner").await;
    let other = make_org(&pg, "owner").await;
    let (_, site) = site_in_org(&pg, &org, false).await;
    let (_, other_site) = site_in_org(&pg, &other, false).await;
    let (_, other_public) = site_in_org(&pg, &other, true).await;

    let stats = Router::new()
        .route("/stats/{site_key}", get(stats_handler))
        .with_state(Arc::clone(&guard));
    let gate = Arc::new(Semaphore::new(0));
    let slow = Router::new()
        .route("/slow", get(slow_handler))
        .with_state(Arc::clone(&gate));
    let required = Required::new(Resource::Stats, ScopeAction::Read);
    let app = protect(stats, Arc::clone(&guard), required).merge(protect(
        slow,
        Arc::clone(&guard),
        required,
    ));

    // 通过 issue_key（CLI 的实现）创建，明文只在返回值里
    let issued = issue_key(
        &pg,
        &ApikeyCreateArgs {
            org: org.slug.clone(),
            user: Some(org.email.clone()),
            name: "e2e".into(),
            scopes: vec!["stats:read".into()],
            expires: None,
        },
    )
    .await
    .expect("创建 key");
    let key = issued.plaintext.clone();
    assert!(key.starts_with("rivu_") && key.len() == 48);
    assert_eq!(issued.prefix, &key[..12]);
    // 库里只有哈希与前缀，没有明文
    let (stored_hash, stored_prefix, stored_scopes): (Vec<u8>, String, Vec<String>) =
        sqlx::query_as("SELECT key_hash, key_prefix, scopes FROM api_keys WHERE id = $1")
            .bind(issued.id)
            .fetch_one(&pg)
            .await
            .unwrap();
    assert_eq!(stored_hash, hash_key(&key).to_vec());
    assert_eq!(stored_prefix, &key[..12]);
    assert_eq!(stored_scopes, ["stats:read:*"]);
    let plaintext_hits: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM api_keys WHERE name = $1 OR key_prefix = $1 OR scopes::text LIKE '%' || $1 || '%'",
    )
    .bind(&key)
    .fetch_one(&pg)
    .await
    .unwrap();
    assert_eq!(plaintext_hits, 0);

    // 401：无 key、错 key（格式对但不存在）、格式错
    let path = format!("/stats/{site}");
    assert_eq!(call(&app, &path, None).await.status, 401);
    let never_issued = generate().plaintext().to_owned();
    assert_eq!(call(&app, &path, Some(&never_issued)).await.status, 401);
    assert_eq!(call(&app, &path, Some("rivu_nope")).await.status, 401);
    // 200：自己组织的站点，带 X-RateLimit-* 头
    let r = call(&app, &path, Some(&key)).await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(hdr(&r, "x-ratelimit-limit"), Some("10000"));
    assert_eq!(hdr(&r, "x-ratelimit-remaining"), Some("59"));
    // 404：别的组织的站点、不存在的站点——响应体相同；别的组织的公开站点可读统计
    let a = call(&app, &format!("/stats/{other_site}"), Some(&key)).await;
    let b = call(&app, "/stats/zzzzzzzzzzzz", Some(&key)).await;
    assert_eq!((a.status, b.status), (404, 404));
    assert_eq!(a.body, b.body);
    assert_eq!(
        call(&app, &format!("/stats/{other_public}"), Some(&key))
            .await
            .status,
        200
    );
    // 403：只有 sites:read 的 key 访问 stats 路由
    let sites_only = new_key(&pg, &org, &["sites:read:*"], None).await;
    assert_eq!(call(&app, &path, Some(&sites_only)).await.status, 403);
    // 404：scope 目标是别的站点
    let (_, second_site) = site_in_org(&pg, &org, false).await;
    let one_site = new_key(&pg, &org, &[&format!("stats:read:{second_site}")], None).await;
    assert_eq!(
        call(&app, &format!("/stats/{second_site}"), Some(&one_site))
            .await
            .status,
        200
    );
    assert_eq!(call(&app, &path, Some(&one_site)).await.status, 404);

    // last_used_at：第一次请求后被写入，并且节流（一分钟内不再变）
    let mut first = None;
    for _ in 0..40 {
        first = last_used(&pg, &key).await;
        if first.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let first = first.expect("last_used_at 应被写入");
    for _ in 0..5 {
        assert_eq!(call(&app, &path, Some(&key)).await.status, 200);
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        last_used(&pg, &key).await.as_deref(),
        Some(first.as_str()),
        "节流：一分钟内不再写库"
    );

    // 失效：过期、用户被禁用、被移出组织、组织被删除、吊销——都立即 401
    let expired = new_key(
        &pg,
        &org,
        &["stats:read:*"],
        Some(jiff::Timestamp::now().as_second() - 10),
    )
    .await;
    assert_eq!(call(&app, &path, Some(&expired)).await.status, 401);
    let future = new_key(
        &pg,
        &org,
        &["stats:read:*"],
        Some(jiff::Timestamp::now().as_second() + 3600),
    )
    .await;
    assert_eq!(call(&app, &path, Some(&future)).await.status, 200);

    let temp = make_org(&pg, "viewer").await;
    let (_, temp_site) = site_in_org(&pg, &temp, false).await;
    let temp_key = new_key(&pg, &temp, &["stats:read:*"], None).await;
    let temp_path = format!("/stats/{temp_site}");
    assert_eq!(call(&app, &temp_path, Some(&temp_key)).await.status, 200);
    sqlx::query("UPDATE users SET disabled_at = now() WHERE id = $1")
        .bind(temp.user_id)
        .execute(&pg)
        .await
        .unwrap();
    assert_eq!(
        call(&app, &temp_path, Some(&temp_key)).await.status,
        401,
        "用户被禁用"
    );
    sqlx::query("UPDATE users SET disabled_at = NULL WHERE id = $1")
        .bind(temp.user_id)
        .execute(&pg)
        .await
        .unwrap();
    assert_eq!(call(&app, &temp_path, Some(&temp_key)).await.status, 200);
    sqlx::query("UPDATE memberships SET deleted_at = now() WHERE organization_id = $1")
        .bind(temp.id)
        .execute(&pg)
        .await
        .unwrap();
    assert_eq!(
        call(&app, &temp_path, Some(&temp_key)).await.status,
        401,
        "用户被移出组织"
    );
    sqlx::query("UPDATE memberships SET deleted_at = NULL WHERE organization_id = $1")
        .bind(temp.id)
        .execute(&pg)
        .await
        .unwrap();
    sqlx::query("UPDATE organizations SET deleted_at = now() WHERE id = $1")
        .bind(temp.id)
        .execute(&pg)
        .await
        .unwrap();
    assert_eq!(
        call(&app, &temp_path, Some(&temp_key)).await.status,
        401,
        "组织被删除"
    );

    assert!(
        rivu_storage_pg::api_keys::revoke_key(&pg, issued.id)
            .await
            .unwrap()
    );
    assert_eq!(
        call(&app, &path, Some(&key)).await.status,
        401,
        "吊销后立即失效"
    );
    assert!(
        !rivu_storage_pg::api_keys::revoke_key(&pg, issued.id)
            .await
            .unwrap()
    );

    // 429：组织小时配额（api_hourly_limit = 3）
    let limited = make_org(&pg, "owner").await;
    sqlx::query("UPDATE organizations SET api_hourly_limit = 3 WHERE id = $1")
        .bind(limited.id)
        .execute(&pg)
        .await
        .unwrap();
    let (_, limited_site) = site_in_org(&pg, &limited, false).await;
    let k1 = new_key(&pg, &limited, &["stats:read:*"], None).await;
    let k2 = new_key(&pg, &limited, &["stats:read:*"], None).await;
    let lpath = format!("/stats/{limited_site}");
    assert_eq!(call(&app, &lpath, Some(&k1)).await.status, 200);
    assert_eq!(call(&app, &lpath, Some(&k2)).await.status, 200);
    let r = call(&app, &lpath, Some(&k1)).await;
    assert_eq!(r.status, 200);
    assert_eq!(hdr(&r, "x-ratelimit-limit"), Some("3"));
    assert_eq!(hdr(&r, "x-ratelimit-remaining"), Some("0"));
    let r = call(&app, &lpath, Some(&k2)).await;
    assert_eq!(r.status, 429, "同组织两把 key 共享配额");
    assert_eq!(hdr(&r, "retry-after"), Some("3600"));
    assert_eq!(hdr(&r, "x-ratelimit-remaining"), Some("0"));

    // 429：并发超限排队后超时（上限 2，排队 300 ms）
    let conc = make_org(&pg, "owner").await;
    let ck = new_key(&pg, &conc, &["stats:read:*"], None).await;
    let mut held = Vec::new();
    for _ in 0..2 {
        let (app, ck) = (app.clone(), ck.clone());
        held.push(tokio::spawn(
            async move { call(&app, "/slow", Some(&ck)).await },
        ));
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
    let started = std::time::Instant::now();
    let r = call(&app, "/slow", Some(&ck)).await;
    assert_eq!(r.status, 429);
    assert_eq!(
        hdr(&r, "retry-after"),
        Some("1"),
        "排队超时 300 ms 向上取整为 1 秒"
    );
    assert!(started.elapsed() >= Duration::from_millis(250));
    gate.add_permits(10);
    for h in held {
        assert_eq!(h.await.unwrap().status, 200);
    }
    // 释放后恢复
    assert_eq!(call(&app, "/slow", Some(&ck)).await.status, 200);
}

// ------------------------------------------------------------------ 用例 3：命令行入口

#[tokio::test]
async fn cli_commands_validate_and_write() {
    let Some(env) = test_env("cli_commands_validate_and_write") else {
        return;
    };
    migrate_once(&env).await;
    let pg = rivu_storage_pg::connect(&env.pg_url, 4)
        .await
        .expect("连接 PG");
    let config = api_config(&env, &[("RIVU_SHIELD_MAX_RULES_PER_SITE", "3")]);
    let (site, key) = make_site(&pg, "cli-e2e").await;

    let add = |kind: ShieldType, value: &str, action: ShieldActionKind| {
        let args = ShieldAddArgs {
            site: key.clone(),
            kind,
            value: value.into(),
            action,
            description: Some("测试".into()),
        };
        let config = config.clone();
        async move { shield_cmd::run(&config, ShieldAction::Add(args)).await }
    };
    // 取值校验：错误不写库
    for (kind, bad) in [
        (ShieldType::Ip, "10.0.0.0/8"),
        (ShieldType::Ip, "nope"),
        (ShieldType::Cidr, "10.0.0.1"),
        (ShieldType::Cidr, "10.0.0.0/40"),
        (ShieldType::Country, "CHN"),
        (ShieldType::Page, "admin"),
        (ShieldType::Hostname, "a.*.com"),
    ] {
        assert!(
            add(kind, bad, ShieldActionKind::Deny).await.is_err(),
            "{bad}"
        );
    }
    assert!(
        rivu_storage_pg::shield::list_rules(&pg, site)
            .await
            .unwrap()
            .is_empty()
    );
    // 正常添加：写入的是规范化后的值
    add(ShieldType::Cidr, "10.1.2.3/8", ShieldActionKind::Deny)
        .await
        .unwrap();
    add(ShieldType::Country, "cn", ShieldActionKind::Deny)
        .await
        .unwrap();
    add(ShieldType::Page, "/a**b", ShieldActionKind::Allow)
        .await
        .unwrap();
    let rules = rivu_storage_pg::shield::list_rules(&pg, site)
        .await
        .unwrap();
    let got: Vec<(&str, &str, &str)> = rules
        .iter()
        .map(|r| (r.kind.as_str(), r.action.as_str(), r.value.as_str()))
        .collect();
    assert_eq!(
        got,
        [
            ("cidr", "deny", "10.0.0.0/8"),
            ("country", "deny", "CN"),
            ("page", "allow", "/a*b")
        ]
    );
    // 重复、上限
    let e = add(ShieldType::Country, "CN", ShieldActionKind::Allow)
        .await
        .unwrap_err();
    assert!(format!("{e:#}").contains("已有同类型同取值"), "{e:#}");
    let e = add(ShieldType::Ip, "1.2.3.4", ShieldActionKind::Deny)
        .await
        .unwrap_err();
    assert!(format!("{e:#}").contains("上限 3"), "{e:#}");
    // 未知站点
    let e = shield_cmd::run(
        &config,
        ShieldAction::List(ShieldListArgs {
            site: "zzzzzzzzzzzz".into(),
        }),
    )
    .await
    .unwrap_err();
    assert!(format!("{e:#}").contains("找不到站点"));
    shield_cmd::run(
        &config,
        ShieldAction::List(ShieldListArgs { site: key.clone() }),
    )
    .await
    .unwrap();
    // 删除：按 id、按类型加取值（取值先规范化）
    shield_cmd::run(
        &config,
        ShieldAction::Remove(ShieldRemoveArgs {
            site: key.clone(),
            id: None,
            kind: Some(ShieldType::Cidr),
            value: Some("10.200.0.0/8".into()),
        }),
    )
    .await
    .unwrap();
    shield_cmd::run(
        &config,
        ShieldAction::Remove(ShieldRemoveArgs {
            site: key.clone(),
            id: Some(rules[1].id.clone()),
            kind: None,
            value: None,
        }),
    )
    .await
    .unwrap();
    let left = rivu_storage_pg::shield::list_rules(&pg, site)
        .await
        .unwrap();
    assert_eq!(left.len(), 1);
    assert!(
        shield_cmd::run(
            &config,
            ShieldAction::Remove(ShieldRemoveArgs {
                site: key.clone(),
                id: Some(rules[1].id.clone()),
                kind: None,
                value: None
            })
        )
        .await
        .is_err(),
        "已删除的规则再删报错"
    );
    // 规则进入站点快照并编译
    let sites = SiteSnapshot::new();
    sites.reload(&pg).await.unwrap();
    let cfg = sites.get(&key).expect("站点在快照里");
    assert_eq!(cfg.shield.len(), 1);
    assert!(
        !cfg.shield.page_blocked("/a/anything/b"),
        "allow 单独存在不屏蔽任何页面"
    );

    // ---- apikey ----
    let org = make_org(&pg, "owner").await;
    let viewer_email = format!("{}@example.test", unique("viewer"));
    let viewer_id: i64 = sqlx::query_scalar(
        "INSERT INTO users (email, password_hash) VALUES ($1, 'x') RETURNING id",
    )
    .bind(&viewer_email)
    .fetch_one(&pg)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO memberships (organization_id, user_id, role) VALUES ($1, $2, 'viewer')",
    )
    .bind(org.id)
    .bind(viewer_id)
    .execute(&pg)
    .await
    .unwrap();
    let outsider_email = format!("{}@example.test", unique("outsider"));
    sqlx::query("INSERT INTO users (email, password_hash) VALUES ($1, 'x')")
        .bind(&outsider_email)
        .execute(&pg)
        .await
        .unwrap();

    let create = |org: &str, user: Option<&str>, scopes: &[&str], expires: Option<&str>| {
        let args = ApikeyCreateArgs {
            org: org.into(),
            user: user.map(str::to_owned),
            name: "cli".into(),
            scopes: scopes.iter().map(|s| (*s).to_owned()).collect(),
            expires: expires.map(str::to_owned),
        };
        let pg = pg.clone();
        async move { issue_key(&pg, &args).await }
    };
    // 默认：org 的 owner、默认 scope、永不过期
    let k = create(&org.slug, None, &[], None).await.unwrap();
    assert_eq!(
        (k.user_email.as_str(), k.scopes.as_slice()),
        (org.email.as_str(), ["stats:read:*".to_owned()].as_slice())
    );
    assert!(k.expires_at.is_none());
    // 指定成员（邮箱不区分大小写）、多个 scope、时长过期
    let k2 = create(
        &org.slug.to_uppercase(),
        Some(&viewer_email.to_uppercase()),
        &["stats:read", "sites:read"],
        Some("30d"),
    )
    .await
    .unwrap();
    assert_eq!(k2.scopes, ["stats:read:*", "sites:read:*"]);
    assert!(k2.expires_at.is_some());
    // 错误：未知组织、非成员、不可用 scope、过去的过期时间
    assert!(create("no-such-org", None, &[], None).await.is_err());
    assert!(
        create(&org.slug, Some(&outsider_email), &[], None)
            .await
            .is_err()
    );
    assert!(
        create(&org.slug, None, &["members:read"], None)
            .await
            .is_err()
    );
    assert!(
        create(&org.slug, None, &[], Some("2020-01-01"))
            .await
            .is_err()
    );
    // 列表与吊销
    let rows = rivu_storage_pg::api_keys::list_keys(&pg, Some(&org.slug))
        .await
        .unwrap();
    assert_eq!(rows.len(), 2);
    assert!(
        rows.iter()
            .all(|r| !r.revoked && r.key_prefix.starts_with("rivu_"))
    );
    apikey_cmd::run(
        &config,
        ApikeyAction::List(ApikeyListArgs {
            org: Some(org.slug.clone()),
        }),
    )
    .await
    .unwrap();
    apikey_cmd::run(&config, ApikeyAction::Revoke(ApikeyRevokeArgs { id: k.id }))
        .await
        .unwrap();
    assert!(
        apikey_cmd::run(&config, ApikeyAction::Revoke(ApikeyRevokeArgs { id: k.id }))
            .await
            .is_err()
    );
    let rows = rivu_storage_pg::api_keys::list_keys(&pg, Some(&org.slug))
        .await
        .unwrap();
    assert_eq!(rows.iter().filter(|r| r.revoked).count(), 1);
    // 吊销的 key 查不到，另一把还在
    assert!(
        rivu_storage_pg::api_keys::find_active_by_hash(&pg, &hash_key(&k.plaintext))
            .await
            .unwrap()
            .is_none()
    );
    let rec = rivu_storage_pg::api_keys::find_active_by_hash(&pg, &hash_key(&k2.plaintext))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (rec.organization_id, rec.user_id, rec.role.as_str()),
        (org.id, viewer_id, "viewer")
    );
}
