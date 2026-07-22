//! The `ciabatta watch` web server.
//!
//! A tiny dependency-free HTTP server (same shape as [`crate::deploy::server`]
//! and [`crate::todo::server`]) that serves the embedded single-page UI and a
//! small JSON API:
//!
//! * `GET  /`            — the UI
//! * `GET  /state.json`  — incremental poll: `?after=<seq>&limit=<n>`
//! * `GET  /search`      — server-side search: `?q=<t1,t2>&mode=&stream=&regex=`
//! * `GET  /download`    — buffer slice as text/plain: `?from=<seq>&to=<seq>&stream=`
//! * `POST /bookmarks`, `/bookmarks/delete`
//! * `POST /triggers`,  `/triggers/delete`

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Result;
use serde::Deserialize;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use super::{Stream, WatchState};

/// The embedded single-page app (HTML + CSS + JS, no external assets).
const INDEX_HTML: &str = include_str!("index.html");

/// Default cap on lines returned in one `/state.json` or `/search` response.
const DEFAULT_LIMIT: usize = 5000;

/// Run the command and serve its live log view at `http://127.0.0.1:port`.
pub async fn serve(store: Arc<WatchState>, command: String, port: u16, open: bool) -> Result<()> {
    let listener = TcpListener::bind(("127.0.0.1", port)).await.map_err(|e| {
        anyhow::anyhow!("Failed to bind 127.0.0.1:{port} ({e}). Try a different --port.")
    })?;

    // Start the watched command; its output streams into the store.
    store.spawn(&command)?;

    let url = format!("http://127.0.0.1:{port}");
    println!("\nWatching: {command}");
    println!("Log view ready at {url}");
    println!("Press Ctrl-C to stop.");
    if open {
        open_browser(&url);
    }

    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                println!("\nStopping watch.");
                return Ok(());
            }
            accepted = listener.accept() => {
                let (socket, _) = accepted?;
                let store = store.clone();
                tokio::spawn(async move {
                    if let Err(e) = handle(socket, &store).await {
                        eprintln!("watch server: connection error: {e}");
                    }
                });
            }
        }
    }
}

// ─── request payloads ────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct AddBookmark {
    seq: u64,
    #[serde(default)]
    label: String,
    #[serde(default)]
    note: Option<String>,
}

#[derive(Deserialize)]
struct AddTrigger {
    pattern: String,
    #[serde(default)]
    is_regex: bool,
}

#[derive(Deserialize)]
struct IdPayload {
    id: u64,
}

async fn handle(mut socket: TcpStream, store: &WatchState) -> Result<()> {
    let Some(req) = read_request(&mut socket).await? else {
        return Ok(());
    };

    let (path, query) = split_query(&req.path);

    let (status, content_type, body): (&str, &str, Vec<u8>) = match (req.method.as_str(), path) {
        ("GET", "/") | ("GET", "/index.html") => {
            ("200 OK", "text/html; charset=utf-8", INDEX_HTML.into())
        }
        ("GET", "/state.json") => {
            let after = query.get("after").and_then(|v| v.parse().ok()).unwrap_or(0);
            let limit = query
                .get("limit")
                .and_then(|v| v.parse().ok())
                .unwrap_or(DEFAULT_LIMIT);
            let json = store.snapshot(after, limit);
            (
                "200 OK",
                "application/json; charset=utf-8",
                serde_json::to_vec(&json)?,
            )
        }
        ("GET", "/search") => {
            let terms: Vec<String> = query
                .get("q")
                .map(|q| {
                    q.split([',', ' '])
                        .map(str::trim)
                        .filter(|t| !t.is_empty())
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default();
            let all = query.get("mode").map(|m| m == "all").unwrap_or(false);
            let regex = query.get("regex").map(|r| r == "1").unwrap_or(false);
            let stream = match query.get("stream").map(String::as_str) {
                Some("stdout") => Some(Stream::Stdout),
                Some("stderr") => Some(Stream::Stderr),
                _ => None,
            };
            let (lines, total) = if terms.is_empty() {
                (Vec::new(), 0)
            } else {
                store.search(&terms, all, regex, stream, DEFAULT_LIMIT)
            };
            let json = serde_json::json!({ "lines": lines, "total": total, "capped": total > lines.len() });
            (
                "200 OK",
                "application/json; charset=utf-8",
                serde_json::to_vec(&json)?,
            )
        }
        ("GET", "/download") => {
            let from = query.get("from").and_then(|v| v.parse().ok()).unwrap_or(0);
            let to = query.get("to").and_then(|v| v.parse().ok()).unwrap_or(0);
            let stream = match query.get("stream").map(String::as_str) {
                Some("stdout") => Some(Stream::Stdout),
                Some("stderr") => Some(Stream::Stderr),
                _ => None,
            };
            let text = store.download(from, to, stream);
            ("200 OK", "text/plain; charset=utf-8", text.into_bytes())
        }
        ("POST", "/bookmarks") => match serde_json::from_str::<AddBookmark>(&req.body) {
            Ok(p) => {
                let id = store.add_bookmark(p.seq, &p.label, p.note);
                ok_json(&serde_json::json!({ "id": id }))
            }
            Err(e) => bad_request(&e.to_string()),
        },
        ("POST", "/bookmarks/delete") => match serde_json::from_str::<IdPayload>(&req.body) {
            Ok(p) => {
                store.remove_bookmark(p.id);
                ok_json(&serde_json::json!({ "ok": true }))
            }
            Err(e) => bad_request(&e.to_string()),
        },
        ("POST", "/triggers") => match serde_json::from_str::<AddTrigger>(&req.body) {
            Ok(p) => match store.add_trigger(&p.pattern, p.is_regex) {
                Ok(id) => ok_json(&serde_json::json!({ "id": id })),
                Err(e) => bad_request(&e.to_string()),
            },
            Err(e) => bad_request(&e.to_string()),
        },
        ("POST", "/triggers/delete") => match serde_json::from_str::<IdPayload>(&req.body) {
            Ok(p) => {
                store.remove_trigger(p.id);
                ok_json(&serde_json::json!({ "ok": true }))
            }
            Err(e) => bad_request(&e.to_string()),
        },
        _ => (
            "404 Not Found",
            "text/plain; charset=utf-8",
            b"not found".to_vec(),
        ),
    };

    write_response(&mut socket, status, content_type, &body).await
}

/// A 200 JSON response.
fn ok_json(v: &serde_json::Value) -> (&'static str, &'static str, Vec<u8>) {
    (
        "200 OK",
        "application/json; charset=utf-8",
        serde_json::to_vec(v).unwrap_or_else(|_| b"{}".to_vec()),
    )
}

/// A 400 response carrying a short plain-text message.
fn bad_request(msg: &str) -> (&'static str, &'static str, Vec<u8>) {
    (
        "400 Bad Request",
        "text/plain; charset=utf-8",
        msg.as_bytes().to_vec(),
    )
}

// ─── HTTP plumbing (adapted from src/deploy/server.rs) ───────────────────────

struct Request {
    method: String,
    path: String,
    body: String,
}

/// Split a request target into its path and decoded query parameters.
fn split_query(target: &str) -> (&str, HashMap<String, String>) {
    match target.split_once('?') {
        None => (target, HashMap::new()),
        Some((path, qs)) => {
            let mut map = HashMap::new();
            for pair in qs.split('&') {
                if pair.is_empty() {
                    continue;
                }
                let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
                map.insert(percent_decode(k), percent_decode(v));
            }
            (path, map)
        }
    }
}

/// Decode `%XX` escapes and `+` (as space) in a query-string component.
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() => {
                let hi = (bytes[i + 1] as char).to_digit(16);
                let lo = (bytes[i + 2] as char).to_digit(16);
                if let (Some(hi), Some(lo)) = (hi, lo) {
                    out.push((hi * 16 + lo) as u8);
                    i += 3;
                } else {
                    out.push(bytes[i]);
                    i += 1;
                }
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Read an HTTP request, including a `Content-Length` body if present. Returns
/// `None` on an empty/closed connection.
async fn read_request(socket: &mut TcpStream) -> Result<Option<Request>> {
    let mut buf = Vec::with_capacity(4096);
    let mut chunk = [0u8; 4096];

    let header_end = loop {
        let n = socket.read(&mut chunk).await?;
        if n == 0 {
            if buf.is_empty() {
                return Ok(None);
            }
            break buf.len();
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(pos) = find_subslice(&buf, b"\r\n\r\n") {
            break pos + 4;
        }
        if buf.len() > 1 << 20 {
            break buf.len(); // guard against unbounded headers
        }
    };

    let head = String::from_utf8_lossy(&buf[..header_end.min(buf.len())]).to_string();
    let mut lines = head.lines();
    let request_line = lines.next().unwrap_or("");
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("GET").to_string();
    let path = parts.next().unwrap_or("/").to_string();

    let content_length = head
        .lines()
        .find_map(|l| {
            let l = l.trim();
            l.strip_prefix("Content-Length:")
                .or_else(|| l.strip_prefix("content-length:"))
        })
        .and_then(|v| v.trim().parse::<usize>().ok())
        .unwrap_or(0);

    let mut body = buf[header_end.min(buf.len())..].to_vec();
    while body.len() < content_length {
        let n = socket.read(&mut chunk).await?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..n]);
    }
    body.truncate(content_length);

    Ok(Some(Request {
        method,
        path,
        body: String::from_utf8_lossy(&body).to_string(),
    }))
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

async fn write_response(
    socket: &mut TcpStream,
    status: &str,
    content_type: &str,
    body: &[u8],
) -> Result<()> {
    let header = format!(
        "HTTP/1.1 {status}\r\n\
         Content-Type: {content_type}\r\n\
         Content-Length: {}\r\n\
         Cache-Control: no-store\r\n\
         Connection: close\r\n\r\n",
        body.len()
    );
    socket.write_all(header.as_bytes()).await?;
    socket.write_all(body).await?;
    socket.flush().await?;
    Ok(())
}

/// Best-effort: open `url` in the platform browser. Never fails the command.
fn open_browser(url: &str) {
    #[cfg(target_os = "macos")]
    let candidates: [(&str, &[&str]); 1] = [("open", &[])];
    #[cfg(target_os = "windows")]
    let candidates: [(&str, &[&str]); 1] = [("cmd", &["/C", "start", ""])];
    #[cfg(all(unix, not(target_os = "macos")))]
    let candidates: [(&str, &[&str]); 2] = [("xdg-open", &[]), ("gio", &["open"])];

    for (cmd, args) in candidates {
        let mut command = std::process::Command::new(cmd);
        command.args(args).arg(url);
        command.stdout(std::process::Stdio::null());
        command.stderr(std::process::Stdio::null());
        if command.spawn().is_ok() {
            return;
        }
    }
}
