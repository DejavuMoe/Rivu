//! `rivu shield add | list | remove`：站点屏蔽规则的命令行管理。
//!
//! 直接连 PostgreSQL（`RIVU_DATABASE_URL`，应用账号）。写入前按类型校验并规范化取值
//! （`rivu_ingest::shield::normalize_value`），并在事务内检查每站上限 `RIVU_SHIELD_MAX_RULES_PER_SITE`。
//! 规则写库后，运行中的 collector 在下一次站点快照重载（`RIVU_SITE_CACHE_REFRESH_SECS`，默认 10 秒）内生效。

use std::process::ExitCode;

use anyhow::{Result, anyhow, bail};
use rivu_common::config::Config;
use rivu_ingest::shield::{RuleAction, RuleType, normalize_value};
use rivu_storage_pg::shield::{AddOutcome, NewRule};
use sqlx::PgPool;

use crate::cli::{
    ShieldAction, ShieldActionKind, ShieldAddArgs, ShieldListArgs, ShieldRemoveArgs, ShieldType,
};

/// 规则备注的字符数上限（命令行与看板端点共用）。
pub const DESCRIPTION_MAX_CHARS: usize = 200;

impl From<ShieldType> for RuleType {
    fn from(t: ShieldType) -> Self {
        match t {
            ShieldType::Ip => RuleType::Ip,
            ShieldType::Cidr => RuleType::Cidr,
            ShieldType::Country => RuleType::Country,
            ShieldType::Hostname => RuleType::Hostname,
            ShieldType::Page => RuleType::Page,
        }
    }
}

impl From<ShieldActionKind> for RuleAction {
    fn from(a: ShieldActionKind) -> Self {
        match a {
            ShieldActionKind::Deny => RuleAction::Deny,
            ShieldActionKind::Allow => RuleAction::Allow,
        }
    }
}

pub async fn run(config: &Config, action: ShieldAction) -> Result<ExitCode> {
    match action {
        ShieldAction::Add(args) => add(config, args).await,
        ShieldAction::List(args) => list(config, args).await,
        ShieldAction::Remove(args) => remove(config, args).await,
    }
}

/// 站点 id；找不到（或已删除）报错。
async fn site_id(pool: &PgPool, site_key: &str) -> Result<u64> {
    match rivu_storage_pg::sites::find_site_by_key(pool, site_key).await? {
        Some(site) => Ok(site.id),
        None => bail!("找不到站点 `{site_key}`（或已删除）"),
    }
}

async fn add(config: &Config, args: ShieldAddArgs) -> Result<ExitCode> {
    let kind = RuleType::from(args.kind);
    let action = RuleAction::from(args.action);
    let value = normalize_value(kind, &args.value).map_err(|e| anyhow!(e))?;
    let description = args
        .description
        .as_deref()
        .map(str::trim)
        .filter(|d| !d.is_empty());
    if description.is_some_and(|d| d.chars().count() > DESCRIPTION_MAX_CHARS) {
        bail!("备注不能超过 {DESCRIPTION_MAX_CHARS} 个字符");
    }

    let pool = rivu_storage_pg::connect(config.database_url.expose(), 2).await?;
    let site_id = site_id(&pool, &args.site).await?;
    let outcome = rivu_storage_pg::shield::add_rule(
        &pool,
        &NewRule {
            site_id,
            kind: kind.as_str(),
            action: action.as_str(),
            value: &value,
            description,
            created_by: None,
        },
        config.shield_max_rules_per_site,
    )
    .await?;
    match outcome {
        AddOutcome::Added(id) => {
            println!("id: {id}");
            println!("site_key: {}", args.site);
            println!("type: {kind}");
            println!("action: {action}");
            println!("value: {value}");
            println!(
                "已写入；运行中的 collector 会在下一次站点快照重载（RIVU_SITE_CACHE_REFRESH_SECS，默认 10 秒）内生效"
            );
            Ok(ExitCode::SUCCESS)
        }
        AddOutcome::Duplicate => bail!(
            "站点 `{}` 已有同类型同取值的规则（{kind} {value}；唯一约束不区分 deny 与 allow，要改动作请先删除再添加）",
            args.site
        ),
        AddOutcome::LimitReached { limit } => bail!(
            "站点 `{}` 的规则已达上限 {limit} 条（RIVU_SHIELD_MAX_RULES_PER_SITE）",
            args.site
        ),
        AddOutcome::SiteNotFound => bail!("找不到站点 `{}`（或已删除）", args.site),
    }
}

async fn list(config: &Config, args: ShieldListArgs) -> Result<ExitCode> {
    let pool = rivu_storage_pg::connect(config.database_url.expose(), 2).await?;
    let site_id = site_id(&pool, &args.site).await?;
    let rules = rivu_storage_pg::shield::list_rules(&pool, site_id).await?;
    if rules.is_empty() {
        println!("站点 `{}` 没有屏蔽规则", args.site);
        return Ok(ExitCode::SUCCESS);
    }
    println!("{:<36}  {:<8}  {:<6}  value", "id", "type", "action");
    for r in &rules {
        let note = r
            .description
            .as_deref()
            .map(|d| format!("  # {d}"))
            .unwrap_or_default();
        println!(
            "{:<36}  {:<8}  {:<5}  {}{note}",
            r.id, r.kind, r.action, r.value
        );
    }
    println!(
        "共 {} 条（上限 {}）",
        rules.len(),
        config.shield_max_rules_per_site
    );
    Ok(ExitCode::SUCCESS)
}

async fn remove(config: &Config, args: ShieldRemoveArgs) -> Result<ExitCode> {
    let pool = rivu_storage_pg::connect(config.database_url.expose(), 2).await?;
    let site_id = site_id(&pool, &args.site).await?;
    let removed = match (&args.id, args.kind, &args.value) {
        (Some(id), None, None) => {
            rivu_storage_pg::shield::remove_rule_by_id(&pool, site_id, id.trim()).await?
        }
        (None, Some(kind), Some(value)) => {
            let kind = RuleType::from(kind);
            let value = normalize_value(kind, value).map_err(|e| anyhow!(e))?;
            rivu_storage_pg::shield::remove_rule_by_value(&pool, site_id, kind.as_str(), &value)
                .await?
        }
        _ => bail!("用 --id，或同时用 --type 和 --value 指定要删除的规则"),
    };
    if !removed {
        bail!("站点 `{}` 里没有匹配的规则（或已删除）", args.site);
    }
    println!("已删除；运行中的 collector 会在下一次站点快照重载内生效");
    Ok(ExitCode::SUCCESS)
}
