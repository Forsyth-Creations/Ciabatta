//! Project registry routes, backing the web app's project switcher.
//!
//! CLI commands `POST /api/projects` with their working directory on every
//! invocation, so opening `ciabatta todo` in a new checkout makes that checkout
//! selectable without any extra step.

use axum::extract::{Path, Query, State};
use axum::http::header;
use axum::response::IntoResponse;
use axum::routing::{delete, get};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::json;

use crate::daemon::app::AppState;
use crate::daemon::projects::Project;

use super::{RouteError, RouteResult};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/projects", get(list).post(register))
        .route("/api/projects/{id}", delete(forget))
        .route("/api/projects/{id}/readme", get(readme))
        .route("/api/projects/{id}/readme/asset", get(readme_asset))
}

/// The names a README goes by, in the order they're looked for.
const README_NAMES: &[&str] = &["README.md", "readme.md", "Readme.md", "README.markdown"];

/// A README bigger than this is not one anybody reads in a browser tab.
const README_LIMIT: u64 = 2 * 1024 * 1024;

/// The project's Markdown README, as text: `{ file, text }`, or 404 when the
/// project root has none.
async fn readme(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> RouteResult<Json<serde_json::Value>> {
    let project = state
        .projects
        .get(&id)
        .ok_or_else(|| RouteError::not_found(format!("No project {id}")))?;
    for name in README_NAMES {
        let path = project.path.join(name);
        let Ok(meta) = std::fs::metadata(&path) else {
            continue;
        };
        if !meta.is_file() {
            continue;
        }
        if meta.len() > README_LIMIT {
            return Err(RouteError::bad_request(format!(
                "{name} is {} MB — too large to show here",
                meta.len() / (1024 * 1024)
            )));
        }
        let text = std::fs::read_to_string(&path)
            .map_err(|e| RouteError::bad_request(format!("Couldn't read {name}: {e}")))?;
        return Ok(Json(json!({ "file": name, "text": text })));
    }
    Err(RouteError::not_found("This project has no README.md"))
}

#[derive(Deserialize)]
pub struct AssetQuery {
    /// Relative to the project root, as the README writes it.
    path: String,
}

/// An image the README shows, by its path relative to the project root.
///
/// Images only, and only from inside the project: this exists so a README's
/// `![diagram](docs/arch.png)` renders, not to serve the checkout.
async fn readme_asset(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<AssetQuery>,
) -> RouteResult<impl IntoResponse> {
    let project = state
        .projects
        .get(&id)
        .ok_or_else(|| RouteError::not_found(format!("No project {id}")))?;
    let content_type = image_type(&query.path)
        .ok_or_else(|| RouteError::bad_request("Only images can be served from a README"))?;
    let root = project
        .path
        .canonicalize()
        .map_err(|e| RouteError::not_found(e.to_string()))?;
    let file = root
        .join(query.path.trim_start_matches('/'))
        .canonicalize()
        .map_err(|_| RouteError::not_found(format!("No file {}", query.path)))?;
    // Canonical on both sides, so `..` and symlinks can't climb out.
    if !file.starts_with(&root) || !file.is_file() {
        return Err(RouteError::not_found(format!("No file {}", query.path)));
    }
    let bytes = std::fs::read(&file).map_err(|e| RouteError::not_found(e.to_string()))?;
    Ok(([(header::CONTENT_TYPE, content_type)], bytes))
}

/// The media type of an image a README can show, by extension.
fn image_type(path: &str) -> Option<&'static str> {
    let extension = std::path::Path::new(path)
        .extension()?
        .to_str()?
        .to_ascii_lowercase();
    Some(match extension.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        _ => return None,
    })
}

#[derive(Deserialize)]
pub struct RegisterPayload {
    /// A directory anywhere inside the checkout. The nearest ancestor with a
    /// `.ciabatta/` directory becomes the project root.
    pub path: String,
}

async fn list(State(state): State<AppState>) -> Json<Vec<Project>> {
    Json(state.projects.list())
}

async fn register(
    State(state): State<AppState>,
    Json(payload): Json<RegisterPayload>,
) -> RouteResult<Json<Project>> {
    let project = state
        .projects
        .register(std::path::Path::new(&payload.path))
        .map_err(RouteError::bad_request)?;
    Ok(Json(project))
}

/// Forget a project.
///
/// Its tasks are promoted to the global list rather than left behind. A task
/// attached to an id nothing resolves any more would be in the file but in no
/// list — deleted in effect but not in fact — and losing somebody's notes
/// because they tidied up their project switcher is not a trade worth making.
async fn forget(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> RouteResult<Json<serde_json::Value>> {
    let removed = state.projects.forget(&id)?;
    if !removed {
        return Err(RouteError::not_found(format!("No project {id}")));
    }

    let promoted = state.todos.globalize(&id)?;
    Ok(Json(json!({ "ok": true, "todos_made_global": promoted })))
}

#[cfg(test)]
mod tests {
    use crate::daemon::app::router;
    use crate::daemon::app::tests::test_state;

    use axum::body::Body;
    use axum::http::{Request, StatusCode, header};
    use tower::ServiceExt;

    #[tokio::test]
    async fn registering_a_missing_directory_is_a_client_error() {
        let app = router(test_state());
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/projects")
                    .header(header::AUTHORIZATION, "Bearer test-token")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"path":"/definitely/not/here"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn an_unknown_projects_readme_is_not_found() {
        let app = router(test_state());
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/projects/no-such-project/readme")
                    .header(header::AUTHORIZATION, "Bearer test-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[test]
    fn a_readme_can_show_images_and_nothing_else() {
        assert_eq!(super::image_type("docs/arch.PNG"), Some("image/png"));
        assert_eq!(super::image_type("logo.svg"), Some("image/svg+xml"));
        assert_eq!(super::image_type(".env"), None);
        assert_eq!(super::image_type("src/main.rs"), None);
        assert_eq!(super::image_type("no-extension"), None);
    }

    #[tokio::test]
    async fn listing_projects_requires_the_token() {
        let app = router(test_state());
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/projects")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }
}
