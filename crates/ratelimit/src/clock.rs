//! 单调时钟：自构造时刻起经过的时间。

use std::time::Duration;

use tokio::time::Instant;

/// 基于 `tokio::time::Instant`：没有 tokio 运行时、或时钟没有暂停时与 `std::time::Instant` 一样走真实时间；
/// `#[tokio::test(start_paused = true)]` 下随 `tokio::time::advance` 推进。可以复制，复制出来的时钟共用同一个起点。
#[derive(Clone, Copy, Debug)]
pub struct Clock {
    base: Instant,
}

impl Default for Clock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock {
    pub fn new() -> Self {
        Clock {
            base: Instant::now(),
        }
    }

    /// 自起点起经过的时间。
    pub fn now(&self) -> Duration {
        Instant::now().saturating_duration_since(self.base)
    }
}
