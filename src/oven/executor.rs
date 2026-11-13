//! `ciabatta oven executor`: a machine that runs other people's workflows.
//!
//! It starts unpaired. It advertises itself over mDNS, prints its address and
//! a pairing code, and waits. An admin adds it on the remote cache's
//! Executors page (where it's listed if the cache is on the same network), or
//! by typing the address and code. The cache then calls `POST /oven/pair` with
//! the code and a token, and from then on the executor works for that cache:
//!
//! 1. Long-poll the cache for a job.
//! 2. Fetch the repository into a mirror kept between jobs, and check out the
//!    job's commit on its branch in a directory of its own.
//! 3. Run `ciabatta workflow …` there, sending every line of output to the
//!    cache, and a heartbeat every few seconds — which is also how it learns
//!    that somebody cancelled the job.
//! 4. Send back the files the run's steps declare as cache `outputs`, and say
//!    how it ended.
//!
//! One job at a time, by construction: it asks for the next only when the
//! last is done.
//!
//! The pairing is saved in `~/.ciabatta/oven/executor.json`, so a restart
//! carries on working for the same cache without pairing again.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
};
use serde::{Deserialize, Serialize};

use super::{
    ExecutorInfo, Finish, Heartbeat, HeartbeatReply, Job, JobState, LogBatch, OutputFile,
    POLL_SECONDS, PairRequest, Poll,
};

/// Wrong codes allowed before the code is replaced with a new one.
const MAX_ATTEMPTS: u32 = 5;

/// How the executor was started.
#[derive(Debug, Clone)]
pub struct Options {
    /// What it's called on the cache; defaults to the host name.
    pub name: Option<String>,
    pub port: u16,
    /// Where its identity and pairing are kept; defaults to ~/.ciabatta/oven.
    pub dir: Option<PathBuf>,
    /// Where repositories and job checkouts go; defaults to `<dir>/work`.
    pub work_dir: Option<PathBuf>,
    /// Repository URLs it will build (globs). Empty: any.
    pub allow_repo: Vec<String>,
    /// Don't advertise over mDNS.
    pub no_mdns: bool,
    /// Check the cache's TLS certificate.
    pub tls_verify: bool,
    /// Forget the saved pairing first.
    pub unpair: bool,
}

/// What survives a restart.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Saved {
    id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pairing: Option<Pairing>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Pairing {
    server: String,
    token: String,
}

struct Shared {
    info: ExecutorInfo,
    code: Mutex<String>,
    attempts: AtomicU32,
    saved: Mutex<Saved>,
    saved_path: PathBuf,
    /// Woken when a pairing arrives.
    paired: tokio::sync::Notify,
    /// The mDNS advertisement and the port it names, once it's up. Kept for
    /// as long as the executor runs — dropping the daemon withdraws it — and
    /// so a pairing can update what it says.
    mdns: Mutex<Option<(mdns_sd::ServiceDaemon, u16)>>,
}

impl Shared {
    fn pairing(&self) -> Option<Pairing> {
        self.saved.lock().unwrap().pairing.clone()
    }

    fn set_pairing(&self, pairing: Option<Pairing>) -> Result<()> {
        let paired = pairing.is_some();
        {
            let mut saved = self.saved.lock().unwrap();
            saved.pairing = pairing;
            save(&self.saved_path, &saved)?;
        }
        self.readvertise(paired);
        Ok(())
    }

    /// Say on the network whether this executor is taken, so a cache that
    /// finds it can tell "add it" from "it belongs to another cache".
    fn readvertise(&self, paired: bool) {
        if let Some((daemon, port)) = &*self.mdns.lock().unwrap() {
            // Registering the same service again replaces its record.
            if let Ok(service) = service_info(&self.info, *port, paired) {
                let _ = daemon.register(service);
            }
        }
    }
}

fn state_dir() -> Result<PathBuf> {
    let home =
        crate::daemon::home_dir().context("No home directory to keep the executor's state in")?;
    Ok(home.join(".ciabatta").join("oven"))
}

fn save(path: &Path, saved: &Saved) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let text = serde_json::to_string_pretty(saved)?;
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, text)?;
    // The token is a credential for the cache.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
    }
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// A new pairing code: six digits, shown as `123-456`.
fn new_code() -> String {
    use rand::Rng;
    let n: u32 = rand::thread_rng().gen_range(0..1_000_000);
    format!("{:03}-{:03}", n / 1000, n % 1000)
}

/// Digits only, so `123 456`, `123-456` and `123456` are the same code.
fn digits(code: &str) -> String {
    code.chars().filter(char::is_ascii_digit).collect()
}

/// The machine's name, as a person would recognise it.
fn host_name() -> String {
    std::env::var("HOSTNAME")
        .ok()
        .filter(|h| !h.trim().is_empty())
        .or_else(|| {
            std::process::Command::new("hostname")
                .output()
                .ok()
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                .filter(|h| !h.is_empty())
        })
        .unwrap_or_else(|| "executor".to_string())
}

/// The address other machines most likely reach this one on: the one the
/// default route leaves from. Nothing is sent — connecting a UDP socket only
/// picks the interface.
fn primary_ip() -> Option<std::net::IpAddr> {
    let socket = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("192.0.2.1:9").ok()?;
    socket.local_addr().ok().map(|a| a.ip())
}

/// Run the executor until Ctrl-C.
pub async fn run(options: Options) -> Result<()> {
    let dir = match &options.dir {
        // Absolute from here on: git and every job run relative to it.
        Some(dir) => {
            std::fs::create_dir_all(dir)
                .with_context(|| format!("Failed to create {}", dir.display()))?;
            dir.canonicalize()?
        }
        None => state_dir()?,
    };
    let saved_path = dir.join("executor.json");
    let mut saved: Saved = std::fs::read_to_string(&saved_path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default();
    if saved.id.is_empty() {
        saved.id = uuid::Uuid::new_v4().to_string();
    }
    if options.unpair {
        saved.pairing = None;
    }
    save(&saved_path, &saved)?;

    let work_dir = match &options.work_dir {
        Some(dir) => dir.clone(),
        None => dir.join("work"),
    };
    std::fs::create_dir_all(&work_dir)
        .with_context(|| format!("Failed to create {}", work_dir.display()))?;
    let work_dir = work_dir.canonicalize()?;
    clear_stale_jobs(&work_dir);

    let name = options.name.clone().unwrap_or_else(host_name);
    let shared = Arc::new(Shared {
        info: ExecutorInfo {
            id: saved.id.clone(),
            name: name.clone(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            platform: super::platform(),
            paired_with: None,
        },
        code: Mutex::new(new_code()),
        attempts: AtomicU32::new(0),
        saved: Mutex::new(saved),
        saved_path,
        paired: tokio::sync::Notify::new(),
        mdns: Mutex::new(None),
    });

    let listener = tokio::net::TcpListener::bind(("0.0.0.0", options.port))
        .await
        .with_context(|| {
            format!(
                "Failed to listen on port {} — is another executor running? Pass --port to use another.",
                options.port
            )
        })?;
    let port = listener.local_addr()?.port();

    let app = Router::new()
        .route("/oven/info", get(info))
        .route("/oven/pair", post(pair))
        .with_state(shared.clone());
    tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, app).await {
            eprintln!("The executor's pairing server stopped: {e}");
        }
    });

    // Before advertising: a firewall that drops mDNS or the pairing port makes
    // the executor invisible, or visible and unpairable, and neither explains
    // itself from the other machine.
    for warning in preflight(port, !options.no_mdns) {
        use owo_colors::OwoColorize;
        eprintln!("{} {warning}", "warning:".yellow().bold());
    }

    if !options.no_mdns {
        let daemon = advertise(&shared.info, port, shared.pairing().is_some());
        *shared.mdns.lock().unwrap() = daemon.map(|d| (d, port));
    }

    announce(&shared, port, &work_dir, &options);

    let worker = work(shared.clone(), work_dir, options.clone());
    tokio::select! {
        result = worker => result,
        _ = tokio::signal::ctrl_c() => {
            eprintln!("\nStopping the executor.");
            Ok(())
        }
    }
}

/// The mDNS port: UDP, multicast to 224.0.0.251 / ff02::fb.
const MDNS_PORT: u16 = 5353;

/// What could stop the cache reaching this executor, as warnings with the fix
/// in them. Best-effort and never fatal: a machine can only see its own
/// firewall's settings, not whether the network between it and the cache lets
/// anything through — traffic to itself skips the firewall, so it can't simply
/// try.
///
/// Two things need to reach it: the cache, once, on the pairing port (TCP) to
/// pair; and, for it to be discovered rather than added by address, mDNS
/// (UDP 5353, multicast). After pairing the executor only makes outgoing
/// connections, so nothing else has to be open.
fn preflight(port: u16, mdns: bool) -> Vec<String> {
    let mut warnings = Vec::new();
    match primary_ip() {
        None => warnings
            .push("this machine has no network route, so no cache can reach it.".to_string()),
        Some(ip) if ip.is_loopback() => warnings.push(format!(
            "this machine's only address is {ip}; a cache on another machine can't reach it."
        )),
        Some(_) => {}
    }
    if mdns && let Err(e) = multicast_works() {
        warnings.push(format!(
            "mDNS needs UDP multicast on port {MDNS_PORT}, which isn't available here ({e}). The \
             cache won't discover this executor; add it by address instead."
        ));
    }
    warnings.extend(firewall(port, mdns));
    warnings
}

/// Whether a socket can join the mDNS multicast group, as the advertiser will.
fn multicast_works() -> Result<()> {
    use socket2::{Domain, Protocol, Socket, Type};
    let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    socket.set_reuse_address(true)?;
    #[cfg(unix)]
    socket.set_reuse_port(true)?;
    let any = std::net::SocketAddr::from(([0, 0, 0, 0], MDNS_PORT));
    socket
        .bind(&any.into())
        .context("couldn't share port 5353")?;
    socket
        .join_multicast_v4(
            &std::net::Ipv4Addr::new(224, 0, 0, 251),
            &std::net::Ipv4Addr::UNSPECIFIED,
        )
        .context("couldn't join the multicast group")?;
    Ok(())
}

/// What the local firewall says, as far as it says it to a user who isn't root.
fn firewall(port: u16, mdns: bool) -> Vec<String> {
    let mut warnings = Vec::new();
    let run = |program: &str, args: &[&str]| -> Option<String> {
        let output = std::process::Command::new(program)
            .args(args)
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .output()
            .ok()?;
        output
            .status
            .success()
            .then(|| String::from_utf8_lossy(&output.stdout).to_string())
    };
    let ports = match mdns {
        true => format!("{port}/tcp (pairing) and {MDNS_PORT}/udp (mDNS)"),
        false => format!("{port}/tcp (pairing)"),
    };

    if cfg!(target_os = "linux") {
        // ufw keeps whether it's on in a file anyone can read; its rules it
        // keeps from everyone but root.
        let ufw_on = std::fs::read_to_string("/etc/ufw/ufw.conf")
            .is_ok_and(|conf| conf.lines().any(|l| l.trim() == "ENABLED=yes"));
        if ufw_on {
            let rules = std::fs::read_to_string("/etc/ufw/user.rules").ok();
            let allows = |needle: &str| {
                rules
                    .as_deref()
                    .is_some_and(|r| r.lines().any(|l| l.contains(needle)))
            };
            let tcp = allows(&format!("--dport {port} ")) || allows(&format!("tcp {port} "));
            let udp = !mdns || allows(&format!("--dport {MDNS_PORT} "));
            match rules {
                Some(_) if tcp && udp => {}
                Some(_) => warnings.push(format!(
                    "ufw is on and doesn't allow {ports}. Allow them with: sudo ufw allow \
                     {port}/tcp{}",
                    if mdns {
                        format!(" && sudo ufw allow {MDNS_PORT}/udp")
                    } else {
                        String::new()
                    }
                )),
                None => warnings.push(format!(
                    "ufw is on, and only root can see its rules. Make sure it allows {ports}: \
                     sudo ufw allow {port}/tcp{}",
                    if mdns {
                        format!(" && sudo ufw allow {MDNS_PORT}/udp")
                    } else {
                        String::new()
                    }
                )),
            }
        }
        if run("firewall-cmd", &["--state"]).is_some_and(|s| s.trim() == "running") {
            let open = run("firewall-cmd", &["--list-ports"]).unwrap_or_default();
            let services = run("firewall-cmd", &["--list-services"]).unwrap_or_default();
            let tcp = open.split_whitespace().any(|p| p == format!("{port}/tcp"));
            let udp = !mdns
                || services.split_whitespace().any(|s| s == "mdns")
                || open
                    .split_whitespace()
                    .any(|p| p == format!("{MDNS_PORT}/udp"));
            if !(tcp && udp) {
                warnings.push(format!(
                    "firewalld is running and doesn't allow {ports}. Allow them with: sudo \
                     firewall-cmd --permanent --add-port={port}/tcp{} && sudo firewall-cmd --reload",
                    if mdns { " --add-service=mdns" } else { "" }
                ));
            }
        }
    }

    if cfg!(target_os = "macos")
        && run(
            "/usr/libexec/ApplicationFirewall/socketfilterfw",
            &["--getglobalstate"],
        )
        .is_some_and(|s| s.contains("enabled"))
    {
        warnings.push(format!(
            "the macOS firewall is on. Allow incoming connections for ciabatta when macOS asks \
             (System Settings → Network → Firewall), or the cache can't reach {ports}."
        ));
    }

    if cfg!(windows)
        && run("netsh", &["advfirewall", "show", "currentprofile", "state"])
            .is_some_and(|s| s.to_uppercase().contains("ON"))
    {
        warnings.push(format!(
            "Windows Defender Firewall is on. Allow ciabatta when Windows asks, or open {ports} \
             from an admin prompt: netsh advfirewall firewall add rule name=ciabatta-oven \
             dir=in action=allow protocol=TCP localport={port}{}",
            if mdns {
                format!(
                    " && netsh advfirewall firewall add rule name=ciabatta-oven-mdns dir=in \
                     action=allow protocol=UDP localport={MDNS_PORT}"
                )
            } else {
                String::new()
            }
        ));
    }
    warnings
}

/// Advertise on mDNS. `None` (with a note) where multicast isn't available.
fn advertise(info: &ExecutorInfo, port: u16, paired: bool) -> Option<mdns_sd::ServiceDaemon> {
    let attempt = || -> Result<mdns_sd::ServiceDaemon> {
        let daemon = mdns_sd::ServiceDaemon::new()?;
        daemon.register(service_info(info, port, paired)?)?;
        Ok(daemon)
    };
    match attempt() {
        Ok(daemon) => Some(daemon),
        Err(e) => {
            eprintln!(
                "note: couldn't advertise on the network ({e:#}). Add this executor by address instead."
            );
            None
        }
    }
}

/// The executor's mDNS record: who it is, and whether it's paired.
fn service_info(info: &ExecutorInfo, port: u16, paired: bool) -> Result<mdns_sd::ServiceInfo> {
    let host: String = info
        .name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let properties = [
        ("id", info.id.as_str()),
        ("name", info.name.as_str()),
        ("version", info.version.as_str()),
        ("platform", info.platform.as_str()),
        ("paired", if paired { "1" } else { "0" }),
    ];
    Ok(mdns_sd::ServiceInfo::new(
        super::SERVICE_TYPE,
        &info.name,
        &format!("{host}.local."),
        "",
        port,
        &properties[..],
    )?
    .enable_addr_auto())
}

/// What a person at this machine needs to add it to a cache.
fn announce(shared: &Shared, port: u16, work_dir: &Path, options: &Options) {
    use owo_colors::OwoColorize;
    let address = match primary_ip() {
        Some(std::net::IpAddr::V4(ip)) => format!("http://{ip}:{port}"),
        Some(std::net::IpAddr::V6(ip)) => format!("http://[{ip}]:{port}"),
        None => format!("http://<this machine>:{port}"),
    };
    let info = &shared.info;
    println!("{} {}", "ciabatta oven executor".bold(), info.name.bold());
    println!("  id            {}", info.id);
    println!("  address       {address}");
    println!(
        "  pairing code  {}",
        shared.code.lock().unwrap().bold().yellow()
    );
    println!(
        "  platform      {} · ciabatta {}",
        info.platform, info.version
    );
    println!("  work dir      {}", work_dir.display());
    println!(
        "  listens on    {port}/tcp for pairing{}",
        match options.no_mdns {
            true => String::new(),
            false => format!(" · advertises on {MDNS_PORT}/udp (mDNS)"),
        }
    );
    if !options.allow_repo.is_empty() {
        println!("  builds only   {}", options.allow_repo.join(", "));
    }
    println!();
    match shared.pairing() {
        Some(pairing) => println!(
            "Paired with {} — waiting for jobs. Pair again with the code above to move it to another cache.",
            pairing.server
        ),
        None => println!(
            "Not paired yet. On the remote cache's Executors page, add {} (it's listed there if the \
             cache is on this network), or add it manually with the address and pairing code above.",
            info.name
        ),
    }
}

async fn info(State(shared): State<Arc<Shared>>) -> Json<ExecutorInfo> {
    let mut info = shared.info.clone();
    info.paired_with = shared.pairing().map(|p| p.server);
    Json(info)
}

async fn pair(
    State(shared): State<Arc<Shared>>,
    Json(request): Json<PairRequest>,
) -> impl IntoResponse {
    let expected = digits(&shared.code.lock().unwrap());
    let given = digits(&request.code);
    if !crate::remote_cache::auth::constant_time_eq(&expected, &given) {
        let attempts = shared.attempts.fetch_add(1, Ordering::Relaxed) + 1;
        eprintln!(
            "A pairing attempt from {} used the wrong code.",
            request.server
        );
        if attempts >= MAX_ATTEMPTS {
            let code = new_code();
            eprintln!("Too many wrong codes; the pairing code is now {code}.");
            *shared.code.lock().unwrap() = code;
            shared.attempts.store(0, Ordering::Relaxed);
        }
        return (StatusCode::FORBIDDEN, "wrong pairing code").into_response();
    }
    if request.token.len() < 16 || request.server.trim().is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            "a pairing needs a server and a token",
        )
            .into_response();
    }
    let pairing = Pairing {
        server: request.server.trim_end_matches('/').to_string(),
        token: request.token,
    };
    if let Err(e) = shared.set_pairing(Some(pairing.clone())) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("couldn't save the pairing: {e:#}"),
        )
            .into_response();
    }
    // A code is good for one pairing.
    *shared.code.lock().unwrap() = new_code();
    shared.attempts.store(0, Ordering::Relaxed);
    println!("Paired with {} — waiting for jobs.", pairing.server);
    shared.paired.notify_waiters();
    Json(shared.info.clone()).into_response()
}

/// How the executor talks to its cache.
struct Cache {
    http: reqwest::Client,
    server: String,
    token: String,
    id: String,
}

/// The cache no longer knows this executor.
#[derive(Debug)]
struct Unpaired;

impl std::fmt::Display for Unpaired {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("this executor isn't paired with the cache any more")
    }
}
impl std::error::Error for Unpaired {}

impl Cache {
    fn new(pairing: &Pairing, id: &str, tls_verify: bool) -> Result<Self> {
        let http = reqwest::Client::builder()
            .danger_accept_invalid_certs(!tls_verify)
            .build()?;
        Ok(Cache {
            http,
            server: pairing.server.clone(),
            token: pairing.token.clone(),
            id: id.to_string(),
        })
    }

    fn request(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        self.http
            .request(method, format!("{}{path}", self.server))
            .bearer_auth(&self.token)
            .header("x-ciabatta-executor", &self.id)
    }

    async fn send<T: serde::de::DeserializeOwned>(
        &self,
        request: reqwest::RequestBuilder,
    ) -> Result<T> {
        let response = request
            .send()
            .await
            .with_context(|| format!("couldn't reach {}", self.server))?;
        let status = response.status();
        if status == reqwest::StatusCode::UNAUTHORIZED {
            return Err(Unpaired.into());
        }
        let body = response.text().await.unwrap_or_default();
        if !status.is_success() {
            let message = serde_json::from_str::<serde_json::Value>(&body)
                .ok()
                .and_then(|v| v["error"].as_str().map(str::to_string))
                .unwrap_or(body);
            bail!("{status}: {message}");
        }
        serde_json::from_str(&body).context("the cache answered with something unexpected")
    }

    async fn poll(&self) -> Result<Option<Job>> {
        let polled: Poll = self
            .send(
                self.request(
                    reqwest::Method::POST,
                    &format!("/api/oven/executors/{}/poll", self.id),
                )
                .timeout(Duration::from_secs(POLL_SECONDS + 15))
                .json(&serde_json::json!({})),
            )
            .await?;
        Ok(polled.job)
    }

    async fn heartbeat(&self, job: Option<u32>) -> Result<bool> {
        let answer: HeartbeatReply = self
            .send(
                self.request(
                    reqwest::Method::POST,
                    &format!("/api/oven/executors/{}/heartbeat", self.id),
                )
                .timeout(Duration::from_secs(15))
                .json(&Heartbeat { job }),
            )
            .await?;
        Ok(answer.cancel)
    }

    async fn log(&self, job: u32, lines: Vec<String>) -> Result<()> {
        let _: serde_json::Value = self
            .send(
                self.request(reqwest::Method::POST, &format!("/api/oven/jobs/{job}/log"))
                    .timeout(Duration::from_secs(30))
                    .json(&LogBatch { lines }),
            )
            .await?;
        Ok(())
    }

    async fn upload(&self, job: u32, path: &str, bytes: Vec<u8>) -> Result<OutputFile> {
        let encoded: Vec<String> = path.split('/').map(url_encode).collect();
        self.send(
            self.request(
                reqwest::Method::PUT,
                &format!("/api/oven/jobs/{job}/outputs/{}", encoded.join("/")),
            )
            .body(bytes),
        )
        .await
    }

    async fn finish(&self, job: u32, finish: &Finish) -> Result<()> {
        let _: serde_json::Value = self
            .send(
                self.request(
                    reqwest::Method::POST,
                    &format!("/api/oven/jobs/{job}/finish"),
                )
                .timeout(Duration::from_secs(30))
                .json(finish),
            )
            .await?;
        Ok(())
    }
}

/// Percent-encode one path segment.
pub(crate) fn url_encode(segment: &str) -> String {
    let mut out = String::new();
    for byte in segment.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// Take jobs, one at a time, for as long as it is paired.
async fn work(shared: Arc<Shared>, work_dir: PathBuf, options: Options) -> Result<()> {
    let mut quiet_errors = 0u32;
    loop {
        let Some(pairing) = shared.pairing() else {
            shared.paired.notified().await;
            continue;
        };
        let cache = Cache::new(&pairing, &shared.info.id, options.tls_verify)?;
        match cache.poll().await {
            Ok(Some(job)) => {
                quiet_errors = 0;
                run_job(&cache, &job, &work_dir, &options).await;
            }
            Ok(None) => quiet_errors = 0,
            Err(e) if e.is::<Unpaired>() => {
                eprintln!(
                    "{} doesn't know this executor any more (it was removed, or paired \
                     somewhere else). Add it again with the pairing code {}.",
                    pairing.server,
                    shared.code.lock().unwrap()
                );
                shared.set_pairing(None)?;
            }
            Err(e) => {
                // Said once, not every five seconds while the cache is down.
                if quiet_errors == 0 {
                    eprintln!("Waiting for the cache: {e:#}");
                }
                quiet_errors += 1;
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        }
    }
}

/// Run one job to the end, reporting everything to the cache. Never fails:
/// whatever goes wrong is what the job's result says.
async fn run_job(cache: &Cache, job: &Job, work_dir: &Path, options: &Options) {
    use owo_colors::OwoColorize;
    let short = job.short_commit();
    println!(
        "{} job {}: {} at {short} for {}",
        "▶".cyan(),
        job.id,
        job.spec().workflows.join(" "),
        if job.requested_by.is_empty() {
            "anonymous"
        } else {
            &job.requested_by
        }
    );

    // Lines go out in batches, every half-second.
    let (line_tx, mut line_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let shipper = async {
        let mut batch = Vec::new();
        let mut tick = tokio::time::interval(Duration::from_millis(500));
        loop {
            tokio::select! {
                line = line_rx.recv() => match line {
                    Some(line) => batch.push(line),
                    None => break,
                },
                _ = tick.tick() => {
                    if !batch.is_empty() {
                        let lines = std::mem::take(&mut batch);
                        if let Err(e) = cache.log(job.id, lines).await {
                            tracing::debug!("couldn't send log lines: {e:#}");
                        }
                    }
                }
            }
        }
        if !batch.is_empty() {
            let _ = cache.log(job.id, batch).await;
        }
    };

    // The heartbeat, which is also how a cancel arrives.
    let (cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);
    let beat = async {
        let mut tick = tokio::time::interval(Duration::from_secs(3));
        loop {
            tick.tick().await;
            if let Ok(true) = cache.heartbeat(Some(job.id)).await {
                let _ = cancel_tx.send(true);
            }
        }
    };

    let running = execute(cache, job, work_dir, options, line_tx, cancel_rx);

    // The job decides when this ends. The shipper runs alongside it, and
    // finishes once the job has dropped every sender and the last batch is out.
    let job_and_beat = async {
        tokio::select! {
            finish = running => finish,
            () = beat => unreachable!("the heartbeat never ends"),
        }
    };
    let (finish, ()) = tokio::join!(job_and_beat, shipper);

    let state = finish.state();
    if let Err(e) = cache.finish(job.id, &finish).await {
        eprintln!("Couldn't report job {}'s result: {e:#}", job.id);
    }
    let _ = cache.heartbeat(None).await;
    let mark = match state {
        JobState::Succeeded => "✓".green().to_string(),
        JobState::Cancelled => "■".yellow().to_string(),
        _ => "✗".red().to_string(),
    };
    println!(
        "{mark} job {} {}{}",
        job.id,
        state.word(),
        finish
            .message
            .as_deref()
            .map(|m| format!(": {m}"))
            .unwrap_or_default()
    );
}

/// Fetch, check out, run, collect. The [`Finish`] to report.
async fn execute(
    cache: &Cache,
    job: &Job,
    work_dir: &Path,
    options: &Options,
    lines: tokio::sync::mpsc::UnboundedSender<String>,
    mut cancel: tokio::sync::watch::Receiver<bool>,
) -> Finish {
    let failed = |message: String| Finish {
        state: JobState::Failed.into(),
        message: Some(message),
        ciabatta_path: None,
        outputs: Vec::new(),
    };
    let say = |line: String| {
        let _ = lines.send(line);
    };

    if !repo_allowed(&job.spec().repo, &options.allow_repo) {
        let message = format!(
            "this executor only builds {}, not {}",
            options.allow_repo.join(", "),
            job.spec().repo
        );
        say(format!("✗ {message}"));
        return failed(message);
    }

    let checkout = work_dir.join("jobs").join(job.id.to_string());
    let prepared = {
        let lines = lines.clone();
        let spec = job.spec().clone();
        let work_dir = work_dir.to_path_buf();
        let checkout = checkout.clone();
        tokio::task::spawn_blocking(move || prepare(&spec, &work_dir, &checkout, &lines)).await
    };
    match prepared {
        Ok(Ok(())) => {}
        Ok(Err(e)) => {
            say(format!("✗ {e:#}"));
            cleanup(work_dir, &checkout, &job.spec().repo, None, &lines);
            return failed(format!("{e:#}"));
        }
        Err(e) => return failed(format!("{e}")),
    }

    let root = checkout.join(&job.spec().root);
    let cwd = root.join(&job.spec().cwd);
    let command = workflow_command(job);
    say(format!(
        "▶ {}",
        command
            .iter()
            .map(|a| shell_quote(a))
            .collect::<Vec<_>>()
            .join(" ")
    ));

    let session = match spawn_job(&command, &cwd, job.id, &lines) {
        Ok(session) => session,
        Err(e) => {
            say(format!("✗ {e:#}"));
            cleanup(work_dir, &checkout, &job.spec().repo, None, &lines);
            return failed(format!("{e:#}"));
        }
    };
    let leader = session.leader;
    let result = tokio::select! {
        status = session.wait() => Some(status),
        _ = cancel.wait_for(|c| *c) => None,
    };
    if result.is_none() {
        say("■ cancelled — stopping the run".to_string());
        stop_session(leader, Duration::from_secs(5)).await;
    }

    let state = match &result {
        None => JobState::Cancelled,
        Some(Ok(status)) if status.success() => JobState::Succeeded,
        Some(_) => JobState::Failed,
    };
    let message = match &result {
        None => Some("cancelled".to_string()),
        Some(Ok(status)) if status.success() => None,
        Some(Ok(_)) => Some(format!("{} failed", job.spec().workflows.join(" "))),
        Some(Err(e)) => Some(format!("{e:#}")),
    };

    let ciabatta_path = ciabatta_path(&root, &job.spec().env);
    let mut outputs = Vec::new();
    if state != JobState::Cancelled {
        match collect_outputs(&cwd, job) {
            Ok(files) if files.is_empty() => say(
                "No outputs to send back: only files a step declares under `cache: outputs:` are \
                 returned."
                    .to_string(),
            ),
            Ok(files) => {
                say(format!("Sending back {} output file(s)…", files.len()));
                for file in files {
                    match std::fs::read(root.join(&file)) {
                        Ok(bytes) => match cache.upload(job.id, &file, bytes).await {
                            Ok(sent) => outputs.push(sent),
                            Err(e) => say(format!("✗ couldn't send {file}: {e:#}")),
                        },
                        Err(e) => say(format!("✗ couldn't read {file}: {e}")),
                    }
                }
            }
            Err(e) => say(format!("✗ couldn't work out the run's outputs: {e:#}")),
        }
    }

    cleanup(work_dir, &checkout, &job.spec().repo, Some(leader), &lines);
    drop(lines);
    Finish {
        state: state.into(),
        message,
        ciabatta_path,
        outputs,
    }
}

/// A job's run: the `ciabatta workflow` process, leading a session of its
/// own, so that everything it starts — its steps, their process groups, what
/// they leave in the background — can be found and stopped by that session,
/// whatever groups they've made for themselves underneath.
struct Session {
    child: tokio::process::Child,
    /// The session's id, which is the leader's pid.
    leader: u32,
    readers: Vec<tokio::task::JoinHandle<()>>,
}

impl Session {
    /// Wait for the run to exit, then for the rest of its output.
    async fn wait(mut self) -> Result<std::process::ExitStatus> {
        let status = self.child.wait().await?;
        // A background process left holding the pipes would keep a reader
        // open forever; give the output a moment, then let it go.
        for reader in self.readers {
            let _ = tokio::time::timeout(Duration::from_secs(2), reader).await;
        }
        Ok(status)
    }
}

/// Start the run in a new session, its output line by line into the log.
fn spawn_job(
    command: &[String],
    cwd: &Path,
    job: u32,
    lines: &tokio::sync::mpsc::UnboundedSender<String>,
) -> Result<Session> {
    use tokio::io::AsyncBufReadExt;
    let mut cmd = tokio::process::Command::new(&command[0]);
    cmd.args(&command[1..])
        .current_dir(cwd)
        .env(super::JOB_VAR, job.to_string())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        // Dropped mid-run (the executor stopping), the run goes with it.
        .kill_on_drop(true);
    crate::color::request(&mut cmd);
    #[cfg(unix)]
    // SAFETY: `setsid` is async-signal-safe, and it is the only call made
    // between fork and exec.
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = cmd
        .spawn()
        .with_context(|| format!("Failed to start {}", command[0]))?;
    let leader = child.id().context("the run exited before it started")?;

    let mut readers = Vec::new();
    if let Some(out) = child.stdout.take() {
        let lines = lines.clone();
        readers.push(tokio::spawn(async move {
            let mut out = tokio::io::BufReader::new(out).lines();
            while let Ok(Some(line)) = out.next_line().await {
                let _ = lines.send(line);
            }
        }));
    }
    if let Some(err) = child.stderr.take() {
        let lines = lines.clone();
        readers.push(tokio::spawn(async move {
            let mut err = tokio::io::BufReader::new(err).lines();
            while let Ok(Some(line)) = err.next_line().await {
                let _ = lines.send(line);
            }
        }));
    }
    Ok(Session {
        child,
        leader,
        readers,
    })
}

/// Every process still in session `sid`, the leader included.
#[cfg(target_os = "linux")]
fn session_pids(sid: u32) -> Vec<i32> {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|e| e.file_name().to_str()?.parse::<i32>().ok())
        .filter(|pid| {
            // `/proc/<pid>/stat`: the session is the fourth field after the
            // command, which is parenthesised and may itself contain spaces.
            std::fs::read_to_string(format!("/proc/{pid}/stat"))
                .ok()
                .and_then(|stat| {
                    let rest = &stat[stat.rfind(')')? + 2..];
                    rest.split_whitespace().nth(3)?.parse::<u32>().ok()
                })
                == Some(sid)
        })
        .collect()
}

/// Every process still in session `sid`, from `ps`.
#[cfg(all(unix, not(target_os = "linux")))]
fn session_pids(sid: u32) -> Vec<i32> {
    let Ok(output) = std::process::Command::new("ps")
        .args(["-A", "-o", "pid=", "-o", "sess="])
        .output()
    else {
        return Vec::new();
    };
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            let pid = parts.next()?.parse::<i32>().ok()?;
            let sess = parts.next()?.parse::<u32>().ok()?;
            (sess == sid).then_some(pid)
        })
        .collect()
}

/// Signal every process in session `sid`.
#[cfg(unix)]
fn signal_session(sid: u32, signal: i32) -> usize {
    let pids = session_pids(sid);
    for pid in &pids {
        // SAFETY: a plain kill(2) on pids just read from the process table; one
        // that has exited since is an ESRCH, which is fine.
        unsafe {
            libc::kill(*pid, signal);
        }
    }
    pids.len()
}

/// Ask the session to stop, then make it.
async fn stop_session(sid: u32, grace: Duration) {
    #[cfg(unix)]
    {
        signal_session(sid, libc::SIGTERM);
        let deadline = tokio::time::Instant::now() + grace;
        while tokio::time::Instant::now() < deadline && !session_pids(sid).is_empty() {
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        signal_session(sid, libc::SIGKILL);
    }
    #[cfg(windows)]
    {
        let _ = grace;
        let _ = std::process::Command::new("taskkill")
            .args(["/PID", &sid.to_string(), "/T", "/F"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }
}

/// Mirrors not used for this long are deleted.
const MIRROR_KEPT_FOR: Duration = Duration::from_secs(30 * 24 * 3600);

/// After every job, however it ended: nothing of it left running, its
/// checkout gone, its mirror tidied, and mirrors nobody has used in a month
/// removed — so an executor that runs for months doesn't fill its disk.
fn cleanup(
    work_dir: &Path,
    checkout: &Path,
    repo: &str,
    session: Option<u32>,
    lines: &tokio::sync::mpsc::UnboundedSender<String>,
) {
    let say = |line: String| {
        let _ = lines.send(line);
    };
    say("▶ cleaning up".to_string());
    #[cfg(unix)]
    if let Some(sid) = session {
        let left = signal_session(sid, libc::SIGKILL);
        if left > 0 {
            say(format!(
                "  stopped {left} process{} the run left running",
                if left == 1 { "" } else { "es" }
            ));
        }
    }
    #[cfg(not(unix))]
    let _ = session;
    if let Err(e) = std::fs::remove_dir_all(checkout)
        && e.kind() != std::io::ErrorKind::NotFound
    {
        say(format!("  couldn't remove {}: {e}", checkout.display()));
    }
    let mirror = mirror_for(work_dir, repo);
    if mirror.is_dir() {
        let _ = std::process::Command::new("git")
            .arg("-C")
            .arg(&mirror)
            .args(["gc", "--auto", "--quiet"])
            .stdin(std::process::Stdio::null())
            .output();
    }
    prune_mirrors(work_dir, &mirror);
}

/// Where `repo`'s mirror lives.
fn mirror_for(work_dir: &Path, repo: &str) -> PathBuf {
    let key = &crate::cache::hash_bytes(repo.as_bytes())[..16];
    work_dir.join("repos").join(format!("{key}.git"))
}

/// Delete mirrors unused for [`MIRROR_KEPT_FOR`], except `keep`.
fn prune_mirrors(work_dir: &Path, keep: &Path) {
    let Ok(entries) = std::fs::read_dir(work_dir.join("repos")) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path == keep {
            continue;
        }
        let used = std::fs::metadata(path.join(".last-used"))
            .or_else(|_| std::fs::metadata(&path))
            .and_then(|m| m.modified());
        if used.is_ok_and(|at| at.elapsed().is_ok_and(|age| age > MIRROR_KEPT_FOR)) {
            let _ = std::fs::remove_dir_all(&path);
        }
    }
}

/// Checkouts a previous run of the executor left behind when it stopped
/// mid-job.
fn clear_stale_jobs(work_dir: &Path) {
    let _ = std::fs::remove_dir_all(work_dir.join("jobs"));
}

/// Whether `repo` is one this executor may build.
fn repo_allowed(repo: &str, allow: &[String]) -> bool {
    allow.is_empty()
        || allow.iter().any(|pattern| {
            pattern == repo || glob::Pattern::new(pattern).is_ok_and(|p| p.matches(repo))
        })
}

/// Run git, its output into the job's log. Never asks for a password: there
/// is nobody here to answer.
fn git(
    dir: Option<&Path>,
    args: &[&str],
    lines: &tokio::sync::mpsc::UnboundedSender<String>,
) -> Result<()> {
    let mut command = std::process::Command::new("git");
    if let Some(dir) = dir {
        command.arg("-C").arg(dir);
    }
    let output = command
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(std::process::Stdio::null())
        .output()
        .context("Failed to run git — is it installed on the executor?")?;
    for line in String::from_utf8_lossy(&output.stderr)
        .lines()
        .chain(String::from_utf8_lossy(&output.stdout).lines())
    {
        let _ = lines.send(format!("  {line}"));
    }
    if !output.status.success() {
        bail!("git {} failed", args.first().copied().unwrap_or_default());
    }
    Ok(())
}

/// The mirror for `repo`, fetched up to date, and the job's commit checked out
/// from it into `checkout` on the job's branch.
fn prepare(
    spec: &super::JobSpec,
    work_dir: &Path,
    checkout: &Path,
    lines: &tokio::sync::mpsc::UnboundedSender<String>,
) -> Result<()> {
    let say = |line: String| {
        let _ = lines.send(line);
    };
    let mirror = mirror_for(work_dir, &spec.repo);
    std::fs::create_dir_all(work_dir.join("repos"))?;

    if mirror.join("HEAD").is_file() {
        say(format!("▶ fetching {}", spec.repo));
        git(Some(&mirror), &["remote", "update", "--prune"], lines)
            .with_context(|| format!("couldn't fetch {}", spec.repo))?;
    } else {
        say(format!("▶ cloning {}", spec.repo));
        let target = mirror.to_string_lossy().into_owned();
        git(
            None,
            &["clone", "--mirror", "--quiet", &spec.repo, &target],
            lines,
        )
        .with_context(|| format!("couldn't clone {} — can this machine reach it?", spec.repo))?;
    }
    // What [`prune_mirrors`] goes by.
    let _ = std::fs::write(mirror.join(".last-used"), crate::cache::store::now());
    let wanted = format!("{}^{{commit}}", spec.commit);
    if git(Some(&mirror), &["cat-file", "-e", &wanted], lines).is_err() {
        git(
            Some(&mirror),
            &["fetch", "--quiet", "origin", &spec.commit],
            lines,
        )
        .with_context(|| {
            format!(
                "{} has no commit {} — was it pushed?",
                spec.repo, spec.commit
            )
        })?;
    }

    let _ = std::fs::remove_dir_all(checkout);
    std::fs::create_dir_all(checkout.parent().unwrap_or(work_dir))?;
    let mirror_path = mirror.to_string_lossy().into_owned();
    let checkout_path = checkout.to_string_lossy().into_owned();
    git(
        None,
        &[
            "clone",
            "--quiet",
            "--shared",
            "--no-checkout",
            &mirror_path,
            &checkout_path,
        ],
        lines,
    )?;
    say(format!(
        "▶ checking out {}{}",
        &spec.commit[..spec.commit.len().min(12)],
        spec.branch
            .as_deref()
            .map(|b| format!(" on {b}"))
            .unwrap_or_default()
    ));
    match &spec.branch {
        // On the branch, so the run's CIABATTA_BRANCH — and CIABATTA_PATH — is
        // what it would have been where it was asked for.
        Some(branch) => git(
            Some(checkout),
            &["checkout", "--quiet", "-B", branch, &spec.commit],
            lines,
        )?,
        None => git(
            Some(checkout),
            &["checkout", "--quiet", "--detach", &spec.commit],
            lines,
        )?,
    }
    // A step that reads `origin` gets the real repository, not the mirror.
    git(
        Some(checkout),
        &["remote", "set-url", "origin", &spec.repo],
        lines,
    )?;
    if checkout.join(".gitmodules").is_file() {
        git(
            Some(checkout),
            &["submodule", "update", "--init", "--recursive", "--quiet"],
            lines,
        )?;
    }
    Ok(())
}

/// The ciabatta binary to run jobs with: this one — or, when this one has been
/// replaced on disk since it started (`ciabatta self update`, a rebuild), the
/// one now at the same path. Linux names a replaced binary `… (deleted)`.
fn own_exe() -> String {
    let exe = std::env::current_exe()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| "ciabatta".to_string());
    match exe.strip_suffix(" (deleted)") {
        Some(path) if Path::new(path).is_file() => path.to_string(),
        _ => exe,
    }
}

/// The `ciabatta workflow …` command a job runs.
fn workflow_command(job: &Job) -> Vec<String> {
    let spec = job.spec();
    let exe = own_exe();
    let mut command = vec![exe, "workflow".to_string()];
    command.extend(spec.workflows.iter().cloned());
    command.extend(["--viz".to_string(), "terminal".to_string()]);
    for member in &spec.only {
        command.extend(["--only".to_string(), member.clone()]);
    }
    if spec.solo {
        command.push("--solo".to_string());
    }
    if spec.all {
        command.push("--all".to_string());
    }
    for term in &spec.filter {
        command.extend(["--filter".to_string(), term.clone()]);
    }
    for var in &spec.env {
        command.extend(["-e".to_string(), var.clone()]);
    }
    if spec.force {
        command.push("--force".to_string());
    }
    if !spec.args.is_empty() {
        command.push("...".to_string());
        command.extend(spec.args.iter().cloned());
    }
    command
}

fn shell_quote(value: &str) -> String {
    if !value.is_empty()
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_./=:@,+".contains(c))
    {
        return value.to_string();
    }
    format!("'{}'", value.replace('\'', r"'\''"))
}

/// The `CIABATTA_PATH` the run had: from the checkout's git, with the job's
/// own `-e` flags over it, as the run itself derives it.
fn ciabatta_path(root: &Path, env: &[String]) -> Option<String> {
    let mut vars = crate::git::local_git_vars(root).ok()?;
    if let Ok(flags) = crate::cli::parse_env_flags(env) {
        vars.extend(flags);
    }
    if let Some(path) = vars.get("CIABATTA_PATH").filter(|p| !p.is_empty()) {
        return Some(path.clone());
    }
    crate::derive_ciabatta_path(&vars)
}

/// Every file the job's steps declare as cache outputs that exists now,
/// relative to the workspace root.
fn collect_outputs(cwd: &Path, job: &Job) -> Result<Vec<String>> {
    use crate::cache::graph::StepContext;
    let mut provided: Vec<String> = crate::cli::parse_env_flags(&job.spec().env)?
        .into_keys()
        .collect();
    provided.extend(crate::cli::parse_custom_args(&job.spec().args)?.into_keys());
    let selection = crate::workspace::graph::Selection {
        only: job.spec().only.clone(),
        solo: job.spec().solo,
        provided,
        skip_env_check: true,
    };
    let (ws, graph) =
        crate::workspace::graph::prepare_many(cwd, &job.spec().workflows, &selection)?;
    let config = crate::config::load_config(&ws.root).unwrap_or_default();
    let context = crate::cache::cli::WorkspaceContext {
        workspace: Some(&ws),
        root: ws.root.clone(),
        config: &config,
    };
    let mut files = std::collections::BTreeSet::new();
    for step in graph.steps.iter().filter(|s| !s.recover) {
        let cache = context.cache_config(step);
        if cache.outputs.is_empty() && cache.cargo.is_none() {
            continue;
        }
        for file in cache.list_outputs(&context.dir(step)).unwrap_or_default() {
            files.insert(file.path);
        }
    }
    Ok(files.into_iter().collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_code_is_its_digits_however_it_is_typed() {
        assert_eq!(digits("123-456"), "123456");
        assert_eq!(digits(" 123 456 "), "123456");
        let code = new_code();
        assert_eq!(code.len(), 7);
        assert_eq!(digits(&code).len(), 6);
    }

    #[test]
    fn the_allowlist_takes_exact_urls_and_globs() {
        assert!(repo_allowed("git@github.com:me/x.git", &[]));
        let allow = vec!["git@github.com:me/*".to_string()];
        assert!(repo_allowed("git@github.com:me/x.git", &allow));
        assert!(!repo_allowed("git@github.com:them/x.git", &allow));
    }

    #[test]
    fn the_job_becomes_the_workflow_command_it_asked_for() {
        let job = Job {
            id: 1,
            spec: Some(super::super::JobSpec {
                workflows: vec!["build".into(), "test".into()],
                only: vec!["api".into()],
                solo: true,
                filter: vec!["tag:fast".into()],
                env: vec!["MODE=release".into()],
                args: vec!["--target=arm".into()],
                force: true,
                ..Default::default()
            }),
            state: JobState::Running.into(),
            requested_by: String::new(),
            created_at: String::new(),
            executor: None,
            started_at: None,
            finished_at: None,
            cancel_requested: false,
            message: None,
            ciabatta_path: None,
            outputs: vec![],
            log_lines: 0,
        };
        let command = workflow_command(&job);
        assert_eq!(
            command[1..].join(" "),
            "workflow build test --viz terminal --only api --solo --filter tag:fast \
             -e MODE=release --force ... --target=arm"
        );
        assert_eq!(shell_quote("it's"), r"'it'\''s'");
        assert_eq!(shell_quote("tag:fast"), "tag:fast");
    }

    #[test]
    fn segments_are_percent_encoded() {
        assert_eq!(url_encode("my file#1.txt"), "my%20file%231.txt");
    }
}
