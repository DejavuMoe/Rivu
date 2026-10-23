//! 站点：加载（collector 的站点快照）、按 `site_key` 查找、建站（`rivu site add`）。
//!
//! 表结构见 `migrations/postgres/20261021100000_core_tables.sql`。
//! `rivu_app` 账号对这些表有 `SELECT/INSERT/UPDATE/DELETE`（`deploy/postgres/init/01-roles.sql` 的默认授权）。

use anyhow::{Context, Result, bail};
use jiff::tz::TimeZone;
use rand::Rng;
use sqlx::{PgPool, Row, postgres::PgRow};

use crate::shield::ShieldRuleRow;

/// `site_key` 的字母表：base32 小写（`^[a-z2-7]{12}$`，system-design §10 Q11）。
const SITE_KEY_ALPHABET: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";
const SITE_KEY_LEN: usize = 12;
/// 随机 key 撞车时的重试次数。32^12 的空间里撞车几乎不可能，这里只是兜底。
const SITE_KEY_ATTEMPTS: usize = 8;

/// 一个站点的缓存所需字段（未删除）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SiteRow {
    /// 同时是 ClickHouse 的 `site_id`。
    pub id: u64,
    pub site_key: String,
    /// IANA 时区名。
    pub timezone: String,
    pub collect_enabled: bool,
    /// hostname 白名单的原始模式（`site_hostnames.pattern`，未删除的）。
    pub hostnames: Vec<String>,
    /// `site_settings.geo_level`：`country` | `region` | `city`（PG 的 CHECK 保证取值；没有 settings 行时按 `city`，与列默认值一致）。
    /// 这里保持字符串，解析在 `rivu-ingest`（storage 层不依赖 `rivu-geo`）。
    pub geo_level: String,
    /// `site_settings.tracker->'hash'` 是否为 JSON `true`（hash 路由模式，站点脚本配置前缀用）。
    pub tracker_hash: bool,
    /// `site_settings.ingest_rate_limit_per_min`：每分钟事件数上限，`None` 用全局默认。
    pub ingest_rate_limit_per_min: Option<u32>,
    /// 未删除的屏蔽规则。只有 [`load_sites`]（站点快照用）会填，[`find_site_by_key`] 不加载（留空）。
    pub shield_rules: Vec<ShieldRuleRow>,
}

fn site_from_row(r: &PgRow) -> Result<SiteRow> {
    let id: i64 = r.try_get("id")?;
    Ok(SiteRow {
        id: u64::try_from(id).context("sites.id 为负数")?,
        site_key: r.try_get("site_key")?,
        timezone: r.try_get("timezone")?,
        collect_enabled: r.try_get("collect_enabled")?,
        hostnames: r.try_get("hostnames")?,
        geo_level: r.try_get("geo_level")?,
        tracker_hash: r.try_get("tracker_hash")?,
        ingest_rate_limit_per_min: r
            .try_get::<Option<i32>, _>("ingest_rate_limit_per_min")?
            .and_then(|n| u32::try_from(n).ok()),
        shield_rules: Vec::new(),
    })
}

/// 加载全部未删除的站点：`sites` + `site_hostnames` + `site_settings`（`collect_enabled`、`geo_level`、`tracker.hash`）。
pub async fn load_sites(pool: &PgPool) -> Result<Vec<SiteRow>> {
    let rows = sqlx::query(
        "SELECT s.id, s.site_key, s.timezone,
                COALESCE(ss.collect_enabled, true) AS collect_enabled,
                COALESCE(ss.geo_level, 'city') AS geo_level,
                COALESCE(ss.tracker -> 'hash' = 'true'::jsonb, false) AS tracker_hash,
                ss.ingest_rate_limit_per_min,
                COALESCE(array_agg(h.pattern ORDER BY h.pattern) FILTER (WHERE h.id IS NOT NULL), '{}') AS hostnames
         FROM sites s
         LEFT JOIN site_settings ss ON ss.site_id = s.id
         LEFT JOIN site_hostnames h ON h.site_id = s.id AND h.deleted_at IS NULL
         WHERE s.deleted_at IS NULL
         GROUP BY s.id, ss.collect_enabled, ss.geo_level, ss.tracker, ss.ingest_rate_limit_per_min
         ORDER BY s.id",
    )
    .fetch_all(pool)
    .await
    .context("加载站点失败")?;
    let mut sites = rows.iter().map(site_from_row).collect::<Result<Vec<_>>>()?;
    // 规则按站点分组挂到站点上（两次查询之间站点被删除的规则会被丢弃）
    let mut by_site: std::collections::HashMap<u64, Vec<ShieldRuleRow>> =
        std::collections::HashMap::new();
    for rule in crate::shield::load_all_rules(pool).await? {
        by_site.entry(rule.site_id).or_default().push(rule);
    }
    for site in &mut sites {
        site.shield_rules = by_site.remove(&site.id).unwrap_or_default();
    }
    Ok(sites)
}

/// 按 `site_key` 查一个未删除的站点。
pub async fn find_site_by_key(pool: &PgPool, site_key: &str) -> Result<Option<SiteRow>> {
    let row = sqlx::query(
        "SELECT s.id, s.site_key, s.timezone,
                COALESCE(ss.collect_enabled, true) AS collect_enabled,
                COALESCE(ss.geo_level, 'city') AS geo_level,
                COALESCE(ss.tracker -> 'hash' = 'true'::jsonb, false) AS tracker_hash,
                ss.ingest_rate_limit_per_min,
                COALESCE(array_agg(h.pattern ORDER BY h.pattern) FILTER (WHERE h.id IS NOT NULL), '{}') AS hostnames
         FROM sites s
         LEFT JOIN site_settings ss ON ss.site_id = s.id
         LEFT JOIN site_hostnames h ON h.site_id = s.id AND h.deleted_at IS NULL
         WHERE s.deleted_at IS NULL AND s.site_key = $1
         GROUP BY s.id, ss.collect_enabled, ss.geo_level, ss.tracker, ss.ingest_rate_limit_per_min",
    )
    .bind(site_key)
    .fetch_optional(pool)
    .await
    .context("查询站点失败")?;
    row.as_ref().map(site_from_row).transpose()
}

/// 站点访问判定需要的字段（`platform-and-ops.md` §4.2）：所属组织和是否公开。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SiteOwner {
    pub id: u64,
    pub site_key: String,
    pub organization_id: i64,
    pub is_public: bool,
}

/// 按 `site_key` 查未删除站点的归属（公开 API 的站点访问判定用）；不存在或已软删除返回 `None`。
pub async fn find_site_owner(pool: &PgPool, site_key: &str) -> Result<Option<SiteOwner>> {
    let row = sqlx::query(
        "SELECT id, site_key, organization_id, is_public FROM sites
         WHERE site_key = $1 AND deleted_at IS NULL",
    )
    .bind(site_key)
    .fetch_optional(pool)
    .await
    .context("查询站点归属失败")?;
    row.map(|r| {
        let id: i64 = r.try_get("id")?;
        Ok(SiteOwner {
            id: u64::try_from(id).context("sites.id 为负数")?,
            site_key: r.try_get("site_key")?,
            organization_id: r.try_get("organization_id")?,
            is_public: r.try_get("is_public")?,
        })
    })
    .transpose()
}

/// 新站点的参数。
#[derive(Debug, Clone)]
pub struct NewSite<'a> {
    pub name: &'a str,
    /// IANA 时区名，建站时校验。
    pub timezone: &'a str,
    /// hostname 白名单，至少一项；第一项是主域名。调用方负责按 hostname 规则规范化。
    /// 白名单为空时 collector 丢弃全部事件（§10 Q12），所以建站时必须给出主域名。
    pub hostnames: &'a [String],
}

/// 建站结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreatedSite {
    pub id: u64,
    pub site_key: String,
    pub organization_id: i64,
}

/// 生成一个 `^[a-z2-7]{12}$` 的随机 `site_key`。
pub fn generate_site_key() -> String {
    let mut rng = rand::rng();
    (0..SITE_KEY_LEN)
        .map(|_| char::from(SITE_KEY_ALPHABET[rng.random_range(0..SITE_KEY_ALPHABET.len())]))
        .collect()
}

/// 检查一条 hostname 模式是否符合 PG 的 CHECK 约束：
/// 全小写，`^(\*\.)?([a-z0-9-]+\.)*[a-z0-9-]+$`。
pub fn validate_hostname_pattern(pattern: &str) -> Result<()> {
    let body = pattern.strip_prefix("*.").unwrap_or(pattern);
    let ok = !body.is_empty()
        && body.split('.').all(|label| {
            !label.is_empty()
                && label
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        });
    if !ok {
        bail!(
            "hostname 模式 `{pattern}` 无效：只允许小写字母、数字、`-` 和 `.`，通配只能是最左一段（`*.example.com`）"
        );
    }
    Ok(())
}

/// 建站：同一个事务里写 `sites`、`site_settings`、`site_hostnames`。
///
/// - 没有任何组织时先建一个名为 `default` 的组织；已有组织时用 id 最小的那个；
/// - `site_key` 是随机的 12 位 base32 小写，撞车时重新生成；
/// - 白名单就是 `hostnames`（含主域名）：建站时自动加入主域名，白名单不会是空的（§10 Q12）。
pub async fn create_site(pool: &PgPool, new: &NewSite<'_>) -> Result<CreatedSite> {
    let name = new.name.trim();
    if name.is_empty() {
        bail!("站点名称不能为空");
    }
    TimeZone::get(new.timezone)
        .with_context(|| format!("`{}` 不是有效的 IANA 时区名", new.timezone))?;
    let mut hostnames: Vec<&str> = Vec::new();
    for h in new.hostnames {
        validate_hostname_pattern(h)?;
        if !hostnames.contains(&h.as_str()) {
            hostnames.push(h);
        }
    }
    if hostnames.is_empty() {
        bail!("至少需要一个 hostname：白名单为空时 collector 会丢弃该站点的全部事件");
    }

    let mut tx = pool.begin().await.context("开启事务失败")?;

    let existing: Option<i64> = sqlx::query_scalar(
        "SELECT id FROM organizations WHERE deleted_at IS NULL ORDER BY id LIMIT 1",
    )
    .fetch_optional(&mut *tx)
    .await
    .context("查询组织失败")?;
    let organization_id = match existing {
        Some(id) => id,
        None => {
            sqlx::query(
                "INSERT INTO organizations (name, slug) VALUES ('default', 'default')
                 ON CONFLICT (lower(slug)) WHERE deleted_at IS NULL DO NOTHING",
            )
            .execute(&mut *tx)
            .await
            .context("创建 default 组织失败")?;
            sqlx::query_scalar(
                "SELECT id FROM organizations WHERE deleted_at IS NULL ORDER BY id LIMIT 1",
            )
            .fetch_one(&mut *tx)
            .await
            .context("读取 default 组织失败")?
        }
    };

    let mut created = None;
    for _ in 0..SITE_KEY_ATTEMPTS {
        let key = generate_site_key();
        let id: Option<i64> = sqlx::query_scalar(
            "INSERT INTO sites (organization_id, site_key, name, timezone) VALUES ($1, $2, $3, $4)
             ON CONFLICT (site_key) DO NOTHING RETURNING id",
        )
        .bind(organization_id)
        .bind(&key)
        .bind(name)
        .bind(new.timezone)
        .fetch_optional(&mut *tx)
        .await
        .context("写入 sites 失败")?;
        if let Some(id) = id {
            created = Some((id, key));
            break;
        }
    }
    let Some((id, site_key)) = created else {
        bail!("连续 {SITE_KEY_ATTEMPTS} 次生成的 site_key 都已存在");
    };

    sqlx::query("INSERT INTO site_settings (site_id) VALUES ($1)")
        .bind(id)
        .execute(&mut *tx)
        .await
        .context("写入 site_settings 失败")?;
    for pattern in hostnames {
        sqlx::query("INSERT INTO site_hostnames (site_id, pattern) VALUES ($1, $2)")
            .bind(id)
            .bind(pattern)
            .execute(&mut *tx)
            .await
            .with_context(|| format!("写入 site_hostnames（{pattern}）失败"))?;
    }
    tx.commit().await.context("提交事务失败")?;

    Ok(CreatedSite {
        id: u64::try_from(id).context("sites.id 为负数")?,
        site_key,
        organization_id,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn site_keys_match_the_pg_check() {
        for _ in 0..200 {
            let k = generate_site_key();
            assert_eq!(k.len(), 12);
            assert!(
                k.bytes()
                    .all(|b| b.is_ascii_lowercase() || (b'2'..=b'7').contains(&b)),
                "{k}"
            );
        }
        assert_ne!(generate_site_key(), generate_site_key());
    }

    #[test]
    fn hostname_patterns_follow_the_pg_check() {
        for ok in [
            "example.com",
            "a-b.example.co.uk",
            "*.example.com",
            "localhost",
            "127.0.0.1",
        ] {
            assert!(validate_hostname_pattern(ok).is_ok(), "{ok}");
        }
        for bad in [
            "",
            "Example.com",
            "*.",
            "*example.com",
            "a.*.com",
            "**.example.com",
            "example..com",
            ".example.com",
            "example.com.",
            "ex ample.com",
            "例え.jp",
            "example.com:8080",
        ] {
            assert!(validate_hostname_pattern(bad).is_err(), "{bad}");
        }
    }
}
