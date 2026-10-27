//! `ResolvedQuery`：把语法上合法的 [`QuerySpec`] 变成可以规划的查询（`system-design.md` §6.3 的 build 与 optimize 两步）。
//!
//! 做三件事：
//! 1. **注入服务端上下文**：站点 id、时区、周起始日、「现在」。请求体里的 `site` 只用来核对，从不决定查询哪个站点；
//! 2. **时间换算**：`date_range` 按站点时区换算成 UTC 毫秒区间 `[start, end)`；相对范围、DST、周起始日、`now` 封顶、
//!    自动时间粒度、对比期都在这里决定；
//! 3. **业务校验**：指标与维度、过滤的组合、正则与取值的数量限制、「尚未支持」的功能，错误带字段路径。
//!
//! `ResolvedQuery` 的字段对 crate 外不可见，构造函数只在 crate 内：路由层拿不到绕过校验的办法（§6.3、R03 §12.3）。
//!
//! # 时间语义（本模块的约定，参考实现独立地再实现一遍）
//!
//! - 「本地日」是站点时区的自然日；本地日 `d` 的起点是 `d 00:00`（落在 DST 缺口里取缺口之后的第一个时刻），
//!   所以 23 / 25 小时的日子都恰好是一个本地日。
//! - `day`：今天；`24h`：`[now - 24h, now]`；`realtime`：`[now - 5 min, now]`；
//!   `Nd`（含 `7d` `28d` `30d`）：截至**今天**的 N 个本地日，含今天；`month`：本月 1 日到今天；
//!   `Nmo`（含 `6mo` `12mo`）：截至今天的 N 个自然月，含本月；`year`：当年 1 月 1 日到今天；
//!   `all`：站点最早事件所在本地日到今天（最早事件由 ClickHouse 查一次，没有数据时只含今天）。
//! - 所有区间的终点都**封顶到 `now`**（`end = min(end, now + 1 ms)`）：未来没有数据，这样 `month`、`year`、`day`
//!   的时间标签不会出现未来的空桶，数据与标签用同一个终点。
//! - `time:week` 的桶以站点设置的周起始日为界，桶标签是该周起始日的日期（首桶可能早于范围起点）。
//! - 一个会话在时间维度上归入「它在范围内首次活跃」的桶：`max(started_at, 范围起点)` 所在的桶，
//!   所以各桶的 `visits` 之和等于整个范围的 `visits`。

use std::collections::BTreeSet;

use jiff::civil::{Date, Weekday};
use jiff::tz::TimeZone;
use jiff::{Span, Timestamp};
use regex::RegexBuilder;
use rivu_geo::{Attribution, GeoCapability};
use serde_json::{Value, json};

use crate::error::QueryError;
use crate::spec::{
    Compare, CustomRange, DateRange, Dimension, Direction, Filter, FilterLeaf, FilterOp,
    FilterTarget, Metric, QuerySpec, RangeBound,
};

/// 调用方（鉴权之后）传入的站点上下文。
#[derive(Clone, Debug)]
pub struct SiteContext {
    pub site_id: u64,
    /// 给出时与 `QuerySpec.site` 核对。
    pub site_key: Option<String>,
    /// 站点时区。
    pub timezone: TimeZone,
    /// 周起始日（站点设置，默认周一，§6.7）。
    pub week_starts_on: Weekday,
    /// 当前加载的地理库的署名与能力（`rivu_geo::GeoDb::attribution()` / `capability()`）；结果含位置维度时回显。
    pub geo_attribution: Option<Attribution>,
    pub geo_capability: Option<GeoCapability>,
    /// 站点的目标（未删除，调用方从 PG 加载，§6.3 build 阶段「加载目标」）：`event:goal` 维度与过滤、`conversion_rate` 用。
    pub goals: Vec<GoalDef>,
}

impl SiteContext {
    /// UTC、周一起始、没有地理库、没有目标的站点（测试用）。
    pub fn new(site_id: u64, timezone: TimeZone) -> Self {
        SiteContext {
            site_id,
            site_key: None,
            timezone,
            week_starts_on: Weekday::Monday,
            geo_attribution: None,
            geo_capability: None,
            goals: Vec::new(),
        }
    }
}

/// 一个目标（`goals` 表的一行）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GoalDef {
    pub id: i64,
    /// 标签：显示名，否则事件名，否则页面通配。`event:goal` 维度的取值，也是过滤里认的名字。
    pub label: String,
    pub kind: GoalKind,
}

/// 目标的完成条件（只看非 engagement 事件）。
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum GoalKind {
    /// 事件名完全相等（区分大小写）。
    Event(String),
    /// `pageview` 事件，且 pathname 整串匹配通配模式：只有 `*` 是通配（匹配任意串，含空串与 `/`），区分大小写，
    /// 与屏蔽规则的 `page` 模式同一语义。
    Page(String),
}

impl GoalDef {
    /// 由 `goals` 表的字段构造：`kind` 是 `event` 或 `page`，否则返回 `None`。
    pub fn from_parts(
        id: i64,
        kind: &str,
        event_name: Option<&str>,
        page_path: Option<&str>,
        display_name: Option<&str>,
    ) -> Option<GoalDef> {
        let kind = match (kind, event_name, page_path) {
            ("event", Some(n), _) => GoalKind::Event(n.to_owned()),
            ("page", _, Some(p)) => GoalKind::Page(p.to_owned()),
            _ => return None,
        };
        let value = match &kind {
            GoalKind::Event(v) | GoalKind::Page(v) => v.clone(),
        };
        Some(GoalDef {
            id,
            label: display_name.map(str::to_owned).unwrap_or(value),
            kind,
        })
    }
}

// ------------------------------------------------------------------------------------------
// 限制
// ------------------------------------------------------------------------------------------

pub const MAX_METRICS: usize = 12;
pub const MAX_DIMENSIONS: usize = 4;
/// 一个叶子过滤里的取值个数上限：`is` 系列、`contains` 系列、正则与通配符系列。
pub const MAX_IS_VALUES: usize = 1000;
pub const MAX_CONTAINS_VALUES: usize = 100;
/// 每个过滤最多 10 个模式、每个最多 256 字符（§6.8）。
pub const MAX_PATTERNS: usize = 10;
pub const MAX_PATTERN_CHARS: usize = 256;
/// 发给 ClickHouse 的正则最长 256 字节（`max_hyperscan_regexp_length`，核实报告 #11）；
/// 通配符转义、`(?i)` 前缀都算在内。
pub const MAX_REGEX_BYTES: usize = 256;
pub const MAX_VALUE_CHARS: usize = 1024;
pub const MAX_FILTER_LEAVES: usize = 100;
/// 时间维度的桶数上限（标签与结果行数）。
pub const MAX_TIME_BUCKETS: usize = 10_000;
/// `time:minute` 的桶数上限：25 小时 = 1500 个桶，足够一个 DST 回拨日（25 小时）或起点不在整分的 `24h`（1441 个桶）。
/// 分钟桶只服务实时视图，更长的范围用 `time:hour`。
pub const MAX_MINUTE_BUCKETS: usize = 1500;

/// 支持的最早本地日期：保证本地午夜换算成 UTC 后仍在 1970 年之后（ClickHouse 的 `Date` 从 1970-01-01 起）。
const MIN_DATE: Date = Date::constant(1970, 1, 3);
const MAX_DATE: Date = Date::constant(2100, 12, 31);

// ------------------------------------------------------------------------------------------
// 列、粒度
// ------------------------------------------------------------------------------------------

/// 维度与过滤用到的「列」（去掉了时间维度和不支持的维度）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) enum Col {
    EventName,
    EventPage,
    EventHostname,
    /// `event:goal`：不是表里的列。维度取值是匹配的目标标签（一个事件可以匹配多个目标），
    /// 过滤叶子换成目标的完成条件（[`Test::Goal`]）。
    EventGoal,
    EntryPage,
    ExitPage,
    Source,
    Channel,
    Referrer,
    UtmSource,
    UtmMedium,
    UtmCampaign,
    UtmContent,
    UtmTerm,
    Country,
    Region,
    City,
    Device,
    Browser,
    BrowserVersion,
    Os,
    OsVersion,
}

impl Col {
    pub(crate) fn from_dimension(d: &Dimension) -> Option<Col> {
        Some(match d {
            Dimension::EventName => Col::EventName,
            Dimension::EventPage => Col::EventPage,
            Dimension::EventHostname => Col::EventHostname,
            Dimension::EventGoal => Col::EventGoal,
            Dimension::VisitEntryPage => Col::EntryPage,
            Dimension::VisitExitPage => Col::ExitPage,
            Dimension::VisitSource => Col::Source,
            Dimension::VisitChannel => Col::Channel,
            Dimension::VisitReferrer => Col::Referrer,
            Dimension::VisitUtmSource => Col::UtmSource,
            Dimension::VisitUtmMedium => Col::UtmMedium,
            Dimension::VisitUtmCampaign => Col::UtmCampaign,
            Dimension::VisitUtmContent => Col::UtmContent,
            Dimension::VisitUtmTerm => Col::UtmTerm,
            Dimension::VisitCountry => Col::Country,
            Dimension::VisitRegion => Col::Region,
            Dimension::VisitCity => Col::City,
            Dimension::VisitDevice => Col::Device,
            Dimension::VisitBrowser => Col::Browser,
            Dimension::VisitBrowserVersion => Col::BrowserVersion,
            Dimension::VisitOs => Col::Os,
            Dimension::VisitOsVersion => Col::OsVersion,
            _ => return None,
        })
    }

    /// 事件级列（`event:*`）。
    pub(crate) fn is_event_level(self) -> bool {
        matches!(
            self,
            Col::EventName | Col::EventPage | Col::EventHostname | Col::EventGoal
        )
    }

    /// 空值（库里存的是空串）的展示文案，沿用 Plausible 的习惯（`docs/research/plausible/03-stats-query-engine.md` §5.1）：
    /// 来源 `Direct / None`、渠道 `Direct`，其余 `(not set)`（Plausible 对国家没有兜底文案，直接丢掉未知行）。
    pub(crate) fn empty_label(self) -> &'static str {
        match self {
            Col::Source | Col::Referrer => "Direct / None",
            Col::Channel => "Direct",
            Col::UtmSource
            | Col::UtmMedium
            | Col::UtmCampaign
            | Col::UtmContent
            | Col::UtmTerm
            | Col::Country
            | Col::Region
            | Col::City
            | Col::Device
            | Col::Browser
            | Col::BrowserVersion
            | Col::Os
            | Col::OsVersion => "(not set)",
            Col::EventName
            | Col::EventPage
            | Col::EventHostname
            | Col::EventGoal
            | Col::EntryPage
            | Col::ExitPage => "(none)",
        }
    }
}

/// 时间粒度。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Gran {
    Minute,
    Hour,
    Day,
    Week,
    Month,
}

impl Gran {
    pub(crate) fn dimension_name(self) -> &'static str {
        match self {
            Gran::Minute => "time:minute",
            Gran::Hour => "time:hour",
            Gran::Day => "time:day",
            Gran::Week => "time:week",
            Gran::Month => "time:month",
        }
    }
}

/// 解析后的维度。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RDim {
    Col(Col),
    Time(Gran),
}

// ------------------------------------------------------------------------------------------
// 过滤树
// ------------------------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Pred {
    And(Vec<Pred>),
    Or(Vec<Pred>),
    Not(Box<Pred>),
    Leaf(Leaf),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Leaf {
    pub(crate) col: Col,
    pub(crate) test: Test,
}

/// 叶子的判定。取值都已规范化：展示文案换回空串、不区分大小写时已转成 ASCII 小写、通配符已转成正则。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Test {
    /// `is` / `is_not`。
    In {
        values: Vec<String>,
        case_sensitive: bool,
        negated: bool,
    },
    /// `contains` / `contains_not`（子串，不匹配空串）。
    Contains {
        needles: Vec<String>,
        case_sensitive: bool,
        negated: bool,
    },
    /// `matches*`：已含 `(?i)` 前缀与通配符转出来的锚点。
    Regex {
        patterns: Vec<String>,
        negated: bool,
    },
    /// `visit:city`：geoname id。
    CityIn { ids: Vec<u32>, negated: bool },
    /// `event:goal` 的 `is` / `is_not`：事件满足其中任一目标的完成条件（`goals` 已按标签或 id 解析、去重；
    /// 没有对上任何目标时为空，`is` 恒假、`is_not` 恒真）。
    Goal { goals: Vec<GoalKind>, negated: bool },
}

impl Pred {
    pub(crate) fn any_leaf(&self, f: &impl Fn(&Leaf) -> bool) -> bool {
        match self {
            Pred::And(c) | Pred::Or(c) => c.iter().any(|p| p.any_leaf(f)),
            Pred::Not(p) => p.any_leaf(f),
            Pred::Leaf(l) => f(l),
        }
    }

    /// 含事件级叶子（`event:*`）。
    pub(crate) fn has_event_leaf(&self) -> bool {
        self.any_leaf(&|l| l.col.is_event_level())
    }

    pub(crate) fn has_leaf_on(&self, col: Col) -> bool {
        self.any_leaf(&|l| l.col == col)
    }

    pub(crate) fn has_geo_leaf(&self) -> bool {
        self.any_leaf(&|l| matches!(l.col, Col::Country | Col::Region | Col::City))
    }

    /// 含 `event:page` / `event:hostname` 叶子：`bounce_rate` 要把它们换成入口页（§6.6）。
    pub(crate) fn has_page_leaf(&self) -> bool {
        self.any_leaf(&|l| matches!(l.col, Col::EventPage | Col::EventHostname))
    }

    /// 去掉全部 `event:goal` 叶子（`conversion_rate` 的分母，§6.5）：`and` / `or` 去掉这些子节点，
    /// 子节点全被去掉的分支与 `not` 也一并去掉；整棵树都是目标叶子时返回 `None`。
    pub(crate) fn without_goal_leaves(&self) -> Option<Pred> {
        match self {
            Pred::Leaf(l) => (l.col != Col::EventGoal).then(|| self.clone()),
            Pred::Not(x) => x.without_goal_leaves().map(|x| Pred::Not(Box::new(x))),
            Pred::And(c) | Pred::Or(c) => {
                let kept: Vec<Pred> = c.iter().filter_map(Pred::without_goal_leaves).collect();
                if kept.is_empty() {
                    None
                } else if matches!(self, Pred::And(_)) {
                    Some(Pred::And(kept))
                } else {
                    Some(Pred::Or(kept))
                }
            }
        }
    }
}

fn leaf_path_error(path: &str, sub: &str, msg: impl Into<String>) -> QueryError {
    QueryError::validation(format!("{path}{sub}"), msg)
}

fn build_pred(
    f: &Filter,
    path: &str,
    leaves: &mut usize,
    first_event_leaf: &mut Option<String>,
    goals: &[GoalDef],
) -> Result<Pred, QueryError> {
    match f {
        Filter::Leaf(l) => {
            *leaves += 1;
            if *leaves > MAX_FILTER_LEAVES {
                return Err(QueryError::validation(
                    "filters",
                    format!("a query may have at most {MAX_FILTER_LEAVES} filter conditions"),
                ));
            }
            let leaf = build_leaf(l, path, goals)?;
            if leaf.col.is_event_level() && first_event_leaf.is_none() {
                *first_event_leaf = Some(path.to_owned());
            }
            Ok(Pred::Leaf(leaf))
        }
        Filter::And(c) | Filter::Or(c) => {
            let mut out = Vec::with_capacity(c.len());
            for (j, child) in c.iter().enumerate() {
                out.push(build_pred(
                    child,
                    &format!("{path}[1][{j}]"),
                    leaves,
                    first_event_leaf,
                    goals,
                )?);
            }
            Ok(if matches!(f, Filter::And(_)) {
                Pred::And(out)
            } else {
                Pred::Or(out)
            })
        }
        Filter::Not(c) => Ok(Pred::Not(Box::new(build_pred(
            c,
            &format!("{path}[1]"),
            leaves,
            first_event_leaf,
            goals,
        )?))),
        Filter::HasDone(_) | Filter::HasNotDone(_) => Err(QueryError::validation(
            format!("{path}[0]"),
            "`has_done` and `has_not_done` filters are not supported yet",
        )),
    }
}

fn build_leaf(l: &FilterLeaf, path: &str, goals: &[GoalDef]) -> Result<Leaf, QueryError> {
    let dim = match &l.target {
        FilterTarget::Segment => {
            return Err(leaf_path_error(
                path,
                "[1]",
                "segment filters are not supported yet",
            ));
        }
        FilterTarget::Dimension(d) => d,
    };
    if dim.is_time() {
        return Err(leaf_path_error(
            path,
            "[1]",
            "time dimensions cannot be used in filters",
        ));
    }
    if !dim.is_supported() {
        return Err(leaf_path_error(
            path,
            "[1]",
            format!("filtering by `{dim}` is not supported yet"),
        ));
    }
    let col = Col::from_dimension(dim).expect("支持的非时间维度都有列");
    let cs = l.case_sensitive.unwrap_or(true);
    let negated = l.op.is_negated();
    if l.values.is_empty() {
        return Err(leaf_path_error(path, "[2]", "expected at least one value"));
    }
    let max_values = match l.op {
        FilterOp::Is | FilterOp::IsNot => MAX_IS_VALUES,
        FilterOp::Contains | FilterOp::ContainsNot => MAX_CONTAINS_VALUES,
        _ => MAX_PATTERNS,
    };
    if l.values.len() > max_values {
        return Err(leaf_path_error(
            path,
            "[2]",
            format!(
                "`{}` accepts at most {max_values} values, got {}",
                l.op.as_str(),
                l.values.len()
            ),
        ));
    }
    let is_pattern = matches!(
        l.op,
        FilterOp::Matches
            | FilterOp::MatchesNot
            | FilterOp::MatchesWildcard
            | FilterOp::MatchesWildcardNot
    );
    let char_limit = if is_pattern {
        MAX_PATTERN_CHARS
    } else {
        MAX_VALUE_CHARS
    };
    for (i, v) in l.values.iter().enumerate() {
        if v.chars().count() > char_limit {
            return Err(leaf_path_error(
                path,
                &format!("[2][{i}]"),
                format!("a value may have at most {char_limit} characters"),
            ));
        }
        if v.contains('\0') {
            return Err(leaf_path_error(
                path,
                &format!("[2][{i}]"),
                "values must not contain NUL characters",
            ));
        }
        if !matches!(l.op, FilterOp::Is | FilterOp::IsNot) && v.is_empty() {
            return Err(leaf_path_error(
                path,
                &format!("[2][{i}]"),
                format!("`{}` values must not be empty", l.op.as_str()),
            ));
        }
    }

    if col == Col::EventGoal {
        if !matches!(l.op, FilterOp::Is | FilterOp::IsNot) {
            return Err(leaf_path_error(
                path,
                "[0]",
                "`event:goal` only supports `is` and `is_not` (values are goal names or ids)",
            ));
        }
        return Ok(Leaf {
            col,
            test: Test::Goal {
                goals: match_goals(goals, &l.values, cs),
                negated,
            },
        });
    }

    if col == Col::City {
        if !matches!(l.op, FilterOp::Is | FilterOp::IsNot) {
            return Err(leaf_path_error(
                path,
                "[0]",
                "`visit:city` only supports `is` and `is_not` (values are GeoNames ids)",
            ));
        }
        let mut ids = Vec::with_capacity(l.values.len());
        for (i, v) in l.values.iter().enumerate() {
            let id: u32 = v.parse().map_err(|_| {
                leaf_path_error(
                    path,
                    &format!("[2][{i}]"),
                    "`visit:city` values must be GeoNames ids (unsigned integers)",
                )
            })?;
            ids.push(id);
        }
        return Ok(Leaf {
            col,
            test: Test::CityIn { ids, negated },
        });
    }

    let test = match l.op {
        FilterOp::Is | FilterOp::IsNot => {
            let label = col.empty_label();
            let values = l
                .values
                .iter()
                .map(|v| {
                    let is_label = if cs {
                        v == label
                    } else {
                        v.eq_ignore_ascii_case(label)
                    };
                    let v = if is_label { String::new() } else { v.clone() };
                    if cs { v } else { v.to_ascii_lowercase() }
                })
                .collect();
            Test::In {
                values,
                case_sensitive: cs,
                negated,
            }
        }
        FilterOp::Contains | FilterOp::ContainsNot => Test::Contains {
            needles: if cs {
                l.values.clone()
            } else {
                l.values.iter().map(|v| v.to_ascii_lowercase()).collect()
            },
            case_sensitive: cs,
            negated,
        },
        FilterOp::Matches
        | FilterOp::MatchesNot
        | FilterOp::MatchesWildcard
        | FilterOp::MatchesWildcardNot => {
            let wildcard = matches!(
                l.op,
                FilterOp::MatchesWildcard | FilterOp::MatchesWildcardNot
            );
            let mut patterns = Vec::with_capacity(l.values.len());
            for (i, v) in l.values.iter().enumerate() {
                let body = if wildcard {
                    wildcard_to_regex(v)
                } else {
                    v.clone()
                };
                let full = if cs { body } else { format!("(?i){body}") };
                if full.len() > MAX_REGEX_BYTES {
                    return Err(leaf_path_error(
                        path,
                        &format!("[2][{i}]"),
                        format!(
                            "the pattern becomes {} bytes after conversion; at most {MAX_REGEX_BYTES} are allowed",
                            full.len()
                        ),
                    ));
                }
                // 用 Rust 的 regex 预检语法：它与 Hyperscan 一样不支持回溯构造（反向引用、环视），
                // 又是线性时间，能提前挡住大部分非法与过大的模式。
                RegexBuilder::new(&format!("(?s){full}"))
                    .size_limit(1 << 20)
                    .build()
                    .map_err(|e| {
                        leaf_path_error(
                            path,
                            &format!("[2][{i}]"),
                            format!("invalid regular expression: {}", first_line(&e.to_string())),
                        )
                    })?;
                patterns.push(full);
            }
            Test::Regex { patterns, negated }
        }
    };
    Ok(Leaf { col, test })
}

/// 过滤取值对上的目标：取值等于目标的标签（`case_sensitive: false` 时按 ASCII 大小写折叠比较），
/// 或等于目标 id 的十进制文本。结果按条件去重、排序（不认识的取值不报错，只是对不上任何目标）。
pub(crate) fn match_goals(goals: &[GoalDef], values: &[String], cs: bool) -> Vec<GoalKind> {
    let hit = |g: &GoalDef| {
        values.iter().any(|v| {
            let by_label = if cs {
                *v == g.label
            } else {
                v.eq_ignore_ascii_case(&g.label)
            };
            by_label || *v == g.id.to_string()
        })
    };
    let set: BTreeSet<GoalKind> = goals
        .iter()
        .filter(|g| hit(g))
        .map(|g| g.kind.clone())
        .collect();
    set.into_iter().collect()
}

/// 页面目标的通配模式转成 ClickHouse `LIKE` 模式：`*`（连续的合并）→ `%`，`%` `_` `\` 前面加 `\` 转义。
/// `LIKE` 整串匹配、区分大小写，与 [`wildcard_to_regex`] 的语义相同，但不走 Hyperscan，没有 256 字节的正则上限。
pub(crate) fn wildcard_to_like(pattern: &str) -> String {
    let mut out = String::with_capacity(pattern.len() + 4);
    let mut prev_star = false;
    for ch in pattern.chars() {
        if ch == '*' {
            if !prev_star {
                out.push('%');
            }
            prev_star = true;
            continue;
        }
        prev_star = false;
        if matches!(ch, '%' | '_' | '\\') {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

fn first_line(s: &str) -> String {
    let line = s
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .unwrap_or(s)
        .trim();
    line.chars().take(200).collect()
}

/// 通配符转正则：`*`（连续的 `*` 等同一个）匹配任意字符串（含空串与 `/`），其余字符按字面匹配，整串匹配。
/// 没有 `|` 之类的运算符：多个候选请用多个取值。
pub(crate) fn wildcard_to_regex(pattern: &str) -> String {
    let mut out = String::from("\\A");
    let mut literal = String::new();
    let mut prev_star = false;
    for ch in pattern.chars() {
        if ch == '*' {
            if !prev_star {
                out.push_str(&regex::escape(&literal));
                literal.clear();
                out.push_str(".*");
            }
            prev_star = true;
        } else {
            literal.push(ch);
            prev_star = false;
        }
    }
    out.push_str(&regex::escape(&literal));
    out.push_str("\\z");
    out
}

// ------------------------------------------------------------------------------------------
// 时间区间
// ------------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum IntervalKind {
    /// 整本地日 `[from, to]`。
    Dates { from: Date, to: Date },
    /// 任意时刻。
    Instants,
}

/// UTC 毫秒区间 `[start_ms, end_ms)`，终点已封顶到 `now`。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Interval {
    pub(crate) start_ms: i64,
    pub(crate) end_ms: i64,
    pub(crate) kind: IntervalKind,
}

impl Interval {
    pub(crate) fn is_empty(&self) -> bool {
        self.end_ms <= self.start_ms
    }
}

fn range_error(path: &str, msg: impl Into<String>) -> QueryError {
    QueryError::validation(path, msg)
}

/// 本地日 `d` 的起点（UTC 毫秒）。
pub(crate) fn day_start_ms(tz: &TimeZone, d: Date) -> Result<i64, QueryError> {
    d.to_zoned(tz.clone())
        .map(|z| z.timestamp().as_millisecond())
        .map_err(|_| range_error("date_range", "the date is out of the supported range"))
}

pub(crate) fn local_date(tz: &TimeZone, ms: i64) -> Result<Date, QueryError> {
    Timestamp::from_millisecond(ms)
        .map(|t| t.to_zoned(tz.clone()).date())
        .map_err(|_| range_error("date_range", "the time is out of the supported range"))
}

fn check_date(d: Date, path: &str) -> Result<(), QueryError> {
    if d < MIN_DATE || d > MAX_DATE {
        return Err(range_error(
            path,
            format!("dates must be between {MIN_DATE} and {MAX_DATE}"),
        ));
    }
    Ok(())
}

fn checked_days(d: Date, days: i64, path: &str) -> Result<Date, QueryError> {
    d.checked_add(Span::new().days(days))
        .map_err(|_| range_error(path, "the date is out of the supported range"))
}

fn dates_interval(tz: &TimeZone, from: Date, to: Date) -> Result<Interval, QueryError> {
    let end_day = checked_days(to, 1, "date_range")?;
    Ok(Interval {
        start_ms: day_start_ms(tz, from)?,
        end_ms: day_start_ms(tz, end_day)?,
        kind: IntervalKind::Dates { from, to },
    })
}

/// 站点本地日期 `[from, to]`（含两端）对应的 UTC 半开区间 `[start_ms, end_ms)`，与看板整日区间同一套换算
/// （本地日起点 = 当天 00:00 的本地时刻，DST 缺口按 jiff 的 compatible 规则）。统计数据按日期删除用它。
pub fn local_dates_utc_ms(tz: &TimeZone, from: Date, to: Date) -> Result<(i64, i64), QueryError> {
    let iv = dates_interval(tz, from, to)?;
    Ok((iv.start_ms, iv.end_ms))
}

fn cap(mut iv: Interval, now_ms: i64) -> Interval {
    iv.end_ms = iv.end_ms.min(now_ms.saturating_add(1));
    if iv.end_ms < iv.start_ms {
        iv.end_ms = iv.start_ms;
    }
    iv
}

fn custom_interval(tz: &TimeZone, c: &CustomRange, path: &str) -> Result<Interval, QueryError> {
    match (c.from, c.to) {
        (RangeBound::Date(from), RangeBound::Date(to)) => {
            check_date(from, &format!("{path}[0]"))?;
            check_date(to, &format!("{path}[1]"))?;
            if from > to {
                return Err(range_error(
                    path,
                    "the start date must not be after the end date",
                ));
            }
            dates_interval(tz, from, to)
        }
        (RangeBound::Instant(from), RangeBound::Instant(to)) => {
            let (f, t) = (from.as_millisecond(), to.as_millisecond());
            let min_ms = day_start_ms(tz, MIN_DATE)?;
            if f < min_ms || t < min_ms || t > day_start_ms(tz, MAX_DATE)? {
                return Err(range_error(
                    path,
                    format!("timestamps must be between {MIN_DATE} and {MAX_DATE}"),
                ));
            }
            if f > t {
                return Err(range_error(
                    path,
                    "the start time must not be after the end time",
                ));
            }
            Ok(Interval {
                start_ms: f,
                end_ms: t,
                kind: IntervalKind::Instants,
            })
        }
        _ => Err(range_error(
            path,
            "both ends must be dates, or both must be timestamps",
        )),
    }
}

/// 把 `date_range` 换算成区间。`data_start_ms`：站点最早事件的时间（`all` 用，没有数据为 `None`）。
pub(crate) fn resolve_range(
    range: &DateRange,
    tz: &TimeZone,
    now_ms: i64,
    data_start_ms: Option<i64>,
) -> Result<Interval, QueryError> {
    let today = local_date(tz, now_ms)?;
    let iv = match range {
        DateRange::Day => dates_interval(tz, today, today)?,
        DateRange::Hours24 => Interval {
            start_ms: now_ms - 24 * 3_600_000,
            end_ms: now_ms.saturating_add(1),
            kind: IntervalKind::Instants,
        },
        DateRange::Realtime => Interval {
            start_ms: now_ms - 5 * 60_000,
            end_ms: now_ms.saturating_add(1),
            kind: IntervalKind::Instants,
        },
        DateRange::Days(n) => {
            let from = checked_days(today, 1 - i64::from(*n), "date_range")?;
            dates_interval(tz, from, today)?
        }
        DateRange::Month => dates_interval(tz, today.first_of_month(), today)?,
        DateRange::Months(n) => {
            let from = today
                .first_of_month()
                .checked_sub(Span::new().months(i64::from(*n) - 1))
                .map_err(|_| range_error("date_range", "the date is out of the supported range"))?;
            dates_interval(tz, from, today)?
        }
        DateRange::Year => dates_interval(tz, today.first_of_year(), today)?,
        DateRange::All => {
            let from = match data_start_ms {
                Some(ms) => local_date(tz, ms)?.min(today),
                None => today,
            };
            dates_interval(tz, from, today)?
        }
        DateRange::Custom(c) => custom_interval(tz, c, "date_range")?,
    };
    Ok(cap(iv, now_ms))
}

/// 对比期（§6.3）。`main` 是已封顶的主区间。
///
/// - 整日区间：`previous_period` 往前平移 N 个本地日（N = 区间的本地日数，终点先裁到今天）；
///   `year_over_year` 起点减一年（2 月 29 日取 28 日），长度不变；
/// - 时刻区间：`previous_period` 往前平移区间的时长；`year_over_year` 起点按站点时区减一年，长度不变；
/// - 显式 `[from, to]` 与 `date_range` 的写法、校验相同。
///
/// 对比期的终点同样封顶到 `now`。
fn resolve_compare(
    c: &Compare,
    main: &Interval,
    tz: &TimeZone,
    now_ms: i64,
) -> Result<Interval, QueryError> {
    let path = "include.compare";
    let iv = match c {
        Compare::Custom(custom) => custom_interval(tz, custom, path)?,
        Compare::PreviousPeriod | Compare::YearOverYear => match main.kind {
            IntervalKind::Dates { from, to } => {
                let today = local_date(tz, now_ms)?;
                let last = if to > today && from <= today {
                    today
                } else {
                    to
                };
                let n = i64::from((last - from).get_days()) + 1;
                let (from2, to2) = if matches!(c, Compare::PreviousPeriod) {
                    (checked_days(from, -n, path)?, checked_days(from, -1, path)?)
                } else {
                    let f = from
                        .checked_sub(Span::new().years(1))
                        .map_err(|_| range_error(path, "the date is out of the supported range"))?;
                    (f, checked_days(f, n - 1, path)?)
                };
                check_date(from2, path)?;
                dates_interval(tz, from2, to2)?
            }
            IntervalKind::Instants => {
                let len = main.end_ms - main.start_ms;
                let start = if matches!(c, Compare::PreviousPeriod) {
                    main.start_ms - len
                } else {
                    let z = Timestamp::from_millisecond(main.start_ms)
                        .map_err(|_| range_error(path, "the time is out of the supported range"))?
                        .to_zoned(tz.clone());
                    z.checked_sub(Span::new().years(1))
                        .map_err(|_| range_error(path, "the time is out of the supported range"))?
                        .timestamp()
                        .as_millisecond()
                };
                if start < 0 {
                    return Err(range_error(
                        path,
                        "the comparison period starts before 1970",
                    ));
                }
                Interval {
                    start_ms: start,
                    end_ms: start + len,
                    kind: IntervalKind::Instants,
                }
            }
        },
    };
    Ok(cap(iv, now_ms))
}

/// 自动时间粒度（`time` 维度）：按区间覆盖的本地日数——
/// ≤ 2 日 → 小时；≤ 40 日 → 日；≤ 364 日（52 周）→ 周；否则月。
/// 与 Plausible 的 48 小时 / 40 天 / 52 周相近，但按「本地日数」而不是时长，23 / 25 小时的日子不会让「两天」变成「日」。
pub(crate) fn auto_granularity(iv: &Interval, tz: &TimeZone) -> Gran {
    let days = if iv.is_empty() {
        1
    } else {
        let first = local_date(tz, iv.start_ms);
        let last = local_date(tz, iv.end_ms - 1);
        match (first, last) {
            (Ok(f), Ok(l)) => i64::from((l - f).get_days()) + 1,
            _ => 1,
        }
    };
    match days {
        ..=2 => Gran::Hour,
        3..=40 => Gran::Day,
        41..=364 => Gran::Week,
        _ => Gran::Month,
    }
}

fn weekday_offset(date: Date, week_start: Weekday) -> i64 {
    let dow = i64::from(date.weekday().to_monday_one_offset());
    let ws = i64::from(week_start.to_monday_one_offset());
    (dow - ws + 7) % 7
}

/// `date` 所在周的起始日。
pub(crate) fn week_start_of(date: Date, week_start: Weekday) -> Date {
    date.checked_sub(Span::new().days(weekday_offset(date, week_start)))
        .expect("周起始日不会越界")
}

/// `YYYY-MM-DDTHH:MM:SS[.mmm]±HH:MM`：站点时区的本地时间带偏移，无歧义（DST 回拨的两个 02:00 可以区分）。
pub(crate) fn fmt_local(tz: &TimeZone, ms: i64) -> String {
    let Ok(ts) = Timestamp::from_millisecond(ms) else {
        return String::new();
    };
    let z = ts.to_zoned(tz.clone());
    let off = z.offset().seconds();
    let (sign, abs) = if off < 0 { ('-', -off) } else { ('+', off) };
    let frac = if z.millisecond() == 0 {
        String::new()
    } else {
        format!(".{:03}", z.millisecond())
    };
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}{frac}{sign}{:02}:{:02}",
        z.year(),
        z.month(),
        z.day(),
        z.hour(),
        z.minute(),
        z.second(),
        abs / 3600,
        (abs % 3600) / 60
    )
}

/// 分钟桶起点（UTC 毫秒）：把时刻对齐到站点本地时间的整分（UTC 偏移都是整分钟，所以与 UTC 整分一致）。
pub(crate) fn minute_floor_ms(tz: &TimeZone, ms: i64) -> i64 {
    let Ok(ts) = Timestamp::from_millisecond(ms) else {
        return ms;
    };
    let z = ts.to_zoned(tz.clone());
    ms - (i64::from(z.second()) * 1000 + i64::from(z.millisecond()))
}

/// 小时桶起点（UTC 毫秒）：把时刻对齐到站点本地时间的整点（非整点时区如 +5:45 也对齐本地整点）。
pub(crate) fn hour_floor_ms(tz: &TimeZone, ms: i64) -> i64 {
    let Ok(ts) = Timestamp::from_millisecond(ms) else {
        return ms;
    };
    let z = ts.to_zoned(tz.clone());
    let into =
        (i64::from(z.minute()) * 60 + i64::from(z.second())) * 1000 + i64::from(z.millisecond());
    ms - into
}

/// 时间标签：完整的桶序列（含空桶）。小时桶标签是 [`fmt_local`]，其余是桶起始日的 `YYYY-MM-DD`。
pub(crate) fn time_labels(
    iv: &Interval,
    gran: Gran,
    tz: &TimeZone,
    week_start: Weekday,
) -> Result<Vec<String>, QueryError> {
    let mut out = Vec::new();
    if iv.is_empty() {
        return Ok(out);
    }
    let too_many = || {
        range_error(
            "dimensions",
            format!(
                "the date range has more than {MAX_TIME_BUCKETS} time buckets; use a coarser time dimension or a shorter range"
            ),
        )
    };
    match gran {
        Gran::Minute => {
            let mut t = minute_floor_ms(tz, iv.start_ms);
            while t < iv.end_ms {
                if out.len() >= MAX_MINUTE_BUCKETS {
                    return Err(range_error(
                        "dimensions",
                        format!(
                            "the date range has more than {MAX_MINUTE_BUCKETS} `time:minute` buckets (about 24 hours); use `time:hour` or a shorter range"
                        ),
                    ));
                }
                out.push(fmt_local(tz, t));
                t += 60_000;
            }
        }
        Gran::Hour => {
            let mut t = hour_floor_ms(tz, iv.start_ms);
            while t < iv.end_ms {
                if out.len() >= MAX_TIME_BUCKETS {
                    return Err(too_many());
                }
                out.push(fmt_local(tz, t));
                t += 3_600_000;
            }
        }
        Gran::Day | Gran::Week | Gran::Month => {
            let first = local_date(tz, iv.start_ms)?;
            let mut d = match gran {
                Gran::Week => week_start_of(first, week_start),
                Gran::Month => first.first_of_month(),
                _ => first,
            };
            let step = |d: Date| -> Result<Date, QueryError> {
                match gran {
                    Gran::Month => d
                        .checked_add(Span::new().months(1))
                        .map_err(|_| range_error("date_range", "the date is out of range")),
                    Gran::Week => checked_days(d, 7, "date_range"),
                    _ => checked_days(d, 1, "date_range"),
                }
            };
            while day_start_ms(tz, d)? < iv.end_ms {
                if out.len() >= MAX_TIME_BUCKETS {
                    return Err(too_many());
                }
                out.push(d.to_string());
                d = step(d)?;
            }
        }
    }
    Ok(out)
}

// ------------------------------------------------------------------------------------------
// ResolvedQuery
// ------------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OrderKey {
    Metric(usize),
    Dim(usize),
}

/// 业务校验后的查询。字段只在 crate 内可见。
#[derive(Clone, Debug)]
pub struct ResolvedQuery {
    pub(crate) tz: TimeZone,
    pub(crate) tz_name: String,
    pub(crate) week_start: Weekday,
    pub(crate) metrics: Vec<Metric>,
    pub(crate) dims: Vec<RDim>,
    pub(crate) filter: Option<Pred>,
    pub(crate) interval: Interval,
    pub(crate) order: Vec<(OrderKey, Direction)>,
    pub(crate) limit: u32,
    pub(crate) offset: u32,
    pub(crate) want_time_labels: bool,
    pub(crate) want_total_rows: bool,
    pub(crate) compare: Option<Interval>,
    /// 回显用：规范化后的请求。
    pub(crate) echo: Echo,
    /// 结果含位置维度（回显地理署名）、或维度 / 过滤涉及位置（回显地理能力）。
    pub(crate) geo_in_dims: bool,
    pub(crate) geo_touched: bool,
    /// 站点的目标（`event:goal` 维度用；过滤叶子里已经解析好了）。
    pub(crate) goals: Vec<GoalDef>,
}

/// `query` 回显里由 resolve 决定的部分（日期范围与时间粒度已展开）。
#[derive(Clone, Debug)]
pub(crate) struct Echo {
    pub(crate) site: Option<String>,
    pub(crate) date_range: Value,
    pub(crate) dimensions: Vec<String>,
    pub(crate) filters: Vec<Value>,
    pub(crate) order_by: Vec<Value>,
    pub(crate) include: Value,
    pub(crate) pagination: Value,
}

impl ResolvedQuery {
    pub(crate) fn time_dim(&self) -> Option<(usize, Gran)> {
        self.dims.iter().enumerate().find_map(|(i, d)| match d {
            RDim::Time(g) => Some((i, *g)),
            RDim::Col(_) => None,
        })
    }

    pub(crate) fn time_labels_for(&self, iv: &Interval) -> Result<Option<Vec<String>>, QueryError> {
        match self.time_dim() {
            Some((_, g)) => time_labels(iv, g, &self.tz, self.week_start).map(Some),
            None => Ok(None),
        }
    }

    /// 解析并校验。`data_start_ms` 只在 `date_range = all` 时需要（调用方先查出站点最早事件）。
    pub(crate) fn build(
        spec: &QuerySpec,
        site: &SiteContext,
        now: Timestamp,
        data_start_ms: Option<i64>,
    ) -> Result<ResolvedQuery, QueryError> {
        let tz = site.timezone.clone();
        let now_ms = now.as_millisecond();

        if let (Some(want), Some(have)) = (&spec.site, &site.site_key)
            && want != have
        {
            return Err(QueryError::validation(
                "site",
                "does not match the site of the request context",
            ));
        }

        // ---- 指标 ----
        if spec.metrics.is_empty() {
            return Err(QueryError::validation(
                "metrics",
                "at least one metric is required",
            ));
        }
        if spec.metrics.len() > MAX_METRICS {
            return Err(QueryError::validation(
                "metrics",
                format!("at most {MAX_METRICS} metrics are allowed"),
            ));
        }
        let mut seen = BTreeSet::new();
        for (i, m) in spec.metrics.iter().enumerate() {
            if !m.is_supported() {
                return Err(QueryError::validation(
                    format!("metrics[{i}]"),
                    format!("`{}` is not supported yet", m.as_str()),
                ));
            }
            if !seen.insert(m.as_str()) {
                return Err(QueryError::validation(
                    format!("metrics[{i}]"),
                    format!("duplicate metric `{}`", m.as_str()),
                ));
            }
        }

        // ---- 日期范围 ----
        let main_iv = resolve_range(&spec.date_range, &tz, now_ms, data_start_ms)?;

        // ---- 维度 ----
        if spec.dimensions.len() > MAX_DIMENSIONS {
            return Err(QueryError::validation(
                "dimensions",
                format!("at most {MAX_DIMENSIONS} dimensions are allowed"),
            ));
        }
        let mut dims = Vec::with_capacity(spec.dimensions.len());
        let mut dim_names: Vec<String> = Vec::new();
        let mut seen_dims = BTreeSet::new();
        let mut time_seen = false;
        for (j, d) in spec.dimensions.iter().enumerate() {
            let path = format!("dimensions[{j}]");
            if !d.is_supported() {
                return Err(QueryError::validation(
                    path,
                    format!("`{d}` is not supported yet"),
                ));
            }
            if !seen_dims.insert(d.as_str().into_owned()) {
                return Err(QueryError::validation(
                    path,
                    format!("duplicate dimension `{d}`"),
                ));
            }
            if d.is_time() {
                if time_seen {
                    return Err(QueryError::validation(
                        path,
                        "only one time dimension is allowed",
                    ));
                }
                time_seen = true;
                let g = match d {
                    Dimension::Time => auto_granularity(&main_iv, &tz),
                    Dimension::TimeMinute => Gran::Minute,
                    Dimension::TimeHour => Gran::Hour,
                    Dimension::TimeDay => Gran::Day,
                    Dimension::TimeWeek => Gran::Week,
                    Dimension::TimeMonth => Gran::Month,
                    _ => unreachable!("不支持的时间维度已被拒绝"),
                };
                dims.push(RDim::Time(g));
                dim_names.push(g.dimension_name().to_owned());
            } else {
                dims.push(RDim::Col(
                    Col::from_dimension(d).expect("支持的非时间维度都有列"),
                ));
                dim_names.push(d.as_str().into_owned());
            }
        }

        // ---- 过滤 ----
        let mut leaves = 0usize;
        let mut first_event_leaf: Option<String> = None;
        let mut parts = Vec::with_capacity(spec.filters.len());
        for (i, f) in spec.filters.iter().enumerate() {
            parts.push(build_pred(
                f,
                &format!("filters[{i}]"),
                &mut leaves,
                &mut first_event_leaf,
                &site.goals,
            )?);
        }
        let filter = match parts.len() {
            0 => None,
            1 => parts.pop(),
            _ => Some(Pred::And(parts)),
        };

        // ---- 组合校验（§6.4 的「不兼容组合直接报错」）----
        validate_combinations(spec, &dims, filter.as_ref(), first_event_leaf.as_deref())?;

        // 时间桶数上限（也校验自动粒度）
        let time_dim = dims.iter().find_map(|d| match d {
            RDim::Time(g) => Some(*g),
            RDim::Col(_) => None,
        });
        if let Some(g) = time_dim {
            time_labels(&main_iv, g, &tz, site.week_starts_on)?;
        }
        if spec.include.time_labels && time_dim.is_none() {
            return Err(QueryError::validation(
                "include.time_labels",
                "requires a time dimension (`time`, `time:minute`, `time:hour`, `time:day`, `time:week` or `time:month`)",
            ));
        }

        // ---- 对比期 ----
        let compare = match &spec.include.compare {
            Some(c) => {
                let iv = resolve_compare(c, &main_iv, &tz, now_ms)?;
                if let Some(g) = time_dim {
                    time_labels(&iv, g, &tz, site.week_starts_on).map_err(|_| {
                        QueryError::validation(
                            "include.compare",
                            "the comparison period has too many time buckets",
                        )
                    })?;
                }
                Some(iv)
            }
            None => None,
        };

        // ---- 排序 ----
        let (order, order_echo) = resolve_order(spec, &dims, &dim_names, time_dim)?;

        // ---- 分页 ----
        if spec.pagination.limit == 0 || spec.pagination.limit > crate::spec::MAX_LIMIT {
            return Err(QueryError::validation(
                "pagination.limit",
                format!("must be between 1 and {}", crate::spec::MAX_LIMIT),
            ));
        }

        let geo_in_dims = dims
            .iter()
            .any(|d| matches!(d, RDim::Col(Col::Country | Col::Region | Col::City)));
        let geo_touched = geo_in_dims || filter.as_ref().is_some_and(|f| f.has_geo_leaf());

        let echo = Echo {
            site: spec.site.clone(),
            date_range: spec.date_range.to_json(),
            dimensions: dim_names,
            filters: spec.filters.iter().map(Filter::to_json).collect(),
            order_by: order_echo,
            include: json!({
                "time_labels": spec.include.time_labels,
                "total_rows": spec.include.total_rows,
                "compare": spec.include.compare.as_ref().map(Compare::to_json),
            }),
            pagination: json!({"limit": spec.pagination.limit, "offset": spec.pagination.offset}),
        };

        Ok(ResolvedQuery {
            tz_name: tz.iana_name().unwrap_or("UTC").to_owned(),
            tz,
            week_start: site.week_starts_on,
            metrics: spec.metrics.clone(),
            dims,
            filter,
            interval: main_iv,
            order,
            limit: spec.pagination.limit,
            offset: spec.pagination.offset,
            want_time_labels: spec.include.time_labels,
            want_total_rows: spec.include.total_rows,
            compare,
            echo,
            geo_in_dims,
            geo_touched,
            goals: site.goals.clone(),
        })
    }
}

const SESSION_METRICS: [Metric; 4] = [
    Metric::BounceRate,
    Metric::VisitDuration,
    Metric::EngagedDuration,
    Metric::ViewsPerVisit,
];

/// 能与 `event:goal` 维度同用的指标。
const GOAL_DIM_METRICS: [Metric; 5] = [
    Metric::Visitors,
    Metric::Visits,
    Metric::Events,
    Metric::Pageviews,
    Metric::ConversionRate,
];

fn validate_combinations(
    spec: &QuerySpec,
    dims: &[RDim],
    filter: Option<&Pred>,
    first_event_leaf: Option<&str>,
) -> Result<(), QueryError> {
    let metric_index = |m: Metric| spec.metrics.iter().position(|x| *x == m);
    let has_time = dims.iter().any(|d| matches!(d, RDim::Time(_)));
    let dim_index = |col: Col| {
        dims.iter()
            .position(|d| matches!(d, RDim::Col(c) if *c == col))
    };

    if let Some(i) = metric_index(Metric::Percentage) {
        if dims.is_empty() {
            return Err(QueryError::validation(
                format!("metrics[{i}]"),
                "`percentage` requires at least one dimension",
            ));
        }
        if has_time {
            return Err(QueryError::validation(
                format!("metrics[{i}]"),
                "`percentage` cannot be combined with time dimensions",
            ));
        }
    }

    if let Some(i) = metric_index(Metric::ExitRate) {
        if !matches!(dims, [RDim::Col(Col::EventPage)]) {
            return Err(QueryError::validation(
                format!("metrics[{i}]"),
                "`exit_rate` requires exactly one dimension, `event:page`",
            ));
        }
        if let Some(path) = first_event_leaf {
            return Err(QueryError::validation(
                path,
                "event-level filters (event:page, event:hostname, event:name, event:goal) cannot be combined with `exit_rate`",
            ));
        }
    }

    for m in [Metric::TimeOnPage, Metric::ScrollDepth] {
        if let Some(i) = metric_index(m) {
            if let Some(j) = dim_index(Col::EventName) {
                return Err(QueryError::validation(
                    format!("dimensions[{j}]"),
                    format!(
                        "`event:name` cannot be combined with `{}` (it is computed from engagement events)",
                        m.as_str()
                    ),
                ));
            }
            if filter.is_some_and(|f| f.has_leaf_on(Col::EventName)) {
                return Err(QueryError::validation(
                    first_event_leaf.unwrap_or("filters"),
                    format!(
                        "`event:name` filters cannot be combined with `{}` (it is computed from engagement events)",
                        m.as_str()
                    ),
                ));
            }
            // 目标只由非 engagement 事件完成：在 engagement 事件上恒不成立
            if let Some(j) = dim_index(Col::EventGoal) {
                return Err(QueryError::validation(
                    format!("dimensions[{j}]"),
                    format!(
                        "`event:goal` cannot be combined with `{}` (it is computed from engagement events)",
                        m.as_str()
                    ),
                ));
            }
            if filter.is_some_and(|f| f.has_leaf_on(Col::EventGoal)) {
                return Err(QueryError::validation(
                    first_event_leaf.unwrap_or("filters"),
                    format!(
                        "`event:goal` filters cannot be combined with `{}` (it is computed from engagement events)",
                        m.as_str()
                    ),
                ));
            }
            let _ = i;
        }
    }

    // `event:goal` 维度：每行是一个目标的完成情况，只配事件形式的计数与转化率
    if let Some(j) = dim_index(Col::EventGoal)
        && let Some(m) = spec.metrics.iter().find(|m| !GOAL_DIM_METRICS.contains(m))
    {
        return Err(QueryError::validation(
            format!("dimensions[{j}]"),
            format!(
                "`event:goal` can only be combined with the metrics visitors, visits, events, pageviews and conversion_rate (got `{}`)",
                m.as_str()
            ),
        ));
    }
    if let Some(i) = metric_index(Metric::ConversionRate)
        && dim_index(Col::EventGoal).is_none()
        && !filter.is_some_and(|f| f.has_leaf_on(Col::EventGoal))
    {
        return Err(QueryError::validation(
            format!("metrics[{i}]"),
            "`conversion_rate` requires an `event:goal` dimension or an `event:goal` filter",
        ));
    }

    if let Some(j) = dim_index(Col::EventName)
        && let Some(m) = SESSION_METRICS.iter().find(|m| spec.metrics.contains(m))
    {
        return Err(QueryError::validation(
            format!("dimensions[{j}]"),
            format!(
                "`event:name` cannot be combined with the session metric `{}`",
                m.as_str()
            ),
        ));
    }
    Ok(())
}

/// 解析后的排序键与回显。
type ResolvedOrder = (Vec<(OrderKey, Direction)>, Vec<Value>);

fn resolve_order(
    spec: &QuerySpec,
    dims: &[RDim],
    dim_names: &[String],
    time_dim: Option<Gran>,
) -> Result<ResolvedOrder, QueryError> {
    let mut out: Vec<(OrderKey, Direction)> = Vec::new();
    let mut echo = Vec::new();
    let mut seen = BTreeSet::new();
    for (k, ob) in spec.order_by.iter().enumerate() {
        let path = format!("order_by[{k}][0]");
        let metric = spec
            .metrics
            .iter()
            .position(|m| m.as_str() == ob.key)
            .map(OrderKey::Metric);
        let dim = spec
            .dimensions
            .iter()
            .position(|d| d.as_str() == ob.key)
            .map(OrderKey::Dim);
        let Some(key) = metric.or(dim) else {
            return Err(QueryError::validation(
                path,
                "must be one of the requested metrics or dimensions",
            ));
        };
        if !seen.insert(ob.key.clone()) {
            return Err(QueryError::validation(path, "duplicate order_by key"));
        }
        out.push((key, ob.direction));
        let shown = match key {
            OrderKey::Dim(j) => dim_names[j].clone(),
            OrderKey::Metric(_) => ob.key.clone(),
        };
        echo.push(
            crate::spec::OrderBy {
                key: shown,
                direction: ob.direction,
            }
            .to_json(),
        );
    }
    if out.is_empty() {
        // 默认：有时间维度先按时间升序，再按第一个指标降序；其余按第一个指标降序。
        if time_dim.is_some() {
            let j = dims
                .iter()
                .position(|d| matches!(d, RDim::Time(_)))
                .expect("有时间维度");
            out.push((OrderKey::Dim(j), Direction::Asc));
            echo.push(
                crate::spec::OrderBy {
                    key: dim_names[j].clone(),
                    direction: Direction::Asc,
                }
                .to_json(),
            );
        }
        out.push((OrderKey::Metric(0), Direction::Desc));
        echo.push(
            crate::spec::OrderBy {
                key: spec.metrics[0].as_str().to_owned(),
                direction: Direction::Desc,
            }
            .to_json(),
        );
    }
    Ok((out, echo))
}

#[cfg(test)]
mod tests {
    use jiff::civil::date;

    use super::*;

    fn tz(name: &str) -> TimeZone {
        TimeZone::get(name).unwrap()
    }

    fn ms(tz: &TimeZone, y: i16, mo: i8, d: i8, h: i8, mi: i8) -> i64 {
        date(y, mo, d)
            .at(h, mi, 0, 0)
            .to_zoned(tz.clone())
            .unwrap()
            .timestamp()
            .as_millisecond()
    }

    fn iv(range: DateRange, tz: &TimeZone, now_ms: i64) -> Interval {
        resolve_range(&range, tz, now_ms, None).unwrap()
    }

    fn dates(i: &Interval) -> (Date, Date) {
        match i.kind {
            IntervalKind::Dates { from, to } => (from, to),
            IntervalKind::Instants => panic!("不是整日区间"),
        }
    }

    #[test]
    fn seven_days_end_today_and_are_capped_at_now() {
        let berlin = tz("Europe/Berlin");
        // 2026-04-02 (周四) 15:00 柏林
        let now = ms(&berlin, 2026, 4, 2, 15, 0);
        let r = iv(DateRange::Days(7), &berlin, now);
        assert_eq!(dates(&r), (date(2026, 3, 27), date(2026, 4, 2)));
        assert_eq!(r.start_ms, ms(&berlin, 2026, 3, 27, 0, 0));
        assert_eq!(r.end_ms, now + 1, "含今天，终点封顶到 now");
        // 跨越 3/29 的夏令时切换：6 个整本地日 = 6*24 - 1 小时，加上今天的 15 小时
        assert_eq!(r.end_ms - 1 - r.start_ms, (6 * 24 - 1 + 15) * 3_600_000);
        assert_eq!(
            dates(&iv(DateRange::Days(1), &berlin, now)),
            (date(2026, 4, 2), date(2026, 4, 2))
        );
    }

    #[test]
    fn day_month_year_end_today_and_are_capped_at_now() {
        let berlin = tz("Europe/Berlin");
        let now = ms(&berlin, 2026, 10, 23, 15, 30);
        let d = iv(DateRange::Day, &berlin, now);
        assert_eq!(dates(&d), (date(2026, 10, 23), date(2026, 10, 23)));
        assert_eq!(d.end_ms, now + 1, "未来没有数据：终点封顶到 now");
        let m = iv(DateRange::Month, &berlin, now);
        assert_eq!(dates(&m), (date(2026, 10, 1), date(2026, 10, 23)));
        let y = iv(DateRange::Year, &berlin, now);
        assert_eq!(dates(&y), (date(2026, 1, 1), date(2026, 10, 23)));
        assert_eq!(y.end_ms, now + 1);
    }

    #[test]
    fn months_end_today_and_include_this_month() {
        let berlin = tz("Europe/Berlin");
        let now = ms(&berlin, 2026, 3, 31, 9, 0);
        let one = iv(DateRange::Months(1), &berlin, now);
        assert_eq!(dates(&one), (date(2026, 3, 1), date(2026, 3, 31)));
        assert_eq!(one.end_ms, now + 1, "含本月，终点封顶到 now");
        assert_eq!(
            dates(&iv(DateRange::Months(6), &berlin, now)),
            (date(2025, 10, 1), date(2026, 3, 31))
        );
        let now = ms(&berlin, 2026, 1, 15, 9, 0);
        assert_eq!(
            dates(&iv(DateRange::Months(12), &berlin, now)),
            (date(2025, 2, 1), date(2026, 1, 15))
        );
    }

    #[test]
    fn rolling_ranges_use_instants() {
        let berlin = tz("Europe/Berlin");
        let now = ms(&berlin, 2026, 3, 29, 12, 0); // 夏令时切换日
        let h = iv(DateRange::Hours24, &berlin, now);
        assert_eq!((h.start_ms, h.end_ms), (now - 24 * 3_600_000, now + 1));
        assert_eq!(h.kind, IntervalKind::Instants);
        let r = iv(DateRange::Realtime, &berlin, now);
        assert_eq!((r.start_ms, r.end_ms), (now - 300_000, now + 1));
    }

    #[test]
    fn local_days_follow_dst_in_both_directions() {
        let berlin = tz("Europe/Berlin");
        let far = ms(&berlin, 2027, 1, 1, 0, 0);
        let day = |y, m, d| {
            let r = iv(
                DateRange::Custom(CustomRange {
                    from: RangeBound::Date(date(y, m, d)),
                    to: RangeBound::Date(date(y, m, d)),
                }),
                &berlin,
                far,
            );
            (r.end_ms - r.start_ms) / 3_600_000
        };
        assert_eq!(day(2026, 3, 28), 24);
        assert_eq!(day(2026, 3, 29), 23);
        assert_eq!(day(2026, 10, 25), 25);
        // 圣保罗在午夜跳过 DST 缺口：2018-11-04 只有 23 小时
        let sp = tz("America/Sao_Paulo");
        let r = iv(
            DateRange::Custom(CustomRange {
                from: RangeBound::Date(date(2018, 11, 4)),
                to: RangeBound::Date(date(2018, 11, 4)),
            }),
            &sp,
            ms(&sp, 2027, 1, 1, 0, 0),
        );
        assert_eq!((r.end_ms - r.start_ms) / 3_600_000, 23);
    }

    #[test]
    fn custom_ranges_validate_order_and_bounds() {
        let utc = TimeZone::UTC;
        let now = ms(&utc, 2026, 6, 1, 0, 0);
        let custom = |a: RangeBound, b: RangeBound| {
            resolve_range(
                &DateRange::Custom(CustomRange { from: a, to: b }),
                &utc,
                now,
                None,
            )
        };
        assert!(
            custom(
                RangeBound::Date(date(2026, 3, 2)),
                RangeBound::Date(date(2026, 3, 1))
            )
            .is_err()
        );
        assert!(
            custom(
                RangeBound::Date(date(1960, 1, 1)),
                RangeBound::Date(date(1960, 1, 2))
            )
            .is_err()
        );
        let t = |s: &str| RangeBound::Instant(s.parse().unwrap());
        assert!(custom(t("2026-03-02T00:00:00Z"), t("2026-03-01T00:00:00Z")).is_err());
        let ok = custom(
            t("2026-03-01T10:00:00+01:00"),
            t("2026-03-01T12:00:00+01:00"),
        )
        .unwrap();
        assert_eq!(ok.end_ms - ok.start_ms, 2 * 3_600_000);
        // 未来的范围：终点封顶到 now，区间为空
        let future = custom(
            RangeBound::Date(date(2030, 1, 1)),
            RangeBound::Date(date(2030, 1, 31)),
        )
        .unwrap();
        assert!(future.is_empty());
    }

    #[test]
    fn auto_granularity_rules() {
        let utc = TimeZone::UTC;
        let now = ms(&utc, 2026, 10, 23, 12, 0);
        let g = |r| auto_granularity(&iv(r, &utc, now), &utc);
        assert_eq!(g(DateRange::Day), Gran::Hour);
        assert_eq!(g(DateRange::Hours24), Gran::Hour);
        assert_eq!(g(DateRange::Realtime), Gran::Hour);
        assert_eq!(g(DateRange::Days(2)), Gran::Hour);
        assert_eq!(g(DateRange::Days(3)), Gran::Day);
        assert_eq!(g(DateRange::Days(40)), Gran::Day);
        assert_eq!(g(DateRange::Days(41)), Gran::Week);
        assert_eq!(g(DateRange::Days(364)), Gran::Week);
        assert_eq!(g(DateRange::Days(365)), Gran::Month);
        // 含本月的 12 个月在 10/23 只有 357 天
        assert_eq!(g(DateRange::Months(12)), Gran::Week);
        assert_eq!(g(DateRange::Months(13)), Gran::Month);
        // 两个本地日跨 DST：仍是「两日」，用小时
        let berlin = tz("Europe/Berlin");
        let now = ms(&berlin, 2026, 3, 30, 12, 0);
        let r = resolve_range(&DateRange::Days(2), &berlin, now, None).unwrap();
        assert_eq!(auto_granularity(&r, &berlin), Gran::Hour);
    }

    #[test]
    fn week_buckets_follow_the_site_week_start() {
        // 2026-03-29 是周日
        let d = date(2026, 3, 29);
        assert_eq!(week_start_of(d, Weekday::Monday), date(2026, 3, 23));
        assert_eq!(week_start_of(d, Weekday::Sunday), date(2026, 3, 29));
        assert_eq!(week_start_of(d, Weekday::Saturday), date(2026, 3, 28));
        assert_eq!(
            week_start_of(date(2026, 3, 28), Weekday::Sunday),
            date(2026, 3, 22)
        );
        let utc = TimeZone::UTC;
        let now = ms(&utc, 2026, 4, 10, 0, 0);
        let r = iv(
            DateRange::Custom(CustomRange {
                from: RangeBound::Date(date(2026, 3, 25)),
                to: RangeBound::Date(date(2026, 4, 8)),
            }),
            &utc,
            now,
        );
        let mon = time_labels(&r, Gran::Week, &utc, Weekday::Monday).unwrap();
        assert_eq!(mon, ["2026-03-23", "2026-03-30", "2026-04-06"]);
        let sun = time_labels(&r, Gran::Week, &utc, Weekday::Sunday).unwrap();
        assert_eq!(sun, ["2026-03-22", "2026-03-29", "2026-04-05"]);
    }

    #[test]
    fn time_labels_cover_empty_buckets_and_dst_hours() {
        let berlin = tz("Europe/Berlin");
        let now = ms(&berlin, 2026, 11, 1, 0, 0);
        let day = |y, m, d| {
            iv(
                DateRange::Custom(CustomRange {
                    from: RangeBound::Date(date(y, m, d)),
                    to: RangeBound::Date(date(y, m, d)),
                }),
                &berlin,
                now,
            )
        };
        let spring = time_labels(&day(2026, 3, 29), Gran::Hour, &berlin, Weekday::Monday).unwrap();
        assert_eq!(spring.len(), 23);
        assert_eq!(spring[1], "2026-03-29T01:00:00+01:00");
        assert_eq!(spring[2], "2026-03-29T03:00:00+02:00", "02:00 不存在");
        let fall = time_labels(&day(2026, 10, 25), Gran::Hour, &berlin, Weekday::Monday).unwrap();
        assert_eq!(fall.len(), 25);
        assert_eq!(fall[2], "2026-10-25T02:00:00+02:00");
        assert_eq!(
            fall[3], "2026-10-25T02:00:00+01:00",
            "回拨的两个 02:00 可以区分"
        );
        // 非整点时区：本地整点
        let kt = tz("Asia/Kathmandu");
        let r = iv(
            DateRange::Custom(CustomRange {
                from: RangeBound::Instant("2026-03-01T00:20:00Z".parse().unwrap()),
                to: RangeBound::Instant("2026-03-01T02:00:00Z".parse().unwrap()),
            }),
            &kt,
            now,
        );
        let labels = time_labels(&r, Gran::Hour, &kt, Weekday::Monday).unwrap();
        // 00:20Z 是本地 06:05：首桶是本地 06:00
        assert_eq!(
            labels,
            ["2026-03-01T06:00:00+05:45", "2026-03-01T07:00:00+05:45"]
        );
        // 月与日
        let r = iv(
            DateRange::Months(3),
            &berlin,
            ms(&berlin, 2026, 4, 15, 0, 0),
        );
        assert_eq!(
            time_labels(&r, Gran::Month, &berlin, Weekday::Monday).unwrap(),
            ["2026-02-01", "2026-03-01", "2026-04-01"]
        );
        assert_eq!(
            time_labels(&r, Gran::Day, &berlin, Weekday::Monday)
                .unwrap()
                .len(),
            28 + 31 + 15
        );
        // 空区间没有标签
        let empty = iv(DateRange::Year, &berlin, ms(&berlin, 2025, 12, 31, 0, 0));
        assert!(!empty.is_empty());
        let future = resolve_range(
            &DateRange::Custom(CustomRange {
                from: RangeBound::Date(date(2030, 1, 1)),
                to: RangeBound::Date(date(2030, 1, 2)),
            }),
            &berlin,
            now,
            None,
        )
        .unwrap();
        assert!(
            time_labels(&future, Gran::Day, &berlin, Weekday::Monday)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn too_many_buckets_is_a_validation_error() {
        let utc = TimeZone::UTC;
        let now = ms(&utc, 2026, 10, 23, 12, 0);
        // 5000 天按日是 5000 个桶，按小时是 12 万个
        let r = iv(DateRange::Days(5000), &utc, now);
        assert_eq!(
            time_labels(&r, Gran::Day, &utc, Weekday::Monday)
                .unwrap()
                .len(),
            5000
        );
        let e = time_labels(&r, Gran::Hour, &utc, Weekday::Monday).unwrap_err();
        assert!(e.as_validation().unwrap().1.contains("time buckets"));
        let r = iv(DateRange::Days(1000), &utc, now);
        assert!(time_labels(&r, Gran::Hour, &utc, Weekday::Monday).is_err());
        assert_eq!(
            time_labels(&r, Gran::Week, &utc, Weekday::Monday)
                .unwrap()
                .len(),
            144
        );
    }

    #[test]
    fn minute_labels_and_bucket_cap() {
        let berlin = tz("Europe/Berlin");
        let now = ms(&berlin, 2026, 10, 24, 15, 42) + 17_000; // 15:42:17
        // realtime：[now - 5min, now]，首桶对齐本地整分
        let r = iv(DateRange::Realtime, &berlin, now);
        let labels = time_labels(&r, Gran::Minute, &berlin, Weekday::Monday).unwrap();
        assert_eq!(labels.len(), 6);
        assert_eq!(labels[0], "2026-10-24T15:37:00+02:00");
        assert_eq!(labels[5], "2026-10-24T15:42:00+02:00");
        // 24h：起点不在整分，多跨一个桶（1441），仍在上限内
        let h = iv(DateRange::Hours24, &berlin, now);
        assert_eq!(
            time_labels(&h, Gran::Minute, &berlin, Weekday::Monday)
                .unwrap()
                .len(),
            1441
        );
        // 非整点时区：偏移带 :45，分钟对齐本地整分
        let kt = tz("Asia/Kathmandu");
        let r = iv(
            DateRange::Custom(CustomRange {
                from: RangeBound::Instant("2026-03-01T00:20:30Z".parse().unwrap()),
                to: RangeBound::Instant("2026-03-01T00:23:00Z".parse().unwrap()),
            }),
            &kt,
            now,
        );
        assert_eq!(
            time_labels(&r, Gran::Minute, &kt, Weekday::Monday).unwrap(),
            [
                "2026-03-01T06:05:00+05:45",
                "2026-03-01T06:06:00+05:45",
                "2026-03-01T06:07:00+05:45"
            ]
        );
        // DST 回拨日 25 小时 = 1500 个桶，正好在上限内；回拨的两个 02:00 可以区分
        let fall = iv(
            DateRange::Custom(CustomRange {
                from: RangeBound::Date(date(2026, 10, 25)),
                to: RangeBound::Date(date(2026, 10, 25)),
            }),
            &berlin,
            ms(&berlin, 2026, 11, 1, 0, 0),
        );
        let l = time_labels(&fall, Gran::Minute, &berlin, Weekday::Monday).unwrap();
        assert_eq!(l.len(), MAX_MINUTE_BUCKETS);
        assert_eq!(l[179], "2026-10-25T02:59:00+02:00");
        assert_eq!(l[180], "2026-10-25T02:00:00+01:00");
        // 超过上限：校验错误
        let two_days = iv(DateRange::Days(2), &berlin, now);
        let e = time_labels(&two_days, Gran::Minute, &berlin, Weekday::Monday).unwrap_err();
        let (path, msg) = e.as_validation().unwrap();
        assert_eq!(path, "dimensions");
        assert!(msg.contains("time:minute"), "{msg}");
    }

    #[test]
    fn previous_period_and_year_over_year() {
        let berlin = tz("Europe/Berlin");
        let now = ms(&berlin, 2026, 10, 23, 12, 0);
        let main = iv(DateRange::Days(7), &berlin, now); // 10/17..10/23
        let prev = resolve_compare(&Compare::PreviousPeriod, &main, &berlin, now).unwrap();
        assert_eq!(dates(&prev), (date(2026, 10, 10), date(2026, 10, 16)));
        assert_eq!(prev.end_ms, main.start_ms);
        let yoy = resolve_compare(&Compare::YearOverYear, &main, &berlin, now).unwrap();
        assert_eq!(dates(&yoy), (date(2025, 10, 17), date(2025, 10, 23)));
        // 本月（到今天）：N = 23，前一个同样长度的区间
        let month = iv(DateRange::Month, &berlin, now);
        let prev = resolve_compare(&Compare::PreviousPeriod, &month, &berlin, now).unwrap();
        assert_eq!(dates(&prev), (date(2026, 9, 8), date(2026, 9, 30)));
        // 相邻：对比期的终点正好是主区间的起点
        assert_eq!(prev.end_ms, month.start_ms);
        // 时刻区间：平移时长
        let h = iv(DateRange::Hours24, &berlin, now);
        let prev = resolve_compare(&Compare::PreviousPeriod, &h, &berlin, now).unwrap();
        assert_eq!(prev.end_ms, h.start_ms);
        assert_eq!(prev.end_ms - prev.start_ms, h.end_ms - h.start_ms);
        // 2 月 29 日起点的同比
        let leap = resolve_range(
            &DateRange::Custom(CustomRange {
                from: RangeBound::Date(date(2024, 2, 29)),
                to: RangeBound::Date(date(2024, 3, 2)),
            }),
            &berlin,
            now,
            None,
        )
        .unwrap();
        let yoy = resolve_compare(&Compare::YearOverYear, &leap, &berlin, now).unwrap();
        assert_eq!(dates(&yoy), (date(2023, 2, 28), date(2023, 3, 2)));
    }

    #[test]
    fn wildcards_become_anchored_escaped_regexes() {
        assert_eq!(wildcard_to_regex("/blog/*"), "\\A/blog/.*\\z");
        assert_eq!(wildcard_to_regex("/a**b"), "\\A/a.*b\\z");
        assert_eq!(wildcard_to_regex("a.b?c"), "\\Aa\\.b\\?c\\z");
        assert_eq!(wildcard_to_regex("*"), "\\A.*\\z");
        assert_eq!(wildcard_to_regex("(x|y)+"), "\\A\\(x\\|y\\)\\+\\z");
        let re = regex::Regex::new(&wildcard_to_regex("/docs/*/intro")).unwrap();
        assert!(re.is_match("/docs/a/b/intro") && re.is_match("/docs//intro"));
        assert!(!re.is_match("/docs/intro") && !re.is_match("x/docs/a/intro"));
    }
}
