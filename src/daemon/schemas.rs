//! Serving the JSON Schemas for ciabatta's config files.
//!
//! These describe the *shape* of `.ciabatta/ciabatta.yaml` and
//! `.ciabatta/workflows/*.yaml` — every field, what it takes, what it's for —
//! and they're what gives an editor field completion and hover documentation.
//! `editors/vscode` ships its own copy because a `.vsix` can only carry files
//! from inside itself; every other editor needs them from somewhere, and the
//! daemon is already running on the developer's machine.
//!
//! **Deliberately unauthenticated.** The consumer is `yaml-language-server`,
//! fetching a URL out of a settings file with no idea the daemon has a token —
//! and a schema is a public description of a file format, with nothing in it
//! worth protecting. They're mounted outside `/api` for the same reason: the
//! URL goes in someone's editor config, and it should read like a document.
//!
//! `ciabatta.schema.json` and `workflow.schema.json` both `$ref` into
//! `common.schema.json` by relative path, so all three have to be served from
//! one directory for those references to resolve. That's what makes this a
//! route per directory rather than a route per file.

use axum::Router;
use axum::extract::Path;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use include_dir::{Dir, include_dir};

use super::app::AppState;

/// The schemas, embedded at compile time so a released binary carries them.
static SCHEMAS: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/editors/schemas");

/// Routes for the schema files, under `/schemas/<name>.json`. Merged into the
/// daemon's *public* layer.
///
/// Editor settings quote these URLs, so treat the path as stable rather than
/// as an implementation detail.
pub fn router() -> Router<AppState> {
    Router::new().route("/schemas/{file}", get(serve))
}

/// Serve one schema by filename.
async fn serve(Path(file): Path<String>) -> Response {
    // No path traversal: `include_dir` only knows the files compiled into it,
    // but a name with a separator would still be worth refusing on sight.
    if file.contains('/') || file.contains('\\') || !file.ends_with(".json") {
        return (StatusCode::NOT_FOUND, "no such schema").into_response();
    }

    let Some(contents) = SCHEMAS.get_file(&file).map(|f| f.contents()) else {
        return (
            StatusCode::NOT_FOUND,
            format!(
                "no such schema: {file}\nAvailable: {}",
                available().join(", ")
            ),
        )
            .into_response();
    };

    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "application/schema+json"),
            // An editor fetches these on every project open. They change only
            // when ciabatta itself does, and the URL is loopback, so a short
            // cache is enough to stay out of the way without pinning a stale
            // copy across an upgrade.
            (header::CACHE_CONTROL, "public, max-age=300"),
            // Zed's language server and any browser-based tool fetch these
            // cross-origin from a page this daemon didn't serve.
            (header::ACCESS_CONTROL_ALLOW_ORIGIN, "*"),
        ],
        contents,
    )
        .into_response()
}

/// Every schema filename the binary carries, sorted — what the web app lists
/// as downloads, and what a 404 suggests.
pub fn available() -> Vec<String> {
    let mut names: Vec<String> = SCHEMAS
        .files()
        .filter_map(|f| f.path().file_name()?.to_str().map(str::to_string))
        .collect();
    names.sort();
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_binary_carries_every_schema_the_editors_need() {
        let names = available();
        for expected in [
            "ciabatta.schema.json",
            "common.schema.json",
            "workflow.schema.json",
        ] {
            assert!(
                names.iter().any(|n| n == expected),
                "{expected} is missing from the embedded schemas: {names:?}",
            );
        }
    }

    #[test]
    fn the_embedded_schemas_are_the_files_the_editors_ship() {
        for name in available() {
            let embedded = SCHEMAS.get_file(&name).expect("listed").contents();
            let source = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("editors/schemas")
                .join(&name);
            let on_disk = std::fs::read(&source).expect("schema should be readable");
            assert_eq!(
                embedded, on_disk,
                "{name} was embedded from somewhere other than editors/schemas/",
            );
        }
    }

    #[tokio::test]
    async fn a_traversal_attempt_is_refused_rather_than_resolved() {
        let response = serve(Path("../Cargo.toml".to_string())).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }
}
