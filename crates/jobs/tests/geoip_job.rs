//! `geoip_refresh` 任务：启动、退避、手工文件、停止信号与指标（本地 HTTP 测试服务器）。

use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use rivu_common::config::{DbIpEdition, GeoConfig, GeoSource, Secret};
use rivu_geo::{GeoDb, GeoLevel, HttpClient, HttpOptions, RefreshOptions};
use rivu_jobs::geoip::{self, GeoRefreshJob, GeoRefreshState};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::watch;

fn country_db() -> Vec<u8> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../geo/tests/data/test-data/GeoLite2-Country-Test.mmdb");
    std::fs::read(path).expect("读取 rivu-geo 的测试数据失败")
}

fn gz(data: &[u8]) -> Vec<u8> {
    use std::io::Write;
    let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    e.write_all(data).unwrap();
    e.finish().unwrap()
}

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "rivu-jobs-{tag}-{}-{:?}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .subsec_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        TempDir(dir)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// 极简 HTTP 服务器：第 n 个请求（从 0 起）由 `respond(n, target)` 决定回复；`None` 表示不回复（挂起）。
struct Server {
    addr: std::net::SocketAddr,
    hits: Arc<Mutex<Vec<(Instant, String)>>>,
    task: tokio::task::JoinHandle<()>,
}

impl Server {
    fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.addr)
    }

    fn count(&self) -> usize {
        self.hits.lock().unwrap().len()
    }

    fn times(&self) -> Vec<Instant> {
        self.hits.lock().unwrap().iter().map(|h| h.0).collect()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn serve<F>(respond: F) -> Server
where
    F: Fn(usize, &str) -> Option<(u16, Vec<u8>)> + Send + Sync + 'static,
{
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let hits = Arc::new(Mutex::new(Vec::new()));
    let (h, respond) = (Arc::clone(&hits), Arc::new(respond));
    let task = tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            let (h, respond) = (Arc::clone(&h), Arc::clone(&respond));
            tokio::spawn(async move {
                let mut buf = vec![0u8; 8192];
                let mut n = 0;
                while !buf[..n].windows(4).any(|w| w == b"\r\n\r\n") {
                    match stream.read(&mut buf[n..]).await {
                        Ok(0) | Err(_) => return,
                        Ok(k) => n += k,
                    }
                }
                let head = String::from_utf8_lossy(&buf[..n]).into_owned();
                let target = head.split(' ').nth(1).unwrap_or("").to_string();
                let index = {
                    let mut g = h.lock().unwrap();
                    g.push((Instant::now(), target.clone()));
                    g.len() - 1
                };
                match respond(index, &target) {
                    Some((status, body)) => {
                        let head = format!(
                            "HTTP/1.1 {status} X\r\nConnection: close\r\nContent-Length: {}\r\n\r\n",
                            body.len()
                        );
                        let _ = stream.write_all(head.as_bytes()).await;
                        let _ = stream.write_all(&body).await;
                        let _ = stream.shutdown().await;
                    }
                    None => {
                        // 挂起，直到客户端断开
                        let mut sink = [0u8; 64];
                        while matches!(stream.read(&mut sink).await, Ok(k) if k > 0) {}
                    }
                }
            });
        }
    });
    Server { addr, hits, task }
}

fn custom_cfg(dir: &TempDir, url: String, interval: Option<Duration>) -> GeoConfig {
    let mut cfg = GeoConfig::new(&dir.0);
    cfg.source = GeoSource::Custom {
        url: Secret::new(url),
    };
    cfg.refresh_interval = interval;
    cfg
}

fn job(cfg: GeoConfig, db: Option<Arc<GeoDb>>, state: &Arc<GeoRefreshState>) -> GeoRefreshJob {
    let mut j = GeoRefreshJob::new(cfg, db, Arc::clone(state));
    j.http = Some(
        HttpClient::with_options(HttpOptions {
            no_proxy: true,
            ..HttpOptions::default()
        })
        .unwrap(),
    );
    j.backoff_start = Duration::from_millis(40);
    j
}

async fn wait_until(what: &str, mut cond: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(15);
    while !cond() {
        assert!(Instant::now() < deadline, "等待超时：{what}");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test]
async fn downloads_immediately_without_a_local_copy_and_ends_when_interval_is_zero() {
    let dir = TempDir::new("once");
    let body = country_db();
    let server = serve(move |_, _| Some((200, body.clone()))).await;
    let cfg = custom_cfg(&dir, server.url("/geo.mmdb"), None);
    let db = Arc::new(GeoDb::from_config(&cfg));
    let state = Arc::new(GeoRefreshState::default());
    let (_stop_tx, stop_rx) = watch::channel(false);

    // 周期为 0：下载一次就结束，不需要停止信号
    tokio::time::timeout(
        Duration::from_secs(15),
        geoip::run(job(cfg.clone(), Some(Arc::clone(&db)), &state), stop_rx),
    )
    .await
    .expect("周期为 0 时任务应当自己结束");

    assert_eq!(server.count(), 1);
    assert!(db.is_loaded(), "刷新成功后应当立即热加载");
    assert_eq!(
        db.lookup("89.160.20.128".parse().unwrap(), GeoLevel::Country)
            .country_code,
        *b"SE"
    );
    let snap = state.snapshot();
    assert_eq!(snap.schedule, "done");
    assert_eq!(
        (snap.last_result, snap.consecutive_failures),
        (Some("updated"), 0)
    );

    // 已有本地副本且周期为 0：任务不需要运行，也不会请求服务器
    let (_tx, rx) = watch::channel(false);
    let state2 = Arc::new(GeoRefreshState::default());
    tokio::time::timeout(
        Duration::from_secs(5),
        geoip::run(job(cfg, None, &state2), rx),
    )
    .await
    .unwrap();
    assert_eq!(server.count(), 1);
    assert_eq!(state2.snapshot().schedule, "done");
}

#[tokio::test]
async fn failures_back_off_by_doubling_and_recover() {
    let dir = TempDir::new("backoff");
    let body = country_db();
    // 前 4 次 500，之后正常
    let server = serve(move |n, _| {
        if n < 4 {
            Some((500, Vec::new()))
        } else {
            Some((200, body.clone()))
        }
    })
    .await;
    let cfg = custom_cfg(
        &dir,
        server.url("/geo.mmdb?token=SECRET"),
        Some(Duration::from_secs(3600)),
    );
    let db = Arc::new(GeoDb::from_config(&cfg));
    let state = Arc::new(GeoRefreshState::default());
    let (stop_tx, stop_rx) = watch::channel(false);
    let handle = tokio::spawn(geoip::run(job(cfg, Some(Arc::clone(&db)), &state), stop_rx));

    wait_until("第 3 次失败后", || {
        state.snapshot().consecutive_failures >= 3
    })
    .await;
    let snap = state.snapshot();
    assert_eq!(snap.last_result, Some("failed"));
    let err = snap.last_error.unwrap();
    assert!(err.contains("HTTP 500"), "{err}");
    assert!(!err.contains("SECRET"), "状态里不能带 URL 令牌：{err}");
    wait_until("恢复并加载", || db.is_loaded()).await;

    // 间隔从 start 起每次加倍：40、80、160、320 ms（sleep 不会提前醒，所以只断言下限）
    let t = server.times();
    assert_eq!(t.len(), 5);
    for (i, expect_ms) in [40u64, 80, 160, 320].into_iter().enumerate() {
        let gap = t[i + 1] - t[i];
        assert!(
            gap >= Duration::from_millis(expect_ms),
            "第 {} 次重试的间隔 {gap:?} 应当 >= {expect_ms}ms",
            i + 1
        );
    }
    let snap = state.snapshot();
    assert_eq!(
        (snap.last_result, snap.consecutive_failures, snap.schedule),
        (Some("updated"), 0, "scheduled")
    );

    stop_tx.send(true).unwrap();
    tokio::time::timeout(Duration::from_secs(5), handle)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(state.snapshot().schedule, "stopped");
}

#[tokio::test]
async fn backoff_never_exceeds_the_refresh_interval() {
    let dir = TempDir::new("cap");
    // 周期 100ms，退避起点 40ms：40、80、100、100、…
    let server = serve(|_, _| Some((500, Vec::new()))).await;
    let cfg = custom_cfg(
        &dir,
        server.url("/geo.mmdb"),
        Some(Duration::from_millis(100)),
    );
    let state = Arc::new(GeoRefreshState::default());
    let (stop_tx, stop_rx) = watch::channel(false);
    let handle = tokio::spawn(geoip::run(job(cfg, None, &state), stop_rx));
    wait_until("5 次请求", || server.count() >= 6).await;
    stop_tx.send(true).unwrap();
    handle.await.unwrap();
    let t = server.times();
    let gaps: Vec<_> = t.windows(2).map(|w| w[1] - w[0]).collect();
    assert!(gaps[0] >= Duration::from_millis(40));
    assert!(gaps[1] >= Duration::from_millis(80));
    for g in &gaps[2..] {
        assert!(*g >= Duration::from_millis(100), "{g:?}");
        assert!(
            *g < Duration::from_millis(1000),
            "封顶之后不应继续加倍：{g:?}"
        );
    }
}

#[tokio::test]
async fn a_manually_placed_database_is_used_but_never_refreshed() {
    let dir = TempDir::new("manual");
    let server = serve(|_, _| Some((500, Vec::new()))).await;
    // 默认来源（DB-IP）+ 没有 meta 的库文件
    let mut cfg = GeoConfig::new(&dir.0);
    cfg.source = GeoSource::DbIp {
        edition: DbIpEdition::City,
    };
    std::fs::create_dir_all(cfg.db_path.parent().unwrap()).unwrap();
    std::fs::write(&cfg.db_path, country_db()).unwrap();
    let state = Arc::new(GeoRefreshState::default());
    let mut j = job(cfg.clone(), None, &state);
    j.opts = RefreshOptions {
        dbip_base_url: server.url("/free"),
        ..RefreshOptions::default()
    };
    let (_tx, rx) = watch::channel(false);
    tokio::time::timeout(Duration::from_secs(5), geoip::run(j, rx))
        .await
        .unwrap();
    assert_eq!(server.count(), 0, "手工放置的库不能触发下载");
    assert_eq!(state.snapshot().schedule, "manual_file");
    assert_eq!(std::fs::read(&cfg.db_path).unwrap(), country_db());
}

#[tokio::test]
async fn stop_signal_ends_a_sleeping_job_and_an_inflight_download() {
    let dir = TempDir::new("stop");
    let body = country_db();
    let hang = Arc::new(Mutex::new(false));
    let h = Arc::clone(&hang);
    let server = serve(move |_, _| {
        if *h.lock().unwrap() {
            None
        } else {
            Some((200, body.clone()))
        }
    })
    .await;
    let cfg = custom_cfg(
        &dir,
        server.url("/geo.mmdb"),
        Some(Duration::from_secs(3600)),
    );

    // 先下载一份，让后面的任务进入「等一个周期」的睡眠
    let state = Arc::new(GeoRefreshState::default());
    let (stop_tx, stop_rx) = watch::channel(false);
    let handle = tokio::spawn(geoip::run(job(cfg.clone(), None, &state), stop_rx));
    wait_until("首次下载", || {
        state.snapshot().last_result == Some("updated")
    })
    .await;
    stop_tx.send(true).unwrap();
    tokio::time::timeout(Duration::from_secs(5), handle)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(server.count(), 1);

    // 睡眠中收到停止信号：立即退出
    let state = Arc::new(GeoRefreshState::default());
    let (stop_tx, stop_rx) = watch::channel(false);
    let handle = tokio::spawn(geoip::run(job(cfg.clone(), None, &state), stop_rx));
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(server.count(), 1, "刚检查过，应当在睡眠中而不是再请求");
    let started = Instant::now();
    stop_tx.send(true).unwrap();
    tokio::time::timeout(Duration::from_secs(5), handle)
        .await
        .unwrap()
        .unwrap();
    assert!(started.elapsed() < Duration::from_secs(2));

    // 下载进行中（服务器挂起）收到停止信号：也能退出，不会等到 10 分钟超时
    *hang.lock().unwrap() = true;
    let mut cfg2 = cfg;
    cfg2.source = GeoSource::Custom {
        url: Secret::new(server.url("/other.mmdb")), // 来源变了 -> 立即下载
    };
    let state = Arc::new(GeoRefreshState::default());
    let (stop_tx, stop_rx) = watch::channel(false);
    let handle = tokio::spawn(geoip::run(job(cfg2, None, &state), stop_rx));
    wait_until("请求已发出", || server.count() >= 2).await;
    let started = Instant::now();
    stop_tx.send(true).unwrap();
    tokio::time::timeout(Duration::from_secs(5), handle)
        .await
        .unwrap()
        .unwrap();
    assert!(started.elapsed() < Duration::from_secs(2));
    // 被取消的下载不留临时文件
    let leftovers: Vec<_> = std::fs::read_dir(dir.0.join("geoip"))
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".tmp"))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
}

static PROM: OnceLock<metrics_exporter_prometheus::PrometheusHandle> = OnceLock::new();

#[tokio::test]
async fn metrics_follow_platform_and_ops_section_10() {
    let recorder = metrics_exporter_prometheus::PrometheusBuilder::new().build_recorder();
    let handle = recorder.handle();
    metrics::set_global_recorder(recorder).expect("本文件只有这个用例安装全局 recorder");
    PROM.set(handle.clone()).ok();

    // 默认来源 DB-IP：第一次失败（500），第二次成功（当月文件，gz）；只有这个用例用 dbip-lite 标签
    let dir = TempDir::new("metrics");
    let gz_body = gz(&country_db());
    let server = serve(move |n, target| {
        assert!(
            target.starts_with("/free/dbip-city-lite-2026-10"),
            "{target}"
        );
        if n == 0 {
            Some((500, Vec::new()))
        } else {
            Some((200, gz_body.clone()))
        }
    })
    .await;
    let mut cfg = GeoConfig::new(&dir.0);
    cfg.refresh_interval = None;
    let db = Arc::new(GeoDb::from_config(&cfg));
    let state = Arc::new(GeoRefreshState::default());
    let mut j = job(cfg, Some(Arc::clone(&db)), &state);
    j.opts = RefreshOptions {
        dbip_base_url: server.url("/free"),
        now: Some("2026-10-22T08:00:00Z".parse().unwrap()),
        ..RefreshOptions::default()
    };
    let (_tx, rx) = watch::channel(false);
    tokio::time::timeout(Duration::from_secs(15), geoip::run(j, rx))
        .await
        .unwrap();
    assert!(db.is_loaded());

    let text = handle.render();
    let line = |needle: &[&str]| {
        text.lines()
            .find(|l| {
                l.starts_with("rivu_geoip_refresh_total") && needle.iter().all(|n| l.contains(n))
            })
            .map(str::to_string)
    };
    let failed = line(&["source=\"dbip-lite\"", "result=\"failed\""])
        .unwrap_or_else(|| panic!("缺少 failed 计数：\n{text}"));
    assert!(failed.ends_with(" 1"), "{failed}");
    let updated = line(&["source=\"dbip-lite\"", "result=\"updated\""])
        .unwrap_or_else(|| panic!("缺少 updated 计数：\n{text}"));
    assert!(updated.ends_with(" 1"), "{updated}");
    assert!(text.lines().any(|l| l == "rivu_geoip_loaded 1"), "{text}");
    let age = text
        .lines()
        .find(|l| l.starts_with("rivu_geoip_db_age_seconds "))
        .unwrap_or_else(|| panic!("缺少库年龄：\n{text}"));
    assert!(
        age.split(' ').nth(1).unwrap().parse::<f64>().unwrap() > 0.0,
        "{age}"
    );
}
