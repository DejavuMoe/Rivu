//! 性质测试：任意输入不 panic、结果确定、几条不变量。

mod common;

use common::In;
use proptest::prelude::*;
use rivu_source::{Channel, classify};

/// 常见的域名片段，让随机 host 有机会走到后缀匹配和 `.{}` 匹配的各个分支。
const PIECES: &[&str] = &[
    "google",
    "com",
    "co",
    "uk",
    "de",
    "jp",
    "www",
    "mail",
    "gemini",
    "youtube",
    "facebook",
    "t",
    "x",
    "bing",
    "yahoo",
    "search",
    "amazon",
    "evil",
    "xyz",
    "localhost",
    "a",
    "",
    "YOUTUBE",
    "Google",
];

fn dotted_host() -> impl Strategy<Value = String> {
    prop::collection::vec(prop::sample::select(PIECES), 0..6).prop_map(|v| v.join("."))
}

const WORDS: &[&str] = &[
    "cpc",
    "ppc",
    "paid",
    "paid-social",
    "organic",
    "social",
    "email",
    "e-mail",
    "display",
    "banner",
    "cpm",
    "video",
    "referral",
    "affiliate",
    "audio",
    "sms",
    "push",
    "mobile",
    "ai-assistant",
    "retargeting",
    "shop",
    "shopping",
    "cross-network",
    "(direct)",
    "(none)",
    "google",
    "facebook",
    "fb-ads",
    "newsletter",
    "firebase",
    "",
];

fn word() -> impl Strategy<Value = String> {
    prop_oneof![
        prop::sample::select(WORDS).prop_map(str::to_owned),
        "[ -~]{0,24}",
        "\\PC{0,16}",
        prop::collection::vec(prop::sample::select(WORDS), 1..3).prop_map(|v| v.join("-")),
    ]
}

fn host_str() -> impl Strategy<Value = String> {
    prop_oneof![dotted_host(), "[ -~]{0,40}", "\\PC{0,24}"]
}

fn click() -> impl Strategy<Value = Option<String>> {
    prop_oneof![
        Just(None),
        prop::sample::select(
            &[
                "gclid",
                "gbraid",
                "wbraid",
                "msclkid",
                "fbclid",
                "twclid",
                "ttclid",
                "li_fat_id",
                "GCLID",
                "other",
                ""
            ][..]
        )
        .prop_map(|s| Some(s.to_owned())),
        "[ -~]{0,12}".prop_map(Some),
    ]
}

#[derive(Debug, Clone)]
struct Raw {
    host: Option<String>,
    src: Option<String>,
    med: Option<String>,
    camp: Option<String>,
    click: Option<String>,
}

fn raw() -> impl Strategy<Value = Raw> {
    (
        prop::option::of(host_str()),
        prop::option::of(word()),
        prop::option::of(word()),
        prop::option::of(word()),
        click(),
    )
        .prop_map(|(host, src, med, camp, click)| Raw {
            host,
            src,
            med,
            camp,
            click,
        })
}

impl Raw {
    fn input(&self) -> In<'_> {
        In {
            host: self.host.as_deref(),
            src: self.src.as_deref(),
            med: self.med.as_deref(),
            camp: self.camp.as_deref(),
            click: self.click.as_deref(),
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 5000, failure_persistence: None, ..ProptestConfig::default() })]

    /// 全函数：任何输入都有结果，不 panic；同样的输入得到同样的结果。
    #[test]
    fn total_and_deterministic(r in raw()) {
        let a = r.input().run();
        let b = r.input().run();
        prop_assert_eq!(&a, &b);
        // 渠道标识总是合法取值，且能往返
        prop_assert_eq!(Channel::parse(a.channel.as_str()), Some(a.channel));
    }

    /// 来源名不含首尾空白。
    #[test]
    fn source_name_shape(r in raw()) {
        let info = r.input().run();
        prop_assert_eq!(info.referrer_source.trim(), info.referrer_source.as_str());
    }

    /// 没有 utm_source、没有 referrer、没有 click id 时，来源恒为空。
    #[test]
    fn no_source_signal_means_empty_source(med in prop::option::of(word()), camp in prop::option::of(word())) {
        let i = In { med: med.as_deref(), camp: camp.as_deref(), ..In::default() };
        prop_assert_eq!(i.source(), "");
    }

    /// ASCII 大小写不影响渠道（GA4：渠道定义不区分大小写）。
    #[test]
    fn channel_is_case_insensitive(r in raw()) {
        let up = |s: &Option<String>| s.as_ref().map(|s| s.to_ascii_uppercase());
        let u = Raw { host: up(&r.host), src: up(&r.src), med: up(&r.med), camp: up(&r.camp), click: r.click.clone() };
        // 未知 utm_source 的名字保留大小写，所以只比渠道
        prop_assert_eq!(r.input().channel(), u.input().channel());
    }

    /// utm 值两侧的空白不影响结果（Utm 里已经 trim，这里再保证库自己也容忍）。
    #[test]
    fn surrounding_whitespace_is_ignored(r in raw()) {
        let pad = |s: &Option<String>| s.as_ref().map(|s| format!("  {s}\t"));
        let p = Raw { host: r.host.clone(), src: pad(&r.src), med: pad(&r.med), camp: pad(&r.camp), click: r.click.clone() };
        prop_assert_eq!(r.input().run(), p.input().run());
    }

    /// 直接调用公开入口（不经测试辅助），Utm 里放任意字符串。
    #[test]
    fn public_entry_point_never_panics(
        host in prop::option::of("\\PC{0,64}"),
        s in prop::option::of("\\PC{0,64}"),
        m in prop::option::of("\\PC{0,64}"),
        c in prop::option::of("\\PC{0,64}"),
        k in prop::option::of("\\PC{0,16}"),
    ) {
        let utm = rivu_domain::Utm { source: s, medium: m, campaign: c, content: None, term: None };
        let info = classify(&rivu_source::SourceInput { referrer_host: host.as_deref(), utm: &utm, click_id_param: k.as_deref() });
        prop_assert!(Channel::ALL.contains(&info.channel));
    }
}
