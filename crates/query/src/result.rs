//! 查询结果：与 Plausible v2 同构的 `results` / `meta` / `query`（`system-design.md` §6.2、R03 §3.5、附录 A）。
//!
//! 约定：
//! - `results` 的每一行是 `{dimensions, metrics}`：`dimensions` 是展示用的字符串（空值已换成 `Direct / None`、
//!   `(not set)` 等文案，小时桶是带偏移的本地时间，其余时间桶是桶起始日），`metrics` 与请求里的 `metrics` 顺序一致；
//! - 计数类指标是整数，比率、时长、平均值是浮点数（比率为 0 到 1，时长为秒，`scroll_depth` 为 0 到 100）；
//!   所有比率分母为 0 时取 0，不产生 `null`；
//! - 时间维度的结果只含**有数据**的桶，完整的桶序列在 `meta.time_labels`（`include.time_labels`）；
//! - 没有时间维度的对比：每行带 `comparison: {dimensions, metrics, change}`；有时间维度的对比：
//!   `comparison_results` 是对比期的行（只含主结果里出现过的非时间维度取值），`meta.comparison_time_labels` 给出对比期的桶；
//! - `change`：`bounce_rate`、`percentage`、`exit_rate` 是绝对差（新 − 旧），其余指标是相对变化 `(新 − 旧) / 旧`
//!   （旧值为 0 时：新值也为 0 记 0，否则记 1）。

use std::collections::BTreeMap;

use rivu_geo::Attribution;
use serde::Serialize;
use serde_json::Value;

use crate::plan::Tier;

/// 一个指标值。
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
#[serde(untagged)]
pub enum MetricValue {
    Int(u64),
    Float(f64),
}

impl MetricValue {
    pub fn as_f64(self) -> f64 {
        match self {
            MetricValue::Int(n) => n as f64,
            MetricValue::Float(f) => f,
        }
    }
}

/// 对比期的同一行。
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Comparison {
    pub dimensions: Vec<String>,
    pub metrics: Vec<MetricValue>,
    pub change: Vec<f64>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ResultRow {
    pub dimensions: Vec<String>,
    pub metrics: Vec<MetricValue>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub comparison: Option<Comparison>,
}

/// `comparison_results` 的一行（有时间维度的对比）。
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ComparisonRow {
    pub dimensions: Vec<String>,
    pub metrics: Vec<MetricValue>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Warning {
    pub code: String,
    pub message: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct Meta {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub time_labels: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub comparison_time_labels: Option<Vec<String>>,
    pub warnings: Vec<Warning>,
    /// 用到的数据层。P1 恒为 `["raw"]`。
    pub tiers: Vec<Tier>,
    /// 每个被请求的指标在本次查询形态下的口径。
    pub metric_semantics: BTreeMap<String, String>,
    /// 结果含位置维度时必带（§4.4「署名要求」）；没有署名（自定义库未配置）时为 `null`。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub geo_attribution: Option<Attribution>,
    /// 维度或过滤涉及位置时给出：`country` 或 `city`（§6.10）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub geo_capability: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_rows: Option<u64>,
}

/// 回显里的解析后的时间范围。
#[derive(Clone, Debug, Serialize)]
pub struct ResolvedRange {
    /// 起点（含），站点时区的 RFC 3339 本地时间。
    pub start: String,
    /// 终点（不含），已封顶到「现在」。
    pub end: String,
}

/// `query`：规范化后的请求回显（日期范围、时间粒度、默认排序与分页已展开）。
#[derive(Clone, Debug, Serialize)]
pub struct EchoedQuery {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub site: Option<String>,
    pub metrics: Vec<String>,
    /// 请求里的写法。
    pub date_range: Value,
    pub resolved_date_range: ResolvedRange,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub comparison_date_range: Option<ResolvedRange>,
    pub time_zone: String,
    /// 自动粒度的 `time` 已换成具体粒度（如 `time:day`）。
    pub dimensions: Vec<String>,
    pub filters: Vec<Value>,
    pub order_by: Vec<Value>,
    pub include: Value,
    pub pagination: Value,
}

#[derive(Clone, Debug, Serialize)]
pub struct QueryResult {
    pub results: Vec<ResultRow>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub comparison_results: Option<Vec<ComparisonRow>>,
    pub meta: Meta,
    pub query: EchoedQuery,
}
