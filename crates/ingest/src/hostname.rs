//! hostname 规范化与站点白名单（`tracker-and-protocol.md` §7 的「hostname」「白名单」两行）。

use std::collections::HashSet;

use url::Host;

/// 去掉结尾的端口（`:8080`、`[::1]:80`）。裸 IPv6（`::1`）里的冒号不是端口分隔符。
fn strip_port(s: &str) -> &str {
    if let Some(i) = s.rfind(':') {
        let (head, tail) = (&s[..i], &s[i + 1..]);
        if tail.bytes().all(|b| b.is_ascii_digit()) && (head.ends_with(']') || !head.contains(':'))
        {
            return head;
        }
    }
    s
}

/// 反复去掉首尾空白、端口、结尾的 `.`，直到不再变化。
///
/// 三者互相可能暴露出对方（`a.:80`、`a:80.`、`a .`），迭代到不动点才能保证幂等。
fn settle(mut s: &str) -> &str {
    loop {
        let next = strip_port(s.trim()).trim_end_matches('.');
        if next.len() == s.len() {
            return next;
        }
        s = next;
    }
}

fn normalize_with(host: &str, strip_www: bool) -> String {
    let lowered = host.to_lowercase();
    let s = settle(&lowered);
    let s = if strip_www {
        s.strip_prefix("www.").unwrap_or(s)
    } else {
        s
    };
    settle(s).to_owned()
}

/// hostname 规范化：小写 → 去结尾 `.` → 去**一个**前导 `www.` → 去端口；IP 字面量原样保留。
///
/// `url` crate 给出的 `host_str()` 已经是 punycode 小写形式，可直接传入；这里不做 IDNA 转换
/// （白名单模式里的 Unicode 域名由 [`HostnamePatterns::new`] 转换）。
///
/// 幂等，唯一的例外是 `www.www.x`：文档只要求去掉「一个」前导 `www.`，所以结果仍以 `www.` 开头时
/// 再规范化一次会再去掉一个。线上每个 hostname 只规范化一次，不受影响。
pub fn normalize_hostname(host: &str) -> String {
    normalize_with(host, true)
}

/// 白名单模式的规范化：先按 hostname 规则处理，再借 `url` crate 把 Unicode 域名转成 punycode。
fn normalize_pattern(pattern: &str, strip_www: bool) -> String {
    let n = normalize_with(pattern, strip_www);
    match Host::parse(&n) {
        Ok(Host::Domain(d)) => normalize_with(&d, strip_www),
        Ok(other) => other.to_string(),
        Err(_) => n,
    }
}

/// 站点 `hostnames` 白名单。精确匹配，或 `*.example.com` 通配。
#[derive(Clone, Debug, Default)]
pub struct HostnamePatterns {
    exact: HashSet<String>,
    /// 通配后缀，含前导 `.`（`*.example.com` 存为 `.example.com`）。
    wildcard_suffixes: HashSet<String>,
}

impl HostnamePatterns {
    /// 模式先按 hostname 规则规范化。
    ///
    /// - `*.example.com` 匹配任意深度的子域，**不含**根域；通配只允许最左一段，
    ///   `a.*.com`、`*example.com`、`**.x.com` 这类写法视为无效并忽略；
    /// - 通配符之后的部分**不去** `www.`（`*.www.example.com` 表示 `www.example.com` 的子域，
    ///   去掉会悄悄放宽到整个 `example.com`）；
    /// - 空串和无效模式忽略，不报错。
    pub fn new<'a>(patterns: impl IntoIterator<Item = &'a str>) -> Self {
        let mut out = Self::default();
        for raw in patterns {
            let p = raw.trim();
            if let Some(rest) = p.strip_prefix("*.") {
                if rest.contains('*') {
                    continue;
                }
                let base = normalize_pattern(rest, false);
                if !base.is_empty() {
                    out.wildcard_suffixes.insert(format!(".{base}"));
                }
            } else if !p.contains('*') {
                let exact = normalize_pattern(p, true);
                if !exact.is_empty() {
                    out.exact.insert(exact);
                }
            }
        }
        out
    }

    /// `host` 须已经过 [`normalize_hostname`]。
    pub fn matches(&self, host: &str) -> bool {
        if self.exact.contains(host) {
            return true;
        }
        if self.wildcard_suffixes.is_empty() {
            return false;
        }
        // 每个 `.` 的位置都是一个候选后缀；`pos > 0` 保证最左一段非空，即不匹配根域本身。
        host.match_indices('.')
            .any(|(pos, _)| pos > 0 && self.wildcard_suffixes.contains(&host[pos..]))
    }

    /// 没有任何有效模式。调用方必须 fail-closed：全部丢弃（`system-design.md` §10 Q12）。
    pub fn is_empty(&self) -> bool {
        self.exact.is_empty() && self.wildcard_suffixes.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn lowercases() {
        assert_eq!(normalize_hostname("Example.COM"), "example.com");
    }

    #[test]
    fn strips_trailing_dot() {
        assert_eq!(normalize_hostname("example.com."), "example.com");
        assert_eq!(normalize_hostname("example.com.."), "example.com");
    }

    #[test]
    fn strips_one_leading_www() {
        assert_eq!(normalize_hostname("www.example.com"), "example.com");
        assert_eq!(normalize_hostname("WWW.Example.com."), "example.com");
        assert_eq!(normalize_hostname("www.www.example.com"), "www.example.com");
        // 只认 `www.` 前缀，不动 `www2.`、`awww.`。
        assert_eq!(normalize_hostname("www2.example.com"), "www2.example.com");
        assert_eq!(normalize_hostname("awww.example.com"), "awww.example.com");
        assert_eq!(normalize_hostname("www"), "www");
    }

    #[test]
    fn strips_port() {
        assert_eq!(normalize_hostname("example.com:8080"), "example.com");
        assert_eq!(normalize_hostname("www.Example.com.:443"), "example.com");
        assert_eq!(normalize_hostname("example.com:"), "example.com");
    }

    #[test]
    fn keeps_ip_literals() {
        assert_eq!(normalize_hostname("127.0.0.1"), "127.0.0.1");
        assert_eq!(normalize_hostname("127.0.0.1:3000"), "127.0.0.1");
        assert_eq!(normalize_hostname("[::1]"), "[::1]");
        assert_eq!(normalize_hostname("[::1]:8080"), "[::1]");
        assert_eq!(normalize_hostname("::1"), "::1");
        assert_eq!(normalize_hostname("[2001:DB8::1]"), "[2001:db8::1]");
    }

    #[test]
    fn punycode_passes_through() {
        assert_eq!(
            normalize_hostname("xn--e1afmkfd.xn--p1ai"),
            "xn--e1afmkfd.xn--p1ai"
        );
    }

    #[test]
    fn trims_whitespace_only() {
        assert_eq!(normalize_hostname("  Example.com "), "example.com");
        assert_eq!(normalize_hostname(""), "");
        assert_eq!(normalize_hostname("."), "");
    }

    #[test]
    fn exact_pattern_matches_after_normalization() {
        let p = HostnamePatterns::new(["WWW.Example.com", "blog.example.com:443"]);
        assert!(p.matches("example.com"));
        assert!(p.matches("blog.example.com"));
        assert!(!p.matches("shop.example.com"));
        assert!(!p.matches("example.org"));
    }

    #[test]
    fn wildcard_matches_subdomains_of_any_depth_but_not_root() {
        let p = HostnamePatterns::new(["*.example.com"]);
        assert!(p.matches("a.example.com"));
        assert!(p.matches("a.b.example.com"));
        assert!(!p.matches("example.com"));
        assert!(!p.matches("notexample.com"));
        assert!(!p.matches("a.example.com.evil.org"));
        assert!(!p.matches(".example.com"));
    }

    #[test]
    fn wildcard_and_root_listed_separately() {
        let p = HostnamePatterns::new(["example.com", "*.example.com"]);
        assert!(p.matches("example.com"));
        assert!(p.matches("a.example.com"));
    }

    #[test]
    fn wildcard_base_keeps_www() {
        let p = HostnamePatterns::new(["*.www.example.com"]);
        assert!(p.matches("a.www.example.com"));
        assert!(!p.matches("a.example.com"));
        assert!(!p.matches("example.com"));
    }

    #[test]
    fn invalid_wildcards_are_ignored() {
        let p = HostnamePatterns::new(["a.*.com", "*example.com", "**.example.com", "*", "*.", ""]);
        assert!(p.is_empty());
        assert!(!p.matches("a.example.com"));
        assert!(!p.matches("example.com"));
    }

    #[test]
    fn empty_whitelist_is_empty_and_matches_nothing() {
        let p = HostnamePatterns::new(std::iter::empty());
        assert!(p.is_empty());
        assert!(!p.matches("example.com"));
        assert!(!p.matches(""));
        assert!(!HostnamePatterns::new(["example.com"]).is_empty());
        assert!(!HostnamePatterns::new(["*.example.com"]).is_empty());
    }

    #[test]
    fn unicode_patterns_become_punycode() {
        let p = HostnamePatterns::new(["例え.jp", "*.Бар.рф"]);
        assert!(p.matches("xn--r8jz45g.jp"));
        // 期望值取 `url` crate 对同一 Unicode 域名给出的 punycode（`host_str()`）。
        let u = url::Url::parse("https://a.Бар.рф/").unwrap();
        let sub = normalize_hostname(u.host_str().unwrap());
        assert!(sub.contains("xn--"));
        assert!(p.matches(&sub));
        assert!(!p.matches(sub.split_once('.').unwrap().1));
    }

    #[test]
    fn ip_patterns_match_exactly() {
        let p = HostnamePatterns::new(["127.0.0.1", "[::1]"]);
        assert!(p.matches("127.0.0.1"));
        assert!(p.matches("[::1]"));
        assert!(!p.matches("127.0.0.2"));
    }

    proptest! {
        /// 任意字符串规范化后再规范化，结果不变（`www.www.` 开头的结果除外，见 `normalize_hostname` 文档）。
        #[test]
        fn normalize_is_idempotent(s in any::<String>()) {
            let once = normalize_hostname(&s);
            let twice = normalize_hostname(&once);
            prop_assert!(twice == once || once.starts_with("www."), "{once:?} -> {twice:?}");
        }

        #[test]
        fn normalize_is_idempotent_for_hostname_like(s in "[A-Za-z0-9.:\\[\\] -]{0,30}") {
            let once = normalize_hostname(&s);
            let twice = normalize_hostname(&once);
            prop_assert!(twice == once || once.starts_with("www."), "{once:?} -> {twice:?}");
        }

        #[test]
        fn patterns_never_panic(p in any::<String>(), h in any::<String>()) {
            let pats = HostnamePatterns::new([p.as_str()]);
            let _ = pats.matches(&normalize_hostname(&h));
        }
    }
}
