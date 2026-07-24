//! The `ciabatta usb` web server.
//!
//! A tiny dependency-free HTTP server (same shape as [`crate::todo::server`])
//! that serves the embedded single-page UI and a small JSON API:
//!
//! * `GET  /`                    — the UI
//! * `GET  /api/ports`           — enumerate serial ports for the picker
//! * `POST /api/send`            — decode the hex prefix + file, write them
//!   to a port
//! * `POST /api/capture/start`   — open two ports and start relaying/logging
//!   traffic between them (see [`super::capture`])
//! * `POST /api/capture/stop`    — stop a running capture and persist it
//! * `GET  /api/capture/status`  — live snapshot of a running capture
//! * `GET  /api/captures`        — list saved captures
//! * `GET  /api/captures/frames` — a saved capture's full frame list
//! * `POST /api/captures/delete` — delete a saved capture
//! * `POST /api/replay`          — replay one direction's frames to a port
//! * `POST /api/export/rust`     — render one direction's frames as a
//!   paste-able `.rs` snippet
//!
//! The file arrives as a hex string in the `/api/send` JSON body, so binary
//! never has to survive the (text-oriented) request reader, and no base64
//! dependency is needed. Blocking work (serial I/O, capture file I/O) always
//! runs on a blocking thread so the async server stays responsive.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use serde::Deserialize;
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use super::capture;

/// The embedded single-page app (HTML + CSS + JS, no external assets).
const INDEX_HTML: &str = include_str!("index.html");

/// Where captures are persisted: `.ciabatta/usb/captures/` under the current
/// working directory. `usb` doesn't require a `.ciabatta` project, so this is
/// created on demand rather than requiring one to already exist.
fn cwd_root() -> PathBuf {
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

/// Serve the USB sender on `bind_host:port` until the process is interrupted.
pub async fn serve(port: u16, open: bool) -> Result<()> {
    let host = crate::config::bind_host();
    let listener = TcpListener::bind((host.as_str(), port))
        .await
        .map_err(|e| {
            anyhow::anyhow!("Failed to bind {host}:{port} ({e}). Try a different --port.")
        })?;

    let url = format!("http://{host}:{port}");
    println!("\nUSB sender ready at {url}");
    println!("Pick a serial device, choose a file, and send it (with an optional hex prefix).");
    println!("Press Ctrl-C to stop.");
    if open {
        open_browser(&url);
    }

    let manager = Arc::new(capture::CaptureManager::new());

    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                println!("\nStopping usb sender.");
                manager.stop_all(&cwd_root());
                return Ok(());
            }
            accepted = listener.accept() => {
                let (socket, _) = accepted?;
                let manager = manager.clone();
                tokio::spawn(async move {
                    if let Err(e) = handle(socket, manager).await {
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

/// The `/api/capture/start` request body.
#[derive(Deserialize)]
struct CaptureStartPayload {
    /// The real hardware's port.
    device_port: String,
    /// Ciabatta's own end of a virtual null-modem pair (see
    /// [`crate::usb::capture`]'s module docs) — *not* the port the
    /// third-party tool connects to; that must be the pair's other port.
    app_port: String,
    baud: u32,
}

/// A JSON payload carrying just an id (`capture/stop`, `captures/delete`).
#[derive(Deserialize)]
struct IdPayload {
    id: String,
}

/// The `/api/replay` request body.
#[derive(Deserialize)]
struct ReplayPayload {
    /// The saved capture to replay from.
    id: String,
    /// `app_to_device` or `device_to_app` — which side to replay.
    direction: String,
    /// Restrict to specific frame indices (into the saved capture); all
    /// frames on `direction` when omitted.
    #[serde(default)]
    frame_indices: Option<Vec<usize>>,
    /// The port to replay onto.
    port: String,
    baud: u32,
    /// Sleep between frames for the recorded delay. Defaults to true.
    #[serde(default = "default_true")]
    honor_timing: bool,
}

fn default_true() -> bool {
    true
}

/// The `/api/export/rust` request body.
#[derive(Deserialize)]
struct ExportPayload {
    id: String,
    direction: String,
    #[serde(default)]
    frame_indices: Option<Vec<usize>>,
}

async fn handle(mut socket: TcpStream, manager: Arc<capture::CaptureManager>) -> Result<()> {
    let Some(req) = read_request(&mut socket).await? else {
        return Ok(());
    };
    let (path, query) = req.path.split_once('?').unwrap_or((req.path.as_str(), ""));

    let (status, content_type, body): (&str, &str, Vec<u8>) = match (req.method.as_str(), path) {
        ("GET", "/") | ("GET", "/index.html") => {
            ("200 OK", "text/html; charset=utf-8", INDEX_HTML.into())
        }
        ("GET", "/api/ports") => match super::list_ports() {
            Ok(ports) => ok_json(json!({ "ports": ports })),
            Err(e) => bad_request(&e.to_string()),
        },
        ("POST", "/api/send") => send(&req.body).await,

        ("POST", "/api/capture/start") => capture_start(manager, &req.body).await,
        ("POST", "/api/capture/stop") => capture_stop(manager, &req.body).await,
        ("GET", "/api/capture/status") => capture_status(&manager, query),
        ("GET", "/api/captures") => captures_list().await,
        ("GET", "/api/captures/frames") => capture_frames(query).await,
        ("POST", "/api/captures/delete") => captures_delete(&req.body).await,
        ("POST", "/api/replay") => capture_replay(&req.body).await,
        ("POST", "/api/export/rust") => capture_export(&req.body).await,

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

/// Handle `POST /api/capture/start`: open both ports and start relaying, on
/// a blocking thread (opening a serial port can block).
async fn capture_start(
    manager: Arc<capture::CaptureManager>,
    body: &str,
) -> (&'static str, &'static str, Vec<u8>) {
    let payload: CaptureStartPayload = match serde_json::from_str(body) {
        Ok(p) => p,
        Err(e) => return bad_request(&e.to_string()),
    };

    let result = tokio::task::spawn_blocking(move || {
        manager.start(&payload.device_port, &payload.app_port, payload.baud)
    })
    .await;

    match result {
        Ok(Ok(id)) => ok_json(json!({ "ok": true, "id": id })),
        Ok(Err(e)) => ok_json(json!({ "ok": false, "error": e.to_string() })),
        Err(e) => bad_request(&format!("capture start task panicked: {e}")),
    }
}

/// Handle `POST /api/capture/stop`: stop the relay threads, sort and persist
/// the captured frames, and report a summary.
async fn capture_stop(
    manager: Arc<capture::CaptureManager>,
    body: &str,
) -> (&'static str, &'static str, Vec<u8>) {
    let payload: IdPayload = match serde_json::from_str(body) {
        Ok(p) => p,
        Err(e) => return bad_request(&e.to_string()),
    };

    let result = tokio::task::spawn_blocking(move || manager.stop(&cwd_root(), &payload.id)).await;

    match result {
        Ok(Ok(session)) => {
            let app_to_device_bytes: usize = session
                .frames
                .iter()
                .filter(|f| f.dir == capture::Direction::AppToDevice)
                .map(|f| f.data.len())
                .sum();
            let device_to_app_bytes: usize = session
                .frames
                .iter()
                .filter(|f| f.dir == capture::Direction::DeviceToApp)
                .map(|f| f.data.len())
                .sum();
            let duration_ms = session.frames.last().map(|f| f.t_ms).unwrap_or(0);
            ok_json(json!({
                "ok": true,
                "id": session.id,
                "frame_count": session.frames.len(),
                "duration_ms": duration_ms,
                "app_to_device_bytes": app_to_device_bytes,
                "device_to_app_bytes": device_to_app_bytes,
            }))
        }
        Ok(Err(e)) => ok_json(json!({ "ok": false, "error": e.to_string() })),
        Err(e) => bad_request(&format!("capture stop task panicked: {e}")),
    }
}

/// Handle `GET /api/capture/status?id=`: a live snapshot for the polling UI.
/// Just a mutex lock, so no blocking thread needed.
fn capture_status(
    manager: &capture::CaptureManager,
    query: &str,
) -> (&'static str, &'static str, Vec<u8>) {
    let Some(id) = query_param(query, "id") else {
        return bad_request("missing 'id' query parameter");
    };

    match manager.status(&id) {
        Some(s) => {
            let recent: Vec<serde_json::Value> = s
                .recent
                .iter()
                .map(|f| {
                    json!({
                        "t_ms": f.t_ms,
                        "dir": direction_label(f.dir),
                        // Same key as the `Frame` field in the saved-capture
                        // JSON (`/api/captures/frames`), so the front end can
                        // treat both shapes the same way.
                        "data": super::encode_hex(&f.data),
                    })
                })
                .collect();
            ok_json(json!({
                "running": true,
                "frame_count": s.frame_count,
                "app_to_device_bytes": s.app_to_device_bytes,
                "device_to_app_bytes": s.device_to_app_bytes,
                "elapsed_ms": s.elapsed_ms,
                "recent": recent,
            }))
        }
        None => ok_json(json!({ "running": false })),
    }
}

/// Handle `GET /api/captures`: saved capture summaries for the picker.
async fn captures_list() -> (&'static str, &'static str, Vec<u8>) {
    let result = tokio::task::spawn_blocking(|| capture::list(&cwd_root())).await;
    match result {
        Ok(Ok(summaries)) => {
            let items: Vec<serde_json::Value> = summaries
                .into_iter()
                .map(|s| {
                    json!({
                        "id": s.id,
                        "created_at": s.created_at,
                        "device_port": s.device_port,
                        "app_port": s.app_port,
                        "baud": s.baud,
                        "frame_count": s.frame_count,
                    })
                })
                .collect();
            ok_json(json!({ "captures": items }))
        }
        Ok(Err(e)) => bad_request(&e.to_string()),
        Err(e) => bad_request(&format!("list task panicked: {e}")),
    }
}

/// Handle `GET /api/captures/frames?id=`: one saved capture's full frame list.
async fn capture_frames(query: &str) -> (&'static str, &'static str, Vec<u8>) {
    let Some(id) = query_param(query, "id") else {
        return bad_request("missing 'id' query parameter");
    };

    let result =
        tokio::task::spawn_blocking(move || capture::CaptureSession::load(&cwd_root(), &id)).await;

    match result {
        Ok(Ok(session)) => match serde_json::to_value(&session) {
            Ok(v) => ok_json(v),
            Err(e) => bad_request(&e.to_string()),
        },
        Ok(Err(e)) => bad_request(&e.to_string()),
        Err(e) => bad_request(&format!("load task panicked: {e}")),
    }
}

/// Handle `POST /api/captures/delete`.
async fn captures_delete(body: &str) -> (&'static str, &'static str, Vec<u8>) {
    let payload: IdPayload = match serde_json::from_str(body) {
        Ok(p) => p,
        Err(e) => return bad_request(&e.to_string()),
    };

    let result =
        tokio::task::spawn_blocking(move || capture::delete(&cwd_root(), &payload.id)).await;

    match result {
        Ok(Ok(removed)) => ok_json(json!({ "ok": true, "removed": removed })),
        Ok(Err(e)) => ok_json(json!({ "ok": false, "error": e.to_string() })),
        Err(e) => bad_request(&format!("delete task panicked: {e}")),
    }
}

/// Handle `POST /api/replay`: load the saved capture, select the requested
/// direction/frames, and write them to the target port.
async fn capture_replay(body: &str) -> (&'static str, &'static str, Vec<u8>) {
    let payload: ReplayPayload = match serde_json::from_str(body) {
        Ok(p) => p,
        Err(e) => return bad_request(&e.to_string()),
    };
    let dir = match parse_direction(&payload.direction) {
        Ok(d) => d,
        Err(e) => return bad_request(&e),
    };

    let result = tokio::task::spawn_blocking(move || {
        let session = capture::CaptureSession::load(&cwd_root(), &payload.id)?;
        let frames: Vec<capture::Frame> =
            capture::select_frames(&session, dir, payload.frame_indices.as_deref())
                .into_iter()
                .cloned()
                .collect();
        capture::replay(&payload.port, payload.baud, &frames, payload.honor_timing)
    })
    .await;

    match result {
        Ok(Ok(written)) => ok_json(json!({ "ok": true, "written": written })),
        Ok(Err(e)) => ok_json(json!({ "ok": false, "error": e.to_string() })),
        Err(e) => bad_request(&format!("replay task panicked: {e}")),
    }
}

/// Handle `POST /api/export/rust`: render the requested direction/frames as a
/// paste-able `.rs` snippet.
async fn capture_export(body: &str) -> (&'static str, &'static str, Vec<u8>) {
    let payload: ExportPayload = match serde_json::from_str(body) {
        Ok(p) => p,
        Err(e) => return bad_request(&e.to_string()),
    };
    let dir = match parse_direction(&payload.direction) {
        Ok(d) => d,
        Err(e) => return bad_request(&e),
    };

    let result = tokio::task::spawn_blocking(move || {
        let session = capture::CaptureSession::load(&cwd_root(), &payload.id)?;
        anyhow::Ok(capture::export_rust(
            &session,
            dir,
            payload.frame_indices.as_deref(),
        ))
    })
    .await;

    match result {
        Ok(Ok(code)) => ok_json(json!({ "ok": true, "code": code })),
        Ok(Err(e)) => ok_json(json!({ "ok": false, "error": e.to_string() })),
        Err(e) => bad_request(&format!("export task panicked: {e}")),
    }
}

/// Parse a direction query/JSON value (`app_to_device` / `device_to_app`).
fn parse_direction(s: &str) -> Result<capture::Direction, String> {
    match s {
        "app_to_device" => Ok(capture::Direction::AppToDevice),
        "device_to_app" => Ok(capture::Direction::DeviceToApp),
        other => Err(format!(
            "Unknown direction '{other}' (expected app_to_device or device_to_app)"
        )),
    }
}

fn direction_label(dir: capture::Direction) -> &'static str {
    match dir {
        capture::Direction::AppToDevice => "app_to_device",
        capture::Direction::DeviceToApp => "device_to_app",
    }
}

/// Look up `key` in a (already-decoded, `&`-joined) query string. Frame/
/// capture ids are plain timestamp-derived strings with no characters that
/// need percent-decoding, so this stays as simple as `ai/server.rs`'s
/// `after=` parsing.
fn query_param(query: &str, key: &str) -> Option<String> {
    query.split('&').find_map(|kv| {
        let (k, v) = kv.split_once('=')?;
        (k == key).then(|| v.to_string())
    })
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
