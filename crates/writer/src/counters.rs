//! 接入计数：进程内按 `(site_id, 10 秒桶, metric, script_version)` 累加，
//! 只刷出已结束的桶，关停时全部刷出（`system-design.md` §5.4）。
//!
//! 表的粒度是分钟：刷出时按桶所在的分钟取整并合并同键的行，`SummingMergeTree` 合并时会把同键行相加。

use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use parking_lot::Mutex;
use rivu_storage_ch::rows::IngestCounterRow;
use time::OffsetDateTime;

/// 计数桶的宽度（秒）。
pub const BUCKET_SECS: i64 = 10;

/// `ingest_counters.script_version` 是 `UInt16`，而协议里的 `sv` 最大 999999：超出的钳到 65535。
pub fn clamp_script_version(sv: u32) -> u16 {
    u16::try_from(sv).unwrap_or(u16::MAX)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct Key {
    site_id: u64,
    /// 桶起点（Unix 秒，10 的倍数）。
    bucket: i64,
    metric: &'static str,
    script_version: u16,
}

/// 进程内计数器。`add` 在请求热路径上调用，只做一次加锁和一次哈希表更新。
#[derive(Default)]
pub struct CounterBuffer {
    inner: Mutex<HashMap<Key, u64>>,
}

pub(crate) fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

impl CounterBuffer {
    /// 在 Unix 时间 `at_secs` 所在的 10 秒桶里加 1。
    pub fn add(&self, site_id: u64, metric: &'static str, script_version: u32, at_secs: i64) {
        let key = Key {
            site_id,
            bucket: at_secs.div_euclid(BUCKET_SECS) * BUCKET_SECS,
            metric,
            script_version: clamp_script_version(script_version),
        };
        *self.inner.lock().entry(key).or_insert(0) += 1;
    }

    /// 取出并移除已经结束的桶（桶终点 ≤ `now_secs`），合并成分钟粒度的行。
    pub fn take_ended(&self, now_secs: i64) -> Vec<IngestCounterRow> {
        let mut map = self.inner.lock();
        let ended: Vec<Key> = map
            .keys()
            .filter(|k| k.bucket + BUCKET_SECS <= now_secs)
            .copied()
            .collect();
        let taken: Vec<(Key, u64)> = ended
            .into_iter()
            .filter_map(|k| map.remove(&k).map(|v| (k, v)))
            .collect();
        drop(map);
        to_minute_rows(taken)
    }

    /// 取出并移除全部桶（关停时）。
    pub fn take_all(&self) -> Vec<IngestCounterRow> {
        let taken: Vec<(Key, u64)> = self.inner.lock().drain().collect();
        to_minute_rows(taken)
    }

    /// 还没有刷出的桶数（诊断、测试用）。
    pub fn pending_buckets(&self) -> usize {
        self.inner.lock().len()
    }
}

/// 把 10 秒桶按所在分钟合并，行顺序固定（便于测试与确定的重试内容）。
fn to_minute_rows(items: Vec<(Key, u64)>) -> Vec<IngestCounterRow> {
    let mut merged: HashMap<(u64, i64, &'static str, u16), u64> = HashMap::new();
    for (k, v) in items {
        let minute = k.bucket.div_euclid(60) * 60;
        *merged
            .entry((k.site_id, minute, k.metric, k.script_version))
            .or_insert(0) += v;
    }
    let mut rows: Vec<IngestCounterRow> = merged
        .into_iter()
        .filter_map(|((site_id, minute, metric, script_version), value)| {
            Some(IngestCounterRow {
                site_id,
                minute: OffsetDateTime::from_unix_timestamp(minute).ok()?,
                metric: metric.to_owned(),
                script_version,
                value,
            })
        })
        .collect();
    rows.sort_by(|a, b| {
        (a.site_id, a.minute, &a.metric, a.script_version).cmp(&(
            b.site_id,
            b.minute,
            &b.metric,
            b.script_version,
        ))
    });
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    fn total(rows: &[IngestCounterRow]) -> u64 {
        rows.iter().map(|r| r.value).sum()
    }

    #[test]
    fn same_bucket_accumulates_and_keys_stay_separate() {
        let c = CounterBuffer::default();
        for _ in 0..3 {
            c.add(1, "accepted", 7, 1_000);
        }
        c.add(1, "accepted", 8, 1_000); // 另一个脚本版本
        c.add(1, "dropped_bot", 7, 1_000); // 另一个指标
        c.add(2, "accepted", 7, 1_000); // 另一个站点
        assert_eq!(c.pending_buckets(), 4);
        let rows = c.take_all();
        assert_eq!(rows.len(), 4);
        assert_eq!(total(&rows), 6);
        let acc = rows
            .iter()
            .find(|r| r.site_id == 1 && r.metric == "accepted" && r.script_version == 7)
            .unwrap();
        assert_eq!(acc.value, 3);
        assert_eq!(c.pending_buckets(), 0);
    }

    #[test]
    fn only_ended_buckets_are_taken() {
        let c = CounterBuffer::default();
        c.add(1, "accepted", 0, 100); // 桶 [100, 110)
        c.add(1, "accepted", 0, 109);
        c.add(1, "accepted", 0, 110); // 桶 [110, 120)
        // 109 秒时 [100,110) 还没结束
        assert!(c.take_ended(109).is_empty());
        // 110 秒：第一个桶结束，第二个还在进行
        let rows = c.take_ended(110);
        assert_eq!(total(&rows), 2);
        assert_eq!(c.pending_buckets(), 1);
        assert_eq!(total(&c.take_ended(120)), 1);
        assert_eq!(c.pending_buckets(), 0);
    }

    #[test]
    fn buckets_in_the_same_minute_merge_into_one_row() {
        let c = CounterBuffer::default();
        // 同一分钟 [120, 180) 内的三个不同桶
        c.add(1, "accepted", 0, 121);
        c.add(1, "accepted", 0, 139);
        c.add(1, "accepted", 0, 179);
        // 下一分钟
        c.add(1, "accepted", 0, 180);
        let rows = c.take_all();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].minute.unix_timestamp(), 120);
        assert_eq!(rows[0].value, 3);
        assert_eq!(rows[1].minute.unix_timestamp(), 180);
        assert_eq!(rows[1].value, 1);
        assert!(rows.iter().all(|r| r.minute.unix_timestamp() % 60 == 0));
    }

    #[test]
    fn script_version_is_clamped_to_u16() {
        assert_eq!(clamp_script_version(0), 0);
        assert_eq!(clamp_script_version(65_535), 65_535);
        assert_eq!(clamp_script_version(65_536), 65_535);
        assert_eq!(clamp_script_version(999_999), 65_535);
    }

    #[test]
    fn negative_times_do_not_panic() {
        let c = CounterBuffer::default();
        c.add(1, "accepted", 0, -1);
        let rows = c.take_all();
        assert_eq!(rows[0].minute.unix_timestamp(), -60);
    }
}
