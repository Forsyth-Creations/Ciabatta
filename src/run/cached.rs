//! Caching a running graph: consulting the cache before a step, and storing
//! what it produced afterwards.
//!
//! Everything about *deciding* lives in [`crate::cache`]; this is the part that
//! acts on the decision while a graph is executing. It exists as its own module
//! because the engine's job — drive a DAG, handle failures, recover — is
//! complicated enough without the cache threaded through it inline.
//!
//! The rule this module holds to: **the cache may make a build faster, never
//! different**. A cache that's down, damaged, or confused costs a rebuild. It
//! never fails a build, and it never lets a step be skipped whose outputs
//! aren't verifiably the ones that step produces.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::cache::graph::StepContext;
use crate::cache::store::{Build, Store};
use crate::cache::{CacheConfig, Decision, FileHash, Source};
use crate::config::CiabattaConfig;
use crate::remote_cache::client::Client;
use crate::run::RunStep;
use crate::workspace::Workspace;

/// The cache, for the duration of one run.
///
/// Built once at the start and threaded through the graph, because two things
/// have to persist across steps: the store handle, and the fingerprints each
/// finished step contributes to its dependents' keys.
pub struct Session {
    store: Store,
    workspace: Option<Workspace>,
    config: CiabattaConfig,
    root: PathBuf,
    recipe_cache: Option<CacheConfig>,
    /// Step name → fingerprint of what it produced. The third dependency.
    fingerprints: BTreeMap<String, String>,
    /// Remote cache to consult, when this project has one configured.
    remote: Option<Remote>,
    /// What happened, for the summary at the end.
    pub stats: Stats,
}

/// A configured remote cache, once its project identity has been resolved.
struct Remote {
    client: Client,
    project: String,
    read_only: bool,
}

/// What the cache did over a run.
#[derive(Debug, Default, Clone)]
pub struct Stats {
    pub fresh: usize,
    pub restored: usize,
    pub rebuilt: usize,
    pub uncached: usize,
    /// Build time not spent, from what the reused entries cost when they ran.
    pub saved_ms: u64,
}

impl Stats {
    /// How many steps were reused.
    pub fn reused(&self) -> usize {
        self.fresh + self.restored
    }

    /// The one-line summary printed at the end of a run, or `None` when nothing
    /// was cached and there is nothing to say.
    pub fn summary(&self) -> Option<String> {
        if self.reused() == 0 && self.rebuilt == 0 {
            return None;
        }
        let mut line = format!("cache: {} reused, {} built", self.reused(), self.rebuilt);
        if self.saved_ms > 0 {
            line.push_str(&format!(
                " — about {} not spent",
                crate::cache::cli::humanize_ms(self.saved_ms)
            ));
        }
        Some(line)
    }
}

/// What the session decided to do about one step.
pub enum Action {
    /// Skip it — the outputs are already correct, or were restored.
    Skip {
        /// What to tell the user.
        note: String,
    },
    /// Run it. The token carries what's needed to store the result afterwards.
    ///
    /// Boxed because it's much the larger variant, and `Skip` is the one this
    /// exists to make common.
    Run(Box<Pending>),
}

/// A step that's about to run, holding what its entry will need.
pub struct Pending {
    key: Option<String>,
    dir: PathBuf,
    config: CacheConfig,
    workspace: String,
    inputs: Vec<FileHash>,
    env: BTreeMap<String, String>,
    upstream: BTreeMap<String, String>,
}

impl Session {
    /// Open the cache for a run rooted at `root`.
    ///
    /// Never fails: a cache that can't be opened is a cache that isn't used.
    /// Refusing to run a build because its optional cache directory wasn't
    /// writable would be exactly the wrong trade.
    pub fn open(
        root: &Path,
        config: &CiabattaConfig,
        recipe_cache: Option<CacheConfig>,
    ) -> Option<Session> {
        let store = match Store::for_project(root) {
            Ok(store) => store,
            Err(e) => {
                tracing::warn!("caching is off for this run: {e:#}");
                return None;
            }
        };

        Some(Session {
            store,
            // A run started inside one package still needs its siblings' cache
            // settings, since a compiled workflow graph spans them.
            workspace: Workspace::discover(root).ok(),
            config: config.clone(),
            root: root.to_path_buf(),
            recipe_cache,
            fingerprints: BTreeMap::new(),
            remote: None,
            stats: Stats::default(),
        })
    }

    /// Resolve this project's identity on its configured remote cache, if it
    /// has one.
    ///
    /// Best-effort and done once, up front: a server that's unreachable is
    /// reported here, at the start of the run, rather than as a surprise on
    /// every step.
    pub async fn connect_remote(&mut self) {
        let Some(remote) = self.config.cache.as_ref().and_then(|c| c.remote()).cloned() else {
            self.warn_about_ignored_remotes();
            return;
        };
        let remote = &remote;

        let client = match Client::new(&remote.url, remote.tls_verify) {
            Ok(client) => client,
            Err(e) => {
                eprintln!("note: the configured remote cache is unusable ({e:#})");
                return;
            }
        };

        let name = remote
            .name
            .clone()
            .or_else(|| self.config.workspace.as_ref().and_then(|w| w.name.clone()))
            .or_else(|| {
                self.root
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
            })
            .unwrap_or_else(|| "project".to_string());

        match client.register(&name, remote.project.as_deref()).await {
            Ok(project) => {
                // The server assigned an id and the config doesn't have it yet.
                // Write it back so every other checkout resolves to the same
                // project instead of registering a new one.
                if remote.project.as_deref() != Some(project.id.as_str())
                    && let Err(e) = record_project_id(&self.root, &project.id)
                {
                    eprintln!(
                        "note: couldn't record the remote cache project id ({e:#}); \
                         add `project: {}` under cache.remote by hand",
                        project.id
                    );
                }

                self.remote = Some(Remote {
                    client,
                    project: project.id,
                    read_only: remote.read_only,
                });
            }
            Err(e) => {
                eprintln!("note: the remote cache is unavailable ({e:#}); using the local one");
            }
        }
    }

    /// Decide what to do about `step`, restoring its outputs when it can.
    pub async fn before(
        &mut self,
        step: &RunStep,
        env: &HashMap<String, String>,
    ) -> Result<Action> {
        let context = self.context();
        let config = context.cache_config(step);
        let dir = context.dir(step);
        let workspace = context.workspace(step);

        let upstream: BTreeMap<String, String> = step
            .needs
            .iter()
            .filter_map(|need| {
                self.fingerprints
                    .get(need)
                    .map(|hash| (need.clone(), hash.clone()))
            })
            .collect();

        if config.why_disabled().is_some() {
            self.stats.uncached += 1;
            return Ok(Action::Run(Box::new(Pending {
                key: None,
                dir,
                config,
                workspace,
                inputs: Vec::new(),
                env: BTreeMap::new(),
                upstream,
            })));
        }

        let env_map: BTreeMap<String, String> =
            env.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        let target = crate::cache::Target {
            name: step.name.clone(),
            workspace: workspace.clone(),
            dir: dir.clone(),
            commands: crate::cache::graph::commands_of(step),
            config: config.clone(),
            upstream: upstream.clone(),
        };

        let mut decision = crate::cache::plan(&target, &env_map, &self.store)?;

        // Nothing local. Before rebuilding, ask the shared cache — somebody
        // else may already have built exactly this.
        if let (Decision::Rebuild { key, .. }, Some(remote)) = (&decision, &self.remote) {
            let key = key.clone();
            if crate::remote_cache::client::try_restore(&remote.client, &remote.project, &key, &dir)
                .await
            {
                // Mirror it locally so the next run doesn't cross the network,
                // and so the entry's inputs are there to diff against.
                if let Ok(Some(entry)) = remote.client.lookup(&remote.project, &key).await {
                    let _ = self.store.write_manifest(&entry);
                }
                decision = Decision::Hit {
                    key,
                    source: Source::Remote,
                    outputs: 0,
                };
            }
        }

        let inputs = config.hash_inputs(&dir)?;
        let env_declared = crate::cache::graph::declared_env(&config, &env_map);

        match decision {
            Decision::Fresh { key, outputs } => {
                self.stats.fresh += 1;
                self.record_saved(&key);
                self.remember(&step.name, &dir, &config)?;
                Ok(Action::Skip {
                    note: format!("up to date ({outputs} output file(s) already correct)"),
                })
            }
            Decision::Hit { key, source, .. } => {
                // A local hit still has to be restored; a remote one already was.
                if source == Source::Local {
                    self.store.restore(&key, &dir)?;
                }
                self.stats.restored += 1;
                self.record_saved(&key);
                self.remember(&step.name, &dir, &config)?;
                Ok(Action::Skip {
                    note: format!("restored from {}", source.label()),
                })
            }
            // `Uncached` can't reach here — a disabled config short-circuits
            // at the top of this function, before a key is ever computed.
            Decision::Rebuild { key, .. } => {
                self.stats.rebuilt += 1;
                Ok(Action::Run(Box::new(Pending {
                    key: Some(key),
                    dir,
                    config,
                    workspace,
                    inputs,
                    env: env_declared,
                    upstream,
                })))
            }
            Decision::Uncached { .. } => {
                self.stats.uncached += 1;
                Ok(Action::Run(Box::new(Pending {
                    key: None,
                    dir,
                    config,
                    workspace,
                    inputs,
                    env: env_declared,
                    upstream,
                })))
            }
        }
    }

    /// Store what a finished step produced.
    ///
    /// Best-effort throughout: a step that built successfully must be reported
    /// as successful even if the cache couldn't keep a copy of it.
    pub async fn after(&mut self, step: &RunStep, pending: Box<Pending>, duration_ms: u64) {
        let Pending {
            key,
            dir,
            config,
            workspace,
            inputs,
            env,
            upstream,
        } = *pending;

        let outputs = match config.hash_outputs(&dir) {
            Ok(outputs) => outputs,
            Err(e) => {
                eprintln!(
                    "note: couldn't collect {}'s outputs to cache ({e:#})",
                    step.name
                );
                return;
            }
        };

        // Whatever it produced is what its dependents key against, cached or not.
        self.fingerprints
            .insert(step.name.clone(), crate::cache::fingerprint(&outputs));

        let Some(key) = key else { return };
        if outputs.is_empty() {
            return;
        }

        let build = Build {
            target: step.name.clone(),
            workspace,
            inputs,
            outputs,
            env,
            upstream,
            duration_ms,
        };

        let entry = match self.store.put(&key, &dir, build) {
            Ok(entry) => entry,
            Err(e) => {
                eprintln!("note: couldn't cache {} ({e:#})", step.name);
                return;
            }
        };

        if let Some(remote) = &self.remote
            && !remote.read_only
        {
            crate::remote_cache::client::try_upload(
                &remote.client,
                &remote.project,
                &key,
                &entry,
                &dir,
            )
            .await;
        }
    }

    /// Point out a `cache.remote` declared on a sub-workspace, which does
    /// nothing.
    ///
    /// The remote is a property of the *project*: the server assigns one id
    /// per project, and one id is what makes every checkout and CI runner
    /// resolve to the same cache. So it's read from the monorepo root, and a
    /// member that declares its own is config that will never be used. Silently
    /// ignoring it would leave somebody wondering why their cache is empty.
    fn warn_about_ignored_remotes(&self) {
        let Some(workspace) = &self.workspace else {
            return;
        };

        let stray: Vec<&str> = workspace
            .members
            .iter()
            .filter(|member| {
                member
                    .config
                    .cache
                    .as_ref()
                    .is_some_and(|c| c.remote().is_some())
            })
            .map(|member| member.name.as_str())
            .collect();

        if stray.is_empty() {
            return;
        }
        eprintln!(
            "note: {} declares `cache.remote`, but the remote cache is configured \
             per project, not per sub-workspace. Move it to {}'s config \
             (`ciabatta cache init --remote <URL>` at the repository root).",
            stray.join(", "),
            workspace
                .root
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| workspace.root.display().to_string()),
        );
    }

    /// Record what a reused step's outputs fingerprint to, so its dependents
    /// key correctly.
    fn remember(&mut self, name: &str, dir: &Path, config: &CacheConfig) -> Result<()> {
        let outputs = config.hash_outputs(dir)?;
        self.fingerprints
            .insert(name.to_string(), crate::cache::fingerprint(&outputs));
        Ok(())
    }

    /// Add a reused entry's original build time to the running total.
    fn record_saved(&mut self, key: &str) {
        if let Ok(Some(entry)) = self.store.get(key) {
            self.stats.saved_ms += entry.duration_ms;
        }
    }

    fn context(&self) -> crate::cache::cli::WorkspaceContext<'_> {
        crate::cache::cli::WorkspaceContext {
            workspace: self.workspace.as_ref(),
            root: self.root.clone(),
            config: &self.config,
            recipe_cache: self.recipe_cache.clone(),
        }
    }
}

/// Write the server-assigned project id back into the workspace config.
///
/// It's committed alongside the config on purpose: it's what makes every
/// checkout and every CI runner resolve to the same project rather than each
/// registering a new one under the same name.
pub fn record_project_id(root: &Path, id: &str) -> Result<()> {
    let path = crate::config::config_path(root)
        .ok_or_else(|| anyhow::anyhow!("no ciabatta config in {}", root.display()))?;
    let existing = std::fs::read_to_string(&path)?;

    // Spliced under the `cache.remote` mapping the user already wrote, so
    // their comments and layout survive — and scoped to that mapping, so a
    // registry's `url:` elsewhere in the file can't be mistaken for it.
    let rendered =
        crate::format::insert_nested(&existing, "cache", "remote", &format!("project: {id}"))?;
    // Only write it back if the result still loads.
    let parsed: CiabattaConfig =
        crate::format::from_str(&rendered, crate::format::Format::of_path(&path))?;
    anyhow::ensure!(
        parsed
            .cache
            .and_then(|c| c.remote)
            .and_then(|r| r.project)
            .as_deref()
            == Some(id),
        "the project id didn't survive being written into {}",
        path.display()
    );

    std::fs::write(&path, rendered)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ciab_cached_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn the_project_id_is_written_back_without_disturbing_the_config() {
        let root = scratch("projectid");
        std::fs::create_dir_all(root.join(".ciabatta")).unwrap();
        std::fs::write(
            root.join(".ciabatta/ciabatta.yaml"),
            "# my comment\nworkspace:\n  name: api\n\ncache:\n  enabled: true\n  \
             inputs: [\"src/**/*\"]\n  remote:\n    url: http://cache:8380\n",
        )
        .unwrap();

        record_project_id(&root, "7f3a-1234").unwrap();

        let rendered = std::fs::read_to_string(root.join(".ciabatta/ciabatta.yaml")).unwrap();
        assert!(rendered.contains("# my comment"), "comments must survive");

        let config: CiabattaConfig =
            crate::format::load(&root.join(".ciabatta/ciabatta.yaml")).unwrap();
        let remote = config.cache.unwrap().remote.unwrap();
        assert_eq!(remote.url, "http://cache:8380");
        assert_eq!(remote.project.as_deref(), Some("7f3a-1234"));
        assert_eq!(config.workspace.unwrap().name.as_deref(), Some("api"));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_config_with_no_remote_is_reported_rather_than_mangled() {
        let root = scratch("noremote");
        std::fs::create_dir_all(root.join(".ciabatta")).unwrap();
        let path = root.join(".ciabatta/ciabatta.yaml");
        std::fs::write(&path, "workspace:\n  name: api\n").unwrap();

        assert!(record_project_id(&root, "abc").is_err());
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "workspace:\n  name: api\n",
            "a failure must leave the config exactly as it was"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn stats_summarize_only_when_there_is_something_to_say() {
        assert!(Stats::default().summary().is_none());

        let stats = Stats {
            fresh: 2,
            restored: 1,
            rebuilt: 3,
            uncached: 0,
            saved_ms: 90_000,
        };
        assert_eq!(stats.reused(), 3);
        assert_eq!(
            stats.summary().unwrap(),
            "cache: 3 reused, 3 built — about 1m 30s not spent"
        );

        // No measured saving → no claim about one.
        let stats = Stats {
            rebuilt: 1,
            ..Default::default()
        };
        assert_eq!(stats.summary().unwrap(), "cache: 0 reused, 1 built");
    }

    #[test]
    fn a_session_opens_even_where_there_is_nothing_cached_yet() {
        let root = scratch("open");
        let session = Session::open(&root, &CiabattaConfig::default(), None);
        assert!(session.is_some(), "an empty project still gets a session");
        let _ = std::fs::remove_dir_all(&root);
    }
}
