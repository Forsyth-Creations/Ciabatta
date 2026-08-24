//! The daemon's axum application: shared state, the router, and `serve`.
//!
//! Every feature route lives in [`super::routes`]; this module just wires them
//! together, wraps the mutating ones in the token check, and puts the embedded
//! web app underneath as a fallback.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::json;
use tokio::sync::Notify;

use super::projects::{Project, Registry};
use super::{DaemonRecord, assets, auth, routes};

/// State shared by every route handler.
#[derive(Clone)]
pub struct AppState {
    /// The bearer token mutating routes require, also injected into the page.
    pub token: String,
    /// This binary's version, reported by `/api/health` so clients can detect
    /// a daemon left over from an older build.
    pub version: &'static str,
    pub started_at: String,
    pub pid: u32,
    /// Known checkouts, for the web app's project switcher.
    pub projects: Arc<Registry>,
    /// The personal task list. Global rather than per-project — see
    /// [`super::routes::todo`].
    pub todos: Arc<crate::todo::Store>,
    /// Per-project live objects, built on first use.
    pub per_project: Arc<Mutex<HashMap<String, ProjectState>>>,
    /// Watch sessions the daemon owns. These outlive the CLI invocation that
    /// created them — that is the whole point of the daemon.
    pub watch: Arc<routes::watch::Sessions>,
    /// Which projects currently have an analyze scan in flight.
    pub analyze: Arc<routes::analyze::Running>,
    /// Runs the daemon owns.
    pub runs: Arc<routes::run::Runs>,
    /// Signalled by `POST /api/shutdown`.
    pub shutdown: Arc<Notify>,
}

/// The live objects belonging to one checkout.
///
/// Built lazily because constructing them touches the filesystem (and, for the
/// assistant, reads config): a daemon serving five projects shouldn't pay for
/// four of them on startup.
#[derive(Clone, Default)]
pub struct ProjectState {
    pub ai_jobs: Option<Arc<crate::ai::jobs::Jobs>>,
    pub assistant: Option<Arc<crate::ai::Assistant>>,
    /// Serializes `/api/ai/ask` per project so concurrent callers can't
    /// interleave a single conversation history.
    pub ask_gate: Option<Arc<tokio::sync::Mutex<()>>>,
}

impl AppState {
    /// Look a project up by id, as a route-friendly error.
    pub fn project(&self, id: &str) -> super::routes::RouteResult<Project> {
        self.projects.get(id).ok_or_else(|| {
            super::routes::RouteError::bad_request(format!("Unknown project '{id}'"))
        })
    }

    /// A project's root directory by id — the common case, since most routes
    /// only ever want somewhere to load config from.
    pub fn project_root(&self, id: &str) -> super::routes::RouteResult<std::path::PathBuf> {
        Ok(self.project(id)?.path)
    }

    /// The assistant for a project, constructing it on first use.
    ///
    /// Building one reads config and loads the brain from disk, so a daemon
    /// serving several checkouts shouldn't pay for all of them up front.
    pub fn assistant(&self, id: &str) -> super::routes::RouteResult<Arc<crate::ai::Assistant>> {
        let project = self.project(id)?;

        if let Some(assistant) = self
            .per_project
            .lock()
            .unwrap()
            .get(&project.id)
            .and_then(|s| s.assistant.clone())
        {
            return Ok(assistant);
        }

        // Built outside the lock: `Assistant::new` does file I/O. It already
        // hands back an Arc.
        let config = crate::config::load_config(&project.path)?;
        let assistant = crate::ai::Assistant::new(&project.path, &config)?;

        let mut guard = self.per_project.lock().unwrap();
        let entry = guard.entry(project.id.clone()).or_default();
        Ok(entry.assistant.get_or_insert(assistant).clone())
    }

    /// The per-project lock guarding `/api/ai/ask`.
    pub fn ask_gate(&self, id: &str) -> super::routes::RouteResult<Arc<tokio::sync::Mutex<()>>> {
        let project = self.project(id)?;
        let mut guard = self.per_project.lock().unwrap();
        let entry = guard.entry(project.id).or_default();
        Ok(entry.ask_gate.get_or_insert_with(Default::default).clone())
    }

    /// The AI job store for a project, opening it on first use.
    pub fn ai_jobs(&self, project: &Project) -> Result<Arc<crate::ai::jobs::Jobs>> {
        // Check the cache first, then build outside the lock — `Jobs::open`
        // does file I/O and shouldn't be holding a mutex shared by every
        // project's requests.
        if let Some(jobs) = self
            .per_project
            .lock()
            .unwrap()
            .get(&project.id)
            .and_then(|s| s.ai_jobs.clone())
        {
            return Ok(jobs);
        }

        let config = crate::config::load_config(&project.path)?;
        let jobs = crate::ai::jobs::Jobs::open(&project.path, &config)?;

        let mut guard = self.per_project.lock().unwrap();
        let entry = guard.entry(project.id.clone()).or_default();
        // Another request may have won the race; prefer whatever landed first
        // so both callers share one store and one jobs.json writer.
        Ok(entry.ai_jobs.get_or_insert(jobs).clone())
    }
}

/// Assemble the router.
///
/// Split into three layers, in order of increasing openness:
///
/// 1. `/api/health` — no token, so liveness probes work from anywhere.
/// 2. everything else under `/api` — token required (see [`auth`]).
/// 3. the embedded web app, as the fallback, with SPA history routing.
pub fn router(state: AppState) -> Router {
    let public = Router::new().route("/api/health", get(health));

    let api = Router::new()
        .route("/api/shutdown", post(shutdown))
        .merge(routes::router())
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            auth::require_token,
        ));

    public
        .merge(api)
        .fallback(assets::serve)
        .layer(tower_http::trace::TraceLayer::new_for_http())
        .with_state(state)
}

/// Liveness probe. Deliberately unauthenticated and cheap: [`super::probe`]
/// calls this on a 400 ms timeout before every command.
async fn health(
    axum::extract::State(state): axum::extract::State<AppState>,
) -> Json<serde_json::Value> {
    Json(json!({
        "ok": true,
        "version": state.version,
        "pid": state.pid,
        "started_at": state.started_at,
    }))
}

/// Ask the daemon to exit. Used by `ciabatta daemon stop`, and by
/// [`super::ensure_running`] when it finds a daemon from an older build.
async fn shutdown(
    axum::extract::State(state): axum::extract::State<AppState>,
) -> Json<serde_json::Value> {
    tracing::info!("shutdown requested");
    state.shutdown.notify_one();
    Json(json!({ "ok": true }))
}

/// Bind the port and serve until shutdown.
///
/// The port is bound *before* the daemon record is written, so a second
/// invocation racing to start a daemon loses cleanly: it gets `AddrInUse`,
/// finds the winner healthy, and exits quietly rather than clobbering the
/// record.
pub async fn serve(port: u16) -> Result<()> {
    // Every run this daemon drives is read in a browser, and the web app parses
    // SGR escapes into styled spans. So the steps are asked for colour — the
    // question "is the daemon's stdout a terminal?" is about a log file and has
    // nothing to do with who ends up reading this.
    crate::color::decide(crate::color::Consumer::Web);

    let host = crate::config::bind_host();
    let addr: SocketAddr = format!("{host}:{port}")
        .parse()
        .with_context(|| format!("Invalid bind address {host}:{port}"))?;

    let listener = match tokio::net::TcpListener::bind(addr).await {
        Ok(l) => l,
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
            // Someone else may have won the startup race. If a healthy ciabatta
            // is answering there, this process has nothing to do.
            if super::find_running().await.is_some() {
                tracing::info!("another ciabatta daemon already holds port {port}; exiting");
                return Ok(());
            }
            anyhow::bail!(
                "Port {port} is already in use by something that isn't ciabatta.\n\
                 Pick another with `ciabatta daemon serve --port <PORT>` or set \
                 {}=<PORT>.",
                super::PORT_ENV
            );
        }
        Err(e) => {
            return Err(e).with_context(|| format!("Failed to bind {addr}"));
        }
    };

    if host != crate::config::DEFAULT_BIND_HOST {
        tracing::warn!(
            "The daemon is bound to {host}, not loopback. Its API can start \
             processes, so anyone who can reach port {port} and read the token \
             can run commands as you."
        );
    }

    // The port bound, so no other daemon is alive on it. A record still
    // sitting in ~/.ciabatta means the daemon that wrote it never reached its
    // own shutdown path — it was killed. Said out loud at startup because the
    // evidence is otherwise invisible: a SIGKILL (the OOM killer, a `kill -9`,
    // a `pkill ciabatta` from a build step) leaves nothing in the log at all,
    // and this line is what distinguishes that from a panic, which does.
    if let Some(stale) = super::read_record() {
        tracing::warn!(
            previous_pid = stale.pid,
            previous_version = %stale.version,
            started_at = %stale.started_at,
            "the previous daemon exited without shutting down cleanly; if there \
             is no panic logged just above this line, it was killed from outside \
             (OOM killer, `kill`, or a step that killed its own process tree)"
        );
    }

    let token = auth::generate_token();
    let state = AppState {
        token: token.clone(),
        version: env!("CARGO_PKG_VERSION"),
        started_at: chrono::Local::now().to_rfc3339(),
        pid: std::process::id(),
        projects: Arc::new(Registry::open()?),
        todos: Arc::new(crate::todo::Store::open()?),
        per_project: Arc::new(Mutex::new(HashMap::new())),
        watch: Arc::new(routes::watch::Sessions::new()),
        analyze: routes::analyze::Running::new(),
        runs: routes::run::Runs::new(),
        shutdown: Arc::new(Notify::new()),
    };

    super::write_record(&DaemonRecord {
        pid: state.pid,
        port,
        version: state.version.to_string(),
        token,
        started_at: state.started_at.clone(),
    })?;

    tracing::info!(
        pid = state.pid,
        version = %state.version,
        log = %crate::daemon::log_path().map(|p| p.display().to_string()).unwrap_or_default(),
        "ciabatta daemon {} listening on http://{host}:{port}",
        state.version
    );

    let signal = state.shutdown.clone();
    let watch = state.watch.clone();
    let result = axum::serve(listener, router(state))
        .with_graceful_shutdown(async move {
            tokio::select! {
                _ = signal.notified() => tracing::info!("shutdown requested over the API"),
                _ = tokio::signal::ctrl_c() => tracing::info!("interrupted (SIGINT)"),
                signal = terminated() => tracing::info!("{signal}"),
            }
            // Watched commands belong to the daemon, so they stop with it.
            watch.stop_all();
        })
        .await;

    // Best effort: if the record still points at us, drop it so the next
    // command starts a fresh daemon instead of probing a dead port.
    if super::read_record().is_some_and(|r| r.pid == std::process::id()) {
        let _ = super::clear_record();
    }

    // Whichever way this went, say so — an ended log is otherwise the same
    // shape whether the daemon stopped on purpose or died mid-request.
    match &result {
        Ok(()) => tracing::info!("ciabatta daemon stopped"),
        Err(e) => tracing::error!("the daemon's HTTP server stopped unexpectedly: {e:#}"),
    }

    result.context("The daemon's HTTP server stopped unexpectedly")
}

/// Resolves when the process is asked to terminate, naming the signal.
///
/// SIGTERM is how nearly everything else asks a background process to stop —
/// `kill`, a session teardown, a container stop — and without this the daemon
/// dies to it with the log ending mid-line, indistinguishable from a crash.
/// SIGKILL cannot be caught at all, which is what the stale-record warning at
/// the next startup is for.
async fn terminated() -> String {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let Ok(mut term) = signal(SignalKind::terminate()) else {
            std::future::pending::<()>().await;
            unreachable!()
        };
        let Ok(mut hup) = signal(SignalKind::hangup()) else {
            std::future::pending::<()>().await;
            unreachable!()
        };
        tokio::select! {
            _ = term.recv() => "terminated (SIGTERM)".to_string(),
            _ = hup.recv() => "hung up (SIGHUP)".to_string(),
        }
    }
    #[cfg(not(unix))]
    {
        std::future::pending::<String>().await
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// An `AppState` backed by a temporary registry, for route tests.
    pub fn test_state() -> AppState {
        AppState {
            token: "test-token".to_string(),
            version: env!("CARGO_PKG_VERSION"),
            started_at: "1970-01-01T00:00:00+00:00".to_string(),
            pid: 0,
            projects: Arc::new(Registry::open().expect("registry opens")),
            todos: Arc::new(crate::todo::Store::open().expect("todo store opens")),
            per_project: Arc::new(Mutex::new(HashMap::new())),
            watch: Arc::new(routes::watch::Sessions::new()),
            analyze: routes::analyze::Running::new(),
            runs: routes::run::Runs::new(),
            shutdown: Arc::new(Notify::new()),
        }
    }

    use axum::body::Body;
    use axum::http::{Request, StatusCode, header};
    use tower::ServiceExt;

    #[tokio::test]
    async fn health_needs_no_token() {
        let app = router(test_state());
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn mutating_routes_reject_a_missing_token() {
        let app = router(test_state());
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/shutdown")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "an unauthenticated caller must not be able to stop the daemon"
        );
    }

    #[tokio::test]
    async fn mutating_routes_reject_a_wrong_token() {
        let app = router(test_state());
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/shutdown")
                    .header(header::AUTHORIZATION, "Bearer not-the-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn mutating_routes_accept_the_token() {
        let app = router(test_state());
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/shutdown")
                    .header(header::AUTHORIZATION, "Bearer test-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn unknown_paths_fall_through_to_the_web_app() {
        let app = router(test_state());
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/watch/42")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let content_type = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default();
        assert!(content_type.starts_with("text/html"));
    }

    #[tokio::test]
    async fn missing_assets_404_instead_of_serving_html() {
        let app = router(test_state());
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/assets/does-not-exist.js")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }
}
