//! 站点：加载（collector 的站点快照）、按 `site_key` 查找、建站（`rivu site add`）。
//!
//! 表结构见 `migrations/postgres/20261021100000_init.sql`。
//! `rivu_app` 账号对这些表有 `SELECT/INSERT/UPDATE/DELETE`（`deploy/postgres/init/01-roles.sql` 的默认授权）。

use anyhow::{Context, Result, bail};
use jiff::tz::TimeZone;
use rand::Rng;
use sqlx::{PgExecutor, PgPool, Postgres, Row, Transaction, postgres::PgRow};

use crate::shield::ShieldRuleRow;

/// `site_key` 的字母表：base32 小写（`^[a-z2-7]{12}$`，system-design §10 Q11）。
const SITE_KEY_ALPHABET: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";
const SITE_KEY_LEN: usize = 12;
/// 随机 key 撞车时的重试次数。32^12 的空间里撞车几乎不可能，这里只是兜底。
const SITE_KEY_ATTEMPTS: usize = 8;

/// 站点脚本的开关（`site_settings.tracker`，`tracker-and-protocol.md` §2.2–§2.4）。键缺失或不是布尔时取默认值：
/// `engagement`、`tagged` 默认开，其余默认关（与 tracker 的 `data-*` 默认值一致）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TrackerSwitches {
    /// hash 路由模式（core 也读）。
    pub hash: bool,
    pub engagement: bool,
    pub outbound: bool,
    /// 文件下载（开时用 tracker 的默认扩展名表）。
    pub downloads: bool,
    pub forms: bool,
    pub tagged: bool,
}

impl Default for TrackerSwitches {
    fn default() -> Self {
        TrackerSwitches {
            hash: false,
            engagement: true,
            outbound: false,
            downloads: false,
            forms: false,
            tagged: true,
        }
    }
}

impl TrackerSwitches {
    /// JSON 里的键名，也是看板 API 与脚本配置前缀里的键名。
    pub const KEYS: [&'static str; 6] = [
        "hash",
        "engagement",
        "outbound",
        "downloads",
        "forms",
        "tagged",
    ];

    /// 从 `site_settings.tracker`（JSON 文本）读取；解析失败时全用默认值。
    pub fn from_json_text(text: &str) -> Self {
        let v: serde_json::Value = serde_json::from_str(text).unwrap_or_default();
        let d = Self::default();
        let b = |k: &str, def: bool| v.get(k).and_then(serde_json::Value::as_bool).unwrap_or(def);
        TrackerSwitches {
            hash: b("hash", d.hash),
            engagement: b("engagement", d.engagement),
            outbound: b("outbound", d.outbound),
            downloads: b("downloads", d.downloads),
            forms: b("forms", d.forms),
            tagged: b("tagged", d.tagged),
        }
    }

    pub fn get(&self, key: &str) -> Option<bool> {
        Some(match key {
            "hash" => self.hash,
            "engagement" => self.engagement,
            "outbound" => self.outbound,
            "downloads" => self.downloads,
            "forms" => self.forms,
            "tagged" => self.tagged,
            _ => return None,
        })
    }

    /// 是否需要 `full` 构建：开了任一 full 专属能力（§2.4；hash 两个构建都支持）。
    pub fn needs_full(&self) -> bool {
        self.engagement || self.outbound || self.downloads || self.forms || self.tagged
    }
}

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
    /// `site_settings.tracker`：站点脚本的开关（配置前缀与 core / full 的选择用）。
    pub tracker: TrackerSwitches,
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
        tracker: TrackerSwitches::from_json_text(&r.try_get::<String, _>("tracker")?),
        ingest_rate_limit_per_min: r
            .try_get::<Option<i32>, _>("ingest_rate_limit_per_min")?
            .and_then(|n| u32::try_from(n).ok()),
        shield_rules: Vec::new(),
    })
}

/// 加载全部未删除的站点：`sites` + `site_hostnames` + `site_settings`（`collect_enabled`、`geo_level`、`tracker`）。
pub async fn load_sites(pool: &PgPool) -> Result<Vec<SiteRow>> {
    load_sites_filtered(pool, None).await
}

/// 只加载 `ids` 里仍未删除的站点（增量刷新用）；已删除或不存在的 id 不出现在结果里。
pub async fn load_sites_by_ids(pool: &PgPool, ids: &[u64]) -> Result<Vec<SiteRow>> {
    let ids = ids
        .iter()
        .map(|&id| i64::try_from(id).context("site_id 超出范围"))
        .collect::<Result<Vec<_>>>()?;
    load_sites_filtered(pool, Some(&ids)).await
}

async fn load_sites_filtered(pool: &PgPool, ids: Option<&[i64]>) -> Result<Vec<SiteRow>> {
    let rows = sqlx::query(
        "SELECT s.id, s.site_key, s.timezone,
                COALESCE(ss.collect_enabled, true) AS collect_enabled,
                COALESCE(ss.geo_level, 'city') AS geo_level,
                COALESCE(ss.tracker, '{}'::jsonb)::text AS tracker,
                ss.ingest_rate_limit_per_min,
                COALESCE(array_agg(h.pattern ORDER BY h.pattern) FILTER (WHERE h.id IS NOT NULL), '{}') AS hostnames
         FROM sites s
         LEFT JOIN site_settings ss ON ss.site_id = s.id
         LEFT JOIN site_hostnames h ON h.site_id = s.id AND h.deleted_at IS NULL
         WHERE s.deleted_at IS NULL AND ($1::bigint[] IS NULL OR s.id = ANY($1))
         GROUP BY s.id, ss.collect_enabled, ss.geo_level, ss.tracker, ss.ingest_rate_limit_per_min
         ORDER BY s.id",
    )
    .bind(ids)
    .fetch_all(pool)
    .await
    .context("加载站点失败")?;
    let mut sites = rows.iter().map(site_from_row).collect::<Result<Vec<_>>>()?;
    // 规则按站点分组挂到站点上（两次查询之间站点被删除的规则会被丢弃）
    let mut by_site: std::collections::HashMap<u64, Vec<ShieldRuleRow>> =
        std::collections::HashMap::new();
    for rule in crate::shield::load_rules(pool, ids).await? {
        by_site.entry(rule.site_id).or_default().push(rule);
    }
    for site in &mut sites {
        site.shield_rules = by_site.remove(&site.id).unwrap_or_default();
    }
    Ok(sites)
}

/// 增量刷新一轮的输入：PG 的当前时间（微秒，作为下一轮的水位）与变更过的站点。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SiteChanges {
    pub now_us: i64,
    pub site_ids: Vec<u64>,
}

/// 增量刷新的回看余量：`now()` 是事务开始的时间，比读水位早开始、晚提交的事务写下的
/// `updated_at` 可能早于水位；余量覆盖 5 秒以内的事务（`platform-and-ops.md` §3.3）。
pub const CHANGE_LOOKBACK_US: i64 = 5_000_000;

/// `updated_at >= since_us − 5 s` 的站点：`sites`、`site_settings`、`site_hostnames`、`shield_rules`
/// 任一行变过（**含已软删除的行**：删除也是变更）。`now()` 与查询在同一条语句里，取同一个时刻。
pub async fn changed_sites(pool: &PgPool, since_us: i64) -> Result<SiteChanges> {
    let row = sqlx::query(
        "WITH since AS (SELECT to_timestamp($1::double precision / 1e6) AS t)
         SELECT (extract(epoch FROM now()) * 1e6)::bigint AS now_us,
                ARRAY(
                    SELECT id FROM sites, since WHERE updated_at >= since.t
                    UNION SELECT site_id FROM site_settings, since WHERE updated_at >= since.t
                    UNION SELECT site_id FROM site_hostnames, since WHERE updated_at >= since.t
                    UNION SELECT site_id FROM shield_rules, since WHERE updated_at >= since.t
                ) AS ids",
    )
    .bind(since_us.saturating_sub(CHANGE_LOOKBACK_US))
    .fetch_one(pool)
    .await
    .context("查询变更过的站点失败")?;
    let ids: Vec<i64> = row.try_get("ids")?;
    Ok(SiteChanges {
        now_us: row.try_get("now_us")?,
        site_ids: ids
            .into_iter()
            .filter_map(|id| u64::try_from(id).ok())
            .collect(),
    })
}

/// PG 的当前时间（微秒）：全量重载之前读一次，作为之后增量刷新的水位。
pub async fn db_now_us(pool: &PgPool) -> Result<i64> {
    sqlx::query_scalar("SELECT (extract(epoch FROM now()) * 1e6)::bigint")
        .fetch_one(pool)
        .await
        .context("读取 PG 时间失败")
}

/// 站点配置变更通知的频道（PG 初始迁移里的 `notify_site_changed` 触发器发送，负载是 `site_id`）。
pub const SITE_CHANGED_CHANNEL: &str = "rivu_site_changed";

/// 按 `site_key` 查一个未删除的站点。
pub async fn find_site_by_key(pool: &PgPool, site_key: &str) -> Result<Option<SiteRow>> {
    let row = sqlx::query(
        "SELECT s.id, s.site_key, s.timezone,
                COALESCE(ss.collect_enabled, true) AS collect_enabled,
                COALESCE(ss.geo_level, 'city') AS geo_level,
                COALESCE(ss.tracker, '{}'::jsonb)::text AS tracker,
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

/// 查询引擎需要的站点设置（`SiteContext` 的来源）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuerySettings {
    /// IANA 时区名。
    pub timezone: String,
    /// 周起始日，0 = 周日，1 = 周一（默认）……6 = 周六（`site_settings.week_starts_on`）。
    pub week_starts_on: i16,
    /// `site_settings.retention_days`：站点的原始数据保留天数，`None` = 全局 `RIVU_RETENTION_DAYS`。
    pub retention_days: Option<i32>,
}

/// 按站点 id 读查询设置；站点不存在或已软删除返回 `None`。没有 `site_settings` 行时周起始日取列默认值 1（周一）。
pub async fn find_query_settings(pool: &PgPool, site_id: u64) -> Result<Option<QuerySettings>> {
    let id = i64::try_from(site_id).context("site_id 超出范围")?;
    let row = sqlx::query(
        "SELECT s.timezone, COALESCE(ss.week_starts_on, 1)::smallint AS week_starts_on, ss.retention_days
         FROM sites s LEFT JOIN site_settings ss ON ss.site_id = s.id
         WHERE s.id = $1 AND s.deleted_at IS NULL",
    )
    .bind(id)
    .fetch_optional(pool)
    .await
    .context("查询站点的查询设置失败")?;
    row.map(|r| {
        Ok(QuerySettings {
            timezone: r.try_get("timezone")?,
            week_starts_on: r.try_get("week_starts_on")?,
            retention_days: r.try_get("retention_days")?,
        })
    })
    .transpose()
}

/// 原始数据清理用：全部站点（含软删除、还没清除的）的 id、时区与站点级保留天数。
pub async fn retention_settings(pool: &PgPool) -> Result<Vec<(u64, String, Option<i32>)>> {
    let rows = sqlx::query(
        "SELECT s.id, s.timezone, ss.retention_days
         FROM sites s LEFT JOIN site_settings ss ON ss.site_id = s.id ORDER BY s.id",
    )
    .fetch_all(pool)
    .await
    .context("查询站点保留期失败")?;
    rows.iter()
        .map(|r| {
            let id: i64 = r.try_get("id")?;
            Ok((
                u64::try_from(id).context("sites.id 为负数")?,
                r.try_get("timezone")?,
                r.try_get("retention_days")?,
            ))
        })
        .collect()
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
    create_site_impl(pool, None, new).await
}

/// 在指定组织里建站（看板的建站端点用；调用方已校验组织存在且用户有权）。其余同 [`create_site`]。
pub async fn create_site_in_org(
    pool: &PgPool,
    organization_id: i64,
    new: &NewSite<'_>,
) -> Result<CreatedSite> {
    create_site_impl(pool, Some(organization_id), new).await
}

async fn create_site_impl(
    pool: &PgPool,
    in_org: Option<i64>,
    new: &NewSite<'_>,
) -> Result<CreatedSite> {
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

    if let Some(organization_id) = in_org {
        // 锁住组织行（共享锁足够）：组织在建站事务里被删除时，不会留下挂在已删除组织下的站点
        let alive: Option<i64> = sqlx::query_scalar(
            "SELECT id FROM organizations WHERE id = $1 AND deleted_at IS NULL FOR SHARE",
        )
        .bind(organization_id)
        .fetch_optional(&mut *tx)
        .await
        .context("查询组织失败")?;
        if alive.is_none() {
            bail!("组织 {organization_id} 不存在或已删除");
        }
        return insert_site(tx, organization_id, name, new.timezone, &hostnames).await;
    }

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

    insert_site(tx, organization_id, name, new.timezone, &hostnames).await
}

/// 在已开启的事务里写 `sites`、`site_settings`、`site_hostnames` 并提交。
async fn insert_site(
    mut tx: sqlx::Transaction<'_, sqlx::Postgres>,
    organization_id: i64,
    name: &str,
    timezone: &str,
    hostnames: &[&str],
) -> Result<CreatedSite> {
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
        .bind(timezone)
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
    for pattern in hostnames.iter().copied() {
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

// ------------------------------------------------------------------------------------------
// 站点管理（看板的设置页）
// ------------------------------------------------------------------------------------------

/// 设置页要的站点字段与设置（没有 `site_settings` 行时取列默认值，老数据兜底）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SiteAdminView {
    pub id: u64,
    pub site_key: String,
    pub name: String,
    pub timezone: String,
    pub is_public: bool,
    pub created_at: String,
    pub week_starts_on: i16,
    pub collect_enabled: bool,
    pub tracker: TrackerSwitches,
    pub ingest_rate_limit_per_min: Option<i32>,
    pub geo_level: String,
    pub retention_days: Option<i32>,
}

/// 读设置页的站点视图；站点不存在或已软删除返回 `None`。
pub async fn admin_view(db: impl PgExecutor<'_>, site_id: u64) -> Result<Option<SiteAdminView>> {
    let id = i64::try_from(site_id).context("site_id 超出范围")?;
    let row = sqlx::query(concat!(
        "SELECT s.id, s.site_key, s.name, s.timezone, s.is_public, ",
        ts_utc!("s.created_at"),
        " AS created_at,
                COALESCE(ss.week_starts_on, 1)::smallint AS week_starts_on,
                COALESCE(ss.collect_enabled, true) AS collect_enabled,
                COALESCE(ss.tracker, '{}'::jsonb)::text AS tracker,
                ss.ingest_rate_limit_per_min,
                COALESCE(ss.geo_level, 'city') AS geo_level,
                ss.retention_days
         FROM sites s LEFT JOIN site_settings ss ON ss.site_id = s.id
         WHERE s.id = $1 AND s.deleted_at IS NULL"
    ))
    .bind(id)
    .fetch_optional(db)
    .await
    .context("查询站点设置失败")?;
    row.map(|r| {
        let id: i64 = r.try_get("id")?;
        Ok(SiteAdminView {
            id: u64::try_from(id).context("sites.id 为负数")?,
            site_key: r.try_get("site_key")?,
            name: r.try_get("name")?,
            timezone: r.try_get("timezone")?,
            is_public: r.try_get("is_public")?,
            created_at: r.try_get("created_at")?,
            week_starts_on: r.try_get("week_starts_on")?,
            collect_enabled: r.try_get("collect_enabled")?,
            tracker: TrackerSwitches::from_json_text(&r.try_get::<String, _>("tracker")?),
            ingest_rate_limit_per_min: r.try_get("ingest_rate_limit_per_min")?,
            geo_level: r.try_get("geo_level")?,
            retention_days: r.try_get("retention_days")?,
        })
    })
    .transpose()
}

/// 站点设置的部分更新：`None` = 不改。取值由调用方校验（PG 的 CHECK 兜底）。
#[derive(Debug, Clone, Default)]
pub struct SitePatch<'a> {
    pub name: Option<&'a str>,
    pub timezone: Option<&'a str>,
    pub is_public: Option<bool>,
    pub week_starts_on: Option<i16>,
    pub collect_enabled: Option<bool>,
    /// 要改的脚本开关（键名见 [`TrackerSwitches::KEYS`]），只覆盖这些键，`site_settings.tracker` 的其他键保留。
    pub tracker: Vec<(&'static str, bool)>,
    /// 外层 `Some` = 要改；内层 `None` = 恢复全局默认。
    pub ingest_rate_limit_per_min: Option<Option<i32>>,
    pub geo_level: Option<&'a str>,
    /// 外层 `Some` = 要改；内层 `None` = 用全局 `RIVU_RETENTION_DAYS`。
    pub retention_days: Option<Option<i32>>,
}

/// 维护任务在同步等待 ClickHouse 时，PG 会话处于事务内空闲状态。
pub async fn begin_maintenance(pool: &PgPool) -> Result<Transaction<'static, Postgres>> {
    let mut tx = pool
        .begin_with("BEGIN ISOLATION LEVEL READ COMMITTED")
        .await?;
    // set_config(..., true) is SET LOCAL. Older PG versions lack transaction_timeout.
    sqlx::query(
        "SELECT set_config(name, '0', true) FROM pg_settings
         WHERE name IN ('idle_in_transaction_session_timeout', 'transaction_timeout')",
    )
    .execute(&mut *tx)
    .await
    .context("设置站点维护事务超时失败")?;
    Ok(tx)
}

/// 锁住站点行，读最新时区。软删除站点的擦除也使用这把锁。
pub async fn lock_timezone(
    tx: &mut Transaction<'_, Postgres>,
    site_id: u64,
    include_deleted: bool,
) -> Result<Option<String>> {
    let id = i64::try_from(site_id).context("site_id 超出范围")?;
    sqlx::query_scalar(
        "SELECT timezone FROM sites WHERE id = $1 AND ($2 OR deleted_at IS NULL) FOR UPDATE",
    )
    .bind(id)
    .bind(include_deleted)
    .fetch_optional(&mut **tx)
    .await
    .context("锁定站点时区失败")
}

/// 在调用方事务里更新 `sites` 与 `site_settings`（没有 settings 行时先补一行）；站点不存在或已软删除返回 `false`。
/// `updated_at` 由触发器维护，collector 的缓存据此增量刷新。
pub async fn update_site(
    tx: &mut Transaction<'_, Postgres>,
    site_id: u64,
    patch: &SitePatch<'_>,
) -> Result<bool> {
    let id = i64::try_from(site_id).context("site_id 超出范围")?;
    let touched: Option<i64> = sqlx::query_scalar(
        "UPDATE sites SET name = COALESCE($2, name), timezone = COALESCE($3, timezone),
                          is_public = COALESCE($4, is_public)
         WHERE id = $1 AND deleted_at IS NULL RETURNING id",
    )
    .bind(id)
    .bind(patch.name)
    .bind(patch.timezone)
    .bind(patch.is_public)
    .fetch_optional(&mut **tx)
    .await
    .context("更新 sites 失败")?;
    if touched.is_none() {
        return Ok(false);
    }
    sqlx::query("INSERT INTO site_settings (site_id) VALUES ($1) ON CONFLICT (site_id) DO NOTHING")
        .bind(id)
        .execute(&mut **tx)
        .await
        .context("补写 site_settings 失败")?;
    sqlx::query(
        "UPDATE site_settings SET
             week_starts_on = COALESCE($2::smallint, week_starts_on),
             collect_enabled = COALESCE($3::boolean, collect_enabled),
             geo_level = COALESCE($4::text, geo_level),
             ingest_rate_limit_per_min =
                 CASE WHEN $5::boolean THEN $6::integer ELSE ingest_rate_limit_per_min END,
             tracker = CASE WHEN $7::text IS NULL THEN tracker ELSE tracker || $7::jsonb END,
             retention_days = CASE WHEN $8::boolean THEN $9::integer ELSE retention_days END
         WHERE site_id = $1",
    )
    .bind(id)
    .bind(patch.week_starts_on)
    .bind(patch.collect_enabled)
    .bind(patch.geo_level)
    .bind(patch.ingest_rate_limit_per_min.is_some())
    .bind(patch.ingest_rate_limit_per_min.flatten())
    .bind((!patch.tracker.is_empty()).then(|| {
        serde_json::Value::Object(
            patch
                .tracker
                .iter()
                .map(|(k, v)| ((*k).to_owned(), serde_json::Value::Bool(*v)))
                .collect(),
        )
        .to_string()
    }))
    .bind(patch.retention_days.is_some())
    .bind(patch.retention_days.flatten())
    .execute(&mut **tx)
    .await
    .context("更新 site_settings 失败")?;
    Ok(true)
}

/// 只软删除站点行（`deleted_at = now()`），不写删除意图；返回是否删除了。看板删除站点用
/// [`crate::stats_deletions::soft_delete_site`]（同一事务写入站点删除意图，恢复窗口过后清除统计数据）。
pub async fn soft_delete_site(pool: &PgPool, site_id: u64) -> Result<bool> {
    let n = sqlx::query("UPDATE sites SET deleted_at = now() WHERE id = $1 AND deleted_at IS NULL")
        .bind(i64::try_from(site_id).context("site_id 超出范围")?)
        .execute(pool)
        .await
        .context("软删除站点失败")?
        .rows_affected();
    Ok(n > 0)
}

/// 白名单里的一条（未删除）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostnameRow {
    pub pattern: String,
    pub created_at: String,
}

/// 站点的白名单，按创建时间升序（第一条是建站时的主域名）。
pub async fn list_hostnames(pool: &PgPool, site_id: u64) -> Result<Vec<HostnameRow>> {
    let rows = sqlx::query(concat!(
        "SELECT pattern, ",
        ts_utc!("created_at"),
        " AS created_at
         FROM site_hostnames WHERE site_id = $1 AND deleted_at IS NULL ORDER BY site_hostnames.created_at, id"
    ))
    .bind(i64::try_from(site_id).context("site_id 超出范围")?)
    .fetch_all(pool)
    .await
    .context("查询站点白名单失败")?;
    rows.iter()
        .map(|r| {
            Ok(HostnameRow {
                pattern: r.try_get("pattern")?,
                created_at: r.try_get("created_at")?,
            })
        })
        .collect()
}

/// 加一条白名单模式（调用方已校验）；已有同一模式的未删除行返回 `None`。
pub async fn add_hostname(
    pool: &PgPool,
    site_id: u64,
    pattern: &str,
) -> Result<Option<HostnameRow>> {
    let row = sqlx::query(concat!(
        "INSERT INTO site_hostnames (site_id, pattern) VALUES ($1, $2)
         ON CONFLICT (site_id, pattern) WHERE deleted_at IS NULL DO NOTHING
         RETURNING pattern, ",
        ts_utc!("created_at"),
        " AS created_at"
    ))
    .bind(i64::try_from(site_id).context("site_id 超出范围")?)
    .bind(pattern)
    .fetch_optional(pool)
    .await
    .context("写入 site_hostnames 失败")?;
    row.map(|r| {
        Ok(HostnameRow {
            pattern: r.try_get("pattern")?,
            created_at: r.try_get("created_at")?,
        })
    })
    .transpose()
}

/// 按模式精确软删除一条白名单；返回是否删除了。允许删到空（空白名单 = collector 丢弃全部事件，fail-closed）。
pub async fn remove_hostname(pool: &PgPool, site_id: u64, pattern: &str) -> Result<bool> {
    let n = sqlx::query(
        "UPDATE site_hostnames SET deleted_at = now()
         WHERE site_id = $1 AND pattern = $2 AND deleted_at IS NULL",
    )
    .bind(i64::try_from(site_id).context("site_id 超出范围")?)
    .bind(pattern)
    .execute(pool)
    .await
    .context("删除站点白名单失败")?
    .rows_affected();
    Ok(n > 0)
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
