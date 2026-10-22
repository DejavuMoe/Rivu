//! 集成测试共用的小工具。各测试文件只用其中一部分，所以允许 dead_code。
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

/// MaxMind 官方测试数据（见 `tests/data/SOURCE.md`）。
pub fn data(rel: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("data")
        .join(rel)
}

pub fn test_mmdb(name: &str) -> PathBuf {
    data(&format!("test-data/{name}.mmdb"))
}

/// 退出时删除的临时目录。
pub struct TempDir(PathBuf);

impl TempDir {
    pub fn new(tag: &str) -> Self {
        static N: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "rivu-geo-{tag}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        TempDir(dir)
    }

    pub fn path(&self) -> &Path {
        &self.0
    }

    pub fn join(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// 读取 mmdb 的 `build_epoch`。
pub fn build_epoch(path: &Path) -> u64 {
    maxminddb::Reader::open_readfile(path)
        .unwrap()
        .metadata()
        .build_epoch
}
