//! The `run` paradigm: a recipe direction whose main stage runs a DAG of
//! dependent script "steps", with `on_error` branches to recovery nodes that
//! offer a choice of fix scripts.
//!
//! This module owns the run config types (referenced from [`crate::config`]),
//! the loader that resolves a recipe's step DAG from a separate flowchart file,
//! the validation of that DAG, and the async engine that executes it. The live
//! web view and the visual builder are served by the daemon, in
//! [`crate::daemon::routes::run`].

pub mod engine;
pub mod filter;
pub mod view;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::Path;

pub use engine::execute;

/// The `[recipies.<name>.run]` sub-table: the run direction of a recipe.
///
/// The step DAG itself normally lives in a separate flowchart file (via the
/// `flowchart` path and optional `entry`); the `login`/`pre`/`post` phase hooks
/// stay in the main config, mirroring the push/pull stage overrides. Inline
/// `steps` are also accepted, so a small pipeline needs no second file.
#[derive(Debug, Deserialize, Serialize, Clone, Default)]
pub struct RunRecipe {
    /// Path (relative to the project root) to a flowchart TOML file holding the
    /// step DAG. When set, its `entry` table supplies the steps.
    pub flowchart: Option<String>,
    /// Which top-level entry of the flowchart file to use. Defaults to the
    /// recipe's own name.
    pub entry: Option<String>,

    /// Override the `login` phase (default: no-op for runs).
    pub login: Option<String>,
    /// Override the `pre-run` phase (default: no-op).
    pub pre: Option<String>,
    /// Override the `post-run` phase (default: no-op).
    pub post: Option<String>,

    /// Path(s) (relative to the project root) to `.env` file(s) sourced before
    /// the run starts. Each `KEY=VALUE` line is loaded into the run's
    /// environment so its phases and steps can see it; values already resolved
    /// (from the ambient environment, CI, git, or `-e` flags) take precedence,
    /// and later files override earlier ones. Missing files are an error.
    /// Accepts a single path (`env_file = ".env"`) or a list
    /// (`env_file = [".env", ".env.run"]`). Merged with the flowchart file's
    /// own `env_file` when a `flowchart` file is used.
    ///
    /// Each path may contain `{VAR}` placeholders resolved from the current
    /// environment, so which file is sourced can be selected at run time:
    /// `env_file = ".env.{RUN_ENV}"` sources `.env.dev` or `.env.prod`
    /// depending on `RUN_ENV`.
    #[serde(default, deserialize_with = "string_or_vec")]
    pub env_file: Vec<String>,

    /// Environment variables that must be set (and non-empty) for the run to
    /// start. Checked before any phase executes; if any are empty/unset the run
    /// is aborted with an error. Merged with the flowchart file's own
    /// `REQUIRED_ENV` when a `flowchart` file is used.
    #[serde(default, rename = "REQUIRED_ENV")]
    pub required_env: Vec<String>,

    /// Steps written inline, when not using a separate `flowchart` file.
    #[serde(default)]
    pub steps: Vec<RunStep>,
}

/// Deserialize a field that TOML may express as either a bare string
/// (`env_file = ".env"`) or an array (`env_file = [".env", ".env.run"]`) into
/// a `Vec<String>`. Shared by `env_file` and the step conditions (`when` /
/// `skip_if`), all of which accept one-or-many.
pub(crate) fn string_or_vec<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum OneOrMany {
        One(String),
        Many(Vec<String>),
    }
    Ok(match OneOrMany::deserialize(deserializer)? {
        OneOrMany::One(s) => vec![s],
        OneOrMany::Many(v) => v,
    })
}

/// A node in the run flowchart: either a normal step (runs an action once its
/// `needs` succeed) or a recovery node (`recover = true`, entered only via some
/// step's `on_error`, offering a choice of fix `options`).
#[derive(Debug, Deserialize, Serialize, Clone, Default)]
pub struct RunStep {
    /// Unique node name; the target of `needs` / `on_error` / `retry` edges.
    pub name: String,

    /// A bash script to run (path relative to the step's working directory).
    pub script: Option<String>,
    /// An inline shell command (`sh -c`), as an alternative to `script`.
    pub run: Option<String>,

    // ─── Documentation & ownership ──────────────────────────────────────────
    // Every script in a monorepo should say what it does and who to ask about
    // it; `ciabatta list` prints both, so nobody has to open the file to find
    // out. `ciabatta init --lib` scaffolds them and nags when they're missing.
    /// One line on what this step does, and what it expects to be true first.
    pub description: Option<String>,
    /// Who owns this step — a name, handle, or team.
    pub owner: Option<String>,
    /// Free-form labels — `fast`, `slow`, `integration`, `frontend`. A compiled
    /// workflow graph folds its sub-workspace's and workflow's tags in here, so
    /// every node carries the full set it inherited.
    ///
    /// These are what `--filter tag:fast` selects on, which is how you run one
    /// slice of a graph without running all of it.
    #[serde(default)]
    pub tags: Vec<String>,

    // ─── Requirements ───────────────────────────────────────────────────────
    /// Executables that must be on `PATH` for this step to work (`cargo`,
    /// `protoc`, `docker`, …). Checked before the graph starts, so a missing
    /// toolchain is reported up front — with the fix from the matching
    /// `[toolchain.<tool>]` entry — instead of surfacing as "command not found"
    /// halfway through a build.
    #[serde(default)]
    pub requires: Vec<String>,

    // ─── Long-running & flaky steps ─────────────────────────────────────────
    /// Wall-clock limit for this step: a duration (`"90s"`, `"10m"`, `"1h30m"`)
    /// or a bare number of seconds. When it expires the step is killed and
    /// marked timed-out, and — since a hung step must not hold up everything
    /// else — the rest of the graph carries on (see `continue_on_error`).
    pub timeout: Option<String>,
    /// Extra attempts to make when the step fails, for transient errors
    /// (default 0: one attempt, no retry).
    #[serde(default)]
    pub retries: u32,
    /// A step that never exits on its own — a dev server, a log tailer, a
    /// watcher. It is started, its dependents are released immediately, and it
    /// keeps running for the rest of the graph rather than hanging it. Follow
    /// its output with `ciabatta watch`.
    #[serde(default)]
    pub persistent: bool,
    /// Don't fail the whole run when this step fails: its dependents are
    /// skipped, every other branch carries on, and the run reports the failure
    /// at the end. Always applied when a `timeout` expires.
    #[serde(default)]
    pub continue_on_error: bool,

    // ─── Phase & placement ──────────────────────────────────────────────────
    /// The special, identifiable phase this step belongs to: `push`, `setup`,
    /// `build`, `test`, `deploy`, … Free-form and cosmetic (the graph labels
    /// the node with it) except for `push`, which gets a built-in action —
    /// see `recipe`.
    pub kind: Option<String>,
    /// For `kind = "push"` steps: the `[recipies]` recipe to publish. With no
    /// `script`/`run` of its own such a step runs `ciabatta push <recipe>` in
    /// its own sub-workspace, so publishing is just another node on the graph.
    pub recipe: Option<String>,
    /// Working directory for this step's action, relative to the project root.
    /// Workflow graphs set it to the owning sub-workspace's directory, so its
    /// scripts run where they were written to run. Defaults to the root.
    pub cwd: Option<String>,
    /// Which sub-workspace this step came from. Filled in when a workspace
    /// workflow graph is compiled; it's what labels each node on the graph.
    pub workspace: Option<String>,
    /// Environment variables for this step alone, layered over the run's own.
    /// A compiled workflow graph folds each sub-workspace's standard variables
    /// in here, so two members' settings can't collide in one shared map.
    #[serde(default)]
    pub env: std::collections::BTreeMap<String, String>,

    /// Names of steps that must succeed before this one runs (the success DAG).
    #[serde(default)]
    pub needs: Vec<String>,
    /// On failure, jump to this recovery node instead of aborting the run.
    pub on_error: Option<String>,

    /// Condition(s) that must ALL hold for this step to run; if any is false the
    /// step is skipped (and treated as satisfied, so its dependents still run).
    /// Accepts one condition or a list. Each is evaluated against the run's
    /// environment, e.g. `when = "env.RUN_ENV == prod"` or
    /// `when = ["env.RUN_ENV == prod", "REGION == us-east-1"]`.
    #[serde(default, deserialize_with = "string_or_vec")]
    pub when: Vec<String>,
    /// Condition(s) that skip this step when ANY holds — the inverse of `when`,
    /// matching "skip if …". Accepts one condition or a list, e.g.
    /// `skip_if = "env.IN_CI == true"`.
    #[serde(default, deserialize_with = "string_or_vec")]
    pub skip_if: Vec<String>,

    /// Marks this node as a recovery node: it presents `options` rather than
    /// running an action of its own.
    #[serde(default)]
    pub recover: bool,
    /// Prompt shown when a recovery node is reached.
    pub message: Option<String>,
    /// After a chosen fix succeeds, re-run this node (typically the failed step).
    pub retry: Option<String>,
    /// The fix choices offered by a recovery node.
    #[serde(default)]
    pub options: Vec<FixOption>,
}

/// One fix choice on a recovery node.
#[derive(Debug, Deserialize, Serialize, Clone, Default)]
pub struct FixOption {
    /// Human-readable label shown in the UI / GUI.
    pub label: String,
    /// A bash script to run as the fix (path relative to the project root).
    pub script: Option<String>,
    /// An inline shell command, as an alternative to `script`.
    pub run: Option<String>,
    /// Run this option automatically in non-interactive mode (plain / CI), where
    /// no operator is present to choose. The first `default` option wins.
    #[serde(default)]
    pub default: bool,
}

impl RunStep {
    /// Whether this node runs an action of its own (as opposed to a pure
    /// recovery node that only presents options). A `kind = "push"` step that
    /// names a `recipe` counts: its action is the built-in `ciabatta push`.
    pub fn has_action(&self) -> bool {
        self.script.is_some() || self.run.is_some() || self.recipe.is_some()
    }

    /// Whether this step publishes — the special, identifiable "push" phase of
    /// a workflow, however its action is spelled.
    pub fn is_push(&self) -> bool {
        self.kind
            .as_deref()
            .is_some_and(|k| k.eq_ignore_ascii_case("push"))
            || self.recipe.is_some()
    }

    /// This step's configured wall-clock limit, if any.
    pub fn timeout_duration(&self) -> Result<Option<std::time::Duration>> {
        match self.timeout.as_deref() {
            None => Ok(None),
            // One flat message rather than a context chain: this surfaces
            // through `validate_flowchart`, whose callers print only the
            // top-level error.
            Some(raw) => parse_duration(raw).map(Some).map_err(|err| {
                anyhow::anyhow!("Step '{}' has an invalid timeout: {err}", self.name)
            }),
        }
    }

    /// The action this step runs, resolving the `kind = "push"` shorthand into
    /// the `ciabatta push` invocation it stands for. Returns `(script, run)`,
    /// mirroring the `script` / `run` pair the executor takes.
    pub fn action(&self) -> (Option<&str>, Option<String>) {
        match (self.script.as_deref(), self.run.as_deref()) {
            (None, None) => (
                None,
                self.recipe.as_deref().map(|recipe| {
                    // Re-invoke this very binary, so a graph run and a manual
                    // `ciabatta push` do exactly the same thing.
                    let exe = std::env::current_exe()
                        .ok()
                        .map(|p| p.to_string_lossy().into_owned())
                        .unwrap_or_else(|| "ciabatta".to_string());
                    format!("{exe} push {recipe} --no-tui")
                }),
            ),
            (script, run) => (script, run.map(str::to_string)),
        }
    }
}

/// Parse a step `timeout` into a [`Duration`](std::time::Duration).
///
/// Accepts a bare number of seconds (`"90"`, or `timeout = 90`), or a sequence
/// of unit-suffixed parts: `h`/`hour(s)`, `m`/`min(s)`/`minute(s)`,
/// `s`/`sec(s)`/`second(s)`, `ms`. Parts combine, so `"1h30m"` and
/// `"90m"` are the same limit.
pub fn parse_duration(raw: &str) -> Result<std::time::Duration> {
    let text = raw.trim().to_ascii_lowercase();
    if text.is_empty() {
        bail!("empty duration");
    }
    // Bare number → seconds, the form `timeout = 600` deserializes into.
    if let Ok(secs) = text.parse::<f64>() {
        if secs < 0.0 {
            bail!("negative duration '{raw}'");
        }
        return Ok(std::time::Duration::from_secs_f64(secs));
    }

    let mut total = std::time::Duration::ZERO;
    let mut number = String::new();
    let mut unit = String::new();
    let mut parts = 0usize;

    // Walk the string folding each "<number><unit>" pair into the total.
    let mut flush = |number: &mut String, unit: &mut String, total: &mut std::time::Duration| {
        if number.is_empty() {
            return Ok(());
        }
        let value: f64 = number
            .parse()
            .map_err(|_| anyhow::anyhow!("invalid number '{number}' in duration '{raw}'"))?;
        let seconds = match unit.as_str() {
            "ms" => value / 1000.0,
            "s" | "sec" | "secs" | "second" | "seconds" => value,
            "m" | "min" | "mins" | "minute" | "minutes" => value * 60.0,
            "h" | "hr" | "hrs" | "hour" | "hours" => value * 3600.0,
            other => bail!("unknown duration unit '{other}' in '{raw}'"),
        };
        *total += std::time::Duration::from_secs_f64(seconds);
        number.clear();
        unit.clear();
        parts += 1;
        Ok(())
    };

    for ch in text.chars() {
        if ch.is_ascii_digit() || ch == '.' {
            // A digit after a unit closes the previous part (`1h30m`).
            if !unit.is_empty() {
                flush(&mut number, &mut unit, &mut total)?;
            }
            number.push(ch);
        } else if ch.is_ascii_alphabetic() {
            unit.push(ch);
        } else if ch.is_whitespace() {
            continue;
        } else {
            bail!("unexpected character '{ch}' in duration '{raw}'");
        }
    }
    flush(&mut number, &mut unit, &mut total)?;

    if parts == 0 {
        bail!("could not parse duration '{raw}' (try \"30s\", \"10m\", or \"1h30m\")");
    }
    Ok(total)
}

/// A flowchart file: a map of entry name → its step list.
///
/// ```toml
/// [web]
///   [[web.steps]]
///   name = "build"
///   script = "scripts/build.sh"
/// ```
#[derive(Debug, Deserialize, Default)]
pub struct FlowchartFile {
    #[serde(flatten)]
    pub entries: HashMap<String, Flowchart>,
}

/// One named flowchart within a [`FlowchartFile`].
#[derive(Debug, Deserialize, Clone, Default)]
pub struct Flowchart {
    /// Environment variables that must be set (and non-empty) for this
    /// flowchart's steps to run. Empty/unset variables abort the run before
    /// any phase executes.
    #[serde(default, rename = "REQUIRED_ENV")]
    pub required_env: Vec<String>,
    /// `.env` file path(s) sourced before this flowchart's run starts. Merged
    /// with any declared on the recipe's `[run]` table.
    #[serde(default, deserialize_with = "string_or_vec")]
    pub env_file: Vec<String>,
    #[serde(default)]
    pub steps: Vec<RunStep>,
}

/// A fully resolved run, ready to execute: the phase hooks plus the validated
/// step DAG.
#[derive(Debug, Clone, Default)]
pub struct ResolvedRun {
    pub login: Option<String>,
    pub pre: Option<String>,
    pub post: Option<String>,
    /// Variables that must be set (non-empty) before the run may start.
    pub required_env: Vec<String>,
    /// `.env` file paths (relative to the project root) to source before the
    /// run starts, in the order they should be applied.
    pub env_files: Vec<String>,
    pub steps: Vec<RunStep>,
}

impl ResolvedRun {
    /// Look up a step node by name.
    pub fn step(&self, name: &str) -> Option<&RunStep> {
        self.steps.iter().find(|s| s.name == name)
    }
}

/// Parse the contents of a `.env` file into ordered `KEY=VALUE` pairs.
///
/// Supports the common `.env` shape: blank lines and `#` comments are ignored,
/// an optional leading `export ` is stripped, and values may be wrapped in
/// single or double quotes (the quotes are removed). Values are otherwise taken
/// verbatim (leading/trailing whitespace trimmed for unquoted values). Lines
/// without an `=` are skipped.
pub(crate) fn parse_env_content(content: &str) -> Vec<(String, String)> {
    let mut pairs = Vec::new();
    for raw in content.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line);
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        if key.is_empty() {
            continue;
        }
        let value = value.trim();
        // Strip a single pair of matching surrounding quotes, if present.
        let value = if value.len() >= 2
            && ((value.starts_with('"') && value.ends_with('"'))
                || (value.starts_with('\'') && value.ends_with('\'')))
        {
            &value[1..value.len() - 1]
        } else {
            value
        };
        pairs.push((key.to_string(), value.to_string()));
    }
    pairs
}

/// Read the resolved `.env` files (relative to `root`) and merge their variables
/// on top of `base`, returning the combined environment for the run.
///
/// Precedence: values already present and non-empty in `base` (the ambient
/// environment, CI, git, or `-e` flags) win, so a `.env` only supplies what
/// isn't already set. Among the files themselves, later files override earlier
/// ones. A missing or unreadable file is an error.
pub fn load_env_files(
    files: &[String],
    root: &Path,
    base: &HashMap<String, String>,
) -> Result<HashMap<String, String>> {
    let mut merged = base.clone();
    for rel in files {
        let path = root.join(rel);
        let content = std::fs::read_to_string(&path)
            .with_context(|| format!("Failed to read env file '{}'", path.display()))?;
        for (key, value) in parse_env_content(&content) {
            // A value already resolved (and non-empty) in `base` wins over every
            // file; but a later file may override an earlier file's value.
            let pinned_by_base = base.get(&key).is_some_and(|v| !v.trim().is_empty());
            if !pinned_by_base {
                merged.insert(key, value);
            }
        }
    }
    Ok(merged)
}

/// A run's environment, resolved as far as the inputs allow, plus whatever it
/// still needs before it may start. Produced by [`prepare_env`].
#[derive(Debug, Clone, Default)]
pub struct PreparedEnv {
    /// What every phase and step will see. Only complete when [`is_ready`] is
    /// true; otherwise it's as far as resolution got.
    ///
    /// [`is_ready`]: PreparedEnv::is_ready
    pub env: HashMap<String, String>,
    /// The `env_file` paths that were resolved and sourced, in order.
    pub sourced: Vec<String>,
    /// `{VAR}` placeholders in `env_file` paths with no value, so the file to
    /// source can't even be named yet.
    pub unresolved_paths: Vec<String>,
    /// `REQUIRED_ENV` variables that are empty or unset.
    pub missing_required: Vec<String>,
}

impl PreparedEnv {
    /// Whether the run may start.
    pub fn is_ready(&self) -> bool {
        self.unresolved_paths.is_empty() && self.missing_required.is_empty()
    }

    /// Every variable a caller must supply before the run can start, in the
    /// order worth asking for them: the ones gating which `.env` file gets
    /// sourced first, since sourcing it may well satisfy the rest.
    pub fn missing(&self) -> Vec<String> {
        let mut all = self.unresolved_paths.clone();
        for var in &self.missing_required {
            if !all.contains(var) {
                all.push(var.clone());
            }
        }
        all
    }
}

/// Resolve a run's environment: substitute `{VAR}` placeholders in its
/// `env_file` paths, source those files over `base`, and check the result
/// against `REQUIRED_ENV`.
///
/// Never runs anything, so both the engine (which does this for real, just
/// before the first phase) and the daemon's launcher (which does it to decide
/// whether to prompt for what's missing) can call it on the same inputs and
/// agree on the answer.
///
/// Unresolvable `env_file` paths short-circuit: nothing can be sourced until
/// the path is known, and a file that *would* have been sourced may be exactly
/// what supplies the `REQUIRED_ENV` variables — so those aren't reported yet.
pub fn prepare_env(
    resolved: &ResolvedRun,
    root: &Path,
    base: &HashMap<String, String>,
) -> Result<PreparedEnv> {
    let mut unresolved_paths: Vec<String> = Vec::new();
    let mut files: Vec<String> = Vec::with_capacity(resolved.env_files.len());
    for raw in &resolved.env_files {
        let unresolved = crate::config::unresolved_vars(raw, base);
        if unresolved.is_empty() {
            files.push(crate::config::substitute_vars(raw, base)?);
        } else {
            for var in unresolved {
                if !unresolved_paths.contains(&var) {
                    unresolved_paths.push(var);
                }
            }
        }
    }

    if !unresolved_paths.is_empty() {
        return Ok(PreparedEnv {
            env: base.clone(),
            unresolved_paths,
            ..Default::default()
        });
    }

    let env = if files.is_empty() {
        base.clone()
    } else {
        load_env_files(&files, root, base)?
    };
    let missing_required = missing_required_env(&resolved.required_env, &env);

    Ok(PreparedEnv {
        env,
        sourced: files,
        unresolved_paths,
        missing_required,
    })
}

/// Return the names from `required` that are absent from `env` or present but
/// empty (after trimming). An empty result means every required variable is
/// set, so the run may proceed. Order follows `required` so the reported list
/// matches how the operator declared `REQUIRED_ENV`.
fn missing_required_env(required: &[String], env: &HashMap<String, String>) -> Vec<String> {
    required
        .iter()
        .filter(|key| {
            env.get(key.as_str())
                .map(|v| v.trim().is_empty())
                .unwrap_or(true)
        })
        .cloned()
        .collect()
}

/// Look up a variable's value for a condition, tolerating a leading `env.`
/// prefix (`env.IN_CI` and `IN_CI` are equivalent). Unset variables read as "".
fn cond_var<'a>(name: &str, env: &'a HashMap<String, String>) -> &'a str {
    let name = name.trim().strip_prefix("env.").unwrap_or(name.trim());
    env.get(name).map(String::as_str).unwrap_or("")
}

/// Whether a value counts as "truthy" for a bare-variable condition: set and
/// non-empty, and not one of the common falsey words.
fn cond_truthy(val: &str) -> bool {
    let v = val.trim();
    !v.is_empty()
        && !matches!(
            v.to_ascii_lowercase().as_str(),
            "false" | "0" | "no" | "off"
        )
}

/// Strip a single pair of matching surrounding quotes from a comparison operand.
fn cond_unquote(s: &str) -> &str {
    let s = s.trim();
    if s.len() >= 2
        && ((s.starts_with('"') && s.ends_with('"')) || (s.starts_with('\'') && s.ends_with('\'')))
    {
        &s[1..s.len() - 1]
    } else {
        s
    }
}

/// Evaluate a single step condition against the run's environment.
///
/// Supported forms (the variable may carry an optional `env.` prefix):
///   * `VAR == value` / `VAR != value` — string comparison of `VAR`'s value
///     (unset reads as empty); the right side may be quoted.
///   * `VAR` — true when `VAR` is truthy (set, non-empty, not `false`/`0`/`no`/`off`).
///   * `!VAR` — the negation of the truthy test.
pub fn eval_condition(cond: &str, env: &HashMap<String, String>) -> Result<bool> {
    let cond = cond.trim();
    if cond.is_empty() {
        bail!("empty condition");
    }
    if let Some((lhs, rhs)) = cond.split_once("!=") {
        return Ok(cond_var(lhs, env) != cond_unquote(rhs));
    }
    if let Some((lhs, rhs)) = cond.split_once("==") {
        return Ok(cond_var(lhs, env) == cond_unquote(rhs));
    }
    if let Some(rest) = cond.strip_prefix('!') {
        return Ok(!cond_truthy(cond_var(rest, env)));
    }
    Ok(cond_truthy(cond_var(cond, env)))
}

/// Decide whether a step should be skipped given the environment, returning a
/// short human-readable reason when it should. A step is skipped if any
/// `skip_if` condition holds, or if any `when` condition does not hold (all
/// `when` conditions must be true to run).
pub fn step_skip_reason(step: &RunStep, env: &HashMap<String, String>) -> Result<Option<String>> {
    for cond in &step.skip_if {
        if eval_condition(cond, env)? {
            return Ok(Some(format!("skip_if `{cond}`")));
        }
    }
    for cond in &step.when {
        if !eval_condition(cond, env)? {
            return Ok(Some(format!("when `{cond}` not met")));
        }
    }
    Ok(None)
}

/// Resolve a recipe's run definition into runnable steps: load the separate
/// flowchart file when one is referenced, otherwise use the inline steps, then
/// validate the resulting DAG.
///
/// `recipe_name` is used as the default flowchart entry when `entry` is unset.
pub fn resolve_run(run: &RunRecipe, recipe_name: &str, root: &Path) -> Result<ResolvedRun> {
    // Variables required by the flowchart entry (if a file is used); merged with
    // any declared on the recipe's `[run]` table below.
    let mut required_env: Vec<String> = Vec::new();
    // `.env` files to source, in application order: flowchart-entry files first,
    // then recipe-level files (so a recipe can layer overrides on top).
    let mut env_files: Vec<String> = Vec::new();

    let steps = match run.flowchart.as_deref() {
        Some(rel) => {
            let path = root.join(rel);
            let content = std::fs::read_to_string(&path).with_context(|| {
                format!(
                    "Failed to read flowchart file '{}' for recipe '{}'",
                    path.display(),
                    recipe_name
                )
            })?;
            let file: FlowchartFile = toml::from_str(&content)
                .with_context(|| format!("Failed to parse flowchart file '{}'", path.display()))?;
            let entry = run.entry.as_deref().unwrap_or(recipe_name);
            let chart = file.entries.get(entry).ok_or_else(|| {
                let mut names: Vec<&String> = file.entries.keys().collect();
                names.sort();
                anyhow::anyhow!(
                    "Flowchart file '{}' has no entry '{}'. Available entries: {}.",
                    path.display(),
                    entry,
                    if names.is_empty() {
                        "(none)".to_string()
                    } else {
                        names
                            .iter()
                            .map(|s| s.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    }
                )
            })?;
            if !run.steps.is_empty() {
                bail!(
                    "Recipe '{}' run defines both a `flowchart` file and inline `steps`; use one or the other.",
                    recipe_name
                );
            }
            required_env.extend(chart.required_env.iter().cloned());
            env_files.extend(chart.env_file.iter().cloned());
            chart.steps.clone()
        }
        None => run.steps.clone(),
    };

    // Recipe-level `REQUIRED_ENV` applies to both inline and file flowcharts;
    // fold it in, de-duplicating so a var listed in both places is checked once.
    for var in &run.required_env {
        if !required_env.contains(var) {
            required_env.push(var.clone());
        }
    }

    // Recipe-level `env_file`(s) are applied after the flowchart's, de-duplicated
    // so the same path listed in both places is sourced once.
    for file in run.env_file.iter() {
        if !env_files.contains(file) {
            env_files.push(file.clone());
        }
    }

    let resolved = ResolvedRun {
        login: run.login.clone(),
        pre: run.pre.clone(),
        post: run.post.clone(),
        required_env,
        env_files,
        steps,
    };
    validate_flowchart(&resolved.steps, recipe_name)?;
    let mut resolved = resolved;
    resolved.steps = topo_order(&resolved.steps);
    Ok(resolved)
}

/// Reorder steps so that dependencies always precede their dependents, giving
/// both the executor and the live view (`--gui`) a logical top-to-bottom order
/// regardless of how the flowchart file happened to list them.
///
/// Normal steps are topologically sorted over their `needs` edges, with ties
/// broken by original position so the result is stable and deterministic. Each
/// recovery node is placed immediately after the first step that routes to it
/// via `on_error` (where an operator would encounter it); any recovery node not
/// referenced that way is appended at the end so nothing is dropped.
///
/// Assumes the DAG has already passed [`validate_flowchart`] (acyclic, every
/// edge resolves), so a total order always exists.
fn topo_order(steps: &[RunStep]) -> Vec<RunStep> {
    use std::collections::VecDeque;

    let idx_of: HashMap<&str, usize> = steps
        .iter()
        .enumerate()
        .map(|(i, s)| (s.name.as_str(), i))
        .collect();
    let normal: Vec<usize> = (0..steps.len()).filter(|&i| !steps[i].recover).collect();
    let is_normal = |name: &str| idx_of.get(name).is_some_and(|&i| !steps[i].recover);

    // In-degree counts `needs` edges to other normal steps only (recovery nodes
    // are entered via `on_error`, not the success DAG).
    let mut indegree: HashMap<usize, usize> = normal.iter().map(|&i| (i, 0)).collect();
    for &i in &normal {
        for dep in &steps[i].needs {
            if is_normal(dep) {
                *indegree.get_mut(&i).unwrap() += 1;
            }
        }
    }

    // Kahn's algorithm. The ready queue is seeded — and refilled — in original
    // order, so among steps with satisfied dependencies the author's ordering is
    // preserved.
    let mut ready: VecDeque<usize> = normal
        .iter()
        .copied()
        .filter(|i| indegree[i] == 0)
        .collect();
    let mut ordered_normal: Vec<usize> = Vec::with_capacity(normal.len());
    while let Some(node) = ready.pop_front() {
        ordered_normal.push(node);
        for &j in &normal {
            if steps[j]
                .needs
                .iter()
                .any(|d| idx_of.get(d.as_str()) == Some(&node))
            {
                let deg = indegree.get_mut(&j).unwrap();
                *deg -= 1;
                if *deg == 0 {
                    ready.push_back(j);
                }
            }
        }
    }

    // Emit each normal step, dropping in the recovery node it first routes to
    // right after it.
    let mut result: Vec<RunStep> = Vec::with_capacity(steps.len());
    let mut placed = vec![false; steps.len()];
    for &node in &ordered_normal {
        result.push(steps[node].clone());
        placed[node] = true;
        if let Some(target) = steps[node].on_error.as_deref()
            && let Some(&ri) = idx_of.get(target)
            && steps[ri].recover
            && !placed[ri]
        {
            result.push(steps[ri].clone());
            placed[ri] = true;
        }
    }
    // Anything not yet emitted (unreferenced recovery nodes, or normal steps a
    // malformed-but-validated graph left out) follows in original order.
    for (i, step) in steps.iter().enumerate() {
        if !placed[i] {
            result.push(step.clone());
        }
    }
    result
}

/// Validate a resolved step DAG:
///   - at least one step, unique names
///   - normal steps have an action; recovery nodes have ≥1 option
///   - `needs` / `on_error` / `retry` reference existing nodes
///   - `on_error` targets are recovery nodes
///   - the success DAG (`needs` edges) is acyclic
pub fn validate_flowchart(steps: &[RunStep], recipe_name: &str) -> Result<()> {
    if steps.is_empty() {
        bail!("Run recipe '{}' has no steps.", recipe_name);
    }

    let mut names: HashSet<&str> = HashSet::new();
    for step in steps {
        if step.name.trim().is_empty() {
            bail!(
                "Run recipe '{}' has a step with an empty name.",
                recipe_name
            );
        }
        if !names.insert(step.name.as_str()) {
            bail!(
                "Run recipe '{}' has a duplicate step name '{}'.",
                recipe_name,
                step.name
            );
        }
    }

    let is_recover = |name: &str| steps.iter().any(|s| s.name == name && s.recover);

    for step in steps {
        // Action / recovery shape.
        if step.recover {
            if step.options.is_empty() {
                bail!(
                    "Recovery node '{}' in run '{}' has no options.",
                    step.name,
                    recipe_name
                );
            }
            for opt in &step.options {
                if opt.label.trim().is_empty() {
                    bail!(
                        "Recovery node '{}' in run '{}' has an option with no label.",
                        step.name,
                        recipe_name
                    );
                }
            }
        } else if !step.has_action() {
            bail!(
                "Step '{}' in run '{}' has no `script`, `run`, or `recipe` to execute.",
                step.name,
                recipe_name
            );
        }

        // A timeout that doesn't parse would silently become "no limit" — the
        // opposite of what an operator guarding a hanging step intended.
        if let Err(err) = step.timeout_duration() {
            bail!("{err} (in run '{recipe_name}')");
        }

        // Conditions must be non-blank so a typo can't silently read as "run".
        for cond in step.when.iter().chain(step.skip_if.iter()) {
            if cond.trim().is_empty() {
                bail!(
                    "Step '{}' in run '{}' has an empty `when`/`skip_if` condition.",
                    step.name,
                    recipe_name
                );
            }
        }

        // Edge targets must exist.
        for dep in &step.needs {
            if !names.contains(dep.as_str()) {
                bail!(
                    "Step '{}' in run '{}' needs '{}', which is not a defined step.",
                    step.name,
                    recipe_name,
                    dep
                );
            }
        }
        if let Some(target) = step.on_error.as_deref() {
            if !names.contains(target) {
                bail!(
                    "Step '{}' in run '{}' has on_error = '{}', which is not a defined step.",
                    step.name,
                    recipe_name,
                    target
                );
            }
            if !is_recover(target) {
                bail!(
                    "Step '{}' in run '{}' routes on_error to '{}', which is not a recovery node (set `recover = true`).",
                    step.name,
                    recipe_name,
                    target
                );
            }
        }
        if let Some(target) = step.retry.as_deref()
            && !names.contains(target)
        {
            bail!(
                "Recovery node '{}' in run '{}' has retry = '{}', which is not a defined step.",
                step.name,
                recipe_name,
                target
            );
        }
    }

    detect_cycle(steps, recipe_name)?;
    Ok(())
}

/// Depth-first cycle detection over the success DAG (`needs` edges only).
fn detect_cycle(steps: &[RunStep], recipe_name: &str) -> Result<()> {
    #[derive(Clone, Copy, PartialEq)]
    enum Mark {
        Unvisited,
        InProgress,
        Done,
    }

    let index: HashMap<&str, usize> = steps
        .iter()
        .enumerate()
        .map(|(i, s)| (s.name.as_str(), i))
        .collect();
    let mut marks = vec![Mark::Unvisited; steps.len()];

    // Iterative DFS to avoid stack overflow on pathological graphs.
    for start in 0..steps.len() {
        if marks[start] != Mark::Unvisited {
            continue;
        }
        // Stack of (node, next-dep-cursor).
        let mut stack: Vec<(usize, usize)> = vec![(start, 0)];
        marks[start] = Mark::InProgress;
        while let Some(&(node, cursor)) = stack.last() {
            let deps = &steps[node].needs;
            if cursor < deps.len() {
                stack.last_mut().unwrap().1 += 1;
                if let Some(&next) = index.get(deps[cursor].as_str()) {
                    match marks[next] {
                        Mark::InProgress => bail!(
                            "Run '{}' has a dependency cycle involving step '{}'.",
                            recipe_name,
                            steps[next].name
                        ),
                        Mark::Unvisited => {
                            marks[next] = Mark::InProgress;
                            stack.push((next, 0));
                        }
                        Mark::Done => {}
                    }
                }
            } else {
                marks[node] = Mark::Done;
                stack.pop();
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn steps_from(toml_str: &str) -> Vec<RunStep> {
        let chart: Flowchart = toml::from_str(toml_str).expect("flowchart parses");
        chart.steps
    }

    #[test]
    fn valid_linear_and_dag_flowcharts_pass() {
        let steps = steps_from(
            r#"
[[steps]]
name = "build"
script = "b.sh"
[[steps]]
name = "test"
run = "make test"
needs = ["build"]
[[steps]]
name = "release"
script = "r.sh"
needs = ["test"]
"#,
        );
        assert!(validate_flowchart(&steps, "web").is_ok());
    }

    #[test]
    fn on_error_routes_to_recovery_node_with_options() {
        let steps = steps_from(
            r#"
[[steps]]
name = "migrate"
script = "m.sh"
on_error = "fix"
[[steps]]
name = "fix"
recover = true
message = "pick a remedy"
retry = "migrate"
options = [
  { label = "rollback", script = "rb.sh" },
  { label = "unlock", run = "make unlock", default = true },
]
"#,
        );
        assert!(validate_flowchart(&steps, "web").is_ok());
    }

    #[test]
    fn rejects_duplicate_names() {
        let steps = steps_from(
            r#"
[[steps]]
name = "a"
script = "a.sh"
[[steps]]
name = "a"
script = "a2.sh"
"#,
        );
        let err = validate_flowchart(&steps, "web").unwrap_err().to_string();
        assert!(err.contains("duplicate step name"));
    }

    #[test]
    fn rejects_missing_edge_targets() {
        let needs = steps_from(
            r#"
[[steps]]
name = "a"
script = "a.sh"
needs = ["ghost"]
"#,
        );
        assert!(
            validate_flowchart(&needs, "web")
                .unwrap_err()
                .to_string()
                .contains("ghost")
        );

        let on_err = steps_from(
            r#"
[[steps]]
name = "a"
script = "a.sh"
on_error = "ghost"
"#,
        );
        assert!(validate_flowchart(&on_err, "web").is_err());
    }

    #[test]
    fn rejects_on_error_to_non_recovery_node() {
        let steps = steps_from(
            r#"
[[steps]]
name = "a"
script = "a.sh"
on_error = "b"
[[steps]]
name = "b"
script = "b.sh"
"#,
        );
        let err = validate_flowchart(&steps, "web").unwrap_err().to_string();
        assert!(err.contains("not a recovery node"));
    }

    #[test]
    fn rejects_recovery_node_without_options() {
        let steps = steps_from(
            r#"
[[steps]]
name = "fix"
recover = true
"#,
        );
        assert!(
            validate_flowchart(&steps, "web")
                .unwrap_err()
                .to_string()
                .contains("no options")
        );
    }

    #[test]
    fn rejects_step_without_action() {
        let steps = steps_from(
            r#"
[[steps]]
name = "a"
"#,
        );
        assert!(
            validate_flowchart(&steps, "web")
                .unwrap_err()
                .to_string()
                .contains("no `script`, `run`, or `recipe`")
        );
    }

    #[test]
    fn resolve_orders_steps_by_dependencies() {
        // Declared out of dependency order, with a recovery node listed last.
        let run = RunRecipe {
            steps: steps_from(
                r#"
[[steps]]
name = "release"
script = "r.sh"
needs = ["migrate"]
[[steps]]
name = "migrate"
script = "m.sh"
needs = ["build"]
on_error = "fix"
[[steps]]
name = "build"
script = "b.sh"
[[steps]]
name = "fix"
recover = true
retry = "migrate"
options = [ { label = "unlock", run = "make unlock", default = true } ]
"#,
            ),
            ..Default::default()
        };
        let resolved = resolve_run(&run, "web", Path::new("/proj")).unwrap();
        let order: Vec<&str> = resolved.steps.iter().map(|s| s.name.as_str()).collect();
        // build → migrate (with its recovery node folded in right after) → release.
        assert_eq!(order, vec!["build", "migrate", "fix", "release"]);
    }

    #[test]
    fn resolve_inline_steps_ok() {
        let run = RunRecipe {
            steps: steps_from(
                r#"
[[steps]]
name = "a"
script = "a.sh"
[[steps]]
name = "b"
run = "echo b"
needs = ["a"]
"#,
            ),
            ..Default::default()
        };
        let resolved = resolve_run(&run, "web", Path::new("/proj")).unwrap();
        assert_eq!(resolved.steps.len(), 2);
        assert_eq!(resolved.step("b").unwrap().needs, vec!["a".to_string()]);
    }

    #[test]
    fn resolve_from_flowchart_file_by_entry_and_default() {
        let dir = std::env::temp_dir().join(format!("ciab_flow_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("runs.toml");
        std::fs::write(
            &file,
            r#"
[web]
  [[web.steps]]
  name = "build"
  script = "b.sh"

[api]
  [[api.steps]]
  name = "ship"
  run = "make ship"
"#,
        )
        .unwrap();

        // Entry defaults to the recipe name.
        let run = RunRecipe {
            flowchart: Some("runs.toml".to_string()),
            ..Default::default()
        };
        let web = resolve_run(&run, "web", &dir).unwrap();
        assert_eq!(web.step("build").unwrap().script.as_deref(), Some("b.sh"));

        // An explicit `entry` overrides the recipe name.
        let run = RunRecipe {
            flowchart: Some("runs.toml".to_string()),
            entry: Some("api".to_string()),
            ..Default::default()
        };
        let api = resolve_run(&run, "web", &dir).unwrap();
        assert!(api.step("ship").is_some());

        // A missing entry is a clear error.
        let run = RunRecipe {
            flowchart: Some("runs.toml".to_string()),
            entry: Some("ghost".to_string()),
            ..Default::default()
        };
        let err = resolve_run(&run, "web", &dir).unwrap_err().to_string();
        assert!(err.contains("no entry 'ghost'"));
        assert!(err.contains("api") && err.contains("web"));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn resolve_merges_required_env_from_file_and_recipe() {
        let dir = std::env::temp_dir().join(format!("ciab_flow_env_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("runs.toml"),
            r#"
[web]
REQUIRED_ENV = ["API_TOKEN", "REGION"]
  [[web.steps]]
  name = "build"
  script = "b.sh"
"#,
        )
        .unwrap();

        // Flowchart file's REQUIRED_ENV plus a recipe-level one, de-duped.
        let run = RunRecipe {
            flowchart: Some("runs.toml".to_string()),
            required_env: vec!["REGION".to_string(), "STAGE".to_string()],
            ..Default::default()
        };
        let resolved = resolve_run(&run, "web", &dir).unwrap();
        assert_eq!(
            resolved.required_env,
            vec![
                "API_TOKEN".to_string(),
                "REGION".to_string(),
                "STAGE".to_string()
            ]
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn resolve_required_env_for_inline_steps() {
        let run = RunRecipe {
            required_env: vec!["RUN_KEY".to_string()],
            steps: steps_from("[[steps]]\nname=\"a\"\nscript=\"a.sh\"\n"),
            ..Default::default()
        };
        let resolved = resolve_run(&run, "web", Path::new("/proj")).unwrap();
        assert_eq!(resolved.required_env, vec!["RUN_KEY".to_string()]);
    }

    #[test]
    fn resolve_rejects_both_file_and_inline_steps() {
        let dir = std::env::temp_dir().join(format!("ciab_flow_both_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("d.toml"),
            "[web]\n[[web.steps]]\nname=\"a\"\nscript=\"a.sh\"\n",
        )
        .unwrap();
        let run = RunRecipe {
            flowchart: Some("d.toml".to_string()),
            steps: steps_from("[[steps]]\nname=\"x\"\nrun=\"true\"\n"),
            ..Default::default()
        };
        let err = resolve_run(&run, "web", &dir).unwrap_err().to_string();
        assert!(err.contains("both") && err.contains("inline"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn parse_env_content_handles_comments_quotes_and_export() {
        let pairs = parse_env_content(
            "# a comment\n\
             \n\
             export API_TOKEN=abc123\n\
             REGION = \"us-east-1\"\n\
             QUOTED='single val'\n\
             EMPTY=\n\
             noequals\n\
             =novalue\n",
        );
        assert_eq!(
            pairs,
            vec![
                ("API_TOKEN".to_string(), "abc123".to_string()),
                ("REGION".to_string(), "us-east-1".to_string()),
                ("QUOTED".to_string(), "single val".to_string()),
                ("EMPTY".to_string(), "".to_string()),
            ]
        );
    }

    #[test]
    fn load_env_files_layers_under_existing_and_across_files() {
        let dir = std::env::temp_dir().join(format!("ciab_envfile_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.env"), "A=1\nB=from_a\nC=from_a\n").unwrap();
        std::fs::write(dir.join("b.env"), "B=from_b\n").unwrap();

        // A is already resolved (and non-empty) so it must not be clobbered; B is
        // overridden by the later file; C comes from the first file.
        let base: HashMap<String, String> = [("A".to_string(), "existing".to_string())]
            .into_iter()
            .collect();
        let merged =
            load_env_files(&["a.env".to_string(), "b.env".to_string()], &dir, &base).unwrap();
        assert_eq!(merged.get("A").unwrap(), "existing");
        assert_eq!(merged.get("B").unwrap(), "from_b");
        assert_eq!(merged.get("C").unwrap(), "from_a");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn load_env_files_errors_on_missing_file() {
        let err = load_env_files(
            &["nope.env".to_string()],
            Path::new("/proj"),
            &HashMap::new(),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("nope.env"));
    }

    #[test]
    fn resolve_collects_env_files_from_file_and_recipe() {
        let dir = std::env::temp_dir().join(format!("ciab_flow_envf_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("runs.toml"),
            r#"
[web]
env_file = ".env.flow"
  [[web.steps]]
  name = "build"
  script = "b.sh"
"#,
        )
        .unwrap();

        // Flowchart-entry file first, then the recipe's own (a list), de-duped.
        let run = RunRecipe {
            flowchart: Some("runs.toml".to_string()),
            env_file: vec![".env.flow".to_string(), ".env.run".to_string()],
            ..Default::default()
        };
        let resolved = resolve_run(&run, "web", &dir).unwrap();
        assert_eq!(
            resolved.env_files,
            vec![".env.flow".to_string(), ".env.run".to_string()]
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn env_file_path_supports_var_substitution() {
        // The engine substitutes `{VAR}` in each path before loading; here we
        // exercise the same substitution + load pipeline directly.
        let dir = std::env::temp_dir().join(format!("ciab_envf_sel_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(".env.dev"), "TARGET=dev-host\n").unwrap();
        std::fs::write(dir.join(".env.prod"), "TARGET=prod-host\n").unwrap();

        let env: HashMap<String, String> = [("RUN_ENV".to_string(), "prod".to_string())]
            .into_iter()
            .collect();
        let path = crate::config::substitute_vars(".env.{RUN_ENV}", &env).unwrap();
        assert_eq!(path, ".env.prod");
        let merged = load_env_files(&[path], &dir, &env).unwrap();
        assert_eq!(merged.get("TARGET").unwrap(), "prod-host");

        std::fs::remove_dir_all(&dir).ok();
    }

    fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn missing_required_env_flags_unset_and_empty_only() {
        let required = vec![
            "API_TOKEN".to_string(),
            "REGION".to_string(),
            "STAGE".to_string(),
        ];
        // API_TOKEN set, REGION empty, STAGE absent → REGION + STAGE missing.
        let missing =
            missing_required_env(&required, &env(&[("API_TOKEN", "abc"), ("REGION", "  ")]));
        assert_eq!(missing, vec!["REGION".to_string(), "STAGE".to_string()]);
    }

    #[test]
    fn missing_required_env_empty_when_all_set() {
        let required = vec!["A".to_string(), "B".to_string()];
        assert!(missing_required_env(&required, &env(&[("A", "1"), ("B", "2")])).is_empty());
        // No requirements → never missing.
        assert!(missing_required_env(&[], &env(&[])).is_empty());
    }

    #[test]
    fn prepare_env_sources_files_and_reports_what_is_still_missing() {
        let dir = std::env::temp_dir().join(format!("ciab_prep_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(".env.prod"), "API_TOKEN=from-file\n").unwrap();

        let resolved = ResolvedRun {
            required_env: vec!["API_TOKEN".to_string(), "REGION".to_string()],
            env_files: vec![".env.{RUN_ENV}".to_string()],
            ..Default::default()
        };

        // RUN_ENV set → the file resolves and is sourced, satisfying API_TOKEN;
        // only REGION (supplied by nothing) is left to ask for.
        let prepared = prepare_env(&resolved, &dir, &env(&[("RUN_ENV", "prod")])).unwrap();
        assert_eq!(prepared.sourced, vec![".env.prod".to_string()]);
        assert_eq!(prepared.env.get("API_TOKEN").unwrap(), "from-file");
        assert!(prepared.unresolved_paths.is_empty());
        assert_eq!(prepared.missing(), vec!["REGION".to_string()]);
        assert!(!prepared.is_ready());

        // Everything supplied → ready, and nothing to prompt for.
        let prepared = prepare_env(
            &resolved,
            &dir,
            &env(&[("RUN_ENV", "prod"), ("REGION", "us-east-1")]),
        )
        .unwrap();
        assert!(prepared.is_ready());
        assert!(prepared.missing().is_empty());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn prepare_env_asks_for_env_file_placeholders_first_and_alone() {
        // RUN_ENV is unset, so `.env.{RUN_ENV}` can't even be named. The file it
        // would resolve to may well supply API_TOKEN, so that isn't reported
        // yet — the caller supplies RUN_ENV and checks again.
        let resolved = ResolvedRun {
            required_env: vec!["API_TOKEN".to_string()],
            env_files: vec![".env.{RUN_ENV}".to_string()],
            ..Default::default()
        };
        let prepared = prepare_env(&resolved, Path::new("/proj"), &HashMap::new()).unwrap();
        assert_eq!(prepared.unresolved_paths, vec!["RUN_ENV".to_string()]);
        assert!(prepared.missing_required.is_empty());
        assert_eq!(prepared.missing(), vec!["RUN_ENV".to_string()]);
        assert!(!prepared.is_ready());
        // Nothing was read, so a missing file isn't an error at this point.
        assert!(prepared.sourced.is_empty());
    }

    #[test]
    fn prepare_env_is_ready_when_nothing_is_declared() {
        let prepared =
            prepare_env(&ResolvedRun::default(), Path::new("/proj"), &HashMap::new()).unwrap();
        assert!(prepared.is_ready());
        assert!(prepared.missing().is_empty());
    }

    #[test]
    fn unresolved_vars_lists_only_what_is_unset() {
        let vars = env(&[("REGION", "us-east-1")]);
        assert_eq!(
            crate::config::unresolved_vars("{REGION}/{STAGE}/{STAGE}", &vars),
            vec!["STAGE".to_string()]
        );
        assert!(crate::config::unresolved_vars("{REGION}", &vars).is_empty());
    }

    #[test]
    fn eval_condition_covers_comparison_and_truthy_forms() {
        let env: HashMap<String, String> = [
            ("IN_CI", "true"),
            ("RUN_ENV", "prod"),
            ("REGION", "us-east-1"),
            ("FLAG_OFF", "false"),
            ("EMPTY", ""),
        ]
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();

        // Equality / inequality, with and without the `env.` prefix and quotes.
        assert!(eval_condition("env.IN_CI == true", &env).unwrap());
        assert!(eval_condition("IN_CI == \"true\"", &env).unwrap());
        assert!(eval_condition("RUN_ENV != dev", &env).unwrap());
        assert!(!eval_condition("RUN_ENV == dev", &env).unwrap());
        assert!(eval_condition("REGION == us-east-1", &env).unwrap());
        // Unset variable reads as empty.
        assert!(eval_condition("MISSING != something", &env).unwrap());
        assert!(!eval_condition("MISSING == something", &env).unwrap());
        // Bare truthy / negation.
        assert!(eval_condition("IN_CI", &env).unwrap());
        assert!(!eval_condition("FLAG_OFF", &env).unwrap());
        assert!(!eval_condition("EMPTY", &env).unwrap());
        assert!(eval_condition("!FLAG_OFF", &env).unwrap());
        assert!(!eval_condition("!IN_CI", &env).unwrap());
    }

    #[test]
    fn step_skip_reason_applies_when_and_skip_if() {
        let ci: HashMap<String, String> = [("IN_CI".to_string(), "true".to_string())]
            .into_iter()
            .collect();
        let local: HashMap<String, String> = HashMap::new();

        // skip_if fires only when its condition holds.
        let step = RunStep {
            name: "notify".into(),
            run: Some("true".into()),
            skip_if: vec!["env.IN_CI == true".into()],
            ..Default::default()
        };
        assert!(step_skip_reason(&step, &ci).unwrap().is_some());
        assert!(step_skip_reason(&step, &local).unwrap().is_none());

        // when requires ALL conditions; a single false one skips.
        let step = RunStep {
            name: "release".into(),
            run: Some("true".into()),
            when: vec!["IN_CI == true".into(), "MISSING == yes".into()],
            ..Default::default()
        };
        let reason = step_skip_reason(&step, &ci).unwrap().unwrap();
        assert!(reason.contains("MISSING"));

        // All when conditions met and no skip_if → runs.
        let step = RunStep {
            name: "release".into(),
            run: Some("true".into()),
            when: vec!["IN_CI == true".into()],
            ..Default::default()
        };
        assert!(step_skip_reason(&step, &ci).unwrap().is_none());
    }

    #[test]
    fn when_skip_if_accept_string_or_list_and_reject_blank() {
        let step: RunStep = toml::from_str(
            "name = \"a\"\nrun = \"true\"\nwhen = \"IN_CI\"\nskip_if = [\"A == b\", \"C\"]\n",
        )
        .unwrap();
        assert_eq!(step.when, vec!["IN_CI".to_string()]);
        assert_eq!(step.skip_if, vec!["A == b".to_string(), "C".to_string()]);

        let steps = steps_from("[[steps]]\nname=\"a\"\nrun=\"true\"\nwhen=\"  \"\n");
        assert!(
            validate_flowchart(&steps, "web")
                .unwrap_err()
                .to_string()
                .contains("empty `when`")
        );
    }

    #[test]
    fn env_file_accepts_string_or_list() {
        let one: RunRecipe = toml::from_str("env_file = \".env\"\n").unwrap();
        assert_eq!(one.env_file, vec![".env".to_string()]);
        let many: RunRecipe = toml::from_str("env_file = [\".env\", \".env.run\"]\n").unwrap();
        assert_eq!(
            many.env_file,
            vec![".env".to_string(), ".env.run".to_string()]
        );
    }

    #[test]
    fn parse_duration_accepts_the_forms_people_write() {
        use std::time::Duration;
        // Bare numbers are seconds, so `timeout = 90` works as well as "90s".
        assert_eq!(parse_duration("90").unwrap(), Duration::from_secs(90));
        assert_eq!(parse_duration("30s").unwrap(), Duration::from_secs(30));
        assert_eq!(parse_duration("10m").unwrap(), Duration::from_secs(600));
        assert_eq!(parse_duration("2h").unwrap(), Duration::from_secs(7200));
        assert_eq!(parse_duration("500ms").unwrap(), Duration::from_millis(500));
        // Compound and spelled-out forms.
        assert_eq!(parse_duration("1h30m").unwrap(), Duration::from_secs(5400));
        assert_eq!(parse_duration("1h 30m").unwrap(), Duration::from_secs(5400));
        assert_eq!(
            parse_duration("2 minutes").unwrap(),
            Duration::from_secs(120)
        );
        // Case doesn't matter.
        assert_eq!(parse_duration("10M").unwrap(), Duration::from_secs(600));

        // Nonsense is rejected rather than silently read as "no limit".
        assert!(parse_duration("").is_err());
        assert!(parse_duration("soon").is_err());
        assert!(parse_duration("10 parsecs").is_err());
        assert!(parse_duration("-5").is_err());
    }

    #[test]
    fn an_unparseable_timeout_is_caught_by_validation() {
        let steps = steps_from("[[steps]]\nname=\"a\"\nrun=\"true\"\ntimeout=\"whenever\"\n");
        let err = validate_flowchart(&steps, "web").unwrap_err().to_string();
        assert!(err.contains("invalid timeout"), "{err}");
    }

    #[test]
    fn a_push_step_gets_its_action_from_the_recipe_it_names() {
        let step: RunStep =
            toml::from_str("name = \"publish\"\nkind = \"push\"\nrecipe = \"binary\"\n").unwrap();
        assert!(step.is_push());
        // No script or run of its own, yet it has something to do.
        assert!(step.has_action());
        let (script, run) = step.action();
        assert!(script.is_none());
        assert!(run.unwrap().contains("push binary --no-tui"));

        // A push step that spells out its own command keeps it.
        let explicit: RunStep =
            toml::from_str("name = \"publish\"\nkind = \"push\"\nrun = \"make release\"\n")
                .unwrap();
        assert!(explicit.is_push());
        assert_eq!(explicit.action().1.as_deref(), Some("make release"));

        // An ordinary step is not a push step.
        let plain: RunStep = toml::from_str("name = \"build\"\nrun = \"true\"\n").unwrap();
        assert!(!plain.is_push());
    }

    #[test]
    fn steps_parse_the_full_monorepo_vocabulary() {
        let steps = steps_from(
            r#"
[[steps]]
name             = "compile"
description      = "Build the release binary"
owner            = "Ada"
run              = "cargo build --release"
requires         = ["cargo", "protoc"]
timeout          = "10m"
retries          = 2
continue_on_error = true
kind             = "build"
cwd              = "packages/api"
[steps.env]
PROFILE = "release"

[[steps]]
name       = "dev-server"
run        = "npm run dev"
persistent = true
"#,
        );
        assert!(validate_flowchart(&steps, "web").is_ok());

        let compile = &steps[0];
        assert_eq!(
            compile.description.as_deref(),
            Some("Build the release binary")
        );
        assert_eq!(compile.owner.as_deref(), Some("Ada"));
        assert_eq!(compile.requires, vec!["cargo", "protoc"]);
        assert_eq!(
            compile.timeout_duration().unwrap(),
            Some(std::time::Duration::from_secs(600))
        );
        assert_eq!(compile.retries, 2);
        assert!(compile.continue_on_error);
        assert_eq!(compile.kind.as_deref(), Some("build"));
        assert_eq!(compile.cwd.as_deref(), Some("packages/api"));
        assert_eq!(compile.env.get("PROFILE").unwrap(), "release");
        assert!(!compile.persistent);

        assert!(steps[1].persistent);
    }

    #[test]
    fn detects_dependency_cycle() {
        let steps = steps_from(
            r#"
[[steps]]
name = "a"
script = "a.sh"
needs = ["c"]
[[steps]]
name = "b"
script = "b.sh"
needs = ["a"]
[[steps]]
name = "c"
script = "c.sh"
needs = ["b"]
"#,
        );
        assert!(
            validate_flowchart(&steps, "web")
                .unwrap_err()
                .to_string()
                .contains("cycle")
        );
    }
}
