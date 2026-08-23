//! Project registry routes, backing the web app's project switcher.
//!
//! CLI commands `POST /api/projects` with their working directory on every
//! invocation, so opening `ciabatta todo` in a new checkout makes that checkout
//! selectable without any extra step.

use axum::extract::{Path, State};
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
