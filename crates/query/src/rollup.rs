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
        }
    }

    pub(crate) fn column(self, col: Col) -> Option<&'static str> {
        self.dims().iter().find(|(c, _)| *c == col).map(|(_, n)| *n)
    }

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
            Family::Pages => m != Measure::CrBase,
            _ => !matches!(m, Measure::Exits | Measure::CrBase),
        }
    }

    /// 构建时这一族的 `visits` 口径：有事件级维度的族是事件形式。
    pub(crate) fn visits_form(self) -> VisitsForm {
        match self {
            Family::Pages | Family::Events => VisitsForm::Events,
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
    let family = match cols.first() {
        None => Family::Site,
        Some(c) => Family::of(*c)?,
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
    /// 有按别的时区构建的行：站点改过时区，要全部删除重建。
    pub tz_changed: bool,
    /// 站点最早事件的本地日（没有事件为 `None`）。
    pub first_day: Option<Date>,
}

pub async fn build_state(
    client: &Client,
    scope: &TenantScope,
    tz: &TimeZone,
) -> Result<BuildState, QueryError> {
    let tz_name = tz.iana_name().unwrap_or("UTC");
    let (sql, params) = sql::rollup_probe_query(scope, tz_name, "1970-01-01", "1970-01-01");
    let rows = fetch(client, &sql, &params).await?;
    let row = rows.first();
    let built_to = row
        .and_then(|r| r.first())
        .and_then(serde_json::Value::as_str)
        .and_then(|s| s.parse().ok());
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
    Ok(BuildState {
        built_to,
        tz_changed,
        first_day,
    })
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
    for family in Family::ALL {
        let (sql, params) = build_query(scope, site, family, day, version, now)?;
        execute(client, &sql, &params).await?;
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
    // site 先删：中途失败时这些天不再算「已构建」
    let order =
        std::iter::once(Family::Site).chain(Family::ALL.into_iter().filter(|f| *f != Family::Site));
    for family in order {
        let sql = format!(
            "DELETE FROM {} WHERE site_id = {{site:UInt64}}{cond}",
            family.table()
        );
        execute(client, &sql, &params).await?;
    }
    Ok(())
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
