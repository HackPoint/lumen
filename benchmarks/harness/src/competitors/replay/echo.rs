//! A local stand-in for the Anthropic API, and the few lines of HTTP a replay needs.
//!
//! A proxy such as Headroom sits between Claude Code and the API. To see what it would forward,
//! it is pointed (`ANTHROPIC_TARGET_API_URL`) at [`EchoUpstream`], which keeps the body of every
//! request it receives and answers with a small valid message. Nothing here reaches a network
//! beyond `127.0.0.1`: the listener binds the loopback address only.
//!
//! [`read_request`] and [`post_json`] are also what a test uses to plant a proxy that rewrites
//! a tool result, to show the capture would see it.

use crate::util::{Ctx, Res};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// One request as received.
#[derive(Debug, Clone)]
pub struct Request {
    pub method: String,
    pub path: String,
    /// Lowercased names.
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Request {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }
}

/// Read one request from `r`. `Ok(None)` when the peer closed before sending one.
pub fn read_request<R: BufRead, W: Write>(r: &mut R, w: &mut W) -> Res<Option<Request>> {
    let mut line = String::new();
    if r.read_line(&mut line).ctx("read the request line")? == 0 {
        return Ok(None);
    }
    let mut parts = line.split_whitespace();
    let method = parts.next().ok_or("an empty request line")?.to_string();
    let path = parts
        .next()
        .ok_or("a request line with no path")?
        .to_string();
    let mut headers = Vec::new();
    loop {
        line.clear();
        if r.read_line(&mut line).ctx("read a header")? == 0 {
            return Err("the request ended inside its headers".to_string());
        }
        let l = line.trim_end_matches(['\r', '\n']);
        if l.is_empty() {
            break;
        }
        if let Some((k, v)) = l.split_once(':') {
            headers.push((k.trim().to_ascii_lowercase(), v.trim().to_string()));
        }
    }
    let get = |n: &str| headers.iter().find(|(k, _)| k == n).map(|(_, v)| v.clone());
    if get("expect").is_some_and(|v| v.eq_ignore_ascii_case("100-continue")) {
        w.write_all(b"HTTP/1.1 100 Continue\r\n\r\n")
            .ctx("answer Expect")?;
        w.flush().ctx("flush")?;
    }
    let mut body = Vec::new();
    if get("transfer-encoding").is_some_and(|v| v.to_ascii_lowercase().contains("chunked")) {
        loop {
            line.clear();
            r.read_line(&mut line).ctx("read a chunk size")?;
            let size = usize::from_str_radix(line.trim().split(';').next().unwrap_or("0"), 16)
                .ctx("a chunk size")?;
            if size == 0 {
                // The trailer, up to the blank line.
                loop {
                    line.clear();
                    if r.read_line(&mut line).ctx("read a trailer")? == 0
                        || line.trim_end_matches(['\r', '\n']).is_empty()
                    {
                        break;
                    }
                }
                break;
            }
            let mut chunk = vec![0u8; size];
            r.read_exact(&mut chunk).ctx("read a chunk")?;
            body.extend_from_slice(&chunk);
            line.clear();
            r.read_line(&mut line).ctx("read a chunk end")?;
        }
    } else if let Some(n) = get("content-length") {
        let n: usize = n.parse().ctx("content-length")?;
        body = vec![0u8; n];
        r.read_exact(&mut body).ctx("read the body")?;
    }
    Ok(Some(Request {
        method,
        path,
        headers,
        body,
    }))
}

/// The reply every request gets: a valid, tiny Messages API answer.
pub fn canned_reply() -> Vec<u8> {
    let body = serde_json::json!({
        "id": "msg_replay", "type": "message", "role": "assistant",
        "model": "claude-sonnet-5-5",
        "content": [{"type": "text", "text": "ok"}],
        "stop_reason": "end_turn", "stop_sequence": null,
        "usage": {"input_tokens": 1, "output_tokens": 1}
    })
    .to_string();
    format!(
        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{body}",
        body.len()
    )
    .into_bytes()
}

/// A listener on `127.0.0.1` that records request bodies and answers [`canned_reply`].
pub struct EchoUpstream {
    pub addr: SocketAddr,
    requests: Arc<Mutex<Vec<Request>>>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl EchoUpstream {
    pub fn start() -> Res<EchoUpstream> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).ctx("bind the echo upstream")?;
        let addr = listener.local_addr().ctx("the echo upstream's address")?;
        let requests: Arc<Mutex<Vec<Request>>> = Arc::default();
        let stop = Arc::new(AtomicBool::new(false));
        let (reqs, halt) = (requests.clone(), stop.clone());
        let thread = std::thread::spawn(move || {
            for conn in listener.incoming() {
                if halt.load(Ordering::SeqCst) {
                    break;
                }
                let Ok(conn) = conn else { continue };
                let reqs = reqs.clone();
                std::thread::spawn(move || serve(conn, &reqs));
            }
        });
        Ok(EchoUpstream {
            addr,
            requests,
            stop,
            thread: Some(thread),
        })
    }

    pub fn url(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// Everything received so far, oldest first.
    pub fn requests(&self) -> Vec<Request> {
        self.requests.lock().map(|r| r.clone()).unwrap_or_default()
    }

    /// The requests whose body contains `marker`: how a replay finds its own among others.
    pub fn bodies_with(&self, marker: &str) -> Vec<Vec<u8>> {
        self.requests()
            .into_iter()
            .filter(|r| r.method == "POST" && contains(&r.body, marker.as_bytes()))
            .map(|r| r.body)
            .collect()
    }
}

impl Drop for EchoUpstream {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // Wake the accept loop.
        let _ = TcpStream::connect_timeout(&self.addr, Duration::from_millis(500));
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn serve(conn: TcpStream, requests: &Mutex<Vec<Request>>) {
    let _ = conn.set_read_timeout(Some(Duration::from_secs(60)));
    let Ok(mut w) = conn.try_clone() else { return };
    let mut r = BufReader::new(conn);
    while let Ok(Some(req)) = read_request(&mut r, &mut w) {
        let close = req
            .header("connection")
            .is_some_and(|v| v.eq_ignore_ascii_case("close"));
        let mut reply = canned_reply();
        if req.method == "HEAD" {
            // A HEAD answer has the headers and no body; sending one would misframe the next
            // response on a kept-alive connection (a proxy's upstream probe is a HEAD).
            if let Some(at) = reply.windows(4).position(|w| w == b"\r\n\r\n") {
                reply.truncate(at + 4);
            }
        }
        if let Ok(mut g) = requests.lock() {
            g.push(req);
        }
        if w.write_all(&reply).is_err() || w.flush().is_err() {
            return;
        }
        if close {
            linger_close(&w, &mut r);
            return;
        }
    }
}

/// Close after the reply without a reset: send FIN first, then read what the client may still
/// send until it closes too. Windows answers a close with unread bytes in the receive buffer
/// with a reset, which the client sees as "connection reset" in place of the reply's end.
fn linger_close(w: &TcpStream, r: &mut BufReader<TcpStream>) {
    if w.shutdown(std::net::Shutdown::Write).is_err() {
        return;
    }
    if w.set_read_timeout(Some(Duration::from_secs(2))).is_err() {
        return;
    }
    let mut sink = [0u8; 4096];
    while matches!(r.read(&mut sink), Ok(n) if n > 0) {}
}

pub fn contains(hay: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty() && hay.windows(needle.len()).any(|w| w == needle)
}

/// POST `body` as JSON to `http://addr/path` and return the status and the response body.
pub fn post_json(
    addr: SocketAddr,
    path: &str,
    body: &[u8],
    timeout: Duration,
) -> Res<(u16, Vec<u8>)> {
    let mut s = TcpStream::connect_timeout(&addr, timeout).ctx(&format!("connect to {addr}"))?;
    s.set_read_timeout(Some(timeout))
        .ctx("set the read timeout")?;
    let head = format!(
        "POST {path} HTTP/1.1\r\nhost: {addr}\r\ncontent-type: application/json\r\n\
         x-api-key: sk-ant-bench-not-a-key\r\nanthropic-version: 2023-06-01\r\n\
         content-length: {}\r\nconnection: close\r\n\r\n",
        body.len()
    );
    s.write_all(head.as_bytes()).ctx("send the request head")?;
    s.write_all(body).ctx("send the request body")?;
    s.flush().ctx("flush")?;
    let mut raw = Vec::new();
    s.read_to_end(&mut raw).ctx("read the response")?;
    let split = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or("the response has no header end")?;
    let head = String::from_utf8_lossy(&raw[..split]).into_owned();
    let status: u16 = head
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse().ok())
        .ok_or_else(|| format!("no status in {head:?}"))?;
    Ok((status, raw[split + 4..].to_vec()))
}

/// GET `http://addr/path`: the status and body. For a proxy's readiness probe.
pub fn get(addr: SocketAddr, path: &str, timeout: Duration) -> Res<(u16, Vec<u8>)> {
    let mut s = TcpStream::connect_timeout(&addr, timeout).ctx(&format!("connect to {addr}"))?;
    s.set_read_timeout(Some(timeout))
        .ctx("set the read timeout")?;
    s.write_all(
        format!("GET {path} HTTP/1.1\r\nhost: {addr}\r\nconnection: close\r\n\r\n").as_bytes(),
    )
    .ctx("send the request")?;
    let mut raw = Vec::new();
    s.read_to_end(&mut raw).ctx("read the response")?;
    let split = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or("the response has no header end")?;
    let head = String::from_utf8_lossy(&raw[..split]).into_owned();
    let status: u16 = head
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse().ok())
        .ok_or_else(|| format!("no status in {head:?}"))?;
    let mut body = raw[split + 4..].to_vec();
    if head
        .to_ascii_lowercase()
        .contains("transfer-encoding: chunked")
    {
        // The request reader's chunk decoder, with the body framed as a request.
        let mut framed = b"POST / HTTP/1.1\r\ntransfer-encoding: chunked\r\n\r\n".to_vec();
        framed.extend_from_slice(&body);
        let mut sink = Vec::new();
        if let Some(req) = read_request(&mut BufReader::new(&framed[..]), &mut sink)? {
            body = req.body;
        }
    }
    Ok((status, body))
}
