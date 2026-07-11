//! A tiny, dependency-free HTTP server for the todo web app.
//!
//! It serves the embedded single-page UI at `/` and a small JSON API under
//! `/api/todos` (list / add / toggle / delete). Every mutating request returns
//! the full, updated list so the front end can simply re-render.
//!
//! It also bridges to the `ciabatta ai` daemon: `/api/ai-status` reports
//! whether that daemon is reachable, and `/api/ship` forwards a todo to it as a
//! background task. The forwarding happens here (server-side) so the browser
//! never makes a cross-origin request to the AI daemon.

use std::sync::Arc;

use anyhow::Result;
use serde::Deserialize;
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use super::{Priority, Store};

/// The embedded single-page app (HTML + CSS + JS, no external assets).
const INDEX_HTML: &str = include_str!("index.html");

/// Serve the todo app on `127.0.0.1:port` until the process is interrupted.
/// `ai_port` is where the `ciabatta ai` daemon is expected to be listening, so
/// the app can offer to ship tasks to it.
pub async fn serve(store: Arc<Store>, port: u16, ai_port: u16) -> Result<()> {
    let listener = TcpListener::bind(("127.0.0.1", port)).await.map_err(|e| {
        anyhow::anyhow!("Failed to bind 127.0.0.1:{port} ({e}). Try a different --port.")
    })?;

    println!("\nTodo app ready at http://127.0.0.1:{port}");
    println!("Ship-to-AI targets the ciabatta ai daemon on port {ai_port}.");
    println!("Press Ctrl-C to stop.");

    loop {
        let (socket, _) = listener.accept().await?;
        let store = store.clone();
        tokio::spawn(async move {
            if let Err(e) = handle(socket, &store, ai_port).await {
                eprintln!("todo server: connection error: {e}");
            }
        });
    }
}

/// Parsed pieces of a request we care about: method, path, and body.
struct Request {
    method: String,
    path: String,
    body: String,
}

/// A JSON payload carrying just an id (toggle / delete).
#[derive(Deserialize)]
struct IdPayload {
    id: u64,
}

/// A JSON payload carrying task text (add).
#[derive(Deserialize)]
struct TextPayload {
    text: String,
}

/// A JSON payload setting a task's priority.
#[derive(Deserialize)]
struct PriorityPayload {
    id: u64,
    priority: Priority,
}

/// A JSON payload replacing a task's text (edit).
#[derive(Deserialize)]
struct EditPayload {
    id: u64,
    text: String,
}

async fn handle(mut socket: TcpStream, store: &Store, ai_port: u16) -> Result<()> {
    let Some(req) = read_request(&mut socket).await? else {
        return Ok(());
    };

    let (status, content_type, body): (&str, &str, Vec<u8>) =
        match (req.method.as_str(), req.path.as_str()) {
            ("GET", "/") | ("GET", "/index.html") => {
                ("200 OK", "text/html; charset=utf-8", INDEX_HTML.into())
            }
            ("GET", "/api/ai-status") => {
                let running = ai_daemon_reachable(ai_port).await;
                ok_json(json!({ "running": running, "port": ai_port }))
            }
            ("POST", "/api/ship") => ship_to_ai(store, &req.body, ai_port).await,
            ("GET", "/api/todos") => json_response(store),
            ("POST", "/api/todos") => match serde_json::from_str::<TextPayload>(&req.body) {
                Ok(p) => match store.add(&p.text) {
                    Ok(_) => json_response(store),
                    Err(e) => bad_request(&e.to_string()),
                },
                Err(e) => bad_request(&e.to_string()),
            },
            ("POST", "/api/todos/toggle") => mutate(store, &req.body, |s, id| s.toggle(id)),
            ("POST", "/api/todos/delete") => mutate(store, &req.body, |s, id| s.remove(id)),
            ("POST", "/api/todos/priority") => {
                match serde_json::from_str::<PriorityPayload>(&req.body) {
                    Ok(p) => match store.set_priority(p.id, p.priority) {
                        Ok(()) => json_response(store),
                        Err(e) => bad_request(&e.to_string()),
                    },
                    Err(e) => bad_request(&e.to_string()),
                }
            }
            ("POST", "/api/todos/edit") => match serde_json::from_str::<EditPayload>(&req.body) {
                Ok(p) => match store.set_text(p.id, &p.text) {
                    Ok(()) => json_response(store),
                    Err(e) => bad_request(&e.to_string()),
                },
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

/// Run an id-based mutation and reply with the refreshed list.
fn mutate(
    store: &Store,
    body: &str,
    op: impl FnOnce(&Store, u64) -> Result<()>,
) -> (&'static str, &'static str, Vec<u8>) {
    match serde_json::from_str::<IdPayload>(body) {
        Ok(p) => match op(store, p.id) {
            Ok(()) => json_response(store),
            Err(e) => bad_request(&e.to_string()),
        },
        Err(e) => bad_request(&e.to_string()),
    }
}

/// The current task list, serialized as a JSON 200 response.
fn json_response(store: &Store) -> (&'static str, &'static str, Vec<u8>) {
    let body = serde_json::to_vec(&store.list()).unwrap_or_else(|_| b"[]".to_vec());
    ("200 OK", "application/json; charset=utf-8", body)
}

/// A JSON value as a 200 response.
fn ok_json(v: serde_json::Value) -> (&'static str, &'static str, Vec<u8>) {
    ("200 OK", "application/json; charset=utf-8", v.to_string().into_bytes())
}

/// Best-effort probe of the `ciabatta ai` daemon: it answers `GET /api/jobs`
/// only when it is up, so a quick, short-timeout request tells us if the
/// ship-to-AI button should be offered.
async fn ai_daemon_reachable(ai_port: u16) -> bool {
    let Ok(client) = reqwest::Client::builder()
        .timeout(std::time::Duration::from_millis(400))
        .build()
    else {
        return false;
    };
    client
        .get(format!("http://127.0.0.1:{ai_port}/api/jobs"))
        .send()
        .await
        .map(|r| r.status().is_success())
        .unwrap_or(false)
}

/// Forward a todo to the AI daemon as a background task. The browser posts
/// `{"id": <todo id>}`; we resolve the task text here and hand it to the
/// daemon's `/api/ship`, tagging the source as `todo:<id>`.
async fn ship_to_ai(
    store: &Store,
    body: &str,
    ai_port: u16,
) -> (&'static str, &'static str, Vec<u8>) {
    let id = match serde_json::from_str::<IdPayload>(body) {
        Ok(p) => p.id,
        Err(e) => return bad_request(&e.to_string()),
    };
    let Some(text) = store.text_of(id) else {
        return bad_request(&format!("no todo #{id}"));
    };

    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
    {
        Ok(c) => c,
        Err(e) => return bad_request(&e.to_string()),
    };
    let resp = client
        .post(format!("http://127.0.0.1:{ai_port}/api/ship"))
        .json(&json!({ "prompt": text, "source": format!("todo:{id}") }))
        .send()
        .await;

    match resp {
        Ok(r) if r.status().is_success() => {
            let job = r.json::<serde_json::Value>().await.unwrap_or_else(|_| json!({}));
            ok_json(json!({ "ok": true, "job": job }))
        }
        Ok(r) => {
            let code = r.status();
            let msg = r.text().await.unwrap_or_default();
            bad_request(&format!("AI daemon returned {code}: {msg}"))
        }
        Err(_) => bad_request(
            "The ciabatta ai daemon isn't reachable. Start it with `ciabatta ai` \
             in your project, then try again.",
        ),
    }
}

/// A 400 response carrying a short plain-text message.
fn bad_request(msg: &str) -> (&'static str, &'static str, Vec<u8>) {
    (
        "400 Bad Request",
        "text/plain; charset=utf-8",
        msg.as_bytes().to_vec(),
    )
}

/// Read a full HTTP request: the head, then any body indicated by
/// `Content-Length`. Returns `None` on an empty/closed connection.
async fn read_request(socket: &mut TcpStream) -> Result<Option<Request>> {
    let mut buf = Vec::with_capacity(2048);
    let mut chunk = [0u8; 2048];

    // Read until we've seen the end of the headers (a blank line).
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

    // Pull the body length from the headers, then read the rest of the body.
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

/// Byte-level substring search (for locating the header/body boundary).
fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// Write a complete HTTP/1.1 response and close the connection.
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
