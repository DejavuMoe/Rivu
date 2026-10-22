//! 访客 ID 的盐（`platform-and-ops.md` §3.3、§6 `salts_rotate`）。
//!
//! 盐只存在 PG，按「站点本地日期对应的日历日」索引，16 字节随机数，不可变。
//! collector 只读；写入（`ensure_salts`、`delete_salts_before`）由 jobs 完成，collector 绝不现场生成。

use anyhow::{Context, Result};
use jiff::civil::Date;
use rand::RngCore;
use sqlx::{PgPool, Row};

/// 16 字节盐。
pub type Salt = [u8; 16];

/// 确保 `days` 里每一天都有盐：缺的补随机盐，已有的不动（`ON CONFLICT DO NOTHING`，并发安全）。
/// 返回新写入的天数。
pub async fn ensure_salts(pool: &PgPool, days: &[Date]) -> Result<u64> {
    let mut inserted = 0;
    for day in days {
        let mut salt = [0u8; 16];
        rand::rng().fill_bytes(&mut salt);
        // 日期以文本传入再转换，不依赖 sqlx 的日期类型特性。
        let result = sqlx::query(
            "INSERT INTO salts (day, salt) VALUES (($1::text)::date, $2) ON CONFLICT (day) DO NOTHING",
        )
        .bind(day.to_string())
        .bind(salt.as_slice())
        .execute(pool)
        .await
        .with_context(|| format!("写入 {day} 的盐失败"))?;
        inserted += result.rows_affected();
    }
    Ok(inserted)
}

/// 删除 `day` 之前（不含）的盐。返回删除的行数。
pub async fn delete_salts_before(pool: &PgPool, day: Date) -> Result<u64> {
    let result = sqlx::query("DELETE FROM salts WHERE day < ($1::text)::date")
        .bind(day.to_string())
        .execute(pool)
        .await
        .context("删除过期的盐失败")?;
    Ok(result.rows_affected())
}

/// 读取 `[from, to]`（含）内存在的盐，按日期升序。
pub async fn load_salts(pool: &PgPool, from: Date, to: Date) -> Result<Vec<(Date, Salt)>> {
    let rows = sqlx::query(
        "SELECT day::text AS day, salt FROM salts
         WHERE day BETWEEN ($1::text)::date AND ($2::text)::date ORDER BY day",
    )
    .bind(from.to_string())
    .bind(to.to_string())
    .fetch_all(pool)
    .await
    .context("读取盐失败")?;
    rows.iter()
        .map(|r| {
            let day: String = r.try_get("day")?;
            let day: Date = day
                .parse()
                .with_context(|| format!("salts.day `{day}` 不是日期"))?;
            let bytes: Vec<u8> = r.try_get("salt")?;
            let salt: Salt = bytes
                .try_into()
                .map_err(|b: Vec<u8>| anyhow::anyhow!("salts.salt 长度为 {}，期望 16", b.len()))?;
            Ok((day, salt))
        })
        .collect()
}
