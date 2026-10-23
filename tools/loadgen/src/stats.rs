//! 延迟统计：HdrHistogram，微秒，1 µs 到 120 s，3 位有效数字。

use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use hdrhistogram::Histogram;
use serde_json::{Value, json};

const MAX_US: u64 = 120_000_000;

pub struct Lat(Mutex<Histogram<u64>>);

impl Default for Lat {
    fn default() -> Self {
        Lat(Mutex::new(
            Histogram::new_with_bounds(1, MAX_US, 3).expect("固定的直方图参数合法"),
        ))
    }
}

impl Lat {
    pub fn record(&self, d: Duration) {
        let us = u64::try_from(d.as_micros())
            .unwrap_or(MAX_US)
            .clamp(1, MAX_US);
        if let Ok(mut h) = self.0.lock() {
            let _ = h.record(us);
        }
    }

    pub fn count(&self) -> u64 {
        self.0.lock().map_or(0, |h| h.len())
    }

    /// 毫秒为单位的摘要。
    pub fn summary(&self) -> Value {
        let Ok(h) = self.0.lock() else {
            return Value::Null;
        };
        if h.is_empty() {
            return json!({"count": 0});
        }
        let ms = |us: u64| (us as f64) / 1000.0;
        json!({
            "count": h.len(),
            "min_ms": ms(h.min()),
            "mean_ms": h.mean() / 1000.0,
            "p50_ms": ms(h.value_at_quantile(0.50)),
            "p95_ms": ms(h.value_at_quantile(0.95)),
            "p99_ms": ms(h.value_at_quantile(0.99)),
            "p999_ms": ms(h.value_at_quantile(0.999)),
            "max_ms": ms(h.max()),
        })
    }

    pub fn quantile_ms(&self, q: f64) -> f64 {
        self.0
            .lock()
            .map_or(0.0, |h| h.value_at_quantile(q) as f64 / 1000.0)
    }
}

#[derive(Default)]
pub struct Counter(AtomicU64);

impl Counter {
    pub fn inc(&self) {
        self.0.fetch_add(1, Ordering::Relaxed);
    }

    pub fn get(&self) -> u64 {
        self.0.load(Ordering::Relaxed)
    }
}

/// 一个事件请求的结局。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// 202，没有 `x-rivu-dropped`
    Accepted,
    /// 202 + `x-rivu-dropped: 1`
    Dropped,
    Overloaded503,
    BadRequest400,
    OtherStatus,
    /// 连接失败、超时等没有拿到响应
    Error,
}

/// 一个阶段（固定速率的一段时间）的事件接入统计。
#[derive(Default)]
pub struct Ingest {
    /// 计划发送的数量（调度器按到达率算出的）
    pub planned: Counter,
    /// 因客户端在途请求数超限而没有发出的（不为零说明压测机本身饱和，结果无效）
    pub client_skipped: Counter,
    pub accepted: Counter,
    pub dropped: Counter,
    pub s503: Counter,
    pub s400: Counter,
    pub other: Counter,
    pub errors: Counter,
    /// 全部已完成请求的延迟，从**计划发送时刻**到响应读完
    pub latency: Lat,
    /// 其中 202 的延迟
    pub latency_202: Lat,
}

impl Ingest {
    pub fn record(&self, o: Outcome, lat: Duration) {
        match o {
            Outcome::Accepted => self.accepted.inc(),
            Outcome::Dropped => self.dropped.inc(),
            Outcome::Overloaded503 => self.s503.inc(),
            Outcome::BadRequest400 => self.s400.inc(),
            Outcome::OtherStatus => self.other.inc(),
            Outcome::Error => self.errors.inc(),
        }
        self.latency.record(lat);
        if matches!(o, Outcome::Accepted | Outcome::Dropped) {
            self.latency_202.record(lat);
        }
    }

    pub fn completed(&self) -> u64 {
        self.accepted.get()
            + self.dropped.get()
            + self.s503.get()
            + self.s400.get()
            + self.other.get()
            + self.errors.get()
    }

    pub fn to_json(&self) -> Value {
        json!({
            "planned": self.planned.get(),
            "client_skipped": self.client_skipped.get(),
            "completed": self.completed(),
            "accepted_202": self.accepted.get(),
            "dropped_202": self.dropped.get(),
            "status_503": self.s503.get(),
            "status_400": self.s400.get(),
            "other_status": self.other.get(),
            "errors": self.errors.get(),
            "latency_all": self.latency.summary(),
            "latency_202": self.latency_202.summary(),
        })
    }
}

#[derive(Default)]
pub struct QueryKind {
    pub ok: Counter,
    pub status_429: Counter,
    pub status_other: Counter,
    pub errors: Counter,
    pub latency: Lat,
}

impl QueryKind {
    pub fn to_json(&self) -> Value {
        json!({
            "ok_200": self.ok.get(),
            "status_429": self.status_429.get(),
            "status_other": self.status_other.get(),
            "errors": self.errors.get(),
            "latency": self.latency.summary(),
        })
    }
}

pub const QUERY_KINDS: [&str; 3] = ["overview", "breakdown_pages", "series_7d"];

/// 一个阶段内的全部统计。
pub struct PhaseStats {
    pub name: String,
    pub rate: f64,
    pub secs: u64,
    pub ingest: Ingest,
    pub queries: [QueryKind; 3],
    pub query_planned: Counter,
    pub query_all: Lat,
    pub probes_sent: Counter,
    pub probes_not_visible: Counter,
    pub probes_failed: Counter,
    pub visibility: Lat,
}

impl PhaseStats {
    pub fn new(name: &str, rate: f64, secs: u64) -> PhaseStats {
        PhaseStats {
            name: name.to_owned(),
            rate,
            secs,
            ingest: Ingest::default(),
            queries: Default::default(),
            query_planned: Counter::default(),
            query_all: Lat::default(),
            probes_sent: Counter::default(),
            probes_not_visible: Counter::default(),
            probes_failed: Counter::default(),
            visibility: Lat::default(),
        }
    }

    pub fn to_json(&self) -> Value {
        let mut queries = serde_json::Map::new();
        for (i, name) in QUERY_KINDS.iter().enumerate() {
            queries.insert((*name).to_owned(), self.queries[i].to_json());
        }
        json!({
            "name": self.name,
            "target_rate_per_s": self.rate,
            "duration_s": self.secs,
            "ingest": self.ingest.to_json(),
            "queries": {
                "planned": self.query_planned.get(),
                "all": self.query_all.summary(),
                "by_kind": Value::Object(queries),
            },
            "visibility": {
                "probes_sent": self.probes_sent.get(),
                "probes_failed_to_send": self.probes_failed.get(),
                "not_visible_within_timeout": self.probes_not_visible.get(),
                "latency": self.visibility.summary(),
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn histogram_reports_quantiles_in_milliseconds() {
        let l = Lat::default();
        for ms in 1..=1000u64 {
            l.record(Duration::from_millis(ms));
        }
        let s = l.summary();
        assert_eq!(s["count"], 1000);
        let p50 = s["p50_ms"].as_f64().unwrap();
        let p99 = s["p99_ms"].as_f64().unwrap();
        let max = s["max_ms"].as_f64().unwrap();
        assert!((495.0..=505.0).contains(&p50), "{p50}");
        assert!((985.0..=995.0).contains(&p99), "{p99}");
        assert!((999.0..=1001.0).contains(&max), "{max}");
    }

    #[test]
    fn empty_histogram_and_extremes_do_not_panic() {
        let l = Lat::default();
        assert_eq!(l.summary()["count"], 0);
        l.record(Duration::ZERO);
        l.record(Duration::from_secs(10_000));
        assert_eq!(l.count(), 2);
    }

    #[test]
    fn ingest_counts_outcomes_and_keeps_202_latency_separate() {
        let i = Ingest::default();
        i.record(Outcome::Accepted, Duration::from_millis(2));
        i.record(Outcome::Dropped, Duration::from_millis(3));
        i.record(Outcome::Overloaded503, Duration::from_millis(900));
        i.record(Outcome::Error, Duration::from_secs(10));
        assert_eq!(i.completed(), 4);
        assert_eq!(i.latency.count(), 4);
        assert_eq!(i.latency_202.count(), 2);
        let j = i.to_json();
        assert_eq!(j["accepted_202"], 1);
        assert_eq!(j["status_503"], 1);
    }
}
