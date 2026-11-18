//! 外部数据导入的记录（`site_imports`）。数据本身在 ClickHouse 的日汇总层（`origin = 'import'`，`import_id = id`），
//! 这里只记录来源、状态与实际写入的日期范围；状态为 `completed` 的导入才参与查询。

use anyhow::{Context, Result};
use sqlx::{PgConnection, PgPool, Row};

fn id_i64(id: u64) -> Result<i64> {
    i64::try_from(id).context("id 超出范围")
}

/// 一条导入记录（看板的导入列表）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportRow {
    pub id: i64,
    pub source: String,
    /// `importing` | `completed` | `failed`
    pub status: String,
    pub file_name: String,
    /// 实际写入的站点本地日期（`YYYY-MM-DD`）；导入完成前为空。
    pub start_date: Option<String>,
    pub end_date: Option<String>,
    pub rows_imported: i64,
    /// 失败原因的代码（看板翻译用）；`error` 是详细信息。
    pub error_code: Option<String>,
    pub error: Option<String>,
    /// 导入的人（CLI 导入或账号已删除为 `None`）。
    pub imported_by: Option<String>,
    pub created_at: String,
    pub completed_at: Option<String>,
}

macro_rules! import_columns {
    () => {
        concat!(
            "id, source, status, file_name, to_char(start_date, 'YYYY-MM-DD') AS start_date, ",
            "to_char(end_date, 'YYYY-MM-DD') AS end_date, rows_imported, error_code, error, ",
            "(SELECT coalesce(u.name, u.email) FROM users u WHERE u.id = site_imports.imported_by) AS imported_by_name, ",
            ts_sec!("created_at"),
            " AS created_at, ",
            ts_sec!("completed_at"),
            " AS completed_at"
        )
    };
}

fn import_row(r: &sqlx::postgres::PgRow) -> Result<ImportRow> {
    Ok(ImportRow {
        id: r.try_get("id")?,
        source: r.try_get("source")?,
        status: r.try_get("status")?,
        file_name: r.try_get("file_name")?,
        start_date: r.try_get("start_date")?,
        end_date: r.try_get("end_date")?,
        rows_imported: r.try_get("rows_imported")?,
        error_code: r.try_get("error_code")?,
        error: r.try_get("error")?,
        imported_by: r.try_get("imported_by_name")?,
        created_at: r.try_get("created_at")?,
        completed_at: r.try_get("completed_at")?,
    })
}

/// 开始一次导入（状态 `importing`），返回记录 id（也是 ClickHouse 里的 `import_id`）；
/// 站点已有进行中的导入时返回 `None`。调用方先锁站点行并读取本次导入使用的时区。
pub async fn create(
    db: &mut PgConnection,
    site_id: u64,
    source: &str,
    file_name: &str,
    imported_by: Option<i64>,
) -> Result<Option<i64>> {
    sqlx::query_scalar(
        "INSERT INTO site_imports (site_id, source, file_name, imported_by) VALUES ($1, $2, $3, $4)
         ON CONFLICT (site_id) WHERE status = 'importing' DO NOTHING RETURNING id",
    )
    .bind(id_i64(site_id)?)
    .bind(source)
    .bind(file_name)
    .bind(imported_by)
    .fetch_optional(db)
    .await
    .context("写入导入记录失败")
}

/// 同一个 PG 快照里检查尚未完成的导入和已完成导入的最后一天。
pub async fn blocks_timezone_change(
    db: &mut PgConnection,
    site_id: u64,
    native_from: Option<String>,
) -> Result<bool> {
    sqlx::query_scalar(
        "SELECT EXISTS (
             SELECT 1 FROM site_imports WHERE site_id = $1 AND
               (status = 'importing' OR (status = 'completed' AND end_date >= $2::text::date))
         )",
    )
    .bind(id_i64(site_id)?)
    .bind(native_from)
    .fetch_one(db)
    .await
    .context("检查导入与目标时区的原生数据是否重叠失败")
}

/// 导入完成：记下实际写入的日期范围与行数。
pub async fn complete(pool: &PgPool, id: i64, start: &str, end: &str, rows: i64) -> Result<()> {
    sqlx::query(
        "UPDATE site_imports SET status = 'completed', start_date = $2::date, end_date = $3::date,
                rows_imported = $4, error_code = NULL, error = NULL, completed_at = now()
         WHERE id = $1",
    )
    .bind(id)
    .bind(start)
    .bind(end)
    .bind(rows)
    .execute(pool)
    .await
    .context("更新导入记录失败")?;
    Ok(())
}

/// 导入失败（已写入的汇总行由调用方删除）。`code` 见表的 `error_code` 约束。
pub async fn fail(pool: &PgPool, id: i64, code: &str, error: &str) -> Result<()> {
    sqlx::query(
        "UPDATE site_imports SET status = 'failed', error_code = $2, error = $3, completed_at = now() WHERE id = $1",
    )
    .bind(id)
    .bind(code)
    .bind(error)
    .execute(pool)
    .await
    .context("更新导入记录失败")?;
    Ok(())
}

/// 站点的导入，新的在前。
pub async fn list(pool: &PgPool, site_id: u64) -> Result<Vec<ImportRow>> {
    let rows = sqlx::query(concat!(
        "SELECT ",
        import_columns!(),
        " FROM site_imports WHERE site_id = $1 ORDER BY site_imports.created_at DESC, id DESC"
    ))
    .bind(id_i64(site_id)?)
    .fetch_all(pool)
    .await
    .context("查询导入记录失败")?;
    rows.iter().map(import_row).collect()
}

pub async fn get(pool: &PgPool, site_id: u64, id: i64) -> Result<Option<ImportRow>> {
    let row = sqlx::query(concat!(
        "SELECT ",
        import_columns!(),
        " FROM site_imports WHERE site_id = $1 AND id = $2"
    ))
    .bind(id_i64(site_id)?)
    .bind(id)
    .fetch_optional(pool)
    .await
    .context("查询导入记录失败")?;
    row.as_ref().map(import_row).transpose()
}

/// 删除导入记录；返回是否删除了。ClickHouse 里的汇总行由调用方先删。
pub async fn delete(pool: &PgPool, site_id: u64, id: i64) -> Result<bool> {
    let n = sqlx::query("DELETE FROM site_imports WHERE site_id = $1 AND id = $2")
        .bind(id_i64(site_id)?)
        .bind(id)
        .execute(pool)
        .await
        .context("删除导入记录失败")?
        .rows_affected();
    Ok(n > 0)
}

/// 已完成的导入覆盖的日期（最早的开始、最晚的结束，`YYYY-MM-DD`）；没有为 `None`。查询时用。
pub async fn completed_range(pool: &PgPool, site_id: u64) -> Result<Option<(String, String)>> {
    let row = sqlx::query(
        "SELECT to_char(min(start_date), 'YYYY-MM-DD') AS a, to_char(max(end_date), 'YYYY-MM-DD') AS b
         FROM site_imports WHERE site_id = $1 AND status = 'completed'",
    )
    .bind(id_i64(site_id)?)
    .fetch_one(pool)
    .await
    .context("查询导入范围失败")?;
    let a: Option<String> = row.try_get("a")?;
    let b: Option<String> = row.try_get("b")?;
    Ok(a.zip(b))
}

/// 与 `[start, end]` 重叠的已完成导入（返回其中一个的 id）。同一段日期导入两次会重复计数，所以要拒绝。
pub async fn overlapping(
    pool: &PgPool,
    site_id: u64,
    start: &str,
    end: &str,
) -> Result<Option<i64>> {
    sqlx::query_scalar(
        "SELECT id FROM site_imports WHERE site_id = $1 AND status = 'completed'
           AND start_date <= $3::date AND end_date >= $2::date ORDER BY id LIMIT 1",
    )
    .bind(id_i64(site_id)?)
    .bind(start)
    .bind(end)
    .fetch_optional(pool)
    .await
    .context("查询重叠的导入失败")
}

/// 进程重启时仍是 `importing` 的导入：它们写到一半，由调用方删除汇总行后置为失败。
pub async fn interrupted(pool: &PgPool) -> Result<Vec<(i64, u64)>> {
    let rows = sqlx::query("SELECT id, site_id FROM site_imports WHERE status = 'importing'")
        .fetch_all(pool)
        .await
        .context("查询未完成的导入失败")?;
    rows.iter()
        .map(|r| {
            let site: i64 = r.try_get("site_id")?;
            Ok((
                r.try_get("id")?,
                u64::try_from(site).context("site_id 为负数")?,
            ))
        })
        .collect()
}
