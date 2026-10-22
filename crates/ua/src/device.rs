//! 设备类别映射，以及浏览器名、系统名、系统版本的归一化。
//!
//! 全部是纯函数，输入是 uap-core 的原始解析结果（[`crate::RawUa`]）和小写化的 UA。

/// 设备类别，直接对应 `SessionAttrs.device`（`as_str`）。
///
/// 只有三类：`system-design.md` 的「设备类别」维度没有「电视」「游戏机」「未知」。
/// 电视、游戏机、车机、未知设备都归入 [`DeviceClass::Desktop`]：这与 Plausible 对 tv / console 的
/// 做法一致（R01 §8.1），手机和平板需要**肯定的证据**，没有证据就不报。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DeviceClass {
    Desktop,
    Mobile,
    Tablet,
}

impl DeviceClass {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Desktop => "desktop",
            Self::Mobile => "mobile",
            Self::Tablet => "tablet",
        }
    }
}

/// 电视、机顶盒、投屏、游戏机：Android 系统但不是手机或平板。
const TV_TOKENS: &[&str] = &[
    "android tv",
    "smart-tv",
    "smarttv",
    "googletv",
    "google tv",
    "crkey", // Chromecast
    "hbbtv",
    "bravia",
    "aftb", // Fire TV 系列型号前缀 AFT*
    "afts",
    "aftm",
    "aftt",
    "aftn",
    "playstation",
    "xbox",
    "nintendo",
];

/// 系统本身就说明是手机的 uap 系统名（无需看 `Mobile` 记号）。
const MOBILE_OS: &[&str] = &[
    "windows phone",
    "windows mobile",
    "blackberry os",
    "symbian",
    "kaios",
    "bada",
    "meego",
    "maemo",
    "series 40",
];

/// 设备类别规则表，**按顺序**，第一条命中即返回。
///
/// | # | 条件 | 结果 | 依据 |
/// |---|---|---|---|
/// | 1 | UA 含 `ipad`，或 uap 设备族为 `iPad` / `Generic Tablet` | Tablet | iPad 在「移动版网站」模式下发 `iPad;` |
/// | 2 | 系统为 iOS，或 UA 含 `iphone` / `ipod` | Mobile | |
/// | 3 | uap 设备族为 `Generic Smartphone` / `Generic Feature Phone` | Mobile | uap-core 对未知机型的兜底分类 |
/// | 4 | 系统为 Android，且 UA 含 TV / 投屏 / 机顶盒 / 游戏机记号 | Desktop | Plausible 对 tv/console 同样不归手机 |
/// | 5 | 系统为 Android，UA 含 `tablet` | Tablet | Firefox 平板：`Android; Tablet;` |
/// | 6 | 系统为 Android，UA 含 `mobile`（`Mobile Safari`、`Mobile;`）或 `opera mini` / `opera mobi` | Mobile | Chrome 官方约定：手机带 `Mobile`，平板不带 |
/// | 7 | 系统为 Android，其余 | Tablet | 同上；UA 精简后型号恒为 `K`，只能靠这条 |
/// | 8 | 系统在 [`MOBILE_OS`] 里 | Mobile | |
/// | 9 | UA 含 `tablet` / `playbook` / `kindle` | Tablet | BlackBerry PlayBook、Kindle |
/// | 10 | UA 含 `mobile`（`IEMobile`、`Opera Mobi`、`Mobile/`） | Mobile | |
/// | 11 | 其余（Windows / macOS / Linux / ChromeOS / 电视 / 未知） | Desktop | |
///
/// 不使用 uap 的 brand / model 去猜平板：型号表只有 `SM-T510` 这类零散条目，覆盖不全，
/// 而且 UA 精简之后 Android 的型号恒为 `K`；`Mobile` 记号在精简前后都保留，才是可靠信号。
///
/// 已知局限（写进研究文档）：iPadOS 13+ 的 Safari 默认「请求桌面网站」，UA 是 `Macintosh`，
/// 服务端无法与 Mac 区分，会被归为 Desktop。Android Chrome 的「桌面版网站」同理（UA 是 `X11; Linux`）。
pub(crate) fn classify(lower: &str, os_family: &str, device_family: &str) -> DeviceClass {
    if lower.contains("ipad") || device_family == "iPad" || device_family == "Generic Tablet" {
        return DeviceClass::Tablet;
    }
    if os_family == "iOS" || lower.contains("iphone") || lower.contains("ipod") {
        return DeviceClass::Mobile;
    }
    if device_family == "Generic Smartphone" || device_family == "Generic Feature Phone" {
        return DeviceClass::Mobile;
    }
    if os_family == "Android" {
        if TV_TOKENS.iter().any(|t| lower.contains(t)) {
            return DeviceClass::Desktop;
        }
        if lower.contains("tablet") {
            return DeviceClass::Tablet;
        }
        if lower.contains("mobile") || lower.contains("opera mini") {
            return DeviceClass::Mobile;
        }
        return DeviceClass::Tablet;
    }
    let os_lower = os_family.to_ascii_lowercase();
    if MOBILE_OS.iter().any(|m| os_lower.contains(m)) {
        return DeviceClass::Mobile;
    }
    if lower.contains("tablet") || lower.contains("playbook") || lower.contains("kindle") {
        return DeviceClass::Tablet;
    }
    if lower.contains("mobile") {
        return DeviceClass::Mobile;
    }
    DeviceClass::Desktop
}

/// 浏览器名归一化：把「移动版 / iOS 版」并回主品牌，设备维度已经区分了手机和桌面。
///
/// 与 Plausible 的归一化表同类（R01 §8.1），但把 WebView 单列而不是并进 Mobile App：
/// WebView 是 App 内置页面，不是用户选的浏览器，看板上单独成行更诚实。
/// 未知（uap 的 `Other`）返回空串。
pub(crate) fn normalize_browser(family: &str) -> &str {
    match family {
        "Other" => "",
        "Chrome Mobile" | "Chrome Mobile iOS" => "Chrome",
        "Mobile Safari" => "Safari",
        "Firefox Mobile" | "Firefox iOS" => "Firefox",
        "Opera Mobile" | "Opera Mini" => "Opera",
        "Edge Mobile" => "Edge",
        "IE Mobile" => "IE",
        "Yandex Browser Lite" => "Yandex Browser",
        "Android" => "Android Browser",
        "Chrome Mobile WebView" | "Mobile Safari UI/WKWebView" => "WebView",
        other => other,
    }
}

/// 系统名归一化。`Mac OS X` 在 2020 年后早已不叫这个名字，统一为 `macOS`。未知返回空串。
pub(crate) fn normalize_os(family: &str) -> &str {
    match family {
        "Other" => "",
        "Mac OS X" => "macOS",
        other => other,
    }
}

/// 取版本的前两段（`major.minor`）。缺 major 则为空串，缺 minor 则只有 major。
///
/// 每段限长 [`MAX_VERSION_PART`]：uap 的个别规则会把任意字符串捕获为版本，攻击者可以借此撑大
/// 维度基数。
pub(crate) fn major_minor(major: Option<&str>, minor: Option<&str>) -> String {
    const MAX_VERSION_PART: usize = 16;
    let ok = |s: &&str| !s.is_empty() && s.len() <= MAX_VERSION_PART;
    let Some(major) = major.filter(ok) else {
        return String::new();
    };
    match minor.filter(ok) {
        Some(minor) => format!("{major}.{minor}"),
        None => major.to_owned(),
    }
}

/// 经过 UA 精简 / 冻结之后，系统版本是否**不再携带信息**。是则 `os_version` 报空串。
///
/// - macOS：Chrome（90+）、Safari、Firefox 都把版本冻结在 `10_15_7`（`10.15`）。
/// - Android：Chrome 110+ 精简为 `Android 10; K`，版本恒为 10、型号恒为 `K`。
///   旧版 Android 10 的 UA 一定带真实型号（`SM-G960F` 等），所以 `10; K)` 这个形状可以放心当作「已精简」。
/// - ChromeOS：精简后平台版本固定为 `14541.0.0`。
///
/// Windows 的 `NT 10.0` 不在此列：它仍然是正确的下限（Windows 10 或 11），保留为 `10`，
/// 文档与看板需注明「10」代表 10 / 11（只有 Client Hints 的 `Sec-CH-UA-Platform-Version` 能区分，
/// 且要 JS 主动请求高熵值，tracker 目前不发送）。
pub(crate) fn os_version_is_frozen(lower: &str, os_family: &str, major_minor: &str) -> bool {
    match os_family {
        "Mac OS X" => major_minor == "10.15",
        "Android" => lower.contains("android 10; k)"),
        "Chrome OS" => true,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_truncation() {
        assert_eq!(major_minor(Some("17"), Some("5")), "17.5");
        assert_eq!(major_minor(Some("17"), None), "17");
        assert_eq!(major_minor(Some("17"), Some("")), "17");
        assert_eq!(major_minor(None, Some("5")), "");
        assert_eq!(major_minor(Some(""), Some("5")), "");
        assert_eq!(major_minor(Some("12345678901234567"), Some("1")), "");
    }

    #[test]
    fn names() {
        assert_eq!(normalize_browser("Chrome Mobile iOS"), "Chrome");
        assert_eq!(normalize_browser("Other"), "");
        assert_eq!(normalize_browser("Samsung Internet"), "Samsung Internet");
        assert_eq!(normalize_os("Mac OS X"), "macOS");
    }
}
