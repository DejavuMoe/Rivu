//! 共享链接（`shared_links`）与它的访问凭证（`shared_link_grants`）。
//!
//! 链接由随机 slug（至少 24 个字符，库里存明文：slug 本身就是访问能力，同 `site_key` 的列表语义）标识；
//! 可选密码（argon2id）、可选过期时间、可选绑定的 segment（强制叠加它的过滤）。
//! 所有读取路径都过滤：链接未吊销、未过期、站点未删除、站点所属组织未删除。

use anyhow::{Context, Result, bail};
use sqlx::{PgPool, Row};

/// 一个有效的共享链接，连同它指向的站点。
#[derive(Clone, PartialEq, Eq)]
pub struct LinkInfo {
    pub id: i64,
    pub site_id: u64,
    pub site_key: String,
    pub site_name: String,
    pub timezone: String,
    /// `None` = 没有密码。
    pub password_hash: Option<String>,
    /// 绑定的 segment 的过滤（JSON 文本）。链接绑定了 segment 却读不到它（已删除）时，[`find_active`] 不返回该链接
    /// （fail-closed：宁可 404，也不能把未过滤的数据放出去）。
    pub segment_filter: Option<String>,
}

impl std::fmt::Debug for LinkInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LinkInfo")
            .field("id", &self.id)
            .field("site_id", &self.site_id)
            .field("has_password", &self.password_hash.is_some())
            .finish()
    }
}

/// slug 的格式：24 到 64 个 base64url 字符。格式不对的不必查库。
pub fn is_slug_shaped(s: &str) -> bool {
    (24..=64).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// 按 slug 查有效的链接。
pub async fn find_active(pool: &PgPool, slug: &str) -> Result<Option<LinkInfo>> {
    let row = sqlx::query(
        "SELECT l.id, l.site_id, s.site_key, s.name AS site_name, s.timezone, l.password_hash,
                l.segment_id, seg.filter::text AS segment_filter
         FROM shared_links l
         JOIN sites s ON s.id = l.site_id AND s.deleted_at IS NULL
         JOIN organizations o ON o.id = s.organization_id AND o.deleted_at IS NULL
         LEFT JOIN segments seg ON seg.id = l.segment_id AND seg.deleted_at IS NULL
         WHERE l.slug = $1 AND l.deleted_at IS NULL AND (l.expires_at IS NULL OR l.expires_at > now())",
    )
    .bind(slug)
    .fetch_optional(pool)
    .await
    .context("查询共享链接失败")?;
    let Some(r) = row else {
        return Ok(None);
    };
    let segment_id: Option<i64> = r.try_get("segment_id")?;
    let segment_filter: Option<String> = r.try_get("segment_filter")?;
    if segment_id.is_some() && segment_filter.is_none() {
        return Ok(None);
    }
    let site_id: i64 = r.try_get("site_id")?;
    Ok(Some(LinkInfo {
        id: r.try_get("id")?,
        site_id: u64::try_from(site_id).context("sites.id 为负数")?,
        site_key: r.try_get("site_key")?,
        site_name: r.try_get("site_name")?,
        timezone: r.try_get("timezone")?,
        password_hash: r.try_get("password_hash")?,
        segment_filter,
    }))
}

/// 新链接的参数。
pub struct NewLink<'a> {
    pub site_key: &'a str,
    pub slug: &'a str,
    /// argon2id 的 PHC 字符串；`None` = 没有密码。
    pub password_hash: Option<&'a str>,
    /// 过期时刻（Unix 秒）；`None` 永不过期。
    pub expires_at_unix: Option<i64>,
    pub created_by: Option<i64>,
    /// 名称（调用方已校验：1 到 100 个字符）；`None` = 没有名称。
    pub name: Option<&'a str>,
}

/// 建链接，返回 `shared_links.id`。站点不存在或已删除报错。
pub async fn create(pool: &PgPool, new: &NewLink<'_>) -> Result<i64> {
    let id: Option<i64> = sqlx::query_scalar(
        "INSERT INTO shared_links (site_id, slug, password_hash, expires_at, created_by, name)
         SELECT s.id, $2, $3, to_timestamp($4::bigint::double precision), $5, $6
         FROM sites s WHERE s.site_key = $1 AND s.deleted_at IS NULL
         RETURNING id",
    )
    .bind(new.site_key)
    .bind(new.slug)
    .bind(new.password_hash)
    .bind(new.expires_at_unix)
    .bind(new.created_by)
    .bind(new.name)
    .fetch_optional(pool)
    .await
    .context("写入 shared_links 失败")?;
    match id {
        Some(id) => Ok(id),
        None => bail!("找不到站点 `{}`（或已删除）", new.site_key),
    }
}

/// `rivu share list` 的一行。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkRow {
    pub id: i64,
    pub slug: String,
    pub has_password: bool,
    pub segment_id: Option<i64>,
    pub created_at: String,
    pub expires_at: Option<String>,
    pub revoked: bool,
}

/// 站点的全部链接（含已吊销的，带 `revoked` 标记），按 id 升序。
pub async fn list(pool: &PgPool, site_key: &str) -> Result<Vec<LinkRow>> {
    let rows = sqlx::query(
        r#"SELECT l.id, l.slug, (l.password_hash IS NOT NULL) AS has_password, l.segment_id,
                  to_char(l.created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS"Z"') AS created_at,
                  to_char(l.expires_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS"Z"') AS expires_at,
                  (l.deleted_at IS NOT NULL) AS revoked
           FROM shared_links l JOIN sites s ON s.id = l.site_id
           WHERE s.site_key = $1 AND s.deleted_at IS NULL
           ORDER BY l.id"#,
    )
    .bind(site_key)
    .fetch_all(pool)
    .await
    .context("列出共享链接失败")?;
    rows.iter()
        .map(|r| {
            Ok(LinkRow {
                id: r.try_get("id")?,
                slug: r.try_get("slug")?,
                has_password: r.try_get("has_password")?,
                segment_id: r.try_get("segment_id")?,
                created_at: r.try_get("created_at")?,
                expires_at: r.try_get("expires_at")?,
                revoked: r.try_get("revoked")?,
            })
        })
        .collect()
}

/// 设置页的一行链接（未吊销的，含已过期的）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkAdminRow {
    pub id: i64,
    pub name: Option<String>,
    pub slug: String,
    pub has_password: bool,
    pub expires_at: Option<String>,
    pub created_at: String,
    /// 创建者（用户已删除、或命令行建的链接为 `None`）。
    pub created_by: Option<LinkCreator>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkCreator {
    pub id: i64,
    pub name: Option<String>,
    pub email: String,
}

macro_rules! admin_select {
    () => {
        concat!(
    "SELECT l.id, l.name, l.slug, (l.password_hash IS NOT NULL) AS has_password, ",
    ts_utc!("l.expires_at"),
    " AS expires_at, ",
    ts_utc!("l.created_at"),
    " AS created_at, u.id AS creator_id, u.name AS creator_name, u.email::text AS creator_email
     FROM shared_links l LEFT JOIN users u ON u.id = l.created_by AND u.deleted_at IS NULL
     WHERE l.site_id = $1 AND l.deleted_at IS NULL"
        )
    };
}

fn admin_row(r: &sqlx::postgres::PgRow) -> Result<LinkAdminRow> {
    let creator_id: Option<i64> = r.try_get("creator_id")?;
    let created_by = match creator_id {
        Some(id) => Some(LinkCreator {
            id,
            name: r.try_get("creator_name")?,
            email: r.try_get("creator_email")?,
        }),
        None => None,
    };
    Ok(LinkAdminRow {
        id: r.try_get("id")?,
        name: r.try_get("name")?,
        slug: r.try_get("slug")?,
        has_password: r.try_get("has_password")?,
        expires_at: r.try_get("expires_at")?,
        created_at: r.try_get("created_at")?,
        created_by,
    })
}

/// 站点未吊销的链接，按创建时间降序（已过期的也在，由前端标注）。
pub async fn list_for_site(pool: &PgPool, site_id: u64) -> Result<Vec<LinkAdminRow>> {
    let rows = sqlx::query(concat!(
        admin_select!(),
        " ORDER BY l.created_at DESC, l.id DESC"
    ))
    .bind(i64::try_from(site_id).context("site_id 超出范围")?)
    .fetch_all(pool)
    .await
    .context("列出共享链接失败")?;
    rows.iter().map(admin_row).collect()
}

/// 按 id 取站点里一条未吊销的链接；不属于该站点返回 `None`。
pub async fn get_for_site(pool: &PgPool, site_id: u64, id: i64) -> Result<Option<LinkAdminRow>> {
    let row = sqlx::query(concat!(admin_select!(), " AND l.id = $2"))
        .bind(i64::try_from(site_id).context("site_id 超出范围")?)
        .bind(id)
        .fetch_optional(pool)
        .await
        .context("查询共享链接失败")?;
    row.as_ref().map(admin_row).transpose()
}

/// 吊销（软删除）站点里的一个链接并删掉它的访问凭证；链接不属于该站点、不存在或已吊销返回 `false`。
pub async fn revoke_for_site(pool: &PgPool, site_id: u64, id: i64) -> Result<bool> {
    let owned: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM shared_links WHERE id = $1 AND site_id = $2)",
    )
    .bind(id)
    .bind(i64::try_from(site_id).context("site_id 超出范围")?)
    .fetch_one(pool)
    .await
    .context("查询共享链接失败")?;
    if !owned {
        return Ok(false);
    }
    revoke(pool, id).await
}

/// 吊销（软删除）一个链接，并删掉它的访问凭证；返回是否吊销了。
pub async fn revoke(pool: &PgPool, id: i64) -> Result<bool> {
    let mut tx = pool.begin().await.context("开启事务失败")?;
    let n = sqlx::query(
        "UPDATE shared_links SET deleted_at = now() WHERE id = $1 AND deleted_at IS NULL",
    )
    .bind(id)
    .execute(&mut *tx)
    .await
    .context("吊销共享链接失败")?
    .rows_affected();
    sqlx::query("DELETE FROM shared_link_grants WHERE shared_link_id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await
        .context("删除访问凭证失败")?;
    tx.commit().await.context("提交事务失败")?;
    Ok(n > 0)
}

/// 签发一个访问凭证（密码通过之后）。顺带清掉已到期的凭证行。
pub async fn create_grant(
    pool: &PgPool,
    link_id: i64,
    token_hash: &[u8; 32],
    ttl_secs: i64,
) -> Result<()> {
    sqlx::query("DELETE FROM shared_link_grants WHERE expires_at < now()")
        .execute(pool)
        .await
        .context("清理到期的访问凭证失败")?;
    sqlx::query(
        "INSERT INTO shared_link_grants (shared_link_id, token_hash, expires_at)
         VALUES ($1, $2, now() + ($3::bigint * interval '1 second'))",
    )
    .bind(link_id)
    .bind(token_hash.as_slice())
    .bind(ttl_secs)
    .execute(pool)
    .await
    .context("写入 shared_link_grants 失败")?;
    Ok(())
}

/// 这个凭证是否对这个链接有效（绑定 link、未到期）。
pub async fn grant_valid(pool: &PgPool, link_id: i64, token_hash: &[u8; 32]) -> Result<bool> {
    sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM shared_link_grants
                        WHERE shared_link_id = $1 AND token_hash = $2 AND expires_at > now())",
    )
    .bind(link_id)
    .bind(token_hash.as_slice())
    .fetch_one(pool)
    .await
    .context("查询访问凭证失败")
}
