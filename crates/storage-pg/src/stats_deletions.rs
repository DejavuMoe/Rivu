//! 统计数据删除意图（`pending_stats_deletions`，`platform-and-ops.md` §3.3、§6）：站点软删除时写入
//! `scope='site'` 的意图（`not_before` = 删除时刻 + 恢复窗口），恢复站点时把它置为 `cancelled`；
//! 按日期范围删除写入 `scope='date_range'` 的意图（`not_before = now()`）。
//! 执行在 `rivu-jobs` 的 `ch_delete_batch`：领取到期意图（`FOR UPDATE SKIP LOCKED`），逐分区删除并记入
//! `partitions_done`，重试时跳过已提交的进度。
//!
//! 日期一律以 `YYYY-MM-DD` 文本进出（站点本地日期；本 crate 不让 sqlx 依赖 jiff）。

use anyhow::{Context, Result};
use sqlx::{PgPool, Row};

use crate::sites::SiteOwner;

/// 领取后的租约：`not_before` 推到「现在 + 租约」，别的进程在租约内不会再领取同一条；
/// 每完成一个分区续一次（[`mark_partition_done`]）。进程崩溃后租约到期，任意进程从已提交的进度续跑。
pub const CLAIM_LEASE_SECS: i64 = 3600;

/// 失败多少次后置为 `failed`（不再自动重试）。
pub const MAX_ATTEMPTS: i32 = 8;

fn id_i64(site_id: u64) -> Result<i64> {
    i64::try_from(site_id).context("site_id 超出范围")
}

/// 软删除站点并在同一事务里写入 `scope='site'` 的删除意图（`not_before = now() + restore_days 天`）。
/// 站点不存在或已删除返回 `false`。部分唯一索引 `pending_stats_deletions_site_once`
/// 保证一个站点只有一条未取消的站点意图（恢复后意图已是 `cancelled`，再删除会插入新的一条）。
pub async fn soft_delete_site(
    pool: &PgPool,
    site_id: u64,
    requested_by: Option<i64>,
    restore_days: u32,
) -> Result<bool> {
    let id = id_i64(site_id)?;
    let mut tx = pool.begin().await.context("开启事务失败")?;
    let n = sqlx::query("UPDATE sites SET deleted_at = now() WHERE id = $1 AND deleted_at IS NULL")
        .bind(id)
        .execute(&mut *tx)
        .await
        .context("软删除站点失败")?
        .rows_affected();
    if n == 0 {
        return Ok(false);
    }
    sqlx::query(
        "INSERT INTO pending_stats_deletions (site_id, scope, not_before, requested_by)
         VALUES ($1, 'site', now() + make_interval(days => $2), $3)
         ON CONFLICT (site_id) WHERE scope = 'site' AND status <> 'cancelled' DO NOTHING",
    )
    .bind(id)
    .bind(i32::try_from(restore_days).context("恢复窗口超出范围")?)
    .bind(requested_by)
    .execute(&mut *tx)
    .await
    .context("写入站点删除意图失败")?;
    tx.commit().await.context("提交事务失败")?;
    Ok(true)
}

/// 已删除、仍在恢复窗口内的站点（「已删除的站点」列表的一行）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeletedSiteRow {
    pub id: u64,
    pub site_key: String,
    pub name: String,
    /// 白名单里最早的一条（建站时的主域名）；没有则为空。
    pub domain: Option<String>,
    pub deleted_at: String,
    /// 恢复窗口截止（站点删除意图的 `not_before`），之后统计数据被清除。
    pub purge_at: String,
}

/// 组织里已软删除、站点删除意图仍是 `pending` 且 `not_before > now()` 的站点，按删除时间倒序。
pub async fn list_deleted_sites(pool: &PgPool, org_id: i64) -> Result<Vec<DeletedSiteRow>> {
    let rows = sqlx::query(concat!(
        "SELECT s.id, s.site_key, s.name,
                (SELECT h.pattern FROM site_hostnames h
                  WHERE h.site_id = s.id AND h.deleted_at IS NULL
                  ORDER BY h.created_at, h.id LIMIT 1) AS domain, ",
        ts_sec!("s.deleted_at"),
        " AS deleted_at, ",
        ts_sec!("p.not_before"),
        " AS purge_at
         FROM sites s
         JOIN organizations o ON o.id = s.organization_id AND o.deleted_at IS NULL
         JOIN pending_stats_deletions p ON p.site_id = s.id AND p.scope = 'site'
                                       AND p.status = 'pending' AND p.not_before > now()
         WHERE s.organization_id = $1 AND s.deleted_at IS NOT NULL
         ORDER BY s.deleted_at DESC, s.id DESC"
    ))
    .bind(org_id)
    .fetch_all(pool)
    .await
    .context("列出已删除站点失败")?;
    rows.iter()
        .map(|r| {
            let id: i64 = r.try_get("id")?;
            Ok(DeletedSiteRow {
                id: u64::try_from(id).context("sites.id 为负数")?,
                site_key: r.try_get("site_key")?,
                name: r.try_get("name")?,
                domain: r.try_get("domain")?,
                deleted_at: r.try_get("deleted_at")?,
                purge_at: r.try_get("purge_at")?,
            })
        })
        .collect()
}

/// 按 id 或 `site_key` 查一个**已软删除**、组织未删除的站点（恢复端点用；可见性由调用方按组织成员关系判定）。
pub async fn find_deleted_site(
    pool: &PgPool,
    id: Option<u64>,
    site_key: Option<&str>,
) -> Result<Option<SiteOwner>> {
    let id = id.map(id_i64).transpose()?;
    let row = sqlx::query(
        "SELECT s.id, s.site_key, s.organization_id, s.is_public
         FROM sites s JOIN organizations o ON o.id = s.organization_id AND o.deleted_at IS NULL
         WHERE s.deleted_at IS NOT NULL AND (s.id = $1 OR s.site_key = $2)",
    )
    .bind(id)
    .bind(site_key)
    .fetch_optional(pool)
    .await
    .context("查询已删除站点失败")?;
    row.map(|r| {
        let id: i64 = r.try_get("id")?;
        Ok(SiteOwner {
            id: u64::try_from(id).context("sites.id 为负数")?,
            site_key: r.try_get("site_key")?,
            organization_id: r.try_get("organization_id")?,
            is_public: r.try_get("is_public")?,
        })
    })
    .transpose()
}

/// 恢复站点的结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestoreOutcome {
    Restored,
    /// 站点不存在或没有被删除。
    NotFound,
    /// 恢复窗口已过、清除已经开始或完成，或没有可撤销的站点删除意图。
    NotRestorable,
}

/// 恢复站点：一个事务里锁站点行与站点删除意图，要求意图仍是 `pending` 且 `not_before > now()`，
/// 然后把意图置为 `cancelled`、清掉 `sites.deleted_at`（`updated_at` 由触发器更新，站点快照下一轮重载即恢复）。
pub async fn restore_site(pool: &PgPool, site_id: u64) -> Result<RestoreOutcome> {
    let id = id_i64(site_id)?;
    let mut tx = pool.begin().await.context("开启事务失败")?;
    let locked: Option<i64> = sqlx::query_scalar(
        "SELECT id FROM sites WHERE id = $1 AND deleted_at IS NOT NULL FOR UPDATE",
    )
    .bind(id)
    .fetch_optional(&mut *tx)
    .await
    .context("锁定站点失败")?;
    if locked.is_none() {
        return Ok(RestoreOutcome::NotFound);
    }
    let intent: Option<i64> = sqlx::query_scalar(
        "SELECT id FROM pending_stats_deletions
         WHERE site_id = $1 AND scope = 'site' AND status = 'pending' AND not_before > now()
         FOR UPDATE",
    )
    .bind(id)
    .fetch_optional(&mut *tx)
    .await
    .context("锁定站点删除意图失败")?;
    let Some(intent) = intent else {
        return Ok(RestoreOutcome::NotRestorable);
    };
    sqlx::query("UPDATE pending_stats_deletions SET status = 'cancelled' WHERE id = $1")
        .bind(intent)
        .execute(&mut *tx)
        .await
        .context("取消站点删除意图失败")?;
    sqlx::query("UPDATE sites SET deleted_at = NULL WHERE id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await
        .context("恢复站点失败")?;
    tx.commit().await.context("提交事务失败")?;
    Ok(RestoreOutcome::Restored)
}

/// 按日期范围删除的一条意图（列表与创建响应）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DateRangeRow {
    pub id: i64,
    /// 站点本地日期 `YYYY-MM-DD`。
    pub from: String,
    pub to: String,
    pub status: String,
    pub created_at: String,
    pub completed_at: Option<String>,
    pub timezone_unproven: bool,
}

/// 日期范围意图的列（`concat!` 里用，保持 `&'static str`）。
macro_rules! date_range_columns {
    () => {
        concat!(
            "id, to_char(date_from, 'YYYY-MM-DD') AS date_from, to_char(date_to, 'YYYY-MM-DD') AS date_to, status, ",
            ts_sec!("created_at"),
            " AS created_at, ",
            ts_sec!("completed_at"),
            " AS completed_at, COALESCE(starts_with(last_error, 'stats_deletion_timezone_unproven:'), false) AS timezone_unproven"
        )
    };
}

fn date_range_row(r: &sqlx::postgres::PgRow) -> Result<DateRangeRow> {
    Ok(DateRangeRow {
        id: r.try_get("id")?,
        from: r.try_get("date_from")?,
        to: r.try_get("date_to")?,
        status: r.try_get("status")?,
        created_at: r.try_get("created_at")?,
        completed_at: r.try_get("completed_at")?,
        timezone_unproven: r.try_get("timezone_unproven")?,
    })
}

/// 写入一条 `scope='date_range'` 的意图（`not_before = now()`，下一轮 `ch_delete_batch` 执行）。
/// 调用方持站点行锁，校验本地日期并按锁内读取的时区冻结 UTC 半开区间。
pub async fn create_date_range(
    db: &mut sqlx::PgConnection,
    site_id: u64,
    from: &str,
    to: &str,
    timezone: &str,
    utc: (i64, i64),
    requested_by: Option<i64>,
) -> Result<DateRangeRow> {
    let row = sqlx::query(concat!(
        "INSERT INTO pending_stats_deletions
            (site_id, scope, date_from, date_to, timezone, utc_start_ms, utc_end_ms, requested_by)
         VALUES ($1, 'date_range', $2::date, $3::date, $4, $5, $6, $7) RETURNING ",
        date_range_columns!()
    ))
    .bind(id_i64(site_id)?)
    .bind(from)
    .bind(to)
    .bind(timezone)
    .bind(utc.0)
    .bind(utc.1)
    .bind(requested_by)
    .fetch_one(db)
    .await
    .context("写入日期范围删除意图失败")?;
    date_range_row(&row)
}

/// Check under the site row lock. An unfinished intent can still delete input, and
/// a completed erase near the rebuild range can have removed inputs for adjacent days.
pub async fn blocks_timezone_change(
    db: &mut sqlx::PgConnection,
    site_id: u64,
    rebuild_since: Option<String>,
) -> Result<bool> {
    sqlx::query_scalar(
        "SELECT EXISTS (
             SELECT 1 FROM pending_stats_deletions
             WHERE site_id = $1 AND status <> 'cancelled' AND NOT rejected_before_write
               AND (status <> 'done' OR
                    ($2::text IS NOT NULL AND (scope = 'site' OR date_to >= $2::text::date)))
         )",
    )
    .bind(id_i64(site_id)?)
    .bind(rebuild_since)
    .fetch_one(db)
    .await
    .context("检查影响时区变更的统计删除意图失败")
}

/// Completed, timezone-proven erases that could leave holes between native day markers.
pub async fn erased_rollup_ranges(
    db: &mut sqlx::PgConnection,
    site_id: u64,
    timezone: &str,
    from: &str,
    to: &str,
) -> Result<Vec<(String, String, i64, i64)>> {
    sqlx::query_as(
        "SELECT DISTINCT to_char(date_from, 'YYYY-MM-DD'), to_char(date_to, 'YYYY-MM-DD'),
                         utc_start_ms, utc_end_ms
         FROM pending_stats_deletions
         WHERE site_id = $1 AND timezone = $2 AND scope = 'date_range' AND status = 'done'
           AND utc_start_ms IS NOT NULL AND utc_end_ms IS NOT NULL
           AND date_from <= $4::text::date AND date_to >= $3::text::date",
    )
    .bind(id_i64(site_id)?)
    .bind(timezone)
    .bind(from)
    .bind(to)
    .fetch_all(db)
    .await
    .context("查询已擦除的汇总日期失败")
}

/// A claim is committed before any ClickHouse write. Even an exhausted or
/// disconnected attempt can have submitted a mutation without a checkpoint.
pub async fn maintenance_fences(
    db: &mut sqlx::PgConnection,
    site_id: Option<u64>,
) -> Result<Vec<u64>> {
    let rows: Vec<i64> = sqlx::query_scalar(
        "SELECT DISTINCT site_id FROM pending_stats_deletions
         WHERE ($1::bigint IS NULL OR site_id = $1)
           AND status IN ('running', 'failed') AND NOT rejected_before_write",
    )
    .bind(site_id.map(id_i64).transpose()?)
    .fetch_all(db)
    .await
    .context("查询未决统计删除意图失败")?;
    rows.into_iter()
        .map(|id| u64::try_from(id).context("site_id 为负数"))
        .collect()
}

/// 站点最近的日期范围删除意图，新的在前（最多 `limit` 条）。
pub async fn list_date_ranges(
    pool: &PgPool,
    site_id: u64,
    limit: i64,
) -> Result<Vec<DateRangeRow>> {
    let rows = sqlx::query(concat!(
        "SELECT ",
        date_range_columns!(),
        " FROM pending_stats_deletions
         WHERE site_id = $1 AND scope = 'date_range'
         ORDER BY pending_stats_deletions.created_at DESC, id DESC LIMIT $2"
    ))
    .bind(id_i64(site_id)?)
    .bind(limit)
    .fetch_all(pool)
    .await
    .context("列出日期范围删除意图失败")?;
    rows.iter().map(date_range_row).collect()
}

// ------------------------------------------------------------------------------------------
// ch_delete_batch：领取与进度
// ------------------------------------------------------------------------------------------

/// 领取到的一条意图。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaimedDeletion {
    pub id: i64,
    pub site_id: u64,
    /// `site` | `date_range`。
    pub scope: String,
    pub date_from: Option<String>,
    pub date_to: Option<String>,
    pub timezone: Option<String>,
    pub utc_start_ms: Option<i64>,
    pub utc_end_ms: Option<i64>,
    pub partitions_done: Vec<String>,
    /// 含本次领取。
    pub attempts: i32,
}

/// 领取至多 `limit` 条到期意图（`status IN ('pending','running') AND not_before <= now()`，`FOR UPDATE SKIP LOCKED`）：
/// 置为 `running`、`attempts + 1`，并把 `not_before` 推到「现在 + [`CLAIM_LEASE_SECS`]」作为租约。
/// 多个进程同时领取不会拿到同一条；崩溃留下的 `running` 在租约到期后重新可领取。
pub async fn claim_due(pool: &PgPool, limit: i64) -> Result<Vec<ClaimedDeletion>> {
    let rows = sqlx::query(
        "WITH claimed AS (
             UPDATE pending_stats_deletions p
                SET status = 'running', attempts = p.attempts + 1, rejected_before_write = false,
                    not_before = now() + make_interval(secs => $2)
              WHERE p.id IN (
                    SELECT id FROM pending_stats_deletions
                     WHERE status IN ('pending', 'running') AND not_before <= now()
                     ORDER BY not_before, id
                     LIMIT $1
                     FOR UPDATE SKIP LOCKED)
             RETURNING p.id, p.site_id, p.scope, p.date_from, p.date_to,
                       p.timezone, p.utc_start_ms, p.utc_end_ms, p.partitions_done, p.attempts)
         SELECT c.id, c.site_id, c.scope,
                to_char(c.date_from, 'YYYY-MM-DD') AS date_from,
                to_char(c.date_to, 'YYYY-MM-DD') AS date_to,
                c.timezone, c.utc_start_ms, c.utc_end_ms, c.partitions_done, c.attempts
           FROM claimed c
          ORDER BY c.id",
    )
    .bind(limit)
    .bind(CLAIM_LEASE_SECS as f64)
    .fetch_all(pool)
    .await
    .context("领取统计数据删除意图失败")?;
    rows.iter()
        .map(|r| {
            let site_id: i64 = r.try_get("site_id")?;
            Ok(ClaimedDeletion {
                id: r.try_get("id")?,
                site_id: u64::try_from(site_id).context("site_id 为负数")?,
                scope: r.try_get("scope")?,
                date_from: r.try_get("date_from")?,
                date_to: r.try_get("date_to")?,
                timezone: r.try_get("timezone")?,
                utc_start_ms: r.try_get("utc_start_ms")?,
                utc_end_ms: r.try_get("utc_end_ms")?,
                partitions_done: r.try_get("partitions_done")?,
                attempts: r.try_get("attempts")?,
            })
        })
        .collect()
}

/// Lock the active claim after locking the site. Superseded claims must not delete.
pub async fn lock_claim(db: &mut sqlx::PgConnection, intent: &ClaimedDeletion) -> Result<bool> {
    let id: Option<i64> = sqlx::query_scalar(
        "SELECT id FROM pending_stats_deletions
         WHERE id = $1 AND attempts = $2 AND status = 'running' FOR UPDATE",
    )
    .bind(intent.id)
    .bind(intent.attempts)
    .fetch_optional(db)
    .await
    .context("锁定统计数据删除意图失败")?;
    Ok(id.is_some())
}

/// 记一个已完成的分区（幂等）并续租约。
pub async fn mark_partition_done(
    db: &mut sqlx::PgConnection,
    id: i64,
    partition: &str,
) -> Result<()> {
    sqlx::query(
        "UPDATE pending_stats_deletions
            SET partitions_done = CASE WHEN $2 = ANY(partitions_done) THEN partitions_done
                                       ELSE array_append(partitions_done, $2) END,
                not_before = clock_timestamp() + make_interval(secs => $3)
          WHERE id = $1 AND status = 'running'",
    )
    .bind(id)
    .bind(partition)
    .bind(CLAIM_LEASE_SECS as f64)
    .execute(db)
    .await
    .context("记录已完成分区失败")?;
    Ok(())
}

/// 全部分区完成：`done` + `completed_at`。
pub async fn mark_done(db: &mut sqlx::PgConnection, id: i64) -> Result<()> {
    sqlx::query(
        "UPDATE pending_stats_deletions
            SET status = 'done', completed_at = clock_timestamp(), last_error = NULL
          WHERE id = $1 AND status = 'running'",
    )
    .bind(id)
    .execute(db)
    .await
    .context("标记删除意图完成失败")?;
    Ok(())
}

/// Stop retrying ambiguous dates. Prior attempts may already have sent a mutation.
pub async fn reject_before_delete(db: &mut sqlx::PgConnection, id: i64, error: &str) -> Result<()> {
    sqlx::query(
        "UPDATE pending_stats_deletions
            SET status = 'failed', completed_at = clock_timestamp(), last_error = $2,
                rejected_before_write = attempts = 1 AND cardinality(partitions_done) = 0
          WHERE id = $1 AND status = 'running'",
    )
    .bind(id)
    .bind(error)
    .execute(db)
    .await
    .context("记录日期范围无法验证失败")?;
    Ok(())
}

/// 一次执行失败：记 `last_error`；`attempts >= MAX_ATTEMPTS` 时置为 `failed`，否则保持 `running`
/// 并把 `not_before` 推到 `now() + backoff_secs` 之后重试（保持 `running`：已经开始清除的站点不能再恢复）。
/// 返回是否已置为 `failed`。
pub async fn mark_attempt_failed(
    pool: &PgPool,
    id: i64,
    attempts: i32,
    error: &str,
    backoff_secs: i64,
) -> Result<bool> {
    let failed = attempts >= MAX_ATTEMPTS;
    // 错误文本截断，避免把很长的 ClickHouse 响应写进表里
    let error: String = error.chars().take(2000).collect();
    sqlx::query(
        "UPDATE pending_stats_deletions
            SET last_error = $2,
                status = CASE WHEN $3 THEN 'failed' ELSE status END,
                completed_at = CASE WHEN $3 THEN now() ELSE completed_at END,
                not_before = CASE WHEN $3 THEN not_before ELSE now() + make_interval(secs => $4) END
          WHERE id = $1 AND status = 'running' AND attempts = $5",
    )
    .bind(id)
    .bind(&error)
    .bind(failed)
    .bind(backoff_secs as f64)
    .bind(attempts)
    .execute(pool)
    .await
    .context("记录删除失败失败")?;
    Ok(failed)
}

/// 失败后的退避：5 分钟起、每次翻倍、最多 6 小时（`attempts` 含本次）。
pub fn backoff_secs(attempts: i32) -> i64 {
    let exp = u32::try_from(attempts.saturating_sub(1))
        .unwrap_or(0)
        .min(10);
    (300_i64 << exp).min(6 * 3600)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_doubles_and_is_capped() {
        assert_eq!(backoff_secs(1), 300);
        assert_eq!(backoff_secs(2), 600);
        assert_eq!(backoff_secs(3), 1200);
        assert_eq!(backoff_secs(8), 6 * 3600);
        assert_eq!(backoff_secs(100), 6 * 3600);
        assert_eq!(backoff_secs(0), 300);
    }
}
