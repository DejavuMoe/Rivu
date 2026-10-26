//! 接入诊断端点：`GET /api/internal/sites/{site}/diagnostics?range=1h|24h|7d`（`platform-and-ops.md` §4.1）。
//!
//! 数据来自 ClickHouse 的 `ingest_counters`（`system-design.md` §5.4）：进程内 10 秒一个桶、只刷出已结束的桶，
//! 落库时取整到分钟。所以最后一个时间桶总是进行中的（`partial_last`），且「最后收到事件」只精确到分钟。
//!
//! # 权限
//!
//! 与站点设置的读取相同（`SiteSettingsRead`）：站点对调用者不可见 404，可见但有效角色不是 editor+ 为 403。
//!
//! # 时间桶
//!
//! 按 UTC 对齐：`1h` 是 60 个 1 分钟桶，`24h` 是 24 个 1 小时桶，`7d` 是 28 个 6 小时桶；最后一个桶包含「现在」，
//! 没有数据的桶补 0。丢弃原因是指标名去掉 `dropped_` 前缀；`dropped_unknown_site` 记在 `site_id = 0`，
//! 不属于任何站点，这里也显式排除。脚本版本只有数字（`sv`，超过 65535 的钳到 65535；0 表示请求没带版本）。

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::Router;
use axum::extract::{Path, Request, State};
use axum::http::StatusCode;
use axum::response::Response;
use axum::routing::get;
use jiff::Timestamp;
use rivu_auth::scope::{Action, allowed};
use rivu_query::QueryError;
use rivu_storage_ch::diagnostics::{self, CounterGroup};
use rivu_storage_pg::access::{self, SiteRef};
use serde_json::{Value, json};

use crate::dash_api::{DashApi, DashError, Res, effective_role, endpoint, json_response};
use crate::query_api::Fail;
use crate::site_admin::only;

/// 「最后收到事件」在时间范围内一个都没有时回溯的天数（与 `ingest_counters` 的 TTL 一致）。
const LAST_EVENT_LOOKBACK_SECS: i64 = 90 * 86_400;

/// 时间范围与它的桶。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Range {
    Hour,
    Day,
    Week,
}

impl Range {
    fn parse(s: Option<&str>) -> Option<Range> {
        match s {
            None | Some("24h") => Some(Range::Day),
            Some("1h") => Some(Range::Hour),
            Some("7d") => Some(Range::Week),
            Some(_) => None,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Range::Hour => "1h",
            Range::Day => "24h",
            Range::Week => "7d",
        }
    }

    /// `(桶数, 桶宽秒)`。
    fn spec(self) -> (i64, i64) {
        match self {
            Range::Hour => (60, 60),
            Range::Day => (24, 3600),
            Range::Week => (28, 6 * 3600),
        }
    }

    /// 第一个桶的起点（含）与最后一个桶的终点（不含）。
    pub(crate) fn window(self, now_secs: i64) -> (i64, i64) {
        let (n, step) = self.spec();
        let last = now_secs.div_euclid(step) * step;
        (last - (n - 1) * step, last + step)
    }
}

fn iso(secs: i64) -> String {
    Timestamp::from_second(secs)
        .map(|t| t.to_string())
        .unwrap_or_default()
}

/// 把 ClickHouse 返回的分组装配成响应体（纯函数，便于测试）。
pub(crate) fn assemble(
    range: Range,
    now_secs: i64,
    groups: &[CounterGroup],
    last_event: Option<i64>,
) -> Value {
    let (n, step) = range.spec();
    let (from, to) = range.window(now_secs);
    // 桶起点 → (accepted, dropped)
    let mut buckets: Vec<(i64, u64, u64)> = (0..n).map(|i| (from + i * step, 0, 0)).collect();
    let mut reasons: BTreeMap<&str, u64> = BTreeMap::new();
    // 版本 → (events, last_seen)
    let mut versions: BTreeMap<u16, (u64, i64)> = BTreeMap::new();
    let (mut accepted, mut dropped) = (0u64, 0u64);
    let mut newest_accepted: Option<i64> = None;

    for g in groups {
        let is_accepted = g.metric_name == "accepted";
        let reason = g.metric_name.strip_prefix("dropped_");
        if !is_accepted && reason.is_none_or(|r| r == "unknown_site") {
            continue;
        }
        let bucket = i64::from(g.bucket);
        let Some(idx) = usize::try_from((bucket - from) / step)
            .ok()
            .filter(|_| bucket >= from && bucket < to)
        else {
            continue;
        };
        if is_accepted {
            buckets[idx].1 += g.total;
            accepted += g.total;
            if g.total > 0 {
                newest_accepted = newest_accepted.max(Some(i64::from(g.last_minute)));
            }
        } else if let Some(r) = reason {
            buckets[idx].2 += g.total;
            dropped += g.total;
            *reasons.entry(r).or_default() += g.total;
        }
        let v = versions.entry(g.script_version).or_insert((0, 0));
        v.0 += g.total;
        v.1 = v.1.max(i64::from(g.last_minute));
    }

    let mut reasons: Vec<(&str, u64)> = reasons.into_iter().filter(|(_, c)| *c > 0).collect();
    reasons.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
    let last_event = last_event.or(newest_accepted);

    json!({
        "range": range.as_str(),
        "step_seconds": step,
        "from": iso(from),
        "to": iso(to),
        "buckets": buckets.iter()
            .map(|(t, a, d)| json!({"t": iso(*t), "accepted": a, "dropped": d}))
            .collect::<Vec<_>>(),
        "totals": {"accepted": accepted, "dropped": dropped},
        "reasons": reasons.iter()
            .map(|(r, c)| json!({"reason": r, "count": c}))
            .collect::<Vec<_>>(),
        "versions": versions.iter().rev()
            .filter(|(_, (events, _))| *events > 0)
            .map(|(v, (events, last))| json!({"version": v, "events": events, "last_seen": iso(*last)}))
            .collect::<Vec<_>>(),
        "last_event_at": last_event.map(iso),
        // 最后一个桶包含「现在」，计数又只刷出已结束的 10 秒桶：它总是不完整的
        "partial_last": now_secs < to,
    })
}

fn map_ch_error(e: clickhouse::error::Error, timeout: u64) -> DashError {
    use clickhouse::error::Error;
    let query_err = match &e {
        Error::TimedOut => QueryError::Timeout,
        Error::BadResponse(text)
            if ["Code: 159", "Code: 209", "Code: 160"]
                .iter()
                .any(|c| text.contains(c)) =>
        {
            QueryError::Timeout
        }
        Error::BadResponse(text)
            if [
                "Code: 241",
                "Code: 202",
                "Code: 158",
                "Code: 307",
                "Code: 396",
                "Code: 201",
            ]
            .iter()
            .any(|c| text.contains(c)) =>
        {
            QueryError::ResourceLimit("the database rejected the query".into())
        }
        _ => QueryError::internal(anyhow::anyhow!("接入诊断的 ClickHouse 查询失败: {e}")),
    };
    metrics::counter!("rivu_query_errors_total", "route" => "internal.diagnostics",
        "kind" => crate::query_api::error_kind(&query_err))
    .increment(1);
    DashError::Fail(Fail::Query(query_err), timeout)
}

/// 查询串里的 `range`（只认这一个参数，值只有 ASCII 字母数字，不需要解码）。
fn range_param(req: &Request) -> Option<&str> {
    req.uri()
        .query()?
        .split('&')
        .find_map(|kv| kv.strip_prefix("range="))
}

async fn get_diagnostics(
    State(api): State<Arc<DashApi>>,
    Path(site_seg): Path<String>,
    req: Request,
) -> Res<Response> {
    let ctx = api.require_session(req.headers()).await?;
    let site_ref = SiteRef::parse(&site_seg).ok_or(DashError::NotFound)?;
    let site = access::find_site(&api.pg, &site_ref)
        .await?
        .ok_or(DashError::NotFound)?;
    let roles = access::user_site_roles(&api.pg, ctx.s.user_id, &site).await?;
    let role = effective_role(&roles).ok_or(DashError::NotFound)?;
    if !allowed(role, Action::SiteSettingsRead) {
        return Err(DashError::Forbidden);
    }
    let range = Range::parse(range_param(&req))
        .ok_or_else(|| DashError::Invalid("range must be one of `1h`, `24h` or `7d`".into()))?;

    let timeout = api.dashboard_timeout_secs;
    let now = (api.clock)().as_second();
    let (from, to) = range.window(now);
    let (Ok(from32), Ok(to32)) = (u32::try_from(from), u32::try_from(to)) else {
        return Err(DashError::Internal);
    };
    let comment = json!({"route": "internal.diagnostics", "site": site.id, "tier": "dashboard",
                         "org": site.organization_id, "user": ctx.s.user_id})
    .to_string();
    let started = std::time::Instant::now();
    let (_, step) = range.spec();
    let groups = diagnostics::counter_groups(
        &api.reader,
        site.id,
        from32,
        to32,
        u32::try_from(step).unwrap_or(60),
        timeout,
        &comment,
    )
    .await
    .map_err(|e| map_ch_error(e, timeout))?;
    // 范围内没有 accepted：往前回溯一次（只有安静的站点才会走到这里）
    let last_event = if groups
        .iter()
        .any(|g| g.metric_name == "accepted" && g.total > 0)
    {
        None
    } else {
        let lookback = u32::try_from((now - LAST_EVENT_LOOKBACK_SECS).max(0)).unwrap_or(0);
        diagnostics::last_accepted_minute(&api.reader, site.id, lookback, timeout, &comment)
            .await
            .map_err(|e| map_ch_error(e, timeout))?
            .map(i64::from)
    };
    metrics::histogram!("rivu_query_seconds", "route" => "internal.diagnostics")
        .record(started.elapsed().as_secs_f64());
    let body = assemble(range, now, &groups, last_event);
    Ok(api.finish(json_response(StatusCode::OK, body), &ctx))
}

pub(crate) fn routes(api: &Arc<DashApi>) -> Router {
    Router::new().merge(endpoint(
        api,
        "internal.diagnostics",
        "/api/internal/sites/{site}/diagnostics",
        only(get(get_diagnostics), "GET, HEAD"),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn g(bucket: u32, metric: &str, v: u16, total: u64, last: u32) -> CounterGroup {
        CounterGroup {
            bucket,
            metric_name: metric.into(),
            script_version: v,
            total,
            last_minute: last,
        }
    }

    #[test]
    fn windows_align_to_utc_buckets() {
        // 2026-10-26T10:17:42Z
        let now = 1_792_996_662;
        let (from, to) = Range::Hour.window(now);
        assert_eq!(to - from, 3600);
        assert_eq!(to % 60, 0);
        assert!(from <= now && now < to);
        let (from, to) = Range::Day.window(now);
        assert_eq!((to - from) / 3600, 24);
        assert_eq!(to % 3600, 0);
        let (from, to) = Range::Week.window(now);
        assert_eq!((to - from) / (6 * 3600), 28);
        assert_eq!(to % (6 * 3600), 0);
        assert!(from <= now && now < to);
    }

    #[test]
    fn assembles_buckets_reasons_and_versions() {
        let now = 1_792_996_662;
        let (from, _) = Range::Hour.window(now);
        let b0 = u32::try_from(from).unwrap();
        let groups = [
            g(b0, "accepted", 7, 5, b0),
            g(b0 + 60, "accepted", 7, 3, b0 + 60),
            g(b0 + 60, "dropped_bot", 7, 2, b0 + 60),
            g(b0 + 60, "dropped_hostname_not_allowed", 8, 4, b0 + 60),
            g(b0 + 60, "dropped_unknown_site", 8, 100, b0 + 60),
            // 窗口之外
            g(b0 - 60, "accepted", 7, 999, b0 - 60),
        ];
        let v = assemble(Range::Hour, now, &groups, None);
        assert_eq!(v["buckets"].as_array().unwrap().len(), 60);
        assert_eq!(v["buckets"][0]["accepted"], 5);
        assert_eq!(v["buckets"][1]["accepted"], 3);
        assert_eq!(v["buckets"][1]["dropped"], 6);
        assert_eq!(v["totals"]["accepted"], 8);
        assert_eq!(v["totals"]["dropped"], 6);
        assert_eq!(v["reasons"][0]["reason"], "hostname_not_allowed");
        assert_eq!(v["reasons"][0]["count"], 4);
        assert_eq!(v["reasons"].as_array().unwrap().len(), 2);
        assert_eq!(v["versions"][0]["version"], 8);
        assert_eq!(v["versions"][0]["events"], 4);
        assert_eq!(v["versions"][1]["events"], 10);
        assert_eq!(v["last_event_at"], iso(i64::from(b0) + 60));
        assert_eq!(v["partial_last"], true);
    }

    #[test]
    fn rejects_unknown_ranges() {
        assert_eq!(Range::parse(None), Some(Range::Day));
        assert_eq!(Range::parse(Some("1h")), Some(Range::Hour));
        assert_eq!(Range::parse(Some("30d")), None);
    }
}
