//! 屏蔽规则（`shield_rules` 表，`platform-and-ops.md` §3.2–§3.3）：加载、增删。
//!
//! 取值的校验与规范化（IP / CIDR 文本、国家码、页面通配）在 `rivu-ingest::shield`：storage 层只管读写，
//! 传进来的 `value` 必须已经规范化。表的 CHECK 约束兜底：`type` 取 `ip | cidr | country | hostname | page`，
//! `action` 取 `deny | allow`，`country` 的值必须是两个大写字母。
//!
//! 每站上限由应用层在事务内先锁 `sites` 行再数规则（[`add_rule`]），并发添加不会超限。
//! 删除是软删除（置 `deleted_at`）：缓存按 `deleted_at` 摘除条目，部分唯一索引允许同值规则再次添加。

use anyhow::{Context, Result};
use sqlx::{PgPool, Row, postgres::PgRow};

/// 一条规则（未删除）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShieldRuleRow {
    /// UUID 文本。
    pub id: String,
    pub site_id: u64,
    /// `ip | cidr | country | hostname | page`
    pub kind: String,
    /// `deny | allow`
    pub action: String,
    pub value: String,
    pub description: Option<String>,
}

fn rule_from_row(r: &PgRow) -> Result<ShieldRuleRow> {
    let site_id: i64 = r.try_get("site_id")?;
    Ok(ShieldRuleRow {
        id: r.try_get("id")?,
        site_id: u64::try_from(site_id).context("shield_rules.site_id 为负数")?,
        kind: r.try_get("type")?,
        action: r.try_get("action")?,
        value: r.try_get("value")?,
        description: r.try_get("description")?,
    })
}

/// 全部未删除站点的未删除规则（站点快照用）。
pub async fn load_all_rules(pool: &PgPool) -> Result<Vec<ShieldRuleRow>> {
    let rows = sqlx::query(
        "SELECT r.id::text AS id, r.site_id, r.type, r.action, r.value, r.description
         FROM shield_rules r
         WHERE r.deleted_at IS NULL
           AND EXISTS (SELECT 1 FROM sites s WHERE s.id = r.site_id AND s.deleted_at IS NULL)
         ORDER BY r.site_id, r.created_at, r.id",
    )
    .fetch_all(pool)
    .await
    .context("加载屏蔽规则失败")?;
    rows.iter().map(rule_from_row).collect()
}

/// 一个站点的未删除规则，按创建顺序。
pub async fn list_rules(pool: &PgPool, site_id: u64) -> Result<Vec<ShieldRuleRow>> {
    let rows = sqlx::query(
        "SELECT id::text AS id, site_id, type, action, value, description
         FROM shield_rules WHERE site_id = $1 AND deleted_at IS NULL ORDER BY created_at, id",
    )
    .bind(i64::try_from(site_id).context("site_id 超出范围")?)
    .fetch_all(pool)
    .await
    .context("查询屏蔽规则失败")?;
    rows.iter().map(rule_from_row).collect()
}

/// 新规则。`value` 必须已规范化。
#[derive(Debug, Clone)]
pub struct NewRule<'a> {
    pub site_id: u64,
    pub kind: &'a str,
    pub action: &'a str,
    pub value: &'a str,
    pub description: Option<&'a str>,
    /// 操作者（`users.id`）；命令行添加时为空。
    pub created_by: Option<i64>,
}

/// 添加的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AddOutcome {
    /// 已添加，带规则 id（UUID 文本）。
    Added(String),
    /// 同站点、同类型、同值的规则已存在（不论 action：唯一约束是 `(site, type, value)`）。
    Duplicate,
    /// 站点已有 `limit` 条规则，达到上限。
    LimitReached { limit: usize },
    /// 站点不存在或已删除。
    SiteNotFound,
}

/// 添加一条规则：事务内先 `SELECT ... FROM sites FOR UPDATE` 锁住站点行，先查重（已有同类型同取值的未删除规则
/// 就报告 [`AddOutcome::Duplicate`]），再数未删除的规则，达到 `max_rules` 就拒绝，否则插入。并发添加同一站点的规则被串行化，不会超限。
pub async fn add_rule(pool: &PgPool, new: &NewRule<'_>, max_rules: usize) -> Result<AddOutcome> {
    let site_id = i64::try_from(new.site_id).context("site_id 超出范围")?;
    let mut tx = pool.begin().await.context("开启事务失败")?;
    let locked: Option<i64> =
        sqlx::query_scalar("SELECT id FROM sites WHERE id = $1 AND deleted_at IS NULL FOR UPDATE")
            .bind(site_id)
            .fetch_optional(&mut *tx)
            .await
            .context("锁定站点行失败")?;
    if locked.is_none() {
        return Ok(AddOutcome::SiteNotFound);
    }
    // 先查重：站点已满时重复项也应当报告「重复」，而不是「已满」
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM shield_rules
                        WHERE site_id = $1 AND type = $2 AND value = $3 AND deleted_at IS NULL)",
    )
    .bind(site_id)
    .bind(new.kind)
    .bind(new.value)
    .fetch_one(&mut *tx)
    .await
    .context("查询重复规则失败")?;
    if exists {
        return Ok(AddOutcome::Duplicate);
    }
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM shield_rules WHERE site_id = $1 AND deleted_at IS NULL",
    )
    .bind(site_id)
    .fetch_one(&mut *tx)
    .await
    .context("统计屏蔽规则失败")?;
    if usize::try_from(count).unwrap_or(usize::MAX) >= max_rules {
        return Ok(AddOutcome::LimitReached { limit: max_rules });
    }
    let id: Option<String> = sqlx::query_scalar(
        "INSERT INTO shield_rules (site_id, type, action, value, description, created_by)
         VALUES ($1, $2, $3, $4, $5, $6)
         ON CONFLICT (site_id, type, value) WHERE deleted_at IS NULL DO NOTHING
         RETURNING id::text",
    )
    .bind(site_id)
    .bind(new.kind)
    .bind(new.action)
    .bind(new.value)
    .bind(new.description)
    .bind(new.created_by)
    .fetch_optional(&mut *tx)
    .await
    .context("写入屏蔽规则失败")?;
    tx.commit().await.context("提交事务失败")?;
    Ok(id.map_or(AddOutcome::Duplicate, AddOutcome::Added))
}

/// 按 id 软删除一条规则；返回是否删除了（不存在、已删除或属于别的站点都返回 `false`）。
pub async fn remove_rule_by_id(pool: &PgPool, site_id: u64, id: &str) -> Result<bool> {
    anyhow::ensure!(is_uuid_text(id), "`{id}` 不是有效的规则 id（UUID）");
    let n = sqlx::query(
        "UPDATE shield_rules SET deleted_at = now()
         WHERE site_id = $1 AND id = ($2::text)::uuid AND deleted_at IS NULL",
    )
    .bind(i64::try_from(site_id).context("site_id 超出范围")?)
    .bind(id)
    .execute(pool)
    .await
    .context("删除屏蔽规则失败")?
    .rows_affected();
    Ok(n > 0)
}

/// 8-4-4-4-12 的十六进制文本。
fn is_uuid_text(s: &str) -> bool {
    s.len() == 36
        && s.bytes().enumerate().all(|(i, b)| match i {
            8 | 13 | 18 | 23 => b == b'-',
            _ => b.is_ascii_hexdigit(),
        })
}

/// 按 `(type, value)` 软删除一条规则；返回是否删除了。
pub async fn remove_rule_by_value(
    pool: &PgPool,
    site_id: u64,
    kind: &str,
    value: &str,
) -> Result<bool> {
    let n = sqlx::query(
        "UPDATE shield_rules SET deleted_at = now()
         WHERE site_id = $1 AND type = $2 AND value = $3 AND deleted_at IS NULL",
    )
    .bind(i64::try_from(site_id).context("site_id 超出范围")?)
    .bind(kind)
    .bind(value)
    .execute(pool)
    .await
    .context("删除屏蔽规则失败")?
    .rows_affected();
    Ok(n > 0)
}
