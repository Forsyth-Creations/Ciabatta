//! Todo routes.
//!
//! Scoped, like every other feature route: the `project` query parameter (or
//! body field) selects which list you're looking at, so the web app's project
//! switcher drives it. Omitting it selects the **global** list — the tasks that
//! belong to no project — which is what the dashboard shows. See
//! [`crate::todo`].
//!
//! Every mutation replies with the full refreshed list *for that scope*, which
//! keeps the client from having to reconcile a patch against its own sort order
//! — and means an edit can't leave the list showing another project's tasks.
//! That also makes `/scope` behave correctly for free: promoting a task to
//! global hands back the project list it just left, which is exactly the list
//! the caller is looking at.

use axum::extract::{Query, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;

use crate::daemon::app::AppState;
use crate::todo::{Priority, Scope, Todo};

use super::{RouteError, RouteResult};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/todos", get(list).post(add))
        .route("/api/todos/toggle", post(toggle))
        .route("/api/todos/delete", post(remove))
        .route("/api/todos/priority", post(set_priority))
        .route("/api/todos/edit", post(edit))
        .route("/api/todos/scope", post(set_scope))
        .route("/api/todos/ship", post(ship))
}

/// Which list to act on. An absent or empty `project` means the global list.
#[derive(Debug, Default, Deserialize)]
pub struct ProjectQuery {
    #[serde(default)]
    pub project: Option<String>,
}

impl ProjectQuery {
    fn scope(&self) -> Scope {
        Scope::from_query(self.project.as_deref())
    }
}

#[derive(Deserialize)]
pub struct IdPayload {
    id: u64,
    #[serde(default)]
    project: Option<String>,
}

#[derive(Deserialize)]
pub struct TextPayload {
    text: String,
    #[serde(default)]
    project: Option<String>,
}

#[derive(Deserialize)]
pub struct PriorityPayload {
    id: u64,
    priority: Priority,
    #[serde(default)]
    project: Option<String>,
}

#[derive(Deserialize)]
pub struct EditPayload {
    id: u64,
    text: String,
    #[serde(default)]
    project: Option<String>,
}

/// Move a task between a project and the global list.
#[derive(Deserialize)]
pub struct ScopePayload {
    id: u64,
    /// Where the task should live now: a project id, or absent for global.
    #[serde(default)]
    target: Option<String>,
    /// The list the caller is looking at, so the reply refreshes it.
    #[serde(default)]
    project: Option<String>,
}

/// The list a mutation should reply with.
fn scope(project: &Option<String>) -> Scope {
    Scope::from_query(project.as_deref())
}

/// A project id from a request field, treating empty as absent.
fn project_id(value: &Option<String>) -> Option<&str> {
    value.as_deref().map(str::trim).filter(|p| !p.is_empty())
}

/// Ship a todo to the AI assistant as a background job.
#[derive(Deserialize)]
pub struct ShipPayload {
    id: u64,
    /// Which checkout the assistant should work in. Unlike the rest of this
    /// module, shipping *is* project-scoped: the agent edits files.
    project: String,
}

async fn list(State(state): State<AppState>, Query(query): Query<ProjectQuery>) -> Json<Vec<Todo>> {
    Json(state.todos.list(&query.scope()))
}

async fn add(
    State(state): State<AppState>,
    Json(payload): Json<TextPayload>,
) -> RouteResult<Json<Vec<Todo>>> {
    let project = project_id(&payload.project);
    state
        .todos
        .add(&payload.text, project)
        .map_err(RouteError::bad_request)?;
    Ok(Json(state.todos.list(&Scope::of(project))))
}

async fn toggle(
    State(state): State<AppState>,
    Json(payload): Json<IdPayload>,
) -> RouteResult<Json<Vec<Todo>>> {
    state
        .todos
        .toggle(payload.id)
        .map_err(RouteError::bad_request)?;
    Ok(Json(state.todos.list(&scope(&payload.project))))
}

async fn remove(
    State(state): State<AppState>,
    Json(payload): Json<IdPayload>,
) -> RouteResult<Json<Vec<Todo>>> {
    state
        .todos
        .remove(payload.id)
        .map_err(RouteError::bad_request)?;
    Ok(Json(state.todos.list(&scope(&payload.project))))
}

async fn set_priority(
    State(state): State<AppState>,
    Json(payload): Json<PriorityPayload>,
) -> RouteResult<Json<Vec<Todo>>> {
    state
        .todos
        .set_priority(payload.id, payload.priority)
        .map_err(RouteError::bad_request)?;
    Ok(Json(state.todos.list(&scope(&payload.project))))
}

/// Edit a task's text in place — what the web app's inline editor calls.
async fn edit(
    State(state): State<AppState>,
    Json(payload): Json<EditPayload>,
) -> RouteResult<Json<Vec<Todo>>> {
    state
        .todos
        .set_text(payload.id, &payload.text)
        .map_err(RouteError::bad_request)?;
    Ok(Json(state.todos.list(&scope(&payload.project))))
}

/// Move a task between a project and the global list.
///
/// This is what "make global" and "move here" call. The reply refreshes
/// whichever list the caller was looking at, so a promoted task disappears from
/// the project view it left without a second request.
async fn set_scope(
    State(state): State<AppState>,
    Json(payload): Json<ScopePayload>,
) -> RouteResult<Json<Vec<Todo>>> {
    let target = project_id(&payload.target);

    // Refuse a move to a project the daemon doesn't know: a typo would
    // otherwise file the task somewhere nothing will ever show it again.
    if let Some(id) = target {
        state.project(id)?;
    }

    if !state.todos.set_project(payload.id, target)? {
        return Err(RouteError::not_found(format!("No todo #{}", payload.id)));
    }
    Ok(Json(state.todos.list(&scope(&payload.project))))
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
