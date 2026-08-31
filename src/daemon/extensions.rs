//! Serving the packaged editor extensions.
//!
//! The VS Code extension isn't on the Marketplace, so until now the only way
//! to get it was to clone the repository and build it. That is a lot to ask of
//! someone who wants field completion in a YAML file. The daemon is already
//! running on their machine, so it carries the `.vsix` and hands it over.
//!
//! Embedded rather than fetched, for the same reason the schemas are: it works
//! on a machine with no route to github.com, and — more usefully — the file it
//! gives you was built from the commit that built the binary serving it. A
//! version skew between the extension and the `ciabatta lsp` it launches is
//! exactly the class of bug that is miserable to diagnose.
//!
//! **Deliberately unauthenticated**, like [`super::schemas`]. The web app
//! turns these into ordinary download links, and a browser following one sends
//! no token; the payload is a public release artifact either way.
//!
//! A build that hasn't run `yarn package` carries nothing here, and that is a
//! supported state — [`available`] returns an empty list and the web app links
//! to the releases page instead. See `build.rs` for why there is no
//! placeholder file.

use axum::Router;
use axum::extract::Path;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use include_dir::{Dir, include_dir};

use super::app::AppState;

/// The packaged extensions, embedded at compile time. Produced by
/// `yarn package`; empty on a build that skipped it.
static EXTENSIONS: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/editors/dist");

/// One downloadable extension.
#[derive(serde::Serialize)]
pub struct Extension {
    /// The filename, which is also the last segment of its download URL.
    pub file: String,
    /// Bytes, so the web app can show a size next to the button.
    pub bytes: usize,
}

/// Routes for the extension files, under `/extensions/…`. Merged into the
/// daemon's *public* layer.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/extensions", get(list))
        .route("/extensions/{file}", get(serve))
}

/// What this binary carries. The web app asks before drawing any buttons,
/// because the answer differs between a release build and a `cargo build`.
async fn list() -> Response {
    axum::Json(available()).into_response()
}

/// Serve one extension by filename.
async fn serve(Path(file): Path<String>) -> Response {
    // `include_dir` only knows the files compiled into it, so traversal can't
    // reach anything — but a name with a separator is still worth refusing on
    // sight rather than looking up.
    if file.contains('/') || file.contains('\\') {
        return (StatusCode::NOT_FOUND, "no such extension").into_response();
    }

    let Some(contents) = EXTENSIONS.get_file(&file).map(|f| f.contents()) else {
        let names: Vec<String> = available().into_iter().map(|e| e.file).collect();
        let detail = if names.is_empty() {
            "This binary was built without `yarn package`, so it carries none.".to_string()
        } else {
            format!("Available: {}", names.join(", "))
        };
        return (
            StatusCode::NOT_FOUND,
            format!("no such extension: {file}\n{detail}"),
        )
            .into_response();
    };

    (
        StatusCode::OK,
        [
            // Not `application/vsix`: there is no registered type for it, and
            // an unknown one makes some browsers render the bytes instead of
            // saving them.
            (header::CONTENT_TYPE, "application/octet-stream".to_string()),
            // Without this the browser saves the file under the last path
            // segment, which is right here but would stop being right the
            // moment a link acquires a query string.
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{file}\""),
            ),
            // These change only when the binary does, and the binary is the
            // thing serving them, so there is nothing to go stale against.
            (header::CACHE_CONTROL, "public, max-age=300".to_string()),
        ],
        contents,
    )
        .into_response()
}

/// Every extension the binary carries, sorted by filename.
pub fn available() -> Vec<Extension> {
    let mut found: Vec<Extension> = EXTENSIONS
        .files()
        .filter_map(|f| {
            Some(Extension {
                file: f.path().file_name()?.to_str()?.to_string(),
                bytes: f.contents().len(),
            })
        })
        .collect();
    found.sort_by(|a, b| a.file.cmp(&b.file));
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Not "the vsix is present": a `cargo build` without `yarn package` is a
    /// supported build, and asserting otherwise would fail for every
    /// contributor who hasn't run the JS half. What must hold is that whatever
    /// is there is packaged output and not, say, a stray lockfile.
    #[test]
    fn everything_carried_is_a_packaged_extension() {
        for extension in available() {
            assert!(
                extension.file.ends_with(".vsix"),
                "editors/dist holds {}, which is not a packaged extension",
                extension.file,
            );
            assert!(
                extension.bytes > 0,
                "{} is empty, so packaging produced a broken file",
                extension.file,
            );
        }
    }

    #[tokio::test]
    async fn a_traversal_attempt_is_refused_rather_than_resolved() {
        let response = serve(Path("../Cargo.toml".to_string())).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn a_missing_extension_says_what_there_is_instead() {
        let response = serve(Path("ciabatta-emacs.vsix".to_string())).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }
}
