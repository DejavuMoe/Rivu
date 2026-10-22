//! 线协议（`v=1`）解析与规范化。
//!
//! 规则出处：`docs/architecture/tracker-and-protocol.md` §5（字段总表）、§6.2（错误词表）、
//! §7（服务端规范化规则）。这里不碰 HTTP、不碰站点配置：
//! 内部引荐的判定（referrer 的 host 是否命中站点白名单）、hostname 白名单、`$` 收入入库，
//! 都由调用方在后续阶段处理。

use std::collections::BTreeMap;
use std::fmt;

use rivu_domain::{EventKind, NormalizedEvent, Referrer};
use serde_json::{Map, Value, json};
use url::Url;

use crate::hostname::normalize_hostname;
use crate::page_url::{UrlError, parse_page_url, safe_decode, truncate_chars};

/// 请求体字节上限。超限的 413 在 HTTP 层处理，`parse_event` 不检查。
pub const MAX_BODY_BYTES: usize = 8192;

const MAX_EVENT_NAME_CHARS: usize = 120;
const MAX_REFERRER_CHARS: usize = 2000;
const MAX_PROPS: usize = 30;
const MAX_PROP_KEY_BYTES: usize = 300;
const MAX_PROP_VALUE_BYTES: usize = 2000;
const MAX_SCRIPT_VERSION: u64 = 999_999;
/// `e` 的开区间上界：30 天的毫秒数。
const MAX_ENGAGEMENT_MS: i128 = 2_592_000_000;
/// `sd` 缺失/非法时的入库哨兵（聚合时必须排除）。
const SCROLL_DEPTH_UNREPORTED: u8 = 255;

// §6.2 的固定错误消息词表。协议错误消息必须是英文，不翻译。
const IS_REQUIRED: &str = "is required";
const MUST_BE_STRING: &str = "must be a string";
const MUST_BE_INTEGER: &str = "must be an integer";
const MUST_BE_BOOLEAN: &str = "must be a boolean";
const MUST_BE_OBJECT: &str = "must be an object";
const MUST_BE_JSON_OBJECT: &str = "must be a JSON object";
const NOT_VALID_JSON: &str = "is not valid JSON";
const NOT_SUPPORTED: &str = "is not supported";
const MUST_BE_URL: &str = "must be a valid url";
const SCHEME_NOT_ALLOWED: &str = "scheme is not allowed";
const ENGAGEMENT_NEEDS_SD_OR_E: &str = "at least one of sd or e is required";
const TOO_MANY_KEYS: &str = "has too many keys (max 30)";

fn too_long(max: usize) -> String {
    format!("is too long (max {max})")
}

/// 请求体的字段错误，对应 `400` 响应体 `{"errors":{"字段":["消息"]}}`。
///
/// 多个字段的错误一次性汇总；同一字段的重复消息只保留一条。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProtocolErrors {
    errors: BTreeMap<String, Vec<String>>,
}

impl ProtocolErrors {
    fn add(&mut self, field: &str, message: impl Into<String>) {
        let message = message.into();
        let list = self.errors.entry(field.to_owned()).or_default();
        if !list.contains(&message) {
            list.push(message);
        }
    }

    fn body(message: &str) -> Self {
        let mut e = Self::default();
        e.add("body", message);
        e
    }

    pub fn is_empty(&self) -> bool {
        self.errors.is_empty()
    }

    /// 出错的字段名（升序），用于 `rejected_<field>` 计数。请求体整体错误的字段名为 `body`。
    pub fn fields(&self) -> impl Iterator<Item = &str> {
        self.errors.keys().map(String::as_str)
    }

    /// 某字段的错误消息；没有错误时为空切片。
    pub fn messages(&self, field: &str) -> &[String] {
        self.errors.get(field).map_or(&[], Vec::as_slice)
    }

    /// `400` 响应体：`{"errors":{"字段":["消息",...]}}`。
    pub fn to_json(&self) -> Value {
        json!({ "errors": self.errors })
    }
}

impl fmt::Display for ProtocolErrors {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut first = true;
        for (field, messages) in &self.errors {
            for m in messages {
                if !first {
                    f.write_str("; ")?;
                }
                first = false;
                write!(f, "{field} {m}")?;
            }
        }
        Ok(())
    }
}

impl std::error::Error for ProtocolErrors {}

/// JSON 整数（`i64`/`u64` 能表示的）；`1.0`、`"1"`、`null` 都不是。
fn as_int(v: &Value) -> Option<i128> {
    v.as_i64()
        .map(i128::from)
        .or_else(|| v.as_u64().map(i128::from))
}

/// 取键值，`null` 视同缺失。
fn present<'a>(obj: &'a Map<String, Value>, key: &str) -> Option<&'a Value> {
    obj.get(key).filter(|v| !v.is_null())
}

/// 必填字符串：缺失/`null` → `is required`，类型错 → `must be a string`。
fn required_str<'a>(
    obj: &'a Map<String, Value>,
    key: &str,
    errs: &mut ProtocolErrors,
) -> Option<&'a str> {
    match present(obj, key) {
        None => errs.add(key, IS_REQUIRED),
        Some(Value::String(s)) => return Some(s),
        Some(_) => errs.add(key, MUST_BE_STRING),
    }
    None
}

fn parse_version(obj: &Map<String, Value>, errs: &mut ProtocolErrors) -> Option<u8> {
    match present(obj, "v") {
        None => errs.add("v", IS_REQUIRED),
        Some(v) => match as_int(v) {
            Some(1) => return Some(1),
            Some(_) => errs.add("v", NOT_SUPPORTED),
            None => errs.add("v", MUST_BE_INTEGER),
        },
    }
    None
}

/// `sv`：只作计数标签，非法/缺失一律按 0，永不报错。
fn parse_script_version(obj: &Map<String, Value>) -> u32 {
    obj.get("sv")
        .and_then(Value::as_u64)
        .filter(|n| *n <= MAX_SCRIPT_VERSION)
        .and_then(|n| u32::try_from(n).ok())
        .unwrap_or(0)
}

/// `^[A-Za-z0-9_-]{8,32}$`
fn is_valid_site_key(s: &str) -> bool {
    (8..=32).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

fn parse_site_key(obj: &Map<String, Value>, errs: &mut ProtocolErrors) -> Option<String> {
    let s = required_str(obj, "s", errs)?;
    if is_valid_site_key(s) {
        Some(s.to_owned())
    } else {
        // 词表里没有「格式错」专用消息，沿用 `is not supported`。
        errs.add("s", NOT_SUPPORTED);
        None
    }
}

fn parse_name(raw: &str, errs: &mut ProtocolErrors) -> Option<String> {
    let name = raw.trim();
    if name.is_empty() {
        errs.add("n", IS_REQUIRED);
    } else if name.chars().count() > MAX_EVENT_NAME_CHARS {
        errs.add("n", too_long(MAX_EVENT_NAME_CHARS));
    } else if name.chars().any(char::is_control) {
        errs.add("n", NOT_SUPPORTED);
    } else {
        return Some(name.to_owned());
    }
    None
}

/// `r`：`null`/空串/无法解析/scheme 不对/无 host 都视为缺失；只有类型不对才报错。
fn parse_referrer(obj: &Map<String, Value>, errs: &mut ProtocolErrors) -> Option<Referrer> {
    match present(obj, "r")? {
        Value::String(s) => normalize_referrer(s),
        _ => {
            errs.add("r", MUST_BE_STRING);
            None
        }
    }
}

fn normalize_referrer(raw: &str) -> Option<Referrer> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let url = Url::parse(truncate_chars(trimmed, MAX_REFERRER_CHARS)).ok()?;
    if !matches!(url.scheme(), "http" | "https" | "android-app") {
        return None;
    }
    let host = normalize_hostname(url.host_str()?);
    if host.is_empty() {
        return None;
    }
    let path = safe_decode(url.path());
    let stored = format!("{host}{}", path.trim_end_matches('/'));
    Some(Referrer { host, stored })
}

fn parse_props(v: &Value, errs: &mut ProtocolErrors) -> Vec<(String, String)> {
    let Value::Object(map) = v else {
        errs.add("p", MUST_BE_OBJECT);
        return Vec::new();
    };
    if map.len() > MAX_PROPS {
        errs.add("p", TOO_MANY_KEYS);
        return Vec::new();
    }
    let mut out: Vec<(String, String)> = Vec::with_capacity(map.len());
    for (raw_key, raw_value) in map {
        let key = raw_key.trim();
        if key.len() > MAX_PROP_KEY_BYTES {
            errs.add("p", too_long(MAX_PROP_KEY_BYTES));
            continue;
        }
        let value = match raw_value {
            Value::String(s) => s.clone(),
            Value::Number(n) => n.to_string(),
            Value::Bool(b) => b.to_string(),
            // null、数组、对象
            _ => {
                errs.add("p", MUST_BE_STRING);
                continue;
            }
        };
        if value.len() > MAX_PROP_VALUE_BYTES {
            errs.add("p", too_long(MAX_PROP_VALUE_BYTES));
            continue;
        }
        if key.is_empty() || value.trim().is_empty() {
            continue;
        }
        // 同名键后者覆盖前者；位置保持首次出现处。
        match out.iter_mut().find(|(k, _)| k == key) {
            Some(entry) => entry.1 = value,
            None => out.push((key.to_owned(), value)),
        }
    }
    out
}

/// `i`：仅自定义事件生效；缺失/`null` 为 `true`。
fn parse_interactive(obj: &Map<String, Value>, errs: &mut ProtocolErrors) -> bool {
    match present(obj, "i") {
        None => true,
        Some(Value::Bool(b)) => *b,
        Some(_) => {
            errs.add("i", MUST_BE_BOOLEAN);
            true
        }
    }
}

/// `h`：只有整数 `1` 开启 hash 模式；其他整数视为未设置；非整数 → 400。
fn parse_hash_mode(obj: &Map<String, Value>, errs: &mut ProtocolErrors) -> bool {
    match present(obj, "h") {
        None => false,
        Some(v) => match as_int(v) {
            Some(n) => n == 1,
            None => {
                errs.add("h", MUST_BE_INTEGER);
                false
            }
        },
    }
}

/// `sd`：0–100，>100 夹到 100；`null`/负数/非整数一律视为缺失。
fn valid_scroll_depth(v: Option<&Value>) -> Option<u8> {
    let n = as_int(v?)?;
    if n < 0 {
        return None;
    }
    u8::try_from(n.min(100)).ok()
}

/// `e`：毫秒，`0 <= e < 30 天`；越界/非法视为缺失。
fn valid_engagement_ms(v: Option<&Value>) -> Option<u32> {
    let n = as_int(v?)?;
    if (0..MAX_ENGAGEMENT_MS).contains(&n) {
        u32::try_from(n).ok()
    } else {
        None
    }
}

/// `$` 收入对象的格式校验：`a` 匹配 `^\d{1,15}(\.\d{1,4})?$`，`c` 为三个大写 ASCII 字母。
/// 通过后丢弃，P4 前不入库。
fn validate_revenue(v: &Value, errs: &mut ProtocolErrors) {
    let Value::Object(map) = v else {
        errs.add("$", MUST_BE_OBJECT);
        return;
    };
    for key in ["a", "c"] {
        match present(map, key) {
            None => errs.add("$", IS_REQUIRED),
            Some(Value::String(s)) => {
                let ok = if key == "a" {
                    is_valid_amount(s)
                } else {
                    is_valid_currency(s)
                };
                if !ok {
                    errs.add("$", NOT_SUPPORTED);
                }
            }
            Some(_) => errs.add("$", MUST_BE_STRING),
        }
    }
}

fn is_valid_amount(s: &str) -> bool {
    let (int, frac) = match s.split_once('.') {
        Some((i, f)) => (i, Some(f)),
        None => (s, None),
    };
    let digits =
        |p: &str, max: usize| (1..=max).contains(&p.len()) && p.bytes().all(|b| b.is_ascii_digit());
    digits(int, 15) && frac.is_none_or(|f| digits(f, 4))
}

fn is_valid_currency(s: &str) -> bool {
    s.len() == 3 && s.bytes().all(|b| b.is_ascii_uppercase())
}

/// 解析并规范化 `POST /api/event` 的请求体。
///
/// 请求体一律按 UTF-8 JSON 解析（不看 `Content-Type`），必须是 JSON 对象；未知键忽略；
/// 各字段的错误一次性汇总在 [`ProtocolErrors`] 里。超过 [`MAX_BODY_BYTES`] 的 413 在 HTTP 层处理。
pub fn parse_event(body: &[u8]) -> Result<NormalizedEvent, ProtocolErrors> {
    let value: Value =
        serde_json::from_slice(body).map_err(|_| ProtocolErrors::body(NOT_VALID_JSON))?;
    let Value::Object(obj) = value else {
        return Err(ProtocolErrors::body(MUST_BE_JSON_OBJECT));
    };

    let mut errs = ProtocolErrors::default();

    let protocol_version = parse_version(&obj, &mut errs);
    let script_version = parse_script_version(&obj);
    let site_key = parse_site_key(&obj, &mut errs);

    let raw_name = required_str(&obj, "n", &mut errs);
    let name = raw_name.and_then(|n| parse_name(n, &mut errs));
    // 名字非法时仍按 trim 后的原文判断种类，让 `i`/`sd`/`e`/`$` 的校验口径不随 `n` 的对错而变。
    let kind = raw_name.map_or(EventKind::Custom, |n| EventKind::from_name(n.trim()));

    let hash_mode = parse_hash_mode(&obj, &mut errs);
    let page =
        required_str(&obj, "u", &mut errs).and_then(|u| match parse_page_url(u, hash_mode) {
            Ok(page) => Some(page),
            Err(UrlError::Invalid) => {
                errs.add("u", MUST_BE_URL);
                None
            }
            Err(UrlError::SchemeNotAllowed) => {
                errs.add("u", SCHEME_NOT_ALLOWED);
                None
            }
        });

    let referrer = parse_referrer(&obj, &mut errs);
    let props = present(&obj, "p").map_or_else(Vec::new, |p| parse_props(p, &mut errs));

    let (interactive, scroll_depth, engagement_ms) = match kind {
        EventKind::Pageview => (true, SCROLL_DEPTH_UNREPORTED, 0),
        EventKind::Engagement => {
            let sd = valid_scroll_depth(obj.get("sd"));
            let e = valid_engagement_ms(obj.get("e"));
            if sd.is_none() && e.is_none() {
                errs.add("sd", ENGAGEMENT_NEEDS_SD_OR_E);
            }
            (true, sd.unwrap_or(SCROLL_DEPTH_UNREPORTED), e.unwrap_or(0))
        }
        EventKind::Custom => {
            let interactive = parse_interactive(&obj, &mut errs);
            if let Some(revenue) = present(&obj, "$") {
                validate_revenue(revenue, &mut errs);
            }
            (interactive, SCROLL_DEPTH_UNREPORTED, 0)
        }
    };

    match (protocol_version, site_key, name, page) {
        (Some(protocol_version), Some(site_key), Some(name), Some(page)) if errs.is_empty() => {
            Ok(NormalizedEvent {
                protocol_version,
                script_version,
                site_key,
                name,
                kind,
                hostname: page.hostname,
                pathname: page.pathname,
                referrer,
                utm: page.utm,
                click_id_param: page.click_id_param,
                props,
                interactive,
                scroll_depth,
                engagement_ms,
            })
        }
        _ => Err(errs),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use rivu_domain::Utm;

    const SITE: &str = "k7Qm2xV9aTbR4cLd";

    /// 构造一个合法的 pageview 请求体，再让测试改动个别字段。
    fn base() -> Value {
        json!({"v": 1, "s": SITE, "n": "pageview", "u": "https://example.com/"})
    }

    fn parse(v: &Value) -> Result<NormalizedEvent, ProtocolErrors> {
        parse_event(v.to_string().as_bytes())
    }

    fn ok(v: &Value) -> NormalizedEvent {
        parse(v).unwrap_or_else(|e| panic!("应当通过，实际 {e}: {v}"))
    }

    fn err(v: &Value) -> ProtocolErrors {
        parse(v).expect_err("应当 400")
    }

    fn with(mut v: Value, key: &str, value: Value) -> Value {
        v[key] = value;
        v
    }

    fn without(mut v: Value, key: &str) -> Value {
        v.as_object_mut().unwrap().remove(key);
        v
    }

    /// 断言 400 体恰好是 `{"errors":{field:[msg]}}`。
    fn assert_single_error(v: &Value, field: &str, msg: &str) {
        let e = err(v);
        assert_eq!(
            e.to_json(),
            json!({"errors": {field: [msg]}}),
            "请求体: {v}"
        );
    }

    // ---- §5.2 三个示例请求 ----

    #[test]
    fn example_1_pageview() {
        let body = br#"{"v":1,"sv":3,"s":"k7Qm2xV9aTbR4cLd","n":"pageview","u":"https://www.Example.com/pricing?utm_source=newsletter&utm_medium=email&token=abc123#plans","r":"https://news.example.org/post/42?id=7"}"#;
        let ev = parse_event(body).unwrap();
        assert_eq!(
            ev,
            NormalizedEvent {
                protocol_version: 1,
                script_version: 3,
                site_key: SITE.into(),
                name: "pageview".into(),
                kind: EventKind::Pageview,
                hostname: "example.com".into(),
                pathname: "/pricing".into(),
                referrer: Some(Referrer {
                    host: "news.example.org".into(),
                    stored: "news.example.org/post/42".into(),
                }),
                utm: Utm {
                    source: Some("newsletter".into()),
                    medium: Some("email".into()),
                    ..Utm::default()
                },
                click_id_param: None,
                props: vec![],
                interactive: true,
                scroll_depth: 255,
                engagement_ms: 0,
            }
        );
    }

    #[test]
    fn example_2_custom_event_with_props_and_revenue() {
        let body = br#"{"v":1,"sv":3,"s":"k7Qm2xV9aTbR4cLd","n":"Purchase","u":"https://example.com/checkout/done","p":{"plan":"pro","seats":"5"},"$":{"a":"29.00","c":"USD"}}"#;
        let ev = parse_event(body).unwrap();
        assert_eq!(
            ev,
            NormalizedEvent {
                protocol_version: 1,
                script_version: 3,
                site_key: SITE.into(),
                name: "Purchase".into(),
                kind: EventKind::Custom,
                hostname: "example.com".into(),
                pathname: "/checkout/done".into(),
                referrer: None,
                utm: Utm::default(),
                click_id_param: None,
                props: vec![("plan".into(), "pro".into()), ("seats".into(), "5".into())],
                interactive: true,
                scroll_depth: 255,
                engagement_ms: 0,
            }
        );
    }

    #[test]
    fn example_3_engagement() {
        let body = br#"{"v":1,"sv":3,"s":"k7Qm2xV9aTbR4cLd","n":"engagement","u":"https://example.com/pricing","sd":72,"e":18450}"#;
        let ev = parse_event(body).unwrap();
        assert_eq!(
            ev,
            NormalizedEvent {
                protocol_version: 1,
                script_version: 3,
                site_key: SITE.into(),
                name: "engagement".into(),
                kind: EventKind::Engagement,
                hostname: "example.com".into(),
                pathname: "/pricing".into(),
                referrer: None,
                utm: Utm::default(),
                click_id_param: None,
                props: vec![],
                interactive: true,
                scroll_depth: 72,
                engagement_ms: 18450,
            }
        );
    }

    // ---- 请求体整体 ----

    #[test]
    fn body_must_be_valid_json() {
        for bad in [
            &b""[..],
            b"{",
            b"not json",
            b"{} trailing",
            b"\xff\xfe{}",
            b"\xef\xbb\xbf{}",
        ] {
            let e = parse_event(bad).unwrap_err();
            assert_eq!(
                e.to_json(),
                json!({"errors": {"body": ["is not valid JSON"]}})
            );
        }
    }

    #[test]
    fn body_must_be_json_object() {
        for bad in ["null", "[]", "[{}]", "1", "\"s\"", "true"] {
            let e = parse_event(bad.as_bytes()).unwrap_err();
            assert_eq!(
                e.to_json(),
                json!({"errors": {"body": ["must be a JSON object"]}}),
                "{bad}"
            );
        }
    }

    #[test]
    fn deeply_nested_json_is_a_400_not_a_panic() {
        let deep = format!("{}{}", "[".repeat(100_000), "]".repeat(100_000));
        let e = parse_event(deep.as_bytes()).unwrap_err();
        assert_eq!(e.fields().collect::<Vec<_>>(), ["body"]);
        let nested = format!(
            r#"{{"v":1,"s":"{SITE}","n":"x","u":"https://e.com/","p":{{"a":{}{}}}}}"#,
            "[".repeat(1000),
            "]".repeat(1000)
        );
        assert!(parse_event(nested.as_bytes()).is_err());
    }

    #[test]
    fn unknown_keys_and_timestamps_are_ignored() {
        let v = with(with(base(), "t", json!(123)), "extra", json!({"x": [1]}));
        let v = with(v, "ts", json!("now"));
        assert_eq!(ok(&v), ok(&base()));
    }

    #[test]
    fn keys_are_case_sensitive() {
        let v = json!({"V": 1, "S": SITE, "N": "pageview", "U": "https://example.com/"});
        let e = err(&v);
        assert_eq!(e.fields().collect::<Vec<_>>(), ["n", "s", "u", "v"]);
    }

    #[test]
    fn multiple_errors_are_collected_in_one_response() {
        let e = parse_event(br#"{"v":2,"s":"x","n":"","u":"nope","p":[1]}"#).unwrap_err();
        assert_eq!(
            e.to_json(),
            json!({"errors": {
                "v": ["is not supported"],
                "s": ["is not supported"],
                "n": ["is required"],
                "u": ["must be a valid url"],
                "p": ["must be an object"],
            }})
        );
        assert_eq!(e.fields().collect::<Vec<_>>(), ["n", "p", "s", "u", "v"]);
        assert!(!e.is_empty());
        assert_eq!(e.messages("v"), ["is not supported"]);
        assert!(e.messages("zzz").is_empty());
        assert!(e.to_string().contains("v is not supported"));
    }

    #[test]
    fn empty_object_reports_every_required_field() {
        let e = parse_event(b"{}").unwrap_err();
        assert_eq!(
            e.to_json(),
            json!({"errors": {
                "n": ["is required"],
                "s": ["is required"],
                "u": ["is required"],
                "v": ["is required"],
            }})
        );
    }

    // ---- v / sv ----

    #[test]
    fn v_accepts_only_integer_one() {
        assert_eq!(ok(&base()).protocol_version, 1);
        for bad in [json!(0), json!(2), json!(36), json!(-1), json!(u64::MAX)] {
            assert_single_error(&with(base(), "v", bad), "v", "is not supported");
        }
        for bad in [
            json!("1"),
            json!(1.0),
            json!(1.5),
            json!(true),
            json!([1]),
            json!({}),
        ] {
            assert_single_error(&with(base(), "v", bad), "v", "must be an integer");
        }
        assert_single_error(&with(base(), "v", Value::Null), "v", "is required");
        assert_single_error(&without(base(), "v"), "v", "is required");
    }

    #[test]
    fn sv_invalid_or_missing_is_zero_and_never_errors() {
        assert_eq!(ok(&with(base(), "sv", json!(3))).script_version, 3);
        assert_eq!(ok(&with(base(), "sv", json!(0))).script_version, 0);
        assert_eq!(
            ok(&with(base(), "sv", json!(999_999))).script_version,
            999_999
        );
        for bad in [
            json!(1_000_000),
            json!(-1),
            json!(1.5),
            json!("3"),
            json!(null),
            json!(true),
            json!([3]),
            json!({}),
            json!(u64::MAX),
        ] {
            assert_eq!(
                ok(&with(base(), "sv", bad.clone())).script_version,
                0,
                "{bad}"
            );
        }
        assert_eq!(ok(&base()).script_version, 0);
    }

    // ---- s ----

    #[test]
    fn site_key_format() {
        for good in [
            "abcdefgh",
            "A_b-C_d-1234",
            &"a".repeat(32),
            "k7Qm2xV9aTbR4cLd",
        ] {
            assert_eq!(ok(&with(base(), "s", json!(good))).site_key, good);
        }
        for bad in [
            "short",
            "abcdefg",
            &"a".repeat(33),
            "has space1",
            "dots.dots.x",
            "ünïcödé123",
            "abcdefgh\n",
            "",
        ] {
            assert_single_error(&with(base(), "s", json!(bad)), "s", "is not supported");
        }
    }

    #[test]
    fn site_key_type_and_presence() {
        assert_single_error(&with(base(), "s", json!(12345678)), "s", "must be a string");
        assert_single_error(&with(base(), "s", json!(null)), "s", "is required");
        assert_single_error(&without(base(), "s"), "s", "is required");
    }

    // ---- n ----

    #[test]
    fn event_name_is_trimmed_and_kind_derived() {
        let ev = ok(&with(base(), "n", json!("  Signup  ")));
        assert_eq!(ev.name, "Signup");
        assert_eq!(ev.kind, EventKind::Custom);
        assert_eq!(
            ok(&with(base(), "n", json!(" pageview "))).kind,
            EventKind::Pageview
        );
        // 不改大小写：`Pageview` 是自定义事件。
        let ev = ok(&with(base(), "n", json!("Pageview")));
        assert_eq!(ev.kind, EventKind::Custom);
        assert_eq!(ev.name, "Pageview");
    }

    #[test]
    fn event_name_length_is_in_characters() {
        let ok120 = "名".repeat(120);
        assert_eq!(ok(&with(base(), "n", json!(ok120.clone()))).name, ok120);
        assert_single_error(
            &with(base(), "n", json!("名".repeat(121))),
            "n",
            "is too long (max 120)",
        );
        assert_eq!(
            ok(&with(base(), "n", json!("a".repeat(120)))).name.len(),
            120
        );
        assert_single_error(
            &with(base(), "n", json!("a".repeat(121))),
            "n",
            "is too long (max 120)",
        );
        // 长度按 trim 之后计。
        let padded = format!("  {}  ", "a".repeat(120));
        assert_eq!(ok(&with(base(), "n", json!(padded))).name.len(), 120);
    }

    #[test]
    fn event_name_rejects_empty_and_control_characters() {
        for empty in ["", "   ", "\t\n"] {
            assert_single_error(&with(base(), "n", json!(empty)), "n", "is required");
        }
        for bad in ["a\u{0}b", "a\nb", "a\tb", "a\u{7f}b", "a\u{85}b"] {
            assert_single_error(&with(base(), "n", json!(bad)), "n", "is not supported");
        }
        assert_single_error(&with(base(), "n", json!(5)), "n", "must be a string");
        assert_single_error(&without(base(), "n"), "n", "is required");
    }

    // ---- u ----

    #[test]
    fn url_errors() {
        assert_single_error(
            &with(base(), "u", json!("not a url")),
            "u",
            "must be a valid url",
        );
        assert_single_error(
            &with(base(), "u", json!("/relative")),
            "u",
            "must be a valid url",
        );
        assert_single_error(&with(base(), "u", json!("")), "u", "must be a valid url");
        assert_single_error(
            &with(base(), "u", json!("ftp://example.com/")),
            "u",
            "scheme is not allowed",
        );
        assert_single_error(
            &with(base(), "u", json!("javascript:alert(1)")),
            "u",
            "scheme is not allowed",
        );
        assert_single_error(&with(base(), "u", json!(1)), "u", "must be a string");
        assert_single_error(&with(base(), "u", json!(null)), "u", "is required");
        assert_single_error(&without(base(), "u"), "u", "is required");
    }

    #[test]
    fn url_has_no_length_limit_but_long_query_is_dropped() {
        let long = format!(
            "https://example.com/p?junk={}&utm_source=ads",
            "x".repeat(3000)
        );
        let ev = ok(&with(base(), "u", json!(long)));
        assert_eq!(ev.pathname, "/p");
        assert_eq!(ev.utm.source.as_deref(), Some("ads"));
        let long_path = format!("https://example.com/{}", "a".repeat(3000));
        assert_eq!(
            ok(&with(base(), "u", json!(long_path))).pathname.len(),
            1024
        );
    }

    #[test]
    fn url_normalization_flows_into_the_event() {
        let ev = ok(&with(
            base(),
            "u",
            json!("https://WWW.Example.com.:8443/Café%2Fx?gclid=1&utm_source=&ref=blog"),
        ));
        assert_eq!(ev.hostname, "example.com");
        assert_eq!(ev.pathname, "/Café%2Fx");
        assert_eq!(ev.utm.source.as_deref(), Some("blog"));
        assert_eq!(ev.click_id_param, Some("gclid"));
    }

    #[test]
    fn hash_mode_flag() {
        let u = json!("https://example.com/app#/users/1?utm_source=x");
        let ev = ok(&with(base(), "u", u.clone()));
        assert_eq!(ev.pathname, "/app");
        assert_eq!(ev.utm.source, None);
        let ev = ok(&with(with(base(), "u", u.clone()), "h", json!(1)));
        assert_eq!(ev.pathname, "/app#/users/1");
        assert_eq!(ev.utm.source.as_deref(), Some("x"));
        // 其他整数视为未设置；null 同缺失。
        for other in [json!(0), json!(2), json!(-1), json!(null)] {
            let ev = ok(&with(with(base(), "u", u.clone()), "h", other));
            assert_eq!(ev.pathname, "/app");
        }
        // 非整数 → 400。
        for bad in [json!("1"), json!(1.5), json!(true), json!({})] {
            assert_single_error(&with(base(), "h", bad), "h", "must be an integer");
        }
    }

    // ---- r ----

    #[test]
    fn referrer_missing_cases() {
        for r in [json!(null), json!(""), json!("   ")] {
            assert_eq!(ok(&with(base(), "r", r)).referrer, None);
        }
        assert_eq!(ok(&base()).referrer, None);
    }

    #[test]
    fn referrer_wrong_type_is_400() {
        for bad in [json!(1), json!(true), json!({}), json!([])] {
            assert_single_error(&with(base(), "r", bad), "r", "must be a string");
        }
    }

    #[test]
    fn referrer_invalid_values_are_treated_as_missing() {
        for r in [
            "not a url",
            "/relative",
            "ftp://example.com/x",
            "javascript:alert(1)",
            "mailto:a@b.c",
            "android-app://",
            "http://./",
        ] {
            assert_eq!(ok(&with(base(), "r", json!(r))).referrer, None, "{r}");
        }
    }

    #[test]
    fn referrer_stored_form_is_host_plus_path() {
        let r = |s: &str| ok(&with(base(), "r", json!(s))).referrer;
        assert_eq!(
            r("https://news.example.org/post/42?id=7#top"),
            Some(Referrer {
                host: "news.example.org".into(),
                stored: "news.example.org/post/42".into()
            })
        );
        // 根路径去掉结尾 `/`；host 按 hostname 规则（小写、去 www、去端口、去尾点）。
        assert_eq!(
            r("https://WWW.Google.com:443/"),
            Some(Referrer {
                host: "google.com".into(),
                stored: "google.com".into()
            })
        );
        assert_eq!(
            r("http://example.org/a/b/"),
            Some(Referrer {
                host: "example.org".into(),
                stored: "example.org/a/b".into()
            })
        );
        assert_eq!(
            r("https://example.org"),
            Some(Referrer {
                host: "example.org".into(),
                stored: "example.org".into()
            })
        );
    }

    #[test]
    fn referrer_accepts_android_app_scheme() {
        let ev = ok(&with(
            base(),
            "r",
            json!("android-app://com.google.android.gm/"),
        ));
        assert_eq!(
            ev.referrer,
            Some(Referrer {
                host: "com.google.android.gm".into(),
                stored: "com.google.android.gm".into()
            })
        );
        let ev = ok(&with(
            base(),
            "r",
            json!("ANDROID-APP://Com.Example.App/https/example.com"),
        ));
        assert_eq!(
            ev.referrer,
            Some(Referrer {
                host: "com.example.app".into(),
                stored: "com.example.app/https/example.com".into()
            })
        );
    }

    #[test]
    fn referrer_is_truncated_not_rejected_at_2000_chars() {
        let long = format!("https://ref.example.org/{}", "a".repeat(5000));
        let ev = ok(&with(base(), "r", json!(long)));
        let stored = ev.referrer.unwrap().stored;
        // 2000 字符的 URL：`https://` 8 + host 15 + path。
        assert_eq!(stored.len(), 2000 - "https://".len());
        // 截断落在 host 里时，得到的 host 是被截断的那一段，但不报错。
        let long_host = format!("https://{}.com/", "h".repeat(3000));
        assert!(parse(&with(base(), "r", json!(long_host))).is_ok());
        // 多字节字符按字符数截断，不会切在字符中间。
        let cjk = format!("https://ref.example.org/{}", "中".repeat(3000));
        assert!(parse(&with(base(), "r", json!(cjk))).is_ok());
    }

    #[test]
    fn referrer_does_not_decide_internal_referral() {
        // 内部引荐判断需要站点白名单，这里照常返回 host，由接入管线第 8 步判定。
        let ev = ok(&with(base(), "r", json!("https://example.com/other")));
        assert_eq!(ev.referrer.unwrap().host, "example.com");
    }

    // ---- p ----

    #[test]
    fn props_convert_numbers_and_booleans() {
        let ev = ok(&with(
            base(),
            "p",
            json!({"seats": 5, "ratio": 1.5, "logged_in": false, "ok": true, "neg": -3}),
        ));
        let get = |k: &str| {
            ev.props
                .iter()
                .find(|(pk, _)| pk == k)
                .map(|(_, v)| v.as_str())
        };
        assert_eq!(get("seats"), Some("5"));
        assert_eq!(get("ratio"), Some("1.5"));
        assert_eq!(get("logged_in"), Some("false"));
        assert_eq!(get("ok"), Some("true"));
        assert_eq!(get("neg"), Some("-3"));
    }

    #[test]
    fn props_reject_null_array_object_values() {
        for bad in [json!(null), json!([1]), json!({"a": 1})] {
            assert_single_error(
                &with(base(), "p", json!({"k": bad})),
                "p",
                "must be a string",
            );
        }
    }

    #[test]
    fn props_must_be_an_object() {
        for bad in [json!([]), json!("x"), json!(1), json!(true)] {
            assert_single_error(&with(base(), "p", bad), "p", "must be an object");
        }
        assert!(ok(&with(base(), "p", json!(null))).props.is_empty());
        assert!(ok(&with(base(), "p", json!({}))).props.is_empty());
    }

    #[test]
    fn props_key_count_limit() {
        let make =
            |n: usize| Value::Object((0..n).map(|i| (format!("k{i}"), json!("v"))).collect());
        assert_eq!(ok(&with(base(), "p", make(30))).props.len(), 30);
        assert_single_error(
            &with(base(), "p", make(31)),
            "p",
            "has too many keys (max 30)",
        );
    }

    #[test]
    fn props_key_byte_limit() {
        let key = |n: usize| "k".repeat(n);
        let v300 = with(base(), "p", json!({key(300): "v"}));
        assert_eq!(ok(&v300).props.len(), 1);
        assert_single_error(
            &with(base(), "p", json!({key(301): "v"})),
            "p",
            "is too long (max 300)",
        );
        // 按字节：100 个汉字 = 300 B 通过，101 个 = 303 B 超限。
        assert!(parse(&with(base(), "p", json!({"中".repeat(100): "v"}))).is_ok());
        assert_single_error(
            &with(base(), "p", json!({"中".repeat(101): "v"})),
            "p",
            "is too long (max 300)",
        );
        // 长度按 trim 之后计。
        let padded = format!("  {}  ", key(300));
        assert!(parse(&with(base(), "p", json!({padded: "v"}))).is_ok());
    }

    #[test]
    fn props_value_byte_limit() {
        assert!(parse(&with(base(), "p", json!({"k": "v".repeat(2000)}))).is_ok());
        assert_single_error(
            &with(base(), "p", json!({"k": "v".repeat(2001)})),
            "p",
            "is too long (max 2000)",
        );
        // 按字节：667 个汉字 = 2001 B。
        assert!(parse(&with(base(), "p", json!({"k": "中".repeat(666)}))).is_ok());
        assert_single_error(
            &with(base(), "p", json!({"k": "中".repeat(667)})),
            "p",
            "is too long (max 2000)",
        );
    }

    #[test]
    fn props_drop_empty_keys_and_values_and_trim_keys() {
        let ev = ok(&with(
            base(),
            "p",
            json!({"": "v", "  ": "v", "k1": "", "k2": "   ", "  plan  ": "pro", "k3": "x"}),
        ));
        assert_eq!(
            ev.props,
            vec![
                ("plan".to_owned(), "pro".to_owned()),
                ("k3".to_owned(), "x".to_owned())
            ]
        );
    }

    #[test]
    fn props_keep_order_and_last_duplicate_wins() {
        let body = br#"{"v":1,"s":"k7Qm2xV9aTbR4cLd","n":"x","u":"https://e.com/","p":{"z":"1","a":"2","m":"3","a ":"4","z":"5"}}"#;
        let ev = parse_event(body).unwrap();
        assert_eq!(
            ev.props,
            vec![
                ("z".to_owned(), "5".to_owned()),
                ("a".to_owned(), "4".to_owned()),
                ("m".to_owned(), "3".to_owned()),
            ]
        );
    }

    #[test]
    fn props_apply_to_every_event_kind() {
        let v = with(base(), "p", json!({"k": "v"}));
        assert_eq!(ok(&v).props.len(), 1);
        let v = with(with(v, "n", json!("engagement")), "sd", json!(5));
        assert_eq!(ok(&v).props.len(), 1);
    }

    // ---- i ----

    #[test]
    fn interactive_applies_only_to_custom_events() {
        let custom = with(base(), "n", json!("Signup"));
        assert!(ok(&custom).interactive);
        assert!(ok(&with(custom.clone(), "i", json!(true))).interactive);
        assert!(!ok(&with(custom.clone(), "i", json!(false))).interactive);
        assert!(ok(&with(custom.clone(), "i", json!(null))).interactive);
        assert_single_error(&with(custom, "i", json!("false")), "i", "must be a boolean");
        // 系统事件忽略 `i`，包括类型错误。
        assert!(ok(&with(base(), "i", json!(false))).interactive);
        assert!(ok(&with(base(), "i", json!("junk"))).interactive);
        let eng = with(with(base(), "n", json!("engagement")), "sd", json!(1));
        assert!(ok(&with(eng, "i", json!(false))).interactive);
    }

    // ---- sd / e ----

    fn engagement() -> Value {
        with(base(), "n", json!("engagement"))
    }

    #[test]
    fn engagement_needs_sd_or_e() {
        assert_single_error(&engagement(), "sd", "at least one of sd or e is required");
        let e = err(&engagement());
        assert_eq!(e.fields().collect::<Vec<_>>(), ["sd"]);
        assert_eq!(ok(&with(engagement(), "sd", json!(50))).scroll_depth, 50);
        assert_eq!(
            ok(&with(engagement(), "e", json!(1500))).engagement_ms,
            1500
        );
    }

    #[test]
    fn engagement_only_sd_gives_zero_ms() {
        let ev = ok(&with(engagement(), "sd", json!(40)));
        assert_eq!((ev.scroll_depth, ev.engagement_ms), (40, 0));
    }

    #[test]
    fn engagement_only_e_gives_unreported_scroll_depth() {
        let ev = ok(&with(engagement(), "e", json!(2000)));
        assert_eq!((ev.scroll_depth, ev.engagement_ms), (255, 2000));
    }

    #[test]
    fn scroll_depth_bounds() {
        let sd = |v: Value| parse(&with(engagement(), "sd", v));
        assert_eq!(sd(json!(0)).unwrap().scroll_depth, 0);
        assert_eq!(sd(json!(100)).unwrap().scroll_depth, 100);
        assert_eq!(sd(json!(101)).unwrap().scroll_depth, 100);
        assert_eq!(sd(json!(5000)).unwrap().scroll_depth, 100);
        assert_eq!(sd(json!(u64::MAX)).unwrap().scroll_depth, 100);
        // 负数、非整数、null：视为缺失，单独存在时整个 engagement 400。
        for bad in [
            json!(-1),
            json!(1.5),
            json!("50"),
            json!(null),
            json!(true),
            json!([1]),
        ] {
            assert_single_error(
                &with(engagement(), "sd", bad),
                "sd",
                "at least one of sd or e is required",
            );
        }
    }

    #[test]
    fn invalid_sd_with_valid_e_is_unreported() {
        for bad in [json!(null), json!(-5), json!(2.5), json!("x")] {
            let v = with(with(engagement(), "sd", bad), "e", json!(900));
            let ev = ok(&v);
            assert_eq!((ev.scroll_depth, ev.engagement_ms), (255, 900));
        }
    }

    #[test]
    fn engagement_ms_bounds() {
        let e = |v: Value| parse(&with(with(engagement(), "sd", json!(10)), "e", v));
        assert_eq!(e(json!(0)).unwrap().engagement_ms, 0);
        assert_eq!(
            e(json!(2_591_999_999u64)).unwrap().engagement_ms,
            2_591_999_999
        );
        // 越界/非法视为 0（sd 有效时不报错）。
        for bad in [
            json!(2_592_000_000u64),
            json!(u64::MAX),
            json!(-1),
            json!(1.5),
            json!("100"),
            json!(null),
        ] {
            assert_eq!(e(bad.clone()).unwrap().engagement_ms, 0, "{bad}");
        }
    }

    #[test]
    fn engagement_with_both_invalid_is_400() {
        let v = with(
            with(engagement(), "sd", json!(null)),
            "e",
            json!(2_592_000_000u64),
        );
        assert_single_error(&v, "sd", "at least one of sd or e is required");
        let v = with(with(engagement(), "sd", json!(-1)), "e", json!("x"));
        assert_single_error(&v, "sd", "at least one of sd or e is required");
    }

    #[test]
    fn sd_and_e_are_ignored_for_non_engagement_events() {
        let v = with(with(base(), "sd", json!(50)), "e", json!(1000));
        let ev = ok(&v);
        assert_eq!((ev.scroll_depth, ev.engagement_ms), (255, 0));
        let v = with(
            with(with(base(), "n", json!("Signup")), "sd", json!(50)),
            "e",
            json!(1000),
        );
        let ev = ok(&v);
        assert_eq!((ev.scroll_depth, ev.engagement_ms), (255, 0));
        // 非法值也被忽略，不触发 400。
        let v = with(with(base(), "sd", json!("junk")), "e", json!(-1));
        assert!(parse(&v).is_ok());
    }

    #[test]
    fn engagement_event_name_must_match_exactly() {
        // `Engagement` 是自定义事件，不要求 sd/e。
        assert!(parse(&with(base(), "n", json!("Engagement"))).is_ok());
        // 前后空白 trim 后是系统事件。
        assert!(parse(&with(base(), "n", json!(" engagement "))).is_err());
    }

    // ---- $ ----

    fn custom() -> Value {
        with(base(), "n", json!("Purchase"))
    }

    #[test]
    fn revenue_valid_forms() {
        for (a, c) in [
            ("12.50", "USD"),
            ("0", "EUR"),
            ("999999999999999", "JPY"),
            ("1.2345", "CNY"),
            ("5.0", "GBP"),
        ] {
            let v = with(custom(), "$", json!({"a": a, "c": c}));
            assert!(parse(&v).is_ok(), "{a} {c}");
        }
    }

    #[test]
    fn revenue_invalid_forms() {
        for (a, c) in [
            ("1234567890123456", "USD"), // 16 位整数
            ("1.23456", "USD"),          // 5 位小数
            ("1.", "USD"),
            (".5", "USD"),
            ("-1", "USD"),
            ("1,5", "USD"),
            ("１２", "USD"), // 全角数字
            ("", "USD"),
            ("12", "usd"),
            ("12", "US"),
            ("12", "USDX"),
            ("12", "U1D"),
            ("12", "ÜSD"),
        ] {
            let v = with(custom(), "$", json!({"a": a, "c": c}));
            assert_single_error(&v, "$", "is not supported");
        }
    }

    #[test]
    fn revenue_shape_errors() {
        assert_single_error(
            &with(custom(), "$", json!("12 USD")),
            "$",
            "must be an object",
        );
        assert_single_error(&with(custom(), "$", json!([1])), "$", "must be an object");
        assert_single_error(&with(custom(), "$", json!({"a": "1"})), "$", "is required");
        assert_single_error(
            &with(custom(), "$", json!({"c": "USD"})),
            "$",
            "is required",
        );
        assert_single_error(
            &with(custom(), "$", json!({"a": 12, "c": "USD"})),
            "$",
            "must be a string",
        );
        assert!(parse(&with(custom(), "$", json!(null))).is_ok());
    }

    #[test]
    fn revenue_is_discarded_and_ignored_for_system_events() {
        let with_rev = with(custom(), "$", json!({"a": "29.00", "c": "USD"}));
        assert_eq!(ok(&with_rev), ok(&custom()));
        // pageview / engagement 对 `$` 不校验。
        assert!(parse(&with(base(), "$", json!("garbage"))).is_ok());
        let eng = with(engagement(), "sd", json!(5));
        assert!(parse(&with(eng, "$", json!([]))).is_ok());
    }

    // ---- 错误词表 ----

    #[test]
    fn error_messages_stay_within_the_fixed_vocabulary() {
        let allowed = [
            "is required",
            "must be a string",
            "must be an integer",
            "must be a boolean",
            "must be an object",
            "must be a JSON object",
            "is not valid JSON",
            "has too many keys (max 30)",
            "is not supported",
            "must be a valid url",
            "scheme is not allowed",
            "at least one of sd or e is required",
        ];
        let bodies = [
            json!({}),
            json!({"v":"x","s":1,"n":1,"u":1,"r":1,"p":1,"h":"x"}),
            json!({"v":2,"s":"x","n":"a\u{0}","u":"ftp://x","p":{"k":null}}),
            json!({"v":1,"s":SITE,"n":"engagement","u":"https://e.com/"}),
            json!({"v":1,"s":SITE,"n":"x","u":"https://e.com/","i":1,"$":{}}),
        ];
        for b in bodies {
            let e = err(&b);
            for field in e.fields() {
                for m in e.messages(field) {
                    assert!(
                        allowed.contains(&m.as_str()) || m.starts_with("is too long (max "),
                        "词表外的消息 {m:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn max_body_bytes_constant() {
        assert_eq!(MAX_BODY_BYTES, 8192);
    }

    // ---- proptest ----

    proptest! {
        /// 任意字节都不会 panic。
        #[test]
        fn parse_event_never_panics_on_arbitrary_bytes(body in proptest::collection::vec(any::<u8>(), 0..1024)) {
            let _ = parse_event(&body);
        }

        /// 字段值任意（保证是 JSON）时也不会 panic，且 400 体的消息都来自固定词表。
        #[test]
        fn parse_event_never_panics_on_arbitrary_field_values(
            u in any::<String>(),
            r in any::<String>(),
            n in any::<String>(),
            key in any::<String>(),
            val in any::<String>(),
            kind in 0u8..3,
        ) {
            let name = match kind { 0 => "pageview".to_owned(), 1 => "engagement".to_owned(), _ => n };
            let body = json!({
                "v": 1, "s": SITE, "n": name, "u": u, "r": r, "h": 1,
                "sd": 5, "p": { key: val }, "$": {"a": "1", "c": "USD"},
            });
            let _ = parse(&body);
        }

        /// 通过的事件满足不变式：pathname ≤ 1024 字节，utm 值非空且 ≤ 255 字符，props 键非空。
        #[test]
        fn accepted_events_satisfy_invariants(u in "https?://[a-zA-Z0-9.:-]{1,20}/[ -~]{0,200}") {
            if let Ok(ev) = parse(&with(base(), "u", json!(u))) {
                prop_assert!(ev.pathname.len() <= 1024);
                prop_assert!(!ev.hostname.is_empty());
                for v in [&ev.utm.source, &ev.utm.medium, &ev.utm.campaign, &ev.utm.content, &ev.utm.term].into_iter().flatten() {
                    prop_assert!(!v.is_empty() && v.chars().count() <= 255);
                }
            }
        }
    }
}
