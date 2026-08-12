//! Watch routes: run a command, stream its output, search it, bookmark it.
//!
//! This is where daemon ownership matters most. The old `ciabatta watch` held
//! the child process in the CLI process itself, so closing the terminal — or
//! Ctrl-C — killed the thing you were watching. Here the daemon spawns and owns
//! it, the CLI is just the first subscriber, and a session outlives the command
//! that started it.
//!
//! New output reaches the browser over SSE. The old page polled `/state.json`
//! on a timer; now [`crate::watch::WatchState`] notifies subscribers directly,
//! so an idle session costs nothing and a busy one has no added latency.

use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use axum::extract::{Path, Query, State};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures::stream::Stream;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::daemon::app::AppState;
use crate::watch::{Stream as LineStream, WatchState};

use super::{RouteError, RouteResult};

/// How many lines a snapshot returns by default.
const DEFAULT_LIMIT: usize = 2_000;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/watch/sessions", get(list).post(create))
        .route("/api/watch/sessions/{id}", get(detail).delete(close))
        .route("/api/watch/sessions/{id}/stream", get(stream))
        .route("/api/watch/sessions/{id}/stop", post(stop))
        .route("/api/watch/sessions/{id}/search", get(search))
        .route("/api/watch/sessions/{id}/export", get(export))
        .route("/api/watch/sessions/{id}/bookmarks", post(add_bookmark))
        .route(
            "/api/watch/sessions/{id}/bookmarks/delete",
            post(remove_bookmark),
        )
        .route("/api/watch/sessions/{id}/triggers", post(add_trigger))
        .route(
            "/api/watch/sessions/{id}/triggers/delete",
            post(remove_trigger),
        )
}

// ─── Sessions ───────────────────────────────────────────────────────────────

/// One watched command.
pub struct Session {
    pub id: u64,
    pub project: String,
    pub command: String,
    /// What this session is, when the command line doesn't say it — the node id
    /// of the `persistent` workflow step that left it behind.
    pub label: Option<String>,
    pub created_at: String,
    pub state: Arc<WatchState>,
}

impl Session {
    fn summary(&self) -> Value {
        json!({
            "id": self.id,
            "project": self.project,
            "command": self.command,
            "label": self.label,
            "created_at": self.created_at,
            "running": self.state.is_running(),
            "lines": self.state.seq().saturating_sub(1),
        })
    }
}

/// Every watch session the daemon owns, across all projects.
#[derive(Default)]
pub struct Sessions {
    inner: Mutex<HashMap<u64, Arc<Session>>>,
    next_id: AtomicU64,
}

impl Sessions {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(1),
        }
    }

    fn insert(&self, session: Session) -> Arc<Session> {
        let session = Arc::new(session);
        self.inner
            .lock()
            .unwrap()
            .insert(session.id, session.clone());
        session
    }

    fn get(&self, id: u64) -> Option<Arc<Session>> {
        self.inner.lock().unwrap().get(&id).cloned()
    }

    fn remove(&self, id: u64) -> Option<Arc<Session>> {
        self.inner.lock().unwrap().remove(&id)
    }

    fn list(&self) -> Vec<Arc<Session>> {
        let mut all: Vec<Arc<Session>> = self.inner.lock().unwrap().values().cloned().collect();
        all.sort_by_key(|s| std::cmp::Reverse(s.id));
        all
    }

    fn next_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    /// Stop every running session. Called on daemon shutdown so watched
    /// commands don't outlive the daemon that owns them.
    pub fn stop_all(&self) {
        for session in self.list() {
            let _ = session.state.stop();
        }
    }
}

// ─── Payloads ───────────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct CreatePayload {
    project: String,
    /// The shell command line. Runs through `sh -c` (or `cmd /C`), so pipes and
    /// `&&` work.
    command: String,
    #[serde(default)]
    triggers: Vec<String>,
    #[serde(default = "default_max_lines")]
    max_lines: usize,
    /// A human name for the session, when the command line alone doesn't say
    /// what it is. A `persistent` workflow step sets it to its node id, so the
    /// session it leaves behind is identifiable as "that step from that run".
    #[serde(default)]
    label: Option<String>,
    /// Where to run, relative to the project root. A persistent step runs from
    /// its own sub-workspace, exactly as it would have inside the graph.
    #[serde(default)]
    cwd: Option<String>,
    /// Environment variables layered over the daemon's own.
    #[serde(default)]
    env: HashMap<String, String>,
}

fn default_max_lines() -> usize {
    200_000
}

#[derive(Deserialize)]
pub struct AfterQuery {
    #[serde(default)]
    after: u64,
    #[serde(default = "default_limit")]
    limit: usize,
}

#[derive(Deserialize)]
pub struct ExportQuery {
    /// Prefix every line with when it arrived. Off by default: useful for a
    /// hang, noise in a stack trace.
    #[serde(default)]
    timestamps: bool,
}

fn default_limit() -> usize {
    DEFAULT_LIMIT
}

#[derive(Deserialize)]
pub struct SearchQuery {
    #[serde(default)]
    q: String,
    /// `any` (default) or `all`.
    #[serde(default)]
    mode: String,
    #[serde(default)]
    regex: bool,
    /// `stdout`, `stderr`, or absent for both.
    #[serde(default)]
    stream: Option<String>,
}

#[derive(Deserialize)]
pub struct AddBookmarkPayload {
    seq: u64,
    label: String,
    #[serde(default)]
    note: Option<String>,
}

#[derive(Deserialize)]
pub struct AddTriggerPayload {
    pattern: String,
    #[serde(default)]
    is_regex: bool,
}

#[derive(Deserialize, Serialize)]
pub struct IdPayload {
    id: u64,
}

// ─── Handlers ───────────────────────────────────────────────────────────────

async fn list(State(state): State<AppState>) -> Json<Vec<Value>> {
    Json(state.watch.list().iter().map(|s| s.summary()).collect())
}

/// Start a command and return the new session.
async fn create(
    State(state): State<AppState>,
    Json(payload): Json<CreatePayload>,
) -> RouteResult<Json<Value>> {
    let root = state.project_root(&payload.project)?;

    // A relative `cwd` is joined onto the project root, and must stay inside
    // it: a session is scoped to a project, and an escaping path would quietly
    // run somewhere the caller never named.
    let cwd = match payload.cwd.as_deref() {
        None => root.clone(),
        Some(rel) => {
            let joined = root.join(rel);
            let resolved = joined
                .canonicalize()
                .map_err(|_| RouteError::bad_request(format!("No such directory: {rel}")))?;
            let root_resolved = root.canonicalize().unwrap_or_else(|_| root.clone());
            if !resolved.starts_with(&root_resolved) {
                return Err(RouteError::bad_request(format!(
                    "cwd '{rel}' is outside the project"
                )));
            }
            resolved
        }
    };

    let watch_state = Arc::new(
        WatchState::new(&payload.command, payload.max_lines).map_err(RouteError::bad_request)?,
    );

    // Seed any triggers supplied with the request (deduped against ones
    // persisted from a previous run of the same command).
    for pattern in &payload.triggers {
        watch_state
            .add_trigger(pattern, false)
            .map_err(RouteError::bad_request)?;
    }

    watch_state
        .spawn(&payload.command, &cwd, &payload.env)
        .map_err(RouteError::bad_request)?;

    let session = state.watch.insert(Session {
        id: state.watch.next_id(),
        project: payload.project,
        command: payload.command,
        label: payload.label,
        created_at: chrono::Local::now().to_rfc3339(),
        state: watch_state,
    });

    Ok(Json(session.summary()))
}

/// A snapshot: session metadata plus lines after `after`.
async fn detail(
    State(state): State<AppState>,
    Path(id): Path<u64>,
    Query(query): Query<AfterQuery>,
) -> RouteResult<Json<Value>> {
    let session = session(&state, id)?;
    let mut value = session.state.state_json(query.after, query.limit);
    value["session"] = session.summary();
    Ok(Json(value))
}

/// The whole session as a plain-text transcript, ready to save or send on.
///
/// Served as a download rather than JSON the browser has to reassemble: the
/// point of the button is to end up with a file you can attach to a ticket, and
/// a 200,000-line log is not something to route through a JSON parser and a
/// string join in the page.
///
/// `?timestamps=true` prefixes each line with its arrival time.
async fn export(
    State(state): State<AppState>,
    Path(id): Path<u64>,
    Query(query): Query<ExportQuery>,
) -> RouteResult<axum::response::Response> {
    use axum::http::header;
    use axum::response::IntoResponse;

    let session = session(&state, id)?;
    let meta = crate::watch::TranscriptMeta {
        id: session.id,
        command: &session.command,
        label: session.label.as_deref(),
        created_at: &session.created_at,
    };
    let body = session.state.transcript(&meta, query.timestamps);

    Ok((
        [
            (
                header::CONTENT_TYPE,
                "text/plain; charset=utf-8".to_string(),
            ),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{}\"", meta.filename()),
            ),
        ],
        body,
    )
        .into_response())
}

/// Live output as Server-Sent Events.
///
/// Each event carries the lines added since the client's last sequence number,
/// so a reconnecting client can resume with `?after=` and miss nothing that is
/// still in the ring buffer.
async fn stream(
    State(state): State<AppState>,
    Path(id): Path<u64>,
    Query(query): Query<AfterQuery>,
) -> RouteResult<Sse<impl Stream<Item = Result<Event, Infallible>>>> {
    let session = session(&state, id)?;
    let mut after = query.after;

    let events = async_stream::stream! {
        loop {
            // Capture "is it still running" *before* reading the lines, so a
            // process that exits between the two reads doesn't cause the final
            // lines to be dropped: worst case we send one extra empty frame.
            let running = session.state.is_running();
            let mut payload = session.state.state_json(after, DEFAULT_LIMIT);

            // Advance past the highest line actually sent — not `next_seq`,
            // which would skip everything beyond the batch limit when a burst
            // of output arrives at once.
            let sent = payload["lines"]
                .as_array()
                .and_then(|lines| lines.last())
                .and_then(|line| line["seq"].as_u64());
            let more_pending = match sent {
                Some(seq) => {
                    after = seq;
                    // A full batch means the buffer still holds more.
                    payload["lines"].as_array().is_some_and(|l| l.len() >= DEFAULT_LIMIT)
                }
                None => false,
            };

            payload["session"] = session.summary();
            if let Ok(event) = Event::default().json_data(&payload) {
                yield Ok(event);
            }

            // Loop straight back round while a burst is still draining, rather
            // than waiting for the next line to arrive.
            if more_pending {
                continue;
            }

            // The process is gone and the client has everything: close the
            // stream rather than leaving it open forever.
            if !running {
                break;
            }

            session.state.changed().await;
        }
    };

    // The keep-alive comment stops idle proxies (and some browsers) from
    // deciding a quiet session has died.
    Ok(Sse::new(events).keep_alive(KeepAlive::default()))
}

/// Stop the watched process, leaving the session and its buffer in place.
async fn stop(State(state): State<AppState>, Path(id): Path<u64>) -> RouteResult<Json<Value>> {
    let session = session(&state, id)?;
    session.state.stop().map_err(RouteError::bad_request)?;
    Ok(Json(json!({ "ok": true })))
}

/// Stop the process (if running) and forget the session entirely.
async fn close(State(state): State<AppState>, Path(id): Path<u64>) -> RouteResult<Json<Value>> {
    let Some(session) = state.watch.remove(id) else {
        return Err(RouteError::not_found(format!("No watch session {id}")));
    };
    // Best effort: an already-exited process has no pid to signal.
    let _ = session.state.stop();
    Ok(Json(json!({ "ok": true })))
}

async fn search(
    State(state): State<AppState>,
    Path(id): Path<u64>,
    Query(query): Query<SearchQuery>,
) -> RouteResult<Json<Value>> {
    let session = session(&state, id)?;

    let terms: Vec<String> = query
        .q
        .split([',', ' '])
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(str::to_string)
        .collect();

    if terms.is_empty() {
        return Ok(Json(json!({ "lines": [], "total": 0, "capped": false })));
    }

    let stream_filter = match query.stream.as_deref() {
        Some("stdout") => Some(LineStream::Stdout),
        Some("stderr") => Some(LineStream::Stderr),
        _ => None,
    };

    let (lines, total) = session.state.search_lines(
        &terms,
        query.mode == "all",
        query.regex,
        stream_filter,
        DEFAULT_LIMIT,
    );

    Ok(Json(json!({
        "lines": lines,
        "total": total,
        "capped": total > lines.len(),
    })))
}

async fn add_bookmark(
    State(state): State<AppState>,
    Path(id): Path<u64>,
    Json(payload): Json<AddBookmarkPayload>,
) -> RouteResult<Json<Value>> {
    let session = session(&state, id)?;
    let bookmark_id = session
        .state
        .add_bookmark(payload.seq, &payload.label, payload.note);
    Ok(Json(json!({ "id": bookmark_id })))
}

async fn remove_bookmark(
    State(state): State<AppState>,
    Path(id): Path<u64>,
    Json(payload): Json<IdPayload>,
) -> RouteResult<Json<Value>> {
    session(&state, id)?.state.remove_bookmark(payload.id);
    Ok(Json(json!({ "ok": true })))
}

async fn add_trigger(
    State(state): State<AppState>,
    Path(id): Path<u64>,
    Json(payload): Json<AddTriggerPayload>,
) -> RouteResult<Json<Value>> {
    let session = session(&state, id)?;
    let trigger_id = session
        .state
        .add_trigger(&payload.pattern, payload.is_regex)
        .map_err(RouteError::bad_request)?;
    Ok(Json(json!({ "id": trigger_id })))
}

async fn remove_trigger(
    State(state): State<AppState>,
    Path(id): Path<u64>,
    Json(payload): Json<IdPayload>,
) -> RouteResult<Json<Value>> {
    session(&state, id)?.state.remove_trigger(payload.id);
    Ok(Json(json!({ "ok": true })))
}

fn session(state: &AppState, id: u64) -> RouteResult<Arc<Session>> {
    state
        .watch
        .get(id)
        .ok_or_else(|| RouteError::not_found(format!("No watch session {id}")))
}

#[cfg(test)]
mod tests {
    use crate::daemon::app::router;
    use crate::daemon::app::tests::test_state;

    use axum::body::Body;
    use axum::http::{Request, StatusCode, header};
    use tower::ServiceExt;

    #[tokio::test]
    async fn creating_a_session_requires_the_token() {
        let app = router(test_state());
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/watch/sessions")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"project":"x","command":"id"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "an unauthenticated caller must not be able to run a command"
        );
    }

    #[tokio::test]
    async fn creating_a_session_rejects_an_unknown_project() {
        let app = router(test_state());
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/watch/sessions")
                    .header(header::AUTHORIZATION, "Bearer test-token")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"project":"nope","command":"id"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn unknown_sessions_are_404() {
        let app = router(test_state());
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/watch/sessions/9999")
                    .header(header::AUTHORIZATION, "Bearer test-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn listing_sessions_starts_empty() {
        let app = router(test_state());
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/watch/sessions")
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
        assert_eq!(&body[..], b"[]");
    }
}
