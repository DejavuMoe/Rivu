//! 看板静态文件（api / all 角色）。看板是 SvelteKit 的单页应用（`apps/dashboard`，adapter-static），由 rivu 直接提供，
//! 反向代理（nginx / Caddy）只需把整个站点转给 rivu。
//!
//! - 启动时把 `RIVU_DASHBOARD_DIR`（默认 `apps/dashboard/build`）里的全部文件读进内存，之后不再读盘；
//!   构建时用 `precompress` 产出的 `.br` / `.gz` 一并读入，按 `Accept-Encoding` 直接发送（`Vary: Accept-Encoding`）。
//! - 路径对应到文件就发送文件；没有对应文件、且最后一段不像文件名（没有扩展名）的路径回落到 `index.html`，
//!   由前端路由决定显示什么。带扩展名但不存在的路径是 404，`/api/*` 与 `/js/*` 不在这里处理。
//! - 缓存：`_app/immutable/` 下的文件名带内容哈希，缓存一年并标 `immutable`；其余（`index.html`、图标）`no-cache`，
//!   每次用 ETag 协商，发布新版本后立即生效。
//! - 目录缺失或为空时，页面路径返回 503 并说明原因，`/readyz` 的 `checks.dashboard` 报告但**不判就绪失败**
//!   （公开 API 与采集不依赖看板）。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

/// 读进内存的总量上限：防止目录配错（指到了大目录）时把整个目录读进内存。看板构建产物约几 MB。
const MAX_TOTAL_BYTES: u64 = 64 * 1024 * 1024;
const IMMUTABLE: &str = "public, max-age=31536000, immutable";
const REVALIDATE: &str = "no-cache";

struct File {
    content_type: &'static str,
    etag: String,
    raw: Vec<u8>,
    br: Option<Vec<u8>>,
    gzip: Option<Vec<u8>>,
}

pub struct DashboardAssets {
    dir: PathBuf,
    files: Result<HashMap<String, File>, String>,
}

fn content_type(path: &str) -> &'static str {
    match path.rsplit('.').next().unwrap_or("") {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" => "application/json",
        "webmanifest" => "application/manifest+json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "ico" => "image/x-icon",
        "webp" => "image/webp",
        "woff2" => "font/woff2",
        "woff" => "font/woff",
        "txt" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

fn etag(bytes: &[u8]) -> String {
    let d = Sha256::digest(bytes);
    let hex: String = d[..16].iter().map(|b| format!("{b:02x}")).collect();
    format!("\"{hex}\"")
}

/// 递归列出目录里的普通文件，返回（相对路径，用 `/` 分隔）。
fn walk(root: &Path, dir: &Path, out: &mut Vec<(String, PathBuf)>) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let ty = entry.file_type()?;
        if ty.is_dir() {
            walk(root, &path, out)?;
        } else if ty.is_file() {
            let rel = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join("/");
            out.push((rel, path));
        }
    }
    Ok(())
}

impl DashboardAssets {
    /// 读入目录。失败不是致命错误：记 `warn`，页面路径返回 503。
    pub fn load(dir: &Path) -> Self {
        let files = Self::read(dir);
        match &files {
            Ok(f) => tracing::info!(dir = %dir.display(), files = f.len(), "看板静态文件已加载"),
            Err(e) => tracing::warn!(
                dir = %dir.display(), error = %e,
                "看板静态文件不可用：页面将返回 503（在 RIVU_DASHBOARD_DIR 里放入 apps/dashboard 的 pnpm build 产物 build/）"
            ),
        }
        DashboardAssets {
            dir: dir.to_owned(),
            files,
        }
    }

    fn read(dir: &Path) -> Result<HashMap<String, File>, String> {
        let mut list = Vec::new();
        walk(dir, dir, &mut list).map_err(|e| format!("读取 {} 失败: {e}", dir.display()))?;
        let total: u64 = list
            .iter()
            .filter_map(|(_, p)| std::fs::metadata(p).ok())
            .map(|m| m.len())
            .sum();
        if total > MAX_TOTAL_BYTES {
            return Err(format!(
                "{} 里的文件共 {} MiB，超过上限 {} MiB，目录可能配错了",
                dir.display(),
                total / 1024 / 1024,
                MAX_TOTAL_BYTES / 1024 / 1024
            ));
        }
        let mut raw: HashMap<String, Vec<u8>> = HashMap::new();
        for (rel, path) in list {
            let bytes =
                std::fs::read(&path).map_err(|e| format!("读取 {} 失败: {e}", path.display()))?;
            raw.insert(rel, bytes);
        }
        if !raw.contains_key("index.html") {
            return Err(format!("{} 里没有 index.html", dir.display()));
        }
        let mut files = HashMap::new();
        for (rel, bytes) in &raw {
            // 压缩副本跟随原文件，不单独作为文件提供
            if rel.ends_with(".br") || rel.ends_with(".gz") {
                continue;
            }
            files.insert(
                rel.clone(),
                File {
                    content_type: content_type(rel),
                    etag: etag(bytes),
                    raw: bytes.clone(),
                    br: raw.get(&format!("{rel}.br")).cloned(),
                    gzip: raw.get(&format!("{rel}.gz")).cloned(),
                },
            );
        }
        Ok(files)
    }

    /// 直接用内存里的文件构造（测试用）。
    pub fn from_files(dir: &Path, entries: &[(&str, &[u8])]) -> Self {
        let files = entries
            .iter()
            .map(|(rel, bytes)| {
                (
                    (*rel).to_owned(),
                    File {
                        content_type: content_type(rel),
                        etag: etag(bytes),
                        raw: bytes.to_vec(),
                        br: None,
                        gzip: None,
                    },
                )
            })
            .collect();
        DashboardAssets {
            dir: dir.to_owned(),
            files: Ok(files),
        }
    }

    /// `/readyz` 的 `checks.dashboard`：只报告，`ok` 恒为 `true`。
    pub fn status(&self) -> Value {
        let mut v = json!({"ok": true, "loaded": self.files.is_ok(), "dir": self.dir.display().to_string()});
        match &self.files {
            Ok(f) => v["files"] = json!(f.len()),
            Err(e) => v["error"] = json!(e),
        }
        v
    }
}

/// 看板路径的处理函数，作为公共路由的 fallback 挂在 api / all 角色上。
pub async fn serve(State(assets): State<Arc<DashboardAssets>>, req: Request) -> Response {
    if req.method() != Method::GET && req.method() != Method::HEAD {
        let mut r = StatusCode::METHOD_NOT_ALLOWED.into_response();
        r.headers_mut()
            .insert(header::ALLOW, HeaderValue::from_static("GET, HEAD"));
        return r;
    }
    let path = req.uri().path();
    if path.starts_with("/api/") || path == "/api" || path.starts_with("/js/") {
        return StatusCode::NOT_FOUND.into_response();
    }
    let files = match &assets.files {
        Ok(f) => f,
        Err(_) => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                [(header::CACHE_CONTROL, "no-store")],
                "Rivu dashboard files are not installed (RIVU_DASHBOARD_DIR)",
            )
                .into_response();
        }
    };
    let rel = path.trim_start_matches('/');
    let (key, file) = match files.get(rel) {
        Some(f) if !rel.is_empty() => (rel, f),
        _ => {
            // 没有扩展名的路径是页面：回落到 index.html；带扩展名的是缺失的资源
            let last = rel.rsplit('/').next().unwrap_or("");
            if last.contains('.') {
                return (StatusCode::NOT_FOUND, [(header::CACHE_CONTROL, "no-cache")])
                    .into_response();
            }
            ("index.html", &files["index.html"])
        }
    };
    respond(key, file, req.headers(), req.method() == Method::HEAD)
}

fn accepts(headers: &HeaderMap, enc: &str) -> bool {
    headers
        .get_all(header::ACCEPT_ENCODING)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .any(|part| {
            let mut it = part.trim().split(';');
            let name = it.next().unwrap_or("").trim();
            let q_zero = it.any(|p| p.trim().replace(' ', "") == "q=0");
            name.eq_ignore_ascii_case(enc) && !q_zero
        })
}

fn respond(key: &str, file: &File, headers: &HeaderMap, head: bool) -> Response {
    let cache = if key.starts_with("_app/immutable/") {
        IMMUTABLE
    } else {
        REVALIDATE
    };
    let not_modified = headers
        .get_all(header::IF_NONE_MATCH)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .any(|t| t.trim() == file.etag || t.trim() == "*");

    let mut builder = Response::builder()
        .header(header::ETAG, &file.etag)
        .header(header::CACHE_CONTROL, cache)
        .header(header::VARY, "Accept-Encoding")
        .header(header::X_CONTENT_TYPE_OPTIONS, "nosniff")
        .header(header::REFERRER_POLICY, "strict-origin-when-cross-origin");
    if not_modified {
        return builder
            .status(StatusCode::NOT_MODIFIED)
            .body(Body::empty())
            .expect("响应头都是合法值");
    }
    let (body, encoding) = match (&file.br, &file.gzip) {
        (Some(b), _) if accepts(headers, "br") => (b, Some("br")),
        (_, Some(g)) if accepts(headers, "gzip") => (g, Some("gzip")),
        _ => (&file.raw, None),
    };
    builder = builder
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, file.content_type)
        .header(header::CONTENT_LENGTH, body.len());
    if let Some(enc) = encoding {
        builder = builder.header(header::CONTENT_ENCODING, enc);
    }
    builder
        .body(if head {
            Body::empty()
        } else {
            Body::from(body.clone())
        })
        .expect("响应头都是合法值")
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::Router;
    use axum::body::to_bytes;
    use tower::ServiceExt;

    fn app() -> Router {
        let assets = DashboardAssets::from_files(
            Path::new("/x"),
            &[
                ("index.html", b"<!doctype html>app"),
                ("_app/immutable/entry/start.abc123.js", b"console.log(1)"),
                ("favicon.svg", b"<svg/>"),
            ],
        );
        Router::new().fallback(serve).with_state(Arc::new(assets))
    }

    async fn get(path: &str, headers: &[(&str, &str)]) -> Response {
        let mut req = Request::builder().uri(path);
        for (k, v) in headers {
            req = req.header(*k, *v);
        }
        app()
            .oneshot(req.body(Body::empty()).unwrap())
            .await
            .unwrap()
    }

    async fn text(r: Response) -> String {
        String::from_utf8(to_bytes(r.into_body(), 1 << 20).await.unwrap().to_vec()).unwrap()
    }

    #[tokio::test]
    async fn pages_fall_back_to_index_and_assets_are_cached() {
        for page in ["/", "/sites", "/s/12/settings/general", "/share/abc"] {
            let r = get(page, &[]).await;
            assert_eq!(r.status(), 200, "{page}");
            assert_eq!(r.headers()[header::CACHE_CONTROL], REVALIDATE);
            assert_eq!(
                r.headers()[header::CONTENT_TYPE],
                "text/html; charset=utf-8"
            );
            assert_eq!(text(r).await, "<!doctype html>app");
        }
        let r = get("/_app/immutable/entry/start.abc123.js", &[]).await;
        assert_eq!(r.status(), 200);
        assert_eq!(r.headers()[header::CACHE_CONTROL], IMMUTABLE);
        assert_eq!(
            r.headers()[header::CONTENT_TYPE],
            "text/javascript; charset=utf-8"
        );
        let r = get("/favicon.svg", &[]).await;
        assert_eq!(r.headers()[header::CONTENT_TYPE], "image/svg+xml");
    }

    #[tokio::test]
    async fn missing_assets_and_other_namespaces_are_404() {
        for path in [
            "/_app/immutable/gone.js",
            "/robots.txt",
            "/api/v1/nope",
            "/js/t.js",
        ] {
            assert_eq!(get(path, &[]).await.status(), 404, "{path}");
        }
    }

    #[tokio::test]
    async fn etag_revalidation_returns_304() {
        let r = get("/sites", &[]).await;
        let tag = r.headers()[header::ETAG].to_str().unwrap().to_owned();
        let r = get("/sites", &[("if-none-match", &tag)]).await;
        assert_eq!(r.status(), 304);
        assert!(text(r).await.is_empty());
    }

    #[tokio::test]
    async fn precompressed_variant_follows_accept_encoding() {
        let mut assets = DashboardAssets::from_files(Path::new("/x"), &[("index.html", b"plain")]);
        if let Ok(files) = &mut assets.files {
            let f = files.get_mut("index.html").unwrap();
            f.br = Some(b"BR".to_vec());
            f.gzip = Some(b"GZ".to_vec());
        }
        let app = Router::new().fallback(serve).with_state(Arc::new(assets));
        let send = |enc: &'static str| {
            let app = app.clone();
            async move {
                let req = Request::builder()
                    .uri("/")
                    .header("accept-encoding", enc)
                    .body(Body::empty())
                    .unwrap();
                app.oneshot(req).await.unwrap()
            }
        };
        let r = send("gzip, deflate, br").await;
        assert_eq!(r.headers()[header::CONTENT_ENCODING], "br");
        assert_eq!(text(r).await, "BR");
        let r = send("gzip").await;
        assert_eq!(r.headers()[header::CONTENT_ENCODING], "gzip");
        let r = send("br;q=0, identity").await;
        assert!(r.headers().get(header::CONTENT_ENCODING).is_none());
        assert_eq!(text(r).await, "plain");
    }

    #[tokio::test]
    async fn unloaded_dir_is_503_and_reported() {
        let assets = DashboardAssets::load(Path::new("/definitely/not/here"));
        assert_eq!(assets.status()["loaded"], false);
        let app = Router::new().fallback(serve).with_state(Arc::new(assets));
        let r = app
            .oneshot(
                Request::builder()
                    .uri("/sites")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r.status(), 503);
    }

    #[tokio::test]
    async fn only_get_and_head() {
        let r = app()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/sites")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r.status(), 405);
    }
}
