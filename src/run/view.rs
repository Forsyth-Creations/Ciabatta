//! The serializable view model for a run.
//!
//! Extracted from the old `--gui` server so the daemon can own it: this is the
//! shape the web app renders, and folding a `ProgressUpdate` into it is the
//! only place run state is interpreted.
//!
//! Everything here is `pub(crate)` rather than private, because the transport
//! now lives in `crate::daemon::routes::run` instead of alongside it.

use anyhow::Result;
use serde::Serialize;

use crate::config::CiabattaConfig;
use crate::runner::{ProgressUpdate, StageKind};

use super::envdeps;

// ─── Serializable live state ────────────────────────────────────────────────

#[derive(Serialize, Clone, Default)]
pub struct GuiState {
    workflows: Vec<WorkflowView>,
    done: bool,
    dry_run: bool,
}

#[derive(Serialize, Clone)]
pub struct WorkflowView {
    name: String,
    status: String,
    error: Option<String>,
    /// The four run phases (login → pre → run → post) with their live
    /// status, so the GUI can show which phase is running and where it stopped.
    stages: Vec<StageView>,
    steps: Vec<StepView>,
    edges: Vec<EdgeView>,
    logs: Vec<String>,
    pending: Option<PendingChoice>,
    /// Every environment variable this run depends on, with the value its steps
    /// will see and where that value came from. Resolved once, when the run is
    /// created — it is what the run started with, not a live view of the
    /// daemon's environment.
    env: crate::run::envdeps::EnvReport,
}

#[derive(Serialize, Clone)]
pub struct StageView {
    name: String,
    /// pending · running · success · skipped · failed
    status: String,
}

#[derive(Serialize, Clone)]
pub struct StepView {
    name: String,
    status: String,
    recover: bool,
    action: Option<String>,
    needs: Vec<String>,
    on_error: Option<String>,
    logs: Vec<String>,

    // ─── Provenance and behaviour ───────────────────────────────────────────
    // A workflow graph draws nodes from several sub-workspaces at once, so a
    // node has to say where it came from and how it behaves — otherwise a
    // failing "compile" in a six-package build names no package at all.
    /// The sub-workspace this node came from, when the run is a workflow graph.
    workspace: Option<String>,
    /// One line on what the step does.
    description: Option<String>,
    /// Who to ask about it.
    owner: Option<String>,
    /// Its phase label (`push`, `setup`, `deploy`, …).
    kind: Option<String>,
    /// Whether it publishes — the special, identifiable push phase.
    push: bool,
    /// Whether it is started and left running rather than waited for.
    persistent: bool,
    /// From a workflow's `background:` array: started before the first wave,
    /// gates nothing, stopped when the run ends.
    background: bool,
    /// Its wall-clock limit, as written.
    timeout: Option<String>,
    /// Tools it needs on `PATH`.
    requires: Vec<String>,

    // ─── Environment ────────────────────────────────────────────────────────
    /// The variables this step's own `[env]` table sets, layered over the
    /// run's. A compiled workflow graph folds its sub-workspace's and
    /// workflow's tables in here too.
    env: std::collections::BTreeMap<String, String>,
    /// The variables this step reads — from its command, working directory and
    /// conditions. Together with `env` these are the edges the graph view
    /// draws between a variable and the steps that depend on it.
    env_refs: Vec<String>,
    /// The `.env` files this step resolves through, outermost first — its own
    /// workspace's last, since that's the one that wins. Empty for a step that
    /// just sees the run's environment.
    ///
    /// Worth showing because "which `.env` did this value come from?" is
    /// otherwise unanswerable in a monorepo, where two packages can set the
    /// same variable and each step sees its own.
    env_files: Vec<String>,

    // ─── Dependencies ───────────────────────────────────────────────────────
    /// The five things this target is defined by: the files it reads, the files
    /// it writes, the variables it keys on, the commands it runs, and the
    /// targets it needs.
    ///
    /// The graph already showed the last of those. The other four were only
    /// ever visible by opening the config, which is precisely when somebody is
    /// asking why a step rebuilt — so the answer belongs next to the step.
    deps: crate::run::deps::TargetDeps,
}

#[derive(Serialize, Clone)]
pub struct EdgeView {
    from: String,
    to: String,
    kind: String,
}

#[derive(Serialize, Clone)]
pub struct PendingChoice {
    step: String,
    message: String,
    options: Vec<String>,
}

impl WorkflowView {
    fn step_mut(&mut self, name: &str) -> Option<&mut StepView> {
        self.steps.iter_mut().find(|s| s.name == name)
    }
}

impl GuiState {
    fn recipe_mut(&mut self, name: &str) -> Option<&mut WorkflowView> {
        self.workflows.iter_mut().find(|r| r.name == name)
    }

    /// Fold one progress update into the live state.
    pub fn apply(&mut self, update: ProgressUpdate) {
        match update {
            ProgressUpdate::Started(name) => {
                if let Some(r) = self.recipe_mut(&name) {
                    r.status = "running".into();
                }
            }
            ProgressUpdate::Log(name, line) => {
                if let Some(r) = self.recipe_mut(&name) {
                    r.logs.push(line);
                }
            }
            ProgressUpdate::StepStarted { workflow, step } => {
                if let Some(r) = self.recipe_mut(&workflow) {
                    // Reaching a step clears any prior pending choice on the workflow.
                    r.pending = None;
                    if let Some(s) = r.step_mut(&step) {
                        s.status = "running".into();
                    }
                }
            }
            ProgressUpdate::StepFinished { workflow, step, ok } => {
                if let Some(r) = self.recipe_mut(&workflow)
                    && let Some(s) = r.step_mut(&step)
                {
                    s.status = if ok {
                        "success".into()
                    } else {
                        "failed".into()
                    };
                }
            }
            ProgressUpdate::StepSkipped {
                workflow,
                step,
                reason,
            } => {
                if let Some(r) = self.recipe_mut(&workflow) {
                    if let Some(s) = r.step_mut(&step) {
                        s.status = "skipped".into();
                        s.logs.push(format!("skipped: {reason}"));
                    }
                    r.logs.push(format!("[{step}] skipped: {reason}"));
                }
            }
            ProgressUpdate::StepLog {
                workflow,
                step,
                line,
            } => {
                if let Some(r) = self.recipe_mut(&workflow) {
                    if let Some(s) = r.step_mut(&step) {
                        s.logs.push(line.clone());
                    }
                    r.logs.push(format!("[{step}] {line}"));
                }
            }
            ProgressUpdate::StepNeedsChoice {
                workflow,
                step,
                message,
                options,
            } => {
                if let Some(r) = self.recipe_mut(&workflow) {
                    r.pending = Some(PendingChoice {
                        step,
                        message,
                        options,
                    });
                }
            }
            ProgressUpdate::Completed(name) => {
                if let Some(r) = self.recipe_mut(&name) {
                    r.status = "success".into();
                    r.pending = None;
                }
            }
            ProgressUpdate::Failed(name, err) => {
                // A run somebody stopped is not a run that failed. It arrives
                // on the same channel because it is still an unsuccessful end,
                // but reporting it as a failure sends the next person looking
                // for a bug that isn't there.
                let stopped = err == crate::runner::STOPPED_MESSAGE;
                let outcome = if stopped { "stopped" } else { "failed" };
                if let Some(r) = self.recipe_mut(&name) {
                    r.status = outcome.into();
                    r.error = Some(err.clone());
                    r.pending = None;
                    r.logs.push(if stopped {
                        format!("■ {err}")
                    } else {
                        format!("✗ {err}")
                    });
                    // Pin the blame on whichever stage was mid-flight, and mark
                    // any later stages as not reached.
                    let mut hit = false;
                    for st in &mut r.stages {
                        if st.status == "running" {
                            st.status = outcome.into();
                            hit = true;
                        } else if hit && st.status == "pending" {
                            st.status = "skipped".into();
                        }
                    }
                }
            }
            ProgressUpdate::StageStarted { workflow, stage } => {
                let label = stage.label();
                if let Some(r) = self.recipe_mut(&workflow)
                    && let Some(s) = r.stages.iter_mut().find(|s| s.name == label)
                {
                    s.status = "running".into();
                }
            }
            ProgressUpdate::StageFinished {
                workflow,
                stage,
                ran,
            } => {
                let label = stage.label();
                if let Some(r) = self.recipe_mut(&workflow)
                    && let Some(s) = r.stages.iter_mut().find(|s| s.name == label)
                    // A stage that already failed stays failed.
                    && s.status != "failed"
                {
                    s.status = if ran {
                        "success".into()
                    } else {
                        "skipped".into()
                    };
                }
            }
            // Runs don't emit stage-file-transfer progress.
            ProgressUpdate::TransferProgress { .. } => {}
        }
        // Every terminal status, "stopped" included — a stopped run that never
        // reported itself done would leave the page saying "running" with a
        // Stop button on a run that had already stopped.
        self.done = self
            .workflows
            .iter()
            .all(|r| matches!(r.status.as_str(), "success" | "failed" | "stopped"));
    }
}

/// Build the initial live state (all steps pending) from the resolved runs.
///
/// `env` is what the run will start with — the daemon's own environment plus
/// whatever the caller supplied — so the view can say which variables each step
/// depends on and what they resolve to, the same list the terminal prints.
pub fn initial_state(
    config: &CiabattaConfig,
    root: &std::path::Path,
    runs: &[(String, crate::run::ResolvedRun)],
    dry_run: bool,
    env: &std::collections::HashMap<String, String>,
) -> Result<GuiState> {
    let mut workflows = Vec::new();
    for (name, resolved) in runs {
        let resolved = resolved.clone();

        // One walk for the whole graph, keyed by step name: every node's
        // inputs, outputs, declared variables and commands, resolved through
        // the same settings the cache itself uses.
        let mut deps: std::collections::HashMap<String, crate::run::deps::TargetDeps> =
            crate::run::deps::collect(config, root, &resolved.steps)
                .into_iter()
                .map(|target| (target.name.clone(), target))
                .collect();

        let mut steps = Vec::new();
        let mut edges = Vec::new();
        for step in &resolved.steps {
            for dep in &step.needs {
                edges.push(EdgeView {
                    from: dep.clone(),
                    to: step.name.clone(),
                    kind: "needs".into(),
                });
            }
            if let Some(t) = step.on_error.as_deref() {
                edges.push(EdgeView {
                    from: step.name.clone(),
                    to: t.to_string(),
                    kind: "error".into(),
                });
            }
            if let Some(t) = step.retry.as_deref() {
                edges.push(EdgeView {
                    from: step.name.clone(),
                    to: t.to_string(),
                    kind: "retry".into(),
                });
            }
            steps.push(StepView {
                name: step.name.clone(),
                status: "pending".into(),
                recover: step.recover,
                action: step.script.clone().or_else(|| step.run.clone()),
                needs: step.needs.clone(),
                on_error: step.on_error.clone(),
                logs: Vec::new(),
                workspace: step.workspace.clone(),
                description: step.description.clone(),
                owner: step.owner.clone(),
                kind: step.kind.clone(),
                push: step.is_push(),
                persistent: step.persistent,
                background: step.background,
                timeout: step.timeout.clone(),
                requires: step.requires.clone(),
                env: step
                    .env
                    .iter()
                    .map(|(key, value)| (key.clone(), envdeps::shown(key, value)))
                    .collect(),
                env_refs: envdeps::step_refs(step),
                env_files: step.env_files.clone(),
                // A recovery node has no build and so no dependencies; the
                // default is the honest empty answer rather than a missing key
                // the viewer would have to special-case.
                deps: deps.remove(&step.name).unwrap_or_default(),
            });
        }

        let stages = StageKind::ALL
            .iter()
            .map(|s| StageView {
                name: s.label().to_string(),
                status: "pending".into(),
            })
            .collect();

        workflows.push(WorkflowView {
            name: name.clone(),
            status: "pending".into(),
            error: None,
            stages,
            steps,
            edges,
            logs: Vec::new(),
            pending: None,
            env: envdeps::collect(&resolved, root, env),
        });
    }
    Ok(GuiState {
        workflows,
        done: false,
        dry_run,
    })
}
