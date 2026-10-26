//! 看板的目标端点（`goals` 表；查询语义见 `system-design.md` §6.5、§6.6）。路由在 [`routes`]，由
//! [`crate::dash_api::router`] 合并；端点表见 `dash_api.rs` 顶部。
//!
//! | 端点 | 权限 |
//! |---|---|
//! | `GET /api/internal/sites/{site}/goals` | 有效角色 editor+（站点访客 editor 也可以），与其他设置读取相同 |
//! | `POST /api/internal/sites/{site}/goals` | `site.content.write`：有效角色 editor+（含站点访客 editor） |
//! | `DELETE /api/internal/sites/{site}/goals/{id}` | 同上 |
//!
//! 看板的目标卡片不调 GET：目标的名字随 `event:goal` 维度的查询结果返回，所以能看看板的人都能看到有转化的目标。
//!
//! 目标只有两类（本期范围）：`event`（事件名完全相等，区分大小写）与 `page`（pathname 通配，只有 `*`，区分大小写，
//! 规范化与屏蔽规则的 `page` 模式相同）。滚动深度目标与属性条件不在本期。
//! 目标每次查询现读（`query_api::load_site_context`），没有缓存，增删立即生效。

use std::sync::Arc;

use axum::Router;
use axum::extract::{Path, Request, State};
use axum::http::StatusCode;
use axum::response::Response;
use axum::routing::{delete, get};
use rivu_auth::scope::Action;
use rivu_ingest::shield::{RuleType, normalize_value};
use rivu_storage_pg::goals::{self, AddGoalOutcome, GoalRow, NewGoal};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::dash_api::{AUTH_BODY_MAX, DashApi, DashError, Res, clean_name, endpoint, read_json};
use crate::site_admin::{done, no_content, only};

/// 每个站点最多的目标数。
pub const MAX_GOALS: usize = 100;
/// 事件名的字符数上限。
pub const EVENT_NAME_MAX_CHARS: usize = 120;

fn goal_json(g: &GoalRow) -> Value {
    json!({
        "id": g.id, "kind": g.kind, "event_name": g.event_name, "page_path": g.page_path,
        "display_name": g.display_name, "label": g.label(), "created_at": g.created_at,
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
    display_name: Option<String>,
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
    let (event_name, page_path) = match body.kind.as_str() {
        "event" => {
            if body.page_path.is_some() {
                return Err(DashError::Invalid(
                    "an event goal must not have a page_path".into(),
                ));
            }
            let raw = body
                .event_name
                .as_deref()
                .ok_or_else(|| DashError::Invalid("an event goal needs an event_name".into()))?;
            (Some(valid_event_name(raw)?), None)
        }
        "page" => {
            if body.event_name.is_some() {
                return Err(DashError::Invalid(
                    "a page goal must not have an event_name".into(),
                ));
            }
            let raw = body
                .page_path
                .as_deref()
                .ok_or_else(|| DashError::Invalid("a page goal needs a page_path".into()))?;
            (None, Some(valid_page_path(raw)?))
        }
        _ => {
            return Err(DashError::Invalid(
                "the kind must be one of event, page".into(),
            ));
        }
    };
    let display_name = clean_name(body.display_name.as_deref())?;
    let outcome = goals::add_goal(
        &api.pg,
        &NewGoal {
            site_id: c.site.id,
            kind: &body.kind,
            event_name: event_name.as_deref(),
            page_path: page_path.as_deref(),
            display_name: display_name.as_deref(),
        },
        MAX_GOALS,
    )
    .await?;
    let id = match outcome {
        AddGoalOutcome::Added(id) => id,
        AddGoalOutcome::Duplicate => {
            return Err(DashError::Conflict(
                "conflict",
                "a goal with the same kind and value already exists".into(),
            ));
        }
        AddGoalOutcome::LabelTaken => {
            return Err(DashError::Conflict(
                "conflict",
                "another goal already uses this name; choose a different display name".into(),
            ));
        }
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
            "internal.goal_delete",
            "/api/internal/sites/{site}/goals/{id}",
            only(delete(delete_goal), "DELETE"),
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
}
