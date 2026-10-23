//! 接入管线（Lite）：`POST /api/event` 的请求体到「已盖章的事件行」，只含内存里的查找和哈希，没有 IO。
//!
//! 顺序（`system-design.md` §4.2，先做便宜的判断，昂贵的 UA 解析和地理查询放在规则之后）：
//!
//! | # | 阶段 | 丢弃原因 |
//! |---|---|---|
//! | 1 | 解析（400，不进 `ingest_counters`） | — |
//! | 2 | 客户端 IP | — |
//! | 3 | 站点查找、`collect_enabled`、hostname 白名单（空白名单 fail-closed） | `unknown_site` `site_disabled` `hostname_not_allowed` |
//! | 4 | 规则（IP / 页面屏蔽、限流、垃圾引荐：尚未实现） | — |
//! | 5 | 机器人：`UaParser::parse`，`bot` 非空即丢弃（空 UA 也是机器人） | `bot` |
//! | 6 | 地理：`GeoDb::lookup(ip, site.geo_level)`，按站点精度截断 | — |
//! | 7 | UA 富化：浏览器、系统、设备类别 | — |
//! | 8 | 来源：内部引荐传 `None`，`rivu_source::classify` 给出 `referrer_source` 与 `channel` | — |
//! | 10 | 身份（盐缺失 `salt_missing`；前一天的盐缺失只是不算 `prev_visitor_id`） | `salt_missing` |
//! | 11 | SessionAssigner → 组装 `EventRow` → BatchWriter（缓冲满 `dropped_overloaded`） | `no_session` `overloaded` |
//!
//! **engagement 跳过第 5–8 步**，直接进身份和 SessionAssigner（没有会话就是 `no_session`）：
//! - engagement 不能开会话（§4.6），而富化的结果只在「开新会话」时才被 SessionAssigner 取用
//!   （会话属性由会话的第一个事件决定，此后盖到会话内的每个事件上，包括 engagement），
//!   所以对 engagement 做 UA / 地理 / 来源富化是白费的；
//! - 机器人的 pageview 在第 5 步已被丢弃，不会产生会话，它之后发来的 engagement 自然没有会话，
//!   在 SessionAssigner 处按 `no_session` 丢弃，不需要再做一遍机器人判定
//!   （代价：这类 engagement 记为 `dropped_no_session` 而不是 `dropped_bot`）。
//!
//! 每个结果都计入 `ingest_counters`：`accepted` 或 `dropped_<原因>`；未知站点记在 `site_id = 0`。
//! 400 不进 `ingest_counters`，只进 Prometheus `rivu_ingest_rejected_total{field}`（§10 Q13）。
//! 每个阶段的耗时记入直方图 `rivu_ingest_stage_seconds{stage}`（§4.2）。
//!
//! 时间由调用方传入（`RequestCtx::now_ms`），所以测试可以把事件放到任意时刻。

mod snapshot;

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use rivu_common::config::Cidr;
use rivu_domain::{
    DropReason, EventKind, METRIC_ACCEPTED, NormalizedEvent, SessionAttrs, UNKNOWN_SITE_ID,
};
use rivu_geo::GeoDb;
use rivu_session::assigner::{Assignment, Incoming, SessionAssigner, SessionConfig, Stamp};
use rivu_session::identity::{Salt, local_date, routing_hash, visitor_hash};
use rivu_source::{SourceInput, classify};
use rivu_storage_ch::rows::EventRow;
use rivu_ua::UaParser;
use rivu_writer::{TrySendError, WriterHandle};
use time::OffsetDateTime;
use tokio::sync::watch;
use tokio::task::JoinHandle;
use uuid::Uuid;

use crate::client_ip::client_ip;
use crate::protocol::{ProtocolErrors, parse_event};

pub use snapshot::{SaltCache, SiteConfig, SiteSnapshot, salt_window, utc_today};

/// 一次请求的上下文：来自 HTTP 层，不来自请求体。
#[derive(Clone, Debug)]
pub struct RequestCtx {
    /// TCP 对端地址。
    pub peer: IpAddr,
    /// 所有 `X-Forwarded-For` 头值，按出现顺序。
    pub xff: Vec<String>,
    pub user_agent: String,
    /// 接收时刻（Unix 毫秒），整个请求共用。
    pub now_ms: i64,
}

/// 管线对一个请求的处理结果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// 事件已进入写缓冲：`202`。
    Accepted,
    /// 被过滤：`202` + `x-rivu-dropped: 1`。
    Dropped(DropReason),
    /// 协议错误：`400`。
    Rejected(ProtocolErrors),
    /// 写缓冲满（或正在关停）：`503`。
    Overloaded,
}

/// 各富化步骤实际执行的次数（只增不减）。测试用它证明「engagement 不触发富化」，
/// 也方便排错；Prometheus 里对应阶段直方图的样本数。
#[derive(Debug, Default)]
pub struct EnrichWork {
    ua_parses: AtomicU64,
    geo_lookups: AtomicU64,
    source_classifications: AtomicU64,
}

impl EnrichWork {
    /// `UaParser::parse` 的调用次数（第 5 步）。
    pub fn ua_parses(&self) -> u64 {
        self.ua_parses.load(Ordering::Relaxed)
    }

    /// `GeoDb::lookup` 的调用次数（第 6 步）。
    pub fn geo_lookups(&self) -> u64 {
        self.geo_lookups.load(Ordering::Relaxed)
    }

    /// `rivu_source::classify` 的调用次数（第 8 步）。
    pub fn source_classifications(&self) -> u64 {
        self.source_classifications.load(Ordering::Relaxed)
    }
}

/// `rivu_ingest_stage_seconds{stage}` 的直方图句柄：构造时取一次，热路径不再做标签查找。
/// 句柄绑定到构造时已安装的 recorder；没有 recorder（单元测试）时是空操作。
struct StageHistograms {
    parse: metrics::Histogram,
    admit: metrics::Histogram,
    bot: metrics::Histogram,
    geo: metrics::Histogram,
    ua: metrics::Histogram,
    source: metrics::Histogram,
    identity: metrics::Histogram,
    session: metrics::Histogram,
    write: metrics::Histogram,
}

/// 阶段耗时直方图的指标名。
pub const STAGE_SECONDS_METRIC: &str = "rivu_ingest_stage_seconds";

impl StageHistograms {
    fn new() -> Self {
        let h = |stage: &'static str| metrics::histogram!(STAGE_SECONDS_METRIC, "stage" => stage);
        StageHistograms {
            parse: h("parse"),
            admit: h("admit"),
            bot: h("bot"),
            geo: h("geo"),
            ua: h("ua"),
            source: h("source"),
            identity: h("identity"),
            session: h("session"),
            write: h("write"),
        }
    }
}

fn observe(h: &metrics::Histogram, start: Instant) {
    h.record(start.elapsed().as_secs_f64());
}

/// 接入管线。线程安全，`process` 可被多个请求并发调用。
pub struct Pipeline {
    sites: Arc<SiteSnapshot>,
    salts: Arc<SaltCache>,
    assigner: SessionAssigner,
    writer: WriterHandle,
    trusted_proxies: Vec<Cidr>,
    /// 分片号用的长期密钥。Lite 不持久化它：重启后随机重新生成，内存里的会话也随之清空
    /// （从 ClickHouse 预热留到以后，§4.6「预热」、§10 Q15）。
    routing_secret: Salt,
    ua: Arc<UaParser>,
    geo: Arc<GeoDb>,
    work: EnrichWork,
    stages: StageHistograms,
}

impl Pipeline {
    /// `ua` 与 `geo` 是进程共享的富化器：`UaParser::embedded()` 构建一次约 130 ms，`GeoDb` 由后台任务热加载。
    /// `geo` 里还没有加载库时，位置字段为空（不阻塞接收，§4.4）。
    pub fn new(
        sites: Arc<SiteSnapshot>,
        salts: Arc<SaltCache>,
        writer: WriterHandle,
        trusted_proxies: Vec<Cidr>,
        ua: Arc<UaParser>,
        geo: Arc<GeoDb>,
    ) -> Self {
        Pipeline {
            sites,
            salts,
            assigner: SessionAssigner::new(SessionConfig::default()),
            writer,
            trusted_proxies,
            routing_secret: rand::random::<Salt>(),
            ua,
            geo,
            work: EnrichWork::default(),
            stages: StageHistograms::new(),
        }
    }

    /// 各富化步骤已执行的次数。
    pub fn enrich_work(&self) -> &EnrichWork {
        &self.work
    }

    /// 换成指定的 routing 密钥和会话参数（测试用，让分片和 `session_id` 可复现）。
    pub fn with_assigner(mut self, assigner: SessionAssigner, routing_secret: Salt) -> Self {
        self.assigner = assigner;
        self.routing_secret = routing_secret;
        self
    }

    pub fn assigner(&self) -> &SessionAssigner {
        &self.assigner
    }

    pub fn sites(&self) -> &Arc<SiteSnapshot> {
        &self.sites
    }

    pub fn salts(&self) -> &Arc<SaltCache> {
        &self.salts
    }

    /// 处理一个请求体。不 panic，不做 IO。
    pub fn process(&self, body: &[u8], ctx: RequestCtx) -> Outcome {
        // 1. 解析：400 不进 ingest_counters，也不暴露站点
        let t = Instant::now();
        let parsed = parse_event(body);
        observe(&self.stages.parse, t);
        let ev = match parsed {
            Ok(ev) => ev,
            Err(errors) => {
                for field in errors.fields() {
                    metrics::counter!("rivu_ingest_rejected_total", "field" => field.to_owned())
                        .increment(1);
                }
                return Outcome::Rejected(errors);
            }
        };

        // 2.–3. 客户端 IP、准入（站点查找、停用、hostname 白名单）
        let t = Instant::now();
        let xff: Vec<&str> = ctx.xff.iter().map(String::as_str).collect();
        let ip = client_ip(ctx.peer, &xff, &self.trusted_proxies);

        // 未知站点没有 site_id，记在 0 上
        let Some(site) = self.sites.get(&ev.site_key) else {
            observe(&self.stages.admit, t);
            return self.dropped(UNKNOWN_SITE_ID, &ev, DropReason::UnknownSite);
        };
        let site_id = site.site_id;
        if !site.collect_enabled {
            observe(&self.stages.admit, t);
            return self.dropped(site_id, &ev, DropReason::SiteDisabled);
        }
        // 空白名单 fail-closed（§10 Q12）
        if site.hostnames.is_empty() || !site.hostnames.matches(&ev.hostname) {
            observe(&self.stages.admit, t);
            return self.dropped(site_id, &ev, DropReason::HostnameNotAllowed);
        }
        observe(&self.stages.admit, t);

        // 4. 规则（IP / 页面屏蔽、垃圾引荐）：尚未实现。

        // 5.–8. 机器人、地理、UA、来源。engagement 整段跳过（见模块文档）：它不能开会话，
        // 属性也不会被取用，没有会话时直接按 `no_session` 丢弃。
        let attrs = if ev.kind == EventKind::Engagement {
            SessionAttrs::default()
        } else {
            match self.enrich(&ev, &site, ip, &ctx.user_agent) {
                Ok(attrs) => attrs,
                Err(reason) => return self.dropped(site_id, &ev, reason),
            }
        };

        // 10. 身份：按事件的站点本地日期选盐（§4.5）
        let t = Instant::now();
        let date = local_date(ctx.now_ms, &site.timezone);
        let Some(salt) = self.salts.get(date) else {
            observe(&self.stages.identity, t);
            return self.dropped(site_id, &ev, DropReason::SaltMissing);
        };
        let visitor_id = visitor_hash(&salt, site_id, ip, &ctx.user_agent);
        // 前一天的盐缺失：不算 prev_visitor_id，这次不续会话，事件照常接收
        let prev_visitor_id = date
            .yesterday()
            .ok()
            .and_then(|d| self.salts.get(d))
            .map(|prev_salt| visitor_hash(&prev_salt, site_id, ip, &ctx.user_agent));
        let routing = routing_hash(&self.routing_secret, site_id, ip, &ctx.user_agent);
        observe(&self.stages.identity, t);

        // 11. 会话分配与盖章
        let t = Instant::now();
        let assigned = self.assigner.assign(&Incoming {
            site_id,
            visitor_id,
            prev_visitor_id,
            routing,
            ts_ms: ctx.now_ms,
            kind: ev.kind,
            pathname: &ev.pathname,
            attrs: &attrs,
        });
        observe(&self.stages.session, t);
        let stamp = match assigned {
            Assignment::Stamped(stamp) => stamp,
            Assignment::NoSession => return self.dropped(site_id, &ev, DropReason::NoSession),
        };

        // 组装行并交给 BatchWriter
        let t = Instant::now();
        let row = build_row(site_id, ev.clone(), &stamp, ctx.now_ms);
        let sent = self.writer.try_send(row);
        observe(&self.stages.write, t);
        match sent {
            Ok(()) => {
                self.writer
                    .count(site_id, METRIC_ACCEPTED, ev.script_version);
                metrics::counter!("rivu_ingest_events_total", "outcome" => "accepted").increment(1);
                Outcome::Accepted
            }
            Err(TrySendError::Full | TrySendError::Closed) => {
                self.dropped(site_id, &ev, DropReason::Overloaded);
                Outcome::Overloaded
            }
        }
    }

    /// 第 5–8 步：机器人 → 地理 → UA 富化 → 来源，得到会话属性；机器人返回 `Err(DropReason::Bot)`。
    ///
    /// 一次 `UaParser::parse` 同时给出机器人判定和 UA 富化的结果（带缓存），它的耗时记在 `bot` 阶段
    /// （第一次需要 UA 的地方）；`ua` 阶段只是把结果写进会话属性。
    fn enrich(
        &self,
        ev: &NormalizedEvent,
        site: &SiteConfig,
        ip: IpAddr,
        user_agent: &str,
    ) -> Result<SessionAttrs, DropReason> {
        // 5. 机器人：空 UA 也算（`BotKind::Empty`，偏离 Plausible，见 docs/research/enrich-ua.md §4.2）。
        // `BotKind` 只用于排错，不进 ingest_counters。
        let t = Instant::now();
        self.work.ua_parses.fetch_add(1, Ordering::Relaxed);
        let ua = self.ua.parse(user_agent);
        observe(&self.stages.bot, t);
        if let Some(kind) = ua.bot {
            tracing::trace!(bot = ?kind, "丢弃机器人事件");
            return Err(DropReason::Bot);
        }

        // 6. 地理：按站点的 geo_level 截断；私有地址、库未加载、查不到都是空值
        let t = Instant::now();
        self.work.geo_lookups.fetch_add(1, Ordering::Relaxed);
        let geo = self.geo.lookup(ip, site.geo_level);
        observe(&self.stages.geo, t);

        // 7. UA 富化
        let t = Instant::now();
        let (device, browser, browser_version, os, os_version) = (
            ua.device.as_str().to_owned(),
            ua.browser.clone(),
            ua.browser_version.clone(),
            ua.os.clone(),
            ua.os_version.clone(),
        );
        observe(&self.stages.ua, t);

        // 8. 来源：referrer 的 host 命中站点白名单是内部引荐，传 None（库不区分它和「没有 referrer」）；
        // 落库的 referrer 也只在外部引荐时保留。
        let t = Instant::now();
        self.work
            .source_classifications
            .fetch_add(1, Ordering::Relaxed);
        let external = ev
            .referrer
            .as_ref()
            .filter(|r| !site.hostnames.matches(&r.host));
        let source = classify(&SourceInput {
            referrer_host: external.map(|r| r.host.as_str()),
            utm: &ev.utm,
            click_id_param: ev.click_id_param,
        });
        observe(&self.stages.source, t);

        Ok(SessionAttrs {
            referrer: external.map(|r| r.stored.clone()).unwrap_or_default(),
            referrer_source: source.referrer_source,
            channel: source.channel.as_str().to_owned(),
            utm_source: ev.utm.source.clone().unwrap_or_default(),
            utm_medium: ev.utm.medium.clone().unwrap_or_default(),
            utm_campaign: ev.utm.campaign.clone().unwrap_or_default(),
            utm_content: ev.utm.content.clone().unwrap_or_default(),
            utm_term: ev.utm.term.clone().unwrap_or_default(),
            click_id_param: ev.click_id_param.unwrap_or_default().to_owned(),
            country_code: geo.country_code,
            region_code: geo.region_code,
            city_geoname_id: geo.city_geoname_id,
            device,
            browser,
            browser_version,
            os,
            os_version,
        })
    }

    /// 记一次丢弃：`ingest_counters` + Prometheus。
    fn dropped(&self, site_id: u64, ev: &NormalizedEvent, reason: DropReason) -> Outcome {
        self.writer
            .count(site_id, reason.metric(), ev.script_version);
        metrics::counter!("rivu_ingest_events_total", "outcome" => reason.as_str()).increment(1);
        Outcome::Dropped(reason)
    }

    /// 清扫过期会话（按分片内事件时间水位，不用墙钟），返回移除的会话数。
    pub fn sweep(&self) -> usize {
        self.assigner.sweep()
    }

    /// 每 `every`（默认 60 s）清扫一次，直到收到停止信号。
    pub fn spawn_sweeper(
        self: &Arc<Self>,
        every: Duration,
        stop: watch::Receiver<bool>,
    ) -> JoinHandle<()> {
        let this = Arc::clone(self);
        snapshot::spawn_periodic(every, stop, move || {
            let this = Arc::clone(&this);
            async move {
                let removed = this.sweep();
                metrics::gauge!("rivu_session_active").set(this.assigner.len() as f64);
                if removed > 0 {
                    tracing::debug!(removed, "已清扫过期会话");
                }
            }
        })
    }
}

/// 注册管线的 Prometheus 指标说明。
pub fn describe_metrics() {
    metrics::describe_counter!(
        "rivu_ingest_rejected_total",
        "协议错误（400）数，按出错字段；不进 ingest_counters"
    );
    metrics::describe_counter!(
        "rivu_ingest_events_total",
        "管线处理结果：accepted 或丢弃原因（unknown_site 等）"
    );
    metrics::describe_gauge!("rivu_session_active", "SessionAssigner 里保留的会话数");
    metrics::describe_histogram!(
        STAGE_SECONDS_METRIC,
        metrics::Unit::Seconds,
        "接入管线各阶段的耗时，按阶段（parse、admit、bot、geo、ua、source、identity、session、write）"
    );
}

/// 毫秒时间戳转 `OffsetDateTime`；超出范围时取 Unix 纪元（不 panic）。
fn datetime_from_ms(ms: i64) -> OffsetDateTime {
    OffsetDateTime::from_unix_timestamp_nanos(i128::from(ms) * 1_000_000)
        .unwrap_or(OffsetDateTime::UNIX_EPOCH)
}

/// 以事件时间为时间戳的 UUIDv7：`event_id` 的高位与 `ts` 一致。
fn event_id(ts_ms: i64) -> Uuid {
    let ms = u64::try_from(ts_ms).unwrap_or(0);
    let ts = uuid::Timestamp::from_unix(
        uuid::NoContext,
        ms / 1000,
        u32::try_from(ms % 1000).unwrap_or(0) * 1_000_000,
    );
    Uuid::new_v7(ts)
}

/// 组装 `events` 的一行。
///
/// `visitor_id` 用 `Stamp::visitor_id`（事件自己当天的 ID，盖章不改写，会话靠 `session_id` 串联；§10 Q9），
/// `ingest_partition` 用 `Stamp::partition`（Q15：`routing % 32`）。
fn build_row(site_id: u64, ev: NormalizedEvent, stamp: &Stamp, ts_ms: i64) -> EventRow {
    let a = &*stamp.attrs;
    let mut props = HashMap::with_capacity(ev.props.len());
    props.extend(ev.props);
    EventRow {
        site_id,
        ts: datetime_from_ms(ts_ms),
        event_id: event_id(ts_ms),
        name: ev.name,
        visitor_id: stamp.visitor_id,
        session_id: stamp.session_id,
        session_start: datetime_from_ms(stamp.session_start_ms),
        ingest_partition: stamp.partition,
        hostname: ev.hostname,
        pathname: ev.pathname,
        entry_pathname: stamp.entry_pathname.to_string(),
        referrer: a.referrer.clone(),
        referrer_source: a.referrer_source.clone(),
        channel: a.channel.clone(),
        utm_source: a.utm_source.clone(),
        utm_medium: a.utm_medium.clone(),
        utm_campaign: a.utm_campaign.clone(),
        utm_content: a.utm_content.clone(),
        utm_term: a.utm_term.clone(),
        click_id_param: a.click_id_param.clone(),
        country_code: a.country_code,
        region_code: a.region_code.clone(),
        city_geoname_id: a.city_geoname_id,
        device: a.device.clone(),
        browser: a.browser.clone(),
        browser_version: a.browser_version.clone(),
        os: a.os.clone(),
        os_version: a.os_version.clone(),
        interactive: ev.interactive,
        props,
        // 协议解析已保证：非 engagement 事件恒为 255（无）
        scroll_depth: ev.scroll_depth,
        engagement_ms: ev.engagement_ms,
        revenue_amount: None,
        revenue_currency: String::new(),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use std::sync::OnceLock;

    use jiff::civil::{Date, date};
    use jiff::tz::TimeZone;
    use rivu_geo::GeoLevel;
    use serde_json::json;
    use tokio::sync::mpsc;

    use super::*;

    const KEY: &str = "aaaaaaaaaaaa";
    const OFF_KEY: &str = "dddddddddddd";
    const EMPTY_KEY: &str = "eeeeeeeeeeee";
    const COUNTRY_KEY: &str = "gggggggggggg";
    const REGION_KEY: &str = "hhhhhhhhhhhh";
    const SITE: u64 = 42;
    const OFF_SITE: u64 = 43;
    const EMPTY_SITE: u64 = 44;
    const COUNTRY_SITE: u64 = 45;
    const REGION_SITE: u64 = 46;
    const MIN: i64 = 60_000;

    /// 真实浏览器 UA（取自 uap-core 测试集的同类字符串）。
    const CHROME_WIN: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36";
    const GOOGLEBOT: &str =
        "Mozilla/5.0 (compatible; Googlebot/2.1; +http://www.google.com/bot.html)";
    /// MaxMind 官方测试库（`GeoIP2-City-Test`）里有记录的地址：`89.160.20.128` -> SE / SE-E / 2694762。
    const SE_IP: &str = "89.160.20.128";

    /// 解析器构建一次要编译全部 uap-core 规则，测试间共享。
    fn ua_parser() -> Arc<UaParser> {
        static PARSER: OnceLock<Arc<UaParser>> = OnceLock::new();
        Arc::clone(PARSER.get_or_init(|| Arc::new(UaParser::embedded().expect("uap-core 规则"))))
    }

    /// 装载了 MaxMind 官方测试库的 `GeoDb`。
    fn test_geo() -> Arc<GeoDb> {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../geo/tests/data/test-data/GeoIP2-City-Test.mmdb");
        let db = GeoDb::new();
        assert!(db.reload_if_changed(&path).expect("加载测试库"));
        Arc::new(db)
    }

    fn berlin() -> TimeZone {
        TimeZone::get("Europe/Berlin").unwrap()
    }

    /// 柏林本地时间 → 毫秒时间戳。
    fn at(d: Date, h: i8, m: i8) -> i64 {
        d.at(h, m, 0, 0)
            .to_zoned(berlin())
            .unwrap()
            .timestamp()
            .as_millisecond()
    }

    fn salts(days: &[Date]) -> HashMap<Date, Salt> {
        days.iter()
            .enumerate()
            .map(|(i, d)| (*d, [i as u8 + 1; 16]))
            .collect()
    }

    fn march(days: std::ops::RangeInclusive<i8>) -> Vec<Date> {
        days.map(|d| date(2026, 3, d)).collect()
    }

    struct Rig {
        pipeline: Pipeline,
        rx: mpsc::Receiver<EventRow>,
        writer: WriterHandle,
        salts: Arc<SaltCache>,
    }

    fn rig(capacity: usize) -> Rig {
        rig_with_geo(capacity, Arc::new(GeoDb::new()))
    }

    fn rig_with_geo(capacity: usize, geo: Arc<GeoDb>) -> Rig {
        let site = |id: u64, collect: bool, hosts: &[&str], geo_level: GeoLevel| {
            Arc::new(SiteConfig {
                site_id: id,
                timezone: berlin(),
                collect_enabled: collect,
                hostnames: crate::hostname::HostnamePatterns::new(hosts.iter().copied()),
                geo_level,
                tracker_hash: false,
            })
        };
        let city = GeoLevel::City;
        let sites = Arc::new(SiteSnapshot::new());
        sites.replace(HashMap::from([
            (KEY.to_owned(), site(SITE, true, &["example.test"], city)),
            (
                OFF_KEY.to_owned(),
                site(OFF_SITE, false, &["example.test"], city),
            ),
            (EMPTY_KEY.to_owned(), site(EMPTY_SITE, true, &[], city)),
            (
                COUNTRY_KEY.to_owned(),
                site(COUNTRY_SITE, true, &["example.test"], GeoLevel::Country),
            ),
            (
                REGION_KEY.to_owned(),
                site(REGION_SITE, true, &["example.test"], GeoLevel::Region),
            ),
        ]));
        let salt_cache = Arc::new(SaltCache::new());
        salt_cache.replace(salts(&march(27..=31)));
        let (writer, rx) = WriterHandle::detached(capacity);
        let trusted = vec!["127.0.0.1/32".parse().unwrap()];
        let pipeline = Pipeline::new(
            sites,
            Arc::clone(&salt_cache),
            writer.clone(),
            trusted,
            ua_parser(),
            geo,
        )
        .with_assigner(
            SessionAssigner::with_seed(SessionConfig::default(), 1),
            [9; 16],
        );
        Rig {
            pipeline,
            rx,
            writer,
            salts: salt_cache,
        }
    }

    fn body(key: &str, name: &str, url: &str) -> Vec<u8> {
        json!({"v": 1, "s": key, "n": name, "u": url})
            .to_string()
            .into_bytes()
    }

    fn pageview(path: &str) -> Vec<u8> {
        body(KEY, "pageview", &format!("https://example.test{path}"))
    }

    fn ctx(ip: &str, ua: &str, now_ms: i64) -> RequestCtx {
        RequestCtx {
            peer: "127.0.0.1".parse().unwrap(),
            xff: vec![ip.to_owned()],
            user_agent: ua.to_owned(),
            now_ms,
        }
    }

    /// 取出并清空计数：`(site_id, metric) -> value`。
    fn counts(w: &WriterHandle) -> BTreeMap<(u64, String), u64> {
        let mut out = BTreeMap::new();
        for r in w.counters().take_all() {
            *out.entry((r.site_id, r.metric)).or_insert(0) += r.value;
        }
        out
    }

    fn key(site: u64, metric: &str) -> (u64, String) {
        (site, metric.to_owned())
    }

    #[test]
    fn accepted_pageview_becomes_a_stamped_row() {
        let mut r = rig(10);
        let t = at(date(2026, 3, 28), 12, 0);
        let out = r.pipeline.process(
            json!({"v": 1, "sv": 5, "s": KEY, "n": "pageview",
                   "u": "https://www.Example.test/pricing?utm_source=news&utm_medium=email&gclid=1#x",
                   "r": "https://news.example.org/post/42?id=7",
                   "p": {"plan": "pro"}})
                .to_string()
                .as_bytes(),
            ctx("203.0.113.9", CHROME_WIN, t),
        );
        assert_eq!(out, Outcome::Accepted);
        let row = r.rx.try_recv().unwrap();
        assert_eq!(row.site_id, SITE);
        assert_eq!(row.name, "pageview");
        assert_eq!(row.hostname, "example.test");
        assert_eq!(row.pathname, "/pricing");
        assert_eq!(row.entry_pathname, "/pricing");
        assert_eq!(row.ts.unix_timestamp() * 1000, t);
        assert_eq!(row.session_start, row.ts);
        assert_ne!(row.session_id, 0);
        assert!(row.ingest_partition < 32);
        assert_eq!(row.referrer, "news.example.org/post/42");
        // utm_source 覆盖 referrer 的 host；utm_medium=email 决定渠道
        assert_eq!(row.referrer_source, "news");
        assert_eq!(row.channel, "Email");
        assert_eq!(
            (row.utm_source.as_str(), row.utm_medium.as_str()),
            ("news", "email")
        );
        assert_eq!(row.click_id_param, "gclid");
        // 203.0.113.9 是文档地址：不是公网，没有地理信息
        assert_eq!(row.country_code, [0, 0]);
        assert_eq!(
            (
                row.device.as_str(),
                row.browser.as_str(),
                row.browser_version.as_str(),
                row.os.as_str(),
                row.os_version.as_str()
            ),
            ("desktop", "Chrome", "126.0", "Windows", "10")
        );
        assert!(row.interactive);
        assert_eq!(row.scroll_depth, 255);
        assert_eq!(row.props.get("plan").map(String::as_str), Some("pro"));
        // UUIDv7，时间戳与事件时间一致
        assert_eq!(row.event_id.get_version_num(), 7);
        let (secs, _) = row.event_id.get_timestamp().unwrap().to_unix();
        assert_eq!(secs as i64, t / 1000);
        // 计数：accepted 记在该站点上
        assert_eq!(
            counts(&r.writer),
            BTreeMap::from([(key(SITE, "accepted"), 1)])
        );
    }

    #[test]
    fn every_drop_reason_is_counted_on_the_right_site() {
        let r = rig(10);
        let t = at(date(2026, 3, 28), 12, 0);
        let p = &r.pipeline;
        let c = || ctx("203.0.113.9", "UA", t);

        assert_eq!(
            p.process(
                &body("zzzzzzzzzzzz", "pageview", "https://example.test/"),
                c()
            ),
            Outcome::Dropped(DropReason::UnknownSite)
        );
        assert_eq!(
            p.process(&body(OFF_KEY, "pageview", "https://example.test/"), c()),
            Outcome::Dropped(DropReason::SiteDisabled)
        );
        assert_eq!(
            p.process(&body(KEY, "pageview", "https://other.test/"), c()),
            Outcome::Dropped(DropReason::HostnameNotAllowed)
        );
        // 空白名单 fail-closed
        assert_eq!(
            p.process(&body(EMPTY_KEY, "pageview", "https://example.test/"), c()),
            Outcome::Dropped(DropReason::HostnameNotAllowed)
        );
        // 事件本地日期（5/1）没有盐
        assert_eq!(
            p.process(
                &pageview("/"),
                ctx("203.0.113.9", "UA", at(date(2026, 5, 1), 12, 0))
            ),
            Outcome::Dropped(DropReason::SaltMissing)
        );
        // 没有会话的 engagement
        assert_eq!(
            p.process(
                &json!({"v":1,"s":KEY,"n":"engagement","u":"https://example.test/","e":1000})
                    .to_string()
                    .into_bytes(),
                c()
            ),
            Outcome::Dropped(DropReason::NoSession)
        );
        assert_eq!(
            counts(&r.writer),
            BTreeMap::from([
                (key(0, "dropped_unknown_site"), 1),
                (key(OFF_SITE, "dropped_site_disabled"), 1),
                (key(SITE, "dropped_hostname_not_allowed"), 1),
                (key(EMPTY_SITE, "dropped_hostname_not_allowed"), 1),
                (key(SITE, "dropped_salt_missing"), 1),
                (key(SITE, "dropped_no_session"), 1),
            ])
        );
    }

    #[test]
    fn protocol_errors_are_rejected_and_not_counted() {
        let mut r = rig(10);
        let t = at(date(2026, 3, 28), 12, 0);
        for bad in [&b"not json"[..], br#"{"v":1}"#, br#"[]"#] {
            let out = r.pipeline.process(bad, ctx("203.0.113.9", "UA", t));
            assert!(
                matches!(out, Outcome::Rejected(ref e) if !e.is_empty()),
                "{out:?}"
            );
        }
        assert!(counts(&r.writer).is_empty(), "400 不进 ingest_counters");
        assert!(r.rx.try_recv().is_err());
        assert_eq!(r.pipeline.assigner().len(), 0, "400 不碰会话状态");
    }

    #[test]
    fn full_buffer_is_overloaded_and_counted_but_not_accepted() {
        let mut r = rig(1);
        let t = at(date(2026, 3, 28), 12, 0);
        assert_eq!(
            r.pipeline
                .process(&pageview("/a"), ctx("203.0.113.9", "UA", t)),
            Outcome::Accepted
        );
        assert_eq!(
            r.pipeline
                .process(&pageview("/b"), ctx("203.0.113.9", "UA", t + 1000)),
            Outcome::Overloaded
        );
        assert_eq!(
            counts(&r.writer),
            BTreeMap::from([
                (key(SITE, "accepted"), 1),
                (key(SITE, "dropped_overloaded"), 1),
            ])
        );
        assert_eq!(r.rx.try_recv().unwrap().pathname, "/a");
        assert!(r.rx.try_recv().is_err());
    }

    #[test]
    fn closed_buffer_is_also_overloaded() {
        let r = rig(1);
        drop(r.rx);
        let t = at(date(2026, 3, 28), 12, 0);
        assert_eq!(
            r.pipeline
                .process(&pageview("/"), ctx("203.0.113.9", "UA", t)),
            Outcome::Overloaded
        );
    }

    #[test]
    fn internal_referrer_is_not_a_source_but_external_is() {
        let mut r = rig(10);
        let t = at(date(2026, 3, 28), 12, 0);
        let with_ref = |r: &str| {
            json!({"v":1,"s":KEY,"n":"pageview","u":"https://example.test/","r":r})
                .to_string()
                .into_bytes()
        };
        // 站点白名单是 example.test：来自本站的引荐是内部引荐，来自别的站才是来源
        r.pipeline.process(
            &with_ref("https://example.test/from"),
            ctx("1.1.1.1", "A", t),
        );
        r.pipeline.process(
            &with_ref("https://news.example.org/p"),
            ctx("2.2.2.2", "A", t),
        );
        let internal = r.rx.try_recv().unwrap();
        let external = r.rx.try_recv().unwrap();
        // 内部引荐：没有来源，渠道是 Direct
        assert_eq!(
            (
                internal.referrer.as_str(),
                internal.referrer_source.as_str(),
                internal.channel.as_str()
            ),
            ("", "", "Direct")
        );
        // 外部引荐：认不出的 host 用 host 本身作来源，渠道 Referral
        assert_eq!(external.referrer, "news.example.org/p");
        assert_eq!(external.referrer_source, "news.example.org");
        assert_eq!(external.channel, "Referral");
    }

    /// 取出一行并返回 `(referrer_source, channel)`。
    fn source_of(row: &EventRow) -> (&str, &str) {
        (row.referrer_source.as_str(), row.channel.as_str())
    }

    #[test]
    fn referrers_are_classified_into_source_and_channel() {
        let mut r = rig(10);
        let t = at(date(2026, 3, 28), 12, 0);
        let send = |r: &Rig, i: u8, extra: serde_json::Value| {
            let mut b = json!({"v":1,"s":KEY,"n":"pageview","u":"https://example.test/"});
            for (k, v) in extra.as_object().unwrap() {
                b[k] = v.clone();
            }
            r.pipeline.process(
                b.to_string().as_bytes(),
                ctx(&format!("1.1.1.{i}"), CHROME_WIN, t),
            )
        };
        // 搜索引擎 referrer（www. 与国家后缀）-> Google / Organic Search
        send(&r, 1, json!({"r": "https://www.google.co.jp/search?q=x"}));
        // 社交
        send(&r, 2, json!({"r": "https://t.co/abc"}));
        // 没有 referrer、没有标签 -> Direct
        send(&r, 3, json!({}));
        // 只有 click id（隐私设置抹掉了 referrer）-> 推断 Google / Paid Search
        send(&r, 4, json!({"u": "https://example.test/?gclid=abc"}));
        let rows: Vec<EventRow> = (0..4).map(|_| r.rx.try_recv().unwrap()).collect();
        assert_eq!(source_of(&rows[0]), ("Google", "Organic Search"));
        assert_eq!(source_of(&rows[1]), ("X (Twitter)", "Organic Social"));
        assert_eq!(source_of(&rows[2]), ("", "Direct"));
        assert_eq!(source_of(&rows[3]), ("Google", "Paid Search"));
        assert_eq!(rows[3].click_id_param, "gclid");
    }

    #[test]
    fn utm_source_overrides_the_referrer_and_medium_decides_the_channel() {
        let mut r = rig(10);
        let t = at(date(2026, 3, 28), 12, 0);
        // referrer 是 google，但链接带了 utm_source=bing&utm_medium=cpc：来源取 utm（别名表规范化成 Bing），
        // 渠道是 Paid Search；referrer 本身仍原样存下
        r.pipeline.process(
            json!({"v":1,"s":KEY,"n":"pageview",
                   "u":"https://example.test/?utm_source=bing&utm_medium=cpc&utm_campaign=spring",
                   "r":"https://www.google.com/"})
            .to_string()
            .as_bytes(),
            ctx("1.1.1.1", CHROME_WIN, t),
        );
        let row = r.rx.try_recv().unwrap();
        assert_eq!(source_of(&row), ("Bing", "Paid Search"));
        assert_eq!(row.referrer, "google.com");
        assert_eq!(
            (row.utm_source.as_str(), row.utm_campaign.as_str()),
            ("bing", "spring")
        );
    }

    #[test]
    fn bots_are_dropped_and_counted() {
        let mut r = rig(10);
        let t = at(date(2026, 3, 28), 12, 0);
        for (i, ua) in [GOOGLEBOT, "", "curl/8.5.0", "HeadlessChrome/126.0.0.0"]
            .into_iter()
            .enumerate()
        {
            assert_eq!(
                r.pipeline
                    .process(&pageview("/"), ctx("1.1.1.1", ua, t + i as i64)),
                Outcome::Dropped(DropReason::Bot),
                "{ua:?}"
            );
        }
        // 真实浏览器不受影响
        assert_eq!(
            r.pipeline
                .process(&pageview("/"), ctx("1.1.1.1", CHROME_WIN, t + 10)),
            Outcome::Accepted
        );
        assert_eq!(
            counts(&r.writer),
            BTreeMap::from([(key(SITE, "dropped_bot"), 4), (key(SITE, "accepted"), 1)])
        );
        assert_eq!(r.rx.try_recv().unwrap().pathname, "/");
        assert!(r.rx.try_recv().is_err(), "机器人事件不写入");
        assert_eq!(r.pipeline.assigner().len(), 1, "机器人不开会话");
    }

    #[test]
    fn a_bots_engagement_has_no_session_and_is_counted_as_no_session() {
        let r = rig(10);
        let t = at(date(2026, 3, 28), 12, 0);
        let eng =
            json!({"v":1,"s":KEY,"n":"engagement","u":"https://example.test/","e":4000,"sd":10})
                .to_string()
                .into_bytes();
        assert_eq!(
            r.pipeline
                .process(&pageview("/"), ctx("1.1.1.1", GOOGLEBOT, t)),
            Outcome::Dropped(DropReason::Bot)
        );
        assert_eq!(
            r.pipeline
                .process(&eng, ctx("1.1.1.1", GOOGLEBOT, t + 1000)),
            Outcome::Dropped(DropReason::NoSession)
        );
    }

    #[test]
    fn geo_is_truncated_to_the_sites_level() {
        let mut r = rig_with_geo(10, test_geo());
        let t = at(date(2026, 3, 28), 12, 0);
        let pv = |key: &str| body(key, "pageview", "https://example.test/");
        for (key, i) in [(COUNTRY_KEY, 0), (REGION_KEY, 1), (KEY, 2)] {
            assert_eq!(
                r.pipeline.process(&pv(key), ctx(SE_IP, CHROME_WIN, t + i)),
                Outcome::Accepted
            );
        }
        let (country, region, city) = (
            r.rx.try_recv().unwrap(),
            r.rx.try_recv().unwrap(),
            r.rx.try_recv().unwrap(),
        );
        // country：只有国家
        assert_eq!(country.country_code, *b"SE");
        assert_eq!(
            (country.region_code.as_str(), country.city_geoname_id),
            ("", 0)
        );
        // region：加上 ISO 3166-2 子区划，没有城市
        assert_eq!(region.country_code, *b"SE");
        assert_eq!(
            (region.region_code.as_str(), region.city_geoname_id),
            ("SE-E", 0)
        );
        // city：全部
        assert_eq!(city.country_code, *b"SE");
        assert_eq!(
            (city.region_code.as_str(), city.city_geoname_id),
            ("SE-E", 2_694_762)
        );
    }

    #[test]
    fn private_addresses_and_unloaded_databases_give_empty_geo() {
        let mut r = rig_with_geo(10, test_geo());
        let t = at(date(2026, 3, 28), 12, 0);
        r.pipeline
            .process(&pageview("/"), ctx("10.1.2.3", CHROME_WIN, t));
        assert_eq!(r.rx.try_recv().unwrap().country_code, [0, 0]);
        // 库没加载：位置为空，事件照常接收
        let mut r = rig(10);
        assert_eq!(
            r.pipeline
                .process(&pageview("/"), ctx(SE_IP, CHROME_WIN, t)),
            Outcome::Accepted
        );
        assert_eq!(r.rx.try_recv().unwrap().country_code, [0, 0]);
    }

    #[test]
    fn engagement_does_not_trigger_enrichment_and_takes_the_sessions_attributes() {
        let mut r = rig_with_geo(10, test_geo());
        let t = at(date(2026, 3, 28), 12, 0);
        let work = r.pipeline.enrich_work();
        // 请求体里带了与会话不同的 referrer：如果 engagement 被富化，来源会变成 Google
        let eng = json!({"v":1,"s":KEY,"n":"engagement","u":"https://example.test/","e":4000,
                         "sd":10,"r":"https://www.google.com/"})
        .to_string()
        .into_bytes();
        // 没有会话的 engagement：丢弃，没有任何富化
        assert_eq!(
            r.pipeline.process(&eng, ctx(SE_IP, CHROME_WIN, t)),
            Outcome::Dropped(DropReason::NoSession)
        );
        assert_eq!(
            (
                work.ua_parses(),
                work.geo_lookups(),
                work.source_classifications()
            ),
            (0, 0, 0)
        );
        // pageview 开会话：三个富化各做一次
        r.pipeline.process(
            &body(
                KEY,
                "pageview",
                "https://example.test/?utm_source=bing&utm_medium=cpc",
            ),
            ctx(SE_IP, CHROME_WIN, t + 1000),
        );
        assert_eq!(
            (
                work.ua_parses(),
                work.geo_lookups(),
                work.source_classifications()
            ),
            (1, 1, 1)
        );
        // 有会话的 engagement：被接受，仍然不富化，行上盖的是会话（第一个 pageview）的属性，
        // 即使 engagement 自己的请求体带了不同的 referrer
        assert_eq!(
            r.pipeline.process(&eng, ctx(SE_IP, CHROME_WIN, t + 2000)),
            Outcome::Accepted
        );
        assert_eq!(
            (
                work.ua_parses(),
                work.geo_lookups(),
                work.source_classifications()
            ),
            (1, 1, 1),
            "engagement 不触发 UA 解析、地理查询和来源分类"
        );
        let (_pv, e) = (r.rx.try_recv().unwrap(), r.rx.try_recv().unwrap());
        assert_eq!(e.name, "engagement");
        assert_eq!(source_of(&e), ("Bing", "Paid Search"));
        assert_eq!(e.country_code, *b"SE");
        assert_eq!(
            (e.device.as_str(), e.browser.as_str()),
            ("desktop", "Chrome")
        );
    }

    #[test]
    fn stage_timing_counters_follow_the_event_kind() {
        // 被丢弃的机器人只做了 UA 解析：没有地理和来源
        let r = rig_with_geo(10, test_geo());
        let t = at(date(2026, 3, 28), 12, 0);
        r.pipeline.process(&pageview("/"), ctx(SE_IP, GOOGLEBOT, t));
        let w = r.pipeline.enrich_work();
        assert_eq!(
            (w.ua_parses(), w.geo_lookups(), w.source_classifications()),
            (1, 0, 0)
        );
        // 站点未知、hostname 不在白名单：连 UA 都不解析（先做便宜的判断）
        r.pipeline.process(
            &body("zzzzzzzzzzzz", "pageview", "https://example.test/"),
            ctx(SE_IP, CHROME_WIN, t),
        );
        r.pipeline.process(
            &body(KEY, "pageview", "https://evil.test/"),
            ctx(SE_IP, CHROME_WIN, t),
        );
        assert_eq!(w.ua_parses(), 1);
    }

    #[test]
    fn session_continues_across_local_midnight_and_each_event_keeps_its_own_visitor_id() {
        let mut r = rig(10);
        let d = date(2026, 3, 28);
        let c = |t| ctx("203.0.113.9", "UA", t);
        r.pipeline.process(&pageview("/a"), c(at(d, 23, 50)));
        r.pipeline
            .process(&pageview("/b"), c(at(d.tomorrow().unwrap(), 0, 10)));
        let (a, b) = (r.rx.try_recv().unwrap(), r.rx.try_recv().unwrap());
        assert_eq!(a.session_id, b.session_id);
        assert_eq!(a.session_start, b.session_start);
        assert_ne!(
            a.visitor_id, b.visitor_id,
            "Q9：盖章不改写 visitor_id，事件保留自己当天的 ID"
        );
        assert_eq!(b.entry_pathname, "/a");
    }

    #[test]
    fn missing_previous_day_salt_means_no_continuation() {
        let mut r = rig(10);
        let d = date(2026, 3, 28);
        let c = |t| ctx("203.0.113.9", "UA", t);
        r.pipeline.process(&pageview("/a"), c(at(d, 23, 50)));
        // 3/28 的盐消失（过期被清了）：3/29 的事件算不出 prev_visitor_id，只能开新会话，但照常接收
        let mut m = salts(&march(27..=31));
        m.remove(&d);
        r.salts.replace(m);
        assert_eq!(
            r.pipeline
                .process(&pageview("/b"), c(at(d.tomorrow().unwrap(), 0, 10))),
            Outcome::Accepted
        );
        let (a, b) = (r.rx.try_recv().unwrap(), r.rx.try_recv().unwrap());
        assert_ne!(a.session_id, b.session_id);
        assert_eq!(b.entry_pathname, "/b");
    }

    #[test]
    fn the_forwarded_address_from_a_trusted_proxy_identifies_the_visitor() {
        let mut r = rig(10);
        let t = at(date(2026, 3, 28), 12, 0);
        r.pipeline
            .process(&pageview("/"), ctx("203.0.113.1", "UA", t));
        r.pipeline
            .process(&pageview("/"), ctx("203.0.113.2", "UA", t + 1000));
        r.pipeline
            .process(&pageview("/"), ctx("203.0.113.1", "UA", t + 2000));
        r.pipeline
            .process(&pageview("/"), ctx("203.0.113.1", "Other", t + 3000));
        let rows: Vec<EventRow> = (0..4).map(|_| r.rx.try_recv().unwrap()).collect();
        assert_eq!(rows[0].session_id, rows[2].session_id);
        assert_ne!(rows[0].session_id, rows[1].session_id);
        assert_ne!(rows[0].session_id, rows[3].session_id);
        // 来自不可信对端的 XFF 被忽略：伪造的头不能制造出新访客
        let mut spoof = ctx("203.0.113.1", "UA", t + 4000);
        spoof.peer = "198.51.100.7".parse().unwrap();
        r.pipeline.process(&pageview("/"), spoof);
        assert_ne!(r.rx.try_recv().unwrap().session_id, rows[0].session_id);
    }

    #[test]
    fn custom_events_and_engagement_carry_their_fields() {
        let mut r = rig(10);
        let t = at(date(2026, 3, 28), 12, 0);
        let c = |dt| ctx("203.0.113.9", "UA", t + dt);
        r.pipeline.process(&pageview("/p"), c(0));
        r.pipeline.process(
            &json!({"v":1,"s":KEY,"n":"Signup","u":"https://example.test/p","i":false,"p":{"a":"1","b":2}})
                .to_string()
                .into_bytes(),
            c(MIN),
        );
        r.pipeline.process(
            &json!({"v":1,"s":KEY,"n":"engagement","u":"https://example.test/p","sd":70,"e":4500})
                .to_string()
                .into_bytes(),
            c(2 * MIN),
        );
        let (_pv, custom, eng) = (
            r.rx.try_recv().unwrap(),
            r.rx.try_recv().unwrap(),
            r.rx.try_recv().unwrap(),
        );
        assert_eq!(custom.name, "Signup");
        assert!(!custom.interactive);
        assert_eq!(custom.props.len(), 2);
        assert_eq!(custom.props["b"], "2");
        assert_eq!(custom.scroll_depth, 255);
        assert_eq!(eng.name, "engagement");
        assert_eq!((eng.scroll_depth, eng.engagement_ms), (70, 4500));
        assert_eq!(eng.entry_pathname, "/p");
    }

    #[test]
    fn counters_conserve_every_non_rejected_request() {
        let mut r = rig(3);
        let t = at(date(2026, 3, 28), 12, 0);
        let mut processed = 0u64;
        let mut rejected = 0u64;
        for i in 0..40i64 {
            let b = match i % 5 {
                0 => body("zzzzzzzzzzzz", "pageview", "https://example.test/"),
                1 => b"{".to_vec(),
                2 => body(KEY, "pageview", "https://other.test/"),
                _ => pageview("/x"),
            };
            match r
                .pipeline
                .process(&b, ctx(&format!("10.0.0.{i}"), "UA", t + i))
            {
                Outcome::Rejected(_) => rejected += 1,
                _ => processed += 1,
            }
        }
        assert_eq!(rejected, 8);
        let total: u64 = counts(&r.writer).values().sum();
        assert_eq!(total, processed, "ingest_counters 之和 = 非 400 的请求数");
        let mut accepted_rows = 0;
        while r.rx.try_recv().is_ok() {
            accepted_rows += 1;
        }
        assert_eq!(accepted_rows, 3, "缓冲容量 3，其余 pageview 为 overloaded");
    }
}
