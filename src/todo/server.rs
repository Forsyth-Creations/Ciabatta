//! A tiny, dependency-free HTTP server for the todo web app.
//!
//! It serves the embedded single-page UI at `/` and a small JSON API under
//! `/api/todos` (list / add / toggle / delete). Every mutating request returns
//! the full, updated list so the front end can simply re-render.

use std::sync::Arc;

use anyhow::Result;
use serde::Deserialize;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use super::Store;

/// The embedded single-page app (HTML + CSS + JS, no external assets).
const INDEX_HTML: &str = include_str!("index.html");

/// Serve the todo app on `127.0.0.1:port` until the process is interrupted.
pub async fn serve(store: Arc<Store>, port: u16) -> Result<()> {
    let listener = TcpListener::bind(("127.0.0.1", port)).await.map_err(|e| {
        anyhow::anyhow!("Failed to bind 127.0.0.1:{port} ({e}). Try a different --port.")
    })?;

    println!("\nTodo app ready at http://127.0.0.1:{port}");
    println!("Press Ctrl-C to stop.");

    loop {
        let (socket, _) = listener.accept().await?;
        let store = store.clone();
        tokio::spawn(async move {
            if let Err(e) = handle(socket, &store).await {
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

async fn handle(mut socket: TcpStream, store: &Store) -> Result<()> {
    let Some(req) = read_request(&mut socket).await? else {
        return Ok(());
    };

    let (status, content_type, body): (&str, &str, Vec<u8>) =
        match (req.method.as_str(), req.path.as_str()) {
            ("GET", "/") | ("GET", "/index.html") => {
                ("200 OK", "text/html; charset=utf-8", INDEX_HTML.into())
            }
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
