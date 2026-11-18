//! 执行：把规划好的 SQL 发给 ClickHouse，组装 [`QueryResult`]（`system-design.md` §6.3 的 exec 与 assemble）。
//!
//! - 用 `rivu_reader` 账号（`readonly=1`）：只设 `log_comment` 与 `max_execution_time`，这两项在 profile 里放开了修改；
//!   `max_memory_usage`、行数上限、正则长度上限由 profile 固定（`deploy/clickhouse/users.d/rivu-dev.xml`）。
//! - 超时按调用方的档位（看板 / API / 公开链接，§10 Q14），由 [`ExecOptions::timeout_secs`] 传入；
//!   一次请求可能发多条查询（总量、主查询、对比），它们共用同一个截止时间。
//! - 结果用 `JSONCompactEachRow` 取回：维度与指标的个数随请求而变，没法用静态的 `Row` 类型。
//! - 错误分类：超时与资源限制（503 / 429）、数据库拒绝了正则（校验错误）、其余是内部错误。

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use clickhouse::Client;
use jiff::Timestamp;
use serde_json::{Value, json};

use crate::error::QueryError;
use crate::plan::{self, Plan, Tier};
use crate::resolve::SiteContext;
use crate::resolve::{Gran, Interval, IntervalKind, RDim, ResolvedQuery, day_start_ms, fmt_local};
use crate::result::{
    Comparison, ComparisonRow, EchoedQuery, Meta, MetricValue, QueryResult, ResolvedRange,
    ResultRow, Warning,
};
use crate::rollup::{self, Split};
use crate::spec::{DateRange, Metric, QuerySpec};
use crate::sql::{
    self, Constraint, MainSqlOptions, OrderMode, ParamValue, Params, TenantScope,
    check_tenant_scoped,
};

/// 对比期时间序列的行数上限（也受 `rivu_reader` 的 `max_result_rows` 限制）。
const COMPARISON_ROW_CAP: u64 = 1_000_000;

/// 执行选项。
#[derive(Clone, Debug)]
pub struct ExecOptions {
    /// 调用方档位的执行时限（秒），不得大于 `rivu_reader` 配额里的上限。
    pub timeout_secs: u64,
    /// 「现在」。相对日期范围、封顶与实时窗口都按它换算；测试注入固定值。
    pub now: Timestamp,
    /// `log_comment` 里的路由名，如 `api.query`、`dashboard.overview`。
    pub route: String,
    /// 额外写进 `log_comment` 的字段（只放 id 一类的标识，如 `org`、`api_key_id`；不放邮箱、IP、过滤值）。
    /// 与保留字段（`route`、`site`、`part`、`tier`）同名时以保留字段为准。
    pub comment: BTreeMap<String, Value>,
    /// 日汇总层（§6.4 选层）：区间里已构建的天的事件数（`rollup_site.events`，估算原始层要扫描的行数）不少于这个值、
    /// 且查询兼容时，已构建的天由汇总层回答。`None`：不用汇总层（默认；差分测试与参考实现比较的是原始层）。
    pub rollup_min_rows: Option<u64>,
}

impl ExecOptions {
    pub fn new(timeout_secs: u64) -> Self {
        ExecOptions {
            timeout_secs,
            now: Timestamp::now(),
            route: "query".to_owned(),
            comment: BTreeMap::new(),
            rollup_min_rows: None,
        }
    }

    /// 打开日汇总层（见 [`ExecOptions::rollup_min_rows`]）；`0` 表示只要兼容、有已构建的天就用。
    pub fn with_rollup(mut self, min_rows: u64) -> Self {
        self.rollup_min_rows = Some(min_rows);
        self
    }

    /// 给 `log_comment` 加一个字段（见 [`ExecOptions::comment`]）。
    pub fn with_comment(mut self, key: impl Into<String>, value: impl Into<Value>) -> Self {
        self.comment.insert(key.into(), value.into());
        self
    }

    pub fn with_now(mut self, now: Timestamp) -> Self {
        self.now = now;
        self
    }

    pub fn with_route(mut self, route: impl Into<String>) -> Self {
        self.route = route.into();
        self
    }
}

struct Deadline {
    start: Instant,
    total: Duration,
}

impl Deadline {
    fn new(secs: u64) -> Self {
        Deadline {
            start: Instant::now(),
            total: Duration::from_secs(secs.max(1)),
        }
    }

    /// 剩余的整秒数（向上取整，至少 1）；已过期返回 `Timeout`。
    fn remaining_secs(&self) -> Result<u64, QueryError> {
        let left = self
            .total
            .checked_sub(self.start.elapsed())
            .filter(|d| !d.is_zero())
            .ok_or(QueryError::Timeout)?;
        Ok(left.as_secs() + u64::from(left.subsec_nanos() > 0))
    }
}

struct Ctx<'a> {
    reader: &'a Client,
    scope: &'a TenantScope,
    opts: &'a ExecOptions,
    deadline: Deadline,
    /// 实际用到的层（`meta.tiers`）。
    tiers: std::sync::Mutex<BTreeSet<Tier>>,
    /// 原始数据里最早的完整本地日（保留期，见 [`SiteContext::retention_days`]）；`None` 不限。
    raw_since: Option<jiff::civil::Date>,
    /// 已完成的导入覆盖的本地日期（[`SiteContext::imported`]）。
    imported: Option<(jiff::civil::Date, jiff::civil::Date)>,
}

/// 每条查询的 `log_comment`（JSON，只含 id 与固定标签）：调用方的额外字段在前，保留字段 `route`、`site`、`part`、`tier` 覆盖同名项。
fn log_comment(opts: &ExecOptions, site_id: u64, part: &str, tier: &str) -> String {
    let mut comment: serde_json::Map<String, Value> = opts
        .comment
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    comment.insert("route".into(), json!(opts.route));
    comment.insert("site".into(), json!(site_id));
    comment.insert("part".into(), json!(part));
    comment.insert("tier".into(), json!(tier));
    Value::Object(comment).to_string()
}

fn ch_code(text: &str) -> Option<u32> {
    let rest = text.split("Code: ").nth(1)?;
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

fn classify(err: clickhouse::error::Error, part: &str) -> QueryError {
    use clickhouse::error::Error;
    match &err {
        Error::TimedOut => QueryError::Timeout,
        Error::BadResponse(text) => {
            let lower = text.to_ascii_lowercase();
            match ch_code(text) {
                // TIMEOUT_EXCEEDED、SOCKET_TIMEOUT、TOO_SLOW
                Some(159 | 209 | 160) => QueryError::Timeout,
                // MEMORY_LIMIT_EXCEEDED、TOO_MANY_SIMULTANEOUS_QUERIES、TOO_MANY_ROWS、TOO_MANY_BYTES、
                // TOO_MANY_ROWS_OR_BYTES、QUOTA_EXCEEDED
                Some(241 | 202 | 158 | 307 | 396 | 201) => {
                    QueryError::ResourceLimit(format!("the database rejected the query ({part})"))
                }
                // CANNOT_COMPILE_REGEXP，或 hyperscan 的长度 / 复杂度限制
                Some(427) => QueryError::validation(
                    "filters",
                    "a regular expression was rejected by the database (unsupported syntax or too complex)",
                ),
                Some(36) if lower.contains("regexp") || lower.contains("hyperscan") => {
                    QueryError::validation(
                        "filters",
                        "a regular expression was rejected by the database (too long or too complex)",
                    )
                }
                _ => QueryError::internal(anyhow::anyhow!("ClickHouse error in {part}: {text}")),
            }
        }
        _ => QueryError::internal(anyhow::anyhow!(
            "ClickHouse request failed in {part}: {err}"
        )),
    }
}

pub(crate) fn bind(mut q: clickhouse::query::Query, params: &Params) -> clickhouse::query::Query {
    for (name, v) in params.iter() {
        q = match v {
            ParamValue::U8(x) => q.param(name, *x),
            ParamValue::U64(x) => q.param(name, *x),
            ParamValue::I64(x) => q.param(name, *x),
            ParamValue::F64(x) => q.param(name, *x),
            ParamValue::Str(s) | ParamValue::Date(s) => q.param(name, s.as_str()),
            ParamValue::StrArray(a) => q.param(name, a),
            ParamValue::U32Array(a) => q.param(name, a),
        };
    }
    q
}

impl Ctx<'_> {
    /// 执行一条 SQL，返回按行的 JSON 数组。
    async fn run(
        &self,
        part: &str,
        sql: &str,
        params: &Params,
    ) -> Result<Vec<Vec<Value>>, QueryError> {
        self.run_tier(part, sql, params, "raw").await
    }

    async fn run_tier(
        &self,
        part: &str,
        sql: &str,
        params: &Params,
        tier: &str,
    ) -> Result<Vec<Vec<Value>>, QueryError> {
        if !check_tenant_scoped(sql) {
            return Err(QueryError::internal(anyhow::anyhow!(
                "generated SQL is not tenant-scoped ({part})"
            )));
        }
        let secs = self.deadline.remaining_secs()?;
        let comment = log_comment(self.opts, self.scope.site_id(), part, tier);
        let q = bind(self.reader.query(sql), params)
            .with_setting("max_execution_time", secs.to_string())
            .with_setting("log_comment", comment);
        let mut cursor = q
            .fetch_bytes("JSONCompactEachRow")
            .map_err(|e| classify(e, part))?;
        let bytes = cursor.collect().await.map_err(|e| classify(e, part))?;
        let mut rows = Vec::new();
        for value in serde_json::Deserializer::from_slice(&bytes).into_iter::<Value>() {
            let value = value.map_err(|e| {
                QueryError::internal(anyhow::anyhow!(
                    "unparsable ClickHouse response ({part}): {e}"
                ))
            })?;
            match value {
                Value::Array(cells) => rows.push(cells),
                other => {
                    return Err(QueryError::internal(anyhow::anyhow!(
                        "unexpected ClickHouse row ({part}): {other}"
                    )));
                }
            }
        }
        Ok(rows)
    }
}

fn cell_u64(v: &Value) -> Option<u64> {
    match v {
        Value::Number(n) => n.as_u64(),
        Value::String(s) => s.parse().ok(),
        _ => None,
    }
}

fn cell_i64(v: &Value) -> Option<i64> {
    match v {
        Value::Number(n) => n.as_i64(),
        Value::String(s) => s.parse().ok(),
        _ => None,
    }
}

fn cell_f64(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.parse().ok(),
        Value::Null => Some(0.0),
        _ => None,
    }
}

fn is_count(m: Metric) -> bool {
    matches!(
        m,
        Metric::Visitors | Metric::Visits | Metric::Pageviews | Metric::Events
    )
}

fn zero(m: Metric) -> MetricValue {
    if is_count(m) {
        MetricValue::Int(0)
    } else {
        MetricValue::Float(0.0)
    }
}

/// 一行主结果：原始维度值（空串不换文案，对比约束要用）、指标、窗口函数给出的总行数。
#[derive(Clone, Debug)]
struct RawRow {
    raw: Vec<String>,
    metrics: Vec<MetricValue>,
    total_rows: Option<u64>,
}

fn parse_rows(
    q: &ResolvedQuery,
    rows: Vec<Vec<Value>>,
    with_total: bool,
) -> Result<Vec<RawRow>, QueryError> {
    let n = q.dims.len();
    let bad =
        |what: &str| QueryError::internal(anyhow::anyhow!("unexpected ClickHouse cell: {what}"));
    rows.into_iter()
        .map(|cells| {
            let want = n + q.metrics.len() + usize::from(with_total);
            if cells.len() != want {
                return Err(bad("column count"));
            }
            let raw = cells[..n]
                .iter()
                .map(|c| {
                    c.as_str()
                        .map(str::to_owned)
                        .ok_or_else(|| bad("dimension"))
                })
                .collect::<Result<Vec<_>, _>>()?;
            let metrics = q
                .metrics
                .iter()
                .enumerate()
                .map(|(i, m)| {
                    let c = &cells[n + i];
                    if is_count(*m) {
                        cell_u64(c)
                            .map(MetricValue::Int)
                            .ok_or_else(|| bad("count"))
                    } else {
                        cell_f64(c)
                            .map(MetricValue::Float)
                            .ok_or_else(|| bad("float"))
                    }
                })
                .collect::<Result<Vec<_>, _>>()?;
            let total_rows = if with_total {
                Some(cell_u64(&cells[n + q.metrics.len()]).ok_or_else(|| bad("total_rows"))?)
            } else {
                None
            };
            Ok(RawRow {
                raw,
                metrics,
                total_rows,
            })
        })
        .collect()
}

/// 原始维度值 → 展示文案。
fn labels(q: &ResolvedQuery, raw: &[String]) -> Vec<String> {
    q.dims
        .iter()
        .zip(raw)
        .map(|(d, v)| match d {
            RDim::Col(c) => {
                if v.is_empty() {
                    c.empty_label().to_owned()
                } else {
                    v.clone()
                }
            }
            RDim::Time(Gran::Minute | Gran::Hour) => v
                .parse::<i64>()
                .map(|secs| fmt_local(&q.tz, secs.saturating_mul(1000)))
                .unwrap_or_else(|_| v.clone()),
            RDim::Time(_) => v.clone(),
        })
        .collect()
}

fn change(m: Metric, new: f64, old: f64) -> f64 {
    if matches!(
        m,
        Metric::BounceRate | Metric::Percentage | Metric::ExitRate | Metric::ConversionRate
    ) {
        new - old
    } else if old == 0.0 {
        if new == 0.0 { 0.0 } else { 1.0 }
    } else {
        (new - old) / old
    }
}

fn resolved_range(q: &ResolvedQuery, iv: &Interval) -> ResolvedRange {
    ResolvedRange {
        start: fmt_local(&q.tz, iv.start_ms),
        end: fmt_local(&q.tz, iv.end_ms),
    }
}

/// 一个区间上的一次完整取数：先 `percentage` 的分母，再主查询。
/// 一次取数的参数：区间、对比约束、分页、排序、是否带总行数。
struct Fetch<'a> {
    iv: Interval,
    constraint: Option<&'a Constraint>,
    page: (u64, u64),
    order: OrderMode,
    with_total_rows: bool,
    part: &'a str,
}

async fn fetch_rows(
    cx: &Ctx<'_>,
    q: &ResolvedQuery,
    plan: &Plan,
    f: Fetch<'_>,
) -> Result<Vec<RawRow>, QueryError> {
    let Fetch {
        iv,
        constraint,
        page,
        order,
        with_total_rows,
        part,
    } = f;
    let total_visitors = if plan.needs_total {
        let (sql, params) = sql::total_visitors_query(cx.scope, q, plan, iv);
        let rows = cx.run(&format!("{part}.total"), &sql, &params).await?;
        let v = rows
            .first()
            .and_then(|r| r.first())
            .and_then(cell_u64)
            .ok_or_else(|| QueryError::internal(anyhow::anyhow!("missing total visitors")))?;
        Some(v)
    } else {
        None
    };
    let (raw_iv, split) = choose_tier(cx, q, plan, iv, part).await?;
    let opts = MainSqlOptions {
        interval: raw_iv,
        split,
        constraint,
        total_visitors,
        limit: page.0,
        offset: page.1,
        order,
        with_total_rows,
    };
    let (sql, params) = sql::main_query(cx.scope, q, plan, &opts);
    let tier = match split {
        None => "raw",
        Some(_) if raw_iv.is_empty() => "rollup",
        Some(_) => "rollup+raw",
    };
    if let Ok(mut t) = cx.tiers.lock() {
        if split.is_some() {
            t.insert(Tier::Rollup);
        }
        if split.is_none() || !raw_iv.is_empty() {
            t.insert(Tier::Raw);
        }
    }
    let rows = cx.run_tier(part, &sql, &params, tier).await?;
    parse_rows(q, rows, with_total_rows)
}

/// 选层（§6.4）：查询兼容、汇总层已构建到区间里、且这些天的事件数不少于阈值时，`[from, R]` 由汇总层回答
/// （R = 已构建的最后一天），原始层只回答 R 之后的天。返回原始层的区间与分层。
async fn choose_tier(
    cx: &Ctx<'_>,
    q: &ResolvedQuery,
    plan: &Plan,
    iv: Interval,
    part: &str,
) -> Result<(Interval, Option<Split>), QueryError> {
    let raw = Ok((iv, None));
    let Some(min_rows) = cx.opts.rollup_min_rows else {
        return raw;
    };
    let IntervalKind::Dates { from, to } = iv.kind else {
        return raw;
    };
    let Some(family) = rollup::compatible_family(q, plan) else {
        return raw;
    };
    // 导入的天只在汇总层里（导入写的是汇总行），与保留期之前的天一样：不看行数阈值
    let imported = cx.imported.filter(|(a, b)| *a <= to && *b >= from);
    // 属性族只有导入的数据：汇总层只回答导入的天，之后的天走原始层
    if let rollup::Family::Props(_) = family {
        let Some((_, imp_to)) = imported else {
            return raw;
        };
        return split_at(q, iv, family, from, imp_to.min(to));
    }
    let (sql, params) = sql::rollup_probe_query(
        cx.scope,
        &q.tz_name,
        &from.to_string(),
        &to.to_string(),
        false,
    );
    let rows = cx.run(&format!("{part}.tier"), &sql, &params).await?;
    let row = rows.first();
    let built: Option<jiff::civil::Date> = row
        .and_then(|r| r.first())
        .and_then(Value::as_str)
        .and_then(|s| s.parse().ok());
    let stale = row.and_then(|r| r.get(1)).and_then(cell_u64).unwrap_or(0);
    let rows_in_range = row.and_then(|r| r.get(2)).and_then(cell_u64).unwrap_or(0);
    let Some(built) = built else {
        return raw;
    };
    // 超出原始数据保留期的天只在汇总层里：不看行数阈值
    let beyond = cx.raw_since.is_some_and(|r| from < r) || imported.is_some();
    if stale > 0 || built < from || (rows_in_range < min_rows && !beyond) {
        return raw;
    }
    split_at(q, iv, family, from, built.min(to))
}

/// `[from, rollup_to]` 由汇总层回答，原始层只回答之后的天。
fn split_at(
    q: &ResolvedQuery,
    iv: Interval,
    family: rollup::Family,
    from: jiff::civil::Date,
    rollup_to: jiff::civil::Date,
) -> Result<(Interval, Option<Split>), QueryError> {
    let raw_start = match rollup_to.tomorrow() {
        Ok(d) => day_start_ms(&q.tz, d)?.min(iv.end_ms),
        Err(_) => iv.end_ms,
    };
    let raw_iv = Interval {
        start_ms: raw_start.max(iv.start_ms),
        end_ms: iv.end_ms,
        kind: iv.kind,
    };
    Ok((
        raw_iv,
        Some(Split {
            family,
            from,
            rollup_to,
        }),
    ))
}

/// 站点最早事件的时间（毫秒）；没有数据为 `None`。
async fn first_event_ms(cx: &Ctx<'_>) -> Result<Option<i64>, QueryError> {
    let (sql, params) = sql::first_event_query(cx.scope);
    let rows = cx.run("first_event", &sql, &params).await?;
    let row = rows
        .first()
        .ok_or_else(|| QueryError::internal(anyhow::anyhow!("empty first_event response")))?;
    let count = row.first().and_then(cell_u64).unwrap_or(0);
    if count == 0 {
        return Ok(None);
    }
    Ok(row.get(1).and_then(cell_i64))
}

/// `all` 可使用原生汇总时，取当前时区最早完成的本地日。
async fn first_native_rollup_ms(
    cx: &Ctx<'_>,
    site: &SiteContext,
    spec: &QuerySpec,
) -> Result<Option<i64>, QueryError> {
    if cx.opts.rollup_min_rows.is_none() {
        return Ok(None);
    }
    // 用三天让自动 `time` 以日粒度参与兼容判断。实际粒度和对比期由最终起点解析。
    let mut shape = spec.clone();
    shape.date_range = DateRange::Days(3);
    shape.include.compare = None;
    let Ok(q) = ResolvedQuery::build(&shape, site, cx.opts.now, None) else {
        return Ok(None);
    };
    match rollup::compatible_family(&q, &plan::plan(&q)) {
        None | Some(rollup::Family::Props(_)) => return Ok(None),
        Some(_) => {}
    }
    let (sql, params) = sql::first_native_rollup_day_query(cx.scope, &q.tz_name);
    let rows = cx
        .run_tier("first_native_rollup_day", &sql, &params, "rollup")
        .await?;
    rows.first()
        .and_then(|r| r.first())
        .and_then(Value::as_str)
        .and_then(|s| s.parse().ok())
        .map(|day| day_start_ms(&q.tz, day))
        .transpose()
}

/// 生成主查询的 SQL 与参数，不执行（快照测试与排障用）。`all` 需要的站点最早事件时间由 `data_start_ms` 传入；
/// `percentage` 的分母用占位值 0。
pub fn explain(
    scope: &TenantScope,
    site: &SiteContext,
    spec: &QuerySpec,
    now: Timestamp,
    data_start_ms: Option<i64>,
) -> Result<(String, Params), QueryError> {
    let q = ResolvedQuery::build(spec, site, now, data_start_ms)?;
    let plan = plan::plan(&q);
    let opts = MainSqlOptions {
        interval: q.interval,
        split: None,
        constraint: None,
        total_visitors: plan.needs_total.then_some(0),
        limit: u64::from(q.limit),
        offset: u64::from(q.offset),
        order: OrderMode::Query,
        with_total_rows: q.want_total_rows,
    };
    Ok(sql::main_query(scope, &q, &plan, &opts))
}

/// 原始数据里最早的完整本地日：今天 − (保留天数 − 1)。
fn raw_since(site: &SiteContext, now: Timestamp) -> Result<Option<jiff::civil::Date>, QueryError> {
    let Some(days) = site.retention_days else {
        return Ok(None);
    };
    let today = crate::resolve::local_date(&site.timezone, now.as_millisecond())?;
    Ok(today
        .checked_sub(jiff::Span::new().days(i64::from(days.max(1)) - 1))
        .ok())
}

/// 属性键列表里的一项（看板的属性卡片用）。
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct PropertyKey {
    pub key: String,
    /// 区间里带这个键的非 engagement 事件数。
    pub events: u64,
}

/// [`property_keys`] 至多返回的键数。
pub const MAX_LISTED_PROPERTY_KEYS: u64 = 100;

/// 区间里（同样的过滤）出现过的属性键，事件多的在前。`spec` 只用它的 `date_range` 与 `filters`，
/// 指标与维度照常校验（看板传 `{"metrics":["events"], …}`）。
pub async fn property_keys(
    reader: &Client,
    scope: &TenantScope,
    site: &SiteContext,
    spec: QuerySpec,
    opts: ExecOptions,
) -> Result<Vec<PropertyKey>, QueryError> {
    if scope.site_id() != site.site_id {
        return Err(QueryError::internal(anyhow::anyhow!(
            "tenant scope ({}) does not match the site context ({})",
            scope.site_id(),
            site.site_id
        )));
    }
    let cx = Ctx {
        reader,
        scope,
        opts: &opts,
        deadline: Deadline::new(opts.timeout_secs),
        tiers: Default::default(),
        raw_since: None,
        imported: None,
    };
    let data_start = if matches!(spec.date_range, DateRange::All) {
        let first = first_event_ms(&cx).await?;
        let imported = match site.imported {
            Some((from, _)) => Some(day_start_ms(&site.timezone, from)?),
            None => None,
        };
        first.into_iter().chain(imported).min()
    } else {
        None
    };
    let q = ResolvedQuery::build(&spec, site, opts.now, data_start)?;
    let plan = plan::plan(&q);
    let (sql, params) =
        sql::property_keys_query(scope, &q, &plan, q.interval, MAX_LISTED_PROPERTY_KEYS);
    let mut rows = cx.run("property_keys", &sql, &params).await?;
    // 导入的属性只在汇总层（`rollup_props`）：区间覆盖导入的天、又没有筛选时（汇总层不能按别的维度筛选）把它们的键也算上
    if let (Some((imp_from, imp_to)), &IntervalKind::Dates { from, to }, None) =
        (site.imported, &q.interval.kind, &q.filter)
        && from <= imp_to
        && to >= imp_from
    {
        let (sql, params) = sql::imported_property_keys_query(
            scope,
            &from.max(imp_from).to_string(),
            &to.min(imp_to).to_string(),
            MAX_LISTED_PROPERTY_KEYS,
        );
        rows.extend(cx.run("property_keys.imported", &sql, &params).await?);
    }
    let mut keys: Vec<PropertyKey> = Vec::new();
    for r in rows {
        let key = r.first().and_then(Value::as_str).map(str::to_owned);
        let events = r.get(1).and_then(cell_u64);
        let (Some(key), Some(events)) = (key, events) else {
            return Err(QueryError::internal(anyhow::anyhow!(
                "unexpected ClickHouse row (property_keys)"
            )));
        };
        match keys.iter_mut().find(|k| k.key == key) {
            Some(k) => k.events += events,
            None => keys.push(PropertyKey { key, events }),
        }
    }
    keys.sort_by(|a, b| b.events.cmp(&a.events).then_with(|| a.key.cmp(&b.key)));
    keys.truncate(MAX_LISTED_PROPERTY_KEYS as usize);
    Ok(keys)
}

/// 执行一条 `QuerySpec`。
///
/// `reader` 用 `rivu_reader` 账号构造并绑定库（`rivu_storage_ch::client`）；`scope` 与 `site.site_id` 必须一致；
/// `spec` 来自 [`QuerySpec::parse`]。解析之外的校验（组合、时间换算、正则限制）在这里完成。
pub async fn execute(
    reader: &Client,
    scope: &TenantScope,
    site: &SiteContext,
    spec: QuerySpec,
    opts: ExecOptions,
) -> Result<QueryResult, QueryError> {
    if scope.site_id() != site.site_id {
        return Err(QueryError::internal(anyhow::anyhow!(
            "tenant scope ({}) does not match the site context ({})",
            scope.site_id(),
            site.site_id
        )));
    }
    let cx = Ctx {
        reader,
        scope,
        opts: &opts,
        deadline: Deadline::new(opts.timeout_secs),
        tiers: Default::default(),
        raw_since: raw_since(site, opts.now)?,
        imported: site.imported,
    };
    // `all` 的起点取原始事件、已完成导入与可用原生汇总中最早的时间。
    let data_start = if matches!(spec.date_range, DateRange::All) {
        let first = first_event_ms(&cx).await?;
        let imported = match site.imported {
            Some((from, _)) => Some(day_start_ms(&site.timezone, from)?),
            None => None,
        };
        let native = first_native_rollup_ms(&cx, site, &spec).await?;
        first.into_iter().chain(imported).chain(native).min()
    } else {
        None
    };
    let q = ResolvedQuery::build(&spec, site, opts.now, data_start)?;
    let plan = plan::plan(&q);

    let page = (u64::from(q.limit), u64::from(q.offset));
    let main = fetch_rows(
        &cx,
        &q,
        &plan,
        Fetch {
            iv: q.interval,
            constraint: None,
            page,
            order: OrderMode::Query,
            with_total_rows: q.want_total_rows,
            part: "main",
        },
    )
    .await?;

    let total_rows = if q.want_total_rows {
        match main.first() {
            Some(r) => r.total_rows,
            None if q.offset == 0 => Some(0),
            None => {
                // 偏移越过了末尾：用偏移 0 的一行读总行数
                let probe = fetch_rows(
                    &cx,
                    &q,
                    &plan,
                    Fetch {
                        iv: q.interval,
                        constraint: None,
                        page: (1, 0),
                        order: OrderMode::Query,
                        with_total_rows: true,
                        part: "main.total_rows",
                    },
                )
                .await?;
                Some(probe.first().and_then(|r| r.total_rows).unwrap_or(0))
            }
        }
    } else {
        None
    };

    // ---- 对比期 ----
    let mut results: Vec<ResultRow> = main
        .iter()
        .map(|r| ResultRow {
            dimensions: labels(&q, &r.raw),
            metrics: r.metrics.clone(),
            comparison: None,
        })
        .collect();
    let mut comparison_results: Option<Vec<ComparisonRow>> = None;
    let mut comparison_labels: Option<Vec<String>> = None;
    if let Some(civ) = q.compare {
        if q.want_time_labels {
            comparison_labels = q.time_labels_for(&civ)?;
        }
        let time_idx = q.time_dim().map(|(i, _)| i);
        let keyed: Vec<usize> = (0..q.dims.len()).filter(|i| Some(*i) != time_idx).collect();
        if main.is_empty() {
            if time_idx.is_some() {
                comparison_results = Some(Vec::new());
            }
        } else {
            let constraint = if keyed.is_empty() {
                None
            } else {
                let tuples: BTreeSet<Vec<String>> = main
                    .iter()
                    .map(|r| keyed.iter().map(|i| r.raw[*i].clone()).collect())
                    .collect();
                Some(Constraint {
                    dims: keyed.clone(),
                    rows: tuples.into_iter().collect(),
                })
            };
            let (limit, order) = if time_idx.is_some() {
                (COMPARISON_ROW_CAP, OrderMode::TimeThenDims)
            } else {
                (main.len() as u64, OrderMode::Query)
            };
            let cmp = fetch_rows(
                &cx,
                &q,
                &plan,
                Fetch {
                    iv: civ,
                    constraint: constraint.as_ref(),
                    page: (limit, 0),
                    order,
                    with_total_rows: false,
                    part: "compare",
                },
            )
            .await?;
            if time_idx.is_some() {
                comparison_results = Some(
                    cmp.iter()
                        .map(|r| ComparisonRow {
                            dimensions: labels(&q, &r.raw),
                            metrics: r.metrics.clone(),
                        })
                        .collect(),
                );
            } else {
                let by_key: BTreeMap<&Vec<String>, &RawRow> =
                    cmp.iter().map(|r| (&r.raw, r)).collect();
                for (row, raw) in results.iter_mut().zip(&main) {
                    let old: Vec<MetricValue> = match by_key.get(&raw.raw) {
                        Some(c) => c.metrics.clone(),
                        None => q.metrics.iter().map(|m| zero(*m)).collect(),
                    };
                    let chg = q
                        .metrics
                        .iter()
                        .zip(row.metrics.iter().zip(&old))
                        .map(|(m, (n, o))| change(*m, n.as_f64(), o.as_f64()))
                        .collect();
                    row.comparison = Some(Comparison {
                        dimensions: row.dimensions.clone(),
                        metrics: old,
                        change: chg,
                    });
                }
            }
        }
    }

    // ---- meta ----
    let mut warnings = Vec::new();
    if site.geo_capability == Some(rivu_geo::GeoCapability::Country)
        && (q.dims.iter().any(|d| {
            matches!(
                d,
                RDim::Col(crate::resolve::Col::Region | crate::resolve::Col::City)
            )
        }) || q.filter.as_ref().is_some_and(|f| {
            f.has_leaf_on(crate::resolve::Col::Region) || f.has_leaf_on(crate::resolve::Col::City)
        }))
    {
        warnings.push(Warning {
            code: "geo_capability_country_only".to_owned(),
            message: "The loaded geolocation database only provides country-level data; region and city values are empty.".to_owned(),
        });
    }
    // 区间伸到保留期之前、而这次没有用汇总层：那些天没有数据（§6.4）
    if let Some(since) = cx.raw_since {
        let since_ms = day_start_ms(&q.tz, since)?;
        let used_rollup = cx.tiers.lock().is_ok_and(|t| t.contains(&Tier::Rollup));
        let starts_before = q.interval.start_ms < since_ms
            || q.compare.as_ref().is_some_and(|c| c.start_ms < since_ms);
        if starts_before && !used_rollup {
            warnings.push(Warning {
                code: "range_beyond_raw_retention".to_owned(),
                message: format!(
                    "Raw data is kept from {since}; earlier days of this range are only available from the daily rollup, which this query cannot use (see the rollup compatibility rules)."
                ),
            });
        }
    }
    // 区间覆盖导入的天、而这次没有用汇总层（查询与汇总层不兼容）：导入的数据没有算进来
    if let Some((imp_from, imp_to)) = site.imported {
        let used_rollup = cx.tiers.lock().is_ok_and(|t| t.contains(&Tier::Rollup));
        let overlaps = |iv: &Interval| match iv.kind {
            IntervalKind::Dates { from, to } => from <= imp_to && to >= imp_from,
            IntervalKind::Instants => {
                let a = day_start_ms(&q.tz, imp_from).unwrap_or(i64::MAX);
                let b = imp_to
                    .tomorrow()
                    .ok()
                    .and_then(|d| day_start_ms(&q.tz, d).ok())
                    .unwrap_or(i64::MIN);
                iv.start_ms < b && iv.end_ms > a
            }
        };
        if !used_rollup && (overlaps(&q.interval) || q.compare.as_ref().is_some_and(overlaps)) {
            warnings.push(Warning {
                code: "imported_data_excluded".to_owned(),
                message: format!(
                    "Imported data ({imp_from} to {imp_to}) is only kept as daily aggregates and is not included in this query (see the rollup compatibility rules)."
                ),
            });
        }
    }
    let time_labels = if q.want_time_labels {
        q.time_labels_for(&q.interval)?
    } else {
        None
    };
    let meta = Meta {
        time_labels,
        comparison_time_labels: comparison_labels,
        warnings,
        tiers: {
            let used = cx.tiers.lock().map(|t| t.clone()).unwrap_or_default();
            if used.is_empty() {
                vec![Tier::Raw]
            } else {
                used.into_iter().collect()
            }
        },
        metric_semantics: plan::metric_semantics(&q, &plan),
        geo_attribution: if q.geo_in_dims {
            site.geo_attribution.clone()
        } else {
            None
        },
        geo_capability: if q.geo_touched {
            site.geo_capability.map(rivu_geo::GeoCapability::as_str)
        } else {
            None
        },
        total_rows,
    };
    let query = EchoedQuery {
        site: q.echo.site.clone(),
        metrics: q.metrics.iter().map(|m| m.as_str().to_owned()).collect(),
        date_range: q.echo.date_range.clone(),
        resolved_date_range: resolved_range(&q, &q.interval),
        comparison_date_range: q.compare.as_ref().map(|iv| resolved_range(&q, iv)),
        time_zone: q.tz_name.clone(),
        dimensions: q.echo.dimensions.clone(),
        filters: q.echo.filters.clone(),
        order_by: q.echo.order_by.clone(),
        include: q.echo.include.clone(),
        pagination: q.echo.pagination.clone(),
    };
    Ok(QueryResult {
        results,
        comparison_results,
        meta,
        query,
    })
}

#[cfg(test)]
mod log_comment_tests {
    use super::*;

    #[test]
    fn log_comment_has_route_site_and_the_callers_ids() {
        let opts = ExecOptions::new(5)
            .with_route("api.query")
            .with_comment("org", 4)
            .with_comment("api_key_id", 9);
        let v: Value = serde_json::from_str(&log_comment(&opts, 77, "main", "raw")).unwrap();
        assert_eq!(
            v,
            json!({"route": "api.query", "site": 77, "part": "main", "tier": "raw",
                   "org": 4, "api_key_id": 9})
        );
    }

    #[test]
    fn reserved_fields_cannot_be_overridden_by_the_caller() {
        let opts = ExecOptions::new(5)
            .with_route("real")
            .with_comment("route", "forged")
            .with_comment("site", 1)
            .with_comment("tier", "forged");
        let v: Value = serde_json::from_str(&log_comment(&opts, 77, "compare", "raw")).unwrap();
        assert_eq!(v["route"], "real");
        assert_eq!(v["site"], 77);
        assert_eq!(v["part"], "compare");
        assert_eq!(v["tier"], "raw");
    }
}
