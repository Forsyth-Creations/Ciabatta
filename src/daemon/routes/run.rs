//! Run routes: execute a workflow DAG live, with fix-it branches.
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
use futures::FutureExt;
use futures::stream::Stream;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::{broadcast, mpsc};

use crate::daemon::app::AppState;
use crate::run::view::{GuiState, initial_state};
use crate::runner::{self, RunCtl, StepChoice};

use super::{RouteError, RouteResult};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/run/workflows", get(workflows))
        .route("/api/run/env-profiles", get(env_profiles))
        .route("/api/run/preflight", post(preflight))
        .route("/api/run/runs", get(list).post(create))
        .route("/api/run/runs/{id}", get(detail).delete(remove))
        .route("/api/run/settings", get(settings).post(set_settings))
        .route("/api/run/runs/{id}/stream", get(stream))
        .route("/api/run/runs/{id}/choose", post(choose))
        .route("/api/run/runs/{id}/input", post(answer))
        .route("/api/run/inputs", get(inputs))
        .route("/api/run/runs/{id}/stop", post(stop))
        .route("/api/run/runs/{id}/rerun", post(rerun))
}

/// Take a lock, surviving a poisoned one.
///
/// A run's state is touched by the engine's fold task and by every HTTP handler
/// that reads it. If one of them panics while holding this lock, the default
/// `.unwrap()` turns that single fault into a panic on *every* subsequent
/// request — burying the one panic that mattered under a hundred that didn't.
/// The data behind it is a view model rebuilt from the next update, so reading
/// it after a panic is worth doing and worth saying out loud.
fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poisoned| {
        tracing::error!(
            "recovering a run lock poisoned by an earlier panic — the panic \
             itself is logged above, and is the fault worth reading"
        );
        poisoned.into_inner()
    })
}

// ─── Runs ───────────────────────────────────────────────────────────────────

/// One run in flight (or finished — the logs stay readable afterwards).
pub struct Run {
    pub id: u64,
    pub project: String,
    pub workflows: Vec<String>,
    pub created_at: String,
    /// The root the run's steps resolve their `cwd` against — the monorepo
    /// root, which is not necessarily the registered project directory.
    pub root: std::path::PathBuf,
    /// The request that started it, kept so it can be started again *exactly*.
    ///
    /// Re-running from the stored payload rather than from a summary is what
    /// makes "run it again" honest: the filters, the `--only` list and the
    /// variables the caller supplied are all part of what ran, and a re-run
    /// reconstructed from the fields a summary happens to expose would quietly
    /// be a different run.
    pub request: CreatePayload,
    pub state: Arc<Mutex<GuiState>>,
    /// Carries a browser's answer back to a waiting recovery step.
    pub choices: broadcast::Sender<StepChoice>,
    /// Carries a browser's answer back to a step waiting in `ciabatta input`.
    pub inputs: broadcast::Sender<crate::run::input::Answer>,
    /// The run's stop switch, held so the Stop button can reach the engine.
    pub cancel: Arc<runner::Cancel>,
    /// Bumped on every applied update, so the SSE loop can tell whether
    /// anything changed without diffing the whole state.
    pub seq: Arc<AtomicU64>,
    /// Notified whenever `seq` moves.
    pub changed: Arc<tokio::sync::Notify>,
}

impl Run {
    fn summary(&self) -> Value {
        let state = lock(&self.state);
        let (done, total, running) = state.progress();
        json!({
            "id": self.id,
            "project": self.project,
            "workflows": self.workflows,
            "created_at": self.created_at,
            "done": serde_json::to_value(&*state).ok().and_then(|v| v["done"].as_bool()).unwrap_or(false),
            // How it ended, not merely that it did: a list of runs that all say
            // "finished" answers the one question nobody is asking.
            "status": state.outcome(),
            // When it actually ran, as opposed to `created_at` (when it was
            // asked for), so the list can say how long each run took.
            "started_at": state.started_at(),
            "finished_at": state.finished_at(),
            // How far along it is, for an indicator that has no room for the
            // graph — the editor's status bar.
            "progress": { "done": done, "total": total, "running": running },
            // What it took to start it, so the run page can print the command
            // that reproduces it and say what it was narrowed to.
            "root": self.root.display().to_string(),
            "dry_run": self.request.dry_run,
            "force": self.request.force,
            "filter": self.request.filter,
            "only": self.request.only,
            "isolated": self.request.isolated,
            "args": self.request.args,
            "env_profile": self.request.env_profile,
        })
    }
}

/// Every run the daemon knows about.
#[derive(Default)]
pub struct Runs {
    inner: Mutex<HashMap<u64, Arc<Run>>>,
    next_id: AtomicU64,
    /// The history on disk. `None` only if the state directory couldn't be
    /// opened, in which case runs work exactly as they used to — in memory,
    /// until the daemon exits.
    store: Option<crate::daemon::run_store::RunStore>,
    /// The environment the last `--gui` launch brought from its terminal, per
    /// monorepo root — not per project, since the CLI registers the package it
    /// was typed in while the browser usually has the root open. Memory only, for the same reason `CreatePayload::env` is never
    /// stored: it is somebody's shell, tokens and all.
    ///
    /// The daemon's own environment is whatever the shell that first started
    /// it had — often days old, and from another terminal. A run started from
    /// the browser layers this over it, so a variable exported in the terminal
    /// that has been driving the project reaches the run the browser starts
    /// too, not only the one `--gui` started.
    terminal_env: Mutex<HashMap<std::path::PathBuf, HashMap<String, String>>>,
}

impl Runs {
    /// Open the run list, restoring whatever the last daemon left behind.
    pub fn new() -> Arc<Self> {
        let store = match crate::daemon::run_store::RunStore::open() {
            Ok(store) => Some(store),
            Err(err) => {
                tracing::warn!("run history is disabled: {err:#}");
                None
            }
        };

        let mut inner = HashMap::new();
        let mut highest = 0;
        for stored in store.iter().flat_map(|s| s.load_all()) {
            highest = highest.max(stored.id);
            let mut state = stored.state;
            // A run still marked running is one the last daemon was killed in
            // the middle of. Its process is gone, so saying it is running would
            // leave a Stop button on something that stopped when the daemon
            // did.
            if !state.done() {
                state.interrupted("the daemon restarted while this run was in flight");
            }
            let (choices, _) = broadcast::channel(64);
            let (inputs, _) = broadcast::channel(64);
            inner.insert(
                stored.id,
                Arc::new(Run {
                    id: stored.id,
                    project: stored.project,
                    workflows: stored.workflows,
                    created_at: stored.created_at,
                    root: stored.root,
                    request: stored.request,
                    state: Arc::new(Mutex::new(state)),
                    choices,
                    inputs,
                    cancel: Arc::new(runner::Cancel::default()),
                    seq: Arc::new(AtomicU64::new(0)),
                    changed: Arc::new(tokio::sync::Notify::new()),
                }),
            );
        }
        if !inner.is_empty() {
            tracing::info!(runs = inner.len(), "restored run history");
        }

        Arc::new(Self {
            inner: Mutex::new(inner),
            next_id: AtomicU64::new(highest + 1),
            store,
            terminal_env: Mutex::default(),
        })
    }

    /// Write a run's current state to the history.
    fn save(&self, run: &Run) {
        let Some(store) = &self.store else { return };
        store.save(&crate::daemon::run_store::StoredRun {
            version: crate::daemon::run_store::version(),
            id: run.id,
            project: run.project.clone(),
            workflows: run.workflows.clone(),
            created_at: run.created_at.clone(),
            root: run.root.clone(),
            request: run.request.clone(),
            state: lock(&run.state).clone(),
        });
    }

    /// Write the current state of the run with this id, if it is still here.
    fn save_id(&self, id: u64) {
        let run = lock(&self.inner).get(&id).cloned();
        if let Some(run) = run {
            self.save(&run);
        }
    }

    /// The TTL settings, or the defaults when there is no store.
    fn ttl(&self) -> crate::daemon::run_store::Settings {
        self.store
            .as_ref()
            .map(|s| s.settings())
            .unwrap_or_default()
    }

    /// Change the TTL, dropping whatever it has just made stale — from memory
    /// too, so the list agrees with the disk without a restart.
    fn set_ttl(&self, next: crate::daemon::run_store::Settings) -> anyhow::Result<usize> {
        let Some(store) = &self.store else {
            return Ok(0);
        };
        let pruned = store.set_settings(next)?;
        let kept: std::collections::HashSet<u64> =
            store.load_all().iter().map(|run| run.id).collect();
        lock(&self.inner).retain(|id, run| {
            // A run in flight has no record on disk to keep it, and isn't
            // history yet either.
            kept.contains(id) || !lock(&run.state).done()
        });
        Ok(pruned)
    }

    /// Forget a run, in memory and on disk.
    fn remove(&self, id: u64) -> Option<Arc<Run>> {
        let run = lock(&self.inner).remove(&id);
        if run.is_some()
            && let Some(store) = &self.store
        {
            store.delete(id);
        }
        run
    }

    fn insert(&self, run: Run) -> Arc<Run> {
        let run = Arc::new(run);
        lock(&self.inner).insert(run.id, run.clone());
        run
    }

    fn get(&self, id: u64) -> Option<Arc<Run>> {
        lock(&self.inner).get(&id).cloned()
    }

    fn list(&self) -> Vec<Arc<Run>> {
        let mut all: Vec<Arc<Run>> = lock(&self.inner).values().cloned().collect();
        all.sort_by_key(|r| std::cmp::Reverse(r.id));
        all
    }

    fn next_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    /// The environment a run starts with: the daemon's own, then — for a run
    /// the browser started — the last terminal's for this project, then what
    /// the request itself carried. A terminal launch replaces the remembered
    /// one, since it is now the latest word on what that shell holds.
    fn environment_for(
        &self,
        root: &std::path::Path,
        payload: &CreatePayload,
    ) -> HashMap<String, String> {
        let mut env: HashMap<String, String> = std::env::vars().collect();
        let mut remembered = lock(&self.terminal_env);
        if payload.terminal {
            remembered.insert(root.to_path_buf(), payload.env.clone());
        } else if let Some(terminal) = remembered.get(root) {
            env.extend(terminal.clone());
        }
        env.extend(payload.env.clone());
        env
    }
}

// ─── Payloads ───────────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct ProjectQuery {
    project: String,
}

#[derive(Deserialize, Serialize, Clone)]
pub struct CreatePayload {
    project: String,
    /// A monorepo workflow name to run instead of workflows. The daemon compiles
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
    /// Values for variables the run needs and the daemon's environment lacks.
    ///
    /// Never written to the run's stored record: these come from the "this run
    /// needs a few variables" prompt, and what people type into it is usually a
    /// token. A re-run of a restored run asks again.
    #[serde(default, skip_serializing)]
    env: HashMap<String, String>,
    #[serde(default)]
    dry_run: bool,
    /// Ignore the cache and run every step. Results are still stored.
    #[serde(default)]
    force: bool,
    /// Custom arguments, as typed after `...` — each becomes an
    /// environment variable of its own name. Kept with the run (unlike `env`), so
    /// "run again" passes them again.
    #[serde(default)]
    args: Vec<String>,
    /// The env profile to run under (`--env-profile`).
    #[serde(default)]
    env_profile: Option<String>,
    /// Where the run was started, relative to the monorepo root — the
    /// directory a `--gui` launch was typed in. The `.env` search runs from
    /// there up to the root, exactly as it does for a run in the terminal;
    /// without it a run from `packages/api` would never read
    /// `packages/api/.env`. Kept with the run, so "run again" searches the
    /// same files.
    #[serde(default)]
    cwd: Option<String>,
    /// Sent by `--gui`: `env` is that terminal's whole environment, and is
    /// remembered for the runs the browser starts next in the same checkout (see
    /// [`Runs::environment_for`]).
    #[serde(default, skip_serializing)]
    terminal: bool,
}

#[derive(Deserialize)]
pub struct AnswerPayload {
    /// The question's id, from the run's `inputs`.
    id: u64,
    value: String,
}

#[derive(Deserialize)]
pub struct ChoosePayload {
    workflow: String,
    step: String,
    /// Index into the pending choice's `options`.
    option: usize,
}

// ─── Handlers ───────────────────────────────────────────────────────────────

/// The workflows a project can run, for the launcher.
async fn workflows(
    State(state): State<AppState>,
    axum::extract::Query(query): axum::extract::Query<ProjectQuery>,
) -> RouteResult<Json<Value>> {
    let root = state.project_root(&query.project)?;
    let names = crate::workspace::Workspace::discover(&root)
        .map(|ws| ws.workflow_names())
        .unwrap_or_default();

    Ok(Json(json!({ "workflows": names })))
}

/// Narrows the run list to one project.
#[derive(Deserialize)]
pub struct ListQuery {
    #[serde(default)]
    project: Option<String>,
}

/// Every run, newest first — or only one project's, with `?project=`.
///
/// Filtered here rather than in the page: one daemon serves every checkout on
/// the machine, and a run list that ignored the project switcher showed
/// another repo's builds under this one's name.
/// The env profiles a project has, for the launcher's picker.
async fn env_profiles(
    State(state): State<AppState>,
    axum::extract::Query(query): axum::extract::Query<ProjectQuery>,
) -> RouteResult<Json<Value>> {
    let root = state.project_root(&query.project)?;
    let root = crate::workspace::Workspace::discover(&root)
        .map(|ws| ws.root)
        .unwrap_or(root);
    let dirs = crate::environment::profiles::workspace_dirs(&root);
    Ok(Json(json!({
        "profiles": crate::environment::profiles::list(&root, &dirs),
    })))
}

async fn list(
    State(state): State<AppState>,
    axum::extract::Query(query): axum::extract::Query<ListQuery>,
) -> Json<Vec<Value>> {
    Json(
        state
            .runs
            .list()
            .iter()
            .filter(|run| query.project.as_ref().is_none_or(|p| &run.project == p))
            .map(|r| r.summary())
            .collect(),
    )
}

/// Start a run and return its id.
async fn create(
    State(state): State<AppState>,
    Json(payload): Json<CreatePayload>,
) -> RouteResult<Json<Value>> {
    start(state, payload).await
}

/// Start a previous run again, from the request that started it.
///
/// The graph is compiled fresh rather than replayed: the point of re-running is
/// usually that something on disk changed, and a replay of the old graph would
/// be the one thing that can't tell you whether the change helped.
async fn rerun(State(state): State<AppState>, Path(id): Path<u64>) -> RouteResult<Json<Value>> {
    let run = state
        .runs
        .get(id)
        .ok_or_else(|| RouteError::not_found("No such run."))?;
    let payload = run.request.clone();
    start(state, payload).await
}

/// The directory a run's `.env` search starts from: `cwd` under `root`, or
/// the root itself when it is missing, or would lead out of the checkout.
fn start_dir(root: &std::path::Path, cwd: Option<&str>) -> std::path::PathBuf {
    let Some(cwd) = cwd.filter(|c| !c.is_empty() && *c != ".") else {
        return root.to_path_buf();
    };
    let relative = std::path::Path::new(cwd);
    let inside = relative
        .components()
        .all(|part| matches!(part, std::path::Component::Normal(_)));
    match inside && root.join(relative).is_dir() {
        true => root.join(relative),
        false => root.to_path_buf(),
    }
}

async fn start(state: AppState, payload: CreatePayload) -> RouteResult<Json<Value>> {
    let project_root = state.project_root(&payload.project)?;
    let config = crate::config::load_config(&project_root)?;

    // A workflow request compiles the monorepo graph and runs it. It also runs
    // from the *monorepo* root rather than the registered project directory,
    // since every step's `cwd` is expressed relative to that.
    let workflows: Vec<String> = payload
        .workflow
        .iter()
        .cloned()
        .chain(payload.workflows.iter().cloned())
        .collect();

    let (root, runs) = {
        if workflows.is_empty() {
            return Err(RouteError::bad_request(
                "No workflow named. Pass `workflow` (or `workflows`) — \
                 everything ciabatta runs is a workflow.",
            ));
        }
        let selection = crate::workspace::graph::Selection {
            only: payload.only.clone(),
            isolated: payload.isolated,
            provided: provided(&payload)?,
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

        let name = graph.label();
        let resolved = crate::workspace::graph::into_run(graph).map_err(RouteError::bad_request)?;
        // Settled here, before anything reads the environment: the view, the
        // pre-flight and the engine all call `with_env_levels` and keep the
        // first answer, so this one is the answer for all three.
        let start = start_dir(&workspace.root, payload.cwd.as_deref());
        let meta = workspace.root_meta.clone();
        let resolved = crate::run::with_env_levels(resolved, &workspace.root, &start, &meta);
        (workspace.root, vec![(name, resolved)])
    };
    let names: Vec<String> = runs.iter().map(|(name, _)| name.clone()).collect();

    // Seed from the daemon's own environment and, for a run the browser
    // started, the last terminal's (`build_env_vars` does the same for a run in
    // a terminal), then layer whatever the caller supplied on top.
    let mut env = state.runs.environment_for(&root, &payload);
    // Custom arguments and the profile after everything else: they were asked
    // for, for this run.
    crate::cli::apply_custom_args(&mut env, &payload.args).map_err(RouteError::bad_request)?;
    if let Some(profile) = payload.env_profile.as_deref().filter(|p| !p.is_empty()) {
        crate::environment::profiles::require(&root, profile).map_err(RouteError::bad_request)?;
        env.insert(
            crate::environment::profiles::PROFILE_VAR.to_string(),
            profile.to_string(),
        );
        env.insert(
            crate::environment::profiles::PROFILE_FROM_VAR.to_string(),
            "launcher".to_string(),
        );
    }

    // Fail fast on a bad flowchart, before anything is spawned. The resolved
    // environment goes in with it, so the view can show each step's variables
    // alongside the step itself.
    let gui_state = Arc::new(Mutex::new(
        initial_state(&config, &root, &runs, payload.dry_run, &env)
            .map_err(RouteError::bad_request)?,
    ));

    // Pre-flight the environment rather than spawning a run that would only
    // abort at the engine's `required_env` gate. 422 carries the variable names
    // so the launcher can prompt for them and post again.
    let missing = missing_env(&root, &runs, &env)?;
    if !missing.is_empty() {
        return Err(RouteError::missing_env(&missing));
    }

    let (choice_tx, _) = broadcast::channel::<StepChoice>(64);
    let (input_tx, _) = broadcast::channel::<crate::run::input::Answer>(64);
    let (progress_tx, mut progress_rx) = mpsc::channel(256);
    // Shared three ways: the run record (so `stop` can reach it), the engine
    // (which checks it), and nothing else.
    let cancel = Arc::new(runner::Cancel::default());

    let run = state.runs.insert(Run {
        id: state.runs.next_id(),
        project: payload.project.clone(),
        workflows: names.clone(),
        created_at: chrono::Local::now().to_rfc3339(),
        root: root.clone(),
        request: payload.clone(),
        state: gui_state.clone(),
        choices: choice_tx.clone(),
        inputs: input_tx.clone(),
        cancel: cancel.clone(),
        seq: Arc::new(AtomicU64::new(0)),
        changed: Arc::new(tokio::sync::Notify::new()),
    });

    let id = run.id;
    // Written down before anything is spawned, so a daemon that dies mid-run
    // still leaves a record of what it was doing — the next one marks it
    // stopped rather than losing it.
    state.runs.save(&run);

    tracing::info!(
        run = id,
        root = %root.display(),
        workflows = ?names,
        dry_run = payload.dry_run,
        "starting run"
    );

    // Why the run stopped, when it stopped for a reason the engine couldn't
    // report itself. Written by the run task before it drops its sender, and
    // read by the fold task once the channel closes — which is the moment it
    // knows nothing else is coming.
    let stopped: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));

    // Fold progress into the view model, waking SSE subscribers as it lands.
    {
        let seq = run.seq.clone();
        let changed = run.changed.clone();
        let stopped = stopped.clone();
        let expected = names.clone();
        let runs = state.runs.clone();
        tokio::spawn(async move {
            let mut reported: Vec<String> = Vec::new();
            while let Some(update) = progress_rx.recv().await {
                // Logged here rather than in the engine because this is the one
                // place every update passes through, and because a daemon that
                // dies mid-run leaves this trail: the last step logged is the
                // step it died in.
                trace(id, &update);
                match &update {
                    runner::ProgressUpdate::Completed(workflow)
                    | runner::ProgressUpdate::Failed(workflow, _) => {
                        reported.push(workflow.clone())
                    }
                    _ => {}
                }
                lock(&gui_state).apply(update);
                seq.fetch_add(1, Ordering::Relaxed);
                changed.notify_waiters();
            }

            // The senders are gone: nothing further can arrive. A workflow with no
            // verdict by now will never get one — its task died without
            // reporting, which is what a panic inside the engine looks like from
            // out here. Saying so is the difference between a run that shows an
            // error and a run that spins in the browser until someone reloads.
            let orphaned: Vec<String> = expected
                .into_iter()
                .filter(|name| !reported.contains(name))
                .collect();

            if !orphaned.is_empty() {
                let reason = lock(&stopped)
                    .clone()
                    .unwrap_or_else(|| "the run stopped without reporting why".to_string());
                tracing::error!(
                    run = id,
                    workflows = ?orphaned,
                    "the run ended without a verdict: {reason}"
                );
                for name in orphaned {
                    lock(&gui_state).apply(runner::ProgressUpdate::Failed(
                        name,
                        format!("{reason} — see `ciabatta daemon logs`"),
                    ));
                    seq.fetch_add(1, Ordering::Relaxed);
                }
            }

            // The run is over and its logs are complete: this is the copy
            // anyone will read tomorrow.
            runs.save_id(id);

            // Wake subscribers one last time so they see the final state and
            // close.
            changed.notify_waiters();
        });
    }

    // Interactive, so recovery steps wait for a browser choice, and
    // interruptible, so the Stop button has something to press.
    let ctl = RunCtl {
        interactive: true,
        choices: Some(choice_tx),
        // And steps can ask questions, answered from the run page's dialog.
        inputs: Some(input_tx),
        cancel: Some(cancel),
        force: payload.force,
        ..Default::default()
    };
    tokio::spawn(async move {
        // `catch_unwind` for a panic in the engine's own frame; tokio turns a
        // panic in a task the engine spawned into an `Err` instead, so both
        // shapes have to be handled to end up with one explanation.
        let outcome = std::panic::AssertUnwindSafe(runner::run_workflow_ctl(
            &runs[0].0,
            &runs[0].1,
            &config,
            &root,
            &env,
            payload.dry_run,
            ctl,
            progress_tx,
        ))
        .catch_unwind()
        .await;

        let reason = match outcome {
            Ok(Ok(())) => {
                tracing::info!(run = id, "run finished");
                return;
            }
            Ok(Err(err)) => {
                tracing::error!(run = id, "run failed: {err:#}");
                format!("{err:#}")
            }
            Err(payload) => {
                let message = payload
                    .downcast_ref::<&str>()
                    .map(|s| s.to_string())
                    .or_else(|| payload.downcast_ref::<String>().cloned())
                    .unwrap_or_else(|| "<non-string panic payload>".to_string());
                tracing::error!(
                    run = id,
                    "the run's engine panicked: {message} — the panic entry \
                     above has the location and stack"
                );
                format!("ciabatta itself panicked: {message}")
            }
        };

        // For the fold task to hand to any workflow the engine never reported on.
        // Set before this task ends, because ending is what closes the channel
        // and sends the fold task looking for it.
        *lock(&stopped) = Some(reason);
    });

    Ok(Json(run.summary()))
}

/// One log line per progress update, at the level its content deserves.
///
/// Step boundaries are `info` because they are the timeline somebody reads
/// after a crash; a step's own output is `debug`, because a build's thousand
/// lines are not that timeline — but they are there under
/// `CIABATTA_LOG=ciabatta=debug` when the crash is inside one of them.
fn trace(run: u64, update: &runner::ProgressUpdate) {
    use runner::ProgressUpdate as P;
    match update {
        P::Started(workflow) => tracing::info!(run, %workflow, "workflow started"),
        P::StageStarted { workflow, stage } => {
            tracing::info!(run, %workflow, stage = ?stage, "stage started")
        }
        P::StageFinished {
            workflow,
            stage,
            ran,
        } => {
            tracing::debug!(run, %workflow, stage = ?stage, ran, "stage finished")
        }
        P::TransferProgress {
            workflow,
            done,
            total,
        } => tracing::debug!(run, %workflow, done, total, "transfer progress"),
        P::Log(workflow, line) => tracing::debug!(run, %workflow, "{line}"),
        P::StepStarted { workflow, step } => tracing::info!(run, %workflow, %step, "step started"),
        P::StepFinished { workflow, step, ok } => {
            if *ok {
                tracing::info!(run, %workflow, %step, "step finished")
            } else {
                tracing::warn!(run, %workflow, %step, "step failed")
            }
        }
        P::StepWarned {
            workflow,
            step,
            detail,
        } => tracing::warn!(run, %workflow, %step, %detail, "step failed as a warning"),
        P::StepSkipped {
            workflow,
            step,
            reason,
        } => tracing::info!(run, %workflow, %step, %reason, "step skipped"),
        P::StepLog {
            workflow,
            step,
            line,
        } => tracing::debug!(run, %workflow, %step, "{line}"),
        P::StepCache {
            workflow,
            step,
            report,
        } => {
            tracing::debug!(run, %workflow, %step, outcome = %report.outcome, "{}", report.summary)
        }
        P::StepProcessing { workflow, step } => {
            tracing::debug!(run, %workflow, %step, "deciding whether the cache has it")
        }
        P::StepRestoring {
            workflow,
            step,
            progress,
        } => tracing::debug!(
            run,
            %workflow,
            %step,
            phase = %progress.phase,
            files = progress.files_done,
            bytes = progress.bytes_done,
            "restoring from the remote cache"
        ),
        P::StepNeedsChoice {
            workflow,
            step,
            message,
            ..
        } => tracing::info!(run, %workflow, %step, %message, "step is waiting for a choice"),
        P::InputRequested {
            workflow,
            step,
            input,
        } => {
            tracing::info!(run, %workflow, %step, prompt = %input.prompt, "step is waiting for input")
        }
        P::InputResolved {
            workflow,
            step,
            outcome,
            ..
        } => tracing::info!(run, %workflow, %step, outcome = outcome.label(), "input settled"),
        P::Warning { workflow, message } => tracing::warn!(run, %workflow, "{message}"),
        P::Completed(workflow) => tracing::info!(run, %workflow, "workflow completed"),
        P::Failed(workflow, err) => tracing::error!(run, %workflow, "workflow failed: {err}"),
    }
}

/// Every variable the named workflows need before they can start, given `env`.
///
/// The union across workflows, de-duplicated and in the order the launcher should
/// ask for them. Empty means the run is ready to go. Runs the same
/// [`crate::run::prepare_env`] the engine does, so the two can't disagree about
/// what's missing.
fn missing_env(
    root: &std::path::Path,
    runs: &[(String, crate::run::ResolvedRun)],
    env: &HashMap<String, String>,
) -> RouteResult<Vec<String>> {
    let meta = crate::config::load_config(root)
        .ok()
        .and_then(|config| config.workspace)
        .unwrap_or_default();
    let mut missing: Vec<String> = Vec::new();
    for (_, resolved) in runs {
        // Folded in the way the engine will, or a variable the root `.env`
        // sets would be asked for all the same.
        let resolved = crate::run::with_env_levels(resolved.clone(), root, root, &meta);
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

/// The variables a request sets for its run — the values it carries and its
/// custom arguments — which count toward `required_env` when the graph is
/// compiled, as they do once it runs.
fn provided(payload: &CreatePayload) -> Result<Vec<String>, RouteError> {
    let args = crate::cli::parse_custom_args(&payload.args).map_err(RouteError::bad_request)?;
    Ok(payload
        .env
        .keys()
        .cloned()
        .chain(args.into_keys())
        .collect())
}

/// What a run would still need, without starting it — so the launcher can put
/// the prompt in front of the operator instead of behind a failed run.
async fn preflight(
    State(state): State<AppState>,
    Json(payload): Json<CreatePayload>,
) -> RouteResult<Json<Value>> {
    let project_root = state.project_root(&payload.project)?;

    let workflows: Vec<String> = payload
        .workflow
        .iter()
        .cloned()
        .chain(payload.workflows.iter().cloned())
        .collect();
    if workflows.is_empty() {
        return Err(RouteError::bad_request(
            "No workflow named. Pass `workflow` (or `workflows`).",
        ));
    }

    let selection = crate::workspace::graph::Selection {
        only: payload.only.clone(),
        isolated: payload.isolated,
        provided: provided(&payload)?,
    };
    let (workspace, graph) =
        crate::workspace::graph::prepare_many(&project_root, &workflows, &selection)
            .map_err(RouteError::bad_request)?;
    let name = graph.label();
    let resolved = crate::workspace::graph::into_run(graph).map_err(RouteError::bad_request)?;
    let runs = vec![(name, resolved)];

    let mut env: HashMap<String, String> = std::env::vars().collect();
    env.extend(payload.env.clone());
    crate::cli::apply_custom_args(&mut env, &payload.args).map_err(RouteError::bad_request)?;

    Ok(Json(
        json!({ "missing_env": missing_env(&workspace.root, &runs, &env)? }),
    ))
}

/// The full current state of a run.
async fn detail(State(state): State<AppState>, Path(id): Path<u64>) -> RouteResult<Json<Value>> {
    let run = get_run(&state, id)?;
    Ok(Json(snapshot(&run)))
}

/// How long a burst of updates is allowed to accumulate before the next
/// snapshot goes out.
///
/// Every frame carries the *whole* run state, so one frame per update meant one
/// full serialization of every step, every dependency and every log line held
/// so far — per log line. A step printing a few thousand lines a second turned
/// that into megabytes a second of near-identical JSON, and the browser, which
/// re-renders the flowchart and the log pane on each frame, stopped responding.
///
/// A tenth of a second is below what anyone reads as lag on a build log and
/// puts a hard ceiling of ten frames a second on the work either end has to do,
/// whatever the step is printing.
const COALESCE: std::time::Duration = std::time::Duration::from_millis(100);

/// Live run state as Server-Sent Events.
async fn stream(
    State(state): State<AppState>,
    Path(id): Path<u64>,
) -> RouteResult<Sse<impl Stream<Item = Result<Event, Infallible>>>> {
    let run = get_run(&state, id)?;

    let events = async_stream::stream! {
        // The sequence last sent, so a tick with nothing new behind it sends
        // nothing. `u64::MAX` rather than 0: a run whose first update lands
        // before the first subscriber must still get an opening frame.
        let mut sent = u64::MAX;

        loop {
            // Registered *before* the state is read, which is what makes this
            // loop safe to coalesce. `notify_waiters` only wakes waiters that
            // are already registered, so an update landing between the read and
            // the await used to be dropped on the floor — the stream then sat
            // idle until the *next* update pushed it along, and on the last
            // line of a run there is no next update. Holding the registration
            // across the read means such an update wakes this immediately.
            let changed = run.changed.notified();
            tokio::pin!(changed);

            let seq = run.seq.load(Ordering::Relaxed);
            if seq != sent {
                sent = seq;
                let payload = snapshot(&run);
                let done = payload["done"].as_bool().unwrap_or(false);

                if let Ok(event) = Event::default().json_data(&payload) {
                    yield Ok(event);
                }

                if done {
                    break;
                }
            }

            changed.await;
            // Let the rest of the burst land before taking the next snapshot.
            // This is the whole coalescing step: whatever arrives in this
            // window is folded into one frame instead of one frame each.
            tokio::time::sleep(COALESCE).await;
        }
    };

    Ok(Sse::new(events).keep_alive(KeepAlive::default()))
}

/// Ask a run to stop.
///
/// Asking rather than killing: the engine stops scheduling, cuts short the step
/// in flight, and still closes out the background tasks it started on the way
/// past. Killing the task outright would leave those holding their ports, which
/// is the state the Stop button most needs to avoid.
///
/// Idempotent, and not an error on a finished run — the button can be clicked
/// twice, or clicked just as the last step lands, and neither is worth an error
/// in somebody's face.
async fn stop(State(state): State<AppState>, Path(id): Path<u64>) -> RouteResult<Json<Value>> {
    let run = get_run(&state, id)?;
    let done = serde_json::to_value(&*lock(&run.state))
        .ok()
        .and_then(|v| v["done"].as_bool())
        .unwrap_or(false);

    if !done {
        run.cancel.stop();
    }
    Ok(Json(json!({ "ok": true, "already_finished": done })))
}

/// Delete a finished run and its logs.
///
/// Refused while the run is going: the record is the run, and deleting it out
/// from under the engine would leave a build writing logs into nothing. Stop it
/// first — the button for that is right there.
async fn remove(State(state): State<AppState>, Path(id): Path<u64>) -> RouteResult<Json<Value>> {
    let run = get_run(&state, id)?;
    if !lock(&run.state).done() {
        return Err(RouteError::bad_request(
            "That run is still going. Stop it first, then delete it.",
        ));
    }
    state.runs.remove(id);
    Ok(Json(json!({ "ok": true })))
}

/// How long run history is kept.
async fn settings(State(state): State<AppState>) -> Json<Value> {
    let settings = state.runs.ttl();
    Json(json!({
        "ttl_hours": settings.ttl_hours,
        "default_ttl_hours": crate::daemon::run_store::DEFAULT_TTL_HOURS,
    }))
}

#[derive(Deserialize)]
pub struct SettingsPayload {
    /// Hours to keep a run's record for. Zero keeps them until they are
    /// deleted by hand.
    ttl_hours: u64,
}

async fn set_settings(
    State(state): State<AppState>,
    Json(payload): Json<SettingsPayload>,
) -> RouteResult<Json<Value>> {
    let next = crate::daemon::run_store::Settings {
        ttl_hours: payload.ttl_hours,
    };
    let pruned = state.runs.set_ttl(next)?;
    Ok(Json(
        json!({ "ttl_hours": next.ttl_hours, "pruned": pruned }),
    ))
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
            workflow: payload.workflow,
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

/// Answer a step's `ciabatta input` question.
async fn answer(
    State(state): State<AppState>,
    Path(id): Path<u64>,
    Json(payload): Json<AnswerPayload>,
) -> RouteResult<Json<Value>> {
    let run = get_run(&state, id)?;
    // Checked against the state rather than trusting a send: the bus outlives
    // any one question, so a send "succeeding" says nothing about whether
    // anyone was still waiting for this answer.
    if !lock(&run.state).input_pending(payload.id) {
        return Err(RouteError::bad_request(
            "That question isn't waiting any more — it was answered, timed out, or its step ended.",
        ));
    }
    // A yes-or-no question gets yes or no, or nothing that a script testing
    // `[ "$answer" = y ]` could misread.
    let question = lock(&run.state)
        .pending_inputs()
        .into_iter()
        .map(|(_, input)| input)
        .find(|input| input.id == payload.id);
    if let Some(question) = question
        && question.yes_no
        && crate::run::input::yes_no(&payload.value).is_none()
        && !(payload.value.is_empty() && question.default.is_some())
    {
        return Err(RouteError::bad_request("This question takes yes or no."));
    }
    run.inputs
        .send(crate::run::input::Answer {
            id: payload.id,
            value: payload.value,
        })
        .map_err(|_| RouteError::bad_request("The run stopped listening for answers."))?;
    Ok(Json(json!({ "ok": true })))
}

/// Every question waiting on an answer, across every run — so the web app can
/// put one in front of the operator whichever page they are on.
async fn inputs(State(state): State<AppState>) -> Json<Value> {
    let waiting: Vec<Value> = state
        .runs
        .list()
        .iter()
        .flat_map(|run| {
            let pending = lock(&run.state).pending_inputs();
            pending.into_iter().map(move |(workflow, input)| {
                let mut value = serde_json::to_value(&input).unwrap_or_default();
                value["run"] = json!(run.id);
                value["project"] = json!(run.project);
                value["workflow"] = json!(workflow);
                value
            })
        })
        .collect();
    Json(json!({ "inputs": waiting }))
}

/// The run's view state plus its identity, as one payload.
fn snapshot(run: &Run) -> Value {
    let mut value = serde_json::to_value(&*lock(&run.state)).unwrap_or_else(|_| json!({}));
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

    fn payload(project: &str, env: &[(&str, &str)], terminal: bool) -> CreatePayload {
        serde_json::from_value(serde_json::json!({
            "project": project,
            "env": env.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect::<HashMap<_, _>>(),
            "terminal": terminal,
        }))
        .unwrap()
    }

    /// A variable exported in the terminal that last launched with `--gui`
    /// reaches a run the browser starts in the same checkout, even when that
    /// terminal registered a package rather than the root.
    #[test]
    fn a_browser_run_inherits_the_last_terminals_environment() {
        let runs = Runs::default();
        let root = std::path::Path::new("/checkout");
        let from_terminal =
            runs.environment_for(root, &payload("api", &[("MYVAR", "hello")], true));
        assert_eq!(
            from_terminal.get("MYVAR").map(String::as_str),
            Some("hello")
        );

        let from_browser = runs.environment_for(root, &payload("root", &[], false));
        assert_eq!(from_browser.get("MYVAR").map(String::as_str), Some("hello"));

        // What the browser typed itself still wins, and another checkout is
        // none of that terminal's business.
        let typed = runs.environment_for(root, &payload("root", &[("MYVAR", "typed")], false));
        assert_eq!(typed.get("MYVAR").map(String::as_str), Some("typed"));
        let elsewhere =
            runs.environment_for(std::path::Path::new("/other"), &payload("x", &[], false));
        assert!(!elsewhere.contains_key("MYVAR"));
    }

    /// The `.env` search starts where the terminal was, but never outside the
    /// checkout.
    #[test]
    fn the_env_search_starts_where_the_run_was_typed() {
        let root = std::env::temp_dir().join(format!("ciabatta-startdir-{}", std::process::id()));
        std::fs::create_dir_all(root.join("packages/api")).unwrap();
        assert_eq!(
            start_dir(&root, Some("packages/api")),
            root.join("packages/api")
        );
        assert_eq!(start_dir(&root, None), root);
        assert_eq!(start_dir(&root, Some("../elsewhere")), root);
        assert_eq!(start_dir(&root, Some("packages/missing")), root);
        std::fs::remove_dir_all(&root).ok();
    }

    /// The reason [`lock`] exists: one panic must not turn every later read of
    /// a run into a panic of its own, because the first one is the one worth
    /// reading and the rest are noise on top of it.
    #[test]
    fn a_poisoned_run_lock_is_recovered_rather_than_re_panicking() {
        let value = Arc::new(Mutex::new(7));

        let poisoner = Arc::clone(&value);
        let panicked = std::thread::spawn(move || {
            let _guard = poisoner.lock().unwrap();
            panic!("while holding the lock");
        })
        .join();
        assert!(panicked.is_err(), "the helper thread should have panicked");
        assert!(value.is_poisoned());

        assert_eq!(*lock(&value), 7);
    }
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

    /// A run built straight from a required-variable list, for the gate below.
    fn run_needing(vars: &[&str]) -> crate::run::ResolvedRun {
        crate::run::ResolvedRun {
            required_env: vars.iter().map(|v| v.to_string()).collect(),
            steps: vec![crate::run::RunStep {
                name: "build".into(),
                run: Some("true".into()),
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    #[test]
    fn missing_env_reports_the_union_across_runs_once() {
        let root = std::path::Path::new("/proj");
        let runs = vec![
            ("web".to_string(), run_needing(&["API_TOKEN", "REGION"])),
            ("api".to_string(), run_needing(&["REGION", "STAGE"])),
        ];

        // REGION is required by both but asked for once, in first-seen order.
        let missing = missing_env(root, &runs, &env_map(&[])).unwrap();
        assert_eq!(missing, vec!["API_TOKEN", "REGION", "STAGE"]);

        // Values already in the environment aren't asked for.
        let missing = missing_env(root, &runs, &env_map(&[("REGION", "us-east-1")])).unwrap();
        assert_eq!(missing, vec!["API_TOKEN", "STAGE"]);

        // Everything supplied → nothing to prompt for, so the run may start.
        let missing = missing_env(
            root,
            &runs,
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
    async fn listing_workflows_rejects_an_unknown_project() {
        let app = router(test_state());
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/run/workflows?project=nope")
                    .header(header::AUTHORIZATION, "Bearer test-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
}
