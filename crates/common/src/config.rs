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

    /// 是否运行后台任务（目前只有 `salts_rotate`）：worker、all。
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

/// 写缓冲（BatchWriter）的参数（`system-design.md` §4.7、§10.1 Q16）。
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

/// 站点缓存的全量重载周期默认值（`RIVU_SITE_CACHE_REFRESH_SECS`）。
pub const DEFAULT_SITE_CACHE_REFRESH_SECS: u64 = 10;

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
    /// `RIVU_SITE_CACHE_REFRESH_SECS`：站点快照全量重载周期（P1 没有增量刷新和 LISTEN/NOTIFY）。
    pub site_cache_refresh: Duration,
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
        let site_cache_refresh_secs = l.parse_in_range(
            "RIVU_SITE_CACHE_REFRESH_SECS",
            DEFAULT_SITE_CACHE_REFRESH_SECS,
            1..=3600,
        );

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
            site_cache_refresh: Duration::from_secs(site_cache_refresh_secs),
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
        // 写缓冲默认值取 system-design §4.7（§10.1 Q16）
        assert_eq!(cfg.buffer.flush_interval, Duration::from_millis(1000));
        assert_eq!(cfg.buffer.max_rows, 20_000);
        assert_eq!(cfg.buffer.max_bytes, 8_388_608);
        assert_eq!(cfg.buffer.channel_capacity, 200_000);
        assert_eq!(cfg.site_cache_refresh, Duration::from_secs(10));
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
}
