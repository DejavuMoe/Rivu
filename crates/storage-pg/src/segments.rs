//! 保存的分群（`segments` 表，`platform-and-ops.md` §3.2）：一组带名字的过滤，按站点共享。
//!
//! 只管站点级分群（`owner_user_id IS NULL`）；个人分群暂不支持，这里的读写都不碰它们。
//! 取值的校验（名称 trim 后 1–100 个字符、无控制字符；过滤按查询的规则校验并规范化）在调用方（看板端点）：
//! storage 层只管读写，传进来的值必须已经校验。表的 CHECK 兜底名称长度，部分唯一索引兜底重名。
//! 每站上限与重名检查在事务内先锁 `sites` 行再做，并发添加、改名不会超限或重名。删除是软删除（置 `deleted_at`）；
//! 还有未吊销的共享链接绑定这个分群时不删（链接会因此 404），由调用方告诉用户是哪些链接。

use anyhow::{Context, Result};
use sqlx::{PgPool, Postgres, Row, Transaction, postgres::PgRow};

/// 一个分群（未删除的站点级分群）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SegmentRow {
    pub id: i64,
    pub name: String,
    /// 过滤（JSON 文本，QuerySpec 的 `filters` 数组）。
    pub filter: String,
    /// UTC、毫秒精度的 RFC 3339 文本。
    pub created_at: String,
    pub updated_at: String,
}

fn segment_from_row(r: &PgRow) -> Result<SegmentRow> {
    Ok(SegmentRow {
        id: r.try_get("id")?,
        name: r.try_get("name")?,
        filter: r.try_get("filter")?,
        created_at: r.try_get("created_at")?,
        updated_at: r.try_get("updated_at")?,
    })
}

macro_rules! select_segment {
    ($rest:literal) => {
        concat!(
            "SELECT id, name, filter::text AS filter, ",
            ts_utc!("created_at"),
            " AS created_at, ",
            ts_utc!("updated_at"),
            " AS updated_at FROM segments
             WHERE site_id = $1 AND deleted_at IS NULL AND owner_user_id IS NULL ",
            $rest
        )
    };
}

fn site_param(site_id: u64) -> Result<i64> {
    i64::try_from(site_id).context("site_id 超出范围")
}

/// 站点的分群，按名称（不区分大小写）、id 排。
pub async fn list_segments(pool: &PgPool, site_id: u64) -> Result<Vec<SegmentRow>> {
    let rows = sqlx::query(select_segment!("ORDER BY lower(name), id"))
        .bind(site_param(site_id)?)
        .fetch_all(pool)
        .await
        .context("查询分群失败")?;
    rows.iter().map(segment_from_row).collect()
}

/// 按 id 取站点的一个分群。
pub async fn get_segment(pool: &PgPool, site_id: u64, id: i64) -> Result<Option<SegmentRow>> {
    let row = sqlx::query(select_segment!("AND id = $2"))
        .bind(site_param(site_id)?)
        .bind(id)
        .fetch_optional(pool)
        .await
        .context("查询分群失败")?;
    row.as_ref().map(segment_from_row).transpose()
}

/// 查询展开用：站点里这些 id 的分群的过滤（JSON 文本）。不存在、已删除、属于别的站点的 id 不在结果里。
pub async fn load_filters(pool: &PgPool, site_id: u64, ids: &[i64]) -> Result<Vec<(i64, String)>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let rows = sqlx::query(
        "SELECT id, filter::text AS filter FROM segments
          WHERE site_id = $1 AND id = ANY($2) AND deleted_at IS NULL AND owner_user_id IS NULL",
    )
    .bind(site_param(site_id)?)
    .bind(ids)
    .fetch_all(pool)
    .await
    .context("读取分群失败")?;
    rows.iter()
        .map(|r| Ok((r.try_get("id")?, r.try_get("filter")?)))
        .collect()
}

/// 添加的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AddSegmentOutcome {
    Added(i64),
    /// 站点已有 `limit` 个分群。
    LimitReached {
        limit: usize,
    },
    /// 站点里已有同名（不区分大小写）的分群。
    NameTaken,
    /// 站点不存在或已删除。
    SiteNotFound,
}

/// 锁住站点行（同一站点的添加、改名串行）；站点不存在或已删除返回 `false`。
async fn lock_site(tx: &mut Transaction<'_, Postgres>, site_id: i64) -> Result<bool> {
    let locked: Option<i64> =
        sqlx::query_scalar("SELECT id FROM sites WHERE id = $1 AND deleted_at IS NULL FOR UPDATE")
            .bind(site_id)
            .fetch_optional(&mut **tx)
            .await
            .context("锁定站点行失败")?;
    Ok(locked.is_some())
}

/// 站点里除 `except` 之外有没有同名（不区分大小写）的分群。
async fn name_taken(
    tx: &mut Transaction<'_, Postgres>,
    site_id: i64,
    name: &str,
    except: Option<i64>,
) -> Result<bool> {
    sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM segments
                         WHERE site_id = $1 AND deleted_at IS NULL AND owner_user_id IS NULL
                           AND lower(name) = lower($2) AND id IS DISTINCT FROM $3)",
    )
    .bind(site_id)
    .bind(name)
    .bind(except)
    .fetch_one(&mut **tx)
    .await
    .context("检查分群重名失败")
}

/// 添加一个分群：事务内先锁站点行，再数分群（达到 `max` 拒绝）、查重名，否则插入。
/// `name` 是已校验的名称，`filter` 是已校验、规范化的过滤（JSON 文本）。
pub async fn add_segment(
    pool: &PgPool,
    site_id: u64,
    name: &str,
    filter: &str,
    max: usize,
) -> Result<AddSegmentOutcome> {
    let site_id = site_param(site_id)?;
    let mut tx = pool.begin().await.context("开启事务失败")?;
    if !lock_site(&mut tx, site_id).await? {
        return Ok(AddSegmentOutcome::SiteNotFound);
    }
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM segments
          WHERE site_id = $1 AND deleted_at IS NULL AND owner_user_id IS NULL",
    )
    .bind(site_id)
    .fetch_one(&mut *tx)
    .await
    .context("统计分群失败")?;
    if usize::try_from(count).unwrap_or(usize::MAX) >= max {
        return Ok(AddSegmentOutcome::LimitReached { limit: max });
    }
    if name_taken(&mut tx, site_id, name, None).await? {
        return Ok(AddSegmentOutcome::NameTaken);
    }
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO segments (site_id, name, filter) VALUES ($1, $2, $3::text::jsonb) RETURNING id",
    )
    .bind(site_id)
    .bind(name)
    .bind(filter)
    .fetch_one(&mut *tx)
    .await
    .context("写入分群失败")?;
    tx.commit().await.context("提交事务失败")?;
    Ok(AddSegmentOutcome::Added(id))
}

/// 修改的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateSegmentOutcome {
    Updated,
    /// 不存在、已删除或属于别的站点。
    NotFound,
    /// 新名称与站点里另一个分群重名（不区分大小写）。
    NameTaken,
}

/// 改一个分群的名称和/或过滤（`None` 表示不改）。改名只和别的分群比重名，大小写不同的同一名称可以改。
pub async fn update_segment(
    pool: &PgPool,
    site_id: u64,
    id: i64,
    name: Option<&str>,
    filter: Option<&str>,
) -> Result<UpdateSegmentOutcome> {
    let site_id = site_param(site_id)?;
    let mut tx = pool.begin().await.context("开启事务失败")?;
    if !lock_site(&mut tx, site_id).await? {
        return Ok(UpdateSegmentOutcome::NotFound);
    }
    if let Some(name) = name
        && name_taken(&mut tx, site_id, name, Some(id)).await?
    {
        return Ok(UpdateSegmentOutcome::NameTaken);
    }
    let n = sqlx::query(
        "UPDATE segments SET name = COALESCE($3, name), filter = COALESCE($4::text::jsonb, filter)
          WHERE site_id = $1 AND id = $2 AND deleted_at IS NULL AND owner_user_id IS NULL",
    )
    .bind(site_id)
    .bind(id)
    .bind(name)
    .bind(filter)
    .execute(&mut *tx)
    .await
    .context("修改分群失败")?
    .rows_affected();
    if n == 0 {
        return Ok(UpdateSegmentOutcome::NotFound);
    }
    tx.commit().await.context("提交事务失败")?;
    Ok(UpdateSegmentOutcome::Updated)
}

/// 绑定了某个分群的共享链接（删除被拒时告诉用户是哪些）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkRef {
    pub id: i64,
    /// 链接的名称；没起名为 `None`。
    pub name: Option<String>,
}

/// 删除的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeleteSegmentOutcome {
    Deleted,
    /// 不存在、已删除或属于别的站点。
    NotFound,
    /// 还有未吊销的共享链接（含已过期的）绑定这个分群，按 id 升序。
    InUse(Vec<LinkRef>),
}

/// 软删除一个分群。先锁分群行（与「建共享链接时绑定分群」串行），还有未吊销的共享链接绑定它就不删。
pub async fn delete_segment(pool: &PgPool, site_id: u64, id: i64) -> Result<DeleteSegmentOutcome> {
    let site_id = site_param(site_id)?;
    let mut tx = pool.begin().await.context("开启事务失败")?;
    let found: Option<i64> = sqlx::query_scalar(
        "SELECT id FROM segments
          WHERE site_id = $1 AND id = $2 AND deleted_at IS NULL AND owner_user_id IS NULL
          FOR UPDATE",
    )
    .bind(site_id)
    .bind(id)
    .fetch_optional(&mut *tx)
    .await
    .context("锁定分群行失败")?;
    if found.is_none() {
        return Ok(DeleteSegmentOutcome::NotFound);
    }
    let links = sqlx::query(
        "SELECT id, name FROM shared_links WHERE segment_id = $1 AND deleted_at IS NULL ORDER BY id",
    )
    .bind(id)
    .fetch_all(&mut *tx)
    .await
    .context("查询绑定分群的共享链接失败")?;
    if !links.is_empty() {
        let links = links
            .iter()
            .map(|r| {
                Ok(LinkRef {
                    id: r.try_get("id")?,
                    name: r.try_get("name")?,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        return Ok(DeleteSegmentOutcome::InUse(links));
    }
    sqlx::query("UPDATE segments SET deleted_at = now() WHERE id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await
        .context("删除分群失败")?;
    tx.commit().await.context("提交事务失败")?;
    Ok(DeleteSegmentOutcome::Deleted)
}
