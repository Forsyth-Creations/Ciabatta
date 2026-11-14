//! The client's side of offloading: `--offload`, and `ciabatta offload …`.
//!
//! `ciabatta build --offload` runs `build` on an executor instead of here. It
//! won't offload what an executor couldn't reproduce — uncommitted changes to
//! tracked files, or a commit no remote has — because the executor clones the
//! repository at the commit and would build something other than what is on
//! this screen. Then it submits the job, prints its id, and streams the log.
//!
//! Ctrl-C stops *watching*, not the job: offloading is for work you'd rather
//! not wait on, and `ciabatta offload logs <id>` picks it back up.
//! `ciabatta offload cancel <id>` is what stops it.
//!
//! When the job ends, the files its steps declare as cache `outputs` are
//! downloaded into the offload directory — `offloaded/` unless
//! `workspace.offload_dir` says otherwise — under the job's execution number
//! (`CIABATTA_EXECUTION_NUMBER` in the run): `offloaded/42/target/release/app`.
//! One folder per execution, so variants of the same commit don't collide.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};

use super::{Job, JobSpec, JobState, LogPage};
use crate::remote_cache::client::Client;

/// The project's remote cache, and its id there — what every offload command
/// needs first.
pub struct Remote {
    pub client: Client,
    pub project: String,
    pub url: String,
    /// The workspace root.
    pub root: PathBuf,
    /// Where outputs land, absolute.
    pub offload_dir: PathBuf,
}

impl Remote {
    /// The remote cache the workspace at (or above) `cwd` uses.
    pub fn discover(cwd: &Path) -> Result<Self> {
        let ws = crate::workspace::Workspace::discover(cwd)?;
        let root = ws.root.clone();
        let config = crate::config::load_config(&root)?;
        let remote = config
            .cache
            .as_ref()
            .and_then(|c| c.remote())
            .filter(|r| r.enabled && !r.url.is_empty())
            .cloned()
            .context(
                "Offloading goes through a remote cache, and this workspace has none. Set \
                 `cache.remote.url` in the root ciabatta.yaml (and run `ciabatta cache init`).",
            )?;
        let project = remote.project.clone().context(
            "This workspace hasn't registered with its remote cache yet. Run `ciabatta cache init` \
             (or any cached run) once to register it.",
        )?;
        let dir = config
            .workspace
            .as_ref()
            .and_then(|w| w.offload_dir.clone())
            .unwrap_or_else(|| super::DEFAULT_OFFLOAD_DIR.to_string());
        Ok(Remote {
            client: Client::new(&remote.url, remote.tls_verify)?,
            project,
            url: remote.url,
            offload_dir: root.join(dir),
            root,
        })
    }

    async fn job(&self, id: u32) -> Result<Job> {
        self.client.get_json(&format!("/api/oven/jobs/{id}")).await
    }

    async fn log(&self, id: u32, from: usize) -> Result<LogPage> {
        self.client
            .get_json(&format!("/api/oven/jobs/{id}/log?from={from}"))
            .await
    }
}

/// What `--offload` was given, beside the workflow arguments themselves.
pub struct Request<'a> {
    pub workflows: &'a [String],
    pub args: &'a crate::cli::WorkflowArgs,
    pub custom_args: &'a [String],
    pub cwd: &'a Path,
    pub executor: Option<String>,
}

fn git(dir: &Path, args: &[&str]) -> Result<String> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .stdin(std::process::Stdio::null())
        .output()
        .context("Failed to run git")?;
    if !output.status.success() {
        bail!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    // Trailing whitespace only: `status --porcelain` starts a line with a
    // space for "unchanged in the index", and the path is found by position.
    Ok(String::from_utf8_lossy(&output.stdout)
        .trim_end()
        .to_string())
}

/// `rel` as a forward-slashed path, `""` for the same directory.
fn relative(from: &Path, to: &Path) -> String {
    let from = from.canonicalize().unwrap_or_else(|_| from.to_path_buf());
    let to = to.canonicalize().unwrap_or_else(|_| to.to_path_buf());
    to.strip_prefix(&from)
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .unwrap_or_default()
}

/// What the executor will check out, checked: committed, and pushed.
struct Source {
    repo: String,
    commit: String,
    branch: Option<String>,
    /// The workspace root, from the top of the repository.
    root: String,
}

fn source(root: &Path) -> Result<Source> {
    let top = PathBuf::from(
        git(root, &["rev-parse", "--show-toplevel"])
            .context("Offloading needs a git repository: the executor clones it at a commit.")?,
    );

    // Tracked files only. An untracked file isn't part of any commit, and
    // `offloaded/` itself is one.
    let dirty = git(root, &["status", "--porcelain", "--untracked-files=no"])?;
    if !dirty.is_empty() {
        let files: Vec<&str> = dirty
            .lines()
            .take(5)
            .map(|l| l.get(3..).unwrap_or(l))
            .collect();
        let more = dirty.lines().count().saturating_sub(files.len());
        bail!(
            "There are uncommitted changes, and the executor builds a commit — it wouldn't see \
             them: {}{}. Commit and push them, then offload again.",
            files.join(", "),
            if more > 0 {
                format!(" and {more} more")
            } else {
                String::new()
            }
        );
    }

    let commit = git(root, &["rev-parse", "HEAD"])?;
    let branch = git(root, &["rev-parse", "--abbrev-ref", "HEAD"])
        .ok()
        .filter(|b| !b.is_empty() && b != "HEAD");

    let on_remote = git(root, &["branch", "-r", "--contains", &commit]).unwrap_or_default();
    if on_remote.trim().is_empty() {
        bail!(
            "{} isn't on any remote branch, so the executor couldn't fetch it. Push it first{}.",
            &commit[..12.min(commit.len())],
            branch
                .as_deref()
                .map(|b| format!(" (git push -u origin {b})"))
                .unwrap_or_default()
        );
    }

    // The branch's own remote, when it tracks one; `origin` otherwise.
    let remote = git(
        root,
        &["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}"],
    )
    .ok()
    .and_then(|upstream| upstream.split_once('/').map(|(r, _)| r.to_string()))
    .unwrap_or_else(|| "origin".to_string());
    let repo = git(root, &["remote", "get-url", &remote])
        .with_context(|| format!("This repository has no '{remote}' remote to clone from"))?;

    Ok(Source {
        repo,
        commit,
        branch,
        root: relative(&top, root),
    })
}

/// `ciabatta <workflow> --offload`: submit, then follow until it ends.
pub async fn run(request: Request<'_>) -> Result<()> {
    let remote = Remote::discover(request.cwd)?;
    let source = source(&remote.root)?;
    let args = request.args;
    let spec = JobSpec {
        project: remote.project.clone(),
        repo: source.repo,
        commit: source.commit,
        branch: source.branch,
        workflows: request.workflows.to_vec(),
        root: source.root,
        cwd: relative(&remote.root, request.cwd),
        only: args.only.clone(),
        solo: args.solo,
        all: args.all,
        filter: args.filter.clone(),
        env: args.env.clone(),
        args: request.custom_args.to_vec(),
        force: args.force,
        executor: request.executor,
    };

    let job: Job = remote.client.post_json("/api/oven/jobs", &spec).await?;
    use owo_colors::OwoColorize;
    println!(
        "{} job {} — {} at {} on {}",
        "Offloaded as".bold(),
        job.id.to_string().bold(),
        spec.workflows.join(" "),
        &spec.commit[..12.min(spec.commit.len())],
        spec.executor
            .as_deref()
            .unwrap_or("the first free executor"),
    );
    println!(
        "{}",
        format!(
            "  Ctrl-C stops watching, not the job · `ciabatta offload cancel {}` stops it",
            job.id
        )
        .dimmed()
    );
    follow(&remote, job.id, 0).await
}

/// Stream a job's log until it ends (or Ctrl-C), then fetch its outputs.
pub async fn follow(remote: &Remote, id: u32, from: usize) -> Result<()> {
    let watching = watch(remote, id, from);
    tokio::select! {
        result = watching => result,
        _ = tokio::signal::ctrl_c() => {
            eprintln!(
                "\nStopped watching; job {id} carries on. `ciabatta offload logs {id}` to watch \
                 again, `ciabatta offload fetch {id}` for its outputs once it's done, \
                 `ciabatta offload cancel {id}` to stop it."
            );
            Ok(())
        }
    }
}

async fn watch(remote: &Remote, id: u32, mut from: usize) -> Result<()> {
    use owo_colors::OwoColorize;
    let mut said: Option<(JobState, Option<String>)> = None;
    let mut misses = 0u32;
    loop {
        let job = match remote.job(id).await {
            Ok(job) => {
                misses = 0;
                job
            }
            // A blip in the network is no reason to stop following.
            Err(e) if misses < 10 => {
                misses += 1;
                tracing::debug!("couldn't read job {id}: {e:#}");
                tokio::time::sleep(Duration::from_secs(2)).await;
                continue;
            }
            Err(e) => return Err(e),
        };
        let now = (job.state(), job.executor.clone());
        if said.as_ref() != Some(&now) {
            match job.state() {
                JobState::Queued => println!("{}", "Waiting for an executor…".dimmed()),
                JobState::Running => println!(
                    "{}",
                    format!(
                        "Running on {}.",
                        job.executor.as_deref().unwrap_or("an executor")
                    )
                    .dimmed()
                ),
                _ => {}
            }
            said = Some(now);
        }

        let page = remote.log(id, from).await.unwrap_or_default();
        for line in &page.lines {
            println!("{line}");
        }
        from = (page.next as usize).max(from);

        if job.state().is_done() {
            // Whatever arrived between the last read and the end.
            let rest = remote.log(id, from).await.unwrap_or_default();
            for line in &rest.lines {
                println!("{line}");
            }
            return finish(remote, &job).await;
        }
        tokio::time::sleep(Duration::from_millis(800)).await;
    }
}

/// Say how a job ended, bring its outputs back, and fail when it did.
async fn finish(remote: &Remote, job: &Job) -> Result<()> {
    use owo_colors::OwoColorize;
    let what = format!(
        "job {} {}{}",
        job.id,
        job.state().word(),
        job.message
            .as_deref()
            .map(|m| format!(": {m}"))
            .unwrap_or_default()
    );
    match job.state() {
        JobState::Succeeded => println!("{} {what}", "✓".green()),
        JobState::Cancelled => println!("{} {what}", "■".yellow()),
        _ => println!("{} {what}", "✗".red()),
    }
    if job.state() != JobState::Cancelled {
        fetch_outputs(remote, job).await?;
    }
    match job.state() {
        JobState::Succeeded => Ok(()),
        JobState::Cancelled => bail!("Job {} was cancelled.", job.id),
        _ => bail!("Job {} failed.", job.id),
    }
}

/// Download a job's outputs under the offload directory.
pub async fn fetch_outputs(remote: &Remote, job: &Job) -> Result<()> {
    if job.outputs.is_empty() {
        println!(
            "No outputs came back. Only files a step declares under `cache: outputs:` are \
             returned."
        );
        return Ok(());
    }
    let base = super::output_dir(&remote.offload_dir, job.id);
    for output in &job.outputs {
        let relative = super::safe_output_path(&output.path)?;
        let encoded: Vec<String> = output
            .path
            .split('/')
            .map(super::executor::url_encode)
            .collect();
        let bytes = remote
            .client
            .get_bytes(&format!(
                "/api/oven/jobs/{}/outputs/{}",
                job.id,
                encoded.join("/")
            ))
            .await
            .with_context(|| format!("Failed to download {}", output.path))?;
        let target = base.join(relative);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create {}", parent.display()))?;
        }
        std::fs::write(&target, bytes)
            .with_context(|| format!("Failed to write {}", target.display()))?;
    }
    let shown = base
        .strip_prefix(&remote.root)
        .unwrap_or(&base)
        .display()
        .to_string();
    println!(
        "{} output file{} → {shown}/",
        job.outputs.len(),
        if job.outputs.len() == 1 { "" } else { "s" }
    );
    Ok(())
}

// ─── `ciabatta offload …` ───────────────────────────────────────────────────

/// `ciabatta offload list`: recent jobs for this project.
pub async fn list(cwd: &Path, all: bool) -> Result<()> {
    let remote = Remote::discover(cwd)?;
    let path = match all {
        true => "/api/oven/jobs".to_string(),
        false => format!("/api/oven/jobs?project={}", remote.project),
    };
    let body: serde_json::Value = remote.client.get_json(&path).await?;
    let jobs: Vec<Job> = serde_json::from_value(body["jobs"].clone()).unwrap_or_default();
    if jobs.is_empty() {
        println!("No offloaded jobs yet. Run a workflow with --offload to start one.");
        return Ok(());
    }
    for job in jobs {
        println!(
            "{:>5}  {:<9}  {:<20}  {}  {}  {}",
            job.id,
            job.state().word(),
            job.spec().workflows.join(" "),
            &job.spec().commit[..10.min(job.spec().commit.len())],
            job.executor.as_deref().unwrap_or("-"),
            job.requested_by,
        );
    }
    Ok(())
}

/// `ciabatta offload logs <id>`: the log so far, and the rest as it comes.
pub async fn logs(cwd: &Path, id: u32) -> Result<()> {
    let remote = Remote::discover(cwd)?;
    follow(&remote, id, 0).await
}

/// `ciabatta offload fetch <id>`: a finished job's outputs.
pub async fn fetch(cwd: &Path, id: u32) -> Result<()> {
    let remote = Remote::discover(cwd)?;
    let job = remote.job(id).await?;
    if !job.state().is_done() {
        bail!(
            "Job {id} is still {}. `ciabatta offload logs {id}` follows it to the end.",
            job.state().word()
        );
    }
    fetch_outputs(&remote, &job).await
}

/// `ciabatta offload cancel <id>`.
pub async fn cancel(cwd: &Path, id: u32) -> Result<()> {
    let remote = Remote::discover(cwd)?;
    let job: Job = remote
        .client
        .post_json(
            &format!("/api/oven/jobs/{id}/cancel"),
            &serde_json::json!({}),
        )
        .await?;
    match job.state() {
        JobState::Cancelled => println!("Job {id} cancelled."),
        JobState::Running => {
            println!("Job {id} will stop at its executor's next check-in (a few seconds).")
        }
        state => println!("Job {id} had already {}.", state.word()),
    }
    Ok(())
}
