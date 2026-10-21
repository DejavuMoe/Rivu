//! 角色的公共启动流程：构建依赖 -> 绑定端口 -> 服务 -> 优雅关停。

use std::process::ExitCode;
use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, Result};
use metrics_exporter_prometheus::PrometheusBuilder;
use rivu_common::build_info;
use rivu_common::config::{Config, Role};
use tokio::net::TcpListener;
use tokio::sync::watch;

use crate::internal::{self, InternalState, Probe};
use crate::public;

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
    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            let _ = stop.wait_for(|stopped| *stopped).await;
        })
        .await
        .with_context(|| format!("{name} 服务异常退出"))
}

pub async fn run(config: Config) -> Result<ExitCode> {
    let role = config.role;
    let started = Instant::now();

    // 依赖：懒连接，依赖暂时不可用时进程照常启动，由 /readyz 报告。
    let ch_account = config
        .ch_probe_account()
        .context("没有可用于 /readyz 的 ClickHouse 账号")?;
    let probe = Probe::new(
        rivu_storage_pg::connect_lazy(config.database_url.expose(), 4)?,
        rivu_storage_ch::client(&config.clickhouse_url, ch_account, None),
        config.ch_database.clone(),
    )?;

    let metrics = PrometheusBuilder::new()
        .install_recorder()
        .context("安装 Prometheus recorder 失败")?;
    internal::describe_metrics();

    let state = Arc::new(InternalState {
        role,
        probe,
        metrics,
        started,
    });

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
    let public_task = async {
        match public_listener {
            Some(l) => serve("公共端口", l, public::router(role), stop_rx.clone()).await,
            None => Ok(()),
        }
    };
    let (internal_result, public_result) = tokio::join!(internal_task, public_task);
    internal_result?;
    public_result?;

    tracing::info!("rivu 已退出");
    Ok(ExitCode::SUCCESS)
}
