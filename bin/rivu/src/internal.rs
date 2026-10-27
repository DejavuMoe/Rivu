//! 内部端口（`RIVU_INTERNAL_ADDR`）：`/healthz`、`/readyz`、`/metrics`。不经反向代理对外暴露。

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::Router;
use axum::extract::State;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Json};
use axum::routing::get;
use metrics_exporter_prometheus::PrometheusHandle;
use rivu_common::build_info;
use rivu_common::config::Role;
use rivu_common::migration::{self, Store};
use rivu_geo::GeoDb;
use rivu_geo::datacenter::DcDb;
use rivu_ingest::pipeline::{SaltCache, SiteSnapshot, utc_today};
use rivu_jobs::GeoRefreshState;
use serde_json::{Value, json};

use crate::script::TrackerAssets;
use crate::session_warm::WarmStatus;

/// 单项检查的超时：依赖卡住时 `/readyz` 也要及时返回 503，而不是挂起。
const CHECK_TIMEOUT: Duration = Duration::from_secs(3);

/// `/readyz` 用到的依赖。
pub struct Probe {
    pub pg: sqlx::PgPool,
    pub ch: clickhouse::Client,
    pub ch_database: String,
    /// 编译期嵌入的各库最高迁移版本
    pub required_pg: u64,
    pub required_ch: u64,
}

impl Probe {
    pub fn new(
        pg: sqlx::PgPool,
        ch: clickhouse::Client,
        ch_database: String,
    ) -> anyhow::Result<Self> {
        let migrations = migration::embedded().map_err(|e| anyhow::anyhow!("{e:?}"))?;
        Ok(Probe {
            pg,
            ch,
            ch_database,
            required_pg: migration::highest_version(&migrations, Store::Postgres).unwrap_or(0),
            required_ch: migration::highest_version(&migrations, Store::ClickHouse).unwrap_or(0),
        })
    }
}

/// collector 角色的就绪条件：站点快照已加载、盐已加载（今天前后各一天）。
/// 会话预热在开始服务之前同步完成，`sessions` 只报告结果，不判失败。
#[derive(Clone)]
pub struct CollectorReadiness {
    pub sites: Arc<SiteSnapshot>,
    pub salts: Arc<SaltCache>,
    pub sessions: Arc<WarmStatus>,
}

/// `/readyz` 里 geoip 项的数据来源。**只报告状态，永远不判失败**：库可能还在后台下载
/// （City 版约 60 MB 压缩、127 MB 解压，远超预热预算），下载完成前事件的位置字段为空（`system-design.md` §4.4）。
pub struct GeoReadiness {
    /// 当前进程持有的库（collector / all）。
    pub db: Option<Arc<GeoDb>>,
    /// `geoip_refresh` 的状态（worker / all）。
    pub refresh: Option<Arc<GeoRefreshState>>,
    pub db_path: PathBuf,
}

pub struct InternalState {
    pub role: Role,
    pub probe: Probe,
    /// 只有 collector / all 有。
    pub collector: Option<CollectorReadiness>,
    /// tracker 构建产物（`/js/<site_key>.js` 的来源），只有 collector / all 有。**只报告，不判就绪失败**：
    /// 缺失时脚本路由返回 503，但事件采集不依赖它。
    pub tracker: Option<Arc<TrackerAssets>>,
    /// 看板静态文件，只有 api / all 有。**只报告，不判就绪失败**：缺失时页面返回 503，API 照常。
    pub dashboard: Option<Arc<crate::dashboard::DashboardAssets>>,
    pub geo: GeoReadiness,
    /// 数据中心网段表：只在开启过滤的 collector / all 有。**只报告，不判就绪失败**：未加载时不丢弃任何事件。
    pub datacenter: Option<Arc<DcDb>>,
    pub metrics: PrometheusHandle,
    pub started: Instant,
}

pub fn router(state: Arc<InternalState>) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .route("/metrics", get(metrics))
        .with_state(state)
}

/// 存活：进程能响应就是 200。
async fn healthz() -> &'static str {
    "ok"
}

async fn with_timeout<T>(fut: impl Future<Output = anyhow::Result<T>>) -> Result<T, String> {
    match tokio::time::timeout(CHECK_TIMEOUT, fut).await {
        Ok(Ok(v)) => Ok(v),
        Ok(Err(e)) => Err(format!("{e:#}")),
        Err(_) => Err(format!("超时（{}s）", CHECK_TIMEOUT.as_secs())),
    }
}

fn check_json(result: &Result<(), String>) -> Value {
    match result {
        Ok(()) => json!({ "ok": true }),
        Err(e) => json!({ "ok": false, "error": e }),
    }
}

/// 某个库的 schema 检查：已应用最高版本 >= 编译期最高版本。
fn schema_json(required: u64, applied: &Result<u64, String>) -> (bool, Value) {
    match applied {
        Ok(applied) => {
            let ok = *applied >= required;
            (
                ok,
                json!({ "ok": ok, "required": required, "applied": applied }),
            )
        }
        Err(e) => (
            false,
            json!({ "ok": false, "required": required, "error": e }),
        ),
    }
}

/// 站点快照至少加载成功过一次（站点数可以是 0）。
fn sites_check(sites: &SiteSnapshot) -> (bool, Value) {
    let ok = sites.is_loaded();
    (ok, json!({ "ok": ok, "loaded": ok, "sites": sites.len() }))
}

/// 盐缓存覆盖 UTC 的 `today-1 ..= today+1`：站点本地日期相对 UTC 日期在 ±1 天内，缺任何一天都会丢事件。
fn salts_check(salts: &SaltCache, today_utc: jiff::civil::Date) -> (bool, Value) {
    let ok = salts.covers(today_utc);
    (
        ok,
        json!({ "ok": ok, "loaded": salts.is_loaded(), "today": today_utc.to_string() }),
    )
}

/// GeoIP 状态：只报告，`ok` 恒为 true，不参与 `ready` 的计算。
fn geoip_check(geo: &GeoReadiness) -> Value {
    let mut v = json!({ "ok": true, "file_present": geo.db_path.is_file() });
    if let Some(db) = &geo.db {
        let s = db.status();
        v["loaded"] = json!(s.loaded);
        v["source"] = json!(s.source);
        v["database_type"] = json!(s.database_type);
        v["build_epoch"] = json!(s.build_epoch);
        v["age_seconds"] = json!(s.age_seconds);
    }
    if let Some(r) = &geo.refresh {
        let s = r.snapshot();
        v["refresh"] = json!({
            "schedule": s.schedule,
            "last_attempt_at": s.last_attempt_at,
            "last_result": s.last_result,
            "last_error": s.last_error,
            "consecutive_failures": s.consecutive_failures,
        });
    }
    v
}

async fn readyz(State(state): State<Arc<InternalState>>) -> impl IntoResponse {
    let p = &state.probe;
    let (pg, ch, pg_version, ch_version) = tokio::join!(
        with_timeout(rivu_storage_pg::ping(&p.pg)),
        with_timeout(rivu_storage_ch::ping(&p.ch)),
        with_timeout(rivu_storage_pg::max_applied_version(&p.pg)),
        with_timeout(rivu_storage_ch::max_applied_version(&p.ch, &p.ch_database)),
    );

    let (pg_schema_ok, pg_schema) = schema_json(p.required_pg, &pg_version);
    let (ch_schema_ok, ch_schema) = schema_json(p.required_ch, &ch_version);
    let mut ready = pg.is_ok() && ch.is_ok() && pg_schema_ok && ch_schema_ok;

    let mut checks = json!({
        "postgres": check_json(&pg),
        "clickhouse": check_json(&ch),
        "schema": {
            "ok": pg_schema_ok && ch_schema_ok,
            "postgres": pg_schema,
            "clickhouse": ch_schema,
        },
    });
    if let Some(c) = &state.collector {
        let (sites_ok, sites) = sites_check(&c.sites);
        let (salts_ok, salts) = salts_check(&c.salts, utc_today());
        ready = ready && sites_ok && salts_ok;
        checks["sites"] = sites;
        checks["salts"] = salts;
        let mut sessions = json!(*c.sessions);
        sessions["ok"] = json!(true);
        checks["sessions"] = sessions;
    }
    checks["geoip"] = geoip_check(&state.geo);
    if let Some(dc) = &state.datacenter {
        let mut v = json!(dc.status());
        v["ok"] = json!(true);
        checks["datacenter"] = v;
    }
    if let Some(t) = &state.tracker {
        checks["tracker"] = t.status();
    }
    if let Some(d) = &state.dashboard {
        checks["dashboard"] = d.status();
    }

    let body = json!({
        "status": if ready { "ready" } else { "not_ready" },
        "version": build_info::VERSION,
        "commit": build_info::GIT_COMMIT,
        "role": state.role.as_str(),
        "checks": checks,
    });
    let status = if ready {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    (status, Json(body))
}

async fn metrics(State(state): State<Arc<InternalState>>) -> impl IntoResponse {
    metrics::gauge!("rivu_uptime_seconds").set(state.started.elapsed().as_secs_f64());
    (
        [(
            header::CONTENT_TYPE,
            "text/plain; version=0.0.4; charset=utf-8",
        )],
        state.metrics.render(),
    )
}

/// 注册进程级指标。`build_info` 恒为 1，版本信息在标签里。
pub fn describe_metrics() {
    metrics::describe_gauge!(
        "rivu_build_info",
        "构建信息，恒为 1，版本与 commit 在标签里"
    );
    metrics::describe_gauge!("rivu_uptime_seconds", "进程已运行的秒数");
    metrics::gauge!(
        "rivu_build_info",
        "version" => build_info::VERSION,
        "commit" => build_info::GIT_COMMIT
    )
    .set(1.0);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_check_passes_when_applied_reaches_required() {
        let (ok, v) = schema_json(10, &Ok(10));
        assert!(ok);
        assert_eq!(v["applied"], 10);
        assert!(schema_json(10, &Ok(11)).0);
        assert!(!schema_json(10, &Ok(9)).0);
        assert!(!schema_json(10, &Ok(0)).0);
    }

    #[test]
    fn schema_check_fails_on_error() {
        let (ok, v) = schema_json(10, &Err("relation does not exist".into()));
        assert!(!ok);
        assert_eq!(v["ok"], false);
        assert!(v["error"].as_str().unwrap().contains("relation"));
    }

    #[test]
    fn sites_check_requires_a_completed_load() {
        let sites = SiteSnapshot::new();
        let (ok, v) = sites_check(&sites);
        assert!(!ok);
        assert_eq!(v["loaded"], false);
        sites.replace(Default::default());
        let (ok, v) = sites_check(&sites);
        assert!(ok, "加载过但没有站点也算就绪");
        assert_eq!(v["sites"], 0);
    }

    #[test]
    fn salts_check_requires_yesterday_today_and_tomorrow() {
        use jiff::civil::date;
        let salts = SaltCache::new();
        let today = date(2026, 3, 29);
        assert!(!salts_check(&salts, today).0);
        let mut m = std::collections::HashMap::new();
        m.insert(date(2026, 3, 28), [1u8; 16]);
        m.insert(date(2026, 3, 29), [1u8; 16]);
        salts.replace(m.clone());
        let (ok, v) = salts_check(&salts, today);
        assert!(!ok, "缺 today+1");
        assert_eq!(v["loaded"], true);
        m.insert(date(2026, 3, 30), [1u8; 16]);
        salts.replace(m);
        assert!(salts_check(&salts, today).0);
    }

    #[test]
    fn geoip_check_only_reports_and_never_fails() {
        // 库还没下载：loaded=false，但 ok 仍为 true（不影响 /readyz 的判断）
        let geo = GeoReadiness {
            db: Some(Arc::new(GeoDb::new())),
            refresh: Some(Arc::new(GeoRefreshState::default())),
            db_path: std::path::PathBuf::from("/definitely/not/here.mmdb"),
        };
        let v = geoip_check(&geo);
        assert_eq!(v["ok"], true);
        assert_eq!(v["loaded"], false);
        assert_eq!(v["file_present"], false);
        assert_eq!(v["source"], Value::Null);
        assert_eq!(v["refresh"]["schedule"], "not_started");
        assert_eq!(v["refresh"]["consecutive_failures"], 0);
        // worker 没有库对象，只报告刷新状态与文件是否存在
        let worker = GeoReadiness {
            db: None,
            refresh: Some(Arc::new(GeoRefreshState::default())),
            db_path: std::path::PathBuf::from("/definitely/not/here.mmdb"),
        };
        let v = geoip_check(&worker);
        assert_eq!(v["ok"], true);
        assert!(v.get("loaded").is_none());
        // api 两样都没有
        let api = GeoReadiness {
            db: None,
            refresh: None,
            db_path: std::path::PathBuf::from("/x"),
        };
        assert_eq!(
            geoip_check(&api),
            json!({"ok": true, "file_present": false})
        );
    }

    #[test]
    fn check_json_shapes() {
        assert_eq!(check_json(&Ok(())), json!({"ok": true}));
        assert_eq!(check_json(&Err("x".into()))["ok"], false);
    }
}
