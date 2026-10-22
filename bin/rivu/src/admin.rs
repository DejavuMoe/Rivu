//! 命令行管理命令：`rivu site add`、`rivu stats`。
//!
//! 它们直接连 PostgreSQL（`RIVU_DATABASE_URL`，应用账号）和 ClickHouse（reader 账号），不经过 HTTP；
//! 配置按 api 角色校验。结果写到 stdout，日志在 stderr。

use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use jiff::civil::Date;
use jiff::tz::TimeZone;
use rivu_common::config::Config;
use rivu_ingest::hostname::{HostnamePatterns, normalize_hostname};
use rivu_storage_pg::sites::{NewSite, validate_hostname_pattern};
use serde_json::json;

use crate::cli::{SiteAddArgs, StatsArgs};

/// `stats` 里页面榜的行数。
const TOP_PAGES_LIMIT: u32 = 10;

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

/// `rivu stats`：查询一个站点在本地日期范围内的核心指标，打印 JSON。
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
    let pages =
        rivu_query::top_pages(&reader, site.id, &tz, from, to, TOP_PAGES_LIMIT, timeout).await?;

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
        "top_pages": pages
            .iter()
            .map(|p| json!({"pathname": p.pathname, "visitors": p.visitors, "pageviews": p.pageviews}))
            .collect::<Vec<_>>(),
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
