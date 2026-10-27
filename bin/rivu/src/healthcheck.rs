//! `rivu healthcheck`：给容器编排用的健康检查（`HEALTHCHECK`、compose 的 `healthcheck`）。
//!
//! 生产镜像里没有 curl：这里直接对内部端口发一个最小的 HTTP/1.0 GET，状态行是 200 就退出 0，
//! 其他状态、连接失败、超时都退出 1，并把原因写到 stderr。不加载、不校验配置（检查每几秒执行一次）。

use std::process::ExitCode;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::cli::HealthcheckArgs;

const TIMEOUT: Duration = Duration::from_secs(5);

/// 检查的目标地址：参数优先，其次 `RIVU_INTERNAL_ADDR`（监听全部地址时改连本机），最后默认端口。
fn target(args: &HealthcheckArgs) -> String {
    if let Some(a) = &args.addr {
        return a.clone();
    }
    match std::env::var("RIVU_INTERNAL_ADDR") {
        Ok(a) if !a.is_empty() => a
            .replacen("0.0.0.0:", "127.0.0.1:", 1)
            .replacen("[::]:", "[::1]:", 1),
        _ => "127.0.0.1:9090".to_owned(),
    }
}

async fn check(addr: &str, path: &str) -> Result<u16, String> {
    let mut stream = TcpStream::connect(addr)
        .await
        .map_err(|e| format!("连接 {addr} 失败: {e}"))?;
    let req = format!("GET {path} HTTP/1.0\r\nHost: {addr}\r\nConnection: close\r\n\r\n");
    stream
        .write_all(req.as_bytes())
        .await
        .map_err(|e| format!("发送请求失败: {e}"))?;
    // 只需要状态行
    let mut buf = [0u8; 64];
    let n = stream
        .read(&mut buf)
        .await
        .map_err(|e| format!("读取响应失败: {e}"))?;
    let head = String::from_utf8_lossy(&buf[..n]);
    head.split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| format!("无法解析响应状态行: {head:?}"))
}

pub async fn run(args: &HealthcheckArgs) -> ExitCode {
    let addr = target(args);
    match tokio::time::timeout(TIMEOUT, check(&addr, &args.path)).await {
        Ok(Ok(200)) => ExitCode::SUCCESS,
        Ok(Ok(status)) => {
            eprintln!("{addr}{}: HTTP {status}", args.path);
            ExitCode::FAILURE
        }
        Ok(Err(e)) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
        Err(_) => {
            eprintln!("{addr}{}: {} 秒内没有响应", args.path, TIMEOUT.as_secs());
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;

    async fn server(status: &'static str) -> String {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap().to_string();
        tokio::spawn(async move {
            let (mut s, _) = l.accept().await.unwrap();
            let mut buf = [0u8; 256];
            let _ = s.read(&mut buf).await;
            let _ = s
                .write_all(format!("HTTP/1.1 {status}\r\n\r\n").as_bytes())
                .await;
        });
        addr
    }

    #[tokio::test]
    async fn status_line_decides_the_result() {
        let addr = server("200 OK").await;
        assert_eq!(check(&addr, "/readyz").await, Ok(200));
        let addr = server("503 Service Unavailable").await;
        assert_eq!(check(&addr, "/readyz").await, Ok(503));
        // 没有人监听的端口
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let free = l.local_addr().unwrap().to_string();
        drop(l);
        assert!(check(&free, "/readyz").await.is_err());
    }

    #[test]
    fn target_prefers_argument_then_env() {
        let a = HealthcheckArgs {
            addr: Some("10.0.0.1:9090".into()),
            path: "/readyz".into(),
        };
        assert_eq!(target(&a), "10.0.0.1:9090");
    }
}
