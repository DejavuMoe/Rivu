//! 差分测试共用的管线：身份 → SessionAssigner。
#![allow(dead_code)]

use std::collections::BTreeMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use jiff::civil::Date;
use jiff::tz::TimeZone;
use rivu_domain::{EventKind, SessionAttrs};
use rivu_reference::RawEvent;
use rivu_session::assigner::{Assignment, Incoming, SessionAssigner, SessionConfig};
use rivu_session::identity::{self, Salt};

pub const ROUTING_SECRET: Salt = [0x5A; 16];

fn splitmix64(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// 按本地日期确定性生成的盐：同一个日期总是同一份盐，不同日期不同。
pub fn salt_for(date: Date) -> Salt {
    let mut state = (i64::from(date.year()) * 372
        + i64::from(date.month()) * 31
        + i64::from(date.day())) as u64;
    let mut out = [0u8; 16];
    out[..8].copy_from_slice(&splitmix64(&mut state).to_le_bytes());
    out[8..].copy_from_slice(&splitmix64(&mut state).to_le_bytes());
    out
}

/// 一个人固定的 (ip, ua)。偶数号用 IPv4，奇数号用 IPv6，再每隔几个用 IPv4-mapped 写法，
/// 以便顺带覆盖规范化（不同的人地址永不相同）。
pub fn person_net(person: u64) -> (IpAddr, String) {
    let ip = match person % 3 {
        0 => IpAddr::V4(Ipv4Addr::new(10, (person >> 8) as u8, person as u8, 1)),
        1 => IpAddr::V6(Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, person as u16)),
        _ => IpAddr::V6(Ipv4Addr::new(10, (person >> 8) as u8, person as u8, 1).to_ipv6_mapped()),
    };
    (ip, format!("Mozilla/5.0 person-{person}"))
}

/// 每个事件的会话属性：`browser` 记下事件下标，方便核对「属性来自会话的第一个事件」。
pub fn attrs_for(index: usize) -> SessionAttrs {
    SessionAttrs {
        browser: format!("e{index}"),
        ..SessionAttrs::default()
    }
}

/// 一个事件的身份三元组：(visitor_id, prev_visitor_id, routing)。
pub fn identity_of(e: &RawEvent, tz: &TimeZone) -> (u64, u64, u64) {
    let (ip, ua) = person_net(e.person);
    let d = identity::local_date(e.ts_ms, tz);
    let visitor = identity::visitor_hash(&salt_for(d), e.site_id, ip, &ua);
    let prev_date = d.yesterday().expect("日期有前一天");
    let prev = identity::visitor_hash(&salt_for(prev_date), e.site_id, ip, &ua);
    let routing = identity::routing_hash(&ROUTING_SECRET, e.site_id, ip, &ua);
    (visitor, prev, routing)
}

pub fn new_assigner(seed: u64) -> SessionAssigner {
    SessionAssigner::with_seed(SessionConfig::default(), seed)
}

/// 按输入顺序喂给 assigner。`sweep_every` 为 `Some(n)` 时每 n 个事件清扫一次。
pub fn run_pipeline(
    assigner: &SessionAssigner,
    events: &[RawEvent],
    tz: &TimeZone,
    sweep_every: Option<usize>,
) -> Vec<Assignment> {
    let mut out = Vec::with_capacity(events.len());
    for (i, e) in events.iter().enumerate() {
        let (visitor_id, prev, routing) = identity_of(e, tz);
        let attrs = attrs_for(i);
        out.push(assigner.assign(&Incoming {
            site_id: e.site_id,
            visitor_id,
            prev_visitor_id: Some(prev),
            routing,
            ts_ms: e.ts_ms,
            kind: e.kind,
            pathname: &e.pathname,
            attrs: &attrs,
        }));
        if let Some(n) = sweep_every
            && (i + 1) % n == 0
        {
            assigner.sweep();
        }
    }
    out
}

pub fn raw(site_id: u64, person: u64, ts_ms: i64, kind: EventKind, path: &str) -> RawEvent {
    RawEvent {
        site_id,
        person,
        ts_ms,
        kind,
        hostname: "example.com".into(),
        pathname: path.into(),
        interactive: false,
        engagement_ms: 0,
        attrs: Default::default(),
    }
}

/// 把已分配的事件按 session_id 分组，返回「事件下标集合」的有序列表（每组内升序）。
pub fn groups(assignments: &[Assignment]) -> Vec<Vec<usize>> {
    let mut by: BTreeMap<u64, Vec<usize>> = BTreeMap::new();
    for (i, a) in assignments.iter().enumerate() {
        if let Assignment::Stamped(s) = a {
            by.entry(s.session_id).or_default().push(i);
        }
    }
    let mut g: Vec<Vec<usize>> = by.into_values().collect();
    g.sort();
    g
}
