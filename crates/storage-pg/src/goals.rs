//! 目标（`goals` 表，`system-design.md` §6.2、§6.5）：列表、增删，以及查询内核用的加载。
//!
//! 取值的校验（事件名、页面通配、显示名）在调用方（看板端点）：storage 层只管读写，传进来的值必须已经校验。
//! 表的 CHECK 约束兜底：`kind` 取 `event | page`，事件目标只有 `event_name`、页面目标只有 `page_path`；
//! 部分唯一索引 `(site_id, kind, COALESCE(event_name, page_path))` 保证同一站点同类同值的目标只有一个。
//!
//! 目标的**标签**（查询结果里 `event:goal` 维度的取值、过滤 `["is","event:goal",[…]]` 的取值）是
//! `COALESCE(display_name, event_name, page_path)`；同一站点里标签也唯一（应用层在事务内检查，见 [`add_goal`]）。
//! 每站上限由应用层在事务内先锁 `sites` 行再数目标，并发添加不会超限。删除是软删除（置 `deleted_at`）。

use anyhow::{Context, Result};
use sqlx::{PgPool, Row, postgres::PgRow};

/// 一个目标（未删除）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoalRow {
    pub id: i64,
    /// `event | page`
    pub kind: String,
    pub event_name: Option<String>,
    pub page_path: Option<String>,
    pub display_name: Option<String>,
    /// UTC、毫秒精度的 RFC 3339 文本。
    pub created_at: String,
}

impl GoalRow {
    /// 标签：显示名，否则事件名，否则页面通配。
    pub fn label(&self) -> &str {
        self.display_name
            .as_deref()
            .or(self.event_name.as_deref())
            .or(self.page_path.as_deref())
            .unwrap_or("")
    }
}

fn goal_from_row(r: &PgRow) -> Result<GoalRow> {
    Ok(GoalRow {
        id: r.try_get("id")?,
        kind: r.try_get("kind")?,
        event_name: r.try_get("event_name")?,
        page_path: r.try_get("page_path")?,
        display_name: r.try_get("display_name")?,
        created_at: r.try_get("created_at")?,
    })
}

/// `SELECT … FROM goals` 加上给定的条件（`concat!` 里用，保持 `&'static str`）。
macro_rules! select_goal {
    ($rest:literal) => {
        concat!(
            "SELECT id, kind, event_name, page_path, display_name, ",
            ts_utc!("created_at"),
            " AS created_at FROM goals ",
            $rest
        )
    };
}

/// 一个站点的未删除目标，按创建顺序（查询内核与设置页共用）。
pub async fn list_goals(pool: &PgPool, site_id: u64) -> Result<Vec<GoalRow>> {
    let rows = sqlx::query(select_goal!(
        "WHERE site_id = $1 AND deleted_at IS NULL ORDER BY goals.created_at, id"
    ))
    .bind(i64::try_from(site_id).context("site_id 超出范围")?)
    .fetch_all(pool)
    .await
    .context("查询目标失败")?;
    rows.iter().map(goal_from_row).collect()
}

/// 按 id 取一个未删除的目标。
pub async fn get_goal(pool: &PgPool, site_id: u64, id: i64) -> Result<Option<GoalRow>> {
    let row = sqlx::query(select_goal!(
        "WHERE site_id = $1 AND id = $2 AND deleted_at IS NULL"
    ))
    .bind(i64::try_from(site_id).context("site_id 超出范围")?)
    .bind(id)
    .fetch_optional(pool)
    .await
    .context("查询目标失败")?;
    row.as_ref().map(goal_from_row).transpose()
}

/// 新目标。取值必须已校验；`kind = "event"` 时只给 `event_name`，`kind = "page"` 时只给 `page_path`。
#[derive(Debug, Clone)]
pub struct NewGoal<'a> {
    pub site_id: u64,
    pub kind: &'a str,
    pub event_name: Option<&'a str>,
    pub page_path: Option<&'a str>,
    pub display_name: Option<&'a str>,
}

/// 添加的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AddGoalOutcome {
    Added(i64),
    /// 同类同值的目标已存在。
    Duplicate,
    /// 另一个目标已经用了同样的标签（显示名、事件名或页面通配）。
    LabelTaken,
    /// 站点已有 `limit` 个目标。
    LimitReached {
        limit: usize,
    },
    /// 站点不存在或已删除。
    SiteNotFound,
}

/// 添加一个目标：事务内先锁站点行，再查重（同类同值 → [`AddGoalOutcome::Duplicate`]；标签相同 →
/// [`AddGoalOutcome::LabelTaken`]），再数未删除的目标（达到 `max_goals` 拒绝），否则插入。
pub async fn add_goal(
    pool: &PgPool,
    new: &NewGoal<'_>,
    max_goals: usize,
) -> Result<AddGoalOutcome> {
    let site_id = i64::try_from(new.site_id).context("site_id 超出范围")?;
    let value = new.event_name.or(new.page_path).unwrap_or("");
    let label = new.display_name.unwrap_or(value);
    let mut tx = pool.begin().await.context("开启事务失败")?;
    let locked: Option<i64> =
        sqlx::query_scalar("SELECT id FROM sites WHERE id = $1 AND deleted_at IS NULL FOR UPDATE")
            .bind(site_id)
            .fetch_optional(&mut *tx)
            .await
            .context("锁定站点行失败")?;
    if locked.is_none() {
        return Ok(AddGoalOutcome::SiteNotFound);
    }
    let duplicate: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM goals WHERE site_id = $1 AND kind = $2
                        AND COALESCE(event_name, page_path) = $3 AND deleted_at IS NULL)",
    )
    .bind(site_id)
    .bind(new.kind)
    .bind(value)
    .fetch_one(&mut *tx)
    .await
    .context("查询重复目标失败")?;
    if duplicate {
        return Ok(AddGoalOutcome::Duplicate);
    }
    let label_taken: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM goals WHERE site_id = $1 AND deleted_at IS NULL
                        AND COALESCE(display_name, event_name, page_path) = $2)",
    )
    .bind(site_id)
    .bind(label)
    .fetch_one(&mut *tx)
    .await
    .context("查询目标标签失败")?;
    if label_taken {
        return Ok(AddGoalOutcome::LabelTaken);
    }
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM goals WHERE site_id = $1 AND deleted_at IS NULL")
            .bind(site_id)
            .fetch_one(&mut *tx)
            .await
            .context("统计目标失败")?;
    if usize::try_from(count).unwrap_or(usize::MAX) >= max_goals {
        return Ok(AddGoalOutcome::LimitReached { limit: max_goals });
    }
    let id: Option<i64> = sqlx::query_scalar(
        "INSERT INTO goals (site_id, kind, event_name, page_path, display_name)
         VALUES ($1, $2, $3, $4, $5)
         ON CONFLICT (site_id, kind, COALESCE(event_name, page_path)) WHERE deleted_at IS NULL DO NOTHING
         RETURNING id",
    )
    .bind(site_id)
    .bind(new.kind)
    .bind(new.event_name)
    .bind(new.page_path)
    .bind(new.display_name)
    .fetch_optional(&mut *tx)
    .await
    .context("写入目标失败")?;
    tx.commit().await.context("提交事务失败")?;
    Ok(id.map_or(AddGoalOutcome::Duplicate, AddGoalOutcome::Added))
}

/// 按 id 软删除一个目标；返回是否删除了（不存在、已删除或属于别的站点都返回 `false`）。
pub async fn remove_goal(pool: &PgPool, site_id: u64, id: i64) -> Result<bool> {
    let n = sqlx::query(
        "UPDATE goals SET deleted_at = now() WHERE site_id = $1 AND id = $2 AND deleted_at IS NULL",
    )
    .bind(i64::try_from(site_id).context("site_id 超出范围")?)
    .bind(id)
    .execute(pool)
    .await
    .context("删除目标失败")?
    .rows_affected();
    Ok(n > 0)
}
