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
/// keep. Kept by [`super::activity::Activity`], which persists them — they used
/// to reset with the server, which made "how is it doing" mean "how has it done
/// since Tuesday's restart".
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Counters {
    #[serde(default)]
    pub hits: u64,
    #[serde(default)]
    pub misses: u64,
    #[serde(default)]
    pub uploads: u64,
    /// Bytes served from the cache — the bandwidth a hit saved re-uploading.
    #[serde(default)]
    pub bytes_served: u64,
    #[serde(default)]
    pub bytes_stored: u64,
    /// Build time hits spared their clients: each hit adds how long the build
    /// it handed back originally took, as recorded in the entry. Saved time
    /// rather than wall-clock time — two runners hitting at once both count.
    #[serde(default)]
    pub saved_ms: u64,
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

        // An id this server hasn't seen is adopted rather than replaced, when
        // it looks like one: that is how a backup server comes to know a
        // project by the same id as the primary that first registered it, so
        // one `project:` in the config works against every server.
        let adopted = id
            .map(str::trim)
            .filter(|id| plausible_id(id))
            .map(str::to_string);
        let project = Project {
            id: adopted.unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
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

    /// Know a project a peer server knows, by its id and name, when this
    /// server doesn't already. A project it does know keeps its own name:
    /// renames follow the clients that send them, not a peer's copy, so two
    /// servers can never take turns renaming each other's.
    pub fn adopt(&self, id: &str, name: &str) -> Result<bool> {
        anyhow::ensure!(
            plausible_id(id),
            "'{id}' isn't a project id this server can use"
        );
        let name = name.trim();
        anyhow::ensure!(!name.is_empty(), "a project needs a name");
        let mut guard = self.inner.lock().unwrap();
        if guard.iter().any(|p| p.id == id) {
            return Ok(false);
        }
        guard.push(Project {
            id: id.to_string(),
            name: name.to_string(),
            created_at: crate::cache::store::now(),
            created_by: Some("a peer server".to_string()),
            last_seen_at: None,
        });
        let snapshot = guard.clone();
        drop(guard);
        save(&self.path, &snapshot)?;
        Ok(true)
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
        }
        Ok(removed)
    }
}

fn save(path: &Path, projects: &[Project]) -> Result<()> {
    let body = serde_json::to_string_pretty(projects)?;
    std::fs::write(path, body).with_context(|| format!("Failed to write {}", path.display()))
}

/// Whether a client-supplied id is fit to name a project: what this server
/// would mint itself (a UUID), or at least something as safe to put in a URL
/// path and a storage key — short, and nothing but letters, digits, `-`, `_`.
fn plausible_id(id: &str) -> bool {
    (8..=64).contains(&id.len())
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
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

    /// How a backup server comes to share the primary's id.
    #[test]
    fn an_unknown_id_is_adopted_when_it_looks_like_one() {
        let dir = scratch("adopt");
        let registry = Registry::open(&dir).unwrap();

        let id = "0b6c1f0e-5c1a-4d2b-9a59-3f1f2c7d8e90";
        let adopted = registry.resolve(Some(id), "monorepo", None).unwrap();
        assert_eq!(adopted.id, id);
        assert_eq!(registry.resolve(Some(id), "monorepo", None).unwrap().id, id);
        assert_eq!(registry.list().len(), 1);

        // Nothing that can't safely name a path or a storage key.
        for bad in ["../../etc", "short", "has space in it", &"x".repeat(65)] {
            let minted = registry.resolve(Some(bad), "other", None).unwrap();
            assert_ne!(minted.id, bad);
        }

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
}
