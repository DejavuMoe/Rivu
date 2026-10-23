//! 公开 API 按组织限流（`platform-and-ops.md` §4.4、`system-design.md` §6.8）。
//!
//! 每个组织三道闸，按这个顺序检查：
//!
//! 1. **小时配额**：固定 3600 秒窗口（从该组织的第一个请求起算，过期后下一个请求开新窗口）内最多 `quota` 次，
//!    `quota` 取 `organizations.api_hourly_limit`，为空用 `RIVU_API_RATE_PER_HOUR`。超限 → [`RejectKind::HourlyQuota`]；
//! 2. **突发令牌桶**：容量 `min(RIVU_API_BURST, quota)`，补充速率 `quota / 3600` 每秒。超限 → [`RejectKind::Burst`]；
//! 3. **并发信号量**：每个组织同时在途的请求不超过 `max_concurrency`（默认 4），多出来的排队等待，
//!    等不到就在 `queue_timeout` 之后拒绝 → [`RejectKind::Concurrency`]。
//!
//! 前两道闸通过时各扣一次（小时计数加一、取走一个令牌），第三道闸超时不退还：被拒绝的请求也占用了配额，
//! 这样反复重试不会让排队变成免费的探测。限额挂在组织上，同组织的所有 key 共享。

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use parking_lot::Mutex;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::{Clock, SHARDS, TokenBucket, shard_of};

/// 小时配额窗口。
const WINDOW: Duration = Duration::from_secs(3600);
/// 每处理这么多次请求顺带清理一次空闲的组织状态。
const SWEEP_EVERY: u64 = 8192;

/// 限流参数（全局默认值；单个组织的小时配额可以覆盖）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ApiLimits {
    /// `RIVU_API_RATE_PER_HOUR`：组织的默认小时配额。
    pub default_hourly: u32,
    /// `RIVU_API_BURST`：突发令牌桶容量。
    pub burst: u32,
    /// 每个组织的并发上限（`system-design.md` §6.8，默认 4）。
    pub max_concurrency: usize,
    /// 并发排队的最长等待。
    pub queue_timeout: Duration,
}

impl ApiLimits {
    pub const DEFAULT_HOURLY: u32 = 10_000;
    pub const DEFAULT_BURST: u32 = 60;
    pub const DEFAULT_MAX_CONCURRENCY: usize = 4;
    pub const DEFAULT_QUEUE_TIMEOUT: Duration = Duration::from_secs(2);
}

impl Default for ApiLimits {
    fn default() -> Self {
        ApiLimits {
            default_hourly: Self::DEFAULT_HOURLY,
            burst: Self::DEFAULT_BURST,
            max_concurrency: Self::DEFAULT_MAX_CONCURRENCY,
            queue_timeout: Self::DEFAULT_QUEUE_TIMEOUT,
        }
    }
}

/// 被哪一道闸拒绝。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RejectKind {
    Burst,
    HourlyQuota,
    Concurrency,
}

impl RejectKind {
    /// 指标 `rivu_ratelimit_rejected_total{scope}` 的标签值。
    pub fn as_str(self) -> &'static str {
        match self {
            RejectKind::Burst => "api_burst",
            RejectKind::HourlyQuota => "api_hourly_quota",
            RejectKind::Concurrency => "api_concurrency",
        }
    }
}

/// 响应头 `X-RateLimit-*` 的内容。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RateInfo {
    /// `X-RateLimit-Limit`：每小时配额。
    pub limit: u32,
    /// `X-RateLimit-Remaining`：此刻还能发的请求数，取「本小时窗口剩余」与「令牌桶里整数个令牌」的较小值。
    pub remaining: u32,
    /// `X-RateLimit-Reset`：距离小时窗口结束的秒数（相对值，向上取整）。被突发令牌桶拒绝时，
    /// `Retry-After` 比它短：下一个令牌很快就来，不必等窗口结束。
    pub reset_secs: u64,
}

/// 被拒绝。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rejection {
    pub kind: RejectKind,
    /// 至少要等多久再试。
    pub retry_after: Duration,
    pub info: RateInfo,
}

impl Rejection {
    /// `Retry-After` 头的值：向上取整的秒数，至少 1。
    pub fn retry_after_secs(&self) -> u64 {
        ceil_secs(self.retry_after).max(1)
    }
}

/// 放行：持有 `permit` 直到请求处理完（drop 即归还并发名额）。
#[derive(Debug)]
pub struct Admitted {
    pub permit: OwnedSemaphorePermit,
    pub info: RateInfo,
}

fn ceil_secs(d: Duration) -> u64 {
    d.as_secs() + u64::from(d.subsec_nanos() > 0)
}

struct OrgInner {
    bucket: TokenBucket,
    quota: u32,
    window_start: Duration,
    used: u32,
}

struct OrgState {
    inner: Mutex<OrgInner>,
    sem: Arc<Semaphore>,
}

/// 按组织的 API 限流器。线程安全，`admit` 可被任意多个请求并发调用。
pub struct ApiLimiter {
    limits: ApiLimits,
    clock: Clock,
    shards: [Mutex<HashMap<i64, Arc<OrgState>>>; SHARDS],
    calls: AtomicU64,
}

impl ApiLimiter {
    pub fn new(limits: ApiLimits) -> Self {
        ApiLimiter {
            limits,
            clock: Clock::new(),
            shards: std::array::from_fn(|_| Mutex::new(HashMap::new())),
            calls: AtomicU64::new(0),
        }
    }

    pub fn limits(&self) -> &ApiLimits {
        &self.limits
    }

    /// 当前保留状态的组织数。
    pub fn tracked_orgs(&self) -> usize {
        self.shards.iter().map(|s| s.lock().len()).sum()
    }

    fn bucket_params(&self, quota: u32) -> (f64, f64) {
        let cap = f64::from(self.limits.burst.min(quota).max(1));
        (cap, f64::from(quota) / WINDOW.as_secs_f64())
    }

    fn state(&self, org_id: i64, quota: u32, now: Duration) -> Arc<OrgState> {
        let key = org_id as u64;
        let mut shard = self.shards[shard_of(key)].lock();
        Arc::clone(shard.entry(org_id).or_insert_with(|| {
            let (cap, rate) = self.bucket_params(quota);
            Arc::new(OrgState {
                inner: Mutex::new(OrgInner {
                    bucket: TokenBucket::new(cap, rate, now),
                    quota,
                    window_start: now,
                    used: 0,
                }),
                sem: Arc::new(Semaphore::new(self.limits.max_concurrency.max(1))),
            })
        }))
    }

    /// 准入：先过小时配额和突发令牌桶（同步），再排队取并发名额（最多等 `queue_timeout`）。
    ///
    /// `hourly_override` 是 `organizations.api_hourly_limit`（`None` 用全局默认）；每次调用都传入当前值，
    /// 配额改了下一个请求就生效（令牌桶就地调整，窗口内已用的次数保留）。
    pub async fn admit(
        &self,
        org_id: i64,
        hourly_override: Option<u32>,
    ) -> Result<Admitted, Rejection> {
        let quota = hourly_override.unwrap_or(self.limits.default_hourly).max(1);
        let now = self.clock.now();
        let state = self.state(org_id, quota, now);
        if self.calls.fetch_add(1, Ordering::Relaxed) % SWEEP_EVERY == SWEEP_EVERY - 1 {
            self.sweep();
        }

        let info = self.take(&state, quota, now)?;

        // 并发：先试着直接拿，拿不到才排队。
        if let Ok(permit) = Arc::clone(&state.sem).try_acquire_owned() {
            return Ok(Admitted { permit, info });
        }
        match tokio::time::timeout(
            self.limits.queue_timeout,
            Arc::clone(&state.sem).acquire_owned(),
        )
        .await
        {
            Ok(Ok(permit)) => Ok(Admitted { permit, info }),
            // 信号量从不关闭；`Ok(Err(_))` 不会发生，按超时处理。
            Ok(Err(_)) | Err(_) => Err(Rejection {
                kind: RejectKind::Concurrency,
                retry_after: self.limits.queue_timeout,
                info,
            }),
        }
    }

    /// 小时配额与令牌桶：通过时各扣一次。
    fn take(&self, state: &OrgState, quota: u32, now: Duration) -> Result<RateInfo, Rejection> {
        let (cap, rate) = self.bucket_params(quota);
        let mut g = state.inner.lock();
        if g.bucket.capacity() != cap || g.bucket.rate() != rate {
            g.bucket.reconfigure(cap, rate, now);
        }
        g.quota = quota;
        // 窗口过期：开新窗口
        if now.saturating_sub(g.window_start) >= WINDOW {
            g.window_start = now;
            g.used = 0;
        }
        let window_left = (g.window_start + WINDOW).saturating_sub(now);
        let reset_secs = ceil_secs(window_left);

        if g.used >= quota {
            return Err(Rejection {
                kind: RejectKind::HourlyQuota,
                retry_after: window_left,
                info: RateInfo {
                    limit: quota,
                    remaining: 0,
                    reset_secs,
                },
            });
        }
        if let Err(wait) = g.bucket.try_take(now) {
            return Err(Rejection {
                kind: RejectKind::Burst,
                retry_after: wait,
                info: RateInfo {
                    limit: quota,
                    remaining: 0,
                    reset_secs,
                },
            });
        }
        g.used += 1;
        let tokens = g.bucket.available(now).floor() as u32;
        Ok(RateInfo {
            limit: quota,
            remaining: (quota - g.used).min(tokens),
            reset_secs,
        })
    }

    /// 清理空闲的组织状态：窗口已过期（或没用过）、令牌桶是满的、没有在途请求。
    pub fn sweep(&self) {
        let now = self.clock.now();
        let max = self.limits.max_concurrency.max(1);
        for shard in &self.shards {
            shard.lock().retain(|_, st| {
                let idle_sem = st.sem.available_permits() >= max;
                let mut g = st.inner.lock();
                let window_over = g.used == 0 || now.saturating_sub(g.window_start) >= WINDOW;
                !(idle_sem && window_over && g.bucket.is_full(now))
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limits(hourly: u32, burst: u32, conc: usize) -> ApiLimits {
        ApiLimits {
            default_hourly: hourly,
            burst,
            max_concurrency: conc,
            queue_timeout: Duration::from_secs(2),
        }
    }

    async fn pass(l: &ApiLimiter, org: i64) -> RateInfo {
        let a = l.admit(org, None).await.expect("应放行");
        a.info
    }

    #[tokio::test(start_paused = true)]
    async fn burst_bucket_drains_then_refills_with_the_clock() {
        // 3600/小时 = 每秒补 1 个；容量 3
        let l = ApiLimiter::new(limits(3600, 3, 8));
        for _ in 0..3 {
            pass(&l, 1).await;
        }
        let r = l.admit(1, None).await.unwrap_err();
        assert_eq!(r.kind, RejectKind::Burst);
        assert_eq!(r.retry_after, Duration::from_secs(1));
        assert_eq!(r.retry_after_secs(), 1);
        assert_eq!(r.info.remaining, 0);
        assert_eq!(r.info.limit, 3600);
        tokio::time::advance(Duration::from_millis(999)).await;
        assert!(l.admit(1, None).await.is_err());
        tokio::time::advance(Duration::from_millis(1)).await;
        pass(&l, 1).await;
        assert!(l.admit(1, None).await.is_err());
        // 10 秒后桶回到满（容量 3）
        tokio::time::advance(Duration::from_secs(10)).await;
        for _ in 0..3 {
            pass(&l, 1).await;
        }
        assert!(l.admit(1, None).await.is_err());
    }

    #[tokio::test(start_paused = true)]
    async fn hourly_quota_is_a_fixed_window() {
        // 配额 5/小时：桶容量取 min(burst 60, 5) = 5
        let l = ApiLimiter::new(limits(5, 60, 8));
        for _ in 0..5 {
            pass(&l, 1).await;
        }
        let r = l.admit(1, None).await.unwrap_err();
        assert_eq!(r.kind, RejectKind::HourlyQuota);
        assert_eq!(r.retry_after, Duration::from_secs(3600));
        assert_eq!(r.info.reset_secs, 3600);
        tokio::time::advance(Duration::from_secs(1800)).await;
        let r = l.admit(1, None).await.unwrap_err();
        assert_eq!(r.kind, RejectKind::HourlyQuota);
        assert_eq!(r.retry_after, Duration::from_secs(1800));
        assert_eq!(r.retry_after_secs(), 1800);
        // 窗口结束：新窗口，配额重置
        tokio::time::advance(Duration::from_secs(1800)).await;
        let info = pass(&l, 1).await;
        assert_eq!(info.remaining, 4);
        assert_eq!(info.reset_secs, 3600);
    }

    #[tokio::test(start_paused = true)]
    async fn remaining_is_the_smaller_of_window_and_bucket() {
        let l = ApiLimiter::new(limits(10_000, 60, 8));
        let first = pass(&l, 1).await;
        assert_eq!(first.limit, 10_000);
        assert_eq!(first.remaining, 59, "桶里 60 个令牌，取走一个");
        let second = pass(&l, 1).await;
        assert_eq!(second.remaining, 58);
        assert_eq!(second.reset_secs, 3600);
    }

    #[tokio::test(start_paused = true)]
    async fn organizations_do_not_share_state_and_override_replaces_the_default() {
        let l = ApiLimiter::new(limits(2, 60, 8));
        pass(&l, 1).await;
        pass(&l, 1).await;
        assert!(l.admit(1, None).await.is_err());
        pass(&l, 2).await;
        // 组织 3 的配额被覆盖成 3
        for _ in 0..3 {
            l.admit(3, Some(3)).await.unwrap();
        }
        assert_eq!(
            l.admit(3, Some(3)).await.unwrap_err().kind,
            RejectKind::HourlyQuota
        );
        // 配额调大后小时闸立即放宽（窗口内已用的 3 次保留），但令牌桶不凭空送令牌：先被突发闸拦住，
        // 上报的 limit 已经是新值；等令牌补出来（5/小时 = 每 720 秒一个）就放行
        let r = l.admit(3, Some(5)).await.unwrap_err();
        assert_eq!((r.kind, r.info.limit), (RejectKind::Burst, 5));
        assert_eq!(r.retry_after, Duration::from_secs(720));
        tokio::time::advance(Duration::from_secs(720)).await;
        let a = l.admit(3, Some(5)).await.unwrap();
        assert_eq!(a.info.limit, 5);
    }

    #[tokio::test(start_paused = true)]
    async fn concurrency_queues_then_times_out_with_429_semantics() {
        let l = Arc::new(ApiLimiter::new(limits(100_000, 1000, 2)));
        let a = l.admit(1, None).await.unwrap();
        let b = l.admit(1, None).await.unwrap();

        // 第三个请求排队：2 秒内没有名额释放 → 并发拒绝
        let started = tokio::time::Instant::now();
        let r = l.admit(1, None).await.unwrap_err();
        assert_eq!(r.kind, RejectKind::Concurrency);
        assert_eq!(r.retry_after, Duration::from_secs(2));
        assert_eq!(r.retry_after_secs(), 2);
        assert!(started.elapsed() >= Duration::from_secs(2));

        // 排队期间有名额释放：等待者拿到名额，不被拒绝
        let l2 = Arc::clone(&l);
        let waiter = tokio::spawn(async move { l2.admit(1, None).await });
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert!(!waiter.is_finished(), "还在排队");
        drop(a);
        let got = waiter.await.unwrap();
        assert!(got.is_ok(), "释放名额后排队者放行");

        // 别的组织不受影响
        l.admit(2, None).await.unwrap();
        drop((b, got));
        // 全部释放后恢复
        l.admit(1, None).await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn sweep_drops_only_idle_organizations() {
        let l = ApiLimiter::new(limits(3600, 3, 2));
        let held = l.admit(1, None).await.unwrap(); // 在途请求
        drop(l.admit(2, None).await.unwrap()); // 立即释放，但桶没满、窗口没过
        assert_eq!(l.tracked_orgs(), 2);
        l.sweep();
        assert_eq!(l.tracked_orgs(), 2, "窗口内且有未补满的桶，保留");
        tokio::time::advance(Duration::from_secs(3601)).await;
        l.sweep();
        assert_eq!(l.tracked_orgs(), 1, "组织 2 空闲，组织 1 还有在途请求");
        drop(held);
        l.sweep();
        assert_eq!(l.tracked_orgs(), 0);
    }
}
