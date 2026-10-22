//! 接入管线（Lite）：`POST /api/event` 的请求体到「已盖章的事件行」，只含内存里的查找和哈希，没有 IO。
//!
//! 顺序（`system-design.md` §4.2，先做便宜的判断）：
//! 解析（400）→ 客户端 IP → 站点查找（`unknown_site`）→ `collect_enabled`（`site_disabled`）→
//! hostname 白名单，空白名单 fail-closed（`hostname_not_allowed`）→ 身份（盐缺失 `salt_missing`；
//! 前一天的盐缺失只是不算 `prev_visitor_id`）→ 会话属性 → SessionAssigner（`no_session`）→
//! 组装 `EventRow` → 交给 BatchWriter（缓冲满则 `Overloaded`，同时计 `dropped_overloaded`）。
//!
//! 每个结果都计入 `ingest_counters`：`accepted` 或 `dropped_<原因>`；未知站点记在 `site_id = 0`。
//! 400 不进 `ingest_counters`，只进 Prometheus `rivu_ingest_rejected_total{field}`（§10 Q13）。
//!
//! 时间由调用方传入（`RequestCtx::now_ms`），所以测试可以把事件放到任意时刻。

mod snapshot;

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

use rivu_common::config::Cidr;
use rivu_domain::{DropReason, METRIC_ACCEPTED, NormalizedEvent, SessionAttrs, UNKNOWN_SITE_ID};
use rivu_session::assigner::{Assignment, Incoming, SessionAssigner, SessionConfig, Stamp};
use rivu_session::identity::{Salt, local_date, routing_hash, visitor_hash};
use rivu_storage_ch::rows::EventRow;
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
}

impl Pipeline {
    pub fn new(
        sites: Arc<SiteSnapshot>,
        salts: Arc<SaltCache>,
        writer: WriterHandle,
        trusted_proxies: Vec<Cidr>,
    ) -> Self {
        Pipeline {
            sites,
            salts,
            assigner: SessionAssigner::new(SessionConfig::default()),
            writer,
            trusted_proxies,
            routing_secret: rand::random::<Salt>(),
        }
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
        let ev = match parse_event(body) {
            Ok(ev) => ev,
            Err(errors) => {
                for field in errors.fields() {
                    metrics::counter!("rivu_ingest_rejected_total", "field" => field.to_owned())
                        .increment(1);
                }
                return Outcome::Rejected(errors);
            }
        };

        // 2. 客户端 IP
        let xff: Vec<&str> = ctx.xff.iter().map(String::as_str).collect();
        let ip = client_ip(ctx.peer, &xff, &self.trusted_proxies);

        // 3. 站点查找：未知站点没有 site_id，记在 0 上
        let Some(site) = self.sites.get(&ev.site_key) else {
            return self.dropped(UNKNOWN_SITE_ID, &ev, DropReason::UnknownSite);
        };
        let site_id = site.site_id;

        // 4. 站点停用
        if !site.collect_enabled {
            return self.dropped(site_id, &ev, DropReason::SiteDisabled);
        }

        // 5. hostname 白名单；空白名单 fail-closed（§10 Q12）
        if site.hostnames.is_empty() || !site.hostnames.matches(&ev.hostname) {
            return self.dropped(site_id, &ev, DropReason::HostnameNotAllowed);
        }

        // 6. 身份：按事件的站点本地日期选盐（§4.5）
        let date = local_date(ctx.now_ms, &site.timezone);
        let Some(salt) = self.salts.get(date) else {
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

        // 7. 会话属性（富化尚未实现的字段留空）
        let attrs = session_attrs(&ev, &site);

        // 8. 会话分配与盖章
        let stamp = match self.assigner.assign(&Incoming {
            site_id,
            visitor_id,
            prev_visitor_id,
            routing,
            ts_ms: ctx.now_ms,
            kind: ev.kind,
            pathname: &ev.pathname,
            attrs: &attrs,
        }) {
            Assignment::Stamped(stamp) => stamp,
            Assignment::NoSession => return self.dropped(site_id, &ev, DropReason::NoSession),
        };

        // 9. 组装行并交给 BatchWriter
        let row = build_row(site_id, ev.clone(), &stamp, ctx.now_ms);
        match self.writer.try_send(row) {
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
}

/// 会话属性：会话的第一个事件决定。
///
/// - referrer：host 命中站点白名单视为内部（不记来源）；否则 `referrer = stored`、
///   `referrer_source = referrer.host`（暂行，等来源库再分类）；
/// - channel 为空串；utm、click id 照抄；地理、UA 为空值（`country_code = [0, 0]`）。
fn session_attrs(ev: &NormalizedEvent, site: &SiteConfig) -> SessionAttrs {
    let (referrer, referrer_source) = match &ev.referrer {
        Some(r) if !site.hostnames.matches(&r.host) => (r.stored.clone(), r.host.clone()),
        _ => (String::new(), String::new()),
    };
    SessionAttrs {
        referrer,
        referrer_source,
        channel: String::new(),
        utm_source: ev.utm.source.clone().unwrap_or_default(),
        utm_medium: ev.utm.medium.clone().unwrap_or_default(),
        utm_campaign: ev.utm.campaign.clone().unwrap_or_default(),
        utm_content: ev.utm.content.clone().unwrap_or_default(),
        utm_term: ev.utm.term.clone().unwrap_or_default(),
        click_id_param: ev.click_id_param.unwrap_or_default().to_owned(),
        country_code: [0, 0],
        region_code: String::new(),
        city_geoname_id: 0,
        device: String::new(),
        browser: String::new(),
        browser_version: String::new(),
        os: String::new(),
        os_version: String::new(),
    }
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

    use jiff::civil::{Date, date};
    use jiff::tz::TimeZone;
    use serde_json::json;
    use tokio::sync::mpsc;

    use super::*;

    const KEY: &str = "aaaaaaaaaaaa";
    const OFF_KEY: &str = "dddddddddddd";
    const EMPTY_KEY: &str = "eeeeeeeeeeee";
    const SITE: u64 = 42;
    const OFF_SITE: u64 = 43;
    const EMPTY_SITE: u64 = 44;
    const MIN: i64 = 60_000;

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
        let site = |id: u64, collect: bool, hosts: &[&str]| {
            Arc::new(SiteConfig {
                site_id: id,
                timezone: berlin(),
                collect_enabled: collect,
                hostnames: crate::hostname::HostnamePatterns::new(hosts.iter().copied()),
            })
        };
        let sites = Arc::new(SiteSnapshot::new());
        sites.replace(HashMap::from([
            (KEY.to_owned(), site(SITE, true, &["example.test"])),
            (OFF_KEY.to_owned(), site(OFF_SITE, false, &["example.test"])),
            (EMPTY_KEY.to_owned(), site(EMPTY_SITE, true, &[])),
        ]));
        let salt_cache = Arc::new(SaltCache::new());
        salt_cache.replace(salts(&march(27..=31)));
        let (writer, rx) = WriterHandle::detached(capacity);
        let trusted = vec!["127.0.0.1/32".parse().unwrap()];
        let pipeline = Pipeline::new(sites, Arc::clone(&salt_cache), writer.clone(), trusted)
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
            ctx("203.0.113.9", "UA/1", t),
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
        assert_eq!(row.referrer_source, "news.example.org");
        assert_eq!(row.channel, "");
        assert_eq!(
            (row.utm_source.as_str(), row.utm_medium.as_str()),
            ("news", "email")
        );
        assert_eq!(row.click_id_param, "gclid");
        assert_eq!(row.country_code, [0, 0]);
        assert_eq!((row.device.as_str(), row.browser.as_str()), ("", ""));
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
        assert_eq!(
            (
                internal.referrer.as_str(),
                internal.referrer_source.as_str()
            ),
            ("", "")
        );
        assert_eq!(external.referrer, "news.example.org/p");
        assert_eq!(external.referrer_source, "news.example.org");
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
