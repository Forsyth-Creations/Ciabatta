//! When each workflow last actually ran, so a workflow nobody runs any more can
//! say so.
//!
//! A monorepo accumulates workflows the way it accumulates scripts: somebody
//! adds `deploy-staging`, the staging environment goes away, and the workflow
//! stays — still listed, still documented, still apparently a thing you could
//! run, and broken in some way nobody will discover until they try. Nothing in
//! the repository records the one fact that would have flagged it, which is
//! that no one has run it since March.
//!
//! So every run writes down what it ran and how it went, and anything that
//! hasn't been run inside [`DEFAULT_STALE_AFTER`] is reported as stale.
//!
//! Two things this is deliberately not:
//!
//! * **Not committed.** It lives under `.ciabatta/history/`, which is ignored,
//!   because it is observation rather than configuration — and because a file
//!   every run rewrites would conflict on every merge. The consequence is that
//!   a fresh checkout knows nothing, which is why "never run here" is a
//!   different answer from "stale" rather than the same one.
//!
//! * **Not a substitute for the remote.** One laptop's history is one person's
//!   habits. A workflow you personally haven't run since March may be the one
//!   CI runs hourly. When a remote cache is configured the records are reported
//!   to it and merged back, so "when did anyone last run this" is answerable —
//!   see [`crate::remote_cache::workflows`].

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::config::CIABATTA_DIR;

/// Where the file lives under `.ciabatta/`. Its own directory rather than a
/// file in `cache/`, because `ciabatta cache clean` empties that one and losing
/// the history to a cache wipe would be a surprising way to lose it.
const HISTORY_DIR: &str = "history";
const FILE: &str = "workflows.json";

/// How long a workflow may go unrun before it is called stale.
///
/// A month: long enough that a workflow run at the start of every sprint is
/// never flagged, short enough that one abandoned in March is flagged by May.
/// Override it with `workspace.stale_after` in the root config.
pub const DEFAULT_STALE_AFTER: &str = "30d";

/// Bumped if the shape changes, so an old file is ignored rather than misread.
const VERSION: u32 = 1;

/// How a run ended, from the point of view of "did this workflow work".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Outcome {
    Success,
    Failed,
    /// Somebody stopped it. Recorded, because it still says the workflow is in
    /// use — but kept apart from a failure, which it isn't.
    Stopped,
}

impl Outcome {
    pub fn label(self) -> &'static str {
        match self {
            Outcome::Success => "success",
            Outcome::Failed => "failed",
            Outcome::Stopped => "stopped",
        }
    }
}

/// One workflow's record: when it last ran, how it went, and how much it has
/// been used.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Record {
    /// The sub-workspace it belongs to, or `"."` for a plain project.
    pub workspace: String,
    /// The workflow name — `build`, `test`, `deploy`.
    pub workflow: String,
    /// RFC 3339, of the most recent run.
    pub last_run_at: String,
    pub last_outcome: Outcome,
    pub last_duration_ms: u64,
    /// RFC 3339, of the first run this file ever saw. Together with `runs` it
    /// answers "is this used weekly or was it used twice in 2024".
    pub first_run_at: String,
    pub runs: u64,
    /// Runs that ended in failure, so a workflow that is run often and fails
    /// every time isn't hidden behind a recent timestamp.
    #[serde(default)]
    pub failures: u64,
}

impl Record {
    /// `member:workflow` — how a record is keyed and how it is printed.
    pub fn id(&self) -> String {
        format!("{}:{}", self.workspace, self.workflow)
    }

    /// Whole days since it last ran, or `None` if the timestamp won't parse.
    pub fn days_since(&self) -> Option<i64> {
        let then = chrono::DateTime::parse_from_rfc3339(&self.last_run_at).ok()?;
        Some((chrono::Utc::now() - then.with_timezone(&chrono::Utc)).num_days())
    }

    /// Whether it has gone unrun for longer than `stale_after`.
    pub fn is_stale(&self, stale_after: std::time::Duration) -> bool {
        let Ok(then) = chrono::DateTime::parse_from_rfc3339(&self.last_run_at) else {
            // An unparseable timestamp is not evidence of staleness. Saying
            // "stale" on the strength of a corrupt file would send somebody to
            // delete a workflow that is run daily.
            return false;
        };
        let age = chrono::Utc::now() - then.with_timezone(&chrono::Utc);
        age.to_std().map(|age| age > stale_after).unwrap_or(false)
    }
}

/// Every workflow this checkout has run, keyed by `member:workflow`.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct History {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    workflows: BTreeMap<String, Record>,
}

fn path_for(root: &Path) -> PathBuf {
    root.join(CIABATTA_DIR).join(HISTORY_DIR).join(FILE)
}

impl History {
    /// Read the history for a project.
    ///
    /// Never fails: no file, an unreadable one, or one written by a future
    /// version all mean "nothing is known yet". History is a nicety, and
    /// refusing to run a build because a JSON file was damaged would be a
    /// spectacularly bad trade.
    pub fn load(root: &Path) -> History {
        let Ok(text) = std::fs::read_to_string(path_for(root)) else {
            return History::default();
        };
        match serde_json::from_str::<History>(&text) {
            Ok(history) if history.version == VERSION => history,
            Ok(_) => {
                tracing::debug!("workflow history is from another version; starting fresh");
                History::default()
            }
            Err(e) => {
                tracing::warn!("couldn't read the workflow history ({e}); starting fresh");
                History::default()
            }
        }
    }

    /// Every record, most recently run first.
    pub fn records(&self) -> Vec<&Record> {
        let mut all: Vec<&Record> = self.workflows.values().collect();
        all.sort_by(|a, b| b.last_run_at.cmp(&a.last_run_at));
        all
    }

    /// One workflow's record.
    pub fn get(&self, workspace: &str, workflow: &str) -> Option<&Record> {
        self.workflows.get(&format!("{workspace}:{workflow}"))
    }

    /// Fold a record in, keeping whichever is more recent.
    ///
    /// Used both by a local run and by what comes back from the remote, which
    /// is why it merges rather than overwrites: a colleague's run of this
    /// workflow yesterday beats mine from March, and mine from this morning
    /// beats theirs from yesterday.
    pub fn merge(&mut self, incoming: Record) {
        let key = incoming.id();
        match self.workflows.get_mut(&key) {
            Some(existing) => {
                existing.runs = existing.runs.max(incoming.runs);
                existing.failures = existing.failures.max(incoming.failures);
                if incoming.first_run_at < existing.first_run_at {
                    existing.first_run_at = incoming.first_run_at;
                }
                if incoming.last_run_at > existing.last_run_at {
                    existing.last_run_at = incoming.last_run_at;
                    existing.last_outcome = incoming.last_outcome;
                    existing.last_duration_ms = incoming.last_duration_ms;
                }
            }
            None => {
                self.workflows.insert(key, incoming);
            }
        }
    }

    /// Note that a workflow just ran, and return the record as it now stands.
    pub fn record(
        &mut self,
        workspace: &str,
        workflow: &str,
        outcome: Outcome,
        duration_ms: u64,
    ) -> Record {
        let now = chrono::Local::now().to_rfc3339();
        let key = format!("{workspace}:{workflow}");
        let entry = self.workflows.entry(key).or_insert_with(|| Record {
            workspace: workspace.to_string(),
            workflow: workflow.to_string(),
            last_run_at: now.clone(),
            last_outcome: outcome,
            last_duration_ms: duration_ms,
            first_run_at: now.clone(),
            runs: 0,
            failures: 0,
        });
        entry.last_run_at = now;
        entry.last_outcome = outcome;
        entry.last_duration_ms = duration_ms;
        entry.runs += 1;
        if outcome == Outcome::Failed {
            entry.failures += 1;
        }
        entry.clone()
    }

    /// Write it back.
    pub fn save(&mut self, root: &Path) -> Result<()> {
        self.version = VERSION;
        let path = path_for(root);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create {}", parent.display()))?;
        }
        let text = serde_json::to_string_pretty(self).context("Failed to encode the history")?;
        std::fs::write(&path, text).with_context(|| format!("Failed to write {}", path.display()))
    }
}

/// How long a workflow may go unrun before this project calls it stale.
///
/// From `workspace.stale_after` in the root config, falling back to
/// [`DEFAULT_STALE_AFTER`]. An unparseable value falls back too, with a
/// warning: a typo in a threshold should not turn the feature off silently.
pub fn stale_after(config: &crate::config::CiabattaConfig) -> std::time::Duration {
    let declared = config
        .workspace
        .as_ref()
        .and_then(|w| w.stale_after.as_deref());
    let raw = declared.unwrap_or(DEFAULT_STALE_AFTER);
    // `parse_duration` answers in seconds — the retention policy's unit, and
    // the one that makes `30d` expressible, which the step-timeout parser
    // deliberately does not.
    let seconds = match crate::cache::store::parse_duration(raw) {
        Ok(seconds) => seconds,
        Err(e) => {
            eprintln!(
                "note: workspace.stale_after ('{raw}') isn't a duration ({e}); \
                 using {DEFAULT_STALE_AFTER}"
            );
            crate::cache::store::parse_duration(DEFAULT_STALE_AFTER)
                .expect("the default is a valid duration")
        }
    };
    std::time::Duration::from_secs(seconds.max(0) as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ciab_hist_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn at(days_ago: i64) -> String {
        (chrono::Utc::now() - chrono::Duration::days(days_ago)).to_rfc3339()
    }

    fn record(workflow: &str, days_ago: i64) -> Record {
        Record {
            workspace: "api".into(),
            workflow: workflow.into(),
            last_run_at: at(days_ago),
            last_outcome: Outcome::Success,
            last_duration_ms: 1_000,
            first_run_at: at(days_ago + 10),
            runs: 3,
            failures: 0,
        }
    }

    const MONTH: std::time::Duration = std::time::Duration::from_secs(30 * 24 * 60 * 60);

    #[test]
    fn a_run_is_recorded_and_survives_a_round_trip() {
        let root = scratch("roundtrip");
        let mut history = History::load(&root);
        assert!(
            history.records().is_empty(),
            "a fresh checkout knows nothing"
        );

        history.record("api", "build", Outcome::Success, 1_500);
        history.record("api", "build", Outcome::Failed, 900);
        history.save(&root).unwrap();

        let reloaded = History::load(&root);
        let entry = reloaded.get("api", "build").expect("recorded");
        assert_eq!(entry.runs, 2);
        assert_eq!(entry.failures, 1, "the failure is counted separately");
        assert_eq!(entry.last_outcome, Outcome::Failed, "the latest run wins");
        assert_eq!(entry.last_duration_ms, 900);
        assert!(entry.first_run_at <= entry.last_run_at);

        let _ = std::fs::remove_dir_all(&root);
    }

    /// Staleness is a verdict, so it must not be reached lightly.
    #[test]
    fn staleness_is_measured_from_the_last_run() {
        assert!(!record("build", 1).is_stale(MONTH), "run yesterday");
        assert!(!record("build", 29).is_stale(MONTH), "just inside");
        assert!(record("build", 31).is_stale(MONTH), "just outside");

        // A corrupt timestamp must not read as stale: it is not evidence of
        // anything, and acting on it would mean deleting a workflow run daily.
        let mut broken = record("build", 400);
        broken.last_run_at = "not a timestamp".into();
        assert!(!broken.is_stale(MONTH));
    }

    /// Reports arrive from many machines, out of order, from clocks that
    /// disagree. A slow one must never walk the picture backwards.
    #[test]
    fn merging_keeps_the_later_run_whichever_way_round_it_arrives() {
        let recent = Record {
            last_outcome: Outcome::Failed,
            runs: 9,
            ..record("build", 1)
        };
        let old = Record {
            last_outcome: Outcome::Success,
            runs: 2,
            ..record("build", 90)
        };

        for (first, second) in [(recent.clone(), old.clone()), (old.clone(), recent.clone())] {
            let mut history = History::default();
            history.merge(first);
            history.merge(second);
            let got = history.get("api", "build").unwrap();
            assert_eq!(
                got.last_run_at, recent.last_run_at,
                "the later run has to win in either order"
            );
            assert_eq!(got.last_outcome, Outcome::Failed);
            assert_eq!(got.runs, 9, "counts take the larger");
            assert_eq!(
                got.first_run_at, old.first_run_at,
                "and the earliest first-seen is kept"
            );
        }
    }

    /// A damaged or future-version file means "nothing known", never a failure:
    /// refusing to run a build because a timestamp file was corrupt would be a
    /// spectacularly bad trade.
    #[test]
    fn an_unreadable_history_is_empty_rather_than_fatal() {
        let root = scratch("damaged");
        let dir = root.join(crate::config::CIABATTA_DIR).join(HISTORY_DIR);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(FILE), "{ this is not json").unwrap();
        assert!(History::load(&root).records().is_empty());

        std::fs::write(dir.join(FILE), r#"{"version":9999,"workflows":{}}"#).unwrap();
        assert!(History::load(&root).records().is_empty());

        let _ = std::fs::remove_dir_all(&root);
    }
}
