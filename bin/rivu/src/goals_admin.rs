//! 看板的目标端点（`goals` 表；查询语义见 `system-design.md` §6.5、§6.6）。路由在 [`routes`]，由
//! [`crate::dash_api::router`] 合并；端点表见 `dash_api.rs` 顶部。
//!
//! | 端点 | 权限 |
//! |---|---|
//! | `GET /api/internal/sites/{site}/goals` | 有效角色 editor+（站点访客 editor 也可以），与其他设置读取相同 |
//! | `POST /api/internal/sites/{site}/goals` | `site.content.write`：有效角色 editor+（含站点访客 editor） |
//! | `PATCH /api/internal/sites/{site}/goals/{id}` | 同上 |
//! | `DELETE /api/internal/sites/{site}/goals/{id}` | 同上 |
//!
//! 看板的目标卡片不调 GET：目标的名字随 `event:goal` 维度的查询结果返回，所以能看看板的人都能看到有转化的目标。
//!
//! 目标有三类：`event`（事件名完全相等，区分大小写）、`page`（pathname 通配，只有 `*`，区分大小写，
//! 规范化与屏蔽规则的 `page` 模式相同）与 `scroll`（pathname 通配同 `page`，加上滚动阈值 1–100；
//! 不给 `page_path` 时是 `/*`，即所有页面）。属性条件不在本期。
//! 添加与编辑的请求体相同：`{"kind", "event_name" | "page_path" [, "scroll_threshold"], "display_name"}`；
//! 编辑整体替换类型、取值与显示名（没给的显示名清空），id 不变。
//! 目标每次查询现读（`query_api::load_site_context`），没有缓存，增删改立即生效。

use std::sync::Arc;

use axum::Router;
use axum::extract::{Path, Request, State};
use axum::http::StatusCode;
use axum::response::Response;
use axum::routing::{get, patch};
use rivu_auth::scope::Action;
use rivu_ingest::shield::{RuleType, normalize_value};
use rivu_storage_pg::goals::{self, AddGoalOutcome, GoalRow, NewGoal, UpdateGoalOutcome};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::dash_api::{AUTH_BODY_MAX, DashApi, DashError, Res, clean_name, endpoint, read_json};
use crate::site_admin::{done, no_content, only};

/// 每个站点最多的目标数。
pub const MAX_GOALS: usize = 100;
/// 事件名的字符数上限。
pub const EVENT_NAME_MAX_CHARS: usize = 120;
/// 滚动深度目标不给 `page_path` 时的通配：所有页面。
pub const SCROLL_ALL_PAGES: &str = "/*";

fn goal_json(g: &GoalRow) -> Value {
    json!({
        "id": g.id, "kind": g.kind, "event_name": g.event_name, "page_path": g.page_path,
        "scroll_threshold": g.scroll_threshold, "display_name": g.display_name,
        "label": g.label(), "created_at": g.created_at,
    })
}

async fn list_goals(
    State(api): State<Arc<DashApi>>,
    Path(site): Path<String>,
    req: Request,
) -> Res<Response> {
    let c = api
        .authorize_admin(req.headers(), &site, Action::SiteSettingsRead, false)
        .await?;
    let rows = goals::list_goals(&api.pg, c.site.id).await?;
    Ok(done(
        &api,
        &c,
        StatusCode::OK,
        json!({"goals": rows.iter().map(goal_json).collect::<Vec<_>>(), "max_goals": MAX_GOALS}),
    ))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GoalBody {
    kind: String,
    #[serde(default)]
    event_name: Option<String>,
    #[serde(default)]
    page_path: Option<String>,
    #[serde(default)]
    scroll_threshold: Option<i64>,
    #[serde(default)]
    display_name: Option<String>,
}

/// 校验后的目标取值。
struct ValidGoal {
    kind: &'static str,
    event_name: Option<String>,
    page_path: Option<String>,
    scroll_threshold: Option<u8>,
    display_name: Option<String>,
}

impl ValidGoal {
    fn new_goal(&self, site_id: u64) -> NewGoal<'_> {
        NewGoal {
            site_id,
            kind: self.kind,
            event_name: self.event_name.as_deref(),
            page_path: self.page_path.as_deref(),
            scroll_threshold: self.scroll_threshold,
            display_name: self.display_name.as_deref(),
        }
    }
}

/// 事件名：trim 后 1–120 个字符、无控制字符，不能是 `engagement`（engagement 事件从不计入目标）。
fn valid_event_name(raw: &str) -> Res<String> {
    let s = raw.trim();
    if s.is_empty() || s.chars().count() > EVENT_NAME_MAX_CHARS {
        return Err(DashError::Invalid(format!(
            "the event_name must be 1 to {EVENT_NAME_MAX_CHARS} characters"
        )));
    }
    if s.chars().any(char::is_control) {
        return Err(DashError::Invalid(
            "the event_name must not contain control characters".into(),
        ));
    }
    if s == "engagement" {
        return Err(DashError::Invalid(
            "`engagement` events never count as goal completions; use a page goal or a custom event name".into(),
        ));
    }
    Ok(s.to_owned())
}

/// 页面通配：与屏蔽规则的 `page` 模式同一规范化（trim、以 `/` 开头、≤ 250 字节、无空白与控制字符、连续的 `*` 合并）。
fn valid_page_path(raw: &str) -> Res<String> {
    normalize_value(RuleType::Page, raw).map_err(|_| {
        DashError::Invalid(
            "the page_path must start with `/`, be at most 250 bytes and contain no whitespace or control characters (`*` is the only wildcard)".into(),
        )
    })
}

/// 滚动阈值：1–100 的整数（百分比）。
fn valid_threshold(raw: Option<i64>) -> Res<u8> {
    raw.and_then(|t| u8::try_from(t).ok())
        .filter(|t| (1..=100).contains(t))
        .ok_or_else(|| {
            DashError::Invalid(
                "a scroll goal needs a scroll_threshold from 1 to 100 (percent)".into(),
            )
        })
}

/// 添加与编辑共用的校验：按类型要求并只允许对应的字段。
fn validate(body: GoalBody) -> Res<ValidGoal> {
    let (kind, event_name, page_path, scroll_threshold) = match body.kind.as_str() {
        "event" => {
            if body.page_path.is_some() || body.scroll_threshold.is_some() {
                return Err(DashError::Invalid(
                    "an event goal must not have a page_path or scroll_threshold".into(),
                ));
            }
            let raw = body
                .event_name
                .as_deref()
                .ok_or_else(|| DashError::Invalid("an event goal needs an event_name".into()))?;
            ("event", Some(valid_event_name(raw)?), None, None)
        }
        "page" => {
            if body.event_name.is_some() || body.scroll_threshold.is_some() {
                return Err(DashError::Invalid(
                    "a page goal must not have an event_name or scroll_threshold".into(),
                ));
            }
            let raw = body
                .page_path
                .as_deref()
                .ok_or_else(|| DashError::Invalid("a page goal needs a page_path".into()))?;
            ("page", None, Some(valid_page_path(raw)?), None)
        }
        "scroll" => {
            if body.event_name.is_some() {
                return Err(DashError::Invalid(
                    "a scroll goal must not have an event_name".into(),
                ));
            }
            let path = match body.page_path.as_deref() {
                None => SCROLL_ALL_PAGES.to_owned(),
                Some(raw) if raw.trim().is_empty() => SCROLL_ALL_PAGES.to_owned(),
                Some(raw) => valid_page_path(raw)?,
            };
            (
                "scroll",
                None,
                Some(path),
                Some(valid_threshold(body.scroll_threshold)?),
            )
        }
        _ => {
            return Err(DashError::Invalid(
                "the kind must be one of event, page, scroll".into(),
            ));
        }
    };
    Ok(ValidGoal {
        kind,
        event_name,
        page_path,
        scroll_threshold,
        display_name: clean_name(body.display_name.as_deref())?,
    })
}

fn conflict_duplicate() -> DashError {
    DashError::Conflict(
        "conflict",
        "a goal with the same kind and value already exists".into(),
    )
}

fn conflict_label() -> DashError {
    DashError::Conflict(
        "conflict",
        "another goal already uses this name; choose a different display name".into(),
    )
}

async fn add_goal(
    State(api): State<Arc<DashApi>>,
    Path(site): Path<String>,
    req: Request,
) -> Res<Response> {
    let headers = req.headers().clone();
    let c = api
        .authorize_admin(&headers, &site, Action::SiteContentWrite, false)
        .await?;
    let body: GoalBody = read_json(req, AUTH_BODY_MAX).await?;
    let goal = validate(body)?;
    let outcome = goals::add_goal(&api.pg, &goal.new_goal(c.site.id), MAX_GOALS).await?;
    let id = match outcome {
        AddGoalOutcome::Added(id) => id,
        AddGoalOutcome::Duplicate => return Err(conflict_duplicate()),
        AddGoalOutcome::LabelTaken => return Err(conflict_label()),
        AddGoalOutcome::LimitReached { limit } => {
            return Err(DashError::Conflict(
                "limit_reached",
                format!("this site already has the maximum of {limit} goals"),
            ));
        }
        AddGoalOutcome::SiteNotFound => return Err(DashError::NotFound),
    };
    let row = goals::get_goal(&api.pg, c.site.id, id)
        .await?
        .ok_or(DashError::Internal)?;
    tracing::info!(
        user_id = c.session.s.user_id,
        site_id = c.site.id,
        goal_id = id,
        "添加目标"
    );
    Ok(done(
        &api,
        &c,
        StatusCode::CREATED,
        json!({"goal": goal_json(&row)}),
    ))
}

async fn update_goal(
    State(api): State<Arc<DashApi>>,
    Path((site, id)): Path<(String, String)>,
    req: Request,
) -> Res<Response> {
    let headers = req.headers().clone();
    let c = api
        .authorize_admin(&headers, &site, Action::SiteContentWrite, false)
        .await?;
    // 不是整数的 id 不可能存在：404（不是 400）
    let id: i64 = id.parse().map_err(|_| DashError::NotFound)?;
    let body: GoalBody = read_json(req, AUTH_BODY_MAX).await?;
    let goal = validate(body)?;
    match goals::update_goal(&api.pg, id, &goal.new_goal(c.site.id)).await? {
        UpdateGoalOutcome::Updated => {}
        UpdateGoalOutcome::Duplicate => return Err(conflict_duplicate()),
        UpdateGoalOutcome::LabelTaken => return Err(conflict_label()),
        UpdateGoalOutcome::NotFound => return Err(DashError::NotFound),
    }
    let row = goals::get_goal(&api.pg, c.site.id, id)
        .await?
        .ok_or(DashError::Internal)?;
    tracing::info!(
        user_id = c.session.s.user_id,
        site_id = c.site.id,
        goal_id = id,
        "编辑目标"
    );
    Ok(done(
        &api,
        &c,
        StatusCode::OK,
        json!({"goal": goal_json(&row)}),
    ))
}

async fn delete_goal(
    State(api): State<Arc<DashApi>>,
    Path((site, id)): Path<(String, String)>,
    req: Request,
) -> Res<Response> {
    let c = api
        .authorize_admin(req.headers(), &site, Action::SiteContentWrite, false)
        .await?;
    // 不是整数的 id 不可能存在：404（不是 400）
    let id: i64 = id.parse().map_err(|_| DashError::NotFound)?;
    if !goals::remove_goal(&api.pg, c.site.id, id).await? {
        return Err(DashError::NotFound);
    }
    tracing::info!(
        user_id = c.session.s.user_id,
        site_id = c.site.id,
        goal_id = id,
        "删除目标"
    );
    Ok(no_content(&api, &c))
}

pub(crate) fn routes(api: &Arc<DashApi>) -> Router {
    let a = api;
    Router::new()
        .merge(endpoint(
            a,
            "internal.goals",
            "/api/internal/sites/{site}/goals",
            only(get(list_goals).post(add_goal), "GET, HEAD, POST"),
        ))
        .merge(endpoint(
            a,
            "internal.goal",
            "/api/internal/sites/{site}/goals/{id}",
            only(patch(update_goal).delete(delete_goal), "PATCH, DELETE"),
        ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_names_are_validated() {
        assert_eq!(valid_event_name("  Signup ").unwrap(), "Signup");
        assert!(valid_event_name(&"x".repeat(120)).is_ok());
        for bad in ["", "   ", "a\u{7}b", "engagement"] {
            assert!(valid_event_name(bad).is_err(), "{bad:?}");
        }
        assert!(valid_event_name(&"x".repeat(121)).is_err());
    }

    #[test]
    fn page_paths_follow_the_shield_page_rules() {
        assert_eq!(
            valid_page_path(" /checkout/**/done ").unwrap(),
            "/checkout/*/done"
        );
        assert!(valid_page_path(&format!("/{}", "a".repeat(249))).is_ok());
        for bad in ["checkout", "/a b", "/a\tb", "", "*"] {
            assert!(valid_page_path(bad).is_err(), "{bad:?}");
        }
        assert!(valid_page_path(&format!("/{}", "a".repeat(250))).is_err());
    }

    fn body(v: Value) -> GoalBody {
        serde_json::from_value(v).unwrap()
    }

    #[test]
    fn scroll_goals_are_validated() {
        let g = validate(body(
            json!({"kind": "scroll", "page_path": " /blog/** ", "scroll_threshold": 75}),
        ))
        .unwrap();
        assert_eq!(
            (g.kind, g.page_path.as_deref(), g.scroll_threshold),
            ("scroll", Some("/blog/*"), Some(75))
        );
        // 不给路径或给空串：所有页面
        for v in [
            json!({"kind": "scroll", "scroll_threshold": 1}),
            json!({"kind": "scroll", "page_path": "  ", "scroll_threshold": 100}),
        ] {
            assert_eq!(validate(body(v)).unwrap().page_path.as_deref(), Some("/*"));
        }
        for bad in [
            json!({"kind": "scroll", "page_path": "/a"}),
            json!({"kind": "scroll", "page_path": "/a", "scroll_threshold": 0}),
            json!({"kind": "scroll", "page_path": "/a", "scroll_threshold": 101}),
            json!({"kind": "scroll", "page_path": "/a", "scroll_threshold": -5}),
            json!({"kind": "scroll", "page_path": "no-slash", "scroll_threshold": 50}),
            json!({"kind": "scroll", "event_name": "A", "scroll_threshold": 50}),
            json!({"kind": "page", "page_path": "/a", "scroll_threshold": 50}),
            json!({"kind": "event", "event_name": "A", "scroll_threshold": 50}),
            json!({"kind": "funnel"}),
        ] {
            assert!(validate(body(bad.clone())).is_err(), "{bad}");
        }
        // 阈值不是整数：反序列化就失败（400）
        assert!(
            serde_json::from_value::<GoalBody>(json!({"kind": "scroll", "scroll_threshold": 50.5}))
                .is_err()
        );
    }
}
