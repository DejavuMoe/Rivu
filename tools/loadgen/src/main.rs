//! `loadgen`：Rivu 的开环压测生成器。设计与方法见 `docs/research/load-test-2026-10.md`。

mod model;
mod run;
mod stats;

use std::path::PathBuf;
use std::time::Duration;

use clap::Parser;
use serde_json::json;

use crate::model::{Site, load_user_agents};
use crate::run::{ChConfig, Config, PhaseSpec};

#[derive(Debug, Parser)]
#[command(
    name = "loadgen",
    about = "Rivu 开环压测生成器：固定到达率，延迟从计划发送时刻起算"
)]
struct Args {
    /// 公共端口
    #[arg(long, default_value = "http://127.0.0.1:8080")]
    target: String,
    /// 阶段 `名称:每秒事件数:秒数`，可重复，按顺序执行
    #[arg(long = "phase", required_unless_present = "dump", value_parser = PhaseSpec::parse)]
    phases: Vec<PhaseSpec>,
    /// 站点 `KEY:ID:host1,host2`，可重复；第一个站点放探针事件；站点按 Zipf(1) 分配流量
    #[arg(long = "site", required = true, value_parser = Site::parse)]
    sites: Vec<Site>,
    /// 事件之外同时发 POST /api/v1/query 的速率（每秒）；0 表示不发
    #[arg(long, default_value_t = 0.0)]
    query_rate: f64,
    /// 查询的 date_range（三种查询共用）
    #[arg(long, default_value = "7d")]
    query_range: String,
    #[arg(long, env = "LOADGEN_API_KEY", hide_env_values = true)]
    api_key: Option<String>,
    /// 轮询事件可见延迟用的 ClickHouse HTTP 地址；不给就不发探针
    #[arg(long)]
    ch_url: Option<String>,
    #[arg(long, default_value = "rivu")]
    ch_database: String,
    #[arg(long, default_value = "rivu_reader")]
    ch_user: String,
    #[arg(
        long,
        env = "LOADGEN_CH_PASSWORD",
        hide_env_values = true,
        default_value = ""
    )]
    ch_password: String,
    /// 探针间隔（毫秒）；0 关闭
    #[arg(long, default_value_t = 1000)]
    probe_interval_ms: u64,
    /// 探针等待可见的最长时间（秒）
    #[arg(long, default_value_t = 30)]
    probe_timeout_secs: u64,
    /// uap-core 测试集所在目录
    #[arg(long, default_value = "crates/ua/tests/data")]
    ua_dir: PathBuf,
    #[arg(long, default_value_t = 20261023)]
    seed: u64,
    /// 事件调度分片数
    #[arg(long, default_value_t = 4)]
    shards: usize,
    /// 客户端在途请求数上限
    #[arg(long, default_value_t = 30_000)]
    max_inflight: usize,
    /// 最后一个阶段结束后等待在途请求的最长秒数
    #[arg(long, default_value_t = 30)]
    drain_secs: u64,
    /// 结果 JSON 的路径（也会在 stdout 打印摘要）
    #[arg(long)]
    out: Option<PathBuf>,
    /// 不发请求：把确定的事件流（与压测同一个生成器，单个分片）写成 JSON 行
    /// `{"ip","ua","body"}` 到这个文件，供 `tools/replay` 同时回放给 rivu 与 Plausible
    #[arg(long)]
    dump: Option<PathBuf>,
    /// `--dump` 写多少个事件
    #[arg(long, default_value_t = 20_000)]
    dump_events: usize,
}

/// `--dump`：单个生成器分片，按种子确定地产生事件，写 JSON 行。
fn dump(
    path: &std::path::Path,
    n: usize,
    seed: u64,
    sites: &[Site],
    uas: &[String],
) -> anyhow::Result<()> {
    use std::io::Write;
    use std::sync::Arc;

    use crate::model::{Generator, Zipf};
    let mut g = Generator::new(
        seed,
        Arc::new(sites.to_vec()),
        Arc::new(Zipf::new(sites.len(), 1.0)),
        Arc::new(Zipf::new(2000, 1.1)),
        uas.len(),
        2000,
    );
    let mut w = std::io::BufWriter::new(std::fs::File::create(path)?);
    for _ in 0..n {
        let e = g.next();
        let line = json!({"ip": e.ip.to_string(), "ua": uas[e.ua_idx], "body": e.body});
        writeln!(w, "{line}")?;
    }
    w.flush()?;
    eprintln!("已写入 {n} 个事件到 {}", path.display());
    Ok(())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let a = Args::parse();
    let uas = load_user_agents(&a.ua_dir)?;
    eprintln!("UA 池：{} 个（来自 {}）", uas.len(), a.ua_dir.display());
    if let Some(path) = &a.dump {
        return dump(path, a.dump_events, a.seed, &a.sites, &uas);
    }
    let ch = a.ch_url.clone().map(|url| ChConfig {
        url: url.trim_end_matches('/').to_owned(),
        database: a.ch_database.clone(),
        user: a.ch_user.clone(),
        password: a.ch_password.clone(),
    });
    let cfg = Config {
        target: a.target.clone(),
        phases: a.phases.clone(),
        sites: a.sites.clone(),
        uas,
        seed: a.seed,
        shards: a.shards,
        max_inflight: a.max_inflight,
        query_rate: a.query_rate,
        query_range: a.query_range.clone(),
        api_key: a.api_key.clone(),
        ch,
        probe_interval: Duration::from_millis(a.probe_interval_ms),
        probe_timeout: Duration::from_secs(a.probe_timeout_secs),
        drain: Duration::from_secs(a.drain_secs),
    };
    let started = std::time::SystemTime::now();
    let phases = run::run(cfg).await?;
    let result = json!({
        "tool": "loadgen",
        "started_unix_ms": started
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(0)),
        "target": a.target,
        "seed": a.seed,
        "shards": a.shards,
        "query_rate_per_s": a.query_rate,
        "query_range": a.query_range,
        "phases": phases.iter().map(|p| p.to_json()).collect::<Vec<_>>(),
    });
    let text = serde_json::to_string_pretty(&result)?;
    if let Some(out) = &a.out {
        std::fs::write(out, &text)?;
    }
    for p in &phases {
        let i = &p.ingest;
        println!(
            "[{}] 目标 {}/s × {}s：计划 {} 完成 {} | 202 接受 {} 丢弃 {} | 503 {} 400 {} 其他 {} 错误 {} | 延迟(计划时刻起) p50 {:.1} p95 {:.1} p99 {:.1} p99.9 {:.1} max {:.1} ms | 可见延迟 p50 {:.0} p95 {:.0} ms（{} 个探针） | 查询 p50 {:.0} p95 {:.0} ms",
            p.name,
            p.rate,
            p.secs,
            i.planned.get(),
            i.completed(),
            i.accepted.get(),
            i.dropped.get(),
            i.s503.get(),
            i.s400.get(),
            i.other.get(),
            i.errors.get(),
            i.latency.quantile_ms(0.5),
            i.latency.quantile_ms(0.95),
            i.latency.quantile_ms(0.99),
            i.latency.quantile_ms(0.999),
            i.latency.quantile_ms(1.0),
            p.visibility.quantile_ms(0.5),
            p.visibility.quantile_ms(0.95),
            p.visibility.count(),
            p.query_all.quantile_ms(0.5),
            p.query_all.quantile_ms(0.95),
        );
    }
    Ok(())
}
