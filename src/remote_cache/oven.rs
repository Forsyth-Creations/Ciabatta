//! The remote cache's side of offloading: executors, and the jobs they run.
//!
//! See [`crate::oven`] for the whole picture. What lives here:
//!
//! * **The executors paired with this cache**, in `storage/oven/executors.json`
//!   — each one's id, name, and the SHA-256 of the token it was given. Never
//!   the token itself, for the same reason [`super::users`] never keeps one.
//! * **The jobs**, one directory each under `storage/oven/jobs/<id>/`: the job
//!   as JSON, its log as lines, and the files it sent back under `outputs/`.
//!   The newest [`MAX_JOBS`] are kept.
//! * **What is on the network**, from mDNS, so the page can offer an executor
//!   nobody has typed an address for.
//!
//! Executors *pull*. Each one holds a long poll open on
//! `/api/oven/executors/{id}/poll`, and a job submitted while it's waiting is
//! handed to it at once. An executor is given one job at a time and asks for
//! another only when it has finished, which is what keeps it to one.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};
use axum::{
    Json, Router,
    body::Bytes,
    extract::{Path as UrlPath, Query, State},
    http::{HeaderMap, StatusCode, header},
    response::IntoResponse,
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::server::{ApiError, ApiResult, AppState};
use crate::oven::{
    Availability, Discovered, ExecutorInfo, ExecutorList, ExecutorState, ExecutorView, Finish,
    Heartbeat, HeartbeatReply, Job, JobList, JobSpec, JobState, LogBatch, LogPage,
    OFFLINE_AFTER_SECONDS, OutputFile, POLL_SECONDS, PairRequest, PairWith, Paired, Poll,
};

/// How many jobs are kept, with their logs and outputs. Older ones go when a
/// new one is submitted.
pub const MAX_JOBS: usize = 200;

/// A log longer than this stops growing; the executor is told so once.
const MAX_LOG_LINES: usize = 200_000;

/// A running job whose executor has been silent this long is given up on.
const ABANDONED_AFTER_SECONDS: i64 = 300;

/// An executor this cache has paired with.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct PairedExecutor {
    id: String,
    name: String,
    token_sha256: String,
    address: String,
    #[serde(default)]
    version: String,
    #[serde(default)]
    platform: String,
    added_at: String,
    #[serde(default)]
    added_by: String,
}

/// What an executor last said, which isn't worth writing to disk.
#[derive(Debug, Clone, Default)]
struct Live {
    last_seen: Option<chrono::DateTime<chrono::Utc>>,
    job: Option<u32>,
}

#[derive(Debug, Default)]
struct Inner {
    executors: Vec<PairedExecutor>,
    live: HashMap<String, Live>,
    jobs: BTreeMap<u32, Job>,
    next_id: u32,
}

/// Executors and jobs, for one cache server.
#[derive(Debug)]
pub struct Oven {
    dir: PathBuf,
    inner: Mutex<Inner>,
    /// Woken when there is something for a waiting executor to look at: a job
    /// submitted, or one cancelled.
    work: tokio::sync::Notify,
    /// What mDNS has found, by service name.
    discovered: Mutex<HashMap<String, Discovered>>,
}

impl Oven {
    /// Open (or start) the store under `storage`.
    pub fn open(storage: &Path) -> Result<Self> {
        let dir = storage.join("oven");
        std::fs::create_dir_all(dir.join("jobs"))
            .with_context(|| format!("Failed to create {}", dir.display()))?;

        let executors: Vec<PairedExecutor> =
            match std::fs::read_to_string(dir.join("executors.json")) {
                Ok(text) => serde_json::from_str(&text).context("executors.json is malformed")?,
                Err(_) => Vec::new(),
            };

        let mut jobs = BTreeMap::new();
        // Counted from the directories, not the jobs that load: a number is
        // never handed out twice, so a job that can't be read is skipped
        // without the next one writing into its folder.
        let mut highest = 0;
        for entry in std::fs::read_dir(dir.join("jobs"))?.flatten() {
            if let Some(id) = entry
                .file_name()
                .to_str()
                .and_then(|n| n.parse::<u32>().ok())
            {
                highest = highest.max(id);
            }
            let path = entry.path().join("job.json");
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            match serde_json::from_str::<Job>(&text) {
                Ok(job) => {
                    jobs.insert(job.id, job);
                }
                Err(e) => tracing::warn!("skipping {}: {e}", path.display()),
            }
        }
        let next_id = highest + 1;

        let oven = Oven {
            dir,
            inner: Mutex::new(Inner {
                executors,
                live: HashMap::new(),
                jobs,
                next_id,
            }),
            work: tokio::sync::Notify::new(),
            discovered: Mutex::new(HashMap::new()),
        };
        // A job that was running when the server stopped has nobody left to
        // report on it — its executor will have been told the server is gone.
        oven.abandon_running("the remote cache restarted while it was running");
        Ok(oven)
    }

    fn job_dir(&self, id: u32) -> PathBuf {
        self.dir.join("jobs").join(id.to_string())
    }

    fn save_executors(&self, inner: &Inner) -> Result<()> {
        let text = serde_json::to_string_pretty(&inner.executors)?;
        write_atomic(&self.dir.join("executors.json"), text.as_bytes())
    }

    fn save_job(&self, job: &Job) -> Result<()> {
        let dir = self.job_dir(job.id);
        std::fs::create_dir_all(&dir)?;
        write_atomic(&dir.join("job.json"), &serde_json::to_vec_pretty(job)?)
    }

    fn abandon_running(&self, why: &str) {
        let mut inner = self.inner.lock().unwrap();
        let running: Vec<u32> = inner
            .jobs
            .values()
            .filter(|j| j.state() == JobState::Running)
            .map(|j| j.id)
            .collect();
        for id in running {
            if let Some(job) = inner.jobs.get_mut(&id) {
                job.set_state(JobState::Failed);
                job.finished_at = Some(crate::cache::store::now());
                job.message = Some(why.to_string());
                let _ = self.save_job(job);
            }
        }
    }

    // ─── Executors ──────────────────────────────────────────────────────────

    /// Every paired executor, with what it is doing.
    pub fn executors(&self) -> Vec<ExecutorView> {
        let inner = self.inner.lock().unwrap();
        let now = chrono::Utc::now();
        inner
            .executors
            .iter()
            .map(|e| {
                let live = inner.live.get(&e.id).cloned().unwrap_or_default();
                let online = live
                    .last_seen
                    .is_some_and(|at| (now - at).num_seconds() < OFFLINE_AFTER_SECONDS);
                ExecutorView {
                    id: e.id.clone(),
                    name: e.name.clone(),
                    state: match (online, live.job) {
                        (false, _) => ExecutorState::Offline,
                        (true, Some(_)) => ExecutorState::Busy,
                        (true, None) => ExecutorState::Idle,
                    }
                    .into(),
                    job: live.job,
                    address: e.address.clone(),
                    version: e.version.clone(),
                    platform: e.platform.clone(),
                    added_at: e.added_at.clone(),
                    added_by: e.added_by.clone(),
                    last_seen: live.last_seen.map(|at| at.to_rfc3339()),
                }
            })
            .collect()
    }

    /// What mDNS can see, marked with whether each is already paired here.
    pub fn discovered(&self) -> Vec<Discovered> {
        let paired: Vec<String> = self
            .inner
            .lock()
            .unwrap()
            .executors
            .iter()
            .map(|e| e.id.clone())
            .collect();
        let mut found: Vec<Discovered> = self
            .discovered
            .lock()
            .unwrap()
            .values()
            .cloned()
            .map(|mut d| {
                // The network says whether it's paired; only this cache knows
                // whether that's with this cache.
                if paired.contains(&d.id) {
                    d.set_availability(Availability::PairedHere);
                }
                d
            })
            .collect();
        found.sort_by(|a, b| a.name.cmp(&b.name));
        found
    }

    /// Whether `name` would collide with a different executor's.
    fn name_taken(&self, id: &str, name: &str) -> bool {
        self.inner
            .lock()
            .unwrap()
            .executors
            .iter()
            .any(|e| e.name.eq_ignore_ascii_case(name) && e.id != id)
    }

    /// Record a pairing. Pairing the same executor again replaces it, and
    /// with it the old token.
    fn add(&self, info: &ExecutorInfo, address: &str, token: &str, by: &str) -> Result<()> {
        let mut inner = self.inner.lock().unwrap();
        inner.executors.retain(|e| e.id != info.id);
        inner.executors.push(PairedExecutor {
            id: info.id.clone(),
            name: info.name.clone(),
            token_sha256: crate::cache::hash_bytes(token.as_bytes()),
            address: address.to_string(),
            version: info.version.clone(),
            platform: info.platform.clone(),
            added_at: crate::cache::store::now(),
            added_by: by.to_string(),
        });
        inner.executors.sort_by(|a, b| a.name.cmp(&b.name));
        self.save_executors(&inner)
    }

    /// Forget an executor. Its token stops working at once.
    fn remove(&self, id: &str) -> Result<bool> {
        let mut inner = self.inner.lock().unwrap();
        let before = inner.executors.len();
        inner.executors.retain(|e| e.id != id);
        inner.live.remove(id);
        if inner.executors.len() == before {
            return Ok(false);
        }
        self.save_executors(&inner)?;
        Ok(true)
    }

    /// Which executor a token belongs to — only `id`'s.
    fn authenticate(&self, id: &str, token: &str) -> Option<String> {
        let hash = crate::cache::hash_bytes(token.as_bytes());
        let mut inner = self.inner.lock().unwrap();
        let name = inner
            .executors
            .iter()
            .find(|e| e.id == id && super::auth::constant_time_eq(&e.token_sha256, &hash))
            .map(|e| e.name.clone())?;
        inner.live.entry(id.to_string()).or_default().last_seen = Some(chrono::Utc::now());
        Some(name)
    }

    // ─── Jobs ───────────────────────────────────────────────────────────────

    /// Queue a job. Refused when nothing could ever run it.
    pub fn submit(&self, spec: JobSpec, requested_by: &str) -> Result<Job, String> {
        let mut inner = self.inner.lock().unwrap();
        if inner.executors.is_empty() {
            return Err(
                "No executors are paired with this remote cache. Start one with \
                 `ciabatta oven executor` and add it on the cache's Executors page."
                    .to_string(),
            );
        }
        if let Some(name) = &spec.executor
            && !inner
                .executors
                .iter()
                .any(|e| e.name.eq_ignore_ascii_case(name))
        {
            let names: Vec<&str> = inner.executors.iter().map(|e| e.name.as_str()).collect();
            return Err(format!(
                "No executor is called '{name}'. This cache has: {}.",
                names.join(", ")
            ));
        }
        if spec.workflows.is_empty() || spec.commit.len() < 7 || spec.repo.trim().is_empty() {
            return Err("A job needs a repository, a commit and a workflow.".to_string());
        }

        let id = inner.next_id;
        inner.next_id += 1;
        let job = Job {
            id,
            spec: Some(spec),
            state: JobState::Queued.into(),
            requested_by: requested_by.to_string(),
            created_at: crate::cache::store::now(),
            executor: None,
            started_at: None,
            finished_at: None,
            cancel_requested: false,
            message: None,
            ciabatta_path: None,
            outputs: Vec::new(),
            log_lines: 0,
        };
        self.save_job(&job).map_err(|e| format!("{e:#}"))?;
        inner.jobs.insert(id, job.clone());
        self.prune(&mut inner);
        drop(inner);
        self.work.notify_waiters();
        Ok(job)
    }

    /// Drop the oldest finished jobs past [`MAX_JOBS`], directory and all.
    fn prune(&self, inner: &mut Inner) {
        while inner.jobs.len() > MAX_JOBS {
            let Some(oldest) = inner
                .jobs
                .values()
                .find(|j| j.state().is_done())
                .map(|j| j.id)
            else {
                break;
            };
            inner.jobs.remove(&oldest);
            let _ = std::fs::remove_dir_all(self.job_dir(oldest));
        }
    }

    pub fn job(&self, id: u32) -> Option<Job> {
        self.inner.lock().unwrap().jobs.get(&id).cloned()
    }

    /// The newest jobs first, for one project or all of them.
    pub fn jobs(&self, project: Option<&str>, limit: usize) -> Vec<Job> {
        self.inner
            .lock()
            .unwrap()
            .jobs
            .values()
            .rev()
            .filter(|j| project.is_none_or(|p| j.spec().project == p))
            .take(limit)
            .cloned()
            .collect()
    }

    /// Hand `executor` the oldest job it may run, if it has none already.
    fn take(&self, id: &str, name: &str) -> Option<Job> {
        let mut inner = self.inner.lock().unwrap();
        if let Some(current) = inner.live.get(id).and_then(|l| l.job)
            && inner
                .jobs
                .get(&current)
                .is_some_and(|j| j.state() == JobState::Running)
        {
            // Asking again while a job is running means it lost track of it
            // (a restart): give it the same one back rather than a second.
            return inner.jobs.get(&current).cloned();
        }
        let next = inner
            .jobs
            .values()
            .find(|j| {
                j.state() == JobState::Queued
                    && !j.cancel_requested
                    && j.spec()
                        .executor
                        .as_deref()
                        .is_none_or(|want| want.eq_ignore_ascii_case(name))
            })
            .map(|j| j.id)?;
        let job = inner.jobs.get_mut(&next)?;
        job.set_state(JobState::Running);
        job.executor = Some(name.to_string());
        job.started_at = Some(crate::cache::store::now());
        let job = job.clone();
        let _ = self.save_job(&job);
        inner.live.entry(id.to_string()).or_default().job = Some(job.id);
        Some(job)
    }

    /// An executor saying it is alive and what it is doing. The answer is
    /// whether to stop the job it named.
    fn heartbeat(&self, id: &str, job: Option<u32>) -> bool {
        let mut inner = self.inner.lock().unwrap();
        let live = inner.live.entry(id.to_string()).or_default();
        live.last_seen = Some(chrono::Utc::now());
        live.job = job;
        job.and_then(|j| inner.jobs.get(&j))
            .is_some_and(|j| j.cancel_requested)
    }

    /// The running job `id`, if `name` is the executor running it.
    fn running_on(&self, id: u32, name: &str) -> Result<(), ApiError> {
        match self.job(id) {
            Some(job) if job.executor.as_deref() == Some(name) && !job.state().is_done() => Ok(()),
            Some(_) => Err(ApiError::new(
                StatusCode::CONFLICT,
                format!("Job {id} isn't running on {name}."),
            )),
            None => Err(ApiError::not_found(format!("There is no job {id}."))),
        }
    }

    fn append_log(&self, id: u32, lines: &[String]) -> Result<()> {
        use std::io::Write;
        let mut inner = self.inner.lock().unwrap();
        let Some(job) = inner.jobs.get_mut(&id) else {
            return Ok(());
        };
        let room = MAX_LOG_LINES.saturating_sub(job.log_lines as usize);
        if room == 0 {
            return Ok(());
        }
        let mut text = String::new();
        let mut written = 0;
        for line in lines.iter().take(room) {
            // One line per line: a stray newline would throw every reader's
            // count off by one from then on.
            text.push_str(&line.replace(['\n', '\r'], " "));
            text.push('\n');
            written += 1;
        }
        if written == room && lines.len() > room {
            text.push_str("(the log is too long; the rest wasn't kept)\n");
            written += 1;
        }
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.job_dir(id).join("log.txt"))?;
        file.write_all(text.as_bytes())?;
        job.log_lines += written as u32;
        Ok(())
    }

    /// Lines `from` onwards of a job's log.
    pub fn log(&self, id: u32, from: usize) -> LogPage {
        let text = std::fs::read_to_string(self.job_dir(id).join("log.txt")).unwrap_or_default();
        let lines: Vec<String> = text.lines().skip(from).map(str::to_string).collect();
        LogPage {
            next: (from + lines.len()) as u32,
            lines,
        }
    }

    pub fn output_file(&self, id: u32, path: &Path) -> PathBuf {
        self.job_dir(id).join("outputs").join(path)
    }

    fn finish(&self, id: u32, executor: &str, finish: Finish) -> Result<Job> {
        let mut inner = self.inner.lock().unwrap();
        let job = inner.jobs.get_mut(&id).context("no such job")?;
        job.set_state(match finish.state() {
            // Not something an executor gets to say about a job it finished.
            JobState::Unspecified | JobState::Queued | JobState::Running => JobState::Failed,
            state => state,
        });
        job.finished_at = Some(crate::cache::store::now());
        job.message = finish.message;
        job.ciabatta_path = finish.ciabatta_path;
        job.outputs = finish.outputs;
        let job = job.clone();
        self.save_job(&job)?;
        if let Some(live) = inner.live.get_mut(executor) {
            live.job = None;
        }
        Ok(job)
    }

    /// Ask a job to stop. A queued one stops at once; a running one when its
    /// executor next checks in.
    pub fn cancel(&self, id: u32, by: &str) -> Option<Job> {
        let mut inner = self.inner.lock().unwrap();
        let job = inner.jobs.get_mut(&id)?;
        match job.state() {
            JobState::Queued => {
                job.set_state(JobState::Cancelled);
                job.finished_at = Some(crate::cache::store::now());
                job.message = Some(format!("cancelled by {by} before it started"));
            }
            JobState::Running => job.cancel_requested = true,
            _ => {}
        }
        let job = job.clone();
        let _ = self.save_job(&job);
        drop(inner);
        self.work.notify_waiters();
        Some(job)
    }

    /// Give up on jobs whose executor has gone quiet.
    pub fn sweep(&self) {
        let now = chrono::Utc::now();
        let mut inner = self.inner.lock().unwrap();
        let silent: Vec<(String, u32)> = inner
            .live
            .iter()
            .filter_map(|(id, live)| {
                let job = live.job?;
                let quiet = live
                    .last_seen
                    .is_none_or(|at| (now - at).num_seconds() > ABANDONED_AFTER_SECONDS);
                quiet.then(|| (id.clone(), job))
            })
            .collect();
        for (executor, id) in silent {
            if let Some(job) = inner.jobs.get_mut(&id)
                && job.state() == JobState::Running
            {
                job.set_state(JobState::Failed);
                job.finished_at = Some(crate::cache::store::now());
                job.message = Some(format!(
                    "{} stopped answering while it was running",
                    job.executor.as_deref().unwrap_or("its executor")
                ));
                let _ = self.save_job(job);
            }
            if let Some(live) = inner.live.get_mut(&executor) {
                live.job = None;
            }
        }
    }
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, bytes).with_context(|| format!("Failed to write {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("Failed to write {}", path.display()))
}

// ─── Discovery ──────────────────────────────────────────────────────────────

/// Watch the network for executors, for as long as the server runs.
///
/// Best-effort: a host without multicast (a container, some VPNs) just never
/// discovers anything, and executors can still be added by address.
pub async fn discover(oven: Arc<Oven>) {
    let daemon = match mdns_sd::ServiceDaemon::new() {
        Ok(daemon) => daemon,
        Err(e) => {
            tracing::warn!("mDNS is unavailable, so executors won't be discovered: {e}");
            return;
        }
    };
    let events = match daemon.browse(crate::oven::SERVICE_TYPE) {
        Ok(events) => events,
        Err(e) => {
            tracing::warn!("couldn't browse for executors: {e}");
            return;
        }
    };
    while let Ok(event) = events.recv_async().await {
        match event {
            mdns_sd::ServiceEvent::ServiceResolved(service) => {
                let Some(id) = service.get_property_val_str("id").map(str::to_string) else {
                    continue;
                };
                // IPv4 first: it's what a person would type, and what most
                // home and office networks route.
                let mut ips: Vec<std::net::IpAddr> = service
                    .get_addresses()
                    .iter()
                    .filter(|ip| !ip.is_loopback())
                    .map(|ip| ip.to_ip_addr())
                    .collect();
                ips.sort_by_key(|ip| (!ip.is_ipv4(), ip.to_string()));
                let Some(ip) = ips.first() else { continue };
                let address = match ip {
                    std::net::IpAddr::V4(v4) => format!("http://{v4}:{}", service.get_port()),
                    std::net::IpAddr::V6(v6) => format!("http://[{v6}]:{}", service.get_port()),
                };
                let found = Discovered {
                    id,
                    name: service
                        .get_property_val_str("name")
                        .unwrap_or(&service.fullname)
                        .to_string(),
                    address,
                    version: service
                        .get_property_val_str("version")
                        .unwrap_or_default()
                        .to_string(),
                    availability: match service.get_property_val_str("paired") {
                        Some("1") => Availability::PairedElsewhere,
                        _ => Availability::Available,
                    }
                    .into(),
                    platform: service
                        .get_property_val_str("platform")
                        .unwrap_or_default()
                        .to_string(),
                };
                tracing::debug!("found executor {} at {}", found.name, found.address);
                oven.discovered
                    .lock()
                    .unwrap()
                    .insert(service.fullname.clone(), found);
            }
            mdns_sd::ServiceEvent::ServiceRemoved(_, fullname) => {
                oven.discovered.lock().unwrap().remove(&fullname);
            }
            _ => {}
        }
    }
}

/// Every half-minute: give up on jobs whose executor has gone.
pub async fn sweeper(oven: Arc<Oven>) {
    let mut every = tokio::time::interval(Duration::from_secs(30));
    loop {
        every.tick().await;
        oven.sweep();
    }
}

// ─── Routes ─────────────────────────────────────────────────────────────────

pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/api/oven/executors",
            get(list_executors).post(pair_executor),
        )
        .route(
            "/api/oven/executors/{id}",
            axum::routing::delete(remove_executor),
        )
        .route("/api/oven/executors/{id}/poll", post(poll))
        .route("/api/oven/executors/{id}/heartbeat", post(heartbeat))
        .route("/api/oven/jobs", get(list_jobs).post(submit_job))
        .route("/api/oven/jobs/{id}", get(get_job))
        .route("/api/oven/jobs/{id}/log", get(read_log).post(write_log))
        .route("/api/oven/jobs/{id}/cancel", post(cancel_job))
        .route("/api/oven/jobs/{id}/finish", post(finish_job))
        .route(
            "/api/oven/jobs/{id}/outputs/{*path}",
            get(get_output).put(put_output),
        )
}

/// The executor a request comes from: its id in the path, its token in the
/// `Authorization` header. Checked whatever the cache's auth mode — an open
/// cache trusts its users, not anything on the network claiming to be an
/// executor somebody paired.
fn executor_from(state: &AppState, headers: &HeaderMap, id: &str) -> ApiResult<String> {
    let token = super::server::bearer(headers).unwrap_or_default();
    state.oven.authenticate(id, &token).ok_or_else(|| {
        ApiError::new(
            StatusCode::UNAUTHORIZED,
            "This executor isn't paired with this cache (or was removed). Pair it again \
             from the cache's Executors page.",
        )
    })
}

/// The executor running job `id`, from its token.
fn executor_for_job(state: &AppState, headers: &HeaderMap, id: u32) -> ApiResult<String> {
    let executor = headers
        .get("x-ciabatta-executor")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let name = executor_from(state, headers, &executor)?;
    state.oven.running_on(id, &name)?;
    Ok(executor)
}

async fn list_executors(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<Json<ExecutorList>> {
    state.identify(&headers)?;
    Ok(Json(ExecutorList {
        executors: state.oven.executors(),
        discovered: state.oven.discovered(),
    }))
}

/// Pair with an executor: check it's there, hand it a token with its code,
/// and remember it.
async fn pair_executor(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<PairWith>,
) -> ApiResult<Json<Paired>> {
    let identity = state.identify_admin(&headers)?;
    let address = normalize_address(&payload.address).map_err(ApiError::bad_request)?;
    let server = match payload.server.filter(|s| !s.trim().is_empty()) {
        Some(server) => server.trim_end_matches('/').to_string(),
        None => own_url(&state, &headers),
    };

    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    let info: ExecutorInfo = http
        .get(format!("{address}/oven/info"))
        .send()
        .await
        .and_then(|r| r.error_for_status())
        .map_err(|e| {
            ApiError::new(
                StatusCode::BAD_GATEWAY,
                format!("Couldn't reach an executor at {address}: {e}"),
            )
        })?
        .json()
        .await
        .map_err(|_| {
            ApiError::new(
                StatusCode::BAD_GATEWAY,
                format!("{address} answered, but not as a ciabatta executor."),
            )
        })?;

    if state.oven.name_taken(&info.id, &info.name) {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            format!(
                "Another executor is already called '{}'. Start this one with a different \
                 --name, or remove the other first.",
                info.name
            ),
        ));
    }

    let token = super::auth::generate_token();
    let response = http
        .post(format!("{address}/oven/pair"))
        .json(&PairRequest {
            code: payload.code.trim().to_string(),
            server: server.clone(),
            token: token.clone(),
        })
        .send()
        .await
        .map_err(|e| ApiError::new(StatusCode::BAD_GATEWAY, format!("Pairing failed: {e}")))?;
    if response.status() == reqwest::StatusCode::FORBIDDEN {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            format!(
                "{} refused the pairing code. Check the code it printed and try again.",
                info.name
            ),
        ));
    }
    if !response.status().is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(ApiError::new(
            StatusCode::BAD_GATEWAY,
            format!("{} didn't pair: {body}", info.name),
        ));
    }

    state
        .oven
        .add(&info, &address, &token, &identity.name)
        .map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))?;
    tracing::info!(
        "{} paired the executor {} ({address})",
        identity.name,
        info.name
    );
    Ok(Json(Paired {
        executor: Some(info),
    }))
}

/// `host:port` or a URL, as `http://host:port` with nothing after it.
fn normalize_address(raw: &str) -> Result<String, String> {
    let raw = raw.trim().trim_end_matches('/');
    if raw.is_empty() {
        return Err("An executor's address is needed, like 192.168.1.20:8381.".to_string());
    }
    let with_scheme = if raw.starts_with("http://") || raw.starts_with("https://") {
        raw.to_string()
    } else {
        format!("http://{raw}")
    };
    let has_port = with_scheme
        .rsplit_once(':')
        .is_some_and(|(_, port)| port.parse::<u16>().is_ok());
    Ok(match has_port {
        true => with_scheme,
        false => format!("{with_scheme}:{}", crate::oven::DEFAULT_PORT),
    })
}

/// How this server was reached, from the request: what an executor on the
/// same network should be able to use too.
fn own_url(state: &AppState, headers: &HeaderMap) -> String {
    let scheme = match state.config.server.tls.is_some() {
        true => "https",
        false => "http",
    };
    let host = headers
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .map(str::to_string)
        .unwrap_or_else(|| format!("localhost:{}", state.config.server.port));
    format!("{scheme}://{host}")
}

async fn remove_executor(
    State(state): State<AppState>,
    headers: HeaderMap,
    UrlPath(id): UrlPath<String>,
) -> ApiResult<Json<serde_json::Value>> {
    let identity = state.identify_admin(&headers)?;
    let removed = state
        .oven
        .remove(&id)
        .map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))?;
    if !removed {
        return Err(ApiError::not_found("No executor has that id."));
    }
    tracing::info!("{} removed the executor {id}", identity.name);
    Ok(Json(json!({ "ok": true })))
}

/// An executor waiting for work: answered with a job as soon as there is one
/// for it, or with nothing after [`POLL_SECONDS`].
async fn poll(
    State(state): State<AppState>,
    headers: HeaderMap,
    UrlPath(id): UrlPath<String>,
) -> ApiResult<Json<Poll>> {
    let name = executor_from(&state, &headers, &id)?;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(POLL_SECONDS);
    loop {
        // Registered before looking, so a job submitted between the look and
        // the wait still wakes it.
        let woken = state.oven.work.notified();
        tokio::pin!(woken);
        woken.as_mut().enable();
        if let Some(job) = state.oven.take(&id, &name) {
            tracing::info!("job {} → {name}", job.id);
            return Ok(Json(Poll { job: Some(job) }));
        }
        state.oven.heartbeat(&id, None);
        if tokio::time::timeout_at(deadline, woken).await.is_err() {
            return Ok(Json(Poll { job: None }));
        }
    }
}

async fn heartbeat(
    State(state): State<AppState>,
    headers: HeaderMap,
    UrlPath(id): UrlPath<String>,
    Json(payload): Json<Heartbeat>,
) -> ApiResult<Json<HeartbeatReply>> {
    executor_from(&state, &headers, &id)?;
    let cancel = state.oven.heartbeat(&id, payload.job);
    Ok(Json(HeartbeatReply { cancel }))
}

#[derive(Deserialize)]
struct JobsQuery {
    #[serde(default)]
    project: Option<String>,
    #[serde(default)]
    limit: Option<usize>,
}

async fn list_jobs(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<JobsQuery>,
) -> ApiResult<Json<JobList>> {
    state.identify(&headers)?;
    let jobs = state.oven.jobs(
        query.project.as_deref(),
        query.limit.unwrap_or(50).min(MAX_JOBS),
    );
    Ok(Json(JobList { jobs }))
}

/// Submit a job. Anyone who may use the cache may offload to it; what an
/// executor will clone is the executor's own decision (`--allow-repo`).
async fn submit_job(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(spec): Json<JobSpec>,
) -> ApiResult<Json<Job>> {
    let identity = state.identify(&headers)?;
    super::server::require_project(&state, &spec.project)?;
    let job = state
        .oven
        .submit(spec, &identity.name)
        .map_err(ApiError::bad_request)?;
    tracing::info!(
        "{} offloaded job {} ({} at {})",
        identity.name,
        job.id,
        job.spec().workflows.join(" "),
        job.short_commit()
    );
    Ok(Json(job))
}

async fn get_job(
    State(state): State<AppState>,
    headers: HeaderMap,
    UrlPath(id): UrlPath<u32>,
) -> ApiResult<Json<Job>> {
    state.identify(&headers)?;
    state
        .oven
        .job(id)
        .map(Json)
        .ok_or_else(|| ApiError::not_found(format!("There is no job {id}.")))
}

#[derive(Deserialize)]
struct LogQuery {
    #[serde(default)]
    from: usize,
}

async fn read_log(
    State(state): State<AppState>,
    headers: HeaderMap,
    UrlPath(id): UrlPath<u32>,
    Query(query): Query<LogQuery>,
) -> ApiResult<Json<LogPage>> {
    state.identify(&headers)?;
    if state.oven.job(id).is_none() {
        return Err(ApiError::not_found(format!("There is no job {id}.")));
    }
    Ok(Json(state.oven.log(id, query.from)))
}

async fn write_log(
    State(state): State<AppState>,
    headers: HeaderMap,
    UrlPath(id): UrlPath<u32>,
    Json(batch): Json<LogBatch>,
) -> ApiResult<Json<serde_json::Value>> {
    executor_for_job(&state, &headers, id)?;
    state
        .oven
        .append_log(id, &batch.lines)
        .map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))?;
    Ok(Json(json!({ "ok": true })))
}

async fn cancel_job(
    State(state): State<AppState>,
    headers: HeaderMap,
    UrlPath(id): UrlPath<u32>,
) -> ApiResult<Json<Job>> {
    let identity = state.identify(&headers)?;
    let job = state
        .oven
        .cancel(id, &identity.name)
        .ok_or_else(|| ApiError::not_found(format!("There is no job {id}.")))?;
    tracing::info!("{} cancelled job {id}", identity.name);
    Ok(Json(job))
}

async fn finish_job(
    State(state): State<AppState>,
    headers: HeaderMap,
    UrlPath(id): UrlPath<u32>,
    Json(finish): Json<Finish>,
) -> ApiResult<Json<Job>> {
    let executor = executor_for_job(&state, &headers, id)?;
    // Only what was actually stored counts as sent back.
    let mut finish = finish;
    finish.outputs.retain(|o| {
        crate::oven::safe_output_path(&o.path)
            .is_ok_and(|p| state.oven.output_file(id, &p).is_file())
    });
    let job = state
        .oven
        .finish(id, &executor, finish)
        .map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))?;
    tracing::info!("job {id} {}", job.state().word());
    Ok(Json(job))
}

async fn put_output(
    State(state): State<AppState>,
    headers: HeaderMap,
    UrlPath((id, path)): UrlPath<(u32, String)>,
    body: Bytes,
) -> ApiResult<Json<OutputFile>> {
    executor_for_job(&state, &headers, id)?;
    let relative = crate::oven::safe_output_path(&path).map_err(ApiError::bad_request)?;
    let target = state.oven.output_file(id, &relative);
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    }
    std::fs::write(&target, &body)
        .map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    Ok(Json(OutputFile {
        path,
        size: body.len() as u64,
    }))
}

async fn get_output(
    State(state): State<AppState>,
    headers: HeaderMap,
    UrlPath((id, path)): UrlPath<(u32, String)>,
) -> ApiResult<impl IntoResponse> {
    state.identify(&headers)?;
    let relative = crate::oven::safe_output_path(&path).map_err(ApiError::bad_request)?;
    let bytes = std::fs::read(state.oven.output_file(id, &relative))
        .map_err(|_| ApiError::not_found(format!("Job {id} sent back no '{path}'.")))?;
    Ok(([(header::CONTENT_TYPE, "application/octet-stream")], bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn oven() -> (PathBuf, Oven) {
        static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("ciabatta-oven-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let oven = Oven::open(&dir).unwrap();
        (dir, oven)
    }

    fn info(id: &str, name: &str) -> ExecutorInfo {
        ExecutorInfo {
            id: id.into(),
            name: name.into(),
            version: "0.10.0".into(),
            platform: "linux-x86_64".into(),
            paired_with: None,
        }
    }

    fn spec(executor: Option<&str>) -> JobSpec {
        JobSpec {
            project: "p".into(),
            repo: "https://example.com/r.git".into(),
            commit: "0123456789abcdef0123456789abcdef01234567".into(),
            workflows: vec!["build".into()],
            executor: executor.map(str::to_string),
            ..Default::default()
        }
    }

    #[test]
    fn nothing_is_queued_with_no_executor_to_run_it() {
        let (_dir, oven) = oven();
        assert!(
            oven.submit(spec(None), "me")
                .unwrap_err()
                .contains("No executors")
        );
        oven.add(&info("a", "box"), "http://x:1", "t", "admin")
            .unwrap();
        assert!(
            oven.submit(spec(Some("nope")), "me")
                .unwrap_err()
                .contains("box")
        );
        assert!(oven.submit(spec(Some("BOX")), "me").is_ok());
    }

    #[test]
    fn an_executor_runs_one_job_at_a_time() {
        let (_dir, oven) = oven();
        oven.add(&info("a", "box"), "http://x:1", "t", "admin")
            .unwrap();
        let first = oven.submit(spec(None), "me").unwrap();
        let second = oven.submit(spec(None), "me").unwrap();

        let taken = oven.take("a", "box").unwrap();
        assert_eq!(taken.id, first.id);
        assert_eq!(taken.state(), JobState::Running);
        // Asking again while it runs hands back the same job, not the next.
        assert_eq!(oven.take("a", "box").unwrap().id, first.id);

        oven.finish(
            first.id,
            "a",
            Finish {
                state: JobState::Succeeded.into(),
                message: None,
                ciabatta_path: Some("/main/abc".into()),
                outputs: vec![],
            },
        )
        .unwrap();
        assert_eq!(oven.take("a", "box").unwrap().id, second.id);
    }

    #[test]
    fn a_job_for_one_executor_waits_for_it() {
        let (_dir, oven) = oven();
        oven.add(&info("a", "box"), "http://x:1", "t", "admin")
            .unwrap();
        oven.add(&info("b", "mac"), "http://y:1", "u", "admin")
            .unwrap();
        oven.submit(spec(Some("mac")), "me").unwrap();
        assert!(oven.take("a", "box").is_none());
        assert_eq!(
            oven.take("b", "mac").unwrap().executor.as_deref(),
            Some("mac")
        );
    }

    #[test]
    fn cancelling_stops_a_queued_job_now_and_a_running_one_at_its_heartbeat() {
        let (_dir, oven) = oven();
        oven.add(&info("a", "box"), "http://x:1", "t", "admin")
            .unwrap();
        let running = oven.submit(spec(None), "me").unwrap();
        let queued = oven.submit(spec(None), "me").unwrap();
        oven.take("a", "box").unwrap();

        assert_eq!(
            oven.cancel(queued.id, "me").unwrap().state(),
            JobState::Cancelled
        );
        assert!(!oven.heartbeat("a", Some(running.id)));
        oven.cancel(running.id, "me");
        assert!(oven.heartbeat("a", Some(running.id)));
    }

    #[test]
    fn only_the_token_it_was_given_speaks_for_an_executor() {
        let (_dir, oven) = oven();
        oven.add(&info("a", "box"), "http://x:1", "secret", "admin")
            .unwrap();
        assert_eq!(oven.authenticate("a", "secret").as_deref(), Some("box"));
        assert!(oven.authenticate("a", "guess").is_none());
        assert!(oven.authenticate("b", "secret").is_none());
        oven.remove("a").unwrap();
        assert!(oven.authenticate("a", "secret").is_none());
    }

    #[test]
    fn jobs_and_executors_survive_a_restart_but_running_jobs_do_not() {
        let (dir, oven) = oven();
        oven.add(&info("a", "box"), "http://x:1", "t", "admin")
            .unwrap();
        let job = oven.submit(spec(None), "me").unwrap();
        oven.take("a", "box").unwrap();
        oven.append_log(job.id, &["one".into(), "two\nthree".into()])
            .unwrap();
        drop(oven);

        let oven = Oven::open(&dir).unwrap();
        assert_eq!(oven.executors().len(), 1);
        let job = oven.job(job.id).unwrap();
        assert_eq!(job.state(), JobState::Failed);
        assert_eq!(oven.log(job.id, 1).lines, vec!["two three".to_string()]);
        // Ids carry on from where they were.
        assert_eq!(oven.submit(spec(None), "me").unwrap().id, job.id + 1);
    }

    #[test]
    fn a_job_that_wont_load_keeps_its_number() {
        let (dir, oven) = oven();
        oven.add(&info("a", "box"), "http://x:1", "t", "admin")
            .unwrap();
        let first = oven.submit(spec(None), "me").unwrap();
        oven.append_log(first.id, &["old".into()]).unwrap();
        drop(oven);
        // Unreadable — written by some other version, say.
        let saved = dir
            .join("oven/jobs")
            .join(first.id.to_string())
            .join("job.json");
        std::fs::write(&saved, "{ not a job").unwrap();

        let oven = Oven::open(&dir).unwrap();
        assert!(oven.job(first.id).is_none());
        let next = oven.submit(spec(None), "me").unwrap();
        assert_eq!(next.id, first.id + 1);
        assert!(oven.log(next.id, 0).lines.is_empty());
    }

    #[test]
    fn addresses_get_a_scheme_and_a_port() {
        assert_eq!(
            normalize_address("10.0.0.5").unwrap(),
            "http://10.0.0.5:8381"
        );
        assert_eq!(
            normalize_address("10.0.0.5:9000/").unwrap(),
            "http://10.0.0.5:9000"
        );
        assert_eq!(
            normalize_address("http://box.local:8381").unwrap(),
            "http://box.local:8381"
        );
        assert!(normalize_address(" ").is_err());
    }
}
