//! 公共端口（`RIVU_LISTEN_ADDR`）的路由。P0 阶段所有业务路由都返回 501，P1 起实现。

use axum::Router;
use axum::http::StatusCode;
use axum::response::Json;
use axum::routing::{any, post};
use rivu_common::config::Role;
use serde_json::{Value, json};

pub fn router(role: Role) -> Router {
    let mut router = Router::new();
    if role.serves_collector() {
        router = router.route("/api/event", post(event_not_implemented));
    }
    if role.serves_api() {
        router = router.route("/api/{*rest}", any(api_not_implemented));
    }
    router
}

fn not_implemented(what: &str) -> (StatusCode, Json<Value>) {
    (
        StatusCode::NOT_IMPLEMENTED,
        Json(json!({
            "error": "not_implemented",
            "message": format!("{what} 尚未实现，计划在 P1 实现"),
            "phase": "P1",
        })),
    )
}

async fn event_not_implemented() -> (StatusCode, Json<Value>) {
    not_implemented("POST /api/event")
}

async fn api_not_implemented() -> (StatusCode, Json<Value>) {
    not_implemented("/api/*")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};

    /// 起一个真实监听端口，发一个最小的 HTTP/1.1 请求，返回 (状态码, 响应体)。
    async fn request(router: Router, method: &str, path: &str) -> (u16, String) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });

        let mut stream = TcpStream::connect(addr).await.unwrap();
        let req = format!(
            "{method} {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nContent-Length: 0\r\n\r\n"
        );
        stream.write_all(req.as_bytes()).await.unwrap();
        let mut raw = String::new();
        stream.read_to_string(&mut raw).await.unwrap();
        let status = raw.split_whitespace().nth(1).unwrap().parse().unwrap();
        let body = raw.split("\r\n\r\n").nth(1).unwrap_or_default().to_string();
        (status, body)
    }

    #[tokio::test]
    async fn collector_serves_event_endpoint_only() {
        let (status, body) = request(router(Role::Collector), "POST", "/api/event").await;
        assert_eq!(status, 501);
        assert!(body.contains("\"phase\":\"P1\""), "{body}");
        let (status, _) = request(router(Role::Collector), "GET", "/api/stats").await;
        assert_eq!(status, 404);
    }

    #[tokio::test]
    async fn api_serves_api_prefix_only() {
        let (status, _) = request(router(Role::Api), "GET", "/api/stats/overview").await;
        assert_eq!(status, 501);
        let (status, _) = request(router(Role::Api), "POST", "/api/event").await;
        // api 角色没有 /api/event 的专用路由，会落到 /api/* 的通配上
        assert_eq!(status, 501);
        let (status, _) = request(router(Role::Api), "GET", "/other").await;
        assert_eq!(status, 404);
    }

    #[tokio::test]
    async fn all_serves_both() {
        let (status, _) = request(router(Role::All), "POST", "/api/event").await;
        assert_eq!(status, 501);
        let (status, _) = request(router(Role::All), "GET", "/api/anything").await;
        assert_eq!(status, 501);
    }
}
