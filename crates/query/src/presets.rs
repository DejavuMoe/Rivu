//! 旧的 `overview` 与 `breakdown`：现在是 [`QuerySpec`] 的预设，内部调用 [`crate::execute`]（`system-design.md` §6.1：
//! 「v1 的 overview、breakdown 改为看板中的查询预设」）。签名与 P1 一致，`rivu stats` 与端到端测试不用改。
//!
//! 口径与 P1 相同，由统一内核的口径决定（§6.5、§6.6）：
//! - `overview`：`visitors visits pageviews bounce_rate visit_duration engaged_duration`，日期是站点本地日期 `[from, to]`；
//! - `breakdown`：`visitors visits pageviews`，按访客数、会话数、浏览次数降序，同数按（原始）维度值升序；
//!   `Page` 维度带过滤 `event:name is pageview`（只看浏览），此时 `visits` 是「浏览过该页的会话数」。
//!
//! 取值与时区：站点按 UTC 以外的时区换算，由调用方给 `tz`；周起始日取周一（这两个预设用不到周）。

use anyhow::{Context, Result, bail};
use jiff::civil::Date;
use jiff::tz::TimeZone;

use crate::exec::{ExecOptions, execute};
use crate::resolve::SiteContext;
use crate::result::{MetricValue, ResultRow};
use crate::spec::{
    CustomRange, DateRange, Dimension as SpecDimension, Direction, Filter, FilterLeaf, FilterOp,
    FilterTarget, Include, Metric, OrderBy, Pagination, QuerySpec, RangeBound,
};
use crate::sql::TenantScope;

/// 核心指标（§6.5）。
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Overview {
    /// `uniq(visitor_id)`，不含 engagement 事件（§10 Q17）。
    pub visitors: u64,
    /// 区间内活跃的会话数。
    pub visits: u64,
    pub pageviews: u64,
    /// 0–1。
    pub bounce_rate: f64,
    /// 秒，不含 engagement 时间（§10 Q2）。
    pub visit_duration: f64,
    /// 秒，只统计 `engagement_ms > 0` 的会话。
    pub engaged_duration: f64,
}

fn dates(from: Date, to: Date) -> DateRange {
    DateRange::Custom(CustomRange {
        from: RangeBound::Date(from),
        to: RangeBound::Date(to),
    })
}

fn int(v: MetricValue) -> u64 {
    match v {
        MetricValue::Int(n) => n,
        MetricValue::Float(f) => f as u64,
    }
}

/// 站点本地日期范围 `[from, to]`（含）的核心指标。`reader` 用 `rivu_reader` 账号构造，
/// `timeout_secs` 是执行时限（看板 / API / 公开看板各一档，§10 Q14）。
pub async fn overview(
    reader: &clickhouse::Client,
    site_id: u64,
    tz: &TimeZone,
    from: Date,
    to: Date,
    timeout_secs: u64,
) -> Result<Overview> {
    if from > to {
        bail!("起始日期 {from} 晚于结束日期 {to}");
    }
    let spec = QuerySpec {
        site: None,
        metrics: vec![
            Metric::Visitors,
            Metric::Visits,
            Metric::Pageviews,
            Metric::BounceRate,
            Metric::VisitDuration,
            Metric::EngagedDuration,
        ],
        date_range: dates(from, to),
        dimensions: Vec::new(),
        filters: Vec::new(),
        order_by: Vec::new(),
        include: Include::default(),
        pagination: Pagination::default(),
    };
    let site = SiteContext::new(site_id, tz.clone());
    let result = execute(
        reader,
        &TenantScope::new(site_id),
        &site,
        spec,
        ExecOptions::new(timeout_secs).with_route("overview"),
    )
    .await
    .context("查询 overview 失败")?;
    let row = result.results.first().context("overview 没有返回行")?;
    let m = &row.metrics;
    let f = |v: MetricValue| v.as_f64();
    Ok(Overview {
        visitors: int(m[0]),
        visits: int(m[1]),
        pageviews: int(m[2]),
        bounce_rate: f(m[3]),
        visit_duration: f(m[4]),
        engaged_duration: f(m[5]),
    })
}

/// 分组维度（P1：页面、来源、渠道、国家、设备类别）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Dimension {
    /// `event:page`：`events.pathname`，只看 pageview。
    Page,
    /// `visit:source`：`referrer_source`。
    Source,
    /// `visit:channel`。
    Channel,
    /// `visit:country`：ISO 3166-1 alpha-2。
    Country,
    /// `visit:device`：desktop / mobile / tablet。
    Device,
}

/// 空值（来源、渠道、国家、设备）的展示文案，沿用 Plausible 的习惯
/// （`docs/research/plausible/03-stats-query-engine.md` §5.1）：来源 `Direct / None`、渠道 `Direct`、
/// 设备 `(not set)`；国家 Plausible 没有兜底文案（直接丢掉未知行），Rivu 保留这一行并按设备同样写 `(not set)`。
pub const NO_SOURCE: &str = "Direct / None";
pub const NO_CHANNEL: &str = "Direct";
pub const NOT_SET: &str = "(not set)";

impl Dimension {
    pub const ALL: [Dimension; 5] = [
        Dimension::Page,
        Dimension::Source,
        Dimension::Channel,
        Dimension::Country,
        Dimension::Device,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Dimension::Page => "page",
            Dimension::Source => "source",
            Dimension::Channel => "channel",
            Dimension::Country => "country",
            Dimension::Device => "device",
        }
    }

    /// 空值（库里存的是空串）的展示文案。
    pub fn empty_label(self) -> &'static str {
        match self {
            Dimension::Page => "(none)",
            Dimension::Source => NO_SOURCE,
            Dimension::Channel => NO_CHANNEL,
            Dimension::Country | Dimension::Device => NOT_SET,
        }
    }

    /// 结果是否含位置数据：含则调用方必须带上 `GeoDb::attribution()`（§4.4「署名要求」，`meta.geo_attribution` 必填）。
    pub fn needs_geo_attribution(self) -> bool {
        self == Dimension::Country
    }

    fn spec_dimension(self) -> SpecDimension {
        match self {
            Dimension::Page => SpecDimension::EventPage,
            Dimension::Source => SpecDimension::VisitSource,
            Dimension::Channel => SpecDimension::VisitChannel,
            Dimension::Country => SpecDimension::VisitCountry,
            Dimension::Device => SpecDimension::VisitDevice,
        }
    }
}

impl std::str::FromStr for Dimension {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Dimension::ALL
            .into_iter()
            .find(|d| d.as_str() == s)
            .ok_or_else(|| {
                format!("`{s}` 不是有效的维度，可选 page | source | channel | country | device")
            })
    }
}

/// 分组结果的一行。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BreakdownRow {
    /// 维度值；空值已换成展示文案（[`Dimension::empty_label`]）。
    pub value: String,
    /// `uniq(visitor_id)`（非 engagement 事件），只读 `events`（Q9、Q17）；`page` 维度只看 pageview。
    pub visitors: u64,
    /// 会话维度：区间内活跃、且（会话首事件的）该属性等于本行的会话数；
    /// `page` 维度：区间内浏览过该页的会话数（一个会话没有单一的页面，所以只能从 events 数）。
    pub visits: u64,
    pub pageviews: u64,
}

fn breakdown_spec(from: Date, to: Date, dim: Dimension, limit: u32) -> QuerySpec {
    let filters = if dim == Dimension::Page {
        vec![Filter::Leaf(FilterLeaf {
            op: FilterOp::Is,
            target: FilterTarget::Dimension(SpecDimension::EventName),
            values: vec!["pageview".to_owned()],
            case_sensitive: None,
        })]
    } else {
        Vec::new()
    };
    let order = |k: &str| OrderBy {
        key: k.to_owned(),
        direction: Direction::Desc,
    };
    QuerySpec {
        site: None,
        metrics: vec![Metric::Visitors, Metric::Visits, Metric::Pageviews],
        date_range: dates(from, to),
        dimensions: vec![dim.spec_dimension()],
        filters,
        order_by: vec![order("visitors"), order("visits"), order("pageviews")],
        include: Include::default(),
        pagination: Pagination {
            limit: limit.clamp(1, crate::spec::MAX_LIMIT),
            offset: 0,
        },
    }
}

fn breakdown_row(row: &ResultRow) -> BreakdownRow {
    BreakdownRow {
        value: row.dimensions[0].clone(),
        visitors: int(row.metrics[0]),
        visits: int(row.metrics[1]),
        pageviews: int(row.metrics[2]),
    }
}

/// 站点本地日期范围 `[from, to]`（含）内按维度分组：每个维度值的访客数、会话数、浏览次数，
/// 按访客数降序，最多 `limit` 行。`timeout_secs` 同 [`overview`]。
///
/// 空值（库里是空串）换成展示文案（[`Dimension::empty_label`]）。基数不受控的维度（页面）要靠 `limit` 截断。
/// 维度含国家时（[`Dimension::needs_geo_attribution`]），调用方必须同时给出 `GeoDb::attribution()`。
#[allow(clippy::too_many_arguments)] // 签名由任务书规定：与 `overview` 的参数顺序一致，再加维度与行数
pub async fn breakdown(
    reader: &clickhouse::Client,
    site_id: u64,
    tz: &TimeZone,
    from: Date,
    to: Date,
    dim: Dimension,
    limit: u32,
    timeout_secs: u64,
) -> Result<Vec<BreakdownRow>> {
    if from > to {
        bail!("起始日期 {from} 晚于结束日期 {to}");
    }
    if limit == 0 {
        return Ok(Vec::new());
    }
    let site = SiteContext::new(site_id, tz.clone());
    let result = execute(
        reader,
        &TenantScope::new(site_id),
        &site,
        breakdown_spec(from, to, dim, limit),
        ExecOptions::new(timeout_secs).with_route(format!("breakdown.{}", dim.as_str())),
    )
    .await
    .with_context(|| format!("查询 breakdown({}) 失败", dim.as_str()))?;
    Ok(result.results.iter().map(breakdown_row).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dimensions_parse_and_label_empty_values() {
        for d in Dimension::ALL {
            assert_eq!(d.as_str().parse::<Dimension>(), Ok(d));
        }
        assert!("browser".parse::<Dimension>().is_err());
        assert_eq!(Dimension::Source.empty_label(), "Direct / None");
        assert_eq!(Dimension::Channel.empty_label(), "Direct");
        assert_eq!(Dimension::Device.empty_label(), "(not set)");
        assert_eq!(Dimension::Country.empty_label(), "(not set)");
        assert!(Dimension::Country.needs_geo_attribution());
        assert!(!Dimension::Device.needs_geo_attribution());
    }

    #[test]
    fn breakdown_presets_are_ordinary_query_specs() {
        let from = jiff::civil::date(2026, 3, 28);
        let page = breakdown_spec(from, from, Dimension::Page, 5);
        assert_eq!(page.dimensions, [SpecDimension::EventPage]);
        // 页面榜只看 pageview
        assert_eq!(page.filters.len(), 1);
        assert_eq!(page.pagination.limit, 5);
        let source = breakdown_spec(from, from, Dimension::Source, 1_000_000);
        assert!(source.filters.is_empty());
        assert_eq!(source.pagination.limit, crate::spec::MAX_LIMIT);
        // 预设与外部请求走同一套序列化形态
        let json = serde_json::to_value(&page).unwrap();
        assert_eq!(QuerySpec::parse(&json).unwrap(), page);
    }
}
