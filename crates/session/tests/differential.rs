//! 差分测试：管线（身份 → SessionAssigner）与独立的参考实现（`rivu-reference`）逐项比较。
//!
//! 判卷标准是 `rivu_reference::sessionize` / `metrics`，它们从「真实的人」直接出发，
//! 不使用 SessionAssigner 的任何代码。见 `docs/brief-p1.md`「方法」。

mod common;

use std::collections::{HashMap, HashSet};

use jiff::civil::{Date, DateTime};
use jiff::tz::TimeZone;
use proptest::prelude::*;
use rivu_domain::EventKind;
use rivu_reference::{RawEvent, RefSession, metrics, sessionize};
use rivu_session::assigner::{Assignment, SHARDS};
use rivu_session::identity;

use common::*;

const SEC: i64 = 1_000;
const MIN: i64 = 60 * SEC;
const HOUR: i64 = 60 * MIN;

/// (时区, 本地起点)：起点都在本地午夜前几小时，流会跨过多个本地午夜；
/// Berlin 的两个起点分别覆盖 2026-03-29（23 h）与 2026-10-25（25 h）两个 DST 切换日。
const CASES: [(&str, &str); 6] = [
    ("UTC", "2026-05-01T20:00:00"),
    ("Asia/Shanghai", "2026-05-01T20:00:00"),
    ("Europe/Berlin", "2026-03-27T20:00:00"),
    ("Europe/Berlin", "2026-10-23T20:00:00"),
    ("Asia/Kathmandu", "2026-05-01T22:00:00"),
    ("America/Los_Angeles", "2026-03-06T20:00:00"),
];

fn tz_and_start(case: usize) -> (TimeZone, i64) {
    let (name, local) = CASES[case % CASES.len()];
    let tz = TimeZone::get(name).expect("时区存在");
    let start = local
        .parse::<DateTime>()
        .unwrap()
        .to_zoned(tz.clone())
        .unwrap()
        .timestamp()
        .as_millisecond();
    (tz, start)
}

#[derive(Clone, Debug)]
struct EvSpec {
    gap_ms: i64,
    site: u8,
    person: u8,
    kind: u8,
    path: u8,
    interactive: bool,
    engagement_ms: u32,
}

fn ev_spec() -> impl Strategy<Value = EvSpec> {
    let gap = prop_oneof![
        3 => 0i64..5 * SEC,
        3 => MIN..20 * MIN,
        2 => (30 * MIN - 10 * SEC)..(30 * MIN + 10 * SEC),
        1 => Just(30 * MIN),
        2 => (31 * MIN)..(6 * HOUR),
        1 => (6 * HOUR)..(30 * HOUR),
    ];
    (
        gap,
        0u8..2,
        0u8..4,
        0u8..20,
        0u8..5,
        any::<bool>(),
        1u32..60_000,
    )
        .prop_map(
            |(gap_ms, site, person, kind, path, interactive, engagement_ms)| EvSpec {
                gap_ms,
                site,
                person,
                kind,
                path,
                interactive,
                engagement_ms,
            },
        )
}

#[derive(Clone, Debug)]
struct Spec {
    case: usize,
    events: Vec<EvSpec>,
    swaps: Vec<bool>,
}

fn spec() -> impl Strategy<Value = Spec> {
    (
        0..CASES.len(),
        prop::collection::vec(ev_spec(), 1..120),
        prop::collection::vec(any::<bool>(), 120),
    )
        .prop_map(|(case, events, swaps)| Spec {
            case,
            events,
            swaps,
        })
}

fn build(spec: &Spec) -> (TimeZone, Vec<RawEvent>) {
    let (tz, start) = tz_and_start(spec.case);
    let mut ts = start;
    let events = spec
        .events
        .iter()
        .map(|s| {
            ts += s.gap_ms;
            let kind = match s.kind {
                0..=10 => EventKind::Pageview,
                11..=16 => EventKind::Engagement,
                _ => EventKind::Custom,
            };
            let mut e = raw(
                1 + u64::from(s.site),
                u64::from(s.person),
                ts,
                kind,
                &format!("/p{}", s.path),
            );
            e.interactive = s.interactive;
            if kind == EventKind::Engagement {
                e.engagement_ms = s.engagement_ms;
            }
            e
        })
        .collect();
    (tz, events)
}

/// 断言管线结果与参考实现完全一致；返回参考会话。
fn assert_matches_reference(
    events: &[RawEvent],
    tz: &TimeZone,
    assignments: &[Assignment],
) -> Vec<RefSession> {
    let reference = sessionize(events);
    let mut ref_groups: Vec<Vec<usize>> =
        reference.iter().map(|s| s.event_indices.clone()).collect();
    ref_groups.sort();
    assert_eq!(
        groups(assignments),
        ref_groups,
        "assigner 切出的会话与参考实现不同"
    );

    // NoSession 恰好是不属于任何参考会话的事件，而且只会是 engagement。
    let in_session: HashSet<usize> = ref_groups.iter().flatten().copied().collect();
    for (i, a) in assignments.iter().enumerate() {
        match a {
            Assignment::NoSession => {
                assert_eq!(
                    events[i].kind,
                    EventKind::Engagement,
                    "只有 engagement 会是 NoSession"
                );
                assert!(!in_session.contains(&i));
            }
            Assignment::Stamped(_) => assert!(in_session.contains(&i)),
        }
    }

    // 盖章字段。
    for rs in &reference {
        let first = rs.event_indices[0];
        let first_own = identity_of(&events[first], tz).0;
        let mut entry = String::new();
        for &i in &rs.event_indices {
            let Assignment::Stamped(s) = &assignments[i] else {
                unreachable!("已在上面核对过")
            };
            let (own, _, routing) = identity_of(&events[i], tz);
            if entry.is_empty() && events[i].kind == EventKind::Pageview {
                entry = events[i].pathname.clone();
            }
            assert_eq!(s.session_start_ms, rs.started_at_ms);
            assert_eq!(s.visitor_id, first_own, "visitor_id 是会话的原始 ID");
            assert_eq!(s.own_visitor_id, own);
            assert_eq!(&*s.entry_pathname, entry, "入口页");
            assert_eq!(
                s.attrs.browser,
                format!("e{first}"),
                "属性来自会话的第一个事件"
            );
            assert_eq!(u64::from(s.partition), routing % SHARDS as u64);
        }
        if let Some((_, path)) = &rs.entry {
            assert_eq!(path, &entry);
        } else {
            assert!(entry.is_empty());
        }
    }
    reference
}

/// 用 Stamp 的 id 按 (站点, 本地日, id) 去重得到每个站点的访客数（`use_own` 选哪个 id）。
fn stamp_visitors(
    events: &[RawEvent],
    tz: &TimeZone,
    assignments: &[Assignment],
    site: u64,
    from: Date,
    to: Date,
    use_own: bool,
) -> u64 {
    let mut set = HashSet::new();
    for (e, a) in events.iter().zip(assignments) {
        let Assignment::Stamped(s) = a else { continue };
        if e.site_id != site || e.kind == EventKind::Engagement {
            continue;
        }
        let d = identity::local_date(e.ts_ms, tz);
        if d >= from && d <= to {
            set.insert((
                d,
                if use_own {
                    s.own_visitor_id
                } else {
                    s.visitor_id
                },
            ));
        }
    }
    set.len() as u64
}

proptest! {
    // 集成测试里 proptest 找不到 lib.rs 来放回归文件，关掉持久化以免每次告警；失败时输出已含最小反例。
    #![proptest_config(ProptestConfig { failure_persistence: None, ..ProptestConfig::default() })]

    /// 1. 有序流：会话切分、NoSession、盖章字段、每日访客数都与参考实现一致；
    ///    中途穿插 `sweep()` 不改变任何结果（清扫只删不可能再续期的会话）。
    #[test]
    fn ordered_stream_matches_reference(spec in spec(), sweep_every in prop::option::of(1usize..15)) {
        let (tz, events) = build(&spec);
        let a = new_assigner(1);
        let assignments = run_pipeline(&a, &events, &tz, sweep_every);
        let reference = assert_matches_reference(&events, &tz, &assignments);

        let from = identity::local_date(events[0].ts_ms, &tz);
        let to = identity::local_date(events[events.len() - 1].ts_ms, &tz);
        for site in 1..=2u64 {
            let m = metrics(&events, &reference, site, &tz, from, to);
            // 访客真值 = 各本地日不同人数之和；用事件自己的 visitor_id 按 (本地日, id) 去重应当相等。
            prop_assert_eq!(m.visitors, stamp_visitors(&events, &tz, &assignments, site, from, to, true));
            // 整个范围内所有该站点的会话都是活跃的。
            prop_assert_eq!(m.visits, reference.iter().filter(|s| s.site_id == site).count() as u64);
            prop_assert!((0.0..=1.0).contains(&m.bounce_rate));
        }
    }

    /// 2. 重放：同一个流、同一个 seed 跑两遍，Stamp 序列完全相同（含 session_id）。
    #[test]
    fn replay_is_identical(spec in spec(), seed in any::<u64>()) {
        let (tz, events) = build(&spec);
        let first = run_pipeline(&new_assigner(seed), &events, &tz, None);
        let second = run_pipeline(&new_assigner(seed), &events, &tz, None);
        prop_assert_eq!(&first, &second);
        // 换种子只换 session_id，不改分组。
        let other = run_pipeline(&new_assigner(seed ^ 1), &events, &tz, None);
        prop_assert_eq!(groups(&first), groups(&other));
    }

    /// 3. 有界乱序（相邻事件交换，间隔 ≤ 5 s）：last_seen 不倒退、不 panic、会话数不少于有序时。
    #[test]
    fn bounded_disorder_invariants(spec in spec()) {
        let (tz, ordered) = build(&spec);
        let mut events = ordered.clone();
        let mut i = 0;
        while i + 1 < events.len() {
            if spec.swaps[i] && events[i + 1].ts_ms - events[i].ts_ms <= 5 * SEC {
                events.swap(i, i + 1);
                i += 2;
            } else {
                i += 1;
            }
        }

        let a = new_assigner(3);
        let mut last_seen: HashMap<u64, i64> = HashMap::new();
        let mut assignments = Vec::new();
        for (idx, e) in events.iter().enumerate() {
            let one = run_one(&a, e, &tz, idx);
            if let Assignment::Stamped(s) = &one {
                let (own, _, routing) = identity_of(e, &tz);
                if let Some(view) = a.peek(e.site_id, routing, own) {
                    prop_assert_eq!(view.session_id, s.session_id);
                    prop_assert!(view.last_seen_ms >= e.ts_ms, "last_seen 不小于刚处理的事件时间");
                    let prev = last_seen.insert(s.session_id, view.last_seen_ms).unwrap_or(i64::MIN);
                    prop_assert!(view.last_seen_ms >= prev, "last_seen 倒退了");
                }
            }
            assignments.push(one);
        }
        let ordered_sessions = sessionize(&ordered).len();
        prop_assert!(
            groups(&assignments).len() >= ordered_sessions,
            "乱序时会话数 {} 少于有序时 {}", groups(&assignments).len(), ordered_sessions
        );
    }
}

/// 只喂一个事件（不清扫），事件下标用来生成属性。
fn run_one(
    a: &rivu_session::assigner::SessionAssigner,
    e: &RawEvent,
    tz: &TimeZone,
    idx: usize,
) -> Assignment {
    let (visitor_id, prev, routing) = identity_of(e, tz);
    let attrs = attrs_for(idx);
    a.assign(&rivu_session::assigner::Incoming {
        site_id: e.site_id,
        visitor_id,
        prev_visitor_id: Some(prev),
        routing,
        ts_ms: e.ts_ms,
        kind: e.kind,
        pathname: &e.pathname,
        attrs: &attrs,
    })
}

/// 7 天上限与别名上限在真实时区规则下的表现：一个人每 29 分钟一个 pageview，连续 9.5 天，
/// 起点 2026-03-25 23:30（Berlin），途中经过 03-29 的春季 DST 切换（该日只有 23 小时）。
///
/// 7 天（168 h）从 03-25 23:30 CET 起算，在 04-02 00:30 CEST 结束，会话跨过 9 个本地日期，
/// 超过 8 个 key 的上限：这是 §4.6 说「正常情况不会发生」的情形之一（另一种是站点改时区）。
/// 结果仍与参考实现一致，因为第 9 天的事件经由 `prev_visitor_id`（第 8 个 key）命中会话，
/// 只是第 9 个 key 加不进去，`alias_cap_hit` 计数。
#[test]
fn long_running_person_across_dst_hits_alias_cap_but_stays_correct() {
    let tz = TimeZone::get("Europe/Berlin").unwrap();
    let start = "2026-03-25T23:30:00"
        .parse::<DateTime>()
        .unwrap()
        .to_zoned(tz.clone())
        .unwrap()
        .timestamp()
        .as_millisecond();
    let n = 9 * 24 * 60 / 29 + 16; // 9.5 天
    let events: Vec<RawEvent> = (0..n)
        .map(|i| raw(1, 7, start + i as i64 * 29 * MIN, EventKind::Pageview, "/"))
        .collect();
    let a = new_assigner(11);
    let assignments = run_pipeline(&a, &events, &tz, Some(50));
    let reference = assert_matches_reference(&events, &tz, &assignments);
    assert_eq!(reference.len(), 2, "7 天上限把 9.5 天切成两个会话");
    assert!(
        a.stats().alias_cap_hit > 0,
        "春季 DST 日让 7 天跨过 9 个本地日期，第 9 个 key 应当触发上限计数"
    );
}

/// 没有 DST 的时区里同样的场景恰好 8 个 key，不触发上限。
#[test]
fn long_running_person_without_dst_stays_within_eight_keys() {
    let tz = TimeZone::get("Asia/Shanghai").unwrap();
    let start = "2026-05-01T23:30:00"
        .parse::<DateTime>()
        .unwrap()
        .to_zoned(tz.clone())
        .unwrap()
        .timestamp()
        .as_millisecond();
    let n = 9 * 24 * 60 / 29 + 16;
    let events: Vec<RawEvent> = (0..n)
        .map(|i| raw(1, 7, start + i as i64 * 29 * MIN, EventKind::Pageview, "/"))
        .collect();
    let a = new_assigner(11);
    let assignments = run_pipeline(&a, &events, &tz, Some(50));
    let reference = assert_matches_reference(&events, &tz, &assignments);
    assert_eq!(reference.len(), 2);
    assert_eq!(a.stats().alias_cap_hit, 0);
}
