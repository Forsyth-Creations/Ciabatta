//! Cache routes: what a build would reuse, and why it wouldn't.
//!
//! These back two things in the web app. The **plan** endpoint answers the same
//! question `ciabatta dry-run` does — for every stage, its input files, its
//! output files, and when it would rebuild, the diff that explains it. The
//! **remote** endpoints proxy a configured remote cache's status, so the
//! browser can show it without needing the cache's own credentials.
//!
//! The proxy matters more than it looks. The daemon already holds this
//! machine's session for the remote cache; a page fetching the cache directly
//! would have to be handed that token, and a token in a page is a token in
//! every viewer's browser history.

use axum::extract::{Query, State};
use axum::routing::get;
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::json;

use crate::daemon::app::AppState;

use super::{RouteError, RouteResult};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/cache/plan", get(plan))
        .route("/api/cache/status", get(status))
        .route("/api/cache/remote", get(remote_status))
}

#[derive(Deserialize)]
pub struct PlanQuery {
    /// The checkout to plan in.
    project: String,
    /// The workflow or workflow to plan. Omitted means every runnable workflow.
    #[serde(default)]
    target: Option<String>,
}

/// What a run would reuse and what it would rebuild.
///
/// Deliberately the same code path `ciabatta dry-run` uses, so the page and the
/// terminal cannot disagree about what is about to happen.
async fn plan(
    State(state): State<AppState>,
    Query(query): Query<PlanQuery>,
) -> RouteResult<Json<serde_json::Value>> {
    let root = state.project_root(&query.project)?;
    let config = crate::config::load_config(&root)?;
    let store = crate::cache::graph::store_for(&root)?;

    let targets: Vec<String> = query.target.clone().into_iter().collect();
    let workspace = crate::workspace::Workspace::discover(&root).ok();

    let (steps, _) = resolve_targets(&root, &config, &targets, &workspace)
        .map_err(|e| RouteError::bad_request(format!("{e:#}")))?;

    // The environment a run would see, so the plan keys the same way one would:
    // the daemon's own environment (which is what a run started from the
    // browser gets), plus this workspace's env files.
    let mut vars: std::collections::HashMap<String, String> = std::env::vars().collect();
    let meta = config.workspace.clone().unwrap_or_default();
    let files = crate::environment::files::resolve(&meta, &root);
    if !files.files.is_empty()
        && let Ok(merged) = crate::run::load_env_files(&files.files, &root, &vars)
    {
        vars = merged;
    }

    let context = crate::cache::cli::WorkspaceContext {
        workspace: workspace.as_ref(),
        root: root.clone(),
        config: &config,
    };
    let plan = crate::cache::graph::plan_graph(
        &steps,
        &context,
        &crate::cache::cli::env_map(&vars),
        &store,
    )?;

    let mut body = crate::cache::cli::plan_json(&plan);
    body["saved_ms"] = json!(plan.saved_ms(&store));
    body["caching"] = json!(plan.has_caching());
    Ok(Json(body))
}

/// Resolve the steps to plan — the same rules `ciabatta dry-run` follows.
fn resolve_targets(
    root: &std::path::Path,
    _config: &crate::config::CiabattaConfig,
    targets: &[String],
    workspace: &Option<crate::workspace::Workspace>,
) -> anyhow::Result<(Vec<crate::run::RunStep>, Option<crate::cache::CacheConfig>)> {
    if let (Some(ws), Some(first)) = (workspace.as_ref(), targets.first())
        && ws.workflow_names().iter().any(|name| name == first)
    {
        let selection = crate::workspace::graph::Selection::default();
        let (_, graph) = crate::workspace::graph::prepare_many(root, targets, &selection)?;
        return Ok((graph.steps, None));
    }

    Err(anyhow::anyhow!(
        "'{}' is not a workflow here. Run `ciabatta list` to see what is.",
        targets
            .first()
            .map(String::as_str)
            .unwrap_or("(nothing named)")
    ))
}

#[derive(Deserialize)]
pub struct ProjectQuery {
    project: String,
}

/// What this project's local cache is holding.
async fn status(
    State(state): State<AppState>,
    Query(query): Query<ProjectQuery>,
) -> RouteResult<Json<serde_json::Value>> {
    let root = state.project_root(&query.project)?;
    let store = crate::cache::graph::store_for(&root)?;
    let stats = store.stats()?;
    let config = crate::config::load_config(&root)?;

    let cache = config.cache.unwrap_or_default();
    Ok(Json(json!({
        "enabled": cache.is_on(),
        "why_disabled": cache.why_disabled(),
        "inputs": cache.inputs,
        "outputs": cache.outputs,
        "exclude": cache.exclude,
        "env": cache.env,
        "remote": cache.remote,
        "path": store.root(),
        "entries": stats.entries,
        "bytes": stats.size,
        "human": crate::cache::store::human_size(stats.size),
        "build_time_ms": stats.build_time_ms,
        "by_workspace": stats.by_workspace,
        "oldest": stats.oldest,
        "newest": stats.newest,
    })))
}

/// The configured remote cache's status, fetched with the daemon's own session.
async fn remote_status(
    State(state): State<AppState>,
    Query(query): Query<ProjectQuery>,
) -> RouteResult<Json<serde_json::Value>> {
    let root = state.project_root(&query.project)?;
    let config = crate::config::load_config(&root)?;

    let Some(remote) = config.cache.as_ref().and_then(|c| c.remote()) else {
        // Not an error: most projects have no remote cache, and the page needs
        // to say so rather than show a failure.
        return Ok(Json(json!({ "configured": false })));
    };

    let client = crate::remote_cache::client::Client::new(&remote.url, remote.tls_verify)
        .map_err(|e| RouteError::bad_request(format!("{e:#}")))?;

    match client.stats().await {
        Ok(stats) => Ok(Json(json!({
            "configured": true,
            "url": remote.url,
            "project": remote.project,
            "read_only": remote.read_only,
            "reachable": true,
            "stats": stats,
        }))),
        // A cache that's down is a fact about the cache, not a failure of this
        // request — the page should render it as "unreachable", not as an error
        // banner over everything else.
        Err(e) => Ok(Json(json!({
            "configured": true,
            "url": remote.url,
            "project": remote.project,
            "read_only": remote.read_only,
            "reachable": false,
            "error": format!("{e:#}"),
        }))),
    }
}

#[cfg(test)]
mod tests {
    use crate::daemon::app::router;
    use crate::daemon::app::tests::test_state;

    use axum::body::Body;
    use axum::http::{Request, StatusCode, header};
    use tower::ServiceExt;

    #[tokio::test]
    async fn the_cache_routes_require_the_token() {
        for uri in [
            "/api/cache/plan?project=x",
            "/api/cache/status?project=x",
            "/api/cache/remote?project=x",
        ] {
            let response = router(test_state())
                .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(
                response.status(),
                StatusCode::UNAUTHORIZED,
                "{uri} should need the token"
            );
        }
    }

    #[tokio::test]
    async fn an_unknown_project_is_a_client_error() {
        let response = router(test_state())
            .oneshot(
                Request::builder()
                    .uri("/api/cache/status?project=definitely-not-real")
                    .header(header::AUTHORIZATION, "Bearer test-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
}
