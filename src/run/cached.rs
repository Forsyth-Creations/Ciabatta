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

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::cache::graph::StepContext;
use crate::cache::store::{Build, Store};
use crate::cache::{CacheConfig, Decision, FileHash, Reason, Source};
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
    /// Step name → fingerprint of what it produced. The third dependency.
    fingerprints: BTreeMap<String, String>,
    /// Steps that ran without being able to say what they produced: they
    /// declare no `cache.outputs`, or they failed and were recovered around.
    ///
    /// Their fingerprint is the same empty value whatever they just did, so a
    /// dependent's key agreeing says nothing about whether what it consumes
    /// moved. Everything behind one of these reruns — see
    /// [`crate::cache::CacheConfig::accounts_for_its_outputs`].
    unaccounted: BTreeSet<String>,
    /// Remote cache to consult, when this project has one configured.
    remote: Option<Remote>,
    /// Keys this run reused without asking the remote for them.
    ///
    /// The first time an entry is restored it is mirrored into the local store,
    /// and from then on every build is answered locally without a byte crossing
    /// the network. That is the point — but it means the shared cache stops
    /// hearing about the artifact everyone depends on, and its retention policy,
    /// which ages entries from last use, eventually evicts the most useful thing
    /// in it. So the uses it can't see are collected here and reported once,
    /// when the run finishes.
    reused_locally: Vec<String>,
    /// `--force`: treat every entry as missing, so each step runs. Results are
    /// still stored, so the run leaves the cache refreshed rather than cold.
    force: bool,
    /// What happened, for the summary at the end.
    pub stats: Stats,
    /// What the session decided about each step, and why — handed to the run
    /// view one step at a time through [`Session::take_report`].
    reports: HashMap<String, CacheReport>,
    /// The step whose report changed last, waiting to be taken.
    fresh_report: Option<String>,
    /// The configured remote cache as the run found it, connected or not, so a
    /// step's report can say whether the shared cache was even asked.
    remote_status: Option<RemoteStatus>,
}

/// Whether the shared cache was in play for this run.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct RemoteStatus {
    pub url: String,
    /// Registered with the server and usable. False means only the local
    /// cache was consulted, which is the first thing to know about a miss
    /// somebody expected the team's cache to answer.
    pub connected: bool,
    pub read_only: bool,
    /// Why it isn't connected, when it isn't.
    #[serde(default)]
    pub error: Option<String>,
}

/// Everything the cache knew about one step: what it decided, the entry it
/// used or compared against, and — when it rebuilt — what moved and what to do
/// about it.
///
/// Sent to the run view as the decision is made, so a node can show that it
/// was served from the cache (and from which), and so the inspector can answer
/// "why didn't this one hit?" from the run itself rather than from a dry run
/// that can only guess at it afterwards.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct CacheReport {
    /// `fresh` · `hit` · `rebuild` · `uncached`.
    pub outcome: String,
    /// `local` or `remote`, on a hit.
    #[serde(default)]
    pub source: Option<String>,
    /// The key this step was looked up under.
    #[serde(default)]
    pub key: Option<String>,
    /// The decision in one sentence.
    pub summary: String,
    /// Why it rebuilt, structured, when it did.
    #[serde(default)]
    pub reason: Option<Reason>,
    /// When the decision was made.
    pub decided_at: String,
    /// The entry that was reused, on a hit.
    #[serde(default)]
    pub entry: Option<EntryInfo>,
    /// The most recent build of this target, on a rebuild: what it was
    /// compared against.
    #[serde(default)]
    pub previous: Option<EntryInfo>,
    /// What moved since `previous`, without the line-by-line hunks — a run's
    /// view model is streamed whole on every change, so the full diff stays on
    /// the cache page.
    #[serde(default)]
    pub diff: Option<DiffSummary>,
    /// How many input files the key covered, and their total size.
    pub input_files: usize,
    pub input_bytes: u64,
    /// The variables folded into the key — names only; values can be secrets.
    #[serde(default)]
    pub env: Vec<String>,
    /// The steps this one keys on, and whether each could vouch for what it
    /// produced.
    #[serde(default)]
    pub upstream: Vec<UpstreamInfo>,
    /// The needed steps that forced this one to rebuild because they ran
    /// without accounting for their outputs.
    #[serde(default)]
    pub blocked_by: Vec<String>,
    /// Build time this hit didn't spend.
    #[serde(default)]
    pub saved_ms: u64,
    /// The shared cache, as this run found it.
    #[serde(default)]
    pub remote: Option<RemoteStatus>,
    /// What was stored after the step ran, when it was.
    #[serde(default)]
    pub stored: Option<StoredInfo>,
    /// What to change so this step can be reused next time. Every entry names
    /// something to go and do — "cache miss" on its own has never helped
    /// anybody.
    #[serde(default)]
    pub hints: Vec<String>,
}

/// A cache entry, as much of it as the run view needs.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct EntryInfo {
    pub key: String,
    pub created_at: String,
    #[serde(default)]
    pub last_used_at: Option<String>,
    pub size: u64,
    pub outputs: usize,
    pub duration_ms: u64,
}

impl From<&crate::cache::store::Entry> for EntryInfo {
    fn from(entry: &crate::cache::store::Entry) -> Self {
        EntryInfo {
            key: entry.key.clone(),
            created_at: entry.created_at.clone(),
            last_used_at: entry.last_used_at.clone(),
            size: entry.size,
            outputs: entry.outputs.len(),
            duration_ms: entry.duration_ms,
        }
    }
}

/// One upstream step, as this step's key saw it.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct UpstreamInfo {
    pub step: String,
    /// The first characters of its output fingerprint, or `None` when it
    /// contributed none (skipped, persistent, or not yet recorded).
    #[serde(default)]
    pub fingerprint: Option<String>,
    /// Whether it ran without being able to say what it produced.
    pub unaccounted: bool,
}

/// A [`Diff`](crate::cache::diff::Diff) with the contents taken out.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct DiffSummary {
    pub files: Vec<FileChange>,
    /// How many files changed in all, when `files` was capped.
    pub files_total: usize,
    pub env: Vec<String>,
    pub upstream: Vec<String>,
    pub summary: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct FileChange {
    pub path: String,
    /// `added` · `removed` · `modified`.
    pub kind: String,
    pub additions: usize,
    pub deletions: usize,
}

/// What a step that ran left in the cache.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct StoredInfo {
    pub at: String,
    pub size: u64,
    pub outputs: usize,
    /// Whether it was published to the shared cache: `None` when there is no
    /// writable one to publish to.
    #[serde(default)]
    pub uploaded: Option<bool>,
    /// Why nothing was stored, when nothing was.
    #[serde(default)]
    pub skipped: Option<String>,
}

/// How many changed files a report lists before it summarizes.
const REPORT_FILES: usize = 25;

impl DiffSummary {
    fn of(diff: &crate::cache::diff::Diff) -> Self {
        let kind = |k: &crate::cache::diff::ChangeKind| {
            match k {
                crate::cache::diff::ChangeKind::Added => "added",
                crate::cache::diff::ChangeKind::Removed => "removed",
                crate::cache::diff::ChangeKind::Modified => "modified",
            }
            .to_string()
        };
        DiffSummary {
            files: diff
                .files
                .iter()
                .take(REPORT_FILES)
                .map(|f| FileChange {
                    path: f.path.clone(),
                    kind: kind(&f.kind),
                    additions: f.additions,
                    deletions: f.deletions,
                })
                .collect(),
            files_total: diff.files.len(),
            env: diff.env.iter().map(|e| e.name.clone()).collect(),
            upstream: diff.upstream.iter().map(|u| u.step.clone()).collect(),
            summary: diff.summary(),
        }
    }
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
    /// Run it.
    ///
    /// The token carries what's needed to store the result afterwards; it's
    /// boxed because it's much the larger variant, and `Skip` is the one this
    /// exists to make common.
    Run {
        /// Why the cache didn't reuse this step, when that's worth saying —
        /// which it is when the step's own key was fine and it is running
        /// because something it needs reran.
        note: Option<String>,
        token: Box<Pending>,
    },
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
    pub fn open(root: &Path, config: &CiabattaConfig) -> Option<Session> {
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
            fingerprints: BTreeMap::new(),
            unaccounted: BTreeSet::new(),
            remote: None,
            reused_locally: Vec::new(),
            force: false,
            stats: Stats::default(),
            reports: HashMap::new(),
            fresh_report: None,
            remote_status: None,
        })
    }

    /// Run every step regardless of what the cache holds (`--force`).
    ///
    /// Only lookups are skipped. What each step produces is still stored, so a
    /// forced run is also how to replace entries you no longer trust.
    pub fn force(&mut self) {
        self.force = true;
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

        self.remote_status = Some(RemoteStatus {
            url: remote.url.clone(),
            connected: false,
            read_only: remote.read_only,
            error: None,
        });

        let client = match Client::new(&remote.url, remote.tls_verify) {
            Ok(client) => client,
            Err(e) => {
                eprintln!("note: the configured remote cache is unusable ({e:#})");
                self.remote_failed(format!("{e:#}"));
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
                if let Some(status) = self.remote_status.as_mut() {
                    status.connected = true;
                }
            }
            Err(e) => {
                eprintln!("note: the remote cache is unavailable ({e:#}); using the local one");
                self.remote_failed(format!("{e:#}"));
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
        let member = context.member(step);
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

        // What this step needs that ran without accounting for its outputs.
        // Computed the same way `dry-run` computes it, so the two agree.
        let reran = crate::cache::graph::reran_upstream(&step.needs, &self.unaccounted);

        if let Some(why) = config.why_disabled() {
            self.stats.uncached += 1;
            let mut report = self.report_base("uncached", why.to_string(), &step.needs, &upstream);
            report.hints = uncached_hints(&config);
            self.file_report(&step.name, report);
            // It runs regardless, and it has nothing to hash afterwards unless
            // it declared outputs — in which case `after` still records them.
            self.note_accountability(&step.name, &config);
            return Ok(Action::Run {
                note: None,
                token: Box::new(Pending {
                    key: None,
                    dir,
                    config,
                    workspace,
                    inputs: Vec::new(),
                    env: BTreeMap::new(),
                    upstream,
                }),
            });
        }

        let env_map: BTreeMap<String, String> =
            env.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        let target = crate::cache::Target {
            name: step.name.clone(),
            workspace: workspace.clone(),
            dir: dir.clone(),
            member: member.clone(),
            commands: crate::cache::graph::commands_of(step),
            config: config.clone(),
            upstream: upstream.clone(),
        };

        let mut decision = crate::cache::plan(&target, &env_map, &self.store)?;

        // A step behind one that ran unaccounted has to run as well: its key
        // matching only means the key cannot see what happened upstream. This
        // is checked before the cache is consulted at all, so a remote entry
        // can't reintroduce what the local decision just withdrew.
        if !reran.is_empty()
            && decision.is_reuse()
            && let Some(key) = decision.key().map(str::to_string)
        {
            decision = Decision::Rebuild {
                key,
                reason: Reason::UpstreamReran {
                    steps: reran.clone(),
                },
            };
        }

        // Forced: whatever the store says, this step runs. Decided before the
        // remote is asked, so a shared entry can't hand back what was refused.
        if self.force
            && decision.is_reuse()
            && let Some(key) = decision.key().map(str::to_string)
        {
            decision = Decision::Rebuild {
                key,
                reason: Reason::Forced,
            };
        }

        // Nothing local. Before rebuilding, ask the shared cache — somebody
        // else may already have built exactly this. Not for a step with no
        // outputs, though: there is nothing a remote entry could restore, and
        // asking anyway only filled the server's stats with misses nobody can
        // do anything about.
        if reran.is_empty()
            && !self.force
            && let (Decision::Rebuild { key, reason }, Some(remote)) = (&decision, &self.remote)
            && !matches!(reason, Reason::NoOutputs)
        {
            let key = key.clone();
            if let Some(entry) = crate::remote_cache::client::try_restore(
                &remote.client,
                &remote.project,
                &key,
                Some(&step.name),
                &dir,
            )
            .await
            {
                // Mirror it locally so the next run doesn't cross the network,
                // and so the entry's inputs are there to diff against.
                //
                // Under *this* store's names. The server keeps the entry under
                // a project-scoped key, with the project id as its workspace,
                // and a copy written as received lands under a filename no
                // local lookup ever asks for — so the next run missed locally,
                // went back to the network, and its misses had no previous
                // build to diff against.
                let mut entry = entry;
                entry.key = key.clone();
                entry.workspace = workspace.clone();
                let _ = self.store.write_manifest(&entry);
                decision = Decision::Hit {
                    key,
                    source: Source::Remote,
                    outputs: entry.outputs.len(),
                };
            }
        }

        let inputs = config.hash_inputs(&dir, member.as_deref())?;
        let env_declared = crate::cache::graph::declared_env(&config, &env_map);

        let mut report = self.report_base(
            match &decision {
                Decision::Fresh { .. } => "fresh",
                Decision::Hit { .. } => "hit",
                Decision::Rebuild { .. } => "rebuild",
                Decision::Uncached { .. } => "uncached",
            },
            decision.describe(),
            &step.needs,
            &upstream,
        );
        report.key = decision.key().map(str::to_string);
        report.input_files = inputs.len();
        report.input_bytes = inputs.iter().map(|f| f.size).sum();
        report.env = env_declared.keys().cloned().collect();
        report.blocked_by = reran.clone();
        match &decision {
            Decision::Fresh { key, .. } | Decision::Hit { key, .. } => {
                if let Decision::Hit { source, .. } = &decision {
                    report.source = Some(
                        match source {
                            Source::Local => "local",
                            Source::Remote => "remote",
                        }
                        .to_string(),
                    );
                }
                if let Ok(Some(entry)) = self.store.get(key) {
                    report.saved_ms = entry.duration_ms;
                    report.entry = Some(EntryInfo::from(&entry));
                }
            }
            Decision::Rebuild { reason, .. } => {
                report.reason = Some(reason.clone());
                report.previous = self
                    .store
                    .latest_for(&workspace, &step.name)
                    .ok()
                    .flatten()
                    .map(|e| EntryInfo::from(&e));
                // The same comparison `dry-run` shows, made against the tree
                // as it is at the moment the decision was taken.
                let diff = self
                    .store
                    .explain(
                        &workspace,
                        &step.name,
                        &dir,
                        &inputs,
                        &env_declared,
                        &upstream,
                    )
                    .ok()
                    .flatten()
                    .filter(|d| !d.is_empty());
                report.hints = rebuild_hints(
                    reason,
                    &config,
                    diff.as_ref(),
                    report.previous.is_some(),
                    &reran,
                    self.remote_status.as_ref(),
                );
                report.diff = diff.as_ref().map(DiffSummary::of);
            }
            Decision::Uncached { .. } => {}
        }
        self.file_report(&step.name, report);

        match decision {
            Decision::Fresh { key, outputs } => {
                self.stats.fresh += 1;
                self.keep_alive(&key);
                self.record_saved(&key);
                self.remember(&step.name, &key, &dir, &config)?;
                Ok(Action::Skip {
                    note: if outputs == 0 && config.writes_nothing() {
                        "up to date (it passed with these exact inputs, and writes nothing)"
                            .to_string()
                    } else {
                        format!("up to date ({outputs} output file(s) already correct)")
                    },
                })
            }
            Decision::Hit { key, source, .. } => {
                // A local hit still has to be restored; a remote one already was
                // — and a remote one told the server it was wanted on the way
                // past, so only the local case needs reporting.
                if source == Source::Local {
                    self.store.restore(&key, &dir)?;
                    self.keep_alive(&key);
                }
                self.stats.restored += 1;
                self.record_saved(&key);
                self.remember(&step.name, &key, &dir, &config)?;
                Ok(Action::Skip {
                    note: format!("restored from {}", source.label()),
                })
            }
            // `Uncached` can't reach here — a disabled config short-circuits
            // at the top of this function, before a key is ever computed.
            Decision::Rebuild { key, reason } => {
                self.stats.rebuilt += 1;
                // Only the upstream case is worth a line: every other reason a
                // step rebuilds is about the step itself, and `dry-run` is
                // where anyone goes to read those.
                let note = matches!(reason, Reason::UpstreamReran { .. })
                    .then(|| format!("running because {}", reason.describe()));
                self.note_accountability(&step.name, &config);
                Ok(Action::Run {
                    note,
                    token: Box::new(Pending {
                        key: Some(key),
                        dir,
                        config,
                        workspace,
                        inputs,
                        env: env_declared,
                        upstream,
                    }),
                })
            }
            Decision::Uncached { .. } => {
                self.stats.uncached += 1;
                self.note_accountability(&step.name, &config);
                Ok(Action::Run {
                    note: None,
                    token: Box::new(Pending {
                        key: None,
                        dir,
                        config,
                        workspace,
                        inputs,
                        env: env_declared,
                        upstream,
                    }),
                })
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
                // Nothing was recorded about what it produced, so its
                // dependents have nothing to check: they rerun behind it.
                self.unaccounted.insert(step.name.clone());
                self.stored(
                    &step.name,
                    StoredInfo {
                        at: crate::cache::store::now(),
                        skipped: Some(format!("its outputs couldn't be collected ({e:#})")),
                        ..Default::default()
                    },
                );
                return;
            }
        };

        // Whatever it produced is what its dependents key against, cached or not.
        self.fingerprints
            .insert(step.name.clone(), crate::cache::fingerprint(&outputs));

        let Some(key) = key else { return };
        // Nothing to keep — unless it said it writes nothing, in which case an
        // entry with no files *is* the result: "this passed with these inputs".
        if outputs.is_empty() && !config.writes_nothing() {
            self.stored(
                &step.name,
                StoredInfo {
                    at: crate::cache::store::now(),
                    skipped: Some(if config.outputs.is_empty() {
                        "no `cache.outputs` are declared, so there was nothing to store".to_string()
                    } else {
                        format!(
                            "its `cache.outputs` ({}) matched no files after it ran",
                            config.outputs.join(", ")
                        )
                    }),
                    ..Default::default()
                },
            );
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
                self.stored(
                    &step.name,
                    StoredInfo {
                        at: crate::cache::store::now(),
                        skipped: Some(format!("the local store refused it ({e:#})")),
                        ..Default::default()
                    },
                );
                return;
            }
        };

        let mut uploaded = None;
        if let Some(remote) = &self.remote
            && !remote.read_only
        {
            uploaded = Some(
                crate::remote_cache::client::try_upload(
                    &remote.client,
                    &remote.project,
                    &key,
                    &entry,
                    &dir,
                )
                .await,
            );
        }
        self.stored(
            &step.name,
            StoredInfo {
                at: entry.created_at.clone(),
                size: entry.size,
                outputs: entry.outputs.len(),
                uploaded,
                skipped: None,
            },
        );
    }

    /// Note that an entry was used, for the end-of-run report to the server.
    fn keep_alive(&mut self, key: &str) {
        // Only worth collecting when there's a server to tell. A step reused
        // twice in one graph is one use as far as retention is concerned.
        if self.remote.is_some() && !self.reused_locally.iter().any(|k| k == key) {
            self.reused_locally.push(key.to_string());
        }
    }

    /// Tell the remote cache which of its entries this run relied on.
    ///
    /// Called once, after the graph finishes, so a two-hundred-step build sends
    /// one request rather than two hundred. Entirely best-effort: the build has
    /// already succeeded, and nothing here is worth reporting to whoever ran it.
    pub async fn finish(&mut self) {
        let Some(remote) = &self.remote else { return };
        if self.reused_locally.is_empty() {
            return;
        }
        crate::remote_cache::client::try_touch(
            &remote.client,
            &remote.project,
            &self.reused_locally,
        )
        .await;
        self.reused_locally.clear();
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

    /// Note that a step which is about to run can't say what it produced, so
    /// its dependents can't be reused on an unchanged key.
    fn note_accountability(&mut self, name: &str, config: &CacheConfig) {
        if !config.accounts_for_its_outputs() {
            self.unaccounted.insert(name.to_string());
        }
    }

    /// Record that a step ran and left no result the cache can account for —
    /// it failed, or a recovery node ran in its place. What it did to the tree
    /// is unknown, so everything downstream of it reruns.
    pub fn mark_unaccounted(&mut self, name: &str) {
        self.unaccounted.insert(name.to_string());
    }

    /// Record what a reused step's outputs fingerprint to, so its dependents
    /// key correctly.
    ///
    /// From the *entry*, not from the disk. The entry's outputs are exactly
    /// what the step produced when it was built — what its dependents were
    /// keyed against then, and what `dry-run` predicts with now. Re-hashing
    /// the output globs instead picked up anything else lying under them (a
    /// stale file from another branch, a second build's artifacts), changed
    /// the fingerprint, and so changed every dependent's key: a reused step
    /// that quietly stopped everything behind it from being reused, while the
    /// dry run promised they would be.
    fn remember(&mut self, name: &str, key: &str, dir: &Path, config: &CacheConfig) -> Result<()> {
        let outputs = match self.store.get(key)? {
            Some(entry) => entry.outputs,
            // Only when the manifest has gone between the decision and here.
            None => config.hash_outputs(dir)?,
        };
        self.fingerprints
            .insert(name.to_string(), crate::cache::fingerprint(&outputs));
        Ok(())
    }

    /// Note that the configured remote couldn't be used, and why.
    fn remote_failed(&mut self, error: String) {
        if let Some(status) = self.remote_status.as_mut() {
            status.connected = false;
            status.error = Some(error);
        }
    }

    /// The parts of a report every decision shares.
    fn report_base(
        &self,
        outcome: &str,
        summary: String,
        needs: &[String],
        upstream: &BTreeMap<String, String>,
    ) -> CacheReport {
        CacheReport {
            outcome: outcome.to_string(),
            summary,
            decided_at: crate::cache::store::now(),
            remote: self.remote_status.clone(),
            upstream: needs
                .iter()
                .map(|need| UpstreamInfo {
                    step: need.clone(),
                    fingerprint: upstream.get(need).map(|f| f.chars().take(12).collect()),
                    unaccounted: self.unaccounted.contains(need),
                })
                .collect(),
            ..Default::default()
        }
    }

    fn file_report(&mut self, step: &str, report: CacheReport) {
        self.reports.insert(step.to_string(), report);
        self.fresh_report = Some(step.to_string());
    }

    /// The report that changed last, if it hasn't been taken yet.
    pub fn take_report(&mut self) -> Option<CacheReport> {
        let step = self.fresh_report.take()?;
        self.reports.get(&step).cloned()
    }

    /// Note what was (or wasn't) stored for a step that ran.
    fn stored(&mut self, step: &str, stored: StoredInfo) {
        if let Some(report) = self.reports.get_mut(step) {
            report.stored = Some(stored);
            self.fresh_report = Some(step.to_string());
        }
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
        }
    }
}

/// What to do about a step the cache isn't looking at.
fn uncached_hints(config: &CacheConfig) -> Vec<String> {
    let mut hints = Vec::new();
    if !config.is_on() {
        hints.push(
            "Caching is off for this step. Set `cache.enabled: true` with the `inputs` it \
             reads (`ciabatta cache init` proposes them) to let it be reused."
                .to_string(),
        );
    } else if config.inputs.is_empty() {
        hints.push(
            "Caching is on but no `cache.inputs` are declared, so there is nothing to key \
             on. List the files this step reads."
                .to_string(),
        );
    }
    if !config.accounts_for_its_outputs() {
        hints.push(
            "It also declares no `cache.outputs`, so every step that needs it has to rebuild \
             after it runs. If it writes nothing a later step reads, set \
             `cache.no_outputs: true`."
                .to_string(),
        );
    }
    hints
}

/// What to change so a step that rebuilt can be reused next time.
///
/// Built from the same facts the inspector shows, so each line points at
/// something on screen rather than at a guess.
fn rebuild_hints(
    reason: &Reason,
    config: &CacheConfig,
    diff: Option<&crate::cache::diff::Diff>,
    has_previous: bool,
    blocked_by: &[String],
    remote: Option<&RemoteStatus>,
) -> Vec<String> {
    let mut hints = Vec::new();
    match reason {
        Reason::NeverBuilt if has_previous => hints.push(
            "Its inputs, variables and upstream steps match the previous build, so what \
             changed is something else in the key: its command, the enabled \
             CIABATTA_FEAT_* features, or the ciabatta version that computed it."
                .to_string(),
        ),
        Reason::NeverBuilt => hints.push(
            "This is the first build of this step with these inputs — the next run with \
             nothing changed should reuse it."
                .to_string(),
        ),
        Reason::NoOutputs => hints.push(
            "Declare `cache.outputs` for the files it writes so they can be stored and \
             restored. If it writes nothing a later step reads (a test, a lint), set \
             `cache.no_outputs: true` instead — it is then skipped when its inputs are \
             unchanged, and stops forcing the steps after it to rebuild."
                .to_string(),
        ),
        // Said below, from `blocked_by`, whatever the reason turned out to be.
        Reason::UpstreamReran { .. } => {}
        Reason::OutputsMissing { .. } => hints.push(
            "The entry for these inputs exists but its stored files are gone — usually \
             retention evicted them. It will be stored again after this run."
                .to_string(),
        ),
        Reason::OutputsModified { modified } => hints.push(format!(
            "Something changed {} after it was built. If another step writes there too, \
             two steps share an output and keep invalidating each other.",
            modified.join(", ")
        )),
        Reason::Forced => {
            hints.push("The run was started with --force, which ignores the cache.".to_string())
        }
        Reason::InputsChanged { .. } => {}
    }

    // An upstream that ran without accounting for its outputs holds this step
    // back whether or not its key also moved — and when it did, the upstream is
    // usually why — so it is named regardless of the reason.
    for upstream in blocked_by {
        hints.push(format!(
            "{upstream} ran without declaring `cache.outputs`, so nothing can tell whether it \
             changed what this step reads, and this step has to rebuild after it every time. \
             Give {upstream} `cache.outputs`, or `cache.no_outputs: true` if it writes nothing \
             this step consumes."
        ));
    }

    if let Some(diff) = diff {
        // An input that is also one of this step's outputs changes every time
        // the step runs, so the step can never hit.
        let own_outputs: Vec<&str> = diff
            .files
            .iter()
            .map(|f| f.path.as_str())
            .filter(|path| {
                config
                    .outputs
                    .iter()
                    .any(|pattern| glob_covers(pattern, path))
            })
            .collect();
        if !own_outputs.is_empty() {
            hints.push(format!(
                "{} {} both an input and an output of this step, so every build changes its \
                 own key. Add {} to `cache.exclude`.",
                listed(&own_outputs),
                if own_outputs.len() == 1 { "is" } else { "are" },
                if own_outputs.len() == 1 { "it" } else { "them" },
            ));
        }
        // A blocked upstream's fingerprint moves for the reason given above;
        // calling it unreproducible as well would send somebody the wrong way.
        for upstream in diff
            .upstream
            .iter()
            .filter(|u| !blocked_by.contains(&u.step))
        {
            hints.push(format!(
                "{} produced different outputs than when this step was last built. If {} \
                 didn't change either, its build isn't reproducible — a timestamp or build \
                 id written into its outputs changes this step's key on every run.",
                upstream.step, upstream.step
            ));
        }
        for variable in &diff.env {
            hints.push(format!(
                "The declared variable {} changed since the last build.",
                variable.name
            ));
        }
    }

    if let Some(remote) = remote
        && !remote.connected
    {
        hints.push(format!(
            "The remote cache at {} wasn't reachable, so only the local cache was asked{}.",
            remote.url,
            remote
                .error
                .as_deref()
                .map(|e| format!(" ({e})"))
                .unwrap_or_default()
        ));
    }
    hints
}

/// Whether an output pattern covers `path` — a glob match, or a directory
/// pattern the path sits under.
fn glob_covers(pattern: &str, path: &str) -> bool {
    if glob::Pattern::new(pattern).is_ok_and(|p| p.matches(path)) {
        return true;
    }
    let dir = pattern
        .trim_end_matches("/**/*")
        .trim_end_matches("/**")
        .trim_end_matches('/');
    !dir.is_empty() && !dir.contains('*') && path.starts_with(&format!("{dir}/"))
}

fn listed(paths: &[&str]) -> String {
    match paths.len() {
        0..=3 => paths.join(", "),
        n => format!("{} and {} more", paths[..3].join(", "), n - 3),
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

    fn cached_step(name: &str, needs: &[&str], outputs: &str) -> RunStep {
        RunStep {
            name: name.to_string(),
            run: Some(format!("make {name}")),
            needs: needs.iter().map(|n| n.to_string()).collect(),
            cache: Some(CacheConfig {
                enabled: Some(true),
                inputs: vec!["src/**/*".into()],
                outputs: vec![format!("{outputs}/**/*")],
                exclude: vec!["gen".into(), "out".into()],
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    /// Run `steps` through one session the way the engine does: decide, and
    /// for anything that runs, "build" it and store the result.
    async fn one_run(root: &Path, steps: &[RunStep]) -> Vec<String> {
        let mut session = Session::open(root, &CiabattaConfig::default()).unwrap();
        let env = HashMap::new();
        let mut outcomes = Vec::new();
        for step in steps {
            let action = session.before(step, &env).await.unwrap();
            let report = session.take_report().unwrap();
            outcomes.push(format!("{}:{}", step.name, report.outcome));
            if let Action::Run { token, .. } = action {
                if let Some(pattern) = step.cache.as_ref().unwrap().outputs.first() {
                    let dir = root.join(pattern.trim_end_matches("/**/*"));
                    std::fs::create_dir_all(&dir).unwrap();
                    std::fs::write(dir.join("built"), format!("{} output", step.name)).unwrap();
                }
                session.after(step, token, 5).await;
            }
        }
        outcomes
    }

    /// A reused step hands its dependents the fingerprint of what it *built*,
    /// not of whatever happens to be lying under its output globs now.
    ///
    /// It used to re-hash the globs, so one stray file next to a reused step's
    /// outputs changed its fingerprint, which changed every dependent's key:
    /// the upstream was reused and everything behind it rebuilt — while
    /// `dry-run`, which fingerprints from the entry, promised a hit.
    #[tokio::test]
    async fn a_reused_upstream_does_not_stop_its_dependents_being_reused() {
        let root = scratch("reusedupstream");
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/lib.rs"), "fn lib() {}").unwrap();
        let steps = [
            cached_step("generate", &[], "gen"),
            cached_step("build", &["generate"], "out"),
        ];

        assert_eq!(
            one_run(&root, &steps).await,
            ["generate:rebuild", "build:rebuild"]
        );

        // Something else drops a file under `generate`'s output directory.
        std::fs::write(root.join("gen/stray"), "not generate's").unwrap();

        assert_eq!(
            one_run(&root, &steps).await,
            ["generate:fresh", "build:fresh"],
            "a reused upstream must not change the keys of the steps behind it"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    /// When a step is blocked by an upstream that can't account for itself,
    /// the report says which one, and what to change.
    #[tokio::test]
    async fn the_report_names_the_upstream_that_blocked_a_step() {
        let root = scratch("blockedreport");
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/lib.rs"), "fn lib() {}").unwrap();
        let mut lint = cached_step("lint", &[], "unused");
        lint.cache.as_mut().unwrap().outputs.clear();
        let build = cached_step("build", &["lint"], "out");

        one_run(&root, &[lint.clone(), build.clone()]).await;

        let mut session = Session::open(&root, &CiabattaConfig::default()).unwrap();
        let env = HashMap::new();
        session.before(&lint, &env).await.unwrap();
        session.before(&build, &env).await.unwrap();
        let report = session.take_report().unwrap();
        assert_eq!(report.outcome, "rebuild");
        assert_eq!(report.blocked_by, ["lint"]);
        assert!(
            report.hints.iter().any(|h| h.contains("no_outputs: true")),
            "{:?}",
            report.hints
        );

        let _ = std::fs::remove_dir_all(&root);
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

    /// Re-registering against a server that hands back a different id has to
    /// overwrite the committed one.
    ///
    /// It used to append a second `project:`, so the document no longer parsed
    /// (`duplicate field`), the write was abandoned, and the id could never be
    /// updated — every run registered a brand-new project and the remote cache
    /// never hit.
    #[test]
    fn an_existing_project_id_is_replaced_rather_than_duplicated() {
        let root = scratch("projectidagain");
        std::fs::create_dir_all(root.join(".ciabatta")).unwrap();
        let path = root.join(".ciabatta/ciabatta.yaml");
        std::fs::write(
            &path,
            "workspace:\n  name: api\n\ncache:\n  enabled: true\n  remote:\n    \
             project: old-id\n    url: http://cache:8380\n    # `project` is filled in \
             by the server.\n",
        )
        .unwrap();

        record_project_id(&root, "new-id").unwrap();

        let rendered = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            rendered.matches("project:").count(),
            1,
            "one project id, not two: {rendered}"
        );
        assert!(
            rendered.contains("# `project` is filled in"),
            "comments survive"
        );

        let config: CiabattaConfig = crate::format::load(&path).unwrap();
        let remote = config.cache.unwrap().remote.unwrap();
        assert_eq!(remote.project.as_deref(), Some("new-id"));
        assert_eq!(remote.url, "http://cache:8380");

        // And doing it twice is stable rather than accumulating keys.
        record_project_id(&root, "third-id").unwrap();
        let again = std::fs::read_to_string(&path).unwrap();
        assert_eq!(again.matches("project:").count(), 1);
        let config: CiabattaConfig = crate::format::load(&path).unwrap();
        assert_eq!(
            config.cache.unwrap().remote.unwrap().project.as_deref(),
            Some("third-id")
        );

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

    fn write(dir: &Path, rel: &str, body: &str) {
        let path = dir.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }

    fn step(name: &str, needs: &[&str], cache: CacheConfig) -> RunStep {
        RunStep {
            name: name.to_string(),
            run: Some(format!("make {name}")),
            needs: needs.iter().map(|s| s.to_string()).collect(),
            cache: Some(cache),
            ..Default::default()
        }
    }

    /// Settings a step can be reused on, and the same settings with the one
    /// thing missing that makes a rerun accountable.
    fn configs() -> (CacheConfig, CacheConfig) {
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
        (cached, no_outputs)
    }

    /// Run every step through the session, as the engine does: ask, then store
    /// what a step that ran produced.
    async fn run_all(session: &mut Session, steps: &[RunStep]) -> Vec<Option<String>> {
        let env = HashMap::new();
        let mut notes = Vec::new();
        for step in steps {
            match session.before(step, &env).await.unwrap() {
                Action::Run { note, token } => {
                    session.after(step, token, 5).await;
                    notes.push(note.or(Some(String::new())));
                }
                Action::Skip { .. } => notes.push(None),
            }
        }
        notes
    }

    /// The runner has to withdraw reuse for the same reason `dry-run` predicts
    /// it will: an upstream that reran and can't say what it produced.
    #[tokio::test]
    async fn a_step_behind_an_unaccountable_rerun_is_not_reused() {
        let root = scratch("rerun");
        write(&root, "src/a.rs", "fn a() {}");
        write(&root, "dist/out", "built");
        let (cached, no_outputs) = configs();
        let steps = vec![
            step("generate", &[], no_outputs),
            step("build", &["generate"], cached),
        ];

        // First run: nothing is cached, so both run and `build` is stored.
        let mut session = Session::open(&root, &CiabattaConfig::default()).unwrap();
        assert_eq!(
            run_all(&mut session, &steps)
                .await
                .iter()
                .filter(|n| n.is_some())
                .count(),
            2,
            "nothing is built yet, so nothing can be skipped"
        );

        // Second run, with not one file touched. `build`'s key is unchanged —
        // but `generate` ran again and nothing recorded what it did.
        let mut session = Session::open(&root, &CiabattaConfig::default()).unwrap();
        let notes = run_all(&mut session, &steps).await;
        let build = notes[1]
            .as_ref()
            .expect("build must not be reused behind an upstream that reran");
        assert!(
            build.contains("generate") && build.contains("cache.outputs"),
            "the reason must name the step and what it's missing: {build}"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    /// `--force` reruns what the cache would have skipped, and stores what it
    /// produces, so the ordinary run after it reuses everything again.
    #[tokio::test]
    async fn a_forced_run_reruns_everything_and_still_stores_it() {
        let root = scratch("forced");
        write(&root, "src/a.rs", "fn a() {}");
        write(&root, "gen/stub.rs", "// generated");
        write(&root, "dist/out", "built");
        let (cached, _) = configs();
        let generates = CacheConfig {
            outputs: vec!["gen/**/*".into()],
            exclude: vec!["dist".into(), "gen".into()],
            ..cached.clone()
        };
        let steps = vec![
            step("generate", &[], generates),
            step("build", &["generate"], cached),
        ];

        let mut session = Session::open(&root, &CiabattaConfig::default()).unwrap();
        run_all(&mut session, &steps).await;

        let mut session = Session::open(&root, &CiabattaConfig::default()).unwrap();
        session.force();
        let notes = run_all(&mut session, &steps).await;
        assert!(
            notes.iter().all(|n| n.is_some()),
            "a forced run must run every step on a warm cache: {notes:?}"
        );
        assert_eq!(session.stats.rebuilt, 2);

        let mut session = Session::open(&root, &CiabattaConfig::default()).unwrap();
        let notes = run_all(&mut session, &steps).await;
        assert!(
            notes.iter().all(|n| n.is_none()),
            "what the forced run produced must be reusable: {notes:?}"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    /// The other half of the same rule: an upstream that *can* prove its
    /// outputs didn't move must not cost its dependents a rerun.
    #[tokio::test]
    async fn a_step_behind_an_accountable_upstream_is_still_reused() {
        let root = scratch("accountable");
        write(&root, "src/a.rs", "fn a() {}");
        write(&root, "gen/stub.rs", "// generated");
        write(&root, "dist/out", "built");
        let (cached, _) = configs();
        let generates = CacheConfig {
            outputs: vec!["gen/**/*".into()],
            exclude: vec!["dist".into(), "gen".into()],
            ..cached.clone()
        };
        let steps = vec![
            step("generate", &[], generates),
            step("build", &["generate"], cached),
        ];

        let mut session = Session::open(&root, &CiabattaConfig::default()).unwrap();
        run_all(&mut session, &steps).await;

        let mut session = Session::open(&root, &CiabattaConfig::default()).unwrap();
        let notes = run_all(&mut session, &steps).await;
        assert!(
            notes.iter().all(|n| n.is_none()),
            "an unchanged graph must reuse everything: {notes:?}"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    /// A step that failed left the tree in a state nothing recorded, so what
    /// runs on past it — by recovery, or because the failure was tolerated —
    /// can't be served from the cache either.
    #[tokio::test]
    async fn nothing_downstream_of_a_failed_step_is_reused() {
        let root = scratch("failed");
        write(&root, "src/a.rs", "fn a() {}");
        write(&root, "gen/stub.rs", "// generated");
        write(&root, "dist/out", "built");
        let (cached, _) = configs();
        let generates = CacheConfig {
            outputs: vec!["gen/**/*".into()],
            exclude: vec!["dist".into(), "gen".into()],
            ..cached.clone()
        };
        let steps = vec![
            step("generate", &[], generates),
            step("build", &["generate"], cached),
        ];

        let mut session = Session::open(&root, &CiabattaConfig::default()).unwrap();
        run_all(&mut session, &steps).await;

        // This time `generate` fails — the engine says so — and the run carries
        // on to `build`, whose key hasn't moved.
        let mut session = Session::open(&root, &CiabattaConfig::default()).unwrap();
        session.mark_unaccounted("generate");
        match session.before(&steps[1], &HashMap::new()).await.unwrap() {
            Action::Run { .. } => {}
            Action::Skip { note } => {
                panic!("build was reused behind a failed upstream: {note}")
            }
        }

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_session_opens_even_where_there_is_nothing_cached_yet() {
        let root = scratch("open");
        let session = Session::open(&root, &CiabattaConfig::default());
        assert!(session.is_some(), "an empty project still gets a session");
        let _ = std::fs::remove_dir_all(&root);
    }
}
