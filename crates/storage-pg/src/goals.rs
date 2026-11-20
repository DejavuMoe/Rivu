//! 目标（`goals` 表，`system-design.md` §6.2、§6.5）：列表、增删改，以及查询内核用的加载。
//!
//! 取值的校验（事件名、页面通配、滚动阈值、显示名）在调用方（看板端点）：storage 层只管读写，传进来的值必须已经校验。
//! 表的 CHECK 约束兜底：`kind` 取 `event | page | scroll`，事件目标只有 `event_name`、页面目标只有 `page_path`，
//! 滚动深度目标有 `page_path` 与 `scroll_threshold`（1–100）；部分唯一索引
//! `(site_id, kind, COALESCE(event_name, page_path), COALESCE(scroll_threshold, 0))` 保证同一站点同类同值的目标只有一个
//! （同一路径、不同阈值的滚动深度目标可以并存）。
//!
//! 目标的**标签**（查询结果里 `event:goal` 维度的取值、过滤 `["is","event:goal",[…]]` 的取值）是显示名，
//! 否则事件名或页面通配，滚动深度目标是 `≥ N%`（[`default_label`]）；同一站点里标签也唯一（应用层在事务内检查，
//! 见 [`add_goal`]、[`update_goal`]）。
//! 每站上限由应用层在事务内先锁 `sites` 行再数目标，并发添加不会超限。删除是软删除（置 `deleted_at`）。

use anyhow::{Context, Result};
use sqlx::{PgPool, Postgres, Row, Transaction, postgres::PgRow};

/// 一个目标（未删除）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoalRow {
    pub id: i64,
    /// `event | page | scroll`
    pub kind: String,
    pub event_name: Option<String>,
    pub page_path: Option<String>,
    /// 滚动深度目标的阈值（百分比，1–100）；其余目标为 `None`。
    pub scroll_threshold: Option<u8>,
    pub display_name: Option<String>,
    /// UTC、毫秒精度的 RFC 3339 文本。
    pub created_at: String,
}

/// 目标没有显示名时的标签：事件名或页面通配；滚动深度目标是 `≥ N%`（与路径无关，同阈值的两个滚动深度目标
/// 要有一个带显示名）。
pub fn default_label(
    kind: &str,
    event_name: Option<&str>,
    page_path: Option<&str>,
    scroll_threshold: Option<u8>,
) -> String {
    match (kind, scroll_threshold) {
        ("scroll", Some(t)) => format!("≥ {t}%"),
        _ => event_name.or(page_path).unwrap_or("").to_owned(),
    }
}

/// SQL 里的标签表达式，与 [`GoalRow::label`] 相同（`concat!` 里用，保持 `&'static str`）。
macro_rules! label_sql {
    () => {
        "COALESCE(display_name, CASE WHEN kind = 'scroll' \
         THEN '≥ ' || scroll_threshold::text || '%' ELSE COALESCE(event_name, page_path) END)"
    };
}

impl GoalRow {
    /// 标签：显示名，否则 [`default_label`]。
    pub fn label(&self) -> String {
        self.display_name.clone().unwrap_or_else(|| {
            default_label(
                &self.kind,
                self.event_name.as_deref(),
                self.page_path.as_deref(),
                self.scroll_threshold,
            )
        })
    }
}

fn goal_from_row(r: &PgRow) -> Result<GoalRow> {
    let threshold: Option<i16> = r.try_get("scroll_threshold")?;
    Ok(GoalRow {
        id: r.try_get("id")?,
        kind: r.try_get("kind")?,
        event_name: r.try_get("event_name")?,
        page_path: r.try_get("page_path")?,
        scroll_threshold: threshold
            .map(u8::try_from)
            .transpose()
            .context("scroll_threshold 超出范围")?,
        display_name: r.try_get("display_name")?,
        created_at: r.try_get("created_at")?,
    })
}

/// `SELECT … FROM goals` 加上给定的条件（`concat!` 里用，保持 `&'static str`）。
macro_rules! select_goal {
    ($rest:literal) => {
        concat!(
            "SELECT id, kind, event_name, page_path, scroll_threshold, display_name, ",
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

/// 新目标（或编辑后的目标）。取值必须已校验：`kind = "event"` 时只给 `event_name`；`kind = "page"` 时只给
/// `page_path`；`kind = "scroll"` 时给 `page_path` 与 `scroll_threshold`（1–100）。
#[derive(Debug, Clone)]
pub struct NewGoal<'a> {
    pub site_id: u64,
    pub kind: &'a str,
    pub event_name: Option<&'a str>,
    pub page_path: Option<&'a str>,
    pub scroll_threshold: Option<u8>,
    pub display_name: Option<&'a str>,
}

impl NewGoal<'_> {
    fn label(&self) -> String {
        self.display_name.map_or_else(
            || {
                default_label(
                    self.kind,
                    self.event_name,
                    self.page_path,
                    self.scroll_threshold,
                )
            },
            str::to_owned,
        )
    }
}

/// 添加的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AddGoalOutcome {
    Added(i64),
    /// 同类同值的目标已存在。
    Duplicate,
    /// 另一个目标已经用了同样的标签（显示名，或默认标签）。
    LabelTaken,
    /// 站点已有 `limit` 个目标。
    LimitReached {
        limit: usize,
    },
    /// 站点不存在或已删除。
    SiteNotFound,
}

/// 编辑的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateGoalOutcome {
    Updated,
    /// 另一个同类同值的目标已存在。
    Duplicate,
    /// 另一个目标已经用了同样的标签。
    LabelTaken,
    /// 目标不存在、已删除或属于别的站点，或者站点已删除。
    NotFound,
}

/// 事务内锁站点行；站点不存在或已删除时返回 `false`。
async fn lock_site(tx: &mut Transaction<'_, Postgres>, site_id: i64) -> Result<bool> {
    let locked: Option<i64> =
        sqlx::query_scalar("SELECT id FROM sites WHERE id = $1 AND deleted_at IS NULL FOR UPDATE")
            .bind(site_id)
            .fetch_optional(&mut **tx)
            .await
            .context("锁定站点行失败")?;
    Ok(locked.is_some())
}

/// 除 `except` 之外：有没有同类同值的目标、有没有标签相同的目标。
async fn conflicts(
    tx: &mut Transaction<'_, Postgres>,
    site_id: i64,
    new: &NewGoal<'_>,
    except: Option<i64>,
) -> Result<(bool, bool)> {
    let value = new.event_name.or(new.page_path).unwrap_or("");
    let duplicate: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM goals WHERE site_id = $1 AND kind = $2
                        AND COALESCE(event_name, page_path) = $3
                        AND COALESCE(scroll_threshold, 0) = $4
                        AND deleted_at IS NULL AND id IS DISTINCT FROM $5)",
    )
    .bind(site_id)
    .bind(new.kind)
    .bind(value)
    .bind(i16::from(new.scroll_threshold.unwrap_or(0)))
    .bind(except)
    .fetch_one(&mut **tx)
    .await
    .context("查询重复目标失败")?;
    let label_taken: bool = sqlx::query_scalar(concat!(
        "SELECT EXISTS (SELECT 1 FROM goals WHERE site_id = $1 AND deleted_at IS NULL
                        AND id IS DISTINCT FROM $3 AND ",
        label_sql!(),
        " = $2)"
    ))
    .bind(site_id)
    .bind(new.label())
    .bind(except)
    .fetch_one(&mut **tx)
    .await
    .context("查询目标标签失败")?;
    Ok((duplicate, label_taken))
}

/// 添加一个目标：事务内先锁站点行，再查重（同类同值 → [`AddGoalOutcome::Duplicate`]；标签相同 →
/// [`AddGoalOutcome::LabelTaken`]），再数未删除的目标（达到 `max_goals` 拒绝），否则插入。
pub async fn add_goal(
    pool: &PgPool,
    new: &NewGoal<'_>,
    max_goals: usize,
) -> Result<AddGoalOutcome> {
    let site_id = i64::try_from(new.site_id).context("site_id 超出范围")?;
    let mut tx = pool.begin().await.context("开启事务失败")?;
    if !lock_site(&mut tx, site_id).await? {
        return Ok(AddGoalOutcome::SiteNotFound);
    }
    let (duplicate, label_taken) = conflicts(&mut tx, site_id, new, None).await?;
    if duplicate {
        return Ok(AddGoalOutcome::Duplicate);
    }
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
        "INSERT INTO goals (site_id, kind, event_name, page_path, scroll_threshold, display_name)
         VALUES ($1, $2, $3, $4, $5, $6)
         ON CONFLICT (site_id, kind, COALESCE(event_name, page_path), COALESCE(scroll_threshold, 0))
             WHERE deleted_at IS NULL DO NOTHING
         RETURNING id",
    )
    .bind(site_id)
    .bind(new.kind)
    .bind(new.event_name)
    .bind(new.page_path)
    .bind(new.scroll_threshold.map(i16::from))
    .bind(new.display_name)
    .fetch_optional(&mut *tx)
    .await
    .context("写入目标失败")?;
    tx.commit().await.context("提交事务失败")?;
    Ok(id.map_or(AddGoalOutcome::Duplicate, AddGoalOutcome::Added))
}

/// 编辑一个目标：整体替换类型、取值与显示名，id 不变（按 id 的过滤仍然指向它）。事务内先锁站点行，
/// 再按与 [`add_goal`] 相同的规则查重（不和自己比）。
pub async fn update_goal(pool: &PgPool, id: i64, new: &NewGoal<'_>) -> Result<UpdateGoalOutcome> {
    let site_id = i64::try_from(new.site_id).context("site_id 超出范围")?;
    let mut tx = pool.begin().await.context("开启事务失败")?;
    if !lock_site(&mut tx, site_id).await? {
        return Ok(UpdateGoalOutcome::NotFound);
    }
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM goals WHERE site_id = $1 AND id = $2 AND deleted_at IS NULL)",
    )
    .bind(site_id)
    .bind(id)
    .fetch_one(&mut *tx)
    .await
    .context("查询目标失败")?;
    if !exists {
        return Ok(UpdateGoalOutcome::NotFound);
    }
    let (duplicate, label_taken) = conflicts(&mut tx, site_id, new, Some(id)).await?;
    if duplicate {
        return Ok(UpdateGoalOutcome::Duplicate);
    }
    if label_taken {
        return Ok(UpdateGoalOutcome::LabelTaken);
    }
    sqlx::query(
        "UPDATE goals SET kind = $3, event_name = $4, page_path = $5, scroll_threshold = $6,
                          display_name = $7
         WHERE site_id = $1 AND id = $2 AND deleted_at IS NULL",
    )
    .bind(site_id)
    .bind(id)
    .bind(new.kind)
    .bind(new.event_name)
    .bind(new.page_path)
    .bind(new.scroll_threshold.map(i16::from))
    .bind(new.display_name)
    .execute(&mut *tx)
    .await
    .context("更新目标失败")?;
    tx.commit().await.context("提交事务失败")?;
    Ok(UpdateGoalOutcome::Updated)
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
