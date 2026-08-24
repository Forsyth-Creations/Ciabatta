//! Who may talk to the remote cache, and how they prove it.
//!
//! Three modes, in increasing order of how much a company will want them:
//!
//! * `open` — anyone who can reach the port. Fine on a trusted network, fine
//!   for a demo, and the default so that standing a server up takes one command
//!   rather than a directory integration.
//! * `token` — issued tokens, stored as SHA-256 hashes so the config file isn't
//!   a list of credentials.
//! * `ldap` — bind against the directory the company already runs, over LDAPS.
//!   Group membership decides who gets in and who may write.
//!
//! A note on what authentication is *for* here. A cache is not a secret store,
//! but it is an execution surface: whoever can write to it decides what
//! everyone else's build produces. Read access is a convenience; write access
//! is trust. That's why `read_only` exists on both a token user and an LDAP
//! group — a fork's CI should benefit from the cache without being able to
//! poison it.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

/// How a server decides who's asking.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AuthConfig {
    /// `open`, `token`, or `ldap`.
    #[serde(default = "default_mode")]
    pub mode: String,

    /// How long an issued session lasts (`"30d"`, `"12h"`).
    #[serde(default = "default_ttl")]
    pub session_ttl: String,

    /// Token-mode users.
    #[serde(default)]
    pub users: Vec<TokenUser>,

    /// LDAP settings, when `mode: ldap`.
    pub ldap: Option<LdapConfig>,
}

fn default_mode() -> String {
    "open".to_string()
}
fn default_ttl() -> String {
    "30d".to_string()
}

impl Default for AuthConfig {
    fn default() -> Self {
        AuthConfig {
            mode: default_mode(),
            session_ttl: default_ttl(),
            users: Vec::new(),
            ldap: None,
        }
    }
}

/// The three ways a server can be configured to authenticate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Open,
    Token,
    Ldap,
}

impl AuthConfig {
    /// Parse and validate the mode, checking the settings it needs are present.
    ///
    /// Validated at startup rather than at first login: a server configured for
    /// LDAP with no `ldap:` block should refuse to start, not accept requests
    /// for a week and then fail the first person who tries to log in.
    pub fn mode(&self) -> Result<Mode> {
        match self.mode.trim().to_lowercase().as_str() {
            "open" | "none" => Ok(Mode::Open),
            "token" => {
                if self.users.is_empty() {
                    bail!(
                        "auth.mode is 'token' but no users are configured. \
                         Add one with `ciabatta remote-cache add-user <name>`, \
                         or set auth.mode to 'open'."
                    );
                }
                Ok(Mode::Token)
            }
            "ldap" | "ldaps" => {
                let ldap = self.ldap.as_ref().context(
                    "auth.mode is 'ldap' but there is no `auth.ldap` section to say which \
                     directory to bind against",
                )?;
                ldap.validate()?;
                Ok(Mode::Ldap)
            }
            other => bail!("Unknown auth.mode '{other}' (expected: open, token, or ldap)"),
        }
    }

    /// The session lifetime in seconds.
    pub fn session_seconds(&self) -> Result<i64> {
        crate::cache::store::parse_duration(&self.session_ttl)
    }
}

/// One token-mode user.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TokenUser {
    pub name: String,
    /// SHA-256 of the user's token, hex encoded. The token itself is shown once,
    /// when it's minted, and never stored.
    pub token_sha256: String,
    /// May read the cache but not write to it.
    #[serde(default)]
    pub read_only: bool,
    /// May manage other users.
    ///
    /// Only ever granted by the operator's own config, or by an existing
    /// admin — never by a request to an `open` server, or somebody could mint
    /// themselves lasting control while the door was open. See
    /// [`crate::remote_cache::users`].
    #[serde(default)]
    pub admin: bool,
}

/// How to reach and interrogate an LDAP directory.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct LdapConfig {
    /// `ldaps://host:636` (or `ldap://` for a plaintext test server).
    pub url: String,

    /// A DN template with `{username}` in it, when every user lives under one
    /// branch. The cheapest configuration, and the one that stops working the
    /// moment somebody is in a different OU.
    pub bind_dn: Option<String>,

    /// Where to search for a user's DN, when `bind_dn` won't do.
    pub base_dn: Option<String>,

    /// The filter to find them by, with `{username}` substituted.
    #[serde(default = "default_user_filter")]
    pub user_filter: String,

    /// A service account to run the search as, when the directory doesn't allow
    /// anonymous search.
    pub search_dn: Option<String>,

    /// The environment variable holding that service account's password. A
    /// password in an environment variable is not wonderful; a password in a
    /// config file checked into git is worse.
    pub search_password_env: Option<String>,

    /// Refuse anyone who isn't a member of this group.
    pub required_group: Option<String>,

    /// The attribute listing a user's groups.
    #[serde(default = "default_group_attribute")]
    pub group_attribute: String,

    /// Members of these groups may write to the cache; everyone else who gets
    /// in is read-only. Empty means everyone who authenticates may write.
    #[serde(default)]
    pub write_groups: Vec<String>,

    /// Verify the directory's TLS certificate.
    ///
    /// Defaults to on, and should stay on. LDAPS with verification off is an
    /// encrypted channel to whoever answered the connection — which, for a
    /// protocol whose entire job is to say who somebody is, is worse than
    /// useless.
    #[serde(default = "default_true")]
    pub tls_verify: bool,

    /// How long to wait on the directory before giving up, in seconds.
    #[serde(default = "default_timeout")]
    pub timeout: u64,
}

fn default_user_filter() -> String {
    "(uid={username})".to_string()
}
fn default_group_attribute() -> String {
    "memberOf".to_string()
}
fn default_true() -> bool {
    true
}
fn default_timeout() -> u64 {
    10
}

impl LdapConfig {
    /// Check the settings hang together before the server accepts traffic.
    pub fn validate(&self) -> Result<()> {
        if self.url.trim().is_empty() {
            bail!(
                "auth.ldap.url is empty — set it to your directory, e.g. ldaps://ldap.example.com:636"
            );
        }
        if self.bind_dn.is_none() && self.base_dn.is_none() {
            bail!(
                "auth.ldap needs either `bind_dn` (a DN template containing {{username}}) or \
                 `base_dn` (where to search for the user). Set one."
            );
        }
        if let Some(template) = &self.bind_dn
            && !template.contains("{username}")
        {
            bail!(
                "auth.ldap.bind_dn must contain {{username}} — it's a template, \
                 e.g. \"uid={{username}},ou=people,dc=example,dc=com\""
            );
        }
        if self.search_dn.is_some() && self.search_password_env.is_none() {
            bail!(
                "auth.ldap.search_dn is set but auth.ldap.search_password_env isn't, \
                 so there's no way to authenticate the search."
            );
        }
        if !self.tls_verify {
            tracing::warn!(
                "auth.ldap.tls_verify is off: the connection is encrypted but the \
                 directory's identity is not checked. Use this only against a test server."
            );
        }
        Ok(())
    }

    /// Substitute `{username}` into a template, escaping it for the context.
    fn render(&self, template: &str, username: &str) -> String {
        template.replace("{username}", &escape_filter(username))
    }
}

/// Escape a value for safe interpolation into an LDAP DN or filter (RFC 4515).
///
/// Without this, a username containing `)` or `*` rewrites the filter it's
/// substituted into — the LDAP equivalent of SQL injection, and just as easy to
/// do by accident.
pub fn escape_filter(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '*' => out.push_str("\\2a"),
            '(' => out.push_str("\\28"),
            ')' => out.push_str("\\29"),
            '\\' => out.push_str("\\5c"),
            '\0' => out.push_str("\\00"),
            '/' => out.push_str("\\2f"),
            ',' | '+' | '"' | '<' | '>' | ';' | '=' => {
                out.push('\\');
                out.push(ch);
            }
            _ => out.push(ch),
        }
    }
    out
}

// ─── Who somebody turned out to be ──────────────────────────────────────────

/// An authenticated caller.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Identity {
    /// The username they logged in as.
    pub name: String,
    /// Whether they may write to the cache.
    pub can_write: bool,
    /// Whether they may manage users.
    #[serde(default)]
    pub is_admin: bool,
    /// Groups the directory reported, for the record and for the web view.
    #[serde(default)]
    pub groups: Vec<String>,
}

impl Identity {
    /// The identity an `open`-mode server hands everybody.
    ///
    /// Not an admin. An open server lets anyone manage users — see
    /// [`crate::remote_cache::users`] for why — but that permission comes from
    /// the mode, not from this identity, so it evaporates the moment the
    /// operator turns authentication on.
    pub fn anonymous() -> Self {
        Identity {
            name: "anonymous".to_string(),
            can_write: true,
            is_admin: false,
            groups: Vec::new(),
        }
    }
}

/// One issued session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    /// SHA-256 of the bearer token. The token is returned to the client once
    /// and never stored, so a leaked session file isn't a leaked credential.
    pub token_sha256: String,
    pub identity: Identity,
    pub issued_at: String,
    pub expires_at: String,
}

impl Session {
    /// Whether this session is still valid.
    pub fn is_live(&self) -> bool {
        chrono::DateTime::parse_from_rfc3339(&self.expires_at)
            .map(|expiry| expiry.with_timezone(&chrono::Utc) > chrono::Utc::now())
            .unwrap_or(false)
    }
}

/// The sessions a server has issued, surviving its restarts.
///
/// **No token is stored.** A session is keyed by the SHA-256 of its bearer
/// token and holds nothing else that could be replayed — exactly the property
/// that makes `users.json` safe to keep next to the artifacts, and the reason
/// this file is safe too. Somebody who reads it learns who is logged in and
/// when their session lapses; they cannot log in as any of them.
///
/// That's what makes durability the right default. A remote cache is restarted
/// for the reasons any server is — a config edit, a new binary to hand out, a
/// host reboot — and signing out every developer and every CI runner each time
/// turns a thirty-second restart into a morning of `remote-cache login`. Worse,
/// it does it silently: the next build just starts missing the cache.
///
/// Expired sessions are dropped when the file is loaded and whenever one is
/// touched, so the file cannot grow without bound.
///
/// Persistence is best-effort in one direction only: failing to *write* costs
/// the sessions issued since the last successful write and is logged, but never
/// fails a login. Failing to *read* — a truncated or hand-edited file — starts
/// empty, which is the old behaviour and asks people to log in again rather
/// than guessing at what the file meant.
#[derive(Debug, Default)]
pub struct Sessions {
    /// Where the table is kept, or `None` for a purely in-memory table (tests,
    /// and any future embedded use).
    path: Option<PathBuf>,
    inner: Mutex<HashMap<String, Session>>,
}

impl Sessions {
    /// Open (or create) the durable session table under `storage`.
    ///
    /// `Sessions::default()` is the in-memory table that does not outlive the
    /// process — what a test wants, and nothing a server should use.
    ///
    /// Never fails: a cache whose sessions file can't be read is a cache whose
    /// users log in again, not a cache that won't start.
    pub fn open(storage: &Path) -> Self {
        let path = storage.join("sessions.json");
        let mut loaded: HashMap<String, Session> = std::fs::read_to_string(&path)
            .ok()
            .and_then(|raw| serde_json::from_str::<Vec<Session>>(&raw).ok())
            .map(|sessions| {
                sessions
                    .into_iter()
                    .map(|session| (session.token_sha256.clone(), session))
                    .collect()
            })
            .unwrap_or_default();

        let before = loaded.len();
        loaded.retain(|_, session| session.is_live());
        if before > 0 {
            tracing::info!(
                "restored {} live session(s) from {} ({} expired)",
                loaded.len(),
                path.display(),
                before - loaded.len()
            );
        }

        Sessions {
            path: Some(path),
            inner: Mutex::new(loaded),
        }
    }

    /// Issue a session for `identity`, returning the bearer token to hand back.
    pub fn issue(&self, identity: Identity, ttl_seconds: i64) -> (String, Session) {
        let token = generate_token();
        let now = chrono::Utc::now();
        let session = Session {
            token_sha256: crate::cache::hash_bytes(token.as_bytes()),
            identity,
            issued_at: now.to_rfc3339(),
            expires_at: (now + chrono::Duration::seconds(ttl_seconds)).to_rfc3339(),
        };
        {
            let mut guard = self.inner.lock().unwrap();
            guard.insert(session.token_sha256.clone(), session.clone());
            self.save(&guard);
        }
        (token, session)
    }

    /// Look a bearer token up, saying *why* it didn't work.
    ///
    /// The distinction matters to whoever is reading the error. "Expired" is
    /// something the client can see coming and the server can date; "unknown"
    /// means this cache has no record of the session at all — it was revoked,
    /// or it was issued by a different server, or (before sessions were
    /// persisted) the server has restarted since. Answering "expired" to all
    /// three sends people looking at a clock when the clock is fine.
    pub fn lookup(&self, token: &str) -> Lookup {
        let hash = crate::cache::hash_bytes(token.as_bytes());
        let mut guard = self.inner.lock().unwrap();
        match guard.get(&hash) {
            Some(session) if session.is_live() => Lookup::Live(session.identity.clone()),
            Some(session) => {
                let expired_at = session.expires_at.clone();
                guard.remove(&hash);
                self.save(&guard);
                Lookup::Expired { at: expired_at }
            }
            None => Lookup::Unknown,
        }
    }

    /// End a session.
    pub fn revoke(&self, token: &str) -> bool {
        let hash = crate::cache::hash_bytes(token.as_bytes());
        let mut guard = self.inner.lock().unwrap();
        let removed = guard.remove(&hash).is_some();
        if removed {
            // A logout that a restart undoes is not a logout.
            self.save(&guard);
        }
        removed
    }

    /// Drop every session whose time is up, and say how many went.
    ///
    /// Called on the server's retention sweep, alongside the artifact eviction
    /// it already does. Expiry is deliberately *not* enforced by pruning on
    /// every write: a session that has lapsed but is still in the table is what
    /// lets [`Self::lookup`] answer "it expired at 3pm on Tuesday" rather than
    /// "never heard of it", and that is by far the more useful of the two to
    /// somebody whose credential looks perfectly fine from their side.
    pub fn prune_expired(&self) -> usize {
        let mut guard = self.inner.lock().unwrap();
        let before = guard.len();
        guard.retain(|_, session| session.is_live());
        let removed = before - guard.len();
        if removed > 0 {
            self.save(&guard);
        }
        removed
    }

    /// How many sessions are live, for the status page.
    pub fn live_count(&self) -> usize {
        self.inner
            .lock()
            .unwrap()
            .values()
            .filter(|s| s.is_live())
            .count()
    }

    /// Write the table out, if this one is durable.
    ///
    /// Takes the guard the caller already holds, so the file can never be
    /// written from a snapshot that a concurrent login has already moved past.
    fn save(&self, sessions: &HashMap<String, Session>) {
        let Some(path) = &self.path else { return };
        let mut ordered: Vec<&Session> = sessions.values().collect();
        // Sorted so a restart, a diff, or an operator reading the file sees a
        // stable order rather than the hash map's.
        ordered.sort_by(|a, b| a.issued_at.cmp(&b.issued_at));

        if let Err(e) = write_sessions(path, &ordered) {
            tracing::warn!(
                "couldn't persist sessions to {} ({e:#}) — everyone stays signed in \
                 until this server restarts",
                path.display()
            );
        }
    }
}

fn write_sessions(path: &Path, sessions: &[&Session]) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create {}", parent.display()))?;
    }
    let body = serde_json::to_string_pretty(sessions)?;
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

/// What a bearer token turned out to be.
#[derive(Debug, Clone, PartialEq)]
pub enum Lookup {
    /// A live session, and who it belongs to.
    Live(Identity),
    /// A session this server issued, whose time is up.
    Expired {
        /// When it lapsed, RFC 3339 — so the error can say so rather than
        /// leaving somebody to guess whether it was minutes or weeks ago.
        at: String,
    },
    /// No record of it at all.
    Unknown,
}

/// A fresh opaque token.
pub fn generate_token() -> String {
    use rand::Rng;
    let mut rng = rand::thread_rng();
    (0..48)
        .map(|_| {
            const ALPHABET: &[u8] =
                b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
            ALPHABET[rng.gen_range(0..ALPHABET.len())] as char
        })
        .collect()
}

// ─── Authenticating ─────────────────────────────────────────────────────────

/// Check a username and password against the configured backend.
///
/// `stored` is the server-managed user list, merged with the config's own for
/// the token check — see [`crate::remote_cache::users`].
pub async fn authenticate(
    config: &AuthConfig,
    stored: &[TokenUser],
    username: &str,
    password: &str,
) -> Result<Identity> {
    match config.mode()? {
        Mode::Open => Ok(Identity {
            name: if username.trim().is_empty() {
                "anonymous".to_string()
            } else {
                username.to_string()
            },
            can_write: true,
            is_admin: false,
            groups: Vec::new(),
        }),
        Mode::Token => authenticate_token(config, stored, username, password),
        Mode::Ldap => {
            let ldap = config.ldap.as_ref().expect("validated by mode()");
            authenticate_ldap(ldap, username, password).await
        }
    }
}

/// Token mode: the "password" is the token itself.
///
/// Config users are checked first, so an operator's entry wins a name
/// collision with one the server minted.
fn authenticate_token(
    config: &AuthConfig,
    stored: &[TokenUser],
    username: &str,
    token: &str,
) -> Result<Identity> {
    let hash = crate::cache::hash_bytes(token.as_bytes());
    let user = config
        .users
        .iter()
        .chain(stored.iter())
        .find(|u| u.name == username && constant_time_eq(&u.token_sha256, &hash));

    match user {
        Some(user) => Ok(Identity {
            name: user.name.clone(),
            can_write: !user.read_only,
            is_admin: user.admin,
            groups: Vec::new(),
        }),
        // Deliberately identical whether the user or the token was wrong.
        None => bail!("Invalid username or token"),
    }
}

/// Compare two hex hashes without leaking which byte differed via timing.
fn constant_time_eq(a: &str, b: &str) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.bytes()
        .zip(b.bytes())
        .fold(0u8, |acc, (x, y)| acc | (x ^ y))
        == 0
}

/// LDAP mode: bind as the user, then read their groups.
///
/// The bind *is* the authentication — if the directory accepts the password,
/// the password is right, and ciabatta never sees or stores a credential beyond
/// the moment it passes it along. Authorization is a second step: group
/// membership decides whether they're allowed in at all, and whether they may
/// write.
pub async fn authenticate_ldap(
    config: &LdapConfig,
    username: &str,
    password: &str,
) -> Result<Identity> {
    use ldap3::{LdapConnAsync, LdapConnSettings, Scope, SearchEntry};

    anyhow::ensure!(
        !password.is_empty(),
        "An empty password is never valid — most directories accept it as an \
         anonymous bind, which would let anyone in as anyone."
    );

    let settings = LdapConnSettings::new()
        .set_conn_timeout(std::time::Duration::from_secs(config.timeout))
        .set_no_tls_verify(!config.tls_verify);

    // Resolve the user's DN: either straight from the template, or by searching
    // for it as the service account.
    let user_dn = match &config.bind_dn {
        Some(template) => config.render(template, username),
        None => {
            let base = config
                .base_dn
                .as_deref()
                .expect("validated: one of bind_dn/base_dn is set");
            let filter = config.render(&config.user_filter, username);

            let (conn, mut ldap) = LdapConnAsync::with_settings(settings.clone(), &config.url)
                .await
                .with_context(|| format!("Failed to reach the directory at {}", config.url))?;
            ldap3::drive!(conn);

            if let (Some(dn), Some(var)) = (&config.search_dn, &config.search_password_env) {
                let secret = std::env::var(var).with_context(|| {
                    format!(
                        "auth.ldap.search_password_env names {var}, but it isn't set in the \
                         server's environment"
                    )
                })?;
                ldap.simple_bind(dn, &secret).await?.success().context(
                    "The LDAP service account could not bind — check search_dn and its password",
                )?;
            }

            let (entries, _) = ldap
                .search(base, Scope::Subtree, &filter, vec!["dn"])
                .await
                .with_context(|| format!("LDAP search failed under {base}"))?
                .success()
                .with_context(|| format!("LDAP search failed under {base}"))?;
            let _ = ldap.unbind().await;

            let entry = entries
                .into_iter()
                .next()
                .context("Invalid username or password")?;
            SearchEntry::construct(entry).dn
        }
    };

    // The bind: this is the actual password check.
    let (conn, mut ldap) = LdapConnAsync::with_settings(settings, &config.url)
        .await
        .with_context(|| format!("Failed to reach the directory at {}", config.url))?;
    ldap3::drive!(conn);

    ldap.simple_bind(&user_dn, password)
        .await?
        .success()
        // Never distinguish "no such user" from "wrong password".
        .map_err(|_| anyhow::anyhow!("Invalid username or password"))?;

    // Now that we're bound as them, read their groups for authorization.
    let groups = read_groups(&mut ldap, &user_dn, &config.group_attribute)
        .await
        .unwrap_or_default();
    let _ = ldap.unbind().await;

    if let Some(required) = &config.required_group
        && !groups.iter().any(|g| g.eq_ignore_ascii_case(required))
    {
        bail!("{username} authenticated, but is not a member of {required}");
    }

    let can_write = config.write_groups.is_empty()
        || config
            .write_groups
            .iter()
            .any(|want| groups.iter().any(|g| g.eq_ignore_ascii_case(want)));

    Ok(Identity {
        name: username.to_string(),
        can_write,
        // LDAP group membership decides read/write; user management stays with
        // the config's own accounts, so a directory can't hand out admin.
        is_admin: false,
        groups,
    })
}

/// Read a user's group memberships from their own entry.
async fn read_groups(
    ldap: &mut ldap3::Ldap,
    user_dn: &str,
    attribute: &str,
) -> Result<Vec<String>> {
    use ldap3::{Scope, SearchEntry};

    let (entries, _) = ldap
        .search(user_dn, Scope::Base, "(objectClass=*)", vec![attribute])
        .await?
        .success()?;

    Ok(entries
        .into_iter()
        .flat_map(|e| {
            SearchEntry::construct(e)
                .attrs
                .remove(attribute)
                .unwrap_or_default()
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_mode_is_validated_before_the_server_accepts_traffic() {
        let open = AuthConfig::default();
        assert_eq!(open.mode().unwrap(), Mode::Open);

        // Token mode with nobody to let in would silently refuse everyone.
        let empty_token = AuthConfig {
            mode: "token".into(),
            ..Default::default()
        };
        let err = empty_token.mode().unwrap_err().to_string();
        assert!(err.contains("no users are configured"), "got: {err}");

        let with_user = AuthConfig {
            mode: "token".into(),
            users: vec![TokenUser {
                name: "ci".into(),
                token_sha256: "abc".into(),
                read_only: false,
                admin: false,
            }],
            ..Default::default()
        };
        assert_eq!(with_user.mode().unwrap(), Mode::Token);

        // LDAP mode with no ldap section must fail at startup, not at first login.
        let ldap_no_config = AuthConfig {
            mode: "ldap".into(),
            ..Default::default()
        };
        let err = ldap_no_config.mode().unwrap_err().to_string();
        assert!(err.contains("auth.ldap"), "got: {err}");

        let err = AuthConfig {
            mode: "kerberos".into(),
            ..Default::default()
        }
        .mode()
        .unwrap_err()
        .to_string();
        assert!(err.contains("Unknown auth.mode"), "got: {err}");
    }

    #[test]
    fn ldap_settings_are_checked_for_the_mistakes_people_actually_make() {
        let base = LdapConfig {
            url: "ldaps://ldap.example.com:636".into(),
            bind_dn: None,
            base_dn: None,
            user_filter: default_user_filter(),
            search_dn: None,
            search_password_env: None,
            required_group: None,
            group_attribute: default_group_attribute(),
            write_groups: vec![],
            tls_verify: true,
            timeout: 10,
        };

        // Neither way of finding the user.
        assert!(base.validate().unwrap_err().to_string().contains("bind_dn"));

        // A bind_dn that forgot the placeholder would bind as one fixed user
        // for everybody — the worst possible outcome, so it's an error.
        let mut fixed = base.clone();
        fixed.bind_dn = Some("uid=admin,ou=people,dc=example,dc=com".into());
        assert!(
            fixed
                .validate()
                .unwrap_err()
                .to_string()
                .contains("{username}")
        );

        let mut good = base.clone();
        good.bind_dn = Some("uid={username},ou=people,dc=example,dc=com".into());
        assert!(good.validate().is_ok());

        // A search account with no way to get its password.
        let mut half = good.clone();
        half.search_dn = Some("cn=svc,dc=example,dc=com".into());
        assert!(
            half.validate()
                .unwrap_err()
                .to_string()
                .contains("search_password_env")
        );

        let mut empty_url = good.clone();
        empty_url.url = "  ".into();
        assert!(
            empty_url
                .validate()
                .unwrap_err()
                .to_string()
                .contains("url")
        );
    }

    /// A username is attacker-controlled text that gets interpolated into a
    /// filter. Without escaping, `*` alone logs in as the first user found.
    #[test]
    fn usernames_cannot_rewrite_the_ldap_filter() {
        assert_eq!(escape_filter("ada"), "ada");
        assert_eq!(escape_filter("*"), "\\2a");
        assert_eq!(
            escape_filter("ada)(uid=*"),
            "ada\\29\\28uid\\3d\\2a".replace("\\3d", "\\=")
        );
        assert_eq!(escape_filter("a\\b"), "a\\5cb");

        let config = LdapConfig {
            url: "ldaps://x".into(),
            bind_dn: Some("uid={username},ou=people,dc=example,dc=com".into()),
            base_dn: None,
            user_filter: default_user_filter(),
            search_dn: None,
            search_password_env: None,
            required_group: None,
            group_attribute: default_group_attribute(),
            write_groups: vec![],
            tls_verify: true,
            timeout: 10,
        };
        let rendered = config.render(config.bind_dn.as_ref().unwrap(), "ada)(uid=*");
        assert!(
            !rendered.contains(")(uid=*"),
            "an injected filter fragment survived: {rendered}"
        );
        assert!(rendered.starts_with("uid=ada\\29"));
    }

    #[tokio::test]
    async fn token_auth_accepts_the_right_token_and_nothing_else() {
        let token = "s3cret-token";
        let config = AuthConfig {
            mode: "token".into(),
            users: vec![
                TokenUser {
                    name: "ci".into(),
                    token_sha256: crate::cache::hash_bytes(token.as_bytes()),
                    read_only: true,
                    admin: false,
                },
                TokenUser {
                    name: "release".into(),
                    token_sha256: crate::cache::hash_bytes(b"another"),
                    read_only: false,
                    admin: false,
                },
            ],
            ..Default::default()
        };

        let identity = authenticate(&config, &[], "ci", token).await.unwrap();
        assert_eq!(identity.name, "ci");
        assert!(!identity.can_write, "a read-only user may not write");

        let identity = authenticate(&config, &[], "release", "another")
            .await
            .unwrap();
        assert!(identity.can_write);

        // Wrong token, and the right token under the wrong name.
        assert!(authenticate(&config, &[], "ci", "nope").await.is_err());
        assert!(authenticate(&config, &[], "release", token).await.is_err());
        assert!(authenticate(&config, &[], "nobody", token).await.is_err());
    }

    #[tokio::test]
    async fn an_open_server_lets_anyone_in_and_says_so() {
        let identity = authenticate(&AuthConfig::default(), &[], "", "")
            .await
            .unwrap();
        assert_eq!(identity, Identity::anonymous());

        let named = authenticate(&AuthConfig::default(), &[], "ada", "")
            .await
            .unwrap();
        assert_eq!(named.name, "ada");
        assert!(named.can_write);
    }

    #[test]
    fn sessions_expire_and_can_be_revoked() {
        let sessions = Sessions::default();
        let identity = Identity {
            name: "ada".into(),
            can_write: true,
            is_admin: false,
            groups: vec!["cn=devs".into()],
        };

        let (token, session) = sessions.issue(identity.clone(), 3600);
        assert!(session.is_live());
        assert_eq!(sessions.lookup(&token), Lookup::Live(identity.clone()));
        assert_eq!(sessions.live_count(), 1);

        // The token itself is never stored — only its hash.
        assert_ne!(session.token_sha256, token);
        assert_eq!(
            session.token_sha256,
            crate::cache::hash_bytes(token.as_bytes())
        );

        assert_eq!(sessions.lookup("some-other-token"), Lookup::Unknown);
        assert!(sessions.revoke(&token));
        assert_eq!(
            sessions.lookup(&token),
            Lookup::Unknown,
            "a revoked session is gone, not merely out of time"
        );
        assert!(!sessions.revoke(&token), "revoking twice is not a success");

        // An already-expired session never resolves — and says so precisely,
        // which is the difference between "check your clock" and "check your
        // URL".
        let (expired, _) = sessions.issue(identity, -1);
        assert!(matches!(sessions.lookup(&expired), Lookup::Expired { .. }));
        assert_eq!(sessions.live_count(), 0);

        // Looking it up dropped it, so the second answer is that there's no
        // record of it at all.
        assert_eq!(sessions.lookup(&expired), Lookup::Unknown);
    }

    /// The point of persisting them: restarting the server must not sign the
    /// whole team out and quietly turn every build into a cache miss.
    #[test]
    fn sessions_survive_a_restart_without_storing_a_token() {
        let dir = std::env::temp_dir().join(format!("ciab_sess_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let identity = Identity {
            name: "ada".into(),
            can_write: true,
            is_admin: false,
            groups: vec!["cn=devs".into()],
        };

        let sessions = Sessions::open(&dir);
        let (token, _) = sessions.issue(identity.clone(), 3600);
        let (expired, _) = sessions.issue(identity.clone(), -1);
        let (logged_out, _) = sessions.issue(identity.clone(), 3600);
        assert!(sessions.revoke(&logged_out));
        drop(sessions);

        // A new process, same storage.
        let restarted = Sessions::open(&dir);
        assert_eq!(restarted.lookup(&token), Lookup::Live(identity));
        assert_eq!(restarted.live_count(), 1);
        assert!(
            matches!(restarted.lookup(&expired), Lookup::Unknown),
            "an expired session must not come back to life — it isn't even loaded"
        );
        assert_eq!(
            restarted.lookup(&logged_out),
            Lookup::Unknown,
            "a logout a restart undoes is not a logout"
        );

        // Whoever reads the file learns who is signed in, never how to be them.
        let raw = std::fs::read_to_string(dir.join("sessions.json")).unwrap();
        assert!(raw.contains("ada"));
        assert!(
            !raw.contains(&token),
            "the bearer token must never be stored"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn tokens_are_long_and_distinct() {
        let a = generate_token();
        let b = generate_token();
        assert_eq!(a.len(), 48);
        assert_ne!(a, b);
        assert!(a.chars().all(|c| c.is_ascii_alphanumeric()));
    }
}
