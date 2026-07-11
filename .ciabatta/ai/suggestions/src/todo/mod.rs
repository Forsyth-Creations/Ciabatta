//! `ciabatta todo` — a tiny personal task list.
//!
//! Three ways to use it:
//!   ciabatta todo "take out the trash"   add a task from the command line
//!   ciabatta todo                         launch a small web app to manage tasks
//!   ciabatta todo -d                      launch that web app in the background
//!
//! Tasks live in a single JSON file under the user's home directory
//! (`~/.ciabatta/todos.json`), so the list is personal and independent of which
//! project directory you happen to be in.
//!
//! When the AI assistant daemon (`ciabatta ai serve`) is running, a task can be
//! handed off to it as a long-running job: each task carries an [`AiTask`]
//! recording its status, a live progress line, and the final answer or error.

pub mod server;

use std::path::PathBuf;
use std::sync::Mutex;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// How important a task is. Higher-priority tasks sort to the top of the list.
/// Serialized as a lowercase string (`"high"`/`"medium"`/`"low"`) so the JSON
/// file stays readable for hand-editing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Priority {
    High,
    #[default]
    Medium,
    Low,
}

impl Priority {
    /// Sort rank: higher is more important, so it sorts first.
    fn rank(self) -> u8 {
        match self {
            Priority::High => 2,
            Priority::Medium => 1,
            Priority::Low => 0,
        }
    }
}

/// Where a task stands with the AI assistant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum AiStatus {
    /// Never handed off to the assistant.
    #[default]
    Idle,
    /// Submitted; the assistant is working on it.
    Running,
    /// The assistant finished; see [`AiTask::answer`].
    Done,
    /// The assistant failed; see [`AiTask::error`].
    Failed,
}

/// The AI-assistant side of a task: its status plus whatever the last run
/// produced. Defaults to an empty, idle job so older todos.json files load.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AiTask {
    #[serde(default)]
    pub status: AiStatus,
    /// A short, human-readable line on what the assistant is doing right now
    /// (updated while `status == Running`).
    #[serde(default)]
    pub progress: String,
    /// The assistant's final answer (when `status == Done`).
    #[serde(default)]
    pub answer: String,
    /// The failure message (when `status == Failed`).
    #[serde(default)]
    pub error: String,
}

/// A single task.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Todo {
    pub id: u64,
    pub text: String,
    #[serde(default)]
    pub done: bool,
    /// How important the task is; drives the list's sort order.
    #[serde(default)]
    pub priority: Priority,
    /// RFC 3339 timestamp of when the task was added.
    pub created_at: String,
    /// The task's relationship with the AI assistant (idle by default).
    #[serde(default)]
    pub ai: AiTask,
}

/// The on-disk task list plus a monotonically increasing id counter, guarded by
/// a mutex so the web server can mutate it from multiple connection tasks.
pub struct Store {
    path: PathBuf,
    inner: Mutex<Vec<Todo>>,
}

impl Store {
    /// Open (or lazily create) the store backed by `~/.ciabatta/todos.json`.
    pub fn open() -> Result<Self> {
        let path = todos_path()?;
        let todos = load(&path)?;
        Ok(Self {
            path,
            inner: Mutex::new(todos),
        })
    }

    /// A snapshot of every task, highest priority first and newest first within
    /// a priority.
    pub fn list(&self) -> Vec<Todo> {
        let todos = self.inner.lock().unwrap();
        let mut out = todos.clone();
        out.sort_by(|a, b| {
            b.priority
                .rank()
                .cmp(&a.priority.rank())
                .then(b.id.cmp(&a.id))
        });
        out
    }

    /// Add a task and persist. Returns the created task.
    pub fn add(&self, text: &str) -> Result<Todo> {
        let text = text.trim();
        anyhow::ensure!(!text.is_empty(), "task text is empty");

        let mut todos = self.inner.lock().unwrap();
        let next_id = todos.iter().map(|t| t.id).max().unwrap_or(0) + 1;
        let todo = Todo {
            id: next_id,
            text: text.to_string(),
            done: false,
            priority: Priority::default(),
            created_at: now_rfc3339(),
            ai: AiTask::default(),
        };
        todos.push(todo.clone());
        save(&self.path, &todos)?;
        Ok(todo)
    }

    /// Flip a task's completion state and persist.
    pub fn toggle(&self, id: u64) -> Result<()> {
        let mut todos = self.inner.lock().unwrap();
        if let Some(t) = todos.iter_mut().find(|t| t.id == id) {
            t.done = !t.done;
        }
        save(&self.path, &todos)
    }

    /// Set a task's priority and persist.
    pub fn set_priority(&self, id: u64, priority: Priority) -> Result<()> {
        let mut todos = self.inner.lock().unwrap();
        if let Some(t) = todos.iter_mut().find(|t| t.id == id) {
            t.priority = priority;
        }
        save(&self.path, &todos)
    }

    /// Replace a task's text and persist.
    pub fn set_text(&self, id: u64, text: &str) -> Result<()> {
        let text = text.trim();
        anyhow::ensure!(!text.is_empty(), "task text is empty");

        let mut todos = self.inner.lock().unwrap();
        if let Some(t) = todos.iter_mut().find(|t| t.id == id) {
            t.text = text.to_string();
        }
        save(&self.path, &todos)
    }

    /// Remove a task and persist.
    pub fn remove(&self, id: u64) -> Result<()> {
        let mut todos = self.inner.lock().unwrap();
        todos.retain(|t| t.id != id);
        save(&self.path, &todos)
    }

    /// The text of a task, if it exists (used to build the AI prompt).
    pub fn text_of(&self, id: u64) -> Option<String> {
        let todos = self.inner.lock().unwrap();
        todos.iter().find(|t| t.id == id).map(|t| t.text.clone())
    }

    /// Mark a task as handed off to the assistant and persist. Returns `false`
    /// if the task doesn't exist or is already running (so we never launch two
    /// jobs for the same task).
    pub fn ai_begin(&self, id: u64) -> Result<bool> {
        let mut todos = self.inner.lock().unwrap();
        let Some(t) = todos.iter_mut().find(|t| t.id == id) else {
            return Ok(false);
        };
        if t.ai.status == AiStatus::Running {
            return Ok(false);
        }
        t.ai = AiTask {
            status: AiStatus::Running,
            progress: "Sent to the assistant…".to_string(),
            answer: String::new(),
            error: String::new(),
        };
        save(&self.path, &todos)?;
        Ok(true)
    }

    /// Update the live progress line for a running task and persist.
    pub fn ai_progress(&self, id: u64, line: &str) -> Result<()> {
        let mut todos = self.inner.lock().unwrap();
        if let Some(t) = todos.iter_mut().find(|t| t.id == id) {
            if t.ai.status == AiStatus::Running {
                t.ai.progress = line.to_string();
            }
        }
        save(&self.path, &todos)
    }

    /// Record a successful AI run (answer + Done) and persist.
    pub fn ai_done(&self, id: u64, answer: &str) -> Result<()> {
        let mut todos = self.inner.lock().unwrap();
        if let Some(t) = todos.iter_mut().find(|t| t.id == id) {
            t.ai.status = AiStatus::Done;
            t.ai.progress = String::new();
            t.ai.answer = answer.to_string();
            t.ai.error = String::new();
        }
        save(&self.path, &todos)
    }

    /// Record a failed AI run (error + Failed) and persist.
    pub fn ai_failed(&self, id: u64, error: &str) -> Result<()> {
        let mut todos = self.inner.lock().unwrap();
        if let Some(t) = todos.iter_mut().find(|t| t.id == id) {
            t.ai.status = AiStatus::Failed;
            t.ai.progress = String::new();
            t.ai.error = error.to_string();
        }
        save(&self.path, &todos)
    }
}

/// The path to the todos file: `$HOME/.ciabatta/todos.json` (creating the
/// `.ciabatta` directory if needed).
fn todos_path() -> Result<PathBuf> {
    let home = home_dir().context("Could not determine your home directory (HOME is unset)")?;
    let dir = home.join(".ciabatta");
    std::fs::create_dir_all(&dir).with_context(|| format!("Failed to create {}", dir.display()))?;
    Ok(dir.join("todos.json"))
}

/// Locate the user's home directory without pulling in an extra dependency.
fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

/// Read and parse the task list, treating a missing file as an empty list.
fn load(path: &PathBuf) -> Result<Vec<Todo>> {
    match std::fs::read_to_string(path) {
        Ok(s) if s.trim().is_empty() => Ok(Vec::new()),
        Ok(s) => {
            serde_json::from_str(&s).with_context(|| format!("Failed to parse {}", path.display()))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(e).with_context(|| format!("Failed to read {}", path.display())),
    }
}

/// Serialize the task list back to disk (pretty-printed for easy hand-editing).
fn save(path: &PathBuf, todos: &[Todo]) -> Result<()> {
    let json = serde_json::to_string_pretty(todos)?;
    std::fs::write(path, json).with_context(|| format!("Failed to write {}", path.display()))
}

/// Current time as an RFC 3339 string.
fn now_rfc3339() -> String {
    chrono::Local::now().to_rfc3339()
}
