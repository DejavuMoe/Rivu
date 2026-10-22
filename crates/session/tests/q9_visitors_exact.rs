//! Q9 定案后访客数精确的回归测试（`docs/architecture/system-design.md` §10 Q9）。
//!
//! 盖章不再改写 `visitor_id`：事件保留自己本地日的 ID，会话只靠 `session_id` 串联。
//! 因此范围内对 `Stamp::visitor_id` 直接 `uniq`（不再需要按本地日配对）就等于参考实现的真值，
//! 即「各本地日不同人数之和」，与日汇总层相加严格一致。

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

/// 范围 `[from, to]` 内非 engagement 事件的 `Stamp::visitor_id` 的不同值个数，即查询里的 `uniq(visitor_id)`。
fn uniq_stamped(
    events: &[RawEvent],
    tz: &TimeZone,
    assignments: &[Assignment],
    from: Date,
    to: Date,
) -> u64 {
    let mut set = HashSet::new();
    for (e, a) in events.iter().zip(assignments) {
        let Assignment::Stamped(s) = a else { continue };
        if e.kind == EventKind::Engagement {
            continue;
        }
        let d = local_date(e.ts_ms, tz);
        if d >= from && d <= to {
            set.insert(s.visitor_id);
        }
    }
    set.len() as u64
}

/// 情形 (a)：某人 d 23:50 来，d+1 00:10 续上同一会话，d+1 05:00 又开了新会话。
/// d+1 当天只有 1 个人，两个会话的事件都带 d+1 的 ID。
#[test]
fn q9_a_same_day_is_exact() {
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
    assert_eq!(a.stats().sessions_started, 2);

    let reference = sessionize(&events);
    assert_eq!(reference.len(), 2);
    let truth = metrics(&events, &reference, 1, &tz, d1, d1).visitors;
    assert_eq!(truth, 1);
    assert_eq!(uniq_stamped(&events, &tz, &asg, d1, d1), truth);

    // d 当天同样精确。
    let truth_d = metrics(&events, &reference, 1, &tz, d, d).visitors;
    assert_eq!(truth_d, 1);
    assert_eq!(uniq_stamped(&events, &tz, &asg, d, d), truth_d);

    // 事件保留自己当天的 ID：跨午夜续上的事件 1 与同日新会话的事件 2 是同一个 ID，与事件 0 不同。
    let (Assignment::Stamped(s0), Assignment::Stamped(s1), Assignment::Stamped(s2)) =
        (&asg[0], &asg[1], &asg[2])
    else {
        panic!("三个事件都应有会话");
    };
    assert_ne!(s1.visitor_id, s0.visitor_id, "不再盖成前一天的 ID");
    assert_eq!(
        s1.visitor_id, s2.visitor_id,
        "同一本地日、同一个人，ID 相同"
    );
    assert_eq!(s0.session_id, s1.session_id);
    assert_ne!(s1.session_id, s2.session_id);
}

/// 情形 (b)：d+1 没有新会话。两天范围里直接 `uniq` 得到 2，等于各日之和，也等于日汇总层相加。
#[test]
fn q9_b_cross_day_uniq_equals_per_day_sum() {
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
    assert_eq!(uniq_stamped(&events, &tz, &asg, d, d1), truth);

    let per_day: u64 = [d, d1]
        .iter()
        .map(|&day| uniq_stamped(&events, &tz, &asg, day, day))
        .sum();
    assert_eq!(per_day, truth, "与日汇总层的每日值相加严格相等");
}
