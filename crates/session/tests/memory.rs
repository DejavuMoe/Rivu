//! 实测会话内存（`docs/brief-p1.md`「dhat / heaptrack：实测会话内存」，`system-design.md` §4.6「内存」）。
//!
//! 用 dhat 的计数分配器量 SessionAssigner 在堆上实际占用的字节：10 万个会话（属性取自真实形状的字符串），
//! 再给其中一半加一个别名 key（跨午夜）。忽略的测试，手动运行：
//!   cargo test --release -p rivu-session --test memory -- --ignored --nocapture

use rivu_domain::{EventKind, SessionAttrs};
use rivu_session::assigner::{Incoming, SessionAssigner, SessionConfig};

#[global_allocator]
static ALLOC: dhat::Alloc = dhat::Alloc;

fn attrs(i: u64) -> SessionAttrs {
    let browsers = [
        ("Chrome", "141.0"),
        ("Safari", "18.6"),
        ("Firefox", "143.0"),
        ("Edge", "141.0"),
    ];
    let (b, bv) = browsers[(i % 4) as usize];
    SessionAttrs {
        referrer: if i.is_multiple_of(3) {
            format!("news.example.org/post/{}", i % 500)
        } else {
            String::new()
        },
        referrer_source: ["", "Google", "Hacker News"][(i % 3) as usize].into(),
        channel: ["Direct", "Organic Search", "Referral"][(i % 3) as usize].into(),
        utm_source: if i.is_multiple_of(10) {
            "newsletter".into()
        } else {
            String::new()
        },
        utm_medium: if i.is_multiple_of(10) {
            "email".into()
        } else {
            String::new()
        },
        utm_campaign: if i.is_multiple_of(10) {
            "autumn-launch".into()
        } else {
            String::new()
        },
        utm_content: String::new(),
        utm_term: String::new(),
        click_id_param: String::new(),
        country_code: *b"DE",
        region_code: "DE-BE".into(),
        city_geoname_id: 2_950_159,
        device: ["desktop", "mobile"][(i % 2) as usize].into(),
        browser: b.into(),
        browser_version: bv.into(),
        os: ["Windows", "macOS", "Android", "iOS"][(i % 4) as usize].into(),
        os_version: ["10", "15.6", "14", "18.6"][(i % 4) as usize].into(),
    }
}

#[test]
#[ignore = "实测会话内存：cargo test --release -p rivu-session --test memory -- --ignored --nocapture"]
fn bytes_per_session_and_alias() {
    let _profiler = dhat::Profiler::builder().testing().build();
    const N: u64 = 100_000;
    let all: Vec<SessionAttrs> = (0..N).map(attrs).collect();
    let paths: Vec<String> = (0..N).map(|i| format!("/blog/post-{}", i % 2000)).collect();

    let a = SessionAssigner::with_seed(SessionConfig::default(), 1);
    let base = dhat::HeapStats::get().curr_bytes;
    for i in 0..N {
        a.assign(&Incoming {
            site_id: 1 + i % 6,
            visitor_id: i.wrapping_mul(0x9E37_79B9_7F4A_7C15),
            prev_visitor_id: None,
            routing: i,
            ts_ms: i as i64,
            kind: EventKind::Pageview,
            pathname: &paths[i as usize],
            attrs: &all[i as usize],
        });
    }
    let sessions = dhat::HeapStats::get().curr_bytes - base;
    // 一半的会话跨午夜：新的 visitor_id 带 prev，加一个别名 key
    for i in (0..N).step_by(2) {
        a.assign(&Incoming {
            site_id: 1 + i % 6,
            visitor_id: i.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ 0xABCD,
            prev_visitor_id: Some(i.wrapping_mul(0x9E37_79B9_7F4A_7C15)),
            routing: i,
            ts_ms: N as i64 + i as i64,
            kind: EventKind::Pageview,
            pathname: &paths[i as usize],
            attrs: &all[i as usize],
        });
    }
    let with_alias = dhat::HeapStats::get().curr_bytes - base;
    let per_session = sessions as f64 / N as f64;
    let per_alias = (with_alias - sessions) as f64 / (N / 2) as f64;
    println!(
        "{N} 个会话：{:.1} MiB，每个会话 {per_session:.0} B；加 {} 个别名 key 后每个别名 {per_alias:.0} B",
        sessions as f64 / 1_048_576.0,
        N / 2
    );
    assert_eq!(a.len(), N as usize);
    // 设计估计每个会话 150–300 B、每个别名约 48 B（§4.6）。这里只防数量级的回归。
    assert!(per_session < 1500.0, "{per_session}");
    assert!(per_alias < 300.0, "{per_alias}");

    const LONG: u64 = 2000;
    let large = long_attrs();
    let path = "x".repeat(2048);
    let a = SessionAssigner::with_seed(SessionConfig::default(), 1);
    let base = dhat::HeapStats::get().curr_bytes;
    for id in 1..=LONG {
        a.assign(&Incoming {
            site_id: 1,
            visitor_id: id,
            prev_visitor_id: None,
            routing: 0,
            ts_ms: id as i64,
            kind: EventKind::Pageview,
            pathname: &path,
            attrs: &large,
        });
    }
    assert_eq!(a.len(), LONG as usize);
    let long_bytes = (dhat::HeapStats::get().curr_bytes - base) as f64 / LONG as f64;
    println!(
        "Long synthetic sessions: {long_bytes:.0} B/session, {:.2} GiB at 2 million without the byte limit",
        long_bytes * 2_000_000.0 / 2_f64.powi(30)
    );
}

fn long_attrs() -> SessionAttrs {
    SessionAttrs {
        referrer: "x".repeat(2000),
        utm_source: "界".repeat(255),
        utm_medium: "界".repeat(255),
        utm_campaign: "界".repeat(255),
        utm_content: "界".repeat(255),
        utm_term: "界".repeat(255),
        ..SessionAttrs::default()
    }
}

#[test]
fn long_sessions_respect_payload_budget_during_ingestion_and_restore() {
    let a = SessionAssigner::with_seed(SessionConfig::default(), 1);
    let attrs = long_attrs();
    let path = "x".repeat(2048);
    for id in 1..=2500 {
        a.assign(&Incoming {
            site_id: 1,
            visitor_id: id,
            prev_visitor_id: None,
            routing: 0,
            ts_ms: id as i64,
            kind: EventKind::Pageview,
            pathname: &path,
            attrs: &attrs,
        });
    }
    let expected = 16 * 1024 * 1024 / (2000 + 5 * 255 * 3 + 2048);
    assert_eq!(a.len(), expected);
    assert!(a.peek(1, 0, 1).is_none());
    assert!(a.peek(1, 0, 2500).is_some());
    let b = SessionAssigner::new(SessionConfig::default());
    let mut records = a.export();
    let mut extra = records.last().unwrap().clone();
    extra.session_id = 0x1f;
    extra.keys = vec![2501];
    extra.last_seen_ms += 1;
    records.push(extra);
    assert_eq!(b.restore(records), expected);
    assert!(b.peek(1, 0, 2501).is_some());
}
