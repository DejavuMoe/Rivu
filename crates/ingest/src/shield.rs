//! 屏蔽规则（`shield_rules` 表，`system-design.md` §4.2 第 4、6 步，`platform-and-ops.md` §3.2–§3.3）。
//!
//! 一个站点的规则在站点快照里编译成 [`ShieldRules`]：
//!
//! | 类型 | 取值 | 编译成 | 命中判定 |
//! |---|---|---|---|
//! | `ip` | 单个地址，规范化文本 | 哈希集合 | 地址相等 |
//! | `cidr` | `网络地址/前缀`，主机位已清零 | 二叉前缀树（IPv4 与 IPv6 各一棵） | 最长前缀之内的任一节点是终点 |
//! | `country` | 两位大写字母；`ZZ` 表示「没有国家」 | 集合 | 国家码相等 |
//! | `page` | 以 `/` 开头的模式，只有 `*`（任意长度，含 `/`）是通配 | 精确值集合加分段通配 | 对**规范化后的 pathname** 区分大小写匹配 |
//! | `hostname` | 与站点白名单同语法（`example.com`、`*.example.com`） | [`HostnamePatterns`] | 对规范化后的事件 hostname 匹配 |
//!
//! **`action` 的语义**（文档只给出 `deny | allow` 两个取值，没有写语义，这里的取舍）：
//! `deny` 规则命中就丢弃；`allow` 规则是**同一类型内的例外**：值命中任一 `allow` 规则就不受该类型 `deny` 规则影响
//! （`allow` 优先）。只有 `allow` 没有 `deny` 时不起作用——不是白名单模式，没有规则就是放行（Plausible 的做法）。
//! 例如 `deny 10.0.0.0/8` 加 `allow 10.1.2.3`：屏蔽整个 10 网段，唯独放过 10.1.2.3。
//! 站点 hostname 白名单（`site_hostnames`）才是 hostname 的白名单，它在规则之前判定，永远 fail-closed。
//!
//! 页面匹配用分段比较而不是正则：模式按 `*` 切成「前缀、若干中段、后缀」，没有回溯，pathname 上限 1024 字节、
//! 模式上限 250 字节，所以匹配代价有界；其余字符（`.` `?` `[` `(` `\` `+` `$` `^`……）都是字面量，没有「转义」问题。
//!
//! IPv4-mapped IPv6（`::ffff:1.2.3.4`）在写入规则和匹配时都先转成 IPv4。

use std::collections::HashSet;
use std::fmt;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use rivu_storage_pg::shield::ShieldRuleRow;

use crate::hostname::{HostnamePatterns, validated_pattern};

/// 页面模式的最大长度（字节，Plausible 同为 250）。
pub const MAX_PAGE_PATTERN_LEN: usize = 250;

/// 规则类型（`shield_rules.type`）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RuleType {
    Ip,
    Cidr,
    Country,
    Hostname,
    Page,
}

impl RuleType {
    pub const ALL: [RuleType; 5] = [
        RuleType::Ip,
        RuleType::Cidr,
        RuleType::Country,
        RuleType::Hostname,
        RuleType::Page,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            RuleType::Ip => "ip",
            RuleType::Cidr => "cidr",
            RuleType::Country => "country",
            RuleType::Hostname => "hostname",
            RuleType::Page => "page",
        }
    }

    pub fn parse(s: &str) -> Option<RuleType> {
        RuleType::ALL.into_iter().find(|t| t.as_str() == s)
    }
}

impl fmt::Display for RuleType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 规则动作（`shield_rules.action`）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RuleAction {
    Deny,
    Allow,
}

impl RuleAction {
    pub fn as_str(self) -> &'static str {
        match self {
            RuleAction::Deny => "deny",
            RuleAction::Allow => "allow",
        }
    }

    pub fn parse(s: &str) -> Option<RuleAction> {
        match s {
            "deny" => Some(RuleAction::Deny),
            "allow" => Some(RuleAction::Allow),
            _ => None,
        }
    }
}

impl fmt::Display for RuleAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

// ---------------------------------------------------------------- 规范化与校验

/// 校验并规范化一条规则的取值，结果就是写进 `shield_rules.value` 的文本。
///
/// - `ip`：一个地址（不能带 `/`），IPv4-mapped 转 IPv4，IPv6 用小写压缩形式；
/// - `cidr`：必须带前缀长度，主机位清零（`10.1.2.3/8` 存为 `10.0.0.0/8`），IPv4-mapped 且前缀 ≥ 96 的转成 IPv4；
/// - `country`：两个 ASCII 字母，转大写（`ZZ` 匹配「没有国家」的事件）；
/// - `hostname`：同站点白名单的规范化；
/// - `page`：以 `/` 开头，≤ 250 字节，不含空白与控制字符，连续的 `*` 合并成一个（同一个模式只有一种写法，
///   `(site, type, value)` 的唯一约束才有意义）。
pub fn normalize_value(kind: RuleType, raw: &str) -> Result<String, String> {
    match kind {
        RuleType::Ip => {
            let s = raw.trim();
            if s.contains('/') {
                return Err(format!(
                    "`{s}` 带前缀长度：单个地址用 ip 类型，网段请用 cidr 类型"
                ));
            }
            let ip: IpAddr = s
                .parse()
                .map_err(|_| format!("`{s}` 不是有效的 IPv4 或 IPv6 地址"))?;
            Ok(ip.to_canonical().to_string())
        }
        RuleType::Cidr => {
            let (net, prefix) = parse_cidr(raw)?;
            Ok(format!("{net}/{prefix}"))
        }
        RuleType::Country => {
            let s = raw.trim().to_ascii_uppercase();
            if s.len() == 2 && s.bytes().all(|b| b.is_ascii_uppercase()) {
                Ok(s)
            } else {
                Err(format!(
                    "`{}` 不是两个字母的国家码（ISO 3166-1 alpha-2，如 CN；ZZ 表示未知）",
                    raw.trim()
                ))
            }
        }
        RuleType::Hostname => validated_pattern(raw).map_err(|e| format!("{e:#}")),
        RuleType::Page => {
            let s = raw.trim();
            if !s.starts_with('/') {
                return Err(format!("页面模式 `{s}` 必须以 `/` 开头"));
            }
            if s.len() > MAX_PAGE_PATTERN_LEN {
                return Err(format!("页面模式超过 {MAX_PAGE_PATTERN_LEN} 字节"));
            }
            if s.chars().any(|c| c.is_whitespace() || c.is_control()) {
                return Err(
                    "页面模式不能含空白或控制字符（pathname 是百分号编码的，空格写成 %20）".into(),
                );
            }
            let mut out = String::with_capacity(s.len());
            for c in s.chars() {
                if c == '*' && out.ends_with('*') {
                    continue;
                }
                out.push(c);
            }
            Ok(out)
        }
    }
}

/// 解析并规范化 CIDR：返回（网络地址，前缀长度）。
fn parse_cidr(raw: &str) -> Result<(IpAddr, u8), String> {
    let s = raw.trim();
    let (addr_part, prefix_part) = s
        .split_once('/')
        .ok_or_else(|| format!("`{s}` 缺少前缀长度（例如 10.0.0.0/8）；单个地址请用 ip 类型"))?;
    let addr: IpAddr = addr_part
        .parse()
        .map_err(|_| format!("`{s}` 的地址部分无效"))?;
    let max: u8 = if addr.is_ipv4() { 32 } else { 128 };
    let prefix: u8 = prefix_part
        .parse()
        .ok()
        .filter(|p| *p <= max)
        .ok_or_else(|| format!("`{s}` 的前缀长度必须在 0..={max}"))?;
    Ok(match addr {
        IpAddr::V4(a) => (IpAddr::V4(mask_v4(a, prefix)), prefix),
        IpAddr::V6(a) => match a.to_ipv4_mapped() {
            Some(v4) if prefix >= 96 => (IpAddr::V4(mask_v4(v4, prefix - 96)), prefix - 96),
            _ => (IpAddr::V6(mask_v6(a, prefix)), prefix),
        },
    })
}

fn mask_v4(a: Ipv4Addr, prefix: u8) -> Ipv4Addr {
    let bits = u32::from(a);
    let masked = if prefix == 0 {
        0
    } else {
        bits & (u32::MAX << (32 - u32::from(prefix)))
    };
    Ipv4Addr::from(masked)
}

fn mask_v6(a: Ipv6Addr, prefix: u8) -> Ipv6Addr {
    let bits = u128::from(a);
    let masked = if prefix == 0 {
        0
    } else {
        bits & (u128::MAX << (128 - u32::from(prefix)))
    };
    Ipv6Addr::from(masked)
}

// ---------------------------------------------------------------- 前缀树

/// 二叉前缀树：节点存在 `Vec` 里，用下标连接（0 是根，不会作为子节点出现，所以 0 表示「没有」）。
/// 查找最多走 32 或 128 步，没有分配。
#[derive(Clone, Debug, Default)]
struct PrefixTrie {
    nodes: Vec<TrieNode>,
}

#[derive(Clone, Copy, Debug, Default)]
struct TrieNode {
    child: [u32; 2],
    /// 有一条规则恰好在这个前缀上结束。
    terminal: bool,
}

impl PrefixTrie {
    fn bit(bits: u128, width: u8, i: u8) -> usize {
        ((bits >> (u32::from(width) - 1 - u32::from(i))) & 1) as usize
    }

    /// `bits` 是 `width` 位的地址（IPv4 为 32 位，放在低位），只有前 `prefix` 位有意义。
    fn insert(&mut self, bits: u128, width: u8, prefix: u8) {
        if self.nodes.is_empty() {
            self.nodes.push(TrieNode::default());
        }
        let mut node = 0usize;
        for i in 0..prefix {
            let b = Self::bit(bits, width, i);
            let next = self.nodes[node].child[b];
            node = if next == 0 {
                let idx = self.nodes.len();
                self.nodes.push(TrieNode::default());
                self.nodes[node].child[b] = u32::try_from(idx).unwrap_or(u32::MAX);
                idx
            } else {
                next as usize
            };
        }
        self.nodes[node].terminal = true;
    }

    /// 地址是否落在任何一条已插入的前缀内。
    fn contains(&self, bits: u128, width: u8) -> bool {
        let Some(root) = self.nodes.first() else {
            return false;
        };
        if root.terminal {
            return true; // /0
        }
        let mut node = 0usize;
        for i in 0..width {
            let next = self.nodes[node].child[Self::bit(bits, width, i)];
            if next == 0 {
                return false;
            }
            node = next as usize;
            if self.nodes[node].terminal {
                return true;
            }
        }
        false
    }

    fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }
}

// ---------------------------------------------------------------- 页面通配

/// 一个含 `*` 的模式：`prefix * middle[0] * middle[1] * ... * suffix`。
#[derive(Clone, Debug)]
struct Glob {
    prefix: Box<str>,
    middle: Vec<Box<str>>,
    suffix: Box<str>,
}

impl Glob {
    /// `pattern` 至少含一个 `*`。连续的 `*` 等价于一个，空的中段被丢弃。
    fn new(pattern: &str) -> Glob {
        let pieces: Vec<&str> = pattern.split('*').collect();
        let last = pieces.len() - 1;
        Glob {
            prefix: pieces[0].into(),
            middle: pieces[1..last]
                .iter()
                .filter(|p| !p.is_empty())
                .map(|p| Box::from(*p))
                .collect(),
            suffix: pieces[last].into(),
        }
    }

    /// 贪心的最左匹配：前缀、各中段按顺序各取最靠左的一处、后缀贴着结尾。只有 `*` 一种通配时这是完备的。
    fn matches(&self, text: &str) -> bool {
        if !text.starts_with(&*self.prefix) {
            return false;
        }
        let mut pos = self.prefix.len();
        for m in &self.middle {
            match text[pos..].find(&**m) {
                Some(i) => pos += i + m.len(),
                None => return false,
            }
        }
        text.len() >= pos + self.suffix.len() && text.ends_with(&*self.suffix)
    }
}

#[derive(Clone, Debug, Default)]
struct PageRules {
    exact: HashSet<Box<str>>,
    globs: Vec<Glob>,
}

impl PageRules {
    fn insert(&mut self, pattern: &str) {
        if pattern.contains('*') {
            self.globs.push(Glob::new(pattern));
        } else {
            self.exact.insert(pattern.into());
        }
    }

    fn matches(&self, path: &str) -> bool {
        self.exact.contains(path) || self.globs.iter().any(|g| g.matches(path))
    }

    fn is_empty(&self) -> bool {
        self.exact.is_empty() && self.globs.is_empty()
    }
}

// ---------------------------------------------------------------- 规则集

/// 一个动作（`deny` 或 `allow`）下的全部规则。
#[derive(Clone, Debug, Default)]
struct RuleSet {
    ips: HashSet<IpAddr>,
    v4: PrefixTrie,
    v6: PrefixTrie,
    countries: HashSet<[u8; 2]>,
    pages: PageRules,
    host_patterns: Vec<String>,
    hosts: HostnamePatterns,
}

impl RuleSet {
    fn has_ip_rules(&self) -> bool {
        !self.ips.is_empty() || !self.v4.is_empty() || !self.v6.is_empty()
    }

    /// `ip` 须已转成规范形式（IPv4-mapped 转 IPv4）。
    fn matches_ip(&self, ip: IpAddr) -> bool {
        if self.ips.contains(&ip) {
            return true;
        }
        match ip {
            IpAddr::V4(a) => self.v4.contains(u128::from(u32::from(a)), 32),
            IpAddr::V6(a) => self.v6.contains(u128::from(a), 128),
        }
    }
}

/// 一条规则编译失败的原因（库里的行不合法，跳过它）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuleError {
    pub rule_id: String,
    pub reason: String,
}

/// 一个站点编译好的屏蔽规则。不可变，随站点快照整体替换。
#[derive(Clone, Debug, Default)]
pub struct ShieldRules {
    deny: RuleSet,
    allow: RuleSet,
    count: usize,
}

impl ShieldRules {
    /// 编译一个站点的规则行。不合法的行（未知类型或动作、取值通不过校验）被跳过并返回错误列表，不影响其余规则：
    /// 校验在写入时已经做过，这里只是防御手工改库。
    pub fn compile(rows: &[ShieldRuleRow]) -> (ShieldRules, Vec<RuleError>) {
        let mut rules = ShieldRules::default();
        let mut errors = Vec::new();
        for row in rows {
            if let Err(reason) = rules.add_row(row) {
                errors.push(RuleError {
                    rule_id: row.id.clone(),
                    reason,
                });
            }
        }
        for set in [&mut rules.deny, &mut rules.allow] {
            set.hosts = HostnamePatterns::new(set.host_patterns.iter().map(String::as_str));
        }
        (rules, errors)
    }

    fn add_row(&mut self, row: &ShieldRuleRow) -> Result<(), String> {
        let kind =
            RuleType::parse(&row.kind).ok_or_else(|| format!("未知的规则类型 `{}`", row.kind))?;
        let action = RuleAction::parse(&row.action)
            .ok_or_else(|| format!("未知的规则动作 `{}`", row.action))?;
        let value = normalize_value(kind, &row.value)?;
        let set = match action {
            RuleAction::Deny => &mut self.deny,
            RuleAction::Allow => &mut self.allow,
        };
        match kind {
            RuleType::Ip => {
                if let Ok(ip) = value.parse::<IpAddr>() {
                    set.ips.insert(ip);
                }
            }
            RuleType::Cidr => {
                let (net, prefix) = parse_cidr(&value)?;
                match net {
                    IpAddr::V4(a) => set.v4.insert(u128::from(u32::from(a)), 32, prefix),
                    IpAddr::V6(a) => set.v6.insert(u128::from(a), 128, prefix),
                }
            }
            RuleType::Country => {
                let b = value.as_bytes();
                set.countries.insert([b[0], b[1]]);
            }
            RuleType::Page => set.pages.insert(&value),
            // 模式先收集，`compile` 结束时一次性编译（`HostnamePatterns` 不支持增量添加）
            RuleType::Hostname => set.host_patterns.push(value),
        }
        self.count += 1;
        Ok(())
    }

    /// 成功编译的规则数。
    pub fn len(&self) -> usize {
        self.count
    }

    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// 客户端 IP 被 `ip` / `cidr` 规则屏蔽（`ip_blocked`）：命中 `deny` 且不命中 `allow`。
    pub fn ip_blocked(&self, ip: IpAddr) -> bool {
        if !self.deny.has_ip_rules() {
            return false;
        }
        let ip = ip.to_canonical();
        self.deny.matches_ip(ip) && !self.allow.matches_ip(ip)
    }

    /// 规范化后的 pathname 被 `page` 规则屏蔽（`page_blocked`）。区分大小写。
    pub fn page_blocked(&self, pathname: &str) -> bool {
        !self.deny.pages.is_empty()
            && self.deny.pages.matches(pathname)
            && !self.allow.pages.matches(pathname)
    }

    /// 国家被 `country` 规则屏蔽（`country_blocked`）。`[0, 0]`（没有国家）按 `ZZ` 判定，与 Plausible 一致。
    pub fn country_blocked(&self, code: [u8; 2]) -> bool {
        if self.deny.countries.is_empty() {
            return false;
        }
        let code = if code == [0, 0] { *b"ZZ" } else { code };
        self.deny.countries.contains(&code) && !self.allow.countries.contains(&code)
    }

    /// 规范化后的事件 hostname 被 `hostname` 规则屏蔽（记 `hostname_not_allowed`）。
    pub fn hostname_blocked(&self, host: &str) -> bool {
        !self.deny.hosts.is_empty()
            && self.deny.hosts.matches(host)
            && !self.allow.hosts.matches(host)
    }

    /// 有没有国家规则（没有就不必为它读国家码）。
    pub fn has_country_rules(&self) -> bool {
        !self.deny.countries.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;
    use rivu_common::config::Cidr;

    use super::*;

    fn row(kind: &str, action: &str, value: &str) -> ShieldRuleRow {
        ShieldRuleRow {
            id: format!("{kind}-{action}-{value}"),
            site_id: 1,
            kind: kind.into(),
            action: action.into(),
            value: value.into(),
            description: None,
        }
    }

    fn deny(kind: &str, value: &str) -> ShieldRuleRow {
        row(kind, "deny", value)
    }

    fn compile(rows: &[ShieldRuleRow]) -> ShieldRules {
        let (rules, errors) = ShieldRules::compile(rows);
        assert!(errors.is_empty(), "{errors:?}");
        rules
    }

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    // ------------------------------------------------------------ CIDR

    #[test]
    fn cidr_ipv4_matches_inside_and_not_outside() {
        let r = compile(&[deny("cidr", "10.0.0.0/8"), deny("cidr", "192.168.1.128/25")]);
        for inside in [
            "10.0.0.0",
            "10.255.255.255",
            "10.1.2.3",
            "192.168.1.128",
            "192.168.1.255",
        ] {
            assert!(r.ip_blocked(ip(inside)), "{inside}");
        }
        for outside in [
            "9.255.255.255",
            "11.0.0.0",
            "192.168.1.127",
            "192.168.2.128",
            "8.8.8.8",
        ] {
            assert!(!r.ip_blocked(ip(outside)), "{outside}");
        }
    }

    #[test]
    fn cidr_ipv6_matches_inside_and_not_outside() {
        let r = compile(&[deny("cidr", "2001:db8::/32"), deny("cidr", "fe80::/10")]);
        for inside in [
            "2001:db8::",
            "2001:db8:ffff:ffff:ffff:ffff:ffff:ffff",
            "2001:db8::1",
            "fe80::1",
            "febf::1",
        ] {
            assert!(r.ip_blocked(ip(inside)), "{inside}");
        }
        for outside in [
            "2001:db7:ffff:ffff:ffff:ffff:ffff:ffff",
            "2001:db9::",
            "fec0::1",
            "::1",
            "2606:4700::1",
        ] {
            assert!(!r.ip_blocked(ip(outside)), "{outside}");
        }
    }

    #[test]
    fn boundary_prefixes_zero_full_and_family_isolation() {
        // /0：该地址族全部命中，另一个地址族不受影响
        let v4_all = compile(&[deny("cidr", "0.0.0.0/0")]);
        assert!(v4_all.ip_blocked(ip("1.2.3.4")) && v4_all.ip_blocked(ip("255.255.255.255")));
        assert!(!v4_all.ip_blocked(ip("2001:db8::1")));
        let v6_all = compile(&[deny("cidr", "::/0")]);
        assert!(v6_all.ip_blocked(ip("2001:db8::1")) && v6_all.ip_blocked(ip("::")));
        assert!(!v6_all.ip_blocked(ip("1.2.3.4")));
        // /32、/128：只有这一个地址
        let host = compile(&[
            deny("cidr", "203.0.113.7/32"),
            deny("cidr", "2001:db8::7/128"),
        ]);
        assert!(host.ip_blocked(ip("203.0.113.7")) && host.ip_blocked(ip("2001:db8::7")));
        assert!(!host.ip_blocked(ip("203.0.113.6")) && !host.ip_blocked(ip("203.0.113.8")));
        assert!(!host.ip_blocked(ip("2001:db8::6")) && !host.ip_blocked(ip("2001:db8::8")));
    }

    #[test]
    fn nested_prefixes_and_overlaps_all_work() {
        let r = compile(&[
            deny("cidr", "10.0.0.0/8"),
            deny("cidr", "10.1.0.0/16"),
            deny("cidr", "10.1.2.0/24"),
        ]);
        assert!(
            r.ip_blocked(ip("10.1.2.3"))
                && r.ip_blocked(ip("10.1.9.9"))
                && r.ip_blocked(ip("10.9.9.9"))
        );
        assert!(!r.ip_blocked(ip("11.1.2.3")));
    }

    #[test]
    fn ipv4_mapped_addresses_are_converted_for_rules_and_for_matching() {
        let r = compile(&[deny("ip", "1.2.3.4"), deny("cidr", "10.0.0.0/8")]);
        assert!(r.ip_blocked(ip("::ffff:1.2.3.4")));
        assert!(r.ip_blocked(ip("::ffff:10.9.9.9")));
        assert!(!r.ip_blocked(ip("::ffff:1.2.3.5")));
        // 规则写成映射形式：规范化成 IPv4
        assert_eq!(
            normalize_value(RuleType::Ip, "::ffff:1.2.3.4").unwrap(),
            "1.2.3.4"
        );
        assert_eq!(
            normalize_value(RuleType::Cidr, "::ffff:10.0.0.0/104").unwrap(),
            "10.0.0.0/8"
        );
        let r = compile(&[deny("cidr", "::ffff:10.0.0.0/104")]);
        assert!(r.ip_blocked(ip("10.1.1.1")) && r.ip_blocked(ip("::ffff:10.1.1.1")));
        // 前缀短于 96 的映射网段不是 IPv4 网段，保持 IPv6
        assert_eq!(
            normalize_value(RuleType::Cidr, "::ffff:0:0/80").unwrap(),
            "::/80"
        );
    }

    #[test]
    fn exact_ip_rules_use_canonical_text() {
        let r = compile(&[
            deny("ip", "2001:DB8:0:0:0:0:0:1"),
            deny("ip", " 203.0.113.9 "),
        ]);
        assert!(r.ip_blocked(ip("2001:db8::1")));
        assert!(r.ip_blocked(ip("203.0.113.9")));
        assert!(!r.ip_blocked(ip("2001:db8::2")));
        assert_eq!(
            normalize_value(RuleType::Ip, "2001:DB8:0:0:0:0:0:1").unwrap(),
            "2001:db8::1"
        );
    }

    #[test]
    fn cidr_normalization_clears_host_bits_and_rejects_bad_input() {
        assert_eq!(
            normalize_value(RuleType::Cidr, "10.1.2.3/8").unwrap(),
            "10.0.0.0/8"
        );
        assert_eq!(
            normalize_value(RuleType::Cidr, "192.168.1.200/25").unwrap(),
            "192.168.1.128/25"
        );
        assert_eq!(
            normalize_value(RuleType::Cidr, "2001:db8::1/32").unwrap(),
            "2001:db8::/32"
        );
        assert_eq!(
            normalize_value(RuleType::Cidr, "1.2.3.4/0").unwrap(),
            "0.0.0.0/0"
        );
        assert_eq!(
            normalize_value(RuleType::Cidr, "1.2.3.4/32").unwrap(),
            "1.2.3.4/32"
        );
        for bad in [
            "10.0.0.0",
            "10.0.0.0/33",
            "::/129",
            "10.0.0.0/-1",
            "10.0.0.0/x",
            "nope/8",
            "/8",
            "",
            "10.0.0.0/8/9",
        ] {
            assert!(normalize_value(RuleType::Cidr, bad).is_err(), "{bad:?}");
        }
        for bad in ["10.0.0.0/8", "nope", "", "1.2.3", "1.2.3.4.5", "1.2.3.4:80"] {
            assert!(normalize_value(RuleType::Ip, bad).is_err(), "{bad:?}");
        }
    }

    proptest! {
        /// 前缀树与 `rivu_common::config::Cidr::contains`（独立实现）在任意前缀与地址上结论一致。
        #[test]
        fn trie_agrees_with_cidr_contains_v4(net in any::<u32>(), prefix in 0u8..=32, probe in any::<u32>(), near in any::<bool>()) {
            let net = Ipv4Addr::from(net);
            let probe = if near { Ipv4Addr::from(u32::from(net) ^ (probe & 0xff)) } else { Ipv4Addr::from(probe) };
            let text = normalize_value(RuleType::Cidr, &format!("{net}/{prefix}")).unwrap();
            let r = compile(&[deny("cidr", &text)]);
            let want = text.parse::<Cidr>().unwrap().contains(IpAddr::V4(probe));
            prop_assert_eq!(r.ip_blocked(IpAddr::V4(probe)), want, "{} {}", text, probe);
        }

        #[test]
        fn trie_agrees_with_cidr_contains_v6(net in any::<u128>(), prefix in 0u8..=128, probe in any::<u128>(), near in any::<bool>()) {
            let net = Ipv6Addr::from(net);
            let probe = if near { Ipv6Addr::from(u128::from(net) ^ (probe & 0xffff)) } else { Ipv6Addr::from(probe) };
            // 排除 IPv4-mapped：它们在匹配前会被转成 IPv4，属于另一条路径（上面的专门测试覆盖）
            prop_assume!(net.to_ipv4_mapped().is_none() && probe.to_ipv4_mapped().is_none());
            let text = normalize_value(RuleType::Cidr, &format!("{net}/{prefix}")).unwrap();
            let r = compile(&[deny("cidr", &text)]);
            let want = text.parse::<Cidr>().unwrap().contains(IpAddr::V6(probe));
            prop_assert_eq!(r.ip_blocked(IpAddr::V6(probe)), want, "{} {}", text, probe);
        }
    }

    // ------------------------------------------------------------ 页面通配

    #[test]
    fn page_patterns_without_a_star_are_exact_and_case_sensitive() {
        let r = compile(&[deny("page", "/admin"), deny("page", "/Docs/Intro")]);
        assert!(r.page_blocked("/admin"));
        assert!(!r.page_blocked("/admin/"), "不带 * 就是精确匹配");
        assert!(!r.page_blocked("/Admin"), "区分大小写");
        assert!(r.page_blocked("/Docs/Intro"));
        assert!(!r.page_blocked("/docs/intro"));
    }

    #[test]
    fn star_matches_any_run_including_slashes_and_nothing() {
        let r = compile(&[
            deny("page", "/admin*"),
            deny("page", "/blog/*/draft"),
            deny("page", "/*.pdf"),
            deny("page", "/a*b*c"),
        ]);
        for hit in [
            "/admin",
            "/admin/",
            "/admin/users/1",
            "/adminx",
            "/blog/2026/draft",
            "/blog/a/b/draft",
            "/blog//draft",
            "/x.pdf",
            "/a/b/c.pdf",
            "/abc",
            "/aXbYc",
            "/a/b/c",
        ] {
            assert!(r.page_blocked(hit), "{hit}");
        }
        for miss in [
            "/",
            "/adm",
            "/blog/draft",
            "/blog/2026/drafts",
            "/blog/2026/draft/x",
            "/x.pdf/",
            "/x.PDF",
            "/ab",
            "/acb",
            "/aXcYb",
        ] {
            assert!(!r.page_blocked(miss), "{miss}");
        }
    }

    #[test]
    fn everything_other_than_star_is_a_literal() {
        // 这些字符在正则里都有含义；这里全部是字面量
        let r = compile(&[
            deny("page", "/a.b"),
            deny("page", "/q?x"),
            deny("page", "/[ab]"),
            deny("page", "/(x|y)"),
            deny("page", "/p+q"),
            deny("page", "/c$"),
            deny("page", "/^d"),
            deny("page", "/e\\f"),
            deny("page", "/{1,2}"),
            deny("page", "/g.*h"),
        ]);
        for hit in [
            "/a.b", "/q?x", "/[ab]", "/(x|y)", "/p+q", "/c$", "/^d", "/e\\f", "/{1,2}", "/g.zzz.h",
            "/g.h",
        ] {
            assert!(r.page_blocked(hit), "{hit}");
        }
        for miss in [
            "/aXb", "/qx", "/q", "/a", "/x", "/y", "/pq", "/ppq", "/c", "/d", "/e/f", "/ef", "/1",
            "/gXh", "/g",
        ] {
            assert!(!r.page_blocked(miss), "{miss}");
        }
    }

    #[test]
    fn page_normalization_collapses_stars_and_validates() {
        assert_eq!(
            normalize_value(RuleType::Page, " /a**b***c ").unwrap(),
            "/a*b*c"
        );
        assert_eq!(normalize_value(RuleType::Page, "/*").unwrap(), "/*");
        for bad in ["", "admin", "*", "/a b", "/a\tb", "/a\nb"] {
            assert!(normalize_value(RuleType::Page, bad).is_err(), "{bad:?}");
        }
        let long = format!("/{}", "a".repeat(MAX_PAGE_PATTERN_LEN));
        assert!(normalize_value(RuleType::Page, &long).is_err());
        let ok = format!("/{}", "a".repeat(MAX_PAGE_PATTERN_LEN - 1));
        assert!(normalize_value(RuleType::Page, &ok).is_ok());
    }

    /// 朴素的递归参考实现：`*` 匹配任意长度（含空）的任意字符。
    fn naive_glob(p: &[u8], t: &[u8]) -> bool {
        match p.split_first() {
            None => t.is_empty(),
            Some((b'*', rest)) => (0..=t.len()).any(|i| naive_glob(rest, &t[i..])),
            Some((c, rest)) => t.first() == Some(c) && naive_glob(rest, &t[1..]),
        }
    }

    proptest! {
        #[test]
        fn glob_agrees_with_a_naive_recursive_matcher(pattern in "/[ab*/.]{0,10}", text in "/[ab/.]{0,14}") {
            let normalized = normalize_value(RuleType::Page, &pattern).unwrap();
            let r = compile(&[deny("page", &normalized)]);
            prop_assert_eq!(
                r.page_blocked(&text),
                naive_glob(normalized.as_bytes(), text.as_bytes()),
                "{} vs {}", normalized, text
            );
        }
    }

    // ------------------------------------------------------------ 国家与主机名

    #[test]
    fn country_rules_use_uppercase_codes_and_zz_means_unknown() {
        let r = compile(&[deny("country", "cn"), deny("country", "ZZ")]);
        assert!(r.country_blocked(*b"CN"));
        assert!(!r.country_blocked(*b"US"));
        assert!(r.country_blocked([0, 0]), "没有国家按 ZZ 判定");
        let r = compile(&[deny("country", "CN")]);
        assert!(!r.country_blocked([0, 0]));
        assert!(r.has_country_rules());
        assert_eq!(normalize_value(RuleType::Country, " us ").unwrap(), "US");
        for bad in ["", "U", "USA", "U1", "中国", "A-"] {
            assert!(normalize_value(RuleType::Country, bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn hostname_rules_follow_the_whitelist_syntax() {
        let r = compile(&[
            deny("hostname", "staging.example.com"),
            deny("hostname", "*.internal.example.com"),
        ]);
        assert!(r.hostname_blocked("staging.example.com"));
        assert!(r.hostname_blocked("a.internal.example.com"));
        assert!(!r.hostname_blocked("internal.example.com"), "通配不含根域");
        assert!(!r.hostname_blocked("example.com"));
        assert_eq!(
            normalize_value(RuleType::Hostname, "WWW.Staging.Example.com").unwrap(),
            "staging.example.com"
        );
        assert!(normalize_value(RuleType::Hostname, "a.*.com").is_err());
    }

    // ------------------------------------------------------------ action 语义与编译

    #[test]
    fn allow_is_an_exception_within_the_same_type() {
        let r = compile(&[
            deny("cidr", "10.0.0.0/8"),
            row("ip", "allow", "10.1.2.3"),
            deny("page", "/admin*"),
            row("page", "allow", "/admin/public*"),
            deny("country", "CN"),
            row("country", "allow", "CN"),
        ]);
        assert!(r.ip_blocked(ip("10.9.9.9")));
        assert!(!r.ip_blocked(ip("10.1.2.3")), "allow 优先");
        assert!(r.page_blocked("/admin/users"));
        assert!(!r.page_blocked("/admin/public/x"));
        assert!(!r.country_blocked(*b"CN"), "同值的 allow 抵消 deny");
        // 例外只在同一类型内：IP 的 allow 不会放过页面规则
        assert!(r.page_blocked("/admin"));
    }

    #[test]
    fn allow_rules_alone_do_not_block_anything() {
        let r = compile(&[
            row("ip", "allow", "1.2.3.4"),
            row("cidr", "allow", "10.0.0.0/8"),
            row("page", "allow", "/ok*"),
            row("country", "allow", "CN"),
        ]);
        assert!(!r.ip_blocked(ip("8.8.8.8")) && !r.ip_blocked(ip("10.1.1.1")));
        assert!(!r.page_blocked("/other") && !r.page_blocked("/ok"));
        assert!(!r.country_blocked(*b"US"));
        assert_eq!(r.len(), 4);
    }

    #[test]
    fn empty_rules_block_nothing() {
        let r = ShieldRules::default();
        assert!(r.is_empty());
        assert!(
            !r.ip_blocked(ip("1.2.3.4"))
                && !r.page_blocked("/")
                && !r.country_blocked(*b"CN")
                && !r.hostname_blocked("a.com")
        );
        assert!(!r.has_country_rules());
    }

    #[test]
    fn invalid_rows_are_skipped_and_reported_without_affecting_the_rest() {
        let rows = [
            deny("ip", "1.2.3.4"),
            deny("ip", "not an ip"),
            deny("frobnicate", "x"),
            row("ip", "maybe", "5.6.7.8"),
            deny("cidr", "10.0.0.0/99"),
            deny("page", "no-slash"),
            deny("country", "XYZ"),
            deny("page", "/ok"),
        ];
        let (rules, errors) = ShieldRules::compile(&rows);
        assert_eq!(rules.len(), 2);
        assert_eq!(errors.len(), 6);
        assert!(rules.ip_blocked(ip("1.2.3.4")) && rules.page_blocked("/ok"));
        assert!(!rules.ip_blocked(ip("5.6.7.8")));
        assert!(
            errors
                .iter()
                .all(|e| !e.reason.is_empty() && !e.rule_id.is_empty())
        );
    }

    #[test]
    fn type_and_action_names_round_trip() {
        for t in RuleType::ALL {
            assert_eq!(RuleType::parse(t.as_str()), Some(t));
        }
        assert_eq!(RuleType::parse("IP"), None);
        for a in [RuleAction::Deny, RuleAction::Allow] {
            assert_eq!(RuleAction::parse(a.as_str()), Some(a));
        }
        assert_eq!(RuleAction::parse("block"), None);
    }

    #[test]
    fn many_rules_compile_and_match_fast_enough_to_be_boring() {
        // 每站上限 500 条：500 条 CIDR 加 500 个页面模式，匹配仍是微秒级
        let mut rows = Vec::new();
        for i in 0..250u32 {
            rows.push(deny("cidr", &format!("10.{}.{}.0/24", i / 256, i % 256)));
            rows.push(deny("cidr", &format!("2001:db8:{i:x}::/48")));
            rows.push(deny("page", &format!("/p{i}/*")));
            rows.push(deny("page", &format!("/exact{i}")));
        }
        let r = compile(&rows);
        assert_eq!(r.len(), 1000);
        assert!(r.ip_blocked(ip("10.0.7.9")) && r.ip_blocked(ip("2001:db8:a::1")));
        assert!(!r.ip_blocked(ip("10.1.0.1")) && !r.ip_blocked(ip("2001:db8:ffff::1")));
        assert!(r.page_blocked("/p249/x/y") && r.page_blocked("/exact3"));
        assert!(!r.page_blocked("/p250/x") && !r.page_blocked("/exact250"));
    }
}
