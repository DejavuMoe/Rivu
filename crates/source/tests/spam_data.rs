//! 垃圾引荐名单的数据驱动测试：用名单文件自身做全量测试，并证明合法来源不被误判。
//!
//! - 名单文件的 SHA-256 与固定值一致（`build.rs` 构建时也核对，这里再用独立实现算一遍）；
//! - 名单里的每个条目都命中：条目本身、大写、随机子域、结尾带点；
//! - 命中判定与一个独立的朴素实现（逐条 `==` / `ends_with`）在大量探针上逐一相同，
//!   包括「前缀拼接但不在标签边界上」的探针（`not` + 条目）；
//! - 常见的合法来源（搜索引擎、社交、站内导航常见的 referrer）不被误判；来源库（Matomo 搜索引擎、
//!   社交、AI 助手，加补充表）里的每一个域名也都不被误判，唯一例外是两个已复核的浏览器劫持类域名。
//!
//! `cargo test -p rivu-source --test spam_data -- --nocapture` 可以看到统计。

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::PathBuf;

use rivu_source::spam::{entries, entry_count, is_spam_referrer};
use serde::Deserialize;

fn data(rel: &str) -> String {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("data")
        .join(rel);
    fs::read_to_string(&p)
        .unwrap_or_else(|e| panic!("读取 {}: {e}", p.display()))
        .replace("\r\n", "\n")
}

/// 文件里的条目（小写、去重），与实现无关地重新读一遍。
fn file_entries() -> BTreeSet<String> {
    data("spam/spammers.txt")
        .lines()
        .map(|l| l.trim().to_ascii_lowercase())
        .filter(|l| !l.is_empty())
        .collect()
}

/// 独立的朴素参考：host 等于某条目，或以 `.<条目>` 结尾。
fn naive(list: &BTreeSet<String>, host: &str) -> bool {
    let host = host.to_ascii_lowercase();
    let host = host.strip_suffix('.').unwrap_or(&host);
    !host.is_empty()
        && list
            .iter()
            .any(|e| host == e || host.ends_with(&format!(".{e}")))
}

#[test]
fn file_matches_the_pinned_provenance() {
    use sha2::{Digest, Sha256};
    let text = data("spam/spammers.txt");
    let digest = Sha256::digest(text.as_bytes());
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    assert_eq!(
        hex,
        "640a63e8324b441c5f22240308e3925cea57464ffaafe6174cfd46c3925c6751"
    );
    assert_eq!(text.len(), 37727);
    let provenance = data("spam/PROVENANCE.md");
    assert!(provenance.contains(&hex), "PROVENANCE.md 里要写明哈希");
    assert!(provenance.contains("eaaa5eaa6990be274fb560488a155459f735318d"));
}

#[test]
fn the_embedded_list_equals_the_file() {
    let from_file = file_entries();
    assert_eq!(entry_count(), from_file.len());
    assert_eq!(entry_count(), 2352);
    let embedded: BTreeSet<String> = entries().map(str::to_owned).collect();
    assert_eq!(embedded, from_file);
}

#[test]
fn every_entry_matches_in_every_form() {
    let list = file_entries();
    for e in &list {
        assert!(is_spam_referrer(e), "条目本身：{e}");
        assert!(is_spam_referrer(&e.to_ascii_uppercase()), "大写：{e}");
        assert!(is_spam_referrer(&format!("www.{e}")), "www：{e}");
        assert!(is_spam_referrer(&format!("a.b.{e}")), "多级子域：{e}");
        assert!(is_spam_referrer(&format!("{e}.")), "结尾的点：{e}");
    }
    println!("名单条目全部命中：{} 条 × 5 种写法", list.len());
}

#[test]
fn matching_agrees_with_a_naive_reference_on_probes() {
    let list = file_entries();
    let mut probes = Vec::new();
    for e in &list {
        // 在标签边界上拼接（应命中）与不在边界上拼接（除非恰好是另一个条目的后缀，否则不命中）
        probes.push(format!("not{e}"));
        probes.push(format!("{e}.example.org"));
        probes.push(format!("x-{e}"));
        // 去掉第一个标签：父域，除非父域本身也被列出，否则不命中
        if let Some((_, parent)) = e.split_once('.') {
            probes.push(parent.to_owned());
        }
    }
    let mut hits = 0;
    for p in &probes {
        let want = naive(&list, p);
        assert_eq!(is_spam_referrer(p), want, "探针 {p}");
        hits += usize::from(want);
    }
    println!(
        "探针 {} 个，命中 {hits} 个，与朴素实现逐一一致",
        probes.len()
    );
}

/// 常见合法来源：搜索引擎、社交、开发者、新闻聚合、云厂商等。任何一个被误判都是严重问题。
const LEGIT: &[&str] = &[
    "google.com",
    "www.google.com",
    "www.google.co.jp",
    "google.de",
    "bing.com",
    "www.bing.com",
    "duckduckgo.com",
    "yahoo.com",
    "search.yahoo.com",
    "baidu.com",
    "yandex.ru",
    "yandex.com",
    "ecosia.org",
    "brave.com",
    "search.brave.com",
    "kagi.com",
    "t.co",
    "twitter.com",
    "x.com",
    "facebook.com",
    "l.facebook.com",
    "lm.facebook.com",
    "instagram.com",
    "l.instagram.com",
    "linkedin.com",
    "lnkd.in",
    "reddit.com",
    "old.reddit.com",
    "news.ycombinator.com",
    "github.com",
    "gitlab.com",
    "stackoverflow.com",
    "youtube.com",
    "m.youtube.com",
    "pinterest.com",
    "tumblr.com",
    "medium.com",
    "wikipedia.org",
    "en.wikipedia.org",
    "zh.wikipedia.org",
    "weibo.com",
    "zhihu.com",
    "mail.google.com",
    "outlook.live.com",
    "chat.openai.com",
    "chatgpt.com",
    "perplexity.ai",
    "claude.ai",
    "blogspot.com",
    "wordpress.com",
    "substack.com",
    "producthunt.com",
    "amazon.com",
    "apple.com",
    "microsoft.com",
    "cloudflare.com",
    "localhost",
    "example.com",
];

#[test]
fn common_legitimate_sources_are_not_flagged() {
    for h in LEGIT {
        assert!(!is_spam_referrer(h), "合法来源被误判为垃圾引荐：{h}");
    }
}

#[derive(Deserialize)]
struct Supplement {
    entries: Vec<SupEntry>,
}

#[derive(Deserialize)]
struct SupEntry {
    #[serde(default)]
    domains: Vec<String>,
}

#[derive(Deserialize)]
struct SearchConfig {
    urls: Vec<String>,
}

/// 把来源库里的域名写法展开成几个具体的 host：`{}` 是国家后缀或语言前缀占位符；路径只保留 host。
fn expand(raw: &str) -> Vec<String> {
    let host = raw.trim().split('/').next().unwrap_or("").to_lowercase();
    if host.is_empty() {
        return Vec::new();
    }
    if host.contains("{}") {
        ["com", "co.uk", "de", "en"]
            .iter()
            .map(|x| host.replace("{}", x))
            .collect()
    } else {
        vec![host]
    }
}

#[test]
fn every_domain_in_the_source_database_is_not_flagged() {
    let mut domains: BTreeMap<String, &'static str> = BTreeMap::new();
    let ai: BTreeMap<String, Vec<String>> =
        serde_saphyr::from_str(&data("matomo/AIAssistants.yml")).unwrap();
    let social: BTreeMap<String, Vec<String>> =
        serde_saphyr::from_str(&data("matomo/Socials.yml")).unwrap();
    let search: BTreeMap<String, Vec<SearchConfig>> =
        serde_saphyr::from_str(&data("matomo/SearchEngines.yml")).unwrap();
    let supplement: Supplement = serde_saphyr::from_str(&data("supplement.yml")).unwrap();
    for d in ai.values().flatten() {
        for h in expand(d) {
            domains.insert(h, "AI 助手");
        }
    }
    for d in social.values().flatten() {
        for h in expand(d) {
            domains.insert(h, "社交");
        }
    }
    for cfgs in search.values() {
        for c in cfgs {
            for d in &c.urls {
                for h in expand(d) {
                    domains.insert(h, "搜索引擎");
                }
            }
        }
    }
    for e in &supplement.entries {
        for d in &e.domains {
            for h in expand(d) {
                domains.insert(h, "补充表");
            }
        }
    }
    assert!(
        domains.len() > 1000,
        "来源库展开后应有上千个 host：{}",
        domains.len()
    );
    let flagged: Vec<_> = domains
        .iter()
        .filter(|(h, _)| is_spam_referrer(h))
        .collect();
    println!(
        "来源库展开后 {} 个 host，被误判 {} 个：{flagged:?}",
        domains.len(),
        flagged.len()
    );
    // 已复核的重叠：两个浏览器劫持类「搜索引擎」（Matomo 的搜索引擎表登记了它们，
    // 但 Matomo 自己的垃圾名单也列了它们的根域）。接入管线里垃圾引荐判定先于来源分类，所以它们的流量
    // 被丢弃，不会出现在来源榜里。出现新的重叠必须人工复核后再加进这里。
    const REVIEWED_OVERLAP: [&str; 2] = ["search.fbdownloader.com", "search.iminent.com"];
    let flagged: Vec<&str> = flagged.iter().map(|(h, _)| h.as_str()).collect();
    assert_eq!(
        flagged, REVIEWED_OVERLAP,
        "来源库里的域名与垃圾引荐名单的重叠必须逐条复核"
    );
}
