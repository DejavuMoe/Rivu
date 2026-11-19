//! 会话预热与关停快照（`system-design.md` §4.6「预热」）。
//!
//! - 启动：`${RIVU_DATA_DIR}/sessions.snapshot` 不超过 30 分钟且格式一致就加载它（含别名 key）；
//!   否则从 ClickHouse 的 `sessions` 重建最近 31 分钟内活跃的会话（每个会话一个 key：`last_visitor_id`）。
//!   快照读过一次就删除：之后若进程崩溃，下次启动不会再读到这份过时的快照。
//! - 关停：停止接收事件之后导出全部会话写快照。
//!
//! 预热在 collector 开始服务之前同步完成（有时间上限），`/readyz` 的 `sessions` 项报告来源与会话数；
//! ClickHouse 不可用或超时只记 warn，照常启动，代价是这段时间的会话被切开（§4.6「失败语义」）。

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use clickhouse::Client;
use rivu_domain::SessionAttrs;
use rivu_ingest::pipeline::Pipeline;
use rivu_session::assigner::SessionRecord;
use rivu_session::snapshot;
use rivu_storage_ch::sessions_warm::{self, WarmSession};
use serde::Serialize;

/// 从 ClickHouse 重建的窗口：超时 30 分钟再加 1 分钟余量（§4.6）。
const WARM_WINDOW_MS: i64 = 31 * 60 * 1000;
/// 预热查询的时间上限。
const WARM_TIMEOUT: Duration = Duration::from_secs(30);
/// 最后活跃时间超过「现在 + 1 分钟」的会话不放回：未来的时间戳只能来自时钟回拨或别的节点时钟快，
/// 放回只会占内存（清扫的水位封顶到墙钟，它们不会再把同一分片的活跃会话清掉）。
const FUTURE_TOLERANCE_MS: i64 = 60_000;

/// 去掉时间戳在未来的会话，返回去掉的个数。
fn drop_future(records: &mut Vec<SessionRecord>, now_ms: i64) -> usize {
    let before = records.len();
    records.retain(|r| r.last_seen_ms <= now_ms + FUTURE_TOLERANCE_MS);
    before - records.len()
}

/// 预热的结果，`/readyz` 报告用。
#[derive(Clone, Debug, Serialize)]
pub struct WarmStatus {
    /// `snapshot`、`clickhouse` 或 `none`（两者都没有得到数据）。
    pub source: &'static str,
    pub sessions: usize,
    /// 最后活跃时间在未来、没有放回的会话数。
    pub future_dropped: usize,
    pub elapsed_ms: u64,
    /// 快照不可用的原因（文件不存在不算）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snapshot_error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub clickhouse_error: Option<String>,
}

pub fn snapshot_path(data_dir: &Path) -> PathBuf {
    data_dir.join(snapshot::FILE_NAME)
}

fn now_ms() -> i64 {
    jiff::Timestamp::now().as_millisecond()
}

fn from_warm(w: WarmSession) -> SessionRecord {
    SessionRecord {
        site_id: w.site_id,
        session_id: w.session_id,
        keys: vec![w.last_visitor_id],
        started_at_ms: w.started_at_ms,
        last_seen_ms: w.last_seen_ms,
        entry: (!w.entry_pathname.is_empty()).then_some(w.entry_pathname),
        attrs: SessionAttrs {
            referrer: w.referrer,
            referrer_source: w.referrer_source,
            channel: w.channel,
            utm_source: w.utm_source,
            utm_medium: w.utm_medium,
            utm_campaign: w.utm_campaign,
            utm_content: w.utm_content,
            utm_term: w.utm_term,
            click_id_param: w.click_id_param,
            country_code: w.country_code,
            region_code: w.region_code,
            city_geoname_id: w.city_geoname_id,
            device: w.device,
            browser: w.browser,
            browser_version: w.browser_version,
            os: w.os,
            os_version: w.os_version,
        },
        partition: w.ingest_partition,
    }
}

/// 预热 `pipeline` 的 SessionAssigner。`ch` 是能读 `sessions` 的客户端（collector 用 writer 账号）。
pub async fn warm(pipeline: &Pipeline, data_dir: &Path, ch: &Client) -> WarmStatus {
    let started = Instant::now();
    let path = snapshot_path(data_dir);
    let mut status = WarmStatus {
        source: "none",
        sessions: 0,
        future_dropped: 0,
        elapsed_ms: 0,
        snapshot_error: None,
        clickhouse_error: None,
    };

    let read = {
        let path = path.clone();
        tokio::task::spawn_blocking(move || snapshot::read_file(&path, now_ms())).await
    };
    match read {
        Ok(Ok(Some(mut records))) => {
            status.source = "snapshot";
            status.future_dropped = drop_future(&mut records, now_ms());
            status.sessions = pipeline.assigner().restore(records);
        }
        Ok(Ok(None)) => {}
        Ok(Err(e)) => status.snapshot_error = Some(e.to_string()),
        Err(e) => status.snapshot_error = Some(format!("读取快照的任务异常：{e}")),
    }
    if path.exists()
        && let Err(e) = std::fs::remove_file(&path)
    {
        tracing::warn!(path = %path.display(), error = %e, "删除已读过的会话快照失败");
    }

    if status.source == "none" {
        let since = now_ms() - WARM_WINDOW_MS;
        match tokio::time::timeout(
            WARM_TIMEOUT,
            sessions_warm::recent_sessions(ch, since, WARM_TIMEOUT.as_secs()),
        )
        .await
        {
            Ok(Ok(rows)) => {
                status.source = "clickhouse";
                let mut records: Vec<SessionRecord> = rows.into_iter().map(from_warm).collect();
                status.future_dropped = drop_future(&mut records, now_ms());
                status.sessions = pipeline.assigner().restore(records);
            }
            Ok(Err(e)) => status.clickhouse_error = Some(e.to_string()),
            Err(_) => {
                status.clickhouse_error = Some(format!("超时（{}s）", WARM_TIMEOUT.as_secs()))
            }
        }
    }

    status.elapsed_ms = started.elapsed().as_millis() as u64;
    if let Some(e) = &status.snapshot_error {
        tracing::warn!(error = %e, "会话快照不可用，改从 ClickHouse 预热");
    }
    if let Some(e) = &status.clickhouse_error {
        tracing::warn!(error = %e, "从 ClickHouse 预热会话失败：这段时间的会话会被切开");
    }
    tracing::info!(
        source = status.source,
        sessions = status.sessions,
        future_dropped = status.future_dropped,
        elapsed_ms = status.elapsed_ms,
        "会话预热完成"
    );
    status
}

/// 关停时写快照。调用前必须已经停止接收事件。
pub async fn save(pipeline: Arc<Pipeline>, data_dir: &Path) -> anyhow::Result<()> {
    let path = snapshot_path(data_dir);
    tokio::task::spawn_blocking(move || {
        let records = pipeline.assigner().export();
        snapshot::write_file(&path, &records, now_ms())?;
        tracing::info!(sessions = records.len(), path = %path.display(), "会话快照已写入");
        Ok::<_, anyhow::Error>(())
    })
    .await??;
    Ok(())
}
