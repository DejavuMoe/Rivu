//! 看板的站点管理端点（建站、设置、白名单、屏蔽规则、共享链接、软删除与恢复、按日期范围删除统计数据）。路由在 [`routes`]，由
//! [`crate::dash_api::router`] 合并；端点表见 `dash_api.rs` 顶部，字段与错误体的约定见 `platform-and-ops.md` §4。
//!
//! # 权限
//!
//! - 「可见」= 站点未删除、组织未删除，且调用者是组织成员或站点访客；不可见一律 404（与不存在相同）；
//! - 可见但角色不够：403 `forbidden`（站点已经可见，不泄露存在性）；没有会话 401；
//! - 角色判定全部走 `rivu_auth::scope::allowed`：读（设置、屏蔽规则、共享链接列表）用有效角色
//!   （组织角色与站点访客角色取高者），写只看组织角色（站点访客永远不够）；
//! - 建站看目标组织里的组织角色，不是该组织成员 404。
//!
//! 写操作记 `info!`（用户 id、站点 id、动作），不记密码、slug 与请求体。

use std::sync::Arc;

use axum::Router;
use axum::extract::{Path, Request, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{MethodRouter, delete, get, patch, post};
use jiff::Timestamp;
use jiff::tz::TimeZone;
use rivu_auth::password::validate_password;
use rivu_auth::scope::{Action, OrgRole, allowed};
use rivu_auth::session;
use rivu_common::config::MIN_RETENTION_DAYS;
use rivu_geo::GeoCapability;
use rivu_ingest::shield::{RuleAction, RuleType, normalize_value};
use rivu_storage_pg::access::{self, SiteRef, UserSiteRoles, VisibleSite};
use rivu_storage_pg::shared_links::{self, LinkAdminRow, NewLink};
use rivu_storage_pg::shield::{self, AddOutcome, NewRule, RuleAdminRow};
use rivu_storage_pg::sites::{self, NewSite, SiteOwner, SitePatch, TrackerSwitches};
use rivu_storage_pg::stats_deletions::{self, RestoreOutcome};
use serde::{Deserialize, Deserializer};
use serde_json::{Value, json};

use crate::dash_api::{
    AUTH_BODY_MAX, DashApi, DashError, Res, SessionCtx, clean_name, effective_role, endpoint,
    json_response, method_not_allowed, ns, read_json, site_list_item,
};
use crate::shield_cmd::DESCRIPTION_MAX_CHARS;

/// `ingest_rate_limit_per_min` 的上限（全局默认更高时取全局默认，保证默认值本身是合法取值）。
const INGEST_RATE_LIMIT_MAX_PER_MIN: u32 = 1_000_000;

// ------------------------------------------------------------------------------------------
// 鉴权
// ------------------------------------------------------------------------------------------

/// 通过鉴权的调用者与站点。
pub(crate) struct Caller {
    pub(crate) session: SessionCtx,
    pub(crate) site: SiteOwner,
    /// 有效角色：max(组织角色, 站点访客角色)。
    role: OrgRole,
    /// 调用者是站点所属组织的成员（不只是站点访客）。
    pub(crate) is_member: bool,
}

impl DashApi {
    /// 站点管理端点的鉴权：会话（401）→ 站点可见（404）→ 角色够不够（403）。
    /// `org_only`：只认组织角色（写动作与 `shared_link.manage`：站点访客不是组织成员）。
    pub(crate) async fn authorize_admin(
        &self,
        headers: &axum::http::HeaderMap,
        site_seg: &str,
        action: Action,
        org_only: bool,
    ) -> Res<Caller> {
        let session = self.require_session(headers).await?;
        let site_ref = SiteRef::parse(site_seg).ok_or(DashError::NotFound)?;
        let site = access::find_site(&self.pg, &site_ref)
            .await?
            .ok_or(DashError::NotFound)?;
        let roles = access::user_site_roles(&self.pg, session.s.user_id, &site).await?;
        let role = effective_role(&roles).ok_or(DashError::NotFound)?;
        check_role(&roles, role, action, org_only)?;
        Ok(Caller {
            session,
            site,
            role,
            is_member: roles.org_role.is_some(),
        })
    }
}

/// 角色矩阵判定：`org_only` 时只看组织角色（没有组织角色 = 站点访客 = 403）。
fn check_role(
    roles: &UserSiteRoles,
    effective: OrgRole,
    action: Action,
    org_only: bool,
) -> Res<()> {
    let role = if org_only {
        roles.org_role.as_deref().and_then(OrgRole::parse)
    } else {
        Some(effective)
    };
    match role {
        Some(r) if allowed(r, action) => Ok(()),
        _ => Err(DashError::Forbidden),
    }
}

pub(crate) fn done(api: &DashApi, c: &Caller, status: StatusCode, body: Value) -> Response {
    api.finish(json_response(status, body), &c.session)
}

pub(crate) fn no_content(api: &DashApi, c: &Caller) -> Response {
    api.finish(ns(StatusCode::NO_CONTENT.into_response()), &c.session)
}

// ------------------------------------------------------------------------------------------
// 取值的校验与规范化
// ------------------------------------------------------------------------------------------

const BAD_DOMAIN: &str =
    "the domain is not a valid hostname (letters, digits, `-` and `.` only, no wildcards)";
const WWW_LEFT: &str = "the domain still starts with `www.` after normalization";

/// 建站的域名规范化：trim、小写、去 `scheme://`、去路径/查询/片段、去端口、去末尾 `.`、去一个前导 `www.`，
/// 结果须是不含通配的合法 hostname，且不能仍以 `www.` 开头（采集侧会再去一次，存下去永远匹配不到）。
fn normalize_domain(raw: &str) -> Result<String, &'static str> {
    let s = raw.trim().to_lowercase();
    if s.is_empty() || s.len() > 2048 {
        return Err(BAD_DOMAIN);
    }
    let s = match s.split_once("://") {
        Some((scheme, rest))
            if !scheme.is_empty()
                && scheme
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'-' | b'.')) =>
        {
            rest
        }
        _ => s.as_str(),
    };
    let s = s.split(['/', '?', '#']).next().unwrap_or("");
    let s = match s.rsplit_once(':') {
        Some((host, port)) if port.bytes().all(|b| b.is_ascii_digit()) => host,
        _ => s,
    };
    let s = s.trim_end_matches('.');
    let s = s.strip_prefix("www.").unwrap_or(s);
    if s.contains('*') || s.len() > 253 {
        return Err(BAD_DOMAIN);
    }
    sites::validate_hostname_pattern(s).map_err(|_| BAD_DOMAIN)?;
    if s.starts_with("www.") {
        return Err(WWW_LEFT);
    }
    Ok(s.to_owned())
}

/// 白名单模式（允许 `*.` 通配）：trim + 小写后按 PG 的 CHECK 校验；`www.` 开头（含 `*.www.`）拒绝。
fn normalize_pattern(raw: &str) -> Result<String, &'static str> {
    const BAD: &str = "the pattern is not a valid hostname pattern (lowercase letters, digits, `-` and `.`; a wildcard only as a leading `*.`)";
    let p = raw.trim().to_lowercase();
    if p.len() > 253 {
        return Err(BAD);
    }
    sites::validate_hostname_pattern(&p).map_err(|_| BAD)?;
    let body = p.strip_prefix("*.").unwrap_or(&p);
    if body.starts_with("www.") {
        return Err(
            "the pattern must not start with `www.` (hostnames are matched after a leading `www.` is stripped, so it would never match)",
        );
    }
    Ok(p)
}

fn valid_timezone(name: &str) -> Res<()> {
    TimeZone::get(name)
        .map(|_| ())
        .map_err(|_| DashError::Invalid("the timezone is not a valid IANA time zone name".into()))
}

/// 名称：trim、非空、≤ 100 字符、无控制字符。
fn required_name(raw: &str) -> Res<String> {
    clean_name(Some(raw))?.ok_or_else(|| DashError::Invalid("the name must not be empty".into()))
}

// ------------------------------------------------------------------------------------------
// POST /api/internal/orgs/{org_id}/sites
// ------------------------------------------------------------------------------------------

#[derive(Deserialize)]
struct CreateSiteBody {
    domain: String,
    #[serde(default)]
    name: Option<String>,
    timezone: String,
}

async fn create_site(
    State(api): State<Arc<DashApi>>,
    Path(org_seg): Path<String>,
    req: Request,
) -> Res<Response> {
    let ctx = api.require_session(req.headers()).await?;
    let org_id: i64 = org_seg.parse().map_err(|_| DashError::NotFound)?;
    // 不是该组织（未删除）的成员：404，与组织不存在相同
    let org = rivu_storage_pg::users::list_user_orgs(&api.pg, ctx.s.user_id)
        .await?
        .into_iter()
        .find(|o| o.id == org_id)
        .ok_or(DashError::NotFound)?;
    let role = OrgRole::parse(&org.role)
        .filter(|r| allowed(*r, Action::SiteCreate))
        .ok_or(DashError::Forbidden)?;
    let body: CreateSiteBody = read_json(req, AUTH_BODY_MAX).await?;
    let domain = normalize_domain(&body.domain).map_err(|m| DashError::Invalid(m.into()))?;
    let name = clean_name(body.name.as_deref())?.unwrap_or_else(|| domain.clone());
    valid_timezone(&body.timezone)?;
    let created = sites::create_site_in_org(
        &api.pg,
        org_id,
        &NewSite {
            name: &name,
            timezone: &body.timezone,
            hostnames: std::slice::from_ref(&domain),
        },
    )
    .await?;
    tracing::info!(
        user_id = ctx.s.user_id,
        site_id = created.id,
        org_id,
        "创建站点"
    );
    let site = VisibleSite {
        id: created.id,
        site_key: created.site_key,
        name,
        timezone: body.timezone,
        is_public: false,
        org_id,
        org_slug: org.slug,
        org_name: org.name,
        roles: UserSiteRoles {
            org_role: Some(org.role),
            guest_role: None,
        },
    };
    Ok(api.finish(
        json_response(
            StatusCode::CREATED,
            json!({"site": site_list_item(&site, Some(role), true)}),
        ),
        &ctx,
    ))
}

// ------------------------------------------------------------------------------------------
// GET settings、PATCH、DELETE site
// ------------------------------------------------------------------------------------------

/// GET settings 与 PATCH 共用的响应体。
async fn settings_body(api: &DashApi, site_id: u64) -> Res<Value> {
    let v = sites::admin_view(&api.pg, site_id)
        .await?
        .ok_or(DashError::NotFound)?;
    let hostnames = sites::list_hostnames(&api.pg, site_id).await?;
    let max = INGEST_RATE_LIMIT_MAX_PER_MIN.max(api.ingest_rate_default.unwrap_or(0));
    Ok(json!({
        "site": {
            "id": v.id, "site_key": v.site_key, "name": v.name, "timezone": v.timezone,
            "is_public": v.is_public, "created_at": v.created_at,
        },
        "settings": {
            "week_starts_on": v.week_starts_on,
            "collect_enabled": v.collect_enabled,
            "tracker": tracker_json(&v.tracker),
            "ingest_rate_limit_per_min": v.ingest_rate_limit_per_min,
            "geo_level": v.geo_level,
            "retention_days": v.retention_days,
            "spike_threshold": v.spike_threshold,
        },
        // Traffic spike alerts only send mail when SMTP is configured.
        "mail_available": api.mailer.is_some(),
        "hostnames": hostnames.iter()
            .map(|h| json!({"pattern": h.pattern, "created_at": h.created_at}))
            .collect::<Vec<_>>(),
        "limits": {
            "ingest_rate_limit_default_per_min": api.ingest_rate_default,
            "shield_max_rules": api.shield_max_rules,
            "ingest_rate_limit_max_per_min": max,
            "retention_days_max": api.retention_days,
        },
        "geo_capability": api.geo.as_ref().and_then(|g| g.capability()).map(GeoCapability::as_str),
    }))
}

async fn get_settings(
    State(api): State<Arc<DashApi>>,
    Path(site): Path<String>,
    req: Request,
) -> Res<Response> {
    let c = api
        .authorize_admin(req.headers(), &site, Action::SiteSettingsRead, false)
        .await?;
    let body = settings_body(&api, c.site.id).await?;
    Ok(done(&api, &c, StatusCode::OK, body))
}

/// 站点脚本开关的响应形态（键名同 [`TrackerSwitches::KEYS`]）。
fn tracker_json(t: &TrackerSwitches) -> Value {
    Value::Object(
        TrackerSwitches::KEYS
            .iter()
            .map(|k| ((*k).to_owned(), Value::Bool(t.get(k).unwrap_or(false))))
            .collect(),
    )
}

/// `PATCH` 里的 `tracker`：每个键可选。
#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct TrackerPatch {
    hash: Option<bool>,
    engagement: Option<bool>,
    outbound: Option<bool>,
    downloads: Option<bool>,
    forms: Option<bool>,
    tagged: Option<bool>,
}

impl TrackerPatch {
    fn pairs(&self) -> Vec<(&'static str, bool)> {
        [
            ("hash", self.hash),
            ("engagement", self.engagement),
            ("outbound", self.outbound),
            ("downloads", self.downloads),
            ("forms", self.forms),
            ("tagged", self.tagged),
        ]
        .into_iter()
        .filter_map(|(k, v)| v.map(|v| (k, v)))
        .collect()
    }

    fn is_empty(&self) -> bool {
        self.pairs().is_empty()
    }
}

/// `Option<Option<T>>`：字段缺失 = `None`，`null` = `Some(None)`，有值 = `Some(Some(v))`。
fn double_option<'de, D, T>(d: D) -> Result<Option<Option<T>>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(d).map(Some)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PatchBody {
    name: Option<String>,
    timezone: Option<String>,
    week_starts_on: Option<i64>,
    is_public: Option<bool>,
    collect_enabled: Option<bool>,
    /// 只覆盖给出的键；未知键 400。
    tracker: Option<TrackerPatch>,
    #[serde(default, deserialize_with = "double_option")]
    ingest_rate_limit_per_min: Option<Option<i64>>,
    geo_level: Option<String>,
    /// 站点的原始数据保留天数；`null` = 用全局 `RIVU_RETENTION_DAYS`。
    #[serde(default, deserialize_with = "double_option")]
    retention_days: Option<Option<i64>>,
    /// 流量激增提醒的阈值（当前在线访客数）；`null` = 关掉。
    #[serde(default, deserialize_with = "double_option")]
    spike_threshold: Option<Option<i64>>,
}

async fn patch_site(
    State(api): State<Arc<DashApi>>,
    Path(site): Path<String>,
    req: Request,
) -> Res<Response> {
    let headers = req.headers().clone();
    let c = api
        .authorize_admin(&headers, &site, Action::SiteSettingsWrite, true)
        .await?;
    let body: PatchBody = read_json(req, AUTH_BODY_MAX).await?;
    let empty = body.name.is_none()
        && body.timezone.is_none()
        && body.week_starts_on.is_none()
        && body.is_public.is_none()
        && body.collect_enabled.is_none()
        && body.tracker.as_ref().is_none_or(TrackerPatch::is_empty)
        && body.ingest_rate_limit_per_min.is_none()
        && body.geo_level.is_none()
        && body.retention_days.is_none()
        && body.spike_threshold.is_none();
    if empty {
        return Err(DashError::Invalid("no fields to update".into()));
    }
    if body.is_public.is_some() && !allowed(c.role, Action::SitePublicToggle) {
        return Err(DashError::Forbidden);
    }
    let name = body.name.as_deref().map(required_name).transpose()?;
    if let Some(tz) = &body.timezone {
        valid_timezone(tz)?;
    }
    let week_starts_on = match body.week_starts_on {
        None => None,
        Some(n @ 0..=6) => Some(n as i16),
        Some(_) => {
            return Err(DashError::Invalid(
                "the week_starts_on must be between 0 (Sunday) and 6 (Saturday)".into(),
            ));
        }
    };
    let max = INGEST_RATE_LIMIT_MAX_PER_MIN.max(api.ingest_rate_default.unwrap_or(0));
    let ingest = match body.ingest_rate_limit_per_min {
        None => None,
        Some(None) => Some(None),
        Some(Some(n)) if (1..=i64::from(max)).contains(&n) => Some(Some(n as i32)),
        Some(Some(_)) => {
            return Err(DashError::Invalid(format!(
                "the ingest_rate_limit_per_min must be between 1 and {max}, or null for the default"
            )));
        }
    };
    if let Some(g) = &body.geo_level
        && !matches!(g.as_str(), "country" | "region" | "city")
    {
        return Err(DashError::Invalid(
            "the geo_level must be one of country, region, city".into(),
        ));
    }
    // 站点只能比全局短；下限与全局的下限相同（30 天）
    let global = api.retention_days;
    let retention = match body.retention_days {
        None => None,
        Some(None) => Some(None),
        Some(Some(n)) if (i64::from(MIN_RETENTION_DAYS)..=i64::from(global)).contains(&n) => {
            Some(Some(n as i32))
        }
        Some(Some(_)) => {
            return Err(DashError::Invalid(format!(
                "the retention_days must be between {MIN_RETENTION_DAYS} and {global}, or null for the instance default"
            )));
        }
    };
    let spike_threshold = match body.spike_threshold {
        None => None,
        Some(None) => Some(None),
        Some(Some(n)) if (1..=i64::from(i32::MAX)).contains(&n) => Some(Some(n as i32)),
        Some(Some(_)) => {
            return Err(DashError::Invalid(format!(
                "the spike_threshold must be between 1 and {}, or null to turn traffic spike alerts off",
                i32::MAX
            )));
        }
    };
    let patch = SitePatch {
        name: name.as_deref(),
        timezone: body.timezone.as_deref(),
        is_public: body.is_public,
        week_starts_on,
        collect_enabled: body.collect_enabled,
        tracker: body
            .tracker
            .as_ref()
            .map(TrackerPatch::pairs)
            .unwrap_or_default(),
        ingest_rate_limit_per_min: ingest,
        geo_level: body.geo_level.as_deref(),
        retention_days: retention,
        spike_threshold,
    };
    let mut tx = api
        .pg
        .begin_with("BEGIN ISOLATION LEVEL READ COMMITTED")
        .await
        .map_err(anyhow::Error::from)?;
    if let Some(next) = patch.timezone {
        let current = sites::lock_timezone(&mut tx, c.site.id, false)
            .await?
            .ok_or(DashError::NotFound)?;
        if current != next
            && let Some(code) = rivu_jobs::rollup::timezone_change_conflict(
                &mut tx,
                &api.reader,
                c.site.id,
                &current,
                next,
                (api.clock)(),
            )
            .await?
        {
            return Err(DashError::Conflict(
                code,
                if code == rivu_jobs::rollup::TIMEZONE_IMPORT_CONFLICT {
                    "the timezone cannot be changed while an import is running or imported dates overlap the native data in the requested timezone"
                } else {
                    "the timezone cannot be changed because the native rollup rebuild is unproven or a statistics deletion affects it"
                }.into(),
            ));
        }
    }
    if !sites::update_site(&mut tx, c.site.id, &patch).await? {
        return Err(DashError::NotFound);
    }
    tx.commit().await.map_err(anyhow::Error::from)?;
    tracing::info!(
        user_id = c.session.s.user_id,
        site_id = c.site.id,
        "更新站点设置"
    );
    let body = settings_body(&api, c.site.id).await?;
    Ok(done(&api, &c, StatusCode::OK, body))
}

#[derive(Deserialize)]
struct DeleteSiteBody {
    confirm: String,
}

async fn delete_site(
    State(api): State<Arc<DashApi>>,
    Path(site): Path<String>,
    req: Request,
) -> Res<Response> {
    let headers = req.headers().clone();
    let c = api
        .authorize_admin(&headers, &site, Action::SiteDelete, true)
        .await?;
    let body: DeleteSiteBody = read_json(req, AUTH_BODY_MAX).await?;
    let view = sites::admin_view(&api.pg, c.site.id)
        .await?
        .ok_or(DashError::NotFound)?;
    // 防误删的服务端兜底：必须与当前名称完全一致
    if body.confirm != view.name {
        return Err(DashError::Invalid(
            "the confirm value must exactly match the site name".into(),
        ));
    }
    // 同一事务里写入 scope='site' 的删除意图：恢复窗口过后由 ch_delete_batch 清除统计数据
    if !stats_deletions::soft_delete_site(
        &api.pg,
        c.site.id,
        Some(c.session.s.user_id),
        api.site_restore_days,
    )
    .await?
    {
        return Err(DashError::NotFound);
    }
    tracing::info!(
        user_id = c.session.s.user_id,
        site_id = c.site.id,
        restore_days = api.site_restore_days,
        "删除站点（软删除，恢复窗口内可恢复）"
    );
    Ok(no_content(&api, &c))
}

// ------------------------------------------------------------------------------------------
// 已删除的站点与恢复
// ------------------------------------------------------------------------------------------

/// 组织级的鉴权：不是该组织（未删除）的成员 404，角色不够 403。返回会话与组织角色。
async fn authorize_org(
    api: &DashApi,
    headers: &axum::http::HeaderMap,
    org_id: i64,
    action: Action,
) -> Res<SessionCtx> {
    let ctx = api.require_session(headers).await?;
    let org = rivu_storage_pg::users::list_user_orgs(&api.pg, ctx.s.user_id)
        .await?
        .into_iter()
        .find(|o| o.id == org_id)
        .ok_or(DashError::NotFound)?;
    OrgRole::parse(&org.role)
        .filter(|r| allowed(*r, action))
        .ok_or(DashError::Forbidden)?;
    Ok(ctx)
}

async fn list_deleted_sites(
    State(api): State<Arc<DashApi>>,
    Path(org_seg): Path<String>,
    req: Request,
) -> Res<Response> {
    let org_id: i64 = org_seg.parse().map_err(|_| DashError::NotFound)?;
    let ctx = authorize_org(&api, req.headers(), org_id, Action::SiteDelete).await?;
    let rows = stats_deletions::list_deleted_sites(&api.pg, org_id).await?;
    let sites: Vec<Value> = rows
        .iter()
        .map(|s| {
            json!({
                "id": s.id, "site_key": s.site_key, "name": s.name, "domain": s.domain,
                "deleted_at": s.deleted_at, "purge_at": s.purge_at,
            })
        })
        .collect();
    Ok(api.finish(json_response(StatusCode::OK, json!({"sites": sites})), &ctx))
}

async fn restore_site(
    State(api): State<Arc<DashApi>>,
    Path(site): Path<String>,
    req: Request,
) -> Res<Response> {
    let ctx = api.require_session(req.headers()).await?;
    // 已删除的站点不在可见性查询里：按引用找已删除的站点，再看调用者在它的组织里的角色
    let site_ref = SiteRef::parse(&site).ok_or(DashError::NotFound)?;
    let (id, key) = match &site_ref {
        SiteRef::Id(id) => (Some(*id), None),
        SiteRef::Key(k) => (None, Some(k.as_str())),
    };
    let owner = stats_deletions::find_deleted_site(&api.pg, id, key)
        .await?
        .ok_or(DashError::NotFound)?;
    let org = rivu_storage_pg::users::list_user_orgs(&api.pg, ctx.s.user_id)
        .await?
        .into_iter()
        .find(|o| o.id == owner.organization_id)
        .ok_or(DashError::NotFound)?;
    OrgRole::parse(&org.role)
        .filter(|r| allowed(*r, Action::SiteDelete))
        .ok_or(DashError::Forbidden)?;
    match stats_deletions::restore_site(&api.pg, owner.id).await? {
        RestoreOutcome::Restored => {}
        RestoreOutcome::NotFound => return Err(DashError::NotFound),
        RestoreOutcome::NotRestorable => {
            return Err(DashError::Conflict(
                "conflict",
                "this site can no longer be restored: the restore window has passed or its data is being deleted".into(),
            ));
        }
    }
    tracing::info!(user_id = ctx.s.user_id, site_id = owner.id, "恢复站点");
    let restored = access::list_visible_sites(&api.pg, ctx.s.user_id)
        .await?
        .into_iter()
        .find(|s| s.id == owner.id)
        .ok_or(DashError::Internal)?;
    let item = site_list_item(
        &restored,
        effective_role(&restored.roles),
        restored.roles.org_role.is_some(),
    );
    Ok(api.finish(json_response(StatusCode::OK, json!({"site": item})), &ctx))
}

// ------------------------------------------------------------------------------------------
// 按日期范围删除统计数据
// ------------------------------------------------------------------------------------------

/// 一次删除最多覆盖的天数（含两端）：原始数据保留 731 天（`events` 的 TTL），更长的范围没有意义。
pub(crate) const STATS_DELETE_MAX_DAYS: i64 = 731;

/// 列表最多返回的条数。
const STATS_DELETIONS_LIST_LIMIT: i64 = 50;

/// 校验日期范围（站点本地日期）：格式 `YYYY-MM-DD`、`from <= to`、`to` 不晚于站点时区的今天、
/// 跨度不超过 [`STATS_DELETE_MAX_DAYS`] 天。错误是给用户看的英文提示。
pub(crate) fn validate_stats_range(
    from: &str,
    to: &str,
    today: jiff::civil::Date,
) -> Result<(jiff::civil::Date, jiff::civil::Date), &'static str> {
    let parse = |s: &str| -> Option<jiff::civil::Date> {
        // 只接受严格的 YYYY-MM-DD
        let b = s.as_bytes();
        let shape = b.len() == 10
            && b[4] == b'-'
            && b[7] == b'-'
            && b.iter()
                .enumerate()
                .all(|(i, c)| i == 4 || i == 7 || c.is_ascii_digit());
        if !shape {
            return None;
        }
        s.parse().ok()
    };
    let (Some(f), Some(t)) = (parse(from), parse(to)) else {
        return Err("the from and to dates must be valid dates in YYYY-MM-DD format");
    };
    if f > t {
        return Err("the from date must not be after the to date");
    }
    if t > today {
        return Err("the to date must not be after today in the site's time zone");
    }
    let days = i64::from((t - f).get_days()) + 1;
    if days > STATS_DELETE_MAX_DAYS {
        return Err("the date range must span at most 731 days");
    }
    Ok((f, t))
}

fn deletion_json(d: &stats_deletions::DateRangeRow) -> Value {
    json!({
        "id": d.id, "from": d.from, "to": d.to, "status": d.status,
        "created_at": d.created_at, "completed_at": d.completed_at,
        "error_code": d.timezone_unproven.then_some(rivu_jobs::stats_delete::TIMEZONE_UNPROVEN),
    })
}

#[derive(Deserialize)]
struct StatsDeletionBody {
    from: String,
    to: String,
    confirm: String,
}

async fn create_stats_deletion(
    State(api): State<Arc<DashApi>>,
    Path(site): Path<String>,
    req: Request,
) -> Res<Response> {
    let headers = req.headers().clone();
    let c = api
        .authorize_admin(&headers, &site, Action::StatsDelete, true)
        .await?;
    let body: StatsDeletionBody = read_json(req, AUTH_BODY_MAX).await?;
    let mut tx = api.pg.begin().await.map_err(anyhow::Error::from)?;
    let timezone = sites::lock_timezone(&mut tx, c.site.id, false)
        .await?
        .ok_or(DashError::NotFound)?;
    let view = sites::admin_view(&mut *tx, c.site.id)
        .await?
        .ok_or(DashError::NotFound)?;
    let tz = TimeZone::get(&timezone).map_err(|_| stats_deletion_timezone_conflict())?;
    let today = (api.clock)().to_zoned(tz.clone()).date();
    let (from, to) = validate_stats_range(body.from.trim(), body.to.trim(), today)
        .map_err(|m| DashError::Invalid(m.into()))?;
    if body.confirm != view.name {
        return Err(DashError::Invalid(
            "the confirm value must exactly match the site name".into(),
        ));
    }
    if !rivu_jobs::stats_delete::can_erase_dates(&api.reader, c.site.id, &tz).await? {
        return Err(stats_deletion_timezone_conflict());
    }
    let utc = rivu_query::resolve::local_dates_utc_ms(&tz, from, to)
        .map_err(|e| DashError::Invalid(e.to_string()))?;
    let row = stats_deletions::create_date_range(
        &mut tx,
        c.site.id,
        &from.to_string(),
        &to.to_string(),
        &timezone,
        utc,
        Some(c.session.s.user_id),
    )
    .await?;
    tx.commit().await.map_err(anyhow::Error::from)?;
    if let Some(kick) = &api.stats_delete_kick {
        kick.notify_one();
    }
    tracing::info!(
        user_id = c.session.s.user_id,
        site_id = c.site.id,
        deletion_id = row.id,
        "请求按日期范围删除统计数据"
    );
    Ok(done(
        &api,
        &c,
        StatusCode::CREATED,
        json!({"deletion": deletion_json(&row)}),
    ))
}

fn stats_deletion_timezone_conflict() -> DashError {
    DashError::Conflict(
        rivu_jobs::stats_delete::TIMEZONE_UNPROVEN,
        "the date range cannot be erased until the timezone of the native rollups is known to match the site".into(),
    )
}

async fn list_stats_deletions(
    State(api): State<Arc<DashApi>>,
    Path(site): Path<String>,
    req: Request,
) -> Res<Response> {
    let c = api
        .authorize_admin(req.headers(), &site, Action::StatsDelete, true)
        .await?;
    let rows =
        stats_deletions::list_date_ranges(&api.pg, c.site.id, STATS_DELETIONS_LIST_LIMIT).await?;
    Ok(done(
        &api,
        &c,
        StatusCode::OK,
        json!({"deletions": rows.iter().map(deletion_json).collect::<Vec<_>>()}),
    ))
}

// ------------------------------------------------------------------------------------------
// 白名单
// ------------------------------------------------------------------------------------------

#[derive(Deserialize)]
struct HostnameBody {
    pattern: String,
}

async fn add_hostname(
    State(api): State<Arc<DashApi>>,
    Path(site): Path<String>,
    req: Request,
) -> Res<Response> {
    let headers = req.headers().clone();
    let c = api
        .authorize_admin(&headers, &site, Action::SiteSettingsWrite, true)
        .await?;
    let body: HostnameBody = read_json(req, AUTH_BODY_MAX).await?;
    let pattern = normalize_pattern(&body.pattern).map_err(|m| DashError::Invalid(m.into()))?;
    let row = sites::add_hostname(&api.pg, c.site.id, &pattern)
        .await?
        .ok_or_else(|| {
            DashError::Conflict("conflict", "this hostname pattern already exists".into())
        })?;
    tracing::info!(
        user_id = c.session.s.user_id,
        site_id = c.site.id,
        "添加站点白名单"
    );
    Ok(done(
        &api,
        &c,
        StatusCode::CREATED,
        json!({"hostname": {"pattern": row.pattern, "created_at": row.created_at}}),
    ))
}

async fn remove_hostname(
    State(api): State<Arc<DashApi>>,
    Path((site, pattern)): Path<(String, String)>,
    req: Request,
) -> Res<Response> {
    let c = api
        .authorize_admin(req.headers(), &site, Action::SiteSettingsWrite, true)
        .await?;
    if !sites::remove_hostname(&api.pg, c.site.id, &pattern).await? {
        return Err(DashError::NotFound);
    }
    tracing::info!(
        user_id = c.session.s.user_id,
        site_id = c.site.id,
        "删除站点白名单"
    );
    Ok(no_content(&api, &c))
}

// ------------------------------------------------------------------------------------------
// 屏蔽规则
// ------------------------------------------------------------------------------------------

fn rule_json(r: &RuleAdminRow) -> Value {
    json!({
        "id": r.rule.id, "type": r.rule.kind, "action": r.rule.action, "value": r.rule.value,
        "description": r.rule.description, "created_at": r.created_at,
    })
}

async fn list_shield(
    State(api): State<Arc<DashApi>>,
    Path(site): Path<String>,
    req: Request,
) -> Res<Response> {
    let c = api
        .authorize_admin(req.headers(), &site, Action::SiteSettingsRead, false)
        .await?;
    let rules = shield::list_rules_admin(&api.pg, c.site.id).await?;
    Ok(done(
        &api,
        &c,
        StatusCode::OK,
        json!({"rules": rules.iter().map(rule_json).collect::<Vec<_>>(),
               "max_rules": api.shield_max_rules}),
    ))
}

#[derive(Deserialize)]
struct ShieldBody {
    #[serde(rename = "type")]
    kind: String,
    action: String,
    value: String,
    #[serde(default)]
    description: Option<String>,
}

/// 取值非法时给用户看的英文提示（按类型固定，不回显输入；规范化本身与命令行共用 `normalize_value`）。
fn shield_value_hint(kind: RuleType) -> &'static str {
    match kind {
        RuleType::Ip => {
            "the value is not a valid single IPv4 or IPv6 address (use the cidr type for ranges)"
        }
        RuleType::Cidr => {
            "the value is not a valid CIDR range with a prefix length, e.g. 10.0.0.0/8"
        }
        RuleType::Country => {
            "the value is not a two-letter country code (ISO 3166-1 alpha-2, e.g. CN; ZZ for unknown)"
        }
        RuleType::Hostname => {
            "the value is not a valid hostname pattern, e.g. example.com or *.example.com"
        }
        RuleType::Page => {
            "the value is not a valid page pattern: it must start with `/`, be at most 250 bytes and contain no whitespace"
        }
    }
}

async fn add_shield(
    State(api): State<Arc<DashApi>>,
    Path(site): Path<String>,
    req: Request,
) -> Res<Response> {
    let headers = req.headers().clone();
    let c = api
        .authorize_admin(&headers, &site, Action::SiteSettingsWrite, true)
        .await?;
    let body: ShieldBody = read_json(req, AUTH_BODY_MAX).await?;
    let kind = RuleType::parse(&body.kind).ok_or_else(|| {
        DashError::Invalid("the type must be one of ip, cidr, country, hostname, page".into())
    })?;
    let action = RuleAction::parse(&body.action)
        .ok_or_else(|| DashError::Invalid("the action must be one of deny, allow".into()))?;
    let value = normalize_value(kind, &body.value)
        .map_err(|_| DashError::Invalid(shield_value_hint(kind).into()))?;
    let description = body
        .description
        .as_deref()
        .map(str::trim)
        .filter(|d| !d.is_empty());
    if description.is_some_and(|d| d.chars().count() > DESCRIPTION_MAX_CHARS) {
        return Err(DashError::Invalid(format!(
            "the description must be at most {DESCRIPTION_MAX_CHARS} characters"
        )));
    }
    let outcome = shield::add_rule(
        &api.pg,
        &NewRule {
            site_id: c.site.id,
            kind: kind.as_str(),
            action: action.as_str(),
            value: &value,
            description,
            created_by: Some(c.session.s.user_id),
        },
        api.shield_max_rules,
    )
    .await?;
    let id = match outcome {
        AddOutcome::Added(id) => id,
        AddOutcome::Duplicate => {
            return Err(DashError::Conflict(
                "conflict",
                "a rule with the same type and value already exists (delete it first to change its action)".into(),
            ));
        }
        AddOutcome::LimitReached { limit } => {
            return Err(DashError::Conflict(
                "limit_reached",
                format!("this site already has the maximum of {limit} shield rules"),
            ));
        }
        AddOutcome::SiteNotFound => return Err(DashError::NotFound),
    };
    let row = shield::get_rule_admin(&api.pg, c.site.id, &id)
        .await?
        .ok_or(DashError::Internal)?;
    tracing::info!(
        user_id = c.session.s.user_id,
        site_id = c.site.id,
        "添加屏蔽规则"
    );
    Ok(done(
        &api,
        &c,
        StatusCode::CREATED,
        json!({"rule": rule_json(&row)}),
    ))
}

async fn delete_shield(
    State(api): State<Arc<DashApi>>,
    Path((site, id)): Path<(String, String)>,
    req: Request,
) -> Res<Response> {
    let c = api
        .authorize_admin(req.headers(), &site, Action::SiteSettingsWrite, true)
        .await?;
    // 不是 UUID 的 id 不可能存在：404（不是 400）
    if !shield::is_uuid_text(&id) || !shield::remove_rule_by_id(&api.pg, c.site.id, &id).await? {
        return Err(DashError::NotFound);
    }
    tracing::info!(
        user_id = c.session.s.user_id,
        site_id = c.site.id,
        "删除屏蔽规则"
    );
    Ok(no_content(&api, &c))
}

// ------------------------------------------------------------------------------------------
// 共享链接
// ------------------------------------------------------------------------------------------

fn link_json(l: &LinkAdminRow) -> Value {
    json!({
        "id": l.id, "name": l.name, "slug": l.slug, "has_password": l.has_password,
        "expires_at": l.expires_at, "created_at": l.created_at,
        "created_by": l.created_by.as_ref()
            .map(|u| json!({"id": u.id, "name": u.name, "email": u.email})),
    })
}

async fn list_links(
    State(api): State<Arc<DashApi>>,
    Path(site): Path<String>,
    req: Request,
) -> Res<Response> {
    let c = api
        .authorize_admin(req.headers(), &site, Action::SiteSettingsRead, false)
        .await?;
    let links = shared_links::list_for_site(&api.pg, c.site.id).await?;
    Ok(done(
        &api,
        &c,
        StatusCode::OK,
        json!({"links": links.iter().map(link_json).collect::<Vec<_>>()}),
    ))
}

#[derive(Deserialize)]
struct LinkBody {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    password: Option<String>,
    #[serde(default)]
    expires_at: Option<String>,
}

async fn create_link(
    State(api): State<Arc<DashApi>>,
    Path(site): Path<String>,
    req: Request,
) -> Res<Response> {
    let headers = req.headers().clone();
    let c = api
        .authorize_admin(&headers, &site, Action::SharedLinkManage, true)
        .await?;
    let body: LinkBody = read_json(req, AUTH_BODY_MAX).await?;
    let name = clean_name(body.name.as_deref())?;
    // 密码规则与命令行一致（`validate_password`：12 到 128 个字符），哈希走 argon2id 服务
    let password_hash = match &body.password {
        None => None,
        Some(p) => {
            validate_password(p).map_err(|m| DashError::Invalid(m.into()))?;
            Some(
                api.passwords
                    .hash(p.clone())
                    .await
                    .map_err(|e| anyhow::anyhow!("{e}"))?,
            )
        }
    };
    let expires_at_unix = match &body.expires_at {
        None => None,
        Some(raw) => {
            let at: Timestamp = raw.trim().parse().map_err(|_| {
                DashError::Invalid("the expires_at must be an RFC 3339 timestamp".into())
            })?;
            if at <= Timestamp::now() {
                return Err(DashError::Invalid(
                    "the expires_at must be in the future".into(),
                ));
            }
            Some(at.as_second())
        }
    };
    let site_key = {
        let v = sites::admin_view(&api.pg, c.site.id)
            .await?
            .ok_or(DashError::NotFound)?;
        v.site_key
    };
    let slug = session::generate().plaintext().to_owned();
    let id = shared_links::create(
        &api.pg,
        &NewLink {
            site_key: &site_key,
            slug: &slug,
            password_hash: password_hash.as_deref(),
            expires_at_unix,
            created_by: Some(c.session.s.user_id),
            name: name.as_deref(),
        },
    )
    .await?;
    let row = shared_links::get_for_site(&api.pg, c.site.id, id)
        .await?
        .ok_or(DashError::Internal)?;
    tracing::info!(
        user_id = c.session.s.user_id,
        site_id = c.site.id,
        link_id = id,
        "创建共享链接"
    );
    Ok(done(
        &api,
        &c,
        StatusCode::CREATED,
        json!({"link": link_json(&row)}),
    ))
}

async fn revoke_link(
    State(api): State<Arc<DashApi>>,
    Path((site, id)): Path<(String, String)>,
    req: Request,
) -> Res<Response> {
    let c = api
        .authorize_admin(req.headers(), &site, Action::SharedLinkManage, true)
        .await?;
    let id: i64 = id.parse().map_err(|_| DashError::NotFound)?;
    if !shared_links::revoke_for_site(&api.pg, c.site.id, id).await? {
        return Err(DashError::NotFound);
    }
    tracing::info!(
        user_id = c.session.s.user_id,
        site_id = c.site.id,
        link_id = id,
        "吊销共享链接"
    );
    Ok(no_content(&api, &c))
}

// ------------------------------------------------------------------------------------------
// 路由
// ------------------------------------------------------------------------------------------

/// 一个路由允许的方法之外一律 405 + Allow。
pub(crate) fn only(
    h: MethodRouter<Arc<DashApi>>,
    allow: &'static str,
) -> MethodRouter<Arc<DashApi>> {
    h.fallback(move || async move { method_not_allowed(allow).await })
}

pub(crate) fn routes(api: &Arc<DashApi>) -> Router {
    let a = api;
    Router::new()
        .merge(endpoint(
            a,
            "internal.site_create",
            "/api/internal/orgs/{org_id}/sites",
            only(post(create_site), "POST"),
        ))
        .merge(endpoint(
            a,
            "internal.site_settings",
            "/api/internal/sites/{site}/settings",
            only(get(get_settings), "GET, HEAD"),
        ))
        .merge(endpoint(
            a,
            "internal.site_update",
            "/api/internal/sites/{site}",
            only(patch(patch_site).delete(delete_site), "PATCH, DELETE"),
        ))
        .merge(endpoint(
            a,
            "internal.deleted_sites",
            "/api/internal/orgs/{org_id}/deleted-sites",
            only(get(list_deleted_sites), "GET, HEAD"),
        ))
        .merge(endpoint(
            a,
            "internal.site_restore",
            "/api/internal/sites/{site}/restore",
            only(post(restore_site), "POST"),
        ))
        .merge(endpoint(
            a,
            "internal.stats_deletions",
            "/api/internal/sites/{site}/stats-deletions",
            only(
                get(list_stats_deletions).post(create_stats_deletion),
                "GET, HEAD, POST",
            ),
        ))
        .merge(endpoint(
            a,
            "internal.hostname_add",
            "/api/internal/sites/{site}/hostnames",
            only(post(add_hostname), "POST"),
        ))
        .merge(endpoint(
            a,
            "internal.hostname_delete",
            "/api/internal/sites/{site}/hostnames/{pattern}",
            only(delete(remove_hostname), "DELETE"),
        ))
        .merge(endpoint(
            a,
            "internal.shield",
            "/api/internal/sites/{site}/shield",
            only(get(list_shield).post(add_shield), "GET, HEAD, POST"),
        ))
        .merge(endpoint(
            a,
            "internal.shield_delete",
            "/api/internal/sites/{site}/shield/{id}",
            only(delete(delete_shield), "DELETE"),
        ))
        .merge(endpoint(
            a,
            "internal.shared_links",
            "/api/internal/sites/{site}/shared-links",
            only(get(list_links).post(create_link), "GET, HEAD, POST"),
        ))
        .merge(endpoint(
            a,
            "internal.shared_link_delete",
            "/api/internal/sites/{site}/shared-links/{id}",
            only(delete(revoke_link), "DELETE"),
        ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn domains_are_normalized() {
        for (raw, want) in [
            ("example.com", "example.com"),
            ("  Example.COM  ", "example.com"),
            ("https://www.Example.com/path?q=1#frag", "example.com"),
            ("http://example.com:8080/", "example.com"),
            ("example.com.", "example.com"),
            ("www.example.com", "example.com"),
            ("example.com:443", "example.com"),
            ("sub.example.co.uk/a/b", "sub.example.co.uk"),
            ("localhost", "localhost"),
            ("127.0.0.1:3000", "127.0.0.1"),
            ("www.example.com.:80", "example.com"),
        ] {
            assert_eq!(normalize_domain(raw).as_deref(), Ok(want), "{raw:?}");
        }
    }

    #[test]
    fn bad_domains_are_rejected() {
        for bad in [
            "",
            "   ",
            "*.example.com",
            "a*.com",
            "exa mple.com",
            "例え.jp",
            "https://",
            "example..com",
            "ex_ample.com",
            "user@example.com",
        ] {
            assert!(normalize_domain(bad).is_err(), "{bad:?}");
        }
        // 去掉一个 www. 之后仍以 www. 开头：采集侧会再去一次，存下去永远匹配不到
        assert_eq!(normalize_domain("www.www.example.com"), Err(WWW_LEFT));
    }

    #[test]
    fn stats_ranges_are_validated() {
        use jiff::civil::date;
        let today = date(2026, 10, 26);
        assert_eq!(
            validate_stats_range("2026-10-01", "2026-10-26", today),
            Ok((date(2026, 10, 1), today))
        );
        assert_eq!(
            validate_stats_range("2026-10-05", "2026-10-05", today),
            Ok((date(2026, 10, 5), date(2026, 10, 5)))
        );
        // 731 天（含两端）可以，732 天不行
        assert!(validate_stats_range("2024-10-26", "2026-10-26", today).is_ok());
        assert!(validate_stats_range("2024-10-25", "2026-10-26", today).is_err());
        for (from, to) in [
            ("2026-10-02", "2026-10-01"),
            ("2026-10-01", "2026-10-27"),
            ("2026-10-1", "2026-10-02"),
            ("2026/10/01", "2026-10-02"),
            ("2026-02-30", "2026-03-01"),
            ("", "2026-10-01"),
            ("2026-10-01T00:00", "2026-10-02"),
        ] {
            assert!(
                validate_stats_range(from, to, today).is_err(),
                "{from:?} {to:?}"
            );
        }
    }

    #[test]
    fn hostname_patterns_allow_wildcards_but_not_www() {
        assert_eq!(
            normalize_pattern(" *.Example.com ").as_deref(),
            Ok("*.example.com")
        );
        assert_eq!(
            normalize_pattern("blog.example.com").as_deref(),
            Ok("blog.example.com")
        );
        for bad in [
            "www.example.com",
            "*.www.example.com",
            "",
            "a.*.com",
            "*example.com",
            "example.com:80",
            "https://example.com",
            "例え.jp",
        ] {
            assert!(normalize_pattern(bad).is_err(), "{bad:?}");
        }
        // 只是以 www 开头的标签不算
        assert_eq!(
            normalize_pattern("www2.example.com").as_deref(),
            Ok("www2.example.com")
        );
    }

    #[test]
    fn site_settings_roles_follow_the_matrix() {
        let roles = |o: Option<&str>, g: Option<&str>| UserSiteRoles {
            org_role: o.map(str::to_owned),
            guest_role: g.map(str::to_owned),
        };
        let eff = |r: &UserSiteRoles| effective_role(r).unwrap();
        // 读：有效角色 editor 起（站点访客 editor 也行）
        let guest_editor = roles(None, Some("editor"));
        assert!(
            check_role(
                &guest_editor,
                eff(&guest_editor),
                Action::SiteSettingsRead,
                false
            )
            .is_ok()
        );
        let viewer = roles(Some("viewer"), None);
        assert!(check_role(&viewer, eff(&viewer), Action::SiteSettingsRead, false).is_err());
        // 写：只认组织角色，站点访客永远 403
        assert!(
            check_role(
                &guest_editor,
                eff(&guest_editor),
                Action::SharedLinkManage,
                true
            )
            .is_err()
        );
        let editor = roles(Some("editor"), None);
        assert!(check_role(&editor, eff(&editor), Action::SharedLinkManage, true).is_ok());
        assert!(check_role(&editor, eff(&editor), Action::SiteSettingsWrite, true).is_err());
        let admin = roles(Some("admin"), Some("viewer"));
        assert!(check_role(&admin, eff(&admin), Action::SiteDelete, true).is_ok());
    }
}
