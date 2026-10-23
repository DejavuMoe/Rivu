//! 令牌桶核心。非线程安全：调用方负责加锁。

use std::time::Duration;

/// 令牌桶：容量 `capacity`（突发上限），每秒补充 `rate` 个令牌，创建时是满的。
///
/// 时间以「自某个固定起点起的 `Duration`」传入，必须单调；传入比上次更早的时间时按「没有时间流逝」处理，
/// 不会让令牌变多或变少。令牌用 `f64` 计，每次取走一个。
#[derive(Clone, Debug)]
pub struct TokenBucket {
    capacity: f64,
    rate: f64,
    tokens: f64,
    last: Duration,
}

/// 最长的「多久后重试」：避免速率极小时 `Duration` 溢出。
const MAX_WAIT: Duration = Duration::from_secs(u32::MAX as u64);

impl TokenBucket {
    /// `capacity` 至少 1，`rate` 必须为正（否则按极小值处理）。
    pub fn new(capacity: f64, rate_per_sec: f64, now: Duration) -> Self {
        let capacity = capacity.max(1.0);
        TokenBucket {
            capacity,
            rate: rate_per_sec.max(f64::MIN_POSITIVE),
            tokens: capacity,
            last: now,
        }
    }

    pub fn capacity(&self) -> f64 {
        self.capacity
    }

    pub fn rate(&self) -> f64 {
        self.rate
    }

    fn refill(&mut self, now: Duration) {
        if now > self.last {
            let elapsed = (now - self.last).as_secs_f64();
            self.tokens = (self.tokens + elapsed * self.rate).min(self.capacity);
            self.last = now;
        }
    }

    /// 取一个令牌。成功返回 `Ok`；令牌不足返回「至少要等多久才会有一个令牌」。
    pub fn try_take(&mut self, now: Duration) -> Result<(), Duration> {
        self.refill(now);
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            Ok(())
        } else {
            let wait = (1.0 - self.tokens) / self.rate;
            Err(Duration::try_from_secs_f64(wait).unwrap_or(MAX_WAIT))
        }
    }

    /// 此刻可用的令牌数（含小数部分）。
    pub fn available(&mut self, now: Duration) -> f64 {
        self.refill(now);
        self.tokens
    }

    /// 桶是满的（空闲）。
    pub fn is_full(&mut self, now: Duration) -> bool {
        self.refill(now);
        self.tokens >= self.capacity
    }

    /// 改容量与速率：先按旧参数补到此刻，再把令牌钳到新容量内（调小立即生效，调大不凭空送令牌）。
    pub fn reconfigure(&mut self, capacity: f64, rate_per_sec: f64, now: Duration) {
        self.refill(now);
        self.capacity = capacity.max(1.0);
        self.rate = rate_per_sec.max(f64::MIN_POSITIVE);
        self.tokens = self.tokens.min(self.capacity);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    #[test]
    fn starts_full_and_drains_to_the_capacity() {
        let mut b = TokenBucket::new(3.0, 1.0, s(0));
        for _ in 0..3 {
            assert!(b.try_take(s(0)).is_ok());
        }
        assert_eq!(b.try_take(s(0)), Err(s(1)), "空桶要等一个补充周期");
    }

    #[test]
    fn refills_with_elapsed_time_and_never_exceeds_the_capacity() {
        let mut b = TokenBucket::new(3.0, 2.0, s(0));
        for _ in 0..3 {
            b.try_take(s(0)).unwrap();
        }
        // 0.5 秒补 1 个
        assert!(b.try_take(Duration::from_millis(499)).is_err());
        assert!(b.try_take(Duration::from_millis(500)).is_ok());
        // 很久以后也只有 capacity 个
        assert!((b.available(s(10_000)) - 3.0).abs() < 1e-9);
        assert!(b.is_full(s(10_000)));
    }

    #[test]
    fn retry_after_shrinks_as_time_passes() {
        let mut b = TokenBucket::new(1.0, 0.5, s(0)); // 每 2 秒一个
        b.try_take(s(0)).unwrap();
        assert_eq!(b.try_take(s(0)), Err(s(2)));
        assert_eq!(b.try_take(s(1)), Err(s(1)));
        assert!(b.try_take(s(2)).is_ok());
    }

    #[test]
    fn time_going_backwards_is_ignored() {
        let mut b = TokenBucket::new(2.0, 1.0, s(100));
        b.try_take(s(100)).unwrap();
        b.try_take(s(100)).unwrap();
        // 回到过去：没有令牌流入，也不会 panic
        assert!(b.try_take(s(50)).is_err());
        assert!(b.try_take(s(101)).is_ok());
    }

    #[test]
    fn reconfigure_clamps_down_immediately_and_does_not_gift_tokens_when_growing() {
        let mut b = TokenBucket::new(10.0, 1.0, s(0));
        b.reconfigure(2.0, 1.0, s(0));
        assert!((b.available(s(0)) - 2.0).abs() < 1e-9);
        let mut b = TokenBucket::new(2.0, 1.0, s(0));
        b.try_take(s(0)).unwrap();
        b.try_take(s(0)).unwrap();
        b.reconfigure(10.0, 1.0, s(0));
        assert!(b.available(s(0)) < 1e-9, "扩容不凭空送令牌");
        assert!((b.available(s(3)) - 3.0).abs() < 1e-9);
    }

    #[test]
    fn tiny_rates_do_not_overflow_the_wait() {
        let mut b = TokenBucket::new(1.0, 0.0, s(0));
        b.try_take(s(0)).unwrap();
        let wait = b.try_take(s(0)).unwrap_err();
        assert!(wait >= s(1_000_000));
    }
}
