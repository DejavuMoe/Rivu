//! 刷新流程：用本地 HTTP 测试服务器覆盖 system-design §4.4「生命周期」「HTTP 约束」的每一条。

mod common;
mod support;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use common::{TempDir, test_mmdb};
use flate2::Compression;
use flate2::write::GzEncoder;
use rivu_common::config::{DbIpEdition, GeoConfig, GeoSource, Secret};
use rivu_geo::{
    GeoDb, GeoLevel, HttpClient, HttpOptions, LocalCopy, RefreshOptions, RefreshOutcome,
    local_copy, read_meta, refresh, refresh_with,
};
use sha2::{Digest, Sha256};
use std::io::Write;
use support::{Reply, Req, start};

const LAST_MODIFIED_1: &str = "Thu, 01 Oct 2026 06:29:26 GMT";
const LAST_MODIFIED_2: &str = "Tue, 01 Sep 2026 06:27:01 GMT";

/// 国家库：没有区域与城市。
fn country_db() -> Vec<u8> {
    std::fs::read(test_mmdb("GeoLite2-Country-Test")).unwrap()
}

/// 城市库：89.160.20.128 有区域 SE-E。
fn city_db() -> Vec<u8> {
    std::fs::read(test_mmdb("GeoIP2-City-Test")).unwrap()
}

fn gz(data: &[u8]) -> Vec<u8> {
    let mut e = GzEncoder::new(Vec::new(), Compression::default());
    e.write_all(data).unwrap();
    e.finish().unwrap()
}

fn tar_gz(path: &str, data: &[u8]) -> Vec<u8> {
    let mut tarball = Vec::new();
    {
        let mut b = tar::Builder::new(&mut tarball);
        let mut h = tar::Header::new_gnu();
        h.set_size(3);
        h.set_mode(0o644);
        h.set_cksum();
        b.append_data(&mut h, "GeoLite2-City_20261020/COPYRIGHT.txt", &b"abc"[..])
            .unwrap();
        let mut h = tar::Header::new_gnu();
        h.set_size(data.len() as u64);
        h.set_mode(0o644);
        h.set_cksum();
        b.append_data(&mut h, path, data).unwrap();
        b.finish().unwrap();
    }
    gz(&tarball)
}

fn sha(data: &[u8]) -> String {
    Sha256::digest(data)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn client() -> HttpClient {
    HttpClient::with_options(HttpOptions {
        no_proxy: true,
        ..HttpOptions::default()
    })
    .unwrap()
}

fn custom_cfg(dir: &Path, url: String) -> GeoConfig {
    let mut cfg = GeoConfig::new(dir);
    cfg.source = GeoSource::Custom {
        url: Secret::new(url),
    };
    cfg
}

fn dbip_opts(server: &support::Server) -> RefreshOptions {
    RefreshOptions {
        dbip_base_url: server.url("/free"),
        now: Some("2026-10-22T08:00:00Z".parse().unwrap()),
        ..RefreshOptions::default()
    }
}

/// 库目录里的文件名（已排序）。
fn names(cfg: &GeoConfig) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(cfg.db_path.parent().unwrap())
        .map(|rd| {
            rd.flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

/// 刷新结束后目录里只应有库和 meta，不能有临时文件。
fn assert_only_db_and_meta(cfg: &GeoConfig) {
    assert_eq!(names(cfg), ["geoip.mmdb", "geoip.mmdb.meta.json"]);
}

fn snapshot(cfg: &GeoConfig) -> (Vec<u8>, Vec<u8>) {
    (
        std::fs::read(&cfg.db_path).unwrap(),
        std::fs::read(rivu_geo::meta_path(&cfg.db_path)).unwrap(),
    )
}

fn ts(s: &str) -> jiff::Timestamp {
    s.parse().unwrap()
}

/// 可变的「源站」：带 ETag / Last-Modified，并按条件请求头回 304。
struct Origin {
    body: Vec<u8>,
    etag: Option<String>,
    last_modified: Option<String>,
}

fn serve_conditional(o: &Origin, req: &Req) -> Reply {
    let etag_hit = matches!((&o.etag, req.header("if-none-match")), (Some(a), Some(b)) if a == b);
    let lm_hit =
        matches!((&o.last_modified, req.header("if-modified-since")), (Some(a), Some(b)) if a == b);
    let mut reply = if etag_hit || lm_hit {
        Reply::status(304)
    } else {
        Reply::ok(o.body.clone())
    };
    if let Some(e) = &o.etag {
        reply = reply.with_header("ETag", e);
    }
    if let Some(l) = &o.last_modified {
        reply = reply.with_header("Last-Modified", l);
    }
    reply
}

// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn first_download_then_304_then_update_with_hot_reload() {
    let dir = TempDir::new("flow");
    let origin = Arc::new(Mutex::new(Origin {
        body: country_db(),
        etag: Some("\"v1\"".into()),
        last_modified: Some(LAST_MODIFIED_1.into()),
    }));
    let o = Arc::clone(&origin);
    let server = start(move |req| serve_conditional(&o.lock().unwrap(), req)).await;
    let cfg = custom_cfg(dir.path(), server.url("/geo.mmdb"));
    let http = client();
    assert_eq!(local_copy(&cfg), LocalCopy::Missing);

    // 1. 首次下载：目录不存在也能建；没有本地副本，所以不带校验头
    let out = refresh(&cfg, &http).await.unwrap();
    let expected_sha = sha(&country_db());
    assert_eq!(
        out,
        RefreshOutcome::Updated {
            sha256: expected_sha.clone(),
            build_epoch: common::build_epoch(&test_mmdb("GeoLite2-Country-Test")),
            bytes: country_db().len() as u64
        }
    );
    assert_eq!(std::fs::read(&cfg.db_path).unwrap(), country_db());
    assert_only_db_and_meta(&cfg);
    let reqs = server.requests();
    assert_eq!(reqs.len(), 1);
    assert_eq!(reqs[0].method, "GET");
    assert_eq!(reqs[0].header("if-none-match"), None);
    assert_eq!(reqs[0].header("if-modified-since"), None);
    assert!(
        reqs[0]
            .header("user-agent")
            .unwrap()
            .starts_with("rivu-geo/")
    );

    // meta：来源、校验头、哈希、署名
    let m1 = read_meta(&cfg.db_path).unwrap();
    assert_eq!(m1.source, "custom");
    assert_eq!(m1.source_id, format!("custom:{}", server.url("/geo.mmdb")));
    assert_eq!(m1.etag.as_deref(), Some("\"v1\""));
    assert_eq!(m1.last_modified.as_deref(), Some(LAST_MODIFIED_1));
    assert_eq!(m1.sha256, expected_sha);
    assert_eq!(m1.database_type, "GeoLite2-Country");
    assert_eq!(m1.attribution, None, "自定义来源没有署名变量时没有署名");
    assert_eq!(local_copy(&cfg), LocalCopy::Current);

    // 原子替换后可以热加载
    let db = GeoDb::new();
    assert!(db.reload_if_changed(&cfg.db_path).unwrap());
    let ip = "89.160.20.128".parse().unwrap();
    assert_eq!(db.lookup(ip, GeoLevel::Region).country_code, *b"SE");
    assert_eq!(
        db.lookup(ip, GeoLevel::Region).region_code,
        "",
        "国家库没有区域"
    );

    // 2. 再刷新：带上 meta 里的 ETag 与 Last-Modified，服务器回 304，只更新检查时间
    let out = refresh(&cfg, &http).await.unwrap();
    assert_eq!(out, RefreshOutcome::NotModified { by_hash: false });
    let reqs = server.requests();
    assert_eq!(reqs[1].header("if-none-match"), Some("\"v1\""));
    assert_eq!(reqs[1].header("if-modified-since"), Some(LAST_MODIFIED_1));
    let m2 = read_meta(&cfg.db_path).unwrap();
    assert_eq!(m2.downloaded_at, m1.downloaded_at);
    assert!(
        ts(&m2.checked_at) > ts(&m1.checked_at),
        "304 要更新检查时间"
    );
    assert_eq!(m2.sha256, m1.sha256);
    assert_eq!(std::fs::read(&cfg.db_path).unwrap(), country_db());
    assert_only_db_and_meta(&cfg);
    assert!(
        !db.reload_if_changed(&cfg.db_path).unwrap(),
        "文件没变，不重载"
    );

    // 3. 源站换了新库：带旧校验头 -> 200 -> 替换，meta 回传新的校验头，热加载后查询结果变了
    *origin.lock().unwrap() = Origin {
        body: city_db(),
        etag: Some("\"v2\"".into()),
        last_modified: Some(LAST_MODIFIED_2.into()),
    };
    let out = refresh(&cfg, &http).await.unwrap();
    assert!(matches!(out, RefreshOutcome::Updated { .. }), "{out:?}");
    let reqs = server.requests();
    assert_eq!(reqs[2].header("if-none-match"), Some("\"v1\""));
    assert_eq!(std::fs::read(&cfg.db_path).unwrap(), city_db());
    let m3 = read_meta(&cfg.db_path).unwrap();
    assert_eq!(m3.etag.as_deref(), Some("\"v2\""));
    assert_eq!(m3.last_modified.as_deref(), Some(LAST_MODIFIED_2));
    assert_eq!(m3.sha256, sha(&city_db()));
    assert_eq!(m3.downloaded_at, m3.checked_at);
    assert_only_db_and_meta(&cfg);
    assert!(db.reload_if_changed(&cfg.db_path).unwrap());
    let info = db.lookup(ip, GeoLevel::Region);
    assert_eq!(
        (info.country_code, info.region_code.as_str()),
        (*b"SE", "SE-E")
    );
}

#[tokio::test]
async fn gz_and_plain_bodies_are_both_accepted() {
    let http = client();
    for (name, body) in [("plain", country_db()), ("gz", gz(&country_db()))] {
        let dir = TempDir::new(name);
        let served = body.clone();
        let server = start(move |_| Reply::ok(served.clone())).await;
        let cfg = custom_cfg(dir.path(), server.url("/geo.mmdb.gz"));
        let out = refresh(&cfg, &http).await.unwrap();
        assert!(
            matches!(out, RefreshOutcome::Updated { .. }),
            "{name}: {out:?}"
        );
        // 落盘的永远是解压后的 mmdb，SHA-256 记的也是解压后内容的
        assert_eq!(std::fs::read(&cfg.db_path).unwrap(), country_db(), "{name}");
        assert_eq!(
            read_meta(&cfg.db_path).unwrap().sha256,
            sha(&country_db()),
            "{name}"
        );
        assert_only_db_and_meta(&cfg);
    }
}

#[tokio::test]
async fn without_validators_the_whole_file_is_compared_by_sha256() {
    let dir = TempDir::new("nohdr");
    let body = Arc::new(Mutex::new(gz(&country_db())));
    let b = Arc::clone(&body);
    // 既没有 ETag 也没有 Last-Modified，也不会回 304
    let server = start(move |_| Reply::ok(b.lock().unwrap().clone())).await;
    let cfg = custom_cfg(dir.path(), server.url("/geo.mmdb.gz"));
    let http = client();

    assert!(matches!(
        refresh(&cfg, &http).await.unwrap(),
        RefreshOutcome::Updated { .. }
    ));
    let m1 = read_meta(&cfg.db_path).unwrap();
    assert_eq!((m1.etag, m1.last_modified), (None, None));
    let modified_before = std::fs::metadata(&cfg.db_path).unwrap().modified().unwrap();

    // 第二次：整文件 GET（不带任何条件头），内容相同 -> 不替换
    let out = refresh(&cfg, &http).await.unwrap();
    assert_eq!(out, RefreshOutcome::NotModified { by_hash: true });
    let reqs = server.requests();
    assert_eq!(reqs[1].header("if-none-match"), None);
    assert_eq!(reqs[1].header("if-modified-since"), None);
    assert_eq!(
        std::fs::metadata(&cfg.db_path).unwrap().modified().unwrap(),
        modified_before,
        "内容相同不能替换文件"
    );
    let m2 = read_meta(&cfg.db_path).unwrap();
    assert_eq!(m2.downloaded_at, m1.downloaded_at);
    assert!(ts(&m2.checked_at) > ts(&m1.checked_at));
    assert_only_db_and_meta(&cfg);

    // 压缩方式变了但解压后内容相同：仍然相同（哈希比的是 mmdb 内容，不是传输内容）
    *body.lock().unwrap() = country_db();
    assert_eq!(
        refresh(&cfg, &http).await.unwrap(),
        RefreshOutcome::NotModified { by_hash: true }
    );

    // 内容变了 -> 替换
    *body.lock().unwrap() = city_db();
    assert!(matches!(
        refresh(&cfg, &http).await.unwrap(),
        RefreshOutcome::Updated { .. }
    ));
    assert_eq!(std::fs::read(&cfg.db_path).unwrap(), city_db());
    assert_only_db_and_meta(&cfg);
}

#[tokio::test]
async fn failures_keep_the_previous_copy_and_leave_no_temp_files() {
    let dir = TempDir::new("fail");
    let current: Arc<Mutex<FailMode>> = Arc::new(Mutex::new(FailMode::Good));
    let c = Arc::clone(&current);
    let server = start(move |_| match &*c.lock().unwrap() {
        FailMode::Good => Reply::ok(country_db()).with_header("ETag", "\"good\""),
        FailMode::Body(b) => Reply::ok(b.clone()),
        FailMode::Status(s) => Reply::status(*s),
        FailMode::Lying(n) => Reply::LyingLength {
            declared: *n,
            body: b"tiny".to_vec(),
        },
        FailMode::Endless => Reply::ChunkedForever {
            chunk: vec![7u8; 16 * 1024],
            cap: 256 * 1024 * 1024,
        },
    })
    .await;
    let cfg = custom_cfg(dir.path(), server.url("/geo.mmdb"));
    let http = client();
    refresh(&cfg, &http).await.unwrap();
    let good = snapshot(&cfg);
    let small = RefreshOptions {
        max_bytes: 256 * 1024,
        ..RefreshOptions::default()
    };

    let cases: Vec<(&str, FailMode, &str, RefreshOptions)> = vec![
        (
            "HTML 错误页",
            FailMode::Body(b"<html>502 Bad Gateway</html>".to_vec()),
            "校验",
            RefreshOptions::default(),
        ),
        (
            "空响应",
            FailMode::Body(Vec::new()),
            "校验",
            RefreshOptions::default(),
        ),
        (
            "截断的 mmdb（元数据在文件末尾）",
            FailMode::Body(country_db()[..country_db().len() / 2].to_vec()),
            "校验",
            RefreshOptions::default(),
        ),
        (
            "database_type 不对（ASN）",
            FailMode::Body(std::fs::read(test_mmdb("GeoLite2-ASN-Test")).unwrap()),
            "database_type",
            RefreshOptions::default(),
        ),
        (
            "database_type 不对（ASN，gz）",
            FailMode::Body(gz(&std::fs::read(test_mmdb("GeoLite2-ASN-Test")).unwrap())),
            "database_type",
            RefreshOptions::default(),
        ),
        (
            "node_count 无效",
            FailMode::Body(
                std::fs::read(test_mmdb("GeoIP2-City-Test-Invalid-Node-Count")).unwrap(),
            ),
            "校验",
            RefreshOptions::default(),
        ),
        (
            "gz 头正确但内容损坏",
            FailMode::Body({
                let mut g = gz(&country_db());
                let n = g.len();
                g.truncate(n / 2);
                g
            }),
            "解压失败",
            RefreshOptions::default(),
        ),
        (
            "HTTP 500",
            FailMode::Status(500),
            "HTTP 500",
            RefreshOptions::default(),
        ),
        (
            "HTTP 404",
            FailMode::Status(404),
            "HTTP 404",
            RefreshOptions::default(),
        ),
        (
            "HTTP 401",
            FailMode::Status(401),
            "HTTP 401",
            RefreshOptions::default(),
        ),
        // 超过 1 GiB：用伪造的 Content-Length（真实上限），不会真的传输
        (
            "Content-Length 超过 1 GiB",
            FailMode::Lying((1 << 30) + 1),
            "Content-Length",
            RefreshOptions::default(),
        ),
        // 流式超限：不带 Content-Length 的无限 chunked 流（缩小上限，不会真的写 1 GiB）
        ("流式内容超限", FailMode::Endless, "超过上限", small.clone()),
        // gzip 炸弹：压缩后很小，解压后超限
        (
            "gzip 炸弹",
            FailMode::Body(gz(&vec![0u8; 8 * 1024 * 1024])),
            "超过上限",
            RefreshOptions {
                max_bytes: 1024 * 1024,
                ..RefreshOptions::default()
            },
        ),
    ];
    for (name, mode, needle, opts) in cases {
        *current.lock().unwrap() = mode;
        let started = Instant::now();
        let err = refresh_with(&cfg, &http, &opts).await.unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains(needle),
            "{name}：错误里应含 `{needle}`，实际 {msg}"
        );
        assert!(started.elapsed() < Duration::from_secs(20), "{name} 太慢了");
        // 旧库与旧 meta 一字节不差，没有临时文件
        assert_eq!(snapshot(&cfg), good, "{name}：旧副本被改动了");
        assert_only_db_and_meta(&cfg);
        // 旧库仍然可用
        let db = GeoDb::new();
        assert!(db.reload_if_changed(&cfg.db_path).unwrap(), "{name}");
    }
    // 之后恢复正常，照常能刷新（失败不会留下坏状态）；Good 模式的服务器不会回 304，内容相同按哈希判断
    *current.lock().unwrap() = FailMode::Good;
    assert_eq!(
        refresh(&cfg, &http).await.unwrap(),
        RefreshOutcome::NotModified { by_hash: true }
    );
}

enum FailMode {
    Good,
    Body(Vec<u8>),
    Status(u16),
    Lying(u64),
    Endless,
}

#[tokio::test]
async fn at_most_five_redirects_are_followed() {
    let http = client();
    for (hops, ok) in [
        (0usize, true),
        (3, true),
        (5, true),
        (6, false),
        (20, false),
    ] {
        let dir = TempDir::new("redir");
        let server = start(move |req| {
            let n: usize = req.target.trim_start_matches("/hop/").parse().unwrap_or(0);
            if n < hops {
                Reply::redirect(format!("/hop/{}", n + 1))
            } else {
                Reply::ok(country_db())
            }
        })
        .await;
        let cfg = custom_cfg(dir.path(), server.url("/hop/0"));
        let result = refresh(&cfg, &http).await;
        assert_eq!(result.is_ok(), ok, "{hops} 次重定向：{result:?}");
        if ok {
            assert_eq!(server.requests().len(), hops + 1);
            assert_only_db_and_meta(&cfg);
        } else {
            assert!(!cfg.db_path.exists(), "失败时不能留下库文件");
            assert!(
                format!("{:#}", result.unwrap_err()).contains("redirect"),
                "错误应说明重定向次数过多"
            );
        }
    }
}

#[tokio::test]
async fn changing_the_source_means_no_local_copy() {
    let dir = TempDir::new("source");
    // 无论请求什么路径，只要校验头匹配就回 304：如果错误地带了旧来源的校验头，就会被 304 糊弄过去
    let origin = Arc::new(Mutex::new(Origin {
        body: country_db(),
        etag: Some("\"same\"".into()),
        last_modified: None,
    }));
    let o = Arc::clone(&origin);
    let server = start(move |req| serve_conditional(&o.lock().unwrap(), req)).await;
    let http = client();

    let cfg_a = custom_cfg(dir.path(), server.url("/a.mmdb?token=one"));
    refresh(&cfg_a, &http).await.unwrap();
    assert_eq!(local_copy(&cfg_a), LocalCopy::Current);
    // 同一个 URL 只换了令牌：来源标识相同，照常条件 GET
    let cfg_a2 = custom_cfg(dir.path(), server.url("/a.mmdb?token=two"));
    assert_eq!(local_copy(&cfg_a2), LocalCopy::Current);
    assert_eq!(
        refresh(&cfg_a2, &http).await.unwrap(),
        RefreshOutcome::NotModified { by_hash: false }
    );
    assert_eq!(
        server.requests()[1].header("if-none-match"),
        Some("\"same\"")
    );

    // 换成另一个 URL：视为没有本地副本，不带校验头，所以拿到 200
    let cfg_b = custom_cfg(dir.path(), server.url("/b.mmdb"));
    assert_eq!(local_copy(&cfg_b), LocalCopy::Foreign);
    *origin.lock().unwrap() = Origin {
        body: city_db(),
        etag: Some("\"same\"".into()),
        last_modified: None,
    };
    let out = refresh(&cfg_b, &http).await.unwrap();
    assert!(matches!(out, RefreshOutcome::Updated { .. }), "{out:?}");
    assert_eq!(server.requests()[2].header("if-none-match"), None);
    assert_eq!(std::fs::read(&cfg_b.db_path).unwrap(), city_db());
    assert_eq!(
        read_meta(&cfg_b.db_path).unwrap().source_id,
        format!("custom:{}", server.url("/b.mmdb"))
    );
    assert_eq!(local_copy(&cfg_b), LocalCopy::Current);

    // 从自定义 URL 改成 DB-IP：同样视为没有本地副本
    let mut cfg_dbip = GeoConfig::new(dir.path());
    cfg_dbip.source = GeoSource::DbIp {
        edition: DbIpEdition::Country,
    };
    assert_eq!(local_copy(&cfg_dbip), LocalCopy::Foreign);
}

#[tokio::test]
async fn a_failed_switch_keeps_serving_the_old_database_and_its_attribution() {
    let dir = TempDir::new("switch");
    let server = start(|req| {
        if req.target.starts_with("/free/") {
            Reply::ok(gz(&country_db()))
        } else {
            Reply::status(500)
        }
    })
    .await;
    let http = client();

    // 先用 DB-IP（country）下载
    let mut cfg = GeoConfig::new(dir.path());
    cfg.source = GeoSource::DbIp {
        edition: DbIpEdition::Country,
    };
    // 测试用的库 database_type 是 GeoLite2-Country，含 Country，校验通过
    refresh_with(&cfg, &http, &dbip_opts(&server))
        .await
        .unwrap();
    let db = GeoDb::from_config(&cfg);
    db.reload_if_changed(&cfg.db_path).unwrap();
    assert_eq!(db.attribution().unwrap().source, "dbip-lite");

    // 改成自定义 URL，新库下载失败：库与署名都还是 DB-IP 的
    let mut cfg2 = custom_cfg(dir.path(), server.url("/custom.mmdb"));
    cfg2.attribution_text = Some("Geo by Me".into());
    assert_eq!(local_copy(&cfg2), LocalCopy::Foreign);
    assert!(refresh(&cfg2, &http).await.is_err());
    db.reload_if_changed(&cfg.db_path).unwrap();
    assert_eq!(db.attribution().unwrap().source, "dbip-lite");
    assert!(db.is_loaded());
}

#[tokio::test]
async fn dbip_tries_current_month_then_falls_back_to_previous_month_on_404() {
    let http = client();

    // 当月文件已发布：只请求一次
    let dir = TempDir::new("dbip-now");
    let server = start(|req| {
        if req.target == "/free/dbip-country-lite-2026-10.mmdb.gz" {
            Reply::ok(gz(&country_db())).with_header("ETag", "\"oct\"")
        } else {
            Reply::status(404)
        }
    })
    .await;
    let mut cfg = GeoConfig::new(dir.path());
    cfg.source = GeoSource::DbIp {
        edition: DbIpEdition::Country,
    };
    assert!(matches!(
        refresh_with(&cfg, &http, &dbip_opts(&server))
            .await
            .unwrap(),
        RefreshOutcome::Updated { .. }
    ));
    assert_eq!(
        server.targets(),
        ["/free/dbip-country-lite-2026-10.mmdb.gz"]
    );
    let m = read_meta(&cfg.db_path).unwrap();
    assert_eq!(
        (m.source.as_str(), m.source_id.as_str()),
        ("dbip-lite", "dbip-lite:country")
    );
    // 署名内置，不读配置
    let a = m.attribution.unwrap();
    assert_eq!(
        (a.text.as_str(), a.url.as_str(), a.license.as_deref()),
        (
            "IP Geolocation by DB-IP",
            "https://db-ip.com",
            Some("CC BY 4.0")
        )
    );

    // 月初当月文件还没有（404）：回退上月，用上月的校验头
    let dir = TempDir::new("dbip-prev");
    let server = start(|req| {
        if req.target == "/free/dbip-city-lite-2026-09.mmdb.gz" {
            Reply::ok(gz(&std::fs::read(test_mmdb("GeoLite2-City-Test")).unwrap()))
        } else {
            Reply::status(404)
        }
    })
    .await;
    let cfg = GeoConfig::new(dir.path()); // 默认 DB-IP city
    assert!(matches!(
        refresh_with(&cfg, &http, &dbip_opts(&server))
            .await
            .unwrap(),
        RefreshOutcome::Updated { .. }
    ));
    assert_eq!(
        server.targets(),
        [
            "/free/dbip-city-lite-2026-10.mmdb.gz",
            "/free/dbip-city-lite-2026-09.mmdb.gz"
        ]
    );
    assert_eq!(read_meta(&cfg.db_path).unwrap().source_id, "dbip-lite:city");

    // 两个月都没有：失败，不留文件
    let dir = TempDir::new("dbip-none");
    let server = start(|_| Reply::status(404)).await;
    let cfg = GeoConfig::new(dir.path());
    let err = refresh_with(&cfg, &http, &dbip_opts(&server))
        .await
        .unwrap_err();
    assert!(format!("{err:#}").contains("HTTP 404"), "{err:#}");
    assert_eq!(server.targets().len(), 2);
    assert!(!cfg.db_path.exists());

    // 当月 500 不回退（只有 404 才回退）
    let dir = TempDir::new("dbip-500");
    let server = start(|_| Reply::status(500)).await;
    let cfg = GeoConfig::new(dir.path());
    assert!(
        refresh_with(&cfg, &http, &dbip_opts(&server))
            .await
            .is_err()
    );
    assert_eq!(server.targets().len(), 1);
}

#[tokio::test]
async fn maxmind_uses_basic_auth_extracts_tar_gz_and_does_not_leak_credentials_on_redirect() {
    let dir = TempDir::new("maxmind");
    let city = std::fs::read(test_mmdb("GeoLite2-City-Test")).unwrap();
    let archive = tar_gz("GeoLite2-City_20261020/GeoLite2-City.mmdb", &city);
    // 对象存储（另一个源：不同端口）：只认校验头，不要认证
    let blob = Arc::new(Mutex::new(archive));
    let b = Arc::clone(&blob);
    let storage = start(move |req| {
        if req.header("if-none-match") == Some("\"r2\"") {
            return Reply::status(304).with_header("ETag", "\"r2\"");
        }
        Reply::ok(b.lock().unwrap().clone())
            .with_header("ETag", "\"r2\"")
            .with_header("Last-Modified", LAST_MODIFIED_1)
    })
    .await;
    let location = storage.url("/blob/GeoLite2-City.tar.gz?X-Amz-Signature=abc");
    let api = start(move |req| {
        if req.header("authorization") != Some("Basic MTIzNDU2OmszeQ==") {
            return Reply::status(401)
                .with_header("WWW-Authenticate", "Basic realm=\"geoip-download\"");
        }
        Reply::redirect(location.clone())
    })
    .await;

    let mut cfg = GeoConfig::new(dir.path());
    cfg.source = GeoSource::MaxMind {
        account_id: "123456".into(),
        license_key: Secret::new("k3y"),
        edition: "GeoLite2-City".into(),
    };
    let opts = RefreshOptions {
        maxmind_base_url: api.url("/geoip/databases"),
        ..RefreshOptions::default()
    };
    let http = client();
    let out = refresh_with(&cfg, &http, &opts).await.unwrap();
    assert!(matches!(out, RefreshOutcome::Updated { .. }), "{out:?}");

    // 现行接口形式与认证
    assert_eq!(
        api.targets(),
        ["/geoip/databases/GeoLite2-City/download?suffix=tar.gz"]
    );
    // 跨源重定向不能带上 Authorization（reqwest 会剥掉）
    assert_eq!(storage.requests().len(), 1);
    assert_eq!(
        storage.requests()[0].header("authorization"),
        None,
        "凭据不能发给重定向目标"
    );

    // 取出 tar.gz 里的 mmdb；meta 记录来源、署名（内置）、重定向目标给的校验头
    assert_eq!(std::fs::read(&cfg.db_path).unwrap(), city);
    let m = read_meta(&cfg.db_path).unwrap();
    assert_eq!(
        (m.source.as_str(), m.source_id.as_str()),
        ("maxmind-geolite2", "maxmind-geolite2:GeoLite2-City")
    );
    assert_eq!(m.etag.as_deref(), Some("\"r2\""));
    assert!(
        m.attribution
            .unwrap()
            .text
            .starts_with("This product includes GeoLite Data created by MaxMind")
    );
    let meta_text = std::fs::read_to_string(rivu_geo::meta_path(&cfg.db_path)).unwrap();
    assert!(
        !meta_text.contains("k3y") && !meta_text.contains("123456") && !meta_text.contains("X-Amz"),
        "{meta_text}"
    );
    assert_only_db_and_meta(&cfg);

    // 再刷新：校验头沿重定向链送到存储端，304
    assert_eq!(
        refresh_with(&cfg, &http, &opts).await.unwrap(),
        RefreshOutcome::NotModified { by_hash: false }
    );
    assert_eq!(
        storage.requests()[1].header("if-none-match"),
        Some("\"r2\"")
    );

    // 凭据错误：401，旧库保留
    let before = snapshot(&cfg);
    cfg.source = GeoSource::MaxMind {
        account_id: "123456".into(),
        license_key: Secret::new("wrong"),
        edition: "GeoLite2-City".into(),
    };
    let err = refresh_with(&cfg, &http, &opts).await.unwrap_err();
    let msg = format!("{err:#}");
    assert!(msg.contains("HTTP 401") && !msg.contains("wrong"), "{msg}");
    assert_eq!(snapshot(&cfg), before);
}

#[tokio::test]
async fn url_tokens_never_appear_in_errors_or_meta() {
    let http = client();
    let dir = TempDir::new("secret");
    let server = start(|req| {
        if req.target.contains("fail") {
            Reply::status(500)
        } else {
            Reply::ok(country_db())
        }
    })
    .await;

    // 成功：meta 里不能有令牌和用户信息
    let url = format!("http://user:pass@{}/geo.mmdb?token=SECRET", server.addr);
    let cfg = custom_cfg(dir.path(), url);
    refresh(&cfg, &http).await.unwrap();
    let meta_text = std::fs::read_to_string(rivu_geo::meta_path(&cfg.db_path)).unwrap();
    assert!(
        !meta_text.contains("SECRET") && !meta_text.contains("pass"),
        "{meta_text}"
    );
    assert!(
        meta_text.contains(&format!("custom:http://{}/geo.mmdb", server.addr)),
        "{meta_text}"
    );

    // HTTP 错误
    let cfg = custom_cfg(
        dir.path(),
        format!("http://{}/fail.mmdb?token=SECRET", server.addr),
    );
    let err = format!("{:#}", refresh(&cfg, &http).await.unwrap_err());
    assert!(!err.contains("SECRET"), "{err}");

    // 连接失败（端口已关闭）
    let closed = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap()
    };
    let cfg = custom_cfg(dir.path(), format!("http://{closed}/geo.mmdb?token=SECRET"));
    let err = format!("{:#}", refresh(&cfg, &http).await.unwrap_err());
    assert!(!err.contains("SECRET"), "{err}");
    assert!(
        err.contains(&format!("http://{closed}/geo.mmdb")),
        "错误应指出（脱敏后的）URL：{err}"
    );
}

#[tokio::test]
async fn a_stalled_server_hits_the_overall_timeout() {
    let dir = TempDir::new("stall");
    let server = start(|_| Reply::Stall).await;
    let cfg = custom_cfg(dir.path(), server.url("/geo.mmdb"));
    let http = HttpClient::with_options(HttpOptions {
        timeout: Duration::from_millis(600),
        no_proxy: true,
        ..HttpOptions::default()
    })
    .unwrap();
    let started = Instant::now();
    assert!(refresh(&cfg, &http).await.is_err());
    assert!(started.elapsed() < Duration::from_secs(10));
    assert!(!cfg.db_path.exists());
}

#[tokio::test]
async fn stale_temp_files_from_a_crash_are_cleaned_up() {
    let dir = TempDir::new("stale");
    let server = start(|_| Reply::ok(country_db())).await;
    let cfg = custom_cfg(dir.path(), server.url("/geo.mmdb"));
    let parent: PathBuf = cfg.db_path.parent().unwrap().to_path_buf();
    std::fs::create_dir_all(&parent).unwrap();
    let old = parent.join("geoip.mmdb.99999.dl.tmp");
    let fresh = parent.join("geoip.mmdb.88888.tmp");
    std::fs::write(&old, b"leftover").unwrap();
    std::fs::write(&fresh, b"in progress elsewhere").unwrap();
    let two_hours_ago = std::time::SystemTime::now() - Duration::from_secs(7200);
    std::fs::File::options()
        .write(true)
        .open(&old)
        .unwrap()
        .set_modified(two_hours_ago)
        .unwrap();

    refresh(&cfg, &client()).await.unwrap();
    assert!(!old.exists(), "超过一小时的残留临时文件应被清理");
    assert!(
        fresh.exists(),
        "新近修改的临时文件可能属于另一个进程，不能删"
    );
}
