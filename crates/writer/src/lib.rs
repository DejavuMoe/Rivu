//! BatchWriter（Lite）：有界 channel 缓冲事件，按「1 s / 2 万行 / 8 MB 先到先写」组批写入 ClickHouse，
//! 失败带稳定的去重 token 退避重试；同时在进程内聚合接入计数。设计见 `system-design.md` §4.7、§5.4。
//!
//! - 请求热路径只做 [`WriterHandle::try_send`]（`try_send`，满了立即返回错误，由调用方回 `503`）和
//!   [`WriterHandle::count`]（一次加锁的累加）；
//! - 写入任务单独运行；一批正在重试时不再从 channel 取数，channel 逐渐变满，背压自然传到 `503`；
//! - token = `{node_id}:{batch_seq}`，`node_id` 每次进程启动随机生成，所以重启后 token 不会和旧进程的撞车；
//!   同一批重试时 token 与内容都不变；
//! - 计数 token = `{node_id}:c{seq}`。`ingest_counters` 的去重窗口由 ClickHouse 初始迁移
//!   打开，「写入成功但响应丢失」后的重试不会让计数重复；
//! - 批次的内容与顺序由 channel 的接收顺序决定。一批横跨两个月的事件会被拆成两个 block，
//!   ClickHouse 给每个 block 派生确定的子 token，重试仍然幂等。

pub mod counters;

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use clickhouse::Client;
use rivu_common::config::BufferConfig;
use rivu_storage_ch::rows::{
    EventRow, IngestCounterRow, insert_events, insert_ingest_counters, supports_deduplicate_insert,
};
use thiserror::Error;
use tokio::sync::{OnceCell, mpsc, watch};
use tokio::task::JoinHandle;
use tokio::time::{Instant, MissedTickBehavior};

pub use counters::CounterBuffer;

/// 批量写入的目标。生产实现是 [`ChSink`]，测试可以换成假的。
pub trait BatchSink: Send + Sync + 'static {
    fn insert_events(
        &self,
        rows: &[EventRow],
        token: &str,
    ) -> impl Future<Output = Result<()>> + Send;

    fn insert_counters(
        &self,
        rows: &[IngestCounterRow],
        token: &str,
    ) -> impl Future<Output = Result<()>> + Send;
}

/// 写入 ClickHouse 的 [`BatchSink`]。客户端应当用 `rivu_writer` 账号构造。
pub struct ChSink {
    client: Client,
    /// 服务端是否支持 `deduplicate_insert`（26.2 起）。第一次写入时探测一次；探测失败不缓存，随写入一起重试。
    has_deduplicate_insert: OnceCell<bool>,
}

impl ChSink {
    pub fn new(client: Client) -> Self {
        ChSink {
            client,
            has_deduplicate_insert: OnceCell::new(),
        }
    }

    async fn has_deduplicate_insert(&self) -> Result<bool> {
        self.has_deduplicate_insert
            .get_or_try_init(|| supports_deduplicate_insert(&self.client))
            .await
            .copied()
    }
}

impl BatchSink for ChSink {
    async fn insert_events(&self, rows: &[EventRow], token: &str) -> Result<()> {
        let dedup = self.has_deduplicate_insert().await?;
        insert_events(&self.client, rows, token, dedup).await
    }

    async fn insert_counters(&self, rows: &[IngestCounterRow], token: &str) -> Result<()> {
        let dedup = self.has_deduplicate_insert().await?;
        insert_ingest_counters(&self.client, rows, token, dedup).await
    }
}

/// 失败重试：指数退避，最长持续一段时间（`system-design.md` §4.7：0.5 s 起，翻倍到 30 s，最长 5 分钟）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RetryPolicy {
    pub initial_backoff: Duration,
    pub max_backoff: Duration,
    /// 从第一次失败起，超过这个时长就放弃这一批。
    pub total: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        RetryPolicy {
            initial_backoff: Duration::from_millis(500),
            max_backoff: Duration::from_secs(30),
            total: Duration::from_secs(300),
        }
    }
}

/// 写入器的参数。
#[derive(Clone, Debug)]
pub struct WriterOptions {
    pub buffer: BufferConfig,
    pub retry: RetryPolicy,
    /// 计数刷出间隔，默认 10 s（与桶宽一致）。
    pub counter_flush_interval: Duration,
    /// 关停时最后一次 flush 的重试预算（上限仍是 `retry.total`）。
    pub shutdown_budget: Duration,
    /// 去重 token 的前缀。每次进程启动随机生成。
    pub node_id: String,
}

impl WriterOptions {
    pub fn new(buffer: BufferConfig) -> Self {
        WriterOptions {
            buffer,
            retry: RetryPolicy::default(),
            counter_flush_interval: Duration::from_secs(counters::BUCKET_SECS as u64),
            shutdown_budget: Duration::from_secs(30),
            node_id: random_node_id(),
        }
    }
}

/// 随机的 node_id：16 个十六进制字符。
pub fn random_node_id() -> String {
    format!("{:016x}", rand::random::<u64>())
}

/// `try_send` 失败的原因。两种情况调用方都应回 `503`。
#[derive(Debug, Error, PartialEq, Eq)]
pub enum TrySendError {
    #[error("写缓冲已满")]
    Full,
    #[error("写缓冲已关闭（正在关停）")]
    Closed,
}

/// 写入器的句柄，可以随意克隆，在请求线程里使用。
#[derive(Clone)]
pub struct WriterHandle {
    tx: mpsc::Sender<EventRow>,
    counters: Arc<CounterBuffer>,
}

impl WriterHandle {
    /// 把一行放进缓冲。满了立即返回 [`TrySendError::Full`]，不等待。
    pub fn try_send(&self, row: EventRow) -> Result<(), TrySendError> {
        self.tx.try_send(row).map_err(|e| match e {
            mpsc::error::TrySendError::Full(_) => TrySendError::Full,
            mpsc::error::TrySendError::Closed(_) => TrySendError::Closed,
        })
    }

    /// 接入计数加 1，计入当前墙钟所在的 10 秒桶（计数描述的是「接收」这件事发生的时刻）。
    pub fn count(&self, site_id: u64, metric: &'static str, script_version: u32) {
        self.counters
            .add(site_id, metric, script_version, counters::now_secs());
    }

    /// 同 [`WriterHandle::count`]，但时刻由调用方给出（Unix 秒）。
    pub fn count_at(&self, site_id: u64, metric: &'static str, script_version: u32, at_secs: i64) {
        self.counters.add(site_id, metric, script_version, at_secs);
    }

    /// 缓冲里还没取走的行数。
    pub fn queued(&self) -> usize {
        self.tx.max_capacity() - self.tx.capacity()
    }

    /// 不带写入任务的句柄和接收端：测试用（例如检查管线放进了什么，或模拟「缓冲已满」）。
    pub fn detached(capacity: usize) -> (WriterHandle, mpsc::Receiver<EventRow>) {
        let (tx, rx) = mpsc::channel(capacity);
        (
            WriterHandle {
                tx,
                counters: Arc::new(CounterBuffer::default()),
            },
            rx,
        )
    }

    /// 这个句柄背后的计数器（测试用）。
    pub fn counters(&self) -> &Arc<CounterBuffer> {
        &self.counters
    }
}

/// 等到停止信号（发送端被丢弃也算停止）。`watch::Ref` 不是 `Send`，所以不能把它带过 await。
async fn stopped(rx: &mut watch::Receiver<bool>) {
    let _ = rx.wait_for(|s| *s).await;
}

/// 带退避的重试。成功返回尝试次数；预算用完仍失败返回最后一次的错误。
async fn with_retry<F, Fut>(policy: &RetryPolicy, what: &str, mut op: F) -> Result<u32>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<()>>,
{
    let started = Instant::now();
    let mut delay = policy.initial_backoff;
    let mut attempt = 0u32;
    loop {
        attempt += 1;
        match op().await {
            Ok(()) => return Ok(attempt),
            Err(e) => {
                if started.elapsed() + delay > policy.total {
                    return Err(e.context(format!("{what} 重试 {attempt} 次后放弃")));
                }
                metrics::counter!("rivu_writer_retries_total", "what" => what.to_owned())
                    .increment(1);
                tracing::warn!(
                    error = %format!("{e:#}"),
                    attempt,
                    retry_in_ms = delay.as_millis() as u64,
                    "{what} 失败，稍后重试"
                );
                tokio::time::sleep(delay).await;
                delay = (delay * 2).min(policy.max_backoff);
            }
        }
    }
}

/// 写一批 events。成功或最终放弃后，`batch` 都被清空。
async fn flush_events<S: BatchSink>(
    sink: &S,
    retry: &RetryPolicy,
    token: &str,
    batch: &mut Vec<EventRow>,
) {
    if batch.is_empty() {
        return;
    }
    let rows = std::mem::take(batch);
    let n = rows.len() as u64;
    let started = Instant::now();
    let result = with_retry(retry, "写入 events", || sink.insert_events(&rows, token)).await;
    match result {
        Ok(_) => {
            metrics::counter!("rivu_writer_batches_total", "result" => "ok").increment(1);
            metrics::counter!("rivu_writer_rows_total", "result" => "ok").increment(n);
            metrics::histogram!("rivu_writer_flush_seconds")
                .record(started.elapsed().as_secs_f64());
        }
        Err(e) => {
            metrics::counter!("rivu_writer_batches_total", "result" => "dropped").increment(1);
            metrics::counter!("rivu_writer_rows_total", "result" => "dropped").increment(n);
            tracing::error!(error = %format!("{e:#}"), rows = n, token, "events 批次最终写入失败，已丢弃");
        }
    }
}

async fn flush_counters<S: BatchSink>(
    sink: &S,
    retry: &RetryPolicy,
    token: &str,
    rows: Vec<IngestCounterRow>,
) {
    if rows.is_empty() {
        return;
    }
    let n = rows.len() as u64;
    match with_retry(retry, "写入 ingest_counters", || {
        sink.insert_counters(&rows, token)
    })
    .await
    {
        Ok(_) => {
            metrics::counter!("rivu_writer_counter_rows_total", "result" => "ok").increment(n);
        }
        Err(e) => {
            metrics::counter!("rivu_writer_counter_rows_total", "result" => "dropped").increment(n);
            tracing::error!(error = %format!("{e:#}"), rows = n, token, "ingest_counters 批次最终写入失败，已丢弃");
        }
    }
}

// 关停前的最后一次 flush 之后 `bytes` 不会再被读，宏里的清零对它是多余的。
#[allow(unused_assignments)]
async fn run_events<S: BatchSink>(
    sink: Arc<S>,
    opts: WriterOptions,
    mut rx: mpsc::Receiver<EventRow>,
    mut stop: watch::Receiver<bool>,
) {
    let cfg = opts.buffer;
    let mut batch: Vec<EventRow> = Vec::with_capacity(cfg.max_rows.min(4096));
    let mut bytes = 0usize;
    let mut seq = 0u64;
    // 第一次 tick 在一个完整间隔之后；每次 flush 后重新计时，所以间隔是「距上一次 flush」。
    let mut ticker =
        tokio::time::interval_at(Instant::now() + cfg.flush_interval, cfg.flush_interval);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);

    // 取 token 并写一批。
    macro_rules! flush {
        ($retry:expr) => {{
            seq += 1;
            let token = format!("{}:{}", opts.node_id, seq);
            flush_events(&*sink, $retry, &token, &mut batch).await;
            bytes = 0;
            ticker.reset();
            metrics::gauge!("rivu_writer_queue_depth").set(rx.len() as f64);
        }};
    }

    loop {
        tokio::select! {
            biased;
            () = stopped(&mut stop) => break,
            maybe = rx.recv() => {
                let Some(row) = maybe else { break };
                bytes += row.approx_bytes();
                batch.push(row);
                if batch.len() >= cfg.max_rows || bytes >= cfg.max_bytes {
                    flush!(&opts.retry);
                }
            }
            _ = ticker.tick() => {
                if !batch.is_empty() {
                    flush!(&opts.retry);
                }
            }
        }
    }

    // 关停：不再接收，排空 channel，按批写完。
    rx.close();
    let budget = RetryPolicy {
        total: opts.shutdown_budget.min(opts.retry.total),
        ..opts.retry
    };
    while let Some(row) = rx.recv().await {
        bytes += row.approx_bytes();
        batch.push(row);
        if batch.len() >= cfg.max_rows || bytes >= cfg.max_bytes {
            flush!(&budget);
        }
    }
    if !batch.is_empty() {
        flush!(&budget);
    }
}

async fn run_counters<S: BatchSink>(
    sink: Arc<S>,
    opts: WriterOptions,
    buf: Arc<CounterBuffer>,
    mut stop: watch::Receiver<bool>,
) {
    let mut seq = 0u64;
    let mut ticker = tokio::time::interval(opts.counter_flush_interval);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
    ticker.tick().await; // 第一次 tick 立即返回，跳过
    loop {
        tokio::select! {
            biased;
            () = stopped(&mut stop) => break,
            _ = ticker.tick() => {
                let rows = buf.take_ended(counters::now_secs());
                if !rows.is_empty() {
                    seq += 1;
                    let token = format!("{}:c{}", opts.node_id, seq);
                    flush_counters(&*sink, &opts.retry, &token, rows).await;
                }
            }
        }
    }
    // 关停：全部刷出，包括还没结束的桶。
    let rows = buf.take_all();
    if !rows.is_empty() {
        seq += 1;
        let token = format!("{}:c{}", opts.node_id, seq);
        let budget = RetryPolicy {
            total: opts.shutdown_budget.min(opts.retry.total),
            ..opts.retry
        };
        flush_counters(&*sink, &budget, &token, rows).await;
    }
}

/// 注册写入器的 Prometheus 指标说明。
pub fn describe_metrics() {
    metrics::describe_counter!(
        "rivu_writer_batches_total",
        "events 批次数，result = ok | dropped（重试到预算用完后丢弃）"
    );
    metrics::describe_counter!(
        "rivu_writer_rows_total",
        "events 行数，result = ok | dropped"
    );
    metrics::describe_counter!("rivu_writer_retries_total", "写入失败后的重试次数");
    metrics::describe_counter!(
        "rivu_writer_counter_rows_total",
        "ingest_counters 行数，result = ok | dropped"
    );
    metrics::describe_histogram!(
        "rivu_writer_flush_seconds",
        "一批 events 的写入耗时（含重试）"
    );
    metrics::describe_gauge!("rivu_writer_queue_depth", "写缓冲里还没取走的行数");
}

/// BatchWriter：持有后台写入任务。
pub struct BatchWriter {
    handle: WriterHandle,
    stop_events: watch::Sender<bool>,
    stop_counters: watch::Sender<bool>,
    events_task: JoinHandle<()>,
    counters_task: JoinHandle<()>,
}

impl BatchWriter {
    /// 启动写入任务（需要在 tokio 运行时内调用）。
    pub fn spawn<S: BatchSink>(sink: Arc<S>, opts: WriterOptions) -> Self {
        let (tx, rx) = mpsc::channel(opts.buffer.channel_capacity);
        let counters = Arc::new(CounterBuffer::default());
        let (stop_events, stop_events_rx) = watch::channel(false);
        let (stop_counters, stop_counters_rx) = watch::channel(false);
        let events_task = tokio::spawn(run_events(
            Arc::clone(&sink),
            opts.clone(),
            rx,
            stop_events_rx,
        ));
        let counters_task = tokio::spawn(run_counters(
            sink,
            opts,
            Arc::clone(&counters),
            stop_counters_rx,
        ));
        BatchWriter {
            handle: WriterHandle { tx, counters },
            stop_events,
            stop_counters,
            events_task,
            counters_task,
        }
    }

    pub fn handle(&self) -> WriterHandle {
        self.handle.clone()
    }

    /// 关停：先排空并写完 events，再把全部计数刷出（包括还没结束的桶）。
    /// 调用前应当已经停止接收新请求。
    pub async fn shutdown(self) -> Result<()> {
        let _ = self.stop_events.send(true);
        self.events_task.await.context("events 写入任务异常退出")?;
        let _ = self.stop_counters.send(true);
        self.counters_task.await.context("计数写入任务异常退出")?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    use parking_lot::Mutex;

    use super::*;

    /// 假的写入目标：记录每次尝试的 token 和成功写入的批次，可以注入失败或卡住。
    #[derive(Default)]
    struct FakeSink {
        fail_first: AtomicUsize,
        always_fail: AtomicBool,
        blocked: AtomicBool,
        attempts: Mutex<Vec<String>>,
        batches: Mutex<Vec<(String, Vec<EventRow>)>>,
        counter_batches: Mutex<Vec<(String, Vec<IngestCounterRow>)>>,
    }

    impl FakeSink {
        async fn gate(&self, token: &str) -> Result<()> {
            self.attempts.lock().push(token.to_owned());
            while self.blocked.load(Ordering::SeqCst) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            if self.always_fail.load(Ordering::SeqCst) {
                anyhow::bail!("注入的失败");
            }
            let left = self.fail_first.load(Ordering::SeqCst);
            if left > 0 {
                self.fail_first.store(left - 1, Ordering::SeqCst);
                anyhow::bail!("注入的失败");
            }
            Ok(())
        }

        fn batch_sizes(&self) -> Vec<usize> {
            self.batches.lock().iter().map(|(_, r)| r.len()).collect()
        }

        fn rows_written(&self) -> usize {
            self.batch_sizes().iter().sum()
        }
    }

    impl BatchSink for FakeSink {
        async fn insert_events(&self, rows: &[EventRow], token: &str) -> Result<()> {
            self.gate(token).await?;
            self.batches.lock().push((token.to_owned(), rows.to_vec()));
            Ok(())
        }

        async fn insert_counters(&self, rows: &[IngestCounterRow], token: &str) -> Result<()> {
            self.gate(token).await?;
            self.counter_batches
                .lock()
                .push((token.to_owned(), rows.to_vec()));
            Ok(())
        }
    }

    fn opts(max_rows: usize, interval: Duration, capacity: usize) -> WriterOptions {
        WriterOptions {
            buffer: BufferConfig {
                flush_interval: interval,
                max_rows,
                max_bytes: 8 << 20,
                channel_capacity: capacity,
            },
            retry: RetryPolicy::default(),
            counter_flush_interval: Duration::from_secs(10),
            shutdown_budget: Duration::from_secs(30),
            node_id: "node".into(),
        }
    }

    fn row(i: u64) -> EventRow {
        EventRow {
            site_id: 1,
            visitor_id: i,
            name: "pageview".into(),
            pathname: format!("/{i}"),
            ..EventRow::default()
        }
    }

    /// 虚拟时钟下等条件成立（最多 60 s 虚拟时间）。
    async fn wait_until(what: &str, cond: impl Fn() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(60);
        while !cond() {
            assert!(Instant::now() < deadline, "等待超时：{what}");
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    #[tokio::test(start_paused = true)]
    async fn row_count_triggers_a_flush_before_the_timer() {
        let sink = Arc::new(FakeSink::default());
        let w = BatchWriter::spawn(Arc::clone(&sink), opts(3, Duration::from_secs(3600), 100));
        let h = w.handle();
        for i in 0..7 {
            h.try_send(row(i)).unwrap();
        }
        wait_until("两批各 3 行", || sink.batch_sizes() == vec![3, 3]).await;
        // 第 7 行留在缓冲里，直到关停
        assert_eq!(sink.rows_written(), 6);
        w.shutdown().await.unwrap();
        assert_eq!(sink.batch_sizes(), vec![3, 3, 1]);
        let tokens: Vec<String> = sink.batches.lock().iter().map(|(t, _)| t.clone()).collect();
        assert_eq!(tokens, ["node:1", "node:2", "node:3"]);
    }

    #[tokio::test(start_paused = true)]
    async fn timer_flushes_a_partial_batch() {
        let sink = Arc::new(FakeSink::default());
        let w = BatchWriter::spawn(Arc::clone(&sink), opts(100, Duration::from_secs(1), 100));
        let h = w.handle();
        h.try_send(row(1)).unwrap();
        h.try_send(row(2)).unwrap();
        assert_eq!(sink.rows_written(), 0);
        tokio::time::sleep(Duration::from_millis(1500)).await;
        assert_eq!(sink.batch_sizes(), vec![2]);
        w.shutdown().await.unwrap();
        assert_eq!(sink.batch_sizes(), vec![2], "关停时没有多余的空批");
    }

    #[tokio::test(start_paused = true)]
    async fn byte_budget_triggers_a_flush() {
        let sink = Arc::new(FakeSink::default());
        let mut o = opts(1000, Duration::from_secs(3600), 100);
        o.buffer.max_bytes = 5_000;
        let w = BatchWriter::spawn(Arc::clone(&sink), o);
        let h = w.handle();
        for i in 0..6 {
            let mut r = row(i);
            r.pathname = "x".repeat(1_000);
            h.try_send(r).unwrap();
        }
        // 每行约 1.2 KB：5 行时累计超过 5000，触发第一批
        wait_until("第一批按字节触发", || {
            !sink.batch_sizes().is_empty()
        })
        .await;
        let first = sink.batch_sizes()[0];
        assert!((3..=5).contains(&first), "第一批 {first} 行");
        w.shutdown().await.unwrap();
        assert_eq!(sink.rows_written(), 6);
    }

    #[tokio::test(start_paused = true)]
    async fn retries_reuse_the_same_token_and_rows() {
        let sink = Arc::new(FakeSink::default());
        sink.fail_first.store(3, Ordering::SeqCst);
        let w = BatchWriter::spawn(Arc::clone(&sink), opts(2, Duration::from_secs(3600), 100));
        let h = w.handle();
        h.try_send(row(1)).unwrap();
        h.try_send(row(2)).unwrap();
        wait_until("重试后写入成功", || sink.batch_sizes() == vec![2]).await;
        // 失败 3 次 + 成功 1 次，token 全部相同
        assert_eq!(*sink.attempts.lock(), ["node:1"; 4]);
        {
            let batches = sink.batches.lock();
            let rows = &batches[0].1;
            assert_eq!(rows[0].visitor_id, 1);
            assert_eq!(rows[1].visitor_id, 2);
        }
        w.shutdown().await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn backoff_doubles_from_half_a_second_up_to_the_cap() {
        let sink = Arc::new(FakeSink::default());
        sink.always_fail.store(true, Ordering::SeqCst);
        let policy = RetryPolicy::default();
        let started = Instant::now();
        let times = Mutex::new(Vec::new());
        let r = with_retry(&policy, "测试", || {
            times.lock().push(started.elapsed());
            sink.gate("t")
        })
        .await;
        assert!(r.is_err());
        let times = times.lock();
        let gaps: Vec<u64> = times
            .windows(2)
            .map(|w| (w[1] - w[0]).as_millis() as u64)
            .collect();
        assert_eq!(
            &gaps[..8],
            &[500, 1000, 2000, 4000, 8000, 16000, 30000, 30000]
        );
        // 总时长不超过 5 分钟
        assert!(times.last().unwrap().as_secs() <= 300);
    }

    #[tokio::test(start_paused = true)]
    async fn a_batch_that_never_succeeds_is_dropped_and_the_next_one_still_goes_out() {
        let sink = Arc::new(FakeSink::default());
        sink.always_fail.store(true, Ordering::SeqCst);
        let mut o = opts(1, Duration::from_secs(3600), 100);
        o.retry.total = Duration::from_secs(5);
        let w = BatchWriter::spawn(Arc::clone(&sink), o);
        let h = w.handle();
        h.try_send(row(1)).unwrap();
        // 5 s 预算用完后放弃：尝试过，但没有成功写入
        wait_until("第一批被放弃", || sink.attempts.lock().len() >= 4).await;
        tokio::time::sleep(Duration::from_secs(10)).await;
        assert_eq!(sink.rows_written(), 0);
        let tried = sink.attempts.lock().len();
        tokio::time::sleep(Duration::from_secs(60)).await;
        assert_eq!(sink.attempts.lock().len(), tried, "放弃后不再重试");

        sink.always_fail.store(false, Ordering::SeqCst);
        h.try_send(row(2)).unwrap();
        wait_until("第二批写入", || sink.rows_written() == 1).await;
        {
            let b = sink.batches.lock();
            assert_eq!(b[0].0, "node:2", "被放弃的批次也占用一个序号");
            assert_eq!(b[0].1[0].visitor_id, 2);
        }
        w.shutdown().await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn full_buffer_rejects_immediately() {
        let sink = Arc::new(FakeSink::default());
        sink.blocked.store(true, Ordering::SeqCst);
        let w = BatchWriter::spawn(Arc::clone(&sink), opts(1, Duration::from_secs(3600), 2));
        let h = w.handle();
        h.try_send(row(0)).unwrap();
        // 等任务取走第 0 行并卡在写入上
        wait_until("任务开始写入", || !sink.attempts.lock().is_empty()).await;
        h.try_send(row(1)).unwrap();
        h.try_send(row(2)).unwrap();
        assert_eq!(h.try_send(row(3)), Err(TrySendError::Full));
        assert_eq!(h.queued(), 2);
        sink.blocked.store(false, Ordering::SeqCst);
        wait_until("全部写完", || sink.rows_written() == 3).await;
        h.try_send(row(4)).unwrap();
        w.shutdown().await.unwrap();
        assert_eq!(sink.rows_written(), 4);
    }

    #[tokio::test(start_paused = true)]
    async fn shutdown_drains_everything_and_closes_the_buffer() {
        let sink = Arc::new(FakeSink::default());
        let w = BatchWriter::spawn(
            Arc::clone(&sink),
            opts(1000, Duration::from_secs(3600), 1000),
        );
        let h = w.handle();
        for i in 0..250 {
            h.try_send(row(i)).unwrap();
        }
        // 还没有任何 flush 条件满足
        tokio::time::sleep(Duration::from_millis(10)).await;
        assert_eq!(sink.rows_written(), 0);
        // 一个刚进入、还没结束的计数桶也要在关停时刷出
        h.count(7, "accepted", 3);
        w.shutdown().await.unwrap();
        assert_eq!(sink.rows_written(), 250);
        assert_eq!(h.try_send(row(999)), Err(TrySendError::Closed));
        let c = sink.counter_batches.lock();
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].0, "node:c1");
        assert_eq!(c[0].1.len(), 1);
        assert_eq!(c[0].1[0].value, 1);
    }

    #[tokio::test(start_paused = true)]
    async fn only_ended_counter_buckets_are_flushed_periodically() {
        let sink = Arc::new(FakeSink::default());
        let w = BatchWriter::spawn(Arc::clone(&sink), opts(10, Duration::from_secs(3600), 10));
        let h = w.handle();
        // 一个早已结束的桶（1970 年）和一个「现在」的桶；虚拟时钟不影响墙钟
        h.count_at(1, "accepted", 0, 100);
        h.count_at(1, "accepted", 0, 101);
        h.count_at(1, "dropped_bot", 0, 100);
        tokio::time::sleep(Duration::from_secs(11)).await;
        {
            let c = sink.counter_batches.lock();
            assert_eq!(c.len(), 1, "10 s 后刷出已结束的桶");
            assert_eq!(c[0].0, "node:c1");
            let total: u64 = c[0].1.iter().map(|r| r.value).sum();
            assert_eq!(total, 3);
            assert!(c[0].1.iter().all(|r| r.minute.unix_timestamp() == 60));
        }
        // 此后没有新计数：不产生空批
        tokio::time::sleep(Duration::from_secs(30)).await;
        assert_eq!(sink.counter_batches.lock().len(), 1);
        w.shutdown().await.unwrap();
        assert_eq!(sink.counter_batches.lock().len(), 1);
    }
}
