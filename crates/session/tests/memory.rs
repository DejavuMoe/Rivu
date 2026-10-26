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
}
