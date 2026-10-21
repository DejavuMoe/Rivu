//! 命令行：`rivu collector | worker | api | migrate | all`。

use std::process::ExitCode;

use clap::{Args, Parser, Subcommand};
use rivu_common::build_info;
use rivu_common::config::{Config, Role};
use rivu_common::telemetry;

use crate::{migrate, run};

#[derive(Debug, Parser)]
#[command(
    name = "rivu",
    about = "Rivu：开源、自托管的网站分析系统",
    version = build_info::long_version()
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// 采集角色：公共端口提供 `POST /api/event`
    Collector,
    /// 后台角色：会话分配、批量写入与后台任务（P0 只启动内部端口）
    Worker,
    /// API 角色：公共端口提供 `/api/*`
    Api,
    /// 执行 PostgreSQL + ClickHouse 迁移后退出
    Migrate(MigrateArgs),
    /// 单进程运行 collector + api + 后台任务（Lite 形态）
    All,
}

#[derive(Debug, Args)]
pub struct MigrateArgs {
    #[command(subcommand)]
    pub action: Option<MigrateAction>,
    /// 允许应用版本号小于已应用最高版本的迁移（分支合并造成的乱序）
    #[arg(long)]
    pub allow_out_of_order: bool,
}

#[derive(Debug, Subcommand)]
pub enum MigrateAction {
    /// 列出每个迁移版本的状态
    Status,
}

impl Command {
    fn role(&self) -> Role {
        match self {
            Command::Collector => Role::Collector,
            Command::Worker => Role::Worker,
            Command::Api => Role::Api,
            Command::Migrate(_) => Role::Migrate,
            Command::All => Role::All,
        }
    }
}

pub async fn main() -> ExitCode {
    let cli = Cli::parse();
    let role = cli.command.role();

    // 1. 加载并校验配置：一次报告全部问题
    let config = match Config::load(role) {
        Ok(c) => c,
        Err(e) => {
            eprint!("{e}");
            return ExitCode::from(2);
        }
    };

    // 2. 初始化 tracing
    if let Err(e) = telemetry::init_tracing(config.log_format, &config.log_level) {
        eprintln!("{e}");
        return ExitCode::from(2);
    }

    // 3. 运行（优雅关停在 run 内部处理）
    let result = match cli.command {
        Command::Migrate(args) => migrate::run_command(&config, args).await,
        _ => run::run(config).await,
    };
    match result {
        Ok(code) => code,
        Err(e) => {
            eprintln!("错误: {e:#}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn parses_all_subcommands() {
        for (arg, role) in [
            ("collector", Role::Collector),
            ("worker", Role::Worker),
            ("api", Role::Api),
            ("all", Role::All),
            ("migrate", Role::Migrate),
        ] {
            let cli = Cli::try_parse_from(["rivu", arg]).unwrap();
            assert_eq!(cli.command.role(), role);
        }
        let cli = Cli::try_parse_from(["rivu", "migrate", "status"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::Migrate(MigrateArgs {
                action: Some(MigrateAction::Status),
                ..
            })
        ));
    }

    #[test]
    fn version_flag_reports_version_and_commit() {
        let err = Cli::try_parse_from(["rivu", "--version"]).unwrap_err();
        let text = err.to_string();
        assert!(text.contains(build_info::VERSION));
        assert!(text.contains("commit"));
    }
}
