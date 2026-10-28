//! 会话预热（`system-design.md` §4.6「预热」）：进程「重启」之后，同一个访客的下一个事件续在原来的会话上。
//!
//! - 从 ClickHouse 预热：第一个管线写入事件，第二个管线（新的 SessionAssigner）用 `rivu_writer` 账号读 `sessions` 重建；
//! - 关停快照：第二个管线写快照，第三个管线加载快照，快照读后删除。
//!
//! 需要 `RIVU_TEST_DATABASE_URL` 与 `RIVU_TEST_CLICKHOUSE_URL`；缺少时跳过。

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use clickhouse::Client;
use rivu::migrate;
use rivu::session_warm;
use rivu_common::config::{BufferConfig, ChAccount, Config, Env, Role, Secret};
use rivu_geo::GeoDb;
use rivu_ingest::pipeline::{Pipeline, SaltCache, SiteSnapshot};
use rivu_storage_pg::sites::NewSite;
use rivu_ua::UaParser;
use rivu_writer::{BatchWriter, ChSink, WriterOptions};
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

const HOST: &str = "warm.example.test";
const UA_A: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/118.0.5993.771 YaBrowser/23.11.2.771 Yowser/2.5 Safari/537.36";
const UA_B: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/16.5 Safari/605.1.15 Ddg/17.2";

fn var(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

fn account(role: &str) -> ChAccount {
    let upper = role.to_uppercase();
    ChAccount {
        user: var(&format!("RIVU_TEST_CH_{upper}_USER")).unwrap_or_else(|| format!("rivu_{role}")),
        password: Secret::new(
            var(&format!("RIVU_TEST_CH_{upper}_PASSWORD"))
                .unwrap_or_else(|| format!("rivu_dev_{role}")),
        ),
    }
}

fn ch_database() -> String {
    var("RIVU_TEST_CH_DATABASE").unwrap_or_else(|| "rivu".into())
}

struct Collector {
    pipeline: Arc<Pipeline>,
    writer: BatchWriter,
    addr: SocketAddr,
}

async fn collector(
    ch_url: &str,
    sites: &Arc<SiteSnapshot>,
    salts: &Arc<SaltCache>,
    ua: &Arc<UaParser>,
    routing_secret: [u8; 16],
) -> Collector {
    let writer = BatchWriter::spawn(
        Arc::new(ChSink::new(rivu_storage_ch::writer_client(
            ch_url,
            &account("writer"),
            &ch_database(),
        ))),
        WriterOptions::new(BufferConfig {
            flush_interval: Duration::from_millis(100),
            ..BufferConfig::default()
        }),
    );
    let pipeline = Arc::new(
        Pipeline::new(
            Arc::clone(sites),
            Arc::clone(salts),
            writer.handle(),
            vec![],
            Arc::clone(ua),
            Arc::new(GeoDb::new()),
        )
        .with_routing_secret(routing_secret),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = rivu::public::router_with_scripts(Some(Arc::clone(&pipeline)), None);
    tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .unwrap();
    });
    Collector {
        pipeline,
        writer,
        addr,
    }
}

async fn pageview(addr: SocketAddr, key: &str, path: &str, ua: &str) {
    let body = json!({"v": 1, "s": key, "n": "pageview", "u": format!("https://{HOST}{path}")})
        .to_string();
    let req = format!(
        "POST /api/event HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\
         Content-Type: text/plain\r\nUser-Agent: {ua}\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    let mut stream = TcpStream::connect(addr).await.unwrap();
    stream.write_all(req.as_bytes()).await.unwrap();
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).await.unwrap();
    let raw = String::from_utf8_lossy(&raw);
    assert!(raw.starts_with("HTTP/1.1 202"), "{raw}");
    assert!(
        !raw.to_ascii_lowercase().contains("\r\nx-rivu-dropped:"),
        "{raw}"
    );
}

/// 站点每个会话的事件数，按会话开始时间排序。
async fn events_per_session(reader: &Client, site_id: u64) -> Vec<u64> {
    reader
        .query(
            "SELECT count() FROM events WHERE site_id = {site:UInt64} \
             GROUP BY session_id ORDER BY min(session_start), session_id",
        )
        .param("site", site_id)
        .fetch_all::<u64>()
        .await
        .expect("按会话计数")
}

#[tokio::test]
async fn restarted_collector_continues_sessions_from_clickhouse_and_snapshot() {
    let (Some(pg_url), Some(ch_url)) = (
        var("RIVU_TEST_DATABASE_URL"),
        var("RIVU_TEST_CLICKHOUSE_URL"),
    ) else {
        println!("跳过：需要设置 RIVU_TEST_DATABASE_URL 和 RIVU_TEST_CLICKHOUSE_URL");
        return;
    };
    let migrator = account("migrator");
    let cfg_env = Env::from_pairs([
        ("RIVU_DATABASE_URL", pg_url.as_str()),
        ("RIVU_CLICKHOUSE_URL", ch_url.as_str()),
        ("RIVU_CH_DATABASE", ch_database().as_str()),
        ("RIVU_CH_MIGRATOR_USER", migrator.user.as_str()),
        ("RIVU_CH_MIGRATOR_PASSWORD", migrator.password.expose()),
    ]);
    let config = Config::from_env(Role::Migrate, &cfg_env).expect("测试配置");
    migrate::apply(&config, false).await.expect("migrate");

    let pg = rivu_storage_pg::connect(&pg_url, 4).await.expect("连接 PG");
    let hostnames = vec![HOST.to_owned()];
    let site = rivu_storage_pg::sites::create_site(
        &pg,
        &NewSite {
            name: "warmup",
            timezone: "UTC",
            hostnames: &hostnames,
        },
    )
    .await
    .expect("建站");
    let today = rivu_ingest::pipeline::utc_today();
    let window: Vec<_> = (-2..=1)
        .map(|d| today.saturating_add(jiff::Span::new().days(d)))
        .collect();
    rivu_storage_pg::salts::ensure_salts(&pg, &window)
        .await
        .expect("ensure_salts");
    let sites = Arc::new(SiteSnapshot::new());
    sites.reload(&pg).await.unwrap();
    let salts = Arc::new(SaltCache::new());
    let (from, to) = rivu_ingest::pipeline::salt_window(today);
    salts.reload(&pg, from, to).await.unwrap();
    let ua = Arc::new(UaParser::embedded().expect("uap-core 规则"));
    // 重启前后同一个 routing 密钥（PG 持久化），同一访客落在同一分片
    let routing = rivu_storage_pg::salts::routing_secret(&pg)
        .await
        .expect("routing 密钥");
    assert_eq!(
        rivu_storage_pg::salts::routing_secret(&pg).await.unwrap(),
        routing
    );
    let writer_ch = rivu_storage_ch::writer_client(&ch_url, &account("writer"), &ch_database());
    let reader = rivu_storage_ch::client(&ch_url, &account("reader"), Some(&ch_database()));
    let data_dir = std::env::temp_dir().join(format!("rivu-warmup-{}", site.id));
    let snapshot = session_warm::snapshot_path(&data_dir);
    let key = &site.site_key;

    // 第一次运行：访客 A 开会话（两个页面），访客 B 开会话（一个页面）
    let c1 = collector(&ch_url, &sites, &salts, &ua, routing).await;
    pageview(c1.addr, key, "/a1", UA_A).await;
    pageview(c1.addr, key, "/a2", UA_A).await;
    pageview(c1.addr, key, "/b1", UA_B).await;
    drop(c1.pipeline);
    c1.writer.shutdown().await.expect("flush");

    // 第二次运行：从 ClickHouse 预热（没有快照文件）
    let c2 = collector(&ch_url, &sites, &salts, &ua, routing).await;
    let status = session_warm::warm(&c2.pipeline, &data_dir, &writer_ch).await;
    assert_eq!(status.source, "clickhouse", "{status:?}");
    assert!(status.sessions >= 2, "{status:?}");
    assert!(status.clickhouse_error.is_none(), "{status:?}");
    pageview(c2.addr, key, "/a3", UA_A).await;
    // 关停：写快照
    session_warm::save(&c2.pipeline, &data_dir).await;
    assert!(snapshot.is_file());
    drop(c2.pipeline);
    c2.writer.shutdown().await.expect("flush");

    // 第三次运行：加载快照（读后删除）
    let c3 = collector(&ch_url, &sites, &salts, &ua, routing).await;
    let status = session_warm::warm(&c3.pipeline, &data_dir, &writer_ch).await;
    assert_eq!(status.source, "snapshot", "{status:?}");
    assert!(status.sessions >= 2, "{status:?}");
    assert!(!snapshot.exists(), "快照读过就删除");
    pageview(c3.addr, key, "/a4", UA_A).await;
    pageview(c3.addr, key, "/b2", UA_B).await;
    drop(c3.pipeline);
    c3.writer.shutdown().await.expect("flush");

    // 三次运行跨过两次「重启」，仍然只有两个会话：A 4 个事件，B 2 个事件
    assert_eq!(events_per_session(&reader, site.id).await, vec![4, 2]);

    let admin = rivu_storage_ch::client(&ch_url, &account("migrator"), Some(&ch_database()));
    for table in ["events", "sessions", "ingest_counters"] {
        let _ = admin
            .query(&format!(
                "ALTER TABLE {table} DELETE WHERE site_id = {{site:UInt64}}"
            ))
            .param("site", site.id)
            .execute()
            .await;
    }
    let _ = std::fs::remove_dir_all(&data_dir);
}
