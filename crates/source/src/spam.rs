//! 垃圾引荐（referrer spam）名单（`system-design.md` §4.2 第 3 步、§4.4）。
//!
//! 数据是 Matomo 的 `referrer-spam-list`（公有领域 / CC0，固定 commit，见 `data/spam/PROVENANCE.md`），
//! 用 `include_str!` 编译进二进制，首次使用时解析成哈希集合（2352 条，微秒级），之后每次查询只做
//! 至多「host 的标签数」次哈希查找，不分配。
//!
//! 匹配规则是 **host 后缀匹配**：referrer 的 host 等于条目，或以 `.<条目>` 结尾才命中。
//! Matomo 自己对名单做子串匹配，这里更严格：`semalt.com` 命中 `semalt.com` 与 `a.semalt.com`，
//! 不命中 `notsemalt.com`。调用方传入的 host 应已按 hostname 规则规范化（小写、无端口、无结尾的点）；
//! 这里仍然容忍大写和结尾的一个点，因为误判比多一次小写化代价大得多。

use std::collections::HashSet;
use std::sync::OnceLock;

/// 名单原文：每行一个主机名，没有空行。
const LIST: &str = include_str!("../data/spam/spammers.txt");

fn set() -> &'static HashSet<Box<str>> {
    static SET: OnceLock<HashSet<Box<str>>> = OnceLock::new();
    SET.get_or_init(|| {
        LIST.lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .map(|l| l.to_ascii_lowercase().into_boxed_str())
            .collect()
    })
}

/// 名单里的条目数（去重、小写之后）。
pub fn entry_count() -> usize {
    set().len()
}

/// 名单里的全部条目（小写，顺序不定）。测试与排错用。
pub fn entries() -> impl Iterator<Item = &'static str> {
    set().iter().map(|e| &**e)
}

/// `host` 是否是已知的垃圾引荐来源（后缀匹配）。空串不命中。
pub fn is_spam_referrer(host: &str) -> bool {
    let host = host.strip_suffix('.').unwrap_or(host);
    if host.is_empty() {
        return false;
    }
    if host.bytes().any(|b| b.is_ascii_uppercase()) {
        return matches_lowercase(&host.to_ascii_lowercase());
    }
    matches_lowercase(host)
}

fn matches_lowercase(host: &str) -> bool {
    let set = set();
    if set.contains(host) {
        return true;
    }
    // 每个 `.` 之后的部分都是一个候选后缀；`host` 自己已经查过。
    host.match_indices('.')
        .any(|(pos, _)| set.contains(&host[pos + 1..]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_is_loaded_lowercased_and_deduplicated() {
        assert_eq!(entry_count(), 2352);
        assert!(entries().all(|e| e == e.to_ascii_lowercase() && !e.is_empty()));
        // 上游唯一一条含大写的条目
        assert!(is_spam_referrer("qiwi.xyz"));
    }

    #[test]
    fn exact_and_subdomain_hosts_match() {
        assert!(is_spam_referrer("semalt.com"));
        assert!(is_spam_referrer("a.semalt.com"));
        assert!(is_spam_referrer("x.y.semalt.com"));
        // 容忍大写与结尾的点
        assert!(is_spam_referrer("SEMALT.com"));
        assert!(is_spam_referrer("semalt.com."));
    }

    #[test]
    fn matching_is_by_label_suffix_not_substring() {
        assert!(!is_spam_referrer("notsemalt.com"));
        assert!(!is_spam_referrer("semalt.com.example.org"));
        assert!(!is_spam_referrer("semalt"));
        assert!(!is_spam_referrer(""));
        assert!(!is_spam_referrer("."));
    }
}
