//! How many of a workspace's workflows may run at once on this machine.
//!
//! `workspace.max_concurrent_runs` in the root config caps it. Each run takes
//! a slot before its first step and holds it until it ends; a run that finds
//! every slot taken waits for one, saying so.
//!
//! A slot is an exclusive lock on one of `max_concurrent_runs` files under
//! `~/.ciabatta/run-slots/<workspace>/`. Locks rather than a counter in a file
//! because the operating system releases a lock when its holder dies: a run
//! killed with `kill -9`, or a daemon that crashed mid-run, can't leave a slot
//! taken forever. They are shared by every process — terminal runs, the
//! daemon's runs, two terminals side by side — which is the point.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};

use crate::runner::Cancel;

/// How often a waiting run tries again.
const POLL: Duration = Duration::from_millis(500);

/// A held slot. Dropping it frees the slot.
#[derive(Debug)]
pub struct Slot {
    _lock: File,
}

/// The workspace's limit, from `workspace.max_concurrent_runs`. `None` —
/// unset, or `0` — means no limit.
pub fn limit(config: &crate::config::CiabattaConfig) -> Option<usize> {
    config
        .workspace
        .as_ref()
        .and_then(|w| w.max_concurrent_runs)
        .filter(|n| *n > 0)
}

/// Where `root`'s slot files live: one directory per workspace, named for a
/// digest of its path so two checkouts of the same repo count separately.
pub fn dir_for(root: &Path) -> Result<PathBuf> {
    use sha2::Digest;
    let canonical = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let digest = sha2::Sha256::digest(canonical.to_string_lossy().as_bytes());
    let name: String = digest[..8].iter().map(|b| format!("{b:02x}")).collect();
    let dir = crate::daemon::state_dir()?.join("run-slots").join(name);
    std::fs::create_dir_all(&dir).with_context(|| format!("Failed to create {}", dir.display()))?;
    Ok(dir)
}

/// Take a free slot in `dir` if there is one.
pub fn try_take(dir: &Path, limit: usize) -> Result<Option<Slot>> {
    for index in 0..limit {
        let path = dir.join(format!("slot-{index}.lock"));
        let file = File::options()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)
            .with_context(|| format!("Failed to open {}", path.display()))?;
        match file.try_lock() {
            Ok(()) => return Ok(Some(Slot { _lock: file })),
            Err(std::fs::TryLockError::WouldBlock) => continue,
            Err(std::fs::TryLockError::Error(e)) => {
                return Err(e).with_context(|| format!("Failed to lock {}", path.display()));
            }
        }
    }
    Ok(None)
}

/// Wait for a slot in `dir`, calling `note` once if the run has to queue.
///
/// Gives up — with the engine's stop message — when `cancel` is pressed, so a
/// queued run in the web app can be stopped before it ever starts.
pub async fn take(
    dir: &Path,
    limit: usize,
    cancel: Option<&Cancel>,
    mut note: impl FnMut(String),
) -> Result<Slot> {
    let started = std::time::Instant::now();
    let mut told = false;
    loop {
        if let Some(slot) = try_take(dir, limit)? {
            if told {
                note(format!(
                    "a run slot came free after {}; starting",
                    crate::runner::elapsed(started.elapsed())
                ));
            }
            return Ok(slot);
        }
        if !told {
            note(format!(
                "waiting for a run slot: this workspace allows {limit} run{} at a time \
                 on this machine (workspace.max_concurrent_runs)",
                if limit == 1 { "" } else { "s" }
            ));
            told = true;
        }
        match cancel {
            Some(cancel) => {
                tokio::select! {
                    () = tokio::time::sleep(POLL) => {}
                    () = cancel.stopped() => anyhow::bail!(crate::runner::STOPPED_MESSAGE),
                }
            }
            None => tokio::time::sleep(POLL).await,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ciab_slots_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn slots_run_out_and_come_back() {
        let dir = scratch("slots_run_out_and_come_back");
        let a = try_take(dir.as_path(), 2).unwrap().expect("first slot");
        let _b = try_take(dir.as_path(), 2).unwrap().expect("second slot");
        assert!(try_take(dir.as_path(), 2).unwrap().is_none(), "both taken");
        drop(a);
        assert!(
            try_take(dir.as_path(), 2).unwrap().is_some(),
            "freed on drop"
        );
    }

    #[tokio::test]
    async fn a_waiting_run_says_so_and_starts_when_freed() {
        let dir = scratch("a_waiting_run_says_so_and_starts_when_freed");
        let held = try_take(dir.as_path(), 1).unwrap().unwrap();
        let path = dir.clone();
        let notes = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen = notes.clone();
        let waiter =
            tokio::spawn(
                async move { take(&path, 1, None, |n| seen.lock().unwrap().push(n)).await },
            );
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(!waiter.is_finished());
        drop(held);
        waiter.await.unwrap().unwrap();
        let notes = notes.lock().unwrap();
        assert!(notes[0].contains("waiting for a run slot"), "{notes:?}");
        assert!(notes[1].contains("came free"), "{notes:?}");
    }

    #[tokio::test]
    async fn stopping_a_queued_run_ends_the_wait() {
        let dir = scratch("stopping_a_queued_run_ends_the_wait");
        let _held = try_take(dir.as_path(), 1).unwrap().unwrap();
        let cancel = Cancel::default();
        cancel.stop();
        let err = take(dir.as_path(), 1, Some(&cancel), |_| {})
            .await
            .unwrap_err();
        assert_eq!(err.to_string(), crate::runner::STOPPED_MESSAGE);
    }
}
