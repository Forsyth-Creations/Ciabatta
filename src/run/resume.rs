//! Re-running a workflow from where it failed.
//!
//! A run that fails in its last step has usually done twenty minutes of work
//! that is still good. `--from-failed`, and the run page's "Re-run from failed",
//! start the workflow again without repeating the steps that passed: those are
//! carried over, and the failed step and everything behind it run.
//!
//! The graph is compiled fresh, so a fix to the workflow file is picked up. That
//! is also why a step is only carried while it can still be trusted to be the
//! step that passed: see [`carried`]. What it can't see is a change on disk that
//! a passed step would have reacted to. That is the operator's call, and "Run
//! again" is the answer when the earlier steps need repeating.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use super::RunStep;
use crate::config::CIABATTA_DIR;

/// Where the records live under `.ciabatta/`, beside the workflow history and
/// for the same reasons: ignored, and not emptied by `ciabatta cache clean`.
const DIR: &str = "history/last-runs";

/// A step's status when it passed by being reused from the cache.
pub const REUSED: &str = "reused";

/// What one step of an earlier run did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PriorStep {
    pub name: String,
    /// `success`, `reused`, `skipped`, `warning`, `failed`, `not run`, …
    pub status: String,
    /// The command it ran, so a step whose command has since changed runs again.
    #[serde(default)]
    pub shell: Option<String>,
}

/// What a run is carrying on from.
#[derive(Debug, Clone, Default)]
pub struct Resume {
    /// How to name the earlier run in a log line: `run #12`, `the last run`.
    pub from: String,
    pub prior: Vec<PriorStep>,
}

/// The record a finished run leaves for `--from-failed`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LastRun {
    /// `success`, `failed` or `stopped`.
    pub outcome: String,
    pub finished_at: String,
    pub steps: Vec<PriorStep>,
}

/// Which of `steps` are carried over rather than run again.
///
/// A step is carried when it passed, its command is the one that passed, and
/// everything it needs still stands behind it. The last of those is what makes
/// an edit safe: change an early step's command and it runs again, and so does
/// everything downstream of it.
///
/// Three kinds are never carried. A background task is needed up for the steps
/// that do run, an after action is the clean-up this run owes, and a recovery
/// node is only reached through a failure. A step a condition skipped isn't
/// carried either: it asks its condition again, which costs nothing.
pub fn carried(steps: &[RunStep], prior: &[PriorStep]) -> HashSet<String> {
    let before: HashMap<&str, &PriorStep> = prior.iter().map(|p| (p.name.as_str(), p)).collect();
    let passed = |p: &PriorStep| matches!(p.status.as_str(), "success" | REUSED);
    let satisfied = |p: &PriorStep| passed(p) || matches!(p.status.as_str(), "skipped" | "warning");

    let mut kept: HashSet<&str> = steps
        .iter()
        .filter(|s| !s.background && !s.after && !s.recover)
        .filter(|s| {
            before.get(s.name.as_str()).is_some_and(|p| {
                passed(p)
                    && p.shell == super::engine::shell_form(s.script.as_deref(), s.run.as_deref())
            })
        })
        .map(|s| s.name.as_str())
        .collect();

    // Until it settles: losing one step can take its dependents with it.
    loop {
        let losing: Vec<&str> = steps
            .iter()
            .filter(|s| kept.contains(s.name.as_str()))
            .filter(|s| {
                !s.needs.iter().all(|need| {
                    kept.contains(need.as_str())
                        || before
                            .get(need.as_str())
                            .is_some_and(|p| !passed(p) && satisfied(p))
                })
            })
            .map(|s| s.name.as_str())
            .collect();
        if losing.is_empty() {
            break;
        }
        for name in losing {
            kept.remove(name);
        }
    }

    kept.into_iter().map(str::to_string).collect()
}

/// The file a workflow's last run is recorded in. The label can hold `:` and
/// `/`, and has to make a file name on every platform.
fn path_for(root: &Path, name: &str) -> PathBuf {
    let file: String = name
        .chars()
        .take(100)
        .map(
            |c| match c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                true => c,
                false => '_',
            },
        )
        .collect();
    root.join(CIABATTA_DIR)
        .join(DIR)
        .join(format!("{file}.json"))
}

/// Write down how a run went, for a later `--from-failed`.
pub fn save(root: &Path, name: &str, outcome: &str, steps: Vec<PriorStep>) -> Result<()> {
    let path = path_for(root, name);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    let record = LastRun {
        outcome: outcome.to_string(),
        finished_at: chrono::Utc::now().to_rfc3339(),
        steps,
    };
    std::fs::write(&path, serde_json::to_vec_pretty(&record)?)
        .with_context(|| format!("writing {}", path.display()))
}

/// What a `--from-failed` run carries on from: `None` when the last run passed,
/// and an error when there has been no run to carry on from.
pub fn from_last_run(root: &Path, name: &str) -> Result<Option<Resume>> {
    let path = path_for(root, name);
    let Ok(content) = std::fs::read(&path) else {
        bail!(
            "No earlier run of '{name}' is on record here, so there is nothing to re-run from. \
             Run it without --from-failed first."
        );
    };
    let record: LastRun =
        serde_json::from_slice(&content).with_context(|| format!("reading {}", path.display()))?;
    if record.outcome == "success" {
        return Ok(None);
    }
    Ok(Some(Resume {
        from: "the last run".to_string(),
        prior: record.steps,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn step(name: &str, needs: &[&str]) -> RunStep {
        RunStep {
            name: name.to_string(),
            run: Some(format!("run {name}")),
            needs: needs.iter().map(|n| n.to_string()).collect(),
            ..Default::default()
        }
    }

    fn was(name: &str, status: &str) -> PriorStep {
        PriorStep {
            name: name.to_string(),
            status: status.to_string(),
            shell: Some(format!("run {name}")),
        }
    }

    fn sorted(set: HashSet<String>) -> Vec<String> {
        let mut names: Vec<String> = set.into_iter().collect();
        names.sort();
        names
    }

    #[test]
    fn what_passed_is_carried_and_the_failure_and_what_follows_are_not() {
        let steps = [
            step("build", &[]),
            step("lint", &[]),
            step("test", &["build"]),
            step("deploy", &["test", "lint"]),
        ];
        let prior = [
            was("build", "success"),
            was("lint", REUSED),
            was("test", "failed"),
            was("deploy", "skipped"),
        ];
        assert_eq!(sorted(carried(&steps, &prior)), ["build", "lint"]);
    }

    #[test]
    fn a_step_a_condition_skipped_is_asked_again_but_does_not_block_what_passed() {
        let steps = [step("gen", &[]), step("build", &["gen"])];
        let prior = [was("gen", "skipped"), was("build", "success")];
        assert_eq!(sorted(carried(&steps, &prior)), ["build"]);
    }

    #[test]
    fn a_changed_command_runs_again_and_takes_its_dependents_with_it() {
        let steps = [
            step("a", &[]),
            step("b", &["a"]),
            step("c", &["b"]),
            step("d", &[]),
        ];
        let mut edited = was("a", "success");
        edited.shell = Some("the old command".to_string());
        let prior = [
            edited,
            was("b", "success"),
            was("c", "success"),
            was("d", "success"),
        ];
        assert_eq!(sorted(carried(&steps, &prior)), ["d"]);
    }

    #[test]
    fn a_step_added_since_runs_and_takes_its_dependents_with_it() {
        let steps = [step("new", &[]), step("b", &["new"])];
        let prior = [was("b", "success")];
        assert!(carried(&steps, &prior).is_empty());
    }

    #[test]
    fn background_after_and_recovery_steps_are_never_carried() {
        let steps = [
            RunStep {
                background: true,
                ..step("mock", &[])
            },
            RunStep {
                after: true,
                ..step("teardown", &[])
            },
            RunStep {
                recover: true,
                ..step("fix", &[])
            },
        ];
        let prior = [
            was("mock", "success"),
            was("teardown", "success"),
            was("fix", "success"),
        ];
        assert!(carried(&steps, &prior).is_empty());
    }

    #[test]
    fn the_last_run_is_read_back_and_a_pass_leaves_nothing_to_re_run() {
        let root = std::env::temp_dir().join(format!("ciabatta-resume-{}", std::process::id()));
        let name = "web:build+test";

        let missing = from_last_run(&root, name).unwrap_err();
        assert!(format!("{missing:#}").contains("nothing to re-run from"));

        save(&root, name, "failed", vec![was("a", "success")]).unwrap();
        let resume = from_last_run(&root, name).unwrap().unwrap();
        assert_eq!(resume.prior, vec![was("a", "success")]);

        save(&root, name, "success", vec![was("a", "success")]).unwrap();
        assert!(from_last_run(&root, name).unwrap().is_none());

        let _ = std::fs::remove_dir_all(&root);
    }
}
