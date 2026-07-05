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

    /// Steps written inline, when not using a separate `flowchart` file.
    #[serde(default)]
    pub steps: Vec<DeployStep>,
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
    pub steps: Vec<DeployStep>,
}

impl ResolvedDeploy {
    /// Look up a step node by name.
    pub fn step(&self, name: &str) -> Option<&DeployStep> {
        self.steps.iter().find(|s| s.name == name)
    }
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
                        names.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")
                    }
                )
            })?;
            if !deploy.steps.is_empty() {
                bail!(
                    "Recipe '{}' deploy defines both a `flowchart` file and inline `steps`; use one or the other.",
                    recipe_name
                );
            }
            chart.steps.clone()
        }
        None => deploy.steps.clone(),
    };

    let resolved = ResolvedDeploy {
        login: deploy.login.clone(),
        pre: deploy.pre.clone(),
        post: deploy.post.clone(),
        steps,
    };
    validate_flowchart(&resolved.steps, recipe_name)?;
    Ok(resolved)
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
            bail!("Deploy recipe '{}' has a step with an empty name.", recipe_name);
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
        assert!(validate_flowchart(&needs, "web").unwrap_err().to_string().contains("ghost"));

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
        assert!(validate_flowchart(&steps, "web").unwrap_err().to_string().contains("no options"));
    }

    #[test]
    fn rejects_step_without_action() {
        let steps = steps_from(
            r#"
[[steps]]
name = "a"
"#,
        );
        assert!(validate_flowchart(&steps, "web").unwrap_err().to_string().contains("no `script` or `run`"));
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
        let err = resolve_deploy(&deploy, "web", &dir).unwrap_err().to_string();
        assert!(err.contains("no entry 'ghost'"));
        assert!(err.contains("api") && err.contains("web"));

        std::fs::remove_dir_all(&dir).ok();
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
        let err = resolve_deploy(&deploy, "web", &dir).unwrap_err().to_string();
        assert!(err.contains("both") && err.contains("inline"));
        std::fs::remove_dir_all(&dir).ok();
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
        assert!(validate_flowchart(&steps, "web").unwrap_err().to_string().contains("cycle"));
    }
}
