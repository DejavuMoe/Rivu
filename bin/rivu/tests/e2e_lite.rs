//! 端到端：`Pipeline::process` -> SessionAssigner -> BatchWriter -> ClickHouse -> `rivu_query::execute`，
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
use jiff::civil::{Date, date};
use jiff::tz::TimeZone;
use rivu::migrate;
use rivu::script::{ScriptService, TrackerAssets};
use rivu_common::config::{BufferConfig, ChAccount, Cidr, Config, Env, Role, Secret};
use rivu_domain::{DropReason, EventKind};
use rivu_geo::GeoDb;
use rivu_ingest::pipeline::{Outcome, Pipeline, RequestCtx, SaltCache, SiteSnapshot};
use rivu_query::result::MetricValue;
use rivu_query::spec::{
    CustomRange, DateRange, Dimension as SpecDim, Direction, Filter, FilterLeaf, FilterOp,
    FilterTarget, Include, Metric, OrderBy, Pagination, RangeBound,
};
use rivu_query::{ExecOptions, QuerySpec, SiteContext, TenantScope};
use rivu_reference::{EventAttrs, Metrics, RawEvent, RefDim, metrics, sessionize};
use rivu_storage_ch::rows::{EventRow, IngestCounterRow};
use rivu_storage_pg::sites::NewSite;
use rivu_ua::UaParser;
use rivu_writer::{BatchSink, BatchWriter, ChSink, WriterOptions};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::OnceCell;

const HOST: &str = "example.test";
const EPS: f64 = 1e-9;
/// 看板档的查询超时（秒）。
const QUERY_TIMEOUT_SECS: u64 = 10;

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
            attrs: EventAttrs::default(),
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

// ---- 查询：用 QuerySpec 取核心指标与分组，与参考实现对比 ------------------------------------------

/// 核心指标（`system-design.md` §6.5）。
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Overview {
    visitors: u64,
    visits: u64,
    pageviews: u64,
    bounce_rate: f64,
    visit_duration: f64,
    engaged_duration: f64,
}

/// 与参考实现的 `RefDim` 一一对应的分组维度。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Dimension {
    Page,
    Source,
    Channel,
    Country,
    Device,
}

impl Dimension {
    const ALL: [Dimension; 5] = [
        Dimension::Page,
        Dimension::Source,
        Dimension::Channel,
        Dimension::Country,
        Dimension::Device,
    ];

    fn as_str(self) -> &'static str {
        match self {
            Dimension::Page => "page",
            Dimension::Source => "source",
            Dimension::Channel => "channel",
            Dimension::Country => "country",
            Dimension::Device => "device",
        }
    }

    fn spec(self) -> SpecDim {
        match self {
            Dimension::Page => SpecDim::EventPage,
            Dimension::Source => SpecDim::VisitSource,
            Dimension::Channel => SpecDim::VisitChannel,
            Dimension::Country => SpecDim::VisitCountry,
            Dimension::Device => SpecDim::VisitDevice,
        }
    }
}

fn spec(from: Date, to: Date, metrics: Vec<Metric>) -> QuerySpec {
    QuerySpec {
        site: None,
        metrics,
        date_range: DateRange::Custom(CustomRange {
            from: RangeBound::Date(from),
            to: RangeBound::Date(to),
        }),
        dimensions: Vec::new(),
        filters: Vec::new(),
        order_by: Vec::new(),
        include: Include::default(),
        pagination: Pagination::default(),
    }
}

fn int(v: MetricValue) -> u64 {
    match v {
        MetricValue::Int(n) => n,
        MetricValue::Float(f) => f as u64,
    }
}

async fn run(
    reader: &Client,
    site_id: u64,
    tz: &TimeZone,
    spec: QuerySpec,
) -> rivu_query::QueryResult {
    rivu_query::execute(
        reader,
        &TenantScope::new(site_id),
        &SiteContext::new(site_id, tz.clone()),
        spec,
        ExecOptions::new(QUERY_TIMEOUT_SECS),
    )
    .await
    .expect("查询")
}

/// 站点本地日期 `[from, to]` 的核心指标。
async fn overview(reader: &Client, site_id: u64, tz: &TimeZone, from: Date, to: Date) -> Overview {
    let metrics = vec![
        Metric::Visitors,
        Metric::Visits,
        Metric::Pageviews,
        Metric::BounceRate,
        Metric::VisitDuration,
        Metric::EngagedDuration,
    ];
    let result = run(reader, site_id, tz, spec(from, to, metrics)).await;
    let m = &result.results[0].metrics;
    Overview {
        visitors: int(m[0]),
        visits: int(m[1]),
        pageviews: int(m[2]),
        bounce_rate: m[3].as_f64(),
        visit_duration: m[4].as_f64(),
        engaged_duration: m[5].as_f64(),
    }
}

/// 按维度分组的 `(值, 访客, 会话, 浏览)`，按三者降序，最多 `limit` 行；页面维度只看 pageview。
async fn breakdown(
    reader: &Client,
    site_id: u64,
    tz: &TimeZone,
    (from, to): (Date, Date),
    dim: Dimension,
    limit: u32,
) -> Vec<Row4> {
    let mut s = spec(
        from,
        to,
        vec![Metric::Visitors, Metric::Visits, Metric::Pageviews],
    );
    s.dimensions = vec![dim.spec()];
    if dim == Dimension::Page {
        s.filters = vec![Filter::Leaf(FilterLeaf {
            op: FilterOp::Is,
            target: FilterTarget::Dimension(SpecDim::EventName),
            values: vec!["pageview".to_owned()],
            case_sensitive: None,
        })];
    }
    let desc = |k: &str| OrderBy {
        key: k.to_owned(),
        direction: Direction::Desc,
    };
    s.order_by = vec![desc("visitors"), desc("visits"), desc("pageviews")];
    s.pagination = Pagination { limit, offset: 0 };
    run(reader, site_id, tz, s)
        .await
        .results
        .iter()
        .map(|r| {
            (
                r.dimensions[0].clone(),
                int(r.metrics[0]),
                int(r.metrics[1]),
                int(r.metrics[2]),
            )
        })
        .collect()
}

fn assert_same_metrics(name: &str, q: &Overview, t: &Metrics) {
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
    // visitors：Q9 定案后（事件保留自己当天的 ID）所有范围都与真值严格相等。
    assert_eq!(q.visitors, t.visitors, "{name}: visitors");
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
             GROUP BY site_id, start_date, session_id)",
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
        ua_parser(),
        Arc::new(GeoDb::new()),
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
    // 3/29 05:00 又开一个新会话 —— Q9 的情形 (a)：3/29 当天两个会话的事件都带 3/29 的 ID，只算 1 个访客
    sim.pv(&p3, at(d28, 23, 50), "/");
    sim.pv(&p3, at(d29, 0, 10), "/docs");
    sim.eng(&p3, at(d29, 0, 12), "/docs", 5000, 30, true);
    sim.pv(&p3, at(d29, 5, 0), "/pricing");

    // P4：夏令时切换日。01:50 CET 与 03:10 CEST 只隔 20 分钟（中间跳过 02:00–03:00），属于同一个会话；
    // 23:40 又开一个会话并跨午夜到 3/30 00:05 —— Q9 的情形 (b)：跨天范围里这个人算两次（3/29、3/30 各一次）
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
    // visitors 也与参考实现的真值（各本地日不同人数之和）严格相等（Q9 定案）。
    let ranges: [(&str, Date, Date); 8] = [
        ("R1 3/28", d28, d28),
        ("R2 3/29（夏令时切换日，23 小时）", d29, d29),
        ("R3 3/30", d30, d30),
        ("R4 3/28-3/29（跨午夜两天）", d28, d29),
        ("R5 3/29-3/30", d29, d30),
        ("R6 3/28-3/30（整个范围）", d28, d30),
        ("R7 3/27（无数据）", d27, d27),
        ("R8 3/31（无数据）", d31, d31),
    ];
    let tz = berlin();
    let mut overviews = Vec::new();
    println!("范围 | 真值 visitors/visits/pageviews/bounce/duration/engaged | 查询值");
    for (name, from, to) in ranges {
        let truth = metrics(&raw, &ref_sessions, site_id, &tz, from, to);
        let q = overview(&reader, site_id, &tz, from, to).await;
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
        assert_same_metrics(name, &q, &truth);
        overviews.push(q);
    }
    // 空范围：全零
    for q in &overviews[6..] {
        assert_eq!(*q, Overview::default());
    }
    // 有数据的范围确实有数据（防止「全 0 对全 0」的假通过）
    assert!(overviews[5].visits == 15 && overviews[5].pageviews == 22);

    // 页面榜：3/28 浏览最多的页面
    let top = breakdown(&reader, site_id, &tz, (d28, d28), Dimension::Page, 3).await;
    assert_eq!(top.len(), 3);
    assert!(top.windows(2).all(|w| w[0].1 >= w[1].1));

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
    let replayed = overview(&reader, site_id, &tz, d28, d30).await;
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

    // 对照：换一个 token 就是新的一批，应该被计入（证明上面不是因为什么都没写进去）。\n    // token 带上 site_id：ClickHouse 的去重日志会记住 token，固定字符串在连续两次运行之间会被当成重复而判成假失败。
    let (_, first_rows) = &batches[0];
    sink.inner
        .insert_events(first_rows, &format!("e2e-control-token-{site_id}"))
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
        Arc::clone(&sites),
        salts,
        writer.handle(),
        vec!["127.0.0.1/32".parse().unwrap()],
        ua_parser(),
        Arc::new(GeoDb::new()),
    ));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let assets = Arc::new(TrackerAssets::from_parts(
        std::path::Path::new("/test"),
        b"/*core*/".to_vec(),
        b"/*full*/".to_vec(),
        3,
    ));
    let scripts = Arc::new(ScriptService::new(
        sites,
        assets,
        "https://stats.example.test",
    ));
    let app = rivu::public::router_with_scripts(Role::Collector, Some(pipeline), Some(scripts));
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
    // 没有 User-Agent 头 = 空 UA = 机器人：202 + x-rivu-dropped，计 dropped_bot
    let r = http_post_with_ua(addr, &pageview(&created.site_key, "/bot"), None).await;
    assert_eq!((r.status, r.header("x-rivu-dropped")), (202, Some("1")));
    // 站点脚本：前缀 + 产物，同一个 HTTP 服务。新站点的默认开关（engagement、tagged 开）选 full
    let r = http_get(addr, &format!("/js/{}.js", created.site_key)).await;
    assert_eq!(r.status, 200);
    assert!(
        r.body.starts_with("window.rivu=window.rivu||function(){"),
        "{}",
        r.body
    );
    assert!(
        r.body.contains(&format!(
            "rivu.c={{\"site\":\"{}\",\"endpoint\":\"https://stats.example.test/api/event\",\"hash\":false,\
             \"engagement\":true,\"outbound\":false,\"downloads\":false,\"forms\":false,\"tagged\":true}};",
            created.site_key
        )),
        "{}",
        r.body
    );
    assert!(r.body.ends_with("/*full*/"));
    assert_eq!(r.header("cache-control"), Some("public, max-age=300"));
    assert_eq!(http_get(addr, "/js/zzzzzzzzzzzz.js").await.status, 404);

    writer.shutdown().await.expect("关停");
    let reader = reader_client(&env.ch_url);
    assert_eq!(count(&reader, "events", created.id).await, 1);
    let counters = counter_sums(&reader, created.id).await;
    assert_eq!(
        counters,
        BTreeMap::from([("accepted".to_owned(), 1), ("dropped_bot".to_owned(), 1)])
    );
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

/// 一个真实浏览器的 UA（取自 uap-core 测试集里的同类字符串）。
const BROWSER_UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/118.0.5993.771 YaBrowser/23.11.2.771 Yowser/2.5 Safari/537.36";

async fn http_post(addr: SocketAddr, body: &str) -> HttpResp {
    http_post_with_ua(addr, body, Some(BROWSER_UA)).await
}

async fn http_get(addr: SocketAddr, path: &str) -> HttpResp {
    let req = format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n");
    http_raw(addr, req).await
}

async fn http_post_with_ua(addr: SocketAddr, body: &str, ua: Option<&str>) -> HttpResp {
    let ua = ua
        .map(|u| format!("User-Agent: {u}\r\n"))
        .unwrap_or_default();
    let req = format!(
        "POST /api/event HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\
         Content-Type: text/plain;charset=UTF-8\r\n{ua}Content-Length: {}\r\n\r\n{body}",
        body.len()
    );
    http_raw(addr, req).await
}

async fn http_raw(addr: SocketAddr, req: String) -> HttpResp {
    let mut stream = TcpStream::connect(addr).await.unwrap();
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

// ======================================================================================
// 富化：机器人、地理、UA、来源 -> 分组查询，与参考实现逐行比较
// ======================================================================================

/// UA 解析器构建一次要编译全部 uap-core 规则，测试间共享。
fn ua_parser() -> Arc<UaParser> {
    static PARSER: std::sync::OnceLock<Arc<UaParser>> = std::sync::OnceLock::new();
    Arc::clone(PARSER.get_or_init(|| Arc::new(UaParser::embedded().expect("uap-core 规则"))))
}

/// 以下 UA 逐字取自 uap-core 的测试集（`crates/ua/tests/data/uap-test_ua.yaml`），
/// 测试 `real_browser_uas_come_from_the_uap_core_test_set` 校验它们确实在那份文件里。
const UA_WIN_YANDEX: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/118.0.5993.771 YaBrowser/23.11.2.771 Yowser/2.5 Safari/537.36";
const UA_MAC_SAFARI: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/16.5 Safari/605.1.15 Ddg/17.2";
const UA_IPHONE: &str = "Mozilla/5.0 (iPhone; CPU iPhone OS 17_2_1 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.2 Mobile/15E148 Safari/604.1 Ddg/17.2";
const UA_IPAD: &str = "Mozilla/5.0 (iPad; CPU OS 12_5_5 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/12.0 EdgiOS/46.3.26 Mobile/15E148 Safari/605.1.15";
const UA_ANDROID: &str = "Mozilla/5.0 (Linux; Android 12; Pixel 6) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/99.0.4844.84 Mobile Safari/537.36 Brave/1.40.128";
const UA_GOOGLEBOT: &str =
    "Mozilla/5.0 (compatible; Googlebot/2.1; +http://www.google.com/bot.html)";

#[test]
fn real_browser_uas_come_from_the_uap_core_test_set() {
    let yaml = include_str!("../../../crates/ua/tests/data/uap-test_ua.yaml");
    for ua in [
        UA_WIN_YANDEX,
        UA_MAC_SAFARI,
        UA_IPHONE,
        UA_IPAD,
        UA_ANDROID,
        UA_GOOGLEBOT,
    ] {
        assert!(
            yaml.contains(&format!("user_agent_string: '{ua}'")),
            "不在 uap-core 测试集里：{ua}"
        );
    }
}

/// MaxMind 官方测试库（`GeoIP2-City-Test.mmdb`）里有记录的地址，期望值来自它的源数据 JSON
/// （`crates/geo/tests/data/source-data/GeoIP2-City-Test.json`，见 `docs/research/enrich-geo.md` §4）。
const IP_SE: &str = "89.160.20.128"; // SE，SE-E，city 2694762
const IP_GB: &str = "81.2.69.142"; // GB
const IP_CN: &str = "175.16.199.1"; // CN
const IP_JP: &str = "2001:218::1"; // JP（IPv6）
const IP_NO_GEO: &str = "203.0.113.50"; // 文档地址，不是公网：没有位置

fn person_at(id: u64, ip: &str, ua: &str) -> Person {
    Person {
        id,
        ip: ip.into(),
        ua: ua.into(),
    }
}

/// 一个事件按**自己的请求**会得到的属性（手写的真值，规则来自 `docs/research/enrich-source.md` 与 `enrich-ua.md`）。
fn attrs(source: &str, channel: &str, country: &str, device: &str) -> EventAttrs {
    EventAttrs {
        referrer_source: source.into(),
        channel: channel.into(),
        country: country.into(),
        device: device.into(),
    }
}

impl Sim {
    /// 带额外字段（`r`、带 UTM 的 `u` 等）的 pageview；`attrs` 是该事件自己的属性真值。
    fn pv_ext(&mut self, p: &Person, ts: i64, path: &str, extra: Value, own: EventAttrs) {
        let mut body =
            json!({"v": 1, "sv": 1, "s": self.site_key, "n": "pageview", "u": Self::url(path)});
        for (k, v) in extra.as_object().expect("extra 是对象") {
            body[k] = v.clone();
        }
        self.push_raw(p, ts, EventKind::Pageview, path, true, 0);
        self.raw.last_mut().expect("刚推入").attrs = own;
        self.site_calls += 1;
        assert_eq!(
            self.call(p, ts, &body),
            Outcome::Accepted,
            "pageview {path}"
        );
    }

    /// 会被丢弃的事件（机器人）：只经过管线，不进参考实现的输入。
    fn expect_dropped(&mut self, p: &Person, ts: i64, body: Value, reason: DropReason) {
        self.site_calls += 1;
        assert_eq!(self.call(p, ts, &body), Outcome::Dropped(reason));
    }
}

fn ref_dim(d: Dimension) -> RefDim {
    match d {
        Dimension::Page => RefDim::Page,
        Dimension::Source => RefDim::Source,
        Dimension::Channel => RefDim::Channel,
        Dimension::Country => RefDim::Country,
        Dimension::Device => RefDim::Device,
    }
}

type Row4 = (String, u64, u64, u64);

#[allow(clippy::too_many_arguments)]
async fn assert_breakdowns_match(
    label: &str,
    reader: &Client,
    site_id: u64,
    tz: &TimeZone,
    raw: &[RawEvent],
    sessions: &[rivu_reference::RefSession],
    from: Date,
    to: Date,
) -> BTreeMap<&'static str, Vec<Row4>> {
    let mut shown = BTreeMap::new();
    for dim in Dimension::ALL {
        for limit in [100u32, 2] {
            let q = breakdown(reader, site_id, tz, (from, to), dim, limit).await;
            let t: Vec<Row4> = rivu_reference::breakdown(
                raw,
                sessions,
                site_id,
                tz,
                from,
                to,
                ref_dim(dim),
                limit as usize,
            )
            .into_iter()
            .map(|r| (r.value, r.visitors, r.visits, r.pageviews))
            .collect();
            assert_eq!(q, t, "{label} / {} / limit {limit}", dim.as_str());
            if limit == 100 {
                shown.insert(dim.as_str(), q);
            }
        }
    }
    shown
}

#[tokio::test]
async fn enriched_pipeline_breakdowns_match_the_reference() {
    let Some(env) = test_env("enriched_pipeline_breakdowns_match_the_reference") else {
        return;
    };
    migrate_once(&env).await;

    // ---- 站点、盐、富化器 ------------------------------------------------------------------
    let pg = rivu_storage_pg::connect(&env.pg_url, 4)
        .await
        .expect("连接 PG");
    let hostnames = vec![HOST.to_owned()];
    let created = rivu_storage_pg::sites::create_site(
        &pg,
        &NewSite {
            name: "e2e-enriched",
            timezone: "Europe/Berlin",
            hostnames: &hostnames,
        },
    )
    .await
    .expect("建站");
    let (site_id, site_key) = (created.id, created.site_key.clone());
    let (d27, d28, d29, d30) = (
        date(2026, 3, 27),
        date(2026, 3, 28),
        date(2026, 3, 29),
        date(2026, 3, 30),
    );
    rivu_storage_pg::salts::ensure_salts(&pg, &days(date(2026, 3, 26), date(2026, 4, 1)))
        .await
        .expect("ensure_salts");
    let sites = Arc::new(SiteSnapshot::new());
    sites.reload(&pg).await.expect("加载站点快照");
    let salts = Arc::new(SaltCache::new());
    salts
        .reload(&pg, date(2026, 3, 20), date(2026, 4, 10))
        .await
        .expect("加载盐");

    // 真实的 GeoDb：MaxMind 官方测试库，手工放置（没有 meta），署名来自配置
    let geo = Arc::new(GeoDb::with_manual_attribution(
        Some("MaxMind test database"),
        Some("https://github.com/maxmind/MaxMind-DB"),
    ));
    let mmdb = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../crates/geo/tests/data/test-data/GeoIP2-City-Test.mmdb");
    assert!(geo.reload_if_changed(&mmdb).expect("加载测试库"));

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
        max_rows: 10,
        max_bytes: 8 << 20,
        channel_capacity: 10_000,
    });
    opts.counter_flush_interval = Duration::from_secs(10);
    let writer = BatchWriter::spawn(Arc::clone(&sink), opts);
    let pipeline = Arc::new(Pipeline::new(
        Arc::clone(&sites),
        Arc::clone(&salts),
        writer.handle(),
        vec!["127.0.0.1/32".parse().unwrap()],
        ua_parser(),
        Arc::clone(&geo),
    ));
    let mut sim = Sim {
        pipeline,
        site_key: site_key.clone(),
        site_id,
        raw: Vec::new(),
        tally: Tally::default(),
        site_calls: 0,
    };

    // ---- 场景（柏林本地时间）：每个事件同时写下它自己的属性真值 ------------------------------------
    let p1 = person_at(1, IP_SE, UA_WIN_YANDEX);
    let p2 = person_at(2, IP_GB, UA_IPHONE);
    let p3 = person_at(3, IP_CN, UA_MAC_SAFARI);
    let p4 = person_at(4, IP_JP, UA_IPAD);
    let p5 = person_at(5, IP_NO_GEO, UA_ANDROID);
    let p6 = person_at(6, IP_SE, UA_MAC_SAFARI);
    let bot = person_at(7, IP_SE, UA_GOOGLEBOT);

    // P1（SE，桌面）：3/28 来自 Google 搜索，会话里第二页的 referrer 是 Twitter（SPA 不会带，这里故意带上，
    // 它必须被忽略：会话属性由首事件决定）；晚上带 utm 再来一次（新会话，来源取 utm_source，渠道 Email）
    let google = attrs("Google", "Organic Search", "SE", "desktop");
    sim.pv_ext(
        &p1,
        at(d28, 12, 0),
        "/",
        json!({"r": "https://www.google.com/search?q=rivu"}),
        google.clone(),
    );
    sim.pv_ext(
        &p1,
        at(d28, 12, 3),
        "/pricing",
        json!({"r": "https://twitter.com/rivu/status/1"}),
        attrs("X (Twitter)", "Organic Social", "SE", "desktop"),
    );
    sim.eng(&p1, at(d28, 12, 4), "/pricing", 6000, 70, true);
    sim.pv_ext(
        &p1,
        at(d28, 20, 0),
        "/pricing",
        json!({"u": "https://example.test/pricing?utm_source=newsletter&utm_medium=email&utm_campaign=spring"}),
        attrs("newsletter", "Email", "SE", "desktop"),
    );

    // P2（GB，手机）：3/29 来自 t.co（X）
    sim.pv_ext(
        &p2,
        at(d29, 9, 0),
        "/",
        json!({"r": "https://t.co/abc123"}),
        attrs("X (Twitter)", "Organic Social", "GB", "mobile"),
    );
    sim.pv_ext(
        &p2,
        at(d29, 9, 2),
        "/docs",
        json!({}),
        attrs("", "Direct", "GB", "mobile"),
    );

    // P3（CN，Mac）：3/28 23:50 直接访问，跨午夜到 3/29 00:10（同一会话，第二页的 referrer 是 Google，被忽略）；
    // 3/30 又来一次，只有 gclid（referrer 被隐私设置抹掉）-> 推断 Google / Paid Search
    let direct_cn = attrs("", "Direct", "CN", "desktop");
    sim.pv_ext(&p3, at(d28, 23, 50), "/blog", json!({}), direct_cn.clone());
    sim.pv_ext(
        &p3,
        at(d29, 0, 10),
        "/docs",
        json!({"r": "https://www.google.com/"}),
        attrs("Google", "Organic Search", "CN", "desktop"),
    );
    sim.pv_ext(
        &p3,
        at(d30, 10, 0),
        "/blog",
        json!({"u": "https://example.test/blog?gclid=abc"}),
        attrs("Google", "Paid Search", "CN", "desktop"),
    );

    // P4（JP，IPv6，平板）：3/29 来自本站自己的页面（内部引荐，算 Direct）；3/30 带 bing 的付费标签，referrer 是 Google
    // （utm_source 覆盖 referrer）；3/30 稍后（新会话）来自 Bing 自然搜索
    sim.pv_ext(
        &p4,
        at(d29, 15, 0),
        "/",
        json!({"r": "https://www.example.test/other"}),
        attrs("", "Direct", "JP", "tablet"),
    );
    sim.pv_ext(
        &p4,
        at(d30, 9, 0),
        "/pricing",
        json!({"u": "https://example.test/pricing?utm_source=bing&utm_medium=cpc",
               "r": "https://www.google.com/"}),
        attrs("Bing", "Paid Search", "JP", "tablet"),
    );
    sim.pv_ext(
        &p4,
        at(d30, 11, 0),
        "/",
        json!({"r": "https://www.bing.com/search?q=rivu"}),
        attrs("Bing", "Organic Search", "JP", "tablet"),
    );

    // P5（没有位置，Android）：认不出的 referrer 域名 -> 来源是 host，渠道 Referral
    sim.pv_ext(
        &p5,
        at(d29, 12, 0),
        "/",
        json!({"r": "https://blog.example.org/post/1"}),
        attrs("blog.example.org", "Referral", "", "mobile"),
    );

    // P6（SE，Mac）：同一天两次访问，间隔超过 30 分钟（两个会话），都是直接访问 —— 访客数只算一次
    sim.pv_ext(
        &p6,
        at(d29, 8, 0),
        "/",
        json!({}),
        attrs("", "Direct", "SE", "desktop"),
    );
    sim.pv_ext(
        &p6,
        at(d29, 9, 0),
        "/blog",
        json!({}),
        attrs("", "Direct", "SE", "desktop"),
    );

    // 机器人：pageview 被丢弃（dropped_bot），它的 engagement 没有会话（dropped_no_session）
    sim.expect_dropped(
        &bot,
        at(d29, 13, 0),
        json!({"v": 1, "sv": 1, "s": site_key, "n": "pageview", "u": Sim::url("/")}),
        DropReason::Bot,
    );
    sim.expect_dropped(
        &bot,
        at(d29, 13, 0) + 1000,
        json!({"v": 1, "sv": 1, "s": site_key, "n": "engagement", "u": Sim::url("/"), "sd": 10, "e": 3000}),
        DropReason::NoSession,
    );

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
    assert_eq!(tally.dropped_count(DropReason::Bot), 1);
    assert_eq!(tally.dropped_count(DropReason::NoSession), 1);
    assert_eq!(tally.overloaded, 0);

    let ref_sessions = sessionize(&raw);
    let absorbed: usize = ref_sessions.iter().map(|s| s.event_indices.len()).sum();
    assert_eq!(
        absorbed as u64, tally.accepted,
        "参考实现并入的事件数 = 接受数"
    );
    assert_eq!(
        raw.len() as u64,
        tally.accepted,
        "没有被丢弃的合法事件混进参考输入"
    );
    assert_eq!(ref_sessions.len(), 11, "场景里一共 11 个会话");

    // ---- 对账 ----------------------------------------------------------------------------------
    let reader = reader_client(&env.ch_url);
    assert_eq!(count(&reader, "events", site_id).await, tally.accepted);
    assert_eq!(
        merged_sessions(&reader, site_id).await,
        ref_sessions.len() as u64
    );
    let counters = counter_sums(&reader, site_id).await;
    println!("ingest_counters：{counters:?}");
    assert_eq!(
        counters,
        BTreeMap::from([
            ("accepted".to_owned(), tally.accepted),
            ("dropped_bot".to_owned(), 1),
            ("dropped_no_session".to_owned(), 1),
        ])
    );
    assert_eq!(
        counters.values().sum::<u64>(),
        site_calls,
        "ingest_counters 总和 = 本站点被处理的请求数"
    );

    // 位置字段按站点的 geo_level（默认 city）存：SE 的事件带区域与城市（源数据：SE-E、2694762）
    #[derive(clickhouse::Row, serde::Deserialize)]
    struct GeoRow {
        region_code: String,
        city_geoname_id: u32,
    }
    let se: Vec<GeoRow> = reader
        .query(
            "SELECT DISTINCT region_code, city_geoname_id FROM events \
             WHERE site_id = {site:UInt64} AND country_code = 'SE'",
        )
        .param("site", site_id)
        .fetch_all()
        .await
        .expect("SE 行");
    assert_eq!(se.len(), 1);
    assert_eq!(
        (se[0].region_code.as_str(), se[0].city_geoname_id),
        ("SE-E", 2_694_762)
    );

    // ---- 每个维度与参考实现逐行相等 ---------------------------------------------------------------
    let tz = berlin();
    for (label, from, to) in [
        ("3/28-3/30", d28, d30),
        ("3/29（夏令时切换日）", d29, d29),
        ("3/28", d28, d28),
        ("3/30", d30, d30),
        ("3/27（无数据）", d27, d27),
    ] {
        let shown =
            assert_breakdowns_match(label, &reader, site_id, &tz, &raw, &ref_sessions, from, to)
                .await;
        if label == "3/28-3/30" {
            for (dim, rows) in &shown {
                println!("[{label}] {dim}：value | visitors/visits/pageviews（查询值 = 参考值）");
                for (v, a, b, c) in rows {
                    println!("    {v:<22} {a}/{b}/{c}");
                }
            }
            // 防止「空对空」的假通过：具体行数与几个手算值
            assert_eq!(shown["country"].len(), 5, "SE GB CN JP (not set)");
            assert_eq!(shown["device"].len(), 3, "desktop mobile tablet");
            let by = |dim: &str, v: &str| shown[dim].iter().find(|r| r.0 == v).cloned();
            assert_eq!(by("country", "SE"), Some(("SE".into(), 2, 4, 5)));
            assert_eq!(
                by("source", "Direct / None"),
                Some(("Direct / None".into(), 4, 4, 5))
            );
            assert_eq!(
                by("channel", "Paid Search"),
                Some(("Paid Search".into(), 2, 2, 2))
            );
        } else if label.contains("无数据") {
            assert!(shown.values().all(Vec::is_empty), "{label}: {shown:?}");
        }
    }

    // ---- 署名与能力：含国家的结果要附带 -------------------------------------------------------------
    let attribution = geo.attribution().expect("手工署名");
    assert_eq!(attribution.text, "MaxMind test database");
    assert_eq!(geo.capability(), Some(rivu_geo::GeoCapability::City));

    cleanup(&admin_client(&env.ch_url), site_id).await;
}
