//! 通用的按键令牌桶：登录（IP 与账号各一桶）、首次初始化、共享链接与公开看板（`(客户端 IP, 链接或站点)`）、
//! 共享链接密码尝试（`platform-and-ops.md` §4.4）。
//!
//! 与 [`KeyedLimiter`](crate::KeyedLimiter) 的区别：限额（容量与补充速率）在构造时固定，键是任意可哈希的值
//! （折成 64 位后分片存放，哈希种子是进程内随机的，外部无法构造冲突）；超限时返回「还要等多久」。
//! 桶表有上限：超过 [`PolicyLimiter::MAX_KEYS`] 时先清掉已经补满的桶（等价于从未出现过）；
//! 清掉之后仍然满员，新键放行并计数（`overflow`）——宁可在被海量不同键冲击时放行新键，也不让限流器自己成为拒绝服务的入口。

use std::collections::HashMap;
use std::hash::{BuildHasher, Hash, RandomState};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use parking_lot::Mutex;

use crate::{Clock, SHARDS, TokenBucket, shard_of};

/// 键 → 令牌桶，限额固定。
pub struct PolicyLimiter {
    clock: Clock,
    capacity: f64,
    rate: f64,
    hasher: RandomState,
    shards: [Mutex<HashMap<u64, TokenBucket>>; SHARDS],
    overflow: AtomicU64,
}

impl PolicyLimiter {
    /// 桶表的总上限（所有分片合计的软上限）。
    pub const MAX_KEYS: usize = 262_144;

    /// 容量 `capacity`（一次最多突发这么多次），平均每 `per` 补充 `refill` 个令牌。
    /// 例：「5 次 / 60 秒」是 `per_window(5, 5, 60s)`；「30 次 / 分钟、突发 10」是 `per_window(10, 30, 60s)`。
    pub fn per_window(capacity: u32, refill: u32, per: Duration) -> Self {
        let rate = f64::from(refill.max(1)) / per.as_secs_f64().max(1e-9);
        PolicyLimiter {
            clock: Clock::new(),
            capacity: f64::from(capacity.max(1)),
            rate,
            hasher: RandomState::new(),
            shards: std::array::from_fn(|_| Mutex::new(HashMap::new())),
            overflow: AtomicU64::new(0),
        }
    }

    /// 把任意可哈希的值折成桶表的键。
    pub fn key_of(&self, key: &impl Hash) -> u64 {
        self.hasher.hash_one(key)
    }

    /// 取一个令牌；`Ok` 放行，`Err(wait)` 是至少还要等多久才会有一个令牌。
    pub fn check(&self, key: &impl Hash) -> Result<(), Duration> {
        self.check_at(self.key_of(key), self.clock.now())
    }

    /// 同 [`check`](Self::check)，键已折成 `u64`、时间由调用方给出（测试用）。
    pub fn check_at(&self, key: u64, now: Duration) -> Result<(), Duration> {
        let shard = &self.shards[shard_of(key)];
        let mut map = shard.lock();
        if !map.contains_key(&key) && map.len() >= Self::MAX_KEYS / SHARDS {
            map.retain(|_, b| !b.is_full(now));
            if map.len() >= Self::MAX_KEYS / SHARDS {
                self.overflow.fetch_add(1, Ordering::Relaxed);
                return Ok(());
            }
        }
        map.entry(key)
            .or_insert_with(|| TokenBucket::new(self.capacity, self.rate, now))
            .try_take(now)
    }

    /// 因桶表满员而放行的新键次数。
    pub fn overflow(&self) -> u64 {
        self.overflow.load(Ordering::Relaxed)
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
    fn five_per_minute_then_rejects_with_the_wait_until_the_next_token() {
        let l = PolicyLimiter::per_window(5, 5, s(60));
        let k = l.key_of(&"a@example.com");
        for i in 0..5 {
            assert!(l.check_at(k, s(0)).is_ok(), "第 {i} 次");
        }
        let wait = l.check_at(k, s(0)).unwrap_err();
        assert_eq!(wait, s(12), "5 次/60 秒 = 每 12 秒补一个");
        assert!(l.check_at(k, s(11)).is_err());
        assert!(l.check_at(k, s(12)).is_ok());
    }

    #[test]
    fn burst_of_ten_with_thirty_per_minute() {
        let l = PolicyLimiter::per_window(10, 30, s(60));
        let k = l.key_of(&("203.0.113.9", 7u64));
        for _ in 0..10 {
            assert!(l.check_at(k, s(0)).is_ok());
        }
        assert!(l.check_at(k, s(0)).is_err());
        // 每秒补 0.5 个：两秒一个
        assert!(l.check_at(k, s(1)).is_err());
        assert!(l.check_at(k, s(2)).is_ok());
    }

    #[test]
    fn keys_are_independent_and_tuple_keys_distinguish_their_parts() {
        let l = PolicyLimiter::per_window(1, 1, s(60));
        let (a, b, c) = (
            l.key_of(&("1.1.1.1", 1u64)),
            l.key_of(&("1.1.1.1", 2u64)),
            l.key_of(&("2.2.2.2", 1u64)),
        );
        assert!(l.check_at(a, s(0)).is_ok());
        assert!(l.check_at(a, s(0)).is_err());
        assert!(l.check_at(b, s(0)).is_ok());
        assert!(l.check_at(c, s(0)).is_ok());
        assert_eq!(l.len(), 3);
    }

    #[test]
    fn works_with_the_wall_clock() {
        let l = PolicyLimiter::per_window(2, 2, s(60));
        assert!(l.check(&"x").is_ok());
        assert!(l.check(&"x").is_ok());
        assert!(l.check(&"x").is_err());
        assert!(l.check(&"y").is_ok());
    }

    #[test]
    fn a_full_table_prunes_idle_buckets_then_lets_new_keys_through() {
        let l = PolicyLimiter::per_window(1, 1, s(1));
        let per_shard = PolicyLimiter::MAX_KEYS / SHARDS;
        // 把 0 号分片灌满：找 per_shard 个落在同一分片的键，每个都被用掉一个令牌（桶是空的，不会被清掉）
        let mut keys = Vec::new();
        let mut k = 0u64;
        while keys.len() < per_shard {
            if shard_of(k) == 0 {
                keys.push(k);
            }
            k += 1;
        }
        for key in &keys {
            assert!(l.check_at(*key, s(0)).is_ok());
        }
        // 新键：表满且没有已补满的桶可清 -> 放行并计数
        let extra = (k..).find(|x| shard_of(*x) == 0).unwrap();
        assert!(l.check_at(extra, s(0)).is_ok());
        assert_eq!(l.overflow(), 1);
        // 过了补充周期，旧桶都补满了：清表后新键正常入表
        assert!(l.check_at(extra, s(10)).is_ok());
        assert_eq!(l.overflow(), 1);
        assert!(l.len() <= 1);
    }
}
