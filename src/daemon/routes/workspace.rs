//! Workspace routes: the monorepo's catalogue and its workflow graphs, as JSON.
//!
//! Everything `ciabatta list` and `ciabatta <workflow> --graph` print in a
//! terminal is served here for the web app, from exactly the same compiler —
//! so the browser's picture of the monorepo can't drift from the CLI's.
//!
//! Nothing here executes anything. Starting a workflow is a run, and runs are
//! `POST /api/run/runs` with a `workflow` field (see [`super::run`]).

use axum::extract::{Query, State};
use axum::routing::get;
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::daemon::app::AppState;
use crate::workspace::{self, Workspace, graph::Selection};

use super::{RouteError, RouteResult};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/workspace", get(catalogue))
        .route("/api/workspace/graph", get(graph))
        .route("/api/workspace/env-drift", get(env_drift))
}

/// Which `.env` files have changed since ciabatta last ran here.
///
/// A peek, never a check: the web app reporting drift must not acknowledge it
/// on the terminal's behalf (see [`crate::environment::cache::peek`]).
async fn env_drift(
    State(state): State<AppState>,
    Query(query): Query<ProjectQuery>,
) -> RouteResult<Json<Value>> {
    let root = state.project_root(&query.project)?;

    // Every `.env` any run in this project sources — the same set the CLI
    // snapshots, gathered from the workspace's workflows and the local workflows.
    let mut files: Vec<String> = Vec::new();
    if let Ok(workspace) = Workspace::discover(&root) {
        for member in &workspace.members {
            for file in member
                .meta
                .env_file
                .iter()
                .chain(member.workflows.values().flat_map(|w| w.env_file.iter()))
            {
                let path = if member.rel == "." {
                    file.clone()
                } else {
                    format!("{}/{}", member.rel.trim_end_matches('/'), file)
                };
                if !files.contains(&path) {
                    files.push(path);
                }
            }
        }
    }
    if let Ok(config) = crate::config::load_config(&root) {
        for workflow in config.workflows.values() {
            for file in &workflow.env_file {
                if !files.contains(file) {
                    files.push(file.clone());
                }
            }
        }
    }

    let drift = crate::environment::cache::peek(&root, &files);
    let mut affected: Vec<&str> = drift.changes.iter().map(|c| c.file()).collect();
    affected.sort();
    affected.dedup();

    Ok(Json(json!({
        "watching": files,
        "first_run": drift.first_run,
        "noteworthy": drift.is_noteworthy(),
        "files": affected,
        "changes": drift.changes,
        "summary": drift.report(),
        "checked_at": crate::environment::cache::load(&root).and_then(|s| s.taken_at),
    })))
}

#[derive(Deserialize)]
pub struct ProjectQuery {
    project: String,
}

#[derive(Deserialize)]
pub struct GraphQuery {
    project: String,
    /// The workflow to compile.
    workflow: String,
    /// Comma-separated sub-workspace names to start from.
    #[serde(default)]
    only: String,
    #[serde(default)]
    isolated: bool,
}

/// Every sub-workspace, its workflows, and who owns them.
///
/// Searching and filtering happen in the browser: the whole catalogue is small
/// (it's declarations, not scan results), and a client-side filter stays
/// responsive as you type.
async fn catalogue(
    State(state): State<AppState>,
    Query(query): Query<ProjectQuery>,
) -> RouteResult<Json<Value>> {
    let root = state.project_root(&query.project)?;
    let workspace = Workspace::discover(&root).map_err(RouteError::bad_request)?;

    let members: Vec<Value> = workspace
        .members
        .iter()
        .map(|member| {
            let workflows: Vec<Value> = member
                .workflows
                .iter()
                .map(|(name, workflow)| {
                    json!({
                        "name": name,
                        "description": workflow.description,
                        "owner": workflow.owner.as_deref().unwrap_or_else(|| member.owner()),
                        "needs": workflow.needs,
                        "requires": workflow.requires,
                        "tags": workflow.tags,
                        "required_env": workflow.required_env,
                        "steps": workflow.steps.iter().map(step_json).collect::<Vec<_>>(),
                    })
                })
                .collect();
            json!({
                "name": member.name,
                "path": member.rel,
                "dir": member.dir.to_string_lossy(),
                "description": member.meta.description,
                "owner": member.owner(),
                "tags": member.meta.tags,
                "depends_on": member.meta.depends_on,
                "workflows": workflows,
            })
        })
        .collect();

    Ok(Json(json!({
        "root": workspace.root.to_string_lossy(),
        "members": members,
        "workflows": workspace.workflow_names(),
        "toolchain": workspace
            .toolchain
            .iter()
            .map(|(tool, spec)| json!({
                "tool": tool,
                "hint": spec.hint,
                "description": spec.description,
            }))
            .collect::<Vec<_>>(),
    })))
}

/// One workflow compiled into nodes, edges, and dependency waves — the shape a
/// flowchart view can draw directly.
async fn graph(
    State(state): State<AppState>,
    Query(query): Query<GraphQuery>,
) -> RouteResult<Json<Value>> {
    let root = state.project_root(&query.project)?;
    let selection = Selection {
        only: query
            .only
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect(),
        isolated: query.isolated,
    };

    let (workspace, compiled) = workspace::graph::prepare(&root, &query.workflow, &selection)
        .map_err(RouteError::bad_request)?;

    // Wave membership is what makes a graph legible, so it's computed here
    // rather than left for the client to re-derive.
    let mut wave_of: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for (index, wave) in compiled.waves().iter().enumerate() {
        for step in wave {
            wave_of.insert(step.name.as_str(), index);
        }
    }

    let nodes: Vec<Value> = compiled
        .steps
        .iter()
        .map(|step| {
            let mut value = step_json(step);
            value["id"] = json!(step.name);
            value["workspace"] = json!(step.workspace);
            value["cwd"] = json!(step.cwd);
            value["wave"] = json!(wave_of.get(step.name.as_str()));
            value
        })
        .collect();

    // Two edge kinds, because they mean different things on a flowchart: one is
    // "this runs after that", the other is "this is where a failure goes".
    let mut edges: Vec<Value> = Vec::new();
    for step in &compiled.steps {
        for dep in &step.needs {
            edges.push(json!({ "from": dep, "to": step.name, "kind": "needs" }));
        }
        if let Some(target) = step.on_error.as_deref() {
            edges.push(json!({ "from": step.name, "to": target, "kind": "on_error" }));
        }
    }

    let missing = workspace::missing_tools(&workspace, &compiled.steps);

    // What the graph depends on besides its steps: the variables it declares,
    // sources, and reads, resolved against the daemon's own environment — the
    // same environment a run started from here would get.
    let env = crate::run::envdeps::collect(
        &crate::run::ResolvedRun {
            required_env: compiled.required_env.clone(),
            env_files: compiled.env_files.clone(),
            steps: compiled.steps.clone(),
            ..Default::default()
        },
        &workspace.root,
        &std::env::vars().collect(),
    );

    Ok(Json(json!({
        "workflow": compiled.label(),
        "root": workspace.root.to_string_lossy(),
        "empty": compiled.is_empty(),
        "units": compiled
            .units
            .iter()
            .map(|unit| json!({
                "member": unit.member,
                "workflow": unit.workflow,
                "steps": compiled.steps_of(&unit.member).len(),
            }))
            .collect::<Vec<_>>(),
        "nodes": nodes,
        "edges": edges,
        "waves": compiled
            .waves()
            .iter()
            .map(|wave| wave.iter().map(|s| s.name.clone()).collect::<Vec<_>>())
            .collect::<Vec<_>>(),
        "missing_tools": missing
            .iter()
            .map(|tool| json!({
                "tool": tool.tool,
                "hint": tool.hint,
                "needed_by": tool.needed_by,
            }))
            .collect::<Vec<_>>(),
        "env": env,
    })))
}

/// One step, described the way the catalogue and the graph both want it.
///
/// `env` is what the step sets for itself and `env_refs` what it reads: a step
/// depends on its variables every bit as much as on the steps before it, and
/// the graph draws both.
fn step_json(step: &crate::run::RunStep) -> Value {
    let env: serde_json::Map<String, Value> = step
        .env
        .iter()
        .map(|(key, value)| (key.clone(), json!(crate::run::envdeps::shown(key, value))))
        .collect();
    json!({
        "name": step.name,
        "description": step.description,
        "owner": step.owner,
        "command": step.run.clone().or_else(|| step.script.clone()),
        "kind": step.kind,
        "registry": step.registry,
        "artifact": step.artifact,
        "publish_path": step.publish_path,
        "push": step.is_push(),
        "needs": step.needs,
        "requires": step.requires,
        "timeout": step.timeout,
        "retries": step.retries,
        "persistent": step.persistent,
        "background": step.background,
        "continue_on_error": step.continue_on_error,
        "recover": step.recover,
        "on_error": step.on_error,
        "env": env,
        "env_refs": crate::run::envdeps::step_refs(step),
    })
}
