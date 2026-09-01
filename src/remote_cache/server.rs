//! The remote cache's HTTP server.
//!
//! One axum app:
//!
//! ```text
//! /                                        the admin page (see `super::page`)
//! /api/health                              unauthenticated liveness + version
//! /api/auth/login  /logout  /me            sessions
//! /api/users…                              credentials, for an admin
//! /api/projects…                           project identity and cache traffic
//! /api/release  /api/release/{platform}    the ciabatta binaries it hands out
//! ```
//!
//! Artifacts move as raw bytes over `PUT`/`GET` on a path per file rather than
//! as one bundled archive. That means a client that already has four of five
//! outputs fetches one, and a partial upload leaves the manifest unwritten (and
//! so the entry invisible) rather than a corrupt archive. The manifest is
//! written last, by design — it is the thing that makes an entry real.

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::{Context, Result};
use axum::body::Bytes;
use axum::extract::{ConnectInfo, DefaultBodyLimit, Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::IntoResponse;
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::json;

use super::auth::{self, Identity, Sessions};
use super::projects::Registry;
use super::releases::Release;
use super::users::Users;
use super::{Listen, ServerConfig};
use crate::cache::store::{Entry, Store};

/// Cap on a single artifact upload. Generous — build outputs are routinely
/// hundreds of megabytes — but not unbounded, because an unbounded body limit
/// on a network service is a way to be handed an out-of-memory condition.
const MAX_ARTIFACT_BYTES: usize = 2 * 1024 * 1024 * 1024;

/// Everything a request handler needs.
#[derive(Clone, Debug)]
pub struct AppState {
    pub config: Arc<ServerConfig>,
    pub store: Arc<Store>,
    pub projects: Arc<Registry>,
    pub sessions: Arc<Sessions>,
    /// The binaries this server advertises, rescanned by the sweep so replacing
    /// a file on disk is all an operator has to do.
    pub release: Arc<std::sync::RwLock<Release>>,
    /// Credentials the server manages itself, alongside the config's own.
    pub users: Arc<Users>,
    /// What every checkout of every project has run, merged — see
    /// [`super::workflows`].
    pub workflows: Arc<super::workflows::Store>,
    pub started_at: String,
}

impl AppState {
    /// Build the state for a validated config.
    pub fn new(config: ServerConfig) -> Result<Self> {
        // Fail here rather than at somebody's first login.
        config.auth.mode()?;
        config.auth.session_seconds().context(
            "auth.session_ttl is not a duration ciabatta understands (try \"30d\" or \"12h\")",
        )?;

        let store = Store::at(config.server.storage.join("cache"))?;
        let projects = Registry::open(&config.server.storage)?;
        let users = Users::open(&config.server.storage)?;
        let workflows = super::workflows::Store::open(&config.server.storage)?;
        let release = config.releases.scan();

        Ok(AppState {
            store: Arc::new(store),
            projects: Arc::new(projects),
            users: Arc::new(users),
            workflows: Arc::new(workflows),
            sessions: Arc::new(Sessions::open(&config.server.storage)),
            release: Arc::new(std::sync::RwLock::new(release)),
            started_at: crate::cache::store::now(),
            config: Arc::new(config),
        })
    }

    /// Who's making this request, or a 401.
    ///
    /// An `open`-mode server skips the check entirely — there is no point
    /// issuing sessions nobody's identity depends on.
    fn identify(&self, headers: &header::HeaderMap) -> Result<Identity, ApiError> {
        if matches!(self.config.auth.mode(), Ok(auth::Mode::Open)) {
            return Ok(Identity::anonymous());
        }

        let token = bearer(headers).ok_or_else(|| {
            ApiError::coded(
                StatusCode::UNAUTHORIZED,
                CODE_NO_CREDENTIAL,
                "This cache requires authentication. Run `ciabatta remote-cache login <URL>`.",
            )
        })?;

        match self.sessions.lookup(&token) {
            auth::Lookup::Live(identity) => Ok(identity),
            auth::Lookup::Expired { at } => Err(ApiError::coded(
                StatusCode::UNAUTHORIZED,
                CODE_SESSION_EXPIRED,
                format!(
                    "Your session expired at {at}. Run `ciabatta remote-cache login <URL>` again."
                ),
            )),
            // Not the same thing as expiry, and saying so saves somebody
            // staring at a credential whose own expiry date is months away.
            auth::Lookup::Unknown => Err(ApiError::coded(
                StatusCode::UNAUTHORIZED,
                CODE_SESSION_UNKNOWN,
                "This cache has no record of your session — it was revoked, or it was \
                 issued by a different server (check the URL and the host name you \
                 logged in with). Run `ciabatta remote-cache login <URL>` again.",
            )),
        }
    }

    /// Who's making this request, and are they allowed to change anything?
    fn identify_writer(&self, headers: &header::HeaderMap) -> Result<Identity, ApiError> {
        let identity = self.identify(headers)?;
        if !identity.can_write {
            return Err(ApiError::new(
                StatusCode::FORBIDDEN,
                format!("{} has read-only access to this cache.", identity.name),
            ));
        }
        Ok(identity)
    }

    /// Who's making this request, and may they manage users?
    ///
    /// On an `open` server: anyone, because open mode already means "I trust
    /// whoever is on this network", and refusing would leave no way to mint the
    /// first credential when locking the cache down. On an authenticated one:
    /// an admin, and only an admin.
    fn identify_admin(&self, headers: &header::HeaderMap) -> Result<Identity, ApiError> {
        if matches!(self.config.auth.mode(), Ok(auth::Mode::Open)) {
            return Ok(Identity::anonymous());
        }

        let identity = self.identify(headers)?;
        if !identity.is_admin {
            return Err(ApiError::new(
                StatusCode::FORBIDDEN,
                format!(
                    "{} isn't an admin on this cache. Add `admin: true` to a user \
                     under `auth.users` in the server's config to make one.",
                    identity.name
                ),
            ));
        }
        Ok(identity)
    }
}

/// Pull a bearer token out of the `Authorization` header.
fn bearer(headers: &header::HeaderMap) -> Option<String> {
    headers
        .get(header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
        .map(str::to_string)
}

// ─── Errors ─────────────────────────────────────────────────────────────────

/// A route failure with a status and a message the client can show verbatim.
#[derive(Debug)]
pub struct ApiError {
    status: StatusCode,
    message: String,
    /// A stable machine-readable tag, when the client can *do* something about
    /// this particular failure.
    ///
    /// The message is for a person and is free to be rewritten; a client that
    /// wants to act on a failure — clear a credential it now knows is dead,
    /// say — must not have to match on English prose to do it.
    code: Option<&'static str>,
}

/// The session the caller presented was issued here, and its time is up.
pub const CODE_SESSION_EXPIRED: &str = "session_expired";
/// This server has no record of the session at all: revoked, or issued
/// somewhere else. Either way the credential holding it is dead for good, and
/// a client should stop presenting it.
pub const CODE_SESSION_UNKNOWN: &str = "session_unknown";
/// No credential was presented at all.
pub const CODE_NO_CREDENTIAL: &str = "no_credential";

impl ApiError {
    fn new(status: StatusCode, message: impl std::fmt::Display) -> Self {
        ApiError {
            status,
            message: message.to_string(),
            code: None,
        }
    }

    /// Tag this failure so a client can act on it without reading the message.
    fn coded(status: StatusCode, code: &'static str, message: impl std::fmt::Display) -> Self {
        ApiError {
            code: Some(code),
            ..Self::new(status, message)
        }
    }

    fn not_found(message: impl std::fmt::Display) -> Self {
        Self::new(StatusCode::NOT_FOUND, message)
    }

    fn bad_request(message: impl std::fmt::Display) -> Self {
        Self::new(StatusCode::BAD_REQUEST, message)
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self {
        ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}"))
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> axum::response::Response {
        // Every route failure passes through here, which makes it the one place
        // that can promise the reason reached the log as well as the client. A
        // 5xx is the server's own fault and is logged as an error; a 4xx is the
        // client's and is logged as a warning, because a wall of 404s from one
        // misconfigured runner should not read as the server falling over.
        if self.status.is_server_error() {
            tracing::error!(status = self.status.as_u16(), "{}", self.message);
        } else {
            tracing::warn!(status = self.status.as_u16(), "{}", self.message);
        }
        let mut body = json!({ "error": self.message });
        if let Some(code) = self.code {
            body["code"] = json!(code);
        }
        (self.status, Json(body)).into_response()
    }
}

type ApiResult<T> = std::result::Result<T, ApiError>;

// ─── Router ─────────────────────────────────────────────────────────────────

/// Assemble the router.
pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/api/health", get(health))
        .route("/api/auth/login", post(login))
        .route("/api/auth/logout", post(logout))
        .route("/api/me", get(me))
        .route("/api/stats", get(stats))
        .route("/api/projects", get(list_projects).post(register_project))
        .route("/api/projects/{id}", delete(forget_project))
        .route(
            "/api/projects/{id}/workflows",
            get(get_workflows).post(report_workflows),
        )
        .route("/api/projects/{id}/cache/touch", post(touch_entries))
        .route(
            "/api/projects/{id}/cache/{key}",
            get(get_entry).put(put_entry),
        )
        .route(
            "/api/projects/{id}/cache/{key}/artifact",
            get(get_artifact).put(put_artifact),
        )
        .route("/api/users", get(list_users).post(create_user))
        .route("/api/users/{name}", delete(revoke_user))
        .route("/api/release", get(get_release))
        .route("/api/release/{platform}", get(download_release))
        // The admin page, last so no API path can be shadowed by it.
        .route("/", get(admin_page))
        .layer(DefaultBodyLimit::max(MAX_ARTIFACT_BYTES))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            log_request,
        ))
        .layer(tower_http::trace::TraceLayer::new_for_http())
        .with_state(state)
}

// ─── Request logging ────────────────────────────────────────────────────────

/// Header names whose values must never reach a log file.
///
/// Matched case-insensitively against the whole name, not by prefix: the point
/// is a short, auditable list rather than a heuristic that quietly stops
/// covering a header somebody adds later.
const REDACTED_HEADERS: &[&str] = &[
    "authorization",
    "proxy-authorization",
    "cookie",
    "set-cookie",
    "x-api-key",
    "x-auth-token",
];

/// Log every request as it arrives and every response as it leaves.
///
/// A shared cache is debugged from the outside — "my CI runner gets a 401",
/// "this key 404s but I uploaded it" — and answering those without the traffic
/// in front of you is guesswork. So the arrival line carries the method, the
/// full path and query, the peer, and (unless turned off) the request's headers
/// with credentials redacted; the departure line carries the status, the size,
/// and how long it took.
///
/// Both lines carry the same `req` id so they can be paired in a busy log, and
/// both are `info` — an operator who started a server expects to see it serving.
/// Turn the pair off with `log.requests: false`, or drop the whole server to
/// `CIABATTA_LOG=ciabatta=warn`.
async fn log_request(
    State(state): State<AppState>,
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    if !state.config.log.requests {
        return next.run(request).await;
    }

    // Read out of the extensions rather than extracted, so a router driven
    // without a listener — every test in this file, and any future in-process
    // caller — logs `-` for the peer instead of failing the request outright.
    // A log line must never be able to break the thing it is describing.
    let peer = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|ConnectInfo(addr)| addr.to_string())
        .unwrap_or_else(|| "-".to_string());

    // Monotonically increasing per process: enough to pair two lines, and
    // cheaper than a UUID on a path that runs for every artifact byte served.
    static NEXT_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let id = NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

    let method = request.method().clone();
    // `path_and_query` rather than the path alone: `?project=…` is exactly the
    // sort of thing that turns out to be the bug.
    let target = request
        .uri()
        .path_and_query()
        .map(|pq| pq.to_string())
        .unwrap_or_else(|| request.uri().path().to_string());

    if state.config.log.headers {
        tracing::info!(
            req = id,
            %peer,
            "→ {method} {target} {{{}}}",
            render_headers(request.headers())
        );
    } else {
        tracing::info!(req = id, %peer, "→ {method} {target}");
    }

    let started = std::time::Instant::now();
    let response = next.run(request).await;
    let elapsed = started.elapsed();

    let status = response.status();
    // Streamed and empty responses carry no Content-Length; saying nothing
    // beats printing a dash somebody has to work out the meaning of.
    let size = response
        .headers()
        .get(header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .map(|bytes| format!(", {bytes} bytes"))
        .unwrap_or_default();

    // The status the client got is the fact worth reading, so a failing
    // response is logged at a level that survives a quieter filter — even
    // though `ApiError` has already explained the reason above it.
    let line = format!(
        "← {} {method} {target} in {:.1}ms{size}",
        status.as_u16(),
        elapsed.as_secs_f64() * 1000.0,
    );
    if status.is_server_error() {
        tracing::error!(req = id, "{line}");
    } else if status.is_client_error() {
        tracing::warn!(req = id, "{line}");
    } else {
        tracing::info!(req = id, "{line}");
    }

    response
}

/// The request's headers as one `name=value` list, with credentials redacted.
fn render_headers(headers: &HeaderMap) -> String {
    headers
        .iter()
        .map(|(name, value)| {
            let name = name.as_str();
            if REDACTED_HEADERS
                .iter()
                .any(|h| name.eq_ignore_ascii_case(h))
            {
                // The *shape* of the credential is the useful part — "they sent
                // a bearer token" versus "they sent nothing at all" is most of
                // a 401 investigation — so keep the scheme and drop the secret.
                let scheme = value
                    .to_str()
                    .ok()
                    .and_then(|v| v.split_whitespace().next().map(str::to_string))
                    .filter(|scheme| scheme.chars().all(|c| c.is_ascii_alphabetic()))
                    .map(|scheme| format!("{scheme} "))
                    .unwrap_or_default();
                return format!("{name}={scheme}<redacted>");
            }
            match value.to_str() {
                Ok(value) => format!("{name}={value}"),
                Err(_) => format!("{name}=<{} bytes>", value.len()),
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

// ─── Health, identity, stats ────────────────────────────────────────────────

/// Liveness, plus the version this server advertises.
///
/// Unauthenticated on purpose: a client needs to be able to see the update
/// notice before it has logged in, and a probe shouldn't need a credential.
async fn health(State(state): State<AppState>) -> Json<serde_json::Value> {
    let release = state.release.read().unwrap();
    Json(json!({
        "ok": true,
        "service": "ciabatta-remote-cache",
        "version": env!("CARGO_PKG_VERSION"),
        "release": *release,
        "auth": state.config.auth.mode,
        "started_at": state.started_at,
    }))
}

#[derive(Deserialize)]
struct LoginPayload {
    #[serde(default)]
    username: String,
    #[serde(default)]
    password: String,
}

/// Exchange a username and password (or token) for a session.
async fn login(
    State(state): State<AppState>,
    Json(payload): Json<LoginPayload>,
) -> ApiResult<Json<serde_json::Value>> {
    let identity = auth::authenticate(
        &state.config.auth,
        &state.users.credentials(),
        &payload.username,
        &payload.password,
    )
    .await
    // Every authentication failure is a 401 with the backend's own wording;
    // the backends are careful not to say which half was wrong.
    .map_err(|e| ApiError::new(StatusCode::UNAUTHORIZED, format!("{e:#}")))?;

    let ttl = state
        .config
        .auth
        .session_seconds()
        .map_err(ApiError::from)?;
    let (token, session) = state.sessions.issue(identity.clone(), ttl);

    tracing::info!("{} logged in", identity.name);
    Ok(Json(json!({
        "token": token,
        "expires_at": session.expires_at,
        "user": identity,
    })))
}

/// End the caller's session.
async fn logout(
    State(state): State<AppState>,
    headers: header::HeaderMap,
) -> Json<serde_json::Value> {
    let revoked = bearer(&headers).is_some_and(|t| state.sessions.revoke(&t));
    Json(json!({ "ok": revoked }))
}

/// Who the caller is, as this server sees them.
async fn me(
    State(state): State<AppState>,
    headers: header::HeaderMap,
) -> ApiResult<Json<Identity>> {
    Ok(Json(state.identify(&headers)?))
}

/// What the cache is holding and how well it's doing.
async fn stats(
    State(state): State<AppState>,
    headers: header::HeaderMap,
) -> ApiResult<Json<serde_json::Value>> {
    state.identify(&headers)?;

    let store = state.store.stats()?;
    let totals = state.projects.totals();
    // The whole point of reporting history here: one machine can only say what
    // *it* ran, and the server is the only thing that sees everybody.
    let workflows = state.workflows.all();
    let stale_after = state.config.staleness();
    let stale: Vec<serde_json::Value> = workflows
        .iter()
        .flat_map(|(project, records)| {
            records
                .iter()
                .filter(|record| record.is_stale(stale_after))
                .map(move |record| {
                    json!({
                        "project": project,
                        "workflow": record.id(),
                        "last_run_at": record.last_run_at,
                        "days": record.days_since(),
                        "runs": record.runs,
                    })
                })
        })
        .collect();
    let projects: Vec<serde_json::Value> = state
        .projects
        .list()
        .into_iter()
        .map(|project| {
            let counters = state.projects.counters(&project.id);
            let runs = workflows.get(&project.id);
            json!({
                "project": project,
                "counters": counters,
                "hit_rate": counters.hit_rate(),
                "entries": store.by_workspace.get(&project.id).copied().unwrap_or(0),
                "workflows": runs.map(Vec::len).unwrap_or(0),
                "stale_workflows": runs
                    .map(|records| {
                        records.iter().filter(|r| r.is_stale(stale_after)).count()
                    })
                    .unwrap_or(0),
            })
        })
        .collect();

    Ok(Json(json!({
        "storage": {
            "entries": store.entries,
            "bytes": store.size,
            "human": crate::cache::store::human_size(store.size),
            "oldest": store.oldest,
            "newest": store.newest,
            "path": state.config.server.storage,
        },
        "counters": totals,
        "hit_rate": totals.hit_rate(),
        "retention": {
            "policy": state.config.retention,
            "description": state.config.retention.describe(),
        },
        "workflows": {
            "tracked": workflows.values().map(Vec::len).sum::<usize>(),
            "stale_after": state.config.staleness_raw(),
            "stale": stale,
        },
        "sessions": state.sessions.live_count(),
        "release": *state.release.read().unwrap(),
        "started_at": state.started_at,
        "projects": projects,
    })))
}

// ─── Projects ───────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct RegisterPayload {
    name: String,
    /// The id this client already holds, when it has one.
    #[serde(default)]
    id: Option<String>,
}

async fn list_projects(
    State(state): State<AppState>,
    headers: header::HeaderMap,
) -> ApiResult<Json<Vec<super::projects::Project>>> {
    state.identify(&headers)?;
    Ok(Json(state.projects.list()))
}

/// Resolve a project, minting an id when this is the first time.
async fn register_project(
    State(state): State<AppState>,
    headers: header::HeaderMap,
    Json(payload): Json<RegisterPayload>,
) -> ApiResult<Json<super::projects::Project>> {
    let identity = state.identify(&headers)?;
    let project = state
        .projects
        .resolve(payload.id.as_deref(), &payload.name, Some(&identity.name))
        .map_err(ApiError::bad_request)?;
    Ok(Json(project))
}

/// Forget a project and everything cached for it.
async fn forget_project(
    State(state): State<AppState>,
    headers: header::HeaderMap,
    Path(id): Path<String>,
) -> ApiResult<Json<serde_json::Value>> {
    state.identify_writer(&headers)?;

    // Artifacts first: a registry entry with orphaned artifacts is tidier than
    // artifacts nothing knows about.
    let mut removed = 0;
    for entry in state.store.list()? {
        if entry.workspace == id {
            state.store.remove(&entry.key)?;
            removed += 1;
        }
    }

    // Its run history goes with it. Leaving that behind would mean a project
    // re-registered under a new id inherits nothing, while the old records sit
    // there forever counting towards a staleness report for a project that no
    // longer exists.
    if let Err(e) = state.workflows.forget(&id) {
        tracing::warn!("couldn't forget {id}'s workflow history: {e:#}");
    }

    if !state.projects.forget(&id)? {
        return Err(ApiError::not_found(format!("No project {id}")));
    }
    Ok(Json(json!({ "ok": true, "entries_removed": removed })))
}

// ─── Cache traffic ──────────────────────────────────────────────────────────

/// Check a project exists before touching its cache.
fn require_project(state: &AppState, id: &str) -> ApiResult<()> {
    state
        .projects
        .get(id)
        .map(|_| ())
        .ok_or_else(|| ApiError::not_found(format!("Unknown project '{id}'")))
}

/// Look a key up. A miss is a 404 — that's the whole protocol.
async fn get_entry(
    State(state): State<AppState>,
    headers: header::HeaderMap,
    Path((id, key)): Path<(String, String)>,
) -> ApiResult<Json<Entry>> {
    state.identify(&headers)?;
    require_project(&state, &id)?;

    let scoped = scoped_key(&id, &key);
    match state.store.get(&scoped)? {
        // An entry whose artifacts have gone is a miss, not a hit that fails
        // halfway through a download.
        Some(entry) if state.store.has_artifacts(&scoped)? => {
            state.projects.record_hit(&id, entry.size);
            state.store.touch(&scoped)?;
            Ok(Json(entry))
        }
        _ => {
            state.projects.record_miss(&id);
            Err(ApiError::not_found(format!("No cache entry for {key}")))
        }
    }
}

#[derive(Deserialize)]
struct TouchPayload {
    #[serde(default)]
    keys: Vec<String>,
}

/// What a checkout reports about the workflows it just ran.
#[derive(Debug, serde::Deserialize)]
struct WorkflowsPayload {
    #[serde(default)]
    workflows: Vec<crate::run::history::Record>,
}

/// Mark entries as still in use, without downloading them.
///
/// Retention ages an artifact from when it was last *used*, so that the thing
/// everyone depends on isn't evicted for being old. That only works if the
/// server hears about the uses — and after the first download it stops hearing
/// about most of them, because the client mirrors the entry into its local
/// store and every later build is answered from there without the network being
/// touched at all. The artifact the whole team relies on daily therefore looks,
/// from here, like one nobody has wanted since the day it was built.
///
/// So a run reports the keys it reused locally, in one request at the end
/// rather than one per step, and they age from now.
///
/// Deliberately allowed to any authenticated caller rather than to writers
/// only: keeping an artifact you are actively using alive is not a way to
/// change what anybody else builds, and a read-only CI runner that couldn't do
/// it would watch the cache it depends on expire underneath it.
async fn touch_entries(
    State(state): State<AppState>,
    headers: header::HeaderMap,
    Path(id): Path<String>,
    Json(payload): Json<TouchPayload>,
) -> ApiResult<Json<serde_json::Value>> {
    state.identify(&headers)?;
    require_project(&state, &id)?;

    // A key this server doesn't have is not an error: the client is reporting
    // what *it* reused, and some of that may only ever have existed locally.
    let mut refreshed = 0usize;
    for key in payload.keys.iter().take(MAX_TOUCH_KEYS) {
        let scoped = scoped_key(&id, key);
        if state.store.get(&scoped)?.is_some() {
            state.store.touch(&scoped)?;
            refreshed += 1;
        }
    }

    Ok(Json(json!({ "ok": true, "refreshed": refreshed })))
}

/// Fold a checkout's workflow history into the project's picture.
///
/// Allowed to any authenticated caller, not to writers only — for the same
/// reason `touch` is. Saying "somebody ran this workflow" changes nothing about
/// what anybody builds, and a read-only CI runner is exactly the caller whose
/// runs most want counting.
async fn report_workflows(
    State(state): State<AppState>,
    headers: header::HeaderMap,
    Path(id): Path<String>,
    Json(payload): Json<WorkflowsPayload>,
) -> ApiResult<Json<serde_json::Value>> {
    state.identify(&headers)?;
    require_project(&state, &id)?;

    let known = state
        .workflows
        .merge(&id, &payload.workflows)
        .map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(json!({ "ok": true, "workflows": known })))
}

/// What every checkout of this project has run.
///
/// The point of the whole exercise: a workflow you have not run since March may
/// be the one CI runs hourly, and only the merged picture can tell the two
/// apart.
async fn get_workflows(
    State(state): State<AppState>,
    headers: header::HeaderMap,
    Path(id): Path<String>,
) -> ApiResult<Json<serde_json::Value>> {
    state.identify(&headers)?;
    require_project(&state, &id)?;
    Ok(Json(
        json!({ "workflows": state.workflows.for_project(&id) }),
    ))
}

/// How many keys one keep-alive request may refresh.
///
/// A graph has as many keys as it has steps, and this bounds the work a single
/// request can ask of the server no matter what a client sends.
const MAX_TOUCH_KEYS: usize = 2_000;

/// Store an entry's manifest.
///
/// Called *after* every artifact has been uploaded, so a half-finished upload
/// leaves no entry at all rather than one promising files that aren't there.
async fn put_entry(
    State(state): State<AppState>,
    headers: header::HeaderMap,
    Path((id, key)): Path<(String, String)>,
    Json(mut entry): Json<Entry>,
) -> ApiResult<Json<serde_json::Value>> {
    state.identify_writer(&headers)?;
    require_project(&state, &id)?;

    let scoped = scoped_key(&id, &key);
    entry.key = scoped.clone();
    // The workspace field is the project id server-side: it's what scopes the
    // stats and what `forget_project` sweeps on.
    entry.workspace = id.clone();

    // Refuse a manifest whose artifacts weren't all uploaded — otherwise the
    // next client to hit this key gets a 404 mid-restore.
    let artifacts = state.store.artifact_dir(&scoped);
    let missing: Vec<&str> = entry
        .outputs
        .iter()
        .filter(|o| !artifacts.join(&o.path).is_file())
        .map(|o| o.path.as_str())
        .collect();
    if !missing.is_empty() {
        return Err(ApiError::bad_request(format!(
            "Upload the artifacts before the manifest — missing: {}",
            missing.join(", ")
        )));
    }

    state.store.write_manifest(&entry)?;
    state.projects.record_upload(&id, entry.size);
    Ok(Json(json!({ "ok": true, "key": key })))
}

#[derive(Deserialize)]
struct ArtifactQuery {
    /// The output file's path, relative to the workspace directory.
    path: String,
}

/// Download one of an entry's output files.
async fn get_artifact(
    State(state): State<AppState>,
    headers: header::HeaderMap,
    Path((id, key)): Path<(String, String)>,
    Query(query): Query<ArtifactQuery>,
) -> ApiResult<impl IntoResponse> {
    state.identify(&headers)?;
    require_project(&state, &id)?;

    let rel = safe_relative(&query.path)?;
    let path = state.store.artifact_dir(&scoped_key(&id, &key)).join(&rel);
    let bytes = std::fs::read(&path)
        .map_err(|_| ApiError::not_found(format!("No cached file '{}' under {key}", query.path)))?;

    Ok(([(header::CONTENT_TYPE, "application/octet-stream")], bytes))
}

/// Upload one of an entry's output files.
async fn put_artifact(
    State(state): State<AppState>,
    headers: header::HeaderMap,
    Path((id, key)): Path<(String, String)>,
    Query(query): Query<ArtifactQuery>,
    body: Bytes,
) -> ApiResult<Json<serde_json::Value>> {
    state.identify_writer(&headers)?;
    require_project(&state, &id)?;

    let rel = safe_relative(&query.path)?;
    let path = state.store.artifact_dir(&scoped_key(&id, &key)).join(&rel);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create {}", parent.display()))
            .map_err(ApiError::from)?;
    }
    std::fs::write(&path, &body)
        .with_context(|| format!("Failed to write {}", path.display()))
        .map_err(ApiError::from)?;

    Ok(Json(json!({
        "ok": true,
        "bytes": body.len(),
        "sha256": crate::cache::hash_bytes(&body),
    })))
}

/// Namespace a cache key by project.
///
/// Two projects with the same inputs would otherwise collide in one flat store
/// — which is exactly the accidental sharing the project id exists to prevent.
fn scoped_key(project: &str, key: &str) -> String {
    format!("{project}-{key}")
}

/// Validate a client-supplied artifact path.
///
/// Every component is checked rather than the string being scanned for `..`,
/// because this path is joined onto a server directory: a client that can send
/// `../../etc/whatever` can read or write anything the server user can. The
/// path must be relative, and must contain nothing but plain names.
fn safe_relative(raw: &str) -> ApiResult<std::path::PathBuf> {
    use std::path::Component;

    let path = std::path::Path::new(raw);
    let mut out = std::path::PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => out.push(part),
            _ => {
                return Err(ApiError::bad_request(format!(
                    "'{raw}' is not a plain relative path"
                )));
            }
        }
    }
    if out.as_os_str().is_empty() {
        return Err(ApiError::bad_request("an artifact path can't be empty"));
    }
    Ok(out)
}

/// The admin page.
///
/// Unauthenticated, because it has to be: on a server that wants credentials,
/// this is where you sign in to get one. The page itself is inert — every
/// action it offers goes through the API, which does check.
async fn admin_page() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        super::page::ADMIN_PAGE,
    )
}

// ─── Users ──────────────────────────────────────────────────────────────────

/// Every credential the server knows about, from the config and its own list.
///
/// Names and flags only — never tokens, and never their hashes. A hash is not a
/// password, but it is still the one thing an attacker would want from this
/// endpoint, and nothing here needs it.
async fn list_users(
    State(state): State<AppState>,
    headers: header::HeaderMap,
) -> ApiResult<Json<serde_json::Value>> {
    state.identify_admin(&headers)?;
    let from_config = &state.config.auth.users;

    Ok(Json(json!({
        "users": state.users.summaries(from_config),
        "mode": state.config.auth.mode,
        // A `token`-mode server with no credentials can't be logged into, so
        // the page needs to say so rather than showing a login that can't work.
        "locked_out": !matches!(state.config.auth.mode(), Ok(auth::Mode::Open))
            && state.users.is_empty(from_config),
    })))
}

#[derive(Deserialize)]
struct CreateUserPayload {
    name: String,
    #[serde(default)]
    read_only: bool,
    #[serde(default)]
    admin: bool,
}

/// Mint a credential. The token comes back once and is never recoverable.
async fn create_user(
    State(state): State<AppState>,
    headers: header::HeaderMap,
    Json(payload): Json<CreateUserPayload>,
) -> ApiResult<Json<serde_json::Value>> {
    let identity = state.identify_admin(&headers)?;
    let open = matches!(state.config.auth.mode(), Ok(auth::Mode::Open));

    // An open server may mint users — that's how you migrate to token mode —
    // but never an admin. Otherwise somebody could grant themselves lasting
    // control while the door was open and keep it after it was shut.
    if payload.admin && open {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "This cache is in `open` mode, so anyone who can reach it can create \
             users — which is exactly why none of them can be an admin. Declare \
             an admin under `auth.users` in the server's config, set \
             `auth.mode: token`, and restart.",
        ));
    }

    let (token, user) = state
        .users
        .create(
            &payload.name,
            payload.read_only,
            payload.admin,
            Some(&identity.name),
            &state.config.auth.users,
        )
        .map_err(ApiError::bad_request)?;

    tracing::info!("{} created the user {}", identity.name, user.name);
    Ok(Json(json!({
        "user": user,
        "token": token,
        "login": format!("ciabatta remote-cache login <URL> --username {}", user.name),
        "note": "This is the only time the token is shown. It isn't stored — only its \
                 hash is — so if it's lost, the credential has to be reissued.",
    })))
}

/// Revoke a server-managed credential.
async fn revoke_user(
    State(state): State<AppState>,
    headers: header::HeaderMap,
    Path(name): Path<String>,
) -> ApiResult<Json<serde_json::Value>> {
    let identity = state.identify_admin(&headers)?;

    let removed = state
        .users
        .remove(&name, &state.config.auth.users)
        .map_err(ApiError::bad_request)?;
    if !removed {
        return Err(ApiError::not_found(format!("No user '{name}'")));
    }

    tracing::info!("{} revoked the user {name}", identity.name);
    Ok(Json(json!({ "ok": true })))
}

// ─── Releases ───────────────────────────────────────────────────────────────

/// What ciabatta builds this server hands out.
async fn get_release(State(state): State<AppState>) -> Json<Release> {
    Json(state.release.read().unwrap().clone())
}

/// Download the binary for a platform.
async fn download_release(
    State(state): State<AppState>,
    Path(platform): Path<String>,
) -> ApiResult<impl IntoResponse> {
    let expected = state
        .release
        .read()
        .unwrap()
        .build(&platform)
        .cloned()
        .ok_or_else(|| {
            ApiError::not_found(format!("This cache has no ciabatta build for {platform}"))
        })?;

    let path = state
        .config
        .releases
        .binaries
        .get(&platform)
        .ok_or_else(|| ApiError::not_found(format!("No binary configured for {platform}")))?;

    let bytes = std::fs::read(path)
        .with_context(|| format!("Failed to read {}", path.display()))
        .map_err(ApiError::from)?;

    // The file may have been replaced between the last scan and this read. The
    // client verifies against whatever `/api/release` told it, so serving bytes
    // that don't match that would just fail on the client with a confusing
    // message — better to say so here.
    let actual = crate::cache::hash_bytes(&bytes);
    if actual != expected.sha256 {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "The binary changed while it was being served — try again in a moment.",
        ));
    }

    Ok((
        [
            (header::CONTENT_TYPE, "application/octet-stream".to_string()),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"ciabatta-{platform}\""),
            ),
        ],
        bytes,
    ))
}

// ─── Serving ────────────────────────────────────────────────────────────────

/// Bind and serve until interrupted, sweeping expired artifacts as it goes.
pub async fn serve(config: ServerConfig) -> Result<()> {
    let listen: Listen = config.server.clone();
    let state = AppState::new(config)?;

    let addr: SocketAddr = state
        .config
        .address()
        .parse()
        .with_context(|| format!("Invalid bind address {}", state.config.address()))?;

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("Failed to bind {addr}"))?;

    announce(&state, &listen);
    tokio::spawn(sweeper(state.clone(), listen.sweep_every.clone()));

    // `into_make_service_with_connect_info` rather than a plain service: the
    // peer address is half of what makes a request log worth having, since
    // "which runner is doing this?" is the first question about any of it.
    axum::serve(
        listener,
        router(state).into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(async {
        let _ = tokio::signal::ctrl_c().await;
        tracing::info!("shutting the remote cache down");
    })
    .await
    .context("The remote cache server stopped unexpectedly")
}

/// Print what an operator needs to see on startup, including the two things
/// most likely to be wrong: an open server on a public interface, and a
/// retention policy that will never evict anything.
fn announce(state: &AppState, listen: &Listen) {
    println!(
        "ciabatta remote cache listening on http://{}",
        state.config.address()
    );
    println!("  storage:   {}", listen.storage.display());
    println!("  retention: {}", state.config.retention.describe());
    println!("  auth:      {}", state.config.auth.mode);
    println!("  logging:   {}", describe_logging(&state.config.log));

    let release = state.release.read().unwrap();
    if release.is_empty() {
        println!("  releases:  none configured");
    } else {
        let platforms: Vec<&str> = release.builds.keys().map(|s| s.as_str()).collect();
        println!(
            "  releases:  {} for {}",
            release.version,
            platforms.join(", ")
        );
    }

    if matches!(state.config.auth.mode(), Ok(auth::Mode::Open)) && listen.bind != "127.0.0.1" {
        println!();
        println!(
            "  ⚠ auth.mode is 'open' on {}: anyone who can reach this port can read",
            listen.bind
        );
        println!("    and overwrite cached build artifacts. Set auth.mode before exposing it.");
    }
    println!();
    println!(
        "Connect with: ciabatta remote-cache login http://<this-host>:{}",
        listen.port
    );
}

/// What the request log will and won't contain, in one line.
fn describe_logging(log: &super::LogConfig) -> String {
    if !log.requests {
        return "off (set log.requests: true to trace requests)".to_string();
    }
    let mut parts = vec!["requests"];
    if log.headers {
        parts.push("headers (credentials redacted)");
    }
    parts.join(" · ")
}

/// Periodically apply the retention policy and rescan the advertised binaries.
///
/// Rescanning is what lets an operator upgrade their team by copying a file:
/// no restart, no config edit, and the next client to check in is told.
async fn sweeper(state: AppState, every: String) {
    let interval = crate::cache::store::parse_duration(&every)
        .unwrap_or(3600)
        .max(60);
    let mut ticker = tokio::time::interval(std::time::Duration::from_secs(interval as u64));
    // The first tick fires immediately; skip it so startup isn't a sweep.
    ticker.tick().await;

    loop {
        ticker.tick().await;

        match state.store.prune(&state.config.retention) {
            Ok(pruned) if !pruned.is_empty() => tracing::info!(
                "retention: evicted {} entr(ies), reclaimed {}",
                pruned.removed.len(),
                crate::cache::store::human_size(pruned.freed)
            ),
            Ok(_) => {}
            Err(e) => tracing::warn!("retention sweep failed: {e:#}"),
        }

        // Sessions age out the same way artifacts do, and this is the one
        // place already asking "what's past its time?" on a timer.
        match state.sessions.prune_expired() {
            0 => {}
            dropped => tracing::info!("retention: dropped {dropped} expired session(s)"),
        }

        let rescanned = state.config.releases.scan();
        let mut current = state.release.write().unwrap();
        if rescanned.builds != current.builds {
            tracing::info!(
                "the advertised ciabatta build changed — clients will be told on their next request"
            );
            *current = rescanned;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("ciab_rcsrv_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn state_with(config: ServerConfig) -> AppState {
        AppState::new(config).expect("state builds")
    }

    fn open_state(dir: &std::path::Path) -> AppState {
        state_with(ServerConfig {
            server: Listen {
                storage: dir.to_path_buf(),
                ..Default::default()
            },
            ..Default::default()
        })
    }

    async fn json(response: axum::response::Response) -> serde_json::Value {
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    fn get(uri: &str) -> Request<Body> {
        Request::builder().uri(uri).body(Body::empty()).unwrap()
    }

    fn post(uri: &str, body: &str) -> Request<Body> {
        Request::builder()
            .method("POST")
            .uri(uri)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string()))
            .unwrap()
    }

    #[tokio::test]
    async fn a_full_cache_round_trip_over_http() {
        let dir = scratch("roundtrip");
        let state = open_state(&dir);

        // Register a project and get an id back.
        let response = router(state.clone())
            .oneshot(post("/api/projects", r#"{"name":"monorepo"}"#))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let project = json(response).await;
        let id = project["id"].as_str().unwrap().to_string();
        assert_eq!(project["name"], "monorepo");

        // A key nobody has built is a 404, and counts as a miss.
        let response = router(state.clone())
            .oneshot(get(&format!("/api/projects/{id}/cache/abc123")))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);

        // Upload the artifact…
        let payload = b"the built binary";
        let response = router(state.clone())
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri(format!(
                        "/api/projects/{id}/cache/abc123/artifact?path=dist/app"
                    ))
                    .body(Body::from(payload.to_vec()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let uploaded = json(response).await;
        assert_eq!(uploaded["sha256"], crate::cache::hash_bytes(payload));

        // …then the manifest.
        let manifest = serde_json::to_string(&Entry {
            key: "abc123".into(),
            target: "build".into(),
            workspace: "ignored".into(),
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
        })
        .unwrap();
        let response = router(state.clone())
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri(format!("/api/projects/{id}/cache/abc123"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(manifest))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        // Now the same key is a hit, and the file comes back byte-identical.
        let response = router(state.clone())
            .oneshot(get(&format!("/api/projects/{id}/cache/abc123")))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let entry = json(response).await;
        assert_eq!(entry["target"], "build");
        assert_eq!(
            entry["workspace"], id,
            "the manifest is scoped to the project"
        );

        let response = router(state.clone())
            .oneshot(get(&format!(
                "/api/projects/{id}/cache/abc123/artifact?path=dist/app"
            )))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(bytes.as_ref(), payload);

        // And the stats reflect one hit and one miss.
        let response = router(state.clone())
            .oneshot(get("/api/stats"))
            .await
            .unwrap();
        let stats = json(response).await;
        assert_eq!(stats["counters"]["hits"], 1);
        assert_eq!(stats["counters"]["misses"], 1);
        assert_eq!(stats["hit_rate"], 50.0);
        assert_eq!(stats["storage"]["entries"], 1);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A manifest naming files that were never uploaded would make the entry a
    /// hit that fails halfway through somebody else's restore.
    #[tokio::test]
    async fn a_manifest_without_its_artifacts_is_refused() {
        let dir = scratch("incomplete");
        let state = open_state(&dir);

        let response = router(state.clone())
            .oneshot(post("/api/projects", r#"{"name":"monorepo"}"#))
            .await
            .unwrap();
        let id = json(response).await["id"].as_str().unwrap().to_string();

        let manifest = serde_json::to_string(&Entry {
            key: "k".into(),
            target: "build".into(),
            workspace: id.clone(),
            inputs: vec![],
            outputs: vec![crate::cache::FileHash {
                path: "dist/never-uploaded".into(),
                sha256: "abc".into(),
                size: 1,
            }],
            env: Default::default(),
            upstream: Default::default(),
            created_at: crate::cache::store::now(),
            last_used_at: None,
            size: 1,
            duration_ms: 1,
        })
        .unwrap();

        let response = router(state.clone())
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri(format!("/api/projects/{id}/cache/k"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(manifest))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let error = json(response).await;
        assert!(
            error["error"]
                .as_str()
                .unwrap()
                .contains("dist/never-uploaded"),
            "the error must name the missing file: {error}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The artifact path is client-supplied and gets joined onto a server
    /// directory. Traversal out of the store must be impossible.
    #[tokio::test]
    async fn artifact_paths_cannot_escape_the_store() {
        let dir = scratch("traversal");
        let state = open_state(&dir);

        let response = router(state.clone())
            .oneshot(post("/api/projects", r#"{"name":"p"}"#))
            .await
            .unwrap();
        let id = json(response).await["id"].as_str().unwrap().to_string();

        for path in [
            "../../../../etc/passwd",
            "/etc/passwd",
            "dist/../../escape",
            "",
        ] {
            let encoded = urlencode(path);
            let response = router(state.clone())
                .oneshot(
                    Request::builder()
                        .method("PUT")
                        .uri(format!(
                            "/api/projects/{id}/cache/k/artifact?path={encoded}"
                        ))
                        .body(Body::from("owned"))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(
                response.status(),
                StatusCode::BAD_REQUEST,
                "'{path}' should have been refused"
            );
        }

        assert!(safe_relative("dist/app").is_ok());
        assert!(safe_relative("a/b/c.txt").is_ok());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn cache_traffic_for_an_unknown_project_is_a_404() {
        let dir = scratch("unknown");
        let state = open_state(&dir);

        let response = router(state.clone())
            .oneshot(get("/api/projects/not-a-real-id/cache/k"))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_token_server_refuses_anonymous_callers_but_health_stays_open() {
        let dir = scratch("token");
        let state = state_with(ServerConfig {
            server: Listen {
                storage: dir.clone(),
                ..Default::default()
            },
            auth: auth::AuthConfig {
                mode: "token".into(),
                users: vec![auth::TokenUser {
                    name: "ci".into(),
                    token_sha256: crate::cache::hash_bytes(b"the-token"),
                    read_only: true,
                    admin: false,
                }],
                ..Default::default()
            },
            ..Default::default()
        });

        // Health is deliberately unauthenticated — probes and the update check.
        let response = router(state.clone())
            .oneshot(get("/api/health"))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        // Everything else needs a session.
        let response = router(state.clone())
            .oneshot(get("/api/projects"))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        // A bad token is a 401 and says nothing about which half was wrong.
        let response = router(state.clone())
            .oneshot(post(
                "/api/auth/login",
                r#"{"username":"ci","password":"wrong"}"#,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        // The right one gets a session.
        let response = router(state.clone())
            .oneshot(post(
                "/api/auth/login",
                r#"{"username":"ci","password":"the-token"}"#,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = json(response).await;
        let token = body["token"].as_str().unwrap().to_string();
        assert_eq!(body["user"]["name"], "ci");
        assert_eq!(body["user"]["can_write"], false);

        let authed = |uri: &str| {
            Request::builder()
                .uri(uri.to_string())
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap()
        };

        let response = router(state.clone())
            .oneshot(authed("/api/projects"))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        // …but a read-only user may not create anything.
        let response = router(state.clone())
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/api/projects/whatever")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn the_server_advertises_and_serves_the_binaries_it_was_given() {
        let dir = scratch("release");
        let binary = dir.join("ciabatta-linux");
        std::fs::write(&binary, b"a linux ciabatta").unwrap();

        let state = state_with(ServerConfig {
            server: Listen {
                storage: dir.join("storage"),
                ..Default::default()
            },
            releases: super::super::releases::ReleaseConfig {
                version: Some("0.2.0".into()),
                notes: None,
                binaries: std::collections::BTreeMap::from([("linux".to_string(), binary)]),
            },
            ..Default::default()
        });

        // Health carries the release, so a client learns about it before login.
        let response = router(state.clone())
            .oneshot(get("/api/health"))
            .await
            .unwrap();
        let health = json(response).await;
        assert_eq!(health["release"]["version"], "0.2.0");
        assert_eq!(
            health["release"]["builds"]["linux"]["sha256"],
            crate::cache::hash_bytes(b"a linux ciabatta")
        );

        // The binary itself downloads byte-for-byte.
        let response = router(state.clone())
            .oneshot(get("/api/release/linux"))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(bytes.as_ref(), b"a linux ciabatta");

        // A platform this server doesn't carry is a 404, not an empty file.
        let response = router(state.clone())
            .oneshot(get("/api/release/windows"))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The admin page has to be reachable without credentials — it is where you
    /// The artifact everyone depends on is the one the server stops hearing
    /// about, because after the first download every client answers from its
    /// own mirror. Retention ages from last use, so without this the most
    /// useful entry in a shared cache is the one most likely to be evicted.
    #[tokio::test]
    async fn a_locally_reused_entry_can_be_kept_alive_without_downloading_it() {
        let dir = scratch("touch");
        let state = open_state(&dir);
        let app = router(state.clone());

        let project = state
            .projects
            .resolve(None, "monorepo", None)
            .expect("project registers");
        let scoped = scoped_key(&project.id, "abc123");

        // An entry that was built a fortnight ago and never fetched since.
        let stale = "2026-08-09T00:00:00+00:00".to_string();
        state
            .store
            .write_manifest(&Entry {
                key: scoped.clone(),
                target: "build".to_string(),
                workspace: project.id.clone(),
                inputs: Vec::new(),
                outputs: Vec::new(),
                env: Default::default(),
                upstream: Default::default(),
                created_at: stale.clone(),
                last_used_at: Some(stale.clone()),
                size: 0,
                duration_ms: 0,
            })
            .unwrap();

        let response = app
            .clone()
            .oneshot(
                Request::post(format!("/api/projects/{}/cache/touch", project.id))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({ "keys": ["abc123", "never-existed"] }).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let entry = state.store.get(&scoped).unwrap().expect("still stored");
        assert!(
            entry.last_touched() > stale.as_str(),
            "the entry must age from now, not from when it was last downloaded"
        );

        // A key the server has never held is the client reporting something
        // that only ever existed locally, not an error.
        assert!(
            state
                .store
                .get(&scoped_key(&project.id, "never-existed"))
                .unwrap()
                .is_none()
        );
    }

    /// A request log is a file somebody will eventually paste into a ticket, so
    /// it must show that a credential was sent without showing what it was.
    #[test]
    fn logged_headers_keep_the_scheme_and_drop_the_secret() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            "Bearer s3cr3t-token".parse().unwrap(),
        );
        headers.insert(header::COOKIE, "session=s3cr3t".parse().unwrap());
        headers.insert("x-api-key", "s3cr3t".parse().unwrap());
        headers.insert(header::USER_AGENT, "ciabatta/0.2.1".parse().unwrap());
        headers.insert(header::CONTENT_LENGTH, "4096".parse().unwrap());

        let rendered = render_headers(&headers);
        assert!(
            !rendered.contains("s3cr3t"),
            "a credential reached the log: {rendered}"
        );
        assert!(
            rendered.contains("authorization=Bearer <redacted>"),
            "the *shape* of the credential is most of a 401 investigation: {rendered}"
        );
        assert!(rendered.contains("cookie=<redacted>"), "{rendered}");
        assert!(rendered.contains("x-api-key=<redacted>"), "{rendered}");

        // Everything else is logged as sent — that's the point.
        assert!(rendered.contains("user-agent=ciabatta/0.2.1"), "{rendered}");
        assert!(rendered.contains("content-length=4096"), "{rendered}");
    }

    /// sign in to get them.
    #[tokio::test]
    async fn the_admin_page_is_served_without_a_session() {
        let dir = scratch("page");
        let state = state_with(ServerConfig {
            server: Listen {
                storage: dir.clone(),
                ..Default::default()
            },
            auth: auth::AuthConfig {
                mode: "token".into(),
                users: vec![auth::TokenUser {
                    name: "root".into(),
                    token_sha256: crate::cache::hash_bytes(b"secret"),
                    read_only: false,
                    admin: true,
                }],
                ..Default::default()
            },
            ..Default::default()
        });

        let response = router(state).oneshot(get("/")).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        assert!(String::from_utf8_lossy(&bytes).starts_with("<!doctype html>"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An open cache may mint users — that's how you migrate to token mode —
    /// but never an admin, or somebody could grant themselves lasting control
    /// while the door was open and keep it after it was shut.
    #[tokio::test]
    async fn an_open_cache_mints_users_but_never_an_admin() {
        let dir = scratch("openusers");
        let state = open_state(&dir);

        let response = router(state.clone())
            .oneshot(post("/api/users", r#"{"name":"ada"}"#))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = json(response).await;
        assert_eq!(body["user"]["name"], "ada");
        assert_eq!(body["user"]["admin"], false);
        let token = body["token"].as_str().unwrap().to_string();
        assert_eq!(token.len(), 48);

        // The escalation this refusal exists to prevent.
        let response = router(state.clone())
            .oneshot(post("/api/users", r#"{"name":"root","admin":true}"#))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert!(
            json(response).await["error"]
                .as_str()
                .unwrap()
                .contains("none of them can be an admin")
        );

        // And a name can't be minted twice — the second would silently shadow.
        let response = router(state.clone())
            .oneshot(post("/api/users", r#"{"name":"ada"}"#))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// On an authenticating cache, user management is an admin's job — and the
    /// credentials it mints work for logging in.
    #[tokio::test]
    async fn a_token_cache_requires_an_admin_to_manage_users() {
        let dir = scratch("adminusers");
        let state = state_with(ServerConfig {
            server: Listen {
                storage: dir.clone(),
                ..Default::default()
            },
            auth: auth::AuthConfig {
                mode: "token".into(),
                users: vec![
                    auth::TokenUser {
                        name: "root".into(),
                        token_sha256: crate::cache::hash_bytes(b"admin-token"),
                        read_only: false,
                        admin: true,
                    },
                    auth::TokenUser {
                        name: "ci".into(),
                        token_sha256: crate::cache::hash_bytes(b"ci-token"),
                        read_only: true,
                        admin: false,
                    },
                ],
                ..Default::default()
            },
            ..Default::default()
        });

        // Anonymous: refused before anything else is considered.
        let response = router(state.clone())
            .oneshot(post("/api/users", r#"{"name":"ada"}"#))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        let sign_in = |who: &str, secret: &str| {
            post(
                "/api/auth/login",
                &format!(r#"{{"username":"{who}","password":"{secret}"}}"#),
            )
        };

        // A non-admin gets in, but not to user management.
        let body = json(
            router(state.clone())
                .oneshot(sign_in("ci", "ci-token"))
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(body["user"]["is_admin"], false);
        let ci = body["token"].as_str().unwrap().to_string();

        let response = router(state.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/users")
                    .header(header::AUTHORIZATION, format!("Bearer {ci}"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"name":"ada"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);

        // The admin does.
        let body = json(
            router(state.clone())
                .oneshot(sign_in("root", "admin-token"))
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(body["user"]["is_admin"], true);
        let admin = body["token"].as_str().unwrap().to_string();

        let response = router(state.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/users")
                    .header(header::AUTHORIZATION, format!("Bearer {admin}"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"name":"ada"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let minted = json(response).await["token"].as_str().unwrap().to_string();

        // …and what it minted is a working credential.
        let response = router(state.clone())
            .oneshot(sign_in("ada", &minted))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = json(response).await;
        assert_eq!(body["user"]["name"], "ada");
        assert_eq!(body["user"]["can_write"], true);
        assert_eq!(body["user"]["is_admin"], false);

        // A config-declared user can't be revoked through the API — the server
        // doesn't own that file.
        let response = router(state.clone())
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/api/users/root")
                    .header(header::AUTHORIZATION, format!("Bearer {admin}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A server whose auth is misconfigured must refuse to start rather than
    /// accept traffic and fail the first person who logs in.
    #[test]
    fn a_misconfigured_server_will_not_start() {
        let dir = scratch("badconfig");
        let err = AppState::new(ServerConfig {
            server: Listen {
                storage: dir.clone(),
                ..Default::default()
            },
            auth: auth::AuthConfig {
                mode: "ldap".into(),
                ..Default::default()
            },
            ..Default::default()
        })
        .unwrap_err()
        .to_string();
        assert!(err.contains("auth.ldap"), "got: {err}");

        let err = AppState::new(ServerConfig {
            server: Listen {
                storage: dir.clone(),
                ..Default::default()
            },
            auth: auth::AuthConfig {
                session_ttl: "whenever".into(),
                ..Default::default()
            },
            ..Default::default()
        })
        .unwrap_err()
        .to_string();
        assert!(err.contains("session_ttl"), "got: {err}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Percent-encode a query value, so the traversal test's paths reach the
    /// handler intact rather than being mangled by the URL parser.
    fn urlencode(value: &str) -> String {
        value
            .bytes()
            .map(|b| match b {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                    (b as char).to_string()
                }
                _ => format!("%{b:02X}"),
            })
            .collect()
    }
}
