//! The cache's command-line surface: `ciabatta dry-run` and `ciabatta cache …`.
//!
//! `dry-run` is the important one. A build cache is a promise that skipping
//! work is safe, and nobody should have to take that on faith — so this
//! prints, for every step, whether it would be reused and, when it wouldn't,
//! precisely which of its three dependencies moved.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use owo_colors::OwoColorize;

use super::graph::{Plan, Planned, StepContext};
use super::store::{Retention, Store};
use super::{CacheConfig, Decision, diff};
use crate::config::CiabattaConfig;
use crate::run::RunStep;
use crate::workspace::Workspace;

/// Resolves each step's cache settings from the monorepo it came from.
///
/// A compiled workflow graph carries the sub-workspace on every node, which is
/// what lets one graph spanning four packages resolve four different `cache:`
/// sections — the alternative, one cache config for the whole repo, would be
/// wrong for every package but the first.
///
/// The *paths* in those four sections are another matter: they all resolve
/// against the workspace root. See [`StepContext::dir`] below for why.
pub struct WorkspaceContext<'a> {
    pub workspace: Option<&'a Workspace>,
    /// The project root, for steps with no sub-workspace of their own.
    pub root: PathBuf,
    /// The root project's own config, used for those steps.
    pub config: &'a CiabattaConfig,
}

impl StepContext for WorkspaceContext<'_> {
    fn cache_config(&self, step: &RunStep) -> CacheConfig {
        let member = step
            .workspace
            .as_deref()
            .and_then(|name| self.workspace.and_then(|ws| ws.member(name)));

        let workspace_level = member
            .map(|m| &m.config)
            .unwrap_or(self.config)
            .cache
            .clone();

        // Two levels, not three: a workflow's own `cache:` was folded into
        // each of its steps when the graph compiled, so `step.cache` already
        // carries it.
        super::graph::effective(workspace_level.as_ref(), step.cache.as_ref())
    }

    fn dir(&self, _step: &RunStep) -> PathBuf {
        // The workspace root, for every step, whichever sub-workspace it came
        // from. The cache is one thing per project — one store, one entry
        // namespace, and one `cache.remote` read from the root — so one
        // directory has to be what its paths mean, and the root is the only
        // one every member can name a file through.
        //
        // Resolving against the member instead is what this used to do, and it
        // forced any step reaching a sibling to write `../`. That `../`
        // survived into the stored manifest, where joining it onto the entry's
        // artifact directory walked back out of it: every key for that target
        // wrote its output to the same shared path, so restoring one entry
        // handed back another's file. The remote cache rejects such a path
        // outright (`safe_relative`), so those steps could never be shared at
        // all.
        self.workspace
            .map(|ws| ws.root.clone())
            .unwrap_or_else(|| self.root.clone())
    }

    fn member(&self, step: &RunStep) -> Option<String> {
        step.workspace
            .as_deref()
            .and_then(|name| self.workspace.and_then(|ws| ws.member(name)))
            // The root member's `rel` is ".", which is not a subtree to keep
            // out of anything.
            .map(|member| member.rel.clone())
            .filter(|rel| !rel.is_empty() && rel != ".")
    }

    fn workspace(&self, step: &RunStep) -> String {
        step.workspace.clone().unwrap_or_else(|| ".".to_string())
    }
}

// ─── dry-run ────────────────────────────────────────────────────────────────

/// Print a plan the way a person reads it.
pub fn print_plan(plan: &Plan, store: &Store, show_diff: bool) {
    if plan.steps.is_empty() {
        println!("Nothing to plan — no runnable steps were selected.");
        return;
    }

    if !plan.has_caching() {
        println!(
            "Caching is off for every step here, so all {} would run.\n\
             Turn it on with `ciabatta cache init`.",
            plan.steps.len()
        );
        return;
    }

    for step in &plan.steps {
        print_step(step, show_diff);
    }

    let (reused, rebuilt) = plan.tally();
    println!();
    println!("{reused} step(s) would be reused, {rebuilt} would run.",);

    let saved = plan.saved_ms(store);
    if saved > 0 {
        println!(
            "That's about {} of build time not spent, based on what those steps cost last time.",
            humanize_ms(saved)
        );
    }
}

/// One step's line, plus its explanation when it has one.
fn print_step(step: &Planned, show_diff: bool) {
    let (mark, label) = match &step.decision {
        Decision::Fresh { .. } => ("✓".green().to_string(), step.decision.describe()),
        Decision::Hit { .. } => ("↺".green().to_string(), step.decision.describe()),
        Decision::Rebuild { .. } => ("●".yellow().to_string(), step.decision.describe()),
        Decision::Uncached { .. } => ("·".dimmed().to_string(), step.decision.describe()),
    };
    println!("{mark} {:<28} {label}", step.name);

    // The inputs and outputs are the contract this step is being judged
    // against, so show them rather than making somebody open the config.
    if !step.inputs.is_empty() || !step.outputs.is_empty() {
        let inputs: u64 = step.inputs.iter().map(|f| f.size).sum();
        let outputs: u64 = step.outputs.iter().map(|f| f.size).sum();
        println!(
            "    {} {} in ({}) → {} out ({})",
            "files:".dimmed(),
            step.inputs.len(),
            super::store::human_size(inputs),
            step.outputs.len(),
            super::store::human_size(outputs),
        );
    }

    let Some(diff) = &step.diff else { return };

    println!("    {} {}", "changed:".dimmed(), diff.summary());
    if !show_diff {
        // Without --diff, name the files but don't print their lines.
        for file in diff.files.iter().take(5) {
            println!(
                "      {} {} (+{} −{})",
                marker(&file.kind),
                file.path,
                file.additions,
                file.deletions
            );
        }
        if diff.files.len() > 5 {
            println!("      … and {} more", diff.files.len() - 5);
        }
        for env in &diff.env {
            println!("      env {} {}", env.name, describe_env(env));
        }
        for upstream in &diff.upstream {
            println!("      stage {} produced different output", upstream.step);
        }
        if !diff.files.is_empty() {
            println!("      {}", "(run with --diff to see the lines)".dimmed());
        }
        return;
    }

    for line in diff::render(diff).lines() {
        println!("      {line}");
    }
}

fn marker(kind: &diff::ChangeKind) -> &'static str {
    match kind {
        diff::ChangeKind::Added => "+",
        diff::ChangeKind::Removed => "-",
        diff::ChangeKind::Modified => "~",
    }
}

fn describe_env(env: &diff::EnvDiff) -> String {
    match env.kind {
        diff::ChangeKind::Added => format!("is new (= {})", env.after.as_deref().unwrap_or("")),
        diff::ChangeKind::Removed => "is gone".to_string(),
        diff::ChangeKind::Modified => format!(
            "changed: {} → {}",
            env.before.as_deref().unwrap_or(""),
            env.after.as_deref().unwrap_or("")
        ),
    }
}

/// A plan as JSON, for scripts and for the web view.
pub fn plan_json(plan: &Plan) -> serde_json::Value {
    let (reused, rebuilt) = plan.tally();
    serde_json::json!({
        "reused": reused,
        "rebuilt": rebuilt,
        "steps": plan.steps.iter().map(|step| serde_json::json!({
            "name": step.name,
            "needs": step.needs,
            "workspace": step.target.workspace,
            "decision": step.decision,
            "key": step.decision.key(),
            "inputs": step.inputs,
            "outputs": step.outputs,
            "diff": step.diff,
        })).collect::<Vec<_>>(),
    })
}

/// Render milliseconds the way somebody would say them out loud.
pub fn humanize_ms(ms: u64) -> String {
    let seconds = ms / 1000;
    if seconds < 60 {
        return format!("{seconds}s");
    }
    let minutes = seconds / 60;
    if minutes < 60 {
        return format!("{}m {}s", minutes, seconds % 60);
    }
    format!("{}h {}m", minutes / 60, minutes % 60)
}

// ─── cache init ─────────────────────────────────────────────────────────────

/// A proposed `cache:` section, worked out from what's in the directory.
#[derive(Debug, Clone, PartialEq)]
pub struct Proposal {
    pub inputs: Vec<String>,
    pub outputs: Vec<String>,
    pub exclude: Vec<String>,
    /// Why each input pattern was proposed, so the user can judge it.
    pub reasons: Vec<(String, &'static str)>,
}

/// Directories that are build output rather than source, wherever they appear.
const OUTPUT_DIRS: &[(&str, &str)] = &[
    ("dist", "bundler and packaging output"),
    ("build", "build output"),
    ("out", "build output"),
    ("target/release", "cargo release output"),
    ("target/debug", "cargo debug output"),
    ("generated", "generated code"),
    ("coverage", "test coverage reports"),
];

/// Files and directories that are inputs, when they're there.
const INPUT_CANDIDATES: &[(&str, &str)] = &[
    ("src/**/*", "source"),
    ("lib/**/*", "source"),
    ("app/**/*", "source"),
    ("scripts/**/*", "the scripts the build runs"),
    ("proto/**/*", "schema definitions"),
    ("Cargo.toml", "the Rust manifest"),
    ("Cargo.lock", "pinned Rust dependencies"),
    ("package.json", "the Node manifest"),
    ("pyproject.toml", "the Python manifest"),
    ("go.mod", "the Go manifest"),
    ("go.sum", "pinned Go dependencies"),
    ("Makefile", "the build definition"),
    ("Dockerfile", "the image definition"),
    ("tsconfig.json", "TypeScript settings"),
];

/// Never inputs, even when an `inputs` pattern would match them.
const ALWAYS_EXCLUDE: &[&str] = &["node_modules", "target", ".git", "__pycache__", ".venv"];

/// Propose a cache config for a directory by looking at what's actually there.
///
/// This is the part of `cache init` that matters. An `inputs` list that misses
/// a file the build reads produces a cache that confidently serves stale
/// artifacts — so the proposal comes from the directory, and every entry says
/// why it's there for the user to check.
/// Propose a cache config for `dir`, with every pattern written relative to the
/// workspace root.
///
/// `dir` is still where the looking happens — a package's `src/` is found by
/// looking in the package. `rel` is where that package sits in the monorepo, and
/// every proposed pattern is prefixed with it, because that is the only spelling
/// the cache resolves (see the [`crate::cache`] module docs). Scaffolding a
/// member with the bare `src/**/*` it would once have got means a `cache:`
/// section that matches nothing at all, which is the silent kind of wrong: a
/// build keyed on an empty input set hits forever.
pub fn propose_under(dir: &Path, rel: Option<&str>) -> Proposal {
    let at = |pattern: &str| match rel {
        Some(rel) if !rel.is_empty() && rel != "." => format!("{rel}/{pattern}"),
        _ => pattern.to_string(),
    };

    let mut inputs = Vec::new();
    let mut reasons = Vec::new();

    for (pattern, why) in INPUT_CANDIDATES {
        if matches_anything(dir, pattern) {
            inputs.push(at(pattern));
            reasons.push((at(pattern), *why));
        }
    }

    let outputs: Vec<String> = OUTPUT_DIRS
        .iter()
        .filter(|(name, _)| dir.join(name).is_dir())
        .map(|(name, _)| at(&format!("{name}/**/*")))
        .collect();

    let exclude: Vec<String> = ALWAYS_EXCLUDE
        .iter()
        .filter(|name| dir.join(name).exists())
        .map(|name| at(name))
        // Anything proposed as an output must never also count as an input, or
        // every build would invalidate itself.
        .chain(
            OUTPUT_DIRS
                .iter()
                .filter(|(name, _)| dir.join(name).is_dir())
                .map(|(name, _)| at(name)),
        )
        .collect();

    Proposal {
        inputs,
        outputs,
        exclude: dedup(exclude),
        reasons,
    }
}

fn dedup(mut values: Vec<String>) -> Vec<String> {
    values.sort();
    values.dedup();
    values
}

/// Whether a glob matches at least one file under `dir`.
fn matches_anything(dir: &Path, pattern: &str) -> bool {
    let joined = dir.join(pattern);
    let Some(as_str) = joined.to_str() else {
        return false;
    };
    glob::glob(as_str)
        .map(|mut paths| paths.any(|p| p.is_ok_and(|p| p.is_file())))
        .unwrap_or(false)
}

impl Proposal {
    /// Whether there's enough here to cache anything.
    pub fn is_usable(&self) -> bool {
        !self.inputs.is_empty() && !self.outputs.is_empty()
    }

    /// Render the proposal as the YAML block to splice into a config.
    pub fn to_yaml(&self, enabled: bool) -> String {
        let mut out = String::new();
        out.push_str("  # What this workflow reads and writes. Getting `inputs` right\n");
        out.push_str("  # is the part that matters: a build that reads a file not listed here\n");
        out.push_str("  # will be handed a stale result when that file changes.\n");
        out.push_str("  #\n");
        out.push_str("  # Paths are relative to the workspace root, not to this file's\n");
        out.push_str("  # directory — one project is one cache, so one directory has to be\n");
        out.push_str("  # what they mean. `ciabatta why <step>` lists what they matched.\n");
        out.push_str(&format!("  enabled: {enabled}\n"));

        out.push_str("  inputs:\n");
        if self.inputs.is_empty() {
            out.push_str("    # TODO: nothing recognizable was found — list your sources here,\n");
            out.push_str("    # e.g. \"src/**/*\"\n");
            out.push_str("    []\n");
        } else {
            for pattern in &self.inputs {
                let why = self
                    .reasons
                    .iter()
                    .find(|(p, _)| p == pattern)
                    .map(|(_, why)| *why)
                    .unwrap_or("");
                out.push_str(&format!("    - \"{pattern}\"   # {why}\n"));
            }
        }

        out.push_str("  outputs:\n");
        if self.outputs.is_empty() {
            out.push_str("    # TODO: no build output directory was found yet. List what your\n");
            out.push_str("    # build produces, e.g. \"dist/**/*\" — with none, nothing can be\n");
            out.push_str("    # restored and every build runs.\n");
            out.push_str("    []\n");
        } else {
            for pattern in &self.outputs {
                out.push_str(&format!("    - \"{pattern}\"\n"));
            }
        }

        if !self.exclude.is_empty() {
            out.push_str("  # Never treated as inputs, so a build can't invalidate itself.\n");
            out.push_str("  exclude:\n");
            for pattern in &self.exclude {
                out.push_str(&format!("    - \"{pattern}\"\n"));
            }
        }

        out.push_str("  # Variables the build's result depends on. A build that behaves\n");
        out.push_str("  # differently under a different PROFILE must list it here.\n");
        out.push_str("  env: []\n");

        out
    }
}

/// Steps of `workflow` that declare their own `cache:`, which wins over the
/// workflow's section.
///
/// Worth reporting after `cache init`: the precedence is deliberate, but
/// "caching is on" followed by nothing being cached — because a step written by
/// `ciabatta convert` still says `enabled: false` — reads as the feature being
/// broken rather than as two settings doing exactly what they say.
pub fn overriding_steps(root: &Path, workflow: &str) -> Vec<(String, bool)> {
    let dir = root
        .join(crate::config::CIABATTA_DIR)
        .join(crate::workspace::WORKFLOWS_DIR);
    let Some(path) = crate::format::find(&dir, workflow) else {
        return Vec::new();
    };
    let Ok(loaded) = crate::format::load::<crate::workspace::Workflow>(&path) else {
        return Vec::new();
    };

    loaded
        .steps
        .iter()
        .filter_map(|step| {
            step.cache
                .as_ref()
                .map(|cache| (step.name.clone(), cache.is_on()))
        })
        .collect()
}

/// Write a proposed `cache:` section into a workflow's file.
///
/// The file definition — what a build reads, writes and depends on — belongs
/// with the build it describes, so it lands in
/// `.ciabatta/workflows/<name>.yaml` next to the steps. A `remote:` URL is the
/// exception: that is one cache server per checkout, not a property of any one
/// workflow, so it goes into `ciabatta.yaml` instead.
///
/// Returns the paths written, workflow file first.
pub fn write_cache_section(
    root: &Path,
    workflow: &str,
    proposal: &Proposal,
    enabled: bool,
    remote: Option<&str>,
    force: bool,
) -> Result<Vec<PathBuf>> {
    let dir = root
        .join(crate::config::CIABATTA_DIR)
        .join(crate::workspace::WORKFLOWS_DIR);
    let path = crate::format::find(&dir, workflow).ok_or_else(|| {
        anyhow::anyhow!(
            "No workflow called '{workflow}' in {}.\n\
             Create it (or run `ciabatta init --lib` to scaffold one), then try again.",
            dir.display()
        )
    })?;

    let existing = std::fs::read_to_string(&path)
        .with_context(|| format!("Failed to read {}", path.display()))?;

    // Anchored at column 0: a step may carry its own nested `cache:`
    // (`ciabatta convert` writes one), and that is not the section this writes.
    // Matching at any indentation would refuse `cache init` on every workflow
    // that has ever been converted from a script.
    if existing.lines().any(|l| l.starts_with("cache:")) && !force {
        bail!(
            "{} already has a `cache:` section. Edit it directly, or pass --force \
             to replace it.",
            path.display()
        );
    }

    let block = format!("cache:\n{}", proposal.to_yaml(enabled));
    let rendered = crate::format::set_top_level(&existing, "cache", &block);

    // This edits a file the user owns; hand it back only if it still loads.
    let parsed: crate::workspace::Workflow =
        crate::format::from_str(&rendered, crate::format::Format::of_path(&path)).with_context(
            || {
                format!(
                    "Writing the cache section would have broken {}, so it was left alone",
                    path.display()
                )
            },
        )?;
    anyhow::ensure!(
        parsed.cache.is_some(),
        "The generated cache section didn't survive a round trip; {} was left alone",
        path.display()
    );

    std::fs::write(&path, rendered)
        .with_context(|| format!("Failed to write {}", path.display()))?;

    let mut written = vec![path];
    if let Some(url) = remote {
        written.push(write_remote_section(root, url, force)?);
    }
    Ok(written)
}

/// Point this checkout at a shared cache server, in `ciabatta.yaml`.
///
/// Separate from the file definition on purpose: one endpoint serves every
/// workflow here, and repeating it per workflow would be four places to change
/// when the server moves.
fn write_remote_section(root: &Path, url: &str, force: bool) -> Result<PathBuf> {
    let path = crate::config::config_path(root).ok_or_else(|| {
        anyhow::anyhow!(
            "No ciabatta config in {}. Run `ciabatta init` first.",
            root.display()
        )
    })?;

    let existing = std::fs::read_to_string(&path)
        .with_context(|| format!("Failed to read {}", path.display()))?;

    if existing.lines().any(|l| l.starts_with("cache:")) && !force {
        bail!(
            "{} already has a `cache:` section. Edit it directly, or pass --force \
             to replace it.",
            path.display()
        );
    }

    let mut block = String::from("cache:\n  remote:\n");
    block.push_str(&format!("    url: {url}\n"));
    block.push_str("    # `project` is filled in by the server the first time this\n");
    block.push_str("    # checkout connects. Commit it: it's what makes every checkout\n");
    block.push_str("    # and every CI runner resolve to the same project.\n");
    let rendered = crate::format::set_top_level(&existing, "cache", &block);

    let parsed: CiabattaConfig =
        crate::format::from_str(&rendered, crate::format::Format::of_path(&path)).with_context(
            || {
                format!(
                    "Writing the remote cache section would have broken {}, so it was left alone",
                    path.display()
                )
            },
        )?;
    anyhow::ensure!(
        parsed.cache.as_ref().and_then(|c| c.remote()).is_some(),
        "The generated remote section didn't survive a round trip; {} was left alone",
        path.display()
    );

    std::fs::write(&path, rendered)
        .with_context(|| format!("Failed to write {}", path.display()))?;
    Ok(path)
}

// ─── cache status / prune ───────────────────────────────────────────────────

/// Print what the local store is holding.
pub fn print_status(store: &Store) -> Result<()> {
    let stats = store.stats()?;

    if stats.entries == 0 {
        println!("The local cache is empty.");
        println!("It fills up as cached steps run — see `ciabatta cache init`.");
        return Ok(());
    }

    println!("Local cache: {}", store.root().display());
    println!(
        "  {} entr(ies), {}",
        stats.entries,
        super::store::human_size(stats.size)
    );
    if stats.build_time_ms > 0 {
        println!(
            "  {} of build time is stored here",
            humanize_ms(stats.build_time_ms)
        );
    }
    if let (Some(oldest), Some(newest)) = (&stats.oldest, &stats.newest) {
        println!("  oldest {oldest}");
        println!("  newest {newest}");
    }

    if !stats.by_workspace.is_empty() {
        println!();
        println!("By workspace:");
        for (workspace, count) in &stats.by_workspace {
            println!("  {workspace:<24} {count} entr(ies)");
        }
    }
    Ok(())
}

/// Apply a retention policy and report what went.
pub fn print_prune(store: &Store, policy: &Retention, dry_run: bool) -> Result<()> {
    if policy.is_unlimited() {
        bail!(
            "Nothing to prune by: pass at least one of --max-age, --max-size, \
             or --max-entries."
        );
    }

    if dry_run {
        // Work out what would go without removing it, by listing and filtering
        // the same way `prune` does.
        let mut entries = store.list()?;
        entries.sort_by(|a, b| a.last_touched().cmp(b.last_touched()));

        let max_age = policy.max_age_seconds()?;
        let max_size = policy.max_size_bytes()?;
        let mut would_go: Vec<(String, &'static str)> = Vec::new();
        let mut kept = Vec::new();

        for entry in entries {
            if max_age.is_some_and(|max| entry.age_seconds().is_some_and(|age| age > max)) {
                would_go.push((entry.key.clone(), "too old"));
            } else {
                kept.push(entry);
            }
        }
        if let Some(max) = max_size {
            let mut total: u64 = kept.iter().map(|e| e.size).sum();
            for entry in &kept {
                if total <= max {
                    break;
                }
                total = total.saturating_sub(entry.size);
                would_go.push((entry.key.clone(), "over the size limit"));
            }
        }
        if let Some(max) = policy.max_entries {
            let surplus = kept.len().saturating_sub(max);
            for entry in kept.iter().take(surplus) {
                would_go.push((entry.key.clone(), "over the entry limit"));
            }
        }

        if would_go.is_empty() {
            println!("Nothing would be evicted under {}.", policy.describe());
            return Ok(());
        }
        println!("Would evict {} entr(ies):", would_go.len());
        for (key, why) in &would_go {
            println!("  {} — {why}", &key[..key.len().min(12)]);
        }
        println!("\nRun without --dry-run to remove them.");
        return Ok(());
    }

    let pruned = store.prune(policy)?;
    if pruned.is_empty() {
        println!("Nothing to evict under {}.", policy.describe());
        return Ok(());
    }

    println!(
        "Evicted {} entr(ies), reclaiming {}.",
        pruned.removed.len(),
        super::store::human_size(pruned.freed)
    );
    if pruned.orphans > 0 {
        println!(
            "Also swept {} orphaned artifact director(ies).",
            pruned.orphans
        );
    }
    Ok(())
}

/// Build a [`Retention`] from the flags, treating "no flags" as unlimited so
/// the caller can refuse rather than silently evicting nothing.
pub fn retention_from_flags(
    max_age: Option<String>,
    max_size: Option<String>,
    max_entries: Option<usize>,
) -> Retention {
    if max_age.is_none() && max_size.is_none() && max_entries.is_none() {
        return Retention::unlimited();
    }
    Retention {
        max_age,
        max_size,
        max_entries,
    }
}

/// The environment a plan is computed against, as a sorted map.
pub fn env_map(vars: &std::collections::HashMap<String, String>) -> BTreeMap<String, String> {
    vars.iter().map(|(k, v)| (k.clone(), v.clone())).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ciab_ccli_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write(dir: &Path, rel: &str, body: &str) {
        let path = dir.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }

    /// Every step resolves its paths against the workspace root, whichever
    /// sub-workspace it came from — and says which member it is, so its own
    /// files survive the nested-workspace exclusion.
    ///
    /// This is the whole of the fix: resolving against the member instead made
    /// any step reaching a sibling write `../`, and a `../` in a stored path
    /// escapes the cache entry it belongs to.
    #[test]
    fn every_step_resolves_its_paths_against_the_workspace_root() {
        let dir = scratch("rootrelative");
        write(
            &dir,
            ".ciabatta/ciabatta.yaml",
            "workspace:\n  name: repo\n",
        );
        write(
            &dir,
            "editors/vscode/.ciabatta/ciabatta.yaml",
            "workspace:\n  name: vscode-extension\n",
        );
        write(&dir, "editors/vscode/src/main.ts", "export {}");

        // `load`, not `discover`: discovery walks *up* from the directory given,
        // and every test here shares one temp dir, so on a machine where the
        // walk settles above this tree it scans a sibling test's fixture —
        // including ones that are deliberately malformed. The rest of the
        // workspace tests take the same route for the same reason.
        let workspace = crate::workspace::Workspace::load(&dir).unwrap();
        let config = CiabattaConfig::default();
        let context = WorkspaceContext {
            workspace: Some(&workspace),
            root: dir.clone(),
            config: &config,
        };

        let member_step = RunStep {
            name: "package".to_string(),
            workspace: Some("vscode-extension".to_string()),
            ..Default::default()
        };
        let root_step = RunStep {
            name: "binary".to_string(),
            workspace: None,
            ..Default::default()
        };

        // Not `editors/vscode` — the root, so `editors/dist/…` is nameable
        // without climbing out of anything.
        assert_eq!(context.dir(&member_step), workspace.root);
        assert_eq!(context.dir(&root_step), workspace.root);

        assert_eq!(
            context.member(&member_step),
            Some("editors/vscode".to_string())
        );
        // The root is not a subtree to keep out of its own inputs.
        assert_eq!(context.member(&root_step), None);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `cache init` in a package proposes paths already rooted at the monorepo,
    /// because the bare `src/**/*` it would once have written now matches
    /// nothing — and a build keyed on an empty input set hits forever.
    #[test]
    fn a_proposal_for_a_member_is_written_from_the_root() {
        let dir = scratch("proposeunder");
        write(&dir, "src/main.rs", "fn main() {}");
        write(&dir, "Cargo.toml", "[package]");
        write(&dir, "dist/app", "built");
        write(&dir, "node_modules/left-pad/index.js", "");

        let proposal = propose_under(&dir, Some("packages/api"));

        assert!(
            proposal
                .inputs
                .contains(&"packages/api/src/**/*".to_string())
        );
        assert!(
            proposal
                .inputs
                .contains(&"packages/api/Cargo.toml".to_string())
        );
        assert!(
            proposal
                .outputs
                .contains(&"packages/api/dist/**/*".to_string())
        );
        // Excludes are matched against the same root-relative path, so they are
        // prefixed too or they would stop excluding anything.
        assert!(
            proposal
                .exclude
                .contains(&"packages/api/node_modules".to_string())
        );
        // The reasons are keyed by the pattern actually written, or `cache init`
        // would print every input with a blank explanation.
        assert!(
            proposal
                .reasons
                .iter()
                .any(|(p, why)| p == "packages/api/src/**/*" && !why.is_empty())
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_proposal_comes_from_what_is_actually_in_the_directory() {
        let dir = scratch("propose");
        write(&dir, "src/main.rs", "fn main() {}");
        write(&dir, "scripts/build.sh", "#!/bin/sh");
        write(&dir, "Cargo.toml", "[package]");
        write(&dir, "Cargo.lock", "");
        write(&dir, "dist/app", "built");
        write(&dir, "node_modules/left-pad/index.js", "");

        let proposal = propose_under(&dir, None);
        assert!(proposal.is_usable());

        assert!(proposal.inputs.contains(&"src/**/*".to_string()));
        assert!(
            proposal.inputs.contains(&"scripts/**/*".to_string()),
            "the scripts a build runs are inputs — an edited build script must \
             not serve a stale artifact"
        );
        assert!(proposal.inputs.contains(&"Cargo.toml".to_string()));
        assert!(proposal.inputs.contains(&"Cargo.lock".to_string()));
        // Nothing that isn't there is proposed.
        assert!(!proposal.inputs.contains(&"package.json".to_string()));
        assert!(!proposal.inputs.contains(&"go.mod".to_string()));

        assert_eq!(proposal.outputs, vec!["dist/**/*".to_string()]);

        // Output directories and dependency trees must never count as inputs.
        assert!(proposal.exclude.contains(&"dist".to_string()));
        assert!(proposal.exclude.contains(&"node_modules".to_string()));

        // Every proposed input says why, so the user can judge it.
        for pattern in &proposal.inputs {
            assert!(
                proposal
                    .reasons
                    .iter()
                    .any(|(p, why)| p == pattern && !why.is_empty()),
                "{pattern} was proposed with no reason"
            );
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_empty_directory_proposes_nothing_usable_and_says_so_in_the_yaml() {
        let dir = scratch("empty");
        let proposal = propose_under(&dir, None);
        assert!(!proposal.is_usable());

        let yaml = proposal.to_yaml(false);
        assert!(yaml.contains("enabled: false"));
        assert!(
            yaml.contains("TODO"),
            "an empty proposal must not look complete"
        );

        // It still has to parse, so `cache init` on a bare directory leaves a
        // workflow file that loads.
        let block = format!("cache:\n{yaml}");
        let workflow: crate::workspace::Workflow =
            crate::format::from_str(&block, crate::format::Format::Yaml).unwrap();
        let cache = workflow.cache.unwrap();
        assert!(!cache.is_on());
        assert!(cache.inputs.is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_generated_yaml_parses_back_into_the_workflow_it_claims_to_be() {
        let dir = scratch("yaml");
        write(&dir, "src/main.rs", "fn main() {}");
        write(&dir, "Cargo.toml", "[package]");
        write(&dir, "dist/app", "built");

        let proposal = propose_under(&dir, None);
        let block = format!("cache:\n{}", proposal.to_yaml(true));
        let workflow: crate::workspace::Workflow =
            crate::format::from_str(&block, crate::format::Format::Yaml)
                .unwrap_or_else(|e| panic!("generated cache section didn't parse: {e}\n\n{block}"));

        let cache = workflow.cache.expect("cache section written");
        assert!(cache.is_on());
        assert!(cache.inputs.contains(&"src/**/*".to_string()));
        assert_eq!(cache.outputs, vec!["dist/**/*".to_string()]);
        assert!(cache.exclude.contains(&"dist".to_string()));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn writing_the_section_preserves_the_rest_of_the_workflow() {
        let dir = scratch("write");
        std::fs::create_dir_all(dir.join(".ciabatta/workflows")).unwrap();
        std::fs::write(
            dir.join(".ciabatta/ciabatta.yaml"),
            "workspace:\n  name: api\n",
        )
        .unwrap();
        std::fs::write(
            dir.join(".ciabatta/workflows/build.yaml"),
            "# my careful comment\ndescription: Build it\nowner: Ada\nsteps:\n  \
             - name: compile\n    run: make\n",
        )
        .unwrap();
        write(&dir, "src/main.rs", "fn main() {}");
        write(&dir, "dist/app", "built");

        let proposal = propose_under(&dir, None);
        let written = write_cache_section(&dir, "build", &proposal, true, None, false).unwrap();
        let path = written[0].clone();
        assert_eq!(
            written.len(),
            1,
            "no remote asked for, so only the workflow"
        );

        let rendered = std::fs::read_to_string(&path).unwrap();
        assert!(rendered.contains("# my careful comment"));

        let workflow: crate::workspace::Workflow = crate::format::load(&path).unwrap();
        assert_eq!(workflow.owner.as_deref(), Some("Ada"));
        assert_eq!(workflow.steps.len(), 1, "the steps must survive");
        assert!(workflow.cache.unwrap().is_on());

        // A second run refuses rather than clobbering what's there…
        let err = write_cache_section(&dir, "build", &proposal, true, None, false).unwrap_err();
        assert!(err.to_string().contains("already has a `cache:` section"));

        // …unless asked to.
        assert!(write_cache_section(&dir, "build", &proposal, false, None, true).is_ok());
        let workflow: crate::workspace::Workflow = crate::format::load(&path).unwrap();
        assert!(!workflow.cache.unwrap().is_on());
        assert_eq!(
            rendered.matches("\ncache:").count(),
            1,
            "replacing must not leave two cache keys"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A step's own nested `cache:` is not the workflow's section — mistaking
    /// one for the other refuses `cache init` on any workflow that has ever
    /// been written by `ciabatta convert`.
    #[test]
    fn a_nested_cache_key_is_not_the_workflows_section() {
        let dir = scratch("nested");
        std::fs::create_dir_all(dir.join(".ciabatta/workflows")).unwrap();
        std::fs::write(
            dir.join(".ciabatta/ciabatta.yaml"),
            "workspace:\n  name: api\n",
        )
        .unwrap();
        std::fs::write(
            dir.join(".ciabatta/workflows/build.yaml"),
            concat!(
                "steps:\n",
                "  - name: build\n",
                "    run: make\n",
                "    cache:\n",
                "      enabled: false\n",
            ),
        )
        .unwrap();
        write(&dir, "src/main.rs", "fn main() {}");
        write(&dir, "dist/app", "built");

        let written =
            write_cache_section(&dir, "build", &propose_under(&dir, None), true, None, false)
                .expect("a step-level cache: must not block the workflow section");

        let workflow: crate::workspace::Workflow = crate::format::load(&written[0]).unwrap();
        assert!(workflow.cache.expect("workflow section written").is_on());
        assert!(
            workflow.steps[0].cache.is_some(),
            "and the step's own settings must survive"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cache_init_needs_a_workflow_to_write_into() {
        let dir = scratch("noworkflow");
        std::fs::create_dir_all(dir.join(".ciabatta")).unwrap();
        std::fs::write(
            dir.join(".ciabatta/ciabatta.yaml"),
            "workspace:\n  name: api\n",
        )
        .unwrap();

        let err = write_cache_section(&dir, "build", &propose_under(&dir, None), true, None, false)
            .unwrap_err();
        assert!(
            err.to_string().contains("No workflow called 'build'"),
            "{err}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The remote is one server per checkout, so it lands in `ciabatta.yaml`
    /// while the file definition stays with the workflow that reads them.
    #[test]
    fn a_remote_goes_to_the_config_and_the_files_to_the_workflow() {
        let dir = scratch("remote");
        std::fs::create_dir_all(dir.join(".ciabatta/workflows")).unwrap();
        std::fs::write(
            dir.join(".ciabatta/ciabatta.yaml"),
            "workspace:\n  name: api\n",
        )
        .unwrap();
        std::fs::write(
            dir.join(".ciabatta/workflows/build.yaml"),
            "steps:\n  - name: build\n    run: make\n",
        )
        .unwrap();
        write(&dir, "src/main.rs", "fn main() {}");
        write(&dir, "dist/app", "built");

        let written = write_cache_section(
            &dir,
            "build",
            &propose_under(&dir, None),
            true,
            Some("http://c:8380"),
            false,
        )
        .unwrap();
        assert_eq!(written.len(), 2, "the workflow file and the config");

        let workflow: crate::workspace::Workflow = crate::format::load(&written[0]).unwrap();
        let cache = workflow.cache.expect("workflow section written");
        assert!(cache.inputs.contains(&"src/**/*".to_string()));
        assert!(
            cache.remote.is_none(),
            "the server does not belong to any one workflow"
        );

        let config: CiabattaConfig = crate::format::load(&written[1]).unwrap();
        let remote = config.cache.and_then(|c| c.remote).expect("remote written");
        assert_eq!(remote.url, "http://c:8380");
        assert!(
            remote.project.is_none(),
            "the id is assigned by the server, not guessed by the client"
        );
        assert!(remote.enabled, "a configured remote defaults to on");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn durations_read_the_way_people_say_them() {
        assert_eq!(humanize_ms(0), "0s");
        assert_eq!(humanize_ms(4_200), "4s");
        assert_eq!(humanize_ms(90_000), "1m 30s");
        assert_eq!(humanize_ms(3_600_000 + 120_000), "1h 2m");
    }
}
