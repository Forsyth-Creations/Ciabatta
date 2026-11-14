//! Offloading: running a workflow on another machine, through the remote cache.
//!
//! ```text
//! ciabatta oven executor            on the build box: advertise, pair, take jobs
//! ciabatta build --offload          anywhere: run `build` on an executor instead
//! ciabatta offload cancel <job>     and the commands that look after a job
//! ```
//!
//! Three parties, and the remote cache is the one in the middle:
//!
//! * **The executor** ([`executor`]) advertises itself on the local network
//!   over mDNS (`_ciabatta-oven._tcp`), the way a Chromecast does, and prints
//!   its address and a pairing code. It answers exactly two requests of its
//!   own — what it is, and "pair with this cache" — and nothing else: once
//!   paired it holds a token for the cache, and from then on it *pulls*. It
//!   asks the cache for its next job, runs one at a time, and posts its status,
//!   its log and its outputs back. So the cache only ever has to reach the
//!   executor once, to pair, and an executor behind NAT works as well as one
//!   next to the server.
//! * **The remote cache** ([`crate::remote_cache::oven`]) lists the executors
//!   it can see on the network and the ones paired with it, queues jobs, hands
//!   each to an idle executor, and keeps the job's log and outputs for the
//!   client to read.
//! * **The client** ([`offload`]) is any `ciabatta` command run with
//!   `--offload`. It checks the commit is one the executor can fetch, submits
//!   the job, prints its id, and streams the log. When the job ends, the files
//!   its steps declared as cache `outputs` land under `offloaded/` (or
//!   `workspace.offload_dir`), in a folder named for the job's execution
//!   number — so two variants of the same commit (different `-e` flags, say)
//!   each land in their own.
//!
//! The types here are the protocol between them, shared so the three can't
//! drift apart.

pub mod executor;
pub mod offload;

/// The mDNS service type executors advertise and the cache browses for.
pub const SERVICE_TYPE: &str = "_ciabatta-oven._tcp.local.";

/// The port an executor listens on for pairing when nothing says otherwise:
/// the remote cache's, plus one.
pub const DEFAULT_PORT: u16 = 8381;

/// Where outputs land in the client's checkout when `workspace.offload_dir`
/// doesn't say.
pub const DEFAULT_OFFLOAD_DIR: &str = "offloaded";

/// The job's number, set in an offloaded run's environment: what its outputs
/// are filed under on the way back, so a step can name things after it too.
pub const JOB_VAR: &str = "CIABATTA_EXECUTION_NUMBER";

/// How long an executor's poll for work is held open before it's answered
/// with "nothing yet".
pub const POLL_SECONDS: u64 = 25;

/// An executor that hasn't been heard from for this long is offline.
pub const OFFLINE_AFTER_SECONDS: i64 = 60;

pub use proto::{
    Availability, Discovered, ExecutorHistory, ExecutorInfo, ExecutorList, ExecutorState,
    ExecutorView, Finish, Heartbeat, HeartbeatReply, Job, JobList, JobSpec, JobState, LogBatch,
    LogPage, OutputFile, PairRequest, PairWith, Paired, Poll, StatusChange,
};

/// The protocol's types, generated from `proto/oven.proto` by `build.rs`:
/// prost for the types, pbjson for the JSON they travel as. The web app's
/// TypeScript comes from the same file — see the test below.
#[allow(clippy::all, clippy::pedantic, missing_docs)]
pub mod proto {
    include!(concat!(env!("OUT_DIR"), "/ciabatta.oven.rs"));
    include!(concat!(env!("OUT_DIR"), "/ciabatta.oven.serde.rs"));
}

impl JobState {
    pub fn is_done(self) -> bool {
        matches!(
            self,
            JobState::Succeeded | JobState::Failed | JobState::Cancelled
        )
    }

    pub fn word(self) -> &'static str {
        match self {
            JobState::Unspecified => "unknown",
            JobState::Queued => "queued",
            JobState::Running => "running",
            JobState::Succeeded => "succeeded",
            JobState::Failed => "failed",
            JobState::Cancelled => "cancelled",
        }
    }
}

impl Job {
    /// What was asked for. Always set on a job the cache made; the default is
    /// for one that somehow arrived without it.
    pub fn spec(&self) -> &JobSpec {
        static EMPTY: std::sync::OnceLock<JobSpec> = std::sync::OnceLock::new();
        self.spec
            .as_ref()
            .unwrap_or_else(|| EMPTY.get_or_init(JobSpec::default))
    }

    /// The commit, short enough to read.
    pub fn short_commit(&self) -> &str {
        let commit = &self.spec().commit;
        &commit[..commit.len().min(12)]
    }
}

/// `linux-x86_64`, `macos-aarch64`: what an executor runs on, for the page to
/// show and a person to choose by.
pub fn platform() -> String {
    format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH)
}

/// Where a job's outputs go under the offload directory: its execution
/// number, so every run — every variant of the same commit — has its own.
pub fn output_dir(offload_dir: &std::path::Path, job: u32) -> std::path::PathBuf {
    offload_dir.join(job.to_string())
}

/// A relative output path, checked: no `..`, no root, no drive.
pub fn safe_output_path(raw: &str) -> anyhow::Result<std::path::PathBuf> {
    use std::path::Component;
    let mut out = std::path::PathBuf::new();
    for component in std::path::Path::new(raw).components() {
        match component {
            Component::Normal(part) => out.push(part),
            _ => anyhow::bail!("'{raw}' is not a plain relative path"),
        }
    }
    anyhow::ensure!(!out.as_os_str().is_empty(), "an output path can't be empty");
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The checked-in TypeScript is what `build.rs` generates from the
    /// `.proto` now. `CIABATTA_REGENERATE=1` writes it instead of comparing.
    #[test]
    fn generated_typescript_is_current() {
        let generated = include_str!(concat!(env!("OUT_DIR"), "/oven.ts"));
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("remote-cache-web/src/gen/oven.ts");
        if std::env::var_os("CIABATTA_REGENERATE").is_some() {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, generated).unwrap();
            return;
        }
        let checked_in = std::fs::read_to_string(&path).unwrap_or_default();
        assert!(
            checked_in == generated,
            "remote-cache-web/src/gen/oven.ts is out of date with proto/oven.proto. Run \
             `CIABATTA_REGENERATE=1 cargo test generated_typescript` to update it."
        );
    }

    #[test]
    fn the_json_is_snake_case_with_enums_by_name() {
        let job = Job {
            id: 7,
            state: JobState::Running as i32,
            requested_by: "me".into(),
            ..Default::default()
        };
        let json = serde_json::to_value(&job).unwrap();
        assert_eq!(json["id"], 7);
        assert_eq!(json["state"], "JOB_STATE_RUNNING");
        assert_eq!(json["requested_by"], "me");
        // Every field is there, set or not.
        assert_eq!(json["cancel_requested"], false);
        let back: Job = serde_json::from_value(json).unwrap();
        assert_eq!(back, job);
        // A field this version doesn't know isn't a reason to refuse.
        let newer: Job =
            serde_json::from_str(r#"{"id": 1, "state": "JOB_STATE_QUEUED", "from_the_future": 1}"#)
                .unwrap();
        assert_eq!(newer.state(), JobState::Queued);
    }

    #[test]
    fn each_execution_gets_its_own_folder() {
        let dir = std::path::Path::new("offloaded");
        assert_eq!(
            output_dir(dir, 42),
            std::path::PathBuf::from("offloaded/42")
        );
        assert_ne!(output_dir(dir, 42), output_dir(dir, 43));
    }

    #[test]
    fn a_path_that_climbs_out_is_refused() {
        assert!(safe_output_path("../x").is_err());
        assert!(safe_output_path("/etc/passwd").is_err());
        assert!(safe_output_path("").is_err());
        assert_eq!(
            safe_output_path("target/release/app").unwrap(),
            std::path::PathBuf::from("target/release/app")
        );
    }
}
