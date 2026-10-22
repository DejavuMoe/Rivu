//! 接入链路各段共用的领域类型：协议解析的产物、会话属性、丢弃原因。
//!
//! 这里只放数据形状和枚举，不放逻辑；解析在 `rivu-ingest`，会话在 `rivu-session`，
//! 落库行结构在 `rivu-storage-ch`。

use std::fmt;

/// 事件种类。`pageview` 和 `engagement` 是系统事件名，其余都是自定义事件。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EventKind {
    Pageview,
    Engagement,
    Custom,
}

impl EventKind {
    pub fn from_name(name: &str) -> Self {
        match name {
            "pageview" => Self::Pageview,
            "engagement" => Self::Engagement,
            _ => Self::Custom,
        }
    }
}

/// 白名单内的 UTM 参数（`tracker-and-protocol.md` §7）。空串一律为 `None`。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Utm {
    pub source: Option<String>,
    pub medium: Option<String>,
    pub campaign: Option<String>,
    pub content: Option<String>,
    pub term: Option<String>,
}

/// 规范化后的 referrer。`host` 已按 hostname 规则规范化，用于判断是否为内部引荐；
/// `stored` 是落库形态：`host + path`，去结尾 `/`，无 query、无 fragment。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Referrer {
    pub host: String,
    pub stored: String,
}

/// `POST /api/event` 请求体经过解析和规范化之后的事件（`tracker-and-protocol.md` §5、§7）。
/// 不含时间戳和客户端上下文：它们来自 HTTP 层，不来自请求体。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NormalizedEvent {
    /// 线协议版本 `v`。
    pub protocol_version: u8,
    /// 脚本构建号 `sv`；非法或缺失为 0，只作计数标签。
    pub script_version: u32,
    pub site_key: String,
    /// trim 之后的事件名。
    pub name: String,
    pub kind: EventKind,
    pub hostname: String,
    pub pathname: String,
    pub referrer: Option<Referrer>,
    pub utm: Utm,
    /// 第一个值非空的 click id 参数的**名字**。
    pub click_id_param: Option<&'static str>,
    /// 保持请求体里的顺序，同名键后者覆盖。
    pub props: Vec<(String, String)>,
    pub interactive: bool,
    /// 0–100；255 表示未上报（只有 engagement 事件可能有值）。
    pub scroll_depth: u8,
    /// 毫秒；非 engagement 事件恒为 0。
    pub engagement_ms: u32,
}

/// 会话属性：会话的第一个事件决定，此后盖到会话内的每个事件上（`system-design.md` §4.6）。
/// 富化（来源分类、地理、UA）尚未实现的字段为空串，`country_code` 为 `[0, 0]`。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SessionAttrs {
    pub referrer: String,
    pub referrer_source: String,
    pub channel: String,
    pub utm_source: String,
    pub utm_medium: String,
    pub utm_campaign: String,
    pub utm_content: String,
    pub utm_term: String,
    pub click_id_param: String,
    pub country_code: [u8; 2],
    pub region_code: String,
    pub city_geoname_id: u32,
    pub device: String,
    pub browser: String,
    pub browser_version: String,
    pub os: String,
    pub os_version: String,
}

/// 事件被过滤的原因（`system-design.md` §4.2；标识符见 §10.1 Q13）。
///
/// 写入 `ingest_counters.metric` 时取 [`DropReason::metric`]，即 `dropped_<原因>`。
/// 原因只进计数，不出现在 HTTP 响应里。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum DropReason {
    UnknownSite,
    SiteDisabled,
    HostnameNotAllowed,
    RateLimited,
    SpamReferrer,
    IpBlocked,
    PageBlocked,
    Bot,
    DatacenterIp,
    CountryBlocked,
    NoSession,
    /// 事件的站点本地日期没有可用的盐（`platform-and-ops.md` §3.3：collector 不现场生成盐）。
    SaltMissing,
    Overloaded,
}

impl DropReason {
    pub const ALL: [DropReason; 13] = [
        Self::UnknownSite,
        Self::SiteDisabled,
        Self::HostnameNotAllowed,
        Self::RateLimited,
        Self::SpamReferrer,
        Self::IpBlocked,
        Self::PageBlocked,
        Self::Bot,
        Self::DatacenterIp,
        Self::CountryBlocked,
        Self::NoSession,
        Self::SaltMissing,
        Self::Overloaded,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::UnknownSite => "unknown_site",
            Self::SiteDisabled => "site_disabled",
            Self::HostnameNotAllowed => "hostname_not_allowed",
            Self::RateLimited => "rate_limited",
            Self::SpamReferrer => "spam_referrer",
            Self::IpBlocked => "ip_blocked",
            Self::PageBlocked => "page_blocked",
            Self::Bot => "bot",
            Self::DatacenterIp => "datacenter_ip",
            Self::CountryBlocked => "country_blocked",
            Self::NoSession => "no_session",
            Self::SaltMissing => "salt_missing",
            Self::Overloaded => "overloaded",
        }
    }

    /// `ingest_counters.metric` 的取值。
    pub fn metric(self) -> &'static str {
        match self {
            Self::UnknownSite => "dropped_unknown_site",
            Self::SiteDisabled => "dropped_site_disabled",
            Self::HostnameNotAllowed => "dropped_hostname_not_allowed",
            Self::RateLimited => "dropped_rate_limited",
            Self::SpamReferrer => "dropped_spam_referrer",
            Self::IpBlocked => "dropped_ip_blocked",
            Self::PageBlocked => "dropped_page_blocked",
            Self::Bot => "dropped_bot",
            Self::DatacenterIp => "dropped_datacenter_ip",
            Self::CountryBlocked => "dropped_country_blocked",
            Self::NoSession => "dropped_no_session",
            Self::SaltMissing => "dropped_salt_missing",
            Self::Overloaded => "dropped_overloaded",
        }
    }
}

impl fmt::Display for DropReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 被接受的事件在 `ingest_counters.metric` 里的取值。
pub const METRIC_ACCEPTED: &str = "accepted";

/// 未知站点在 `ingest_counters.site_id` 里的取值（§10.1 Q13）。PG 的 identity 从 1 开始，不会冲突。
pub const UNKNOWN_SITE_ID: u64 = 0;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metric_names_are_prefixed_reason_names() {
        for r in DropReason::ALL {
            assert_eq!(r.metric(), format!("dropped_{}", r.as_str()));
        }
    }

    #[test]
    fn event_kind_from_name() {
        assert_eq!(EventKind::from_name("pageview"), EventKind::Pageview);
        assert_eq!(EventKind::from_name("engagement"), EventKind::Engagement);
        assert_eq!(EventKind::from_name("Pageview"), EventKind::Custom);
        assert_eq!(EventKind::from_name("Signup"), EventKind::Custom);
    }
}
