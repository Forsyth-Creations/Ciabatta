//! A tiny, dependency-free HTTP server for the analyze view.
//!
//! It serves exactly two things: the embedded single-page UI at `/`, and the
//! analysis JSON at `/graph.json`. That's all the view needs, so there's no
//! reason to pull in a full web framework.

use anyhow::Result;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// The embedded single-page app (HTML + CSS + JS, no external assets).
const INDEX_HTML: &str = include_str!("index.html");

/// Serve the graph on the configured bind host and `port` until the process is
/// interrupted.
pub async fn serve(graph_json: String, port: u16) -> Result<()> {
    let host = crate::config::bind_host();
    let listener = TcpListener::bind((host.as_str(), port)).await.map_err(|e| {
        anyhow::anyhow!("Failed to bind {host}:{port} ({e}). Try a different --port.")
    })?;

    println!("\nAnalyze view ready at http://{host}:{port}");
    println!("Press Ctrl-C to stop.");

    let json = std::sync::Arc::new(graph_json);
    loop {
        let (socket, _) = listener.accept().await?;
        let json = json.clone();
        tokio::spawn(async move {
            if let Err(e) = handle(socket, &json).await {
                eprintln!("analyze server: connection error: {e}");
            }
        });
    }
}

async fn handle(mut socket: TcpStream, graph_json: &str) -> Result<()> {
    // GET requests are small; one read of the head is enough to get the path.
    let mut buf = [0u8; 2048];
    let n = socket.read(&mut buf).await?;
    if n == 0 {
        return Ok(());
    }
    let request = String::from_utf8_lossy(&buf[..n]);
    let path = request.split_whitespace().nth(1).unwrap_or("/");

    let (status, content_type, body): (&str, &str, &[u8]) = if path.starts_with("/graph.json") {
        (
            "200 OK",
            "application/json; charset=utf-8",
            graph_json.as_bytes(),
        )
    } else if path == "/" || path.starts_with("/index") {
        ("200 OK", "text/html; charset=utf-8", INDEX_HTML.as_bytes())
    } else {
        ("404 Not Found", "text/plain; charset=utf-8", b"not found")
    };

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
