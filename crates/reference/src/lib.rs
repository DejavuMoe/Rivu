//! 指标口径的参考实现：判卷标准，不是生产代码。
//!
//! 它**独立于** `rivu-session` 的 `SessionAssigner`：不共享任何会话状态、哈希或索引，
//! 只从「真实的人（`person`）」和原始事件出发，按口径直接切会话、算指标。
//! 管线（身份 → SessionAssigner → 写入 → 查询）的结果要与这里逐项比较（`docs/brief-p1.md`「方法」）。
//!
//! 口径来源：
//! - 会话切分：`docs/architecture/system-design.md` §4.6（续期 ≤ 30 min 且距开始 ≤ 7 d）与 §10 Q7；
//! - 会话聚合列：§5.3；派生指标：§5.3、§6.5；
//! - `visitors` 的真值：§10 Q9 的定义，即「各站点本地日的不同人数之和」。
//!
//! 为了保证它简单到不会错，这里宁可 O(n log n) 排序、整体遍历，也不做任何增量或索引优化。

use std::collections::{BTreeSet, HashSet};

use jiff::Timestamp;
use jiff::civil::Date;
use jiff::tz::TimeZone;
use rivu_domain::EventKind;

/// 会话不活跃超时：30 分钟（§4.6）。
pub const TIMEOUT_MS: i64 = 30 * 60 * 1000;
/// 会话最长时长：7 天（§4.6、§10 Q7）。
pub const MAX_DURATION_MS: i64 = 7 * 24 * 60 * 60 * 1000;

/// 一个事件「按自己的请求」算出的会话属性（测试作者按研究文档手写的真值，不调用任何富化代码）。
/// 会话的属性由**会话首事件**决定：同一会话里后续事件的这些字段会被忽略（管线把首事件的属性盖到每个事件上）。
/// 空串表示没有：来源为空是 Direct / None，国家为空是未知，设备为空是未知。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EventAttrs {
    /// 规范来源名（`Google`、`X (Twitter)`……）。
    pub referrer_source: String,
    /// 渠道（`Organic Search`、`Direct`……）。
    pub channel: String,
    /// ISO 3166-1 alpha-2。
    pub country: String,
    /// desktop / mobile / tablet。
    pub device: String,
}

/// 一条原始事件，带上测试里已知的「真实的人」。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RawEvent {
    pub site_id: u64,
    /// 真实的人。同一个 `(site_id, person)` 的所有事件属于同一个人。
    pub person: u64,
    pub ts_ms: i64,
    pub kind: EventKind,
    pub hostname: String,
    pub pathname: String,
    pub interactive: bool,
    /// 只有 engagement 事件有值。
    pub engagement_ms: u32,
    /// 该事件自己的会话属性；只有会话首事件的值有意义（见 [`EventAttrs`]）。
    pub attrs: EventAttrs,
}

/// 参考实现切出的一个会话，字段与 `sessions` 表（§5.3）的聚合列一一对应。
#[derive(Clone, Debug, PartialEq)]
pub struct RefSession {
    pub site_id: u64,
    pub person: u64,
    /// 属于该会话的事件在输入切片里的下标，升序（同一个人按 `ts` 稳定排序后的先后）。
    pub event_indices: Vec<usize>,
    pub started_at_ms: i64,
    /// 不含 engagement。
    pub last_event_at_ms: i64,
    /// 含 engagement。
    pub last_seen_at_ms: i64,
    pub pageviews: u64,
    /// 不含 engagement。
    pub events: u64,
    /// 非 pageview、非 engagement，且 `interactive`。
    pub interactive_events: u64,
    pub engagement_ms: u64,
    /// 第一个 pageview 的 `(hostname, pathname)`。
    pub entry: Option<(String, String)>,
    /// 最后一个 pageview 的 `(hostname, pathname)`；同一毫秒取输入里靠后的。
    pub exit: Option<(String, String)>,
}

impl RefSession {
    fn begin(e: &RawEvent) -> Self {
        Self {
            site_id: e.site_id,
            person: e.person,
            event_indices: Vec::new(),
            started_at_ms: e.ts_ms,
            last_event_at_ms: e.ts_ms,
            last_seen_at_ms: e.ts_ms,
            pageviews: 0,
            events: 0,
            interactive_events: 0,
            engagement_ms: 0,
            entry: None,
            exit: None,
        }
    }

    fn absorb(&mut self, index: usize, e: &RawEvent) {
        self.event_indices.push(index);
        self.last_seen_at_ms = self.last_seen_at_ms.max(e.ts_ms);
        self.engagement_ms += u64::from(e.engagement_ms);
        if e.kind == EventKind::Engagement {
            return;
        }
        self.last_event_at_ms = self.last_event_at_ms.max(e.ts_ms);
        self.events += 1;
        match e.kind {
            EventKind::Pageview => {
                self.pageviews += 1;
                let page = (e.hostname.clone(), e.pathname.clone());
                if self.entry.is_none() {
                    self.entry = Some(page.clone());
                }
                self.exit = Some(page);
            }
            EventKind::Custom if e.interactive => self.interactive_events += 1,
            _ => {}
        }
    }
}

/// 按「人」切会话，用默认的 30 min / 7 d。
pub fn sessionize(events: &[RawEvent]) -> Vec<RefSession> {
    sessionize_with(events, TIMEOUT_MS, MAX_DURATION_MS)
}

/// 同 [`sessionize`]，但超时和最长时长可调（测试用来把 7 天上限压缩到几小时）。
///
/// 规则：同一 `(site_id, person)` 的事件按 `ts` 稳定排序（同一毫秒保持输入顺序）。
/// 当前会话存在，且 `ts - last_seen ≤ timeout` 并且 `ts - started_at ≤ max_duration` 时事件并入；
/// 否则 engagement 被丢弃（不开会话，也不改动任何已有会话），其余事件开新会话。
/// 返回值按 `(site_id, person, 开始时间)` 排序。
pub fn sessionize_with(
    events: &[RawEvent],
    timeout_ms: i64,
    max_duration_ms: i64,
) -> Vec<RefSession> {
    let mut order: Vec<usize> = (0..events.len()).collect();
    // `sort_by_key` 是稳定排序，同一 ts 保持输入顺序。
    order.sort_by_key(|&i| (events[i].site_id, events[i].person, events[i].ts_ms));

    let mut out = Vec::new();
    let mut cur: Option<RefSession> = None;
    for i in order {
        let e = &events[i];
        if cur
            .as_ref()
            .is_some_and(|c| c.site_id != e.site_id || c.person != e.person)
        {
            out.extend(cur.take());
        }
        let joins = cur.as_ref().is_some_and(|c| {
            e.ts_ms - c.last_seen_at_ms <= timeout_ms
                && e.ts_ms - c.started_at_ms <= max_duration_ms
        });
        if !joins {
            if e.kind == EventKind::Engagement {
                continue;
            }
            out.extend(cur.take());
            cur = Some(RefSession::begin(e));
        }
        if let Some(c) = cur.as_mut() {
            c.absorb(i, e);
        }
    }
    out.extend(cur);
    out
}

/// 一个查询范围的核心指标。
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Metrics {
    pub visitors: u64,
    pub visits: u64,
    pub pageviews: u64,
    /// 0–1。
    pub bounce_rate: f64,
    /// 秒。
    pub visit_duration: f64,
    /// 秒。
    pub engaged_duration: f64,
}

fn date_of(ts_ms: i64, tz: &TimeZone) -> Date {
    Timestamp::from_millisecond(ts_ms)
        .expect("时间戳在 jiff 的范围内")
        .to_zoned(tz.clone())
        .date()
}

/// 站点本地日期 `date` 的 00:00（处理 DST 缺口：落在缺口里就取缺口之后的第一个时刻）对应的 UTC 毫秒。
fn day_start_ms(date: Date, tz: &TimeZone) -> i64 {
    date.to_zoned(tz.clone())
        .expect("日期在 jiff 的范围内")
        .timestamp()
        .as_millisecond()
}

/// 范围 = 站点本地日期 `[from, to]`（含），时区 `tz`。
///
/// - `visitors`：Σ 每个本地日（该日有**非 engagement** 事件的不同 `person` 数），即 §10 Q9 的真值。
///   §6.5 写的是 `uniq(visitor_id)`，没有说是否排除 engagement；engagement 只会落在已有会话里，
///   会话里一定有更早的非 engagement 事件，但那个事件可能在前一天，所以排除与否会影响跨午夜的数字。
///   这里按「访客 = 访问过的人」排除 engagement，与 `realtime`（§6.5）同口径。
/// - `pageviews`：范围内（按事件时间）的 pageview 数。
/// - `visits`：区间内活跃的会话，`last_event_at ≥ 范围起点` 且 `started_at < 范围终点（次日 00:00）`（§6.5）。
/// - `bounce_rate`：`pageviews ≤ 1 AND interactive_events = 0` 的会话 / visits（§5.3）。
/// - `visit_duration`：`avg(last_event_at − started_at)`，秒，不含 engagement（§5.3、Q2）。
/// - `engaged_duration`：`engagement_ms > 0` 的会话里 `sum(engagement_ms) / 1000 / 这类会话数`（§6.5）。
///
/// 所有比率分母为 0 时取 0（§6.5 的 `ifNotFinite`）。`from > to` 时全为 0。
pub fn metrics(
    events: &[RawEvent],
    sessions: &[RefSession],
    site_id: u64,
    tz: &TimeZone,
    from: Date,
    to: Date,
) -> Metrics {
    if from > to {
        return Metrics::default();
    }
    let start_ms = day_start_ms(from, tz);
    let end_ms = day_start_ms(to.tomorrow().expect("日期在 jiff 的范围内"), tz);

    let mut daily: HashSet<(Date, u64)> = HashSet::new();
    let mut pageviews = 0u64;
    for e in events.iter().filter(|e| e.site_id == site_id) {
        if e.kind == EventKind::Engagement {
            continue;
        }
        let d = date_of(e.ts_ms, tz);
        if d >= from && d <= to {
            daily.insert((d, e.person));
        }
        if e.kind == EventKind::Pageview && e.ts_ms >= start_ms && e.ts_ms < end_ms {
            pageviews += 1;
        }
    }

    let active: Vec<&RefSession> = sessions
        .iter()
        .filter(|s| {
            s.site_id == site_id && s.last_event_at_ms >= start_ms && s.started_at_ms < end_ms
        })
        .collect();
    let visits = active.len() as u64;

    let mut bounces = 0u64;
    let mut duration_ms_sum = 0i128;
    let mut engaged_sessions = 0u64;
    let mut engaged_ms_sum = 0u64;
    for s in &active {
        if s.pageviews <= 1 && s.interactive_events == 0 {
            bounces += 1;
        }
        duration_ms_sum += i128::from(s.last_event_at_ms - s.started_at_ms);
        if s.engagement_ms > 0 {
            engaged_sessions += 1;
            engaged_ms_sum += s.engagement_ms;
        }
    }
    let ratio = |num: f64, den: u64| if den == 0 { 0.0 } else { num / den as f64 };
    Metrics {
        visitors: daily.len() as u64,
        visits,
        pageviews,
        bounce_rate: ratio(bounces as f64, visits),
        visit_duration: ratio(duration_ms_sum as f64 / 1000.0, visits),
        engaged_duration: ratio(engaged_ms_sum as f64 / 1000.0, engaged_sessions),
    }
}

/// 一个站点在 `[from, to]` 内每个本地日的访客数（排序后的 `(日期, 人数)`）。
/// 与 [`metrics`] 的 `visitors` 同口径，供测试逐日对照。
pub fn daily_visitors(
    events: &[RawEvent],
    site_id: u64,
    tz: &TimeZone,
    from: Date,
    to: Date,
) -> Vec<(Date, u64)> {
    let mut set: BTreeSet<(Date, u64)> = BTreeSet::new();
    for e in events
        .iter()
        .filter(|e| e.site_id == site_id && e.kind != EventKind::Engagement)
    {
        let d = date_of(e.ts_ms, tz);
        if d >= from && d <= to {
            set.insert((d, e.person));
        }
    }
    let mut out: Vec<(Date, u64)> = Vec::new();
    for (d, _) in set {
        match out.last_mut() {
            Some((ld, n)) if *ld == d => *n += 1,
            _ => out.push((d, 1)),
        }
    }
    out
}

/// 分组维度。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RefDim {
    /// 页面：只看 pageview。
    Page,
    Source,
    Channel,
    Country,
    Device,
}

/// 分组的一行（已按展示文案命名空值）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RefBreakdownRow {
    pub value: String,
    pub visitors: u64,
    pub visits: u64,
    pub pageviews: u64,
}

/// 空值的展示文案。故意与 `rivu-query` 各写一份：参考实现不共享查询层的任何代码。
fn empty_label(dim: RefDim) -> &'static str {
    match dim {
        RefDim::Page => "(none)",
        RefDim::Source => "Direct / None",
        RefDim::Channel => "Direct",
        RefDim::Country | RefDim::Device => "(not set)",
    }
}

/// 会话属性维度上，一个会话的取值：会话**首事件**（按时间、同一毫秒按输入顺序的第一个）的属性。
fn session_value(dim: RefDim, s: &RefSession, events: &[RawEvent]) -> String {
    let a = &events[s.event_indices[0]].attrs;
    match dim {
        RefDim::Source => a.referrer_source.clone(),
        RefDim::Channel => a.channel.clone(),
        RefDim::Country => a.country.clone(),
        RefDim::Device => a.device.clone(),
        RefDim::Page => unreachable!("页面不是会话属性"),
    }
}

/// 按维度分组，范围 = 站点本地日期 `[from, to]`（含），时区 `tz`；口径：
///
/// - `visitors`：Σ 每个本地日（该日在该维度值下有**非 engagement** 事件的不同 `person` 数），
///   与 [`metrics`] 的 `visitors` 同口径；会话属性维度里，事件的维度值是它所在会话的首事件属性；
/// - `pageviews`：范围内、维度值等于该值的 pageview 数；
/// - `visits`：会话属性维度 = 区间内活跃的会话（口径同 [`metrics`]）按首事件属性分组的数量；
///   页面维度 = 范围内浏览过该页的不同会话数；
/// - 页面维度只看 pageview（`visitors` 也是）；
/// - 空值换成展示文案；三个数都是 0 的行丢掉；
/// - 排序：访客数、会话数、浏览次数降序，同数按**原始**维度值（空串最小）升序；取前 `limit` 行。
#[allow(clippy::too_many_arguments)] // 与 `rivu_query::breakdown` 的参数一一对应
pub fn breakdown(
    events: &[RawEvent],
    sessions: &[RefSession],
    site_id: u64,
    tz: &TimeZone,
    from: Date,
    to: Date,
    dim: RefDim,
    limit: usize,
) -> Vec<RefBreakdownRow> {
    if from > to {
        return Vec::new();
    }
    let start_ms = day_start_ms(from, tz);
    let end_ms = day_start_ms(to.tomorrow().expect("日期在 jiff 的范围内"), tz);

    // 事件所属会话：事件下标 -> 会话下标
    let mut owner: Vec<Option<usize>> = vec![None; events.len()];
    for (si, s) in sessions.iter().enumerate() {
        for &i in &s.event_indices {
            owner[i] = Some(si);
        }
    }

    #[derive(Default)]
    struct Acc {
        persons: HashSet<(Date, u64)>,
        sessions: HashSet<usize>,
        visits: u64,
        pageviews: u64,
    }
    let mut groups: std::collections::BTreeMap<String, Acc> = std::collections::BTreeMap::new();

    for (i, e) in events.iter().enumerate() {
        if e.site_id != site_id || e.ts_ms < start_ms || e.ts_ms >= end_ms {
            continue;
        }
        // 没有并入任何会话的事件（被丢弃的 engagement）不在管线的输出里
        let Some(si) = owner[i] else { continue };
        let is_pv = e.kind == EventKind::Pageview;
        let value = match dim {
            RefDim::Page => {
                if !is_pv {
                    continue;
                }
                e.pathname.clone()
            }
            _ => session_value(dim, &sessions[si], events),
        };
        let acc = groups.entry(value).or_default();
        if e.kind != EventKind::Engagement {
            acc.persons.insert((date_of(e.ts_ms, tz), e.person));
        }
        if is_pv {
            acc.pageviews += 1;
            acc.sessions.insert(si);
        }
    }

    if dim != RefDim::Page {
        for s in sessions.iter().filter(|s| {
            s.site_id == site_id && s.last_event_at_ms >= start_ms && s.started_at_ms < end_ms
        }) {
            groups
                .entry(session_value(dim, s, events))
                .or_default()
                .visits += 1;
        }
    }

    let mut rows: Vec<(String, RefBreakdownRow)> = groups
        .into_iter()
        .map(|(raw, a)| {
            let visits = if dim == RefDim::Page {
                a.sessions.len() as u64
            } else {
                a.visits
            };
            let value = if raw.is_empty() {
                empty_label(dim).to_owned()
            } else {
                raw.clone()
            };
            (
                raw,
                RefBreakdownRow {
                    value,
                    visitors: a.persons.len() as u64,
                    visits,
                    pageviews: a.pageviews,
                },
            )
        })
        .filter(|(_, r)| r.visitors > 0 || r.visits > 0 || r.pageviews > 0)
        .collect();
    // BTreeMap 已按原始值升序；稳定排序保持这个次序作为最后的平手规则
    rows.sort_by(|(_, a), (_, b)| {
        (b.visitors, b.visits, b.pageviews).cmp(&(a.visitors, a.visits, a.pageviews))
    });
    rows.into_iter().map(|(_, r)| r).take(limit).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::civil::date;

    const MIN: i64 = 60_000;
    const DAY: i64 = 24 * 60 * MIN;

    fn ev(person: u64, ts_ms: i64, kind: EventKind, path: &str) -> RawEvent {
        RawEvent {
            site_id: 1,
            person,
            ts_ms,
            kind,
            hostname: "example.com".into(),
            pathname: path.into(),
            interactive: false,
            engagement_ms: 0,
            attrs: EventAttrs::default(),
        }
    }

    fn pv(person: u64, ts: i64, path: &str) -> RawEvent {
        ev(person, ts, EventKind::Pageview, path)
    }

    fn eng(person: u64, ts: i64, ms: u32) -> RawEvent {
        RawEvent {
            engagement_ms: ms,
            ..ev(person, ts, EventKind::Engagement, "/")
        }
    }

    #[test]
    fn timeout_boundary_is_inclusive() {
        let evs = [
            pv(1, 0, "/a"),
            pv(1, 30 * MIN, "/b"),
            pv(1, 60 * MIN + 1, "/c"),
        ];
        let s = sessionize(&evs);
        assert_eq!(s.len(), 2);
        assert_eq!(s[0].event_indices, vec![0, 1]);
        assert_eq!(s[1].event_indices, vec![2]);
    }

    #[test]
    fn engagement_extends_but_never_opens() {
        let evs = [
            eng(1, 0, 100),          // 没有会话：丢弃
            pv(1, MIN, "/a"),        // 开会话
            eng(1, 30 * MIN, 5_000), // 推进 last_seen 到 30min
            pv(1, 59 * MIN, "/b"),   // 距 last_seen 29min：并入
            eng(1, 200 * MIN, 1),    // 超时：丢弃，且不开会话
        ];
        let s = sessionize(&evs);
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].event_indices, vec![1, 2, 3]);
        assert_eq!(s[0].last_event_at_ms, 59 * MIN);
        assert_eq!(s[0].last_seen_at_ms, 59 * MIN);
        assert_eq!(s[0].engagement_ms, 5_000);
        assert_eq!((s[0].pageviews, s[0].events), (2, 2));
    }

    #[test]
    fn engagement_only_extends_last_seen_not_last_event() {
        let evs = [pv(1, 0, "/a"), eng(1, 20 * MIN, 1), pv(1, 49 * MIN, "/b")];
        let s = sessionize(&evs);
        assert_eq!(s.len(), 1, "engagement 让 49min 的 pageview 仍在超时内");
        let evs = [pv(1, 0, "/a"), pv(1, 49 * MIN, "/b")];
        assert_eq!(sessionize(&evs).len(), 2);
        let evs = [pv(1, 0, "/a"), eng(1, 20 * MIN, 1)];
        let s = sessionize(&evs);
        assert_eq!(s[0].last_event_at_ms, 0);
        assert_eq!(s[0].last_seen_at_ms, 20 * MIN);
    }

    #[test]
    fn seven_day_cap() {
        // 每 25 分钟一个事件，连续 8 天。
        let evs: Vec<RawEvent> = (0..=(8 * DAY / (25 * MIN)))
            .map(|i| pv(1, i * 25 * MIN, "/"))
            .collect();
        let s = sessionize(&evs);
        assert_eq!(s.len(), 2);
        assert!(s[0].last_event_at_ms - s[0].started_at_ms <= MAX_DURATION_MS);
        assert!(s[1].started_at_ms - s[0].started_at_ms > MAX_DURATION_MS);
        // 恰好 7 天（间隔 1 天，把超时放宽）仍然并入，多 1ms 就切开。
        let mut evs: Vec<RawEvent> = (0..=7).map(|d| pv(1, d * DAY, "/")).collect();
        evs.push(pv(1, 7 * DAY + 1, "/"));
        let s = sessionize_with(&evs, DAY, MAX_DURATION_MS);
        assert_eq!(s.len(), 2);
        assert_eq!(s[0].event_indices, (0..=7).collect::<Vec<_>>());
        assert_eq!(s[1].event_indices, vec![8]);
    }

    #[test]
    fn people_and_sites_are_separate() {
        let mut other_site = pv(1, 1, "/");
        other_site.site_id = 2;
        let evs = [pv(1, 0, "/"), pv(2, 1, "/"), other_site];
        assert_eq!(sessionize(&evs).len(), 3);
    }

    #[test]
    fn ties_keep_input_order_and_entry_exit() {
        let evs = [
            pv(1, 5, "/second-in-input"),
            pv(1, 5, "/third"),
            pv(1, 0, "/first"),
        ];
        let s = sessionize(&evs);
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].event_indices, vec![2, 0, 1]);
        assert_eq!(s[0].entry.as_ref().map(|p| p.1.as_str()), Some("/first"));
        assert_eq!(s[0].exit.as_ref().map(|p| p.1.as_str()), Some("/third"));
    }

    #[test]
    fn entry_is_none_before_first_pageview() {
        let mut c = ev(1, 0, EventKind::Custom, "/x");
        c.interactive = true;
        let s = sessionize(&[c, pv(1, MIN, "/a")]);
        assert_eq!(s[0].interactive_events, 1);
        assert_eq!(s[0].entry.as_ref().map(|p| p.1.as_str()), Some("/a"));
        let mut c = ev(1, 0, EventKind::Custom, "/x");
        c.interactive = false;
        let s = sessionize(&[c]);
        assert_eq!(s[0].entry, None);
        assert_eq!((s[0].pageviews, s[0].events), (0, 1));
    }

    #[test]
    fn metrics_by_hand() {
        // 1970-01-01，UTC。
        let evs = [
            pv(1, 0, "/a"),
            pv(1, MIN, "/b"),
            pv(2, 0, "/a"),
            eng(2, 10_000, 5_000),
        ];
        let s = sessionize(&evs);
        let tz = TimeZone::UTC;
        let d = date(1970, 1, 1);
        let m = metrics(&evs, &s, 1, &tz, d, d);
        assert_eq!(m.visitors, 2);
        assert_eq!(m.visits, 2);
        assert_eq!(m.pageviews, 3);
        assert_eq!(m.bounce_rate, 0.5);
        assert_eq!(m.visit_duration, 30.0);
        assert_eq!(m.engaged_duration, 5.0);
        // 别的站点、别的日子：全零。
        assert_eq!(metrics(&evs, &s, 9, &tz, d, d), Metrics::default());
        let later = date(1970, 1, 3);
        assert_eq!(metrics(&evs, &s, 1, &tz, later, later), Metrics::default());
    }

    #[test]
    fn interactive_custom_event_prevents_bounce() {
        let mut c = ev(1, MIN, EventKind::Custom, "/");
        c.interactive = true;
        let evs = [pv(1, 0, "/a"), c];
        let s = sessionize(&evs);
        let d = date(1970, 1, 1);
        let m = metrics(&evs, &s, 1, &TimeZone::UTC, d, d);
        assert_eq!((m.visits, m.pageviews, m.bounce_rate), (1, 1, 0.0));
    }

    #[test]
    fn visitors_are_summed_per_local_day_not_deduplicated_across_days() {
        // Shanghai（UTC+8）：同一个人跨午夜的一个会话，次日又开一个新会话。
        let tz = TimeZone::get("Asia/Shanghai").unwrap();
        let t = |h: i8, m: i8, day: i8| {
            date(2026, 5, day)
                .at(h, m, 0, 0)
                .to_zoned(tz.clone())
                .unwrap()
                .timestamp()
                .as_millisecond()
        };
        let evs = [
            pv(1, t(23, 50, 1), "/a"),
            pv(1, t(0, 10, 2), "/b"), // 跨午夜，续上
            pv(1, t(5, 0, 2), "/c"),  // 新会话
        ];
        let s = sessionize(&evs);
        assert_eq!(s.len(), 2);
        let m = metrics(&evs, &s, 1, &tz, date(2026, 5, 2), date(2026, 5, 2));
        assert_eq!(m.visitors, 1);
        assert_eq!(m.visits, 2); // 两个会话在 5/2 都活跃
        let m = metrics(&evs, &s, 1, &tz, date(2026, 5, 1), date(2026, 5, 2));
        assert_eq!(m.visitors, 2, "跨天范围按各日求和");
        assert_eq!(m.visits, 2);
        assert_eq!(
            daily_visitors(&evs, 1, &tz, date(2026, 5, 1), date(2026, 5, 2)),
            vec![(date(2026, 5, 1), 1), (date(2026, 5, 2), 1)]
        );
    }

    #[test]
    fn dst_days_have_23_and_25_hours() {
        let tz = TimeZone::get("Europe/Berlin").unwrap();
        let ms = |y: i16, mo: i8, d: i8| day_start_ms(date(y, mo, d), &tz);
        assert_eq!(ms(2026, 3, 30) - ms(2026, 3, 29), 23 * 60 * MIN);
        assert_eq!(ms(2026, 10, 26) - ms(2026, 10, 25), 25 * 60 * MIN);
        // 范围终点是「次日 00:00」：DST 日里 23:30 的事件仍属于当天。
        let evs = [pv(1, ms(2026, 3, 30) - 30 * MIN, "/")];
        let s = sessionize(&evs);
        let d = date(2026, 3, 29);
        let m = metrics(&evs, &s, 1, &tz, d, d);
        assert_eq!((m.visitors, m.visits, m.pageviews), (1, 1, 1));
        let d = date(2026, 3, 30);
        assert_eq!(metrics(&evs, &s, 1, &tz, d, d), Metrics::default());
    }

    fn pv_with(person: u64, ts: i64, path: &str, source: &str, device: &str) -> RawEvent {
        RawEvent {
            attrs: EventAttrs {
                referrer_source: source.into(),
                channel: if source.is_empty() {
                    "Direct"
                } else {
                    "Referral"
                }
                .into(),
                country: if device == "mobile" { "" } else { "SE" }.into(),
                device: device.into(),
            },
            ..pv(person, ts, path)
        }
    }

    fn rows(evs: &[RawEvent], dim: RefDim, limit: usize) -> Vec<(String, u64, u64, u64)> {
        let s = sessionize(evs);
        let d = date(1970, 1, 1);
        breakdown(evs, &s, 1, &TimeZone::UTC, d, d, dim, limit)
            .into_iter()
            .map(|r| (r.value, r.visitors, r.visits, r.pageviews))
            .collect()
    }

    #[test]
    fn breakdown_by_hand_uses_the_first_events_attributes() {
        let evs = [
            pv_with(1, 0, "/a", "Google", "desktop"),
            // 同一会话的第二个事件自己的来源是 Bing：被忽略，会话仍是 Google
            pv_with(1, MIN, "/b", "Bing", "desktop"),
            eng(1, 2 * MIN, 1000),
            pv_with(2, 0, "/a", "", "mobile"),
            // 没有会话的 engagement：丢弃，不出现在任何维度里
            eng(3, 0, 5),
        ];
        let t = |v: &str, a, b, c| (v.to_owned(), a, b, c);
        assert_eq!(
            rows(&evs, RefDim::Source, 10),
            vec![t("Google", 1, 1, 2), t("Direct / None", 1, 1, 1)]
        );
        assert_eq!(
            rows(&evs, RefDim::Channel, 10),
            vec![t("Referral", 1, 1, 2), t("Direct", 1, 1, 1)]
        );
        assert_eq!(
            rows(&evs, RefDim::Device, 10),
            vec![t("desktop", 1, 1, 2), t("mobile", 1, 1, 1)]
        );
        assert_eq!(
            rows(&evs, RefDim::Country, 10),
            vec![t("SE", 1, 1, 2), t("(not set)", 1, 1, 1)]
        );
        // 页面：只看 pageview；visits = 浏览过该页的会话数
        assert_eq!(
            rows(&evs, RefDim::Page, 10),
            vec![t("/a", 2, 2, 2), t("/b", 1, 1, 1)]
        );
        // limit 在排序之后截断
        assert_eq!(rows(&evs, RefDim::Page, 1), vec![t("/a", 2, 2, 2)]);
    }

    #[test]
    fn breakdown_visits_count_active_sessions_even_without_events_in_range() {
        // 会话从 1/1 的 23:50 持续到 1/2 的 00:10：1/2 的范围里它有 1 个 visit，
        // 但开会话的 /a 在 1/1，所以只有 1 个 pageview（/b），属性仍取首事件（Google）
        let evs = [
            pv_with(1, 23 * 60 * MIN + 50 * MIN, "/a", "Google", "desktop"),
            pv_with(1, DAY + 10 * MIN, "/b", "Bing", "desktop"),
            eng(1, DAY + 12 * MIN, 1000),
        ];
        let s = sessionize(&evs);
        let d2 = date(1970, 1, 2);
        let r = breakdown(&evs, &s, 1, &TimeZone::UTC, d2, d2, RefDim::Source, 10);
        assert_eq!(r.len(), 1);
        assert_eq!(
            (
                r[0].value.as_str(),
                r[0].visitors,
                r[0].visits,
                r[0].pageviews
            ),
            ("Google", 1, 1, 1)
        );
    }

    #[test]
    fn visits_include_sessions_straddling_the_range() {
        let evs = [
            pv(1, 23 * 60 * MIN + 50 * MIN, "/a"),
            pv(1, 24 * 60 * MIN + 10 * MIN, "/b"),
        ];
        let s = sessionize(&evs);
        let d2 = date(1970, 1, 2);
        let m = metrics(&evs, &s, 1, &TimeZone::UTC, d2, d2);
        assert_eq!((m.visits, m.visitors, m.pageviews), (1, 1, 1));
        assert_eq!(m.visit_duration, 20.0 * 60.0);
    }
}
