//! Q9 已知偏差的固定用例（`docs/architecture/system-design.md` §10.1 Q9）。
//!
//! 现行 §4.6「盖章」把跨本地午夜续上的事件的 `visitor_id` 改写为会话的原始 ID（前一天的 ID），
//! 于是访客数在单日和跨天都会失真。参考实现按「各本地日不同的人数之和」定义真值。
//! 这里把偏差写成断言：**Q9 定案（不再改写 `visitor_id`）后，翻转标注了「定案后翻转」的断言**。

mod common;

use std::collections::HashSet;

use jiff::civil::{Date, DateTime, date};
use jiff::tz::TimeZone;
use rivu_domain::EventKind;
use rivu_reference::{RawEvent, metrics, sessionize};
use rivu_session::assigner::Assignment;
use rivu_session::identity::local_date;

use common::*;

fn at(tz: &TimeZone, local: &str) -> i64 {
    local
        .parse::<DateTime>()
        .unwrap()
        .to_zoned(tz.clone())
        .unwrap()
        .timestamp()
        .as_millisecond()
}

/// 用 Stamp 的某个 id，按 (id, 事件本地日) 去重，统计非 engagement 事件在 `[from, to]` 内的访客数。
fn visitors_by_stamp(
    events: &[RawEvent],
    tz: &TimeZone,
    assignments: &[Assignment],
    from: Date,
    to: Date,
    own: bool,
) -> usize {
    let mut set = HashSet::new();
    for (e, a) in events.iter().zip(assignments) {
        let Assignment::Stamped(s) = a else { continue };
        if e.kind == EventKind::Engagement {
            continue;
        }
        let d = local_date(e.ts_ms, tz);
        if d >= from && d <= to {
            set.insert((d, if own { s.own_visitor_id } else { s.visitor_id }));
        }
    }
    set.len()
}

/// 情形 (a)：某人 d 23:50 来，d+1 00:10 续上同一会话，d+1 05:00 又开了新会话。
#[test]
fn q9_a_same_day_double_count() {
    let tz = TimeZone::get("Asia/Shanghai").unwrap();
    let (d, d1) = (date(2026, 5, 1), date(2026, 5, 2));
    let events = vec![
        raw(
            1,
            7,
            at(&tz, "2026-05-01T23:50:00"),
            EventKind::Pageview,
            "/a",
        ),
        raw(
            1,
            7,
            at(&tz, "2026-05-02T00:10:00"),
            EventKind::Pageview,
            "/b",
        ), // 跨午夜，续上
        raw(
            1,
            7,
            at(&tz, "2026-05-02T05:00:00"),
            EventKind::Pageview,
            "/c",
        ), // 间隔 > 30 min，新会话
    ];
    let a = new_assigner(1);
    let asg = run_pipeline(&a, &events, &tz, None);
    assert_eq!(
        groups(&asg),
        vec![vec![0, 1], vec![2]],
        "两个会话，第一个跨午夜"
    );

    // 参考实现的真值：d+1 当天只有 1 个人。
    let reference = sessionize(&events);
    assert_eq!(reference.len(), 2);
    let truth = metrics(&events, &reference, 1, &tz, d1, d1).visitors;
    assert_eq!(truth, 1);

    // 现行 §4.6：事件 1 被盖成会话的原始 ID（d 的 ID），事件 2 是 d+1 的 ID，
    // 按 (id, 本地日) 去重，d+1 当天被算成 2 个访客。
    // Q9 定案后（不再改写 visitor_id）：把下一行翻转为 assert_eq!(stamped, 1)。
    let stamped = visitors_by_stamp(&events, &tz, &asg, d1, d1, false);
    assert_eq!(stamped, 2, "§10.1 Q9(a)：单日多算");
    assert_ne!(stamped as u64, truth);

    // 改用事件自己的 visitor_id（Q9 的建议做法）就等于真值。
    let own = visitors_by_stamp(&events, &tz, &asg, d1, d1, true);
    assert_eq!(own as u64, truth);

    // 另外几个事实：事件 1 的两个 ID 不同，事件 2 的 visitor_id 等于第一天的 ID。
    let (Assignment::Stamped(s0), Assignment::Stamped(s1), Assignment::Stamped(s2)) =
        (&asg[0], &asg[1], &asg[2])
    else {
        panic!("三个事件都应有会话");
    };
    assert_eq!(
        s1.visitor_id, s0.own_visitor_id,
        "跨午夜的事件被盖成前一天的 ID"
    );
    assert_ne!(s1.visitor_id, s1.own_visitor_id);
    assert_eq!(s2.visitor_id, s2.own_visitor_id);
    assert_ne!(s2.visitor_id, s1.visitor_id);
    assert_eq!(a.stats().sessions_started, 2);

    // d 当天：只有 1 个访客，两种口径一致。
    assert_eq!(visitors_by_stamp(&events, &tz, &asg, d, d, false), 1);
    assert_eq!(visitors_by_stamp(&events, &tz, &asg, d, d, true), 1);
}

/// 情形 (b)：d+1 没有新会话。两天范围里按 id 直接 `uniq`（不分日）只得到 1，真值是各日之和 2。
#[test]
fn q9_b_cross_day_uniq_disagrees_with_per_day_sum() {
    let tz = TimeZone::get("Europe/Berlin").unwrap();
    let (d, d1) = (date(2026, 5, 1), date(2026, 5, 2));
    let events = vec![
        raw(
            1,
            7,
            at(&tz, "2026-05-01T23:50:00"),
            EventKind::Pageview,
            "/a",
        ),
        raw(
            1,
            7,
            at(&tz, "2026-05-02T00:10:00"),
            EventKind::Pageview,
            "/b",
        ),
    ];
    let a = new_assigner(1);
    let asg = run_pipeline(&a, &events, &tz, None);
    assert_eq!(groups(&asg), vec![vec![0, 1]]);
    let reference = sessionize(&events);
    let truth = metrics(&events, &reference, 1, &tz, d, d1).visitors;
    assert_eq!(truth, 2, "真值：d 一个、d+1 一个，各日之和");

    // 范围内对盖章后的 visitor_id 直接去重（§6.5 的 `uniq(visitor_id)`）：只有 1 个不同的值。
    // Q9 定案后：翻转为 assert_eq!(uniq_stamped, 2)。
    let uniq_stamped = asg
        .iter()
        .filter_map(|a| match a {
            Assignment::Stamped(s) => Some(s.visitor_id),
            Assignment::NoSession => None,
        })
        .collect::<HashSet<_>>()
        .len();
    assert_eq!(
        uniq_stamped, 1,
        "§10.1 Q9(b)：跨天 uniq 与日汇总层相加不一致"
    );
    assert_ne!(uniq_stamped as u64, truth);

    // 用 own_visitor_id 按 (id, 本地日) 去重，等于真值；直接 uniq 则是 2 个不同的 ID，也等于真值。
    assert_eq!(
        visitors_by_stamp(&events, &tz, &asg, d, d1, true) as u64,
        truth
    );
    let uniq_own = asg
        .iter()
        .filter_map(|a| match a {
            Assignment::Stamped(s) => Some(s.own_visitor_id),
            Assignment::NoSession => None,
        })
        .collect::<HashSet<_>>()
        .len();
    assert_eq!(uniq_own as u64, truth);
}
