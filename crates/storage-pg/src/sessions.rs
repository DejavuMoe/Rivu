//! 登录会话（`auth_sessions`，`platform-and-ops.md` §3.3、§5）：只存令牌的 SHA-256。
//!
//! 读取路径（[`find_active`]）同一条查询判定：未撤销、未过期、用户未禁用也未删除——
//! 所以禁用或删除用户后，他的会话在下一个请求就失效，不依赖清理任务。

use anyhow::{Context, Result};
use sqlx::{PgPool, Row};

/// 按令牌摘要查到的有效会话与它的用户。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveSession {
    pub id: i64,
    pub user_id: i64,
    pub email: String,
    pub name: Option<String>,
    pub is_instance_admin: bool,
    /// 距上次续期（`last_used_at`）过去的秒数；调用方据此决定要不要续期（超过 10 分钟才写库）。
    pub idle_secs: i64,
}

/// 建一个会话，返回 `auth_sessions.id`。`ttl_secs` 是有效期（从现在起）。
pub async fn create(
    pool: &PgPool,
    user_id: i64,
    token_hash: &[u8; 32],
    device_label: Option<&str>,
    ttl_secs: i64,
) -> Result<i64> {
    sqlx::query_scalar(
        "INSERT INTO auth_sessions (user_id, token_hash, device_label, expires_at)
         VALUES ($1, $2, $3, now() + ($4::bigint * interval '1 second')) RETURNING id",
    )
    .bind(user_id)
    .bind(token_hash.as_slice())
    .bind(device_label)
    .bind(ttl_secs)
    .fetch_one(pool)
    .await
    .context("写入 auth_sessions 失败")
}

/// 按令牌摘要查当前有效的会话。
pub async fn find_active(pool: &PgPool, token_hash: &[u8; 32]) -> Result<Option<ActiveSession>> {
    let row = sqlx::query(
        "SELECT s.id, s.user_id, u.email::text AS email, u.name, u.is_instance_admin,
                GREATEST(extract(epoch FROM (now() - s.last_used_at)), 0)::bigint AS idle_secs
         FROM auth_sessions s
         JOIN users u ON u.id = s.user_id AND u.deleted_at IS NULL AND u.disabled_at IS NULL
         WHERE s.token_hash = $1 AND s.revoked_at IS NULL AND s.expires_at > now()",
    )
    .bind(token_hash.as_slice())
    .fetch_optional(pool)
    .await
    .context("查询会话失败")?;
    row.map(|r| {
        Ok(ActiveSession {
            id: r.try_get("id")?,
            user_id: r.try_get("user_id")?,
            email: r.try_get("email")?,
            name: r.try_get("name")?,
            is_instance_admin: r.try_get("is_instance_admin")?,
            idle_secs: r.try_get("idle_secs")?,
        })
    })
    .transpose()
}

/// 续期：`last_used_at = now()`，`expires_at = now() + ttl`。已撤销的会话不续。
pub async fn renew(pool: &PgPool, id: i64, ttl_secs: i64) -> Result<()> {
    sqlx::query(
        "UPDATE auth_sessions
         SET last_used_at = now(), expires_at = now() + ($2::bigint * interval '1 second')
         WHERE id = $1 AND revoked_at IS NULL",
    )
    .bind(id)
    .bind(ttl_secs)
    .execute(pool)
    .await
    .context("续期会话失败")?;
    Ok(())
}

/// 撤销用户自己的一个会话；返回是否撤销了（不存在、不是他的、已撤销都是 `false`）。
pub async fn revoke(pool: &PgPool, user_id: i64, id: i64) -> Result<bool> {
    let n = sqlx::query(
        "UPDATE auth_sessions SET revoked_at = now()
         WHERE id = $1 AND user_id = $2 AND revoked_at IS NULL",
    )
    .bind(id)
    .bind(user_id)
    .execute(pool)
    .await
    .context("撤销会话失败")?
    .rows_affected();
    Ok(n > 0)
}

/// 按令牌摘要撤销（登录时替换掉请求里带的旧会话）。
pub async fn revoke_by_hash(pool: &PgPool, token_hash: &[u8; 32]) -> Result<()> {
    sqlx::query(
        "UPDATE auth_sessions SET revoked_at = now() WHERE token_hash = $1 AND revoked_at IS NULL",
    )
    .bind(token_hash.as_slice())
    .execute(pool)
    .await
    .context("撤销会话失败")?;
    Ok(())
}

/// 撤销用户的全部会话（「退出所有设备」），返回撤销的个数。
pub async fn revoke_all(pool: &PgPool, user_id: i64) -> Result<u64> {
    Ok(sqlx::query(
        "UPDATE auth_sessions SET revoked_at = now() WHERE user_id = $1 AND revoked_at IS NULL",
    )
    .bind(user_id)
    .execute(pool)
    .await
    .context("撤销会话失败")?
    .rows_affected())
}

/// 会话列表的一行。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRow {
    pub id: i64,
    pub device_label: Option<String>,
    /// UTC ISO 文本。
    pub created_at: String,
    pub last_used_at: String,
    pub expires_at: String,
}

/// 用户当前有效的会话，最近使用的在前。
pub async fn list_active(pool: &PgPool, user_id: i64) -> Result<Vec<SessionRow>> {
    let rows = sqlx::query(
        r#"SELECT id, device_label,
                  to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS"Z"') AS created_at,
                  to_char(last_used_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS"Z"') AS last_used_at,
                  to_char(expires_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS"Z"') AS expires_at
           FROM auth_sessions
           WHERE user_id = $1 AND revoked_at IS NULL AND expires_at > now()
           ORDER BY last_used_at DESC, id DESC"#,
    )
    .bind(user_id)
    .fetch_all(pool)
    .await
    .context("列出会话失败")?;
    rows.iter()
        .map(|r| {
            Ok(SessionRow {
                id: r.try_get("id")?,
                device_label: r.try_get("device_label")?,
                created_at: r.try_get("created_at")?,
                last_used_at: r.try_get("last_used_at")?,
                expires_at: r.try_get("expires_at")?,
            })
        })
        .collect()
}

/// 全部有效会话数（指标 `rivu_auth_active_sessions`）：未撤销、未过期，用户未禁用也未删除。
pub async fn count_active(pool: &PgPool) -> Result<i64> {
    sqlx::query_scalar(
        "SELECT count(*) FROM auth_sessions s
         JOIN users u ON u.id = s.user_id AND u.deleted_at IS NULL AND u.disabled_at IS NULL
         WHERE s.revoked_at IS NULL AND s.expires_at > now()",
    )
    .fetch_one(pool)
    .await
    .context("统计有效会话失败")
}

/// `auth_sessions_cleanup`：删除过期与已撤销的行，以及到期的共享链接访问凭证。返回 `(会话行数, 凭证行数)`。
pub async fn cleanup(pool: &PgPool) -> Result<(u64, u64)> {
    let sessions =
        sqlx::query("DELETE FROM auth_sessions WHERE expires_at < now() OR revoked_at IS NOT NULL")
            .execute(pool)
            .await
            .context("清理 auth_sessions 失败")?
            .rows_affected();
    let grants = sqlx::query("DELETE FROM shared_link_grants WHERE expires_at < now()")
        .execute(pool)
        .await
        .context("清理 shared_link_grants 失败")?
        .rows_affected();
    Ok((sessions, grants))
}
