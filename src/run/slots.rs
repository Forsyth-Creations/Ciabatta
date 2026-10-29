//! How many of a workspace's workflows may run at once on this machine.
//!
//! `workspace.max_concurrent_runs` in the root config caps it — one at a time
//! unless it says otherwise, and `0` for no limit. Each run takes a slot before
//! its first step and holds it until it ends; a run that finds every slot taken
//! waits for one, saying so.
//!
//! A run started *by a step* of a run that holds a slot — a script that calls
//! `ciabatta test` — shares its parent's slot instead of queueing for one: with
//! the default of one, it would otherwise wait forever on the run waiting for
//! it. [`SLOT_VAR`] in a step's environment is how the child knows.
//!
//! A slot is an exclusive lock on one of `max_concurrent_runs` files under
//! `~/.ciabatta/run-slots/<workspace>/`. Locks rather than a counter in a file
//! because the operating system releases a lock when its holder dies: a run
//! killed with `kill -9`, or a daemon that crashed mid-run, can't leave a slot
//! taken forever. They are shared by every process — terminal runs, the
//! daemon's runs, two terminals side by side — which is the point.
//!
//! Next to each lock, a run writes a `.pid` file naming itself, so a run left
//! waiting can say what it is waiting on, and `ciabatta self reset` can find
//! the processes to stop. A file of its own rather than the lock's contents:
//! Windows won't let another process read a file somebody holds locked.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};

use crate::runner::Cancel;

/// Set in every step's environment to the slot directory its run holds a slot
/// in, so a nested run of the same workspace knows not to queue behind it.
pub const SLOT_VAR: &str = "CIABATTA_RUN_SLOT";

/// The limit when `max_concurrent_runs` is unset: one run at a time.
pub const DEFAULT_LIMIT: usize = 1;

/// How often a waiting run tries again.
const POLL: Duration = Duration::from_millis(500);

/// What a stuck run is told to do about it.
pub const RESET_HINT: &str = "Pass `--start-anyway` to start without waiting. If you believe \
     this is in error and a process is still running that shouldn't be, run `ciabatta self reset`";

/// A held slot. Dropping it frees the slot.
#[derive(Debug)]
pub struct Slot {
    _lock: File,
    pid_file: PathBuf,
}

impl Drop for Slot {
    fn drop(&mut self) {
        // Before the lock goes (fields drop after this), so the next holder's
        // own pid file can't be the one removed.
        let _ = std::fs::remove_file(&self.pid_file);
    }
}

/// The run holding a slot, as its `.pid` file tells it.
#[derive(Debug, Clone, PartialEq)]
pub struct Holder {
    pub pid: u32,
    /// The workflow it is running.
    pub workflow: String,
    /// When it took the slot, local time.
    pub since: String,
}

impl Holder {
    fn parse(raw: &str) -> Option<Self> {
        let mut lines = raw.lines();
        Some(Self {
            pid: lines.next()?.trim().parse().ok()?,
            workflow: lines.next().unwrap_or_default().to_string(),
            since: lines.next().unwrap_or_default().to_string(),
        })
    }

    fn describe(&self) -> String {
        format!(
            "`{}` (pid {}, since {})",
            self.workflow, self.pid, self.since
        )
    }
}

fn lock_path(dir: &Path, index: usize) -> PathBuf {
    dir.join(format!("slot-{index}.lock"))
}

fn pid_path(lock: &Path) -> PathBuf {
    lock.with_extension("pid")
}

/// The workspace's limit, from `workspace.max_concurrent_runs`: unset is
/// [`DEFAULT_LIMIT`], and `0` — `None` here — is no limit at all.
pub fn limit(config: &crate::config::CiabattaConfig) -> Option<usize> {
    match config
        .workspace
        .as_ref()
        .and_then(|w| w.max_concurrent_runs)
    {
        None => Some(DEFAULT_LIMIT),
        Some(0) => None,
        Some(n) => Some(n),
    }
}

/// Whether this run was started by a step of a run already holding a slot in
/// `dir` — and so runs inside that slot rather than queueing for its own.
pub fn inherited(dir: &Path, env: &std::collections::HashMap<String, String>) -> bool {
    let held = env
        .get(SLOT_VAR)
        .cloned()
        .or_else(|| std::env::var(SLOT_VAR).ok());
    held.is_some_and(|held| Path::new(&held) == dir)
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

/// Take a free slot in `dir` if there is one, for a run of `workflow`.
pub fn try_take(dir: &Path, limit: usize, workflow: &str) -> Result<Option<Slot>> {
    for index in 0..limit {
        let path = lock_path(dir, index);
        let file = File::options()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)
            .with_context(|| format!("Failed to open {}", path.display()))?;
        match file.try_lock() {
            Ok(()) => {
                let pid_file = pid_path(&path);
                // Best effort: a missing pid file only costs the waiting
                // message its detail, and `self reset` its target.
                let _ = std::fs::write(
                    &pid_file,
                    format!(
                        "{}\n{workflow}\n{}\n",
                        std::process::id(),
                        chrono::Local::now().format("%H:%M:%S")
                    ),
                );
                return Ok(Some(Slot {
                    _lock: file,
                    pid_file,
                }));
            }
            Err(std::fs::TryLockError::WouldBlock) => continue,
            Err(std::fs::TryLockError::Error(e)) => {
                return Err(e).with_context(|| format!("Failed to lock {}", path.display()));
            }
        }
    }
    Ok(None)
}

/// The runs holding slots in `dir`, as far as their pid files say.
pub fn holders(dir: &Path, limit: usize) -> Vec<Holder> {
    (0..limit)
        .filter_map(|index| std::fs::read_to_string(pid_path(&lock_path(dir, index))).ok())
        .filter_map(|raw| Holder::parse(&raw))
        .collect()
}

/// Every slot on this machine that is still held, with whoever holds it.
///
/// "Still held" is checked against the lock itself, not the pid file: a run
/// that died left its pid file behind, and the pid in it may by now belong to
/// some unrelated process that must not be the one `self reset` kills.
pub fn held_everywhere() -> Result<Vec<Holder>> {
    let root = crate::daemon::state_dir()?.join("run-slots");
    let mut held = Vec::new();
    let Ok(workspaces) = std::fs::read_dir(&root) else {
        return Ok(held);
    };
    for workspace in workspaces.flatten() {
        let Ok(files) = std::fs::read_dir(workspace.path()) else {
            continue;
        };
        for file in files.flatten() {
            let lock = file.path();
            if lock.extension().is_none_or(|e| e != "lock") {
                continue;
            }
            let free = File::options()
                .write(true)
                .open(&lock)
                .is_ok_and(|f| f.try_lock().is_ok());
            if free {
                continue;
            }
            if let Some(holder) = std::fs::read_to_string(pid_path(&lock))
                .ok()
                .and_then(|raw| Holder::parse(&raw))
            {
                held.push(holder);
            }
        }
    }
    Ok(held)
}

/// Remove every slot file on this machine. Only safe once their holders are
/// gone — which is what `self reset` makes sure of first.
pub fn clear_all() -> Result<()> {
    let root = crate::daemon::state_dir()?.join("run-slots");
    match std::fs::remove_dir_all(&root) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e).with_context(|| format!("Failed to remove {}", root.display())),
    }
}

/// Wait for a slot in `dir`, calling `note` once if the run has to queue.
///
/// Gives up — with the engine's stop message — when `cancel` is pressed, so a
/// queued run in the web app can be stopped before it ever starts.
pub async fn take(
    dir: &Path,
    limit: usize,
    workflow: &str,
    cancel: Option<&Cancel>,
    mut note: impl FnMut(String),
) -> Result<Slot> {
    let started = std::time::Instant::now();
    let mut told = false;
    loop {
        if let Some(slot) = try_take(dir, limit, workflow)? {
            if told {
                note(format!(
                    "a run slot came free after {}; starting",
                    crate::runner::elapsed(started.elapsed())
                ));
            }
            return Ok(slot);
        }
        if !told {
            let holders = holders(dir, limit);
            let held_by = match holders.is_empty() {
                true => String::new(),
                false => format!(
                    "; held by {}",
                    holders
                        .iter()
                        .map(Holder::describe)
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            };
            note(format!(
                "waiting for a run slot: this workspace allows {limit} run{} at a time \
                 on this machine (workspace.max_concurrent_runs){held_by}. {RESET_HINT}",
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
    fn unset_is_one_at_a_time_and_zero_is_no_limit() {
        let read = |yaml: &str| {
            let config: crate::config::CiabattaConfig =
                crate::format::from_str(yaml, crate::format::Format::Yaml).unwrap();
            limit(&config)
        };
        assert_eq!(read("workspace: {}\n"), Some(1));
        assert_eq!(read("{}\n"), Some(1), "no workspace table at all");
        assert_eq!(read("workspace:\n  max_concurrent_runs: 0\n"), None);
        assert_eq!(read("workspace:\n  max_concurrent_runs: 3\n"), Some(3));
    }

    #[test]
    fn a_run_started_by_a_step_shares_its_parents_slot() {
        let dir = scratch("inherited");
        let env = std::collections::HashMap::from([(
            SLOT_VAR.to_string(),
            dir.to_string_lossy().into_owned(),
        )]);
        assert!(inherited(&dir, &env));
        assert!(
            !inherited(&scratch("elsewhere"), &env),
            "another workspace queues"
        );
    }

    #[test]
    fn slots_run_out_and_come_back() {
        let dir = scratch("slots_run_out_and_come_back");
        let a = try_take(dir.as_path(), 2, "t")
            .unwrap()
            .expect("first slot");
        let _b = try_take(dir.as_path(), 2, "t")
            .unwrap()
            .expect("second slot");
        assert!(
            try_take(dir.as_path(), 2, "t").unwrap().is_none(),
            "both taken"
        );
        drop(a);
        // Retried briefly rather than checked once: a process another test
        // spawns in parallel holds a copy of every open descriptor — this
        // lock included — between its fork and its exec, so the lock can
        // outlive the drop by a moment. Real runs poll for a slot anyway
        // (see `POLL`); this flaked on macOS CI.
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let freed = loop {
            if let Some(slot) = try_take(dir.as_path(), 2, "t").unwrap() {
                break Some(slot);
            }
            if std::time::Instant::now() >= deadline {
                break None;
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        assert!(freed.is_some(), "freed on drop");
    }

    #[tokio::test]
    async fn a_waiting_run_says_so_and_starts_when_freed() {
        let dir = scratch("a_waiting_run_says_so_and_starts_when_freed");
        let held = try_take(dir.as_path(), 1, "t").unwrap().unwrap();
        let path = dir.clone();
        let notes = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen = notes.clone();
        let waiter = tokio::spawn(async move {
            take(&path, 1, "w", None, |n| seen.lock().unwrap().push(n)).await
        });
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(!waiter.is_finished());
        drop(held);
        waiter.await.unwrap().unwrap();
        let notes = notes.lock().unwrap();
        assert!(notes[0].contains("waiting for a run slot"), "{notes:?}");
        assert!(notes[0].contains("held by `t`"), "{notes:?}");
        assert!(notes[0].contains("ciabatta self reset"), "{notes:?}");
        assert!(notes[1].contains("came free"), "{notes:?}");
    }

    #[test]
    fn a_held_slot_names_its_holder_until_it_is_freed() {
        let dir = scratch("a_held_slot_names_its_holder_until_it_is_freed");
        let held = try_take(dir.as_path(), 1, "build").unwrap().unwrap();
        let named = holders(&dir, 1);
        assert_eq!(named.len(), 1);
        assert_eq!(named[0].pid, std::process::id());
        assert_eq!(named[0].workflow, "build");
        drop(held);
        assert!(
            holders(&dir, 1).is_empty(),
            "the pid file goes with the slot"
        );
    }

    #[tokio::test]
    async fn stopping_a_queued_run_ends_the_wait() {
        let dir = scratch("stopping_a_queued_run_ends_the_wait");
        let _held = try_take(dir.as_path(), 1, "t").unwrap().unwrap();
        let cancel = Cancel::default();
        cancel.stop();
        let err = take(dir.as_path(), 1, "w", Some(&cancel), |_| {})
            .await
            .unwrap_err();
        assert_eq!(err.to_string(), crate::runner::STOPPED_MESSAGE);
    }
}
