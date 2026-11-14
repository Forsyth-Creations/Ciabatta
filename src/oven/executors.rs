//! `ciabatta executor …`: the executors the project's remote cache manages.
//!
//! ```text
//! ciabatta executor start            run this machine as one (see [`super::executor`])
//! ciabatta executor list             the ones paired with the cache, online now
//! ciabatta executor status <NAME>    what one is doing, and when it was online
//! ciabatta executor stop <NAME>      cancel the job it's running
//! ciabatta executor remove <NAME>    unpair it from the cache
//! ```
//!
//! Everything but `start` asks the cache, so it works from any machine that
//! uses it, not only the executor's own.

use std::path::Path;

use anyhow::{Result, bail};
use owo_colors::OwoColorize;

use super::offload::Remote;
use super::{ExecutorHistory, ExecutorState, ExecutorView, Job, JobState};

/// Every executor paired with the cache.
async fn paired(remote: &Remote) -> Result<Vec<ExecutorView>> {
    let body: serde_json::Value = remote.client.get_json("/api/oven/executors").await?;
    Ok(serde_json::from_value(body["executors"].clone()).unwrap_or_default())
}

/// The executor called `name` — or, failing that, the one whose id is or
/// starts with it.
async fn find(remote: &Remote, name: &str) -> Result<ExecutorView> {
    let all = paired(remote).await?;
    if let Some(e) = all.iter().find(|e| e.name == name) {
        return Ok(e.clone());
    }
    let by_id: Vec<_> = all.iter().filter(|e| e.id.starts_with(name)).collect();
    match by_id.as_slice() {
        [one] => Ok((*one).clone()),
        [] if all.is_empty() => bail!(
            "No executors are paired with {}. Start one with `ciabatta executor start` and add it \
             on the cache's Executors page.",
            remote.url
        ),
        [] => bail!(
            "No executor called '{name}' is paired with {}. Paired: {}.",
            remote.url,
            all.iter()
                .map(|e| e.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ),
        _ => bail!("'{name}' starts more than one executor's id; use its name."),
    }
}

fn state_word(e: &ExecutorView) -> String {
    match e.state() {
        ExecutorState::Idle => "idle".to_string(),
        ExecutorState::Busy => format!("busy (job {})", e.job.unwrap_or_default()),
        ExecutorState::Offline | ExecutorState::Unspecified => "offline".to_string(),
    }
}

/// How long ago an RFC 3339 time was, as a person would say it.
fn ago(at: &str) -> Option<String> {
    let at = chrono::DateTime::parse_from_rfc3339(at).ok()?;
    let ms = (chrono::Utc::now() - at.with_timezone(&chrono::Utc))
        .num_milliseconds()
        .max(0);
    Some(crate::cache::cli::humanize_ms(ms as u64))
}

/// The cache's record of when the executor came and went. An older cache
/// doesn't keep one; that's not worth failing over.
async fn history(remote: &Remote, id: &str) -> Option<ExecutorHistory> {
    remote
        .client
        .get_json(&format!("/api/oven/executors/{id}/history"))
        .await
        .ok()
}

/// `ciabatta executor list`: what this cache has to run jobs on — only
/// what's online unless `all`.
pub async fn list(cwd: &Path, all: bool) -> Result<()> {
    let remote = Remote::discover(cwd)?;
    let paired = paired(&remote).await?;
    if paired.is_empty() {
        println!(
            "No executors are paired with {}. Start one with `ciabatta executor start` and add it \
             on the cache's Executors page.",
            remote.url
        );
        return Ok(());
    }
    let total = paired.len();
    let shown: Vec<_> = paired
        .into_iter()
        .filter(|e| all || e.state() != ExecutorState::Offline)
        .collect();
    if shown.is_empty() {
        println!(
            "None of the {total} executor{} paired with {} {} online. `--all` lists them anyway.",
            if total == 1 { "" } else { "s" },
            remote.url,
            if total == 1 { "is" } else { "are" },
        );
        return Ok(());
    }
    for e in &shown {
        // How long it has been in its current state.
        let since = history(&remote, &e.id)
            .await
            .and_then(|h| h.changes.last().and_then(|c| ago(&c.at)))
            .map(|d| format!("for {d}"))
            .unwrap_or_default();
        println!(
            "{:<20}  {:<16}  {:<14}  {}  {}",
            e.name,
            state_word(e),
            since,
            e.platform,
            e.version
        );
    }
    if !all && shown.len() < total {
        let offline = total - shown.len();
        println!("({offline} offline; `--all` lists them too)");
    }
    Ok(())
}

/// `ciabatta executor status <NAME>`.
pub async fn status(cwd: &Path, name: &str) -> Result<()> {
    let remote = Remote::discover(cwd)?;
    let e = find(&remote, name).await?;
    let state = match e.state() {
        ExecutorState::Idle => state_word(&e).green().to_string(),
        ExecutorState::Busy => state_word(&e).cyan().to_string(),
        _ => state_word(&e).dimmed().to_string(),
    };
    println!("{} — {state}", e.name.bold());
    if let Some(id) = e.job {
        // What the job is, if the cache will say.
        if let Ok(job) = remote
            .client
            .get_json::<Job>(&format!("/api/oven/jobs/{id}"))
            .await
        {
            let spec = job.spec();
            println!(
                "  job           {id}: {} at {} for {}",
                spec.workflows.join(" "),
                &spec.commit[..10.min(spec.commit.len())],
                job.requested_by
            );
        }
    }
    println!("  id            {}", e.id);
    println!("  address       {}", e.address);
    println!("  platform      {} · ciabatta {}", e.platform, e.version);
    if let Some(seen) = e.last_seen.as_deref().and_then(ago) {
        println!("  last seen     {seen} ago");
    }
    match ago(&e.added_at) {
        Some(added) => println!("  added         {added} ago by {}", e.added_by),
        None => println!("  added by      {}", e.added_by),
    }
    if let Some(h) = history(&remote, &e.id).await
        && !h.changes.is_empty()
    {
        println!();
        println!("  {}", "recently".dimmed());
        for change in h.changes.iter().rev().take(5) {
            println!(
                "  {:<8} {} ago",
                if change.online { "online" } else { "offline" },
                ago(&change.at).unwrap_or_else(|| change.at.clone())
            );
        }
    }
    Ok(())
}

/// `ciabatta executor stop <NAME>`: cancel the job it's running.
pub async fn stop(cwd: &Path, name: &str) -> Result<()> {
    let remote = Remote::discover(cwd)?;
    let e = find(&remote, name).await?;
    let Some(id) = e.job.filter(|_| e.state() == ExecutorState::Busy) else {
        println!("{} isn't running a job ({}).", e.name, state_word(&e));
        return Ok(());
    };
    let job: Job = remote
        .client
        .post_json(
            &format!("/api/oven/jobs/{id}/cancel"),
            &serde_json::json!({}),
        )
        .await?;
    match job.state() {
        JobState::Cancelled => println!("Cancelled job {id} on {}.", e.name),
        JobState::Running => println!(
            "Job {id} on {} will stop at its next check-in (a few seconds).",
            e.name
        ),
        state => println!("Job {id} on {} had already {}.", e.name, state.word()),
    }
    Ok(())
}

/// `ciabatta executor remove <NAME>`: unpair it. It waits to be paired again.
pub async fn remove(cwd: &Path, name: &str) -> Result<()> {
    let remote = Remote::discover(cwd)?;
    let e = find(&remote, name).await?;
    let _: serde_json::Value = remote
        .client
        .delete_json(&format!(
            "/api/oven/executors/{}",
            super::executor::url_encode(&e.id)
        ))
        .await?;
    println!(
        "Removed {} from {}. It waits to be paired again.",
        e.name, remote.url
    );
    Ok(())
}
