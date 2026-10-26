//! 边缘访问日志与 `ingest_counters` 的严格对账（`docs/brief-p1.md` 验收第 1 条）。
//!
//! 输入是 Caddy 的 JSON 访问日志（`deploy/Caddyfile`：删除客户端 IP 与请求头，保留响应头）。只看 `POST /api/event`：
//!
//! | 边缘看到的 | 应等于 `ingest_counters` 里的 |
//! |---|---|
//! | 202 且没有 `x-rivu-dropped` | `accepted` |
//! | 202 且有 `x-rivu-dropped` | 除 `dropped_overloaded` 之外全部 `dropped_*` 之和 |
//! | 503 | `dropped_overloaded` |
//!
//! 其余状态不进 `ingest_counters`，单独列出：400（协议错误，只进 Prometheus）、408 / 413 / 414 / 431（HTTP 层）、
//! 502 / 504（上游不可用：Caddy 重试 `lb_try_duration` 之后仍失败，事件丢失）。
//!
//! 计数窗口是日志里第一个与最后一个请求所在的分钟（含），站点限定为 `--sites` 加上 `site_id = 0`（未知站点，
//! `--exclude-unknown-site` 去掉）。
//! 所以对账期间这个 ClickHouse 库里不能有别的流量写这些站点和 `site_id = 0`。逐分钟的差异只做参考：
//! 边缘按 Caddy 收到请求的时刻分钟，计数按 rivu 收到的时刻分钟，分钟边界上的请求可能落到相邻分钟。

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::Parser;
use rivu_common::config::{ChAccount, Secret};
use serde::Serialize;
use serde_json::Value;

#[derive(Parser, Debug)]
#[command(about = "Caddy 访问日志与 ingest_counters 的严格对账")]
struct Args {
    /// Caddy 的 JSON 访问日志文件
    #[arg(long)]
    caddy_log: PathBuf,
    /// 参与对账的站点 id（逗号分隔）；默认还包含 `site_id = 0`（未知站点）
    #[arg(long, value_delimiter = ',')]
    sites: Vec<u64>,
    /// 不包含 `site_id = 0`：共享的 ClickHouse 里还有别的采集流量时用，此时边缘日志也不能含未知站点的请求
    #[arg(long)]
    exclude_unknown_site: bool,
    #[arg(long, default_value = "http://127.0.0.1:8123")]
    ch_url: String,
    #[arg(long, default_value = "rivu")]
    ch_database: String,
    #[arg(long, default_value = "rivu_reader")]
    ch_user: String,
    #[arg(long, env = "RECONCILE_CH_PASSWORD", hide_env_values = true)]
    ch_password: String,
    /// 结果 JSON 的输出路径（可选）
    #[arg(long)]
    out: Option<PathBuf>,
}

/// 边缘的一类结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
enum Edge {
    Accepted,
    Dropped,
    Overloaded,
    /// 400：协议错误
    Rejected,
    /// 408 / 413 / 414 / 431
    HttpLayer,
    /// 502 / 504
    UpstreamFailed,
    Other,
}

/// 一行访问日志里对账需要的部分；不是 `POST /api/event` 的行返回 `None`。
fn classify(line: &str) -> Option<(f64, Edge, u16)> {
    let v: Value = serde_json::from_str(line).ok()?;
    if !v.get("logger")?.as_str()?.starts_with("http.log.access") {
        return None;
    }
    let req = v.get("request")?;
    if req.get("method")?.as_str()? != "POST" {
        return None;
    }
    let uri = req.get("uri")?.as_str()?;
    if uri.split('?').next()? != "/api/event" {
        return None;
    }
    let ts = v.get("ts")?.as_f64()?;
    let status = u16::try_from(v.get("status")?.as_u64()?).ok()?;
    let dropped = v
        .get("resp_headers")
        .and_then(Value::as_object)
        .is_some_and(|h| h.keys().any(|k| k.eq_ignore_ascii_case("x-rivu-dropped")));
    let edge = match status {
        202 if dropped => Edge::Dropped,
        202 => Edge::Accepted,
        503 => Edge::Overloaded,
        400 => Edge::Rejected,
        408 | 413 | 414 | 431 => Edge::HttpLayer,
        502 | 504 => Edge::UpstreamFailed,
        _ => Edge::Other,
    };
    Some((ts, edge, status))
}

/// 一分钟的 (接受, 丢弃, 503 / overloaded)。
type Minute = (u64, u64, u64);

#[derive(Debug, Default, Serialize)]
struct EdgeTally {
    by_class: BTreeMap<String, u64>,
    by_status: BTreeMap<u16, u64>,
    /// 分钟（Unix 秒）-> (接受, 丢弃, 503)
    per_minute: BTreeMap<i64, Minute>,
    first_ts: Option<f64>,
    last_ts: Option<f64>,
    unparsable_lines: u64,
}

fn tally(reader: impl BufRead) -> Result<EdgeTally> {
    let mut t = EdgeTally::default();
    for line in reader.lines() {
        let line = line.context("读取访问日志失败")?;
        if line.trim().is_empty() {
            continue;
        }
        if serde_json::from_str::<Value>(&line).is_err() {
            t.unparsable_lines += 1;
            continue;
        }
        let Some((ts, edge, status)) = classify(&line) else {
            continue;
        };
        *t.by_class
            .entry(
                serde_json::to_value(edge)?
                    .as_str()
                    .unwrap_or("?")
                    .to_owned(),
            )
            .or_default() += 1;
        *t.by_status.entry(status).or_default() += 1;
        let minute = (ts / 60.0).floor() as i64 * 60;
        let m = t.per_minute.entry(minute).or_default();
        match edge {
            Edge::Accepted => m.0 += 1,
            Edge::Dropped => m.1 += 1,
            Edge::Overloaded => m.2 += 1,
            _ => {}
        }
        t.first_ts = Some(t.first_ts.map_or(ts, |f| f.min(ts)));
        t.last_ts = Some(t.last_ts.map_or(ts, |l| l.max(ts)));
    }
    Ok(t)
}

#[derive(clickhouse::Row, serde::Deserialize, Debug)]
struct CounterRow {
    minute: u32,
    metric: String,
    total: u64,
}

#[derive(Debug, Serialize)]
struct Check {
    name: &'static str,
    edge: u64,
    counters: u64,
    ok: bool,
}

#[derive(Debug, Serialize)]
struct Report {
    window: (i64, i64),
    sites: Vec<u64>,
    edge: EdgeTally,
    counters_by_metric: BTreeMap<String, u64>,
    checks: Vec<Check>,
    /// 逐分钟的差异（参考）：分钟、边缘 (接受, 丢弃, 503)、计数 (接受, 丢弃, overloaded)
    minute_diffs: Vec<(i64, Minute, Minute)>,
    ok: bool,
}

async fn run(args: Args) -> Result<Report> {
    let file = std::fs::File::open(&args.caddy_log)
        .with_context(|| format!("打开 {} 失败", args.caddy_log.display()))?;
    let edge = tally(BufReader::new(file))?;
    let (Some(first), Some(last)) = (edge.first_ts, edge.last_ts) else {
        anyhow::bail!("访问日志里没有 POST /api/event");
    };
    let from = (first / 60.0).floor() as i64 * 60;
    let to = (last / 60.0).floor() as i64 * 60 + 60;

    let account = ChAccount {
        user: args.ch_user.clone(),
        password: Secret::new(args.ch_password.clone()),
    };
    let ch = rivu_storage_ch::client(&args.ch_url, &account, Some(&args.ch_database));
    let mut sites = args.sites.clone();
    if !args.exclude_unknown_site {
        sites.push(0);
    }
    sites.sort_unstable();
    sites.dedup();
    let rows: Vec<CounterRow> = ch
        .query(
            "SELECT toUInt32(toUnixTimestamp(minute)) AS minute, toString(metric) AS metric, sum(value) AS total
             FROM ingest_counters
             WHERE minute >= toDateTime({from:Int64}) AND minute < toDateTime({to:Int64})
               AND has({sites:Array(UInt64)}, site_id)
             GROUP BY minute, metric ORDER BY minute, metric",
        )
        .param("from", from)
        .param("to", to)
        .param("sites", &sites)
        .fetch_all()
        .await
        .context("查询 ingest_counters 失败")?;

    let mut by_metric: BTreeMap<String, u64> = BTreeMap::new();
    let mut ctr_minute: BTreeMap<i64, Minute> = BTreeMap::new();
    for r in &rows {
        *by_metric.entry(r.metric.clone()).or_default() += r.total;
        let m = ctr_minute.entry(i64::from(r.minute)).or_default();
        if r.metric == "accepted" {
            m.0 += r.total;
        } else if r.metric == "dropped_overloaded" {
            m.2 += r.total;
        } else if r.metric.starts_with("dropped_") {
            m.1 += r.total;
        }
    }
    let class = |k: &str| edge.by_class.get(k).copied().unwrap_or(0);
    let accepted = by_metric.get("accepted").copied().unwrap_or(0);
    let overloaded = by_metric.get("dropped_overloaded").copied().unwrap_or(0);
    let dropped: u64 = by_metric
        .iter()
        .filter(|(k, _)| k.starts_with("dropped_") && *k != "dropped_overloaded")
        .map(|(_, v)| v)
        .sum();
    let check = |name, edge: u64, counters: u64| Check {
        name,
        edge,
        counters,
        ok: edge == counters,
    };
    let checks = vec![
        check("202 未丢弃 = accepted", class("accepted"), accepted),
        check(
            "202 + x-rivu-dropped = dropped_*（不含 overloaded）",
            class("dropped"),
            dropped,
        ),
        check("503 = dropped_overloaded", class("overloaded"), overloaded),
        check(
            "合计：202 + 503 = 全部计数",
            class("accepted") + class("dropped") + class("overloaded"),
            accepted + dropped + overloaded,
        ),
    ];
    let mut minute_diffs = Vec::new();
    let minutes: std::collections::BTreeSet<i64> = edge
        .per_minute
        .keys()
        .chain(ctr_minute.keys())
        .copied()
        .collect();
    for m in minutes {
        let e = edge.per_minute.get(&m).copied().unwrap_or_default();
        let c = ctr_minute.get(&m).copied().unwrap_or_default();
        if e != c {
            minute_diffs.push((m, e, c));
        }
    }
    let ok = checks.iter().all(|c| c.ok);
    Ok(Report {
        window: (from, to),
        sites,
        edge,
        counters_by_metric: by_metric,
        checks,
        minute_diffs,
        ok,
    })
}

fn print(r: &Report) {
    println!(
        "对账窗口：[{}, {})（Unix 秒，按分钟），站点 {:?}",
        r.window.0, r.window.1, r.sites
    );
    println!("边缘（POST /api/event）按类别：{:?}", r.edge.by_class);
    println!("边缘按状态码：{:?}", r.edge.by_status);
    if r.edge.unparsable_lines > 0 {
        println!("无法解析的日志行：{}", r.edge.unparsable_lines);
    }
    println!("ingest_counters 按指标：{:?}", r.counters_by_metric);
    for c in &r.checks {
        println!(
            "  [{}] {}：边缘 {} / 计数 {}",
            if c.ok { "OK" } else { "差异" },
            c.name,
            c.edge,
            c.counters
        );
    }
    if r.minute_diffs.is_empty() {
        println!("逐分钟：全部一致");
    } else {
        println!("逐分钟不一致（参考，分钟边界的请求可能落到相邻分钟）：");
        for (m, e, c) in &r.minute_diffs {
            println!("  {m}: 边缘 {e:?} / 计数 {c:?}");
        }
    }
    println!("结论：{}", if r.ok { "严格一致" } else { "不一致" });
}

#[tokio::main]
async fn main() -> ExitCode {
    let args = Args::parse();
    let out = args.out.clone();
    match run(args).await {
        Ok(report) => {
            print(&report);
            if let Some(path) = out
                && let Err(e) = std::fs::write(
                    &path,
                    serde_json::to_string_pretty(&report).unwrap_or_default(),
                )
            {
                eprintln!("写 {} 失败：{e}", path.display());
            }
            if report.ok {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
        Err(e) => {
            eprintln!("对账失败：{e:#}");
            ExitCode::from(2)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(status: u16, uri: &str, method: &str, dropped: bool, ts: f64) -> String {
        let mut resp = serde_json::json!({"Content-Length": ["0"]});
        if dropped {
            resp["X-Rivu-Dropped"] = serde_json::json!(["1"]);
        }
        serde_json::json!({
            "level": "info", "ts": ts, "logger": "http.log.access.log0", "msg": "handled request",
            "request": {"proto": "HTTP/1.1", "method": method, "host": "stats.example.test", "uri": uri},
            "status": status, "resp_headers": resp, "duration": 0.001, "size": 0,
        })
        .to_string()
    }

    #[test]
    fn classifies_event_requests_by_status_and_dropped_header() {
        assert_eq!(
            classify(&line(202, "/api/event", "POST", false, 1.0))
                .unwrap()
                .1,
            Edge::Accepted
        );
        assert_eq!(
            classify(&line(202, "/api/event", "POST", true, 1.0))
                .unwrap()
                .1,
            Edge::Dropped
        );
        assert_eq!(
            classify(&line(503, "/api/event", "POST", false, 1.0))
                .unwrap()
                .1,
            Edge::Overloaded
        );
        assert_eq!(
            classify(&line(400, "/api/event?x=1", "POST", false, 1.0))
                .unwrap()
                .1,
            Edge::Rejected
        );
        assert_eq!(
            classify(&line(413, "/api/event", "POST", false, 1.0))
                .unwrap()
                .1,
            Edge::HttpLayer
        );
        assert_eq!(
            classify(&line(502, "/api/event", "POST", false, 1.0))
                .unwrap()
                .1,
            Edge::UpstreamFailed
        );
        assert!(classify(&line(204, "/api/event", "OPTIONS", false, 1.0)).is_none());
        assert!(classify(&line(200, "/js/abc.js", "GET", false, 1.0)).is_none());
        assert!(classify(&line(202, "/api/events", "POST", false, 1.0)).is_none());
        assert!(classify(r#"{"logger":"http.handlers.reverse_proxy","msg":"x"}"#).is_none());
    }

    #[test]
    fn tally_counts_per_minute_and_window() {
        let log = [
            line(202, "/api/event", "POST", false, 120.5),
            line(202, "/api/event", "POST", true, 150.0),
            line(503, "/api/event", "POST", false, 179.9),
            line(202, "/api/event", "POST", false, 180.0),
            line(400, "/api/event", "POST", false, 181.0),
            "not json".to_owned(),
        ]
        .join("\n");
        let t = tally(log.as_bytes()).unwrap();
        assert_eq!(t.per_minute[&120], (1, 1, 1));
        assert_eq!(t.per_minute[&180], (1, 0, 0));
        assert_eq!(t.by_class["rejected"], 1);
        assert_eq!(t.by_status[&202], 3);
        assert_eq!((t.first_ts, t.last_ts), (Some(120.5), Some(181.0)));
        assert_eq!(t.unparsable_lines, 1);
    }
}
