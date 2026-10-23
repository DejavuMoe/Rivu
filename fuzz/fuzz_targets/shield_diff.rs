//! 屏蔽规则（IP / CIDR 前缀树与页面通配）对照朴素实现的差分测试。
//! 输入是结构化的：一组规则 + 若干查询。朴素实现直接用整数掩码比较与 DP 通配，与 `ShieldRules` 的
//! 前缀树 / 分段贪心匹配独立。语义：命中任一 deny 且不命中任一 allow 才算屏蔽。
#![no_main]

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use rivu_ingest::shield::{RuleType, ShieldRules, normalize_value};
use rivu_storage_pg::shield::ShieldRuleRow;

#[derive(Arbitrary, Debug)]
struct IpRule {
    allow: bool,
    v6: bool,
    mapped: bool,
    bits: u128,
    /// 为 None 表示单个地址规则（ip 类型），否则是 CIDR 前缀长度（取模到合法范围）
    prefix: Option<u8>,
}

#[derive(Arbitrary, Debug)]
struct PageRule {
    allow: bool,
    pattern: String,
}

#[derive(Arbitrary, Debug)]
struct Input {
    ip_rules: Vec<IpRule>,
    page_rules: Vec<PageRule>,
    ips: Vec<(bool, bool, u128)>,
    paths: Vec<String>,
    /// 随机文本也当作 ip / cidr / page 取值喂给 `normalize_value`，只检查不 panic 与幂等
    junk: Vec<String>,
}

fn mk_ip(v6: bool, mapped: bool, bits: u128) -> IpAddr {
    if v6 {
        let b = if mapped {
            (0xffffu128 << 32) | (bits & 0xffff_ffff)
        } else {
            bits
        };
        IpAddr::V6(Ipv6Addr::from(b))
    } else {
        IpAddr::V4(Ipv4Addr::from(bits as u32))
    }
}

/// 规范化到 (位宽, 值)：IPv4-mapped 视为 IPv4。
fn canon(ip: IpAddr) -> (u8, u128) {
    match ip {
        IpAddr::V4(a) => (32, u128::from(u32::from(a))),
        IpAddr::V6(a) => match a.to_ipv4_mapped() {
            Some(v4) => (32, u128::from(u32::from(v4))),
            None => (128, u128::from(a)),
        },
    }
}

/// 规则 (地址, 前缀) -> 规范化的 (位宽, 值, 前缀)。IPv4-mapped 的 CIDR 只有前缀 >= 96 时才降成 IPv4。
fn canon_rule(ip: IpAddr, prefix: u32) -> (u8, u128, u32) {
    match ip {
        IpAddr::V4(a) => (32, u128::from(u32::from(a)), prefix),
        IpAddr::V6(a) => match a.to_ipv4_mapped() {
            Some(v4) if prefix >= 96 => (32, u128::from(u32::from(v4)), prefix - 96),
            _ => (128, u128::from(a), prefix),
        },
    }
}

fn prefix_matches(width: u8, rule: u128, prefix: u32, addr: u128) -> bool {
    let w = u32::from(width);
    let shift = w - prefix;
    if shift >= 128 {
        return true;
    }
    (rule >> shift) == (addr >> shift)
}

/// 朴素通配：`*` 匹配任意长度（含空、含 `/`），其余字节字面量。
fn glob(pat: &[u8], text: &[u8]) -> bool {
    let mut dp = vec![false; text.len() + 1];
    dp[0] = true;
    for &p in pat {
        if p == b'*' {
            for i in 1..=text.len() {
                dp[i] = dp[i] || dp[i - 1];
            }
        } else {
            for i in (1..=text.len()).rev() {
                dp[i] = dp[i - 1] && text[i - 1] == p;
            }
            dp[0] = false;
        }
    }
    dp[text.len()]
}

fuzz_target!(|input: Input| {
    for j in input.junk.iter().take(16) {
        for kind in [RuleType::Ip, RuleType::Cidr, RuleType::Page, RuleType::Country] {
            if let Ok(v) = normalize_value(kind, j) {
                assert_eq!(normalize_value(kind, &v).ok().as_deref(), Some(v.as_str()), "{kind} {j:?}");
            }
        }
    }

    let mut rows = Vec::new();
    // (allow, width, value, prefix)
    let mut ip_expect: Vec<(bool, u8, u128, u32)> = Vec::new();
    for (i, r) in input.ip_rules.iter().take(64).enumerate() {
        let ip = mk_ip(r.v6, r.mapped, r.bits);
        let max = if ip.is_ipv4() { 32u32 } else { 128 };
        let (kind, value, prefix) = match r.prefix {
            None => ("ip", ip.to_string(), max),
            Some(p) => {
                let p = u32::from(p) % (max + 1);
                ("cidr", format!("{ip}/{p}"), p)
            }
        };
        let (w, v, p) = canon_rule(ip, prefix);
        ip_expect.push((r.allow, w, v, p));
        rows.push(ShieldRuleRow {
            id: format!("ip{i}"),
            site_id: 1,
            kind: kind.into(),
            action: if r.allow { "allow" } else { "deny" }.into(),
            value,
            description: None,
        });
    }
    let mut page_expect: Vec<(bool, Vec<u8>)> = Vec::new();
    for (i, r) in input.page_rules.iter().take(32).enumerate() {
        // 只有校验通过的模式才进入规则集；期望值用规范化后的模式
        if let Ok(p) = normalize_value(RuleType::Page, &r.pattern) {
            page_expect.push((r.allow, p.into_bytes()));
            rows.push(ShieldRuleRow {
                id: format!("pg{i}"),
                site_id: 1,
                kind: "page".into(),
                action: if r.allow { "allow" } else { "deny" }.into(),
                value: r.pattern.clone(),
                description: None,
            });
        }
    }
    let (rules, errors) = ShieldRules::compile(&rows);
    assert!(errors.is_empty(), "校验通过的规则不应编译失败：{errors:?}");

    for &(v6, mapped, bits) in input.ips.iter().take(64) {
        let ip = mk_ip(v6, mapped, bits);
        let (w, a) = canon(ip);
        let hit = |allow: bool| {
            ip_expect
                .iter()
                .any(|&(al, rw, rv, rp)| al == allow && rw == w && prefix_matches(w, rv, rp, a))
        };
        let expected = hit(false) && !hit(true);
        assert_eq!(rules.ip_blocked(ip), expected, "ip {ip}");
    }
    // 规则里的网段端点也查一遍：边界最容易错
    for &(_, w, v, _) in ip_expect.iter().take(16) {
        let ip = if w == 32 {
            IpAddr::V4(Ipv4Addr::from(v as u32))
        } else {
            IpAddr::V6(Ipv6Addr::from(v))
        };
        let (cw, a) = canon(ip);
        let hit = |allow: bool| {
            ip_expect
                .iter()
                .any(|&(al, rw, rv, rp)| al == allow && rw == cw && prefix_matches(cw, rv, rp, a))
        };
        assert_eq!(rules.ip_blocked(ip), hit(false) && !hit(true), "ip {ip}");
    }

    let mut paths: Vec<String> = input.paths.iter().take(32).cloned().collect();
    // 把模式里的 `*` 换成字面量也当作路径：命中边界
    for (_, p) in page_expect.iter().take(8) {
        paths.push(String::from_utf8_lossy(p).replace('*', "ab"));
        paths.push(String::from_utf8_lossy(p).replace('*', ""));
    }
    for path in &paths {
        if path.len() > 2048 {
            continue;
        }
        let hit = |allow: bool| {
            page_expect
                .iter()
                .any(|(al, p)| *al == allow && glob(p, path.as_bytes()))
        };
        let expected = hit(false) && !hit(true);
        assert_eq!(rules.page_blocked(path), expected, "path {path:?}");
    }
});

