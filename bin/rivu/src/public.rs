//! 公共端口（`RIVU_LISTEN_ADDR`）的路由。
//!
//! collector / all 角色实现 `POST /api/event` 与 `OPTIONS /api/event`，严格照
//! `docs/architecture/tracker-and-protocol.md` §6.1–§6.3：
//!
//! | 情况 | 响应 |
//! |---|---|
//! | 接受 | `202` 空体 |
//! | 被过滤 | `202` 空体 + `x-rivu-dropped: 1`（原因不暴露，只进 `ingest_counters`） |
//! | 协议错误 | `400` + `{"errors":{"字段":["消息"]}}` |
//! | 请求体 > 8192 字节 | `413` + `{"errors":{"body":["is too large (max 8192 bytes)"]}}` + `Connection: close` |
//! | 写缓冲满 | `503` 空体 + `Retry-After: 1–10` |
//! | `OPTIONS` | `204` + CORS 预检头 |
//! | 其他方法 | `405` + `Allow: POST, OPTIONS` |
//!
//! 所有响应都带 `Access-Control-Allow-Origin: *`、`Access-Control-Expose-Headers`、`Cache-Control: no-store`、
//! `Cross-Origin-Resource-Policy: cross-origin`；不带 `Allow-Credentials`，也不设 cookie（请求里的 `Cookie` 被忽略）。
//! 其余 `/api/*`（api 角色）P1 仍返回 `501`。
//!
//! collector / all 角色另有 `GET /js/<site_key>.js`（站点专属脚本，实现在 [`crate::script`]）。

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::extract::{ConnectInfo, Request, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Json, Response};
use axum::routing::{any, post};
use rivu_common::config::Role;
use rivu_ingest::pipeline::{Outcome, Pipeline, RequestCtx};
use rivu_ingest::protocol::MAX_BODY_BYTES;
use serde_json::{Value, json};

use crate::script::{self, ScriptService};

/// 读请求体的超时（§6.1：读体超时 5 s）。
const BODY_READ_TIMEOUT: Duration = Duration::from_secs(5);

const X_RIVU_DROPPED: HeaderName = HeaderName::from_static("x-rivu-dropped");

/// 公共端口的路由（不含站点脚本）。`pipeline` 只在 collector / all 角色有值。
pub fn router(role: Role, pipeline: Option<Arc<Pipeline>>) -> Router {
    router_with_scripts(role, pipeline, None)
}

/// 公共端口的路由。`pipeline` 与 `scripts` 只在 collector / all 角色有值；
/// `scripts` 提供 `GET /js/<site_key>.js`（`tracker-and-protocol.md` §2.3、§2.4）。
pub fn router_with_scripts(
    role: Role,
    pipeline: Option<Arc<Pipeline>>,
    scripts: Option<Arc<ScriptService>>,
) -> Router {
    let mut router = Router::new();
    if role.serves_collector()
        && let Some(scripts) = scripts
    {
        router = router.merge(script::router(scripts));
    }
    if role.serves_collector() {
        let collector = Router::new()
            .route(
                "/api/event",
                post(event).options(preflight).fallback(method_not_allowed),
            )
            .with_state(pipeline);
        router = router.merge(collector);
    }
    if role.serves_api() {
        router = router.route("/api/{*rest}", any(api_not_implemented));
    }
    router
}

/// 所有响应共有的头（§6.1「所有响应」）。
fn with_common_headers(mut resp: Response) -> Response {
    let h = resp.headers_mut();
    h.insert(
        header::ACCESS_CONTROL_ALLOW_ORIGIN,
        HeaderValue::from_static("*"),
    );
    h.insert(
        header::ACCESS_CONTROL_EXPOSE_HEADERS,
        HeaderValue::from_static("x-rivu-dropped, Retry-After"),
    );
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    h.insert(
        HeaderName::from_static("cross-origin-resource-policy"),
        HeaderValue::from_static("cross-origin"),
    );
    resp
}

fn empty(status: StatusCode) -> Response {
    with_common_headers(status.into_response())
}

fn json_error(status: StatusCode, body: &Value) -> Response {
    with_common_headers((status, Json(body)).into_response())
}

fn too_large() -> Response {
    let mut resp = json_error(
        StatusCode::PAYLOAD_TOO_LARGE,
        &json!({"errors": {"body": [format!("is too large (max {MAX_BODY_BYTES} bytes)")]}}),
    );
    resp.headers_mut()
        .insert(header::CONNECTION, HeaderValue::from_static("close"));
    resp
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

fn header_str(headers: &HeaderMap, name: header::HeaderName) -> String {
    headers
        .get(name)
        .map(|v| String::from_utf8_lossy(v.as_bytes()).into_owned())
        .unwrap_or_default()
}

/// 所有 `X-Forwarded-For` 头值，按出现顺序；不是有效文本的丢弃（客户端 IP 判定对解析失败回退对端地址）。
fn forwarded_for(headers: &HeaderMap) -> Vec<String> {
    headers
        .get_all("x-forwarded-for")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .map(str::to_owned)
        .collect()
}

async fn event(
    State(pipeline): State<Option<Arc<Pipeline>>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    req: Request,
) -> Response {
    let Some(pipeline) = pipeline else {
        // collector 角色总会带管线；走到这里说明装配出了问题。
        tracing::error!("collector 路由没有接入管线");
        return empty(StatusCode::INTERNAL_SERVER_ERROR);
    };

    // 先看 Content-Length：声明超限直接 413，不读体。
    let declared = req
        .headers()
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<u64>().ok());
    if declared.is_some_and(|n| n > MAX_BODY_BYTES as u64) {
        return too_large();
    }

    let user_agent = header_str(req.headers(), header::USER_AGENT);
    let xff = forwarded_for(req.headers());
    // 分块传输按流式累计：超过上限即停止读取（多读 1 字节用来判断「超限」）。
    let body: Body = req.into_body();
    let bytes = match tokio::time::timeout(BODY_READ_TIMEOUT, to_bytes(body, MAX_BODY_BYTES)).await
    {
        Ok(Ok(bytes)) => bytes,
        Ok(Err(_)) => return too_large(),
        Err(_) => {
            let mut resp = empty(StatusCode::REQUEST_TIMEOUT);
            resp.headers_mut()
                .insert(header::CONNECTION, HeaderValue::from_static("close"));
            return resp;
        }
    };

    let outcome = pipeline.process(
        &bytes,
        RequestCtx {
            peer: peer.ip(),
            xff,
            user_agent,
            now_ms: now_ms(),
        },
    );
    match outcome {
        Outcome::Accepted => empty(StatusCode::ACCEPTED),
        Outcome::Dropped(_) => {
            let mut resp = empty(StatusCode::ACCEPTED);
            resp.headers_mut()
                .insert(X_RIVU_DROPPED, HeaderValue::from_static("1"));
            resp
        }
        Outcome::Rejected(errors) => json_error(StatusCode::BAD_REQUEST, &errors.to_json()),
        Outcome::Overloaded => {
            let mut resp = empty(StatusCode::SERVICE_UNAVAILABLE);
            let secs: u8 = rand::random_range(1..=10);
            resp.headers_mut()
                .insert(header::RETRY_AFTER, HeaderValue::from(u16::from(secs)));
            resp
        }
    }
}

async fn preflight() -> Response {
    let mut resp = empty(StatusCode::NO_CONTENT);
    let h = resp.headers_mut();
    h.insert(
        header::ACCESS_CONTROL_ALLOW_METHODS,
        HeaderValue::from_static("POST, OPTIONS"),
    );
    h.insert(
        header::ACCESS_CONTROL_ALLOW_HEADERS,
        HeaderValue::from_static("Content-Type"),
    );
    h.insert(
        header::ACCESS_CONTROL_MAX_AGE,
        HeaderValue::from_static("86400"),
    );
    resp
}

async fn method_not_allowed() -> Response {
    let mut resp = empty(StatusCode::METHOD_NOT_ALLOWED);
    resp.headers_mut()
        .insert(header::ALLOW, HeaderValue::from_static("POST, OPTIONS"));
    resp
}

async fn api_not_implemented() -> (StatusCode, Json<Value>) {
    (
        StatusCode::NOT_IMPLEMENTED,
        Json(json!({
            "error": "not_implemented",
            "message": "/api/* 尚未实现，计划在 P2 实现；P1 用 `rivu stats` 命令行查询",
            "phase": "P2",
        })),
    )
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use rivu_common::config::Cidr;
    use rivu_geo::{GeoDb, GeoLevel};
    use rivu_ingest::pipeline::{SaltCache, SiteConfig, SiteSnapshot, utc_today};
    use rivu_ua::UaParser;
    use rivu_writer::WriterHandle;

    use super::*;
    use crate::testutil::{Resp, request, send};

    const KEY: &str = "k7qm2xv9atbr";
    /// 没有 `User-Agent` 头的请求是机器人（空 UA），所以测试请求要带一个真实浏览器的 UA。
    const BROWSER_UA: &str = "User-Agent: Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36";

    /// 一条管线：一个 UTC 站点，白名单 example.test，盐覆盖今天前后。
    fn collector(
        capacity: usize,
    ) -> (
        Arc<Pipeline>,
        WriterHandle,
        tokio::sync::mpsc::Receiver<rivu_storage_ch::rows::EventRow>,
    ) {
        let sites = Arc::new(SiteSnapshot::new());
        sites.replace(HashMap::from([(
            KEY.to_owned(),
            Arc::new(SiteConfig {
                site_id: 7,
                timezone: jiff::tz::TimeZone::UTC,
                collect_enabled: true,
                hostnames: rivu_ingest::hostname::HostnamePatterns::new(["example.test"]),
                geo_level: GeoLevel::City,
                tracker_hash: false,
            }),
        )]));
        let salts = Arc::new(SaltCache::new());
        let today = utc_today();
        salts.replace(
            (-2i64..=2)
                .map(|d| {
                    (
                        today.saturating_add(jiff::Span::new().days(d)),
                        [d as u8; 16],
                    )
                })
                .collect(),
        );
        let (writer, rx) = WriterHandle::detached(capacity);
        let trusted: Vec<Cidr> = vec!["10.0.0.0/8".parse().unwrap()];
        (
            Arc::new(Pipeline::new(
                sites,
                salts,
                writer.clone(),
                trusted,
                Arc::new(UaParser::embedded().expect("uap-core")),
                Arc::new(GeoDb::new()),
            )),
            writer,
            rx,
        )
    }

    fn pageview_body(key: &str, host: &str) -> Vec<u8> {
        json!({"v": 1, "s": key, "n": "pageview", "u": format!("https://{host}/")})
            .to_string()
            .into_bytes()
    }

    async fn post_event(p: &Arc<Pipeline>, body: &[u8]) -> Resp {
        let len = format!("Content-Length: {}", body.len());
        send(
            router(Role::Collector, Some(Arc::clone(p))),
            "POST",
            "/api/event",
            &["Content-Type: text/plain;charset=UTF-8", BROWSER_UA, &len],
            body,
        )
        .await
    }

    fn assert_common_headers(r: &Resp) {
        assert_eq!(r.header("access-control-allow-origin"), Some("*"));
        assert_eq!(
            r.header("access-control-expose-headers"),
            Some("x-rivu-dropped, Retry-After")
        );
        assert_eq!(r.header("cache-control"), Some("no-store"));
        assert_eq!(
            r.header("cross-origin-resource-policy"),
            Some("cross-origin")
        );
        assert_eq!(r.header("access-control-allow-credentials"), None);
        assert_eq!(r.header("set-cookie"), None);
    }

    #[tokio::test]
    async fn accepted_event_is_202_with_no_body_and_no_cookie() {
        let (p, _w, mut rx) = collector(10);
        let body = pageview_body(KEY, "example.test");
        let len = format!("Content-Length: {}", body.len());
        let r = send(
            router(Role::Collector, Some(Arc::clone(&p))),
            "POST",
            "/api/event",
            &[
                "Content-Type: text/plain;charset=UTF-8",
                BROWSER_UA,
                "Cookie: session=abc",
                &len,
            ],
            &body,
        )
        .await;
        assert_eq!(r.status, 202);
        assert_eq!(r.body, "");
        assert_eq!(r.header("x-rivu-dropped"), None);
        assert_common_headers(&r);
        assert_eq!(rx.try_recv().unwrap().site_id, 7);
    }
    #[tokio::test]
    async fn filtered_event_is_202_with_dropped_header() {
        let (p, w, mut rx) = collector(10);
        for body in [
            pageview_body("zzzzzzzzzzzz", "example.test"), // 未知站点
            pageview_body(KEY, "other.test"),              // hostname 不在白名单
        ] {
            let r = post_event(&p, &body).await;
            assert_eq!(r.status, 202);
            assert_eq!(r.body, "");
            assert_eq!(r.header("x-rivu-dropped"), Some("1"));
            assert_common_headers(&r);
        }
        assert!(rx.try_recv().is_err());
        // 原因只进计数，不出现在响应里
        let metrics: Vec<String> = w
            .counters()
            .take_all()
            .into_iter()
            .map(|r| r.metric)
            .collect();
        assert!(metrics.contains(&"dropped_unknown_site".to_owned()));
        assert!(metrics.contains(&"dropped_hostname_not_allowed".to_owned()));
    }

    #[tokio::test]
    async fn protocol_errors_are_400_json() {
        let (p, _w, _rx) = collector(10);
        let r = post_event(&p, b"not json").await;
        assert_eq!(r.status, 400);
        assert_eq!(r.header("content-type"), Some("application/json"));
        let v: Value = serde_json::from_str(&r.body).unwrap();
        assert_eq!(v, json!({"errors": {"body": ["is not valid JSON"]}}));
        assert_common_headers(&r);

        let r = post_event(&p, br#"{"v":1,"n":"pageview"}"#).await;
        assert_eq!(r.status, 400);
        let v: Value = serde_json::from_str(&r.body).unwrap();
        assert_eq!(v["errors"]["s"], json!(["is required"]));
        assert_eq!(v["errors"]["u"], json!(["is required"]));
    }

    #[tokio::test]
    async fn oversized_declared_body_is_413_and_connection_close() {
        let (p, _w, _rx) = collector(10);
        // 只声明、不发送：服务端看到 Content-Length 就回 413，不会等待请求体
        let r = send(
            router(Role::Collector, Some(p)),
            "POST",
            "/api/event",
            &["Content-Length: 8193"],
            b"",
        )
        .await;
        assert_eq!(r.status, 413);
        let v: Value = serde_json::from_str(&r.body).unwrap();
        assert_eq!(
            v,
            json!({"errors": {"body": ["is too large (max 8192 bytes)"]}})
        );
        assert_eq!(r.header("connection"), Some("close"));
        assert_common_headers(&r);
    }

    #[tokio::test]
    async fn body_at_the_limit_is_still_parsed() {
        let (p, _w, _rx) = collector(10);
        // 恰好 8192 字节的合法 JSON：不是 413（这里 `p` 里的值会让它 400，但绝不是 413）
        let pad = "x".repeat(8192 - r#"{"v":1,"pad":""}"#.len());
        let body = format!(r#"{{"v":1,"pad":"{pad}"}}"#);
        assert_eq!(body.len(), 8192);
        let r = post_event(&p, body.as_bytes()).await;
        assert_eq!(r.status, 400);
    }

    #[tokio::test]
    async fn chunked_body_over_the_limit_is_413() {
        let (p, _w, _rx) = collector(10);
        let big = vec![b'a'; 8193];
        let mut chunked = format!("{:x}\r\n", big.len()).into_bytes();
        chunked.extend_from_slice(&big);
        chunked.extend_from_slice(b"\r\n0\r\n\r\n");
        let r = send(
            router(Role::Collector, Some(p)),
            "POST",
            "/api/event",
            &["Transfer-Encoding: chunked"],
            &chunked,
        )
        .await;
        assert_eq!(r.status, 413);
        assert_eq!(r.header("connection"), Some("close"));
    }

    #[tokio::test]
    async fn full_buffer_is_503_with_retry_after() {
        let (p, _w, _rx) = collector(1);
        assert_eq!(
            post_event(&p, &pageview_body(KEY, "example.test"))
                .await
                .status,
            202
        );
        let r = post_event(&p, &pageview_body(KEY, "example.test")).await;
        assert_eq!(r.status, 503);
        assert_eq!(r.body, "");
        let secs: u8 = r.header("retry-after").unwrap().parse().unwrap();
        assert!((1..=10).contains(&secs), "{secs}");
        assert_common_headers(&r);
    }

    #[tokio::test]
    async fn options_is_a_204_preflight() {
        let (p, _w, _rx) = collector(1);
        let r = request(router(Role::Collector, Some(p)), "OPTIONS", "/api/event").await;
        assert_eq!(r.status, 204);
        assert_eq!(
            r.header("access-control-allow-methods"),
            Some("POST, OPTIONS")
        );
        assert_eq!(
            r.header("access-control-allow-headers"),
            Some("Content-Type")
        );
        assert_eq!(r.header("access-control-max-age"), Some("86400"));
        assert_common_headers(&r);
    }

    #[tokio::test]
    async fn other_methods_are_405_with_allow() {
        let (p, _w, _rx) = collector(1);
        for method in ["GET", "PUT", "DELETE", "HEAD"] {
            let r = request(
                router(Role::Collector, Some(Arc::clone(&p))),
                method,
                "/api/event",
            )
            .await;
            assert_eq!(r.status, 405, "{method}");
            assert_eq!(r.header("allow"), Some("POST, OPTIONS"), "{method}");
            assert_common_headers(&r);
        }
    }

    #[tokio::test]
    async fn collector_serves_event_endpoint_only() {
        let (p, _w, _rx) = collector(1);
        let r = request(
            router(Role::Collector, Some(Arc::clone(&p))),
            "GET",
            "/api/stats",
        )
        .await;
        assert_eq!(r.status, 404);
        let r = request(router(Role::Collector, Some(p)), "GET", "/").await;
        assert_eq!(r.status, 404);
    }

    #[tokio::test]
    async fn api_serves_api_prefix_only() {
        let r = request(router(Role::Api, None), "GET", "/api/stats/overview").await;
        assert_eq!(r.status, 501);
        assert!(r.body.contains("\"phase\":\"P2\""), "{}", r.body);
        let r = request(router(Role::Api, None), "POST", "/api/event").await;
        // api 角色没有 /api/event 的专用路由，会落到 /api/* 的通配上
        assert_eq!(r.status, 501);
        let r = request(router(Role::Api, None), "GET", "/other").await;
        assert_eq!(r.status, 404);
    }

    #[tokio::test]
    async fn all_serves_both() {
        let (p, _w, _rx) = collector(1);
        let r = post_event(&p, &pageview_body(KEY, "example.test")).await;
        assert_eq!(r.status, 202);
        let r = request(
            router(Role::All, Some(Arc::clone(&p))),
            "GET",
            "/api/anything",
        )
        .await;
        assert_eq!(r.status, 501);
        // all 角色的 /api/event 走 collector 的专用路由，不是 501
        let r = request(router(Role::All, Some(p)), "OPTIONS", "/api/event").await;
        assert_eq!(r.status, 204);
    }
}
