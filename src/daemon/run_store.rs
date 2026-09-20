//! Run history that outlives the daemon.
//!
//! A run belongs to the daemon rather than to the terminal that asked for it —
//! that is the whole design — but until now it belonged to the daemon *process*:
//! restarting it (an upgrade, a reboot, `ciabatta daemon restart`) threw away
//! every run and every log the daemon had. The logs are usually the reason
//! anyone opens a run at all, and "why did last night's build fail" is exactly
//! the question a restart erased.
//!
//! So each run is written to `~/.ciabatta/runs/<id>.json` — beside the project
//! registry, and global for the same reason it is: run ids are handed out by the
//! daemon across every project, so per-project files would collide on the first
//! restart.
//!
//! Two deliberate limits:
//!
//! * **Records expire.** A build's logs are capped at five thousand lines per
//!   step, but a hundred kept runs is still a directory nobody asked for. The
//!   TTL in [`Settings`] is how long a record survives its own creation; it is
//!   set from the web app, since that is where the runs are read.
//!
//! * **The variables a caller supplied are not written down.** The request that
//!   started a run is kept so it can be run again, but its `env` map is whatever
//!   somebody typed into the "this run needs a few variables" prompt — tokens,
//!   as often as not. It is dropped on the way to disk, and a re-run of a
//!   restored run asks for them again.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::run::view::GuiState;

use super::routes::run::CreatePayload;

/// Bumped if the shape changes, so an old record is dropped rather than
/// misread.
const VERSION: u32 = 1;

/// How long a run's record is kept by default: a week, which covers "what
/// happened overnight" and "what happened before the weekend" without keeping
/// months of builds nobody will read.
pub const DEFAULT_TTL_HOURS: u64 = 24 * 7;

/// One run, as it is written to disk.
#[derive(Serialize, Deserialize, Clone)]
pub struct StoredRun {
    #[serde(default)]
    pub version: u32,
    pub id: u64,
    pub project: String,
    pub workflows: Vec<String>,
    pub created_at: String,
    pub root: PathBuf,
    /// What started it, minus the caller-supplied variables (see the module
    /// docs).
    pub request: CreatePayload,
    pub state: GuiState,
}

/// How long run records are kept.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct Settings {
    /// Hours a record survives after the run was created. Zero means "keep
    /// them until I delete them", which is a real answer for a machine where
    /// the run log *is* the record of what shipped.
    pub ttl_hours: u64,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            ttl_hours: DEFAULT_TTL_HOURS,
        }
    }
}

/// The run records on disk, and the TTL that governs them.
pub struct RunStore {
    dir: PathBuf,
    settings: Mutex<Settings>,
}

impl RunStore {
    /// Open (and create) `~/.ciabatta/runs/`.
    pub fn open() -> Result<Self> {
        Self::open_at(super::state_dir()?.join("runs"))
    }

    /// Open a store in a specific directory — what `open` does, and what the
    /// tests use to stay out of the real one.
    pub fn open_at(dir: PathBuf) -> Result<Self> {
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("Failed to create {}", dir.display()))?;
        let settings = load_settings(&dir);
        Ok(Self {
            dir,
            settings: Mutex::new(settings),
        })
    }

    fn settings_path(&self) -> PathBuf {
        self.dir.join("settings.json")
    }

    fn record_path(&self, id: u64) -> PathBuf {
        self.dir.join(format!("{id}.json"))
    }

    pub fn settings(&self) -> Settings {
        *self.settings.lock().unwrap()
    }

    /// Change the TTL and apply it immediately — a TTL that only took effect on
    /// the next restart would look broken to whoever just shortened it.
    pub fn set_settings(&self, next: Settings) -> Result<usize> {
        *self.settings.lock().unwrap() = next;
        let json = serde_json::to_string_pretty(&next)?;
        std::fs::write(self.settings_path(), json)
            .with_context(|| format!("Failed to write {}", self.settings_path().display()))?;
        Ok(self.prune())
    }

    /// Write one run's record, replacing any earlier copy.
    ///
    /// Best effort: a run that can't be written down is still a run in flight,
    /// and failing the request that triggered the write would be a strange way
    /// to report a full disk.
    pub fn save(&self, run: &StoredRun) {
        let path = self.record_path(run.id);
        let result = serde_json::to_vec_pretty(run)
            .map_err(anyhow::Error::from)
            .and_then(|json| Ok(std::fs::write(&path, json)?));
        if let Err(err) = result {
            tracing::warn!(run = run.id, "couldn't save the run's history: {err:#}");
        }
    }

    /// Forget one run.
    pub fn delete(&self, id: u64) {
        let path = self.record_path(id);
        if let Err(err) = std::fs::remove_file(&path)
            && err.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(run = id, "couldn't delete the run's history: {err:#}");
        }
    }

    /// Every stored run, oldest id first, with anything past its TTL deleted on
    /// the way past.
    pub fn load_all(&self) -> Vec<StoredRun> {
        self.prune();
        let mut runs: Vec<StoredRun> = Vec::new();
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return runs;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.file_name().is_some_and(|n| n == "settings.json") {
                continue;
            }
            match read_record(&path) {
                Some(run) => runs.push(run),
                // A record from an older shape, or one a crash left half
                // written. Neither is worth an error on startup; both are worth
                // clearing out so they aren't retried forever.
                None => {
                    tracing::warn!(path = %path.display(), "dropping an unreadable run record");
                    let _ = std::fs::remove_file(&path);
                }
            }
        }
        runs.sort_by_key(|run| run.id);
        runs
    }

    /// Delete records older than the TTL, returning how many went.
    pub fn prune(&self) -> usize {
        let ttl = self.settings().ttl_hours;
        if ttl == 0 {
            return 0;
        }
        let cutoff = chrono::Local::now() - chrono::Duration::hours(ttl as i64);
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return 0;
        };
        let mut gone = 0;
        for entry in entries.flatten() {
            let path = entry.path();
            if path.file_name().is_some_and(|n| n == "settings.json") {
                continue;
            }
            let Some(run) = read_record(&path) else {
                continue;
            };
            let expired = chrono::DateTime::parse_from_rfc3339(&run.created_at)
                .map(|at| at < cutoff)
                // An unparseable timestamp is from a record we can't age, and
                // keeping it forever is the safer of the two mistakes.
                .unwrap_or(false);
            if expired && std::fs::remove_file(&path).is_ok() {
                gone += 1;
            }
        }
        gone
    }
}

fn read_record(path: &Path) -> Option<StoredRun> {
    if path.extension().is_none_or(|e| e != "json") {
        return None;
    }
    let text = std::fs::read_to_string(path).ok()?;
    let run: StoredRun = serde_json::from_str(&text).ok()?;
    (run.version == VERSION).then_some(run)
}

fn load_settings(dir: &Path) -> Settings {
    std::fs::read_to_string(dir.join("settings.json"))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

/// The version stamp to write on a new record.
pub const fn version() -> u32 {
    VERSION
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store(name: &str) -> RunStore {
        let dir = std::env::temp_dir().join(format!("ciab_runs_{}_{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        RunStore::open_at(dir).expect("store opens")
    }

    fn record(id: u64, created_at: &str) -> StoredRun {
        StoredRun {
            version: VERSION,
            id,
            project: "p".into(),
            workflows: vec!["build".into()],
            created_at: created_at.into(),
            root: PathBuf::from("/tmp"),
            request: serde_json::from_str("{\"project\":\"p\",\"workflow\":\"build\"}")
                .expect("payload parses"),
            state: GuiState::default(),
        }
    }

    #[test]
    fn a_saved_run_comes_back_after_a_restart() {
        let store = store("roundtrip");
        store.save(&record(7, &chrono::Local::now().to_rfc3339()));

        let restored = store.load_all();
        assert_eq!(restored.len(), 1);
        assert_eq!(restored[0].id, 7);
        assert_eq!(restored[0].workflows, vec!["build".to_string()]);

        store.delete(7);
        assert!(store.load_all().is_empty());
    }

    #[test]
    fn the_ttl_drops_old_records_and_keeps_new_ones() {
        let store = store("ttl");
        let old = (chrono::Local::now() - chrono::Duration::hours(50)).to_rfc3339();
        store.save(&record(1, &old));
        store.save(&record(2, &chrono::Local::now().to_rfc3339()));

        let pruned = store
            .set_settings(Settings { ttl_hours: 24 })
            .expect("settings save");
        assert_eq!(pruned, 1);
        let kept: Vec<u64> = store.load_all().iter().map(|r| r.id).collect();
        assert_eq!(kept, vec![2]);

        // Zero means keep everything, however old.
        store.save(&record(3, &old));
        store.set_settings(Settings { ttl_hours: 0 }).expect("off");
        assert_eq!(store.load_all().len(), 2);
    }

    #[test]
    fn the_stored_request_never_carries_the_variables_it_was_given() {
        let payload: CreatePayload =
            serde_json::from_str(r#"{"project":"p","workflow":"build","env":{"TOKEN":"hunter2"}}"#)
                .expect("payload parses");
        let json = serde_json::to_string(&payload).expect("serializes");
        assert!(!json.contains("hunter2"), "{json}");
        assert!(!json.contains("TOKEN"), "{json}");
    }
}
