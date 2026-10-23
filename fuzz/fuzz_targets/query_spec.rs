//! `rivu_query`：任意 JSON 文本 -> `QuerySpec::parse_str` -> resolve + SQL 生成（`explain`）。
//! 对能生成 SQL 的输入断言：
//! - 每个 `FROM` 带租户范围（`site_id = {site:UInt64}`）；
//! - SQL 里没有 `?`，每个 `{name:Type}` 占位符都有同名同型的参数；
//! - 过滤取值里的字符串不会出现在 SQL 文本里（只走参数）；
//! - 序列化回 JSON 再解析得到同一个 `QuerySpec`（往返）。
#![no_main]

use std::collections::BTreeSet;

use jiff::Timestamp;
use jiff::civil::Weekday;
use jiff::tz::TimeZone;
use libfuzzer_sys::fuzz_target;
use rivu_query::{QuerySpec, SiteContext, TenantScope, explain};
use serde_json::Value;

const SITE_ID: u64 = 4242;

fn placeholders(sql: &str) -> BTreeSet<(String, String)> {
    let mut out = BTreeSet::new();
    let mut rest = sql;
    while let Some(i) = rest.find('{') {
        let after = &rest[i + 1..];
        let Some(j) = after.find('}') else { break };
        if let Some((name, ty)) = after[..j].split_once(':') {
            out.insert((name.to_owned(), ty.to_owned()));
        }
        rest = &after[j + 1..];
    }
    out
}

/// 过滤树里所有长度 >= 6 的字符串。
fn filter_strings(v: &Value, out: &mut Vec<String>) {
    match v {
        Value::String(s) if s.len() >= 6 => out.push(s.clone()),
        Value::Array(a) => a.iter().for_each(|x| filter_strings(x, out)),
        Value::Object(m) => m.values().for_each(|x| filter_strings(x, out)),
        _ => {}
    }
}

fuzz_target!(|data: &[u8]| {
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };
    let Ok(spec) = QuerySpec::parse_str(text) else {
        return;
    };

    // 往返：规范 JSON 再解析，得到同一个 spec
    if let Ok(json) = serde_json::to_value(&spec) {
        let again = QuerySpec::parse(&json);
        assert_eq!(again.as_ref().ok(), Some(&spec), "序列化往返不一致：{json}");
    }

    let raw: Value = serde_json::from_str(text).expect("parse_str 成功意味着是合法 JSON");
    let mut strings = Vec::new();
    if let Some(f) = raw.get("filters") {
        filter_strings(f, &mut strings);
    }

    let now: Timestamp = "2026-10-23T12:00:00+02:00".parse().unwrap();
    let scope = TenantScope::new(SITE_ID);
    let contexts = [
        ("UTC", Weekday::Monday),
        ("Europe/Berlin", Weekday::Sunday),
        ("Pacific/Kiritimati", Weekday::Saturday),
    ];
    for (tz, week) in contexts {
        let mut site = SiteContext::new(SITE_ID, TimeZone::get(tz).unwrap());
        site.week_starts_on = week;
        let Ok((sql, params)) = explain(&scope, &site, &spec, now, Some(1_700_000_000_000)) else {
            continue;
        };
        assert!(!sql.contains('?'), "{sql}");
        assert!(sql.contains("{site:UInt64}"), "{sql}");
        for (name, ty) in placeholders(&sql) {
            let p = params
                .get(&name)
                .unwrap_or_else(|| panic!("占位符 {name} 没有参数：{sql}"));
            assert_eq!(p.clickhouse_type(), ty, "{name}");
        }
        // 没有过滤时的 SQL 作基线：取值是 SQL 常量片段的子串（例如 `visitors`）时不算泄漏
        let mut base = raw.clone();
        if let Some(o) = base.as_object_mut() {
            o.remove("filters");
        }
        let baseline = QuerySpec::parse(&base)
            .ok()
            .and_then(|b| explain(&scope, &site, &b, now, Some(1_700_000_000_000)).ok())
            .map(|(s, _)| s);
        if let Some(baseline) = baseline {
            for s in &strings {
                let structural = s
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b == b'_' || b == b':');
                if !structural && !s.contains('{') && !baseline.contains(s.as_str()) {
                    assert!(!sql.contains(s.as_str()), "过滤取值 {s:?} 出现在 SQL 文本里：{sql}");
                }
            }
        }
    }
});


