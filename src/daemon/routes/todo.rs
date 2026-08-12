//! Todo routes.
//!
//! Deliberately *not* project-scoped: the task list lives in
//! `~/.ciabatta/todos.json` and is personal to the user, independent of which
//! checkout they're looking at. See [`crate::todo`].
//!
//! Every mutation replies with the full refreshed list, which is what the old
//! server did too — it keeps the client from having to reconcile a patch
//! against its own sort order.

use axum::extract::State;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;

use crate::daemon::app::AppState;
use crate::todo::{Priority, Todo};

use super::{RouteError, RouteResult};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/todos", get(list).post(add))
        .route("/api/todos/toggle", post(toggle))
        .route("/api/todos/delete", post(remove))
        .route("/api/todos/priority", post(set_priority))
        .route("/api/todos/edit", post(edit))
        .route("/api/todos/ship", post(ship))
}

#[derive(Deserialize)]
pub struct IdPayload {
    id: u64,
}

#[derive(Deserialize)]
pub struct TextPayload {
    text: String,
}

#[derive(Deserialize)]
pub struct PriorityPayload {
    id: u64,
    priority: Priority,
}

#[derive(Deserialize)]
pub struct EditPayload {
    id: u64,
    text: String,
}

/// Ship a todo to the AI assistant as a background job.
#[derive(Deserialize)]
pub struct ShipPayload {
    id: u64,
    /// Which checkout the assistant should work in. Unlike the rest of this
    /// module, shipping *is* project-scoped: the agent edits files.
    project: String,
}

async fn list(State(state): State<AppState>) -> Json<Vec<Todo>> {
    Json(state.todos.list())
}

async fn add(
    State(state): State<AppState>,
    Json(payload): Json<TextPayload>,
) -> RouteResult<Json<Vec<Todo>>> {
    state
        .todos
        .add(&payload.text)
        .map_err(RouteError::bad_request)?;
    Ok(Json(state.todos.list()))
}

async fn toggle(
    State(state): State<AppState>,
    Json(payload): Json<IdPayload>,
) -> RouteResult<Json<Vec<Todo>>> {
    state
        .todos
        .toggle(payload.id)
        .map_err(RouteError::bad_request)?;
    Ok(Json(state.todos.list()))
}

async fn remove(
    State(state): State<AppState>,
    Json(payload): Json<IdPayload>,
) -> RouteResult<Json<Vec<Todo>>> {
    state
        .todos
        .remove(payload.id)
        .map_err(RouteError::bad_request)?;
    Ok(Json(state.todos.list()))
}

async fn set_priority(
    State(state): State<AppState>,
    Json(payload): Json<PriorityPayload>,
) -> RouteResult<Json<Vec<Todo>>> {
    state
        .todos
        .set_priority(payload.id, payload.priority)
        .map_err(RouteError::bad_request)?;
    Ok(Json(state.todos.list()))
}

async fn edit(
    State(state): State<AppState>,
    Json(payload): Json<EditPayload>,
) -> RouteResult<Json<Vec<Todo>>> {
    state
        .todos
        .set_text(payload.id, &payload.text)
        .map_err(RouteError::bad_request)?;
    Ok(Json(state.todos.list()))
}

/// Hand a todo to the AI assistant to complete in the background.
///
/// This used to be an HTTP round trip: the todo server would POST to the
/// separate `ciabatta ai` daemon on another port, purely so the browser
/// wouldn't have to make a cross-origin request. Both now live in this
/// process, so it's a direct call — and there's no longer a case where the
/// todo app is up but the AI daemon isn't.
async fn ship(
    State(state): State<AppState>,
    Json(payload): Json<ShipPayload>,
) -> RouteResult<Json<serde_json::Value>> {
    let Some(text) = state.todos.text_of(payload.id) else {
        return Err(RouteError::not_found(format!("No todo #{}", payload.id)));
    };

    let project = state
        .projects
        .get(&payload.project)
        .ok_or_else(|| RouteError::bad_request(format!("Unknown project {}", payload.project)))?;

    let jobs = state.ai_jobs(&project)?;
    let job_id = jobs.ship(&text, &format!("todo:{}", payload.id))?;

    Ok(Json(serde_json::json!({ "ok": true, "job": job_id })))
}

#[cfg(test)]
mod tests {
    use crate::daemon::app::router;
    use crate::daemon::app::tests::test_state;

    use axum::body::Body;
    use axum::http::{Request, StatusCode, header};
    use tower::ServiceExt;

    #[tokio::test]
    async fn listing_todos_requires_the_token() {
        let app = router(test_state());
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/todos")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn listing_todos_returns_an_array() {
        let app = router(test_state());
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/todos")
                    .header(header::AUTHORIZATION, "Bearer test-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(parsed.is_array(), "expected a JSON array, got {parsed}");
    }

    #[tokio::test]
    async fn shipping_an_unknown_todo_is_a_404() {
        let app = router(test_state());
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/todos/ship")
                    .header(header::AUTHORIZATION, "Bearer test-token")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"id":99999999,"project":"nope"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }
}
