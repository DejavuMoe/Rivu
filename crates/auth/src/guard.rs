//! API key 鉴权 + 按组织限流的 axum 中间件，以及站点访问判定。
//!
//! 用法（公开查询路由由别处挂载，这里只给组件）：
//!
//! ```ignore
//! let guard = Arc::new(ApiGuard::new(PgStore::new(pool), ApiLimits::default()));
//! let routes = Router::new()
//!     .route("/api/v1/query", post(query))     // 处理函数里 `guard.authorize_site(&ctx, site_key, ..)`
//!     .with_state(Arc::clone(&guard));
//! let app = protect(routes, guard, Required::new(Resource::Stats, ScopeAction::Read));
//! ```
//!
//! [`protect`] 用 `route_layer`：只作用于匹配到的路由，未知路径照常 404，不要求鉴权。

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::extract::{FromRequestParts, Request, State};
use axum::http::request::Parts;
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Json, Response};
use parking_lot::Mutex;
use rivu_ratelimit::{ApiLimiter, ApiLimits, RateInfo, Rejection};
use rivu_storage_pg::sites::SiteOwner;
use serde_json::json;
use tokio::time::Instant;

use crate::key::{ct_eq, hash_key, is_well_formed, parse_bearer};
use crate::scope::{Action, OrgRole, Resource, ScopeAction, Scopes, allowed};
use crate::store::AuthStore;

/// `last_used_at` 的最短写入间隔：同一把 key 一分钟最多写一次库。
pub const LAST_USED_INTERVAL: Duration = Duration::from_secs(60);

const X_RATELIMIT_LIMIT: HeaderName = HeaderName::from_static("x-ratelimit-limit");
const X_RATELIMIT_REMAINING: HeaderName = HeaderName::from_static("x-ratelimit-remaining");
const X_RATELIMIT_RESET: HeaderName = HeaderName::from_static("x-ratelimit-reset");

/// 鉴权通过后的调用方身份，放在请求扩展里（处理函数用 `AuthContext` 提取器取出）。
#[derive(Clone, Debug)]
pub struct AuthContext {
    pub api_key_id: i64,
    /// key 所属组织：限流的键，也是站点访问判定里「key 的 org 必须等于站点所属 org」的一侧。
    pub org_id: i64,
    /// key 所属用户（`api_keys.user_id` 非空，所以这里不是 `Option`）。
    pub user_id: i64,
    /// 用户当前在该组织里的角色：取 scope 与角色矩阵的交集（`platform-and-ops.md` §4.2 第 5 步）。
    pub role: OrgRole,
    /// key 的作用域。可访问的站点集合不预先展开，而是按站点判定：`scopes.covers_site(..)` 加组织比较。
    pub scopes: Scopes,
    /// `organizations.api_hourly_limit`，`None` 用全局默认。
    pub hourly_limit: Option<u32>,
}

impl<S: Send + Sync> FromRequestParts<S> for AuthContext {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, ApiError> {
        parts
            .extensions
            .get::<AuthContext>()
            .cloned()
            .ok_or_else(|| {
                // 路由没有套 `protect`：宁可拒绝也不能放行
                tracing::error!("处理函数要求 AuthContext，但路由没有经过 API key 中间件");
                ApiError::Unauthorized
            })
    }
}

/// 路由要求的 scope：key 必须有这个资源与动作（任意目标）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Required {
    pub resource: Resource,
    pub action: ScopeAction,
}

impl Required {
    pub fn new(resource: Resource, action: ScopeAction) -> Self {
        Required { resource, action }
    }
}

/// 失败响应。除限流外都不暴露原因细节。
#[derive(Debug)]
pub enum ApiError {
    /// 401：缺失、格式错误、未知、已吊销、已过期、用户被禁用或被移出组织，一律同一个响应。
    Unauthorized,
    /// 403：key 没有这个资源与动作的 scope。
    Forbidden(Required),
    /// 404：站点不存在、已删除、属于别的组织，或 key 无权访问；响应完全相同，避免枚举 `site_key`。
    NotFound,
    /// 429。
    RateLimited(Rejection),
    /// 500：存储出错（细节只进日志）。
    Internal,
}

fn set_rate_headers(h: &mut HeaderMap, info: RateInfo) {
    h.insert(X_RATELIMIT_LIMIT, HeaderValue::from(info.limit));
    h.insert(X_RATELIMIT_REMAINING, HeaderValue::from(info.remaining));
    h.insert(X_RATELIMIT_RESET, HeaderValue::from(info.reset_secs));
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let mut resp = match &self {
            ApiError::Unauthorized => (
                StatusCode::UNAUTHORIZED,
                Json(json!({"error": "unauthorized",
                            "message": "missing or invalid API key; send `Authorization: Bearer <key>`"})),
            )
                .into_response(),
            ApiError::Forbidden(req) => (
                StatusCode::FORBIDDEN,
                Json(json!({"error": "forbidden",
                            "message": format!("the API key lacks the required scope `{}:{}`",
                                               req.resource.as_str(), req.action.as_str())})),
            )
                .into_response(),
            ApiError::NotFound => (
                StatusCode::NOT_FOUND,
                Json(json!({"error": "not_found", "message": "site not found"})),
            )
                .into_response(),
            ApiError::RateLimited(rej) => {
                let mut r = (
                    StatusCode::TOO_MANY_REQUESTS,
                    Json(json!({"error": "rate_limited",
                                "message": "too many requests; retry after the indicated number of seconds",
                                "retry_after": rej.retry_after_secs()})),
                )
                    .into_response();
                r.headers_mut()
                    .insert(header::RETRY_AFTER, HeaderValue::from(rej.retry_after_secs()));
                set_rate_headers(r.headers_mut(), rej.info);
                r
            }
            ApiError::Internal => (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": "internal", "message": "internal error"})),
            )
                .into_response(),
        };
        let h = resp.headers_mut();
        h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
        if matches!(self, ApiError::Unauthorized) {
            h.insert(
                header::WWW_AUTHENTICATE,
                HeaderValue::from_static("Bearer realm=\"rivu\""),
            );
        }
        resp
    }
}

/// 每把 key 的 `last_used_at` 写入节流：内存里记下上次写入时刻，间隔内的请求不碰 PG。
struct LastUsed {
    interval: Duration,
    seen: Mutex<HashMap<i64, Instant>>,
}

impl LastUsed {
    fn should_touch(&self, key_id: i64) -> bool {
        let now = Instant::now();
        let mut seen = self.seen.lock();
        if seen.len() > 4096 {
            let interval = self.interval;
            seen.retain(|_, t| now.saturating_duration_since(*t) < interval);
        }
        match seen.get(&key_id) {
            Some(t) if now.saturating_duration_since(*t) < self.interval => false,
            _ => {
                seen.insert(key_id, now);
                true
            }
        }
    }
}

/// 鉴权与限流的共享状态：存储、按组织的限流器、`last_used_at` 节流。
pub struct ApiGuard<S> {
    store: Arc<S>,
    limiter: ApiLimiter,
    last_used: LastUsed,
}

impl<S: AuthStore> ApiGuard<S> {
    pub fn new(store: S, limits: ApiLimits) -> Self {
        ApiGuard {
            store: Arc::new(store),
            limiter: ApiLimiter::new(limits),
            last_used: LastUsed {
                interval: LAST_USED_INTERVAL,
                seen: Mutex::new(HashMap::new()),
            },
        }
    }

    /// 改 `last_used_at` 的最短写入间隔（测试用）。
    pub fn with_last_used_interval(mut self, interval: Duration) -> Self {
        self.last_used.interval = interval;
        self
    }

    pub fn limiter(&self) -> &ApiLimiter {
        &self.limiter
    }

    /// `Authorization` 头 → [`AuthContext`]。
    ///
    /// 流程：解析 `Bearer` → 检查 key 格式（不对就不查库）→ SHA-256 → 按摘要查有效 key →
    /// 用常数时间比较再确认一次摘要 → 解析角色。任何一步失败都是 401，不区分原因（原因只进 debug 日志）。
    /// 成功后按节流写 `last_used_at`（后台任务，不阻塞请求，间隔内不写）。
    pub async fn authenticate(&self, authorization: Option<&str>) -> Result<AuthContext, ApiError> {
        let Some(raw) = authorization else {
            tracing::debug!("API 请求缺少 Authorization 头");
            return Err(ApiError::Unauthorized);
        };
        let Some(token) = parse_bearer(raw) else {
            tracing::debug!("Authorization 头不是 Bearer 令牌");
            return Err(ApiError::Unauthorized);
        };
        if !is_well_formed(token) {
            tracing::debug!("API key 格式不对");
            return Err(ApiError::Unauthorized);
        }
        let hash = hash_key(token);
        let record = match self.store.find_key(&hash).await {
            Ok(Some(r)) => r,
            Ok(None) => {
                tracing::debug!("API key 未知、已吊销或已失效");
                return Err(ApiError::Unauthorized);
            }
            Err(e) => {
                tracing::error!(error = %format!("{e:#}"), "查询 API key 失败");
                return Err(ApiError::Internal);
            }
        };
        // 查表已经是按摘要精确匹配；再用常数时间比较一次，防御存储层的异常行为
        if !ct_eq(&record.key_hash, &hash) {
            tracing::error!(key_id = record.id, "存储返回的 key 摘要与请求不一致");
            return Err(ApiError::Unauthorized);
        }
        let Some(role) = OrgRole::parse(&record.role) else {
            tracing::error!(key_id = record.id, role = %record.role, "API key 所属用户的角色无法识别");
            return Err(ApiError::Unauthorized);
        };
        if self.last_used.should_touch(record.id) {
            let store = Arc::clone(&self.store);
            let id = record.id;
            tokio::spawn(async move {
                if let Err(e) = store.touch_last_used(id).await {
                    tracing::warn!(key_id = id, error = %format!("{e:#}"), "更新 API key 的 last_used_at 失败");
                }
            });
        }
        Ok(AuthContext {
            api_key_id: record.id,
            org_id: record.organization_id,
            user_id: record.user_id,
            role,
            scopes: Scopes::from_texts(&record.scopes),
            hourly_limit: record.api_hourly_limit.and_then(|n| u32::try_from(n).ok()),
        })
    }

    /// 站点访问判定（`platform-and-ops.md` §4.2 第 1、5、6、7 步；会话用户与 shared link 是别的主体，不在这里）：
    ///
    /// 1. 站点不存在或已软删除 → 404；
    /// 2. key 的组织等于站点所属组织，且 key 的 scope 覆盖该站点（`*` 或该 `site_key`），
    ///    且用户当前角色允许该动作（scope 与角色取交集）→ 放行；
    /// 3. 否则，站点是公开的且动作是读统计 → 放行（对应 Plausible 的 `sites.public`）；
    /// 4. 其他 → 404（不是 403：不泄露站点是否存在）。
    pub async fn authorize_site(
        &self,
        ctx: &AuthContext,
        site_key: &str,
        resource: Resource,
        action: ScopeAction,
    ) -> Result<SiteOwner, ApiError> {
        let site = match self.store.find_site(site_key).await {
            Ok(Some(s)) => s,
            Ok(None) => return Err(ApiError::NotFound),
            Err(e) => {
                tracing::error!(error = %format!("{e:#}"), "查询站点归属失败");
                return Err(ApiError::Internal);
            }
        };
        let granted = site.organization_id == ctx.org_id
            && ctx.scopes.covers_site(resource, action, &site.site_key)
            && allowed(ctx.role, Action::of(resource, action));
        let public_read = site.is_public && Action::of(resource, action) == Action::StatsRead;
        if granted || public_read {
            Ok(site)
        } else {
            Err(ApiError::NotFound)
        }
    }
}

/// [`guard_middleware`] 的状态：守卫加本路由要求的 scope。
pub struct GuardState<S> {
    pub guard: Arc<ApiGuard<S>>,
    pub required: Required,
}

impl<S> Clone for GuardState<S> {
    fn clone(&self) -> Self {
        GuardState {
            guard: Arc::clone(&self.guard),
            required: self.required,
        }
    }
}

/// axum 中间件：401 → 429 → 403，通过后把 [`AuthContext`] 放进请求扩展，响应带 `X-RateLimit-*`。
///
/// 并发名额（`permit`）持有到处理函数返回、响应头生成之后才归还；响应体是流式的话，名额在响应头发出时归还。
/// 用 `middleware::from_fn_with_state(state, guard_middleware::<S>)`，或者直接用 [`protect`]。
pub async fn guard_middleware<S: AuthStore>(
    State(state): State<GuardState<S>>,
    mut req: Request,
    next: Next,
) -> Response {
    let authorization = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let ctx = match state.guard.authenticate(authorization.as_deref()).await {
        Ok(ctx) => ctx,
        Err(e) => return e.into_response(),
    };

    let admitted = match state
        .guard
        .limiter
        .admit(ctx.org_id, ctx.hourly_limit)
        .await
    {
        Ok(a) => a,
        Err(rej) => {
            metrics::counter!("rivu_ratelimit_rejected_total", "scope" => rej.kind.as_str())
                .increment(1);
            return ApiError::RateLimited(rej).into_response();
        }
    };

    if !ctx
        .scopes
        .grants(state.required.resource, state.required.action)
    {
        let mut resp = ApiError::Forbidden(state.required).into_response();
        set_rate_headers(resp.headers_mut(), admitted.info);
        return resp;
    }

    req.extensions_mut().insert(ctx);
    let mut resp = next.run(req).await;
    set_rate_headers(resp.headers_mut(), admitted.info);
    drop(admitted);
    resp
}

/// 给整个路由表套上 API key 鉴权与限流（`route_layer`：只作用于匹配到的路由，未知路径照常 404）。
pub fn protect<S: AuthStore>(
    router: Router,
    guard: Arc<ApiGuard<S>>,
    required: Required,
) -> Router {
    router.route_layer(middleware::from_fn_with_state(
        GuardState { guard, required },
        guard_middleware::<S>,
    ))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use axum::Router;
    use axum::body::{Body, to_bytes};
    use axum::extract::Path;
    use axum::http::Request;
    use axum::routing::{get, put};
    use rivu_storage_pg::api_keys::KeyRecord;
    use tokio::sync::Semaphore;
    use tower::ServiceExt;

    use super::*;
    use crate::key::generate;

    const SITE_A: &str = "aaaaaaaaaaaa"; // 组织 1 的站点
    const SITE_B: &str = "bbbbbbbbbbbb"; // 组织 1 的另一个站点
    const SITE_X: &str = "xxxxxxxxxxxx"; // 组织 2 的站点
    const SITE_PUB: &str = "pppppppppppp"; // 组织 2 的公开站点
    const SITE_GONE: &str = "gggggggggggg"; // 不存在（或已删除）

    #[derive(Default)]
    struct MemoryStore {
        keys: Mutex<HashMap<[u8; 32], KeyRecord>>,
        sites: HashMap<String, SiteOwner>,
        touches: Mutex<Vec<i64>>,
        fail: bool,
    }

    impl AuthStore for MemoryStore {
        async fn find_key(&self, hash: &[u8; 32]) -> anyhow::Result<Option<KeyRecord>> {
            if self.fail {
                anyhow::bail!("数据库不可用");
            }
            Ok(self.keys.lock().get(hash).cloned())
        }

        async fn find_site(&self, site_key: &str) -> anyhow::Result<Option<SiteOwner>> {
            if self.fail {
                anyhow::bail!("数据库不可用");
            }
            Ok(self.sites.get(site_key).cloned())
        }

        async fn touch_last_used(&self, key_id: i64) -> anyhow::Result<()> {
            self.touches.lock().push(key_id);
            Ok(())
        }
    }

    fn site(id: u64, key: &str, org: i64, public: bool) -> (String, SiteOwner) {
        (
            key.to_owned(),
            SiteOwner {
                id,
                site_key: key.to_owned(),
                organization_id: org,
                is_public: public,
            },
        )
    }

    fn store() -> MemoryStore {
        MemoryStore {
            sites: HashMap::from([
                site(1, SITE_A, 1, false),
                site(2, SITE_B, 1, false),
                site(3, SITE_X, 2, false),
                site(4, SITE_PUB, 2, true),
            ]),
            ..Default::default()
        }
    }

    /// 在存储里登记一把 key，返回明文。
    fn add_key(
        store: &MemoryStore,
        id: i64,
        org: i64,
        role: &str,
        scopes: &[&str],
        limit: Option<i32>,
    ) -> String {
        let k = generate();
        store.keys.lock().insert(
            k.hash(),
            KeyRecord {
                id,
                organization_id: org,
                user_id: 100 + id,
                role: role.into(),
                scopes: scopes.iter().map(|s| (*s).to_owned()).collect(),
                api_hourly_limit: limit,
                key_hash: k.hash().to_vec(),
            },
        );
        k.plaintext().to_owned()
    }

    type Guard = Arc<ApiGuard<MemoryStore>>;

    async fn stats_handler(
        State(guard): State<Guard>,
        ctx: AuthContext,
        Path(site_key): Path<String>,
    ) -> Result<Json<serde_json::Value>, ApiError> {
        let site = guard
            .authorize_site(&ctx, &site_key, Resource::Stats, ScopeAction::Read)
            .await?;
        Ok(Json(
            json!({"site_id": site.id, "org": ctx.org_id, "user": ctx.user_id}),
        ))
    }

    async fn settings_handler(
        State(guard): State<Guard>,
        ctx: AuthContext,
        Path(site_key): Path<String>,
    ) -> Result<Json<serde_json::Value>, ApiError> {
        let site = guard
            .authorize_site(&ctx, &site_key, Resource::Sites, ScopeAction::Write)
            .await?;
        Ok(Json(json!({"site_id": site.id})))
    }

    /// 慢处理函数：持有并发名额，直到测试放行。
    async fn slow_handler(State(gate): State<Arc<Semaphore>>) -> &'static str {
        let _p = gate.acquire().await.unwrap();
        "ok"
    }

    fn app(guard: &Guard) -> Router {
        let stats = Router::new()
            .route("/stats/{site_key}", get(stats_handler))
            .with_state(Arc::clone(guard));
        let settings = Router::new()
            .route("/settings/{site_key}", put(settings_handler))
            .with_state(Arc::clone(guard));
        protect(
            stats,
            Arc::clone(guard),
            Required::new(Resource::Stats, ScopeAction::Read),
        )
        .merge(protect(
            settings,
            Arc::clone(guard),
            Required::new(Resource::Sites, ScopeAction::Write),
        ))
    }

    fn limits(hourly: u32, burst: u32, conc: usize) -> ApiLimits {
        ApiLimits {
            default_hourly: hourly,
            burst,
            max_concurrency: conc,
            queue_timeout: Duration::from_secs(2),
        }
    }

    fn guard_with(store: MemoryStore, l: ApiLimits) -> Guard {
        Arc::new(ApiGuard::new(store, l))
    }

    struct Resp {
        status: u16,
        headers: HeaderMap,
        body: String,
    }

    async fn call(app: &Router, method: &str, path: &str, auth: Option<&str>) -> Resp {
        let mut req = Request::builder().method(method).uri(path);
        if let Some(a) = auth {
            req = req.header(header::AUTHORIZATION, a);
        }
        let resp = app
            .clone()
            .oneshot(req.body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = resp.status().as_u16();
        let headers = resp.headers().clone();
        let body =
            String::from_utf8(to_bytes(resp.into_body(), 1 << 20).await.unwrap().to_vec()).unwrap();
        Resp {
            status,
            headers,
            body,
        }
    }

    async fn get_stats(app: &Router, key: &str, site: &str) -> Resp {
        call(
            app,
            "GET",
            &format!("/stats/{site}"),
            Some(&format!("Bearer {key}")),
        )
        .await
    }

    fn hdr<'a>(r: &'a Resp, name: &str) -> Option<&'a str> {
        r.headers.get(name).and_then(|v| v.to_str().ok())
    }

    #[tokio::test]
    async fn missing_malformed_unknown_and_wrong_keys_are_all_the_same_401() {
        let st = store();
        let good = add_key(&st, 1, 1, "owner", &["stats:read:*"], None);
        let g = guard_with(st, limits(10_000, 60, 4));
        let app = app(&g);
        let never_issued = generate().plaintext().to_owned();
        let mut tweaked = good.clone();
        tweaked.pop();
        tweaked.push(if good.ends_with('A') { 'B' } else { 'A' });
        let cases: Vec<Option<String>> = vec![
            None,                              // 没有头
            Some(String::new()),               // 空头
            Some("Basic dXNlcjpwYXNz".into()), // 不是 Bearer
            Some("Bearer".into()),
            Some("Bearer not-a-key".into()),        // 格式不对
            Some(format!("Bearer {never_issued}")), // 格式对、库里没有
            Some(format!("Bearer {tweaked}")),      // 好 key 改了一个字符
            Some(format!("Bearer {}", &good[1..])), // 少一个字符
        ];
        let mut bodies = Vec::new();
        for c in &cases {
            let r = call(&app, "GET", &format!("/stats/{SITE_A}"), c.as_deref()).await;
            assert_eq!(r.status, 401, "{c:?}");
            assert_eq!(hdr(&r, "www-authenticate"), Some("Bearer realm=\"rivu\""));
            assert_eq!(hdr(&r, "cache-control"), Some("no-store"));
            bodies.push(r.body);
        }
        assert!(
            bodies.windows(2).all(|w| w[0] == w[1]),
            "401 的响应体不能暴露原因"
        );
        // 非 UTF-8 / 非 ASCII 的头值
        let req = Request::builder()
            .uri(format!("/stats/{SITE_A}"))
            .header(
                header::AUTHORIZATION,
                HeaderValue::from_bytes(b"Bearer \xff\xfe").unwrap(),
            )
            .body(Body::empty())
            .unwrap();
        assert_eq!(app.clone().oneshot(req).await.unwrap().status(), 401);
        // 好 key 仍然能用
        assert_eq!(get_stats(&app, &good, SITE_A).await.status, 200);
        // 未知路径照常 404，不要求鉴权
        assert_eq!(call(&app, "GET", "/nope", None).await.status, 404);
    }

    #[tokio::test]
    async fn valid_key_gets_the_site_and_rate_limit_headers() {
        let st = store();
        let key = add_key(&st, 1, 1, "viewer", &["stats:read:*"], None);
        let g = guard_with(st, limits(10_000, 60, 4));
        let app = app(&g);
        let r = get_stats(&app, &key, SITE_A).await;
        assert_eq!(r.status, 200);
        let v: serde_json::Value = serde_json::from_str(&r.body).unwrap();
        assert_eq!(
            (v["site_id"].as_u64(), v["org"].as_i64(), v["user"].as_i64()),
            (Some(1), Some(1), Some(101))
        );
        assert_eq!(hdr(&r, "x-ratelimit-limit"), Some("10000"));
        assert_eq!(hdr(&r, "x-ratelimit-remaining"), Some("59"));
        assert_eq!(hdr(&r, "x-ratelimit-reset"), Some("3600"));
        let r = get_stats(&app, &key, SITE_B).await;
        assert_eq!(hdr(&r, "x-ratelimit-remaining"), Some("58"));
    }

    #[tokio::test]
    async fn insufficient_scope_is_403_and_still_counts_against_the_quota() {
        let st = store();
        // 只有 sites:read，访问 stats 路由
        let key = add_key(&st, 1, 1, "owner", &["sites:read:*"], None);
        let g = guard_with(st, limits(10_000, 60, 4));
        let app = app(&g);
        let r = get_stats(&app, &key, SITE_A).await;
        assert_eq!(r.status, 403);
        assert!(r.body.contains("stats:read"), "{}", r.body);
        assert_eq!(
            hdr(&r, "x-ratelimit-remaining"),
            Some("59"),
            "403 也消耗配额"
        );
        // 有 sites:write 的 key 访问写路由：路由层放行
        let w = add_key(&g.store, 2, 1, "owner", &["sites:write:*"], None);
        let r = call(
            &app,
            "PUT",
            &format!("/settings/{SITE_A}"),
            Some(&format!("Bearer {w}")),
        )
        .await;
        assert_eq!(r.status, 200);
        // 只有 stats:read 的 key 访问写路由：403
        let ro = add_key(&g.store, 3, 1, "owner", &["stats:read:*"], None);
        let r = call(
            &app,
            "PUT",
            &format!("/settings/{SITE_A}"),
            Some(&format!("Bearer {ro}")),
        )
        .await;
        assert_eq!(r.status, 403);
    }

    #[tokio::test]
    async fn no_access_to_the_site_is_a_404_with_an_identical_body() {
        let st = store();
        let all = add_key(&st, 1, 1, "owner", &["stats:read:*"], None);
        let one = add_key(&st, 2, 1, "owner", &[&format!("stats:read:{SITE_A}")], None);
        let g = guard_with(st, limits(10_000, 60, 4));
        let app = app(&g);
        // 能访问：自己组织的站点
        assert_eq!(get_stats(&app, &all, SITE_A).await.status, 200);
        assert_eq!(get_stats(&app, &all, SITE_B).await.status, 200);
        assert_eq!(get_stats(&app, &one, SITE_A).await.status, 200);
        // 404：scope 目标不含该站点、别的组织的站点、不存在的站点——响应体完全一样
        let a = get_stats(&app, &one, SITE_B).await;
        let b = get_stats(&app, &all, SITE_X).await;
        let c = get_stats(&app, &all, SITE_GONE).await;
        assert_eq!((a.status, b.status, c.status), (404, 404, 404));
        assert_eq!(a.body, b.body);
        assert_eq!(b.body, c.body);
        // 公开站点：别的组织的 key 读统计放行（第 6 步），写不放行
        assert_eq!(get_stats(&app, &all, SITE_PUB).await.status, 200);
        let w = add_key(&g.store, 3, 1, "owner", &["sites:write:*"], None);
        let r = call(
            &app,
            "PUT",
            &format!("/settings/{SITE_PUB}"),
            Some(&format!("Bearer {w}")),
        )
        .await;
        assert_eq!(r.status, 404);
    }

    #[tokio::test]
    async fn the_users_role_intersects_with_the_scope() {
        let st = store();
        // scope 允许 sites:write，但用户只是 viewer / editor：角色不允许，404
        let viewer = add_key(
            &st,
            1,
            1,
            "viewer",
            &["sites:write:*", "stats:read:*"],
            None,
        );
        let editor = add_key(&st, 2, 1, "editor", &["sites:write:*"], None);
        let admin = add_key(&st, 3, 1, "admin", &["sites:write:*"], None);
        let g = guard_with(st, limits(10_000, 60, 4));
        let app = app(&g);
        let put = |k: &str| {
            let app = app.clone();
            let k = k.to_owned();
            async move {
                call(
                    &app,
                    "PUT",
                    &format!("/settings/{SITE_A}"),
                    Some(&format!("Bearer {k}")),
                )
                .await
                .status
            }
        };
        assert_eq!(put(&viewer).await, 404);
        assert_eq!(put(&editor).await, 404);
        assert_eq!(put(&admin).await, 200);
        // 读统计所有角色都行
        assert_eq!(get_stats(&app, &viewer, SITE_A).await.status, 200);
    }

    #[tokio::test(start_paused = true)]
    async fn hourly_quota_exhaustion_is_429_with_retry_after_and_reset_headers() {
        let st = store();
        let key = add_key(&st, 1, 1, "owner", &["stats:read:*"], None);
        let g = guard_with(st, limits(3, 60, 4));
        let app = app(&g);
        for i in 0..3 {
            let r = get_stats(&app, &key, SITE_A).await;
            assert_eq!(r.status, 200, "第 {i} 个");
            assert_eq!(hdr(&r, "x-ratelimit-limit"), Some("3"));
            assert_eq!(
                hdr(&r, "x-ratelimit-remaining"),
                Some(&*(2 - i).to_string())
            );
        }
        let r = get_stats(&app, &key, SITE_A).await;
        assert_eq!(r.status, 429);
        assert_eq!(hdr(&r, "retry-after"), Some("3600"));
        assert_eq!(hdr(&r, "x-ratelimit-limit"), Some("3"));
        assert_eq!(hdr(&r, "x-ratelimit-remaining"), Some("0"));
        assert_eq!(hdr(&r, "x-ratelimit-reset"), Some("3600"));
        assert!(r.body.contains("rate_limited"));
        // 窗口过去后恢复
        tokio::time::advance(Duration::from_secs(3600)).await;
        assert_eq!(get_stats(&app, &key, SITE_A).await.status, 200);
    }

    #[tokio::test(start_paused = true)]
    async fn burst_bucket_rejects_a_spike_and_refills() {
        let st = store();
        let key = add_key(&st, 1, 1, "owner", &["stats:read:*"], None);
        // 3600/小时 = 1/秒，突发 2
        let g = guard_with(st, limits(3600, 2, 4));
        let app = app(&g);
        assert_eq!(get_stats(&app, &key, SITE_A).await.status, 200);
        assert_eq!(get_stats(&app, &key, SITE_A).await.status, 200);
        let r = get_stats(&app, &key, SITE_A).await;
        assert_eq!(r.status, 429);
        assert_eq!(hdr(&r, "retry-after"), Some("1"));
        tokio::time::advance(Duration::from_secs(1)).await;
        assert_eq!(get_stats(&app, &key, SITE_A).await.status, 200);
    }

    #[tokio::test(start_paused = true)]
    async fn the_quota_belongs_to_the_org_not_the_key_and_orgs_are_independent() {
        let st = store();
        let k1 = add_key(&st, 1, 1, "owner", &["stats:read:*"], None);
        let k2 = add_key(&st, 2, 1, "viewer", &["stats:read:*"], None); // 同组织的另一把
        let other_org = add_key(&st, 3, 2, "owner", &["stats:read:*"], None);
        let g = guard_with(st, limits(2, 60, 4));
        let app = app(&g);
        assert_eq!(get_stats(&app, &k1, SITE_A).await.status, 200);
        assert_eq!(get_stats(&app, &k2, SITE_A).await.status, 200);
        assert_eq!(
            get_stats(&app, &k1, SITE_A).await.status,
            429,
            "同组织共享配额"
        );
        assert_eq!(get_stats(&app, &k2, SITE_A).await.status, 429);
        assert_eq!(
            get_stats(&app, &other_org, SITE_X).await.status,
            200,
            "别的组织不受影响"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn the_organizations_own_limit_overrides_the_default() {
        let st = store();
        let key = add_key(&st, 1, 1, "owner", &["stats:read:*"], Some(1));
        let g = guard_with(st, limits(10_000, 60, 4));
        let app = app(&g);
        let r = get_stats(&app, &key, SITE_A).await;
        assert_eq!((r.status, hdr(&r, "x-ratelimit-limit")), (200, Some("1")));
        assert_eq!(get_stats(&app, &key, SITE_A).await.status, 429);
    }

    #[tokio::test(start_paused = true)]
    async fn concurrency_over_the_limit_queues_then_gets_429_with_retry_after() {
        let st = store();
        let key = add_key(&st, 1, 1, "owner", &["stats:read:*"], None);
        let g = guard_with(st, limits(100_000, 1000, 2));
        let gate = Arc::new(Semaphore::new(0));
        let slow = Router::new()
            .route("/slow", get(slow_handler))
            .with_state(Arc::clone(&gate));
        let app = protect(
            slow,
            Arc::clone(&g),
            Required::new(Resource::Stats, ScopeAction::Read),
        );
        let auth = format!("Bearer {key}");

        // 两个请求占满并发名额（处理函数卡在 gate 上）
        let mut held = Vec::new();
        for _ in 0..2 {
            let (app, auth) = (app.clone(), auth.clone());
            held.push(tokio::spawn(async move {
                call(&app, "GET", "/slow", Some(&auth)).await
            }));
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
        assert!(held.iter().all(|h| !h.is_finished()));

        // 第三个请求排队，2 秒内没有名额 → 429 + Retry-After
        let started = Instant::now();
        let r = call(&app, "GET", "/slow", Some(&auth)).await;
        assert_eq!(r.status, 429);
        assert_eq!(hdr(&r, "retry-after"), Some("2"));
        assert!(started.elapsed() >= Duration::from_secs(2));

        // 排队期间有名额释放：排队的请求拿到名额并成功
        let (app2, auth2) = (app.clone(), auth.clone());
        let queued = tokio::spawn(async move { call(&app2, "GET", "/slow", Some(&auth2)).await });
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert!(!queued.is_finished(), "还在排队");
        gate.add_permits(1); // 放行一个在途请求
        tokio::time::sleep(Duration::from_millis(10)).await;
        gate.add_permits(10); // 放行其余
        for h in held {
            assert_eq!(h.await.unwrap().status, 200);
        }
        assert_eq!(queued.await.unwrap().status, 200);
    }

    #[tokio::test(start_paused = true)]
    async fn last_used_at_is_written_at_most_once_per_interval() {
        let st = store();
        let key = add_key(&st, 7, 1, "owner", &["stats:read:*"], None);
        let g = guard_with(st, limits(100_000, 1000, 4));
        let app = app(&g);
        for _ in 0..5 {
            assert_eq!(get_stats(&app, &key, SITE_A).await.status, 200);
        }
        tokio::task::yield_now().await;
        tokio::time::sleep(Duration::from_millis(1)).await;
        assert_eq!(*g.store.touches.lock(), [7], "5 个请求只写一次库");
        tokio::time::advance(LAST_USED_INTERVAL - Duration::from_secs(1)).await;
        get_stats(&app, &key, SITE_A).await;
        tokio::time::sleep(Duration::from_millis(1)).await;
        assert_eq!(g.store.touches.lock().len(), 1, "间隔内不再写");
        tokio::time::advance(Duration::from_secs(2)).await;
        get_stats(&app, &key, SITE_A).await;
        tokio::time::sleep(Duration::from_millis(1)).await;
        assert_eq!(*g.store.touches.lock(), [7, 7], "过了间隔再写一次");
        // 失败的认证不写
        get_stats(&app, generate().plaintext(), SITE_A).await;
        tokio::time::sleep(Duration::from_millis(1)).await;
        assert_eq!(g.store.touches.lock().len(), 2);
    }

    #[tokio::test]
    async fn storage_failures_are_500_and_never_an_allow() {
        let mut st = store();
        st.fail = true;
        let good = generate();
        let g = guard_with(st, limits(10_000, 60, 4));
        let app = app(&g);
        let r = get_stats(&app, good.plaintext(), SITE_A).await;
        assert_eq!(r.status, 500);
        assert!(!r.body.contains("数据库"), "不泄露内部细节");
    }

    #[tokio::test]
    async fn a_handler_without_the_middleware_fails_closed() {
        let st = store();
        let g = guard_with(st, limits(10_000, 60, 4));
        let unprotected = Router::new()
            .route("/stats/{site_key}", get(stats_handler))
            .with_state(g);
        let r = call(&unprotected, "GET", &format!("/stats/{SITE_A}"), None).await;
        assert_eq!(r.status, 401);
    }
}
