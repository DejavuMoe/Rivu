//! `rivu` 二进制的实现：子命令、角色启动流程、内部/公共 HTTP 端口与迁移执行器。

pub mod admin;
pub mod api_auth;
pub mod apikey_cmd;
pub mod cli;
pub mod dash_api;
pub mod internal;
pub mod migrate;
pub mod public;
pub mod query_api;
pub mod run;
pub mod script;
pub mod shield_cmd;
#[cfg(test)]
mod testutil;
pub mod user_cmd;
