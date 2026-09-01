//! Workflow run history, shared through the cache server.
//!
//! One laptop's history answers "when did *I* last run this", which is the
//! wrong question. A workflow you haven't touched since March may be the one CI
//! runs hourly; a workflow nobody anywhere has run since March is the one worth
//! deleting. The difference matters, and only something every checkout talks to
//! can tell them apart — which the cache server already is.
//!
//! So a run reports what it ran to the project's remote cache, if it has one,
//! and the server keeps the merged picture: for each `member:workflow`, the
//! most recent run by anybody, how many runs it has seen, and how many failed.
//!
//! Two properties this deliberately has:
//!
//! * **Best-effort, always.** A server that is down, slow, or read-only costs a
//!   timestamp, never a build. Nothing here returns an error to the caller.
//!
//! * **Merged, not overwritten.** Reports arrive from many machines in no
//!   particular order, including from clocks that disagree. Taking the later
//!   timestamp and the larger counts means a slow report can never walk the
//!   picture backwards.

use std::path::Path;

use crate::config::CiabattaConfig;
use crate::run::history::Record;

/// Cap on how many records one report may carry.
///
/// A monorepo has as many units as it has packages; this bounds what a single
/// request can ask of the server however large the graph.
pub const MAX_REPORTED: usize = 500;

/// Tell the project's remote cache what just ran, and take back what everyone
/// else has run.
///
/// Both directions in one place, at the end of a run, on purpose. The reading
/// side — `ciabatta list`, the web app — then never waits on a network call to
/// answer "when did this last run": it reads the local file, which was brought
/// up to date the last time this checkout ran anything. A cache server that is
/// down slows nothing down and hides nothing; it just means the shared picture
/// is as of the last successful sync.
///
/// Silent when no remote is configured, which is the common case — the whole
/// feature degrades to a local-only history rather than to an error.
pub async fn sync(config: &CiabattaConfig, root: &Path, records: &[Record]) {
    if records.is_empty() {
        return;
    }
    let Some(remote) = config.cache.as_ref().and_then(|c| c.remote()) else {
        return;
    };
    // A read-only client may still report: saying "somebody ran this" is not
    // changing what anybody builds, and a read-only CI runner is exactly the
    // caller whose runs most want counting.
    // The id may have been assigned by *this* run: the cache session registers
    // the project on connect and writes the id into the config file, which the
    // copy loaded at startup — the one passed in here — does not have. Reading
    // it back off disk is the difference between a fresh checkout's first run
    // being reported and being silently dropped.
    let from_disk;
    let project = match remote.project.as_deref() {
        Some(project) => project,
        None => {
            from_disk = crate::config::load_config(root)
                .ok()
                .and_then(|c| c.cache.and_then(|c| c.remote().cloned()))
                .and_then(|r| r.project);
            match from_disk.as_deref() {
                Some(project) => project,
                // Never registered. The cache itself reports that loudly; there
                // is no reason to say it twice.
                None => return,
            }
        }
    };

    let client = match super::client::Client::new(&remote.url, remote.tls_verify) {
        Ok(client) => client,
        Err(_) => return,
    };

    if let Err(e) = client.report_workflows(project, records).await {
        // Debug, not a warning: it costs a shared timestamp, and the build it
        // is attached to already succeeded or failed on its own merits.
        tracing::debug!("couldn't report workflow history to the remote cache: {e:#}");
        return;
    }

    // Merged in, not written over: the local file is the more recent authority
    // for what *this* machine did, and `History::merge` keeps whichever run is
    // later either way.
    match client.workflows(project).await {
        Ok(shared) if !shared.is_empty() => {
            let mut history = crate::run::history::History::load(root);
            for record in shared {
                history.merge(record);
            }
            if let Err(e) = history.save(root) {
                tracing::debug!("couldn't merge the shared workflow history in: {e:#}");
            }
        }
        Ok(_) => {}
        Err(e) => tracing::debug!("couldn't read the shared workflow history: {e:#}"),
    }
}

// ─── The server's side ──────────────────────────────────────────────────────

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Mutex;

use anyhow::{Context, Result};

/// Every project's merged workflow history, as the server holds it.
///
/// One file rather than one per project: this is a handful of records per
/// project, read on a status page and written a few times a build. A directory
/// of tiny files would buy nothing and cost an fsync each.
#[derive(Debug)]
pub struct Store {
    path: PathBuf,
    /// project id → `member:workflow` → record.
    inner: Mutex<BTreeMap<String, BTreeMap<String, Record>>>,
}

impl Store {
    /// Open (or create) the store under `storage`.
    pub fn open(storage: &std::path::Path) -> Result<Self> {
        std::fs::create_dir_all(storage)
            .with_context(|| format!("Failed to create {}", storage.display()))?;
        let path = storage.join("workflows.json");
        let inner = std::fs::read_to_string(&path)
            .ok()
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_default();
        Ok(Store {
            path,
            inner: Mutex::new(inner),
        })
    }

    /// Fold a batch of reports into a project's picture.
    ///
    /// Merging, never replacing: reports arrive from many machines in no
    /// particular order and from clocks that disagree, so the later timestamp
    /// and the larger counts win. A slow report can then never undo a fast one.
    pub fn merge(&self, project: &str, records: &[Record]) -> Result<usize> {
        let mut guard = lock(&self.inner);
        let entry = guard.entry(project.to_string()).or_default();
        for record in records.iter().take(MAX_REPORTED) {
            let key = record.id();
            match entry.get_mut(&key) {
                Some(existing) => {
                    existing.runs = existing.runs.max(record.runs);
                    existing.failures = existing.failures.max(record.failures);
                    if record.first_run_at < existing.first_run_at {
                        existing.first_run_at = record.first_run_at.clone();
                    }
                    if record.last_run_at > existing.last_run_at {
                        existing.last_run_at = record.last_run_at.clone();
                        existing.last_outcome = record.last_outcome;
                        existing.last_duration_ms = record.last_duration_ms;
                    }
                }
                None => {
                    entry.insert(key, record.clone());
                }
            }
        }
        let total = entry.len();
        let snapshot = guard.clone();
        drop(guard);
        self.write(&snapshot)?;
        Ok(total)
    }

    /// What a project has run, most recently run first.
    pub fn for_project(&self, project: &str) -> Vec<Record> {
        let guard = lock(&self.inner);
        let mut all: Vec<Record> = guard
            .get(project)
            .map(|m| m.values().cloned().collect())
            .unwrap_or_default();
        all.sort_by(|a, b| b.last_run_at.cmp(&a.last_run_at));
        all
    }

    /// Every project's records, for the server-wide view: which workflows are
    /// stale across everyone using this cache.
    pub fn all(&self) -> BTreeMap<String, Vec<Record>> {
        let guard = lock(&self.inner);
        guard
            .iter()
            .map(|(project, records)| {
                let mut all: Vec<Record> = records.values().cloned().collect();
                all.sort_by(|a, b| b.last_run_at.cmp(&a.last_run_at));
                (project.clone(), all)
            })
            .collect()
    }

    /// Drop a project's history, for when the project itself is forgotten.
    pub fn forget(&self, project: &str) -> Result<()> {
        let mut guard = lock(&self.inner);
        guard.remove(project);
        let snapshot = guard.clone();
        drop(guard);
        self.write(&snapshot)
    }

    fn write(&self, snapshot: &BTreeMap<String, BTreeMap<String, Record>>) -> Result<()> {
        let text = serde_json::to_string_pretty(snapshot)
            .context("Failed to encode the workflow history")?;
        std::fs::write(&self.path, text)
            .with_context(|| format!("Failed to write {}", self.path.display()))
    }
}

/// A poisoned lock here means a thread panicked mid-update. The data is a
/// timestamp table, not a bank balance; recovering it beats refusing to serve.
fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}
