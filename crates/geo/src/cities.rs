//! 城市名索引（`system-design.md` §4.4「城市名」、§10 Q30）。
//!
//! `visit:city` 存的是 GeoNames id（`city_geoname_id`），看板要显示名字。名字取自**当前加载的 City 库本身**：
//! 库加载（和每次热加载）后，在后台线程遍历库里的全部网段，只解码 `city.geoname_id`、`city.names` 的
//! `en` 与 `zh-CN`、`country.iso_code`，建一张 `geoname_id → 名字` 的表。不另外下载 GeoNames，也不自己做
//! 「名字 → id」映射（§10 Q24）。
//!
//! 内存布局：所有名字拼进一个字符串，表项是按 id 排序的定长数组（每项 24 字节），查找用二分。
//! 多个网段指向同一条记录，按记录在数据区的偏移去重，每条记录只解码一次。

use std::collections::{BTreeMap, HashMap, HashSet};

use anyhow::{Context, Result};
use maxminddb::{Reader, WithinOptions};
use serde::{Deserialize, Serialize};

use crate::db::country_code;

/// 城市名的语言。库里有多种语言，Rivu 只留看板支持的两种。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NameLang {
    /// 英文（`en`），也是其他语言缺名字时的回退。
    En,
    /// 简体中文（`zh-CN`）；库里没有中文名的城市回退到英文名。
    ZhCn,
}

impl NameLang {
    /// 从 `Accept-Language` 取：第一个语言标签以 `zh` 开头为中文，其余（含缺失）为英文。
    /// 与邮件的语言判定（`rivu_mail::Lang`）同一规则。
    pub fn from_accept_language(header: Option<&str>) -> NameLang {
        let first = header
            .and_then(|h| h.split(',').next())
            .map(|t| t.trim().to_ascii_lowercase())
            .unwrap_or_default();
        if first.starts_with("zh") {
            NameLang::ZhCn
        } else {
            NameLang::En
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            NameLang::En => "en",
            NameLang::ZhCn => "zh-CN",
        }
    }
}

/// `meta.city_names` 里的一项：`{"name": "...", "country": "DE"}`。没有国家时 `country` 为 `null`。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CityName {
    pub name: String,
    pub country: Option<String>,
}

/// 名字在 `text` 里的位置；`len == 0` 表示没有。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Span {
    start: u32,
    len: u32,
}

#[derive(Debug, Clone, Copy)]
struct Entry {
    id: u32,
    en: Span,
    zh: Span,
    /// ISO 3166-1 alpha-2，大写；没有为 `[0, 0]`。
    country: [u8; 2],
}

/// `geoname_id → 名字` 的只读表，由 [`CityIndex::build`] 从一个 City 库建出。
#[derive(Debug)]
pub struct CityIndex {
    entries: Box<[Entry]>,
    text: Box<str>,
    /// 建它的那个库在 `GeoDb` 里的代数。
    pub(crate) generation: u64,
}

/// 建索引时的统计，写进日志与测量。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BuildStats {
    /// 遍历到的网段数。
    pub networks: u64,
    /// 解码过的记录数（按偏移去重后）。
    pub records: u64,
    /// 解码失败而跳过的记录数。
    pub skipped: u64,
}

/// 只解码城市名需要的字段；其余字段由解码器跳过。
#[derive(Deserialize)]
struct Rec<'a> {
    #[serde(borrow, default)]
    city: Option<CityRec<'a>>,
    #[serde(borrow, default)]
    country: Option<CountryRec<'a>>,
}

#[derive(Deserialize)]
struct CityRec<'a> {
    #[serde(default)]
    geoname_id: Option<u32>,
    #[serde(borrow, default)]
    names: Option<Names<'a>>,
}

#[derive(Deserialize)]
struct Names<'a> {
    #[serde(borrow, default)]
    en: Option<&'a str>,
    #[serde(borrow, default, rename = "zh-CN")]
    zh: Option<&'a str>,
}

#[derive(Deserialize)]
struct CountryRec<'a> {
    #[serde(borrow, default)]
    iso_code: Option<&'a str>,
}

/// 解码后的一条城市记录，给取键函数用。
pub(crate) struct Decoded<'a> {
    pub(crate) geoname_id: Option<u32>,
    #[cfg_attr(not(test), expect(dead_code, reason = "只有测量用的取键函数读它"))]
    pub(crate) en: Option<&'a str>,
    pub(crate) country: [u8; 2],
}

/// 每遍历这么多个网段检查一次是否该放弃（库又被替换了）。
const CANCEL_CHECK_EVERY: u64 = 1 << 16;

impl CityIndex {
    /// 遍历库建表。`cancelled` 返回 `true` 时放弃，返回 `Ok(None)`。
    pub(crate) fn build<S: AsRef<[u8]>>(
        reader: &Reader<S>,
        generation: u64,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<(CityIndex, BuildStats)>> {
        Self::build_with(reader, generation, cancelled, |d| d.geoname_id)
    }

    /// 同 [`CityIndex::build`]，键由 `key` 从记录里取（测量用：DB-IP 的库没有 geoname id）。
    pub(crate) fn build_with<S: AsRef<[u8]>>(
        reader: &Reader<S>,
        generation: u64,
        cancelled: impl Fn() -> bool,
        key: impl Fn(&Decoded<'_>) -> Option<u32>,
    ) -> Result<Option<(CityIndex, BuildStats)>> {
        let mut stats = BuildStats::default();
        let mut seen: HashSet<usize> = HashSet::new();
        let mut map: HashMap<u32, Entry> = HashMap::new();
        let mut text = String::new();
        let mut push = |s: Option<&str>| -> Span {
            match s.map(str::trim).filter(|s| !s.is_empty()) {
                Some(s) => match (u32::try_from(text.len()), u32::try_from(s.len())) {
                    (Ok(start), Ok(len)) => {
                        text.push_str(s);
                        Span { start, len }
                    }
                    _ => Span::default(),
                },
                None => Span::default(),
            }
        };
        for item in reader
            .networks(WithinOptions::default())
            .context("遍历 GeoIP 库失败")?
        {
            if stats.networks.is_multiple_of(CANCEL_CHECK_EVERY) && cancelled() {
                return Ok(None);
            }
            stats.networks += 1;
            let item = item.context("遍历 GeoIP 库失败")?;
            let Some(offset) = item.offset() else {
                continue;
            };
            if !seen.insert(offset) {
                continue;
            }
            stats.records += 1;
            let rec = match item.decode::<Rec>() {
                Ok(Some(rec)) => rec,
                Ok(None) => continue,
                Err(_) => {
                    stats.skipped += 1;
                    continue;
                }
            };
            let Some(city) = rec.city else {
                continue;
            };
            let (en, zh) = city.names.map_or((None, None), |n| (n.en, n.zh));
            let decoded = Decoded {
                geoname_id: city.geoname_id,
                en,
                country: rec
                    .country
                    .and_then(|c| c.iso_code)
                    .map_or([0, 0], country_code),
            };
            let Some(id) = key(&decoded).filter(|id| *id != 0) else {
                continue;
            };
            if map.contains_key(&id) {
                continue;
            }
            // 没有英文名时用中文名顶上；两种都没有就不收（看板显示 id 本身）
            let (en, zh) = match (en, zh) {
                (None, Some(z)) => (Some(z), None),
                other => other,
            };
            let en = push(en);
            if en.len == 0 {
                continue;
            }
            let zh = push(zh);
            map.insert(
                id,
                Entry {
                    id,
                    en,
                    zh,
                    country: decoded.country,
                },
            );
        }
        let mut entries: Vec<Entry> = map.into_values().collect();
        entries.sort_unstable_by_key(|e| e.id);
        Ok(Some((
            CityIndex {
                entries: entries.into_boxed_slice(),
                text: text.into_boxed_str(),
                generation,
            },
            stats,
        )))
    }

    fn span(&self, s: Span) -> Option<&str> {
        if s.len == 0 {
            return None;
        }
        let start = s.start as usize;
        self.text.get(start..start + s.len as usize)
    }

    /// 一个城市的名字：要中文而库里没有中文名时回退到英文名。查不到返回 `None`。
    pub fn get(&self, id: u32, lang: NameLang) -> Option<CityName> {
        let i = self.entries.binary_search_by_key(&id, |e| e.id).ok()?;
        let e = &self.entries[i];
        let name = match lang {
            NameLang::ZhCn => self.span(e.zh).or_else(|| self.span(e.en)),
            NameLang::En => self.span(e.en),
        }?;
        Some(CityName {
            name: name.to_owned(),
            country: (e.country != [0, 0])
                .then(|| String::from_utf8_lossy(&e.country).into_owned()),
        })
    }

    /// 一批 id 的名字，键是 id 的十进制字符串（与 `visit:city` 的维度值一致）；查不到的 id 不出现。
    pub fn names(
        &self,
        ids: impl IntoIterator<Item = u32>,
        lang: NameLang,
    ) -> BTreeMap<String, CityName> {
        ids.into_iter()
            .filter_map(|id| self.get(id, lang).map(|n| (id.to_string(), n)))
            .collect()
    }

    /// 城市数。
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// 表本身占的堆内存（字节）：表项数组加名字字符串。
    pub fn heap_bytes(&self) -> usize {
        std::mem::size_of_val(&*self.entries) + self.text.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn test_db(file: &str) -> Reader<Vec<u8>> {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/data/test-data")
            .join(file);
        Reader::from_source(std::fs::read(path).unwrap()).unwrap()
    }

    #[test]
    fn accept_language_picks_chinese_or_english() {
        for (h, want) in [
            (Some("zh-CN,zh;q=0.9,en;q=0.8"), NameLang::ZhCn),
            (Some("zh"), NameLang::ZhCn),
            (Some("ZH-tw"), NameLang::ZhCn),
            (Some("en-US,zh;q=0.5"), NameLang::En),
            (Some("de"), NameLang::En),
            (Some(""), NameLang::En),
            (None, NameLang::En),
        ] {
            assert_eq!(NameLang::from_accept_language(h), want, "{h:?}");
        }
        assert_eq!(NameLang::ZhCn.as_str(), "zh-CN");
    }

    #[test]
    fn builds_from_the_maxmind_city_test_database() {
        let reader = test_db("GeoIP2-City-Test.mmdb");
        let (idx, stats) = CityIndex::build(&reader, 7, || false).unwrap().unwrap();
        assert_eq!(idx.generation, 7);
        // 源数据（`source-data/GeoIP2-City-Test.json`）里有 8 个不同的城市
        assert_eq!(idx.len(), 8);
        assert!(stats.networks >= stats.records && stats.records > 0);
        assert_eq!(stats.skipped, 0);
        // 有中文名：长春
        let cc = idx.get(2_038_180, NameLang::ZhCn).unwrap();
        assert_eq!(cc.name, "长春");
        assert_eq!(cc.country.as_deref(), Some("CN"));
        assert_eq!(idx.get(2_038_180, NameLang::En).unwrap().name, "Changchun");
        // 没有中文名：伦敦回退到英文
        let london = idx.get(2_643_743, NameLang::ZhCn).unwrap();
        assert_eq!(london.name, "London");
        assert_eq!(london.country.as_deref(), Some("GB"));
        // 非 ASCII 的英文名原样保留
        assert_eq!(idx.get(2_694_762, NameLang::En).unwrap().name, "Linköping");
        assert_eq!(idx.get(2_694_762, NameLang::ZhCn).unwrap().name, "林雪平");
        // 库里没有的 id
        assert_eq!(idx.get(1, NameLang::En), None);
        assert!(idx.heap_bytes() > 0);

        let names = idx.names([2_038_180, 2_643_743, 1], NameLang::ZhCn);
        assert_eq!(names.len(), 2);
        assert_eq!(names["2038180"].name, "长春");
        assert_eq!(
            serde_json::to_value(&names["2643743"]).unwrap(),
            serde_json::json!({"name": "London", "country": "GB"})
        );
    }

    #[test]
    fn country_databases_have_no_cities() {
        let reader = test_db("GeoIP2-Country-Test.mmdb");
        let (idx, _) = CityIndex::build(&reader, 1, || false).unwrap().unwrap();
        assert!(idx.is_empty());
    }

    /// 在一个真实大小的库上测建索引的耗时与内存（默认忽略）：
    ///
    /// ```text
    /// RIVU_CITY_INDEX_BENCH_MMDB=/path/to/city.mmdb     ///   cargo test --release -p rivu-geo --lib -- --ignored --nocapture measure_
    /// ```
    ///
    /// 库没有 geoname id（如 DB-IP City Lite）时，另用「国家 + 英文名」的哈希当键再建一次，
    /// 得到与真实城市数相当的表，用来估计内存。
    #[test]
    #[ignore = "需要一个真实大小的 City 库"]
    fn measure_build_cost_on_a_real_database() {
        let Ok(path) = std::env::var("RIVU_CITY_INDEX_BENCH_MMDB") else {
            eprintln!("没有设置 RIVU_CITY_INDEX_BENCH_MMDB，跳过");
            return;
        };
        // Linux 上进程的常驻内存与峰值（KiB），看建索引的临时开销
        let mem = || {
            let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
            let kib = |k: &str| {
                status
                    .lines()
                    .find_map(|l| l.strip_prefix(k))
                    .and_then(|v| v.trim().trim_end_matches("kB").trim().parse::<u64>().ok())
                    .unwrap_or(0)
            };
            (kib("VmRSS:"), kib("VmHWM:"))
        };
        let bytes = std::fs::read(&path).unwrap();
        let file_mib = bytes.len() as f64 / 1048576.0;
        let reader = Reader::from_source(bytes).unwrap();
        eprintln!(
            "库：{path}（{file_mib:.1} MiB，database_type = {}）",
            reader.metadata().database_type
        );
        let (rss0, _) = mem();
        let report = |label: &str, idx: &CityIndex, stats: &BuildStats, secs: f64| {
            let (rss, hwm) = mem();
            eprintln!(
                "{label}：{secs:.2} s，网段 {}，记录 {}，跳过 {}，城市 {}，表 {:.2} MiB；建之前常驻 {:.1} MiB，之后 {:.1} MiB，峰值 {:.1} MiB",
                stats.networks,
                stats.records,
                stats.skipped,
                idx.len(),
                idx.heap_bytes() as f64 / 1048576.0,
                rss0 as f64 / 1024.0,
                rss as f64 / 1024.0,
                hwm as f64 / 1024.0
            );
        };
        let t = std::time::Instant::now();
        let (idx, stats) = CityIndex::build(&reader, 1, || false).unwrap().unwrap();
        report("geoname_id 为键", &idx, &stats, t.elapsed().as_secs_f64());
        if idx.is_empty() {
            use std::hash::{BuildHasher, BuildHasherDefault, DefaultHasher};
            let hasher = BuildHasherDefault::<DefaultHasher>::default();
            let t = std::time::Instant::now();
            let (idx, stats) = CityIndex::build_with(
                &reader,
                1,
                || false,
                |d| {
                    let h = hasher.hash_one((d.country, d.en?));
                    Some((h as u32) | 1)
                },
            )
            .unwrap()
            .unwrap();
            report(
                "（国家, 英文名）哈希为键",
                &idx,
                &stats,
                t.elapsed().as_secs_f64(),
            );
        }
    }

    #[test]
    fn cancelled_builds_return_nothing() {
        // 第一个网段之前就检查一次
        let reader = test_db("GeoLite2-City-Test.mmdb");
        assert!(CityIndex::build(&reader, 1, || true).unwrap().is_none());
    }
}
