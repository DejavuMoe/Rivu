//! 与 Plausible CE 并行回放（`docs/brief-p1.md` 验收：「与 Plausible CE 并行回放同一请求流，所有差异都能对应到刻意偏离清单」）。
//!
//! - `send`：读 `loadgen --dump` 写的事件流（每行 `{ip, ua, body}`，body 是 Rivu 线协议），按固定速率、同样的顺序，
//!   把每个事件**同时**发给 rivu（`POST /api/event`，原样）与 Plausible CE（`POST /api/event`，换成 Plausible 的字段），
//!   两边都带同一个 `User-Agent` 与 `X-Forwarded-For`。
//! - `compare`：两边都写完之后，按站点取同一天的核心指标与维度分组（rivu 用公开查询 API，Plausible 用公开站点的内部统计 API），
//!   逐项列出差异，供对照 `system-design.md` 附录 C 的刻意偏离清单。
//!
//! 字段换算（Plausible 的事件 API，`docs/research/plausible/01-ingestion-pipeline.md` §2.2）：
//! `n` → `name`，`u` → `url`，`r` → `referrer`，`p` → `props`，`i` → `interactive`，`h` → `hashMode`，`sd` / `e` 原样；
//! `domain` 取 URL 的 host 去掉一个前导 `www.`（Plausible 的站点按这个域名注册）。

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use serde_json::{Map, Value, json};

#[derive(Parser, Debug)]
#[command(about = "rivu 与 Plausible CE 的并行回放与对比")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// 回放事件流
    Send {
        /// `loadgen --dump` 写的 JSON 行
        #[arg(long)]
        input: PathBuf,
        /// rivu 的公共地址，例如 http://127.0.0.1:8080
        #[arg(long)]
        rivu: String,
        /// Plausible CE 的地址，例如 http://127.0.0.1:8000
        #[arg(long)]
        plausible: String,
        /// 每秒事件数
        #[arg(long, default_value_t = 50.0)]
        rate: f64,
        /// 发送结果写到这里（每个事件两边的状态与是否被丢弃）
        #[arg(long)]
        out: PathBuf,
    },
    /// 对比两边的统计
    Compare {
        #[arg(long)]
        rivu: String,
        #[arg(long, env = "REPLAY_RIVU_API_KEY", hide_env_values = true)]
        rivu_api_key: String,
        #[arg(long)]
        plausible: String,
        /// 站点对 `rivu_site_key=plausible_domain`，可重复
        #[arg(long = "site", value_parser = parse_pair)]
        sites: Vec<(String, String)>,
        /// 统计日期（两边的站点时区都是 UTC）
        #[arg(long)]
        date: String,
        #[arg(long)]
        out: PathBuf,
    },
}

fn parse_pair(s: &str) -> Result<(String, String), String> {
    s.split_once('=')
        .map(|(a, b)| (a.to_owned(), b.to_owned()))
        .ok_or_else(|| format!("应为 key=domain：{s}"))
}

/// Rivu 线协议 → Plausible 事件 API 的请求体。
fn to_plausible(body: &Value) -> Result<Value> {
    let o = body.as_object().context("事件不是 JSON 对象")?;
    let u = o.get("u").and_then(Value::as_str).context("缺少 u")?;
    let host = url::Url::parse(u)?
        .host_str()
        .context("URL 没有 host")?
        .to_ascii_lowercase();
    let domain = host.strip_prefix("www.").unwrap_or(&host).to_owned();
    let mut p = Map::new();
    p.insert("name".into(), o.get("n").cloned().unwrap_or(Value::Null));
    p.insert("url".into(), Value::String(u.to_owned()));
    p.insert("domain".into(), Value::String(domain));
    for (from, to) in [
        ("r", "referrer"),
        ("p", "props"),
        ("i", "interactive"),
        ("h", "hashMode"),
        ("sd", "sd"),
        ("e", "e"),
    ] {
        if let Some(v) = o.get(from) {
            p.insert(to.into(), v.clone());
        }
    }
    Ok(Value::Object(p))
}

#[derive(Debug, Default, serde::Serialize)]
struct SideTally {
    by_status: BTreeMap<u16, u64>,
    dropped: u64,
    errors: u64,
}

async fn post(
    client: &reqwest::Client,
    url: &str,
    body: String,
    ua: &str,
    ip: &str,
    dropped_header: &str,
) -> Result<(u16, bool)> {
    let r = client
        .post(url)
        .header("content-type", "text/plain")
        .header("user-agent", ua)
        .header("x-forwarded-for", ip)
        .body(body)
        .send()
        .await?;
    let dropped = r.headers().contains_key(dropped_header);
    Ok((r.status().as_u16(), dropped))
}

async fn send(
    input: PathBuf,
    rivu: String,
    plausible: String,
    rate: f64,
    out: PathBuf,
) -> Result<()> {
    let lines: Vec<Value> = std::fs::read_to_string(&input)?
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(serde_json::from_str)
        .collect::<Result<_, _>>()?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()?;
    let rivu_url = format!("{}/api/event", rivu.trim_end_matches('/'));
    let pl_url = format!("{}/api/event", plausible.trim_end_matches('/'));
    let tallies = Arc::new(tokio::sync::Mutex::new((
        SideTally::default(),
        SideTally::default(),
    )));
    let mismatches = Arc::new(tokio::sync::Mutex::new(Vec::<Value>::new()));
    let period = Duration::from_secs_f64(1.0 / rate);
    let t0 = Instant::now();
    let mut tasks = Vec::with_capacity(lines.len());
    for (i, line) in lines.iter().enumerate() {
        // 按计划时刻发送（开环），两边同时发
        let at = t0 + period.mul_f64(i as f64);
        tokio::time::sleep_until(at.into()).await;
        let ua = line["ua"].as_str().unwrap_or_default().to_owned();
        let ip = line["ip"].as_str().unwrap_or_default().to_owned();
        let body: Value = serde_json::from_str(line["body"].as_str().unwrap_or("{}"))?;
        let pl_body = to_plausible(&body)?.to_string();
        let name = body["n"].as_str().unwrap_or_default().to_owned();
        let mismatches = Arc::clone(&mismatches);
        let (client, rivu_url, pl_url, tallies) = (
            client.clone(),
            rivu_url.clone(),
            pl_url.clone(),
            Arc::clone(&tallies),
        );
        tasks.push(tokio::spawn(async move {
            let (a, b) = tokio::join!(
                post(
                    &client,
                    &rivu_url,
                    body.to_string(),
                    &ua,
                    &ip,
                    "x-rivu-dropped"
                ),
                post(&client, &pl_url, pl_body, &ua, &ip, "x-plausible-dropped"),
            );
            // 两边判定不同（一边接受、一边丢弃）的事件记下来，供逐条归因
            if let (Ok((_, da)), Ok((_, db))) = (&a, &b)
                && da != db
            {
                mismatches.lock().await.push(
                    json!({"i": i, "n": name, "ua": ua, "rivu_dropped": da, "plausible_dropped": db}),
                );
            }
            let mut guard = tallies.lock().await;
            let (rv, pl) = &mut *guard;
            for (side, res) in [(rv, a), (pl, b)] {
                match res {
                    Ok((status, dropped)) => {
                        *side.by_status.entry(status).or_default() += 1;
                        if dropped {
                            side.dropped += 1;
                        }
                    }
                    Err(_) => side.errors += 1,
                }
            }
        }));
        if i % 1000 == 999 {
            eprintln!("已发送 {} / {}", i + 1, lines.len());
        }
    }
    for t in tasks {
        let _ = t.await;
    }
    let t = tallies.lock().await;
    let mut mm = mismatches.lock().await;
    mm.sort_by_key(|v| v["i"].as_u64());
    let report = json!({"events": lines.len(), "rivu": t.0, "plausible": t.1,
                        "elapsed_secs": t0.elapsed().as_secs_f64(), "mismatches": mm.len()});
    std::fs::write(
        out.with_extension("mismatches.jsonl"),
        mm.iter().map(|v| v.to_string() + "\n").collect::<String>(),
    )?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    std::fs::write(out, serde_json::to_string_pretty(&report)?)?;
    Ok(())
}

async fn get_json(client: &reqwest::Client, url: &str) -> Result<Value> {
    let r = client.get(url).send().await?;
    let status = r.status();
    let text = r.text().await?;
    anyhow::ensure!(
        status.is_success(),
        "{url} 返回 {status}：{}",
        &text[..text.len().min(300)]
    );
    Ok(serde_json::from_str(&text)?)
}

/// Plausible 内部统计 API 的分组结果：兼容 `[...]` 与 `{"results": [...]}` 两种形态，取 `name` 与 `visitors`；
/// 国家优先取两位代码 `code`，与 rivu 的取值对齐。
fn plausible_rows(v: &Value) -> BTreeMap<String, u64> {
    let arr = v
        .get("results")
        .and_then(Value::as_array)
        .or_else(|| v.as_array())
        .cloned()
        .unwrap_or_default();
    arr.iter()
        .filter_map(|r| {
            Some((
                r.get("code")
                    .and_then(Value::as_str)
                    .or_else(|| r.get("name")?.as_str())?
                    .to_owned(),
                r.get("visitors")?.as_u64()?,
            ))
        })
        .collect()
}

/// 差异摘要：核心指标两边的值；每个维度两边的访客合计、只出现在一边的值、计数不同的值（最多 15 个）。
fn summarize(r: &Map<String, Value>, p_top: &Value, dims: &BTreeMap<&str, Value>) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    let p = |name: &str| {
        p_top["top_stats"]
            .as_array()
            .and_then(|a| a.iter().find(|s| s["graph_metric"] == name))
            .map(|s| s["value"].clone())
            .unwrap_or(Value::Null)
    };
    for (k, scale) in [
        ("visitors", 1.0),
        ("visits", 1.0),
        ("pageviews", 1.0),
        ("bounce_rate", 100.0),
        ("visit_duration", 1.0),
    ] {
        let rv = r.get(k).and_then(Value::as_f64).map(|x| x * scale);
        let _ = writeln!(out, "  {k}: rivu {rv:?} / plausible {}", p(k));
    }
    for (dim, v) in dims {
        let rows = |side: &str| -> BTreeMap<String, u64> {
            v[side]
                .as_object()
                .map(|o| {
                    o.iter()
                        .filter_map(|(k, x)| Some((k.clone(), x.as_u64()?)))
                        .collect()
                })
                .unwrap_or_default()
        };
        let (a, b) = (rows("rivu"), rows("plausible"));
        let only_r: Vec<_> = a.keys().filter(|k| !b.contains_key(*k)).cloned().collect();
        let only_p: Vec<_> = b.keys().filter(|k| !a.contains_key(*k)).cloned().collect();
        let diff: Vec<String> = a
            .iter()
            .filter_map(|(k, x)| {
                b.get(k)
                    .filter(|y| *y != x)
                    .map(|y| format!("{k}: {x}/{y}"))
            })
            .collect();
        let _ = writeln!(
            out,
            "  {dim}: 合计 rivu {} / plausible {}；只在 rivu {:?}；只在 plausible {:?}；不同 {} 个 {:?}",
            a.values().sum::<u64>(),
            b.values().sum::<u64>(),
            only_r.iter().take(10).collect::<Vec<_>>(),
            only_p.iter().take(10).collect::<Vec<_>>(),
            diff.len(),
            diff.iter().take(15).collect::<Vec<_>>()
        );
    }
    out
}

#[allow(clippy::too_many_arguments)]
async fn compare(
    rivu: String,
    key: String,
    plausible: String,
    sites: Vec<(String, String)>,
    date: String,
    out: PathBuf,
) -> Result<()> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(60))
        .build()?;
    let pl = plausible.trim_end_matches('/');
    let query = |body: Value| {
        let client = client.clone();
        let url = format!("{}/api/v1/query", rivu.trim_end_matches('/'));
        let key = key.clone();
        async move {
            let r = client
                .post(&url)
                .bearer_auth(&key)
                .json(&body)
                .send()
                .await?;
            let status = r.status();
            let v: Value = r.json().await?;
            anyhow::ensure!(status.is_success(), "rivu 查询返回 {status}：{v}");
            Ok::<Value, anyhow::Error>(v)
        }
    };
    let mut report = Vec::new();
    for (site, domain) in sites {
        let range = json!([date, date]);
        let metrics = [
            "visitors",
            "visits",
            "pageviews",
            "bounce_rate",
            "visit_duration",
        ];
        let r_top = query(json!({"site": site, "metrics": metrics, "date_range": range})).await?;
        let r_vals = r_top["results"][0]["metrics"].clone();
        let p_top = get_json(
            &client,
            &format!("{pl}/api/stats/{domain}/top-stats?period=day&date={date}"),
        )
        .await?;
        let mut dims = BTreeMap::new();
        for (rdim, pseg) in [
            ("event:page", "pages"),
            ("visit:source", "sources"),
            ("visit:channel", "channels"),
            ("visit:country", "countries"),
            ("visit:device", "screen-sizes"),
            ("visit:browser", "browsers"),
            ("visit:os", "operating-systems"),
        ] {
            let rv = query(
                json!({"site": site, "metrics": ["visitors"], "dimensions": [rdim],
                                  "date_range": range, "pagination": {"limit": 1000}}),
            )
            .await?;
            let rrows: BTreeMap<String, u64> = rv["results"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .iter()
                .filter_map(|x| {
                    Some((
                        x["dimensions"][0].as_str()?.to_owned(),
                        x["metrics"][0].as_u64()?,
                    ))
                })
                .collect();
            let pv = get_json(
                &client,
                &format!("{pl}/api/stats/{domain}/{pseg}?period=day&date={date}&limit=1000"),
            )
            .await
            .unwrap_or(Value::Null);
            dims.insert(
                rdim,
                json!({"rivu": rrows, "plausible": plausible_rows(&pv)}),
            );
        }
        let rmetrics: Map<String, Value> = metrics
            .iter()
            .zip(r_vals.as_array().cloned().unwrap_or_default())
            .map(|(k, v)| (k.to_string(), v))
            .collect();
        let summary = summarize(&rmetrics, &p_top, &dims);
        println!("== {site} / {domain}\n{summary}");
        report.push(json!({
            "site": site, "domain": domain,
            "rivu_metrics": rmetrics,
            "plausible_top_stats": p_top,
            "breakdowns": dims,
            "summary": summary,
        }));
    }
    let text = serde_json::to_string_pretty(&report)?;
    std::fs::write(out, &text)?;
    println!("{text}");
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    match Cli::parse().cmd {
        Cmd::Send {
            input,
            rivu,
            plausible,
            rate,
            out,
        } => send(input, rivu, plausible, rate, out).await,
        Cmd::Compare {
            rivu,
            rivu_api_key,
            plausible,
            sites,
            date,
            out,
        } => compare(rivu, rivu_api_key, plausible, sites, date, out).await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_wire_bodies_to_plausible_events() {
        let b = json!({"v":1,"sv":1,"s":"k","n":"pageview","u":"https://www.Site1.test/a?utm_source=x",
                       "r":"https://www.google.com/"});
        assert_eq!(
            to_plausible(&b).unwrap(),
            json!({"name":"pageview","url":"https://www.Site1.test/a?utm_source=x","domain":"site1.test",
                   "referrer":"https://www.google.com/"})
        );
        let e = json!({"v":1,"s":"k","n":"engagement","u":"https://site1.test/a","sd":40,"e":1200});
        let p = to_plausible(&e).unwrap();
        assert_eq!((p["sd"].clone(), p["e"].clone()), (json!(40), json!(1200)));
        let c = json!({"v":1,"s":"k","n":"Signup","u":"https://site1.test/a","p":{"plan":"pro"},"i":false});
        let p = to_plausible(&c).unwrap();
        assert_eq!(p["props"], json!({"plan":"pro"}));
        assert_eq!(p["interactive"], json!(false));
    }

    #[test]
    fn reads_both_plausible_breakdown_shapes() {
        let a = json!([{"name":"/a","visitors":3},{"name":"/b","visitors":1}]);
        let b = json!({"results":[{"name":"/a","visitors":3}]});
        assert_eq!(plausible_rows(&a).len(), 2);
        assert_eq!(plausible_rows(&b)["/a"], 3);
    }
}
