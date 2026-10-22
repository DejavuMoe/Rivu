//! 机器人检测率与误判率（外部数据源）。
//!
//! - 检测率：`monperrus/crawler-user-agents` 的 `crawler-user-agents.json`（MIT，固定 commit，
//!   见 `tests/data/PROVENANCE.md`）里每条爬虫的每个 `instances` UA 是否被判为机器人。
//!   注意规则里的名字表由我们自己维护，部分条目是参照这份名单补的，所以这个检测率偏乐观；
//!   同时报告「只用 uap-core Spider」的基线，说明自维护规则带来了多少。
//! - 误判率：uap-core 测试集（test_ua.yaml、test_device.yaml）里被 uap-core 标为非 Spider 的 UA，
//!   被我们判成机器人的比例；逐条列出并给出原因。

use std::collections::BTreeSet;

use rivu_ua::{BotKind, UaParser};
use serde::Deserialize;

const DATA: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/");

#[derive(Deserialize)]
struct Crawler {
    pattern: String,
    instances: Vec<String>,
}

#[derive(Deserialize)]
struct Cases {
    test_cases: Vec<Case>,
}

#[derive(Deserialize)]
struct Case {
    user_agent_string: String,
}

fn crawlers() -> Vec<Crawler> {
    let text = std::fs::read_to_string(format!("{DATA}crawler-user-agents.json")).unwrap();
    serde_json::from_str(&text).unwrap()
}

fn uap_uas(file: &str) -> Vec<String> {
    let text = std::fs::read_to_string(format!("{DATA}{file}")).unwrap();
    serde_saphyr::from_str::<Cases>(&text)
        .unwrap()
        .test_cases
        .into_iter()
        .map(|c| c.user_agent_string)
        .collect()
}

#[test]
fn crawler_detection_rate() {
    let parser = UaParser::embedded().unwrap();
    let list = crawlers();
    let (mut total, mut hit, mut hit_uap_only) = (0usize, 0usize, 0usize);
    let mut by_kind = std::collections::BTreeMap::<&str, usize>::new();
    let mut missed = Vec::new();
    for c in &list {
        for ua in &c.instances {
            total += 1;
            let info = parser.parse_uncached(ua);
            match info.bot {
                Some(k) => {
                    hit += 1;
                    *by_kind.entry(k.as_str()).or_default() += 1;
                }
                None => missed.push((&c.pattern, ua)),
            }
            if parser.parse_raw(ua).device == "Spider" {
                hit_uap_only += 1;
            }
        }
    }
    let rate = hit as f64 / total as f64;
    println!(
        "crawler-user-agents: {} 条规则，{total} 个实例 UA；检测 {hit}（{:.2}%）；仅 uap Spider {hit_uap_only}（{:.2}%）；种类 {by_kind:?}",
        list.len(),
        100.0 * rate,
        100.0 * hit_uap_only as f64 / total as f64
    );
    for (p, ua) in &missed {
        println!("MISS [{p}] {ua}");
    }
    // 下限以实测为准（见 docs/research/enrich-ua.md）：实测 84.58%、uap 基线 62.36%。
    // 余下的是一长串只出现 1–2 次的小众爬虫（SEO 工具、RSS 抓取器……），它们几乎不执行 JS，
    // 不会触发 tracker；要靠名单逐个补，不是通用规则能解决的。
    assert!(rate >= 0.84, "检测率 {rate}");
    assert!(
        hit_uap_only as f64 / total as f64 >= 0.62,
        "uap-core Spider 基线下降"
    );
    assert!(hit > hit_uap_only, "自维护规则没有带来增益");
}

#[test]
fn false_positives_on_uap_corpus() {
    let parser = UaParser::embedded().unwrap();
    let mut uas: BTreeSet<String> = BTreeSet::new();
    uas.extend(uap_uas("uap-test_ua.yaml"));
    uas.extend(uap_uas("uap-test_device.yaml"));
    uas.extend(uap_uas("uap-test_os.yaml"));
    let mut considered = 0;
    let mut browser_shaped = 0;
    let mut flagged = Vec::new();
    for ua in &uas {
        if parser.parse_raw(ua).device == "Spider" {
            continue;
        }
        considered += 1;
        if is_browser_shaped(ua) {
            browser_shaped += 1;
        }
        if let Some(k) = parser.parse_uncached(ua).bot {
            flagged.push((k, ua));
        }
    }
    println!(
        "uap 语料：去重 {} 条，非 Spider {considered} 条，被判机器人 {} 条（{:.3}%）",
        uas.len(),
        flagged.len(),
        100.0 * flagged.len() as f64 / considered as f64
    );
    for (k, ua) in &flagged {
        println!("FP {k:?} {ua}");
    }
    // 浏览器形态的 UA（`Mozilla/` 或 `Opera/` 开头：所有现代浏览器和 WebView）里被判成机器人的，
    // 必须逐条有原因。这里列的全是真机器人：uap-core 没有把它们归为 Spider，
    // 因为它们本来就是「披着浏览器 UA 的自动化 / 合成监控 / 抓站工具」。
    let browser_flagged: Vec<_> = flagged
        .iter()
        .filter(|(_, ua)| is_browser_shaped(ua))
        .collect();
    println!(
        "浏览器形态 UA {browser_shaped} 条，被判机器人 {} 条",
        browser_flagged.len()
    );
    for (k, ua) in &browser_flagged {
        let lower = ua.to_ascii_lowercase();
        let reason = EXPLAINED
            .iter()
            .find(|(token, _)| lower.contains(token))
            .map(|(_, why)| *why);
        assert!(reason.is_some(), "无法解释的误判 {k:?}: {ua}");
    }
    // 非浏览器形态（curl、okhttp、WhatsApp 原生客户端、爬虫……）不是「真实浏览器」，
    // 不会运行 tracker；它们被判为机器人是预期行为，只统计不断言。
    let _ = BotKind::Spider;
}

fn is_browser_shaped(ua: &str) -> bool {
    ua.starts_with("Mozilla/") || ua.starts_with("Opera/")
}

/// 浏览器形态 UA 里被判为机器人的已知原因（小写子串 → 原因）。
const EXPLAINED: &[(&str, &str)] = &[
    ("headlesschrome", "无头 Chrome"),
    ("phantomjs", "PhantomJS 无头浏览器"),
    ("httrack", "HTTrack 整站抓取工具"),
    ("ruxitsynthetic", "Dynatrace 合成监控，会执行 JS"),
    ("catchpoint", "Catchpoint 合成监控，会执行 JS"),
];
