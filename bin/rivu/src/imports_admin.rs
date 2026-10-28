//! 看板的数据导入端点（`site_imports` 表；导入引擎见 `rivu_jobs::import_plausible`）。路由在 [`routes`]，由
//! [`crate::dash_api::router`] 合并。
//!
//! | 端点 | 权限 |
//! |---|---|
//! | `GET /api/internal/sites/{site}/imports` | `site.settings.write`：组织角色 owner、admin |
//! | `POST /api/internal/sites/{site}/imports?file_name=…` | 同上；请求体是 Plausible 导出的 zip 原样字节 |
//! | `DELETE /api/internal/sites/{site}/imports/{id}` | 同上 |
//!
//! 上传后先建记录（`importing`）并返回 202，导入在后台跑；看板轮询列表直到状态变成 `completed` 或 `failed`。
//! 一个站点同时只有一个进行中的导入（409 `import_running`）。导入完成与删除导入都让站点的结果缓存失效。
//! 进程在导入途中退出时，下次启动由 `recover_interrupted` 清理并置为失败。

use std::sync::Arc;

use axum::Router;
use axum::extract::{Path, Request, State};
use axum::http::StatusCode;
use axum::response::Response;
use axum::routing::{delete, get};
use jiff::tz::TimeZone;
use rivu_auth::scope::Action;
use rivu_query::TenantScope;
use rivu_storage_pg::imports::{self, ImportRow};
use rivu_storage_pg::sites;
use serde_json::{Value, json};

use crate::dash_api::{DashApi, DashError, Res, endpoint};
use crate::query_api;
use crate::site_admin::{done, no_content, only};

/// 上传的上限。Plausible 导出是压缩过的日汇总，几年的数据也只有几十 MB。
pub const IMPORT_BODY_MAX: usize = 128 << 20;
/// 文件名的字符数上限（只用于展示）。
const FILE_NAME_MAX_CHARS: usize = 200;

fn import_json(i: &ImportRow) -> Value {
    json!({
        "id": i.id, "source": i.source, "status": i.status, "file_name": i.file_name,
        "start_date": i.start_date, "end_date": i.end_date, "rows_imported": i.rows_imported,
        "error_code": i.error_code, "imported_by": i.imported_by,
        "created_at": i.created_at, "completed_at": i.completed_at,
    })
}

fn maintainer(api: &DashApi) -> Res<&clickhouse::Client> {
    api.maintainer.as_ref().ok_or_else(|| {
        tracing::error!("导入端点没有 rivu_maintainer 客户端");
        DashError::Internal
    })
}

/// 展示用的文件名：去掉路径部分与控制字符，截断。
fn clean_file_name(raw: Option<&str>) -> String {
    let base = raw
        .unwrap_or("")
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or("")
        .chars()
        .filter(|c| !c.is_control())
        .take(FILE_NAME_MAX_CHARS)
        .collect::<String>();
    let base = base.trim();
    if base.is_empty() {
        "export.zip".to_owned()
    } else {
        base.to_owned()
    }
}

async fn list_imports(
    State(api): State<Arc<DashApi>>,
    Path(site): Path<String>,
    req: Request,
) -> Res<Response> {
    let c = api
        .authorize_admin(req.headers(), &site, Action::SiteSettingsWrite, true)
        .await?;
    let rows = imports::list(&api.pg, c.site.id).await?;
    let view = sites::admin_view(&api.pg, c.site.id)
        .await?
        .ok_or(DashError::NotFound)?;
    // 站点自己的数据从哪天开始：导入只写这天之前的日子（页面提示用）
    let tz = TimeZone::get(&view.timezone).unwrap_or(TimeZone::UTC);
    let state = rivu_query::rollup::build_state(&api.reader, &TenantScope::new(c.site.id), &tz)
        .await
        .map_err(|e| DashError::Fail(query_api::Fail::Query(e), api.dashboard_timeout_secs))?;
    Ok(done(
        &api,
        &c,
        StatusCode::OK,
        json!({
            "imports": rows.iter().map(import_json).collect::<Vec<_>>(),
            "native_from": state.native_from.map(|d| d.to_string()),
        }),
    ))
}

async fn create_import(
    State(api): State<Arc<DashApi>>,
    Path(site): Path<String>,
    req: Request,
) -> Res<Response> {
    let headers = req.headers().clone();
    let file_name = req.uri().query().and_then(|q| {
        url::form_urlencoded::parse(q.as_bytes())
            .find(|(k, _)| k == "file_name")
            .map(|(_, v)| v.into_owned())
    });
    let c = api
        .authorize_admin(&headers, &site, Action::SiteSettingsWrite, true)
        .await?;
    let ch = maintainer(&api)?.clone();
    let view = sites::admin_view(&api.pg, c.site.id)
        .await?
        .ok_or(DashError::NotFound)?;
    let zip = query_api::read_body(req, IMPORT_BODY_MAX)
        .await
        .map_err(|f| DashError::Fail(f, 0))?;
    if zip.is_empty() {
        return Err(DashError::Invalid(
            "the request body must be the export zip".into(),
        ));
    }
    let file_name = clean_file_name(file_name.as_deref());
    let Some(id) = imports::create(
        &api.pg,
        c.site.id,
        "plausible_csv",
        &file_name,
        Some(c.session.s.user_id),
    )
    .await?
    else {
        return Err(DashError::Conflict(
            "import_running",
            "another import for this site is still running".into(),
        ));
    };
    tracing::info!(
        user_id = c.session.s.user_id,
        site_id = c.site.id,
        import_id = id,
        bytes = zip.len(),
        "开始导入 Plausible 导出"
    );
    let (pg, cache, site_id) = (api.pg.clone(), api.cache.clone(), c.site.id);
    tokio::spawn(async move {
        let res = rivu_jobs::import_plausible::execute(
            &pg,
            &ch,
            id,
            site_id,
            &view.timezone,
            zip.to_vec(),
        )
        .await;
        if res.is_ok()
            && let Some(cache) = cache
        {
            cache.invalidate_site(site_id);
        }
    });
    let row = imports::get(&api.pg, c.site.id, id)
        .await?
        .ok_or(DashError::Internal)?;
    Ok(done(
        &api,
        &c,
        StatusCode::ACCEPTED,
        json!({"import": import_json(&row)}),
    ))
}

async fn delete_import(
    State(api): State<Arc<DashApi>>,
    Path((site, id)): Path<(String, String)>,
    req: Request,
) -> Res<Response> {
    let c = api
        .authorize_admin(req.headers(), &site, Action::SiteSettingsWrite, true)
        .await?;
    let id: i64 = id.parse().map_err(|_| DashError::NotFound)?;
    let row = imports::get(&api.pg, c.site.id, id)
        .await?
        .ok_or(DashError::NotFound)?;
    if row.status == "importing" {
        return Err(DashError::Conflict(
            "import_running",
            "the import is still running".into(),
        ));
    }
    if row.status == "completed" {
        let import_id = u64::try_from(id).map_err(|_| DashError::NotFound)?;
        rivu_query::rollup::delete_import(maintainer(&api)?, &TenantScope::new(c.site.id), import_id)
            .await
            .map_err(|e| {
                tracing::error!(site_id = c.site.id, import_id = id, error = %e, "删除导入的汇总行失败");
                DashError::Internal
            })?;
    }
    imports::delete(&api.pg, c.site.id, id).await?;
    if let Some(cache) = &api.cache {
        cache.invalidate_site(c.site.id);
    }
    tracing::info!(
        user_id = c.session.s.user_id,
        site_id = c.site.id,
        import_id = id,
        "删除导入"
    );
    Ok(no_content(&api, &c))
}

pub(crate) fn routes(api: &Arc<DashApi>) -> Router {
    let a = api;
    Router::new()
        .merge(endpoint(
            a,
            "internal.imports",
            "/api/internal/sites/{site}/imports",
            only(get(list_imports).post(create_import), "GET, HEAD, POST"),
        ))
        .merge(endpoint(
            a,
            "internal.import_delete",
            "/api/internal/sites/{site}/imports/{id}",
            only(delete(delete_import), "DELETE"),
        ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_names_are_display_only() {
        assert_eq!(
            clean_file_name(Some("blog_20261028.zip")),
            "blog_20261028.zip"
        );
        assert_eq!(clean_file_name(Some("C:\\Users\\a\\x.zip")), "x.zip");
        assert_eq!(clean_file_name(Some("../../etc/x.zip")), "x.zip");
        assert_eq!(clean_file_name(Some("  ")), "export.zip");
        assert_eq!(clean_file_name(None), "export.zip");
        assert_eq!(clean_file_name(Some("a\u{0}b.zip")), "ab.zip");
    }
}
