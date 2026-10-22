//! 机器人识别规则（`system-design.md` §4.2 第 5 步、§4.4）。
//!
//! 来源有三层，按优先级：
//! 1. uap-core 的设备解析给出 `Spider`（[`BotKind::Spider`]）；
//! 2. 自维护的 Headless 规则（[`BotKind::Headless`]）；
//! 3. 自维护的自动化 / 脚本 HTTP 客户端 / 通用爬虫词规则（[`BotKind::Automation`]、[`BotKind::Spider`]）。
//!
//! 空 UA 的处理见 [`BotKind::Empty`]。
//!
//! 全部是子串 / 前缀比较，没有正则，所以对输入长度是线性的，也不存在回溯问题。
//! 输入是已经 ASCII 小写化并截断过的 UA（[`crate::MAX_UA_BYTES`]）。

/// 机器人的种类。管线只关心「是不是」（→ `DropReason::Bot`），种类留给统计和排错。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BotKind {
    /// 搜索 / 内容爬虫、链接预览、监控探针：uap-core 的 `Spider`，或命中自维护的爬虫规则。
    Spider,
    /// 无头浏览器（HeadlessChrome、PhantomJS 等）。Plausible 同样把 Headless Chrome 当机器人丢弃。
    Headless,
    /// 浏览器自动化框架与脚本 HTTP 客户端（Selenium、Puppeteer、curl、python-requests 等）。
    Automation,
    /// UA 为空（或只有空白）。
    ///
    /// 决定：**算机器人**。浏览器发出的 `fetch` / `sendBeacon` 请求一定带 `User-Agent` 头，
    /// 空 UA 只可能来自脚本或被剥掉头的转发链。这与 Plausible 不同（它不解析也不丢弃，
    /// 事件照常入库，user_id 里 UA 当 `""`，R01 §8.1），属于刻意偏离，要进「刻意偏离清单」。
    Empty,
}

impl BotKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Spider => "spider",
            Self::Headless => "headless",
            Self::Automation => "automation",
            Self::Empty => "empty",
        }
    }
}

/// 无头浏览器标记（子串）。
const HEADLESS: &[&str] = &["headless", "phantomjs", "slimerjs"];

/// 浏览器自动化框架标记（子串）。
const AUTOMATION_SUBSTR: &[&str] = &[
    "selenium",
    "webdriver",
    "chromedriver",
    "geckodriver",
    "puppeteer",
    "playwright",
    "cypress",
    "casperjs",
    "htmlunit",
    "jsdom",
];

/// 脚本 / 库 HTTP 客户端（前缀）。真浏览器的 UA 一律以 `Mozilla/` 或 `Opera/` 开头，
/// 这些客户端发请求时 UA 以自己的名字开头。
const HTTP_CLIENT_PREFIX: &[&str] = &[
    "curl/",
    "wget/",
    "python-requests",
    "python-urllib",
    "python-httpx",
    "python/",
    "aiohttp",
    "go-http-client",
    "java/",
    "apache-httpclient",
    "okhttp",
    "lwp-",
    "node-fetch",
    "node/",
    "axios",
    "undici",
    "got ",
    "scrapy",
    "postmanruntime",
    "insomnia",
    "httpie",
    "guzzlehttp",
    "ruby",
    "php/",
    "reqwest",
    "colly",
    "http.rb",
    "faraday",
    "restsharp",
    "winhttp",
    "powershell",
];

/// 不带 `bot` 字样、但确定是爬虫 / 抓取 / 预览 / 监控的名字（子串）。
///
/// 这张表是**自维护**的，增补时必须给出证据（访问日志里的 UA，或 crawler-user-agents 的条目）。
const SPIDER_NAMES: &[&str] = &[
    // 会执行 JS 的合成监控 / 体验测试 / 扫描器：它们真的会触发 tracker。
    "synthetics",
    "checkly",
    "dareboost",
    "cookiehubscan",
    "ruxitsynthetic",
    "catchpoint",
    "speedcurve",
    "webpagetest",
    "sitespeed",
    "google web preview",
    "google-site-verification",
    "google-ads",
    "google-safety",
    "meta-external",
    "nikto",
    "zgrab",
    "nmap",
    "w3c_",
    "w3c-",
    "w3c/",
    "libwww-perl",
    "spider",
    "crawl",
    "slurp",
    "scrape",
    "scrapy",
    "mediapartners-google",
    "adsbot",
    "facebookexternalhit",
    "facebookcatalog",
    "feedfetcher",
    "feedparser",
    "google-read-aloud",
    "google-inspectiontool",
    "googleother",
    "apis-google",
    "chrome-lighthouse",
    "pingdom",
    "uptime",
    "ia_archiver",
    "bingpreview",
    "yandexmetrika",
    "linkcheck",
    "w3c_validator",
    "w3c-checklink",
    "validator.nu",
    "whatsapp/",
    "skypeuripreview",
    "embedly",
    "wordpress/",
    "ahrefs",
    "semrush",
    "mj12",
    "dotbot",
    "petalbot",
    "bytespider",
    "gptbot",
    "ccbot",
    "claudebot",
    "perplexity",
    "ahc/",
    "archive.org",
    "wayback",
    "heritrix",
    "nutch",
    "httrack",
    "teleport pro",
    "webcopier",
    "offline explorer",
];

/// 在小写 UA 里找 `bot` 作为一个词的结尾（`googlebot`、`bot/1.0`、`bot;`、`bot)`），
/// 后面紧跟字母的不算（`botanical`、`bottle`、`abbott`）。
///
/// 已知例外：手机品牌 Cubot（`Cubot X19`）以 `bot` 结尾，单独排除。
fn has_bot_word(lower: &str) -> bool {
    let bytes = lower.as_bytes();
    let mut from = 0;
    while let Some(i) = lower[from..].find("bot") {
        let start = from + i;
        let end = start + 3;
        let next_is_letter = bytes.get(end).is_some_and(|b| b.is_ascii_lowercase());
        let is_cubot = lower[..start].ends_with("cu");
        if !next_is_letter && !is_cubot {
            return true;
        }
        from = end;
    }
    false
}

/// `@` 后面跟着带字母顶级域的域名（`support@admantx.com`）。`android@119`（Ecosia）、`Build/ICS@ROM`、
/// `th-TH@calendar=gregorian`、`@TAB-NINE` 这类真浏览器 UA 里的 `@` 都不满足。
fn has_email(lower: &str) -> bool {
    let b = lower.as_bytes();
    lower.match_indices('@').any(|(i, _)| {
        let domain: &str = {
            let rest = &lower[i + 1..];
            let end = rest
                .find(|c: char| !(c.is_ascii_alphanumeric() || c == '.' || c == '-'))
                .unwrap_or(rest.len());
            &rest[..end]
        };
        let tld = domain.rsplit('.').next().unwrap_or("");
        i > 0
            && b[i - 1].is_ascii_alphanumeric()
            && domain.contains('.')
            && tld.len() >= 2
            && tld.bytes().all(|c| c.is_ascii_lowercase())
    })
}

/// 爬虫的「自报家门」惯例：UA 里带联系 URL 或邮箱。
///
/// - `+http` / `mailto:` / 邮箱（含 `@`）：Googlebot、Bingbot 等通行格式。
/// - 不以 `mozilla/`、`opera/` 开头（所有现代浏览器和 WebView 都以其一开头）又带 URL（`http://`、`https://`、`www.`）。
///
/// 不认「任意 UA 含 URL」：试验过，会误伤把论坛网址写进 UA 的定制 ROM（uap-core 语料里就有）；
/// 也试验过「`compatible;` 但不是 IE」，误伤 Konqueror、NetFront、Polaris 等老浏览器和功能机，已放弃。
fn advertises_identity(lower: &str) -> bool {
    if lower.contains("+http") || lower.contains("mailto:") || has_email(lower) {
        return true;
    }
    let browser_prefix = lower.starts_with("mozilla/") || lower.starts_with("opera/");
    !browser_prefix
        && (lower.contains("http://") || lower.contains("https://") || lower.contains("www."))
}

/// 对小写 UA 做机器人判定。`spider_device` 为 uap-core 设备解析是否给出 `Spider`。
pub(crate) fn detect(lower: &str, spider_device: bool) -> Option<BotKind> {
    if spider_device {
        return Some(BotKind::Spider);
    }
    if HEADLESS.iter().any(|t| lower.contains(t)) {
        return Some(BotKind::Headless);
    }
    if AUTOMATION_SUBSTR.iter().any(|t| lower.contains(t))
        || HTTP_CLIENT_PREFIX.iter().any(|p| lower.starts_with(p))
    {
        return Some(BotKind::Automation);
    }
    if has_bot_word(lower)
        || SPIDER_NAMES.iter().any(|t| lower.contains(t))
        || advertises_identity(lower)
    {
        return Some(BotKind::Spider);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bot_word_boundaries() {
        assert!(has_bot_word("mozilla/5.0 (compatible; googlebot/2.1)"));
        assert!(has_bot_word("bot/1.0"));
        assert!(has_bot_word("foo bot"));
        assert!(!has_bot_word("mozilla/5.0 botanical"));
        assert!(!has_bot_word("an abbott phone"));
        assert!(!has_bot_word("android 9; cubot x19 build/pq3b"));
        // 例外只吃掉 cubot 那一处，同一 UA 里的其它 bot 词仍然命中。
        assert!(has_bot_word("cubot x19; somebot/1"));
    }

    #[test]
    fn kinds() {
        assert_eq!(detect("x", true), Some(BotKind::Spider));
        assert_eq!(
            detect("mozilla/5.0 headlesschrome/120", false),
            Some(BotKind::Headless)
        );
        assert_eq!(detect("curl/8.5.0", false), Some(BotKind::Automation));
        assert_eq!(
            detect("mozilla/5.0 (compatible; bingbot/2.0)", false),
            Some(BotKind::Spider)
        );
        assert_eq!(
            detect("mozilla/5.0 (windows nt 10.0) chrome/120", false),
            None
        );
    }
}
