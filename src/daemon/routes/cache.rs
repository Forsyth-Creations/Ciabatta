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
use axum::routing::{get, post};
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
        .route("/api/cache/files", get(files))
        .route("/api/cache/tree", get(tree))
        .route("/api/cache/target-files", post(save_target_files))
}

#[derive(Deserialize)]
pub struct TreeQuery {
    project: String,
}

/// The most files, and the most directories, the picker is sent.
const TREE_LIMIT: usize = 50_000;

/// Every file and directory in the checkout, relative to the workspace root —
/// what the run page's file picker browses to choose a step's inputs and
/// outputs. Paths git ignores are listed too, since `target/` is where an
/// output is picked from, and `ignored` names the top of each ignored subtree
/// so the picker can mark them.
///
/// Past [`TREE_LIMIT`], what git keeps is listed first: a checkout with a
/// huge `node_modules` still shows all of its sources.
async fn tree(
    State(state): State<AppState>,
    Query(query): Query<TreeQuery>,
) -> RouteResult<Json<serde_json::Value>> {
    let project = state.project_root(&query.project)?;
    let root = crate::workspace::Workspace::discover(&project)
        .map(|ws| ws.root)
        .unwrap_or(project);
    let (all, kept) = tokio::task::spawn_blocking({
        let root = root.clone();
        move || -> anyhow::Result<_> {
            Ok((
                crate::cache::every_path(&root)?,
                crate::cache::not_ignored(&root)?,
            ))
        }
    })
    .await
    .map_err(|e| anyhow::anyhow!("listing the checkout failed: {e}"))??;
    Ok(Json(listing(&root, all, &kept)))
}

/// [`tree`]'s answer, from every path and the ones git keeps.
fn listing(
    root: &std::path::Path,
    all: Vec<(String, bool)>,
    kept: &std::collections::HashSet<String>,
) -> serde_json::Value {
    let mut files: Vec<String> = Vec::new();
    let mut dirs: Vec<String> = Vec::new();
    let mut ignored_files: Vec<String> = Vec::new();
    let mut ignored_dirs: Vec<String> = Vec::new();
    let mut ignored: Vec<String> = Vec::new();
    for (path, is_dir) in all {
        if kept.contains(&path) {
            if is_dir {
                dirs.push(path)
            } else {
                files.push(path)
            }
            continue;
        }
        // The top of an ignored subtree: its parent is kept (or is the root).
        let parent = path.rsplit_once('/').map(|(p, _)| p);
        if parent.is_none_or(|p| kept.contains(p)) {
            ignored.push(path.clone());
        }
        if is_dir {
            ignored_dirs.push(path)
        } else {
            ignored_files.push(path)
        }
    }
    let total = files.len() + ignored_files.len();
    for list in [
        &mut files,
        &mut dirs,
        &mut ignored_files,
        &mut ignored_dirs,
        &mut ignored,
    ] {
        list.sort();
    }
    files.extend(ignored_files);
    dirs.extend(ignored_dirs);
    files.truncate(TREE_LIMIT);
    dirs.truncate(TREE_LIMIT);
    json!({
        "root": root.to_string_lossy().replace('\\', "/"),
        "files": files,
        "dirs": dirs,
        "ignored": ignored,
        "truncated": total > TREE_LIMIT,
    })
}

#[derive(Deserialize)]
pub struct TargetFilesPayload {
    project: String,
    /// The step's cache id, `<member>:<workflow>:<step>`.
    target: String,
    #[serde(default)]
    inputs: Vec<String>,
    /// Earlier steps whose outputs it reads, by cache id.
    #[serde(default)]
    inputs_from: Vec<String>,
    #[serde(default)]
    outputs: Vec<String>,
}

/// Set the inputs and outputs picked for a step in its workflow file — the
/// lists are replaced, not added to. See
/// [`crate::cache::cli::save_target_files`].
async fn save_target_files(
    State(state): State<AppState>,
    Json(payload): Json<TargetFilesPayload>,
) -> RouteResult<Json<serde_json::Value>> {
    // Every path stays inside the checkout: these are written into a config
    // that a later run resolves against the root.
    let escapes = |p: &String| {
        p.is_empty()
            || std::path::Path::new(p).is_absolute()
            || p.split(['/', '\\']).any(|part| part == "..")
    };
    if payload.inputs.iter().chain(&payload.outputs).any(escapes)
        || payload.inputs_from.iter().any(String::is_empty)
    {
        return Err(RouteError::bad_request(
            "Paths are relative to the workspace root and stay inside it.",
        ));
    }
    let project = state.project_root(&payload.project)?;
    let workspace =
        crate::workspace::Workspace::discover(&project).map_err(RouteError::bad_request)?;
    let written = crate::cache::cli::save_target_files(
        &workspace,
        &payload.target,
        &payload.inputs,
        &payload.inputs_from,
        &payload.outputs,
    )
    .map_err(RouteError::bad_request)?;
    Ok(Json(json!({ "path": written })))
}

#[derive(Deserialize)]
pub struct FilesQuery {
    project: String,
    /// The cache key a run reported for the step.
    key: String,
}

/// Every file the cache saw for one key, each with its hash: the inputs it was
/// keyed on and the outputs it stored (or, for a step still building, the
/// outputs as they stand on disk right now).
///
/// Served on demand rather than carried in the run's view model, which is
/// streamed whole on every change — a monorepo step can read thousands of
/// files, and nobody needs them until they click on it.
async fn files(
    State(state): State<AppState>,
    Query(query): Query<FilesQuery>,
) -> RouteResult<Json<serde_json::Value>> {
    // A key is hex; anything else is somebody trying a path.
    if query.key.is_empty() || !query.key.chars().all(|c| c.is_ascii_alphanumeric()) {
        return Err(RouteError::bad_request("not a cache key"));
    }
    let root = state.project_root(&query.project)?;
    let store = crate::cache::graph::store_for(&root)?;
    let entry = store.get(&query.key)?;
    let inspection = store.inspection(&query.key);

    let (inputs, outputs, stored, target, workspace, env, upstream, at) = match (entry, inspection)
    {
        (Some(entry), _) => (
            entry.inputs,
            entry.outputs,
            true,
            entry.target,
            entry.workspace,
            entry.env.into_keys().collect::<Vec<_>>(),
            entry.upstream,
            entry.created_at,
        ),
        (None, Some(inspection)) => {
            let outputs =
                crate::cache::hash_matching(&inspection.dir, &inspection.output_patterns, &[])
                    .unwrap_or_default();
            (
                inspection.inputs,
                outputs,
                false,
                inspection.target,
                inspection.workspace,
                inspection.env,
                inspection.upstream,
                inspection.decided_at,
            )
        }
        (None, None) => {
            return Err(RouteError::not_found(format!(
                "nothing is recorded under cache key {}",
                query.key
            )));
        }
    };

    Ok(Json(json!({
        "key": query.key,
        "target": target,
        "workspace": workspace,
        "stored": stored,
        "at": at,
        "algorithm": "sha256",
        "inputs": inputs,
        "outputs": outputs,
        "env": env,
        "upstream": upstream,
    })))
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
    // Nothing named means everything: every workflow the project declares,
    // compiled into one graph — what the page's "every runnable workflow"
    // offers, and what used to come back as "'(nothing named)' is not a
    // workflow here".
    if targets.is_empty()
        && let Some(ws) = workspace.as_ref()
    {
        let all = ws.workflow_names();
        if all.is_empty() {
            return Ok((Vec::new(), None));
        }
        let selection = crate::workspace::graph::Selection::default();
        let (_, graph) = crate::workspace::graph::prepare_many(root, &all, &selection)?;
        return Ok((graph.steps, None));
    }

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
            "/api/cache/files?project=x&key=abc",
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

#[cfg(test)]
mod tree_tests {
    use std::path::Path;

    /// `target/` is what the 0.7.1 picker couldn't offer: git ignores it, and
    /// it is where a build's outputs are picked from.
    #[test]
    fn the_tree_lists_what_git_ignores_and_marks_it() {
        let dir = std::env::temp_dir().join(format!("ciabatta-tree-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for (rel, body) in [
            (".gitignore", "target/\n*.log\n"),
            ("src/main.rs", "fn main() {}"),
            ("target/release/app", "bin"),
            ("src/debug.log", "log"),
        ] {
            let path = dir.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, body).unwrap();
        }
        let all = crate::cache::every_path(&dir).unwrap();
        let kept = crate::cache::not_ignored(&dir).unwrap();
        let listed = super::listing(Path::new(&dir), all, &kept);
        let strings =
            |key: &str| -> Vec<String> { serde_json::from_value(listed[key].clone()).unwrap() };

        let files = strings("files");
        assert!(
            files.contains(&"target/release/app".to_string()),
            "{files:?}"
        );
        assert!(files.contains(&"src/debug.log".to_string()), "{files:?}");
        // What git keeps comes first.
        assert!(
            files.iter().position(|f| f == "src/main.rs")
                < files.iter().position(|f| f == "target/release/app")
        );
        assert!(strings("dirs").contains(&"target/release".to_string()));
        assert_eq!(strings("ignored"), ["src/debug.log", "target"]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
