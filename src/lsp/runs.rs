//! Run status, where the workflows are written.
//!
//! Someone editing `build.yaml` wants to know two things the file can't tell
//! them: whether the run they started from the web app is still going, and
//! whether this workflow worked last time. Both used to mean switching to a
//! terminal or a browser tab. The editor already has a place for each — a
//! progress indicator in its status bar, and diagnostics and hovers on the file
//! — and every editor that speaks LSP draws them, so the one server serves Zed
//! and VS Code alike.
//!
//! Two sources, because they answer different questions:
//!
//! * **The history file** (`.ciabatta/history/workflows.json`) records how each
//!   workflow's last run ended, wherever it was started from — a terminal, the
//!   web app, CI on this machine. It's a file, so reading it needs nothing
//!   running.
//! * **The daemon** knows what is running *now*, step by step. Asked only if
//!   one is up; without it there is simply no live progress to show.

use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use anyhow::Result;
use serde_json::{Value, json};

use super::rpc;
use crate::run::history::{History, Outcome, Record};

/// A run the editor is showing progress for.
struct Live {
    token: String,
    title: String,
    /// The last message sent, so an unchanged run isn't re-sent every tick.
    shown: String,
}

/// Watches the project's runs and tells the editor about them.
pub struct RunWatch {
    /// The project root, once the editor has said which folder it opened.
    root: Option<PathBuf>,
    /// Whether the editor draws `$/progress`; most do, and one that doesn't
    /// gets only the end-of-run message.
    progress: bool,
    /// The daemon's view of this project's runs as of the last tick.
    runs: Vec<Value>,
    live: HashMap<u64, Live>,
    /// When the history file last changed, so a run finishing (anywhere)
    /// is noticed and the open workflow files re-checked.
    history_mtime: Option<SystemTime>,
    requests: u64,
    runtime: Option<tokio::runtime::Runtime>,
}

impl Drop for RunWatch {
    /// `ciabatta lsp` runs inside the CLI's own runtime, and dropping a runtime
    /// from there panics — on every editor shutdown, after a clean `exit`.
    /// Shutting it down in the background is the form tokio allows there.
    fn drop(&mut self) {
        if let Some(runtime) = self.runtime.take() {
            runtime.shutdown_background();
        }
    }
}

impl RunWatch {
    pub fn new() -> Self {
        Self {
            root: None,
            progress: false,
            runs: Vec::new(),
            live: HashMap::new(),
            history_mtime: None,
            requests: 0,
            // One small runtime for the daemon client, built once: the server
            // is otherwise synchronous, and starting a runtime per poll would
            // cost more than the poll.
            runtime: tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .ok(),
        }
    }

    /// Take what the editor said in `initialize`: which folder it opened, and
    /// whether it can draw progress.
    pub fn initialize(&mut self, params: &Value) {
        self.progress = params["capabilities"]["window"]["workDoneProgress"]
            .as_bool()
            .unwrap_or(false);
        let folder = params["workspaceFolders"][0]["uri"]
            .as_str()
            .or_else(|| params["rootUri"].as_str())
            .and_then(rpc::uri_to_path);
        if let Some(folder) = folder {
            self.root = project_root(&folder);
        }
        self.history_mtime = self.root.as_deref().and_then(history_mtime);
    }

    /// Adopt the project a newly opened file belongs to, if none is known yet
    /// — an editor opened on a single file sends no folder.
    pub fn adopt(&mut self, file: &Path) {
        if self.root.is_none() {
            self.root = file.parent().and_then(project_root);
            self.history_mtime = self.root.as_deref().and_then(history_mtime);
        }
    }

    /// Look again. Returns whether a run has finished since the last look, in
    /// which case the open workflow files' status is out of date.
    pub fn tick(&mut self, output: &mut impl Write) -> Result<bool> {
        let Some(root) = self.root.clone() else {
            return Ok(false);
        };

        let mtime = history_mtime(&root);
        let mut finished = mtime != self.history_mtime;
        self.history_mtime = mtime;

        self.runs = self.fetch(&root).unwrap_or_default();
        let running: Vec<&Value> = self.runs.iter().filter(|r| r["done"] == false).collect();
        let current: HashSet<u64> = running.iter().filter_map(|r| r["id"].as_u64()).collect();

        for run in &running {
            let Some(id) = run["id"].as_u64() else {
                continue;
            };
            let message = live_message(run);
            if let Some(live) = self.live.get_mut(&id) {
                if live.shown != message {
                    live.shown = message.clone();
                    if self.progress {
                        rpc::notify(
                            output,
                            "$/progress",
                            json!({ "token": live.token, "value": {
                                "kind": "report",
                                "message": message,
                                "percentage": percentage(run),
                            }}),
                        )?;
                    }
                }
                continue;
            }

            let title = format!("ciabatta {}", workflows(run));
            let token = format!("ciabatta-run-{id}");
            if self.progress {
                self.requests += 1;
                rpc::request(
                    output,
                    &json!(format!("ciabatta-{}", self.requests)),
                    "window/workDoneProgress/create",
                    json!({ "token": token }),
                )?;
                rpc::notify(
                    output,
                    "$/progress",
                    json!({ "token": token, "value": {
                        "kind": "begin",
                        "title": title,
                        "message": message,
                        "percentage": percentage(run),
                        "cancellable": false,
                    }}),
                )?;
            }
            self.live.insert(
                id,
                Live {
                    token,
                    title,
                    shown: message,
                },
            );
        }

        // Runs that were live and no longer are: say how they ended.
        let ended: Vec<u64> = self
            .live
            .keys()
            .filter(|id| !current.contains(id))
            .copied()
            .collect();
        for id in ended {
            let live = self.live.remove(&id).expect("listed above");
            let run = self.runs.iter().find(|r| r["id"].as_u64() == Some(id));
            let status = run.and_then(|r| r["status"].as_str()).unwrap_or("finished");
            let took = run
                .and_then(took)
                .map(|t| format!(" in {t}"))
                .unwrap_or_default();
            let summary = format!("{} {status}{took}", live.title);
            if self.progress {
                rpc::notify(
                    output,
                    "$/progress",
                    json!({ "token": live.token, "value": { "kind": "end", "message": summary }}),
                )?;
            }
            // A failure is worth interrupting for — the run was started
            // somewhere else, and this may be the first anyone hears of it. A
            // success isn't: the status bar already said so on its way out.
            if status == "failed" {
                let link = crate::daemon::read_record()
                    .map(|d| format!(" — http://127.0.0.1:{}/run/{id}", d.port))
                    .unwrap_or_default();
                rpc::notify(
                    output,
                    "window/showMessage",
                    json!({ "type": 1, "message": format!("{summary} (run #{id}){link}") }),
                )?;
            }
            finished = true;
        }

        Ok(finished)
    }

    /// This project's runs, from the daemon, if one is up. A daemon that is
    /// down or slow is the same as no runs: the editor carries on regardless.
    fn fetch(&self, root: &Path) -> Option<Vec<Value>> {
        let runtime = self.runtime.as_ref()?;
        let daemon = crate::daemon::read_record()?;
        let project = crate::daemon::projects::project_id(root);
        let here = root.display().to_string();
        let url = format!("http://127.0.0.1:{}/api/run/runs", daemon.port);
        // On a thread of its own: `ciabatta lsp` is started from inside the
        // CLI's runtime, and blocking on another runtime from a thread that is
        // already driving one panics.
        let runs: Vec<Value> = std::thread::scope(|scope| {
            scope
                .spawn(|| {
                    runtime.block_on(async {
                        reqwest::Client::new()
                            .get(&url)
                            .bearer_auth(&daemon.token)
                            .timeout(Duration::from_millis(800))
                            .send()
                            .await
                            .ok()?
                            .json()
                            .await
                            .ok()
                    })
                })
                .join()
                .ok()
                .flatten()
        })?;
        Some(
            runs.into_iter()
                // By id, or by the root the run actually ran in: a project
                // registered from a sub-directory has an id of its own, but its
                // runs still run here.
                .filter(|r| {
                    r["project"].as_str() == Some(project.as_str())
                        || r["root"].as_str() == Some(here.as_str())
                })
                .collect(),
        )
    }

    /// The run of `workflow` in flight right now, if there is one.
    fn running(&self, workflow: &str) -> Option<&Value> {
        self.runs.iter().find(|r| {
            r["done"] == false
                && r["workflows"].as_array().is_some_and(|names| {
                    names
                        .iter()
                        .filter_map(Value::as_str)
                        .any(|name| name == workflow || name.rsplit(':').next() == Some(workflow))
                })
        })
    }

    /// What the editor should say about a workflow, for its hover: what is
    /// happening now, and how it went last time.
    pub fn hover(&self, member: Option<&str>, workflow: &str) -> Option<String> {
        let root = self.root.as_deref()?;
        let mut parts = Vec::new();
        if let Some(run) = self.running(workflow) {
            parts.push(format!(
                "**Running now** (run #{}) — {}",
                run["id"],
                live_message(run)
            ));
        }
        match last(root, member, workflow) {
            Some(record) => parts.push(describe(&record)),
            None => parts.push(format!(
                "`{workflow}` hasn't been run in this checkout yet."
            )),
        }
        Some(parts.join("\n\n"))
    }

    /// A diagnostic for a workflow whose last run failed, so it shows in the
    /// editor's problems list beside the file it's about. Nothing for one that
    /// passed: a warning that says "this is fine" teaches people to ignore
    /// warnings.
    pub fn diagnostic(&self, member: Option<&str>, workflow: &str) -> Option<Value> {
        let record = last(self.root.as_deref()?, member, workflow)?;
        if record.last_outcome != Outcome::Failed {
            return None;
        }
        Some(json!({
            "range": {
                "start": { "line": 0, "character": 0 },
                "end": { "line": 0, "character": 0 },
            },
            "severity": 2,
            "source": "ciabatta",
            "message": describe_plain(&record),
        }))
    }
}

/// The project a path belongs to, found the way a run finds it — which is
/// where the run writes its history.
fn project_root(path: &Path) -> Option<PathBuf> {
    crate::workspace::find_workspace_root(path)
}

fn history_mtime(root: &Path) -> Option<SystemTime> {
    std::fs::metadata(
        root.join(crate::config::CIABATTA_DIR)
            .join("history")
            .join("workflows.json"),
    )
    .and_then(|m| m.modified())
    .ok()
}

/// The history record for a workflow: the member's own, or — in a project
/// with no members — the root's, which history files under `.`.
fn last(root: &Path, member: Option<&str>, workflow: &str) -> Option<Record> {
    let history = History::load(root);
    member
        .and_then(|m| history.get(m, workflow))
        .or_else(|| history.get(".", workflow))
        .cloned()
}

/// "2/7 steps · compile" — how far a live run is, in a status bar's width.
fn live_message(run: &Value) -> String {
    let progress = &run["progress"];
    let (Some(done), Some(total)) = (progress["done"].as_u64(), progress["total"].as_u64()) else {
        // A daemon from before it reported progress.
        return "running".to_string();
    };
    let running: Vec<&str> = progress["running"]
        .as_array()
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    match running.as_slice() {
        [] => format!("{done}/{total} steps"),
        [one] => format!("{done}/{total} steps · {one}"),
        [one, rest @ ..] => format!("{done}/{total} steps · {one} +{}", rest.len()),
    }
}

fn percentage(run: &Value) -> Option<u64> {
    let done = run["progress"]["done"].as_u64()?;
    let total = run["progress"]["total"].as_u64().filter(|t| *t > 0)?;
    Some(done * 100 / total)
}

fn workflows(run: &Value) -> String {
    run["workflows"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default()
}

/// How long a finished run took, from its two timestamps.
fn took(run: &Value) -> Option<String> {
    let parse = |key: &str| chrono::DateTime::parse_from_rfc3339(run[key].as_str()?).ok();
    let ms = (parse("finished_at")? - parse("started_at")?).num_milliseconds();
    Some(crate::runner::elapsed(Duration::from_millis(
        ms.max(0) as u64
    )))
}

/// A record as one sentence, for a diagnostic.
fn describe_plain(record: &Record) -> String {
    let mut text = format!(
        "Last run {} {}, after {}",
        record.last_outcome.label(),
        ago(&record.last_run_at),
        crate::runner::elapsed(Duration::from_millis(record.last_duration_ms)),
    );
    if record.failures > 0 {
        text.push_str(&format!(
            " — {} of {} runs here have failed",
            record.failures, record.runs
        ));
    }
    text
}

/// A record as Markdown, for a hover.
fn describe(record: &Record) -> String {
    let mark = match record.last_outcome {
        Outcome::Success => "✓",
        Outcome::Failed => "✗",
        Outcome::Stopped => "■",
    };
    format!(
        "{mark} **Last run {}** {} · took {}\n\n{} run{} in this checkout, {} failed",
        record.last_outcome.label(),
        ago(&record.last_run_at),
        crate::runner::elapsed(Duration::from_millis(record.last_duration_ms)),
        record.runs,
        if record.runs == 1 { "" } else { "s" },
        record.failures,
    )
}

/// "3 minutes ago" — the age of a timestamp, at the precision people use.
fn ago(at: &str) -> String {
    let Ok(then) = chrono::DateTime::parse_from_rfc3339(at) else {
        return "at an unknown time".to_string();
    };
    let seconds = (chrono::Utc::now() - then.with_timezone(&chrono::Utc)).num_seconds();
    let (n, unit) = match seconds {
        s if s < 60 => return "just now".to_string(),
        s if s < 3_600 => (s / 60, "minute"),
        s if s < 86_400 => (s / 3_600, "hour"),
        s => (s / 86_400, "day"),
    };
    format!("{n} {unit}{} ago", if n == 1 { "" } else { "s" })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_live_run_says_how_far_along_it_is() {
        let run = json!({ "progress": { "done": 2, "total": 7, "running": ["api:compile"] } });
        assert_eq!(live_message(&run), "2/7 steps · api:compile");
        assert_eq!(percentage(&run), Some(28));

        let parallel = json!({ "progress": { "done": 0, "total": 4, "running": ["a", "b", "c"] } });
        assert_eq!(live_message(&parallel), "0/4 steps · a +2");

        // An older daemon that doesn't report progress still says something.
        assert_eq!(live_message(&json!({})), "running");
        assert_eq!(percentage(&json!({})), None);
    }

    #[test]
    fn a_finished_run_says_how_long_it_took() {
        let run = json!({
            "started_at": "2026-10-01T10:00:00-04:00",
            "finished_at": "2026-10-01T10:01:05-04:00",
        });
        assert_eq!(took(&run).as_deref(), Some("1m05s"));
        assert_eq!(took(&json!({ "started_at": "2026-10-01T10:00:00Z" })), None);
    }

    #[test]
    fn ages_read_the_way_people_say_them() {
        let at = |secs: i64| (chrono::Utc::now() - chrono::Duration::seconds(secs)).to_rfc3339();
        assert_eq!(ago(&at(5)), "just now");
        assert_eq!(ago(&at(60)), "1 minute ago");
        assert_eq!(ago(&at(7_200)), "2 hours ago");
        assert_eq!(ago(&at(3 * 86_400)), "3 days ago");
        assert_eq!(ago("not a time"), "at an unknown time");
    }

    #[test]
    fn a_failed_workflow_gets_a_diagnostic_and_a_passing_one_does_not() {
        let root = std::env::temp_dir().join(format!("ciabatta-lsp-runs-{}", std::process::id()));
        std::fs::create_dir_all(root.join(".ciabatta")).unwrap();
        std::fs::write(root.join(".ciabatta/ciabatta.yaml"), "workspace: {}\n").unwrap();
        let mut history = History::load(&root);
        history.record("api", "build", Outcome::Failed, 1_500);
        history.record("api", "test", Outcome::Success, 900);
        history.save(&root).unwrap();

        let mut watch = RunWatch::new();
        watch.adopt(&root.join(".ciabatta").join("ciabatta.yaml"));

        let warning = watch
            .diagnostic(Some("api"), "build")
            .expect("a failure is reported");
        assert_eq!(warning["severity"], 2);
        assert!(
            warning["message"]
                .as_str()
                .unwrap()
                .contains("Last run failed")
        );
        assert!(watch.diagnostic(Some("api"), "test").is_none());

        assert!(
            watch
                .hover(Some("api"), "test")
                .unwrap()
                .contains("Last run success")
        );
        assert!(
            watch
                .hover(Some("api"), "deploy")
                .unwrap()
                .contains("hasn't been run")
        );

        std::fs::remove_dir_all(&root).ok();
    }
}
