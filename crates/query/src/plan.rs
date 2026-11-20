//! 规划：把 [`ResolvedQuery`] 变成「哪些组件、各算哪些度量」（`system-design.md` §6.4 选表与选层）。
//!
//! # 层
//!
//! 原始层（`events` + `sessions`）与日汇总层（`rollup_*`，见 [`crate::rollup`]）。选层在执行时做（要看汇总层构建到了哪一天），
//! 规划只决定组件与度量；一次查询实际用到的层写在 `meta.tiers`。
//!
//! # 选表（§6.4 的表落成代码）
//!
//! | 对象 | 归属 |
//! |---|---|
//! | `visitors` `pageviews` `events`；`visits`（有 `event:*` 维度时） | `events` 的**事件组件** |
//! | `visits`（没有 `event:*` 维度时）`views_per_visit` `visit_duration` `engaged_duration` | `sessions` 的**会话组件** |
//! | `bounce_rate` | 会话组件；过滤里有 `event:page` / `event:hostname` 时是**独立的会话组件**（入口页口径，§6.6） |
//! | `time_on_page` `scroll_depth` | `events` 的 engagement 事件：**engagement 组件** |
//! | `exit_rate` | 会话组件（按退出页分组的退出次数）÷ 事件组件的 `pageviews`：**退出组件** |
//! | `percentage` | `visitors` ÷ 无维度的访客总数（另一次查询） |
//! | `conversion_rate` | 事件组件的 `visitors` ÷ **转化分母组件**：`events` 上去掉 `event:goal` 叶子的过滤、同样的其余维度的访客数，`event:goal` 维度在这个组件里展开成全部目标标签（`arrayJoin`），所以每个目标行都拿到同一组其余维度值的分母；有维度时外层 `HAVING` 去掉没有转化访客的行 |
//!
//! 「组合」表：事件组件遇到会话级的过滤，用 `session_id IN (…sessions…)`；遇到会话级的维度（入口页、退出页）用
//! `ANY LEFT JOIN` 一个按会话聚合的子查询；会话组件遇到事件级的过滤，用 `session_id IN (…events…)`（「浏览过」语义）。
//! 多个组件的结果用 `UNION ALL` 拼成同一组列，再按维度整体聚合：每个度量只在一个组件里非零，
//! 比率在外层用分子分母重新计算，所以排序与分页在 SQL 里完成。
//!
//! 同一个 `visits` 的口径只取决于**维度**，不取决于同时查了哪些别的指标：
//! 有 `event:*` 维度时是「含该维度值的事件所属的不同会话数」，否则是「区间内活跃的会话数」。

use std::collections::BTreeMap;

use serde::Serialize;

use crate::resolve::{Col, RDim, ResolvedQuery};
use crate::spec::Metric;

/// 数据层。
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Tier {
    Raw,
    Rollup,
}

/// 组件输出的度量列（`c_*`）。每个度量只由一个组件产生，其余组件在该列填 0。
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum Measure {
    Visitors,
    Pageviews,
    Events,
    Visits,
    /// 会话组件的会话数（`views_per_visit`、`visit_duration` 的分母）。
    SessN,
    SessPv,
    SessDurMs,
    SessEngMs,
    SessEngN,
    /// 跳出口径的会话数与跳出数。
    BounceN,
    Bounces,
    Exits,
    TpMs,
    TpN,
    ScSum,
    ScN,
    /// `conversion_rate` 的分母：去掉目标过滤后的访客数（转化分母组件）。
    CrBase,
}

impl Measure {
    pub(crate) fn col(self) -> &'static str {
        match self {
            Measure::Visitors => "c_visitors",
            Measure::Pageviews => "c_pageviews",
            Measure::Events => "c_events",
            Measure::Visits => "c_visits",
            Measure::SessN => "c_s_n",
            Measure::SessPv => "c_s_pv",
            Measure::SessDurMs => "c_s_dur",
            Measure::SessEngMs => "c_s_eng_ms",
            Measure::SessEngN => "c_s_eng_n",
            Measure::BounceN => "c_b_n",
            Measure::Bounces => "c_b_bounces",
            Measure::Exits => "c_x_exits",
            Measure::TpMs => "c_tp_ms",
            Measure::TpN => "c_tp_n",
            Measure::ScSum => "c_sc_sum",
            Measure::ScN => "c_sc_n",
            Measure::CrBase => "c_cr_base",
        }
    }
}

/// `visits` 的口径。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum VisitsForm {
    /// 事件形式：含维度值的（非 engagement）事件所属的不同会话数。
    Events,
    /// 会话形式：区间内活跃的会话数。
    Sessions,
}

#[derive(Clone, Debug)]
pub(crate) struct Plan {
    pub(crate) measures: Vec<Measure>,
    pub(crate) visits_form: VisitsForm,
    /// 需要事件组件 / engagement 组件 / 会话组件 / 跳出组件 / 退出组件。
    pub(crate) events: bool,
    pub(crate) engagement: bool,
    pub(crate) sessions: bool,
    /// `bounce_rate` 的会话口径与会话组件不同（过滤里有 `event:page` / `event:hostname`）：单独一个组件。
    pub(crate) bounce_separate: bool,
    pub(crate) exits: bool,
    /// `percentage`：先查无维度的访客总数。
    pub(crate) needs_total: bool,
    /// `conversion_rate`：转化分母组件（去掉目标过滤的访客数，`event:goal` 维度展开成全部目标标签）；
    /// 有维度时外层 `HAVING` 去掉没有转化访客的行。
    pub(crate) conv_base: bool,
}

pub(crate) fn plan(q: &ResolvedQuery) -> Plan {
    let mut measures: Vec<Measure> = Vec::new();
    let mut need = |ms: &[Measure]| {
        for m in ms {
            if !measures.contains(m) {
                measures.push(*m);
            }
        }
    };
    let has_event_dim = q
        .dims
        .iter()
        .any(|d| matches!(d, RDim::Col(c) if c.is_event_level()));
    let visits_form = if has_event_dim {
        VisitsForm::Events
    } else {
        VisitsForm::Sessions
    };
    for m in &q.metrics {
        match m {
            Metric::Visitors => need(&[Measure::Visitors]),
            Metric::Pageviews => need(&[Measure::Pageviews]),
            Metric::Events => need(&[Measure::Events]),
            Metric::Visits => need(&[Measure::Visits]),
            Metric::ViewsPerVisit => need(&[Measure::SessPv, Measure::SessN]),
            Metric::BounceRate => need(&[Measure::Bounces, Measure::BounceN]),
            Metric::VisitDuration => need(&[Measure::SessDurMs, Measure::SessN]),
            Metric::EngagedDuration => need(&[Measure::SessEngMs, Measure::SessEngN]),
            Metric::Percentage => need(&[Measure::Visitors]),
            Metric::ExitRate => need(&[Measure::Exits, Measure::Pageviews]),
            Metric::TimeOnPage => need(&[Measure::TpMs, Measure::TpN]),
            Metric::ScrollDepth => need(&[Measure::ScSum, Measure::ScN]),
            Metric::ConversionRate => need(&[Measure::Visitors, Measure::CrBase]),
            Metric::GroupConversionRate => {
                unreachable!("不支持的指标已在 resolve 阶段被拒绝")
            }
        }
    }
    measures.sort();

    let events = [Measure::Visitors, Measure::Pageviews, Measure::Events]
        .iter()
        .any(|m| measures.contains(m))
        || (visits_form == VisitsForm::Events && measures.contains(&Measure::Visits));
    let engagement = [Measure::TpMs, Measure::TpN, Measure::ScSum, Measure::ScN]
        .iter()
        .any(|m| measures.contains(m));
    let bounce = measures.contains(&Measure::Bounces);
    let page_leaf = q.filter.as_ref().is_some_and(|f| f.has_page_leaf());
    let bounce_separate = bounce && page_leaf;
    let session_measures = [
        Measure::SessN,
        Measure::SessPv,
        Measure::SessDurMs,
        Measure::SessEngMs,
        Measure::SessEngN,
    ]
    .iter()
    .any(|m| measures.contains(m))
        || (visits_form == VisitsForm::Sessions && measures.contains(&Measure::Visits));
    let sessions = session_measures || (bounce && !bounce_separate);
    Plan {
        visits_form,
        events,
        engagement,
        sessions,
        bounce_separate,
        exits: measures.contains(&Measure::Exits),
        needs_total: q.metrics.contains(&Metric::Percentage),
        conv_base: measures.contains(&Measure::CrBase),
        measures,
    }
}

// ------------------------------------------------------------------------------------------
// meta.metric_semantics
// ------------------------------------------------------------------------------------------

/// 每个被请求的指标的口径说明（英文），随过滤与维度而变（§6.6）。
pub(crate) fn metric_semantics(q: &ResolvedQuery, p: &Plan) -> BTreeMap<String, String> {
    let page_filter = q.filter.as_ref().is_some_and(|f| f.has_page_leaf());
    let event_filter = q.filter.as_ref().is_some_and(|f| f.has_event_leaf());
    let entry_dim = q
        .dims
        .iter()
        .any(|d| matches!(d, RDim::Col(Col::EventPage | Col::EventHostname)));
    let goal_dim = q
        .dims
        .iter()
        .any(|d| matches!(d, RDim::Col(Col::EventGoal)));
    let mut out = BTreeMap::new();
    for m in &q.metrics {
        let mut text = match m {
            Metric::Visitors => "Distinct visitors (approximate `uniq` of the per-day visitor id) with at least one non-engagement event in the range; computed from events, so it equals the sum of the per-local-day visitor counts.".to_owned(),
            Metric::Pageviews => "Number of pageview events in the range (engagement events are never counted).".to_owned(),
            Metric::Events => "Number of non-engagement events (pageviews and custom events) in the range.".to_owned(),
            Metric::Visits => match p.visits_form {
                VisitsForm::Events => "Distinct sessions that have at least one matching non-engagement event in the range (grouped by the event dimension values).".to_owned(),
                VisitsForm::Sessions => "Sessions active in the range: last non-engagement event at or after the start and session start before the end (a session spanning the range boundary counts in every range it is active in).".to_owned(),
            },
            Metric::ViewsPerVisit => "Pageviews per session over the sessions active in the range, using each session's total pageview count.".to_owned(),
            Metric::BounceRate => "Share (0 to 1) of sessions active in the range that have at most one pageview and no interactive custom event. Sessions are not cut at the range boundary.".to_owned(),
            Metric::VisitDuration => "Average session duration in seconds over the sessions active in the range: last non-engagement event minus session start, engagement time excluded.".to_owned(),
            Metric::EngagedDuration => "Average engaged time in seconds per session, over the active sessions that reported engagement time (engagement_ms > 0).".to_owned(),
            Metric::Percentage => "Visitors of the row divided by the total visitors of the same query without dimensions (0 to 1).".to_owned(),
            Metric::ExitRate => "Exits of the page divided by its pageviews: sessions active in the range whose last pageview is the page, over pageviews of the page in the range. Can exceed 1 when the exit happens after the range.".to_owned(),
            Metric::TimeOnPage => "Average engagement time in seconds per session that has engagement events matching the row, computed from engagement events only.".to_owned(),
            Metric::ScrollDepth => "Average over sessions of the maximum scroll depth (0 to 100) reported by engagement events matching the row; engagement events without a scroll depth are ignored.".to_owned(),
            Metric::ConversionRate => "Visitors of the row who completed the goal (a matching non-engagement event in the range) divided by the visitors of the same range with the same filters except the event:goal filter conditions (removed from the filter tree), counted per value of the row's other dimensions (0 to 1). With dimensions, rows without converting visitors are omitted.".to_owned(),
            Metric::GroupConversionRate => String::new(),
        };
        if goal_dim
            && matches!(
                m,
                Metric::Visitors | Metric::Visits | Metric::Events | Metric::Pageviews
            )
        {
            text.push_str(
                " With an event:goal dimension each row counts the events that complete the goal (event goals: the event name; page goals: pageviews whose pathname matches the pattern; scroll depth goals: engagement events on a matching pathname whose scroll depth is at least the threshold, which count towards visitors and visits only, so events and pageviews are 0 for those rows); an event completing several goals counts in each.",
            );
        }
        if event_filter {
            match m {
                Metric::Visitors | Metric::Pageviews | Metric::Events => text.push_str(
                    " With event-level filters (event:page, event:hostname, event:name, event:goal) only the matching events are counted: visitors and pageviews are those who viewed the matching pages or completed the goal.",
                ),
                Metric::BounceRate if page_filter => text.push_str(
                    " With an event:page or event:hostname filter only sessions whose entry page matches are counted (and the session must also have a matching event in the range).",
                ),
                Metric::Visits
                | Metric::ViewsPerVisit
                | Metric::VisitDuration
                | Metric::EngagedDuration
                | Metric::BounceRate => text.push_str(
                    " With event-level filters the sessions that have a matching event in the range (viewed a matching page, completed the goal) are counted.",
                ),
                _ => {}
            }
        }
        if entry_dim
            && matches!(
                m,
                Metric::ViewsPerVisit
                    | Metric::BounceRate
                    | Metric::VisitDuration
                    | Metric::EngagedDuration
            )
        {
            text.push_str(
                " With an event:page or event:hostname dimension, sessions are grouped by their entry page.",
            );
        }
        out.insert(m.as_str().to_owned(), text);
    }
    out
}
