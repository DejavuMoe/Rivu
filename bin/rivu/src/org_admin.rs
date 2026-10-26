//! 看板的组织管理端点：成员、邀请、站点访客、自己的 API key，以及公开的邀请页（查询与接受）。路由在 [`routes`]，
//! 由 [`crate::dash_api::router`] 合并；端点表见 `dash_api.rs` 顶部，规则见 `platform-and-ops.md` §4.1。
//!
//! # 权限
//!
//! - 组织不存在、或调用者不是它的（未删除的）成员：404（与不存在相同）；是成员但角色不够：403 `forbidden`；没有会话 401；
//! - 角色判定走 `rivu_auth::scope`：`MemberManage`、`SiteMembershipManage`（owner、admin）与 `ApiKeyManageOwn`（所有角色），
//!   「谁能给谁什么角色、谁能改谁」是纯函数 `can_assign`、`can_modify`，锁内再判一次（见 `rivu_storage_pg::members`）；
//! - 谁也不能通过这些端点改自己的角色或移除自己（403）；组织至少保留一名 owner（409）；
//! - 这些端点只认会话：API key 永远调不到（key 只能走公开 API 的 Bearer 鉴权）。
//!
//! # 邀请
//!
//! 不发邮件：创建邀请返回一次性链接 `<RIVU_BASE_URL>/invite/<token>`，由邀请人转交。令牌是 32 字节随机数的 base64url，
//! 库里只存 SHA-256，响应里只出现这一次。邀请页的两个端点不需要登录，按客户端 IP 限流（在查库之前），
//! 无效、过期、已接受、已撤销、组织或站点已删除一律同一个 404。
//!
//! 日志与其它写端点一致：写操作记 `info!`（用户 id、组织 id、动作），不记令牌、密码、邮箱与 key 明文。

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use axum::Router;
use axum::extract::{ConnectInfo, Path, Request, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, patch, post};
use jiff::{Span, Timestamp};
use rivu_auth::identity::normalize_email;
use rivu_auth::key;
use rivu_auth::password::validate_password;
use rivu_auth::scope::{Action, DEFAULT_SCOPE, OrgRole, allowed, can_assign, can_modify};
use rivu_auth::session;
use rivu_storage_pg::api_keys::{self, NewApiKey, OwnKeyRow};
use rivu_storage_pg::members::{
    self, AcceptOutcome, GuestRow, InvitationRow, InviteOutcome, MemberChange, MemberRow,
    NewAccount, NewInvitation,
};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::dash_api::{
    AUTH_BODY_MAX, DashApi, DashError, Res, SessionCtx, clean_name, endpoint, json_response, ns,
    read_json, with_cookie,
};
use crate::site_admin::only;

/// 邀请的有效期。
const INVITATION_TTL_SECS: i64 = 7 * 24 * 3600;
/// 创建 API key 时 `expires_in_days` 允许的取值。
const KEY_EXPIRY_DAYS: [u32; 3] = [30, 90, 365];
/// 一把 key 最多限定的站点数（防止滥用请求体）。
const KEY_MAX_SITES: usize = 100;

// ------------------------------------------------------------------------------------------
// 鉴权
// ------------------------------------------------------------------------------------------

/// 通过鉴权的调用者与组织。
struct OrgCaller {
    session: SessionCtx,
    org_id: i64,
    role: OrgRole,
}

/// 会话（401）→ 组织成员（404）→ 角色够不够（403；`action` 为 `None` 表示任一成员）。
async fn authorize_org(
    api: &DashApi,
    headers: &HeaderMap,
    org_seg: &str,
    action: Option<Action>,
) -> Res<OrgCaller> {
    let session = api.require_session(headers).await?;
    let org_id: i64 = org_seg.parse().map_err(|_| DashError::NotFound)?;
    let role = members::member_role(&api.pg, org_id, session.s.user_id)
        .await?
        .ok_or(DashError::NotFound)?;
    // 库里的 CHECK 保证角色可识别；认不出来按无权限处理（fail-closed）
    let role = OrgRole::parse(&role).ok_or(DashError::Forbidden)?;
    if action.is_some_and(|a| !allowed(role, a)) {
        return Err(DashError::Forbidden);
    }
    Ok(OrgCaller {
        session,
        org_id,
        role,
    })
}

fn done(api: &DashApi, c: &OrgCaller, status: StatusCode, body: Value) -> Response {
    api.finish(json_response(status, body), &c.session)
}

fn no_content(api: &DashApi, c: &OrgCaller) -> Response {
    api.finish(ns(StatusCode::NO_CONTENT.into_response()), &c.session)
}

fn parse_id(seg: &str) -> Res<i64> {
    seg.parse().map_err(|_| DashError::NotFound)
}

// ------------------------------------------------------------------------------------------
// JSON 形状
// ------------------------------------------------------------------------------------------

fn site_json(site: &Option<(i64, String)>) -> Value {
    site.as_ref()
        .map_or(Value::Null, |(id, name)| json!({"id": id, "name": name}))
}

fn member_json(m: &MemberRow, me: i64) -> Value {
    json!({
        "user_id": m.user_id, "name": m.name, "email": m.email, "role": m.role,
        "joined_at": m.joined_at, "last_active_at": m.last_active_at, "me": m.user_id == me,
    })
}

fn invitation_json(i: &InvitationRow) -> Value {
    json!({
        "id": i.id, "email": i.email, "role": i.role, "site": site_json(&i.site),
        "site_role": i.site_role, "expires_at": i.expires_at, "invited_by": i.invited_by,
    })
}

fn guest_json(g: &GuestRow) -> Value {
    json!({
        "id": g.id, "user_id": g.user_id, "name": g.name, "email": g.email,
        "site": {"id": g.site_id, "name": g.site_name}, "role": g.role,
    })
}

// ------------------------------------------------------------------------------------------
// 成员
// ------------------------------------------------------------------------------------------

async fn list_members(
    State(api): State<Arc<DashApi>>,
    Path(org_seg): Path<String>,
    req: Request,
) -> Res<Response> {
    let c = authorize_org(&api, req.headers(), &org_seg, None).await?;
    let me = c.session.s.user_id;
    let members = members::list_members(&api.pg, c.org_id).await?;
    let invitations = members::list_pending_invitations(&api.pg, c.org_id).await?;
    let guests = members::list_guests(&api.pg, c.org_id).await?;
    Ok(done(
        &api,
        &c,
        StatusCode::OK,
        json!({
            "members": members.iter().map(|m| member_json(m, me)).collect::<Vec<_>>(),
            "invitations": invitations.iter().map(invitation_json).collect::<Vec<_>>(),
            "guests": guests.iter().map(guest_json).collect::<Vec<_>>(),
            "can_manage": allowed(c.role, Action::MemberManage),
            "my_role": c.role.as_str(),
        }),
    ))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PatchMemberBody {
    role: String,
}

/// 把锁内的判定结果翻成响应错误。
fn change_error(c: MemberChange) -> DashError {
    match c {
        MemberChange::NotMember => DashError::NotFound,
        MemberChange::Denied => DashError::Forbidden,
        MemberChange::LastOwner => DashError::Conflict(
            "conflict",
            "the organization must keep at least one owner".into(),
        ),
        MemberChange::Done { .. } => DashError::Internal,
    }
}

async fn patch_member(
    State(api): State<Arc<DashApi>>,
    Path((org_seg, user_seg)): Path<(String, String)>,
    req: Request,
) -> Res<Response> {
    let headers = req.headers().clone();
    let c = authorize_org(&api, &headers, &org_seg, Some(Action::MemberManage)).await?;
    let target = parse_id(&user_seg)?;
    // 谁也不能改自己的角色（含 owner 自降：转让所有权要由另一名 owner 来改）
    if target == c.session.s.user_id {
        return Err(DashError::Forbidden);
    }
    let body: PatchMemberBody = read_json(req, AUTH_BODY_MAX).await?;
    let new_role = OrgRole::parse(&body.role).ok_or_else(|| {
        DashError::Invalid("the role must be one of owner, admin, editor, viewer".into())
    })?;
    let outcome = members::change_role(
        &api.pg,
        c.org_id,
        c.session.s.user_id,
        target,
        new_role.as_str(),
        |actor, current| match (OrgRole::parse(actor), OrgRole::parse(current)) {
            (Some(a), Some(t)) => can_modify(a, t) && can_assign(a, new_role),
            _ => false,
        },
    )
    .await?;
    if !matches!(outcome, MemberChange::Done { .. }) {
        return Err(change_error(outcome));
    }
    tracing::info!(
        user_id = c.session.s.user_id,
        org_id = c.org_id,
        target_user_id = target,
        "修改成员角色"
    );
    let m = members::get_member(&api.pg, c.org_id, target)
        .await?
        .ok_or(DashError::NotFound)?;
    Ok(done(
        &api,
        &c,
        StatusCode::OK,
        member_json(&m, c.session.s.user_id),
    ))
}

async fn remove_member(
    State(api): State<Arc<DashApi>>,
    Path((org_seg, user_seg)): Path<(String, String)>,
    req: Request,
) -> Res<Response> {
    let c = authorize_org(&api, req.headers(), &org_seg, Some(Action::MemberManage)).await?;
    let target = parse_id(&user_seg)?;
    if target == c.session.s.user_id {
        return Err(DashError::Forbidden);
    }
    let outcome = members::remove_member(
        &api.pg,
        c.org_id,
        c.session.s.user_id,
        target,
        |actor, current| match (OrgRole::parse(actor), OrgRole::parse(current)) {
            (Some(a), Some(t)) => can_modify(a, t),
            _ => false,
        },
    )
    .await?;
    let MemberChange::Done { keys_revoked } = outcome else {
        return Err(change_error(outcome));
    };
    tracing::info!(
        user_id = c.session.s.user_id,
        org_id = c.org_id,
        target_user_id = target,
        keys_revoked,
        "移除成员"
    );
    Ok(no_content(&api, &c))
}

// ------------------------------------------------------------------------------------------
// 邀请与站点访客
// ------------------------------------------------------------------------------------------

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InviteBody {
    email: String,
    #[serde(default)]
    role: Option<String>,
    #[serde(default)]
    site_id: Option<i64>,
    #[serde(default)]
    site_role: Option<String>,
}

async fn create_invitation(
    State(api): State<Arc<DashApi>>,
    Path(org_seg): Path<String>,
    req: Request,
) -> Res<Response> {
    let headers = req.headers().clone();
    let c = authorize_org(&api, &headers, &org_seg, Some(Action::MemberManage)).await?;
    let body: InviteBody = read_json(req, AUTH_BODY_MAX).await?;
    let email = normalize_email(&body.email).map_err(|m| DashError::Invalid(m.into()))?;

    // 两种邀请：组织成员（role）、站点访客（site_id 加 site_role）；混着给或缺一半都是 400
    let (role, site): (Option<OrgRole>, Option<(i64, &str)>) = match (
        &body.role,
        body.site_id,
        &body.site_role,
    ) {
        (Some(role), None, None) => {
            let role = OrgRole::parse(role).ok_or_else(|| {
                DashError::Invalid("the role must be one of admin, editor, viewer".into())
            })?;
            if role == OrgRole::Owner {
                return Err(DashError::Invalid(
                    "an owner cannot be invited; invite as admin and change the role afterwards"
                        .into(),
                ));
            }
            if !can_assign(c.role, role) {
                return Err(DashError::Forbidden);
            }
            (Some(role), None)
        }
        (None, Some(site_id), Some(site_role)) => {
            if !allowed(c.role, Action::SiteMembershipManage) {
                return Err(DashError::Forbidden);
            }
            let site_role = match site_role.as_str() {
                "editor" => "editor",
                "viewer" => "viewer",
                _ => {
                    return Err(DashError::Invalid(
                        "the site_role must be one of editor, viewer".into(),
                    ));
                }
            };
            (None, Some((site_id, site_role)))
        }
        _ => {
            return Err(DashError::Invalid(
                    "give either a role (organization member) or a site_id with a site_role (site guest)"
                        .into(),
                ));
        }
    };

    let token = session::generate();
    let outcome = members::create_invitation(
        &api.pg,
        &NewInvitation {
            org_id: c.org_id,
            email: &email,
            role: role.map(OrgRole::as_str),
            site,
            token_hash: token.hash(),
            invited_by: c.session.s.user_id,
            ttl_secs: INVITATION_TTL_SECS,
        },
    )
    .await?;
    let id = match outcome {
        InviteOutcome::Created(id) => id,
        InviteOutcome::AlreadyMember => {
            return Err(DashError::Conflict(
                "conflict",
                "this email address already belongs to a member of the organization".into(),
            ));
        }
        InviteOutcome::AlreadyGuest => {
            return Err(DashError::Conflict(
                "conflict",
                "this email address is already a guest of the site".into(),
            ));
        }
        InviteOutcome::Pending => {
            return Err(DashError::Conflict(
                "conflict",
                "a pending invitation for this email address already exists".into(),
            ));
        }
        InviteOutcome::SiteNotFound => return Err(DashError::NotFound),
    };
    let row = members::get_pending_invitation(&api.pg, c.org_id, id)
        .await?
        .ok_or(DashError::Internal)?;
    tracing::info!(
        user_id = c.session.s.user_id,
        org_id = c.org_id,
        invitation_id = id,
        guest = site.is_some(),
        "创建邀请"
    );
    let url = format!(
        "{}/invite/{}",
        api.base_url.trim_end_matches('/'),
        token.plaintext()
    );
    Ok(done(
        &api,
        &c,
        StatusCode::CREATED,
        json!({"invitation": invitation_json(&row), "url": url}),
    ))
}

async fn revoke_invitation(
    State(api): State<Arc<DashApi>>,
    Path((org_seg, id)): Path<(String, String)>,
    req: Request,
) -> Res<Response> {
    let c = authorize_org(&api, req.headers(), &org_seg, Some(Action::MemberManage)).await?;
    let id = parse_id(&id)?;
    if !members::revoke_invitation(&api.pg, c.org_id, id).await? {
        return Err(DashError::NotFound);
    }
    tracing::info!(
        user_id = c.session.s.user_id,
        org_id = c.org_id,
        invitation_id = id,
        "撤销邀请"
    );
    Ok(no_content(&api, &c))
}

async fn remove_guest(
    State(api): State<Arc<DashApi>>,
    Path((org_seg, id)): Path<(String, String)>,
    req: Request,
) -> Res<Response> {
    let c = authorize_org(
        &api,
        req.headers(),
        &org_seg,
        Some(Action::SiteMembershipManage),
    )
    .await?;
    let id = parse_id(&id)?;
    if !members::remove_guest(&api.pg, c.org_id, id).await? {
        return Err(DashError::NotFound);
    }
    tracing::info!(
        user_id = c.session.s.user_id,
        org_id = c.org_id,
        guest_id = id,
        "移除站点访客"
    );
    Ok(no_content(&api, &c))
}

// ------------------------------------------------------------------------------------------
// 邀请页（公开）
// ------------------------------------------------------------------------------------------

/// 令牌的摘要；形状不对的令牌不可能存在，直接 404（不查库）。
fn token_hash(token: &str) -> Res<[u8; 32]> {
    if !session::is_well_formed(token) {
        return Err(DashError::NotFound);
    }
    Ok(session::hash_token(token))
}

async fn invitation_info(
    State(api): State<Arc<DashApi>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Path(token): Path<String>,
    req: Request,
) -> Res<Response> {
    // 限流在一切判断之前：乱猜令牌同样消耗配额
    api.check_invite_rate(peer, req.headers())?;
    let hash = token_hash(&token)?;
    let info = members::find_pending_by_token(&api.pg, &hash)
        .await?
        .ok_or(DashError::NotFound)?;
    // cookie 可有可无：有有效会话就告诉页面当前登录的是谁
    let ctx = api.session(req.headers()).await?;
    let body = json!({
        "email": info.email,
        "organization": {"id": info.org_id, "name": info.org_name},
        "role": info.role,
        "site": site_json(&info.site),
        "site_role": info.site_role,
        "inviter": info.inviter,
        "expires_at": info.expires_at,
        "account_exists": info.account_exists,
        "signed_in_as": ctx.as_ref().map(|c| c.s.email.as_str()),
    });
    let resp = json_response(StatusCode::OK, body);
    Ok(match &ctx {
        Some(c) => api.finish(resp, c),
        None => resp,
    })
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct AcceptBody {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    password: Option<String>,
}

async fn accept_invitation(
    State(api): State<Arc<DashApi>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Path(token): Path<String>,
    req: Request,
) -> Res<Response> {
    let headers = req.headers().clone();
    api.check_invite_rate(peer, &headers)?;
    let hash = token_hash(&token)?;
    let body: AcceptBody = read_json(req, AUTH_BODY_MAX).await?;
    let ctx = api.session(&headers).await?;

    // 先（不加锁地）看一眼邀请：没有账号的受邀者要先校验并哈希密码（argon2 很慢，不能放进持锁的事务）；
    // 真正的判定在事务里对邀请行加锁后重做
    let info = members::find_pending_by_token(&api.pg, &hash)
        .await?
        .ok_or(DashError::NotFound)?;
    let account = if info.account_exists {
        None
    } else {
        let password = body
            .password
            .as_deref()
            .ok_or_else(|| DashError::Invalid("the password is required".into()))?;
        validate_password(password).map_err(|m| DashError::Invalid(m.into()))?;
        let name = clean_name(body.name.as_deref())?;
        let hash = api
            .passwords
            .hash(password.to_owned())
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        Some((name, hash))
    };
    let new_account = account.as_ref().map(|(name, hash)| NewAccount {
        name: name.as_deref(),
        password_hash: hash,
    });

    let outcome = members::accept_invitation(
        &api.pg,
        &hash,
        ctx.as_ref().map(|c| c.s.user_id),
        new_account.as_ref(),
    )
    .await?;
    let (user_id, org_id, site_id, created) = match outcome {
        AcceptOutcome::Accepted {
            user_id,
            org_id,
            site_id,
            created_account,
        } => (user_id, org_id, site_id, created_account),
        AcceptOutcome::NotFound => return Err(DashError::NotFound),
        AcceptOutcome::NeedSession => return Err(DashError::Unauthorized),
        AcceptOutcome::EmailMismatch => return Err(DashError::EmailMismatch),
        AcceptOutcome::NeedAccount => {
            return Err(DashError::Invalid("the password is required".into()));
        }
        AcceptOutcome::EmailTaken => {
            return Err(DashError::Conflict(
                "conflict",
                "an account with this email address already exists".into(),
            ));
        }
        AcceptOutcome::AlreadyMember => {
            return Err(DashError::Conflict(
                "conflict",
                "you are already a member of this organization".into(),
            ));
        }
    };
    tracing::info!(
        user_id,
        org_id,
        site_id,
        created_account = created,
        "接受邀请"
    );
    let resp = json_response(
        StatusCode::OK,
        json!({"organization_id": org_id, "site_id": site_id}),
    );
    if created {
        // 与登录相同的会话 cookie；不撤销请求里带的别的会话
        let cookie = api.start_session(user_id, None, &headers).await?;
        return Ok(with_cookie(resp, &cookie));
    }
    Ok(match &ctx {
        Some(c) => api.finish(resp, c),
        None => resp,
    })
}

// ------------------------------------------------------------------------------------------
// API key（只管自己的）
// ------------------------------------------------------------------------------------------

/// scope 文本里限定的站点 key（`*` 与不是 `stats:read:<site_key>` 的不算）。
fn scope_site_keys(scopes: &[String]) -> impl Iterator<Item = &str> {
    scopes
        .iter()
        .filter_map(|s| s.strip_prefix("stats:read:"))
        .filter(|k| *k != "*")
}

fn key_json(k: &OwnKeyRow, names: &HashMap<String, String>) -> Value {
    let sites: Vec<Value> = scope_site_keys(&k.scopes)
        .filter_map(|key| names.get(key).map(|n| json!({"site_key": key, "name": n})))
        .collect();
    json!({
        "id": k.id, "name": k.name, "key_prefix": k.key_prefix, "scopes": k.scopes, "sites": sites,
        "created_at": k.created_at, "last_used_at": k.last_used_at, "expires_at": k.expires_at,
    })
}

/// 解析一批 key 里涉及的站点名（组织内未删除的站点）。
async fn site_names(
    api: &DashApi,
    org_id: i64,
    keys: &[OwnKeyRow],
) -> Res<HashMap<String, String>> {
    let wanted: Vec<String> = keys
        .iter()
        .flat_map(|k| scope_site_keys(&k.scopes).map(str::to_owned))
        .collect();
    if wanted.is_empty() {
        return Ok(HashMap::new());
    }
    Ok(api_keys::resolve_sites(&api.pg, org_id, &wanted)
        .await?
        .into_iter()
        .collect())
}

async fn list_api_keys(
    State(api): State<Arc<DashApi>>,
    Path(org_seg): Path<String>,
    req: Request,
) -> Res<Response> {
    let c = authorize_org(&api, req.headers(), &org_seg, Some(Action::ApiKeyManageOwn)).await?;
    let keys = api_keys::list_own(&api.pg, c.org_id, c.session.s.user_id, None).await?;
    let names = site_names(&api, c.org_id, &keys).await?;
    let hourly = api_keys::org_hourly_limit(&api.pg, c.org_id)
        .await?
        .and_then(|v| u32::try_from(v).ok())
        .unwrap_or(api.api_rate.hourly_default);
    Ok(done(
        &api,
        &c,
        StatusCode::OK,
        json!({
            "keys": keys.iter().map(|k| key_json(k, &names)).collect::<Vec<_>>(),
            "limits": {
                "hourly": hourly,
                "burst": api.api_rate.burst.min(hourly),
                "concurrency": api.api_rate.max_concurrency,
            },
        }),
    ))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateKeyBody {
    name: String,
    #[serde(default)]
    sites: Option<Vec<String>>,
    #[serde(default)]
    expires_in_days: Option<u32>,
}

async fn create_api_key(
    State(api): State<Arc<DashApi>>,
    Path(org_seg): Path<String>,
    req: Request,
) -> Res<Response> {
    let headers = req.headers().clone();
    let c = authorize_org(&api, &headers, &org_seg, Some(Action::ApiKeyManageOwn)).await?;
    let body: CreateKeyBody = read_json(req, AUTH_BODY_MAX).await?;
    let name = clean_name(Some(&body.name))?
        .ok_or_else(|| DashError::Invalid("the name must not be empty".into()))?;
    if body
        .expires_in_days
        .is_some_and(|d| !KEY_EXPIRY_DAYS.contains(&d))
    {
        return Err(DashError::Invalid(
            "the expires_in_days must be one of 30, 90, 365, or null for no expiry".into(),
        ));
    }
    let scopes: Vec<String> = match body.sites {
        None => vec![DEFAULT_SCOPE.to_owned()],
        Some(sites) => {
            let mut unique: Vec<String> = Vec::new();
            for s in sites {
                if !unique.contains(&s) {
                    unique.push(s);
                }
            }
            if unique.is_empty() || unique.len() > KEY_MAX_SITES {
                return Err(DashError::Invalid(format!(
                    "the sites must be null (all sites) or a list of 1 to {KEY_MAX_SITES} site keys"
                )));
            }
            let found = api_keys::resolve_sites(&api.pg, c.org_id, &unique).await?;
            if found.len() != unique.len() {
                return Err(DashError::Invalid(
                    "the sites must all be sites of this organization".into(),
                ));
            }
            unique.iter().map(|k| format!("stats:read:{k}")).collect()
        }
    };
    // 过期时刻按真实时钟算（库里的过期判定用 `now()`，不能被测试里替换的时钟带偏）
    let expires_at_unix = match body.expires_in_days {
        None => None,
        Some(days) => Some(
            Timestamp::now()
                .checked_add(Span::new().hours(i64::from(days) * 24))
                .map_err(|_| DashError::Internal)?
                .as_second(),
        ),
    };
    let new_key = key::generate();
    let id = api_keys::create_key(
        &api.pg,
        &NewApiKey {
            organization_id: c.org_id,
            user_id: c.session.s.user_id,
            name: &name,
            key_prefix: new_key.prefix(),
            key_hash: new_key.hash(),
            scopes: &scopes,
            expires_at_unix,
        },
    )
    .await?;
    let keys = api_keys::list_own(&api.pg, c.org_id, c.session.s.user_id, Some(id)).await?;
    let row = keys.first().ok_or(DashError::Internal)?;
    let names = site_names(&api, c.org_id, &keys).await?;
    tracing::info!(
        user_id = c.session.s.user_id,
        org_id = c.org_id,
        key_id = id,
        "创建 API key"
    );
    Ok(done(
        &api,
        &c,
        StatusCode::CREATED,
        json!({"key": new_key.plaintext(), "item": key_json(row, &names)}),
    ))
}

async fn revoke_api_key(
    State(api): State<Arc<DashApi>>,
    Path((org_seg, id)): Path<(String, String)>,
    req: Request,
) -> Res<Response> {
    let c = authorize_org(&api, req.headers(), &org_seg, Some(Action::ApiKeyManageOwn)).await?;
    let id = parse_id(&id)?;
    if !api_keys::revoke_own(&api.pg, c.org_id, c.session.s.user_id, id).await? {
        return Err(DashError::NotFound);
    }
    tracing::info!(
        user_id = c.session.s.user_id,
        org_id = c.org_id,
        key_id = id,
        "吊销 API key"
    );
    Ok(no_content(&api, &c))
}

// ------------------------------------------------------------------------------------------
// 路由
// ------------------------------------------------------------------------------------------

pub(crate) fn routes(api: &Arc<DashApi>) -> Router {
    let a = api;
    Router::new()
        .merge(endpoint(
            a,
            "internal.members",
            "/api/internal/orgs/{org_id}/members",
            only(get(list_members), "GET, HEAD"),
        ))
        .merge(endpoint(
            a,
            "internal.member",
            "/api/internal/orgs/{org_id}/members/{user_id}",
            only(patch(patch_member).delete(remove_member), "PATCH, DELETE"),
        ))
        .merge(endpoint(
            a,
            "internal.invitations",
            "/api/internal/orgs/{org_id}/invitations",
            only(post(create_invitation), "POST"),
        ))
        .merge(endpoint(
            a,
            "internal.invitation_revoke",
            "/api/internal/orgs/{org_id}/invitations/{id}",
            only(delete(revoke_invitation), "DELETE"),
        ))
        .merge(endpoint(
            a,
            "internal.guest_remove",
            "/api/internal/orgs/{org_id}/guests/{id}",
            only(delete(remove_guest), "DELETE"),
        ))
        .merge(endpoint(
            a,
            "internal.invitation_info",
            "/api/internal/invitations/{token}",
            only(get(invitation_info), "GET, HEAD"),
        ))
        .merge(endpoint(
            a,
            "internal.invitation_accept",
            "/api/internal/invitations/{token}/accept",
            only(post(accept_invitation), "POST"),
        ))
        .merge(endpoint(
            a,
            "internal.api_keys",
            "/api/internal/orgs/{org_id}/api-keys",
            only(get(list_api_keys).post(create_api_key), "GET, HEAD, POST"),
        ))
        .merge(endpoint(
            a,
            "internal.api_key_delete",
            "/api/internal/orgs/{org_id}/api-keys/{id}",
            only(delete(revoke_api_key), "DELETE"),
        ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_sites_are_resolved_from_scopes() {
        let k = OwnKeyRow {
            id: 1,
            name: "k".into(),
            key_prefix: "rivu_abcdefg".into(),
            scopes: vec![
                "stats:read:abcdefghijkl".into(),
                "stats:read:mnopqrstuvwx".into(),
            ],
            created_at: "2026-10-26T08:00:00Z".into(),
            last_used_at: None,
            expires_at: None,
        };
        // 已删除的站点（不在名称表里）不出现
        let names = HashMap::from([("abcdefghijkl".to_owned(), "Docs".to_owned())]);
        let v = key_json(&k, &names);
        assert_eq!(
            v["sites"],
            json!([{"site_key": "abcdefghijkl", "name": "Docs"}])
        );
        // `*` 没有站点列表
        let all = OwnKeyRow {
            scopes: vec!["stats:read:*".into()],
            ..k
        };
        assert_eq!(key_json(&all, &names)["sites"], json!([]));
    }

    #[test]
    fn malformed_tokens_are_not_found_without_a_lookup() {
        assert!(matches!(token_hash("short"), Err(DashError::NotFound)));
        assert!(matches!(
            token_hash(&"=".repeat(43)),
            Err(DashError::NotFound)
        ));
        assert!(token_hash(session::generate().plaintext()).is_ok());
    }
}
