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
pub mod extensions;
pub mod projects;
pub mod routes;
pub mod run_store;
pub mod schemas;

use std::io::ErrorKind;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// Environment variable overriding the port the daemon listens on.
pub const PORT_ENV: &str = "CIABATTA_DAEMON_PORT";

/// The port the daemon falls back to when it can't have [`PREFERRED_PORT`].
/// Deliberately one number, replacing the six ports the individual servers used
/// to claim (7878 / 8080 / 8088 / 8090 / 8091 / 8095).
pub const DEFAULT_PORT: u16 = 8099;

/// The port the daemon tries first, so the web app is plain
/// `http://oven.localhost` with no port to remember. Linux keeps ports below
/// 1024 for root, so there it usually takes a one-time `setcap` (which the CLI
/// prints when it falls back); macOS and Windows allow it as is, unless
/// something else already has it.
pub const PREFERRED_PORT: u16 = 80;

/// The name the web app is opened under. Every `*.localhost` name resolves to
/// this machine's loopback without a hosts-file entry — browsers and curl do it
/// themselves, on every OS — so nothing needs root to set it up.
pub const WEB_HOST: &str = "oven.localhost";

/// Environment variable overriding [`WEB_HOST`] in the URLs ciabatta prints
/// and opens (`127.0.0.1`, for a browser that doesn't resolve `*.localhost`).
pub const WEB_HOST_ENV: &str = "CIABATTA_WEB_HOST";

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
    /// Why the daemon isn't on [`PREFERRED_PORT`] and how to get it there,
    /// when it chose its port itself and had to fall back. Printed by the
    /// command that started it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port_note: Option<String>,
}

/// A reachable daemon: everything a CLI command needs to talk to it.
#[derive(Debug, Clone)]
pub struct DaemonHandle {
    /// e.g. `http://127.0.0.1:8099` — the port is part of this, so it isn't
    /// carried separately.
    pub base_url: String,
    /// Where a person opens the web app: `http://oven.localhost`, with the port
    /// when it isn't 80. Every URL ciabatta prints or opens uses this; the API
    /// calls use [`Self::base_url`].
    pub web_url: String,
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

/// The port someone asked for: `--port` if given, else [`PORT_ENV`]. `None`
/// leaves the daemon to choose: see [`candidate_ports`].
pub fn requested_port(flag: Option<u16>) -> Option<u16> {
    flag.or_else(|| std::env::var(PORT_ENV).ok()?.trim().parse().ok())
}

/// The ports a daemon tries, in order: only the one asked for, or
/// [`PREFERRED_PORT`] then [`DEFAULT_PORT`].
pub fn candidate_ports(flag: Option<u16>) -> Vec<u16> {
    match requested_port(flag) {
        Some(port) => vec![port],
        None => vec![PREFERRED_PORT, DEFAULT_PORT],
    }
}

/// The web app's address on `port`: `http://oven.localhost`, with `:port`
/// unless it's 80.
///
/// A daemon bound to one specific non-loopback address can't be reached
/// through `*.localhost`, which is always loopback, so that address is used
/// instead.
pub fn web_url(port: u16) -> String {
    let host = match std::env::var(WEB_HOST_ENV)
        .ok()
        .filter(|h| !h.trim().is_empty())
    {
        Some(host) => host.trim().to_string(),
        None => {
            let probe = probe_host();
            match probe.as_str() {
                "127.0.0.1" | "localhost" | "::1" => WEB_HOST.to_string(),
                _ => probe,
            }
        }
    };
    match port {
        80 => format!("http://{host}"),
        _ => format!("http://{host}:{port}"),
    }
}

/// What to say when a daemon that chose its own port couldn't have
/// [`PREFERRED_PORT`], given why.
pub fn port_note(error: &std::io::Error) -> String {
    let exe = std::env::current_exe()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "$(which ciabatta)".to_string());
    match error.kind() {
        ErrorKind::PermissionDenied if cfg!(target_os = "linux") => format!(
            "Port {PREFERRED_PORT} needs permission on Linux, so the web app is on \
             {DEFAULT_PORT}. For plain http://{WEB_HOST}, allow it once (again after each \
             ciabatta upgrade):\n    sudo setcap cap_net_bind_service=+ep {exe}\n\
             then `ciabatta daemon restart`."
        ),
        ErrorKind::PermissionDenied if cfg!(windows) => format!(
            "Windows refused port {PREFERRED_PORT} (another program has reserved it — \
             `netsh http show servicestate` lists who), so the web app is on {DEFAULT_PORT}."
        ),
        ErrorKind::AddrInUse => format!(
            "Something else is using port {PREFERRED_PORT}, so the web app is on \
             {DEFAULT_PORT}. Free it for plain http://{WEB_HOST}, then `ciabatta daemon restart`."
        ),
        _ => format!(
            "Couldn't use port {PREFERRED_PORT} ({error}), so the web app is on {DEFAULT_PORT}."
        ),
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
    let port = requested_port(port);
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
    let handle = wait_for_startup(current_version).await?;
    // Said by the command that started the daemon, since the daemon itself has
    // only a log file to say it in.
    if let Some(note) = read_record().and_then(|r| r.port_note) {
        eprintln!("note: {note}");
    }
    Ok(handle)
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
            self.daemon.web_url.trim_end_matches('/'),
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
/// For a caller working on a checkout it isn't standing in: the session has to
/// be registered against that project, not the operator's current directory.
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
        web_url: web_url(record.port),
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

/// Poll until a daemon of the expected version answers on the port its record
/// names. The record is what says which port: a daemon left to choose tries
/// [`PREFERRED_PORT`] first and may end up on [`DEFAULT_PORT`].
async fn wait_for_startup(expected_version: &str) -> Result<DaemonHandle> {
    let deadline = std::time::Instant::now() + STARTUP_TIMEOUT;
    loop {
        if let Some(record) = read_record()
            && let Some(health) = probe(record.port).await
            && health.version == expected_version
        {
            return Ok(handle_from(&record, health.pid));
        }
        if std::time::Instant::now() >= deadline {
            anyhow::bail!(
                "The ciabatta daemon didn't come up within {}s.\n\
                 Check the log with `ciabatta daemon logs`, or start it in the \
                 foreground with `ciabatta daemon serve` to see the error.",
                STARTUP_TIMEOUT.as_secs()
            );
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Re-launch this executable as a detached `ciabatta daemon serve`, with its
/// stdio pointed away from the terminal so it survives the parent exiting.
///
/// Mirrors the detach pattern the todo app used before the daemon existed, with
/// one difference: stderr goes to `daemon.log` rather than to `/dev/null`.
/// `tracing` writes to that file itself, so this is only for what `tracing`
/// never sees — the runtime's own last words. A double panic, an abort, a
/// failed allocation and a stack overflow all print to stderr and then kill the
/// process, and discarding them is exactly why a daemon that dies mid-run looks
/// like it vanished for no reason.
fn spawn_detached(port: Option<u16>) -> Result<()> {
    use std::process::{Command, Stdio};

    let exe = std::env::current_exe().context("Failed to locate the ciabatta executable")?;

    // Best effort: a daemon that can't open its log is still better than no
    // daemon, so a failure here falls back to the old behaviour.
    let errors = log_path()
        .ok()
        .and_then(|path| {
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .ok()
        })
        .map(Stdio::from)
        .unwrap_or_else(Stdio::null);

    let mut command = Command::new(exe);
    command.arg("daemon").arg("serve");
    // Only a port someone asked for is passed on; otherwise the daemon picks.
    if let Some(port) = port {
        command.arg("--port").arg(port.to_string());
    }
    command
        // Started from inside a step, it would otherwise carry that run's slot
        // for life and treat every run it is asked for as nested in it.
        .env_remove(crate::run::slots::SLOT_VAR)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(errors)
        .spawn()
        .context("Failed to start the ciabatta daemon in the background")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_requested_port_is_the_only_one_tried() {
        assert_eq!(candidate_ports(Some(1234)), vec![1234]);
    }

    #[test]
    fn left_to_choose_it_tries_80_then_the_default() {
        // The env var is not set in the test environment, so this exercises the
        // daemon choosing for itself (mutating the process environment here
        // would race with other tests).
        if std::env::var(PORT_ENV).is_err() {
            assert_eq!(candidate_ports(None), vec![PREFERRED_PORT, DEFAULT_PORT]);
        }
    }

    #[test]
    fn the_web_url_is_oven_localhost_and_drops_port_80() {
        if std::env::var(WEB_HOST_ENV).is_err() && probe_host() == "127.0.0.1" {
            assert_eq!(web_url(80), "http://oven.localhost");
            assert_eq!(web_url(8099), "http://oven.localhost:8099");
        }
    }

    #[test]
    fn handle_builds_urls_without_doubling_slashes() {
        let handle = DaemonHandle {
            base_url: "http://127.0.0.1:8099/".to_string(),
            web_url: "http://oven.localhost:8099".to_string(),
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
