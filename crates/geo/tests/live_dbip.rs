//! DB-IP Lite 实测（需要网络，默认 `#[ignore]`）。
//!
//! 手动运行：
//!
//! ```text
//! # 1. 先把五个 RIR 的 delegated 统计文件下载到一个目录（公开记录，每日更新）：
//! #    https://ftp.apnic.net/stats/apnic/delegated-apnic-latest
//! #    https://ftp.arin.net/pub/stats/arin/delegated-arin-extended-latest
//! #    https://ftp.ripe.net/pub/stats/ripencc/delegated-ripencc-latest
//! #    https://ftp.lacnic.net/pub/stats/lacnic/delegated-lacnic-latest
//! #    https://ftp.afrinic.net/pub/stats/afrinic/delegated-afrinic-latest
//! $env:RIVU_RIR_DIR = "<上面文件所在目录>"
//! $env:RIVU_GEO_LIVE_EDITION = "country"   # 或 city（约 60 MB 压缩包），默认 country
//! cargo test -p rivu-geo --test live_dbip -- --ignored --nocapture
//! ```
//!
//! 做三件事：
//! 1. 用 `refresh()` 对真实的 `download.db-ip.com` 下载一次（真实 TLS、真实重定向与校验头），
//!    再刷新一次，应当得到 304；
//! 2. 校验通过后加载，署名与 DB-IP 下载页给出的文字一致；
//! 3. 取 RIR 公开分配记录里的大量地址块（delegated 统计文件：地址段 -> 国家），在每个块的中间取一个地址，
//!    与 DB-IP 的国家比较，报告一致率。RIR 记录的是「注册国家」，DB-IP 是「地理位置」，
//!    跨国使用的地址块本来就会不一致，所以不要求 100%。

mod common;

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::path::PathBuf;
use std::time::Instant;

use common::TempDir;
use rivu_common::config::{DbIpEdition, GeoConfig, GeoSource};
use rivu_geo::{GeoDb, GeoLevel, HttpClient, RefreshOutcome, read_meta, refresh};

struct Block {
    rir: String,
    cc: String,
    ip: IpAddr,
    text: String,
    /// 块里的地址数（IPv6 记 0，不参与加权）
    weight: u64,
}

/// 解析一个 delegated 文件，每个符合条件的记录产出一个样本地址。
fn parse_delegated(rir: &str, text: &str) -> Vec<Block> {
    let mut out = Vec::new();
    for line in text.lines() {
        if line.starts_with('#') {
            continue;
        }
        let f: Vec<&str> = line.split('|').collect();
        if f.len() < 7 || !matches!(f[6], "allocated" | "assigned") {
            continue;
        }
        let cc = f[1];
        if cc.len() != 2
            || !cc.bytes().all(|b| b.is_ascii_uppercase())
            || matches!(cc, "EU" | "AP" | "ZZ")
        {
            continue;
        }
        match f[2] {
            "ipv4" => {
                let (Ok(start), Ok(count)) = (f[3].parse::<Ipv4Addr>(), f[4].parse::<u64>()) else {
                    continue;
                };
                // 只取 /24 及更大的块，样本取块的中间
                if count < 256 {
                    continue;
                }
                let mid = u64::from(u32::from(start)) + count / 2;
                out.push(Block {
                    rir: rir.into(),
                    cc: cc.into(),
                    ip: IpAddr::V4(Ipv4Addr::from(u32::try_from(mid).unwrap())),
                    text: format!("{}/{count} 的中点", f[3]),
                    weight: count,
                });
            }
            "ipv6" => {
                let (Ok(start), Ok(len)) = (f[3].parse::<Ipv6Addr>(), f[4].parse::<u32>()) else {
                    continue;
                };
                if !(16..=64).contains(&len) {
                    continue;
                }
                let sample = u128::from(start) + (1u128 << (127 - len));
                out.push(Block {
                    rir: rir.into(),
                    cc: cc.into(),
                    ip: IpAddr::V6(Ipv6Addr::from(sample)),
                    text: format!("{}/{len} 的中点", f[3]),
                    weight: 0,
                });
            }
            _ => {}
        }
    }
    out
}

/// 固定种子的洗牌，保证每次抽到同一批样本。
fn shuffle<T>(v: &mut [T], mut seed: u64) {
    for i in (1..v.len()).rev() {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        v.swap(i, (seed % (i as u64 + 1)) as usize);
    }
}

#[tokio::test]
#[ignore = "需要网络：下载 DB-IP Lite（country 约 4 MB，city 约 60 MB）；还需要 RIVU_RIR_DIR"]
async fn dbip_lite_live_download_and_agreement_with_rir_delegations() {
    let edition = match std::env::var("RIVU_GEO_LIVE_EDITION").as_deref() {
        Ok("city") => DbIpEdition::City,
        _ => DbIpEdition::Country,
    };
    let rir_dir = PathBuf::from(
        std::env::var("RIVU_RIR_DIR").expect("请设置 RIVU_RIR_DIR，见本文件顶部的说明"),
    );

    // --- 1. 真实下载 ---
    let dir = TempDir::new("live");
    let mut cfg = GeoConfig::new(dir.path());
    cfg.source = GeoSource::DbIp { edition };
    let http = HttpClient::new().unwrap();
    let started = Instant::now();
    let out = refresh(&cfg, &http).await.expect("下载 DB-IP Lite 失败");
    let elapsed = started.elapsed();
    let RefreshOutcome::Updated {
        sha256,
        build_epoch,
        bytes,
    } = out
    else {
        panic!("首次下载应当是 Updated：{out:?}");
    };
    let meta = read_meta(&cfg.db_path).unwrap();
    eprintln!("== 下载 {edition:?}");
    eprintln!("   耗时 {elapsed:?}，解压后 {bytes} 字节，sha256 {sha256}");
    eprintln!(
        "   build_epoch {build_epoch}，database_type {}",
        meta.database_type
    );
    eprintln!(
        "   etag {:?}，last_modified {:?}",
        meta.etag, meta.last_modified
    );
    eprintln!("   来源标识 {}", meta.source_id);
    assert!(
        meta.database_type.starts_with("DBIP-"),
        "{}",
        meta.database_type
    );
    assert!(
        meta.etag.is_some() || meta.last_modified.is_some(),
        "DB-IP 应当提供校验头"
    );

    // 第二次：真实服务器应当对条件 GET 回 304
    let again = refresh(&cfg, &http).await.unwrap();
    eprintln!("   第二次刷新：{again:?}");
    assert_eq!(again, RefreshOutcome::NotModified { by_hash: false });

    // --- 2. 加载与署名 ---
    let db = GeoDb::from_config(&cfg);
    assert!(db.reload_if_changed(&cfg.db_path).unwrap());
    let a = db.attribution().unwrap();
    eprintln!("   署名：{a:?}");
    assert_eq!(a.source, "dbip-lite");
    assert_eq!(a.text, "IP Geolocation by DB-IP"); // 页面给出的署名 HTML：<a href='https://db-ip.com'>IP Geolocation by DB-IP</a>
    assert_eq!(a.url, "https://db-ip.com");

    // 原始记录长什么样（看字段，不看数值）
    let reader = maxminddb::Reader::open_readfile(&cfg.db_path).unwrap();
    for probe in ["8.8.8.8", "114.114.114.114", "2001:4860:4860::8888"] {
        let rec: Option<serde_json::Value> = reader
            .lookup(probe.parse().unwrap())
            .unwrap()
            .decode()
            .unwrap();
        eprintln!(
            "   原始记录 {probe}: {}",
            rec.map_or("(无)".into(), |v| v.to_string())
        );
    }

    // --- 3. 与 RIR 公开分配记录比较 ---
    let mut blocks = Vec::new();
    for (file, rir) in [
        ("delegated-apnic-latest", "apnic"),
        ("delegated-arin-extended-latest", "arin"),
        ("delegated-ripencc-latest", "ripencc"),
        ("delegated-lacnic-latest", "lacnic"),
        ("delegated-afrinic-latest", "afrinic"),
    ] {
        let text = std::fs::read_to_string(rir_dir.join(file))
            .unwrap_or_else(|e| panic!("读取 {file} 失败：{e}"));
        blocks.extend(parse_delegated(rir, &text));
    }
    let (mut v4, mut v6): (Vec<Block>, Vec<Block>) =
        blocks.into_iter().partition(|b| b.ip.is_ipv4());
    eprintln!("== RIR 样本池：IPv4 块 {}，IPv6 块 {}", v4.len(), v6.len());
    shuffle(&mut v4, 0x9E37_79B9_7F4A_7C15);
    shuffle(&mut v6, 0xD1B5_4A32_D192_ED03);
    v4.truncate(5000);
    v6.truncate(2000);

    let mut total = 0usize;
    let mut agree = 0usize;
    let mut unknown = 0usize;
    let (mut w_total, mut w_agree) = (0u64, 0u64);
    let mut by_rir: HashMap<String, (usize, usize)> = HashMap::new();
    let mut mismatches: HashMap<(String, String), usize> = HashMap::new();
    let mut examples = Vec::new();
    let (mut with_region, mut with_city) = (0usize, 0usize);
    for b in v4.iter().chain(v6.iter()) {
        let info = db.lookup(b.ip, GeoLevel::City);
        total += 1;
        with_region += usize::from(!info.region_code.is_empty());
        with_city += usize::from(info.city_geoname_id != 0);
        let got = String::from_utf8_lossy(&info.country_code).into_owned();
        let ok = info.country_code != [0, 0] && got == b.cc;
        let family = if b.ip.is_ipv4() { "ipv4" } else { "ipv6" };
        for key in [b.rir.clone(), family.to_string()] {
            let e = by_rir.entry(key).or_default();
            e.0 += 1;
            e.1 += usize::from(ok);
        }
        w_total += b.weight;
        if ok {
            w_agree += b.weight;
            agree += 1;
        } else if info.country_code == [0, 0] {
            unknown += 1;
        } else {
            *mismatches.entry((b.cc.clone(), got.clone())).or_default() += 1;
            if examples.len() < 12 {
                examples.push(format!(
                    "{} ({} {}) RIR={} DB-IP={got}",
                    b.ip, b.rir, b.text, b.cc
                ));
            }
        }
    }
    let rate = agree as f64 / total as f64;
    eprintln!(
        "== 一致性：{agree}/{total} = {:.2}%（DB-IP 返回空值 {unknown}）",
        rate * 100.0
    );
    eprintln!(
        "   IPv4 按地址数加权：{:.2}%（{w_agree}/{w_total}）",
        w_agree as f64 / w_total as f64 * 100.0
    );
    let mut rows: Vec<_> = by_rir.into_iter().collect();
    rows.sort();
    for (k, (n, ok)) in rows {
        eprintln!("   {k:8} {ok}/{n} = {:.2}%", ok as f64 / n as f64 * 100.0);
    }
    let mut top: Vec<_> = mismatches.into_iter().collect();
    top.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    eprintln!("   最常见的不一致（RIR 国家 -> DB-IP 国家）：");
    for ((rir_cc, got), n) in top.iter().take(12) {
        eprintln!("     {rir_cc} -> {got}: {n}");
    }
    for e in &examples {
        eprintln!("   例：{e}");
    }
    eprintln!("   区域非空 {with_region}/{total}，城市 geoname_id 非零 {with_city}/{total}");
    assert!(
        rate >= 0.85,
        "与 RIR 注册国家的一致率应不低于 85%，实际 {:.2}%",
        rate * 100.0
    );
}
