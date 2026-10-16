//! Pidfile-based lifecycle management for the `ciabatta daemon` background
//! process: start it detached, stop it, and check whether it's running.
//!
//! There's no true "daemon" IPC here — this is the same shape as
//! `main::spawn_detached_todo`: re-exec this binary with stdio discarded so it
//! keeps running after the parent exits, and track it with a pidfile at
//! `~/.ciabatta/daemon/daemon.pid` (the same directory the registry lives in).

use std::path::PathBuf;
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};

use super::daemon_home_dir;

fn pid_path() -> Result<PathBuf> {
    Ok(daemon_home_dir()?.join("daemon.pid"))
}

/// `~/.ciabatta/daemon/daemon.log` — where the poll loop logs detected drift
/// and `on_change` command results when running detached (no console to
/// print to).
pub fn log_path() -> Result<PathBuf> {
    Ok(daemon_home_dir()?.join("daemon.log"))
}

/// The pid recorded in the pidfile, if any (regardless of whether that
/// process is still alive — callers should check with [`is_alive`]).
pub fn read_pid() -> Result<Option<u32>> {
    let path = pid_path()?;
    match std::fs::read_to_string(&path) {
        Ok(s) => Ok(s.trim().parse::<u32>().ok()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("Failed to read {}", path.display())),
    }
}

fn write_pid(pid: u32) -> Result<()> {
    let path = pid_path()?;
    std::fs::write(&path, pid.to_string()).with_context(|| format!("Failed to write {}", path.display()))
}

/// Remove the pidfile, ignoring a missing file.
pub fn clear_pid() -> Result<()> {
    let path = pid_path()?;
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e).with_context(|| format!("Failed to remove {}", path.display())),
    }
}

/// Whether `pid` refers to a live process, checked without any extra
/// dependency: `kill -0` on unix, `tasklist` on Windows.
#[cfg(unix)]
pub fn is_alive(pid: u32) -> bool {
    Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[cfg(windows)]
pub fn is_alive(pid: u32) -> bool {
    let Ok(output) = Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/NH"])
        .output()
    else {
        return false;
    };
    String::from_utf8_lossy(&output.stdout).contains(&pid.to_string())
}

/// The currently-running daemon's pid, if the pidfile points at a live
/// process. Stale pidfiles (process no longer alive) are treated as "not
/// running" — callers that then start a fresh daemon will overwrite it.
pub fn running_pid() -> Result<Option<u32>> {
    Ok(read_pid()?.filter(|&pid| is_alive(pid)))
}

/// Record this process (the one actually running the poll loop + web view)
/// as the daemon. Called once at the top of the foreground run, whether or
/// not it was reached via detaching.
pub fn record_self() -> Result<()> {
    write_pid(std::process::id())
}

/// Re-exec this binary as a detached background process running
/// `ciabatta daemon --port <port> --interval <interval>` (no `-d`, so the
/// child takes the foreground path in its own process), with stdio
/// discarded so it survives this process exiting. Fails if a daemon is
/// already running.
pub fn spawn_detached(port: u16, interval: u64) -> Result<u32> {
    if let Some(pid) = running_pid()? {
        bail!("daemon already running (pid {pid}) — stop it first with `ciabatta daemon stop`");
    }

    let exe = std::env::current_exe().context("Failed to locate the ciabatta executable")?;
    let child = Command::new(exe)
        .arg("daemon")
        .arg("--port")
        .arg(port.to_string())
        .arg("--interval")
        .arg(interval.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("Failed to start the background daemon")?;

    let pid = child.id();
    write_pid(pid)?;
    Ok(pid)
}

/// Stop a detached daemon by pid, cross-platform.
pub fn stop() -> Result<()> {
    let Some(pid) = running_pid()? else {
        clear_pid()?;
        bail!("daemon is not running");
    };

    #[cfg(unix)]
    let status = Command::new("kill").arg(pid.to_string()).status();
    #[cfg(windows)]
    let status = Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/F"])
        .status();

    let status = status.context("Failed to send stop signal to the daemon process")?;
    if !status.success() {
        bail!("Failed to stop daemon (pid {pid})");
    }
    clear_pid()
}
