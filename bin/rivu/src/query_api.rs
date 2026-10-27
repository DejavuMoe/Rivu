//! 公开查询 API（api 与 all 角色，挂在公共端口上）。
//!
//! | 端点 | 鉴权 | 说明 |
//! |---|---|---|
//! | `POST /api/v1/query` | API key（`stats:read`），按组织限流 | 请求体是 [`QuerySpec`]（`system-design.md` §6.2） |
//! | `GET /api/v1/query/schema.json` | 无 | [`rivu_query::schema_json`]，带 `ETag` 与 `Cache-Control: public, max-age=3600` |
//!
//! # `POST /api/v1/query` 的处理顺序
//!
//! ```text
//! 401 缺失或无效的 key ─► 429 按组织限流 ─► 403 key 没有 stats:read 的 scope      （rivu_auth::protect，限流在 scope 之前）
//!   ─► 413 / 408 / 400 读体（上限 1 MiB、读体超时 10 s）与解析 QuerySpec            （不碰数据库）
//!   ─► 400 缺少 `site`
//!   ─► 404 站点不存在、已删除、属于别的组织或 key 无权访问（同一个响应）            （ApiGuard::authorize_site）
//!   ─► 读站点设置（PG）─► execute（API 档超时 RIVU_API_QUERY_TIMEOUT_SECS）─► 200
//! ```
//!
//! 先解析、后判站点：一个 400 只说明请求体有问题，不泄露站点是否存在；而站点判定在任何 ClickHouse 查询之前。
//!
//! # 站点解析
//!
//! 请求体里的 `site`（`site_key`）是唯一的站点标识；`authorize_site` 用 PG 判定归属与访问权（同一个 key 对应的站点，
//! `SiteOwner`），再用站点 id 读 `timezone` 与 `week_starts_on`（[`rivu_storage_pg::sites::find_query_settings`]）。
//! 不用 collector 的站点快照：api 角色没有快照，快照里也没有周起始日，并且快照最多落后 10 s——
//! 站点被删除或改时区后，公开 API 不该再按旧设置回答。代价是每个查询多两次主键查询。
//! 地理署名与能力取自进程里的 [`GeoDb`]（api 角色也持有库，见 `run.rs`）。
//!
//! # 响应
//!
//! - `200`：[`rivu_query::QueryResult`] 的 JSON（`results` / `meta` / `query`，必要时 `comparison_results`）；
//! - 错误体都有 `error`（机器可读的代码）与 `message`（英文说明），与 `rivu-auth` 的 401 / 403 / 404 / 429 风格一致：
//!
//! | 状态 | `error` | 备注 |
//! |---|---|---|
//! | 400 | `invalid_query` | 另有 `errors: [{"path": "filters[0][2]", "message": "..."}]`；非法 JSON、未知字段、校验错误都是这个形状，顶层问题的 `path` 为 `""` |
//! | 401 | `unauthorized` | `WWW-Authenticate: Bearer realm="rivu"` |
//! | 403 | `forbidden` | |
//! | 404 | `not_found` | 站点不存在与无权访问：完全相同的响应 |
//! | 408 | `request_timeout` | 读请求体超过 10 s；`Connection: close` |
//! | 413 | `payload_too_large` | 请求体超过 1 MiB；`Connection: close` |
//! | 429 | `rate_limited` / `resource_limit` | 限流（`Retry-After`、`X-RateLimit-*`）/ ClickHouse 资源限制（`Retry-After`） |
//! | 500 | `internal` | 不含细节，细节只进日志 |
//! | 503 | `query_timeout` | 查询超过 API 档时限；`Retry-After` |
//!
//! 所有 `POST /api/v1/query` 的响应（包括 401 / 403 / 429）都带 `Cache-Control: no-store`；经过限流之后的响应带
//! `X-RateLimit-Limit|Remaining|Reset`。**不开 CORS**：公开 API 用 `Authorization: Bearer`，面向服务端调用；
//! 文档没有为它规定 CORS（只有 `/api/event` 与站点脚本需要跨域），浏览器页面跨域调用会被浏览器拦截，
//! 这是有意的——把 API key 放进浏览器页面等于公开它。`OPTIONS` 预检也不应答（未带 key 时是 401，带了是 405）。
//!
//! # 请求体上限
//!
//! [`MAX_BODY_BYTES`] = 1 MiB。`QuerySpec` 本身限制了过滤叶子（100）与取值个数（`is` 系列 1000、`contains` 系列 100）、
//! 取值长度（1024 字符），一个很大的合法请求（1000 个 256 字符的路径）约 256 KiB；1 MiB 留足余量，同时让
//! 一个组织的并发请求（默认 4）最多占用几 MiB 内存。先看 `Content-Length`，声明超限直接 413 不读体；分块传输按流累计。
//! 不检查 `Content-Type`（`curl -d` 默认发表单类型；请求体一律按 JSON 解析，不是 JSON 就是 400）。
//!
//! # 指标与日志
//!
//! - `rivu_api_requests_total{route,status}`：`route` 取值 `api.query`、`api.query_schema`、`api.not_found`，
//!   401 / 403 / 429 也计入；
//! - `rivu_query_seconds{route="api.query",cache}`：查询耗时（直方图，秒；`cache` = `hit` | `miss`，§6.9 结果缓存）；`rivu_query_errors_total{route,kind}`；
//! - ClickHouse 的 `log_comment`：`{"route":"api.query","site":<id>,"org":<id>,"api_key_id":<id>,"part":..,"tier":..}`，
//!   只含 id，不含 key 明文、邮箱、IP、过滤值。

use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use axum::Router;
use axum::body::to_bytes;
use axum::extract::{Request, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Json, Response};
use axum::routing::{get, post};
use jiff::Timestamp;
use jiff::civil::Weekday;
use jiff::tz::TimeZone;
use rivu_auth::{
    ApiError, ApiGuard, AuthContext, AuthStore, Required, Resource, ScopeAction, protect,
};
use rivu_geo::GeoDb;
use rivu_query::{
    ExecOptions, GoalDef, QueryCache, QueryError, QueryResult, QuerySpec, SiteContext, TenantScope,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::PgPool;

/// `rivu_api_requests_total` 与日志里的路由名。
pub const ROUTE_QUERY: &str = "api.query";
pub const ROUTE_SCHEMA: &str = "api.query_schema";
pub const ROUTE_NOT_FOUND: &str = "api.not_found";

pub const QUERY_PATH: &str = "/api/v1/query";
pub const SCHEMA_PATH: &str = "/api/v1/query/schema.json";

/// 请求体上限（字节）。见模块文档「请求体上限」。
pub const MAX_BODY_BYTES: usize = 1 << 20;
/// 读请求体的超时。
const BODY_READ_TIMEOUT: Duration = Duration::from_secs(10);
/// 503（查询超时）与 429（资源限制）的 `Retry-After`（秒）：同样的查询马上重试多半还是超时，
/// 建议调用方缩小范围；这里给一个保守的退避。
pub const RETRY_AFTER_SECS: u64 = 10;
/// schema 的 `Cache-Control: max-age`（秒）：schema 只随版本变化，配合 `ETag` 重新验证。
pub const SCHEMA_MAX_AGE_SECS: u64 = 3600;
/// 查询耗时直方图的桶（秒）：覆盖毫秒级到 API 档的 20 s 上限。
pub const QUERY_SECONDS_BUCKETS: [f64; 13] = [
    0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 20.0, 30.0,
];

type Clock = Arc<dyn Fn() -> Timestamp + Send + Sync>;

/// 公开查询 API 的依赖。进程里只建一个：限流状态在 `ApiGuard` 里。
pub struct QueryApi<S> {
    guard: Arc<ApiGuard<S>>,
    pg: PgPool,
    /// `rivu_reader` 账号的客户端（已绑定库）。
    reader: clickhouse::Client,
    geo: Option<Arc<GeoDb>>,
    /// `RIVU_API_QUERY_TIMEOUT_SECS`。
    timeout_secs: u64,
    /// `RIVU_ROLLUP_MIN_ROWS`；`None` 不用日汇总层。
    rollup_min_rows: Option<u64>,
    /// 结果缓存（进程内共用，§6.9）；`None` 不缓存。
    cache: Option<Arc<QueryCache>>,
    /// `RIVU_RETENTION_DAYS`。
    retention_days: u32,
    clock: Clock,
}

impl<S: AuthStore> QueryApi<S> {
    pub fn new(
        guard: Arc<ApiGuard<S>>,
        pg: PgPool,
        reader: clickhouse::Client,
        geo: Option<Arc<GeoDb>>,
        timeout_secs: u64,
    ) -> Self {
        QueryApi {
            guard,
            pg,
            reader,
            geo,
            timeout_secs,
            rollup_min_rows: None,
            cache: None,
            retention_days: rivu_common::config::DEFAULT_RETENTION_DAYS,
            clock: Arc::new(Timestamp::now),
        }
    }

    /// 打开日汇总层（`RIVU_ROLLUP_MIN_ROWS`，见 [`ExecOptions::rollup_min_rows`]）。
    pub fn with_rollup(mut self, min_rows: u64) -> Self {
        self.rollup_min_rows = Some(min_rows);
        self
    }

    /// 原始数据保留期（`RIVU_RETENTION_DAYS`，默认 731）。
    pub fn with_retention(mut self, days: u32) -> Self {
        self.retention_days = days;
        self
    }

    /// 使用结果缓存（与看板共用同一个）。
    pub fn with_cache(mut self, cache: Arc<QueryCache>) -> Self {
        self.cache = Some(cache);
        self
    }

    /// 替换「现在」（测试用：相对日期范围与封顶都按它换算）。
    pub fn with_clock(mut self, clock: impl Fn() -> Timestamp + Send + Sync + 'static) -> Self {
        self.clock = Arc::new(clock);
        self
    }

    /// 站点上下文：时区与周起始日来自 PG，地理署名与能力来自进程里的库。站点此刻已不存在返回 `NotFound`。
    async fn site_context(&self, site_id: u64, site_key: &str) -> Result<SiteContext, Fail> {
        load_site_context(
            &self.pg,
            self.geo.as_deref(),
            site_id,
            site_key,
            self.retention_days,
        )
        .await
        .map_err(Fail::from)
    }
}

/// 站点上下文（公开 API 与看板的内部端点共用）：时区与周起始日读 PG（不用 collector 的快照，原因见模块文档），
/// 地理署名与能力取自进程里的库。站点此刻已不存在返回 `NotFound`。
pub(crate) async fn load_site_context(
    pg: &PgPool,
    geo: Option<&GeoDb>,
    site_id: u64,
    site_key: &str,
    retention_days: u32,
) -> Result<SiteContext, ApiError> {
    let settings = match rivu_storage_pg::sites::find_query_settings(pg, site_id).await {
        Ok(Some(s)) => s,
        Ok(None) => return Err(ApiError::NotFound),
        Err(e) => {
            tracing::error!(site_id, error = %format!("{e:#}"), "读取站点的查询设置失败");
            return Err(ApiError::Internal);
        }
    };
    let timezone = TimeZone::get(&settings.timezone).map_err(|e| {
        tracing::error!(site_id, timezone = %settings.timezone, error = %e, "站点时区无法解析");
        ApiError::Internal
    })?;
    let week_starts_on = i8::try_from(settings.week_starts_on)
        .ok()
        .and_then(|w| Weekday::from_sunday_zero_offset(w).ok())
        .ok_or_else(|| {
            tracing::error!(
                site_id,
                week_starts_on = settings.week_starts_on,
                "站点的周起始日不合法"
            );
            ApiError::Internal
        })?;
    // 目标（§6.3 build 阶段「加载目标」）：`event:goal` 维度与过滤、`conversion_rate` 用；每次查询现读，增删立即生效
    let goals = rivu_storage_pg::goals::list_goals(pg, site_id)
        .await
        .map_err(|e| {
            tracing::error!(site_id, error = %format!("{e:#}"), "读取站点目标失败");
            ApiError::Internal
        })?
        .iter()
        .filter_map(|g| {
            GoalDef::from_parts(
                g.id,
                &g.kind,
                g.event_name.as_deref(),
                g.page_path.as_deref(),
                g.display_name.as_deref(),
            )
        })
        .collect();
    Ok(SiteContext {
        site_id,
        site_key: Some(site_key.to_owned()),
        timezone,
        week_starts_on,
        geo_attribution: geo.and_then(|g| g.attribution()),
        geo_capability: geo.and_then(|g| g.capability()),
        goals,
        // 站点设置只能比全局短（PATCH 时校验；全局调小后也以全局为准）
        retention_days: Some(
            settings
                .retention_days
                .and_then(|d| u32::try_from(d).ok())
                .map_or(retention_days, |d| d.min(retention_days)),
        ),
    })
}
// ------------------------------------------------------------------------------------------
// 错误
// ------------------------------------------------------------------------------------------

/// 处理函数里的失败，统一转成响应。
#[derive(Debug)]
pub(crate) enum Fail {
    /// 鉴权与站点访问的错误（`rivu-auth` 已定义响应）。
    Auth(ApiError),
    Query(QueryError),
    TooLarge,
    BodyTimeout,
    /// 读请求体时连接出错。
    Unreadable,
}

impl From<ApiError> for Fail {
    fn from(e: ApiError) -> Self {
        Fail::Auth(e)
    }
}

impl From<QueryError> for Fail {
    fn from(e: QueryError) -> Self {
        Fail::Query(e)
    }
}

pub(crate) fn no_store(resp: &mut Response) {
    resp.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
}

fn close_connection(resp: &mut Response) {
    resp.headers_mut()
        .insert(header::CONNECTION, HeaderValue::from_static("close"));
}

fn retry_after(resp: &mut Response) {
    resp.headers_mut()
        .insert(header::RETRY_AFTER, HeaderValue::from(RETRY_AFTER_SECS));
}

/// 400 的响应体：`error` + `message`（第一条错误）+ `errors[{path, message}]`。
fn invalid_query_body(errors: &[(&str, &str)]) -> Value {
    let first = errors.first().map_or("invalid query", |(_, m)| *m);
    json!({
        "error": "invalid_query",
        "message": first,
        "errors": errors
            .iter()
            .map(|(path, message)| json!({"path": path, "message": message}))
            .collect::<Vec<_>>(),
    })
}

/// 指标 `rivu_query_errors_total` 的 `kind` 标签。
pub(crate) fn error_kind(e: &QueryError) -> &'static str {
    match e {
        QueryError::Validation { .. } => "validation",
        QueryError::Timeout => "timeout",
        QueryError::ResourceLimit(_) => "resource_limit",
        QueryError::Internal(_) => "internal",
    }
}

/// `QueryError` → 响应，状态码取自 [`QueryError::http_status_hint`]。内部错误不泄露细节，只记日志。
pub(crate) fn query_error_response(e: &QueryError, timeout_secs: Option<u64>) -> Response {
    let status =
        StatusCode::from_u16(e.http_status_hint()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let mut resp = match e {
        QueryError::Validation { path, message } => (
            status,
            Json(invalid_query_body(&[(path.as_str(), message.as_str())])),
        )
            .into_response(),
        QueryError::Timeout => {
            let limit = timeout_secs.map_or(String::new(), |s| format!(" ({s} s)"));
            let mut r = (
                status,
                Json(json!({"error": "query_timeout",
                            "message": format!("the query exceeded its time limit{limit}; narrow the date range or add filters, then retry")})),
            )
                .into_response();
            retry_after(&mut r);
            r
        }
        QueryError::ResourceLimit(detail) => {
            tracing::warn!(detail = %detail, "查询触发了 ClickHouse 的资源限制");
            let mut r = (
                status,
                Json(json!({"error": "resource_limit",
                            "message": "the query exceeded a resource limit; narrow the date range, the dimensions or the filters"})),
            )
                .into_response();
            retry_after(&mut r);
            r
        }
        QueryError::Internal(err) => {
            tracing::error!(error = %format!("{err:#}"), "查询失败（内部错误）");
            ApiError::Internal.into_response()
        }
    };
    no_store(&mut resp);
    resp
}

/// 解析 / 读体 / 站点判定之外的失败的响应。`timeout_secs` 只用于超时提示文字。
pub(crate) fn fail_response(f: Fail, timeout_secs: u64) -> Response {
    let mut resp = match f {
        Fail::Auth(e) => e.into_response(),
        Fail::Query(e) => query_error_response(&e, Some(timeout_secs)),
        Fail::TooLarge => {
            let mut r = (
                StatusCode::PAYLOAD_TOO_LARGE,
                Json(json!({"error": "payload_too_large",
                            "message": format!("the request body is too large (max {MAX_BODY_BYTES} bytes)")})),
            )
                .into_response();
            close_connection(&mut r);
            r
        }
        Fail::BodyTimeout => {
            let mut r = (
                StatusCode::REQUEST_TIMEOUT,
                Json(json!({"error": "request_timeout",
                            "message": "timed out while reading the request body"})),
            )
                .into_response();
            close_connection(&mut r);
            r
        }
        Fail::Unreadable => (
            StatusCode::BAD_REQUEST,
            Json(invalid_query_body(&[(
                "",
                "the request body could not be read",
            )])),
        )
            .into_response(),
    };
    no_store(&mut resp);
    resp
}

// ------------------------------------------------------------------------------------------
// POST /api/v1/query
// ------------------------------------------------------------------------------------------

/// 读请求体：声明的长度超限直接拒绝；分块按流累计，超过上限即停止。
pub(crate) async fn read_body(req: Request, max_bytes: usize) -> Result<axum::body::Bytes, Fail> {
    let declared = req
        .headers()
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<u64>().ok());
    if declared.is_some_and(|n| n > max_bytes as u64) {
        return Err(Fail::TooLarge);
    }
    match tokio::time::timeout(BODY_READ_TIMEOUT, to_bytes(req.into_body(), max_bytes)).await {
        Ok(Ok(bytes)) => Ok(bytes),
        Ok(Err(e)) => {
            let over_limit = std::error::Error::source(&e).is_some_and(|s| {
                s.downcast_ref::<http_body_util::LengthLimitError>()
                    .is_some()
            });
            if over_limit {
                Err(Fail::TooLarge)
            } else {
                Err(Fail::Unreadable)
            }
        }
        Err(_) => Err(Fail::BodyTimeout),
    }
}

/// 执行一条查询：先查结果缓存（§6.9），未命中再执行并写入缓存；记录耗时（按是否命中分开）与失败指标。
pub(crate) async fn execute_cached(
    reader: &clickhouse::Client,
    cache: Option<&QueryCache>,
    site: &SiteContext,
    spec: QuerySpec,
    opts: ExecOptions,
    route: &'static str,
) -> Result<QueryResult, QueryError> {
    let started = Instant::now();
    let key = cache.map(|c| c.key(site, &spec, opts.now));
    if let (Some(c), Some(k)) = (cache, key)
        && let Some(hit) = c.get(k)
    {
        metrics::histogram!("rivu_query_seconds", "route" => route, "cache" => "hit")
            .record(started.elapsed().as_secs_f64());
        return Ok(QueryResult::clone(&hit));
    }
    let result =
        rivu_query::execute(reader, &TenantScope::new(site.site_id), site, spec, opts).await;
    metrics::histogram!("rivu_query_seconds", "route" => route, "cache" => "miss")
        .record(started.elapsed().as_secs_f64());
    match result {
        Ok(r) => {
            if let (Some(c), Some(k)) = (cache, key) {
                c.insert(k, Arc::new(r.clone()));
            }
            Ok(r)
        }
        Err(e) => {
            metrics::counter!("rivu_query_errors_total", "route" => route, "kind" => error_kind(&e))
                .increment(1);
            Err(e)
        }
    }
}

async fn run_query<S: AuthStore>(
    api: &QueryApi<S>,
    ctx: &AuthContext,
    req: Request,
) -> Result<QueryResult, Fail> {
    let bytes = read_body(req, MAX_BODY_BYTES).await?;
    let text = std::str::from_utf8(&bytes)
        .map_err(|_| QueryError::validation("", "the body is not valid UTF-8"))?;
    let spec = QuerySpec::parse_str(text)?;
    let Some(site_key) = spec.site.clone() else {
        // 公开 API 没有站点路径参数：`site` 必填（看板的内部端点才允许省略）
        return Err(QueryError::validation("site", "this field is required").into());
    };
    if !rivu_domain::is_site_key(&site_key) {
        return Err(ApiError::NotFound.into());
    }
    let owner = api
        .guard
        .authorize_site(ctx, &site_key, Resource::Stats, ScopeAction::Read)
        .await?;
    let site = api.site_context(owner.id, &owner.site_key).await?;

    let mut opts = ExecOptions::new(api.timeout_secs)
        .with_now((api.clock)())
        .with_route(ROUTE_QUERY)
        .with_comment("org", ctx.org_id)
        .with_comment("api_key_id", ctx.api_key_id);
    opts.rollup_min_rows = api.rollup_min_rows;
    execute_cached(
        &api.reader,
        api.cache.as_deref(),
        &site,
        spec,
        opts,
        ROUTE_QUERY,
    )
    .await
    .map_err(Fail::Query)
}

async fn query<S: AuthStore>(
    State(api): State<Arc<QueryApi<S>>>,
    ctx: AuthContext,
    req: Request,
) -> Response {
    match run_query(&api, &ctx, req).await {
        Ok(result) => {
            let mut resp = Json(result).into_response();
            no_store(&mut resp);
            resp
        }
        Err(f) => fail_response(f, api.timeout_secs),
    }
}

async fn query_method_not_allowed() -> Response {
    let mut resp = StatusCode::METHOD_NOT_ALLOWED.into_response();
    resp.headers_mut()
        .insert(header::ALLOW, HeaderValue::from_static("POST"));
    resp
}

// ------------------------------------------------------------------------------------------
// GET /api/v1/query/schema.json
// ------------------------------------------------------------------------------------------

struct SchemaDoc {
    body: String,
    etag: String,
}

fn schema_doc() -> &'static SchemaDoc {
    static DOC: OnceLock<SchemaDoc> = OnceLock::new();
    DOC.get_or_init(|| {
        let body = serde_json::to_string(&rivu_query::schema_json()).expect("schema 可序列化");
        let digest = Sha256::digest(body.as_bytes());
        let hex: String = digest[..16].iter().map(|b| format!("{b:02x}")).collect();
        SchemaDoc {
            body,
            etag: format!("\"{hex}\""),
        }
    })
}

/// `If-None-Match` 是否命中（`*`、逗号分隔的列表、弱校验前缀 `W/` 都按规范处理）。
fn etag_matches(if_none_match: &str, etag: &str) -> bool {
    if_none_match.split(',').any(|t| {
        let t = t.trim();
        t == "*" || t.strip_prefix("W/").unwrap_or(t) == etag
    })
}

async fn schema(headers: HeaderMap) -> Response {
    let doc = schema_doc();
    let cache = HeaderValue::from_str(&format!("public, max-age={SCHEMA_MAX_AGE_SECS}"))
        .expect("合法的头值");
    let etag = HeaderValue::from_str(&doc.etag).expect("合法的头值");
    let hit = headers
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| etag_matches(v, &doc.etag));
    let mut resp = if hit {
        StatusCode::NOT_MODIFIED.into_response()
    } else {
        (
            [(
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/json"),
            )],
            doc.body.clone(),
        )
            .into_response()
    };
    let h = resp.headers_mut();
    h.insert(header::ETAG, etag);
    h.insert(header::CACHE_CONTROL, cache);
    resp
}

async fn schema_method_not_allowed() -> Response {
    let mut resp = StatusCode::METHOD_NOT_ALLOWED.into_response();
    resp.headers_mut()
        .insert(header::ALLOW, HeaderValue::from_static("GET, HEAD"));
    resp
}

// ------------------------------------------------------------------------------------------
// 指标中间件、路由
// ------------------------------------------------------------------------------------------

#[derive(Clone, Copy)]
pub(crate) struct Instrument {
    route: &'static str,
    /// 响应没有自己的 `Cache-Control` 时补 `no-store`（公开查询的所有响应都不可缓存）。
    default_no_store: bool,
}

const X_CONTENT_TYPE_OPTIONS: HeaderName = HeaderName::from_static("x-content-type-options");

/// 计入 `rivu_api_requests_total{route,status}`，并按需补 `Cache-Control: no-store`。
/// 套在 `protect` 之外，所以 401 / 403 / 429 也被计数。
async fn instrument(State(i): State<Instrument>, req: Request, next: Next) -> Response {
    let mut resp = next.run(req).await;
    metrics::counter!(
        "rivu_api_requests_total",
        "route" => i.route,
        "status" => resp.status().as_u16().to_string()
    )
    .increment(1);
    if i.default_no_store && !resp.headers().contains_key(header::CACHE_CONTROL) {
        no_store(&mut resp);
    }
    resp.headers_mut()
        .insert(X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    resp
}

/// 给一个路由表套上请求计数（`route` 是固定的低基数标签）。
pub fn count_requests(router: Router, route: &'static str) -> Router {
    router.layer(middleware::from_fn_with_state(
        Instrument {
            route,
            default_no_store: false,
        },
        instrument,
    ))
}

/// 公开查询 API 的路由：`POST /api/v1/query`（鉴权、限流）与 `GET /api/v1/query/schema.json`（公开）。
/// 没有匹配的 `/api/*` 由 [`crate::public`] 返回 404。
pub fn router<S: AuthStore>(api: Arc<QueryApi<S>>) -> Router {
    let guard = Arc::clone(&api.guard);
    let query_routes = Router::new()
        .route(
            QUERY_PATH,
            post(query::<S>).fallback(query_method_not_allowed),
        )
        .with_state(api);
    let query_routes = protect(
        query_routes,
        guard,
        Required::new(Resource::Stats, ScopeAction::Read),
    )
    .layer(middleware::from_fn_with_state(
        Instrument {
            route: ROUTE_QUERY,
            default_no_store: true,
        },
        instrument,
    ));

    let schema_routes =
        Router::new().route(SCHEMA_PATH, get(schema).fallback(schema_method_not_allowed));
    let schema_routes = count_requests(schema_routes, ROUTE_SCHEMA);
    query_routes.merge(schema_routes)
}

/// 注册指标说明。
pub fn describe_metrics() {
    metrics::describe_counter!(
        "rivu_api_requests_total",
        "公开 API 请求数，按路由与状态码（含 401 / 403 / 429）"
    );
    metrics::describe_histogram!(
        "rivu_query_seconds",
        metrics::Unit::Seconds,
        "QuerySpec 执行耗时，按路由与是否命中结果缓存（cache = hit | miss）"
    );
    metrics::describe_counter!(
        "rivu_query_errors_total",
        "QuerySpec 执行失败次数，按路由与类别（validation / timeout / resource_limit / internal）"
    );
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use self::memory::MemoryStore;
    use super::*;
    use crate::testutil::{Resp, send};

    /// 测试用的内存存储（不依赖 PostgreSQL）。
    mod memory {
        use std::collections::HashMap;
        use std::sync::Mutex;

        use rivu_auth::AuthStore;
        use rivu_storage_pg::api_keys::KeyRecord;
        use rivu_storage_pg::sites::SiteOwner;

        #[derive(Default)]
        pub struct MemoryStore {
            pub keys: Mutex<HashMap<[u8; 32], KeyRecord>>,
            pub sites: HashMap<String, SiteOwner>,
        }

        impl AuthStore for MemoryStore {
            async fn find_key(&self, hash: &[u8; 32]) -> anyhow::Result<Option<KeyRecord>> {
                Ok(self.keys.lock().unwrap().get(hash).cloned())
            }

            async fn find_site(&self, site_key: &str) -> anyhow::Result<Option<SiteOwner>> {
                Ok(self.sites.get(site_key).cloned())
            }

            async fn touch_last_used(&self, _key_id: i64) -> anyhow::Result<()> {
                Ok(())
            }
        }
    }

    const OWN_SITE: &str = "aaaaaaaaaaaa";
    const OTHER_SITE: &str = "bbbbbbbbbbbb";

    fn site(id: u64, key: &str, org: i64) -> (String, rivu_storage_pg::sites::SiteOwner) {
        (
            key.to_owned(),
            rivu_storage_pg::sites::SiteOwner {
                id,
                site_key: key.to_owned(),
                organization_id: org,
                is_public: false,
            },
        )
    }

    fn add_key(store: &MemoryStore, scopes: &[&str]) -> String {
        let k = rivu_auth::key::generate();
        store.keys.lock().unwrap().insert(
            k.hash(),
            rivu_storage_pg::api_keys::KeyRecord {
                id: 7,
                organization_id: 1,
                user_id: 9,
                role: "owner".into(),
                scopes: scopes.iter().map(|s| (*s).to_owned()).collect(),
                api_hourly_limit: None,
                key_hash: k.hash().to_vec(),
            },
        );
        k.plaintext().to_owned()
    }

    /// 内存存储 + 不会被连接的 PG / ClickHouse：只能走到「判定站点」为止的路径。
    fn app(scopes: &[&str]) -> (Router, String) {
        let store = MemoryStore {
            sites: HashMap::from([site(1, OWN_SITE, 1), site(2, OTHER_SITE, 2)]),
            ..Default::default()
        };
        let key = add_key(&store, scopes);
        let guard = Arc::new(ApiGuard::new(store, rivu_ratelimit::ApiLimits::default()));
        let pg = rivu_storage_pg::connect_lazy("postgres://u:p@127.0.0.1:1/none", 1).unwrap();
        let api = QueryApi::new(guard, pg, clickhouse::Client::default(), None, 20);
        (router(Arc::new(api)), key)
    }

    async fn post_query(app: &Router, key: Option<&str>, headers: &[&str], body: &[u8]) -> Resp {
        let auth = key.map(|k| format!("Authorization: Bearer {k}"));
        let len = format!("Content-Length: {}", body.len());
        let mut h: Vec<&str> = vec!["Content-Type: application/json", &len];
        if let Some(a) = &auth {
            h.push(a);
        }
        h.extend_from_slice(headers);
        send(app.clone(), "POST", QUERY_PATH, &h, body).await
    }

    fn json_body(r: &Resp) -> Value {
        serde_json::from_str(&r.body).unwrap_or_else(|e| panic!("不是 JSON：{e}：{}", r.body))
    }

    // ---------------------------------------------------------------- 错误映射

    #[test]
    fn query_errors_map_onto_status_codes_and_bodies() {
        let r = query_error_response(&QueryError::validation("filters[0][2]", "bad value"), None);
        assert_eq!(r.status(), 400);
        assert_eq!(r.headers()["cache-control"], "no-store");

        let r = query_error_response(&QueryError::Timeout, Some(20));
        assert_eq!(r.status(), 503);
        assert_eq!(
            r.headers()["retry-after"],
            RETRY_AFTER_SECS.to_string().as_str()
        );
        assert_eq!(r.headers()["cache-control"], "no-store");

        let r = query_error_response(&QueryError::ResourceLimit("main secret".into()), None);
        assert_eq!(r.status(), 429);
        assert_eq!(
            r.headers()["retry-after"],
            RETRY_AFTER_SECS.to_string().as_str()
        );

        let r = query_error_response(
            &QueryError::internal(anyhow::anyhow!("password=hunter2 at 10.0.0.5")),
            None,
        );
        assert_eq!(r.status(), 500);
        assert!(r.headers().get("retry-after").is_none());
        assert_eq!(r.headers()["cache-control"], "no-store");
    }

    #[tokio::test]
    async fn error_bodies_have_the_documented_shape_and_never_leak_details() {
        async fn body_of(r: Response) -> Value {
            let b = to_bytes(r.into_body(), 1 << 20).await.unwrap();
            serde_json::from_slice(&b).unwrap()
        }
        let v = body_of(query_error_response(
            &QueryError::validation("filters[0][2]", "bad value"),
            None,
        ))
        .await;
        assert_eq!(
            v,
            json!({"error": "invalid_query", "message": "bad value",
                   "errors": [{"path": "filters[0][2]", "message": "bad value"}]})
        );
        let v = body_of(query_error_response(&QueryError::Timeout, Some(20))).await;
        assert_eq!(v["error"], "query_timeout");
        assert!(v["message"].as_str().unwrap().contains("20 s"));
        let v = body_of(query_error_response(
            &QueryError::ResourceLimit("the database rejected the query (main) SECRET".into()),
            None,
        ))
        .await;
        assert_eq!(v["error"], "resource_limit");
        assert!(!v.to_string().contains("SECRET"));
        let v = body_of(query_error_response(
            &QueryError::internal(anyhow::anyhow!("password=hunter2 at 10.0.0.5")),
            None,
        ))
        .await;
        assert_eq!(v, json!({"error": "internal", "message": "internal error"}));
    }

    #[test]
    fn error_kinds_cover_every_variant() {
        assert_eq!(error_kind(&QueryError::validation("", "x")), "validation");
        assert_eq!(error_kind(&QueryError::Timeout), "timeout");
        assert_eq!(
            error_kind(&QueryError::ResourceLimit(String::new())),
            "resource_limit"
        );
        assert_eq!(
            error_kind(&QueryError::internal(anyhow::anyhow!("x"))),
            "internal"
        );
    }

    // ---------------------------------------------------------------- 鉴权链与请求体

    #[tokio::test]
    async fn unauthenticated_requests_are_401_without_reading_the_body() {
        let (app, _key) = app(&["stats:read:*"]);
        let r = post_query(&app, None, &[], b"{}").await;
        assert_eq!(r.status, 401);
        assert_eq!(r.header("cache-control"), Some("no-store"));
        assert_eq!(json_body(&r)["error"], "unauthorized");
        // 声明超大的体：401 先于 413（没有 key 的人不能让服务端去读体）
        let r = send(
            app.clone(),
            "POST",
            QUERY_PATH,
            &["Content-Length: 99999999"],
            b"",
        )
        .await;
        assert_eq!(r.status, 401);
    }

    #[tokio::test]
    async fn scope_without_stats_read_is_403() {
        let (app, key) = app(&["sites:read:*"]);
        let r = post_query(&app, Some(&key), &[], b"{}").await;
        assert_eq!(r.status, 403);
        assert_eq!(r.header("cache-control"), Some("no-store"));
        assert!(r.header("x-ratelimit-limit").is_some());
    }

    #[tokio::test]
    async fn bad_bodies_are_400_with_paths() {
        let (app, key) = app(&["stats:read:*"]);
        let day = r#""date_range":"day""#;
        let cases: [(&str, String, &str, &str); 6] = [
            ("非法 JSON", "{not json".into(), "", "not valid JSON"),
            ("空体", String::new(), "", "not valid JSON"),
            ("不是对象", "[1]".into(), "", "must be a JSON object"),
            (
                "未知字段",
                format!(r#"{{"site":"{OWN_SITE}","metrics":["visitors"],{day},"bogus":1}}"#),
                "bogus",
                "unknown field",
            ),
            (
                "缺少 metrics",
                format!(r#"{{"site":"{OWN_SITE}",{day}}}"#),
                "metrics",
                "required",
            ),
            (
                "缺少 site",
                format!(r#"{{"metrics":["visitors"],{day}}}"#),
                "site",
                "required",
            ),
        ];
        for (name, body, path, message) in cases {
            let r = post_query(&app, Some(&key), &[], body.as_bytes()).await;
            assert_eq!(r.status, 400, "{name}：{}", r.body);
            assert_eq!(r.header("content-type"), Some("application/json"), "{name}");
            assert_eq!(r.header("cache-control"), Some("no-store"), "{name}");
            assert!(r.header("x-ratelimit-remaining").is_some(), "{name}");
            let v = json_body(&r);
            assert_eq!(v["error"], "invalid_query", "{name}");
            let e = &v["errors"][0];
            assert_eq!(e["path"], path, "{name}：{v}");
            assert!(
                e["message"].as_str().unwrap().contains(message),
                "{name}：{v}"
            );
            assert_eq!(v["message"], e["message"], "{name}");
        }
        // 非 UTF-8
        let r = post_query(&app, Some(&key), &[], &[0xff, 0xfe]).await;
        assert_eq!(r.status, 400);
        assert!(r.body.contains("UTF-8"));
    }

    #[tokio::test]
    async fn body_limit_is_enforced_for_declared_and_chunked_bodies() {
        let (app, key) = app(&["stats:read:*"]);
        // 声明超限：不读体直接 413 + Connection: close
        let len = format!("Content-Length: {}", MAX_BODY_BYTES + 1);
        let auth = format!("Authorization: Bearer {key}");
        let r = send(app.clone(), "POST", QUERY_PATH, &[&len, &auth], b"").await;
        assert_eq!(r.status, 413);
        assert_eq!(r.header("connection"), Some("close"));
        assert_eq!(r.header("cache-control"), Some("no-store"));
        let v = json_body(&r);
        assert_eq!(v["error"], "payload_too_large");
        assert!(v["message"].as_str().unwrap().contains("1048576"));

        // 分块传输：按流累计，超限 413
        let big = vec![b' '; MAX_BODY_BYTES + 1];
        let mut chunked = format!("{:x}\r\n", big.len()).into_bytes();
        chunked.extend_from_slice(&big);
        chunked.extend_from_slice(b"\r\n0\r\n\r\n");
        let r = send(
            app.clone(),
            "POST",
            QUERY_PATH,
            &["Transfer-Encoding: chunked", &auth],
            &chunked,
        )
        .await;
        assert_eq!(r.status, 413, "{}", r.body);

        // 恰好在上限：读得进来，只是内容不合法（400，不是 413）
        let pad = " ".repeat(MAX_BODY_BYTES - 2);
        let at_limit = format!("{{{pad}}}");
        assert_eq!(at_limit.len(), MAX_BODY_BYTES);
        let r = post_query(&app, Some(&key), &[], at_limit.as_bytes()).await;
        assert_eq!(r.status, 400, "{}", &r.body[..r.body.len().min(200)]);
    }

    #[tokio::test]
    async fn unknown_foreign_and_overlong_site_keys_get_the_identical_404() {
        let (app, key) = app(&["stats:read:*"]);
        let body = |site: &str| {
            format!(r#"{{"site":"{site}","metrics":["visitors"],"date_range":"day"}}"#)
        };
        let unknown = post_query(&app, Some(&key), &[], body("zzzzzzzzzzzz").as_bytes()).await;
        let foreign = post_query(&app, Some(&key), &[], body(OTHER_SITE).as_bytes()).await;
        let long = post_query(&app, Some(&key), &[], body(&"a".repeat(500)).as_bytes()).await;
        assert_eq!(unknown.status, 404);
        assert_eq!(foreign.status, 404);
        assert_eq!(long.status, 404);
        assert_eq!(unknown.body, foreign.body);
        assert_eq!(unknown.body, long.body);
        assert_eq!(json_body(&unknown)["error"], "not_found");
        assert_eq!(foreign.header("cache-control"), Some("no-store"));
    }

    #[tokio::test]
    async fn only_post_is_allowed_on_the_query_route() {
        let (app, key) = app(&["stats:read:*"]);
        let auth = format!("Authorization: Bearer {key}");
        let r = send(app.clone(), "GET", QUERY_PATH, &[&auth], b"").await;
        assert_eq!(r.status, 405);
        assert_eq!(r.header("allow"), Some("POST"));
        assert_eq!(r.header("cache-control"), Some("no-store"));
        assert_eq!(r.header("access-control-allow-origin"), None, "不开 CORS");
        // 预检：不应答 CORS
        let r = send(app.clone(), "OPTIONS", QUERY_PATH, &[&auth], b"").await;
        assert_eq!(r.status, 405);
        assert_eq!(r.header("access-control-allow-origin"), None);
        assert_eq!(r.header("access-control-allow-methods"), None);
    }

    // ---------------------------------------------------------------- schema.json

    #[tokio::test]
    async fn schema_is_public_cacheable_and_parseable() {
        let (app, _key) = app(&["stats:read:*"]);
        let r = send(app.clone(), "GET", SCHEMA_PATH, &[], b"").await;
        assert_eq!(r.status, 200);
        assert_eq!(r.header("content-type"), Some("application/json"));
        assert_eq!(r.header("cache-control"), Some("public, max-age=3600"));
        let etag = r.header("etag").expect("ETag").to_owned();
        assert!(etag.starts_with('"') && etag.ends_with('"'), "{etag}");
        assert_eq!(r.header("access-control-allow-origin"), None);
        // 无需鉴权，也不消耗配额
        assert_eq!(r.header("x-ratelimit-limit"), None);
        let v = json_body(&r);
        assert_eq!(v, rivu_query::schema_json());
        assert!(v["properties"]["metrics"].is_object(), "{v}");
        assert!(v.to_string().contains("views_per_visit"));

        // 条件请求
        for inm in [
            etag.clone(),
            format!("W/{etag}"),
            format!("\"x\", {etag}"),
            "*".into(),
        ] {
            let h = format!("If-None-Match: {inm}");
            let r = send(app.clone(), "GET", SCHEMA_PATH, &[&h], b"").await;
            assert_eq!(r.status, 304, "{inm}");
            assert_eq!(r.body, "");
            assert_eq!(r.header("etag"), Some(etag.as_str()));
            assert_eq!(r.header("cache-control"), Some("public, max-age=3600"));
        }
        let r = send(
            app.clone(),
            "GET",
            SCHEMA_PATH,
            &["If-None-Match: \"stale\""],
            b"",
        )
        .await;
        assert_eq!(r.status, 200);
        // 其他方法
        let r = send(
            app.clone(),
            "POST",
            SCHEMA_PATH,
            &["Content-Length: 0"],
            b"",
        )
        .await;
        assert_eq!(r.status, 405);
        assert_eq!(r.header("allow"), Some("GET, HEAD"));
    }

    #[test]
    fn etag_comparison_follows_the_rfc() {
        assert!(etag_matches("\"a\"", "\"a\""));
        assert!(etag_matches("W/\"a\"", "\"a\""));
        assert!(etag_matches("\"b\" , \"a\"", "\"a\""));
        assert!(etag_matches("*", "\"a\""));
        assert!(!etag_matches("\"b\"", "\"a\""));
        assert!(!etag_matches("", "\"a\""));
    }

    #[test]
    fn schema_etag_is_stable_within_a_process() {
        assert_eq!(schema_doc().etag, schema_doc().etag);
        assert_eq!(
            schema_doc().body,
            serde_json::to_string(&rivu_query::schema_json()).unwrap()
        );
    }
}
