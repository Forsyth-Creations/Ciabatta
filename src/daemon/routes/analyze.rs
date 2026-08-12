//! Analyze routes: the codebase dependency graph.
//!
//! A scan walks the whole tree and can take a while, so it never happens
//! inline on a page load. `POST /api/analyze/scans` kicks one off and returns
//! immediately; `GET /api/analyze/graph` serves whatever the project last
//! produced. That last result is read straight from the file the CLI already
//! writes, so `ciabatta analyze` on the command line and the web view always
//! agree.

use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::{Query, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::daemon::app::AppState;

use super::{RouteError, RouteResult};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/analyze/graph", get(graph))
        .route("/api/analyze/scans", post(scan))
        .route("/api/analyze/status", get(status))
}

/// Where a project's analysis lands. Matches what `ciabatta analyze` writes, so
/// either entry point produces a file the other can read.
fn graph_path(root: &std::path::Path) -> PathBuf {
    root.join("ciabatta-analyze.json")
}

#[derive(Deserialize)]
pub struct ProjectQuery {
    project: String,
}

#[derive(Deserialize)]
pub struct ScanPayload {
    project: String,
    /// Query the OSV database for known vulnerabilities. Needs network, and is
    /// much slower, so it's opt-in exactly as the `--check-vulns` flag is.
    #[serde(default)]
    check_vulns: bool,
}

/// The last analysis for a project, if there is one.
async fn graph(
    State(state): State<AppState>,
    Query(query): Query<ProjectQuery>,
) -> RouteResult<Json<Value>> {
    let root = state.project_root(&query.project)?;
    let path = graph_path(&root);

    let Ok(raw) = tokio::fs::read_to_string(&path).await else {
        // Not an error: a project simply may not have been scanned yet, and the
        // page turns this into a "run a scan" prompt.
        return Ok(Json(json!({ "scanned": false })));
    };

    let mut graph: Value = serde_json::from_str(&raw).map_err(|e| {
        RouteError::bad_request(format!("{} is not valid JSON: {e}", path.display()))
    })?;
    graph["scanned"] = json!(true);
    Ok(Json(graph))
}

/// Whether a scan is currently running for this project.
async fn status(
    State(state): State<AppState>,
    Query(query): Query<ProjectQuery>,
) -> RouteResult<Json<Value>> {
    let root = state.project_root(&query.project)?;
    Ok(Json(json!({ "running": state.analyze.is_running(&root) })))
}

/// Start a scan. Returns as soon as it's queued; poll `status` for completion.
async fn scan(
    State(state): State<AppState>,
    Json(payload): Json<ScanPayload>,
) -> RouteResult<Json<Value>> {
    let root = state.project_root(&payload.project)?;

    if !state.analyze.begin(&root) {
        return Ok(Json(json!({
            "ok": false,
            "error": "A scan is already running for this project.",
        })));
    }

    let running = state.analyze.clone();
    tokio::spawn(async move {
        let result = run_scan(root.clone(), payload.check_vulns).await;
        if let Err(e) = result {
            tracing::warn!("analyze scan of {} failed: {e:#}", root.display());
        }
        running.finish(&root);
    });

    Ok(Json(json!({ "ok": true })))
}

/// Do the actual work: build the graph, optionally check vulnerabilities, and
/// write it where both the CLI and `graph` above expect to find it.
async fn run_scan(root: PathBuf, check_vulns: bool) -> anyhow::Result<()> {
    let config = crate::config::load_config(&root)?;

    // The scan is CPU- and IO-bound and walks the whole tree, so it must not run
    // on the async runtime — a big repo would stall every other request.
    let scan_root = root.clone();
    let scan_config = config.clone();
    let mut graph = tokio::task::spawn_blocking(move || {
        let requirements = scan_config
            .analyze
            .as_ref()
            .and_then(|a| a.requirements.as_ref())
            .map(|p| scan_root.join(p));
        let trace = scan_config
            .analyze
            .as_ref()
            .and_then(|a| a.trace.as_ref())
            .map(|p| scan_root.join(p));

        let inputs = crate::analyze::RequirementInputs {
            requirements_file: requirements.as_deref(),
            trace_file: trace.as_deref(),
        };
        // The quiet variant: this runs inside the daemon, where the noisy one's
        // stderr summary would just clutter the log.
        crate::analyze::analyze_quiet(&scan_root, &scan_config, &inputs)
    })
    .await??;

    if check_vulns {
        crate::analyze::check_vulnerabilities(&mut graph).await?;
    }

    let json = serde_json::to_string_pretty(&graph)?;
    tokio::fs::write(graph_path(&root), json).await?;
    Ok(())
}

/// Tracks which projects have a scan in flight, so two clicks don't launch two
/// full tree walks over the same repo.
#[derive(Default)]
pub struct Running {
    inner: std::sync::Mutex<std::collections::HashSet<PathBuf>>,
}

impl Running {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Claim the slot for `root`. Returns false if a scan is already running.
    fn begin(&self, root: &std::path::Path) -> bool {
        self.inner.lock().unwrap().insert(root.to_path_buf())
    }

    fn finish(&self, root: &std::path::Path) {
        self.inner.lock().unwrap().remove(root);
    }

    fn is_running(&self, root: &std::path::Path) -> bool {
        self.inner.lock().unwrap().contains(root)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::app::router;
    use crate::daemon::app::tests::test_state;

    use axum::body::Body;
    use axum::http::{Request, StatusCode, header};
    use tower::ServiceExt;

    #[test]
    fn a_second_scan_of_the_same_project_is_refused() {
        let running = Running::new();
        let root = std::path::Path::new("/tmp/example");

        assert!(running.begin(root), "the first scan claims the slot");
        assert!(!running.begin(root), "a concurrent scan must be refused");
        assert!(running.is_running(root));

        running.finish(root);
        assert!(!running.is_running(root));
        assert!(running.begin(root), "the slot is reusable once finished");
    }

    #[test]
    fn scans_of_different_projects_are_independent() {
        let running = Running::new();
        assert!(running.begin(std::path::Path::new("/tmp/a")));
        assert!(running.begin(std::path::Path::new("/tmp/b")));
    }

    #[tokio::test]
    async fn the_graph_route_rejects_an_unknown_project() {
        let app = router(test_state());
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/analyze/graph?project=nope")
                    .header(header::AUTHORIZATION, "Bearer test-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn starting_a_scan_requires_the_token() {
        let app = router(test_state());
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/analyze/scans")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"project":"x"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }
}
