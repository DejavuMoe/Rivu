//! 访客身份（`system-design.md` §4.5）：每日盐哈希的 `visitor_id`、不随盐轮换的 routing 哈希、站点本地日期。
//!
//! 原始 IP 和 UA 只在这里被哈希，不会被保存。

use std::hash::Hasher;
use std::net::IpAddr;

use jiff::Timestamp;
use jiff::civil::Date;
use jiff::tz::TimeZone;
use siphasher::sip::SipHasher24;

/// 16 字节的盐，同时也是 SipHash-2-4 的 key。
pub type Salt = [u8; 16];

/// 最晚、最早可以表示的毫秒时间戳（约公元 ±9999 年），超出的值被钳到边界，避免 jiff 报错。
const TS_LIMIT_MS: i64 = 253_402_300_799_000;

/// `SipHash-2-4(salt, site_id ‖ len‖ip ‖ len‖ua)`，输入用长度前缀拼接，没有边界歧义：
///
/// - `site_id`：`u64` 小端；
/// - `ip`：1 字节长度（4 或 16）‖ 地址字节。IPv4-mapped IPv6（`::ffff:a.b.c.d`）先规范化为 IPv4；
/// - `ua`：`u32` 小端长度 ‖ UTF-8 字节。
fn keyed_hash(key: &Salt, site_id: u64, ip: IpAddr, ua: &str) -> u64 {
    let mut h = SipHasher24::new_with_key(key);
    h.write(&site_id.to_le_bytes());
    let v4 = match ip {
        IpAddr::V4(a) => Some(a),
        IpAddr::V6(a) => a.to_ipv4_mapped(),
    };
    match (v4, ip) {
        (Some(a), _) => {
            h.write(&[4]);
            h.write(&a.octets());
        }
        (None, ip) => {
            // 只有非 mapped 的 IPv6 走到这里；IPv4 恒有 `v4`。
            let octets = match ip {
                IpAddr::V6(a) => a.octets(),
                IpAddr::V4(a) => a.to_ipv6_mapped().octets(),
            };
            h.write(&[16]);
            h.write(&octets);
        }
    }
    let ua = ua.as_bytes();
    h.write(&u32::try_from(ua.len()).unwrap_or(u32::MAX).to_le_bytes());
    h.write(ua);
    h.finish()
}

/// 访客 ID：`SipHash-2-4(salt, site_id ‖ len‖ip ‖ len‖ua)`。`salt` 按事件的站点本地日期选取。
pub fn visitor_hash(salt: &Salt, site_id: u64, ip: IpAddr, ua: &str) -> u64 {
    keyed_hash(salt, site_id, ip, ua)
}

/// routing 哈希：输入同 [`visitor_hash`]，key 换成长期不变的 `routing_secret`。
/// 不随盐轮换，所以同一访客跨午夜的所有 key 总在同一个分片（Q15：`routing % 32`）。
pub fn routing_hash(secret: &Salt, site_id: u64, ip: IpAddr, ua: &str) -> u64 {
    keyed_hash(secret, site_id, ip, ua)
}

/// 毫秒时间戳在站点时区下的本地日期（jiff 处理 DST 与历史时区规则）。
pub fn local_date(ts_ms: i64, tz: &TimeZone) -> Date {
    let ts = Timestamp::from_millisecond(ts_ms.clamp(-TS_LIMIT_MS, TS_LIMIT_MS))
        .unwrap_or(Timestamp::UNIX_EPOCH);
    tz.to_datetime(ts).date()
}

#[cfg(test)]
mod tests {
    use std::net::{Ipv4Addr, Ipv6Addr};

    use jiff::civil::date;

    use super::*;

    const SALT: Salt = [1; 16];

    fn v4(a: u8, b: u8, c: u8, d: u8) -> IpAddr {
        IpAddr::V4(Ipv4Addr::new(a, b, c, d))
    }

    #[test]
    fn matches_manual_byte_layout() {
        // 手工拼出字节串再整体哈希，核对流式写入与文档的布局一致。
        let mut buf = Vec::new();
        buf.extend_from_slice(&7u64.to_le_bytes());
        buf.push(4);
        buf.extend_from_slice(&[203, 0, 113, 9]);
        buf.extend_from_slice(&5u32.to_le_bytes());
        buf.extend_from_slice(b"Agent");
        let mut h = SipHasher24::new_with_key(&SALT);
        h.write(&buf);
        assert_eq!(
            visitor_hash(&SALT, 7, v4(203, 0, 113, 9), "Agent"),
            h.finish()
        );

        let v6: Ipv6Addr = "2001:db8::1".parse().unwrap();
        let mut buf = Vec::new();
        buf.extend_from_slice(&7u64.to_le_bytes());
        buf.push(16);
        buf.extend_from_slice(&v6.octets());
        buf.extend_from_slice(&0u32.to_le_bytes());
        let mut h = SipHasher24::new_with_key(&SALT);
        h.write(&buf);
        assert_eq!(visitor_hash(&SALT, 7, IpAddr::V6(v6), ""), h.finish());
    }

    #[test]
    fn ipv4_mapped_ipv6_is_normalized() {
        let mapped: IpAddr = "::ffff:203.0.113.9".parse().unwrap();
        assert_eq!(
            visitor_hash(&SALT, 1, mapped, "ua"),
            visitor_hash(&SALT, 1, v4(203, 0, 113, 9), "ua")
        );
        // 不是 mapped 的 IPv6 不被折叠。
        let other: IpAddr = "::203.0.113.9".parse().unwrap();
        assert_ne!(
            visitor_hash(&SALT, 1, other, "ua"),
            visitor_hash(&SALT, 1, v4(203, 0, 113, 9), "ua")
        );
    }

    #[test]
    fn every_input_matters() {
        let base = visitor_hash(&SALT, 1, v4(1, 2, 3, 4), "ua");
        assert_ne!(base, visitor_hash(&[2; 16], 1, v4(1, 2, 3, 4), "ua"));
        assert_ne!(base, visitor_hash(&SALT, 2, v4(1, 2, 3, 4), "ua"));
        assert_ne!(base, visitor_hash(&SALT, 1, v4(1, 2, 3, 5), "ua"));
        assert_ne!(base, visitor_hash(&SALT, 1, v4(1, 2, 3, 4), "ub"));
        // routing 用另一把 key，同样的输入得到不同的值。
        assert_ne!(base, routing_hash(&[9; 16], 1, v4(1, 2, 3, 4), "ua"));
        assert_eq!(
            routing_hash(&[9; 16], 1, v4(1, 2, 3, 4), "ua"),
            routing_hash(&[9; 16], 1, v4(1, 2, 3, 4), "ua")
        );
    }

    #[test]
    fn local_date_follows_site_timezone() {
        let shanghai = TimeZone::get("Asia/Shanghai").unwrap();
        // 2026-05-01T16:30:00Z = 上海 05-02 00:30。
        let ts = date(2026, 5, 1)
            .at(16, 30, 0, 0)
            .to_zoned(TimeZone::UTC)
            .unwrap();
        let ms = ts.timestamp().as_millisecond();
        assert_eq!(local_date(ms, &TimeZone::UTC), date(2026, 5, 1));
        assert_eq!(local_date(ms, &shanghai), date(2026, 5, 2));
        // 尼泊尔 +5:45：18:14Z 仍是 05-01，18:15Z 进入 05-02。
        let kathmandu = TimeZone::get("Asia/Kathmandu").unwrap();
        let at = |h, m| {
            date(2026, 5, 1)
                .at(h, m, 0, 0)
                .to_zoned(TimeZone::UTC)
                .unwrap()
                .timestamp()
                .as_millisecond()
        };
        assert_eq!(local_date(at(18, 14), &kathmandu), date(2026, 5, 1));
        assert_eq!(local_date(at(18, 15), &kathmandu), date(2026, 5, 2));
    }

    #[test]
    fn local_date_handles_dst_and_extreme_inputs() {
        let berlin = TimeZone::get("Europe/Berlin").unwrap();
        // 2026-03-29 02:00 CET 跳到 03:00 CEST：UTC 01:00 起是 CEST。
        let utc = |d: Date, h, m| {
            d.at(h, m, 0, 0)
                .to_zoned(TimeZone::UTC)
                .unwrap()
                .timestamp()
                .as_millisecond()
        };
        let d = date(2026, 3, 29);
        assert_eq!(local_date(utc(d, 0, 59), &berlin), d);
        assert_eq!(local_date(utc(d, 21, 59), &berlin), d);
        assert_eq!(local_date(utc(d, 22, 0), &berlin), date(2026, 3, 30));
        // 不 panic。
        let _ = local_date(i64::MIN, &berlin);
        let _ = local_date(i64::MAX, &berlin);
    }
}
