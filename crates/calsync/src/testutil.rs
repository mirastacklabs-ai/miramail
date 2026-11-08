//! Tiny in-process HTTP/1.1 mock server on 127.0.0.1 (tests only).

use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

#[derive(Debug, Clone)]
pub struct Req {
    pub method: String,
    pub path: String,
    pub headers: Vec<(String, String)>,
    pub body: String,
}

impl Req {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
    }
}

#[derive(Debug, Clone)]
pub struct Resp {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: String,
}

impl Resp {
    pub fn new(status: u16, body: &str) -> Self {
        Resp { status, headers: vec![], body: body.into() }
    }
    pub fn with(mut self, k: &str, v: &str) -> Self {
        self.headers.push((k.into(), v.into()));
        self
    }
}

pub type Handler = dyn Fn(&Req) -> Resp + Send + Sync;

pub struct Mock {
    pub port: u16,
    pub log: Arc<Mutex<Vec<Req>>>,
}

impl Mock {
    pub fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{}", self.port, path)
    }
    pub fn requests(&self) -> Vec<Req> {
        self.log.lock().unwrap().clone()
    }
}

pub async fn serve(handler: impl Fn(&Req) -> Resp + Send + Sync + 'static) -> Mock {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let log = Arc::new(Mutex::new(Vec::new()));
    let handler: Arc<Handler> = Arc::new(handler);
    let lg = log.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut s, _)) = listener.accept().await else { break };
            let (h, lg) = (handler.clone(), lg.clone());
            tokio::spawn(async move {
                let mut buf = Vec::new();
                let mut tmp = [0u8; 4096];
                let head_end = loop {
                    if let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                        break p;
                    }
                    match s.read(&mut tmp).await {
                        Ok(0) | Err(_) => return,
                        Ok(n) => buf.extend_from_slice(&tmp[..n]),
                    }
                };
                let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
                let mut lines = head.split("\r\n");
                let mut first = lines.next().unwrap_or("").split(' ');
                let method = first.next().unwrap_or("").to_string();
                let path = first.next().unwrap_or("").to_string();
                let headers: Vec<(String, String)> = lines
                    .filter_map(|l| l.split_once(':').map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string())))
                    .collect();
                let clen: usize = headers.iter().find(|(k, _)| k == "content-length").and_then(|(_, v)| v.parse().ok()).unwrap_or(0);
                let mut body = buf[head_end + 4..].to_vec();
                while body.len() < clen {
                    match s.read(&mut tmp).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => body.extend_from_slice(&tmp[..n]),
                    }
                }
                let req = Req { method, path, headers, body: String::from_utf8_lossy(&body).to_string() };
                lg.lock().unwrap().push(req.clone());
                let r = h(&req);
                let mut out = format!("HTTP/1.1 {} X\r\nContent-Length: {}\r\nConnection: close\r\n", r.status, r.body.len());
                for (k, v) in &r.headers {
                    out.push_str(&format!("{k}: {v}\r\n"));
                }
                out.push_str("\r\n");
                out.push_str(&r.body);
                let _ = s.write_all(out.as_bytes()).await;
                let _ = s.shutdown().await;
            });
        }
    });
    Mock { port, log }
}
