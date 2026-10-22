//! 来源解析与渠道规则。规则的出处、顺序和与 GA4/Plausible 的差异见 `docs/research/enrich-source.md`。
//!
//! 所有比较都是 ASCII 大小写不敏感且不分配（GA4 帮助页：「渠道定义不区分大小写」）。
//! 非 ASCII 字符不做大小写折叠：规则里出现的关键字（`cpc`、`paid`、`video`……）全是 ASCII。

use std::borrow::Cow;

use rivu_domain::Utm;

use crate::channel::Channel;
use crate::table::{Kind, lookup_alias, lookup_host};

// ---------- 文本判断 ----------

fn eq_ci(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
}

fn one_of_ci(s: &str, set: &[&str]) -> bool {
    set.iter().any(|x| eq_ci(s, x))
}

/// `needle` 必须是小写 ASCII。
fn contains_ci(hay: &str, needle: &str) -> bool {
    let n = needle.as_bytes();
    hay.as_bytes()
        .windows(n.len())
        .any(|w| w.eq_ignore_ascii_case(n))
}

fn starts_with_ci(hay: &str, prefix: &str) -> bool {
    hay.len() >= prefix.len()
        && hay.as_bytes()[..prefix.len()].eq_ignore_ascii_case(prefix.as_bytes())
}

fn ends_with_ci(hay: &str, suffix: &str) -> bool {
    hay.len() >= suffix.len()
        && hay.as_bytes()[hay.len() - suffix.len()..].eq_ignore_ascii_case(suffix.as_bytes())
}

// ---------- GA4 正则的手写等价物 ----------
// 正则里的 `.` 不匹配 `\n`，整串锚定的 `^(.*x.*)$` 因此在含换行的输入上整体失败；
// 调用方已去掉控制字符，这里仍保持同样的行为，让「GA 正则」与这里逐字符对得上。

/// `^(.*cp.*|ppc|retargeting|paid.*)$`（GA4：付费 medium）。
fn is_paid_medium(m: &str) -> bool {
    !m.contains('\n')
        && (contains_ci(m, "cp")
            || eq_ci(m, "ppc")
            || eq_ci(m, "retargeting")
            || starts_with_ci(m, "paid"))
}

/// `^(.*(([^a-df-z]|^)shop|shopping).*)$`（GA4：购物 campaign）。
/// `shop` 前面必须是行首，或除 a–d、f–z 以外的任意字符（即 `e`、数字、符号、非 ASCII）：
/// `eshop`、`e-shop`、`shop` 命中，`workshop`、`photoshop` 不命中；`shopping` 出现在任何位置都命中。
fn is_shopping_campaign(c: &str) -> bool {
    if c.contains('\n') {
        return false;
    }
    if contains_ci(c, "shopping") {
        return true;
    }
    let b = c.as_bytes();
    b.windows(4).enumerate().any(|(i, w)| {
        w.eq_ignore_ascii_case(b"shop")
            && (i == 0 || {
                let p = b[i - 1].to_ascii_lowercase();
                !(matches!(p, b'a'..=b'd') || matches!(p, b'f'..=b'z'))
            })
    })
}

/// `^(.*video.*)$`（GA4：自然视频 medium）。
fn medium_contains_video(m: &str) -> bool {
    !m.contains('\n') && contains_ci(m, "video")
}

/// GA4：`email|e-mail|e_mail|e mail`（整词）。Rivu 放宽成「包含」并加上 `newsletter`，见偏离清单。
fn mentions_email(s: &str) -> bool {
    ["email", "e-mail", "e_mail", "e mail", "newsletter"]
        .iter()
        .any(|k| contains_ci(s, k))
}

// ---------- click id ----------

/// click id 对应的平台。`alias` 是平台在别名表里的小写写法，名字和类别从表里取，不在这里重复。
struct ClickPlatform {
    param: &'static str,
    alias: &'static str,
    /// 这个 click id 是否是「该平台的广告点击」的证据。只有官方文档说明由广告点击自动附加的才算。
    paid: bool,
}

const CLICK_PLATFORMS: [ClickPlatform; 8] = [
    // Google Ads 自动标记（gclid；iOS 隐私场景的 gbraid、wbraid）
    ClickPlatform {
        param: "gclid",
        alias: "google",
        paid: true,
    },
    ClickPlatform {
        param: "gbraid",
        alias: "google",
        paid: true,
    },
    ClickPlatform {
        param: "wbraid",
        alias: "google",
        paid: true,
    },
    // Microsoft Advertising 自动标记
    ClickPlatform {
        param: "msclkid",
        alias: "bing",
        paid: true,
    },
    // TikTok 官方帮助页：点击广告时附加到落地页 URL
    ClickPlatform {
        param: "ttclid",
        alias: "tiktok",
        paid: true,
    },
    // fbclid 会附加到 Facebook 上任何外链点击（含自然帖子），不能证明付费，只用来推断来源
    ClickPlatform {
        param: "fbclid",
        alias: "facebook",
        paid: false,
    },
    // twclid、li_fat_id 没有找到平台官方文档，保守起见只推断来源
    ClickPlatform {
        param: "twclid",
        alias: "x (twitter)",
        paid: false,
    },
    ClickPlatform {
        param: "li_fat_id",
        alias: "linkedin",
        paid: false,
    },
];

fn click_platform(param: &str) -> Option<&'static ClickPlatform> {
    CLICK_PLATFORMS.iter().find(|p| eq_ci(p.param, param))
}
// ---------- 来源解析 ----------

pub(crate) struct Source<'a> {
    /// 写入 `referrer_source` 的名字；空串表示没有来源。
    pub name: Cow<'a, str>,
    pub kind: Kind,
    /// 来源写法本身表示付费（`fb-ads`、`adwords`）。
    pub paid_alias: bool,
}

impl Source<'_> {
    const NONE: Source<'static> = Source {
        name: Cow::Borrowed(""),
        kind: Kind::Other,
        paid_alias: false,
    };
}

fn nonempty(s: Option<&str>) -> Option<&str> {
    s.map(str::trim).filter(|s| !s.is_empty())
}

/// 来源优先级（研究文档 §5）：utm_source（含 `source`、`ref` 回退）> referrer host > click id 推断。
pub(crate) fn resolve_source<'a>(
    referrer_host: Option<&'a str>,
    utm_source: Option<&'a str>,
    click_id_param: Option<&str>,
) -> Source<'a> {
    if let Some(token) = nonempty(utm_source) {
        // 1. 别名表（名字本身也是别名）；2. 像域名的写法（`facebook.com`、`l.facebook.com`）查 host 表；
        // 3. 都不认识就原样保留，保留大小写。
        if let Some((e, paid)) = lookup_alias(token) {
            return Source {
                name: Cow::Borrowed(e.name),
                kind: e.kind,
                paid_alias: paid,
            };
        }
        if token.contains('.')
            && let Some(e) = lookup_host(token)
        {
            return Source {
                name: Cow::Borrowed(e.name),
                kind: e.kind,
                paid_alias: false,
            };
        }
        return Source {
            name: Cow::Borrowed(token),
            kind: Kind::Other,
            paid_alias: false,
        };
    }
    if let Some(host) = nonempty(referrer_host) {
        let host = host.trim_end_matches(|c: char| c == '.' || c.is_whitespace());
        if let Some(e) = lookup_host(host) {
            return Source {
                name: Cow::Borrowed(e.name),
                kind: e.kind,
                paid_alias: false,
            };
        }
        let name = if host.bytes().any(|b| b.is_ascii_uppercase()) {
            Cow::Owned(host.to_ascii_lowercase())
        } else {
            Cow::Borrowed(host)
        };
        return Source {
            name,
            kind: Kind::Other,
            paid_alias: false,
        };
    }
    if let Some(p) = click_id_param.and_then(click_platform)
        && let Some((e, _)) = lookup_alias(p.alias)
    {
        return Source {
            name: Cow::Borrowed(e.name),
            kind: e.kind,
            paid_alias: false,
        };
    }
    Source::NONE
}

/// click id 是否构成付费证据：click id 的平台必须就是已解析出的来源（`gclid` 配 Google，`msclkid` 配 Bing……）。
/// 来源是 DuckDuckGo 却带着 `gclid`，不是 Google Ads 点击。
fn click_id_paid(src: &Source<'_>, click_id_param: Option<&str>) -> Option<Channel> {
    let p = click_platform(click_id_param?)?;
    if !p.paid {
        return None;
    }
    let (e, _) = lookup_alias(p.alias)?;
    if src.name != e.name {
        return None;
    }
    match e.kind {
        Kind::Search => Some(Channel::PaidSearch),
        Kind::Social => Some(Channel::PaidSocial),
        _ => None,
    }
}

// ---------- 渠道 ----------

/// 有序规则表，先匹配先赢。每一条的出处与 GA4 帮助页原文见研究文档 §3.3。
pub(crate) fn decide(src: &Source<'_>, utm: &Utm, click_id_param: Option<&str>) -> Channel {
    let medium = nonempty(utm.medium.as_deref()).unwrap_or("");
    let campaign = nonempty(utm.campaign.as_deref()).unwrap_or("");
    let utm_source = nonempty(utm.source.as_deref()).unwrap_or("");
    let name = src.name.as_ref();

    let paid_medium = is_paid_medium(medium);
    let paid = paid_medium || src.paid_alias;
    let shop_campaign = is_shopping_campaign(campaign);

    // R01 Direct：来源恰为 `(direct)` 且 medium 为 `(not set)`/`(none)`（或没有）。GA4 里是第一条。
    if eq_ci(name, "(direct)") && (medium.is_empty() || one_of_ci(medium, &["(not set)", "(none)"]))
    {
        return Channel::Direct;
    }
    // R02 Cross-network
    if contains_ci(campaign, "cross-network") {
        return Channel::CrossNetwork;
    }
    // R03–R06 付费：来源类别 + 付费证据
    if (src.kind == Kind::Shopping || shop_campaign) && paid {
        return Channel::PaidShopping;
    }
    if src.kind == Kind::Search && paid {
        return Channel::PaidSearch;
    }
    if src.kind == Kind::Social && paid {
        return Channel::PaidSocial;
    }
    if src.kind == Kind::Video && paid {
        return Channel::PaidVideo;
    }
    // R07 Display：只看 medium。在 Paid Search/Social/Video 之后，与 GA4 页面顺序一致。
    if one_of_ci(
        medium,
        &["display", "banner", "expandable", "interstitial", "cpm"],
    ) {
        return Channel::Display;
    }
    // R08 Paid Other：付费 medium 但没有命中上面任何一个来源类别
    if paid_medium {
        return Channel::PaidOther;
    }
    // R09 click id 付费推断（Rivu 追加，GA4 里对应 Google Ads 自动标记）
    if let Some(ch) = click_id_paid(src, click_id_param) {
        return ch;
    }
    // R10 Organic Shopping
    if src.kind == Kind::Shopping || shop_campaign {
        return Channel::OrganicShopping;
    }
    // R11 Organic Social
    if src.kind == Kind::Social
        || one_of_ci(
            medium,
            &[
                "social",
                "social-network",
                "social-media",
                "sm",
                "social network",
                "social media",
            ],
        )
    {
        return Channel::OrganicSocial;
    }
    // R12 Organic Video
    if src.kind == Kind::Video || medium_contains_video(medium) {
        return Channel::OrganicVideo;
    }
    // R13 AI Assistant：在 Organic Search 之前，已知 AI 来源不会被 `medium=organic` 抢走（偏离 GA4 页面顺序）
    if src.kind == Kind::Ai || eq_ci(medium, "ai-assistant") {
        return Channel::AiAssistant;
    }
    // R14 Organic Search
    if src.kind == Kind::Search || eq_ci(medium, "organic") {
        return Channel::OrganicSearch;
    }
    // R15 Email
    if src.kind == Kind::Email || mentions_email(utm_source) || mentions_email(medium) {
        return Channel::Email;
    }
    // R16 Affiliates
    if eq_ci(medium, "affiliate") {
        return Channel::Affiliates;
    }
    // R17 Audio
    if eq_ci(medium, "audio") {
        return Channel::Audio;
    }
    // R18 SMS
    if eq_ci(name, "sms") || eq_ci(medium, "sms") {
        return Channel::Sms;
    }
    // R19 Mobile Push Notifications
    if ends_with_ci(medium, "push")
        || contains_ci(medium, "mobile")
        || contains_ci(medium, "notification")
        || eq_ci(name, "firebase")
    {
        return Channel::MobilePushNotifications;
    }
    // R20 Referral：medium 为 referral/app/link，或者有来源（GA4 里 referrer 流量的 medium 恒为 referral）
    if one_of_ci(medium, &["referral", "app", "link"]) || !name.is_empty() {
        return Channel::Referral;
    }
    // R21 Direct
    Channel::Direct
}
