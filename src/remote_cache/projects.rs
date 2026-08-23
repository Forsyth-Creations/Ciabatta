//! What the remote cache calls a project, and why it isn't just a name.
//!
//! A cache keyed on project *name* breaks in two ordinary ways: two teams pick
//! the same name and start silently sharing artifacts, or one team renames
//! theirs and loses every entry they had. So the server assigns an id the first
//! time it sees a project, hands it back, and the client writes it into the
//! workspace config next to the name.
//!
//! From then on the id is the identity. The name is a label — it can change,
//! and the cache follows it. Two projects can even share a display name; they
//! will never share an id, and so will never share an artifact.
//!
//! The id is committed alongside the config on purpose. Every checkout of the
//! repo and every CI runner then resolves to the same project without anyone
//! configuring anything, which is the whole point.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// A project the server knows about.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Project {
    /// The identifier the server assigned. Stable for the life of the project.
    pub id: String,
    /// What to call it. Changeable, and only ever a label.
    pub name: String,
    /// RFC 3339 timestamp of first registration.
    pub created_at: String,
    /// Who registered it.
    #[serde(default)]
    pub created_by: Option<String>,
    /// When an artifact for it was last stored or served.
    #[serde(default)]
    pub last_seen_at: Option<String>,
}

/// Running counts, so the status page can say whether the cache is earning its
/// keep. Hits and misses are per-process — they reset with the server, which is
/// the honest thing for a "how is it doing right now" number.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Counters {
    pub hits: u64,
    pub misses: u64,
    pub uploads: u64,
    /// Bytes served from the cache — the bandwidth a hit saved re-uploading.
    pub bytes_served: u64,
    pub bytes_stored: u64,
}

impl Counters {
    /// Hit rate as a percentage, or `None` before anything has been asked for.
    ///
    /// This is the number that says whether the cache is working. A rate near
    /// zero usually means the keys aren't stable — an undeclared input, or a
    /// timestamp baked into a build — rather than that nothing is reusable.
    pub fn hit_rate(&self) -> Option<f64> {
        let total = self.hits + self.misses;
        (total > 0).then(|| self.hits as f64 * 100.0 / total as f64)
    }
}

/// The server's project registry, persisted next to its artifacts.
#[derive(Debug)]
pub struct Registry {
    path: PathBuf,
    inner: Mutex<Vec<Project>>,
    counters: Mutex<BTreeMap<String, Counters>>,
}

impl Registry {
    /// Open (or create) the registry under `storage`.
    pub fn open(storage: &Path) -> Result<Self> {
        std::fs::create_dir_all(storage)
            .with_context(|| format!("Failed to create {}", storage.display()))?;
        let path = storage.join("projects.json");
        let projects = std::fs::read_to_string(&path)
            .ok()
            .and_then(|raw| serde_json::from_str::<Vec<Project>>(&raw).ok())
            .unwrap_or_default();

        Ok(Registry {
            path,
            inner: Mutex::new(projects),
            counters: Mutex::new(BTreeMap::new()),
        })
    }

    /// Every project, most recently registered last.
    pub fn list(&self) -> Vec<Project> {
        self.inner.lock().unwrap().clone()
    }

    /// Look a project up by its id.
    pub fn get(&self, id: &str) -> Option<Project> {
        self.inner
            .lock()
            .unwrap()
            .iter()
            .find(|p| p.id == id)
            .cloned()
    }

    /// Resolve a project, registering it if this is the first time.
    ///
    /// A client that already knows its id sends it and gets that project back,
    /// even if the name has since changed — the id wins, because the id is the
    /// identity. A client with no id gets one minted for it.
    ///
    /// Note what deliberately does *not* happen: a client with no id whose name
    /// matches an existing project does **not** adopt that project. Names
    /// collide, and silently joining a stranger's cache is exactly the failure
    /// the id exists to prevent. It gets its own id, and the operator can see
    /// two same-named projects on the status page and sort it out.
    pub fn resolve(&self, id: Option<&str>, name: &str, by: Option<&str>) -> Result<Project> {
        let name = name.trim();
        anyhow::ensure!(!name.is_empty(), "a project needs a name");

        let mut guard = self.inner.lock().unwrap();

        if let Some(id) = id.map(str::trim).filter(|s| !s.is_empty())
            && let Some(existing) = guard.iter_mut().find(|p| p.id == id)
        {
            // Follow a rename, since the id is what identifies it.
            if existing.name != name {
                existing.name = name.to_string();
            }
            existing.last_seen_at = Some(crate::cache::store::now());
            let project = existing.clone();
            let snapshot = guard.clone();
            drop(guard);
            save(&self.path, &snapshot)?;
            return Ok(project);
        }

        let project = Project {
            id: uuid::Uuid::new_v4().to_string(),
            name: name.to_string(),
            created_at: crate::cache::store::now(),
            created_by: by.map(str::to_string),
            last_seen_at: Some(crate::cache::store::now()),
        };
        guard.push(project.clone());
        let snapshot = guard.clone();
        drop(guard);

        save(&self.path, &snapshot)?;
        Ok(project)
    }

    /// Forget a project. Its cached artifacts are removed separately, by the
    /// caller, so this can't half-delete something.
    pub fn forget(&self, id: &str) -> Result<bool> {
        let mut guard = self.inner.lock().unwrap();
        let before = guard.len();
        guard.retain(|p| p.id != id);
        let removed = guard.len() != before;
        let snapshot = guard.clone();
        drop(guard);

        if removed {
            save(&self.path, &snapshot)?;
            self.counters.lock().unwrap().remove(id);
        }
        Ok(removed)
    }

    /// Record a cache lookup that found something.
    pub fn record_hit(&self, project: &str, bytes: u64) {
        let mut guard = self.counters.lock().unwrap();
        let counters = guard.entry(project.to_string()).or_default();
        counters.hits += 1;
        counters.bytes_served += bytes;
    }

    /// Record a cache lookup that didn't.
    pub fn record_miss(&self, project: &str) {
        self.counters
            .lock()
            .unwrap()
            .entry(project.to_string())
            .or_default()
            .misses += 1;
    }

    /// Record an artifact being stored.
    pub fn record_upload(&self, project: &str, bytes: u64) {
        let mut guard = self.counters.lock().unwrap();
        let counters = guard.entry(project.to_string()).or_default();
        counters.uploads += 1;
        counters.bytes_stored += bytes;
    }

    /// One project's counters.
    pub fn counters(&self, project: &str) -> Counters {
        self.counters
            .lock()
            .unwrap()
            .get(project)
            .cloned()
            .unwrap_or_default()
    }

    /// Every project's counters, summed.
    pub fn totals(&self) -> Counters {
        self.counters
            .lock()
            .unwrap()
            .values()
            .fold(Counters::default(), |mut acc, c| {
                acc.hits += c.hits;
                acc.misses += c.misses;
                acc.uploads += c.uploads;
                acc.bytes_served += c.bytes_served;
                acc.bytes_stored += c.bytes_stored;
                acc
            })
    }
}

fn save(path: &Path, projects: &[Project]) -> Result<()> {
    let body = serde_json::to_string_pretty(projects)?;
    std::fs::write(path, body).with_context(|| format!("Failed to write {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ciab_rcproj_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn an_id_is_minted_once_and_then_honoured() {
        let dir = scratch("mint");
        let registry = Registry::open(&dir).unwrap();

        let first = registry.resolve(None, "monorepo", Some("ada")).unwrap();
        assert!(!first.id.is_empty());
        assert_eq!(first.name, "monorepo");
        assert_eq!(first.created_by.as_deref(), Some("ada"));

        // A client that already has the id gets the same project back.
        let again = registry.resolve(Some(&first.id), "monorepo", None).unwrap();
        assert_eq!(again.id, first.id);
        assert_eq!(registry.list().len(), 1);

        // The id follows a rename; the name is only a label.
        let renamed = registry
            .resolve(Some(&first.id), "the-monorepo", None)
            .unwrap();
        assert_eq!(renamed.id, first.id);
        assert_eq!(renamed.name, "the-monorepo");
        assert_eq!(registry.list().len(), 1);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The failure the id exists to prevent: two unrelated teams both calling
    /// their repo "api" must not end up sharing a cache.
    #[test]
    fn a_matching_name_alone_never_joins_an_existing_project() {
        let dir = scratch("collide");
        let registry = Registry::open(&dir).unwrap();

        let theirs = registry.resolve(None, "api", Some("team-a")).unwrap();
        let mine = registry.resolve(None, "api", Some("team-b")).unwrap();

        assert_ne!(
            theirs.id, mine.id,
            "same name must not mean same project — that's a silently shared cache"
        );
        assert_eq!(registry.list().len(), 2);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_registry_survives_a_restart() {
        let dir = scratch("persist");
        let id = {
            let registry = Registry::open(&dir).unwrap();
            registry.resolve(None, "monorepo", None).unwrap().id
        };

        let reopened = Registry::open(&dir).unwrap();
        assert_eq!(reopened.list().len(), 1);
        assert_eq!(reopened.get(&id).unwrap().name, "monorepo");

        assert!(reopened.forget(&id).unwrap());
        assert!(!reopened.forget(&id).unwrap());
        assert!(Registry::open(&dir).unwrap().list().is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_unnamed_project_is_refused() {
        let dir = scratch("unnamed");
        let registry = Registry::open(&dir).unwrap();
        assert!(registry.resolve(None, "   ", None).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn counters_track_per_project_and_in_total() {
        let dir = scratch("counters");
        let registry = Registry::open(&dir).unwrap();

        assert!(registry.counters("a").hit_rate().is_none());

        registry.record_hit("a", 1000);
        registry.record_hit("a", 500);
        registry.record_miss("a");
        registry.record_upload("a", 2000);
        registry.record_miss("b");

        let a = registry.counters("a");
        assert_eq!(a.hits, 2);
        assert_eq!(a.misses, 1);
        assert_eq!(a.uploads, 1);
        assert_eq!(a.bytes_served, 1500);
        assert_eq!(a.bytes_stored, 2000);
        assert!((a.hit_rate().unwrap() - 66.666).abs() < 0.01);

        let totals = registry.totals();
        assert_eq!(totals.hits, 2);
        assert_eq!(totals.misses, 2);
        assert_eq!(totals.hit_rate().unwrap(), 50.0);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
