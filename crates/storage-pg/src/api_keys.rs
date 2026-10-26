//! API key（`api_keys` 表，`platform-and-ops.md` §3.3、§4.3、§5）：创建、查找、列出、吊销。
//!
//! 表里只有明文的 SHA-256（`key_hash`，`bytea(32)`，唯一）和明文前 12 个字符（`key_prefix`，仅用于列表识别）；
//! 明文只在创建时由调用方生成并输出一次。key 的格式、生成与哈希在 `rivu-auth`，这里只认 32 字节摘要。
//!
//! 本 crate 的 sqlx 没有启用时间类型特性：时间戳以 Unix 秒（`bigint`）传入，以 ISO 文本（UTC）读出。

use anyhow::{Context, Result, bail};
use sqlx::{PgPool, Row};

/// 按哈希查到的、当前有效的 key（未吊销、未过期，所属组织未删除，所属用户未禁用未删除且仍是组织成员）。
///
/// 这一条查询同时完成 `platform-and-ops.md` §4.3 的「失效」判定：过期、软删除、所属用户被移出 org 或被禁用
/// 都查不到，调用方统一按 401 处理。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyRecord {
    pub id: i64,
    pub organization_id: i64,
    pub user_id: i64,
    /// 用户在该组织里的角色：`owner | admin | editor | viewer`。
    pub role: String,
    /// `resource:action:target` 文本（未解析）。
    pub scopes: Vec<String>,
    /// `organizations.api_hourly_limit`，`None` 用全局默认。
    pub api_hourly_limit: Option<i32>,
    /// 库里存的摘要（调用方用常数时间比较再确认一次）。
    pub key_hash: Vec<u8>,
}

/// 按 SHA-256 摘要查找当前有效的 key。
pub async fn find_active_by_hash(pool: &PgPool, hash: &[u8; 32]) -> Result<Option<KeyRecord>> {
    let row = sqlx::query(
        "SELECT k.id, k.organization_id, k.user_id, k.scopes, k.key_hash, o.api_hourly_limit, m.role
         FROM api_keys k
         JOIN organizations o ON o.id = k.organization_id AND o.deleted_at IS NULL
         JOIN users u ON u.id = k.user_id AND u.deleted_at IS NULL AND u.disabled_at IS NULL
         JOIN memberships m ON m.organization_id = k.organization_id AND m.user_id = k.user_id
                           AND m.deleted_at IS NULL
         WHERE k.key_hash = $1 AND k.deleted_at IS NULL
           AND (k.expires_at IS NULL OR k.expires_at > now())",
    )
    .bind(hash.as_slice())
    .fetch_optional(pool)
    .await
    .context("查询 API key 失败")?;
    row.map(|r| {
        Ok(KeyRecord {
            id: r.try_get("id")?,
            organization_id: r.try_get("organization_id")?,
            user_id: r.try_get("user_id")?,
            role: r.try_get("role")?,
            scopes: r.try_get("scopes")?,
            api_hourly_limit: r.try_get("api_hourly_limit")?,
            key_hash: r.try_get("key_hash")?,
        })
    })
    .transpose()
}

/// 更新 `last_used_at`。调用方负责节流（不要每个请求写一次）。
pub async fn touch_last_used(pool: &PgPool, id: i64) -> Result<()> {
    sqlx::query("UPDATE api_keys SET last_used_at = now() WHERE id = $1")
        .bind(id)
        .execute(pool)
        .await
        .context("更新 api_keys.last_used_at 失败")?;
    Ok(())
}

/// 新 key 的参数。
#[derive(Debug, Clone)]
pub struct NewApiKey<'a> {
    pub organization_id: i64,
    pub user_id: i64,
    pub name: &'a str,
    /// 明文前 12 个字符。
    pub key_prefix: &'a str,
    /// SHA-256(明文)。
    pub key_hash: [u8; 32],
    /// 已校验、规范化的 scope 文本。
    pub scopes: &'a [String],
    /// 过期时刻（Unix 秒）；`None` 永不过期。
    pub expires_at_unix: Option<i64>,
}

/// 写入一把 key，返回 `api_keys.id`。
pub async fn create_key(pool: &PgPool, new: &NewApiKey<'_>) -> Result<i64> {
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO api_keys (organization_id, user_id, name, key_prefix, key_hash, scopes, expires_at)
         VALUES ($1, $2, $3, $4, $5, $6, to_timestamp($7::bigint::double precision))
         RETURNING id",
    )
    .bind(new.organization_id)
    .bind(new.user_id)
    .bind(new.name)
    .bind(new.key_prefix)
    .bind(new.key_hash.as_slice())
    .bind(new.scopes)
    .bind(new.expires_at_unix)
    .fetch_one(pool)
    .await
    .context("写入 api_keys 失败")?;
    Ok(id)
}

/// 列表里的一行（不含哈希与明文）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyListRow {
    pub id: i64,
    pub org_slug: String,
    pub user_email: String,
    pub name: String,
    pub key_prefix: String,
    pub scopes: Vec<String>,
    /// UTC ISO 文本。
    pub created_at: String,
    pub last_used_at: Option<String>,
    pub expires_at: Option<String>,
    pub revoked: bool,
}

/// 列出 key（含已吊销的，带 `revoked` 标记）；`org_slug` 给出时只列该组织的。按 id 升序。
pub async fn list_keys(pool: &PgPool, org_slug: Option<&str>) -> Result<Vec<KeyListRow>> {
    let rows = sqlx::query(
        "SELECT k.id, o.slug AS org_slug, u.email::text AS user_email, k.name, k.key_prefix, k.scopes,
                to_char(k.created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') AS created_at,
                to_char(k.last_used_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') AS last_used_at,
                to_char(k.expires_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') AS expires_at,
                (k.deleted_at IS NOT NULL) AS revoked
         FROM api_keys k
         JOIN organizations o ON o.id = k.organization_id
         JOIN users u ON u.id = k.user_id
         WHERE ($1::text IS NULL OR lower(o.slug) = lower($1))
         ORDER BY k.id",
    )
        .bind(org_slug)
        .fetch_all(pool)
        .await
        .context("列出 API key 失败")?;
    rows.iter()
        .map(|r| {
            Ok(KeyListRow {
                id: r.try_get("id")?,
                org_slug: r.try_get("org_slug")?,
                user_email: r.try_get("user_email")?,
                name: r.try_get("name")?,
                key_prefix: r.try_get("key_prefix")?,
                scopes: r.try_get("scopes")?,
                created_at: r.try_get("created_at")?,
                last_used_at: r.try_get("last_used_at")?,
                expires_at: r.try_get("expires_at")?,
                revoked: r.try_get("revoked")?,
            })
        })
        .collect()
}

/// 吊销（软删除）一把 key；返回是否吊销了（不存在或已吊销返回 `false`）。
pub async fn revoke_key(pool: &PgPool, id: i64) -> Result<bool> {
    let n =
        sqlx::query("UPDATE api_keys SET deleted_at = now() WHERE id = $1 AND deleted_at IS NULL")
            .bind(id)
            .execute(pool)
            .await
            .context("吊销 API key 失败")?
            .rows_affected();
    Ok(n > 0)
}

/// 看板「我的 API key」列表里的一行（只含调用者自己的 key；不含哈希与明文）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnKeyRow {
    pub id: i64,
    pub name: String,
    pub key_prefix: String,
    pub scopes: Vec<String>,
    /// UTC ISO 文本（秒精度）。
    pub created_at: String,
    pub last_used_at: Option<String>,
    pub expires_at: Option<String>,
}

/// 用户在组织里未吊销的 key（过期的仍列出，由看板标注），新的在前；`only_id` 给出时只取这一把。
pub async fn list_own(
    pool: &PgPool,
    org_id: i64,
    user_id: i64,
    only_id: Option<i64>,
) -> Result<Vec<OwnKeyRow>> {
    let rows = sqlx::query(concat!(
        "SELECT k.id, k.name, k.key_prefix, k.scopes, ",
        ts_sec!("k.created_at"),
        " AS created_at, ",
        ts_sec!("k.last_used_at"),
        " AS last_used_at, ",
        ts_sec!("k.expires_at"),
        " AS expires_at
         FROM api_keys k
         WHERE k.organization_id = $1 AND k.user_id = $2 AND k.deleted_at IS NULL
           AND ($3::bigint IS NULL OR k.id = $3)
         ORDER BY k.created_at DESC, k.id DESC"
    ))
    .bind(org_id)
    .bind(user_id)
    .bind(only_id)
    .fetch_all(pool)
    .await
    .context("列出 API key 失败")?;
    rows.iter()
        .map(|r| {
            Ok(OwnKeyRow {
                id: r.try_get("id")?,
                name: r.try_get("name")?,
                key_prefix: r.try_get("key_prefix")?,
                scopes: r.try_get("scopes")?,
                created_at: r.try_get("created_at")?,
                last_used_at: r.try_get("last_used_at")?,
                expires_at: r.try_get("expires_at")?,
            })
        })
        .collect()
}

/// 吊销用户自己在这个组织里未吊销的 key；别人的、别的组织的、不存在或已吊销的返回 `false`。
pub async fn revoke_own(pool: &PgPool, org_id: i64, user_id: i64, id: i64) -> Result<bool> {
    let n = sqlx::query(
        "UPDATE api_keys SET deleted_at = now()
         WHERE id = $1 AND organization_id = $2 AND user_id = $3 AND deleted_at IS NULL",
    )
    .bind(id)
    .bind(org_id)
    .bind(user_id)
    .execute(pool)
    .await
    .context("吊销 API key 失败")?
    .rows_affected();
    Ok(n > 0)
}

/// 组织里未删除站点中 `site_key` 在给定列表里的 `(site_key, 名称)`（校验 scope 目标、解析列表里的站点名）。
pub async fn resolve_sites(
    pool: &PgPool,
    org_id: i64,
    site_keys: &[String],
) -> Result<Vec<(String, String)>> {
    sqlx::query_as(
        "SELECT site_key, name FROM sites
         WHERE organization_id = $1 AND deleted_at IS NULL AND site_key = ANY($2)
         ORDER BY id",
    )
    .bind(org_id)
    .bind(site_keys)
    .fetch_all(pool)
    .await
    .context("查询站点失败")
}

/// 组织的 API 小时配额（`organizations.api_hourly_limit`，`None` 用全局默认）；组织不存在也是 `None`。
pub async fn org_hourly_limit(pool: &PgPool, org_id: i64) -> Result<Option<i32>> {
    let v: Option<Option<i32>> =
        sqlx::query_scalar("SELECT api_hourly_limit FROM organizations WHERE id = $1")
            .bind(org_id)
            .fetch_optional(pool)
            .await
            .context("查询组织配额失败")?;
    Ok(v.flatten())
}

/// 按 slug（不区分大小写）找未删除的组织，返回 id。
pub async fn find_org_id_by_slug(pool: &PgPool, slug: &str) -> Result<Option<i64>> {
    sqlx::query_scalar(
        "SELECT id FROM organizations WHERE lower(slug) = lower($1) AND deleted_at IS NULL",
    )
    .bind(slug)
    .fetch_optional(pool)
    .await
    .context("查询组织失败")
}

/// 为 `rivu apikey create` 选定 key 的所属用户：
/// - 给了邮箱：该用户必须未删除、未禁用，并且是该组织的成员（不看角色：viewer 也可以有 key，`api_key.manage_own`）；
/// - 没给：取该组织里最早加入的 owner。
///
/// 返回 `(user_id, 邮箱)`。
pub async fn resolve_key_owner(
    pool: &PgPool,
    organization_id: i64,
    email: Option<&str>,
) -> Result<(i64, String)> {
    let row = sqlx::query(
        "SELECT u.id, u.email::text AS email
         FROM memberships m JOIN users u ON u.id = m.user_id
         WHERE m.organization_id = $1 AND m.deleted_at IS NULL
           AND u.deleted_at IS NULL AND u.disabled_at IS NULL
           AND (($2::text IS NULL AND m.role = 'owner') OR u.email = ($2::text)::citext)
         ORDER BY m.id
         LIMIT 1",
    )
    .bind(organization_id)
    .bind(email)
    .fetch_optional(pool)
    .await
    .context("查询 key 所属用户失败")?;
    match (row, email) {
        (Some(r), _) => Ok((r.try_get("id")?, r.try_get("email")?)),
        (None, Some(e)) => bail!("用户 `{e}` 不存在、已被禁用，或不是该组织的成员"),
        (None, None) => bail!("该组织没有可用的 owner；请用 --user 指定一个组织成员的邮箱"),
    }
}
