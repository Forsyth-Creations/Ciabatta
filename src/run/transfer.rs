//! Moving an artifact between the working tree and a registry.
//!
//! A `kind: push` / `kind: pull` step is an ordinary node on a workflow graph
//! whose action happens to be a registry transfer rather than a shell command.
//! Everything that decides *what* moves and *where* lives here, so the engine
//! only has to know that some steps transfer and the rest run commands.
//!
//! There is no separate pipeline any more: a step that needs something built
//! first says `needs`, and a step that has to run afterwards is just the next
//! node. What used to be `pre`/`post` stage overrides are edges on the graph.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::config::{CiabattaConfig, PublishPath, substitute_vars, validate_publish_path};
use crate::registry::{self, RegistryOpOptions};

use super::{Direction, RunStep, Transfer};

/// How many file transfers within one step may run at once. Bounds concurrent
/// `aws s3 cp` / upload processes so a large directory pushes in parallel
/// without spawning an unbounded number of subprocesses.
const MAX_CONCURRENT_TRANSFERS: usize = 40;

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
pub async fn resolve_pull_commit(
    transfer: &Transfer<'_>,
    registry_config: &crate::config::RegistryConfig,
    root: &Path,
    container_cmd: &str,
    env_vars: &HashMap<String, String>,
    sink: &mut crate::registry::LogSink<'_>,
) -> Option<HashMap<String, String>> {
    let reg_cfg = registry_config;
    let reg_name = transfer.registry?;
    let branch = env_vars.get("CIABATTA_BRANCH").filter(|v| !v.is_empty())?;
    let exact = env_vars
        .get("CIABATTA_COMMIT")
        .filter(|v| !v.is_empty())?
        .clone();
    let Some(PublishPath::Single(template)) = transfer.publish_path else {
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
            local_image: None,
            env_vars: &trial,
            dry_run: false,
            container_cmd,
        };
        match registry::exists(&opts).await {
            // Exact commit exists → keep it (no override).
            Ok(Some(true)) if *commit == exact => return None,
            Ok(Some(true)) => {
                sink.push(format!(
                    "commit {} has no artifact; pulling newest match on {}: {}",
                    short_sha(&exact),
                    branch,
                    short_sha(commit),
                ));
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
pub fn set_commit(vars: &mut HashMap<String, String>, commit: &str) {
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

/// Resolve the (local file, remote path) pairs a transfer step moves.
///
/// Paths are relative to the step's own working directory, the same way a
/// step's `script` and `run` are — a package publishes what it built, and
/// `artifact: dist/app` means that package's `dist`, not the monorepo root's.
///
///   - `publish_path: remote/path` → one pair: `artifact` → the
///     remote path (with `{VAR}` substitution).
///   - `publish_path = ["glob", …]`   → one pair per matched file: the file →
///     `{CIABATTA_PATH}/<file-relative-to-root, with strip_prefix removed>`.
///   - no `publish_path`              → a single pair (so the login stage still
///     has registry options) with an empty remote path.
pub fn build_transfers(
    transfer: &Transfer<'_>,
    root: &Path,
    env_vars: &HashMap<String, String>,
) -> Result<Vec<(PathBuf, String)>> {
    // A Docker/ECR image step (`local_image`) tags and pushes a single image;
    // there is no local file to walk. The remote reference is `publish_path`
    // (with {VAR} substitution), falling back to the local image's own name:tag.
    if let Some(image) = transfer.local_image {
        let remote = match transfer.publish_path {
            Some(PublishPath::Single(path)) => substitute_vars(path, env_vars)?,
            None => image.to_string(),
            Some(PublishPath::Many(_)) => bail!(
                "local_image pushes a single image, so publish_path must be a single remote \
                 image reference (e.g. \"app:{{CIABATTA_COMMIT}}\"), not a list of globs"
            ),
        };
        return Ok(vec![(PathBuf::from(image), remote)]);
    }

    match transfer.publish_path {
        Some(PublishPath::Single(path)) => {
            let remote = substitute_vars(path, env_vars)?;
            let local = root.join(transfer.artifact.unwrap_or("."));
            if local.is_dir() {
                // A directory artifact uploads each contained file individually,
                // recreating its tree under the remote publish path (the
                // registry creates sub-folders as needed). This is what the
                // documented `artifact: some/dir` steps rely on.
                let files = walk_files(&local)?;
                if files.is_empty() {
                    bail!(
                        "artifact '{}' is an empty directory; nothing to push",
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
            let strip = transfer.strip_prefix;
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
            let local = root.join(transfer.artifact.unwrap_or("."));
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

/// Perform a transfer step: log in to its registry, then move every resolved
/// (local, remote) pair.
///
/// Progress is reported per file, so a directory artifact shows a counter
/// rather than sitting silent through fifty uploads. Transfers run concurrently
/// (bounded by [`MAX_CONCURRENT_TRANSFERS`]) and their logs are buffered per
/// transfer and replayed in declaration order, so out-of-order completion
/// doesn't interleave into nonsense.
#[allow(clippy::too_many_arguments)]
pub async fn run(
    step: &RunStep,
    transfer: &Transfer<'_>,
    config: &CiabattaConfig,
    root: &Path,
    cwd: &Path,
    env_vars: &HashMap<String, String>,
    dry_run: bool,
    sink: &mut crate::registry::LogSink<'_>,
    progress: impl Fn(usize, usize),
) -> Result<()> {
    let registry_name = transfer.registry.ok_or_else(|| {
        anyhow::anyhow!(
            "Step '{}' is a {} step but names no `registry`.",
            step.name,
            transfer.direction.label()
        )
    })?;
    let registry_config = config.registries.get(registry_name).ok_or_else(|| {
        let mut available: Vec<&String> = config.registries.keys().collect();
        available.sort();
        anyhow::anyhow!(
            "Step '{}' names registry '{registry_name}', which isn't defined. Available: {}.",
            step.name,
            if available.is_empty() {
                "(none)".to_string()
            } else {
                available
                    .iter()
                    .map(|s| s.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            }
        )
    })?;

    // A single `publish_path` may reference {CIABATTA_*}; check before doing
    // anything, so an unresolvable path fails the step rather than uploading to
    // a literal "{CIABATTA_COMMIT}" folder.
    let mut env_vars = env_vars.clone();
    if let Some(PublishPath::Single(path)) = transfer.publish_path {
        validate_publish_path(path, &env_vars)?;
    }

    let container_cmd = config
        .system
        .as_ref()
        .and_then(|s| s.containers.clone())
        .unwrap_or_else(|| "docker".to_string());

    // Pulling by branch: when the exact commit has nothing published, walk back
    // through the branch's history for the newest one that does.
    if transfer.direction == Direction::Pull
        && let Some(adjusted) = resolve_pull_commit(
            transfer,
            registry_config,
            cwd,
            &container_cmd,
            &env_vars,
            sink,
        )
        .await
    {
        env_vars = adjusted;
    }

    let pairs = build_transfers(transfer, cwd, &env_vars)?;
    let opts: Vec<RegistryOpOptions<'_>> = pairs
        .iter()
        .map(|(local, remote)| RegistryOpOptions {
            registry_name,
            registry_config,
            local_path: local.as_path(),
            remote_path: remote.as_str(),
            local_image: transfer.local_image,
            env_vars: &env_vars,
            dry_run,
            container_cmd: &container_cmd,
        })
        .collect();

    if dry_run {
        sink.push(format!(
            "[dry-run] would {} {} file(s) via '{registry_name}'",
            transfer.direction.label(),
            opts.len()
        ));
        for o in &opts {
            sink.push(format!(
                "[dry-run]   {} → {}",
                o.local_path.display(),
                o.remote_path
            ));
        }
        return Ok(());
    }

    // Authentication is per-registry, so the first transfer's options speak for
    // all of them.
    if let Some(first) = opts.first() {
        match registry_config.login_script.as_deref() {
            Some(script) => {
                let path = root.join(script);
                let command = format!(
                    "bash '{}'",
                    path.display().to_string().replace('\'', r"'\''")
                );
                sink.push(format!("$ {command}"));
                registry::run_shell_command_opts(&command, root, &env_vars, false, sink).await?;
            }
            None => {
                let mut lines: Vec<String> = Vec::new();
                registry::default_login(first, &mut lines).await?;
                for line in lines {
                    sink.push(line);
                }
            }
        }
    }

    use futures::stream::StreamExt;
    let total = opts.len();
    progress(0, total);

    let mut futs = Vec::with_capacity(total);
    for (i, o) in opts.iter().enumerate() {
        futs.push(one(i, o, transfer.direction));
    }
    let mut stream = futures::stream::iter(futs).buffer_unordered(MAX_CONCURRENT_TRANSFERS);

    let mut sublogs: Vec<Option<Vec<String>>> = (0..total).map(|_| None).collect();
    let mut done = 0;
    let mut first_err: Option<anyhow::Error> = None;
    while let Some((i, res, sublog)) = stream.next().await {
        sublogs[i] = Some(sublog);
        done += 1;
        progress(done, total);
        if let Err(e) = res {
            first_err = Some(e);
            // Stop launching new transfers; in-flight ones are cancelled as the
            // stream unwinds.
            break;
        }
    }
    drop(stream);

    for sublog in sublogs.into_iter().flatten() {
        for line in sublog {
            sink.push(line);
        }
    }

    match first_err {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

/// One transfer into its own log buffer, tagged with its original index so the
/// caller can replay concurrent results in order.
async fn one(
    i: usize,
    o: &RegistryOpOptions<'_>,
    direction: Direction,
) -> (usize, Result<()>, Vec<String>) {
    let mut sublog: Vec<String> = Vec::new();
    let res = match direction {
        Direction::Push => registry::push(o, &mut sublog).await,
        Direction::Pull => registry::pull(o, &mut sublog).await,
    };
    (i, res, sublog)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A push transfer built straight from its fields, the way a `kind: push`
    /// step resolves into one.
    fn push_transfer(
        registry: Option<&'static str>,
        artifact: Option<&'static str>,
        local_image: Option<&'static str>,
        publish_path: Option<&'static PublishPath>,
        strip_prefix: Option<&'static str>,
    ) -> Transfer<'static> {
        Transfer {
            direction: Direction::Push,
            registry,
            artifact,
            local_image,
            publish_path,
            strip_prefix,
        }
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
        vars.insert("CIABATTA_PATH".to_string(), "/v1".to_string());
        set_commit(&mut vars, "newer");
        assert_eq!(vars["CIABATTA_COMMIT"], "newer");
        assert_eq!(vars["CIABATTA_PATH"], "/v1");
    }

    #[test]
    fn short_sha_truncates() {
        assert_eq!(short_sha("0d63ea6123181a46"), "0d63ea61");
        assert_eq!(short_sha("abc"), "abc");
    }

    #[test]
    fn build_transfers_expands_directory_into_per_file_uploads() {
        use std::fs;
        let tmp = std::env::temp_dir().join(format!("ciabatta_dir_push_{}", std::process::id()));
        let dist = tmp.join("dist");
        fs::create_dir_all(dist.join("assets")).unwrap();
        fs::write(dist.join("index.html"), b"x").unwrap();
        fs::write(dist.join("assets").join("app.js"), b"y").unwrap();

        static PATH: std::sync::LazyLock<PublishPath> = std::sync::LazyLock::new(|| {
            PublishPath::Single("team/app/{CIABATTA_COMMIT}/site".to_string())
        });
        let transfer = push_transfer(None, Some("dist"), None, Some(&PATH), None);
        let mut vars = HashMap::new();
        vars.insert("CIABATTA_COMMIT".to_string(), "abc".to_string());

        let transfers = build_transfers(&transfer, &tmp, &vars).unwrap();
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

    /// A package publishes what *it* built. Resolving against the monorepo root
    /// would silently publish the wrong file — or nothing — in a workspace where
    /// two packages both have a `dist/`.
    #[test]
    fn an_artifact_resolves_against_the_steps_own_directory() {
        use std::fs;
        let tmp = std::env::temp_dir().join(format!("ciab_cwd_push_{}", std::process::id()));
        let pkg = tmp.join("packages/api");
        fs::create_dir_all(&pkg).unwrap();
        fs::write(tmp.join("app.tgz"), b"root").unwrap();
        fs::write(pkg.join("app.tgz"), b"package").unwrap();

        static PATH: std::sync::LazyLock<PublishPath> =
            std::sync::LazyLock::new(|| PublishPath::Single("api/app.tgz".to_string()));
        let transfer = push_transfer(Some("nexus"), Some("app.tgz"), None, Some(&PATH), None);

        let transfers = build_transfers(&transfer, &pkg, &HashMap::new()).unwrap();
        assert_eq!(transfers.len(), 1);
        assert_eq!(
            transfers[0].0,
            pkg.join("app.tgz"),
            "the package's own artifact, not the root's"
        );

        fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn build_transfers_uses_local_image_without_touching_filesystem() {
        // A docker/ECR image step: no file is walked; the single transfer maps
        // the local image (as local_path) to the substituted remote reference.
        static PATH: std::sync::LazyLock<PublishPath> =
            std::sync::LazyLock::new(|| PublishPath::Single("app:{CIABATTA_COMMIT}".to_string()));
        let transfer = push_transfer(Some("ecr"), None, Some("app:latest"), Some(&PATH), None);
        let mut vars = HashMap::new();
        vars.insert("CIABATTA_COMMIT".to_string(), "abc".to_string());

        // A path that doesn't exist would blow up the file-based branch; here it's
        // ignored entirely.
        let transfers = build_transfers(&transfer, Path::new("/nonexistent"), &vars).unwrap();
        assert_eq!(transfers.len(), 1);
        assert_eq!(transfers[0].0, PathBuf::from("app:latest"));
        assert_eq!(transfers[0].1, "app:abc");
    }

    #[test]
    fn build_transfers_local_image_defaults_remote_to_image_ref() {
        // With no publish_path, the remote reference reuses the local image name.
        let transfer = push_transfer(Some("dockerhub"), None, Some("app:v1"), None, None);
        let transfers =
            build_transfers(&transfer, Path::new("/nonexistent"), &HashMap::new()).unwrap();
        assert_eq!(
            transfers,
            vec![(PathBuf::from("app:v1"), "app:v1".to_string())]
        );
    }

    #[test]
    fn build_transfers_local_image_rejects_glob_list() {
        static PATH: std::sync::LazyLock<PublishPath> =
            std::sync::LazyLock::new(|| PublishPath::Many(vec!["dist/*".to_string()]));
        let transfer = push_transfer(None, None, Some("app:v1"), Some(&PATH), None);
        assert!(build_transfers(&transfer, Path::new("/x"), &HashMap::new()).is_err());
    }
}
