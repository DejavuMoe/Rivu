//! 命令行：`rivu serve | migrate | healthcheck | site add | import plausible|list|delete | shield add|list|remove | apikey create|list|revoke | stats |
//! setup | user add|disable|enable|list | org create | org member add | share create|list|revoke`。

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Args, Parser, Subcommand};
use rivu_common::build_info;
use rivu_common::config::{Config, Role};
use rivu_common::telemetry;

use crate::{admin, apikey_cmd, migrate, run, shield_cmd, user_cmd};

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
    /// 运行 Rivu：采集、查询 API、看板与后台任务（部署形态，一个实例）
    Serve,
    /// 执行 PostgreSQL + ClickHouse 迁移后退出
    Migrate(MigrateArgs),
    /// 站点管理
    Site(SiteArgs),
    /// 导入外部数据：plausible | list | delete
    Import(ImportArgs),
    /// 导出 CSV：时间范围与筛选下的趋势与各维度明细，写成一个 zip（同看板的「导出」）
    Export(ExportArgs),
    /// 执行一个 QuerySpec（同 `POST /api/v1/query`，不需要 API key），打印 QueryResult JSON
    Query(QueryArgs),
    /// 站点屏蔽规则管理（IP / CIDR / 国家 / 页面 / hostname）
    Shield(ShieldArgs),
    /// 公开 API 的 API key 管理
    Apikey(ApikeyArgs),
    /// 首次初始化：创建首个用户（实例管理员）、首个组织与 owner 关系（只在 users 为空时可用）
    Setup(SetupArgs),
    /// 用户管理：add | disable | enable | list
    User(UserArgs),
    /// 组织管理：create | member add
    Org(OrgArgs),
    /// 共享链接管理：create | list | revoke
    Share(ShareArgs),
    /// 容器健康检查：请求内部端口的 /readyz，200 时退出码 0，否则 1（不读取其他配置）
    Healthcheck(HealthcheckArgs),
}

#[derive(Debug, Args)]
pub struct HealthcheckArgs {
    /// 内部端口地址；默认取 RIVU_INTERNAL_ADDR（0.0.0.0 换成 127.0.0.1），再没有则 127.0.0.1:9090
    #[arg(long, value_name = "HOST:PORT")]
    pub addr: Option<String>,
    /// 检查的路径：/readyz（就绪）或 /healthz（存活）
    #[arg(long, default_value = "/readyz")]
    pub path: String,
}

/// 密码来源。**没有命令行明文密码参数**（会进 shell 历史与进程列表）。
#[derive(Debug, Args, Clone, Default)]
pub struct PasswordArgs {
    /// 从文件读密码（去掉结尾的一个换行）
    #[arg(long, value_name = "PATH", conflicts_with = "password_stdin")]
    pub password_file: Option<std::path::PathBuf>,
    /// 从标准输入读密码（第一行）
    #[arg(long)]
    pub password_stdin: bool,
}

#[derive(Debug, Args)]
pub struct SetupArgs {
    /// 首个用户的邮箱
    #[arg(long)]
    pub email: String,
    /// 首个组织的名称（1 到 100 个字符）
    #[arg(long)]
    pub org_name: String,
    /// 首个用户的显示名（可选）
    #[arg(long)]
    pub name: Option<String>,
    #[command(flatten)]
    pub password: PasswordArgs,
}

#[derive(Debug, Args)]
pub struct UserArgs {
    #[command(subcommand)]
    pub action: UserAction,
}

#[derive(Debug, Subcommand)]
pub enum UserAction {
    /// 创建用户（密码 12–128 个字符）
    Add(UserAddArgs),
    /// 禁用用户：他的登录会话立即失效
    Disable(UserEmailArgs),
    /// 重新启用用户（不恢复已撤销的会话）
    Enable(UserEmailArgs),
    /// 列出用户
    List,
}

#[derive(Debug, Args)]
pub struct UserAddArgs {
    /// 邮箱
    #[arg(long)]
    pub email: String,
    /// 显示名（可选）
    #[arg(long)]
    pub name: Option<String>,
    /// 设为实例管理员（只管用户与实例设置，不能据此读取任意站点的统计）
    #[arg(long)]
    pub instance_admin: bool,
    #[command(flatten)]
    pub password: PasswordArgs,
}

#[derive(Debug, Args)]
pub struct UserEmailArgs {
    /// 用户的邮箱
    #[arg(long)]
    pub email: String,
}

#[derive(Debug, Args)]
pub struct OrgArgs {
    #[command(subcommand)]
    pub action: OrgAction,
}

#[derive(Debug, Subcommand)]
pub enum OrgAction {
    /// 创建组织
    Create(OrgCreateArgs),
    /// 组织成员管理
    #[command(subcommand)]
    Member(OrgMemberAction),
}

#[derive(Debug, Args)]
pub struct OrgCreateArgs {
    /// 组织名称（1 到 100 个字符）
    #[arg(long)]
    pub name: String,
    /// slug（省略则由名称生成）
    #[arg(long)]
    pub slug: Option<String>,
}

#[derive(Debug, Subcommand)]
pub enum OrgMemberAction {
    /// 把用户加入组织，或改他的角色（至少保留一名 owner）
    Add(OrgMemberAddArgs),
}

/// 组织角色。
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum OrgRoleArg {
    Owner,
    Admin,
    Editor,
    Viewer,
}

#[derive(Debug, Args)]
pub struct OrgMemberAddArgs {
    /// 组织 slug
    #[arg(long)]
    pub org: String,
    /// 用户的邮箱
    #[arg(long)]
    pub email: String,
    /// 角色
    #[arg(long, value_enum, default_value = "viewer")]
    pub role: OrgRoleArg,
}

#[derive(Debug, Args)]
pub struct ShareArgs {
    #[command(subcommand)]
    pub action: ShareAction,
}

#[derive(Debug, Subcommand)]
pub enum ShareAction {
    /// 创建共享链接：slug 只在这里与 `list` 里显示
    Create(ShareCreateArgs),
    /// 列出站点的共享链接
    List(ShareListArgs),
    /// 吊销共享链接（立即失效，已签发的访问凭证一并删除）
    Revoke(ShareRevokeArgs),
}

#[derive(Debug, Args)]
pub struct ShareCreateArgs {
    /// 站点的 site_key
    #[arg(long)]
    pub site: String,
    /// 可选的链接密码（argon2id；同样只能从文件或标准输入来）
    #[command(flatten)]
    pub password: PasswordArgs,
    /// 过期时间：时长（7d、12h、30m）、RFC 3339 时刻或日期；省略则不过期
    #[arg(long)]
    pub expires: Option<String>,
}

#[derive(Debug, Args)]
pub struct ShareListArgs {
    /// 站点的 site_key
    #[arg(long)]
    pub site: String,
}

#[derive(Debug, Args)]
pub struct ShareRevokeArgs {
    /// 链接的 id（`share list` 的第一列）
    #[arg(long)]
    pub id: i64,
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
pub struct ImportArgs {
    #[command(subcommand)]
    pub action: ImportAction,
}

#[derive(Debug, Subcommand)]
pub enum ImportAction {
    /// 导入 Plausible 的数据导出（站点设置 → Imports & Exports → Export 得到的 zip），写进日汇总层
    Plausible {
        /// 站点的 site_key
        #[arg(long)]
        site: String,
        /// 导出的 zip 文件
        file: PathBuf,
    },
    /// 列出站点的导入
    List {
        #[arg(long)]
        site: String,
    },
    /// 删除一次导入（连同它写入的汇总数据）
    Delete {
        #[arg(long)]
        site: String,
        /// 导入的 id（见 list）
        #[arg(long)]
        id: i64,
    },
}

#[derive(Debug, Args)]
pub struct ExportArgs {
    /// 站点的 site_key
    #[arg(long)]
    pub site: String,
    /// 时间范围，QuerySpec 的写法：30d、month、12mo、all，或 2026-01-01,2026-03-31
    #[arg(long, default_value = "30d")]
    pub range: String,
    /// 筛选，QuerySpec 的 filters JSON 数组
    #[arg(long, value_name = "JSON")]
    pub filters: Option<String>,
    /// 趋势的粒度：minute | hour | day | week | month
    #[arg(long, default_value = "day")]
    pub interval: String,
    /// 输出文件；省略时写到当前目录，文件名同看板下载
    #[arg(long, value_name = "PATH")]
    pub out: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub struct QueryArgs {
    /// QuerySpec JSON 文件（`site` 必填）；省略时从 stdin 读
    #[arg(long, value_name = "PATH")]
    pub spec: Option<PathBuf>,
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
            Command::Serve => Role::Serve,
            Command::Migrate(_) => Role::Migrate,
            // 管理命令连 PG（应用账号）和 ClickHouse（reader）
            Command::Site(_)
            | Command::Query(_)
            | Command::Export(_)
            | Command::Import(_)
            | Command::Shield(_)
            | Command::Apikey(_)
            | Command::Setup(_)
            | Command::User(_)
            | Command::Org(_)
            | Command::Share(_)
            | Command::Healthcheck(_) => Role::Admin,
        }
    }
}

pub async fn main() -> ExitCode {
    let cli = Cli::parse();
    // 健康检查在容器里频繁执行，不加载、不校验配置
    if let Command::Healthcheck(args) = &cli.command {
        return crate::healthcheck::run(args).await;
    }
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
        Command::Query(args) => admin::query(&config, args).await,
        Command::Export(args) => admin::export(&config, args).await,
        Command::Import(ImportArgs { action }) => admin::import(&config, action).await,
        Command::Shield(ShieldArgs { action }) => shield_cmd::run(&config, action).await,
        Command::Apikey(ApikeyArgs { action }) => apikey_cmd::run(&config, action).await,
        Command::Setup(args) => user_cmd::setup(&config, args).await,
        Command::User(args) => user_cmd::user(&config, args).await,
        Command::Org(args) => user_cmd::org(&config, args).await,
        Command::Share(args) => user_cmd::share(&config, args).await,
        Command::Serve => run::run(config).await,
        Command::Healthcheck(_) => unreachable!("健康检查在加载配置之前处理"),
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
        for (arg, role) in [("serve", Role::Serve), ("migrate", Role::Migrate)] {
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
    fn parses_query_spec_source() {
        let cli = Cli::try_parse_from(["rivu", "query", "--spec", "q.json"]).unwrap();
        let Command::Query(a) = &cli.command else {
            panic!("不是 query");
        };
        assert_eq!(a.spec.as_deref(), Some(std::path::Path::new("q.json")));
        assert_eq!(cli.command.role(), Role::Admin);
        // 省略 --spec：从 stdin 读
        let cli = Cli::try_parse_from(["rivu", "query"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::Query(QueryArgs { spec: None })
        ));
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
        assert_eq!(cli.command.role(), Role::Admin);
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
        assert_eq!(cli.command.role(), Role::Admin);
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
    fn parses_setup_user_org_and_share_commands() {
        let p = |args: &[&str]| {
            Cli::try_parse_from(std::iter::once("rivu").chain(args.iter().copied()))
        };
        // setup：密码只来自文件或标准输入，没有明文参数
        let cli = p(&[
            "setup",
            "--email",
            "a@b.co",
            "--org-name",
            "Acme",
            "--password-file",
            "pw.txt",
        ])
        .unwrap();
        assert_eq!(cli.command.role(), Role::Admin);
        let Command::Setup(a) = cli.command else {
            panic!("不是 setup");
        };
        assert_eq!(
            (
                a.email.as_str(),
                a.org_name.as_str(),
                a.password.password_stdin
            ),
            ("a@b.co", "Acme", false)
        );
        assert!(a.password.password_file.is_some());
        assert!(
            p(&[
                "setup",
                "--email",
                "a@b.co",
                "--org-name",
                "x",
                "--password-stdin"
            ])
            .is_ok()
        );
        assert!(
            p(&[
                "setup",
                "--email",
                "a@b.co",
                "--org-name",
                "x",
                "--password",
                "hunter2hunter2"
            ])
            .is_err()
        );
        assert!(
            p(&[
                "setup",
                "--email",
                "a@b.co",
                "--org-name",
                "x",
                "--password-file",
                "f",
                "--password-stdin"
            ])
            .is_err(),
            "两个来源互斥"
        );
        assert!(p(&["setup", "--email", "a@b.co"]).is_err());

        // user
        let cli = p(&[
            "user",
            "add",
            "--email",
            "u@b.co",
            "--instance-admin",
            "--password-stdin",
        ])
        .unwrap();
        let Command::User(UserArgs {
            action: UserAction::Add(a),
        }) = cli.command
        else {
            panic!("不是 user add");
        };
        assert!(a.instance_admin && a.password.password_stdin);
        assert!(p(&["user", "disable", "--email", "u@b.co"]).is_ok());
        assert!(p(&["user", "enable", "--email", "u@b.co"]).is_ok());
        assert!(p(&["user", "list"]).is_ok());
        assert!(p(&["user", "add", "--email", "u@b.co", "--password", "x"]).is_err());

        // org
        assert!(p(&["org", "create", "--name", "Acme"]).is_ok());
        let cli = p(&[
            "org", "member", "add", "--org", "acme", "--email", "u@b.co", "--role", "editor",
        ])
        .unwrap();
        let Command::Org(OrgArgs {
            action: OrgAction::Member(OrgMemberAction::Add(a)),
        }) = cli.command
        else {
            panic!("不是 org member add");
        };
        assert_eq!(a.role, OrgRoleArg::Editor);
        let cli = p(&["org", "member", "add", "--org", "acme", "--email", "u@b.co"]).unwrap();
        let Command::Org(OrgArgs {
            action: OrgAction::Member(OrgMemberAction::Add(a)),
        }) = cli.command
        else {
            panic!();
        };
        assert_eq!(a.role, OrgRoleArg::Viewer, "角色默认 viewer");
        assert!(
            p(&[
                "org", "member", "add", "--org", "a", "--email", "u@b.co", "--role", "root"
            ])
            .is_err()
        );

        // share
        let cli = p(&[
            "share",
            "create",
            "--site",
            "abcdefghijkl",
            "--password-file",
            "pw",
            "--expires",
            "7d",
        ])
        .unwrap();
        let Command::Share(ShareArgs {
            action: ShareAction::Create(a),
        }) = cli.command
        else {
            panic!("不是 share create");
        };
        assert_eq!(
            (a.site.as_str(), a.expires.as_deref()),
            ("abcdefghijkl", Some("7d"))
        );
        assert!(
            p(&["share", "create", "--site", "abcdefghijkl"]).is_ok(),
            "密码可选"
        );
        assert!(p(&["share", "list", "--site", "x"]).is_ok());
        assert!(p(&["share", "revoke", "--id", "3"]).is_ok());
        assert!(p(&["share", "revoke", "--id", "x"]).is_err());
    }

    #[test]
    fn version_flag_reports_version_and_commit() {
        let err = Cli::try_parse_from(["rivu", "--version"]).unwrap_err();
        let text = err.to_string();
        assert!(text.contains(build_info::VERSION));
        assert!(text.contains("commit"));
    }
}
