//! GeoIP 库的下载与刷新（`system-design.md` §4.4「生命周期」「HTTP 约束」）。
//!
//! 一次刷新：条件 GET（带 `If-None-Match` / `If-Modified-Since`，取值来自 meta）→ 流式写入同目录临时文件
//! （超过 1 GiB 中止）→ 解压（gzip 按魔数判断；MaxMind 的 `tar.gz` 取其中的 `.mmdb`）→ 校验 mmdb →
//! fsync → `rename` 原子覆盖 → 原子写 meta。任何一步失败都删除临时文件，上一份有效副本原样保留。

use std::fs::File;
use std::io::{self, BufWriter, Cursor, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result, anyhow, bail, ensure};
use flate2::read::MultiGzDecoder;
use jiff::{Span, Timestamp};
use reqwest::header::{ETAG, IF_MODIFIED_SINCE, IF_NONE_MATCH, LAST_MODIFIED};
use rivu_common::config::{DbIpEdition, GeoConfig, GeoSource, redact_url};
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;

use crate::attribution::{Attribution, StoredAttribution};
use crate::db;
use crate::meta::{self, Meta};

/// 单次下载（压缩前与解压后各自）的上限：1 GiB。
pub const MAX_DOWNLOAD_BYTES: u64 = 1 << 30;
pub const DBIP_BASE_URL: &str = "https://download.db-ip.com/free";
pub const MAXMIND_BASE_URL: &str = "https://download.maxmind.com/geoip/databases";
/// 最多跟随的重定向次数。
pub const MAX_REDIRECTS: usize = 5;

/// HTTP 客户端的参数。默认：连接超时 10 s，整体超时 10 min（含读取响应体），读系统代理环境变量。
#[derive(Debug, Clone, Copy)]
pub struct HttpOptions {
    pub connect_timeout: Duration,
    pub timeout: Duration,
    /// 不使用任何代理（测试用：本地测试服务器不能被系统代理截走）。
    pub no_proxy: bool,
}

impl Default for HttpOptions {
    fn default() -> Self {
        HttpOptions {
            connect_timeout: Duration::from_secs(10),
            timeout: Duration::from_secs(600),
            no_proxy: false,
        }
    }
}

/// 下载用的 HTTP 客户端：rustls（ring + webpki-roots，与 clickhouse 的选择一致），只说 HTTP/1.1，
/// 不自动解压（`.mmdb.gz` 要原样落盘再解压），最多 5 次重定向。
#[derive(Clone)]
pub struct HttpClient {
    client: reqwest::Client,
}

impl HttpClient {
    pub fn new() -> Result<Self> {
        Self::with_options(HttpOptions::default())
    }

    pub fn with_options(opts: HttpOptions) -> Result<Self> {
        let mut roots = rustls::RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let mut tls = rustls::ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .context("初始化 TLS 配置失败")?
            .with_root_certificates(roots)
            .with_no_client_auth();
        tls.alpn_protocols = vec![b"http/1.1".to_vec()];

        let mut b = reqwest::Client::builder()
            .tls_backend_preconfigured(tls)
            .redirect(reqwest::redirect::Policy::limited(MAX_REDIRECTS))
            .connect_timeout(opts.connect_timeout)
            .timeout(opts.timeout)
            .user_agent(concat!("rivu-geo/", env!("CARGO_PKG_VERSION")));
        if opts.no_proxy {
            b = b.no_proxy();
        }
        Ok(HttpClient {
            client: b.build().context("创建 HTTP 客户端失败")?,
        })
    }

    /// GET 一个不大的文件到内存：只接受 200，超过 `max_bytes` 中止（数据中心网段等文本清单用）。
    pub async fn get_bytes(&self, url: &str, max_bytes: u64) -> Result<Vec<u8>> {
        let mut resp = self.client.get(url).send().await.map_err(|e| {
            anyhow!(
                "请求 {} 失败：{}",
                redact_url(url),
                describe(&e.without_url())
            )
        })?;
        let status = resp.status().as_u16();
        ensure!(
            status == 200,
            "服务器返回 HTTP {status}（{}）",
            redact_url(url)
        );
        if let Some(len) = resp.content_length() {
            ensure!(
                len <= max_bytes,
                "Content-Length {len} 超过上限 {max_bytes} 字节（{}）",
                redact_url(url)
            );
        }
        let mut out = Vec::new();
        while let Some(chunk) = resp
            .chunk()
            .await
            .map_err(|e| anyhow!("读取响应失败：{}", describe(&e.without_url())))?
        {
            ensure!(
                (out.len() + chunk.len()) as u64 <= max_bytes,
                "下载内容超过上限 {max_bytes} 字节，中止（{}）",
                redact_url(url)
            );
            out.extend_from_slice(&chunk);
        }
        Ok(out)
    }
}

/// 刷新的可调参数；默认值就是生产值，测试用来指向本地服务器、缩小上限、固定「当前月份」。
#[derive(Debug, Clone)]
pub struct RefreshOptions {
    pub max_bytes: u64,
    pub dbip_base_url: String,
    pub maxmind_base_url: String,
    /// 当前时间；`None` 取系统时钟。只影响 DB-IP 的月份。
    pub now: Option<Timestamp>,
}

impl Default for RefreshOptions {
    fn default() -> Self {
        RefreshOptions {
            max_bytes: MAX_DOWNLOAD_BYTES,
            dbip_base_url: DBIP_BASE_URL.into(),
            maxmind_base_url: MAXMIND_BASE_URL.into(),
            now: None,
        }
    }
}

/// 一次刷新的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefreshOutcome {
    /// 下载、校验并替换了库。
    Updated {
        sha256: String,
        build_epoch: u64,
        /// 新库（解压后）的字节数。
        bytes: u64,
    },
    /// 库没有变化：服务器回了 304，或者（没有校验头时）整文件的 SHA-256 与 meta 相同。
    NotModified { by_hash: bool },
}

impl RefreshOutcome {
    /// `rivu_geoip_refresh_total{result}` 的取值。
    pub fn result_label(&self) -> &'static str {
        match self {
            RefreshOutcome::Updated { .. } => "updated",
            RefreshOutcome::NotModified { .. } => "not_modified",
        }
    }
}

/// 本地副本相对当前配置的状态，决定启动时要不要立即下载。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalCopy {
    /// 没有库文件：立即下载。
    Missing,
    /// 手工放置的库（没有 meta）且配置的是默认来源：直接使用，**不自动刷新**，避免定期下载把它覆盖掉。
    Manual,
    /// 有库文件，但来源标识与当前配置不一致，或没有 meta 而配置了显式来源（URL / MaxMind key）：
    /// 视为没有本地副本，立即下载（旧文件在新库校验通过前继续使用）。
    Foreign,
    /// meta 与当前配置一致：按周期做条件 GET。
    Current,
}

/// 判断本地副本的状态。
pub fn local_copy(cfg: &GeoConfig) -> LocalCopy {
    if !cfg.db_path.is_file() {
        return LocalCopy::Missing;
    }
    match meta::read(&cfg.db_path) {
        Some(m) if m.source_id == cfg.source.id() => LocalCopy::Current,
        Some(_) => LocalCopy::Foreign,
        None if cfg.source.is_explicit() => LocalCopy::Foreign,
        None => LocalCopy::Manual,
    }
}

/// 下载目标：候选 URL（DB-IP 先当月、404 回退上月）与认证。
struct Target {
    urls: Vec<String>,
    basic_auth: Option<(String, String)>,
    attribution: Option<Attribution>,
}

fn target(cfg: &GeoConfig, opts: &RefreshOptions) -> Result<Target> {
    Ok(match &cfg.source {
        GeoSource::Custom { url } => Target {
            urls: vec![url.expose().to_string()],
            basic_auth: None,
            attribution: Attribution::custom(
                cfg.attribution_text.as_deref(),
                cfg.attribution_url.as_deref(),
            ),
        },
        GeoSource::MaxMind {
            account_id,
            license_key,
            edition,
        } => Target {
            // 现行接口：https://download.maxmind.com/geoip/databases/<edition>/download?suffix=tar.gz，
            // HTTP Basic（用户名 Account ID，密码 License Key），返回 tar.gz，会重定向到对象存储。
            urls: vec![format!(
                "{}/{edition}/download?suffix=tar.gz",
                opts.maxmind_base_url.trim_end_matches('/')
            )],
            basic_auth: Some((account_id.clone(), license_key.expose().to_string())),
            attribution: Some(Attribution::maxmind_geolite2()),
        },
        GeoSource::DbIp { edition } => {
            let now = opts.now.unwrap_or_else(Timestamp::now);
            Target {
                urls: dbip_urls(&opts.dbip_base_url, *edition, now),
                basic_auth: None,
                attribution: Some(Attribution::dbip_lite()),
            }
        }
    })
}

/// DB-IP 的两个候选 URL：当月（UTC）与上月。每月 1 日发布，所以月初当月文件可能还没有。
pub fn dbip_urls(base: &str, edition: DbIpEdition, now: Timestamp) -> Vec<String> {
    let this_month = now
        .to_zoned(jiff::tz::TimeZone::UTC)
        .date()
        .first_of_month();
    let prev_month = this_month.saturating_sub(Span::new().months(1));
    [this_month, prev_month]
        .into_iter()
        .map(|d| {
            format!(
                "{}/dbip-{}-lite-{:04}-{:02}.mmdb.gz",
                base.trim_end_matches('/'),
                edition.as_str(),
                d.year(),
                d.month()
            )
        })
        .collect()
}

/// 刷新一次：条件 GET、解压、校验、fsync、原子 rename、写 meta。
///
/// 注意：这个函数本身不判断本地文件是不是手工放置的（[`LocalCopy::Manual`]），调用方（`geoip_refresh`
/// 任务）负责跳过。
pub async fn refresh(cfg: &GeoConfig, http: &HttpClient) -> Result<RefreshOutcome> {
    refresh_with(cfg, http, &RefreshOptions::default()).await
}

/// [`refresh`] 的可调版本。
pub async fn refresh_with(
    cfg: &GeoConfig,
    http: &HttpClient,
    opts: &RefreshOptions,
) -> Result<RefreshOutcome> {
    let db_path = cfg.db_path.clone();
    let dir = db_path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
    tokio::fs::create_dir_all(&dir)
        .await
        .with_context(|| format!("创建目录 {} 失败", dir.display()))?;
    remove_stale_temps(&db_path);

    let source_id = cfg.source.id();
    // 只有 meta 与当前来源一致、库文件也在时，才带校验头；否则当作没有本地副本。
    let prev: Option<Meta> = match local_copy(cfg) {
        LocalCopy::Current => meta::read(&db_path),
        _ => None,
    };
    let tgt = target(cfg, opts)?;
    let now = Timestamp::now().to_string();

    let mut chosen = None;
    let n = tgt.urls.len();
    for (i, url) in tgt.urls.iter().enumerate() {
        let mut req = http.client.get(url);
        if let Some((user, pass)) = &tgt.basic_auth {
            req = req.basic_auth(user, Some(pass));
        }
        if let Some(p) = &prev {
            if let Some(etag) = &p.etag {
                req = req.header(IF_NONE_MATCH, etag);
            }
            if let Some(lm) = &p.last_modified {
                req = req.header(IF_MODIFIED_SINCE, lm);
            }
        }
        let resp = req.send().await.map_err(|e| {
            anyhow!(
                "请求 {} 失败：{}",
                redact_url(url),
                describe(&e.without_url())
            )
        })?;
        let status = resp.status();
        match status.as_u16() {
            200 => {
                chosen = Some((url.clone(), resp));
                break;
            }
            304 => {
                let Some(p) = prev
                    .as_ref()
                    .filter(|p| p.etag.is_some() || p.last_modified.is_some())
                else {
                    bail!("服务器对没有校验头的请求回了 304（{}）", redact_url(url));
                };
                touch_meta(&db_path, p, &now, tgt.attribution.as_ref())?;
                return Ok(RefreshOutcome::NotModified { by_hash: false });
            }
            // DB-IP 当月文件还没发布：回退上月
            404 if i + 1 < n => continue,
            _ => bail!("服务器返回 HTTP {}（{}）", status.as_u16(), redact_url(url)),
        }
    }
    let (url, mut resp) = chosen.ok_or_else(|| anyhow!("没有可用的下载地址"))?;

    if let Some(len) = resp.content_length() {
        ensure!(
            len <= opts.max_bytes,
            "Content-Length {len} 超过上限 {} 字节，中止下载（{}）",
            opts.max_bytes,
            redact_url(&url)
        );
    }
    let etag = header_str(&resp, ETAG);
    let last_modified = header_str(&resp, LAST_MODIFIED);

    // 流式写入临时文件
    let name = db_path
        .file_name()
        .context("RIVU_GEOIP_DB_PATH 没有文件名")?
        .to_string_lossy()
        .into_owned();
    let pid = std::process::id();
    let guard = TempGuard(vec![
        dir.join(format!("{name}.{pid}.dl.tmp")),
        dir.join(format!("{name}.{pid}.tmp")),
    ]);
    let (raw, out) = (guard.0[0].clone(), guard.0[1].clone());
    let mut total = 0u64;
    {
        let file = tokio::fs::File::create(&raw)
            .await
            .with_context(|| format!("创建临时文件 {} 失败", raw.display()))?;
        let mut w = tokio::io::BufWriter::with_capacity(256 * 1024, file);
        while let Some(chunk) = resp
            .chunk()
            .await
            .map_err(|e| anyhow!("读取响应失败：{}", describe(&e.without_url())))?
        {
            total += chunk.len() as u64;
            ensure!(
                total <= opts.max_bytes,
                "下载内容超过上限 {} 字节，中止",
                opts.max_bytes
            );
            w.write_all(&chunk).await.context("写入临时文件失败")?;
        }
        w.flush().await.context("写入临时文件失败")?;
    }
    drop(resp);

    let fin = Finalize {
        raw,
        out,
        db_path,
        max_bytes: opts.max_bytes,
        source_kind: cfg.source.kind(),
        source_id,
        etag,
        last_modified,
        prev,
        attribution: tgt.attribution.as_ref().map(StoredAttribution::from),
        now,
    };
    // 解压、哈希、校验、fsync、rename 都是阻塞 IO，放到阻塞线程池；`guard` 随之移入，结束时清理临时文件。
    tokio::task::spawn_blocking(move || {
        let _guard = guard;
        finalize(fin)
    })
    .await
    .context("刷新任务异常退出")?
}

struct Finalize {
    raw: PathBuf,
    out: PathBuf,
    db_path: PathBuf,
    max_bytes: u64,
    source_kind: &'static str,
    source_id: String,
    etag: Option<String>,
    last_modified: Option<String>,
    prev: Option<Meta>,
    attribution: Option<StoredAttribution>,
    now: String,
}

fn finalize(f: Finalize) -> Result<RefreshOutcome> {
    let (sha256, bytes) = extract(&f.raw, &f.out, f.max_bytes)?;

    // 没有校验头的服务器：整文件内容与 meta 相同就不替换。有校验头却回了 200、内容相同时同样不必替换。
    if let Some(p) = &f.prev
        && p.sha256 == sha256
    {
        let refreshed = Meta {
            etag: f.etag.clone().or_else(|| p.etag.clone()),
            last_modified: f.last_modified.clone().or_else(|| p.last_modified.clone()),
            checked_at: f.now.clone(),
            attribution: f.attribution.clone(),
            ..p.clone()
        };
        meta::write(&f.db_path, &refreshed)?;
        return Ok(RefreshOutcome::NotModified { by_hash: true });
    }

    // 校验：失败则不替换，上一份副本原样保留
    let (reader, _key) = db::open_validated(&f.out).context("下载的文件未通过校验，保留旧副本")?;
    let metadata = reader.metadata();
    let (build_epoch, database_type) = (metadata.build_epoch, metadata.database_type.clone());
    drop(reader);

    // extract 已经对写入的文件 fsync；rename 同目录原子覆盖，随后才写 meta。
    // 崩溃在两步之间：库已是新的、meta 还是旧的，下次刷新多下载一次，不会出错。
    std::fs::rename(&f.out, &f.db_path)
        .with_context(|| format!("替换 {} 失败", f.db_path.display()))?;
    sync_parent_dir(&f.db_path);
    meta::write(
        &f.db_path,
        &Meta {
            source: f.source_kind.into(),
            source_id: f.source_id,
            etag: f.etag,
            last_modified: f.last_modified,
            sha256: sha256.clone(),
            downloaded_at: f.now.clone(),
            checked_at: f.now,
            build_epoch,
            database_type,
            attribution: f.attribution,
        },
    )?;
    Ok(RefreshOutcome::Updated {
        sha256,
        build_epoch,
        bytes,
    })
}

/// 304 或哈希相同：只更新检查时间（自定义来源还同步署名）。
fn touch_meta(
    db_path: &Path,
    prev: &Meta,
    now: &str,
    attribution: Option<&Attribution>,
) -> Result<()> {
    meta::write(
        db_path,
        &Meta {
            checked_at: now.to_string(),
            attribution: attribution.map(StoredAttribution::from),
            ..prev.clone()
        },
    )
}

/// 把下载的原始文件解成 mmdb 写到 `out`，返回内容的 SHA-256 与长度。
///
/// 处理三种形态：gzip（按魔数 `1f 8b` 判断）、tar（头部有 `ustar`，取第一个 `.mmdb` 文件，MaxMind 用）、
/// 裸 mmdb。解压后的大小同样受 `max` 限制，防止 gzip 炸弹。
fn extract(raw: &Path, out: &Path, max: u64) -> Result<(String, u64)> {
    let mut input = File::open(raw).context("打开下载的临时文件失败")?;
    let mut magic = [0u8; 2];
    let n = read_fully(&mut input, &mut magic)?;
    let chained = Cursor::new(magic[..n].to_vec()).chain(input);
    let mut reader: Box<dyn Read> = if n == 2 && magic == [0x1f, 0x8b] {
        Box::new(MultiGzDecoder::new(chained))
    } else {
        Box::new(chained)
    };

    let mut head = vec![0u8; 512];
    let n = read_fully(&mut reader, &mut head)?;
    head.truncate(n);
    let is_tar = head.len() >= 262 && &head[257..262] == b"ustar";
    let reader = Cursor::new(head).chain(reader);

    let mut w = HashingWriter::new(File::create(out).context("创建临时文件失败")?);
    if is_tar {
        let mut archive = tar::Archive::new(reader);
        let mut found = false;
        for entry in archive.entries().context("读取 tar 归档失败")? {
            let mut entry = entry.context("读取 tar 条目失败")?;
            if !entry.header().entry_type().is_file() {
                continue;
            }
            let is_mmdb = entry
                .path()
                .context("tar 条目路径无效")?
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("mmdb"));
            if !is_mmdb {
                continue;
            }
            ensure!(entry.size() <= max, "tar 里的 mmdb 超过上限 {max} 字节");
            copy_limited(&mut entry, &mut w, max)?;
            found = true;
            break;
        }
        ensure!(found, "tar 归档里没有 .mmdb 文件");
    } else {
        let mut reader = reader;
        copy_limited(&mut reader, &mut w, max).context("解压失败")?;
    }
    w.finish()
}

fn copy_limited(r: &mut impl Read, w: &mut impl Write, max: u64) -> Result<()> {
    let n = io::copy(&mut r.take(max + 1), w)?;
    ensure!(n <= max, "解压后的内容超过上限 {max} 字节");
    Ok(())
}

/// 读满 `buf` 或读到 EOF，返回读到的字节数。
fn read_fully(r: &mut impl Read, buf: &mut [u8]) -> io::Result<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        match r.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(filled)
}

/// 边写边算 SHA-256；`finish` 时 flush 并 fsync。
struct HashingWriter {
    inner: BufWriter<File>,
    hasher: Sha256,
    len: u64,
}

impl HashingWriter {
    fn new(file: File) -> Self {
        HashingWriter {
            inner: BufWriter::with_capacity(256 * 1024, file),
            hasher: Sha256::new(),
            len: 0,
        }
    }

    fn finish(self) -> Result<(String, u64)> {
        let file = self.inner.into_inner().map_err(|e| e.into_error())?;
        file.sync_all().context("fsync 临时文件失败")?;
        let digest = self.hasher.finalize();
        Ok((
            digest.iter().map(|b| format!("{b:02x}")).collect(),
            self.len,
        ))
    }
}

impl Write for HashingWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.hasher.update(&buf[..n]);
        self.len += n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

/// 离开作用域时删除临时文件（成功时 `out` 已被 rename 走，删除会静默失败）。
struct TempGuard(Vec<PathBuf>);

impl Drop for TempGuard {
    fn drop(&mut self) {
        for p in &self.0 {
            let _ = std::fs::remove_file(p);
        }
    }
}

/// 清理崩溃遗留的临时文件：库文件名开头、`.tmp` 结尾、一小时以上没有修改。
fn remove_stale_temps(db_path: &Path) {
    let (Some(dir), Some(name)) = (db_path.parent(), db_path.file_name()) else {
        return;
    };
    let prefix = format!("{}.", name.to_string_lossy());
    let Ok(entries) = std::fs::read_dir(if dir.as_os_str().is_empty() {
        Path::new(".")
    } else {
        dir
    }) else {
        return;
    };
    for e in entries.flatten() {
        let fname = e.file_name().to_string_lossy().into_owned();
        if !(fname.starts_with(&prefix) && fname.ends_with(".tmp")) {
            continue;
        }
        let old = e
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| SystemTime::now().duration_since(t).ok())
            .is_some_and(|age| age > Duration::from_secs(3600));
        if old {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

/// rename 之后 fsync 父目录，让目录项落盘（仅 Unix；Windows 不能以普通方式打开目录）。
fn sync_parent_dir(path: &Path) {
    #[cfg(unix)]
    if let Some(dir) = path.parent().filter(|p| !p.as_os_str().is_empty())
        && let Ok(d) = File::open(dir)
    {
        let _ = d.sync_all();
    }
    #[cfg(not(unix))]
    let _ = path;
}

fn header_str(resp: &reqwest::Response, name: reqwest::header::HeaderName) -> Option<String> {
    resp.headers()
        .get(name)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string)
}

/// 错误及其来源链，用 `: ` 连接。
fn describe(e: &dyn std::error::Error) -> String {
    let mut out = e.to_string();
    let mut cur = e.source();
    while let Some(s) = cur {
        out.push_str(": ");
        out.push_str(&s.to_string());
        cur = s.source();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ts(s: &str) -> Timestamp {
        s.parse().unwrap()
    }

    #[test]
    fn dbip_urls_are_current_then_previous_month_utc() {
        let urls = dbip_urls(DBIP_BASE_URL, DbIpEdition::City, ts("2026-10-22T08:00:00Z"));
        assert_eq!(
            urls,
            [
                "https://download.db-ip.com/free/dbip-city-lite-2026-10.mmdb.gz",
                "https://download.db-ip.com/free/dbip-city-lite-2026-09.mmdb.gz",
            ]
        );
        // 跨年
        let urls = dbip_urls(
            DBIP_BASE_URL,
            DbIpEdition::Country,
            ts("2027-01-01T00:00:00Z"),
        );
        assert_eq!(
            urls,
            [
                "https://download.db-ip.com/free/dbip-country-lite-2027-01.mmdb.gz",
                "https://download.db-ip.com/free/dbip-country-lite-2026-12.mmdb.gz",
            ]
        );
        // 月份按 UTC：东八区 11 月 1 日 07:30 在 UTC 仍是 10 月 31 日 23:30
        let urls = dbip_urls(
            "http://h/",
            DbIpEdition::City,
            ts("2026-11-01T07:30:00+08:00"),
        );
        assert_eq!(urls[0], "http://h/dbip-city-lite-2026-10.mmdb.gz");
        assert_eq!(urls[1], "http://h/dbip-city-lite-2026-09.mmdb.gz");
    }

    #[test]
    fn extract_handles_plain_gz_and_tar_gz_and_enforces_limit() {
        use flate2::Compression;
        use flate2::write::GzEncoder;

        let dir = std::env::temp_dir().join(format!("rivu-geo-extract-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let payload: Vec<u8> = (0..50_000u32).map(|i| (i % 251) as u8).collect();
        let sha = {
            let d = Sha256::digest(&payload);
            d.iter().map(|b| format!("{b:02x}")).collect::<String>()
        };
        let (raw, out) = (dir.join("raw"), dir.join("out"));

        // 裸文件
        std::fs::write(&raw, &payload).unwrap();
        assert_eq!(extract(&raw, &out, 1 << 20).unwrap(), (sha.clone(), 50_000));
        assert_eq!(std::fs::read(&out).unwrap(), payload);

        // gzip
        let mut gz = GzEncoder::new(Vec::new(), Compression::default());
        gz.write_all(&payload).unwrap();
        std::fs::write(&raw, gz.finish().unwrap()).unwrap();
        assert_eq!(extract(&raw, &out, 1 << 20).unwrap().0, sha);

        // tar.gz：先放一个无关文件，再放 .mmdb（MaxMind 的归档还有 LICENSE、COPYRIGHT 文本）
        let mut tarball = Vec::new();
        {
            let mut b = tar::Builder::new(&mut tarball);
            for (path, data) in [
                ("GeoLite2-City_20261020/COPYRIGHT.txt", &b"copyright"[..]),
                ("GeoLite2-City_20261020/GeoLite2-City.mmdb", &payload[..]),
            ] {
                let mut h = tar::Header::new_gnu();
                h.set_size(data.len() as u64);
                h.set_mode(0o644);
                h.set_cksum();
                b.append_data(&mut h, path, data).unwrap();
            }
            b.finish().unwrap();
        }
        let mut gz = GzEncoder::new(Vec::new(), Compression::default());
        gz.write_all(&tarball).unwrap();
        std::fs::write(&raw, gz.finish().unwrap()).unwrap();
        assert_eq!(extract(&raw, &out, 1 << 20).unwrap(), (sha.clone(), 50_000));

        // 上限：解压后超过 max 就中止（gzip 炸弹）
        let zeros = vec![0u8; 1 << 20];
        let mut gz = GzEncoder::new(Vec::new(), Compression::best());
        gz.write_all(&zeros).unwrap();
        let bomb = gz.finish().unwrap();
        assert!(bomb.len() < 4096, "炸弹本身很小");
        std::fs::write(&raw, bomb).unwrap();
        let err = extract(&raw, &out, 64 * 1024).unwrap_err();
        assert!(format!("{err:#}").contains("超过上限"), "{err:#}");

        // tar 里没有 mmdb
        let mut tarball = Vec::new();
        {
            let mut b = tar::Builder::new(&mut tarball);
            let mut h = tar::Header::new_gnu();
            h.set_size(3);
            h.set_mode(0o644);
            h.set_cksum();
            b.append_data(&mut h, "only.txt", &b"abc"[..]).unwrap();
            b.finish().unwrap();
        }
        std::fs::write(&raw, tarball).unwrap();
        let err = extract(&raw, &out, 1 << 20).unwrap_err();
        assert!(format!("{err:#}").contains("没有 .mmdb"), "{err:#}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn local_copy_states() {
        let dir = std::env::temp_dir().join(format!("rivu-geo-local-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut cfg = GeoConfig::new(&dir);
        cfg.db_path = dir.join("geoip.mmdb");
        assert_eq!(local_copy(&cfg), LocalCopy::Missing);

        std::fs::write(&cfg.db_path, b"x").unwrap();
        // 默认来源 + 没有 meta：手工放置
        assert_eq!(local_copy(&cfg), LocalCopy::Manual);
        // 显式来源 + 没有 meta：视为没有本地副本
        let mut custom = cfg.clone();
        custom.source = GeoSource::Custom {
            url: rivu_common::config::Secret::new("https://x.test/a.mmdb"),
        };
        assert_eq!(local_copy(&custom), LocalCopy::Foreign);

        let m = Meta {
            source: "dbip-lite".into(),
            source_id: cfg.source.id(),
            etag: None,
            last_modified: None,
            sha256: String::new(),
            downloaded_at: String::new(),
            checked_at: String::new(),
            build_epoch: 0,
            database_type: String::new(),
            attribution: None,
        };
        meta::write(&cfg.db_path, &m).unwrap();
        assert_eq!(local_copy(&cfg), LocalCopy::Current);
        // 来源变了（DB-IP -> 自定义 URL，或 city -> country）
        assert_eq!(local_copy(&custom), LocalCopy::Foreign);
        let mut country = cfg.clone();
        country.source = GeoSource::DbIp {
            edition: DbIpEdition::Country,
        };
        assert_eq!(local_copy(&country), LocalCopy::Foreign);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
