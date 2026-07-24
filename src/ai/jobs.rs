//! Background AI tasks — "ship to AI".
//!
//! A job is a prompt the assistant works on behind the scenes: it runs the full
//! agent loop autonomously (in auto-accept mode, so changes actually land) while
//! you keep working. Jobs are tracked with a status and persisted to
//! `.ciabatta/ai/jobs.json`, so their results survive restarts and can be polled
//! from the mind-map GUI or listed with `ciabatta ai jobs`.
//!
//! Each job runs on its own [`Assistant`] with an isolated conversation, so a
//! background task never tangles with your interactive session's history.

use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::mpsc;

use super::{AiEvent, Assistant, Mode};
use crate::config::CiabattaConfig;

/// Where a job is in its lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum JobStatus {
    Queued,
    Running,
    Done,
    Failed,
}

impl JobStatus {
    pub fn label(self) -> &'static str {
        match self {
            JobStatus::Queued => "queued",
            JobStatus::Running => "running",
            JobStatus::Done => "done",
            JobStatus::Failed => "failed",
        }
    }
}

/// One background task and everything it produced.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Job {
    pub id: u64,
    pub prompt: String,
    /// Where the job came from, e.g. "manual", "gui", or "todo:3".
    pub source: String,
    pub status: JobStatus,
    pub created_at: String,
    #[serde(default)]
    pub started_at: Option<String>,
    #[serde(default)]
    pub finished_at: Option<String>,
    /// Human-readable tool steps the agent took.
    #[serde(default)]
    pub steps: Vec<String>,
    /// Files the job changed (auto-accepted while running).
    #[serde(default)]
    pub changed_files: Vec<String>,
    #[serde(default)]
    pub answer: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
    /// The saved conversation this job's transcript lives in, for resuming.
    #[serde(default)]
    pub conversation_id: Option<String>,
}

/// The background-job runner: a persisted queue plus the project context needed
/// to spin up a fresh assistant per job.
pub struct Jobs {
    root: PathBuf,
    config: CiabattaConfig,
    path: PathBuf,
    inner: Mutex<Vec<Job>>,
    seq: AtomicU64,
}

impl Jobs {
    /// Open (or create) the job store under `.ciabatta/ai/jobs.json`.
    pub fn open(root: &std::path::Path, config: &CiabattaConfig) -> Result<std::sync::Arc<Self>> {
        let dir = root.join(crate::config::CIABATTA_DIR).join("ai");
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("Failed to create {}", dir.display()))?;
        let path = dir.join("jobs.json");
        let jobs = match std::fs::read_to_string(&path) {
            Ok(s) if s.trim().is_empty() => Vec::new(),
            Ok(s) => serde_json::from_str(&s)
                .with_context(|| format!("Failed to parse {}", path.display()))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(e).with_context(|| format!("Failed to read {}", path.display())),
        };
        Ok(std::sync::Arc::new(Self {
            root: root.to_path_buf(),
            config: config.clone(),
            path,
            inner: Mutex::new(jobs),
            seq: AtomicU64::new(1),
        }))
    }

    /// Change counter, bumped on every mutation so the GUI can poll cheaply.
    pub fn seq(&self) -> u64 {
        self.seq.load(Ordering::Relaxed)
    }

    /// A snapshot of every job, newest first.
    pub fn list(&self) -> Vec<Job> {
        let mut jobs = self.inner.lock().unwrap().clone();
        jobs.sort_by_key(|j| std::cmp::Reverse(j.id));
        jobs
    }

    /// The jobs as a JSON payload for the daemon API.
    pub fn snapshot_json(&self) -> Value {
        json!({ "seq": self.seq(), "jobs": self.list() })
    }

    /// Queue a task and start it in the background, returning its id. The work
    /// happens on a spawned task; poll [`Self::list`] for progress.
    pub fn ship(self: &std::sync::Arc<Self>, prompt: &str, source: &str) -> Result<u64> {
        let id = self.enqueue(prompt, source)?;
        let this = self.clone();
        let prompt = prompt.to_string();
        tokio::spawn(async move {
            this.execute(id, &prompt).await;
        });
        Ok(id)
    }

    /// Queue a task and run it to completion (used by the CLI, which then prints
    /// the result). Returns the finished job record.
    pub async fn ship_and_wait(
        self: &std::sync::Arc<Self>,
        prompt: &str,
        source: &str,
    ) -> Result<Job> {
        let id = self.enqueue(prompt, source)?;
        self.execute(id, prompt).await;
        self.get(id).context("job vanished after completion")
    }

    /// A single job by id.
    pub fn get(&self, id: u64) -> Option<Job> {
        self.inner
            .lock()
            .unwrap()
            .iter()
            .find(|j| j.id == id)
            .cloned()
    }

    /// Create a queued job record and persist it.
    fn enqueue(&self, prompt: &str, source: &str) -> Result<u64> {
        let prompt = prompt.trim();
        anyhow::ensure!(!prompt.is_empty(), "cannot ship an empty task to the AI");
        let mut jobs = self.inner.lock().unwrap();
        let id = jobs.iter().map(|j| j.id).max().unwrap_or(0) + 1;
        jobs.push(Job {
            id,
            prompt: prompt.to_string(),
            source: source.to_string(),
            status: JobStatus::Queued,
            created_at: now(),
            started_at: None,
            finished_at: None,
            steps: Vec::new(),
            changed_files: Vec::new(),
            answer: None,
            error: None,
            conversation_id: None,
        });
        persist(&self.path, &jobs);
        self.seq.fetch_add(1, Ordering::Relaxed);
        Ok(id)
    }

    /// Run one job: a fresh auto-accept assistant works the prompt to a finish,
    /// and the outcome (answer or error, steps, changed files) is recorded.
    async fn execute(&self, id: u64, prompt: &str) {
        self.update(id, |j| {
            j.status = JobStatus::Running;
            j.started_at = Some(now());
        });

        let assistant = match Assistant::new(&self.root, &self.config) {
            Ok(a) => a,
            Err(e) => {
                self.update(id, |j| {
                    j.status = JobStatus::Failed;
                    j.finished_at = Some(now());
                    j.error = Some(e.to_string());
                });
                return;
            }
        };
        // Behind-the-scenes work should actually complete, so changes land
        // without waiting for interactive approval.
        assistant.set_mode(Mode::AutoAccept);

        let (tx, mut rx) = mpsc::channel::<AiEvent>(64);
        let collector = tokio::spawn(async move {
            let mut steps = Vec::new();
            let mut changed = Vec::new();
            while let Some(ev) = rx.recv().await {
                match ev {
                    AiEvent::Status(s) => steps.push(s),
                    AiEvent::Suggestion(c) => changed.push(c.file),
                    _ => {}
                }
            }
            (steps, changed)
        });

        let result = assistant.ask(prompt, tx).await;
        let (steps, changed) = collector.await.unwrap_or_default();
        let conversation_id = assistant.conversation_id().await;

        self.update(id, |j| {
            j.steps = steps.clone();
            j.changed_files = changed.clone();
            j.conversation_id = Some(conversation_id.clone());
            j.finished_at = Some(now());
            match &result {
                Ok(answer) => {
                    j.status = JobStatus::Done;
                    j.answer = Some(answer.clone());
                }
                Err(e) => {
                    j.status = JobStatus::Failed;
                    // Full context chain, so a failed job records *why* (timeout,
                    // refused connection, …) not just a generic message.
                    j.error = Some(format!("{e:#}"));
                }
            }
        });
    }

    /// Apply a mutation to one job, persist, and bump the sequence.
    fn update(&self, id: u64, f: impl FnOnce(&mut Job)) {
        let mut jobs = self.inner.lock().unwrap();
        if let Some(j) = jobs.iter_mut().find(|j| j.id == id) {
            f(j);
        }
        persist(&self.path, &jobs);
        self.seq.fetch_add(1, Ordering::Relaxed);
    }
}

/// Persist the job list, logging (not surfacing) any write error — a failed
/// save shouldn't abort a running job.
fn persist(path: &PathBuf, jobs: &[Job]) {
    match serde_json::to_string_pretty(jobs) {
        Ok(json) => {
            if let Err(e) = std::fs::write(path, json) {
                tracing::debug!("failed to persist jobs to {}: {e}", path.display());
            }
        }
        Err(e) => tracing::debug!("failed to serialize jobs: {e}"),
    }
}

fn now() -> String {
    chrono::Local::now().to_rfc3339()
}
