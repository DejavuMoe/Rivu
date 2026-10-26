//! 站点快照的增量刷新与 NOTIFY（`system-design.md` §4.3）：新站点、白名单变更、删除不用等全量重载就生效。
//!
//! 需要 `RIVU_TEST_DATABASE_URL`（rivu_migrator）；缺少时跳过。只用 PG。

use std::sync::Arc;
use std::time::{Duration, Instant};

use rivu::migrate;
use rivu_common::config::{Config, Env, Role};
use rivu_ingest::pipeline::{Refresh, SiteSnapshot};
use rivu_storage_pg::sites::NewSite;
use sqlx::PgPool;
use tokio::sync::watch;

fn var(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

async fn setup() -> Option<PgPool> {
    let (Some(pg_url), Some(ch_url)) = (
        var("RIVU_TEST_DATABASE_URL"),
        var("RIVU_TEST_CLICKHOUSE_URL"),
    ) else {
        println!("跳过：需要设置 RIVU_TEST_DATABASE_URL 和 RIVU_TEST_CLICKHOUSE_URL");
        return None;
    };
    let cfg_env = Env::from_pairs([
        ("RIVU_DATABASE_URL", pg_url.as_str()),
        ("RIVU_CLICKHOUSE_URL", ch_url.as_str()),
        (
            "RIVU_CH_DATABASE",
            var("RIVU_TEST_CH_DATABASE").as_deref().unwrap_or("rivu"),
        ),
        (
            "RIVU_CH_MIGRATOR_USER",
            var("RIVU_TEST_CH_MIGRATOR_USER")
                .as_deref()
                .unwrap_or("rivu_migrator"),
        ),
        (
            "RIVU_CH_MIGRATOR_PASSWORD",
            var("RIVU_TEST_CH_MIGRATOR_PASSWORD")
                .as_deref()
                .unwrap_or("rivu_dev_migrator"),
        ),
    ]);
    let config = Config::from_env(Role::Migrate, &cfg_env).expect("测试配置");
    migrate::apply(&config, false).await.expect("migrate");
    Some(rivu_storage_pg::connect(&pg_url, 4).await.expect("连接 PG"))
}

async fn new_site(pg: &PgPool, name: &str) -> rivu_storage_pg::sites::CreatedSite {
    let hostnames = vec![format!("{name}.cache.test")];
    rivu_storage_pg::sites::create_site(
        pg,
        &NewSite {
            name,
            timezone: "UTC",
            hostnames: &hostnames,
        },
    )
    .await
    .expect("建站")
}

/// 在 `limit` 内轮询直到 `cond` 成立，返回用时。
async fn eventually(limit: Duration, mut cond: impl FnMut() -> bool) -> Option<Duration> {
    let start = Instant::now();
    while start.elapsed() < limit {
        if cond() {
            return Some(start.elapsed());
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    cond().then(|| start.elapsed())
}

#[tokio::test]
async fn incremental_refresh_picks_up_new_changed_and_deleted_sites() {
    let Some(pg) = setup().await else {
        return;
    };
    let a = new_site(&pg, "inc-a").await;
    let snap = SiteSnapshot::new();
    assert!(matches!(snap.refresh(&pg).await.unwrap(), Refresh::Full(_)));
    assert!(snap.get(&a.site_key).is_some());

    // 没有变化：增量刷新什么都不做（别的测试并行改动的站点可能被带上，所以只断言自己的站点）
    assert!(matches!(
        snap.refresh(&pg).await.unwrap(),
        Refresh::Incremental { .. }
    ));

    let b = new_site(&pg, "inc-b").await;
    rivu_storage_pg::sites::add_hostname(&pg, a.id, "extra.cache.test")
        .await
        .unwrap();
    let Refresh::Incremental { reloaded, .. } = snap.refresh(&pg).await.unwrap() else {
        panic!("应为增量刷新");
    };
    assert!(reloaded >= 2, "a 与 b 都重新加载：{reloaded}");
    assert!(snap.get(&b.site_key).is_some(), "新站点");
    assert!(
        snap.get(&a.site_key)
            .unwrap()
            .hostnames
            .matches("extra.cache.test"),
        "新白名单"
    );

    rivu_storage_pg::sites::soft_delete_site(&pg, a.id)
        .await
        .unwrap();
    let Refresh::Incremental { removed, .. } = snap.refresh(&pg).await.unwrap() else {
        panic!("应为增量刷新");
    };
    assert!(removed >= 1);
    assert!(snap.get(&a.site_key).is_none(), "删除后摘除");
    assert!(snap.get(&b.site_key).is_some(), "其他站点不受影响");
    rivu_storage_pg::sites::soft_delete_site(&pg, b.id)
        .await
        .unwrap();
}

#[tokio::test]
async fn notify_makes_changes_visible_within_a_second() {
    let Some(pg) = setup().await else {
        return;
    };
    let snap = Arc::new(SiteSnapshot::new());
    let (stop_tx, stop_rx) = watch::channel(false);
    // 定时刷新设成 1 小时：只剩启动时的一次全量重载，之后的变化只能经 NOTIFY 到达
    let task = snap.spawn_refresh(pg.clone(), Duration::from_secs(3600), stop_rx);
    assert!(
        eventually(Duration::from_secs(5), || snap.is_loaded())
            .await
            .is_some()
    );
    // LISTEN 在全量重载之后建立：给它一点时间
    tokio::time::sleep(Duration::from_millis(500)).await;

    let c = new_site(&pg, "notify-c").await;
    let took = eventually(Duration::from_secs(3), || snap.get(&c.site_key).is_some())
        .await
        .expect("新站点经 NOTIFY 出现");
    assert!(took < Duration::from_secs(2), "{took:?}");

    rivu_storage_pg::sites::soft_delete_site(&pg, c.id)
        .await
        .unwrap();
    eventually(Duration::from_secs(3), || snap.get(&c.site_key).is_none())
        .await
        .expect("删除经 NOTIFY 生效");

    stop_tx.send(true).unwrap();
    tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .expect("后台任务在停止信号后退出")
        .unwrap();
}
