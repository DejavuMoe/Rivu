//! host 后缀匹配、`.{}` 国家/地区后缀占位符、来源优先级。规则的定义在研究文档 §4.3、§5。

mod common;

use common::{In, host, med, src};
use rivu_source::Channel::*;

// ---------- 后缀匹配 ----------

#[test]
fn suffix_match_hits_the_key_and_every_subdomain() {
    for h in ["google.co.jp", "www.google.co.jp", "a.b.c.google.co.jp"] {
        assert_eq!(host(h).source(), "Google", "host={h}");
    }
    for h in ["en.wikipedia.org", "zh.m.wikipedia.org", "wikipedia.org"] {
        assert_eq!(host(h).source(), "Wikipedia", "host={h}");
    }
    assert_eq!(host("cn.bing.com").source(), "Bing");
}

#[test]
fn suffix_match_is_on_label_boundaries_only() {
    // `notyoutube.com` 以 `youtube.com` 结尾，但不是它的子域
    for h in [
        "notyoutube.com",
        "evil-google.co.jp",
        "xt.co",
        "fakereddit.com",
    ] {
        assert_eq!(host(h).source(), h, "host={h}");
        assert_eq!(host(h).channel(), Referral, "host={h}");
    }
    // 攻击者可以把已知域名放在前面，后面接自己的域名
    for h in [
        "google.com.evil.com",
        "youtube.com.evil.example",
        "facebook.com.attacker.net",
    ] {
        assert_eq!(host(h).source(), h, "host={h}");
    }
}

#[test]
fn a_bare_top_level_label_never_matches() {
    for h in ["com", "jp", "co.uk", "localhost"] {
        assert_eq!(host(h).source(), h, "host={h}");
    }
}

#[test]
fn the_more_specific_key_wins() {
    assert_eq!(host("gemini.google.com").source(), "Google Gemini");
    assert_eq!(host("notebooklm.google.com").source(), "NotebookLM");
    assert_eq!(host("mail.google.com").source(), "Gmail");
    assert_eq!(host("accounts.google.com").source(), "Google");
    assert_eq!(host("mail.yahoo.com").source(), "Yahoo! Mail");
    assert_eq!(host("shopping.yahoo.com").source(), "Yahoo! Shopping");
    assert_eq!(host("search.yahoo.com").source(), "Yahoo!");
    assert_eq!(host("blog.naver.com").source(), "Naver Blog");
    assert_eq!(host("search.naver.com").source(), "Naver");
    assert_eq!(host("music.youtube.com").source(), "YouTube");
}

#[test]
fn matching_ignores_ascii_case_and_a_trailing_dot() {
    for h in ["WWW.GOOGLE.COM", "Google.Com", "www.google.com."] {
        assert_eq!(host(h).source(), "Google", "host={h}");
    }
}

// ---------- `.{}` 占位符 ----------

#[test]
fn tld_wildcard_matches_country_codes_generic_tlds_and_combined_tlds() {
    for h in [
        "google.com",
        "google.de",
        "google.fr",
        "google.co.uk",
        "google.com.br",
        "google.co.jp",
        "google.com.au",
        "www.google.nl",
        "images.google.fr",
        "news.google.co.in",
        "scholar.google.com.hk",
        "google.edu.cn",
    ] {
        assert_eq!(host(h).source(), "Google", "host={h}");
    }
    assert_eq!(host("amazon.co.uk").source(), "Amazon");
    assert_eq!(host("www.ebay.com.au").source(), "eBay");
    assert_eq!(host("pinterest.ru").source(), "Pinterest");
}

#[test]
fn tld_wildcard_boundaries() {
    // 三个字母的非通用后缀、四个字母以上的后缀不是国家/地区后缀
    for h in ["google.xyz", "google.info", "google.shop", "google.co.xyz"] {
        assert_eq!(host(h).source(), h, "host={h}");
    }
    // 中间夹了别的标签：`google` 不在最右边
    for h in ["google.evil.de", "google.attacker.co.uk"] {
        assert_eq!(host(h).source(), h, "host={h}");
    }
    // 两个标签的后缀只接受 com/org/net/co/it/edu 开头
    assert_eq!(host("google.xx.uk").source(), "google.xx.uk");
}

#[test]
fn exact_key_and_wildcard_agree_when_both_exist() {
    // google.com 同时有精确键和 google.{}；平手取精确键，结果相同
    assert_eq!(host("google.com").source(), "Google");
}

// ---------- 带路径的数据条目不能降级成 host ----------

#[test]
fn path_entries_are_not_downgraded_to_the_whole_site() {
    // 数据里的 `x.com/i/grok` 只有 host 的接口无法识别：x.com 整站仍是 X (Twitter)，不是 Grok
    assert_eq!(host("x.com").source(), "X (Twitter)");
    assert_eq!(host("x.com").channel(), OrganicSocial);
    // 数据里的 `google.com/products` 是 Google Shopping：google.com 整站仍是 Google 搜索
    assert_eq!(host("google.com").channel(), OrganicSearch);
}

// ---------- 未知、IP、非 ASCII ----------

#[test]
fn unknown_hosts_use_themselves_as_the_source() {
    for h in [
        "example.org",
        "192.168.0.1",
        "[::1]",
        "xn--fsq.xn--0zwm56d",
        "例子.中国",
        "a",
        "com.reddit.unknown",
    ] {
        let info = host(h).run();
        assert_eq!(info.referrer_source, h, "host={h}");
        assert_eq!(info.channel, Referral);
    }
}

#[test]
fn android_package_referrers() {
    assert_eq!(host("com.reddit.frontpage").source(), "Reddit");
    assert_eq!(host("org.telegram.messenger").source(), "Telegram");
}

// ---------- 来源优先级 ----------

#[test]
fn utm_source_beats_the_referrer() {
    let i = In {
        host: Some("google.com"),
        src: Some("newsletter"),
        ..In::default()
    };
    assert_eq!(i.source(), "newsletter");
    assert_eq!(i.channel(), Email);
    // utm_source 认识：用它的规范名，渠道按它的类别
    let i = In {
        host: Some("example.org"),
        src: Some("facebook"),
        ..In::default()
    };
    assert_eq!(i.source(), "Facebook");
    assert_eq!(i.channel(), OrganicSocial);
}

#[test]
fn referrer_is_used_when_there_is_no_utm_source() {
    let i = In {
        host: Some("google.com"),
        med: Some("cpc"),
        ..In::default()
    };
    assert_eq!(i.source(), "Google");
    assert_eq!(i.channel(), PaidSearch);
    // 只有 utm_medium=social：来源仍取 referrer
    let i = In {
        host: Some("t.co"),
        med: Some("social"),
        ..In::default()
    };
    assert_eq!(i.source(), "X (Twitter)");
}

#[test]
fn utm_source_may_be_a_domain() {
    assert_eq!(src("facebook.com").source(), "Facebook");
    assert_eq!(src("l.facebook.com").source(), "Facebook");
    assert_eq!(src("Google.com").source(), "Google");
    // 不认识的域名原样保留
    assert_eq!(src("Example.COM").source(), "Example.COM");
}

#[test]
fn unknown_utm_source_keeps_its_case() {
    let info = src("BetaList").run();
    assert_eq!(info.referrer_source, "BetaList");
    assert_eq!(info.channel, Referral);
}

#[test]
fn internal_referral_is_none_and_gives_direct_unless_something_else_identifies_the_source() {
    // 调用方判定为内部引荐后传 None：没有任何其他信号时是 Direct，来源为空
    let info = In::default().run();
    assert_eq!((info.referrer_source.as_str(), info.channel), ("", Direct));
    // 落地页自带 utm_source 时，来源按 utm_source（内部跳转带标签的链接仍算该来源）
    assert_eq!(src("newsletter").source(), "newsletter");
    // medium 规则不依赖来源
    assert_eq!(med("email").channel(), Email);
}

#[test]
fn click_id_alone_infers_source_and_channel() {
    let i = In {
        click: Some("gclid"),
        ..In::default()
    };
    let info = i.run();
    assert_eq!(
        (info.referrer_source.as_str(), info.channel),
        ("Google", PaidSearch)
    );
    for (c, name, ch) in [
        ("gbraid", "Google", PaidSearch),
        ("wbraid", "Google", PaidSearch),
        ("msclkid", "Bing", PaidSearch),
        ("ttclid", "TikTok", PaidSocial),
        ("fbclid", "Facebook", OrganicSocial),
        ("twclid", "X (Twitter)", OrganicSocial),
        ("li_fat_id", "LinkedIn", OrganicSocial),
    ] {
        let info = In {
            click: Some(c),
            ..In::default()
        }
        .run();
        assert_eq!(
            (info.referrer_source.as_str(), info.channel),
            (name, ch),
            "click id {c}"
        );
    }
    // 未知的参数名：不推断
    let info = In {
        click: Some("whatever"),
        ..In::default()
    }
    .run();
    assert_eq!((info.referrer_source.as_str(), info.channel), ("", Direct));
}

#[test]
fn click_id_does_not_override_an_existing_source() {
    // 有 referrer：来源取 referrer，click id 只在平台一致时作为付费证据
    let i = In {
        host: Some("example.org"),
        click: Some("gclid"),
        ..In::default()
    };
    assert_eq!(i.source(), "example.org");
    assert_eq!(i.channel(), Referral);
    // 有 utm_source：同理
    let i = In {
        src: Some("newsletter"),
        click: Some("gclid"),
        ..In::default()
    };
    assert_eq!(i.source(), "newsletter");
    assert_eq!(i.channel(), Email);
}

#[test]
fn blank_values_count_as_absent() {
    let i = In {
        host: Some(" "),
        src: Some(" "),
        med: Some("\t"),
        camp: Some(""),
        click: None,
    };
    let info = i.run();
    assert_eq!((info.referrer_source.as_str(), info.channel), ("", Direct));
    // 空 utm_source 不压过 referrer
    let i = In {
        host: Some("google.com"),
        src: Some(""),
        ..In::default()
    };
    assert_eq!(i.source(), "Google");
}

// ---------- 吞吐（粗略，默认忽略） ----------

/// `cargo test -p rivu-source --release --test host_matching perf_smoke -- --ignored --nocapture`
#[test]
#[ignore = "手动跑的粗略计时，不做断言"]
fn perf_smoke() {
    let hosts = [
        "www.google.co.jp",
        "t.co",
        "example.org",
        "mail.google.com",
        "en.wikipedia.org",
        "news.ycombinator.com",
    ];
    let utm = rivu_domain::Utm {
        medium: Some("cpc".into()),
        ..Default::default()
    };
    let n = 2_000_000u32;
    let start = std::time::Instant::now();
    let mut sink = 0usize;
    for i in 0..n {
        let info = rivu_source::classify(&rivu_source::SourceInput {
            referrer_host: Some(hosts[(i as usize) % hosts.len()]),
            utm: &utm,
            click_id_param: None,
        });
        sink += info.referrer_source.len();
    }
    let ns = start.elapsed().as_nanos() as f64 / f64::from(n);
    println!("classify：{ns:.0} ns/次（{sink}）");
}
