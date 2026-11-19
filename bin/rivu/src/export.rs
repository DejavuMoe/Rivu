//! CSV 导出：看板当前视图（时间范围与筛选）下的趋势与各维度明细，每个报表一个 CSV，打成一个 zip。
//!
//! 每个文件就是一次普通查询（与看板同一条查询路径：选层、结果缓存、导入的数据），所以导出的数字与看板一致。
//! 文件与列：
//!
//! | 文件 | 维度 | 指标 |
//! |---|---|---|
//! | `visitors.csv` | 时间桶（`interval`，缺省按天；没有数据的桶也列出，记 0） | visitors visits pageviews views_per_visit bounce_rate visit_duration |
//! | `sources.csv` `channels.csv` `referrers.csv` `utm_*.csv` `entry_pages.csv` `countries.csv` `regions.csv` `cities.csv` `devices.csv` `browsers.csv` `browser_versions.csv` `operating_systems.csv` `operating_system_versions.csv` | 对应的访问维度 | visitors visits pageviews bounce_rate visit_duration |
//! | `exit_pages.csv` | `visit:exit_page` | visitors visits |
//! | `pages.csv` | `event:page` | visitors pageviews time_on_page scroll_depth（筛选里有事件级条件时只有前两列：另两个指标与它们不兼容） |
//! | `custom_events.csv` | `event:name` | visitors events |
//! | `conversions.csv` | `event:goal` | visitors events conversion_rate |
//! | `custom_props.csv` | 每个属性键一组 `event:props:<k>` | property value visitors events |
//!
//! 比率是 0 到 1（4 位小数），时长是整数秒，滚动深度是 0 到 100 的整数。每个文件最多 [`MAX_LIMIT`] 行（按 visitors 降序）。

use std::future::Future;
use std::io::Write;

use jiff::Timestamp;
use jiff::tz::TimeZone;
use rivu_query::result::{MetricValue, QueryResult};
use rivu_query::spec::{Dimension, MAX_LIMIT};
use rivu_query::{QueryError, QuerySpec};
use serde_json::{Value, json};

/// 一次导出最多带的属性键（`custom_props.csv`）。
pub const MAX_PROP_KEYS: usize = 50;

const SESSION: &[&str] = &[
    "visitors",
    "visits",
    "pageviews",
    "bounce_rate",
    "visit_duration",
];

/// 一个报表：文件名、维度（列名、维度名）、指标；`fallback` 是与筛选不兼容时退回的指标。
struct Part {
    file: &'static str,
    dims: &'static [(&'static str, &'static str)],
    metrics: &'static [&'static str],
    fallback: Option<&'static [&'static str]>,
}

const PARTS: &[Part] = &[
    Part {
        file: "sources.csv",
        dims: &[("source", "visit:source")],
        metrics: SESSION,
        fallback: None,
    },
    Part {
        file: "channels.csv",
        dims: &[("channel", "visit:channel")],
        metrics: SESSION,
        fallback: None,
    },
    Part {
        file: "referrers.csv",
        dims: &[("referrer", "visit:referrer")],
        metrics: SESSION,
        fallback: None,
    },
    Part {
        file: "utm_mediums.csv",
        dims: &[("utm_medium", "visit:utm_medium")],
        metrics: SESSION,
        fallback: None,
    },
    Part {
        file: "utm_sources.csv",
        dims: &[("utm_source", "visit:utm_source")],
        metrics: SESSION,
        fallback: None,
    },
    Part {
        file: "utm_campaigns.csv",
        dims: &[("utm_campaign", "visit:utm_campaign")],
        metrics: SESSION,
        fallback: None,
    },
    Part {
        file: "utm_contents.csv",
        dims: &[("utm_content", "visit:utm_content")],
        metrics: SESSION,
        fallback: None,
    },
    Part {
        file: "utm_terms.csv",
        dims: &[("utm_term", "visit:utm_term")],
        metrics: SESSION,
        fallback: None,
    },
    Part {
        file: "pages.csv",
        dims: &[("page", "event:page")],
        metrics: &["visitors", "pageviews", "time_on_page", "scroll_depth"],
        fallback: Some(&["visitors", "pageviews"]),
    },
    Part {
        file: "entry_pages.csv",
        dims: &[("entry_page", "visit:entry_page")],
        metrics: SESSION,
        fallback: None,
    },
    Part {
        file: "exit_pages.csv",
        dims: &[("exit_page", "visit:exit_page")],
        metrics: &["visitors", "visits"],
        fallback: None,
    },
    Part {
        file: "countries.csv",
        dims: &[("country", "visit:country")],
        metrics: SESSION,
        fallback: None,
    },
    Part {
        file: "regions.csv",
        dims: &[("region", "visit:region")],
        metrics: SESSION,
        fallback: None,
    },
    Part {
        file: "cities.csv",
        dims: &[("city", "visit:city")],
        metrics: SESSION,
        fallback: None,
    },
    Part {
        file: "devices.csv",
        dims: &[("device", "visit:device")],
        metrics: SESSION,
        fallback: None,
    },
    Part {
        file: "browsers.csv",
        dims: &[("browser", "visit:browser")],
        metrics: SESSION,
        fallback: None,
    },
    Part {
        file: "browser_versions.csv",
        dims: &[
            ("browser", "visit:browser"),
            ("browser_version", "visit:browser_version"),
        ],
        metrics: SESSION,
        fallback: None,
    },
    Part {
        file: "operating_systems.csv",
        dims: &[("operating_system", "visit:os")],
        metrics: SESSION,
        fallback: None,
    },
    Part {
        file: "operating_system_versions.csv",
        dims: &[
            ("operating_system", "visit:os"),
            ("operating_system_version", "visit:os_version"),
        ],
        metrics: SESSION,
        fallback: None,
    },
    Part {
        file: "custom_events.csv",
        dims: &[("name", "event:name")],
        metrics: &["visitors", "events"],
        fallback: None,
    },
    Part {
        file: "conversions.csv",
        dims: &[("goal", "event:goal")],
        metrics: &["visitors", "events", "conversion_rate"],
        fallback: None,
    },
];

/// 请求：看板当前的时间范围与筛选（QuerySpec 的写法），`interval` 是趋势的粒度。
#[derive(Clone, Debug)]
pub struct ExportRequest {
    date_range: Value,
    filters: Value,
    interval: &'static str,
}

impl ExportRequest {
    /// 读请求体 `{"date_range", "filters"?, "interval"?}`；错误与查询的校验错误同形。
    pub fn parse(body: &Value) -> Result<Self, QueryError> {
        let obj = body
            .as_object()
            .ok_or_else(|| QueryError::validation("", "the body must be a JSON object"))?;
        if let Some(k) = obj
            .keys()
            .find(|k| !matches!(k.as_str(), "date_range" | "filters" | "interval" | "site"))
        {
            return Err(QueryError::validation(k, "unknown field"));
        }
        let date_range = obj
            .get("date_range")
            .cloned()
            .ok_or_else(|| QueryError::validation("date_range", "is required"))?;
        if date_range == "realtime" {
            return Err(QueryError::validation(
                "date_range",
                "realtime can't be exported",
            ));
        }
        let filters = obj.get("filters").cloned().unwrap_or_else(|| json!([]));
        let interval = match obj.get("interval").map(Value::as_str) {
            None => "day",
            Some(Some("minute")) => "minute",
            Some(Some("hour")) => "hour",
            Some(Some("day")) => "day",
            Some(Some("week")) => "week",
            Some(Some("month")) => "month",
            Some(_) => {
                return Err(QueryError::validation(
                    "interval",
                    "must be one of minute, hour, day, week, month",
                ));
            }
        };
        let req = ExportRequest {
            date_range,
            filters,
            interval,
        };
        // 时间范围与筛选先按查询的规则校验一次，错误指向请求里的字段
        req.spec(&[], &["visitors"], false)?;
        Ok(req)
    }

    fn spec(&self, dims: &[&str], metrics: &[&str], labels: bool) -> Result<QuerySpec, QueryError> {
        let mut v = json!({
            "metrics": metrics,
            "date_range": self.date_range,
            "filters": self.filters,
            "dimensions": dims,
        });
        if !dims.is_empty() {
            v["pagination"] = json!({"limit": MAX_LIMIT});
        }
        if labels {
            v["include"] = json!({"time_labels": true});
        }
        QuerySpec::parse(&v)
    }

    /// 属性键列表用的查询（时间范围与筛选）。
    pub fn keys_spec(&self) -> Result<QuerySpec, QueryError> {
        self.spec(&[], &["events"], false)
    }
}

/// 导出的结果：zip 的字节与覆盖的日期（站点本地日，`YYYY-MM-DD`）。
pub struct Export {
    pub zip: Vec<u8>,
    pub from: String,
    pub to: String,
}

/// 时长（秒）与滚动深度（0 到 100）取整，比率与平均值保留 4 位小数。
fn cell(metric: &str, v: &MetricValue) -> String {
    match v {
        MetricValue::Int(n) => n.to_string(),
        MetricValue::Float(f)
            if matches!(metric, "visit_duration" | "time_on_page" | "scroll_depth") =>
        {
            format!("{:.0}", f.round())
        }
        MetricValue::Float(f) => {
            let s = format!("{f:.4}");
            s.trim_end_matches('0').trim_end_matches('.').to_owned()
        }
    }
}

/// 维度取值（来自访客的 URL、referrer、属性）以 `= + - @` 开头时，表格软件会把它当公式：前面加 `'`。
fn text(s: String) -> String {
    if s.starts_with(['=', '+', '-', '@', '\t', '\r']) {
        format!("'{s}")
    } else {
        s
    }
}

/// UTF-8 带 BOM（Excel 才能正确识别中文）。
fn csv_bytes(header: &[&str], rows: impl IntoIterator<Item = Vec<String>>) -> Vec<u8> {
    let mut w = csv::Writer::from_writer(b"\xEF\xBB\xBF".to_vec());
    // 写进内存不会失败
    let _ = w.write_record(header);
    for r in rows {
        let _ = w.write_record(&r);
    }
    w.into_inner().unwrap_or_default()
}

/// 查询回显里的范围换成站点本地的起止日（终点不含，所以取前一刻的日期）。
fn local_days(r: &QueryResult) -> (String, String) {
    let tz = TimeZone::get(&r.query.time_zone).unwrap_or(TimeZone::UTC);
    let day = |s: &str, back: bool| {
        s.parse::<Timestamp>()
            .ok()
            .and_then(|t| {
                if back {
                    t.checked_sub(jiff::SignedDuration::from_secs(1)).ok()
                } else {
                    Some(t)
                }
            })
            .map(|t| t.to_zoned(tz.clone()).date().to_string())
            .unwrap_or_default()
    };
    let range = &r.query.resolved_date_range;
    (day(&range.start, false), day(&range.end, true))
}

/// 跑完全部查询并打包。`run` 执行一条查询（调用方决定执行路径与鉴权），`keys` 是要导出的属性键。
pub async fn build<F, Fut>(
    req: &ExportRequest,
    keys: &[String],
    mut run: F,
) -> Result<Export, QueryError>
where
    F: FnMut(QuerySpec) -> Fut,
    Fut: Future<Output = Result<QueryResult, QueryError>>,
{
    let mut files: Vec<(String, Vec<u8>)> = Vec::new();

    // 趋势：没有数据的桶也列出
    let metrics = [
        "visitors",
        "visits",
        "pageviews",
        "views_per_visit",
        "bounce_rate",
        "visit_duration",
    ];
    let time_dim = format!("time:{}", req.interval);
    let trend = run(req.spec(&[&time_dim], &metrics, true)?).await?;
    let (from, to) = local_days(&trend);
    let labels = trend.meta.time_labels.clone().unwrap_or_else(|| {
        trend
            .results
            .iter()
            .map(|r| r.dimensions[0].clone())
            .collect()
    });
    let rows =
        labels.iter().map(|l| {
            let hit = trend.results.iter().find(|r| &r.dimensions[0] == l);
            std::iter::once(l.clone())
                .chain((0..metrics.len()).map(|i| {
                    hit.map_or_else(|| "0".to_owned(), |r| cell(metrics[i], &r.metrics[i]))
                }))
                .collect()
        });
    let mut header = vec!["date"];
    header.extend(metrics);
    files.push(("visitors.csv".into(), csv_bytes(&header, rows)));

    for part in PARTS {
        let dims: Vec<&str> = part.dims.iter().map(|(_, d)| *d).collect();
        let (res, used) = match run(req.spec(&dims, part.metrics, false)?).await {
            Ok(r) => (r, part.metrics),
            Err(QueryError::Validation { .. }) if part.fallback.is_some() => {
                let fb = part.fallback.unwrap_or_default();
                (run(req.spec(&dims, fb, false)?).await?, fb)
            }
            Err(e) => return Err(e),
        };
        let mut header: Vec<&str> = part.dims.iter().map(|(c, _)| *c).collect();
        header.extend(used);
        let rows = res.results.iter().map(|r| {
            r.dimensions
                .iter()
                .cloned()
                .map(text)
                .chain(r.metrics.iter().zip(used).map(|(v, m)| cell(m, v)))
                .collect()
        });
        files.push((part.file.into(), csv_bytes(&header, rows)));
    }

    let mut props = Vec::new();
    let valid_keys = keys.iter().filter_map(|key| {
        let dim = format!("event:props:{key}");
        Dimension::parse(&dim).ok().map(|_| (key, dim))
    });
    for (key, dim) in valid_keys.take(MAX_PROP_KEYS) {
        let res = run(req.spec(&[&dim], &["visitors", "events"], false)?).await?;
        props.extend(res.results.into_iter().map(|r| {
            std::iter::once(text(key.clone()))
                .chain(r.dimensions.into_iter().map(text))
                .chain(
                    r.metrics
                        .iter()
                        .zip(["visitors", "events"])
                        .map(|(v, m)| cell(m, v)),
                )
                .collect::<Vec<_>>()
        }));
    }
    files.push((
        "custom_props.csv".into(),
        csv_bytes(&["property", "value", "visitors", "events"], props),
    ));

    let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let opts = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    for (name, bytes) in files {
        zip.start_file(name, opts)
            .and_then(|()| zip.write_all(&bytes).map_err(Into::into))
            .map_err(|e| QueryError::Internal(anyhow::anyhow!("写 zip 失败：{e}")))?;
    }
    let zip = zip
        .finish()
        .map_err(|e| QueryError::Internal(anyhow::anyhow!("写 zip 失败：{e}")))?
        .into_inner();
    Ok(Export { zip, from, to })
}

/// 下载的文件名：`rivu-<站点名的 ASCII 部分>-<起>-<止>.zip`。
pub fn file_name(site_name: &str, e: &Export) -> String {
    let slug: String = site_name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '-' {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    let slug = slug.trim_matches('-');
    let slug = if slug.is_empty() { "site" } else { slug };
    format!("rivu-{slug}-{}-{}.zip", e.from, e.to)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_are_validated_like_queries() {
        assert!(ExportRequest::parse(&json!({"date_range": "30d"})).is_ok());
        assert!(ExportRequest::parse(&json!({"date_range": "30d", "interval": "month", "filters": [["is", "visit:country", ["CN"]]]})).is_ok());
        for bad in [
            json!({}),
            json!({"date_range": "realtime"}),
            json!({"date_range": "30d", "interval": "year"}),
            json!({"date_range": "30d", "metrics": ["visitors"]}),
            json!({"date_range": "30d", "filters": [["nope"]]}),
            json!([]),
        ] {
            assert!(
                matches!(
                    ExportRequest::parse(&bad),
                    Err(QueryError::Validation { .. })
                ),
                "{bad}"
            );
        }
    }

    #[test]
    fn cells_and_file_names() {
        assert_eq!(cell("visitors", &MetricValue::Int(12)), "12");
        assert_eq!(cell("bounce_rate", &MetricValue::Float(0.70781)), "0.7078");
        assert_eq!(cell("views_per_visit", &MetricValue::Float(124.0)), "124");
        assert_eq!(cell("bounce_rate", &MetricValue::Float(0.0)), "0");
        assert_eq!(cell("visit_duration", &MetricValue::Float(107.7077)), "108");
        let e = Export {
            zip: vec![],
            from: "2026-09-01".into(),
            to: "2026-09-30".into(),
        };
        assert_eq!(
            file_name("Blog.Dejavu.moe", &e),
            "rivu-blog.dejavu.moe-2026-09-01-2026-09-30.zip"
        );
        assert_eq!(
            file_name("中文站点", &e),
            "rivu-site-2026-09-01-2026-09-30.zip"
        );
        assert_eq!(
            csv_bytes(&["a", "b"], [vec!["x,y".into(), "1".into()]]),
            b"\xEF\xBB\xBFa,b\n\"x,y\",1\n"
        );
        assert_eq!(text("=HYPERLINK(1)".into()), "'=HYPERLINK(1)");
        assert_eq!(text("/docs".into()), "/docs");
    }
}
