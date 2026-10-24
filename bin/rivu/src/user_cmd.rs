//! `rivu setup`、`rivu user add | disable | enable | list`、`rivu org create`、`rivu org member add`、
//! `rivu share create | list | revoke`：首次初始化、用户、组织与共享链接的命令行管理（支撑测试与运维）。
//!
//! 直接连 PostgreSQL（`RIVU_DATABASE_URL`，应用账号），不经过 HTTP，所以不受 `RIVU_SETUP_TOKEN` 约束
//! （能连库的人本来就能改库）。**不接受命令行明文密码参数**（会进 shell 历史与进程列表）：
//! 密码只从 `--password-file <路径>`（去掉结尾的一个换行）或 `--password-stdin`（读第一行）来。

use std::io::BufRead;
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use jiff::Timestamp;
use rivu_auth::identity::{normalize_email, normalize_org_name, slugify};
use rivu_auth::password::{hash_blocking, validate_password};
use rivu_auth::session;
use rivu_common::config::Config;
use rivu_storage_pg::shared_links::NewLink;
use rivu_storage_pg::users::{NewUser, SetupDone};
use sqlx::PgPool;

use crate::apikey_cmd::parse_expiry;
use crate::cli::{
    OrgAction, OrgArgs, OrgMemberAction, OrgRoleArg, PasswordArgs, SetupArgs, ShareAction,
    ShareArgs, ShareCreateArgs, UserAction, UserArgs,
};

/// 读密码：`--password-file`（去掉结尾的一个换行，其余空白原样保留）或 `--password-stdin`（第一行）。
/// 两个都没给返回 `None`。
pub fn read_password(args: &PasswordArgs, stdin: &mut impl BufRead) -> Result<Option<String>> {
    let raw = match (&args.password_file, args.password_stdin) {
        (Some(path), false) => std::fs::read_to_string(path)
            .with_context(|| format!("无法读取密码文件 {}", path.display()))?,
        (None, true) => {
            let mut line = String::new();
            stdin.read_line(&mut line).context("读取标准输入失败")?;
            line
        }
        (None, false) => return Ok(None),
        (Some(_), true) => bail!("--password-file 与 --password-stdin 只能给一个"),
    };
    let pw = raw
        .strip_suffix("\r\n")
        .or_else(|| raw.strip_suffix('\n'))
        .unwrap_or(&raw);
    if pw.is_empty() {
        bail!("密码是空的");
    }
    Ok(Some(pw.to_owned()))
}

/// 必须给出密码。
fn require_password(args: &PasswordArgs) -> Result<String> {
    read_password(args, &mut std::io::stdin().lock())?
        .context("需要密码：用 --password-file <路径> 或 --password-stdin（不接受命令行明文密码）")
}

/// 校验并哈希一个密码（长度规则同 HTTP 端点）。
fn hash_password(password: &str) -> Result<String> {
    validate_password(password).map_err(anyhow::Error::msg)?;
    hash_blocking(password).map_err(|e| anyhow::anyhow!("{e}"))
}

// ------------------------------------------------------------------------------------------
// setup
// ------------------------------------------------------------------------------------------

/// 首次初始化（CLI 与测试共用）：校验输入，`users` 为空时创建首个用户（实例管理员）、首个组织与 owner 关系。
/// 已经有用户返回错误。
pub async fn setup_with(
    pool: &PgPool,
    email: &str,
    password: &str,
    org_name: &str,
    name: Option<&str>,
) -> Result<SetupDone> {
    let email = normalize_email(email).map_err(anyhow::Error::msg)?;
    let org_name = normalize_org_name(org_name).map_err(anyhow::Error::msg)?;
    let hash = hash_password(password)?;
    rivu_storage_pg::users::create_first_user(
        pool,
        &email,
        name.map(str::trim).filter(|n| !n.is_empty()),
        &hash,
        &org_name,
        &slugify(&org_name),
    )
    .await?
    .context("实例已经初始化过了：users 表里已经有用户（用 `rivu user add` 添加更多用户）")
}

pub async fn setup(config: &Config, args: SetupArgs) -> Result<ExitCode> {
    let password = require_password(&args.password)?;
    let pool = rivu_storage_pg::connect(config.database_url.expose(), 2).await?;
    let done = setup_with(
        &pool,
        &args.email,
        &password,
        &args.org_name,
        args.name.as_deref(),
    )
    .await?;
    println!("初始化完成。");
    println!("user_id: {}", done.user_id);
    println!("org_id: {}", done.org_id);
    println!("org: {}", done.org_slug);
    Ok(ExitCode::SUCCESS)
}

// ------------------------------------------------------------------------------------------
// user
// ------------------------------------------------------------------------------------------

pub async fn user(config: &Config, args: UserArgs) -> Result<ExitCode> {
    let pool = rivu_storage_pg::connect(config.database_url.expose(), 2).await?;
    match args.action {
        UserAction::Add(a) => {
            let password = require_password(&a.password)?;
            let email = normalize_email(&a.email).map_err(anyhow::Error::msg)?;
            let hash = hash_password(&password)?;
            let id = rivu_storage_pg::users::create_user(
                &pool,
                &NewUser {
                    email: &email,
                    name: a.name.as_deref().map(str::trim).filter(|n| !n.is_empty()),
                    password_hash: &hash,
                    is_instance_admin: a.instance_admin,
                },
            )
            .await?;
            println!("用户已创建。");
            println!("id: {id}");
            println!("email: {email}");
        }
        UserAction::Disable(a) => set_disabled(&pool, &a.email, true).await?,
        UserAction::Enable(a) => set_disabled(&pool, &a.email, false).await?,
        UserAction::List => {
            let rows = rivu_storage_pg::users::list_users(&pool).await?;
            if rows.is_empty() {
                println!("(没有用户)");
            }
            for r in rows {
                println!(
                    "{}\t{}\t{}\t{}{}\t{}\t{}",
                    r.id,
                    r.email,
                    r.name.as_deref().unwrap_or("-"),
                    if r.is_instance_admin {
                        "instance-admin"
                    } else {
                        "-"
                    },
                    if r.disabled { "\tdisabled" } else { "" },
                    if r.memberships.is_empty() {
                        "-".to_owned()
                    } else {
                        r.memberships.join(",")
                    },
                    r.created_at,
                );
            }
        }
    }
    Ok(ExitCode::SUCCESS)
}

async fn set_disabled(pool: &PgPool, email: &str, disabled: bool) -> Result<()> {
    let email = normalize_email(email).map_err(anyhow::Error::msg)?;
    if !rivu_storage_pg::users::set_disabled(pool, &email, disabled).await? {
        bail!("找不到用户 `{email}`（或已删除）");
    }
    println!(
        "用户 {email} 已{}{}",
        if disabled { "禁用" } else { "启用" },
        if disabled {
            "，他的全部登录会话已撤销"
        } else {
            ""
        }
    );
    Ok(())
}

// ------------------------------------------------------------------------------------------
// org
// ------------------------------------------------------------------------------------------

pub async fn org(config: &Config, args: OrgArgs) -> Result<ExitCode> {
    let pool = rivu_storage_pg::connect(config.database_url.expose(), 2).await?;
    match args.action {
        OrgAction::Create(a) => {
            let name = normalize_org_name(&a.name).map_err(anyhow::Error::msg)?;
            let slug = a.slug.unwrap_or_else(|| slugify(&name));
            let id = rivu_storage_pg::users::create_org(&pool, &name, &slug).await?;
            println!("组织已创建。");
            println!("id: {id}");
            println!("slug: {slug}");
        }
        OrgAction::Member(OrgMemberAction::Add(a)) => {
            let Some(org_id) =
                rivu_storage_pg::api_keys::find_org_id_by_slug(&pool, a.org.trim()).await?
            else {
                bail!("找不到组织 `{}`（或已删除）", a.org);
            };
            let email = normalize_email(&a.email).map_err(anyhow::Error::msg)?;
            let Some(user) = rivu_storage_pg::users::find_by_email(&pool, &email).await? else {
                bail!("找不到用户 `{email}`（或已删除）");
            };
            let role = a.role.as_str();
            let (old, new) =
                rivu_storage_pg::users::upsert_member(&pool, org_id, user.id, role).await?;
            match old {
                None => println!("{email} 已加入 {}，角色 {new}", a.org),
                Some(o) => println!("{email} 在 {} 的角色：{o} -> {new}", a.org),
            }
        }
    }
    Ok(ExitCode::SUCCESS)
}

impl OrgRoleArg {
    pub fn as_str(self) -> &'static str {
        match self {
            OrgRoleArg::Owner => "owner",
            OrgRoleArg::Admin => "admin",
            OrgRoleArg::Editor => "editor",
            OrgRoleArg::Viewer => "viewer",
        }
    }
}

// ------------------------------------------------------------------------------------------
// share
// ------------------------------------------------------------------------------------------

/// 新建的共享链接。
#[derive(Debug)]
pub struct IssuedLink {
    pub id: i64,
    pub slug: String,
    pub has_password: bool,
    pub expires_at: Option<Timestamp>,
}

/// 建共享链接（CLI 与测试共用）。slug 是 32 字节随机数的 base64url（43 个字符，`shared_links.slug` 要求至少 24 个）。
pub async fn issue_link(
    pool: &PgPool,
    site_key: &str,
    password: Option<&str>,
    expires: Option<&str>,
) -> Result<IssuedLink> {
    let password_hash = password.map(hash_password).transpose()?;
    let expires_at = expires
        .map(|e| parse_expiry(e, Timestamp::now()))
        .transpose()?;
    let slug = session::generate().plaintext().to_owned();
    let id = rivu_storage_pg::shared_links::create(
        pool,
        &NewLink {
            site_key,
            slug: &slug,
            password_hash: password_hash.as_deref(),
            expires_at_unix: expires_at,
            created_by: None,
            name: None,
        },
    )
    .await?;
    Ok(IssuedLink {
        id,
        slug,
        has_password: password_hash.is_some(),
        expires_at: expires_at.and_then(|s| Timestamp::from_second(s).ok()),
    })
}

pub async fn share(config: &Config, args: ShareArgs) -> Result<ExitCode> {
    let pool = rivu_storage_pg::connect(config.database_url.expose(), 2).await?;
    match args.action {
        ShareAction::Create(ShareCreateArgs {
            site,
            password,
            expires,
        }) => {
            let pw = read_password(&password, &mut std::io::stdin().lock())?;
            let link = issue_link(&pool, &site, pw.as_deref(), expires.as_deref()).await?;
            println!("共享链接已创建。slug 就是访问凭证，请当作秘密保管。");
            println!("id: {}", link.id);
            println!("slug: {}", link.slug);
            println!("password: {}", if link.has_password { "yes" } else { "no" });
            if let Some(e) = link.expires_at {
                println!("expires: {e}");
            }
            println!(
                "入口: GET /api/internal/shared/{}；查询时带请求头 X-Rivu-Share: <slug>",
                link.slug
            );
        }
        ShareAction::List(a) => {
            let rows = rivu_storage_pg::shared_links::list(&pool, &a.site).await?;
            if rows.is_empty() {
                println!("(没有共享链接)");
            }
            for r in rows {
                println!(
                    "{}\t{}\t{}\t{}\t{}{}",
                    r.id,
                    r.slug,
                    if r.has_password { "password" } else { "-" },
                    r.expires_at.as_deref().unwrap_or("-"),
                    r.created_at,
                    if r.revoked { "\trevoked" } else { "" },
                );
            }
        }
        ShareAction::Revoke(a) => {
            if !rivu_storage_pg::shared_links::revoke(&pool, a.id).await? {
                bail!("找不到共享链接 {}（或已吊销）", a.id);
            }
            println!("共享链接 {} 已吊销", a.id);
        }
    }
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;
    use std::path::PathBuf;

    use super::*;

    fn temp_file(name: &str, content: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("rivu-pw-test-{}-{name}", std::process::id()));
        std::fs::write(&p, content).unwrap();
        p
    }

    fn args(file: Option<PathBuf>, stdin: bool) -> PasswordArgs {
        PasswordArgs {
            password_file: file,
            password_stdin: stdin,
        }
    }

    #[test]
    fn password_file_strips_exactly_one_trailing_newline_and_keeps_other_whitespace() {
        let mut empty = Cursor::new(Vec::new());
        for (name, content, want) in [
            ("a", "correct horse battery\n", "correct horse battery"),
            ("b", "correct horse battery\r\n", "correct horse battery"),
            ("c", "  spaced password  \n", "  spaced password  "),
            ("d", "no newline at all", "no newline at all"),
            ("e", "two\n\n", "two\n"),
        ] {
            let f = temp_file(name, content);
            let got = read_password(&args(Some(f.clone()), false), &mut empty).unwrap();
            assert_eq!(got.as_deref(), Some(want), "{content:?}");
            let _ = std::fs::remove_file(f);
        }
    }

    #[test]
    fn stdin_reads_the_first_line_only() {
        let mut input = Cursor::new(b"first line password\nsecond line\n".to_vec());
        let got = read_password(&args(None, true), &mut input).unwrap();
        assert_eq!(got.as_deref(), Some("first line password"));
    }

    #[test]
    fn missing_empty_and_conflicting_sources_are_handled() {
        let mut empty = Cursor::new(Vec::new());
        assert_eq!(read_password(&args(None, false), &mut empty).unwrap(), None);
        assert!(
            read_password(&args(None, true), &mut empty).is_err(),
            "空输入"
        );
        let f = temp_file("empty", "\n");
        assert!(read_password(&args(Some(f.clone()), false), &mut empty).is_err());
        let _ = std::fs::remove_file(f);
        assert!(
            read_password(
                &args(Some(PathBuf::from("/definitely/not/here")), false),
                &mut empty
            )
            .is_err()
        );
        assert!(read_password(&args(Some(PathBuf::from("x")), true), &mut empty).is_err());
    }

    #[test]
    fn short_passwords_are_rejected_before_hashing() {
        assert!(hash_password("short").is_err());
        let h = hash_password("a long enough password").unwrap();
        assert!(h.starts_with("$argon2id$"));
    }

    #[test]
    fn roles_map_to_the_database_values() {
        assert_eq!(OrgRoleArg::Owner.as_str(), "owner");
        assert_eq!(OrgRoleArg::Viewer.as_str(), "viewer");
    }
}
