//! The `ciabatta daemon` status web view: a small page listing every watched
//! workspace, its bound profile, and when each of its keys last changed.
//!
//! Deliberately never serves resolved *values* — only key names and change
//! timestamps ([`EnvChecker::status`]) — so a value pulled from
//! `source = aws_secrets` is never rendered, even to localhost.
//!
//! Endpoints:
//!   * `GET /`            — the embedded status page
//!   * `GET /api/status`  — the same data as JSON, for polling

use std::sync::Arc;

use anyhow::Result;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use super::EnvChecker;

/// The embedded single-page status view (HTML + CSS + JS, no external assets).
const INDEX_HTML: &str = include_str!("index.html");

/// Bind the status view on `127.0.0.1:port` (or `CIABATTA_BIND_HOST`) and
/// spawn its accept loop in the background, returning the base URL
/// immediately — mirrors `ai::server::spawn`, since the daemon's foreground
/// work (the poll loop, driven by the caller) runs concurrently with this.
pub async fn spawn(checker: Arc<EnvChecker>, port: u16) -> Result<String> {
    let host = crate::config::bind_host();
    let listener = TcpListener::bind((host.as_str(), port))
        .await
        .map_err(|e| anyhow::anyhow!("Failed to bind {host}:{port} ({e}). Try a different --port."))?;
    let url = format!("http://{host}:{port}/");

    tokio::spawn(async move {
        loop {
            let (socket, _) = match listener.accept().await {
                Ok(pair) => pair,
                Err(e) => {
                    tracing::debug!(error = %e, "daemon server: accept failed");
                    continue;
                }
            };
            let checker = checker.clone();
            tokio::spawn(async move {
                if let Err(e) = handle(socket, &checker).await {
                    tracing::debug!(error = %e, "daemon server: connection error");
                }
            });
        }
    });

    Ok(url)
}

async fn handle(mut socket: TcpStream, checker: &EnvChecker) -> Result<()> {
    let Some(req) = read_request(&mut socket).await? else {
        return Ok(());
    };

    let (status, content_type, body): (&str, &str, Vec<u8>) = match (req.method.as_str(), req.path.as_str()) {
        ("GET", "/") | ("GET", "/index.html") => {
            ("200 OK", "text/html; charset=utf-8", INDEX_HTML.into())
        }
        ("GET", "/api/status") => {
            let json = serde_json::to_vec(&checker.status())?;
            ("200 OK", "application/json; charset=utf-8", json)
        }
        _ => (
            "404 Not Found",
            "text/plain; charset=utf-8",
            b"not found".to_vec(),
        ),
    };

    write_response(&mut socket, status, content_type, &body).await
}

// ─── minimal HTTP plumbing (same shape as watch::server / todo::server) ─────

struct Request {
    method: String,
    path: String,
}

async fn read_request(socket: &mut TcpStream) -> Result<Option<Request>> {
    let mut buf = Vec::with_capacity(2048);
    let mut chunk = [0u8; 2048];

    loop {
        let n = socket.read(&mut chunk).await?;
        if n == 0 {
            if buf.is_empty() {
                return Ok(None);
            }
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
        if find_subslice(&buf, b"\r\n\r\n").is_some() || buf.len() > 1 << 16 {
            break;
        }
    }

    let head = String::from_utf8_lossy(&buf).to_string();
    let request_line = head.lines().next().unwrap_or("");
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("GET").to_string();
    let path = parts.next().unwrap_or("/").to_string();

    Ok(Some(Request { method, path }))
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
