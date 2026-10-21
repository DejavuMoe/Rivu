//! tracing 初始化。

use crate::config::LogFormat;
use tracing_subscriber::EnvFilter;

/// 校验 `RIVU_LOG_LEVEL`（EnvFilter 指令）是否可解析，无副作用。
pub fn check_level(level: &str) -> Result<(), String> {
    EnvFilter::try_new(level)
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// 初始化全局 tracing subscriber。日志写到 stderr，避免与将来可能的 stdout 输出混在一起。
///
/// `level` 是 `RIVU_LOG_LEVEL` 的值，按 `EnvFilter` 指令解析（例如 `info` 或 `rivu=debug,warn`）。
pub fn init_tracing(format: LogFormat, level: &str) -> Result<(), String> {
    let filter = EnvFilter::try_new(level).map_err(|e| format!("RIVU_LOG_LEVEL 无效: {e}"))?;
    let builder = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr);
    let result = match format {
        LogFormat::Json => builder.json().with_current_span(false).try_init(),
        LogFormat::Pretty => builder.pretty().try_init(),
    };
    result.map_err(|e| format!("tracing 初始化失败: {e}"))
}
