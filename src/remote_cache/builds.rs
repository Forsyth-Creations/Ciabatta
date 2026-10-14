//! Build records, kept by the cache server so a build can be looked at again.
//!
//! [`super::workflows`] keeps one line per workflow — when it last ran, how
//! often it fails. That answers "is anyone still running this?" and nothing
//! about any *particular* build. This keeps the builds themselves: who ran
//! what, on which branch and commit, how it ended, and the flow it ran — every
//! step, what it needed, how long it took, and whether the cache reused it.
//! The server shows them as a feed on its own page (`/builds`), each one
//! openable into the same graph the run view drew while it was happening.
//!
//! **Metadata only.** Logs stay on the machine that produced them: they can
//! carry secrets, they are most of a run's size, and the questions this
//! answers — "what did last night's CI build reuse?", "which commit was the
//! failing one built from?" — don't need them.
//!
//! Best-effort in the same way the rest of the shared history is: a server
//! that is down or slow costs a record, never a build.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::config::CiabattaConfig;
use crate::runner::ProgressUpdate;

/// How many builds the server keeps per project. Older ones are dropped as
/// new ones arrive — this is a feed, not an archive.
pub const MAX_BUILDS_PER_PROJECT: usize = 500;

/// Cap on the steps one record may carry, so a single request is bounded
/// however large the graph.
pub const MAX_STEPS: usize = 2_000;

/// One build, as the server keeps it.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct BuildRecord {
    /// Assigned by the client, so a retried report can't be stored twice.
    pub id: String,
    /// What was run — the workflow name, or several joined with `+`.
    pub workflow: String,
    /// `success` · `failed` · `stopped`.
    pub outcome: String,
    pub started_at: String,
    pub finished_at: String,
    pub duration_ms: u64,
    /// The ciabatta that ran it.
    #[serde(default)]
    pub version: String,
    /// Who and where: the OS user and host, for telling a laptop from CI.
    #[serde(default)]
    pub user: Option<String>,
    #[serde(default)]
    pub host: Option<String>,
    /// The CI system it ran under, when it ran under one.
    #[serde(default)]
    pub ci: Option<String>,
    #[serde(default)]
    pub branch: Option<String>,
    #[serde(default)]
    pub commit: Option<String>,
    #[serde(default)]
    pub tag: Option<String>,
    #[serde(default)]
    pub build_number: Option<String>,
    /// The error that ended it, when it failed.
    #[serde(default)]
    pub error: Option<String>,
    /// The flow it ran.
    #[serde(default)]
    pub steps: Vec<BuildStep>,
    /// Filled in by the server: which user reported it.
    #[serde(default)]
    pub reported_by: Option<String>,
}

/// One step of a recorded build.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct BuildStep {
    pub name: String,
    #[serde(default)]
    pub workspace: Option<String>,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub needs: Vec<String>,
    /// `success` · `failed` · `skipped` · `reused` · `not run`.
    pub status: String,
    #[serde(default)]
    pub duration_ms: Option<u64>,
    /// What the cache decided: `fresh` · `hit` · `rebuild` · `uncached`.
    #[serde(default)]
    pub cache: Option<String>,
    /// `local` or `remote`, on a hit.
    #[serde(default)]
    pub cache_source: Option<String>,
    /// The key it was looked up under.
    #[serde(default)]
    pub cache_key: Option<String>,
    /// Why it was skipped, or what the cache said about it.
    #[serde(default)]
    pub note: Option<String>,
}

/// A record without its steps — what the feed lists.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BuildSummary {
    #[serde(flatten)]
    pub record: BuildRecord,
    pub step_count: usize,
    pub reused: usize,
    pub built: usize,
    pub failed: usize,
}

impl BuildRecord {
    pub fn summary(&self) -> BuildSummary {
        let count = |status: &str| self.steps.iter().filter(|s| s.status == status).count();
        BuildSummary {
            step_count: self.steps.len(),
            reused: count("reused"),
            built: count("success"),
            failed: count("failed"),
            record: BuildRecord {
                steps: Vec::new(),
                ..self.clone()
            },
        }
    }
}

// ─── Recording a run ────────────────────────────────────────────────────────

/// Collects what each step did, from the run's own progress updates.
///
/// Fed every update on its way to whatever is displaying the run, so the
/// record is built from exactly what the run view saw rather than from a
/// second account of it that could disagree.
#[derive(Debug, Default)]
pub struct Ledger {
    started: HashMap<String, std::time::Instant>,
    steps: HashMap<String, BuildStep>,
}

impl Ledger {
    pub fn observe(&mut self, update: &ProgressUpdate) {
        match update {
            ProgressUpdate::StepStarted { step, .. } => {
                self.started.insert(step.clone(), std::time::Instant::now());
                self.step(step).status = "running".to_string();
            }
            ProgressUpdate::StepFinished { step, ok, .. } => {
                let elapsed = self
                    .started
                    .get(step)
                    .map(|at| at.elapsed().as_millis() as u64);
                let entry = self.step(step);
                entry.status = if *ok { "success" } else { "failed" }.to_string();
                entry.duration_ms = elapsed;
            }
            ProgressUpdate::StepWarned { step, detail, .. } => {
                let entry = self.step(step);
                entry.status = "warning".to_string();
                entry.note = Some(detail.clone());
            }
            ProgressUpdate::StepSkipped { step, reason, .. } => {
                let entry = self.step(step);
                // The cache skipping a step is a reuse, not a condition.
                if !matches!(entry.cache.as_deref(), Some("fresh" | "hit")) {
                    entry.status = "skipped".to_string();
                }
                entry.note = Some(reason.clone());
            }
            ProgressUpdate::StepCache { step, report, .. } => {
                let entry = self.step(step);
                entry.cache = Some(report.outcome.clone());
                entry.cache_source = report.source.clone();
                entry.cache_key = report.key.clone();
                if matches!(report.outcome.as_str(), "fresh" | "hit") {
                    entry.status = "reused".to_string();
                }
                if entry.note.is_none() {
                    entry.note = Some(report.summary.clone());
                }
            }
            _ => {}
        }
    }

    fn step(&mut self, name: &str) -> &mut BuildStep {
        self.steps
            .entry(name.to_string())
            .or_insert_with(|| BuildStep {
                name: name.to_string(),
                status: "not run".to_string(),
                ..Default::default()
            })
    }

    /// The flow, in the graph's own order, with what each step did.
    pub fn steps(mut self, graph: &[crate::run::RunStep]) -> Vec<BuildStep> {
        graph
            .iter()
            .take(MAX_STEPS)
            .map(|node| {
                let mut step = self.steps.remove(&node.name).unwrap_or_else(|| BuildStep {
                    name: node.name.clone(),
                    status: "not run".to_string(),
                    ..Default::default()
                });
                // Still "running" means the run ended around it.
                if step.status == "running" {
                    step.status = "not run".to_string();
                }
                step.workspace = node.workspace.clone();
                step.kind = node
                    .kind
                    .clone()
                    .or_else(|| node.recover.then(|| "recover".to_string()));
                step.needs = node.needs.clone();
                step
            })
            .collect()
    }
}

/// The context a record carries besides its steps, read from the run's
/// variables the same way its publish paths read them.
pub fn describe(env: &HashMap<String, String>) -> BuildRecord {
    let get = |key: &str| env.get(key).filter(|v| !v.is_empty()).cloned();
    BuildRecord {
        id: uuid::Uuid::new_v4().to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        user: std::env::var("USER")
            .or_else(|_| std::env::var("USERNAME"))
            .ok()
            .filter(|u| !u.is_empty()),
        host: std::env::var("HOSTNAME")
            .ok()
            .or_else(|| std::env::var("COMPUTERNAME").ok())
            .or_else(|| std::fs::read_to_string("/etc/hostname").ok())
            .map(|h| h.trim().to_string())
            .filter(|h| !h.is_empty()),
        ci: detect_ci(),
        branch: get("CIABATTA_BRANCH"),
        commit: get("CIABATTA_COMMIT"),
        tag: get("CIABATTA_TAG"),
        build_number: get("CIABATTA_BUILD_NUMBER"),
        ..Default::default()
    }
}

/// The CI system this process is running under, by the variable each one
/// sets on every job — or `None` on somebody's own machine.
fn detect_ci() -> Option<String> {
    let set = |key: &str| std::env::var(key).is_ok_and(|v| !v.is_empty());
    [
        ("GITHUB_ACTIONS", "GitHub Actions"),
        ("GITLAB_CI", "GitLab CI"),
        ("JENKINS_URL", "Jenkins"),
        ("CIRCLECI", "CircleCI"),
        ("TRAVIS", "Travis CI"),
        ("TF_BUILD", "Azure DevOps"),
        ("BITBUCKET_BUILD_NUMBER", "Bitbucket Pipelines"),
    ]
    .iter()
    .find(|(key, _)| set(key))
    .map(|(_, name)| name.to_string())
    .or_else(|| set("CI").then(|| "CI".to_string()))
}

/// Send a finished build to the project's remote cache, when it has one.
///
/// Silent when there is no remote, and on any failure: the build already
/// happened, and a missing feed entry is not worth a line of anybody's output.
pub async fn report(config: &CiabattaConfig, root: &Path, record: &BuildRecord) {
    let Some(remote) = config.cache.as_ref().and_then(|c| c.remote()) else {
        return;
    };
    // The project id may have been written by this very run — see
    // `workflows::sync` for why it's read back off disk.
    let project = match remote.project.clone() {
        Some(project) => project,
        None => match crate::config::load_config(root)
            .ok()
            .and_then(|c| c.cache.and_then(|c| c.remote().cloned()))
            .and_then(|r| r.project)
        {
            Some(project) => project,
            None => return,
        },
    };
    let Ok(client) = super::client::Client::new(&remote.url, remote.tls_verify) else {
        return;
    };
    if let Err(e) = client.report_build(&project, record).await {
        tracing::debug!("couldn't report this build to the remote cache: {e:#}");
    }
}

// ─── The server's side ──────────────────────────────────────────────────────

/// Every project's recent builds, one JSON-lines file per project.
///
/// Appended to, not rewritten, on every report — a busy CI fleet reports a
/// build a minute — and compacted back down to [`MAX_BUILDS_PER_PROJECT`] once
/// it has grown to twice that.
#[derive(Debug)]
pub struct Store {
    dir: PathBuf,
    lock: Mutex<()>,
}

impl Store {
    pub fn open(storage: &Path) -> Result<Self> {
        let dir = storage.join("builds");
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("Failed to create {}", dir.display()))?;
        Ok(Store {
            dir,
            lock: Mutex::new(()),
        })
    }

    fn path(&self, project: &str) -> PathBuf {
        // Project ids are server-assigned, but never trust one as a path.
        let safe: String = project
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        self.dir.join(format!("{safe}.jsonl"))
    }

    /// Add a build to a project's feed. A record whose id is already there is
    /// a retried report and is dropped.
    pub fn add(&self, project: &str, record: &BuildRecord) -> Result<()> {
        let _guard = self.lock.lock().unwrap_or_else(|p| p.into_inner());
        let path = self.path(project);
        // Every line is written by this function, and serde writes the `id`
        // field first — so a retry is spotted without parsing the whole feed.
        let raw = std::fs::read_to_string(&path).unwrap_or_default();
        let prefix = format!("{{\"id\":{},", serde_json::to_string(&record.id)?);
        if raw.lines().any(|line| line.starts_with(&prefix)) {
            return Ok(());
        }

        let line = serde_json::to_string(record).context("Failed to encode a build")?;
        if raw.lines().count() + 1 >= MAX_BUILDS_PER_PROJECT * 2 {
            let mut existing = read_lines(&path);
            existing.push(record.clone());
            let keep = existing.split_off(existing.len() - MAX_BUILDS_PER_PROJECT);
            let body: Vec<String> = keep
                .iter()
                .filter_map(|r| serde_json::to_string(r).ok())
                .collect();
            std::fs::write(&path, body.join("\n") + "\n")
                .with_context(|| format!("Failed to write {}", path.display()))?;
            return Ok(());
        }

        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .with_context(|| format!("Failed to open {}", path.display()))?;
        writeln!(file, "{line}").with_context(|| format!("Failed to write {}", path.display()))
    }

    /// A project's builds, newest first, at most `limit` of them.
    pub fn list(&self, project: &str, limit: usize) -> Vec<BuildRecord> {
        let mut all = read_lines(&self.path(project));
        all.sort_by(|a, b| b.finished_at.cmp(&a.finished_at));
        all.truncate(limit.min(MAX_BUILDS_PER_PROJECT));
        all
    }

    /// One build, by id.
    pub fn get(&self, project: &str, id: &str) -> Option<BuildRecord> {
        read_lines(&self.path(project))
            .into_iter()
            .find(|r| r.id == id)
    }

    /// Every project's builds, newest first, for the server-wide feed.
    pub fn feed(&self, projects: &[String], limit: usize) -> Vec<(String, BuildRecord)> {
        let mut all: Vec<(String, BuildRecord)> = projects
            .iter()
            .flat_map(|project| {
                self.list(project, limit)
                    .into_iter()
                    .map(move |r| (project.clone(), r))
            })
            .collect();
        all.sort_by(|a, b| b.1.finished_at.cmp(&a.1.finished_at));
        all.truncate(limit);
        all
    }

    /// Drop a project's builds, for when the project itself is forgotten.
    pub fn forget(&self, project: &str) {
        let _ = std::fs::remove_file(self.path(project));
    }
}

/// Every record in a JSON-lines file. A line that won't parse — a torn write,
/// or a record from a future version — is skipped rather than failing the lot.
fn read_lines(path: &Path) -> Vec<BuildRecord> {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    raw.lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ciab_builds_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn record(id: &str, finished_at: &str) -> BuildRecord {
        BuildRecord {
            id: id.to_string(),
            workflow: "build".to_string(),
            outcome: "success".to_string(),
            finished_at: finished_at.to_string(),
            steps: vec![
                BuildStep {
                    name: "compile".to_string(),
                    status: "reused".to_string(),
                    ..Default::default()
                },
                BuildStep {
                    name: "test".to_string(),
                    needs: vec!["compile".to_string()],
                    status: "success".to_string(),
                    ..Default::default()
                },
            ],
            ..Default::default()
        }
    }

    #[test]
    fn builds_are_listed_newest_first_and_a_retry_is_stored_once() {
        let dir = scratch("order");
        let store = Store::open(&dir).unwrap();
        store
            .add("p1", &record("a", "2026-01-01T00:00:00Z"))
            .unwrap();
        store
            .add("p1", &record("b", "2026-01-02T00:00:00Z"))
            .unwrap();
        store
            .add("p1", &record("a", "2026-01-01T00:00:00Z"))
            .unwrap();

        let ids: Vec<String> = store.list("p1", 10).into_iter().map(|r| r.id).collect();
        assert_eq!(ids, vec!["b", "a"]);
        assert_eq!(store.get("p1", "a").unwrap().steps.len(), 2);
        assert!(store.list("p2", 10).is_empty());

        let feed = store.feed(&["p1".to_string(), "p2".to_string()], 1);
        assert_eq!(feed.len(), 1);
        assert_eq!(feed[0].1.id, "b");

        store.forget("p1");
        assert!(store.list("p1", 10).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_feed_is_compacted_rather_than_growing_forever() {
        let dir = scratch("compact");
        let store = Store::open(&dir).unwrap();
        for i in 0..(MAX_BUILDS_PER_PROJECT * 2 + 3) {
            store
                .add(
                    "p",
                    &record(
                        &format!("{i:05}"),
                        &format!("2026-01-01T00:{:02}:{:02}Z", i / 60 % 60, i % 60),
                    ),
                )
                .unwrap();
        }
        let raw = std::fs::read_to_string(store.path("p")).unwrap();
        assert!(raw.lines().count() <= MAX_BUILDS_PER_PROJECT * 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_summary_counts_what_the_steps_did() {
        let summary = record("x", "2026-01-01T00:00:00Z").summary();
        assert_eq!(summary.step_count, 2);
        assert_eq!(summary.reused, 1);
        assert_eq!(summary.built, 1);
        assert!(summary.record.steps.is_empty());
    }

    #[test]
    fn the_ledger_turns_progress_into_a_flow() {
        let mut ledger = Ledger::default();
        ledger.observe(&ProgressUpdate::StepCache {
            workflow: "build".into(),
            step: "compile".into(),
            report: Box::new(crate::run::cached::CacheReport {
                outcome: "hit".into(),
                source: Some("remote".into()),
                key: Some("abc".into()),
                summary: "restored".into(),
                ..Default::default()
            }),
        });
        ledger.observe(&ProgressUpdate::StepSkipped {
            workflow: "build".into(),
            step: "compile".into(),
            reason: "restored from the remote cache".into(),
        });
        ledger.observe(&ProgressUpdate::StepStarted {
            workflow: "build".into(),
            step: "test".into(),
        });
        ledger.observe(&ProgressUpdate::StepFinished {
            workflow: "build".into(),
            step: "test".into(),
            ok: false,
        });

        let graph = vec![
            crate::run::RunStep {
                name: "compile".into(),
                ..Default::default()
            },
            crate::run::RunStep {
                name: "test".into(),
                needs: vec!["compile".into()],
                ..Default::default()
            },
            crate::run::RunStep {
                name: "deploy".into(),
                needs: vec!["test".into()],
                ..Default::default()
            },
        ];
        let steps = ledger.steps(&graph);
        let statuses: Vec<&str> = steps.iter().map(|s| s.status.as_str()).collect();
        assert_eq!(statuses, vec!["reused", "failed", "not run"]);
        assert_eq!(steps[0].cache_key.as_deref(), Some("abc"));
        assert_eq!(steps[0].cache_source.as_deref(), Some("remote"));
        assert_eq!(steps[1].needs, vec!["compile".to_string()]);
        assert!(steps[1].duration_ms.is_some());
    }
}
