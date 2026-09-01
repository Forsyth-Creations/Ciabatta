//! Talking to a remote cache from a build.
//!
//! The contract this client holds to is that a remote cache is an optimisation,
//! and an optimisation must never be able to fail a build. Every lookup and
//! every upload is best-effort: a server that's down, slow, or has forgotten
//! who you are costs you a rebuild and a line on stderr, not a red pipeline.
//! The one place that isn't true is `login`, which is a user asking a direct
//! question and deserves a direct answer.
//!
//! Credentials live in `~/.ciabatta/remote-cache.json`, keyed by server URL, so
//! one machine can be logged in to several caches at once — which happens the
//! moment somebody has a work cache and a personal one.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use super::auth::Identity;
use super::projects::Project;
use super::releases::Release;
use crate::cache::store::Entry;

/// How long to wait on a cache server before giving up and building.
///
/// Short on purpose. Waiting thirty seconds to discover the cache is down is
/// strictly worse than having no cache at all.
const TIMEOUT: Duration = Duration::from_secs(10);

/// A longer limit for the artifact transfers themselves, which are legitimately
/// large.
const TRANSFER_TIMEOUT: Duration = Duration::from_secs(300);

/// The saved credentials for every cache this machine knows about.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Credentials {
    /// Server URL → what we know about it.
    #[serde(default)]
    pub servers: BTreeMap<String, Credential>,
}

/// One server's saved session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Credential {
    pub token: String,
    pub user: String,
    pub expires_at: Option<String>,
    /// The release this server was advertising when we last spoke to it, so the
    /// update notice can be shown without an extra round trip.
    #[serde(default)]
    pub release: Option<Release>,
    /// Whether this machine verifies the server's certificate, remembered from
    /// the login so later commands reach it the same way.
    #[serde(default = "yes")]
    pub tls_verify: bool,
}

fn yes() -> bool {
    true
}

impl Credential {
    /// Whether the saved session is still worth sending.
    pub fn is_live(&self) -> bool {
        match &self.expires_at {
            None => true,
            Some(when) => chrono::DateTime::parse_from_rfc3339(when)
                .map(|expiry| expiry.with_timezone(&chrono::Utc) > chrono::Utc::now())
                .unwrap_or(false),
        }
    }
}

impl Credentials {
    /// Load the saved credentials, treating any problem as "none".
    pub fn load() -> Self {
        let Ok(path) = super::credentials_path() else {
            return Credentials::default();
        };
        std::fs::read_to_string(path)
            .ok()
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_default()
    }

    /// Persist them.
    pub fn save(&self) -> Result<()> {
        let path = super::credentials_path()?;
        let body = serde_json::to_string_pretty(self)?;
        std::fs::write(&path, body)
            .with_context(|| format!("Failed to write {}", path.display()))?;
        restrict_permissions(&path);
        Ok(())
    }

    /// The credential for a server, if it's still live.
    pub fn get(&self, url: &str) -> Option<&Credential> {
        self.servers
            .get(normalize(url).as_str())
            .filter(|c| c.is_live())
    }

    /// Record a login.
    pub fn set(&mut self, url: &str, credential: Credential) {
        self.servers.insert(normalize(url), credential);
    }

    /// Forget a server.
    pub fn remove(&mut self, url: &str) -> bool {
        self.servers.remove(normalize(url).as_str()).is_some()
    }

    /// Whether this machine verifies `url`'s certificate, from the last login.
    ///
    /// Defaults to verifying for a server never logged in to — the safe answer
    /// when nobody has said otherwise.
    pub fn tls_verify(&self, url: &str) -> bool {
        self.servers
            .get(normalize(url).as_str())
            .map(|c| c.tls_verify)
            .unwrap_or(true)
    }

    /// Other saved logins that are almost certainly the *same server* under a
    /// different name.
    ///
    /// One machine is reachable as `localhost`, as `127.0.0.1`, and as `[::1]`,
    /// and a credential is keyed by the URL somebody typed rather than by the
    /// server it reached. So `remote-cache login http://127.0.0.1:8380` followed
    /// by a workspace configured for `http://localhost:8380` leaves you logged
    /// in to a server that tells you it has never heard of you — which is true,
    /// and useless.
    ///
    /// Only loopback names are treated this way. Two DNS names for one host is
    /// a thing this can't know from here, and guessing at it would be worse
    /// than saying nothing.
    pub fn same_server_as(&self, url: &str) -> Vec<String> {
        let Some((host, port)) = host_and_port(url) else {
            return Vec::new();
        };
        if !is_loopback(&host) {
            return Vec::new();
        }
        self.servers
            .keys()
            .filter(|saved| normalize(saved) != normalize(url))
            .filter(|saved| host_and_port(saved).is_some_and(|(h, p)| p == port && is_loopback(&h)))
            .cloned()
            .collect()
    }
}

/// The host and port of a URL, lowercased, without pulling in a URL parser for
/// the two fields anybody here needs.
fn host_and_port(url: &str) -> Option<(String, String)> {
    let rest = normalize(url)
        .split_once("://")
        .map(|(_, rest)| rest.to_string())
        .unwrap_or_else(|| normalize(url));
    let authority = rest.split(['/', '?']).next()?.to_lowercase();
    // An IPv6 literal is bracketed, so the last colon is the port separator
    // only when it comes after the closing bracket.
    match authority.rsplit_once(':') {
        Some((host, port)) if !port.contains(']') && port.chars().all(|c| c.is_ascii_digit()) => {
            Some((host.to_string(), port.to_string()))
        }
        _ => Some((authority, String::new())),
    }
}

/// Whether a host name is this machine talking to itself.
fn is_loopback(host: &str) -> bool {
    matches!(
        host.trim_matches(['[', ']']),
        "localhost" | "127.0.0.1" | "::1" | "0.0.0.0"
    )
}

/// The credentials file holds bearer tokens; nobody else on the machine needs
/// to read it.
#[cfg(unix)]
fn restrict_permissions(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
}

#[cfg(not(unix))]
fn restrict_permissions(_path: &Path) {}

/// Canonical form of a server URL, so `http://host:8380` and
/// `http://host:8380/` are the same entry rather than two.
pub fn normalize(url: &str) -> String {
    url.trim().trim_end_matches('/').to_string()
}

// ─── The client ─────────────────────────────────────────────────────────────

/// A connection to one remote cache.
#[derive(Debug, Clone)]
pub struct Client {
    base: String,
    token: Option<String>,
    http: reqwest::Client,
}

impl Client {
    /// Build a client for `url`, using whatever session is saved for it.
    ///
    /// `tls_verify` is a required argument rather than a builder option on
    /// purpose: turning certificate checking off is a decision, and a default
    /// that call sites can forget to override is a decision nobody made.
    pub fn new(url: &str, tls_verify: bool) -> Result<Self> {
        let base = normalize(url);
        anyhow::ensure!(!base.is_empty(), "a remote cache needs a URL");

        let mut builder = reqwest::Client::builder().timeout(TRANSFER_TIMEOUT);
        if !tls_verify {
            builder = builder.danger_accept_invalid_certs(true);
        }

        Ok(Client {
            token: Credentials::load().get(&base).map(|c| c.token.clone()),
            base,
            http: builder.build().context("Failed to build the HTTP client")?,
        })
    }

    /// A client for `url` using whatever TLS setting was saved when this
    /// machine logged in, defaulting to verifying.
    ///
    /// For the commands that act on a server without a workspace to read the
    /// setting from — `logout`, `status`, `self update`.
    pub fn saved(url: &str) -> Result<Self> {
        Self::new(url, Credentials::load().tls_verify(url))
    }

    /// A client with an explicit token, for the login flow itself.
    pub fn with_token(url: &str, tls_verify: bool, token: Option<String>) -> Result<Self> {
        let mut client = Self::new(url, tls_verify)?;
        client.token = token;
        Ok(client)
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }

    fn authed(&self, request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match &self.token {
            Some(token) => request.bearer_auth(token),
            None => request,
        }
    }

    /// The server's health, which carries the version it advertises.
    pub async fn health(&self) -> Result<Health> {
        let response = self
            .http
            .get(self.url("/api/health"))
            .timeout(TIMEOUT)
            .send()
            .await
            .with_context(|| format!("Could not reach the remote cache at {}", self.base))?;
        parse(response).await
    }

    /// Log in, returning the session token and who the server says you are.
    pub async fn login(&self, username: &str, password: &str) -> Result<LoginResponse> {
        let response = self
            .http
            .post(self.url("/api/auth/login"))
            .timeout(TIMEOUT)
            .json(&serde_json::json!({ "username": username, "password": password }))
            .send()
            .await
            .with_context(|| format!("Could not reach the remote cache at {}", self.base))?;
        parse(response).await
    }

    /// End this session on the server.
    pub async fn logout(&self) -> Result<()> {
        let _ = self
            .authed(self.http.post(self.url("/api/auth/logout")))
            .timeout(TIMEOUT)
            .send()
            .await;
        Ok(())
    }

    /// The server's stats, for `remote-cache status` and the web view.
    pub async fn stats(&self) -> Result<serde_json::Value> {
        let response = self
            .authed(self.http.get(self.url("/api/stats")))
            .timeout(TIMEOUT)
            .send()
            .await
            .with_context(|| format!("Could not reach the remote cache at {}", self.base))?;
        parse(response).await
    }

    /// Resolve this project's identity on the server, registering it if needed.
    pub async fn register(&self, name: &str, id: Option<&str>) -> Result<Project> {
        let response = self
            .authed(self.http.post(self.url("/api/projects")))
            .timeout(TIMEOUT)
            .json(&serde_json::json!({ "name": name, "id": id }))
            .send()
            .await
            .with_context(|| format!("Could not reach the remote cache at {}", self.base))?;
        parse(response).await
    }

    /// Look a key up. `None` is a miss; an error is a server problem.
    pub async fn lookup(&self, project: &str, key: &str) -> Result<Option<Entry>> {
        let response = self
            .authed(
                self.http
                    .get(self.url(&format!("/api/projects/{project}/cache/{key}"))),
            )
            .timeout(TIMEOUT)
            .send()
            .await
            .with_context(|| format!("Could not reach the remote cache at {}", self.base))?;

        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        parse(response).await.map(Some)
    }

    /// Download an entry's outputs into `dir`, verifying each against the
    /// manifest before it lands.
    pub async fn download(
        &self,
        project: &str,
        key: &str,
        entry: &Entry,
        dir: &Path,
    ) -> Result<()> {
        for output in &entry.outputs {
            let response = self
                .authed(
                    self.http
                        .get(self.url(&format!("/api/projects/{project}/cache/{key}/artifact"))),
                )
                .query(&[("path", &output.path)])
                .send()
                .await
                .with_context(|| format!("Failed to download {}", output.path))?
                .error_for_status()
                .with_context(|| format!("Failed to download {}", output.path))?;

            let bytes = response.bytes().await?;
            let actual = crate::cache::hash_bytes(&bytes);
            anyhow::ensure!(
                actual == output.sha256,
                "The remote cache served a {} that doesn't match its recorded hash — \
                 refusing to write it",
                output.path
            );

            let path = dir.join(&output.path);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("Failed to create {}", parent.display()))?;
            }
            std::fs::write(&path, &bytes)
                .with_context(|| format!("Failed to write {}", path.display()))?;
        }
        Ok(())
    }

    /// Upload a build's outputs and then its manifest.
    ///
    /// Report the keys a run reused, so the server ages them from now.
    ///
    /// A short timeout, because nothing waits on the answer: this runs at the
    /// end of a build that has already produced everything it was asked for.
    pub async fn touch(&self, project: &str, keys: &[String]) -> Result<()> {
        let response = self
            .authed(
                self.http
                    .post(self.url(&format!("/api/projects/{project}/cache/touch"))),
            )
            .timeout(TIMEOUT)
            .json(&serde_json::json!({ "keys": keys }))
            .send()
            .await
            .context("Could not reach the remote cache")?;
        let _: serde_json::Value = parse(response).await?;
        Ok(())
    }

    /// Manifest last, always: it is what makes the entry visible, so writing it
    /// before the files it names would publish an entry that can't be restored.
    pub async fn upload(&self, project: &str, key: &str, entry: &Entry, dir: &Path) -> Result<()> {
        for output in &entry.outputs {
            let bytes = std::fs::read(dir.join(&output.path))
                .with_context(|| format!("Failed to read {} to upload it", output.path))?;

            self.authed(
                self.http
                    .put(self.url(&format!("/api/projects/{project}/cache/{key}/artifact"))),
            )
            .query(&[("path", &output.path)])
            .body(bytes)
            .send()
            .await
            .with_context(|| format!("Failed to upload {}", output.path))?
            .error_for_status()
            .with_context(|| format!("Failed to upload {}", output.path))?;
        }

        self.authed(
            self.http
                .put(self.url(&format!("/api/projects/{project}/cache/{key}"))),
        )
        .timeout(TIMEOUT)
        .json(entry)
        .send()
        .await
        .context("Failed to publish the cache manifest")?
        .error_for_status()
        .context("Failed to publish the cache manifest")?;

        Ok(())
    }

    /// Report what workflows this checkout just ran.
    ///
    /// The server merges rather than replaces, so reports arriving out of order
    /// — which they will, from many machines — cannot walk the picture
    /// backwards. See [`super::workflows`].
    pub async fn report_workflows(
        &self,
        project: &str,
        records: &[crate::run::history::Record],
    ) -> Result<()> {
        let capped: Vec<_> = records
            .iter()
            .take(super::workflows::MAX_REPORTED)
            .collect();
        self.authed(
            self.http
                .post(self.url(&format!("/api/projects/{project}/workflows"))),
        )
        .timeout(TIMEOUT)
        .json(&serde_json::json!({ "workflows": capped }))
        .send()
        .await
        .context("Failed to report workflow history")?
        .error_for_status()
        .context("Failed to report workflow history")?;
        Ok(())
    }

    /// What every checkout of a project has run, as the server has it.
    pub async fn workflows(&self, project: &str) -> Result<Vec<crate::run::history::Record>> {
        let response = self
            .authed(
                self.http
                    .get(self.url(&format!("/api/projects/{project}/workflows"))),
            )
            .timeout(TIMEOUT)
            .send()
            .await
            .context("Failed to read the shared workflow history")?;
        let body: serde_json::Value = parse(response).await?;
        Ok(serde_json::from_value(body["workflows"].clone()).unwrap_or_default())
    }

    /// What ciabatta build this server hands out.
    pub async fn release(&self) -> Result<Release> {
        let response = self
            .http
            .get(self.url("/api/release"))
            .timeout(TIMEOUT)
            .send()
            .await
            .with_context(|| format!("Could not reach the remote cache at {}", self.base))?;
        parse(response).await
    }

    /// Download the binary for a platform.
    pub async fn download_release(&self, platform: &str) -> Result<Vec<u8>> {
        let response = self
            .http
            .get(self.url(&format!("/api/release/{platform}")))
            .send()
            .await
            .with_context(|| format!("Could not reach the remote cache at {}", self.base))?;

        if response.status() == reqwest::StatusCode::NOT_FOUND {
            bail!("{} has no ciabatta build for {platform}", self.base);
        }
        let response = response
            .error_for_status()
            .context("The cache refused to serve the binary")?;
        Ok(response.bytes().await?.to_vec())
    }
}

/// What `/api/health` returns.
#[derive(Debug, Clone, Deserialize)]
pub struct Health {
    #[serde(default)]
    pub ok: bool,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub auth: String,
    #[serde(default)]
    pub release: Option<Release>,
}

/// What `/api/auth/login` returns.
#[derive(Debug, Clone, Deserialize)]
pub struct LoginResponse {
    pub token: String,
    pub expires_at: Option<String>,
    pub user: Identity,
}

/// A failure the server explained for itself.
///
/// Carried as its own error type rather than a formatted string so a caller can
/// ask *what kind* of failure it was — see [`ServerError::session_is_dead`] —
/// without matching on English prose that is free to be reworded.
#[derive(Debug, Clone)]
pub struct ServerError {
    /// The server's own message, fit to print verbatim.
    pub message: String,
    /// Its machine-readable tag, when it sent one.
    pub code: Option<String>,
}

impl std::fmt::Display for ServerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for ServerError {}

impl ServerError {
    /// Whether the credential that produced this is dead for good.
    ///
    /// An expired session and one the server has never heard of are both
    /// finished: no retry, no backoff and no amount of waiting brings either
    /// back. Only a fresh login will, which is why it's worth knowing — a
    /// client that keeps a credential it has been told is dead will fail this
    /// way on every command until somebody works out why.
    pub fn session_is_dead(&self) -> bool {
        matches!(
            self.code.as_deref(),
            Some("session_expired" | "session_unknown")
        )
    }

    /// Whether the server refused this for want of a usable credential, in any
    /// of the ways it can.
    ///
    /// Wider than [`Self::session_is_dead`]: having *no* credential for a URL
    /// is not a credential to throw away, but it is the same question from the
    /// caller's side — "why won't it let me in, when I know I logged in?" —
    /// and deserves the same help.
    pub fn is_auth_failure(&self) -> bool {
        self.session_is_dead() || self.code.as_deref() == Some("no_credential")
    }
}

/// Turn a response into `T`, surfacing the server's own error message.
///
/// A cache that says "your session has expired, log in again" is worth
/// relaying verbatim; "HTTP 401" is not.
async fn parse<T: serde::de::DeserializeOwned>(response: reqwest::Response) -> Result<T> {
    let status = response.status();
    let body = response.text().await.unwrap_or_default();

    if !status.is_success() {
        return Err(anyhow::Error::new(ServerError {
            message: error_message(status, &body),
            code: error_code(&body),
        }));
    }

    serde_json::from_str(&body).context("The remote cache returned something unexpected")
}

/// The failure's machine-readable tag, when the server sent one.
fn error_code(body: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()?
        .get("code")?
        .as_str()
        .map(str::to_string)
}

/// The most useful thing that can be said about a failed response: the server's
/// own `error` field, then its raw body, then the bare status.
fn error_message(status: reqwest::StatusCode, body: &str) -> String {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v["error"].as_str().map(str::to_string))
        .unwrap_or_else(|| {
            if body.trim().is_empty() {
                status.to_string()
            } else {
                body.to_string()
            }
        })
}

// ─── Best-effort helpers used during a build ────────────────────────────────

/// Try the remote cache for `key`, restoring into `dir` on a hit.
///
/// Returns `Ok(false)` for every kind of miss — including a server that's down
/// — because a cache lookup must never be the reason a build fails. Problems
/// are reported once, on stderr, and then got out of the way of.
pub async fn try_restore(client: &Client, project: &str, key: &str, dir: &Path) -> bool {
    match client.lookup(project, key).await {
        Ok(Some(entry)) => match client.download(project, key, &entry, dir).await {
            Ok(()) => true,
            Err(e) => {
                eprintln!("note: the remote cache had this build but couldn't serve it ({e:#})");
                false
            }
        },
        Ok(None) => false,
        Err(e) => {
            eprintln!("note: the remote cache is unavailable ({e:#}); building locally");
            false
        }
    }
}

/// Tell the remote cache which entries a run reused, so retention ages them
/// from now.
///
/// One request for the whole run, and entirely best-effort: this is bookkeeping
/// that keeps a shared cache useful, and no part of it is worth a line of
/// output on a build that otherwise succeeded.
pub async fn try_touch(client: &Client, project: &str, keys: &[String]) {
    if keys.is_empty() {
        return;
    }
    if let Err(e) = client.touch(project, keys).await {
        tracing::debug!("couldn't refresh {} cache entr(ies): {e:#}", keys.len());
    }
}

/// Publish a build to the remote cache, best-effort.
pub async fn try_upload(client: &Client, project: &str, key: &str, entry: &Entry, dir: &Path) {
    if let Err(e) = client.upload(project, key, entry, dir).await {
        eprintln!("note: couldn't publish this build to the remote cache ({e:#})");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_normalize_so_one_server_is_one_entry() {
        assert_eq!(normalize("http://cache:8380/"), "http://cache:8380");
        assert_eq!(normalize("  http://cache:8380  "), "http://cache:8380");
        assert_eq!(normalize("http://cache:8380"), "http://cache:8380");

        let mut credentials = Credentials::default();
        credentials.set(
            "http://cache:8380/",
            Credential {
                token: "t".into(),
                user: "ada".into(),
                expires_at: None,
                release: None,
                tls_verify: true,
            },
        );
        // Saved with a trailing slash, found without one.
        assert!(credentials.get("http://cache:8380").is_some());
        assert!(credentials.get("http://cache:8380/").is_some());
        assert!(credentials.remove("http://cache:8380"));
        assert!(credentials.get("http://cache:8380").is_none());
    }

    #[test]
    fn an_expired_credential_is_not_offered() {
        let past = (chrono::Utc::now() - chrono::Duration::hours(1)).to_rfc3339();
        let future = (chrono::Utc::now() + chrono::Duration::hours(1)).to_rfc3339();

        let mut credentials = Credentials::default();
        credentials.set(
            "http://a",
            Credential {
                token: "t".into(),
                user: "ada".into(),
                expires_at: Some(past),
                release: None,
                tls_verify: true,
            },
        );
        credentials.set(
            "http://b",
            Credential {
                token: "t".into(),
                user: "ada".into(),
                expires_at: Some(future),
                release: None,
                tls_verify: true,
            },
        );
        credentials.set(
            "http://c",
            Credential {
                token: "t".into(),
                user: "ada".into(),
                // No expiry at all — an open server's session.
                expires_at: None,
                release: None,
                tls_verify: true,
            },
        );

        assert!(credentials.get("http://a").is_none(), "expired");
        assert!(credentials.get("http://b").is_some());
        assert!(credentials.get("http://c").is_some());
    }

    #[test]
    fn a_client_needs_somewhere_to_connect_to() {
        assert!(Client::new("   ", true).is_err());
        assert!(Client::new("http://cache:8380", true).is_ok());
        // Both TLS settings build a usable client; the difference is what it
        // will accept from the far end.
        assert!(Client::new("https://cache.example.com", false).is_ok());
    }

    /// A server nobody has logged in to verifies, and one logged in to with
    /// verification off keeps that setting for later commands.
    #[test]
    fn the_tls_setting_is_remembered_per_server() {
        let mut credentials = Credentials::default();
        assert!(
            credentials.tls_verify("https://never-seen"),
            "verifying is the answer when nobody has said otherwise"
        );

        credentials.set(
            "https://self-signed",
            Credential {
                token: "t".into(),
                user: "ada".into(),
                expires_at: None,
                release: None,
                tls_verify: false,
            },
        );
        assert!(!credentials.tls_verify("https://self-signed"));
        assert!(!credentials.tls_verify("https://self-signed/"));
    }

    /// The exact confusion this exists to end: logged in as `127.0.0.1`, with a
    /// workspace configured for `localhost`, told by the server that it has
    /// never heard of you — which is true, and on its own useless.
    #[test]
    fn a_loopback_login_under_another_name_is_recognised_as_the_same_server() {
        let mut credentials = Credentials::default();
        let credential = Credential {
            token: "t".into(),
            user: "ada".into(),
            expires_at: None,
            release: None,
            tls_verify: true,
        };
        credentials.set("http://127.0.0.1:8380", credential.clone());
        credentials.set("http://cache.example.com:8380", credential.clone());

        assert_eq!(
            credentials.same_server_as("http://localhost:8380/"),
            vec!["http://127.0.0.1:8380".to_string()],
            "a trailing slash and a different loopback spelling are the same machine"
        );

        // A different port is a different server, loopback or not.
        assert!(
            credentials
                .same_server_as("http://localhost:9999")
                .is_empty()
        );

        // And two DNS names for one host is not something this can know, so it
        // must not claim to.
        assert!(
            credentials
                .same_server_as("http://cache.internal:8380")
                .is_empty(),
            "guessing at DNS aliases would be worse than saying nothing"
        );
    }

    /// A dead session has to be recognisable without matching on prose, or the
    /// client can never safely act on it.
    #[test]
    fn a_dead_session_is_told_apart_from_any_other_failure() {
        let dead = |code: &str| ServerError {
            message: "whatever the server said".into(),
            code: Some(code.into()),
        };
        assert!(dead("session_expired").session_is_dead());
        assert!(dead("session_unknown").session_is_dead());

        // Missing credentials are fixed by logging in, not by discarding one.
        assert!(!dead("no_credential").session_is_dead());
        assert!(
            !ServerError {
                message: "Unknown project 'nope'".into(),
                code: None,
            }
            .session_is_dead(),
            "an untagged failure must never be read as an auth failure"
        );
    }

    #[test]
    fn a_failure_carries_the_servers_code_when_it_sent_one() {
        let body = serde_json::json!({
            "error": "This cache has no record of your session",
            "code": "session_unknown",
        })
        .to_string();
        assert_eq!(error_code(&body).as_deref(), Some("session_unknown"));
        assert_eq!(error_code("<html>nginx</html>"), None);
        assert_eq!(error_code(r#"{"error":"no code here"}"#), None);
    }

    /// A cache that says "your session has expired, log in again" is worth
    /// relaying verbatim; "HTTP status 401" is not.
    #[test]
    fn server_errors_come_back_as_their_own_message() {
        let advice = "Your session has expired. Run `ciabatta remote-cache login <URL>` again.";
        let body = serde_json::json!({ "error": advice }).to_string();
        assert_eq!(
            error_message(reqwest::StatusCode::UNAUTHORIZED, &body),
            advice
        );

        // A body that isn't ciabatta's error shape is still better than nothing.
        assert_eq!(
            error_message(reqwest::StatusCode::BAD_GATEWAY, "<html>nginx</html>"),
            "<html>nginx</html>"
        );

        // …and an empty body falls back to the status.
        assert_eq!(
            error_message(reqwest::StatusCode::BAD_GATEWAY, "   "),
            reqwest::StatusCode::BAD_GATEWAY.to_string()
        );
    }
}
