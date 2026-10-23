//! 站点专属脚本 `GET /js/<site_key>.js`（`tracker-and-protocol.md` §2.1、§2.3、§2.4）。
//!
//! 响应 = **配置前缀** + `core.js` 的字节：
//!
//! ```js
//! window.rivu=window.rivu||function(){(rivu.q=rivu.q||[]).push(arguments)};rivu.c={"site":"…","endpoint":"…","hash":false};
//! ```
//!
//! - 前缀由唯一的映射函数 [`tracker_prefix`] 从站点配置生成（有测试，防止库中开关与脚本脱节）；
//!   `endpoint` 来自配置项 `RIVU_PUBLIC_BASE_URL`，**不取请求的 `Host`**（防止 CDN 缓存被污染）；
//!   字符串按 JSON 转义并额外转义 `<`、U+2028、U+2029。
//! - 只发 `core`：`full` 尚未实现（`apps/tracker/README.md`）；站点启用 full 专属能力的选择逻辑留到 full 之后。
//!   前缀里只有 core 读取的键（`site`、`endpoint`、`hash`）。
//! - `core.js` 不在 cargo 构建里生成（构建里不跑 pnpm）：启动时从 `RIVU_TRACKER_DIR`（默认 `apps/tracker/dist`）
//!   读入 `core.js` 与 `meta.json`（`{"sv": 构建号}`）并缓存字节，之后不再读盘。目录缺失或文件不全时，
//!   本路由对已知站点返回 `503`，并在 `/readyz` 的 `checks.tracker` 里报告，但**不判就绪失败**
//!   （采集本身不依赖脚本分发）。`sv` 来自产物，服务端不改写。生产镜像应把 CI 构建的 `dist/` 放进该目录。
//! - 响应头：`Content-Type: application/javascript; charset=utf-8`、`ETag`、`Cache-Control: public, max-age=300`、
//!   `X-Content-Type-Options: nosniff`、`Cross-Origin-Resource-Policy: cross-origin`、`Access-Control-Allow-Origin: *`；
//!   `If-None-Match` 命中返回 `304`。
//! - 未知 `site_key`（含已删除的站点，它们不在快照里）与已停用（`collect_enabled = false`）的站点：
//!   `404`、`Cache-Control: no-store`、空体。

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::Router;
use axum::extract::{Path as UrlPath, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use rivu_ingest::pipeline::SiteSnapshot;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

/// 站点脚本的缓存时间（秒）：开关变更最多这么久生效（§2.4，以 ETag 换更长缓存）。
pub const SCRIPT_MAX_AGE_SECS: u32 = 300;

/// `core.js` 的最大字节数：防止目录里放错文件而被整个读进内存（预算是 1 KiB gzip，几 KB 原文）。
const MAX_CORE_BYTES: u64 = 256 * 1024;

/// 已读入内存的 tracker 构建产物。
struct Loaded {
    core: Vec<u8>,
    /// 构建号，来自 `meta.json`。
    sv: u32,
}

/// `RIVU_TRACKER_DIR` 里读到的构建产物（启动时加载一次）。
pub struct TrackerAssets {
    dir: PathBuf,
    loaded: Result<Loaded, String>,
}

impl TrackerAssets {
    /// 从目录读入 `core.js` 与 `meta.json`。缺失或无效不是致命错误：记 `warn`，路由返回 503。
    pub fn load(dir: &Path) -> Self {
        let loaded = Self::read(dir);
        match &loaded {
            Ok(l) => tracing::info!(
                dir = %dir.display(), sv = l.sv, bytes = l.core.len(),
                "tracker 构建产物已加载"
            ),
            Err(e) => tracing::warn!(
                dir = %dir.display(), error = %e,
                "tracker 构建产物不可用：/js/<site_key>.js 将返回 503（在 RIVU_TRACKER_DIR 里放入 pnpm build 的 dist/）"
            ),
        }
        TrackerAssets {
            dir: dir.to_owned(),
            loaded,
        }
    }

    fn read(dir: &Path) -> Result<Loaded, String> {
        let core_path = dir.join("core.js");
        let meta_path = dir.join("meta.json");
        let md = std::fs::metadata(&core_path)
            .map_err(|e| format!("读取 {} 失败: {e}", core_path.display()))?;
        if !md.is_file() || md.len() == 0 || md.len() > MAX_CORE_BYTES {
            return Err(format!(
                "{} 不是 1 B–{} KiB 的普通文件（实际 {} B）",
                core_path.display(),
                MAX_CORE_BYTES / 1024,
                md.len()
            ));
        }
        let core = std::fs::read(&core_path)
            .map_err(|e| format!("读取 {} 失败: {e}", core_path.display()))?;
        let meta = std::fs::read_to_string(&meta_path)
            .map_err(|e| format!("读取 {} 失败: {e}", meta_path.display()))?;
        let meta: Value = serde_json::from_str(&meta)
            .map_err(|e| format!("{} 不是合法 JSON: {e}", meta_path.display()))?;
        let sv = meta
            .get("sv")
            .and_then(Value::as_u64)
            .and_then(|v| u32::try_from(v).ok())
            .ok_or_else(|| format!("{} 缺少整数字段 sv", meta_path.display()))?;
        Ok(Loaded { core, sv })
    }

    /// 直接用内存里的字节构造（测试用）。
    pub fn from_parts(dir: &Path, core: Vec<u8>, sv: u32) -> Self {
        TrackerAssets {
            dir: dir.to_owned(),
            loaded: Ok(Loaded { core, sv }),
        }
    }

    pub fn is_loaded(&self) -> bool {
        self.loaded.is_ok()
    }

    /// `/readyz` 的 `checks.tracker`：只报告，`ok` 恒为 `true`。
    pub fn status(&self) -> Value {
        let mut v =
            json!({"ok": true, "loaded": self.is_loaded(), "dir": self.dir.display().to_string()});
        match &self.loaded {
            Ok(l) => {
                v["sv"] = json!(l.sv);
                v["core_bytes"] = json!(l.core.len());
            }
            Err(e) => v["error"] = json!(e),
        }
        v
    }
}

/// 站点脚本分发所需的全部状态。
pub struct ScriptService {
    sites: Arc<SiteSnapshot>,
    assets: Arc<TrackerAssets>,
    /// 完整的采集端点 URL（`<RIVU_PUBLIC_BASE_URL>/api/event`）。
    endpoint: String,
}

impl ScriptService {
    pub fn new(
        sites: Arc<SiteSnapshot>,
        assets: Arc<TrackerAssets>,
        public_base_url: &str,
    ) -> Self {
        ScriptService {
            sites,
            assets,
            endpoint: format!("{}/api/event", public_base_url.trim_end_matches('/')),
        }
    }
}

/// JSON 字符串字面量，额外转义 `<`、U+2028、U+2029（§2.3：防止 `</script>` 与旧引擎的行终止符问题）。
fn js_string(s: &str) -> String {
    let json = serde_json::to_string(s).expect("字符串总能序列化");
    let mut out = String::with_capacity(json.len() + 8);
    for c in json.chars() {
        match c {
            '<' => out.push_str("\\u003c"),
            '\u{2028}' => out.push_str("\\u2028"),
            '\u{2029}' => out.push_str("\\u2029"),
            c => out.push(c),
        }
    }
    out
}

/// `site_key` 只含 `[A-Za-z0-9_-]`（§2.3）。
fn valid_site_key(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// 站点脚本的配置前缀：**唯一**的「站点配置 → 脚本配置」映射（§2.3）。
/// 键序固定：`site`、`endpoint`、`hash`（core 读取的键；full 的开关等 full 实现后再加）。
/// `site_key` 含非法字符时返回 `None`（调用方已按路径校验过，这里再守一次，因为它会进入 JS 源码）。
pub fn tracker_prefix(site_key: &str, endpoint: &str, hash: bool) -> Option<String> {
    if !valid_site_key(site_key) {
        return None;
    }
    Some(format!(
        "window.rivu=window.rivu||function(){{(rivu.q=rivu.q||[]).push(arguments)}};rivu.c={{\"site\":{},\"endpoint\":{},\"hash\":{hash}}};",
        js_string(site_key),
        js_string(endpoint),
    ))
}

/// ETag：`sha256(构建号 ‖ 前缀 ‖ core.js)` 的前 16 字节，十六进制，带引号。
/// 文档（§2.4）写的是 `sha256(构建号 ‖ 前缀)`；这里额外混入产物字节，这样同一构建号下重新构建出不同的产物
/// 也不会被旧 ETag 误判为未变化。
fn etag(sv: u32, prefix: &str, core: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(sv.to_be_bytes());
    h.update([0]);
    h.update(prefix.as_bytes());
    h.update([0]);
    h.update(core);
    let digest = h.finalize();
    let mut out = String::with_capacity(34);
    out.push('"');
    for b in &digest[..16] {
        let _ = write!(out, "{b:02x}");
    }
    out.push('"');
    out
}

/// `If-None-Match` 是否命中（弱比较：忽略 `W/`；`*` 命中任何已存在的资源）。
fn if_none_match_hits(headers: &HeaderMap, etag: &str) -> bool {
    headers
        .get_all(header::IF_NONE_MATCH)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .map(str::trim)
        .any(|c| c == "*" || c.strip_prefix("W/").unwrap_or(c) == etag)
}

const CORP: HeaderName = HeaderName::from_static("cross-origin-resource-policy");
const NOSNIFF: HeaderName = HeaderName::from_static("x-content-type-options");

fn script_headers(h: &mut HeaderMap, cache_control: &'static str) {
    h.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static(cache_control),
    );
    h.insert(CORP, HeaderValue::from_static("cross-origin"));
    h.insert(
        header::ACCESS_CONTROL_ALLOW_ORIGIN,
        HeaderValue::from_static("*"),
    );
    h.insert(NOSNIFF, HeaderValue::from_static("nosniff"));
}

/// 404 / 503 这类不可缓存的空体响应。
fn no_store(status: StatusCode) -> Response {
    let mut resp = status.into_response();
    script_headers(resp.headers_mut(), "no-store");
    resp
}

pub fn router(svc: Arc<ScriptService>) -> Router {
    Router::new()
        .route("/js/{file}", get(serve))
        .with_state(svc)
}

async fn serve(
    State(svc): State<Arc<ScriptService>>,
    UrlPath(file): UrlPath<String>,
    headers: HeaderMap,
) -> Response {
    // 路径必须是 `<site_key>.js`；其它形态（含将来的 `t.js`、`t.<sv>.js`）在这里一律 404
    let Some(key) = file.strip_suffix(".js").filter(|k| valid_site_key(k)) else {
        return no_store(StatusCode::NOT_FOUND);
    };
    let Some(site) = svc.sites.get(key).filter(|s| s.collect_enabled) else {
        return no_store(StatusCode::NOT_FOUND);
    };
    let Ok(loaded) = &svc.assets.loaded else {
        let mut resp = no_store(StatusCode::SERVICE_UNAVAILABLE);
        resp.headers_mut()
            .insert(header::RETRY_AFTER, HeaderValue::from_static("30"));
        return resp;
    };
    let Some(prefix) = tracker_prefix(key, &svc.endpoint, site.tracker_hash) else {
        return no_store(StatusCode::NOT_FOUND);
    };

    let etag = etag(loaded.sv, &prefix, &loaded.core);
    let etag_value = HeaderValue::from_str(&etag).expect("ETag 是 ASCII");
    let cache_control = "public, max-age=300";
    if if_none_match_hits(&headers, &etag) {
        let mut resp = StatusCode::NOT_MODIFIED.into_response();
        script_headers(resp.headers_mut(), cache_control);
        resp.headers_mut().insert(header::ETAG, etag_value);
        return resp;
    }

    let mut body = Vec::with_capacity(prefix.len() + loaded.core.len());
    body.extend_from_slice(prefix.as_bytes());
    body.extend_from_slice(&loaded.core);
    let mut resp = (StatusCode::OK, body).into_response();
    let h = resp.headers_mut();
    script_headers(h, cache_control);
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/javascript; charset=utf-8"),
    );
    h.insert(header::ETAG, etag_value);
    resp
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use jiff::tz::TimeZone;
    use rivu_geo::GeoLevel;
    use rivu_ingest::hostname::HostnamePatterns;
    use rivu_ingest::pipeline::SiteConfig;

    use super::*;
    use crate::testutil::{request, send};

    const KEY: &str = "k7qm2xv5atbr";
    const OFF_KEY: &str = "dddddddddddd";
    const HASH_KEY: &str = "hhhhhhhhhhhh";
    const CORE: &str = "(function(){window.rivu.sv=7})();";
    const BASE: &str = "https://stats.example.com";

    fn site(collect_enabled: bool, tracker_hash: bool) -> Arc<SiteConfig> {
        Arc::new(SiteConfig {
            site_id: 1,
            timezone: TimeZone::UTC,
            collect_enabled,
            hostnames: HostnamePatterns::new(["example.test"]),
            geo_level: GeoLevel::Country,
            tracker_hash,
        })
    }

    fn service(assets: TrackerAssets) -> Arc<ScriptService> {
        let sites = Arc::new(SiteSnapshot::new());
        sites.replace(HashMap::from([
            (KEY.to_owned(), site(true, false)),
            (OFF_KEY.to_owned(), site(false, false)),
            (HASH_KEY.to_owned(), site(true, true)),
        ]));
        Arc::new(ScriptService::new(sites, Arc::new(assets), BASE))
    }

    fn loaded_service() -> Arc<ScriptService> {
        service(TrackerAssets::from_parts(
            Path::new("/x"),
            CORE.as_bytes().to_vec(),
            7,
        ))
    }

    #[test]
    fn prefix_has_the_documented_shape() {
        assert_eq!(
            tracker_prefix(KEY, "https://stats.example.com/api/event", false).unwrap(),
            "window.rivu=window.rivu||function(){(rivu.q=rivu.q||[]).push(arguments)};\
             rivu.c={\"site\":\"k7qm2xv5atbr\",\"endpoint\":\"https://stats.example.com/api/event\",\"hash\":false};"
        );
        let hash = tracker_prefix(KEY, "https://s/api/event", true).unwrap();
        assert!(hash.ends_with("\"hash\":true};"), "{hash}");
    }

    #[test]
    fn prefix_matches_the_fixture_reference_in_apps_tracker() {
        // `apps/tracker/test/fixture/server.ts` 的 `buildPrefix` 是同形态的参考：
        // 桩部分必须逐字节相同，配置是一个 JSON 对象字面量。
        let fixture = include_str!("../../../apps/tracker/test/fixture/server.ts");
        let stub = "window.rivu=window.rivu||function(){(rivu.q=rivu.q||[]).push(arguments)};";
        assert!(fixture.contains(stub), "夹具里的队列桩与服务端的前缀不一致");
        assert!(
            tracker_prefix(KEY, "http://e/api/event", false)
                .unwrap()
                .starts_with(stub)
        );
    }

    #[test]
    fn prefix_is_valid_json_config_and_escapes_dangerous_characters() {
        // 额外转义 `<`、U+2028、U+2029：即使 endpoint 里有它们，也不会提前结束 <script> 或断行
        let ep = "https://a.test/</script>\u{2028}\u{2029}\"\\x";
        let p = tracker_prefix(KEY, ep, false).unwrap();
        assert!(
            !p.contains('<') && !p.contains('\u{2028}') && !p.contains('\u{2029}'),
            "{p}"
        );
        // 去掉桩后剩下 `rivu.c={…};`，花括号里的部分是合法 JSON，且还原后与输入相同
        let json = p
            .split_once("rivu.c=")
            .and_then(|(_, rest)| rest.strip_suffix(';'))
            .unwrap();
        let v: Value = serde_json::from_str(json).unwrap();
        assert_eq!(v["endpoint"], ep);
        assert_eq!(v["site"], KEY);
        assert_eq!(v["hash"], false);
    }

    #[test]
    fn prefix_refuses_site_keys_outside_the_allowed_alphabet() {
        for bad in ["", "a b", "a\"b", "a;b", "a/b", "a.b", "</script>", "密钥"] {
            assert_eq!(
                tracker_prefix(bad, "https://s/api/event", false),
                None,
                "{bad:?}"
            );
        }
        assert!(tracker_prefix("A-z_09", "https://s/api/event", false).is_some());
    }

    #[test]
    fn etag_depends_on_build_prefix_and_bytes() {
        let a = etag(1, "p", b"c");
        assert_eq!(a, etag(1, "p", b"c"));
        assert_ne!(a, etag(2, "p", b"c"));
        assert_ne!(a, etag(1, "q", b"c"));
        assert_ne!(a, etag(1, "p", b"d"));
        assert_eq!(a.len(), 34, "两个引号加 32 位十六进制（16 字节）");
        assert!(a.starts_with('"') && a.ends_with('"'));
    }

    #[tokio::test]
    async fn known_site_gets_prefix_plus_core_with_the_documented_headers() {
        let svc = loaded_service();
        let r = request(router(svc), "GET", &format!("/js/{KEY}.js")).await;
        assert_eq!(r.status, 200);
        let want = format!(
            "{}{CORE}",
            tracker_prefix(KEY, "https://stats.example.com/api/event", false).unwrap()
        );
        assert_eq!(r.body, want);
        assert_eq!(
            r.header("content-type"),
            Some("application/javascript; charset=utf-8")
        );
        assert_eq!(r.header("cache-control"), Some("public, max-age=300"));
        assert_eq!(r.header("x-content-type-options"), Some("nosniff"));
        assert_eq!(
            r.header("cross-origin-resource-policy"),
            Some("cross-origin")
        );
        assert_eq!(r.header("access-control-allow-origin"), Some("*"));
        assert_eq!(r.header("set-cookie"), None);
        let etag = r.header("etag").unwrap();
        assert!(etag.starts_with('"') && etag.len() == 34, "{etag}");
    }

    #[tokio::test]
    async fn endpoint_comes_from_the_configuration_not_the_host_header() {
        let r = send(
            router(loaded_service()),
            "GET",
            &format!("/js/{KEY}.js"),
            &["Host: evil.example", "X-Forwarded-Host: evil.example"],
            b"",
        )
        .await;
        assert_eq!(r.status, 200);
        assert!(
            r.body
                .contains("\"endpoint\":\"https://stats.example.com/api/event\"")
        );
        assert!(!r.body.contains("evil.example"));
    }

    #[tokio::test]
    async fn hash_mode_comes_from_the_site_settings() {
        let r = request(
            router(loaded_service()),
            "GET",
            &format!("/js/{HASH_KEY}.js"),
        )
        .await;
        assert_eq!(r.status, 200);
        assert!(r.body.contains("\"hash\":true"));
    }

    #[tokio::test]
    async fn matching_if_none_match_is_304_without_a_body() {
        let svc = loaded_service();
        let path = format!("/js/{KEY}.js");
        let first = request(router(Arc::clone(&svc)), "GET", &path).await;
        let etag = first.header("etag").unwrap().to_owned();
        for inm in [
            format!("If-None-Match: {etag}"),
            format!("If-None-Match: W/{etag}"),
            format!("If-None-Match: \"nope\", {etag}"),
            "If-None-Match: *".to_owned(),
        ] {
            let r = send(router(Arc::clone(&svc)), "GET", &path, &[&inm], b"").await;
            assert_eq!(r.status, 304, "{inm}");
            assert_eq!(r.raw_body.len(), 0, "{inm}");
            assert_eq!(r.header("etag"), Some(etag.as_str()));
            assert_eq!(r.header("cache-control"), Some("public, max-age=300"));
        }
        let r = send(
            router(svc),
            "GET",
            &path,
            &["If-None-Match: \"something-else\""],
            b"",
        )
        .await;
        assert_eq!(r.status, 200);
    }

    #[tokio::test]
    async fn etag_changes_when_the_build_changes() {
        let a = request(router(loaded_service()), "GET", &format!("/js/{KEY}.js")).await;
        let other = service(TrackerAssets::from_parts(
            Path::new("/x"),
            CORE.as_bytes().to_vec(),
            8,
        ));
        let b = request(router(other), "GET", &format!("/js/{KEY}.js")).await;
        assert_ne!(a.header("etag"), b.header("etag"));
    }

    #[tokio::test]
    async fn head_has_the_headers_and_no_body() {
        let r = request(router(loaded_service()), "HEAD", &format!("/js/{KEY}.js")).await;
        assert_eq!(r.status, 200);
        assert!(r.header("etag").is_some());
        assert_eq!(r.raw_body.len(), 0);
    }

    #[tokio::test]
    async fn unknown_and_disabled_sites_are_404_no_store_with_an_empty_body() {
        for path in [
            "/js/zzzzzzzzzzzz.js",        // 未知（含已删除：不在快照里）
            &format!("/js/{OFF_KEY}.js"), // 已停用
            "/js/.js",
            "/js/t.js",
            "/js/t.7.js",
            &format!("/js/{KEY}"),
            "/js/a%20b.js",
        ] {
            let r = request(router(loaded_service()), "GET", path).await;
            assert_eq!(r.status, 404, "{path}");
            assert_eq!(r.body, "", "{path}");
            assert_eq!(r.header("cache-control"), Some("no-store"), "{path}");
            assert_eq!(r.header("etag"), None, "{path}");
        }
    }

    #[tokio::test]
    async fn other_methods_are_not_allowed() {
        for m in ["POST", "PUT", "DELETE"] {
            let r = request(router(loaded_service()), m, &format!("/js/{KEY}.js")).await;
            assert_eq!(r.status, 405, "{m}");
        }
    }

    #[tokio::test]
    async fn missing_build_output_is_503_for_known_sites_and_still_404_for_unknown_ones() {
        let dir = std::env::temp_dir().join(format!("rivu-no-such-tracker-{}", std::process::id()));
        let svc = service(TrackerAssets::load(&dir));
        assert!(!svc.assets.is_loaded());
        let r = request(router(Arc::clone(&svc)), "GET", &format!("/js/{KEY}.js")).await;
        assert_eq!(r.status, 503);
        assert_eq!(r.header("cache-control"), Some("no-store"));
        assert!(r.header("retry-after").is_some());
        let r = request(router(svc), "GET", "/js/zzzzzzzzzzzz.js").await;
        assert_eq!(r.status, 404);
    }

    #[test]
    fn assets_are_loaded_from_a_directory_and_validated() {
        let dir = std::env::temp_dir().join(format!("rivu-tracker-assets-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        // 空目录：不可用，状态里有原因，但 ok 仍为 true（不影响就绪）
        let a = TrackerAssets::load(&dir);
        assert!(!a.is_loaded());
        let s = a.status();
        assert_eq!(
            (s["ok"].as_bool(), s["loaded"].as_bool()),
            (Some(true), Some(false))
        );
        assert!(s["error"].as_str().unwrap().contains("core.js"));

        // 只有 core.js：缺 meta.json（没有构建号，不能当产物用）
        std::fs::write(dir.join("core.js"), CORE).unwrap();
        assert!(
            TrackerAssets::load(&dir).status()["error"]
                .as_str()
                .unwrap()
                .contains("meta.json")
        );
        // meta.json 无效
        for bad in [
            "not json",
            "{}",
            "{\"sv\":\"x\"}",
            "{\"sv\":-1}",
            "{\"sv\":99999999999}",
        ] {
            std::fs::write(dir.join("meta.json"), bad).unwrap();
            assert!(!TrackerAssets::load(&dir).is_loaded(), "{bad}");
        }
        // 空的 core.js
        std::fs::write(dir.join("meta.json"), "{\"sv\":42}\n").unwrap();
        std::fs::write(dir.join("core.js"), "").unwrap();
        assert!(!TrackerAssets::load(&dir).is_loaded());

        // 正常
        std::fs::write(dir.join("core.js"), CORE).unwrap();
        let a = TrackerAssets::load(&dir);
        assert!(a.is_loaded());
        let s = a.status();
        assert_eq!(s["sv"], 42);
        assert_eq!(s["core_bytes"], CORE.len());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
