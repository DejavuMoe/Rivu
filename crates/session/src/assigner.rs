//! SessionAssigner（`system-design.md` §4.6）：给事件分配 `session_id`，并把会话级字段盖到事件上。
//!
//! - 状态按 `routing % 32` 分成 32 个分片，每片一把 `parking_lot::Mutex`；
//!   routing 哈希不随盐轮换，所以同一访客的所有 key（原始、别名、前一天）总在同一个分片（§4.5、Q15）。
//! - 临界区只有查找、更新、盖章，没有 IO，也没有 `.await`。
//! - 索引 key 是 `(site_id, visitor_id)`，一个会话最多有 [`MAX_KEYS`] 个 key（原始 + 别名）。
//! - 过期按「分片内事件时间水位 − timeout」清扫，不用墙钟（实时处理与积压重放都正确）。

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use parking_lot::Mutex;
use rivu_domain::{EventKind, SessionAttrs};

/// 分片数，等于 Reliable 的分区数 P（§10 Q8、Q15）。`routing % SHARDS` 同时是 [`Stamp::partition`]。
pub const SHARDS: usize = 32;

/// 一个会话最多持有的 visitor_id 个数：7 天最多跨 7 个本地午夜，加上原始 ID 共 8 个（§4.6「别名 key」）。
pub const MAX_KEYS: usize = 8;

/// SessionAssigner 的参数。
#[derive(Clone, Debug)]
pub struct SessionConfig {
    /// 不活跃超时（毫秒），默认 30 分钟。
    pub timeout_ms: i64,
    /// 会话最长时长（毫秒），默认 7 天（§10 Q7）。
    pub max_duration_ms: i64,
    /// 同时保留的会话数上限，默认 200 万；超出后按 LRU 淘汰整个会话。
    /// 上限平均分给各分片（每片 `ceil(max_sessions / 32)`，至少 1），所以是近似的全局上限。
    pub max_sessions: usize,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            timeout_ms: 30 * 60 * 1000,
            max_duration_ms: 7 * 24 * 60 * 60 * 1000,
            max_sessions: 2_000_000,
        }
    }
}

/// 一个待分配会话的事件，由身份层和 HTTP 层算好。
#[derive(Clone, Debug)]
pub struct Incoming<'a> {
    pub site_id: u64,
    /// 用事件的站点本地日期的盐算出。
    pub visitor_id: u64,
    /// 用前一个本地日期的盐算出；那份盐不存在时为 `None`。
    pub prev_visitor_id: Option<u64>,
    /// routing 哈希（不随盐轮换）。
    pub routing: u64,
    pub ts_ms: i64,
    pub kind: EventKind,
    pub pathname: &'a str,
    /// 只在开新会话时克隆。
    pub attrs: &'a SessionAttrs,
}

/// 分配结果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Assignment {
    Stamped(Stamp),
    /// engagement 事件没有（可续的）会话：丢弃，计入 `no_session`（§4.6「engagement」）。
    NoSession,
}

/// 盖到事件上的会话级字段。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stamp {
    /// 现行 §4.6：会话的原始 ID（`keys[0]`）。Q9 定案后可能改用 [`Stamp::own_visitor_id`]。
    pub visitor_id: u64,
    /// 事件自己的 `visitor_id`（用事件本地日期的盐算出）。
    pub own_visitor_id: u64,
    pub session_id: u64,
    pub session_start_ms: i64,
    /// 会话第一个 pageview 的 pathname；在它之前为空串。
    pub entry_pathname: Arc<str>,
    pub attrs: Arc<SessionAttrs>,
    /// `routing % 32`，写入 `ingest_partition`（Q15）。
    pub partition: u8,
}

/// 累计计数。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AssignerStats {
    /// 开出的新会话数。
    pub sessions_started: u64,
    /// 没有可续会话的 engagement 数。
    pub no_session: u64,
    /// 别名 key 已满 8 个、没能再加的次数（`session_alias_cap_hit`）。
    pub alias_cap_hit: u64,
    /// 因 `max_sessions` 被 LRU 淘汰的会话数。
    pub evicted: u64,
    /// 被 [`SessionAssigner::sweep`] 清扫的会话数。
    pub expired: u64,
}

/// 会话的只读视图，给测试和诊断用（[`SessionAssigner::peek`]）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SessionView {
    pub session_id: u64,
    pub started_at_ms: i64,
    pub last_seen_ms: i64,
    pub key_count: usize,
}

struct Session {
    site_id: u64,
    id: u64,
    /// 第一个是会话的原始 ID，其余是别名。
    keys: Vec<u64>,
    started_at: i64,
    /// 取 max，乱序事件不会让它倒退。
    last_seen: i64,
    entry: Arc<str>,
    has_entry: bool,
    attrs: Arc<SessionAttrs>,
    /// LRU 序号（`Shard::lru` 的 key）。
    tick: u64,
}

impl Session {
    /// 事件能否并入这个会话：`ts - last_seen ≤ timeout` 且 `ts - started_at ≤ max_duration`。
    fn accepts(&self, ts_ms: i64, cfg: &SessionConfig) -> bool {
        ts_ms.saturating_sub(self.last_seen) <= cfg.timeout_ms
            && ts_ms.saturating_sub(self.started_at) <= cfg.max_duration_ms
    }
}

struct Shard {
    sessions: HashMap<u64, Session>,
    /// `(site_id, visitor_id)` → `session_id`。
    index: HashMap<(u64, u64), u64>,
    /// LRU 顺序：序号 → `session_id`，最小的最久未用。
    lru: BTreeMap<u64, u64>,
    next_tick: u64,
    /// 分片内见过的最大事件时间。
    watermark: i64,
    rng: u64,
    stats: AssignerStats,
}

/// splitmix64：给 `session_id` 用的小型确定性随机数发生器。
fn splitmix64(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

impl Shard {
    fn new(seed: u64) -> Self {
        Self {
            sessions: HashMap::new(),
            index: HashMap::new(),
            lru: BTreeMap::new(),
            next_tick: 0,
            watermark: i64::MIN,
            rng: seed,
            stats: AssignerStats::default(),
        }
    }

    fn next_session_id(&mut self) -> u64 {
        loop {
            let id = splitmix64(&mut self.rng);
            if id != 0 && !self.sessions.contains_key(&id) {
                return id;
            }
        }
    }

    /// 先按 `visitor_id` 查，查不到再按 `prev_visitor_id` 查。返回 `(session_id, 是否经由 prev 命中)`。
    fn lookup(&self, ev: &Incoming<'_>) -> Option<(u64, bool)> {
        if let Some(&sid) = self.index.get(&(ev.site_id, ev.visitor_id)) {
            return Some((sid, false));
        }
        let prev = ev.prev_visitor_id?;
        self.index.get(&(ev.site_id, prev)).map(|&sid| (sid, true))
    }

    /// 移除一个会话及其仍指向它的全部索引项。
    fn remove_session(&mut self, sid: u64) -> bool {
        let Some(s) = self.sessions.remove(&sid) else {
            return false;
        };
        self.lru.remove(&s.tick);
        for k in &s.keys {
            // 只删仍指向本会话的项：key 若已被新会话占用，就不动它。
            if self.index.get(&(s.site_id, *k)) == Some(&sid) {
                self.index.remove(&(s.site_id, *k));
            }
        }
        true
    }

    fn open(&mut self, cap: usize, ev: &Incoming<'_>, empty: &Arc<str>) -> u64 {
        let id = self.next_session_id();
        self.next_tick += 1;
        let tick = self.next_tick;
        self.sessions.insert(
            id,
            Session {
                site_id: ev.site_id,
                id,
                keys: vec![ev.visitor_id],
                started_at: ev.ts_ms,
                last_seen: ev.ts_ms,
                entry: Arc::clone(empty),
                has_entry: false,
                attrs: Arc::new(ev.attrs.clone()),
                tick,
            },
        );
        // 若该 key 还指向一个已过期、尚未清扫的旧会话，直接占用；旧会话清扫时不会误删它。
        self.index.insert((ev.site_id, ev.visitor_id), id);
        self.lru.insert(tick, id);
        self.stats.sessions_started += 1;
        while self.sessions.len() > cap {
            let Some((_, &oldest)) = self.lru.first_key_value() else {
                break;
            };
            self.remove_session(oldest);
            self.stats.evicted += 1;
        }
        id
    }

    fn assign(
        &mut self,
        cfg: &SessionConfig,
        cap: usize,
        empty: &Arc<str>,
        ev: &Incoming<'_>,
        partition: u8,
    ) -> Assignment {
        self.watermark = self.watermark.max(ev.ts_ms);

        let live = self.lookup(ev).filter(|(sid, _)| {
            self.sessions
                .get(sid)
                .is_some_and(|s| s.accepts(ev.ts_ms, cfg))
        });
        let (sid, via_prev) = match live {
            Some(hit) => hit,
            None => {
                // 没有可续的会话（没找到，或找到了但已超时 / 超过 7 天）。
                if ev.kind == EventKind::Engagement {
                    self.stats.no_session += 1;
                    return Assignment::NoSession;
                }
                (self.open(cap, ev, empty), false)
            }
        };

        let Some(s) = self.sessions.get_mut(&sid) else {
            // 刚查到或刚插入，不会缺；保守地当作没有会话。
            self.stats.no_session += 1;
            return Assignment::NoSession;
        };
        s.last_seen = s.last_seen.max(ev.ts_ms);
        if ev.kind == EventKind::Pageview && !s.has_entry {
            s.entry = Arc::from(ev.pathname);
            s.has_entry = true;
        }
        let mut new_alias = false;
        if via_prev && !s.keys.contains(&ev.visitor_id) {
            if s.keys.len() < MAX_KEYS {
                s.keys.push(ev.visitor_id);
                new_alias = true;
            } else {
                self.stats.alias_cap_hit += 1;
            }
        }
        self.next_tick += 1;
        let old_tick = std::mem::replace(&mut s.tick, self.next_tick);
        let stamp = Stamp {
            visitor_id: s.keys[0],
            own_visitor_id: ev.visitor_id,
            session_id: s.id,
            session_start_ms: s.started_at,
            entry_pathname: Arc::clone(&s.entry),
            attrs: Arc::clone(&s.attrs),
            partition,
        };
        if new_alias {
            self.index.insert((ev.site_id, ev.visitor_id), sid);
        }
        self.lru.remove(&old_tick);
        self.lru.insert(self.next_tick, sid);
        Assignment::Stamped(stamp)
    }

    fn sweep(&mut self, timeout_ms: i64) -> usize {
        if self.watermark == i64::MIN {
            return 0;
        }
        let horizon = self.watermark.saturating_sub(timeout_ms);
        let expired: Vec<u64> = self
            .sessions
            .values()
            .filter(|s| s.last_seen < horizon)
            .map(|s| s.id)
            .collect();
        let mut removed = 0;
        for sid in expired {
            if self.remove_session(sid) {
                removed += 1;
            }
        }
        self.stats.expired += removed as u64;
        removed
    }
}

/// 把事件按 §4.6 的规则分配到会话。`&self` 可被多线程并发调用。
pub struct SessionAssigner {
    cfg: SessionConfig,
    shard_cap: usize,
    shards: Vec<Mutex<Shard>>,
    empty: Arc<str>,
}

impl SessionAssigner {
    /// `session_id` 用随机 u64（非 0）。
    pub fn new(cfg: SessionConfig) -> Self {
        Self::with_seed(cfg, rand::random())
    }

    /// 确定性版本：同样的种子与同样的调用序列得到同样的 `session_id`（测试、重放用）。
    pub fn with_seed(cfg: SessionConfig, seed: u64) -> Self {
        let shard_cap = cfg.max_sessions.div_ceil(SHARDS).max(1);
        let mut state = seed;
        let shards = (0..SHARDS)
            .map(|_| Mutex::new(Shard::new(splitmix64(&mut state))))
            .collect();
        Self {
            cfg,
            shard_cap,
            shards,
            empty: Arc::from(""),
        }
    }

    /// 查找或开会话、续期、盖章。
    pub fn assign(&self, ev: &Incoming<'_>) -> Assignment {
        let shard = (ev.routing % SHARDS as u64) as usize;
        self.shards[shard]
            .lock()
            .assign(&self.cfg, self.shard_cap, &self.empty, ev, shard as u8)
    }

    /// 按各分片「事件时间水位 − timeout」清扫过期会话，返回移除的会话数。
    /// 一次移除会话的全部 key，且只删除仍指向该会话的索引项。
    pub fn sweep(&self) -> usize {
        self.shards
            .iter()
            .map(|s| s.lock().sweep(self.cfg.timeout_ms))
            .sum()
    }

    /// 当前保留的会话数（含已超时但尚未清扫的）。
    pub fn len(&self) -> usize {
        self.shards.iter().map(|s| s.lock().sessions.len()).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn stats(&self) -> AssignerStats {
        let mut total = AssignerStats::default();
        for s in &self.shards {
            let st = s.lock().stats;
            total.sessions_started += st.sessions_started;
            total.no_session += st.no_session;
            total.alias_cap_hit += st.alias_cap_hit;
            total.evicted += st.evicted;
            total.expired += st.expired;
        }
        total
    }

    /// 按索引 key 查看会话的当前状态，不改变任何状态。`routing` 用来定位分片。
    pub fn peek(&self, site_id: u64, routing: u64, visitor_id: u64) -> Option<SessionView> {
        let shard = self.shards[(routing % SHARDS as u64) as usize].lock();
        let sid = shard.index.get(&(site_id, visitor_id))?;
        let s = shard.sessions.get(sid)?;
        Some(SessionView {
            session_id: s.id,
            started_at_ms: s.started_at,
            last_seen_ms: s.last_seen,
            key_count: s.keys.len(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIN: i64 = 60_000;
    const DAY: i64 = 24 * 60 * MIN;
    const SITE: u64 = 1;
    const ROUTING: u64 = 5;

    fn attrs() -> &'static SessionAttrs {
        static A: std::sync::OnceLock<SessionAttrs> = std::sync::OnceLock::new();
        A.get_or_init(SessionAttrs::default)
    }

    fn ev(visitor: u64, prev: Option<u64>, ts: i64, kind: EventKind, path: &str) -> Incoming<'_> {
        Incoming {
            site_id: SITE,
            visitor_id: visitor,
            prev_visitor_id: prev,
            routing: ROUTING,
            ts_ms: ts,
            kind,
            pathname: path,
            attrs: attrs(),
        }
    }

    fn stamped(a: Assignment) -> Stamp {
        match a {
            Assignment::Stamped(s) => s,
            Assignment::NoSession => panic!("期望 Stamped，得到 NoSession"),
        }
    }

    fn assigner() -> SessionAssigner {
        SessionAssigner::with_seed(SessionConfig::default(), 42)
    }

    #[test]
    fn renewal_boundary_is_inclusive_30_minutes() {
        let a = assigner();
        let s0 = stamped(a.assign(&ev(10, None, 0, EventKind::Pageview, "/a")));
        let s1 = stamped(a.assign(&ev(10, None, 30 * MIN, EventKind::Pageview, "/b")));
        assert_eq!(s0.session_id, s1.session_id);
        let s2 = stamped(a.assign(&ev(10, None, 60 * MIN + 1, EventKind::Pageview, "/c")));
        assert_ne!(s1.session_id, s2.session_id);
        assert_eq!(&*s2.entry_pathname, "/c");
        assert_eq!(s2.session_start_ms, 60 * MIN + 1);
    }

    #[test]
    fn seven_day_cap_starts_new_session() {
        // 超时放宽到 1 天，每天一个事件，才能触达 7 天上限。
        let cfg = SessionConfig {
            timeout_ms: DAY,
            ..SessionConfig::default()
        };
        let a = SessionAssigner::with_seed(cfg, 1);
        let first = stamped(a.assign(&ev(10, None, 0, EventKind::Pageview, "/")));
        let mut last = first.clone();
        for d in 1..=7 {
            last = stamped(a.assign(&ev(10, None, d * DAY, EventKind::Pageview, "/")));
            assert_eq!(last.session_id, first.session_id, "第 {d} 天仍在同一会话");
        }
        // 恰好 7 天并入；多 1ms 开新会话（尽管距 last_seen 只有 1ms）。
        let over = stamped(a.assign(&ev(10, None, 7 * DAY + 1, EventKind::Pageview, "/")));
        assert_ne!(over.session_id, last.session_id);
        assert_eq!(over.session_start_ms, 7 * DAY + 1);
        // 新会话之后的事件续在新会话上。
        let next = stamped(a.assign(&ev(10, None, 7 * DAY + 2, EventKind::Pageview, "/")));
        assert_eq!(next.session_id, over.session_id);
    }

    #[test]
    fn alias_keys_capped_at_eight_and_counted() {
        // 每天换一个 visitor_id（每个事件都带前一天的 ID），超时放宽到 2 天，上限放宽到 100 天。
        let cfg = SessionConfig {
            timeout_ms: 2 * DAY,
            max_duration_ms: 100 * DAY,
            ..SessionConfig::default()
        };
        let a = SessionAssigner::with_seed(cfg, 1);
        let day = |d: i64| {
            let prev = (d > 0).then(|| 99 + d as u64);
            ev(100 + d as u64, prev, d * DAY, EventKind::Pageview, "/")
        };
        let first = stamped(a.assign(&day(0)));
        for d in 1..8 {
            let s = stamped(a.assign(&day(d)));
            assert_eq!(s.session_id, first.session_id);
            assert_eq!(s.visitor_id, 100, "盖章用会话的原始 ID");
            assert_eq!(s.own_visitor_id, 100 + d as u64);
        }
        let view = a.peek(SITE, ROUTING, 107).unwrap();
        assert_eq!(view.key_count, MAX_KEYS);
        assert_eq!(a.stats().alias_cap_hit, 0);

        // 第 9 天：经由 prev(107) 命中，续期照常，但别名加不进去。
        let s8 = stamped(a.assign(&day(8)));
        assert_eq!(s8.session_id, first.session_id);
        assert_eq!(a.stats().alias_cap_hit, 1);
        assert!(
            a.peek(SITE, ROUTING, 108).is_none(),
            "第 9 个 key 没有建立索引"
        );
        // 同一天再来一个事件，仍然经由 prev 命中，再计一次。
        let s8b = stamped(a.assign(&Incoming {
            ts_ms: 8 * DAY + MIN,
            ..day(8)
        }));
        assert_eq!(s8b.session_id, first.session_id);
        assert_eq!(a.stats().alias_cap_hit, 2);
        assert_eq!(a.peek(SITE, ROUTING, 107).unwrap().key_count, MAX_KEYS);
    }

    #[test]
    fn prev_visitor_id_continues_session_and_adds_alias() {
        let a = assigner();
        let s0 = stamped(a.assign(&ev(10, Some(9), 0, EventKind::Pageview, "/a")));
        // 跨午夜：新 ID 11，前一天的 ID 是 10。
        let s1 = stamped(a.assign(&ev(11, Some(10), 5 * MIN, EventKind::Pageview, "/b")));
        assert_eq!(s1.session_id, s0.session_id);
        assert_eq!((s1.visitor_id, s1.own_visitor_id), (10, 11));
        assert_eq!(a.peek(SITE, ROUTING, 11).unwrap().key_count, 2);
        // 乱序的前一天事件仍能查到会话。
        let s2 = stamped(a.assign(&ev(10, Some(9), 4 * MIN, EventKind::Pageview, "/c")));
        assert_eq!(s2.session_id, s0.session_id);
        assert_eq!(a.stats().sessions_started, 1);
    }

    #[test]
    fn different_sites_do_not_share_sessions() {
        let a = assigner();
        let s0 = stamped(a.assign(&ev(10, None, 0, EventKind::Pageview, "/")));
        let s1 = stamped(a.assign(&Incoming {
            site_id: 2,
            ..ev(10, None, 1, EventKind::Pageview, "/")
        }));
        assert_ne!(s0.session_id, s1.session_id);
    }

    #[test]
    fn last_seen_never_regresses_on_out_of_order_events() {
        let a = assigner();
        stamped(a.assign(&ev(10, None, 10 * MIN, EventKind::Pageview, "/a")));
        let s = stamped(a.assign(&ev(10, None, 5 * MIN, EventKind::Pageview, "/b")));
        assert_eq!(s.session_start_ms, 10 * MIN, "迟到事件不改 session_start");
        assert_eq!(a.peek(SITE, ROUTING, 10).unwrap().last_seen_ms, 10 * MIN);
        stamped(a.assign(&ev(10, None, 20 * MIN, EventKind::Pageview, "/c")));
        stamped(a.assign(&ev(10, None, 12 * MIN, EventKind::Pageview, "/d")));
        assert_eq!(a.peek(SITE, ROUTING, 10).unwrap().last_seen_ms, 20 * MIN);
    }

    #[test]
    fn entry_is_empty_until_first_pageview() {
        let a = assigner();
        // 自定义事件可以开会话，此时还没有 pageview，entry 为空。
        let c = stamped(a.assign(&ev(10, None, 0, EventKind::Custom, "/signup")));
        assert_eq!(&*c.entry_pathname, "");
        let e = stamped(a.assign(&ev(10, None, MIN, EventKind::Engagement, "/signup")));
        assert_eq!(&*e.entry_pathname, "");
        let p = stamped(a.assign(&ev(10, None, 2 * MIN, EventKind::Pageview, "/first")));
        assert_eq!(&*p.entry_pathname, "/first");
        let p2 = stamped(a.assign(&ev(10, None, 3 * MIN, EventKind::Pageview, "/second")));
        assert_eq!(
            &*p2.entry_pathname, "/first",
            "入口页只由第一个 pageview 决定"
        );
        let c2 = stamped(a.assign(&ev(10, None, 4 * MIN, EventKind::Custom, "/other")));
        assert_eq!(&*c2.entry_pathname, "/first");
        assert_eq!(c.session_id, c2.session_id);
    }

    #[test]
    fn engagement_without_session_is_dropped_and_with_session_extends() {
        let a = assigner();
        assert_eq!(
            a.assign(&ev(10, None, 0, EventKind::Engagement, "/")),
            Assignment::NoSession
        );
        assert!(a.is_empty(), "engagement 不开会话");
        assert_eq!(a.stats().no_session, 1);

        let p = stamped(a.assign(&ev(10, None, MIN, EventKind::Pageview, "/a")));
        // engagement 把 last_seen 推到 25min，使 54min 的 pageview 仍在超时内。
        let e = stamped(a.assign(&ev(10, None, 25 * MIN, EventKind::Engagement, "/a")));
        assert_eq!(e.session_id, p.session_id);
        assert_eq!(&*e.entry_pathname, "/a");
        let p2 = stamped(a.assign(&ev(10, None, 54 * MIN, EventKind::Pageview, "/b")));
        assert_eq!(p2.session_id, p.session_id);
        // 超时之后的 engagement 不能复活会话，也不开新会话。
        assert_eq!(
            a.assign(&ev(10, None, 200 * MIN, EventKind::Engagement, "/b")),
            Assignment::NoSession
        );
        assert_eq!(a.stats().no_session, 2);
        assert_eq!(a.stats().sessions_started, 1);
    }

    #[test]
    fn stamp_carries_attrs_of_first_event_and_partition() {
        let a = assigner();
        let first_attrs = SessionAttrs {
            browser: "Firefox".into(),
            ..SessionAttrs::default()
        };
        let later_attrs = SessionAttrs {
            browser: "Chrome".into(),
            ..SessionAttrs::default()
        };
        let mut e0 = ev(10, None, 0, EventKind::Pageview, "/");
        e0.attrs = &first_attrs;
        e0.routing = 32 * 7 + 19;
        let s0 = stamped(a.assign(&e0));
        let mut e1 = ev(10, None, MIN, EventKind::Pageview, "/");
        e1.attrs = &later_attrs;
        e1.routing = 32 * 7 + 19;
        let s1 = stamped(a.assign(&e1));
        assert_eq!(s0.attrs.browser, "Firefox");
        assert_eq!(s1.attrs.browser, "Firefox", "会话属性由首个事件决定");
        assert!(Arc::ptr_eq(&s0.attrs, &s1.attrs));
        assert_eq!((s0.partition, s1.partition), (19, 19));
    }

    #[test]
    fn sweep_only_removes_index_entries_still_pointing_to_the_session() {
        let a = assigner();
        // S1 有两个 key：10（原始）与 11（别名，经由 prev 加入）。
        let s1 = stamped(a.assign(&ev(10, None, 0, EventKind::Pageview, "/")));
        stamped(a.assign(&ev(11, Some(10), MIN, EventKind::Pageview, "/")));
        assert_eq!(a.peek(SITE, ROUTING, 11).unwrap().session_id, s1.session_id);
        // 3 小时后，key 11 又来了（没带 prev）：S1 已超时，开 S2，占用 key 11。
        let s2 = stamped(a.assign(&ev(11, None, 3 * 60 * MIN, EventKind::Pageview, "/")));
        assert_ne!(s2.session_id, s1.session_id);
        assert_eq!(a.len(), 2, "S1 已超时但尚未清扫");
        assert_eq!(a.peek(SITE, ROUTING, 10).unwrap().session_id, s1.session_id);

        // 清扫：S1 过期；key 10 随它删除，key 11 已属于 S2，必须保留。
        assert_eq!(a.sweep(), 1);
        assert_eq!(a.len(), 1);
        assert!(a.peek(SITE, ROUTING, 10).is_none());
        assert_eq!(a.peek(SITE, ROUTING, 11).unwrap().session_id, s2.session_id);
        assert_eq!(a.stats().expired, 1);
        // S2 仍可续期。
        let s3 = stamped(a.assign(&ev(11, None, 3 * 60 * MIN + 1, EventKind::Pageview, "/")));
        assert_eq!(s3.session_id, s2.session_id);
        // 再清扫一次：没有东西可删。
        assert_eq!(a.sweep(), 0);
    }

    #[test]
    fn sweep_uses_event_time_watermark_not_wall_clock() {
        let a = assigner();
        stamped(a.assign(&ev(10, None, 1_000_000_000, EventKind::Pageview, "/")));
        // 水位没有前进：不清扫，无论墙钟过了多久。
        assert_eq!(a.sweep(), 0);
        // 同一分片里另一个访客把水位推进到 31 分钟之后。
        stamped(a.assign(&ev(
            20,
            None,
            1_000_000_000 + 31 * MIN,
            EventKind::Pageview,
            "/",
        )));
        assert_eq!(a.sweep(), 1);
        assert!(a.peek(SITE, ROUTING, 10).is_none());
        assert!(a.peek(SITE, ROUTING, 20).is_some());
    }

    #[test]
    fn lru_evicts_least_recently_used_whole_session() {
        // 每片上限 = ceil(64 / 32) = 2。
        let cfg = SessionConfig {
            max_sessions: 64,
            ..SessionConfig::default()
        };
        let a = SessionAssigner::with_seed(cfg, 7);
        stamped(a.assign(&ev(1, None, 0, EventKind::Pageview, "/")));
        // B 有别名 key 22。
        stamped(a.assign(&ev(2, Some(99), 1, EventKind::Pageview, "/")));
        stamped(a.assign(&ev(22, Some(2), 2, EventKind::Pageview, "/")));
        // 再碰 A，让 B 成为最久未用。
        stamped(a.assign(&ev(1, None, 3, EventKind::Pageview, "/")));
        assert_eq!(a.len(), 2);
        // C 进来：淘汰 B（含它的别名 key）。
        stamped(a.assign(&ev(3, None, 4, EventKind::Pageview, "/")));
        assert_eq!(a.len(), 2);
        assert_eq!(a.stats().evicted, 1);
        assert!(a.peek(SITE, ROUTING, 1).is_some());
        assert!(a.peek(SITE, ROUTING, 2).is_none());
        assert!(
            a.peek(SITE, ROUTING, 22).is_none(),
            "淘汰整个会话，含别名 key"
        );
        assert!(a.peek(SITE, ROUTING, 3).is_some());
    }

    #[test]
    fn seeded_assigners_replay_identically_and_ids_are_nonzero() {
        let run = |seed| {
            let a = SessionAssigner::with_seed(SessionConfig::default(), seed);
            (0..200u64)
                .map(|i| {
                    stamped(a.assign(&ev(i, None, i as i64, EventKind::Pageview, "/"))).session_id
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(run(9), run(9));
        assert_ne!(run(9), run(10));
        assert!(run(9).iter().all(|&id| id != 0));
        // `new` 不 panic、id 非 0。
        let a = SessionAssigner::new(SessionConfig::default());
        assert_ne!(
            stamped(a.assign(&ev(1, None, 0, EventKind::Pageview, "/"))).session_id,
            0
        );
    }

    #[test]
    fn concurrent_assign_is_safe() {
        let a = Arc::new(assigner());
        let handles: Vec<_> = (0..4u64)
            .map(|t| {
                let a = Arc::clone(&a);
                std::thread::spawn(move || {
                    for i in 0..1_000u64 {
                        let id = t * 10_000 + i % 50;
                        let e = Incoming {
                            routing: id,
                            ..ev(id, None, i as i64, EventKind::Pageview, "/")
                        };
                        assert!(matches!(a.assign(&e), Assignment::Stamped(_)));
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(a.len(), 200);
    }
}
