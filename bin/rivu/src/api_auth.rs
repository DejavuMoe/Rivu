//! 公开 API 的鉴权与限流装配：把配置（`RIVU_API_RATE_PER_HOUR` 等）和 PG 连接池接成 [`ApiGuard`]。
//!
//! 公开查询路由（`POST /api/v1/query` 等）挂载时：
//!
//! ```ignore
//! let guard = rivu::api_auth::build_guard(&config, pg.clone());
//! let routes = Router::new().route("/api/v1/query", post(query)).with_state(Arc::clone(&guard));
//! let routes = rivu_auth::protect(routes, guard, Required::new(Resource::Stats, ScopeAction::Read));
//! ```
//!
//! 处理函数里用 `AuthContext` 提取器取身份，再 `guard.authorize_site(&ctx, site_key, ..)` 判定站点访问。

use std::sync::Arc;

use rivu_auth::{ApiGuard, PgStore};
use rivu_common::config::{ApiRateConfig, Config};
use rivu_ratelimit::ApiLimits;
use sqlx::PgPool;

/// 配置里的限流参数转成限流器的参数。
pub fn api_limits(cfg: &ApiRateConfig) -> ApiLimits {
    ApiLimits {
        default_hourly: cfg.hourly_default,
        burst: cfg.burst,
        max_concurrency: cfg.max_concurrency as usize,
        queue_timeout: cfg.queue_timeout,
    }
}

/// 用 PostgreSQL 做存储、按配置限流的守卫。进程里只建一个并共享（限流状态在它里面）。
pub fn build_guard(config: &Config, pool: PgPool) -> Arc<ApiGuard<PgStore>> {
    Arc::new(ApiGuard::new(
        PgStore::new(pool),
        api_limits(&config.api_rate),
    ))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn config_maps_onto_the_limiter_parameters() {
        let l = api_limits(&ApiRateConfig {
            hourly_default: 500,
            burst: 5,
            max_concurrency: 2,
            queue_timeout: Duration::from_millis(250),
        });
        assert_eq!(
            (
                l.default_hourly,
                l.burst,
                l.max_concurrency,
                l.queue_timeout
            ),
            (500, 5, 2, Duration::from_millis(250))
        );
        // 默认值与文档一致：10000 / 60 / 并发 4
        let d = api_limits(&ApiRateConfig::default());
        assert_eq!(
            (d.default_hourly, d.burst, d.max_concurrency),
            (10_000, 60, 4)
        );
        assert_eq!(d, ApiLimits::default());
    }
}
