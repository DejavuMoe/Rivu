//! 看板的图表注释端点（`annotations` 表，`platform-and-ops.md` §3.2、§4.1）。路由在 [`routes`]，由
//! [`crate::dash_api::router`] 合并；端点表见 `dash_api.rs` 顶部。
//!
//! | 端点 | 权限 |
//! |---|---|
//! | `GET /api/internal/sites/{site}/annotations?from=&to=` | `stats.read`：能在登录后的看板里看这个站点统计的人（含站点访客 viewer） |
//! | `POST /api/internal/sites/{site}/annotations` | `site.content.write`：有效角色 editor+（含站点访客 editor） |
//! | `PATCH /api/internal/sites/{site}/annotations/{id}` | 同上 |
//! | `DELETE /api/internal/sites/{site}/annotations/{id}` | 同上 |
//!
//! 注释按站点共享：谁都可以改、删别人加的注释（与目标一样）。只有登录会话能调到：公开看板、共享链接与公开查询 API
//! 都不返回注释（没有会话 401，站点不可见 404）。
//!
//! 作者：`author` 只按成员页（`GET /orgs/{org_id}/members`）会给调用者看的样子给出。组织成员能在成员页看到成员和站点访客的
//! 名称与邮箱，所以作者仍是组织成员或访客时给组织成员看；站点访客调不到成员页，只能看到自己写的注释的作者（就是自己）。
//! 其余情况 `author` 为 `null`，`mine` 说明是不是调用者自己写的。

use std::sync::Arc;

use axum::Router;
use axum::extract::{Path, Request, State};
use axum::http::StatusCode;
use axum::response::Response;
use axum::routing::{get, patch};
use jiff::civil::Date;
use rivu_auth::scope::Action;
use rivu_storage_pg::annotations::{self, AddAnnotationOutcome, AnnotationRow};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::dash_api::{AUTH_BODY_MAX, DashApi, DashError, Res, endpoint, read_json};
use crate::site_admin::{Caller, done, no_content, only};

/// 每个站点最多的注释数（未删除的）。
pub const MAX_ANNOTATIONS: usize = 500;
/// 注释文本的字符数上限（trim 之后）。
pub const TEXT_MAX_CHARS: usize = 255;
/// 注释日期允许的年份（含两端）。
const MIN_YEAR: i16 = 2000;
const MAX_YEAR: i16 = 2099;

fn annotation_json(c: &Caller, a: &AnnotationRow) -> Value {
    let me = c.session.s.user_id;
    let mine = a.created_by == Some(me);
    let show_author = a.author_email.is_some() && (mine || (c.is_member && a.author_listed));
    let author = if show_author {
        json!({"name": a.author_name, "email": a.author_email})
    } else {
        Value::Null
    };
    json!({
        "id": a.id, "date": a.date, "text": a.text, "author": author, "mine": mine,
        "created_at": a.created_at, "updated_at": a.updated_at,
    })
}

/// 文本：trim 后 1–255 个字符、不含控制字符（含换行与制表符：注释只有一行）。
fn valid_text(raw: &str) -> Res<String> {
    let s = raw.trim();
    if s.is_empty() || s.chars().count() > TEXT_MAX_CHARS {
        return Err(DashError::Invalid(format!(
            "the text must be 1 to {TEXT_MAX_CHARS} characters"
        )));
    }
    if s.chars().any(char::is_control) {
        return Err(DashError::Invalid(
            "the text must not contain control characters".into(),
        ));
    }
    Ok(s.to_owned())
}

/// 日期：`YYYY-MM-DD`，年份在 2000–2099 之间。`field` 用在错误消息里。
fn valid_date(raw: &str, field: &str) -> Res<String> {
    let bad = || {
        DashError::Invalid(format!(
            "the {field} must be a date (YYYY-MM-DD) between {MIN_YEAR}-01-01 and {MAX_YEAR}-12-31"
        ))
    };
    if raw.len() != 10 {
        return Err(bad());
    }
    let d: Date = raw.parse().map_err(|_| bad())?;
    if !(MIN_YEAR..=MAX_YEAR).contains(&d.year()) {
        return Err(bad());
    }
    Ok(d.to_string())
}

/// 查询串里的 `from`、`to`（都可选，含两端）；`from` 晚于 `to` 是 400。
fn list_range(req: &Request) -> Res<(Option<String>, Option<String>)> {
    let (mut from, mut to) = (None, None);
    if let Some(q) = req.uri().query() {
        for (k, v) in url::form_urlencoded::parse(q.as_bytes()) {
            match k.as_ref() {
                "from" => from = Some(valid_date(&v, "from")?),
                "to" => to = Some(valid_date(&v, "to")?),
                _ => {}
            }
        }
    }
    if let (Some(f), Some(t)) = (&from, &to)
        && f > t
    {
        return Err(DashError::Invalid("from must not be after to".into()));
    }
    Ok((from, to))
}

async fn list_annotations(
    State(api): State<Arc<DashApi>>,
    Path(site): Path<String>,
    req: Request,
) -> Res<Response> {
    let c = api
        .authorize_admin(req.headers(), &site, Action::StatsRead, false)
        .await?;
    let (from, to) = list_range(&req)?;
    let rows =
        annotations::list_annotations(&api.pg, c.site.id, from.as_deref(), to.as_deref()).await?;
    let body = json!({
        "annotations": rows.iter().map(|a| annotation_json(&c, a)).collect::<Vec<_>>(),
        "max_annotations": MAX_ANNOTATIONS,
    });
    Ok(done(&api, &c, StatusCode::OK, body))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NewBody {
    date: String,
    text: String,
}

async fn add_annotation(
    State(api): State<Arc<DashApi>>,
    Path(site): Path<String>,
    req: Request,
) -> Res<Response> {
    let headers = req.headers().clone();
    let c = api
        .authorize_admin(&headers, &site, Action::SiteContentWrite, false)
        .await?;
    let body: NewBody = read_json(req, AUTH_BODY_MAX).await?;
    let date = valid_date(&body.date, "date")?;
    let text = valid_text(&body.text)?;
    let outcome = annotations::add_annotation(
        &api.pg,
        c.site.id,
        &date,
        &text,
        c.session.s.user_id,
        MAX_ANNOTATIONS,
    )
    .await?;
    let id = match outcome {
        AddAnnotationOutcome::Added(id) => id,
        AddAnnotationOutcome::LimitReached { limit } => {
            return Err(DashError::Conflict(
                "limit_reached",
                format!("this site already has the maximum of {limit} annotations"),
            ));
        }
        AddAnnotationOutcome::SiteNotFound => return Err(DashError::NotFound),
    };
    let row = annotations::get_annotation(&api.pg, c.site.id, id)
        .await?
        .ok_or(DashError::Internal)?;
    tracing::info!(
        user_id = c.session.s.user_id,
        site_id = c.site.id,
        annotation_id = id,
        "添加注释"
    );
    Ok(done(
        &api,
        &c,
        StatusCode::CREATED,
        json!({"annotation": annotation_json(&c, &row)}),
    ))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PatchBody {
    #[serde(default)]
    date: Option<String>,
    #[serde(default)]
    text: Option<String>,
}

/// 路径里的注释 id：不是整数的 id 不可能存在，404（不是 400）。
fn parse_id(raw: &str) -> Res<i64> {
    raw.parse().map_err(|_| DashError::NotFound)
}

async fn update_annotation(
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
    if body.date.is_none() && body.text.is_none() {
        return Err(DashError::Invalid("give at least one of date, text".into()));
    }
    let date = body
        .date
        .as_deref()
        .map(|d| valid_date(d, "date"))
        .transpose()?;
    let text = body.text.as_deref().map(valid_text).transpose()?;
    if !annotations::update_annotation(&api.pg, c.site.id, id, text.as_deref(), date.as_deref())
        .await?
    {
        return Err(DashError::NotFound);
    }
    let row = annotations::get_annotation(&api.pg, c.site.id, id)
        .await?
        .ok_or(DashError::NotFound)?;
    tracing::info!(
        user_id = c.session.s.user_id,
        site_id = c.site.id,
        annotation_id = id,
        "修改注释"
    );
    Ok(done(
        &api,
        &c,
        StatusCode::OK,
        json!({"annotation": annotation_json(&c, &row)}),
    ))
}

async fn delete_annotation(
    State(api): State<Arc<DashApi>>,
    Path((site, id)): Path<(String, String)>,
    req: Request,
) -> Res<Response> {
    let c = api
        .authorize_admin(req.headers(), &site, Action::SiteContentWrite, false)
        .await?;
    let id = parse_id(&id)?;
    if !annotations::remove_annotation(&api.pg, c.site.id, id).await? {
        return Err(DashError::NotFound);
    }
    tracing::info!(
        user_id = c.session.s.user_id,
        site_id = c.site.id,
        annotation_id = id,
        "删除注释"
    );
    Ok(no_content(&api, &c))
}

pub(crate) fn routes(api: &Arc<DashApi>) -> Router {
    let a = api;
    Router::new()
        .merge(endpoint(
            a,
            "internal.annotations",
            "/api/internal/sites/{site}/annotations",
            only(
                get(list_annotations).post(add_annotation),
                "GET, HEAD, POST",
            ),
        ))
        .merge(endpoint(
            a,
            "internal.annotation",
            "/api/internal/sites/{site}/annotations/{id}",
            only(
                patch(update_annotation).delete(delete_annotation),
                "PATCH, DELETE",
            ),
        ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn texts_are_trimmed_bounded_and_single_line() {
        assert_eq!(valid_text("  Released v2.0 ").unwrap(), "Released v2.0");
        assert_eq!(valid_text("发布 v2.0").unwrap(), "发布 v2.0");
        assert!(valid_text(&"字".repeat(255)).is_ok());
        assert!(valid_text(&"字".repeat(256)).is_err());
        for bad in ["", "   ", "a\nb", "a\tb", "a\u{7}b", "a\u{9f}b"] {
            assert!(valid_text(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn dates_are_plain_local_days_in_a_sane_range() {
        assert_eq!(valid_date("2026-11-20", "date").unwrap(), "2026-11-20");
        assert!(valid_date("2000-01-01", "date").is_ok());
        assert!(valid_date("2099-12-31", "date").is_ok());
        for bad in [
            "",
            "2026-11-31",
            "2026-1-20",
            "1999-12-31",
            "2100-01-01",
            "2026-11-20T00:00:00Z",
            "20261120",
            "today",
        ] {
            assert!(valid_date(bad, "date").is_err(), "{bad:?}");
        }
    }
}
