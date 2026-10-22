//! 渠道（acquisition channel）取值集合。
//!
//! 取值与顺序见 `docs/research/enrich-source.md` §3；英文标识沿用 Google Analytics 4
//! 默认渠道分组的官方名称，中文名取自 Google 官方中文帮助页的渠道名称。

/// 渠道。`as_str()` 是写入 ClickHouse 的稳定标识，**一旦发布不能改**；新增取值要加 `RULES_VERSION`。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Channel {
    Direct,
    CrossNetwork,
    PaidSearch,
    PaidSocial,
    PaidVideo,
    PaidShopping,
    PaidOther,
    Display,
    OrganicSearch,
    OrganicSocial,
    OrganicVideo,
    OrganicShopping,
    AiAssistant,
    Email,
    Affiliates,
    Audio,
    Sms,
    MobilePushNotifications,
    Referral,
}

impl Channel {
    /// 全部取值，按建议的展示顺序（直接和自然类在前，付费类在后）。**不是**规则的匹配顺序，
    /// 匹配顺序见 `rules.rs` 和研究文档 §3.3。
    pub const ALL: [Channel; 19] = [
        Channel::Direct,
        Channel::OrganicSearch,
        Channel::OrganicSocial,
        Channel::OrganicVideo,
        Channel::OrganicShopping,
        Channel::AiAssistant,
        Channel::Email,
        Channel::Referral,
        Channel::Affiliates,
        Channel::Audio,
        Channel::Sms,
        Channel::MobilePushNotifications,
        Channel::PaidSearch,
        Channel::PaidSocial,
        Channel::PaidVideo,
        Channel::PaidShopping,
        Channel::PaidOther,
        Channel::Display,
        Channel::CrossNetwork,
    ];

    /// 稳定的英文标识，也是 ClickHouse `channel` 列的值。
    pub const fn as_str(self) -> &'static str {
        match self {
            Channel::Direct => "Direct",
            Channel::CrossNetwork => "Cross-network",
            Channel::PaidSearch => "Paid Search",
            Channel::PaidSocial => "Paid Social",
            Channel::PaidVideo => "Paid Video",
            Channel::PaidShopping => "Paid Shopping",
            Channel::PaidOther => "Paid Other",
            Channel::Display => "Display",
            Channel::OrganicSearch => "Organic Search",
            Channel::OrganicSocial => "Organic Social",
            Channel::OrganicVideo => "Organic Video",
            Channel::OrganicShopping => "Organic Shopping",
            Channel::AiAssistant => "AI Assistant",
            Channel::Email => "Email",
            Channel::Affiliates => "Affiliates",
            Channel::Audio => "Audio",
            Channel::Sms => "SMS",
            Channel::MobilePushNotifications => "Mobile Push Notifications",
            Channel::Referral => "Referral",
        }
    }

    /// 中文名（Google Analytics 官方简体中文帮助页「默认渠道说明」表里的渠道名）。
    pub const fn zh_name(self) -> &'static str {
        match self {
            Channel::Direct => "直接",
            Channel::CrossNetwork => "跨广告网络",
            Channel::PaidSearch => "付费搜索",
            Channel::PaidSocial => "付费社交媒体广告",
            Channel::PaidVideo => "付费视频",
            Channel::PaidShopping => "付费购物",
            Channel::PaidOther => "其他付费渠道",
            Channel::Display => "展示广告",
            Channel::OrganicSearch => "自然搜索",
            Channel::OrganicSocial => "自然社交",
            Channel::OrganicVideo => "自然视频",
            Channel::OrganicShopping => "自然购物",
            Channel::AiAssistant => "AI 助理",
            Channel::Email => "邮件",
            Channel::Affiliates => "联属营销方",
            Channel::Audio => "音频",
            Channel::Sms => "短信",
            Channel::MobilePushNotifications => "移动推送通知",
            Channel::Referral => "引荐",
        }
    }

    /// `as_str()` 的逆运算，精确匹配（区分大小写）。查询层校验 `visit:channel` 过滤值时用。
    pub fn parse(s: &str) -> Option<Channel> {
        Channel::ALL.into_iter().find(|c| c.as_str() == s)
    }
}

impl std::fmt::Display for Channel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}
