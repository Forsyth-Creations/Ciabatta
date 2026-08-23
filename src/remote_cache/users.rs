//! Credentials the server manages itself.
//!
//! `auth.users` in the config file is the operator's list: they wrote it, they
//! own it, and the server never edits it. That's the right home for the
//! accounts that matter — but it means minting a credential requires editing a
//! file and restarting, which is a poor answer to "let somebody in".
//!
//! So the server also keeps its own list in `storage/users.json`, and the two
//! are merged at login. Config users win a name collision, because a file the
//! operator maintains should not be silently overridden by one the server
//! wrote.
//!
//! **Tokens are never stored.** A token is generated, shown to the caller once,
//! and only its SHA-256 is kept. Losing one means minting another; there is no
//! way to read it back, which is the property that makes this file safe to sit
//! next to the artifacts.
//!
//! ## Who may create one
//!
//! * `token` / `ldap` mode — an authenticated **admin**.
//! * `open` mode — anyone who can reach the server, because open mode already
//!   means "I trust whoever is on this network", and refusing would make the
//!   open→token migration impossible: you could never mint the first
//!   credential.
//!
//! With one hard exception: **a user created on an open server is never an
//! admin.** Otherwise somebody could mint themselves admin while the door is
//! open and keep it after the operator locks the cache down. Admin comes from
//! the operator's own config, or from an existing admin — never from the
//! network.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use super::auth::TokenUser;

/// A server-managed user, with the bookkeeping the admin page shows.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredUser {
    #[serde(flatten)]
    pub user: TokenUser,
    /// RFC 3339 timestamp of when the credential was minted.
    pub created_at: String,
    /// Who minted it.
    #[serde(default)]
    pub created_by: Option<String>,
}

/// What the API reports about a user. Deliberately not [`StoredUser`]: that
/// carries the token hash, and a hash is still a thing worth not handing out.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Summary {
    pub name: String,
    pub read_only: bool,
    pub admin: bool,
    pub created_at: String,
    pub created_by: Option<String>,
    /// Whether this one comes from the operator's config file, and so can't be
    /// revoked through the API.
    pub from_config: bool,
}

/// The server-managed user list.
#[derive(Debug)]
pub struct Users {
    path: PathBuf,
    inner: Mutex<Vec<StoredUser>>,
}

impl Users {
    /// Open (or create) the list under `storage`.
    pub fn open(storage: &Path) -> Result<Self> {
        std::fs::create_dir_all(storage)
            .with_context(|| format!("Failed to create {}", storage.display()))?;
        let path = storage.join("users.json");
        let users = std::fs::read_to_string(&path)
            .ok()
            .and_then(|raw| serde_json::from_str::<Vec<StoredUser>>(&raw).ok())
            .unwrap_or_default();

        Ok(Users {
            path,
            inner: Mutex::new(users),
        })
    }

    /// Every server-managed user, as credentials for the login check.
    pub fn credentials(&self) -> Vec<TokenUser> {
        self.inner
            .lock()
            .unwrap()
            .iter()
            .map(|stored| stored.user.clone())
            .collect()
    }

    /// Every user the server knows about, config-declared ones included.
    ///
    /// One list because that's the question an operator is asking — "who can
    /// get in?" — and answering it from two places is how a stale account goes
    /// unnoticed.
    pub fn summaries(&self, from_config: &[TokenUser]) -> Vec<Summary> {
        let mut out: Vec<Summary> = from_config
            .iter()
            .map(|user| Summary {
                name: user.name.clone(),
                read_only: user.read_only,
                admin: user.admin,
                created_at: String::new(),
                created_by: None,
                from_config: true,
            })
            .collect();

        for stored in self.inner.lock().unwrap().iter() {
            // A config entry of the same name is the one that wins at login,
            // so it's the one to report.
            if out.iter().any(|s| s.name == stored.user.name) {
                continue;
            }
            out.push(Summary {
                name: stored.user.name.clone(),
                read_only: stored.user.read_only,
                admin: stored.user.admin,
                created_at: stored.created_at.clone(),
                created_by: stored.created_by.clone(),
                from_config: false,
            });
        }

        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }

    /// Mint a credential, returning the token — the only time it exists in
    /// readable form.
    pub fn create(
        &self,
        name: &str,
        read_only: bool,
        admin: bool,
        by: Option<&str>,
        from_config: &[TokenUser],
    ) -> Result<(String, Summary)> {
        let name = name.trim();
        anyhow::ensure!(!name.is_empty(), "a user needs a name");
        anyhow::ensure!(
            name.chars()
                .all(|c| c.is_alphanumeric() || matches!(c, '-' | '_' | '.' | '@')),
            "'{name}' has characters a username shouldn't: use letters, digits, \
             and - _ . @"
        );

        if from_config.iter().any(|u| u.name == name) {
            bail!(
                "'{name}' is declared in the server's config file. Edit it there, \
                 or pick a different name."
            );
        }

        let mut guard = self.inner.lock().unwrap();
        if guard.iter().any(|u| u.user.name == name) {
            bail!("'{name}' already exists. Revoke it first to issue a new token.");
        }

        let token = super::auth::generate_token();
        let stored = StoredUser {
            user: TokenUser {
                name: name.to_string(),
                token_sha256: crate::cache::hash_bytes(token.as_bytes()),
                read_only,
                admin,
            },
            created_at: crate::cache::store::now(),
            created_by: by.map(str::to_string),
        };

        guard.push(stored.clone());
        let snapshot = guard.clone();
        drop(guard);
        save(&self.path, &snapshot)?;

        Ok((
            token,
            Summary {
                name: stored.user.name,
                read_only: stored.user.read_only,
                admin: stored.user.admin,
                created_at: stored.created_at,
                created_by: stored.created_by,
                from_config: false,
            },
        ))
    }

    /// Revoke a server-managed user.
    ///
    /// A config-declared user can't be revoked here: the server doesn't own
    /// that file, and quietly rewriting an operator's config is not something a
    /// button on a web page should do.
    pub fn remove(&self, name: &str, from_config: &[TokenUser]) -> Result<bool> {
        if from_config.iter().any(|u| u.name == name) {
            bail!(
                "'{name}' is declared in the server's config file — remove it from \
                 `auth.users` there and restart."
            );
        }

        let mut guard = self.inner.lock().unwrap();
        let before = guard.len();
        guard.retain(|u| u.user.name != name);
        let removed = guard.len() != before;
        let snapshot = guard.clone();
        drop(guard);

        if removed {
            save(&self.path, &snapshot)?;
        }
        Ok(removed)
    }

    /// Whether any credential exists at all, from either source.
    ///
    /// A `token`-mode server with none can't be logged into, so the admin page
    /// says so rather than showing an empty list and a login form that will
    /// never accept anything.
    pub fn is_empty(&self, from_config: &[TokenUser]) -> bool {
        from_config.is_empty() && self.inner.lock().unwrap().is_empty()
    }
}

/// Write the list back with owner-only permissions — it holds token hashes.
fn save(path: &Path, users: &[StoredUser]) -> Result<()> {
    let body = serde_json::to_string_pretty(users)?;
    std::fs::write(path, body).with_context(|| format!("Failed to write {}", path.display()))?;
    restrict_permissions(path);
    Ok(())
}

#[cfg(unix)]
fn restrict_permissions(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
}

#[cfg(not(unix))]
fn restrict_permissions(_path: &Path) {}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ciab_users_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn config_user(name: &str) -> TokenUser {
        TokenUser {
            name: name.to_string(),
            token_sha256: crate::cache::hash_bytes(b"from-the-config"),
            read_only: false,
            admin: true,
        }
    }

    #[test]
    fn a_minted_token_is_shown_once_and_only_its_hash_is_kept() {
        let dir = scratch("mint");
        let users = Users::open(&dir).unwrap();

        let (token, summary) = users
            .create("ada", false, false, Some("root"), &[])
            .unwrap();
        assert_eq!(summary.name, "ada");
        assert_eq!(summary.created_by.as_deref(), Some("root"));
        assert!(!summary.from_config);

        // The credential the login check sees carries the hash, not the token.
        let credentials = users.credentials();
        assert_eq!(credentials.len(), 1);
        assert_eq!(
            credentials[0].token_sha256,
            crate::cache::hash_bytes(token.as_bytes())
        );

        // And the token appears nowhere on disk.
        let raw = std::fs::read_to_string(dir.join("users.json")).unwrap();
        assert!(
            !raw.contains(&token),
            "the token must never be written down: {raw}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn names_are_unique_and_reasonable() {
        let dir = scratch("names");
        let users = Users::open(&dir).unwrap();

        users.create("ada", false, false, None, &[]).unwrap();
        let err = users
            .create("ada", false, false, None, &[])
            .unwrap_err()
            .to_string();
        assert!(err.contains("already exists"), "got: {err}");

        assert!(users.create("  ", false, false, None, &[]).is_err());
        assert!(
            users
                .create("ada lovelace", false, false, None, &[])
                .is_err()
        );
        assert!(
            users
                .create("ada/../root", false, false, None, &[])
                .is_err()
        );
        // The ordinary shapes a username takes are all fine.
        assert!(users.create("ci-runner_2", false, false, None, &[]).is_ok());
        assert!(
            users
                .create("ada@example.com", false, false, None, &[])
                .is_ok()
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The config file is the operator's. The server reports what's in it, and
    /// refuses to shadow or rewrite it from a web request.
    #[test]
    fn config_users_are_reported_but_never_edited() {
        let dir = scratch("config");
        let users = Users::open(&dir).unwrap();
        let from_config = vec![config_user("root")];

        users
            .create("ada", true, false, None, &from_config)
            .unwrap();

        let summaries = users.summaries(&from_config);
        assert_eq!(summaries.len(), 2);
        let root = summaries.iter().find(|s| s.name == "root").unwrap();
        assert!(root.from_config);
        assert!(root.admin);
        let ada = summaries.iter().find(|s| s.name == "ada").unwrap();
        assert!(!ada.from_config);
        assert!(ada.read_only);

        // Neither shadowing…
        let err = users
            .create("root", false, false, None, &from_config)
            .unwrap_err()
            .to_string();
        assert!(err.contains("config file"), "got: {err}");

        // …nor revoking through the API.
        let err = users.remove("root", &from_config).unwrap_err().to_string();
        assert!(err.contains("config file"), "got: {err}");

        // A server-managed one revokes fine.
        assert!(users.remove("ada", &from_config).unwrap());
        assert!(!users.remove("ada", &from_config).unwrap());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_list_survives_a_restart() {
        let dir = scratch("persist");
        let token = {
            let users = Users::open(&dir).unwrap();
            users.create("ada", false, true, None, &[]).unwrap().0
        };

        let reopened = Users::open(&dir).unwrap();
        let credentials = reopened.credentials();
        assert_eq!(credentials.len(), 1);
        assert!(credentials[0].admin);
        assert_eq!(
            credentials[0].token_sha256,
            crate::cache::hash_bytes(token.as_bytes())
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn emptiness_accounts_for_both_sources() {
        let dir = scratch("empty");
        let users = Users::open(&dir).unwrap();

        assert!(users.is_empty(&[]));
        assert!(!users.is_empty(&[config_user("root")]));

        users.create("ada", false, false, None, &[]).unwrap();
        assert!(!users.is_empty(&[]));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
