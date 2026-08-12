//! Remembering what the `.env` files said last time, so a change to them can
//! be announced instead of discovered.
//!
//! `.env` files are usually gitignored, but the ones a monorepo checks in —
//! `.env.example`, `.env.ci`, per-package defaults — move underneath people all
//! the time. Somebody pulls, a variable they'd never heard of becomes required,
//! and the build fails three minutes later with an error about something else
//! entirely.
//!
//! So every run snapshots the variables its `.env` files define into
//! `.ciabatta/cache/env.json`, and the next run diffs against it. Added,
//! removed, and changed variables are reported once, in the terminal and
//! through the daemon API for the web app.
//!
//! **Values are never stored** — only a salted-per-key hash of each, which is
//! enough to notice a change and useless to anyone reading the cache file. A
//! `.env` holds credentials; a cache of it must not become a second copy of
//! them.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::config::CIABATTA_DIR;

/// Where the snapshot lives, relative to the project root.
const CACHE_FILE: &str = "cache/env.json";

/// One file's worth of remembered variables: name → hash of its value.
type Vars = BTreeMap<String, String>;

/// The snapshot on disk: every `.env` file ciabatta has sourced, by path.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Snapshot {
    /// Bumped if the format ever changes, so an old cache is discarded rather
    /// than misread.
    #[serde(default = "current_version")]
    version: u32,
    /// When this snapshot was taken, for the "changed since" line.
    #[serde(default)]
    pub taken_at: Option<String>,
    /// Path (relative to the project root) → its variables.
    #[serde(default)]
    files: BTreeMap<String, Vars>,
}

fn current_version() -> u32 {
    1
}

/// What changed in one variable between snapshots.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum Change {
    /// A variable that wasn't there before.
    Added { file: String, key: String },
    /// A variable that has gone.
    Removed { file: String, key: String },
    /// A variable whose value is different now.
    Changed { file: String, key: String },
    /// A whole file that appeared.
    FileAdded { file: String, keys: usize },
    /// A whole file that is no longer sourced, or no longer exists.
    FileRemoved { file: String },
}

impl Change {
    /// The file this change is in.
    pub fn file(&self) -> &str {
        match self {
            Change::Added { file, .. }
            | Change::Removed { file, .. }
            | Change::Changed { file, .. }
            | Change::FileAdded { file, .. }
            | Change::FileRemoved { file } => file,
        }
    }

    /// One line describing it, as shown in the terminal.
    pub fn describe(&self) -> String {
        match self {
            Change::Added { file, key } => format!("+ {key} is new in {file}"),
            Change::Removed { file, key } => format!("- {key} is gone from {file}"),
            Change::Changed { file, key } => format!("~ {key} changed in {file}"),
            Change::FileAdded { file, keys } => {
                format!("+ {file} is new ({keys} variable(s))")
            }
            Change::FileRemoved { file } => format!("- {file} is no longer there"),
        }
    }
}

/// The result of a drift check: what changed, and the snapshot to store.
#[derive(Debug, Default)]
pub struct Drift {
    pub changes: Vec<Change>,
    /// True the first time a project is seen, when everything looks "added"
    /// but nothing has actually drifted.
    pub first_run: bool,
}

impl Drift {
    /// Whether there's anything worth telling the user about.
    pub fn is_noteworthy(&self) -> bool {
        !self.first_run && !self.changes.is_empty()
    }

    /// The terminal report, or `None` when there's nothing to say.
    ///
    /// Deliberately short: this is a notice printed ahead of a build somebody
    /// asked for, not the main event. It says what moved and where, and trusts
    /// the reader to open the file.
    pub fn report(&self) -> Option<String> {
        if !self.is_noteworthy() {
            return None;
        }
        let mut out = format!(
            "Environment files changed since the last run ({} change{}):",
            self.changes.len(),
            if self.changes.len() == 1 { "" } else { "s" }
        );
        for change in &self.changes {
            out.push_str(&format!("\n  {}", change.describe()));
        }
        out.push_str("\nCheck them before trusting this run — `git diff` on those files says why.");
        Some(out)
    }
}

/// Where a project's snapshot lives.
pub fn cache_path(root: &Path) -> PathBuf {
    root.join(CIABATTA_DIR).join(CACHE_FILE)
}

/// Read the stored snapshot, treating anything unreadable or stale as absent —
/// a corrupt cache should cost a notification, never a run.
pub fn load(root: &Path) -> Option<Snapshot> {
    let raw = std::fs::read_to_string(cache_path(root)).ok()?;
    let snapshot: Snapshot = serde_json::from_str(&raw).ok()?;
    (snapshot.version == current_version()).then_some(snapshot)
}

/// Snapshot the given `.env` files (paths relative to `root`) and diff them
/// against what was stored last time.
///
/// Missing files are simply absent from the snapshot rather than an error: a
/// `.env` that isn't there yet is an ordinary state, and the run's own loader
/// is what decides whether that's fatal.
pub fn check(root: &Path, env_files: &[String]) -> Drift {
    let current = read_files(root, env_files);
    let drift = compare(root, &current);

    // Store the new state even when nothing changed, so `taken_at` tracks the
    // last time we actually looked.
    let snapshot = Snapshot {
        version: current_version(),
        taken_at: Some(chrono::Local::now().to_rfc3339()),
        files: current,
    };
    if let Err(e) = store(root, &snapshot) {
        tracing::debug!(error = %e, "could not write the env cache");
    }
    drift
}

/// Diff the current files against the stored snapshot **without** updating it.
///
/// This is what the web app polls. Reporting a change must not also acknowledge
/// it: if the browser's poll consumed the drift, the terminal run that follows
/// would say nothing, and the person who never opened the browser would be the
/// one who most needed telling.
pub fn peek(root: &Path, env_files: &[String]) -> Drift {
    compare(root, &read_files(root, env_files))
}

/// The shared half of [`check`] and [`peek`]: current state against stored.
fn compare(root: &Path, current: &BTreeMap<String, Vars>) -> Drift {
    match load(root) {
        None => Drift {
            changes: Vec::new(),
            first_run: true,
        },
        Some(previous) => Drift {
            changes: diff(&previous.files, current),
            first_run: false,
        },
    }
}

/// Compare two snapshots, file by file then key by key.
fn diff(previous: &BTreeMap<String, Vars>, current: &BTreeMap<String, Vars>) -> Vec<Change> {
    let mut changes = Vec::new();

    for (file, now) in current {
        let Some(before) = previous.get(file) else {
            // A brand-new file is one line, not one line per variable in it.
            changes.push(Change::FileAdded {
                file: file.clone(),
                keys: now.len(),
            });
            continue;
        };
        for (key, hash) in now {
            match before.get(key) {
                None => changes.push(Change::Added {
                    file: file.clone(),
                    key: key.clone(),
                }),
                Some(old) if old != hash => changes.push(Change::Changed {
                    file: file.clone(),
                    key: key.clone(),
                }),
                Some(_) => {}
            }
        }
        for key in before.keys() {
            if !now.contains_key(key) {
                changes.push(Change::Removed {
                    file: file.clone(),
                    key: key.clone(),
                });
            }
        }
    }

    for file in previous.keys() {
        if !current.contains_key(file) {
            changes.push(Change::FileRemoved { file: file.clone() });
        }
    }

    changes
}

/// Read and hash every listed `.env` file that exists.
fn read_files(root: &Path, env_files: &[String]) -> BTreeMap<String, Vars> {
    let mut out: BTreeMap<String, Vars> = BTreeMap::new();
    for rel in env_files {
        let path = root.join(rel);
        let Ok(content) = std::fs::read_to_string(&path) else {
            continue;
        };
        let vars: Vars = crate::run::parse_env_content(&content)
            .into_iter()
            .map(|(key, value)| {
                let hash = hash_value(&key, &value);
                (key, hash)
            })
            .collect();
        out.insert(rel.clone(), vars);
    }
    out
}

/// Hash a value so a change is detectable without the value being recoverable.
///
/// FNV-1a over the key and the value together: keying it means two variables
/// that happen to share a value don't share a hash, so the cache leaks nothing
/// about which secrets are duplicated. This is a change detector, not a
/// password hash — it doesn't need to resist an attacker who already has the
/// plaintext to test against, and the alternative (storing values) is what it
/// exists to avoid.
fn hash_value(key: &str, value: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in key.as_bytes().iter().chain(b"=").chain(value.as_bytes()) {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x1000_0000_01b3);
    }
    format!("{hash:016x}")
}

/// Write the snapshot, creating `.ciabatta/cache/` if it isn't there.
fn store(root: &Path, snapshot: &Snapshot) -> Result<()> {
    let path = cache_path(root);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create {}", parent.display()))?;
    }
    let json = serde_json::to_string_pretty(snapshot)?;
    std::fs::write(&path, json).with_context(|| format!("Failed to write {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ciab_envcache_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_env(root: &Path, rel: &str, body: &str) {
        let path = root.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, body).unwrap();
    }

    #[test]
    fn the_first_run_records_without_reporting_drift() {
        let root = scratch("first");
        write_env(&root, ".env", "API_URL=http://localhost\nTOKEN=abc\n");

        let drift = check(&root, &[".env".to_string()]);
        assert!(drift.first_run);
        assert!(!drift.is_noteworthy());
        assert!(drift.report().is_none());
        // …but it did leave a snapshot behind.
        assert!(cache_path(&root).exists());

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn an_unchanged_file_is_silent() {
        let root = scratch("same");
        write_env(&root, ".env", "TOKEN=abc\n");
        check(&root, &[".env".to_string()]);

        let drift = check(&root, &[".env".to_string()]);
        assert!(!drift.first_run);
        assert!(drift.changes.is_empty());

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn added_changed_and_removed_variables_are_each_reported() {
        let root = scratch("drift");
        write_env(&root, ".env", "KEPT=1\nCHANGED=old\nGONE=1\n");
        check(&root, &[".env".to_string()]);

        // The shape of a git pull landing on someone's checkout.
        write_env(&root, ".env", "KEPT=1\nCHANGED=new\nBRAND_NEW=1\n");
        let drift = check(&root, &[".env".to_string()]);

        assert!(drift.is_noteworthy());
        assert!(drift.changes.contains(&Change::Added {
            file: ".env".into(),
            key: "BRAND_NEW".into()
        }));
        assert!(drift.changes.contains(&Change::Changed {
            file: ".env".into(),
            key: "CHANGED".into()
        }));
        assert!(drift.changes.contains(&Change::Removed {
            file: ".env".into(),
            key: "GONE".into()
        }));
        // An untouched variable is not mentioned.
        assert!(!drift.changes.iter().any(|c| matches!(
            c,
            Change::Changed { key, .. } if key == "KEPT"
        )));

        let report = drift.report().unwrap();
        assert!(report.contains("BRAND_NEW is new"), "{report}");
        assert!(report.contains("CHANGED changed"), "{report}");

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_new_file_is_one_change_not_one_per_variable() {
        let root = scratch("newfile");
        write_env(&root, ".env", "A=1\n");
        check(&root, &[".env".to_string()]);

        write_env(&root, "packages/api/.env", "B=1\nC=2\nD=3\n");
        let drift = check(
            &root,
            &[".env".to_string(), "packages/api/.env".to_string()],
        );
        assert_eq!(
            drift.changes,
            vec![Change::FileAdded {
                file: "packages/api/.env".into(),
                keys: 3
            }]
        );

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_deleted_file_is_reported() {
        let root = scratch("delfile");
        write_env(&root, ".env", "A=1\n");
        check(&root, &[".env".to_string()]);
        std::fs::remove_file(root.join(".env")).unwrap();

        let drift = check(&root, &[".env".to_string()]);
        assert_eq!(
            drift.changes,
            vec![Change::FileRemoved {
                file: ".env".into()
            }]
        );

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn values_are_never_written_to_the_cache() {
        let root = scratch("secrets");
        write_env(&root, ".env", "PASSWORD=hunter2\n");
        check(&root, &[".env".to_string()]);

        let stored = std::fs::read_to_string(cache_path(&root)).unwrap();
        assert!(stored.contains("PASSWORD"), "the key is what we track");
        assert!(
            !stored.contains("hunter2"),
            "the cache must not become a second copy of the secrets: {stored}"
        );

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn two_variables_sharing_a_value_do_not_share_a_hash() {
        assert_ne!(hash_value("A", "same"), hash_value("B", "same"));
        assert_eq!(hash_value("A", "same"), hash_value("A", "same"));
    }

    #[test]
    fn a_corrupt_cache_is_ignored_rather_than_fatal() {
        let root = scratch("corrupt");
        write_env(&root, ".env", "A=1\n");
        std::fs::create_dir_all(cache_path(&root).parent().unwrap()).unwrap();
        std::fs::write(cache_path(&root), "{ not json").unwrap();

        // Treated as "never seen before", and rewritten cleanly.
        let drift = check(&root, &[".env".to_string()]);
        assert!(drift.first_run);
        assert!(load(&root).is_some());

        std::fs::remove_dir_all(&root).ok();
    }
}
