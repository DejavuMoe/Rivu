//! 开环（open-loop）压测引擎。
//!
//! 事件按**固定到达率**发送：第 n 个请求的**计划发送时刻**是 `阶段起点 + n / 速率`，与前面的响应有没有回来无关
//! （每个请求是一个独立的 tokio 任务，调度器不等响应）。延迟从计划时刻算到响应读完，所以服务端变慢、
//! 或者压测机自己调度落后，都会体现在延迟里，不会被「发送方等响应而自动降速」掩盖（coordinated omission）。
//! 调度落后时不睡眠、立刻补发，计划时刻不变。调度器会提前不超过 0.5 ms 发送（定时器精度），
//! 这部分提前量使延迟最多少算 0.5 ms。

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use reqwest::Client;
use serde_json::json;
use tokio::task::JoinSet;
use tokio::time::{Instant, sleep, sleep_until};

use crate::model::{Generated, Generator, Kind, Site, Zipf, random_ip};
use crate::stats::{Outcome, PhaseStats, QUERY_KINDS};

#[derive(Clone, Debug)]
pub struct PhaseSpec {
    pub name: String,
    pub rate: f64,
    pub secs: u64,
}

impl PhaseSpec {
    /// `名称:速率:秒数`，例如 `sustained:1000:300`。
    pub fn parse(s: &str) -> Result<PhaseSpec, String> {
        let parts: Vec<&str> = s.split(':').collect();
        let [name, rate, secs] = parts[..] else {
            return Err(format!("`{s}` 不是 名称:速率:秒数"));
        };
        let rate: f64 = rate.parse().map_err(|_| format!("`{rate}` 不是速率"))?;
        let secs: u64 = secs.parse().map_err(|_| format!("`{secs}` 不是秒数"))?;
        if name.is_empty() || !rate.is_finite() || rate <= 0.0 || secs == 0 {
            return Err(format!("`{s}`：名称不能为空，速率与秒数必须大于 0"));
        }
        Ok(PhaseSpec {
            name: name.to_owned(),
            rate,
            secs,
        })
    }
}

#[derive(Clone, Debug)]
pub struct ChConfig {
    pub url: String,
    pub database: String,
    pub user: String,
    pub password: String,
}

#[derive(Clone, Debug)]
pub struct Config {
    /// 公共端口，例如 `http://127.0.0.1:8080`
    pub target: String,
    pub phases: Vec<PhaseSpec>,
    pub sites: Vec<Site>,
    pub uas: Vec<String>,
    pub seed: u64,
    /// 事件调度分片数（每片一个任务，各自带会话池）
    pub shards: usize,
    /// 在途请求数上限，超过则不发并计入 `client_skipped`
    pub max_inflight: usize,
    pub query_rate: f64,
    pub api_key: Option<String>,
    pub ch: Option<ChConfig>,
    pub probe_interval: Duration,
    pub probe_timeout: Duration,
    /// 最后一个阶段结束后等在途请求与探针的最长时间
    pub drain: Duration,
}

struct Ctx {
    cfg: Config,
    client: Client,
    event_url: String,
    query_url: String,
    uas: Vec<String>,
    sites: Arc<Vec<Site>>,
    site_zipf: Arc<Zipf>,
    stats: Vec<Arc<PhaseStats>>,
    /// 各阶段相对 `t0` 的起点
    offsets: Vec<Duration>,
    total: Duration,
    inflight: AtomicUsize,
    background: AtomicUsize,
    run_id: u64,
}

impl Ctx {
    fn phase_at(&self, off: Duration) -> usize {
        self.offsets
            .iter()
            .rposition(|o| *o <= off)
            .unwrap_or(0)
            .min(self.stats.len() - 1)
    }
}

pub fn build_client() -> anyhow::Result<Client> {
    // reqwest 用 rustls-no-provider：安装 ring 提供者（重复安装会返回 Err，忽略）
    let _ = rustls::crypto::ring::default_provider().install_default();
    Ok(Client::builder()
        .pool_max_idle_per_host(8192)
        .tcp_nodelay(true)
        .timeout(Duration::from_secs(10))
        .build()?)
}

async fn post_event(ctx: &Ctx, g: &Generated) -> Outcome {
    let res = ctx
        .client
        .post(&ctx.event_url)
        .header("user-agent", &ctx.uas[g.ua_idx])
        .header("x-forwarded-for", g.ip.to_string())
        .header("content-type", "text/plain")
        .body(g.body.clone())
        .send()
        .await;
    match res {
        Ok(resp) => {
            let status = resp.status().as_u16();
            let dropped = resp.headers().contains_key("x-rivu-dropped");
            let _ = resp.bytes().await;
            match status {
                202 if dropped => Outcome::Dropped,
                202 => Outcome::Accepted,
                503 => Outcome::Overloaded503,
                400 => Outcome::BadRequest400,
                _ => Outcome::OtherStatus,
            }
        }
        Err(_) => Outcome::Error,
    }
}

async fn send_event(ctx: Arc<Ctx>, g: Generated, planned: Instant, phase: usize) {
    let outcome = post_event(&ctx, &g).await;
    ctx.stats[phase]
        .ingest
        .record(outcome, Instant::now().saturating_duration_since(planned));
    ctx.inflight.fetch_sub(1, Ordering::Relaxed);
}

/// 睡到 `planned`；已经落后或只差不到 0.5 ms 就不睡。
async fn wait_until(planned: Instant) {
    if planned > Instant::now() + Duration::from_micros(500) {
        sleep_until(planned).await;
    }
}

async fn event_shard(ctx: Arc<Ctx>, shard: usize, t0: Instant) {
    let mut generator = Generator::new(
        ctx.cfg.seed.wrapping_add(shard as u64 * 0x9E37_79B9),
        ctx.sites.clone(),
        ctx.site_zipf.clone(),
        Arc::new(Zipf::new(2000, 1.1)),
        ctx.uas.len(),
        2000,
    );
    let shards = ctx.cfg.shards as f64;
    for (pi, phase) in ctx.cfg.phases.iter().enumerate() {
        let start = t0 + ctx.offsets[pi];
        // 每片的周期是 shards / rate，片与片之间错开 1 / rate
        let period = Duration::from_secs_f64(shards / phase.rate);
        let shift = Duration::from_secs_f64(shard as f64 / phase.rate);
        let end = start + Duration::from_secs(phase.secs);
        let mut n: u64 = 0;
        loop {
            let planned = start + shift + period.mul_f64(n as f64);
            if planned >= end {
                break;
            }
            wait_until(planned).await;
            n += 1;
            ctx.stats[pi].ingest.planned.inc();
            if ctx.inflight.load(Ordering::Relaxed) >= ctx.cfg.max_inflight {
                ctx.stats[pi].ingest.client_skipped.inc();
                continue;
            }
            ctx.inflight.fetch_add(1, Ordering::Relaxed);
            let g = generator.next();
            tokio::spawn(send_event(ctx.clone(), g, planned, pi));
        }
    }
}

fn query_body(kind: usize, key: &str) -> String {
    match kind {
        0 => json!({
            "site": key,
            "metrics": ["visitors", "visits", "pageviews", "bounce_rate", "visit_duration"],
            "date_range": "7d",
        }),
        1 => json!({
            "site": key,
            "metrics": ["visitors", "pageviews"],
            "dimensions": ["event:page"],
            "date_range": "7d",
            "order_by": [["visitors", "desc"]],
            "pagination": {"limit": 10},
        }),
        _ => json!({
            "site": key,
            "metrics": ["visitors", "pageviews"],
            "dimensions": ["time:day"],
            "date_range": "7d",
        }),
    }
    .to_string()
}

async fn send_query(ctx: Arc<Ctx>, kind: usize, key: String, planned: Instant, phase: usize) {
    let Some(api_key) = ctx.cfg.api_key.as_deref() else {
        return;
    };
    let res = ctx
        .client
        .post(&ctx.query_url)
        .header("authorization", format!("Bearer {api_key}"))
        .header("content-type", "application/json")
        .body(query_body(kind, &key))
        .send()
        .await;
    let q = &ctx.stats[phase].queries[kind];
    match res {
        Ok(resp) => {
            let status = resp.status().as_u16();
            let _ = resp.bytes().await;
            match status {
                200 => q.ok.inc(),
                429 => q.status_429.inc(),
                _ => q.status_other.inc(),
            }
        }
        Err(_) => q.errors.inc(),
    }
    let lat = Instant::now().saturating_duration_since(planned);
    q.latency.record(lat);
    ctx.stats[phase].query_all.record(lat);
    ctx.background.fetch_sub(1, Ordering::Relaxed);
}

async fn query_loop(ctx: Arc<Ctx>, t0: Instant) {
    let period = Duration::from_secs_f64(1.0 / ctx.cfg.query_rate);
    let mut rng = <rand::rngs::SmallRng as rand::SeedableRng>::seed_from_u64(ctx.cfg.seed ^ 0x51);
    let mut n: u64 = 0;
    loop {
        let planned = t0 + period.mul_f64(n as f64);
        if planned >= t0 + ctx.total {
            break;
        }
        wait_until(planned).await;
        let kind = (n % QUERY_KINDS.len() as u64) as usize;
        n += 1;
        let phase = ctx.phase_at(planned.saturating_duration_since(t0));
        ctx.stats[phase].query_planned.inc();
        let site = ctx.site_zipf.sample(&mut rng);
        ctx.background.fetch_add(1, Ordering::Relaxed);
        tokio::spawn(send_query(
            ctx.clone(),
            kind,
            ctx.sites[site].key.clone(),
            planned,
            phase,
        ));
    }
}

async fn ch_count(ctx: &Ctx, ch: &ChConfig, site_id: u64, path: &str) -> Option<u64> {
    let sql = format!(
        "SELECT count() FROM events WHERE site_id = {site_id} AND name = 'pageview' AND pathname = '{path}'"
    );
    let resp = ctx
        .client
        .post(format!("{}/?database={}", ch.url, ch.database))
        .header("x-clickhouse-user", &ch.user)
        .header("x-clickhouse-key", &ch.password)
        .body(sql)
        .send()
        .await
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    resp.text().await.ok()?.trim().parse().ok()
}

/// 探针：发一个带唯一 path 的 pageview，轮询 ClickHouse 直到这一行可见。可见延迟从发送前一刻起算
/// （含接入、写缓冲 flush、ClickHouse 插入与查询轮询间隔；轮询间隔 50 ms，所以最多多算约 50 ms 加一次查询耗时）。
async fn probe(ctx: Arc<Ctx>, n: u64, planned: Instant, phase: usize) {
    let Some(ch) = ctx.cfg.ch.as_ref() else {
        ctx.background.fetch_sub(1, Ordering::Relaxed);
        return;
    };
    let site = &ctx.sites[0];
    let path = format!("/__probe/{:x}/{n}", ctx.run_id);
    let mut rng = <rand::rngs::SmallRng as rand::SeedableRng>::seed_from_u64(ctx.run_id ^ n);
    let body = json!({
        "v": 1, "sv": 1, "s": site.key, "n": "pageview",
        "u": format!("https://{}{path}", site.hosts[0]),
    })
    .to_string();
    let g = Generated {
        body,
        ip: random_ip(&mut rng),
        ua_idx: (n as usize) % ctx.uas.len(),
        site_idx: 0,
        kind: Kind::Pageview,
    };
    let st = &ctx.stats[phase];
    st.probes_sent.inc();
    ctx.inflight.fetch_add(1, Ordering::Relaxed);
    let t_send = Instant::now();
    let outcome = post_event(&ctx, &g).await;
    ctx.inflight.fetch_sub(1, Ordering::Relaxed);
    st.ingest
        .record(outcome, Instant::now().saturating_duration_since(planned));
    if outcome == Outcome::Accepted {
        loop {
            if ch_count(&ctx, ch, site.id, &path).await.unwrap_or(0) > 0 {
                st.visibility.record(t_send.elapsed());
                break;
            }
            if t_send.elapsed() > ctx.cfg.probe_timeout {
                st.probes_not_visible.inc();
                break;
            }
            sleep(Duration::from_millis(50)).await;
        }
    } else {
        st.probes_failed.inc();
    }
    ctx.background.fetch_sub(1, Ordering::Relaxed);
}

async fn probe_loop(ctx: Arc<Ctx>, t0: Instant) {
    let mut n: u64 = 0;
    loop {
        let planned = t0 + ctx.cfg.probe_interval.mul_f64(n as f64);
        if planned >= t0 + ctx.total {
            break;
        }
        wait_until(planned).await;
        let phase = ctx.phase_at(planned.saturating_duration_since(t0));
        n += 1;
        ctx.background.fetch_add(1, Ordering::Relaxed);
        tokio::spawn(probe(ctx.clone(), n, planned, phase));
    }
}

/// 跑完全部阶段，返回每个阶段的统计。
pub async fn run(cfg: Config) -> anyhow::Result<Vec<Arc<PhaseStats>>> {
    anyhow::ensure!(!cfg.phases.is_empty(), "至少一个阶段");
    anyhow::ensure!(!cfg.sites.is_empty(), "至少一个站点");
    anyhow::ensure!(!cfg.uas.is_empty(), "没有 UA");
    anyhow::ensure!(cfg.shards > 0, "shards 必须大于 0");
    let mut offsets = Vec::new();
    let mut acc = Duration::ZERO;
    for p in &cfg.phases {
        offsets.push(acc);
        acc += Duration::from_secs(p.secs);
    }
    let stats: Vec<Arc<PhaseStats>> = cfg
        .phases
        .iter()
        .map(|p| Arc::new(PhaseStats::new(&p.name, p.rate, p.secs)))
        .collect();
    let sites = Arc::new(cfg.sites.clone());
    let ctx = Arc::new(Ctx {
        client: build_client()?,
        event_url: format!("{}/api/event", cfg.target.trim_end_matches('/')),
        query_url: format!("{}/api/v1/query", cfg.target.trim_end_matches('/')),
        uas: cfg.uas.clone(),
        site_zipf: Arc::new(Zipf::new(sites.len(), 1.0)),
        sites,
        stats: stats.clone(),
        offsets,
        total: acc,
        inflight: AtomicUsize::new(0),
        background: AtomicUsize::new(0),
        run_id: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(1, |d| d.as_nanos() as u64 & 0xffff_ffff_ffff),
        cfg,
    });

    let t0 = Instant::now() + Duration::from_millis(300);
    let mut tasks = JoinSet::new();
    for shard in 0..ctx.cfg.shards {
        tasks.spawn(event_shard(ctx.clone(), shard, t0));
    }
    if ctx.cfg.query_rate > 0.0 && ctx.cfg.api_key.is_some() {
        tasks.spawn(query_loop(ctx.clone(), t0));
    }
    if ctx.cfg.ch.is_some() && !ctx.cfg.probe_interval.is_zero() {
        tasks.spawn(probe_loop(ctx.clone(), t0));
    }
    while let Some(r) = tasks.join_next().await {
        r?;
    }
    // 等在途请求与探针收尾
    let deadline = Instant::now() + ctx.cfg.drain;
    while (ctx.inflight.load(Ordering::Relaxed) > 0 || ctx.background.load(Ordering::Relaxed) > 0)
        && Instant::now() < deadline
    {
        sleep(Duration::from_millis(100)).await;
    }
    Ok(stats)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicU64;

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    use super::*;

    /// 最小的 HTTP/1.1 桩服务：每个请求读完头和体，睡 `delay` 后回 202；记录收到的请求数。
    async fn stub(delay: Duration) -> (String, Arc<AtomicU64>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let hits = Arc::new(AtomicU64::new(0));
        let h = hits.clone();
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    return;
                };
                let h = h.clone();
                tokio::spawn(async move {
                    let mut buf = Vec::new();
                    let mut chunk = [0u8; 4096];
                    loop {
                        // 一个请求：读到头结束，再按 content-length 读体
                        let head_end = loop {
                            if let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                                break p + 4;
                            }
                            match sock.read(&mut chunk).await {
                                Ok(0) | Err(_) => return,
                                Ok(n) => buf.extend_from_slice(&chunk[..n]),
                            }
                        };
                        let head = String::from_utf8_lossy(&buf[..head_end]).to_ascii_lowercase();
                        let len: usize = head
                            .lines()
                            .find_map(|l| l.strip_prefix("content-length:"))
                            .and_then(|v| v.trim().parse().ok())
                            .unwrap_or(0);
                        while buf.len() < head_end + len {
                            match sock.read(&mut chunk).await {
                                Ok(0) | Err(_) => return,
                                Ok(n) => buf.extend_from_slice(&chunk[..n]),
                            }
                        }
                        buf.drain(..head_end + len);
                        h.fetch_add(1, Ordering::Relaxed);
                        sleep(delay).await;
                        if sock
                            .write_all(b"HTTP/1.1 202 Accepted\r\ncontent-length: 0\r\n\r\n")
                            .await
                            .is_err()
                        {
                            return;
                        }
                    }
                });
            }
        });
        (format!("http://{addr}"), hits)
    }

    fn cfg(target: String, rate: f64, secs: u64) -> Config {
        Config {
            target,
            phases: vec![PhaseSpec {
                name: "t".into(),
                rate,
                secs,
            }],
            sites: vec![Site::parse("k7Qm2xV9aTbR4cLd:1:a.test").unwrap()],
            uas: vec![
                "Mozilla/5.0 (X11; Linux x86_64; rv:126.0) Gecko/20100101 Firefox/126.0".into(),
            ],
            seed: 1,
            shards: 2,
            max_inflight: 10_000,
            query_rate: 0.0,
            api_key: None,
            ch: None,
            probe_interval: Duration::ZERO,
            probe_timeout: Duration::from_secs(1),
            drain: Duration::from_secs(10),
        }
    }

    #[test]
    fn phase_spec_parsing() {
        let p = PhaseSpec::parse("sustained:1000:300").unwrap();
        assert_eq!(
            (p.name.as_str(), p.rate, p.secs),
            ("sustained", 1000.0, 300)
        );
        for bad in ["x", "a:b:1", "a:0:5", "a:5:0", ":5:5", "a:5:5:5"] {
            assert!(PhaseSpec::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn query_bodies_are_valid_json_for_every_kind() {
        for k in 0..QUERY_KINDS.len() {
            let v: serde_json::Value = serde_json::from_str(&query_body(k, "abc")).unwrap();
            assert_eq!(v["site"], "abc");
            assert!(v["metrics"].is_array());
        }
    }

    /// 开环：服务端每个请求慢 300 ms，而 20 个/秒的到达率下串行客户端一秒只能完成约 3 个。
    /// 开环生成器照样按计划发满，且延迟（从计划时刻算起）反映服务端的慢。
    #[tokio::test]
    async fn open_loop_keeps_arrival_rate_when_the_server_is_slow() {
        let (url, hits) = stub(Duration::from_millis(300)).await;
        let stats = run(cfg(url, 20.0, 2)).await.unwrap();
        let i = &stats[0].ingest;
        assert_eq!(i.planned.get(), 40, "按 20/s × 2 s 计划 40 个");
        assert_eq!(i.completed(), 40);
        assert_eq!(i.accepted.get(), 40);
        assert_eq!(hits.load(Ordering::Relaxed), 40);
        // 每个请求至少 300 ms；不是 coordinated omission 的话，延迟不会低于服务端的处理时间
        assert!(
            i.latency.quantile_ms(0.0) >= 290.0,
            "{}",
            i.latency.quantile_ms(0.0)
        );
        assert!(i.latency.quantile_ms(1.0) < 1500.0);
    }

    /// 压测机自己调度落后（这里用很高的速率 + 很小的在途上限模拟「发不出去」）：不发的计入 `client_skipped`，
    /// 计划数仍然等于速率 × 时长。
    #[tokio::test]
    async fn skipped_requests_are_counted_not_hidden() {
        let (url, _hits) = stub(Duration::from_millis(500)).await;
        let mut c = cfg(url, 200.0, 1);
        c.max_inflight = 5;
        let stats = run(c).await.unwrap();
        let i = &stats[0].ingest;
        assert_eq!(i.planned.get(), 200);
        assert!(i.client_skipped.get() > 150, "{}", i.client_skipped.get());
        assert_eq!(i.completed() + i.client_skipped.get(), 200);
    }

    #[tokio::test]
    async fn connection_errors_are_recorded_with_latency() {
        // 端口 1 上没有服务
        let mut c = cfg("http://127.0.0.1:1".into(), 10.0, 1);
        c.drain = Duration::from_secs(5);
        let stats = run(c).await.unwrap();
        let i = &stats[0].ingest;
        assert_eq!(i.errors.get(), 10);
        assert_eq!(i.latency.count(), 10);
    }
}
