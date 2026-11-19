//! Account-bound weekly reports. A committed claim consumes the SMTP attempt.

use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::extract::{Path, Request, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use jiff::civil::{Date, Weekday};
use jiff::tz::TimeZone;
use jiff::{Timestamp, ToSpan};
use rivu_auth::session;
use rivu_mail::Mail;
use rivu_query::result::MetricValue;
use rivu_query::{ExecOptions, QueryResult, QuerySpec};
use rivu_storage_pg::access::{self, SiteRef, UserSiteRoles};
use serde::Deserialize;
use serde_json::json;
use sqlx::{PgConnection, PgPool, Postgres, Transaction};
use tokio::sync::watch;

use crate::dash_api::{
    DashApi, DashError, SessionCtx, endpoint, json_response, may_read_stats, method_not_allowed,
    read_json,
};
use crate::query_api::{execute_cached, load_site_context, no_store};

type Res<T> = Result<T, DashError>;
const UNSUBSCRIBE_PATH: &str = "/api/weekly-report/unsubscribe";

pub(crate) fn routes(api: &Arc<DashApi>) -> Router {
    endpoint(
        api,
        "internal.weekly_report",
        "/api/internal/sites/{site}/weekly-report",
        get(status)
            .post(request)
            .delete(unsubscribe)
            .fallback(|| method_not_allowed("GET, HEAD, POST, DELETE")),
    )
    .merge(endpoint(
        api,
        "internal.weekly_report_confirm",
        "/api/internal/sites/{site}/weekly-report/confirm",
        post(confirm).fallback(|| method_not_allowed("POST")),
    ))
    // This endpoint authorizes solely with the mail token, never cookies.
    .merge(crate::query_api::count_requests(
        Router::new()
            .route(
                "/api/weekly-report/unsubscribe/{token}",
                get(unsubscribe_page).post(unsubscribe_by_token),
            )
            .with_state(Arc::clone(api)),
        "weekly_report.unsubscribe",
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
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RequestCode {
    lang: Lang,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ConfirmCode {
    code: String,
}

struct Recipient {
    email: String,
    timezone: String,
    week_starts_on: i16,
}

// The user lock serializes requests across sites and serializes claims with opt-out.
// Share locks keep the account's site, calendar and access valid through commit.
async fn lock_recipient(
    tx: &mut PgConnection,
    user_id: i64,
    site_id: i64,
) -> anyhow::Result<Option<Recipient>> {
    let Some(email) = sqlx::query_scalar(
        "SELECT email::text FROM users WHERE id = $1 AND disabled_at IS NULL
         AND deleted_at IS NULL FOR NO KEY UPDATE",
    )
    .bind(user_id)
    .fetch_optional(&mut *tx)
    .await?
    else {
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

enum Subscription {
    Off,
    Pending(Vec<u8>),
    Active,
}

async fn subscription(
    tx: &mut PgConnection,
    user_id: i64,
    site_id: i64,
    email: &str,
) -> anyhow::Result<Subscription> {
    let row = sqlx::query_as::<_, (bool, Option<Vec<u8>>)>(
        "SELECT confirmed_at IS NOT NULL,
                CASE WHEN requested_at > statement_timestamp() - interval '15 minutes'
                     THEN code_hash END
         FROM weekly_report_subscriptions WHERE user_id = $1 AND site_id = $2 AND email = $3",
    )
    .bind(user_id)
    .bind(site_id)
    .bind(email)
    .fetch_optional(tx)
    .await?;
    Ok(match row {
        Some((true, _)) => Subscription::Active,
        Some((false, Some(hash))) => Subscription::Pending(hash),
        _ => Subscription::Off,
    })
}

fn status_response(
    api: &DashApi,
    ctx: &SessionCtx,
    recipient: &Recipient,
    status: &str,
) -> Response {
    api.finish(
        json_response(
            StatusCode::OK,
            json!({
                "available": api.mailer.is_some(),
                "status": status,
                "email": recipient.email,
                "timezone": recipient.timezone,
                "week_starts_on": recipient.week_starts_on,
            }),
        ),
        ctx,
    )
}

async fn status(
    State(api): State<Arc<DashApi>>,
    Path(site): Path<String>,
    headers: HeaderMap,
) -> Res<Response> {
    let ctx = api.require_session(&headers).await?;
    let site_id = site_id(&api, &site).await?;
    let user_id = ctx.s.user_id;
    let (mut tx, recipient) = begin(&api, user_id, site_id).await?;
    let state = match subscription(&mut tx, user_id, site_id, &recipient.email).await? {
        Subscription::Off => "off",
        Subscription::Pending(_) => "pending",
        Subscription::Active => "active",
    };
    tx.commit().await.map_err(anyhow::Error::from)?;
    Ok(status_response(&api, &ctx, &recipient, state))
}

fn mail_unavailable() -> Response {
    json_response(
        StatusCode::SERVICE_UNAVAILABLE,
        json!({"error": "mail_unavailable", "message": "email delivery is unavailable"}),
    )
}

async fn request(
    State(api): State<Arc<DashApi>>,
    Path(site): Path<String>,
    req: Request,
) -> Res<Response> {
    let ctx = api.require_session(req.headers()).await?;
    let RequestCode { lang } = read_json(req, 256).await?;
    let site_id = site_id(&api, &site).await?;
    let user_id = ctx.s.user_id;
    let (mut tx, recipient) = begin(&api, user_id, site_id).await?;
    let Some(mailer) = &api.mailer else {
        return Ok(mail_unavailable());
    };
    if matches!(
        subscription(&mut tx, user_id, site_id, &recipient.email).await?,
        Subscription::Active
    ) {
        sqlx::query(
            "UPDATE weekly_report_subscriptions SET lang = $3 WHERE user_id = $1 AND site_id = $2",
        )
        .bind(user_id)
        .bind(site_id)
        .bind(lang.as_str())
        .execute(&mut *tx)
        .await
        .map_err(anyhow::Error::from)?;
        tx.commit().await.map_err(anyhow::Error::from)?;
        return Ok(status_response(&api, &ctx, &recipient, "active"));
    }
    let wait: Option<f64> = sqlx::query_scalar(
        "SELECT extract(epoch FROM max(requested_at) + interval '1 minute' - statement_timestamp())::float8
         FROM weekly_report_subscriptions WHERE user_id = $1",
    )
    .bind(user_id)
    .fetch_one(&mut *tx)
    .await
    .map_err(anyhow::Error::from)?;
    if let Some(wait) = wait.filter(|w| *w > 0.0) {
        return Err(DashError::RateLimited(Duration::from_secs_f64(wait)));
    }
    let token = session::generate();
    sqlx::query(
        "INSERT INTO weekly_report_subscriptions (user_id, site_id, email, lang, requested_at, code_hash)
         VALUES ($1, $2, $3, $4, statement_timestamp(), $5)
         ON CONFLICT (user_id, site_id) DO UPDATE
         SET email = excluded.email, lang = excluded.lang, requested_at = excluded.requested_at,
             code_hash = excluded.code_hash, attempts = 0, confirmed_at = NULL",
    )
    .bind(user_id)
    .bind(site_id)
    .bind(&recipient.email)
    .bind(lang.as_str())
    .bind(token.hash().as_slice())
    .execute(&mut *tx)
    .await
    .map_err(anyhow::Error::from)?;
    tx.commit().await.map_err(anyhow::Error::from)?;

    let (subject, message) = match lang {
        Lang::Zh => (
            "Rivu 周报验证码",
            "请将下面的验证码粘贴到 Rivu。验证码在 15 分钟内有效。",
        ),
        Lang::En => (
            "Rivu weekly report confirmation code",
            "Paste this code into Rivu within 15 minutes.",
        ),
    };
    let mail = Mail {
        to: recipient.email.clone(),
        subject: subject.into(),
        body: format!("{message}\n\n{}\n", token.plaintext()),
        unsubscribe_url: None,
    };
    if mailer.send(mail).await.is_err() {
        tracing::warn!(site_id, user_id, "weekly report confirmation email failed");
        return Ok(json_response(
            StatusCode::BAD_GATEWAY,
            json!({"error": "mail_failed", "message": "the confirmation email could not be delivered"}),
        ));
    }
    Ok(status_response(&api, &ctx, &recipient, "pending"))
}

async fn confirm(
    State(api): State<Arc<DashApi>>,
    Path(site): Path<String>,
    req: Request,
) -> Res<Response> {
    let ctx = api.require_session(req.headers()).await?;
    let ConfirmCode { code } = read_json(req, 256).await?;
    let site_id = site_id(&api, &site).await?;
    let user_id = ctx.s.user_id;
    let (mut tx, recipient) = begin(&api, user_id, site_id).await?;
    if api.mailer.is_none() {
        return Ok(mail_unavailable());
    }
    let invalid = || {
        DashError::BadRequest(
            "invalid_code",
            "the confirmation code is invalid or expired",
        )
    };
    let Subscription::Pending(stored) =
        subscription(&mut tx, user_id, site_id, &recipient.email).await?
    else {
        return Err(invalid());
    };
    let matches = rivu_auth::key::ct_eq(&stored, &session::hash_token(code.trim()));
    let confirmed = sqlx::query_scalar::<_, bool>(
        "UPDATE weekly_report_subscriptions
         SET confirmed_at = CASE WHEN $3 THEN statement_timestamp() END,
             code_hash = CASE WHEN $3 OR attempts = 4 THEN NULL ELSE code_hash END,
             attempts = CASE WHEN $3 THEN 0 ELSE attempts + 1 END
         WHERE user_id = $1 AND site_id = $2
           AND requested_at > statement_timestamp() - interval '15 minutes'
         RETURNING confirmed_at IS NOT NULL",
    )
    .bind(user_id)
    .bind(site_id)
    .bind(matches)
    .fetch_optional(&mut *tx)
    .await
    .map_err(anyhow::Error::from)?;
    tx.commit().await.map_err(anyhow::Error::from)?;
    if confirmed != Some(true) {
        return Err(invalid());
    }
    Ok(status_response(&api, &ctx, &recipient, "active"))
}

async fn unsubscribe(
    State(api): State<Arc<DashApi>>,
    Path(site): Path<String>,
    headers: HeaderMap,
) -> Res<Response> {
    let ctx = api.require_session(&headers).await?;
    let site_id = site_id(&api, &site).await?;
    let user_id = ctx.s.user_id;
    let (mut tx, _) = begin(&api, user_id, site_id).await?;
    sqlx::query(
        "UPDATE weekly_report_subscriptions SET email = NULL, code_hash = NULL, attempts = 0,
         confirmed_at = NULL WHERE user_id = $1 AND site_id = $2",
    )
    .bind(user_id)
    .bind(site_id)
    .execute(&mut *tx)
    .await
    .map_err(anyhow::Error::from)?;
    tx.commit().await.map_err(anyhow::Error::from)?;
    let mut response = StatusCode::NO_CONTENT.into_response();
    no_store(&mut response);
    Ok(api.finish(response, &ctx))
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

async fn unsubscribe_page(Path(token): Path<String>) -> Res<Response> {
    if !session::is_well_formed(&token) {
        return Err(DashError::NotFound);
    }
    Ok(unsubscribe_response(
        r#"<!doctype html><html lang="en"><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1"><title>Unsubscribe · Rivu</title>
        <h1>Unsubscribe from Rivu weekly reports</h1>
        <p lang="zh">退订 Rivu 周报</p>
        <form method="post"><button>Unsubscribe · <span lang="zh">退订</span></button></form></html>"#,
    ))
}

async fn unsubscribe_by_token(
    State(api): State<Arc<DashApi>>,
    Path(token): Path<String>,
) -> Res<Response> {
    if !session::is_well_formed(&token) {
        return Err(DashError::NotFound);
    }
    let updated = sqlx::query(
        "UPDATE weekly_report_subscriptions w
         SET email = NULL, code_hash = NULL, attempts = 0, confirmed_at = NULL
         FROM weekly_report_claims c
         WHERE c.unsubscribe_hash = $1 AND w.user_id = c.user_id AND w.site_id = c.site_id
           AND w.confirmed_at = c.confirmed_at",
    )
    .bind(session::hash_token(&token).as_slice())
    .execute(&api.pg)
    .await
    .map_err(anyhow::Error::from)?
    .rows_affected();
    if updated == 0 {
        return Ok(unsubscribe_response(
            r#"<!doctype html><html lang="en"><meta charset="utf-8"><title>Rivu weekly reports</title>
            <p>This link has expired or the subscription is already off.</p><p lang="zh">退订链接已过期，或订阅已关闭。</p></html>"#,
        ));
    }
    Ok(unsubscribe_response(
        r#"<!doctype html><html lang="en"><meta charset="utf-8"><title>Rivu weekly reports</title>
        <p>You have unsubscribed from Rivu weekly reports for this site.</p><p lang="zh">你已退订此站点的 Rivu 周报。</p></html>"#,
    ))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Period {
    from: Date,
    to: Date,
    start: Timestamp,
    end: Timestamp,
}

fn due_period(
    now: Timestamp,
    tz: &TimeZone,
    week_start: Weekday,
) -> anyhow::Result<Option<Period>> {
    let today = now.to_zoned(tz.clone()).date();
    let offset = (today.weekday().to_sunday_zero_offset() - week_start.to_sunday_zero_offset())
        .rem_euclid(7);
    let end_day = today.checked_sub(offset.days())?;
    if now < end_day.at(9, 0, 0, 0).to_zoned(tz.clone())?.timestamp() {
        return Ok(None);
    }
    let from = end_day.checked_sub(7.days())?;
    Ok(Some(Period {
        from,
        to: end_day.yesterday()?,
        start: from.to_zoned(tz.clone())?.timestamp(),
        end: end_day.to_zoned(tz.clone())?.timestamp(),
    }))
}

struct Site {
    id: i64,
    key: String,
    name: String,
    timezone: String,
    week_starts_on: i16,
}

impl Site {
    fn due(&self, now: Timestamp) -> anyhow::Result<Option<Period>> {
        let tz = TimeZone::get(&self.timezone)?;
        let week_start = Weekday::from_sunday_zero_offset(i8::try_from(self.week_starts_on)?)?;
        due_period(now, &tz, week_start)
    }
}

async fn candidates(pg: &PgPool, site_id: i64, period: Period) -> anyhow::Result<Vec<i64>> {
    Ok(sqlx::query_scalar(
        "SELECT w.user_id FROM weekly_report_subscriptions w
         JOIN users u ON u.id = w.user_id AND u.disabled_at IS NULL AND u.deleted_at IS NULL
         JOIN sites s ON s.id = w.site_id
         WHERE w.site_id = $1 AND w.email = u.email::text
           AND w.confirmed_at < to_timestamp($2)
           AND (EXISTS (SELECT FROM memberships m WHERE m.organization_id = s.organization_id
                        AND m.user_id = w.user_id AND m.deleted_at IS NULL)
                OR EXISTS (SELECT FROM site_memberships m WHERE m.site_id = w.site_id
                           AND m.user_id = w.user_id AND m.deleted_at IS NULL))
           AND NOT EXISTS (SELECT FROM weekly_report_claims c WHERE c.user_id = w.user_id
                           AND c.site_id = w.site_id AND c.period_end > to_timestamp($3))
         ORDER BY w.user_id",
    )
    .bind(site_id)
    .bind(period.end.as_second() as f64)
    .bind(period.start.as_second() as f64)
    .fetch_all(pg)
    .await?)
}

async fn claim(
    api: &DashApi,
    site: &Site,
    user_id: i64,
    period: Period,
    unsubscribe_hash: &[u8; 32],
) -> anyhow::Result<Option<(String, String)>> {
    let mut tx = api.pg.begin().await?;
    let Some(recipient) = lock_recipient(&mut tx, user_id, site.id).await? else {
        return Ok(None);
    };
    if recipient.timezone != site.timezone
        || recipient.week_starts_on != site.week_starts_on
        || site.due((api.clock)())? != Some(period)
    {
        return Ok(None);
    }
    // A previous end after this start fences overlap after any calendar change.
    let lang = sqlx::query_scalar(
        "WITH eligible AS (
             SELECT lang, confirmed_at FROM weekly_report_subscriptions
             WHERE user_id = $1 AND site_id = $2 AND email = $3
               AND confirmed_at < to_timestamp($4)
               AND NOT EXISTS (SELECT FROM weekly_report_claims
                   WHERE user_id = $1 AND site_id = $2 AND period_end > to_timestamp($5))
         ), claimed AS (
             INSERT INTO weekly_report_claims (user_id, site_id, period_end, confirmed_at, unsubscribe_hash)
             SELECT $1, $2, to_timestamp($4), confirmed_at, $6 FROM eligible
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
    .fetch_optional(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(lang.map(|lang| (recipient.email, lang)))
}

fn totals(result: &QueryResult) -> anyhow::Result<(u64, u64, u64)> {
    anyhow::ensure!(
        result.meta.warnings.is_empty(),
        "weekly query warnings: {:?}",
        result.meta.warnings
    );
    match result.results.as_slice() {
        [row] => match row.metrics.as_slice() {
            [
                MetricValue::Int(visitors),
                MetricValue::Int(visits),
                MetricValue::Int(pageviews),
            ] => Ok((*visitors, *visits, *pageviews)),
            _ => anyhow::bail!("weekly query did not return three integer totals"),
        },
        _ => anyhow::bail!("weekly query did not return a totals row"),
    }
}

async fn deliver(
    api: &DashApi,
    mailer: &rivu_mail::Mailer,
    site: &Site,
    now: Timestamp,
) -> anyhow::Result<()> {
    let Some(period) = site.due(now)? else {
        return Ok(());
    };
    let candidates = candidates(&api.pg, site.id, period).await?;
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
        || i16::from(context.week_starts_on.to_sunday_zero_offset()) != site.week_starts_on
    {
        return Ok(());
    }
    let spec = QuerySpec::parse(&json!({
        "metrics": ["visitors", "visits", "pageviews"],
        "date_range": [period.from.to_string(), period.to.to_string()],
    }))?;
    let mut options = ExecOptions::new(api.dashboard_timeout_secs)
        .with_now(now)
        .with_route("weekly_report");
    options.rollup_min_rows = api.rollup_min_rows;
    let result =
        execute_cached(&api.reader, None, &context, spec, options, "weekly_report").await?;
    let (visitors, visits, pageviews) = totals(&result)?;
    let base = api.base_url.trim_end_matches('/');
    let link = format!("{base}/s/{}", site.id);
    let (from, to) = (period.from, period.to);
    let name = &site.name;
    for user_id in candidates {
        let token = session::generate();
        let (email, lang) = match claim(api, site, user_id, period, &token.hash()).await {
            Ok(Some(recipient)) => recipient,
            Ok(None) => continue,
            Err(error) => {
                tracing::warn!(site_id = site.id, user_id, %error, "weekly report claim failed");
                continue;
            }
        };
        let unsubscribe_url = format!("{base}{UNSUBSCRIBE_PATH}/{}", token.plaintext());
        let (subject, body) = if lang == "zh" {
            (
                "Rivu 周报",
                format!(
                    "{name}\n{from} 至 {to}（{}）\n\n访客：{visitors}\n访问：{visits}\n浏览量：{pageviews}\n\n查看仪表盘：{link}\n退订周报：{unsubscribe_url}\n",
                    site.timezone
                ),
            )
        } else {
            (
                "Rivu weekly report",
                format!(
                    "{name}\n{from} to {to} ({})\n\nVisitors: {visitors}\nVisits: {visits}\nPageviews: {pageviews}\n\nView dashboard: {link}\nUnsubscribe: {unsubscribe_url}\n",
                    site.timezone
                ),
            )
        };
        if mailer
            .send(Mail {
                to: email,
                subject: subject.into(),
                body,
                unsubscribe_url: Some(unsubscribe_url),
            })
            .await
            .is_err()
        {
            tracing::warn!(
                site_id = site.id,
                user_id,
                "weekly report SMTP attempt failed"
            );
        }
    }
    Ok(())
}

/// Evaluate only the latest completed local week, grouped by site.
pub async fn run_once(api: &DashApi) -> anyhow::Result<()> {
    let Some(mailer) = &api.mailer else {
        return Ok(());
    };
    let now = (api.clock)();
    let sites = sqlx::query_as::<_, (i64, String, String, String, i16)>(
        "SELECT s.id, s.site_key, s.name, s.timezone, COALESCE(ss.week_starts_on, 1)::smallint
         FROM sites s JOIN organizations o ON o.id = s.organization_id AND o.deleted_at IS NULL
         LEFT JOIN site_settings ss ON ss.site_id = s.id
         WHERE s.deleted_at IS NULL AND EXISTS (
             SELECT FROM weekly_report_subscriptions w WHERE w.site_id = s.id AND w.confirmed_at IS NOT NULL)
         ORDER BY s.id",
    )
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
        if let Err(error) = deliver(api, mailer, &site, now).await {
            tracing::warn!(site_id = id, %error, "weekly report skipped");
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
                        tracing::warn!(%error, "weekly report tick failed");
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
}
