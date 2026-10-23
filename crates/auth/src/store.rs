//! 鉴权需要的存储访问。抽成 trait 是为了让鉴权与限流链路的测试不依赖 PostgreSQL；
//! 生产用 [`PgStore`]。

use std::future::Future;

use anyhow::Result;
use rivu_storage_pg::api_keys::KeyRecord;
use rivu_storage_pg::sites::SiteOwner;
use sqlx::PgPool;

/// 鉴权用到的三个存储操作。
pub trait AuthStore: Send + Sync + 'static {
    /// 按 SHA-256 摘要查找**当前有效**的 key：未吊销、未过期，所属组织未删除，所属用户未禁用且仍是组织成员。
    /// 这些失效条件都在存储层判定（`platform-and-ops.md` §4.3 要求立即失效），所以每个请求都查一次，不做正向缓存。
    fn find_key(&self, hash: &[u8; 32]) -> impl Future<Output = Result<Option<KeyRecord>>> + Send;

    /// 按 `site_key` 查未软删除的站点归属。
    fn find_site(&self, site_key: &str) -> impl Future<Output = Result<Option<SiteOwner>>> + Send;

    /// 写 `last_used_at`。调用方负责节流。
    fn touch_last_used(&self, key_id: i64) -> impl Future<Output = Result<()>> + Send;
}

/// PostgreSQL 实现。
#[derive(Clone)]
pub struct PgStore {
    pool: PgPool,
}

impl PgStore {
    pub fn new(pool: PgPool) -> Self {
        PgStore { pool }
    }
}

impl AuthStore for PgStore {
    async fn find_key(&self, hash: &[u8; 32]) -> Result<Option<KeyRecord>> {
        rivu_storage_pg::api_keys::find_active_by_hash(&self.pool, hash).await
    }

    async fn find_site(&self, site_key: &str) -> Result<Option<SiteOwner>> {
        rivu_storage_pg::sites::find_site_owner(&self.pool, site_key).await
    }

    async fn touch_last_used(&self, key_id: i64) -> Result<()> {
        rivu_storage_pg::api_keys::touch_last_used(&self.pool, key_id).await
    }
}
