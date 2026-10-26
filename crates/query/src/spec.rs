//! `QuerySpec`：公开 API 与看板共用的查询请求（`system-design.md` §6.2）。
//!
//! 这一层只做**语法**检查，不访问数据库，也不知道站点：
//! - 所有结构 `deny_unknown_fields`，维度、指标、操作符都是封闭集合；
//! - 过滤树、`date_range`、`order_by`、`include.compare` 用与 Plausible v2 相同的数组写法，
//!   由手写的 (反)序列化实现，并手写对应的 JSON Schema；
//! - [`QuerySpec::parse`] 返回带**字段路径**的校验错误（`filters[0][2][1]`）；
//! - 业务校验（指标与维度的组合、时间范围换算、正则限制、「尚未支持」的功能）在 [`crate::resolve`]。
//!
//! 「尚未支持」的取值（`group_conversion_rate`、`event:props:<k>`、`has_done` 等）能解析成功，
//! 在 resolve 阶段才返回明确的 `not supported yet` 校验错误，这样 schema 与 Plausible 的取值集合一致。

use std::borrow::Cow;
use std::fmt;

use jiff::Timestamp;
use jiff::civil::Date;
use schemars::{JsonSchema, Schema, SchemaGenerator};
use serde::de::{self, Deserializer};
use serde::ser::Serializer;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::error::QueryError;

/// 默认与最大的 `pagination.limit`。
pub const MAX_LIMIT: u32 = 10_000;
/// `Nd` 的 N 上限与 `Nmo` 的 N 上限（Plausible 的取值）。
pub const MAX_RANGE_DAYS: u32 = 5000;
pub const MAX_RANGE_MONTHS: u32 = 100;
/// 过滤树的最大嵌套深度（顶层数组不计）。
pub const MAX_FILTER_DEPTH: usize = 8;

// ------------------------------------------------------------------------------------------
// 路径标记：自定义反序列化把「相对子路径」与消息编码进错误文本，`QuerySpec::parse` 再拆开。
// ------------------------------------------------------------------------------------------

const MARK_START: char = '\u{1}';
const MARK_END: char = '\u{2}';

fn marked(sub_path: &str, message: &str) -> String {
    format!("{MARK_START}{sub_path}{MARK_END}{message}")
}

/// 拆出 `(子路径, 消息)`；没有标记时子路径为空。
fn unmark(text: &str) -> (&str, &str) {
    if let Some(rest) = text.strip_prefix(MARK_START)
        && let Some((sub, msg)) = rest.split_once(MARK_END)
    {
        return (sub, msg);
    }
    ("", text)
}

/// 自定义反序列化内部使用的错误：`path` 相对于该值本身。
#[derive(Debug)]
struct SubError {
    path: String,
    message: String,
}

impl SubError {
    fn at(path: impl Into<String>, message: impl Into<String>) -> Self {
        SubError {
            path: path.into(),
            message: message.into(),
        }
    }

    fn prefixed(self, prefix: &str) -> Self {
        SubError {
            path: format!("{prefix}{}", self.path),
            message: self.message,
        }
    }

    fn into_de<E: de::Error>(self) -> E {
        E::custom(marked(&self.path, &self.message))
    }
}

// ------------------------------------------------------------------------------------------
// 指标
// ------------------------------------------------------------------------------------------

/// 指标。`group_conversion_rate` 与收入（`total_revenue` `average_revenue`）在本轮不支持：能解析，resolve 阶段报 `not supported yet`。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Metric {
    Visitors,
    Visits,
    Pageviews,
    Events,
    ViewsPerVisit,
    BounceRate,
    VisitDuration,
    EngagedDuration,
    Percentage,
    ExitRate,
    TimeOnPage,
    ScrollDepth,
    ConversionRate,
    GroupConversionRate,
    TotalRevenue,
    AverageRevenue,
}

impl Metric {
    pub const ALL: [Metric; 16] = [
        Metric::Visitors,
        Metric::Visits,
        Metric::Pageviews,
        Metric::Events,
        Metric::ViewsPerVisit,
        Metric::BounceRate,
        Metric::VisitDuration,
        Metric::EngagedDuration,
        Metric::Percentage,
        Metric::ExitRate,
        Metric::TimeOnPage,
        Metric::ScrollDepth,
        Metric::ConversionRate,
        Metric::GroupConversionRate,
        Metric::TotalRevenue,
        Metric::AverageRevenue,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Metric::Visitors => "visitors",
            Metric::Visits => "visits",
            Metric::Pageviews => "pageviews",
            Metric::Events => "events",
            Metric::ViewsPerVisit => "views_per_visit",
            Metric::BounceRate => "bounce_rate",
            Metric::VisitDuration => "visit_duration",
            Metric::EngagedDuration => "engaged_duration",
            Metric::Percentage => "percentage",
            Metric::ExitRate => "exit_rate",
            Metric::TimeOnPage => "time_on_page",
            Metric::ScrollDepth => "scroll_depth",
            Metric::ConversionRate => "conversion_rate",
            Metric::GroupConversionRate => "group_conversion_rate",
            Metric::TotalRevenue => "total_revenue",
            Metric::AverageRevenue => "average_revenue",
        }
    }

    /// 是否在本轮实现。
    pub fn is_supported(self) -> bool {
        !matches!(
            self,
            Metric::GroupConversionRate | Metric::TotalRevenue | Metric::AverageRevenue
        )
    }
}

// ------------------------------------------------------------------------------------------
// 维度
// ------------------------------------------------------------------------------------------

/// 分组与过滤的维度。`event:props:<k>` 本轮不支持（能解析）。`event:goal` 的取值是站点目标的标签（§6.5）。
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Dimension {
    EventName,
    EventPage,
    EventHostname,
    EventGoal,
    EventProp(String),
    VisitEntryPage,
    VisitExitPage,
    VisitSource,
    VisitChannel,
    VisitReferrer,
    VisitUtmSource,
    VisitUtmMedium,
    VisitUtmCampaign,
    VisitUtmContent,
    VisitUtmTerm,
    VisitCountry,
    VisitRegion,
    VisitCity,
    VisitDevice,
    VisitBrowser,
    VisitBrowserVersion,
    VisitOs,
    VisitOsVersion,
    /// 自动粒度（规则见 `resolve::auto_granularity`）。
    Time,
    TimeMinute,
    TimeHour,
    TimeDay,
    TimeWeek,
    TimeMonth,
}

/// 固定名字的维度（不含 `event:props:<k>`），顺序即 schema 里的枚举顺序。
const FIXED_DIMENSIONS: [(&str, Dimension); 28] = [
    ("event:name", Dimension::EventName),
    ("event:page", Dimension::EventPage),
    ("event:hostname", Dimension::EventHostname),
    ("event:goal", Dimension::EventGoal),
    ("visit:entry_page", Dimension::VisitEntryPage),
    ("visit:exit_page", Dimension::VisitExitPage),
    ("visit:source", Dimension::VisitSource),
    ("visit:channel", Dimension::VisitChannel),
    ("visit:referrer", Dimension::VisitReferrer),
    ("visit:utm_source", Dimension::VisitUtmSource),
    ("visit:utm_medium", Dimension::VisitUtmMedium),
    ("visit:utm_campaign", Dimension::VisitUtmCampaign),
    ("visit:utm_content", Dimension::VisitUtmContent),
    ("visit:utm_term", Dimension::VisitUtmTerm),
    ("visit:country", Dimension::VisitCountry),
    ("visit:region", Dimension::VisitRegion),
    ("visit:city", Dimension::VisitCity),
    ("visit:device", Dimension::VisitDevice),
    ("visit:browser", Dimension::VisitBrowser),
    ("visit:browser_version", Dimension::VisitBrowserVersion),
    ("visit:os", Dimension::VisitOs),
    ("visit:os_version", Dimension::VisitOsVersion),
    ("time", Dimension::Time),
    ("time:minute", Dimension::TimeMinute),
    ("time:hour", Dimension::TimeHour),
    ("time:day", Dimension::TimeDay),
    ("time:week", Dimension::TimeWeek),
    ("time:month", Dimension::TimeMonth),
];

/// `event:props:<k>` 的键长上限。
pub const MAX_PROP_KEY_LEN: usize = 200;

impl Dimension {
    pub fn parse(s: &str) -> Result<Self, String> {
        if let Some((_, d)) = FIXED_DIMENSIONS.iter().find(|(name, _)| *name == s) {
            return Ok(d.clone());
        }
        if let Some(key) = s.strip_prefix("event:props:") {
            if key.is_empty() || key.chars().count() > MAX_PROP_KEY_LEN {
                return Err(format!(
                    "the property key in `event:props:<key>` must be 1 to {MAX_PROP_KEY_LEN} characters"
                ));
            }
            return Ok(Dimension::EventProp(key.to_owned()));
        }
        let shown: String = s.chars().take(60).collect();
        Err(format!(
            "unknown dimension `{shown}`; expected one of {}, or `event:props:<key>`",
            FIXED_DIMENSIONS
                .iter()
                .map(|(n, _)| format!("`{n}`"))
                .collect::<Vec<_>>()
                .join(", ")
        ))
    }

    pub fn as_str(&self) -> Cow<'static, str> {
        if let Dimension::EventProp(k) = self {
            return Cow::Owned(format!("event:props:{k}"));
        }
        let name = FIXED_DIMENSIONS
            .iter()
            .find(|(_, d)| d == self)
            .map(|(n, _)| *n)
            .expect("固定维度都在表里");
        Cow::Borrowed(name)
    }

    /// 时间维度（含自动粒度）。
    pub fn is_time(&self) -> bool {
        matches!(
            self,
            Dimension::Time
                | Dimension::TimeMinute
                | Dimension::TimeHour
                | Dimension::TimeDay
                | Dimension::TimeWeek
                | Dimension::TimeMonth
        )
    }

    /// 位置类维度：结果含位置数据，必须带地理署名（§4.4）。
    pub fn is_geo(&self) -> bool {
        matches!(
            self,
            Dimension::VisitCountry | Dimension::VisitRegion | Dimension::VisitCity
        )
    }

    /// 事件级维度（`event:*`）。
    pub fn is_event_level(&self) -> bool {
        matches!(
            self,
            Dimension::EventName
                | Dimension::EventPage
                | Dimension::EventHostname
                | Dimension::EventGoal
                | Dimension::EventProp(_)
        )
    }

    /// 本轮不支持的维度。
    pub fn is_supported(&self) -> bool {
        !matches!(self, Dimension::EventProp(_))
    }
}

impl fmt::Display for Dimension {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.as_str())
    }
}

impl Serialize for Dimension {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.as_str())
    }
}

impl<'de> Deserialize<'de> for Dimension {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Dimension::parse(&s).map_err(de::Error::custom)
    }
}

impl JsonSchema for Dimension {
    fn schema_name() -> Cow<'static, str> {
        "Dimension".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        let names: Vec<&str> = FIXED_DIMENSIONS.iter().map(|(n, _)| *n).collect();
        schema(json!({
            "description": "A dimension. `event:goal` groups by the site's goals (values are goal labels). `event:props:<key>` is accepted by the schema but not supported yet.",
            "anyOf": [
                {"type": "string", "enum": names},
                {"type": "string", "pattern": "^event:props:.+$", "maxLength": 212}
            ]
        }))
    }
}

fn schema(value: Value) -> Schema {
    Schema::try_from(value).expect("手写的 schema 是对象")
}

// ------------------------------------------------------------------------------------------
// date_range
// ------------------------------------------------------------------------------------------

/// 自定义范围的一端：日期（站点本地日）或带 UTC 偏移的时间戳。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RangeBound {
    Date(Date),
    Instant(Timestamp),
}

impl RangeBound {
    fn parse(s: &str) -> Result<Self, String> {
        const EXPECTED: &str =
            "expected a date (YYYY-MM-DD) or an RFC 3339 timestamp with a UTC offset";
        if s.len() > 64 {
            return Err(EXPECTED.into());
        }
        // 日期只接受 `YYYY-MM-DD`：jiff 的 `Date` 解析会忽略时间部分，所以先按长度挡住带时间的写法。
        if s.len() == 10
            && let Ok(d) = s.parse::<Date>()
        {
            return Ok(RangeBound::Date(d));
        }
        if let Ok(t) = s.parse::<Timestamp>() {
            return Ok(RangeBound::Instant(t));
        }
        if let Ok(z) = s.parse::<jiff::Zoned>() {
            return Ok(RangeBound::Instant(z.timestamp()));
        }
        Err(EXPECTED.into())
    }

    fn to_json(self) -> Value {
        match self {
            RangeBound::Date(d) => Value::String(d.to_string()),
            RangeBound::Instant(t) => Value::String(t.to_string()),
        }
    }
}

/// `[from, to]`：两端同为日期（含两端的本地日）或同为时间戳（`[from, to)`）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CustomRange {
    pub from: RangeBound,
    pub to: RangeBound,
}

impl CustomRange {
    fn parse(arr: &[Value]) -> Result<Self, SubError> {
        if arr.len() != 2 {
            return Err(SubError::at(
                "",
                "a custom range must be an array of exactly two values: [from, to]",
            ));
        }
        let mut bounds = [RangeBound::Date(Date::constant(1970, 1, 1)); 2];
        for (i, v) in arr.iter().enumerate() {
            let s = v
                .as_str()
                .ok_or_else(|| SubError::at(format!("[{i}]"), "expected a string"))?;
            bounds[i] = RangeBound::parse(s).map_err(|m| SubError::at(format!("[{i}]"), m))?;
        }
        match (bounds[0], bounds[1]) {
            (RangeBound::Date(_), RangeBound::Date(_))
            | (RangeBound::Instant(_), RangeBound::Instant(_)) => {}
            _ => {
                return Err(SubError::at(
                    "[1]",
                    "both ends must be dates, or both must be timestamps",
                ));
            }
        }
        Ok(CustomRange {
            from: bounds[0],
            to: bounds[1],
        })
    }

    fn to_json(self) -> Value {
        Value::Array(vec![self.from.to_json(), self.to.to_json()])
    }
}

/// 日期范围（§6.2、§6.7）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DateRange {
    /// 今天（站点本地日），截至现在。
    Day,
    /// 滚动 24 小时：`[now - 24h, now]`。
    Hours24,
    /// 本月 1 日到今天。
    Month,
    /// 当年 1 月 1 日到今天。
    Year,
    /// 站点最早事件所在日到今天。
    All,
    /// 最近 5 分钟。
    Realtime,
    /// 截至今天的 N 个本地日（含今天）。`7d`、`28d`、`30d` 都是它。
    Days(u32),
    /// 截至今天的 N 个自然月（含本月）。`6mo`、`12mo` 都是它。
    Months(u32),
    Custom(CustomRange),
}

impl DateRange {
    fn parse_keyword(s: &str) -> Result<Self, String> {
        let simple = match s {
            "day" => Some(DateRange::Day),
            "24h" => Some(DateRange::Hours24),
            "month" => Some(DateRange::Month),
            "year" => Some(DateRange::Year),
            "all" => Some(DateRange::All),
            "realtime" => Some(DateRange::Realtime),
            _ => None,
        };
        if let Some(r) = simple {
            return Ok(r);
        }
        let (digits, is_days) = if let Some(n) = s.strip_suffix("mo") {
            (n, false)
        } else if let Some(n) = s.strip_suffix('d') {
            (n, true)
        } else {
            (s, true)
        };
        let all_digits = !digits.is_empty()
            && digits.len() <= 9
            && digits.bytes().all(|b| b.is_ascii_digit())
            && digits != s;
        if all_digits {
            let n: u32 = digits.parse().unwrap_or(u32::MAX);
            let (max, unit) = if is_days {
                (MAX_RANGE_DAYS, "days")
            } else {
                (MAX_RANGE_MONTHS, "months")
            };
            if n == 0 || n > max {
                return Err(format!("the number of {unit} must be between 1 and {max}"));
            }
            return Ok(if is_days {
                DateRange::Days(n)
            } else {
                DateRange::Months(n)
            });
        }
        let shown: String = s.chars().take(40).collect();
        Err(format!(
            "unknown date range `{shown}`; expected day, 24h, month, year, all, realtime, <N>d, <N>mo, or [from, to]"
        ))
    }

    /// 规范写法（回显用）。
    pub fn to_json(&self) -> Value {
        match self {
            DateRange::Day => json!("day"),
            DateRange::Hours24 => json!("24h"),
            DateRange::Month => json!("month"),
            DateRange::Year => json!("year"),
            DateRange::All => json!("all"),
            DateRange::Realtime => json!("realtime"),
            DateRange::Days(n) => json!(format!("{n}d")),
            DateRange::Months(n) => json!(format!("{n}mo")),
            DateRange::Custom(c) => c.to_json(),
        }
    }
}

impl Serialize for DateRange {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        self.to_json().serialize(s)
    }
}

impl<'de> Deserialize<'de> for DateRange {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        match Value::deserialize(d)? {
            Value::String(s) => DateRange::parse_keyword(&s).map_err(de::Error::custom),
            Value::Array(arr) => CustomRange::parse(&arr)
                .map(DateRange::Custom)
                .map_err(SubError::into_de),
            _ => Err(de::Error::custom(
                "date_range must be a string such as \"7d\" or an array [from, to]",
            )),
        }
    }
}

fn custom_range_schema() -> Value {
    json!({
        "type": "array",
        "prefixItems": [{"type": "string"}, {"type": "string"}],
        "minItems": 2,
        "maxItems": 2,
        "description": "[from, to]: two dates (YYYY-MM-DD, both days included, site time zone) or two RFC 3339 timestamps with a UTC offset (from included, to excluded)."
    })
}

impl JsonSchema for DateRange {
    fn schema_name() -> Cow<'static, str> {
        "DateRange".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        schema(json!({
            "description": "The period to query. Relative ranges follow the site's time zone; `7d` is the last 7 local days including today.",
            "anyOf": [
                {"type": "string", "enum": ["day", "24h", "month", "year", "all", "realtime"]},
                {"type": "string", "pattern": "^[0-9]{1,9}d$", "description": "N local days ending today, today included (1 to 5000), e.g. 7d, 28d, 30d."},
                {"type": "string", "pattern": "^[0-9]{1,9}mo$", "description": "N calendar months ending today, this month included (1 to 100), e.g. 6mo, 12mo."},
                custom_range_schema()
            ]
        }))
    }
}

// ------------------------------------------------------------------------------------------
// 过滤
// ------------------------------------------------------------------------------------------

/// 叶子过滤的操作符。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FilterOp {
    Is,
    IsNot,
    Contains,
    ContainsNot,
    Matches,
    MatchesNot,
    MatchesWildcard,
    MatchesWildcardNot,
}

impl FilterOp {
    pub const ALL: [FilterOp; 8] = [
        FilterOp::Is,
        FilterOp::IsNot,
        FilterOp::Contains,
        FilterOp::ContainsNot,
        FilterOp::Matches,
        FilterOp::MatchesNot,
        FilterOp::MatchesWildcard,
        FilterOp::MatchesWildcardNot,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            FilterOp::Is => "is",
            FilterOp::IsNot => "is_not",
            FilterOp::Contains => "contains",
            FilterOp::ContainsNot => "contains_not",
            FilterOp::Matches => "matches",
            FilterOp::MatchesNot => "matches_not",
            FilterOp::MatchesWildcard => "matches_wildcard",
            FilterOp::MatchesWildcardNot => "matches_wildcard_not",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|op| op.as_str() == s)
    }

    /// 取反的操作符（`*_not`）。
    pub fn is_negated(self) -> bool {
        matches!(
            self,
            FilterOp::IsNot
                | FilterOp::ContainsNot
                | FilterOp::MatchesNot
                | FilterOp::MatchesWildcardNot
        )
    }
}

/// 叶子过滤的对象：维度，或分群（`["is", "segment", [id]]`，本轮不支持）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FilterTarget {
    Dimension(Dimension),
    Segment,
}

impl FilterTarget {
    pub fn as_str(&self) -> Cow<'static, str> {
        match self {
            FilterTarget::Dimension(d) => d.as_str(),
            FilterTarget::Segment => Cow::Borrowed("segment"),
        }
    }
}

/// 叶子：`[op, dimension, [values], {case_sensitive}?]`。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FilterLeaf {
    pub op: FilterOp,
    pub target: FilterTarget,
    /// 整数值已转成十进制字符串（`visit:city` 的 geoname id）。
    pub values: Vec<String>,
    /// 缺省 `true`。
    pub case_sensitive: Option<bool>,
}

/// 过滤树节点。顶层 `filters` 数组是隐式 AND。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Filter {
    Leaf(FilterLeaf),
    And(Vec<Filter>),
    Or(Vec<Filter>),
    Not(Box<Filter>),
    /// 本轮不支持。
    HasDone(Box<Filter>),
    /// 本轮不支持。
    HasNotDone(Box<Filter>),
}

fn parse_filter(v: &Value, depth: usize) -> Result<Filter, SubError> {
    if depth > MAX_FILTER_DEPTH {
        return Err(SubError::at(
            "",
            format!("filters may be nested at most {MAX_FILTER_DEPTH} levels deep"),
        ));
    }
    let arr = v.as_array().ok_or_else(|| {
        SubError::at(
            "",
            "a filter must be an array such as [\"is\", \"visit:country\", [\"DE\"]]",
        )
    })?;
    let op = arr
        .first()
        .and_then(Value::as_str)
        .ok_or_else(|| SubError::at("[0]", "the filter operator must be a string"))?;
    let expect_len = |want: &[usize], what: &str| -> Result<(), SubError> {
        if want.contains(&arr.len()) {
            Ok(())
        } else {
            Err(SubError::at(
                "",
                format!("`{what}` filters {}", {
                    if want.len() == 1 {
                        format!("have exactly {} elements", want[0])
                    } else {
                        format!("have {} or {} elements", want[0], want[1])
                    }
                }),
            ))
        }
    };
    match op {
        "and" | "or" => {
            expect_len(&[2], op)?;
            let children = arr[1]
                .as_array()
                .ok_or_else(|| SubError::at("[1]", "expected an array of filters"))?;
            if children.is_empty() {
                return Err(SubError::at("[1]", "expected at least one filter"));
            }
            let mut out = Vec::with_capacity(children.len());
            for (i, c) in children.iter().enumerate() {
                out.push(parse_filter(c, depth + 1).map_err(|e| e.prefixed(&format!("[1][{i}]")))?);
            }
            Ok(if op == "and" {
                Filter::And(out)
            } else {
                Filter::Or(out)
            })
        }
        "not" | "has_done" | "has_not_done" => {
            expect_len(&[2], op)?;
            let child = Box::new(parse_filter(&arr[1], depth + 1).map_err(|e| e.prefixed("[1]"))?);
            Ok(match op {
                "not" => Filter::Not(child),
                "has_done" => Filter::HasDone(child),
                _ => Filter::HasNotDone(child),
            })
        }
        _ => {
            let Some(op) = FilterOp::parse(op) else {
                let shown: String = op.chars().take(40).collect();
                return Err(SubError::at(
                    "[0]",
                    format!(
                        "unknown filter operator `{shown}`; expected one of {}, and, or, not, has_done, has_not_done",
                        FilterOp::ALL
                            .iter()
                            .map(|o| o.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                ));
            };
            expect_len(&[3, 4], op.as_str())?;
            let dim_str = arr[1]
                .as_str()
                .ok_or_else(|| SubError::at("[1]", "the filter dimension must be a string"))?;
            let target = if dim_str == "segment" {
                FilterTarget::Segment
            } else {
                FilterTarget::Dimension(
                    Dimension::parse(dim_str).map_err(|m| SubError::at("[1]", m))?,
                )
            };
            let raw_values = arr[2]
                .as_array()
                .ok_or_else(|| SubError::at("[2]", "expected an array of values"))?;
            let mut values = Vec::with_capacity(raw_values.len());
            for (i, rv) in raw_values.iter().enumerate() {
                values.push(match rv {
                    Value::String(s) => s.clone(),
                    Value::Number(n) if n.is_i64() || n.is_u64() => n.to_string(),
                    _ => {
                        return Err(SubError::at(
                            format!("[2][{i}]"),
                            "expected a string or an integer",
                        ));
                    }
                });
            }
            let case_sensitive = match arr.get(3) {
                None => None,
                Some(Value::Object(m)) => {
                    if let Some(k) = m.keys().find(|k| k.as_str() != "case_sensitive") {
                        let shown: String = k.chars().take(40).collect();
                        return Err(SubError::at(
                            format!("[3].{shown}"),
                            "unknown modifier; only `case_sensitive` is supported",
                        ));
                    }
                    match m.get("case_sensitive") {
                        None => None,
                        Some(Value::Bool(b)) => Some(*b),
                        Some(_) => {
                            return Err(SubError::at(
                                "[3].case_sensitive",
                                "expected true or false",
                            ));
                        }
                    }
                }
                Some(_) => {
                    return Err(SubError::at(
                        "[3]",
                        "expected an object such as {\"case_sensitive\": false}",
                    ));
                }
            };
            Ok(Filter::Leaf(FilterLeaf {
                op,
                target,
                values,
                case_sensitive,
            }))
        }
    }
}

impl Filter {
    pub fn to_json(&self) -> Value {
        match self {
            Filter::Leaf(l) => {
                let mut a = vec![
                    json!(l.op.as_str()),
                    json!(l.target.as_str()),
                    json!(l.values),
                ];
                if let Some(cs) = l.case_sensitive {
                    a.push(json!({"case_sensitive": cs}));
                }
                Value::Array(a)
            }
            Filter::And(c) => json!(["and", c.iter().map(Filter::to_json).collect::<Vec<_>>()]),
            Filter::Or(c) => json!(["or", c.iter().map(Filter::to_json).collect::<Vec<_>>()]),
            Filter::Not(c) => json!(["not", c.to_json()]),
            Filter::HasDone(c) => json!(["has_done", c.to_json()]),
            Filter::HasNotDone(c) => json!(["has_not_done", c.to_json()]),
        }
    }
}

impl Serialize for Filter {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        self.to_json().serialize(s)
    }
}

impl<'de> Deserialize<'de> for Filter {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let v = Value::deserialize(d)?;
        parse_filter(&v, 0).map_err(SubError::into_de)
    }
}

impl JsonSchema for Filter {
    fn schema_name() -> Cow<'static, str> {
        "Filter".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        let ops: Vec<&str> = FilterOp::ALL.iter().map(|o| o.as_str()).collect();
        schema(json!({
            "description": "A filter tree node. Leaf: [op, dimension, [values], {\"case_sensitive\": bool}?]. Branch: [\"and\" | \"or\", [filters]] or [\"not\", filter]. `has_done`, `has_not_done` and `segment` are accepted by the schema but not supported yet.",
            "anyOf": [
                {
                    "type": "array",
                    "prefixItems": [
                        {"type": "string", "enum": ops},
                        {"anyOf": [{"$ref": "#/$defs/Dimension"}, {"const": "segment"}]},
                        {"type": "array", "items": {"type": ["string", "integer"]}, "minItems": 1},
                        {"type": "object", "properties": {"case_sensitive": {"type": "boolean"}}, "additionalProperties": false}
                    ],
                    "minItems": 3,
                    "maxItems": 4
                },
                {
                    "type": "array",
                    "prefixItems": [
                        {"enum": ["and", "or"]},
                        {"type": "array", "items": {"$ref": "#/$defs/Filter"}, "minItems": 1}
                    ],
                    "minItems": 2,
                    "maxItems": 2
                },
                {
                    "type": "array",
                    "prefixItems": [
                        {"enum": ["not", "has_done", "has_not_done"]},
                        {"$ref": "#/$defs/Filter"}
                    ],
                    "minItems": 2,
                    "maxItems": 2
                }
            ]
        }))
    }
}

// ------------------------------------------------------------------------------------------
// order_by、include、pagination
// ------------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Direction {
    Asc,
    Desc,
}

/// `[key, "asc" | "desc"]`：`key` 必须是请求里的指标或维度（按请求里的写法，自动粒度的时间维度写 `time`）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OrderBy {
    pub key: String,
    pub direction: Direction,
}

impl OrderBy {
    pub fn to_json(&self) -> Value {
        json!([
            self.key,
            match self.direction {
                Direction::Asc => "asc",
                Direction::Desc => "desc",
            }
        ])
    }
}

impl Serialize for OrderBy {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        self.to_json().serialize(s)
    }
}

impl<'de> Deserialize<'de> for OrderBy {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let v = Value::deserialize(d)?;
        let parse = || -> Result<OrderBy, SubError> {
            let arr = v
                .as_array()
                .filter(|a| a.len() == 2)
                .ok_or_else(|| SubError::at("", "expected [key, \"asc\" | \"desc\"]"))?;
            let key = arr[0]
                .as_str()
                .ok_or_else(|| SubError::at("[0]", "expected a metric or dimension name"))?;
            let direction = match arr[1].as_str() {
                Some("asc") => Direction::Asc,
                Some("desc") => Direction::Desc,
                _ => {
                    return Err(SubError::at("[1]", "expected \"asc\" or \"desc\""));
                }
            };
            Ok(OrderBy {
                key: key.to_owned(),
                direction,
            })
        };
        parse().map_err(SubError::into_de)
    }
}

impl JsonSchema for OrderBy {
    fn schema_name() -> Cow<'static, str> {
        "OrderBy".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        schema(json!({
            "description": "[metric_or_dimension, direction]. The key must be one of the requested metrics or dimensions.",
            "type": "array",
            "prefixItems": [{"type": "string"}, {"enum": ["asc", "desc"]}],
            "minItems": 2,
            "maxItems": 2
        }))
    }
}

/// 对比期：`previous_period`、`year_over_year` 或显式的 `[from, to]`（写法同 `date_range`）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Compare {
    PreviousPeriod,
    YearOverYear,
    Custom(CustomRange),
}

impl Compare {
    pub fn to_json(&self) -> Value {
        match self {
            Compare::PreviousPeriod => json!("previous_period"),
            Compare::YearOverYear => json!("year_over_year"),
            Compare::Custom(c) => c.to_json(),
        }
    }
}

impl Serialize for Compare {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        self.to_json().serialize(s)
    }
}

impl<'de> Deserialize<'de> for Compare {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        match Value::deserialize(d)? {
            Value::String(s) => match s.as_str() {
                "previous_period" => Ok(Compare::PreviousPeriod),
                "year_over_year" => Ok(Compare::YearOverYear),
                _ => Err(de::Error::custom(
                    "expected \"previous_period\", \"year_over_year\", or [from, to]",
                )),
            },
            Value::Array(arr) => CustomRange::parse(&arr)
                .map(Compare::Custom)
                .map_err(SubError::into_de),
            _ => Err(de::Error::custom(
                "expected \"previous_period\", \"year_over_year\", or [from, to]",
            )),
        }
    }
}

impl JsonSchema for Compare {
    fn schema_name() -> Cow<'static, str> {
        "Compare".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        schema(json!({
            "description": "The comparison period. The comparison query is restricted to the dimension values of the main result.",
            "anyOf": [
                {"type": "string", "enum": ["previous_period", "year_over_year"]},
                custom_range_schema()
            ]
        }))
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Include {
    /// 返回 `meta.time_labels`（完整的时间桶序列，含空桶）。必须带时间维度。
    #[serde(default)]
    pub time_labels: bool,
    /// 返回 `meta.total_rows`（不计 limit / offset 的总行数）。
    #[serde(default)]
    pub total_rows: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compare: Option<Compare>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Pagination {
    /// 1 到 10000，缺省 10000。
    #[serde(default = "default_limit")]
    #[schemars(range(min = 1, max = 10000))]
    pub limit: u32,
    #[serde(default)]
    pub offset: u32,
}

fn default_limit() -> u32 {
    MAX_LIMIT
}

impl Default for Pagination {
    fn default() -> Self {
        Pagination {
            limit: MAX_LIMIT,
            offset: 0,
        }
    }
}

// ------------------------------------------------------------------------------------------
// QuerySpec
// ------------------------------------------------------------------------------------------

/// 查询请求。`POST /api/v1/query` 与看板的内部查询端点是同一个类型（§6.1）。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct QuerySpec {
    /// `site_key`。公开 API（`POST /api/v1/query`）必填，缺失返回 400；看板内部端点由路径给出站点时可以省略。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub site: Option<String>,
    /// 至少一个，不得重复。
    pub metrics: Vec<Metric>,
    pub date_range: DateRange,
    #[serde(default)]
    pub dimensions: Vec<Dimension>,
    /// 顶层数组是隐式 AND。
    #[serde(default)]
    pub filters: Vec<Filter>,
    #[serde(default)]
    pub order_by: Vec<OrderBy>,
    #[serde(default)]
    pub include: Include,
    #[serde(default)]
    pub pagination: Pagination,
}

impl QuerySpec {
    /// 解析并做语法检查。错误带字段路径与英文消息。
    pub fn parse(value: &Value) -> Result<QuerySpec, QueryError> {
        if !value.is_object() {
            return Err(QueryError::validation(
                "",
                "the query must be a JSON object",
            ));
        }
        serde_path_to_error::deserialize::<_, QuerySpec>(value).map_err(|err| {
            let path = err.path().to_string();
            let inner = err.inner().to_string();
            let (sub, message) = unmark(&inner);
            let mut path = if path == "." { String::new() } else { path };
            path.push_str(sub);
            let mut message = message.to_owned();
            // 缺少字段、未知字段的错误没有路径：从消息里取出字段名。
            for prefix in ["missing field `", "unknown field `"] {
                if path.is_empty()
                    && let Some(rest) = message.strip_prefix(prefix)
                    && let Some((name, _)) = rest.split_once('`')
                {
                    path = name.to_owned();
                    if prefix.starts_with("missing") {
                        message = "this field is required".into();
                    } else if let Some((_, tail)) = message.split_once("expected") {
                        message = format!("unknown field; expected{tail}");
                    }
                    break;
                }
            }
            QueryError::validation(path, message)
        })
    }

    pub fn parse_str(text: &str) -> Result<QuerySpec, QueryError> {
        let value: Value = serde_json::from_str(text)
            .map_err(|e| QueryError::validation("", format!("the body is not valid JSON: {e}")))?;
        QuerySpec::parse(&value)
    }
}

/// `GET /api/v1/query/schema.json`：由 `schemars` 从 Rust 类型生成（draft 2020-12），文档与校验同源。
pub fn schema_json() -> Value {
    let generator = schemars::generate::SchemaSettings::draft2020_12().into_generator();
    let root = generator.into_root_schema_for::<QuerySpec>();
    sorted(serde_json::to_value(root).expect("schema 可序列化"))
}

/// 递归地按键排序：工作区里别的 crate 打开 `serde_json/preserve_order` 时，输出顺序也不变（快照与文档都稳定）。
fn sorted(v: Value) -> Value {
    match v {
        Value::Object(m) => {
            let ordered: std::collections::BTreeMap<String, Value> =
                m.into_iter().map(|(k, v)| (k, sorted(v))).collect();
            Value::Object(ordered.into_iter().collect())
        }
        Value::Array(a) => Value::Array(a.into_iter().map(sorted).collect()),
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(v: Value) -> Result<QuerySpec, (String, String)> {
        QuerySpec::parse(&v).map_err(|e| {
            let (p, m) = e.as_validation().expect("校验错误");
            (p.to_owned(), m.to_owned())
        })
    }

    fn base() -> Value {
        json!({"metrics": ["visitors"], "date_range": "7d"})
    }

    #[test]
    fn minimal_query_gets_defaults() {
        let q = parse(base()).unwrap();
        assert_eq!(q.metrics, [Metric::Visitors]);
        assert_eq!(q.date_range, DateRange::Days(7));
        assert!(q.dimensions.is_empty() && q.filters.is_empty() && q.order_by.is_empty());
        assert_eq!(q.pagination, Pagination::default());
        assert_eq!(q.include, Include::default());
    }

    #[test]
    fn full_query_roundtrips_through_serialization() {
        let v = json!({
            "site": "abcdefghijkl",
            "metrics": ["visitors", "bounce_rate"],
            "date_range": ["2026-03-01", "2026-03-31"],
            "dimensions": ["visit:country", "time:day"],
            "filters": [
                ["is", "event:page", ["/a", "/b"], {"case_sensitive": false}],
                ["or", [["contains", "visit:source", ["goo"]], ["not", ["is", "visit:device", ["mobile"]]]]]
            ],
            "order_by": [["visitors", "desc"], ["time:day", "asc"]],
            "include": {"time_labels": true, "total_rows": true, "compare": "previous_period"},
            "pagination": {"limit": 50, "offset": 10}
        });
        let q = parse(v.clone()).unwrap();
        assert_eq!(serde_json::to_value(&q).unwrap(), v);
    }

    #[test]
    fn unknown_fields_are_rejected_with_their_path() {
        let mut v = base();
        v["bogus"] = json!(1);
        let (p, m) = parse(v).unwrap_err();
        assert_eq!(p, "bogus");
        assert!(m.starts_with("unknown field"), "{m}");

        let mut v = base();
        v["pagination"] = json!({"limit": 5, "page": 2});
        let (p, _) = parse(v).unwrap_err();
        assert_eq!(p, "pagination.page");

        let mut v = base();
        v["include"] = json!({"imports": true});
        assert_eq!(parse(v).unwrap_err().0, "include.imports");
    }

    #[test]
    fn missing_required_fields() {
        assert_eq!(
            parse(json!({"date_range": "7d"})).unwrap_err(),
            ("metrics".into(), "this field is required".into())
        );
        assert_eq!(
            parse(json!({"metrics": ["visitors"]})).unwrap_err().0,
            "date_range"
        );
        let (p, m) = QuerySpec::parse(&json!([1]))
            .map_err(|e| {
                let (p, m) = e.as_validation().unwrap();
                (p.to_owned(), m.to_owned())
            })
            .unwrap_err();
        assert_eq!(
            (p.as_str(), m.as_str()),
            ("", "the query must be a JSON object")
        );
    }

    #[test]
    fn bad_metric_and_dimension_names_carry_indexes() {
        let (p, m) =
            parse(json!({"metrics": ["visitors", "visitorz"], "date_range": "day"})).unwrap_err();
        assert_eq!(p, "metrics[1]");
        assert!(m.contains("unknown variant `visitorz`"), "{m}");

        let (p, m) = parse(json!({
            "metrics": ["visitors"], "date_range": "day",
            "dimensions": ["visit:country", "visit:planet"]
        }))
        .unwrap_err();
        assert_eq!(p, "dimensions[1]");
        assert!(m.starts_with("unknown dimension `visit:planet`"), "{m}");
    }

    #[test]
    fn date_range_forms() {
        let r = |v: Value| {
            parse(json!({"metrics": ["visitors"], "date_range": v})).map(|q| q.date_range)
        };
        assert_eq!(r(json!("day")).unwrap(), DateRange::Day);
        assert_eq!(r(json!("24h")).unwrap(), DateRange::Hours24);
        assert_eq!(r(json!("realtime")).unwrap(), DateRange::Realtime);
        assert_eq!(r(json!("30d")).unwrap(), DateRange::Days(30));
        assert_eq!(r(json!("91d")).unwrap(), DateRange::Days(91));
        assert_eq!(r(json!("6mo")).unwrap(), DateRange::Months(6));
        assert_eq!(r(json!("12mo")).unwrap(), DateRange::Months(12));
        assert!(matches!(
            r(json!(["2026-03-28T10:00:00+01:00", "2026-03-29T10:00:00Z"])).unwrap(),
            DateRange::Custom(CustomRange {
                from: RangeBound::Instant(_),
                to: RangeBound::Instant(_)
            })
        ));
        assert!(matches!(
            r(json!([
                "2026-03-28T10:00:00+01:00[Europe/Berlin]",
                "2026-03-29T10:00:00Z"
            ]))
            .unwrap(),
            DateRange::Custom(_)
        ));
        for (bad, path) in [
            (json!("0d"), "date_range"),
            (json!("5001d"), "date_range"),
            (json!("101mo"), "date_range"),
            (json!("d"), "date_range"),
            (json!("-3d"), "date_range"),
            (json!("yesterday"), "date_range"),
            (json!(7), "date_range"),
            (json!(["2026-03-28"]), "date_range"),
            (json!(["2026-03-28", "nope"]), "date_range[1]"),
            (json!(["2026-03-28", 5]), "date_range[1]"),
            (
                json!(["2026-03-28", "2026-03-29T10:00:00Z"]),
                "date_range[1]",
            ),
            (
                json!(["2026-03-28T10:00:00", "2026-03-29T10:00:00Z"]),
                "date_range[0]",
            ),
        ] {
            let (p, _) = r(bad.clone()).unwrap_err();
            assert_eq!(p, path, "{bad}");
        }
    }

    #[test]
    fn filter_errors_have_nested_paths() {
        let f = |filters: Value| {
            parse(json!({"metrics": ["visitors"], "date_range": "day", "filters": filters}))
                .map(|q| q.filters)
        };
        assert!(f(json!([["is", "visit:country", ["DE"]]])).is_ok());
        let cases = [
            (json!(["is", "visit:country", ["DE"]]), "filters[0]"),
            (json!([[]]), "filters[0][0]"),
            (json!([["wat", "visit:country", ["DE"]]]), "filters[0][0]"),
            (json!([["is", "visit:country"]]), "filters[0]"),
            (json!([["is", "visit:nope", ["x"]]]), "filters[0][1]"),
            (json!([["is", "visit:country", "DE"]]), "filters[0][2]"),
            (json!([["is", "visit:country", [1.5]]]), "filters[0][2][0]"),
            (
                json!([["is", "visit:country", ["DE"], {"case": 1}]]),
                "filters[0][3].case",
            ),
            (
                json!([["is", "visit:country", ["DE"], {"case_sensitive": "no"}]]),
                "filters[0][3].case_sensitive",
            ),
            (json!([["and", []]]), "filters[0][1]"),
            (
                json!([[
                    "and",
                    [
                        ["is", "visit:country", ["DE"]],
                        ["or", [["is", "visit:os", [3, {}]]]]
                    ]
                ]]),
                "filters[0][1][1][1][0][2][1]",
            ),
            (json!([["not"]]), "filters[0]"),
        ];
        for (bad, path) in cases {
            let (p, _) = parse(
                json!({"metrics": ["visitors"], "date_range": "day", "filters": bad.clone()}),
            )
            .unwrap_err();
            assert_eq!(p, path, "{bad}");
        }
    }

    #[test]
    fn filter_nesting_depth_is_limited() {
        let mut f = json!(["is", "visit:country", ["DE"]]);
        for _ in 0..(MAX_FILTER_DEPTH + 1) {
            f = json!(["not", f]);
        }
        let (_, m) = parse(json!({"metrics": ["visitors"], "date_range": "day", "filters": [f]}))
            .unwrap_err();
        assert!(m.contains("nested at most"), "{m}");
    }

    #[test]
    fn integers_in_filter_values_become_strings() {
        let q = parse(json!({"metrics": ["visitors"], "date_range": "day",
            "filters": [["is", "visit:city", [2694762, "5"]]]}))
        .unwrap();
        let Filter::Leaf(l) = &q.filters[0] else {
            panic!()
        };
        assert_eq!(l.values, ["2694762", "5"]);
    }

    #[test]
    fn unsupported_features_still_parse() {
        let q = parse(json!({
            "metrics": ["conversion_rate", "total_revenue"], "date_range": "day",
            "dimensions": ["event:goal", "event:props:plan", "time:minute"],
            "filters": [["has_done", ["is", "event:name", ["Signup"]]], ["is", "segment", [3]]]
        }))
        .unwrap();
        assert_eq!(q.dimensions[1], Dimension::EventProp("plan".into()));
        assert!(matches!(q.filters[0], Filter::HasDone(_)));
        assert!(matches!(&q.filters[1], Filter::Leaf(l) if l.target == FilterTarget::Segment));
    }

    #[test]
    fn order_by_and_include_syntax() {
        let (p, _) = parse(json!({"metrics": ["visitors"], "date_range": "day",
            "order_by": [["visitors", "up"]]}))
        .unwrap_err();
        assert_eq!(p, "order_by[0][1]");
        let (p, _) = parse(json!({"metrics": ["visitors"], "date_range": "day",
            "include": {"compare": "last_week"}}))
        .unwrap_err();
        assert_eq!(p, "include.compare");
        let (p, _) = parse(json!({"metrics": ["visitors"], "date_range": "day",
            "include": {"compare": ["2026-01-01", "bad"]}}))
        .unwrap_err();
        assert_eq!(p, "include.compare[1]");
    }

    #[test]
    fn schema_declares_closed_objects_and_all_enums() {
        let s = schema_json();
        assert_eq!(s["additionalProperties"], json!(false));
        assert_eq!(s["required"], json!(["metrics", "date_range"]));
        let defs = &s["$defs"];
        assert_eq!(
            defs["Metric"]["enum"].as_array().unwrap().len(),
            Metric::ALL.len()
        );
        assert!(defs["Filter"].is_object() && defs["Dimension"].is_object());
        assert_eq!(defs["Include"]["additionalProperties"], json!(false));
    }

    #[test]
    fn dimension_names_roundtrip() {
        for (name, d) in FIXED_DIMENSIONS {
            assert_eq!(Dimension::parse(name).unwrap(), d);
            assert_eq!(d.as_str(), name);
        }
        assert_eq!(
            Dimension::parse("event:props:a:b").unwrap().as_str(),
            "event:props:a:b"
        );
        assert!(Dimension::parse("event:props:").is_err());
        assert!(Dimension::parse(&format!("event:props:{}", "x".repeat(201))).is_err());
    }
}
