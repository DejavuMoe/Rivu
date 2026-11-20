//! 多站点汇总视图（`GET /api/internal/orgs/{org_id}/consolidated`，`system-design.md` §6.11）：一个组织里调用者能读的
//! 全部站点的核心指标加在一起，外加按日期的趋势和每个站点各自的数字。
//!
//! # 范围与权限
//!
//! - 只在登录后的看板里有：会话鉴权（没有会话 401），API key、共享链接、公开看板都调不到，公开查询 API 也没有对应的入口；
//! - 站点 = 该组织里未删除、组织未删除、调用者能读统计（`stats.read`，组织成员或站点访客，有效角色取高者）的站点，
//!   判定与看板单站查询的会话分支相同（[`crate::dash_api::may_read_stats`]）；
//! - 一个可读的站点都没有就是 404：不是成员、组织不存在或已删除、组织里没有站点、只是别的组织的站点访客，响应完全相同；
//! - 最多 [`MAX_SITES`] 个站点（按站点名排在前面的），超过时 `meta.truncated = true`。
//!
//! # 口径
//!
//! Rivu 不跨站识别访客，所以 `visitors` 是各站点访客数之和（同一个人访问了两个站点算两次），`meta.visitors = "sum_of_sites"`
//! 并带一句说明；`visits`、`pageviews` 是和；`bounce_rate`、`visit_duration` 是按各站点 `visits` 加权的平均。每个站点按自己的
//! 时区算日期，趋势按日期标签相加（同一个标签在不同站点是各自的本地日）。
//!
//! # 实现
//!
//! 每个站点跑两次统一查询内核（合计、按天或按月的趋势），走看板单站查询的同一条路径
//! （[`DashApi::query_as_user`]：站点上下文、看板档超时、`log_comment`、结果缓存），在这里合并。同时在跑的站点查询数不超过
//! 公开 API 的每组织并发上限（`RIVU_API_MAX_CONCURRENCY`，默认 4）。任何一个站点的查询失败，整个请求按那个错误返回。

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use axum::Router;
use axum::extract::{Path, Request, State};
use axum::http::StatusCode;
use axum::response::Response;
use axum::routing::get;
use rivu_query::result::MetricValue;
use rivu_query::{QueryResult, QuerySpec};
use rivu_storage_pg::access::{self, VisibleSite};
use serde_json::{Value, json};
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

use crate::dash_api::{DashApi, DashError, Res, endpoint, json_response, may_read_stats};
use crate::query_api::Fail;
use crate::site_admin::only;

/// 一次汇总最多的站点数。
pub const MAX_SITES: usize = 100;
/// 路由标签（`rivu_api_requests_total`、`rivu_query_seconds`）。
const ROUTE: &str = "internal.consolidated";
/// 汇总的指标，顺序就是查询里 `metrics` 的顺序。
const METRICS: [&str; 5] = [
    "visitors",
    "visits",
    "pageviews",
    "bounce_rate",
    "visit_duration",
];
/// `meta.note`：访客数是怎么加出来的。
const NOTE: &str = "Rivu does not identify people across sites, so visitors is the sum of each site's visitors: \
someone who visited two sites counts twice. visits and pageviews are sums; bounce_rate and visit_duration are \
averages weighted by each site's visits. Each site counts days in its own time zone.";

// ------------------------------------------------------------------------------------------
// 参数
// ------------------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
struct Params {
    /// QuerySpec 的 `date_range`：关键词或 `[from, to]`。
    date_range: Value,
    /// `day` 或 `month`。
    interval: &'static str,
    /// `previous_period`、`year_over_year`，或不对比。
    compare: Option<&'static str>,
}

fn invalid(msg: &str) -> DashError {
    DashError::Invalid(msg.to_owned())
}

/// 查询串：`range`（看板的时间范围关键词，默认 `30d`，不能是 `realtime`）或 `from` + `to`（站点本地日期，含两端）；
/// `interval` = `day`（默认）| `month`；`compare` = `off`（默认）| `previous_period` | `year_over_year`。
/// 关键词本身由查询内核校验（`7d`、`6mo`、`all` 等，与单站查询相同）。
fn parse_params(query: Option<&str>) -> Res<Params> {
    let (mut range, mut from, mut to, mut interval, mut compare) = (None, None, None, None, None);
    if let Some(q) = query {
        for (k, v) in url::form_urlencoded::parse(q.as_bytes()) {
            let v = v.into_owned();
            match k.as_ref() {
                "range" => range = Some(v),
                "from" => from = Some(v),
                "to" => to = Some(v),
                "interval" => interval = Some(v),
                "compare" => compare = Some(v),
                _ => {}
            }
        }
    }
    let date_range = match (range, from, to) {
        (Some(_), Some(_), _) | (Some(_), _, Some(_)) => {
            return Err(invalid("give either range or from and to, not both"));
        }
        (None, Some(f), Some(t)) => {
            if f.len() != 10 || t.len() != 10 {
                return Err(invalid("from and to must be dates (YYYY-MM-DD)"));
            }
            json!([f, t])
        }
        (None, Some(_), None) | (None, None, Some(_)) => {
            return Err(invalid("from and to must be given together"));
        }
        (Some(r), None, None) if r == "realtime" => {
            return Err(invalid(
                "realtime is not available in the consolidated view",
            ));
        }
        (Some(r), None, None) => {
            if r.is_empty() || r.len() > 16 {
                return Err(invalid("unknown range"));
            }
            Value::String(r)
        }
        (None, None, None) => json!("30d"),
    };
    let interval = match interval.as_deref() {
        None | Some("day") => "day",
        Some("month") => "month",
        Some(_) => return Err(invalid("interval must be day or month")),
    };
    let compare = match compare.as_deref() {
        None | Some("off") => None,
        Some("previous_period") => Some("previous_period"),
        Some("year_over_year") => Some("year_over_year"),
        Some(_) => {
            return Err(invalid(
                "compare must be off, previous_period or year_over_year",
            ));
        }
    };
    Ok(Params {
        date_range,
        interval,
        compare,
    })
}

fn totals_spec(p: &Params) -> Value {
    let mut v = json!({"metrics": METRICS, "date_range": p.date_range});
    if let Some(c) = p.compare {
        v["include"] = json!({"compare": c});
    }
    v
}

fn trend_spec(p: &Params) -> Value {
    json!({
        "metrics": METRICS,
        "date_range": p.date_range,
        "dimensions": [format!("time:{}", p.interval)],
        "include": {"time_labels": true},
    })
}

// ------------------------------------------------------------------------------------------
// 合并
// ------------------------------------------------------------------------------------------

/// 可相加的形式：计数直接相加，比率与平均时长乘以 `visits` 后相加，最后再除回去。
#[derive(Debug, Default, Clone, Copy, PartialEq)]
struct Sum {
    visitors: u64,
    visits: u64,
    pageviews: u64,
    bounce_weighted: f64,
    duration_weighted: f64,
}

fn as_u64(v: Option<&MetricValue>) -> u64 {
    match v {
        Some(MetricValue::Int(n)) => *n,
        // 计数类指标总是整数；万一是浮点，按四舍五入（不会出现负数）
        Some(MetricValue::Float(f)) if f.is_finite() && *f > 0.0 => f.round() as u64,
        _ => 0,
    }
}

fn as_f64(v: Option<&MetricValue>) -> f64 {
    v.map(|m| m.as_f64())
        .filter(|f| f.is_finite())
        .unwrap_or(0.0)
}

impl Sum {
    /// 一行结果（`metrics` 与 [`METRICS`] 同序）。
    fn of(m: &[MetricValue]) -> Sum {
        let visits = as_u64(m.get(1));
        Sum {
            visitors: as_u64(m.first()),
            visits,
            pageviews: as_u64(m.get(2)),
            bounce_weighted: as_f64(m.get(3)) * visits as f64,
            duration_weighted: as_f64(m.get(4)) * visits as f64,
        }
    }

    fn add(&mut self, o: &Sum) {
        self.visitors += o.visitors;
        self.visits += o.visits;
        self.pageviews += o.pageviews;
        self.bounce_weighted += o.bounce_weighted;
        self.duration_weighted += o.duration_weighted;
    }

    fn json(&self) -> Value {
        let avg = |w: f64| {
            if self.visits == 0 {
                0.0
            } else {
                w / self.visits as f64
            }
        };
        json!({
            "visitors": self.visitors,
            "visits": self.visits,
            "pageviews": self.pageviews,
            "bounce_rate": avg(self.bounce_weighted),
            "visit_duration": avg(self.duration_weighted),
        })
    }
}

/// 一个站点自己的数字：直接用查询内核的取值（不经加权再除回去，避免浮点误差）。
fn site_metrics(m: &[MetricValue]) -> Value {
    json!({
        "visitors": as_u64(m.first()),
        "visits": as_u64(m.get(1)),
        "pageviews": as_u64(m.get(2)),
        "bounce_rate": as_f64(m.get(3)),
        "visit_duration": as_f64(m.get(4)),
    })
}

/// 一个站点的两份结果。
struct SiteResults {
    totals: QueryResult,
    trend: QueryResult,
}

/// 合并各站点的结果，生成响应体。`sites` 与 `results` 一一对应。
fn combine(
    p: &Params,
    sites: &[VisibleSite],
    results: &[SiteResults],
    sites_total: usize,
) -> Value {
    let mut totals = Sum::default();
    let mut comparison = Sum::default();
    let mut trend: BTreeMap<String, Sum> = BTreeMap::new();
    let mut warnings = Vec::new();
    let mut site_rows = Vec::with_capacity(sites.len());
    for (s, r) in sites.iter().zip(results) {
        let row = r.totals.results.first();
        let metrics = row.map(|x| x.metrics.as_slice()).unwrap_or(&[]);
        totals.add(&Sum::of(metrics));
        let mut site = json!({
            "id": s.id,
            "site_key": s.site_key,
            "name": s.name,
            "timezone": s.timezone,
            "totals": site_metrics(metrics),
            "range": {
                "start": r.totals.query.resolved_date_range.start,
                "end": r.totals.query.resolved_date_range.end,
            },
        });
        if p.compare.is_some() {
            let cm = row
                .and_then(|x| x.comparison.as_ref())
                .map(|c| c.metrics.as_slice())
                .unwrap_or(&[]);
            comparison.add(&Sum::of(cm));
            site["comparison_totals"] = site_metrics(cm);
        }
        site_rows.push(site);

        for label in r.trend.meta.time_labels.iter().flatten() {
            trend.entry(label.clone()).or_default();
        }
        for row in &r.trend.results {
            if let Some(label) = row.dimensions.first() {
                trend
                    .entry(label.clone())
                    .or_default()
                    .add(&Sum::of(&row.metrics));
            }
        }
        let mut seen = BTreeSet::new();
        for w in r.totals.meta.warnings.iter().chain(&r.trend.meta.warnings) {
            if seen.insert(w.code.clone()) {
                warnings.push(json!({"site_id": s.id, "code": w.code, "message": w.message}));
            }
        }
    }
    let first = &sites[0];
    let mut body = json!({
        "organization": {"id": first.org_id, "name": first.org_name, "slug": first.org_slug},
        "date_range": p.date_range,
        "interval": p.interval,
        "compare": p.compare,
        "totals": totals.json(),
        "trend": trend.iter().map(|(label, sum)| {
            let mut v = sum.json();
            v["date"] = json!(label);
            v
        }).collect::<Vec<_>>(),
        "sites": site_rows,
        "meta": {
            "visitors": "sum_of_sites",
            "note": NOTE,
            "sites_total": sites_total,
            "sites_included": sites.len(),
            "max_sites": MAX_SITES,
            "truncated": sites_total > sites.len(),
            "warnings": warnings,
        },
    });
    if p.compare.is_some() {
        body["comparison_totals"] = comparison.json();
    }
    body
}

// ------------------------------------------------------------------------------------------
// 端点
// ------------------------------------------------------------------------------------------

fn parse_spec(v: &Value, timeout: u64) -> Res<QuerySpec> {
    QuerySpec::parse(v).map_err(|e| DashError::Fail(Fail::Query(e), timeout))
}

async fn consolidated(
    State(api): State<Arc<DashApi>>,
    Path(org_seg): Path<String>,
    req: Request,
) -> Res<Response> {
    let ctx = api.require_session(req.headers()).await?;
    let org_id: i64 = org_seg.parse().map_err(|_| DashError::NotFound)?;
    let user_id = ctx.s.user_id;
    let mut sites: Vec<VisibleSite> = access::list_visible_sites(&api.pg, user_id)
        .await?
        .into_iter()
        .filter(|s| s.org_id == org_id && may_read_stats(&s.roles))
        .collect();
    if sites.is_empty() {
        return Err(DashError::NotFound);
    }
    let params = parse_params(req.uri().query())?;
    let sites_total = sites.len();
    sites.truncate(MAX_SITES);

    // 先在这里解析一次：取值不对时在跑任何查询之前就 400
    let timeout = api.dashboard_timeout_secs;
    let specs = [totals_spec(&params), trend_spec(&params)];
    for v in &specs {
        parse_spec(v, timeout)?;
    }

    let permits = Arc::new(Semaphore::new(
        usize::try_from(api.api_rate.max_concurrency)
            .unwrap_or(1)
            .max(1),
    ));
    let mut set = JoinSet::new();
    for (i, s) in sites.iter().enumerate() {
        for (kind, v) in specs.iter().enumerate() {
            let api = Arc::clone(&api);
            let permits = Arc::clone(&permits);
            let (site_id, site_key, v) = (s.id, s.site_key.clone(), v.clone());
            set.spawn(async move {
                let _permit = permits.acquire_owned().await;
                let spec = parse_spec(&v, timeout)?;
                let r = api
                    .query_as_user(user_id, site_id, &site_key, org_id, spec, ROUTE)
                    .await?;
                Ok::<_, DashError>((i, kind, r))
            });
        }
    }
    let mut slots: Vec<[Option<QueryResult>; 2]> = (0..sites.len()).map(|_| [None, None]).collect();
    while let Some(joined) = set.join_next().await {
        let (i, kind, r) = joined.map_err(|e| {
            tracing::error!(error = %e, "汇总视图的站点查询任务异常结束");
            DashError::Internal
        })??;
        slots[i][kind] = Some(r);
    }
    let results: Vec<SiteResults> = slots
        .into_iter()
        .map(|[t, d]| match (t, d) {
            (Some(totals), Some(trend)) => Ok(SiteResults { totals, trend }),
            _ => Err(DashError::Internal),
        })
        .collect::<Res<_>>()?;
    let body = combine(&params, &sites, &results, sites_total);
    Ok(api.finish(json_response(StatusCode::OK, body), &ctx))
}

pub(crate) fn routes(api: &Arc<DashApi>) -> Router {
    Router::new().merge(endpoint(
        api,
        ROUTE,
        "/api/internal/orgs/{org_id}/consolidated",
        only(get(consolidated), "GET, HEAD"),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(q: &str) -> Res<Params> {
        parse_params(Some(q))
    }

    #[test]
    fn params_default_to_the_dashboard_range() {
        let d = parse_params(None).unwrap();
        assert_eq!(d.date_range, json!("30d"));
        assert_eq!(d.interval, "day");
        assert_eq!(d.compare, None);
        let c = p("from=2026-11-01&to=2026-11-10&interval=month&compare=previous_period").unwrap();
        assert_eq!(c.date_range, json!(["2026-11-01", "2026-11-10"]));
        assert_eq!(c.interval, "month");
        assert_eq!(c.compare, Some("previous_period"));
        assert_eq!(p("range=6mo&compare=off").unwrap().date_range, json!("6mo"));
    }

    #[test]
    fn bad_params_are_rejected() {
        for q in [
            "range=realtime",
            "range=7d&from=2026-11-01&to=2026-11-02",
            "from=2026-11-01",
            "to=2026-11-01",
            "from=2026-11-01T00:00:00Z&to=2026-11-02T00:00:00Z",
            "interval=week",
            "interval=hour",
            "compare=yes",
            "range=",
        ] {
            assert!(p(q).is_err(), "{q}");
        }
    }

    #[test]
    fn rates_are_weighted_by_visits() {
        let a = Sum::of(&[
            MetricValue::Int(3),
            MetricValue::Int(3),
            MetricValue::Int(6),
            MetricValue::Float(1.0 / 3.0),
            MetricValue::Float(50.0),
        ]);
        let b = Sum::of(&[
            MetricValue::Int(2),
            MetricValue::Int(2),
            MetricValue::Int(3),
            MetricValue::Float(0.5),
            MetricValue::Float(30.0),
        ]);
        let mut t = Sum::default();
        t.add(&a);
        t.add(&b);
        let v = t.json();
        assert_eq!(v["visitors"], 5);
        assert_eq!(v["visits"], 5);
        assert_eq!(v["pageviews"], 9);
        assert!((v["bounce_rate"].as_f64().unwrap() - 0.4).abs() < 1e-12);
        assert!((v["visit_duration"].as_f64().unwrap() - 42.0).abs() < 1e-9);
        // 没有访问时比率是 0，不是 NaN
        assert_eq!(Sum::default().json()["bounce_rate"], 0.0);
    }
}
