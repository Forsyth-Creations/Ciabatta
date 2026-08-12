//! AI assistant routes: the architecture mind map and background jobs.
//!
//! The `Assistant` and its `Jobs` store are per-project and expensive to build
//! (they read config and load the brain), so they're created on first use and
//! then cached for the daemon's lifetime — see
//! [`crate::daemon::app::AppState::assistant`].
//!
//! `/api/ai/graph` keeps the old sequence-number protocol: pass `?after=N` and
//! get `{changed:false}` back while the brain hasn't moved. It's a cheap poll,
//! and the mind map's shape changes rarely enough that a push stream would earn
//! nothing. Long-running work (`ask`, `ship`) reports through the jobs list.

use axum::extract::{Query, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::mpsc;

use crate::daemon::app::AppState;

use super::{RouteError, RouteResult};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/ai/graph", get(graph))
        .route("/api/ai/jobs", get(jobs))
        .route("/api/ai/ask", post(ask))
        .route("/api/ai/ship", post(ship))
        .route("/api/ai/confirm", post(confirm))
        .route("/api/ai/confirm-all", post(confirm_all))
        .route("/api/ai/prune", post(prune))
        .route("/api/ai/feedback", post(feedback))
}

#[derive(Deserialize)]
pub struct GraphQuery {
    project: String,
    /// The brain sequence the client already has. Unchanged means no payload.
    #[serde(default)]
    after: u64,
}

#[derive(Deserialize)]
pub struct ProjectQuery {
    project: String,
}

#[derive(Deserialize)]
pub struct AskPayload {
    project: String,
    prompt: String,
}

#[derive(Deserialize)]
pub struct ShipPayload {
    project: String,
    prompt: String,
    #[serde(default)]
    source: String,
}

#[derive(Deserialize)]
pub struct ConfirmPayload {
    project: String,
    file: String,
    accept: bool,
}

#[derive(Deserialize)]
pub struct ConfirmAllPayload {
    project: String,
    accept: bool,
}

#[derive(Deserialize)]
pub struct PrunePayload {
    project: String,
    /// `file`, `architecture`, or `tag`.
    kind: String,
    /// The file path or architecture name.
    id: String,
    #[serde(default)]
    tag: String,
}

#[derive(Deserialize)]
pub struct FeedbackPayload {
    project: String,
    positive: bool,
    #[serde(default)]
    note: String,
}

/// The mind map, or `{changed:false}` when the client is already current.
async fn graph(
    State(state): State<AppState>,
    Query(query): Query<GraphQuery>,
) -> RouteResult<Json<Value>> {
    let assistant = state.assistant(&query.project)?;

    // `activity` rides along even on unchanged responses so the page's status
    // line can follow a burn-in between map mutations.
    let activity = json!(assistant.activity());

    if assistant.brain.seq() == query.after {
        return Ok(Json(
            json!({ "seq": query.after, "changed": false, "activity": activity }),
        ));
    }

    let mut graph = assistant.brain.graph_json();
    graph["changed"] = json!(true);
    graph["activity"] = activity;
    Ok(Json(graph))
}

/// Background job status.
async fn jobs(
    State(state): State<AppState>,
    Query(query): Query<ProjectQuery>,
) -> RouteResult<Json<Value>> {
    let project = state.project(&query.project)?;
    Ok(Json(state.ai_jobs(&project)?.snapshot_json()))
}

/// Ask a question and wait for the whole answer.
///
/// Statuses are collected rather than streamed: this is the programmatic
/// surface, and one JSON reply is enough. Anything long-running belongs in
/// `ship`, which returns immediately and reports through the jobs list.
async fn ask(
    State(state): State<AppState>,
    Json(payload): Json<AskPayload>,
) -> RouteResult<Json<Value>> {
    let assistant = state.assistant(&payload.project)?;
    let gate = state.ask_gate(&payload.project)?;

    // One question at a time per project, so concurrent callers can't
    // interleave a single conversation history.
    let _running = gate.lock().await;

    let (tx, mut rx) = mpsc::channel(64);
    let collector = tokio::spawn(async move {
        let mut steps = Vec::new();
        let mut suggestions = Vec::new();
        while let Some(event) = rx.recv().await {
            match event {
                crate::ai::AiEvent::Status(s) => steps.push(s),
                crate::ai::AiEvent::Suggestion(c) => suggestions.push(json!({
                    "file": c.file,
                    "diff": c.diff,
                    "reason": c.reason,
                    "state": c.state.label(),
                })),
                _ => {}
            }
        }
        (steps, suggestions)
    });

    let answer = assistant
        .ask(&payload.prompt, tx)
        .await
        .map_err(RouteError::bad_request)?;
    let (steps, suggestions) = collector.await.unwrap_or_default();

    Ok(Json(json!({
        "answer": answer,
        "steps": steps,
        "suggestions": suggestions,
        "confidence": assistant.brain.confidence(),
    })))
}

/// Queue a task for the assistant to complete autonomously.
async fn ship(
    State(state): State<AppState>,
    Json(payload): Json<ShipPayload>,
) -> RouteResult<Json<Value>> {
    let project = state.project(&payload.project)?;
    let jobs = state.ai_jobs(&project)?;

    let source = if payload.source.trim().is_empty() {
        "gui"
    } else {
        payload.source.trim()
    };

    let id = jobs
        .ship(&payload.prompt, source)
        .map_err(RouteError::bad_request)?;
    Ok(Json(json!({ "ok": true, "id": id })))
}

/// Accept or reject one pending tag proposal.
async fn confirm(
    State(state): State<AppState>,
    Json(payload): Json<ConfirmPayload>,
) -> RouteResult<Json<Value>> {
    let assistant = state.assistant(&payload.project)?;
    match assistant.brain.confirm(&payload.file, payload.accept) {
        Ok(true) => Ok(Json(json!({ "ok": true }))),
        Ok(false) => Err(RouteError::not_found(
            "No pending confirmation for that file.",
        )),
        Err(e) => Err(RouteError::bad_request(e)),
    }
}

async fn confirm_all(
    State(state): State<AppState>,
    Json(payload): Json<ConfirmAllPayload>,
) -> RouteResult<Json<Value>> {
    let assistant = state.assistant(&payload.project)?;
    let resolved = assistant
        .brain
        .confirm_all(payload.accept)
        .map_err(RouteError::bad_request)?;
    Ok(Json(json!({ "ok": true, "resolved": resolved })))
}

/// Remove knowledge from the map.
async fn prune(
    State(state): State<AppState>,
    Json(payload): Json<PrunePayload>,
) -> RouteResult<Json<Value>> {
    let assistant = state.assistant(&payload.project)?;

    let result = match payload.kind.as_str() {
        "file" => assistant.brain.forget_file(&payload.id),
        "architecture" => assistant.brain.forget_architecture(&payload.id),
        "tag" => assistant.brain.untag_file(&payload.id, &payload.tag),
        other => {
            return Err(RouteError::bad_request(format!(
                "Unknown prune kind '{other}' (expected file, architecture, or tag)"
            )));
        }
    };

    match result {
        Ok(true) => Ok(Json(json!({ "ok": true }))),
        Ok(false) => Err(RouteError::not_found("Nothing in the map matches that.")),
        Err(e) => Err(RouteError::bad_request(e)),
    }
}

/// Train the per-project confidence score.
async fn feedback(
    State(state): State<AppState>,
    Json(payload): Json<FeedbackPayload>,
) -> RouteResult<Json<Value>> {
    let assistant = state.assistant(&payload.project)?;
    let files = assistant.files_touched();
    let confidence = assistant
        .brain
        .record_feedback(payload.positive, files, &payload.note)
        .map_err(RouteError::bad_request)?;
    Ok(Json(json!({ "ok": true, "confidence": confidence })))
}

#[cfg(test)]
mod tests {
    use crate::daemon::app::router;
    use crate::daemon::app::tests::test_state;

    use axum::body::Body;
    use axum::http::{Request, StatusCode, header};
    use tower::ServiceExt;

    #[tokio::test]
    async fn the_graph_route_rejects_an_unknown_project() {
        let app = router(test_state());
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/ai/graph?project=nope")
                    .header(header::AUTHORIZATION, "Bearer test-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn shipping_a_task_requires_the_token() {
        let app = router(test_state());
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/ai/ship")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"project":"x","prompt":"do a thing"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "an unauthenticated caller must not be able to start an agent run"
        );
    }
}
