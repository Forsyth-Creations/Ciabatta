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

/// Resolves each step's cache settings and working directory from the
/// monorepo it came from.
///
/// A compiled workflow graph carries the sub-workspace on every node, which is
/// what lets one graph spanning four packages resolve four different `cache:`
/// sections — the alternative, one cache config for the whole repo, would be
/// wrong for every package but the first.
pub struct WorkspaceContext<'a> {
    pub workspace: Option<&'a Workspace>,
    /// The project root, for steps with no sub-workspace of their own.
    pub root: PathBuf,
    /// The root project's own config, used for those steps.
    pub config: &'a CiabattaConfig,
    /// The recipe being planned, when one recipe's settings apply throughout.
    pub recipe_cache: Option<CacheConfig>,
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

        super::graph::effective(
            workspace_level.as_ref(),
            self.recipe_cache.as_ref(),
            step.cache.as_ref(),
        )
    }

    fn dir(&self, step: &RunStep) -> PathBuf {
        // A workflow step runs in its own sub-workspace, and its inputs and
        // outputs are written relative to that — the same paths somebody would
        // type if they ran the script by hand.
        if let Some(member) = step
            .workspace
            .as_deref()
            .and_then(|name| self.workspace.and_then(|ws| ws.member(name)))
        {
            return member.dir.clone();
        }
        match step.cwd.as_deref() {
            Some(cwd) => self.root.join(cwd),
            None => self.root.clone(),
        }
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
pub fn propose(dir: &Path) -> Proposal {
    let mut inputs = Vec::new();
    let mut reasons = Vec::new();

    for (pattern, why) in INPUT_CANDIDATES {
        if matches_anything(dir, pattern) {
            inputs.push((*pattern).to_string());
            reasons.push(((*pattern).to_string(), *why));
        }
    }

    let outputs: Vec<String> = OUTPUT_DIRS
        .iter()
        .filter(|(name, _)| dir.join(name).is_dir())
        .map(|(name, _)| format!("{name}/**/*"))
        .collect();

    let exclude: Vec<String> = ALWAYS_EXCLUDE
        .iter()
        .filter(|name| dir.join(name).exists())
        .map(|name| (*name).to_string())
        // Anything proposed as an output must never also count as an input, or
        // every build would invalidate itself.
        .chain(
            OUTPUT_DIRS
                .iter()
                .filter(|(name, _)| dir.join(name).is_dir())
                .map(|(name, _)| (*name).to_string()),
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
    pub fn to_yaml(&self, enabled: bool, remote: Option<&str>) -> String {
        let mut out = String::new();
        out.push_str("  # What this workspace's builds read and write. Getting `inputs` right\n");
        out.push_str("  # is the part that matters: a build that reads a file not listed here\n");
        out.push_str("  # will be handed a stale result when that file changes.\n");
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

        if let Some(url) = remote {
            out.push_str("  remote:\n");
            out.push_str(&format!("    url: {url}\n"));
            out.push_str("    # `project` is filled in by the server the first time this\n");
            out.push_str("    # workspace connects. Commit it: it's what makes every checkout\n");
            out.push_str("    # and every CI runner resolve to the same project.\n");
        }

        out
    }
}

/// Recipes and steps that declare their own `cache:`, which will win over the
/// workspace section.
///
/// Worth reporting after `cache init`: the precedence is deliberate, but
/// "caching is on" followed by nothing being cached — because a step written by
/// `ciabatta convert` still says `enabled: false` — reads as the feature being
/// broken rather than as two settings doing exactly what they say.
pub fn overriding_steps(config: &CiabattaConfig) -> Vec<(String, bool)> {
    let mut found = Vec::new();

    for (name, entry) in &config.recipes {
        if let Some(cache) = &entry.cache {
            found.push((name.clone(), cache.enabled));
        }
        let Some(run) = entry.run_recipe() else {
            continue;
        };
        for step in &run.steps {
            if let Some(cache) = &step.cache {
                found.push((format!("{name}.{}", step.name), cache.enabled));
            }
        }
    }

    found.sort();
    found
}

/// Write a proposed `cache:` section into a project's config.
pub fn write_cache_section(
    root: &Path,
    proposal: &Proposal,
    enabled: bool,
    remote: Option<&str>,
    force: bool,
) -> Result<PathBuf> {
    let path = crate::config::config_path(root).ok_or_else(|| {
        anyhow::anyhow!(
            "No ciabatta config in {}. Run `ciabatta init` first.",
            root.display()
        )
    })?;

    let existing = std::fs::read_to_string(&path)
        .with_context(|| format!("Failed to read {}", path.display()))?;

    // Anchored at column 0: a recipe or a step may carry its own nested
    // `cache:` (`ciabatta convert` writes one), and that is not the section
    // this writes. Matching at any indentation would refuse `cache init` on
    // every project that has ever converted a script.
    if existing.lines().any(|l| l.starts_with("cache:")) && !force {
        bail!(
            "{} already has a `cache:` section. Edit it directly, or pass --force \
             to replace it.",
            path.display()
        );
    }

    let block = format!("cache:\n{}", proposal.to_yaml(enabled, remote));
    let rendered = crate::format::set_top_level(&existing, "cache", &block);

    // This edits a file the user owns; hand it back only if it still loads.
    let parsed: CiabattaConfig =
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

    #[test]
    fn the_proposal_comes_from_what_is_actually_in_the_directory() {
        let dir = scratch("propose");
        write(&dir, "src/main.rs", "fn main() {}");
        write(&dir, "scripts/build.sh", "#!/bin/sh");
        write(&dir, "Cargo.toml", "[package]");
        write(&dir, "Cargo.lock", "");
        write(&dir, "dist/app", "built");
        write(&dir, "node_modules/left-pad/index.js", "");

        let proposal = propose(&dir);
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
        let proposal = propose(&dir);
        assert!(!proposal.is_usable());

        let yaml = proposal.to_yaml(false, None);
        assert!(yaml.contains("enabled: false"));
        assert!(
            yaml.contains("TODO"),
            "an empty proposal must not look complete"
        );

        // It still has to parse, so `cache init` on a bare directory leaves a
        // config that loads.
        let block = format!("cache:\n{yaml}");
        let config: CiabattaConfig =
            crate::format::from_str(&block, crate::format::Format::Yaml).unwrap();
        let cache = config.cache.unwrap();
        assert!(!cache.enabled);
        assert!(cache.inputs.is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_generated_yaml_parses_back_into_the_config_it_claims_to_be() {
        let dir = scratch("yaml");
        write(&dir, "src/main.rs", "fn main() {}");
        write(&dir, "Cargo.toml", "[package]");
        write(&dir, "dist/app", "built");

        let proposal = propose(&dir);
        let block = format!(
            "cache:\n{}",
            proposal.to_yaml(true, Some("http://cache:8380"))
        );
        let config: CiabattaConfig = crate::format::from_str(&block, crate::format::Format::Yaml)
            .unwrap_or_else(|e| panic!("generated cache section didn't parse: {e}\n\n{block}"));

        let cache = config.cache.expect("cache section written");
        assert!(cache.enabled);
        assert!(cache.inputs.contains(&"src/**/*".to_string()));
        assert_eq!(cache.outputs, vec!["dist/**/*".to_string()]);
        assert!(cache.exclude.contains(&"dist".to_string()));

        let remote = cache.remote.expect("remote written");
        assert_eq!(remote.url, "http://cache:8380");
        assert!(
            remote.project.is_none(),
            "the id is assigned by the server, not guessed by the client"
        );
        assert!(remote.enabled, "a configured remote defaults to on");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn writing_the_section_preserves_the_rest_of_the_config() {
        let dir = scratch("write");
        std::fs::create_dir_all(dir.join(".ciabatta")).unwrap();
        std::fs::write(
            dir.join(".ciabatta/ciabatta.yaml"),
            "# my careful comment\nworkspace:\n  name: api\n  owner: Ada\n",
        )
        .unwrap();
        write(&dir, "src/main.rs", "fn main() {}");
        write(&dir, "dist/app", "built");

        let proposal = propose(&dir);
        let path = write_cache_section(&dir, &proposal, true, None, false).unwrap();

        let rendered = std::fs::read_to_string(&path).unwrap();
        assert!(rendered.contains("# my careful comment"));

        let config: CiabattaConfig = crate::format::load(&path).unwrap();
        assert_eq!(config.workspace.unwrap().name.as_deref(), Some("api"));
        assert!(config.cache.unwrap().enabled);

        // A second run refuses rather than clobbering what's there…
        let err = write_cache_section(&dir, &proposal, true, None, false).unwrap_err();
        assert!(err.to_string().contains("already has a `cache:` section"));

        // …unless asked to.
        assert!(write_cache_section(&dir, &proposal, false, None, true).is_ok());
        let config: CiabattaConfig = crate::format::load(&path).unwrap();
        assert!(!config.cache.unwrap().enabled);
        assert_eq!(
            rendered.matches("\ncache:").count(),
            1,
            "replacing must not leave two cache keys"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A step's own nested `cache:` is not the workspace's section — mistaking
    /// one for the other refuses `cache init` on any project that has ever run
    /// `ciabatta convert`.
    #[test]
    fn a_nested_cache_key_is_not_the_workspace_section() {
        let dir = scratch("nested");
        std::fs::create_dir_all(dir.join(".ciabatta")).unwrap();
        std::fs::write(
            dir.join(".ciabatta/ciabatta.yaml"),
            concat!(
                "recipies:\n",
                "  build:\n",
                "    run:\n",
                "      steps:\n",
                "        - name: build\n",
                "          run: make\n",
                "          cache:\n",
                "            enabled: false\n",
            ),
        )
        .unwrap();
        write(&dir, "src/main.rs", "fn main() {}");
        write(&dir, "dist/app", "built");

        let path = write_cache_section(&dir, &propose(&dir), true, None, false)
            .expect("a step-level cache: must not block the workspace section");

        let config: CiabattaConfig = crate::format::load(&path).unwrap();
        assert!(config.cache.expect("workspace section written").enabled);
        assert!(
            config.recipes["build"].run_recipe().unwrap().steps[0]
                .cache
                .is_some(),
            "and the step's own settings must survive"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cache_init_needs_a_project_to_write_into() {
        let dir = scratch("noproject");
        let err = write_cache_section(&dir, &propose(&dir), true, None, false).unwrap_err();
        assert!(err.to_string().contains("ciabatta init"));
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
