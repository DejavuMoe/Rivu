//! 单次解析耗时（对照 `system-design.md` §7：缓存命中约 1 µs、未命中 50–200 µs）。
//!
//! 默认忽略，要在 release 下跑：
//! `cargo test -p rivu-ua --release --test perf -- --ignored --nocapture`

use std::hint::black_box;
use std::sync::Arc;
use std::time::Instant;

use rivu_ua::UaParser;
use serde::Deserialize;

const DATA: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/");

#[derive(Deserialize)]
struct Cases {
    test_cases: Vec<Case>,
}
#[derive(Deserialize)]
struct Case {
    user_agent_string: String,
}

fn percentile(sorted: &[u128], p: f64) -> u128 {
    sorted[((sorted.len() - 1) as f64 * p) as usize]
}

#[test]
#[ignore = "性能测量，release 下手动运行"]
fn per_call_latency() {
    let t = Instant::now();
    let parser = UaParser::embedded().unwrap();
    println!("embedded() 构建耗时 {:?}", t.elapsed());

    // 真实分布的 UA：uap-core 的 test_ua.yaml（浏览器形态的）。
    let text = std::fs::read_to_string(format!("{DATA}uap-test_ua.yaml")).unwrap();
    let uas: Vec<String> = serde_saphyr::from_str::<Cases>(&text)
        .unwrap()
        .test_cases
        .into_iter()
        .map(|c| c.user_agent_string)
        .filter(|u| u.starts_with("Mozilla/"))
        .collect();
    println!("样本 UA {} 条", uas.len());

    // 未命中：parse_uncached，逐条计时，3 轮取分位。
    let mut ns = Vec::new();
    for _ in 0..3 {
        for ua in &uas {
            let t = Instant::now();
            black_box(parser.parse_uncached(black_box(ua)));
            ns.push(t.elapsed().as_nanos());
        }
    }
    ns.sort_unstable();
    let mean = ns.iter().sum::<u128>() as f64 / ns.len() as f64;
    println!(
        "未命中（parse_uncached）: mean {:.1} µs, p50 {:.1} µs, p95 {:.1} µs, p99 {:.1} µs, max {:.1} µs",
        mean / 1e3,
        percentile(&ns, 0.50) as f64 / 1e3,
        percentile(&ns, 0.95) as f64 / 1e3,
        percentile(&ns, 0.99) as f64 / 1e3,
        *ns.last().unwrap() as f64 / 1e3
    );

    // 经 parse() 的未命中（含缓存插入）：每条 UA 第一次。
    let mut ns = Vec::new();
    for ua in &uas {
        let t = Instant::now();
        black_box(parser.parse(black_box(ua)));
        ns.push(t.elapsed().as_nanos());
    }
    ns.sort_unstable();
    println!(
        "未命中（parse，含缓存写入）: p50 {:.1} µs, p95 {:.1} µs",
        percentile(&ns, 0.50) as f64 / 1e3,
        percentile(&ns, 0.95) as f64 / 1e3
    );

    // 命中：同一批 UA 再来 N 轮（moka 写入是异步结算的，先让它落稳）。
    for ua in &uas {
        parser.parse(ua);
    }
    let mut ns = Vec::new();
    let mut keep: Option<Arc<_>> = None;
    for _ in 0..20 {
        for ua in &uas {
            let t = Instant::now();
            keep = Some(black_box(parser.parse(black_box(ua))));
            ns.push(t.elapsed().as_nanos());
        }
    }
    black_box(keep);
    ns.sort_unstable();
    let mean = ns.iter().sum::<u128>() as f64 / ns.len() as f64;
    println!(
        "命中（parse）: mean {:.0} ns, p50 {} ns, p95 {} ns, p99 {} ns",
        mean,
        percentile(&ns, 0.50),
        percentile(&ns, 0.95),
        percentile(&ns, 0.99)
    );
    println!("缓存条目数 {}", parser.cache_entry_count());
}
