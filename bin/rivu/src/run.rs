//! 角色的公共启动流程：构建依赖 -> 绑定端口 -> 装配 -> 服务 -> 优雅关停。
//!
//! 装配（按角色）：
//! - collector / all：BatchWriter（`rivu_writer` 账号）、站点快照（含屏蔽规则与站点限额）、盐缓存、UA 解析器、
//!   接入管线（hostname、限流、垃圾引荐、屏蔽规则、机器人、地理、UA、来源）、
//!   会话预热（关停快照或 ClickHouse，开始服务之前完成）与清扫、站点脚本分发（`GET /js/<site_key>.js`，`core.js` 启动时从 `RIVU_TRACKER_DIR` 读入）；
//! - worker / all：后台任务 `salts_rotate`（启动时补跑一次，之后每小时一次）、`geoip_refresh`
//!   （无本地副本时立即下载，之后按 `RIVU_GEOIP_REFRESH_INTERVAL` 条件 GET）、`ch_delete_batch`
//!   （启动后一轮，之后每 `RIVU_STATS_DELETE_INTERVAL_SECS` 一轮，`rivu_maintainer` 账号）、`rollup_build`
//!   （日汇总层，每 `RIVU_ROLLUP_INTERVAL_SECS` 一轮，同一账号）、`ch_monitor`（ClickHouse parts / merge / mutation 积压，每 30 秒）；
//! - collector / all：持有 GeoIP 库并每小时检查文件修改时间、热加载；管线第 6 步用它查询位置；
//! - api / all：看板静态文件（`RIVU_DASHBOARD_DIR`，见 [`crate::dashboard`]）；公开查询 API（`POST /api/v1/query`、`GET /api/v1/query/schema.json`，见 [`crate::query_api`]）：
//!   `rivu_reader` 账号的 ClickHouse 客户端、API key 鉴权与按组织限流（[`crate::api_auth::build_guard`]）；
//!   看板与它的内部端点（[`crate::dash_api`]）；api 角色也持有 GeoIP 库（只用来在 `meta` 里回显署名与精度）。
//!
//! 关停顺序：停止接收（HTTP 服务优雅关停）→ 停止后台刷新任务 → 写会话快照 → 排空 BatchWriter 并 flush →
//! 刷出全部接入计数（包括还没结束的 10 秒桶）。

use std::net::SocketAddr;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use metrics_exporter_prometheus::{Matcher, PrometheusBuilder};
use rivu_common::build_info;
use rivu_common::config::{Config, Role};
use rivu_geo::GeoDb;
use rivu_geo::datacenter::DcDb;
use rivu_ingest::pipeline::{Pipeline, SaltCache, SiteSnapshot, salt_window, utc_today};
use rivu_jobs::GeoRefreshState;
use rivu_ua::UaParser;
use rivu_writer::{BatchWriter, ChSink, WriterOptions};
use sqlx::PgPool;
use tokio::net::TcpListener;
use tokio::sync::watch;
use tokio::task::JoinHandle;

use crate::api_auth;
use crate::dash_api::{self, DashApi};
use crate::internal::{self, CollectorReadiness, GeoReadiness, InternalState, Probe};
use crate::public;
use crate::query_api::{self, QueryApi};
use crate::script::{ScriptService, TrackerAssets};
use crate::session_warm;

/// collector 比较 GeoIP 库文件修改时间的周期（`platform-and-ops.md` §6：每小时）。
const GEO_RELOAD_CHECK_INTERVAL: Duration = Duration::from_secs(3600);

/// 盐缓存的重载周期（`platform-and-ops.md` §3.3：各进程每 60 秒从 PG 刷新）。
const SALT_CACHE_REFRESH: Duration = Duration::from_secs(60);
/// collector 检查数据中心网段文件修改时间的周期。
const DATACENTER_RELOAD_CHECK_INTERVAL: Duration = Duration::from_secs(300);
/// 启动时读取 routing 密钥的时间上限。
const ROUTING_SECRET_TIMEOUT: Duration = Duration::from_secs(10);
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
    tracker: Arc<TrackerAssets>,
    scripts: Arc<ScriptService>,
}

/// 装配 collector：写入器、站点快照、盐缓存与管线，并启动它们的后台任务。
/// 站点快照和盐先尽力加载一次，之后由后台任务定期重载；加载失败不阻止启动，由 `/readyz` 报告。
async fn start_collector(
    config: &Config,
    pg: &PgPool,
    geo_db: Arc<GeoDb>,
    datacenter: Option<Arc<DcDb>>,
    stop: &watch::Receiver<bool>,
    background: &mut Vec<JoinHandle<()>>,
) -> Result<Collector> {
    let account = config
        .ch_writer
        .as_ref()
        .context("collector 需要 RIVU_CH_WRITER_USER / RIVU_CH_WRITER_PASSWORD")?;
    let ch = rivu_storage_ch::writer_client(&config.clickhouse_url, account, &config.ch_database);
    let writer = BatchWriter::spawn(
        Arc::new(ChSink::new(ch.clone())),
        WriterOptions::new(config.buffer),
    );

    let sites = Arc::new(SiteSnapshot::new());
    let pepper = config
        .visitor_pepper
        .as_ref()
        .context("collector 需要 RIVU_VISITOR_PEPPER")?;
    let salts = Arc::new(SaltCache::new().with_pepper(Some(pepper.expose().as_bytes())));
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

    // 分片号的 routing 密钥：PG 里持久化，重启前后一致，会话预热才对得上（§4.5、§4.6）。
    // 读不到时用随机值照常启动，代价是本次运行之前的会话无法续上。
    let routing_secret = match tokio::time::timeout(
        ROUTING_SECRET_TIMEOUT,
        rivu_storage_pg::salts::routing_secret(pg),
    )
    .await
    {
        Ok(Ok(s)) => s,
        Ok(Err(e)) => {
            tracing::warn!(error = %format!("{e:#}"), "读取 routing 密钥失败，本次运行用随机值：会话无法预热");
            rand::random()
        }
        Err(_) => {
            tracing::warn!("读取 routing 密钥超时，本次运行用随机值：会话无法预热");
            rand::random()
        }
    };

    // uap-core 规则的编译约 130 ms（release），放到阻塞线程里，不占用异步工作线程。
    let ua = Arc::new(
        tokio::task::spawn_blocking(UaParser::embedded)
            .await
            .context("构建 UA 解析器的任务异常")?
            .context("构建 UA 解析器失败")?,
    );
    let mut pipeline = Pipeline::new(
        Arc::clone(&sites),
        Arc::clone(&salts),
        writer.handle(),
        config.trusted_proxies.clone(),
        ua,
        geo_db,
    )
    .with_default_ingest_rate(config.ingest_rate_limit_per_min)
    .with_routing_secret(routing_secret);
    // 数据中心网段（可选）：collector 按文件修改时间热加载 worker 下载的清单
    if let Some(dc) = datacenter {
        background.push(dc.spawn_reloader(
            config.datacenter.path.clone(),
            DATACENTER_RELOAD_CHECK_INTERVAL,
            stop.clone(),
        ));
        pipeline = pipeline.with_datacenter(dc);
    }
    let pipeline = Arc::new(pipeline);
    // 开始服务之前预热会话：关停快照，或从 ClickHouse 重建（§4.6「预热」）。
    let sessions = Arc::new(session_warm::warm(&pipeline, &config.geo.data_dir, &ch).await);
    background.push(pipeline.spawn_sweeper(SESSION_SWEEP_INTERVAL, stop.clone()));

    // 站点脚本：core.js 启动时读入并缓存；缺失不阻止启动，路由返回 503，/readyz 报告。
    if !config.public_base_url_explicit {
        tracing::warn!(
            base_url = %config.public_base_url,
            "未设置 RIVU_PUBLIC_BASE_URL：站点脚本里的采集端点取 http://<RIVU_LISTEN_ADDR>，只适合开发；生产必须设置为对外的 https 地址"
        );
    }
    let tracker = Arc::new(TrackerAssets::load(&config.tracker_dir));
    let scripts = Arc::new(ScriptService::new(
        Arc::clone(&sites),
        Arc::clone(&tracker),
        &config.public_base_url,
    ));

    Ok(Collector {
        pipeline,
        readiness: CollectorReadiness {
            sites,
            salts,
            sessions,
        },
        writer,
        tracker,
        scripts,
    })
}

/// 公共路由加上看板：看板是 fallback，只接住其他路由都不匹配的路径。
fn with_dashboard(
    router: axum::Router,
    dashboard: Option<Arc<crate::dashboard::DashboardAssets>>,
) -> axum::Router {
    match dashboard {
        Some(d) => router.merge(
            axum::Router::new()
                .fallback(crate::dashboard::serve)
                .with_state(d),
        ),
        None => router,
    }
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

    // 阶段耗时在微秒量级（UA 缓存命中约 150 ns，未命中约 20 µs，地理查询几 µs）：
    // 用固定桶而不是默认的 summary，这样多个副本的结果可以相加。
    let metrics = PrometheusBuilder::new()
        .set_buckets_for_metric(
            Matcher::Full(rivu_ingest::pipeline::STAGE_SECONDS_METRIC.to_owned()),
            &[
                1e-7, 5e-7, 1e-6, 2.5e-6, 5e-6, 1e-5, 2.5e-5, 5e-5, 1e-4, 2.5e-4, 5e-4, 1e-3, 5e-3,
                1e-2, 0.1,
            ],
        )
        .context("设置阶段耗时直方图的桶失败")?
        // 查询耗时：毫秒到 API 档上限（20 s）
        .set_buckets_for_metric(
            Matcher::Full("rivu_query_seconds".to_owned()),
            &query_api::QUERY_SECONDS_BUCKETS,
        )
        .context("设置查询耗时直方图的桶失败")?
        .install_recorder()
        .context("安装 Prometheus recorder 失败")?;
    internal::describe_metrics();
    rivu_writer::describe_metrics();
    rivu_ingest::pipeline::describe_metrics();
    rivu_geo::describe_metrics();
    query_api::describe_metrics();
    dash_api::describe_metrics();
    rivu_jobs::stats_delete::describe_metrics();
    rivu_jobs::rollup::describe_metrics();
    rivu_jobs::ch_monitor::describe_metrics();

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

    // 后台任务 ch_delete_batch：执行到期的统计数据删除意图（`rivu_maintainer` 账号）。
    // all 里看板写入日期范围意图后通过 `stats_delete_kick` 立即唤醒一次。
    // 结果缓存（§6.9）：看板与公开 API 共用；同进程的 ch_delete_batch 删除完成后让站点的结果失效
    let query_cache = role.serves_api().then(|| {
        Arc::new(rivu_query::QueryCache::new(
            rivu_query::cache::DEFAULT_CAPACITY_ROWS,
        ))
    });
    let stats_delete_kick = if role.runs_jobs() {
        let account = config
            .ch_maintainer
            .as_ref()
            .context("worker 需要 RIVU_CH_MAINTAINER_USER / RIVU_CH_MAINTAINER_PASSWORD")?;
        let maintainer =
            rivu_storage_ch::client(&config.clickhouse_url, account, Some(&config.ch_database));
        let kick = rivu_jobs::StatsDeleteKick::default();
        background.push(rivu_jobs::spawn_stats_delete(
            pg.clone(),
            maintainer.clone(),
            config.stats_delete_interval,
            Arc::clone(&kick),
            query_cache.clone(),
            bg_stop_rx.clone(),
        ));
        // 后台任务 rollup_build：日汇总层（同一个 maintainer 账号）
        // 后台任务 ch_monitor：parts、merge 与 mutation 积压
        background.push(rivu_jobs::spawn_ch_monitor(
            maintainer.clone(),
            config.ch_database.clone(),
            bg_stop_rx.clone(),
        ));
        // 后台任务 ch_monitor：parts、merge 与 mutation 积压
        background.push(rivu_jobs::spawn_ch_monitor(
            maintainer.clone(),
            config.ch_database.clone(),
            bg_stop_rx.clone(),
        ));
        background.push(rivu_jobs::spawn_rollup_build(
            pg.clone(),
            maintainer,
            config.rollup_interval,
            bg_stop_rx.clone(),
        ));
        Some(kick)
    } else {
        None
    };

    // GeoIP：collector / all 持有库并热加载（管线接入之前只是加载与报告状态）；
    // worker / all 运行 `geoip_refresh`，all 里刷新成功后直接热加载同一个库。
    for w in config.geo.warnings() {
        tracing::warn!("{w}");
    }
    // collector 用它查位置；api 只用它回显署名与精度（`meta.geo_attribution` / `geo_capability`）。
    let geo_db = (role.serves_collector() || role.serves_api())
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

    // 数据中心网段过滤（默认关闭）：worker / all 下载清单，collector / all 热加载并在管线第 5 步使用。
    let datacenter = (config.datacenter.enabled && (role.serves_collector() || role.runs_jobs()))
        .then(|| Arc::new(DcDb::new()));
    if config.datacenter.enabled {
        tracing::info!(
            sources = config.datacenter.sources.len(),
            path = %config.datacenter.path.display(),
            "数据中心网段过滤已开启：命中的事件按 datacenter_ip 丢弃"
        );
        if role.runs_jobs() {
            background.push(rivu_jobs::spawn_datacenter_refresh(
                config.datacenter.clone(),
                datacenter.clone().filter(|_| role.serves_collector()),
                bg_stop_rx.clone(),
            ));
        }
    }

    let collector = if role.serves_collector() {
        let geo = geo_db.clone().context("collector 角色没有 GeoIP 库对象")?;
        Some(
            start_collector(
                &config,
                &pg,
                geo,
                datacenter.clone(),
                &bg_stop_rx,
                &mut background,
            )
            .await?,
        )
    } else {
        None
    };

    // 公开查询 API：只在 api / all 角色。reader 账号是 readonly=1，客户端不设任何 setting（见 `rivu_storage_ch::client`）。
    let query_routes = if role.serves_api() {
        let reader_account = config
            .ch_reader
            .as_ref()
            .context("api 需要 RIVU_CH_READER_USER / RIVU_CH_READER_PASSWORD")?;
        let reader = rivu_storage_ch::client(
            &config.clickhouse_url,
            reader_account,
            Some(&config.ch_database),
        );
        let guard = api_auth::build_guard(&config, pg.clone());
        if !config.base_url_explicit {
            tracing::warn!(
                base_url = %config.base_url,
                "未设置 RIVU_BASE_URL：内部端点的 CSRF 按 Origin == http://<RIVU_LISTEN_ADDR> 校验，只适合开发；生产必须设置为对外的 https 地址"
            );
        }
        if !config.cookie_secure {
            tracing::warn!(
                "会话 cookie 不带 Secure（RIVU_BASE_URL 是 http 或 RIVU_COOKIE_SECURE=off）：只适合开发"
            );
        }
        match rivu_storage_pg::users::any_user_exists(&pg).await {
            Ok(false) => tracing::warn!(
                token_required = config.setup_token.is_some(),
                "实例还没有用户：POST /api/internal/setup 或 `rivu setup` 可以创建首个用户（建议设置 RIVU_SETUP_TOKEN）"
            ),
            Ok(true) => {}
            Err(e) => tracing::warn!(error = %format!("{e:#}"), "检查实例是否已初始化失败"),
        }
        let cache = query_cache.clone().context("api 角色的结果缓存")?;
        let dash = Arc::new(
            DashApi::new(&config, pg.clone(), reader.clone(), geo_db.clone())
                .context("装配内部端点失败")?
                .with_stats_delete_kick(stats_delete_kick.clone())
                .with_cache(Arc::clone(&cache)),
        );
        background.push(dash_api::spawn_maintenance(pg.clone(), bg_stop_rx.clone()));
        Some(
            query_api::router(Arc::new(
                QueryApi::new(
                    guard,
                    pg.clone(),
                    reader,
                    geo_db.clone(),
                    config.query_timeouts.api_secs,
                )
                .with_rollup(config.rollup_min_rows)
                .with_cache(cache),
            ))
            .merge(dash_api::router(dash)),
        )
    } else {
        None
    };

    // 看板静态文件：api / all 角色，启动时读进内存
    let dashboard = role.serves_api().then(|| {
        Arc::new(crate::dashboard::DashboardAssets::load(
            &config.dashboard_dir,
        ))
    });

    let state = Arc::new(InternalState {
        role,
        probe,
        collector: collector.as_ref().map(|c| c.readiness.clone()),
        datacenter: datacenter.filter(|_| role.serves_collector()),
        tracker: collector.as_ref().map(|c| Arc::clone(&c.tracker)),
        dashboard: dashboard.clone(),
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
    let scripts = collector.as_ref().map(|c| Arc::clone(&c.scripts));
    let public_task = async {
        match public_listener {
            Some(l) => {
                serve(
                    "公共端口",
                    l,
                    with_dashboard(
                        public::router_full(role, pipeline, scripts, query_routes),
                        dashboard,
                    ),
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
            session_warm::save(&c.pipeline, &config.geo.data_dir).await;
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
