//! The ciabatta daemon: one long-lived background process serving one web app.
//!
//! Every web-facing command (`todo`, `watch`, `run --gui`, `analyze`, `ai`)
//! talks to this daemon instead of standing up its own server. The
//! daemon owns the work too — a `watch` session keeps running after the
//! terminal that started it goes away.
//!
//! # Discovery
//!
//! A running daemon records itself in `~/.ciabatta/daemon.json` (mode 0600,
//! because it carries the API token). [`ensure_running`] reads that file,
//! probes `GET /api/health`, and spawns a detached daemon if nothing healthy
//! answers.

pub mod app;
pub mod assets;
pub mod auth;
pub mod projects;
pub mod routes;

use std::io::ErrorKind;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// Environment variable overriding the port the daemon listens on.
pub const PORT_ENV: &str = "CIABATTA_DAEMON_PORT";

/// The default daemon port. Deliberately one number, replacing the six ports
/// the individual servers used to claim (7878 / 8080 / 8088 / 8090 / 8091 / 8095).
pub const DEFAULT_PORT: u16 = 8099;

/// How long to wait for a health probe before deciding nothing is there.
const PROBE_TIMEOUT: Duration = Duration::from_millis(400);

/// How long [`ensure_running`] waits for a freshly spawned daemon to come up.
const STARTUP_TIMEOUT: Duration = Duration::from_secs(5);

/// What a running daemon writes to `~/.ciabatta/daemon.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonRecord {
    pub pid: u32,
    pub port: u16,
    /// The version of the binary running the daemon. A mismatch against the
    /// current binary means the daemon is stale and should be replaced.
    pub version: String,
    /// Bearer token required by mutating API routes. See [`auth`].
    pub token: String,
    pub started_at: String,
}

/// A reachable daemon: everything a CLI command needs to talk to it.
#[derive(Debug, Clone)]
pub struct DaemonHandle {
    /// e.g. `http://127.0.0.1:8099` — the port is part of this, so it isn't
    /// carried separately.
    pub base_url: String,
    pub token: String,
    pub pid: u32,
}

impl DaemonHandle {
    /// A `reqwest` client with the bearer token pre-attached.
    pub fn client(&self) -> Result<reqwest::Client> {
        let mut headers = reqwest::header::HeaderMap::new();
        let mut value = reqwest::header::HeaderValue::from_str(&format!("Bearer {}", self.token))
            .context("The daemon token isn't a valid HTTP header value")?;
        value.set_sensitive(true);
        headers.insert(reqwest::header::AUTHORIZATION, value);
        reqwest::Client::builder()
            .default_headers(headers)
            .timeout(Duration::from_secs(30))
            .build()
            .context("Failed to build the daemon HTTP client")
    }

    /// The full URL for an API path, e.g. `api/projects`.
    pub fn url(&self, path: &str) -> String {
        format!(
            "{}/{}",
            self.base_url.trim_end_matches('/'),
            path.trim_start_matches('/')
        )
    }
}

/// The port the daemon should use: `--port` if given, else [`PORT_ENV`], else
/// [`DEFAULT_PORT`].
pub fn resolve_port(flag: Option<u16>) -> u16 {
    if let Some(p) = flag {
        return p;
    }
    match std::env::var(PORT_ENV) {
        Ok(v) => v.trim().parse().unwrap_or(DEFAULT_PORT),
        Err(_) => DEFAULT_PORT,
    }
}

/// `~/.ciabatta`, created if it doesn't exist.
pub fn state_dir() -> Result<PathBuf> {
    let home = home_dir().context("Could not determine your home directory (HOME is unset)")?;
    let dir = home.join(".ciabatta");
    std::fs::create_dir_all(&dir).with_context(|| format!("Failed to create {}", dir.display()))?;
    Ok(dir)
}

/// The same home-directory lookup [`crate::todo`] uses, kept in sync so the
/// daemon and the todo store agree on where personal state lives.
pub fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

/// Path of the daemon record file.
pub fn record_path() -> Result<PathBuf> {
    Ok(state_dir()?.join("daemon.json"))
}

/// Path of the daemon log file. Detached daemons have nowhere to print, so
/// everything goes here.
pub fn log_path() -> Result<PathBuf> {
    Ok(state_dir()?.join("daemon.log"))
}

/// Read the daemon record, if one exists and parses.
pub fn read_record() -> Option<DaemonRecord> {
    let path = record_path().ok()?;
    let raw = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&raw).ok()
}

/// Write the daemon record with owner-only permissions — it holds the token.
pub fn write_record(record: &DaemonRecord) -> Result<()> {
    let path = record_path()?;
    let body = serde_json::to_string_pretty(record)?;
    std::fs::write(&path, body).with_context(|| format!("Failed to write {}", path.display()))?;
    restrict_permissions(&path)?;
    Ok(())
}

/// Remove the daemon record (on clean shutdown, or when it's known stale).
pub fn clear_record() -> Result<()> {
    let path = record_path()?;
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e).with_context(|| format!("Failed to remove {}", path.display())),
    }
}

/// Restrict a file to owner read/write. A no-op on Windows, where the file
/// already lands in the user's profile directory.
#[cfg(unix)]
fn restrict_permissions(path: &std::path::Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .with_context(|| format!("Failed to restrict permissions on {}", path.display()))
}

#[cfg(not(unix))]
fn restrict_permissions(_path: &std::path::Path) -> Result<()> {
    Ok(())
}

/// The health-probe response.
#[derive(Debug, Deserialize)]
struct Health {
    #[allow(dead_code)]
    ok: bool,
    version: String,
    pid: u32,
}

/// Probe a port for a healthy ciabatta daemon. Returns its reported version and
/// pid, or `None` if nothing usable answered in [`PROBE_TIMEOUT`].
async fn probe(port: u16) -> Option<Health> {
    let client = reqwest::Client::builder()
        .timeout(PROBE_TIMEOUT)
        .build()
        .ok()?;
    let host = probe_host();
    let resp = client
        .get(format!("http://{host}:{port}/api/health"))
        .send()
        .await
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    resp.json::<Health>().await.ok()
}

/// The host to *probe* on. Even when the daemon binds `0.0.0.0`, loopback
/// reaches it, so probes always go to loopback.
fn probe_host() -> String {
    let bind = crate::config::bind_host();
    if bind == "0.0.0.0" || bind == "::" {
        "127.0.0.1".to_string()
    } else {
        bind
    }
}

/// Make sure a daemon matching this binary's version is running, starting one in
/// the background if not, and return a handle for talking to it.
///
/// This is the single entry point every web-facing command calls.
pub async fn ensure_running(port: Option<u16>) -> Result<DaemonHandle> {
    let port = resolve_port(port);
    let current_version = env!("CARGO_PKG_VERSION");

    // A record plus a healthy, version-matched daemon means we're already done.
    if let Some(record) = read_record()
        && let Some(health) = probe(record.port).await
    {
        if health.version == current_version {
            return Ok(handle_from(&record, health.pid));
        }

        // A daemon from an older build is running. Retire it so the new
        // binary's routes and web app are the ones being served.
        tracing::info!(
            "Replacing ciabatta daemon {} (pid {}) with {current_version}",
            health.version,
            health.pid
        );
        let _ = shutdown(&record).await;
        wait_for_exit(record.port).await;
        clear_record()?;
    }

    // Nothing healthy on the recorded port. Something may still be listening on
    // the port we're about to use, so say so clearly rather than looping.
    spawn_detached(port)?;
    wait_for_startup(port, current_version).await
}

/// What a CLI command needs to hand the user off to the web app: a running
/// daemon, and the project id for the directory they ran the command in.
pub struct Session {
    pub daemon: DaemonHandle,
    pub project: crate::daemon::projects::Project,
}

impl Session {
    /// The web app URL for a page, carrying the project selection so the
    /// browser lands on the checkout the command was run in.
    pub fn page_url(&self, path: &str) -> String {
        format!(
            "{}/{}?project={}",
            self.daemon.base_url.trim_end_matches('/'),
            path.trim_start_matches('/'),
            self.project.id
        )
    }
}

/// The common opening move for every web-facing command: make sure the daemon
/// is up, then register the current directory as a project so the web app can
/// scope its views to it.
pub async fn connect(port: Option<u16>) -> Result<Session> {
    let cwd = std::env::current_dir().context("Failed to get the current directory")?;
    connect_at(port, &cwd).await
}

/// [`connect`], for a directory other than the current one.
///
/// The run engine needs this: a `persistent` step is handed to the daemon as a
/// watch session, and it has to be registered against the project the *run* is
/// in — which, for a monorepo workflow, is the workspace root rather than
/// wherever the operator happened to be standing.
pub async fn connect_at(port: Option<u16>, dir: &std::path::Path) -> Result<Session> {
    let daemon = ensure_running(port).await?;
    let cwd = dir.to_path_buf();

    let project = daemon
        .client()?
        .post(daemon.url("/api/projects"))
        .json(&serde_json::json!({ "path": cwd }))
        .send()
        .await
        .context("Failed to register this directory with the ciabatta daemon")?
        .error_for_status()
        .context("The daemon rejected this directory as a project")?
        .json::<crate::daemon::projects::Project>()
        .await
        .context("The daemon returned an unexpected project payload")?;

    Ok(Session { daemon, project })
}

/// Best-effort: open `url` in the platform browser. Never fails the command.
///
/// One copy, shared by every command — this used to be duplicated verbatim in
/// the watch, run, and analyze servers.
pub fn open_browser(url: &str) {
    #[cfg(target_os = "macos")]
    let candidates: [(&str, &[&str]); 1] = [("open", &[])];
    #[cfg(target_os = "windows")]
    let candidates: [(&str, &[&str]); 1] = [("cmd", &["/C", "start", ""])];
    #[cfg(all(unix, not(target_os = "macos")))]
    let candidates: [(&str, &[&str]); 2] = [("xdg-open", &[]), ("gio", &["open"])];

    for (cmd, args) in candidates {
        let mut command = std::process::Command::new(cmd);
        command.args(args).arg(url);
        command.stdout(std::process::Stdio::null());
        command.stderr(std::process::Stdio::null());
        if command.spawn().is_ok() {
            return;
        }
    }
}

/// Return a handle only if a daemon is already running — never starts one.
/// Used by `ciabatta daemon status` / `stop`.
pub async fn find_running() -> Option<DaemonHandle> {
    let record = read_record()?;
    let health = probe(record.port).await?;
    Some(handle_from(&record, health.pid))
}

fn handle_from(record: &DaemonRecord, pid: u32) -> DaemonHandle {
    DaemonHandle {
        base_url: format!("http://{}:{}", probe_host(), record.port),
        token: record.token.clone(),
        pid,
    }
}

/// Ask a daemon to shut itself down.
pub async fn shutdown(record: &DaemonRecord) -> Result<()> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(3))
        .build()?;
    let _ = client
        .post(format!(
            "http://{}:{}/api/shutdown",
            probe_host(),
            record.port
        ))
        .header(
            reqwest::header::AUTHORIZATION,
            format!("Bearer {}", record.token),
        )
        .send()
        .await;
    Ok(())
}

/// Poll until the port stops answering, or we give up.
async fn wait_for_exit(port: u16) {
    for _ in 0..30 {
        if probe(port).await.is_none() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Poll until a daemon of the expected version answers on `port`.
async fn wait_for_startup(port: u16, expected_version: &str) -> Result<DaemonHandle> {
    let deadline = std::time::Instant::now() + STARTUP_TIMEOUT;
    loop {
        if let Some(health) = probe(port).await
            && health.version == expected_version
        {
            let record = read_record().context(
                "The daemon started but didn't write ~/.ciabatta/daemon.json. \
                 Check `ciabatta daemon logs`.",
            )?;
            return Ok(handle_from(&record, health.pid));
        }
        if std::time::Instant::now() >= deadline {
            anyhow::bail!(
                "The ciabatta daemon didn't come up on port {port} within {}s.\n\
                 Check the log with `ciabatta daemon logs`, or start it in the \
                 foreground with `ciabatta daemon serve` to see the error.",
                STARTUP_TIMEOUT.as_secs()
            );
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Re-launch this executable as a detached `ciabatta daemon serve`, with its
/// stdio discarded so it survives the parent exiting.
///
/// Mirrors the detach pattern the todo app used before the daemon existed.
fn spawn_detached(port: u16) -> Result<()> {
    use std::process::{Command, Stdio};

    let exe = std::env::current_exe().context("Failed to locate the ciabatta executable")?;
    Command::new(exe)
        .arg("daemon")
        .arg("serve")
        .arg("--port")
        .arg(port.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("Failed to start the ciabatta daemon in the background")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_port_prefers_the_flag() {
        assert_eq!(resolve_port(Some(1234)), 1234);
    }

    #[test]
    fn resolve_port_falls_back_to_the_default() {
        // The env var is not set in the test environment, so this exercises the
        // final fallback rather than the env branch (which would race with
        // other tests if we mutated the process environment here).
        if std::env::var(PORT_ENV).is_err() {
            assert_eq!(resolve_port(None), DEFAULT_PORT);
        }
    }

    #[test]
    fn handle_builds_urls_without_doubling_slashes() {
        let handle = DaemonHandle {
            base_url: "http://127.0.0.1:8099/".to_string(),
            token: "t".to_string(),
            pid: 1,
        };
        assert_eq!(
            handle.url("/api/projects"),
            "http://127.0.0.1:8099/api/projects"
        );
        assert_eq!(
            handle.url("api/projects"),
            "http://127.0.0.1:8099/api/projects"
        );
    }
}
