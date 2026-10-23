//! `rivu apikey create | list | revoke`：公开 API 的 API key 管理。
//!
//! key 的格式、生成与哈希在 `rivu-auth`（`rivu_` + 32 字节随机数的 base64url，库里只存 SHA-256 与前 12 个字符）。
//! 明文只在 `create` 时输出一次，之后无法找回。直接连 PostgreSQL（`RIVU_DATABASE_URL`，应用账号）。

use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use jiff::civil::Date;
use jiff::tz::TimeZone;
use jiff::{Span, Timestamp};
use rivu_auth::key::generate;
use rivu_auth::scope::{DEFAULT_SCOPE, Scope};
use rivu_common::config::Config;
use rivu_storage_pg::api_keys::NewApiKey;
use sqlx::PgPool;

use crate::cli::{ApikeyAction, ApikeyCreateArgs, ApikeyListArgs, ApikeyRevokeArgs};

pub async fn run(config: &Config, action: ApikeyAction) -> Result<ExitCode> {
    match action {
        ApikeyAction::Create(args) => create(config, args).await,
        ApikeyAction::List(args) => list(config, args).await,
        ApikeyAction::Revoke(args) => revoke(config, args).await,
    }
}

/// 解析并规范化 `--scopes`：省略目标的写法（`stats:read`）补成 `*`，去重，保持顺序；空列表用默认 scope。
pub fn normalize_scopes(raw: &[String]) -> Result<Vec<String>> {
    let mut out: Vec<String> = Vec::new();
    for s in raw.iter().filter(|s| !s.trim().is_empty()) {
        let n = Scope::normalize(s).map_err(anyhow::Error::msg)?;
        if !out.contains(&n) {
            out.push(n);
        }
    }
    if out.is_empty() {
        out.push(DEFAULT_SCOPE.to_owned());
    }
    Ok(out)
}

/// 解析 `--expires`，返回 Unix 秒。支持三种写法：
/// - 时长：`90d`、`12h`、`30m`（从 `now` 起算）；
/// - RFC 3339 时刻：`2027-01-01T00:00:00Z`、`2027-01-01T08:00:00+08:00`；
/// - 日期：`2027-01-01`（UTC 当天零点）。
///
/// 必须晚于 `now`。
pub fn parse_expiry(raw: &str, now: Timestamp) -> Result<i64> {
    let raw = raw.trim();
    let at = if let Some(span) = parse_duration(raw) {
        now.checked_add(span).context("过期时间超出范围")?
    } else if let Ok(ts) = raw.parse::<Timestamp>() {
        ts
    } else if let Ok(d) = raw.parse::<Date>() {
        d.to_zoned(TimeZone::UTC)
            .context("日期超出范围")?
            .timestamp()
    } else {
        bail!(
            "`{raw}` 不是有效的过期时间：写成时长（90d、12h、30m）、RFC 3339 时刻或 YYYY-MM-DD 日期"
        );
    };
    if at <= now {
        bail!("过期时间 {at} 已经过去");
    }
    Ok(at.as_second())
}

/// `<正整数><d|h|m>`。
fn parse_duration(raw: &str) -> Option<Span> {
    let unit = raw.chars().last()?;
    let n: i64 = raw[..raw.len() - unit.len_utf8()]
        .parse()
        .ok()
        .filter(|n| *n > 0)?;
    match unit {
        // `Timestamp` 不接受日历单位（天），按 24 小时算
        'd' => Some(Span::new().hours(n.checked_mul(24)?)),
        'h' => Some(Span::new().hours(n)),
        'm' => Some(Span::new().minutes(n)),
        _ => None,
    }
}

/// 新建的 key（含明文：只有创建它的调用方见得到这一次）。
#[derive(Debug)]
pub struct IssuedKey {
    pub id: i64,
    pub plaintext: String,
    pub prefix: String,
    pub org_slug: String,
    pub user_email: String,
    pub scopes: Vec<String>,
    pub expires_at: Option<Timestamp>,
}

/// 校验参数、写入一把 key。`create` 在它之上打印结果；测试直接调用它拿到明文。
pub async fn issue_key(pool: &PgPool, args: &ApikeyCreateArgs) -> Result<IssuedKey> {
    let name = args.name.trim();
    if name.is_empty() || name.chars().count() > 100 {
        bail!("key 名称需要 1 到 100 个字符");
    }
    let scopes = normalize_scopes(&args.scopes)?;
    let expires_at = args
        .expires
        .as_deref()
        .map(|e| parse_expiry(e, Timestamp::now()))
        .transpose()?;

    let org_slug = args.org.trim();
    let Some(org_id) = rivu_storage_pg::api_keys::find_org_id_by_slug(pool, org_slug).await? else {
        bail!("找不到组织 `{org_slug}`（或已删除）");
    };
    let (user_id, user_email) =
        rivu_storage_pg::api_keys::resolve_key_owner(pool, org_id, args.user.as_deref()).await?;

    let key = generate();
    let id = rivu_storage_pg::api_keys::create_key(
        pool,
        &NewApiKey {
            organization_id: org_id,
            user_id,
            name,
            key_prefix: key.prefix(),
            key_hash: key.hash(),
            scopes: &scopes,
            expires_at_unix: expires_at,
        },
    )
    .await?;
    Ok(IssuedKey {
        id,
        plaintext: key.plaintext().to_owned(),
        prefix: key.prefix().to_owned(),
        org_slug: org_slug.to_owned(),
        user_email,
        scopes,
        expires_at: expires_at.and_then(|s| Timestamp::from_second(s).ok()),
    })
}

async fn create(config: &Config, args: ApikeyCreateArgs) -> Result<ExitCode> {
    let pool = rivu_storage_pg::connect(config.database_url.expose(), 2).await?;
    let key = issue_key(&pool, &args).await?;
    println!("API key 已创建。明文只显示这一次，之后无法找回，请立即保存。");
    println!("key: {}", key.plaintext);
    println!("id: {}", key.id);
    println!("prefix: {}", key.prefix);
    println!("org: {}", key.org_slug);
    println!("user: {}", key.user_email);
    println!("scopes: {}", key.scopes.join(", "));
    match key.expires_at {
        Some(ts) => println!("expires: {ts}"),
        None => println!("expires: never"),
    }
    Ok(ExitCode::SUCCESS)
}
async fn list(config: &Config, args: ApikeyListArgs) -> Result<ExitCode> {
    let pool = rivu_storage_pg::connect(config.database_url.expose(), 2).await?;
    let rows = rivu_storage_pg::api_keys::list_keys(&pool, args.org.as_deref()).await?;
    if rows.is_empty() {
        println!("没有 API key");
        return Ok(ExitCode::SUCCESS);
    }
    println!(
        "{:<6}  {:<12}  {:<14}  {:<28}  {:<9}  {:<20}  {:<20}  name / scopes",
        "id", "prefix", "org", "user", "state", "last_used", "expires"
    );
    for r in &rows {
        let state = if r.revoked { "revoked" } else { "active" };
        println!(
            "{:<6}  {:<12}  {:<14}  {:<28}  {:<9}  {:<20}  {:<20}  {} / {}",
            r.id,
            r.key_prefix,
            r.org_slug,
            r.user_email,
            state,
            r.last_used_at.as_deref().unwrap_or("-"),
            r.expires_at.as_deref().unwrap_or("never"),
            r.name,
            r.scopes.join(",")
        );
    }
    Ok(ExitCode::SUCCESS)
}

async fn revoke(config: &Config, args: ApikeyRevokeArgs) -> Result<ExitCode> {
    let pool = rivu_storage_pg::connect(config.database_url.expose(), 2).await?;
    if !rivu_storage_pg::api_keys::revoke_key(&pool, args.id).await? {
        bail!("没有 id 为 {} 的有效 API key（不存在或已吊销）", args.id);
    }
    println!(
        "已吊销 API key {}；每个 API 请求都实时查库，立即失效",
        args.id
    );
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ts(s: &str) -> Timestamp {
        s.parse().unwrap()
    }

    #[test]
    fn scopes_default_normalize_and_dedupe() {
        assert_eq!(normalize_scopes(&[]).unwrap(), ["stats:read:*"]);
        assert_eq!(normalize_scopes(&["".into()]).unwrap(), ["stats:read:*"]);
        assert_eq!(
            normalize_scopes(&[
                "stats:read".into(),
                "sites:read".into(),
                "stats:read:*".into()
            ])
            .unwrap(),
            ["stats:read:*", "sites:read:*"]
        );
        assert_eq!(
            normalize_scopes(&["stats:read:abcdefghijkl".into()]).unwrap(),
            ["stats:read:abcdefghijkl"]
        );
        for bad in ["stats", "stats:write", "members:read", "stats:read:nope"] {
            assert!(normalize_scopes(&[bad.into()]).is_err(), "{bad}");
        }
    }

    #[test]
    fn expiry_accepts_durations_timestamps_and_dates() {
        let now = ts("2026-10-23T00:00:00Z");
        assert_eq!(
            parse_expiry("90d", now).unwrap(),
            ts("2027-01-21T00:00:00Z").as_second()
        );
        assert_eq!(
            parse_expiry("12h", now).unwrap(),
            ts("2026-10-23T12:00:00Z").as_second()
        );
        assert_eq!(
            parse_expiry("30m", now).unwrap(),
            ts("2026-10-23T00:30:00Z").as_second()
        );
        assert_eq!(
            parse_expiry("2027-01-01T00:00:00Z", now).unwrap(),
            ts("2027-01-01T00:00:00Z").as_second()
        );
        assert_eq!(
            parse_expiry("2027-01-01T08:00:00+08:00", now).unwrap(),
            ts("2027-01-01T00:00:00Z").as_second()
        );
        assert_eq!(
            parse_expiry("2027-01-01", now).unwrap(),
            ts("2027-01-01T00:00:00Z").as_second()
        );
    }

    #[test]
    fn expiry_rejects_the_past_and_garbage() {
        let now = ts("2026-10-23T00:00:00Z");
        for bad in [
            "",
            "0d",
            "-5d",
            "5x",
            "d",
            "yesterday",
            "2026-10-22",
            "2026-10-23T00:00:00Z",
            "2020-01-01T00:00:00Z",
            "2026-13-01",
        ] {
            assert!(parse_expiry(bad, now).is_err(), "{bad:?}");
        }
    }
}
