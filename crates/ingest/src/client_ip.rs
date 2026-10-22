//! 客户端 IP 判定（`system-design.md` §4.2 第 2 步；`tracker-and-protocol.md` §6.1「客户端 IP」）。

use std::net::{IpAddr, SocketAddr};

use rivu_common::config::Cidr;

/// IPv4-mapped IPv6（`::ffff:a.b.c.d`）转 IPv4，其余原样。
///
/// 不用 `Ipv6Addr::to_ipv4`：它会把 `::1` 这类 IPv4-compatible 地址也转成 `0.0.0.1`。
fn canonical(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(ip, IpAddr::V4),
        v4 => v4,
    }
}

/// 解析 `X-Forwarded-For` 的一个元素：`1.2.3.4`、`1.2.3.4:5678`、`[::1]`、`[::1]:80`、`::1`。
fn parse_hop(token: &str) -> Option<IpAddr> {
    let t = token.trim();
    if let Ok(ip) = t.parse::<IpAddr>() {
        return Some(ip);
    }
    if let Ok(sa) = t.parse::<SocketAddr>() {
        return Some(sa.ip());
    }
    t.strip_prefix('[')?.strip_suffix(']')?.parse().ok()
}

fn is_trusted(ip: IpAddr, trusted: &[Cidr]) -> bool {
    trusted.iter().any(|c| c.contains(ip))
}

/// 判定客户端 IP。
///
/// `peer` 不在 `trusted` 内：返回 `peer`，转发头一概不看。否则把所有 `X-Forwarded-For` 头值按
/// 出现顺序拼接（逗号分隔），从右往左跳过属于 `trusted` 的地址，取第一个不可信地址。
/// 任一被检查的元素解析失败，或全部元素都可信，回退 `peer`；永不 panic。
/// 结果里 IPv4-mapped IPv6 已转成 IPv4。
pub fn client_ip(peer: IpAddr, xff_values: &[&str], trusted: &[Cidr]) -> IpAddr {
    let peer = canonical(peer);
    if !is_trusted(peer, trusted) {
        return peer;
    }
    let hops = xff_values
        .iter()
        .filter(|v| !v.trim().is_empty())
        .flat_map(|v| v.split(','));
    // 先收集再反向：`split` 不是双端迭代器。头部上限 16 KB，开销可忽略。
    let hops: Vec<&str> = hops.collect();
    for token in hops.into_iter().rev() {
        let Some(ip) = parse_hop(token) else {
            return peer;
        };
        let ip = canonical(ip);
        if !is_trusted(ip, trusted) {
            return ip;
        }
    }
    peer
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    fn cidrs(list: &[&str]) -> Vec<Cidr> {
        list.iter().map(|s| s.parse().unwrap()).collect()
    }

    #[test]
    fn untrusted_peer_ignores_forwarded_header() {
        let trusted = cidrs(&["10.0.0.0/8"]);
        assert_eq!(
            client_ip(ip("203.0.113.9"), &["1.2.3.4"], &trusted),
            ip("203.0.113.9")
        );
    }

    #[test]
    fn empty_trusted_list_always_returns_peer() {
        assert_eq!(client_ip(ip("10.0.0.1"), &["1.2.3.4"], &[]), ip("10.0.0.1"));
    }

    #[test]
    fn trusted_peer_uses_rightmost_untrusted_hop() {
        let trusted = cidrs(&["10.0.0.0/8"]);
        assert_eq!(
            client_ip(ip("10.0.0.1"), &["198.51.100.7"], &trusted),
            ip("198.51.100.7")
        );
        // 客户端伪造的左侧元素不会被采用。
        assert_eq!(
            client_ip(
                ip("10.0.0.1"),
                &["6.6.6.6, 198.51.100.7, 10.0.0.2"],
                &trusted
            ),
            ip("198.51.100.7")
        );
    }

    #[test]
    fn multiple_header_values_are_concatenated_in_order() {
        let trusted = cidrs(&["10.0.0.0/8"]);
        assert_eq!(
            client_ip(
                ip("10.0.0.1"),
                &["6.6.6.6, 198.51.100.7", "10.0.0.2"],
                &trusted
            ),
            ip("198.51.100.7")
        );
        // 最右边的头值里的不可信地址优先。
        assert_eq!(
            client_ip(ip("10.0.0.1"), &["10.0.0.3", "203.0.113.5"], &trusted),
            ip("203.0.113.5")
        );
    }

    #[test]
    fn all_hops_trusted_falls_back_to_peer() {
        let trusted = cidrs(&["10.0.0.0/8"]);
        assert_eq!(
            client_ip(ip("10.0.0.1"), &["10.1.1.1, 10.2.2.2"], &trusted),
            ip("10.0.0.1")
        );
    }

    #[test]
    fn missing_or_empty_header_falls_back_to_peer() {
        let trusted = cidrs(&["10.0.0.0/8"]);
        assert_eq!(client_ip(ip("10.0.0.1"), &[], &trusted), ip("10.0.0.1"));
        assert_eq!(
            client_ip(ip("10.0.0.1"), &["", "  "], &trusted),
            ip("10.0.0.1")
        );
    }

    #[test]
    fn unparsable_hop_falls_back_to_peer() {
        let trusted = cidrs(&["10.0.0.0/8"]);
        for bad in [
            "unknown",
            "1.2.3",
            "1.2.3.4, garbage",
            "1.2.3.4,",
            "fe80::1%eth0",
        ] {
            assert_eq!(
                client_ip(ip("10.0.0.1"), &[bad], &trusted),
                ip("10.0.0.1"),
                "{bad}"
            );
        }
    }

    #[test]
    fn garbage_left_of_first_untrusted_hop_is_never_parsed() {
        let trusted = cidrs(&["10.0.0.0/8"]);
        assert_eq!(
            client_ip(ip("10.0.0.1"), &["garbage, 198.51.100.7"], &trusted),
            ip("198.51.100.7")
        );
    }

    #[test]
    fn hop_formats_with_port_and_brackets() {
        let trusted = cidrs(&["10.0.0.0/8"]);
        let peer = ip("10.0.0.1");
        assert_eq!(
            client_ip(peer, &["198.51.100.7:4711"], &trusted),
            ip("198.51.100.7")
        );
        assert_eq!(
            client_ip(peer, &["[2001:db8::7]"], &trusted),
            ip("2001:db8::7")
        );
        assert_eq!(
            client_ip(peer, &["[2001:db8::7]:99"], &trusted),
            ip("2001:db8::7")
        );
        assert_eq!(
            client_ip(peer, &["2001:db8::7"], &trusted),
            ip("2001:db8::7")
        );
    }

    #[test]
    fn ipv4_mapped_results_are_canonicalized() {
        let trusted = cidrs(&["10.0.0.0/8"]);
        assert_eq!(
            client_ip(ip("::ffff:203.0.113.9"), &[], &trusted),
            ip("203.0.113.9")
        );
        assert_eq!(
            client_ip(ip("10.0.0.1"), &["::ffff:198.51.100.7"], &trusted),
            ip("198.51.100.7")
        );
        // 映射形式的可信代理同样按 IPv4 比对。
        assert_eq!(
            client_ip(ip("::ffff:10.0.0.1"), &["198.51.100.7"], &trusted),
            ip("198.51.100.7")
        );
        assert_eq!(
            client_ip(ip("10.0.0.1"), &["::ffff:10.0.0.2, 198.51.100.7"], &trusted),
            ip("198.51.100.7")
        );
        // IPv4-compatible（非映射）地址不被改写。
        assert_eq!(client_ip(ip("::1"), &[], &[]), ip("::1"));
    }

    #[test]
    fn ipv6_trusted_range() {
        let trusted = cidrs(&["fd00::/8"]);
        assert_eq!(
            client_ip(ip("fd00::1"), &["2001:db8::1, fd00::2"], &trusted),
            ip("2001:db8::1")
        );
    }

    proptest! {
        #[test]
        fn never_panics(
            peer in any::<std::net::IpAddr>(),
            xff in proptest::collection::vec(any::<String>(), 0..4),
        ) {
            let trusted = cidrs(&["10.0.0.0/8", "::/0"]);
            let refs: Vec<&str> = xff.iter().map(String::as_str).collect();
            let _ = client_ip(peer, &refs, &trusted);
            let _ = client_ip(peer, &refs, &[]);
        }
    }
}
