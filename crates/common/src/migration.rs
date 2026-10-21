//! 迁移文件模型：文件名解析、校验和、两个库的文件合并并按版本号全局排序。
//!
//! 借鉴 Plausible 的 interweave_migrate：PostgreSQL 与 ClickHouse 的迁移共用一个版本序列，
//! 因为跨库之间会有先后依赖。文件放在 `migrations/postgres/` 和 `migrations/clickhouse/`，
//! 命名 `<14 位时间戳版本>_<name>.sql`，编译期嵌入二进制。

use std::collections::BTreeMap;
use std::fmt;

use include_dir::{Dir, include_dir};
use sha2::{Digest, Sha256};

static EMBEDDED: Dir<'static> = include_dir!("$CARGO_MANIFEST_DIR/../../migrations");

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Store {
    Postgres,
    ClickHouse,
}

impl Store {
    pub fn dir_name(self) -> &'static str {
        match self {
            Store::Postgres => "postgres",
            Store::ClickHouse => "clickhouse",
        }
    }
}

impl fmt::Display for Store {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad(self.dir_name())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Migration {
    pub version: u64,
    pub name: String,
    pub store: Store,
    /// 文件名，例如 `20261021100000_core_tables.sql`
    pub file: String,
    pub sql: String,
    /// 内容的 SHA-256（十六进制，换行符按 LF 归一化后计算）
    pub checksum: String,
}

/// 迁移记录表里的一行。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppliedMigration {
    pub version: u64,
    pub name: String,
    pub checksum: String,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MigrationError {
    #[error(
        "{store}/{file}: 文件名必须是 `<14 位时间戳>_<name>.sql`（name 只含小写字母、数字、下划线）"
    )]
    InvalidFileName { store: Store, file: String },
    #[error("版本号 {version} 重复：{first} 和 {second}")]
    DuplicateVersion {
        version: u64,
        first: String,
        second: String,
    },
    #[error("{store}/{file}: 文件不是有效的 UTF-8")]
    NotUtf8 { store: Store, file: String },
}

/// 解析 `<14 位时间戳>_<name>.sql`，返回 (版本, name)。
pub fn parse_file_name(file: &str) -> Option<(u64, String)> {
    let stem = file.strip_suffix(".sql")?;
    let (version, name) = stem.split_once('_')?;
    if version.len() != 14 || !version.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    if name.is_empty()
        || !name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
    {
        return None;
    }
    Some((version.parse().ok()?, name.to_string()))
}

/// 内容校验和。CRLF 归一化为 LF，避免 Windows 上 `core.autocrlf` 改变校验和。
pub fn checksum(content: &str) -> String {
    let normalized = content.replace("\r\n", "\n");
    let digest = Sha256::digest(normalized.as_bytes());
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// 输入：某个库目录下的一个文件。
#[derive(Debug, Clone, Copy)]
pub struct SourceFile<'a> {
    pub store: Store,
    pub file_name: &'a str,
    pub content: &'a str,
}

/// 合并两个目录的文件，按版本号全局升序排序。收集全部错误，不在第一个错误处停止。
/// 不以 `.sql` 结尾的文件（README、.gitkeep）被忽略。
pub fn merge<'a>(
    files: impl IntoIterator<Item = SourceFile<'a>>,
) -> Result<Vec<Migration>, Vec<MigrationError>> {
    let mut errors = Vec::new();
    let mut by_version: BTreeMap<u64, Migration> = BTreeMap::new();

    for f in files {
        if !f.file_name.ends_with(".sql") {
            continue;
        }
        let Some((version, name)) = parse_file_name(f.file_name) else {
            errors.push(MigrationError::InvalidFileName {
                store: f.store,
                file: f.file_name.to_string(),
            });
            continue;
        };
        let migration = Migration {
            version,
            name,
            store: f.store,
            file: f.file_name.to_string(),
            sql: f.content.to_string(),
            checksum: checksum(f.content),
        };
        if let Some(existing) = by_version.get(&version) {
            errors.push(MigrationError::DuplicateVersion {
                version,
                first: format!("{}/{}", existing.store, existing.file),
                second: format!("{}/{}", f.store, f.file_name),
            });
        } else {
            by_version.insert(version, migration);
        }
    }

    if errors.is_empty() {
        Ok(by_version.into_values().collect())
    } else {
        Err(errors)
    }
}

/// 编译期嵌入的全部迁移，已合并并排序。
pub fn embedded() -> Result<Vec<Migration>, Vec<MigrationError>> {
    let mut errors = Vec::new();
    let mut sources: Vec<(Store, String, String)> = Vec::new();
    for store in [Store::Postgres, Store::ClickHouse] {
        let Some(dir) = EMBEDDED.get_dir(store.dir_name()) else {
            continue;
        };
        for file in dir.files() {
            let file_name = file
                .path()
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            match file.contents_utf8() {
                Some(content) => sources.push((store, file_name, content.to_string())),
                None => errors.push(MigrationError::NotUtf8 {
                    store,
                    file: file_name,
                }),
            }
        }
    }
    let merged = merge(
        sources
            .iter()
            .map(|(store, file_name, content)| SourceFile {
                store: *store,
                file_name,
                content,
            }),
    );
    match merged {
        Ok(m) if errors.is_empty() => Ok(m),
        Ok(_) => Err(errors),
        Err(mut e) => {
            errors.append(&mut e);
            Err(errors)
        }
    }
}

/// 某个库在编译期的最高迁移版本。
pub fn highest_version(migrations: &[Migration], store: Store) -> Option<u64> {
    migrations
        .iter()
        .filter(|m| m.store == store)
        .map(|m| m.version)
        .max()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn src(store: Store, file_name: &'static str) -> SourceFile<'static> {
        SourceFile {
            store,
            file_name,
            content: "SELECT 1;",
        }
    }

    #[test]
    fn parses_valid_names() {
        assert_eq!(
            parse_file_name("20261021100000_core_tables.sql"),
            Some((20261021100000, "core_tables".to_string()))
        );
        assert_eq!(
            parse_file_name("00000000000001_a1.sql"),
            Some((1, "a1".to_string()))
        );
    }

    #[test]
    fn rejects_invalid_names() {
        for bad in [
            "2026102110000_short.sql",     // 13 位
            "202610211000000_long.sql",    // 15 位
            "20261021100000.sql",          // 没有 name
            "20261021100000_.sql",         // name 为空
            "20261021100000_Upper.sql",    // 大写
            "20261021100000_has-dash.sql", // 连字符
            "2026102110000a_x.sql",        // 非数字
            "20261021100000_x.txt",        // 扩展名
            "abc.sql",
        ] {
            assert_eq!(parse_file_name(bad), None, "{bad} 应当被拒绝");
        }
    }

    #[test]
    fn merges_and_sorts_globally_across_stores() {
        let merged = merge([
            src(Store::ClickHouse, "20261021100300_c.sql"),
            src(Store::Postgres, "20261021100400_d.sql"),
            src(Store::Postgres, "20261021100000_a.sql"),
            src(Store::ClickHouse, "20261021100100_b.sql"),
        ])
        .unwrap();
        let order: Vec<(u64, Store)> = merged.iter().map(|m| (m.version, m.store)).collect();
        assert_eq!(
            order,
            vec![
                (20261021100000, Store::Postgres),
                (20261021100100, Store::ClickHouse),
                (20261021100300, Store::ClickHouse),
                (20261021100400, Store::Postgres),
            ]
        );
        assert_eq!(
            highest_version(&merged, Store::Postgres),
            Some(20261021100400)
        );
        assert_eq!(
            highest_version(&merged, Store::ClickHouse),
            Some(20261021100300)
        );
    }

    #[test]
    fn duplicate_version_across_stores_is_an_error() {
        let err = merge([
            src(Store::Postgres, "20261021100000_a.sql"),
            src(Store::ClickHouse, "20261021100000_b.sql"),
        ])
        .unwrap_err();
        assert_eq!(err.len(), 1);
        assert!(matches!(
            &err[0],
            MigrationError::DuplicateVersion {
                version: 20261021100000,
                ..
            }
        ));
    }

    #[test]
    fn duplicate_version_within_a_store_is_an_error() {
        let err = merge([
            src(Store::Postgres, "20261021100000_a.sql"),
            src(Store::Postgres, "20261021100000_b.sql"),
        ])
        .unwrap_err();
        assert!(matches!(&err[0], MigrationError::DuplicateVersion { .. }));
    }

    #[test]
    fn all_errors_are_collected_and_non_sql_files_ignored() {
        let err = merge([
            src(Store::Postgres, "bad.sql"),
            src(Store::Postgres, "README.md"),
            src(Store::ClickHouse, "20261021100000_a.sql"),
            src(Store::ClickHouse, "20261021100000_b.sql"),
        ])
        .unwrap_err();
        assert_eq!(err.len(), 2, "{err:?}");
    }

    #[test]
    fn checksum_is_stable_and_ignores_line_endings() {
        assert_eq!(checksum("a\nb\n"), checksum("a\r\nb\r\n"));
        assert_ne!(checksum("a\nb\n"), checksum("a\nc\n"));
        assert_eq!(checksum("").len(), 64);
        // SHA-256("") 的已知值
        assert_eq!(
            checksum(""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn embedded_migrations_are_valid() {
        let migrations = embedded().expect("嵌入的迁移文件必须合法、版本不重复");
        assert!(highest_version(&migrations, Store::Postgres).is_some());
        assert!(highest_version(&migrations, Store::ClickHouse).is_some());
        // 已排序且版本严格递增
        assert!(migrations.windows(2).all(|w| w[0].version < w[1].version));
    }
}
