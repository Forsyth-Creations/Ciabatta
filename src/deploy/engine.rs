//! The deploy DAG engine: drives a resolved flowchart through the four deploy
//! phases (`login → pre → deploy → post`), where the `deploy` phase executes the
//! step graph — running ready steps, and on failure routing to `on_error`
//! recovery nodes that offer a choice of fix scripts.

use std::collections::HashMap;
use std::path::Path;

use anyhow::{Result, bail};
use tokio::sync::mpsc::{self, UnboundedSender};
use tokio::task::JoinHandle;

use crate::config::CiabattaConfig;
use crate::registry::{self, LogSink};
use crate::runner::{DeployCtl, ProgressUpdate, StageKind};

use super::{DeployStep, ResolvedDeploy, resolve_deploy};

/// How many times a single step may be re-run through recovery before the deploy
/// gives up — bounds retry loops so a persistently failing step can't spin forever.
const MAX_STEP_ATTEMPTS: u32 = 20;

/// Return the names from `required` that are absent from `env_vars` or present
/// but empty (after trimming). An empty result means every required variable is
/// set, so the deploy may proceed. Order follows `required` so the reported list
/// matches how the operator declared `REQUIRED_ENV`.
fn missing_required_env(required: &[String], env_vars: &HashMap<String, String>) -> Vec<String> {
    required
        .iter()
        .filter(|key| {
            env_vars
                .get(key.as_str())
                .map(|v| v.trim().is_empty())
                .unwrap_or(true)
        })
        .cloned()
        .collect()
}

/// Whether a step counts as "satisfied" for the purposes of its dependents.
#[derive(Clone, Copy, PartialEq, Eq)]
enum StepState {
    Pending,
    Succeeded,
    /// A fix ran and the branch was cleared without a retry — treated as
    /// satisfied so downstream steps proceed.
    Recovered,
    Failed,
}

impl StepState {
    fn satisfied(self) -> bool {
        matches!(self, StepState::Succeeded | StepState::Recovered)
    }
}

/// Entry point for `RunMode::Deploy`, called from `runner::run_one`. Resolves the
/// recipe's flowchart, then runs the four deploy phases.
pub async fn run_deploy(
    name: &str,
    config: &CiabattaConfig,
    root: &Path,
    env_vars: &HashMap<String, String>,
    dry_run: bool,
    ctl: &DeployCtl,
    tx: &mpsc::Sender<ProgressUpdate>,
) -> Result<()> {
    let entry = config
        .recipes
        .get(name)
        .ok_or_else(|| anyhow::anyhow!("Recipe '{}' not found", name))?;
    let deploy = entry
        .deploy_recipe()
        .ok_or_else(|| anyhow::anyhow!("Recipe '{}' has no [deploy] definition", name))?;
    let resolved = resolve_deploy(deploy, name, root)?;

    // Gate the whole flowchart on `REQUIRED_ENV`: if any required variable is
    // empty or unset, abort before running a single phase, surfacing the missing
    // names to both the console and the deploy GUI.
    let missing = missing_required_env(&resolved.required_env, env_vars);
    if !missing.is_empty() {
        let list = missing.join(", ");
        // Console: printed directly so it shows even in `--gui` mode, where
        // progress updates are folded into the browser view rather than stdout.
        eprintln!(
            "[{name}] ✗ deploy aborted — required env variable(s) empty or unset: {list}"
        );
        // GUI: emit a log line per missing variable into the recipe's log panel.
        let _ = tx
            .send(ProgressUpdate::Log(
                name.to_string(),
                format!("✗ Deploy aborted before running — missing required env variable(s): {list}"),
            ))
            .await;
        for var in &missing {
            let _ = tx
                .send(ProgressUpdate::Log(
                    name.to_string(),
                    format!("  • {var} is empty or unset"),
                ))
                .await;
        }
        // Returning Err becomes a `Failed` update (shown as the recipe's error in
        // the GUI, and on stderr by the plain runner).
        bail!(
            "Deploy '{name}' cannot run: required env variable(s) empty or unset: {list}. \
             Set them (see REQUIRED_ENV in the flowchart) and retry."
        );
    }

    for stage in StageKind::ALL {
        let _ = tx
            .send(ProgressUpdate::StageStarted {
                recipe: name.to_string(),
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
                run_dag(&resolved, name, root, env_vars, dry_run, ctl, tx).await?;
                true
            }
            StageKind::Post => {
                run_phase_hook(resolved.post.as_deref(), name, root, env_vars, dry_run, tx).await?
            }
        };

        let _ = tx
            .send(ProgressUpdate::StageFinished {
                recipe: name.to_string(),
                stage,
                ran,
            })
            .await;
    }

    Ok(())
}

/// Run an optional phase hook (login/pre/post) as a shell command, forwarding its
/// output as recipe log lines. Returns whether a command actually ran.
async fn run_phase_hook(
    cmd: Option<&str>,
    recipe: &str,
    root: &Path,
    env_vars: &HashMap<String, String>,
    dry_run: bool,
    tx: &mpsc::Sender<ProgressUpdate>,
) -> Result<bool> {
    let Some(cmd) = cmd else { return Ok(false) };
    let mut log: Vec<String> = Vec::new();
    let (line_tx, forwarder) = recipe_log_stream(tx, recipe);
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
async fn run_dag(
    resolved: &ResolvedDeploy,
    recipe: &str,
    root: &Path,
    env_vars: &HashMap<String, String>,
    dry_run: bool,
    ctl: &DeployCtl,
    tx: &mpsc::Sender<ProgressUpdate>,
) -> Result<()> {
    let mut state: HashMap<&str, StepState> = resolved
        .steps
        .iter()
        .map(|s| (s.name.as_str(), StepState::Pending))
        .collect();
    let mut attempts: HashMap<&str, u32> = HashMap::new();

    loop {
        // A step is ready when it is Pending, not a recovery node, and all its
        // `needs` are satisfied. Recovery nodes are only entered via on_error.
        let ready: Vec<&DeployStep> = resolved
            .steps
            .iter()
            .filter(|s| !s.recover)
            .filter(|s| state.get(s.name.as_str()) == Some(&StepState::Pending))
            .filter(|s| {
                s.needs
                    .iter()
                    .all(|dep| state.get(dep.as_str()).map(|st| st.satisfied()).unwrap_or(false))
            })
            .collect();

        if ready.is_empty() {
            break;
        }

        // Run this wave sequentially. Deploy steps are ordered, side-effecting
        // shell work (build → migrate → release); serial execution keeps their
        // logs readable and recovery prompts unambiguous.
        for step in ready {
            let outcome = run_step_action(step, recipe, root, env_vars, dry_run, tx).await;
            match outcome {
                Ok(()) => {
                    state.insert(step.name.as_str(), StepState::Succeeded);
                }
                Err(err) => {
                    state.insert(step.name.as_str(), StepState::Failed);
                    // No recovery route → the whole deploy fails here.
                    let Some(target) = step.on_error.as_deref() else {
                        bail!("Deploy step '{}' failed: {}", step.name, err);
                    };
                    recover(
                        resolved,
                        step,
                        target,
                        recipe,
                        root,
                        env_vars,
                        dry_run,
                        ctl,
                        tx,
                        &mut state,
                        &mut attempts,
                    )
                    .await?;
                }
            }
        }
    }

    // Any step still Failed with no path forward means the deploy didn't complete.
    if let Some(failed) = resolved
        .steps
        .iter()
        .find(|s| state.get(s.name.as_str()) == Some(&StepState::Failed))
    {
        bail!(
            "Deploy did not complete: step '{}' failed and was not recovered.",
            failed.name
        );
    }
    Ok(())
}

/// Handle a failed step by entering its recovery node: pick a fix option
/// (interactively via the UI, or the `default` one when non-interactive), run
/// it, and either re-queue a `retry` target or clear the branch.
#[allow(clippy::too_many_arguments)]
async fn recover<'a>(
    resolved: &'a ResolvedDeploy,
    failed: &'a DeployStep,
    target: &str,
    recipe: &str,
    root: &Path,
    env_vars: &HashMap<String, String>,
    dry_run: bool,
    ctl: &DeployCtl,
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
            "Deploy step '{}' still failing after {} recovery attempts; giving up.",
            failed.name,
            MAX_STEP_ATTEMPTS
        );
    }

    let labels: Vec<String> = node.options.iter().map(|o| o.label.clone()).collect();
    let message = node
        .message
        .clone()
        .unwrap_or_else(|| format!("Step '{}' failed — choose a fix:", failed.name));

    let choice = pick_option(node, recipe, &message, &labels, ctl, tx).await?;
    let option = node
        .options
        .get(choice)
        .ok_or_else(|| anyhow::anyhow!("recovery option {} out of range", choice))?;

    // Run the chosen fix as the recovery node's action.
    let _ = tx
        .send(ProgressUpdate::StepStarted {
            recipe: recipe.to_string(),
            step: node.name.clone(),
        })
        .await;
    let mut log: Vec<String> = Vec::new();
    let (line_tx, forwarder) = step_log_stream(tx, recipe, &node.name);
    let res = {
        let mut sink = LogSink::streaming(&mut log, line_tx);
        sink.push(format!("recover: {}", option.label));
        run_action(
            option.script.as_deref(),
            option.run.as_deref(),
            root,
            env_vars,
            dry_run,
            &mut sink,
        )
        .await
    };
    let _ = forwarder.await;
    let fixed = res.is_ok();
    let _ = tx
        .send(ProgressUpdate::StepFinished {
            recipe: recipe.to_string(),
            step: node.name.clone(),
            ok: fixed,
        })
        .await;

    if let Err(e) = res {
        bail!("Recovery '{}' for step '{}' failed: {}", option.label, failed.name, e);
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
    node: &DeployStep,
    recipe: &str,
    message: &str,
    labels: &[String],
    ctl: &DeployCtl,
    tx: &mpsc::Sender<ProgressUpdate>,
) -> Result<usize> {
    if ctl.interactive
        && let Some(bus) = ctl.choices.as_ref()
    {
        // Subscribe BEFORE announcing, so a fast UI reply can't race ahead of us.
        let mut rx = bus.subscribe();
        let _ = tx
            .send(ProgressUpdate::StepNeedsChoice {
                recipe: recipe.to_string(),
                step: node.name.clone(),
                message: message.to_string(),
                options: labels.to_vec(),
            })
            .await;
        loop {
            match rx.recv().await {
                Ok(choice)
                    if choice.recipe == recipe && choice.step == node.name =>
                {
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
    node.options
        .iter()
        .position(|o| o.default)
        .ok_or_else(|| {
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
async fn run_step_action(
    step: &DeployStep,
    recipe: &str,
    root: &Path,
    env_vars: &HashMap<String, String>,
    dry_run: bool,
    tx: &mpsc::Sender<ProgressUpdate>,
) -> Result<()> {
    let _ = tx
        .send(ProgressUpdate::StepStarted {
            recipe: recipe.to_string(),
            step: step.name.clone(),
        })
        .await;

    let mut log: Vec<String> = Vec::new();
    let (line_tx, forwarder) = step_log_stream(tx, recipe, &step.name);
    let res = {
        let mut sink = LogSink::streaming(&mut log, line_tx);
        run_action(
            step.script.as_deref(),
            step.run.as_deref(),
            root,
            env_vars,
            dry_run,
            &mut sink,
        )
        .await
    };
    // Flush all streamed lines into the UI before reporting the step's outcome.
    let _ = forwarder.await;

    let _ = tx
        .send(ProgressUpdate::StepFinished {
            recipe: recipe.to_string(),
            step: step.name.clone(),
            ok: res.is_ok(),
        })
        .await;
    res
}

/// Run a step/option action: a bash `script` path (relative to root) or an inline
/// `run` shell command. Exactly one is expected (validation enforces it for
/// steps; recovery options may legitimately have neither, meaning "no-op").
async fn run_action(
    script: Option<&str>,
    run: Option<&str>,
    root: &Path,
    env_vars: &HashMap<String, String>,
    dry_run: bool,
    sink: &mut LogSink<'_>,
) -> Result<()> {
    match (script, run) {
        (Some(script), _) => {
            let path = root.join(script);
            sink.push(format!("Running script: {}", path.display()));
            if dry_run {
                sink.push(format!("[dry-run] would run: bash {}", path.display()));
                return Ok(());
            }
            registry::run_script(&path.to_string_lossy(), env_vars, sink).await
        }
        (None, Some(cmd)) => {
            sink.push(format!("$ {cmd}"));
            if dry_run {
                sink.push(format!("[dry-run] would run: {cmd}"));
                return Ok(());
            }
            registry::run_shell_command(cmd, root, env_vars, sink).await
        }
        (None, None) => {
            // A recovery option with no action: nothing to do (mark resolved).
            sink.push("(no action)".to_string());
            Ok(())
        }
    }
}

/// Spawn a task that forwards each streamed output line as a step-scoped
/// `StepLog` update. Returns the line sender to feed into a streaming
/// [`LogSink`], plus the task handle: dropping the sender ends the task, and
/// awaiting the handle guarantees every line has been folded into the UI state.
fn step_log_stream(
    tx: &mpsc::Sender<ProgressUpdate>,
    recipe: &str,
    step: &str,
) -> (UnboundedSender<String>, JoinHandle<()>) {
    let (line_tx, mut line_rx) = mpsc::unbounded_channel::<String>();
    let tx = tx.clone();
    let recipe = recipe.to_string();
    let step = step.to_string();
    let handle = tokio::spawn(async move {
        while let Some(line) = line_rx.recv().await {
            let _ = tx
                .send(ProgressUpdate::StepLog {
                    recipe: recipe.clone(),
                    step: step.clone(),
                    line,
                })
                .await;
        }
    });
    (line_tx, handle)
}

/// Like [`step_log_stream`], but forwards lines as recipe-level `Log` updates
/// (used by the login/pre/post phase hooks, which aren't tied to a step).
fn recipe_log_stream(
    tx: &mpsc::Sender<ProgressUpdate>,
    recipe: &str,
) -> (UnboundedSender<String>, JoinHandle<()>) {
    let (line_tx, mut line_rx) = mpsc::unbounded_channel::<String>();
    let tx = tx.clone();
    let recipe = recipe.to_string();
    let handle = tokio::spawn(async move {
        while let Some(line) = line_rx.recv().await {
            let _ = tx
                .send(ProgressUpdate::Log(recipe.clone(), line))
                .await;
        }
    });
    (line_tx, handle)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[test]
    fn missing_required_env_flags_unset_and_empty_only() {
        let required = vec!["API_TOKEN".to_string(), "REGION".to_string(), "STAGE".to_string()];
        // API_TOKEN set, REGION empty, STAGE absent → REGION + STAGE missing.
        let missing = missing_required_env(&required, &env(&[("API_TOKEN", "abc"), ("REGION", "  ")]));
        assert_eq!(missing, vec!["REGION".to_string(), "STAGE".to_string()]);
    }

    #[test]
    fn missing_required_env_empty_when_all_set() {
        let required = vec!["A".to_string(), "B".to_string()];
        assert!(missing_required_env(&required, &env(&[("A", "1"), ("B", "2")])).is_empty());
        // No requirements → never missing.
        assert!(missing_required_env(&[], &env(&[])).is_empty());
    }
}
