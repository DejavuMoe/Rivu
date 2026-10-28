//! 导入 Plausible 的数据导出（站点设置 → Imports & Exports → Export，得到一个 zip，里面是 `imported_*.csv`）。
//!
//! 导出是「日期 × 一个维度族」的日汇总，与 Rivu 的日汇总层一一对应，所以写进汇总层（`origin = 'import'`，
//! `import_id` = `site_imports.id`），查询这些天时由汇总层回答（`system-design.md` §6.4）：
//!
//! | 文件 | 写入 | 说明 |
//! |---|---|---|
//! | `imported_visitors` | `rollup_site`；`rollup_events` 的 `pageview` 行 | `events` = 浏览量 + 当天的自定义事件数（不含 engagement） |
//! | `imported_sources` | `rollup_sources` | 来源与渠道按 Rivu 自己的规则从 referrer 与 UTM 重新判定（与原生数据一致），认不出时保留 Plausible 的来源名 |
//! | `imported_pages` | `rollup_pages` | 多个 hostname 按路径相加；会话指标取自 `imported_entry_pages`（入口页口径，同原生），退出数取自 `imported_exit_pages` |
//! | `imported_entry_pages` / `imported_exit_pages` | `rollup_entry_pages` / `rollup_exit_pages` | |
//! | `imported_locations` | `rollup_locations` | 城市 `0` 为空 |
//! | `imported_devices` / `imported_browsers` / `imported_operating_systems` | `rollup_devices` / `rollup_browsers` / `rollup_os` | 名字换成 Rivu 的写法（`Desktop` → `desktop`、`Mac` → `macOS`、`Microsoft Edge` → `Edge`……） |
//! | `imported_custom_events` | `rollup_events` | 按事件名相加，跳过 `engagement` |
//! | `imported_custom_props` | `rollup_props` | |
//!
//! 时长从秒换成毫秒。只写站点自己的数据开始之前的天（同一天两边都有会重复计数）；与已完成的导入重叠时拒绝。

use std::collections::BTreeMap;
use std::io::Read;
use std::path::Path;

use anyhow::{Context, Result, bail};
use clickhouse::Client;
use jiff::civil::Date;
use jiff::tz::TimeZone;
use rivu_query::TenantScope;
use rivu_query::rollup::{self, ImportRow, Measures};
use sqlx::PgPool;

/// 导入的结果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Summary {
    pub start: Date,
    pub end: Date,
    /// 写入的汇总行数（各表合计）。
    pub rows: u64,
    /// 因为站点自己的数据已经开始而跳过的天（`None`：没有跳过）。
    pub clipped_from: Option<Date>,
}

/// 一个文件的内容：表头 → 列号，加上各行。
struct Csv {
    name: String,
    index: BTreeMap<String, usize>,
    rows: Vec<csv::ByteRecord>,
}

impl Csv {
    fn col(&self, rec: &csv::ByteRecord, name: &str) -> Result<String> {
        let i = self
            .index
            .get(name)
            .with_context(|| format!("{} 缺少列 {name}", self.name))?;
        // 导出里可能有不是 UTF-8 的字节（个别路径），换成替换字符，不让整个导入失败
        Ok(String::from_utf8_lossy(rec.get(*i).unwrap_or(b""))
            .trim()
            .to_owned())
    }

    fn num(&self, rec: &csv::ByteRecord, name: &str) -> Result<u64> {
        let s = self.col(rec, name)?;
        if s.is_empty() {
            return Ok(0);
        }
        let v: f64 = s
            .parse()
            .with_context(|| format!("{} 的 {name} 不是数字：{s}", self.name))?;
        Ok(if v.is_finite() && v > 0.0 {
            v.round() as u64
        } else {
            0
        })
    }

    fn day(&self, rec: &csv::ByteRecord) -> Result<Date> {
        let s = self.col(rec, "date")?;
        s.parse()
            .with_context(|| format!("{} 的日期无效：{s}", self.name))
    }
}

/// 读出 zip 里的 `imported_<族>_*.csv`，按族名索引。
fn read_zip(path: &Path) -> Result<BTreeMap<String, Csv>> {
    let file = std::fs::File::open(path).with_context(|| format!("打不开 {}", path.display()))?;
    let mut zip = zip::ZipArchive::new(file).context("不是有效的 zip 文件")?;
    let mut out = BTreeMap::new();
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i)?;
        let name = entry.name()?.rsplit('/').next().unwrap_or("").to_owned();
        let Some(rest) = name.strip_prefix("imported_") else {
            continue;
        };
        if !name.ends_with(".csv") {
            continue;
        }
        // imported_<族>_<开始>_<结束>.csv：族名本身可能带下划线（entry_pages、operating_systems）
        let family = rest
            .trim_end_matches(".csv")
            .rsplitn(3, '_')
            .nth(2)
            .unwrap_or("")
            .to_owned();
        let mut bytes = Vec::new();
        entry
            .read_to_end(&mut bytes)
            .with_context(|| format!("读取 {name} 失败"))?;
        let mut reader = csv::ReaderBuilder::new()
            .has_headers(true)
            .from_reader(bytes.as_slice());
        let index = reader
            .byte_headers()
            .with_context(|| format!("{name} 没有表头"))?
            .iter()
            .enumerate()
            .map(|(i, h)| (String::from_utf8_lossy(h).trim().to_owned(), i))
            .collect();
        let rows = reader
            .byte_records()
            .collect::<Result<Vec<_>, _>>()
            .with_context(|| format!("解析 {name} 失败"))?;
        out.insert(family, Csv { name, index, rows });
    }
    if !out.contains_key("visitors") {
        bail!("zip 里没有 imported_visitors_*.csv：这不是 Plausible 的数据导出");
    }
    Ok(out)
}

fn device(s: &str) -> String {
    match s {
        "Desktop" => "desktop",
        "Mobile" => "mobile",
        "Tablet" => "tablet",
        _ => "",
    }
    .to_owned()
}

fn browser(s: &str) -> String {
    match s {
        "Microsoft Edge" => "Edge",
        "Samsung Browser" => "Samsung Internet",
        "Mobile App" => "WebView",
        "(not set)" => "",
        other => other,
    }
    .to_owned()
}

fn os(s: &str) -> String {
    match s {
        "Mac" => "macOS",
        "GNU/Linux" => "Linux",
        "(not set)" => "",
        other => other,
    }
    .to_owned()
}

/// 来源与渠道：按 Rivu 的规则从 referrer 与 UTM 判定；认不出来源时保留 Plausible 的来源名。
fn source_channel(source: &str, referrer: &str, utm: &rivu_domain::Utm) -> (String, String) {
    let host = referrer
        .split('/')
        .next()
        .filter(|h| !h.is_empty())
        .map(str::to_ascii_lowercase);
    let info = rivu_source::classify(&rivu_source::SourceInput {
        referrer_host: host.as_deref(),
        utm,
        click_id_param: None,
    });
    let name = if info.referrer_source.is_empty() {
        source.to_owned()
    } else {
        info.referrer_source
    };
    (name, info.channel.as_str().to_owned())
}

/// 会话类度量（访问、跳出、时长、浏览量）：各族的公共部分。
fn session(visitors: u64, visits: u64, bounces: u64, duration_s: u64, pageviews: u64) -> Measures {
    Measures {
        visitors,
        pageviews,
        events: pageviews,
        visits_s: visits,
        s_n: visits,
        s_pv: pageviews,
        s_dur_ms: duration_s.saturating_mul(1000),
        b_n: visits,
        bounces,
        ..Measures::default()
    }
}

type Agg = BTreeMap<(Date, Vec<String>), Measures>;

fn put(agg: &mut Agg, day: Date, dims: Vec<String>, m: Measures) {
    agg.entry((day, dims)).or_default().add(&m);
}

/// 把 CSV 换成各表的行。`keep(day)` 决定这一天要不要。
fn transform(
    files: &BTreeMap<String, Csv>,
    keep: &dyn Fn(Date) -> bool,
) -> Result<BTreeMap<&'static str, Agg>> {
    let mut out: BTreeMap<&'static str, Agg> = BTreeMap::new();
    let mut custom_events_per_day: BTreeMap<Date, u64> = BTreeMap::new();

    if let Some(f) = files.get("custom_events") {
        let agg = out.entry("rollup_events").or_default();
        for r in &f.rows {
            let day = f.day(r)?;
            let name = f.col(r, "name")?;
            if !keep(day) || name == "engagement" || name.is_empty() {
                continue;
            }
            let events = f.num(r, "events")?;
            *custom_events_per_day.entry(day).or_default() += events;
            let m = Measures {
                visitors: f.num(r, "visitors")?,
                events,
                ..Measures::default()
            };
            put(agg, day, vec![name], m);
        }
    }
    if let Some(f) = files.get("custom_props") {
        let agg = out.entry("rollup_props").or_default();
        for r in &f.rows {
            let day = f.day(r)?;
            if !keep(day) {
                continue;
            }
            let m = Measures {
                visitors: f.num(r, "visitors")?,
                events: f.num(r, "events")?,
                ..Measures::default()
            };
            put(agg, day, vec![f.col(r, "property")?, f.col(r, "value")?], m);
        }
    }

    // 页面：浏览、停留与滚动来自 pages；会话指标来自 entry_pages（入口页口径）；退出来自 exit_pages
    let mut pages: Agg = BTreeMap::new();
    let mut site_tp: BTreeMap<Date, Measures> = BTreeMap::new();
    if let Some(f) = files.get("pages") {
        for r in &f.rows {
            let day = f.day(r)?;
            if !keep(day) {
                continue;
            }
            let pageviews = f.num(r, "pageviews")?;
            let m = Measures {
                visitors: f.num(r, "visitors")?,
                pageviews,
                events: pageviews,
                visits_ev: f.num(r, "visits")?,
                tp_ms: f.num(r, "total_time_on_page")?.saturating_mul(1000),
                tp_n: f.num(r, "total_time_on_page_visits")?,
                sc_sum: f.num(r, "total_scroll_depth")?,
                sc_n: f.num(r, "total_scroll_depth_visits")?,
                ..Measures::default()
            };
            site_tp.entry(day).or_default().add(&Measures {
                tp_ms: m.tp_ms,
                tp_n: m.tp_n,
                sc_sum: m.sc_sum,
                sc_n: m.sc_n,
                ..Measures::default()
            });
            put(&mut pages, day, vec![f.col(r, "page")?], m);
        }
    }
    if let Some(f) = files.get("entry_pages") {
        let agg = out.entry("rollup_entry_pages").or_default();
        for r in &f.rows {
            let day = f.day(r)?;
            if !keep(day) {
                continue;
            }
            let page = f.col(r, "entry_page")?;
            let m = session(
                f.num(r, "visitors")?,
                f.num(r, "entrances")?,
                f.num(r, "bounces")?,
                f.num(r, "visit_duration")?,
                f.num(r, "pageviews")?,
            );
            put(agg, day, vec![page.clone()], m);
            // 页面族：会话指标按入口页归属（不重复计访客与浏览量）
            put(
                &mut pages,
                day,
                vec![page],
                Measures {
                    s_n: m.s_n,
                    s_pv: m.s_pv,
                    s_dur_ms: m.s_dur_ms,
                    b_n: m.b_n,
                    bounces: m.bounces,
                    ..Measures::default()
                },
            );
        }
    }
    if let Some(f) = files.get("exit_pages") {
        let agg = out.entry("rollup_exit_pages").or_default();
        for r in &f.rows {
            let day = f.day(r)?;
            if !keep(day) {
                continue;
            }
            let page = f.col(r, "exit_page")?;
            let exits = f.num(r, "exits")?;
            let m = session(
                f.num(r, "visitors")?,
                exits,
                f.num(r, "bounces")?,
                f.num(r, "visit_duration")?,
                f.num(r, "pageviews")?,
            );
            put(agg, day, vec![page.clone()], m);
            put(
                &mut pages,
                day,
                vec![page],
                Measures {
                    exits,
                    ..Measures::default()
                },
            );
        }
    }
    if !pages.is_empty() {
        out.insert("rollup_pages", pages);
    }

    // 其余会话族：同样的五个度量
    let families: [(&str, &'static str, &[&str]); 5] = [
        (
            "sources",
            "rollup_sources",
            &[
                "source",
                "referrer",
                "utm_source",
                "utm_medium",
                "utm_campaign",
                "utm_content",
                "utm_term",
            ],
        ),
        (
            "locations",
            "rollup_locations",
            &["country", "region", "city"],
        ),
        ("devices", "rollup_devices", &["device"]),
        (
            "browsers",
            "rollup_browsers",
            &["browser", "browser_version"],
        ),
        (
            "operating_systems",
            "rollup_os",
            &["operating_system", "operating_system_version"],
        ),
    ];
    for (file, table, cols) in families {
        let Some(f) = files.get(file) else {
            continue;
        };
        let agg = out.entry(table).or_default();
        for r in &f.rows {
            let day = f.day(r)?;
            if !keep(day) {
                continue;
            }
            let raw: Vec<String> = cols.iter().map(|c| f.col(r, c)).collect::<Result<_>>()?;
            let dims = match file {
                "sources" => {
                    let utm = rivu_domain::Utm {
                        source: Some(raw[2].clone()).filter(|s| !s.is_empty()),
                        medium: Some(raw[3].clone()).filter(|s| !s.is_empty()),
                        campaign: Some(raw[4].clone()).filter(|s| !s.is_empty()),
                        content: Some(raw[5].clone()).filter(|s| !s.is_empty()),
                        term: Some(raw[6].clone()).filter(|s| !s.is_empty()),
                    };
                    let (source, channel) = source_channel(&raw[0], &raw[1], &utm);
                    let mut d = vec![channel, source];
                    d.extend(raw[1..].iter().cloned());
                    d
                }
                "locations" => vec![
                    raw[0].clone(),
                    raw[1].clone(),
                    if raw[2] == "0" {
                        String::new()
                    } else {
                        raw[2].clone()
                    },
                ],
                "devices" => vec![device(&raw[0])],
                "browsers" => vec![browser(&raw[0]), raw[1].clone()],
                _ => vec![os(&raw[0]), raw[1].clone()],
            };
            let m = session(
                f.num(r, "visitors")?,
                f.num(r, "visits")?,
                f.num(r, "bounces")?,
                f.num(r, "visit_duration")?,
                f.num(r, "pageviews")?,
            );
            put(agg, day, dims, m);
        }
    }

    // 站点级：访客文件；事件数 = 浏览量 + 自定义事件；停留与滚动按页面相加。事件族补一行 pageview
    let f = &files["visitors"];
    let mut site: Agg = BTreeMap::new();
    for r in &f.rows {
        let day = f.day(r)?;
        if !keep(day) {
            continue;
        }
        let visits = f.num(r, "visits")?;
        let pageviews = f.num(r, "pageviews")?;
        let mut m = session(
            f.num(r, "visitors")?,
            visits,
            f.num(r, "bounces")?,
            f.num(r, "visit_duration")?,
            pageviews,
        );
        m.events = pageviews + custom_events_per_day.get(&day).copied().unwrap_or(0);
        if let Some(tp) = site_tp.get(&day) {
            m.add(tp);
        }
        put(&mut site, day, vec![], m);
        put(
            out.entry("rollup_events").or_default(),
            day,
            vec!["pageview".to_owned()],
            Measures {
                visitors: m.visitors,
                pageviews,
                events: pageviews,
                visits_ev: visits,
                ..Measures::default()
            },
        );
    }
    out.insert("rollup_site", site);
    Ok(out)
}

/// 执行一次导入：读 zip、按站点自己的数据截断、写进汇总层。`import_id` 是 `site_imports.id`（调用方先建记录，
/// 成功后记为完成；失败时调用方删除已写入的行）。`ch` 用 `rivu_maintainer` 账号（汇总表的 INSERT）。
pub async fn run(
    pg: &PgPool,
    ch: &Client,
    site_id: u64,
    tz_name: &str,
    import_id: u64,
    zip_path: &Path,
) -> Result<Summary> {
    let path = zip_path.to_owned();
    let files = tokio::task::spawn_blocking(move || read_zip(&path))
        .await
        .context("读取 zip 的任务异常退出")??;

    // 站点自己的数据从哪天开始：那天及之后不导入（同一天两边都有会重复计数）
    let tz = TimeZone::get(tz_name).unwrap_or(TimeZone::UTC);
    let scope = TenantScope::new(site_id);
    let state = rollup::build_state(ch, &scope, &tz)
        .await
        .map_err(|e| anyhow::anyhow!("读取站点数据范围失败：{e}"))?;
    let native_from = state.first_day;
    let keep = |d: Date| native_from.is_none_or(|n| d < n);

    let tables = transform(&files, &keep)?;
    let site_days: Vec<Date> = tables
        .get("rollup_site")
        .map(|a| a.keys().map(|(d, _)| *d).collect())
        .unwrap_or_default();
    let (Some(start), Some(end)) = (site_days.first().copied(), site_days.last().copied()) else {
        bail!(match native_from {
            Some(d) =>
                format!("导出里没有 {d}（站点自己的数据开始的那天）之前的数据，没有可以导入的天"),
            None => "导出里没有数据".to_owned(),
        });
    };
    if let Some(other) =
        rivu_storage_pg::imports::overlapping(pg, site_id, &start.to_string(), &end.to_string())
            .await?
    {
        bail!("与已有的导入 #{other} 的日期重叠（{start} – {end}），先删除那次导入");
    }
    let clipped_from = native_from.filter(|n| {
        files["visitors"]
            .rows
            .iter()
            .filter_map(|r| files["visitors"].day(r).ok())
            .any(|d| d >= *n)
    });

    let mut rows = 0u64;
    // rollup_site 最后写：与构建任务一致，它的行是「这一天有数据」的标记
    let order = tables
        .keys()
        .copied()
        .filter(|t| *t != "rollup_site")
        .chain(std::iter::once("rollup_site"));
    for table in order {
        let batch: Vec<ImportRow> = tables[table]
            .iter()
            .map(|((day, dims), m)| ImportRow {
                day: *day,
                dims: dims.clone(),
                m: *m,
            })
            .collect();
        rows += batch.len() as u64;
        rollup::insert_import(ch, &scope, table, tz_name, import_id, &batch)
            .await
            .map_err(|e| anyhow::anyhow!("写入 {table} 失败：{e}"))?;
    }
    Ok(Summary {
        start,
        end,
        rows,
        clipped_from,
    })
}

/// 建记录、导入、记结果；失败时删除已写入的汇总行并把记录置为失败。返回记录 id 与结果。
pub async fn import_file(
    pg: &PgPool,
    ch: &Client,
    site_id: u64,
    tz_name: &str,
    zip_path: &Path,
    file_name: &str,
    imported_by: Option<i64>,
) -> Result<(i64, Summary)> {
    let id = rivu_storage_pg::imports::create(pg, site_id, "plausible_csv", file_name, imported_by)
        .await?;
    let import_id = u64::try_from(id).context("导入 id 为负数")?;
    match run(pg, ch, site_id, tz_name, import_id, zip_path).await {
        Ok(s) => {
            rivu_storage_pg::imports::complete(
                pg,
                id,
                &s.start.to_string(),
                &s.end.to_string(),
                i64::try_from(s.rows).unwrap_or(i64::MAX),
            )
            .await?;
            tracing::info!(site_id, import_id = id, rows = s.rows, start = %s.start, end = %s.end, "导入完成");
            Ok((id, s))
        }
        Err(e) => {
            let msg = format!("{e:#}");
            if let Err(d) = rollup::delete_import(ch, &TenantScope::new(site_id), import_id).await {
                tracing::error!(site_id, import_id = id, error = %d, "清理失败导入的汇总行失败");
            }
            rivu_storage_pg::imports::fail(pg, id, &msg).await?;
            tracing::warn!(site_id, import_id = id, error = %msg, "导入失败");
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_follow_rivu() {
        assert_eq!(device("Desktop"), "desktop");
        assert_eq!(device("(not set)"), "");
        assert_eq!(os("Mac"), "macOS");
        assert_eq!(os("GNU/Linux"), "Linux");
        assert_eq!(os("Windows"), "Windows");
        assert_eq!(browser("Microsoft Edge"), "Edge");
        assert_eq!(browser("Mobile App"), "WebView");
        let empty = rivu_domain::Utm::default();
        let (s, c) = source_channel("Bing", "cn.bing.com", &empty);
        assert_eq!(c, "Organic Search");
        assert!(!s.is_empty());
        assert_eq!(
            source_channel("", "", &empty),
            (String::new(), "Direct".to_owned())
        );
    }
}
