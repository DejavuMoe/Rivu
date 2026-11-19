//! SQL 构建器：`(String, Params)`（`system-design.md` §6.3、§6.8）。
//!
//! # 安全约束
//!
//! - **只拼服务端常量片段**：列名、函数名、操作符都来自枚举和本文件里的字面量；
//!   请求里的任何取值（过滤的值、模式、站点、时间）一律走 `{name:Type}` 服务端参数（[`Params`]），
//!   不用客户端 `?` 绑定，也不转义后拼接（核实报告 #10、#13）。
//! - **租户隔离靠类型**：生成 `FROM events` / `FROM sessions` 的函数只有 [`scoped_events`] 和 [`scoped_sessions`]，
//!   都要 `&TenantScope`，并把 `site_id = {site:UInt64}` 紧跟在 `FROM` 后面；子查询（`IN (…)`、`JOIN`）也一样，
//!   所以每个 `FROM` 都重复注入站点。[`check_tenant_scoped`] 再对最终文本做一遍扫描，作为纵深防御。
//! - SQL 文本里没有 `?`（`clickhouse` crate 会把它当绑定占位符）。
//!
//! # 结构
//!
//! 每个查询由若干**组件**（[`crate::plan`]）`UNION ALL` 而成，所有组件输出同一组列：
//! 维度 `d0..dk`（字符串）与度量 `c_*`（每个度量只在一个组件里非零）。
//! 外层按维度聚合（`sum` 各度量）、用分子分母算比率、排序、分页：
//!
//! ```text
//! SELECT d0, …, <指标表达式> AS m0, … [, count() OVER () AS total_rows]
//! FROM ( 事件组件 UNION ALL 会话组件 UNION ALL … )
//! GROUP BY d0, …  ORDER BY …  LIMIT {limit:UInt64} OFFSET {offset:UInt64}
//! ```

use std::collections::BTreeSet;

use crate::plan::{Measure, Plan, VisitsForm};
use crate::resolve::{
    Col, GoalKind, Gran, Interval, Leaf, MAX_PROP_KEYS, OrderKey, Pred, RDim, ResolvedQuery, Test,
    wildcard_to_like,
};
use crate::rollup::{Family, MEASURE_COLUMNS, Split, measure_column};
use crate::spec::{Direction, Metric};

/// 租户范围：每个 `FROM` 都必须带它。构造它的是鉴权之后的调用方。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TenantScope {
    site_id: u64,
}

impl TenantScope {
    pub fn new(site_id: u64) -> Self {
        TenantScope { site_id }
    }

    pub fn site_id(&self) -> u64 {
        self.site_id
    }
}

/// 一个服务端参数的值。
#[derive(Clone, Debug, PartialEq)]
pub enum ParamValue {
    U8(u8),
    U64(u64),
    I64(i64),
    F64(f64),
    Str(String),
    /// `YYYY-MM-DD`。
    Date(String),
    StrArray(Vec<String>),
    U32Array(Vec<u32>),
    U64Array(Vec<u64>),
}

impl ParamValue {
    /// ClickHouse 类型名（`{name:Type}` 里的 `Type`）。
    pub fn clickhouse_type(&self) -> &'static str {
        match self {
            ParamValue::U8(_) => "UInt8",
            ParamValue::U64(_) => "UInt64",
            ParamValue::I64(_) => "Int64",
            ParamValue::F64(_) => "Float64",
            ParamValue::Str(_) => "String",
            ParamValue::Date(_) => "Date",
            ParamValue::StrArray(_) => "Array(String)",
            ParamValue::U32Array(_) => "Array(UInt32)",
            ParamValue::U64Array(_) => "Array(UInt64)",
        }
    }
}

/// 一条 SQL 的全部参数，按声明顺序。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Params(Vec<(String, ParamValue)>);

impl Params {
    pub fn iter(&self) -> impl Iterator<Item = (&str, &ParamValue)> {
        self.0.iter().map(|(k, v)| (k.as_str(), v))
    }

    pub fn get(&self, name: &str) -> Option<&ParamValue> {
        self.0.iter().find(|(k, _)| k == name).map(|(_, v)| v)
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub(crate) fn push(&mut self, name: &str, v: ParamValue) {
        self.0.push((name.to_owned(), v));
    }
}

/// 对比查询用：主查询结果里的维度值约束对比期，保证两边行集一致（§6.3）。
/// `dims` 是 `ResolvedQuery.dims` 的下标（不含时间维度），`rows` 是对应的原始维度值（空串不换成展示文案）。
#[derive(Clone, Debug)]
pub(crate) struct Constraint {
    pub(crate) dims: Vec<usize>,
    pub(crate) rows: Vec<Vec<String>>,
}

/// 外层的排序方式。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OrderMode {
    /// 请求里的 `order_by`（已补默认值）。
    Query,
    /// 对比期的时间序列：时间升序，再按其余维度升序。
    TimeThenDims,
}

pub(crate) struct MainSqlOptions<'a> {
    /// 原始层组件的区间。有 `split` 时只含汇总层之后的天（可能为空：整个范围都由汇总层回答）。
    pub(crate) interval: Interval,
    /// 汇总层负责的天与族（§6.4 选层）。
    pub(crate) split: Option<Split>,
    pub(crate) completed_import_ids: &'a [u64],
    pub(crate) constraint: Option<&'a Constraint>,
    /// `percentage` 的分母。
    pub(crate) total_visitors: Option<u64>,
    pub(crate) limit: u64,
    pub(crate) offset: u64,
    pub(crate) order: OrderMode,
    pub(crate) with_total_rows: bool,
}

// ------------------------------------------------------------------------------------------
// 带租户范围的 FROM：全部 `FROM events` / `FROM sessions` 都从这里出
// ------------------------------------------------------------------------------------------

/// `FROM events WHERE site_id = …`。参数 `site` 的值是 `scope.site_id()`。
fn scoped_events(_scope: &TenantScope) -> &'static str {
    "FROM events WHERE site_id = {site:UInt64}"
}

fn scoped_sessions(_scope: &TenantScope) -> &'static str {
    "FROM sessions WHERE site_id = {site:UInt64}"
}

/// An accepted INSERT can land after recovery. PostgreSQL completion controls visibility.
fn completed_rollups(params: &mut Params, ids: &[u64]) -> &'static str {
    if ids.is_empty() {
        return "0";
    }
    params.push("import_ids", ParamValue::U64Array(ids.to_vec()));
    "origin = 'import' AND import_id IN {import_ids:Array(UInt64)}"
}

/// 扫描最终的 SQL 文本：每个 `FROM events` / `FROM sessions` 后面必须紧跟 `WHERE site_id = {site:UInt64}`，
/// 且文本里没有 `?`。
pub(crate) fn check_tenant_scoped(sql: &str) -> bool {
    if sql.contains('?') {
        return false;
    }
    let mut tables: Vec<&str> = vec!["events", "sessions", "ingest_counters"];
    tables.extend(Family::TABLES);
    for table in tables {
        let needle = format!("FROM {table}");
        let mut rest = sql;
        while let Some(i) = rest.find(&needle) {
            let mut after = &rest[i + needle.len()..];
            // 命中的是更长表名的前缀（`FROM events_x`）：不是这张表
            if after.starts_with(|c: char| c.is_ascii_alphanumeric() || c == '_') {
                rest = after;
                continue;
            }
            after = after.strip_prefix(" FINAL").unwrap_or(after);
            // 租户条件紧跟在表名（与 FINAL）之后
            if !after.starts_with(" WHERE site_id = {site:UInt64}") {
                return false;
            }
            rest = after;
        }
    }
    true
}

// ------------------------------------------------------------------------------------------
// 列表达式
// ------------------------------------------------------------------------------------------

/// `events` 上的列表达式（用于过滤与取值）。`country_code` 是 `FixedString(2)`，`toString` 去掉尾部 NUL，
/// 未知（`\0\0`）得到空串。入口页与退出页不在 `events` 上，由会话子查询提供。属性是 `props[键]`，键是参数 `pk<i>`
/// （没有这个键时取到空串，展示为 `(none)`）。
fn ev_expr(col: Col) -> &'static str {
    const PROPS: [&str; MAX_PROP_KEYS] = [
        "props[{pk0:String}]",
        "props[{pk1:String}]",
        "props[{pk2:String}]",
        "props[{pk3:String}]",
        "props[{pk4:String}]",
        "props[{pk5:String}]",
        "props[{pk6:String}]",
        "props[{pk7:String}]",
    ];
    match col {
        Col::EventName => "name",
        Col::EventPage => "pathname",
        Col::EventHostname => "hostname",
        // 不是列：目标叶子由 `Builder::goal_cond` 生成条件，目标维度由 `Builder::goal_dim_expr` 生成取值，不会用到这里
        Col::EventGoal => "name",
        Col::Source => "referrer_source",
        Col::Channel => "channel",
        Col::Referrer => "referrer",
        Col::UtmSource => "utm_source",
        Col::UtmMedium => "utm_medium",
        Col::UtmCampaign => "utm_campaign",
        Col::UtmContent => "utm_content",
        Col::UtmTerm => "utm_term",
        Col::Country => "toString(country_code)",
        Col::Region => "region_code",
        Col::City => "city_geoname_id",
        Col::Device => "device",
        Col::Browser => "browser",
        Col::BrowserVersion => "browser_version",
        Col::Os => "os",
        Col::OsVersion => "os_version",
        Col::EntryPage => "s_entry_path",
        Col::ExitPage => "s_exit_path",
        Col::Prop(i) => PROPS[usize::from(i)],
    }
}

/// 会话子查询（合并后的 `sessions`）里各字段的别名与定义。别名刻意与表列名不同（`a_*`、`s_*`），
/// 避免 `any(x) AS x` 造成聚合函数嵌套歧义。
const SESSION_FIELDS: [(&str, &str); 22] = [
    ("s_pv", "sum(pageviews)"),
    ("s_inter", "sum(interactive_events)"),
    ("s_eng", "sum(engagement_ms)"),
    ("a_source", "any(referrer_source)"),
    ("a_channel", "any(channel)"),
    ("a_referrer", "any(referrer)"),
    ("a_utm_source", "any(utm_source)"),
    ("a_utm_medium", "any(utm_medium)"),
    ("a_utm_campaign", "any(utm_campaign)"),
    ("a_utm_content", "any(utm_content)"),
    ("a_utm_term", "any(utm_term)"),
    ("a_country", "toString(any(country_code))"),
    ("a_region", "any(region_code)"),
    ("a_city", "any(city_geoname_id)"),
    ("a_device", "any(device)"),
    ("a_browser", "any(browser)"),
    ("a_browser_version", "any(browser_version)"),
    ("a_os", "any(os)"),
    ("a_os_version", "any(os_version)"),
    ("s_entry_host", "tupleElement(argMinIfMerge(entry_page), 1)"),
    ("s_entry_path", "tupleElement(argMinIfMerge(entry_page), 2)"),
    ("s_exit_path", "tupleElement(argMaxIfMerge(exit_page), 2)"),
];

/// 会话子查询里对应的别名。事件级的列没有会话别名。
fn sess_alias(col: Col) -> Option<&'static str> {
    Some(match col {
        Col::Source => "a_source",
        Col::Channel => "a_channel",
        Col::Referrer => "a_referrer",
        Col::UtmSource => "a_utm_source",
        Col::UtmMedium => "a_utm_medium",
        Col::UtmCampaign => "a_utm_campaign",
        Col::UtmContent => "a_utm_content",
        Col::UtmTerm => "a_utm_term",
        Col::Country => "a_country",
        Col::Region => "a_region",
        Col::City => "a_city",
        Col::Device => "a_device",
        Col::Browser => "a_browser",
        Col::BrowserVersion => "a_browser_version",
        Col::Os => "a_os",
        Col::OsVersion => "a_os_version",
        Col::EntryPage => "s_entry_path",
        Col::ExitPage => "s_exit_path",
        Col::EventName | Col::EventPage | Col::EventHostname | Col::EventGoal | Col::Prop(_) => {
            return None;
        }
    })
}

type Needs = BTreeSet<&'static str>;

/// 过滤树的求值上下文。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ctx {
    /// 事件行（`events` 的子查询）。
    Events,
    /// 会话行（合并后的 `sessions`）。`entry`：`event:page` / `event:hostname` 叶子按入口页判定（`bounce_rate`，§6.6）；
    /// 否则它们是「会话在范围内有匹配的事件」。
    Sess { entry: bool },
    /// 汇总表的行：叶子直接作用在族的维度列上（兼容判断保证叶子都在族里）。
    Rollup(Family),
}

/// 组件里一个维度的取值方式。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DimMode {
    Events,
    /// 会话组件：`event:page` / `event:hostname` 换成入口页。
    SessEntry,
    /// 退出组件：`event:page` 换成退出页。
    SessExit,
    /// 转化分母组件：同 `Events`，但 `event:goal` 展开成全部目标标签（每个目标行都拿到同样的分母）。
    ConvBase,
}

struct Builder<'a> {
    scope: &'a TenantScope,
    q: &'a ResolvedQuery,
    plan: &'a Plan,
    params: Params,
    n: usize,
    constraint: Option<&'a Constraint>,
}

fn utc_date(ms: i64) -> String {
    jiff::Timestamp::from_millisecond(ms)
        .map(|t| t.to_zoned(jiff::tz::TimeZone::UTC).date().to_string())
        .unwrap_or_else(|_| "1970-01-01".to_owned())
}

fn utc_date_minus_days(ms: i64, days: i64) -> String {
    let d = jiff::Timestamp::from_millisecond(ms)
        .map(|t| t.to_zoned(jiff::tz::TimeZone::UTC).date())
        .unwrap_or(jiff::civil::Date::constant(1970, 1, 1));
    let min = jiff::civil::Date::constant(1970, 1, 1);
    d.checked_sub(jiff::Span::new().days(days))
        .map(|d| d.max(min))
        .unwrap_or(min)
        .to_string()
}

impl<'a> Builder<'a> {
    fn new(
        scope: &'a TenantScope,
        q: &'a ResolvedQuery,
        plan: &'a Plan,
        iv: Interval,
        constraint: Option<&'a Constraint>,
    ) -> Self {
        let mut params = Params::default();
        params.push("site", ParamValue::U64(scope.site_id()));
        params.push("from_ms", ParamValue::I64(iv.start_ms));
        params.push("to_ms", ParamValue::I64(iv.end_ms));
        // 排序键含 `toDate(ts)`：先按 UTC 日期裁剪，再按毫秒精确过滤。空区间也给出合法的日期。
        let last_ms = if iv.end_ms > iv.start_ms {
            iv.end_ms - 1
        } else {
            iv.start_ms
        };
        params.push("from_date", ParamValue::Date(utc_date(iv.start_ms)));
        params.push("to_date", ParamValue::Date(utc_date(last_ms)));
        // 会话最长 7 天（§4.6、§10 Q7）：区间内活跃的会话开始于区间起点的 7 天之前以内。
        params.push(
            "s_from",
            ParamValue::Date(utc_date_minus_days(
                iv.start_ms,
                crate::rollup::SESSION_LOOKBACK_DAYS,
            )),
        );
        params.push("tz", ParamValue::Str(q.tz_name.clone()));
        params.push(
            "wk",
            ParamValue::U8(u8::try_from(q.week_start.to_monday_one_offset()).unwrap_or(1)),
        );
        for (i, k) in q.prop_keys.iter().enumerate() {
            params.push(&format!("pk{i}"), ParamValue::Str(k.clone()));
        }
        Builder {
            scope,
            q,
            plan,
            params,
            n: 0,
            constraint,
        }
    }

    /// 登记一个取值参数，返回 SQL 里的占位符。
    fn p(&mut self, v: ParamValue) -> String {
        let name = format!("p{}", self.n);
        self.n += 1;
        let ph = format!("{{{name}:{}}}", v.clickhouse_type());
        self.params.push(&name, v);
        ph
    }

    // ---- 带租户范围的扫描 ----

    /// `SELECT <select> FROM events WHERE site_id = … AND <时间范围> [AND <extra>]`。
    fn events_scan(&self, select: &str, extra: Option<&str>) -> String {
        let mut s = format!(
            "SELECT {select} {} AND toDate(ts) >= {{from_date:Date}} AND toDate(ts) <= {{to_date:Date}} \
             AND ts >= fromUnixTimestamp64Milli({{from_ms:Int64}}, 'UTC') \
             AND ts < fromUnixTimestamp64Milli({{to_ms:Int64}}, 'UTC')",
            scoped_events(self.scope)
        );
        if let Some(e) = extra {
            s.push_str(" AND ");
            s.push_str(e);
        }
        s
    }

    /// 按会话聚合的子查询：合并 `AggregatingMergeTree` 里尚未合并的部分行（内层按完整排序键
    /// `(site_id, start_date, session_id)` 分组，Q9）。`active`：只保留「区间内活跃」的会话
    /// （`last_event_at ≥ 起点 AND started_at < 终点`）。
    fn sessions_rel(&self, needs: &Needs, active: bool) -> String {
        let mut cols = String::from("session_id");
        if active {
            cols.push_str(", min(started_at) AS s_start, max(last_event_at) AS s_last");
        }
        for (alias, def) in SESSION_FIELDS {
            if needs.contains(alias) {
                cols.push_str(&format!(", {def} AS {alias}"));
            }
        }
        let mut s = format!(
            "SELECT {cols} {} AND start_date >= {{s_from:Date}} AND start_date <= {{to_date:Date}} \
             GROUP BY site_id, start_date, session_id",
            scoped_sessions(self.scope)
        );
        if active {
            s.push_str(
                " HAVING s_last >= fromUnixTimestamp64Milli({from_ms:Int64}, 'UTC') \
                 AND s_start < fromUnixTimestamp64Milli({to_ms:Int64}, 'UTC')",
            );
        }
        s
    }

    // ---- 过滤 ----

    fn pred(&mut self, p: &Pred, ctx: Ctx, needs: &mut Needs) -> String {
        match p {
            Pred::And(c) => format!(
                "({})",
                c.iter()
                    .map(|x| self.pred(x, ctx, needs))
                    .collect::<Vec<_>>()
                    .join(" AND ")
            ),
            Pred::Or(c) => format!(
                "({})",
                c.iter()
                    .map(|x| self.pred(x, ctx, needs))
                    .collect::<Vec<_>>()
                    .join(" OR ")
            ),
            Pred::Not(x) => format!("NOT ({})", self.pred(x, ctx, needs)),
            Pred::Leaf(l) => self.leaf(l, ctx, needs),
        }
    }

    fn leaf(&mut self, l: &Leaf, ctx: Ctx, needs: &mut Needs) -> String {
        match ctx {
            Ctx::Events => match l.col {
                Col::EntryPage | Col::ExitPage => {
                    // 入口页 / 退出页只在 sessions：`session_id IN (按会话聚合的子查询 WHERE 条件)`
                    let mut inner = Needs::new();
                    inner.insert(ev_expr(l.col));
                    let cond = self.cond(ev_expr(l.col), l);
                    let rel = self.sessions_rel(&inner, false);
                    format!("session_id IN (SELECT session_id FROM ({rel}) WHERE {cond})")
                }
                col => self.cond(ev_expr(col), l),
            },
            Ctx::Rollup(f) => {
                let column = f.column(l.col).expect("兼容判断保证过滤只落在族的维度上");
                // 城市存的是 GeoNames id 的十进制文本；过滤取值是 id
                let e = if l.col == Col::City {
                    "toUInt32OrZero(city)".to_owned()
                } else {
                    column.to_owned()
                };
                self.cond(&e, l)
            }
            Ctx::Sess { entry } => match l.col {
                Col::EventName
                | Col::EventPage
                | Col::EventHostname
                | Col::EventGoal
                | Col::Prop(_) => {
                    if entry && l.col == Col::EventPage {
                        needs.insert("s_entry_path");
                        return self.cond("s_entry_path", l);
                    }
                    if entry && l.col == Col::EventHostname {
                        needs.insert("s_entry_host");
                        return self.cond("s_entry_host", l);
                    }
                    // 「会话在范围内有匹配的事件」：对非 engagement 事件求叶子条件（存在语义）
                    let cond = self.cond(ev_expr(l.col), l);
                    let scan = self.events_scan(
                        "session_id",
                        Some(&format!("name != 'engagement' AND {cond}")),
                    );
                    format!("session_id IN ({scan})")
                }
                col => {
                    let alias = sess_alias(col).expect("会话级的列都有别名");
                    needs.insert(alias);
                    self.cond(alias, l)
                }
            },
        }
    }

    /// 叶子对表达式 `e` 的条件。
    fn cond(&mut self, e: &str, l: &Leaf) -> String {
        match &l.test {
            Test::In {
                values,
                case_sensitive,
                negated,
            } => {
                let ph = self.p(ParamValue::StrArray(values.clone()));
                let c = if *case_sensitive {
                    format!("{e} IN {ph}")
                } else {
                    format!("lower({e}) IN {ph}")
                };
                wrap_neg(c, *negated)
            }
            Test::Contains {
                needles,
                case_sensitive,
                negated,
            } => {
                let ph = self.p(ParamValue::StrArray(needles.clone()));
                let f = if *case_sensitive {
                    "multiSearchAny"
                } else {
                    "multiSearchAnyCaseInsensitive"
                };
                wrap_neg(format!("{f}({e}, {ph})"), *negated)
            }
            Test::Regex { patterns, negated } => {
                let ph = self.p(ParamValue::StrArray(patterns.clone()));
                wrap_neg(format!("multiMatchAny({e}, {ph})"), *negated)
            }
            Test::CityIn { ids, negated } => {
                let ph = self.p(ParamValue::U32Array(ids.clone()));
                wrap_neg(format!("{e} IN {ph}"), *negated)
            }
            Test::Goal { goals, negated } => {
                let c = self.goal_cond(goals);
                wrap_neg(c, *negated)
            }
        }
    }

    /// 事件满足任一目标的完成条件（在 `events` 的列上求值）：事件目标 `name = …`；
    /// 页面目标 `name = 'pageview' AND pathname LIKE …`（通配转成 `LIKE`，区分大小写、整串匹配）。没有目标时恒假。
    fn goal_cond(&mut self, goals: &[GoalKind]) -> String {
        if goals.is_empty() {
            return "0".to_owned();
        }
        let parts: Vec<String> = goals
            .iter()
            .map(|g| match g {
                GoalKind::Event(name) => {
                    let ph = self.p(ParamValue::Str(name.clone()));
                    format!("name = {ph}")
                }
                GoalKind::Page(pattern) => {
                    let ph = self.p(ParamValue::Str(wildcard_to_like(pattern)));
                    format!("(name = 'pageview' AND pathname LIKE {ph})")
                }
            })
            .collect();
        format!("({})", parts.join(" OR "))
    }

    /// `event:goal` 维度在事件组件里的取值：事件匹配的目标标签（去重），`arrayJoin` 展开成一行一个；
    /// 不匹配任何目标的事件展开成零行，不进结果。
    fn goal_dim_expr(&mut self) -> String {
        let goals = self.q.goals.clone();
        if goals.is_empty() {
            return "arrayJoin(emptyArrayString())".to_owned();
        }
        let items: Vec<String> = goals
            .iter()
            .map(|g| {
                let c = self.goal_cond(std::slice::from_ref(&g.kind));
                let label = self.p(ParamValue::Str(g.label.clone()));
                format!("if({c}, {label}, '')")
            })
            .collect();
        format!(
            "arrayJoin(arrayDistinct(arrayFilter(g_l -> g_l != '', [{}])))",
            items.join(", ")
        )
    }

    /// 转化分母组件里的 `event:goal` 取值：全部目标标签（去重），每个事件都展开成每个标签一行。
    fn goal_labels_expr(&mut self) -> String {
        let labels: BTreeSet<String> = self.q.goals.iter().map(|g| g.label.clone()).collect();
        if labels.is_empty() {
            return "arrayJoin(emptyArrayString())".to_owned();
        }
        let ph = self.p(ParamValue::StrArray(labels.into_iter().collect()));
        format!("arrayJoin({ph})")
    }

    // ---- 维度 ----

    /// 时间桶表达式。`base` 是时刻表达式（事件的 `ts`，或会话的「范围内首次活跃」时刻）。
    fn time_expr(gran: Gran, base: &str) -> String {
        let local = format!("toTimeZone({base}, {{tz:String}})");
        match gran {
            Gran::Minute => {
                format!("toString(toUnixTimestamp(toStartOfInterval({local}, INTERVAL 1 MINUTE)))")
            }
            Gran::Hour => {
                format!("toString(toUnixTimestamp(toStartOfInterval({local}, INTERVAL 1 HOUR)))")
            }
            Gran::Day => format!("toString(toDate({local}))"),
            Gran::Week => format!(
                "toString(subtractDays(toDate({local}), modulo(toDayOfWeek(toDate({local})) - {{wk:UInt8}} + 7, 7)))"
            ),
            Gran::Month => format!("toString(toStartOfMonth({local}))"),
        }
    }

    fn dim_expr(&mut self, d: RDim, mode: DimMode, needs: &mut Needs) -> String {
        match d {
            RDim::Time(g) => match mode {
                DimMode::Events | DimMode::ConvBase => Self::time_expr(g, "ts"),
                DimMode::SessEntry | DimMode::SessExit => Self::time_expr(
                    g,
                    "greatest(s_start, fromUnixTimestamp64Milli({from_ms:Int64}, 'UTC'))",
                ),
            },
            RDim::Col(Col::EventGoal) => match mode {
                DimMode::ConvBase => self.goal_labels_expr(),
                _ => self.goal_dim_expr(),
            },
            RDim::Col(col) => match mode {
                DimMode::Events | DimMode::ConvBase => match col {
                    Col::City => {
                        "if(city_geoname_id = 0, '', toString(city_geoname_id))".to_owned()
                    }
                    Col::EntryPage | Col::ExitPage => {
                        needs.insert(ev_expr(col));
                        ev_expr(col).to_owned()
                    }
                    c => format!("toString({})", ev_expr(c)),
                },
                DimMode::SessEntry | DimMode::SessExit => {
                    let alias = match col {
                        Col::EventPage if mode == DimMode::SessEntry => "s_entry_path",
                        Col::EventPage => "s_exit_path",
                        Col::EventHostname => "s_entry_host",
                        c => sess_alias(c).expect("会话组件不会有 event:name 维度"),
                    };
                    needs.insert(alias);
                    if col == Col::City {
                        "if(a_city = 0, '', toString(a_city))".to_owned()
                    } else {
                        format!("toString({alias})")
                    }
                }
            },
        }
    }

    /// `d0 AS d0`-风格的维度列表，以及 `GROUP BY` 的列名。
    fn dim_select(&mut self, mode: DimMode, needs: &mut Needs) -> String {
        let q = self.q;
        q.dims
            .iter()
            .enumerate()
            .map(|(i, d)| format!("{} AS d{i}", self.dim_expr(*d, mode, needs)))
            .collect::<Vec<_>>()
            .join(", ")
    }

    fn group_by(&self) -> String {
        if self.q.dims.is_empty() {
            String::new()
        } else {
            format!(
                " GROUP BY {}",
                (0..self.q.dims.len())
                    .map(|i| format!("d{i}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        }
    }

    /// 对比期的维度约束（用组件自己的 `d{i}` 别名）。
    fn constraint_sql(&mut self) -> Option<String> {
        let c = self.constraint?;
        if c.dims.is_empty() {
            return None;
        }
        let mut arrays = Vec::new();
        for k in 0..c.dims.len() {
            let col: Vec<String> = c.rows.iter().map(|r| r[k].clone()).collect();
            arrays.push(self.p(ParamValue::StrArray(col)));
        }
        let names = c.dims.iter().map(|i| format!("d{i}")).collect::<Vec<_>>();
        if names.len() == 1 {
            return Some(format!("{} IN {}", names[0], arrays[0]));
        }
        let zipped = format!("arrayZip({})", arrays.join(", "));
        let elems = (1..=names.len())
            .map(|k| format!("t.{k}"))
            .collect::<Vec<_>>()
            .join(", ");
        Some(format!(
            "({}) IN (SELECT {elems} FROM (SELECT arrayJoin({zipped}) AS t))",
            names.join(", ")
        ))
    }

    // ---- 度量列 ----

    /// 一个组件的度量列：`own` 给出它负责的度量的表达式，其余填 0。
    fn measure_cols(&self, own: &[(Measure, &str)]) -> String {
        let mut out = String::new();
        for m in &self.plan.measures {
            let expr = own
                .iter()
                .find(|(x, _)| x == m)
                .map(|(_, e)| *e)
                .unwrap_or("toUInt64(0)");
            if !out.is_empty() || !self.q.dims.is_empty() {
                out.push_str(", ");
            }
            out.push_str(&format!("{expr} AS {}", m.col()));
        }
        out
    }

    // ---- 组件 ----

    fn tree_events(&mut self, needs: &mut Needs) -> Option<String> {
        let f = self.q.filter.clone()?;
        Some(self.pred(&f, Ctx::Events, needs))
    }

    fn join_clause(&self, needs: &Needs) -> String {
        let join_needs: Needs = needs
            .iter()
            .copied()
            .filter(|a| matches!(*a, "s_entry_path" | "s_exit_path"))
            .collect();
        if join_needs.is_empty() {
            return String::new();
        }
        let rel = self.sessions_rel(&join_needs, false);
        format!(" ANY LEFT JOIN ({rel}) AS sx USING (session_id)")
    }

    fn where_clause(parts: Vec<String>) -> String {
        if parts.is_empty() {
            String::new()
        } else {
            format!(" WHERE {}", parts.join(" AND "))
        }
    }

    /// 事件组件：`visitors`、`pageviews`、`events`，以及事件形式的 `visits`。
    fn comp_events(&mut self) -> String {
        let mut needs = Needs::new();
        let tree = self.tree_events(&mut needs);
        let mut extra = String::from("name != 'engagement'");
        if let Some(t) = tree {
            extra.push_str(" AND ");
            extra.push_str(&t);
        }
        let scan = self.events_scan("*", Some(&extra));
        let dims = self.dim_select(DimMode::Events, &mut needs);
        let join = self.join_clause(&needs);
        let mut own: Vec<(Measure, &str)> = vec![
            (Measure::Visitors, "uniq(visitor_id)"),
            (Measure::Pageviews, "countIf(name = 'pageview')"),
            (Measure::Events, "count()"),
        ];
        if self.plan.visits_form == VisitsForm::Events {
            own.push((Measure::Visits, "uniq(session_id)"));
        }
        let measures = self.measure_cols(&own);
        let cons = self.constraint_sql();
        let where_ = Self::where_clause(cons.into_iter().collect());
        format!(
            "SELECT {dims}{measures} FROM ({scan}) AS e{join}{where_}{}",
            self.group_by()
        )
    }

    /// 转化分母组件：`conversion_rate` 的分母（§6.5）。与事件组件相同的扫描，但过滤去掉了 `event:goal` 叶子，
    /// `event:goal` 维度展开成全部目标标签；只给出 `c_cr_base = uniq(visitor_id)`。
    fn comp_conv_base(&mut self) -> String {
        let mut needs = Needs::new();
        let mut extra = String::from("name != 'engagement'");
        if let Some(t) = self.q.filter.as_ref().and_then(Pred::without_goal_leaves) {
            extra.push_str(" AND ");
            extra.push_str(&self.pred(&t, Ctx::Events, &mut needs));
        }
        let scan = self.events_scan("*", Some(&extra));
        let dims = self.dim_select(DimMode::ConvBase, &mut needs);
        let join = self.join_clause(&needs);
        let measures = self.measure_cols(&[(Measure::CrBase, "uniq(visitor_id)")]);
        let cons = self.constraint_sql();
        let where_ = Self::where_clause(cons.into_iter().collect());
        format!(
            "SELECT {dims}{measures} FROM ({scan}) AS e{join}{where_}{}",
            self.group_by()
        )
    }

    /// engagement 组件：`time_on_page`、`scroll_depth`。内层按（维度, 会话）聚合。
    fn comp_engagement(&mut self) -> String {
        let mut needs = Needs::new();
        let tree = self.tree_events(&mut needs);
        let mut extra = String::from("name = 'engagement'");
        if let Some(t) = tree {
            extra.push_str(" AND ");
            extra.push_str(&t);
        }
        let scan = self.events_scan("*", Some(&extra));
        let dims = self.dim_select(DimMode::Events, &mut needs);
        let join = self.join_clause(&needs);
        let cons = self.constraint_sql();
        let where_ = Self::where_clause(cons.into_iter().collect());
        let n = self.q.dims.len();
        let dim_cols = (0..n).map(|i| format!("d{i}")).collect::<Vec<_>>();
        let inner_group = dim_cols
            .iter()
            .cloned()
            .chain(std::iter::once("session_id".to_owned()))
            .collect::<Vec<_>>()
            .join(", ");
        let inner_dims = if dims.is_empty() {
            String::new()
        } else {
            format!("{dims}, ")
        };
        let inner = format!(
            "SELECT {inner_dims}session_id, sum(engagement_ms) AS e_ms, \
             maxIf(scroll_depth, scroll_depth <= 100) AS e_sd, countIf(scroll_depth <= 100) AS e_sdc \
             FROM ({scan}) AS e{join}{where_} GROUP BY {inner_group}"
        );
        let own: Vec<(Measure, &str)> = vec![
            (Measure::TpMs, "sum(e_ms)"),
            (Measure::TpN, "count()"),
            (Measure::ScSum, "sumIf(e_sd, e_sdc > 0)"),
            (Measure::ScN, "countIf(e_sdc > 0)"),
        ];
        let measures = self.measure_cols(&own);
        let outer_dims = dim_cols.join(", ");
        let outer_dims = if outer_dims.is_empty() {
            String::new()
        } else {
            outer_dims
        };
        format!(
            "SELECT {outer_dims}{measures} FROM ({inner}){}",
            self.group_by()
        )
    }

    /// 会话组件的公共部分。`kind`：主会话组件 / 跳出口径的独立组件 / 退出组件。
    fn comp_sessions(&mut self, kind: SessKind) -> String {
        let mut needs = Needs::new();
        let mode = if kind == SessKind::Exits {
            DimMode::SessExit
        } else {
            DimMode::SessEntry
        };
        let mut conds: Vec<String> = Vec::new();
        if let Some(f) = self.q.filter.clone() {
            conds.push(self.pred(&f, Ctx::Sess { entry: false }, &mut needs));
            if kind == SessKind::BounceSeparate {
                conds.push(self.pred(&f, Ctx::Sess { entry: true }, &mut needs));
            }
        }
        if kind == SessKind::Exits {
            needs.insert("s_pv");
            conds.push("s_pv > 0".to_owned());
        }
        let dims = self.dim_select(mode, &mut needs);
        if let Some(c) = self.constraint_sql() {
            conds.push(c);
        }
        let own: Vec<(Measure, &str)> = match kind {
            SessKind::Main => {
                needs.extend(["s_pv", "s_inter", "s_eng"]);
                let mut v = vec![
                    (Measure::SessN, "count()"),
                    (Measure::SessPv, "sum(s_pv)"),
                    (
                        Measure::SessDurMs,
                        "sum(toUInt64(toUnixTimestamp64Milli(s_last) - toUnixTimestamp64Milli(s_start)))",
                    ),
                    (Measure::SessEngMs, "sumIf(s_eng, s_eng > 0)"),
                    (Measure::SessEngN, "countIf(s_eng > 0)"),
                ];
                // 跳出口径与会话口径相同时由这个组件一并给出；不同时（过滤里有 event:page / event:hostname）
                // 由独立的组件给出，这里不能再给，否则会重复计数
                if !self.plan.bounce_separate {
                    v.push((Measure::BounceN, "count()"));
                    v.push((Measure::Bounces, "countIf(s_pv <= 1 AND s_inter = 0)"));
                }
                if self.plan.visits_form == VisitsForm::Sessions {
                    v.push((Measure::Visits, "count()"));
                }
                v
            }
            SessKind::BounceSeparate => {
                needs.extend(["s_pv", "s_inter"]);
                vec![
                    (Measure::BounceN, "count()"),
                    (Measure::Bounces, "countIf(s_pv <= 1 AND s_inter = 0)"),
                ]
            }
            SessKind::Exits => vec![(Measure::Exits, "count()")],
        };
        let measures = self.measure_cols(&own);
        let rel = self.sessions_rel(&needs, true);
        let where_ = Self::where_clause(conds);
        format!(
            "SELECT {dims}{measures} FROM ({rel}){where_}{}",
            self.group_by()
        )
    }

    // ---- 汇总层 ----

    /// 汇总层组件：`[from, rollup_to]` 这些本地日的行，按查询的维度再相加。输出与原始层组件相同的列。
    /// `FINAL` 之内只放键列上的条件（#9），过滤与对比约束放在外面。
    fn comp_rollup(&mut self, split: &Split, completed_import_ids: &[u64]) -> String {
        let f = split.family;
        let r_from = self.p(ParamValue::Date(split.from.to_string()));
        let r_to = self.p(ParamValue::Date(split.rollup_to.to_string()));
        // 属性族：只取这个键的行（`key` 是排序键里的列，可以放在 FINAL 之内）
        let key = match f {
            Family::Props(i) => format!(" AND key = {{pk{i}:String}}"),
            _ => String::new(),
        };
        let imports = completed_rollups(&mut self.params, completed_import_ids);
        let scan = format!(
            "SELECT * FROM {} FINAL WHERE site_id = {{site:UInt64}} AND day >= {r_from} AND day <= {r_to}{key} \
             AND (origin = 'native' OR ({imports}))",
            f.table()
        );
        let q = self.q;
        let dims = q
            .dims
            .iter()
            .enumerate()
            .map(|(i, d)| {
                let e = match d {
                    RDim::Time(g) => Self::rollup_time_expr(*g),
                    RDim::Col(c) => f.column(*c).expect("兼容判断保证维度都在族里").to_owned(),
                };
                format!("{e} AS d{i}")
            })
            .collect::<Vec<_>>()
            .join(", ");
        let form = self.plan.visits_form;
        let own: Vec<(Measure, String)> = self
            .plan
            .measures
            .iter()
            .map(|m| (*m, format!("sum({})", measure_column(*m, form))))
            .collect();
        let own_ref: Vec<(Measure, &str)> = own.iter().map(|(m, e)| (*m, e.as_str())).collect();
        let measures = self.measure_cols(&own_ref);
        // 汇总行带着全部度量；原始层只在规划用到的组件有数据时才出行（例如只要 time_on_page 时，
        // 没有 engagement 事件的页面不出现），所以只取这些组件有数据的行
        let mut present = Vec::new();
        if self.plan.events {
            present.push("events > 0");
        }
        if self.plan.sessions {
            present.push("s_n > 0");
        }
        if self.plan.exits {
            present.push("exits > 0");
        }
        if self.plan.engagement {
            present.push("tp_n > 0");
        }
        // 导入的行不套这条：导出里有「有访客、浏览为 0」的行（只有自定义事件的访问），它们也要算
        present.push("origin = 'import'");
        let mut conds = vec![format!("({})", present.join(" OR "))];
        if let Some(t) = self.q.filter.clone() {
            let mut needs = Needs::new();
            conds.push(self.pred(&t, Ctx::Rollup(f), &mut needs));
        }
        if let Some(c) = self.constraint_sql() {
            conds.push(c);
        }
        let where_ = Self::where_clause(conds);
        format!(
            "SELECT {dims}{measures} FROM ({scan}) AS r{where_}{}",
            self.group_by()
        )
    }

    /// 汇总层的时间桶（只有天、周、月）：与 [`Self::time_expr`] 的文本格式相同。
    fn rollup_time_expr(g: Gran) -> String {
        match g {
            Gran::Day => "toString(day)".to_owned(),
            Gran::Week => {
                "toString(subtractDays(day, modulo(toDayOfWeek(day) - {wk:UInt8} + 7, 7)))"
                    .to_owned()
            }
            Gran::Month => "toString(toStartOfMonth(day))".to_owned(),
            Gran::Minute | Gran::Hour => unreachable!("兼容判断已排除比天更细的粒度"),
        }
    }

    /// 原始层的组件（规划里要的那些）。
    fn raw_components(&mut self) -> Vec<String> {
        let mut comps: Vec<String> = Vec::new();
        if self.plan.events {
            comps.push(self.comp_events());
        }
        if self.plan.sessions {
            comps.push(self.comp_sessions(SessKind::Main));
        }
        if self.plan.bounce_separate {
            comps.push(self.comp_sessions(SessKind::BounceSeparate));
        }
        if self.plan.exits {
            comps.push(self.comp_sessions(SessKind::Exits));
        }
        if self.plan.engagement {
            comps.push(self.comp_engagement());
        }
        if self.plan.conv_base {
            comps.push(self.comp_conv_base());
        }
        comps
    }

    /// 构建一天一族的汇总行：`INSERT INTO rollup_x … SELECT` 原始层组件按族的维度聚合后的度量和。
    fn rollup_build_sql(&mut self, family: Family, day: &str, version: u64) -> String {
        let comps = self.raw_components().join(" UNION ALL ");
        let day_ph = self.p(ParamValue::Date(day.to_owned()));
        let version_ph = self.p(ParamValue::U64(version));
        let mut cols: Vec<String> = ["site_id", "day", "origin", "import_id", "version"]
            .iter()
            .map(|s| (*s).to_owned())
            .collect();
        let mut vals: Vec<String> = vec![
            "{site:UInt64}".to_owned(),
            day_ph,
            "'native'".to_owned(),
            "0".to_owned(),
            version_ph,
        ];
        if family == Family::Site {
            cols.push("tz".to_owned());
            vals.push("{tz:String}".to_owned());
        }
        for (i, (_, name)) in family.dims().iter().enumerate() {
            cols.push((*name).to_owned());
            vals.push(format!("d{i}"));
        }
        let form = self.plan.visits_form;
        let produced = |m: Measure| self.plan.measures.contains(&m);
        for (name, m) in MEASURE_COLUMNS {
            cols.push(name.to_owned());
            // visits 按口径分两列：只填与这一族口径相同的那一列
            let fill = produced(m) && (m != Measure::Visits || form == VisitsForm::Sessions);
            vals.push(if fill {
                format!("sum({})", m.col())
            } else {
                "toUInt64(0)".to_owned()
            });
        }
        cols.push("visits_ev".to_owned());
        vals.push(if produced(Measure::Visits) && form == VisitsForm::Events {
            format!("sum({})", Measure::Visits.col())
        } else {
            "toUInt64(0)".to_owned()
        });
        format!(
            "INSERT INTO {} ({}) SELECT {} FROM ({comps}){}",
            family.table(),
            cols.join(", "),
            vals.join(", "),
            self.group_by()
        )
    }
    // ---- 外层 ----

    fn metric_expr(&self, m: Metric) -> String {
        let s = |x: Measure| format!("sum({})", x.col());
        match m {
            Metric::Visitors => s(Measure::Visitors),
            Metric::Pageviews => s(Measure::Pageviews),
            Metric::Events => s(Measure::Events),
            Metric::Visits => s(Measure::Visits),
            Metric::ViewsPerVisit => format!(
                "ifNotFinite({} / {}, 0)",
                s(Measure::SessPv),
                s(Measure::SessN)
            ),
            Metric::BounceRate => format!(
                "ifNotFinite({} / {}, 0)",
                s(Measure::Bounces),
                s(Measure::BounceN)
            ),
            Metric::VisitDuration => format!(
                "ifNotFinite({} / {} / 1000, 0)",
                s(Measure::SessDurMs),
                s(Measure::SessN)
            ),
            Metric::EngagedDuration => format!(
                "ifNotFinite({} / 1000 / {}, 0)",
                s(Measure::SessEngMs),
                s(Measure::SessEngN)
            ),
            Metric::Percentage => format!(
                "ifNotFinite({} / {{total:Float64}}, 0)",
                s(Measure::Visitors)
            ),
            Metric::ExitRate => format!(
                "ifNotFinite({} / {}, 0)",
                s(Measure::Exits),
                s(Measure::Pageviews)
            ),
            Metric::TimeOnPage => format!(
                "ifNotFinite({} / 1000 / {}, 0)",
                s(Measure::TpMs),
                s(Measure::TpN)
            ),
            Metric::ScrollDepth => format!(
                "ifNotFinite({} / {}, 0)",
                s(Measure::ScSum),
                s(Measure::ScN)
            ),
            Metric::ConversionRate => format!(
                "ifNotFinite({} / {}, 0)",
                s(Measure::Visitors),
                s(Measure::CrBase)
            ),
            Metric::GroupConversionRate => {
                unreachable!("不支持的指标已在 resolve 阶段被拒绝")
            }
        }
    }

    fn order_by(&self, mode: OrderMode) -> String {
        let n = self.q.dims.len();
        let mut parts: Vec<String> = Vec::new();
        let mut used_dims: BTreeSet<usize> = BTreeSet::new();
        match mode {
            OrderMode::Query => {
                for (key, dir) in &self.q.order {
                    let d = if *dir == Direction::Asc {
                        "ASC"
                    } else {
                        "DESC"
                    };
                    match key {
                        OrderKey::Metric(i) => parts.push(format!("m{i} {d}")),
                        OrderKey::Dim(j) => {
                            used_dims.insert(*j);
                            parts.push(format!("d{j} {d}"));
                        }
                    }
                }
            }
            OrderMode::TimeThenDims => {
                if let Some((j, _)) = self.q.time_dim() {
                    used_dims.insert(j);
                    parts.push(format!("d{j} ASC"));
                }
            }
        }
        // 其余维度按原始值升序：保证结果（含分页）完全确定
        for j in 0..n {
            if !used_dims.contains(&j) {
                parts.push(format!("d{j} ASC"));
            }
        }
        if parts.is_empty() {
            String::new()
        } else {
            format!(" ORDER BY {}", parts.join(", "))
        }
    }

    fn main_sql(&mut self, o: &MainSqlOptions<'_>) -> String {
        // 有汇总层时，原始层只回答汇总层之后的天；整个范围都在汇总层里时没有原始层组件
        let mut comps: Vec<String> = if o.split.is_some() && o.interval.is_empty() {
            Vec::new()
        } else {
            self.raw_components()
        };
        if let Some(split) = &o.split {
            comps.push(self.comp_rollup(split, o.completed_import_ids));
        }
        // 转化率：分母组件会给出没有转化的维度值（例如每个目标），有维度时只保留有转化访客的行
        let having = if self.plan.conv_base && !self.q.dims.is_empty() {
            format!(" HAVING sum({}) > 0", Measure::Visitors.col())
        } else {
            String::new()
        };
        let n = self.q.dims.len();
        let mut select: Vec<String> = (0..n).map(|i| format!("d{i}")).collect();
        for (i, m) in self.q.metrics.iter().enumerate() {
            select.push(format!("{} AS m{i}", self.metric_expr(*m)));
        }
        if o.with_total_rows {
            select.push("count() OVER () AS total_rows".to_owned());
        }
        let from = comps.join(" UNION ALL ");
        let limit = self.p(ParamValue::U64(o.limit));
        let offset = self.p(ParamValue::U64(o.offset));
        format!(
            "SELECT {} FROM ({from}){}{having}{} LIMIT {limit} OFFSET {offset}",
            select.join(", "),
            self.group_by(),
            self.order_by(o.order),
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SessKind {
    Main,
    BounceSeparate,
    Exits,
}

fn wrap_neg(c: String, negated: bool) -> String {
    if negated { format!("NOT ({c})") } else { c }
}

/// 主查询的 SQL 与参数。
pub(crate) fn main_query(
    scope: &TenantScope,
    q: &ResolvedQuery,
    plan: &Plan,
    o: &MainSqlOptions<'_>,
) -> (String, Params) {
    let mut b = Builder::new(scope, q, plan, o.interval, o.constraint);
    if let Some(t) = o.total_visitors {
        b.params.push("total", ParamValue::F64(t as f64));
    }
    let sql = b.main_sql(o);
    (sql, b.params)
}

/// 一天一族的汇总行的构建语句（`INSERT … SELECT`）。`q` 是那一天的整日查询，维度是族的维度；`plan` 是构建用的规划。
pub(crate) fn rollup_build_query(
    scope: &TenantScope,
    q: &ResolvedQuery,
    plan: &Plan,
    family: Family,
    day: &str,
    version: u64,
) -> (String, Params) {
    let mut b = Builder::new(scope, q, plan, q.interval, None);
    let sql = b.rollup_build_sql(family, day, version);
    (sql, b.params)
}

/// 选层的探测（§6.4）：已构建到的最后一天、时区不同的行数（站点改过时区，重建之前不用汇总层）、
/// `[from, to]` 内已构建的天里的事件数（估算原始层要扫描的行数）。
pub(crate) fn rollup_probe_query(
    scope: &TenantScope,
    tz: &str,
    from: &str,
    to: &str,
    report: Option<(jiff::civil::Date, &[u64])>,
) -> (String, Params) {
    let mut params = Params::default();
    params.push("site", ParamValue::U64(scope.site_id()));
    params.push("tz", ParamValue::Str(tz.to_owned()));
    params.push("from_day", ParamValue::Date(from.to_owned()));
    params.push("to_day", ParamValue::Date(to.to_owned()));
    // 构建任务只看原生行。查询时，未结束的原生日不能覆盖原始数据，导入日仍按导入时的时区归日。
    let (origin, marker_tz) = match report {
        None => ("origin = 'native'".to_owned(), ", argMin(tz, day)"),
        Some((today, ids)) => {
            params.push("today", ParamValue::Date(today.to_string()));
            let imports = completed_rollups(&mut params, ids);
            (
                format!("((origin = 'native' AND day < {{today:Date}}) OR ({imports}))"),
                "",
            )
        }
    };
    let sql = format!(
        "SELECT toString(maxOrNull(day)), countIf(origin = 'native' AND tz != {{tz:String}}), \
         sumIf(events, day >= {{from_day:Date}} AND day <= {{to_day:Date}}), toString(minOrNull(day)){marker_tz} \
         FROM rollup_site FINAL WHERE site_id = {{site:UInt64}} AND {origin}"
    );
    (sql, params)
}

/// 站点当前时区最早完成的原生汇总日（`rollup_site` 最后写入）。
pub(crate) fn first_native_rollup_day_query(
    scope: &TenantScope,
    tz: &str,
    today: jiff::civil::Date,
) -> (String, Params) {
    let mut params = Params::default();
    params.push("site", ParamValue::U64(scope.site_id()));
    params.push("tz", ParamValue::Str(tz.to_owned()));
    params.push("today", ParamValue::Date(today.to_string()));
    let sql = "SELECT toString(minOrNull(day)) FROM rollup_site FINAL \
               WHERE site_id = {site:UInt64} AND origin = 'native' AND tz = {tz:String} \
               AND day < {today:Date}"
        .to_owned();
    (sql, params)
}

/// `percentage` 的分母：同样的区间与过滤、无维度的访客总数。
pub(crate) fn total_visitors_query(
    scope: &TenantScope,
    q: &ResolvedQuery,
    plan: &Plan,
    iv: Interval,
) -> (String, Params) {
    let mut b = Builder::new(scope, q, plan, iv, None);
    let mut needs = Needs::new();
    let mut extra = String::from("name != 'engagement'");
    if let Some(t) = b.tree_events(&mut needs) {
        extra.push_str(" AND ");
        extra.push_str(&t);
    }
    let sql = b.events_scan("uniq(visitor_id)", Some(&extra));
    (sql, b.params)
}

/// 区间里（同样的过滤）非 engagement 事件上出现过的属性键与事件数，事件多的在前，至多 `limit` 个。
pub(crate) fn property_keys_query(
    scope: &TenantScope,
    q: &ResolvedQuery,
    plan: &Plan,
    iv: Interval,
    limit: u64,
) -> (String, Params) {
    let mut b = Builder::new(scope, q, plan, iv, None);
    let mut needs = Needs::new();
    let mut extra = String::from("name != 'engagement'");
    if let Some(t) = b.tree_events(&mut needs) {
        extra.push_str(" AND ");
        extra.push_str(&t);
    }
    let scan = b.events_scan("arrayJoin(mapKeys(props)) AS k", Some(&extra));
    let limit = b.p(ParamValue::U64(limit));
    let sql = format!(
        "SELECT k, count() AS c FROM ({scan}) GROUP BY k ORDER BY c DESC, k ASC LIMIT {limit}"
    );
    (sql, b.params)
}

/// 导入的属性键（`rollup_props`，只有导入的行）：`[from, to]` 是站点本地日。
pub(crate) fn imported_property_keys_query(
    scope: &TenantScope,
    from: &str,
    to: &str,
    limit: u64,
    completed_import_ids: &[u64],
) -> (String, Params) {
    let mut params = Params::default();
    params.push("site", ParamValue::U64(scope.site_id()));
    params.push("from_day", ParamValue::Date(from.to_owned()));
    params.push("to_day", ParamValue::Date(to.to_owned()));
    params.push("lim", ParamValue::U64(limit));
    let imports = completed_rollups(&mut params, completed_import_ids);
    let sql = format!(
        "SELECT key, sum(events) AS c FROM rollup_props FINAL \
         WHERE site_id = {{site:UInt64}} AND {imports} \
         AND day >= {{from_day:Date}} AND day <= {{to_day:Date}} \
         GROUP BY key ORDER BY c DESC, key ASC LIMIT {{lim:UInt64}}"
    );
    (sql, params)
}

/// 站点最早事件的时间（`date_range = all`）：`(行数, 毫秒)`，没有数据时行数为 0。
/// 用排序键 `(site_id, toDate(ts), …)` 的前缀读最早的一天，再取这一天里最早的时刻。
pub(crate) fn first_event_query(scope: &TenantScope) -> (String, Params) {
    let mut params = Params::default();
    params.push("site", ParamValue::U64(scope.site_id()));
    let sql = format!(
        "SELECT count(), toUnixTimestamp64Milli(min(ts)) {} AND toDate(ts) = \
         (SELECT toDate(ts) {} ORDER BY toDate(ts) ASC LIMIT 1)",
        scoped_events(scope),
        scoped_events(scope),
    );
    (sql, params)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tenant_scan_detects_unscoped_from() {
        assert!(check_tenant_scoped(
            "SELECT 1 FROM events WHERE site_id = {site:UInt64} AND x IN (SELECT y FROM sessions WHERE site_id = {site:UInt64})"
        ));
        assert!(!check_tenant_scoped("SELECT 1 FROM events WHERE x = 1"));
        assert!(!check_tenant_scoped(
            "SELECT 1 FROM events WHERE site_id = {site:UInt64} AND a IN (SELECT 1 FROM sessions)"
        ));
        assert!(!check_tenant_scoped(
            "SELECT 1 FROM events WHERE site_id = {site:UInt64} AND a = ?"
        ));
        assert!(!check_tenant_scoped(
            "SELECT 1 FROM events WHERE site_id = 5"
        ));
    }

    #[test]
    fn param_types_match_clickhouse_names() {
        assert_eq!(
            ParamValue::StrArray(vec![]).clickhouse_type(),
            "Array(String)"
        );
        assert_eq!(
            ParamValue::U32Array(vec![]).clickhouse_type(),
            "Array(UInt32)"
        );
        assert_eq!(ParamValue::Date(String::new()).clickhouse_type(), "Date");
    }

    #[test]
    fn utc_dates_are_clamped_at_the_epoch() {
        assert_eq!(utc_date_minus_days(86_400_000 * 3, 7), "1970-01-01");
        assert_eq!(utc_date_minus_days(86_400_000 * 20, 7), "1970-01-14");
    }
}
