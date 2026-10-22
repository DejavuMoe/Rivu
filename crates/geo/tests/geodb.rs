//! `GeoDb` 的热加载、校验与署名（署名跟随实际加载的库）。

mod common;

use common::{TempDir, build_epoch, test_mmdb};
use rivu_geo::{GeoDb, GeoLevel, Meta, StoredAttribution, write_meta};

fn city_lookup(db: &GeoDb, ip: &str) -> [u8; 2] {
    db.lookup(ip.parse().unwrap(), GeoLevel::Country)
        .country_code
}

fn meta_for(db_file: &std::path::Path, source: &str, attribution: Option<(&str, &str)>) -> Meta {
    Meta {
        source: source.into(),
        source_id: format!("{source}:test"),
        etag: None,
        last_modified: None,
        sha256: "0".repeat(64),
        downloaded_at: "2026-10-22T00:00:00Z".into(),
        checked_at: "2026-10-22T00:00:00Z".into(),
        build_epoch: build_epoch(db_file),
        database_type: "test".into(),
        attribution: attribution.map(|(text, url)| StoredAttribution {
            text: text.into(),
            url: url.into(),
            license: None,
        }),
    }
}

#[test]
fn reload_detects_changes_and_keeps_old_db_when_new_file_is_bad() {
    let dir = TempDir::new("reload");
    let path = dir.join("geoip.mmdb");
    let db = GeoDb::new();

    // 文件还不存在：不是错误，也没有加载
    assert!(!db.reload_if_changed(&path).unwrap());
    assert!(!db.is_loaded());

    // 首次加载
    std::fs::write(
        &path,
        std::fs::read(test_mmdb("GeoIP2-Country-Test")).unwrap(),
    )
    .unwrap();
    assert!(db.reload_if_changed(&path).unwrap());
    assert!(db.is_loaded());
    assert_eq!(city_lookup(&db, "89.160.20.128"), *b"SE");
    let status = db.status();
    assert_eq!(status.database_type.as_deref(), Some("GeoIP2-Country"));
    // 没有变化：不重载
    assert!(!db.reload_if_changed(&path).unwrap());

    // 换成另一个库（内容和长度都不同）：热加载
    std::fs::write(
        &path,
        std::fs::read(test_mmdb("GeoLite2-City-Test")).unwrap(),
    )
    .unwrap();
    assert!(db.reload_if_changed(&path).unwrap());
    assert_eq!(db.status().database_type.as_deref(), Some("GeoLite2-City"));
    assert_eq!(city_lookup(&db, "175.16.199.1"), *b"CN");

    // 坏文件：返回错误，旧库继续服务
    for bad in [
        &b"definitely not an mmdb file"[..],
        &std::fs::read(test_mmdb("GeoLite2-ASN-Test")).unwrap()[..], // database_type 不对
        &std::fs::read(test_mmdb("GeoIP2-City-Test-Invalid-Node-Count")).unwrap()[..],
        // 注意：Broken-Double-Format 只是数据区里某个 double 的编码有问题，§4.4 的校验（元数据、
        // database_type、node_count、一次探测查询）不会发现它；整库 `verify()` 太贵，不做。
        &b""[..],
    ] {
        std::fs::write(&path, bad).unwrap();
        let err = db.reload_if_changed(&path).unwrap_err();
        eprintln!("坏文件（{} 字节）的错误：{err:#}", bad.len());
        assert!(db.is_loaded());
        assert_eq!(
            db.status().database_type.as_deref(),
            Some("GeoLite2-City"),
            "旧库必须保留"
        );
        assert_eq!(city_lookup(&db, "175.16.199.1"), *b"CN");
    }

    // 文件被删除：保留已加载的库
    std::fs::remove_file(&path).unwrap();
    assert!(!db.reload_if_changed(&path).unwrap());
    assert!(db.is_loaded());
}

#[test]
fn asn_database_is_rejected_by_database_type() {
    let dir = TempDir::new("asn");
    let path = dir.join("geoip.mmdb");
    std::fs::copy(test_mmdb("GeoLite2-ASN-Test"), &path).unwrap();
    let db = GeoDb::new();
    let err = db.reload_if_changed(&path).unwrap_err();
    assert!(format!("{err:#}").contains("database_type"), "{err:#}");
    assert!(!db.is_loaded());
}

#[test]
fn attribution_for_manually_placed_file_comes_from_configuration() {
    let dir = TempDir::new("attr-manual");
    let path = dir.join("geoip.mmdb");
    std::fs::copy(test_mmdb("GeoLite2-Country-Test"), &path).unwrap();

    // 没有署名变量：没有署名
    let db = GeoDb::new();
    db.reload_if_changed(&path).unwrap();
    assert_eq!(db.attribution(), None);
    assert_eq!(db.status().source, Some("custom"));

    // 配置了署名
    let db = GeoDb::with_manual_attribution(Some("Geo by Me"), Some("https://me.test"));
    db.reload_if_changed(&path).unwrap();
    let a = db.attribution().unwrap();
    assert_eq!(
        (a.source, a.text.as_str(), a.url.as_str()),
        ("custom", "Geo by Me", "https://me.test")
    );
    assert_eq!(a.license, None);
}

#[test]
fn attribution_follows_the_loaded_database_not_the_configuration() {
    let dir = TempDir::new("attr-meta");
    let path = dir.join("geoip.mmdb");
    std::fs::copy(test_mmdb("GeoLite2-Country-Test"), &path).unwrap();
    // 配置里的署名属于「自定义」，但库是 DB-IP 下载的：显示 DB-IP 的署名
    let db = GeoDb::with_manual_attribution(Some("Geo by Me"), Some("https://me.test"));
    write_meta(&path, &meta_for(&path, "dbip-lite", None)).unwrap();
    db.reload_if_changed(&path).unwrap();
    let a = db.attribution().unwrap();
    assert_eq!(a.source, "dbip-lite");
    assert_eq!(a.text, "IP Geolocation by DB-IP");
    assert_eq!(a.url, "https://db-ip.com");
    assert_eq!(a.license.as_deref(), Some("CC BY 4.0"));
    assert_eq!(db.status().source, Some("dbip-lite"));

    // MaxMind
    write_meta(&path, &meta_for(&path, "maxmind-geolite2", None)).unwrap();
    // meta 变了但库没变：不重载库，署名照样跟上
    assert!(!db.reload_if_changed(&path).unwrap());
    let a = db.attribution().unwrap();
    assert_eq!(a.source, "maxmind-geolite2");
    assert!(
        a.text
            .starts_with("This product includes GeoLite Data created by MaxMind")
    );

    // 自定义来源：署名取自 meta 里存的值
    write_meta(
        &path,
        &meta_for(&path, "custom", Some(("From meta", "https://meta.test"))),
    )
    .unwrap();
    db.reload_if_changed(&path).unwrap();
    let a = db.attribution().unwrap();
    assert_eq!(
        (a.source, a.text.as_str(), a.url.as_str()),
        ("custom", "From meta", "https://meta.test")
    );

    // meta 的 build_epoch 与库不一致（文件被手工换过）：meta 作废，退回手工署名
    let mut stale = meta_for(&path, "dbip-lite", None);
    stale.build_epoch += 1;
    write_meta(&path, &stale).unwrap();
    db.reload_if_changed(&path).unwrap();
    let a = db.attribution().unwrap();
    assert_eq!((a.source, a.text.as_str()), ("custom", "Geo by Me"));
}
