//! 与 Plausible 的行为对照。
//!
//! 用例来自 Plausible 仓库的 `test/plausible/ingestion/acquisition_test.exs`（静态用例表）
//! 和 `external_controller_test.exs`（来源解析），只抄「输入 → 期望」的行为，不抄实现。
//! Plausible 的用例直接给出已解析的 `referrer_source` 名；Rivu 的输入是 referrer host 和 utm，
//! 所以这里把名字换回它的典型 host / utm_source。
//!
//! 每一行记 Plausible 的期望和 Rivu 的期望；两者不同的行就是「刻意偏离」，
//! 必须在 `docs/research/enrich-source.md` §7 的偏离清单里有对应条目（用例名里的 `D<n>`）。

mod common;

use common::In;

struct Row {
    label: &'static str,
    input: In<'static>,
    plausible: &'static str,
    rivu: &'static str,
    /// 偏离清单里的编号；`None` 表示两者一致。
    deviation: Option<&'static str>,
}

fn row(label: &'static str, input: In<'static>, expected: &'static str) -> Row {
    Row {
        label,
        input,
        plausible: expected,
        rivu: expected,
        deviation: None,
    }
}

fn dev(
    label: &'static str,
    input: In<'static>,
    plausible: &'static str,
    rivu: &'static str,
    d: &'static str,
) -> Row {
    Row {
        label,
        input,
        plausible,
        rivu,
        deviation: Some(d),
    }
}

fn i() -> In<'static> {
    In::default()
}

fn rows() -> Vec<Row> {
    vec![
        // ---- acquisition_test.exs 的静态用例（按原顺序） ----
        row("direct", i(), "Direct"),
        row(
            "cross-network",
            In {
                camp: Some("cross-network"),
                ..i()
            },
            "Cross-network",
        ),
        row(
            "shopping campaign + paid",
            In {
                camp: Some("shopping"),
                med: Some("paid"),
                ..i()
            },
            "Paid Shopping",
        ),
        row(
            "shopify.com + paid",
            In {
                host: Some("shopify.com"),
                med: Some("paid"),
                ..i()
            },
            "Paid Shopping",
        ),
        row(
            "shopify utm + paid",
            In {
                src: Some("shopify"),
                med: Some("paid"),
                ..i()
            },
            "Paid Shopping",
        ),
        row(
            "duckduckgo + paid",
            In {
                host: Some("duckduckgo.com"),
                med: Some("paid"),
                ..i()
            },
            "Paid Search",
        ),
        row(
            "google + gclid",
            In {
                host: Some("google.com"),
                click: Some("gclid"),
                ..i()
            },
            "Paid Search",
        ),
        row(
            "duckduckgo + gclid",
            In {
                host: Some("duckduckgo.com"),
                click: Some("gclid"),
                ..i()
            },
            "Organic Search",
        ),
        row(
            "bing + msclkid",
            In {
                host: Some("bing.com"),
                click: Some("msclkid"),
                ..i()
            },
            "Paid Search",
        ),
        row(
            "duckduckgo + msclkid",
            In {
                host: Some("duckduckgo.com"),
                click: Some("msclkid"),
                ..i()
            },
            "Organic Search",
        ),
        row(
            "google utm + paid",
            In {
                src: Some("google"),
                med: Some("paid"),
                ..i()
            },
            "Paid Search",
        ),
        row(
            "tiktok + paid",
            In {
                host: Some("tiktok.com"),
                med: Some("paid"),
                ..i()
            },
            "Paid Social",
        ),
        row(
            "tiktok utm + paid",
            In {
                src: Some("tiktok"),
                med: Some("paid"),
                ..i()
            },
            "Paid Social",
        ),
        row(
            "youtube + paid",
            In {
                host: Some("youtube.com"),
                med: Some("paid"),
                ..i()
            },
            "Paid Video",
        ),
        row(
            "youtube utm + paid",
            In {
                src: Some("youtube"),
                med: Some("paid"),
                ..i()
            },
            "Paid Video",
        ),
        row(
            "banner",
            In {
                med: Some("banner"),
                ..i()
            },
            "Display",
        ),
        row(
            "cpc",
            In {
                med: Some("cpc"),
                ..i()
            },
            "Paid Other",
        ),
        row(
            "walmart.com",
            In {
                host: Some("walmart.com"),
                ..i()
            },
            "Organic Shopping",
        ),
        row(
            "walmart utm",
            In {
                src: Some("walmart"),
                ..i()
            },
            "Organic Shopping",
        ),
        row(
            "shop campaign",
            In {
                camp: Some("shop"),
                ..i()
            },
            "Organic Shopping",
        ),
        row(
            "facebook",
            In {
                host: Some("facebook.com"),
                ..i()
            },
            "Organic Social",
        ),
        row(
            "twitter utm",
            In {
                src: Some("twitter"),
                ..i()
            },
            "Organic Social",
        ),
        row(
            "social medium",
            In {
                med: Some("social"),
                ..i()
            },
            "Organic Social",
        ),
        row(
            "vimeo",
            In {
                host: Some("vimeo.com"),
                ..i()
            },
            "Organic Video",
        ),
        row(
            "vimeo utm",
            In {
                src: Some("vimeo"),
                ..i()
            },
            "Organic Video",
        ),
        row(
            "video medium",
            In {
                med: Some("video"),
                ..i()
            },
            "Organic Video",
        ),
        row(
            "duckduckgo",
            In {
                host: Some("duckduckgo.com"),
                ..i()
            },
            "Organic Search",
        ),
        row(
            "duckduckgo utm",
            In {
                src: Some("duckduckgo"),
                ..i()
            },
            "Organic Search",
        ),
        row(
            "referral medium",
            In {
                med: Some("referral"),
                ..i()
            },
            "Referral",
        ),
        row(
            "email utm",
            In {
                src: Some("email"),
                ..i()
            },
            "Email",
        ),
        row(
            "email medium",
            In {
                med: Some("email"),
                ..i()
            },
            "Email",
        ),
        row(
            "affiliate",
            In {
                med: Some("affiliate"),
                ..i()
            },
            "Affiliates",
        ),
        row(
            "audio",
            In {
                med: Some("audio"),
                ..i()
            },
            "Audio",
        ),
        row(
            "sms utm",
            In {
                src: Some("sms"),
                ..i()
            },
            "SMS",
        ),
        row(
            "sms medium",
            In {
                med: Some("sms"),
                ..i()
            },
            "SMS",
        ),
        row(
            "app-push",
            In {
                med: Some("app-push"),
                ..i()
            },
            "Mobile Push Notifications",
        ),
        row(
            "example-mobile",
            In {
                med: Some("example-mobile"),
                ..i()
            },
            "Mobile Push Notifications",
        ),
        row(
            "othersite.com",
            In {
                host: Some("othersite.com"),
                ..i()
            },
            "Referral",
        ),
        row(
            "threads utm",
            In {
                src: Some("threads"),
                ..i()
            },
            "Organic Social",
        ),
        row(
            "ig utm",
            In {
                src: Some("ig"),
                ..i()
            },
            "Organic Social",
        ),
        row(
            "yt utm",
            In {
                src: Some("yt"),
                ..i()
            },
            "Organic Video",
        ),
        row(
            "yt-ads utm",
            In {
                src: Some("yt-ads"),
                ..i()
            },
            "Paid Video",
        ),
        row(
            "fb utm",
            In {
                src: Some("fb"),
                ..i()
            },
            "Organic Social",
        ),
        row(
            "fb-ads utm",
            In {
                src: Some("fb-ads"),
                ..i()
            },
            "Paid Social",
        ),
        row(
            "fbad utm",
            In {
                src: Some("fbad"),
                ..i()
            },
            "Paid Social",
        ),
        row(
            "facebook-ads utm",
            In {
                src: Some("facebook-ads"),
                ..i()
            },
            "Paid Social",
        ),
        row(
            "reddit-ads utm",
            In {
                src: Some("Reddit-ads"),
                ..i()
            },
            "Paid Social",
        ),
        row(
            "google_ads utm",
            In {
                src: Some("google_ads"),
                ..i()
            },
            "Paid Search",
        ),
        row(
            "Google-ads utm",
            In {
                src: Some("Google-ads"),
                ..i()
            },
            "Paid Search",
        ),
        row(
            "Adwords utm",
            In {
                src: Some("Adwords"),
                ..i()
            },
            "Paid Search",
        ),
        row(
            "twitter-ads utm",
            In {
                src: Some("twitter-ads"),
                ..i()
            },
            "Paid Social",
        ),
        row(
            "reddit",
            In {
                host: Some("reddit.com"),
                ..i()
            },
            "Organic Social",
        ),
        dev(
            "perplexity",
            In {
                host: Some("perplexity.ai"),
                ..i()
            },
            "AI Assistants",
            "AI Assistant",
            "D1",
        ),
        row(
            "teams",
            In {
                host: Some("teams.microsoft.com"),
                ..i()
            },
            "Organic Social",
        ),
        row(
            "wikipedia",
            In {
                host: Some("en.wikipedia.org"),
                ..i()
            },
            "Referral",
        ),
        row(
            "bing",
            In {
                host: Some("bing.com"),
                ..i()
            },
            "Organic Search",
        ),
        row(
            "brave",
            In {
                host: Some("search.brave.com"),
                ..i()
            },
            "Organic Search",
        ),
        row(
            "yandex",
            In {
                host: Some("yandex.ru"),
                ..i()
            },
            "Organic Search",
        ),
        row(
            "discord",
            In {
                host: Some("discord.com"),
                ..i()
            },
            "Organic Social",
        ),
        row(
            "baidu",
            In {
                host: Some("baidu.com"),
                ..i()
            },
            "Organic Search",
        ),
        row(
            "telegram",
            In {
                host: Some("t.me"),
                ..i()
            },
            "Organic Social",
        ),
        row(
            "sogou",
            In {
                host: Some("sogou.com"),
                ..i()
            },
            "Organic Search",
        ),
        row(
            "linktree",
            In {
                host: Some("linktr.ee"),
                ..i()
            },
            "Referral",
        ),
        row(
            "hacker news",
            In {
                host: Some("news.ycombinator.com"),
                ..i()
            },
            "Organic Social",
        ),
        row(
            "yahoo",
            In {
                host: Some("search.yahoo.com"),
                ..i()
            },
            "Organic Search",
        ),
        row(
            "gmail",
            In {
                host: Some("mail.google.com"),
                ..i()
            },
            "Email",
        ),
        row(
            "newsletter-uk utm",
            In {
                src: Some("Newsletter-UK"),
                ..i()
            },
            "Email",
        ),
        row(
            "temu.com",
            In {
                host: Some("temu.com"),
                ..i()
            },
            "Organic Shopping",
        ),
        row(
            "bluesky",
            In {
                host: Some("bsky.app"),
                ..i()
            },
            "Organic Social",
        ),
        row(
            "mastodon",
            In {
                host: Some("mastodon.social"),
                ..i()
            },
            "Organic Social",
        ),
        dev(
            "gemini",
            In {
                host: Some("gemini.google.com"),
                ..i()
            },
            "AI Assistants",
            "AI Assistant",
            "D1",
        ),
        dev(
            "chatgpt",
            In {
                host: Some("chatgpt.com"),
                ..i()
            },
            "AI Assistants",
            "AI Assistant",
            "D1",
        ),
        dev(
            "claude",
            In {
                host: Some("claude.ai"),
                ..i()
            },
            "AI Assistants",
            "AI Assistant",
            "D1",
        ),
        dev(
            "phind",
            In {
                host: Some("phind.com"),
                ..i()
            },
            "AI Assistants",
            "AI Assistant",
            "D1",
        ),
        dev(
            "deepseek",
            In {
                host: Some("chat.deepseek.com"),
                ..i()
            },
            "AI Assistants",
            "AI Assistant",
            "D1",
        ),
        dev(
            "copilot",
            In {
                host: Some("copilot.microsoft.com"),
                ..i()
            },
            "AI Assistants",
            "AI Assistant",
            "D1",
        ),
        dev(
            "grok",
            In {
                host: Some("grok.com"),
                ..i()
            },
            "AI Assistants",
            "AI Assistant",
            "D1",
        ),
        row(
            "slack",
            In {
                host: Some("slack.com"),
                ..i()
            },
            "Organic Social",
        ),
        row(
            "producthunt",
            In {
                host: Some("producthunt.com"),
                ..i()
            },
            "Organic Social",
        ),
        row(
            "github",
            In {
                host: Some("github.com"),
                ..i()
            },
            "Organic Social",
        ),
        row(
            "steamcommunity",
            In {
                host: Some("steamcommunity.com"),
                ..i()
            },
            "Organic Social",
        ),
        row(
            "vkontakte",
            In {
                host: Some("vk.com"),
                ..i()
            },
            "Organic Social",
        ),
        row(
            "threads",
            In {
                host: Some("l.threads.com"),
                ..i()
            },
            "Organic Social",
        ),
        row(
            "ecosia",
            In {
                host: Some("ecosia.org"),
                ..i()
            },
            "Organic Search",
        ),
        row(
            "kagi",
            In {
                host: Some("kagi.com"),
                ..i()
            },
            "Organic Search",
        ),
        row(
            "microsoft 365",
            In {
                host: Some("excel.officeapps.live.com"),
                ..i()
            },
            "Referral",
        ),
        row(
            "google + display + unknown click id",
            In {
                host: Some("google.com"),
                med: Some("display"),
                click: Some("123identifier"),
                ..i()
            },
            "Display",
        ),
        // ---- 行为不同的地方（偏离清单） ----
        // D2：Plausible 的 Elixir 版和 SQL 版在这里不一致（SQL: Display；Elixir: Paid Search）；Rivu 只有一份，取 Display。
        dev(
            "google + display + gclid",
            In {
                host: Some("google.com"),
                med: Some("display"),
                click: Some("gclid"),
                ..i()
            },
            "Display(SQL) / Paid Search(Elixir)",
            "Display",
            "D2",
        ),
        // D3：Plausible 的 Direct 是兜底，`medium=organic` 没有来源时落到 Direct；GA4 有这条规则。
        dev(
            "organic medium without source",
            In {
                med: Some("organic"),
                ..i()
            },
            "Direct",
            "Organic Search",
            "D3",
        ),
        // D4：Plausible 没有 `ai-assistant` medium。
        dev(
            "ai-assistant medium",
            In {
                med: Some("ai-assistant"),
                ..i()
            },
            "Direct",
            "AI Assistant",
            "D4",
        ),
        // D6：只有 click id、没有 referrer 和 utm（隐私设置抹掉 referrer 时常见）：Plausible 因来源为空是 Direct。
        dev(
            "gclid only",
            In {
                click: Some("gclid"),
                ..i()
            },
            "Direct",
            "Paid Search",
            "D6",
        ),
        dev(
            "msclkid only",
            In {
                click: Some("msclkid"),
                ..i()
            },
            "Direct",
            "Paid Search",
            "D6",
        ),
        // D7：GA4 的 Direct 规则只接受 `(direct)` 配 `(none)`/`(not set)`；两个系统都不会把它算作 Referral。
        dev(
            "(direct)/(none) tagged",
            In {
                src: Some("(direct)"),
                med: Some("(none)"),
                ..i()
            },
            "Referral",
            "Direct",
            "D7",
        ),
    ]
}

#[test]
fn rivu_matches_expectation_on_every_row() {
    let mut equal = 0;
    let mut deviating = 0;
    for r in rows() {
        let got = r.input.channel().as_str();
        assert_eq!(got, r.rivu, "{}", r.label);
        if r.deviation.is_none() {
            assert_eq!(
                got, r.plausible,
                "{}：未标记偏离，必须与 Plausible 一致",
                r.label
            );
            equal += 1;
        } else if r.plausible != r.rivu {
            deviating += 1;
        }
    }
    println!("Plausible 对照：一致 {equal} 行，偏离 {deviating} 行");
    assert!(equal >= 75, "对照用例不应缩水");
}

#[test]
fn deviations_are_all_documented() {
    // 偏离清单的编号必须出现在研究文档里，防止代码里标了偏离而文档没写。
    let doc = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../docs/research/enrich-source.md"
    ))
    .expect("研究文档 docs/research/enrich-source.md 必须存在");
    for r in rows() {
        if let Some(d) = r.deviation
            && r.plausible != r.rivu
        {
            assert!(
                doc.contains(&format!("{d}**")) || doc.contains(&format!("| {d} |")),
                "偏离 {d} 没有写进研究文档"
            );
        }
    }
}

// ---- external_controller_test.exs 的来源解析行为 ----

#[test]
fn utm_source_overrides_referrer_source() {
    // "utm_source overrides referrer source"：referrer betalist.com，utm_source=betalist，来源是 betalist
    let i = In {
        host: Some("betalist.com"),
        src: Some("betalist"),
        ..In::default()
    };
    assert_eq!(i.source(), "betalist");
    // utm_source 与 referrer 不同时取 utm_source
    let i = In {
        host: Some("google.com"),
        src: Some("newsletter"),
        ..In::default()
    };
    assert_eq!(i.source(), "newsletter");
}

#[test]
fn utm_source_matching_a_known_name_uses_the_canonical_capitalization() {
    // "if utm_source matches a capitalized form from ref_inspector, the capitalized form is recorded"
    assert_eq!(common::src("facebook").source(), "Facebook");
    assert_eq!(common::src("FACEBOOK").source(), "Facebook");
    assert_eq!(common::src("fb").source(), "Facebook");
    assert_eq!(common::src("ig").source(), "Instagram");
    assert_eq!(common::src("adwords").source(), "Google");
    // 未知来源原样保留，包括大小写
    assert_eq!(common::src("BetaList").source(), "BetaList");
}

#[test]
fn referrer_host_variants_resolve_to_one_source() {
    // 研究文档任务书：google.com / www.google.co.jp → Google；Plausible: google.co.uk → Google
    for h in [
        "google.com",
        "www.google.com",
        "google.co.uk",
        "www.google.co.jp",
        "google.de",
        "google.com.br",
        "images.google.fr",
    ] {
        assert_eq!(common::host(h).source(), "Google", "host={h}");
    }
}

#[test]
fn unknown_referrer_uses_the_host_as_source() {
    // Plausible：RefInspector 里没有就直接用 host
    assert_eq!(
        common::host("indiehackers.com").source(),
        "indiehackers.com"
    );
    assert_eq!(
        common::host("blog.example.org").source(),
        "blog.example.org"
    );
}
