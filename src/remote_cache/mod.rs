//! The shared remote cache: a small server anyone can stand up, so a team's
//! builds stop repeating each other's work.
//!
//! ```text
//! ciabatta remote-cache init     write a config for a server
//! ciabatta remote-cache start    run it
//! ciabatta remote-cache login    authenticate a developer against it
//! ```
//!
//! The server is deliberately modest. It stores artifacts on its own local
//! filesystem in exactly the layout [`crate::cache::store`] uses locally, so an
//! artifact a laptop uploads is byte-identical to the one a CI runner pulls
//! back. There's no object store to provision and no database to migrate — the
//! smallest thing that could work, because a cache nobody can be bothered to
//! run is a cache nobody has.
//!
//! What it does add over the local store is the three things a *shared* cache
//! needs and a local one doesn't:
//!
//! * **Identity.** Who is asking, and are they allowed to? See [`auth`] —
//!   LDAPS against the directory a company already runs, or issued tokens.
//! * **Project identity.** A project is a name *and* a server-assigned id, and
//!   the id is what's written back into the workspace config. Names get reused
//!   and renamed; the id is what makes "the same project" mean something across
//!   checkouts and CI runners.
//! * **Distribution.** The server knows which ciabatta binaries it was pointed
//!   at, and their hashes — so a client that connects with an older build is
//!   told, and `ciabatta self update` can fetch the new one from the same place
//!   it already trusts for artifacts. See [`releases`].

pub mod auth;
pub mod client;
pub mod page;
pub mod projects;
pub mod releases;
pub mod server;
pub mod users;
pub mod workflows;

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::cache::store::Retention;

/// The config file a server reads, written by `ciabatta remote-cache init`.
pub const CONFIG_STEM: &str = "remote-cache";

/// The port the cache server listens on when nothing says otherwise.
pub const DEFAULT_PORT: u16 = 8380;

/// A remote cache server's configuration.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct ServerConfig {
    /// Where to listen, and where to keep things.
    #[serde(default)]
    pub server: Listen,

    /// When artifacts stop being worth their disk space.
    #[serde(default)]
    pub retention: Retention,

    /// Who may talk to this server, and how they prove it.
    #[serde(default)]
    pub auth: auth::AuthConfig,

    /// The ciabatta builds this server hands out, so connected clients learn
    /// about a new version and can update from here.
    #[serde(default)]
    pub releases: releases::ReleaseConfig,

    /// What the server writes to its log for each request it handles.
    #[serde(default)]
    pub log: LogConfig,

    /// How long a workflow may go unrun before this server calls it stale
    /// (`"30d"`, `"90d"`).
    ///
    /// The server's own threshold, not its clients'. A team can decide a
    /// workflow is stale after a fortnight while the cache serving five teams
    /// only wants to hear about a quarter of silence — and the server is
    /// answering a different question from any one checkout: not "have I run
    /// this lately" but "has anybody".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stale_after: Option<String>,
}

/// How much the server says about the traffic it serves.
///
/// A cache that hands back the wrong artifact, refuses a login, or 404s a key
/// somebody swears they uploaded is debugged from the outside — from what the
/// client sent and what the server answered. Both sides of that are logged by
/// default, because the alternative is asking an operator to reproduce a
/// problem they've already had.
///
/// ```yaml
/// log:
///   requests: true
///   headers:  true
/// ```
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct LogConfig {
    /// One line as each request arrives, and one as its response leaves.
    #[serde(default = "default_true")]
    pub requests: bool,

    /// Include the request's headers on the arrival line.
    ///
    /// Credential-bearing headers (`authorization`, `cookie`, `proxy-
    /// authorization`, `x-api-key`) are logged as `<redacted>` — the point is
    /// to see *that* a credential was sent and in what form, never what it was.
    /// A cache log is a file somebody will eventually paste into a ticket.
    #[serde(default = "default_true")]
    pub headers: bool,
}

impl Default for LogConfig {
    /// Hand-written so the defaults match what parsing an absent `log:` section
    /// produces, rather than the `false` a derive would give.
    fn default() -> Self {
        LogConfig {
            requests: true,
            headers: true,
        }
    }
}

fn default_true() -> bool {
    true
}

/// Where the server listens and where it puts things.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Listen {
    /// Interface to bind. Defaults to `0.0.0.0` — unlike ciabatta's local
    /// daemon, a *shared* cache that only loopback can reach is useless.
    #[serde(default = "default_bind")]
    pub bind: String,

    /// Port to listen on.
    #[serde(default = "default_port")]
    pub port: u16,

    /// Directory holding the artifact store and the project registry, relative
    /// to the config file.
    #[serde(default = "default_storage")]
    pub storage: PathBuf,

    /// How often to run the retention sweep, as a duration (`"1h"`, `"30m"`).
    #[serde(default = "default_sweep")]
    pub sweep_every: String,
}

fn default_bind() -> String {
    "0.0.0.0".to_string()
}
fn default_port() -> u16 {
    DEFAULT_PORT
}
fn default_storage() -> PathBuf {
    PathBuf::from("storage")
}
fn default_sweep() -> String {
    "1h".to_string()
}

impl Default for Listen {
    fn default() -> Self {
        Listen {
            bind: default_bind(),
            port: default_port(),
            storage: default_storage(),
            sweep_every: default_sweep(),
        }
    }
}

impl ServerConfig {
    /// The staleness threshold, as configured or defaulted.
    pub fn staleness(&self) -> std::time::Duration {
        let raw = self.staleness_raw();
        let seconds = crate::cache::store::parse_duration(&raw).unwrap_or_else(|_| {
            crate::cache::store::parse_duration(crate::run::history::DEFAULT_STALE_AFTER)
                .expect("the default is a valid duration")
        });
        std::time::Duration::from_secs(seconds.max(0) as u64)
    }

    /// The threshold as written, for saying what it is.
    pub fn staleness_raw(&self) -> String {
        self.stale_after
            .clone()
            .unwrap_or_else(|| crate::run::history::DEFAULT_STALE_AFTER.to_string())
    }

    /// Load a server config, resolving `storage` relative to the config file so
    /// a server can be started from any working directory.
    pub fn load(path: &Path) -> Result<(Self, PathBuf)> {
        let mut config: ServerConfig = crate::format::load(path)?;
        let base = path.parent().unwrap_or(Path::new("."));
        if config.server.storage.is_relative() {
            config.server.storage = base.join(&config.server.storage);
        }
        config.releases.resolve_relative(base);
        Ok((config, base.to_path_buf()))
    }

    /// Find the server config in `dir`, in either format.
    pub fn find(dir: &Path) -> Option<PathBuf> {
        crate::format::find(dir, CONFIG_STEM)
    }

    /// The socket address to bind.
    pub fn address(&self) -> String {
        format!("{}:{}", self.server.bind, self.server.port)
    }
}

/// The config `remote-cache init` writes: a working server, with every option
/// it doesn't turn on shown commented out next to it.
///
/// The commented block is not padding. Authentication and retention are the two
/// settings an operator will need within a week of standing this up, and a
/// config that doesn't mention them sends them to a web search instead of to
/// the line below the one they're already looking at.
pub fn starter_config(port: u16, storage: &str) -> String {
    format!(
        r#"# A ciabatta remote cache.
#
#   ciabatta remote-cache start          run it (add --config to point elsewhere)
#   ciabatta remote-cache login <URL>    how a developer connects to it
#
# Artifacts are stored on this machine's own filesystem, under `storage` below.

server:
  # 0.0.0.0 so the team can actually reach it. Put it behind a reverse proxy
  # with TLS for anything beyond a trusted network — this server speaks HTTP.
  bind: 0.0.0.0
  port: {port}
  storage: {storage}
  # How often to evict artifacts that breach the retention policy.
  sweep_every: 1h

# ─── Retention ─────────────────────────────────────────────────────────────────
# Age is measured from last *use*, not from when an artifact was built — the
# thing everyone still depends on shouldn't be evicted for being old. Remove a
# limit to stop enforcing it; remove all three to keep everything forever.
retention:
  max_age: 30d
  max_size: 10GB
  # max_entries: 50000

# ─── Who may connect ───────────────────────────────────────────────────────────
# mode: open   — anybody who can reach the port (a trusted network, or a demo)
#       token  — issued tokens, listed under `users` below
#       ldap   — bind against your directory over LDAPS
auth:
  mode: open

  # How long a session lasts before the client has to log in again.
  session_ttl: 30d

  # token mode: each user's token, stored as a SHA-256 so this file isn't a list
  # of credentials. Mint one with `ciabatta remote-cache add-user <name>`.
  # users:
  #   - name: ci
  #     token_sha256: "…"
  #     # Read the cache but never write to it — what a fork's CI should get.
  #     read_only: true

  # ldap mode: authenticate against the directory you already run.
  # ldap:
  #   url: ldaps://ldap.example.com:636
  #   # Where a username becomes a DN. Either bind_dn as a template…
  #   bind_dn: "uid={{username}},ou=people,dc=example,dc=com"
  #   # …or search for it, which is what you want when people live in several OUs.
  #   base_dn: "dc=example,dc=com"
  #   user_filter: "(uid={{username}})"
  #   # A service account to run that search as, when anonymous search is off.
  #   search_dn: "cn=ciabatta,ou=services,dc=example,dc=com"
  #   search_password_env: CIABATTA_LDAP_PASSWORD
  #   # Authorization: refuse anyone who isn't in this group.
  #   required_group: "cn=engineering,ou=groups,dc=example,dc=com"
  #   group_attribute: memberOf
  #   # Members of these groups may write; everyone else is read-only.
  #   # write_groups: ["cn=ci,ou=groups,dc=example,dc=com"]
  #   # Verify the directory's certificate. Turn this off only against a test
  #   # server — with it off, LDAPS is an encrypted channel to whoever answered.
  #   tls_verify: true

# ─── Handing out ciabatta itself ───────────────────────────────────────────────
# Point these at the binaries you want your team on. The server hashes them,
# tells connected clients when the hash changes, and serves them to
# `ciabatta self update` — so upgrading everyone is copying two files here.
#
# releases:
#   version: "0.2.0"
#   binaries:
#     linux: /srv/ciabatta/ciabatta-linux-x86_64
#     windows: /srv/ciabatta/ciabatta-windows-x86_64.exe
#     macos: /srv/ciabatta/ciabatta-macos-aarch64

# ─── Logging ───────────────────────────────────────────────────────────────────
# One line as each request arrives and one as its response leaves, so a cache
# miss nobody can explain can be read back off the wire. Credential-bearing
# headers are logged as <redacted>. Raise the detail with
# CIABATTA_LOG=ciabatta=debug.
log:
  requests: true
  headers: true
"#
    )
}

/// Where a client keeps its credentials: `~/.ciabatta/remote-cache.json`.
pub fn credentials_path() -> Result<PathBuf> {
    let home = home_dir().context("Could not determine your home directory (HOME is unset)")?;
    let dir = home.join(".ciabatta");
    std::fs::create_dir_all(&dir).with_context(|| format!("Failed to create {}", dir.display()))?;
    Ok(dir.join("remote-cache.json"))
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_starter_config_parses_and_is_usable_as_written() {
        let rendered = starter_config(DEFAULT_PORT, "storage");
        let config: ServerConfig = crate::format::from_str(&rendered, crate::format::Format::Yaml)
            .unwrap_or_else(|e| panic!("the starter config must parse: {e}\n\n{rendered}"));

        assert_eq!(config.server.port, DEFAULT_PORT);
        assert_eq!(config.server.bind, "0.0.0.0");
        assert_eq!(config.server.storage, PathBuf::from("storage"));
        assert_eq!(config.retention.max_age.as_deref(), Some("30d"));
        assert_eq!(config.retention.max_size.as_deref(), Some("10GB"));
        // Everything the comments describe is off until it's uncommented.
        assert!(config.auth.ldap.is_none());
        assert!(config.auth.users.is_empty());
        assert!(config.releases.binaries.is_empty());
    }

    #[test]
    fn storage_resolves_relative_to_the_config_file() {
        let dir = std::env::temp_dir().join(format!("ciab_rc_cfg_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("remote-cache.yaml");
        std::fs::write(&path, starter_config(9000, "artifacts")).unwrap();

        let (config, base) = ServerConfig::load(&path).unwrap();
        assert_eq!(base, dir);
        assert_eq!(
            config.server.storage,
            dir.join("artifacts"),
            "a server must be startable from any working directory"
        );
        assert_eq!(config.address(), "0.0.0.0:9000");
        assert_eq!(ServerConfig::find(&dir).unwrap(), path);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
