//! 两步验证（`user_totp`）。密钥是密文（`rivu_auth::totp::Vault`），这里只负责存取与原子的状态变化：
//! 用过的时间步与恢复码都在一条 UPDATE 里检查加修改，并发的两次登录不会都用上同一个验证码。

use anyhow::{Context, Result};
use sqlx::{PgPool, Row};

/// 一个用户的两步验证状态。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TotpRow {
    pub secret_enc: Vec<u8>,
    /// 已启用（设置中为 `false`）。
    pub enabled: bool,
    pub last_step: i64,
    /// 剩余的恢复码个数。
    pub recovery_remaining: i64,
}

pub async fn get(pool: &PgPool, user_id: i64) -> Result<Option<TotpRow>> {
    let row = sqlx::query(
        "SELECT secret_enc, enabled_at IS NOT NULL AS enabled, last_step,
                cardinality(recovery_hashes)::bigint AS remaining
         FROM user_totp WHERE user_id = $1",
    )
    .bind(user_id)
    .fetch_optional(pool)
    .await
    .context("查询两步验证失败")?;
    row.map(|r| {
        Ok(TotpRow {
            secret_enc: r.try_get("secret_enc")?,
            enabled: r.try_get("enabled")?,
            last_step: r.try_get("last_step")?,
            recovery_remaining: r.try_get("remaining")?,
        })
    })
    .transpose()
}

/// 开始设置：写入新密钥（未启用）。已启用时不覆盖，返回 `false`。
pub async fn begin_setup(pool: &PgPool, user_id: i64, secret_enc: &[u8]) -> Result<bool> {
    let n = sqlx::query(
        "INSERT INTO user_totp (user_id, secret_enc) VALUES ($1, $2)
         ON CONFLICT (user_id) DO UPDATE
            SET secret_enc = EXCLUDED.secret_enc, last_step = 0, recovery_hashes = '{}', created_at = now()
          WHERE user_totp.enabled_at IS NULL",
    )
    .bind(user_id)
    .bind(secret_enc)
    .execute(pool)
    .await
    .context("写入两步验证密钥失败")?
    .rows_affected();
    Ok(n > 0)
}

fn hashes(h: &[[u8; 32]]) -> Vec<Vec<u8>> {
    h.iter().map(|x| x.to_vec()).collect()
}

/// 用第一个验证码确认后启用，同时写入恢复码。设置中且这一步没用过才成功。
pub async fn enable(pool: &PgPool, user_id: i64, step: i64, recovery: &[[u8; 32]]) -> Result<bool> {
    let n = sqlx::query(
        "UPDATE user_totp SET enabled_at = now(), last_step = $2, recovery_hashes = $3
         WHERE user_id = $1 AND enabled_at IS NULL AND last_step < $2",
    )
    .bind(user_id)
    .bind(step)
    .bind(hashes(recovery))
    .execute(pool)
    .await
    .context("启用两步验证失败")?
    .rows_affected();
    Ok(n > 0)
}

/// 登录时记下用过的时间步；这一步（或更晚的）已经用过返回 `false`（重放）。
pub async fn use_step(pool: &PgPool, user_id: i64, step: i64) -> Result<bool> {
    let n = sqlx::query(
        "UPDATE user_totp SET last_step = $2
         WHERE user_id = $1 AND enabled_at IS NOT NULL AND last_step < $2",
    )
    .bind(user_id)
    .bind(step)
    .execute(pool)
    .await
    .context("记录两步验证时间步失败")?
    .rows_affected();
    Ok(n > 0)
}

/// 用掉一个恢复码；有这个码返回剩余个数，没有返回 `None`。
pub async fn use_recovery(pool: &PgPool, user_id: i64, hash: &[u8; 32]) -> Result<Option<i64>> {
    sqlx::query_scalar(
        "UPDATE user_totp SET recovery_hashes = array_remove(recovery_hashes, $2)
         WHERE user_id = $1 AND enabled_at IS NOT NULL AND $2 = ANY (recovery_hashes)
         RETURNING cardinality(recovery_hashes)::bigint",
    )
    .bind(user_id)
    .bind(&hash[..])
    .fetch_optional(pool)
    .await
    .context("使用恢复码失败")
}

/// 换一组新的恢复码（旧的全部作废）；没有启用返回 `false`。
pub async fn replace_recovery(pool: &PgPool, user_id: i64, recovery: &[[u8; 32]]) -> Result<bool> {
    let n = sqlx::query(
        "UPDATE user_totp SET recovery_hashes = $2 WHERE user_id = $1 AND enabled_at IS NOT NULL",
    )
    .bind(user_id)
    .bind(hashes(recovery))
    .execute(pool)
    .await
    .context("更新恢复码失败")?
    .rows_affected();
    Ok(n > 0)
}

/// 关闭（或放弃设置）；返回是否有记录。
pub async fn disable(pool: &PgPool, user_id: i64) -> Result<bool> {
    let n = sqlx::query("DELETE FROM user_totp WHERE user_id = $1")
        .bind(user_id)
        .execute(pool)
        .await
        .context("关闭两步验证失败")?
        .rows_affected();
    Ok(n > 0)
}
