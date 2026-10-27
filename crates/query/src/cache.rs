//! 结果缓存（`system-design.md` §6.9）：进程内的 moka，看板与公开 API 共用。
//!
//! 键是这些内容的哈希：站点 id、请求（`QuerySpec` 的规范 JSON）、影响结果的站点配置（时区、周起始日、目标、
//! 地理库的署名与能力）、站点的数据代数，以及「现在」所在的时间桶。配置直接进键，所以改目标、改时区立即生效，
//! 不需要版本号；删除统计数据之后调用 [`QueryCache::invalidate_site`] 让代数加一，旧结果不再命中。
//!
//! 时间桶的长度按请求的范围：`realtime` 3 s；`day`、`24h` 10 s；含今天的范围 60 s；已结束的历史范围 10 min。
//! 同一个桶里的请求拿到同一份结果（按桶里第一次执行时的「现在」算出）。只缓存成功的结果。
//! 多个 api 进程各自缓存，删除数据后其他进程的旧结果最多再存活一个桶长。

use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use jiff::Timestamp;

use crate::resolve::SiteContext;
use crate::result::QueryResult;
use crate::spec::{DateRange, QuerySpec, RangeBound};

/// 缓存的总容量，按结果行数计（每个结果至少算 1 行）。
pub const DEFAULT_CAPACITY_ROWS: u64 = 500_000;

/// 最长的时间桶，也是条目的存活上限。
const MAX_BUCKET: Duration = Duration::from_secs(600);

pub struct QueryCache {
    results: moka::sync::Cache<u64, Arc<QueryResult>>,
    generations: Mutex<HashMap<u64, u64>>,
}

impl QueryCache {
    pub fn new(capacity_rows: u64) -> Self {
        QueryCache {
            results: moka::sync::Cache::builder()
                .max_capacity(capacity_rows)
                .weigher(|_, r: &Arc<QueryResult>| {
                    u32::try_from(r.results.len() + 1).unwrap_or(u32::MAX)
                })
                .time_to_live(MAX_BUCKET)
                .build(),
            generations: Mutex::new(HashMap::new()),
        }
    }

    fn generation(&self, site_id: u64) -> u64 {
        self.generations
            .lock()
            .map(|g| g.get(&site_id).copied().unwrap_or(0))
            .unwrap_or(0)
    }

    /// 站点的数据变了（删除统计数据）：之前缓存的结果都不再命中。
    pub fn invalidate_site(&self, site_id: u64) {
        if let Ok(mut g) = self.generations.lock() {
            *g.entry(site_id).or_insert(0) += 1;
        }
    }

    /// 这次请求的缓存键。
    pub fn key(&self, site: &SiteContext, spec: &QuerySpec, now: Timestamp) -> u64 {
        let mut h = DefaultHasher::new();
        site.site_id.hash(&mut h);
        self.generation(site.site_id).hash(&mut h);
        serde_json::to_string(spec).unwrap_or_default().hash(&mut h);
        site.timezone.iana_name().unwrap_or("").hash(&mut h);
        format!("{:?}", site.week_starts_on).hash(&mut h);
        format!("{:?}", site.goals).hash(&mut h);
        format!("{:?}", site.geo_attribution).hash(&mut h);
        format!("{:?}", site.geo_capability).hash(&mut h);
        let bucket = bucket_len(site, spec, now).as_millis() as i64;
        bucket.hash(&mut h);
        now.as_millisecond().div_euclid(bucket).hash(&mut h);
        h.finish()
    }

    pub fn get(&self, key: u64) -> Option<Arc<QueryResult>> {
        self.results.get(&key)
    }

    pub fn insert(&self, key: u64, result: Arc<QueryResult>) {
        self.results.insert(key, result);
    }
}

/// 时间桶的长度。
pub fn bucket_len(site: &SiteContext, spec: &QuerySpec, now: Timestamp) -> Duration {
    const REALTIME: Duration = Duration::from_secs(3);
    const TODAY: Duration = Duration::from_secs(10);
    const RECENT: Duration = Duration::from_secs(60);
    match &spec.date_range {
        DateRange::Realtime => REALTIME,
        DateRange::Day | DateRange::Hours24 => TODAY,
        DateRange::Custom(c) => {
            let ended = match (c.from, c.to) {
                (_, RangeBound::Date(to)) => to < now.to_zoned(site.timezone.clone()).date(),
                (_, RangeBound::Instant(to)) => to < now,
            };
            if ended { MAX_BUCKET } else { RECENT }
        }
        _ => RECENT,
    }
}

#[cfg(test)]
mod tests {
    use jiff::tz::TimeZone;
    use serde_json::json;

    use super::*;

    fn spec(v: serde_json::Value) -> QuerySpec {
        QuerySpec::parse(&v).unwrap()
    }

    fn site() -> SiteContext {
        SiteContext::new(7, TimeZone::get("Asia/Shanghai").unwrap())
    }

    #[test]
    fn buckets_follow_the_range() {
        let now: Timestamp = "2026-10-28T12:00:00+08:00".parse().unwrap();
        let b = |range: serde_json::Value| {
            bucket_len(
                &site(),
                &spec(json!({"metrics": ["visitors"], "date_range": range})),
                now,
            )
            .as_secs()
        };
        assert_eq!(b(json!("realtime")), 3);
        assert_eq!(b(json!("day")), 10);
        assert_eq!(b(json!("24h")), 10);
        assert_eq!(b(json!("7d")), 60);
        assert_eq!(b(json!("12mo")), 60);
        assert_eq!(b(json!(["2026-10-01", "2026-10-27"])), 600);
        // 终点是站点时区的今天：还会变
        assert_eq!(b(json!(["2026-10-01", "2026-10-28"])), 60);
        assert_eq!(
            b(json!([
                "2026-10-27T00:00:00+08:00",
                "2026-10-28T00:00:00+08:00"
            ])),
            600
        );
    }

    #[test]
    fn keys_change_with_the_bucket_config_and_generation() {
        let cache = QueryCache::new(100);
        let s = spec(json!({"metrics": ["visitors"], "date_range": "7d"}));
        let t0: Timestamp = "2026-10-28T12:00:00Z".parse().unwrap();
        let t1: Timestamp = "2026-10-28T12:00:30Z".parse().unwrap();
        let t2: Timestamp = "2026-10-28T12:01:00Z".parse().unwrap();
        let k = cache.key(&site(), &s, t0);
        assert_eq!(cache.key(&site(), &s, t1), k, "同一个 60 s 桶");
        assert_ne!(cache.key(&site(), &s, t2), k, "下一个桶");
        let other = spec(json!({"metrics": ["pageviews"], "date_range": "7d"}));
        assert_ne!(cache.key(&site(), &other, t0), k);
        let mut tz = site();
        tz.timezone = TimeZone::UTC;
        assert_ne!(cache.key(&tz, &s, t0), k, "时区进键");
        cache.invalidate_site(7);
        assert_ne!(cache.key(&site(), &s, t0), k, "删除数据后代数加一");
        let mut other_site = site();
        other_site.site_id = 8;
        let k8 = cache.key(&other_site, &s, t0);
        cache.invalidate_site(7);
        assert_eq!(cache.key(&other_site, &s, t0), k8, "只影响那个站点");
    }
}
