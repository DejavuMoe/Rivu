//! PostgreSQL 访问（sqlx）：连接池、迁移记录表与迁移执行，以及站点与盐。

/// SQL 片段：把 timestamptz 列格式化成 UTC、毫秒精度的 RFC 3339 文本（`concat!` 里用，保持 `&'static str`）。
macro_rules! ts_utc {
    ($col:literal) => {
        concat!(
            "to_char(",
            $col,
            r#" AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"')"#
        )
    };
}

/// 同 [`ts_utc`]，但秒精度（`2026-10-26T08:00:00Z`，成员与 API key 的看板端点用）。
macro_rules! ts_sec {
    ($col:literal) => {
        concat!(
            "to_char(",
            $col,
            r#" AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS"Z"')"#
        )
    };
}

pub mod access;
pub mod api_keys;
pub mod goals;
pub mod imports;
pub mod members;
pub mod salts;
pub mod sessions;
pub mod shared_links;
pub mod shield;
pub mod sites;
pub mod stats_deletions;
pub mod users;

use std::time::Duration;

use anyhow::{Context, Result};
use rivu_common::migration::{AppliedMigration, Migration};
use sqlx::pool::PoolConnection;
use sqlx::postgres::PgPoolOptions;
use sqlx::{AssertSqlSafe, PgPool, Postgres, Row};

/// 迁移记录表。结构由设计固定：`schema_migrations(version bigint primary key, name text, checksum text, applied_at timestamptz)`。
const CREATE_MIGRATIONS_TABLE: &str = "CREATE TABLE IF NOT EXISTS schema_migrations (
    version    bigint PRIMARY KEY,
    name       text NOT NULL,
    checksum   text NOT NULL,
    applied_at timestamptz NOT NULL DEFAULT now()
)";

/// 懒连接：进程启动时 PostgreSQL 暂时不可用也不会失败，`/readyz` 会如实报告。
pub fn connect_lazy(url: &str, max_connections: u32) -> Result<PgPool> {
    PgPoolOptions::new()
        .max_connections(max_connections)
        .acquire_timeout(Duration::from_secs(3))
        .connect_lazy(url)
        .context("PostgreSQL 连接串无法解析")
}

/// 立即建立连接（`migrate` 使用）。
pub async fn connect(url: &str, max_connections: u32) -> Result<PgPool> {
    PgPoolOptions::new()
        .max_connections(max_connections)
        .acquire_timeout(Duration::from_secs(10))
        .connect(url)
        .await
        .context("无法连接 PostgreSQL")
}

/// `SELECT 1`。
pub async fn ping(pool: &PgPool) -> Result<()> {
    sqlx::query("SELECT 1")
        .execute(pool)
        .await
        .context("PostgreSQL SELECT 1 失败")?;
    Ok(())
}

/// 迁移执行期间持有的互斥锁（`pg_advisory_lock`）。同时只允许一个 `rivu migrate` 在跑。
pub struct MigrationLock {
    conn: PoolConnection<Postgres>,
}

impl MigrationLock {
    pub async fn acquire(pool: &PgPool) -> Result<Self> {
        let mut conn = pool.acquire().await.context("获取迁移锁用的连接失败")?;
        sqlx::query("SELECT pg_advisory_lock(hashtext('rivu:migrate'))")
            .execute(&mut *conn)
            .await
            .context("获取 pg_advisory_lock 失败")?;
        Ok(MigrationLock { conn })
    }

    pub async fn release(mut self) -> Result<()> {
        sqlx::query("SELECT pg_advisory_unlock(hashtext('rivu:migrate'))")
            .execute(&mut *self.conn)
            .await
            .context("释放 pg_advisory_lock 失败")?;
        Ok(())
    }
}

pub async fn ensure_migrations_table(pool: &PgPool) -> Result<()> {
    // 先检查再建，避免 `IF NOT EXISTS` 产生 NOTICE 日志噪音。
    // 并发安全由调用方持有的 MigrationLock 保证。
    let exists: bool = sqlx::query_scalar("SELECT to_regclass('schema_migrations') IS NOT NULL")
        .fetch_one(pool)
        .await
        .context("检查 schema_migrations 是否存在失败")?;
    if exists {
        return Ok(());
    }
    sqlx::query(CREATE_MIGRATIONS_TABLE)
        .execute(pool)
        .await
        .context("创建 schema_migrations 失败")?;
    Ok(())
}

/// 已应用的迁移。表还不存在时返回空列表（全新数据库）。
pub async fn applied_migrations(pool: &PgPool) -> Result<Vec<AppliedMigration>> {
    let exists: bool = sqlx::query_scalar("SELECT to_regclass('schema_migrations') IS NOT NULL")
        .fetch_one(pool)
        .await
        .context("检查 schema_migrations 是否存在失败")?;
    if !exists {
        return Ok(Vec::new());
    }
    let rows =
        sqlx::query("SELECT version, name, checksum FROM schema_migrations ORDER BY version")
            .fetch_all(pool)
            .await
            .context("读取 schema_migrations 失败")?;
    Ok(rows
        .iter()
        .map(|r| AppliedMigration {
            version: r.get::<i64, _>("version") as u64,
            name: r.get("name"),
            checksum: r.get("checksum"),
        })
        .collect())
}

/// 已应用的最高版本（`/readyz` 用）。表不存在或为空都视为错误或 0，由调用方比较。
pub async fn max_applied_version(pool: &PgPool) -> Result<u64> {
    let v: i64 = sqlx::query_scalar("SELECT COALESCE(max(version), 0) FROM schema_migrations")
        .fetch_one(pool)
        .await
        .context("读取 schema_migrations 最高版本失败")?;
    Ok(v as u64)
}

/// 在一个事务内执行迁移文件并写入记录。
pub async fn apply_migration(pool: &PgPool, migration: &Migration) -> Result<()> {
    let mut tx = pool.begin().await.context("开启事务失败")?;
    // raw_sql 使用简单查询协议，一次可以执行多条语句。
    // 迁移文件在编译期嵌入二进制，不是外部输入，所以可以标记为 SQL 安全。
    sqlx::raw_sql(AssertSqlSafe(migration.sql.as_str()))
        .execute(&mut *tx)
        .await
        .with_context(|| format!("执行 postgres/{} 失败", migration.file))?;
    sqlx::query("INSERT INTO schema_migrations (version, name, checksum) VALUES ($1, $2, $3)")
        .bind(migration.version as i64)
        .bind(&migration.name)
        .bind(&migration.checksum)
        .execute(&mut *tx)
        .await
        .context("写入 schema_migrations 失败")?;
    tx.commit().await.context("提交事务失败")?;
    Ok(())
}
