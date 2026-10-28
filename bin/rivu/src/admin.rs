//! 命令行管理命令：`rivu site add`、`rivu query`。
//!
//! 它们直接连 PostgreSQL（`RIVU_DATABASE_URL`，应用账号）和 ClickHouse（reader 账号），不经过 HTTP；
//! 配置按管理命令（`Role::Admin`）校验。结果写到 stdout，日志在 stderr。

use std::process::ExitCode;

use anyhow::{Context, Result, anyhow, bail};
use jiff::tz::TimeZone;
use rivu_common::config::Config;
use rivu_geo::GeoDb;
use rivu_query::{ExecOptions, QuerySpec, TenantScope};
use rivu_storage_pg::sites::NewSite;

use crate::cli::{QueryArgs, SiteAddArgs};
use crate::query_api::load_site_context;

/// 规范化一条 hostname 白名单模式（实现与屏蔽规则的 hostname 类型共用，见 `rivu_ingest::hostname::validated_pattern`）。
pub fn normalize_pattern(raw: &str) -> Result<String> {
    rivu_ingest::hostname::validated_pattern(raw)
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

/// `rivu query`：公开查询 API（`POST /api/v1/query`）的命令行版本，不经过 HTTP 与 API key。
/// 从文件或 stdin 读一个 QuerySpec（`site` 必填），执行后把 QueryResult 打印成 JSON，与 API 的响应体相同。
pub async fn query(config: &Config, args: QueryArgs) -> Result<ExitCode> {
    let text = match &args.spec {
        Some(path) => std::fs::read_to_string(path)
            .with_context(|| format!("读取 {} 失败", path.display()))?,
        None => std::io::read_to_string(std::io::stdin()).context("读取 stdin 失败")?,
    };
    let spec = QuerySpec::parse_str(&text).map_err(|e| anyhow!("QuerySpec 无效：{e}"))?;
    let Some(site_key) = spec.site.clone() else {
        bail!("QuerySpec 的 `site` 必填");
    };

    let pool = rivu_storage_pg::connect(config.database_url.expose(), 2).await?;
    let Some(site) = rivu_storage_pg::sites::find_site_by_key(&pool, &site_key).await? else {
        bail!("找不到站点 `{site_key}`（或已删除）");
    };
    let account = config
        .ch_reader
        .as_ref()
        .context("query 需要 RIVU_CH_READER_USER / RIVU_CH_READER_PASSWORD")?;
    let reader =
        rivu_storage_ch::client(&config.clickhouse_url, account, Some(&config.ch_database));

    // 署名与能力跟随实际加载的库（§4.4、§6.10）：读 `rivu serve` 热加载的同一个文件；文件不存在则为 null。
    let geo = GeoDb::from_config(&config.geo);
    if let Err(e) = geo.reload_if_changed(&config.geo.db_path) {
        tracing::warn!(error = %format!("{e:#}"), "读取 GeoIP 库失败，geo_attribution 与 geo_capability 为 null");
    }
    let ctx = load_site_context(
        &pool,
        Some(&geo),
        site.id,
        &site.site_key,
        config.retention_days,
    )
    .await
    .map_err(|e| anyhow!("读取站点 `{site_key}` 的查询设置失败：{e:?}"))?;
    // 等同看板查询，用看板档的超时（§10 Q14）。
    let opts = ExecOptions::new(config.query_timeouts.dashboard_secs)
        .with_route("cli.query")
        .with_rollup(config.rollup_min_rows);
    let result = rivu_query::execute(&reader, &TenantScope::new(site.id), &ctx, spec, opts)
        .await
        .map_err(|e| anyhow!("查询失败：{e}"))?;
    println!("{}", serde_json::to_string_pretty(&result)?);
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
