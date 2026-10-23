//! SQL 快照与注入测试（`insta`）。
//!
//! 快照里能直接看出：取值只以 `{name:Type}` 参数出现、每个 `FROM` 都带 `site_id = {site:UInt64}`、
//! 比率在外层用分子分母重算、排序与分页在 SQL 里。更新快照：`INSTA_UPDATE=always cargo test -p rivu-query`，
//! 然后审阅 `src/snapshots/` 里的差异。

use std::collections::BTreeSet;

use jiff::Timestamp;
use jiff::civil::Weekday;
use jiff::tz::TimeZone;
use serde_json::{Value, json};

use crate::error::QueryError;
use crate::exec::explain;
use crate::resolve::SiteContext;
use crate::spec::QuerySpec;
use crate::sql::{ParamValue, Params, TenantScope, check_tenant_scoped};

const SITE_ID: u64 = 4242;

fn site() -> SiteContext {
    let mut s = SiteContext::new(SITE_ID, TimeZone::get("Europe/Berlin").unwrap());
    s.week_starts_on = Weekday::Sunday;
    s
}

/// 2026-10-23 12:00 Europe/Berlin。
fn now() -> Timestamp {
    "2026-10-23T12:00:00+02:00".parse().unwrap()
}

fn build(spec: Value) -> Result<(String, Params), QueryError> {
    let spec = QuerySpec::parse(&spec)?;
    explain(&TenantScope::new(SITE_ID), &site(), &spec, now(), None)
}

/// 在关键字前换行，按括号深度缩进：只为让快照好读。
fn pretty(sql: &str) -> String {
    const KEYWORDS: [&str; 10] = [
        " FROM ",
        " WHERE ",
        " GROUP BY ",
        " HAVING ",
        " ORDER BY ",
        " LIMIT ",
        " UNION ALL ",
        " ANY LEFT JOIN ",
        " AND ",
        " OR ",
    ];
    let mut out = String::new();
    let mut depth = 0usize;
    let mut i = 0;
    let bytes = sql.as_bytes();
    while i < sql.len() {
        let rest = &sql[i..];
        if let Some(k) = KEYWORDS.iter().find(|k| rest.starts_with(**k)) {
            // AND / OR 只在 WHERE 的顶层换行太啰嗦：不换
            if *k == " AND " || *k == " OR " {
                out.push_str(k);
            } else {
                out.push('\n');
                out.push_str(&"  ".repeat(depth));
                out.push_str(k.trim_start());
            }
            i += k.len();
            continue;
        }
        match bytes[i] {
            b'(' => {
                depth += 1;
                if rest.starts_with("(SELECT") {
                    out.push('(');
                    out.push('\n');
                    out.push_str(&"  ".repeat(depth));
                    i += 1;
                    continue;
                }
                out.push('(');
            }
            b')' => {
                depth = depth.saturating_sub(1);
                out.push(')');
            }
            _ => {
                let ch = rest.chars().next().unwrap();
                out.push(ch);
                i += ch.len_utf8();
                continue;
            }
        }
        i += 1;
    }
    out
}

fn render(sql: &str, params: &Params) -> String {
    let mut s = pretty(sql);
    s.push_str("\n\n-- params\n");
    for (name, v) in params.iter() {
        let shown = match v {
            ParamValue::StrArray(a) => format!("{a:?}"),
            ParamValue::U32Array(a) => format!("{a:?}"),
            ParamValue::Str(x) | ParamValue::Date(x) => format!("{x:?}"),
            ParamValue::U8(x) => x.to_string(),
            ParamValue::U64(x) => x.to_string(),
            ParamValue::I64(x) => x.to_string(),
            ParamValue::F64(x) => x.to_string(),
        };
        s.push_str(&format!("{name}: {} = {shown}\n", v.clickhouse_type()));
    }
    s
}

fn snap(name: &str, spec: Value) {
    let (sql, params) = build(spec).unwrap_or_else(|e| panic!("{name}: {e}"));
    assert!(check_tenant_scoped(&sql), "{name}: 有未带租户范围的 FROM");
    insta::with_settings!({snapshot_path => "snapshots", prepend_module_to_snapshot => false}, {
        insta::assert_snapshot!(name, render(&sql, &params));
    });
}

#[test]
fn sql_overview() {
    snap(
        "overview",
        json!({
            "metrics": ["visitors", "visits", "pageviews", "bounce_rate", "visit_duration", "engaged_duration"],
            "date_range": ["2026-03-28", "2026-03-30"]
        }),
    );
}

#[test]
fn sql_breakdown_session_attribute() {
    snap(
        "breakdown_country",
        json!({
            "metrics": ["visitors", "visits", "pageviews"],
            "date_range": "7d",
            "dimensions": ["visit:country"],
            "order_by": [["visitors", "desc"], ["visits", "desc"], ["pageviews", "desc"]],
            "pagination": {"limit": 10}
        }),
    );
}

#[test]
fn sql_top_pages_with_event_dimension() {
    snap(
        "top_pages",
        json!({
            "metrics": ["visitors", "visits", "pageviews", "bounce_rate", "time_on_page", "scroll_depth"],
            "date_range": "30d",
            "dimensions": ["event:page"],
            "include": {"total_rows": true},
            "pagination": {"limit": 100, "offset": 20}
        }),
    );
    // 只看 pageview 的页面榜（P1 的 `breakdown(Page)` 预设）
    snap(
        "top_pages_pageviews_only",
        json!({
            "metrics": ["visitors", "visits", "pageviews"],
            "date_range": "30d",
            "dimensions": ["event:page"],
            "filters": [["is", "event:name", ["pageview"]]],
            "order_by": [["visitors", "desc"], ["visits", "desc"], ["pageviews", "desc"]],
            "pagination": {"limit": 10}
        }),
    );
}

#[test]
fn sql_bounce_rate_with_page_filter_is_a_separate_component() {
    snap(
        "bounce_rate_page_filter",
        json!({
            "metrics": ["visitors", "pageviews", "visits", "bounce_rate", "visit_duration"],
            "date_range": "month",
            "filters": [["is", "event:page", ["/pricing"]]]
        }),
    );
}

#[test]
fn sql_filter_tree_with_every_operator() {
    snap(
        "filter_tree",
        json!({
            "metrics": ["visitors"],
            "date_range": "28d",
            "filters": [
                ["is", "visit:country", ["DE", "(not set)"]],
                ["or", [
                    ["contains", "event:page", ["blog", "docs"], {"case_sensitive": false}],
                    ["not", ["matches", "visit:browser", ["^Chrome|Safari$"]]],
                    ["and", [
                        ["matches_wildcard", "event:hostname", ["*.example.com"]],
                        ["is_not", "visit:city", [2694762]]
                    ]]
                ]],
                ["matches_wildcard_not", "visit:entry_page", ["/docs/**"]],
                ["contains_not", "visit:utm_campaign", ["spring"]],
                ["matches_not", "visit:referrer", ["^x\\.com"], {"case_sensitive": false}],
                ["is_not", "visit:exit_page", ["/"]]
            ]
        }),
    );
}

#[test]
fn sql_time_series_with_week_start() {
    snap(
        "time_week",
        json!({
            "metrics": ["visitors", "visits", "bounce_rate"],
            "date_range": "6mo",
            "dimensions": ["time:week", "visit:device"],
            "include": {"time_labels": true}
        }),
    );
    snap(
        "time_hour_auto",
        json!({
            "metrics": ["pageviews", "visit_duration"],
            "date_range": "24h",
            "dimensions": ["time"]
        }),
    );
}

#[test]
fn sql_entry_and_exit_page_dimensions_join_sessions() {
    snap(
        "entry_exit_dimensions",
        json!({
            "metrics": ["visitors", "pageviews", "time_on_page"],
            "date_range": "7d",
            "dimensions": ["visit:entry_page", "visit:exit_page"]
        }),
    );
}

#[test]
fn sql_exit_rate_and_percentage() {
    snap(
        "exit_rate",
        json!({
            "metrics": ["pageviews", "exit_rate"],
            "date_range": "7d",
            "dimensions": ["event:page"],
            "filters": [["is", "visit:country", ["US"]]]
        }),
    );
    snap(
        "percentage",
        json!({
            "metrics": ["visitors", "percentage"],
            "date_range": "7d",
            "dimensions": ["visit:source"],
            "filters": [["is_not", "visit:channel", ["Direct"]]]
        }),
    );
}

#[test]
fn sql_all_and_realtime_ranges() {
    let spec = QuerySpec::parse(&json!({"metrics": ["visitors"], "date_range": "all"})).unwrap();
    let start = "2025-12-30T08:00:00Z"
        .parse::<Timestamp>()
        .unwrap()
        .as_millisecond();
    let (sql, params) = explain(
        &TenantScope::new(SITE_ID),
        &site(),
        &spec,
        now(),
        Some(start),
    )
    .unwrap();
    insta::with_settings!({snapshot_path => "snapshots", prepend_module_to_snapshot => false}, {
        insta::assert_snapshot!("all_range", render(&sql, &params));
    });
    snap(
        "realtime",
        json!({"metrics": ["visitors"], "date_range": "realtime"}),
    );
}

#[test]
fn sql_comparison_query_is_constrained_by_main_rows() {
    use crate::plan::plan;
    use crate::resolve::ResolvedQuery;
    use crate::sql::{Constraint, MainSqlOptions, OrderMode, main_query};
    let spec = QuerySpec::parse(&json!({
        "metrics": ["visitors", "bounce_rate"],
        "date_range": "7d",
        "dimensions": ["visit:country", "visit:device"],
        "include": {"compare": "previous_period"}
    }))
    .unwrap();
    let q = ResolvedQuery::build(&spec, &site(), now(), None).unwrap();
    let p = plan(&q);
    let cons = Constraint {
        dims: vec![0, 1],
        rows: vec![
            vec!["DE".to_owned(), "desktop".to_owned()],
            vec![String::new(), "mobile".to_owned()],
        ],
    };
    let (sql, params) = main_query(
        &TenantScope::new(SITE_ID),
        &q,
        &p,
        &MainSqlOptions {
            interval: q.compare.unwrap(),
            constraint: Some(&cons),
            total_visitors: None,
            limit: 2,
            offset: 0,
            order: OrderMode::Query,
            with_total_rows: false,
        },
    );
    assert!(check_tenant_scoped(&sql));
    insta::with_settings!({snapshot_path => "snapshots", prepend_module_to_snapshot => false}, {
        insta::assert_snapshot!("compare_constrained", render(&sql, &params));
    });
}

#[test]
fn json_schema_snapshot() {
    insta::with_settings!({snapshot_path => "snapshots", prepend_module_to_snapshot => false}, {
        insta::assert_json_snapshot!("query_schema", crate::schema_json());
    });
}

// ------------------------------------------------------------------------------------------
// 注入与限制
// ------------------------------------------------------------------------------------------

/// 恶意取值：引号、反斜杠、`{x:String}` 形式的文本、注释、换行、占位符、Unicode。
fn nasty_values() -> Vec<String> {
    vec![
        "'; DROP TABLE events; --".to_owned(),
        "\\'\\\\\\".to_owned(),
        "a\\'b\"c`d".to_owned(),
        "{x:String}".to_owned(),
        "{p0:Array(String)} {site:UInt64}".to_owned(),
        "/* c */ OR 1=1 --".to_owned(),
        "line1\nline2\ttab\r".to_owned(),
        "? ?? ?fields".to_owned(),
        "%s %d ${x} {{x}}".to_owned(),
        "') OR site_id != 0 OR ('1'='1".to_owned(),
        "日本語 😀 \u{202e}evil".to_owned(),
        "UNION ALL SELECT * FROM sessions WHERE 1=1".to_owned(),
    ]
}

fn placeholders(sql: &str) -> BTreeSet<(String, String)> {
    let mut out = BTreeSet::new();
    let mut rest = sql;
    while let Some(i) = rest.find('{') {
        let after = &rest[i + 1..];
        let Some(j) = after.find('}') else { break };
        let inner = &after[..j];
        if let Some((name, ty)) = inner.split_once(':') {
            out.insert((name.to_owned(), ty.to_owned()));
        }
        rest = &after[j + 1..];
    }
    out
}

#[test]
fn user_values_never_reach_the_sql_text() {
    let ops = [
        "is",
        "is_not",
        "contains",
        "contains_not",
        "matches_wildcard",
        "matches_wildcard_not",
    ];
    let dims = [
        "event:page",
        "event:hostname",
        "visit:source",
        "visit:utm_term",
        "visit:entry_page",
        "visit:exit_page",
        "visit:browser",
    ];
    let mut checked = 0;
    for value in nasty_values() {
        for op in ops {
            for dim in dims {
                for cs in [true, false] {
                    let spec = json!({
                        "metrics": ["visitors", "visits", "bounce_rate"],
                        "date_range": "7d",
                        "dimensions": ["visit:country"],
                        "filters": [[op, dim, [value], {"case_sensitive": cs}],
                                    ["not", ["or", [[op, dim, [value]], ["is", "event:name", [value]]]]]],
                        "include": {"compare": "previous_period"}
                    });
                    let (sql, params) = build(spec).unwrap_or_else(|e| panic!("{op} {dim}: {e}"));
                    assert!(check_tenant_scoped(&sql), "{sql}");
                    assert!(!sql.contains('?'), "{sql}");
                    // 整个取值，以及几个标志性的片段，都不能出现在 SQL 文本里。
                    // （恰好长得像占位符的取值 `{p0:Array(String)}` 会和真正的占位符撞上，只检查它以外的。）
                    let tokens = [
                        value.as_str(),
                        "DROP TABLE",
                        "OR 1=1",
                        "日本語",
                        "evil",
                        "!= 0 OR",
                    ];
                    for token in tokens {
                        if value.contains(token) && !token.starts_with('{') && token != "{x:String}"
                        {
                            assert!(
                                !sql.contains(token),
                                "取值里的 `{token}` 出现在 SQL 文本里：{sql}"
                            );
                        }
                    }
                    assert!(!sql.contains("{x:String}"), "{sql}");
                    // 每个占位符都有同名同型的参数
                    for (name, ty) in placeholders(&sql) {
                        let p = params
                            .get(&name)
                            .unwrap_or_else(|| panic!("占位符 {name} 没有参数：{sql}"));
                        assert_eq!(p.clickhouse_type(), ty, "{name}");
                    }
                    checked += 1;
                }
            }
        }
    }
    assert!(checked > 500, "{checked}");
}

#[test]
fn nasty_regexes_are_rejected_or_parameterised() {
    for value in nasty_values() {
        let spec = json!({
            "metrics": ["visitors"],
            "date_range": "7d",
            "filters": [["matches", "event:page", [value]]]
        });
        match build(spec) {
            Ok((sql, params)) => {
                // 合法的正则（例如只含普通字符）：照样只走参数
                assert!(!sql.contains(&value), "{sql}");
                assert!(
                    params
                        .iter()
                        .any(|(_, v)| matches!(v, ParamValue::StrArray(a) if a.contains(&value)))
                );
            }
            Err(QueryError::Validation { path, message }) => {
                assert_eq!(path, "filters[0][2][0]");
                assert!(
                    message.starts_with("invalid regular expression"),
                    "{message}"
                );
            }
            Err(e) => panic!("{e}"),
        }
    }
}

fn validation(spec: Value) -> (String, String) {
    let shown = spec.to_string();
    match build(spec) {
        Err(QueryError::Validation { path, message }) => (path, message),
        Ok(_) => panic!("期望校验错误，但请求被接受了：{shown}"),
        Err(e) => panic!("期望校验错误，得到 {e}：{shown}"),
    }
}

#[test]
fn regex_and_value_limits() {
    let f = |op: &str, values: Value| {
        validation(json!({
            "metrics": ["visitors"], "date_range": "day",
            "filters": [[op, "event:page", values]]
        }))
    };
    // 每个过滤最多 10 个模式
    let eleven: Vec<String> = (0..11).map(|i| format!("a{i}")).collect();
    for op in [
        "matches",
        "matches_not",
        "matches_wildcard",
        "matches_wildcard_not",
    ] {
        let (p, m) = f(op, json!(eleven));
        assert_eq!(p, "filters[0][2]", "{op}");
        assert!(m.contains("at most 10"), "{m}");
        let ten: Vec<String> = (0..10).map(|i| format!("a{i}")).collect();
        assert!(
            build(json!({"metrics": ["visitors"], "date_range": "day",
            "filters": [[op, "event:page", ten]]}))
            .is_ok()
        );
    }
    // 每个模式最多 256 字符
    let long = "a".repeat(257);
    let (p, m) = f("matches", json!([long]));
    assert_eq!(p, "filters[0][2][0]");
    assert!(m.contains("256"), "{m}");
    let exactly = "a".repeat(256);
    assert!(
        build(json!({"metrics": ["visitors"], "date_range": "day",
        "filters": [["matches", "event:page", [exactly]]]}))
        .is_ok()
    );
    // 加上 `(?i)` 之后超过发给数据库的 256 字节上限
    let (p, m) = validation(json!({"metrics": ["visitors"], "date_range": "day",
        "filters": [["matches", "event:page", ["a".repeat(256)], {"case_sensitive": false}]]}));
    assert_eq!(p, "filters[0][2][0]");
    assert!(m.contains("bytes after conversion"), "{m}");
    // 通配符转义后膨胀：128 个 `.` 转成 256 字节以上
    let (p, m) = f("matches_wildcard", json!(["...".repeat(80)]));
    assert_eq!(p, "filters[0][2][0]");
    assert!(m.contains("bytes after conversion"), "{m}");
    // 非法正则、回溯构造
    for bad in ["(", "[a-", "a{2,1}", "(?<=x)y", "(a)\\1", "*a"] {
        let (p, m) = f("matches", json!([bad]));
        assert_eq!(p, "filters[0][2][0]", "{bad}");
        assert!(m.starts_with("invalid regular expression"), "{bad}: {m}");
    }
    // 空值、NUL、数量
    assert_eq!(f("contains", json!([""])).0, "filters[0][2][0]");
    assert_eq!(f("matches", json!([""])).0, "filters[0][2][0]");
    assert_eq!(f("is", json!(["a\u{0}b"])).0, "filters[0][2][0]");
    assert_eq!(f("is", json!([])).0, "filters[0][2]");
    let many: Vec<String> = (0..1001).map(|i| i.to_string()).collect();
    assert!(f("is", json!(many)).1.contains("at most 1000"));
    let many: Vec<String> = (0..101).map(|i| i.to_string()).collect();
    assert!(f("contains", json!(many)).1.contains("at most 100"));
    assert!(f("is", json!(["x".repeat(1025)])).1.contains("1024"));
    // 过滤条件总数
    let leaves: Vec<Value> = (0..101)
        .map(|i| json!(["is", "event:page", [format!("/p{i}")]]))
        .collect();
    let (p, m) =
        validation(json!({"metrics": ["visitors"], "date_range": "day", "filters": leaves}));
    assert_eq!(p, "filters");
    assert!(m.contains("at most 100"), "{m}");
}

#[test]
fn invalid_combinations_and_unsupported_features_fail_with_paths() {
    let v = |spec: Value| validation(spec);
    let base = |m: Value, d: Value| json!({"metrics": m, "dimensions": d, "date_range": "7d"});
    // 尚未支持
    for (spec, path, what) in [
        (
            json!({"metrics": ["conversion_rate"], "date_range": "day"}),
            "metrics[0]",
            "not supported yet",
        ),
        (
            json!({"metrics": ["visitors", "total_revenue"], "date_range": "day"}),
            "metrics[1]",
            "not supported yet",
        ),
        (
            base(json!(["visitors"]), json!(["event:goal"])),
            "dimensions[0]",
            "not supported yet",
        ),
        (
            base(
                json!(["visitors"]),
                json!(["visit:country", "event:props:plan"]),
            ),
            "dimensions[1]",
            "not supported yet",
        ),
        (
            base(json!(["visitors"]), json!(["time:minute"])),
            "dimensions[0]",
            "not supported yet",
        ),
        (
            json!({"metrics": ["visitors"], "date_range": "day", "filters": [["has_done", ["is", "event:name", ["x"]]]]}),
            "filters[0][0]",
            "not supported yet",
        ),
        (
            json!({"metrics": ["visitors"], "date_range": "day", "filters": [["is", "segment", [1]]]}),
            "filters[0][1]",
            "not supported yet",
        ),
        (
            json!({"metrics": ["visitors"], "date_range": "day", "filters": [["is", "event:goal", ["x"]]]}),
            "filters[0][1]",
            "not supported yet",
        ),
        (
            json!({"metrics": ["visitors"], "date_range": "day", "filters": [["is", "event:props:a", ["x"]]]}),
            "filters[0][1]",
            "not supported yet",
        ),
    ] {
        let (p, m) = v(spec.clone());
        assert_eq!(p, path, "{spec}");
        assert!(m.contains(what), "{m}");
    }
    // 组合
    for (spec, path) in [
        (base(json!(["percentage"]), json!([])), "metrics[0]"),
        (
            base(json!(["percentage"]), json!(["time:day"])),
            "metrics[0]",
        ),
        (
            base(json!(["exit_rate"]), json!(["visit:country"])),
            "metrics[0]",
        ),
        (
            base(json!(["exit_rate"]), json!(["event:page", "visit:country"])),
            "metrics[0]",
        ),
        (
            base(json!(["bounce_rate"]), json!(["event:name"])),
            "dimensions[0]",
        ),
        (
            base(json!(["time_on_page"]), json!(["event:name"])),
            "dimensions[0]",
        ),
        (
            json!({"metrics": ["scroll_depth"], "date_range": "day", "filters": [["is", "event:name", ["x"]]]}),
            "filters[0]",
        ),
        (
            json!({"metrics": ["exit_rate"], "dimensions": ["event:page"], "date_range": "day", "filters": [["is", "event:page", ["/"]]]}),
            "filters[0]",
        ),
        (
            json!({"metrics": ["visitors"], "date_range": "day", "include": {"time_labels": true}}),
            "include.time_labels",
        ),
        (
            base(json!(["visitors"]), json!(["time:day", "time:hour"])),
            "dimensions[1]",
        ),
        (
            base(
                json!(["visitors"]),
                json!(["visit:country", "visit:country"]),
            ),
            "dimensions[1]",
        ),
        (
            json!({"metrics": ["visitors", "visitors"], "date_range": "day"}),
            "metrics[1]",
        ),
        (
            json!({"metrics": ["visitors"], "date_range": "day", "filters": [["is", "time:day", ["x"]]]}),
            "filters[0][1]",
        ),
        (
            json!({"metrics": ["visitors"], "date_range": "day", "filters": [["contains", "visit:city", ["26"]]]}),
            "filters[0][0]",
        ),
        (
            json!({"metrics": ["visitors"], "date_range": "day", "filters": [["is", "visit:city", ["abc"]]]}),
            "filters[0][2][0]",
        ),
        (
            json!({"metrics": ["visitors"], "date_range": ["2026-03-02", "2026-03-01"]}),
            "date_range",
        ),
        (
            json!({"metrics": ["visitors"], "date_range": "day", "pagination": {"limit": 0}}),
            "pagination.limit",
        ),
        (
            json!({"metrics": ["visitors"], "date_range": "day", "pagination": {"limit": 10001}}),
            "pagination.limit",
        ),
    ] {
        let (p, _) = v(spec.clone());
        assert_eq!(p, path, "{spec}");
    }
    // order_by 只能是已查询的指标或维度（也挡住注入式的键）
    for key in [
        "pageviews",
        "visit:source",
        "visitors; DROP TABLE events",
        "m0",
        "d0",
        "time",
    ] {
        let (p, m) = v(
            json!({"metrics": ["visitors"], "dimensions": ["visit:country"],
            "date_range": "day", "order_by": [[key, "asc"]]}),
        );
        assert_eq!(p, "order_by[0][0]");
        assert!(m.contains("requested"), "{m}");
    }
    // 站点核对：给出的 site 与上下文不一致时拒绝，一致时通过
    let mut ctx = site();
    ctx.site_key = Some("abcdefghijkl".into());
    let other = QuerySpec::parse(
        &json!({"metrics": ["visitors"], "date_range": "day", "site": "someone-else"}),
    )
    .unwrap();
    match explain(&TenantScope::new(SITE_ID), &ctx, &other, now(), None) {
        Err(QueryError::Validation { path, .. }) => assert_eq!(path, "site"),
        other => panic!("{other:?}"),
    }
    let spec = QuerySpec::parse(
        &json!({"metrics": ["visitors"], "date_range": "day", "site": "abcdefghijkl"}),
    )
    .unwrap();
    assert!(explain(&TenantScope::new(SITE_ID), &ctx, &spec, now(), None).is_ok());
}

#[test]
fn every_generated_statement_is_tenant_scoped() {
    // 把支持的指标、维度、过滤各种组合起来过一遍（不执行）
    let metrics: [&str; 12] = [
        "visitors",
        "visits",
        "pageviews",
        "events",
        "views_per_visit",
        "bounce_rate",
        "visit_duration",
        "engaged_duration",
        "percentage",
        "exit_rate",
        "time_on_page",
        "scroll_depth",
    ];
    let dims = [
        json!([]),
        json!(["event:page"]),
        json!(["visit:entry_page"]),
        json!(["visit:exit_page", "visit:country"]),
        json!(["time:day"]),
        json!(["time", "visit:city"]),
    ];
    let filters = [
        json!([]),
        json!([["is", "event:page", ["/a"]]]),
        json!([
            ["is", "visit:exit_page", ["/a"]],
            ["not", ["is", "visit:entry_page", ["/b"]]]
        ]),
        json!([[
            "or",
            [
                ["is", "event:hostname", ["a.com"]],
                ["contains", "visit:source", ["g"]]
            ]
        ]]),
    ];
    let mut built = 0;
    for m in metrics {
        for d in &dims {
            for f in &filters {
                let spec = json!({"metrics": [m], "dimensions": d, "filters": f, "date_range": "30d",
                    "include": {"total_rows": true}});
                match build(spec) {
                    Ok((sql, _)) => {
                        assert!(check_tenant_scoped(&sql), "{m} {d} {f}: {sql}");
                        // 每个 FROM 都有，且数量不少于子查询数
                        assert!(
                            sql.matches("FROM events").count()
                                + sql.matches("FROM sessions").count()
                                >= 1
                        );
                        built += 1;
                    }
                    Err(QueryError::Validation { .. }) => {}
                    Err(e) => panic!("{e}"),
                }
            }
        }
    }
    assert!(built > 100, "{built}");
}
