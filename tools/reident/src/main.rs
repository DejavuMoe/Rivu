//! 重识别测试（`docs/brief-p1.md` 验收：「用数据库导出做重识别测试，结论写进隐私说明」）。
//!
//! 以「拿到了数据库导出的人」为攻击者，检查五件事：
//!
//! 1. **跨天关联（visitor_id）**：同一站点同一个 `visitor_id` 的事件时间跨度超过 48 小时的个数。访客 ID 按站点本地日的盐计算，
//!    一个本地日加上时区偏移最多覆盖约 50 小时的 UTC 时间，超过 48 小时（再留余量）只能来自哈希碰撞；期望为 0。
//! 2. **跨天关联（session_id）**：按设计，连续活跃（间隔不超过 30 分钟）的会话最长 7 天，`session_id` 会把它跨过的几天串起来；
//!    报告跨过 24 小时的会话数与最长时长。
//! 3. **准标识符唯一性**：每个站点每个 UTC 日，按（国家、地区、城市、设备、浏览器与版本、系统与版本）分组，
//!    数每组的访客数（k）。k = 1 的访客在这一天里由这些属性唯一确定：结合站外信息（比如知道某人用的手机型号与城市）可能认出来。
//! 4. **个人信息扫描**：pathname、入口页、referrer、UTM、属性值里形如邮箱、长数字串（电话、证件号）、IPv4 地址的内容。
//! 5. **盐的寿命与暴力还原成本**：PG 里现存的盐的日期（只应有 UTC 的 today−3 … today+2）；在本机测 SipHash 的速度，
//!    估算「拿到某天的盐之后，遍历全部 IPv4 × 一组常见 UA 还原 IP」要多久。盐删除之后，128 位的盐无法遍历。
//!
//! 只读：ClickHouse 用 reader 账号，PG 只读 `salts` 的日期（不读盐的值）。

use std::collections::BTreeMap;
use std::net::{IpAddr, Ipv4Addr};
use std::path::PathBuf;
use std::time::Instant;

use anyhow::{Context, Result};
use clap::Parser;
use rivu_common::config::{ChAccount, Secret};
use serde::Serialize;

#[derive(Parser, Debug)]
#[command(about = "重识别测试")]
struct Args {
    #[arg(long, default_value = "http://127.0.0.1:8123")]
    ch_url: String,
    #[arg(long, default_value = "rivu")]
    ch_database: String,
    #[arg(long, default_value = "rivu_reader")]
    ch_user: String,
    #[arg(long, env = "REIDENT_CH_PASSWORD", hide_env_values = true)]
    ch_password: String,
    /// PostgreSQL 连接串（只读 salts 的日期）；省略则跳过第 5 项的盐检查
    #[arg(long, env = "REIDENT_DATABASE_URL", hide_env_values = true)]
    database_url: Option<String>,
    /// 只看这些站点（逗号分隔）；省略则全部
    #[arg(long, value_delimiter = ',')]
    sites: Vec<u64>,
    /// 暴力还原成本估算里的「常见 UA」个数
    #[arg(long, default_value_t = 1000)]
    ua_candidates: u64,
    #[arg(long)]
    out: Option<PathBuf>,
}

#[derive(Debug, Serialize, Default)]
struct Report {
    events: u64,
    sites: u64,
    days: u64,
    visitor_ids: u64,
    /// 1. visitor_id 跨度超过 48 小时的 (站点, visitor_id) 个数
    visitor_ids_spanning_over_48h: u64,
    /// 2. 跨过 24 小时的会话数、最长会话的小时数
    sessions_over_24h: u64,
    longest_session_hours: f64,
    /// 3. 访客按所在准标识符组的大小 k 分布：k -> 访客数（k ≥ 10 合并为 10）
    k_distribution: BTreeMap<u64, u64>,
    unique_visitor_share: f64,
    /// 4. 个人信息扫描：字段 -> 模式 -> 命中行数
    pii_hits: BTreeMap<String, BTreeMap<String, u64>>,
    pii_examples: Vec<String>,
    /// 5. 盐
    salt_dates: Option<Vec<String>>,
    siphash_per_sec: f64,
    ipv4_sweep_one_ua_secs: f64,
    ipv4_sweep_ua_candidates_hours: f64,
}

fn site_filter(sites: &[u64]) -> String {
    if sites.is_empty() {
        "1".into()
    } else {
        format!(
            "site_id IN ({})",
            sites
                .iter()
                .map(u64::to_string)
                .collect::<Vec<_>>()
                .join(",")
        )
    }
}

/// 把命中的值打码：只保留前 3 个字符，其余换成 *。
fn mask(s: &str) -> String {
    s.chars()
        .enumerate()
        .map(|(i, c)| if i < 3 { c } else { '*' })
        .take(40)
        .collect()
}

/// 单核 SipHash-2-4（visitor_hash 同一实现）每秒的次数。
fn siphash_rate() -> f64 {
    let salt = [7u8; 16];
    let ua = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/141.0.0.0 Safari/537.36";
    let n: u32 = 5_000_000;
    let start = Instant::now();
    let mut acc = 0u64;
    for i in 0..n {
        acc ^= rivu_session::identity::visitor_hash(&salt, 1, IpAddr::V4(Ipv4Addr::from(i)), ua);
    }
    let secs = start.elapsed().as_secs_f64();
    std::hint::black_box(acc);
    f64::from(n) / secs
}

async fn run(args: &Args) -> Result<Report> {
    let account = ChAccount {
        user: args.ch_user.clone(),
        password: Secret::new(args.ch_password.clone()),
    };
    let ch = rivu_storage_ch::client(&args.ch_url, &account, Some(&args.ch_database));
    let f = site_filter(&args.sites);
    let mut r = Report::default();

    let (events, sites, days, visitors): (u64, u64, u64, u64) = ch
        .query(&format!(
            "SELECT count(), uniqExact(site_id), uniqExact(site_id, toDate(ts)), uniqExact(site_id, visitor_id)
             FROM events WHERE {f}"
        ))
        .fetch_one()
        .await
        .context("统计事件失败")?;
    (r.events, r.sites, r.days, r.visitor_ids) = (events, sites, days, visitors);

    // 1. visitor_id 的时间跨度
    r.visitor_ids_spanning_over_48h = ch
        .query(&format!(
            "SELECT count() FROM (SELECT site_id, visitor_id FROM events WHERE {f}
             GROUP BY site_id, visitor_id HAVING dateDiff('hour', min(ts), max(ts)) > 48)"
        ))
        .fetch_one()
        .await?;

    // 2. 会话跨度
    let (over24, longest_ms): (u64, i64) = ch
        .query(&format!(
            "SELECT countIf(span > 86400000), max(span) FROM (
                SELECT toInt64(toUnixTimestamp64Milli(max(ts)) - toUnixTimestamp64Milli(min(ts))) AS span
                FROM events WHERE {f} GROUP BY site_id, session_id)"
        ))
        .fetch_one()
        .await?;
    r.sessions_over_24h = over24;
    r.longest_session_hours = longest_ms as f64 / 3_600_000.0;

    // 3. 准标识符组的大小
    let rows: Vec<(u64, u64)> = ch
        .query(&format!(
            "SELECT least(k, 10) AS kk, sum(k) FROM (
                SELECT uniqExact(visitor_id) AS k FROM events WHERE {f} AND name = 'pageview'
                GROUP BY site_id, toDate(ts), country_code, region_code, city_geoname_id, device,
                         browser, browser_version, os, os_version)
             GROUP BY kk ORDER BY kk"
        ))
        .fetch_all()
        .await?;
    let total: u64 = rows.iter().map(|(_, n)| n).sum();
    for (k, n) in rows {
        r.k_distribution.insert(k, n);
    }
    r.unique_visitor_share = if total == 0 {
        0.0
    } else {
        r.k_distribution.get(&1).copied().unwrap_or(0) as f64 / total as f64
    };

    // 4. 个人信息扫描（RE2 语法）
    let patterns = [
        ("email", r"[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}"),
        ("long_digits", r"[0-9]{9,}"),
        ("ipv4", r"(^|[^0-9.])([0-9]{1,3}\.){3}[0-9]{1,3}([^0-9.]|$)"),
    ];
    let fields = [
        ("pathname", "pathname"),
        ("entry_pathname", "entry_pathname"),
        ("referrer", "referrer"),
        (
            "utm",
            "concat(utm_source, ' ', utm_medium, ' ', utm_campaign, ' ', utm_content, ' ', utm_term)",
        ),
        ("props", "arrayStringConcat(mapValues(props), ' ')"),
    ];
    for (fname, expr) in fields {
        for (pname, re) in patterns {
            let n: u64 = ch
                .query(&format!(
                    "SELECT count() FROM events WHERE {f} AND match({expr}, {{re:String}})"
                ))
                .param("re", re)
                .fetch_one()
                .await?;
            r.pii_hits
                .entry(fname.to_owned())
                .or_default()
                .insert(pname.to_owned(), n);
            if n > 0 && r.pii_examples.len() < 10 {
                let ex: Vec<String> = ch
                    .query(&format!(
                        "SELECT DISTINCT extract({expr}, {{re:String}}) FROM events
                         WHERE {f} AND match({expr}, {{re:String}}) LIMIT 2"
                    ))
                    .param("re", re)
                    .fetch_all()
                    .await?;
                for e in ex {
                    r.pii_examples
                        .push(format!("{fname}/{pname}: {}", mask(&e)));
                }
            }
        }
    }

    // 5. 盐与暴力还原成本
    if let Some(url) = &args.database_url {
        let pg = rivu_storage_pg::connect(url, 1).await?;
        let dates: Vec<String> = sqlx::query_scalar("SELECT day::text FROM salts ORDER BY day")
            .fetch_all(&pg)
            .await
            .context("读取盐的日期失败")?;
        r.salt_dates = Some(dates);
    }
    r.siphash_per_sec = siphash_rate();
    r.ipv4_sweep_one_ua_secs = 4_294_967_296.0 / r.siphash_per_sec;
    r.ipv4_sweep_ua_candidates_hours =
        r.ipv4_sweep_one_ua_secs * args.ua_candidates as f64 / 3600.0;
    Ok(r)
}

fn print(r: &Report, ua_candidates: u64) {
    println!(
        "数据：{} 个事件，{} 个站点，{} 个站点日，{} 个 (站点, visitor_id)",
        r.events, r.sites, r.days, r.visitor_ids
    );
    println!(
        "1. visitor_id 跨度超过 48 小时：{}（期望 0）",
        r.visitor_ids_spanning_over_48h
    );
    println!(
        "2. 跨过 24 小时的会话：{}，最长 {:.1} 小时（按设计 ≤ 7 天）",
        r.sessions_over_24h, r.longest_session_hours
    );
    println!(
        "3. 准标识符组大小 k 的访客分布（k≥10 合并）：{:?}；k=1 的访客占 {:.1}%",
        r.k_distribution,
        r.unique_visitor_share * 100.0
    );
    println!("4. 个人信息扫描（命中行数）：{:?}", r.pii_hits);
    for e in &r.pii_examples {
        println!("   例：{e}");
    }
    if let Some(d) = &r.salt_dates {
        println!("5. PG 里现存的盐：{d:?}");
    }
    println!(
        "   SipHash 单核 {:.1} M/s：遍历 IPv4 × 1 个 UA 约 {:.0} 秒；× {ua_candidates} 个 UA 约 {:.1} 小时（单核）",
        r.siphash_per_sec / 1e6,
        r.ipv4_sweep_one_ua_secs,
        r.ipv4_sweep_ua_candidates_hours
    );
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let r = run(&args).await?;
    print(&r, args.ua_candidates);
    if let Some(p) = &args.out {
        std::fs::write(p, serde_json::to_string_pretty(&r)?)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mask_keeps_three_chars() {
        assert_eq!(mask("alice@example.com"), "ali**************");
        assert_eq!(mask("ab"), "ab");
    }

    #[test]
    fn site_filter_shapes() {
        assert_eq!(site_filter(&[]), "1");
        assert_eq!(site_filter(&[3, 4]), "site_id IN (3,4)");
    }
}
