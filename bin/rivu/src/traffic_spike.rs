//! Traffic spike alerts: an email when a site's current visitors reach the threshold set in its
//! settings (`site_settings.spike_threshold`).
//!
//! Recipients switch the alert on per site on the account page, like the weekly report, and use the
//! same confirmed mailbox ([`crate::weekly_report`], subscription `kind = 'spike'`). Once a minute
//! one ClickHouse query counts the current visitors (distinct visitors in the last 5 minutes, the
//! dashboard's `realtime` number) of every site that has a threshold and at least one recipient.
//!
//! A row in `traffic_spikes` means the site is in a spike. It starts the first time the count
//! reaches the threshold and ends once the count has stayed below it for [`COOLDOWN`]. Each
//! recipient is claimed once per spike (`weekly_report_claims`, `period_end` = spike start), so one
//! spike sends one email per recipient, also across restarts and several processes.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use clickhouse::Row;
use jiff::tz::TimeZone;
use jiff::{SignedDuration, Timestamp};
use rivu_auth::session;
use rivu_mail::weekly_report::{Spike, spike};
use serde::Deserialize;
use sqlx::PgPool;
use tokio::sync::watch;

use crate::dash_api::DashApi;
use crate::weekly_report::{Kind, lock_recipient};

/// How long the current visitors must stay below the threshold before a spike is over. A new
/// spike, and with it a new email, can only start after that.
pub const COOLDOWN: SignedDuration = SignedDuration::from_mins(30);
/// The `realtime` window: distinct visitors in the last 5 minutes.
const WINDOW: SignedDuration = SignedDuration::from_mins(5);

/// An active spike.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct State {
    pub(crate) started_at: Timestamp,
    /// The first check below the threshold since the count last reached it.
    pub(crate) below_since: Option<Timestamp>,
}

/// What one check does to a site's spike.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Step {
    /// No spike, and none starts.
    Idle,
    /// A new spike: alert every recipient.
    Start(State),
    /// Still at or above the threshold: alert recipients who have not had this spike's email yet.
    Above(State),
    /// Below the threshold, cooldown still running: no email.
    Cooling(State),
    /// Below the threshold for the whole cooldown: the spike is over.
    End,
}

/// The state machine, with the time passed in.
pub(crate) fn step(
    state: Option<State>,
    visitors: u64,
    threshold: u64,
    now: Timestamp,
    cooldown: SignedDuration,
) -> Step {
    match (state, visitors >= threshold) {
        (None, false) => Step::Idle,
        (None, true) => Step::Start(State {
            started_at: now,
            below_since: None,
        }),
        (Some(s), true) => Step::Above(State {
            below_since: None,
            ..s
        }),
        (Some(s), false) => {
            let since = s.below_since.unwrap_or(now);
            if now.duration_since(since) >= cooldown {
                Step::End
            } else {
                Step::Cooling(State {
                    below_since: Some(since),
                    ..s
                })
            }
        }
    }
}

struct Site {
    id: i64,
    name: String,
    timezone: String,
    threshold: u64,
}

#[derive(Row, Deserialize)]
struct Current {
    site_id: u64,
    visitors: u64,
}

/// Sites that are watched: a threshold, alive, and someone switched the alert on. Spike state of
/// any other site is dropped first, so turning the threshold off or the last switch off ends it.
async fn watched_sites(pg: &PgPool) -> anyhow::Result<Vec<Site>> {
    const WATCHED: &str = "FROM sites s
         JOIN organizations o ON o.id = s.organization_id AND o.deleted_at IS NULL
         JOIN site_settings ss ON ss.site_id = s.id
         WHERE s.deleted_at IS NULL AND ss.spike_threshold IS NOT NULL
           AND EXISTS (SELECT FROM weekly_report_subscriptions w
                       WHERE w.site_id = s.id AND w.kind = 'spike')";
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "DELETE FROM traffic_spikes t WHERE NOT EXISTS (SELECT {WATCHED} AND s.id = t.site_id)"
    )))
    .execute(pg)
    .await?;
    let rows = sqlx::query_as::<_, (i64, String, String, i32)>(sqlx::AssertSqlSafe(format!(
        "SELECT s.id, s.name, s.timezone, ss.spike_threshold {WATCHED} ORDER BY s.id"
    )))
    .fetch_all(pg)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(id, name, timezone, threshold)| Site {
            id,
            name,
            timezone,
            threshold: u64::try_from(threshold).unwrap_or(u64::MAX),
        })
        .collect())
}

/// One query for every watched site: distinct visitors in `[now − 5 min, now]`, the same filter
/// as the dashboard's `realtime` query (non-engagement events; the date bounds prune by the
/// primary key `(site_id, toDate(ts), …)`). Sites without events are missing from the result.
async fn current_visitors(
    api: &DashApi,
    sites: &[u64],
    now: Timestamp,
) -> anyhow::Result<HashMap<u64, u64>> {
    let from = now - WINDOW;
    let date = |t: Timestamp| t.to_zoned(TimeZone::UTC).date().to_string();
    let rows = api
        .reader
        .query(
            "SELECT site_id, uniq(visitor_id) AS visitors
             FROM events
             WHERE site_id IN {sites:Array(UInt64)}
               AND toDate(ts) >= {from_date:Date} AND toDate(ts) <= {to_date:Date}
               AND ts >= fromUnixTimestamp64Milli({from_ms:Int64}, 'UTC')
               AND ts < fromUnixTimestamp64Milli({to_ms:Int64}, 'UTC')
               AND name != 'engagement'
             GROUP BY site_id",
        )
        .param("sites", sites)
        .param("from_date", date(from).as_str())
        .param("to_date", date(now).as_str())
        .param("from_ms", from.as_millisecond())
        .param("to_ms", now.as_millisecond() + 1)
        .with_setting("max_execution_time", api.dashboard_timeout_secs.to_string())
        .with_setting("log_comment", "traffic_spike")
        .fetch_all::<Current>()
        .await?;
    Ok(rows.into_iter().map(|r| (r.site_id, r.visitors)).collect())
}

/// Spike times are whole seconds, stored and compared as `to_timestamp(seconds)`.
fn secs(t: Timestamp) -> i64 {
    t.as_second()
}

fn from_secs(s: i64) -> anyhow::Result<Timestamp> {
    Ok(Timestamp::from_second(s)?)
}

/// Applies one check under the spike row's lock and returns the active spike when it is at or
/// above the threshold (the only time recipients get mail).
async fn advance(
    pg: &PgPool,
    site: &Site,
    visitors: u64,
    now: Timestamp,
) -> anyhow::Result<Option<State>> {
    let mut tx = pg.begin().await?;
    let state = sqlx::query_as::<_, (i64, Option<i64>)>(
        "SELECT extract(epoch FROM started_at)::bigint, extract(epoch FROM below_since)::bigint
         FROM traffic_spikes WHERE site_id = $1 FOR UPDATE",
    )
    .bind(site.id)
    .fetch_optional(&mut *tx)
    .await?
    .map(|(started, below)| -> anyhow::Result<State> {
        Ok(State {
            started_at: from_secs(started)?,
            below_since: below.map(from_secs).transpose()?,
        })
    })
    .transpose()?;
    let alert = match step(state, visitors, site.threshold, now, COOLDOWN) {
        Step::Idle => None,
        Step::Start(next) => {
            // Another process may have started the same spike first; it sends the emails.
            let inserted = sqlx::query(
                "INSERT INTO traffic_spikes (site_id, started_at) VALUES ($1, to_timestamp($2))
                 ON CONFLICT (site_id) DO NOTHING",
            )
            .bind(site.id)
            .bind(secs(next.started_at))
            .execute(&mut *tx)
            .await?
            .rows_affected();
            (inserted == 1).then_some(next)
        }
        Step::Above(next) | Step::Cooling(next) => {
            sqlx::query(
                "UPDATE traffic_spikes SET below_since = to_timestamp($2) WHERE site_id = $1",
            )
            .bind(site.id)
            .bind(next.below_since.map(secs))
            .execute(&mut *tx)
            .await?;
            next.below_since.is_none().then_some(next)
        }
        Step::End => {
            sqlx::query("DELETE FROM traffic_spikes WHERE site_id = $1")
                .bind(site.id)
                .execute(&mut *tx)
                .await?;
            None
        }
    };
    tx.commit().await?;
    Ok(alert)
}

/// Recipients of this spike who have not been claimed yet. Same rules as the weekly report: an
/// active account, the confirmed current mailbox, read access to the site, and a switch that was
/// on before the spike started.
async fn candidates(pg: &PgPool, site_id: i64, started_at: Timestamp) -> anyhow::Result<Vec<i64>> {
    Ok(sqlx::query_scalar(
        "SELECT w.user_id FROM weekly_report_subscriptions w
         JOIN users u ON u.id = w.user_id AND u.disabled_at IS NULL AND u.deleted_at IS NULL
         JOIN sites s ON s.id = w.site_id
         JOIN weekly_report_emails e ON e.user_id = w.user_id AND e.email = w.email
                                     AND e.confirmed_at IS NOT NULL
         WHERE w.site_id = $1 AND w.kind = 'spike' AND w.email = u.email::text
           AND w.confirmed_at < to_timestamp($2)
           AND (EXISTS (SELECT FROM memberships m WHERE m.organization_id = s.organization_id
                        AND m.user_id = w.user_id AND m.deleted_at IS NULL)
                OR EXISTS (SELECT FROM site_memberships m WHERE m.site_id = w.site_id
                           AND m.user_id = w.user_id AND m.deleted_at IS NULL))
           AND NOT EXISTS (SELECT FROM weekly_report_claims c WHERE c.user_id = w.user_id
                           AND c.site_id = w.site_id AND c.kind = 'spike'
                           AND c.period_end = to_timestamp($2))
         ORDER BY w.user_id",
    )
    .bind(site_id)
    .bind(secs(started_at))
    .fetch_all(pg)
    .await?)
}

/// Rechecks the recipient under the weekly report's locks and claims this spike for them.
async fn claim(
    pg: &PgPool,
    site: &Site,
    user_id: i64,
    started_at: Timestamp,
    unsubscribe_hash: &[u8; 32],
) -> anyhow::Result<Option<(String, String)>> {
    let mut tx = pg.begin().await?;
    let Some(recipient) = lock_recipient(&mut tx, user_id, site.id).await? else {
        return Ok(None);
    };
    let lang = sqlx::query_scalar(
        "WITH eligible AS (
             SELECT lang, confirmed_at FROM weekly_report_subscriptions
             WHERE user_id = $1 AND site_id = $2 AND kind = 'spike' AND email = $3
               AND confirmed_at < to_timestamp($4)
               AND EXISTS (SELECT FROM weekly_report_emails
                   WHERE user_id = $1 AND email = $3 AND confirmed_at IS NOT NULL)
               AND EXISTS (SELECT FROM traffic_spikes
                   WHERE site_id = $2 AND started_at = to_timestamp($4))
         ), claimed AS (
             INSERT INTO weekly_report_claims
                 (user_id, site_id, kind, period_end, confirmed_at, unsubscribe_hash)
             SELECT $1, $2, 'spike', to_timestamp($4), confirmed_at, $5 FROM eligible
             ON CONFLICT DO NOTHING RETURNING user_id
         )
         SELECT lang FROM eligible CROSS JOIN claimed",
    )
    .bind(user_id)
    .bind(site.id)
    .bind(&recipient.email)
    .bind(secs(started_at))
    .bind(unsubscribe_hash.as_slice())
    .fetch_optional(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(lang.map(|lang: String| (recipient.email, lang)))
}

async fn alert(
    api: &DashApi,
    mailer: &rivu_mail::Mailer,
    site: &Site,
    spike_state: State,
    visitors: u64,
    now: Timestamp,
) -> anyhow::Result<()> {
    let candidates = candidates(&api.pg, site.id, spike_state.started_at).await?;
    if candidates.is_empty() {
        return Ok(());
    }
    let local = now.to_zoned(TimeZone::get(&site.timezone)?).datetime();
    let base = api.base_url.trim_end_matches('/');
    for user_id in candidates {
        let token = session::generate();
        let claimed = claim(
            &api.pg,
            site,
            user_id,
            spike_state.started_at,
            &token.hash(),
        )
        .await;
        let (email, lang) = match claimed {
            Ok(Some(recipient)) => recipient,
            Ok(None) => continue,
            Err(error) => {
                tracing::warn!(site_id = site.id, user_id, %error, "traffic spike claim failed");
                continue;
            }
        };
        let unsubscribe_url = format!(
            "{base}{}/{}",
            Kind::Spike.unsubscribe_path(),
            token.plaintext()
        );
        let mail = spike(&Spike {
            lang: if lang == "zh" {
                rivu_mail::Lang::Zh
            } else {
                rivu_mail::Lang::En
            },
            to: &email,
            base_url: base,
            site_id: site.id,
            site_name: &site.name,
            timezone: &site.timezone,
            at: local,
            visitors,
            threshold: site.threshold,
            unsubscribe_url: &unsubscribe_url,
        });
        if mailer.send(mail).await.is_err() {
            tracing::warn!(
                site_id = site.id,
                user_id,
                "traffic spike SMTP attempt failed"
            );
        }
    }
    Ok(())
}

/// One check of every watched site.
pub async fn run_once(api: &DashApi) -> anyhow::Result<()> {
    let Some(mailer) = &api.mailer else {
        return Ok(());
    };
    // Whole seconds, so the spike start round-trips through PostgreSQL exactly.
    let now = Timestamp::from_second((api.clock)().as_second())?;
    let sites = watched_sites(&api.pg).await?;
    if sites.is_empty() {
        return Ok(());
    }
    let ids = sites
        .iter()
        .map(|s| u64::try_from(s.id))
        .collect::<Result<Vec<_>, _>>()?;
    // A failed count changes no state: a spike neither starts nor cools down on missing data.
    let counts = current_visitors(api, &ids, now).await?;
    for (site, id) in sites.iter().zip(&ids) {
        let visitors = counts.get(id).copied().unwrap_or(0);
        match advance(&api.pg, site, visitors, now).await {
            Ok(Some(state)) => {
                if let Err(error) = alert(api, mailer, site, state, visitors, now).await {
                    tracing::warn!(site_id = site.id, %error, "traffic spike alert skipped");
                }
            }
            Ok(None) => {}
            Err(error) => tracing::warn!(site_id = site.id, %error, "traffic spike check failed"),
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
                        tracing::warn!(%error, "traffic spike tick failed");
                    }
                } => {}
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(minute: i64) -> Timestamp {
        Timestamp::from_second(1_795_000_000 + minute * 60).unwrap()
    }

    /// Runs the state machine over one count per minute and returns the minutes that alert.
    fn alerts(counts: &[u64], threshold: u64) -> Vec<i64> {
        let mut state = None;
        let mut out = Vec::new();
        for (minute, &visitors) in (0_i64..).zip(counts) {
            match step(state, visitors, threshold, at(minute), COOLDOWN) {
                Step::Idle => {}
                Step::Start(s) => {
                    out.push(minute);
                    state = Some(s);
                }
                Step::Above(s) | Step::Cooling(s) => state = Some(s),
                Step::End => state = None,
            }
        }
        out
    }

    #[test]
    fn one_spike_starts_once_and_keeps_its_start() {
        let s = step(None, 132, 100, at(0), COOLDOWN);
        let Step::Start(state) = s else {
            panic!("{s:?}")
        };
        assert_eq!(state.started_at, at(0));
        assert_eq!(step(None, 99, 100, at(0), COOLDOWN), Step::Idle);
        // Exactly at the threshold counts as reached.
        assert!(matches!(
            step(None, 100, 100, at(0), COOLDOWN),
            Step::Start(_)
        ));
        assert_eq!(
            step(Some(state), 500, 100, at(5), COOLDOWN),
            Step::Above(state)
        );
    }

    #[test]
    fn the_cooldown_runs_from_the_first_check_below_and_resets_above() {
        let start = State {
            started_at: at(0),
            below_since: None,
        };
        let cooling = step(Some(start), 10, 100, at(3), COOLDOWN);
        let below = State {
            below_since: Some(at(3)),
            ..start
        };
        assert_eq!(cooling, Step::Cooling(below));
        assert_eq!(
            step(Some(below), 10, 100, at(32), COOLDOWN),
            Step::Cooling(below)
        );
        assert_eq!(step(Some(below), 10, 100, at(33), COOLDOWN), Step::End);
        // Back above the threshold: the spike continues and the cooldown starts over.
        assert_eq!(
            step(Some(below), 150, 100, at(20), COOLDOWN),
            Step::Above(start)
        );
    }

    #[test]
    fn a_wobbling_count_sends_one_alert_per_spike() {
        let mut counts = vec![50, 120, 90, 130, 80];
        counts.extend([80; 28]); // Minutes 5 to 32: below, but within 30 minutes of minute 4.
        counts.push(150); // Minute 33: still the same spike.
        counts.extend([0; 31]); // Minutes 34 to 64: the cooldown runs out at minute 64.
        counts.push(200); // Minute 65: a new spike.
        assert_eq!(alerts(&counts, 100), [1, 65]);
        // A cooldown that has not quite run out keeps the spike going.
        let mut almost = vec![120];
        almost.extend([0; 30]); // Minutes 1 to 30; minute 31 would end it.
        almost.push(120);
        assert_eq!(alerts(&almost, 100), [0]);
    }
}
