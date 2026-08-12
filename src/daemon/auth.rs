//! Bearer-token auth for the daemon's API.
//!
//! The old per-command servers were short-lived and tied to a single
//! invocation. The daemon is not: it runs indefinitely and exposes routes that
//! spawn processes (`POST /api/watch/sessions`, `POST /api/run/runs`). On
//! loopback that is no worse than a shell, but `CIABATTA_BIND_HOST=0.0.0.0` is
//! a supported setting for running inside containers — and there it would hand
//! arbitrary command execution to anyone who can reach the port.
//!
//! So every state-changing route requires a token that is generated at startup
//! and stored in the 0600 `~/.ciabatta/daemon.json`. Reading the page implies
//! being able to read that file, so the browser is simply handed the token in a
//! `<meta>` tag; this costs a local user nothing and closes the network hole.
//!
//! `GET /api/health` is exempt so that liveness probes work without the token.

use axum::extract::State;
use axum::http::{Request, StatusCode, header};
use axum::middleware::Next;
use axum::response::Response;
use rand::RngCore;

use super::app::AppState;

/// Generate a fresh 256-bit token, hex encoded.
pub fn generate_token() -> String {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Reject requests that don't carry the daemon's bearer token.
///
/// Applied to `/api/*` except the health route, which is registered outside
/// this layer.
pub async fn require_token(
    State(state): State<AppState>,
    request: Request<axum::body::Body>,
    next: Next,
) -> Result<Response, StatusCode> {
    let presented = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        // EventSource can't set headers, so SSE routes accept the token as a
        // query parameter instead. It only ever travels over loopback.
        .map(str::to_string)
        .or_else(|| token_from_query(request.uri().query()));

    match presented {
        Some(token) if constant_time_eq(&token, &state.token) => Ok(next.run(request).await),
        _ => Err(StatusCode::UNAUTHORIZED),
    }
}

/// Pull `token=...` out of a query string.
fn token_from_query(query: Option<&str>) -> Option<String> {
    let query = query?;
    query.split('&').find_map(|pair| {
        let (k, v) = pair.split_once('=')?;
        (k == "token").then(|| v.to_string())
    })
}

/// Compare two tokens without leaking their common prefix length through
/// timing. Both are hex strings of the same length in practice, but compare
/// defensively anyway.
fn constant_time_eq(a: &str, b: &str) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.bytes()
        .zip(b.bytes())
        .fold(0u8, |acc, (x, y)| acc | (x ^ y))
        == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_are_64_hex_chars_and_distinct() {
        let a = generate_token();
        let b = generate_token();
        assert_eq!(a.len(), 64);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b);
    }

    #[test]
    fn constant_time_eq_matches_string_equality() {
        assert!(constant_time_eq("abc", "abc"));
        assert!(!constant_time_eq("abc", "abd"));
        assert!(!constant_time_eq("abc", "ab"));
        assert!(constant_time_eq("", ""));
    }

    #[test]
    fn extracts_the_token_from_a_query_string() {
        assert_eq!(token_from_query(Some("token=abc")), Some("abc".into()));
        assert_eq!(
            token_from_query(Some("project=p1&token=abc&after=3")),
            Some("abc".into())
        );
        assert_eq!(token_from_query(Some("project=p1")), None);
        assert_eq!(token_from_query(None), None);
    }
}
