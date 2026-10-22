//! 来源查找表：host 后缀匹配、国家/地区后缀占位符、utm_source 别名。
//!
//! 表本身由 `build.rs` 从 `data/` 生成，这里只有查询。三张表都是排好序的静态切片，
//! 查询是二分，比较时就地做 ASCII 大小写折叠，所以不需要先把输入转成小写：热路径没有分配。

use std::cmp::Ordering;

/// 来源的类别，渠道规则按它判断「这是搜索/社交/视频/购物/AI 助手/网页邮箱」。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Search,
    Social,
    Video,
    Shopping,
    Ai,
    Email,
    /// 只有名字，没有类别（Wikipedia、Linktree……），渠道落到 Referral。
    Other,
}

pub(crate) struct Entry {
    pub name: &'static str,
    pub kind: Kind,
}

include!(concat!(env!("OUT_DIR"), "/tables.rs"));

/// `key` 已经是小写；`input` 逐字节折叠成小写后比较，顺序与 build.rs 里 `BTreeMap<String, _>` 一致。
fn cmp_ci(input: &str, key: &str) -> Ordering {
    let (a, b) = (input.as_bytes(), key.as_bytes());
    for (x, y) in a.iter().zip(b) {
        match x.to_ascii_lowercase().cmp(y) {
            Ordering::Equal => {}
            other => return other,
        }
    }
    a.len().cmp(&b.len())
}

fn find_host(table: &[(&str, u16)], input: &str) -> Option<u16> {
    table
        .binary_search_by(|(k, _)| cmp_ci(input, k).reverse())
        .ok()
        .map(|i| table[i].1)
}

fn label_count(s: &str) -> usize {
    s.bytes().filter(|&b| b == b'.').count() + 1
}

/// 从最长到最短依次尝试 `host` 的后缀（按 `.` 切）。单个标签（顶级域本身）只在它就是整个输入时尝试，
/// 所以 `evil.com` 不会因为 `com` 而命中。返回命中的后缀所含标签数。
fn longest_suffix(
    table: &[(&str, u16)],
    host: &str,
    allow_single_tail: bool,
) -> Option<(usize, u16)> {
    let mut rest = host;
    loop {
        if let Some(id) = find_host(table, rest) {
            return Some((label_count(rest), id));
        }
        match rest.find('.') {
            Some(p) if allow_single_tail || rest[p + 1..].contains('.') => rest = &rest[p + 1..],
            _ => return None,
        }
    }
}

/// `{}` 能代表的后缀：一个两字母国家/地区代码，或 `com/net/org/edu`；
/// 或者 `com/org/net/co/it/edu` 加一个两字母国家代码（`co.uk`、`com.br`）。
/// 与 Matomo 数据说明里的「combined TLDs」一致，见研究文档 §4.3。
fn is_cc2(s: &str) -> bool {
    s.len() == 2 && s.bytes().all(|b| b.is_ascii_alphabetic())
}

fn is_generic_tld(s: &str) -> bool {
    ["com", "net", "org", "edu"]
        .iter()
        .any(|g| s.eq_ignore_ascii_case(g))
}

fn is_combined_prefix(s: &str) -> bool {
    ["com", "org", "net", "co", "it", "edu"]
        .iter()
        .any(|g| s.eq_ignore_ascii_case(g))
}

fn wildcard_match(host: &str) -> Option<(usize, u16)> {
    let (head, last) = host.rsplit_once('.')?;
    let mut best: Option<(usize, u16)> = None;
    let mut consider = |stem_host: &str, tail_labels: usize| {
        if stem_host.is_empty() {
            return;
        }
        if let Some((n, id)) = longest_suffix(TLD_STEMS, stem_host, true) {
            let covered = n + tail_labels;
            if best.is_none_or(|(b, _)| covered > b) {
                best = Some((covered, id));
            }
        }
    };
    if is_cc2(last) || is_generic_tld(last) {
        consider(head, 1);
    }
    if is_cc2(last)
        && let Some((head2, mid)) = head.rsplit_once('.')
        && is_combined_prefix(mid)
    {
        consider(head2, 2);
    }
    best
}

/// 查 host：精确/后缀匹配与 `{}` 匹配里覆盖标签更多（更具体）的赢，平手取精确匹配。
/// `host` 期望已按 hostname 规则规范化；大小写和结尾的 `.` 这里也容忍。
pub(crate) fn lookup_host(host: &str) -> Option<&'static Entry> {
    let host = host
        .trim_end_matches(|c: char| c == '.' || c.is_whitespace())
        .trim_start();
    if host.is_empty() {
        return None;
    }
    let exact = longest_suffix(HOSTS, host, false);
    let wild = wildcard_match(host);
    let id = match (exact, wild) {
        (Some((e, ei)), Some((w, wi))) => {
            if w > e {
                wi
            } else {
                ei
            }
        }
        (Some((_, id)), None) | (None, Some((_, id))) => id,
        (None, None) => return None,
    };
    Some(&ENTRIES[usize::from(id)])
}

/// 查 utm_source 别名（不区分大小写）。返回条目，以及这个写法本身是否表示「付费」。
pub(crate) fn lookup_alias(token: &str) -> Option<(&'static Entry, bool)> {
    ALIASES
        .binary_search_by(|(k, _, _)| cmp_ci(token, k).reverse())
        .ok()
        .map(|i| (&ENTRIES[usize::from(ALIASES[i].1)], ALIASES[i].2))
}
