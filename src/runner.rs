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
    #[cfg(test)]
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
    /// A run step failed, but declared `warn_on_error`: sent after its
    /// `StepFinished { ok: false }`. It counts as done — its dependents run and
    /// the run can still succeed — so viewers show it as a warning rather than
    /// a failure. `detail` is the failure as reported.
    StepWarned {
        workflow: String,
        step: String,
        detail: String,
    },
    /// A run step was skipped because its `when`/`skip_if` condition said so.
    /// It counts as satisfied, so its dependents still run. `reason` is a short
    /// explanation (e.g. ``skip_if `env.IN_CI == true` ``).
    /// A step that passed in an earlier run, carried over by `--from-failed`
    /// instead of being run again. `from` names that run.
    StepCarried {
        workflow: String,
        step: String,
        from: String,
    },
    StepSkipped {
        workflow: String,
        step: String,
        reason: String,
    },
    /// What the cache decided about a step, and why — sent as the decision is
    /// made, and again once a step that ran has been stored. The run view
    /// keeps the latest for each step.
    StepCache {
        workflow: String,
        step: String,
        report: Box<crate::run::cached::CacheReport>,
    },
    /// The cache is deciding about a step: hashing its inputs, resolving its
    /// `cache.cargo` closure, checking its outputs. Sent before the decision
    /// is made, which on a big Cargo workspace can take a while — so the
    /// step shows it is being worked on rather than sitting at "not started".
    /// Ends with its `StepSkipped`, `StepRestoring` or `StepStarted`.
    StepProcessing {
        workflow: String,
        step: String,
    },
    /// A step's outputs are being fetched from the remote cache: sent when
    /// the lookup starts, and then as the download progresses. Ends with the
    /// step's `StepSkipped` (restored) or `StepStarted` (not there after all,
    /// or it couldn't be served, so it builds).
    StepRestoring {
        workflow: String,
        step: String,
        progress: crate::run::cached::Restoring,
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
    /// A step ran `ciabatta input` and is waiting for an answer. Viewers that
    /// can answer reply with a [`crate::run::input::Answer`] on
    /// [`RunCtl::inputs`]; every viewer shows it until [`Self::InputResolved`].
    InputRequested {
        workflow: String,
        step: String,
        input: Box<crate::run::input::Pending>,
    },
    /// Question `id` was settled — answered, timed out, or abandoned by the
    /// step that asked it — and should come down.
    InputResolved {
        workflow: String,
        step: String,
        id: u64,
        outcome: crate::run::input::Outcome,
    },
    /// Something about the run its viewers should see before anything else —
    /// today, this ciabatta and the remote cache running different versions.
    /// The terminal draws it as a banner; the web app as an alert.
    Warning {
        workflow: String,
        message: String,
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

/// How often a debug run says what is still in progress.
const HEARTBEAT: std::time::Duration = std::time::Duration::from_secs(30);

/// A debug run's record of what each step is doing and since when — what the
/// heartbeat reports, so a hang reads as "deploy has been waiting for input for
/// 4m" rather than as silence.
#[derive(Default)]
struct Flight {
    /// Step → what it's doing, and since when.
    active: std::collections::BTreeMap<String, (&'static str, std::time::Instant)>,
}

impl Flight {
    fn set(&mut self, step: &str, doing: Option<&'static str>) {
        match doing {
            Some(doing) => {
                self.active
                    .insert(step.to_string(), (doing, std::time::Instant::now()));
            }
            None => {
                self.active.remove(step);
            }
        }
    }

    fn observe(&mut self, update: &ProgressUpdate) {
        match update {
            ProgressUpdate::StepProcessing { step, .. } => {
                self.set(step, Some("checking the cache"))
            }
            ProgressUpdate::StepRestoring { step, .. } => {
                if !self
                    .active
                    .get(step)
                    .is_some_and(|(d, _)| *d == "restoring")
                {
                    self.set(step, Some("restoring"));
                    tracing::debug!("{step}: restoring from the remote cache");
                }
            }
            ProgressUpdate::StepStarted { step, .. } => {
                self.set(step, Some("running"));
                tracing::debug!("{step}: started");
            }
            ProgressUpdate::StepFinished { step, ok, .. } => {
                self.set(step, None);
                tracing::debug!("{step}: {}", if *ok { "finished" } else { "failed" });
            }
            ProgressUpdate::StepWarned { step, .. } => self.set(step, None),
            ProgressUpdate::StepCarried { step, from, .. } => {
                self.set(step, None);
                tracing::debug!("{step}: carried over from {from}");
            }
            ProgressUpdate::StepSkipped { step, reason, .. } => {
                self.set(step, None);
                tracing::debug!("{step}: skipped ({reason})");
            }
            ProgressUpdate::StepCache { step, report, .. } => {
                tracing::debug!("{step}: cache {}: {}", report.outcome, report.summary);
            }
            ProgressUpdate::StepNeedsChoice { step, .. } => {
                self.set(step, Some("waiting for a recovery choice"));
                tracing::debug!("{step}: waiting for a recovery choice");
            }
            ProgressUpdate::InputRequested { step, input, .. } => {
                self.set(step, Some("waiting for input"));
                tracing::debug!("{step}: asking \"{}\"", input.prompt);
            }
            ProgressUpdate::InputResolved { step, outcome, .. } => {
                self.set(step, Some("running"));
                tracing::debug!("{step}: question settled ({})", outcome.label());
            }
            ProgressUpdate::StageStarted { workflow, stage } => {
                tracing::debug!("{workflow}: {} started", stage.label());
            }
            ProgressUpdate::Log(workflow, line) => tracing::debug!("{workflow}: {line}"),
            ProgressUpdate::Warning { message, .. } => tracing::debug!("warning: {message}"),
            ProgressUpdate::Completed(workflow) => tracing::debug!("{workflow}: completed"),
            ProgressUpdate::Failed(workflow, error) => {
                tracing::debug!("{workflow}: failed: {error}")
            }
            _ => {}
        }
    }

    fn report(&self) {
        if self.active.is_empty() {
            tracing::debug!("still running; no step is in progress");
            return;
        }
        let doing = self
            .active
            .iter()
            .map(|(step, (doing, since))| {
                format!("{step} {doing} for {}", elapsed(since.elapsed()))
            })
            .collect::<Vec<_>>()
            .join("; ");
        tracing::debug!("in progress: {doing}");
    }
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

/// Turn the signals that end a terminal process — SIGINT, SIGTERM, SIGHUP — into
/// a [`Cancel::stop`] for as long as the returned guard is held.
///
/// Every step runs as the leader of its own process group, so that stopping it
/// can take down everything it spawned. The price is that the terminal's
/// Ctrl-C, closing the terminal, or a `kill` reaches only ciabatta, never the
/// steps — and a process ended by a signal runs no destructors, so the
/// `killpg` that was meant to clean up never happens and every dev server is
/// left holding its port. Catching the signal and stopping the run instead lets
/// the engine unwind the way the TUI's stop key does.
///
/// A second signal while the run is still unwinding, or any signal once it has
/// finished, exits at once: somebody who asks twice means it.
pub fn stop_on_signals(cancel: std::sync::Arc<Cancel>) -> SignalGuard {
    let finished = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let done = finished.clone();
    tokio::spawn(async move {
        let Some(mut signals) = TerminatingSignals::listen() else {
            return;
        };
        loop {
            let code = signals.next().await;
            if done.load(std::sync::atomic::Ordering::SeqCst) || cancel.is_stopped() {
                force_exit(code);
            }
            crate::tui::note("Stopping the run…".to_string());
            cancel.stop();
        }
    });
    SignalGuard { finished }
}

/// Held for the life of a run started under [`stop_on_signals`]. Dropping it
/// marks the run finished, so a later signal exits rather than stopping a run
/// that is no longer there to stop.
pub struct SignalGuard {
    finished: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl Drop for SignalGuard {
    fn drop(&mut self) {
        self.finished
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }
}

/// The signals that should end a run. `listen` is `None` if they can't be
/// listened for, in which case the default behaviour stands.
#[cfg(unix)]
struct TerminatingSignals([tokio::signal::unix::Signal; 3]);

#[cfg(unix)]
impl TerminatingSignals {
    fn listen() -> Option<Self> {
        use tokio::signal::unix::{SignalKind, signal};
        Some(Self([
            signal(SignalKind::interrupt()).ok()?,
            signal(SignalKind::terminate()).ok()?,
            signal(SignalKind::hangup()).ok()?,
        ]))
    }

    /// The next one to arrive, as the exit code it conventionally maps to.
    async fn next(&mut self) -> i32 {
        let [int, term, hup] = &mut self.0;
        tokio::select! {
            _ = int.recv() => 128 + libc::SIGINT,
            _ = term.recv() => 128 + libc::SIGTERM,
            _ = hup.recv() => 128 + libc::SIGHUP,
        }
    }
}

#[cfg(not(unix))]
struct TerminatingSignals;

#[cfg(not(unix))]
impl TerminatingSignals {
    fn listen() -> Option<Self> {
        Some(Self)
    }

    async fn next(&mut self) -> i32 {
        // Errors only if the handler can't be installed; treat it as asked.
        let _ = tokio::signal::ctrl_c().await;
        130
    }
}

/// Leave now, giving the terminal back first if the TUI had it.
fn force_exit(code: i32) -> ! {
    if crate::tui::owns_screen() {
        let _ = crossterm::terminal::disable_raw_mode();
        let _ = crossterm::execute!(
            std::io::stdout(),
            crossterm::event::DisableMouseCapture,
            crossterm::terminal::LeaveAlternateScreen,
            crossterm::cursor::Show
        );
    }
    std::process::exit(code);
}

/// The policy a run is executed under: how it resolves recovery choices, what it
/// carries over from an earlier run, and how strictly it holds steps to what they
/// declared. Push/pull ignore this entirely.
#[derive(Default, Clone)]
pub struct RunCtl {
    /// When true, recovery nodes wait for a UI choice on `choices`; when false
    /// (plain / CI) the engine auto-picks the first `default` option or fails.
    pub interactive: bool,
    /// Broadcast bus the UI publishes [`StepChoice`]s on. Each workflow engine
    /// subscribes and filters for its own workflow + step.
    pub choices: Option<tokio::sync::broadcast::Sender<StepChoice>>,
    /// Bus the viewer publishes answers to `ciabatta input` questions on.
    /// `None` means nobody can answer: each question takes its `--default`
    /// or fails, which is what CI wants.
    pub inputs: Option<tokio::sync::broadcast::Sender<crate::run::input::Answer>>,
    /// Run every step against only the files it declared under `cache.inputs`,
    /// in an isolated copy of the tree — so a build that reads something it
    /// never declared fails now rather than being served a stale artifact
    /// later. Off by default and opt-in only; see [`crate::run::isolate`].
    pub authoritative: bool,
    /// Extra paths to stage into each `authoritative` sandbox, from
    /// `--sandbox-also`. Symlinked, and explicitly outside the guarantee.
    pub sandbox_also: Vec<String>,
    /// `--from-failed`: the earlier run this one carries on from. Steps that
    /// passed there aren't run again.
    pub resume: Option<crate::run::resume::Resume>,
    /// `--force`: ignore what the cache holds and run every step. Results are
    /// still stored, so the next ordinary run can reuse them.
    pub force: bool,
    /// The run's stop switch, when something is in a position to ask — the
    /// daemon holds one per run so the Stop button in the web app can reach it.
    /// `None` for a run nobody can interrupt.
    pub cancel: Option<std::sync::Arc<Cancel>>,
    /// How many steps may run at once (`--jobs`). `None` defers to
    /// `CIABATTA_JOBS`, then to the machine's CPU count — see [`Self::jobs`].
    pub jobs: Option<usize>,
    /// `--start-anyway`: start now even when the workspace's run slots are
    /// all taken (`workspace.max_concurrent_runs`), without taking one.
    pub start_anyway: bool,
}

impl RunCtl {
    /// How many steps may run side by side. Never zero: a limit of nothing
    /// would deadlock the scheduler rather than mean anything useful.
    pub fn jobs(&self) -> usize {
        self.jobs
            .or_else(|| {
                std::env::var("CIABATTA_JOBS")
                    .ok()
                    .and_then(|raw| raw.trim().parse().ok())
            })
            .unwrap_or_else(|| {
                std::thread::available_parallelism()
                    .map(|n| n.get())
                    .unwrap_or(4)
            })
            .max(1)
    }
}

/// Drive one compiled workflow to completion, reporting progress as it goes.
///
/// There is nothing to fan out over any more: a workflow is a single graph, and
/// what used to be "several workflows in parallel" is now several branches of one
/// DAG, scheduled by the engine against their real dependencies rather than by
/// being named on the same command line.
///
/// [`RunCtl`] carries the run's policy — how recovery choices are resolved and
/// whether steps are held to their declared inputs. `RunCtl::default()` is the plain
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
    if let Some(message) = crate::remote_cache::version::mismatch(config).await {
        let _ = tx
            .send(ProgressUpdate::Warning {
                workflow: name.to_string(),
                message,
            })
            .await;
    }
    tracing::debug!(workflow = %name, dry_run, "starting workflow");

    let started = std::time::Instant::now();
    let started_at = chrono::Utc::now().to_rfc3339();

    // Every update passes through a ledger on its way out, so the build record
    // the remote cache keeps is made of exactly what the run view was shown.
    let (inner_tx, mut inner_rx) = mpsc::channel::<ProgressUpdate>(256);
    let outer = tx.clone();
    let tap = tokio::spawn(async move {
        let mut ledger = crate::remote_cache::builds::Ledger::default();
        // Under `--debug`, the same stream is narrated to the log, and every
        // so often the log is told what is still going — so a run that hangs
        // says where.
        let mut flight = tracing::enabled!(tracing::Level::DEBUG).then(Flight::default);
        let mut beat = tokio::time::interval(HEARTBEAT);
        beat.tick().await;
        loop {
            tokio::select! {
                update = inner_rx.recv() => {
                    let Some(update) = update else { break };
                    if let Some(flight) = flight.as_mut() {
                        flight.observe(&update);
                    }
                    ledger.observe(&update);
                    let _ = outer.send(update).await;
                }
                _ = beat.tick(), if flight.is_some() => {
                    if let Some(flight) = &flight {
                        flight.report();
                    }
                }
            }
        }
        ledger
    });
    // The run's slot and its question broker, both settled by the workspace
    // root's config — they are about the machine and the people at it, not
    // about any one package.
    let setup = async {
        let workspace = crate::config::load_config(root).unwrap_or_else(|_| config.clone());
        // `(slot, dir)`: the slot this run holds — none when it shares its
        // parent's — and the directory its steps are told about.
        let slot = match crate::run::slots::limit(&workspace) {
            // A dry run does no work, so it takes no slot; `--start-anyway`
            // asked not to wait for one.
            Some(limit) if !dry_run && !ctl.start_anyway => {
                let dir = crate::run::slots::dir_for(root)?;
                if crate::run::slots::inherited(&dir, env_vars) {
                    Some((None, dir))
                } else {
                    let note_tx = inner_tx.clone();
                    let held =
                        crate::run::slots::take(&dir, limit, name, ctl.cancel.as_deref(), |line| {
                            let _ = note_tx.try_send(ProgressUpdate::Log(name.to_string(), line));
                        })
                        .await?;
                    Some((Some(held), dir))
                }
            }
            _ => None,
        };
        let timeout = crate::run::input::workspace_timeout(&workspace)?;
        let broker =
            crate::run::input::Broker::start(name, timeout, ctl.inputs.clone(), inner_tx.clone())
                .await?;
        anyhow::Ok((slot, broker))
    };
    let result = match setup.await {
        Ok((slot, broker)) => {
            let mut env_vars = broker.extend(env_vars);
            if let Some((_, dir)) = &slot {
                env_vars.insert(
                    crate::run::slots::SLOT_VAR.to_string(),
                    dir.to_string_lossy().into_owned(),
                );
            }
            let result = crate::run::execute(
                name, resolved, config, root, &env_vars, dry_run, &ctl, &inner_tx,
            )
            .await;
            // The broker holds a sender; it has to go before the tap can end.
            drop(broker);
            drop(slot);
            result
        }
        Err(e) => Err(e),
    };
    drop(inner_tx);
    let ledger = tap.await.unwrap_or_default();

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

    // Write down that this ran. Here rather than inside the engine because this
    // is the one place every run passes through — terminal, daemon, monorepo
    // graph and plain project alike — so there is no second path that could
    // quietly stop recording.
    //
    // A dry run is not a run: it is asking what *would* happen, and letting it
    // refresh the timestamp would make "when did anyone last actually run this"
    // answerable only by accident.
    if !dry_run {
        let outcome = match &result {
            Ok(()) => crate::run::history::Outcome::Success,
            Err(e) if e.to_string() == STOPPED_MESSAGE => crate::run::history::Outcome::Stopped,
            Err(_) => crate::run::history::Outcome::Failed,
        };
        let duration_ms = started.elapsed().as_millis() as u64;
        record_run(name, resolved, config, root, outcome, duration_ms).await;
        if let Err(e) = crate::run::resume::save(
            root,
            name,
            match outcome {
                crate::run::history::Outcome::Success => "success",
                crate::run::history::Outcome::Stopped => "stopped",
                crate::run::history::Outcome::Failed => "failed",
            },
            ledger.prior(&resolved.steps),
        ) {
            crate::tui::note(format!(
                "note: couldn't record this run for --from-failed: {e:#}"
            ));
        }

        // A run with no CI variables (one started from the web app, or a
        // terminal without `--local`) still came from some commit: say which.
        let mut described = env_vars.clone();
        if !described.contains_key("CIABATTA_COMMIT")
            && let Ok(git) = crate::git::local_git_vars(root)
        {
            for (key, value) in git {
                described.entry(key).or_insert(value);
            }
        }
        let mut build = crate::remote_cache::builds::describe(&described);
        build.workflow = name.to_string();
        build.outcome = match outcome {
            crate::run::history::Outcome::Success => "success",
            crate::run::history::Outcome::Stopped => "stopped",
            crate::run::history::Outcome::Failed => "failed",
        }
        .to_string();
        build.started_at = started_at;
        build.finished_at = chrono::Utc::now().to_rfc3339();
        build.duration_ms = duration_ms;
        build.error = result.as_ref().err().map(|e| format!("{e:#}"));
        build.steps = ledger.steps(&resolved.steps);
        crate::remote_cache::builds::report(config, root, &build).await;
    }

    result
}

/// Note what this run covered, locally and — when one is configured — on the
/// shared cache.
///
/// Entirely best-effort. Bookkeeping that could fail a build which otherwise
/// succeeded would be a bad trade for a timestamp, so every error here is a
/// note on stderr and nothing more.
async fn record_run(
    name: &str,
    resolved: &crate::run::ResolvedRun,
    config: &CiabattaConfig,
    root: &Path,
    outcome: crate::run::history::Outcome,
    duration_ms: u64,
) {
    use crate::run::history::History;

    // A plain project has no units of its own — one workflow, one workspace —
    // so it is recorded under ".", the same name the rest of ciabatta gives the
    // root when it is a package rather than an umbrella.
    let units: Vec<crate::run::Unit> = if resolved.units.is_empty() {
        vec![crate::run::Unit {
            workspace: ".".to_string(),
            workflow: name.to_string(),
        }]
    } else {
        resolved.units.clone()
    };

    let mut history = History::load(root);
    let records: Vec<crate::run::history::Record> = units
        .iter()
        .map(|unit| history.record(&unit.workspace, &unit.workflow, outcome, duration_ms))
        .collect();
    if let Err(e) = history.save(root) {
        crate::tui::note(format!(
            "note: couldn't record this run in the workflow history ({e:#})"
        ));
    }

    crate::remote_cache::workflows::sync(config, root, &records).await;
}

/// How long something took, precise enough to compare two runs of it: tenths
/// of a second while that still matters, whole units once it doesn't.
///
/// Distinct from the engine's timeout formatter, which renders a limit the way
/// it was written; this renders a measurement, where `0s` for a 400ms step would
/// be a lie.
pub fn elapsed(d: std::time::Duration) -> String {
    let ms = d.as_millis();
    if ms < 60_000 {
        return format!("{:.1}s", ms as f64 / 1000.0);
    }
    let secs = d.as_secs();
    let (h, m, s) = (secs / 3600, (secs % 3600) / 60, secs % 60);
    if h > 0 {
        format!("{h}h{m:02}m")
    } else {
        format!("{m}m{s:02}s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn elapsed_times_read_the_way_people_say_them() {
        use std::time::Duration;
        assert_eq!(elapsed(Duration::from_millis(0)), "0.0s");
        assert_eq!(elapsed(Duration::from_millis(420)), "0.4s");
        assert_eq!(elapsed(Duration::from_millis(12_345)), "12.3s");
        assert_eq!(elapsed(Duration::from_secs(65)), "1m05s");
        assert_eq!(elapsed(Duration::from_secs(3_725)), "1h02m");
    }

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
    }
}
