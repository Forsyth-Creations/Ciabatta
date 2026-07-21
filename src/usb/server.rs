//! The `ciabatta usb` web server.
//!
//! A tiny dependency-free HTTP server (same shape as [`crate::todo::server`])
//! that serves the embedded single-page UI and a small JSON API:
//!
//! * `GET  /`          — the UI
//! * `GET  /api/ports` — enumerate serial ports for the picker
//! * `POST /api/send`  — decode the hex prefix + file, write them to a port
//!
//! The file arrives as a hex string in the `/api/send` JSON body, so binary
//! never has to survive the (text-oriented) request reader, and no base64
//! dependency is needed. The actual port write is blocking, so it runs on a
//! blocking thread to keep the async server responsive.

use anyhow::Result;
use serde::Deserialize;
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// The embedded single-page app (HTML + CSS + JS, no external assets).
const INDEX_HTML: &str = include_str!("index.html");

/// Serve the USB sender on `bind_host:port` until the process is interrupted.
pub async fn serve(port: u16, open: bool) -> Result<()> {
    let host = crate::config::bind_host();
    let listener = TcpListener::bind((host.as_str(), port)).await.map_err(|e| {
        anyhow::anyhow!("Failed to bind {host}:{port} ({e}). Try a different --port.")
    })?;

    let url = format!("http://{host}:{port}");
    println!("\nUSB sender ready at {url}");
    println!("Pick a serial device, choose a file, and send it (with an optional hex prefix).");
    println!("Press Ctrl-C to stop.");
    if open {
        open_browser(&url);
    }

    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                println!("\nStopping usb sender.");
                return Ok(());
            }
            accepted = listener.accept() => {
                let (socket, _) = accepted?;
                tokio::spawn(async move {
                    if let Err(e) = handle(socket).await {
                        eprintln!("usb server: connection error: {e}");
                    }
                });
            }
        }
    }
}

/// The `/api/send` request body: a port, baud rate, and the prefix + file, both
/// as hex strings.
#[derive(Deserialize)]
struct SendPayload {
    port: String,
    baud: u32,
    #[serde(default)]
    prefix: String,
    #[serde(default)]
    data: String,
}

async fn handle(mut socket: TcpStream) -> Result<()> {
    let Some(req) = read_request(&mut socket).await? else {
        return Ok(());
    };

    let (status, content_type, body): (&str, &str, Vec<u8>) =
        match (req.method.as_str(), req.path.as_str()) {
            ("GET", "/") | ("GET", "/index.html") => {
                ("200 OK", "text/html; charset=utf-8", INDEX_HTML.into())
            }
            ("GET", "/api/ports") => match super::list_ports() {
                Ok(ports) => ok_json(json!({ "ports": ports })),
                Err(e) => bad_request(&e.to_string()),
            },
            ("POST", "/api/send") => send(&req.body).await,
            _ => (
                "404 Not Found",
                "text/plain; charset=utf-8",
                b"not found".to_vec(),
            ),
        };

    write_response(&mut socket, status, content_type, &body).await
}

/// Handle `POST /api/send`: parse the payload, then run the blocking serial
/// write on a blocking thread so the async runtime isn't stalled.
async fn send(body: &str) -> (&'static str, &'static str, Vec<u8>) {
    let payload: SendPayload = match serde_json::from_str(body) {
        Ok(p) => p,
        Err(e) => return bad_request(&e.to_string()),
    };

    let result = tokio::task::spawn_blocking(move || {
        super::send(&payload.port, payload.baud, &payload.prefix, &payload.data)
    })
    .await;

    match result {
        Ok(Ok(written)) => ok_json(json!({ "ok": true, "written": written })),
        Ok(Err(e)) => ok_json(json!({ "ok": false, "error": e.to_string() })),
        Err(e) => bad_request(&format!("send task panicked: {e}")),
    }
}

/// A 200 JSON response.
fn ok_json(v: serde_json::Value) -> (&'static str, &'static str, Vec<u8>) {
    (
        "200 OK",
        "application/json; charset=utf-8",
        v.to_string().into_bytes(),
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

// ─── HTTP plumbing (adapted from src/todo/server.rs) ─────────────────────────

struct Request {
    method: String,
    path: String,
    body: String,
}

/// Read a full HTTP request: the head, then any body indicated by
/// `Content-Length`. Returns `None` on an empty/closed connection.
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
