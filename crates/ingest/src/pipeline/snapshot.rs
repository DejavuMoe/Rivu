//! 站点快照与盐缓存：热路径只读内存，后台任务定期从 PG 全量重载。
//!
//! P1 只有全量重载（`RIVU_SITE_CACHE_REFRESH_SECS`，默认 10 s）；增量刷新与 `LISTEN/NOTIFY` 留到以后
//! （`system-design.md` §4.3）。热路径不回源：查不到 `site_key` 就按 `unknown_site` 丢弃。

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::Result;
use arc_swap::ArcSwap;
use jiff::Timestamp;
use jiff::civil::Date;
use jiff::tz::TimeZone;
use rivu_geo::GeoLevel;
use rivu_storage_pg::salts::Salt;
use rivu_storage_pg::sites::SiteRow;
use sqlx::PgPool;
use tokio::sync::watch;
use tokio::task::JoinHandle;

use crate::hostname::HostnamePatterns;

/// 一个站点在热路径上需要的配置。
#[derive(Clone, Debug)]
pub struct SiteConfig {
    pub site_id: u64,
    pub timezone: TimeZone,
    pub collect_enabled: bool,
    pub hostnames: HostnamePatterns,
    /// 地理精度（`site_settings.geo_level`）：第 6 步按它截断，更细的字段不存。
    pub geo_level: GeoLevel,
    /// 站点脚本是否用 hash 路由模式（`site_settings.tracker.hash`，只影响 `/js/<site_key>.js` 的配置前缀）。
    pub tracker_hash: bool,
}

impl SiteConfig {
    /// 从 PG 行构造；时区名无效时返回错误。`geo_level` 解析失败按 `country`（最粗的精度，
    /// 不会多存数据）处理并记 `warn`：PG 的 CHECK 保证取值，这里只是防御。
    pub fn from_row(row: &SiteRow) -> Result<Self> {
        let timezone = TimeZone::get(&row.timezone)
            .map_err(|e| anyhow::anyhow!("站点 {} 的时区 `{}` 无效: {e}", row.id, row.timezone))?;
        let geo_level = row.geo_level.parse::<GeoLevel>().unwrap_or_else(|e| {
            tracing::warn!(site_id = row.id, error = %e, "站点的 geo_level 无效，按 country 处理");
            GeoLevel::Country
        });
        Ok(SiteConfig {
            site_id: row.id,
            timezone,
            collect_enabled: row.collect_enabled,
            hostnames: HostnamePatterns::new(row.hostnames.iter().map(String::as_str)),
            geo_level,
            tracker_hash: row.tracker_hash,
        })
    }
}

/// `site_key -> SiteConfig`。原子替换，读取无锁。
pub struct SiteSnapshot {
    map: ArcSwap<HashMap<String, Arc<SiteConfig>>>,
    loaded: AtomicBool,
}

impl Default for SiteSnapshot {
    fn default() -> Self {
        Self::new()
    }
}

impl SiteSnapshot {
    /// 空快照，`is_loaded()` 为 false，直到第一次 [`SiteSnapshot::replace`]。
    pub fn new() -> Self {
        SiteSnapshot {
            map: ArcSwap::from_pointee(HashMap::new()),
            loaded: AtomicBool::new(false),
        }
    }

    pub fn get(&self, site_key: &str) -> Option<Arc<SiteConfig>> {
        self.map.load().get(site_key).cloned()
    }

    pub fn len(&self) -> usize {
        self.map.load().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// 至少成功加载过一次（`/readyz` 用）。
    pub fn is_loaded(&self) -> bool {
        self.loaded.load(Ordering::Acquire)
    }

    /// 整体替换并标记为已加载。
    pub fn replace(&self, sites: HashMap<String, Arc<SiteConfig>>) {
        self.map.store(Arc::new(sites));
        self.loaded.store(true, Ordering::Release);
    }

    /// 把 PG 行转成快照内容。时区无效的站点被跳过并记 `error`（该站点的事件会按 `unknown_site` 丢弃）。
    pub fn build(rows: &[SiteRow]) -> HashMap<String, Arc<SiteConfig>> {
        let mut out = HashMap::with_capacity(rows.len());
        for row in rows {
            match SiteConfig::from_row(row) {
                Ok(cfg) => {
                    out.insert(row.site_key.clone(), Arc::new(cfg));
                }
                Err(e) => {
                    tracing::error!(site_id = row.id, error = %e, "站点配置无效，已从快照中跳过");
                }
            }
        }
        out
    }

    /// 从 PG 全量重载一次，返回站点数。
    pub async fn reload(&self, pool: &PgPool) -> Result<usize> {
        let rows = rivu_storage_pg::sites::load_sites(pool).await?;
        let map = Self::build(&rows);
        let n = map.len();
        self.replace(map);
        Ok(n)
    }

    /// 启动后台全量重载：立即加载一次，之后每 `every` 一次。失败只记日志，继续用旧快照。
    pub fn spawn_refresh(
        self: &Arc<Self>,
        pool: PgPool,
        every: Duration,
        stop: watch::Receiver<bool>,
    ) -> JoinHandle<()> {
        let this = Arc::clone(self);
        spawn_periodic(every, stop, move || {
            let this = Arc::clone(&this);
            let pool = pool.clone();
            async move {
                match this.reload(&pool).await {
                    Ok(n) => tracing::debug!(sites = n, "站点快照已刷新"),
                    Err(e) => {
                        tracing::warn!(error = %format!("{e:#}"), "刷新站点快照失败，继续使用旧快照")
                    }
                }
            }
        })
    }
}

/// 盐缓存使用的日期范围：UTC 的 `today-3 ..= today+2`。
/// 事件的本地日期在 `today-1 ..= today+1`，`prev_visitor_id` 还要再早一天（`salts_rotate` 保留到 `today-2`）。
pub fn salt_window(today_utc: Date) -> (Date, Date) {
    let from = today_utc.saturating_sub(jiff::Span::new().days(3));
    let to = today_utc.saturating_add(jiff::Span::new().days(2));
    (from, to)
}

/// 当前的 UTC 日期。
pub fn utc_today() -> Date {
    Timestamp::now().to_zoned(TimeZone::UTC).date()
}

/// `日期 -> 盐`。collector 只读不生成：缺盐就丢弃事件（`salt_missing`）。
pub struct SaltCache {
    map: ArcSwap<HashMap<Date, Salt>>,
    loaded: AtomicBool,
}

impl Default for SaltCache {
    fn default() -> Self {
        Self::new()
    }
}

impl SaltCache {
    pub fn new() -> Self {
        SaltCache {
            map: ArcSwap::from_pointee(HashMap::new()),
            loaded: AtomicBool::new(false),
        }
    }

    pub fn get(&self, day: Date) -> Option<Salt> {
        self.map.load().get(&day).copied()
    }

    pub fn replace(&self, salts: HashMap<Date, Salt>) {
        self.map.store(Arc::new(salts));
        self.loaded.store(true, Ordering::Release);
    }

    /// 至少成功加载过一次。
    pub fn is_loaded(&self) -> bool {
        self.loaded.load(Ordering::Acquire)
    }

    /// `today-1 ..= today+1` 的盐是否都在（`/readyz` 用）：UTC 以西/以东的站点本地日期可能偏离 UTC 日期一天。
    pub fn covers(&self, today_utc: Date) -> bool {
        let map = self.map.load();
        [-1i8, 0, 1].iter().all(|offset| {
            today_utc
                .checked_add(jiff::Span::new().days(*offset))
                .is_ok_and(|d| map.contains_key(&d))
        })
    }

    /// 从 PG 读取 `[from, to]` 的盐并整体替换，返回条数。
    pub async fn reload(&self, pool: &PgPool, from: Date, to: Date) -> Result<usize> {
        let rows = rivu_storage_pg::salts::load_salts(pool, from, to).await?;
        let n = rows.len();
        self.replace(rows.into_iter().collect());
        Ok(n)
    }

    /// 启动后台重载：立即加载一次，之后每 `every`（默认 60 s）一次。
    pub fn spawn_refresh(
        self: &Arc<Self>,
        pool: PgPool,
        every: Duration,
        stop: watch::Receiver<bool>,
    ) -> JoinHandle<()> {
        let this = Arc::clone(self);
        spawn_periodic(every, stop, move || {
            let this = Arc::clone(&this);
            let pool = pool.clone();
            async move {
                let (from, to) = salt_window(utc_today());
                match this.reload(&pool, from, to).await {
                    Ok(n) => tracing::debug!(salts = n, "盐缓存已刷新"),
                    Err(e) => {
                        tracing::warn!(error = %format!("{e:#}"), "刷新盐缓存失败，继续使用旧缓存")
                    }
                }
            }
        })
    }
}

/// 立即执行一次 `job`，之后每 `every` 执行一次，直到收到停止信号。
pub(crate) fn spawn_periodic<F, Fut>(
    every: Duration,
    mut stop: watch::Receiver<bool>,
    mut job: F,
) -> JoinHandle<()>
where
    F: FnMut() -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(every);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            // 第一次 tick 立即返回，所以进程启动时马上加载一次。
            tokio::select! {
                () = stopped(&mut stop) => break,
                _ = ticker.tick() => job().await,
            }
        }
    })
}

/// 等到停止信号（发送端被丢弃也算停止）。`watch::Ref` 不是 `Send`，不能带过 await，所以包一层。
async fn stopped(rx: &mut watch::Receiver<bool>) {
    let _ = rx.wait_for(|s| *s).await;
}

#[cfg(test)]
mod tests {
    use jiff::civil::date;

    use super::*;

    fn row(id: u64, key: &str, tz: &str, hosts: &[&str]) -> SiteRow {
        SiteRow {
            id,
            site_key: key.into(),
            timezone: tz.into(),
            collect_enabled: true,
            hostnames: hosts.iter().map(|s| (*s).to_owned()).collect(),
            geo_level: "city".into(),
            tracker_hash: false,
        }
    }

    #[test]
    fn geo_level_is_parsed_and_an_invalid_value_falls_back_to_country() {
        for (raw, want) in [
            ("country", GeoLevel::Country),
            ("region", GeoLevel::Region),
            ("city", GeoLevel::City),
            ("planet", GeoLevel::Country),
            ("", GeoLevel::Country),
            ("City", GeoLevel::Country),
        ] {
            let mut r = row(1, "aaaaaaaaaaaa", "UTC", &["a.test"]);
            r.geo_level = raw.into();
            assert_eq!(SiteConfig::from_row(&r).unwrap().geo_level, want, "{raw:?}");
        }
    }

    #[test]
    fn tracker_hash_flag_is_carried_into_the_config() {
        let mut r = row(1, "aaaaaaaaaaaa", "UTC", &["a.test"]);
        assert!(!SiteConfig::from_row(&r).unwrap().tracker_hash);
        r.tracker_hash = true;
        assert!(SiteConfig::from_row(&r).unwrap().tracker_hash);
    }

    #[test]
    fn build_skips_sites_with_invalid_timezone() {
        let map = SiteSnapshot::build(&[
            row(1, "aaaaaaaaaaaa", "Europe/Berlin", &["example.test"]),
            row(2, "bbbbbbbbbbbb", "Mars/Olympus", &["x.test"]),
        ]);
        assert_eq!(map.len(), 1);
        assert_eq!(map["aaaaaaaaaaaa"].site_id, 1);
    }

    #[test]
    fn snapshot_is_not_loaded_until_replaced() {
        let s = SiteSnapshot::new();
        assert!(!s.is_loaded());
        assert!(s.get("x").is_none());
        s.replace(SiteSnapshot::build(&[row(
            7,
            "ccccccccccc2",
            "UTC",
            &["a.test", "*.b.test"],
        )]));
        assert!(s.is_loaded());
        let cfg = s.get("ccccccccccc2").unwrap();
        assert_eq!(cfg.site_id, 7);
        assert!(cfg.hostnames.matches("a.test"));
        assert!(cfg.hostnames.matches("x.b.test"));
        assert!(!cfg.hostnames.matches("b.test"));
        // 整体替换：旧站点消失
        s.replace(HashMap::new());
        assert!(s.is_loaded() && s.is_empty());
    }

    #[test]
    fn empty_hostname_list_yields_an_empty_whitelist() {
        let cfg = SiteConfig::from_row(&row(1, "aaaaaaaaaaaa", "UTC", &[])).unwrap();
        assert!(cfg.hostnames.is_empty());
    }

    #[test]
    fn salts_must_cover_yesterday_today_and_tomorrow() {
        let c = SaltCache::new();
        let today = date(2026, 3, 29);
        assert!(!c.is_loaded());
        assert!(!c.covers(today));
        let mut m = HashMap::new();
        m.insert(date(2026, 3, 28), [1u8; 16]);
        m.insert(date(2026, 3, 29), [2u8; 16]);
        c.replace(m.clone());
        assert!(c.is_loaded());
        assert!(!c.covers(today), "缺 today+1");
        m.insert(date(2026, 3, 30), [3u8; 16]);
        c.replace(m);
        assert!(c.covers(today));
        assert!(!c.covers(date(2026, 3, 30)), "缺 3/31");
        assert_eq!(c.get(date(2026, 3, 29)), Some([2u8; 16]));
        assert_eq!(c.get(date(2026, 3, 1)), None);
    }

    #[test]
    fn salt_window_spans_three_days_back_and_two_ahead() {
        let (from, to) = salt_window(date(2026, 3, 1));
        assert_eq!(from, date(2026, 2, 26));
        assert_eq!(to, date(2026, 3, 3));
    }

    #[tokio::test(start_paused = true)]
    async fn periodic_job_runs_immediately_then_every_interval_until_stopped() {
        use std::sync::atomic::AtomicUsize;
        let n = Arc::new(AtomicUsize::new(0));
        let (stop_tx, stop_rx) = watch::channel(false);
        let n2 = Arc::clone(&n);
        let h = spawn_periodic(Duration::from_secs(10), stop_rx, move || {
            let n = Arc::clone(&n2);
            async move {
                n.fetch_add(1, Ordering::SeqCst);
            }
        });
        tokio::time::sleep(Duration::from_millis(1)).await;
        assert_eq!(n.load(Ordering::SeqCst), 1, "启动时立即执行一次");
        tokio::time::sleep(Duration::from_secs(25)).await;
        assert_eq!(n.load(Ordering::SeqCst), 3);
        stop_tx.send(true).unwrap();
        h.await.unwrap();
        tokio::time::sleep(Duration::from_secs(60)).await;
        assert_eq!(n.load(Ordering::SeqCst), 3, "停止后不再执行");
    }
}
