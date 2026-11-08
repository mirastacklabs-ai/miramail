//! Hand-rolled HTTP/1.1 mock LLM server for offline tests (loopback only, tokio only).
//!
//! Include from another integration test with:
//! ```ignore
//! mod support;               // tests/support/mod.rs  (same crate: crates/aichat/tests/*.rs)
//! use support::{Mock, Script};
//! ```
//! From a different crate, use `#[path = "../../aichat/tests/support/mod.rs"] mod support;`.
//!
//! `Mock::start(scripts)` binds 127.0.0.1:0. Connection N is answered with script N (the last
//! script repeats). Every request (method, path, lower-cased headers, body) is recorded. All
//! responses are `Connection: close`; the server never reads more than a request's declared body.
#![allow(dead_code)]

pub mod actions;
pub mod cal;
pub mod drafts;

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

#[derive(Debug, Clone)]
pub struct Recorded {
    pub method: String,
    pub path: String,
    /// Header names lower-cased.
    pub headers: HashMap<String, String>,
    pub body: Vec<u8>,
}

impl Recorded {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(&name.to_ascii_lowercase()).map(String::as_str)
    }
    pub fn body_str(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
}

#[derive(Debug, Clone)]
pub enum Step {
    /// Write raw bytes and flush.
    Send(Vec<u8>),
    Delay(Duration),
    /// Close the connection now (mid-stream disconnect when used after a partial body).
    Close,
    /// Keep the socket open, writing nothing, until the client disconnects.
    Hang,
}

#[derive(Debug, Clone, Default)]
pub struct Script {
    pub steps: Vec<Step>,
}

pub fn head(status: u16, reason: &str, headers: &[(&str, &str)], content_length: Option<usize>) -> Vec<u8> {
    let mut s = format!("HTTP/1.1 {status} {reason}\r\nConnection: close\r\n");
    for (k, v) in headers {
        s.push_str(&format!("{k}: {v}\r\n"));
    }
    if let Some(n) = content_length {
        s.push_str(&format!("Content-Length: {n}\r\n"));
    }
    s.push_str("\r\n");
    s.into_bytes()
}

impl Script {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn then(mut self, s: Step) -> Self {
        self.steps.push(s);
        self
    }
    pub fn send(self, bytes: impl Into<Vec<u8>>) -> Self {
        self.then(Step::Send(bytes.into()))
    }
    pub fn delay_ms(self, ms: u64) -> Self {
        self.then(Step::Delay(Duration::from_millis(ms)))
    }
    /// Complete response with Content-Length.
    pub fn response(status: u16, headers: &[(&str, &str)], body: &str) -> Self {
        Self::new()
            .send(head(status, "X", headers, Some(body.len())))
            .send(body.as_bytes().to_vec())
    }
    pub fn json(status: u16, body: &str) -> Self {
        Self::response(status, &[("Content-Type", "application/json")], body)
    }
    pub fn redirect(status: u16, location: &str) -> Self {
        Self::response(status, &[("Location", location)], "")
    }
    /// SSE head (EOF-delimited body); follow with `.send(..)` chunks.
    pub fn sse_head() -> Self {
        Self::new().send(head(200, "OK", &[("Content-Type", "text/event-stream")], None))
    }
    /// Send `data` in pieces of at most `n` bytes (arbitrary byte boundaries, mid-UTF-8 too),
    /// with `gap_ms` between pieces.
    pub fn chunked_by(mut self, data: &[u8], n: usize, gap_ms: u64) -> Self {
        for c in data.chunks(n.max(1)) {
            self.steps.push(Step::Send(c.to_vec()));
            if gap_ms > 0 {
                self.steps.push(Step::Delay(Duration::from_millis(gap_ms)));
            }
        }
        self
    }
    pub fn close(self) -> Self {
        self.then(Step::Close)
    }
    pub fn hang(self) -> Self {
        self.then(Step::Hang)
    }
}

pub struct Mock {
    pub addr: std::net::SocketAddr,
    requests: Arc<Mutex<Vec<Recorded>>>,
    disconnects: Arc<AtomicUsize>,
    accepted: Arc<AtomicUsize>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Mock {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Mock {
    pub async fn start(scripts: Vec<Script>) -> Mock {
        assert!(!scripts.is_empty());
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind loopback");
        let addr = listener.local_addr().unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let disconnects = Arc::new(AtomicUsize::new(0));
        let accepted = Arc::new(AtomicUsize::new(0));
        let (r2, d2, a2) = (requests.clone(), disconnects.clone(), accepted.clone());
        let task = tokio::spawn(async move {
            loop {
                let Ok((sock, _)) = listener.accept().await else { return };
                let n = a2.fetch_add(1, Ordering::SeqCst);
                let script = scripts[n.min(scripts.len() - 1)].clone();
                let (r, d) = (r2.clone(), d2.clone());
                tokio::spawn(async move { serve(sock, script, r, d).await });
            }
        });
        Mock { addr, requests, disconnects, accepted, task }
    }

    /// `http://127.0.0.1:<port>` (append a path prefix yourself, e.g. "/v1").
    pub fn url(&self) -> String {
        format!("http://{}", self.addr)
    }
    pub fn requests(&self) -> Vec<Recorded> {
        self.requests.lock().unwrap().clone()
    }
    pub fn connections(&self) -> usize {
        self.accepted.load(Ordering::SeqCst)
    }
    /// Connections the client closed while the script was still in `Hang`.
    pub fn disconnects(&self) -> usize {
        self.disconnects.load(Ordering::SeqCst)
    }
    /// Wait until `n` hung connections were closed by the client (true) or the timeout hits.
    pub async fn wait_disconnects(&self, n: usize, timeout: Duration) -> bool {
        let end = tokio::time::Instant::now() + timeout;
        while tokio::time::Instant::now() < end {
            if self.disconnects() >= n {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        self.disconnects() >= n
    }
}

async fn read_request(sock: &mut TcpStream) -> Option<Recorded> {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 4096];
    let head_end = loop {
        if let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break p;
        }
        let n = sock.read(&mut tmp).await.ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&tmp[..n]);
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
    let mut lines = head.split("\r\n");
    let mut first = lines.next()?.split_whitespace();
    let method = first.next()?.to_string();
    let path = first.next()?.to_string();
    let mut headers = HashMap::new();
    for l in lines {
        if let Some((k, v)) = l.split_once(':') {
            headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
        }
    }
    let want: usize = headers.get("content-length").and_then(|v| v.parse().ok()).unwrap_or(0);
    let mut body = buf[head_end + 4..].to_vec();
    while body.len() < want {
        let n = sock.read(&mut tmp).await.ok()?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&tmp[..n]);
    }
    body.truncate(want);
    Some(Recorded { method, path, headers, body })
}

async fn serve(mut sock: TcpStream, script: Script, reqs: Arc<Mutex<Vec<Recorded>>>, disc: Arc<AtomicUsize>) {
    let Some(rec) = read_request(&mut sock).await else { return };
    reqs.lock().unwrap().push(rec);
    for step in script.steps {
        match step {
            Step::Send(b) => {
                if sock.write_all(&b).await.is_err() || sock.flush().await.is_err() {
                    return;
                }
            }
            Step::Delay(d) => tokio::time::sleep(d).await,
            Step::Close => return,
            Step::Hang => {
                let mut b = [0u8; 64];
                loop {
                    match sock.read(&mut b).await {
                        Ok(0) | Err(_) => {
                            disc.fetch_add(1, Ordering::SeqCst);
                            return;
                        }
                        Ok(_) => {}
                    }
                }
            }
        }
    }
}
