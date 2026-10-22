//! 本地 HTTP/1.1 测试服务器：每个连接一个请求，响应完毕即关闭。
//!
//! 自己实现而不用 axum/hyper，是因为要能撒谎：声明一个根本不会发送的 `Content-Length`，
//! 或者无限发送 chunked 数据，直到客户端断开。
#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;

#[derive(Debug, Clone)]
pub struct Req {
    pub method: String,
    /// 路径加查询串
    pub target: String,
    /// 头名小写
    pub headers: HashMap<String, String>,
}

impl Req {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).map(String::as_str)
    }
}

pub enum Reply {
    Full {
        status: u16,
        headers: Vec<(String, String)>,
        body: Vec<u8>,
    },
    /// 声明 `declared` 字节的 Content-Length，只发 `body`，然后挂着不动，直到客户端断开。
    LyingLength { declared: u64, body: Vec<u8> },
    /// chunked 编码，反复发送 `chunk`，直到写失败（客户端断开）或累计 `cap` 字节（防止测试失控）。
    ChunkedForever { chunk: Vec<u8>, cap: u64 },
    /// 接受连接后什么都不回。
    Stall,
}

impl Reply {
    pub fn ok(body: impl Into<Vec<u8>>) -> Self {
        Reply::Full {
            status: 200,
            headers: Vec::new(),
            body: body.into(),
        }
    }

    pub fn status(status: u16) -> Self {
        Reply::Full {
            status,
            headers: Vec::new(),
            body: Vec::new(),
        }
    }

    pub fn redirect(location: impl Into<String>) -> Self {
        Reply::Full {
            status: 302,
            headers: vec![("Location".into(), location.into())],
            body: Vec::new(),
        }
    }

    pub fn with_header(mut self, name: &str, value: &str) -> Self {
        if let Reply::Full { headers, .. } = &mut self {
            headers.push((name.into(), value.into()));
        }
        self
    }
}

pub struct Server {
    pub addr: std::net::SocketAddr,
    pub requests: Arc<Mutex<Vec<Req>>>,
    task: JoinHandle<()>,
}

impl Server {
    pub fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.addr)
    }

    pub fn requests(&self) -> Vec<Req> {
        self.requests.lock().unwrap().clone()
    }

    /// 已收到的请求目标（路径加查询串）。
    pub fn targets(&self) -> Vec<String> {
        self.requests().into_iter().map(|r| r.target).collect()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}

pub async fn start<F>(handler: F) -> Server
where
    F: Fn(&Req) -> Reply + Send + Sync + 'static,
{
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let handler = Arc::new(handler);
    let log = Arc::clone(&requests);
    let task = tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };
            let (handler, log) = (Arc::clone(&handler), Arc::clone(&log));
            tokio::spawn(async move {
                let _ = serve(stream, &*handler, &log).await;
            });
        }
    });
    Server {
        addr,
        requests,
        task,
    }
}

async fn read_request(stream: &mut TcpStream) -> std::io::Result<Req> {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 2048];
    loop {
        let n = stream.read(&mut tmp).await?;
        if n == 0 {
            return Err(std::io::ErrorKind::UnexpectedEof.into());
        }
        buf.extend_from_slice(&tmp[..n]);
        if buf.windows(4).any(|w| w == b"\r\n\r\n") {
            break;
        }
        if buf.len() > 65_536 {
            return Err(std::io::ErrorKind::InvalidData.into());
        }
    }
    let text = String::from_utf8_lossy(&buf).into_owned();
    let mut lines = text.split("\r\n");
    let mut first = lines.next().unwrap_or_default().split(' ');
    let (method, target) = (
        first.next().unwrap_or_default().to_string(),
        first.next().unwrap_or_default().to_string(),
    );
    let headers = lines
        .take_while(|l| !l.is_empty())
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string()))
        .collect();
    Ok(Req {
        method,
        target,
        headers,
    })
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        302 => "Found",
        304 => "Not Modified",
        401 => "Unauthorized",
        404 => "Not Found",
        500 => "Internal Server Error",
        _ => "Status",
    }
}

async fn serve<F>(mut stream: TcpStream, handler: &F, log: &Mutex<Vec<Req>>) -> std::io::Result<()>
where
    F: Fn(&Req) -> Reply,
{
    let req = read_request(&mut stream).await?;
    log.lock().unwrap().push(req.clone());
    match handler(&req) {
        Reply::Full {
            status,
            headers,
            body,
        } => {
            let mut head = format!(
                "HTTP/1.1 {status} {}\r\nConnection: close\r\n",
                reason(status)
            );
            for (k, v) in &headers {
                head.push_str(&format!("{k}: {v}\r\n"));
            }
            if status != 304 {
                head.push_str(&format!("Content-Length: {}\r\n", body.len()));
            }
            head.push_str("\r\n");
            stream.write_all(head.as_bytes()).await?;
            stream.write_all(&body).await?;
            stream.shutdown().await
        }
        Reply::LyingLength { declared, body } => {
            let head = format!(
                "HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: {declared}\r\n\r\n"
            );
            stream.write_all(head.as_bytes()).await?;
            stream.write_all(&body).await?;
            hold_until_closed(&mut stream).await
        }
        Reply::ChunkedForever { chunk, cap } => {
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nConnection: close\r\nTransfer-Encoding: chunked\r\n\r\n",
                )
                .await?;
            let mut sent = 0u64;
            while sent < cap {
                stream
                    .write_all(format!("{:x}\r\n", chunk.len()).as_bytes())
                    .await?;
                stream.write_all(&chunk).await?;
                stream.write_all(b"\r\n").await?;
                sent += chunk.len() as u64;
            }
            Ok(())
        }
        Reply::Stall => hold_until_closed(&mut stream).await,
    }
}

/// 不再写任何东西，等客户端关闭（最多 30 秒，防止测试挂死）。
async fn hold_until_closed(stream: &mut TcpStream) -> std::io::Result<()> {
    let mut buf = [0u8; 64];
    let _ = tokio::time::timeout(Duration::from_secs(30), async {
        while matches!(stream.read(&mut buf).await, Ok(n) if n > 0) {}
    })
    .await;
    Ok(())
}
