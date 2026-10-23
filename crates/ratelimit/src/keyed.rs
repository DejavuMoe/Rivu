//! 按 `u64` 键分片的令牌桶表：采集侧按站点限流（`system-design.md` §4.2 第 3 步）。

use std::collections::HashMap;
use std::time::Duration;

use parking_lot::Mutex;

use crate::{Clock, SHARDS, TokenBucket, shard_of};

/// 键 → 令牌桶。16 个分片各一把锁，热路径只锁一个分片、做一次哈希查找和几次浮点运算；
/// 桶在键第一次出现时创建（唯一的分配），之后不再分配。
///
/// 限额按「每分钟 N 个」给出：容量 N（可以一次突发一分钟的量），补充速率 N/60 每秒。
/// 键的限额变化时（站点设置改了）在下一次调用里就地调整桶，不重置已用的令牌。
pub struct KeyedLimiter {
    clock: Clock,
    shards: [Mutex<HashMap<u64, TokenBucket>>; SHARDS],
}

impl Default for KeyedLimiter {
    fn default() -> Self {
        Self::new()
    }
}

impl KeyedLimiter {
    pub fn new() -> Self {
        KeyedLimiter {
            clock: Clock::new(),
            shards: std::array::from_fn(|_| Mutex::new(HashMap::new())),
        }
    }

    /// 取一个令牌；`true` 表示放行。`per_min` 为 0 时按 1 处理（调用方用 `None` 表示不限流，不会走到这里）。
    pub fn allow_per_min(&self, key: u64, per_min: u32) -> bool {
        self.allow_per_min_at(key, per_min, self.clock.now())
    }

    /// 同 [`allow_per_min`](Self::allow_per_min)，时间由调用方给出（测试用）。
    pub fn allow_per_min_at(&self, key: u64, per_min: u32, now: Duration) -> bool {
        let capacity = f64::from(per_min.max(1));
        let rate = capacity / 60.0;
        let mut shard = self.shards[shard_of(key)].lock();
        let bucket = shard
            .entry(key)
            .or_insert_with(|| TokenBucket::new(capacity, rate, now));
        if bucket.capacity() != capacity || bucket.rate() != rate {
            bucket.reconfigure(capacity, rate, now);
        }
        bucket.try_take(now).is_ok()
    }

    /// 只保留 `keep` 为真的键（站点被删除后清理它的桶）。
    pub fn retain(&self, keep: impl Fn(u64) -> bool) {
        for shard in &self.shards {
            shard.lock().retain(|k, _| keep(*k));
        }
    }

    /// 当前的桶数。
    pub fn len(&self) -> usize {
        self.shards.iter().map(|s| s.lock().len()).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    #[test]
    fn allows_the_per_minute_quota_then_rejects_and_refills_over_time() {
        let l = KeyedLimiter::new();
        // 每分钟 60 个：容量 60，每秒补 1 个
        for i in 0..60 {
            assert!(l.allow_per_min_at(7, 60, s(0)), "第 {i} 个");
        }
        assert!(!l.allow_per_min_at(7, 60, s(0)));
        assert!(!l.allow_per_min_at(7, 60, Duration::from_millis(999)));
        assert!(l.allow_per_min_at(7, 60, s(1)));
        assert!(!l.allow_per_min_at(7, 60, s(1)));
        // 一分钟后整桶补满
        for _ in 0..60 {
            assert!(l.allow_per_min_at(7, 60, s(61)));
        }
        assert!(!l.allow_per_min_at(7, 60, s(61)));
    }

    #[test]
    fn keys_are_independent() {
        let l = KeyedLimiter::new();
        assert!(l.allow_per_min_at(1, 1, s(0)));
        assert!(!l.allow_per_min_at(1, 1, s(0)));
        assert!(l.allow_per_min_at(2, 1, s(0)));
        assert_eq!(l.len(), 2);
    }

    #[test]
    fn a_changed_limit_applies_in_place_without_resetting() {
        let l = KeyedLimiter::new();
        for _ in 0..10 {
            assert!(l.allow_per_min_at(1, 10, s(0)));
        }
        assert!(!l.allow_per_min_at(1, 10, s(0)));
        // 调大：已用的令牌不重置，新容量靠时间补
        assert!(!l.allow_per_min_at(1, 600, s(0)));
        assert!(l.allow_per_min_at(1, 600, s(1)));
        // 调小：令牌立即钳到新容量（100 个里还剩 99 个，钳到 5 个）
        assert!(l.allow_per_min_at(2, 100, s(0)));
        let allowed = (0..10).filter(|_| l.allow_per_min_at(2, 5, s(0))).count();
        assert_eq!(allowed, 5);
    }

    #[test]
    fn retain_drops_buckets_of_removed_keys() {
        let l = KeyedLimiter::new();
        for k in 0..100 {
            l.allow_per_min_at(k, 10, s(0));
        }
        l.retain(|k| k % 2 == 0);
        assert_eq!(l.len(), 50);
        assert!(!l.is_empty());
    }

    #[test]
    fn works_without_a_runtime_using_the_wall_clock() {
        let l = KeyedLimiter::new();
        assert!(l.allow_per_min(1, 3));
        assert!(l.allow_per_min(1, 3));
        assert!(l.allow_per_min(1, 3));
        assert!(
            !l.allow_per_min(1, 3),
            "容量 3，补充速率 0.05/s，微秒内不会补出第 4 个"
        );
    }
}
