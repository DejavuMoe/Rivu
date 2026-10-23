//! 命令行管理命令：`rivu site add`、`rivu stats`。
//!
//! 它们直接连 PostgreSQL（`RIVU_DATABASE_URL`，应用账号）和 ClickHouse（reader 账号），不经过 HTTP；
//! 配置按 api 角色校验。结果写到 stdout，日志在 stderr。

use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use jiff::civil::Date;
use jiff::tz::TimeZone;
use rivu_common::config::Config;
use rivu_geo::{GeoCapability, GeoDb};
use rivu_ingest::hostname::{HostnamePatterns, normalize_hostname};
use rivu_query::Dimension;
use rivu_storage_pg::sites::{NewSite, validate_hostname_pattern};
use serde_json::json;

use crate::cli::{SiteAddArgs, StatsArgs};

/// `stats` 里每个维度榜的行数。
const BREAKDOWN_LIMIT: u32 = 10;

/// 规范化一条 hostname 白名单模式：小写、去空白；精确匹配按 hostname 规则（去端口、结尾的 `.`、前导 `www.`），
/// 通配 `*.example.com` 保留通配头，通配之后的部分不去 `www.`（见 `HostnamePatterns::new`）。
pub fn normalize_pattern(raw: &str) -> Result<String> {
    let lowered = raw.trim().to_lowercase();
    let pattern = match lowered.strip_prefix("*.") {
        Some(rest) => format!("*.{}", rest.trim_end_matches('.')),
        None => normalize_hostname(&lowered),
    };
    validate_hostname_pattern(&pattern).with_context(|| format!("hostname `{raw}` 无效"))?;
    // 再过一遍运行时的白名单编译：它认为无效的模式会被悄悄忽略，建站时就要拒绝。
    if HostnamePatterns::new([pattern.as_str()]).is_empty() {
        bail!("hostname `{raw}` 无效：规范化后为 `{pattern}`，不是有效的白名单模式");
    }
    Ok(pattern)
}

/// `rivu site add`：建站，打印 `site_key` 和 `site_id`。
pub async fn site_add(config: &Config, args: SiteAddArgs) -> Result<ExitCode> {
    let mut hostnames: Vec<String> = Vec::new();
    for raw in &args.hostname {
        let p = normalize_pattern(raw)?;
        if !hostnames.contains(&p) {
            hostnames.push(p);
        }
    }
    TimeZone::get(&args.timezone)
        .with_context(|| format!("`{}` 不是有效的 IANA 时区名", args.timezone))?;

    let pool = rivu_storage_pg::connect(config.database_url.expose(), 2).await?;
    let created = rivu_storage_pg::sites::create_site(
        &pool,
        &NewSite {
            name: &args.name,
            timezone: &args.timezone,
            hostnames: &hostnames,
        },
    )
    .await?;

    println!("site_key: {}", created.site_key);
    println!("site_id: {}", created.id);
    println!("timezone: {}", args.timezone);
    println!("hostnames: {}", hostnames.join(", "));
    Ok(ExitCode::SUCCESS)
}

/// `rivu stats`：查询一个站点在本地日期范围内的核心指标与各维度分组（页面、来源、渠道、国家、设备，每个最多 10 行），
/// 打印 JSON。`meta` 带 `geo_attribution` 与 `geo_capability`（结果含国家时必填）。
pub async fn stats(config: &Config, args: StatsArgs) -> Result<ExitCode> {
    let pool = rivu_storage_pg::connect(config.database_url.expose(), 2).await?;
    let Some(site) = rivu_storage_pg::sites::find_site_by_key(&pool, &args.site).await? else {
        bail!("找不到站点 `{}`（或已删除）", args.site);
    };
    let tz = TimeZone::get(&site.timezone)
        .with_context(|| format!("站点时区 `{}` 无效", site.timezone))?;
    let account = config
        .ch_reader
        .as_ref()
        .context("stats 需要 RIVU_CH_READER_USER / RIVU_CH_READER_PASSWORD")?;
    let reader =
        rivu_storage_ch::client(&config.clickhouse_url, account, Some(&config.ch_database));

    let (from, to): (Date, Date) = (args.from, args.to);
    // `rivu stats` 等同看板查询，用看板档的超时（§10 Q14）。
    let timeout = config.query_timeouts.dashboard_secs;
    let overview = rivu_query::overview(&reader, site.id, &tz, from, to, timeout).await?;
    let mut breakdowns = serde_json::Map::new();
    for dim in Dimension::ALL {
        let rows = rivu_query::breakdown(
            &reader,
            site.id,
            &tz,
            from,
            to,
            dim,
            BREAKDOWN_LIMIT,
            timeout,
        )
        .await?;
        breakdowns.insert(
            dim.as_str().to_owned(),
            rows.iter()
                .map(|r| {
                    json!({"value": r.value, "visitors": r.visitors, "visits": r.visits,
                           "pageviews": r.pageviews})
                })
                .collect::<Vec<_>>()
                .into(),
        );
    }

    // 含国家的结果必须带署名与库的能力（system-design §4.4、§6.10）。署名与能力跟随实际加载的库：
    // 这里读同一个库文件（collector / all 进程热加载的就是它）；文件不存在则两者都是 null。
    let geo = GeoDb::from_config(&config.geo);
    if let Err(e) = geo.reload_if_changed(&config.geo.db_path) {
        tracing::warn!(error = %format!("{e:#}"), "读取 GeoIP 库失败，geo_attribution 与 geo_capability 为 null");
    }
    let geo_meta = json!({
        "geo_attribution": geo.attribution(),
        "geo_capability": geo.capability().map(GeoCapability::as_str),
    });

    let out = json!({
        "site_key": site.site_key,
        "site_id": site.id,
        "timezone": site.timezone,
        "from": from.to_string(),
        "to": to.to_string(),
        "visitors": overview.visitors,
        "visits": overview.visits,
        "pageviews": overview.pageviews,
        "bounce_rate": overview.bounce_rate,
        "visit_duration": overview.visit_duration,
        "engaged_duration": overview.engaged_duration,
        "breakdown": breakdowns,
        "meta": geo_meta,
    });
    println!("{}", serde_json::to_string_pretty(&out)?);
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_patterns_follow_the_hostname_rules() {
        assert_eq!(normalize_pattern("Example.COM").unwrap(), "example.com");
        assert_eq!(
            normalize_pattern(" www.example.com ").unwrap(),
            "example.com"
        );
        assert_eq!(normalize_pattern("example.com.").unwrap(), "example.com");
        assert_eq!(normalize_pattern("localhost:8080").unwrap(), "localhost");
    }

    #[test]
    fn wildcards_keep_their_head_and_do_not_strip_www() {
        assert_eq!(normalize_pattern("*.Example.com").unwrap(), "*.example.com");
        assert_eq!(
            normalize_pattern("*.www.example.com").unwrap(),
            "*.www.example.com"
        );
    }

    #[test]
    fn invalid_patterns_are_rejected() {
        for bad in [
            "",
            "   ",
            "a.*.com",
            "*example.com",
            "exa mple.com",
            "*.",
            "例え.jp",
        ] {
            assert!(normalize_pattern(bad).is_err(), "{bad:?}");
        }
    }
}
