//! The remote cache's admin app.
//!
//! A Vite + React build from `remote-cache-web/`, embedded in the binary by
//! `include_dir!` the same way the daemon embeds `tool_frontend` — so a cache
//! server is still one file, and the page still works on a headless box with
//! no route to the internet: every script, stylesheet and font it loads comes
//! from here, never from a CDN. `build.rs` guarantees the directory exists at
//! compile time, substituting a placeholder page when it hasn't been built.
//!
//! It exists for one job the CLI can't do well — **minting a credential** —
//! and for an operator who has just opened a page about their cache and wants
//! to know whether it's working and what to do if it isn't: how the hit rate
//! is moving, which targets miss most, the latest traffic, what retention
//! will evict next, and the builds and releases reported to it.
//!
//! Unauthenticated, because it has to be: on a server that wants credentials,
//! this is where you sign in to get one. The app itself is inert — every
//! action it offers goes through the API, which does check.

use axum::Json;
use axum::http::{StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use include_dir::{Dir, include_dir};
use serde_json::json;

use crate::daemon::assets::{content_type, looks_like_asset};

/// The built admin app, embedded at compile time.
static DIST: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/remote-cache-web/dist");

/// Serve a file from the bundle, falling back to `index.html` so the app's
/// own routes (`/builds`, `/releases`, …) survive a reload.
pub async fn serve(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');

    // An API path that matched no route is a client bug, and it should hear
    // so in the API's own shape rather than receive a web page.
    if path == "api" || path.starts_with("api/") {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": format!("No such endpoint: /{path}") })),
        )
            .into_response();
    }

    if let Some(file) = DIST.get_file(path) {
        return file_response(path, file.contents());
    }
    if looks_like_asset(path) {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    }
    match DIST.get_file("index.html") {
        Some(file) => file_response("index.html", file.contents()),
        None => (
            StatusCode::INTERNAL_SERVER_ERROR,
            "The admin app is missing from this binary.",
        )
            .into_response(),
    }
}

fn file_response(path: &str, body: &'static [u8]) -> Response {
    // Vite fingerprints everything under assets/, so those never change; the
    // entry point must be re-read so an upgraded server serves its new app.
    let cache = if path.starts_with("assets/") {
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

#[cfg(test)]
mod tests {
    use super::*;

    async fn get(path: &str) -> Response {
        serve(path.parse().unwrap()).await
    }

    #[tokio::test]
    async fn app_routes_fall_back_to_the_entry_point() {
        for path in ["/", "/builds", "/releases", "/users"] {
            let response = get(path).await;
            assert_eq!(response.status(), StatusCode::OK, "{path}");
            assert_eq!(
                response.headers()[header::CONTENT_TYPE],
                "text/html; charset=utf-8"
            );
        }
    }

    #[tokio::test]
    async fn an_unknown_api_path_is_a_json_404_not_the_page() {
        let response = get("/api/nonsense").await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert!(
            response.headers()[header::CONTENT_TYPE]
                .to_str()
                .unwrap()
                .starts_with("application/json")
        );
    }

    #[tokio::test]
    async fn a_missing_asset_is_a_404() {
        let response = get("/assets/nope-1234.js").await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    /// The property the old hand-written page had and the app must keep: it
    /// works on a box with no network, so nothing it loads is fetched from
    /// anywhere but this server.
    #[test]
    fn the_entry_point_loads_nothing_from_elsewhere() {
        let html = String::from_utf8_lossy(DIST.get_file("index.html").unwrap().contents());
        for attr in ["src=\"http", "href=\"http", "src=\"//", "href=\"//"] {
            assert!(
                !html.contains(attr),
                "the admin app must not load anything external ({attr})"
            );
        }
    }
}
