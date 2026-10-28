//! `geoip_refresh`（`platform-and-ops.md` §6、`system-design.md` §4.4「生命周期」）。
//!
//! - 启动时：没有本地副本（或来源标识与配置不一致）就立即下载；有副本则按 meta 里的最近检查时间排下一次。
//! - 之后每隔 `RIVU_GEOIP_REFRESH_INTERVAL` 做一次条件 GET（±10% 抖动）；`0` 表示不定期刷新，
//!   只在没有本地副本时下载一次。
//! - 失败：沿用上一份有效副本；10 分钟后重试，之后每次加倍，最长到一个刷新周期（`0` 时取 24 小时）。
//! - 手工放置的库（没有 meta，且配置的是默认的 DB-IP 来源）直接使用，不自动刷新，免得定期下载把它覆盖掉。
//!
//! 下载本身（条件 GET、校验、原子替换）在 `rivu-geo`；这里只管调度、指标和失败退避。
//! 当前进程持有 [`GeoDb`] 时，成功后立即热加载；另外持有库的一方每小时比较文件修改时间。

use std::sync::Arc;
use std::time::Duration;

use jiff::Timestamp;
use parking_lot::Mutex;
use rivu_common::config::GeoConfig;
use rivu_geo::{GeoDb, HttpClient, LocalCopy, RefreshOptions, RefreshOutcome};
use tokio::sync::watch;
use tokio::task::JoinHandle;

/// 失败后的第一次重试间隔。
pub const BACKOFF_START: Duration = Duration::from_secs(10 * 60);
/// 不定期刷新（`RIVU_GEOIP_REFRESH_INTERVAL=0`）时，失败退避的上限。
pub const BACKOFF_CAP_WHEN_NO_INTERVAL: Duration = Duration::from_secs(24 * 3600);
/// 刷新周期的抖动幅度：±10%。
pub const JITTER: f64 = 0.1;

/// 给 `/readyz` 看的任务状态。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GeoRefreshSnapshot {
    /// `not_started` | `scheduled`（按周期刷新）| `initial_only`（周期为 0，只在没有副本时下载一次）|
    /// `manual_file`（手工放置的库，不自动刷新）| `done`（周期为 0 且已下载完成）| `stopped`
    pub schedule: &'static str,
    /// 最近一次尝试的时间（RFC 3339）
    pub last_attempt_at: Option<String>,
    /// `updated` | `not_modified` | `failed`
    pub last_result: Option<&'static str>,
    pub last_error: Option<String>,
    /// 连续失败次数；成功后清零
    pub consecutive_failures: u32,
}

/// 任务状态，`geoip_refresh` 写、`/readyz` 读。
#[derive(Debug)]
pub struct GeoRefreshState(Mutex<GeoRefreshSnapshot>);

impl Default for GeoRefreshState {
    fn default() -> Self {
        GeoRefreshState(Mutex::new(GeoRefreshSnapshot {
            schedule: "not_started",
            ..GeoRefreshSnapshot::default()
        }))
    }
}

impl GeoRefreshState {
    pub fn snapshot(&self) -> GeoRefreshSnapshot {
        self.0.lock().clone()
    }

    fn set_schedule(&self, schedule: &'static str) {
        self.0.lock().schedule = schedule;
    }

    fn record(&self, result: &'static str, error: Option<String>) {
        let mut s = self.0.lock();
        s.last_attempt_at = Some(Timestamp::now().to_string());
        s.last_result = Some(result);
        if result == "failed" {
            s.consecutive_failures += 1;
        } else {
            s.consecutive_failures = 0;
        }
        s.last_error = error;
    }
}

/// 刷新周期加抖动。`unit` 取 `[-1, 1]`：`-1` 是 −10%，`1` 是 +10%。
pub fn jittered(interval: Duration, unit: f64) -> Duration {
    interval.mul_f64(1.0 + JITTER * unit.clamp(-1.0, 1.0))
}

/// 第 `failures` 次连续失败后的等待：10 分钟起，每次加倍，不超过 `cap`。
pub fn backoff(failures: u32, start: Duration, cap: Duration) -> Duration {
    let exp = failures.saturating_sub(1).min(32);
    start
        .checked_mul(1u32.checked_shl(exp).unwrap_or(u32::MAX))
        .map_or(cap, |d| d.min(cap))
}

/// 启动后第一次执行前的等待；`None` 表示根本不需要这个任务。
///
/// - 没有本地副本、或来源标识与配置不一致：立即执行；
/// - 手工放置的库：不需要；
/// - 有副本：周期为 0 时不需要；否则在「最近一次检查时间 + 带抖动的周期」执行（已经过期就立即执行），
///   这样频繁重启不会每次都去请求服务器。
pub fn initial_delay(
    local: LocalCopy,
    last_checked: Option<Timestamp>,
    interval: Option<Duration>,
    now: Timestamp,
    unit: f64,
) -> Option<Duration> {
    match local {
        LocalCopy::Missing | LocalCopy::Foreign => Some(Duration::ZERO),
        LocalCopy::Manual => None,
        LocalCopy::Current => {
            let next = jittered(interval?, unit);
            let elapsed = last_checked
                .and_then(|t| now.since(t).ok())
                .and_then(|s| Duration::try_from(s).ok())
                .unwrap_or(next);
            Some(next.saturating_sub(elapsed))
        }
    }
}

fn jitter_unit() -> f64 {
    rand::random::<f64>() * 2.0 - 1.0
}

/// 一次任务运行所需的全部输入。生产用 [`spawn_geoip_refresh`]；测试可以替换 HTTP 客户端、
/// 下载参数（指向本地服务器）和首次退避。
pub struct GeoRefreshJob {
    pub cfg: GeoConfig,
    /// 当前进程里的库；刷新成功后立即热加载。没有（worker）则由 collector 每小时自己加载。
    pub db: Option<Arc<GeoDb>>,
    pub state: Arc<GeoRefreshState>,
    /// `None` 时用默认的 [`HttpClient::new`]。
    pub http: Option<HttpClient>,
    pub opts: RefreshOptions,
    pub backoff_start: Duration,
}

impl GeoRefreshJob {
    pub fn new(cfg: GeoConfig, db: Option<Arc<GeoDb>>, state: Arc<GeoRefreshState>) -> Self {
        GeoRefreshJob {
            cfg,
            db,
            state,
            http: None,
            opts: RefreshOptions::default(),
            backoff_start: BACKOFF_START,
        }
    }
}

/// 启动 `geoip_refresh`，直到收到停止信号（或周期为 0 且已完成）。
pub fn spawn_geoip_refresh(
    cfg: GeoConfig,
    db: Option<Arc<GeoDb>>,
    state: Arc<GeoRefreshState>,
    stop: watch::Receiver<bool>,
) -> JoinHandle<()> {
    tokio::spawn(run(GeoRefreshJob::new(cfg, db, state), stop))
}

/// 任务主体。
pub async fn run(job: GeoRefreshJob, mut stop: watch::Receiver<bool>) {
    let GeoRefreshJob {
        cfg,
        db,
        state,
        http,
        opts,
        backoff_start,
    } = job;
    let kind = cfg.source.kind();
    let local = rivu_geo::local_copy(&cfg);
    let last_checked = rivu_geo::read_meta(&cfg.db_path).and_then(|m| m.checked_at.parse().ok());
    let Some(mut delay) = initial_delay(
        local,
        last_checked,
        cfg.refresh_interval,
        Timestamp::now(),
        jitter_unit(),
    ) else {
        if local == LocalCopy::Manual {
            state.set_schedule("manual_file");
            tracing::info!(
                path = %cfg.db_path.display(),
                "GeoIP 库是手工放置的（没有 meta.json）：直接使用，不自动刷新。\
                 要改用自动下载，删除该文件，或设置 RIVU_GEOIP_DB_URL / RIVU_MAXMIND_LICENSE_KEY"
            );
        } else {
            state.set_schedule("done");
            tracing::info!(
                source = kind,
                "GeoIP 已有本地副本且 RIVU_GEOIP_REFRESH_INTERVAL=0：不定期刷新"
            );
        }
        return;
    };
    state.set_schedule(if cfg.refresh_interval.is_some() {
        "scheduled"
    } else {
        "initial_only"
    });
    let http = match http {
        Some(h) => h,
        None => match HttpClient::new() {
            Ok(h) => h,
            Err(e) => {
                tracing::error!(error = %format!("{e:#}"), "创建 GeoIP 下载用的 HTTP 客户端失败，geoip_refresh 不运行");
                return;
            }
        },
    };
    let cap = cfg.refresh_interval.unwrap_or(BACKOFF_CAP_WHEN_NO_INTERVAL);
    if delay.is_zero() {
        tracing::info!(source = kind, local = ?local, "GeoIP 没有可用的本地副本，立即下载");
    }

    let mut failures = 0u32;
    loop {
        tokio::select! {
            () = stopped(&mut stop) => break,
            () = tokio::time::sleep(delay) => {}
        }
        // 下载可能持续数分钟；停止信号到来时直接丢弃这次刷新（临时文件由 guard 清理）。
        let result = tokio::select! {
            () = stopped(&mut stop) => break,
            r = rivu_geo::refresh_with(&cfg, &http, &opts) => r,
        };
        match result {
            Ok(outcome) => {
                failures = 0;
                let label = outcome.result_label();
                metrics::counter!("rivu_geoip_refresh_total", "source" => kind, "result" => label)
                    .increment(1);
                state.record(label, None);
                match &outcome {
                    RefreshOutcome::Updated {
                        build_epoch, bytes, ..
                    } => tracing::info!(
                        source = kind,
                        build_epoch,
                        bytes,
                        "geoip_refresh：已下载并替换 GeoIP 库"
                    ),
                    RefreshOutcome::NotModified { by_hash } => {
                        tracing::info!(source = kind, by_hash, "geoip_refresh：库没有变化");
                    }
                }
                reload(&db, &cfg).await;
                match cfg.refresh_interval {
                    Some(interval) => delay = jittered(interval, jitter_unit()),
                    None => {
                        state.set_schedule("done");
                        break;
                    }
                }
            }
            Err(e) => {
                failures += 1;
                metrics::counter!("rivu_geoip_refresh_total", "source" => kind, "result" => "failed")
                    .increment(1);
                let msg = format!("{e:#}");
                delay = backoff(failures, backoff_start, cap);
                tracing::warn!(
                    source = kind,
                    failures,
                    retry_in_secs = delay.as_secs(),
                    error = %msg,
                    "geoip_refresh 失败，沿用上一份有效副本"
                );
                state.record("failed", Some(msg));
            }
        }
    }
    if state.snapshot().schedule != "done" {
        state.set_schedule("stopped");
    }
}

/// 刷新成功后立即热加载（库的文件或 meta 变了；没变则什么都不做）。
async fn reload(db: &Option<Arc<GeoDb>>, cfg: &GeoConfig) {
    let Some(db) = db else { return };
    let (db, path) = (Arc::clone(db), cfg.db_path.clone());
    match tokio::task::spawn_blocking(move || db.reload_if_changed(&path)).await {
        Ok(Ok(true)) => tracing::info!("GeoIP 库已热加载"),
        Ok(Ok(false)) => {}
        Ok(Err(e)) => {
            tracing::warn!(error = %format!("{e:#}"), "刷新后热加载 GeoIP 库失败，继续使用旧库")
        }
        Err(e) => tracing::error!(error = %e, "GeoIP 热加载任务异常"),
    }
}

/// 等到停止信号（发送端被丢弃也算停止）。`watch::Ref` 不是 `Send`，不能带过 await，所以包一层。
async fn stopped(rx: &mut watch::Receiver<bool>) {
    let _ = rx.wait_for(|s| *s).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    const H: Duration = Duration::from_secs(3600);

    #[test]
    fn jitter_stays_within_ten_percent() {
        let day = Duration::from_secs(86_400);
        assert_eq!(jittered(day, 0.0), day);
        assert_eq!(jittered(day, 1.0), day.mul_f64(1.1));
        assert_eq!(jittered(day, -1.0), day.mul_f64(0.9));
        // 越界的随机数被夹住
        assert_eq!(jittered(day, 5.0), day.mul_f64(1.1));
        assert_eq!(jittered(day, -5.0), day.mul_f64(0.9));
    }

    #[test]
    fn backoff_starts_at_ten_minutes_and_doubles_up_to_the_cap() {
        let cap = Duration::from_secs(24 * 3600);
        let minutes = |n: u32| backoff(n, BACKOFF_START, cap).as_secs() / 60;
        assert_eq!(
            (1..=9).map(minutes).collect::<Vec<_>>(),
            [10, 20, 40, 80, 160, 320, 640, 1280, 1440]
        );
        assert_eq!(minutes(10), 1440, "封顶后保持不变");
        assert_eq!(minutes(1000), 1440, "次数很大也不溢出");
        // 周期比退避短时，以周期为上限（周期下限 1h）
        assert_eq!(backoff(1, BACKOFF_START, H), Duration::from_secs(600));
        assert_eq!(backoff(2, BACKOFF_START, H), Duration::from_secs(1200));
        assert_eq!(backoff(4, BACKOFF_START, H), H);
    }

    #[test]
    fn initial_delay_rules() {
        let now: Timestamp = "2026-10-22T12:00:00Z".parse().unwrap();
        let ago = |secs: i64| Some(now - jiff::SignedDuration::from_secs(secs));
        let day = Duration::from_secs(86_400);

        // 没有本地副本、来源不一致：立即
        for local in [LocalCopy::Missing, LocalCopy::Foreign] {
            assert_eq!(
                initial_delay(local, None, Some(day), now, 0.0),
                Some(Duration::ZERO)
            );
            assert_eq!(
                initial_delay(local, ago(10), None, now, 0.0),
                Some(Duration::ZERO)
            );
        }
        // 手工放置：没有这个任务
        assert_eq!(
            initial_delay(LocalCopy::Manual, None, Some(day), now, 0.0),
            None
        );
        // 有副本且周期为 0：没有这个任务
        assert_eq!(
            initial_delay(LocalCopy::Current, ago(10), None, now, 0.0),
            None
        );
        // 有副本：检查时间 + 周期；2 小时前检查过，24 小时周期 -> 还剩 22 小时
        assert_eq!(
            initial_delay(LocalCopy::Current, ago(7200), Some(day), now, 0.0),
            Some(Duration::from_secs(22 * 3600))
        );
        // 抖动同样作用于第一次
        assert_eq!(
            initial_delay(LocalCopy::Current, ago(0), Some(day), now, 1.0),
            Some(day.mul_f64(1.1))
        );
        // 已经过期：立即
        assert_eq!(
            initial_delay(LocalCopy::Current, ago(3 * 86_400), Some(day), now, 0.0),
            Some(Duration::ZERO)
        );
        // 不知道上次检查时间（meta 里的时间解析失败）：按一个完整周期之后之前处理，即立即
        assert_eq!(
            initial_delay(LocalCopy::Current, None, Some(day), now, 0.0),
            Some(Duration::ZERO)
        );
    }

    #[test]
    fn state_tracks_consecutive_failures() {
        let s = GeoRefreshState::default();
        assert_eq!(s.snapshot().schedule, "not_started");
        s.record("failed", Some("boom".into()));
        s.record("failed", Some("boom".into()));
        let snap = s.snapshot();
        assert_eq!(
            (snap.consecutive_failures, snap.last_result),
            (2, Some("failed"))
        );
        assert_eq!(snap.last_error.as_deref(), Some("boom"));
        s.record("updated", None);
        let snap = s.snapshot();
        assert_eq!(
            (snap.consecutive_failures, snap.last_result),
            (0, Some("updated"))
        );
        assert_eq!(snap.last_error, None);
        assert!(snap.last_attempt_at.is_some());
    }
}
