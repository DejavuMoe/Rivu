//! 命令行：`rivu collector | worker | api | migrate | all | site add | shield add|list|remove | apikey create|list|revoke | stats`。

use std::process::ExitCode;

use clap::{Args, Parser, Subcommand};
use jiff::civil::Date;
use rivu_common::build_info;
use rivu_common::config::{Config, Role};
use rivu_common::telemetry;

use crate::{admin, apikey_cmd, migrate, run, shield_cmd};

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
    /// 查询一个站点在日期范围内的核心指标与维度分组（打印 JSON）
    Stats(StatsArgs),
    /// 站点屏蔽规则管理（IP / CIDR / 国家 / 页面 / hostname）
    Shield(ShieldArgs),
    /// 公开 API 的 API key 管理
    Apikey(ApikeyArgs),
}

#[derive(Debug, Args)]
pub struct ShieldArgs {
    #[command(subcommand)]
    pub action: ShieldAction,
}

#[derive(Debug, Subcommand)]
pub enum ShieldAction {
    /// 添加一条规则（校验取值并检查每站上限 RIVU_SHIELD_MAX_RULES_PER_SITE）
    Add(ShieldAddArgs),
    /// 列出站点的规则
    List(ShieldListArgs),
    /// 删除一条规则（软删除）：用 --id，或用 --type 加 --value
    Remove(ShieldRemoveArgs),
}

/// 规则类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum ShieldType {
    /// 单个 IP 地址（IPv4 或 IPv6）
    Ip,
    /// 网段，如 10.0.0.0/8、2001:db8::/32
    Cidr,
    /// 国家码（ISO 3166-1 alpha-2，如 CN；ZZ 表示没有地理信息的事件）
    Country,
    /// hostname 模式（example.com 或 *.example.com），在站点白名单之内再屏蔽
    Hostname,
    /// 页面路径模式，以 / 开头，只有 * 是通配（如 /admin*）
    Page,
}

/// 规则动作。
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum ShieldActionKind {
    /// 命中就丢弃
    Deny,
    /// 同一类型内的例外：命中则不受该类型 deny 规则影响
    Allow,
}

#[derive(Debug, Args)]
pub struct ShieldAddArgs {
    /// 站点的 site_key
    #[arg(long)]
    pub site: String,
    /// 规则类型
    #[arg(long = "type", value_enum)]
    pub kind: ShieldType,
    /// 规则取值（按类型校验并规范化）
    #[arg(long)]
    pub value: String,
    /// 动作
    #[arg(long, value_enum, default_value = "deny")]
    pub action: ShieldActionKind,
    /// 备注
    #[arg(long)]
    pub description: Option<String>,
}

#[derive(Debug, Args)]
pub struct ShieldListArgs {
    /// 站点的 site_key
    #[arg(long)]
    pub site: String,
}

#[derive(Debug, Args)]
pub struct ShieldRemoveArgs {
    /// 站点的 site_key
    #[arg(long)]
    pub site: String,
    /// 规则 id（`shield list` 打印的 UUID）
    #[arg(long, conflicts_with_all = ["kind", "value"])]
    pub id: Option<String>,
    /// 规则类型（与 --value 一起使用）
    #[arg(long = "type", value_enum, requires = "value")]
    pub kind: Option<ShieldType>,
    /// 规则取值（与 --type 一起使用；会先按类型规范化再匹配）
    #[arg(long, requires = "kind")]
    pub value: Option<String>,
}

#[derive(Debug, Args)]
pub struct ApikeyArgs {
    #[command(subcommand)]
    pub action: ApikeyAction,
}

#[derive(Debug, Subcommand)]
pub enum ApikeyAction {
    /// 创建 API key：明文只在这里输出一次
    Create(ApikeyCreateArgs),
    /// 列出 API key（不含明文）
    List(ApikeyListArgs),
    /// 吊销 API key
    Revoke(ApikeyRevokeArgs),
}

#[derive(Debug, Args)]
pub struct ApikeyCreateArgs {
    /// 组织 slug
    #[arg(long)]
    pub org: String,
    /// key 所属用户的邮箱（必须是该组织成员）；省略则用该组织最早加入的 owner
    #[arg(long)]
    pub user: Option<String>,
    /// key 的名称
    #[arg(long)]
    pub name: String,
    /// 作用域，逗号分隔或重复使用：stats:read、sites:read、sites:write、exports:read；
    /// 目标默认 `*`（全部站点），也可写成 stats:read:<site_key>。默认 stats:read:*
    #[arg(long, value_delimiter = ',', num_args = 1..)]
    pub scopes: Vec<String>,
    /// 过期时间：时长（90d、12h、30m）、RFC 3339 时刻（2027-01-01T00:00:00Z）或日期（UTC 当天零点）；省略则不过期
    #[arg(long)]
    pub expires: Option<String>,
}

#[derive(Debug, Args)]
pub struct ApikeyListArgs {
    /// 只列这个组织的 key（slug）
    #[arg(long)]
    pub org: Option<String>,
}

#[derive(Debug, Args)]
pub struct ApikeyRevokeArgs {
    /// key 的 id（`apikey list` 的第一列）
    #[arg(long)]
    pub id: i64,
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
            Command::Site(_) | Command::Stats(_) | Command::Shield(_) | Command::Apikey(_) => {
                Role::Api
            }
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
        Command::Shield(ShieldArgs { action }) => shield_cmd::run(&config, action).await,
        Command::Apikey(ApikeyArgs { action }) => apikey_cmd::run(&config, action).await,
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
    fn parses_shield_commands() {
        let cli = Cli::try_parse_from([
            "rivu",
            "shield",
            "add",
            "--site",
            "abcdefghijkl",
            "--type",
            "cidr",
            "--value",
            "10.0.0.0/8",
            "--action",
            "allow",
            "--description",
            "办公室",
        ])
        .unwrap();
        assert_eq!(cli.command.role(), Role::Api);
        let Command::Shield(ShieldArgs {
            action: ShieldAction::Add(a),
        }) = cli.command
        else {
            panic!("不是 shield add");
        };
        assert_eq!(
            (a.site.as_str(), a.kind, a.action),
            ("abcdefghijkl", ShieldType::Cidr, ShieldActionKind::Allow)
        );
        assert_eq!(a.description.as_deref(), Some("办公室"));
        // 动作默认 deny；五种类型都认
        for t in ["ip", "cidr", "country", "hostname", "page"] {
            let cli = Cli::try_parse_from([
                "rivu", "shield", "add", "--site", "x", "--type", t, "--value", "v",
            ])
            .unwrap();
            let Command::Shield(ShieldArgs {
                action: ShieldAction::Add(a),
            }) = cli.command
            else {
                panic!("不是 shield add");
            };
            assert_eq!(a.action, ShieldActionKind::Deny);
        }
        assert!(
            Cli::try_parse_from([
                "rivu", "shield", "add", "--site", "x", "--type", "regex", "--value", "v"
            ])
            .is_err()
        );
        assert!(
            Cli::try_parse_from(["rivu", "shield", "add", "--site", "x", "--type", "ip"]).is_err()
        );
        assert!(Cli::try_parse_from(["rivu", "shield", "list", "--site", "x"]).is_ok());
        assert!(Cli::try_parse_from(["rivu", "shield", "list"]).is_err());

        // remove：--id，或 --type 加 --value，二者不能混用
        assert!(
            Cli::try_parse_from(["rivu", "shield", "remove", "--site", "x", "--id", "u"]).is_ok()
        );
        assert!(
            Cli::try_parse_from([
                "rivu", "shield", "remove", "--site", "x", "--type", "ip", "--value", "1.2.3.4"
            ])
            .is_ok()
        );
        assert!(
            Cli::try_parse_from(["rivu", "shield", "remove", "--site", "x", "--type", "ip"])
                .is_err()
        );
        assert!(
            Cli::try_parse_from([
                "rivu", "shield", "remove", "--site", "x", "--value", "1.2.3.4"
            ])
            .is_err()
        );
        assert!(
            Cli::try_parse_from([
                "rivu", "shield", "remove", "--site", "x", "--id", "u", "--type", "ip", "--value",
                "v"
            ])
            .is_err()
        );
    }

    #[test]
    fn parses_apikey_commands() {
        let cli = Cli::try_parse_from([
            "rivu",
            "apikey",
            "create",
            "--org",
            "acme",
            "--user",
            "a@b.c",
            "--name",
            "ci",
            "--scopes",
            "stats:read,sites:read",
            "--scopes",
            "exports:read",
            "--expires",
            "90d",
        ])
        .unwrap();
        assert_eq!(cli.command.role(), Role::Api);
        let Command::Apikey(ApikeyArgs {
            action: ApikeyAction::Create(a),
        }) = cli.command
        else {
            panic!("不是 apikey create");
        };
        assert_eq!(
            (a.org.as_str(), a.user.as_deref(), a.name.as_str()),
            ("acme", Some("a@b.c"), "ci")
        );
        assert_eq!(a.scopes, ["stats:read", "sites:read", "exports:read"]);
        assert_eq!(a.expires.as_deref(), Some("90d"));
        // 只给必填项：用户、scope、过期都可省
        let cli =
            Cli::try_parse_from(["rivu", "apikey", "create", "--org", "acme", "--name", "ci"])
                .unwrap();
        let Command::Apikey(ApikeyArgs {
            action: ApikeyAction::Create(a),
        }) = cli.command
        else {
            panic!("不是 apikey create");
        };
        assert!(a.user.is_none() && a.scopes.is_empty() && a.expires.is_none());
        assert!(Cli::try_parse_from(["rivu", "apikey", "create", "--org", "acme"]).is_err());
        assert!(Cli::try_parse_from(["rivu", "apikey", "list"]).is_ok());
        assert!(Cli::try_parse_from(["rivu", "apikey", "list", "--org", "acme"]).is_ok());
        let cli = Cli::try_parse_from(["rivu", "apikey", "revoke", "--id", "7"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::Apikey(ApikeyArgs {
                action: ApikeyAction::Revoke(ApikeyRevokeArgs { id: 7 })
            })
        ));
        assert!(Cli::try_parse_from(["rivu", "apikey", "revoke", "--id", "x"]).is_err());
    }

    #[test]
    fn version_flag_reports_version_and_commit() {
        let err = Cli::try_parse_from(["rivu", "--version"]).unwrap_err();
        let text = err.to_string();
        assert!(text.contains(build_info::VERSION));
        assert!(text.contains("commit"));
    }
}
