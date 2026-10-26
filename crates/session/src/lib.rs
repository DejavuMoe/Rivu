//! 访客身份与 SessionAssigner：给事件分配 `session_id` 并盖章。
//! 设计见 `docs/architecture/system-design.md` §4.5（身份）与 §4.6（SessionAssigner）。
//!
//! 本 crate 是纯逻辑：不碰 HTTP、不碰数据库，临界区内没有 IO（[`snapshot`] 的文件读写在关停与启动时进行）。
//! 正确性由 `crates/reference`（独立的参考实现）做差分测试判定，见 `tests/`。

pub mod assigner;
pub mod identity;
pub mod snapshot;
