use anyhow::{Context, Result, bail};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tokio::sync::mpsc;

use crate::config::{
    CiabattaConfig, PublishPath, RegistryConfig, SimpleRecipe, substitute_vars,
    validate_publish_path,
};
use crate::registry::{self, RegistryOpOptions};

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

    /// Full, mode-aware label, e.g. "pre-push" / "post-pull".
    pub fn label(self, mode: RunMode) -> &'static str {
        match (self, mode) {
            (StageKind::Login, _) => "login",
            (StageKind::Pre, RunMode::Push) => "pre-push",
            (StageKind::Pre, RunMode::Pull) => "pre-pull",
            (StageKind::Main, RunMode::Push) => "push",
            (StageKind::Main, RunMode::Pull) => "pull",
            (StageKind::Post, RunMode::Push) => "post-push",
            (StageKind::Post, RunMode::Pull) => "post-pull",
        }
    }

    /// Compact label for cramped UI (drops the push/pull suffix on pre/post).
    pub fn short(self, mode: RunMode) -> &'static str {
        match (self, mode) {
            (StageKind::Login, _) => "login",
            (StageKind::Pre, _) => "pre",
            (StageKind::Main, RunMode::Push) => "push",
            (StageKind::Main, RunMode::Pull) => "pull",
            (StageKind::Post, _) => "post",
        }
    }
}

#[derive(Debug, Clone)]
pub enum ProgressUpdate {
    Started(String),
    StageStarted {
        recipe: String,
        stage: StageKind,
    },
    /// A stage finished successfully. `ran` is false when it fell through to a
    /// default no-op (nothing to do), true when it actually executed something.
    StageFinished {
        recipe: String,
        stage: StageKind,
        ran: bool,
    },
    /// Progress within a multi-file main stage: `done` of `total` files have
    /// been transferred. Only emitted when a recipe transfers more than one file
    /// (a list-form `publish_path`).
    TransferProgress {
        recipe: String,
        done: usize,
        total: usize,
    },
    Log(String, String),
    Completed(String),
    Failed(String, String),
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum RunMode {
    Push,
    Pull,
}

/// Pre-flight validation: all publish-path vars must be set.
pub fn validate_recipes(
    config: &CiabattaConfig,
    recipe_names: &[String],
    env_vars: &HashMap<String, String>,
    mode: &RunMode,
) -> Result<()> {
    for name in recipe_names {
        let entry = config
            .recipes
            .get(name)
            .ok_or_else(|| anyhow::anyhow!("Recipe '{}' not found in config", name))?;

        let recipe = match mode {
            RunMode::Push => entry.push_recipe(),
            RunMode::Pull => entry
                .pull_recipe()
                .ok_or_else(|| anyhow::anyhow!("Recipe '{}' has no pull action defined", name))?,
        };

        // Only the built-in main action consumes publish_path; if the user
        // overrode `main` (or uses a bash_script) the placeholders are theirs.
        if recipe.main.is_none() && recipe.bash_script.is_none() {
            match recipe.publish_path.as_ref() {
                // A single remote path: every {VAR} must be resolvable.
                Some(PublishPath::Single(path)) => validate_publish_path(path, env_vars)?,
                // A list of globs publishes under {CIABATTA_PATH}, so that must
                // be derivable (from CIABATTA_TAG, or BRANCH + COMMIT).
                Some(PublishPath::Many(_))
                    if env_vars
                        .get("CIABATTA_PATH")
                        .filter(|v| !v.is_empty())
                        .is_none() =>
                {
                    bail!(
                        "Recipe '{}' uses a list-form publish_path, which uploads under \
                         CIABATTA_PATH, but CIABATTA_PATH is not set. It derives from \
                         CIABATTA_TAG, or from CIABATTA_BRANCH + CIABATTA_COMMIT; \
                         provide one via your CI system or with -e.",
                        name
                    );
                }
                Some(PublishPath::Many(_)) => {}
                None => {}
            }
        }
    }
    Ok(())
}

pub async fn run_all(
    config: &CiabattaConfig,
    root: &Path,
    recipe_names: &[String],
    env_vars: &HashMap<String, String>,
    dry_run: bool,
    mode: RunMode,
    tx: mpsc::Sender<ProgressUpdate>,
) -> Result<()> {
    let mut handles = Vec::new();
    for name in recipe_names {
        let name = name.clone();
        let config = config.clone();
        let root = root.to_path_buf();
        let env_vars = env_vars.clone();
        let tx = tx.clone();

        let handle = tokio::spawn(async move {
            run_one(name, &config, &root, &env_vars, dry_run, mode, tx).await
        });
        handles.push(handle);
    }

    for handle in handles {
        handle.await??;
    }

    Ok(())
}

async fn run_one(
    name: String,
    config: &CiabattaConfig,
    root: &Path,
    env_vars: &HashMap<String, String>,
    dry_run: bool,
    mode: RunMode,
    tx: mpsc::Sender<ProgressUpdate>,
) -> Result<()> {
    let _ = tx.send(ProgressUpdate::Started(name.clone())).await;
    tracing::debug!(
        recipe = %name,
        mode = if mode == RunMode::Push { "push" } else { "pull" },
        dry_run,
        "starting recipe"
    );

    let result = execute_recipe(&name, config, root, env_vars, dry_run, mode, &tx).await;

    match result {
        Ok(()) => {
            let _ = tx.send(ProgressUpdate::Completed(name)).await;
        }
        Err(ref e) => {
            let _ = tx.send(ProgressUpdate::Failed(name, e.to_string())).await;
        }
    }

    result
}

/// Drive a single recipe through its four-stage pipeline.
async fn execute_recipe(
    name: &str,
    config: &CiabattaConfig,
    root: &Path,
    env_vars: &HashMap<String, String>,
    dry_run: bool,
    mode: RunMode,
    tx: &mpsc::Sender<ProgressUpdate>,
) -> Result<()> {
    let entry = config
        .recipes
        .get(name)
        .ok_or_else(|| anyhow::anyhow!("Recipe '{}' not found", name))?;

    let recipe: SimpleRecipe = match mode {
        RunMode::Push => entry.push_recipe(),
        RunMode::Pull => entry
            .pull_recipe()
            .ok_or_else(|| anyhow::anyhow!("Recipe '{}' has no pull action", name))?,
    };

    // Resolve the registry (if any) and the artifact paths once up front, so the
    // login and main stages share them.
    let registry_config = match recipe.registry.as_deref() {
        Some(rn) => Some(
            config
                .registries
                .get(rn)
                .ok_or_else(|| anyhow::anyhow!("Registry '{}' not found in config", rn))?,
        ),
        None => None,
    };

    let container_cmd = config
        .system
        .as_ref()
        .and_then(|s| s.containers.as_deref())
        .unwrap_or("docker");

    // On a pull, if the exact commit's artifact is missing, fall back to the
    // newest commit on the branch that has one (works in both local and CI
    // mode; it just needs the branch's git history to be available). Any
    // override produces an adjusted variable map the transfers resolve against.
    let overridden;
    let env_vars: &HashMap<String, String> = if mode == RunMode::Pull {
        match resolve_pull_commit(&recipe, registry_config, root, container_cmd, env_vars, name, tx)
            .await
        {
            Some(adjusted) => {
                overridden = adjusted;
                &overridden
            }
            None => env_vars,
        }
    } else {
        env_vars
    };

    // Resolve the (local file, remote path) pairs this recipe transfers. A
    // single publish_path yields one pair; a list of globs yields one per
    // matched file. Owned here so the borrowed `opts` below can reference them.
    let transfers = build_transfers(&recipe, root, env_vars)?;

    let opts: Vec<RegistryOpOptions> = match registry_config {
        Some(rc) => transfers
            .iter()
            .map(|(local, remote)| RegistryOpOptions {
                registry_name: recipe.registry.as_deref().unwrap_or_default(),
                registry_config: rc,
                local_path: local.as_path(),
                remote_path: remote.as_str(),
                env_vars,
                dry_run,
                container_cmd,
            })
            .collect(),
        None => Vec::new(),
    };

    tracing::debug!(
        recipe = %name,
        transfers = transfers.len(),
        registry = recipe.registry.as_deref().unwrap_or("-"),
        "resolved recipe transfers"
    );

    for stage in StageKind::ALL {
        tracing::debug!(recipe = %name, stage = stage.label(mode), "running stage");
        let _ = tx
            .send(ProgressUpdate::StageStarted {
                recipe: name.to_string(),
                stage,
            })
            .await;

        let mut log: Vec<String> = Vec::new();
        let result = run_stage(
            stage,
            name,
            &recipe,
            opts.as_slice(),
            root,
            env_vars,
            dry_run,
            mode,
            &mut log,
            tx,
        )
        .await;

        for line in &log {
            let _ = tx
                .send(ProgressUpdate::Log(name.to_string(), line.clone()))
                .await;
        }

        match result {
            Ok(ran) => {
                let _ = tx
                    .send(ProgressUpdate::StageFinished {
                        recipe: name.to_string(),
                        stage,
                        ran,
                    })
                    .await;
            }
            // The Failed update (with the current stage) is emitted by run_one.
            Err(e) => return Err(e),
        }
    }

    Ok(())
}

/// Execute a single stage. Returns `Ok(true)` if it actually ran a command,
/// `Ok(false)` if it fell through to a default no-op.
#[allow(clippy::too_many_arguments)]
async fn run_stage(
    stage: StageKind,
    name: &str,
    recipe: &SimpleRecipe,
    opts: &[RegistryOpOptions<'_>],
    root: &Path,
    env_vars: &HashMap<String, String>,
    dry_run: bool,
    mode: RunMode,
    log: &mut Vec<String>,
    tx: &mpsc::Sender<ProgressUpdate>,
) -> Result<bool> {
    match stage {
        StageKind::Login => {
            if let Some(cmd) = recipe.login.as_deref() {
                run_shell(cmd, root, env_vars, dry_run, log).await?;
                Ok(true)
            } else if let Some(opts) = opts.first() {
                // Authentication is per-registry, so the first transfer's opts
                // are representative of them all.
                if let Some(script) = opts.registry_config.login_script.as_deref() {
                    run_login_script(script, root, env_vars, dry_run, log).await?;
                    Ok(true)
                } else {
                    registry::default_login(opts, log).await
                }
            } else {
                Ok(false)
            }
        }
        StageKind::Pre => run_optional(recipe.pre.as_deref(), root, env_vars, dry_run, log).await,
        StageKind::Main => {
            if let Some(cmd) = recipe.main.as_deref() {
                run_shell(cmd, root, env_vars, dry_run, log).await?;
                Ok(true)
            } else if let Some(script) = recipe.bash_script.as_deref() {
                run_bash_script(script, root, env_vars, dry_run, log).await?;
                Ok(true)
            } else if !opts.is_empty() {
                // One built-in transfer per resolved (local, remote) pair. When
                // there's more than one file, report file-level progress so the
                // TUI can show a "done/total" counter as each upload completes.
                //
                // Transfers run concurrently (bounded by MAX_CONCURRENT_TRANSFERS)
                // so a directory of many files — each its own `aws s3 cp` /
                // upload — is no longer bottlenecked on serial, one-at-a-time
                // process spawns. Per-transfer logs are buffered separately and
                // merged back in the original order once everything settles, so
                // the log stays readable despite out-of-order completion.
                use futures::stream::StreamExt;

                let total = opts.len();
                let report = |done: usize| {
                    if total > 1 {
                        let _ = tx.try_send(ProgressUpdate::TransferProgress {
                            recipe: name.to_string(),
                            done,
                            total,
                        });
                    }
                };
                report(0);

                let mut futs = Vec::with_capacity(total);
                for (i, o) in opts.iter().enumerate() {
                    futs.push(run_transfer(i, o, mode));
                }
                let mut stream =
                    futures::stream::iter(futs).buffer_unordered(MAX_CONCURRENT_TRANSFERS);

                // Collect per-transfer output keyed by index so it can be
                // replayed in the original order, regardless of which upload
                // finished first.
                let mut sublogs: Vec<Option<Vec<String>>> = (0..total).map(|_| None).collect();
                let mut done = 0;
                let mut first_err: Option<anyhow::Error> = None;
                while let Some((i, res, sublog)) = stream.next().await {
                    sublogs[i] = Some(sublog);
                    done += 1;
                    report(done);
                    if let Err(e) = res {
                        first_err = Some(e);
                        // Stop launching new transfers; in-flight ones are
                        // dropped (cancelled) as the stream unwinds.
                        break;
                    }
                }
                drop(stream);

                for sublog in sublogs.into_iter().flatten() {
                    log.extend(sublog);
                }

                if let Some(e) = first_err {
                    return Err(e);
                }
                Ok(true)
            } else {
                bail!(
                    "Recipe '{}' has no push/pull action. Define a registry + publish_path, \
                     a bash_script, or a `main` command.",
                    name
                )
            }
        }
        StageKind::Post => run_optional(recipe.post.as_deref(), root, env_vars, dry_run, log).await,
    }
}

/// How many file transfers within a single recipe's main stage may run at once.
/// Bounds concurrent `aws s3 cp` / upload processes so a large directory pushes
/// in parallel without spawning an unbounded number of subprocesses.
const MAX_CONCURRENT_TRANSFERS: usize = 40;

/// Run a single push/pull transfer into its own log buffer, tagged with its
/// original index so callers can merge concurrent results back into order.
async fn run_transfer(
    i: usize,
    o: &RegistryOpOptions<'_>,
    mode: RunMode,
) -> (usize, Result<()>, Vec<String>) {
    let mut sublog: Vec<String> = Vec::new();
    let res = match mode {
        RunMode::Push => registry::push(o, &mut sublog).await,
        RunMode::Pull => registry::pull(o, &mut sublog).await,
    };
    (i, res, sublog)
}

/// The most commits to probe when walking a branch's history for a published
/// artifact — bounds the number of existence requests on large repositories.
const MAX_PULL_CANDIDATES: usize = 50;

/// Candidate commits to probe for a pull, newest first: the exact commit,
/// then the branch's history. Tries the local branch ref, then `origin/<branch>`,
/// then `HEAD` — covering CI's detached-HEAD checkouts — and stops at the first
/// ref that yields history. Bounded to [`MAX_PULL_CANDIDATES`].
fn branch_candidates(root: &Path, branch: &str, exact: &str) -> Vec<String> {
    let mut candidates = vec![exact.to_string()];
    let mut seen: std::collections::HashSet<String> =
        std::collections::HashSet::from([exact.to_string()]);

    let origin = format!("origin/{branch}");
    for refname in [branch, origin.as_str(), "HEAD"] {
        let Ok(history) = crate::git::branch_commits(root, refname, MAX_PULL_CANDIDATES) else {
            continue;
        };
        for c in history {
            if seen.insert(c.clone()) {
                candidates.push(c);
            }
        }
        // Got usable history from this ref; don't also fold in the others.
        if candidates.len() > 1 {
            break;
        }
    }
    candidates.truncate(MAX_PULL_CANDIDATES);
    candidates
}

/// On a pull, pick the best commit for the branch: keep the exact commit when
/// its artifact exists, otherwise walk the branch history (newest first) and use
/// the most recent commit that does. Returns an adjusted variable map when a
/// different commit was chosen, or `None` to keep the current one.
///
/// Works in both local and CI mode. It only applies to a single `publish_path`
/// that references `{CIABATTA_COMMIT}` on a registry we can cheaply probe (HTTP),
/// and needs the branch's git history to be available (a normal CI checkout has
/// it). Network errors leave the commit unchanged so the pull surfaces them.
async fn resolve_pull_commit(
    recipe: &SimpleRecipe,
    registry_config: Option<&RegistryConfig>,
    root: &Path,
    container_cmd: &str,
    env_vars: &HashMap<String, String>,
    recipe_name: &str,
    tx: &mpsc::Sender<ProgressUpdate>,
) -> Option<HashMap<String, String>> {
    let reg_cfg = registry_config?;
    let reg_name = recipe.registry.as_deref()?;
    let branch = env_vars.get("CIABATTA_BRANCH").filter(|v| !v.is_empty())?;
    let exact = env_vars
        .get("CIABATTA_COMMIT")
        .filter(|v| !v.is_empty())?
        .clone();
    let Some(PublishPath::Single(template)) = recipe.publish_path.as_ref() else {
        return None;
    };
    if !template.contains("{CIABATTA_COMMIT}") {
        return None;
    }

    let candidates = branch_candidates(root, branch, &exact);
    for commit in &candidates {
        let mut trial = env_vars.clone();
        set_commit(&mut trial, commit);
        let Ok(remote) = substitute_vars(template, &trial) else {
            continue;
        };
        let opts = RegistryOpOptions {
            registry_name: reg_name,
            registry_config: reg_cfg,
            local_path: Path::new(""),
            remote_path: &remote,
            env_vars: &trial,
            dry_run: false,
            container_cmd,
        };
        match registry::exists(&opts).await {
            // Exact commit exists → keep it (no override).
            Ok(Some(true)) if *commit == exact => return None,
            Ok(Some(true)) => {
                let _ = tx
                    .send(ProgressUpdate::Log(
                        recipe_name.to_string(),
                        format!(
                            "commit {} has no artifact; pulling newest match on {}: {}",
                            short_sha(&exact),
                            branch,
                            short_sha(commit),
                        ),
                    ))
                    .await;
                return Some(trial);
            }
            Ok(Some(false)) => continue,
            // Registry can't be probed, or a network error occurred: don't
            // override — let the pull run against the exact commit.
            Ok(None) | Err(_) => return None,
        }
    }
    None
}

/// Override `CIABATTA_COMMIT` in a variable map, keeping the derived
/// `CIABATTA_PATH` consistent when it isn't tag-based.
fn set_commit(vars: &mut HashMap<String, String>, commit: &str) {
    vars.insert("CIABATTA_COMMIT".to_string(), commit.to_string());
    let has_tag = vars
        .get("CIABATTA_TAG")
        .map(|t| !t.is_empty())
        .unwrap_or(false);
    if !has_tag
        && let Some(branch) = vars.get("CIABATTA_BRANCH").cloned()
        && !branch.is_empty()
    {
        vars.insert("CIABATTA_PATH".to_string(), format!("/{branch}/{commit}"));
    }
}

/// First 8 characters of a commit SHA (or the whole thing if shorter).
fn short_sha(sha: &str) -> &str {
    sha.get(..8).unwrap_or(sha)
}

/// Resolve the (local file, remote path) pairs a recipe transfers in its main
/// stage.
///
///   - `publish_path = "remote/path"` → one pair: `local_artifact_path` → the
///     remote path (with `{VAR}` substitution).
///   - `publish_path = ["glob", …]`   → one pair per matched file: the file →
///     `{CIABATTA_PATH}/<file-relative-to-root, with strip_prefix removed>`.
///   - no `publish_path`              → a single pair (so the login stage still
///     has registry options) with an empty remote path.
fn build_transfers(
    recipe: &SimpleRecipe,
    root: &Path,
    env_vars: &HashMap<String, String>,
) -> Result<Vec<(PathBuf, String)>> {
    match recipe.publish_path.as_ref() {
        Some(PublishPath::Single(path)) => {
            let remote = substitute_vars(path, env_vars)?;
            let local = root.join(recipe.local_artifact_path.as_deref().unwrap_or("."));
            if local.is_dir() {
                // A directory artifact uploads each contained file individually,
                // recreating its tree under the remote publish path (the
                // registry creates sub-folders as needed). This is what the
                // documented `local_artifact_path = "some/dir"` recipes rely on.
                let files = walk_files(&local)?;
                if files.is_empty() {
                    bail!(
                        "local_artifact_path '{}' is an empty directory; nothing to push",
                        local.display()
                    );
                }
                let base = remote.trim_end_matches('/');
                let transfers = files
                    .into_iter()
                    .map(|file| {
                        let rel = file
                            .strip_prefix(&local)
                            .unwrap_or(&file)
                            .to_string_lossy()
                            .replace('\\', "/");
                        let remote = format!("{}/{}", base, rel.trim_start_matches('/'));
                        (file, remote)
                    })
                    .collect();
                Ok(transfers)
            } else {
                Ok(vec![(local, remote)])
            }
        }
        Some(PublishPath::Many(patterns)) => {
            let base = env_vars
                .get("CIABATTA_PATH")
                .filter(|v| !v.is_empty())
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "list-form publish_path uploads under CIABATTA_PATH, which is not set"
                    )
                })?;
            let strip = recipe.strip_prefix.as_deref();
            let mut transfers = Vec::new();
            for pattern in patterns {
                let matched = glob_files(root, pattern)?;
                if matched.is_empty() {
                    bail!("publish_path pattern '{}' matched no files", pattern);
                }
                for file in matched {
                    let remote = remote_for_file(&file, root, base, strip);
                    transfers.push((file, remote));
                }
            }
            Ok(transfers)
        }
        None => {
            let local = root.join(recipe.local_artifact_path.as_deref().unwrap_or("."));
            Ok(vec![(local, String::new())])
        }
    }
}

/// Recursively collect every regular file under `dir`, sorted for stable order.
fn walk_files(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let entries = std::fs::read_dir(&d)
            .with_context(|| format!("Failed to read directory {}", d.display()))?;
        for entry in entries {
            let entry = entry?;
            let file_type = entry.file_type()?;
            if file_type.is_dir() {
                stack.push(entry.path());
            } else if file_type.is_file() {
                files.push(entry.path());
            }
        }
    }
    files.sort();
    Ok(files)
}

/// Expand a glob `pattern` (relative to `root`) into the matching regular files.
fn glob_files(root: &Path, pattern: &str) -> Result<Vec<PathBuf>> {
    let full = root.join(pattern);
    let entries = glob::glob(&full.to_string_lossy())
        .with_context(|| format!("Invalid glob pattern '{}'", pattern))?;
    let mut files = Vec::new();
    for entry in entries {
        let path = entry.with_context(|| format!("Failed to read glob match for '{}'", pattern))?;
        if path.is_file() {
            files.push(path);
        }
    }
    files.sort();
    Ok(files)
}

/// Build the remote path for a matched file: its path relative to `root`, with
/// `strip` removed from the front, joined under `base` (`CIABATTA_PATH`).
fn remote_for_file(file: &Path, root: &Path, base: &str, strip: Option<&str>) -> String {
    let rel = file.strip_prefix(root).unwrap_or(file);
    let rel = rel.to_string_lossy().replace('\\', "/");
    let rel = rel.as_str();
    let stripped = match strip {
        Some(prefix) => {
            let prefix = prefix.trim_start_matches('/');
            rel.strip_prefix(prefix).unwrap_or(rel)
        }
        None => rel,
    };
    format!(
        "{}/{}",
        base.trim_end_matches('/'),
        stripped.trim_start_matches('/')
    )
}

/// Run an optional stage override command; no-op (Ok(false)) when absent.
async fn run_optional(
    cmd: Option<&str>,
    root: &Path,
    env_vars: &HashMap<String, String>,
    dry_run: bool,
    log: &mut Vec<String>,
) -> Result<bool> {
    match cmd {
        Some(c) => {
            run_shell(c, root, env_vars, dry_run, log).await?;
            Ok(true)
        }
        None => Ok(false),
    }
}

/// Run an arbitrary shell command (the stage-override mechanism).
async fn run_shell(
    cmd: &str,
    root: &Path,
    env_vars: &HashMap<String, String>,
    dry_run: bool,
    log: &mut Vec<String>,
) -> Result<()> {
    log.push(format!("$ {cmd}"));
    if dry_run {
        log.push(format!("[dry-run] would run: {cmd}"));
        return Ok(());
    }
    registry::run_shell_command(cmd, root, env_vars, log).await
}

async fn run_bash_script(
    script: &str,
    root: &Path,
    env_vars: &HashMap<String, String>,
    dry_run: bool,
    log: &mut Vec<String>,
) -> Result<()> {
    let script_path = root.join(script);
    log.push(format!("Running script: {}", script_path.display()));

    if dry_run {
        log.push(format!(
            "[dry-run] would run: bash {}",
            script_path.display()
        ));
        return Ok(());
    }

    registry::run_script(&script_path.to_string_lossy(), env_vars, log).await
}

async fn run_login_script(
    script: &str,
    root: &Path,
    env_vars: &HashMap<String, String>,
    dry_run: bool,
    log: &mut Vec<String>,
) -> Result<()> {
    let script_path = root.join(script);
    log.push(format!("Running login script: {}", script_path.display()));

    if dry_run {
        log.push(format!(
            "[dry-run] would run: bash {}",
            script_path.display()
        ));
        return Ok(());
    }

    registry::run_script(&script_path.to_string_lossy(), env_vars, log).await
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
    fn stage_labels_are_mode_aware() {
        assert_eq!(StageKind::Login.label(RunMode::Push), "login");
        assert_eq!(StageKind::Pre.label(RunMode::Push), "pre-push");
        assert_eq!(StageKind::Pre.label(RunMode::Pull), "pre-pull");
        assert_eq!(StageKind::Main.label(RunMode::Push), "push");
        assert_eq!(StageKind::Main.label(RunMode::Pull), "pull");
        assert_eq!(StageKind::Post.label(RunMode::Pull), "post-pull");
        // Compact forms drop the direction suffix on pre/post.
        assert_eq!(StageKind::Pre.short(RunMode::Push), "pre");
        assert_eq!(StageKind::Post.short(RunMode::Pull), "post");
        assert_eq!(StageKind::Main.short(RunMode::Push), "push");
    }

    #[test]
    fn remote_for_file_joins_under_base_and_strips_prefix() {
        let root = Path::new("/proj");
        let file = Path::new("/proj/dist/app.tar.gz");

        // No strip_prefix: preserve the path relative to root.
        assert_eq!(
            remote_for_file(file, root, "/main/abc123", None),
            "/main/abc123/dist/app.tar.gz"
        );
        // strip_prefix removes the leading fragment (with or without a slash).
        assert_eq!(
            remote_for_file(file, root, "/main/abc123/", Some("dist/")),
            "/main/abc123/app.tar.gz"
        );
        assert_eq!(
            remote_for_file(file, root, "/main/abc123", Some("dist")),
            "/main/abc123/app.tar.gz"
        );
        // A tag-style base (trailing slash) joins cleanly.
        assert_eq!(
            remote_for_file(file, root, "/v1.2.3/", Some("dist")),
            "/v1.2.3/app.tar.gz"
        );
    }

    #[test]
    fn set_commit_updates_commit_and_derived_path() {
        let mut vars = HashMap::new();
        vars.insert("CIABATTA_BRANCH".to_string(), "main".to_string());
        vars.insert("CIABATTA_COMMIT".to_string(), "old".to_string());
        vars.insert("CIABATTA_PATH".to_string(), "/main/old".to_string());

        set_commit(&mut vars, "new");
        assert_eq!(vars["CIABATTA_COMMIT"], "new");
        // CIABATTA_PATH is kept consistent when it isn't tag-based.
        assert_eq!(vars["CIABATTA_PATH"], "/main/new");

        // With a tag set, the tag-based path is left untouched.
        vars.insert("CIABATTA_TAG".to_string(), "v1".to_string());
        vars.insert("CIABATTA_PATH".to_string(), "/v1/".to_string());
        set_commit(&mut vars, "newer");
        assert_eq!(vars["CIABATTA_COMMIT"], "newer");
        assert_eq!(vars["CIABATTA_PATH"], "/v1/");
    }

    #[test]
    fn short_sha_truncates() {
        assert_eq!(short_sha("0d63ea6123181a46"), "0d63ea61");
        assert_eq!(short_sha("abc"), "abc");
    }

    #[test]
    fn build_transfers_expands_directory_into_per_file_uploads() {
        use std::fs;
        let tmp =
            std::env::temp_dir().join(format!("ciabatta_dir_push_{}", std::process::id()));
        let dist = tmp.join("dist");
        fs::create_dir_all(dist.join("assets")).unwrap();
        fs::write(dist.join("index.html"), b"x").unwrap();
        fs::write(dist.join("assets").join("app.js"), b"y").unwrap();

        let recipe = SimpleRecipe {
            local_artifact_path: Some("dist".to_string()),
            publish_path: Some(PublishPath::Single(
                "team/app/{CIABATTA_COMMIT}/site".to_string(),
            )),
            ..Default::default()
        };
        let mut vars = HashMap::new();
        vars.insert("CIABATTA_COMMIT".to_string(), "abc".to_string());

        let transfers = build_transfers(&recipe, &tmp, &vars).unwrap();
        let mut remotes: Vec<String> = transfers.iter().map(|(_, r)| r.clone()).collect();
        remotes.sort();
        assert_eq!(
            remotes,
            vec![
                "team/app/abc/site/assets/app.js".to_string(),
                "team/app/abc/site/index.html".to_string(),
            ]
        );

        fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn validate_list_publish_path_requires_ciabatta_path() {
        let cfg: CiabattaConfig = toml::from_str(
            r#"
[recipies.a]
registry = "nexus"
publish_path = ["dist/*.tar.gz"]
"#,
        )
        .unwrap();

        // Without CIABATTA_PATH the list form fails validation up front.
        let empty = HashMap::new();
        assert!(validate_recipes(&cfg, &["a".to_string()], &empty, &RunMode::Push).is_err());

        // With CIABATTA_PATH set it passes.
        let mut vars = HashMap::new();
        vars.insert("CIABATTA_PATH".to_string(), "/main/abc".to_string());
        assert!(validate_recipes(&cfg, &["a".to_string()], &vars, &RunMode::Push).is_ok());
    }

    #[test]
    fn validate_skips_publish_path_when_main_overridden() {
        let vars = HashMap::new();

        // main override: publish_path placeholders are the user's concern.
        let cfg: CiabattaConfig = toml::from_str(
            r#"
[recipies.a]
registry = "nexus"
publish_path = "x/{MISSING_VAR}/y"
main = "echo hi"
"#,
        )
        .unwrap();
        assert!(validate_recipes(&cfg, &["a".to_string()], &vars, &RunMode::Push).is_ok());

        // built-in main: the missing variable must be caught up front.
        let cfg2: CiabattaConfig = toml::from_str(
            r#"
[recipies.a]
registry = "nexus"
publish_path = "x/{MISSING_VAR}/y"
"#,
        )
        .unwrap();
        assert!(validate_recipes(&cfg2, &["a".to_string()], &vars, &RunMode::Push).is_err());
    }
}
