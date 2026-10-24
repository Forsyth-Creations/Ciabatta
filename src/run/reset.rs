//! `ciabatta self reset`: stop everything ciabatta has running on this machine
//! and free every run slot.
//!
//! The way out when a run slot is taken by something that shouldn't have it —
//! a run that is wedged, or one whose terminal is long gone — and the next run
//! sits "waiting for a run slot" forever. It stops the daemon (and with it the
//! runs and watch sessions it is keeping), kills each run still holding a slot
//! along with everything it started, and then clears the slot files.

use anyhow::Result;

/// What a reset did, for the command to report.
#[derive(Debug, Default)]
pub struct Report {
    /// The daemon's pid, if one was running and was stopped.
    pub daemon: Option<u32>,
    /// The runs that held slots and were killed.
    pub runs: Vec<super::slots::Holder>,
}

pub async fn reset() -> Result<Report> {
    let mut report = Report::default();
    let me = std::process::id();

    // The daemon first: its runs hold slots too, and it would otherwise be
    // killed below as one of their holders without the chance to save them.
    if let Some(record) = crate::daemon::read_record() {
        if crate::daemon::find_running().await.is_some() {
            // Its tree is taken before asking it to stop: once it has exited,
            // the watch sessions it started are orphans nothing can find.
            let tree = tree(record.pid);
            let _ = crate::daemon::shutdown(&record).await;
            for _ in 0..30 {
                if crate::daemon::find_running().await.is_none() {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
            for pid in tree {
                kill(pid);
            }
            report.daemon = Some(record.pid);
        }
        crate::daemon::clear_record()?;
    }

    for holder in super::slots::held_everywhere()? {
        if holder.pid == me || Some(holder.pid) == report.daemon {
            continue;
        }
        for pid in tree(holder.pid) {
            kill(pid);
        }
        report.runs.push(holder);
    }

    super::slots::clear_all()?;
    Ok(report)
}

/// `pid` and every process descended from it, parents first.
#[cfg(unix)]
fn tree(pid: u32) -> Vec<u32> {
    let listing = std::process::Command::new("ps")
        .args(["-A", "-o", "pid=", "-o", "ppid="])
        .output()
        .map(|out| String::from_utf8_lossy(&out.stdout).into_owned())
        .unwrap_or_default();
    let parents: Vec<(u32, u32)> = listing
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            Some((fields.next()?.parse().ok()?, fields.next()?.parse().ok()?))
        })
        .collect();
    let mut found = vec![pid];
    let mut index = 0;
    while index < found.len() {
        let parent = found[index];
        found.extend(
            parents
                .iter()
                .filter(|(child, ppid)| *ppid == parent && !found.contains(child))
                .map(|(child, _)| *child)
                .collect::<Vec<_>>(),
        );
        index += 1;
    }
    found
}

/// On Windows `taskkill /T` walks the tree itself, so the tree is the pid.
#[cfg(not(unix))]
fn tree(pid: u32) -> Vec<u32> {
    vec![pid]
}

fn kill(pid: u32) {
    #[cfg(unix)]
    // SAFETY: `kill` with a pid read from `ps` or a slot's pid file. Failing —
    // the process is already gone — is the outcome wanted anyway.
    unsafe {
        libc::kill(pid as libc::pid_t, libc::SIGKILL);
    }
    #[cfg(windows)]
    {
        let _ = std::process::Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }
    #[cfg(not(any(unix, windows)))]
    let _ = pid;
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn the_tree_holds_the_process_and_its_children() {
        let mut child = std::process::Command::new("sh")
            .args(["-c", "sleep 30 & wait"])
            .spawn()
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(300));
        let found = tree(child.id());
        assert_eq!(found[0], child.id());
        assert!(found.len() >= 2, "the sleep under the shell: {found:?}");
        for pid in found {
            kill(pid);
        }
        let _ = child.wait();
    }
}
