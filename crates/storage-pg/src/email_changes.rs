//! 更换登录邮箱（`email_changes`，`platform-and-ops.md` §5「更换登录邮箱」）。
//!
//! 配置了 SMTP 时，请求只记下一条待确认的更换（只存链接令牌的 SHA-256），新邮箱里的链接打开并确认后才真正改；
//! 没有 SMTP 时（以及运维的 `rivu user set-email`）直接改。真正改邮箱的两条路径都在同一个事务里：
//! 改 `users.email`、撤销会话、作废找回密码的令牌与待确认的更换，并处理周报的邮箱确认。

use anyhow::{Context, Result};
use sqlx::{PgConnection, PgPool};

/// 一条还有效的待确认更换。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pending {
    pub new_email: String,
    /// RFC 3339（UTC，秒精度）。
    pub expires_at: String,
}

/// 用户当前有效的待确认更换：没过期，而且账号邮箱仍是发起时的邮箱。
pub async fn pending(pool: &PgPool, user_id: i64) -> Result<Option<Pending>> {
    let row: Option<(String, String)> = sqlx::query_as(concat!(
        "SELECT c.new_email::text, ",
        ts_sec!("c.expires_at"),
        " FROM email_changes c JOIN users u ON u.id = c.user_id AND u.email = c.old_email
         WHERE c.user_id = $1 AND c.expires_at > now()"
    ))
    .bind(user_id)
    .fetch_optional(pool)
    .await
    .context("查询待确认的邮箱更换失败")?;
    Ok(row.map(|(new_email, expires_at)| Pending {
        new_email,
        expires_at,
    }))
}

/// 新邮箱是否已被别的（未删除的）账号使用。
pub async fn email_taken(pool: &PgPool, user_id: i64, email: &str) -> Result<bool> {
    sqlx::query_scalar(
        "SELECT EXISTS (SELECT FROM users WHERE email = ($1::text)::citext
                        AND deleted_at IS NULL AND id <> $2)",
    )
    .bind(email)
    .bind(user_id)
    .fetch_one(pool)
    .await
    .context("查询邮箱是否已被使用失败")
}

/// 记下一条待确认的更换，替换这个用户之前的那条（旧链接随之失效）。返回 `RFC 3339` 的过期时间；
/// 账号已停用、删除，或邮箱已不是 `old_email`（并发的另一次更换）时返回 `None`。
pub async fn create(
    pool: &PgPool,
    user_id: i64,
    old_email: &str,
    new_email: &str,
    lang: &str,
    token_hash: &[u8; 32],
    ttl_secs: i64,
) -> Result<Option<String>> {
    let mut tx = pool.begin().await.context("开启事务失败")?;
    let locked: Option<i64> = sqlx::query_scalar(
        "SELECT id FROM users WHERE id = $1 AND email = ($2::text)::citext
         AND deleted_at IS NULL AND disabled_at IS NULL FOR NO KEY UPDATE",
    )
    .bind(user_id)
    .bind(old_email)
    .fetch_optional(&mut *tx)
    .await
    .context("锁定用户失败")?;
    if locked.is_none() {
        return Ok(None);
    }
    let expires_at: String = sqlx::query_scalar(concat!(
        "INSERT INTO email_changes (user_id, old_email, new_email, lang, token_hash, expires_at)
         VALUES ($1, ($2::text)::citext, ($3::text)::citext, $4, $5,
                 now() + make_interval(secs => $6))
         ON CONFLICT (user_id) DO UPDATE
         SET old_email = excluded.old_email, new_email = excluded.new_email, lang = excluded.lang,
             token_hash = excluded.token_hash, expires_at = excluded.expires_at, created_at = now()
         RETURNING ",
        ts_sec!("expires_at")
    ))
    .bind(user_id)
    .bind(old_email)
    .bind(new_email)
    .bind(lang)
    .bind(&token_hash[..])
    .bind(ttl_secs as f64)
    .fetch_one(&mut *tx)
    .await
    .context("写入 email_changes 失败")?;
    tx.commit().await.context("提交事务失败")?;
    Ok(Some(expires_at))
}

/// 删掉这个用户的待确认更换；给了 `token_hash` 时只删这一条（发信失败时撤回刚写的那条）。返回是否删了。
pub async fn cancel(pool: &PgPool, user_id: i64, token_hash: Option<&[u8; 32]>) -> Result<bool> {
    let n = sqlx::query(
        "DELETE FROM email_changes WHERE user_id = $1 AND ($2::bytea IS NULL OR token_hash = $2)",
    )
    .bind(user_id)
    .bind(token_hash.map(|h| &h[..]))
    .execute(pool)
    .await
    .context("删除 email_changes 失败")?
    .rows_affected();
    Ok(n > 0)
}

/// 链接有效（存在、没过期、账号正常且邮箱仍是发起时的邮箱）时返回新邮箱，不消耗它（确认页打开时检查）。
pub async fn lookup(pool: &PgPool, token_hash: &[u8; 32]) -> Result<Option<String>> {
    sqlx::query_scalar(
        "SELECT c.new_email::text FROM email_changes c
         JOIN users u ON u.id = c.user_id AND u.email = c.old_email
                     AND u.deleted_at IS NULL AND u.disabled_at IS NULL
         WHERE c.token_hash = $1 AND c.expires_at > now()",
    )
    .bind(&token_hash[..])
    .fetch_optional(pool)
    .await
    .context("查询邮箱更换链接失败")
}

/// 真正改了邮箱。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Changed {
    pub user_id: i64,
    pub old_email: String,
    pub new_email: String,
    /// 发起时的语言（`zh` / `en`）；直接修改时为 `None`。
    pub lang: Option<String>,
    /// 撤销了几个会话。
    pub revoked_sessions: u64,
}

/// 改邮箱的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Changed(Changed),
    /// 链接无效（不存在、过期、用过、账号邮箱已变）或用户不存在。
    NotFound,
    /// 新邮箱已被别的账号使用。
    Taken,
}

/// 凭链接确认更换。`presented` 是确认请求带着的有效会话 `(用户 id, 会话 id)`：属于这个用户时保留这一个，
/// 其余全部撤销；没有就全部撤销。旧邮箱已确认收周报时，确认跟到新邮箱（链接证明了新邮箱能收信），已开的站点改投新邮箱。
pub async fn confirm(
    pool: &PgPool,
    token_hash: &[u8; 32],
    presented: Option<(i64, i64)>,
) -> Result<Outcome> {
    let mut tx = pool.begin().await.context("开启事务失败")?;
    // 先锁用户再读待确认的那一行：与发起、取消和周报的写入同一个顺序
    let user_id: Option<i64> =
        sqlx::query_scalar("SELECT user_id FROM email_changes WHERE token_hash = $1")
            .bind(&token_hash[..])
            .fetch_optional(&mut *tx)
            .await
            .context("查询邮箱更换链接失败")?;
    let Some(user_id) = user_id else {
        return Ok(Outcome::NotFound);
    };
    if lock_user(&mut tx, user_id).await?.is_none() {
        return Ok(Outcome::NotFound);
    }
    let row: Option<(String, String, String)> = sqlx::query_as(
        "SELECT c.old_email::text, c.new_email::text, c.lang FROM email_changes c
         JOIN users u ON u.id = c.user_id AND u.email = c.old_email
         WHERE c.token_hash = $1 AND c.user_id = $2 AND c.expires_at > now()",
    )
    .bind(&token_hash[..])
    .bind(user_id)
    .fetch_optional(&mut *tx)
    .await
    .context("查询邮箱更换链接失败")?;
    let Some((old_email, new_email, lang)) = row else {
        return Ok(Outcome::NotFound);
    };
    let keep = presented.filter(|(u, _)| *u == user_id).map(|(_, s)| s);
    let Some(revoked) = apply(&mut tx, user_id, &new_email, true, keep).await? else {
        // 新邮箱这期间被别人用了：这条更换再也成不了，删掉（事务可能已因唯一约束中止，另起一条语句）
        drop(tx);
        sqlx::query("DELETE FROM email_changes WHERE token_hash = $1")
            .bind(&token_hash[..])
            .execute(pool)
            .await
            .context("删除 email_changes 失败")?;
        return Ok(Outcome::Taken);
    };
    // 周报：旧邮箱已确认收周报时，确认跟到新邮箱（链接证明了新邮箱能收信）；待确认的已在 `apply` 里作废
    let moved = sqlx::query(
        "UPDATE weekly_report_emails SET email = $2, confirmed_at = statement_timestamp()
         WHERE user_id = $1 AND email = $3 AND confirmed_at IS NOT NULL",
    )
    .bind(user_id)
    .bind(&new_email)
    .bind(&old_email)
    .execute(&mut *tx)
    .await
    .context("更新周报邮箱确认失败")?
    .rows_affected();
    if moved > 0 {
        // 同周报确认新邮箱：已开的站点改投新邮箱，新的订阅时间让发到旧邮箱的退订链接失效
        sqlx::query(
            "UPDATE weekly_report_subscriptions SET email = $2, confirmed_at = statement_timestamp()
             WHERE user_id = $1 AND email <> $2",
        )
        .bind(user_id)
        .bind(&new_email)
        .execute(&mut *tx)
        .await
        .context("更新周报订阅的邮箱失败")?;
    }
    tx.commit().await.context("提交事务失败")?;
    Ok(Outcome::Changed(Changed {
        user_id,
        old_email,
        new_email,
        lang: Some(lang),
        revoked_sessions: revoked,
    }))
}

/// 直接改邮箱（没有 SMTP 时的账户页、`rivu user set-email`）。`keep_session` 是要保留的会话
/// （`None` 撤销全部）。新邮箱没有经过收信验证，周报的邮箱确认不跟过去：在账户页重新确认之前不发周报。
pub async fn change_now(
    pool: &PgPool,
    user_id: i64,
    new_email: &str,
    keep_session: Option<i64>,
) -> Result<Outcome> {
    let mut tx = pool.begin().await.context("开启事务失败")?;
    let Some(old_email) = lock_user(&mut tx, user_id).await? else {
        return Ok(Outcome::NotFound);
    };
    let Some(revoked) = apply(&mut tx, user_id, new_email, false, keep_session).await? else {
        return Ok(Outcome::Taken);
    };
    tx.commit().await.context("提交事务失败")?;
    Ok(Outcome::Changed(Changed {
        user_id,
        old_email,
        new_email: new_email.to_owned(),
        lang: None,
        revoked_sessions: revoked,
    }))
}

/// 锁住一个正常的（未停用、未删除）用户，返回他当前的邮箱。
async fn lock_user(tx: &mut PgConnection, user_id: i64) -> Result<Option<String>> {
    sqlx::query_scalar(
        "SELECT email::text FROM users WHERE id = $1 AND deleted_at IS NULL AND disabled_at IS NULL
         FOR UPDATE",
    )
    .bind(user_id)
    .fetch_optional(tx)
    .await
    .context("锁定用户失败")
}

/// 改邮箱的公共部分（调用方已锁住用户）。新邮箱已被别人使用时返回 `None`（这时事务可能已中止，
/// 调用方只能回滚），否则返回撤销的会话数。
async fn apply(
    tx: &mut PgConnection,
    user_id: i64,
    new_email: &str,
    verified: bool,
    keep_session: Option<i64>,
) -> Result<Option<u64>> {
    let taken: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT FROM users WHERE email = ($1::text)::citext
                        AND deleted_at IS NULL AND id <> $2)",
    )
    .bind(new_email)
    .bind(user_id)
    .fetch_one(&mut *tx)
    .await
    .context("查询邮箱是否已被使用失败")?;
    if taken {
        return Ok(None);
    }
    // 上面查过了，唯一约束仍可能被并发插入的新用户撞上：当作已被使用
    let updated = sqlx::query(
        "UPDATE users SET email = ($2::text)::citext,
                          email_verified_at = CASE WHEN $3 THEN now() END
         WHERE id = $1",
    )
    .bind(user_id)
    .bind(new_email)
    .bind(verified)
    .execute(&mut *tx)
    .await;
    match updated {
        Ok(_) => {}
        Err(e)
            if e.as_database_error()
                .is_some_and(|d| d.code().as_deref() == Some("23505")) =>
        {
            return Ok(None);
        }
        Err(e) => return Err(e).context("更新 users.email 失败"),
    }
    sqlx::query("DELETE FROM email_changes WHERE user_id = $1")
        .bind(user_id)
        .execute(&mut *tx)
        .await
        .context("删除 email_changes 失败")?;
    // 发到旧邮箱的找回密码链接不能再用来接管账号
    crate::auth_tokens::invalidate_password_resets(&mut *tx, user_id).await?;
    // 周报：发给旧邮箱、还没确认的链接作废
    sqlx::query(
        "UPDATE weekly_report_emails SET token_hash = NULL, token_expires_at = NULL
         WHERE user_id = $1 AND token_hash IS NOT NULL",
    )
    .bind(user_id)
    .execute(&mut *tx)
    .await
    .context("作废周报确认链接失败")?;
    let revoked = sqlx::query(
        "UPDATE auth_sessions SET revoked_at = now()
         WHERE user_id = $1 AND revoked_at IS NULL AND id IS DISTINCT FROM $2",
    )
    .bind(user_id)
    .bind(keep_session)
    .execute(&mut *tx)
    .await
    .context("撤销会话失败")?
    .rows_affected();
    Ok(Some(revoked))
}

/// 改密码与凭邮件重设密码时作废待确认的更换（同一个事务）：拿回账号的人不必再担心别人发起的更换。
pub(crate) async fn invalidate(db: &mut PgConnection, user_id: i64) -> Result<()> {
    sqlx::query("DELETE FROM email_changes WHERE user_id = $1")
        .bind(user_id)
        .execute(db)
        .await
        .context("作废待确认的邮箱更换失败")?;
    Ok(())
}
