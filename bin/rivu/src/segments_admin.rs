//! 看板的分群端点（`segments` 表，`platform-and-ops.md` §3.2、§4.1；查询语义见 `system-design.md` §6.2、§6.3）。
//! 路由在 [`routes`]，由 [`crate::dash_api::router`] 合并；端点表见 `dash_api.rs` 顶部。
//!
//! | 端点 | 权限 |
//! |---|---|
//! | `GET /api/internal/sites/{site}/segments` | `stats.read`：能在登录后的看板里看这个站点统计的人（含站点访客 viewer） |
//! | `POST /api/internal/sites/{site}/segments` | `site.content.write`：有效角色 editor+（含站点访客 editor） |
//! | `PATCH /api/internal/sites/{site}/segments/{id}` | 同上 |
//! | `DELETE /api/internal/sites/{site}/segments/{id}` | 同上 |
//!
//! 分群是一组带名字的过滤，按站点共享：能看这个站点的人都能应用，editor 以上都能改、删别人建的分群（与目标、注释一样）。
//! 只有站点级分群（`owner_user_id` 为空），个人分群暂不支持。应用分群就是在查询的过滤里加 `["is", "segment", [id]]`，
//! build 阶段展开成分群的过滤（`query_api::attach_segments`），所以看板、共享链接、公开查询 API 与 `rivu query`
//! 都按同一种方式展开。分群的过滤与 QuerySpec 的 `filters` 同一个语法（顶层隐式 AND），保存前按查询的规则校验，
//! 存规范化后的写法。还有未吊销的共享链接绑定一个分群时不能删（409 `segment_in_use`，带这些链接的 id 和名称）。

use std::sync::Arc;

use axum::Router;
use axum::extract::{Path, Request, State};
use axum::http::StatusCode;
use axum::response::Response;
use axum::routing::{get, patch};
use rivu_auth::scope::Action;
use rivu_query::{QueryError, SegmentDef, parse_segment_filters, validate_segment};
use rivu_storage_pg::segments::{
    self, AddSegmentOutcome, DeleteSegmentOutcome, SegmentRow, UpdateSegmentOutcome,
};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::dash_api::{AUTH_BODY_MAX, DashApi, DashError, Res, endpoint, read_json};
use crate::query_api::Fail;
use crate::site_admin::{Caller, done, no_content, only};

/// 每个站点最多的分群数（未删除的）。
pub const MAX_SEGMENTS: usize = 100;
/// 名称的字符数上限（trim 之后）。
pub const NAME_MAX_CHARS: usize = 100;

/// 库里的过滤按规范写法给出（早期手工写进库里的单个节点也变成数组）；读不懂的原样给出。
fn filters_json(id: i64, text: &str) -> Value {
    let raw: Value = serde_json::from_str(text).unwrap_or(Value::Null);
    match SegmentDef::from_json(id, &raw) {
        Ok(seg) => serde_json::to_value(&seg.filters).unwrap_or(raw),
        Err(_) => raw,
    }
}

fn segment_json(s: &SegmentRow) -> Value {
    json!({
        "id": s.id, "name": s.name, "filters": filters_json(s.id, &s.filter),
        "created_at": s.created_at, "updated_at": s.updated_at,
    })
}

/// 名称：trim 后 1–100 个字符、不含控制字符。
fn valid_name(raw: &str) -> Res<String> {
    let s = raw.trim();
    if s.is_empty() || s.chars().count() > NAME_MAX_CHARS {
        return Err(DashError::Invalid(format!(
            "the name must be 1 to {NAME_MAX_CHARS} characters"
        )));
    }
    if s.chars().any(char::is_control) {
        return Err(DashError::Invalid(
            "the name must not contain control characters".into(),
        ));
    }
    Ok(s.to_owned())
}

/// 过滤：按查询的语法解析、按查询的规则校验（用站点现在的目标解析 `event:goal`），返回规范化的 JSON 文本。
/// 错误与查询的 400 同形（`invalid_query`，`errors[].path` 以 `filters` 开头）。
async fn valid_filters(api: &DashApi, c: &Caller, raw: &Value) -> Res<String> {
    let invalid = |e: QueryError| DashError::Fail(Fail::Query(e), api.dashboard_timeout_secs);
    let filters = parse_segment_filters(raw).map_err(invalid)?;
    let site = api.site_context(c.site.id, &c.site.site_key).await?;
    validate_segment(&filters, &site).map_err(invalid)?;
    serde_json::to_string(&filters).map_err(|e| {
        tracing::error!(error = %e, "序列化分群过滤失败");
        DashError::Internal
    })
}

fn name_taken() -> DashError {
    DashError::Conflict(
        "conflict",
        "this site already has a segment with this name".into(),
    )
}

async fn list_segments(
    State(api): State<Arc<DashApi>>,
    Path(site): Path<String>,
    req: Request,
) -> Res<Response> {
    let c = api
        .authorize_admin(req.headers(), &site, Action::StatsRead, false)
        .await?;
    let rows = segments::list_segments(&api.pg, c.site.id).await?;
    let body = json!({
        "segments": rows.iter().map(segment_json).collect::<Vec<_>>(),
        "max_segments": MAX_SEGMENTS,
    });
    Ok(done(&api, &c, StatusCode::OK, body))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NewBody {
    name: String,
    filters: Value,
}

async fn add_segment(
    State(api): State<Arc<DashApi>>,
    Path(site): Path<String>,
    req: Request,
) -> Res<Response> {
    let headers = req.headers().clone();
    let c = api
        .authorize_admin(&headers, &site, Action::SiteContentWrite, false)
        .await?;
    let body: NewBody = read_json(req, AUTH_BODY_MAX).await?;
    let name = valid_name(&body.name)?;
    let filter = valid_filters(&api, &c, &body.filters).await?;
    let id = match segments::add_segment(&api.pg, c.site.id, &name, &filter, MAX_SEGMENTS).await? {
        AddSegmentOutcome::Added(id) => id,
        AddSegmentOutcome::LimitReached { limit } => {
            return Err(DashError::Conflict(
                "limit_reached",
                format!("this site already has the maximum of {limit} segments"),
            ));
        }
        AddSegmentOutcome::NameTaken => return Err(name_taken()),
        AddSegmentOutcome::SiteNotFound => return Err(DashError::NotFound),
    };
    let row = segments::get_segment(&api.pg, c.site.id, id)
        .await?
        .ok_or(DashError::Internal)?;
    tracing::info!(
        user_id = c.session.s.user_id,
        site_id = c.site.id,
        segment_id = id,
        "添加分群"
    );
    Ok(done(
        &api,
        &c,
        StatusCode::CREATED,
        json!({"segment": segment_json(&row)}),
    ))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PatchBody {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    filters: Option<Value>,
}

/// 路径里的分群 id：不是整数的 id 不可能存在，404（不是 400）。
fn parse_id(raw: &str) -> Res<i64> {
    raw.parse().map_err(|_| DashError::NotFound)
}

async fn update_segment(
    State(api): State<Arc<DashApi>>,
    Path((site, id)): Path<(String, String)>,
    req: Request,
) -> Res<Response> {
    let headers = req.headers().clone();
    let c = api
        .authorize_admin(&headers, &site, Action::SiteContentWrite, false)
        .await?;
    let id = parse_id(&id)?;
    let body: PatchBody = read_json(req, AUTH_BODY_MAX).await?;
    if body.name.is_none() && body.filters.is_none() {
        return Err(DashError::Invalid(
            "give at least one of name, filters".into(),
        ));
    }
    let name = body.name.as_deref().map(valid_name).transpose()?;
    let filter = match &body.filters {
        Some(f) => Some(valid_filters(&api, &c, f).await?),
        None => None,
    };
    match segments::update_segment(&api.pg, c.site.id, id, name.as_deref(), filter.as_deref())
        .await?
    {
        UpdateSegmentOutcome::Updated => {}
        UpdateSegmentOutcome::NotFound => return Err(DashError::NotFound),
        UpdateSegmentOutcome::NameTaken => return Err(name_taken()),
    }
    let row = segments::get_segment(&api.pg, c.site.id, id)
        .await?
        .ok_or(DashError::NotFound)?;
    tracing::info!(
        user_id = c.session.s.user_id,
        site_id = c.site.id,
        segment_id = id,
        "修改分群"
    );
    Ok(done(
        &api,
        &c,
        StatusCode::OK,
        json!({"segment": segment_json(&row)}),
    ))
}

async fn delete_segment(
    State(api): State<Arc<DashApi>>,
    Path((site, id)): Path<(String, String)>,
    req: Request,
) -> Res<Response> {
    let c = api
        .authorize_admin(req.headers(), &site, Action::SiteContentWrite, false)
        .await?;
    let id = parse_id(&id)?;
    match segments::delete_segment(&api.pg, c.site.id, id).await? {
        DeleteSegmentOutcome::Deleted => {}
        DeleteSegmentOutcome::NotFound => return Err(DashError::NotFound),
        DeleteSegmentOutcome::InUse(links) => {
            // 告诉页面是哪些链接在用（链接 id 与名称；能删分群的人在设置页也能看到这些链接）
            let body = json!({
                "error": "segment_in_use",
                "message": "shared links use this segment; revoke them first",
                "links": links.iter().map(|l| json!({"id": l.id, "name": l.name})).collect::<Vec<_>>(),
            });
            return Ok(done(&api, &c, StatusCode::CONFLICT, body));
        }
    }
    tracing::info!(
        user_id = c.session.s.user_id,
        site_id = c.site.id,
        segment_id = id,
        "删除分群"
    );
    Ok(no_content(&api, &c))
}

pub(crate) fn routes(api: &Arc<DashApi>) -> Router {
    let a = api;
    Router::new()
        .merge(endpoint(
            a,
            "internal.segments",
            "/api/internal/sites/{site}/segments",
            only(get(list_segments).post(add_segment), "GET, HEAD, POST"),
        ))
        .merge(endpoint(
            a,
            "internal.segment",
            "/api/internal/sites/{site}/segments/{id}",
            only(
                patch(update_segment).delete(delete_segment),
                "PATCH, DELETE",
            ),
        ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_trimmed_bounded_and_single_line() {
        assert_eq!(
            valid_name("  Mobile from search ").unwrap(),
            "Mobile from search"
        );
        assert_eq!(valid_name("搜索来的手机访客").unwrap(), "搜索来的手机访客");
        assert!(valid_name(&"字".repeat(100)).is_ok());
        assert!(valid_name(&"字".repeat(101)).is_err());
        for bad in ["", "   ", "a\nb", "a\tb", "a\u{7}b", "a\u{9f}b"] {
            assert!(valid_name(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn stored_filters_come_back_in_the_normalized_array_form() {
        assert_eq!(
            filters_json(1, r#"["is","visit:country",["SE"]]"#),
            json!([["is", "visit:country", ["SE"]]])
        );
        assert_eq!(
            filters_json(1, r#"[["is","visit:city",[2673730]]]"#),
            json!([["is", "visit:city", ["2673730"]]])
        );
        assert_eq!(filters_json(1, r#"["nope"]"#), json!(["nope"]));
    }
}
