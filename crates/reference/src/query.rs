//! `QuerySpec` 的参考实现：独立于 `rivu-query`，只从原始事件按口径直接计算（`docs/brief-p1.md`「方法」）。
//!
//! 它**不复用** `rivu-query` 的任何代码、类型或 SQL：请求按 JSON 自己解析，日期范围、时间桶、过滤求值、
//! 指标定义、排序分页、对比期都在这里再实现一遍，写法是最直白的 O(n) 遍历。管线（真实的 ClickHouse）与它逐项比较。
//!
//! 口径来源：`docs/architecture/system-design.md` §6.2–§6.7，以及 `rivu-query` 各模块文档里写明的约定：
//!
//! - 事件级的一切（维度、过滤、`visitors` `pageviews` `events`）只看**非 engagement** 事件；`time_on_page`、
//!   `scroll_depth` 只看 engagement 事件。没有并入任何会话的事件（被丢弃的 engagement）不存在。
//! - 会话属性（来源、渠道、UTM、位置、设备）取**会话首事件**自己的属性，同一会话里后来的事件的这些字段被忽略。
//! - `visitors` = 范围内不同的（站点本地日, 人）数，即各本地日访客数之和（§10 Q9）。
//! - 会话「活跃」= `last_event_at ≥ 起点 且 started_at < 终点`。会话在时间维度上归入 `max(started_at, 起点)` 的桶。
//! - 过滤树在事件行上逐行求值；在会话行上求值时，事件级叶子是「会话在范围内有满足该叶子的非 engagement 事件」；
//!   `bounce_rate` 再要求把 `event:page` / `event:hostname` 叶子换成入口页之后的树也成立。
//! - `visits`：有 `event:*` 维度时是「含该维度值的事件所属的不同会话数」，否则是「区间内活跃的会话数」。
//! - 所有区间的终点封顶到 `now + 1 ms`。
//!
//! 通配符用手写的 glob 匹配（不转正则）；`matches` 用 Rust 的 `regex`（线性时间，与 Hyperscan 在测试用的语法子集上一致）。

use std::collections::{BTreeMap, BTreeSet, HashSet};

use jiff::civil::{Date, Weekday};
use jiff::tz::TimeZone;
use jiff::{Span, Timestamp};
use regex::RegexBuilder;
use rivu_domain::EventKind;
use serde_json::Value;

use crate::{EventAttrs, RawEvent, RefSession, sessionize};

/// 会话属性的来源字段（事件自己的值；只有会话首事件的值有意义）。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RefAttrs {
    pub referrer: String,
    pub referrer_source: String,
    pub channel: String,
    pub utm_source: String,
    pub utm_medium: String,
    pub utm_campaign: String,
    pub utm_content: String,
    pub utm_term: String,
    /// ISO 3166-1 alpha-2，空串为未知。
    pub country: String,
    pub region: String,
    /// GeoNames id，0 为未知。
    pub city: u32,
    pub device: String,
    pub browser: String,
    pub browser_version: String,
    pub os: String,
    pub os_version: String,
}

/// 一条被管线接受（或会被丢弃的 engagement）的原始事件。
#[derive(Clone, Debug)]
pub struct RefEvent {
    pub site_id: u64,
    pub person: u64,
    pub ts_ms: i64,
    /// `pageview`、`engagement` 或自定义事件名。
    pub name: String,
    pub hostname: String,
    pub pathname: String,
    pub interactive: bool,
    pub engagement_ms: u32,
    /// 255 表示没有。
    pub scroll_depth: u8,
    pub attrs: RefAttrs,
}

impl RefEvent {
    fn kind(&self) -> EventKind {
        EventKind::from_name(&self.name)
    }

    fn to_raw(&self) -> RawEvent {
        RawEvent {
            site_id: self.site_id,
            person: self.person,
            ts_ms: self.ts_ms,
            kind: self.kind(),
            hostname: self.hostname.clone(),
            pathname: self.pathname.clone(),
            interactive: self.interactive,
            engagement_ms: self.engagement_ms,
            attrs: EventAttrs::default(),
        }
    }
}

/// 站点上下文与「现在」。
#[derive(Clone, Debug)]
pub struct RefSite {
    pub site_id: u64,
    pub tz: TimeZone,
    pub week_start: Weekday,
    pub now_ms: i64,
}

/// 一行结果。`raw` 是未换成展示文案的维度值（排序用），`dimensions` 是展示文案。
#[derive(Clone, Debug, PartialEq)]
pub struct RefRow {
    pub raw: Vec<String>,
    pub dimensions: Vec<String>,
    pub metrics: Vec<f64>,
    /// 没有时间维度的对比：对比期的指标与变化。
    pub comparison: Option<(Vec<f64>, Vec<f64>)>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct RefResult {
    pub rows: Vec<RefRow>,
    pub time_labels: Option<Vec<String>>,
    pub comparison_time_labels: Option<Vec<String>>,
    /// 有时间维度的对比：对比期的行 `(展示维度, 指标)`。
    pub comparison_rows: Option<Vec<(Vec<String>, Vec<f64>)>>,
    pub total_rows: Option<u64>,
}

/// 每个事件是否并入了某个会话（被丢弃的 engagement 没有）。`events` 必须属于同一个站点。
pub fn owned(events: &[RefEvent]) -> Vec<bool> {
    let raws: Vec<RawEvent> = events.iter().map(RefEvent::to_raw).collect();
    let mut flags = vec![false; events.len()];
    for s in sessionize(&raws) {
        for i in s.event_indices {
            flags[i] = true;
        }
    }
    flags
}

/// 指标是否是整数计数。
pub fn is_count_metric(name: &str) -> bool {
    matches!(name, "visitors" | "visits" | "pageviews" | "events")
}

// ------------------------------------------------------------------------------------------
// 解析
// ------------------------------------------------------------------------------------------

#[derive(Clone, Debug)]
struct Leaf {
    op: String,
    dim: String,
    values: Vec<String>,
    case_sensitive: bool,
}

#[derive(Clone, Debug)]
enum Node {
    And(Vec<Node>),
    Or(Vec<Node>),
    Not(Box<Node>),
    Leaf(Leaf),
}

fn parse_node(v: &Value) -> Result<Node, String> {
    let a = v.as_array().ok_or("filter must be an array")?;
    let op = a.first().and_then(Value::as_str).ok_or("filter op")?;
    match op {
        "and" | "or" => {
            let c = a[1]
                .as_array()
                .ok_or("children")?
                .iter()
                .map(parse_node)
                .collect::<Result<Vec<_>, _>>()?;
            Ok(if op == "and" {
                Node::And(c)
            } else {
                Node::Or(c)
            })
        }
        "not" => Ok(Node::Not(Box::new(parse_node(&a[1])?))),
        _ => {
            let values = a[2]
                .as_array()
                .ok_or("values")?
                .iter()
                .map(|x| match x {
                    Value::String(s) => Ok(s.clone()),
                    Value::Number(n) => Ok(n.to_string()),
                    _ => Err("value type".to_owned()),
                })
                .collect::<Result<Vec<_>, _>>()?;
            let case_sensitive = a
                .get(3)
                .and_then(|m| m.get("case_sensitive"))
                .and_then(Value::as_bool)
                .unwrap_or(true);
            Ok(Node::Leaf(Leaf {
                op: op.to_owned(),
                dim: a[1].as_str().ok_or("dim")?.to_owned(),
                values,
                case_sensitive,
            }))
        }
    }
}

#[derive(Clone, Debug)]
enum CompareSpec {
    Previous,
    YearOverYear,
    Custom(Value),
}

#[derive(Clone, Debug)]
struct Spec {
    metrics: Vec<String>,
    range: Value,
    dims: Vec<String>,
    filter: Option<Node>,
    order: Vec<(String, bool)>,
    want_time_labels: bool,
    want_total_rows: bool,
    compare: Option<CompareSpec>,
    limit: usize,
    offset: usize,
}

fn parse_spec(v: &Value) -> Result<Spec, String> {
    let strs = |key: &str| -> Vec<String> {
        v.get(key)
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default()
    };
    let filters = v
        .get("filters")
        .and_then(Value::as_array)
        .map(|a| a.iter().map(parse_node).collect::<Result<Vec<_>, _>>())
        .transpose()?
        .unwrap_or_default();
    let filter = match filters.len() {
        0 => None,
        1 => filters.into_iter().next(),
        _ => Some(Node::And(filters)),
    };
    let order = v
        .get("order_by")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|o| {
                    let o = o.as_array()?;
                    Some((o[0].as_str()?.to_owned(), o[1].as_str()? == "asc"))
                })
                .collect()
        })
        .unwrap_or_default();
    let include = v.get("include");
    let flag = |k: &str| {
        include
            .and_then(|i| i.get(k))
            .and_then(Value::as_bool)
            .unwrap_or(false)
    };
    let compare = include
        .and_then(|i| i.get("compare"))
        .map(|c| match c.as_str() {
            Some("previous_period") => CompareSpec::Previous,
            Some("year_over_year") => CompareSpec::YearOverYear,
            _ => CompareSpec::Custom(c.clone()),
        });
    let pag = v.get("pagination");
    Ok(Spec {
        metrics: strs("metrics"),
        range: v.get("date_range").cloned().ok_or("date_range")?,
        dims: strs("dimensions"),
        filter,
        order,
        want_time_labels: flag("time_labels"),
        want_total_rows: flag("total_rows"),
        compare,
        limit: pag
            .and_then(|p| p.get("limit"))
            .and_then(Value::as_u64)
            .unwrap_or(10_000) as usize,
        offset: pag
            .and_then(|p| p.get("offset"))
            .and_then(Value::as_u64)
            .unwrap_or(0) as usize,
    })
}

// ------------------------------------------------------------------------------------------
// 时间
// ------------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug)]
struct Iv {
    start: i64,
    end: i64,
    /// 整本地日区间的首末日。
    days: Option<(Date, Date)>,
}

struct Clock<'a> {
    site: &'a RefSite,
}

impl Clock<'_> {
    fn zoned(&self, ms: i64) -> jiff::Zoned {
        Timestamp::from_millisecond(ms)
            .expect("毫秒在 jiff 范围内")
            .to_zoned(self.site.tz.clone())
    }

    fn date(&self, ms: i64) -> Date {
        self.zoned(ms).date()
    }

    fn midnight(&self, d: Date) -> i64 {
        d.to_zoned(self.site.tz.clone())
            .expect("日期在范围内")
            .timestamp()
            .as_millisecond()
    }

    fn day_iv(&self, from: Date, to: Date) -> Iv {
        Iv {
            start: self.midnight(from),
            end: self.midnight(to.tomorrow().expect("日期在范围内")),
            days: Some((from, to)),
        }
    }

    fn cap(&self, mut iv: Iv) -> Iv {
        iv.end = iv.end.min(self.site.now_ms + 1);
        iv.end = iv.end.max(iv.start);
        iv
    }

    fn parse_custom(&self, arr: &[Value]) -> Result<Iv, String> {
        let a = arr[0].as_str().ok_or("range bound")?;
        let b = arr[1].as_str().ok_or("range bound")?;
        if a.len() == 10 {
            let (from, to): (Date, Date) = (
                a.parse().map_err(|e| format!("{e}"))?,
                b.parse().map_err(|e| format!("{e}"))?,
            );
            return Ok(self.day_iv(from, to));
        }
        let ts = |s: &str| -> Result<i64, String> {
            if let Ok(t) = s.parse::<Timestamp>() {
                return Ok(t.as_millisecond());
            }
            s.parse::<jiff::Zoned>()
                .map(|z| z.timestamp().as_millisecond())
                .map_err(|e| format!("{e}"))
        };
        Ok(Iv {
            start: ts(a)?,
            end: ts(b)?,
            days: None,
        })
    }

    /// `all` 的 `data_start`：站点最早事件的时间。
    fn resolve(&self, v: &Value, data_start: Option<i64>) -> Result<Iv, String> {
        let now = self.site.now_ms;
        let today = self.date(now);
        let iv = match v {
            Value::Array(arr) => self.parse_custom(arr)?,
            Value::String(s) => match s.as_str() {
                "day" => self.day_iv(today, today),
                "24h" => Iv {
                    start: now - 86_400_000,
                    end: now + 1,
                    days: None,
                },
                "realtime" => Iv {
                    start: now - 300_000,
                    end: now + 1,
                    days: None,
                },
                "month" => self.day_iv(today.first_of_month(), today),
                "year" => self.day_iv(today.first_of_year(), today),
                "all" => {
                    let from = data_start
                        .map(|ms| self.date(ms).min(today))
                        .unwrap_or(today);
                    self.day_iv(from, today)
                }
                other => {
                    if let Some(n) = other.strip_suffix("mo") {
                        let n: i64 = n.parse().map_err(|_| "months")?;
                        let this_month = today.first_of_month();
                        let from = this_month
                            .checked_sub(Span::new().months(n))
                            .map_err(|e| e.to_string())?;
                        self.day_iv(from, this_month.yesterday().map_err(|e| e.to_string())?)
                    } else if let Some(n) = other.strip_suffix('d') {
                        let n: i64 = n.parse().map_err(|_| "days")?;
                        let from = today
                            .checked_sub(Span::new().days(n))
                            .map_err(|e| e.to_string())?;
                        self.day_iv(from, today.yesterday().map_err(|e| e.to_string())?)
                    } else {
                        return Err(format!("unknown range {other}"));
                    }
                }
            },
            _ => return Err("range".into()),
        };
        Ok(self.cap(iv))
    }

    fn compare_iv(&self, c: &CompareSpec, main: &Iv) -> Result<Iv, String> {
        let iv = match c {
            CompareSpec::Custom(v) => self.parse_custom(v.as_array().ok_or("compare")?)?,
            CompareSpec::Previous | CompareSpec::YearOverYear => match main.days {
                Some((from, to)) => {
                    let today = self.date(self.site.now_ms);
                    let last = if to > today && from <= today {
                        today
                    } else {
                        to
                    };
                    let n = i64::from((last - from).get_days()) + 1;
                    let (f2, t2) = if matches!(c, CompareSpec::Previous) {
                        (
                            from.checked_sub(Span::new().days(n))
                                .map_err(|e| e.to_string())?,
                            from.yesterday().map_err(|e| e.to_string())?,
                        )
                    } else {
                        let f = from
                            .checked_sub(Span::new().years(1))
                            .map_err(|e| e.to_string())?;
                        (
                            f,
                            f.checked_add(Span::new().days(n - 1))
                                .map_err(|e| e.to_string())?,
                        )
                    };
                    self.day_iv(f2, t2)
                }
                None => {
                    let len = main.end - main.start;
                    let start = if matches!(c, CompareSpec::Previous) {
                        main.start - len
                    } else {
                        self.zoned(main.start)
                            .checked_sub(Span::new().years(1))
                            .map_err(|e| e.to_string())?
                            .timestamp()
                            .as_millisecond()
                    };
                    Iv {
                        start,
                        end: start + len,
                        days: None,
                    }
                }
            },
        };
        Ok(self.cap(iv))
    }

    /// 自动粒度：按区间覆盖的本地日数。
    fn auto_gran(&self, iv: &Iv) -> &'static str {
        let days = if iv.end <= iv.start {
            1
        } else {
            i64::from((self.date(iv.end - 1) - self.date(iv.start)).get_days()) + 1
        };
        if days <= 2 {
            "hour"
        } else if days <= 40 {
            "day"
        } else if days <= 364 {
            "week"
        } else {
            "month"
        }
    }

    fn week_floor(&self, d: Date) -> Date {
        let dow = i64::from(d.weekday().to_monday_one_offset());
        let ws = i64::from(self.site.week_start.to_monday_one_offset());
        d.checked_sub(Span::new().days((dow - ws).rem_euclid(7)))
            .expect("周起始日在范围内")
    }

    fn fmt_hour(&self, ms: i64) -> String {
        let z = self.zoned(ms);
        let off = z.offset().seconds();
        let sign = if off < 0 { '-' } else { '+' };
        let off = off.abs();
        format!(
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}{sign}{:02}:{:02}",
            z.year(),
            z.month(),
            z.day(),
            z.hour(),
            z.minute(),
            z.second(),
            off / 3600,
            (off % 3600) / 60
        )
    }

    /// 桶的原始值（排序与分组用）。
    fn bucket(&self, gran: &str, ms: i64) -> String {
        let z = self.zoned(ms);
        match gran {
            "minute" => {
                let into = i64::from(z.second()) * 1000 + i64::from(z.millisecond());
                ((ms - into).div_euclid(1000)).to_string()
            }
            "hour" => {
                let into = (i64::from(z.minute()) * 60 + i64::from(z.second())) * 1000
                    + i64::from(z.millisecond());
                ((ms - into).div_euclid(1000)).to_string()
            }
            "day" => z.date().to_string(),
            "week" => self.week_floor(z.date()).to_string(),
            _ => z.date().first_of_month().to_string(),
        }
    }

    /// 桶的展示值。
    fn bucket_label(&self, gran: &str, raw: &str) -> String {
        if gran == "hour" || gran == "minute" {
            self.fmt_hour(raw.parse::<i64>().expect("小时 / 分钟桶是秒数") * 1000)
        } else {
            raw.to_owned()
        }
    }

    fn labels(&self, gran: &str, iv: &Iv) -> Vec<String> {
        let mut out = Vec::new();
        if iv.end <= iv.start {
            return out;
        }
        if gran == "hour" || gran == "minute" {
            let step = if gran == "hour" { 3_600_000 } else { 60_000 };
            let first: i64 = self.bucket(gran, iv.start).parse().expect("秒数");
            let mut t = first * 1000;
            while t < iv.end {
                out.push(self.fmt_hour(t));
                t += step;
            }
            return out;
        }
        let first_day = self.date(iv.start);
        let mut d = match gran {
            "week" => self.week_floor(first_day),
            "month" => first_day.first_of_month(),
            _ => first_day,
        };
        while self.midnight(d) < iv.end {
            out.push(d.to_string());
            d = match gran {
                "week" => d.checked_add(Span::new().days(7)).expect("范围内"),
                "month" => d.checked_add(Span::new().months(1)).expect("范围内"),
                _ => d.tomorrow().expect("范围内"),
            };
        }
        out
    }
}

// ------------------------------------------------------------------------------------------
// 过滤叶子
// ------------------------------------------------------------------------------------------

fn empty_label(dim: &str) -> &'static str {
    match dim {
        "visit:source" | "visit:referrer" => "Direct / None",
        "visit:channel" => "Direct",
        "event:name" | "event:page" | "event:hostname" | "visit:entry_page" | "visit:exit_page" => {
            "(none)"
        }
        _ => "(not set)",
    }
}

/// 手写的 glob：`*`（连续的 `*` 等同一个）匹配任意字符串，其余字符按字面匹配，整串匹配。
fn glob_match(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    // dp[j]：pattern 的前 i 个字符能否匹配 text 的前 j 个字符
    let mut dp = vec![false; t.len() + 1];
    dp[0] = true;
    for &pc in &p {
        let mut next = vec![false; t.len() + 1];
        if pc == '*' {
            let mut any = false;
            for j in 0..=t.len() {
                any = any || dp[j];
                next[j] = any;
            }
        } else {
            for j in 1..=t.len() {
                next[j] = dp[j - 1] && t[j - 1] == pc;
            }
        }
        dp = next;
    }
    dp[t.len()]
}

fn leaf_test(leaf: &Leaf, raw: &str, city: u32) -> bool {
    let cs = leaf.case_sensitive;
    let fold = |s: &str| {
        if cs {
            s.to_owned()
        } else {
            s.to_ascii_lowercase()
        }
    };
    let hit = match leaf.op.trim_end_matches("_not") {
        "is" => {
            if leaf.dim == "visit:city" {
                leaf.values.iter().any(|v| v.parse::<u32>() == Ok(city))
            } else {
                let label = empty_label(&leaf.dim);
                let target = fold(raw);
                leaf.values.iter().any(|v| {
                    let is_label = if cs {
                        v == label
                    } else {
                        v.eq_ignore_ascii_case(label)
                    };
                    let v = if is_label { "" } else { v.as_str() };
                    fold(v) == target
                })
            }
        }
        "contains" => {
            let hay = fold(raw);
            leaf.values.iter().any(|n| hay.contains(&fold(n)))
        }
        "matches" => leaf.values.iter().any(|p| {
            let pat = format!("(?s){}{p}", if cs { "" } else { "(?i)" });
            RegexBuilder::new(&pat)
                .build()
                .expect("测试只生成合法的正则")
                .is_match(raw)
        }),
        "matches_wildcard" => {
            let text = fold(raw);
            leaf.values.iter().any(|p| glob_match(&fold(p), &text))
        }
        other => panic!("未知操作符 {other}"),
    };
    if leaf.op.ends_with("_not") { !hit } else { hit }
}

// ------------------------------------------------------------------------------------------
// 模型
// ------------------------------------------------------------------------------------------

struct Model<'a> {
    ev: &'a [RefEvent],
    sess: Vec<RefSession>,
    /// 事件 → 会话
    owner: Vec<Option<usize>>,
    clock: Clock<'a>,
}

#[derive(Default, Clone)]
struct Acc {
    visitors: HashSet<(Date, u64)>,
    pageviews: u64,
    events: u64,
    ev_sessions: BTreeSet<usize>,
    s_n: u64,
    s_pv: u64,
    s_dur_ms: i128,
    s_eng_ms: u64,
    s_eng_n: u64,
    b_n: u64,
    bounces: u64,
    exits: u64,
    /// engagement：会话 → (时长, 有效滚动深度的最大值, 有效滚动事件数)
    eng: BTreeMap<usize, (u64, u8, u64)>,
}

impl Model<'_> {
    fn attrs(&self, s: usize) -> &RefAttrs {
        &self.ev[self.sess[s].event_indices[0]].attrs
    }

    fn entry(&self, s: usize) -> (&str, &str) {
        self.sess[s]
            .entry
            .as_ref()
            .map(|(h, p)| (h.as_str(), p.as_str()))
            .unwrap_or(("", ""))
    }

    fn exit_path(&self, s: usize) -> &str {
        self.sess[s]
            .exit
            .as_ref()
            .map(|(_, p)| p.as_str())
            .unwrap_or("")
    }

    /// 会话级维度的原始值（visit:*）；`event:*` 不在这里。
    fn visit_value(&self, dim: &str, s: usize) -> (String, u32) {
        let a = self.attrs(s);
        let v = match dim {
            "visit:entry_page" => self.entry(s).1.to_owned(),
            "visit:exit_page" => self.exit_path(s).to_owned(),
            "visit:source" => a.referrer_source.clone(),
            "visit:channel" => a.channel.clone(),
            "visit:referrer" => a.referrer.clone(),
            "visit:utm_source" => a.utm_source.clone(),
            "visit:utm_medium" => a.utm_medium.clone(),
            "visit:utm_campaign" => a.utm_campaign.clone(),
            "visit:utm_content" => a.utm_content.clone(),
            "visit:utm_term" => a.utm_term.clone(),
            "visit:country" => a.country.clone(),
            "visit:region" => a.region.clone(),
            "visit:city" => {
                if a.city == 0 {
                    String::new()
                } else {
                    a.city.to_string()
                }
            }
            "visit:device" => a.device.clone(),
            "visit:browser" => a.browser.clone(),
            "visit:browser_version" => a.browser_version.clone(),
            "visit:os" => a.os.clone(),
            "visit:os_version" => a.os_version.clone(),
            other => panic!("未知会话维度 {other}"),
        };
        (v, a.city)
    }

    fn event_value(&self, dim: &str, i: usize) -> String {
        let e = &self.ev[i];
        match dim {
            "event:name" => e.name.clone(),
            "event:page" => e.pathname.clone(),
            "event:hostname" => e.hostname.clone(),
            _ => panic!("未知事件维度 {dim}"),
        }
    }

    /// 在事件行 `i` 上求过滤树。
    fn eval_event(&self, n: &Node, i: usize) -> bool {
        match n {
            Node::And(c) => c.iter().all(|x| self.eval_event(x, i)),
            Node::Or(c) => c.iter().any(|x| self.eval_event(x, i)),
            Node::Not(x) => !self.eval_event(x, i),
            Node::Leaf(l) => {
                if l.dim.starts_with("event:") {
                    leaf_test(l, &self.event_value(&l.dim, i), 0)
                } else {
                    let s = self.owner[i].expect("参与求值的事件都有会话");
                    let (v, city) = self.visit_value(&l.dim, s);
                    leaf_test(l, &v, city)
                }
            }
        }
    }

    /// 在会话行 `s` 上求过滤树。`entry`：`event:page` / `event:hostname` 叶子按入口页判定。
    fn eval_session(&self, n: &Node, s: usize, entry: bool, iv: &Iv) -> bool {
        match n {
            Node::And(c) => c.iter().all(|x| self.eval_session(x, s, entry, iv)),
            Node::Or(c) => c.iter().any(|x| self.eval_session(x, s, entry, iv)),
            Node::Not(x) => !self.eval_session(x, s, entry, iv),
            Node::Leaf(l) => {
                if l.dim.starts_with("event:") {
                    if entry && l.dim == "event:page" {
                        return leaf_test(l, self.entry(s).1, 0);
                    }
                    if entry && l.dim == "event:hostname" {
                        return leaf_test(l, self.entry(s).0, 0);
                    }
                    self.sess[s].event_indices.iter().any(|&i| {
                        let e = &self.ev[i];
                        e.kind() != EventKind::Engagement
                            && e.ts_ms >= iv.start
                            && e.ts_ms < iv.end
                            && leaf_test(l, &self.event_value(&l.dim, i), 0)
                    })
                } else {
                    let (v, city) = self.visit_value(&l.dim, s);
                    leaf_test(l, &v, city)
                }
            }
        }
    }

    /// 事件行上的维度原始值。
    fn event_dims(&self, dims: &[String], i: usize, gran: &str) -> Vec<String> {
        let s = self.owner[i].expect("有会话");
        dims.iter()
            .map(|d| {
                if d.starts_with("time") {
                    self.clock.bucket(gran, self.ev[i].ts_ms)
                } else if d.starts_with("event:") {
                    self.event_value(d, i)
                } else {
                    self.visit_value(d, s).0
                }
            })
            .collect()
    }

    /// 会话行上的维度原始值。`exit`：`event:page` 取退出页，否则取入口页。
    fn session_dims(
        &self,
        dims: &[String],
        s: usize,
        gran: &str,
        iv: &Iv,
        exit: bool,
    ) -> Vec<String> {
        dims.iter()
            .map(|d| match d.as_str() {
                d if d.starts_with("time") => self
                    .clock
                    .bucket(gran, self.sess[s].started_at_ms.max(iv.start)),
                "event:page" => {
                    if exit {
                        self.exit_path(s).to_owned()
                    } else {
                        self.entry(s).1.to_owned()
                    }
                }
                "event:hostname" => self.entry(s).0.to_owned(),
                d => self.visit_value(d, s).0,
            })
            .collect()
    }
}

// ------------------------------------------------------------------------------------------
// 执行
// ------------------------------------------------------------------------------------------

struct Run<'a> {
    spec: &'a Spec,
    m: &'a Model<'a>,
    gran: &'a str,
    has_event_dim: bool,
}

impl Run<'_> {
    fn needs(&self, metric: &str) -> bool {
        self.spec.metrics.iter().any(|m| m == metric)
    }

    /// 在区间 `iv` 上计算所有组件，按维度原始值分组。
    fn groups(&self, iv: &Iv) -> BTreeMap<Vec<String>, Acc> {
        let mut groups = self.groups_raw(iv);
        // 没有维度的查询恒有一行（数据为空时全是 0）
        if self.spec.dims.is_empty() {
            groups.entry(Vec::new()).or_default();
        }
        groups
    }

    fn groups_raw(&self, iv: &Iv) -> BTreeMap<Vec<String>, Acc> {
        let m = self.m;
        let spec = self.spec;
        let mut groups: BTreeMap<Vec<String>, Acc> = BTreeMap::new();
        let in_range = |ts: i64| ts >= iv.start && ts < iv.end;
        let filter = spec.filter.as_ref();

        let ev_needed = self.needs("visitors")
            || self.needs("pageviews")
            || self.needs("events")
            || self.needs("percentage")
            || self.needs("exit_rate")
            || (self.needs("visits") && self.has_event_dim);
        if ev_needed {
            for (i, e) in m.ev.iter().enumerate() {
                if m.owner[i].is_none() || e.kind() == EventKind::Engagement || !in_range(e.ts_ms) {
                    continue;
                }
                if filter.is_some_and(|f| !m.eval_event(f, i)) {
                    continue;
                }
                let key = m.event_dims(&spec.dims, i, self.gran);
                let a = groups.entry(key).or_default();
                a.visitors.insert((m.clock.date(e.ts_ms), e.person));
                a.events += 1;
                if e.kind() == EventKind::Pageview {
                    a.pageviews += 1;
                }
                a.ev_sessions.insert(m.owner[i].expect("有会话"));
            }
        }

        let ss_needed = self.needs("views_per_visit")
            || self.needs("visit_duration")
            || self.needs("engaged_duration")
            || (self.needs("visits") && !self.has_event_dim);
        let sb_needed = self.needs("bounce_rate");
        let exits_needed = self.needs("exit_rate");
        if ss_needed || sb_needed || exits_needed {
            for (s, sess) in m.sess.iter().enumerate() {
                let active = sess.last_event_at_ms >= iv.start && sess.started_at_ms < iv.end;
                if !active {
                    continue;
                }
                let viewed = filter.is_none_or(|f| m.eval_session(f, s, false, iv));
                if !viewed {
                    continue;
                }
                if ss_needed {
                    let a = groups
                        .entry(m.session_dims(&spec.dims, s, self.gran, iv, false))
                        .or_default();
                    a.s_n += 1;
                    a.s_pv += sess.pageviews;
                    a.s_dur_ms += i128::from(sess.last_event_at_ms - sess.started_at_ms);
                    if sess.engagement_ms > 0 {
                        a.s_eng_ms += sess.engagement_ms;
                        a.s_eng_n += 1;
                    }
                }
                if sb_needed && filter.is_none_or(|f| m.eval_session(f, s, true, iv)) {
                    let a = groups
                        .entry(m.session_dims(&spec.dims, s, self.gran, iv, false))
                        .or_default();
                    a.b_n += 1;
                    if sess.pageviews <= 1 && sess.interactive_events == 0 {
                        a.bounces += 1;
                    }
                }
                if exits_needed && sess.pageviews > 0 {
                    groups
                        .entry(m.session_dims(&spec.dims, s, self.gran, iv, true))
                        .or_default()
                        .exits += 1;
                }
            }
        }

        if self.needs("time_on_page") || self.needs("scroll_depth") {
            for (i, e) in m.ev.iter().enumerate() {
                if m.owner[i].is_none() || e.kind() != EventKind::Engagement || !in_range(e.ts_ms) {
                    continue;
                }
                if filter.is_some_and(|f| !m.eval_event(f, i)) {
                    continue;
                }
                let key = m.event_dims(&spec.dims, i, self.gran);
                let a = groups.entry(key).or_default();
                let slot = a.eng.entry(m.owner[i].expect("有会话")).or_default();
                slot.0 += u64::from(e.engagement_ms);
                if e.scroll_depth <= 100 {
                    slot.1 = slot.1.max(e.scroll_depth);
                    slot.2 += 1;
                }
            }
        }
        groups
    }

    fn total_visitors(&self, iv: &Iv) -> u64 {
        let m = self.m;
        let mut set: HashSet<(Date, u64)> = HashSet::new();
        for (i, e) in m.ev.iter().enumerate() {
            if m.owner[i].is_none()
                || e.kind() == EventKind::Engagement
                || e.ts_ms < iv.start
                || e.ts_ms >= iv.end
            {
                continue;
            }
            if self
                .spec
                .filter
                .as_ref()
                .is_some_and(|f| !m.eval_event(f, i))
            {
                continue;
            }
            set.insert((m.clock.date(e.ts_ms), e.person));
        }
        set.len() as u64
    }

    fn metric_values(&self, a: &Acc, total: Option<u64>) -> Vec<f64> {
        let div = |n: f64, d: f64| if d == 0.0 { 0.0 } else { n / d };
        self.spec
            .metrics
            .iter()
            .map(|name| match name.as_str() {
                "visitors" => a.visitors.len() as f64,
                "pageviews" => a.pageviews as f64,
                "events" => a.events as f64,
                "visits" => {
                    if self.has_event_dim {
                        a.ev_sessions.len() as f64
                    } else {
                        a.s_n as f64
                    }
                }
                "views_per_visit" => div(a.s_pv as f64, a.s_n as f64),
                "bounce_rate" => div(a.bounces as f64, a.b_n as f64),
                "visit_duration" => {
                    if a.s_n == 0 {
                        0.0
                    } else {
                        (a.s_dur_ms as f64 / a.s_n as f64) / 1000.0
                    }
                }
                "engaged_duration" => div(a.s_eng_ms as f64 / 1000.0, a.s_eng_n as f64),
                "percentage" => div(a.visitors.len() as f64, total.unwrap_or(0) as f64),
                "exit_rate" => div(a.exits as f64, a.pageviews as f64),
                "time_on_page" => {
                    let ms: u64 = a.eng.values().map(|x| x.0).sum();
                    div(ms as f64 / 1000.0, a.eng.len() as f64)
                }
                "scroll_depth" => {
                    let valid: Vec<f64> = a
                        .eng
                        .values()
                        .filter(|x| x.2 > 0)
                        .map(|x| f64::from(x.1))
                        .collect();
                    div(valid.iter().sum::<f64>(), valid.len() as f64)
                }
                other => panic!("参考实现不支持指标 {other}"),
            })
            .collect()
    }

    fn display(&self, raw: &[String]) -> Vec<String> {
        self.spec
            .dims
            .iter()
            .zip(raw)
            .map(|(d, v)| {
                if d.starts_with("time") {
                    self.m.clock.bucket_label(self.gran, v)
                } else if v.is_empty() {
                    empty_label(d).to_owned()
                } else {
                    v.clone()
                }
            })
            .collect()
    }

    fn sort(&self, rows: &mut [(Vec<String>, Vec<f64>)], by: &[(String, bool)]) {
        let dims = &self.spec.dims;
        // 请求里 `time` 自动粒度写的是 `time`，其余按写法；这里 `dims[j]` 与请求一致
        let key_of = |k: &str| -> (bool, usize) {
            if let Some(j) = dims.iter().position(|d| d == k) {
                (true, j)
            } else {
                (
                    false,
                    self.spec
                        .metrics
                        .iter()
                        .position(|m| m == k)
                        .expect("order_by 在请求里"),
                )
            }
        };
        let keys: Vec<(bool, usize, bool)> = by
            .iter()
            .map(|(k, asc)| {
                let (is_dim, idx) = key_of(k);
                (is_dim, idx, *asc)
            })
            .collect();
        let used: BTreeSet<usize> = keys.iter().filter(|k| k.0).map(|k| k.1).collect();
        rows.sort_by(|a, b| {
            for (is_dim, idx, asc) in &keys {
                let ord = if *is_dim {
                    a.0[*idx].as_bytes().cmp(b.0[*idx].as_bytes())
                } else {
                    a.1[*idx].total_cmp(&b.1[*idx])
                };
                let ord = if *asc { ord } else { ord.reverse() };
                if ord.is_ne() {
                    return ord;
                }
            }
            for j in 0..dims.len() {
                if !used.contains(&j) {
                    let ord = a.0[j].as_bytes().cmp(b.0[j].as_bytes());
                    if ord.is_ne() {
                        return ord;
                    }
                }
            }
            std::cmp::Ordering::Equal
        });
    }
}

fn change(metric: &str, new: f64, old: f64) -> f64 {
    if matches!(metric, "bounce_rate" | "percentage" | "exit_rate") {
        new - old
    } else if old == 0.0 {
        if new == 0.0 { 0.0 } else { 1.0 }
    } else {
        (new - old) / old
    }
}

/// 执行一个请求。`events` 是该站点收到的全部事件（含会被丢弃的 engagement）。
pub fn run(spec_json: &Value, site: &RefSite, events: &[RefEvent]) -> Result<RefResult, String> {
    let spec = parse_spec(spec_json)?;
    let events: Vec<RefEvent> = events
        .iter()
        .filter(|e| e.site_id == site.site_id)
        .cloned()
        .collect();
    let raws: Vec<RawEvent> = events.iter().map(RefEvent::to_raw).collect();
    let sess = sessionize(&raws);
    let mut owner = vec![None; events.len()];
    for (si, s) in sess.iter().enumerate() {
        for &i in &s.event_indices {
            owner[i] = Some(si);
        }
    }
    let clock = Clock { site };
    let data_start = events
        .iter()
        .enumerate()
        .filter(|(i, _)| owner[*i].is_some())
        .map(|(_, e)| e.ts_ms)
        .min();
    let main_iv = clock.resolve(&spec.range, data_start)?;
    let model = Model {
        ev: &events,
        sess,
        owner,
        clock: Clock { site },
    };
    let time_idx = spec.dims.iter().position(|d| d.starts_with("time"));
    let gran = match time_idx.map(|i| spec.dims[i].as_str()) {
        Some("time") => clock.auto_gran(&main_iv),
        Some("time:minute") => "minute",
        Some("time:hour") => "hour",
        Some("time:day") => "day",
        Some("time:week") => "week",
        Some("time:month") => "month",
        _ => "day",
    };
    let has_event_dim = spec.dims.iter().any(|d| d.starts_with("event:"));
    let run = Run {
        spec: &spec,
        m: &model,
        gran,
        has_event_dim,
    };

    let total = |iv: &Iv| run.needs("percentage").then(|| run.total_visitors(iv));
    let main_groups = run.groups(&main_iv);
    let main_total = total(&main_iv);
    let mut rows: Vec<(Vec<String>, Vec<f64>)> = main_groups
        .iter()
        .map(|(k, a)| (k.clone(), run.metric_values(a, main_total)))
        .collect();
    let order: Vec<(String, bool)> = if spec.order.is_empty() {
        let mut o = Vec::new();
        if let Some(i) = time_idx {
            o.push((spec.dims[i].clone(), true));
        }
        o.push((spec.metrics[0].clone(), false));
        o
    } else {
        spec.order.clone()
    };
    run.sort(&mut rows, &order);
    let total_rows = spec.want_total_rows.then_some(rows.len() as u64);
    let page: Vec<(Vec<String>, Vec<f64>)> = rows
        .into_iter()
        .skip(spec.offset)
        .take(spec.limit)
        .collect();

    let mut out = RefResult {
        total_rows,
        ..RefResult::default()
    };
    if spec.want_time_labels {
        out.time_labels = Some(clock.labels(gran, &main_iv));
    }

    let mut result_rows: Vec<RefRow> = page
        .iter()
        .map(|(raw, metrics)| RefRow {
            raw: raw.clone(),
            dimensions: run.display(raw),
            metrics: metrics.clone(),
            comparison: None,
        })
        .collect();

    if let Some(c) = &spec.compare {
        let civ = clock.compare_iv(c, &main_iv)?;
        if spec.want_time_labels {
            out.comparison_time_labels = Some(clock.labels(gran, &civ));
        }
        let keyed: Vec<usize> = (0..spec.dims.len())
            .filter(|i| Some(*i) != time_idx)
            .collect();
        if !page.is_empty() {
            let allowed: BTreeSet<Vec<String>> = page
                .iter()
                .map(|(raw, _)| keyed.iter().map(|i| raw[*i].clone()).collect())
                .collect();
            let ctotal = total(&civ);
            let cgroups: BTreeMap<Vec<String>, Acc> = run
                .groups(&civ)
                .into_iter()
                .filter(|(k, _)| {
                    keyed.is_empty()
                        || allowed
                            .contains(&keyed.iter().map(|i| k[*i].clone()).collect::<Vec<_>>())
                })
                .collect();
            if let Some(ti) = time_idx {
                let mut crow: Vec<(Vec<String>, Vec<f64>)> = cgroups
                    .iter()
                    .map(|(k, a)| (k.clone(), run.metric_values(a, ctotal)))
                    .collect();
                let order = vec![(spec.dims[ti].clone(), true)];
                run.sort(&mut crow, &order);
                out.comparison_rows = Some(
                    crow.into_iter()
                        .map(|(raw, metrics)| (run.display(&raw), metrics))
                        .collect(),
                );
            } else {
                for row in &mut result_rows {
                    let old = match cgroups.get(&row.raw) {
                        Some(a) => run.metric_values(a, ctotal),
                        None => vec![0.0; spec.metrics.len()],
                    };
                    let chg = spec
                        .metrics
                        .iter()
                        .zip(row.metrics.iter().zip(&old))
                        .map(|(m, (n, o))| change(m, *n, *o))
                        .collect();
                    row.comparison = Some((old, chg));
                }
            }
        } else if time_idx.is_some() {
            out.comparison_rows = Some(Vec::new());
        }
    }
    out.rows = result_rows;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use jiff::civil::date;
    use serde_json::json;

    use super::*;

    const MIN: i64 = 60_000;

    fn site(tz: &str, now: Timestamp) -> RefSite {
        RefSite {
            site_id: 1,
            tz: TimeZone::get(tz).unwrap(),
            week_start: Weekday::Monday,
            now_ms: now.as_millisecond(),
        }
    }

    fn pv(person: u64, ts: i64, path: &str, source: &str) -> RefEvent {
        RefEvent {
            site_id: 1,
            person,
            ts_ms: ts,
            name: "pageview".into(),
            hostname: "example.test".into(),
            pathname: path.into(),
            interactive: true,
            engagement_ms: 0,
            scroll_depth: 255,
            attrs: RefAttrs {
                referrer_source: source.into(),
                ..RefAttrs::default()
            },
        }
    }

    fn eng(person: u64, ts: i64, path: &str, ms: u32, sd: u8) -> RefEvent {
        RefEvent {
            name: "engagement".into(),
            engagement_ms: ms,
            scroll_depth: sd,
            ..pv(person, ts, path, "")
        }
    }

    fn at(d: Date, h: i8, m: i8, tz: &TimeZone) -> i64 {
        d.at(h, m, 0, 0)
            .to_zoned(tz.clone())
            .unwrap()
            .timestamp()
            .as_millisecond()
    }

    #[test]
    fn glob_matching() {
        assert!(glob_match("/blog/*", "/blog/a/b"));
        assert!(glob_match("/blog/*", "/blog/"));
        assert!(!glob_match("/blog/*", "/blog"));
        assert!(glob_match("*", ""));
        assert!(glob_match("a**b", "axxb"));
        assert!(glob_match("a*b*c", "abxbc"));
        assert!(!glob_match("a*b", "abx"));
        assert!(glob_match("a.b", "a.b") && !glob_match("a.b", "axb"));
        assert!(glob_match("(x|y)+", "(x|y)+") && !glob_match("(x|y)+", "x"));
    }

    #[test]
    fn by_hand_page_filter_semantics() {
        // 两个人：1 号 /a -> /b；2 号 /b 入口 -> /a。过滤 event:page is /a：
        // visitors/pageviews 数「浏览过 /a」，bounce_rate 数「入口是 /a」的会话（1 号：2 页，不是跳出）
        let tz = TimeZone::UTC;
        let d = date(2026, 3, 10);
        let t = |h, m| at(d, h, m, &tz);
        let evs = vec![
            pv(1, t(10, 0), "/a", ""),
            pv(1, t(10, 1), "/b", ""),
            pv(2, t(11, 0), "/b", ""),
            pv(2, t(11, 1), "/a", ""),
            pv(3, t(12, 0), "/a", ""),
        ];
        let s = site(
            "UTC",
            Timestamp::from_millisecond(at(date(2026, 3, 11), 0, 0, &tz)).unwrap(),
        );
        let spec = json!({
            "metrics": ["visitors", "pageviews", "visits", "bounce_rate"],
            "date_range": ["2026-03-10", "2026-03-10"],
            "filters": [["is", "event:page", ["/a"]]]
        });
        let r = run(&spec, &s, &evs).unwrap();
        assert_eq!(r.rows.len(), 1);
        // visitors 3、pageviews 3、visits 3（浏览过 /a 的会话）；bounce：入口是 /a 的会话 1 号（不跳出）和 3 号（跳出）
        assert_eq!(r.rows[0].metrics, vec![3.0, 3.0, 3.0, 0.5]);
    }

    #[test]
    fn time_series_and_compare_by_hand() {
        let tz = TimeZone::UTC;
        let t = |d: Date, h| at(d, h, 0, &tz);
        let evs = vec![
            pv(1, t(date(2026, 3, 9), 10), "/", ""),
            pv(2, t(date(2026, 3, 10), 10), "/", ""),
            pv(3, t(date(2026, 3, 10), 11), "/", ""),
            eng(3, t(date(2026, 3, 10), 11) + MIN, "/", 4000, 50),
        ];
        let s = site(
            "UTC",
            Timestamp::from_millisecond(t(date(2026, 3, 12), 0)).unwrap(),
        );
        let spec = json!({
            "metrics": ["visitors", "time_on_page", "scroll_depth"],
            "date_range": ["2026-03-10", "2026-03-10"],
            "dimensions": ["event:page"],
            "include": {"compare": "previous_period", "total_rows": true}
        });
        let r = run(&spec, &s, &evs).unwrap();
        assert_eq!(r.total_rows, Some(1));
        assert_eq!(r.rows[0].metrics, vec![2.0, 4.0, 50.0]);
        let (old, chg) = r.rows[0].comparison.clone().unwrap();
        assert_eq!(old, vec![1.0, 0.0, 0.0]);
        assert_eq!(chg, vec![1.0, 1.0, 1.0]);
        // 时间维度：标签补齐空桶
        let spec = json!({
            "metrics": ["visitors"],
            "date_range": ["2026-03-08", "2026-03-11"],
            "dimensions": ["time:day"],
            "include": {"time_labels": true}
        });
        let r = run(&spec, &s, &evs).unwrap();
        assert_eq!(
            r.time_labels.unwrap(),
            ["2026-03-08", "2026-03-09", "2026-03-10", "2026-03-11"]
        );
        let got: Vec<(String, f64)> = r
            .rows
            .iter()
            .map(|x| (x.dimensions[0].clone(), x.metrics[0]))
            .collect();
        assert_eq!(
            got,
            [("2026-03-09".into(), 1.0), ("2026-03-10".into(), 2.0)]
        );
    }
}
