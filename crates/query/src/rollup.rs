//! 日汇总层（`system-design.md` §5.5、§6.4 选层）。
//!
//! # 族
//!
//! 每个族一张表（`migrations/clickhouse/20261028100000_rollup.sql`），一行 = 站点本地日 × 族的维度取值。
//! 构建用查询引擎自己的 SQL 构建器：某天某族的行，就是对那一天、按该族维度分组的原始层组件的度量（`c_*` 的和），
//! 所以单日的数字与原始层一致。跨多天相加时：计数类直接相加；访客数因为访客 ID 每天轮换，各天之和就是原始层的
//! `uniq`（§5.5）；唯一的近似是跨午夜的会话在两天里各算一次（§6.4「已知近似」）。
//!
//! | 族 | 维度 | 适用的度量 |
//! |---|---|---|
//! | `site` | 无 | 全部（不含退出） |
//! | `pages` | `event:page` | 事件度量与事件形式的 visits；会话度量按入口页分组（与原始层对 `event:page` 维度的口径相同）；退出；engagement |
//! | `entry_pages` / `exit_pages` | `visit:entry_page` / `visit:exit_page` | 全部（不含退出） |
//! | `sources` | 渠道、来源、referrer、UTM ×5 | 同上 |
//! | `locations` | 国家、地区、城市 | 同上 |
//! | `devices` / `browsers` / `os` | 设备；浏览器与版本；系统与版本 | 同上 |
//! | `events` | `event:name` | 事件度量与事件形式的 visits |
//!
//! 查询用到族里的一部分维度时（例如 `sources` 只按渠道分组），按这部分维度把行再相加。会话属性在会话内恒定，
//! 所以会话类度量可以相加；访客数在「同一访客同一天有两个会话、族里的其他维度不同」时会多算（与 Plausible 的导入数据相同）。
//!
//! # 兼容（§6.4）
//!
//! 一个查询能用汇总层，当且仅当：区间是整本地日；时间粒度不细于天；非时间维度都在同一个族里；
//! 过滤只涉及这些维度本身（这样每一行对应的正是汇总表里的若干行之和，不会把同一访客在不同取值下的计数加起来）；
//! 没有事件级过滤（`event:*`，它们在会话指标上是「存在」语义，汇总表回答不了）；没有 `percentage`、`conversion_rate`。

use std::time::Duration;

use clickhouse::Client;
use jiff::Timestamp;
use jiff::civil::Date;
use jiff::tz::TimeZone;
use serde_json::json;

use crate::error::QueryError;
use crate::exec::bind;
use crate::plan::{self, Measure, Plan, VisitsForm};
use crate::resolve::{Col, Gran, Pred, RDim, ResolvedQuery, SiteContext, local_date};
use crate::spec::{Metric, QuerySpec};
use crate::sql::{self, ParamValue, Params, TenantScope};

/// The raw session scan includes this many UTC dates before the start of the interval.
pub const SESSION_LOOKBACK_DAYS: i64 = 7;

/// 一个维度族（一张汇总表）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Family {
    Site,
    Pages,
    EntryPages,
    ExitPages,
    Sources,
    Locations,
    Devices,
    Browsers,
    Os,
    Events,
    /// 自定义属性（`rollup_props`，键 × 取值）：只有导入的数据，序号指向查询的属性键（[`Col::Prop`]）。
    /// 不在 [`Family::ALL`] 里：`rollup_build` 不构建它，原生数据的属性查询走原始层。
    Props(u8),
}

impl Family {
    pub const ALL: [Family; 10] = [
        Family::Pages,
        Family::EntryPages,
        Family::ExitPages,
        Family::Sources,
        Family::Locations,
        Family::Devices,
        Family::Browsers,
        Family::Os,
        Family::Events,
        // site 放在最后：它的行是「这一天已构建」的标记，其余族都写完之后才写它
        Family::Site,
    ];

    pub fn table(self) -> &'static str {
        match self {
            Family::Site => "rollup_site",
            Family::Pages => "rollup_pages",
            Family::EntryPages => "rollup_entry_pages",
            Family::ExitPages => "rollup_exit_pages",
            Family::Sources => "rollup_sources",
            Family::Locations => "rollup_locations",
            Family::Devices => "rollup_devices",
            Family::Browsers => "rollup_browsers",
            Family::Os => "rollup_os",
            Family::Events => "rollup_events",
            Family::Props(_) => "rollup_props",
        }
    }

    /// 族的维度（按表里的列序）与对应的列名。
    pub(crate) fn dims(self) -> &'static [(Col, &'static str)] {
        match self {
            Family::Site => &[],
            Family::Pages => &[(Col::EventPage, "page")],
            Family::EntryPages => &[(Col::EntryPage, "entry_page")],
            Family::ExitPages => &[(Col::ExitPage, "exit_page")],
            Family::Sources => &[
                (Col::Channel, "channel"),
                (Col::Source, "source"),
                (Col::Referrer, "referrer"),
                (Col::UtmSource, "utm_source"),
                (Col::UtmMedium, "utm_medium"),
                (Col::UtmCampaign, "utm_campaign"),
                (Col::UtmContent, "utm_content"),
                (Col::UtmTerm, "utm_term"),
            ],
            Family::Locations => &[
                (Col::Country, "country"),
                (Col::Region, "region"),
                (Col::City, "city"),
            ],
            Family::Devices => &[(Col::Device, "device")],
            Family::Browsers => &[
                (Col::Browser, "browser"),
                (Col::BrowserVersion, "browser_version"),
            ],
            Family::Os => &[(Col::Os, "os"), (Col::OsVersion, "os_version")],
            Family::Events => &[(Col::EventName, "name")],
            // 维度是 `value`，键另由条件 `key = pk<i>` 限定（见 [`Family::column`]）
            Family::Props(_) => &[],
        }
    }

    pub(crate) fn column(self, col: Col) -> Option<&'static str> {
        if let Family::Props(i) = self {
            return (col == Col::Prop(i)).then_some("value");
        }
        self.dims().iter().find(|(c, _)| *c == col).map(|(_, n)| *n)
    }

    /// 全部汇总表（含 `rollup_props`），删除数据时用。
    pub const TABLES: [&'static str; 11] = [
        "rollup_pages",
        "rollup_entry_pages",
        "rollup_exit_pages",
        "rollup_sources",
        "rollup_locations",
        "rollup_devices",
        "rollup_browsers",
        "rollup_os",
        "rollup_events",
        "rollup_props",
        "rollup_site",
    ];

    fn of(col: Col) -> Option<Family> {
        Family::ALL.into_iter().find(|f| f.column(col).is_some())
    }

    /// 族能给出的度量。
    pub(crate) fn has(self, m: Measure) -> bool {
        match self {
            Family::Events => matches!(
                m,
                Measure::Visitors | Measure::Pageviews | Measure::Events | Measure::Visits
            ),
            Family::Props(_) => matches!(m, Measure::Visitors | Measure::Events),
            Family::Pages => m != Measure::CrBase,
            _ => !matches!(m, Measure::Exits | Measure::CrBase),
        }
    }

    /// 构建时这一族的 `visits` 口径：有事件级维度的族是事件形式。
    pub(crate) fn visits_form(self) -> VisitsForm {
        match self {
            Family::Pages | Family::Events | Family::Props(_) => VisitsForm::Events,
            _ => VisitsForm::Sessions,
        }
    }
}

/// 汇总表的度量列（与引擎组件的度量一一对应）。
pub(crate) const MEASURE_COLUMNS: [(&str, Measure); 16] = [
    ("visitors", Measure::Visitors),
    ("pageviews", Measure::Pageviews),
    ("events", Measure::Events),
    ("s_n", Measure::SessN),
    ("s_pv", Measure::SessPv),
    ("s_dur_ms", Measure::SessDurMs),
    ("s_eng_ms", Measure::SessEngMs),
    ("s_eng_n", Measure::SessEngN),
    ("b_n", Measure::BounceN),
    ("bounces", Measure::Bounces),
    ("exits", Measure::Exits),
    ("tp_ms", Measure::TpMs),
    ("tp_n", Measure::TpN),
    ("sc_sum", Measure::ScSum),
    ("sc_n", Measure::ScN),
    // `visits` 按口径分两列，见 `measure_column`
    ("visits_s", Measure::Visits),
];

/// 查询里一个度量在汇总表里的列。
pub(crate) fn measure_column(m: Measure, form: VisitsForm) -> &'static str {
    if m == Measure::Visits {
        return match form {
            VisitsForm::Events => "visits_ev",
            VisitsForm::Sessions => "visits_s",
        };
    }
    MEASURE_COLUMNS
        .iter()
        .find(|(_, x)| *x == m)
        .map(|(c, _)| *c)
        .expect("CrBase 不在汇总层，兼容判断已排除")
}

fn pred_cols(p: &Pred, out: &mut Vec<Col>) -> bool {
    match p {
        Pred::And(c) | Pred::Or(c) => c.iter().all(|x| pred_cols(x, out)),
        Pred::Not(x) => pred_cols(x, out),
        Pred::Leaf(l) => {
            out.push(l.col);
            true
        }
    }
}

/// 查询能否由汇总层回答；能则返回用哪个族。`plan` 是这次查询的规划。
pub(crate) fn compatible_family(q: &ResolvedQuery, plan: &Plan) -> Option<Family> {
    if !matches!(q.interval.kind, crate::resolve::IntervalKind::Dates { .. }) {
        return None;
    }
    if q.metrics
        .iter()
        .any(|m| matches!(m, Metric::Percentage | Metric::ConversionRate))
    {
        return None;
    }
    let mut cols: Vec<Col> = Vec::new();
    for d in &q.dims {
        match d {
            RDim::Time(Gran::Day | Gran::Week | Gran::Month) => {}
            RDim::Time(_) => return None,
            RDim::Col(c) => cols.push(*c),
        }
    }
    let mut filter_cols: Vec<Col> = Vec::new();
    if let Some(f) = &q.filter {
        pred_cols(f, &mut filter_cols);
    }
    // 过滤只能落在已分组的维度上，且不能是事件级（存在语义）
    if filter_cols
        .iter()
        .any(|c| c.is_event_level() || !cols.contains(c))
    {
        return None;
    }
    let family = match cols.as_slice() {
        [] => Family::Site,
        // 属性只有单独一个属性维度时能用（rollup_props 只有键 × 取值）
        [Col::Prop(i)] => Family::Props(*i),
        [c, ..] => Family::of(*c)?,
    };
    if cols.iter().any(|c| family.column(*c).is_none()) {
        return None;
    }
    // 维度与引擎的口径要一致：visits 的口径由维度决定，族的口径必须相同
    if plan.measures.contains(&Measure::Visits) && plan.visits_form != family.visits_form() {
        return None;
    }
    if plan.measures.iter().any(|m| !family.has(*m)) {
        return None;
    }
    Some(family)
}

/// 一次查询的分层：`[from, rollup_to]` 由汇总层回答，其余天（如果有）由原始层回答。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Split {
    pub(crate) family: Family,
    pub(crate) from: Date,
    pub(crate) rollup_to: Date,
}

// ------------------------------------------------------------------------------------------
// 构建与维护（`rivu_maintainer` 账号：events / sessions 的 SELECT，汇总表的 INSERT 与轻量删除）
// ------------------------------------------------------------------------------------------

/// 构建一族时请求的指标：族能给出的全部度量（`Family::has`）。
fn build_metrics(f: Family) -> &'static [&'static str] {
    const COMMON: [&str; 10] = [
        "visitors",
        "pageviews",
        "events",
        "visits",
        "views_per_visit",
        "bounce_rate",
        "visit_duration",
        "engaged_duration",
        "time_on_page",
        "scroll_depth",
    ];
    const PAGES: [&str; 11] = [
        "visitors",
        "pageviews",
        "events",
        "visits",
        "views_per_visit",
        "bounce_rate",
        "visit_duration",
        "engaged_duration",
        "time_on_page",
        "scroll_depth",
        "exit_rate",
    ];
    match f {
        Family::Pages => &PAGES,
        Family::Events => &["visitors", "pageviews", "events", "visits"],
        _ => &COMMON,
    }
}

/// 一天一族的构建语句。指标的组合按「只带族里一个维度」的形状校验（`exit_rate` 要求恰好 `event:page`），
/// 然后把维度换成族的全部维度（多于查询 API 允许的 4 个）。
pub(crate) fn build_query(
    scope: &TenantScope,
    site: &SiteContext,
    family: Family,
    day: Date,
    version: u64,
    now: Timestamp,
) -> Result<(String, Params), QueryError> {
    let dims: &[&str] = match family {
        Family::Pages => &["event:page"],
        Family::Events => &["event:name"],
        _ => &[],
    };
    let d = day.to_string();
    let spec = QuerySpec::parse(&json!({
        "metrics": build_metrics(family),
        "date_range": [d, d],
        "dimensions": dims,
    }))?;
    let mut q = ResolvedQuery::build(&spec, site, now, None)?;
    q.dims = family.dims().iter().map(|(c, _)| RDim::Col(*c)).collect();
    q.order.clear();
    let plan = plan::plan(&q);
    debug_assert!(plan.measures.iter().all(|m| family.has(*m)));
    Ok(sql::rollup_build_query(
        scope, &q, &plan, family, &d, version,
    ))
}

async fn fetch(
    client: &Client,
    sql: &str,
    params: &Params,
) -> Result<Vec<Vec<serde_json::Value>>, QueryError> {
    if !sql::check_tenant_scoped(sql) {
        return Err(QueryError::internal(anyhow::anyhow!(
            "generated SQL is not tenant-scoped"
        )));
    }
    let bytes = bind(client.query(sql), params)
        .fetch_bytes("JSONCompactEachRow")
        .map_err(QueryError::internal)?
        .collect()
        .await
        .map_err(QueryError::internal)?;
    serde_json::Deserializer::from_slice(&bytes)
        .into_iter::<Vec<serde_json::Value>>()
        .map(|r| r.map_err(QueryError::internal))
        .collect()
}

async fn execute(client: &Client, sql: &str, params: &Params) -> Result<(), QueryError> {
    if !sql::check_tenant_scoped(sql) {
        return Err(QueryError::internal(anyhow::anyhow!(
            "generated SQL is not tenant-scoped"
        )));
    }
    bind(client.query(sql), params)
        .execute()
        .await
        .map_err(QueryError::internal)
}

/// 一个站点的构建进度。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BuildState {
    /// 已构建（`rollup_site` 有行）的最后一个本地日。
    pub built_to: Option<Date>,
    /// 有按别的时区构建的行；删除重建前必须另行检查 raw 覆盖。
    pub tz_changed: bool,
    /// 站点最早事件的本地日（没有事件为 `None`）。
    pub first_day: Option<Date>,
    pub first_event_ms: Option<i64>,
    /// Oldest completed native day and the timezone recorded when it was built.
    pub first_marker: Option<(Date, String)>,
    /// 站点自己的数据从哪天开始：最早事件与最早的原生汇总行中较早的一天（保留期删掉旧事件后，
    /// 汇总行还在）。导入只写这天之前的日子。
    pub native_from: Option<Date>,
}

pub async fn build_state(
    client: &Client,
    scope: &TenantScope,
    tz: &TimeZone,
) -> Result<BuildState, QueryError> {
    let tz_name = tz.iana_name().unwrap_or("UTC");
    let (sql, params) = sql::rollup_probe_query(scope, tz_name, "1970-01-01", "1970-01-01", None);
    let rows = fetch(client, &sql, &params).await?;
    let row = rows.first();
    let built_to = row
        .and_then(|r| r.first())
        .and_then(serde_json::Value::as_str)
        .and_then(|s| s.parse().ok());
    let first_built: Option<Date> = row
        .and_then(|r| r.get(3))
        .and_then(serde_json::Value::as_str)
        .and_then(|s| s.parse().ok());
    let first_marker = first_built.zip(
        row.and_then(|r| r.get(4))
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned),
    );
    let tz_changed = row
        .and_then(|r| r.get(1))
        .and_then(|v| v.as_u64().or_else(|| v.as_str()?.parse().ok()))
        .unwrap_or(0)
        > 0;
    let (sql, params) = sql::first_event_query(scope);
    let rows = fetch(client, &sql, &params).await?;
    let first_ms = rows.first().and_then(|r| {
        let n = r
            .first()?
            .as_u64()
            .or_else(|| r.first()?.as_str()?.parse().ok())?;
        if n == 0 {
            return None;
        }
        let v = r.get(1)?;
        v.as_i64().or_else(|| v.as_str()?.parse().ok())
    });
    let first_day = match first_ms {
        Some(ms) => Some(local_date(tz, ms)?),
        None => None,
    };
    let native_from = match (first_day, first_built) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    };
    Ok(BuildState {
        built_to,
        tz_changed,
        first_day,
        first_event_ms: first_ms,
        first_marker,
        native_from,
    })
}

/// Earliest native day, including rows left behind by a failed family build or delete.
pub async fn first_native_day(
    client: &Client,
    scope: &TenantScope,
) -> Result<Option<Date>, QueryError> {
    let per_family = Family::TABLES
        .map(|table| {
            format!(
                "(SELECT day FROM {table} WHERE site_id = {{site:UInt64}} AND origin = 'native' \
                  ORDER BY day LIMIT 1)"
            )
        })
        .join(" UNION ALL ");
    let sql = format!("SELECT toString(minOrNull(day)) FROM ({per_family})");
    let mut params = Params::default();
    params.push("site", ParamValue::U64(scope.site_id()));
    let rows = fetch(client, &sql, &params).await?;
    match rows.first().and_then(|row| row.first()) {
        Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::String(day)) => day.parse().map(Some).map_err(QueryError::internal),
        _ => Err(QueryError::internal(anyhow::anyhow!(
            "invalid first native day response"
        ))),
    }
}

/// Families only store a day label. Require a matching site marker for every native day.
pub async fn native_days_match_timezone(
    client: &Client,
    scope: &TenantScope,
    timezone: &str,
) -> Result<bool, QueryError> {
    let mut checks = vec![
        "SELECT day FROM rollup_site WHERE site_id = {site:UInt64}
         AND origin = 'native' AND tz != {tz:String} LIMIT 1"
            .to_owned(),
    ];
    for table in Family::TABLES.into_iter().filter(|&t| t != "rollup_site") {
        checks.push(format!(
            "SELECT day FROM {table} WHERE site_id = {{site:UInt64}} AND origin = 'native'
             AND day NOT IN (SELECT day FROM rollup_site WHERE site_id = {{site:UInt64}}
                             AND origin = 'native' AND tz = {{tz:String}}) LIMIT 1"
        ));
    }
    let union = checks
        .into_iter()
        .map(|s| format!("({s})"))
        .collect::<Vec<_>>()
        .join(" UNION ALL ");
    let sql = format!("SELECT day FROM ({union}) LIMIT 1");
    let mut params = Params::default();
    params.push("site", ParamValue::U64(scope.site_id()));
    params.push("tz", ParamValue::Str(timezone.to_owned()));
    Ok(fetch(client, &sql, &params).await?.is_empty())
}

/// 构建一个站点的一个本地日：各族依次写入，`site` 最后写（它的行是「这一天已构建」的标记）。
/// 重复构建同一天是安全的：同键的行按 `version` 取最新。
pub async fn build_day(
    client: &Client,
    scope: &TenantScope,
    site: &SiteContext,
    day: Date,
    version: u64,
    now: Timestamp,
) -> Result<(), QueryError> {
    let client = client.clone().with_setting("async_insert", "0");
    for family in Family::ALL {
        let (sql, params) = build_query(scope, site, family, day, version, now)?;
        execute(&client, &sql, &params).await?;
    }
    Ok(())
}

/// 删除一个站点的汇总行：`days` 给出时只删这些本地日，否则全部；`native_only` 时导入的行（`origin = 'import'`）不动。
/// 幂等。
pub async fn delete(
    client: &Client,
    scope: &TenantScope,
    days: Option<(Date, Date)>,
    native_only: bool,
) -> Result<(), QueryError> {
    let mut params = Params::default();
    params.push("site", ParamValue::U64(scope.site_id()));
    let mut cond = String::new();
    if native_only {
        cond.push_str(" AND origin = 'native'");
    }
    if let Some((from, to)) = days {
        params.push("from_day", ParamValue::Date(from.to_string()));
        params.push("to_day", ParamValue::Date(to.to_string()));
        cond.push_str(" AND day >= {from_day:Date} AND day <= {to_day:Date}");
    }
    // 重建和范围擦除重试仍需时区凭据，最后删除标记。
    let mut order = Family::TABLES;
    if !native_only && days.is_none() {
        order.rotate_right(1);
    }
    let client = client.clone().with_setting("lightweight_deletes_sync", "2");
    for table in order {
        let sql = format!("DELETE FROM {table} WHERE site_id = {{site:UInt64}}{cond}");
        execute(&client, &sql, &params).await?;
    }
    Ok(())
}

/// 汇总行的度量（列名同 [`MEASURE_COLUMNS`] 加 `visits_ev`）。导入时按「日期 × 维度」先在内存里相加再写入：
/// `ReplacingMergeTree` 会让同键的行互相覆盖。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Measures {
    pub visitors: u64,
    pub pageviews: u64,
    pub events: u64,
    pub visits_ev: u64,
    pub visits_s: u64,
    pub s_n: u64,
    pub s_pv: u64,
    pub s_dur_ms: u64,
    pub s_eng_ms: u64,
    pub s_eng_n: u64,
    pub b_n: u64,
    pub bounces: u64,
    pub exits: u64,
    pub tp_ms: u64,
    pub tp_n: u64,
    pub sc_sum: u64,
    pub sc_n: u64,
}

impl Measures {
    pub fn add(&mut self, o: &Measures) {
        for (a, b) in self.fields_mut().into_iter().zip(o.fields()) {
            *a = a.saturating_add(b.1);
        }
    }

    fn fields(&self) -> [(&'static str, u64); 17] {
        [
            ("visitors", self.visitors),
            ("pageviews", self.pageviews),
            ("events", self.events),
            ("visits_ev", self.visits_ev),
            ("visits_s", self.visits_s),
            ("s_n", self.s_n),
            ("s_pv", self.s_pv),
            ("s_dur_ms", self.s_dur_ms),
            ("s_eng_ms", self.s_eng_ms),
            ("s_eng_n", self.s_eng_n),
            ("b_n", self.b_n),
            ("bounces", self.bounces),
            ("exits", self.exits),
            ("tp_ms", self.tp_ms),
            ("tp_n", self.tp_n),
            ("sc_sum", self.sc_sum),
            ("sc_n", self.sc_n),
        ]
    }

    fn fields_mut(&mut self) -> [&mut u64; 17] {
        [
            &mut self.visitors,
            &mut self.pageviews,
            &mut self.events,
            &mut self.visits_ev,
            &mut self.visits_s,
            &mut self.s_n,
            &mut self.s_pv,
            &mut self.s_dur_ms,
            &mut self.s_eng_ms,
            &mut self.s_eng_n,
            &mut self.b_n,
            &mut self.bounces,
            &mut self.exits,
            &mut self.tp_ms,
            &mut self.tp_n,
            &mut self.sc_sum,
            &mut self.sc_n,
        ]
    }
}

/// 导入写入的一行：站点本地日、维度取值（与表的维度列一一对应）、度量。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportRow {
    pub day: Date,
    pub dims: Vec<String>,
    pub m: Measures,
}

/// 各汇总表的维度列（导入写入时的白名单；`rollup_site` 另有 `tz`）。
pub const IMPORT_DIMS: [(&str, &[&str]); 11] = [
    ("rollup_site", &[]),
    ("rollup_pages", &["page"]),
    ("rollup_entry_pages", &["entry_page"]),
    ("rollup_exit_pages", &["exit_page"]),
    (
        "rollup_sources",
        &[
            "channel",
            "source",
            "referrer",
            "utm_source",
            "utm_medium",
            "utm_campaign",
            "utm_content",
            "utm_term",
        ],
    ),
    ("rollup_locations", &["country", "region", "city"]),
    ("rollup_devices", &["device"]),
    ("rollup_browsers", &["browser", "browser_version"]),
    ("rollup_os", &["os", "os_version"]),
    ("rollup_events", &["name"]),
    ("rollup_props", &["key", "value"]),
];

/// 导入写入的行：每张汇总表一个结构体（列名即字段名），维度列不同、度量列相同。
/// `day` 是 RowBinary 的 `Date`（1970-01-01 起的天数），`origin` 是 `Enum8` 的 `'import' = 2`。
macro_rules! import_row {
    ($name:ident $(with $tz:ident)? { $($dim:ident),* }) => {
        #[derive(clickhouse::Row, serde::Serialize)]
        struct $name {
            site_id: u64,
            day: u16,
            origin: i8,
            import_id: u64,
            version: u64,
            $($tz: String,)?
            $($dim: String,)*
            visitors: u64,
            pageviews: u64,
            events: u64,
            visits_ev: u64,
            visits_s: u64,
            s_n: u64,
            s_pv: u64,
            s_dur_ms: u64,
            s_eng_ms: u64,
            s_eng_n: u64,
            b_n: u64,
            bounces: u64,
            exits: u64,
            tp_ms: u64,
            tp_n: u64,
            sc_sum: u64,
            sc_n: u64,
        }

        impl $name {
            #[allow(unused_variables, unused_mut)]
            fn new(site_id: u64, import_id: u64, tz: &str, r: &ImportRow) -> Result<Self, QueryError> {
                let mut dims = r.dims.iter().cloned();
                let m = &r.m;
                Ok($name {
                    site_id,
                    day: rowbinary_date(r.day)?,
                    origin: 2,
                    import_id,
                    version: 1,
                    $($tz: tz.to_owned(),)?
                    $($dim: dims.next().unwrap_or_default(),)*
                    visitors: m.visitors,
                    pageviews: m.pageviews,
                    events: m.events,
                    visits_ev: m.visits_ev,
                    visits_s: m.visits_s,
                    s_n: m.s_n,
                    s_pv: m.s_pv,
                    s_dur_ms: m.s_dur_ms,
                    s_eng_ms: m.s_eng_ms,
                    s_eng_n: m.s_eng_n,
                    b_n: m.b_n,
                    bounces: m.bounces,
                    exits: m.exits,
                    tp_ms: m.tp_ms,
                    tp_n: m.tp_n,
                    sc_sum: m.sc_sum,
                    sc_n: m.sc_n,
                })
            }
        }
    };
}

import_row!(SiteImportRow with tz {});
import_row!(PagesImportRow { page });
import_row!(EntryPagesImportRow { entry_page });
import_row!(ExitPagesImportRow { exit_page });
import_row!(SourcesImportRow {
    channel,
    source,
    referrer,
    utm_source,
    utm_medium,
    utm_campaign,
    utm_content,
    utm_term
});
import_row!(LocationsImportRow {
    country,
    region,
    city
});
import_row!(DevicesImportRow { device });
import_row!(BrowsersImportRow {
    browser,
    browser_version
});
import_row!(OsImportRow { os, os_version });
import_row!(EventsImportRow { name });
import_row!(PropsImportRow { key, value });

fn rowbinary_date(d: Date) -> Result<u16, QueryError> {
    let days = Date::constant(1970, 1, 1)
        .until(d)
        .map_err(|e| QueryError::internal(anyhow::anyhow!("日期超出范围：{e}")))?
        .get_days();
    u16::try_from(days).map_err(|_| QueryError::internal(anyhow::anyhow!("日期超出范围：{d}")))
}

async fn write_rows<T>(client: &Client, table: &str, rows: Vec<T>) -> Result<(), QueryError>
where
    T: for<'a> clickhouse::Row<Value<'a> = T> + serde::Serialize + 'static,
{
    let mut insert = client
        .insert::<T>(table)
        .await
        .map_err(QueryError::internal)?
        .with_setting("async_insert", "0")
        .with_timeouts(Some(Duration::from_secs(60)), Some(Duration::from_secs(60)));
    for row in &rows {
        insert.write(row).await.map_err(QueryError::internal)?;
    }
    // Keep polling the driver's timeout if the import stops waiting for this response.
    tokio::spawn(insert.end())
        .await
        .map_err(QueryError::internal)?
        .map_err(QueryError::internal)
}

/// 把导入的行写进一张汇总表（`origin = 'import'`，`import_id`，`version = 1`，一次 RowBinary 写入）。
/// `table` 必须在 [`IMPORT_DIMS`] 里，每行的 `dims` 与该表的维度列一一对应；`tz` 是站点时区（写进 `rollup_site.tz`）。
pub async fn insert_import(
    client: &Client,
    scope: &TenantScope,
    table: &str,
    tz: &str,
    import_id: u64,
    rows: &[ImportRow],
) -> Result<(), QueryError> {
    let want = IMPORT_DIMS
        .iter()
        .find(|(t, _)| *t == table)
        .map(|(_, d)| d.len())
        .ok_or_else(|| QueryError::internal(anyhow::anyhow!("unknown rollup table {table}")))?;
    if let Some(r) = rows.iter().find(|r| r.dims.len() != want) {
        return Err(QueryError::internal(anyhow::anyhow!(
            "import row for {table} has {} dimensions, expected {want}",
            r.dims.len()
        )));
    }
    let site = scope.site_id();
    macro_rules! go {
        ($ty:ident) => {
            write_rows(
                client,
                table,
                rows.iter()
                    .map(|r| $ty::new(site, import_id, tz, r))
                    .collect::<Result<Vec<_>, _>>()?,
            )
            .await
        };
    }
    match table {
        "rollup_site" => go!(SiteImportRow),
        "rollup_pages" => go!(PagesImportRow),
        "rollup_entry_pages" => go!(EntryPagesImportRow),
        "rollup_exit_pages" => go!(ExitPagesImportRow),
        "rollup_sources" => go!(SourcesImportRow),
        "rollup_locations" => go!(LocationsImportRow),
        "rollup_devices" => go!(DevicesImportRow),
        "rollup_browsers" => go!(BrowsersImportRow),
        "rollup_os" => go!(OsImportRow),
        "rollup_events" => go!(EventsImportRow),
        _ => go!(PropsImportRow),
    }
}
/// 删除一次导入写入的全部汇总行（各表的 `origin = 'import' AND import_id = …`）。幂等。
pub async fn delete_import(
    client: &Client,
    scope: &TenantScope,
    import_id: u64,
) -> Result<(), QueryError> {
    let mut params = Params::default();
    params.push("site", ParamValue::U64(scope.site_id()));
    params.push("imp", ParamValue::U64(import_id));
    tokio::time::timeout(Duration::from_secs(120), async {
        for table in Family::TABLES {
            let sql = format!(
                "DELETE FROM {table} WHERE site_id = {{site:UInt64}} AND origin = 'import' AND import_id = {{imp:UInt64}}"
            );
            execute(client, &sql, &params).await?;
        }
        Ok(())
    })
    .await
    .map_err(QueryError::internal)?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_column_belongs_to_at_most_one_family() {
        let mut seen = std::collections::BTreeMap::new();
        for f in Family::ALL {
            for (c, _) in f.dims() {
                assert!(seen.insert(*c, f).is_none(), "{c:?} 在两个族里");
            }
        }
        // 每个族的表名不同
        let tables: std::collections::BTreeSet<_> = Family::ALL.iter().map(|f| f.table()).collect();
        assert_eq!(tables.len(), Family::ALL.len());
    }

    #[test]
    fn measure_columns_cover_every_rollup_measure() {
        use Measure::*;
        for m in [
            Visitors, Pageviews, Events, Visits, SessN, SessPv, SessDurMs, SessEngMs, SessEngN,
            BounceN, Bounces, Exits, TpMs, TpN, ScSum, ScN,
        ] {
            let _ = measure_column(m, VisitsForm::Sessions);
        }
        assert_eq!(measure_column(Visits, VisitsForm::Events), "visits_ev");
    }
}
