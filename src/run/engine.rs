//! The run DAG engine: drives a resolved flowchart through the four run
//! phases (`login → pre → run → post`), where the `run` phase executes the
//! step graph — running ready steps, and on failure routing to `on_error`
//! recovery nodes that offer a choice of fix scripts.

use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result, bail};
use tokio::sync::mpsc::{self, UnboundedSender};
use tokio::task::JoinHandle;

use crate::config::CiabattaConfig;
use crate::registry::{self, LogSink};
use crate::runner::{ProgressUpdate, RunCtl, StageKind};

use super::{ResolvedRun, RunStep, prepare_env, transfer};

/// How many times a single step may be re-run through recovery before the run
/// gives up — bounds retry loops so a persistently failing step can't spin forever.
const MAX_STEP_ATTEMPTS: u32 = 20;

/// Whether a step counts as "satisfied" for the purposes of its dependents.
#[derive(Clone, Copy, PartialEq, Eq)]
enum StepState {
    Pending,
    Succeeded,
    /// A fix ran and the branch was cleared without a retry — treated as
    /// satisfied so downstream steps proceed.
    Recovered,
    /// A `when`/`skip_if` condition excluded the step — satisfied so downstream
    /// steps proceed, but nothing ran.
    Skipped,
    /// A `persistent` step was started and left running in the background. Its
    /// dependents are released immediately: waiting for a dev server to exit is
    /// exactly the hang persistence exists to avoid.
    Started,
    /// Its action is in flight. Not satisfied: its dependents wait for it.
    Running,
    Failed,
    /// It failed, but declared `warn_on_error`: the failure is reported as a
    /// warning and the step counts as done, so its dependents proceed.
    Warned,
}

impl StepState {
    fn satisfied(self) -> bool {
        matches!(
            self,
            StepState::Succeeded
                | StepState::Recovered
                | StepState::Skipped
                | StepState::Started
                | StepState::Warned
        )
    }
}

/// How a step's attempt ended, for the failure summary at the end of a run.
struct StepFailure {
    step: String,
    /// The failure as reported, already phrased for an operator.
    detail: String,
}

/// A started `persistent` step or background task, and what now owns it.
struct Persistent {
    step: String,
    ownership: Ownership,
}

/// Who is keeping a `persistent` step alive.
enum Ownership {
    /// The daemon, as a watch session. This is the normal case and the point of
    /// persistence: the process outlives the run that started it, keeps
    /// collecting output, and can be tailed or stopped later by id.
    Daemon { session: u64, url: String },
    /// This process, as a background task — the fallback for when no daemon
    /// could be reached. The step still doesn't block the graph, but it can't
    /// outlive the run either, so it's stopped at the end and said so.
    Local(JoinHandle<()>),
}

/// Entry point for a run: drives an already-compiled workflow graph through the
/// four phases.
#[allow(clippy::too_many_arguments)]
pub async fn execute(
    name: &str,
    resolved: &ResolvedRun,
    config: &CiabattaConfig,
    root: &Path,
    env_vars: &HashMap<String, String>,
    dry_run: bool,
    ctl: &RunCtl,
    tx: &mpsc::Sender<ProgressUpdate>,
) -> Result<()> {
    let resolved = resolved.clone();

    // Source any configured `.env` file(s) before anything runs, layering their
    // values under whatever is already resolved (CI / git / `-e`), and gate the
    // whole flowchart on `required_env`. The daemon's launcher runs the very
    // same check before it spawns us, so it can prompt for what's missing
    // instead of starting a run that would only abort here.
    // The `.env` rules, before anything is sourced:
    //   * a build that declares required_env must say where those variables are
    //     documented, or a fresh checkout fails on a variable nobody mentioned;
    //   * a missing `.env` is generated from that template rather than being an
    //     error, which is the whole point of declaring it.
    let meta = config.workspace.clone().unwrap_or_default();

    // A compiled workflow graph was already checked per sub-workspace, where
    // the member that declared each requirement was known — so only a plain
    // project workflow is checked here. Applying the root's config to a
    // monorepo's requirements would demand `env_default` from a repository
    // root that never declared the variables in the first place.
    let from_workspace = resolved.steps.iter().any(|step| step.workspace.is_some());
    if !from_workspace {
        crate::environment::files::require_template(&meta, root, &resolved.required_env, name)?;
    }

    // An elected profile is copied into each workspace's `.env` before
    // anything is generated or sourced: the run then reads those `.env`s like
    // any other. A `.env` somebody wrote is backed up first; a dry run only
    // says what it would copy.
    if let Some(profile) = crate::environment::profiles::active(env_vars) {
        for done in crate::environment::profiles::apply(root, &profile, dry_run)? {
            if let Some(line) = crate::environment::profiles::describe(&done, dry_run) {
                let _ = tx.send(ProgressUpdate::Log(name.to_string(), line)).await;
            }
        }
    }

    // A fresh checkout has no `.env` — the checked-in template is what it has.
    // Generated for the project root *and* for every sub-workspace this run
    // touches, since a member's steps resolve through the member's own file.
    for written in generate_missing_env(&meta, root, &resolved, dry_run)? {
        let _ = tx
            .send(ProgressUpdate::Log(
                name.to_string(),
                match dry_run {
                    true => format!("[dry-run] would generate {}", written.display()),
                    false => format!("Generated {} because it was missing", written.display()),
                },
            ))
            .await;
    }

    // With the files in place, settle where the environment is searched: the
    // workspace the run was started from up to the root. The CLI has usually
    // decided already; a run that arrives without it starts at the root.
    let resolved = crate::run::with_env_levels(resolved, root, root, &meta);

    let prepared = prepare_env(&resolved, root, env_vars)?;
    for file in &prepared.sourced {
        let _ = tx
            .send(ProgressUpdate::Log(
                name.to_string(),
                format!("Sourcing env file: {file}"),
            ))
            .await;
    }

    // Say which features are on before anything runs. A build shaped by a
    // variable nobody mentioned is the hardest kind to explain afterwards, and
    // these change the cache key — so a run that reuses nothing when you
    // expected it to should be able to point at the line that says why.
    if let Some(line) = prepared.features.describe() {
        let _ = tx.send(ProgressUpdate::Log(name.to_string(), line)).await;
    }

    // Likewise the profile and any custom arguments: both change what the
    // steps see, and the arguments exist nowhere but in the command that was
    // typed — so they're marked (⚑, drawn in yellow) and the log says where
    // they'd be better kept.
    // The profile is named with why it's the one in use — or why there's
    // none — since it decides what each workspace's `.env` holds.
    let (_, profile_reason) = crate::environment::profiles::choice(env_vars, root);
    let _ = tx
        .send(ProgressUpdate::Log(
            name.to_string(),
            format!("env profile: {profile_reason}"),
        ))
        .await;
    let arguments = crate::cli::custom_arg_names(&prepared.env);
    let overrode = crate::cli::custom_arg_overrides(&prepared.env);
    let mut custom: Vec<(&String, &String)> = prepared
        .env
        .iter()
        .filter(|(key, _)| arguments.contains(&key.as_str()))
        .collect();
    custom.sort();
    for (key, value) in &custom {
        let _ = tx
            .send(ProgressUpdate::Log(
                name.to_string(),
                match overrode.contains(&key.as_str()) {
                    true => {
                        format!("⚑ custom argument {key}={value} (overrides the terminal's value)")
                    }
                    false => format!("⚑ custom argument {key}={value}"),
                },
            ))
            .await;
    }
    if !custom.is_empty() {
        let _ = tx
            .send(ProgressUpdate::Log(
                name.to_string(),
                "⚑ tip: if this run needs these every time, put them in .env.<name> and run \
                 with --env-profile <name>"
                    .to_string(),
            ))
            .await;
    }

    if !prepared.is_ready() {
        let help = missing_env_help(name, &prepared);
        // Console: printed directly so it shows even in `--gui` mode, where
        // progress updates are folded into the browser view rather than stdout.
        // Yellow and on lines of its own, so it can't be lost in the summary
        // printed above it.
        // Not while the TUI has the screen, though: it shows them from the log.
        if !crate::tui::owns_screen() {
            use owo_colors::OwoColorize;
            for line in &help {
                eprintln!("{}", format!("[{name}] {line}").style(crate::color::warn()));
            }
        }
        // GUI: the same lines into the workflow's log panel.
        for line in help {
            let _ = tx.send(ProgressUpdate::Log(name.to_string(), line)).await;
        }
        // Returning Err becomes a `Failed` update (shown as the workflow's error in
        // the GUI, and on stderr by the plain runner).
        bail!(
            "Run '{name}' cannot start: required variable(s) not set: {} — see above for how to \
             set them",
            prepared.missing().join(", ")
        );
    }
    let env_vars = &prepared.env;

    for stage in StageKind::ALL {
        let _ = tx
            .send(ProgressUpdate::StageStarted {
                workflow: name.to_string(),
                stage,
            })
            .await;

        let ran = match stage {
            StageKind::Login => {
                run_phase_hook(resolved.login.as_deref(), name, root, env_vars, dry_run, tx).await?
            }
            StageKind::Pre => {
                run_phase_hook(resolved.pre.as_deref(), name, root, env_vars, dry_run, tx).await?
            }
            StageKind::Main => {
                // Caching is opt-in per workspace, so this is usually a no-op —
                // but it has to be set up before the graph starts, since each
                // step's key depends on what the steps before it produced.
                //
                // Off entirely under `--authoritative`. A cache hit skips the
                // step, and a step that doesn't run isn't held to anything —
                // so on a warm cache the flag would quietly verify nothing,
                // which is worse than not offering it. The cache is also the
                // thing under suspicion: you reach for this flag precisely
                // when you think it has been serving stale results, and
                // answering the question with the accused is no answer.
                let mut cache = if dry_run || ctl.authoritative {
                    None
                } else {
                    let mut session = super::cached::Session::open(root, config);
                    if let Some(session) = session.as_mut() {
                        if ctl.force {
                            session.force();
                        }
                        session.connect_remote().await;
                    }
                    session
                };

                if ctl.force && !dry_run && cache.is_some() {
                    let _ = tx
                        .send(ProgressUpdate::Log(
                            name.to_string(),
                            "force: the cache is ignored, so every step runs; what they \
                             produce is still stored"
                                .to_string(),
                        ))
                        .await;
                }

                if ctl.authoritative && !dry_run {
                    let _ = tx
                        .send(ProgressUpdate::Log(
                            name.to_string(),
                            "authoritative: every step runs against only its declared \
                             cache.inputs, and the cache is bypassed so none is skipped"
                                .to_string(),
                        ))
                        .await;
                }

                // `--authoritative` resolves what each step declared through
                // the same context the cache uses, so "its inputs" means one
                // thing across the whole tool. Built here rather than inside
                // the cache session because the two are independent: holding a
                // build to its declarations is worth doing with caching off,
                // and is arguably most worth doing then.
                let mut isolation = match ctl.authoritative && !dry_run {
                    false => None,
                    true => {
                        let workspace = crate::workspace::Workspace::discover(root).ok();
                        let context = crate::cache::cli::WorkspaceContext {
                            workspace: workspace.as_ref(),
                            root: root.to_path_buf(),
                            config,
                        };
                        Some(super::isolate::Isolation::plan(
                            root,
                            &resolved.steps,
                            &context,
                            &ctl.sandbox_also,
                        ))
                    }
                };

                run_graph(
                    &resolved,
                    name,
                    config,
                    root,
                    &prepared,
                    dry_run,
                    ctl,
                    tx,
                    cache.as_mut(),
                    isolation.as_mut(),
                )
                .await?;

                if let Some(summary) = isolation.as_ref().and_then(|i| i.summary()) {
                    let _ = tx
                        .send(ProgressUpdate::Log(name.to_string(), summary))
                        .await;
                }

                // The graph is done: tell the shared cache which of its
                // entries this run leant on, so they age from now rather than
                // from whenever they were last downloaded.
                if let Some(session) = cache.as_mut() {
                    session.finish().await;
                }

                if let Some(summary) = cache.as_ref().and_then(|c| c.stats.summary()) {
                    let _ = tx
                        .send(ProgressUpdate::Log(name.to_string(), summary))
                        .await;
                }
                true
            }
            StageKind::Post => {
                run_phase_hook(resolved.post.as_deref(), name, root, env_vars, dry_run, tx).await?
            }
        };

        let _ = tx
            .send(ProgressUpdate::StageFinished {
                workflow: name.to_string(),
                stage,
                ran,
            })
            .await;
    }

    Ok(())
}

/// Run an optional phase hook (login/pre/post) as a shell command, forwarding its
/// output as workflow log lines. Returns whether a command actually ran.
/// The run's environment variable saying how the rest of the run went, as an
/// after action sees it: `succeeded` or `failed`.
pub const RUN_OUTCOME_VAR: &str = "CIABATTA_RUN_OUTCOME";

/// Run the graph, then its after actions.
///
/// After actions are held out of the graph and run in a pass of their own once
/// it is over, whichever way it went. The graph's own failure is the one worth
/// reporting; an after action's counts only when the graph had passed.
#[allow(clippy::too_many_arguments)]
async fn run_graph(
    resolved: &ResolvedRun,
    workflow: &str,
    config: &CiabattaConfig,
    root: &Path,
    env: &super::PreparedEnv,
    dry_run: bool,
    ctl: &RunCtl,
    tx: &mpsc::Sender<ProgressUpdate>,
    cache: Option<&mut super::cached::Session>,
    isolation: Option<&mut super::isolate::Isolation>,
) -> Result<()> {
    let (main, after) = split_after(resolved);
    let outcome = run_dag(
        &main, workflow, config, root, env, dry_run, ctl, tx, cache, isolation,
    )
    .await;
    let cleaned = run_after(
        &after,
        outcome.is_ok(),
        workflow,
        config,
        root,
        env,
        dry_run,
        ctl,
        tx,
    )
    .await;
    outcome.and(cleaned)
}

/// Split a run into its graph and its after actions.
///
/// The after actions keep only the edges among themselves: the ones the
/// compiler drew to the graph's last steps are there for the drawing, and
/// would leave an after action blocked forever behind a step that failed.
fn split_after(resolved: &ResolvedRun) -> (ResolvedRun, ResolvedRun) {
    let (after, steps): (Vec<RunStep>, Vec<RunStep>) =
        resolved.steps.iter().cloned().partition(|s| s.after);
    let names: std::collections::HashSet<String> = after.iter().map(|s| s.name.clone()).collect();
    let after = after
        .into_iter()
        .map(|mut step| {
            step.needs.retain(|dep| names.contains(dep));
            step
        })
        .collect();
    (
        ResolvedRun {
            steps,
            ..resolved.clone()
        },
        ResolvedRun {
            steps: after,
            ..resolved.clone()
        },
    )
}

/// Run a workflow's after actions, once the rest of the run is over.
///
/// They run whether the graph passed or failed — that is what they are for —
/// and see [`RUN_OUTCOME_VAR`] to tell which. A run that was asked to stop
/// skips them, since stopping means "start nothing further". Never cached: a
/// clean-up that was skipped because nothing changed would not have cleaned.
#[allow(clippy::too_many_arguments)]
async fn run_after(
    after: &ResolvedRun,
    succeeded: bool,
    workflow: &str,
    config: &CiabattaConfig,
    root: &Path,
    env: &super::PreparedEnv,
    dry_run: bool,
    ctl: &RunCtl,
    tx: &mpsc::Sender<ProgressUpdate>,
) -> Result<()> {
    if after.steps.is_empty() {
        return Ok(());
    }
    if ctl.cancel.as_ref().is_some_and(|c| c.is_stopped()) {
        for step in &after.steps {
            let _ = tx
                .send(ProgressUpdate::StepSkipped {
                    workflow: workflow.to_string(),
                    step: step.name.clone(),
                    reason: "not started — the run was stopped".to_string(),
                })
                .await;
        }
        return Ok(());
    }
    let outcome = match succeeded {
        true => "succeeded",
        false => "failed",
    };
    let _ = tx
        .send(ProgressUpdate::Log(
            workflow.to_string(),
            format!(
                "after: the run {outcome}; running {} after action step(s) \
                 ({RUN_OUTCOME_VAR}={outcome})",
                after.steps.len()
            ),
        ))
        .await;
    let mut env = env.clone();
    env.env
        .insert(RUN_OUTCOME_VAR.to_string(), outcome.to_string());
    run_dag(
        after, workflow, config, root, &env, dry_run, ctl, tx, None, None,
    )
    .await
    .context("an after action failed")
}

async fn run_phase_hook(
    cmd: Option<&str>,
    workflow: &str,
    root: &Path,
    env_vars: &HashMap<String, String>,
    dry_run: bool,
    tx: &mpsc::Sender<ProgressUpdate>,
) -> Result<bool> {
    let Some(cmd) = cmd else { return Ok(false) };
    let mut log: Vec<String> = Vec::new();
    let (line_tx, forwarder) = recipe_log_stream(tx, workflow);
    let res = {
        let mut sink = LogSink::streaming(&mut log, line_tx);
        sink.push(format!("$ {cmd}"));
        if dry_run {
            sink.push(format!("[dry-run] would run: {cmd}"));
            Ok(())
        } else {
            registry::run_shell_command(cmd, root, env_vars, &mut sink).await
        }
    };
    // Dropping the sink closes the line channel; awaiting the forwarder flushes
    // every streamed line into the UI state before we move on.
    let _ = forwarder.await;
    res?;
    Ok(true)
}

/// Execute the step DAG. Starts every step whose `needs` are satisfied, up to
/// [`RunCtl::jobs`] at once, and starts more as each one finishes; on a step
/// failure, routes to its `on_error` recovery node.
#[allow(clippy::too_many_arguments)]
async fn run_dag(
    resolved: &ResolvedRun,
    workflow: &str,
    config: &CiabattaConfig,
    root: &Path,
    env: &super::PreparedEnv,
    dry_run: bool,
    ctl: &RunCtl,
    tx: &mpsc::Sender<ProgressUpdate>,
    mut cache: Option<&mut super::cached::Session>,
    mut isolation: Option<&mut super::isolate::Isolation>,
) -> Result<()> {
    // Every tool the graph's steps declare has to be on PATH before anything
    // runs. Discovering a missing toolchain three steps in — as a bare "command
    // not found" from some script — is the failure mode this prevents.
    preflight_tools(resolved, workflow, root, dry_run, tx).await?;

    let mut state: HashMap<&str, StepState> = resolved
        .steps
        .iter()
        .map(|s| (s.name.as_str(), StepState::Pending))
        .collect();
    let mut attempts: HashMap<&str, u32> = HashMap::new();
    // Failures that didn't abort the run (`continue_on_error`, or a `timeout`
    // that expired), reported together once the rest of the graph has drained.
    let mut tolerated: Vec<StepFailure> = Vec::new();
    let mut persistent: Vec<Persistent> = Vec::new();

    // Background tasks go up before the first wave, not when the scheduler
    // happens to reach them. They exist so the steps can finish, so "started
    // somewhere in wave 1, in whatever order the units compiled" is not good
    // enough — a step in wave 1 could otherwise run before the mock API it
    // talks to had been launched at all.
    if !dry_run {
        for step in resolved.steps.iter().filter(|s| s.background) {
            let step_env = &env.env;
            persistent.push(start_persistent(step, workflow, root, step_env, ctl, tx).await?);
            state.insert(step.name.as_str(), StepState::Started);
        }
    }

    // Set once the run has been asked to stop, so the unwinding below can say
    // that is what happened rather than reporting a failed build.
    let mut stopped = false;
    // The failure that ends the run, once one has. Nothing new is started
    // after it, but what is already running is allowed to finish: killing a
    // sibling half way through its own work would leave the tree in a state
    // nobody asked for, and its log would read like a second failure.
    let mut fatal: Option<anyhow::Error> = None;

    // Steps whose `needs` are met run side by side, up to this many at once.
    // They used to run one after another even when nothing connected them,
    // which made a wide graph exactly as slow as a tall one.
    let jobs = ctl.jobs();
    let mut in_flight = futures::stream::FuturesUnordered::new();
    // What a running step will need once it finishes — kept beside the futures
    // rather than inside them, because the cache session is borrowed mutably
    // to decide about the next step while this one is still running.
    let mut pending_tokens: HashMap<&str, Box<super::cached::Pending>> = HashMap::new();

    loop {
        // Asked to stop: schedule nothing further. Whatever is running was
        // raced against the same switch, so it ends promptly on its own.
        if ctl.cancel.as_ref().is_some_and(|c| c.is_stopped()) {
            stopped = true;
        }

        // ── start everything that can start ─────────────────────────────────
        // Repeated until it settles, because a step that is skipped or served
        // from the cache satisfies its dependents without running — and they
        // may be able to start right now, too.
        let mut progressed = true;
        while progressed && !stopped && fatal.is_none() && in_flight.len() < jobs {
            progressed = false;

            // A step is ready when it is Pending, not a recovery node, and all
            // its `needs` are satisfied. Recovery nodes are only entered via
            // on_error.
            let ready: Vec<&RunStep> = resolved
                .steps
                .iter()
                .filter(|s| !s.recover)
                .filter(|s| state.get(s.name.as_str()) == Some(&StepState::Pending))
                .filter(|s| {
                    s.needs.iter().all(|dep| {
                        state
                            .get(dep.as_str())
                            .map(|st| st.satisfied())
                            .unwrap_or(false)
                    })
                })
                .collect();

            for step in ready {
                if in_flight.len() >= jobs {
                    break;
                }
                if ctl.cancel.as_ref().is_some_and(|c| c.is_stopped()) {
                    stopped = true;
                    break;
                }

                // A `when`/`skip_if` condition can exclude the step; if so,
                // mark it satisfied (so dependents proceed) and move on
                // without running it. Every one of these reads the step's
                // *own* environment: its workspace's `.env` first, then
                // outward. Two members of a monorepo can set the same variable
                // to different values and each of their steps sees its own.
                let step_env = &env.env;

                if let Some(reason) = super::step_skip_reason(step, step_env)? {
                    state.insert(step.name.as_str(), StepState::Skipped);
                    progressed = true;
                    let _ = tx
                        .send(ProgressUpdate::StepSkipped {
                            workflow: workflow.to_string(),
                            step: step.name.clone(),
                            reason,
                        })
                        .await;
                    continue;
                }

                // A persistent step is started and left running: the graph
                // moves on without it, so a dev server can't hang everything
                // behind it. Background tasks never reach here — they were
                // started before the first wave and are already `Started`.
                if step.persistent && !dry_run {
                    persistent
                        .push(start_persistent(step, workflow, root, step_env, ctl, tx).await?);
                    state.insert(step.name.as_str(), StepState::Started);
                    progressed = true;
                    continue;
                }

                // Ask the cache before doing the work. A hit restores this
                // step's declared outputs and marks it satisfied, so
                // everything downstream proceeds exactly as if it had run.
                let mut pending = None;
                if let Some(session) = cache.as_deref_mut() {
                    session.report_to(tx.clone(), workflow);
                    let _ = tx
                        .send(ProgressUpdate::StepProcessing {
                            workflow: workflow.to_string(),
                            step: step.name.clone(),
                        })
                        .await;
                    let decided = session.before(step, step_env).await;
                    // Whatever was decided, the run view gets the details —
                    // that's what puts the cache icon on the node, and what
                    // the inspector reads when somebody asks why it missed.
                    if let Some(report) = session.take_report() {
                        // Said once, as the decision is made — the report is
                        // sent again after the step runs, and this isn't news
                        // the second time.
                        if !report.outputs_in_inputs.is_empty() {
                            let _ = tx
                                .send(ProgressUpdate::Log(
                                    workflow.to_string(),
                                    format!(
                                        "warning: {}: {}",
                                        step.name,
                                        crate::cache::describe_outputs_in_inputs(
                                            &report.outputs_in_inputs
                                        )
                                    ),
                                ))
                                .await;
                        }
                        let _ = tx
                            .send(ProgressUpdate::StepCache {
                                workflow: workflow.to_string(),
                                step: step.name.clone(),
                                report: Box::new(report),
                            })
                            .await;
                    }
                    match decided {
                        Ok(super::cached::Action::Skip { note }) => {
                            state.insert(step.name.as_str(), StepState::Skipped);
                            progressed = true;
                            let _ = tx
                                .send(ProgressUpdate::StepSkipped {
                                    workflow: workflow.to_string(),
                                    step: step.name.clone(),
                                    reason: note,
                                })
                                .await;
                            continue;
                        }
                        Ok(super::cached::Action::Run { note, token }) => {
                            if let Some(note) = note {
                                let _ = tx
                                    .send(ProgressUpdate::Log(
                                        workflow.to_string(),
                                        format!("{}: {note}", step.name),
                                    ))
                                    .await;
                            }
                            pending = Some(token);
                        }
                        // A cache that can't decide costs a rebuild, never a
                        // build. Nothing will be recorded about what the step
                        // produced, so its dependents can't trust their keys
                        // either — without this they would key on a missing
                        // upstream, which is a different key from every run
                        // where the decision worked.
                        Err(e) => {
                            session.mark_unaccounted(&step.name);
                            let _ = tx
                                .send(ProgressUpdate::Log(
                                    workflow.to_string(),
                                    format!("note: skipping the cache for {} ({e:#})", step.name),
                                ))
                                .await;
                        }
                    }
                }

                // Under `--authoritative` the step runs against a copy of the
                // tree holding only what it declared. A step that declares
                // nothing gets no sandbox and is counted as unverified, and a
                // sandbox that can't be built is reported and skipped rather
                // than failing the build: the flag is here to find undeclared
                // inputs, not to become a new way for a run to die.
                let mut sandbox = None;
                if let Some(iso) = isolation.as_deref_mut() {
                    match iso.prepare(step, step_env) {
                        Ok(prepared) => sandbox = prepared,
                        Err(e) => {
                            let _ = tx
                                .send(ProgressUpdate::Log(
                                    workflow.to_string(),
                                    format!(
                                        "note: {} could not be isolated, running it normally ({e:#})",
                                        step.name
                                    ),
                                ))
                                .await;
                        }
                    }
                }
                if let Some(sandbox) = sandbox.as_ref() {
                    let _ = tx
                        .send(ProgressUpdate::Log(
                            workflow.to_string(),
                            match sandbox.linked {
                                0 => format!(
                                    "{}: isolated with {} declared input file(s)",
                                    step.name, sandbox.staged
                                ),
                                linked => format!(
                                    "{}: isolated with {} declared input file(s), plus {} \
                                     unvouched-for path(s) from --sandbox-also",
                                    step.name, sandbox.staged, linked
                                ),
                            },
                        ))
                        .await;
                }

                if let Some(token) = pending {
                    pending_tokens.insert(step.name.as_str(), token);
                }
                state.insert(step.name.as_str(), StepState::Running);
                progressed = true;

                let cancel = ctl.cancel.as_ref();
                in_flight.push(async move {
                    let started = std::time::Instant::now();
                    let outcome = run_step_action(
                        step,
                        workflow,
                        config,
                        root,
                        step_env,
                        dry_run,
                        sandbox.as_ref().map(|s| s.dir.as_path()),
                        cancel,
                        tx,
                    )
                    .await;
                    (step, sandbox, started.elapsed(), outcome)
                });
            }
        }

        // ── wait for something to finish ────────────────────────────────────
        let Some((step, sandbox, took, outcome)) = futures::StreamExt::next(&mut in_flight).await
        else {
            // Nothing running and nothing more could start: the graph is
            // done, or what's left is blocked behind a failure.
            break;
        };
        let pending = pending_tokens.remove(step.name.as_str());
        let step_env = &env.env;

        // Whatever happened, the sandbox has to be dealt with before the
        // outcome is: a success owes its outputs to the real tree, and a
        // failure owes the operator the directory to look in.
        let outcome = match (sandbox, isolation.as_deref_mut()) {
            (Some(sandbox), Some(iso)) if outcome.is_ok() => match iso.collect(sandbox) {
                Ok(_) => outcome,
                // The step worked but its outputs didn't come back, so the
                // tree is not what a successful run should leave behind.
                // That is a failure, and saying otherwise would strand
                // everything downstream on missing files.
                Err(e) => Err(e.context(format!(
                    "{} succeeded in isolation but its declared outputs could not be collected",
                    step.name
                ))),
            },
            (Some(sandbox), Some(iso)) => {
                let kept = iso.keep(sandbox);
                let _ = tx
                    .send(ProgressUpdate::Log(
                        workflow.to_string(),
                        format!(
                            "{} failed in isolation. Its sandbox is kept at {} — if it reads \
                             a file that isn't there, that file is missing from cache.inputs.",
                            step.name,
                            kept.display(),
                        ),
                    ))
                    .await;
                outcome
            }
            (_, _) => outcome,
        };

        match outcome {
            Ok(()) => {
                state.insert(step.name.as_str(), StepState::Succeeded);
                // Only a step that actually succeeded is worth keeping.
                if let (Some(session), Some(token)) = (cache.as_deref_mut(), pending) {
                    session.after(step, token, took.as_millis() as u64).await;
                    if let Some(report) = session.take_report() {
                        let _ = tx
                            .send(ProgressUpdate::StepCache {
                                workflow: workflow.to_string(),
                                step: step.name.clone(),
                                report: Box::new(report),
                            })
                            .await;
                    }
                }
            }
            Err(err) if err.is::<Stopped>() => {
                // Not a failure: somebody pressed Stop, and this step was cut
                // short by that. Recovery must not be entered — there is
                // nothing to put back on the rails — and it must not be
                // reported as a broken build, or the next person goes looking
                // for a bug that isn't there.
                state.insert(step.name.as_str(), StepState::Failed);
                if let Some(session) = cache.as_deref_mut() {
                    session.mark_unaccounted(&step.name);
                }
                stopped = true;
            }
            Err(err) => {
                state.insert(step.name.as_str(), StepState::Failed);
                // It ran and left the tree in a state nothing recorded —
                // whatever runs on past this, by recovery or by tolerance,
                // can't be served from the cache behind it.
                if let Some(session) = cache.as_deref_mut() {
                    session.mark_unaccounted(&step.name);
                }

                // A recovery route takes precedence: it exists to put the run
                // back on the rails rather than write the failure off. Not
                // once the run is already failing, though — a fix-it prompt
                // for a run that can't finish anyway answers nothing.
                if fatal.is_none()
                    && !stopped
                    && let Some(target) = step.on_error.as_deref()
                {
                    recover(
                        resolved,
                        step,
                        target,
                        workflow,
                        root,
                        step_env,
                        dry_run,
                        ctl,
                        tx,
                        &mut state,
                        &mut attempts,
                    )
                    .await?;
                    continue;
                }

                // A failure the step said to treat as a warning: say so, and
                // carry on as though it had succeeded. Nothing it produced is
                // cached — it was marked unaccounted above — so whatever runs
                // behind it reruns rather than trusting a half-made output.
                if step.warn_on_error && !err.is::<TimedOut>() {
                    state.insert(step.name.as_str(), StepState::Warned);
                    let _ = tx
                        .send(ProgressUpdate::Log(
                            workflow.to_string(),
                            format!(
                                "⚠ step '{}' failed, but it is marked warn_on_error, so the \
                                 run carries on: {err}",
                                step.name
                            ),
                        ))
                        .await;
                    let _ = tx
                        .send(ProgressUpdate::StepWarned {
                            workflow: workflow.to_string(),
                            step: step.name.clone(),
                            detail: err.to_string(),
                        })
                        .await;
                    continue;
                }

                // A tolerated failure takes this branch out of the graph and
                // lets every independent branch finish; the run still ends up
                // failing, but with the full picture.
                if step.continue_on_error || err.is::<TimedOut>() {
                    let _ = tx
                        .send(ProgressUpdate::Log(
                            workflow.to_string(),
                            format!(
                                "⚠ step '{}' failed but the graph continues: {err}",
                                step.name
                            ),
                        ))
                        .await;
                    tolerated.push(StepFailure {
                        step: step.name.clone(),
                        detail: err.to_string(),
                    });
                    continue;
                }

                // Nothing to fall back on: this failure ends the run, once
                // whatever else is running has finished.
                if fatal.is_none() {
                    fatal = Some(anyhow::anyhow!("Run step '{}' failed: {}", step.name, err));
                }
            }
        }
    }

    if let Some(err) = fatal {
        // Everything that never got going was waiting on this run going
        // somewhere; say why it didn't, rather than leaving it pending.
        for step in &resolved.steps {
            if !step.recover && state.get(step.name.as_str()) == Some(&StepState::Pending) {
                let _ = tx
                    .send(ProgressUpdate::StepSkipped {
                        workflow: workflow.to_string(),
                        step: step.name.clone(),
                        reason: "not started — the run failed".to_string(),
                    })
                    .await;
            }
        }
        stop_persistent(persistent, workflow, tx).await;
        return Err(err);
    }

    // Steps left Pending were waiting on a branch that failed; say so, rather
    // than leaving them silently unaccounted for in the graph.
    for step in &resolved.steps {
        if !step.recover && state.get(step.name.as_str()) == Some(&StepState::Pending) {
            let _ = tx
                .send(ProgressUpdate::StepSkipped {
                    workflow: workflow.to_string(),
                    step: step.name.clone(),
                    reason: "blocked by a failed dependency".to_string(),
                })
                .await;
        }
    }

    stop_persistent(persistent, workflow, tx).await;

    // A stopped run is not a failed one. Somebody asked for this, they know why,
    // and reporting it as a broken build would send the next person looking for
    // a bug that isn't there. Whatever the step that was cut short reported is
    // beside the point, so this is checked before the failure summaries.
    if stopped || ctl.cancel.as_ref().is_some_and(|c| c.is_stopped()) {
        let _ = tx
            .send(ProgressUpdate::Log(
                workflow.to_string(),
                "■ stopped — the run was asked to stop, so nothing further was started".to_string(),
            ))
            .await;
        bail!("{}", crate::runner::STOPPED_MESSAGE);
    }

    // A step still Failed and untolerated means recovery ran out of road.
    if let Some(failed) = resolved.steps.iter().find(|s| {
        state.get(s.name.as_str()) == Some(&StepState::Failed)
            && !tolerated.iter().any(|f| f.step == s.name)
    }) {
        bail!(
            "Run did not complete: step '{}' failed and was not recovered.",
            failed.name
        );
    }

    if !tolerated.is_empty() {
        let summary = tolerated
            .iter()
            .map(|f| format!("{} ({})", f.step, f.detail))
            .collect::<Vec<_>>()
            .join("; ");
        bail!(
            "Run finished with {} failed step(s): {summary}",
            tolerated.len()
        );
    }
    Ok(())
}

/// Marker error for a step killed by its own `timeout`, so the DAG loop can
/// tell "this hung and we cut it loose" (never fatal to the rest of the graph)
/// apart from an ordinary non-zero exit.
#[derive(Debug)]
struct TimedOut;

impl std::fmt::Display for TimedOut {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "timed out")
    }
}

impl std::error::Error for TimedOut {}

/// Marker error for a step cut short because the run was asked to stop.
///
/// Kept apart from an ordinary failure because it is not one: nobody needs to
/// go and look at why the step "failed", and a stopped run should not be
/// reported as a broken build. See [`crate::runner::Cancel`].
#[derive(Debug)]
struct Stopped;

impl std::fmt::Display for Stopped {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "stopped")
    }
}

impl std::error::Error for Stopped {}

/// Put a missing `.env` in place, for the project root and for every
/// sub-workspace this run's steps come from.
///
/// This is the payoff for committing a template: a fresh checkout builds
/// instead of failing on a variable nobody has heard of. It never overwrites,
/// so an edited `.env` is safe, and it happens here — at the start of a run —
/// rather than while a graph is merely being planned, because generating a file
/// is a side effect and planning shouldn't have any.
///
/// A dry run reports what it would generate and writes nothing: "without
/// actually running anything" has to include not leaving files behind.
///
/// Returns the paths written (or, on a dry run, the ones that would be).
fn generate_missing_env(
    meta: &crate::workspace::WorkspaceMeta,
    root: &Path,
    resolved: &ResolvedRun,
    dry_run: bool,
) -> Result<Vec<std::path::PathBuf>> {
    use crate::environment::files;

    let mut written = Vec::new();
    let generate = |meta: &crate::workspace::WorkspaceMeta,
                    dir: &Path|
     -> Result<Option<std::path::PathBuf>> {
        let target = files::target_file(meta);
        if dry_run {
            let would = files::template_for(meta, dir).is_some() && !dir.join(&target).exists();
            return Ok(would.then(|| dir.join(target)));
        }
        files::generate_from_template(meta, dir, &target)
    };

    if let Some(path) = generate(meta, root)? {
        written.push(path);
    }

    // Only the members this run actually touches: generating files across a
    // whole monorepo because one package was built would be a surprise.
    let members: Vec<&str> = resolved
        .steps
        .iter()
        .filter_map(|step| step.workspace.as_deref())
        .collect();
    if members.is_empty() {
        return Ok(written);
    }

    // A workspace that can't be loaded is not a reason to fail a run that was
    // about to work: without it there is simply nothing extra to generate.
    let Ok(workspace) = crate::workspace::Workspace::discover(root) else {
        return Ok(written);
    };
    for member in &workspace.members {
        if !members.contains(&member.name.as_str()) {
            continue;
        }
        if let Some(path) = generate(&member.meta, &member.dir)? {
            written.push(path);
        }
    }

    Ok(written)
}

/// Check every tool the graph's steps declare in `requires` against `PATH`,
/// reporting all of them at once with whatever fix the project documented.
///
/// Runs before the first step, because "you are missing protoc, install it
/// with X" is worth knowing before a ten-minute compile, not after it.
async fn preflight_tools(
    resolved: &ResolvedRun,
    workflow: &str,
    root: &Path,
    dry_run: bool,
    tx: &mpsc::Sender<ProgressUpdate>,
) -> Result<()> {
    let mut missing: Vec<(String, Vec<String>)> = Vec::new();
    for step in &resolved.steps {
        for tool in &step.requires {
            if crate::workspace::tool_on_path(tool) {
                continue;
            }
            match missing.iter_mut().find(|(name, _)| name == tool) {
                Some((_, users)) => users.push(step.name.clone()),
                None => missing.push((tool.clone(), vec![step.name.clone()])),
            }
        }
    }
    if missing.is_empty() {
        return Ok(());
    }

    let hints = crate::workspace::toolchain_hints(root);
    let mut lines = vec![format!(
        "Missing {} build tool(s) this run needs:",
        missing.len()
    )];
    for (tool, users) in &missing {
        lines.push(format!("  • {tool} — required by {}", users.join(", ")));
        if let Some(hint) = hints.get(tool.as_str()) {
            lines.push(format!("    install it with: {hint}"));
        }
    }
    if dry_run {
        // A dry run is exactly where you want to hear about this without it
        // being fatal — nothing was going to execute anyway.
        for line in &lines {
            let _ = tx
                .send(ProgressUpdate::Log(
                    workflow.to_string(),
                    format!("[dry-run] {line}"),
                ))
                .await;
        }
        return Ok(());
    }
    for line in &lines {
        let _ = tx
            .send(ProgressUpdate::Log(workflow.to_string(), line.clone()))
            .await;
    }
    bail!(
        "{}\nInstall them, or document how in a [toolchain.<tool>] section.",
        lines.join("\n")
    );
}

/// Start a `persistent` step and return immediately, without waiting for it.
///
/// A `persistent` step is handed to the daemon as a watch session, which is
/// what collects its output: the process outlives the run, and can be tailed
/// live with `ciabatta watch --attach <id>`.
///
/// A background task is run here instead, beside the graph, with its output
/// streamed into its own node like any other step's. It exists only for this
/// run, so there is nothing for a watch session to outlive it for; the run
/// stops it on the way out (see [`stop_persistent`]).
///
/// If no daemon can be reached, a persistent step falls back to running here
/// too. It behaves the same from the graph's point of view; the difference is
/// that it stops with this run.
async fn start_persistent(
    step: &RunStep,
    workflow: &str,
    root: &Path,
    env_vars: &HashMap<String, String>,
    ctl: &RunCtl,
    tx: &mpsc::Sender<ProgressUpdate>,
) -> Result<Persistent> {
    let _ = tx
        .send(ProgressUpdate::StepStarted {
            workflow: workflow.to_string(),
            step: step.name.clone(),
        })
        .await;

    let cwd = step_cwd(step, root);
    let (script, run) = step.action();
    let command = shell_form(script, run.as_deref())
        .ok_or_else(|| anyhow::anyhow!("persistent step '{}' has no action", step.name))?;
    let env = step_env(step, env_vars);

    let log = |line: String| {
        let tx = tx.clone();
        let workflow = workflow.to_string();
        let name = step.name.clone();
        async move {
            let _ = tx
                .send(ProgressUpdate::StepLog {
                    workflow,
                    step: name,
                    line,
                })
                .await;
        }
    };

    // A background task lives and dies with this run, so it runs here, like
    // any other step: its output is its node's log, read where every other
    // step's is, rather than in a watch session somewhere else.
    if step.background {
        log(format!("$ {command}   (background — nothing waits for it)")).await;
        return Ok(Persistent {
            step: step.name.clone(),
            ownership: Ownership::Local(spawn_locally(&command, &cwd, env, step, workflow, tx)),
        });
    }

    log(format!("$ {command}   (persistent — the graph continues)")).await;

    // `Err` here covers both "the caller doesn't want a daemon" and "we tried
    // and couldn't", because the fallback is the same either way.
    let handoff = match ctl.persist_via_daemon {
        true => hand_to_daemon(step, root, &command, &env).await,
        false => Err(anyhow::anyhow!("daemon handoff is disabled for this run")),
    };

    match handoff {
        Ok((session, url)) => {
            log(format!(
                "handed to the ciabatta daemon as watch session #{session} — \
                 it outlives this run"
            ))
            .await;
            // Its output goes to the session from here on, not into this panel.
            // Saying so beats leaving an empty log to be puzzled over.
            log("its output is collected there, not below".to_string()).await;
            log(format!("follow it:  ciabatta watch --attach {session}")).await;
            log(format!("stop it:    ciabatta watch --stop {session}")).await;
            log(format!("live view:  {url}")).await;
            Ok(Persistent {
                step: step.name.clone(),
                ownership: Ownership::Daemon { session, url },
            })
        }
        Err(err) => {
            log(format!(
                "⚠ couldn't hand this to the daemon ({err}); running it here instead, \
                 so it stops with this run and its output is not kept"
            ))
            .await;
            Ok(Persistent {
                step: step.name.clone(),
                ownership: Ownership::Local(spawn_locally(&command, &cwd, env, step, workflow, tx)),
            })
        }
    }
}

/// Register the run's project with the daemon and start the step as a watch
/// session in its own sub-workspace, returning the session id and its page URL.
async fn hand_to_daemon(
    step: &RunStep,
    root: &Path,
    command: &str,
    env: &HashMap<String, String>,
) -> Result<(u64, String)> {
    // The project is the run's root, not the operator's cwd: for a monorepo
    // workflow those differ, and the session has to be scoped to the workspace
    // whose directories its `cwd` is relative to.
    let session = crate::daemon::connect_at(None, root).await?;

    let created: serde_json::Value = session
        .daemon
        .client()?
        .post(session.daemon.url("/api/watch/sessions"))
        .json(&serde_json::json!({
            "project": session.project.id,
            "command": command,
            // Named after the graph node, so a session found later is
            // identifiable as the step that left it behind.
            "label": step.name,
            "cwd": step.cwd,
            "env": env,
        }))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;

    let id = created["id"]
        .as_u64()
        .context("the daemon returned no session id")?;
    Ok((id, format!("{}/watch/{id}", session.daemon.base_url)))
}

/// Run a persistent step here, in a background task, when the daemon isn't
/// available. Spawned with `kill_on_drop` so aborting the task takes the
/// subprocess with it rather than orphaning it.
fn spawn_locally(
    command: &str,
    cwd: &Path,
    env: HashMap<String, String>,
    step: &RunStep,
    workflow: &str,
    tx: &mpsc::Sender<ProgressUpdate>,
) -> JoinHandle<()> {
    let (line_tx, forwarder) = step_log_stream(tx, workflow, &step.name);
    let command = command.to_string();
    let cwd = cwd.to_path_buf();
    let name = step.name.clone();
    tokio::spawn(async move {
        let mut log: Vec<String> = Vec::new();
        {
            let mut sink = LogSink::streaming(&mut log, line_tx);
            if let Err(err) =
                registry::run_shell_command_opts(&command, &cwd, &env, true, &mut sink).await
            {
                sink.push(format!("persistent step '{name}' ended: {err}"));
            }
        }
        let _ = forwarder.await;
    })
}

/// Close out everything the run started and left running.
///
/// A **background task** is scoped to this run: it was started so the steps
/// could finish, they have, and leaving it up would hand the operator a process
/// still holding its port for the next run to collide with. It runs here, so it
/// is stopped, and what it logged stays in its node.
///
/// A **`persistent` step** is the opposite promise: `ciabatta dev` exists to
/// leave a dev server to work against, and killing it on the way out would make
/// persistence pointless. The daemon owns it, it is left alone, and the id
/// needed to reach it later is reported.
///
/// Whatever runs here — a background task, or a persistent step whose daemon
/// couldn't be reached — has nothing to outlive this process, so it is stopped.
async fn stop_persistent(
    running: Vec<Persistent>,
    workflow: &str,
    tx: &mpsc::Sender<ProgressUpdate>,
) {
    for job in running {
        let line = match job.ownership {
            // A `persistent` step is the opposite promise: `ciabatta dev`
            // exists to leave a dev server behind, and killing it on the way
            // out would make persistence pointless. Left alone, and reported
            // with the id needed to reach it later.
            Ownership::Daemon { session, url } => Some(format!(
                "still running as watch session #{session} — {url}  \
                 (stop it with `ciabatta watch --stop {session}`)"
            )),
            Ownership::Local(handle) => {
                let was_running = !handle.is_finished();
                handle.abort();
                was_running.then(|| "stopped (the run finished)".to_string())
            }
        };

        if let Some(line) = line {
            let _ = tx
                .send(ProgressUpdate::StepLog {
                    workflow: workflow.to_string(),
                    step: job.step.clone(),
                    line,
                })
                .await;
        }
        let _ = tx
            .send(ProgressUpdate::StepFinished {
                workflow: workflow.to_string(),
                step: job.step,
                ok: true,
            })
            .await;
    }
}

/// The directory a step's action runs from: its own `cwd` (a sub-workspace's
/// directory, for a workflow graph) resolved against the project root, or the
/// root itself.
fn step_cwd(step: &RunStep, root: &Path) -> std::path::PathBuf {
    match step.cwd.as_deref() {
        Some(rel) => root.join(rel),
        None => root.to_path_buf(),
    }
}

/// The environment a step's action sees: the run's, with the step's own `env`
/// layered on top. A compiled workflow graph puts each sub-workspace's standard
/// variables there, so two members can set the same name to different values
/// without one of them silently winning.
///
/// It also names the step, so `ciabatta input` can say which step is asking.
fn step_env(step: &RunStep, env_vars: &HashMap<String, String>) -> HashMap<String, String> {
    let mut merged = env_vars.clone();
    for (key, value) in &step.env {
        merged.insert(key.clone(), value.clone());
    }
    merged.insert(super::input::STEP_VAR.to_string(), step.name.clone());
    merged
}

/// Render a step's action as a single shell command. A `script` becomes
/// `bash <path>` so both forms run through one code path — and so a script
/// picks up its step's `cwd` the same way an inline command does.
pub(crate) fn shell_form(script: Option<&str>, run: Option<&str>) -> Option<String> {
    match (script, run) {
        (Some(script), _) => Some(format!("bash {}", shell_quote(script))),
        (None, Some(cmd)) => Some(cmd.to_string()),
        (None, None) => None,
    }
}

/// Single-quote a path for `sh -c`, so spaces in a script path don't split it
/// into two arguments.
fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', r"'\''"))
}

/// Handle a failed step by entering its recovery node: pick a fix option
/// (interactively via the UI, or the `default` one when non-interactive), run
/// it, and either re-queue a `retry` target or clear the branch.
#[allow(clippy::too_many_arguments)]
async fn recover<'a>(
    resolved: &'a ResolvedRun,
    failed: &'a RunStep,
    target: &str,
    workflow: &str,
    root: &Path,
    env_vars: &HashMap<String, String>,
    dry_run: bool,
    ctl: &RunCtl,
    tx: &mpsc::Sender<ProgressUpdate>,
    state: &mut HashMap<&'a str, StepState>,
    attempts: &mut HashMap<&'a str, u32>,
) -> Result<()> {
    let node = resolved
        .step(target)
        .ok_or_else(|| anyhow::anyhow!("recovery node '{}' not found", target))?;

    let count = attempts.entry(failed.name.as_str()).or_insert(0);
    *count += 1;
    if *count > MAX_STEP_ATTEMPTS {
        bail!(
            "Run step '{}' still failing after {} recovery attempts; giving up.",
            failed.name,
            MAX_STEP_ATTEMPTS
        );
    }

    let labels: Vec<String> = node.options.iter().map(|o| o.label.clone()).collect();
    let message = node
        .message
        .clone()
        .unwrap_or_else(|| format!("Step '{}' failed — choose a fix:", failed.name));

    let choice = pick_option(node, workflow, &message, &labels, ctl, tx).await?;
    let option = node
        .options
        .get(choice)
        .ok_or_else(|| anyhow::anyhow!("recovery option {} out of range", choice))?;

    // Run the chosen fix as the recovery node's action.
    let _ = tx
        .send(ProgressUpdate::StepStarted {
            workflow: workflow.to_string(),
            step: node.name.clone(),
        })
        .await;
    let mut log: Vec<String> = Vec::new();
    let (line_tx, forwarder) = step_log_stream(tx, workflow, &node.name);
    let res = {
        let mut sink = LogSink::streaming(&mut log, line_tx);
        sink.push(format!("recover: {}", option.label));
        run_action(
            option.script.as_deref(),
            option.run.as_deref(),
            // A fix runs where the step it's fixing ran, so a per-sub-workspace
            // remedy ("re-run the codegen") lands in the right directory.
            &step_cwd(failed, root),
            env_vars,
            dry_run,
            None,
            ctl.cancel.as_ref(),
            &mut sink,
        )
        .await
    };
    let _ = forwarder.await;
    let fixed = res.is_ok();
    let _ = tx
        .send(ProgressUpdate::StepFinished {
            workflow: workflow.to_string(),
            step: node.name.clone(),
            ok: fixed,
        })
        .await;

    if let Err(e) = res {
        bail!(
            "Recovery '{}' for step '{}' failed: {}",
            option.label,
            failed.name,
            e
        );
    }

    state.insert(node.name.as_str(), StepState::Succeeded);

    // A retry re-queues the named step (usually the one that failed); otherwise
    // the failed branch is considered cleared so downstream steps can proceed.
    // Validation guarantees any `retry` target exists in the graph.
    match node.retry.as_deref() {
        Some(retry) => {
            if let Some(s) = resolved.steps.iter().find(|s| s.name == retry) {
                state.insert(s.name.as_str(), StepState::Pending);
            }
        }
        None => {
            state.insert(failed.name.as_str(), StepState::Recovered);
        }
    }
    Ok(())
}

/// Choose a recovery option. Interactive runs ask the UI and wait; non-interactive
/// runs auto-pick the first `default` option, or fail if none is marked.
async fn pick_option(
    node: &RunStep,
    workflow: &str,
    message: &str,
    labels: &[String],
    ctl: &RunCtl,
    tx: &mpsc::Sender<ProgressUpdate>,
) -> Result<usize> {
    if ctl.interactive
        && let Some(bus) = ctl.choices.as_ref()
    {
        // Subscribe BEFORE announcing, so a fast UI reply can't race ahead of us.
        let mut rx = bus.subscribe();
        let _ = tx
            .send(ProgressUpdate::StepNeedsChoice {
                workflow: workflow.to_string(),
                step: node.name.clone(),
                message: message.to_string(),
                options: labels.to_vec(),
            })
            .await;
        loop {
            match rx.recv().await {
                Ok(choice) if choice.workflow == workflow && choice.step == node.name => {
                    if choice.option < node.options.len() {
                        return Ok(choice.option);
                    }
                    // Out-of-range selection: ignore and keep waiting.
                }
                Ok(_) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                    bail!(
                        "Recovery for '{}' needs a choice but the UI channel closed.",
                        node.name
                    );
                }
            }
        }
    }

    // Non-interactive: the first option flagged `default` is the unattended fix.
    node.options.iter().position(|o| o.default).ok_or_else(|| {
        anyhow::anyhow!(
            "Recovery node '{}' needs an operator choice, but this run is non-interactive \
                 and no option is marked `default = true`. Options: {}.",
            node.name,
            labels.join(", ")
        )
    })
}

/// Run a normal step's action, emitting start/log/finish updates. Returns the
/// action's result so the caller can route failures to recovery.
///
/// Two guards wrap the action itself: a `timeout` that kills a step which has
/// stopped making progress, and `retries` for a step that fails in a way worth
/// trying again.
#[allow(clippy::too_many_arguments)]
async fn run_step_action(
    step: &RunStep,
    workflow: &str,
    config: &CiabattaConfig,
    root: &Path,
    env_vars: &HashMap<String, String>,
    dry_run: bool,
    // `isolated_in`: where to run instead of the step's real directory — the
    // sandbox, under `--authoritative`. `None` is the ordinary case.
    isolated_in: Option<&Path>,
    // The run's stop switch, raced against the action so a step already in
    // flight ends when the button is pressed rather than when it finishes.
    cancel: Option<&std::sync::Arc<crate::runner::Cancel>>,
    tx: &mpsc::Sender<ProgressUpdate>,
) -> Result<()> {
    let _ = tx
        .send(ProgressUpdate::StepStarted {
            workflow: workflow.to_string(),
            step: step.name.clone(),
        })
        .await;

    let limit = step.timeout_duration()?;
    let (script, run) = step.action();
    let cwd = match isolated_in {
        Some(dir) => dir.to_path_buf(),
        None => step_cwd(step, root),
    };
    let env_vars = &step_env(step, env_vars);
    let attempts = step.retries.saturating_add(1);
    let mut res = Ok(());

    for attempt in 1..=attempts {
        let mut log: Vec<String> = Vec::new();
        let (line_tx, forwarder) = step_log_stream(tx, workflow, &step.name);
        res = {
            let mut sink = LogSink::streaming(&mut log, line_tx);
            if attempt > 1 {
                sink.push(format!("retry {}/{}", attempt - 1, step.retries));
            }
            // A transfer step with no command of its own performs the built-in
            // registry move; one that names a `run`/`script` runs that instead,
            // which is how a project keeps its own publishing script while
            // still being a node on the graph.
            match step.transfer() {
                // A release step is the same idea pointed at the cache: the
                // built-in action, unless the step brings a command of its own.
                _ if step.release_direction().is_some() && script.is_none() && run.is_none() => {
                    let direction = step.release_direction().expect("checked above");
                    let action = crate::run::release::run_step(
                        step, direction, config, root, &cwd, env_vars, dry_run, &mut sink,
                    );
                    match limit {
                        None => action.await,
                        Some(limit) => match tokio::time::timeout(limit, action).await {
                            Ok(result) => result,
                            Err(_) => Err(anyhow::Error::new(TimedOut).context(format!(
                                "timed out after {} and was killed",
                                format_duration(limit)
                            ))),
                        },
                    }
                }
                Some(transfer) if script.is_none() && run.is_none() => {
                    let action = transfer::run(
                        step,
                        &transfer,
                        config,
                        root,
                        &cwd,
                        env_vars,
                        dry_run,
                        &mut sink,
                        |done, total| {
                            if total > 1 {
                                let _ = tx.try_send(ProgressUpdate::TransferProgress {
                                    workflow: workflow.to_string(),
                                    done,
                                    total,
                                });
                            }
                        },
                    );
                    match limit {
                        None => action.await,
                        Some(limit) => match tokio::time::timeout(limit, action).await {
                            Ok(result) => result,
                            Err(_) => Err(anyhow::Error::new(TimedOut).context(format!(
                                "timed out after {} and was killed",
                                format_duration(limit)
                            ))),
                        },
                    }
                }
                _ => {
                    run_action(
                        script,
                        run.as_deref(),
                        &cwd,
                        env_vars,
                        dry_run,
                        limit,
                        cancel,
                        &mut sink,
                    )
                    .await
                }
            }
        };
        // Flush all streamed lines into the UI before deciding what's next.
        let _ = forwarder.await;

        // A timeout means the step is stuck, not flaky — retrying just spends
        // the same wall-clock time over again.
        if res.is_ok() || res.as_ref().is_err_and(|e| e.is::<TimedOut>()) {
            break;
        }
    }

    let _ = tx
        .send(ProgressUpdate::StepFinished {
            workflow: workflow.to_string(),
            step: step.name.clone(),
            ok: res.is_ok(),
        })
        .await;
    res
}

/// What to tell someone whose run can't start for want of variables: which
/// ones, why each is needed, and every way there is to set them — with the
/// names filled in, so the fix can be copied rather than worked out.
fn missing_env_help(workflow: &str, prepared: &super::PreparedEnv) -> Vec<String> {
    let missing = prepared.missing();
    let width = missing.iter().map(String::len).max().unwrap_or(0);
    let mut lines = vec![format!(
        "✗ This run can't start: {} variable{} it needs {} not set:",
        missing.len(),
        if missing.len() == 1 { "" } else { "s" },
        if missing.len() == 1 { "is" } else { "are" },
    )];
    for var in &missing {
        let why = match prepared.unresolved_paths.contains(var) {
            true => "names the env_file to source",
            false => "listed in required_env",
        };
        lines.push(format!("    {var:<width$}  ({why})"));
    }
    lines.extend(crate::environment::files::how_to_set(
        workflow,
        &missing,
        "the workspace's .env (or .ciabatta/.env.<profile>, run with --env-profile <profile>)",
    ));
    lines
}

/// Run a step/option action from `cwd`: a bash `script` path or an inline `run`
/// shell command. Exactly one is expected (validation enforces it for steps;
/// recovery options may legitimately have neither, meaning "no-op").
///
/// `limit`, when set, caps how long the action may take; past it the child is
/// killed and the action fails with [`TimedOut`].
#[allow(clippy::too_many_arguments)]
async fn run_action(
    script: Option<&str>,
    run: Option<&str>,
    cwd: &Path,
    env_vars: &HashMap<String, String>,
    dry_run: bool,
    limit: Option<std::time::Duration>,
    cancel: Option<&std::sync::Arc<crate::runner::Cancel>>,
    sink: &mut LogSink<'_>,
) -> Result<()> {
    let Some(command) = shell_form(script, run) else {
        // A recovery option with no action: nothing to do (mark resolved).
        sink.push("(no action)".to_string());
        return Ok(());
    };

    match script {
        Some(path) => sink.push(format!("Running script: {}", cwd.join(path).display())),
        // One log line per line of the command: a multi-line `run:` echoed as
        // a single entry is one "line" with newlines in it, which the run
        // page's fixed-height log rows draw on top of each other.
        None => {
            for (i, line) in command.trim_end().lines().enumerate() {
                sink.push(match i {
                    0 => format!("$ {line}"),
                    _ => format!("  {line}"),
                });
            }
        }
    }
    if dry_run {
        sink.push(format!("[dry-run] would run: {command}"));
        return Ok(());
    }

    // `kill_on_drop` is what makes both the timeout and the stop button real:
    // dropping the future has to take the child with it, or a "killed" step
    // would keep running with nothing left watching it.
    let killable = limit.is_some() || cancel.is_some();
    let action = registry::run_shell_command_opts(&command, cwd, env_vars, killable, sink);

    let timed = async {
        match limit {
            None => action.await,
            Some(limit) => match tokio::time::timeout(limit, action).await {
                Ok(result) => result,
                Err(_) => Err(anyhow::Error::new(TimedOut).context(format!(
                    "timed out after {} and was killed",
                    format_duration(limit)
                ))),
            },
        }
    };

    let Some(cancel) = cancel else {
        return timed.await;
    };
    tokio::select! {
        result = timed => result,
        // Losing the race drops `timed`, and with it the child.
        () = cancel.stopped() => Err(anyhow::Error::new(Stopped)),
    }
}

/// Render a duration the way it was most likely written (`10m`, `1h30m`, `45s`),
/// for messages about a step that ran out of time.
pub(crate) fn format_duration(d: std::time::Duration) -> String {
    let secs = d.as_secs();
    let (h, m, s) = (secs / 3600, (secs % 3600) / 60, secs % 60);
    let mut out = String::new();
    if h > 0 {
        out.push_str(&format!("{h}h"));
    }
    if m > 0 {
        out.push_str(&format!("{m}m"));
    }
    if s > 0 || out.is_empty() {
        out.push_str(&format!("{s}s"));
    }
    out
}

/// Spawn a task that forwards each streamed output line as a step-scoped
/// `StepLog` update. Returns the line sender to feed into a streaming
/// [`LogSink`], plus the task handle: dropping the sender ends the task, and
/// awaiting the handle guarantees every line has been folded into the UI state.
fn step_log_stream(
    tx: &mpsc::Sender<ProgressUpdate>,
    workflow: &str,
    step: &str,
) -> (UnboundedSender<String>, JoinHandle<()>) {
    let (line_tx, mut line_rx) = mpsc::unbounded_channel::<String>();
    let tx = tx.clone();
    let workflow = workflow.to_string();
    let step = step.to_string();
    let handle = tokio::spawn(async move {
        while let Some(line) = line_rx.recv().await {
            let _ = tx
                .send(ProgressUpdate::StepLog {
                    workflow: workflow.clone(),
                    step: step.clone(),
                    line,
                })
                .await;
        }
    });
    (line_tx, handle)
}

/// Like [`step_log_stream`], but forwards lines as workflow-level `Log` updates
/// (used by the login/pre/post phase hooks, which aren't tied to a step).
fn recipe_log_stream(
    tx: &mpsc::Sender<ProgressUpdate>,
    workflow: &str,
) -> (UnboundedSender<String>, JoinHandle<()>) {
    let (line_tx, mut line_rx) = mpsc::unbounded_channel::<String>();
    let tx = tx.clone();
    let workflow = workflow.to_string();
    let handle = tokio::spawn(async move {
        while let Some(line) = line_rx.recv().await {
            let _ = tx.send(ProgressUpdate::Log(workflow.clone(), line)).await;
        }
    });
    (line_tx, handle)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::run::RunStep;

    #[test]
    fn a_run_missing_variables_says_which_and_how_to_set_them() {
        let prepared = crate::run::PreparedEnv {
            unresolved_paths: vec!["STAGE".into()],
            missing_required: vec!["API_TOKEN".into()],
            ..Default::default()
        };
        let help = missing_env_help("deploy", &prepared).join("\n");
        assert!(help.contains("2 variables it needs are not set"), "{help}");
        assert!(
            help.contains("STAGE      (names the env_file to source)"),
            "{help}"
        );
        assert!(
            help.contains("API_TOKEN  (listed in required_env)"),
            "{help}"
        );
        assert!(
            help.contains("ciabatta deploy ... --stage=… --api-token=…"),
            "{help}"
        );
        assert!(help.contains("export STAGE=… API_TOKEN=…"), "{help}");
    }

    /// Run a step DAG to completion, collecting every progress update.
    ///
    /// Non-interactive (no operator to answer a recovery prompt), which is the
    /// mode CI runs in and the one these behaviours have to hold up under.
    async fn drive(steps: Vec<RunStep>, root: &Path) -> (Result<()>, Vec<ProgressUpdate>) {
        drive_with(steps, root, None).await
    }

    /// [`drive`], with a stop switch the test can press.
    async fn drive_with(
        steps: Vec<RunStep>,
        root: &Path,
        cancel: Option<std::sync::Arc<crate::runner::Cancel>>,
    ) -> (Result<()>, Vec<ProgressUpdate>) {
        drive_jobs(steps, root, cancel, None).await
    }

    /// [`drive_with`], with a limit on how many steps run at once.
    async fn drive_jobs(
        steps: Vec<RunStep>,
        root: &Path,
        cancel: Option<std::sync::Arc<crate::runner::Cancel>>,
        jobs: Option<usize>,
    ) -> (Result<()>, Vec<ProgressUpdate>) {
        let resolved = ResolvedRun {
            steps,
            ..Default::default()
        };
        let (tx, mut rx) = mpsc::channel(256);
        let ctl = RunCtl {
            interactive: false,
            choices: None,
            inputs: None,
            // Tests must not reach for — let alone start — a real daemon, so
            // persistent steps take the in-process path here.
            persist_via_daemon: false,
            authoritative: false,
            sandbox_also: Vec::new(),
            force: false,
            cancel,
            jobs,
            start_anyway: false,
        };
        let env: HashMap<String, String> = std::env::vars().collect();

        let collector = tokio::spawn(async move {
            let mut updates = Vec::new();
            while let Some(update) = rx.recv().await {
                updates.push(update);
            }
            updates
        });

        let prepared = crate::run::PreparedEnv {
            env,
            ..Default::default()
        };
        let result = run_graph(
            &resolved,
            "test",
            &CiabattaConfig::default(),
            root,
            &prepared,
            false,
            &ctl,
            &tx,
            None,
            None,
        )
        .await;
        drop(tx);
        (result, collector.await.unwrap())
    }

    /// Which steps finished, and whether each succeeded.
    fn outcomes(updates: &[ProgressUpdate]) -> Vec<(String, bool)> {
        updates
            .iter()
            .filter_map(|u| match u {
                ProgressUpdate::StepFinished { step, ok, .. } => Some((step.clone(), *ok)),
                _ => None,
            })
            .collect()
    }

    fn skipped(updates: &[ProgressUpdate]) -> Vec<(String, String)> {
        updates
            .iter()
            .filter_map(|u| match u {
                ProgressUpdate::StepSkipped { step, reason, .. } => {
                    Some((step.clone(), reason.clone()))
                }
                _ => None,
            })
            .collect()
    }

    fn step(name: &str, command: &str) -> RunStep {
        RunStep {
            name: name.to_string(),
            run: Some(command.to_string()),
            ..Default::default()
        }
    }

    /// Two steps that need nothing from each other run at the same time.
    ///
    /// Each waits for the other to have started, so this only passes if they
    /// are genuinely in flight together: run one after the other, the first
    /// would wait for a sibling that never starts, and time out.
    #[tokio::test]
    async fn independent_steps_run_side_by_side() {
        let root = std::env::temp_dir().join(format!("ciab_parallel_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();

        let meet = |me: &str, other: &str| {
            let mut s = step(
                me,
                &format!(
                    "touch {me}.started; i=0; while [ ! -f {other}.started ]; do \
                     i=$((i+1)); [ $i -gt 100 ] && exit 1; sleep 0.05; done"
                ),
            );
            s.timeout = Some("20s".into());
            s
        };
        let mut after = step("after", "true");
        after.needs = vec!["left".into(), "right".into()];

        let (result, updates) = drive_jobs(
            vec![meet("left", "right"), meet("right", "left"), after],
            &root,
            None,
            Some(4),
        )
        .await;
        assert!(result.is_ok(), "{result:?}");
        let finished = outcomes(&updates);
        for name in ["left", "right", "after"] {
            assert!(finished.contains(&(name.to_string(), true)), "{finished:?}");
        }
        // `after` needs both, so it can only have started once both finished.
        let order: Vec<String> = updates
            .iter()
            .filter_map(|u| match u {
                ProgressUpdate::StepStarted { step, .. } => Some(format!("start {step}")),
                ProgressUpdate::StepFinished { step, .. } => Some(format!("end {step}")),
                _ => None,
            })
            .collect();
        let at = |what: &str| order.iter().position(|o| o == what).unwrap();
        assert!(at("start after") > at("end left") && at("start after") > at("end right"));
        // And the two siblings overlapped.
        assert!(at("start right") < at("end left") && at("start left") < at("end right"));

        let _ = std::fs::remove_dir_all(&root);
    }

    /// `-j 1` puts things back the way they were: one step at a time.
    #[tokio::test]
    async fn one_job_runs_one_step_at_a_time() {
        let root = std::env::temp_dir();
        let (result, updates) = drive_jobs(
            vec![step("a", "true"), step("b", "true"), step("c", "true")],
            &root,
            None,
            Some(1),
        )
        .await;
        assert!(result.is_ok());
        let mut running = 0;
        for update in &updates {
            match update {
                ProgressUpdate::StepStarted { .. } => {
                    running += 1;
                    assert_eq!(running, 1, "two steps were running at once under -j 1");
                }
                ProgressUpdate::StepFinished { .. } => running -= 1,
                _ => {}
            }
        }
    }

    /// A failure ends the run, but not the sibling already running beside it —
    /// that one finishes, and nothing new starts.
    #[tokio::test]
    async fn a_failure_lets_running_siblings_finish_and_starts_nothing_new() {
        let root = std::env::temp_dir();
        let mut slow = step("slow", "sleep 0.5");
        slow.timeout = Some("20s".into());
        let mut behind = step("behind", "true");
        behind.needs = vec!["slow".into()];

        let (result, updates) = drive_jobs(
            vec![step("broken", "exit 3"), slow, behind],
            &root,
            None,
            Some(4),
        )
        .await;

        assert!(result.is_err());
        let finished = outcomes(&updates);
        assert!(finished.contains(&("broken".to_string(), false)));
        assert!(
            finished.contains(&("slow".to_string(), true)),
            "a sibling in flight is allowed to finish: {finished:?}"
        );
        assert!(
            skipped(&updates).iter().any(|(s, _)| s == "behind"),
            "nothing new starts once the run has failed"
        );
    }

    #[tokio::test]
    async fn a_timed_out_step_is_killed_and_the_rest_of_the_graph_finishes() {
        // `hangs` never exits on its own. Its own dependent must be reported as
        // blocked, while the unrelated branch runs to completion — the whole
        // point of a timeout rather than a stuck graph.
        let steps = vec![
            RunStep {
                timeout: Some("1s".into()),
                ..step("hangs", "sleep 30")
            },
            RunStep {
                needs: vec!["hangs".into()],
                ..step("downstream", "echo nope")
            },
            step("independent", "echo yes"),
        ];

        let started = std::time::Instant::now();
        let (result, updates) = drive(steps, Path::new(".")).await;

        // Cut loose promptly, not after the full 30 seconds.
        assert!(started.elapsed() < std::time::Duration::from_secs(10));

        let err = result.unwrap_err().to_string();
        assert!(err.contains("hangs"), "{err}");
        assert!(err.contains("timed out"), "{err}");

        let finished = outcomes(&updates);
        assert!(finished.contains(&("hangs".to_string(), false)));
        assert!(finished.contains(&("independent".to_string(), true)));
        // The dependent never ran, and says why.
        assert!(!finished.iter().any(|(name, _)| name == "downstream"));
        assert_eq!(
            skipped(&updates),
            vec![(
                "downstream".to_string(),
                "blocked by a failed dependency".to_string()
            )]
        );
    }

    #[tokio::test]
    async fn retries_give_a_flaky_step_another_chance() {
        let marker = std::env::temp_dir().join(format!("ciab_retry_{}", std::process::id()));
        let _ = std::fs::remove_file(&marker);
        let path = marker.to_string_lossy().to_string();

        // Fails the first time, succeeds the second.
        let steps = vec![RunStep {
            retries: 2,
            ..step(
                "flaky",
                &format!("if [ -f '{path}' ]; then echo ok; else touch '{path}'; exit 1; fi"),
            )
        }];

        let (result, updates) = drive(steps, Path::new(".")).await;
        assert!(result.is_ok(), "{:?}", result.err());
        // One StepFinished, reporting success — the retry is internal to the step.
        assert_eq!(outcomes(&updates), vec![("flaky".to_string(), true)]);

        let _ = std::fs::remove_file(&marker);
    }

    #[tokio::test]
    async fn retries_are_exhausted_rather_than_looping_forever() {
        let steps = vec![RunStep {
            retries: 1,
            ..step("doomed", "exit 3")
        }];
        let (result, _) = drive(steps, Path::new(".")).await;
        assert!(result.unwrap_err().to_string().contains("doomed"));
    }

    #[tokio::test]
    async fn continue_on_error_keeps_other_branches_running() {
        let steps = vec![
            RunStep {
                continue_on_error: true,
                ..step("optional", "exit 1")
            },
            step("required", "echo ran"),
        ];
        let (result, updates) = drive(steps, Path::new(".")).await;

        // The run still fails — tolerating a failure isn't hiding it.
        let err = result.unwrap_err().to_string();
        assert!(err.contains("1 failed step(s)"), "{err}");
        assert!(err.contains("optional"), "{err}");
        assert!(outcomes(&updates).contains(&("required".to_string(), true)));
    }

    #[tokio::test]
    async fn warn_on_error_reports_the_failure_and_runs_its_dependents() {
        let steps = vec![
            RunStep {
                warn_on_error: true,
                ..step("lint", "exit 1")
            },
            RunStep {
                needs: vec!["lint".into()],
                ..step("package", "echo ran")
            },
        ];
        let (result, updates) = drive(steps, Path::new(".")).await;

        // A warning, not a failure: the run succeeds...
        assert!(result.is_ok(), "{:?}", result.err());
        // ...the step behind it still ran...
        assert!(outcomes(&updates).contains(&("package".to_string(), true)));
        // ...and the failure was still said out loud, as a warning.
        assert!(outcomes(&updates).contains(&("lint".to_string(), false)));
        assert!(updates.iter().any(|u| matches!(
            u,
            ProgressUpdate::StepWarned { step, .. } if step == "lint"
        )));
    }

    #[tokio::test]
    async fn a_persistent_step_releases_its_dependents_instead_of_hanging_them() {
        let steps = vec![
            RunStep {
                persistent: true,
                ..step("server", "while true; do sleep 1; done")
            },
            RunStep {
                needs: vec!["server".into()],
                ..step("probe", "echo talked-to-it")
            },
        ];

        let started = std::time::Instant::now();
        let (result, updates) = drive(steps, Path::new(".")).await;

        assert!(result.is_ok(), "{:?}", result.err());
        // Nothing waited for a loop that never ends.
        assert!(started.elapsed() < std::time::Duration::from_secs(10));
        assert!(outcomes(&updates).contains(&("probe".to_string(), true)));
        // The persistent step is closed out rather than left spinning.
        assert!(outcomes(&updates).contains(&("server".to_string(), true)));
    }

    #[tokio::test]
    async fn without_a_daemon_a_persistent_step_says_it_cannot_outlive_the_run() {
        // A `persistent` step is meant to outlive the run, and the fallback
        // cannot deliver that: there is no daemon to hand ownership to, so it
        // dies with this process. Silently doing that would be worse than not
        // persisting at all.
        let steps = vec![RunStep {
            persistent: true,
            ..step("server", "while true; do sleep 1; done")
        }];
        let (result, updates) = drive(steps, Path::new(".")).await;
        assert!(result.is_ok(), "{:?}", result.err());

        let logs = step_logs(&updates);
        assert!(logs.contains("couldn't hand this to the daemon"), "{logs}");
        assert!(logs.contains("stops with this run"), "{logs}");
        assert!(logs.contains("stopped (the run finished)"), "{logs}");
        // And nothing is left behind.
        assert!(!logs.contains("watch session"), "{logs}");
    }

    /// The lifetime split, which is the whole point of the two kinds existing.
    ///
    /// Both take the local fallback here (no daemon in tests), and the fallback
    /// stops everything — so what this pins is the *reporting*: the background
    /// task is stopped because it is a background task, and the persistent step
    /// is stopped only because there was no daemon to hand it to.
    #[tokio::test]
    async fn a_background_task_and_a_persistent_step_say_different_things() {
        let steps = vec![
            RunStep {
                persistent: true,
                background: true,
                ..step("mock-api", "while true; do sleep 1; done")
            },
            RunStep {
                persistent: true,
                ..step("dev-server", "while true; do sleep 1; done")
            },
            step("compile", "echo built"),
        ];
        let (result, updates) = drive(steps, Path::new(".")).await;
        assert!(result.is_ok(), "{:?}", result.err());

        let logs = step_logs(&updates);
        // The persistent step is told it can't outlive this run, because
        // without a daemon it can't. The background task is never promised
        // that in the first place.
        assert!(logs.contains("stops with this run"), "{logs}");
        assert!(outcomes(&updates).contains(&("compile".to_string(), true)));
    }

    /// A background task is up before anything else runs — not merely somewhere
    /// in wave 1, which is where the scheduler would otherwise have put it.
    #[tokio::test]
    async fn a_background_task_starts_before_the_first_wave() {
        // Declared *after* the step, so declaration order can't be what makes
        // this pass.
        let steps = vec![
            step("compile", "echo built"),
            RunStep {
                persistent: true,
                background: true,
                ..step("mock-api", "while true; do sleep 1; done")
            },
        ];
        let (result, updates) = drive(steps, Path::new(".")).await;
        assert!(result.is_ok(), "{:?}", result.err());

        let order: Vec<&str> = updates
            .iter()
            .filter_map(|u| match u {
                ProgressUpdate::StepStarted { step, .. } => Some(step.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(
            order.first(),
            Some(&"mock-api"),
            "the background task has to be up first: {order:?}"
        );
    }

    /// A background task runs inside the run, so what it prints lands in its
    /// own node like any other step's output — not in a watch session.
    #[tokio::test]
    async fn a_background_task_logs_into_its_own_node() {
        let steps = vec![
            RunStep {
                persistent: true,
                background: true,
                ..step(
                    "mock-api",
                    "echo mock listening; while true; do sleep 1; done",
                )
            },
            // Long enough for the task's first line to arrive.
            step("compile", "sleep 0.5; echo built"),
        ];
        let (result, updates) = drive(steps, Path::new(".")).await;
        assert!(result.is_ok(), "{:?}", result.err());

        let mock: Vec<&str> = updates
            .iter()
            .filter_map(|u| match u {
                ProgressUpdate::StepLog { step, line, .. } if step == "mock-api" => {
                    Some(line.as_str())
                }
                _ => None,
            })
            .collect();
        assert!(mock.contains(&"mock listening"), "{mock:?}");
        assert!(
            !mock.iter().any(|l| l.contains("watch session")),
            "{mock:?}"
        );
        assert!(
            !mock
                .iter()
                .any(|l| l.contains("couldn't hand this to the daemon")),
            "a background task never goes to the daemon, so there is nothing to warn about: {mock:?}"
        );
    }

    fn after(name: &str, command: &str, needs: &[&str]) -> RunStep {
        RunStep {
            after: true,
            needs: needs.iter().map(|n| n.to_string()).collect(),
            ..step(name, command)
        }
    }

    /// An after action runs last, and is told the run went well.
    #[tokio::test]
    async fn an_after_action_runs_once_the_graph_is_done() {
        let steps = vec![
            step("compile", "echo built"),
            after(
                "teardown",
                "echo \"outcome=$CIABATTA_RUN_OUTCOME\"",
                &["compile"],
            ),
        ];
        let (result, updates) = drive(steps, Path::new(".")).await;
        assert!(result.is_ok(), "{:?}", result.err());
        let order: Vec<String> = outcomes(&updates).into_iter().map(|(s, _)| s).collect();
        assert_eq!(order, ["compile", "teardown"]);
        assert!(step_logs(&updates).contains("outcome=succeeded"));
    }

    /// The point of an after action: a failure upstream doesn't leave it
    /// blocked, and the run still fails for the step that broke.
    #[tokio::test]
    async fn an_after_action_runs_when_the_graph_fails() {
        let steps = vec![
            step("compile", "exit 3"),
            step("package", "echo never"),
            after(
                "teardown",
                "echo \"outcome=$CIABATTA_RUN_OUTCOME\"",
                &["package"],
            ),
        ];
        let steps = {
            let mut steps = steps;
            steps[1].needs = vec!["compile".into()];
            steps
        };
        let (result, updates) = drive(steps, Path::new(".")).await;
        let err = result.expect_err("the run still fails").to_string();
        assert!(
            err.contains("compile"),
            "the graph's failure is reported: {err}"
        );
        assert!(outcomes(&updates).contains(&("teardown".to_string(), true)));
        assert!(step_logs(&updates).contains("outcome=failed"));
    }

    /// An after action that fails fails a run that had otherwise passed.
    #[tokio::test]
    async fn a_failing_after_action_fails_the_run() {
        let steps = vec![
            step("compile", "echo built"),
            after("teardown", "exit 1", &["compile"]),
        ];
        let (result, _) = drive(steps, Path::new(".")).await;
        let err = format!("{:#}", result.expect_err("the after action failed"));
        assert!(err.contains("after action"), "{err}");
    }

    /// Stopping means start nothing further, clean-up included.
    #[tokio::test]
    async fn a_stopped_run_skips_its_after_actions() {
        let cancel = std::sync::Arc::new(crate::runner::Cancel::default());
        cancel.stop();
        let steps = vec![
            step("compile", "echo built"),
            after("teardown", "echo ran", &["compile"]),
        ];
        let (result, updates) = drive_with(steps, Path::new("."), Some(cancel)).await;
        assert!(result.is_err());
        assert!(
            skipped(&updates)
                .iter()
                .any(|(s, why)| s == "teardown" && why.contains("stopped")),
            "{:?}",
            skipped(&updates)
        );
        assert!(!step_logs(&updates).contains("ran"));
    }

    /// Asking a run to stop ends it without running what was left.
    #[tokio::test]
    async fn a_stopped_run_skips_what_it_had_not_started() {
        let cancel = std::sync::Arc::new(crate::runner::Cancel::default());
        // Already stopped when the engine starts, which is the same state the
        // engine sees when the button is pressed a moment in.
        cancel.stop();

        let steps = vec![step("first", "echo one"), step("second", "echo two")];
        let (result, updates) = drive_with(steps, Path::new("."), Some(cancel)).await;

        let err = result.expect_err("a stopped run does not report success");
        assert!(err.to_string().contains("stopped"), "{err}");
        assert!(
            outcomes(&updates).is_empty(),
            "nothing should have run: {:?}",
            outcomes(&updates)
        );
    }

    /// A stopped run still closes out the background tasks it started —
    /// otherwise stopping would leave exactly the mess it exists to prevent.
    #[tokio::test]
    async fn a_stopped_run_still_stops_its_background_tasks() {
        let cancel = std::sync::Arc::new(crate::runner::Cancel::default());
        let steps = vec![
            RunStep {
                persistent: true,
                background: true,
                ..step("mock-api", "while true; do sleep 1; done")
            },
            // Long enough that the stop below lands while it is in flight.
            step("slow", "sleep 30"),
        ];

        let armed = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(400)).await;
            armed.stop();
        });

        let started = std::time::Instant::now();
        let (result, updates) = drive_with(steps, Path::new("."), Some(cancel)).await;

        assert!(result.is_err(), "a stopped run does not report success");
        assert!(
            started.elapsed() < std::time::Duration::from_secs(20),
            "the step in flight has to be cut short, not waited out"
        );
        assert!(
            step_logs(&updates).contains("stopped (the run finished)"),
            "the background task must be closed out: {}",
            step_logs(&updates)
        );
    }

    #[tokio::test]
    async fn a_background_step_never_gates_what_comes_after_it() {
        // The property the whole feature rests on. `probe` declares it needs a
        // task that never exits, and still runs — if background steps were
        // waited on like any other, this test would hang until the harness
        // killed it rather than fail.
        let steps = vec![
            RunStep {
                persistent: true,
                ..step("server", "while true; do sleep 1; done")
            },
            RunStep {
                needs: vec!["server".to_string()],
                ..step("probe", "echo reached")
            },
        ];
        let started = std::time::Instant::now();
        let (result, updates) = drive(steps, Path::new(".")).await;

        assert!(result.is_ok(), "{:?}", result.err());
        assert!(
            outcomes(&updates).contains(&("probe".to_string(), true)),
            "the step behind a background task has to run"
        );
        assert!(started.elapsed() < std::time::Duration::from_secs(10));
        // And the task is closed out rather than left spinning.
        assert!(step_logs(&updates).contains("stopped (the run finished)"));
    }

    /// Every step log line the run emitted, joined.
    fn step_logs(updates: &[ProgressUpdate]) -> String {
        updates
            .iter()
            .filter_map(|u| match u {
                ProgressUpdate::StepLog { line, .. } => Some(line.clone()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[tokio::test]
    async fn a_missing_required_tool_stops_the_run_before_anything_executes() {
        let steps = vec![RunStep {
            requires: vec!["ciabatta-no-such-tool".into()],
            ..step("build", "echo should-not-run")
        }];
        let (result, updates) = drive(steps, Path::new(".")).await;

        let err = result.unwrap_err().to_string();
        assert!(err.contains("ciabatta-no-such-tool"), "{err}");
        assert!(err.contains("required by build"), "{err}");
        // Nothing ran.
        assert!(outcomes(&updates).is_empty());
    }

    #[tokio::test]
    async fn a_step_runs_in_its_own_cwd_with_its_own_env() {
        let root = std::env::temp_dir().join(format!("ciab_cwd_{}", std::process::id()));
        let nested = root.join("packages/api");
        std::fs::create_dir_all(&nested).unwrap();

        let steps = vec![RunStep {
            cwd: Some("packages/api".into()),
            env: [("STEP_ONLY".to_string(), "yes".to_string())]
                .into_iter()
                .collect(),
            ..step("here", "pwd && echo \"env=$STEP_ONLY\"")
        }];
        let (result, updates) = drive(steps, &root).await;
        assert!(result.is_ok(), "{:?}", result.err());

        let logs: String = updates
            .iter()
            .filter_map(|u| match u {
                ProgressUpdate::StepLog { line, .. } => Some(line.clone()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(logs.contains("packages/api"), "{logs}");
        assert!(logs.contains("env=yes"), "{logs}");

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn durations_are_formatted_the_way_they_were_written() {
        use std::time::Duration;
        assert_eq!(format_duration(Duration::from_secs(45)), "45s");
        assert_eq!(format_duration(Duration::from_secs(600)), "10m");
        assert_eq!(format_duration(Duration::from_secs(5400)), "1h30m");
        assert_eq!(format_duration(Duration::ZERO), "0s");
    }

    #[test]
    fn a_script_action_is_quoted_so_a_spaced_path_stays_one_argument() {
        assert_eq!(
            shell_form(Some("scripts/my build.sh"), None).unwrap(),
            "bash 'scripts/my build.sh'"
        );
        assert_eq!(shell_form(None, Some("make test")).unwrap(), "make test");
        assert!(shell_form(None, None).is_none());
    }
}
