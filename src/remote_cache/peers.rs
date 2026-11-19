//! Remote cache servers keeping each other in step: `peers:` in the server's
//! config.
//!
//! ```yaml
//! peers:
//!   - https://cache-dr.internal:8380
//!   - url: https://cache-eu.internal:8380
//!     # When the peer asks for a login: who this server is there.
//!     username: peer-sync
//!     password_env: CIABATTA_PEER_TOKEN
//!     tls_verify: true
//! ```
//!
//! Each server is told about the others and pulls from them what it lacks: the
//! projects they know, the cache entries they hold (artifacts and manifest,
//! verified against the manifest's hashes as they land), the builds in their
//! feeds, and their workflow history. Pulling rather than pushing means a peer
//! only ever needs to be *read*: the account a server logs in to its peers
//! with can be read-only, and a server that was down catches up by itself the
//! moment it is back, from whichever peers it can reach.
//!
//! It happens on a timer (`server.sync_every`, a minute by default) and, so
//! the servers don't sit a minute apart, on a nudge: a server that has just
//! been written to tells its peers, and they pull straight away. A server that
//! pulled something new nudges its own peers in turn, so a chain of servers
//! converges without every one having to list every other — and since a nudge
//! only follows something new, the ripple stops once they agree.
//!
//! The same `peers:` list can be copied to every server, itself included: each
//! server has an instance id (`GET /api/health`), and one that finds itself in
//! the list skips that entry.
//!
//! What a server removed on purpose stays removed. An entry its retention
//! evicted, everything cleared from the admin page, a project forgotten — each
//! is remembered for a while ([`FORGET_FOR`]) so the copy a peer still holds
//! isn't pulled straight back. Removal is not replicated: clearing or
//! forgetting is done on each server.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use super::client::{Client, normalize};
use super::server::AppState;

/// How long a deliberate removal keeps a peer's copy from being pulled back.
const FORGET_FOR: Duration = Duration::from_secs(30 * 24 * 3600);

/// How long a burst of writes is gathered before the peers are nudged, so a
/// build uploading two hundred steps sends one nudge rather than two hundred.
const NUDGE_AFTER: Duration = Duration::from_secs(2);

/// How long a nudged server waits before pulling, for the same reason.
const PULL_AFTER: Duration = Duration::from_secs(1);

/// The fewest seconds `server.sync_every` may ask for.
const MIN_EVERY: i64 = 5;

/// The interval when `server.sync_every` says nothing.
pub fn default_every() -> String {
    "1m".to_string()
}

// ─── Config ─────────────────────────────────────────────────────────────────

/// One other server holding the same cache, as `peers:` lists it: a URL, or a
/// URL with the credential and TLS settings to reach it with.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Peer {
    pub url: String,

    /// Who this server logs in to the peer as, when the peer's `auth.mode`
    /// isn't `open`. A token user on a token-mode peer
    /// (`ciabatta remote-cache add-user`); read-only access is enough.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,

    /// The environment variable holding that user's token or password. Named
    /// rather than written here, so the config is never a credential.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password_env: Option<String>,

    /// Check the peer's certificate. Off only for a test server.
    pub tls_verify: bool,

    /// A CA certificate to trust the peer's certificate by, relative to the
    /// config file — for a peer with a self-signed one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ca_cert: Option<PathBuf>,
}

impl<'de> Deserialize<'de> for Peer {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Full {
            url: String,
            #[serde(default)]
            username: Option<String>,
            #[serde(default)]
            password_env: Option<String>,
            #[serde(default = "yes")]
            tls_verify: bool,
            #[serde(default)]
            ca_cert: Option<PathBuf>,
        }
        fn yes() -> bool {
            true
        }
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            Url(String),
            Full(Full),
        }
        Ok(match Raw::deserialize(deserializer)? {
            Raw::Url(url) => Peer {
                url,
                username: None,
                password_env: None,
                tls_verify: true,
                ca_cert: None,
            },
            Raw::Full(full) => Peer {
                url: full.url,
                username: full.username,
                password_env: full.password_env,
                tls_verify: full.tls_verify,
                ca_cert: full.ca_cert,
            },
        })
    }
}

impl Peer {
    pub fn resolve_relative(&mut self, base: &Path) {
        if let Some(ca) = self.ca_cert.as_mut()
            && ca.is_relative()
        {
            *ca = base.join(&*ca);
        }
    }
}

// ─── What a server remembers ────────────────────────────────────────────────

/// How one peer has been doing, for `GET /api/peers` and the admin page.
#[derive(Debug, Clone, Default, Serialize)]
pub struct PeerStatus {
    pub url: String,
    /// This entry is the server itself, and is skipped.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub is_self: bool,
    /// When a sync with it last finished, successfully or not.
    pub last_sync_at: Option<String>,
    /// When one last succeeded.
    pub last_ok_at: Option<String>,
    /// Why the last one failed, when it did.
    pub last_error: Option<String>,
    /// What has been pulled from it since this server started.
    pub pulled_entries: u64,
    pub pulled_bytes: u64,
    pub pulled_builds: u64,
    /// The cache entries it held at the last sync, across every project.
    pub entries: Option<u64>,
}

/// Removals not to undo: `scoped key → unix seconds` and `project id → unix
/// seconds`.
#[derive(Debug, Default, Serialize, Deserialize)]
struct Forgotten {
    #[serde(default)]
    entries: BTreeMap<String, u64>,
    #[serde(default)]
    projects: BTreeMap<String, u64>,
}

/// This server's side of peering: who its peers are, how each is doing, and
/// what it must not pull back.
#[derive(Debug)]
pub struct Peers {
    peers: Vec<Peer>,
    /// Who this server is, so it can spot itself in its own `peers:` list.
    pub instance: String,
    /// Woken by a nudge (or `POST /api/peers/sync`) to pull now.
    wake: tokio::sync::Notify,
    /// A nudge to the peers is already on its way.
    nudging: AtomicBool,
    /// The session this server holds on each peer that asked for a login.
    sessions: Mutex<HashMap<String, String>>,
    status: Mutex<Vec<PeerStatus>>,
    forgotten: Mutex<Forgotten>,
    forgotten_path: PathBuf,
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
}

impl Peers {
    pub fn open(storage: &Path, peers: &[Peer]) -> Result<Self> {
        std::fs::create_dir_all(storage)
            .with_context(|| format!("Failed to create {}", storage.display()))?;
        let instance_path = storage.join("instance-id");
        let instance = match std::fs::read_to_string(&instance_path) {
            Ok(id) if !id.trim().is_empty() => id.trim().to_string(),
            _ => {
                let id = uuid::Uuid::new_v4().to_string();
                std::fs::write(&instance_path, &id)
                    .with_context(|| format!("Failed to write {}", instance_path.display()))?;
                id
            }
        };
        let forgotten_path = storage.join("forgotten.json");
        let forgotten = std::fs::read_to_string(&forgotten_path)
            .ok()
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_default();
        Ok(Peers {
            status: Mutex::new(
                peers
                    .iter()
                    .map(|p| PeerStatus {
                        url: normalize(&p.url),
                        ..Default::default()
                    })
                    .collect(),
            ),
            peers: peers.to_vec(),
            instance,
            wake: tokio::sync::Notify::new(),
            nudging: AtomicBool::new(false),
            sessions: Mutex::new(HashMap::new()),
            forgotten: Mutex::new(forgotten),
            forgotten_path,
        })
    }

    pub fn is_empty(&self) -> bool {
        self.peers.is_empty()
    }

    pub fn status(&self) -> Vec<PeerStatus> {
        self.status.lock().unwrap().clone()
    }

    fn update(&self, url: &str, change: impl FnOnce(&mut PeerStatus)) {
        let mut status = self.status.lock().unwrap();
        if let Some(entry) = status.iter_mut().find(|s| s.url == normalize(url)) {
            change(entry);
        }
    }

    /// Pull from every peer now rather than at the next tick.
    pub fn wake(&self) {
        self.wake.notify_one();
    }

    // ── Removals ──

    /// Remember that these entries (scoped keys) were removed on purpose.
    pub fn forget_entries<'a>(&self, keys: impl IntoIterator<Item = &'a str>) {
        if self.is_empty() {
            return;
        }
        let now = unix_now();
        let mut forgotten = self.forgotten.lock().unwrap();
        for key in keys {
            forgotten.entries.insert(key.to_string(), now);
        }
        self.save(&forgotten);
    }

    /// Remember that a project was forgotten on purpose.
    pub fn forget_project(&self, id: &str) {
        if self.is_empty() {
            return;
        }
        let mut forgotten = self.forgotten.lock().unwrap();
        forgotten.projects.insert(id.to_string(), unix_now());
        self.save(&forgotten);
    }

    /// An entry written here again is wanted again.
    pub fn remember_entry(&self, key: &str) {
        let mut forgotten = self.forgotten.lock().unwrap();
        if forgotten.entries.remove(key).is_some() {
            self.save(&forgotten);
        }
    }

    /// A project registered here again is wanted again.
    pub fn remember_project(&self, id: &str) {
        let mut forgotten = self.forgotten.lock().unwrap();
        if forgotten.projects.remove(id).is_some() {
            self.save(&forgotten);
        }
    }

    fn is_forgotten_entry(&self, key: &str) -> bool {
        self.forgotten.lock().unwrap().entries.contains_key(key)
    }

    fn is_forgotten_project(&self, id: &str) -> bool {
        self.forgotten.lock().unwrap().projects.contains_key(id)
    }

    /// Let removals older than [`FORGET_FOR`] go: by then every peer's own
    /// retention has had its chance to drop the same entries.
    fn expire(&self) {
        let cutoff = unix_now().saturating_sub(FORGET_FOR.as_secs());
        let mut forgotten = self.forgotten.lock().unwrap();
        let before = forgotten.entries.len() + forgotten.projects.len();
        forgotten.entries.retain(|_, at| *at >= cutoff);
        forgotten.projects.retain(|_, at| *at >= cutoff);
        if forgotten.entries.len() + forgotten.projects.len() != before {
            self.save(&forgotten);
        }
    }

    fn save(&self, forgotten: &Forgotten) {
        let written = serde_json::to_string(forgotten)
            .map_err(anyhow::Error::from)
            .and_then(|body| {
                std::fs::write(&self.forgotten_path, body).map_err(anyhow::Error::from)
            });
        if let Err(e) = written {
            tracing::warn!("couldn't save {}: {e:#}", self.forgotten_path.display());
        }
    }

    // ── Talking to peers ──

    /// A client for `peer`, with whatever session this server holds there.
    fn client(&self, peer: &Peer) -> Result<Client> {
        let token = self
            .sessions
            .lock()
            .unwrap()
            .get(&normalize(&peer.url))
            .cloned();
        Client::with_token(&peer.url, peer.tls_verify, peer.ca_cert.as_deref(), token)
    }

    /// A client `peer` will answer: the held session if it still works,
    /// otherwise a fresh login with the configured credential.
    async fn signed_in(&self, peer: &Peer) -> Result<Client> {
        let client = self.client(peer)?;
        match client.get_json::<serde_json::Value>("/api/me").await {
            Ok(_) => return Ok(client),
            Err(e)
                if !e
                    .chain()
                    .any(|c| c.downcast_ref::<super::client::ServerError>().is_some()) =>
            {
                return Err(e);
            }
            Err(_) => {}
        }
        let username = peer.username.as_deref().with_context(|| {
            format!(
                "{} asks for a login: give this peer a `username` and `password_env`",
                normalize(&peer.url)
            )
        })?;
        let password = match peer.password_env.as_deref() {
            Some(name) => std::env::var(name)
                .with_context(|| format!("{name} (this peer's password_env) isn't set"))?,
            None => anyhow::bail!(
                "{} asks for a login: name the variable holding {username}'s token in \
                 `password_env`",
                normalize(&peer.url)
            ),
        };
        let bare = Client::with_token(&peer.url, peer.tls_verify, peer.ca_cert.as_deref(), None)?;
        let login = bare
            .login(username, &password)
            .await
            .with_context(|| format!("couldn't log in as {username}"))?;
        self.sessions
            .lock()
            .unwrap()
            .insert(normalize(&peer.url), login.token.clone());
        Client::with_token(
            &peer.url,
            peer.tls_verify,
            peer.ca_cert.as_deref(),
            Some(login.token),
        )
    }

    /// Tell every peer this server has something new, so they pull now. Many
    /// calls in quick succession send one nudge.
    pub fn nudge(self: &std::sync::Arc<Self>) {
        if self.is_empty() || self.nudging.swap(true, Ordering::SeqCst) {
            return;
        }
        // No runtime (a synchronous test): the timer will do.
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            self.nudging.store(false, Ordering::SeqCst);
            return;
        };
        let me = self.clone();
        runtime.spawn(async move {
            tokio::time::sleep(NUDGE_AFTER).await;
            me.nudging.store(false, Ordering::SeqCst);
            let skip: HashSet<String> = me
                .status()
                .into_iter()
                .filter(|s| s.is_self)
                .map(|s| s.url)
                .collect();
            let sends = me
                .peers
                .iter()
                .filter(|p| !skip.contains(&normalize(&p.url)))
                .map(|peer| {
                    let me = me.clone();
                    async move {
                        let client = me.signed_in(peer).await?;
                        client
                            .post_json::<_, serde_json::Value>(
                                "/api/peers/nudge",
                                &serde_json::json!({ "from": me.instance }),
                            )
                            .await
                    }
                });
            for (peer, sent) in me.peers.iter().zip(futures::future::join_all(sends).await) {
                if let Err(e) = sent {
                    tracing::debug!("couldn't nudge {}: {e:#}", normalize(&peer.url));
                }
            }
        });
    }
}

// ─── Pulling ────────────────────────────────────────────────────────────────

/// What one pull from one peer brought over.
#[derive(Debug, Default, PartialEq)]
pub struct Pulled {
    pub entries: u64,
    pub bytes: u64,
    pub builds: u64,
    /// Entries whose last use there was later than here, carried over so
    /// retention ages them the same on both.
    pub touched: u64,
    /// What the peer holds, across every project.
    pub held: u64,
}

impl Pulled {
    fn anything_new(&self) -> bool {
        self.entries > 0 || self.builds > 0
    }
}

/// Keep pulling from every peer: at start, every `server.sync_every`, and
/// whenever nudged.
pub async fn replicate(state: AppState) {
    if state.peers.is_empty() {
        return;
    }
    let every = crate::cache::store::parse_duration(&state.config.server.sync_every)
        .unwrap_or(60)
        .max(MIN_EVERY) as u64;
    loop {
        let found_new = sync_once(&state).await;
        if found_new {
            state.peers.nudge();
        }
        tokio::select! {
            () = tokio::time::sleep(Duration::from_secs(every)) => {}
            () = state.peers.wake.notified() => {
                tokio::time::sleep(PULL_AFTER).await;
            }
        }
    }
}

/// One round: pull from each peer in turn. True when anything new arrived.
pub async fn sync_once(state: &AppState) -> bool {
    state.peers.expire();
    let mut found_new = false;
    for peer in &state.peers.peers {
        let url = normalize(&peer.url);
        let pulled = pull(state, peer).await;
        let at = crate::cache::store::now();
        match pulled {
            Ok(None) => state.peers.update(&url, |s| s.is_self = true),
            Ok(Some(pulled)) => {
                if pulled.anything_new() {
                    found_new = true;
                    tracing::info!(
                        "peers: pulled {} entr(ies) ({}) and {} build(s) from {url}",
                        pulled.entries,
                        crate::cache::store::human_size(pulled.bytes),
                        pulled.builds
                    );
                }
                state.peers.update(&url, |s| {
                    s.last_sync_at = Some(at.clone());
                    s.last_ok_at = Some(at);
                    s.last_error = None;
                    s.pulled_entries += pulled.entries;
                    s.pulled_bytes += pulled.bytes;
                    s.pulled_builds += pulled.builds;
                    s.entries = Some(pulled.held);
                });
            }
            Err(e) => {
                let why = format!("{e:#}");
                tracing::warn!("peers: couldn't sync with {url}: {why}");
                state.peers.update(&url, |s| {
                    s.last_sync_at = Some(at);
                    s.last_error = Some(why);
                });
            }
        }
    }
    found_new
}

/// Pull from one peer what this server lacks. `None` when the peer is this
/// server.
async fn pull(state: &AppState, peer: &Peer) -> Result<Option<Pulled>> {
    let probe = state.peers.client(peer)?;
    let health = probe.health().await?;
    if health.instance.as_deref() == Some(state.peers.instance.as_str()) {
        return Ok(None);
    }
    let client = state.peers.signed_in(peer).await?;

    let projects: Vec<super::projects::Project> = client.get_json("/api/projects").await?;

    // What this server holds, per project, so each listing is compared
    // without rereading the store.
    let mut held: HashMap<String, HashMap<String, crate::cache::store::Entry>> = HashMap::new();
    for entry in state.store.list()? {
        if state.store.has_artifacts(&entry.key)? {
            held.entry(entry.workspace.clone())
                .or_default()
                .insert(entry.key.clone(), entry);
        }
    }

    let mut pulled = Pulled::default();
    let mut errors = Vec::new();
    for project in projects {
        if state.peers.is_forgotten_project(&project.id) {
            continue;
        }
        if let Err(e) = state.projects.adopt(&project.id, &project.name) {
            errors.push(format!("project {}: {e:#}", project.id));
            continue;
        }
        let empty = HashMap::new();
        let here = held.get(&project.id).unwrap_or(&empty);
        if let Err(e) = pull_entries(state, &client, &project.id, here, &mut pulled).await {
            errors.push(format!("{}'s entries: {e:#}", project.name));
        }
        if let Err(e) = pull_builds(state, &client, &project.id, &mut pulled).await {
            errors.push(format!("{}'s builds: {e:#}", project.name));
        }
        match client.workflows(&project.id).await {
            Ok(records) if !records.is_empty() => {
                if let Err(e) = state.workflows.merge(&project.id, &records) {
                    errors.push(format!("{}'s workflows: {e:#}", project.name));
                }
            }
            Ok(_) => {}
            Err(e) => errors.push(format!("{}'s workflows: {e:#}", project.name)),
        }
    }
    if !errors.is_empty() {
        anyhow::bail!("{}", errors.join("; "));
    }
    Ok(Some(pulled))
}

async fn pull_entries(
    state: &AppState,
    client: &Client,
    project: &str,
    here: &HashMap<String, crate::cache::store::Entry>,
    pulled: &mut Pulled,
) -> Result<()> {
    let listed = client.list_entries(project).await?;
    pulled.held += listed.len() as u64;
    let mut failures = Vec::new();
    for listing in listed {
        let scoped = super::server::scoped_key(project, &listing.key);
        if let Some(local) = here.get(&scoped) {
            // Used more recently there: age it from then here too.
            if let Some(theirs) = listing.last_used_at.as_deref()
                && later(theirs, local.last_touched())
            {
                let mut entry = local.clone();
                entry.last_used_at = Some(theirs.to_string());
                state.store.write_manifest(&entry)?;
                pulled.touched += 1;
            }
            continue;
        }
        if state.peers.is_forgotten_entry(&scoped) {
            continue;
        }
        match copy_entry(state, client, project, &listing.key, &scoped).await {
            Ok(Some(size)) => {
                pulled.entries += 1;
                pulled.bytes += size;
            }
            Ok(None) => {}
            Err(e) => failures.push(format!("{}: {e:#}", listing.key)),
        }
    }
    match failures.len() {
        0 => Ok(()),
        n => anyhow::bail!("{n} couldn't be copied ({})", failures.join(", ")),
    }
}

/// Copy one entry into this server's store, artifacts first and manifest
/// last, as an upload would — so a copy cut short leaves no entry behind.
async fn copy_entry(
    state: &AppState,
    client: &Client,
    project: &str,
    key: &str,
    scoped: &str,
) -> Result<Option<u64>> {
    let Some(mut entry) = client.fetch_for_sync(project, key).await? else {
        // Evicted there since it was listed.
        return Ok(None);
    };
    let dir = state.store.artifact_dir(scoped);
    if let Err(e) = client.download(project, key, &entry, &dir).await {
        let _ = std::fs::remove_dir_all(&dir);
        return Err(e);
    }
    entry.key = scoped.to_string();
    entry.workspace = project.to_string();
    state.store.write_manifest(&entry)?;
    Ok(Some(entry.size))
}

/// Whether timestamp `a` is after `b`, comparing instants rather than text.
fn later(a: &str, b: &str) -> bool {
    match (
        chrono::DateTime::parse_from_rfc3339(a),
        chrono::DateTime::parse_from_rfc3339(b),
    ) {
        (Ok(a), Ok(b)) => a > b,
        _ => false,
    }
}

async fn pull_builds(
    state: &AppState,
    client: &Client,
    project: &str,
    pulled: &mut Pulled,
) -> Result<()> {
    #[derive(Deserialize)]
    struct Listing {
        #[serde(default)]
        builds: Vec<Summary>,
    }
    #[derive(Deserialize)]
    struct Summary {
        id: String,
    }
    #[derive(Deserialize)]
    struct One {
        build: super::builds::BuildRecord,
    }

    let limit = super::builds::MAX_BUILDS_PER_PROJECT;
    let listing: Listing = client
        .get_json(&format!("/api/projects/{project}/builds?limit={limit}"))
        .await?;
    let here: HashSet<String> = state
        .builds
        .list(project, limit)
        .into_iter()
        .map(|b| b.id)
        .collect();
    for summary in listing.builds {
        // A build id is a path segment below; anything that would need
        // escaping is skipped rather than sent half-understood.
        let safe = summary
            .id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
        if here.contains(&summary.id) || !safe {
            continue;
        }
        let one: One = client
            .get_json(&format!("/api/projects/{project}/builds/{}", summary.id))
            .await?;
        state.builds.add(project, &one.build)?;
        pulled.builds += 1;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(dir: &Path, peers: Vec<Peer>) -> AppState {
        AppState::new(super::super::ServerConfig {
            server: super::super::Listen {
                storage: dir.to_path_buf(),
                ..Default::default()
            },
            peers,
            ..Default::default()
        })
        .unwrap()
    }

    fn peer(url: &str) -> Peer {
        serde_json::from_value(serde_json::json!(url)).unwrap()
    }

    /// Two real servers: one is built to, the other pulls it all over — and
    /// a server listed among its own peers recognises itself.
    #[tokio::test]
    async fn a_server_pulls_projects_entries_builds_and_uses_from_its_peer() {
        let root = std::env::temp_dir().join(format!("ciab_peers_e2e_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let b_url = format!("http://{}", listener.local_addr().unwrap());
        let b = state(&root.join("b"), vec![peer(&b_url)]);
        let app = super::super::server::router(b.clone())
            .into_make_service_with_connect_info::<std::net::SocketAddr>();
        tokio::spawn(async move { axum::serve(listener, app).await });

        // Built to B, as a client would.
        let id = "0b6c1f0e-5c1a-4d2b-9a59-3f1f2c7d8e90";
        let client = Client::with_token(&b_url, true, None, None).unwrap();
        client.register("monorepo", Some(id)).await.unwrap();
        let built = root.join("built");
        std::fs::create_dir_all(built.join("dist")).unwrap();
        let payload = b"the built binary";
        std::fs::write(built.join("dist/app"), payload).unwrap();
        let entry = crate::cache::store::Entry {
            key: "abc123".into(),
            target: "build".into(),
            workspace: String::new(),
            inputs: vec![],
            outputs: vec![crate::cache::FileHash {
                path: "dist/app".into(),
                sha256: crate::cache::hash_bytes(payload),
                size: payload.len() as u64,
            }],
            env: Default::default(),
            upstream: Default::default(),
            created_at: crate::cache::store::now(),
            last_used_at: None,
            size: payload.len() as u64,
            duration_ms: 500,
        };
        client.upload(id, "abc123", &entry, &built).await.unwrap();
        let record: super::super::builds::BuildRecord = serde_json::from_str(
            r#"{"id": "b-1", "workflow": "build", "outcome": "success",
                "started_at": "2026-10-01T00:00:00Z", "finished_at": "2026-10-01T00:01:00Z",
                "duration_ms": 60000, "steps": []}"#,
        )
        .unwrap();
        client.report_build(id, &record).await.unwrap();

        // B finds itself in its own list and skips it.
        assert!(!sync_once(&b).await);
        assert!(b.peers.status()[0].is_self);

        // A pulls everything over.
        let a = state(&root.join("a"), vec![peer(&b_url)]);
        assert!(sync_once(&a).await, "{:?}", a.peers.status());
        let status = &a.peers.status()[0];
        assert_eq!(status.last_error, None);
        assert_eq!((status.pulled_entries, status.pulled_builds), (1, 1));
        assert_eq!(a.projects.get(id).unwrap().name, "monorepo");
        let scoped = super::super::server::scoped_key(id, "abc123");
        assert!(a.store.has_artifacts(&scoped).unwrap());
        assert_eq!(
            std::fs::read(a.store.artifact_dir(&scoped).join("dist/app")).unwrap(),
            payload
        );
        assert_eq!(a.store.get(&scoped).unwrap().unwrap().workspace, id);
        assert!(a.builds.get(id, "b-1").is_some());

        // In step: nothing more to pull.
        assert!(!sync_once(&a).await);

        // A use on B carries over, so both age the entry alike.
        client.touch(id, &["abc123".to_string()]).await.unwrap();
        sync_once(&a).await;
        assert!(
            a.store
                .get(&scoped)
                .unwrap()
                .unwrap()
                .last_used_at
                .is_some()
        );

        // Evicted on A on purpose: B's copy isn't pulled back.
        a.store.remove(&scoped).unwrap();
        a.peers.forget_entries([scoped.as_str()]);
        assert!(!sync_once(&a).await);
        assert!(a.store.get(&scoped).unwrap().is_none());

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_peer_is_a_url_or_a_url_with_settings() {
        let peers: Vec<Peer> = crate::format::from_str(
            "- http://a:8380\n- url: http://b:8380\n  username: sync\n  \
             password_env: PEER_TOKEN\n  tls_verify: false\n",
            crate::format::Format::Yaml,
        )
        .unwrap();
        assert_eq!(peers[0].url, "http://a:8380");
        assert!(peers[0].tls_verify && peers[0].username.is_none());
        assert_eq!(peers[1].username.as_deref(), Some("sync"));
        assert_eq!(peers[1].password_env.as_deref(), Some("PEER_TOKEN"));
        assert!(!peers[1].tls_verify);
    }

    #[test]
    fn timestamps_compare_as_instants() {
        assert!(later("2026-11-19T10:00:00+00:00", "2026-11-19T09:00:00Z"));
        assert!(!later("2026-11-19T10:00:00+02:00", "2026-11-19T09:00:00Z"));
        assert!(!later("garbage", "2026-11-19T09:00:00Z"));
    }

    #[test]
    fn removals_are_remembered_until_written_again() {
        let dir = std::env::temp_dir().join(format!("ciab_peers_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let peers = Peers::open(&dir, &[serde_json::from_str("\"http://b\"").unwrap()]).unwrap();
        peers.forget_entries(["p-abc"]);
        peers.forget_project("p");
        assert!(peers.is_forgotten_entry("p-abc") && peers.is_forgotten_project("p"));

        // Survives a restart, and so does the instance id.
        let again = Peers::open(&dir, &peers.peers).unwrap();
        assert_eq!(again.instance, peers.instance);
        assert!(again.is_forgotten_entry("p-abc"));
        again.remember_entry("p-abc");
        again.remember_project("p");
        assert!(!again.is_forgotten_entry("p-abc") && !again.is_forgotten_project("p"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
