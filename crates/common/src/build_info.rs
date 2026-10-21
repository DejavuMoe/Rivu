//! 构建信息。
//!
//! git commit 由构建环境通过环境变量 `RIVU_GIT_COMMIT` 注入（CI、Dockerfile 里设置），
//! build.rs 不会去调用 git；没有注入时显示 `unknown`。

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

pub const GIT_COMMIT: &str = match option_env!("RIVU_GIT_COMMIT") {
    Some(commit) => commit,
    None => "unknown",
};

/// `rivu --version` 的输出：`0.1.0 (commit abc1234)`。
pub fn long_version() -> String {
    format!("{VERSION} (commit {GIT_COMMIT})")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_version_contains_both_parts() {
        let v = long_version();
        assert!(v.starts_with(VERSION));
        assert!(v.contains(GIT_COMMIT));
    }
}
