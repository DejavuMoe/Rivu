//! 命令行：`rivu collector | worker | api | migrate | all | site add | stats`。

use std::process::ExitCode;

use clap::{Args, Parser, Subcommand};
use jiff::civil::Date;
use rivu_common::build_info;
use rivu_common::config::{Config, Role};
use rivu_common::telemetry;

use crate::{admin, migrate, run};

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
    /// 后台角色：后台任务（P1 只有 salts_rotate；会话分配与批量写入在 Reliable 模式下移到这里，尚未实现）
    Worker,
    /// API 角色：公共端口提供 `/api/*`（P1 仍返回 501）
    Api,
    /// 执行 PostgreSQL + ClickHouse 迁移后退出
    Migrate(MigrateArgs),
    /// 单进程运行 collector + api + 后台任务（Lite 形态）
    All,
    /// 站点管理
    Site(SiteArgs),
    /// 查询一个站点在日期范围内的核心指标（打印 JSON）
    Stats(StatsArgs),
}

#[derive(Debug, Args)]
pub struct SiteArgs {
    #[command(subcommand)]
    pub action: SiteAction,
}

#[derive(Debug, Subcommand)]
pub enum SiteAction {
    /// 建站：打印 site_key 和 site_id
    Add(SiteAddArgs),
}

#[derive(Debug, Args)]
pub struct SiteAddArgs {
    /// 站点名称
    #[arg(long)]
    pub name: String,
    /// hostname 白名单，至少一个（第一个是主域名），可写多个或重复使用：`--hostname a.com b.com`。
    /// 支持 `*.example.com` 通配（不含根域）。白名单为空时 collector 会丢弃该站点的全部事件
    #[arg(long, required = true, num_args = 1..)]
    pub hostname: Vec<String>,
    /// 站点时区（IANA 名），决定「本地日」的边界
    #[arg(long, default_value = "UTC")]
    pub timezone: String,
}

#[derive(Debug, Args)]
pub struct StatsArgs {
    /// 站点的 site_key
    #[arg(long)]
    pub site: String,
    /// 起始日期（站点本地日期，含），YYYY-MM-DD
    #[arg(long)]
    pub from: Date,
    /// 结束日期（站点本地日期，含），YYYY-MM-DD
    #[arg(long)]
    pub to: Date,
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
            // 管理命令连 PG（应用账号）和 ClickHouse（reader），按 api 角色校验配置
            Command::Site(_) | Command::Stats(_) => Role::Api,
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
        Command::Site(SiteArgs {
            action: SiteAction::Add(args),
        }) => admin::site_add(&config, args).await,
        Command::Stats(args) => admin::stats(&config, args).await,
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
    fn parses_site_add_with_one_or_many_hostnames() {
        let cli = Cli::try_parse_from([
            "rivu",
            "site",
            "add",
            "--name",
            "Blog",
            "--hostname",
            "a.com",
            "*.a.com",
            "--hostname",
            "b.com",
            "--timezone",
            "Europe/Berlin",
        ])
        .unwrap();
        let Command::Site(SiteArgs {
            action: SiteAction::Add(a),
        }) = cli.command
        else {
            panic!("不是 site add");
        };
        assert_eq!(a.name, "Blog");
        assert_eq!(a.hostname, ["a.com", "*.a.com", "b.com"]);
        assert_eq!(a.timezone, "Europe/Berlin");
        // 时区默认 UTC；hostname 必填
        let cli =
            Cli::try_parse_from(["rivu", "site", "add", "--name", "x", "--hostname", "a.com"])
                .unwrap();
        let Command::Site(SiteArgs {
            action: SiteAction::Add(a),
        }) = cli.command
        else {
            panic!("不是 site add");
        };
        assert_eq!(a.timezone, "UTC");
        assert!(Cli::try_parse_from(["rivu", "site", "add", "--name", "x"]).is_err());
    }

    #[test]
    fn parses_stats_dates() {
        let cli = Cli::try_parse_from([
            "rivu",
            "stats",
            "--site",
            "abcdefghijkl",
            "--from",
            "2026-03-28",
            "--to",
            "2026-03-30",
        ])
        .unwrap();
        let Command::Stats(a) = cli.command else {
            panic!("不是 stats");
        };
        assert_eq!(a.site, "abcdefghijkl");
        assert_eq!(a.from.to_string(), "2026-03-28");
        assert_eq!(a.to.to_string(), "2026-03-30");
        assert!(
            Cli::try_parse_from([
                "rivu",
                "stats",
                "--site",
                "x",
                "--from",
                "03-28",
                "--to",
                "2026-03-30"
            ])
            .is_err()
        );
        assert_eq!(
            Cli::try_parse_from([
                "rivu",
                "stats",
                "--site",
                "x",
                "--from",
                "2026-03-28",
                "--to",
                "2026-03-30"
            ])
            .unwrap()
            .command
            .role(),
            Role::Api
        );
    }

    #[test]
    fn version_flag_reports_version_and_commit() {
        let err = Cli::try_parse_from(["rivu", "--version"]).unwrap_err();
        let text = err.to_string();
        assert!(text.contains(build_info::VERSION));
        assert!(text.contains("commit"));
    }
}
