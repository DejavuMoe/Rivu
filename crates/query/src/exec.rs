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
use crate::plan::{self, Plan};
use crate::resolve::SiteContext;
use crate::resolve::{Gran, Interval, RDim, ResolvedQuery, fmt_local};
use crate::result::{
    Comparison, ComparisonRow, EchoedQuery, Meta, MetricValue, QueryResult, ResolvedRange,
    ResultRow, Warning,
};
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
}

impl ExecOptions {
    pub fn new(timeout_secs: u64) -> Self {
        ExecOptions {
            timeout_secs,
            now: Timestamp::now(),
            route: "query".to_owned(),
        }
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

fn bind(mut q: clickhouse::query::Query, params: &Params) -> clickhouse::query::Query {
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
        if !check_tenant_scoped(sql) {
            return Err(QueryError::internal(anyhow::anyhow!(
                "generated SQL is not tenant-scoped ({part})"
            )));
        }
        let secs = self.deadline.remaining_secs()?;
        let comment = json!({
            "route": self.opts.route,
            "site": self.scope.site_id(),
            "part": part,
            "tier": "raw",
        })
        .to_string();
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
            RDim::Time(Gran::Hour) => v
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
        Metric::BounceRate | Metric::Percentage | Metric::ExitRate
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
    let opts = MainSqlOptions {
        interval: iv,
        constraint,
        total_visitors,
        limit: page.0,
        offset: page.1,
        order,
        with_total_rows,
    };
    let (sql, params) = sql::main_query(cx.scope, q, plan, &opts);
    let rows = cx.run(part, &sql, &params).await?;
    parse_rows(q, rows, with_total_rows)
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
        constraint: None,
        total_visitors: plan.needs_total.then_some(0),
        limit: u64::from(q.limit),
        offset: u64::from(q.offset),
        order: OrderMode::Query,
        with_total_rows: q.want_total_rows,
    };
    Ok(sql::main_query(scope, &q, &plan, &opts))
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
    };
    // `all` 要先知道站点最早事件的时间
    let data_start = if matches!(spec.date_range, DateRange::All) {
        first_event_ms(&cx).await?
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
    let time_labels = if q.want_time_labels {
        q.time_labels_for(&q.interval)?
    } else {
        None
    };
    let meta = Meta {
        time_labels,
        comparison_time_labels: comparison_labels,
        warnings,
        tiers: plan.tiers.clone(),
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
