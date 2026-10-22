//! 公开 API 的行为测试：版本截断、空 UA、超长 UA、缓存、设备类别、UA 精简与冻结、机器人。
//!
//! 样例 UA 是真实浏览器发出的字符串（Chrome 精简格式见 chromium.org/updates/ua-reduction，
//! iPadOS 桌面 UA 见 Apple 开发者论坛 thread 119186，Safari 26 冻结见 WebKit bug 298260）。

use std::sync::Arc;
use std::time::{Duration, Instant};

use rivu_ua::{BotKind, DeviceClass, MAX_UA_BYTES, UaParser};

fn parser() -> UaParser {
    UaParser::embedded().unwrap()
}

const CHROME_WIN: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.6099.109 Safari/537.36";
const CHROME_ANDROID_REDUCED: &str = "Mozilla/5.0 (Linux; Android 10; K) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0.0.0 Mobile Safari/537.36";
const CHROME_ANDROID_TABLET_REDUCED: &str = "Mozilla/5.0 (Linux; Android 10; K) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36";
const SAFARI_IPHONE: &str = "Mozilla/5.0 (iPhone; CPU iPhone OS 17_5 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.5 Mobile/15E148 Safari/604.1";
const SAFARI_IPHONE_26: &str = "Mozilla/5.0 (iPhone; CPU iPhone OS 18_6 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/26.0 Mobile/15E148 Safari/604.1";
const SAFARI_IPAD_MOBILE: &str = "Mozilla/5.0 (iPad; CPU OS 17_5 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.5 Mobile/15E148 Safari/604.1";
/// iPadOS 13+ 默认的「桌面网站」UA：与 Mac Safari 不可区分。
const SAFARI_IPADOS_DESKTOP: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.5 Safari/605.1.15";
const FIREFOX_LINUX: &str =
    "Mozilla/5.0 (X11; Linux x86_64; rv:109.0) Gecko/20100101 Firefox/115.0";

fn info(ua: &str) -> rivu_ua::UaInfo {
    parser().parse_uncached(ua)
}

#[test]
fn versions_are_cut_to_major_minor() {
    let i = info(CHROME_WIN);
    assert_eq!(
        (i.browser.as_str(), i.browser_version.as_str()),
        ("Chrome", "120.0")
    );
    assert_eq!((i.os.as_str(), i.os_version.as_str()), ("Windows", "10"));
    assert_eq!(i.device, DeviceClass::Desktop);
    assert_eq!(i.bot, None);

    let i = info(FIREFOX_LINUX);
    assert_eq!(
        (i.browser.as_str(), i.browser_version.as_str()),
        ("Firefox", "115.0")
    );
    assert_eq!(i.os, "Linux");
    assert_eq!(i.os_version, "");

    let i = info(SAFARI_IPHONE);
    assert_eq!(
        (i.browser.as_str(), i.browser_version.as_str()),
        ("Safari", "17.5")
    );
    assert_eq!((i.os.as_str(), i.os_version.as_str()), ("iOS", "17.5"));
    assert_eq!(i.device, DeviceClass::Mobile);
}

#[test]
fn unknown_ua_is_empty_fields_desktop_not_bot() {
    let i = info("Mozilla/5.0");
    assert_eq!(i.browser, "");
    assert_eq!(i.os, "");
    assert_eq!(i.device, DeviceClass::Desktop);
    assert_eq!(i.bot, None);
}

#[test]
fn empty_ua_is_a_bot_of_kind_empty() {
    let p = parser();
    for ua in ["", " ", "\t\r\n "] {
        let i = p.parse(ua);
        assert_eq!(i.bot, Some(BotKind::Empty), "{ua:?}");
        assert!(i.is_bot());
    }
    // 空 UA 共享同一个结果，不进缓存。
    assert!(Arc::ptr_eq(&p.parse(""), &p.parse("  ")));
    assert_eq!(p.cache_entry_count(), 0);
}

#[test]
fn over_long_ua_is_truncated_before_parsing() {
    let p = parser();
    // 1 MB 的垃圾、后面藏着机器人字样：截断后不可见，也不会花一秒钟去匹配。
    let junk = format!("{CHROME_WIN} {} Googlebot/2.1", "A".repeat(1 << 20));
    let t = Instant::now();
    let got = p.parse_uncached(&junk);
    let elapsed = t.elapsed();
    assert_eq!(got, p.parse_uncached(&junk[..MAX_UA_BYTES]));
    assert_eq!(got.browser, "Chrome");
    assert!(
        elapsed < Duration::from_secs(1),
        "1 MB UA 解析耗时 {elapsed:?}"
    );

    // 经典的回溯炸弹形状：重复的括号、空格、分号。`regex` 保证线性时间，加上截断，耗时有上界。
    for bomb in [
        format!("Mozilla/5.0 ({}", "(".repeat(200_000)),
        format!("Mozilla/5.0 (Linux; Android {}", "; ".repeat(100_000)),
        format!("{}!", "a/1.".repeat(100_000)),
        " ".repeat(200_000) + "x",
    ] {
        let t = Instant::now();
        let _ = p.parse_uncached(&bomb);
        assert!(t.elapsed() < Duration::from_secs(1), "{:?}", t.elapsed());
    }

    // 截断落在多字节字符中间：不能 panic。
    let wide = "中".repeat(MAX_UA_BYTES); // 3 字节一个，边界必然落在字符中间
    let _ = p.parse(&wide);
    let _ = p.parse(&format!("a{wide}"));
}

#[test]
fn cache_hit_returns_the_same_arc() {
    let p = parser();
    let a = p.parse(CHROME_WIN);
    let b = p.parse(CHROME_WIN);
    assert!(Arc::ptr_eq(&a, &b));
    // 首尾空白不产生新条目。
    let c = p.parse(&format!("  {CHROME_WIN}\n"));
    assert!(Arc::ptr_eq(&a, &c));
    // 超长 UA 的缓存键是截断后的字符串：两个只在尾部不同的超长 UA 命中同一条。
    let long1 = format!("{CHROME_WIN} {}1", "x".repeat(MAX_UA_BYTES));
    let long2 = format!("{CHROME_WIN} {}2", "x".repeat(MAX_UA_BYTES));
    assert!(Arc::ptr_eq(&p.parse(&long1), &p.parse(&long2)));
}

#[test]
fn cache_entries_expire_after_ttl() {
    let p = UaParser::with_cache(100, Duration::from_millis(50)).unwrap();
    let a = p.parse(CHROME_WIN);
    std::thread::sleep(Duration::from_millis(120));
    let b = p.parse(CHROME_WIN);
    assert!(!Arc::ptr_eq(&a, &b), "TTL 过后应重新解析");
    assert_eq!(*a, *b);
}

#[test]
fn cache_is_bounded() {
    let p = UaParser::with_cache(50, Duration::from_secs(3600)).unwrap();
    for i in 0..2000 {
        p.parse(&format!(
            "Mozilla/5.0 (X11; Linux x86_64) Gecko/20100101 Firefox/{i}.0"
        ));
    }
    // moka 的淘汰是异步结算的，但不会无限增长。
    assert!(p.cache_entry_count() <= 200, "{}", p.cache_entry_count());
}

#[test]
fn device_class_rules() {
    use DeviceClass::*;
    let cases: &[(&str, DeviceClass)] = &[
        (CHROME_WIN, Desktop),
        (FIREFOX_LINUX, Desktop),
        (SAFARI_IPHONE, Mobile),
        (SAFARI_IPAD_MOBILE, Tablet),
        // iPadOS 默认发桌面 UA：服务端无法识别为平板（已知局限）。
        (SAFARI_IPADOS_DESKTOP, Desktop),
        (CHROME_ANDROID_REDUCED, Mobile),
        (CHROME_ANDROID_TABLET_REDUCED, Tablet),
        (
            "Mozilla/5.0 (Android 14; Mobile; rv:128.0) Gecko/128.0 Firefox/128.0",
            Mobile,
        ),
        (
            "Mozilla/5.0 (Android 14; Tablet; rv:128.0) Gecko/128.0 Firefox/128.0",
            Tablet,
        ),
        (
            "Mozilla/5.0 (Linux; Android 13; SM-S918B) AppleWebKit/537.36 (KHTML, like Gecko) SamsungBrowser/23.0 Chrome/115.0.0.0 Mobile Safari/537.36",
            Mobile,
        ),
        (
            "Mozilla/5.0 (Linux; Android 13; SM-X710) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36",
            Tablet,
        ),
        (
            "Mozilla/5.0 (Linux; Android 9; BRAVIA 4K GB Build/PTT1.190515.001.S38) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/114.0.0.0 Safari/537.36",
            Desktop,
        ),
        (
            "Mozilla/5.0 (X11; CrOS x86_64 14541.0.0) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36",
            Desktop,
        ),
        (
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36",
            Desktop,
        ),
        (
            "Mozilla/5.0 (Windows Phone 10.0; Android 4.2.1; Microsoft; Lumia 950) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/46.0.2486.0 Mobile Safari/537.36 Edge/13.10586",
            Mobile,
        ),
        (
            "Mozilla/5.0 (iPhone; CPU iPhone OS 17_5 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) CriOS/126.0.6478.153 Mobile/15E148 Safari/604.1",
            Mobile,
        ),
        (
            "Mozilla/5.0 (iPad; CPU OS 17_5 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) CriOS/126.0.6478.153 Mobile/15E148 Safari/604.1",
            Tablet,
        ),
        (
            "Opera/9.80 (Android; Opera Mini/36.2.2254/119.132; U; id) Presto/2.12.423 Version/12.16",
            Mobile,
        ),
    ];
    let p = parser();
    for (ua, want) in cases {
        assert_eq!(p.parse_uncached(ua).device, *want, "{ua}");
    }
}

#[test]
fn ua_reduction_and_frozen_versions() {
    // Windows：NT 10.0 是 10 或 11，保留为 "10"。
    assert_eq!(info(CHROME_WIN).os_version, "10");

    // macOS：Chrome / Safari / Firefox 都冻结在 10.15，不携带信息 → 空。
    let mac_chrome = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36";
    let mac_firefox =
        "Mozilla/5.0 (Macintosh; Intel Mac OS X 10.15; rv:128.0) Gecko/20100101 Firefox/128.0";
    for ua in [mac_chrome, mac_firefox, SAFARI_IPADOS_DESKTOP] {
        let i = info(ua);
        assert_eq!(i.os, "macOS", "{ua}");
        assert_eq!(i.os_version, "", "{ua}");
    }
    // 真实的老版本 macOS 不受影响。
    let old = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_13_6) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/12.0 Safari/605.1.15";
    assert_eq!(info(old).os_version, "10.13");

    // Android：`Android 10; K)` 是精简形状，版本不可信 → 空；带真实型号的 Android 10 保留。
    assert_eq!(info(CHROME_ANDROID_REDUCED).os, "Android");
    assert_eq!(info(CHROME_ANDROID_REDUCED).os_version, "");
    let real10 = "Mozilla/5.0 (Linux; Android 10; SM-G960F) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/99.0.4844.73 Mobile Safari/537.36";
    assert_eq!(info(real10).os_version, "10");

    // iOS Safari 26+：UA 里的 18_6 是冻结值，用 Version/26.0。Chrome iOS 不受影响。
    assert_eq!(info(SAFARI_IPHONE_26).os_version, "26.0");
    let cri = "Mozilla/5.0 (iPhone; CPU iPhone OS 18_5 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) CriOS/138.0.7204.119 Mobile/15E148 Safari/604.1";
    assert_eq!(info(cri).os_version, "18.5");
    // iOS Safari 18 真实的 18.6 不被改写。
    let s18 = "Mozilla/5.0 (iPhone; CPU iPhone OS 18_6 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.6 Mobile/15E148 Safari/604.1";
    assert_eq!(info(s18).os_version, "18.6");

    // ChromeOS：精简后平台版本固定为 14541.0.0。
    let cros = "Mozilla/5.0 (X11; CrOS x86_64 14541.0.0) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36";
    assert_eq!(info(cros).os, "Chrome OS");
    assert_eq!(info(cros).os_version, "");
}

#[test]
fn browser_names_are_normalized() {
    let names = |ua: &str| info(ua).browser;
    assert_eq!(names(CHROME_ANDROID_REDUCED), "Chrome");
    assert_eq!(
        names(
            "Mozilla/5.0 (iPhone; CPU iPhone OS 17_5 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) CriOS/126.0.6478.153 Mobile/15E148 Safari/604.1"
        ),
        "Chrome"
    );
    assert_eq!(names(SAFARI_IPHONE), "Safari");
    assert_eq!(
        names("Mozilla/5.0 (Android 14; Mobile; rv:128.0) Gecko/128.0 Firefox/128.0"),
        "Firefox"
    );
    assert_eq!(
        names(
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36 Edg/120.0.2210.91"
        ),
        "Edge"
    );
}

#[test]
fn bots() {
    let p = parser();
    let bot = |ua: &str| p.parse_uncached(ua).bot;
    assert_eq!(
        bot("Mozilla/5.0 (compatible; Googlebot/2.1; +http://www.google.com/bot.html)"),
        Some(BotKind::Spider)
    );
    assert_eq!(
        bot(
            "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) HeadlessChrome/120.0.0.0 Safari/537.36"
        ),
        Some(BotKind::Headless)
    );
    assert_eq!(bot("curl/8.5.0"), Some(BotKind::Automation));
    assert_eq!(bot("python-requests/2.31.0"), Some(BotKind::Automation));
    assert_eq!(
        bot(
            "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36 Selenium"
        ),
        Some(BotKind::Automation)
    );
    // 真浏览器与「名字里恰好带 bot / w3c 字样」的设备不误判。
    for ua in [
        CHROME_WIN,
        SAFARI_IPHONE,
        FIREFOX_LINUX,
        CHROME_ANDROID_REDUCED,
        "Mozilla/5.0 (Linux; Android 9; Cubot X19 Build/PQ3B.190801.002) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/80.0.3987.162 Mobile Safari/537.36",
        "Mozilla/5.0 (Linux; U; Android 4.0.4; de-de; ALLVIEW3CONECT Build/IMM76D) AppleWebKit/534.30 (KHTML, like Gecko) Version/4.0 Safari/534.30",
    ] {
        assert_eq!(bot(ua), None, "{ua}");
    }
}

/// `bot` 字段与 `UaInfo::is_bot` 的一致性，以及 `BotKind::as_str` / `DeviceClass::as_str` 的取值
/// （`device` 直接写进 `SessionAttrs.device` 和 ClickHouse，取值是对外契约）。
#[test]
fn string_forms_are_stable() {
    assert_eq!(DeviceClass::Desktop.as_str(), "desktop");
    assert_eq!(DeviceClass::Mobile.as_str(), "mobile");
    assert_eq!(DeviceClass::Tablet.as_str(), "tablet");
    assert_eq!(BotKind::Spider.as_str(), "spider");
    assert_eq!(BotKind::Headless.as_str(), "headless");
    assert_eq!(BotKind::Automation.as_str(), "automation");
    assert_eq!(BotKind::Empty.as_str(), "empty");
}
