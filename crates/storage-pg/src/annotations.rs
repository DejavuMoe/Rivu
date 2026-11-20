//! 图表注释（`annotations` 表，`platform-and-ops.md` §3.2）：钉在某个站点本地日期上的一句话，按站点共享。
//!
//! 取值的校验（文本 trim 后 1–255 个字符、无控制字符，日期格式与范围）在调用方（看板端点）：storage 层只管读写，
//! 传进来的值必须已经校验。表的 CHECK 兜底长度。每站上限由应用层在事务内先锁 `sites` 行再数注释，并发添加不会超限。
//! 删除是软删除（置 `deleted_at`）。日期以 `YYYY-MM-DD` 文本进出，不依赖 sqlx 的日期类型。

use anyhow::{Context, Result};
use sqlx::{PgPool, Row, postgres::PgRow};

/// 一条注释（未删除），带作者信息。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnnotationRow {
    pub id: i64,
    /// 站点本地日期，`YYYY-MM-DD`。
    pub date: String,
    pub text: String,
    /// 作者的用户 id；账号已删除时为空。
    pub created_by: Option<i64>,
    /// 作者的名称与邮箱：账号已删除（软删除或硬删除）时为空。
    pub author_name: Option<String>,
    pub author_email: Option<String>,
    /// 作者现在是否出现在站点所属组织的成员列表里（组织成员，或该组织某个未删除站点的访客）。
    /// 看板端点据此决定要不要把作者的名称与邮箱给组织成员看：成员页看不到的人，这里也不显示。
    pub author_listed: bool,
    /// UTC、毫秒精度的 RFC 3339 文本。
    pub created_at: String,
    pub updated_at: String,
}

fn annotation_from_row(r: &PgRow) -> Result<AnnotationRow> {
    Ok(AnnotationRow {
        id: r.try_get("id")?,
        date: r.try_get("date")?,
        text: r.try_get("text")?,
        created_by: r.try_get("created_by")?,
        author_name: r.try_get("author_name")?,
        author_email: r.try_get("author_email")?,
        author_listed: r.try_get("author_listed")?,
        created_at: r.try_get("created_at")?,
        updated_at: r.try_get("updated_at")?,
    })
}

/// `SELECT … FROM annotations` 加上给定的条件（`concat!` 里用，保持 `&'static str`）。
macro_rules! select_annotation {
    ($rest:literal) => {
        concat!(
            "SELECT a.id, to_char(a.date, 'YYYY-MM-DD') AS date, a.text, a.created_by,
                    u.name AS author_name, u.email::text AS author_email,
                    (u.id IS NOT NULL AND (
                        EXISTS (SELECT 1 FROM memberships m
                                 WHERE m.organization_id = s.organization_id AND m.user_id = u.id
                                   AND m.deleted_at IS NULL)
                        OR EXISTS (SELECT 1 FROM site_memberships g
                                     JOIN sites gs ON gs.id = g.site_id AND gs.deleted_at IS NULL
                                    WHERE gs.organization_id = s.organization_id AND g.user_id = u.id
                                      AND g.deleted_at IS NULL))) AS author_listed, ",
            ts_utc!("a.created_at"),
            " AS created_at, ",
            ts_utc!("a.updated_at"),
            " AS updated_at
             FROM annotations a
             JOIN sites s ON s.id = a.site_id
             LEFT JOIN users u ON u.id = a.created_by AND u.deleted_at IS NULL ",
            $rest
        )
    };
}

fn site_param(site_id: u64) -> Result<i64> {
    i64::try_from(site_id).context("site_id 超出范围")
}

/// 一个站点的未删除注释，按日期、id 排；`from`、`to`（含两端，`YYYY-MM-DD`）给出时只取范围内的。
pub async fn list_annotations(
    pool: &PgPool,
    site_id: u64,
    from: Option<&str>,
    to: Option<&str>,
) -> Result<Vec<AnnotationRow>> {
    let rows = sqlx::query(select_annotation!(
        "WHERE a.site_id = $1 AND a.deleted_at IS NULL
           AND ($2::text IS NULL OR a.date >= $2::text::date)
           AND ($3::text IS NULL OR a.date <= $3::text::date)
         ORDER BY a.date, a.id"
    ))
    .bind(site_param(site_id)?)
    .bind(from)
    .bind(to)
    .fetch_all(pool)
    .await
    .context("查询注释失败")?;
    rows.iter().map(annotation_from_row).collect()
}

/// 按 id 取一条未删除的注释。
pub async fn get_annotation(pool: &PgPool, site_id: u64, id: i64) -> Result<Option<AnnotationRow>> {
    let row = sqlx::query(select_annotation!(
        "WHERE a.site_id = $1 AND a.id = $2 AND a.deleted_at IS NULL"
    ))
    .bind(site_param(site_id)?)
    .bind(id)
    .fetch_optional(pool)
    .await
    .context("查询注释失败")?;
    row.as_ref().map(annotation_from_row).transpose()
}

/// 添加的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AddAnnotationOutcome {
    Added(i64),
    /// 站点已有 `limit` 条注释。
    LimitReached {
        limit: usize,
    },
    /// 站点不存在或已删除。
    SiteNotFound,
}

/// 添加一条注释：事务内先锁站点行，再数未删除的注释（达到 `max` 拒绝），否则插入。
/// `date` 是已校验的 `YYYY-MM-DD`，`text` 是已校验的文本。
pub async fn add_annotation(
    pool: &PgPool,
    site_id: u64,
    date: &str,
    text: &str,
    created_by: i64,
    max: usize,
) -> Result<AddAnnotationOutcome> {
    let site_id = site_param(site_id)?;
    let mut tx = pool.begin().await.context("开启事务失败")?;
    let locked: Option<i64> =
        sqlx::query_scalar("SELECT id FROM sites WHERE id = $1 AND deleted_at IS NULL FOR UPDATE")
            .bind(site_id)
            .fetch_optional(&mut *tx)
            .await
            .context("锁定站点行失败")?;
    if locked.is_none() {
        return Ok(AddAnnotationOutcome::SiteNotFound);
    }
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM annotations WHERE site_id = $1 AND deleted_at IS NULL",
    )
    .bind(site_id)
    .fetch_one(&mut *tx)
    .await
    .context("统计注释失败")?;
    if usize::try_from(count).unwrap_or(usize::MAX) >= max {
        return Ok(AddAnnotationOutcome::LimitReached { limit: max });
    }
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO annotations (site_id, date, text, created_by)
         VALUES ($1, $2::text::date, $3, $4) RETURNING id",
    )
    .bind(site_id)
    .bind(date)
    .bind(text)
    .bind(created_by)
    .fetch_one(&mut *tx)
    .await
    .context("写入注释失败")?;
    tx.commit().await.context("提交事务失败")?;
    Ok(AddAnnotationOutcome::Added(id))
}

/// 改一条注释的文本和/或日期（`None` 表示不改）；返回是否改到了（不存在、已删除或属于别的站点都返回 `false`）。
pub async fn update_annotation(
    pool: &PgPool,
    site_id: u64,
    id: i64,
    text: Option<&str>,
    date: Option<&str>,
) -> Result<bool> {
    let n = sqlx::query(
        "UPDATE annotations SET text = COALESCE($3, text), date = COALESCE($4::text::date, date)
          WHERE site_id = $1 AND id = $2 AND deleted_at IS NULL",
    )
    .bind(site_param(site_id)?)
    .bind(id)
    .bind(text)
    .bind(date)
    .execute(pool)
    .await
    .context("修改注释失败")?
    .rows_affected();
    Ok(n > 0)
}

/// 按 id 软删除一条注释；返回是否删除了。
pub async fn remove_annotation(pool: &PgPool, site_id: u64, id: i64) -> Result<bool> {
    let n = sqlx::query(
        "UPDATE annotations SET deleted_at = now()
          WHERE site_id = $1 AND id = $2 AND deleted_at IS NULL",
    )
    .bind(site_param(site_id)?)
    .bind(id)
    .execute(pool)
    .await
    .context("删除注释失败")?
    .rows_affected();
    Ok(n > 0)
}
