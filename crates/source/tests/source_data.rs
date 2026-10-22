//! 来源数据一致性：用数据文件自身做全量数据驱动测试。
//!
//! 期望值只来自 `data/` 里的 YAML（Matomo 三份列表 + 补充表），不来自 `build.rs` 生成的表：
//! 对每个条目的每个域名，构造若干应命中它的 host（域名本身、`www.` 前缀、任意子域、大小写变体；
//! 带 `{}` 的域名按几种典型的国家/地区后缀展开），断言 `classify` 返回该条目的名字和类别对应的渠道。
//! 同一域名被多个条目声明时，按固定优先级（补充表 > AI 助手 > 社交 > 搜索）取胜者，
//! 冲突在 [`conflicts_are_exactly_the_reviewed_set`] 里逐条列出并断言，数据升级后出现新冲突会失败，逼人复核。
//!
//! 运行 `cargo test -p rivu-source --test source_data -- --nocapture` 可以看到统计。

mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::PathBuf;

use common::{host, src};
use rivu_source::{Channel, DATA_FINGERPRINT, RULES_VERSION};
use serde::Deserialize;

// ---------- 数据读取 ----------

fn data(rel: &str) -> String {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("data")
        .join(rel);
    fs::read_to_string(&p)
        .unwrap_or_else(|e| panic!("读取 {}: {e}", p.display()))
        .replace("\r\n", "\n")
}

#[derive(Deserialize)]
struct Supplement {
    renames: BTreeMap<String, String>,
    entries: Vec<SupEntry>,
}

#[derive(Deserialize)]
struct SupEntry {
    name: String,
    kind: String,
    #[serde(default)]
    domains: Vec<String>,
    #[serde(default)]
    aliases: Vec<String>,
    #[serde(default)]
    paid_aliases: Vec<String>,
    basis: String,
}

#[derive(Deserialize)]
struct SearchConfig {
    urls: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Kind {
    Search,
    Social,
    Video,
    Shopping,
    Ai,
    Email,
    Other,
}

impl Kind {
    fn parse(s: &str) -> Kind {
        match s {
            "search" => Kind::Search,
            "social" => Kind::Social,
            "video" => Kind::Video,
            "shopping" => Kind::Shopping,
            "ai" => Kind::Ai,
            "email" => Kind::Email,
            "other" => Kind::Other,
            _ => panic!("未知 kind {s}"),
        }
    }

    /// 未带 medium 的单独来源应得到的渠道。
    fn channel(self) -> Channel {
        match self {
            Kind::Search => Channel::OrganicSearch,
            Kind::Social => Channel::OrganicSocial,
            Kind::Video => Channel::OrganicVideo,
            Kind::Shopping => Channel::OrganicShopping,
            Kind::Ai => Channel::AiAssistant,
            Kind::Email => Channel::Email,
            Kind::Other => Channel::Referral,
        }
    }
}

#[derive(Clone, Debug)]
struct Claim {
    /// 0 补充表，1 AI，2 社交，3 搜索。
    rank: u8,
    origin: &'static str,
    name: String,
    kind: Kind,
}

/// 域名 → 键。带路径的返回 `None`；返回 `(键, 是否 .{} 通配)`。规则的定义在研究文档 §4.3。
fn norm(raw: &str) -> Option<(String, bool)> {
    let mut s = raw.trim().to_lowercase();
    if s.is_empty() || s.contains('/') {
        return None;
    }
    let wild = s.ends_with(".{}");
    if wild {
        s.truncate(s.len() - 3);
    }
    if let Some(i) = s.rfind("{}.") {
        s = s[i + 3..].to_owned();
    }
    while let Some(r) = s.strip_prefix("www.") {
        s = r.to_owned();
    }
    (!s.is_empty() && !s.contains("{}")).then_some((s, wild))
}

struct Dataset {
    /// 键 → 声明列表（按登记顺序）。
    claims: BTreeMap<(String, bool), Vec<Claim>>,
    /// 路径条目（被跳过）：`(来源, 名字, 原始写法)`。
    skipped_paths: Vec<(&'static str, String, String)>,
    /// 名字（小写）→ 声明（按登记顺序），用于 utm_source 名字别名。
    names: BTreeMap<String, Vec<Claim>>,
    supplement: Supplement,
    /// Matomo 三份文件里的（名字, 原始域名）总数。
    matomo_pairs: usize,
}

fn load() -> Dataset {
    let supplement: Supplement = serde_saphyr::from_str(&data("supplement.yml")).unwrap();
    let ai: BTreeMap<String, Vec<String>> =
        serde_saphyr::from_str(&data("matomo/AIAssistants.yml")).unwrap();
    let social: BTreeMap<String, Vec<String>> =
        serde_saphyr::from_str(&data("matomo/Socials.yml")).unwrap();
    let search: BTreeMap<String, Vec<SearchConfig>> =
        serde_saphyr::from_str(&data("matomo/SearchEngines.yml")).unwrap();

    let mut ds = Dataset {
        claims: BTreeMap::new(),
        skipped_paths: Vec::new(),
        names: BTreeMap::new(),
        matomo_pairs: 0,
        supplement,
    };
    let rename =
        |n: &str, r: &BTreeMap<String, String>| r.get(n).cloned().unwrap_or_else(|| n.to_owned());

    let push =
        |ds: &mut Dataset, rank: u8, origin: &'static str, name: &str, kind: Kind, raw: &str| {
            let claim = Claim {
                rank,
                origin,
                name: name.to_owned(),
                kind,
            };
            match norm(raw) {
                Some(k) => ds.claims.entry(k).or_default().push(claim),
                None => ds
                    .skipped_paths
                    .push((origin, name.to_owned(), raw.to_owned())),
            }
        };

    for e in &ds
        .supplement
        .entries
        .iter()
        .map(|e| (e.name.clone(), Kind::parse(&e.kind), e.domains.clone()))
        .collect::<Vec<_>>()
    {
        ds.names.entry(e.0.to_lowercase()).or_default().push(Claim {
            rank: 0,
            origin: "supplement",
            name: e.0.clone(),
            kind: e.1,
        });
        for d in &e.2 {
            push(&mut ds, 0, "supplement", &e.0, e.1, d);
        }
    }
    let renames = ds.supplement.renames.clone();
    for (origin, rank, kind, entries) in [
        (
            "AIAssistants",
            1u8,
            Kind::Ai,
            ai.into_iter().collect::<Vec<_>>(),
        ),
        ("Socials", 2, Kind::Social, social.into_iter().collect()),
        (
            "SearchEngines",
            3,
            Kind::Search,
            search
                .into_iter()
                .map(|(n, c)| (n, c.into_iter().flat_map(|c| c.urls).collect()))
                .collect(),
        ),
    ] {
        for (name, urls) in entries {
            let name = rename(&name, &renames);
            ds.names
                .entry(name.to_lowercase())
                .or_default()
                .push(Claim {
                    rank,
                    origin,
                    name: name.clone(),
                    kind,
                });
            for u in urls {
                ds.matomo_pairs += 1;
                push(&mut ds, rank, origin, &name, kind, &u);
            }
        }
    }
    ds
}

/// 一个键的胜者：优先级最小者；同级取先登记的。
fn winner(claims: &[Claim]) -> &Claim {
    claims.iter().min_by_key(|c| c.rank).unwrap()
}

/// 应该命中某个键的 host 样本。
fn samples(key: &str, wild: bool) -> Vec<String> {
    if wild {
        let mut v: Vec<String> = [".com", ".de", ".co.uk", ".com.br", ".co.jp", ".fr", ".it"]
            .iter()
            .map(|t| format!("{key}{t}"))
            .collect();
        v.push(format!("www.{key}.com"));
        v.push(format!("{key}.COM"));
        v
    } else {
        vec![
            key.to_owned(),
            format!("www.{key}"),
            format!("rivu-test.{key}"),
            key.to_ascii_uppercase(),
            format!("{key}."),
        ]
    }
}

// ---------- 版本钉 ----------

/// 规则或数据变了就要加 `RULES_VERSION`：这里把版本和数据指纹钉在一起。
/// 改了 `data/` 下任何文件，指纹会变，本测试失败；加版本、更新这两个常量、再改研究文档。
const PINNED_RULES_VERSION: u16 = 1;
const PINNED_DATA_FINGERPRINT: &str =
    "c79e78035ea9db13a4443cab192a4666b769a6148e7b0faba51c8a54c3d9e1d9";

#[test]
fn rules_version_is_pinned_to_the_data_fingerprint() {
    assert_eq!(
        (RULES_VERSION, DATA_FINGERPRINT),
        (PINNED_RULES_VERSION, PINNED_DATA_FINGERPRINT),
        "data/ 变了：RULES_VERSION 加一，并更新本文件的钉值和 docs/research/enrich-source.md"
    );
}

// ---------- 全量数据驱动 ----------

#[test]
fn every_domain_of_every_entry_resolves_to_its_winning_name_and_channel() {
    let ds = load();
    let mut checked = 0usize;
    let mut hosts_checked = 0usize;
    let mut mismatches = Vec::new();
    for ((key, wild), claims) in &ds.claims {
        let w = winner(claims);
        checked += 1;
        for h in samples(key, *wild) {
            hosts_checked += 1;
            let info = host(&h).run();
            // 更具体的键（如 gemini.google.com 之于 google.com）可能让样本合法地落到别的条目，
            // 只有「样本自己的最长命中键就是 key」时才断言；这里用独立判断：没有更长的键是样本的后缀。
            let hijacked = ds.claims.keys().any(|(k2, w2)| {
                !*w2 && (k2 != key || *w2 != *wild) && k2.len() > key.len() && {
                    let lower = h.trim_end_matches('.').to_lowercase();
                    lower == *k2 || lower.ends_with(&format!(".{k2}"))
                }
            });
            if hijacked {
                continue;
            }
            if info.referrer_source != w.name || info.channel != w.kind.channel() {
                mismatches.push(format!(
                    "{h} -> ({}, {}) 期望 ({}, {}) [{} / {}]",
                    info.referrer_source,
                    info.channel,
                    w.name,
                    w.kind.channel(),
                    w.origin,
                    key
                ));
            }
        }
    }
    println!(
        "数据驱动：{checked} 个域名键，{hosts_checked} 个 host 样本，{} 个不一致",
        mismatches.len()
    );
    assert!(mismatches.is_empty(), "不一致：\n{}", mismatches.join("\n"));
}

#[test]
fn coverage_statistics() {
    let ds = load();
    let total_pairs = ds.matomo_pairs;
    let skipped_matomo = ds
        .skipped_paths
        .iter()
        .filter(|(o, _, _)| *o != "supplement")
        .count();
    let keys = ds.claims.len();
    let wild = ds.claims.keys().filter(|(_, w)| *w).count();
    println!("Matomo 三份文件的（名字, 域名）对：{total_pairs}");
    println!("  带路径被跳过：{skipped_matomo}（分类接口只有 host）");
    for (o, n, raw) in ds
        .skipped_paths
        .iter()
        .filter(|(o, _, _)| *o != "supplement")
    {
        println!("    [{o}] {n}: {raw}");
    }
    println!("  合并后的域名键：{keys}（其中 .{{}} 通配 {wild}）");
    let covered = total_pairs - skipped_matomo;
    println!(
        "  参与匹配的（名字, 域名）对：{covered}，覆盖率 {:.1}%",
        100.0 * covered as f64 / total_pairs as f64
    );
    assert!(skipped_matomo <= 20, "被跳过的路径条目异常增多，请复核");
    assert!(covered * 100 >= total_pairs * 97);
}

// ---------- 冲突 ----------

/// 一个键被多个不同（名字, 类别）声明。
fn conflicts(ds: &Dataset) -> Vec<String> {
    let mut out = Vec::new();
    for ((key, wild), claims) in &ds.claims {
        let distinct: BTreeSet<(String, Kind)> =
            claims.iter().map(|c| (c.name.clone(), c.kind)).collect();
        if distinct.len() > 1 {
            let w = winner(claims);
            let losers: Vec<String> = claims
                .iter()
                .filter(|c| (c.name.as_str(), c.kind) != (w.name.as_str(), w.kind))
                .map(|c| format!("{}:{}({:?})", c.origin, c.name, c.kind))
                .collect();
            out.push(format!(
                "{}{} 胜者 {}:{}({:?}) 败者 {}",
                key,
                if *wild { ".{}" } else { "" },
                w.origin,
                w.name,
                w.kind,
                losers.join(", ")
            ));
        }
    }
    out
}

#[test]
fn conflicts_are_exactly_the_reviewed_set() {
    let ds = load();
    let found = conflicts(&ds);
    println!("同一域名两个名字或类别的冲突：{} 条", found.len());
    for c in &found {
        println!("  {c}");
    }
    // 复核结论（研究文档 §4.4）：
    // - AI 助手赢过搜索：ChatGPT、Perplexity、You 在 Matomo 里同时是「搜索引擎」和「AI 助手」，
    //   Rivu 的渠道里有独立的 AI Assistant，AI 的判断更具体；
    // - 社交赢过搜索：Matomo 的搜索列表里有 `www.facebook.com`（站内搜索），整站应归社交；
    // - 补充表赢过 Matomo 社交：YouTube、Vimeo、Niconico 在 GA4 清单里是视频，Matomo 归社交；
    // - 补充表赢过 Matomo 搜索：Digg、FriendFeed 在 GA4 清单里是社交，Matomo 当成搜索引擎（它们有站内搜索）。
    let expected: &[&str] = &[
        "chat.openai.com 胜者 AIAssistants:ChatGPT(Ai) 败者 SearchEngines:ChatGPT(Search)",
        "chatgpt.com 胜者 AIAssistants:ChatGPT(Ai) 败者 SearchEngines:ChatGPT(Search)",
        "digg.com 胜者 supplement:Digg(Social) 败者 SearchEngines:Digg(Search)",
        "facebook.com 胜者 Socials:Facebook(Social) 败者 SearchEngines:Facebook(Search)",
        "friendfeed.com 胜者 supplement:FriendFeed(Social) 败者 SearchEngines:FriendFeed(Search)",
        "nicovideo.jp 胜者 supplement:Niconico(Video) 败者 Socials:Niconico(Social)",
        "perplexity.ai 胜者 AIAssistants:Perplexity(Ai) 败者 SearchEngines:Perplexity(Search)",
        "vimeo.com 胜者 supplement:Vimeo(Video) 败者 Socials:Vimeo(Social)",
        "you.com 胜者 AIAssistants:You(Ai) 败者 SearchEngines:You(Search)",
        "youtu.be 胜者 supplement:YouTube(Video) 败者 Socials:YouTube(Social)",
        "youtube.com 胜者 supplement:YouTube(Video) 败者 Socials:YouTube(Social)",
    ];
    assert_eq!(
        found.iter().map(String::as_str).collect::<Vec<_>>(),
        expected,
        "冲突集合变化：复核后更新 expected 和研究文档 §4.4"
    );
}

// ---------- 别名 ----------

#[test]
fn every_entry_name_and_alias_resolves_as_utm_source() {
    let ds = load();
    // 名字：不区分大小写，解析成胜者的展示名
    for (lower, claims) in &ds.names {
        let w = winner(claims);
        for variant in [w.name.clone(), lower.clone(), w.name.to_ascii_uppercase()] {
            let info = src(&variant).run();
            assert_eq!(info.referrer_source, w.name, "utm_source={variant}");
            assert_eq!(info.channel, w.kind.channel(), "utm_source={variant}");
        }
    }
    // 补充表别名
    for e in &ds.supplement.entries {
        let kind = Kind::parse(&e.kind);
        for a in e.aliases.iter().chain(&e.paid_aliases) {
            let info = src(a).run();
            let claims = ds.names.get(&a.to_lowercase());
            // 别名与某个名字重名时，先登记的赢（补充表在前）
            let expect_name = claims
                .map(|c| winner(c).name.clone())
                .unwrap_or_else(|| e.name.clone());
            assert_eq!(info.referrer_source, expect_name, "alias={a}");
            let _ = kind;
        }
    }
}

#[test]
fn paid_aliases_are_paid_evidence_for_their_kind() {
    let ds = load();
    let mut n = 0;
    for e in &ds.supplement.entries {
        for a in &e.paid_aliases {
            let want = match Kind::parse(&e.kind) {
                Kind::Search => Channel::PaidSearch,
                Kind::Social => Channel::PaidSocial,
                Kind::Video => Channel::PaidVideo,
                Kind::Shopping => Channel::PaidShopping,
                k => panic!("{}：{:?} 类别不应有 paid_aliases", e.name, k),
            };
            assert_eq!(src(a).channel(), want, "paid alias {a}");
            n += 1;
        }
    }
    println!("付费别名：{n} 个");
    assert!(n >= 20);
}

#[test]
fn supplement_is_well_formed() {
    let ds = load();
    let mut seen = BTreeSet::new();
    for e in &ds.supplement.entries {
        assert!(!e.basis.trim().is_empty(), "{} 缺少依据", e.name);
        assert!(
            seen.insert((e.name.clone(), e.kind.clone())),
            "补充表里重复的条目 {}",
            e.name
        );
        for d in e.domains.iter().chain(&e.aliases).chain(&e.paid_aliases) {
            assert_eq!(d, d.trim(), "{}: {d:?} 有多余空白", e.name);
            assert!(!d.is_empty());
        }
        for a in e.aliases.iter().chain(&e.paid_aliases) {
            assert_eq!(a, &a.to_lowercase(), "{}: 别名 {a} 必须小写", e.name);
        }
        for d in &e.domains {
            assert!(!d.contains('/'), "{}: 域名 {d} 不能带路径", e.name);
            assert_eq!(d, &d.to_lowercase(), "{}: 域名 {d} 必须小写", e.name);
        }
    }
    println!(
        "补充表：{} 条，改名 {} 条",
        ds.supplement.entries.len(),
        ds.supplement.renames.len()
    );
}

// ---------- 与 Google 官方来源清单对照（需要本地文件，默认忽略） ----------

/// 读取 `RIVU_GA4_SOURCES_TSV` 指向的文件（每行 `source<TAB>SOURCE_CATEGORY_X`，来自 GA4 帮助页的
/// 「Download a spreadsheet of GA4 default-channel-group sources and categories」），
/// 逐条用 `utm_source=<source>` 和（带点号时）`referrer host=<source>` 两种方式分类，统计与 Google 类别的一致率。
/// 清单是 Google 的内容，没有明确许可，不入库；获取方法见研究文档 §2.2。
///
/// 运行：`RIVU_GA4_SOURCES_TSV=<path> cargo test -p rivu-source --test source_data ga4_list -- --ignored --nocapture`
#[test]
#[ignore = "需要 RIVU_GA4_SOURCES_TSV 指向本地的 Google 来源清单"]
fn ga4_list_agreement() {
    let path = std::env::var("RIVU_GA4_SOURCES_TSV").expect("设置 RIVU_GA4_SOURCES_TSV");
    let text = fs::read_to_string(path).unwrap();
    let mut by_cat: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    let mut misses: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let (s, cat) = line.split_once('\t').expect("tab 分隔");
        let want = match cat.trim() {
            "SOURCE_CATEGORY_SEARCH" => Channel::OrganicSearch,
            "SOURCE_CATEGORY_SOCIAL" => Channel::OrganicSocial,
            "SOURCE_CATEGORY_VIDEO" => Channel::OrganicVideo,
            "SOURCE_CATEGORY_SHOPPING" => Channel::OrganicShopping,
            other => panic!("未知类别 {other}"),
        };
        let s = s.trim();
        // 带点号的写法同时是 host；不带点号的只可能是 utm_source 名字。
        let got = if s.contains('.') {
            host(s).channel()
        } else {
            src(s).channel()
        };
        let e = by_cat.entry(cat.trim().to_owned()).or_default();
        e.0 += 1;
        if got == want {
            e.1 += 1;
        } else {
            misses
                .entry(cat.trim().to_owned())
                .or_default()
                .push(format!("{s} -> {got}"));
        }
    }
    let (mut total, mut agree) = (0, 0);
    for (cat, (n, ok)) in &by_cat {
        println!("{cat}: {ok}/{n} = {:.1}%", 100.0 * *ok as f64 / *n as f64);
        total += n;
        agree += ok;
    }
    println!(
        "合计：{agree}/{total} = {:.1}%",
        100.0 * agree as f64 / total as f64
    );
    for (cat, v) in &misses {
        println!("--- {cat} 不一致 {} 条", v.len());
        for m in v {
            println!("  {m}");
        }
    }
}
