//! 用户、组织与成员关系（`rivu user ...`、`rivu org ...`、首次初始化、登录）。
//!
//! 表结构见 `migrations/postgres/20261021100000_init.sql`。邮箱列是 `citext`（不区分大小写，
//! 唯一约束只管未删除的行），这里一律以 `text` 传入、查询里转 `citext`。
//! 本 crate 的 sqlx 没有启用时间类型特性：时间以 ISO 文本（UTC）读出。

use anyhow::{Context, Result, bail};
use sqlx::{PgPool, Row};

/// 登录需要的用户信息（含密码哈希）。
#[derive(Clone, PartialEq, Eq)]
pub struct LoginUser {
    pub id: i64,
    pub email: String,
    pub name: Option<String>,
    pub password_hash: String,
    pub is_instance_admin: bool,
    /// `disabled_at` 非空。
    pub disabled: bool,
}

impl std::fmt::Debug for LoginUser {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LoginUser")
            .field("id", &self.id)
            .field("password_hash", &"[redacted]")
            .field("disabled", &self.disabled)
            .finish()
    }
}

/// 按邮箱查未删除的用户（含已禁用的：由调用方决定怎么处理）。
pub async fn find_by_email(pool: &PgPool, email: &str) -> Result<Option<LoginUser>> {
    let row = sqlx::query(
        "SELECT id, email::text AS email, name, password_hash, is_instance_admin,
                (disabled_at IS NOT NULL) AS disabled
         FROM users WHERE email = ($1::text)::citext AND deleted_at IS NULL",
    )
    .bind(email)
    .fetch_optional(pool)
    .await
    .context("查询用户失败")?;
    row.map(|r| {
        Ok(LoginUser {
            id: r.try_get("id")?,
            email: r.try_get("email")?,
            name: r.try_get("name")?,
            password_hash: r.try_get("password_hash")?,
            is_instance_admin: r.try_get("is_instance_admin")?,
            disabled: r.try_get("disabled")?,
        })
    })
    .transpose()
}

/// 新用户的参数。
#[derive(Clone)]
pub struct NewUser<'a> {
    /// 已规范化的邮箱。
    pub email: &'a str,
    pub name: Option<&'a str>,
    /// argon2id 的 PHC 字符串。
    pub password_hash: &'a str,
    pub is_instance_admin: bool,
}

/// 建用户，返回 `users.id`。邮箱已被未删除的用户使用则报错。
pub async fn create_user(pool: &PgPool, new: &NewUser<'_>) -> Result<i64> {
    let id: Option<i64> = sqlx::query_scalar(
        "INSERT INTO users (email, password_hash, name, is_instance_admin)
         VALUES (($1::text)::citext, $2, $3, $4)
         ON CONFLICT (email) WHERE deleted_at IS NULL DO NOTHING
         RETURNING id",
    )
    .bind(new.email)
    .bind(new.password_hash)
    .bind(new.name)
    .bind(new.is_instance_admin)
    .fetch_optional(pool)
    .await
    .context("写入 users 失败")?;
    id.with_context(|| format!("邮箱 `{}` 已被使用", new.email))
}

/// 禁用或启用用户；禁用时同一个事务内撤销他的全部登录会话（会话读取路径同样会校验用户状态，这里是双保险）。
/// 返回是否找到了这个用户。
pub async fn set_disabled(pool: &PgPool, email: &str, disabled: bool) -> Result<bool> {
    let mut tx = pool.begin().await.context("开启事务失败")?;
    let id: Option<i64> = sqlx::query_scalar(
        "UPDATE users SET disabled_at = CASE WHEN $2 THEN COALESCE(disabled_at, now()) ELSE NULL END
         WHERE email = ($1::text)::citext AND deleted_at IS NULL RETURNING id",
    )
    .bind(email)
    .bind(disabled)
    .fetch_optional(&mut *tx)
    .await
    .context("更新 users.disabled_at 失败")?;
    let Some(id) = id else {
        return Ok(false);
    };
    if disabled {
        sqlx::query(
            "UPDATE auth_sessions SET revoked_at = now() WHERE user_id = $1 AND revoked_at IS NULL",
        )
        .bind(id)
        .execute(&mut *tx)
        .await
        .context("撤销会话失败")?;
    }
    tx.commit().await.context("提交事务失败")?;
    Ok(true)
}

/// 未删除用户当前的密码哈希（修改密码时校验当前密码）。
pub async fn password_hash(pool: &PgPool, user_id: i64) -> Result<Option<String>> {
    sqlx::query_scalar("SELECT password_hash FROM users WHERE id = $1 AND deleted_at IS NULL")
        .bind(user_id)
        .fetch_optional(pool)
        .await
        .context("查询密码哈希失败")
}

/// 改密码：同一个事务内更新哈希并撤销该用户的全部登录会话（调用方随后签发一个新会话）。
/// 返回是否找到了这个用户。
pub async fn change_password(pool: &PgPool, user_id: i64, new_hash: &str) -> Result<bool> {
    let mut tx = pool.begin().await.context("开启事务失败")?;
    let n = sqlx::query("UPDATE users SET password_hash = $2 WHERE id = $1 AND deleted_at IS NULL")
        .bind(user_id)
        .bind(new_hash)
        .execute(&mut *tx)
        .await
        .context("更新 users.password_hash 失败")?
        .rows_affected();
    if n == 0 {
        return Ok(false);
    }
    sqlx::query(
        "UPDATE auth_sessions SET revoked_at = now() WHERE user_id = $1 AND revoked_at IS NULL",
    )
    .bind(user_id)
    .execute(&mut *tx)
    .await
    .context("撤销会话失败")?;
    tx.commit().await.context("提交事务失败")?;
    Ok(true)
}

/// `rivu user list` 的一行（不含密码哈希）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserListRow {
    pub id: i64,
    pub email: String,
    pub name: Option<String>,
    pub is_instance_admin: bool,
    pub disabled: bool,
    /// UTC ISO 文本。
    pub created_at: String,
    /// 所属组织：`slug:role`。
    pub memberships: Vec<String>,
}

/// 列出未删除的用户，按 id 升序。
pub async fn list_users(pool: &PgPool) -> Result<Vec<UserListRow>> {
    let rows = sqlx::query(
        r#"SELECT u.id, u.email::text AS email, u.name, u.is_instance_admin,
                  (u.disabled_at IS NOT NULL) AS disabled,
                  to_char(u.created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS"Z"') AS created_at,
                  COALESCE(array_agg(o.slug || ':' || m.role ORDER BY o.slug)
                           FILTER (WHERE m.id IS NOT NULL AND o.id IS NOT NULL), '{}') AS memberships
           FROM users u
           LEFT JOIN memberships m ON m.user_id = u.id AND m.deleted_at IS NULL
           LEFT JOIN organizations o ON o.id = m.organization_id AND o.deleted_at IS NULL
           WHERE u.deleted_at IS NULL
           GROUP BY u.id ORDER BY u.id"#,
    )
    .fetch_all(pool)
    .await
    .context("列出用户失败")?;
    rows.iter()
        .map(|r| {
            Ok(UserListRow {
                id: r.try_get("id")?,
                email: r.try_get("email")?,
                name: r.try_get("name")?,
                is_instance_admin: r.try_get("is_instance_admin")?,
                disabled: r.try_get("disabled")?,
                created_at: r.try_get("created_at")?,
                memberships: r.try_get("memberships")?,
            })
        })
        .collect()
}

/// 建组织（`rivu org create`）。slug 已被未删除的组织使用则报错。
pub async fn create_org(pool: &PgPool, name: &str, slug: &str) -> Result<i64> {
    let id: Option<i64> = sqlx::query_scalar(
        "INSERT INTO organizations (name, slug) VALUES ($1, $2)
         ON CONFLICT (lower(slug)) WHERE deleted_at IS NULL DO NOTHING RETURNING id",
    )
    .bind(name)
    .bind(slug)
    .fetch_optional(pool)
    .await
    .context("写入 organizations 失败")?;
    id.with_context(|| format!("组织 slug `{slug}` 已被使用"))
}

/// 把用户加入组织，或改他在该组织里的角色（`rivu org member add`）。
/// 状态规则（`platform-and-ops.md` §4.1）：组织至少保留一名 owner——把最后一名 owner 降级会被拒绝。
/// 返回 `(旧角色, 新角色)`。
pub async fn upsert_member(
    pool: &PgPool,
    org_id: i64,
    user_id: i64,
    role: &str,
) -> Result<(Option<String>, String)> {
    let mut tx = pool.begin().await.context("开启事务失败")?;
    // 锁组织行，再数 owner（§4.1）
    sqlx::query("SELECT id FROM organizations WHERE id = $1 AND deleted_at IS NULL FOR UPDATE")
        .bind(org_id)
        .fetch_optional(&mut *tx)
        .await
        .context("锁定组织失败")?
        .context("组织不存在或已删除")?;
    let old: Option<String> = sqlx::query_scalar(
        "SELECT role FROM memberships WHERE organization_id = $1 AND user_id = $2 AND deleted_at IS NULL",
    )
    .bind(org_id)
    .bind(user_id)
    .fetch_optional(&mut *tx)
    .await
    .context("查询成员关系失败")?;
    if old.as_deref() == Some("owner") && role != "owner" {
        let owners: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM memberships WHERE organization_id = $1 AND role = 'owner' AND deleted_at IS NULL",
        )
        .bind(org_id)
        .fetch_one(&mut *tx)
        .await
        .context("统计 owner 失败")?;
        if owners <= 1 {
            bail!("组织至少要保留一名 owner：不能降级最后一名 owner");
        }
    }
    sqlx::query(
        "INSERT INTO memberships (organization_id, user_id, role) VALUES ($1, $2, $3)
         ON CONFLICT (organization_id, user_id) WHERE deleted_at IS NULL DO UPDATE SET role = EXCLUDED.role",
    )
    .bind(org_id)
    .bind(user_id)
    .bind(role)
    .execute(&mut *tx)
    .await
    .context("写入 memberships 失败")?;
    tx.commit().await.context("提交事务失败")?;
    Ok((old, role.to_owned()))
}

/// 用户所属的组织（未删除的成员关系与组织）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserOrg {
    pub id: i64,
    pub name: String,
    pub slug: String,
    pub role: String,
}

pub async fn list_user_orgs(pool: &PgPool, user_id: i64) -> Result<Vec<UserOrg>> {
    let rows = sqlx::query(
        "SELECT o.id, o.name, o.slug, m.role
         FROM memberships m JOIN organizations o ON o.id = m.organization_id AND o.deleted_at IS NULL
         WHERE m.user_id = $1 AND m.deleted_at IS NULL
         ORDER BY o.name, o.id",
    )
    .bind(user_id)
    .fetch_all(pool)
    .await
    .context("查询用户的组织失败")?;
    rows.iter()
        .map(|r| {
            Ok(UserOrg {
                id: r.try_get("id")?,
                name: r.try_get("name")?,
                slug: r.try_get("slug")?,
                role: r.try_get("role")?,
            })
        })
        .collect()
}

/// 首次初始化的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetupDone {
    pub user_id: i64,
    pub org_id: i64,
    pub org_slug: String,
}

/// 首次初始化（`platform-and-ops.md` §5「首个用户」）：`users` 为空时，在同一个事务里持 advisory lock 创建
/// 首个用户（`is_instance_admin`）、首个组织与 owner 成员关系。`users` 里有任何一行（含已删除、已禁用的）都算已初始化，
/// 返回 `None`——并发的两次初始化，后到的在锁上等待，然后看到用户已存在。
pub async fn create_first_user(
    pool: &PgPool,
    email: &str,
    name: Option<&str>,
    password_hash: &str,
    org_name: &str,
    org_slug_base: &str,
) -> Result<Option<SetupDone>> {
    let mut tx = pool.begin().await.context("开启事务失败")?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtext('rivu:setup'))")
        .execute(&mut *tx)
        .await
        .context("获取 setup 的 advisory lock 失败")?;
    let any: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM users)")
        .fetch_one(&mut *tx)
        .await
        .context("检查 users 是否为空失败")?;
    if any {
        return Ok(None);
    }
    let user_id: i64 = sqlx::query_scalar(
        "INSERT INTO users (email, password_hash, name, is_instance_admin)
         VALUES (($1::text)::citext, $2, $3, true) RETURNING id",
    )
    .bind(email)
    .bind(password_hash)
    .bind(name)
    .fetch_one(&mut *tx)
    .await
    .context("写入首个用户失败")?;

    let mut org: Option<(i64, String)> = None;
    for attempt in 0..8 {
        let slug = if attempt == 0 {
            org_slug_base.to_owned()
        } else {
            format!("{org_slug_base}-{:04x}", rand::random::<u16>())
        };
        let id: Option<i64> = sqlx::query_scalar(
            "INSERT INTO organizations (name, slug) VALUES ($1, $2)
             ON CONFLICT (lower(slug)) WHERE deleted_at IS NULL DO NOTHING RETURNING id",
        )
        .bind(org_name)
        .bind(&slug)
        .fetch_optional(&mut *tx)
        .await
        .context("写入首个组织失败")?;
        if let Some(id) = id {
            org = Some((id, slug));
            break;
        }
    }
    let (org_id, org_slug) = org.context("连续 8 次生成的组织 slug 都已存在")?;
    sqlx::query(
        "INSERT INTO memberships (organization_id, user_id, role) VALUES ($1, $2, 'owner')",
    )
    .bind(org_id)
    .bind(user_id)
    .execute(&mut *tx)
    .await
    .context("写入首个 owner 失败")?;
    tx.commit().await.context("提交事务失败")?;
    Ok(Some(SetupDone {
        user_id,
        org_id,
        org_slug,
    }))
}

/// `users` 里是否已有任何一行（含已删除、已禁用的）。
pub async fn any_user_exists(pool: &PgPool) -> Result<bool> {
    sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM users)")
        .fetch_one(pool)
        .await
        .context("检查 users 是否为空失败")
}
