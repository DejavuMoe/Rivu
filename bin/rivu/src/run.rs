//! 角色的公共启动流程：构建依赖 -> 绑定端口 -> 装配 -> 服务 -> 优雅关停。
//!
//! 装配（按角色）：
//! - collector / all：BatchWriter（`rivu_writer` 账号）、站点快照、盐缓存、接入管线、会话清扫；
//! - worker / all：后台任务 `salts_rotate`（启动时补跑一次，之后每小时一次）、`geoip_refresh`
//!   （无本地副本时立即下载，之后按 `RIVU_GEOIP_REFRESH_INTERVAL` 条件 GET）；
//! - collector / all：持有 GeoIP 库并每小时检查文件修改时间、热加载（管线尚未接入，见 `rivu-geo`）；
//! - api：P1 没有 HTTP 接口，`/api/*` 返回 501，指标用命令行 `rivu stats` 查询。
//!
//! 关停顺序：停止接收（HTTP 服务优雅关停）→ 停止后台刷新任务 → 排空 BatchWriter 并 flush →
//! 刷出全部接入计数（包括还没结束的 10 秒桶）。

use std::net::SocketAddr;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use metrics_exporter_prometheus::PrometheusBuilder;
use rivu_common::build_info;
use rivu_common::config::{Config, Role};
use rivu_geo::GeoDb;
use rivu_ingest::pipeline::{Pipeline, SaltCache, SiteSnapshot, salt_window, utc_today};
use rivu_jobs::GeoRefreshState;
use rivu_writer::{BatchWriter, ChSink, WriterOptions};
use sqlx::PgPool;
use tokio::net::TcpListener;
use tokio::sync::watch;
use tokio::task::JoinHandle;

use crate::internal::{self, CollectorReadiness, GeoReadiness, InternalState, Probe};
use crate::public;

/// collector 比较 GeoIP 库文件修改时间的周期（`platform-and-ops.md` §6：每小时）。
const GEO_RELOAD_CHECK_INTERVAL: Duration = Duration::from_secs(3600);

/// 盐缓存的重载周期（`platform-and-ops.md` §3.3：各进程每 60 秒从 PG 刷新）。
const SALT_CACHE_REFRESH: Duration = Duration::from_secs(60);
/// 会话清扫周期（§4.6「过期」）。
const SESSION_SWEEP_INTERVAL: Duration = Duration::from_secs(60);

/// 等待 Ctrl-C；Unix 下还等待 SIGTERM。
async fn wait_for_signal() {
    let ctrl_c = async {
        if let Err(e) = tokio::signal::ctrl_c().await {
            tracing::error!(error = %e, "监听 Ctrl-C 失败");
            std::future::pending::<()>().await;
        }
    };

    #[cfg(unix)]
    let terminate = async {
        use tokio::signal::unix::{SignalKind, signal};
        match signal(SignalKind::terminate()) {
            Ok(mut s) => {
                s.recv().await;
            }
            Err(e) => {
                tracing::error!(error = %e, "监听 SIGTERM 失败");
                std::future::pending::<()>().await;
            }
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => tracing::info!("收到 Ctrl-C，开始优雅关停"),
        () = terminate => tracing::info!("收到 SIGTERM，开始优雅关停"),
    }
}

async fn serve(
    name: &'static str,
    listener: TcpListener,
    app: axum::Router,
    mut stop: watch::Receiver<bool>,
) -> Result<()> {
    // `ConnectInfo<SocketAddr>`：collector 要用 TCP 对端地址判定客户端 IP。
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(async move {
        let _ = stop.wait_for(|stopped| *stopped).await;
    })
    .await
    .with_context(|| format!("{name} 服务异常退出"))
}

/// PG 连接池上限（`platform-and-ops.md` §7：collector 4 / api 16 / worker 8 / all 24）。
fn pg_max_connections(role: Role) -> u32 {
    match role {
        Role::Collector => 4,
        Role::Api => 16,
        Role::Worker => 8,
        Role::All => 24,
        Role::Migrate => 2,
    }
}

/// collector 一侧装配出的组件。
struct Collector {
    pipeline: Arc<Pipeline>,
    readiness: CollectorReadiness,
    writer: BatchWriter,
}

/// 装配 collector：写入器、站点快照、盐缓存与管线，并启动它们的后台任务。
/// 站点快照和盐先尽力加载一次，之后由后台任务定期重载；加载失败不阻止启动，由 `/readyz` 报告。
async fn start_collector(
    config: &Config,
    pg: &PgPool,
    stop: &watch::Receiver<bool>,
    background: &mut Vec<JoinHandle<()>>,
) -> Result<Collector> {
    let account = config
        .ch_writer
        .as_ref()
        .context("collector 需要 RIVU_CH_WRITER_USER / RIVU_CH_WRITER_PASSWORD")?;
    let ch = rivu_storage_ch::writer_client(&config.clickhouse_url, account, &config.ch_database);
    let writer = BatchWriter::spawn(Arc::new(ChSink::new(ch)), WriterOptions::new(config.buffer));

    let sites = Arc::new(SiteSnapshot::new());
    let salts = Arc::new(SaltCache::new());
    match sites.reload(pg).await {
        Ok(n) => tracing::info!(sites = n, "站点快照已加载"),
        Err(e) => tracing::warn!(error = %format!("{e:#}"), "启动时加载站点快照失败，稍后重试"),
    }
    let (from, to) = salt_window(utc_today());
    match salts.reload(pg, from, to).await {
        Ok(n) => tracing::info!(salts = n, "盐缓存已加载"),
        Err(e) => tracing::warn!(error = %format!("{e:#}"), "启动时加载盐失败，稍后重试"),
    }
    background.push(sites.spawn_refresh(pg.clone(), config.site_cache_refresh, stop.clone()));
    background.push(salts.spawn_refresh(pg.clone(), SALT_CACHE_REFRESH, stop.clone()));

    let pipeline = Arc::new(Pipeline::new(
        Arc::clone(&sites),
        Arc::clone(&salts),
        writer.handle(),
        config.trusted_proxies.clone(),
    ));
    background.push(pipeline.spawn_sweeper(SESSION_SWEEP_INTERVAL, stop.clone()));

    Ok(Collector {
        pipeline,
        readiness: CollectorReadiness { sites, salts },
        writer,
    })
}

pub async fn run(config: Config) -> Result<ExitCode> {
    let role = config.role;
    let started = Instant::now();

    // 依赖：懒连接，依赖暂时不可用时进程照常启动，由 /readyz 报告。
    let ch_account = config
        .ch_probe_account()
        .context("没有可用于 /readyz 的 ClickHouse 账号")?;
    let pg = rivu_storage_pg::connect_lazy(config.database_url.expose(), pg_max_connections(role))?;
    let probe = Probe::new(
        pg.clone(),
        rivu_storage_ch::client(&config.clickhouse_url, ch_account, None),
        config.ch_database.clone(),
    )?;

    let metrics = PrometheusBuilder::new()
        .install_recorder()
        .context("安装 Prometheus recorder 失败")?;
    internal::describe_metrics();
    rivu_writer::describe_metrics();
    rivu_ingest::pipeline::describe_metrics();
    rivu_geo::describe_metrics();

    // 先绑定端口：端口被占用时不应该留下已经启动的后台任务。
    let internal_listener = TcpListener::bind(config.internal_addr)
        .await
        .with_context(|| format!("无法绑定内部端口 {}", config.internal_addr))?;
    let public_listener = if role == Role::Worker {
        None
    } else {
        Some(
            TcpListener::bind(config.listen_addr)
                .await
                .with_context(|| format!("无法绑定公共端口 {}", config.listen_addr))?,
        )
    };

    // 后台任务的停止信号与 HTTP 服务分开：先停服务，再停后台任务，最后排空写缓冲。
    let (bg_stop_tx, bg_stop_rx) = watch::channel(false);
    let mut background: Vec<JoinHandle<()>> = Vec::new();

    // 后台任务 salts_rotate：启动时先补跑一次（collector 才有盐可读），之后每小时一次。
    if role.runs_jobs() {
        rivu_jobs::run_once(&pg).await;
        background.push(rivu_jobs::spawn_salts_rotation(
            pg.clone(),
            bg_stop_rx.clone(),
            false,
        ));
    }

    // GeoIP：collector / all 持有库并热加载（管线接入之前只是加载与报告状态）；
    // worker / all 运行 `geoip_refresh`，all 里刷新成功后直接热加载同一个库。
    for w in config.geo.warnings() {
        tracing::warn!("{w}");
    }
    let geo_db = role
        .serves_collector()
        .then(|| Arc::new(GeoDb::from_config(&config.geo)));
    if let Some(db) = &geo_db {
        background.push(db.spawn_reloader(
            config.geo.db_path.clone(),
            GEO_RELOAD_CHECK_INTERVAL,
            bg_stop_rx.clone(),
        ));
    }
    let geo_refresh = role
        .runs_jobs()
        .then(|| Arc::new(GeoRefreshState::default()));
    if let Some(state) = &geo_refresh {
        background.push(rivu_jobs::spawn_geoip_refresh(
            config.geo.clone(),
            geo_db.clone(),
            Arc::clone(state),
            bg_stop_rx.clone(),
        ));
    }

    let collector = if role.serves_collector() {
        Some(start_collector(&config, &pg, &bg_stop_rx, &mut background).await?)
    } else {
        None
    };

    let state = Arc::new(InternalState {
        role,
        probe,
        collector: collector.as_ref().map(|c| c.readiness.clone()),
        geo: GeoReadiness {
            db: geo_db,
            refresh: geo_refresh,
            db_path: config.geo.db_path.clone(),
        },
        metrics,
        started,
    });

    tracing::info!(
        role = role.as_str(),
        mode = ?config.mode,
        version = build_info::VERSION,
        commit = build_info::GIT_COMMIT,
        internal_addr = %config.internal_addr,
        listen_addr = ?public_listener.as_ref().map(|_| config.listen_addr),
        "rivu 已启动"
    );

    let (stop_tx, stop_rx) = watch::channel(false);
    tokio::spawn(async move {
        wait_for_signal().await;
        let _ = stop_tx.send(true);
    });

    let internal_task = serve(
        "内部端口",
        internal_listener,
        internal::router(state),
        stop_rx.clone(),
    );
    let pipeline = collector.as_ref().map(|c| Arc::clone(&c.pipeline));
    let public_task = async {
        match public_listener {
            Some(l) => {
                serve(
                    "公共端口",
                    l,
                    public::router(role, pipeline),
                    stop_rx.clone(),
                )
                .await
            }
            None => Ok(()),
        }
    };
    let (internal_result, public_result) = tokio::join!(internal_task, public_task);

    // 关停：HTTP 已停止接收 -> 停后台任务 -> 排空写缓冲、刷出计数。
    // 即使服务异常退出也要走完，避免丢掉缓冲里的数据。
    tracing::info!("已停止接收新请求，开始排空写缓冲");
    let _ = bg_stop_tx.send(true);
    for task in background {
        let _ = task.await;
    }
    let drained = match collector {
        Some(c) => {
            drop(c.pipeline);
            c.writer.shutdown().await
        }
        None => Ok(()),
    };

    internal_result?;
    public_result?;
    drained?;

    tracing::info!("rivu 已退出");
    Ok(ExitCode::SUCCESS)
}
