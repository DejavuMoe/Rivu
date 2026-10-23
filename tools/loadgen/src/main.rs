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
    #[arg(long = "phase", required = true, value_parser = PhaseSpec::parse)]
    phases: Vec<PhaseSpec>,
    /// 站点 `KEY:ID:host1,host2`，可重复；第一个站点放探针事件；站点按 Zipf(1) 分配流量
    #[arg(long = "site", required = true, value_parser = Site::parse)]
    sites: Vec<Site>,
    /// 事件之外同时发 POST /api/v1/query 的速率（每秒）；0 表示不发
    #[arg(long, default_value_t = 0.0)]
    query_rate: f64,
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
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let a = Args::parse();
    let uas = load_user_agents(&a.ua_dir)?;
    eprintln!("UA 池：{} 个（来自 {}）", uas.len(), a.ua_dir.display());
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
