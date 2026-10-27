//! 差分测试：统一查询内核（`rivu_query::execute`，真实的 ClickHouse）对照独立的参考实现
//! （`rivu_reference::query`，只看原始事件）。
//!
//! 事件由确定性的随机生成器产生（多人、跨本地午夜与夏令时切换日、多种 UA / IP / referrer / utm / 页面、
//! engagement 带 sd 或 e、自定义事件），经真实管线（`Pipeline` + 可控时钟）写入 ClickHouse；
//! 再随机生成数百个合法的 `QuerySpec`（指标子集、0–2 个维度、随机过滤树、随机日期范围与「现在」、order_by、
//! limit / offset、`include`、对比期；目标：事件与页面目标、通配、`event:goal` 维度与过滤、`conversion_rate`），
//! 逐个比较两边的结果：计数严格相等，比率 1e-9 容差，
//! 行序、`time_labels`、`total_rows`、对比期都要一致。失败时把请求最小化后打印。
//!
//! 需要真实的 PostgreSQL 和 ClickHouse，缺变量时跳过（同 `e2e_lite.rs`）：
//!   RIVU_TEST_DATABASE_URL、RIVU_TEST_CLICKHOUSE_URL，可选 RIVU_TEST_CH_{MIGRATOR,WRITER,READER}_{USER,PASSWORD}、
//!   RIVU_TEST_CH_DATABASE。
//! 另外：
//!   RIVU_DIFF_SEED    随机种子（默认固定）
//!   RIVU_DIFF_CASES   随机请求的个数（默认 500）
//!
//! 参考实现的输入：事件的人、时间、名字、engagement 值是生成器自己的真值；`hostname` 与 `pathname` 用生成值
//! （并断言与入库值一致）；会话属性（来源、渠道、UTM、位置、设备）取入库后**会话首事件**自己的值——
//! 这样测试验证的是查询语义、会话切分与盖章，而不是富化规则（富化有各自的测试）。

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use clickhouse::Client;
use jiff::Timestamp;
use jiff::civil::{Date, Weekday, date};
use jiff::tz::TimeZone;
use rand::rngs::StdRng;
use rand::seq::IndexedRandom;
use rand::{Rng, SeedableRng};
use rivu::migrate;
use rivu_common::config::{BufferConfig, ChAccount, Config, Env, Role, Secret};
use rivu_geo::GeoDb;
use rivu_ingest::pipeline::{Outcome, Pipeline, RequestCtx, SaltCache, SiteSnapshot};
use rivu_query::resolve::{GoalDef, GoalKind};
use rivu_query::result::{MetricValue, QueryResult};
use rivu_query::{ExecOptions, QueryError, QuerySpec, SiteContext, TenantScope, execute};
use rivu_reference::query::{
    RefAttrs, RefEvent, RefGoal, RefGoalKind, RefResult, RefSite, is_count_metric, owned,
    run as ref_run,
};
use rivu_storage_pg::sites::NewSite;
use rivu_ua::UaParser;
use rivu_writer::{BatchWriter, ChSink, WriterOptions};
use serde_json::{Value, json};
use tokio::sync::OnceCell;

const SEED_DEFAULT: u64 = 0x5EED_2026_1023;
const CASES_DEFAULT: usize = 500;
const TIMEOUT_SECS: u64 = 15;

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

async fn migrate_once(pg_url: &str, ch_url: &str) {
    static DONE: OnceCell<()> = OnceCell::const_new();
    DONE.get_or_init(|| async {
        let migrator = account("migrator");
        let cfg_env = Env::from_pairs([
            ("RIVU_DATABASE_URL", pg_url),
            ("RIVU_CLICKHOUSE_URL", ch_url),
            ("RIVU_CH_DATABASE", ch_database().as_str()),
            ("RIVU_CH_MIGRATOR_USER", migrator.user.as_str()),
            ("RIVU_CH_MIGRATOR_PASSWORD", migrator.password.expose()),
        ]);
        let config = Config::from_env(Role::Migrate, &cfg_env).expect("测试配置");
        migrate::apply(&config, false).await.expect("migrate");
    })
    .await;
}

// ======================================================================================
// 场景数据
// ======================================================================================

/// 以下 UA 与 IP 同 `e2e_lite.rs`（取自 uap-core 与 MaxMind 官方测试库）；再加两个常见的桌面 UA。
const UAS: [&str; 7] = [
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/118.0.5993.771 YaBrowser/23.11.2.771 Yowser/2.5 Safari/537.36",
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/16.5 Safari/605.1.15 Ddg/17.2",
    "Mozilla/5.0 (iPhone; CPU iPhone OS 17_2_1 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.2 Mobile/15E148 Safari/604.1 Ddg/17.2",
    "Mozilla/5.0 (iPad; CPU OS 12_5_5 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/12.0 EdgiOS/46.3.26 Mobile/15E148 Safari/605.1.15",
    "Mozilla/5.0 (Linux; Android 12; Pixel 6) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/99.0.4844.84 Mobile Safari/537.36 Brave/1.40.128",
    "Mozilla/5.0 (X11; Linux x86_64; rv:126.0) Gecko/20100101 Firefox/126.0",
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36",
];
const IPS: [&str; 5] = [
    "89.160.20.128", // SE，有区域与城市
    "81.2.69.142",   // GB
    "175.16.199.1",  // CN
    "2001:218::1",   // JP（IPv6）
    "203.0.113.50",  // 文档地址：没有位置
];
const PAGES: [&str; 9] = [
    "/",
    "/pricing",
    "/docs",
    "/docs/intro",
    "/blog/post-1",
    "/blog/post-2",
    "/contact",
    "/about/team",
    "/docs/api",
];
const CUSTOM_NAMES: [&str; 3] = ["Signup", "Download", "Outbound Link: Click"];
const REFERRERS: [Option<&str>; 8] = [
    None,
    None,
    None,
    Some("https://www.google.com/search?q=rivu"),
    Some("https://t.co/abc123"),
    Some("https://www.bing.com/search?q=rivu"),
    Some("https://blog.example.org/post/1"),
    Some("https://www.example.test/internal"),
];
const UTMS: [Option<&str>; 6] = [
    None,
    None,
    None,
    Some(
        "?utm_source=newsletter&utm_medium=email&utm_campaign=spring&utm_content=hero&utm_term=analytics",
    ),
    Some("?utm_source=bing&utm_medium=cpc&utm_campaign=brand"),
    Some("?gclid=abc123"),
];

/// 两个站点共用的目标：`(id, kind, 值, 显示名)`。事件目标用生成器的自定义事件名；页面目标覆盖精确路径、
/// 前缀通配、中间通配、只有 `*`（匹配所有 pageview），以及一个永远完成不了的目标（不出现在结果里）。
const GOALS: [(i64, &str, &str, Option<&str>); 8] = [
    (101, "event", "Signup", None),
    (102, "event", "Download", Some("Got the PDF")),
    (103, "event", "never-sent", Some("Never")),
    (104, "page", "/pricing", None),
    (105, "page", "/docs*", Some("Docs")),
    (106, "page", "/*/intro", None),
    (107, "page", "/blog/*", Some("Blog")),
    (108, "page", "/*", Some("Any page")),
];

fn goal_defs() -> Vec<GoalDef> {
    GOALS
        .iter()
        .map(|(id, kind, v, d)| {
            let (e, p) = if *kind == "event" {
                (Some(*v), None)
            } else {
                (None, Some(*v))
            };
            GoalDef::from_parts(*id, kind, e, p, *d).expect("合法的目标")
        })
        .collect()
}

fn ref_goals() -> Vec<RefGoal> {
    goal_defs()
        .into_iter()
        .map(|g| RefGoal {
            id: g.id,
            label: g.label,
            kind: match g.kind {
                GoalKind::Event(n) => RefGoalKind::Event(n),
                GoalKind::Page(p) => RefGoalKind::Page(p),
            },
        })
        .collect()
}

struct SiteDef {
    id: u64,
    key: String,
    tz_name: &'static str,
    tz: TimeZone,
    week_start: Weekday,
    hostnames: &'static [&'static str],
}

impl SiteDef {
    fn at(&self, d: Date, h: i8, m: i8, s: i8, ms: i32) -> i64 {
        d.at(h, m, s, ms * 1_000_000)
            .to_zoned(self.tz.clone())
            .expect("本地时间有效")
            .timestamp()
            .as_millisecond()
    }
}

/// 「现在」的候选：落在数据的不同位置，包括夏令时切换之后、全部数据之后、部分数据之前（封顶语义）。
fn now_pool(berlin: &TimeZone) -> Vec<i64> {
    let at = |d: Date, h: i8, m: i8| {
        d.at(h, m, 0, 0)
            .to_zoned(berlin.clone())
            .unwrap()
            .timestamp()
            .as_millisecond()
    };
    vec![
        at(date(2026, 3, 29), 12, 0),
        at(date(2026, 4, 3), 15, 0),
        at(date(2026, 10, 25), 12, 0),
        at(date(2026, 11, 2), 9, 0),
    ]
}

#[derive(Clone, Debug)]
struct GenEvent {
    person: usize,
    ts_ms: i64,
    name: String,
    host: String,
    path: String,
    interactive: bool,
    e: Option<u32>,
    sd: Option<u8>,
    referrer: Option<&'static str>,
    utm: Option<&'static str>,
}

fn pick<'a, T>(rng: &mut StdRng, xs: &'a [T]) -> &'a T {
    xs.choose(rng).expect("非空")
}

/// 生成一个站点的事件：窗口 A 覆盖三月底的夏令时切换日，窗口 B 覆盖十月底的回拨日，
/// 另有零星的早期事件与贴近各个「现在」的访问（给 `realtime` 与 `24h` 用）。
fn generate(rng: &mut StdRng, site: &SiteDef, persons: usize, nows: &[i64]) -> Vec<GenEvent> {
    let windows: [(Date, i64); 2] = [(date(2026, 3, 26), 9), (date(2026, 10, 22), 7)];
    let mut out: Vec<GenEvent> = Vec::new();
    for person in 0..persons {
        let visits = rng.random_range(3..9);
        for _ in 0..visits {
            let (d0, span) = if rng.random_bool(0.04) {
                // 早期的零星访问：给 week / month / year / all 一点跨度
                (date(2025, 12, 28), 40)
            } else {
                *pick(rng, &windows)
            };
            let day = d0
                .checked_add(jiff::Span::new().days(rng.random_range(0..span)))
                .unwrap();
            let hour = match rng.random_range(0..100) {
                0..=39 => rng.random_range(0..24),
                40..=64 => rng.random_range(22..24),
                65..=84 => rng.random_range(0..5),
                _ => rng.random_range(8..20),
            };
            let start = site.at(
                day,
                hour,
                rng.random_range(0..60),
                rng.random_range(0..60),
                rng.random_range(0..1000),
            );
            visit(rng, site, person, start, &mut out);
        }
    }
    // 贴近各个「现在」的访问：最后 5 分钟内、最后 24 小时内各来几个
    for &now in nows {
        for _ in 0..6 {
            let person = rng.random_range(0..persons);
            let back = if rng.random_bool(0.5) {
                rng.random_range(0..240_000)
            } else {
                rng.random_range(0..20 * 3_600_000)
            };
            visit(rng, site, person, now - back - 90_000, &mut out);
        }
    }
    // 全局按时间排序（管线按到达顺序处理），同站点内毫秒唯一（入库行靠 ts 对回事件）
    out.sort_by_key(|e| e.ts_ms);
    let mut last = i64::MIN;
    for e in &mut out {
        if e.ts_ms <= last {
            e.ts_ms = last + 1;
        }
        last = e.ts_ms;
    }
    out
}

fn visit(rng: &mut StdRng, site: &SiteDef, person: usize, start: i64, out: &mut Vec<GenEvent>) {
    let n = rng.random_range(1..8);
    let mut t = start;
    let mut cur_page: &str = pick(rng, &PAGES);
    let mut cur_host: &str = if rng.random_bool(0.8) {
        site.hostnames[0]
    } else {
        pick(rng, site.hostnames)
    };
    for k in 0..n {
        let r = rng.random_range(0..100);
        let ev = if k == 0 || r < 55 {
            cur_page = pick(rng, &PAGES);
            if rng.random_bool(0.1) {
                cur_host = pick(rng, site.hostnames);
            }
            ("pageview".to_owned(), true, None, None)
        } else if r < 78 {
            // engagement：sd、e、或两者
            let (e, sd) = match rng.random_range(0..3) {
                0 => (None, Some(rng.random_range(0..=100u8))),
                1 => (Some(rng.random_range(0..90_000u32)), None),
                _ => (
                    Some(rng.random_range(0..120_000u32)),
                    Some(rng.random_range(0..=100u8)),
                ),
            };
            ("engagement".to_owned(), true, e, sd)
        } else {
            (
                pick(rng, &CUSTOM_NAMES).to_string(),
                rng.random_bool(0.75),
                None,
                None,
            )
        };
        let (name, interactive, e, sd) = ev;
        let first = k == 0;
        out.push(GenEvent {
            person,
            ts_ms: t,
            name,
            host: cur_host.to_owned(),
            path: cur_page.to_owned(),
            interactive,
            e,
            sd,
            // 来源与 UTM 只在新会话的首事件有意义，但每个事件都带一个随机的（后来的会被忽略，这正是要验证的）
            referrer: *pick(rng, &REFERRERS),
            utm: if first || rng.random_bool(0.3) {
                *pick(rng, &UTMS)
            } else {
                None
            },
        });
        t += match rng.random_range(0..100) {
            0..=54 => rng.random_range(5_000..480_000),
            55..=74 => rng.random_range(480_000..1_500_000),
            75..=86 => rng.random_range(1_740_000..1_860_000), // 29–31 分钟：续期边界
            87..=94 => rng.random_range(1_860_000..7_200_000),
            _ => rng.random_range(60_000..14_400_000),
        };
    }
}

fn body_for(site: &SiteDef, e: &GenEvent) -> Value {
    let url = format!(
        "https://{}{}{}",
        e.host,
        e.path,
        if e.name == "engagement" {
            ""
        } else {
            e.utm.unwrap_or("")
        }
    );
    let mut body = json!({"v": 1, "sv": 1, "s": site.key, "n": e.name, "u": url});
    if e.name != "engagement" {
        if let Some(r) = e.referrer {
            body["r"] = json!(r);
        }
        if !e.interactive {
            body["i"] = json!(false);
        }
    } else {
        if let Some(sd) = e.sd {
            body["sd"] = json!(sd);
        }
        if let Some(ms) = e.e {
            body["e"] = json!(ms);
        }
    }
    body
}

#[derive(clickhouse::Row, serde::Deserialize)]
struct Stored {
    ts_ms: i64,
    hostname: String,
    pathname: String,
    referrer: String,
    referrer_source: String,
    channel: String,
    utm_source: String,
    utm_medium: String,
    utm_campaign: String,
    utm_content: String,
    utm_term: String,
    country: String,
    region_code: String,
    city_geoname_id: u32,
    device: String,
    browser: String,
    browser_version: String,
    os: String,
    os_version: String,
}

async fn read_back(reader: &Client, site_id: u64) -> BTreeMap<i64, Stored> {
    reader
        .query(
            "SELECT toUnixTimestamp64Milli(ts) AS ts_ms, hostname, pathname, referrer, referrer_source, channel, \
             utm_source, utm_medium, utm_campaign, utm_content, utm_term, toString(country_code) AS country, \
             region_code, city_geoname_id, device, browser, browser_version, os, os_version \
             FROM events WHERE site_id = {site:UInt64}",
        )
        .param("site", site_id)
        .fetch_all::<Stored>()
        .await
        .expect("读回 events")
        .into_iter()
        .map(|r| (r.ts_ms, r))
        .collect()
}

// ======================================================================================
// 请求生成
// ======================================================================================

const METRICS: [&str; 13] = [
    "conversion_rate",
    "visitors",
    "visits",
    "pageviews",
    "events",
    "views_per_visit",
    "bounce_rate",
    "visit_duration",
    "engaged_duration",
    "percentage",
    "exit_rate",
    "time_on_page",
    "scroll_depth",
];
const EVENT_DIMS: [&str; 4] = ["event:name", "event:page", "event:hostname", "event:goal"];
/// 能与 `event:goal` 维度同用的指标。
const GOAL_DIM_METRICS: [&str; 5] = [
    "visitors",
    "visits",
    "pageviews",
    "events",
    "conversion_rate",
];
const VISIT_DIMS: [&str; 18] = [
    "visit:entry_page",
    "visit:exit_page",
    "visit:source",
    "visit:channel",
    "visit:referrer",
    "visit:utm_source",
    "visit:utm_medium",
    "visit:utm_campaign",
    "visit:utm_content",
    "visit:utm_term",
    "visit:country",
    "visit:region",
    "visit:city",
    "visit:device",
    "visit:browser",
    "visit:browser_version",
    "visit:os",
    "visit:os_version",
];
const TIME_DIMS: [&str; 6] = [
    "time",
    "time:minute",
    "time:hour",
    "time:day",
    "time:week",
    "time:month",
];
const OPS: [&str; 8] = [
    "is",
    "is_not",
    "contains",
    "contains_not",
    "matches",
    "matches_not",
    "matches_wildcard",
    "matches_wildcard_not",
];
const REGEXES: [&str; 12] = [
    "^/docs",
    "ing$",
    "^/$",
    "pr.*g",
    "blog/post-[12]",
    "(docs|blog)",
    "Chrome|Safari",
    "^$",
    "^(Direct|Google)",
    "e",
    "o+",
    "^[A-Z]{2}$",
];
const WILDCARDS: [&str; 10] = [
    "/docs*", "*s*", "/blog/*", "*", "/", "*post*", "example*", "*.test", "D*", "*e",
];

struct Pools {
    by_dim: BTreeMap<&'static str, Vec<String>>,
}

impl Pools {
    fn new(events: &[RefEvent]) -> Self {
        let mut sets: BTreeMap<&'static str, BTreeSet<String>> = BTreeMap::new();
        for e in events {
            let a = &e.attrs;
            let mut add = |d: &'static str, v: &str| {
                if !v.is_empty() {
                    sets.entry(d).or_default().insert(v.to_owned());
                }
            };
            add("event:name", &e.name);
            add("event:page", &e.pathname);
            add("event:hostname", &e.hostname);
            add("visit:entry_page", &e.pathname);
            add("visit:exit_page", &e.pathname);
            add("visit:source", &a.referrer_source);
            add("visit:channel", &a.channel);
            add("visit:referrer", &a.referrer);
            add("visit:utm_source", &a.utm_source);
            add("visit:utm_medium", &a.utm_medium);
            add("visit:utm_campaign", &a.utm_campaign);
            add("visit:utm_content", &a.utm_content);
            add("visit:utm_term", &a.utm_term);
            add("visit:country", &a.country);
            add("visit:region", &a.region);
            if a.city != 0 {
                add("visit:city", &a.city.to_string());
            }
            add("visit:device", &a.device);
            add("visit:browser", &a.browser);
            add("visit:browser_version", &a.browser_version);
            add("visit:os", &a.os);
            add("visit:os_version", &a.os_version);
        }
        Pools {
            by_dim: sets
                .into_iter()
                .map(|(k, v)| (k, v.into_iter().collect()))
                .collect(),
        }
    }

    fn values(&self, dim: &str) -> &[String] {
        self.by_dim.get(dim).map(Vec::as_slice).unwrap_or(&[])
    }
}

fn empty_label(dim: &str) -> &'static str {
    match dim {
        "visit:source" | "visit:referrer" => "Direct / None",
        "visit:channel" => "Direct",
        "event:name" | "event:page" | "event:hostname" | "visit:entry_page" | "visit:exit_page" => {
            "(none)"
        }
        _ => "(not set)",
    }
}

struct Gen<'a> {
    rng: StdRng,
    pools: &'a Pools,
    nows: &'a [i64],
    berlin: TimeZone,
}

#[derive(Clone, Copy, Default)]
struct FilterRules {
    /// 不要事件级叶子（`exit_rate`）。
    no_event_leaf: bool,
    /// 不要 `event:name` 与 `event:goal` 叶子（`time_on_page`、`scroll_depth`）。
    no_event_name: bool,
}

impl Gen<'_> {
    fn chance(&mut self, p: f64) -> bool {
        self.rng.random_bool(p)
    }

    /// 从该维度已有的值派生一个模式（保证有不少命中）：只用字母数字、`/`、`-`，避免正则元字符。
    fn derived(&mut self, pool: &[String], regex: bool) -> Option<String> {
        if pool.is_empty() || self.chance(0.4) {
            return None;
        }
        let s = pick(&mut self.rng, pool).clone();
        if !s
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '/' || c == '-')
        {
            return None;
        }
        let chars: Vec<char> = s.chars().collect();
        let len = self.rng.random_range(1..=chars.len().min(5));
        let from = self.rng.random_range(0..=chars.len() - len);
        let mid: String = chars[from..from + len].iter().collect();
        let head: String = chars[..len].iter().collect();
        let tail: String = chars[chars.len() - len..].iter().collect();
        Some(match (regex, self.rng.random_range(0..3)) {
            (true, 0) => format!("^{head}"),
            (true, 1) => format!("{tail}$"),
            (true, _) => format!("{mid}|{head}"),
            (false, 0) => format!("{head}*"),
            (false, 1) => format!("*{tail}"),
            (false, _) => format!("*{mid}*"),
        })
    }

    fn leaf(&mut self, rules: FilterRules) -> Value {
        let dim = loop {
            let d = if self.chance(0.4) {
                *pick(&mut self.rng, &EVENT_DIMS)
            } else {
                *pick(&mut self.rng, &VISIT_DIMS)
            };
            if rules.no_event_leaf && d.starts_with("event:") {
                continue;
            }
            if rules.no_event_name && (d == "event:name" || d == "event:goal") {
                continue;
            }
            break d;
        };
        if dim == "event:goal" {
            return self.goal_leaf();
        }
        let op = if dim == "visit:city" {
            *pick(&mut self.rng, &["is", "is_not"])
        } else {
            *pick(&mut self.rng, &OPS)
        };
        let pool: Vec<String> = self.pools.values(dim).to_vec();
        let values: Vec<Value> = match op {
            "is" | "is_not" => {
                let mut v: Vec<Value> = Vec::new();
                for _ in 0..self.rng.random_range(1..4) {
                    let roll = self.rng.random_range(0..100);
                    let s = if roll < 70 && !pool.is_empty() {
                        pick(&mut self.rng, &pool).clone()
                    } else if roll < 85 && dim != "visit:city" {
                        // 展示文案：应当命中空值
                        empty_label(dim).to_owned()
                    } else if roll < 92 && dim != "visit:city" {
                        String::new()
                    } else if dim == "visit:city" {
                        "0".to_owned()
                    } else {
                        "no-such-value".to_owned()
                    };
                    let s = if self.chance(0.2) && dim != "visit:city" {
                        s.to_uppercase()
                    } else {
                        s
                    };
                    if dim == "visit:city" && self.chance(0.5) {
                        v.push(json!(s.parse::<u64>().unwrap_or(0)));
                    } else {
                        v.push(json!(s));
                    }
                }
                v
            }
            "contains" | "contains_not" => (0..self.rng.random_range(1..3))
                .map(|_| {
                    if !pool.is_empty() && self.chance(0.8) {
                        let s = pick(&mut self.rng, &pool).clone();
                        let chars: Vec<char> = s.chars().collect();
                        let len = self.rng.random_range(1..=chars.len().min(4));
                        let from = self.rng.random_range(0..=chars.len() - len);
                        let sub: String = chars[from..from + len].iter().collect();
                        json!(if self.chance(0.2) {
                            sub.to_uppercase()
                        } else {
                            sub
                        })
                    } else {
                        json!("zz")
                    }
                })
                .collect(),
            "matches" | "matches_not" => (0..self.rng.random_range(1..3))
                .map(|_| match self.derived(&pool, true) {
                    Some(p) => json!(p),
                    None => json!(*pick(&mut self.rng, &REGEXES)),
                })
                .collect(),
            _ => (0..self.rng.random_range(1..3))
                .map(|_| match self.derived(&pool, false) {
                    Some(p) => json!(p),
                    None => json!(*pick(&mut self.rng, &WILDCARDS)),
                })
                .collect(),
        };
        let mut f = vec![json!(op), json!(dim), Value::Array(values)];
        if self.chance(0.3) {
            f.push(json!({"case_sensitive": self.chance(0.4)}));
        }
        Value::Array(f)
    }

    /// `event:goal` 叶子：`is` / `is_not`，取值是目标的标签（偶尔换大小写）、id（字符串或整数）或不存在的名字。
    fn goal_leaf(&mut self) -> Value {
        let op = *pick(&mut self.rng, &["is", "is", "is_not"]);
        let mut values: Vec<Value> = Vec::new();
        for _ in 0..self.rng.random_range(1..3) {
            let (id, _, v, d) = *pick(&mut self.rng, &GOALS);
            let label = d.unwrap_or(v).to_owned();
            values.push(match self.rng.random_range(0..10) {
                0..=4 => json!(label),
                5 => json!(label.to_uppercase()),
                6 => json!(id),
                7 => json!(id.to_string()),
                _ => json!("no-such-goal"),
            });
        }
        let mut f = vec![json!(op), json!("event:goal"), Value::Array(values)];
        if self.chance(0.3) {
            f.push(json!({"case_sensitive": self.chance(0.4)}));
        }
        Value::Array(f)
    }

    fn node(&mut self, depth: usize, rules: FilterRules) -> Value {
        if depth < 2 && self.chance(0.35) {
            match self.rng.random_range(0..3) {
                0 => json!([
                    "and",
                    [self.node(depth + 1, rules), self.node(depth + 1, rules)]
                ]),
                1 => json!([
                    "or",
                    [self.node(depth + 1, rules), self.node(depth + 1, rules)]
                ]),
                _ => json!(["not", self.node(depth + 1, rules)]),
            }
        } else {
            self.leaf(rules)
        }
    }

    fn timestamp_bound(&mut self, instant: i64) -> String {
        let offsets = ["Z", "+01:00", "+02:00", "+05:45", "-03:00"];
        let off = *pick(&mut self.rng, &offsets);
        let secs = instant.div_euclid(1000);
        let z = Timestamp::from_second(secs).unwrap();
        let o: jiff::tz::Offset = if off == "Z" {
            jiff::tz::Offset::UTC
        } else {
            let sign = if off.starts_with('-') { -1 } else { 1 };
            let h: i32 = off[1..3].parse().unwrap();
            let m: i32 = off[4..6].parse().unwrap();
            jiff::tz::Offset::from_seconds(sign * (h * 3600 + m * 60)).unwrap()
        };
        let local = z.to_zoned(TimeZone::fixed(o)).datetime();
        if off == "Z" {
            format!("{local}Z")
        } else {
            format!("{local}{off}")
        }
    }

    /// 日期范围；`small`：时间维度是小时，范围不能太长。
    fn date_range(&mut self, small: bool, minute: bool, now: i64) -> Value {
        let form = self.rng.random_range(0..100);
        if minute {
            // 分钟桶上限 25 小时：只用 ≤ 25 小时的范围（含 DST 回拨日与跨午夜的时间戳范围）
            return match form {
                0..=24 => json!("day"),
                25..=44 => json!("24h"),
                45..=64 => json!("realtime"),
                65..=74 => json!("1d"),
                _ => {
                    let (from, _) = self.random_dates(0, now);
                    let start = from
                        .at(
                            self.rng.random_range(0..24),
                            self.rng.random_range(0..60),
                            0,
                            0,
                        )
                        .to_zoned(self.berlin.clone())
                        .unwrap()
                        .timestamp()
                        .as_millisecond();
                    let len = self.rng.random_range(60_000..20 * 3_600_000);
                    let a = self.timestamp_bound(start);
                    let b = self.timestamp_bound(start + len);
                    json!([a, b])
                }
            };
        }
        if small {
            return match form {
                0..=19 => json!("day"),
                20..=34 => json!("24h"),
                35..=44 => json!("realtime"),
                45..=54 => json!("2d"),
                55..=64 => json!("1d"),
                65..=74 => json!("7d"),
                _ => {
                    let (from, len) = self.random_dates(10, now);
                    let to = from.checked_add(jiff::Span::new().days(len)).unwrap();
                    json!([from.to_string(), to.to_string()])
                }
            };
        }
        match form {
            0..=5 => json!("day"),
            6..=8 => json!("24h"),
            9..=10 => json!("realtime"),
            11..=18 => json!("7d"),
            19..=24 => json!("28d"),
            25..=30 => json!("30d"),
            31..=33 => json!("91d"),
            34..=37 => json!("month"),
            38..=40 => json!("3mo"),
            41..=43 => json!("6mo"),
            44..=46 => json!("12mo"),
            47..=50 => json!("year"),
            51..=54 => json!("all"),
            55..=85 => {
                let (from, len) = self.random_dates(14, now);
                let to = from.checked_add(jiff::Span::new().days(len)).unwrap();
                json!([from.to_string(), to.to_string()])
            }
            _ => {
                let (from, _) = self.random_dates(0, now);
                let start = from
                    .at(
                        self.rng.random_range(0..24),
                        self.rng.random_range(0..60),
                        0,
                        0,
                    )
                    .to_zoned(self.berlin.clone())
                    .unwrap()
                    .timestamp()
                    .as_millisecond();
                let len = self.rng.random_range(600_000..80 * 3_600_000);
                let a = self.timestamp_bound(start);
                let b = self.timestamp_bound(start + len);
                json!([a, b])
            }
        }
    }

    /// 窗口附近的随机日期与长度；起点不晚于 `now`（否则范围在未来，结果必空）。
    fn random_dates(&mut self, max_len: i64, now: i64) -> (Date, i64) {
        let today = jiff::Timestamp::from_millisecond(now)
            .unwrap()
            .to_zoned(self.berlin.clone())
            .date();
        let bases: Vec<Date> = [
            date(2026, 3, 26),
            date(2026, 3, 26),
            date(2026, 3, 26),
            date(2026, 10, 22),
            date(2026, 10, 22),
            date(2026, 10, 22),
            date(2025, 12, 28),
            date(2026, 3, 18),
        ]
        .into_iter()
        .filter(|b| *b <= today)
        .collect();
        let base = *pick(&mut self.rng, &bases);
        let from = base
            .checked_add(jiff::Span::new().days(self.rng.random_range(0..7)))
            .unwrap();
        let len = if max_len == 0 {
            0
        } else {
            self.rng.random_range(0..=max_len)
        };
        (from, len)
    }

    /// 返回 `(请求, 站点下标, now)`。`focus`：这一轮必含的指标（轮换，保证每个指标都被大量覆盖）。
    fn spec(&mut self, focus: &str) -> (Value, usize, i64) {
        let site_idx = self.rng.random_range(0..2);
        let now = *pick(&mut self.rng, self.nows);

        // 过滤规则随焦点指标而定
        let mut rules = FilterRules::default();
        if focus == "exit_rate" {
            rules.no_event_leaf = true;
        }
        if matches!(focus, "time_on_page" | "scroll_depth") {
            rules.no_event_name = true;
        }

        // 维度
        let mut dims: Vec<&str> = Vec::new();
        let ndims = match self.rng.random_range(0..100) {
            0..=19 => 0,
            20..=64 => 1,
            _ => 2,
        };
        let mut have_time = false;
        for _ in 0..ndims {
            let d = match self.rng.random_range(0..100) {
                0..=24 if !have_time => {
                    have_time = true;
                    *pick(&mut self.rng, &TIME_DIMS)
                }
                25..=59 => *pick(&mut self.rng, &EVENT_DIMS),
                _ => *pick(&mut self.rng, &VISIT_DIMS),
            };
            if !dims.contains(&d) {
                dims.push(d);
            }
        }
        match focus {
            "exit_rate" => {
                dims = vec!["event:page"];
                have_time = false;
            }
            "percentage" => {
                dims.retain(|d| !d.starts_with("time") && *d != "event:goal");
                have_time = false;
                if dims.is_empty() {
                    dims.push(*pick(&mut self.rng, &VISIT_DIMS));
                }
            }
            "time_on_page" | "scroll_depth" | "views_per_visit" | "bounce_rate"
            | "visit_duration" | "engaged_duration" => {
                dims.retain(|d| *d != "event:name" && *d != "event:goal");
            }
            _ => {}
        }
        let hour_dim = dims.contains(&"time:hour");
        let minute_dim = dims.contains(&"time:minute");

        // 过滤
        let mut filters: Vec<Value> = Vec::new();
        let nfilters = match self.rng.random_range(0..100) {
            0..=49 => 0,
            50..=94 => 1,
            _ => 2,
        };
        for _ in 0..nfilters {
            filters.push(self.node(0, rules));
        }
        // 转化率要有目标维度或目标过滤：都没有时补一个（目标过滤常与其他过滤组合）
        if focus == "conversion_rate"
            && !dims.contains(&"event:goal")
            && !Value::Array(filters.clone())
                .to_string()
                .contains("\"event:goal\"")
        {
            if self.chance(0.5) && dims.len() < 3 {
                dims.insert(0, "event:goal");
            } else {
                filters.push(self.goal_leaf());
            }
        }
        let filter_json = Value::Array(filters.clone());
        let text = filter_json.to_string();
        let has_event_leaf = text.contains("\"event:name\"")
            || text.contains("\"event:page\"")
            || text.contains("\"event:hostname\"")
            || text.contains("\"event:goal\"");
        let has_event_name_leaf =
            text.contains("\"event:name\"") || text.contains("\"event:goal\"");
        let has_goal_leaf = text.contains("\"event:goal\"");

        // 指标
        let dims_has_event_name = dims.contains(&"event:name");
        let dims_has_goal = dims.contains(&"event:goal");
        let mut eligible: Vec<&str> = vec!["visitors", "visits", "pageviews", "events"];
        if !dims_has_event_name && !dims_has_goal {
            eligible.extend([
                "views_per_visit",
                "bounce_rate",
                "visit_duration",
                "engaged_duration",
            ]);
        }
        if !dims.is_empty() && !have_time && !dims_has_goal {
            eligible.push("percentage");
        }
        if dims == ["event:page"] && !has_event_leaf {
            eligible.push("exit_rate");
        }
        if !dims_has_event_name && !has_event_name_leaf && !dims_has_goal {
            eligible.extend(["time_on_page", "scroll_depth"]);
        }
        if dims_has_goal || has_goal_leaf {
            eligible.push("conversion_rate");
        }
        if dims_has_goal {
            eligible.retain(|m| GOAL_DIM_METRICS.contains(m));
        }
        let mut metrics: Vec<&str> = Vec::new();
        if eligible.contains(&focus) {
            metrics.push(focus);
        }
        for _ in 0..self.rng.random_range(0..4) {
            let m = *pick(&mut self.rng, &eligible);
            if !metrics.contains(&m) {
                metrics.push(m);
            }
        }
        if metrics.is_empty() {
            metrics.push("visitors");
        }
        // 打乱顺序（焦点指标不总在第一个）
        for i in (1..metrics.len()).rev() {
            let j = self.rng.random_range(0..=i);
            metrics.swap(i, j);
        }

        let mut spec = json!({
            "metrics": metrics,
            "date_range": self.date_range(hour_dim, minute_dim, now),
        });
        if !dims.is_empty() {
            spec["dimensions"] = json!(dims);
        }
        if !filters.is_empty() {
            spec["filters"] = filter_json;
        }
        // order_by
        if self.chance(0.5) {
            let mut keys: Vec<String> = metrics.iter().map(|m| m.to_string()).collect();
            keys.extend(dims.iter().map(|d| d.to_string()));
            let mut ob: Vec<Value> = Vec::new();
            let mut used: Vec<String> = Vec::new();
            for _ in 0..self.rng.random_range(1..3) {
                let k = pick(&mut self.rng, &keys).clone();
                if used.contains(&k) {
                    continue;
                }
                used.push(k.clone());
                ob.push(json!([k, if self.chance(0.5) { "asc" } else { "desc" }]));
            }
            spec["order_by"] = Value::Array(ob);
        }
        // include
        let mut include = serde_json::Map::new();
        if have_time && self.chance(0.6) {
            include.insert("time_labels".into(), json!(true));
        }
        if self.chance(0.4) {
            include.insert("total_rows".into(), json!(true));
        }
        if self.chance(0.35) {
            let c = match self.rng.random_range(0..4) {
                0 | 1 => json!("previous_period"),
                2 => json!("year_over_year"),
                _ => {
                    let (from, len) = self.random_dates(20, now);
                    let to = from.checked_add(jiff::Span::new().days(len)).unwrap();
                    json!([from.to_string(), to.to_string()])
                }
            };
            include.insert("compare".into(), c);
        }
        if !include.is_empty() {
            spec["include"] = Value::Object(include);
        }
        if self.chance(0.45) {
            let limit = *pick(&mut self.rng, &[1u32, 2, 3, 5, 10, 50]);
            let offset = *pick(&mut self.rng, &[0u32, 0, 0, 0, 1, 2]);
            spec["pagination"] = json!({"limit": limit, "offset": offset});
        }
        (spec, site_idx, now)
    }
}

// ======================================================================================
// 比较与最小化
// ======================================================================================

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() <= 1e-9 * a.abs().max(b.abs()).max(1.0)
}

fn metric_names(spec: &Value) -> Vec<String> {
    spec["metrics"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m.as_str().unwrap().to_owned())
        .collect()
}

fn compare_metrics(
    names: &[String],
    got: &[MetricValue],
    want: &[f64],
    what: &str,
) -> Result<(), String> {
    if got.len() != want.len() {
        return Err(format!("{what}: 指标个数 {} 对 {}", got.len(), want.len()));
    }
    for ((name, g), w) in names.iter().zip(got).zip(want) {
        match g {
            MetricValue::Int(n) => {
                if !is_count_metric(name) {
                    return Err(format!("{what}: {name} 不该是整数"));
                }
                if *n as f64 != *w {
                    return Err(format!("{what}: {name} 查询 {n} 参考 {w}"));
                }
            }
            MetricValue::Float(f) => {
                if is_count_metric(name) {
                    return Err(format!("{what}: {name} 应该是整数"));
                }
                if !close(*f, *w) {
                    return Err(format!("{what}: {name} 查询 {f} 参考 {w}"));
                }
            }
        }
    }
    Ok(())
}

fn compare_results(spec: &Value, got: &QueryResult, want: &RefResult) -> Result<(), String> {
    let names = metric_names(spec);
    if got.results.len() != want.rows.len() {
        return Err(format!(
            "行数：查询 {} 参考 {}",
            got.results.len(),
            want.rows.len()
        ));
    }
    for (i, (g, w)) in got.results.iter().zip(&want.rows).enumerate() {
        if g.dimensions != w.dimensions {
            return Err(format!(
                "第 {i} 行维度：查询 {:?} 参考 {:?}",
                g.dimensions, w.dimensions
            ));
        }
        compare_metrics(
            &names,
            &g.metrics,
            &w.metrics,
            &format!("第 {i} 行 {:?}", g.dimensions),
        )?;
        match (&g.comparison, &w.comparison) {
            (None, None) => {}
            (Some(gc), Some((wm, wc))) => {
                if gc.dimensions != g.dimensions {
                    return Err(format!("第 {i} 行对比的维度不一致"));
                }
                compare_metrics(
                    &names,
                    &gc.metrics,
                    wm,
                    &format!("第 {i} 行对比期 {:?}", g.dimensions),
                )?;
                for (k, (a, b)) in gc.change.iter().zip(wc).enumerate() {
                    if !close(*a, *b) {
                        return Err(format!("第 {i} 行 {} 的变化：查询 {a} 参考 {b}", names[k]));
                    }
                }
            }
            (g, w) => {
                return Err(format!(
                    "第 {i} 行对比期有无不一致：查询 {} 参考 {}",
                    g.is_some(),
                    w.is_some()
                ));
            }
        }
    }
    if got.meta.time_labels != want.time_labels {
        return Err(format!(
            "time_labels：查询 {:?} 参考 {:?}",
            got.meta.time_labels, want.time_labels
        ));
    }
    if got.meta.comparison_time_labels != want.comparison_time_labels {
        return Err(format!(
            "comparison_time_labels：查询 {:?} 参考 {:?}",
            got.meta.comparison_time_labels, want.comparison_time_labels
        ));
    }
    if got.meta.total_rows != want.total_rows {
        return Err(format!(
            "total_rows：查询 {:?} 参考 {:?}",
            got.meta.total_rows, want.total_rows
        ));
    }
    match (&got.comparison_results, &want.comparison_rows) {
        (None, None) => {}
        (Some(g), Some(w)) => {
            if g.len() != w.len() {
                return Err(format!(
                    "comparison_results 行数：查询 {} 参考 {}",
                    g.len(),
                    w.len()
                ));
            }
            for (i, (a, (wd, wm))) in g.iter().zip(w).enumerate() {
                if &a.dimensions != wd {
                    return Err(format!(
                        "comparison_results 第 {i} 行维度：查询 {:?} 参考 {:?}",
                        a.dimensions, wd
                    ));
                }
                compare_metrics(
                    &names,
                    &a.metrics,
                    wm,
                    &format!("comparison_results 第 {i} 行"),
                )?;
            }
        }
        (g, w) => {
            return Err(format!(
                "comparison_results 有无不一致：查询 {} 参考 {}",
                g.is_some(),
                w.is_some()
            ));
        }
    }
    // 元信息的基本形状
    if got.meta.tiers != [rivu_query::plan::Tier::Raw] {
        return Err("meta.tiers 应恒为 [raw]".into());
    }
    for m in &names {
        if !got.meta.metric_semantics.contains_key(m) {
            return Err(format!("meta.metric_semantics 缺少 {m}"));
        }
    }
    Ok(())
}

enum Verdict {
    Ok {
        rows: usize,
    },
    Mismatch(String),
    /// 引擎拒绝了请求（校验错误）：生成器本不该产生，或是桶数超限。
    Rejected(String),
}

struct Harness {
    reader: Client,
    sites: Vec<SiteDef>,
    /// 每个站点的参考输入。
    events: Vec<Vec<RefEvent>>,
}

impl Harness {
    async fn check(&self, spec: &Value, site_idx: usize, now: i64) -> Verdict {
        let site = &self.sites[site_idx];
        let parsed = match QuerySpec::parse(spec) {
            Ok(s) => s,
            Err(e) => return Verdict::Rejected(format!("解析：{e}")),
        };
        let ctx = SiteContext {
            site_id: site.id,
            site_key: Some(site.key.clone()),
            timezone: site.tz.clone(),
            week_starts_on: site.week_start,
            geo_attribution: None,
            geo_capability: None,
            goals: goal_defs(),
            retention_days: None,
        };
        let got = execute(
            &self.reader,
            &TenantScope::new(site.id),
            &ctx,
            parsed,
            ExecOptions::new(TIMEOUT_SECS)
                .with_now(Timestamp::from_millisecond(now).unwrap())
                .with_route("differential"),
        )
        .await;
        let got = match got {
            Ok(g) => g,
            Err(QueryError::Validation { path, message }) => {
                return Verdict::Rejected(format!("{path}: {message}"));
            }
            Err(e) => return Verdict::Mismatch(format!("执行失败：{e}")),
        };
        let want = match ref_run(
            spec,
            &RefSite {
                site_id: site.id,
                tz: site.tz.clone(),
                week_start: site.week_start,
                now_ms: now,
                goals: ref_goals(),
            },
            &self.events[site_idx],
        ) {
            Ok(w) => w,
            Err(e) => return Verdict::Mismatch(format!("参考实现失败：{e}")),
        };
        match compare_results(spec, &got, &want) {
            Ok(()) => Verdict::Ok {
                rows: got.results.len(),
            },
            Err(m) => Verdict::Mismatch(m),
        }
    }

    /// 贪心最小化：反复尝试去掉一块，只要仍然是「不一致」就保留。
    async fn shrink(&self, mut spec: Value, site_idx: usize, now: i64) -> Value {
        loop {
            let mut progressed = false;
            for cand in shrink_candidates(&spec) {
                if matches!(self.check(&cand, site_idx, now).await, Verdict::Mismatch(_)) {
                    spec = cand;
                    progressed = true;
                    break;
                }
            }
            if !progressed {
                return spec;
            }
        }
    }
}

fn shrink_candidates(spec: &Value) -> Vec<Value> {
    let mut out = Vec::new();
    for key in ["metrics", "dimensions", "filters", "order_by"] {
        if let Some(arr) = spec.get(key).and_then(Value::as_array) {
            for i in 0..arr.len() {
                if key == "metrics" && arr.len() == 1 {
                    continue;
                }
                let mut c = spec.clone();
                let a = c[key].as_array_mut().unwrap();
                a.remove(i);
                if a.is_empty() {
                    c.as_object_mut().unwrap().remove(key);
                }
                out.push(c);
            }
        }
    }
    for key in ["include", "pagination"] {
        if spec.get(key).is_some() {
            let mut c = spec.clone();
            c.as_object_mut().unwrap().remove(key);
            out.push(c);
        }
    }
    if let Some(inc) = spec.get("include").and_then(Value::as_object) {
        for k in inc.keys() {
            let mut c = spec.clone();
            c["include"].as_object_mut().unwrap().remove(k);
            out.push(c);
        }
    }
    // 过滤节点：用子节点替换 and/or/not；去掉叶子里多余的值
    if let Some(fs) = spec.get("filters").and_then(Value::as_array) {
        for (i, f) in fs.iter().enumerate() {
            for r in simplify_filter(f) {
                let mut c = spec.clone();
                c["filters"][i] = r;
                out.push(c);
            }
        }
    }
    out
}

fn simplify_filter(f: &Value) -> Vec<Value> {
    let Some(a) = f.as_array() else { return vec![] };
    let mut out = Vec::new();
    match a[0].as_str().unwrap_or("") {
        "and" | "or" => {
            let kids = a[1].as_array().unwrap();
            for k in kids {
                out.push(k.clone());
            }
            for (i, k) in kids.iter().enumerate() {
                for r in simplify_filter(k) {
                    let mut c = f.clone();
                    c[1][i] = r;
                    out.push(c);
                }
            }
        }
        "not" => {
            out.push(a[1].clone());
            for r in simplify_filter(&a[1]) {
                out.push(json!(["not", r]));
            }
        }
        _ => {
            let vals = a[2].as_array().unwrap();
            if vals.len() > 1 {
                for i in 0..vals.len() {
                    let mut c = f.clone();
                    c[2].as_array_mut().unwrap().remove(i);
                    out.push(c);
                }
            }
            if a.len() == 4 {
                out.push(json!([a[0], a[1], a[2]]));
            }
        }
    }
    out
}

// ======================================================================================
// 测试
// ======================================================================================

fn ua_parser() -> Arc<UaParser> {
    static PARSER: std::sync::OnceLock<Arc<UaParser>> = std::sync::OnceLock::new();
    Arc::clone(PARSER.get_or_init(|| Arc::new(UaParser::embedded().expect("uap-core 规则"))))
}

fn days_between(from: Date, to: Date) -> Vec<Date> {
    let mut out = Vec::new();
    let mut d = from;
    while d <= to {
        out.push(d);
        d = d.tomorrow().unwrap();
    }
    out
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn query_engine_matches_the_reference_on_random_specs() {
    let (Some(pg_url), Some(ch_url)) = (
        var("RIVU_TEST_DATABASE_URL"),
        var("RIVU_TEST_CLICKHOUSE_URL"),
    ) else {
        println!(
            "跳过 query_engine_matches_the_reference_on_random_specs：需要设置 RIVU_TEST_DATABASE_URL 和 RIVU_TEST_CLICKHOUSE_URL"
        );
        return;
    };
    let seed = std::env::var("RIVU_DIFF_SEED")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(SEED_DEFAULT);
    let cases = std::env::var("RIVU_DIFF_CASES")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(CASES_DEFAULT);
    println!("差分测试：种子 {seed}，随机请求 {cases} 个（RIVU_DIFF_SEED / RIVU_DIFF_CASES 可改）");
    migrate_once(&pg_url, &ch_url).await;

    // ---- 站点、盐 -------------------------------------------------------------------------
    let pg = rivu_storage_pg::connect(&pg_url, 4).await.expect("连接 PG");
    let berlin = TimeZone::get("Europe/Berlin").unwrap();
    let mut sites = Vec::new();
    for (name, tz_name, week_start, hostnames) in [
        (
            "diff-berlin",
            "Europe/Berlin",
            Weekday::Monday,
            &["example.test", "shop.example.test"][..],
        ),
        (
            "diff-kathmandu",
            "Asia/Kathmandu",
            Weekday::Sunday,
            &["example.test", "blog.example.test"][..],
        ),
    ] {
        let hosts: Vec<String> = hostnames.iter().map(|s| s.to_string()).collect();
        let created = rivu_storage_pg::sites::create_site(
            &pg,
            &NewSite {
                name,
                timezone: tz_name,
                hostnames: &hosts,
            },
        )
        .await
        .expect("建站");
        sites.push(SiteDef {
            id: created.id,
            key: created.site_key,
            tz_name,
            tz: TimeZone::get(tz_name).unwrap(),
            week_start,
            hostnames: match tz_name {
                "Europe/Berlin" => &["example.test", "shop.example.test"],
                _ => &["example.test", "blog.example.test"],
            },
        });
    }
    rivu_storage_pg::salts::ensure_salts(&pg, &days_between(date(2025, 12, 1), date(2026, 11, 8)))
        .await
        .expect("ensure_salts");
    let snapshot = Arc::new(SiteSnapshot::new());
    snapshot.reload(&pg).await.expect("加载站点快照");
    let salts = Arc::new(SaltCache::new());
    salts
        .reload(&pg, date(2025, 11, 25), date(2026, 11, 10))
        .await
        .expect("加载盐");

    // ---- 管线与写入器 -----------------------------------------------------------------------
    let geo = Arc::new(GeoDb::with_manual_attribution(
        Some("MaxMind test database"),
        Some("https://github.com/maxmind/MaxMind-DB"),
    ));
    let mmdb = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../crates/geo/tests/data/test-data/GeoIP2-City-Test.mmdb");
    assert!(geo.reload_if_changed(&mmdb).expect("加载测试库"));
    let sink = Arc::new(ChSink::new(rivu_storage_ch::writer_client(
        &ch_url,
        &account("writer"),
        &ch_database(),
    )));
    let mut wopts = WriterOptions::new(BufferConfig {
        flush_interval: Duration::from_millis(100),
        max_rows: 500,
        max_bytes: 8 << 20,
        channel_capacity: 100_000,
    });
    wopts.counter_flush_interval = Duration::from_secs(10);
    let writer = BatchWriter::spawn(Arc::clone(&sink), wopts);
    let pipeline = Pipeline::new(
        Arc::clone(&snapshot),
        Arc::clone(&salts),
        writer.handle(),
        vec!["127.0.0.1/32".parse().unwrap()],
        ua_parser(),
        Arc::clone(&geo),
    );
    let _: Option<SocketAddr> = None;

    // ---- 生成并发送事件 ---------------------------------------------------------------------
    let mut rng = StdRng::seed_from_u64(seed);
    let nows = now_pool(&berlin);
    let persons = 24;
    let mut generated: Vec<Vec<GenEvent>> = Vec::new();
    let mut accepted: Vec<Vec<bool>> = Vec::new();
    for site in &sites {
        let evs = generate(&mut rng, site, persons, &nows);
        let mut ok = Vec::with_capacity(evs.len());
        for e in &evs {
            let k = e.person;
            let ctx = RequestCtx {
                peer: "127.0.0.1".parse().unwrap(),
                xff: vec![IPS[k % IPS.len()].to_owned()],
                user_agent: UAS[(k / IPS.len()) % UAS.len()].to_owned(),
                now_ms: e.ts_ms,
            };
            let out = pipeline.process(body_for(site, e).to_string().as_bytes(), ctx);
            match (&out, e.name == "engagement") {
                (Outcome::Accepted, _) => ok.push(true),
                (Outcome::Dropped(rivu_domain::DropReason::NoSession), true) => ok.push(false),
                _ => panic!("意外的管线结果 {out:?}：{e:?}"),
            }
        }
        generated.push(evs);
        accepted.push(ok);
    }
    drop(pipeline);
    writer.shutdown().await.expect("关停写入器");

    // ---- 参考实现的输入 ---------------------------------------------------------------------
    let reader = rivu_storage_ch::client(&ch_url, &account("reader"), Some(&ch_database()));
    let mut all_events: Vec<Vec<RefEvent>> = Vec::new();
    for (si, site) in sites.iter().enumerate() {
        let stored = read_back(&reader, site.id).await;
        let n_accepted = accepted[si].iter().filter(|a| **a).count();
        assert_eq!(stored.len(), n_accepted, "入库行数 = 管线接受数");
        let mut refs = Vec::new();
        for (e, acc) in generated[si].iter().zip(&accepted[si]) {
            let attrs = match stored.get(&e.ts_ms) {
                Some(s) => {
                    assert!(*acc, "没被接受的事件不该入库");
                    assert_eq!(
                        (s.hostname.as_str(), s.pathname.as_str()),
                        (e.host.as_str(), e.path.as_str())
                    );
                    RefAttrs {
                        referrer: s.referrer.clone(),
                        referrer_source: s.referrer_source.clone(),
                        channel: s.channel.clone(),
                        utm_source: s.utm_source.clone(),
                        utm_medium: s.utm_medium.clone(),
                        utm_campaign: s.utm_campaign.clone(),
                        utm_content: s.utm_content.clone(),
                        utm_term: s.utm_term.clone(),
                        country: s.country.clone(),
                        region: s.region_code.clone(),
                        city: s.city_geoname_id,
                        device: s.device.clone(),
                        browser: s.browser.clone(),
                        browser_version: s.browser_version.clone(),
                        os: s.os.clone(),
                        os_version: s.os_version.clone(),
                    }
                }
                None => {
                    assert!(!*acc, "被接受的事件应当入库");
                    RefAttrs::default()
                }
            };
            refs.push(RefEvent {
                site_id: site.id,
                person: e.person as u64 + 1,
                ts_ms: e.ts_ms,
                name: e.name.clone(),
                hostname: e.host.clone(),
                pathname: e.path.clone(),
                interactive: e.interactive,
                engagement_ms: e.e.unwrap_or(0),
                scroll_depth: e.sd.unwrap_or(255),
                attrs,
            });
        }
        // 参考实现的会话切分与管线一致：每个被接受的事件都并入了会话，每个被丢弃的 engagement 都没有
        let flags = owned(&refs);
        assert_eq!(
            flags, accepted[si],
            "参考实现并入会话的事件 = 管线接受的事件（{}）",
            site.tz_name
        );
        all_events.push(refs);
    }
    let flat: Vec<RefEvent> = all_events.iter().flatten().cloned().collect();
    let pools = Pools::new(&flat);
    let total_events: usize = all_events.iter().map(Vec::len).sum();
    let sessions: usize = {
        let mut s = 0;
        for evs in &all_events {
            s += rivu_reference::sessionize(
                &evs.iter()
                    .map(|e| rivu_reference::RawEvent {
                        site_id: e.site_id,
                        person: e.person,
                        ts_ms: e.ts_ms,
                        kind: rivu_domain::EventKind::from_name(&e.name),
                        hostname: e.hostname.clone(),
                        pathname: e.pathname.clone(),
                        interactive: e.interactive,
                        engagement_ms: e.engagement_ms,
                        attrs: rivu_reference::EventAttrs::default(),
                    })
                    .collect::<Vec<_>>(),
            )
            .len();
        }
        s
    };
    println!(
        "事件 {total_events} 条（两个站点：Europe/Berlin 与 Asia/Kathmandu），会话 {sessions} 个；\
         国家 {:?}，设备 {:?}",
        pools.values("visit:country"),
        pools.values("visit:device")
    );

    // ---- 随机请求 ---------------------------------------------------------------------------
    let harness = Harness {
        reader,
        sites,
        events: all_events,
    };
    let mut gen_ = Gen {
        rng: StdRng::seed_from_u64(seed ^ 0xA5A5_A5A5_A5A5_A5A5),
        pools: &pools,
        nows: &nows,
        berlin,
    };
    let (mut ok, mut nonempty, mut rejected, mut rows_total) = (0usize, 0usize, 0usize, 0usize);
    let mut features: BTreeMap<&str, usize> = BTreeMap::new();
    let mut failures: Vec<String> = Vec::new();
    for i in 0..cases {
        let focus = METRICS[i % METRICS.len()];
        let (spec, site_idx, now) = gen_.spec(focus);
        match harness.check(&spec, site_idx, now).await {
            Verdict::Ok { rows } => {
                ok += 1;
                rows_total += rows;
                if rows > 0 {
                    nonempty += 1;
                } else {
                    let no_filter = spec.get("filters").is_none();
                    let no_offset = spec["pagination"]["offset"].as_u64().unwrap_or(0) == 0;
                    *features
                        .entry(match (no_filter, no_offset) {
                            (true, true) => "empty:plain",
                            (false, true) => "empty:filters",
                            (true, false) => "empty:offset",
                            (false, false) => "empty:filters+offset",
                        })
                        .or_default() += 1;
                    if no_filter && no_offset && std::env::var("RIVU_DIFF_VERBOSE").is_ok() {
                        println!("空结果 #{i} 站点 {site_idx} now={now}: {spec}");
                    }
                }
                let text = spec.to_string();
                for (k, pat) in [
                    ("filters", "\"filters\""),
                    ("compare", "\"compare\""),
                    ("time_labels", "\"time_labels\""),
                    ("dimensions", "\"dimensions\""),
                    ("order_by", "\"order_by\""),
                    ("pagination", "\"pagination\""),
                    ("total_rows", "\"total_rows\""),
                    ("matches", "\"matches"),
                    ("contains", "\"contains"),
                    ("case_insensitive", "\"case_sensitive\":false"),
                    ("goal", "\"event:goal\""),
                    ("conversion_rate", "\"conversion_rate\""),
                ] {
                    if text.contains(pat) {
                        *features.entry(k).or_default() += 1;
                    }
                }
            }
            Verdict::Rejected(msg) => {
                if msg.contains("time buckets") {
                    rejected += 1;
                } else {
                    failures.push(format!(
                        "#{i} 站点 {site_idx} now={now}\n请求被引擎拒绝：{msg}\n{}",
                        serde_json::to_string_pretty(&spec).unwrap()
                    ));
                }
            }
            Verdict::Mismatch(msg) => {
                let min = harness.shrink(spec.clone(), site_idx, now).await;
                let min_msg = match harness.check(&min, site_idx, now).await {
                    Verdict::Mismatch(m) => m,
                    _ => msg.clone(),
                };
                failures.push(format!(
                    "#{i} 站点 {} ({}) now={now}\n不一致：{msg}\n最小化后的请求（种子 {seed}）：\n{}\n最小化后的差异：{min_msg}",
                    site_idx,
                    harness.sites[site_idx].tz_name,
                    serde_json::to_string_pretty(&min).unwrap()
                ));
            }
        }
        if failures.len() >= 3 {
            break;
        }
    }
    println!(
        "差分测试：{cases} 个随机请求，{ok} 个与参考实现完全一致（{nonempty} 个结果非空，共比较 {rows_total} 行），\
         {rejected} 个因桶数超限被引擎拒绝；覆盖：{features:?}"
    );
    for site in &harness.sites {
        cleanup(
            &rivu_storage_ch::client(&ch_url, &account("migrator"), Some(&ch_database())),
            site.id,
        )
        .await;
    }
    assert!(failures.is_empty(), "\n{}", failures.join("\n\n"));
    assert!(
        nonempty * 10 >= ok * 7,
        "非空结果太少，测试可能空转：{nonempty}/{ok}"
    );
    let _: HashSet<u8> = HashSet::new();
}
