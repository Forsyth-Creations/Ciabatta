//! Planning a whole graph, not just one step.
//!
//! A step's third dependency is the output of the steps it needs, so a graph
//! can only be planned in dependency order: `api:build`'s key isn't knowable
//! until `proto:generate`'s outputs are. This module walks the graph once, in
//! order, carrying each step's output fingerprint forward to its dependents.
//!
//! That ordering is also what makes invalidation propagate correctly. Change a
//! `.proto` file and `proto:generate` misses; its outputs change; every step
//! downstream of it gets a different key and misses too — each for a reason it
//! can name, rather than everything rebuilding because one thing did.
//!
//! What this produces is a *prediction*. `ciabatta dry-run` prints it and stops;
//! the runner uses the same function and then acts on it. The two cannot
//! disagree, which is the only way a dry run is worth reading.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::Result;

use super::store::Store;
use super::{CacheConfig, Decision, Target, diff::Diff};
use crate::run::RunStep;

/// One step, planned: what it would do, and why.
#[derive(Debug, Clone)]
pub struct Planned {
    /// The step's name in the graph.
    pub name: String,
    /// What it needs.
    pub needs: Vec<String>,
    /// The cache target derived from it.
    pub target: Target,
    /// What would happen.
    pub decision: Decision,
    /// The input files, as they are right now.
    pub inputs: Vec<super::FileHash>,
    /// The output files it declares, as they are right now (empty before a
    /// first build).
    pub outputs: Vec<super::FileHash>,
    /// What changed since the last run of this step, when there was one and
    /// something did.
    pub diff: Option<Diff>,
}

impl Planned {
    /// Whether this step's build would be skipped.
    pub fn is_reuse(&self) -> bool {
        self.decision.is_reuse()
    }
}

/// A whole graph's plan.
#[derive(Debug, Clone, Default)]
pub struct Plan {
    pub steps: Vec<Planned>,
}

impl Plan {
    /// How many steps would be reused, and how many would run.
    pub fn tally(&self) -> (usize, usize) {
        let reused = self.steps.iter().filter(|s| s.is_reuse()).count();
        (reused, self.steps.len() - reused)
    }

    /// The build time the reused steps represent, in milliseconds.
    ///
    /// Taken from what those builds actually cost last time, so it's a measured
    /// number rather than an estimate.
    pub fn saved_ms(&self, store: &Store) -> u64 {
        self.steps
            .iter()
            .filter(|s| s.is_reuse())
            .filter_map(|s| s.decision.key())
            .filter_map(|key| store.get(key).ok().flatten())
            .map(|entry| entry.duration_ms)
            .sum()
    }

    /// Whether any step is cached at all.
    pub fn has_caching(&self) -> bool {
        self.steps
            .iter()
            .any(|s| !matches!(s.decision, Decision::Uncached { .. }))
    }
}

/// How to find the cache settings and working directory for a step.
///
/// A closure rather than a config lookup, because the answer depends on which
/// sub-workspace the step came from — and that's knowledge the workspace layer
/// has and this module shouldn't need to duplicate.
pub trait StepContext {
    /// The cache settings in force for a step.
    fn cache_config(&self, step: &RunStep) -> CacheConfig;
    /// The directory its `inputs`/`outputs` are relative to.
    fn dir(&self, step: &RunStep) -> PathBuf;
    /// The workspace it belongs to.
    fn workspace(&self, step: &RunStep) -> String;
}

/// Plan every step in `steps`, in the order given.
///
/// `steps` must already be in dependency order — [`crate::run::resolve_run`] and
/// the workflow compiler both topologically sort, so callers get this for free.
pub fn plan_graph(
    steps: &[RunStep],
    context: &dyn StepContext,
    env: &BTreeMap<String, String>,
    store: &Store,
) -> Result<Plan> {
    // Step name → the fingerprint of what it produced, carried forward to its
    // dependents as their third dependency.
    let mut fingerprints: BTreeMap<String, String> = BTreeMap::new();
    let mut planned: Vec<Planned> = Vec::new();

    for step in steps {
        // A recovery node isn't part of the success graph and has no build to
        // cache — it exists to be jumped to when something else fails.
        if step.recover {
            continue;
        }

        let config = context.cache_config(step);
        let dir = context.dir(step);
        let workspace = context.workspace(step);

        // Only the steps this one actually needs, so an unrelated change
        // upstream in a different branch of the graph doesn't invalidate it.
        let upstream: BTreeMap<String, String> = step
            .needs
            .iter()
            .filter_map(|need| {
                fingerprints
                    .get(need)
                    .map(|hash| (need.clone(), hash.clone()))
            })
            .collect();

        let target = Target {
            name: step.name.clone(),
            workspace: workspace.clone(),
            dir: dir.clone(),
            commands: commands_of(step),
            config: config.clone(),
            upstream: upstream.clone(),
        };

        let decision = super::plan(&target, env, store)?;
        let inputs = config.hash_inputs(&dir)?;
        let outputs = config.hash_outputs(&dir)?;

        // Downstream steps depend on what this one produced. On a hit that's
        // the stored output set; otherwise it's whatever is on disk now, which
        // is the honest prediction for a step that hasn't run yet.
        let fingerprint = match decision.key().and_then(|k| store.get(k).ok().flatten()) {
            Some(entry) if decision.is_reuse() => super::fingerprint(&entry.outputs),
            _ => super::fingerprint(&outputs),
        };
        fingerprints.insert(step.name.clone(), fingerprint);

        // Only explain a miss — on a hit there is, by construction, nothing to
        // show, and printing an empty diff is noise.
        let diff = if matches!(decision, Decision::Rebuild { .. }) {
            let declared = declared_env(&config, env);
            store
                .explain(&workspace, &step.name, &dir, &inputs, &declared, &upstream)?
                .filter(|d| !d.is_empty())
        } else {
            None
        };

        planned.push(Planned {
            name: step.name.clone(),
            needs: step.needs.clone(),
            target,
            decision,
            inputs,
            outputs,
            diff,
        });
    }

    Ok(Plan { steps: planned })
}

/// The declared environment variables and their current values.
pub fn declared_env(
    config: &CacheConfig,
    env: &BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    config
        .env
        .iter()
        .map(|name| (name.clone(), env.get(name).cloned().unwrap_or_default()))
        .collect()
}

/// The commands a step runs, as they go into its key.
///
/// A step that runs a *script* keys on the script's path here and on the
/// script's contents through `inputs` — which is why `cache init` scaffolds
/// `scripts/**` into the inputs. Keying on the path alone would let an edited
/// build script serve a stale artifact.
pub fn commands_of(step: &RunStep) -> Vec<String> {
    let mut commands = Vec::new();
    if let Some(run) = &step.run {
        commands.push(run.clone());
    }
    if let Some(script) = &step.script {
        commands.push(format!("script:{script}"));
    }
    if let Some(recipe) = &step.recipe {
        commands.push(format!("recipe:{recipe}"));
    }
    commands
}

/// Merge cache settings from the three levels that can declare them —
/// workspace, recipe, target — with the most specific level winning each field
/// it actually mentions.
///
/// A monorepo wants `cache.inputs` written once per workspace, not once per
/// target; but a target that reads a file none of its neighbours do, or that
/// switches on a variable of its own, has to be able to say so without
/// restating everything else. So each level is layered over the one above it
/// field by field: a list the target declares replaces the inherited one whole
/// (half-merged input globs would be very hard to reason about), and a list it
/// leaves out is inherited unchanged.
///
/// `enabled` is the field that makes this safe rather than clever, and it is
/// why it is an `Option`. A target writing
///
/// ```yaml
/// cache:
///   env: [PROFILE]
/// ```
///
/// means "I also depend on PROFILE" — it must not silently turn off the
/// caching its workspace switched on, and it must not silently turn caching
/// *on* for a workspace that never asked. Only an explicit `enabled:` at some
/// level decides, and the most specific explicit one wins, so a single target
/// can still opt out with `enabled: false`.
pub fn effective(
    workspace: Option<&CacheConfig>,
    recipe: Option<&CacheConfig>,
    step: Option<&CacheConfig>,
) -> CacheConfig {
    let mut merged = workspace.cloned().unwrap_or_default();
    for over in [recipe, step].into_iter().flatten() {
        layer(&mut merged, over);
    }
    merged
}

/// Apply one level's declarations over what it inherited.
fn layer(base: &mut CacheConfig, over: &CacheConfig) {
    if let Some(enabled) = over.enabled {
        base.enabled = Some(enabled);
    }
    if !over.inputs.is_empty() {
        base.inputs = over.inputs.clone();
    }
    if !over.outputs.is_empty() {
        base.outputs = over.outputs.clone();
    }
    if !over.env.is_empty() {
        base.env = over.env.clone();
    }
    if !over.exclude.is_empty() {
        base.exclude = over.exclude.clone();
    }
    // The remote is a property of the project, not of a target within it (see
    // `Session::warn_about_ignored_remotes`), so a level only ever adds one
    // where none was inherited.
    if over.remote.is_some() {
        base.remote = over.remote.clone();
    }
}

/// Where the store for a project lives.
pub fn store_for(root: &Path) -> Result<Store> {
    Store::for_project(root)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixed {
        dir: PathBuf,
        config: CacheConfig,
    }

    impl StepContext for Fixed {
        fn cache_config(&self, _step: &RunStep) -> CacheConfig {
            self.config.clone()
        }
        fn dir(&self, _step: &RunStep) -> PathBuf {
            self.dir.clone()
        }
        fn workspace(&self, _step: &RunStep) -> String {
            "api".to_string()
        }
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ciab_graph_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write(dir: &Path, rel: &str, body: &str) {
        let path = dir.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }

    fn step(name: &str, run: &str, needs: &[&str]) -> RunStep {
        RunStep {
            name: name.to_string(),
            run: Some(run.to_string()),
            needs: needs.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn a_graph_plans_in_order_and_carries_fingerprints_forward() {
        let dir = scratch("order");
        write(&dir, "src/a.rs", "fn a() {}");
        write(&dir, "dist/out", "built");
        let store = Store::at(dir.join(".cache")).unwrap();

        let context = Fixed {
            dir: dir.clone(),
            config: CacheConfig {
                enabled: Some(true),
                inputs: vec!["src/**/*".into()],
                outputs: vec!["dist/**/*".into()],
                ..Default::default()
            },
        };
        let steps = vec![
            step("generate", "make gen", &[]),
            step("build", "make build", &["generate"]),
        ];

        let plan = plan_graph(&steps, &context, &BTreeMap::new(), &store).unwrap();
        assert_eq!(plan.steps.len(), 2);

        // The downstream step's key includes the upstream step's fingerprint.
        assert!(plan.steps[0].target.upstream.is_empty());
        assert_eq!(
            plan.steps[1].target.upstream.keys().collect::<Vec<_>>(),
            vec!["generate"]
        );
        assert!(!plan.steps[1].target.upstream["generate"].is_empty());

        // Nothing is built yet, so both would run.
        let (reused, rebuilt) = plan.tally();
        assert_eq!((reused, rebuilt), (0, 2));
        assert!(plan.has_caching());

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The point of the third dependency: a step whose own files didn't change
    /// still rebuilds when what it consumes did.
    #[test]
    fn a_changed_upstream_output_changes_the_downstream_key() {
        let dir = scratch("propagate");
        write(&dir, "src/a.rs", "fn a() {}");
        write(&dir, "dist/out", "v1");
        let store = Store::at(dir.join(".cache")).unwrap();

        let context = Fixed {
            dir: dir.clone(),
            config: CacheConfig {
                enabled: Some(true),
                inputs: vec!["src/**/*".into()],
                outputs: vec!["dist/**/*".into()],
                ..Default::default()
            },
        };
        let steps = vec![
            step("generate", "make gen", &[]),
            step("build", "make build", &["generate"]),
        ];

        let before = plan_graph(&steps, &context, &BTreeMap::new(), &store).unwrap();
        let downstream_key = before.steps[1].decision.key().unwrap().to_string();

        // Nothing under `src/` moved — only what the upstream step produced.
        write(&dir, "dist/out", "v2");
        let after = plan_graph(&steps, &context, &BTreeMap::new(), &store).unwrap();

        assert_ne!(
            after.steps[1].decision.key().unwrap(),
            downstream_key,
            "a changed upstream output must invalidate its dependents"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A step only depends on what it declares it needs — a change in an
    /// unrelated branch must not invalidate it.
    #[test]
    fn an_unrelated_branch_does_not_invalidate_a_step() {
        let dir = scratch("unrelated");
        write(&dir, "src/a.rs", "fn a() {}");
        write(&dir, "dist/out", "v1");
        let store = Store::at(dir.join(".cache")).unwrap();

        let context = Fixed {
            dir: dir.clone(),
            config: CacheConfig {
                enabled: Some(true),
                inputs: vec!["src/**/*".into()],
                outputs: vec!["dist/**/*".into()],
                ..Default::default()
            },
        };
        // `lint` needs nothing; `build` needs `generate`.
        let steps = vec![
            step("generate", "make gen", &[]),
            step("lint", "make lint", &[]),
            step("build", "make build", &["generate"]),
        ];

        let plan = plan_graph(&steps, &context, &BTreeMap::new(), &store).unwrap();
        let build = plan.steps.iter().find(|s| s.name == "build").unwrap();
        assert_eq!(
            build.target.upstream.keys().collect::<Vec<_>>(),
            vec!["generate"],
            "build must not depend on lint, which it never declared"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn recovery_nodes_are_not_cacheable_units() {
        let dir = scratch("recover");
        let store = Store::at(dir.join(".cache")).unwrap();
        let context = Fixed {
            dir: dir.clone(),
            config: CacheConfig::default(),
        };

        let steps = vec![
            step("build", "make", &[]),
            RunStep {
                name: "fix-build".into(),
                recover: true,
                ..Default::default()
            },
        ];

        let plan = plan_graph(&steps, &context, &BTreeMap::new(), &store).unwrap();
        assert_eq!(plan.steps.len(), 1);
        assert_eq!(plan.steps[0].name, "build");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_uncached_workspace_plans_but_reuses_nothing() {
        let dir = scratch("uncached");
        let store = Store::at(dir.join(".cache")).unwrap();
        let context = Fixed {
            dir: dir.clone(),
            config: CacheConfig::default(),
        };

        let plan = plan_graph(
            &[step("build", "make", &[])],
            &context,
            &BTreeMap::new(),
            &store,
        )
        .unwrap();
        assert!(!plan.has_caching());
        assert_eq!(plan.tally(), (0, 1));
        assert!(matches!(plan.steps[0].decision, Decision::Uncached { .. }));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_target_declares_only_what_differs_and_inherits_the_rest() {
        let workspace = CacheConfig {
            enabled: Some(true),
            inputs: vec!["src/**/*".into()],
            outputs: vec!["dist/**/*".into()],
            exclude: vec!["dist".into()],
            ..Default::default()
        };

        assert_eq!(effective(Some(&workspace), None, None), workspace);
        assert_eq!(effective(None, None, None), CacheConfig::default());

        // The whole point of per-target dependencies: naming one extra variable
        // must not cost the target its inputs, its outputs, or its caching.
        let declares_env = CacheConfig {
            env: vec!["PROFILE".into()],
            ..Default::default()
        };
        let merged = effective(Some(&workspace), None, Some(&declares_env));
        assert_eq!(merged.env, vec!["PROFILE".to_string()]);
        assert_eq!(merged.inputs, workspace.inputs);
        assert_eq!(merged.outputs, workspace.outputs);
        assert_eq!(merged.exclude, workspace.exclude);
        assert!(
            merged.is_on(),
            "declaring a dependency must not turn caching off"
        );

        // A list it does declare replaces the inherited one whole.
        let declares_inputs = CacheConfig {
            inputs: vec!["proto/**/*".into()],
            ..Default::default()
        };
        let merged = effective(Some(&workspace), None, Some(&declares_inputs));
        assert_eq!(merged.inputs, vec!["proto/**/*".to_string()]);
        assert_eq!(merged.outputs, workspace.outputs);

        // And an explicit `enabled: false` on the target still wins.
        let opts_out = CacheConfig {
            enabled: Some(false),
            ..Default::default()
        };
        assert!(!effective(Some(&workspace), None, Some(&opts_out)).is_on());

        // A target can't switch caching on for a workspace that never asked.
        let no_workspace_opinion = CacheConfig {
            inputs: vec!["proto/**/*".into()],
            ..Default::default()
        };
        assert!(!effective(None, None, Some(&no_workspace_opinion)).is_on());
    }

    #[test]
    fn a_steps_command_is_part_of_what_it_keys_on() {
        assert_eq!(
            commands_of(&step("build", "cargo build", &[])),
            vec!["cargo build".to_string()]
        );

        let scripted = RunStep {
            name: "build".into(),
            script: Some("scripts/build.sh".into()),
            ..Default::default()
        };
        assert_eq!(
            commands_of(&scripted),
            vec!["script:scripts/build.sh".to_string()]
        );

        let publish = RunStep {
            name: "publish".into(),
            recipe: Some("binary".into()),
            ..Default::default()
        };
        assert_eq!(commands_of(&publish), vec!["recipe:binary".to_string()]);
    }
}
