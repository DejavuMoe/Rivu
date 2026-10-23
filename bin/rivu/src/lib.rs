//! `rivu` 二进制的实现：子命令、角色启动流程、内部/公共 HTTP 端口与迁移执行器。

pub mod admin;
pub mod cli;
pub mod internal;
pub mod migrate;
pub mod public;
pub mod run;
pub mod script;
#[cfg(test)]
mod testutil;
