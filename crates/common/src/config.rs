//! 配置：全部来自 `RIVU_` 前缀的环境变量，每个变量都支持 `<NAME>_FILE`。
//!
//! 规则（docs/architecture/platform-and-ops.md §7）：
//! - `X` 和 `X_FILE` 同时设置视为冲突，拒绝启动；
//! - `X_FILE` 指向的文件内容去掉首尾空白后作为值；
//! - 值为空字符串等同于未设置（compose 里常见 `X=`）；
//! - 启动时集中校验，一次性报告全部问题；按子命令只校验所需变量；
//! - 程序本身不读 `.env` 文件，由调用方（shell、compose）加载。

use std::collections::HashMap;
use std::fmt;
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::str::FromStr;
use std::time::Duration;

use crate::telemetry::check_level;

/// 子命令对应的角色。不同角色要求的变量不同。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Collector,
    Worker,
    Api,
    Migrate,
    All,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Role::Collector => "collector",
            Role::Worker => "worker",
            Role::Api => "api",
            Role::Migrate => "migrate",
            Role::All => "all",
        }
    }

    /// 是否挂载 collector 的公共路由（`POST /api/event`）。
    pub fn serves_collector(self) -> bool {
        matches!(self, Role::Collector | Role::All)
    }

    /// 是否挂载 api 的公共路由（`/api/*`）。
    pub fn serves_api(self) -> bool {
        matches!(self, Role::Api | Role::All)
    }

    /// 是否运行后台任务（`salts_rotate`、`geoip_refresh`）：worker、all。
    pub fn runs_jobs(self) -> bool {
        matches!(self, Role::Worker | Role::All)
    }

    fn needs_ch_writer(self) -> bool {
        matches!(self, Role::Collector | Role::Worker | Role::All)
    }

    fn needs_ch_reader(self) -> bool {
        matches!(self, Role::Api | Role::All)
    }

    fn needs_ch_maintainer(self) -> bool {
        // 启用 jobs 的进程：worker、all
        matches!(self, Role::Worker | Role::All)
    }
}

/// 敏感字符串：Debug 输出被遮蔽，避免凭据进入日志。
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    pub fn new(value: impl Into<String>) -> Self {
        Secret(value.into())
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret([redacted])")
    }
}

/// 一个 ClickHouse 账号。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChAccount {
    pub user: String,
    pub password: Secret,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogFormat {
    Json,
    Pretty,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Lite,
    Reliable,
}

/// CIDR 网段（`RIVU_TRUSTED_PROXIES` 的元素）。P0 只解析和校验，P1 用于客户端 IP 判定。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cidr {
    addr: IpAddr,
    prefix: u8,
}

impl Cidr {
    pub fn addr(&self) -> IpAddr {
        self.addr
    }

    pub fn prefix(&self) -> u8 {
        self.prefix
    }

    /// `ip` 是否落在该网段内（IPv4 与 IPv6 不互相匹配）。
    pub fn contains(&self, ip: IpAddr) -> bool {
        fn masked(bits: u128, total: u32, prefix: u8) -> u128 {
            if prefix == 0 {
                0
            } else {
                bits >> (total - u32::from(prefix))
            }
        }
        match (self.addr, ip) {
            (IpAddr::V4(a), IpAddr::V4(b)) => {
                masked(u32::from(a).into(), 32, self.prefix)
                    == masked(u32::from(b).into(), 32, self.prefix)
            }
            (IpAddr::V6(a), IpAddr::V6(b)) => {
                masked(u128::from(a), 128, self.prefix) == masked(u128::from(b), 128, self.prefix)
            }
            _ => false,
        }
    }
}

impl FromStr for Cidr {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim();
        let (addr_part, prefix_part) = match s.split_once('/') {
            Some((a, p)) => (a, Some(p)),
            None => (s, None),
        };
        let addr: IpAddr = addr_part
            .parse()
            .map_err(|_| format!("`{s}` 不是有效的 IP 或 CIDR"))?;
        let max = if addr.is_ipv4() { 32 } else { 128 };
        let prefix = match prefix_part {
            None => max,
            Some(p) => p
                .parse::<u8>()
                .ok()
                .filter(|p| *p <= max)
                .ok_or_else(|| format!("`{s}` 的前缀长度必须在 0..={max}"))?,
        };
        Ok(Cidr { addr, prefix })
    }
}

/// 写缓冲（BatchWriter）的参数（`system-design.md` §4.7、§10 Q16）。
///
/// 默认值取 §4.7：1 s、2 万行、8 MiB，channel 容量 20 万条（变量名取 `platform-and-ops.md` §7）。
/// 压测（P2）后再调。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BufferConfig {
    /// `RIVU_BUFFER_FLUSH_INTERVAL_MS`：距上一次 flush 最多等多久。
    pub flush_interval: Duration,
    /// `RIVU_BUFFER_MAX_ROWS`：单批行数上限。
    pub max_rows: usize,
    /// `RIVU_BUFFER_MAX_BYTES`：单批字节数上限（估算值）。
    pub max_bytes: usize,
    /// `RIVU_BUFFER_CHANNEL_CAPACITY`：有界 channel 的容量，满了 `POST /api/event` 回 `503`。
    pub channel_capacity: usize,
}

impl BufferConfig {
    pub const DEFAULT_FLUSH_INTERVAL_MS: u64 = 1000;
    pub const DEFAULT_MAX_ROWS: usize = 20_000;
    pub const DEFAULT_MAX_BYTES: usize = 8_388_608;
    pub const DEFAULT_CHANNEL_CAPACITY: usize = 200_000;
}

impl Default for BufferConfig {
    fn default() -> Self {
        BufferConfig {
            flush_interval: Duration::from_millis(Self::DEFAULT_FLUSH_INTERVAL_MS),
            max_rows: Self::DEFAULT_MAX_ROWS,
            max_bytes: Self::DEFAULT_MAX_BYTES,
            channel_capacity: Self::DEFAULT_CHANNEL_CAPACITY,
        }
    }
}

/// ClickHouse 查询的执行时限，按调用方分三档（`system-design.md` §6.8、§10 Q14）。
///
/// 每档都必须不大于 `rivu_reader` profile 里 `max_execution_time` 的上限（该约束由 ClickHouse 强制）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QueryTimeouts {
    /// `RIVU_QUERY_TIMEOUT_SECS`：看板（登录会话）。
    pub dashboard_secs: u64,
    /// `RIVU_API_QUERY_TIMEOUT_SECS`：公开 API（API key）。
    pub api_secs: u64,
    /// `RIVU_PUBLIC_QUERY_TIMEOUT_SECS`：公开看板与共享链接。
    pub public_secs: u64,
}

impl QueryTimeouts {
    pub const DEFAULT_DASHBOARD_SECS: u64 = 10;
    pub const DEFAULT_API_SECS: u64 = 20;
    pub const DEFAULT_PUBLIC_SECS: u64 = 5;
}

impl Default for QueryTimeouts {
    fn default() -> Self {
        QueryTimeouts {
            dashboard_secs: Self::DEFAULT_DASHBOARD_SECS,
            api_secs: Self::DEFAULT_API_SECS,
            public_secs: Self::DEFAULT_PUBLIC_SECS,
        }
    }
}

/// 站点缓存的全量重载周期默认值（`RIVU_SITE_CACHE_REFRESH_SECS`）。
pub const DEFAULT_SITE_CACHE_REFRESH_SECS: u64 = 10;

/// `RIVU_DBIP_EDITION`：DB-IP Lite 的版本。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DbIpEdition {
    City,
    Country,
}

impl DbIpEdition {
    /// 下载文件名里的版本片段：`dbip-<city|country>-lite-<YYYY-MM>.mmdb.gz`。
    pub fn as_str(self) -> &'static str {
        match self {
            DbIpEdition::City => "city",
            DbIpEdition::Country => "country",
        }
    }
}

/// GeoIP 数据源（`system-design.md` §4.4「GeoIP 数据源、更新与署名」）。
///
/// 只启用优先级最高的一个：`RIVU_GEOIP_DB_URL` > `RIVU_MAXMIND_LICENSE_KEY` > DB-IP Lite。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GeoSource {
    /// 自定义 mmdb 直链（`.mmdb` 或 `.mmdb.gz`）。URL 可能带令牌，所以是 [`Secret`]。
    Custom { url: Secret },
    /// MaxMind GeoLite2：HTTP Basic，用户名是 Account ID，密码是 License Key。
    MaxMind {
        account_id: String,
        license_key: Secret,
        /// `GeoLite2-City` 或 `GeoLite2-Country`
        edition: String,
    },
    /// DB-IP Lite（默认）。
    DbIp { edition: DbIpEdition },
}

impl GeoSource {
    /// 来源类别，也是署名里的 `source` 和指标里的 `source` 标签。
    pub fn kind(&self) -> &'static str {
        match self {
            GeoSource::Custom { .. } => "custom",
            GeoSource::MaxMind { .. } => "maxmind-geolite2",
            GeoSource::DbIp { .. } => "dbip-lite",
        }
    }

    /// 来源标识，写进 meta.json；与当前配置不一致时视为没有本地副本。
    /// 不含任何凭据：自定义 URL 只保留 scheme、主机、路径。
    pub fn id(&self) -> String {
        match self {
            GeoSource::Custom { url } => format!("custom:{}", redact_url(url.expose())),
            GeoSource::MaxMind { edition, .. } => format!("maxmind-geolite2:{edition}"),
            GeoSource::DbIp { edition } => format!("dbip-lite:{}", edition.as_str()),
        }
    }

    /// 用户显式选择的来源（URL 或 MaxMind key）；默认的 DB-IP 不算。
    pub fn is_explicit(&self) -> bool {
        !matches!(self, GeoSource::DbIp { .. })
    }
}

/// GeoIP 配置。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeoConfig {
    /// `RIVU_DATA_DIR`
    pub data_dir: PathBuf,
    /// `RIVU_GEOIP_DB_PATH`：所有来源都落在这个文件；旁边是 `<文件名>.meta.json`。
    pub db_path: PathBuf,
    pub source: GeoSource,
    /// 配置了更高优先级来源而被忽略的变量名，用于启动时的 `warn`。
    pub ignored: Vec<&'static str>,
    /// `RIVU_GEOIP_REFRESH_INTERVAL`；`None` 表示 `0`（不定期刷新，只在没有本地副本时下载一次）。
    pub refresh_interval: Option<Duration>,
    /// `RIVU_GEOIP_ATTRIBUTION_TEXT`：自定义来源与手工放置文件的署名文本。
    pub attribution_text: Option<String>,
    /// `RIVU_GEOIP_ATTRIBUTION_URL`
    pub attribution_url: Option<String>,
}

impl GeoConfig {
    pub const DEFAULT_DATA_DIR: &'static str = "/var/lib/rivu";
    pub const DEFAULT_REFRESH_INTERVAL: Duration = Duration::from_secs(24 * 3600);
    pub const MIN_REFRESH_INTERVAL: Duration = Duration::from_secs(3600);
    pub const DEFAULT_MAXMIND_EDITION: &'static str = "GeoLite2-City";

    /// 默认配置：DB-IP Lite city，24 小时刷新，库放在 `data_dir/geoip/geoip.mmdb`。
    pub fn new(data_dir: impl Into<PathBuf>) -> Self {
        let data_dir = data_dir.into();
        GeoConfig {
            db_path: data_dir.join("geoip").join("geoip.mmdb"),
            data_dir,
            source: GeoSource::DbIp {
                edition: DbIpEdition::City,
            },
            ignored: Vec::new(),
            refresh_interval: Some(Self::DEFAULT_REFRESH_INTERVAL),
            attribution_text: None,
            attribution_url: None,
        }
    }

    /// 启动时应当记 `warn` 的配置问题（校验已通过，只是值得提醒）。
    pub fn warnings(&self) -> Vec<String> {
        let mut out = Vec::new();
        if !self.ignored.is_empty() {
            let winner = match &self.source {
                GeoSource::Custom { .. } => "RIVU_GEOIP_DB_URL",
                GeoSource::MaxMind { .. } => "RIVU_MAXMIND_LICENSE_KEY",
                GeoSource::DbIp { .. } => "DB-IP Lite",
            };
            out.push(format!(
                "同时配置了多个 GeoIP 来源：只使用 {winner}（优先级 RIVU_GEOIP_DB_URL > RIVU_MAXMIND_LICENSE_KEY > DB-IP Lite），忽略 {}",
                self.ignored.join("、")
            ));
        }
        if let GeoSource::Custom { url } = &self.source
            && url.expose().len() >= 7
            && url.expose()[..7].eq_ignore_ascii_case("http://")
        {
            out.push(format!(
                "RIVU_GEOIP_DB_URL（{}）使用明文 http：下载内容与其中的令牌可能被窃听或篡改，建议改用 https",
                redact_url(url.expose())
            ));
        }
        out
    }
}

/// 去掉 URL 里的用户信息、查询串和片段，只留 scheme、主机、路径，用于日志与来源标识。
/// 不依赖 `url` crate；无法识别 scheme 时返回占位符，绝不原样回显。
pub fn redact_url(raw: &str) -> String {
    let raw = raw.trim();
    let Some((scheme, rest)) = raw.split_once("://") else {
        return "<invalid-url>".into();
    };
    if scheme.is_empty()
        || !scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c))
    {
        return "<invalid-url>".into();
    }
    // 先砍掉查询串和片段，再拆 authority 与路径
    let rest = rest.split(['?', '#']).next().unwrap_or("");
    let (authority, path) = match rest.find('/') {
        Some(i) => rest.split_at(i),
        None => (rest, ""),
    };
    let host = authority.rsplit('@').next().unwrap_or("");
    if host.is_empty() {
        return "<invalid-url>".into();
    }
    format!("{}://{host}{path}", scheme.to_ascii_lowercase())
}

/// 解析 `RIVU_GEOIP_REFRESH_INTERVAL`：`0`，或「整数 + s/m/h/d」（如 `24h`、`90m`）。
fn parse_interval(raw: &str) -> Result<Duration, String> {
    let raw = raw.trim();
    if raw == "0" {
        return Ok(Duration::ZERO);
    }
    let split = raw
        .find(|c: char| !c.is_ascii_digit())
        .ok_or_else(|| "缺少单位（s、m、h、d）".to_string())?;
    let (num, unit) = raw.split_at(split);
    let n: u64 = num.parse().map_err(|_| "数字部分无效".to_string())?;
    let secs_per = match unit {
        "s" => 1,
        "m" => 60,
        "h" => 3600,
        "d" => 86_400,
        _ => return Err(format!("单位 `{unit}` 无效，可选 s、m、h、d")),
    };
    n.checked_mul(secs_per)
        .map(Duration::from_secs)
        .ok_or_else(|| "数值过大".to_string())
}

/// 校验通过后的配置。
#[derive(Debug, Clone)]
pub struct Config {
    pub role: Role,
    /// PostgreSQL 连接串。`migrate` 角色优先取 `RIVU_MIGRATE_DATABASE_URL`，其次 `RIVU_DATABASE_URL`。
    pub database_url: Secret,
    pub clickhouse_url: String,
    pub ch_database: String,
    pub ch_migrator: Option<ChAccount>,
    pub ch_writer: Option<ChAccount>,
    pub ch_reader: Option<ChAccount>,
    pub ch_maintainer: Option<ChAccount>,
    pub listen_addr: SocketAddr,
    pub internal_addr: SocketAddr,
    pub log_format: LogFormat,
    pub log_level: String,
    pub mode: Mode,
    pub trusted_proxies: Vec<Cidr>,
    pub buffer: BufferConfig,
    pub query_timeouts: QueryTimeouts,
    /// `RIVU_SITE_CACHE_REFRESH_SECS`：站点快照全量重载周期（P1 没有增量刷新和 LISTEN/NOTIFY）。
    pub site_cache_refresh: Duration,
    /// GeoIP 数据源、刷新与署名（`migrate` 不校验，取默认值）。
    pub geo: GeoConfig,
}

impl Config {
    /// 用于 `/readyz`、`schema_migrations` 读取的 ClickHouse 账号：reader、writer、maintainer 中第一个可用的。
    pub fn ch_probe_account(&self) -> Option<&ChAccount> {
        self.ch_reader
            .as_ref()
            .or(self.ch_writer.as_ref())
            .or(self.ch_maintainer.as_ref())
    }

    /// 从进程环境加载并校验。
    pub fn load(role: Role) -> Result<Config, ConfigError> {
        Config::from_env(role, &Env::from_process())
    }

    /// 从给定环境加载并校验（测试用）。
    pub fn from_env(role: Role, env: &Env) -> Result<Config, ConfigError> {
        let mut l = Loader {
            env,
            problems: Vec::new(),
        };

        let database_url = if role == Role::Migrate {
            // migrate 需要 DDL 权限：优先用 RIVU_MIGRATE_DATABASE_URL
            match l.optional("RIVU_MIGRATE_DATABASE_URL") {
                Some(v) => Some(v),
                None => l.optional("RIVU_DATABASE_URL"),
            }
            .or_else(|| {
                l.problems.push(
                    "RIVU_MIGRATE_DATABASE_URL 或 RIVU_DATABASE_URL 必须设置一个（migrate 需要 DDL 权限的 PostgreSQL 账号；也可用 *_FILE 形式）"
                        .into(),
                );
                None
            })
        } else {
            l.required("RIVU_DATABASE_URL")
        };
        if let Some(url) = &database_url {
            l.check_scheme("RIVU_DATABASE_URL", url, &["postgres://", "postgresql://"]);
        }

        let clickhouse_url = l.required("RIVU_CLICKHOUSE_URL");
        if let Some(url) = &clickhouse_url {
            l.check_scheme("RIVU_CLICKHOUSE_URL", url, &["http://", "https://"]);
        }

        let ch_database = l
            .optional("RIVU_CH_DATABASE")
            .unwrap_or_else(|| "rivu".into());
        if !is_identifier(&ch_database) {
            l.problems.push(format!(
                "RIVU_CH_DATABASE=`{ch_database}` 不是合法的库名（只允许字母、数字、下划线，且不以数字开头）"
            ));
        }

        let ch_migrator = l.account("RIVU_CH_MIGRATOR", role == Role::Migrate);
        let ch_writer = l.account("RIVU_CH_WRITER", role.needs_ch_writer());
        let ch_reader = l.account("RIVU_CH_READER", role.needs_ch_reader());
        let ch_maintainer = l.account("RIVU_CH_MAINTAINER", role.needs_ch_maintainer());

        let listen_addr = l.parse_or(
            "RIVU_LISTEN_ADDR",
            "127.0.0.1:8080".parse().unwrap(),
            "host:port，例如 127.0.0.1:8080",
        );
        let internal_addr = l.parse_or(
            "RIVU_INTERNAL_ADDR",
            "127.0.0.1:9090".parse().unwrap(),
            "host:port，例如 127.0.0.1:9090",
        );

        let log_format = match l.optional("RIVU_LOG_FORMAT").as_deref() {
            None | Some("json") => LogFormat::Json,
            Some("pretty") => LogFormat::Pretty,
            Some(other) => {
                l.problems.push(format!(
                    "RIVU_LOG_FORMAT=`{other}` 无效，可选 json | pretty"
                ));
                LogFormat::Json
            }
        };

        let log_level = l
            .optional("RIVU_LOG_LEVEL")
            .unwrap_or_else(|| "info".into());
        if let Err(e) = check_level(&log_level) {
            l.problems
                .push(format!("RIVU_LOG_LEVEL=`{log_level}` 无效: {e}"));
        }

        let mode = match l.optional("RIVU_MODE").as_deref() {
            None | Some("lite") => Mode::Lite,
            Some("reliable") => Mode::Reliable,
            Some(other) => {
                l.problems
                    .push(format!("RIVU_MODE=`{other}` 无效，可选 lite | reliable"));
                Mode::Lite
            }
        };
        if role == Role::All && mode == Mode::Reliable {
            l.problems
                .push("`rivu all` 只允许 RIVU_MODE=lite；reliable 模式请分别运行 collector / worker / api".into());
        }

        let mut trusted_proxies = Vec::new();
        if let Some(raw) = l.optional("RIVU_TRUSTED_PROXIES") {
            for item in raw.split(',').map(str::trim).filter(|s| !s.is_empty()) {
                match item.parse::<Cidr>() {
                    Ok(c) => trusted_proxies.push(c),
                    Err(e) => l.problems.push(format!("RIVU_TRUSTED_PROXIES: {e}")),
                }
            }
        }

        let flush_interval_ms = l.parse_in_range(
            "RIVU_BUFFER_FLUSH_INTERVAL_MS",
            BufferConfig::DEFAULT_FLUSH_INTERVAL_MS,
            10..=60_000,
        );
        let max_rows = l.parse_in_range(
            "RIVU_BUFFER_MAX_ROWS",
            BufferConfig::DEFAULT_MAX_ROWS,
            // 上限取 ClickHouse 的 max_insert_block_size（默认 1048449）以下：每个 INSERT 只产生一个 block（§4.7）。
            1..=1_000_000,
        );
        let max_bytes = l.parse_in_range(
            "RIVU_BUFFER_MAX_BYTES",
            BufferConfig::DEFAULT_MAX_BYTES,
            1024..=268_435_456,
        );
        let channel_capacity = l.parse_in_range(
            "RIVU_BUFFER_CHANNEL_CAPACITY",
            BufferConfig::DEFAULT_CHANNEL_CAPACITY,
            1..=10_000_000,
        );
        if channel_capacity < max_rows {
            l.problems.push(format!(
                "RIVU_BUFFER_CHANNEL_CAPACITY={channel_capacity} 不能小于 RIVU_BUFFER_MAX_ROWS={max_rows}：缓冲装不下一整批"
            ));
        }
        let query_timeouts = QueryTimeouts {
            dashboard_secs: l.parse_in_range(
                "RIVU_QUERY_TIMEOUT_SECS",
                QueryTimeouts::DEFAULT_DASHBOARD_SECS,
                1..=300,
            ),
            api_secs: l.parse_in_range(
                "RIVU_API_QUERY_TIMEOUT_SECS",
                QueryTimeouts::DEFAULT_API_SECS,
                1..=300,
            ),
            public_secs: l.parse_in_range(
                "RIVU_PUBLIC_QUERY_TIMEOUT_SECS",
                QueryTimeouts::DEFAULT_PUBLIC_SECS,
                1..=300,
            ),
        };
        let site_cache_refresh_secs = l.parse_in_range(
            "RIVU_SITE_CACHE_REFRESH_SECS",
            DEFAULT_SITE_CACHE_REFRESH_SECS,
            1..=3600,
        );

        // GeoIP 变量对 migrate 没有意义：解析但丢弃问题，避免无关变量挡住迁移。
        let problems_before_geo = l.problems.len();
        let geo = l.geo();
        if role == Role::Migrate {
            l.problems.truncate(problems_before_geo);
        }

        if !l.problems.is_empty() {
            return Err(ConfigError {
                problems: l.problems,
            });
        }

        // 到这里所有必填项都已存在，unwrap 不会失败。
        Ok(Config {
            role,
            database_url: Secret(database_url.expect("checked")),
            clickhouse_url: clickhouse_url.expect("checked"),
            ch_database,
            ch_migrator,
            ch_writer,
            ch_reader,
            ch_maintainer,
            listen_addr,
            internal_addr,
            log_format,
            log_level,
            mode,
            trusted_proxies,
            buffer: BufferConfig {
                flush_interval: Duration::from_millis(flush_interval_ms),
                max_rows,
                max_bytes,
                channel_capacity,
            },
            query_timeouts,
            site_cache_refresh: Duration::from_secs(site_cache_refresh_secs),
            geo,
        })
    }
}

/// 一次性报告的全部配置问题。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigError {
    pub problems: Vec<String>,
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "配置无效，共 {} 个问题：", self.problems.len())?;
        for (i, p) in self.problems.iter().enumerate() {
            writeln!(f, "  {}. {p}", i + 1)?;
        }
        Ok(())
    }
}

impl std::error::Error for ConfigError {}

/// 环境变量来源。生产用进程环境，测试用固定键值。
#[derive(Debug, Clone, Default)]
pub struct Env {
    vars: HashMap<String, String>,
}

impl Env {
    pub fn from_process() -> Self {
        Env {
            vars: std::env::vars().collect(),
        }
    }

    pub fn from_pairs<K: Into<String>, V: Into<String>>(
        pairs: impl IntoIterator<Item = (K, V)>,
    ) -> Self {
        Env {
            vars: pairs
                .into_iter()
                .map(|(k, v)| (k.into(), v.into()))
                .collect(),
        }
    }

    fn get(&self, name: &str) -> Option<&str> {
        self.vars
            .get(name)
            .map(String::as_str)
            .filter(|v| !v.is_empty())
    }
}

struct Loader<'a> {
    env: &'a Env,
    problems: Vec<String>,
}

impl Loader<'_> {
    /// 读取 `name`，或 `name_FILE` 指向的文件。两者同时给出时报冲突。
    fn optional(&mut self, name: &str) -> Option<String> {
        let file_name = format!("{name}_FILE");
        match (self.env.get(name), self.env.get(&file_name)) {
            (Some(_), Some(_)) => {
                self.problems
                    .push(format!("{name} 和 {file_name} 不能同时设置"));
                None
            }
            (Some(v), None) => Some(v.to_string()),
            (None, Some(path)) => match std::fs::read_to_string(path) {
                Ok(content) => {
                    let value = content.trim().to_string();
                    (!value.is_empty()).then_some(value)
                }
                Err(e) => {
                    self.problems
                        .push(format!("{file_name}=`{path}` 无法读取: {e}"));
                    None
                }
            },
            (None, None) => None,
        }
    }

    fn required(&mut self, name: &str) -> Option<String> {
        let before = self.problems.len();
        let v = self.optional(name);
        if v.is_none() && self.problems.len() == before {
            self.problems
                .push(format!("{name} 必填（也可以用 {name}_FILE 指向文件）"));
        }
        v
    }

    /// 读取 `<prefix>_USER` / `<prefix>_PASSWORD`。
    /// `required` 为 false 时可以都不设，但不能只设一个。
    fn account(&mut self, prefix: &str, required: bool) -> Option<ChAccount> {
        let user_name = format!("{prefix}_USER");
        let pass_name = format!("{prefix}_PASSWORD");
        let (user, password) = if required {
            (self.required(&user_name), self.required(&pass_name))
        } else {
            (self.optional(&user_name), self.optional(&pass_name))
        };
        match (user, password) {
            (Some(user), Some(password)) => Some(ChAccount {
                user,
                password: Secret(password),
            }),
            (None, None) => None,
            (Some(_), None) if !required => {
                self.problems
                    .push(format!("{user_name} 已设置，但 {pass_name} 缺失"));
                None
            }
            (None, Some(_)) if !required => {
                self.problems
                    .push(format!("{pass_name} 已设置，但 {user_name} 缺失"));
                None
            }
            _ => None,
        }
    }

    fn parse_or<T: FromStr>(&mut self, name: &str, default: T, expect: &str) -> T {
        match self.optional(name) {
            None => default,
            Some(raw) => raw.parse().unwrap_or_else(|_| {
                self.problems
                    .push(format!("{name}=`{raw}` 无效，期望 {expect}"));
                default
            }),
        }
    }

    /// 读取整数变量并检查取值范围（闭区间）。未设置取默认值；非法时记录问题并回退默认值。
    fn parse_in_range<T>(&mut self, name: &str, default: T, range: std::ops::RangeInclusive<T>) -> T
    where
        T: FromStr + PartialOrd + fmt::Display + Copy,
    {
        match self.optional(name) {
            None => default,
            Some(raw) => match raw.parse::<T>() {
                Ok(v) if range.contains(&v) => v,
                Ok(_) => {
                    self.problems.push(format!(
                        "{name}=`{raw}` 超出范围，期望 {} 到 {} 之间的整数",
                        range.start(),
                        range.end()
                    ));
                    default
                }
                Err(_) => {
                    self.problems.push(format!(
                        "{name}=`{raw}` 无效，期望 {} 到 {} 之间的整数",
                        range.start(),
                        range.end()
                    ));
                    default
                }
            },
        }
    }

    /// 读取全部 GeoIP 变量（`system-design.md` §4.4、`platform-and-ops.md` §7）。
    fn geo(&mut self) -> GeoConfig {
        let data_dir = self
            .optional("RIVU_DATA_DIR")
            .unwrap_or_else(|| GeoConfig::DEFAULT_DATA_DIR.into());
        let mut cfg = GeoConfig::new(&data_dir);
        if let Some(p) = self.optional("RIVU_GEOIP_DB_PATH") {
            cfg.db_path = PathBuf::from(p);
        }
        if cfg.db_path.file_name().is_none() {
            self.problems
                .push("RIVU_GEOIP_DB_PATH 必须指向一个文件（不能是目录）".into());
        }

        // 来源与优先级。三组变量都先读出来，这样格式问题一次性报告，被忽略的来源也要校验。
        let url = self.optional("RIVU_GEOIP_DB_URL");
        if let Some(u) = &url {
            self.check_http_url("RIVU_GEOIP_DB_URL", u);
        }
        let license_key = self.optional("RIVU_MAXMIND_LICENSE_KEY");
        let account_id = self.optional("RIVU_MAXMIND_ACCOUNT_ID");
        let mm_edition = self.optional("RIVU_MAXMIND_EDITION");
        if let Some(e) = &mm_edition
            && !matches!(e.as_str(), "GeoLite2-City" | "GeoLite2-Country")
        {
            self.problems.push(format!(
                "RIVU_MAXMIND_EDITION=`{e}` 无效，可选 GeoLite2-City | GeoLite2-Country"
            ));
        }
        if let Some(id) = &account_id
            && (id.is_empty() || id.chars().any(|c| c.is_whitespace() || c == ':'))
        {
            self.problems.push(
                "RIVU_MAXMIND_ACCOUNT_ID 不能包含空白或冒号（它是 HTTP Basic 的用户名）".into(),
            );
        }
        let dbip_edition = match self.optional("RIVU_DBIP_EDITION").as_deref() {
            None => DbIpEdition::City,
            Some(v) if v.eq_ignore_ascii_case("city") => DbIpEdition::City,
            Some(v) if v.eq_ignore_ascii_case("country") => DbIpEdition::Country,
            Some(other) => {
                self.problems.push(format!(
                    "RIVU_DBIP_EDITION=`{other}` 无效，可选 city | country"
                ));
                DbIpEdition::City
            }
        };

        let maxmind = license_key.map(|key| (key, account_id));
        let mm_source = |l: &mut Self, key: String, account: Option<String>| match account {
            Some(account_id) => Some(GeoSource::MaxMind {
                account_id,
                license_key: Secret(key),
                edition: mm_edition
                    .clone()
                    .unwrap_or_else(|| GeoConfig::DEFAULT_MAXMIND_EDITION.into()),
            }),
            None => {
                l.problems.push(
                    "设置了 RIVU_MAXMIND_LICENSE_KEY 就必须同时设置 RIVU_MAXMIND_ACCOUNT_ID：MaxMind 现行下载接口用 HTTP Basic（用户名 Account ID，密码 License Key）"
                        .into(),
                );
                None
            }
        };
        match (url, maxmind) {
            (Some(u), mm) => {
                cfg.source = GeoSource::Custom { url: Secret(u) };
                if mm.is_some() {
                    cfg.ignored.push("RIVU_MAXMIND_LICENSE_KEY");
                }
            }
            (None, Some((key, account))) => {
                if let Some(s) = mm_source(self, key, account) {
                    cfg.source = s;
                }
            }
            (None, None) => {
                cfg.source = GeoSource::DbIp {
                    edition: dbip_edition,
                }
            }
        }

        // 刷新周期
        if let Some(raw) = self.optional("RIVU_GEOIP_REFRESH_INTERVAL") {
            match parse_interval(&raw) {
                Ok(d) if d.is_zero() => cfg.refresh_interval = None,
                Ok(d) if d >= GeoConfig::MIN_REFRESH_INTERVAL => cfg.refresh_interval = Some(d),
                Ok(_) => self.problems.push(format!(
                    "RIVU_GEOIP_REFRESH_INTERVAL=`{raw}` 小于下限 1h（设为 0 表示不定期刷新）"
                )),
                Err(e) => self.problems.push(format!(
                    "RIVU_GEOIP_REFRESH_INTERVAL=`{raw}` 无效：{e}；期望 0 或 整数加 s/m/h/d，例如 24h"
                )),
            }
        }

        // 署名（自定义来源与手工放置文件使用）
        cfg.attribution_text = self.optional("RIVU_GEOIP_ATTRIBUTION_TEXT");
        cfg.attribution_url = self.optional("RIVU_GEOIP_ATTRIBUTION_URL");
        if let Some(u) = cfg.attribution_url.clone() {
            self.check_http_url("RIVU_GEOIP_ATTRIBUTION_URL", &u);
        }
        cfg
    }

    /// 要求 `http://` 或 `https://` 且带主机；不回显 value（URL 里可能有令牌）。
    fn check_http_url(&mut self, name: &str, value: &str) {
        self.check_scheme(name, value, &["http://", "https://"]);
        if (value.starts_with("http://") || value.starts_with("https://"))
            && (redact_url(value) == "<invalid-url>" || value.chars().any(char::is_whitespace))
        {
            self.problems
                .push(format!("{name} 不是有效的 URL（缺少主机或含空白字符）"));
        }
    }

    fn check_scheme(&mut self, name: &str, value: &str, schemes: &[&str]) {
        if !schemes.iter().any(|s| value.starts_with(s)) {
            // 不回显 value：PG 连接串里可能带密码
            self.problems
                .push(format!("{name} 必须以 {} 开头", schemes.join(" 或 ")));
        }
    }
}

fn is_identifier(s: &str) -> bool {
    let mut chars = s.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn base_pairs() -> Vec<(&'static str, &'static str)> {
        vec![
            ("RIVU_DATABASE_URL", "postgres://u:p@localhost/rivu"),
            ("RIVU_CLICKHOUSE_URL", "http://localhost:8123"),
            ("RIVU_CH_MIGRATOR_USER", "rivu_migrator"),
            ("RIVU_CH_MIGRATOR_PASSWORD", "m"),
            ("RIVU_CH_WRITER_USER", "rivu_writer"),
            ("RIVU_CH_WRITER_PASSWORD", "w"),
            ("RIVU_CH_READER_USER", "rivu_reader"),
            ("RIVU_CH_READER_PASSWORD", "r"),
            ("RIVU_CH_MAINTAINER_USER", "rivu_maintainer"),
            ("RIVU_CH_MAINTAINER_PASSWORD", "t"),
        ]
    }

    fn temp_file(name: &str, content: &str) -> PathBuf {
        let path =
            std::env::temp_dir().join(format!("rivu-cfg-test-{}-{name}", std::process::id()));
        std::fs::write(&path, content).unwrap();
        path
    }

    #[test]
    fn defaults_apply() {
        let cfg = Config::from_env(Role::All, &Env::from_pairs(base_pairs())).unwrap();
        assert_eq!(cfg.ch_database, "rivu");
        assert_eq!(cfg.listen_addr, "127.0.0.1:8080".parse().unwrap());
        assert_eq!(cfg.internal_addr, "127.0.0.1:9090".parse().unwrap());
        assert_eq!(cfg.log_format, LogFormat::Json);
        assert_eq!(cfg.log_level, "info");
        assert_eq!(cfg.mode, Mode::Lite);
        assert!(cfg.trusted_proxies.is_empty());
        // 写缓冲默认值取 system-design §4.7（§10 Q16）
        assert_eq!(cfg.buffer.flush_interval, Duration::from_millis(1000));
        assert_eq!(cfg.buffer.max_rows, 20_000);
        assert_eq!(cfg.buffer.max_bytes, 8_388_608);
        assert_eq!(cfg.buffer.channel_capacity, 200_000);
        assert_eq!(cfg.site_cache_refresh, Duration::from_secs(10));
        // 查询超时按调用方分三档（§6.8、§10 Q14）
        assert_eq!(
            cfg.query_timeouts,
            QueryTimeouts {
                dashboard_secs: 10,
                api_secs: 20,
                public_secs: 5
            }
        );
    }

    fn with_env(extra: &[(&str, &str)]) -> Result<Config, ConfigError> {
        let mut env = Env::from_pairs(base_pairs());
        for (k, v) in extra {
            env.vars.insert((*k).into(), (*v).into());
        }
        Config::from_env(Role::All, &env)
    }

    #[test]
    fn buffer_variables_are_read() {
        let cfg = with_env(&[
            ("RIVU_BUFFER_FLUSH_INTERVAL_MS", "250"),
            ("RIVU_BUFFER_MAX_ROWS", "5000"),
            ("RIVU_BUFFER_MAX_BYTES", "1048576"),
            ("RIVU_BUFFER_CHANNEL_CAPACITY", "20000"),
            ("RIVU_SITE_CACHE_REFRESH_SECS", "3"),
        ])
        .unwrap();
        assert_eq!(cfg.buffer.flush_interval, Duration::from_millis(250));
        assert_eq!(cfg.buffer.max_rows, 5000);
        assert_eq!(cfg.buffer.max_bytes, 1_048_576);
        assert_eq!(cfg.buffer.channel_capacity, 20_000);
        assert_eq!(cfg.site_cache_refresh, Duration::from_secs(3));
    }

    #[test]
    fn buffer_variables_are_validated() {
        for (name, bad) in [
            ("RIVU_BUFFER_FLUSH_INTERVAL_MS", "0"),
            ("RIVU_BUFFER_FLUSH_INTERVAL_MS", "9"),
            ("RIVU_BUFFER_FLUSH_INTERVAL_MS", "60001"),
            ("RIVU_BUFFER_FLUSH_INTERVAL_MS", "fast"),
            ("RIVU_BUFFER_MAX_ROWS", "0"),
            ("RIVU_BUFFER_MAX_ROWS", "1000001"),
            ("RIVU_BUFFER_MAX_BYTES", "100"),
            ("RIVU_BUFFER_MAX_BYTES", "-1"),
            ("RIVU_BUFFER_CHANNEL_CAPACITY", "0"),
            ("RIVU_SITE_CACHE_REFRESH_SECS", "0"),
            ("RIVU_SITE_CACHE_REFRESH_SECS", "3601"),
        ] {
            let err = with_env(&[(name, bad)]).unwrap_err();
            assert_eq!(err.problems.len(), 1, "{name}={bad}: {err}");
            assert!(err.problems[0].contains(name), "{name}={bad}: {err}");
        }
    }

    #[test]
    fn query_timeout_variables_are_read_and_validated() {
        let cfg = with_env(&[
            ("RIVU_QUERY_TIMEOUT_SECS", "12"),
            ("RIVU_API_QUERY_TIMEOUT_SECS", "1"),
            ("RIVU_PUBLIC_QUERY_TIMEOUT_SECS", "300"),
        ])
        .unwrap();
        assert_eq!(
            cfg.query_timeouts,
            QueryTimeouts {
                dashboard_secs: 12,
                api_secs: 1,
                public_secs: 300
            }
        );
        for name in [
            "RIVU_QUERY_TIMEOUT_SECS",
            "RIVU_API_QUERY_TIMEOUT_SECS",
            "RIVU_PUBLIC_QUERY_TIMEOUT_SECS",
        ] {
            for bad in ["0", "301", "-1", "slow"] {
                let err = with_env(&[(name, bad)]).unwrap_err();
                assert_eq!(err.problems.len(), 1, "{name}={bad}: {err}");
                assert!(err.problems[0].contains(name), "{name}={bad}: {err}");
            }
        }
    }

    #[test]
    fn channel_must_hold_a_whole_batch() {
        let err = with_env(&[
            ("RIVU_BUFFER_MAX_ROWS", "5000"),
            ("RIVU_BUFFER_CHANNEL_CAPACITY", "4999"),
        ])
        .unwrap_err();
        assert_eq!(err.problems.len(), 1, "{err}");
        assert!(err.problems[0].contains("RIVU_BUFFER_CHANNEL_CAPACITY"));
        // 相等可以
        assert!(
            with_env(&[
                ("RIVU_BUFFER_MAX_ROWS", "5000"),
                ("RIVU_BUFFER_CHANNEL_CAPACITY", "5000"),
            ])
            .is_ok()
        );
        // 只调大 max_rows、没有同时调大容量：默认容量 20 万足够
        assert!(with_env(&[("RIVU_BUFFER_MAX_ROWS", "200000")]).is_ok());
    }

    #[test]
    fn only_worker_and_all_run_jobs() {
        assert!(Role::All.runs_jobs());
        assert!(Role::Worker.runs_jobs());
        assert!(!Role::Collector.runs_jobs());
        assert!(!Role::Api.runs_jobs());
        assert!(!Role::Migrate.runs_jobs());
    }

    #[test]
    fn file_variant_is_read_and_trimmed() {
        let path = temp_file("pw", "  s3cret\n\n");
        let mut pairs = base_pairs();
        pairs.retain(|(k, _)| *k != "RIVU_CH_WRITER_PASSWORD");
        let mut env = Env::from_pairs(pairs);
        env.vars.insert(
            "RIVU_CH_WRITER_PASSWORD_FILE".into(),
            path.to_string_lossy().into_owned(),
        );
        let cfg = Config::from_env(Role::Collector, &env).unwrap();
        assert_eq!(cfg.ch_writer.unwrap().password.expose(), "s3cret");
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn value_and_file_together_conflict() {
        let path = temp_file("conflict", "x");
        let mut env = Env::from_pairs(base_pairs());
        env.vars.insert(
            "RIVU_CH_WRITER_PASSWORD_FILE".into(),
            path.to_string_lossy().into_owned(),
        );
        let err = Config::from_env(Role::Collector, &env).unwrap_err();
        assert!(
            err.problems
                .iter()
                .any(|p| p.contains("RIVU_CH_WRITER_PASSWORD 和 RIVU_CH_WRITER_PASSWORD_FILE")),
            "{err}"
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn unreadable_file_is_reported() {
        let mut pairs = base_pairs();
        pairs.retain(|(k, _)| *k != "RIVU_DATABASE_URL");
        let mut env = Env::from_pairs(pairs);
        env.vars.insert(
            "RIVU_DATABASE_URL_FILE".into(),
            "/definitely/not/here".into(),
        );
        let err = Config::from_env(Role::Collector, &env).unwrap_err();
        assert_eq!(err.problems.len(), 1, "{err}");
        assert!(err.problems[0].contains("RIVU_DATABASE_URL_FILE"));
    }

    #[test]
    fn all_problems_are_listed_at_once() {
        let env = Env::from_pairs([
            ("RIVU_LISTEN_ADDR", "nonsense"),
            ("RIVU_LOG_FORMAT", "xml"),
            ("RIVU_MODE", "turbo"),
            ("RIVU_TRUSTED_PROXIES", "10.0.0.0/8, 10.0.0.0/99, nope"),
        ]);
        let err = Config::from_env(Role::All, &env).unwrap_err();
        let text = err.to_string();
        for needle in [
            "RIVU_DATABASE_URL 必填",
            "RIVU_CLICKHOUSE_URL 必填",
            "RIVU_CH_WRITER_USER 必填",
            "RIVU_CH_READER_PASSWORD 必填",
            "RIVU_CH_MAINTAINER_USER 必填",
            "RIVU_LISTEN_ADDR",
            "RIVU_LOG_FORMAT",
            "RIVU_MODE",
            "10.0.0.0/99",
            "nope",
        ] {
            assert!(text.contains(needle), "缺少 `{needle}`:\n{text}");
        }
        assert!(err.problems.len() >= 10);
    }

    #[test]
    fn requirements_depend_on_role() {
        // migrate 只需要 migrator 账号和两个 URL
        let env = Env::from_pairs([
            ("RIVU_DATABASE_URL", "postgres://u:p@localhost/rivu"),
            ("RIVU_CLICKHOUSE_URL", "http://localhost:8123"),
            ("RIVU_CH_MIGRATOR_USER", "m"),
            ("RIVU_CH_MIGRATOR_PASSWORD", "m"),
        ]);
        assert!(Config::from_env(Role::Migrate, &env).is_ok());
        // 同样的环境对 api 角色不够：缺 reader
        let err = Config::from_env(Role::Api, &env).unwrap_err();
        assert!(err.to_string().contains("RIVU_CH_READER_USER"));
        // collector 需要 writer，不需要 reader / maintainer
        let err = Config::from_env(Role::Collector, &env).unwrap_err();
        let text = err.to_string();
        assert!(text.contains("RIVU_CH_WRITER_USER"));
        assert!(!text.contains("RIVU_CH_READER_USER"));
        assert!(!text.contains("RIVU_CH_MAINTAINER_USER"));
    }

    #[test]
    fn migrate_prefers_migrate_database_url() {
        let mut env = Env::from_pairs(base_pairs());
        env.vars.insert(
            "RIVU_MIGRATE_DATABASE_URL".into(),
            "postgres://mig:p@localhost/rivu".into(),
        );
        let cfg = Config::from_env(Role::Migrate, &env).unwrap();
        assert!(cfg.database_url.expose().contains("mig:"));
        let cfg = Config::from_env(Role::Collector, &env).unwrap();
        assert!(cfg.database_url.expose().contains("u:p@"));
    }

    #[test]
    fn half_configured_optional_account_is_an_error() {
        // migrate 角色下 writer 是可选的，但只给用户名不行
        let env = Env::from_pairs([
            ("RIVU_DATABASE_URL", "postgres://u:p@localhost/rivu"),
            ("RIVU_CLICKHOUSE_URL", "http://localhost:8123"),
            ("RIVU_CH_MIGRATOR_USER", "m"),
            ("RIVU_CH_MIGRATOR_PASSWORD", "m"),
            ("RIVU_CH_WRITER_USER", "w"),
        ]);
        let err = Config::from_env(Role::Migrate, &env).unwrap_err();
        assert!(err.to_string().contains("RIVU_CH_WRITER_PASSWORD 缺失"));
    }

    #[test]
    fn all_with_reliable_is_rejected() {
        let mut env = Env::from_pairs(base_pairs());
        env.vars.insert("RIVU_MODE".into(), "reliable".into());
        assert!(Config::from_env(Role::All, &env).is_err());
        assert!(Config::from_env(Role::Collector, &env).is_ok());
    }

    #[test]
    fn empty_value_counts_as_unset() {
        let mut env = Env::from_pairs(base_pairs());
        env.vars.insert("RIVU_LISTEN_ADDR".into(), String::new());
        env.vars.insert("RIVU_LISTEN_ADDR_FILE".into(), "/x".into());
        // 空值不算「同时设置」，于是读取 _FILE（不存在 -> 读取错误，而不是冲突）
        let err = Config::from_env(Role::All, &env).unwrap_err();
        assert!(err.problems.iter().all(|p| !p.contains("不能同时设置")));
    }

    #[test]
    fn secrets_are_redacted_in_debug() {
        let cfg = Config::from_env(Role::All, &Env::from_pairs(base_pairs())).unwrap();
        let dbg = format!("{cfg:?}");
        assert!(!dbg.contains("u:p@localhost"), "{dbg}");
        assert!(dbg.contains("redacted"));
    }

    #[test]
    fn trusted_proxies_parse_and_match() {
        let mut env = Env::from_pairs(base_pairs());
        env.vars.insert(
            "RIVU_TRUSTED_PROXIES".into(),
            "172.28.0.0/24, 127.0.0.1, fd00::/8".into(),
        );
        let cfg = Config::from_env(Role::All, &env).unwrap();
        assert_eq!(cfg.trusted_proxies.len(), 3);
        let net = cfg.trusted_proxies[0];
        assert!(net.contains("172.28.0.77".parse().unwrap()));
        assert!(!net.contains("172.28.1.1".parse().unwrap()));
        assert!(cfg.trusted_proxies[1].contains("127.0.0.1".parse().unwrap()));
        assert!(!cfg.trusted_proxies[1].contains("127.0.0.2".parse().unwrap()));
        assert!(cfg.trusted_proxies[2].contains("fd12::1".parse().unwrap()));
        assert!(!net.contains("fd12::1".parse().unwrap()));
        let all: Cidr = "0.0.0.0/0".parse().unwrap();
        assert!(all.contains("8.8.8.8".parse().unwrap()));
    }

    #[test]
    fn rejects_bad_database_name_and_schemes() {
        let mut env = Env::from_pairs(base_pairs());
        env.vars.insert("RIVU_CH_DATABASE".into(), "ri;vu".into());
        env.vars
            .insert("RIVU_CLICKHOUSE_URL".into(), "ftp://x".into());
        env.vars
            .insert("RIVU_DATABASE_URL".into(), "mysql://x".into());
        let err = Config::from_env(Role::All, &env).unwrap_err();
        assert_eq!(err.problems.len(), 3, "{err}");
        assert!(!err.to_string().contains("mysql://x"), "不能回显连接串");
    }

    // --- GeoIP（system-design §4.4、platform-and-ops §7） ---

    #[test]
    fn geo_defaults_to_dbip_city_with_daily_refresh() {
        let cfg = with_env(&[]).unwrap().geo;
        assert_eq!(cfg.data_dir, PathBuf::from("/var/lib/rivu"));
        assert_eq!(
            cfg.db_path,
            PathBuf::from("/var/lib/rivu")
                .join("geoip")
                .join("geoip.mmdb")
        );
        assert_eq!(
            cfg.source,
            GeoSource::DbIp {
                edition: DbIpEdition::City
            }
        );
        assert_eq!(cfg.source.kind(), "dbip-lite");
        assert_eq!(cfg.source.id(), "dbip-lite:city");
        assert_eq!(cfg.refresh_interval, Some(Duration::from_secs(86_400)));
        assert!(cfg.ignored.is_empty());
        assert!(cfg.warnings().is_empty());
        assert_eq!(cfg.attribution_text, None);
    }

    #[test]
    fn geo_data_dir_derives_default_db_path_and_path_overrides() {
        let cfg = with_env(&[("RIVU_DATA_DIR", "/srv/rivu")]).unwrap().geo;
        assert_eq!(
            cfg.db_path,
            PathBuf::from("/srv/rivu").join("geoip").join("geoip.mmdb")
        );
        let cfg = with_env(&[
            ("RIVU_DATA_DIR", "/srv/rivu"),
            ("RIVU_GEOIP_DB_PATH", "/mnt/geo/my.mmdb"),
        ])
        .unwrap()
        .geo;
        assert_eq!(cfg.db_path, PathBuf::from("/mnt/geo/my.mmdb"));
    }

    #[test]
    fn geo_dbip_edition() {
        let cfg = with_env(&[("RIVU_DBIP_EDITION", "country")]).unwrap().geo;
        assert_eq!(cfg.source.id(), "dbip-lite:country");
        let err = with_env(&[("RIVU_DBIP_EDITION", "isp")]).unwrap_err();
        assert_eq!(err.problems.len(), 1, "{err}");
        assert!(err.problems[0].contains("RIVU_DBIP_EDITION"));
    }

    #[test]
    fn geo_source_priority_url_over_maxmind_over_dbip() {
        // 只有 MaxMind
        let cfg = with_env(&[
            ("RIVU_MAXMIND_LICENSE_KEY", "k3y"),
            ("RIVU_MAXMIND_ACCOUNT_ID", "123456"),
        ])
        .unwrap()
        .geo;
        assert_eq!(
            cfg.source,
            GeoSource::MaxMind {
                account_id: "123456".into(),
                license_key: Secret::new("k3y"),
                edition: "GeoLite2-City".into(),
            }
        );
        assert_eq!(cfg.source.id(), "maxmind-geolite2:GeoLite2-City");
        assert!(cfg.ignored.is_empty());
        // MaxMind 与 DB-IP 版本变量并存：MaxMind 胜出，DB-IP 的版本变量不算「来源」，不报警告
        let cfg = with_env(&[
            ("RIVU_MAXMIND_LICENSE_KEY", "k3y"),
            ("RIVU_MAXMIND_ACCOUNT_ID", "1"),
            ("RIVU_MAXMIND_EDITION", "GeoLite2-Country"),
            ("RIVU_DBIP_EDITION", "country"),
        ])
        .unwrap()
        .geo;
        assert_eq!(cfg.source.id(), "maxmind-geolite2:GeoLite2-Country");
        // URL 与 MaxMind 同时配置：URL 胜出，忽略 MaxMind，并有 warn
        let cfg = with_env(&[
            (
                "RIVU_GEOIP_DB_URL",
                "https://example.test/geo.mmdb.gz?token=SECRET",
            ),
            ("RIVU_MAXMIND_LICENSE_KEY", "k3y"),
            ("RIVU_MAXMIND_ACCOUNT_ID", "1"),
        ])
        .unwrap()
        .geo;
        assert_eq!(cfg.source.kind(), "custom");
        assert_eq!(cfg.ignored, vec!["RIVU_MAXMIND_LICENSE_KEY"]);
        let warnings = cfg.warnings();
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("RIVU_GEOIP_DB_URL"));
        assert!(warnings[0].contains("RIVU_MAXMIND_LICENSE_KEY"));
    }

    #[test]
    fn geo_maxmind_key_requires_account_id_and_valid_edition() {
        let err = with_env(&[("RIVU_MAXMIND_LICENSE_KEY", "k3y")]).unwrap_err();
        assert_eq!(err.problems.len(), 1, "{err}");
        assert!(err.problems[0].contains("RIVU_MAXMIND_ACCOUNT_ID"));
        assert!(!err.to_string().contains("k3y"), "不能回显 license key");
        let err = with_env(&[
            ("RIVU_MAXMIND_LICENSE_KEY", "k3y"),
            ("RIVU_MAXMIND_ACCOUNT_ID", "1"),
            ("RIVU_MAXMIND_EDITION", "GeoLite2-ASN"),
        ])
        .unwrap_err();
        assert!(
            err.problems
                .iter()
                .any(|p| p.contains("RIVU_MAXMIND_EDITION"))
        );
        // 带冒号的 Account ID 会破坏 Basic 认证
        let err = with_env(&[
            ("RIVU_MAXMIND_LICENSE_KEY", "k3y"),
            ("RIVU_MAXMIND_ACCOUNT_ID", "1:2"),
        ])
        .unwrap_err();
        assert!(
            err.problems
                .iter()
                .any(|p| p.contains("RIVU_MAXMIND_ACCOUNT_ID"))
        );
    }

    #[test]
    fn geo_url_is_validated_redacted_and_supports_file() {
        for bad in ["ftp://x/y.mmdb", "geo.mmdb", "https://", "https://a b/c"] {
            let err = with_env(&[("RIVU_GEOIP_DB_URL", bad)]).unwrap_err();
            assert_eq!(err.problems.len(), 1, "{bad}: {err}");
            assert!(err.problems[0].contains("RIVU_GEOIP_DB_URL"));
        }
        // 令牌来自文件，Debug 与来源标识里都不出现
        let path = temp_file(
            "geourl",
            "  https://u:pw@git.example/dejavu/GeoLite.mmdb?t=SECRET\n",
        );
        let mut env = Env::from_pairs(base_pairs());
        env.vars.insert(
            "RIVU_GEOIP_DB_URL_FILE".into(),
            path.to_string_lossy().into_owned(),
        );
        let cfg = Config::from_env(Role::All, &env).unwrap();
        let dbg = format!("{:?}", cfg.geo);
        assert!(!dbg.contains("SECRET") && !dbg.contains("pw@"), "{dbg}");
        assert_eq!(
            cfg.geo.source.id(),
            "custom:https://git.example/dejavu/GeoLite.mmdb"
        );
        // 同时设置值和 _FILE 是冲突
        env.vars
            .insert("RIVU_GEOIP_DB_URL".into(), "https://x.test/a.mmdb".into());
        let err = Config::from_env(Role::All, &env).unwrap_err();
        assert!(
            err.to_string()
                .contains("RIVU_GEOIP_DB_URL 和 RIVU_GEOIP_DB_URL_FILE")
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn geo_plain_http_url_warns() {
        let cfg = with_env(&[("RIVU_GEOIP_DB_URL", "http://10.0.0.5/geo.mmdb?t=SECRET")])
            .unwrap()
            .geo;
        let w = cfg.warnings();
        assert_eq!(w.len(), 1, "{w:?}");
        assert!(w[0].contains("http://10.0.0.5/geo.mmdb"));
        assert!(!w[0].contains("SECRET"), "警告里不能带查询串");
        assert!(
            with_env(&[("RIVU_GEOIP_DB_URL", "https://10.0.0.5/geo.mmdb")])
                .unwrap()
                .geo
                .warnings()
                .is_empty()
        );
    }

    #[test]
    fn geo_refresh_interval_parse_and_limits() {
        let ok = |v: &str| {
            with_env(&[("RIVU_GEOIP_REFRESH_INTERVAL", v)])
                .unwrap()
                .geo
                .refresh_interval
        };
        assert_eq!(ok("24h"), Some(Duration::from_secs(86_400)));
        assert_eq!(ok("1h"), Some(Duration::from_secs(3600)));
        assert_eq!(ok("60m"), Some(Duration::from_secs(3600)));
        assert_eq!(ok("3600s"), Some(Duration::from_secs(3600)));
        assert_eq!(ok("7d"), Some(Duration::from_secs(7 * 86_400)));
        assert_eq!(ok("0"), None, "0 = 不定期刷新");
        for bad in [
            "59m",
            "3599s",
            "1",
            "h",
            "1.5h",
            "-1h",
            "1w",
            "forever",
            "99999999999999999999h",
        ] {
            let err = with_env(&[("RIVU_GEOIP_REFRESH_INTERVAL", bad)]).unwrap_err();
            assert_eq!(err.problems.len(), 1, "{bad}: {err}");
            assert!(
                err.problems[0].contains("RIVU_GEOIP_REFRESH_INTERVAL"),
                "{bad}"
            );
        }
    }

    #[test]
    fn geo_attribution_variables_are_read_and_url_checked() {
        let cfg = with_env(&[
            ("RIVU_GEOIP_ATTRIBUTION_TEXT", "Geo data by Example"),
            ("RIVU_GEOIP_ATTRIBUTION_URL", "https://example.test/geo"),
        ])
        .unwrap()
        .geo;
        assert_eq!(cfg.attribution_text.as_deref(), Some("Geo data by Example"));
        assert_eq!(
            cfg.attribution_url.as_deref(),
            Some("https://example.test/geo")
        );
        let err = with_env(&[("RIVU_GEOIP_ATTRIBUTION_URL", "javascript:alert(1)")]).unwrap_err();
        assert!(err.problems[0].contains("RIVU_GEOIP_ATTRIBUTION_URL"));
    }

    #[test]
    fn geo_variables_do_not_block_migrate() {
        let env = Env::from_pairs([
            ("RIVU_DATABASE_URL", "postgres://u:p@localhost/rivu"),
            ("RIVU_CLICKHOUSE_URL", "http://localhost:8123"),
            ("RIVU_CH_MIGRATOR_USER", "m"),
            ("RIVU_CH_MIGRATOR_PASSWORD", "m"),
            ("RIVU_GEOIP_REFRESH_INTERVAL", "1m"),
            ("RIVU_MAXMIND_LICENSE_KEY", "k"),
        ]);
        assert!(Config::from_env(Role::Migrate, &env).is_ok());
        assert!(Config::from_env(Role::Collector, &env).is_err());
    }

    #[test]
    fn redact_url_drops_credentials_query_and_fragment() {
        assert_eq!(
            redact_url("https://user:pw@Example.COM:8443/a/b.mmdb.gz?token=x#frag"),
            "https://Example.COM:8443/a/b.mmdb.gz"
        );
        assert_eq!(redact_url("http://h"), "http://h");
        assert_eq!(redact_url("HTTPS://h/p"), "https://h/p");
        assert_eq!(redact_url("https://tok@h?x=1"), "https://h");
        assert_eq!(redact_url("not a url"), "<invalid-url>");
        assert_eq!(redact_url("https://"), "<invalid-url>");
        assert_eq!(redact_url("https://?token=SECRET"), "<invalid-url>");
    }
}
