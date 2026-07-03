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

pub mod server;

use std::path::PathBuf;
use std::sync::Mutex;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// A single task.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Todo {
    pub id: u64,
    pub text: String,
    #[serde(default)]
    pub done: bool,
    /// RFC 3339 timestamp of when the task was added.
    pub created_at: String,
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

    /// A snapshot of every task, newest first.
    pub fn list(&self) -> Vec<Todo> {
        let todos = self.inner.lock().unwrap();
        let mut out = todos.clone();
        out.sort_by(|a, b| b.id.cmp(&a.id));
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
            created_at: now_rfc3339(),
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

    /// Remove a task and persist.
    pub fn remove(&self, id: u64) -> Result<()> {
        let mut todos = self.inner.lock().unwrap();
        todos.retain(|t| t.id != id);
        save(&self.path, &todos)
    }
}

/// The path to the todos file: `$HOME/.ciabatta/todos.json` (creating the
/// `.ciabatta` directory if needed).
fn todos_path() -> Result<PathBuf> {
    let home = home_dir().context("Could not determine your home directory (HOME is unset)")?;
    let dir = home.join(".ciabatta");
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("Failed to create {}", dir.display()))?;
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
        Ok(s) => serde_json::from_str(&s)
            .with_context(|| format!("Failed to parse {}", path.display())),
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
