//! The serializable view model for a run.
//!
//! Extracted from the old `--gui` server so the daemon can own it: this is the
//! shape the web app renders, and folding a `ProgressUpdate` into it is the
//! only place run state is interpreted.
//!
//! Everything here is `pub(crate)` rather than private, because the transport
//! now lives in `crate::daemon::routes::run` instead of alongside it.

use anyhow::Result;
use serde::{Deserialize, Serialize};

use std::collections::VecDeque;

use crate::config::CiabattaConfig;
use crate::runner::{ProgressUpdate, StageKind};

use super::envdeps;

/// How many lines of output are kept, per step and per workflow.
///
/// Unbounded before this, which is the state the browser could not survive: the
/// SSE stream sends the whole view model on every change, so a step emitting a
/// hundred thousand lines was sending a hundred thousand snapshots whose size
/// grew with each one. Quadratic in the log length, and the tab stopped
/// responding long before the build finished.
///
/// Five thousand lines is more scrollback than anyone reads in a browser and
/// several times what a failing step's tail needs. The full output is on disk
/// either way — this is the live view, not the record.
const LOG_LIMIT: usize = 5_000;

/// Append a line, dropping the oldest once the buffer is full.
///
/// The count of what was dropped is kept and sent to the viewer, because a log
/// that silently begins in the middle is a log that gets read as the beginning.
fn push_log(logs: &mut VecDeque<String>, dropped: &mut usize, line: String) {
    logs.push_back(line);
    while logs.len() > LOG_LIMIT {
        logs.pop_front();
        *dropped += 1;
    }
}

/// The moment an update was folded in, as the view model records it.
///
/// Stamped here rather than carried on `ProgressUpdate`, because the view is
/// fed live as the engine reports: the gap between the two is a channel hop,
/// and every other consumer of the updates would have to ignore a field only
/// this one reads.
fn now() -> String {
    chrono::Local::now().to_rfc3339()
}

/// A stamp as an instant, so two can be compared across UTC offsets. One that
/// doesn't parse sorts first rather than failing the comparison.
fn parse_time(at: &str) -> Option<chrono::DateTime<chrono::FixedOffset>> {
    chrono::DateTime::parse_from_rfc3339(at).ok()
}

// ─── Serializable live state ────────────────────────────────────────────────

#[derive(Serialize, Deserialize, Clone, Default)]
pub struct GuiState {
    workflows: Vec<WorkflowView>,
    done: bool,
    dry_run: bool,
}

#[derive(Serialize, Deserialize, Clone, Default)]
pub struct WorkflowView {
    name: String,
    status: String,
    error: Option<String>,
    /// The four run phases (login → pre → run → post) with their live
    /// status, so the GUI can show which phase is running and where it stopped.
    stages: Vec<StageView>,
    steps: Vec<StepView>,
    edges: Vec<EdgeView>,
    logs: VecDeque<String>,
    /// Lines dropped off the front of `logs` once it hit [`LOG_LIMIT`].
    dropped_logs: usize,
    pending: Option<PendingChoice>,
    /// Every environment variable this run depends on, with the value its steps
    /// will see and where that value came from. Resolved once, when the run is
    /// created — it is what the run started with, not a live view of the
    /// daemon's environment.
    env: crate::run::envdeps::EnvReport,
    /// When it started and finished, RFC 3339 in local time — so the viewer can
    /// say how long it took and when it ended. Defaulted so a run recorded
    /// before these existed still loads from disk.
    #[serde(default)]
    started_at: Option<String>,
    #[serde(default)]
    finished_at: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Default)]
pub struct StageView {
    name: String,
    /// pending · running · success · skipped · failed
    status: String,
    /// When it started and finished, RFC 3339 in local time — so the viewer can
    /// say how long it took and when it ended. Defaulted so a run recorded
    /// before these existed still loads from disk.
    #[serde(default)]
    started_at: Option<String>,
    #[serde(default)]
    finished_at: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Default)]
pub struct StepView {
    name: String,
    status: String,
    recover: bool,
    action: Option<String>,
    /// The directory this step's action runs from, relative to the run's root —
    /// a sub-workspace's directory in a compiled graph, absent for a step that
    /// runs from the root itself.
    cwd: Option<String>,
    /// The exact command the engine hands to the shell: an inline `run` as
    /// written, or a `script` as the `bash <path>` it becomes.
    ///
    /// Separate from `action`, which is whichever of the two was written and so
    /// can't say which it was. This is what the "recreate" view types out, and
    /// it comes from the engine's own renderer so the two can't drift.
    shell: Option<String>,
    needs: Vec<String>,
    on_error: Option<String>,
    logs: VecDeque<String>,
    /// Lines dropped off the front of `logs` once it hit [`LOG_LIMIT`].
    dropped_logs: usize,

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
    /// What the cache decided about it in this run and why, once it has —
    /// absent for a step the cache never looked at (caching off for the run,
    /// a dry run, a condition that skipped it).
    #[serde(default)]
    cache: Option<crate::run::cached::CacheReport>,
    /// When it started and finished, RFC 3339 in local time — so the viewer can
    /// say how long it took and when it ended. Defaulted so a run recorded
    /// before these existed still loads from disk.
    #[serde(default)]
    started_at: Option<String>,
    #[serde(default)]
    finished_at: Option<String>,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct EdgeView {
    from: String,
    to: String,
    kind: String,
}

#[derive(Serialize, Deserialize, Clone)]
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
                    r.started_at = Some(now());
                }
            }
            ProgressUpdate::Log(name, line) => {
                if let Some(r) = self.recipe_mut(&name) {
                    push_log(&mut r.logs, &mut r.dropped_logs, line);
                }
            }
            ProgressUpdate::StepStarted { workflow, step } => {
                if let Some(r) = self.recipe_mut(&workflow) {
                    // Reaching a step clears any prior pending choice on the workflow.
                    r.pending = None;
                    if let Some(s) = r.step_mut(&step) {
                        s.status = "running".into();
                        // A step can run more than once — a retry routes back
                        // to it — and the time that matters is the latest go.
                        s.started_at = Some(now());
                        s.finished_at = None;
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
                    s.finished_at = Some(now());
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
                        s.finished_at = Some(now());
                        push_log(
                            &mut s.logs,
                            &mut s.dropped_logs,
                            format!("skipped: {reason}"),
                        );
                    }
                    let line = format!("[{step}] skipped: {reason}");
                    push_log(&mut r.logs, &mut r.dropped_logs, line);
                }
            }
            ProgressUpdate::StepCache {
                workflow,
                step,
                report,
            } => {
                if let Some(r) = self.recipe_mut(&workflow)
                    && let Some(s) = r.step_mut(&step)
                {
                    s.cache = Some(*report);
                }
            }
            ProgressUpdate::StepLog {
                workflow,
                step,
                line,
            } => {
                if let Some(r) = self.recipe_mut(&workflow) {
                    if let Some(s) = r.step_mut(&step) {
                        push_log(&mut s.logs, &mut s.dropped_logs, line.clone());
                    }
                    let line = format!("[{step}] {line}");
                    push_log(&mut r.logs, &mut r.dropped_logs, line);
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
                    r.finished_at = Some(now());
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
                    let at = now();
                    r.finished_at = Some(at.clone());
                    let line = if stopped {
                        format!("■ {err}")
                    } else {
                        format!("✗ {err}")
                    };
                    push_log(&mut r.logs, &mut r.dropped_logs, line);
                    // Pin the blame on whichever stage was mid-flight, and mark
                    // any later stages as not reached.
                    let mut hit = false;
                    for st in &mut r.stages {
                        if st.status == "running" {
                            st.status = outcome.into();
                            st.finished_at = Some(at.clone());
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
                    s.started_at = Some(now());
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
                    s.finished_at = Some(now());
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

impl GuiState {
    /// Whether every workflow has reached a terminal state.
    pub fn done(&self) -> bool {
        self.done
    }

    /// Mark a run nobody will hear from again as stopped.
    ///
    /// The engine reports every ending it knows about, but it can't report the
    /// one where the daemon itself goes away: the run's processes died with it,
    /// and the record left behind still says "running". Left alone, that record
    /// comes back from disk with a Stop button on a run that stopped when the
    /// machine rebooted.
    pub fn interrupted(&mut self, reason: &str) {
        for workflow in &mut self.workflows {
            if matches!(workflow.status.as_str(), "success" | "failed" | "stopped") {
                continue;
            }
            workflow.status = "stopped".into();
            workflow.error = Some(reason.to_string());
            workflow.pending = None;
            for step in &mut workflow.steps {
                if step.status == "running" {
                    step.status = "stopped".into();
                }
            }
            for stage in &mut workflow.stages {
                if stage.status == "running" {
                    stage.status = "stopped".into();
                }
            }
        }
        self.done = true;
    }

    /// How far along the run is: steps finished, steps in all, and the ones
    /// running right now — what an editor's progress indicator can say about
    /// a run without being sent its logs.
    ///
    /// Recovery steps count only once they've been entered: they run when
    /// something fails, and a success that never touches them is not "7 of 9".
    pub fn progress(&self) -> (usize, usize, Vec<String>) {
        let steps = self
            .workflows
            .iter()
            .flat_map(|w| &w.steps)
            .filter(|s| !s.recover || s.status != "pending");
        let (mut done, mut total, mut running) = (0, 0, Vec::new());
        for step in steps {
            total += 1;
            match step.status.as_str() {
                "pending" => {}
                "running" => running.push(step.name.clone()),
                _ => done += 1,
            }
        }
        (done, total, running)
    }

    /// When the run started: its first workflow to start. None until one has.
    pub fn started_at(&self) -> Option<&str> {
        self.workflows
            .iter()
            .filter_map(|w| w.started_at.as_deref())
            .min_by_key(|at| parse_time(at))
    }

    /// When the run finished: its last workflow to finish — and None while any
    /// of them is still going, or ended without a time (the daemon went away
    /// under it), since the run's end is then unknown.
    pub fn finished_at(&self) -> Option<&str> {
        if !self.done {
            return None;
        }
        let ends: Option<Vec<&str>> = self
            .workflows
            .iter()
            .map(|w| w.finished_at.as_deref())
            .collect();
        ends?.into_iter().max_by_key(|at| parse_time(at))
    }

    /// The run's verdict in one word: `running` until every workflow has
    /// finished, then the worst thing that happened to any of them.
    ///
    /// The list page shows a run as an icon, and "done" was never the
    /// interesting half of that — a finished run and a failed one looked
    /// identical until you opened them.
    pub fn outcome(&self) -> &'static str {
        if !self.done {
            return "running";
        }
        if self.workflows.iter().any(|w| w.status == "failed") {
            return "failed";
        }
        if self.workflows.iter().any(|w| w.status == "stopped") {
            return "stopped";
        }
        "success"
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
                cwd: step.cwd.clone(),
                shell: crate::run::engine::shell_form(step.script.as_deref(), step.run.as_deref()),
                needs: step.needs.clone(),
                on_error: step.on_error.clone(),
                logs: VecDeque::new(),
                dropped_logs: 0,
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
                cache: None,
                started_at: None,
                finished_at: None,
            });
        }

        let stages = StageKind::ALL
            .iter()
            .map(|s| StageView {
                name: s.label().to_string(),
                status: "pending".into(),
                started_at: None,
                finished_at: None,
            })
            .collect();

        workflows.push(WorkflowView {
            name: name.clone(),
            status: "pending".into(),
            error: None,
            stages,
            steps,
            edges,
            logs: VecDeque::new(),
            dropped_logs: 0,
            pending: None,
            env: envdeps::collect(&resolved, root, env, Some(config)),
            started_at: None,
            finished_at: None,
        });
    }
    Ok(GuiState {
        workflows,
        done: false,
        dry_run,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_log_buffer_keeps_the_tail_and_counts_what_it_dropped() {
        let mut logs = VecDeque::new();
        let mut dropped = 0;

        for n in 0..LOG_LIMIT + 250 {
            push_log(&mut logs, &mut dropped, format!("line {n}"));
        }

        assert_eq!(logs.len(), LOG_LIMIT);
        assert_eq!(dropped, 250);
        // The tail is what survives — the end of a build log is the half that
        // says what went wrong.
        assert_eq!(logs.back().unwrap(), &format!("line {}", LOG_LIMIT + 249));
        assert_eq!(logs.front().unwrap(), "line 250");
    }

    #[test]
    fn a_log_under_the_limit_is_untouched_and_reports_nothing_dropped() {
        let mut logs = VecDeque::new();
        let mut dropped = 0;
        push_log(&mut logs, &mut dropped, "only line".into());

        assert_eq!(logs.len(), 1);
        assert_eq!(dropped, 0);
    }

    fn one_step_run() -> GuiState {
        GuiState {
            workflows: vec![WorkflowView {
                name: "build".into(),
                status: "pending".into(),
                stages: StageKind::ALL
                    .iter()
                    .map(|s| StageView {
                        name: s.label().into(),
                        status: "pending".into(),
                        ..Default::default()
                    })
                    .collect(),
                steps: vec![StepView {
                    name: "compile".into(),
                    status: "pending".into(),
                    ..Default::default()
                }],
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    #[test]
    fn a_run_records_when_each_phase_and_step_started_and_finished() {
        let mut state = one_step_run();
        let workflow = || "build".to_string();
        state.apply(ProgressUpdate::Started(workflow()));
        state.apply(ProgressUpdate::StageStarted {
            workflow: workflow(),
            stage: StageKind::Main,
        });
        state.apply(ProgressUpdate::StepStarted {
            workflow: workflow(),
            step: "compile".into(),
        });

        let w = &state.workflows[0];
        assert!(w.started_at.is_some() && w.finished_at.is_none());
        assert!(w.steps[0].started_at.is_some() && w.steps[0].finished_at.is_none());
        let main = &w.stages[StageKind::Main.index()];
        assert!(main.started_at.is_some() && main.finished_at.is_none());
        // A phase that hasn't been reached has no times at all.
        assert!(w.stages[StageKind::Post.index()].started_at.is_none());

        state.apply(ProgressUpdate::StepFinished {
            workflow: workflow(),
            step: "compile".into(),
            ok: true,
        });
        state.apply(ProgressUpdate::StageFinished {
            workflow: workflow(),
            stage: StageKind::Main,
            ran: true,
        });
        state.apply(ProgressUpdate::Completed(workflow()));

        let w = &state.workflows[0];
        assert!(w.finished_at.is_some());
        assert!(w.steps[0].finished_at.is_some());
        assert!(w.stages[StageKind::Main.index()].finished_at.is_some());
    }

    #[test]
    fn a_failed_run_closes_the_phase_it_failed_in() {
        let mut state = one_step_run();
        state.apply(ProgressUpdate::Started("build".into()));
        state.apply(ProgressUpdate::StageStarted {
            workflow: "build".into(),
            stage: StageKind::Pre,
        });
        state.apply(ProgressUpdate::Failed("build".into(), "boom".into()));

        let w = &state.workflows[0];
        assert!(w.finished_at.is_some());
        assert!(w.stages[StageKind::Pre.index()].finished_at.is_some());
        assert!(w.stages[StageKind::Main.index()].finished_at.is_none());
    }

    #[test]
    fn a_run_recorded_before_timestamps_still_loads() {
        let mut json = serde_json::to_value(one_step_run()).unwrap();
        let workflow = &mut json["workflows"][0];
        workflow.as_object_mut().unwrap().remove("started_at");
        workflow["steps"][0]
            .as_object_mut()
            .unwrap()
            .remove("started_at");
        workflow["stages"][0]
            .as_object_mut()
            .unwrap()
            .remove("finished_at");

        let state: GuiState = serde_json::from_value(json).expect("old records deserialize");
        assert!(state.workflows[0].started_at.is_none());
    }

    #[test]
    fn progress_counts_finished_steps_and_names_running_ones() {
        let mut state = one_step_run();
        let mut more = state.workflows[0].steps[0].clone();
        more.name = "link".into();
        let mut recovery = more.clone();
        recovery.name = "fix".into();
        recovery.recover = true;
        state.workflows[0].steps.extend([more, recovery]);
        assert_eq!(state.progress(), (0, 2, vec![]));

        state.apply(ProgressUpdate::StepStarted {
            workflow: "build".into(),
            step: "compile".into(),
        });
        assert_eq!(state.progress(), (0, 2, vec!["compile".to_string()]));

        state.apply(ProgressUpdate::StepFinished {
            workflow: "build".into(),
            step: "compile".into(),
            ok: false,
        });
        // The recovery branch joins the count once the run goes into it.
        state.apply(ProgressUpdate::StepStarted {
            workflow: "build".into(),
            step: "fix".into(),
        });
        assert_eq!(state.progress(), (1, 3, vec!["fix".to_string()]));
    }

    #[test]
    fn a_run_spans_its_first_start_to_its_last_finish() {
        let mut state = one_step_run();
        let mut second = state.workflows[0].clone();
        second.name = "test".into();
        state.workflows.push(second);
        state.workflows[0].started_at = Some("2026-09-27T10:00:05-04:00".into());
        state.workflows[1].started_at = Some("2026-09-27T14:00:00+00:00".into());
        assert_eq!(state.started_at(), Some("2026-09-27T14:00:00+00:00"));
        // Not finished until every workflow has.
        assert_eq!(state.finished_at(), None);

        state.workflows[0].finished_at = Some("2026-09-27T10:01:00-04:00".into());
        state.workflows[1].finished_at = Some("2026-09-27T14:00:30+00:00".into());
        state.done = true;
        assert_eq!(state.finished_at(), Some("2026-09-27T10:01:00-04:00"));

        // A run the daemon lost track of has no known end.
        state.workflows[1].finished_at = None;
        assert_eq!(state.finished_at(), None);
    }
}
