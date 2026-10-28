//! 一次性令牌（`auth_tokens`）：目前只有找回密码。只存 token 的 SHA-256。

use anyhow::{Context, Result};
use sqlx::PgPool;

/// 为用户签发找回密码的令牌：同一个事务里作废他之前没用过的令牌（只有最新的一封邮件有效）。
pub async fn create_password_reset(
    pool: &PgPool,
    user_id: i64,
    token_hash: &[u8; 32],
    ttl_secs: i64,
) -> Result<()> {
    let mut tx = pool.begin().await.context("开启事务失败")?;
    sqlx::query(
        "DELETE FROM auth_tokens WHERE user_id = $1 AND kind = 'password_reset' AND used_at IS NULL",
    )
    .bind(user_id)
    .execute(&mut *tx)
    .await
    .context("作废旧的找回密码令牌失败")?;
    sqlx::query(
        "INSERT INTO auth_tokens (user_id, kind, token_hash, expires_at)
         VALUES ($1, 'password_reset', $2, now() + make_interval(secs => $3))",
    )
    .bind(user_id)
    .bind(&token_hash[..])
    .bind(ttl_secs as f64)
    .execute(&mut *tx)
    .await
    .context("写入找回密码令牌失败")?;
    tx.commit().await.context("提交事务失败")?;
    Ok(())
}

/// 令牌有效（存在、没用过、没过期、用户没被禁用或删除）时返回用户 id，不消耗它（重设页面打开时检查）。
pub async fn password_reset_user(pool: &PgPool, token_hash: &[u8; 32]) -> Result<Option<i64>> {
    sqlx::query_scalar(
        "SELECT t.user_id FROM auth_tokens t JOIN users u ON u.id = t.user_id
         WHERE t.token_hash = $1 AND t.kind = 'password_reset' AND t.used_at IS NULL
           AND t.expires_at > now() AND u.deleted_at IS NULL AND u.disabled_at IS NULL",
    )
    .bind(&token_hash[..])
    .fetch_optional(pool)
    .await
    .context("查询找回密码令牌失败")
}

/// 用掉令牌并改密码（同一个事务：令牌只能用一次，并发的两次提交只有一次成功）；改密码会撤销该用户的全部会话。
/// 令牌无效返回 `None`，否则返回用户 id。
pub async fn reset_password(
    pool: &PgPool,
    token_hash: &[u8; 32],
    new_hash: &str,
) -> Result<Option<i64>> {
    let mut tx = pool.begin().await.context("开启事务失败")?;
    let user_id: Option<i64> = sqlx::query_scalar(
        "UPDATE auth_tokens t SET used_at = now() FROM users u
         WHERE u.id = t.user_id AND t.token_hash = $1 AND t.kind = 'password_reset'
           AND t.used_at IS NULL AND t.expires_at > now() AND u.deleted_at IS NULL AND u.disabled_at IS NULL
         RETURNING t.user_id",
    )
    .bind(&token_hash[..])
    .fetch_optional(&mut *tx)
    .await
    .context("使用找回密码令牌失败")?;
    let Some(user_id) = user_id else {
        return Ok(None);
    };
    sqlx::query("UPDATE users SET password_hash = $2 WHERE id = $1")
        .bind(user_id)
        .bind(new_hash)
        .execute(&mut *tx)
        .await
        .context("更新 users.password_hash 失败")?;
    sqlx::query(
        "UPDATE auth_sessions SET revoked_at = now() WHERE user_id = $1 AND revoked_at IS NULL",
    )
    .bind(user_id)
    .execute(&mut *tx)
    .await
    .context("撤销会话失败")?;
    tx.commit().await.context("提交事务失败")?;
    Ok(Some(user_id))
}
