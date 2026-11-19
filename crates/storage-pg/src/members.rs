//! 成员、邀请与站点访客（`memberships`、`invitations`、`site_memberships`；看板的成员管理端点与邀请接受页）。
//!
//! 规则（`platform-and-ops.md` §4.1）在事务里落实：
//! - 改角色与移除成员先 `SELECT ... FOR UPDATE` 锁组织行，再读两边的当前角色、数 owner：组织至少保留一名 owner；
//!   「谁能改谁」的判定（`rivu_auth::scope::can_assign`、`can_modify`）由调用方以闭包传入，
//!   这样判定用的是锁内读到的最新角色，而不是请求开始时的快照；
//! - 移除成员的同一个事务里软删除他在该组织的 API key（否则重新加回他会让旧 key 复活）；
//! - 建邀请先软删除同一 `(组织, 邮箱, 范围)` 下已过期的待处理邀请（否则部分唯一索引 `invitations_pending` 会拦住过期后的重新邀请）；
//! - 接受邀请锁邀请行（按令牌哈希），在同一事务里建用户（如需）、写成员关系并标记已接受。
//!
//! 邀请令牌只以 SHA-256 存库（`token_hash`）。时间以 UTC ISO 文本（秒精度）读出。

use anyhow::{Context, Result};
use sqlx::{PgPool, Row};

// ------------------------------------------------------------------------------------------
// 成员
// ------------------------------------------------------------------------------------------

/// 用户在组织里的角色（未删除的成员关系与组织）；不是成员返回 `None`。
pub async fn member_role(pool: &PgPool, org_id: i64, user_id: i64) -> Result<Option<String>> {
    sqlx::query_scalar(
        "SELECT m.role
         FROM memberships m
         JOIN organizations o ON o.id = m.organization_id AND o.deleted_at IS NULL
         WHERE m.organization_id = $1 AND m.user_id = $2 AND m.deleted_at IS NULL",
    )
    .bind(org_id)
    .bind(user_id)
    .fetch_optional(pool)
    .await
    .context("查询成员角色失败")
}

/// 成员列表的一行。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemberRow {
    pub user_id: i64,
    pub name: Option<String>,
    pub email: String,
    pub role: String,
    pub joined_at: String,
    /// 该用户全部会话的 `last_used_at` 最大值。
    pub last_active_at: Option<String>,
}

/// 成员（未删除的用户），owner → admin → editor → viewer，同角色按名称（没有名称用邮箱）排。
pub async fn list_members(pool: &PgPool, org_id: i64) -> Result<Vec<MemberRow>> {
    let rows = sqlx::query(concat!(
        "SELECT u.id AS user_id, u.name, u.email::text AS email, m.role, ",
        ts_sec!("m.created_at"),
        " AS joined_at, ",
        ts_sec!("(SELECT max(s.last_used_at) FROM auth_sessions s WHERE s.user_id = u.id)"),
        " AS last_active_at
         FROM memberships m
         JOIN users u ON u.id = m.user_id AND u.deleted_at IS NULL
         WHERE m.organization_id = $1 AND m.deleted_at IS NULL
         ORDER BY CASE m.role WHEN 'owner' THEN 0 WHEN 'admin' THEN 1 WHEN 'editor' THEN 2 ELSE 3 END,
                  lower(COALESCE(u.name, u.email::text)), u.id"
    ))
    .bind(org_id)
    .fetch_all(pool)
    .await
    .context("列出成员失败")?;
    rows.iter().map(member_from_row).collect()
}

fn member_from_row(r: &sqlx::postgres::PgRow) -> Result<MemberRow> {
    Ok(MemberRow {
        user_id: r.try_get("user_id")?,
        name: r.try_get("name")?,
        email: r.try_get("email")?,
        role: r.try_get("role")?,
        joined_at: r.try_get("joined_at")?,
        last_active_at: r.try_get("last_active_at")?,
    })
}

/// 一个成员（改角色的响应用）。
pub async fn get_member(pool: &PgPool, org_id: i64, user_id: i64) -> Result<Option<MemberRow>> {
    let row = sqlx::query(concat!(
        "SELECT u.id AS user_id, u.name, u.email::text AS email, m.role, ",
        ts_sec!("m.created_at"),
        " AS joined_at, ",
        ts_sec!("(SELECT max(s.last_used_at) FROM auth_sessions s WHERE s.user_id = u.id)"),
        " AS last_active_at
         FROM memberships m
         JOIN users u ON u.id = m.user_id AND u.deleted_at IS NULL
         WHERE m.organization_id = $1 AND m.user_id = $2 AND m.deleted_at IS NULL"
    ))
    .bind(org_id)
    .bind(user_id)
    .fetch_optional(pool)
    .await
    .context("查询成员失败")?;
    row.as_ref().map(member_from_row).transpose()
}

/// 改角色、移除成员的共同结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemberChange {
    /// 已生效。`keys_revoked` 只在移除时有意义（被软删除的 API key 数）。
    Done { keys_revoked: u64 },
    /// 组织不存在，或调用者、目标不是它的成员。
    NotMember,
    /// 判定闭包拒绝（角色不够）。
    Denied,
    /// 会让组织没有 owner。
    LastOwner,
}

/// 锁组织行，读调用者与目标的当前角色。`Err(outcome)` = 已经能给出结论。
async fn lock_and_roles(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    org_id: i64,
    actor_id: i64,
    target_id: i64,
) -> Result<Result<(String, String), MemberChange>> {
    let locked =
        sqlx::query("SELECT id FROM organizations WHERE id = $1 AND deleted_at IS NULL FOR UPDATE")
            .bind(org_id)
            .fetch_optional(&mut **tx)
            .await
            .context("锁定组织失败")?;
    if locked.is_none() {
        return Ok(Err(MemberChange::NotMember));
    }
    let role_of = |user_id: i64| {
        sqlx::query_scalar::<_, String>(
            "SELECT role FROM memberships WHERE organization_id = $1 AND user_id = $2 AND deleted_at IS NULL",
        )
        .bind(org_id)
        .bind(user_id)
    };
    let Some(actor) = role_of(actor_id)
        .fetch_optional(&mut **tx)
        .await
        .context("查询调用者角色失败")?
    else {
        return Ok(Err(MemberChange::NotMember));
    };
    let Some(target) = role_of(target_id)
        .fetch_optional(&mut **tx)
        .await
        .context("查询成员角色失败")?
    else {
        return Ok(Err(MemberChange::NotMember));
    };
    Ok(Ok((actor, target)))
}

/// 组织里 owner 的数量（调用时已持有组织行锁）。
async fn count_owners(tx: &mut sqlx::Transaction<'_, sqlx::Postgres>, org_id: i64) -> Result<i64> {
    sqlx::query_scalar(
        "SELECT count(*) FROM memberships WHERE organization_id = $1 AND role = 'owner' AND deleted_at IS NULL",
    )
    .bind(org_id)
    .fetch_one(&mut **tx)
    .await
    .context("统计 owner 失败")
}

/// 改成员角色。`allow(调用者当前角色, 目标当前角色)` 是锁内的权限判定（`can_modify` 加 `can_assign`）。
pub async fn change_role(
    pool: &PgPool,
    org_id: i64,
    actor_id: i64,
    target_id: i64,
    new_role: &str,
    allow: impl FnOnce(&str, &str) -> bool,
) -> Result<MemberChange> {
    let mut tx = pool.begin().await.context("开启事务失败")?;
    let (actor, target) = match lock_and_roles(&mut tx, org_id, actor_id, target_id).await? {
        Ok(roles) => roles,
        Err(done) => return Ok(done),
    };
    if !allow(&actor, &target) {
        return Ok(MemberChange::Denied);
    }
    if target == "owner" && new_role != "owner" && count_owners(&mut tx, org_id).await? <= 1 {
        return Ok(MemberChange::LastOwner);
    }
    sqlx::query(
        "UPDATE memberships SET role = $3
         WHERE organization_id = $1 AND user_id = $2 AND deleted_at IS NULL",
    )
    .bind(org_id)
    .bind(target_id)
    .bind(new_role)
    .execute(&mut *tx)
    .await
    .context("更新成员角色失败")?;
    tx.commit().await.context("提交事务失败")?;
    Ok(MemberChange::Done { keys_revoked: 0 })
}

/// 移除成员（软删除）并在同一个事务里软删除他在这个组织的 API key。`allow` 同 [`change_role`]。
pub async fn remove_member(
    pool: &PgPool,
    org_id: i64,
    actor_id: i64,
    target_id: i64,
    allow: impl FnOnce(&str, &str) -> bool,
) -> Result<MemberChange> {
    let mut tx = pool.begin().await.context("开启事务失败")?;
    let (actor, target) = match lock_and_roles(&mut tx, org_id, actor_id, target_id).await? {
        Ok(roles) => roles,
        Err(done) => return Ok(done),
    };
    if !allow(&actor, &target) {
        return Ok(MemberChange::Denied);
    }
    if target == "owner" && count_owners(&mut tx, org_id).await? <= 1 {
        return Ok(MemberChange::LastOwner);
    }
    sqlx::query(
        "UPDATE memberships SET deleted_at = now()
         WHERE organization_id = $1 AND user_id = $2 AND deleted_at IS NULL",
    )
    .bind(org_id)
    .bind(target_id)
    .execute(&mut *tx)
    .await
    .context("移除成员失败")?;
    let keys_revoked = sqlx::query(
        "UPDATE api_keys SET deleted_at = now()
         WHERE organization_id = $1 AND user_id = $2 AND deleted_at IS NULL",
    )
    .bind(org_id)
    .bind(target_id)
    .execute(&mut *tx)
    .await
    .context("吊销成员的 API key 失败")?
    .rows_affected();
    sqlx::query(
        "DELETE FROM weekly_report_subscriptions w USING sites s
         WHERE w.site_id = s.id AND w.user_id = $2 AND s.organization_id = $1
           AND NOT EXISTS (SELECT FROM site_memberships g
               WHERE g.site_id = w.site_id AND g.user_id = w.user_id AND g.deleted_at IS NULL)",
    )
    .bind(org_id)
    .bind(target_id)
    .execute(&mut *tx)
    .await
    .context("清理成员周报订阅失败")?;
    tx.commit().await.context("提交事务失败")?;
    Ok(MemberChange::Done { keys_revoked })
}

// ------------------------------------------------------------------------------------------
// 邀请
// ------------------------------------------------------------------------------------------

/// 待处理邀请（组织邀请与站点访客邀请）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvitationRow {
    pub id: i64,
    pub email: String,
    pub role: Option<String>,
    /// 访客邀请的站点 `(id, 名称)`。
    pub site: Option<(i64, String)>,
    pub site_role: Option<String>,
    pub expires_at: String,
    /// 邀请人的显示名（没有名称为 `None`）。
    pub invited_by: Option<String>,
}

const INVITATION_COLUMNS: &str = concat!(
    "i.id, i.email::text AS email, i.role, i.site_id, s.name AS site_name, i.site_role, ",
    ts_sec!("i.expires_at"),
    " AS expires_at, u.name AS inviter"
);

fn invitation_from_row(r: &sqlx::postgres::PgRow) -> Result<InvitationRow> {
    let site_id: Option<i64> = r.try_get("site_id")?;
    let site_name: Option<String> = r.try_get("site_name")?;
    Ok(InvitationRow {
        id: r.try_get("id")?,
        email: r.try_get("email")?,
        role: r.try_get("role")?,
        site: site_id.zip(site_name),
        site_role: r.try_get("site_role")?,
        expires_at: r.try_get("expires_at")?,
        invited_by: r.try_get("inviter")?,
    })
}

/// 待处理的邀请：未接受、未撤销、未过期，访客邀请的站点仍存在。最早的在前。
pub async fn list_pending_invitations(pool: &PgPool, org_id: i64) -> Result<Vec<InvitationRow>> {
    let sql = format!(
        "SELECT {INVITATION_COLUMNS}
         FROM invitations i
         LEFT JOIN sites s ON s.id = i.site_id AND s.deleted_at IS NULL
         LEFT JOIN users u ON u.id = i.invited_by
         WHERE i.organization_id = $1 AND i.accepted_at IS NULL AND i.deleted_at IS NULL
           AND i.expires_at > now() AND (i.site_id IS NULL OR s.id IS NOT NULL)
         ORDER BY i.id"
    );
    let rows = sqlx::query(sqlx::AssertSqlSafe(sql))
        .bind(org_id)
        .fetch_all(pool)
        .await
        .context("列出邀请失败")?;
    rows.iter().map(invitation_from_row).collect()
}

/// 一个待处理的邀请（创建邀请的响应用）。
pub async fn get_pending_invitation(
    pool: &PgPool,
    org_id: i64,
    id: i64,
) -> Result<Option<InvitationRow>> {
    let sql = format!(
        "SELECT {INVITATION_COLUMNS}
         FROM invitations i
         LEFT JOIN sites s ON s.id = i.site_id AND s.deleted_at IS NULL
         LEFT JOIN users u ON u.id = i.invited_by
         WHERE i.organization_id = $1 AND i.id = $2 AND i.accepted_at IS NULL AND i.deleted_at IS NULL
           AND i.expires_at > now()"
    );
    let row = sqlx::query(sqlx::AssertSqlSafe(sql))
        .bind(org_id)
        .bind(id)
        .fetch_optional(pool)
        .await
        .context("查询邀请失败")?;
    row.as_ref().map(invitation_from_row).transpose()
}

/// 新邀请的参数。组织邀请：`role` 有值、`site` 为空；访客邀请：`site = (站点 id, 访客角色)`、`role` 为空。
#[derive(Debug, Clone)]
pub struct NewInvitation<'a> {
    pub org_id: i64,
    /// 已规范化（小写）的邮箱。
    pub email: &'a str,
    pub role: Option<&'a str>,
    pub site: Option<(i64, &'a str)>,
    pub token_hash: [u8; 32],
    pub invited_by: i64,
    /// 有效期（秒，从现在起）。
    pub ttl_secs: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InviteOutcome {
    Created(i64),
    /// 这个邮箱已经是组织成员。
    AlreadyMember,
    /// 这个邮箱已经是该站点的访客。
    AlreadyGuest,
    /// 同一范围已有未过期的待处理邀请。
    Pending,
    /// 站点不属于这个组织，或已删除。
    SiteNotFound,
}

/// 建邀请：同一事务里先清掉同范围已过期的待处理邀请，再插入（并发的重复邀请由部分唯一索引兜底，返回 `Pending`）。
pub async fn create_invitation(pool: &PgPool, new: &NewInvitation<'_>) -> Result<InviteOutcome> {
    let mut tx = pool.begin().await.context("开启事务失败")?;
    let site_id = new.site.map(|(id, _)| id);
    if let Some(site_id) = site_id {
        let found: Option<i32> = sqlx::query_scalar(
            "SELECT 1 FROM sites WHERE id = $1 AND organization_id = $2 AND deleted_at IS NULL",
        )
        .bind(site_id)
        .bind(new.org_id)
        .fetch_optional(&mut *tx)
        .await
        .context("查询站点失败")?;
        if found.is_none() {
            return Ok(InviteOutcome::SiteNotFound);
        }
    }
    let member: bool = sqlx::query_scalar(
        "SELECT EXISTS (
             SELECT 1 FROM memberships m JOIN users u ON u.id = m.user_id AND u.deleted_at IS NULL
             WHERE m.organization_id = $1 AND m.deleted_at IS NULL AND u.email = ($2::text)::citext)",
    )
    .bind(new.org_id)
    .bind(new.email)
    .fetch_one(&mut *tx)
    .await
    .context("查询成员失败")?;
    if member {
        return Ok(InviteOutcome::AlreadyMember);
    }
    if let Some(site_id) = site_id {
        let guest: bool = sqlx::query_scalar(
            "SELECT EXISTS (
                 SELECT 1 FROM site_memberships g JOIN users u ON u.id = g.user_id AND u.deleted_at IS NULL
                 WHERE g.site_id = $1 AND g.deleted_at IS NULL AND u.email = ($2::text)::citext)",
        )
        .bind(site_id)
        .bind(new.email)
        .fetch_one(&mut *tx)
        .await
        .context("查询站点访客失败")?;
        if guest {
            return Ok(InviteOutcome::AlreadyGuest);
        }
    }
    sqlx::query(
        "UPDATE invitations SET deleted_at = now()
         WHERE organization_id = $1 AND email = ($2::text)::citext AND COALESCE(site_id, 0) = COALESCE($3::bigint, 0)
           AND accepted_at IS NULL AND deleted_at IS NULL AND expires_at <= now()",
    )
    .bind(new.org_id)
    .bind(new.email)
    .bind(site_id)
    .execute(&mut *tx)
    .await
    .context("清理过期邀请失败")?;
    let id: Option<i64> = sqlx::query_scalar(
        "INSERT INTO invitations (organization_id, email, role, site_id, site_role, token_hash, invited_by, expires_at)
         VALUES ($1, ($2::text)::citext, $3, $4, $5, $6, $7, now() + ($8::bigint * interval '1 second'))
         ON CONFLICT (organization_id, email, COALESCE(site_id, 0))
             WHERE accepted_at IS NULL AND deleted_at IS NULL DO NOTHING
         RETURNING id",
    )
    .bind(new.org_id)
    .bind(new.email)
    .bind(new.role)
    .bind(site_id)
    .bind(new.site.map(|(_, r)| r))
    .bind(new.token_hash.as_slice())
    .bind(new.invited_by)
    .bind(new.ttl_secs)
    .fetch_optional(&mut *tx)
    .await
    .context("写入 invitations 失败")?;
    let Some(id) = id else {
        return Ok(InviteOutcome::Pending);
    };
    tx.commit().await.context("提交事务失败")?;
    Ok(InviteOutcome::Created(id))
}

/// 撤销待处理的邀请（软删除）；不是这个组织的、已接受、已撤销或已过期的返回 `false`。
pub async fn revoke_invitation(pool: &PgPool, org_id: i64, id: i64) -> Result<bool> {
    let n = sqlx::query(
        "UPDATE invitations SET deleted_at = now()
         WHERE id = $1 AND organization_id = $2 AND accepted_at IS NULL AND deleted_at IS NULL
           AND expires_at > now()",
    )
    .bind(id)
    .bind(org_id)
    .execute(pool)
    .await
    .context("撤销邀请失败")?
    .rows_affected();
    Ok(n > 0)
}

// ------------------------------------------------------------------------------------------
// 站点访客
// ------------------------------------------------------------------------------------------

/// 访客列表的一行。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuestRow {
    /// `site_memberships.id`。
    pub id: i64,
    pub user_id: i64,
    pub name: Option<String>,
    pub email: String,
    pub site_id: i64,
    pub site_name: String,
    pub role: String,
}

/// 组织站点（未删除）上的访客（用户未删除），按站点名、邮箱排。
pub async fn list_guests(pool: &PgPool, org_id: i64) -> Result<Vec<GuestRow>> {
    let rows = sqlx::query(
        "SELECT g.id, g.user_id, u.name, u.email::text AS email, s.id AS site_id, s.name AS site_name, g.role
         FROM site_memberships g
         JOIN sites s ON s.id = g.site_id AND s.deleted_at IS NULL AND s.organization_id = $1
         JOIN users u ON u.id = g.user_id AND u.deleted_at IS NULL
         WHERE g.deleted_at IS NULL
         ORDER BY lower(s.name), s.id, u.email::text, g.id",
    )
    .bind(org_id)
    .fetch_all(pool)
    .await
    .context("列出站点访客失败")?;
    rows.iter()
        .map(|r| {
            Ok(GuestRow {
                id: r.try_get("id")?,
                user_id: r.try_get("user_id")?,
                name: r.try_get("name")?,
                email: r.try_get("email")?,
                site_id: r.try_get("site_id")?,
                site_name: r.try_get("site_name")?,
                role: r.try_get("role")?,
            })
        })
        .collect()
}

/// 移除站点访客（软删除）；必须是这个组织未删除站点上的有效访客关系。
pub async fn remove_guest(pool: &PgPool, org_id: i64, id: i64) -> Result<bool> {
    let mut tx = pool.begin().await?;
    sqlx::query("SELECT id FROM organizations WHERE id = $1 FOR UPDATE")
        .bind(org_id)
        .execute(&mut *tx)
        .await?;
    let revoked: Option<(i64, i64)> = sqlx::query_as(
        "UPDATE site_memberships g SET deleted_at = now()
         FROM sites s
         WHERE g.id = $1 AND g.deleted_at IS NULL
           AND s.id = g.site_id AND s.organization_id = $2 AND s.deleted_at IS NULL
         RETURNING g.user_id, g.site_id",
    )
    .bind(id)
    .bind(org_id)
    .fetch_optional(&mut *tx)
    .await
    .context("移除站点访客失败")?;
    let Some((user_id, site_id)) = revoked else {
        return Ok(false);
    };
    sqlx::query(
        "DELETE FROM weekly_report_subscriptions w WHERE user_id = $1 AND site_id = $2
           AND NOT EXISTS (SELECT FROM memberships m
               WHERE m.organization_id = $3 AND m.user_id = $1 AND m.deleted_at IS NULL)",
    )
    .bind(user_id)
    .bind(site_id)
    .bind(org_id)
    .execute(&mut *tx)
    .await
    .context("清理站点访客周报订阅失败")?;
    tx.commit().await?;
    Ok(true)
}

// ------------------------------------------------------------------------------------------
// 接受邀请
// ------------------------------------------------------------------------------------------

/// 邀请页展示的信息。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InviteInfo {
    pub email: String,
    pub org_id: i64,
    pub org_name: String,
    pub role: Option<String>,
    pub site: Option<(i64, String)>,
    pub site_role: Option<String>,
    pub inviter: Option<String>,
    pub expires_at: String,
    /// 邮箱已经有（未删除的）账号。
    pub account_exists: bool,
}

/// 按令牌摘要查待处理的邀请：未接受、未撤销、未过期，组织与（访客邀请的）站点未删除。
pub async fn find_pending_by_token(pool: &PgPool, hash: &[u8; 32]) -> Result<Option<InviteInfo>> {
    let row = sqlx::query(concat!(
        "SELECT i.email::text AS email, i.organization_id, o.name AS org_name, i.role, i.site_id,
                s.name AS site_name, i.site_role, u.name AS inviter, ",
        ts_sec!("i.expires_at"),
        " AS expires_at,
                EXISTS (SELECT 1 FROM users x WHERE x.email = i.email AND x.deleted_at IS NULL) AS account_exists
         FROM invitations i
         JOIN organizations o ON o.id = i.organization_id AND o.deleted_at IS NULL
         LEFT JOIN sites s ON s.id = i.site_id AND s.deleted_at IS NULL
         LEFT JOIN users u ON u.id = i.invited_by
         WHERE i.token_hash = $1 AND i.accepted_at IS NULL AND i.deleted_at IS NULL
           AND i.expires_at > now() AND (i.site_id IS NULL OR s.id IS NOT NULL)"
    ))
    .bind(hash.as_slice())
    .fetch_optional(pool)
    .await
    .context("查询邀请失败")?;
    row.map(|r| {
        let site_id: Option<i64> = r.try_get("site_id")?;
        let site_name: Option<String> = r.try_get("site_name")?;
        Ok(InviteInfo {
            email: r.try_get("email")?,
            org_id: r.try_get("organization_id")?,
            org_name: r.try_get("org_name")?,
            role: r.try_get("role")?,
            site: site_id.zip(site_name),
            site_role: r.try_get("site_role")?,
            inviter: r.try_get("inviter")?,
            expires_at: r.try_get("expires_at")?,
            account_exists: r.try_get("account_exists")?,
        })
    })
    .transpose()
}

/// 没有账号的受邀者要创建的账号。
#[derive(Debug, Clone)]
pub struct NewAccount<'a> {
    pub name: Option<&'a str>,
    /// argon2id 的 PHC 字符串。
    pub password_hash: &'a str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcceptOutcome {
    Accepted {
        user_id: i64,
        org_id: i64,
        site_id: Option<i64>,
        /// 本次创建了账号（调用方要为他登录）。
        created_account: bool,
    },
    /// 邀请无效：不存在、已接受、已撤销、已过期、组织或站点已删除，或账号被禁用。
    NotFound,
    /// 邮箱已有账号，但请求没有登录会话。
    NeedSession,
    /// 邮箱已有账号，但登录的是另一个用户。
    EmailMismatch,
    /// 邮箱没有账号而请求没给创建账号用的信息。
    NeedAccount,
    /// 创建账号时邮箱已被别人抢先注册。
    EmailTaken,
    /// 已经是成员（或这个站点的访客）。
    AlreadyMember,
}

/// 接受邀请。一个事务：锁邀请行 → 判定账号（已有账号必须是 `session_user_id` 本人；没有账号则用 `account` 建一个）→
/// 写成员关系或访客关系 → 标记已接受。任何一步失败都整体回滚，邀请仍然待处理。
pub async fn accept_invitation(
    pool: &PgPool,
    hash: &[u8; 32],
    session_user_id: Option<i64>,
    account: Option<&NewAccount<'_>>,
) -> Result<AcceptOutcome> {
    let mut tx = pool.begin().await.context("开启事务失败")?;
    let inv = sqlx::query(
        "SELECT i.id, i.organization_id, i.email::text AS email, i.role, i.site_id, i.site_role
         FROM invitations i
         WHERE i.token_hash = $1 AND i.accepted_at IS NULL AND i.deleted_at IS NULL AND i.expires_at > now()
         FOR UPDATE OF i",
    )
    .bind(hash.as_slice())
    .fetch_optional(&mut *tx)
    .await
    .context("锁定邀请失败")?;
    let Some(inv) = inv else {
        return Ok(AcceptOutcome::NotFound);
    };
    let inv_id: i64 = inv.try_get("id")?;
    let org_id: i64 = inv.try_get("organization_id")?;
    let email: String = inv.try_get("email")?;
    let role: Option<String> = inv.try_get("role")?;
    let site_id: Option<i64> = inv.try_get("site_id")?;
    let site_role: Option<String> = inv.try_get("site_role")?;

    let org_alive: Option<i32> =
        sqlx::query_scalar("SELECT 1 FROM organizations WHERE id = $1 AND deleted_at IS NULL")
            .bind(org_id)
            .fetch_optional(&mut *tx)
            .await
            .context("查询组织失败")?;
    let site_alive = match site_id {
        None => true,
        Some(site_id) => sqlx::query_scalar::<_, i32>(
            "SELECT 1 FROM sites WHERE id = $1 AND organization_id = $2 AND deleted_at IS NULL",
        )
        .bind(site_id)
        .bind(org_id)
        .fetch_optional(&mut *tx)
        .await
        .context("查询站点失败")?
        .is_some(),
    };
    if org_alive.is_none() || !site_alive {
        return Ok(AcceptOutcome::NotFound);
    }

    let existing: Option<(i64, bool)> = sqlx::query_as(
        "SELECT id, (disabled_at IS NOT NULL) FROM users WHERE email = ($1::text)::citext AND deleted_at IS NULL",
    )
    .bind(&email)
    .fetch_optional(&mut *tx)
    .await
    .context("查询用户失败")?;
    let (user_id, created_account) = match existing {
        Some((_, true)) => return Ok(AcceptOutcome::NotFound),
        Some((id, false)) => match session_user_id {
            None => return Ok(AcceptOutcome::NeedSession),
            Some(s) if s != id => return Ok(AcceptOutcome::EmailMismatch),
            Some(_) => (id, false),
        },
        None => {
            let Some(account) = account else {
                return Ok(AcceptOutcome::NeedAccount);
            };
            let id: Option<i64> = sqlx::query_scalar(
                "INSERT INTO users (email, password_hash, name, is_instance_admin)
                 VALUES (($1::text)::citext, $2, $3, false)
                 ON CONFLICT (email) WHERE deleted_at IS NULL DO NOTHING
                 RETURNING id",
            )
            .bind(&email)
            .bind(account.password_hash)
            .bind(account.name)
            .fetch_optional(&mut *tx)
            .await
            .context("写入 users 失败")?;
            match id {
                Some(id) => (id, true),
                None => return Ok(AcceptOutcome::EmailTaken),
            }
        }
    };

    let inserted: Option<i64> = match (site_id, role.as_deref(), site_role.as_deref()) {
        (None, Some(role), _) => sqlx::query_scalar(
            "INSERT INTO memberships (organization_id, user_id, role) VALUES ($1, $2, $3)
             ON CONFLICT (organization_id, user_id) WHERE deleted_at IS NULL DO NOTHING
             RETURNING id",
        )
        .bind(org_id)
        .bind(user_id)
        .bind(role)
        .fetch_optional(&mut *tx)
        .await
        .context("写入 memberships 失败")?,
        (Some(site_id), _, Some(site_role)) => {
            let member: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM memberships
                                WHERE organization_id = $1 AND user_id = $2 AND deleted_at IS NULL)",
            )
            .bind(org_id)
            .bind(user_id)
            .fetch_one(&mut *tx)
            .await
            .context("查询成员关系失败")?;
            if member {
                None
            } else {
                sqlx::query_scalar(
                    "INSERT INTO site_memberships (site_id, user_id, role) VALUES ($1, $2, $3)
                     ON CONFLICT (site_id, user_id) WHERE deleted_at IS NULL DO NOTHING
                     RETURNING id",
                )
                .bind(site_id)
                .bind(user_id)
                .bind(site_role)
                .fetch_optional(&mut *tx)
                .await
                .context("写入 site_memberships 失败")?
            }
        }
        // 表的 CHECK 保证两种形状之一
        _ => return Ok(AcceptOutcome::NotFound),
    };
    if inserted.is_none() {
        return Ok(AcceptOutcome::AlreadyMember);
    }
    sqlx::query("UPDATE invitations SET accepted_at = now() WHERE id = $1")
        .bind(inv_id)
        .execute(&mut *tx)
        .await
        .context("标记邀请已接受失败")?;
    tx.commit().await.context("提交事务失败")?;
    Ok(AcceptOutcome::Accepted {
        user_id,
        org_id,
        site_id,
        created_account,
    })
}
