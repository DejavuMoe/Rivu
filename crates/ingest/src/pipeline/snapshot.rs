//! 站点快照与盐缓存：热路径只读内存，后台任务从 PG 刷新。
//!
//! 站点快照（`system-design.md` §4.3、`platform-and-ops.md` §3.3）：
//! - 每 `RIVU_SITE_CACHE_REFRESH_SECS`（默认 10 s）按 `updated_at` 水位增量刷新：只重新加载变更过的站点，
//!   已删除的从快照摘除（删除是置 `deleted_at`，同样更新 `updated_at`）；
//! - 每 [`FULL_RELOAD_EVERY`] 全量重载一次兜底；
//! - `LISTEN rivu_site_changed`：PG 触发器在站点配置变更时 NOTIFY，收到后立即增量刷新，新站点约 1 秒内可用。
//!
//! 热路径不回源：查不到 `site_key` 就按 `unknown_site` 丢弃。

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

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
use crate::shield::ShieldRules;

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
    /// 屏蔽规则（`shield_rules`）：IP 集合、CIDR 前缀树、国家集合、页面通配、hostname 规则，
    /// 随快照整体替换；管线第 3、4、6 步使用。
    pub shield: ShieldRules,
    /// `site_settings.ingest_rate_limit_per_min`：每分钟事件数上限；`None` 用全局默认
    /// （`RIVU_INGEST_RATE_LIMIT_PER_MIN`，由管线持有）。
    pub ingest_rate_limit_per_min: Option<u32>,
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
        // 规则在写入时已经校验；这里编译失败的行只可能来自手工改库：跳过并记 `warn`，不影响站点其余规则。
        let (shield, rule_errors) = ShieldRules::compile(&row.shield_rules);
        for e in &rule_errors {
            tracing::warn!(site_id = row.id, rule_id = %e.rule_id, reason = %e.reason, "屏蔽规则无效，已跳过");
        }
        Ok(SiteConfig {
            site_id: row.id,
            timezone,
            collect_enabled: row.collect_enabled,
            hostnames: HostnamePatterns::new(row.hostnames.iter().map(String::as_str)),
            geo_level,
            tracker_hash: row.tracker_hash,
            shield,
            ingest_rate_limit_per_min: row.ingest_rate_limit_per_min,
        })
    }
}

/// 全量重载的周期（增量刷新之外的兜底，§4.3）。
pub const FULL_RELOAD_EVERY: Duration = Duration::from_secs(5 * 60);
/// 收到 NOTIFY 之后等一小会儿再刷新，把同一事务或相邻事务的多条通知合成一次。
const NOTIFY_DEBOUNCE: Duration = Duration::from_millis(200);

/// 一次刷新做了什么（日志与测试用）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refresh {
    /// 全量重载，站点数。
    Full(usize),
    /// 增量刷新：重新加载了 `reloaded` 个站点，摘除了 `removed` 个。
    Incremental { reloaded: usize, removed: usize },
}

/// 增量刷新的状态：水位（PG 时间，微秒）与上次全量重载的时刻。
struct RefreshState {
    watermark_us: Option<i64>,
    last_full: Option<Instant>,
}

/// `site_key -> SiteConfig`。原子替换，读取无锁。
pub struct SiteSnapshot {
    map: ArcSwap<HashMap<String, Arc<SiteConfig>>>,
    loaded: AtomicBool,
    /// 串行化刷新（定时与 NOTIFY 两个来源）。
    state: tokio::sync::Mutex<RefreshState>,
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
            state: tokio::sync::Mutex::new(RefreshState {
                watermark_us: None,
                last_full: None,
            }),
        }
    }

    pub fn get(&self, site_key: &str) -> Option<Arc<SiteConfig>> {
        self.map.load().get(site_key).cloned()
    }

    pub fn len(&self) -> usize {
        self.map.load().len()
    }

    /// 快照里全部站点的 id。
    pub fn site_ids(&self) -> HashSet<u64> {
        self.map.load().values().map(|c| c.site_id).collect()
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

    /// 把增量结果并进当前快照：先摘除 `changed` 里的全部站点，再放回仍未删除的 `rows`。
    /// `changed` 不在 `rows` 里的站点就是被删除了。返回 `(重新加载数, 摘除数)`。
    pub fn apply_changes(&self, changed: &[u64], rows: &[SiteRow]) -> (usize, usize) {
        let changed: HashSet<u64> = changed.iter().copied().collect();
        let mut map = HashMap::clone(&self.map.load());
        let mut dropped = HashSet::new();
        map.retain(|_, cfg| {
            let keep = !changed.contains(&cfg.site_id);
            if !keep {
                dropped.insert(cfg.site_id);
            }
            keep
        });
        let fresh = Self::build(rows);
        for cfg in fresh.values() {
            dropped.remove(&cfg.site_id);
        }
        let reloaded = fresh.len();
        map.extend(fresh);
        self.map.store(Arc::new(map));
        (reloaded, dropped.len())
    }

    /// 从 PG 全量重载一次，返回站点数。之后的增量刷新以重载前读到的 PG 时间为水位。
    pub async fn reload(&self, pool: &PgPool) -> Result<usize> {
        let mut state = self.state.lock().await;
        self.reload_locked(pool, &mut state).await
    }

    async fn reload_locked(&self, pool: &PgPool, state: &mut RefreshState) -> Result<usize> {
        let now_us = rivu_storage_pg::sites::db_now_us(pool).await?;
        let rows = rivu_storage_pg::sites::load_sites(pool).await?;
        let map = Self::build(&rows);
        let n = map.len();
        self.replace(map);
        state.watermark_us = Some(now_us);
        state.last_full = Some(Instant::now());
        Ok(n)
    }

    /// 刷新一次：还没有水位或距上次全量重载超过 [`FULL_RELOAD_EVERY`] 时全量重载，否则增量刷新。
    pub async fn refresh(&self, pool: &PgPool) -> Result<Refresh> {
        let mut state = self.state.lock().await;
        let full_due = state
            .last_full
            .is_none_or(|t| t.elapsed() >= FULL_RELOAD_EVERY);
        let Some(since) = state.watermark_us.filter(|_| !full_due) else {
            return self
                .reload_locked(pool, &mut state)
                .await
                .map(Refresh::Full);
        };
        let changes = rivu_storage_pg::sites::changed_sites(pool, since).await?;
        let (reloaded, removed) = if changes.site_ids.is_empty() {
            (0, 0)
        } else {
            let rows = rivu_storage_pg::sites::load_sites_by_ids(pool, &changes.site_ids).await?;
            self.apply_changes(&changes.site_ids, &rows)
        };
        state.watermark_us = Some(changes.now_us);
        Ok(Refresh::Incremental { reloaded, removed })
    }

    async fn refresh_logged(&self, pool: &PgPool, trigger: &'static str) {
        match self.refresh(pool).await {
            Ok(Refresh::Full(n)) => tracing::debug!(sites = n, trigger, "站点快照已全量重载"),
            Ok(Refresh::Incremental { reloaded, removed }) => {
                if reloaded + removed > 0 {
                    tracing::debug!(reloaded, removed, trigger, "站点快照已增量刷新");
                }
            }
            Err(e) => {
                tracing::warn!(error = %format!("{e:#}"), trigger, "刷新站点快照失败，继续使用旧快照")
            }
        }
    }

    /// 启动后台刷新：立即刷新一次，之后每 `every` 一次（增量为主，定期全量）；
    /// 同时 `LISTEN rivu_site_changed`，收到通知后立即增量刷新。失败只记日志，继续用旧快照。
    pub fn spawn_refresh(
        self: &Arc<Self>,
        pool: PgPool,
        every: Duration,
        stop: watch::Receiver<bool>,
    ) -> JoinHandle<()> {
        let listener = {
            let this = Arc::clone(self);
            let pool = pool.clone();
            let stop = stop.clone();
            tokio::spawn(async move { this.listen(pool, stop).await })
        };
        let this = Arc::clone(self);
        let periodic = spawn_periodic(every, stop, move || {
            let this = Arc::clone(&this);
            let pool = pool.clone();
            async move { this.refresh_logged(&pool, "timer").await }
        });
        tokio::spawn(async move {
            let _ = periodic.await;
            let _ = listener.await;
        })
    }

    /// 监听变更通知。连接断开时 sqlx 自动重连（断开期间的通知丢失，由定时刷新兜底）；
    /// 建立监听失败时 30 秒后重试。
    async fn listen(self: Arc<Self>, pool: PgPool, mut stop: watch::Receiver<bool>) {
        use sqlx::postgres::PgListener;
        loop {
            let mut listener = match PgListener::connect_with(&pool).await {
                Ok(mut l) => match l.listen(rivu_storage_pg::sites::SITE_CHANGED_CHANNEL).await {
                    Ok(()) => l,
                    Err(e) => {
                        tracing::warn!(error = %e, "LISTEN 站点变更失败，30 秒后重试");
                        if wait_or_stop(&mut stop, Duration::from_secs(30)).await {
                            return;
                        }
                        continue;
                    }
                },
                Err(e) => {
                    tracing::warn!(error = %e, "连接 PG 监听站点变更失败，30 秒后重试");
                    if wait_or_stop(&mut stop, Duration::from_secs(30)).await {
                        return;
                    }
                    continue;
                }
            };
            loop {
                tokio::select! {
                    () = stopped(&mut stop) => return,
                    n = listener.recv() => match n {
                        Ok(_) => {
                            // 合并紧挨着的通知：先等一小会儿，再把已经到达的都取走
                            if wait_or_stop(&mut stop, NOTIFY_DEBOUNCE).await {
                                return;
                            }
                            while listener.next_buffered().is_some() {}
                            self.refresh_logged(&pool, "notify").await;
                        }
                        Err(e) => {
                            tracing::warn!(error = %e, "接收站点变更通知失败，重新建立监听");
                            break;
                        }
                    },
                }
            }
        }
    }
}

/// 等 `d`；期间收到停止信号返回 true。
async fn wait_or_stop(stop: &mut watch::Receiver<bool>, d: Duration) -> bool {
    tokio::select! {
        () = stopped(stop) => true,
        () = tokio::time::sleep(d) => false,
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
            ingest_rate_limit_per_min: None,
            shield_rules: Vec::new(),
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
    fn apply_changes_replaces_changed_sites_and_drops_deleted_ones() {
        let s = SiteSnapshot::new();
        s.replace(SiteSnapshot::build(&[
            row(1, "aaaaaaaaaaaa", "UTC", &["a.test"]),
            row(2, "bbbbbbbbbbbb", "UTC", &["b.test"]),
            row(3, "cccccccccccc", "UTC", &["c.test"]),
        ]));
        // 1 改了白名单，2 被删除，4 是新站点，3 没变
        let (reloaded, removed) = s.apply_changes(
            &[1, 2, 4],
            &[
                row(1, "aaaaaaaaaaaa", "UTC", &["a2.test"]),
                row(4, "dddddddddddd", "UTC", &["d.test"]),
            ],
        );
        assert_eq!((reloaded, removed), (2, 1));
        assert!(s.get("aaaaaaaaaaaa").unwrap().hostnames.matches("a2.test"));
        assert!(!s.get("aaaaaaaaaaaa").unwrap().hostnames.matches("a.test"));
        assert!(s.get("bbbbbbbbbbbb").is_none());
        assert_eq!(s.get("cccccccccccc").unwrap().site_id, 3);
        assert_eq!(s.get("dddddddddddd").unwrap().site_id, 4);
        assert_eq!(s.len(), 3);
        // 变更的站点时区变坏：同样被摘除（build 跳过它）
        let (reloaded, removed) =
            s.apply_changes(&[3], &[row(3, "cccccccccccc", "Mars/Olympus", &[])]);
        assert_eq!((reloaded, removed), (0, 1));
        assert!(s.get("cccccccccccc").is_none());
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
