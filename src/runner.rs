use anyhow::Result;
use std::collections::HashMap;
use std::path::Path;
use tokio::sync::mpsc;

use crate::config::CiabattaConfig;

/// The four ordered stages of a push or pull pipeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StageKind {
    Login,
    Pre,
    Main,
    Post,
}

impl StageKind {
    /// All stages in execution order.
    pub const ALL: [StageKind; 4] = [
        StageKind::Login,
        StageKind::Pre,
        StageKind::Main,
        StageKind::Post,
    ];

    /// Position in the pipeline (0..4).
    pub fn index(self) -> usize {
        match self {
            StageKind::Login => 0,
            StageKind::Pre => 1,
            StageKind::Main => 2,
            StageKind::Post => 3,
        }
    }

    /// Full label for this phase, e.g. "pre-run".
    pub fn label(self) -> &'static str {
        match self {
            StageKind::Login => "login",
            StageKind::Pre => "pre-run",
            StageKind::Main => "run",
            StageKind::Post => "post-run",
        }
    }

    /// Compact label for cramped UI.
    pub fn short(self) -> &'static str {
        match self {
            StageKind::Login => "login",
            StageKind::Pre => "pre",
            StageKind::Main => "run",
            StageKind::Post => "post",
        }
    }
}

#[derive(Debug, Clone)]
pub enum ProgressUpdate {
    Started(String),
    StageStarted {
        workflow: String,
        stage: StageKind,
    },
    /// A stage finished successfully. `ran` is false when it fell through to a
    /// default no-op (nothing to do), true when it actually executed something.
    StageFinished {
        workflow: String,
        stage: StageKind,
        ran: bool,
    },
    /// Progress within a multi-file main stage: `done` of `total` files have
    /// been transferred. Only emitted when a workflow transfers more than one file
    /// (a list-form `publish_path`).
    TransferProgress {
        workflow: String,
        done: usize,
        total: usize,
    },
    Log(String, String),
    /// A run step (DAG node) started running its action.
    StepStarted {
        workflow: String,
        step: String,
    },
    /// A run step finished. `ok` is false when the action failed (it may then
    /// be routed to a recovery node).
    StepFinished {
        workflow: String,
        step: String,
        ok: bool,
    },
    /// A run step was skipped because its `when`/`skip_if` condition said so.
    /// It counts as satisfied, so its dependents still run. `reason` is a short
    /// explanation (e.g. ``skip_if `env.IN_CI == true` ``).
    StepSkipped {
        workflow: String,
        step: String,
        reason: String,
    },
    /// A log line produced by a specific run step's action.
    StepLog {
        workflow: String,
        step: String,
        line: String,
    },
    /// A recovery node is waiting for the operator to pick a fix option. The UI
    /// replies with a [`StepChoice`] over the run control channel. `options`
    /// carries the option labels in order.
    StepNeedsChoice {
        workflow: String,
        step: String,
        message: String,
        options: Vec<String>,
    },
    Completed(String),
    Failed(String, String),
}

/// A recovery decision sent from the UI (TUI / `--gui`) back to the run
/// engine: run option `option` of recovery node `step` for workflow `workflow`.
#[derive(Debug, Clone)]
pub struct StepChoice {
    pub workflow: String,
    pub step: String,
    pub option: usize,
}

/// What a run reports when it was stopped rather than failed.
///
/// A constant rather than a string written twice, because the view layer has to
/// recognise exactly what the engine produced in order to tell "somebody
/// pressed Stop" from "the build is broken" — and two copies of a sentence are
/// two things to keep in step.
pub const STOPPED_MESSAGE: &str = "Run stopped.";

/// A run's stop switch: shared with whoever might ask it to stop, and checked
/// by the engine between steps and while one is in flight.
///
/// A flag rather than dropping the run's future outright, because a run that is
/// merely killed leaves whatever it started behind — the background tasks it
/// launched, most of all. Asking gives the engine the chance to unwind: stop
/// scheduling, kill the step that is running, and still close out everything it
/// owns on the way past.
#[derive(Default, Debug)]
pub struct Cancel {
    stopped: std::sync::atomic::AtomicBool,
    notify: tokio::sync::Notify,
}

impl Cancel {
    /// Ask the run to stop. Idempotent: stopping a stopped run is not an error,
    /// which matters when the button can be clicked twice.
    pub fn stop(&self) {
        self.stopped
            .store(true, std::sync::atomic::Ordering::SeqCst);
        self.notify.notify_waiters();
    }

    /// Whether a stop has been asked for.
    pub fn is_stopped(&self) -> bool {
        self.stopped.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Resolves once a stop has been asked for, for racing against a step.
    ///
    /// The waiter is registered *before* the flag is re-read, so a stop landing
    /// between the two isn't missed — the opposite order is a run that ignores
    /// the button until its current step happens to finish.
    pub async fn stopped(&self) {
        loop {
            let waiting = self.notify.notified();
            if self.is_stopped() {
                return;
            }
            waiting.await;
        }
    }
}

/// The policy a run is executed under: how it resolves recovery choices, what
/// happens to work that outlives it, and how strictly it holds steps to what
/// they declared. Push/pull ignore this entirely.
#[derive(Clone)]
pub struct RunCtl {
    /// When true, recovery nodes wait for a UI choice on `choices`; when false
    /// (plain / CI) the engine auto-picks the first `default` option or fails.
    pub interactive: bool,
    /// Broadcast bus the UI publishes [`StepChoice`]s on. Each workflow engine
    /// subscribes and filters for its own workflow + step.
    pub choices: Option<tokio::sync::broadcast::Sender<StepChoice>>,
    /// Hand `persistent` steps to the daemon as watch sessions, so they outlive
    /// the run — starting the daemon if it isn't up. On by default, because a
    /// dev server that dies with the build that started it isn't persistent at
    /// all. Turned off where spawning a daemon would be a surprise: unit tests,
    /// and anywhere a caller asks for a self-contained run.
    pub persist_via_daemon: bool,
    /// Run every step against only the files it declared under `cache.inputs`,
    /// in an isolated copy of the tree — so a build that reads something it
    /// never declared fails now rather than being served a stale artifact
    /// later. Off by default and opt-in only; see [`crate::run::isolate`].
    pub authoritative: bool,
    /// Extra paths to stage into each `authoritative` sandbox, from
    /// `--sandbox-also`. Symlinked, and explicitly outside the guarantee.
    pub sandbox_also: Vec<String>,
    /// The run's stop switch, when something is in a position to ask — the
    /// daemon holds one per run so the Stop button in the web app can reach it.
    /// `None` for a run nobody can interrupt.
    pub cancel: Option<std::sync::Arc<Cancel>>,
}

impl Default for RunCtl {
    fn default() -> Self {
        Self {
            interactive: false,
            choices: None,
            persist_via_daemon: true,
            authoritative: false,
            sandbox_also: Vec::new(),
            cancel: None,
        }
    }
}

/// Drive one compiled workflow to completion, reporting progress as it goes.
///
/// There is nothing to fan out over any more: a workflow is a single graph, and
/// what used to be "several workflows in parallel" is now several branches of one
/// DAG, scheduled by the engine against their real dependencies rather than by
/// being named on the same command line.
///
/// [`RunCtl`] carries the run's policy — how recovery choices are resolved,
/// whether persistent steps are handed to the daemon, and whether steps are
/// held to their declared inputs. `RunCtl::default()` is the plain
/// non-interactive run.
#[allow(clippy::too_many_arguments)]
pub async fn run_workflow_ctl(
    name: &str,
    resolved: &crate::run::ResolvedRun,
    config: &CiabattaConfig,
    root: &Path,
    env_vars: &HashMap<String, String>,
    dry_run: bool,
    ctl: RunCtl,
    tx: mpsc::Sender<ProgressUpdate>,
) -> Result<()> {
    let _ = tx.send(ProgressUpdate::Started(name.to_string())).await;
    tracing::debug!(workflow = %name, dry_run, "starting workflow");

    let result =
        crate::run::execute(name, resolved, config, root, env_vars, dry_run, &ctl, &tx).await;

    match result {
        Ok(()) => {
            let _ = tx.send(ProgressUpdate::Completed(name.to_string())).await;
        }
        Err(ref e) => {
            let _ = tx
                .send(ProgressUpdate::Failed(name.to_string(), e.to_string()))
                .await;
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stage_order_and_indices() {
        let idx: Vec<usize> = StageKind::ALL.iter().map(|s| s.index()).collect();
        assert_eq!(idx, vec![0, 1, 2, 3]);
    }

    #[test]
    fn stage_labels_name_the_four_phases() {
        assert_eq!(StageKind::Login.label(), "login");
        assert_eq!(StageKind::Pre.label(), "pre-run");
        assert_eq!(StageKind::Main.label(), "run");
        assert_eq!(StageKind::Post.label(), "post-run");
        // Compact forms for cramped UI.
        assert_eq!(StageKind::Pre.short(), "pre");
        assert_eq!(StageKind::Main.short(), "run");
        assert_eq!(StageKind::Post.short(), "post");
    }
}
