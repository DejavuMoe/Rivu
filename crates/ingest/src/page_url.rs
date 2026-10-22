//! `u` 字段的 URL 规范化（`tracker-and-protocol.md` §7：URL 解析、hostname、pathname、
//! hash 模式、query 白名单、click id）。纯函数，供 [`crate::protocol`] 使用。

use std::borrow::Cow;

use rivu_domain::Utm;
use url::{Url, form_urlencoded};

use crate::hostname::normalize_hostname;

/// pathname 的字节上限（含 hash 模式追加的部分）。
pub(crate) const MAX_PATHNAME_BYTES: usize = 1024;
/// UTM / 来源参数值的字符上限。
const MAX_PARAM_CHARS: usize = 255;

/// click id 参数名，按优先级排列（前 6 项同 Plausible，后 2 项为 Rivu 追加）。
const CLICK_ID_PARAMS: [&str; 8] = [
    "gclid",
    "gbraid",
    "wbraid",
    "msclkid",
    "fbclid",
    "twclid",
    "ttclid",
    "li_fat_id",
];

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct PageUrl {
    pub hostname: String,
    pub pathname: String,
    pub utm: Utm,
    pub click_id_param: Option<&'static str>,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum UrlError {
    /// 无法解析，或没有 host。
    Invalid,
    /// scheme 不是 http / https。
    SchemeNotAllowed,
}

pub(crate) fn truncate_chars(s: &str, max: usize) -> &str {
    match s.char_indices().nth(max) {
        Some((i, _)) => &s[..i],
        None => s,
    }
}

/// 按字节截断，落在字符边界上（向前退）。
pub(crate) fn truncate_bytes(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// 安全解码：按字节解码 `%XX`，但保留 `%2F %5C %3F %23 %25` 与控制字符（<0x20、0x7F）的转义形态
///（原样保留，不改大小写）。出现非法转义或解码结果不是 UTF-8 时，整串原样返回，不报错。
/// 结果不会比输入长。
pub(crate) fn safe_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'%' {
            out.push(bytes[i]);
            i += 1;
            continue;
        }
        let hi = bytes.get(i + 1).copied().and_then(hex_val);
        let lo = bytes.get(i + 2).copied().and_then(hex_val);
        let (Some(hi), Some(lo)) = (hi, lo) else {
            return s.to_owned();
        };
        let b = (hi << 4) | lo;
        if matches!(b, b'/' | b'\\' | b'?' | b'#' | b'%') || b < 0x20 || b == 0x7F {
            out.extend_from_slice(&bytes[i..i + 3]);
        } else {
            out.push(b);
        }
        i += 3;
    }
    String::from_utf8(out).unwrap_or_else(|_| s.to_owned())
}

/// query 参数值的清理：去控制字符 → trim → 截断到 255 字符。结果为空即「缺失」。
fn clean_param(v: &str) -> String {
    let no_ctrl: String = v.chars().filter(|c| !c.is_control()).collect();
    truncate_chars(no_ctrl.trim(), MAX_PARAM_CHARS).to_owned()
}

/// 在若干组参数里取 `key` 的第一个清理后非空的值。靠前的组优先级更高；空串即缺失，不压过后面的取值。
fn first_value(pairs: &[(Cow<'_, str>, Cow<'_, str>)], key: &str) -> Option<String> {
    pairs
        .iter()
        .filter(|(k, _)| k == key)
        .map(|(_, v)| clean_param(v))
        .find(|v| !v.is_empty())
}

/// 解析并规范化页面 URL。`hash_mode` 即请求里的 `h == 1`。
pub(crate) fn parse_page_url(raw: &str, hash_mode: bool) -> Result<PageUrl, UrlError> {
    let url = Url::parse(raw).map_err(|_| UrlError::Invalid)?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(UrlError::SchemeNotAllowed);
    }
    let hostname = normalize_hostname(url.host_str().ok_or(UrlError::Invalid)?);
    if hostname.is_empty() {
        return Err(UrlError::Invalid);
    }

    let mut pathname = safe_decode(if url.path().is_empty() {
        "/"
    } else {
        url.path()
    });

    // 真实 query 在前，hash 里截去的 query 在后（优先级更低）。
    let mut pairs: Vec<(Cow<'_, str>, Cow<'_, str>)> = url.query_pairs().collect();
    if hash_mode && let Some(fragment) = url.fragment().filter(|f| !f.is_empty()) {
        let (route, hash_query) = match fragment.split_once('?') {
            Some((r, q)) => (r, Some(q)),
            None => (fragment, None),
        };
        let looks_like_token =
            !(route.starts_with('/') || route.starts_with("!/")) && route.contains('=');
        if !looks_like_token && !route.is_empty() {
            pathname.push('#');
            pathname.push_str(&safe_decode(route));
        }
        if let Some(q) = hash_query {
            pairs.extend(form_urlencoded::parse(q.as_bytes()));
        }
    }
    let pathname = truncate_bytes(&pathname, MAX_PATHNAME_BYTES).to_owned();

    let utm = Utm {
        source: first_value(&pairs, "utm_source")
            .or_else(|| first_value(&pairs, "source"))
            .or_else(|| first_value(&pairs, "ref")),
        medium: first_value(&pairs, "utm_medium"),
        campaign: first_value(&pairs, "utm_campaign"),
        content: first_value(&pairs, "utm_content"),
        term: first_value(&pairs, "utm_term"),
    };
    let click_id_param = CLICK_ID_PARAMS
        .into_iter()
        .find(|name| first_value(&pairs, name).is_some());

    Ok(PageUrl {
        hostname,
        pathname,
        utm,
        click_id_param,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page(u: &str) -> PageUrl {
        parse_page_url(u, false).unwrap()
    }

    fn hashed(u: &str) -> PageUrl {
        parse_page_url(u, true).unwrap()
    }

    // ---- URL 解析与 scheme ----

    #[test]
    fn rejects_unparsable_urls() {
        for bad in ["", "not a url", "/relative/path", "http://", "http://./"] {
            assert_eq!(parse_page_url(bad, false), Err(UrlError::Invalid), "{bad}");
        }
    }

    #[test]
    fn rejects_non_http_schemes() {
        for bad in [
            "ftp://example.com/",
            "file:///etc/passwd",
            "javascript:alert(1)",
            "data:text/html,hi",
            "mailto:a@example.com",
        ] {
            assert_eq!(
                parse_page_url(bad, false),
                Err(UrlError::SchemeNotAllowed),
                "{bad}"
            );
        }
    }

    #[test]
    fn scheme_is_case_insensitive() {
        assert_eq!(page("HTTPS://Example.com/x").hostname, "example.com");
    }

    // ---- hostname ----

    #[test]
    fn hostname_rules_apply() {
        assert_eq!(page("https://WWW.Example.COM./a").hostname, "example.com");
        assert_eq!(page("https://example.com:8443/a").hostname, "example.com");
        assert_eq!(page("http://127.0.0.1:3000/").hostname, "127.0.0.1");
        assert_eq!(page("http://[::1]:3000/").hostname, "[::1]");
        assert_eq!(page("https://例え.jp/").hostname, "xn--r8jz45g.jp");
    }

    // ---- pathname ----

    #[test]
    fn empty_path_becomes_slash_and_trailing_slash_is_kept() {
        assert_eq!(page("https://example.com").pathname, "/");
        assert_eq!(page("https://example.com?x=1").pathname, "/");
        assert_eq!(page("https://example.com/Docs/").pathname, "/Docs/");
        assert_eq!(page("https://example.com/a?x=1#frag").pathname, "/a");
    }

    #[test]
    fn safe_decode_decodes_ordinary_escapes() {
        assert_eq!(safe_decode("/caf%C3%A9"), "/café");
        assert_eq!(safe_decode("/a%20b"), "/a b");
        assert_eq!(
            page("https://example.com/%E4%B8%AD%E6%96%87").pathname,
            "/中文"
        );
        assert_eq!(page("https://example.com/café").pathname, "/café");
    }

    #[test]
    fn safe_decode_keeps_reserved_and_control_escapes() {
        assert_eq!(safe_decode("/a%2Fb"), "/a%2Fb");
        assert_eq!(safe_decode("/a%2fb"), "/a%2fb");
        assert_eq!(safe_decode("/a%5Cb"), "/a%5Cb");
        assert_eq!(safe_decode("/a%3Fb"), "/a%3Fb");
        assert_eq!(safe_decode("/a%23b"), "/a%23b");
        assert_eq!(safe_decode("/100%25"), "/100%25");
        assert_eq!(safe_decode("/a%00b"), "/a%00b");
        assert_eq!(safe_decode("/a%0Ab"), "/a%0Ab");
        assert_eq!(safe_decode("/a%7Fb"), "/a%7Fb");
        // 混合：可解码的解码，保留项不动。
        assert_eq!(safe_decode("/%41%2F%42"), "/A%2FB");
    }

    #[test]
    fn safe_decode_leaves_whole_string_on_invalid_escape() {
        assert_eq!(safe_decode("/a%zz/%41"), "/a%zz/%41");
        assert_eq!(safe_decode("/a%4"), "/a%4");
        assert_eq!(safe_decode("/a%"), "/a%");
        assert_eq!(page("https://example.com/a%zz/%41").pathname, "/a%zz/%41");
    }

    #[test]
    fn safe_decode_leaves_whole_string_on_invalid_utf8() {
        assert_eq!(safe_decode("/%FF%41"), "/%FF%41");
        assert_eq!(safe_decode("/%C3"), "/%C3");
        assert_eq!(page("https://example.com/%FF%41").pathname, "/%FF%41");
    }

    #[test]
    fn safe_decode_never_grows() {
        for s in ["/%41%42", "%2F%2F", "/a%zz", "%E4%B8%AD", ""] {
            assert!(safe_decode(s).len() <= s.len(), "{s}");
        }
    }

    #[test]
    fn pathname_is_truncated_to_1024_bytes_on_char_boundary() {
        let long = format!("https://example.com/{}", "a".repeat(2000));
        assert_eq!(page(&long).pathname.len(), 1024);
        // "/" + 中 * N：1 + 3N；N = 341 时为 1024，N = 342 时为 1027，截到 341 个字。
        let cjk = format!("https://example.com/{}", "中".repeat(400));
        let p = page(&cjk).pathname;
        assert_eq!(p.len(), 1024);
        assert!(p.chars().skip(1).all(|c| c == '中'));
        // 恰好 1024 字节不截断。
        let exact = format!("https://example.com/{}", "a".repeat(1023));
        assert_eq!(page(&exact).pathname.len(), 1024);
        assert_eq!(truncate_bytes("a中", 2), "a");
        assert_eq!(truncate_bytes("abc", 10), "abc");
    }

    #[test]
    fn long_query_does_not_fail_the_url() {
        let long = format!(
            "https://example.com/p?utm_source=a&junk={}",
            "x".repeat(5000)
        );
        let p = page(&long);
        assert_eq!(p.pathname, "/p");
        assert_eq!(p.utm.source.as_deref(), Some("a"));
    }

    // ---- query 白名单 ----

    #[test]
    fn extracts_utm_params_and_drops_the_rest() {
        let p = page(
            "https://example.com/?utm_source=newsletter&utm_medium=email&utm_campaign=spring&utm_content=hero&utm_term=rivu&token=abc",
        );
        assert_eq!(p.utm.source.as_deref(), Some("newsletter"));
        assert_eq!(p.utm.medium.as_deref(), Some("email"));
        assert_eq!(p.utm.campaign.as_deref(), Some("spring"));
        assert_eq!(p.utm.content.as_deref(), Some("hero"));
        assert_eq!(p.utm.term.as_deref(), Some("rivu"));
        assert_eq!(p.pathname, "/");
        assert_eq!(p.click_id_param, None);
    }

    #[test]
    fn source_falls_back_in_order() {
        assert_eq!(
            page("https://e.com/?ref=r&source=s&utm_source=u")
                .utm
                .source
                .as_deref(),
            Some("u")
        );
        assert_eq!(
            page("https://e.com/?ref=r&source=s").utm.source.as_deref(),
            Some("s")
        );
        assert_eq!(
            page("https://e.com/?ref=r").utm.source.as_deref(),
            Some("r")
        );
        assert_eq!(page("https://e.com/").utm.source, None);
    }

    #[test]
    fn empty_value_is_missing_and_does_not_beat_fallback() {
        let p = page("https://e.com/?utm_source=&ref=x&utm_medium=");
        assert_eq!(p.utm.source.as_deref(), Some("x"));
        assert_eq!(p.utm.medium, None);
        // 仅空白、仅控制字符同样算缺失。
        let p = page("https://e.com/?utm_source=%20%20&source=%01%02&ref=r");
        assert_eq!(p.utm.source.as_deref(), Some("r"));
    }

    #[test]
    fn first_non_empty_duplicate_wins() {
        let p = page("https://e.com/?utm_source=a&utm_source=b");
        assert_eq!(p.utm.source.as_deref(), Some("a"));
        let p = page("https://e.com/?utm_source=&utm_source=b");
        assert_eq!(p.utm.source.as_deref(), Some("b"));
    }

    #[test]
    fn keys_are_case_sensitive() {
        let p = page("https://e.com/?UTM_SOURCE=a&Utm_Medium=b");
        assert_eq!(p.utm, Utm::default());
    }

    #[test]
    fn values_are_trimmed_cleaned_and_truncated() {
        let p = page("https://e.com/?utm_campaign=%20%20spring%20sale%20&utm_content=a%00b%0Ac");
        assert_eq!(p.utm.campaign.as_deref(), Some("spring sale"));
        assert_eq!(p.utm.content.as_deref(), Some("abc"));
        let long = "x".repeat(300);
        let p = page(&format!("https://e.com/?utm_term={long}"));
        assert_eq!(p.utm.term.as_deref(), Some("x".repeat(255).as_str()));
        let cjk = "中".repeat(300);
        let p = page(&format!("https://e.com/?utm_term={cjk}"));
        assert_eq!(p.utm.term.as_ref().map(|s| s.chars().count()), Some(255));
    }

    #[test]
    fn malformed_escapes_in_query_are_kept_and_invalid_utf8_is_replaced() {
        let p = page("https://e.com/?utm_source=a%zzb&utm_medium=%FF");
        assert_eq!(p.utm.source.as_deref(), Some("a%zzb"));
        assert_eq!(p.utm.medium.as_deref(), Some("\u{FFFD}"));
        let p = page("https://e.com/?utm_source=a+b");
        assert_eq!(p.utm.source.as_deref(), Some("a b"));
    }

    // ---- click id ----

    #[test]
    fn click_id_stores_only_the_parameter_name() {
        assert_eq!(
            page("https://e.com/?gclid=abc").click_id_param,
            Some("gclid")
        );
        assert_eq!(
            page("https://e.com/?li_fat_id=1").click_id_param,
            Some("li_fat_id")
        );
        assert_eq!(
            page("https://e.com/?ttclid=1").click_id_param,
            Some("ttclid")
        );
    }

    #[test]
    fn click_id_follows_priority_order_not_query_order() {
        assert_eq!(
            page("https://e.com/?fbclid=1&gclid=2&msclkid=3").click_id_param,
            Some("gclid")
        );
        assert_eq!(
            page("https://e.com/?li_fat_id=1&twclid=2").click_id_param,
            Some("twclid")
        );
    }

    #[test]
    fn click_id_skips_empty_values() {
        assert_eq!(
            page("https://e.com/?gclid=&gbraid=%20&wbraid=ok").click_id_param,
            Some("wbraid")
        );
        assert_eq!(page("https://e.com/?gclid=").click_id_param, None);
    }

    #[test]
    fn click_id_value_never_leaks_into_other_fields() {
        let p = page("https://e.com/p?gclid=SECRET");
        assert_eq!(p.pathname, "/p");
        assert_eq!(p.utm, Utm::default());
    }

    // ---- hash 模式 ----

    #[test]
    fn hash_is_ignored_without_hash_mode() {
        assert_eq!(page("https://e.com/app#/users/1").pathname, "/app");
    }

    #[test]
    fn hash_route_is_appended_in_hash_mode() {
        assert_eq!(
            hashed("https://e.com/app#/users/1").pathname,
            "/app#/users/1"
        );
        assert_eq!(hashed("https://e.com/#!/home").pathname, "/#!/home");
        assert_eq!(hashed("https://e.com/app#plans").pathname, "/app#plans");
    }

    #[test]
    fn hash_is_cut_at_first_question_mark() {
        let p = hashed("https://e.com/app#/users?id=7&utm_source=x");
        assert_eq!(p.pathname, "/app#/users");
        assert_eq!(p.utm.source.as_deref(), Some("x"));
    }

    #[test]
    fn hash_query_has_lower_priority_than_real_query() {
        let p = hashed("https://e.com/app?utm_source=real#/r?utm_source=hash&utm_medium=m");
        assert_eq!(p.utm.source.as_deref(), Some("real"));
        assert_eq!(p.utm.medium.as_deref(), Some("m"));
        // 真实 query 里是空串时，hash 里的值补上。
        let p = hashed("https://e.com/app?utm_source=#/r?utm_source=hash");
        assert_eq!(p.utm.source.as_deref(), Some("hash"));
    }

    #[test]
    fn hash_query_is_ignored_without_hash_mode() {
        let p = page("https://e.com/app#/r?utm_source=hash");
        assert_eq!(p.utm.source, None);
    }

    #[test]
    fn hash_with_equals_and_no_route_prefix_is_dropped() {
        let p = hashed("https://e.com/cb#access_token=SECRET&token_type=bearer");
        assert_eq!(p.pathname, "/cb");
        let p = hashed("https://e.com/cb#state=abc");
        assert_eq!(p.pathname, "/cb");
        // 以 `/` 或 `!/` 开头的路由即使含 `=` 也保留。
        assert_eq!(hashed("https://e.com/#/a=b").pathname, "/#/a=b");
        assert_eq!(hashed("https://e.com/#!/a=b").pathname, "/#!/a=b");
    }

    #[test]
    fn dropped_hash_still_yields_whitelisted_params_after_question_mark() {
        let p = hashed("https://e.com/cb#access_token=SECRET?utm_source=x");
        assert_eq!(p.pathname, "/cb");
        assert_eq!(p.utm.source.as_deref(), Some("x"));
    }

    #[test]
    fn empty_fragment_adds_nothing() {
        assert_eq!(hashed("https://e.com/app#").pathname, "/app");
        assert_eq!(hashed("https://e.com/app#?utm_source=x").pathname, "/app");
    }

    #[test]
    fn hash_is_safely_decoded_too() {
        assert_eq!(hashed("https://e.com/#/caf%C3%A9").pathname, "/#/café");
        assert_eq!(hashed("https://e.com/#/a%2Fb").pathname, "/#/a%2Fb");
    }

    #[test]
    fn hash_is_included_in_pathname_truncation() {
        let u = format!("https://e.com/{}#/tail", "a".repeat(1020));
        let p = hashed(&u).pathname;
        assert_eq!(p.len(), 1024);
        assert!(p.ends_with("#/t"));
    }
}
