//! UA 解析（uap-core）与机器人识别（`system-design.md` §4.2 第 5、7 步，§4.3、§4.4）。
//!
//! 纯库，不接管线。输入是原始 `User-Agent` 头，输出 [`UaInfo`]：
//! `browser` / `browser_version` / `os` / `os_version` / `device` 可以直接填 `SessionAttrs`，
//! `bot` 为 `Some` 时管线应以 `DropReason::Bot` 丢弃。
//!
//! 数据与实现：
//! - 规则库是 uap-core 的 `regexes.yaml`，固定到一个 commit，随仓库提供并用 `include_str!` 嵌入
//!   （`data/uap-core/`，来源见该目录的 `PROVENANCE.md`）；
//! - 匹配用官方 Rust 实现 `ua-parser`（`regex-filtered` 预过滤），YAML 用纯 Rust 的 `serde-saphyr`；
//! - 结果按（截断后的）UA 字符串缓存在 moka 里：10 万条、TTL 1 小时（§4.3）。
//!
//! 研究、数据来源、实测数字：`docs/research/enrich-ua.md`。

mod bot;
mod device;

use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;

pub use bot::BotKind;
pub use device::DeviceClass;

/// 随仓库提供、固定 commit 的 uap-core 规则库。
const REGEXES_YAML: &str = include_str!("../data/uap-core/regexes.yaml");

/// 解析前把 UA 截断到这个字节数（在字符边界上）。
///
/// 依据：真实浏览器 UA 在 100–300 字节；uap-core 测试集 UA 的最大长度见研究文档，
/// crawler-user-agents 里最长的爬虫 UA 也在这个量级以内。上限有两个作用：
/// 1. 限制每次未命中的 CPU：正则库 `regex` 本身保证线性时间、不会灾难性回溯，
///    但 1270 条规则乘以输入长度仍是线性成本，不设上限时一个 1 MB 的 UA 就是 ~1 MB × 规则数；
/// 2. 限制缓存占用：key 是截断后的字符串，10 万条 × 512 B ≈ 50 MB 是最坏情况的上界。
///
/// 截断丢掉的是 UA 的尾部；尾部的 `Mobile Safari/537.36 (compatible; Googlebot/2.1)` 属于 300 字节之内，
/// 不受影响，超长 UA 本身就不正常。
pub const MAX_UA_BYTES: usize = 512;

/// 缓存容量与 TTL（`system-design.md` §4.3）。
pub const CACHE_CAPACITY: u64 = 100_000;
pub const CACHE_TTL: Duration = Duration::from_secs(60 * 60);

/// 浏览器名、系统名的长度上限：uap-core 有若干「把任意 token 当作家族名」的兜底规则，
/// 不设上限时攻击者可以用随机 UA 造出无限基数的维度值。
const MAX_NAME_BYTES: usize = 64;

/// 一个 UA 的解析结果。字段都是空串表示「未知」。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UaInfo {
    pub browser: String,
    /// `major.minor`（缺 minor 时只有 major）。
    pub browser_version: String,
    pub os: String,
    /// `major.minor`；UA 精简 / 冻结后不再携带信息的系统版本为空串（见 `device` 模块）。
    pub os_version: String,
    pub device: DeviceClass,
    pub bot: Option<BotKind>,
}

impl UaInfo {
    pub fn is_bot(&self) -> bool {
        self.bot.is_some()
    }

    fn empty() -> Self {
        Self {
            browser: String::new(),
            browser_version: String::new(),
            os: String::new(),
            os_version: String::new(),
            device: DeviceClass::Desktop,
            bot: Some(BotKind::Empty),
        }
    }
}

/// uap-core 的原始解析结果（族名未归一化，版本是前两段以外的都保留）。
///
/// 只用于一致性测试和排错，不是业务接口。`Other`（uap 的「未知」）按原样保留。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RawUa {
    pub browser: String,
    pub browser_major: Option<String>,
    pub browser_minor: Option<String>,
    pub browser_patch: Option<String>,
    pub os: String,
    pub os_major: Option<String>,
    pub os_minor: Option<String>,
    pub os_patch: Option<String>,
    pub os_patch_minor: Option<String>,
    pub device: String,
    pub device_brand: Option<String>,
    pub device_model: Option<String>,
}

pub struct UaParser {
    extractor: ua_parser::Extractor<'static>,
    cache: moka::sync::Cache<String, Arc<UaInfo>>,
    empty: Arc<UaInfo>,
}

impl UaParser {
    /// 用随仓库提供的 uap-core `regexes.yaml`，缓存取 §4.3 的默认值。
    pub fn embedded() -> anyhow::Result<Self> {
        Self::with_cache(CACHE_CAPACITY, CACHE_TTL)
    }

    /// 同 [`Self::embedded`]，但可指定缓存容量与 TTL（测试用）。
    pub fn with_cache(capacity: u64, ttl: Duration) -> anyhow::Result<Self> {
        let regexes: ua_parser::Regexes<'static> =
            serde_saphyr::from_str(REGEXES_YAML).context("解析内嵌的 uap-core regexes.yaml")?;
        let extractor = ua_parser::Extractor::try_from(regexes)
            .map_err(|e| anyhow::anyhow!("编译 uap-core 规则: {e}"))?;
        let cache = moka::sync::Cache::builder()
            .max_capacity(capacity)
            .time_to_live(ttl)
            .build();
        Ok(Self {
            extractor,
            cache,
            empty: Arc::new(UaInfo::empty()),
        })
    }

    /// 解析一个 UA，带缓存。永不失败：解析不出来的字段为空串，设备默认 Desktop。
    ///
    /// 先去掉首尾空白并截断到 [`MAX_UA_BYTES`]，缓存键是处理后的字符串；空串（空 UA）不进缓存，
    /// 直接返回同一个共享的 `Empty` 结果。命中缓存时不分配（`get` 用 `&str` 查）。
    pub fn parse(&self, ua: &str) -> Arc<UaInfo> {
        let ua = prepare(ua);
        if ua.is_empty() {
            return Arc::clone(&self.empty);
        }
        if let Some(hit) = self.cache.get(ua) {
            return hit;
        }
        let info = Arc::new(self.parse_uncached_prepared(ua));
        self.cache.insert(ua.to_owned(), Arc::clone(&info));
        info
    }

    /// 不经缓存解析（基准与测试用）。同样做首尾空白与长度处理。
    pub fn parse_uncached(&self, ua: &str) -> UaInfo {
        let ua = prepare(ua);
        if ua.is_empty() {
            return UaInfo::empty();
        }
        self.parse_uncached_prepared(ua)
    }

    /// uap-core 的原始结果，**不**截断、不归一化。一致性测试用。
    pub fn parse_raw(&self, ua: &str) -> RawUa {
        let (b, o, d) = self.extractor.extract(ua);
        let mut raw = RawUa {
            browser: "Other".into(),
            os: "Other".into(),
            device: "Other".into(),
            ..RawUa::default()
        };
        if let Some(b) = b {
            raw.browser = b.family.into_owned();
            raw.browser_major = b.major.map(str::to_owned);
            raw.browser_minor = b.minor.map(str::to_owned);
            raw.browser_patch = b.patch.map(str::to_owned);
        }
        if let Some(o) = o {
            raw.os = o.os.into_owned();
            raw.os_major = o.major.map(|c| c.into_owned());
            raw.os_minor = o.minor.map(|c| c.into_owned());
            raw.os_patch = o.patch.map(|c| c.into_owned());
            raw.os_patch_minor = o.patch_minor.map(|c| c.into_owned());
        }
        if let Some(d) = d {
            raw.device = d.device.into_owned();
            raw.device_brand = d.brand.map(|c| c.into_owned());
            raw.device_model = d.model.map(|c| c.into_owned());
        }
        raw
    }

    /// 缓存里现有的条目数（近似值，moka 的写入是异步结算的）。
    pub fn cache_entry_count(&self) -> u64 {
        self.cache.entry_count()
    }

    fn parse_uncached_prepared(&self, ua: &str) -> UaInfo {
        let raw = self.parse_raw(ua);
        let lower = ua.to_ascii_lowercase();

        let bot = bot::detect(&lower, raw.device == "Spider");

        let browser = clip(device::normalize_browser(&raw.browser));
        let browser_version =
            device::major_minor(raw.browser_major.as_deref(), raw.browser_minor.as_deref());
        let os = clip(device::normalize_os(&raw.os));
        let mut os_version = device::major_minor(raw.os_major.as_deref(), raw.os_minor.as_deref());
        if device::os_version_is_frozen(&lower, &raw.os, &os_version) {
            os_version.clear();
        } else if raw.os == "iOS" && raw.browser == "Mobile Safari" {
            // Safari 26 起 UA 里的 iOS 版本冻结在 18.x，真实版本只有 `Version/26.x`
            // （26 起 Safari 与 iOS 版本号一致）。
            if let Some(v) = safari26_ios_version(&raw, &os_version) {
                os_version = v;
            }
        }

        UaInfo {
            browser,
            browser_version,
            os,
            os_version,
            device: device::classify(&lower, &raw.os, &raw.device),
            bot,
        }
    }
}

/// iOS Safari 26+：UA 里的系统版本被冻结在 18.x，用 Safari 版本（`Version/N`）代替。
fn safari26_ios_version(raw: &RawUa, os_version: &str) -> Option<String> {
    let safari_major: u32 = raw.browser_major.as_deref()?.parse().ok()?;
    let os_major: u32 = os_version.split('.').next()?.parse().ok()?;
    (safari_major >= 26 && os_major == 18)
        .then(|| device::major_minor(raw.browser_major.as_deref(), raw.browser_minor.as_deref()))
}

/// 去首尾空白，再按 [`MAX_UA_BYTES`] 在字符边界上截断。
fn prepare(ua: &str) -> &str {
    let ua = ua.trim();
    &ua[..ua.floor_char_boundary(MAX_UA_BYTES)]
}

fn clip(s: &str) -> String {
    s[..s.floor_char_boundary(MAX_NAME_BYTES)].to_owned()
}
