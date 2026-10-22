//! 采集 HTTP、接入管线各阶段与富化（UA / 地理 / 来源 / 渠道）。P1 实现，见 docs/architecture/system-design.md §4。
//!
//! 不碰 IO 的纯函数：
//! - [`protocol`]：线协议解析与规范化（`tracker-and-protocol.md` §5–§7，管线第 1 步）；
//! - [`hostname`]：hostname 规范化与站点白名单匹配；
//! - [`client_ip`]：客户端 IP 判定（管线第 2 步）。
//!
//! [`pipeline`] 把它们串成接入管线（站点快照、盐缓存、会话分配，再交给 BatchWriter）：
//! `process` 本身没有 IO，只有 `SiteSnapshot` / `SaltCache` 的后台刷新任务会读 PG。

pub mod client_ip;
pub mod hostname;
mod page_url;
pub mod pipeline;
pub mod protocol;
