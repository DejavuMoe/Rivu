//! 查询：核心指标（`overview`）与按维度分组（`breakdown`：页面、来源、渠道、国家、设备），
//! 直接读 ClickHouse 的原始层（`events` + `sessions`）。
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

/// 分组维度（P1：页面、来源、渠道、国家、设备类别）。维度名来自这个枚举，SQL 片段是写死的常量，
/// 不拼接任何用户输入（§6.8）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Dimension {
    /// `event:page`：`events.pathname`，只看 pageview。
    Page,
    /// `visit:source`：`referrer_source`。
    Source,
    /// `visit:channel`。
    Channel,
    /// `visit:country`：ISO 3166-1 alpha-2。
    Country,
    /// `visit:device`：desktop / mobile / tablet。
    Device,
}

/// 空值（来源、渠道、国家、设备）的展示文案，沿用 Plausible 的习惯
/// （`docs/research/plausible/03-stats-query-engine.md` §5.1）：来源 `Direct / None`、渠道 `Direct`、
/// 设备 `(not set)`；国家 Plausible 没有兜底文案（直接丢掉未知行），Rivu 保留这一行并按设备同样写 `(not set)`。
pub const NO_SOURCE: &str = "Direct / None";
pub const NO_CHANNEL: &str = "Direct";
pub const NOT_SET: &str = "(not set)";

impl Dimension {
    pub const ALL: [Dimension; 5] = [
        Dimension::Page,
        Dimension::Source,
        Dimension::Channel,
        Dimension::Country,
        Dimension::Device,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Dimension::Page => "page",
            Dimension::Source => "source",
            Dimension::Channel => "channel",
            Dimension::Country => "country",
            Dimension::Device => "device",
        }
    }

    /// 空值（库里存的是空串）的展示文案。
    pub fn empty_label(self) -> &'static str {
        match self {
            Dimension::Page => "(none)",
            Dimension::Source => NO_SOURCE,
            Dimension::Channel => NO_CHANNEL,
            Dimension::Country | Dimension::Device => NOT_SET,
        }
    }

    /// 结果是否含位置数据：含则调用方必须带上 `GeoDb::attribution()`（§4.4「署名要求」，`meta.geo_attribution` 必填）。
    pub fn needs_geo_attribution(self) -> bool {
        self == Dimension::Country
    }

    /// `events` 与 `sessions` 上同名的列表达式（取字符串）。`country_code` 是 `FixedString(2)`，
    /// `toString` 会去掉尾部的 NUL，所以未知（`\0\0`）得到空串。
    fn column_expr(self) -> &'static str {
        match self {
            Dimension::Page => "pathname",
            Dimension::Source => "toString(referrer_source)",
            Dimension::Channel => "toString(channel)",
            Dimension::Country => "toString(country_code)",
            Dimension::Device => "toString(device)",
        }
    }
}

impl std::str::FromStr for Dimension {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Dimension::ALL
            .into_iter()
            .find(|d| d.as_str() == s)
            .ok_or_else(|| {
                format!("`{s}` 不是有效的维度，可选 page | source | channel | country | device")
            })
    }
}

/// 分组结果的一行。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BreakdownRow {
    /// 维度值；空值已换成展示文案（[`Dimension::empty_label`]）。
    pub value: String,
    /// `uniqIf(visitor_id, name != 'engagement')`，只读 `events`（Q9、Q17）；`page` 维度只看 pageview。
    pub visitors: u64,
    /// 会话维度：区间内活跃、且（会话首事件的）该属性等于本行的会话数；
    /// `page` 维度：区间内浏览过该页的会话数（一个会话没有单一的页面，所以只能从 events 数）。
    pub visits: u64,
    pub pageviews: u64,
}

#[derive(Row, Deserialize)]
struct BreakdownAgg {
    v: String,
    visitors: u64,
    visits: u64,
    pageviews: u64,
}

/// 排序与截断：访客数、会话数、浏览次数降序，同数按（原始）维度值升序。
const BREAKDOWN_TAIL: &str = "
ORDER BY visitors DESC, visits DESC, pageviews DESC, v ASC
LIMIT {limit:UInt32}";

/// `events` 的时间裁剪：排序键含 `toDate(ts)`，先按 UTC 日期裁剪，再按毫秒精确过滤。
const EVENTS_RANGE: &str = "site_id = {site:UInt64}
      AND toDate(ts) >= {from_date:Date} AND toDate(ts) <= {to_date:Date}
      AND ts >= fromUnixTimestamp64Milli({from_ms:Int64}, 'UTC')
      AND ts <  fromUnixTimestamp64Milli({to_ms:Int64}, 'UTC')";

/// 生成某个维度的分组 SQL。
///
/// **会话属性维度**（来源、渠道、国家、设备）两张表各出一部分，`UNION ALL` 后按维度值合并：
/// - `events` 侧：`visitors`、`pageviews`，按事件上盖的属性分组（事件上的会话属性是会话首事件的值，在会话内恒定）；
/// - `sessions` 侧：`visits`，两级聚合（内层按排序键 `(site_id, start_date, session_id)` 合并未合并的部分行，
///   属性用 `any(...)`——会话内恒定，所以取任意一个就是首事件的值；保留「区间内活跃」的会话，口径同 [`overview`]），
///   外层按属性计数。
///
/// 合并而不用 JOIN：某个值可能只出现在一侧（会话开始于区间之前、区间内只有 engagement；或相反），
/// 另一侧按 0 计；三个数都是 0 的行（只有 engagement 事件的值）丢掉。排序和 `LIMIT` 在合并之后做，
/// 所以截断不会让某个值只剩一半的数字。
///
/// **页面维度**只读 `events`（pageview）：一个会话没有单一的页面，`visits` 是浏览过该页的会话数。
///
/// 内层别名与列名刻意不同（`c_*`、`s_*`）：外层 `sum(c_x) AS x` 若与内层列同名，会被当成聚合函数嵌套。
fn breakdown_sql(dim: Dimension) -> String {
    let expr = dim.column_expr();
    if dim == Dimension::Page {
        return format!(
            "SELECT {expr} AS v,
       uniq(visitor_id) AS visitors,
       uniq(session_id) AS visits,
       count() AS pageviews
FROM events
WHERE {EVENTS_RANGE}
  AND name = 'pageview'
GROUP BY v{BREAKDOWN_TAIL}"
        );
    }
    format!(
        "SELECT v,
       sum(c_visitors) AS visitors,
       sum(c_visits) AS visits,
       sum(c_pageviews) AS pageviews
FROM (
    SELECT {expr} AS v,
           uniqIf(visitor_id, name != 'engagement') AS c_visitors,
           toUInt64(0) AS c_visits,
           countIf(name = 'pageview') AS c_pageviews
    FROM events
    WHERE {EVENTS_RANGE}
    GROUP BY v
    UNION ALL
    SELECT s_v AS v,
           toUInt64(0) AS c_visitors,
           count() AS c_visits,
           toUInt64(0) AS c_pageviews
    FROM (
        SELECT site_id, start_date, session_id,
               any({expr}) AS s_v,
               min(started_at) AS s_start,
               max(last_event_at) AS s_last
        FROM sessions
        WHERE site_id = {{site:UInt64}}
          AND start_date >= {{from_date:Date}} - 7 AND start_date <= {{to_date:Date}}
        GROUP BY site_id, start_date, session_id
        HAVING s_last >= fromUnixTimestamp64Milli({{from_ms:Int64}}, 'UTC')
           AND s_start <  fromUnixTimestamp64Milli({{to_ms:Int64}}, 'UTC')
    )
    GROUP BY v
)
GROUP BY v
HAVING visitors > 0 OR visits > 0 OR pageviews > 0{BREAKDOWN_TAIL}"
    )
}

/// 站点本地日期范围 `[from, to]`（含）内按维度分组：每个维度值的访客数、会话数、浏览次数，
/// 按访客数降序，最多 `limit` 行。`timeout_secs` 同 [`overview`]。
///
/// 空值（库里是空串）换成展示文案（[`Dimension::empty_label`]）。基数不受控的维度（页面）要靠 `limit` 截断。
/// 维度含国家时（[`Dimension::needs_geo_attribution`]），调用方必须同时给出 `GeoDb::attribution()`。
#[allow(clippy::too_many_arguments)] // 签名由任务书规定：与 `overview` 的参数顺序一致，再加维度与行数
pub async fn breakdown(
    reader: &Client,
    site_id: u64,
    tz: &TimeZone,
    from: Date,
    to: Date,
    dim: Dimension,
    limit: u32,
    timeout_secs: u64,
) -> Result<Vec<BreakdownRow>> {
    let b = bounds(tz, from, to)?;
    let sql = breakdown_sql(dim);
    let rows = bind(
        reader.query(&sql),
        site_id,
        &b,
        timeout_secs,
        &format!("breakdown.{}", dim.as_str()),
    )
    .param("limit", limit)
    .fetch_all::<BreakdownAgg>()
    .await
    .with_context(|| format!("查询 breakdown({}) 失败", dim.as_str()))?;
    Ok(rows
        .into_iter()
        .map(|r| BreakdownRow {
            value: if r.v.is_empty() {
                dim.empty_label().to_owned()
            } else {
                r.v
            },
            visitors: r.visitors,
            visits: r.visits,
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
        let breakdowns: Vec<String> = Dimension::ALL.into_iter().map(breakdown_sql).collect();
        let fixed = [EVENTS_SQL, SESSIONS_SQL];
        for sql in fixed
            .iter()
            .copied()
            .chain(breakdowns.iter().map(String::as_str))
        {
            assert!(sql.contains("{site:UInt64}"));
            // `?` 是客户端 .bind() 的占位符，这里不允许出现
            assert!(!sql.contains('?'), "{sql}");
        }
        for (dim, sql) in Dimension::ALL.into_iter().zip(&breakdowns) {
            // 所有取值（站点、日期、毫秒区间、行数）都走参数
            for p in [
                "{site:UInt64}",
                "{from_date:Date}",
                "{to_date:Date}",
                "{from_ms:Int64}",
                "{to_ms:Int64}",
                "{limit:UInt32}",
            ] {
                assert!(sql.contains(p), "{dim:?} 缺少 {p}:\n{sql}");
            }
            // 没有残留的格式化占位符：片段都是写死的常量
            assert!(!sql.contains("{{") && !sql.contains("}}"), "{sql}");
        }
    }

    #[test]
    fn session_dimensions_merge_events_and_sessions_and_page_reads_events_only() {
        for dim in Dimension::ALL {
            let sql = breakdown_sql(dim);
            if dim == Dimension::Page {
                assert!(!sql.contains("FROM sessions"), "{sql}");
                assert!(sql.contains("name = 'pageview'"), "{sql}");
                continue;
            }
            // 两张表各一部分，visitors 排除 engagement（Q9、Q17），visits 来自两级聚合，截断在合并之后
            assert!(
                sql.contains("FROM events") && sql.contains("FROM sessions"),
                "{sql}"
            );
            assert!(
                sql.contains("uniqIf(visitor_id, name != 'engagement')"),
                "{sql}"
            );
            assert!(
                sql.contains("GROUP BY site_id, start_date, session_id"),
                "{sql}"
            );
            assert!(sql.contains("UNION ALL"), "{sql}");
            assert!(
                sql.find("LIMIT").unwrap() > sql.rfind("UNION ALL").unwrap(),
                "{sql}"
            );
        }
    }

    #[test]
    fn dimensions_parse_and_label_empty_values() {
        for d in Dimension::ALL {
            assert_eq!(d.as_str().parse::<Dimension>(), Ok(d));
        }
        assert!("browser".parse::<Dimension>().is_err());
        assert_eq!(Dimension::Source.empty_label(), "Direct / None");
        assert_eq!(Dimension::Channel.empty_label(), "Direct");
        assert_eq!(Dimension::Device.empty_label(), "(not set)");
        assert_eq!(Dimension::Country.empty_label(), "(not set)");
        assert!(Dimension::Country.needs_geo_attribution());
        assert!(!Dimension::Device.needs_geo_attribution());
    }
}
