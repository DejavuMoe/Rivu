//! 端到端：`Pipeline::process` -> SessionAssigner -> BatchWriter -> ClickHouse -> `rivu_query::overview`，
//! 与独立的参考实现（`rivu_reference`）在同一批事件上算出的真值逐项比较。
//!
//! 需要真实的 PostgreSQL 和 ClickHouse。缺少环境变量时跳过并打印原因：
//!   RIVU_TEST_DATABASE_URL     有 DDL 与写权限的 PostgreSQL 连接串（rivu_migrator）
//!   RIVU_TEST_CLICKHOUSE_URL   ClickHouse HTTP 地址
//! 可选（默认值是 deploy/compose.dev.yml 里的开发账号）：
//!   RIVU_TEST_CH_{MIGRATOR,WRITER,READER}_{USER,PASSWORD}
//!   RIVU_TEST_CH_DATABASE      默认 rivu
//!
//! 事件时间由测试控制（直接给 `RequestCtx::now_ms`），放在过去的固定日期 2026-03-28..30（柏林时区），
//! 覆盖 3 月 29 日的夏令时切换日（23 小时）和本地午夜。

use std::collections::{BTreeMap, HashSet};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use clickhouse::Client;
use jiff::Timestamp;
use jiff::civil::{Date, date};
use jiff::tz::TimeZone;
use rivu::migrate;
use rivu_common::config::{BufferConfig, ChAccount, Cidr, Config, Env, Role, Secret};
use rivu_domain::{DropReason, EventKind};
use rivu_ingest::pipeline::{Outcome, Pipeline, RequestCtx, SaltCache, SiteSnapshot};
use rivu_query::Overview;
use rivu_reference::{Metrics, RawEvent, RefSession, metrics, sessionize};
use rivu_storage_ch::rows::{EventRow, IngestCounterRow};
use rivu_storage_pg::sites::NewSite;
use rivu_writer::{BatchSink, BatchWriter, ChSink, WriterOptions};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::OnceCell;

const HOST: &str = "example.test";
const EPS: f64 = 1e-9;

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

/// 测试环境；缺变量时返回 None（调用方跳过）。
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

/// 迁移只做一次（两个测试并行时，第二个等第一个做完）。
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

/// 包一层 ChSink，记下每个成功写入的 events 与 ingest_counters 批次（token 与内容），供去重重放用。
struct RecordingSink {
    inner: ChSink,
    batches: Mutex<Vec<(String, Vec<EventRow>)>>,
    counter_batches: Mutex<Vec<(String, Vec<IngestCounterRow>)>>,
}

impl BatchSink for RecordingSink {
    async fn insert_events(&self, rows: &[EventRow], token: &str) -> anyhow::Result<()> {
        self.inner.insert_events(rows, token).await?;
        self.batches
            .lock()
            .unwrap()
            .push((token.to_owned(), rows.to_vec()));
        Ok(())
    }

    async fn insert_counters(&self, rows: &[IngestCounterRow], token: &str) -> anyhow::Result<()> {
        self.inner.insert_counters(rows, token).await?;
        self.counter_batches
            .lock()
            .unwrap()
            .push((token.to_owned(), rows.to_vec()));
        Ok(())
    }
}

fn berlin() -> TimeZone {
    TimeZone::get("Europe/Berlin").expect("tzdb")
}

/// 柏林本地时间 → Unix 毫秒。
fn at(d: Date, h: i8, m: i8) -> i64 {
    d.at(h, m, 0, 0)
        .to_zoned(berlin())
        .expect("本地时间有效")
        .timestamp()
        .as_millisecond()
}

fn local_date(ts_ms: i64) -> Date {
    Timestamp::from_millisecond(ts_ms)
        .unwrap()
        .to_zoned(berlin())
        .date()
}

/// 一个「真实的人」：不同的 (X-Forwarded-For, UA)。
struct Person {
    id: u64,
    ip: String,
    ua: String,
}

fn person(id: u64) -> Person {
    Person {
        id,
        ip: format!("203.0.113.{id}"),
        ua: format!("Mozilla/5.0 (e2e person {id})"),
    }
}

/// 管线返回结果的计数，用来和 `ingest_counters` 对账。
#[derive(Default, Debug)]
struct Tally {
    accepted: u64,
    /// `ingest_counters.metric` -> 次数
    dropped: BTreeMap<&'static str, u64>,
    overloaded: u64,
    rejected: u64,
}

impl Tally {
    fn record(&mut self, outcome: &Outcome) {
        match outcome {
            Outcome::Accepted => self.accepted += 1,
            Outcome::Dropped(r) => *self.dropped.entry(r.metric()).or_insert(0) += 1,
            Outcome::Overloaded => self.overloaded += 1,
            Outcome::Rejected(_) => self.rejected += 1,
        }
    }

    fn dropped_count(&self, reason: DropReason) -> u64 {
        self.dropped.get(reason.metric()).copied().unwrap_or(0)
    }
}

/// 驱动管线的小模拟器：每个事件带显式时间，同时按「本该被计入」的口径记下参考实现的输入。
struct Sim {
    pipeline: Arc<Pipeline>,
    site_key: String,
    site_id: u64,
    /// 发给本站点、hostname 合法的事件（不管管线最终接受还是丢弃）：参考实现自己决定哪些会并入会话。
    raw: Vec<RawEvent>,
    tally: Tally,
    /// 本站点被管线处理（非 400、非未知站点）的请求数。
    site_calls: u64,
}

impl Sim {
    fn call(&mut self, p: &Person, at_ms: i64, body: &Value) -> Outcome {
        let out = self.pipeline.process(
            body.to_string().as_bytes(),
            RequestCtx {
                peer: "127.0.0.1".parse().unwrap(),
                xff: vec![p.ip.clone()],
                user_agent: p.ua.clone(),
                now_ms: at_ms,
            },
        );
        self.tally.record(&out);
        out
    }

    fn push_raw(
        &mut self,
        p: &Person,
        ts: i64,
        kind: EventKind,
        path: &str,
        interactive: bool,
        e: u32,
    ) {
        self.raw.push(RawEvent {
            site_id: self.site_id,
            person: p.id,
            ts_ms: ts,
            kind,
            hostname: HOST.into(),
            pathname: path.into(),
            interactive,
            engagement_ms: e,
        });
    }

    fn url(path: &str) -> String {
        format!("https://{HOST}{path}")
    }

    fn pv(&mut self, p: &Person, ts: i64, path: &str) {
        let body =
            json!({"v": 1, "sv": 1, "s": self.site_key, "n": "pageview", "u": Self::url(path)});
        self.push_raw(p, ts, EventKind::Pageview, path, true, 0);
        self.site_calls += 1;
        assert_eq!(
            self.call(p, ts, &body),
            Outcome::Accepted,
            "pageview {path}"
        );
    }

    /// engagement：`expect_session` 为 false 时期望被丢弃（没有会话）。
    fn eng(&mut self, p: &Person, ts: i64, path: &str, ms: u32, sd: u8, expect_session: bool) {
        let body = json!({"v": 1, "sv": 1, "s": self.site_key, "n": "engagement",
                          "u": Self::url(path), "sd": sd, "e": ms});
        self.push_raw(p, ts, EventKind::Engagement, path, true, ms);
        self.site_calls += 1;
        let want = if expect_session {
            Outcome::Accepted
        } else {
            Outcome::Dropped(DropReason::NoSession)
        };
        assert_eq!(self.call(p, ts, &body), want, "engagement {path}");
    }

    /// 自定义事件；`interactive` 为 false 时发 `"i": false`。
    fn custom(&mut self, p: &Person, ts: i64, name: &str, path: &str, interactive: bool) {
        let mut body =
            json!({"v": 1, "sv": 1, "s": self.site_key, "n": name, "u": Self::url(path)});
        if !interactive {
            body["i"] = json!(false);
        }
        self.push_raw(p, ts, EventKind::Custom, path, interactive, 0);
        self.site_calls += 1;
        assert_eq!(self.call(p, ts, &body), Outcome::Accepted, "custom {name}");
    }
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() <= EPS
}

fn assert_same_metrics(name: &str, q: &Overview, t: &Metrics, visitor_deviation: i64) {
    assert_eq!(q.visits, t.visits, "{name}: visits");
    assert_eq!(q.pageviews, t.pageviews, "{name}: pageviews");
    assert!(
        close(q.bounce_rate, t.bounce_rate),
        "{name}: bounce_rate 查询 {} 真值 {}",
        q.bounce_rate,
        t.bounce_rate
    );
    assert!(
        close(q.visit_duration, t.visit_duration),
        "{name}: visit_duration 查询 {} 真值 {}",
        q.visit_duration,
        t.visit_duration
    );
    assert!(
        close(q.engaged_duration, t.engaged_duration),
        "{name}: engaged_duration 查询 {} 真值 {}",
        q.engaged_duration,
        t.engaged_duration
    );
    // visitors：没有 Q9 情形的范围必须相等；有的范围 = 真值 + 已知偏差（见调用处的注释）。
    assert_eq!(
        i64::try_from(q.visitors).unwrap(),
        i64::try_from(t.visitors).unwrap() + visitor_deviation,
        "{name}: visitors 查询值应 = 真值 {} + 已知偏差 {visitor_deviation}",
        t.visitors
    );
}

/// §10.1 Q9 暂行口径下，系统应当给出的 visitors，由参考实现的会话独立推出：
/// 盖章后每个非 engagement 事件的 `visitor_id` 是「会话原始 ID」，即 (人, 会话起始事件的本地日期)
/// 的哈希；范围内不同 ID 的个数就是 `uniq(visitor_id)`。
fn q9_visitors(
    events: &[RawEvent],
    sessions: &[RefSession],
    site_id: u64,
    from: Date,
    to: Date,
) -> u64 {
    let mut ids: HashSet<(u64, Date)> = HashSet::new();
    for s in sessions.iter().filter(|s| s.site_id == site_id) {
        for &i in &s.event_indices {
            let e = &events[i];
            let d = local_date(e.ts_ms);
            if e.kind != EventKind::Engagement && d >= from && d <= to {
                ids.insert((s.person, local_date(s.started_at_ms)));
            }
        }
    }
    ids.len() as u64
}

async fn count(client: &Client, table: &str, site_id: u64) -> u64 {
    client
        .query(&format!(
            "SELECT count() FROM {table} WHERE site_id = {{site:UInt64}}"
        ))
        .param("site", site_id)
        .fetch_one::<u64>()
        .await
        .expect("count")
}

/// `sessions` 合并后的会话数（按排序键分组，合并 AggregatingMergeTree 里尚未合并的部分行）。
async fn merged_sessions(client: &Client, site_id: u64) -> u64 {
    client
        .query(
            "SELECT count() FROM (SELECT 1 FROM sessions WHERE site_id = {site:UInt64} \
             GROUP BY site_id, start_date, visitor_id, session_id)",
        )
        .param("site", site_id)
        .fetch_one::<u64>()
        .await
        .expect("会话数")
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

fn days(from: Date, to: Date) -> Vec<Date> {
    let mut out = Vec::new();
    let mut d = from;
    while d <= to {
        out.push(d);
        d = d.tomorrow().unwrap();
    }
    out
}

#[tokio::test]
async fn pipeline_to_clickhouse_matches_the_reference() {
    let Some(env) = test_env("pipeline_to_clickhouse_matches_the_reference") else {
        return;
    };
    migrate_once(&env).await;

    // ---- 站点、盐 -------------------------------------------------------------------------
    let pg = rivu_storage_pg::connect(&env.pg_url, 4)
        .await
        .expect("连接 PG");
    let hostnames = vec![HOST.to_owned()];
    let created = rivu_storage_pg::sites::create_site(
        &pg,
        &NewSite {
            name: "e2e-lite",
            timezone: "Europe/Berlin",
            hostnames: &hostnames,
        },
    )
    .await
    .expect("建站");
    let (site_id, site_key) = (created.id, created.site_key.clone());
    println!("站点 site_key={site_key} site_id={site_id}");

    let (d27, d28, d29, d30, d31) = (
        date(2026, 3, 27),
        date(2026, 3, 28),
        date(2026, 3, 29),
        date(2026, 3, 30),
        date(2026, 3, 31),
    );
    // 事件本地日期 3/28..3/30，前一天的盐（3/27）用于跨午夜续会话；多备两天
    rivu_storage_pg::salts::ensure_salts(&pg, &days(date(2026, 3, 26), date(2026, 4, 1)))
        .await
        .expect("ensure_salts");

    let sites = Arc::new(SiteSnapshot::new());
    sites.reload(&pg).await.expect("加载站点快照");
    assert!(sites.get(&site_key).is_some(), "新站点应在快照里");
    let salts = Arc::new(SaltCache::new());
    salts
        .reload(&pg, date(2026, 3, 20), date(2026, 4, 10))
        .await
        .expect("加载盐");

    // ---- 写入器与管线 -----------------------------------------------------------------------
    let sink = Arc::new(RecordingSink {
        inner: ChSink::new(rivu_storage_ch::writer_client(
            &env.ch_url,
            &account("writer"),
            &ch_database(),
        )),
        batches: Mutex::new(Vec::new()),
        counter_batches: Mutex::new(Vec::new()),
    });
    let mut opts = WriterOptions::new(BufferConfig {
        flush_interval: Duration::from_millis(100),
        // 小批：保证会有多个批次，去重重放才有意义
        max_rows: 10,
        max_bytes: 8 << 20,
        channel_capacity: 10_000,
    });
    opts.counter_flush_interval = Duration::from_secs(10);
    let writer = BatchWriter::spawn(Arc::clone(&sink), opts);
    let trusted: Vec<Cidr> = vec!["127.0.0.1/32".parse().unwrap()];
    let pipeline = Arc::new(Pipeline::new(
        Arc::clone(&sites),
        Arc::clone(&salts),
        writer.handle(),
        trusted,
    ));

    let mut sim = Sim {
        pipeline,
        site_key: site_key.clone(),
        site_id,
        raw: Vec::new(),
        tally: Tally::default(),
        site_calls: 0,
    };

    // ---- 场景（柏林本地时间）-------------------------------------------------------------------
    let (p1, p2, p3, p4, p5, p6, p7, p8, p9) = (
        person(1),
        person(2),
        person(3),
        person(4),
        person(5),
        person(6),
        person(7),
        person(8),
        person(9),
    );

    // P1：3/28 一个多页会话（含 engagement 与 i:true 的自定义事件）；晚上再来一次（i:false 不算互动，跳出）；3/30 再来
    sim.pv(&p1, at(d28, 12, 0), "/");
    sim.pv(&p1, at(d28, 12, 2), "/pricing");
    sim.pv(&p1, at(d28, 12, 5), "/docs");
    sim.eng(&p1, at(d28, 12, 6), "/docs", 7000, 80, true);
    sim.custom(&p1, at(d28, 12, 7), "Signup", "/docs", true);
    sim.eng(&p1, at(d28, 12, 8), "/docs", 3000, 90, true);
    sim.pv(&p1, at(d28, 20, 0), "/pricing");
    sim.custom(&p1, at(d28, 20, 1), "Newsletter", "/pricing", false);
    sim.pv(&p1, at(d30, 10, 0), "/");
    sim.eng(&p1, at(d30, 10, 1), "/", 4000, 40, true);

    // P2：3/28 单页跳出；3/30 再来一次单页跳出
    sim.pv(&p2, at(d28, 15, 0), "/blog");
    sim.pv(&p2, at(d30, 9, 0), "/blog");

    // P3：跨本地午夜（3/28 23:50 -> 3/29 00:10）续上同一个会话，午夜后还有 engagement；
    // 3/29 05:00 又开一个新会话 —— §10.1 Q9 的情形 (a)：3/29 当天按盖章后的 ID 会多算一个访客
    sim.pv(&p3, at(d28, 23, 50), "/");
    sim.pv(&p3, at(d29, 0, 10), "/docs");
    sim.eng(&p3, at(d29, 0, 12), "/docs", 5000, 30, true);
    sim.pv(&p3, at(d29, 5, 0), "/pricing");

    // P4：夏令时切换日。01:50 CET 与 03:10 CEST 只隔 20 分钟（中间跳过 02:00–03:00），属于同一个会话；
    // 23:40 又开一个会话并跨午夜到 3/30 00:05 —— Q9 的情形 (b)：跨天范围里这个人只算一次
    sim.pv(&p4, at(d29, 1, 50), "/dst");
    sim.pv(&p4, at(d29, 3, 10), "/dst2");
    sim.pv(&p4, at(d29, 23, 40), "/late");
    sim.pv(&p4, at(d30, 0, 5), "/late2");

    // P5：间隔 31 分钟 -> 两个会话
    sim.pv(&p5, at(d29, 10, 0), "/");
    sim.pv(&p5, at(d29, 10, 31), "/x");

    // P6：间隔恰好 30 分钟 -> 仍是同一个会话（续期条件 ≤ 30 min）
    sim.pv(&p6, at(d29, 11, 0), "/");
    sim.pv(&p6, at(d29, 11, 30), "/b");
    sim.pv(&p6, at(d29, 12, 0), "/c");

    // P7：没有会话的 engagement 被丢弃；之后正常访问并带 engagement
    sim.eng(&p7, at(d29, 9, 0), "/x", 1000, 10, false);
    sim.pv(&p7, at(d29, 9, 10), "/");
    sim.eng(&p7, at(d29, 9, 12), "/", 2000, 50, true);

    // P8：先发 i:true 的自定义事件开会话，再看一页 -> 有互动，不是跳出
    sim.custom(&p8, at(d30, 14, 0), "Purchase", "/checkout", true);
    sim.pv(&p8, at(d30, 14, 1), "/checkout");

    // P9：先发 i:false 的自定义事件开会话，再看一页 -> 没有互动，仍是跳出
    sim.custom(&p9, at(d30, 16, 0), "Ping", "/", false);
    sim.pv(&p9, at(d30, 16, 1), "/");

    // 不该被计入的请求：未知站点 x2、hostname 不在白名单 x1、400 x3
    let probe = person(10);
    let invalid = |sim: &mut Sim, body: Value| sim.call(&probe, at(d29, 12, 0), &body);
    for _ in 0..2 {
        let out = invalid(
            &mut sim,
            json!({"v": 1, "s": "zzzzzzzzzzzz", "n": "pageview", "u": Sim::url("/")}),
        );
        assert_eq!(out, Outcome::Dropped(DropReason::UnknownSite));
    }
    let out = invalid(
        &mut sim,
        json!({"v": 1, "s": site_key, "n": "pageview", "u": "https://evil.test/"}),
    );
    assert_eq!(out, Outcome::Dropped(DropReason::HostnameNotAllowed));
    sim.site_calls += 1;
    for bad in [
        json!({"v": 1, "n": "pageview"}),
        json!({"v": 2, "s": site_key, "n": "pageview", "u": Sim::url("/")}),
        json!({"v": 1, "s": site_key, "n": "engagement", "u": Sim::url("/")}),
    ] {
        assert!(matches!(invalid(&mut sim, bad), Outcome::Rejected(_)));
    }
    let out = sim.pipeline.process(
        b"{not json",
        RequestCtx {
            peer: "127.0.0.1".parse().unwrap(),
            xff: vec![],
            user_agent: String::new(),
            now_ms: at(d29, 12, 0),
        },
    );
    assert!(matches!(out, Outcome::Rejected(_)));
    sim.tally.record(&out);

    // ---- flush ----------------------------------------------------------------------------
    let Sim {
        raw,
        tally,
        site_calls,
        pipeline,
        ..
    } = sim;
    drop(pipeline);
    writer.shutdown().await.expect("关停写入器");

    println!("管线结果：{tally:?}，本站点被处理的请求 {site_calls}");
    assert_eq!(tally.overloaded, 0);
    assert_eq!(tally.rejected, 4);
    assert_eq!(tally.dropped_count(DropReason::UnknownSite), 2);
    assert_eq!(tally.dropped_count(DropReason::HostnameNotAllowed), 1);
    assert_eq!(tally.dropped_count(DropReason::NoSession), 1);

    // ---- 参考实现的真值 -------------------------------------------------------------------------
    let ref_sessions = sessionize(&raw);
    let absorbed: usize = ref_sessions.iter().map(|s| s.event_indices.len()).sum();
    assert_eq!(
        absorbed as u64, tally.accepted,
        "参考实现并入会话的事件数 = 管线接受的事件数"
    );
    assert_eq!(
        raw.len() as u64,
        tally.accepted + tally.dropped_count(DropReason::NoSession),
        "合法事件 = 接受 + 没有会话的 engagement"
    );
    assert_eq!(ref_sessions.len(), 15, "场景里一共 15 个会话");

    // ---- ClickHouse 里的行数 -------------------------------------------------------------------
    let reader = reader_client(&env.ch_url);
    assert_eq!(
        count(&reader, "events", site_id).await,
        tally.accepted,
        "events 行数 = 接受数"
    );
    assert_eq!(
        merged_sessions(&reader, site_id).await,
        ref_sessions.len() as u64,
        "sessions 合并后的会话数 = 参考实现的会话数"
    );

    // ---- 各范围的指标对比 -------------------------------------------------------------------------
    // 最后一列是 visitors 的已知偏差（查询值 − 真值），来自 §10.1 Q9：盖章把跨午夜续上的事件的 visitor_id
    // 改写成会话起始日的 ID。本场景里：
    //   P3 在 3/29 有两个会话（00:10 续上 3/28 的，05:00 新开的），按盖章后的 ID 当天算 2 个人（真值 1）：+1；
    //   P4 的会话 3/29 23:40 -> 3/30 00:05，3/30 的事件仍是 3/29 的 ID，跨过两天的范围里只算 1 次（真值 2）：−1。
    //   R2 [3/29]：+1；R4 [3/28,3/29]：P3 两个 ID 恰好对应两天，P4 只在 3/29 —— 0；
    //   R5 [3/29,3/30]：+1 −1 = 0；R6 [3/28,3/30]：只剩 P4 的 −1。Q9 定案（不再改写 ID）后这一列应当全部变成 0。
    let ranges: [(&str, Date, Date, i64); 8] = [
        ("R1 3/28", d28, d28, 0),
        ("R2 3/29（夏令时切换日，23 小时）", d29, d29, 1),
        ("R3 3/30", d30, d30, 0),
        ("R4 3/28-3/29（跨午夜两天）", d28, d29, 0),
        ("R5 3/29-3/30（P3 的 +1 与 P4 的 −1 抵消）", d29, d30, 0),
        ("R6 3/28-3/30（整个范围）", d28, d30, -1),
        ("R7 3/27（无数据）", d27, d27, 0),
        ("R8 3/31（无数据）", d31, d31, 0),
    ];
    let tz = berlin();
    let mut overviews = Vec::new();
    println!("范围 | 真值 visitors/visits/pageviews/bounce/duration/engaged | 查询值");
    for (name, from, to, deviation) in ranges {
        let truth = metrics(&raw, &ref_sessions, site_id, &tz, from, to);
        let q = rivu_query::overview(&reader, site_id, &tz, from, to)
            .await
            .expect("overview");
        println!(
            "{name} | {}/{}/{}/{:.6}/{:.6}/{:.6} | {}/{}/{}/{:.6}/{:.6}/{:.6}",
            truth.visitors,
            truth.visits,
            truth.pageviews,
            truth.bounce_rate,
            truth.visit_duration,
            truth.engaged_duration,
            q.visitors,
            q.visits,
            q.pageviews,
            q.bounce_rate,
            q.visit_duration,
            q.engaged_duration,
        );
        assert_same_metrics(name, &q, &truth, deviation);
        // 暂行口径的独立推导：Q9 的 visitors 由参考实现的会话独立推出，应与查询值一致
        assert_eq!(
            q.visitors,
            q9_visitors(&raw, &ref_sessions, site_id, from, to),
            "{name}: visitors = 按盖章 ID 数出的不同访客数"
        );
        overviews.push(q);
    }
    // 空范围：全零
    for q in &overviews[6..] {
        assert_eq!(*q, Overview::default());
    }
    // 有数据的范围确实有数据（防止「全 0 对全 0」的假通过）
    assert!(overviews[5].visits == 15 && overviews[5].pageviews == 22);

    // 页面榜：3/28 浏览最多的页面
    let top = rivu_query::top_pages(&reader, site_id, &tz, d28, d28, 3)
        .await
        .expect("top_pages");
    assert_eq!(top.len(), 3);
    assert!(top.windows(2).all(|w| w[0].visitors >= w[1].visitors));

    // ---- 对账：ingest_counters ----------------------------------------------------------------
    let counters = counter_sums(&reader, site_id).await;
    println!("ingest_counters（本站点）：{counters:?}");
    let mut expected: BTreeMap<String, u64> = BTreeMap::new();
    expected.insert("accepted".into(), tally.accepted);
    for (metric, n) in &tally.dropped {
        if *metric != DropReason::UnknownSite.metric() {
            expected.insert((*metric).to_owned(), *n);
        }
    }
    assert_eq!(
        counters, expected,
        "本站点的 accepted 与各 dropped 逐项相等"
    );
    let total: u64 = counters.values().sum();
    assert_eq!(
        total, site_calls,
        "ingest_counters 总和 = 本站点被处理（Accepted/Dropped/Overloaded）的次数"
    );
    // 未知站点记在 site_id = 0 上，该行被别的测试共享，只核对「至少」这么多
    let unknown = counter_sums(&reader, 0).await;
    assert!(
        unknown.get("dropped_unknown_site").copied().unwrap_or(0) >= 2,
        "{unknown:?}"
    );

    // ---- 去重：同一批同一个 token 重放，events 与 sessions 都不翻倍 -----------------------------
    let batches = sink.batches.lock().unwrap().clone();
    assert!(batches.len() >= 2, "应当有多个批次：{}", batches.len());
    let tokens: HashSet<&String> = batches.iter().map(|(t, _)| t).collect();
    assert_eq!(tokens.len(), batches.len(), "每批的 token 唯一");
    assert!(
        batches.iter().all(|(t, _)| t.contains(':')),
        "token 形如 node:seq"
    );
    let events_before = count(&reader, "events", site_id).await;
    let sessions_rows_before = count(&reader, "sessions", site_id).await;
    for (token, rows) in &batches {
        sink.inner
            .insert_events(rows, token)
            .await
            .expect("用同一个 token 重放");
    }
    assert_eq!(
        count(&reader, "events", site_id).await,
        events_before,
        "重放后 events 行数不变"
    );
    assert_eq!(
        count(&reader, "sessions", site_id).await,
        sessions_rows_before,
        "重放后 sessions 的部分聚合行数不变（MV 去重生效）"
    );
    let replayed = rivu_query::overview(&reader, site_id, &tz, d28, d30)
        .await
        .expect("overview");
    assert_eq!(replayed, overviews[5], "重放后整个范围的指标不变");

    // 计数批次同理：同一个 token 重放不能让 ingest_counters 重复累加（迁移 20261022100000 打开了去重窗口），
    // 否则「写入成功但响应丢失」的重试会破坏与请求数的对账。
    let counter_batches = sink.counter_batches.lock().unwrap().clone();
    assert!(!counter_batches.is_empty(), "应当至少写过一批计数");
    let counters_before = counter_sums(&reader, site_id).await;
    for (token, rows) in &counter_batches {
        sink.inner
            .insert_counters(rows, token)
            .await
            .expect("用同一个 token 重放计数");
    }
    assert_eq!(
        counter_sums(&reader, site_id).await,
        counters_before,
        "重放后 ingest_counters 不变"
    );

    // 对照：换一个 token 就是新的一批，应该被计入（证明上面不是因为什么都没写进去）
    let (_, first_rows) = &batches[0];
    sink.inner
        .insert_events(first_rows, "e2e-control-token")
        .await
        .expect("新 token");
    assert_eq!(
        count(&reader, "events", site_id).await,
        events_before + first_rows.len() as u64
    );

    cleanup(&admin_client(&env.ch_url), site_id).await;
}

/// 通过真实 HTTP 验证状态码与响应头（路由与头部的细节在 `public.rs` 的单元测试里）；
/// 事件用真实时钟，盐用 UTC 的今天前后各一天。
#[tokio::test]
async fn http_endpoint_end_to_end() {
    let Some(env) = test_env("http_endpoint_end_to_end") else {
        return;
    };
    migrate_once(&env).await;

    let pg = rivu_storage_pg::connect(&env.pg_url, 4)
        .await
        .expect("连接 PG");
    let hostnames = vec![HOST.to_owned()];
    let created = rivu_storage_pg::sites::create_site(
        &pg,
        &NewSite {
            name: "e2e-http",
            timezone: "UTC",
            hostnames: &hostnames,
        },
    )
    .await
    .expect("建站");
    let today = rivu_ingest::pipeline::utc_today();
    let window = days(
        today.saturating_sub(jiff::Span::new().days(2)),
        today.saturating_add(jiff::Span::new().days(1)),
    );
    rivu_storage_pg::salts::ensure_salts(&pg, &window)
        .await
        .expect("ensure_salts");

    let sites = Arc::new(SiteSnapshot::new());
    sites.reload(&pg).await.unwrap();
    let salts = Arc::new(SaltCache::new());
    let (from, to) = rivu_ingest::pipeline::salt_window(today);
    salts.reload(&pg, from, to).await.unwrap();
    assert!(salts.covers(today));

    let writer = BatchWriter::spawn(
        Arc::new(ChSink::new(rivu_storage_ch::writer_client(
            &env.ch_url,
            &account("writer"),
            &ch_database(),
        ))),
        WriterOptions::new(BufferConfig {
            flush_interval: Duration::from_millis(100),
            ..BufferConfig::default()
        }),
    );
    let pipeline = Arc::new(Pipeline::new(
        sites,
        salts,
        writer.handle(),
        vec!["127.0.0.1/32".parse().unwrap()],
    ));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = rivu::public::router(Role::Collector, Some(pipeline));
    tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .unwrap();
    });

    let pageview = |key: &str, path: &str| {
        json!({"v": 1, "s": key, "n": "pageview", "u": format!("https://{HOST}{path}")}).to_string()
    };
    // 202 空体
    let r = http_post(addr, &pageview(&created.site_key, "/a")).await;
    assert_eq!(r.status, 202);
    assert_eq!(r.body, "");
    assert_eq!(r.header("x-rivu-dropped"), None);
    assert_eq!(r.header("access-control-allow-origin"), Some("*"));
    assert_eq!(r.header("cache-control"), Some("no-store"));
    assert_eq!(r.header("set-cookie"), None);
    // 被过滤：202 + x-rivu-dropped
    let r = http_post(addr, &pageview("zzzzzzzzzzzz", "/a")).await;
    assert_eq!((r.status, r.header("x-rivu-dropped")), (202, Some("1")));
    // 400
    let r = http_post(addr, "{}").await;
    assert_eq!(r.status, 400);
    assert!(r.body.contains("\"errors\""), "{}", r.body);

    writer.shutdown().await.expect("关停");
    let reader = reader_client(&env.ch_url);
    assert_eq!(count(&reader, "events", created.id).await, 1);
    let counters = counter_sums(&reader, created.id).await;
    assert_eq!(counters, BTreeMap::from([("accepted".to_owned(), 1)]));
    cleanup(&admin_client(&env.ch_url), created.id).await;
}

struct HttpResp {
    status: u16,
    headers: Vec<(String, String)>,
    body: String,
}

impl HttpResp {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

async fn http_post(addr: SocketAddr, body: &str) -> HttpResp {
    let mut stream = TcpStream::connect(addr).await.unwrap();
    let req = format!(
        "POST /api/event HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\
         Content-Type: text/plain;charset=UTF-8\r\nUser-Agent: e2e-http\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(req.as_bytes()).await.unwrap();
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).await.unwrap();
    let raw = String::from_utf8_lossy(&raw).into_owned();
    let (head, body) = raw.split_once("\r\n\r\n").unwrap_or((&raw, ""));
    let mut lines = head.lines();
    let status = lines
        .next()
        .unwrap()
        .split_whitespace()
        .nth(1)
        .unwrap()
        .parse()
        .unwrap();
    let headers = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_owned(), v.trim().to_owned()))
        .collect();
    HttpResp {
        status,
        headers,
        body: body.to_owned(),
    }
}
