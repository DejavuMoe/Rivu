//! 查询：核心指标（`overview`）与页面榜（`top_pages`），直接读 ClickHouse 的原始层（`events` + `sessions`）。
//! 口径见 `docs/architecture/system-design.md` §5.3（派生指标）、§6.5（指标定义）、§6.7（时间与时区）。
//!
//! P1 只有这两个函数，没有 QuerySpec / 规划器 / 缓存（P2）。约定：
//! - 站点本地日期范围 `[from, to]`（含）在 Rust 里用 jiff 换算成 UTC 毫秒区间 `[start, end)`，
//!   夏令时切换日（23 h / 25 h）和缺口都由 jiff 处理；
//! - 取值一律走服务端参数 `{name:Type}` + `.param()`，不拼接、不用客户端转义的 `.bind()`（§6.8）；
//! - 表名不带库名：调用方的客户端要用 `with_database` 绑定库（`rivu_storage_ch::client`）；
//! - 用 `rivu_reader` 账号（`readonly=1`）：只设 `log_comment` 与 `max_execution_time`，这两项在 profile 里放开了修改。

use anyhow::{Context, Result, bail};
use clickhouse::{Client, Row};
use jiff::Span;
use jiff::civil::Date;
use jiff::tz::TimeZone;
use serde::Deserialize;

/// 核心指标（§6.5）。
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Overview {
    /// `uniq(visitor_id)`，不含 engagement 事件（§10 Q17）。
    pub visitors: u64,
    /// 区间内活跃的会话数。
    pub visits: u64,
    pub pageviews: u64,
    /// 0–1。
    pub bounce_rate: f64,
    /// 秒，不含 engagement 时间（§10 Q2）。
    pub visit_duration: f64,
    /// 秒，只统计 `engagement_ms > 0` 的会话。
    pub engaged_duration: f64,
}

/// 页面榜的一行。
#[derive(Clone, Debug, PartialEq)]
pub struct PageStat {
    pub pathname: String,
    /// 浏览过该页的访客数。
    pub visitors: u64,
    pub pageviews: u64,
}

/// 站点本地日期范围换算出的参数。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Bounds {
    /// 区间起点（含），UTC 毫秒。
    pub start_ms: i64,
    /// 区间终点（不含），UTC 毫秒：`to` 的次日本地 00:00。
    pub end_ms: i64,
    /// 起点所在的 UTC 日期。
    pub start_date: Date,
    /// 终点前最后一毫秒所在的 UTC 日期。
    pub last_date: Date,
}

/// 把站点本地日期范围 `[from, to]`（含）换算成 UTC 区间。
///
/// 本地日期 `d` 的起点是 `d` 00:00（在站点时区）；落在 DST 缺口里（个别时区在午夜切换）时，
/// jiff 取缺口之后的第一个时刻。`to` 的终点取次日起点，所以 23 h / 25 h 的日子也恰好是一整个本地日。
pub fn bounds(tz: &TimeZone, from: Date, to: Date) -> Result<Bounds> {
    if from > to {
        bail!("起始日期 {from} 晚于结束日期 {to}");
    }
    let start = from
        .to_zoned(tz.clone())
        .with_context(|| format!("无法把 {from} 换算成时间戳"))?
        .timestamp();
    let next = to
        .checked_add(Span::new().days(1))
        .with_context(|| format!("{to} 之后没有下一天"))?;
    let end = next
        .to_zoned(tz.clone())
        .with_context(|| format!("无法把 {next} 换算成时间戳"))?
        .timestamp();
    let last = end
        .checked_sub(Span::new().milliseconds(1))
        .context("终点前一毫秒超出范围")?;
    Ok(Bounds {
        start_ms: start.as_millisecond(),
        end_ms: end.as_millisecond(),
        start_date: start.to_zoned(TimeZone::UTC).date(),
        last_date: last.to_zoned(TimeZone::UTC).date(),
    })
}

/// 把 `Bounds`、站点和执行时限绑到查询上。所有 SQL 共用同一组参数名。
/// 时限按调用方分三档（§6.8、§10 Q14），由调用方传入，不得大于 `rivu_reader` 配额里的上限。
fn bind(
    q: clickhouse::query::Query,
    site_id: u64,
    b: &Bounds,
    timeout_secs: u64,
    what: &str,
) -> clickhouse::query::Query {
    q.param("site", site_id)
        .param("from_date", b.start_date.to_string())
        .param("to_date", b.last_date.to_string())
        .param("from_ms", b.start_ms)
        .param("to_ms", b.end_ms)
        .with_setting("max_execution_time", timeout_secs.to_string())
        .with_setting(
            "log_comment",
            format!(r#"{{"route":"{what}","site":{site_id}}}"#),
        )
}

#[derive(Row, Deserialize)]
struct EventAgg {
    visitors: u64,
    pageviews: u64,
}

#[derive(Row, Deserialize)]
struct SessionAgg {
    visits: u64,
    bounce_rate: f64,
    visit_duration: f64,
    engaged_duration: f64,
}

/// `visitors` 与 `pageviews`：只读 `events`。排序键含 `toDate(ts)`，所以先按 UTC 日期裁剪，再按毫秒精确过滤。
const EVENTS_SQL: &str = "
SELECT uniqIf(visitor_id, name != 'engagement') AS visitors,
       countIf(name = 'pageview') AS pageviews
FROM events
WHERE site_id = {site:UInt64}
  AND toDate(ts) >= {from_date:Date} AND toDate(ts) <= {to_date:Date}
  AND ts >= fromUnixTimestamp64Milli({from_ms:Int64}, 'UTC')
  AND ts <  fromUnixTimestamp64Milli({to_ms:Int64}, 'UTC')";

/// 会话指标：两级聚合（§5.3 的代价）。
///
/// 内层按完整排序键 `(site_id, start_date, session_id)`（Q9）合并 AggregatingMergeTree 里尚未合并的部分行，并保留「区间内活跃」的会话：
/// `last_event_at ≥ 起点 AND started_at < 终点`；`start_date ≥ from_date − 7` 依赖 7 天的会话上限（§4.6、§6.5）。
/// 外层算比率，`ifNotFinite` 防除零。内层别名与列名刻意不同，避免聚合函数嵌套歧义。
const SESSIONS_SQL: &str = "
SELECT count() AS visits,
       ifNotFinite(countIf(s_pv <= 1 AND s_inter = 0) / count(), 0) AS bounce_rate,
       ifNotFinite(avg(dur_ms) / 1000, 0) AS visit_duration,
       ifNotFinite(sumIf(s_eng, s_eng > 0) / 1000 / countIf(s_eng > 0), 0) AS engaged_duration
FROM (
    SELECT site_id, start_date, session_id,
           min(started_at) AS s_start,
           max(last_event_at) AS s_last,
           sum(pageviews) AS s_pv,
           sum(interactive_events) AS s_inter,
           sum(engagement_ms) AS s_eng,
           toUnixTimestamp64Milli(max(last_event_at)) - toUnixTimestamp64Milli(min(started_at)) AS dur_ms
    FROM sessions
    WHERE site_id = {site:UInt64}
      AND start_date >= {from_date:Date} - 7 AND start_date <= {to_date:Date}
    GROUP BY site_id, start_date, session_id
    HAVING s_last >= fromUnixTimestamp64Milli({from_ms:Int64}, 'UTC')
       AND s_start <  fromUnixTimestamp64Milli({to_ms:Int64}, 'UTC')
)";

/// 站点本地日期范围 `[from, to]`（含）的核心指标。`reader` 用 `rivu_reader` 账号构造，
/// `timeout_secs` 是执行时限（看板 / API / 公开看板各一档，§10 Q14）。
pub async fn overview(
    reader: &Client,
    site_id: u64,
    tz: &TimeZone,
    from: Date,
    to: Date,
    timeout_secs: u64,
) -> Result<Overview> {
    let b = bounds(tz, from, to)?;
    let events = bind(
        reader.query(EVENTS_SQL),
        site_id,
        &b,
        timeout_secs,
        "overview.events",
    )
    .fetch_one::<EventAgg>();
    let sessions = bind(
        reader.query(SESSIONS_SQL),
        site_id,
        &b,
        timeout_secs,
        "overview.sessions",
    )
    .fetch_one::<SessionAgg>();
    let (e, s) = tokio::try_join!(events, sessions).context("查询 overview 失败")?;
    Ok(Overview {
        visitors: e.visitors,
        visits: s.visits,
        pageviews: e.pageviews,
        bounce_rate: s.bounce_rate,
        visit_duration: s.visit_duration,
        engaged_duration: s.engaged_duration,
    })
}

#[derive(Row, Deserialize)]
struct PageAgg {
    pathname: String,
    visitors: u64,
    pageviews: u64,
}

const TOP_PAGES_SQL: &str = "
SELECT pathname,
       uniq(visitor_id) AS visitors,
       count() AS pageviews
FROM events
WHERE site_id = {site:UInt64}
  AND name = 'pageview'
  AND toDate(ts) >= {from_date:Date} AND toDate(ts) <= {to_date:Date}
  AND ts >= fromUnixTimestamp64Milli({from_ms:Int64}, 'UTC')
  AND ts <  fromUnixTimestamp64Milli({to_ms:Int64}, 'UTC')
GROUP BY pathname
ORDER BY visitors DESC, pageviews DESC, pathname ASC
LIMIT {limit:UInt32}";

/// 页面榜：浏览过该页的访客数与浏览次数，按访客数降序（同数按浏览次数、路径）。
/// `timeout_secs` 同 [`overview`]。
pub async fn top_pages(
    reader: &Client,
    site_id: u64,
    tz: &TimeZone,
    from: Date,
    to: Date,
    limit: u32,
    timeout_secs: u64,
) -> Result<Vec<PageStat>> {
    let b = bounds(tz, from, to)?;
    let rows = bind(
        reader.query(TOP_PAGES_SQL),
        site_id,
        &b,
        timeout_secs,
        "top_pages",
    )
    .param("limit", limit)
    .fetch_all::<PageAgg>()
    .await
    .context("查询 top_pages 失败")?;
    Ok(rows
        .into_iter()
        .map(|r| PageStat {
            pathname: r.pathname,
            visitors: r.visitors,
            pageviews: r.pageviews,
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use jiff::civil::date;

    use super::*;

    const HOUR: i64 = 3_600_000;

    fn tz(name: &str) -> TimeZone {
        TimeZone::get(name).unwrap()
    }

    #[test]
    fn utc_day_is_24_hours() {
        let b = bounds(&TimeZone::UTC, date(2026, 3, 29), date(2026, 3, 29)).unwrap();
        assert_eq!(b.end_ms - b.start_ms, 24 * HOUR);
        assert_eq!(
            (b.start_date, b.last_date),
            (date(2026, 3, 29), date(2026, 3, 29))
        );
    }

    #[test]
    fn berlin_spring_forward_day_is_23_hours() {
        let berlin = tz("Europe/Berlin");
        let b = bounds(&berlin, date(2026, 3, 29), date(2026, 3, 29)).unwrap();
        assert_eq!(b.end_ms - b.start_ms, 23 * HOUR);
        // 本地 3/29 00:00 CET = 3/28 23:00 UTC；终点 3/30 00:00 CEST = 3/29 22:00 UTC
        assert_eq!(b.start_date, date(2026, 3, 28));
        assert_eq!(b.last_date, date(2026, 3, 29));
        // 前后两天正常
        let d28 = bounds(&berlin, date(2026, 3, 28), date(2026, 3, 28)).unwrap();
        let d30 = bounds(&berlin, date(2026, 3, 30), date(2026, 3, 30)).unwrap();
        assert_eq!(d28.end_ms - d28.start_ms, 24 * HOUR);
        assert_eq!(d30.end_ms - d30.start_ms, 24 * HOUR);
        // 相邻日期首尾相接，没有缝隙也没有重叠
        assert_eq!(d28.end_ms, b.start_ms);
        assert_eq!(b.end_ms, d30.start_ms);
    }

    #[test]
    fn berlin_fall_back_day_is_25_hours() {
        let berlin = tz("Europe/Berlin");
        let b = bounds(&berlin, date(2026, 10, 25), date(2026, 10, 25)).unwrap();
        assert_eq!(b.end_ms - b.start_ms, 25 * HOUR);
    }

    #[test]
    fn multi_day_range_spans_the_dst_change() {
        let berlin = tz("Europe/Berlin");
        let b = bounds(&berlin, date(2026, 3, 28), date(2026, 3, 30)).unwrap();
        assert_eq!(b.end_ms - b.start_ms, (24 + 23 + 24) * HOUR);
    }

    #[test]
    fn midnight_gap_zones_start_the_day_after_the_gap() {
        // 圣保罗 2018 年 11 月 4 日 00:00 跳到 01:00（DST 缺口在午夜）：这一天只有 23 小时
        let sp = tz("America/Sao_Paulo");
        let b = bounds(&sp, date(2018, 11, 4), date(2018, 11, 4)).unwrap();
        assert_eq!(b.end_ms - b.start_ms, 23 * HOUR);
    }

    #[test]
    fn reversed_range_is_an_error() {
        assert!(bounds(&TimeZone::UTC, date(2026, 3, 30), date(2026, 3, 29)).is_err());
    }

    #[test]
    fn sql_uses_server_side_parameters_only() {
        for sql in [EVENTS_SQL, SESSIONS_SQL, TOP_PAGES_SQL] {
            assert!(sql.contains("{site:UInt64}"));
            // `?` 是客户端 .bind() 的占位符，这里不允许出现
            assert!(!sql.contains('?'), "{sql}");
        }
    }
}
