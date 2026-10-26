//! ClickHouse 访问（官方 `clickhouse` crate，HTTP + RowBinary + LZ4）：客户端、迁移记录表与迁移执行。

pub mod diagnostics;
pub mod rows;
pub mod sessions_warm;

use anyhow::{Context, Result};
use clickhouse::{Client, Compression, Row};
use rivu_common::config::ChAccount;
use rivu_common::migration::{AppliedMigration, Migration};
use serde::Deserialize;

/// 构造客户端。`database` 为 `None` 时不绑定库（迁移里要先 `CREATE DATABASE`）。
///
/// readonly=1 的账号（`rivu_reader`）不能修改 setting，所以这里不设置任何 setting，
/// 并关闭压缩（压缩会带上 `enable_http_compression`）；写入方在各自的 INSERT 上用 `with_setting`。
pub fn client(url: &str, account: &ChAccount, database: Option<&str>) -> Client {
    let mut c = Client::default()
        .with_url(url)
        .with_user(&account.user)
        .with_password(account.password.expose())
        .with_compression(Compression::None);
    if let Some(db) = database {
        c = c.with_database(db);
    }
    c
}

/// 写入账号（`rivu_writer`）的客户端：请求体用 LZ4 压缩（RowBinary + LZ4，`system-design.md` §4.7）。
/// writer 账号不是 readonly，可以压缩；reader 账号不能，见 [`client`]。
pub fn writer_client(url: &str, account: &ChAccount, database: &str) -> Client {
    client(url, account, Some(database)).with_compression(Compression::Lz4)
}

/// `SELECT 1`。
pub async fn ping(client: &Client) -> Result<()> {
    let v: u8 = client
        .query("SELECT 1")
        .fetch_one()
        .await
        .context("ClickHouse SELECT 1 失败")?;
    anyhow::ensure!(v == 1, "ClickHouse SELECT 1 返回了 {v}");
    Ok(())
}

/// 把迁移文件按「行尾的 `;`」拆成多条语句（ClickHouse 的 HTTP 接口一次只执行一条）。
///
/// 规则：
/// - 整行注释（`--` 开头）被丢弃；
/// - 一行去掉行尾 `-- 注释`（引号之外）并去掉尾部空白后，以 `;` 结尾就结束当前语句；
/// - 结尾的 `;` 不包含在返回的语句里；只含空白的语句被跳过；
/// - 不处理 `/* */` 块注释和跨行字符串里恰好出现在行尾的 `;`，迁移文件里避免这样写。
pub fn split_statements(sql: &str) -> Vec<String> {
    let mut statements = Vec::new();
    let mut current = String::new();
    for line in sql.lines() {
        if line.trim_start().starts_with("--") {
            continue;
        }
        let code = strip_trailing_comment(line);
        let trimmed = code.trim_end();
        if let Some(body) = trimmed.strip_suffix(';') {
            current.push_str(body);
            push_statement(&mut statements, &mut current);
        } else {
            current.push_str(code);
            current.push('\n');
        }
    }
    push_statement(&mut statements, &mut current);
    statements
}

fn push_statement(statements: &mut Vec<String>, current: &mut String) {
    let stmt = current.trim();
    if !stmt.is_empty() {
        statements.push(stmt.to_string());
    }
    current.clear();
}

/// 去掉一行末尾的 `-- 注释`，忽略单引号字符串里的 `--`。
fn strip_trailing_comment(line: &str) -> &str {
    let bytes = line.as_bytes();
    let mut in_quote = false;
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' if in_quote => i += 1, // 转义字符，跳过下一个字节
            b'\'' => in_quote = !in_quote,
            b'-' if !in_quote && bytes.get(i + 1) == Some(&b'-') => return &line[..i],
            _ => {}
        }
        i += 1;
    }
    line
}

fn render(sql: &str, database: &str) -> String {
    sql.replace("{{database}}", database)
}

#[derive(Row, Deserialize)]
struct AppliedRow {
    version: u64,
    name: String,
    checksum: String,
}

async fn table_exists(client: &Client, database: &str) -> Result<bool> {
    let n: u64 = client
        .query(
            "SELECT count() FROM system.tables WHERE database = ? AND name = 'schema_migrations'",
        )
        .bind(database)
        .fetch_one()
        .await
        .context("检查 schema_migrations 是否存在失败")?;
    Ok(n > 0)
}

/// 已应用的迁移。库或表还不存在时返回空列表（全新实例）。
pub async fn applied_migrations(client: &Client, database: &str) -> Result<Vec<AppliedMigration>> {
    if !table_exists(client, database).await? {
        return Ok(Vec::new());
    }
    let rows = client
        .query(&format!(
            "SELECT version, name, checksum FROM {database}.schema_migrations FINAL ORDER BY version"
        ))
        .fetch_all::<AppliedRow>()
        .await
        .context("读取 schema_migrations 失败")?;
    Ok(rows
        .into_iter()
        .map(|r| AppliedMigration {
            version: r.version,
            name: r.name,
            checksum: r.checksum,
        })
        .collect())
}

/// 已应用的最高版本（`/readyz` 用）。
pub async fn max_applied_version(client: &Client, database: &str) -> Result<u64> {
    client
        .query(&format!(
            "SELECT max(version) FROM {database}.schema_migrations"
        ))
        .fetch_one::<u64>()
        .await
        .context("读取 schema_migrations 最高版本失败")
}

/// 逐条执行迁移里的语句，然后写入记录。ClickHouse 没有事务，所以迁移语句必须幂等。
pub async fn apply_migration(client: &Client, database: &str, migration: &Migration) -> Result<()> {
    let sql = render(&migration.sql, database);
    for (i, stmt) in split_statements(&sql).iter().enumerate() {
        client.query_raw(stmt).execute().await.with_context(|| {
            format!(
                "执行 clickhouse/{} 的第 {} 条语句失败",
                migration.file,
                i + 1
            )
        })?;
    }

    // 第一个迁移才创建库，所以记录表放在语句执行之后再建。
    client
        .query(&format!(
            "CREATE TABLE IF NOT EXISTS {database}.schema_migrations
             (
                 version    UInt64,
                 name       String,
                 checksum   String,
                 applied_at DateTime64(3, 'UTC') DEFAULT now64(3)
             )
             ENGINE = ReplacingMergeTree
             ORDER BY version"
        ))
        .execute()
        .await
        .context("创建 schema_migrations 失败")?;
    client
        .query(&format!(
            "INSERT INTO {database}.schema_migrations (version, name, checksum) \
             SELECT {{v:UInt64}}, {{n:String}}, {{c:String}}"
        ))
        .param("v", migration.version)
        .param("n", migration.name.as_str())
        .param("c", migration.checksum.as_str())
        .with_setting("async_insert", "0")
        .execute()
        .await
        .context("写入 schema_migrations 失败")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_on_line_ending_semicolons() {
        let sql = "CREATE DATABASE IF NOT EXISTS rivu;\n\nCREATE TABLE t\n(\n  a UInt8\n)\nENGINE = MergeTree\nORDER BY a;\n";
        let stmts = split_statements(sql);
        assert_eq!(stmts.len(), 2);
        assert_eq!(stmts[0], "CREATE DATABASE IF NOT EXISTS rivu");
        assert!(stmts[1].starts_with("CREATE TABLE t"));
        assert!(stmts[1].ends_with("ORDER BY a"));
        assert!(!stmts[1].contains(';'));
    }

    #[test]
    fn semicolons_inside_a_line_do_not_split() {
        let stmts = split_statements("SELECT 'a;b', 2; SELECT 3\nFROM x;");
        // 第一行的 `;` 不在行尾（后面还有内容）
        assert_eq!(stmts, vec!["SELECT 'a;b', 2; SELECT 3\nFROM x"]);
    }

    #[test]
    fn comments_are_dropped_and_do_not_hide_terminators() {
        let sql = "-- 整行注释;\nSELECT 1; -- 行尾注释\n-- 另一条注释\nSELECT 2 -- 没有分号\n;\n";
        assert_eq!(split_statements(sql), vec!["SELECT 1", "SELECT 2"]);
    }

    #[test]
    fn dashes_inside_strings_are_not_comments() {
        let stmts = split_statements("SELECT '--x';\n");
        assert_eq!(stmts, vec!["SELECT '--x'"]);
    }

    #[test]
    fn trailing_statement_without_semicolon_is_kept() {
        assert_eq!(
            split_statements("SELECT 1;\nSELECT 2"),
            vec!["SELECT 1", "SELECT 2"]
        );
    }

    #[test]
    fn empty_and_comment_only_input_yields_nothing() {
        assert!(split_statements("").is_empty());
        assert!(split_statements("-- nothing\n\n;\n  ;\n").is_empty());
    }

    #[test]
    fn handles_crlf() {
        assert_eq!(
            split_statements("SELECT 1;\r\nSELECT 2;\r\n"),
            vec!["SELECT 1", "SELECT 2"]
        );
    }

    #[test]
    fn embedded_clickhouse_migrations_split_into_expected_counts() {
        let all = rivu_common::migration::embedded().unwrap();
        for m in all
            .iter()
            .filter(|m| m.store == rivu_common::migration::Store::ClickHouse)
        {
            let stmts = split_statements(&render(&m.sql, "rivu"));
            assert!(!stmts.is_empty(), "{} 没有语句", m.file);
            for s in &stmts {
                assert!(!s.contains("{{"), "{} 里有未替换的占位符", m.file);
            }
        }
    }

    #[test]
    fn database_placeholder_is_substituted() {
        assert_eq!(
            render("CREATE DATABASE {{database}}", "x"),
            "CREATE DATABASE x"
        );
    }
}
