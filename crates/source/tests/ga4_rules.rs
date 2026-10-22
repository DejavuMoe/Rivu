//! GA4 默认渠道分组规则逐条测试。
//!
//! 规则原文：<https://support.google.com/analytics/answer/9756891>「Default channel definitions /
//! Channels for manual traffic」表（查阅于 2026-10-22，摘录见 `docs/research/enrich-source.md` §2）。
//! 每个测试名以规则编号开头（`r07_`），对应研究文档 §3.3 的有序规则表；
//! 每条规则至少一个正例和一个边界反例。「（偏离）」标记的用例对应研究文档「刻意偏离清单」。

mod common;

use common::{In, camp, host, med, src};
use rivu_source::Channel::*;

// ---------- R01 Direct：Source exactly matches "(direct)" AND Medium is one of ("(not set)", "(none)") ----------

#[test]
fn r01_direct_source_direct_with_none_or_not_set_medium() {
    let i = In {
        src: Some("(direct)"),
        med: Some("(none)"),
        ..In::default()
    };
    assert_eq!(i.channel(), Direct);
    let i = In {
        src: Some("(direct)"),
        med: Some("(not set)"),
        ..In::default()
    };
    assert_eq!(i.channel(), Direct);
    // 大小写不敏感（页面：Channel definitions aren't case sensitive）
    let i = In {
        src: Some("(DIRECT)"),
        med: Some("(None)"),
        ..In::default()
    };
    assert_eq!(i.channel(), Direct);
    // medium 缺失在 GA 里显示为 (not set)
    assert_eq!(src("(direct)").channel(), Direct);
}

#[test]
fn r01_boundary_direct_requires_exact_source_and_unset_medium() {
    // medium 不是 (none)/(not set)：不是 Direct，落到 Referral（来源非空）
    let i = In {
        src: Some("(direct)"),
        med: Some("referral"),
        ..In::default()
    };
    assert_eq!(i.channel(), Referral);
    // 来源不是恰好 "(direct)"
    let i = In {
        src: Some("direct"),
        med: Some("(none)"),
        ..In::default()
    };
    assert_eq!(i.channel(), Referral);
}

// ---------- R02 Cross-network：Campaign Name contains "cross-network" ----------

#[test]
fn r02_cross_network_campaign_contains() {
    assert_eq!(camp("cross-network").channel(), CrossNetwork);
    assert_eq!(camp("Q4-CROSS-NETWORK-pmax").channel(), CrossNetwork);
    // 优先于后面所有规则：付费搜索来源也归 Cross-network
    let i = In {
        src: Some("google"),
        med: Some("cpc"),
        camp: Some("cross-network"),
        ..In::default()
    };
    assert_eq!(i.channel(), CrossNetwork);
}

#[test]
fn r02_boundary_cross_network_needs_the_hyphen() {
    assert_eq!(camp("cross network").channel(), Direct);
    assert_eq!(camp("crossnetwork").channel(), Direct);
    assert_eq!(camp("cross_network").channel(), Direct);
}

// ---------- R03 Paid Shopping：(shopping 来源 OR campaign 匹配购物正则) AND 付费 medium ----------

#[test]
fn r03_paid_shopping_source_or_campaign_with_paid_medium() {
    let i = In {
        host: Some("amazon.com"),
        med: Some("cpc"),
        ..In::default()
    };
    assert_eq!(i.channel(), PaidShopping);
    let i = In {
        src: Some("foo"),
        med: Some("cpc"),
        camp: Some("eshop_spring"),
        ..In::default()
    };
    assert_eq!(i.channel(), PaidShopping);
    let i = In {
        src: Some("foo"),
        med: Some("paid"),
        camp: Some("shopping"),
        ..In::default()
    };
    assert_eq!(i.channel(), PaidShopping);
}

#[test]
fn r03_boundary_workshop_is_not_a_shopping_campaign() {
    // `([^a-df-z]|^)shop`：shop 前面是 k（a–d、f–z）不算
    let i = In {
        src: Some("foo"),
        med: Some("cpc"),
        camp: Some("workshop"),
        ..In::default()
    };
    assert_eq!(i.channel(), PaidOther);
    // 购物来源但 medium 不付费：不是 Paid Shopping
    assert_eq!(host("amazon.com").channel(), OrganicShopping);
}

// ---------- R04 Paid Search：搜索来源 AND 付费 medium ----------

#[test]
fn r04_paid_search_search_source_with_paid_medium() {
    for m in [
        "cpc",
        "ppc",
        "retargeting",
        "paid",
        "paid-search",
        "CPC",
        "PPC",
        "Paid_Search",
    ] {
        let i = In {
            host: Some("google.com"),
            med: Some(m),
            ..In::default()
        };
        assert_eq!(i.channel(), PaidSearch, "medium={m}");
    }
    // utm_source 也能表明搜索来源
    let i = In {
        src: Some("bing"),
        med: Some("cpc"),
        ..In::default()
    };
    assert_eq!(i.channel(), PaidSearch);
}

#[test]
fn r04_boundary_paid_medium_regex_is_anchored() {
    // `^(.*cp.*|ppc|retargeting|paid.*)$`：
    // ppc、retargeting 必须整串相等；paid 必须在开头；cp 可以在任意位置。
    for m in [
        "unpaid",
        "ppc-brand",
        "retargeting-fb",
        "pay",
        "pc",
        "organic",
    ] {
        let i = In {
            host: Some("google.com"),
            med: Some(m),
            ..In::default()
        };
        assert_eq!(i.channel(), OrganicSearch, "medium={m} 不应被当作付费");
    }
    // `.*cp.*`：cp 出现在任意位置都算（"tcp"、"a-cp-b" 这类写法 GA 同样算付费）
    for m in ["tcp", "a-cp-b", "cpm", "cpa", "cpv"] {
        let i = In {
            host: Some("google.com"),
            med: Some(m),
            ..In::default()
        };
        assert_eq!(i.channel(), PaidSearch, "medium={m}");
    }
}

// ---------- R05 Paid Social ----------

#[test]
fn r05_paid_social_social_source_with_paid_medium() {
    let i = In {
        host: Some("facebook.com"),
        med: Some("paid_social"),
        ..In::default()
    };
    assert_eq!(i.channel(), PaidSocial);
    let i = In {
        src: Some("linkedin"),
        med: Some("cpc"),
        ..In::default()
    };
    assert_eq!(i.channel(), PaidSocial);
}

#[test]
fn r05_boundary_social_source_without_paid_medium_is_organic() {
    assert_eq!(host("facebook.com").channel(), OrganicSocial);
    let i = In {
        host: Some("facebook.com"),
        med: Some("social"),
        ..In::default()
    };
    assert_eq!(i.channel(), OrganicSocial);
}

// ---------- R06 Paid Video ----------

#[test]
fn r06_paid_video_video_source_with_paid_medium() {
    let i = In {
        host: Some("youtube.com"),
        med: Some("cpv"),
        ..In::default()
    };
    assert_eq!(i.channel(), PaidVideo);
    let i = In {
        src: Some("vimeo"),
        med: Some("paid"),
        ..In::default()
    };
    assert_eq!(i.channel(), PaidVideo);
}

#[test]
fn r06_boundary_video_source_without_paid_medium_is_organic() {
    assert_eq!(host("youtube.com").channel(), OrganicVideo);
}

// ---------- R07 Display：Medium is one of (display, banner, expandable, interstitial, cpm) ----------

#[test]
fn r07_display_exact_medium_list() {
    for m in [
        "display",
        "banner",
        "expandable",
        "interstitial",
        "cpm",
        "DISPLAY",
        "Banner",
    ] {
        assert_eq!(med(m).channel(), Display, "medium={m}");
    }
    // 没有付费来源类别时 cpm 是 Display，不是 Paid Other（R07 在 R08 之前）
    let i = In {
        src: Some("foo"),
        med: Some("cpm"),
        ..In::default()
    };
    assert_eq!(i.channel(), Display);
}

#[test]
fn r07_boundary_display_is_exact_and_after_paid_search_social_video() {
    // 整串相等，不是包含
    for m in ["displayads", "display-ads", "banners"] {
        assert_eq!(med(m).channel(), Direct, "medium={m}");
    }
    // GA4 页面里 Display 排在 Paid Search 之后：cpm + 搜索来源是 Paid Search
    let i = In {
        host: Some("google.com"),
        med: Some("cpm"),
        ..In::default()
    };
    assert_eq!(i.channel(), PaidSearch);
    // medium=display 不是付费正则，搜索来源配 display 是 Display
    let i = In {
        host: Some("google.com"),
        med: Some("display"),
        ..In::default()
    };
    assert_eq!(i.channel(), Display);
}

// ---------- R08 Paid Other：付费 medium，但没有命中上面的来源类别 ----------

#[test]
fn r08_paid_other_paid_medium_without_category_source() {
    assert_eq!(med("cpc").channel(), PaidOther);
    assert_eq!(med("PPC").channel(), PaidOther);
    let i = In {
        host: Some("example.org"),
        med: Some("paid-newsletter"),
        ..In::default()
    };
    assert_eq!(i.channel(), PaidOther);
}

#[test]
fn r08_boundary_unpaid_and_organic_are_not_paid_other() {
    assert_eq!(med("unpaid").channel(), Direct);
    assert_eq!(med("pc").channel(), Direct);
}

// ---------- R09 click id 付费推断（Rivu 追加，对应 GA4 的 Google Ads 自动标记；见偏离清单） ----------

#[test]
fn r09_click_id_confirms_paid_only_for_the_matching_platform() {
    let i = In {
        host: Some("google.com"),
        click: Some("gclid"),
        ..In::default()
    };
    assert_eq!(i.channel(), PaidSearch);
    let i = In {
        host: Some("bing.com"),
        click: Some("msclkid"),
        ..In::default()
    };
    assert_eq!(i.channel(), PaidSearch);
    let i = In {
        host: Some("tiktok.com"),
        click: Some("ttclid"),
        ..In::default()
    };
    assert_eq!(i.channel(), PaidSocial);
}

#[test]
fn r09_boundary_click_id_of_another_platform_or_explicit_medium_does_not_count() {
    // 来源是 DuckDuckGo 却带 gclid：不是 Google Ads 点击
    let i = In {
        host: Some("duckduckgo.com"),
        click: Some("gclid"),
        ..In::default()
    };
    assert_eq!(i.channel(), OrganicSearch);
    // Google 来源带 msclkid：同样不算
    let i = In {
        host: Some("google.com"),
        click: Some("msclkid"),
        ..In::default()
    };
    assert_eq!(i.channel(), OrganicSearch);
    // 显式 medium=display 优先于 click id
    let i = In {
        host: Some("google.com"),
        med: Some("display"),
        click: Some("gclid"),
        ..In::default()
    };
    assert_eq!(i.channel(), Display);
    // fbclid 会附加到任何 Facebook 外链点击，不能证明付费
    let i = In {
        host: Some("facebook.com"),
        click: Some("fbclid"),
        ..In::default()
    };
    assert_eq!(i.channel(), OrganicSocial);
}

// ---------- R10 Organic Shopping：购物来源 OR campaign 匹配购物正则 ----------

#[test]
fn r10_organic_shopping_source_or_campaign_regex() {
    assert_eq!(host("ebay.de").channel(), OrganicShopping);
    for c in [
        "shopping",
        "shop",
        "Shop",
        "eshop",
        "e-shop",
        "my_shop",
        "1shop",
        "shops",
        "holiday-shopping-2026",
    ] {
        assert_eq!(camp(c).channel(), OrganicShopping, "campaign={c}");
    }
}

#[test]
fn r10_boundary_shop_preceded_by_a_letter_other_than_e_is_not_shopping() {
    // `[^a-df-z]` 排除 a–d、f–z：只有 e、数字、符号能出现在 shop 前面
    for c in ["workshop", "photoshop", "bookshop", "barbershop", "ashop"] {
        assert_eq!(camp(c).channel(), Direct, "campaign={c}");
    }
    assert_eq!(
        camp("coffeeshop").channel(),
        OrganicShopping,
        "e 在允许的字符里"
    );
}
// ---------- R11 Organic Social：社交来源 OR medium 在列表里 ----------

#[test]
fn r11_organic_social_source_or_medium_list() {
    assert_eq!(host("t.co").channel(), OrganicSocial);
    for m in [
        "social",
        "social-network",
        "social-media",
        "sm",
        "social network",
        "social media",
        "Social-Media",
        "SM",
    ] {
        assert_eq!(med(m).channel(), OrganicSocial, "medium={m}");
    }
}

#[test]
fn r11_boundary_social_medium_is_exact() {
    for m in [
        "socialmedia",
        "social_media",
        "social-networks",
        "sms-social",
    ] {
        assert_ne!(med(m).channel(), OrganicSocial, "medium={m}");
    }
}

// ---------- R12 Organic Video：视频来源 OR medium 匹配 ^(.*video.*)$ ----------

#[test]
fn r12_organic_video_source_or_medium_contains_video() {
    assert_eq!(host("vimeo.com").channel(), OrganicVideo);
    for m in ["video", "Video", "short-video", "videos", "VIDEO_AD"] {
        assert_eq!(med(m).channel(), OrganicVideo, "medium={m}");
    }
}

#[test]
fn r12_boundary_vid_is_not_video() {
    assert_eq!(med("vid").channel(), Direct);
    assert_eq!(med("vide0").channel(), Direct);
}

// ---------- R13 AI Assistant：medium 恰为 "ai-assistant"；来源是已知 AI 助手 ----------

#[test]
fn r13_ai_assistant_medium_or_known_source() {
    assert_eq!(med("ai-assistant").channel(), AiAssistant);
    assert_eq!(med("AI-Assistant").channel(), AiAssistant);
    for h in [
        "chatgpt.com",
        "chat.openai.com",
        "claude.ai",
        "perplexity.ai",
        "gemini.google.com",
        "copilot.microsoft.com",
        "chat.deepseek.com",
        "grok.com",
    ] {
        assert_eq!(host(h).channel(), AiAssistant, "host={h}");
    }
}

#[test]
fn r13_boundary_ai_medium_is_exact_and_gemini_is_not_google_search() {
    assert_eq!(med("ai assistant").channel(), Direct);
    assert_eq!(med("ai-assistants").channel(), Direct);
    // gemini.google.com 比 google.com 更具体
    assert_eq!(host("gemini.google.com").source(), "Google Gemini");
    assert_eq!(host("www.google.com").channel(), OrganicSearch);
    // （偏离）已知 AI 来源在 medium=organic 时仍是 AI Assistant，GA4 页面顺序里 Organic Search 在前
    let i = In {
        host: Some("chatgpt.com"),
        med: Some("organic"),
        ..In::default()
    };
    assert_eq!(i.channel(), AiAssistant);
}

// ---------- R14 Organic Search：搜索来源 OR Medium exactly matches organic ----------

#[test]
fn r14_organic_search_source_or_medium_organic() {
    assert_eq!(host("www.google.co.jp").channel(), OrganicSearch);
    assert_eq!(host("duckduckgo.com").channel(), OrganicSearch);
    assert_eq!(med("organic").channel(), OrganicSearch);
    assert_eq!(med("Organic").channel(), OrganicSearch);
}

#[test]
fn r14_boundary_organic_medium_is_exact() {
    for m in ["organic-social", "organics", "non-organic", "inorganic"] {
        assert_ne!(med(m).channel(), OrganicSearch, "medium={m}");
    }
}

// ---------- R15 Email：Source/Medium = email|e-mail|e_mail|e mail ----------

#[test]
fn r15_email_medium_or_source() {
    for m in ["email", "e-mail", "e_mail", "e mail", "EMAIL", "E-Mail"] {
        assert_eq!(med(m).channel(), Email, "medium={m}");
        assert_eq!(src(m).channel(), Email, "source={m}");
    }
}

#[test]
fn r15_boundary_gmail_medium_is_not_email_but_webmail_referrer_is() {
    // "gmail" 里 mail 前面是 g 不是 e，不命中 `e[-_ ]?mail`
    assert_eq!(med("gmail").channel(), Direct);
    assert_eq!(med("mail").channel(), Direct);
    // （偏离）网页邮箱的 referrer 视为 Email；GA4 里它是 Referral
    assert_eq!(host("mail.google.com").channel(), Email);
    // （偏离）包含而非整词，并加上 newsletter（Plausible 的做法）
    assert_eq!(med("email-campaign").channel(), Email);
    assert_eq!(src("Newsletter-UK").channel(), Email);
}

// ---------- R16 Affiliates：Medium = affiliate ----------

#[test]
fn r16_affiliates_medium_exact() {
    assert_eq!(med("affiliate").channel(), Affiliates);
    assert_eq!(med("Affiliate").channel(), Affiliates);
}

#[test]
fn r16_boundary_affiliates_plural_is_not_affiliate() {
    assert_eq!(med("affiliates").channel(), Direct);
    assert_eq!(med("affiliate-program").channel(), Direct);
}

// ---------- R17 Audio：Medium exactly matches audio ----------

#[test]
fn r17_audio_medium_exact() {
    assert_eq!(med("audio").channel(), Audio);
    assert_eq!(med("AUDIO").channel(), Audio);
}

#[test]
fn r17_boundary_podcast_is_not_audio() {
    assert_eq!(med("podcast").channel(), Direct);
    assert_eq!(med("audio-ad").channel(), Direct);
}

// ---------- R18 SMS：Source/Medium exactly matches sms ----------

#[test]
fn r18_sms_source_or_medium_exact() {
    assert_eq!(med("sms").channel(), Sms);
    assert_eq!(med("SMS").channel(), Sms);
    assert_eq!(src("sms").channel(), Sms);
}

#[test]
fn r18_boundary_sms_is_exact() {
    assert_eq!(med("sms-campaign").channel(), Direct);
    assert_ne!(src("smsc").channel(), Sms);
}

// ---------- R19 Mobile Push Notifications：Medium ends with push / contains mobile|notification / Source = firebase ----------

#[test]
fn r19_mobile_push_medium_suffix_or_contains_or_firebase_source() {
    for m in [
        "push",
        "app-push",
        "PUSH",
        "mobile",
        "example-mobile",
        "notification",
        "push-notifications",
        "Notification",
    ] {
        assert_eq!(med(m).channel(), MobilePushNotifications, "medium={m}");
    }
    assert_eq!(src("firebase").channel(), MobilePushNotifications);
    assert_eq!(src("Firebase").channel(), MobilePushNotifications);
}

#[test]
fn r19_boundary_push_must_be_a_suffix() {
    // "pushy"、"push-ads" 不以 push 结尾，也不含 mobile/notification
    assert_eq!(med("pushy").channel(), Direct);
    assert_eq!(med("push-ads").channel(), Direct);
    assert_ne!(src("firebase-ads").channel(), MobilePushNotifications);
}

// ---------- R20 Referral：Medium is one of (referral, app, link) ----------

#[test]
fn r20_referral_medium_list_or_any_source() {
    for m in ["referral", "app", "link", "Referral", "LINK"] {
        assert_eq!(med(m).channel(), Referral, "medium={m}");
    }
    // 来源非空（referrer 流量在 GA4 里 medium 恒为 referral）
    assert_eq!(host("example.org").channel(), Referral);
}

#[test]
fn r20_boundary_referral_medium_is_exact() {
    for m in ["linkedin", "links", "apps", "referrals"] {
        assert_eq!(med(m).channel(), Direct, "medium={m}");
    }
}

#[test]
fn r20_unrecognised_medium_with_a_source_falls_back_to_referral_not_unassigned() {
    // （偏离 D9）GA4 在这里给出 Unassigned；Rivu 没有这个取值，有来源就是 Referral，没有来源就是 Direct
    let i = In {
        src: Some("betalist"),
        med: Some("weird-medium"),
        ..In::default()
    };
    assert_eq!(i.channel(), Referral);
    let i = In {
        host: Some("example.org"),
        med: Some("weird-medium"),
        ..In::default()
    };
    assert_eq!(i.channel(), Referral);
    assert_eq!(med("weird-medium").channel(), Direct);
}

// ---------- R21 Direct：其余 ----------

#[test]
fn r21_direct_is_the_remainder() {
    assert_eq!(In::default().channel(), Direct);
    // 空串在 Utm 里已是 None；这里直接传空白也等价于缺失
    let i = In {
        host: Some("  "),
        src: Some(""),
        med: Some(" "),
        ..In::default()
    };
    assert_eq!(i.channel(), Direct);
    assert_eq!(i.source(), "");
}

// ---------- 渠道取值集合 ----------

#[test]
fn channel_identifiers_are_unique_and_round_trip() {
    use std::collections::HashSet;
    let mut seen = HashSet::new();
    for c in rivu_source::Channel::ALL {
        assert!(seen.insert(c.as_str()), "重复标识 {}", c.as_str());
        assert_eq!(rivu_source::Channel::parse(c.as_str()), Some(c));
        assert!(!c.zh_name().is_empty());
    }
    assert_eq!(seen.len(), 19);
    assert_eq!(rivu_source::Channel::parse("organic search"), None);
}
