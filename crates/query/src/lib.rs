//! 统一查询内核（`docs/architecture/system-design.md` §6）。
//!
//! 一个 [`QuerySpec`] 进，一个 [`QueryResult`] 出；看板、公开 API 与 `rivu query` 都走同一条路径：
//!
//! ```text
//! JSON ─► spec::QuerySpec::parse        // 语法：deny_unknown_fields、封闭枚举，错误带字段路径
//!      ─► resolve::ResolvedQuery        // 注入站点与时区、UTC 边界、业务校验（构造函数只在 crate 内可见）
//!      ─► plan                          // 指标与维度 → 组件与表
//!      ─► sql                           // (String, Params)：只拼常量片段，取值一律 {name:Type} 参数，每个 FROM 带 TenantScope
//!      ─► exec                          // 选层（rollup：日汇总层，见 [`rollup`]）、log_comment、调用方档位的 max_execution_time；组装 results / meta / query
//! ```
//!
//! 不在本 crate 里的：HTTP 路由、鉴权、限流（调用方传入 [`TenantScope`]、[`SiteContext`] 与 [`ExecOptions`]）。
//!
//! 口径见各模块的文档：时间语义在 [`resolve`]，选表与组合在 [`plan`]，结果结构在 [`result`]，
//! 请求里每个指标在本次查询形态下的口径还会写进 `meta.metric_semantics`。
//!
//! 取值与表名：表名不带库名，调用方的客户端要用 `with_database` 绑定库（`rivu_storage_ch::client`）；
//! 用 `rivu_reader` 账号（`readonly=1`）。

pub mod error;
pub mod exec;
pub mod plan;
pub mod resolve;
pub mod result;
pub mod rollup;
pub mod spec;
pub mod sql;

#[cfg(test)]
mod sql_tests;

pub use error::QueryError;
pub use exec::{ExecOptions, execute, explain};
pub use resolve::{GoalDef, GoalKind, ResolvedQuery, SiteContext};
pub use result::QueryResult;
pub use spec::QuerySpec;
pub use sql::TenantScope;

/// `GET /api/v1/query/schema.json`：`QuerySpec` 的 JSON Schema（draft 2020-12），由 `schemars` 从 Rust 类型生成。
pub fn schema_json() -> serde_json::Value {
    spec::schema_json()
}
