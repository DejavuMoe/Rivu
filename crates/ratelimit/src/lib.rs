//! 内存限流（`platform-and-ops.md` §4.4）：令牌桶、按键分片的桶表、按组织的 API 限流器。
//!
//! 全部是进程内状态，单节点准确；多副本时各进程独立计数（有效上限约为副本数倍），重启清零。
//! 要精确的全局限额需要换成 PG 或 Valkey，不在本 crate 范围内。
//!
//! - [`TokenBucket`]：纯数据结构，时间由调用方以 `Duration`（自某个起点起）传入，所以单元测试可以任意推进时间；
//! - [`Clock`]：基于 `tokio::time::Instant` 的单调时钟，`#[tokio::test(start_paused = true)]` 下可以暂停与推进；
//! - [`KeyedLimiter`]：`u64` 键到令牌桶的分片表，采集侧按站点限流用它；
//! - [`PolicyLimiter`]：固定限额、任意键的令牌桶表（登录、共享链接与公开看板，按 IP 加账号或链接）；
//! - [`ApiLimiter`]：公开 API 按组织限流（小时配额 + 突发令牌桶 + 并发信号量，`system-design.md` §6.8）。

mod api;
mod bucket;
mod clock;
mod keyed;
mod policy;

pub use api::{Admitted, ApiLimiter, ApiLimits, RateInfo, RejectKind, Rejection};
pub use bucket::TokenBucket;
pub use clock::Clock;
pub use keyed::KeyedLimiter;
pub use policy::PolicyLimiter;

/// 分片数。键空间是站点 id 或组织 id，数量级是几十到几万，16 个分片足以让锁竞争可以忽略。
const SHARDS: usize = 16;

/// 把键散到分片：乘以黄金比例常数后取高位，顺序递增的 id 也能均匀分开。
fn shard_of(key: u64) -> usize {
    (key.wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 60) as usize % SHARDS
}
