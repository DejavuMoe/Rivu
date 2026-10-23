//! 公开 API 的 API key 鉴权与按组织限流（`platform-and-ops.md` §4、§5，`system-design.md` §6.8）。
//!
//! 可复用的组件，公开查询路由由别处挂载：
//!
//! - [`key`]：key 的格式（`rivu_` + 32 字节随机数的 base64url）、生成、SHA-256 哈希、常数时间比较、`Authorization` 解析；
//! - [`scope`]：scope（`stats:read:*`）、角色权限矩阵；
//! - [`guard`]：[`ApiGuard`]（存储 + 限流器 + `last_used_at` 节流）与 axum 中间件 [`guard_middleware`] / [`protect`]：
//!   `Authorization: Bearer` → 401 → 按组织限流 429 → 作用域 403 → 把 [`AuthContext`] 放进请求扩展，响应带 `X-RateLimit-*`；
//!   处理函数里用 [`ApiGuard::authorize_site`] 做站点访问判定（无权限一律 404，不泄露站点是否存在）；
//! - [`password`]（argon2id）、[`session`]（登录会话令牌与 cookie）、[`csrf`]（内部端点的 CSRF 判定）：
//!   看板的会话鉴权用，HTTP 处理在 `bin/rivu` 的 `dash_api`；
//! - [`store`]：存储抽象（[`AuthStore`]）与 PostgreSQL 实现（[`PgStore`]）。
//!
//! 检查顺序：401（缺失、格式错、未知、吊销、过期、用户被禁用或被移出组织）→ 429（小时配额、突发令牌桶、并发排队超时）
//! → 403（key 根本没有这个资源与动作的 scope）→ 404（站点不存在、属于别的组织、或 scope 目标不含该站点）。
//! 限流在 scope 与站点校验之前，这样用错误请求反复试探也要消耗配额（Plausible 同样先限流，`03-stats-query-engine.md` §1.2）。

pub mod csrf;
pub mod guard;
pub mod identity;
pub mod key;
pub mod password;
pub mod scope;
pub mod session;
pub mod store;

pub use guard::{ApiError, ApiGuard, AuthContext, GuardState, Required, guard_middleware, protect};
pub use scope::{Action, OrgRole, Resource, Scope, ScopeAction, Scopes, Target};
pub use store::{AuthStore, PgStore};
