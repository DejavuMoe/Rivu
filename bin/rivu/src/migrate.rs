//! `rivu migrate`：交织执行 PostgreSQL 与 ClickHouse 迁移。
//!
//! 两个目录的迁移文件合并后按版本号全局升序依次执行。每个库有自己的记录表
//! （PG `schema_migrations`，CH `<db>.schema_migrations`）；已应用集合是两者并集。

use std::collections::HashMap;
use std::fmt::Write as _;
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use rivu_common::config::Config;
use rivu_common::migration::{self, AppliedMigration, Migration, Store};

use crate::cli::{MigrateAction, MigrateArgs};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum State {
    Applied,
    Pending,
    /// 已应用，但文件内容和记录的校验和不一致
    ChecksumMismatch {
        recorded: String,
    },
}

#[derive(Debug, Clone)]
pub struct Entry {
    pub migration: Migration,
    pub state: State,
}

#[derive(Debug, Clone)]
pub struct Report {
    pub entries: Vec<Entry>,
    /// 数据库里有记录，但本二进制没有对应文件（例如回滚到了旧版本）
    pub unknown: Vec<(Store, AppliedMigration)>,
}

impl Report {
    pub fn pending(&self) -> Vec<&Migration> {
        self.entries
            .iter()
            .filter(|e| e.state == State::Pending)
            .map(|e| &e.migration)
            .collect()
    }

    pub fn mismatches(&self) -> Vec<&Entry> {
        self.entries
            .iter()
            .filter(|e| matches!(e.state, State::ChecksumMismatch { .. }))
            .collect()
    }

    /// 版本号小于「已应用最高版本」的待执行迁移（乱序）。
    pub fn out_of_order(&self) -> Vec<&Migration> {
        let max_applied = self
            .entries
            .iter()
            .filter(|e| e.state != State::Pending)
            .map(|e| e.migration.version)
            .max();
        match max_applied {
            None => Vec::new(),
            Some(max) => self
                .pending()
                .into_iter()
                .filter(|m| m.version < max)
                .collect(),
        }
    }

    /// 校验和不一致、记录不认识，这两类问题都必须阻止继续迁移。
    pub fn integrity_problems(&self) -> Vec<String> {
        let mut problems = Vec::new();
        for e in self.mismatches() {
            if let State::ChecksumMismatch { recorded } = &e.state {
                problems.push(format!(
                    "{}/{} 已应用，但文件内容被修改过（记录的 checksum {}…，当前 {}…）；已应用的迁移文件不可修改，请新增迁移",
                    e.migration.store,
                    e.migration.file,
                    &recorded[..recorded.len().min(12)],
                    &e.migration.checksum[..12],
                ));
            }
        }
        for (store, a) in &self.unknown {
            problems.push(format!(
                "{store} 里记录了版本 {}（{}），但本二进制没有对应的迁移文件；二进制版本可能比数据库旧",
                a.version, a.name
            ));
        }
        problems
    }
}

/// 把嵌入的迁移和两个库的已应用记录对照，得出每个版本的状态。纯函数，便于测试。
pub fn classify(
    migrations: &[Migration],
    applied_pg: &[AppliedMigration],
    applied_ch: &[AppliedMigration],
) -> Report {
    let mut recorded: HashMap<(Store, u64), &AppliedMigration> = HashMap::new();
    for a in applied_pg {
        recorded.insert((Store::Postgres, a.version), a);
    }
    for a in applied_ch {
        recorded.insert((Store::ClickHouse, a.version), a);
    }

    let entries = migrations
        .iter()
        .map(|m| {
            let state = match recorded.remove(&(m.store, m.version)) {
                None => State::Pending,
                Some(a) if a.checksum == m.checksum => State::Applied,
                Some(a) => State::ChecksumMismatch {
                    recorded: a.checksum.clone(),
                },
            };
            Entry {
                migration: m.clone(),
                state,
            }
        })
        .collect();

    let mut unknown: Vec<(Store, AppliedMigration)> = recorded
        .into_iter()
        .map(|((store, _), a)| (store, a.clone()))
        .collect();
    unknown.sort_by_key(|(store, a)| (a.version, *store));

    Report { entries, unknown }
}

/// 文本表格，`rivu migrate status` 的输出。
pub fn format_report(report: &Report) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "{:<16}{:<12}{:<28}STATE", "VERSION", "STORE", "NAME");
    for e in &report.entries {
        let state = match &e.state {
            State::Applied => "已应用".to_string(),
            State::Pending => "待执行".to_string(),
            State::ChecksumMismatch { .. } => "校验和不一致".to_string(),
        };
        let _ = writeln!(
            out,
            "{:<16}{:<12}{:<28}{}",
            e.migration.version, e.migration.store, e.migration.name, state
        );
    }
    for (store, a) in &report.unknown {
        let _ = writeln!(
            out,
            "{:<16}{:<12}{:<28}未知（数据库里有记录，二进制里没有文件）",
            a.version, store, a.name
        );
    }
    let pending = report.pending().len();
    let applied = report
        .entries
        .iter()
        .filter(|e| e.state == State::Applied)
        .count();
    let _ = writeln!(
        out,
        "共 {} 个迁移：{applied} 已应用，{pending} 待执行",
        report.entries.len()
    );
    out
}

/// 到两个库的连接。
struct Conns {
    pg: sqlx::PgPool,
    ch: clickhouse::Client,
    ch_database: String,
}

impl Conns {
    async fn connect(config: &Config) -> Result<Self> {
        let account = config
            .ch_migrator
            .as_ref()
            .context("缺少 ClickHouse migrator 账号")?;
        Ok(Conns {
            pg: rivu_storage_pg::connect(config.database_url.expose(), 2).await?,
            // 不绑定库：第一个 ClickHouse 迁移负责 CREATE DATABASE
            ch: rivu_storage_ch::client(&config.clickhouse_url, account, None),
            ch_database: config.ch_database.clone(),
        })
    }

    async fn report(&self) -> Result<Report> {
        let migrations = embedded()?;
        let applied_pg = rivu_storage_pg::applied_migrations(&self.pg).await?;
        let applied_ch = rivu_storage_ch::applied_migrations(&self.ch, &self.ch_database).await?;
        Ok(classify(&migrations, &applied_pg, &applied_ch))
    }
}

fn embedded() -> Result<Vec<Migration>> {
    migration::embedded().map_err(|errors| {
        let list: Vec<String> = errors.iter().map(|e| format!("  - {e}")).collect();
        anyhow::anyhow!("嵌入的迁移文件有问题：\n{}", list.join("\n"))
    })
}

/// 列出状态，不修改任何东西。
pub async fn status(config: &Config) -> Result<Report> {
    Conns::connect(config).await?.report().await
}

/// 应用全部待执行迁移，返回实际应用的迁移。幂等：全部已应用时返回空列表。
pub async fn apply(config: &Config, allow_out_of_order: bool) -> Result<Vec<Migration>> {
    let conns = Conns::connect(config).await?;
    let lock = rivu_storage_pg::MigrationLock::acquire(&conns.pg).await?;
    let result = apply_locked(&conns, allow_out_of_order).await;
    let released = lock.release().await;
    let applied = result?;
    released?;
    Ok(applied)
}

async fn apply_locked(conns: &Conns, allow_out_of_order: bool) -> Result<Vec<Migration>> {
    rivu_storage_pg::ensure_migrations_table(&conns.pg).await?;
    let report = conns.report().await?;

    let problems = report.integrity_problems();
    if !problems.is_empty() {
        bail!("迁移记录与文件不一致：\n  - {}", problems.join("\n  - "));
    }
    let out_of_order = report.out_of_order();
    if !out_of_order.is_empty() && !allow_out_of_order {
        let list: Vec<String> = out_of_order
            .iter()
            .map(|m| format!("{}/{}", m.store, m.file))
            .collect();
        bail!(
            "待执行的迁移版本小于已应用的最高版本（乱序）：{}；确认无误后加 --allow-out-of-order",
            list.join(", ")
        );
    }

    let pending: Vec<Migration> = report.pending().into_iter().cloned().collect();
    let mut applied = Vec::new();
    for m in pending {
        match m.store {
            Store::Postgres => rivu_storage_pg::apply_migration(&conns.pg, &m).await?,
            Store::ClickHouse => {
                rivu_storage_ch::apply_migration(&conns.ch, &conns.ch_database, &m).await?
            }
        }
        println!("已应用 {} {}/{}", m.version, m.store, m.name);
        applied.push(m);
    }
    Ok(applied)
}

/// `rivu migrate [status]`
pub async fn run_command(config: &Config, args: MigrateArgs) -> Result<ExitCode> {
    match args.action {
        Some(MigrateAction::Status) => {
            let report = status(config).await?;
            print!("{}", format_report(&report));
            let problems = report.integrity_problems();
            if problems.is_empty() {
                Ok(ExitCode::SUCCESS)
            } else {
                for p in problems {
                    eprintln!("问题: {p}");
                }
                Ok(ExitCode::FAILURE)
            }
        }
        None => {
            let applied = apply(config, args.allow_out_of_order).await?;
            if applied.is_empty() {
                println!("没有待执行的迁移");
            } else {
                println!("完成：本次应用了 {} 个迁移", applied.len());
            }
            Ok(ExitCode::SUCCESS)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rivu_common::migration::{SourceFile, merge};

    fn migrations() -> Vec<Migration> {
        merge([
            SourceFile {
                store: Store::Postgres,
                file_name: "20260101000000_a.sql",
                content: "A",
            },
            SourceFile {
                store: Store::ClickHouse,
                file_name: "20260101000100_b.sql",
                content: "B",
            },
            SourceFile {
                store: Store::Postgres,
                file_name: "20260101000200_c.sql",
                content: "C",
            },
        ])
        .unwrap()
    }

    fn rec(m: &Migration) -> AppliedMigration {
        AppliedMigration {
            version: m.version,
            name: m.name.clone(),
            checksum: m.checksum.clone(),
        }
    }

    #[test]
    fn fresh_databases_have_everything_pending() {
        let ms = migrations();
        let r = classify(&ms, &[], &[]);
        assert_eq!(r.pending().len(), 3);
        assert!(r.integrity_problems().is_empty());
        assert!(r.out_of_order().is_empty());
    }

    #[test]
    fn applied_set_is_union_of_both_stores() {
        let ms = migrations();
        let r = classify(&ms, &[rec(&ms[0]), rec(&ms[2])], &[rec(&ms[1])]);
        assert!(r.pending().is_empty());
        assert!(format_report(&r).contains("0 待执行"));
    }

    #[test]
    fn changed_checksum_is_reported() {
        let ms = migrations();
        let mut bad = rec(&ms[0]);
        bad.checksum = "0".repeat(64);
        let r = classify(&ms, &[bad], &[]);
        assert_eq!(r.mismatches().len(), 1);
        let problems = r.integrity_problems();
        assert_eq!(problems.len(), 1);
        assert!(problems[0].contains("20260101000000_a.sql"));
    }

    #[test]
    fn same_version_in_the_wrong_store_is_not_a_match() {
        // 版本号只在各自库的记录里匹配：CH 记录里出现 PG 文件的版本，只算「未知」
        let ms = migrations();
        let r = classify(&ms, &[], &[rec(&ms[0])]);
        assert_eq!(r.pending().len(), 3);
        assert_eq!(r.unknown.len(), 1);
        assert!(!r.integrity_problems().is_empty());
    }

    #[test]
    fn unknown_applied_version_is_reported() {
        let ms = migrations();
        let ghost = AppliedMigration {
            version: 20990101000000,
            name: "from_the_future".into(),
            checksum: "x".into(),
        };
        let r = classify(&ms, &[ghost], &[]);
        assert_eq!(r.unknown.len(), 1);
        assert_eq!(r.integrity_problems().len(), 1);
    }

    #[test]
    fn pending_below_max_applied_is_out_of_order() {
        let ms = migrations();
        // 最后一个已应用，中间的 CH 迁移还没应用
        let r = classify(&ms, &[rec(&ms[2])], &[]);
        let ooo = r.out_of_order();
        assert_eq!(ooo.len(), 2); // a 和 b 都小于 c
        // 按顺序应用到一半停下：a 已应用，b、c 待执行 -> 不算乱序
        let r = classify(&ms, &[rec(&ms[0])], &[]);
        assert!(r.out_of_order().is_empty());
    }
}
