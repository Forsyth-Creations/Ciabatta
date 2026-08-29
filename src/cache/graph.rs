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
//! Propagation through output hashes only works for steps that *have* outputs
//! to hash. A step that declares none — an uncached one, or one whose config
//! lists `inputs` but no `outputs` — runs every time and fingerprints to the
//! same empty value each time, so a key downstream of it agreeing proves
//! nothing about whether its result moved. Those steps are tracked here as
//! *unaccounted*, and everything behind one runs as well ([`Reason::UpstreamReran`]).
//! That is the one case where reuse is withdrawn without a named file having
//! changed, and it is withdrawn because the alternative is serving a stale
//! artifact.
//!
//! What this produces is a *prediction*. `ciabatta dry-run` prints it and stops;
//! the runner uses the same function and then acts on it. The two cannot
//! disagree, which is the only way a dry run is worth reading.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::Result;

use super::store::Store;
use super::{CacheConfig, Decision, Reason, Target, diff::Diff};
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
    // Steps that will run without declaring what they produce, so nothing
    // downstream of them can be reused on the strength of an unchanged key.
    let mut unaccounted: BTreeSet<String> = BTreeSet::new();
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

        let mut decision = super::plan(&target, env, store)?;

        // A step behind one that runs unaccounted has to run too. Its key can
        // agree only because it cannot see what happened upstream.
        let reran = reran_upstream(&step.needs, &unaccounted);
        if !reran.is_empty()
            && decision.is_reuse()
            && let Some(key) = decision.key().map(str::to_string)
        {
            decision = Decision::Rebuild {
                key,
                reason: Reason::UpstreamReran { steps: reran },
            };
        }

        // And it inherits the doubt: a step that runs without accounting for
        // its outputs leaves its own dependents nothing to check either.
        if !decision.is_reuse() && !config.accounts_for_its_outputs() {
            unaccounted.insert(step.name.clone());
        }

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

/// Which of `needs` are steps that run without accounting for their outputs.
///
/// Shared by the planner and the runner so `dry-run` and `run` can't disagree
/// about which steps have to rerun behind one.
pub fn reran_upstream(needs: &[String], unaccounted: &BTreeSet<String>) -> Vec<String> {
    needs
        .iter()
        .filter(|need| unaccounted.contains(need.as_str()))
        .cloned()
        .collect()
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
    // A transfer step runs no command, so what identifies it is what it moves
    // and where: two pushes of the same artifact to different paths are not
    // interchangeable, and the cache must not treat them as such.
    if let Some(transfer) = step.transfer() {
        commands.push(format!(
            "{}:{}:{}",
            transfer.direction.label(),
            transfer.registry.unwrap_or("-"),
            match transfer.publish_path {
                Some(crate::config::PublishPath::Single(p)) => p.clone(),
                Some(crate::config::PublishPath::Many(globs)) => globs.join(","),
                None => transfer.artifact.unwrap_or("-").to_string(),
            }
        ));
    }
    commands
}

/// Merge cache settings from the three levels that can declare them —
/// workspace, workflow, target — with the most specific level winning each field
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
pub fn effective(workspace: Option<&CacheConfig>, step: Option<&CacheConfig>) -> CacheConfig {
    let mut merged = workspace.cloned().unwrap_or_default();
    if let Some(over) = step {
        layer_over(&mut merged, over);
    }
    merged
}

/// Apply one level's declarations over what it inherited.
pub fn layer_over(base: &mut CacheConfig, over: &CacheConfig) {
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

    /// A context that gives each step its own cache settings, for the cases
    /// where the difference between two steps is the point.
    struct PerStep {
        dir: PathBuf,
        configs: BTreeMap<String, CacheConfig>,
    }

    impl StepContext for PerStep {
        fn cache_config(&self, step: &RunStep) -> CacheConfig {
            self.configs.get(&step.name).cloned().unwrap_or_default()
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

    /// Record an entry under `key`, as if that step had just built what's on
    /// disk — so a later plan can be a hit rather than a first build.
    fn store_built(store: &Store, key: &str, dir: &Path, config: &CacheConfig) {
        store
            .put(
                key,
                dir,
                crate::cache::store::Build {
                    target: "build".into(),
                    workspace: "api".into(),
                    inputs: config.hash_inputs(dir).unwrap(),
                    outputs: config.hash_outputs(dir).unwrap(),
                    env: BTreeMap::new(),
                    upstream: BTreeMap::new(),
                    duration_ms: 10,
                },
            )
            .unwrap();
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

    /// A step that declares no outputs runs every time and fingerprints to the
    /// same empty value whatever it did, so a dependent's key agreeing proves
    /// nothing. Everything behind it has to run too.
    #[test]
    fn a_step_behind_an_unaccountable_rerun_reruns_as_well() {
        let dir = scratch("unaccounted");
        write(&dir, "src/a.rs", "fn a() {}");
        write(&dir, "dist/out", "built");
        let store = Store::at(dir.join(".cache")).unwrap();

        let cached = CacheConfig {
            enabled: Some(true),
            inputs: vec!["src/**/*".into()],
            outputs: vec!["dist/**/*".into()],
            exclude: vec!["dist".into()],
            ..Default::default()
        };
        // Same settings, minus the one thing that makes a rerun accountable.
        let no_outputs = CacheConfig {
            outputs: Vec::new(),
            ..cached.clone()
        };

        let steps = vec![
            step("generate", "make gen", &[]),
            step("build", "make build", &["generate"]),
        ];
        let context = PerStep {
            dir: dir.clone(),
            configs: [
                ("generate".to_string(), no_outputs),
                ("build".to_string(), cached.clone()),
            ]
            .into_iter()
            .collect(),
        };

        // Store what `build` would have produced, under the key it plans with,
        // so that on its own it would be a hit.
        let key = plan_graph(&steps, &context, &BTreeMap::new(), &store)
            .unwrap()
            .steps[1]
            .decision
            .key()
            .unwrap()
            .to_string();
        store_built(&store, &key, &dir, &cached);

        let plan = plan_graph(&steps, &context, &BTreeMap::new(), &store).unwrap();
        assert!(
            matches!(
                &plan.steps[1].decision,
                Decision::Rebuild {
                    reason: Reason::UpstreamReran { steps },
                    ..
                } if steps == &["generate".to_string()]
            ),
            "a step behind an unaccountable rerun must run: {:?}",
            plan.steps[1].decision
        );

        // The same graph, with the upstream accounting for what it produces:
        // its key is what decides, and nothing moved, so the hit stands.
        write(&dir, "gen/stub.rs", "// generated");
        let accountable = PerStep {
            dir: dir.clone(),
            configs: [
                (
                    "generate".to_string(),
                    CacheConfig {
                        outputs: vec!["gen/**/*".into()],
                        exclude: vec!["dist".into(), "gen".into()],
                        ..cached.clone()
                    },
                ),
                ("build".to_string(), cached.clone()),
            ]
            .into_iter()
            .collect(),
        };
        let plan = plan_graph(&steps, &accountable, &BTreeMap::new(), &store).unwrap();
        let build_key = plan.steps[1].decision.key().unwrap().to_string();
        store_built(&store, &build_key, &dir, &cached);
        let plan = plan_graph(&steps, &accountable, &BTreeMap::new(), &store).unwrap();
        assert!(
            plan.steps[1].is_reuse(),
            "an upstream that can prove its outputs didn't move must not force a rerun: {:?}",
            plan.steps[1].decision
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The doubt is inherited: it reaches past the step immediately behind the
    /// one that reran, through any step that can't account for itself either.
    #[test]
    fn the_rerun_reaches_the_steps_behind_the_steps_behind_it() {
        let dir = scratch("chain");
        write(&dir, "src/a.rs", "fn a() {}");
        write(&dir, "dist/out", "built");
        let store = Store::at(dir.join(".cache")).unwrap();

        let cached = CacheConfig {
            enabled: Some(true),
            inputs: vec!["src/**/*".into()],
            outputs: vec!["dist/**/*".into()],
            exclude: vec!["dist".into()],
            ..Default::default()
        };
        let no_outputs = CacheConfig {
            outputs: Vec::new(),
            ..cached.clone()
        };

        // generate → build → test, where neither of the first two declares an
        // output. Only `test` could be reused on its key, and it must not be.
        let steps = vec![
            step("generate", "make gen", &[]),
            step("build", "make build", &["generate"]),
            step("test", "make test", &["build"]),
        ];
        let context = PerStep {
            dir: dir.clone(),
            configs: [
                ("generate".to_string(), no_outputs.clone()),
                ("build".to_string(), no_outputs),
                ("test".to_string(), cached.clone()),
            ]
            .into_iter()
            .collect(),
        };

        let key = plan_graph(&steps, &context, &BTreeMap::new(), &store)
            .unwrap()
            .steps[2]
            .decision
            .key()
            .unwrap()
            .to_string();
        store_built(&store, &key, &dir, &cached);

        let plan = plan_graph(&steps, &context, &BTreeMap::new(), &store).unwrap();
        assert_eq!(plan.tally(), (0, 3));
        assert!(
            matches!(
                &plan.steps[2].decision,
                Decision::Rebuild {
                    reason: Reason::UpstreamReran { steps },
                    ..
                } if steps == &["build".to_string()]
            ),
            "the doubt must carry through `build`: {:?}",
            plan.steps[2].decision
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

        assert_eq!(effective(Some(&workspace), None), workspace);
        assert_eq!(effective(None, None), CacheConfig::default());

        // The whole point of per-target dependencies: naming one extra variable
        // must not cost the target its inputs, its outputs, or its caching.
        let declares_env = CacheConfig {
            env: vec!["PROFILE".into()],
            ..Default::default()
        };
        let merged = effective(Some(&workspace), Some(&declares_env));
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
        let merged = effective(Some(&workspace), Some(&declares_inputs));
        assert_eq!(merged.inputs, vec!["proto/**/*".to_string()]);
        assert_eq!(merged.outputs, workspace.outputs);

        // And an explicit `enabled: false` on the target still wins.
        let opts_out = CacheConfig {
            enabled: Some(false),
            ..Default::default()
        };
        assert!(!effective(Some(&workspace), Some(&opts_out)).is_on());

        // A target can't switch caching on for a workspace that never asked.
        let no_workspace_opinion = CacheConfig {
            inputs: vec!["proto/**/*".into()],
            ..Default::default()
        };
        assert!(!effective(None, Some(&no_workspace_opinion)).is_on());
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

        // A transfer step runs no command, so what it moves and where stands in.
        let publish = RunStep {
            name: "publish".into(),
            kind: Some("push".into()),
            registry: Some("nexus".into()),
            publish_path: Some(crate::config::PublishPath::Single("app/bin".into())),
            ..Default::default()
        };
        assert_eq!(
            commands_of(&publish),
            vec!["push:nexus:app/bin".to_string()]
        );
    }
}
