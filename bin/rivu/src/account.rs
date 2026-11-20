//! 账号安全：找回密码（邮件）、两步验证（TOTP）与登录的第二步。路由在 [`routes`]，由 [`crate::dash_api::router`] 合并。
//!
//! | 端点 | 鉴权 | 说明 |
//! |---|---|---|
//! | `GET /api/internal/auth-options` | 无 | `{"password_reset": bool}`：配置了 SMTP 才有「忘记密码」 |
//! | `POST /api/internal/password-reset` | 无 | `{"email"}` → 202，账号存在与否响应相同；发信在后台，令牌 1 小时有效、只有最新一封有效 |
//! | `GET /api/internal/password-reset/{token}` | 无 | 令牌有效 200，否则 404（页面据此显示「链接已失效」） |
//! | `POST /api/internal/password-reset/{token}` | 无 | `{"password"}` → 改密码、撤销全部会话；不自动登录（开了两步验证的账号仍要验证码） |
//! | `POST /api/internal/login/totp` | 登录返回的 challenge | `{"challenge","code"}`：6 位验证码或恢复码 → 建会话 |
//! | `GET /api/internal/me/totp` | 会话 | `{"available","enabled","recovery_remaining"}` |
//! | `POST /api/internal/me/totp/setup` | 会话 + 当前密码 | 生成新密钥（未启用）→ `{"secret","uri","qr_svg"}` |
//! | `POST /api/internal/me/totp/enable` | 会话 | `{"code"}` 确认后启用 → `{"recovery_codes"}`（只出现这一次） |
//! | `POST /api/internal/me/totp/recovery-codes` | 会话 + 当前密码 | 换一组恢复码 |
//! | `POST /api/internal/me/totp/disable` | 会话 + 当前密码 | 关闭 |
//! | `GET /api/internal/me/email` | 会话 | `{"email","confirm_by_email","pending"}` |
//! | `POST /api/internal/me/email` | 会话 + 当前密码 | `{"new_email","current_password","lang"?}`：有 SMTP 时 202 并给新邮箱发确认链接（24 小时）；没有时 200 立即生效 |
//! | `DELETE /api/internal/me/email` | 会话 | 取消待确认的更换 → 204 |
//! | `GET /api/email-change/{token}` | 链接令牌 | 有效 `{"email": 新邮箱}`，否则统一 404；不改任何状态 |
//! | `POST /api/email-change/{token}` | 链接令牌 | 确认更换 → `{"email","signed_in"}`；带着本人会话时保留它，其余会话全部撤销 |
//!
//! 找回密码与 challenge 的限流：请求按 IP 与邮箱各一桶；验证码每个 challenge 最多试 5 次，另外计入登录的 IP 桶。
//! 更换邮箱：当前密码计入改密码的 `(用户, IP)` 桶，发确认邮件再计入找回密码的邮箱桶（按新邮箱），链接按 IP 限流。

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use axum::Router;
use axum::extract::{ConnectInfo, Path, Request, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use rivu_auth::identity::normalize_email;
use rivu_auth::password::{MAX_PASSWORD_CHARS, validate_password};
use rivu_auth::{session, totp};
use rivu_mail::Lang;
use rivu_storage_pg::email_changes::{self, Outcome};
use rivu_storage_pg::{auth_tokens, totp as totp_store, users};
use serde::Deserialize;
use serde_json::json;

use crate::dash_api::{
    AUTH_BODY_MAX, DashApi, DashError, Res, endpoint, ip_bucket, json_response, rate_limited,
    read_json, user_json, with_cookie,
};
use crate::site_admin::only;

/// 登录第二步的有效期。
pub const CHALLENGE_TTL: Duration = Duration::from_secs(300);
/// 每个 challenge 最多试几次验证码。
const CHALLENGE_ATTEMPTS: u32 = 5;
/// 找回密码令牌的有效期。
const RESET_TTL_SECS: i64 = 3600;

/// 密码正确、还差验证码的一次登录。
#[derive(Clone)]
pub struct Challenge {
    user_id: i64,
    email: String,
    device_label: Option<String>,
    attempts: Arc<AtomicU32>,
}

fn lang(headers: &HeaderMap) -> Lang {
    Lang::from_accept_language(
        headers
            .get(header::ACCEPT_LANGUAGE)
            .and_then(|v| v.to_str().ok()),
    )
}

fn vault(api: &DashApi) -> Res<&totp::Vault> {
    api.vault.as_deref().ok_or(DashError::BadRequest(
        "totp_unavailable",
        "two-factor authentication needs RIVU_SECRET_KEY on the server",
    ))
}

fn now_secs(api: &DashApi) -> u64 {
    u64::try_from((api.clock)().as_second()).unwrap_or(0)
}

/// 用户开了两步验证时，记下这次登录并返回 challenge（明文，只给这一次响应）。
pub(crate) async fn challenge_if_enabled(
    api: &DashApi,
    user_id: i64,
    email: &str,
    device_label: Option<&str>,
) -> Res<Option<String>> {
    let Some(row) = totp_store::get(&api.pg, user_id).await? else {
        return Ok(None);
    };
    if !row.enabled {
        return Ok(None);
    }
    let token = session::generate();
    api.login_challenges.insert(
        token.hash(),
        Challenge {
            user_id,
            email: email.to_owned(),
            device_label: device_label.map(str::to_owned),
            attempts: Arc::new(AtomicU32::new(0)),
        },
    );
    Ok(Some(token.plaintext().to_owned()))
}

#[derive(Deserialize)]
struct TotpLoginBody {
    challenge: String,
    code: String,
}

const EXPIRED: DashError = DashError::BadRequest(
    "challenge_expired",
    "the sign-in attempt expired; enter your email and password again",
);

async fn login_totp(
    State(api): State<Arc<DashApi>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    req: Request,
) -> Res<Response> {
    let headers = req.headers().clone();
    let ip = ip_bucket(api.client_ip(peer, &headers));
    api.limiters
        .login_ip
        .check(&ip)
        .map_err(|w| rate_limited("login", w))?;
    let body: TotpLoginBody = read_json(req, AUTH_BODY_MAX).await?;
    if !session::is_well_formed(&body.challenge) {
        return Err(EXPIRED);
    }
    let key = session::hash_token(&body.challenge);
    let Some(ch) = api.login_challenges.get(&key) else {
        return Err(EXPIRED);
    };
    if ch.attempts.fetch_add(1, Ordering::SeqCst) >= CHALLENGE_ATTEMPTS {
        api.login_challenges.invalidate(&key);
        return Err(EXPIRED);
    }
    // 这 5 分钟里账号可能被禁用或删除
    let user = users::find_by_email(&api.pg, &ch.email)
        .await?
        .filter(|u| u.id == ch.user_id && !u.disabled)
        .ok_or(EXPIRED)?;
    let row = totp_store::get(&api.pg, user.id)
        .await?
        .filter(|r| r.enabled)
        .ok_or(EXPIRED)?;

    let mut recovery_remaining = None;
    let ok = if totp::looks_like_recovery_code(&body.code) {
        recovery_remaining =
            totp_store::use_recovery(&api.pg, user.id, &totp::recovery_hash(&body.code)).await?;
        recovery_remaining.is_some()
    } else {
        let secret = vault(&api)?.open(&row.secret_enc).ok_or_else(|| {
            tracing::error!(
                user_id = user.id,
                "两步验证密钥解不开：RIVU_SECRET_KEY 换了却没配旧密钥？"
            );
            DashError::Internal
        })?;
        match totp::verify(
            &secret,
            &body.code,
            now_secs(&api),
            row.last_step.max(0) as u64,
        ) {
            Some(step) => totp_store::use_step(&api.pg, user.id, step as i64).await?,
            None => false,
        }
    };
    if !ok {
        metrics::counter!("rivu_login_total", "result" => "totp_failure").increment(1);
        tracing::debug!(user_id = user.id, "两步验证失败");
        return Err(DashError::BadRequest(
            "invalid_code",
            "the code is incorrect or was already used",
        ));
    }
    api.login_challenges.invalidate(&key);
    api.revoke_presented(&headers).await;
    let cookie = api
        .start_session(user.id, ch.device_label.as_deref(), &headers)
        .await?;
    metrics::counter!("rivu_login_total", "result" => "success").increment(1);
    tracing::info!(
        user_id = user.id,
        recovery = recovery_remaining.is_some(),
        "登录成功（两步验证）"
    );
    let mut body = json!({"user": user_json(user.id, &user.email, user.name.as_deref(), user.is_instance_admin)});
    if let Some(n) = recovery_remaining {
        body["recovery_remaining"] = json!(n);
    }
    Ok(with_cookie(json_response(StatusCode::OK, body), &cookie))
}

// ------------------------------------------------------------------------------------------
// 找回密码
// ------------------------------------------------------------------------------------------

async fn auth_options(State(api): State<Arc<DashApi>>) -> Res<Response> {
    Ok(json_response(
        StatusCode::OK,
        json!({"password_reset": api.mailer.is_some()}),
    ))
}

#[derive(Deserialize)]
struct ResetRequestBody {
    email: String,
}

async fn request_reset(
    State(api): State<Arc<DashApi>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    req: Request,
) -> Res<Response> {
    let Some(mailer) = api.mailer.clone() else {
        return Err(DashError::NotFound);
    };
    let headers = req.headers().clone();
    let ip = ip_bucket(api.client_ip(peer, &headers));
    let body: ResetRequestBody = read_json(req, AUTH_BODY_MAX).await?;
    let email = normalize_email(&body.email).map_err(|m| DashError::Invalid(m.into()))?;
    api.limiters
        .reset_ip
        .check(&ip)
        .and_then(|()| api.limiters.reset_email.check(&email))
        .map_err(|w| rate_limited("password_reset", w))?;
    // 查账号、签令牌、发信都在后台：响应与账号是否存在无关（内容与耗时都一样）
    let (pg, base, lang) = (api.pg.clone(), api.base_url.clone(), lang(&headers));
    tokio::spawn(async move {
        let user = match users::find_by_email(&pg, &email).await {
            Ok(Some(u)) if !u.disabled => u,
            Ok(_) => return,
            Err(e) => {
                tracing::warn!(error = %format!("{e:#}"), "找回密码：查询用户失败");
                return;
            }
        };
        let token = session::generate();
        if let Err(e) =
            auth_tokens::create_password_reset(&pg, user.id, &token.hash(), RESET_TTL_SECS).await
        {
            tracing::warn!(user_id = user.id, error = %format!("{e:#}"), "找回密码：写入令牌失败");
            return;
        }
        let link = format!(
            "{}/reset-password/{}",
            base.trim_end_matches('/'),
            token.plaintext()
        );
        let mail =
            rivu_mail::password_reset(lang, &user.email, &link, (RESET_TTL_SECS / 60) as u32);
        match mailer.send(mail).await {
            Ok(()) => tracing::info!(user_id = user.id, "已发送找回密码邮件"),
            Err(e) => {
                tracing::warn!(user_id = user.id, error = %format!("{e:#}"), "发送找回密码邮件失败")
            }
        }
    });
    Ok(json_response(StatusCode::ACCEPTED, json!({})))
}

fn reset_token(api: &DashApi, peer: SocketAddr, headers: &HeaderMap, token: &str) -> Res<[u8; 32]> {
    let ip = ip_bucket(api.client_ip(peer, headers));
    api.limiters
        .invite_view
        .check(&ip)
        .map_err(|w| rate_limited("password_reset", w))?;
    if !session::is_well_formed(token) {
        return Err(DashError::NotFound);
    }
    Ok(session::hash_token(token))
}

async fn check_reset(
    State(api): State<Arc<DashApi>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Path(token): Path<String>,
    req: Request,
) -> Res<Response> {
    let hash = reset_token(&api, peer, req.headers(), &token)?;
    auth_tokens::password_reset_user(&api.pg, &hash)
        .await?
        .ok_or(DashError::NotFound)?;
    Ok(json_response(StatusCode::OK, json!({})))
}

#[derive(Deserialize)]
struct ResetBody {
    password: String,
}

async fn confirm_reset(
    State(api): State<Arc<DashApi>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Path(token): Path<String>,
    req: Request,
) -> Res<Response> {
    let hash = reset_token(&api, peer, &req.headers().clone(), &token)?;
    let body: ResetBody = read_json(req, AUTH_BODY_MAX).await?;
    validate_password(&body.password).map_err(|_| {
        DashError::BadRequest(
            "invalid_new_password",
            "the new password must be 12 to 128 characters",
        )
    })?;
    let new_hash = api
        .passwords
        .hash(body.password)
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let user_id = auth_tokens::reset_password(&api.pg, &hash, &new_hash)
        .await?
        .ok_or(DashError::NotFound)?;
    tracing::info!(user_id, "通过邮件重设密码，全部会话已撤销");
    Ok(json_response(StatusCode::OK, json!({})))
}

// ------------------------------------------------------------------------------------------
// 账户的两步验证
// ------------------------------------------------------------------------------------------

/// 敏感操作前再输一次当前密码（限流同改密码）。
async fn confirm_password(
    api: &DashApi,
    peer: SocketAddr,
    headers: &HeaderMap,
    user_id: i64,
    password: String,
) -> Res<()> {
    let ip = ip_bucket(api.client_ip(peer, headers));
    api.limiters
        .password_change
        .check(&(user_id, ip))
        .map_err(|w| rate_limited("password_change", w))?;
    let stored = users::password_hash(&api.pg, user_id)
        .await?
        .ok_or(DashError::Unauthorized)?;
    let ok = password.chars().count() <= MAX_PASSWORD_CHARS
        && api.passwords.verify(password, Some(&stored)).await;
    if ok {
        Ok(())
    } else {
        Err(DashError::BadRequest(
            "wrong_password",
            "the current password is incorrect",
        ))
    }
}

async fn totp_status(State(api): State<Arc<DashApi>>, req: Request) -> Res<Response> {
    let ctx = api.require_session(req.headers()).await?;
    let row = totp_store::get(&api.pg, ctx.s.user_id).await?;
    let enabled = row.as_ref().is_some_and(|r| r.enabled);
    let body = json!({
        "available": api.vault.is_some(),
        "enabled": enabled,
        "recovery_remaining": row.filter(|r| r.enabled).map(|r| r.recovery_remaining),
    });
    Ok(api.finish(json_response(StatusCode::OK, body), &ctx))
}

#[derive(Deserialize)]
struct PasswordBody {
    password: String,
}

fn qr_svg(text: &str) -> Res<String> {
    let code =
        qrcode::QrCode::new(text.as_bytes()).map_err(|e| anyhow::anyhow!("生成二维码失败：{e}"))?;
    // 深色码、白底：暗色主题下也要能扫
    Ok(code
        .render::<qrcode::render::svg::Color>()
        .min_dimensions(184, 184)
        .quiet_zone(true)
        .dark_color(qrcode::render::svg::Color("#000000"))
        .light_color(qrcode::render::svg::Color("#ffffff"))
        .build())
}

async fn totp_setup(
    State(api): State<Arc<DashApi>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    req: Request,
) -> Res<Response> {
    let headers = req.headers().clone();
    let ctx = api.require_session(&headers).await?;
    let vault = vault(&api)?;
    let body: PasswordBody = read_json(req, AUTH_BODY_MAX).await?;
    confirm_password(&api, peer, &headers, ctx.s.user_id, body.password).await?;
    let secret = totp::generate_secret();
    if !totp_store::begin_setup(&api.pg, ctx.s.user_id, &vault.seal(&secret)).await? {
        return Err(DashError::Conflict(
            "totp_enabled",
            "two-factor authentication is already on; turn it off first".into(),
        ));
    }
    let uri = totp::otpauth_uri("Rivu", &ctx.s.email, &secret);
    let body = json!({"secret": totp::base32(&secret), "qr_svg": qr_svg(&uri)?, "uri": uri});
    Ok(api.finish(json_response(StatusCode::OK, body), &ctx))
}

#[derive(Deserialize)]
struct CodeBody {
    code: String,
}

async fn totp_enable(State(api): State<Arc<DashApi>>, req: Request) -> Res<Response> {
    let headers = req.headers().clone();
    let ctx = api.require_session(&headers).await?;
    let vault = vault(&api)?;
    let body: CodeBody = read_json(req, AUTH_BODY_MAX).await?;
    let user_id = ctx.s.user_id;
    let row = totp_store::get(&api.pg, user_id)
        .await?
        .filter(|r| !r.enabled)
        .ok_or(DashError::Conflict(
            "totp_not_pending",
            "start the setup first".into(),
        ))?;
    let secret = vault.open(&row.secret_enc).ok_or(DashError::Internal)?;
    let step = totp::verify(
        &secret,
        &body.code,
        now_secs(&api),
        row.last_step.max(0) as u64,
    )
    .ok_or(DashError::BadRequest(
        "invalid_code",
        "the code is incorrect; check the time on your device",
    ))?;
    let (codes, hashes) = totp::recovery_codes();
    if !totp_store::enable(&api.pg, user_id, step as i64, &hashes).await? {
        return Err(DashError::Conflict(
            "totp_not_pending",
            "start the setup first".into(),
        ));
    }
    tracing::info!(user_id, "开启两步验证");
    Ok(api.finish(
        json_response(StatusCode::OK, json!({"recovery_codes": codes})),
        &ctx,
    ))
}

async fn totp_recovery_codes(
    State(api): State<Arc<DashApi>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    req: Request,
) -> Res<Response> {
    let headers = req.headers().clone();
    let ctx = api.require_session(&headers).await?;
    let body: PasswordBody = read_json(req, AUTH_BODY_MAX).await?;
    confirm_password(&api, peer, &headers, ctx.s.user_id, body.password).await?;
    let (codes, hashes) = totp::recovery_codes();
    if !totp_store::replace_recovery(&api.pg, ctx.s.user_id, &hashes).await? {
        return Err(DashError::Conflict(
            "totp_disabled",
            "two-factor authentication is off".into(),
        ));
    }
    tracing::info!(user_id = ctx.s.user_id, "更换恢复码");
    Ok(api.finish(
        json_response(StatusCode::OK, json!({"recovery_codes": codes})),
        &ctx,
    ))
}

async fn totp_disable(
    State(api): State<Arc<DashApi>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    req: Request,
) -> Res<Response> {
    let headers = req.headers().clone();
    let ctx = api.require_session(&headers).await?;
    let body: PasswordBody = read_json(req, AUTH_BODY_MAX).await?;
    confirm_password(&api, peer, &headers, ctx.s.user_id, body.password).await?;
    totp_store::disable(&api.pg, ctx.s.user_id).await?;
    tracing::info!(user_id = ctx.s.user_id, "关闭两步验证");
    Ok(api.finish(json_response(StatusCode::OK, json!({})), &ctx))
}

// ------------------------------------------------------------------------------------------
// 更换登录邮箱
// ------------------------------------------------------------------------------------------

/// 确认链接的有效期。
const EMAIL_CHANGE_TTL_SECS: i64 = 24 * 3600;
/// 确认邮件里链接指向的看板页面；页面调用 `/api/email-change/{token}`。
const CONFIRM_EMAIL_PAGE: &str = "/email-change/confirm";

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
enum BodyLang {
    Zh,
    En,
}

#[derive(Deserialize)]
struct EmailChangeBody {
    new_email: String,
    current_password: String,
    /// 邮件的语言；不给就按 `Accept-Language`。
    #[serde(default)]
    lang: Option<BodyLang>,
}

fn email_taken() -> DashError {
    DashError::Conflict(
        "email_taken",
        "an account with this email address already exists".into(),
    )
}

fn lang_str(lang: Lang) -> &'static str {
    match lang {
        Lang::Zh => "zh",
        Lang::En => "en",
    }
}

async fn email_status(State(api): State<Arc<DashApi>>, req: Request) -> Res<Response> {
    let ctx = api.require_session(req.headers()).await?;
    let pending = email_changes::pending(&api.pg, ctx.s.user_id).await?;
    let body = json!({
        "email": ctx.s.email,
        "confirm_by_email": api.mailer.is_some(),
        "pending": pending.map(|p| json!({"new_email": p.new_email, "expires_at": p.expires_at})),
    });
    Ok(api.finish(json_response(StatusCode::OK, body), &ctx))
}

/// 更换登录邮箱（`platform-and-ops.md` §5）：校验新邮箱 → 当前密码（限流同改密码）→ 新邮箱没被别的账号用。
/// 有 SMTP 时记下待确认的更换并给新邮箱发链接（202）；没有 SMTP 时立即生效（200），保留当前会话、撤销其余会话。
async fn request_email_change(
    State(api): State<Arc<DashApi>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    req: Request,
) -> Res<Response> {
    let headers = req.headers().clone();
    let ctx = api.require_session(&headers).await?;
    let user_id = ctx.s.user_id;
    let body: EmailChangeBody = read_json(req, AUTH_BODY_MAX).await?;
    let new_email =
        normalize_email(&body.new_email).map_err(|m| DashError::BadRequest("invalid_email", m))?;
    if new_email == ctx.s.email.to_lowercase() {
        return Err(DashError::BadRequest(
            "same_email",
            "the new email address is the same as the current one",
        ));
    }
    confirm_password(&api, peer, &headers, user_id, body.current_password).await?;
    if email_changes::email_taken(&api.pg, user_id, &new_email).await? {
        return Err(email_taken());
    }
    let lang = match body.lang {
        Some(BodyLang::Zh) => Lang::Zh,
        Some(BodyLang::En) => Lang::En,
        None => lang(&headers),
    };

    let Some(mailer) = api.mailer.clone() else {
        // 没有 SMTP：没法验证新邮箱，凭当前密码直接改
        return match email_changes::change_now(&api.pg, user_id, &new_email, Some(ctx.s.id)).await?
        {
            Outcome::Changed(c) => {
                tracing::info!(
                    user_id,
                    revoked = c.revoked_sessions,
                    "更换登录邮箱（未配置 SMTP，立即生效），其他会话已撤销"
                );
                let user = user_json(
                    user_id,
                    &c.new_email,
                    ctx.s.name.as_deref(),
                    ctx.s.is_instance_admin,
                );
                Ok(api.finish(
                    json_response(StatusCode::OK, json!({"status": "changed", "user": user})),
                    &ctx,
                ))
            }
            Outcome::Taken => Err(email_taken()),
            Outcome::NotFound => Err(DashError::Unauthorized),
        };
    };

    // 发信有成本，也不能拿来轰炸别人的邮箱：按新邮箱计入找回密码的邮箱桶
    api.limiters
        .reset_email
        .check(&new_email)
        .map_err(|w| rate_limited("email_change", w))?;
    let token = session::generate();
    let hash = token.hash();
    let expires_at = email_changes::create(
        &api.pg,
        user_id,
        &ctx.s.email,
        &new_email,
        lang_str(lang),
        &hash,
        EMAIL_CHANGE_TTL_SECS,
    )
    .await?
    .ok_or(DashError::Unauthorized)?;
    let link = format!(
        "{}{CONFIRM_EMAIL_PAGE}/{}",
        api.base_url.trim_end_matches('/'),
        token.plaintext()
    );
    let mail = rivu_mail::email_change::confirmation(
        lang,
        &new_email,
        &link,
        (EMAIL_CHANGE_TTL_SECS / 3600) as u32,
    );
    if let Err(e) = mailer.send(mail).await {
        tracing::warn!(user_id, error = %format!("{e:#}"), "发送更换邮箱的确认邮件失败");
        // 没人收到这条链接：撤回它（之前的那条已被替换，不恢复）
        email_changes::cancel(&api.pg, user_id, Some(&hash)).await?;
        return Ok(api.finish(
            json_response(
                StatusCode::BAD_GATEWAY,
                json!({"error": "mail_failed", "message": "the confirmation email could not be delivered"}),
            ),
            &ctx,
        ));
    }
    tracing::info!(user_id, "已发送更换登录邮箱的确认邮件");
    let body = json!({
        "status": "pending",
        "pending": {"new_email": new_email, "expires_at": expires_at},
    });
    Ok(api.finish(json_response(StatusCode::ACCEPTED, body), &ctx))
}

async fn cancel_email_change(State(api): State<Arc<DashApi>>, req: Request) -> Res<Response> {
    let ctx = api.require_session(req.headers()).await?;
    email_changes::cancel(&api.pg, ctx.s.user_id, None).await?;
    Ok(api.finish(
        crate::dash_api::ns(StatusCode::NO_CONTENT.into_response()),
        &ctx,
    ))
}

/// 链接端点不把链接经 `Referer` 带给别的站点。
fn no_referrer(mut response: Response) -> Response {
    response
        .headers_mut()
        .insert("referrer-policy", HeaderValue::from_static("no-referrer"));
    response
}

/// 打开确认页时检查链接：不改任何状态，邮件客户端预取链接不会替用户确认。各种失败都是同一个 404。
async fn check_email_change(
    State(api): State<Arc<DashApi>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Path(token): Path<String>,
    headers: HeaderMap,
) -> Response {
    let result = async {
        let hash = reset_token(&api, peer, &headers, &token)?;
        let email = email_changes::lookup(&api.pg, &hash)
            .await?
            .ok_or(DashError::NotFound)?;
        Ok::<_, DashError>(json_response(StatusCode::OK, json!({"email": email})))
    }
    .await;
    no_referrer(result.unwrap_or_else(IntoResponse::into_response))
}

/// 凭链接确认更换。不需要登录（链接本身证明能收新邮箱的信）；请求带着本人的有效会话时保留它，其余会话全部撤销。
async fn confirm_email_change(
    State(api): State<Arc<DashApi>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Path(token): Path<String>,
    headers: HeaderMap,
) -> Response {
    let result = async {
        let hash = reset_token(&api, peer, &headers, &token)?;
        let presented = api.session(&headers).await?;
        let outcome = email_changes::confirm(
            &api.pg,
            &hash,
            presented.as_ref().map(|c| (c.s.user_id, c.s.id)),
        )
        .await?;
        let c = match outcome {
            Outcome::Changed(c) => c,
            Outcome::NotFound => return Err(DashError::NotFound),
            Outcome::Taken => return Err(email_taken()),
        };
        let signed_in = presented
            .as_ref()
            .is_some_and(|p| p.s.user_id == c.user_id);
        tracing::info!(
            user_id = c.user_id,
            revoked = c.revoked_sessions,
            kept_current = signed_in,
            "凭链接更换登录邮箱，其他会话已撤销"
        );
        if let Some(mailer) = api.mailer.clone() {
            let lang = match c.lang.as_deref() {
                Some("zh") => Lang::Zh,
                _ => Lang::En,
            };
            let mail = rivu_mail::email_change::changed_notice(
                lang,
                &c.old_email,
                &rivu_mail::email_change::mask_email(&c.new_email),
            );
            let user_id = c.user_id;
            tokio::spawn(async move {
                if let Err(e) = mailer.send(mail).await {
                    tracing::warn!(user_id, error = %format!("{e:#}"), "发送登录邮箱已更改的通知失败");
                }
            });
        }
        let response = json_response(
            StatusCode::OK,
            json!({"email": c.new_email, "signed_in": signed_in}),
        );
        Ok(match presented.filter(|_| signed_in) {
            Some(ctx) => api.finish(response, &ctx),
            None => response,
        })
    }
    .await;
    no_referrer(result.unwrap_or_else(IntoResponse::into_response))
}

/// 发邀请邮件（配置了 SMTP 时）；返回是否已交给发信。`target` 是组织名或站点名。
pub(crate) fn send_invitation(
    api: &DashApi,
    headers: &HeaderMap,
    to: &str,
    inviter: &str,
    target: &str,
    link: &str,
    days: u32,
) -> bool {
    let Some(mailer) = api.mailer.clone() else {
        return false;
    };
    let mail = rivu_mail::invitation(lang(headers), to, inviter, target, link, days);
    tokio::spawn(async move {
        if let Err(e) = mailer.send(mail).await {
            tracing::warn!(error = %format!("{e:#}"), "发送邀请邮件失败");
        }
    });
    true
}

pub(crate) fn routes(api: &Arc<DashApi>) -> Router {
    let a = api;
    Router::new()
        .merge(endpoint(
            a,
            "internal.auth_options",
            "/api/internal/auth-options",
            only(get(auth_options), "GET, HEAD"),
        ))
        .merge(endpoint(
            a,
            "internal.password_reset",
            "/api/internal/password-reset",
            only(post(request_reset), "POST"),
        ))
        .merge(endpoint(
            a,
            "internal.password_reset_token",
            "/api/internal/password-reset/{token}",
            only(get(check_reset).post(confirm_reset), "GET, HEAD, POST"),
        ))
        .merge(endpoint(
            a,
            "internal.login_totp",
            "/api/internal/login/totp",
            only(post(login_totp), "POST"),
        ))
        .merge(endpoint(
            a,
            "internal.totp",
            "/api/internal/me/totp",
            only(get(totp_status), "GET, HEAD"),
        ))
        .merge(endpoint(
            a,
            "internal.totp_setup",
            "/api/internal/me/totp/setup",
            only(post(totp_setup), "POST"),
        ))
        .merge(endpoint(
            a,
            "internal.totp_enable",
            "/api/internal/me/totp/enable",
            only(post(totp_enable), "POST"),
        ))
        .merge(endpoint(
            a,
            "internal.totp_recovery",
            "/api/internal/me/totp/recovery-codes",
            only(post(totp_recovery_codes), "POST"),
        ))
        .merge(endpoint(
            a,
            "internal.me_email",
            "/api/internal/me/email",
            only(
                get(email_status)
                    .post(request_email_change)
                    .delete(cancel_email_change),
                "GET, HEAD, POST, DELETE",
            ),
        ))
        .merge(endpoint(
            a,
            "internal.email_change_token",
            "/api/email-change/{token}",
            only(
                get(check_email_change).post(confirm_email_change),
                "GET, HEAD, POST",
            ),
        ))
        .merge(endpoint(
            a,
            "internal.totp_disable",
            "/api/internal/me/totp/disable",
            only(post(totp_disable), "POST"),
        ))
}
