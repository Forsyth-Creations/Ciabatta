//! The `deploy` paradigm: a recipe direction whose main stage runs a DAG of
//! dependent script "steps", with `on_error` branches to recovery nodes that
//! offer a choice of fix scripts.
//!
//! This module owns the deploy config types (referenced from [`crate::config`]),
//! the loader that resolves a recipe's step DAG from a separate flowchart file,
//! the validation of that DAG, and the async engine that executes it. The live
//! web view (`--gui`) and the visual builder (`--build`) live in
//! [`server`](crate::deploy::server).

pub mod engine;
pub mod server;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::Path;

pub use engine::run_deploy;

/// The `[recipies.<name>.deploy]` sub-table: the deploy direction of a recipe.
///
/// The step DAG itself normally lives in a separate flowchart file (via the
/// `flowchart` path and optional `entry`); the `login`/`pre`/`post` phase hooks
/// stay in the main config, mirroring the push/pull stage overrides. Inline
/// `steps` are also accepted, so a small pipeline needs no second file.
#[derive(Debug, Deserialize, Serialize, Clone, Default)]
pub struct DeployRecipe {
    /// Path (relative to the project root) to a flowchart TOML file holding the
    /// step DAG. When set, its `entry` table supplies the steps.
    pub flowchart: Option<String>,
    /// Which top-level entry of the flowchart file to use. Defaults to the
    /// recipe's own name.
    pub entry: Option<String>,

    /// Override the `login` phase (default: no-op for deploys).
    pub login: Option<String>,
    /// Override the `pre-deploy` phase (default: no-op).
    pub pre: Option<String>,
    /// Override the `post-deploy` phase (default: no-op).
    pub post: Option<String>,

    /// Path(s) (relative to the project root) to `.env` file(s) sourced before
    /// the deploy runs. Each `KEY=VALUE` line is loaded into the deploy's
    /// environment so its phases and steps can see it; values already resolved
    /// (from the ambient environment, CI, git, or `-e` flags) take precedence,
    /// and later files override earlier ones. Missing files are an error.
    /// Accepts a single path (`env_file = ".env"`) or a list
    /// (`env_file = [".env", ".env.deploy"]`). Merged with the flowchart file's
    /// own `env_file` when a `flowchart` file is used.
    ///
    /// Each path may contain `{VAR}` placeholders resolved from the current
    /// environment, so which file is sourced can be selected at run time:
    /// `env_file = ".env.{DEPLOY_ENV}"` sources `.env.dev` or `.env.prod`
    /// depending on `DEPLOY_ENV`.
    #[serde(default, deserialize_with = "string_or_vec")]
    pub env_file: Vec<String>,

    /// Environment variables that must be set (and non-empty) for the deploy to
    /// run. Checked before any phase executes; if any are empty/unset the deploy
    /// is aborted with an error. Merged with the flowchart file's own
    /// `REQUIRED_ENV` when a `flowchart` file is used.
    #[serde(default, rename = "REQUIRED_ENV")]
    pub required_env: Vec<String>,

    /// Steps written inline, when not using a separate `flowchart` file.
    #[serde(default)]
    pub steps: Vec<DeployStep>,
}

/// Deserialize a field that TOML may express as either a bare string
/// (`env_file = ".env"`) or an array (`env_file = [".env", ".env.deploy"]`) into
/// a `Vec<String>`. Shared by `env_file` and the step conditions (`when` /
/// `skip_if`), all of which accept one-or-many.
fn string_or_vec<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
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

/// A node in the deploy flowchart: either a normal step (runs an action once its
/// `needs` succeed) or a recovery node (`recover = true`, entered only via some
/// step's `on_error`, offering a choice of fix `options`).
#[derive(Debug, Deserialize, Serialize, Clone, Default)]
pub struct DeployStep {
    /// Unique node name; the target of `needs` / `on_error` / `retry` edges.
    pub name: String,

    /// A bash script to run (path relative to the project root).
    pub script: Option<String>,
    /// An inline shell command (`sh -c`), as an alternative to `script`.
    pub run: Option<String>,

    /// Names of steps that must succeed before this one runs (the success DAG).
    #[serde(default)]
    pub needs: Vec<String>,
    /// On failure, jump to this recovery node instead of aborting the deploy.
    pub on_error: Option<String>,

    /// Condition(s) that must ALL hold for this step to run; if any is false the
    /// step is skipped (and treated as satisfied, so its dependents still run).
    /// Accepts one condition or a list. Each is evaluated against the deploy's
    /// environment, e.g. `when = "env.DEPLOY_ENV == prod"` or
    /// `when = ["env.DEPLOY_ENV == prod", "REGION == us-east-1"]`.
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

impl DeployStep {
    /// Whether this node runs an action of its own (as opposed to a pure
    /// recovery node that only presents options).
    pub fn has_action(&self) -> bool {
        self.script.is_some() || self.run.is_some()
    }
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
    /// flowchart's deploy steps to run. Empty/unset variables abort the deploy
    /// before any phase executes.
    #[serde(default, rename = "REQUIRED_ENV")]
    pub required_env: Vec<String>,
    /// `.env` file path(s) sourced before this flowchart's deploy runs. Merged
    /// with any declared on the recipe's `[deploy]` table.
    #[serde(default, deserialize_with = "string_or_vec")]
    pub env_file: Vec<String>,
    #[serde(default)]
    pub steps: Vec<DeployStep>,
}

/// A fully resolved deploy ready to run: the phase hooks plus the validated
/// step DAG.
#[derive(Debug, Clone, Default)]
pub struct ResolvedDeploy {
    pub login: Option<String>,
    pub pre: Option<String>,
    pub post: Option<String>,
    /// Variables that must be set (non-empty) before the deploy may run.
    pub required_env: Vec<String>,
    /// `.env` file paths (relative to the project root) to source before the
    /// deploy runs, in the order they should be applied.
    pub env_files: Vec<String>,
    pub steps: Vec<DeployStep>,
}

impl ResolvedDeploy {
    /// Look up a step node by name.
    pub fn step(&self, name: &str) -> Option<&DeployStep> {
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
fn parse_env_content(content: &str) -> Vec<(String, String)> {
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
/// on top of `base`, returning the combined environment for the deploy.
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
    !v.is_empty() && !matches!(v.to_ascii_lowercase().as_str(), "false" | "0" | "no" | "off")
}

/// Strip a single pair of matching surrounding quotes from a comparison operand.
fn cond_unquote(s: &str) -> &str {
    let s = s.trim();
    if s.len() >= 2
        && ((s.starts_with('"') && s.ends_with('"'))
            || (s.starts_with('\'') && s.ends_with('\'')))
    {
        &s[1..s.len() - 1]
    } else {
        s
    }
}

/// Evaluate a single step condition against the deploy's environment.
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
pub fn step_skip_reason(
    step: &DeployStep,
    env: &HashMap<String, String>,
) -> Result<Option<String>> {
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

/// Resolve a recipe's deploy definition into runnable steps: load the separate
/// flowchart file when one is referenced, otherwise use the inline steps, then
/// validate the resulting DAG.
///
/// `recipe_name` is used as the default flowchart entry when `entry` is unset.
pub fn resolve_deploy(
    deploy: &DeployRecipe,
    recipe_name: &str,
    root: &Path,
) -> Result<ResolvedDeploy> {
    // Variables required by the flowchart entry (if a file is used); merged with
    // any declared on the recipe's `[deploy]` table below.
    let mut required_env: Vec<String> = Vec::new();
    // `.env` files to source, in application order: flowchart-entry files first,
    // then recipe-level files (so a recipe can layer overrides on top).
    let mut env_files: Vec<String> = Vec::new();

    let steps = match deploy.flowchart.as_deref() {
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
            let entry = deploy.entry.as_deref().unwrap_or(recipe_name);
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
            if !deploy.steps.is_empty() {
                bail!(
                    "Recipe '{}' deploy defines both a `flowchart` file and inline `steps`; use one or the other.",
                    recipe_name
                );
            }
            required_env.extend(chart.required_env.iter().cloned());
            env_files.extend(chart.env_file.iter().cloned());
            chart.steps.clone()
        }
        None => deploy.steps.clone(),
    };

    // Recipe-level `REQUIRED_ENV` applies to both inline and file flowcharts;
    // fold it in, de-duplicating so a var listed in both places is checked once.
    for var in &deploy.required_env {
        if !required_env.contains(var) {
            required_env.push(var.clone());
        }
    }

    // Recipe-level `env_file`(s) are applied after the flowchart's, de-duplicated
    // so the same path listed in both places is sourced once.
    for file in deploy.env_file.iter() {
        if !env_files.contains(file) {
            env_files.push(file.clone());
        }
    }

    let resolved = ResolvedDeploy {
        login: deploy.login.clone(),
        pre: deploy.pre.clone(),
        post: deploy.post.clone(),
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
fn topo_order(steps: &[DeployStep]) -> Vec<DeployStep> {
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
    let mut result: Vec<DeployStep> = Vec::with_capacity(steps.len());
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
pub fn validate_flowchart(steps: &[DeployStep], recipe_name: &str) -> Result<()> {
    if steps.is_empty() {
        bail!("Deploy recipe '{}' has no steps.", recipe_name);
    }

    let mut names: HashSet<&str> = HashSet::new();
    for step in steps {
        if step.name.trim().is_empty() {
            bail!(
                "Deploy recipe '{}' has a step with an empty name.",
                recipe_name
            );
        }
        if !names.insert(step.name.as_str()) {
            bail!(
                "Deploy recipe '{}' has a duplicate step name '{}'.",
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
                    "Recovery node '{}' in deploy '{}' has no options.",
                    step.name,
                    recipe_name
                );
            }
            for opt in &step.options {
                if opt.label.trim().is_empty() {
                    bail!(
                        "Recovery node '{}' in deploy '{}' has an option with no label.",
                        step.name,
                        recipe_name
                    );
                }
            }
        } else if !step.has_action() {
            bail!(
                "Step '{}' in deploy '{}' has no `script` or `run` to execute.",
                step.name,
                recipe_name
            );
        }

        // Conditions must be non-blank so a typo can't silently read as "run".
        for cond in step.when.iter().chain(step.skip_if.iter()) {
            if cond.trim().is_empty() {
                bail!(
                    "Step '{}' in deploy '{}' has an empty `when`/`skip_if` condition.",
                    step.name,
                    recipe_name
                );
            }
        }

        // Edge targets must exist.
        for dep in &step.needs {
            if !names.contains(dep.as_str()) {
                bail!(
                    "Step '{}' in deploy '{}' needs '{}', which is not a defined step.",
                    step.name,
                    recipe_name,
                    dep
                );
            }
        }
        if let Some(target) = step.on_error.as_deref() {
            if !names.contains(target) {
                bail!(
                    "Step '{}' in deploy '{}' has on_error = '{}', which is not a defined step.",
                    step.name,
                    recipe_name,
                    target
                );
            }
            if !is_recover(target) {
                bail!(
                    "Step '{}' in deploy '{}' routes on_error to '{}', which is not a recovery node (set `recover = true`).",
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
                "Recovery node '{}' in deploy '{}' has retry = '{}', which is not a defined step.",
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
fn detect_cycle(steps: &[DeployStep], recipe_name: &str) -> Result<()> {
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
                            "Deploy '{}' has a dependency cycle involving step '{}'.",
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

    fn steps_from(toml_str: &str) -> Vec<DeployStep> {
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
                .contains("no `script` or `run`")
        );
    }

    #[test]
    fn resolve_orders_steps_by_dependencies() {
        // Declared out of dependency order, with a recovery node listed last.
        let deploy = DeployRecipe {
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
        let resolved = resolve_deploy(&deploy, "web", Path::new("/proj")).unwrap();
        let order: Vec<&str> = resolved.steps.iter().map(|s| s.name.as_str()).collect();
        // build → migrate (with its recovery node folded in right after) → release.
        assert_eq!(order, vec!["build", "migrate", "fix", "release"]);
    }

    #[test]
    fn resolve_inline_steps_ok() {
        let deploy = DeployRecipe {
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
        let resolved = resolve_deploy(&deploy, "web", Path::new("/proj")).unwrap();
        assert_eq!(resolved.steps.len(), 2);
        assert_eq!(resolved.step("b").unwrap().needs, vec!["a".to_string()]);
    }

    #[test]
    fn resolve_from_flowchart_file_by_entry_and_default() {
        let dir = std::env::temp_dir().join(format!("ciab_flow_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("deploys.toml");
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
        let deploy = DeployRecipe {
            flowchart: Some("deploys.toml".to_string()),
            ..Default::default()
        };
        let web = resolve_deploy(&deploy, "web", &dir).unwrap();
        assert_eq!(web.step("build").unwrap().script.as_deref(), Some("b.sh"));

        // An explicit `entry` overrides the recipe name.
        let deploy = DeployRecipe {
            flowchart: Some("deploys.toml".to_string()),
            entry: Some("api".to_string()),
            ..Default::default()
        };
        let api = resolve_deploy(&deploy, "web", &dir).unwrap();
        assert!(api.step("ship").is_some());

        // A missing entry is a clear error.
        let deploy = DeployRecipe {
            flowchart: Some("deploys.toml".to_string()),
            entry: Some("ghost".to_string()),
            ..Default::default()
        };
        let err = resolve_deploy(&deploy, "web", &dir)
            .unwrap_err()
            .to_string();
        assert!(err.contains("no entry 'ghost'"));
        assert!(err.contains("api") && err.contains("web"));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn resolve_merges_required_env_from_file_and_recipe() {
        let dir = std::env::temp_dir().join(format!("ciab_flow_env_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("deploys.toml"),
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
        let deploy = DeployRecipe {
            flowchart: Some("deploys.toml".to_string()),
            required_env: vec!["REGION".to_string(), "STAGE".to_string()],
            ..Default::default()
        };
        let resolved = resolve_deploy(&deploy, "web", &dir).unwrap();
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
        let deploy = DeployRecipe {
            required_env: vec!["DEPLOY_KEY".to_string()],
            steps: steps_from("[[steps]]\nname=\"a\"\nscript=\"a.sh\"\n"),
            ..Default::default()
        };
        let resolved = resolve_deploy(&deploy, "web", Path::new("/proj")).unwrap();
        assert_eq!(resolved.required_env, vec!["DEPLOY_KEY".to_string()]);
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
        let deploy = DeployRecipe {
            flowchart: Some("d.toml".to_string()),
            steps: steps_from("[[steps]]\nname=\"x\"\nrun=\"true\"\n"),
            ..Default::default()
        };
        let err = resolve_deploy(&deploy, "web", &dir)
            .unwrap_err()
            .to_string();
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
        std::fs::write(&dir.join("a.env"), "A=1\nB=from_a\nC=from_a\n").unwrap();
        std::fs::write(&dir.join("b.env"), "B=from_b\n").unwrap();

        // A is already resolved (and non-empty) so it must not be clobbered; B is
        // overridden by the later file; C comes from the first file.
        let base: HashMap<String, String> =
            [("A".to_string(), "existing".to_string())].into_iter().collect();
        let merged = load_env_files(
            &["a.env".to_string(), "b.env".to_string()],
            &dir,
            &base,
        )
        .unwrap();
        assert_eq!(merged.get("A").unwrap(), "existing");
        assert_eq!(merged.get("B").unwrap(), "from_b");
        assert_eq!(merged.get("C").unwrap(), "from_a");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn load_env_files_errors_on_missing_file() {
        let err = load_env_files(&["nope.env".to_string()], Path::new("/proj"), &HashMap::new())
            .unwrap_err()
            .to_string();
        assert!(err.contains("nope.env"));
    }

    #[test]
    fn resolve_collects_env_files_from_file_and_recipe() {
        let dir = std::env::temp_dir().join(format!("ciab_flow_envf_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("deploys.toml"),
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
        let deploy = DeployRecipe {
            flowchart: Some("deploys.toml".to_string()),
            env_file: vec![".env.flow".to_string(), ".env.deploy".to_string()],
            ..Default::default()
        };
        let resolved = resolve_deploy(&deploy, "web", &dir).unwrap();
        assert_eq!(
            resolved.env_files,
            vec![".env.flow".to_string(), ".env.deploy".to_string()]
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

        let env: HashMap<String, String> =
            [("DEPLOY_ENV".to_string(), "prod".to_string())].into_iter().collect();
        let path = crate::config::substitute_vars(".env.{DEPLOY_ENV}", &env).unwrap();
        assert_eq!(path, ".env.prod");
        let merged = load_env_files(&[path], &dir, &env).unwrap();
        assert_eq!(merged.get("TARGET").unwrap(), "prod-host");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn eval_condition_covers_comparison_and_truthy_forms() {
        let env: HashMap<String, String> = [
            ("IN_CI", "true"),
            ("DEPLOY_ENV", "prod"),
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
        assert!(eval_condition("DEPLOY_ENV != dev", &env).unwrap());
        assert!(!eval_condition("DEPLOY_ENV == dev", &env).unwrap());
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
        let ci: HashMap<String, String> =
            [("IN_CI".to_string(), "true".to_string())].into_iter().collect();
        let local: HashMap<String, String> = HashMap::new();

        // skip_if fires only when its condition holds.
        let step = DeployStep {
            name: "notify".into(),
            run: Some("true".into()),
            skip_if: vec!["env.IN_CI == true".into()],
            ..Default::default()
        };
        assert!(step_skip_reason(&step, &ci).unwrap().is_some());
        assert!(step_skip_reason(&step, &local).unwrap().is_none());

        // when requires ALL conditions; a single false one skips.
        let step = DeployStep {
            name: "release".into(),
            run: Some("true".into()),
            when: vec!["IN_CI == true".into(), "MISSING == yes".into()],
            ..Default::default()
        };
        let reason = step_skip_reason(&step, &ci).unwrap().unwrap();
        assert!(reason.contains("MISSING"));

        // All when conditions met and no skip_if → runs.
        let step = DeployStep {
            name: "release".into(),
            run: Some("true".into()),
            when: vec!["IN_CI == true".into()],
            ..Default::default()
        };
        assert!(step_skip_reason(&step, &ci).unwrap().is_none());
    }

    #[test]
    fn when_skip_if_accept_string_or_list_and_reject_blank() {
        let step: DeployStep = toml::from_str(
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
        let one: DeployRecipe = toml::from_str("env_file = \".env\"\n").unwrap();
        assert_eq!(one.env_file, vec![".env".to_string()]);
        let many: DeployRecipe =
            toml::from_str("env_file = [\".env\", \".env.deploy\"]\n").unwrap();
        assert_eq!(
            many.env_file,
            vec![".env".to_string(), ".env.deploy".to_string()]
        );
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
