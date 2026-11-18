//! 命令行管理命令：`rivu site add`、`rivu query`、`rivu import`。
//!
//! 它们直接连 PostgreSQL（`RIVU_DATABASE_URL`，应用账号）和 ClickHouse（reader 账号），不经过 HTTP；
//! 配置按管理命令（`Role::Admin`）校验。结果写到 stdout，日志在 stderr。

use std::process::ExitCode;

use anyhow::{Context, Result, anyhow, bail};
use jiff::tz::TimeZone;
use rivu_common::config::Config;
use rivu_geo::GeoDb;
use rivu_query::{ExecOptions, QuerySpec, SiteContext, TenantScope};
use rivu_storage_pg::sites::NewSite;
use serde_json::{Value, json};

use crate::cli::{ExportArgs, ImportAction, MailAction, QueryArgs, SiteAddArgs};
use crate::export::ExportRequest;
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

    let (reader, ctx) = query_context(config, &site_key).await?;
    // 等同看板查询，用看板档的超时（§10 Q14）。
    let opts = ExecOptions::new(config.query_timeouts.dashboard_secs)
        .with_route("cli.query")
        .with_rollup(config.rollup_min_rows);
    let result = rivu_query::execute(&reader, &TenantScope::new(ctx.site_id), &ctx, spec, opts)
        .await
        .map_err(|e| anyhow!("查询失败：{e}"))?;
    println!("{}", serde_json::to_string_pretty(&result)?);
    Ok(ExitCode::SUCCESS)
}

/// 查询用的 `rivu_reader` 客户端与站点上下文（`rivu query`、`rivu export`）。
async fn query_context(
    config: &Config,
    site_key: &str,
) -> Result<(clickhouse::Client, SiteContext)> {
    let pool = rivu_storage_pg::connect(config.database_url.expose(), 2).await?;
    let Some(site) = rivu_storage_pg::sites::find_site_by_key(&pool, site_key).await? else {
        bail!("找不到站点 `{site_key}`（或已删除）");
    };
    let account = config
        .ch_reader
        .as_ref()
        .context("需要 RIVU_CH_READER_USER / RIVU_CH_READER_PASSWORD")?;
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
    Ok((reader, ctx))
}

/// `rivu export`：看板「导出」的命令行版本（[`crate::export`]），写一个 zip。
pub async fn export(config: &Config, args: ExportArgs) -> Result<ExitCode> {
    let date_range = match args.range.split_once(',') {
        Some((a, b)) => json!([a.trim(), b.trim()]),
        None => json!(args.range),
    };
    let filters: Value = match &args.filters {
        Some(f) => serde_json::from_str(f).context("--filters 不是有效的 JSON")?,
        None => json!([]),
    };
    let body = json!({"date_range": date_range, "filters": filters, "interval": args.interval});
    let request = ExportRequest::parse(&body).map_err(|e| anyhow!("导出参数无效：{e}"))?;
    let (reader, ctx) = query_context(config, &args.site).await?;
    let scope = TenantScope::new(ctx.site_id);
    let opts = ExecOptions::new(config.query_timeouts.dashboard_secs)
        .with_route("cli.export")
        .with_rollup(config.rollup_min_rows);
    let keys = rivu_query::property_keys(
        &reader,
        &scope,
        &ctx,
        request.keys_spec().map_err(|e| anyhow!("{e}"))?,
        opts.clone(),
    )
    .await
    .map_err(|e| anyhow!("读取属性键失败：{e}"))?;
    let keys: Vec<String> = keys.into_iter().map(|k| k.key).collect();
    let (reader, ctx_ref, scope_ref) = (&reader, &ctx, &scope);
    let export = crate::export::build(&request, &keys, move |spec| {
        rivu_query::execute(reader, scope_ref, ctx_ref, spec, opts.clone())
    })
    .await
    .map_err(|e| anyhow!("导出失败：{e}"))?;
    let path = args
        .out
        .unwrap_or_else(|| crate::export::file_name(&args.site, &export).into());
    std::fs::write(&path, &export.zip).with_context(|| format!("写入 {} 失败", path.display()))?;
    println!(
        "{}（{} – {}，{} 字节）",
        path.display(),
        export.from,
        export.to,
        export.zip.len()
    );
    Ok(ExitCode::SUCCESS)
}

/// `rivu import`：导入 Plausible 的数据导出、列出与删除导入。写汇总表用 `rivu_maintainer` 账号。
/// 运行中的 `rivu serve` 的结果缓存最多 10 分钟后才看到导入的数据。
pub async fn import(config: &Config, action: ImportAction) -> Result<ExitCode> {
    let site_key = match &action {
        ImportAction::Plausible { site, .. }
        | ImportAction::List { site }
        | ImportAction::Delete { site, .. } => site.clone(),
    };
    let pool = rivu_storage_pg::connect(config.database_url.expose(), 2).await?;
    let Some(site) = rivu_storage_pg::sites::find_site_by_key(&pool, &site_key).await? else {
        bail!("找不到站点 `{site_key}`（或已删除）");
    };
    let maintainer = || -> Result<clickhouse::Client> {
        let account = config
            .ch_maintainer
            .as_ref()
            .context("导入需要 RIVU_CH_MAINTAINER_USER / RIVU_CH_MAINTAINER_PASSWORD")?;
        Ok(rivu_storage_ch::client(
            &config.clickhouse_url,
            account,
            Some(&config.ch_database),
        ))
    };
    match action {
        ImportAction::Plausible { file, .. } => {
            let ch = maintainer()?;
            let name = file
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "export.zip".to_owned());
            let zip = std::fs::read(&file).with_context(|| format!("打不开 {}", file.display()))?;
            let (id, s) =
                rivu_jobs::import_plausible::import_file(&pool, &ch, site.id, zip, &name, None)
                    .await?;
            println!("import_id: {id}");
            println!("dates: {} – {}", s.start, s.end);
            println!("rows: {}", s.rows);
            if let Some(d) = s.clipped_from {
                println!("skipped: {d} 及之后（站点自己的数据从这天开始）");
            }
        }
        ImportAction::List { .. } => {
            for r in rivu_storage_pg::imports::list(&pool, site.id).await? {
                println!(
                    "{}\t{}\t{}\t{} – {}\t{} 行\t{}{}",
                    r.id,
                    r.status,
                    r.file_name,
                    r.start_date.as_deref().unwrap_or("-"),
                    r.end_date.as_deref().unwrap_or("-"),
                    r.rows_imported,
                    r.created_at,
                    r.error.map(|e| format!("\t{e}")).unwrap_or_default()
                );
            }
        }
        ImportAction::Delete { id, .. } => {
            if rivu_storage_pg::imports::get(&pool, site.id, id)
                .await?
                .is_none()
            {
                bail!("站点 `{site_key}` 没有导入 #{id}");
            }
            let ch = maintainer()?;
            let import_id = u64::try_from(id).context("导入 id 为负数")?;
            rivu_query::rollup::delete_import(&ch, &TenantScope::new(site.id), import_id)
                .await
                .map_err(|e| anyhow!("删除汇总行失败：{e}"))?;
            rivu_storage_pg::imports::delete(&pool, site.id, id).await?;
            println!("已删除导入 #{id}");
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// `rivu mail test`：用 `RIVU_SMTP_*` 连接 SMTP 服务器并发一封测试邮件。
pub async fn mail(config: &Config, action: MailAction) -> Result<ExitCode> {
    let MailAction::Test { to } = action;
    let smtp = config
        .smtp
        .as_ref()
        .context("没有配置 SMTP：设置 RIVU_SMTP_URL 与 RIVU_SMTP_FROM")?;
    let mailer = rivu_mail::Mailer::smtp(smtp.url.expose(), &smtp.from)?;
    mailer.check().await?;
    mailer.send(rivu_mail::test_mail(&to)).await?;
    println!("已发送测试邮件到 {to}");
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
