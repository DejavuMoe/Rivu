//! mmdb 查询、署名与热加载（`system-design.md` §4.2 第 6 步、§4.3、§4.4）。
//!
//! # 为什么不用内存映射
//!
//! §4.3 写的是「内存映射的 mmdb」，但 `maxminddb` 的 mmap 入口 `Reader::open_mmap` 是 `unsafe fn`
//! （映射期间文件被截断或原地改写会得到未定义行为，在 Linux 上通常是 SIGBUS），而工作区禁止
//! `unsafe_code`。所以这里把文件整个读进堆（`Reader<Vec<u8>>`）：City 库解压后约 121 MB，
//! Country 库约 8 MB，每个加载它的进程各占一份。热加载时新旧两份同时存在，旧的在途查询结束后释放。
//! 我们自己的替换是「写临时文件再 `rename`」，旧映射本来也安全；风险只在运维者原地改写库文件，
//! 这正是不用 mmap 想避开的。内存吃紧的部署可以选 DB-IP country 版。

use std::collections::BTreeMap;
use std::fs::File;
use std::io::Read;
use std::net::{IpAddr, Ipv4Addr};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, ensure};
use arc_swap::ArcSwapOption;
use maxminddb::Reader;
use rivu_common::config::GeoConfig;
use serde::Deserialize;
use tokio::sync::watch;
use tokio::task::JoinHandle;

use crate::attribution::{Attribution, SOURCE_CUSTOM, SOURCE_DBIP};
use crate::cities::{CityIndex, CityName, NameLang};
use crate::meta;

/// 一次查询的地理信息；查不到时全为空值。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GeoInfo {
    /// ISO 3166-1 alpha-2，大写；没有为 `[0, 0]`。
    pub country_code: [u8; 2],
    /// ISO 3166-2，如 `CN-31`；没有为空串。
    pub region_code: String,
    /// GeoNames id；没有为 0。
    pub city_geoname_id: u32,
}

impl GeoInfo {
    pub fn is_empty(&self) -> bool {
        self.country_code == [0, 0] && self.region_code.is_empty() && self.city_geoname_id == 0
    }
}

/// 站点设置 `geo_level`：按这个精度截断，更细的字段不存。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum GeoLevel {
    Country,
    Region,
    City,
}

impl GeoLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            GeoLevel::Country => "country",
            GeoLevel::Region => "region",
            GeoLevel::City => "city",
        }
    }
}

impl std::str::FromStr for GeoLevel {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "country" => Ok(GeoLevel::Country),
            "region" => Ok(GeoLevel::Region),
            "city" => Ok(GeoLevel::City),
            other => Err(format!(
                "`{other}` 不是有效的 geo_level，可选 country | region | city"
            )),
        }
    }
}

/// 当前库能提供的地理精度（`system-design.md` §6.10 的 `meta.geo_capability`）。
/// 看板据此判断站点的 `geo_level`（region / city）是否超出库的能力，并提示运维者。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum GeoCapability {
    /// 只有国家：DB-IP Lite（city 版也没有 ISO 子区划码与 geoname id），或任何 Country 版库。
    Country,
    /// 含区域与城市：MaxMind City、自定义 City 库。（自定义库若缺子区划代码，地区报表仍为空，
    /// 提示不会触发，这是已知盲区。）
    City,
}

impl GeoCapability {
    pub fn as_str(self) -> &'static str {
        match self {
            GeoCapability::Country => "country",
            GeoCapability::City => "city",
        }
    }
}

/// 判定规则：来源为 `dbip-lite`，或库类型（`database_type`，不区分大小写）含 `Country` → `Country`；其余 `City`。
pub fn capability_of(source: &str, database_type: &str) -> GeoCapability {
    if source == SOURCE_DBIP || database_type.to_ascii_lowercase().contains("country") {
        GeoCapability::Country
    } else {
        GeoCapability::City
    }
}

/// 只解码用到的字段；其余字段由解码器跳过。同一份结构同时适用于 Country 和 City 库。
#[derive(Deserialize, Default)]
struct Record<'a> {
    #[serde(borrow, default)]
    country: Option<CountryRec<'a>>,
    #[serde(default)]
    city: Option<CityRec>,
    #[serde(borrow, default)]
    subdivisions: Vec<SubdivisionRec<'a>>,
}

#[derive(Deserialize, Default)]
struct CountryRec<'a> {
    #[serde(borrow, default)]
    iso_code: Option<&'a str>,
}

#[derive(Deserialize, Default)]
struct CityRec {
    #[serde(default)]
    geoname_id: Option<u32>,
}

#[derive(Deserialize, Default)]
struct SubdivisionRec<'a> {
    #[serde(borrow, default)]
    iso_code: Option<&'a str>,
}

/// 两个字母的国家码，大写。非字母（`A1`、`A2`、`T1` 等匿名代理、卫星、Tor 占位码）和
/// 未知占位码 `ZZ`、`XX` 都算没有（与 Plausible 的 `geolocation.ex` 一致，R01 §5）。
pub(crate) fn country_code(raw: &str) -> [u8; 2] {
    let b = raw.as_bytes();
    if b.len() != 2 || !b.iter().all(u8::is_ascii_alphabetic) {
        return [0, 0];
    }
    let code = [b[0].to_ascii_uppercase(), b[1].to_ascii_uppercase()];
    if code == *b"ZZ" || code == *b"XX" {
        return [0, 0];
    }
    code
}

/// ISO 3166-2 的子区划部分是 1 到 3 位字母数字。
fn region_code(country: [u8; 2], raw: &str) -> Option<String> {
    let ok = (1..=3).contains(&raw.len()) && raw.bytes().all(|b| b.is_ascii_alphanumeric());
    ok.then(|| {
        format!(
            "{}{}-{}",
            country[0] as char,
            country[1] as char,
            raw.to_ascii_uppercase()
        )
    })
}

/// 能进 GeoIP 库的公网地址？私有、回环、链路本地、CGNAT、文档、保留、组播地址直接返回空值，
/// 不去查库（库里没有这些段，查了也只是白费一次树遍历，还可能被伪造的 XFF 利用）。
/// 调用方已经把 IPv4-mapped IPv6 转成 IPv4。
fn is_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(a) => {
            let o = a.octets();
            !(a.is_unspecified()
                || a.is_loopback()
                || a.is_private()
                || a.is_link_local()
                || a.is_broadcast()
                || a.is_multicast()
                || a.is_documentation()
                || o[0] == 0 // 0.0.0.0/8
                || (o[0] == 100 && (o[1] & 0xc0) == 64) // 100.64.0.0/10 CGNAT
                || (o[0] == 192 && o[1] == 0 && o[2] == 0) // 192.0.0.0/24 IETF 协议分配
                || (o[0] == 198 && (o[1] & 0xfe) == 18) // 198.18.0.0/15 基准测试
                || o[0] >= 240) // 240.0.0.0/4 保留
        }
        IpAddr::V6(a) => {
            let s = a.segments();
            !(a.is_unspecified()
                || a.is_loopback()
                || a.is_multicast()
                || (s[0] & 0xfe00) == 0xfc00 // fc00::/7 ULA
                || (s[0] & 0xffc0) == 0xfe80 // fe80::/10 链路本地
                || (s[0] == 0x2001 && s[1] == 0x0db8) // 2001:db8::/32 文档
                || (s[0] == 0x0100 && s[1] == 0 && s[2] == 0 && s[3] == 0) // 100::/64 丢弃
                || s[..6] == [0; 6]) // ::/96 IPv4 兼容（已废弃）
        }
    }
}

fn lookup_in(reader: &Reader<Vec<u8>>, ip: IpAddr, level: GeoLevel) -> GeoInfo {
    let Ok(result) = reader.lookup(ip) else {
        return GeoInfo::default();
    };
    if !result.has_data() {
        return GeoInfo::default();
    }
    let Ok(Some(rec)) = result.decode::<Record>() else {
        return GeoInfo::default();
    };
    let cc = rec
        .country
        .and_then(|c| c.iso_code)
        .map_or([0, 0], country_code);
    if cc == [0, 0] {
        // 没有国家就没有地区和城市（Plausible 同样如此）
        return GeoInfo::default();
    }
    let mut info = GeoInfo {
        country_code: cc,
        ..GeoInfo::default()
    };
    if level >= GeoLevel::Region {
        info.region_code = rec
            .subdivisions
            .first()
            .and_then(|s| s.iso_code)
            .and_then(|code| region_code(cc, code))
            .unwrap_or_default();
    }
    if level >= GeoLevel::City {
        info.city_geoname_id = rec.city.and_then(|c| c.geoname_id).unwrap_or(0);
    }
    info
}

/// 文件的「版本」：长度与修改时间。热加载靠它判断是否变化。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FileKey {
    len: u64,
    modified: Option<SystemTime>,
}

/// 校验 mmdb（`system-design.md` §4.4「校验」）：元数据可解析（`from_source` 已保证）、
/// `binary_format_major_version = 2`、`database_type` 含 `Country` 或 `City`（不区分大小写，
/// 覆盖 `GeoLite2-*` 与 `DBIP-*-Lite`）、`node_count > 0`、对固定公网地址查询一次且解码不出错。
pub(crate) fn validate(reader: &Reader<Vec<u8>>) -> Result<()> {
    let m = reader.metadata();
    ensure!(
        m.binary_format_major_version == 2,
        "binary_format_major_version = {}，只支持 2",
        m.binary_format_major_version
    );
    let ty = m.database_type.to_ascii_lowercase();
    ensure!(
        ty.contains("country") || ty.contains("city"),
        "database_type = `{}` 不是 Country 或 City 库",
        m.database_type
    );
    ensure!(m.node_count > 0, "node_count 为 0");
    let probe = IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8));
    let result = reader.lookup(probe).context("查询探测地址失败")?;
    if result.has_data() {
        result
            .decode::<Record>()
            .context("探测地址的记录解码失败")?;
    }
    Ok(())
}

/// 读入并校验一个 mmdb 文件。
pub(crate) fn open_validated(path: &Path) -> Result<(Reader<Vec<u8>>, FileKey)> {
    let mut file = File::open(path).with_context(|| format!("打开 {} 失败", path.display()))?;
    let md = file.metadata()?;
    let key = FileKey {
        len: md.len(),
        modified: md.modified().ok(),
    };
    let mut buf = Vec::with_capacity(usize::try_from(md.len()).unwrap_or(0));
    file.read_to_end(&mut buf)
        .with_context(|| format!("读取 {} 失败", path.display()))?;
    let reader = Reader::from_source(buf).context("不是有效的 mmdb 文件")?;
    validate(&reader)?;
    Ok((reader, key))
}

struct Db {
    reader: Reader<Vec<u8>>,
    key: FileKey,
    /// 第几次加载（从 1 起）：城市名索引据此判断自己属于哪个库、是否已被新库取代。
    generation: u64,
}

/// 城市名索引的状态（`system-design.md` §4.4「城市名」）。单独放在 `Arc` 里，后台建索引的线程只持有它和
/// 正在建索引的那个库。
#[derive(Default)]
struct Cities {
    index: ArcSwapOption<CityIndex>,
    /// 最近一次加载的库的代数。
    generation: AtomicU64,
    /// 同一时刻只建一份索引：新库加载时旧的建索引线程会放弃。
    build: Mutex<()>,
}

/// 当前库的来源与署名。
#[derive(Debug, Clone, PartialEq, Eq)]
struct Origin {
    source: &'static str,
    build_epoch: u64,
    database_type: String,
    attribution: Option<Attribution>,
}

/// 库的运行状态，给 `/readyz` 用。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeoStatus {
    pub loaded: bool,
    pub source: Option<&'static str>,
    pub database_type: Option<String>,
    /// 库的构建时间（Unix 秒）。
    pub build_epoch: Option<u64>,
    /// 距构建时间的秒数。
    pub age_seconds: Option<u64>,
}

/// 当前加载的 GeoIP 库。`ArcSwap` 持有 reader 与署名；未加载时 `lookup` 返回空值。
///
/// 查询是只读的，可以在任意线程并发调用；热加载不阻塞查询。
pub struct GeoDb {
    db: ArcSwapOption<Db>,
    origin: ArcSwapOption<Origin>,
    /// 手工放置的库（没有 meta）使用的署名，来自 `RIVU_GEOIP_ATTRIBUTION_*`。
    manual_attribution: Option<Attribution>,
    cities: Arc<Cities>,
    /// 加载 City 库后是否自动在后台建城市名索引（[`GeoDb::with_city_names`]）。
    auto_city_names: bool,
}

impl std::fmt::Debug for GeoDb {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GeoDb")
            .field("status", &self.status())
            .finish()
    }
}

impl Default for GeoDb {
    fn default() -> Self {
        Self::new()
    }
}

impl GeoDb {
    /// 空库：`lookup` 返回空值，`attribution()` 为 `None`。
    pub fn new() -> Self {
        GeoDb {
            db: ArcSwapOption::empty(),
            origin: ArcSwapOption::empty(),
            manual_attribution: None,
            cities: Arc::default(),
            auto_city_names: false,
        }
    }

    /// 每次加载 City 库后在后台线程建城市名索引（`rivu serve` 用）。不开时只能用
    /// [`GeoDb::build_city_index`] 同步建（命令行与测试用）。
    #[must_use]
    pub fn with_city_names(mut self) -> Self {
        self.auto_city_names = true;
        self
    }

    /// 同 [`GeoDb::new`]，并指定手工放置文件的署名。
    pub fn with_manual_attribution(text: Option<&str>, url: Option<&str>) -> Self {
        GeoDb {
            manual_attribution: Attribution::custom(text, url),
            ..Self::new()
        }
    }

    pub fn from_config(cfg: &GeoConfig) -> Self {
        Self::with_manual_attribution(
            cfg.attribution_text.as_deref(),
            cfg.attribution_url.as_deref(),
        )
    }

    /// 查询。先把 IPv4-mapped IPv6 转成 IPv4；私有与保留地址、库未加载、查不到、解码失败都返回空值。
    pub fn lookup(&self, ip: IpAddr, level: GeoLevel) -> GeoInfo {
        let ip = ip.to_canonical();
        if !is_public(ip) {
            return GeoInfo::default();
        }
        match &*self.db.load() {
            Some(db) => lookup_in(&db.reader, ip, level),
            None => GeoInfo::default(),
        }
    }

    /// 当前生效库的署名。跟随**实际加载的库**：库来自 meta 里记录的来源；没有 meta（或 meta 与库不匹配，
    /// 说明文件被手工换过）的库取手工署名。未加载时为 `None`。
    pub fn attribution(&self) -> Option<Attribution> {
        self.origin
            .load()
            .as_ref()
            .and_then(|o| o.attribution.clone())
    }

    /// 当前加载的库的地理精度（`meta.geo_capability`）；未加载时为 `None`。
    /// 取自**实际加载的库**（来源与 `database_type`），不是配置。
    pub fn capability(&self) -> Option<GeoCapability> {
        self.origin
            .load()
            .as_ref()
            .filter(|_| self.is_loaded())
            .map(|o| capability_of(o.source, &o.database_type))
    }

    pub fn is_loaded(&self) -> bool {
        self.db.load().is_some()
    }

    /// 城市名索引是否可用：当前库是 City 级，且索引已经建好。
    pub fn city_names_ready(&self) -> bool {
        self.capability() == Some(GeoCapability::City) && self.cities.index.load().is_some()
    }

    /// 一批 GeoNames id 的城市名（`meta.city_names`），键是 id 的十进制字符串。索引没建好、库不是 City 级时
    /// 返回 `None`（调用方省略该字段）；建好了但一个也查不到时返回空表。id 为 0（未知城市）不查。
    pub fn city_names(
        &self,
        ids: impl IntoIterator<Item = u32>,
        lang: NameLang,
    ) -> Option<BTreeMap<String, CityName>> {
        if self.capability() != Some(GeoCapability::City) {
            return None;
        }
        let index = self.cities.index.load_full()?;
        Some(index.names(ids.into_iter().filter(|id| *id != 0), lang))
    }

    /// 为当前库同步建城市名索引，建好返回 `true`。库没加载、不是 City 级、索引已是当前库的、或建到一半被
    /// 新库取代时返回 `false`。耗时与库大小成正比（GeoLite2 City 约几秒），所以 `rivu serve` 在后台线程里
    /// 调用（[`GeoDb::with_city_names`]），不阻塞启动、`/readyz` 与采集。
    pub fn build_city_index(&self) -> Result<bool> {
        let Some(db) = self.db.load_full() else {
            return Ok(false);
        };
        if self.capability() != Some(GeoCapability::City) {
            self.cities.index.store(None);
            return Ok(false);
        }
        build_city_index_for(&self.cities, &db)
    }

    fn spawn_city_index_build(&self) {
        let Some(db) = self.db.load_full() else {
            return;
        };
        let cities = Arc::clone(&self.cities);
        let spawned = std::thread::Builder::new()
            .name("rivu-geo-cities".into())
            .spawn(move || {
                if let Err(e) = build_city_index_for(&cities, &db) {
                    tracing::warn!(error = %format!("{e:#}"), "建城市名索引失败，城市只显示 id");
                }
            });
        if let Err(e) = spawned {
            tracing::warn!(error = %e, "无法启动建城市名索引的线程，城市只显示 id");
        }
    }

    pub fn status(&self) -> GeoStatus {
        match &*self.origin.load() {
            Some(o) if self.is_loaded() => GeoStatus {
                loaded: true,
                source: Some(o.source),
                database_type: Some(o.database_type.clone()),
                build_epoch: Some(o.build_epoch),
                age_seconds: Some(now_epoch().saturating_sub(o.build_epoch)),
            },
            _ => GeoStatus {
                loaded: false,
                source: None,
                database_type: None,
                build_epoch: None,
                age_seconds: None,
            },
        }
    }

    /// 文件的长度或修改时间变了就重新读入、校验并原子替换，返回 `true`。
    /// 文件不存在返回 `Ok(false)`（保留已加载的库）；读入或校验失败返回错误并保留旧库。
    /// 无论库是否重载，都会按 meta 刷新署名（刷新任务收到 304 时只会改 meta）。
    pub fn reload_if_changed(&self, path: &Path) -> Result<bool> {
        let md = match std::fs::metadata(path) {
            Ok(md) => md,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(e) => return Err(e).with_context(|| format!("读取 {} 的状态失败", path.display())),
        };
        let key = FileKey {
            len: md.len(),
            modified: md.modified().ok(),
        };
        let unchanged = self.db.load().as_ref().is_some_and(|d| d.key == key);
        let mut reloaded = false;
        if !unchanged {
            // 先读入并校验，成功后才替换：失败时继续用旧库。
            let (reader, key) = open_validated(path)?;
            let build_epoch = reader.metadata().build_epoch;
            let database_type = reader.metadata().database_type.clone();
            self.origin.store(Some(Arc::new(self.origin_for(
                path,
                build_epoch,
                database_type,
            ))));
            let generation = self.cities.generation.fetch_add(1, Ordering::SeqCst) + 1;
            self.db.store(Some(Arc::new(Db {
                reader,
                key,
                generation,
            })));
            reloaded = true;
            if self.capability() == Some(GeoCapability::City) {
                // 旧索引在新索引建好前继续提供名字（城市的 id 跨版本不变），建好后替换
                if self.auto_city_names {
                    self.spawn_city_index_build();
                }
            } else {
                self.cities.index.store(None);
            }
        } else if let Some(cur) = self.origin.load_full() {
            let refreshed = self.origin_for(path, cur.build_epoch, cur.database_type.clone());
            if refreshed != *cur {
                self.origin.store(Some(Arc::new(refreshed)));
            }
        }
        self.publish_metrics();
        Ok(reloaded)
    }

    fn origin_for(&self, path: &Path, build_epoch: u64, database_type: String) -> Origin {
        match meta::read(path) {
            // meta 的 build_epoch 必须与库一致，否则库被手工换过，meta 描述的是另一个文件
            Some(m) if m.build_epoch == build_epoch => {
                let attribution = Attribution::from_stored(&m.source, m.attribution.as_ref());
                let source = attribution.as_ref().map_or(SOURCE_CUSTOM, |a| a.source);
                Origin {
                    source,
                    build_epoch,
                    database_type,
                    attribution,
                }
            }
            _ => Origin {
                source: SOURCE_CUSTOM,
                build_epoch,
                database_type,
                attribution: self.manual_attribution.clone(),
            },
        }
    }

    /// 更新 `rivu_geoip_loaded` 与 `rivu_geoip_db_age_seconds`（`platform-and-ops.md` §10）。
    pub fn publish_metrics(&self) {
        let s = self.status();
        metrics::gauge!("rivu_geoip_loaded").set(if s.loaded { 1.0 } else { 0.0 });
        if let Some(age) = s.age_seconds {
            metrics::gauge!("rivu_geoip_db_age_seconds").set(age as f64);
        }
    }

    /// 启动热加载任务：立即检查一次（启动加载），之后每 `interval`（`platform-and-ops.md`：每小时）
    /// 比较一次文件，直到收到停止信号。读文件放在 `spawn_blocking` 里，不占用异步工作线程。
    pub fn spawn_reloader(
        self: &Arc<Self>,
        path: PathBuf,
        interval: Duration,
        mut stop: watch::Receiver<bool>,
    ) -> JoinHandle<()> {
        let db = Arc::clone(self);
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(interval);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                tokio::select! {
                    () = async { let _ = stop.wait_for(|s| *s).await; } => break,
                    _ = ticker.tick() => {
                        let (worker, path) = (Arc::clone(&db), path.clone());
                        match tokio::task::spawn_blocking(move || worker.reload_if_changed(&path)).await {
                            Ok(Ok(true)) => {
                                let s = db.status();
                                tracing::info!(
                                    source = s.source,
                                    database_type = s.database_type,
                                    build_epoch = s.build_epoch,
                                    "GeoIP 库已加载"
                                );
                            }
                            Ok(Ok(false)) => {}
                            Ok(Err(e)) => {
                                tracing::warn!(error = %format!("{e:#}"), "GeoIP 库热加载失败，继续使用旧库");
                            }
                            Err(e) => tracing::error!(error = %e, "GeoIP 热加载任务异常"),
                        }
                    }
                }
            }
        })
    }
}

/// 为库 `db` 建索引并发布。同一时刻只有一个线程在建；`db` 已被新库取代时放弃。
fn build_city_index_for(cities: &Cities, db: &Db) -> Result<bool> {
    let _guard = cities
        .build
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let superseded = || cities.generation.load(Ordering::SeqCst) != db.generation;
    if superseded()
        || cities
            .index
            .load()
            .as_ref()
            .is_some_and(|i| i.generation == db.generation)
    {
        return Ok(false);
    }
    let started = Instant::now();
    let built = CityIndex::build(&db.reader, db.generation, superseded);
    let (index, stats) = match built {
        Ok(Some(b)) => b,
        Ok(None) => return Ok(false),
        Err(e) => {
            // 不留旧库的索引冒充新库的
            if !superseded() {
                cities.index.store(None);
            }
            return Err(e);
        }
    };
    if superseded() {
        return Ok(false);
    }
    tracing::info!(
        cities = index.len(),
        bytes = index.heap_bytes(),
        networks = stats.networks,
        records = stats.records,
        skipped = stats.skipped,
        elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        "城市名索引已建好"
    );
    cities.index.store(Some(Arc::new(index)));
    Ok(true)
}

fn now_epoch() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// 注册 GeoIP 指标的说明（`platform-and-ops.md` §10）。
pub fn describe_metrics() {
    metrics::describe_counter!(
        "rivu_geoip_refresh_total",
        "GeoIP 库刷新次数，按来源与结果（updated | not_modified | failed）"
    );
    metrics::describe_gauge!("rivu_geoip_loaded", "GeoIP 库是否已加载（0/1）");
    metrics::describe_gauge!(
        "rivu_geoip_db_age_seconds",
        "已加载 GeoIP 库的年龄（秒，按库的 build_epoch）"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn country_codes_are_normalized() {
        assert_eq!(country_code("CN"), *b"CN");
        assert_eq!(country_code("cn"), *b"CN");
        for bad in ["", "C", "CHN", "A1", "T1", "ZZ", "XX", "中国", "1A"] {
            assert_eq!(country_code(bad), [0, 0], "{bad}");
        }
    }

    #[test]
    fn region_codes_are_country_dash_subdivision() {
        assert_eq!(region_code(*b"CN", "31").as_deref(), Some("CN-31"));
        assert_eq!(region_code(*b"GB", "eng").as_deref(), Some("GB-ENG"));
        for bad in ["", "ABCD", "3-1", "é"] {
            assert_eq!(region_code(*b"CN", bad), None, "{bad}");
        }
    }

    #[test]
    fn reserved_and_private_addresses_are_not_public() {
        for s in [
            "0.0.0.0",
            "0.1.2.3",
            "10.0.0.1",
            "127.0.0.1",
            "169.254.1.1",
            "172.16.0.1",
            "172.31.255.255",
            "192.168.1.1",
            "100.64.0.1",
            "100.127.255.255",
            "192.0.0.1",
            "192.0.2.1",
            "198.18.0.1",
            "198.19.255.255",
            "198.51.100.1",
            "203.0.113.1",
            "224.0.0.1",
            "239.255.255.255",
            "240.0.0.1",
            "255.255.255.255",
            "::",
            "::1",
            "fc00::1",
            "fd12:3456::1",
            "fe80::1",
            "ff02::1",
            "2001:db8::1",
            "100::1",
            "::1.2.3.4",
        ] {
            assert!(!is_public(s.parse().unwrap()), "{s} 应当是非公网地址");
        }
        for s in [
            "1.1.1.1",
            "8.8.8.8",
            "100.63.255.255",
            "100.128.0.1",
            "172.15.255.255",
            "172.32.0.1",
            "192.0.1.1",
            "198.17.255.255",
            "198.20.0.1",
            "223.255.255.255",
            "2001:4860:4860::8888",
            "2a02:cf40::1",
            "2001:218::1",
        ] {
            assert!(is_public(s.parse().unwrap()), "{s} 应当是公网地址");
        }
    }

    #[test]
    fn capability_follows_source_and_database_type() {
        use crate::attribution::SOURCE_MAXMIND;
        // DB-IP Lite：city 版也只有国家
        assert_eq!(
            capability_of(SOURCE_DBIP, "DBIP-City-Lite"),
            GeoCapability::Country
        );
        assert_eq!(
            capability_of(SOURCE_DBIP, "DBIP-Country-Lite"),
            GeoCapability::Country
        );
        // MaxMind：City 版 -> city，Country 版 -> country
        assert_eq!(
            capability_of(SOURCE_MAXMIND, "GeoLite2-City"),
            GeoCapability::City
        );
        assert_eq!(
            capability_of(SOURCE_MAXMIND, "GeoLite2-Country"),
            GeoCapability::Country
        );
        // 自定义库看库类型（不区分大小写）
        assert_eq!(
            capability_of(SOURCE_CUSTOM, "GeoIP2-City"),
            GeoCapability::City
        );
        assert_eq!(
            capability_of(SOURCE_CUSTOM, "MyCOUNTRY-db"),
            GeoCapability::Country
        );
        assert_eq!(GeoCapability::Country.as_str(), "country");
        assert_eq!(GeoCapability::City.as_str(), "city");
        assert!(GeoCapability::Country < GeoCapability::City);
    }

    #[test]
    fn capability_comes_from_the_loaded_database() {
        // 未加载：没有能力可报
        assert_eq!(GeoDb::new().capability(), None);
        // 手工放置的 City 测试库（没有 meta -> 来源 custom）-> city；Country 测试库 -> country
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/test-data");
        for (file, want) in [
            ("GeoIP2-City-Test.mmdb", GeoCapability::City),
            ("GeoIP2-Country-Test.mmdb", GeoCapability::Country),
        ] {
            let db = GeoDb::new();
            assert!(db.reload_if_changed(&dir.join(file)).unwrap());
            assert_eq!(db.capability(), Some(want), "{file}");
        }
    }

    fn test_db_path(file: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/data/test-data")
            .join(file)
    }

    #[test]
    fn city_names_come_from_the_loaded_city_database() {
        let db = GeoDb::new();
        // 没加载库：没有名字
        assert_eq!(db.city_names([2_038_180], NameLang::En), None);
        assert!(!db.build_city_index().unwrap());

        assert!(
            db.reload_if_changed(&test_db_path("GeoIP2-City-Test.mmdb"))
                .unwrap()
        );
        // 没开后台建索引：索引建好之前名字缺席，不报错
        assert!(!db.city_names_ready());
        assert_eq!(db.city_names([2_038_180], NameLang::En), None);

        assert!(db.build_city_index().unwrap());
        // 已是当前库的索引：不重建
        assert!(!db.build_city_index().unwrap());
        assert!(db.city_names_ready());
        let names = db
            .city_names([2_038_180, 2_643_743, 0, 42], NameLang::ZhCn)
            .unwrap();
        assert_eq!(
            serde_json::to_value(&names).unwrap(),
            serde_json::json!({
                "2038180": {"name": "长春", "country": "CN"},
                "2643743": {"name": "London", "country": "GB"},
            })
        );
        // 建好了但一个也查不到：空表（与「还没建好」区分开）
        assert_eq!(db.city_names([42], NameLang::En), Some(BTreeMap::new()));

        // 换成 Country 库：索引随即丢弃
        assert!(
            db.reload_if_changed(&test_db_path("GeoIP2-Country-Test.mmdb"))
                .unwrap()
        );
        assert!(!db.city_names_ready());
        assert_eq!(db.city_names([2_038_180], NameLang::En), None);
        assert!(!db.build_city_index().unwrap());
        assert!(db.cities.index.load().is_none());
    }

    #[test]
    fn city_names_are_built_in_the_background_after_each_load() {
        let db = GeoDb::new().with_city_names();
        assert!(
            db.reload_if_changed(&test_db_path("GeoLite2-City-Test.mmdb"))
                .unwrap()
        );
        let deadline = Instant::now() + Duration::from_secs(10);
        while !db.city_names_ready() {
            assert!(Instant::now() < deadline, "后台建索引超时");
            std::thread::sleep(Duration::from_millis(5));
        }
        let first = db.cities.index.load_full().unwrap();
        assert_eq!(first.generation, 1);
        assert_eq!(
            db.city_names([2_694_762], NameLang::ZhCn).unwrap()["2694762"].name,
            "林雪平"
        );
        // 热加载另一个 City 库：新索引建好前旧索引继续提供名字，之后换成新库的
        assert!(
            db.reload_if_changed(&test_db_path("GeoIP2-City-Test.mmdb"))
                .unwrap()
        );
        while db
            .cities
            .index
            .load()
            .as_ref()
            .is_none_or(|i| i.generation != 2)
        {
            assert!(db.city_names_ready(), "重建期间名字不应缺席");
            assert!(Instant::now() < deadline, "后台重建索引超时");
            std::thread::sleep(Duration::from_millis(5));
        }
        // 只在 GeoIP2 测试库里的城市（新加坡）
        assert_eq!(
            db.city_names([1_880_252], NameLang::ZhCn).unwrap()["1880252"].name,
            "新加坡"
        );
    }

    #[test]
    fn geo_level_parses_and_orders() {
        assert_eq!("region".parse::<GeoLevel>(), Ok(GeoLevel::Region));
        assert!("planet".parse::<GeoLevel>().is_err());
        assert!(GeoLevel::Country < GeoLevel::Region && GeoLevel::Region < GeoLevel::City);
        assert_eq!(GeoLevel::City.as_str(), "city");
    }

    #[test]
    fn empty_db_returns_empty_values() {
        let db = GeoDb::new();
        assert!(!db.is_loaded());
        assert_eq!(db.attribution(), None);
        let info = db.lookup("8.8.8.8".parse().unwrap(), GeoLevel::City);
        assert!(info.is_empty());
        assert!(!db.status().loaded);
        // 文件不存在不是错误
        assert!(
            !db.reload_if_changed(Path::new("/definitely/not/here.mmdb"))
                .unwrap()
        );
    }
}
