//! Serves the embedded `tool_frontend` bundle.
//!
//! The whole Vite build is baked into the binary by `include_dir!`, so a
//! released ciabatta is still a single file with no runtime asset directory.
//! `build.rs` guarantees the directory exists at compile time, substituting a
//! placeholder page when the bundle hasn't been built.
//!
//! Two behaviours matter here:
//!
//! * **SPA history fallback** — TanStack Router owns paths like `/watch/3`, so
//!   any unmatched non-asset path returns `index.html` and lets the client
//!   route it.
//! * **Token injection** — `index.html` is rewritten on the way out to carry
//!   the daemon's API token in a `<meta>` tag, which is how the web app
//!   authenticates without a login flow. See [`super::auth`].

use axum::extract::State;
use axum::http::{StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use include_dir::{Dir, include_dir};

use super::app::AppState;

/// The built web app, embedded at compile time.
static DIST: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/tool_frontend/dist");

/// Serve a static asset, falling back to `index.html` so client-side routes
/// survive a page reload.
pub async fn serve(State(state): State<AppState>, uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');

    if let Some(file) = DIST.get_file(path) {
        return asset_response(path, file.contents());
    }

    // Anything that looks like a file but wasn't found is a genuine 404 —
    // falling back to index.html for a missing .js would produce a confusing
    // "unexpected token '<'" error in the console instead.
    if looks_like_asset(path) {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    }

    index_html(&state.token)
}

/// The SPA entry point, with the API token injected.
pub fn index_html(token: &str) -> Response {
    let Some(file) = DIST.get_file("index.html") else {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            "The web app bundle is missing from this binary.",
        )
            .into_response();
    };

    let html = String::from_utf8_lossy(file.contents());
    let injected = inject_token(&html, token);

    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "text/html; charset=utf-8"),
            // The token is embedded, so the page must never be cached to disk
            // by a shared proxy or survive a daemon restart with a stale token.
            (header::CACHE_CONTROL, "no-store"),
        ],
        injected,
    )
        .into_response()
}

/// Insert `<meta name="ciabatta-token" content="...">` into the document head.
fn inject_token(html: &str, token: &str) -> String {
    let meta = format!(r#"<meta name="ciabatta-token" content="{token}">"#);
    match html.find("<head>") {
        Some(idx) => {
            let split = idx + "<head>".len();
            format!("{}{meta}{}", &html[..split], &html[split..])
        }
        // Vite always emits a <head>, but a hand-written placeholder might not.
        None => format!("{meta}{html}"),
    }
}

/// A static asset with a long-lived cache header when the filename is hashed.
fn asset_response(path: &str, body: &'static [u8]) -> Response {
    let cache = if path.starts_with("assets/") {
        // Vite fingerprints everything under assets/, so these are immutable.
        "public, max-age=31536000, immutable"
    } else {
        "no-cache"
    };

    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, content_type(path)),
            (header::CACHE_CONTROL, cache),
        ],
        body,
    )
        .into_response()
}

/// Whether a path should 404 rather than fall through to the SPA.
fn looks_like_asset(path: &str) -> bool {
    path.rsplit('/')
        .next()
        .is_some_and(|name| name.contains('.'))
}

/// Content type by extension. Small hand-rolled table — the bundle only ever
/// contains a handful of asset kinds, and this avoids a mime dependency.
fn content_type(path: &str) -> &'static str {
    match path.rsplit('.').next().unwrap_or("") {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" | "map" => "application/json; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "ico" => "image/x-icon",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "txt" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn injects_the_token_into_the_head() {
        let html = "<!doctype html><html><head><title>x</title></head><body></body></html>";
        let out = inject_token(html, "deadbeef");
        assert!(out.contains(r#"<meta name="ciabatta-token" content="deadbeef">"#));
        // It must land inside <head>, before the existing children.
        let meta_at = out.find("ciabatta-token").unwrap();
        assert!(meta_at > out.find("<head>").unwrap());
        assert!(meta_at < out.find("<title>").unwrap());
    }

    #[test]
    fn injects_even_without_a_head_tag() {
        let out = inject_token("<h1>placeholder</h1>", "abc");
        assert!(out.contains(r#"content="abc""#));
        assert!(out.contains("<h1>placeholder</h1>"));
    }

    #[test]
    fn distinguishes_assets_from_spa_routes() {
        assert!(looks_like_asset("assets/index-a1b2.js"));
        assert!(looks_like_asset("favicon.ico"));
        assert!(!looks_like_asset("watch/3"));
        assert!(!looks_like_asset("run/builder"));
        assert!(!looks_like_asset(""));
    }

    #[test]
    fn maps_extensions_to_content_types() {
        assert_eq!(
            content_type("assets/app-1a2b.js"),
            "text/javascript; charset=utf-8"
        );
        assert_eq!(
            content_type("assets/app-1a2b.css"),
            "text/css; charset=utf-8"
        );
        assert_eq!(content_type("logo.svg"), "image/svg+xml");
        assert_eq!(content_type("weird.xyz"), "application/octet-stream");
    }
}
