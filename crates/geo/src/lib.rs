//! GeoIP：mmdb 读取、下载刷新与署名（`system-design.md` §4.4「GeoIP 数据源、更新与署名（Q5）」）。
//!
//! - [`GeoDb`]：`ArcSwap` 持有当前 reader 与其署名；按文件修改时间热加载；未加载时查询返回空值。
//! - [`refresh`]：对配置的来源（自定义 URL > MaxMind GeoLite2 > DB-IP Lite）做条件 GET，校验后原子替换。
//! - [`Attribution`]：署名，跟随实际加载的库。
//!
//! 管线接入（第 6 步「地理」）另行进行；刷新任务在 `rivu-jobs` 里（`geoip_refresh`）。
//!
//! [`datacenter`]：可选的数据中心网段表（第 5 步，`datacenter_ip`），同样是「下载 → 文件 → 热加载」。

mod attribution;
pub mod datacenter;
mod db;
mod meta;
mod refresh;

pub use attribution::{Attribution, SOURCE_CUSTOM, SOURCE_DBIP, SOURCE_MAXMIND, StoredAttribution};
pub use db::{GeoCapability, GeoDb, GeoInfo, GeoLevel, GeoStatus, capability_of, describe_metrics};
pub use meta::{META_SUFFIX, Meta, meta_path, read as read_meta, write as write_meta};
pub use refresh::{
    DBIP_BASE_URL, HttpClient, HttpOptions, LocalCopy, MAX_DOWNLOAD_BYTES, MAX_REDIRECTS,
    MAXMIND_BASE_URL, RefreshOptions, RefreshOutcome, dbip_urls, local_copy, refresh, refresh_with,
};
