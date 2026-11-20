//! Account-bound weekly and monthly reports. A committed claim consumes the SMTP attempt.
//!
//! An account confirms its mailbox once through an emailed link; every site it may read then has
//! one on/off switch per [`Kind`], and a subscription row exists only while that switch is on.
//! The tables keep their weekly names; the `kind` column tells the reports apart.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::extract::{Path, Request, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{MethodRouter, get, post, put};
use jiff::civil::{Date, Weekday};
use jiff::tz::TimeZone;
use jiff::{Timestamp, ToSpan};
use rivu_auth::session;
use rivu_mail::weekly_report::{Cadence, Report, confirmation, report};
use rivu_query::result::MetricValue;
use rivu_query::{ExecOptions, QueryResult, QuerySpec};
use rivu_storage_pg::access::{self, SiteRef, UserSiteRoles};
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::{PgConnection, PgExecutor, PgPool, Postgres, Transaction};
use tokio::sync::watch;

use crate::dash_api::{
    DashApi, DashError, endpoint, json_response, may_read_stats, method_not_allowed, read_json,
};
use crate::query_api::{execute_cached, load_site_context, no_store};

type Res<T> = Result<T, DashError>;
/// Dashboard page that the confirmation email links to; it calls `/api/weekly-report/confirm/{token}`.
const CONFIRM_PAGE: &str = "/weekly-report/confirm";

/// Which report a switch, a claim or an unsubscribe link belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Weekly,
    Monthly,
}

impl Kind {
    const ALL: [Kind; 2] = [Kind::Weekly, Kind::Monthly];

    /// The `kind` column value.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Weekly => "weekly",
            Self::Monthly => "monthly",
        }
    }

    fn cadence(self) -> Cadence {
        match self {
            Self::Weekly => Cadence::Weekly,
            Self::Monthly => Cadence::Monthly,
        }
    }

    /// Metric route label of the totals query.
    fn route(self) -> &'static str {
        match self {
            Self::Weekly => "weekly_report",
            Self::Monthly => "monthly_report",
        }
    }

    fn unsubscribe_path(self) -> &'static str {
        match self {
            Self::Weekly => "/api/weekly-report/unsubscribe",
            Self::Monthly => "/api/monthly-report/unsubscribe",
        }
    }
}

/// `PUT`/`DELETE /api/internal/sites/{site}/{weekly,monthly}-report`.
fn switch(kind: Kind) -> MethodRouter<Arc<DashApi>> {
    put(
        move |api: State<Arc<DashApi>>, site: Path<String>, req: Request| {
            subscribe(kind, api, site, req)
        },
    )
    .delete(
        move |api: State<Arc<DashApi>>, site: Path<String>, headers: HeaderMap| {
            unsubscribe(kind, api, site, headers)
        },
    )
    .fallback(|| method_not_allowed("PUT, DELETE"))
}

/// The unsubscribe link mailed with a report; it authorizes solely with the token, never cookies.
fn unsubscribe_link(api: &Arc<DashApi>, kind: Kind, route: &'static str) -> Router {
    crate::query_api::count_requests(
        Router::new()
            .route(
                &format!("{}/{{token}}", kind.unsubscribe_path()),
                get(move |token: Path<String>| unsubscribe_page(kind, token)).post(
                    move |api: State<Arc<DashApi>>, token: Path<String>| {
                        unsubscribe_by_token(kind, api, token)
                    },
                ),
            )
            .with_state(Arc::clone(api)),
        route,
    )
}

pub(crate) fn routes(api: &Arc<DashApi>) -> Router {
    endpoint(
        api,
        "internal.weekly_report_account",
        "/api/internal/account/weekly-report",
        get(account_status).fallback(|| method_not_allowed("GET, HEAD")),
    )
    .merge(endpoint(
        api,
        "internal.weekly_report_confirmation",
        "/api/internal/account/weekly-report/confirmation",
        post(request_confirmation)
            .delete(cancel_confirmation)
            .fallback(|| method_not_allowed("POST, DELETE")),
    ))
    .merge(endpoint(
        api,
        "internal.weekly_report",
        "/api/internal/sites/{site}/weekly-report",
        switch(Kind::Weekly),
    ))
    .merge(endpoint(
        api,
        "internal.monthly_report",
        "/api/internal/sites/{site}/monthly-report",
        switch(Kind::Monthly),
    ))
    // These endpoints authorize solely with the mail token, never cookies.
    .merge(crate::query_api::count_requests(
        Router::new()
            .route(
                "/api/weekly-report/confirm/{token}",
                get(confirm_info).post(confirm_by_token),
            )
            .with_state(Arc::clone(api)),
        "weekly_report.confirm",
    ))
    .merge(unsubscribe_link(
        api,
        Kind::Weekly,
        "weekly_report.unsubscribe",
    ))
    .merge(unsubscribe_link(
        api,
        Kind::Monthly,
        "monthly_report.unsubscribe",
    ))
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Lang {
    Zh,
    En,
}

impl Lang {
    fn as_str(self) -> &'static str {
        match self {
            Self::Zh => "zh",
            Self::En => "en",
        }
    }

    fn mail(self) -> rivu_mail::Lang {
        match self {
            Self::Zh => rivu_mail::Lang::Zh,
            Self::En => rivu_mail::Lang::En,
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LangBody {
    lang: Lang,
}

struct Recipient {
    email: String,
    timezone: String,
    week_starts_on: i16,
}

/// Locks an active account; every write to its mailbox or subscriptions holds this lock.
async fn lock_user(tx: &mut PgConnection, user_id: i64) -> anyhow::Result<Option<String>> {
    Ok(sqlx::query_scalar(
        "SELECT email::text FROM users WHERE id = $1 AND disabled_at IS NULL
         AND deleted_at IS NULL FOR NO KEY UPDATE",
    )
    .bind(user_id)
    .fetch_optional(tx)
    .await?)
}

// The user lock serializes mailbox and subscription changes with claims.
// Share locks keep the account's site, calendar and access valid through commit.
async fn lock_recipient(
    tx: &mut PgConnection,
    user_id: i64,
    site_id: i64,
) -> anyhow::Result<Option<Recipient>> {
    let Some(email) = lock_user(&mut *tx, user_id).await? else {
        return Ok(None);
    };
    let Some((timezone, org_id)) = sqlx::query_as::<_, (String, i64)>(
        "SELECT s.timezone, o.id FROM sites s
         JOIN organizations o ON o.id = s.organization_id AND o.deleted_at IS NULL
         WHERE s.id = $1 AND s.deleted_at IS NULL FOR SHARE OF s, o",
    )
    .bind(site_id)
    .fetch_optional(&mut *tx)
    .await?
    else {
        return Ok(None);
    };
    let (org_role, guest_role, week_starts_on) = sqlx::query_as(
        "SELECT
          (SELECT role FROM memberships WHERE organization_id = $1 AND user_id = $2
           AND deleted_at IS NULL FOR SHARE),
          (SELECT role FROM site_memberships WHERE site_id = $3 AND user_id = $2
           AND deleted_at IS NULL FOR SHARE),
          COALESCE((SELECT week_starts_on FROM site_settings WHERE site_id = $3 FOR SHARE), 1)::smallint",
    )
    .bind(org_id)
    .bind(user_id)
    .bind(site_id)
    .fetch_one(tx)
    .await?;
    Ok(may_read_stats(&UserSiteRoles {
        org_role,
        guest_role,
    })
    .then_some(Recipient {
        email,
        timezone,
        week_starts_on,
    }))
}

async fn site_id(api: &DashApi, site: &str) -> Res<i64> {
    let site = SiteRef::parse(site).ok_or(DashError::NotFound)?;
    let site = access::find_site(&api.pg, &site)
        .await?
        .ok_or(DashError::NotFound)?;
    Ok(i64::try_from(site.id).map_err(anyhow::Error::from)?)
}

async fn begin(
    api: &DashApi,
    user_id: i64,
    site_id: i64,
) -> Res<(Transaction<'static, Postgres>, Recipient)> {
    let mut tx = api.pg.begin().await.map_err(anyhow::Error::from)?;
    let recipient = lock_recipient(&mut tx, user_id, site_id)
        .await?
        .ok_or(DashError::NotFound)?;
    Ok((tx, recipient))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum EmailStatus {
    Confirmed,
    Pending,
    Unconfirmed,
}

impl EmailStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Confirmed => "confirmed",
            Self::Pending => "pending",
            Self::Unconfirmed => "unconfirmed",
        }
    }
}

/// The mailbox state for the account's current address.
async fn email_status<'c>(
    db: impl PgExecutor<'c>,
    user_id: i64,
    email: &str,
) -> anyhow::Result<EmailStatus> {
    let row = sqlx::query_as::<_, (bool, bool)>(
        "SELECT confirmed_at IS NOT NULL,
                COALESCE(token_expires_at > statement_timestamp(), false)
         FROM weekly_report_emails WHERE user_id = $1 AND email = $2",
    )
    .bind(user_id)
    .bind(email)
    .fetch_optional(db)
    .await?;
    Ok(match row {
        Some((true, _)) => EmailStatus::Confirmed,
        Some((false, true)) => EmailStatus::Pending,
        _ => EmailStatus::Unconfirmed,
    })
}

/// `GET /api/internal/account/weekly-report`; the confirmation request returns it too.
async fn account_body(api: &DashApi, user_id: i64, email: &str) -> Res<Value> {
    let status = email_status(&api.pg, user_id, email).await?;
    let mut sites: Vec<_> = access::list_visible_sites(&api.pg, user_id)
        .await?
        .into_iter()
        .filter(|s| may_read_stats(&s.roles))
        .collect();
    // Case-insensitive, so "blog" does not sort after "Zeta"; the id keeps equal names stable.
    sites.sort_by_cached_key(|s| (s.name.to_lowercase(), s.id));
    let ids = sites
        .iter()
        .map(|s| i64::try_from(s.id))
        .collect::<Result<Vec<_>, _>>()
        .map_err(anyhow::Error::from)?;
    let details: HashMap<i64, (i16, bool, bool)> = sqlx::query_as::<_, (i64, i16, bool, bool)>(
        "SELECT s.id, COALESCE(ss.week_starts_on, 1)::smallint,
                EXISTS (SELECT FROM weekly_report_subscriptions w
                        WHERE w.user_id = $2 AND w.site_id = s.id AND w.kind = 'weekly'),
                EXISTS (SELECT FROM weekly_report_subscriptions w
                        WHERE w.user_id = $2 AND w.site_id = s.id AND w.kind = 'monthly')
         FROM unnest($1::bigint[]) AS s (id)
         LEFT JOIN site_settings ss ON ss.site_id = s.id",
    )
    .bind(&ids)
    .bind(user_id)
    .fetch_all(&api.pg)
    .await
    .map_err(anyhow::Error::from)?
    .into_iter()
    .map(|(id, week_starts_on, weekly, monthly)| (id, (week_starts_on, weekly, monthly)))
    .collect();
    let sites: Vec<Value> = sites
        .iter()
        .zip(&ids)
        .map(|(site, id)| {
            let (week_starts_on, weekly, monthly) =
                details.get(id).copied().unwrap_or((1, false, false));
            // `subscribed` is the weekly switch; it predates the monthly report.
            json!({
                "id": site.id,
                "name": site.name,
                "timezone": site.timezone,
                "week_starts_on": week_starts_on,
                "subscribed": weekly,
                "monthly_subscribed": monthly,
            })
        })
        .collect();
    Ok(json!({
        "available": api.mailer.is_some(),
        "email": email,
        "email_status": status.as_str(),
        "sites": sites,
    }))
}

async fn account_status(State(api): State<Arc<DashApi>>, headers: HeaderMap) -> Res<Response> {
    let ctx = api.require_session(&headers).await?;
    let body = account_body(&api, ctx.s.user_id, &ctx.s.email).await?;
    Ok(api.finish(json_response(StatusCode::OK, body), &ctx))
}

fn mail_unavailable() -> Response {
    json_response(
        StatusCode::SERVICE_UNAVAILABLE,
        json!({"error": "mail_unavailable", "message": "email delivery is unavailable"}),
    )
}

/// The mailbox row before a new link replaced it, restored if the email cannot be sent.
/// Timestamps travel as text so they round-trip exactly.
type SavedEmail = (
    String,
    String,
    Option<Vec<u8>>,
    Option<String>,
    Option<String>,
);

async fn request_confirmation(State(api): State<Arc<DashApi>>, req: Request) -> Res<Response> {
    let ctx = api.require_session(req.headers()).await?;
    let LangBody { lang } = read_json(req, 256).await?;
    let Some(mailer) = api.mailer.clone() else {
        return Ok(api.finish(mail_unavailable(), &ctx));
    };
    let user_id = ctx.s.user_id;
    let mut tx = api.pg.begin().await.map_err(anyhow::Error::from)?;
    let email = lock_user(&mut tx, user_id)
        .await?
        .ok_or(DashError::Unauthorized)?;
    if email_status(&mut *tx, user_id, &email).await? == EmailStatus::Confirmed {
        sqlx::query("UPDATE weekly_report_emails SET lang = $2 WHERE user_id = $1")
            .bind(user_id)
            .bind(lang.as_str())
            .execute(&mut *tx)
            .await
            .map_err(anyhow::Error::from)?;
        tx.commit().await.map_err(anyhow::Error::from)?;
        let body = account_body(&api, user_id, &email).await?;
        return Ok(api.finish(json_response(StatusCode::OK, body), &ctx));
    }
    let wait: Option<f64> = sqlx::query_scalar(
        "SELECT extract(epoch FROM requested_at + interval '1 minute' - statement_timestamp())::float8
         FROM weekly_report_emails WHERE user_id = $1",
    )
    .bind(user_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(anyhow::Error::from)?
    .flatten();
    if let Some(wait) = wait.filter(|w| *w > 0.0) {
        return Err(DashError::RateLimited(Duration::from_secs_f64(wait)));
    }
    let saved: Option<SavedEmail> = sqlx::query_as(
        "SELECT email, lang, token_hash, token_expires_at::text, confirmed_at::text
         FROM weekly_report_emails WHERE user_id = $1",
    )
    .bind(user_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(anyhow::Error::from)?;
    let token = session::generate();
    let hash = token.hash();
    sqlx::query(
        "INSERT INTO weekly_report_emails
             (user_id, email, lang, requested_at, token_hash, token_expires_at)
         VALUES ($1, $2, $3, statement_timestamp(), $4, statement_timestamp() + interval '1 hour')
         ON CONFLICT (user_id) DO UPDATE
         SET email = excluded.email, lang = excluded.lang, requested_at = excluded.requested_at,
             token_hash = excluded.token_hash, token_expires_at = excluded.token_expires_at,
             confirmed_at = NULL",
    )
    .bind(user_id)
    .bind(&email)
    .bind(lang.as_str())
    .bind(hash.as_slice())
    .execute(&mut *tx)
    .await
    .map_err(anyhow::Error::from)?;
    tx.commit().await.map_err(anyhow::Error::from)?;

    let base = api.base_url.trim_end_matches('/');
    let link = format!("{base}{CONFIRM_PAGE}/{}", token.plaintext());
    let mail = confirmation(lang.mail(), &email, &link);
    if mailer.send(mail).await.is_err() {
        tracing::warn!(user_id, "weekly report confirmation email failed");
        // Nobody received the new link: put the earlier state back, but keep the request time
        // so the rate limit still counts this attempt.
        let (email, lang, token_hash, expires_at, confirmed_at) = match saved {
            Some((email, lang, hash, expires, confirmed)) => {
                (Some(email), Some(lang), hash, expires, confirmed)
            }
            None => (None, None, None, None, None),
        };
        sqlx::query(
            "UPDATE weekly_report_emails
             SET email = COALESCE($3, email), lang = COALESCE($4, lang), token_hash = $5,
                 token_expires_at = $6::timestamptz, confirmed_at = $7::timestamptz
             WHERE user_id = $1 AND token_hash = $2",
        )
        .bind(user_id)
        .bind(hash.as_slice())
        .bind(email)
        .bind(lang)
        .bind(token_hash)
        .bind(expires_at)
        .bind(confirmed_at)
        .execute(&api.pg)
        .await
        .map_err(anyhow::Error::from)?;
        return Ok(api.finish(
            json_response(
                StatusCode::BAD_GATEWAY,
                json!({"error": "mail_failed", "message": "the confirmation email could not be delivered"}),
            ),
            &ctx,
        ));
    }
    let body = account_body(&api, user_id, &email).await?;
    Ok(api.finish(json_response(StatusCode::OK, body), &ctx))
}

async fn cancel_confirmation(State(api): State<Arc<DashApi>>, headers: HeaderMap) -> Res<Response> {
    let ctx = api.require_session(&headers).await?;
    let mut tx = api.pg.begin().await.map_err(anyhow::Error::from)?;
    lock_user(&mut tx, ctx.s.user_id)
        .await?
        .ok_or(DashError::Unauthorized)?;
    sqlx::query(
        "UPDATE weekly_report_emails SET token_hash = NULL, token_expires_at = NULL
         WHERE user_id = $1",
    )
    .bind(ctx.s.user_id)
    .execute(&mut *tx)
    .await
    .map_err(anyhow::Error::from)?;
    tx.commit().await.map_err(anyhow::Error::from)?;
    let mut response = StatusCode::NO_CONTENT.into_response();
    no_store(&mut response);
    Ok(api.finish(response, &ctx))
}

async fn subscribe(
    kind: Kind,
    State(api): State<Arc<DashApi>>,
    Path(site): Path<String>,
    req: Request,
) -> Res<Response> {
    let ctx = api.require_session(req.headers()).await?;
    let LangBody { lang } = read_json(req, 256).await?;
    let site_id = site_id(&api, &site).await?;
    let user_id = ctx.s.user_id;
    let (mut tx, recipient) = begin(&api, user_id, site_id).await?;
    if email_status(&mut *tx, user_id, &recipient.email).await? != EmailStatus::Confirmed {
        return Err(DashError::Conflict(
            "email_unconfirmed",
            format!(
                "confirm your email address before subscribing to {} reports",
                kind.as_str()
            ),
        ));
    }
    // An unchanged address keeps its subscription time, so earlier unsubscribe links stay valid.
    sqlx::query(
        "INSERT INTO weekly_report_subscriptions AS w
             (user_id, site_id, kind, email, lang, confirmed_at)
         VALUES ($1, $2, $5, $3, $4, statement_timestamp())
         ON CONFLICT (user_id, site_id, kind) DO UPDATE
         SET lang = excluded.lang, email = excluded.email,
             confirmed_at = CASE WHEN w.email = excluded.email THEN w.confirmed_at
                                 ELSE excluded.confirmed_at END",
    )
    .bind(user_id)
    .bind(site_id)
    .bind(&recipient.email)
    .bind(lang.as_str())
    .bind(kind.as_str())
    .execute(&mut *tx)
    .await
    .map_err(anyhow::Error::from)?;
    tx.commit().await.map_err(anyhow::Error::from)?;
    let mut response = StatusCode::NO_CONTENT.into_response();
    no_store(&mut response);
    Ok(api.finish(response, &ctx))
}

async fn unsubscribe(
    kind: Kind,
    State(api): State<Arc<DashApi>>,
    Path(site): Path<String>,
    headers: HeaderMap,
) -> Res<Response> {
    let ctx = api.require_session(&headers).await?;
    let site_id = site_id(&api, &site).await?;
    let user_id = ctx.s.user_id;
    let (mut tx, _) = begin(&api, user_id, site_id).await?;
    sqlx::query(
        "DELETE FROM weekly_report_subscriptions WHERE user_id = $1 AND site_id = $2 AND kind = $3",
    )
    .bind(user_id)
    .bind(site_id)
    .bind(kind.as_str())
    .execute(&mut *tx)
    .await
    .map_err(anyhow::Error::from)?;
    tx.commit().await.map_err(anyhow::Error::from)?;
    let mut response = StatusCode::NO_CONTENT.into_response();
    no_store(&mut response);
    Ok(api.finish(response, &ctx))
}

/// Token endpoints answer every failure with the same 404 and never leak the link to a referrer.
fn token_response(result: Res<Value>) -> Response {
    let mut response = match result {
        Ok(body) => json_response(StatusCode::OK, body),
        Err(error) => error.into_response(),
    };
    response
        .headers_mut()
        .insert("referrer-policy", HeaderValue::from_static("no-referrer"));
    response
}

/// Reading the link changes nothing, so a mail client that prefetches it confirms nothing.
async fn confirm_info(State(api): State<Arc<DashApi>>, Path(token): Path<String>) -> Response {
    token_response(pending_email(&api, &token).await)
}

async fn pending_email(api: &DashApi, token: &str) -> Res<Value> {
    if !session::is_well_formed(token) {
        return Err(DashError::NotFound);
    }
    let email: Option<String> = sqlx::query_scalar(
        "SELECT e.email FROM weekly_report_emails e
         JOIN users u ON u.id = e.user_id AND u.disabled_at IS NULL AND u.deleted_at IS NULL
                     AND u.email::text = e.email
         WHERE e.token_hash = $1 AND e.token_expires_at > statement_timestamp()",
    )
    .bind(session::hash_token(token).as_slice())
    .fetch_optional(&api.pg)
    .await
    .map_err(anyhow::Error::from)?;
    let email = email.ok_or(DashError::NotFound)?;
    Ok(json!({ "email": email }))
}

async fn confirm_by_token(State(api): State<Arc<DashApi>>, Path(token): Path<String>) -> Response {
    token_response(confirm_email(&api, &token).await)
}

async fn confirm_email(api: &DashApi, token: &str) -> Res<Value> {
    if !session::is_well_formed(token) {
        return Err(DashError::NotFound);
    }
    let hash = session::hash_token(token);
    let mut tx = api.pg.begin().await.map_err(anyhow::Error::from)?;
    let user_id: i64 =
        sqlx::query_scalar("SELECT user_id FROM weekly_report_emails WHERE token_hash = $1")
            .bind(hash.as_slice())
            .fetch_optional(&mut *tx)
            .await
            .map_err(anyhow::Error::from)?
            .ok_or(DashError::NotFound)?;
    let email = lock_user(&mut tx, user_id)
        .await?
        .ok_or(DashError::NotFound)?;
    let confirmed = sqlx::query(
        "UPDATE weekly_report_emails
         SET confirmed_at = statement_timestamp(), token_hash = NULL, token_expires_at = NULL
         WHERE user_id = $1 AND token_hash = $2 AND email = $3
           AND token_expires_at > statement_timestamp()",
    )
    .bind(user_id)
    .bind(hash.as_slice())
    .bind(&email)
    .execute(&mut *tx)
    .await
    .map_err(anyhow::Error::from)?
    .rows_affected();
    if confirmed == 0 {
        return Err(DashError::NotFound);
    }
    // Sites switched on under an earlier address follow the newly confirmed mailbox. The new
    // subscription time retires unsubscribe links that were mailed to the earlier address.
    sqlx::query(
        "UPDATE weekly_report_subscriptions SET email = $2, confirmed_at = statement_timestamp()
         WHERE user_id = $1 AND email <> $2",
    )
    .bind(user_id)
    .bind(&email)
    .execute(&mut *tx)
    .await
    .map_err(anyhow::Error::from)?;
    tx.commit().await.map_err(anyhow::Error::from)?;
    Ok(json!({ "email": email }))
}

fn unsubscribe_response(page: &'static str) -> Response {
    let mut response = Html(page).into_response();
    no_store(&mut response);
    response
        .headers_mut()
        .insert("referrer-policy", HeaderValue::from_static("no-referrer"));
    response.headers_mut().insert(
        "content-security-policy",
        HeaderValue::from_static("default-src 'none'; form-action 'self'; frame-ancestors 'none'"),
    );
    response
}

/// The bilingual pages behind an unsubscribe link: (confirmation page, done, already off).
fn unsubscribe_pages(kind: Kind) -> [&'static str; 3] {
    match kind {
        Kind::Weekly => [
            r#"<!doctype html><html lang="en"><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1"><title>Unsubscribe · Rivu</title>
        <h1>Unsubscribe from Rivu weekly reports</h1>
        <p lang="zh">退订 Rivu 周报</p>
        <form method="post"><button>Unsubscribe · <span lang="zh">退订</span></button></form></html>"#,
            r#"<!doctype html><html lang="en"><meta charset="utf-8"><title>Rivu weekly reports</title>
        <p>You have unsubscribed from Rivu weekly reports for this site.</p><p lang="zh">你已退订此站点的 Rivu 周报。</p></html>"#,
            r#"<!doctype html><html lang="en"><meta charset="utf-8"><title>Rivu weekly reports</title>
            <p>This link has expired or the subscription is already off.</p><p lang="zh">退订链接已过期，或订阅已关闭。</p></html>"#,
        ],
        Kind::Monthly => [
            r#"<!doctype html><html lang="en"><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1"><title>Unsubscribe · Rivu</title>
        <h1>Unsubscribe from Rivu monthly reports</h1>
        <p lang="zh">退订 Rivu 月报</p>
        <form method="post"><button>Unsubscribe · <span lang="zh">退订</span></button></form></html>"#,
            r#"<!doctype html><html lang="en"><meta charset="utf-8"><title>Rivu monthly reports</title>
        <p>You have unsubscribed from Rivu monthly reports for this site.</p><p lang="zh">你已退订此站点的 Rivu 月报。</p></html>"#,
            r#"<!doctype html><html lang="en"><meta charset="utf-8"><title>Rivu monthly reports</title>
            <p>This link has expired or the subscription is already off.</p><p lang="zh">退订链接已过期，或订阅已关闭。</p></html>"#,
        ],
    }
}

async fn unsubscribe_page(kind: Kind, Path(token): Path<String>) -> Res<Response> {
    if !session::is_well_formed(&token) {
        return Err(DashError::NotFound);
    }
    Ok(unsubscribe_response(unsubscribe_pages(kind)[0]))
}

/// Switches off only the report and the site the email was about; the mailbox stays confirmed.
/// The request body is ignored, so this is also the RFC 8058 one-click target: mailbox providers
/// POST `List-Unsubscribe=One-Click` here without cookies.
async fn unsubscribe_by_token(
    kind: Kind,
    State(api): State<Arc<DashApi>>,
    Path(token): Path<String>,
) -> Res<Response> {
    if !session::is_well_formed(&token) {
        return Err(DashError::NotFound);
    }
    let deleted = sqlx::query(
        "DELETE FROM weekly_report_subscriptions w
         USING weekly_report_claims c
         WHERE c.unsubscribe_hash = $1 AND c.kind = $2
           AND w.user_id = c.user_id AND w.site_id = c.site_id AND w.kind = c.kind
           AND w.confirmed_at = c.confirmed_at",
    )
    .bind(session::hash_token(&token).as_slice())
    .bind(kind.as_str())
    .execute(&api.pg)
    .await
    .map_err(anyhow::Error::from)?
    .rows_affected();
    let [_, done, already_off] = unsubscribe_pages(kind);
    Ok(unsubscribe_response(if deleted == 0 {
        already_off
    } else {
        done
    }))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Period {
    from: Date,
    to: Date,
    start: Timestamp,
    end: Timestamp,
}

impl Period {
    /// `[from, first)` as local dates and their local midnights.
    fn new(from: Date, first: Date, tz: &TimeZone) -> anyhow::Result<Self> {
        Ok(Period {
            from,
            to: first.yesterday()?,
            start: from.to_zoned(tz.clone())?.timestamp(),
            end: first.to_zoned(tz.clone())?.timestamp(),
        })
    }
}

/// Reports go out from 09:00 local time on the first day after the period.
fn sendable(now: Timestamp, first: Date, tz: &TimeZone) -> anyhow::Result<bool> {
    Ok(now >= first.at(9, 0, 0, 0).to_zoned(tz.clone())?.timestamp())
}

/// The latest complete local week, once it may be sent.
fn due_period(
    now: Timestamp,
    tz: &TimeZone,
    week_start: Weekday,
) -> anyhow::Result<Option<Period>> {
    let today = now.to_zoned(tz.clone()).date();
    let offset = (today.weekday().to_sunday_zero_offset() - week_start.to_sunday_zero_offset())
        .rem_euclid(7);
    let end_day = today.checked_sub(offset.days())?;
    if !sendable(now, end_day, tz)? {
        return Ok(None);
    }
    Ok(Some(Period::new(
        end_day.checked_sub(7.days())?,
        end_day,
        tz,
    )?))
}

/// The previous local calendar month, once it may be sent (09:00 on the 1st).
fn due_month(now: Timestamp, tz: &TimeZone) -> anyhow::Result<Option<Period>> {
    let first = now.to_zoned(tz.clone()).date().first_of_month();
    if !sendable(now, first, tz)? {
        return Ok(None);
    }
    Ok(Some(Period::new(first.checked_sub(1.month())?, first, tz)?))
}

struct Site {
    id: i64,
    key: String,
    name: String,
    timezone: String,
    week_starts_on: i16,
}

impl Site {
    fn due(&self, kind: Kind, now: Timestamp) -> anyhow::Result<Option<Period>> {
        let tz = TimeZone::get(&self.timezone)?;
        match kind {
            Kind::Weekly => {
                let week_start =
                    Weekday::from_sunday_zero_offset(i8::try_from(self.week_starts_on)?)?;
                due_period(now, &tz, week_start)
            }
            Kind::Monthly => due_month(now, &tz),
        }
    }
}

async fn candidates(
    pg: &PgPool,
    kind: Kind,
    site_id: i64,
    period: Period,
) -> anyhow::Result<Vec<i64>> {
    Ok(sqlx::query_scalar(
        "SELECT w.user_id FROM weekly_report_subscriptions w
         JOIN users u ON u.id = w.user_id AND u.disabled_at IS NULL AND u.deleted_at IS NULL
         JOIN sites s ON s.id = w.site_id
         JOIN weekly_report_emails e ON e.user_id = w.user_id AND e.email = w.email
                                     AND e.confirmed_at IS NOT NULL
         WHERE w.site_id = $1 AND w.kind = $4 AND w.email = u.email::text
           AND w.confirmed_at < to_timestamp($2)
           AND (EXISTS (SELECT FROM memberships m WHERE m.organization_id = s.organization_id
                        AND m.user_id = w.user_id AND m.deleted_at IS NULL)
                OR EXISTS (SELECT FROM site_memberships m WHERE m.site_id = w.site_id
                           AND m.user_id = w.user_id AND m.deleted_at IS NULL))
           AND NOT EXISTS (SELECT FROM weekly_report_claims c WHERE c.user_id = w.user_id
                           AND c.site_id = w.site_id AND c.kind = w.kind
                           AND c.period_end > to_timestamp($3))
         ORDER BY w.user_id",
    )
    .bind(site_id)
    .bind(period.end.as_second() as f64)
    .bind(period.start.as_second() as f64)
    .bind(kind.as_str())
    .fetch_all(pg)
    .await?)
}

async fn claim(
    api: &DashApi,
    kind: Kind,
    site: &Site,
    user_id: i64,
    period: Period,
    unsubscribe_hash: &[u8; 32],
) -> anyhow::Result<Option<(String, String)>> {
    let mut tx = api.pg.begin().await?;
    let Some(recipient) = lock_recipient(&mut tx, user_id, site.id).await? else {
        return Ok(None);
    };
    // The week start only shapes weekly periods.
    if recipient.timezone != site.timezone
        || (kind == Kind::Weekly && recipient.week_starts_on != site.week_starts_on)
        || site.due(kind, (api.clock)())? != Some(period)
    {
        return Ok(None);
    }
    // A previous end after this start fences overlap after any calendar change.
    let lang = sqlx::query_scalar(
        "WITH eligible AS (
             SELECT lang, confirmed_at FROM weekly_report_subscriptions
             WHERE user_id = $1 AND site_id = $2 AND kind = $7 AND email = $3
               AND confirmed_at < to_timestamp($4)
               AND EXISTS (SELECT FROM weekly_report_emails
                   WHERE user_id = $1 AND email = $3 AND confirmed_at IS NOT NULL)
               AND NOT EXISTS (SELECT FROM weekly_report_claims
                   WHERE user_id = $1 AND site_id = $2 AND kind = $7
                     AND period_end > to_timestamp($5))
         ), claimed AS (
             INSERT INTO weekly_report_claims
                 (user_id, site_id, kind, period_end, confirmed_at, unsubscribe_hash)
             SELECT $1, $2, $7, to_timestamp($4), confirmed_at, $6 FROM eligible
             ON CONFLICT DO NOTHING RETURNING user_id
         )
         SELECT lang FROM eligible CROSS JOIN claimed",
    )
    .bind(user_id)
    .bind(site.id)
    .bind(&recipient.email)
    .bind(period.end.as_second() as f64)
    .bind(period.start.as_second() as f64)
    .bind(unsubscribe_hash.as_slice())
    .bind(kind.as_str())
    .fetch_optional(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(lang.map(|lang| (recipient.email, lang)))
}

fn totals(result: &QueryResult) -> anyhow::Result<(u64, u64, u64)> {
    anyhow::ensure!(
        result.meta.warnings.is_empty(),
        "report query warnings: {:?}",
        result.meta.warnings
    );
    match result.results.as_slice() {
        [row] => match row.metrics.as_slice() {
            [
                MetricValue::Int(visitors),
                MetricValue::Int(visits),
                MetricValue::Int(pageviews),
            ] => Ok((*visitors, *visits, *pageviews)),
            _ => anyhow::bail!("report query did not return three integer totals"),
        },
        _ => anyhow::bail!("report query did not return a totals row"),
    }
}

async fn deliver(
    api: &DashApi,
    mailer: &rivu_mail::Mailer,
    kind: Kind,
    site: &Site,
    now: Timestamp,
) -> anyhow::Result<()> {
    let Some(period) = site.due(kind, now)? else {
        return Ok(());
    };
    let candidates = candidates(&api.pg, kind, site.id, period).await?;
    if candidates.is_empty() {
        return Ok(());
    }
    let context = load_site_context(
        &api.pg,
        None,
        u64::try_from(site.id)?,
        &site.key,
        api.retention_days,
    )
    .await
    .map_err(|_| anyhow::anyhow!("could not load site query settings"))?;
    if context.timezone != TimeZone::get(&site.timezone)?
        || (kind == Kind::Weekly
            && i16::from(context.week_starts_on.to_sunday_zero_offset()) != site.week_starts_on)
    {
        return Ok(());
    }
    let spec = QuerySpec::parse(&json!({
        "metrics": ["visitors", "visits", "pageviews"],
        "date_range": [period.from.to_string(), period.to.to_string()],
    }))?;
    let mut options = ExecOptions::new(api.dashboard_timeout_secs)
        .with_now(now)
        .with_route(kind.route());
    options.rollup_min_rows = api.rollup_min_rows;
    let result = execute_cached(&api.reader, None, &context, spec, options, kind.route()).await?;
    let (visitors, visits, pageviews) = totals(&result)?;
    let base = api.base_url.trim_end_matches('/');
    for user_id in candidates {
        let token = session::generate();
        let (email, lang) = match claim(api, kind, site, user_id, period, &token.hash()).await {
            Ok(Some(recipient)) => recipient,
            Ok(None) => continue,
            Err(error) => {
                tracing::warn!(
                    site_id = site.id,
                    user_id,
                    kind = kind.as_str(),
                    %error,
                    "report claim failed"
                );
                continue;
            }
        };
        let unsubscribe_url = format!("{base}{}/{}", kind.unsubscribe_path(), token.plaintext());
        let mail = report(&Report {
            lang: if lang == "zh" {
                rivu_mail::Lang::Zh
            } else {
                rivu_mail::Lang::En
            },
            cadence: kind.cadence(),
            to: &email,
            base_url: base,
            site_id: site.id,
            site_name: &site.name,
            timezone: &site.timezone,
            from: period.from,
            until: period.to,
            visitors,
            visits,
            pageviews,
            unsubscribe_url: &unsubscribe_url,
        });
        if mailer.send(mail).await.is_err() {
            tracing::warn!(
                site_id = site.id,
                user_id,
                kind = kind.as_str(),
                "report SMTP attempt failed"
            );
        }
    }
    Ok(())
}

/// Evaluate only the latest completed local week and month, grouped by report and site.
pub async fn run_once(api: &DashApi) -> anyhow::Result<()> {
    let Some(mailer) = &api.mailer else {
        return Ok(());
    };
    let now = (api.clock)();
    for kind in Kind::ALL {
        let sites = sqlx::query_as::<_, (i64, String, String, String, i16)>(
            "SELECT s.id, s.site_key, s.name, s.timezone, COALESCE(ss.week_starts_on, 1)::smallint
             FROM sites s JOIN organizations o ON o.id = s.organization_id AND o.deleted_at IS NULL
             LEFT JOIN site_settings ss ON ss.site_id = s.id
             WHERE s.deleted_at IS NULL AND EXISTS (
                 SELECT FROM weekly_report_subscriptions w WHERE w.site_id = s.id AND w.kind = $1)
             ORDER BY s.id",
        )
        .bind(kind.as_str())
        .fetch_all(&api.pg)
        .await?;
        for (id, key, name, timezone, week_starts_on) in sites {
            let site = Site {
                id,
                key,
                name,
                timezone,
                week_starts_on,
            };
            if let Err(error) = deliver(api, mailer, kind, &site, now).await {
                tracing::warn!(site_id = id, kind = kind.as_str(), %error, "report skipped");
            }
        }
    }
    Ok(())
}

pub(crate) fn spawn(
    api: Arc<DashApi>,
    mut stop: watch::Receiver<bool>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticks = tokio::time::interval(Duration::from_secs(60));
        ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                biased;
                _ = stop.wait_for(|stopped| *stopped) => break,
                _ = async {
                    ticks.tick().await;
                    if let Err(error) = run_once(&api).await {
                        tracing::warn!(%error, "report tick failed");
                    }
                } => {}
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_day_waits_until_nine_in_the_site_timezone() {
        let tz = TimeZone::get("Asia/Taipei").unwrap();
        let due = |at: &str| due_period(at.parse().unwrap(), &tz, Weekday::Sunday).unwrap();
        assert_eq!(due("2027-01-03T00:59:59Z"), None);
        let period = due("2027-01-03T01:00:00Z").unwrap();
        assert_eq!(period.from.to_string(), "2026-12-27");
        assert_eq!(period.to.to_string(), "2027-01-02");
        assert_eq!(due("2027-01-09T15:59:59Z"), Some(period));
        assert_eq!(due("2027-01-09T16:00:00Z"), None);
    }

    #[test]
    fn dst_weeks_use_local_midnights_in_both_directions() {
        let tz = TimeZone::get("America/New_York").unwrap();
        for (at, start, end, hours) in [
            (
                "2026-03-15T13:00:00Z",
                "2026-03-08T05:00:00Z",
                "2026-03-15T04:00:00Z",
                167,
            ),
            (
                "2026-11-08T14:00:00Z",
                "2026-11-01T04:00:00Z",
                "2026-11-08T05:00:00Z",
                169,
            ),
        ] {
            let period = due_period(at.parse().unwrap(), &tz, Weekday::Sunday)
                .unwrap()
                .unwrap();
            assert_eq!(period.start, start.parse::<Timestamp>().unwrap());
            assert_eq!(period.end, end.parse::<Timestamp>().unwrap());
            assert_eq!(
                period.end.as_second() - period.start.as_second(),
                hours * 3600
            );
        }
    }
    #[test]
    fn a_month_is_due_from_nine_on_the_first_in_the_site_timezone() {
        let tz = TimeZone::get("Asia/Taipei").unwrap();
        let due = |at: &str| due_month(at.parse().unwrap(), &tz).unwrap();
        let october = due("2026-11-30T15:59:59Z").unwrap();
        assert_eq!(october.from.to_string(), "2026-10-01");
        assert_eq!(october.to.to_string(), "2026-10-31");
        // Local midnight on the 1st starts the new month, but nothing is due until 09:00.
        assert_eq!(due("2026-11-30T16:00:00Z"), None);
        assert_eq!(due("2026-12-01T00:59:59Z"), None);
        let november = due("2026-12-01T01:00:00Z").unwrap();
        assert_eq!(november.from.to_string(), "2026-11-01");
        assert_eq!(november.to.to_string(), "2026-11-30");
        assert_eq!(
            november.start,
            "2026-10-31T16:00:00Z".parse::<Timestamp>().unwrap()
        );
        assert_eq!(
            november.end,
            "2026-11-30T16:00:00Z".parse::<Timestamp>().unwrap()
        );
        assert_eq!(due("2026-12-31T15:59:59Z"), Some(november));
    }

    #[test]
    fn december_is_due_on_new_years_day_west_of_utc() {
        let tz = TimeZone::get("America/New_York").unwrap();
        let due = |at: &str| due_month(at.parse().unwrap(), &tz).unwrap();
        // Already 2027 in UTC, still 31 December in New York: November is the latest month.
        let november = due("2027-01-01T04:59:59Z").unwrap();
        assert_eq!(november.to.to_string(), "2026-11-30");
        assert_eq!(due("2027-01-01T05:00:00Z"), None);
        assert_eq!(due("2027-01-01T13:59:59Z"), None);
        let december = due("2027-01-01T14:00:00Z").unwrap();
        assert_eq!(december.from.to_string(), "2026-12-01");
        assert_eq!(december.to.to_string(), "2026-12-31");
        assert_eq!(
            december.start,
            "2026-12-01T05:00:00Z".parse::<Timestamp>().unwrap()
        );
        assert_eq!(
            december.end,
            "2027-01-01T05:00:00Z".parse::<Timestamp>().unwrap()
        );
        // A month across a DST change is still bounded by local midnights.
        let march = due("2026-04-01T13:00:00Z").unwrap();
        assert_eq!(
            march.start,
            "2026-03-01T05:00:00Z".parse::<Timestamp>().unwrap()
        );
        assert_eq!(
            march.end,
            "2026-04-01T04:00:00Z".parse::<Timestamp>().unwrap()
        );
        // February in a leap year ends on the 29th.
        let february = due("2028-03-01T14:00:00Z").unwrap();
        assert_eq!(february.to.to_string(), "2028-02-29");
    }
}
