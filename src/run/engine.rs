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
    Failed,
}

impl StepState {
    fn satisfied(self) -> bool {
        matches!(
            self,
            StepState::Succeeded | StepState::Recovered | StepState::Skipped | StepState::Started
        )
    }
}

/// How a step's attempt ended, for the failure summary at the end of a run.
struct StepFailure {
    step: String,
    /// The failure as reported, already phrased for an operator.
    detail: String,
}

/// A started `persistent` step, and what now owns it.
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
    // whole flowchart on `REQUIRED_ENV`. The daemon's launcher runs the very
    // same check before it spawns us, so it can prompt for what's missing
    // instead of starting a run that would only abort here.
    // The `.env` rules, before anything is sourced:
    //   * a build that declares REQUIRED_ENV must say where those variables are
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

    // With the files in place, fold the project's own env files into the run's
    // — `.env` by default, or whatever `env_file` names instead. A compiled
    // workflow's members don't come in here: each of their steps carries its
    // own chain, so one member's settings can't be read by another's steps.
    let mut resolved = resolved;
    let workspace_files = crate::environment::files::resolve(&meta, root);
    for file in workspace_files.files {
        if !resolved.env_files.contains(&file) {
            resolved.env_files.insert(0, file);
        }
    }
    let resolved = resolved;

    let prepared = prepare_env(&resolved, root, env_vars)?;
    for file in &prepared.sourced {
        let _ = tx
            .send(ProgressUpdate::Log(
                name.to_string(),
                format!("Sourcing env file: {file}"),
            ))
            .await;
    }

    if !prepared.is_ready() {
        let list = prepared.missing().join(", ");
        // Console: printed directly so it shows even in `--gui` mode, where
        // progress updates are folded into the browser view rather than stdout.
        eprintln!("[{name}] ✗ run aborted — env variable(s) empty or unset: {list}");
        // GUI: emit a log line per missing variable into the workflow's log panel.
        let _ = tx
            .send(ProgressUpdate::Log(
                name.to_string(),
                format!("✗ Run aborted before starting — missing env variable(s): {list}"),
            ))
            .await;
        for var in &prepared.unresolved_paths {
            let _ = tx
                .send(ProgressUpdate::Log(
                    name.to_string(),
                    format!("  • {var} is unset, so the env_file to source can't be resolved"),
                ))
                .await;
        }
        for var in &prepared.missing_required {
            let _ = tx
                .send(ProgressUpdate::Log(
                    name.to_string(),
                    format!("  • {var} (REQUIRED_ENV) is empty or unset"),
                ))
                .await;
        }
        // Returning Err becomes a `Failed` update (shown as the workflow's error in
        // the GUI, and on stderr by the plain runner).
        bail!(
            "Run '{name}' cannot start: env variable(s) empty or unset: {list}. \
             Set them (with -e {list}=…, or see REQUIRED_ENV in the flowchart) and retry."
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
                let mut cache = if dry_run {
                    None
                } else {
                    let mut session = super::cached::Session::open(root, config);
                    if let Some(session) = session.as_mut() {
                        session.connect_remote().await;
                    }
                    session
                };

                run_dag(
                    &resolved,
                    name,
                    config,
                    root,
                    &prepared,
                    dry_run,
                    ctl,
                    tx,
                    cache.as_mut(),
                )
                .await?;

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

/// Execute the step DAG. Runs steps whose `needs` are satisfied, one wave at a
/// time; on a step failure, routes to its `on_error` recovery node.
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

    loop {
        // A step is ready when it is Pending, not a recovery node, and all its
        // `needs` are satisfied. Recovery nodes are only entered via on_error.
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

        if ready.is_empty() {
            break;
        }

        // Run this wave sequentially. Run steps are ordered, side-effecting
        // shell work (build → migrate → release); serial execution keeps their
        // logs readable and recovery prompts unambiguous.
        for step in ready {
            // A `when`/`skip_if` condition can exclude the step; if so, mark it
            // satisfied (so dependents proceed) and move on without running it.
            // Every one of these reads the step's *own* environment: its
            // workspace's `.env` first, then outward. Two members of a
            // monorepo can set the same variable to different values and
            // each of their steps sees its own.
            let step_env = env.for_step(&step.name);

            if let Some(reason) = super::step_skip_reason(step, step_env)? {
                state.insert(step.name.as_str(), StepState::Skipped);
                let _ = tx
                    .send(ProgressUpdate::StepSkipped {
                        workflow: workflow.to_string(),
                        step: step.name.clone(),
                        reason,
                    })
                    .await;
                continue;
            }

            // A persistent step is started and left running: the graph moves on
            // without it, so a dev server can't hang everything behind it.
            if step.persistent && !dry_run {
                persistent.push(start_persistent(step, workflow, root, step_env, ctl, tx).await?);
                state.insert(step.name.as_str(), StepState::Started);
                continue;
            }

            // Ask the cache before doing the work. A hit restores this step's
            // declared outputs and marks it satisfied, so everything downstream
            // proceeds exactly as if it had run.
            let mut pending = None;
            if let Some(session) = cache.as_deref_mut() {
                match session.before(step, step_env).await {
                    Ok(super::cached::Action::Skip { note }) => {
                        state.insert(step.name.as_str(), StepState::Skipped);
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
                    // A cache that can't decide costs a rebuild, never a build.
                    Err(e) => {
                        let _ = tx
                            .send(ProgressUpdate::Log(
                                workflow.to_string(),
                                format!("note: skipping the cache for {} ({e:#})", step.name),
                            ))
                            .await;
                    }
                }
            }

            let started = std::time::Instant::now();
            let outcome =
                run_step_action(step, workflow, config, root, step_env, dry_run, tx).await;
            match outcome {
                Ok(()) => {
                    state.insert(step.name.as_str(), StepState::Succeeded);
                    // Only a step that actually succeeded is worth keeping.
                    if let (Some(session), Some(token)) = (cache.as_deref_mut(), pending) {
                        session
                            .after(step, token, started.elapsed().as_millis() as u64)
                            .await;
                    }
                }
                Err(err) => {
                    state.insert(step.name.as_str(), StepState::Failed);
                    // It ran and left the tree in a state nothing recorded —
                    // whatever runs on past this, by recovery or by tolerance,
                    // can't be served from the cache behind it.
                    if let Some(session) = cache.as_deref_mut() {
                        session.mark_unaccounted(&step.name);
                    }

                    // A recovery route takes precedence: it exists to put the
                    // run back on the rails rather than write the failure off.
                    if let Some(target) = step.on_error.as_deref() {
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

                    // A tolerated failure takes this branch out of the graph
                    // and lets every independent branch finish; the run still
                    // ends up failing, but with the full picture.
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

                    // Nothing to fall back on: this failure ends the run.
                    stop_persistent(persistent, workflow, tx).await;
                    bail!("Run step '{}' failed: {}", step.name, err);
                }
            }
        }
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
/// The step is handed to the daemon as a watch session, which is what makes it
/// genuinely persistent: the daemon owns the process, so a dev server started
/// by `ciabatta build` is still up — and still collecting output — after the
/// build finishes and the terminal closes. The session id is reported so it can
/// be tailed with `ciabatta watch --attach <id>` or stopped with `--stop <id>`.
///
/// If no daemon can be reached, the step falls back to a background task inside
/// this process. It still doesn't block the graph, but it can't outlive the run
/// — which the log says plainly rather than leaving it to be discovered.
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
                "handed to the ciabatta daemon as watch session #{session} — it outlives this run"
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
                 so it will stop when the run does"
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

/// Close out every persistent step now that the graph is done.
///
/// Daemon-owned sessions are deliberately left running — outliving the run is
/// the whole point — and are only reported, with the id needed to reach them
/// later. Local fallbacks have nothing left to own them once this process goes
/// away, so they're stopped here rather than orphaned.
///
/// Either way the node stops showing as in-flight: a graph that leaves a step
/// spinning forever after the run has finished is just misleading.
async fn stop_persistent(
    running: Vec<Persistent>,
    workflow: &str,
    tx: &mpsc::Sender<ProgressUpdate>,
) {
    for job in running {
        let line = match job.ownership {
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
fn step_env(step: &RunStep, env_vars: &HashMap<String, String>) -> HashMap<String, String> {
    if step.env.is_empty() {
        return env_vars.clone();
    }
    let mut merged = env_vars.clone();
    for (key, value) in &step.env {
        merged.insert(key.clone(), value.clone());
    }
    merged
}

/// Render a step's action as a single shell command. A `script` becomes
/// `bash <path>` so both forms run through one code path — and so a script
/// picks up its step's `cwd` the same way an inline command does.
fn shell_form(script: Option<&str>, run: Option<&str>) -> Option<String> {
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
async fn run_step_action(
    step: &RunStep,
    workflow: &str,
    config: &CiabattaConfig,
    root: &Path,
    env_vars: &HashMap<String, String>,
    dry_run: bool,
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
    let cwd = step_cwd(step, root);
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

/// Run a step/option action from `cwd`: a bash `script` path or an inline `run`
/// shell command. Exactly one is expected (validation enforces it for steps;
/// recovery options may legitimately have neither, meaning "no-op").
///
/// `limit`, when set, caps how long the action may take; past it the child is
/// killed and the action fails with [`TimedOut`].
async fn run_action(
    script: Option<&str>,
    run: Option<&str>,
    cwd: &Path,
    env_vars: &HashMap<String, String>,
    dry_run: bool,
    limit: Option<std::time::Duration>,
    sink: &mut LogSink<'_>,
) -> Result<()> {
    let Some(command) = shell_form(script, run) else {
        // A recovery option with no action: nothing to do (mark resolved).
        sink.push("(no action)".to_string());
        return Ok(());
    };

    match script {
        Some(path) => sink.push(format!("Running script: {}", cwd.join(path).display())),
        None => sink.push(format!("$ {command}")),
    }
    if dry_run {
        sink.push(format!("[dry-run] would run: {command}"));
        return Ok(());
    }

    // `kill_on_drop` is what makes the timeout real: dropping the future has to
    // take the stuck child with it, or the "killed" step would keep running.
    let action = registry::run_shell_command_opts(&command, cwd, env_vars, limit.is_some(), sink);
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

/// Render a duration the way it was most likely written (`10m`, `1h30m`, `45s`),
/// for messages about a step that ran out of time.
fn format_duration(d: std::time::Duration) -> String {
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

    /// Run a step DAG to completion, collecting every progress update.
    ///
    /// Non-interactive (no operator to answer a recovery prompt), which is the
    /// mode CI runs in and the one these behaviours have to hold up under.
    async fn drive(steps: Vec<RunStep>, root: &Path) -> (Result<()>, Vec<ProgressUpdate>) {
        let resolved = ResolvedRun {
            steps,
            ..Default::default()
        };
        let (tx, mut rx) = mpsc::channel(256);
        let ctl = RunCtl {
            interactive: false,
            choices: None,
            // Tests must not reach for — let alone start — a real daemon, so
            // persistent steps take the in-process path here.
            persist_via_daemon: false,
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
        let result = run_dag(
            &resolved,
            "test",
            &CiabattaConfig::default(),
            root,
            &prepared,
            false,
            &ctl,
            &tx,
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
    async fn without_a_daemon_a_persistent_step_says_it_will_not_outlive_the_run() {
        // The fallback path has to be honest about what it gives up: the step
        // still doesn't block the graph, but it dies with the run, and silently
        // doing that would be worse than not persisting at all.
        let steps = vec![RunStep {
            persistent: true,
            ..step("server", "while true; do sleep 1; done")
        }];
        let (result, updates) = drive(steps, Path::new(".")).await;
        assert!(result.is_ok(), "{:?}", result.err());

        let logs = step_logs(&updates);
        assert!(logs.contains("couldn't hand this to the daemon"), "{logs}");
        assert!(logs.contains("stop when the run does"), "{logs}");
        assert!(logs.contains("stopped (the run finished)"), "{logs}");
        // And nothing is left behind.
        assert!(!logs.contains("watch session"), "{logs}");
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
