//! The "AI assistant" daemon: a tiny HTTP server (no framework) exposing the
//! live architecture mind map and a JSON API.
//!
//! Endpoints:
//!   * `GET  /`                  — the embedded graph page
//!   * `GET  /api/graph?after=N` — the mind map; returns `{changed:false}`
//!                                 while the brain's sequence is still `N`,
//!                                 so the page can poll cheaply in real time
//!   * `POST /api/ask`           — `{"prompt": "..."}` → run the agent loop
//!   * `POST /api/confirm`       — `{"file": "...", "accept": true}` resolve a
//!                                 pending tag confirmation
//!   * `POST /api/feedback`      — `{"positive": true, "note": "..."}` train
//!                                 the confidence score
//!   * `POST /api/prune`         — `{"kind": "file"|"architecture"|"tag",
//!                                 "id": "...", "tag": "..."}` remove knowledge
//!                                 from the mind map
//!   * `POST /api/ship`          — `{"prompt": "..."}` start a background AI
//!                                 task; returns its id immediately
//!   * `GET  /api/jobs`          — background task status (poll for progress)

use std::sync::Arc;

use anyhow::Result;
use serde::Deserialize;
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;

use super::Assistant;
use super::jobs::Jobs;

/// The embedded single-page mind-map UI (HTML + CSS + JS, no external assets).
const INDEX_HTML: &str = include_str!("index.html");

#[derive(Deserialize)]
struct AskPayload {
    prompt: String,
}

#[derive(Deserialize)]
struct ShipPayload {
    prompt: String,
    #[serde(default)]
    source: String,
}

#[derive(Deserialize)]
struct ConfirmPayload {
    file: String,
    accept: bool,
}

#[derive(Deserialize)]
struct ConfirmAllPayload {
    accept: bool,
}

#[derive(Deserialize)]
struct FeedbackPayload {
    positive: bool,
    #[serde(default)]
    note: String,
}

#[derive(Deserialize)]
struct PrunePayload {
    /// "file" (forget a file), "architecture" (forget an architecture), or
    /// "tag" (remove one tag from a file).
    kind: String,
    /// The file path or architecture name.
    id: String,
    /// The tag to remove (kind = "tag" only).
    #[serde(default)]
    tag: String,
}

/// Bind the daemon on `127.0.0.1:port`, spawn its accept loop, and return the
/// base URL. The server lives as long as the process.
pub async fn spawn(assistant: Arc<Assistant>, jobs: Arc<Jobs>, port: u16) -> Result<String> {
    let listener = TcpListener::bind(("127.0.0.1", port)).await.map_err(|e| {
        anyhow::anyhow!("Failed to bind 127.0.0.1:{port} ({e}). Try a different --port.")
    })?;
    let url = format!("http://127.0.0.1:{port}/");

    // One question at a time: /api/ask serializes on this lock so concurrent
    // callers can't interleave a single conversation history.
    let ask_gate = Arc::new(tokio::sync::Mutex::new(()));

    tokio::spawn(async move {
        loop {
            let Ok((socket, _)) = listener.accept().await else {
                break;
            };
            let assistant = assistant.clone();
            let jobs = jobs.clone();
            let ask_gate = ask_gate.clone();
            tokio::spawn(async move {
                if let Err(e) = handle(socket, &assistant, &jobs, &ask_gate).await {
                    tracing::debug!("ai server: connection error: {e}");
                }
            });
        }
    });

    Ok(url)
}

async fn handle(
    mut socket: TcpStream,
    assistant: &Assistant,
    jobs: &Arc<Jobs>,
    ask_gate: &tokio::sync::Mutex<()>,
) -> Result<()> {
    let Some(req) = read_request(&mut socket).await? else {
        return Ok(());
    };
    let (path, query) = req.path.split_once('?').unwrap_or((req.path.as_str(), ""));

    let (status, content_type, body): (&str, &str, Vec<u8>) = match (req.method.as_str(), path) {
        ("GET", "/") | ("GET", "/index.html") => {
            ("200 OK", "text/html; charset=utf-8", INDEX_HTML.into())
        }

        ("GET", "/api/graph") => {
            let after: u64 = query
                .split('&')
                .find_map(|kv| kv.strip_prefix("after="))
                .and_then(|v| v.parse().ok())
                .unwrap_or(0);
            // `activity` rides on every response (even unchanged ones) so the
            // page's status line follows a burn-in between map mutations.
            let activity = json!(assistant.activity());
            let body = if assistant.brain.seq() == after {
                json!({"seq": after, "changed": false, "activity": activity})
                    .to_string()
                    .into_bytes()
            } else {
                let mut graph = assistant.brain.graph_json();
                graph["changed"] = json!(true);
                graph["activity"] = activity;
                graph.to_string().into_bytes()
            };
            ("200 OK", "application/json; charset=utf-8", body)
        }

        ("POST", "/api/ask") => match serde_json::from_str::<AskPayload>(&req.body) {
            Ok(p) => {
                // Statuses are collected (not streamed) — the HTTP API is the
                // daemon's programmatic surface, so one JSON reply is enough.
                let _running = ask_gate.lock().await;
                let (tx, mut rx) = mpsc::channel(64);
                let collector = tokio::spawn(async move {
                    let mut steps = Vec::new();
                    let mut suggestions = Vec::new();
                    while let Some(ev) = rx.recv().await {
                        match ev {
                            super::AiEvent::Status(s) => steps.push(s),
                            super::AiEvent::Suggestion(c) => suggestions.push(json!({
                                "file": c.file,
                                "diff": c.diff,
                                "reason": c.reason,
                                "state": c.state.label(),
                            })),
                            _ => {}
                        }
                    }
                    (steps, suggestions)
                });
                match assistant.ask(&p.prompt, tx).await {
                    Ok(answer) => {
                        let (steps, suggestions) = collector.await.unwrap_or_default();
                        let body = json!({
                            "answer": answer,
                            "steps": steps,
                            "suggestions": suggestions,
                            "confidence": assistant.brain.confidence(),
                        });
                        ("200 OK", "application/json; charset=utf-8", body.to_string().into_bytes())
                    }
                    Err(e) => bad_request(&e.to_string()),
                }
            }
            Err(e) => bad_request(&e.to_string()),
        },

        ("POST", "/api/confirm") => match serde_json::from_str::<ConfirmPayload>(&req.body) {
            Ok(p) => match assistant.brain.confirm(&p.file, p.accept) {
                Ok(true) => ok_json(json!({"ok": true})),
                Ok(false) => bad_request("no pending confirmation for that file"),
                Err(e) => bad_request(&e.to_string()),
            },
            Err(e) => bad_request(&e.to_string()),
        },

        ("POST", "/api/confirm-all") => match serde_json::from_str::<ConfirmAllPayload>(&req.body) {
            Ok(p) => match assistant.brain.confirm_all(p.accept) {
                Ok(n) => ok_json(json!({"ok": true, "resolved": n})),
                Err(e) => bad_request(&e.to_string()),
            },
            Err(e) => bad_request(&e.to_string()),
        },

        ("POST", "/api/ship") => match serde_json::from_str::<ShipPayload>(&req.body) {
            Ok(p) => {
                let source = if p.source.trim().is_empty() { "gui" } else { p.source.trim() };
                match jobs.ship(&p.prompt, source) {
                    Ok(id) => ok_json(json!({"ok": true, "id": id})),
                    Err(e) => bad_request(&e.to_string()),
                }
            }
            Err(e) => bad_request(&e.to_string()),
        },

        ("GET", "/api/jobs") => ok_json(jobs.snapshot_json()),

        ("POST", "/api/prune") => match serde_json::from_str::<PrunePayload>(&req.body) {
            Ok(p) => {
                let result = match p.kind.as_str() {
                    "file" => assistant.brain.forget_file(&p.id),
                    "architecture" => assistant.brain.forget_architecture(&p.id),
                    "tag" => assistant.brain.untag_file(&p.id, &p.tag),
                    other => Err(anyhow::anyhow!("unknown prune kind '{other}'")),
                };
                match result {
                    Ok(true) => ok_json(json!({"ok": true})),
                    Ok(false) => bad_request("nothing in the map matches that"),
                    Err(e) => bad_request(&e.to_string()),
                }
            }
            Err(e) => bad_request(&e.to_string()),
        },

        ("POST", "/api/feedback") => match serde_json::from_str::<FeedbackPayload>(&req.body) {
            Ok(p) => {
                let files = assistant.files_touched();
                match assistant.brain.record_feedback(p.positive, files, &p.note) {
                    Ok(c) => ok_json(json!({"ok": true, "confidence": c})),
                    Err(e) => bad_request(&e.to_string()),
                }
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

fn ok_json(v: serde_json::Value) -> (&'static str, &'static str, Vec<u8>) {
    ("200 OK", "application/json; charset=utf-8", v.to_string().into_bytes())
}

fn bad_request(msg: &str) -> (&'static str, &'static str, Vec<u8>) {
    (
        "400 Bad Request",
        "text/plain; charset=utf-8",
        msg.as_bytes().to_vec(),
    )
}

/// Parsed pieces of a request we care about: method, path, and body.
struct Request {
    method: String,
    path: String,
    body: String,
}

/// Read a full HTTP request: the head, then any body indicated by
/// `Content-Length`. Returns `None` on an empty/closed connection.
async fn read_request(socket: &mut TcpStream) -> Result<Option<Request>> {
    let mut buf = Vec::with_capacity(2048);
    let mut chunk = [0u8; 2048];

    let header_end = loop {
        let n = socket.read(&mut chunk).await?;
        if n == 0 {
            if buf.is_empty() {
                return Ok(None);
            }
            break None;
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(pos) = find_subsequence(&buf, b"\r\n\r\n") {
            break Some(pos + 4);
        }
    };

    let head_len = header_end.unwrap_or(buf.len());
    let head = String::from_utf8_lossy(&buf[..head_len]).to_string();

    let mut lines = head.lines();
    let request_line = lines.next().unwrap_or_default();
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("GET").to_string();
    let path = parts.next().unwrap_or("/").to_string();

    let content_length = lines
        .take_while(|l| !l.is_empty())
        .find_map(|l| {
            let (name, value) = l.split_once(':')?;
            name.trim()
                .eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().ok())
                .flatten()
        })
        .unwrap_or(0);

    let mut body_bytes = buf[head_len..].to_vec();
    while body_bytes.len() < content_length {
        let n = socket.read(&mut chunk).await?;
        if n == 0 {
            break;
        }
        body_bytes.extend_from_slice(&chunk[..n]);
    }
    let body = String::from_utf8_lossy(&body_bytes).to_string();

    Ok(Some(Request { method, path, body }))
}

fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
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
