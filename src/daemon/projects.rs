//! The project registry.
//!
//! One global daemon serves many checkouts, but most ciabatta state is
//! per-repo: `.ciabatta/ai/`, `.ciabatta/workflows/`, `.ciabatta/.cache/`.
//! So every feature route except todo is scoped by a `project` id, and this
//! module maps that id back to a directory on disk.
//!
//! Todo is scoped too, from 0.2.0: the list lives in `~/.ciabatta/todos.json`
//! but each task carries the project it belongs to, so the switcher selects
//! which list you see.
//!
//! The registry persists to `~/.ciabatta/projects.json` so the project switcher
//! still lists your repos after a daemon restart.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// A checkout the daemon knows about.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Project {
    /// Short stable hash of the canonical path — safe to put in a URL.
    pub id: String,
    /// The absolute project root (the directory holding `.ciabatta/`).
    pub path: PathBuf,
    /// The directory's own name, for display.
    pub name: String,
}

/// The set of known projects, backed by `~/.ciabatta/projects.json`.
#[derive(Debug)]
pub struct Registry {
    path: PathBuf,
    inner: Mutex<Vec<Project>>,
}

impl Registry {
    /// Open the registry, loading any previously registered projects.
    pub fn open() -> Result<Self> {
        let path = super::state_dir()?.join("projects.json");
        let inner = Mutex::new(load(&path));
        Ok(Self { path, inner })
    }

    /// Every known project, most recently registered last.
    pub fn list(&self) -> Vec<Project> {
        self.inner.lock().unwrap().clone()
    }

    /// Look a project up by id.
    pub fn get(&self, id: &str) -> Option<Project> {
        self.inner
            .lock()
            .unwrap()
            .iter()
            .find(|p| p.id == id)
            .cloned()
    }

    /// Register a directory, returning the resulting project. Registering an
    /// already-known path is a no-op that returns the existing entry, so CLI
    /// commands can call this unconditionally on every invocation.
    ///
    /// `dir` may be anywhere inside a checkout: the nearest ancestor holding a
    /// `.ciabatta/` directory becomes the root, falling back to `dir` itself
    /// when there is no such ancestor (a project that hasn't run `init` yet).
    pub fn register(&self, dir: &Path) -> Result<Project> {
        let canonical = dir
            .canonicalize()
            .with_context(|| format!("No such directory: {}", dir.display()))?;
        let root = crate::config::find_root(&canonical).unwrap_or(canonical);

        let project = Project {
            id: project_id(&root),
            name: display_name(&root),
            path: root,
        };

        let mut guard = self.inner.lock().unwrap();
        if let Some(existing) = guard.iter().find(|p| p.id == project.id) {
            return Ok(existing.clone());
        }
        guard.push(project.clone());
        let snapshot = guard.clone();
        drop(guard);

        save(&self.path, &snapshot)?;
        Ok(project)
    }

    /// Forget a project. Its on-disk state is untouched — this only removes it
    /// from the switcher.
    pub fn forget(&self, id: &str) -> Result<bool> {
        let mut guard = self.inner.lock().unwrap();
        let before = guard.len();
        guard.retain(|p| p.id != id);
        let removed = guard.len() != before;
        let snapshot = guard.clone();
        drop(guard);

        if removed {
            save(&self.path, &snapshot)?;
        }
        Ok(removed)
    }
}

/// A short, stable, filesystem-independent id for a project root.
///
/// FNV-1a over the path string, hex encoded. This only needs to be stable and
/// collision-resistant enough for a handful of local checkouts, not
/// cryptographic.
///
/// Public because it's a *pure function of the path*, and that matters: a CLI
/// command can work out which project it's in without opening the registry.
/// Opening it would mean writing a file the running daemon already holds in
/// memory, leaving the daemon's copy stale — so the two would disagree about
/// which projects exist.
pub fn project_id(root: &Path) -> String {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;

    let hash = root
        .to_string_lossy()
        .bytes()
        .fold(OFFSET, |acc, b| (acc ^ u64::from(b)).wrapping_mul(PRIME));
    format!("{hash:016x}")
}

/// The name shown in the project switcher: the directory's own name, or the
/// full path if it somehow has none (a filesystem root).
fn display_name(root: &Path) -> String {
    root.file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| root.to_string_lossy().to_string())
}

/// Read the registry file, treating any problem as "no projects yet" — a
/// corrupted file shouldn't stop the daemon from starting.
fn load(path: &Path) -> Vec<Project> {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    serde_json::from_str(&raw).unwrap_or_default()
}

fn save(path: &Path, projects: &[Project]) -> Result<()> {
    let body = serde_json::to_string_pretty(projects)?;
    std::fs::write(path, body).with_context(|| format!("Failed to write {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_ids_are_stable_and_path_specific() {
        let a = project_id(Path::new("/home/me/repo"));
        let b = project_id(Path::new("/home/me/repo"));
        let c = project_id(Path::new("/home/me/other"));

        assert_eq!(a, b, "the same path must always hash the same");
        assert_ne!(a, c);
        assert_eq!(a.len(), 16);
        assert!(a.chars().all(|ch| ch.is_ascii_hexdigit()));
    }

    #[test]
    fn display_name_is_the_directory_name() {
        assert_eq!(display_name(Path::new("/home/me/ciabatta")), "ciabatta");
        assert_eq!(display_name(Path::new("/")), "/");
    }

    #[test]
    fn load_tolerates_a_missing_or_corrupt_file() {
        assert!(load(Path::new("/nonexistent/projects.json")).is_empty());

        let dir = std::env::temp_dir().join("ciabatta-registry-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("corrupt.json");
        std::fs::write(&path, "{not json").unwrap();
        assert!(load(&path).is_empty());
        let _ = std::fs::remove_file(&path);
    }
}
