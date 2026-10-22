//! 用 MaxMind 官方测试库与它们的 JSON 源数据互相印证（来源见 `tests/data/SOURCE.md`）。
//!
//! 期望值只来自 `source-data/*.json`（生成 mmdb 的输入），与被测代码无关：对其中**每一个**网段，
//! 取首、中、尾三个地址，查询后逐条断言 country / region / city。多个网段重叠时取最长前缀。
//! 期望的推导规则（与 Plausible 一致，system-design §4.2 第 6 步）：
//! - country = `country.iso_code`（不回退到 `registered_country` 与 `represented_country`）；
//! - region = `<country>-<subdivisions[0].iso_code>`；
//! - city = `city.geoname_id`；
//! - 没有 country 时，region 与 city 也为空。

mod common;

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use common::{data, test_mmdb};
use rivu_geo::{GeoDb, GeoInfo, GeoLevel};
use serde_json::Value;

struct Network {
    v4: bool,
    first: u128,
    last: u128,
    prefix: u32,
    record: Value,
    text: String,
}

impl Network {
    fn contains(&self, ip: IpAddr) -> bool {
        let (v4, x) = to_u128(ip);
        v4 == self.v4 && (self.first..=self.last).contains(&x)
    }
}

fn to_u128(ip: IpAddr) -> (bool, u128) {
    match ip {
        IpAddr::V4(a) => (true, u128::from(u32::from(a))),
        IpAddr::V6(a) => (false, u128::from(a)),
    }
}

fn from_u128(v4: bool, x: u128) -> IpAddr {
    if v4 {
        IpAddr::V4(Ipv4Addr::from(u32::try_from(x).unwrap()))
    } else {
        IpAddr::V6(Ipv6Addr::from(x))
    }
}

fn load_networks(file: &str) -> Vec<Network> {
    let text = std::fs::read_to_string(data(&format!("source-data/{file}.json"))).unwrap();
    let docs: Vec<serde_json::Map<String, Value>> = serde_json::from_str(&text).unwrap();
    let mut out = Vec::new();
    for doc in docs {
        for (net, record) in doc {
            let (addr, prefix) = net.split_once('/').expect("源数据的键是 CIDR");
            let addr: IpAddr = addr.parse().unwrap();
            let prefix: u32 = prefix.parse().unwrap();
            let (v4, raw) = to_u128(addr);
            let width = if v4 { 32 } else { 128 };
            let host_bits = width - prefix;
            let mask = if host_bits == 0 {
                0
            } else {
                (1u128 << host_bits) - 1
            };
            let first = raw & !mask;
            out.push(Network {
                v4,
                first,
                last: first | mask,
                prefix,
                record,
                text: net,
            });
        }
    }
    out
}

/// 从源记录推导期望值。
fn expected(record: &Value, level: GeoLevel) -> GeoInfo {
    let Some(cc) = record["country"]["iso_code"].as_str() else {
        return GeoInfo::default();
    };
    assert!(
        cc.len() == 2 && cc.bytes().all(|b| b.is_ascii_uppercase()) && cc != "ZZ" && cc != "XX",
        "测试数据里出现了需要特殊处理的国家码 {cc}"
    );
    let mut info = GeoInfo {
        country_code: [cc.as_bytes()[0], cc.as_bytes()[1]],
        ..GeoInfo::default()
    };
    if level >= GeoLevel::Region
        && let Some(sub) = record["subdivisions"][0]["iso_code"].as_str()
    {
        info.region_code = format!("{cc}-{sub}");
    }
    if level >= GeoLevel::City {
        info.city_geoname_id = record["city"]["geoname_id"]
            .as_u64()
            .map_or(0, |v| u32::try_from(v).unwrap());
    }
    info
}

struct Tally {
    addresses: usize,
    with_country: usize,
    with_region: usize,
    with_city: usize,
    without_country: usize,
}

/// 对 `file` 的每个网段做逐条比对，返回统计。
fn check_all(db_name: &str, source: &str) -> (usize, Tally) {
    let nets = load_networks(source);
    let db = GeoDb::new();
    assert!(
        db.reload_if_changed(&test_mmdb(db_name)).unwrap(),
        "{db_name} 应当加载成功"
    );
    let mut t = Tally {
        addresses: 0,
        with_country: 0,
        with_region: 0,
        with_city: 0,
        without_country: 0,
    };
    for net in &nets {
        let mid = net.first + (net.last - net.first) / 2;
        for x in [net.first, mid, net.last] {
            let ip = from_u128(net.v4, x);
            // 重叠网段取最长前缀（源数据里 214.78.0.0/19 与 214.78.120.0/22 之类互相包含）
            let best = nets
                .iter()
                .filter(|n| n.contains(ip))
                .max_by_key(|n| n.prefix)
                .unwrap();
            for level in [GeoLevel::Country, GeoLevel::Region, GeoLevel::City] {
                let want = expected(&best.record, level);
                let got = db.lookup(ip, level);
                assert_eq!(
                    got, want,
                    "{db_name} {ip} 在 {}（最长匹配 {}）level={level:?}",
                    net.text, best.text
                );
                // IPv4 地址写成 IPv4-mapped IPv6 结果相同
                if let IpAddr::V4(a) = ip {
                    assert_eq!(
                        db.lookup(IpAddr::V6(a.to_ipv6_mapped()), level),
                        want,
                        "{ip} mapped"
                    );
                }
            }
            let full = expected(&best.record, GeoLevel::City);
            t.addresses += 1;
            if full.country_code == [0, 0] {
                t.without_country += 1;
            } else {
                t.with_country += 1;
            }
            t.with_region += usize::from(!full.region_code.is_empty());
            t.with_city += usize::from(full.city_geoname_id != 0);
        }
    }
    (nets.len(), t)
}

#[test]
fn geoip2_city_test_matches_source_data() {
    let (networks, t) = check_all("GeoIP2-City-Test", "GeoIP2-City-Test");
    eprintln!(
        "GeoIP2-City-Test: {networks} 个网段，{} 个地址，国家 {} 区域 {} 城市 {} 无国家 {}",
        t.addresses, t.with_country, t.with_region, t.with_city, t.without_country
    );
    assert_eq!(networks, 250);
    assert_eq!(t.addresses, 750);
    // 不能是空转：数据里确实有区域、城市、无国家的记录
    assert!(t.with_region > 0 && t.with_city > 0 && t.without_country > 0);
}

#[test]
fn geolite2_city_test_matches_source_data() {
    let (networks, t) = check_all("GeoLite2-City-Test", "GeoLite2-City-Test");
    eprintln!(
        "GeoLite2-City-Test: {networks} 个网段，{} 个地址，国家 {} 区域 {} 城市 {}",
        t.addresses, t.with_country, t.with_region, t.with_city
    );
    assert_eq!(networks, 242);
    assert!(t.with_region > 0 && t.with_city > 0);
}

#[test]
fn geolite2_country_test_matches_source_data() {
    let (networks, t) = check_all("GeoLite2-Country-Test", "GeoLite2-Country-Test");
    eprintln!(
        "GeoLite2-Country-Test: {networks} 个网段，{} 个地址，国家 {}",
        t.addresses, t.with_country
    );
    assert_eq!(networks, 244);
    // Country 库没有区域和城市：高精度查询也必须是空的
    assert_eq!(t.with_region, 0);
    assert_eq!(t.with_city, 0);
}

#[test]
fn geoip2_country_test_matches_source_data() {
    let (networks, t) = check_all("GeoIP2-Country-Test", "GeoIP2-Country-Test");
    eprintln!(
        "GeoIP2-Country-Test: {networks} 个网段，{} 个地址，国家 {} 无国家 {}",
        t.addresses, t.with_country, t.without_country
    );
    assert_eq!(networks, 355);
    assert!(
        t.without_country > 0,
        "数据里有只有 registered_country 的网段（5.83.124.0/22）"
    );
}

/// 几个手工挑的地址：把「源数据里的值」写死一遍，防止比对逻辑本身被改坏而不自知。
#[test]
fn spot_checks_with_hand_written_expectations() {
    let db = GeoDb::new();
    db.reload_if_changed(&test_mmdb("GeoIP2-City-Test"))
        .unwrap();
    let ip = |s: &str| s.parse::<IpAddr>().unwrap();

    // 瑞典 Linköping：区域 E（Östergötland），GeoNames 2694762
    let info = db.lookup(ip("89.160.20.128"), GeoLevel::City);
    assert_eq!(info.country_code, *b"SE");
    assert_eq!(info.region_code, "SE-E");
    assert_eq!(info.city_geoname_id, 2_694_762);
    // 按站点设置截断
    let info = db.lookup(ip("89.160.20.128"), GeoLevel::Region);
    assert_eq!(
        (info.region_code.as_str(), info.city_geoname_id),
        ("SE-E", 0)
    );
    let info = db.lookup(ip("89.160.20.128"), GeoLevel::Country);
    assert_eq!(
        (
            info.country_code,
            info.region_code.as_str(),
            info.city_geoname_id
        ),
        (*b"SE", "", 0)
    );

    // 英国伦敦：subdivisions 有两级（ENG、WBK），只取第一级
    let info = db.lookup(ip("81.2.69.142"), GeoLevel::City);
    assert_eq!(
        (
            info.country_code,
            info.region_code.as_str(),
            info.city_geoname_id
        ),
        (*b"GB", "GB-ENG", 2_643_743)
    );
    // 中国吉林：区域 22
    let info = db.lookup(ip("175.16.199.1"), GeoLevel::City);
    assert_eq!(
        (
            info.country_code,
            info.region_code.as_str(),
            info.city_geoname_id
        ),
        (*b"CN", "CN-22", 2_038_180)
    );
    // IPv6
    let info = db.lookup(ip("2001:218::1"), GeoLevel::City);
    assert_eq!(
        (
            info.country_code,
            info.region_code.as_str(),
            info.city_geoname_id
        ),
        (*b"JP", "", 0)
    );
    // 只有国家和城市、没有区域（新加坡）
    let info = db.lookup(ip("214.0.0.1"), GeoLevel::City);
    assert_eq!(
        (
            info.country_code,
            info.region_code.as_str(),
            info.city_geoname_id
        ),
        (*b"SG", "", 1_880_252)
    );
    // 国家取 country，不取 registered_country（GB 的 2.125.160.216/29 注册在 FR）
    let info = db.lookup(ip("2.125.160.217"), GeoLevel::Country);
    assert_eq!(info.country_code, *b"GB");
    // 库里没有的地址
    assert!(db.lookup(ip("1.1.1.1"), GeoLevel::City).is_empty());
    assert!(db.lookup(ip("2606:4700::1111"), GeoLevel::City).is_empty());
}

#[test]
fn ipv4_mapped_ipv6_is_treated_as_ipv4() {
    let db = GeoDb::new();
    db.reload_if_changed(&test_mmdb("GeoIP2-City-Test"))
        .unwrap();
    let v4: IpAddr = "89.160.20.128".parse().unwrap();
    let mapped: IpAddr = "::ffff:89.160.20.128".parse().unwrap();
    let want = db.lookup(v4, GeoLevel::City);
    assert_eq!(want.country_code, *b"SE");
    assert_eq!(db.lookup(mapped, GeoLevel::City), want);
    // 映射的私有地址同样返回空值
    assert!(
        db.lookup("::ffff:10.0.0.1".parse().unwrap(), GeoLevel::City)
            .is_empty()
    );
}

#[test]
fn private_and_reserved_addresses_return_empty() {
    let db = GeoDb::new();
    db.reload_if_changed(&test_mmdb("GeoIP2-City-Test"))
        .unwrap();
    for s in [
        "10.1.2.3",
        "127.0.0.1",
        "172.16.5.5",
        "192.168.0.10",
        "169.254.9.9",
        "100.64.1.1",
        "0.0.0.0",
        "255.255.255.255",
        "224.0.0.251",
        "192.0.2.7",
        "198.51.100.7",
        "203.0.113.7",
        "::1",
        "::",
        "fe80::1",
        "fd00::1",
        "ff02::1",
        "2001:db8::1",
        "::ffff:192.168.1.1",
    ] {
        let info = db.lookup(s.parse().unwrap(), GeoLevel::City);
        assert!(info.is_empty(), "{s} 应当返回空值，实际 {info:?}");
    }
}

#[test]
fn unloaded_db_returns_empty_and_has_no_attribution() {
    let db = GeoDb::new();
    assert!(!db.is_loaded());
    assert!(
        db.lookup("89.160.20.128".parse().unwrap(), GeoLevel::City)
            .is_empty()
    );
    assert_eq!(db.attribution(), None);
}
