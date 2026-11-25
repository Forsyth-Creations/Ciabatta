//! Secrets: values a workflow needs that no checkout should hold — API keys,
//! passwords, deploy tokens — kept by the remote cache and nowhere else.
//!
//! They belong to a project and come in **groups**, because the same name
//! usually means a different value per target: `dev` and `prod` each have
//! their own `TF_VAR_root_password`. A group is nothing more than its name;
//! setting the first secret in it makes it, removing the last forgets it.
//!
//! The cache hands values out three ways, and lists only names otherwise:
//!
//! - `ciabatta secrets use <group>` — to a writer, who puts them in their
//!   terminal's environment (or a CI job's);
//! - to the executor running an offloaded job that names the group;
//! - never to the web app, which can set and remove a secret but not read one
//!   back.
//!
//! Stored in `secrets.json` beside the cache's other state, owner-only, the way
//! [`super::users`] keeps its token hashes. They are not encrypted at rest:
//! the key would have to sit on the same disk. Whoever can read the cache's
//! storage directory can read them — so can whoever can write to an `open`
//! cache, which is the price of `open`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

/// One stored secret.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Stored {
    value: String,
    updated_at: String,
    #[serde(default)]
    updated_by: Option<String>,
}

/// project → group → name → secret.
type Table = BTreeMap<String, BTreeMap<String, BTreeMap<String, Stored>>>;

/// A secret as it may be listed: everything but the value.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Listed {
    pub name: String,
    pub updated_at: String,
    #[serde(default)]
    pub updated_by: Option<String>,
}

/// One group, as it may be listed.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Group {
    pub name: String,
    pub secrets: Vec<Listed>,
}

/// The cache's secrets, persisted next to its artifacts.
#[derive(Debug)]
pub struct Secrets {
    path: PathBuf,
    inner: Mutex<Table>,
}

/// Whether `name` may name a group: a plain word, as an env profile's is,
/// so the two can share names (`dev`, `prod`).
pub fn valid_group(name: &str) -> bool {
    crate::environment::profiles::valid_name(name)
}

/// Whether `name` may name a secret: what a shell would accept as a variable.
pub fn valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

impl Secrets {
    pub fn open(storage: &Path) -> Result<Self> {
        std::fs::create_dir_all(storage)
            .with_context(|| format!("Failed to create {}", storage.display()))?;
        let path = storage.join("secrets.json");
        let table = match std::fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str(&text)
                .with_context(|| format!("{} is not valid JSON", path.display()))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Table::new(),
            Err(e) => return Err(e).with_context(|| format!("Failed to read {}", path.display())),
        };
        Ok(Secrets {
            path,
            inner: Mutex::new(table),
        })
    }

    /// Every group a project has, by name, with its secrets' names.
    pub fn list(&self, project: &str) -> Vec<Group> {
        let inner = self.inner.lock().unwrap();
        inner
            .get(project)
            .map(|groups| {
                groups
                    .iter()
                    .map(|(name, secrets)| Group {
                        name: name.clone(),
                        secrets: secrets
                            .iter()
                            .map(|(name, s)| Listed {
                                name: name.clone(),
                                updated_at: s.updated_at.clone(),
                                updated_by: s.updated_by.clone(),
                            })
                            .collect(),
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// A group's values, by name. Empty for a group that doesn't exist — a job
    /// that names one nobody has filled in yet runs without, and its steps say
    /// what's missing.
    pub fn values(&self, project: &str, group: &str) -> BTreeMap<String, String> {
        let inner = self.inner.lock().unwrap();
        inner
            .get(project)
            .and_then(|groups| groups.get(group))
            .map(|secrets| {
                secrets
                    .iter()
                    .map(|(name, s)| (name.clone(), s.value.clone()))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Whether a project has a group of this name.
    pub fn has_group(&self, project: &str, group: &str) -> bool {
        let inner = self.inner.lock().unwrap();
        inner
            .get(project)
            .is_some_and(|groups| groups.contains_key(group))
    }

    /// Set (or replace) a secret.
    pub fn set(&self, project: &str, group: &str, name: &str, value: &str, by: &str) -> Result<()> {
        if !valid_group(group) {
            bail!("'{group}' can't name a secrets group: use letters, digits, '-' and '_'.");
        }
        if !valid_name(name) {
            bail!(
                "'{name}' can't name a secret: it becomes an environment variable, so use \
                 letters, digits and '_', not starting with a digit."
            );
        }
        let mut inner = self.inner.lock().unwrap();
        inner
            .entry(project.to_string())
            .or_default()
            .entry(group.to_string())
            .or_default()
            .insert(
                name.to_string(),
                Stored {
                    value: value.to_string(),
                    updated_at: crate::cache::store::now(),
                    updated_by: (!by.is_empty()).then(|| by.to_string()),
                },
            );
        self.save(&inner)
    }

    /// Remove a secret, and its group with it when it was the last. Whether
    /// there was one to remove.
    pub fn remove(&self, project: &str, group: &str, name: &str) -> Result<bool> {
        let mut inner = self.inner.lock().unwrap();
        let Some(groups) = inner.get_mut(project) else {
            return Ok(false);
        };
        let Some(secrets) = groups.get_mut(group) else {
            return Ok(false);
        };
        let removed = secrets.remove(name).is_some();
        if secrets.is_empty() {
            groups.remove(group);
        }
        if groups.is_empty() {
            inner.remove(project);
        }
        if removed {
            self.save(&inner)?;
        }
        Ok(removed)
    }

    /// Forget a project's secrets entirely — when the project is forgotten.
    pub fn forget_project(&self, project: &str) -> Result<()> {
        let mut inner = self.inner.lock().unwrap();
        if inner.remove(project).is_some() {
            self.save(&inner)?;
        }
        Ok(())
    }

    fn save(&self, table: &Table) -> Result<()> {
        let body = serde_json::to_vec_pretty(table)?;
        // Written owner-only from the start rather than tightened after, so
        // there's no moment the values sit in a world-readable file.
        let tmp = self.path.with_extension("json.tmp");
        write_private(&tmp, &body)?;
        std::fs::rename(&tmp, &self.path)
            .with_context(|| format!("Failed to write {}", self.path.display()))
    }
}

/// Write `body` to `path`, readable by its owner only.
pub(crate) fn write_private(path: &Path, body: &[u8]) -> Result<()> {
    use std::io::Write;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .with_context(|| format!("Failed to write {}", path.display()))?;
    file.write_all(body)
        .with_context(|| format!("Failed to write {}", path.display()))?;
    #[cfg(unix)]
    {
        // `mode` only applies to a file it creates; one left behind keeps
        // whatever it had.
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

// ─── Routes ─────────────────────────────────────────────────────────────────

use axum::{
    Json, Router,
    extract::{Path as UrlPath, State},
    http::HeaderMap,
    routing::{get, put},
};

use super::server::{ApiError, ApiResult, AppState};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/projects/{id}/secrets", get(list_groups))
        .route("/api/projects/{id}/secrets/{group}", get(group_values))
        .route(
            "/api/projects/{id}/secrets/{group}/{name}",
            put(set_secret).delete(remove_secret),
        )
}

/// The groups and their secrets' names. Anyone who may read the cache may see
/// which secrets exist; nobody reads a value through here.
async fn list_groups(
    State(state): State<AppState>,
    headers: HeaderMap,
    UrlPath(id): UrlPath<String>,
) -> ApiResult<Json<serde_json::Value>> {
    state.identify(&headers)?;
    super::server::require_project(&state, &id)?;
    Ok(Json(
        serde_json::json!({ "groups": state.secrets.list(&id) }),
    ))
}

/// A group's values — what `ciabatta secrets use` puts in a terminal. A
/// writer's only: a read-only token is what a fork's CI is given.
async fn group_values(
    State(state): State<AppState>,
    headers: HeaderMap,
    UrlPath((id, group)): UrlPath<(String, String)>,
) -> ApiResult<Json<serde_json::Value>> {
    let identity = state.identify_writer(&headers)?;
    super::server::require_project(&state, &id)?;
    if !state.secrets.has_group(&id, &group) {
        let known: Vec<String> = state
            .secrets
            .list(&id)
            .into_iter()
            .map(|g| g.name)
            .collect();
        return Err(ApiError::not_found(if known.is_empty() {
            format!(
                "This project has no secrets yet, so no group '{group}'. Add one with \
                 `ciabatta secrets set {group} <NAME>`, or on the cache's Secrets page."
            )
        } else {
            format!(
                "This project has no secrets group '{group}' (it has: {}).",
                known.join(", ")
            )
        }));
    }
    let values = state.secrets.values(&id, &group);
    tracing::info!(
        "{} read the {} secret(s) in {id}/{group}",
        identity.name,
        values.len()
    );
    Ok(Json(
        serde_json::json!({ "group": group, "secrets": values }),
    ))
}

#[derive(Deserialize)]
struct SetBody {
    value: String,
}

async fn set_secret(
    State(state): State<AppState>,
    headers: HeaderMap,
    UrlPath((id, group, name)): UrlPath<(String, String, String)>,
    Json(body): Json<SetBody>,
) -> ApiResult<Json<serde_json::Value>> {
    let identity = state.identify_writer(&headers)?;
    super::server::require_project(&state, &id)?;
    state
        .secrets
        .set(&id, &group, &name, &body.value, &identity.name)
        .map_err(ApiError::bad_request)?;
    tracing::info!("{} set secret {name} in {id}/{group}", identity.name);
    Ok(Json(serde_json::json!({ "group": group, "name": name })))
}

async fn remove_secret(
    State(state): State<AppState>,
    headers: HeaderMap,
    UrlPath((id, group, name)): UrlPath<(String, String, String)>,
) -> ApiResult<Json<serde_json::Value>> {
    let identity = state.identify_writer(&headers)?;
    super::server::require_project(&state, &id)?;
    let removed = state
        .secrets
        .remove(&id, &group, &name)
        .map_err(ApiError::bad_request)?;
    if !removed {
        return Err(ApiError::not_found(format!(
            "There is no secret {name} in group '{group}'."
        )));
    }
    tracing::info!("{} removed secret {name} from {id}/{group}", identity.name);
    Ok(Json(serde_json::json!({ "removed": true })))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ciab_secrets_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn groups_keep_their_own_values_and_survive_a_restart() {
        let dir = scratch("groups");
        let secrets = Secrets::open(&dir).unwrap();
        secrets.set("p", "dev", "TOKEN", "dev-value", "me").unwrap();
        secrets
            .set("p", "prod", "TOKEN", "prod-value", "me")
            .unwrap();
        secrets
            .set("other", "dev", "TOKEN", "elsewhere", "me")
            .unwrap();

        let reopened = Secrets::open(&dir).unwrap();
        assert_eq!(reopened.values("p", "dev")["TOKEN"], "dev-value");
        assert_eq!(reopened.values("p", "prod")["TOKEN"], "prod-value");
        assert!(reopened.values("p", "staging").is_empty());
        let groups: Vec<String> = reopened.list("p").into_iter().map(|g| g.name).collect();
        assert_eq!(groups, ["dev", "prod"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn listing_never_carries_a_value() {
        let dir = scratch("listing");
        let secrets = Secrets::open(&dir).unwrap();
        secrets.set("p", "dev", "TOKEN", "hunter2", "me").unwrap();
        let listed = serde_json::to_string(&secrets.list("p")).unwrap();
        assert!(!listed.contains("hunter2"), "{listed}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn removing_the_last_secret_forgets_the_group() {
        let dir = scratch("remove");
        let secrets = Secrets::open(&dir).unwrap();
        secrets.set("p", "dev", "A", "1", "me").unwrap();
        assert!(secrets.remove("p", "dev", "A").unwrap());
        assert!(!secrets.remove("p", "dev", "A").unwrap());
        assert!(!secrets.has_group("p", "dev"));
        assert!(secrets.list("p").is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn names_must_be_usable_as_variables() {
        let dir = scratch("names");
        let secrets = Secrets::open(&dir).unwrap();
        assert!(secrets.set("p", "dev", "1BAD", "x", "me").is_err());
        assert!(secrets.set("p", "dev", "BAD-NAME", "x", "me").is_err());
        assert!(secrets.set("p", "../dev", "OK", "x", "me").is_err());
        assert!(secrets.set("p", "dev", "_OK_2", "x", "me").is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn the_file_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = scratch("perms");
        let secrets = Secrets::open(&dir).unwrap();
        secrets.set("p", "dev", "A", "1", "me").unwrap();
        let mode = std::fs::metadata(dir.join("secrets.json"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
