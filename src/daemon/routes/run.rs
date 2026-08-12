//! Run routes: execute a recipe DAG live, with fix-it branches.
//!
//! The daemon owns the run, so it survives the terminal that started it —
//! the same change `watch` got, and it matters more here: a half-finished
//! run killed by a closed laptop lid is a bad afternoon.
//!
//! Two channels move in opposite directions, both already provided by the
//! engine:
//!
//! * `ProgressUpdate` flows out of [`crate::runner::run_all_ctl`] into
//!   [`crate::run::view::GuiState`], which the SSE stream serves.
//! * `StepChoice` flows back in over the [`crate::runner::RunCtl`] broadcast
//!   when someone answers a recovery prompt in the browser.

use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use axum::extract::{Path, State};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures::stream::Stream;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::{broadcast, mpsc};

use crate::daemon::app::AppState;
use crate::run::view::{GuiState, initial_state};
use crate::runner::{self, RunCtl, RunMode, StepChoice};

use super::{RouteError, RouteResult};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/run/recipes", get(recipes))
        .route("/api/run/preflight", post(preflight))
        .route("/api/run/runs", get(list).post(create))
        .route("/api/run/runs/{id}", get(detail))
        .route("/api/run/runs/{id}/stream", get(stream))
        .route("/api/run/runs/{id}/choose", post(choose))
}

// ─── Runs ───────────────────────────────────────────────────────────────────

/// One run in flight (or finished — the logs stay readable afterwards).
pub struct Run {
    pub id: u64,
    pub project: String,
    pub recipes: Vec<String>,
    pub created_at: String,
    pub state: Arc<Mutex<GuiState>>,
    /// Carries a browser's answer back to a waiting recovery step.
    pub choices: broadcast::Sender<StepChoice>,
    /// Bumped on every applied update, so the SSE loop can tell whether
    /// anything changed without diffing the whole state.
    pub seq: Arc<AtomicU64>,
    /// Notified whenever `seq` moves.
    pub changed: Arc<tokio::sync::Notify>,
}

impl Run {
    fn summary(&self) -> Value {
        let state = self.state.lock().unwrap();
        json!({
            "id": self.id,
            "project": self.project,
            "recipes": self.recipes,
            "created_at": self.created_at,
            "done": serde_json::to_value(&*state).ok().and_then(|v| v["done"].as_bool()).unwrap_or(false),
        })
    }
}

/// Every run the daemon knows about.
#[derive(Default)]
pub struct Runs {
    inner: Mutex<HashMap<u64, Arc<Run>>>,
    next_id: AtomicU64,
}

impl Runs {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(1),
        })
    }

    fn insert(&self, run: Run) -> Arc<Run> {
        let run = Arc::new(run);
        self.inner.lock().unwrap().insert(run.id, run.clone());
        run
    }

    fn get(&self, id: u64) -> Option<Arc<Run>> {
        self.inner.lock().unwrap().get(&id).cloned()
    }

    fn list(&self) -> Vec<Arc<Run>> {
        let mut all: Vec<Arc<Run>> = self.inner.lock().unwrap().values().cloned().collect();
        all.sort_by_key(|r| std::cmp::Reverse(r.id));
        all
    }

    fn next_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }
}

// ─── Payloads ───────────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct ProjectQuery {
    project: String,
}

#[derive(Deserialize)]
pub struct CreatePayload {
    project: String,
    /// Recipe names. Empty means every run-capable recipe.
    #[serde(default)]
    recipes: Vec<String>,
    /// A monorepo workflow name to run instead of recipes. The daemon compiles
    /// the cross-workspace graph itself from the same declarations the CLI
    /// reads, so a browser-launched workflow can't drift from a terminal one.
    #[serde(default)]
    workflow: Option<String>,
    /// Further workflows folded into the same graph, matching
    /// `ciabatta run build test` on the command line.
    #[serde(default)]
    workflows: Vec<String>,
    /// With `workflow`: start only from these sub-workspaces.
    #[serde(default)]
    only: Vec<String>,
    /// With `workflow`: don't follow dependencies into other sub-workspaces.
    #[serde(default)]
    isolated: bool,
    /// With `workflow`: run only the steps these terms select. Same syntax as
    /// the CLI's `--filter`.
    #[serde(default)]
    filter: Vec<String>,
    #[serde(default)]
    env: HashMap<String, String>,
    #[serde(default)]
    dry_run: bool,
}

#[derive(Deserialize)]
pub struct ChoosePayload {
    recipe: String,
    step: String,
    /// Index into the pending choice's `options`.
    option: usize,
}

// ─── Handlers ───────────────────────────────────────────────────────────────

/// Which recipes a request targets: the ones it named, or every run-capable
/// recipe when it named none — matching `select_run_names` on the CLI side.
fn run_capable_names(config: &crate::config::CiabattaConfig, requested: &[String]) -> Vec<String> {
    if !requested.is_empty() {
        return requested.to_vec();
    }
    let mut all: Vec<String> = config
        .recipes
        .iter()
        .filter(|(_, entry)| entry.run_recipe().is_some())
        .map(|(name, _)| name.clone())
        .collect();
    all.sort();
    all
}

/// The run-capable recipes in a project, for the launcher.
async fn recipes(
    State(state): State<AppState>,
    axum::extract::Query(query): axum::extract::Query<ProjectQuery>,
) -> RouteResult<Json<Value>> {
    let root = state.project_root(&query.project)?;
    let config = crate::config::load_config(&root)?;

    Ok(Json(json!({ "recipes": run_capable_names(&config, &[]) })))
}

async fn list(State(state): State<AppState>) -> Json<Vec<Value>> {
    Json(state.runs.list().iter().map(|r| r.summary()).collect())
}

/// Start a run and return its id.
async fn create(
    State(state): State<AppState>,
    Json(payload): Json<CreatePayload>,
) -> RouteResult<Json<Value>> {
    let project_root = state.project_root(&payload.project)?;
    let mut config = crate::config::load_config(&project_root)?;

    // A workflow request compiles the monorepo graph and installs it as a
    // single run-capable recipe; from there it's an ordinary run. It also runs
    // from the *monorepo* root rather than the registered project directory,
    // since every step's `cwd` is expressed relative to that.
    let workflows: Vec<String> = payload
        .workflow
        .iter()
        .cloned()
        .chain(payload.workflows.iter().cloned())
        .collect();

    let (root, names) = if workflows.is_empty() {
        (project_root, run_capable_names(&config, &payload.recipes))
    } else {
        let selection = crate::workspace::graph::Selection {
            only: payload.only.clone(),
            isolated: payload.isolated,
        };
        let (workspace, mut graph) =
            crate::workspace::graph::prepare_many(&project_root, &workflows, &selection)
                .map_err(RouteError::bad_request)?;

        // A filtered graph is compiled whole and then pruned, exactly as the
        // CLI does it, so the browser and the terminal can't disagree about
        // what a given filter selects.
        let filters =
            crate::run::filter::parse_all(&payload.filter).map_err(RouteError::bad_request)?;
        let (steps, _) =
            crate::run::filter::apply(&graph.steps, &filters).map_err(RouteError::bad_request)?;
        graph.steps = steps;

        let name = crate::workspace::graph::install_as_recipe(&mut config, graph);
        (workspace.root, vec![name])
    };

    // Seed from the daemon's own environment, so a run started from the browser
    // sees what one started from a terminal would (`build_env_vars` does the
    // same), then layer whatever the caller supplied on top.
    let mut env: HashMap<String, String> = std::env::vars().collect();
    env.extend(payload.env.clone());

    // Fail fast on a bad flowchart, before anything is spawned. The resolved
    // environment goes in with it, so the view can show each step's variables
    // alongside the step itself.
    let gui_state = Arc::new(Mutex::new(
        initial_state(&config, &root, &names, payload.dry_run, &env)
            .map_err(RouteError::bad_request)?,
    ));

    // Pre-flight the environment rather than spawning a run that would only
    // abort at the engine's `REQUIRED_ENV` gate. 422 carries the variable names
    // so the launcher can prompt for them and post again.
    let missing = missing_env(&config, &root, &names, &env)?;
    if !missing.is_empty() {
        return Err(RouteError::missing_env(&missing));
    }

    let (choice_tx, _) = broadcast::channel::<StepChoice>(64);
    let (progress_tx, mut progress_rx) = mpsc::channel(256);

    let run = state.runs.insert(Run {
        id: state.runs.next_id(),
        project: payload.project,
        recipes: names.clone(),
        created_at: chrono::Local::now().to_rfc3339(),
        state: gui_state.clone(),
        choices: choice_tx.clone(),
        seq: Arc::new(AtomicU64::new(0)),
        changed: Arc::new(tokio::sync::Notify::new()),
    });

    // Fold progress into the view model, waking SSE subscribers as it lands.
    {
        let seq = run.seq.clone();
        let changed = run.changed.clone();
        tokio::spawn(async move {
            while let Some(update) = progress_rx.recv().await {
                gui_state.lock().unwrap().apply(update);
                seq.fetch_add(1, Ordering::Relaxed);
                changed.notify_waiters();
            }
            // The engine dropped its sender: the run is over. Wake subscribers
            // one last time so they see the final state and close.
            changed.notify_waiters();
        });
    }

    // Interactive, so recovery steps wait for a browser choice.
    let ctl = RunCtl {
        interactive: true,
        choices: Some(choice_tx),
        ..Default::default()
    };
    tokio::spawn(async move {
        let _ = runner::run_all_ctl(
            &config,
            &root,
            &names,
            &env,
            payload.dry_run,
            RunMode::Run,
            ctl,
            progress_tx,
        )
        .await;
    });

    Ok(Json(run.summary()))
}

/// Every variable the named recipes need before they can start, given `env`.
///
/// The union across recipes, de-duplicated and in the order the launcher should
/// ask for them. Empty means the run is ready to go. Runs the same
/// [`crate::run::prepare_env`] the engine does, so the two can't disagree about
/// what's missing.
fn missing_env(
    config: &crate::config::CiabattaConfig,
    root: &std::path::Path,
    names: &[String],
    env: &HashMap<String, String>,
) -> RouteResult<Vec<String>> {
    let mut missing: Vec<String> = Vec::new();
    for name in names {
        let entry = config
            .recipes
            .get(name)
            .ok_or_else(|| RouteError::bad_request(format!("Recipe '{name}' not found")))?;
        let recipe = entry.run_recipe().ok_or_else(|| {
            RouteError::bad_request(format!("Recipe '{name}' has no [run] definition"))
        })?;
        let resolved =
            crate::run::resolve_run(recipe, name, root).map_err(RouteError::bad_request)?;
        let prepared =
            crate::run::prepare_env(&resolved, root, env).map_err(RouteError::bad_request)?;
        for var in prepared.missing() {
            if !missing.contains(&var) {
                missing.push(var);
            }
        }
    }
    Ok(missing)
}

/// What a run would still need, without starting it — so the launcher can put
/// the prompt in front of the operator instead of behind a failed run.
async fn preflight(
    State(state): State<AppState>,
    Json(payload): Json<CreatePayload>,
) -> RouteResult<Json<Value>> {
    let root = state.project_root(&payload.project)?;
    let config = crate::config::load_config(&root)?;
    let names = run_capable_names(&config, &payload.recipes);

    let mut env: HashMap<String, String> = std::env::vars().collect();
    env.extend(payload.env.clone());

    Ok(Json(
        json!({ "missing_env": missing_env(&config, &root, &names, &env)? }),
    ))
}

/// The full current state of a run.
async fn detail(State(state): State<AppState>, Path(id): Path<u64>) -> RouteResult<Json<Value>> {
    let run = get_run(&state, id)?;
    Ok(Json(snapshot(&run)))
}

/// Live run state as Server-Sent Events.
async fn stream(
    State(state): State<AppState>,
    Path(id): Path<u64>,
) -> RouteResult<Sse<impl Stream<Item = Result<Event, Infallible>>>> {
    let run = get_run(&state, id)?;

    let events = async_stream::stream! {
        loop {
            let payload = snapshot(&run);
            let done = payload["done"].as_bool().unwrap_or(false);

            if let Ok(event) = Event::default().json_data(&payload) {
                yield Ok(event);
            }

            if done {
                break;
            }

            run.changed.notified().await;
        }
    };

    Ok(Sse::new(events).keep_alive(KeepAlive::default()))
}

/// Answer a waiting recovery prompt.
async fn choose(
    State(state): State<AppState>,
    Path(id): Path<u64>,
    Json(payload): Json<ChoosePayload>,
) -> RouteResult<Json<Value>> {
    let run = get_run(&state, id)?;

    // A send error means nothing is waiting — the step already moved on, or the
    // run finished. That's a stale click, not a server fault.
    run.choices
        .send(StepChoice {
            recipe: payload.recipe,
            step: payload.step,
            option: payload.option,
        })
        .map_err(|_| {
            RouteError::bad_request(
                "Nothing is waiting for that choice — the step already moved on.",
            )
        })?;

    Ok(Json(json!({ "ok": true })))
}

/// The run's view state plus its identity, as one payload.
fn snapshot(run: &Run) -> Value {
    let mut value = serde_json::to_value(&*run.state.lock().unwrap()).unwrap_or_else(|_| json!({}));
    value["run"] = run.summary();
    value["seq"] = json!(run.seq.load(Ordering::Relaxed));
    value
}

fn get_run(state: &AppState, id: u64) -> RouteResult<Arc<Run>> {
    state
        .runs
        .get(id)
        .ok_or_else(|| RouteError::not_found(format!("No run {id}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::app::router;
    use crate::daemon::app::tests::test_state;

    use axum::body::Body;
    use axum::http::{Request, StatusCode, header};
    use tower::ServiceExt;

    fn env_map(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn missing_env_reports_the_union_across_recipes_once() {
        let config: crate::config::CiabattaConfig = toml::from_str(
            r#"
[recipies.web.run]
REQUIRED_ENV = ["API_TOKEN", "REGION"]
[[recipies.web.run.steps]]
name = "build"
run = "true"

[recipies.api.run]
REQUIRED_ENV = ["REGION", "STAGE"]
[[recipies.api.run.steps]]
name = "ship"
run = "true"
"#,
        )
        .expect("config parses");
        let root = std::path::Path::new("/proj");
        let names = vec!["web".to_string(), "api".to_string()];

        // REGION is required by both but asked for once, in first-seen order.
        let missing = missing_env(&config, root, &names, &env_map(&[])).unwrap();
        assert_eq!(missing, vec!["API_TOKEN", "REGION", "STAGE"]);

        // Values already in the environment aren't asked for.
        let missing =
            missing_env(&config, root, &names, &env_map(&[("REGION", "us-east-1")])).unwrap();
        assert_eq!(missing, vec!["API_TOKEN", "STAGE"]);

        // Everything supplied → nothing to prompt for, so the run may start.
        let missing = missing_env(
            &config,
            root,
            &names,
            &env_map(&[("API_TOKEN", "t"), ("REGION", "r"), ("STAGE", "s")]),
        )
        .unwrap();
        assert!(missing.is_empty());
    }

    #[test]
    fn missing_env_rejection_carries_the_names_for_the_launcher() {
        let error = RouteError::missing_env(&["API_TOKEN".to_string(), "REGION".to_string()]);
        assert_eq!(error.status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(
            error.details.unwrap()["missing_env"],
            serde_json::json!(["API_TOKEN", "REGION"])
        );
    }

    #[tokio::test]
    async fn starting_a_run_requires_the_token() {
        let app = router(test_state());
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/run/runs")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"project":"x"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "an unauthenticated caller must not be able to start a run"
        );
    }

    #[tokio::test]
    async fn unknown_runs_are_404() {
        let app = router(test_state());
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/run/runs/4242")
                    .header(header::AUTHORIZATION, "Bearer test-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn listing_recipes_rejects_an_unknown_project() {
        let app = router(test_state());
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/run/recipes?project=nope")
                    .header(header::AUTHORIZATION, "Bearer test-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
}
