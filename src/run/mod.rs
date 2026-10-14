//! The `run` paradigm: a workflow direction whose main stage runs a DAG of
//! dependent script "steps", with `on_error` branches to recovery nodes that
//! offer a choice of fix scripts.
//!
//! This module owns the run config types (referenced from [`crate::config`]),
//! the loader that resolves a workflow's step DAG from a separate flowchart file,
//! the validation of that DAG, and the async engine that executes it. The live
//! web view and the visual builder are served by the daemon, in
//! [`crate::daemon::routes::run`].

pub mod cached;
pub mod deps;
pub mod engine;
pub mod envdeps;
pub mod filter;
pub mod history;
pub mod isolate;
pub mod release;
pub mod transfer;
pub mod view;
pub mod why;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;

pub use engine::execute;

/// Deserialize a field that TOML may express as either a bare string
/// (`env_file = ".env"`) or an array (`env_file = [".env", ".env.run"]`) into
/// a `Vec<String>`. Shared by `env_file` and the step conditions (`when` /
/// `skip_if`), all of which accept one-or-many.
pub(crate) fn string_or_vec<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum OneOrMany {
        One(String),
        Many(Vec<String>),
    }
    Ok(match OneOrMany::deserialize(deserializer)? {
        OneOrMany::One(s) => vec![s],
        OneOrMany::Many(v) => v,
    })
}

/// Refuse `env:` on a step. Removed in 0.6.4: a workflow file names the
/// variables it uses and never sets their values, so a step-level table of
/// values has no place in one. An empty table reads, as nothing.
fn no_step_env<'de, D>(
    deserializer: D,
) -> std::result::Result<std::collections::BTreeMap<String, String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    let names: Vec<String> = match &value {
        serde_json::Value::Null => Vec::new(),
        serde_json::Value::Object(map) => map.keys().cloned().collect(),
        serde_json::Value::Array(items) => items
            .iter()
            .map(|v| {
                v.as_str()
                    .map(str::to_string)
                    .unwrap_or_else(|| v.to_string())
            })
            .collect(),
        other => vec![other.to_string()],
    };
    if names.is_empty() {
        return Ok(std::collections::BTreeMap::new());
    }
    let lines: Vec<String> = match &value {
        serde_json::Value::Object(map) => map
            .iter()
            .map(|(k, v)| match v {
                serde_json::Value::String(s) => format!("{k}={s}"),
                other => format!("{k}={other}"),
            })
            .collect(),
        _ => names.iter().map(|n| format!("{n}=…")).collect(),
    };
    Err(serde::de::Error::custom(format!(
        "a step can't set environment variables (`env:` on a step was removed in 0.6.4) — \
         put the values in the workspace's .env (or the .ciabatta/.env.<profile> it is \
         copied from):\n    {}\nand name them in the workflow's list: `env: [{}]`",
        lines.join("\n    "),
        names.join(", ")
    )))
}

/// A node in the run flowchart: either a normal step (runs an action once its
/// `needs` succeed) or a recovery node (`recover = true`, entered only via some
/// step's `on_error`, offering a choice of fix `options`).
#[derive(Debug, Deserialize, Serialize, Clone, Default)]
pub struct RunStep {
    /// Unique node name; the target of `needs` / `on_error` / `retry` edges.
    pub name: String,

    /// A bash script to run (path relative to the step's working directory).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub script: Option<String>,
    /// An inline shell command (`sh -c`), as an alternative to `script`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run: Option<String>,

    // ─── Documentation & ownership ──────────────────────────────────────────
    // Every script in a monorepo should say what it does and who to ask about
    // it; `ciabatta list` prints both, so nobody has to open the file to find
    // out. `ciabatta init --lib` scaffolds them and nags when they're missing.
    /// One line on what this step does, and what it expects to be true first.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Who owns this step — a name, handle, or team.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    /// Free-form labels — `fast`, `slow`, `integration`, `frontend`. A compiled
    /// workflow graph folds its sub-workspace's and workflow's tags in here, so
    /// every node carries the full set it inherited.
    ///
    /// These are what `--filter tag:fast` selects on, which is how you run one
    /// slice of a graph without running all of it.
    #[serde(default)]
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,

    // ─── Requirements ───────────────────────────────────────────────────────
    /// Executables that must be on `PATH` for this step to work (`cargo`,
    /// `protoc`, `docker`, …). Checked before the graph starts, so a missing
    /// toolchain is reported up front — with the fix from the matching
    /// `[toolchain.<tool>]` entry — instead of surfacing as "command not found"
    /// halfway through a build.
    #[serde(default)]
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub requires: Vec<String>,

    // ─── Long-running & flaky steps ─────────────────────────────────────────
    /// Wall-clock limit for this step: a duration (`"90s"`, `"10m"`, `"1h30m"`)
    /// or a bare number of seconds. When it expires the step is killed and
    /// marked timed-out, and — since a hung step must not hold up everything
    /// else — the rest of the graph carries on (see `continue_on_error`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout: Option<String>,
    /// Extra attempts to make when the step fails, for transient errors
    /// (default 0: one attempt, no retry).
    #[serde(default)]
    #[serde(skip_serializing_if = "crate::format::is_zero")]
    pub retries: u32,
    /// A step that never exits on its own — a dev server, a log tailer, a
    /// watcher. It is started, its dependents are released immediately, and it
    /// keeps running for the rest of the graph rather than hanging it.
    ///
    /// It **outlives the run**: the daemon takes ownership as a watch session,
    /// so `ciabatta dev` leaves you a dev server to work against rather than
    /// killing it on the way out. Stop it with `ciabatta watch --stop <id>`.
    /// For a server that exists only to get this run through and should go away
    /// with it, use the workflow's [`background`] array instead.
    ///
    /// [`background`]: crate::workspace::Workflow::background
    #[serde(default)]
    #[serde(skip_serializing_if = "crate::format::is_false")]
    pub persistent: bool,
    /// Set by the compiler on entries of a workflow's `background:` array.
    ///
    /// `#[serde(skip)]` on purpose: it is not a key anybody writes on a step,
    /// and accepting one there would give two spellings for the same idea with
    /// different meanings — a step is in the order, a background task is not.
    /// The API surfaces build their own JSON, so the flag still reaches the web
    /// app without living in the config format.
    ///
    /// A background task is a persistent one scoped to a single run: it is
    /// started before the first wave, gates nothing, and is stopped again once
    /// every stage has succeeded or failed. The distinction from
    /// [`Self::persistent`] is only ever *lifetime* — a mock API that exists to
    /// get this run through should not still be holding its port afterwards,
    /// and a dev server should.
    #[serde(skip)]
    pub background: bool,
    /// Set by the compiler from the workflow's `required_env`: the names this
    /// step's run refuses to start without, which also key its cache entry.
    /// `#[serde(skip)]` for the same reason as `background` — it's declared on
    /// the workflow, not the step.
    #[serde(skip)]
    pub required_env: Vec<String>,
    /// Set by the compiler from the workflow's `env` list: the variables this
    /// step's workflow says it uses. Their values come from the run's own
    /// environment — the terminal, then the `.env` search, which a profile
    /// decides — and every one of them keys the step's cache entry.
    /// `#[serde(skip)]` for the same reason as `required_env`.
    #[serde(skip)]
    pub declared_env: Vec<String>,
    /// What the cache knows this step as: `<member>:<workflow>:<step>`, set by
    /// the workflow compiler.
    ///
    /// The graph's own node id can't be used for that. It is shortened to
    /// `<member>:<step>` when a member contributes only one workflow to the
    /// graph, so the same target compiled into two different workflows' graphs
    /// would get two different names — and two different cache keys, and
    /// rebuild in each. `None` outside a workflow graph, where the step's name
    /// is already all there is.
    #[serde(skip)]
    pub cache_id: Option<String>,
    /// Don't fail the whole run when this step fails: its dependents are
    /// skipped, every other branch carries on, and the run reports the failure
    /// at the end. Always applied when a `timeout` expires.
    #[serde(default)]
    #[serde(skip_serializing_if = "crate::format::is_false")]
    pub continue_on_error: bool,
    /// Treat this step failing as a warning: the failure is reported, but the
    /// step counts as done — its dependents run, and the run itself still
    /// succeeds. For a stage whose result is worth knowing about but not worth
    /// stopping for: a linter being phased in, an optional upload, a report.
    ///
    /// The difference from [`Self::continue_on_error`] is what happens next:
    /// that one stops this branch and fails the run at the end, this one does
    /// neither.
    #[serde(default)]
    #[serde(skip_serializing_if = "crate::format::is_false")]
    pub warn_on_error: bool,

    // ─── Phase & placement ──────────────────────────────────────────────────
    /// The special, identifiable phase this step belongs to: `setup`, `build`,
    /// `test`, `deploy`, … Free-form and cosmetic (the graph labels the node
    /// with it) except for `push` and `pull`, which select the built-in
    /// registry action — see the transfer fields below.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,

    // ─── Transfer (kind = "push" / "pull") ──────────────────────────────────
    // Publishing an artifact is a step like any other: it sits on the graph,
    // declares what it needs, and is reported and cached the same way. These
    // fields describe *what* moves and *where*; `kind` decides which direction.
    /// The `registries:` entry this step transfers through.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub registry: Option<String>,
    /// The local file or directory to publish (or the destination a pull writes
    /// to), relative to the step's working directory. A directory is uploaded
    /// file by file, preserving its structure under the publish path.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artifact: Option<String>,
    /// Docker/ECR only: a local image reference (`name` or `name:tag`) to push.
    /// It is retagged to the registry's target reference before pushing, and a
    /// pull retags what it fetched back to this name. With this set,
    /// `publish_path` is the remote image reference; without it the local
    /// reference is reused verbatim.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub local_image: Option<String>,
    /// Destination path in the registry, supporting `{CIABATTA_*}` variable
    /// substitution — or a list of local globs, each uploaded under
    /// `{CIABATTA_PATH}` keeping its relative path (see [`PublishPath`]).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub publish_path: Option<crate::config::PublishPath>,
    /// For the glob-list form of `publish_path`: a leading path fragment
    /// stripped from each matched file before it is joined under
    /// `{CIABATTA_PATH}`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub strip_prefix: Option<String>,
    /// For a `kind = "pull"` step: the push step it mirrors, as
    /// `<workflow>:<step>` or a bare `<step>` in the same workflow.
    ///
    /// Push and pull are the same artifact in opposite directions, and stating
    /// it twice is how the two drift apart. The named step's transfer fields
    /// are copied in when the graph is compiled; anything this step sets for
    /// itself wins over what it inherits.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    /// Working directory for this step's action, relative to the project root.
    /// Workflow graphs set it to the owning sub-workspace's directory, so its
    /// scripts run where they were written to run. Defaults to the root.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// Which sub-workspace this step came from. Filled in when a workspace
    /// workflow graph is compiled; it's what labels each node on the graph.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
    /// Values layered over the run's environment for this step — filled in by
    /// the workflow compiler from a sub-workspace's deprecated `workspace.env`
    /// table, so two members' settings can't collide in one shared map.
    ///
    /// Not something a step can write. A workflow file names variables (its
    /// `env:` list) and never sets them: their values come from the `.env`,
    /// and so from the elected env profile. A step `env:` is refused by
    /// [`no_step_env`] with the place its values belong.
    #[serde(default, deserialize_with = "no_step_env")]
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub env: std::collections::BTreeMap<String, String>,

    /// The env files this step's workflow names (`env_file:` on the
    /// workflow), as paths relative to the run root.
    ///
    /// Searched for the whole run, ahead of the workspaces' own `.env` files —
    /// see [`prepare_env`]. Empty for a workflow that names none.
    #[serde(default)]
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub env_files: Vec<String>,

    /// Names of steps that must succeed before this one runs (the success DAG).
    #[serde(default)]
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub needs: Vec<String>,
    /// On failure, jump to this recovery node instead of aborting the run.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub on_error: Option<String>,

    /// Condition(s) that must ALL hold for this step to run; if any is false the
    /// step is skipped (and treated as satisfied, so its dependents still run).
    /// Accepts one condition or a list. Each is evaluated against the run's
    /// environment, e.g. `when = "env.RUN_ENV == prod"` or
    /// `when = ["env.RUN_ENV == prod", "REGION == us-east-1"]`.
    #[serde(default, deserialize_with = "string_or_vec")]
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub when: Vec<String>,
    /// Condition(s) that skip this step when ANY holds — the inverse of `when`,
    /// matching "skip if …". Accepts one condition or a list, e.g.
    /// `skip_if = "env.IN_CI == true"`.
    #[serde(default, deserialize_with = "string_or_vec")]
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub skip_if: Vec<String>,

    /// Marks this node as a recovery node: it presents `options` rather than
    /// running an action of its own.
    #[serde(default)]
    #[serde(skip_serializing_if = "crate::format::is_false")]
    pub recover: bool,
    /// Prompt shown when a recovery node is reached.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// After a chosen fix succeeds, re-run this node (typically the failed step).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry: Option<String>,
    /// The fix choices offered by a recovery node.
    #[serde(default)]
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<FixOption>,

    /// Cache settings for this step alone, overriding the workflow's and the
    /// workspace's. For the step that reads something none of its neighbours
    /// do — most steps inherit and never write this.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache: Option<crate::cache::CacheConfig>,

    /// For a `kind: release-push` / `kind: release-pull` step: which release,
    /// which version, and which files. See [`release`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub release: Option<release::ReleaseSpec>,
}

/// One fix choice on a recovery node.
#[derive(Debug, Deserialize, Serialize, Clone, Default)]
pub struct FixOption {
    /// Human-readable label shown in the UI / GUI.
    pub label: String,
    /// A bash script to run as the fix (path relative to the project root).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub script: Option<String>,
    /// An inline shell command, as an alternative to `script`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run: Option<String>,
    /// Run this option automatically in non-interactive mode (plain / CI), where
    /// no operator is present to choose. The first `default` option wins.
    #[serde(default)]
    #[serde(skip_serializing_if = "crate::format::is_false")]
    pub default: bool,
}

/// Which direction a `kind = "push"` / `kind = "pull"` step moves an artifact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// Publish the local artifact to the registry.
    Push,
    /// Fetch the artifact from the registry into the working tree.
    Pull,
}

impl Direction {
    /// The word this direction is spelled with, in config and in output.
    pub fn label(self) -> &'static str {
        match self {
            Direction::Push => "push",
            Direction::Pull => "pull",
        }
    }
}

/// A step's resolved registry transfer: what moves, where, and which way.
///
/// Produced once the graph is compiled, so nothing downstream has to re-read a
/// `from:` back-reference or wonder whether a step is a transfer.
#[derive(Debug, Clone, PartialEq)]
pub struct Transfer<'a> {
    pub direction: Direction,
    pub registry: Option<&'a str>,
    pub artifact: Option<&'a str>,
    pub local_image: Option<&'a str>,
    pub publish_path: Option<&'a crate::config::PublishPath>,
    pub strip_prefix: Option<&'a str>,
}

impl RunStep {
    /// The name the cache keys this step under — see [`Self::cache_id`].
    pub fn cache_name(&self) -> &str {
        self.cache_id.as_deref().unwrap_or(&self.name)
    }

    /// Whether this node runs an action of its own, as opposed to a pure
    /// recovery node that only presents options. A transfer step counts: its
    /// action is the built-in registry move.
    pub fn has_action(&self) -> bool {
        self.script.is_some()
            || self.run.is_some()
            || self.direction().is_some()
            || self.release_direction().is_some()
    }

    /// Whether this is a `kind: release-push` or `kind: release-pull` step,
    /// and which.
    pub fn release_direction(&self) -> Option<release::Direction> {
        release::Direction::of_kind(self.kind.as_deref()?)
    }

    /// The transfer direction this step's `kind` selects, or `None` for an
    /// ordinary step.
    ///
    /// Only `kind` decides. A step carrying `registry:` but no transfer `kind`
    /// is a plain step that happens to name a registry — silently promoting it
    /// would make `kind` untrustworthy as the thing that says what a node does.
    pub fn direction(&self) -> Option<Direction> {
        match self.kind.as_deref()? {
            k if k.eq_ignore_ascii_case("push") => Some(Direction::Push),
            k if k.eq_ignore_ascii_case("pull") => Some(Direction::Pull),
            _ => None,
        }
    }

    /// This step's transfer, when it has one.
    pub fn transfer(&self) -> Option<Transfer<'_>> {
        Some(Transfer {
            direction: self.direction()?,
            registry: self.registry.as_deref(),
            artifact: self.artifact.as_deref(),
            local_image: self.local_image.as_deref(),
            publish_path: self.publish_path.as_ref(),
            strip_prefix: self.strip_prefix.as_deref(),
        })
    }

    /// Whether this step publishes — the identifiable "push" phase of a
    /// workflow.
    pub fn is_push(&self) -> bool {
        self.direction() == Some(Direction::Push)
    }

    /// Copy the transfer fields of the step this one mirrors (`from:`) into any
    /// it hasn't set for itself.
    ///
    /// What the step states directly always wins: `from` is a default, not an
    /// override, so a pull that differs in one field says only that field.
    pub fn inherit_transfer(&mut self, source: &RunStep) {
        if self.registry.is_none() {
            self.registry = source.registry.clone();
        }
        if self.artifact.is_none() {
            self.artifact = source.artifact.clone();
        }
        if self.local_image.is_none() {
            self.local_image = source.local_image.clone();
        }
        if self.publish_path.is_none() {
            self.publish_path = source.publish_path.clone();
        }
        if self.strip_prefix.is_none() {
            self.strip_prefix = source.strip_prefix.clone();
        }
    }

    /// This step's configured wall-clock limit, if any.
    pub fn timeout_duration(&self) -> Result<Option<std::time::Duration>> {
        match self.timeout.as_deref() {
            None => Ok(None),
            // One flat message rather than a context chain: this surfaces
            // through `validate_flowchart`, whose callers print only the
            // top-level error.
            Some(raw) => parse_duration(raw).map(Some).map_err(|err| {
                anyhow::anyhow!("Step '{}' has an invalid timeout: {err}", self.name)
            }),
        }
    }

    /// The shell action this step runs, or `None` when it has none of its own —
    /// which for a transfer step means the engine performs the built-in move.
    /// Returns `(script, run)`, mirroring the pair the executor takes.
    pub fn action(&self) -> (Option<&str>, Option<String>) {
        (
            self.script.as_deref(),
            self.run.as_deref().map(str::to_string),
        )
    }
}

/// Parse a step `timeout` into a [`Duration`](std::time::Duration).
///
/// Accepts a bare number of seconds (`"90"`, or `timeout = 90`), or a sequence
/// of unit-suffixed parts: `h`/`hour(s)`, `m`/`min(s)`/`minute(s)`,
/// `s`/`sec(s)`/`second(s)`, `ms`. Parts combine, so `"1h30m"` and
/// `"90m"` are the same limit.
pub fn parse_duration(raw: &str) -> Result<std::time::Duration> {
    let text = raw.trim().to_ascii_lowercase();
    if text.is_empty() {
        bail!("empty duration");
    }
    // Bare number → seconds, the form `timeout = 600` deserializes into.
    if let Ok(secs) = text.parse::<f64>() {
        if secs < 0.0 {
            bail!("negative duration '{raw}'");
        }
        return Ok(std::time::Duration::from_secs_f64(secs));
    }

    let mut total = std::time::Duration::ZERO;
    let mut number = String::new();
    let mut unit = String::new();
    let mut parts = 0usize;

    // Walk the string folding each "<number><unit>" pair into the total.
    let mut flush = |number: &mut String, unit: &mut String, total: &mut std::time::Duration| {
        if number.is_empty() {
            return Ok(());
        }
        let value: f64 = number
            .parse()
            .map_err(|_| anyhow::anyhow!("invalid number '{number}' in duration '{raw}'"))?;
        let seconds = match unit.as_str() {
            "ms" => value / 1000.0,
            "s" | "sec" | "secs" | "second" | "seconds" => value,
            "m" | "min" | "mins" | "minute" | "minutes" => value * 60.0,
            "h" | "hr" | "hrs" | "hour" | "hours" => value * 3600.0,
            other => bail!("unknown duration unit '{other}' in '{raw}'"),
        };
        *total += std::time::Duration::from_secs_f64(seconds);
        number.clear();
        unit.clear();
        parts += 1;
        Ok(())
    };

    for ch in text.chars() {
        if ch.is_ascii_digit() || ch == '.' {
            // A digit after a unit closes the previous part (`1h30m`).
            if !unit.is_empty() {
                flush(&mut number, &mut unit, &mut total)?;
            }
            number.push(ch);
        } else if ch.is_ascii_alphabetic() {
            unit.push(ch);
        } else if ch.is_whitespace() {
            continue;
        } else {
            bail!("unexpected character '{ch}' in duration '{raw}'");
        }
    }
    flush(&mut number, &mut unit, &mut total)?;

    if parts == 0 {
        bail!("could not parse duration '{raw}' (try \"30s\", \"10m\", or \"1h30m\")");
    }
    Ok(total)
}

/// A fully resolved run, ready to execute: the phase hooks plus the validated
/// step DAG.
#[derive(Debug, Clone, Default)]
pub struct ResolvedRun {
    pub login: Option<String>,
    pub pre: Option<String>,
    pub post: Option<String>,
    /// Variables that must be set (non-empty) before the run may start.
    pub required_env: Vec<String>,
    /// `.env` file paths (relative to the project root) to source before the
    /// run starts, in the order they should be applied.
    pub env_files: Vec<String>,
    /// The workspaces the environment is searched through, nearest first —
    /// the one the run was started from, up to the root. Set by
    /// [`with_env_levels`].
    pub env_levels: Vec<EnvLevel>,
    pub steps: Vec<RunStep>,
    /// The `(sub-workspace, workflow)` units this run covers.
    ///
    /// A compiled graph spans several — `ciabatta build` in a monorepo runs
    /// four packages' `build` workflows as one DAG — and the steps themselves
    /// can't say which, because a step knows its sub-workspace but not the
    /// workflow it came from. Carried here so the run can write down what it
    /// actually ran; see [`history`].
    pub units: Vec<Unit>,
}

/// One `(sub-workspace, workflow)` pair a run covered.
#[derive(Debug, Deserialize, Serialize, Clone, Default, PartialEq, Eq)]
pub struct Unit {
    /// The sub-workspace's name, or `"."` for a project with no members.
    pub workspace: String,
    pub workflow: String,
}

impl ResolvedRun {
    /// Look up a step node by name.
    pub fn step(&self, name: &str) -> Option<&RunStep> {
        self.steps.iter().find(|s| s.name == name)
    }
}

/// Parse the contents of a `.env` file into ordered `KEY=VALUE` pairs.
///
/// Supports the common `.env` shape: blank lines and `#` comments are ignored,
/// an optional leading `export ` is stripped, and values may be wrapped in
/// single or double quotes (the quotes are removed). Values are otherwise taken
/// verbatim (leading/trailing whitespace trimmed for unquoted values). Lines
/// without an `=` are skipped.
pub fn parse_env_content(content: &str) -> Vec<(String, String)> {
    let mut pairs = Vec::new();
    for raw in content.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line);
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        if key.is_empty() {
            continue;
        }
        let value = value.trim();
        // Strip a single pair of matching surrounding quotes, if present.
        let value = if value.len() >= 2
            && ((value.starts_with('"') && value.ends_with('"'))
                || (value.starts_with('\'') && value.ends_with('\'')))
        {
            &value[1..value.len() - 1]
        } else {
            value
        };
        pairs.push((key.to_string(), value.to_string()));
    }
    pairs
}

/// Read the resolved `.env` files (relative to `root`) and merge their variables
/// on top of `base`, returning the combined environment for the run.
///
/// Precedence: values already present and non-empty in `base` (the ambient
/// environment, CI, git, or `-e` flags) win, so a `.env` only supplies what
/// isn't already set. Among the files themselves, later files override earlier
/// ones. A missing or unreadable file is an error.
pub fn load_env_files(
    files: &[String],
    root: &Path,
    base: &HashMap<String, String>,
) -> Result<HashMap<String, String>> {
    let mut merged = base.clone();
    for rel in files {
        let path = root.join(rel);
        let content = std::fs::read_to_string(&path)
            .with_context(|| format!("Failed to read env file '{}'", path.display()))?;
        for (key, value) in parse_env_content(&content) {
            // A value already resolved (and non-empty) in `base` wins over every
            // file; but a later file may override an earlier file's value.
            let pinned_by_base = base.get(&key).is_some_and(|v| !v.trim().is_empty());
            if !pinned_by_base {
                merged.insert(key, value);
            }
        }
    }
    Ok(merged)
}

/// One workspace's say in a run's environment: its `.env` (or `env_file`s)
/// and its defaults file. Paths are relative to the monorepo root.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EnvLevel {
    /// The workspace's directory, relative to the run root (`.` for the root):
    /// where its `.ciabatta/.env.<profile>` files are looked for.
    pub rel: String,
    /// The workspace's env files by name, whether or not they exist yet — a
    /// missing one is skipped when read.
    pub files: Vec<String>,
    /// The one a profile is copied into (the first `env_file`, or `.env`),
    /// and so the one its profile file stands in for.
    pub target: String,
    /// Its `env_default` — or a `.env.default` sitting beside it.
    pub default: Option<String>,
}

/// Give a run the workspaces its environment is searched through: the one it
/// was started from (`start`), then each enclosing one, up to the root.
///
/// Does nothing when the run already has them, so the CLI — which knows the
/// directory it was started in — decides, and the engine, the pre-run report
/// and the daemon can all call this on the same run and agree. `meta` is the
/// root's config, for a project that isn't a monorepo.
pub fn with_env_levels(
    mut resolved: ResolvedRun,
    root: &Path,
    start: &Path,
    meta: &crate::workspace::WorkspaceMeta,
) -> ResolvedRun {
    use crate::environment::files;

    if !resolved.env_levels.is_empty() {
        return resolved;
    }
    let level = |meta: &crate::workspace::WorkspaceMeta, dir: &Path, rel: &str| EnvLevel {
        rel: rel.to_string(),
        target: files::join_rel(rel, &files::target_file(meta)),
        files: match meta.env_file.is_empty() {
            true => vec![files::join_rel(rel, files::DEFAULT_ENV_FILE)],
            false => meta
                .env_file
                .iter()
                .map(|f| files::join_rel(rel, f))
                .collect(),
        },
        default: files::defaults_file(meta, dir).map(|f| files::join_rel(rel, &f)),
    };

    let Ok(workspace) = crate::workspace::Workspace::discover(root) else {
        resolved.env_levels = vec![level(meta, root, ".")];
        return resolved;
    };
    let start = start.canonicalize().unwrap_or_else(|_| start.to_path_buf());
    // Nearest first: every member whose directory holds `start`, deepest
    // first. The root is the last level whether or not it's a member.
    let mut enclosing: Vec<&crate::workspace::Member> = workspace
        .members
        .iter()
        .filter(|m| m.rel != ".")
        .filter(|m| start.starts_with(m.dir.canonicalize().unwrap_or_else(|_| m.dir.clone())))
        .collect();
    enclosing.sort_by_key(|m| std::cmp::Reverse(m.dir.components().count()));
    let mut levels: Vec<EnvLevel> = enclosing
        .into_iter()
        .map(|m| level(&m.meta, &m.dir, &m.rel))
        .collect();
    levels.push(level(&workspace.root_meta, &workspace.root, "."));
    resolved.env_levels = levels;
    resolved
}

/// What kind of file a value came from — which is why it was searched where
/// it was, and so why it won.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileKind {
    /// Named by a workflow (`env_file:`), searched before every workspace.
    Workflow,
    /// The elected profile's `.ciabatta/.env.<name>`, standing in for the
    /// `.env` it's copied into.
    Profile,
    /// A workspace's `.env` (or `env_file`).
    Env,
    /// A workspace's defaults file, searched after its `.env`.
    Default,
}

/// How a variable's value was decided: every file that offered one, so the
/// report can say what won and what it beat.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Source {
    /// The environment ciabatta was started with (the terminal, CI, git, or a
    /// `-e` flag) had a non-empty value. It always wins.
    pub terminal: bool,
    /// Every file that sets it, in search order — the first is the one that
    /// won, unless `terminal`.
    pub files: Vec<String>,
    /// What kind of file the winner is, when a file won.
    pub kind: Option<FileKind>,
}

/// A run's environment, resolved as far as the inputs allow, plus whatever it
/// still needs before it may start. Produced by [`prepare_env`].
#[derive(Debug, Clone, Default)]
pub struct PreparedEnv {
    /// What every step and phase of the run sees: one environment for the
    /// whole run. Only complete when [`is_ready`] is true; otherwise it's as
    /// far as resolution got.
    ///
    /// [`is_ready`]: PreparedEnv::is_ready
    pub env: HashMap<String, String>,
    /// Every file that exists in the search, in the order searched — the
    /// first to set a variable decides it.
    pub sourced: Vec<String>,
    /// How each variable any of those files mentions was decided.
    pub sources: HashMap<String, Source>,
    /// `{VAR}` placeholders in `env_file` paths with no value, so the file to
    /// source can't even be named yet.
    pub unresolved_paths: Vec<String>,
    /// `required_env` variables that are empty or unset.
    pub missing_required: Vec<String>,
    /// The build features this run was started with — every `CIABATTA_FEAT_*`
    /// the resolved environment carries. Reported at the start of the run, and
    /// folded into every cache key by [`crate::cache::plan`].
    pub features: crate::environment::features::Features,
}

impl PreparedEnv {
    /// Whether the run may start.
    pub fn is_ready(&self) -> bool {
        self.unresolved_paths.is_empty() && self.missing_required.is_empty()
    }

    /// Every variable a caller must supply before the run can start, in the
    /// order worth asking for them: the ones gating which `.env` file gets
    /// sourced first, since sourcing it may well satisfy the rest.
    pub fn missing(&self) -> Vec<String> {
        let mut all = self.unresolved_paths.clone();
        for var in &self.missing_required {
            if !all.contains(var) {
                all.push(var.clone());
            }
        }
        all
    }
}

/// Resolve a run's environment: substitute `{VAR}` placeholders in its
/// `env_file` paths, search the files for each variable, and check the result
/// against `required_env`.
///
/// **One environment for the whole run, decided by a search.** For each
/// variable, the first of these to set it wins:
///
/// 1. the environment ciabatta was started with — terminal, CI, git, `-e`;
/// 2. the env files the run's workflows name (`env_file:` on a workflow);
/// 3. each workspace from the one the run started in up to the root
///    ([`with_env_levels`]), and in each: its `.env` — or, with a profile
///    elected, the `.ciabatta/.env.<profile>` that's copied into it — then its
///    `.env.default`.
///
/// What none of them sets is unset; a `required_env` left unset stops the run.
///
/// Never runs anything, so both the engine (which does this for real, just
/// before the first phase) and the daemon's launcher (which does it to decide
/// whether to prompt for what's missing) can call it on the same inputs and
/// agree on the answer.
///
/// Unresolvable `env_file` paths on the run itself short-circuit: nothing can
/// be sourced until the path is known, and a file that *would* have been
/// sourced may be exactly what supplies the `required_env` variables — so
/// those aren't reported yet. A step's workflow file with an unset placeholder
/// is skipped.
pub fn prepare_env(
    resolved: &ResolvedRun,
    root: &Path,
    base: &HashMap<String, String>,
) -> Result<PreparedEnv> {
    use crate::config::{substitute_vars, unresolved_vars};

    let mut unresolved_paths: Vec<String> = Vec::new();
    let mut required: Vec<String> = Vec::with_capacity(resolved.env_files.len());
    for raw in &resolved.env_files {
        let unresolved = unresolved_vars(raw, base);
        if unresolved.is_empty() {
            required.push(substitute_vars(raw, base)?);
        } else {
            for var in unresolved {
                if !unresolved_paths.contains(&var) {
                    unresolved_paths.push(var);
                }
            }
        }
    }

    if !unresolved_paths.is_empty() {
        return Ok(PreparedEnv {
            env: base.clone(),
            unresolved_paths,
            ..Default::default()
        });
    }

    // The search, in order: (file, must it exist, what kind of file).
    let profile = crate::environment::profiles::active(base);
    let mut search: Vec<(String, bool, FileKind)> = Vec::new();
    let push = |search: &mut Vec<(String, bool, FileKind)>, file: String, needed, kind| {
        if !search.iter().any(|(f, _, _)| *f == file) {
            search.push((file, needed, kind));
        }
    };

    // The workflows' own files: the run's, which must exist, then each
    // step's, which are skipped when absent. Later in a list beats earlier.
    let mut workflow_files: Vec<(String, bool)> =
        required.iter().map(|f| (f.clone(), true)).collect();
    for step in &resolved.steps {
        for raw in &step.env_files {
            if unresolved_vars(raw, base).is_empty() {
                let path = substitute_vars(raw, base)?;
                if !workflow_files.iter().any(|(f, _)| *f == path) {
                    workflow_files.push((path, false));
                }
            }
        }
    }
    for (file, needed) in workflow_files.into_iter().rev() {
        push(&mut search, file, needed, FileKind::Workflow);
    }

    // Then each workspace, nearest first: its files (the last-named first),
    // then its defaults. With a profile elected, the workspace's
    // `.ciabatta/.env.<profile>` stands in for the `.env` it's copied into —
    // the same values once the copy is made, and the right ones in a dry run
    // that makes no copy.
    for level in &resolved.env_levels {
        for file in level.files.iter().rev() {
            let stand_in = profile
                .as_deref()
                .filter(|_| *file == level.target)
                .map(|p| crate::environment::profiles::profile_file(&level.rel, p))
                .filter(|f| root.join(f).is_file());
            match stand_in {
                Some(profile_file) => push(&mut search, profile_file, false, FileKind::Profile),
                None => push(&mut search, file.clone(), false, FileKind::Env),
            }
        }
        if let Some(default) = &level.default {
            push(&mut search, default.clone(), false, FileKind::Default);
        }
    }

    let mut sourced: Vec<String> = Vec::new();
    let mut env = base.clone();
    let mut sources: HashMap<String, Source> = HashMap::new();
    for (file, needed, kind) in search {
        let path = root.join(&file);
        if !needed && !path.is_file() {
            continue;
        }
        let content = std::fs::read_to_string(&path)
            .with_context(|| format!("Failed to read env file '{}'", path.display()))?;
        // Within one file the last assignment counts, as a shell would have it.
        let mut values: Vec<(String, String)> = Vec::new();
        for (key, value) in parse_env_content(&content) {
            values.retain(|(k, _)| *k != key);
            values.push((key, value));
        }
        for (key, value) in values {
            let source = sources.entry(key.clone()).or_insert_with(|| Source {
                terminal: base.get(&key).is_some_and(|v| !v.trim().is_empty()),
                ..Default::default()
            });
            if source.files.is_empty() && !source.terminal {
                source.kind = Some(kind);
                env.insert(key, value);
            }
            source.files.push(file.clone());
        }
        sourced.push(file);
    }

    let missing_required = missing_required_env(&resolved.required_env, &env);

    // Features last, once every file has been searched — a `CIABATTA_FEAT_*`
    // set in a file is a feature exactly as much as one set on the command
    // line, and reading them before the files were sourced would silently make
    // only the latter count.
    let features = crate::environment::features::Features::from_env(&env);
    features.export_into(&mut env);

    Ok(PreparedEnv {
        env,
        sourced,
        sources,
        unresolved_paths,
        missing_required,
        features,
    })
}

/// Return the names from `required` that are absent from `env` or present but
/// empty (after trimming). An empty result means every required variable is
/// set, so the run may proceed. Order follows `required` so the reported list
/// matches how the operator declared `required_env`.
fn missing_required_env(required: &[String], env: &HashMap<String, String>) -> Vec<String> {
    required
        .iter()
        .filter(|key| {
            env.get(key.as_str())
                .map(|v| v.trim().is_empty())
                .unwrap_or(true)
        })
        .cloned()
        .collect()
}

/// Look up a variable's value for a condition, tolerating a leading `env.`
/// prefix (`env.IN_CI` and `IN_CI` are equivalent). Unset variables read as "".
fn cond_var<'a>(name: &str, env: &'a HashMap<String, String>) -> &'a str {
    let name = name.trim().strip_prefix("env.").unwrap_or(name.trim());
    env.get(name).map(String::as_str).unwrap_or("")
}

/// Whether a value counts as "truthy" for a bare-variable condition.
///
/// The rule lives in [`crate::environment::truthy`] because build features read
/// their variables by exactly the same one, and two copies of it would
/// eventually disagree about `off`.
fn cond_truthy(val: &str) -> bool {
    crate::environment::truthy(val)
}

/// Strip a single pair of matching surrounding quotes from a comparison operand.
fn cond_unquote(s: &str) -> &str {
    let s = s.trim();
    if s.len() >= 2
        && ((s.starts_with('"') && s.ends_with('"')) || (s.starts_with('\'') && s.ends_with('\'')))
    {
        &s[1..s.len() - 1]
    } else {
        s
    }
}

/// Evaluate a single step condition against the run's environment.
///
/// Supported forms (the variable may carry an optional `env.` prefix):
///   * `VAR == value` / `VAR != value` — string comparison of `VAR`'s value
///     (unset reads as empty); the right side may be quoted.
///   * `VAR` — true when `VAR` is truthy (set, non-empty, not `false`/`0`/`no`/`off`).
///   * `!VAR` — the negation of the truthy test.
///   * `feature.<name>` — true when that build feature is on. Sugar for the
///     `CIABATTA_FEAT_<NAME>` variable behind it, but spelled the way the
///     feature is named rather than the way the variable is, and matched
///     case-insensitively with `-` and `_` treated alike. `!feature.<name>` and
///     comparisons work as they do for any other name.
pub fn eval_condition(cond: &str, env: &HashMap<String, String>) -> Result<bool> {
    let cond = cond.trim();
    if cond.is_empty() {
        bail!("empty condition");
    }
    if let Some((lhs, rhs)) = cond.split_once("!=") {
        return Ok(cond_var(lhs, env) != cond_unquote(rhs));
    }
    if let Some((lhs, rhs)) = cond.split_once("==") {
        return Ok(cond_var(lhs, env) == cond_unquote(rhs));
    }
    if let Some(rest) = cond.strip_prefix('!') {
        return Ok(!cond_bare(rest, env));
    }
    Ok(cond_bare(cond, env))
}

/// The truthy test for a bare condition: a build feature when it is written as
/// one, an environment variable otherwise.
fn cond_bare(name: &str, env: &HashMap<String, String>) -> bool {
    match feature_ref(name) {
        Some(feature) => crate::environment::features::Features::from_env(env).is_on(&feature),
        None => cond_truthy(cond_var(name, env)),
    }
}

/// The feature a condition names, if it names one: `feature.new_ui` → `new_ui`.
///
/// `feat.` is accepted too, because the variable is spelled `CIABATTA_FEAT_`
/// and typing the shorter one is the obvious mistake to make.
fn feature_ref(cond: &str) -> Option<String> {
    let cond = cond.trim();
    let rest = cond
        .strip_prefix("feature.")
        .or_else(|| cond.strip_prefix("feat."))?;
    match rest.trim().is_empty() {
        true => None,
        false => Some(rest.to_string()),
    }
}

/// Decide whether a step should be skipped given the environment, returning a
/// short human-readable reason when it should. A step is skipped if any
/// `skip_if` condition holds, or if any `when` condition does not hold (all
/// `when` conditions must be true to run).
pub fn step_skip_reason(step: &RunStep, env: &HashMap<String, String>) -> Result<Option<String>> {
    for cond in &step.skip_if {
        if eval_condition(cond, env)? {
            return Ok(Some(format!("skip_if `{cond}`")));
        }
    }
    for cond in &step.when {
        if !eval_condition(cond, env)? {
            return Ok(Some(format!("when `{cond}` not met")));
        }
    }
    Ok(None)
}

/// Reorder steps so that dependencies always precede their dependents, giving
/// both the executor and the live view (`--gui`) a logical top-to-bottom order
/// regardless of how the flowchart file happened to list them.
///
/// Normal steps are topologically sorted over their `needs` edges, with ties
/// broken by original position so the result is stable and deterministic. Each
/// recovery node is placed immediately after the first step that routes to it
/// via `on_error` (where an operator would encounter it); any recovery node not
/// referenced that way is appended at the end so nothing is dropped.
///
/// Assumes the DAG has already passed [`validate_flowchart`] (acyclic, every
/// edge resolves), so a total order always exists.
pub fn topo_order(steps: &[RunStep]) -> Vec<RunStep> {
    use std::collections::VecDeque;

    let idx_of: HashMap<&str, usize> = steps
        .iter()
        .enumerate()
        .map(|(i, s)| (s.name.as_str(), i))
        .collect();
    let normal: Vec<usize> = (0..steps.len()).filter(|&i| !steps[i].recover).collect();
    let is_normal = |name: &str| idx_of.get(name).is_some_and(|&i| !steps[i].recover);

    // In-degree counts `needs` edges to other normal steps only (recovery nodes
    // are entered via `on_error`, not the success DAG).
    let mut indegree: HashMap<usize, usize> = normal.iter().map(|&i| (i, 0)).collect();
    for &i in &normal {
        for dep in &steps[i].needs {
            if is_normal(dep) {
                *indegree.get_mut(&i).unwrap() += 1;
            }
        }
    }

    // Kahn's algorithm. The ready queue is seeded — and refilled — in original
    // order, so among steps with satisfied dependencies the author's ordering is
    // preserved.
    let mut ready: VecDeque<usize> = normal
        .iter()
        .copied()
        .filter(|i| indegree[i] == 0)
        .collect();
    let mut ordered_normal: Vec<usize> = Vec::with_capacity(normal.len());
    while let Some(node) = ready.pop_front() {
        ordered_normal.push(node);
        for &j in &normal {
            if steps[j]
                .needs
                .iter()
                .any(|d| idx_of.get(d.as_str()) == Some(&node))
            {
                let deg = indegree.get_mut(&j).unwrap();
                *deg -= 1;
                if *deg == 0 {
                    ready.push_back(j);
                }
            }
        }
    }

    // Emit each normal step, dropping in the recovery node it first routes to
    // right after it.
    let mut result: Vec<RunStep> = Vec::with_capacity(steps.len());
    let mut placed = vec![false; steps.len()];
    for &node in &ordered_normal {
        result.push(steps[node].clone());
        placed[node] = true;
        if let Some(target) = steps[node].on_error.as_deref()
            && let Some(&ri) = idx_of.get(target)
            && steps[ri].recover
            && !placed[ri]
        {
            result.push(steps[ri].clone());
            placed[ri] = true;
        }
    }
    // Anything not yet emitted (unreferenced recovery nodes, or normal steps a
    // malformed-but-validated graph left out) follows in original order.
    for (i, step) in steps.iter().enumerate() {
        if !placed[i] {
            result.push(step.clone());
        }
    }
    result
}

/// Validate a resolved step DAG:
///   - at least one step, unique names
///   - normal steps have an action; recovery nodes have ≥1 option
///   - `needs` / `on_error` / `retry` reference existing nodes
///   - `on_error` targets are recovery nodes
///   - the success DAG (`needs` edges) is acyclic
pub fn validate_flowchart(steps: &[RunStep], run_name: &str) -> Result<()> {
    if steps.is_empty() {
        bail!("Workflow '{}' has no steps.", run_name);
    }

    let mut names: HashSet<&str> = HashSet::new();
    for step in steps {
        if step.name.trim().is_empty() {
            bail!("Workflow '{}' has a step with an empty name.", run_name);
        }
        if !names.insert(step.name.as_str()) {
            bail!(
                "Workflow '{}' has a duplicate step name '{}'.",
                run_name,
                step.name
            );
        }
    }

    let is_recover = |name: &str| steps.iter().any(|s| s.name == name && s.recover);

    for step in steps {
        // Action / recovery shape.
        if step.recover {
            if step.options.is_empty() {
                bail!(
                    "Recovery node '{}' in run '{}' has no options.",
                    step.name,
                    run_name
                );
            }
            for opt in &step.options {
                if opt.label.trim().is_empty() {
                    bail!(
                        "Recovery node '{}' in run '{}' has an option with no label.",
                        step.name,
                        run_name
                    );
                }
            }
        } else if !step.has_action() {
            bail!(
                "Step '{}' in run '{}' has no `script`, `run`, or `workflow` to execute.",
                step.name,
                run_name
            );
        }

        // A release push with nothing to publish can only fail at run time,
        // after the build it was waiting on — say so before anything runs.
        if step.release_direction() == Some(release::Direction::Push)
            && step.script.is_none()
            && step.run.is_none()
            && step.release.as_ref().is_none_or(|r| r.files.is_empty())
        {
            bail!(
                "Step '{}' in run '{}' is a `kind: release-push` with no `release.files`                  — list the globs of what to publish.",
                step.name,
                run_name
            );
        }

        // A timeout that doesn't parse would silently become "no limit" — the
        // opposite of what an operator guarding a hanging step intended.
        if let Err(err) = step.timeout_duration() {
            bail!("{err} (in run '{run_name}')");
        }

        // Conditions must be non-blank so a typo can't silently read as "run".
        for cond in step.when.iter().chain(step.skip_if.iter()) {
            if cond.trim().is_empty() {
                bail!(
                    "Step '{}' in run '{}' has an empty `when`/`skip_if` condition.",
                    step.name,
                    run_name
                );
            }
        }

        // Edge targets must exist.
        for dep in &step.needs {
            if !names.contains(dep.as_str()) {
                bail!(
                    "Step '{}' in run '{}' needs '{}', which is not a defined step.",
                    step.name,
                    run_name,
                    dep
                );
            }
        }
        if let Some(target) = step.on_error.as_deref() {
            if !names.contains(target) {
                bail!(
                    "Step '{}' in run '{}' has on_error = '{}', which is not a defined step.",
                    step.name,
                    run_name,
                    target
                );
            }
            if !is_recover(target) {
                bail!(
                    "Step '{}' in run '{}' routes on_error to '{}', which is not a recovery node (set `recover = true`).",
                    step.name,
                    run_name,
                    target
                );
            }
        }
        if let Some(target) = step.retry.as_deref()
            && !names.contains(target)
        {
            bail!(
                "Recovery node '{}' in run '{}' has retry = '{}', which is not a defined step.",
                step.name,
                run_name,
                target
            );
        }
    }

    detect_cycle(steps, run_name)?;
    Ok(())
}

/// Depth-first cycle detection over the success DAG (`needs` edges only).
fn detect_cycle(steps: &[RunStep], run_name: &str) -> Result<()> {
    #[derive(Clone, Copy, PartialEq)]
    enum Mark {
        Unvisited,
        InProgress,
        Done,
    }

    let index: HashMap<&str, usize> = steps
        .iter()
        .enumerate()
        .map(|(i, s)| (s.name.as_str(), i))
        .collect();
    let mut marks = vec![Mark::Unvisited; steps.len()];

    // Iterative DFS to avoid stack overflow on pathological graphs.
    for start in 0..steps.len() {
        if marks[start] != Mark::Unvisited {
            continue;
        }
        // Stack of (node, next-dep-cursor).
        let mut stack: Vec<(usize, usize)> = vec![(start, 0)];
        marks[start] = Mark::InProgress;
        while let Some(&(node, cursor)) = stack.last() {
            let deps = &steps[node].needs;
            if cursor < deps.len() {
                stack.last_mut().unwrap().1 += 1;
                if let Some(&next) = index.get(deps[cursor].as_str()) {
                    match marks[next] {
                        Mark::InProgress => bail!(
                            "Run '{}' has a dependency cycle involving step '{}'.",
                            run_name,
                            steps[next].name
                        ),
                        Mark::Unvisited => {
                            marks[next] = Mark::InProgress;
                            stack.push((next, 0));
                        }
                        Mark::Done => {}
                    }
                }
            } else {
                marks[node] = Mark::Done;
                stack.pop();
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A step list on its own, the way a workflow file spells one.
    #[derive(Deserialize)]
    struct StepList {
        #[serde(default)]
        steps: Vec<RunStep>,
    }

    fn steps_from(toml_str: &str) -> Vec<RunStep> {
        let list: StepList = toml::from_str(toml_str).expect("steps parse");
        list.steps
    }

    #[test]
    fn valid_linear_and_dag_flowcharts_pass() {
        let steps = steps_from(
            r#"
[[steps]]
name = "build"
script = "b.sh"
[[steps]]
name = "test"
run = "make test"
needs = ["build"]
[[steps]]
name = "release"
script = "r.sh"
needs = ["test"]
"#,
        );
        assert!(validate_flowchart(&steps, "web").is_ok());
    }

    #[test]
    fn on_error_routes_to_recovery_node_with_options() {
        let steps = steps_from(
            r#"
[[steps]]
name = "migrate"
script = "m.sh"
on_error = "fix"
[[steps]]
name = "fix"
recover = true
message = "pick a remedy"
retry = "migrate"
options = [
  { label = "rollback", script = "rb.sh" },
  { label = "unlock", run = "make unlock", default = true },
]
"#,
        );
        assert!(validate_flowchart(&steps, "web").is_ok());
    }

    #[test]
    fn rejects_duplicate_names() {
        let steps = steps_from(
            r#"
[[steps]]
name = "a"
script = "a.sh"
[[steps]]
name = "a"
script = "a2.sh"
"#,
        );
        let err = validate_flowchart(&steps, "web").unwrap_err().to_string();
        assert!(err.contains("duplicate step name"));
    }

    #[test]
    fn rejects_missing_edge_targets() {
        let needs = steps_from(
            r#"
[[steps]]
name = "a"
script = "a.sh"
needs = ["ghost"]
"#,
        );
        assert!(
            validate_flowchart(&needs, "web")
                .unwrap_err()
                .to_string()
                .contains("ghost")
        );

        let on_err = steps_from(
            r#"
[[steps]]
name = "a"
script = "a.sh"
on_error = "ghost"
"#,
        );
        assert!(validate_flowchart(&on_err, "web").is_err());
    }

    #[test]
    fn rejects_on_error_to_non_recovery_node() {
        let steps = steps_from(
            r#"
[[steps]]
name = "a"
script = "a.sh"
on_error = "b"
[[steps]]
name = "b"
script = "b.sh"
"#,
        );
        let err = validate_flowchart(&steps, "web").unwrap_err().to_string();
        assert!(err.contains("not a recovery node"));
    }

    #[test]
    fn rejects_recovery_node_without_options() {
        let steps = steps_from(
            r#"
[[steps]]
name = "fix"
recover = true
"#,
        );
        assert!(
            validate_flowchart(&steps, "web")
                .unwrap_err()
                .to_string()
                .contains("no options")
        );
    }

    #[test]
    fn rejects_step_without_action() {
        let steps = steps_from(
            r#"
[[steps]]
name = "a"
"#,
        );
        assert!(
            validate_flowchart(&steps, "web")
                .unwrap_err()
                .to_string()
                .contains("no `script`, `run`, or `workflow`")
        );
    }

    #[test]
    fn parse_env_content_handles_comments_quotes_and_export() {
        let pairs = parse_env_content(
            "# a comment\n\
             \n\
             export API_TOKEN=abc123\n\
             REGION = \"us-east-1\"\n\
             QUOTED='single val'\n\
             EMPTY=\n\
             noequals\n\
             =novalue\n",
        );
        assert_eq!(
            pairs,
            vec![
                ("API_TOKEN".to_string(), "abc123".to_string()),
                ("REGION".to_string(), "us-east-1".to_string()),
                ("QUOTED".to_string(), "single val".to_string()),
                ("EMPTY".to_string(), "".to_string()),
            ]
        );
    }

    #[test]
    fn load_env_files_layers_under_existing_and_across_files() {
        let dir = std::env::temp_dir().join(format!("ciab_envfile_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.env"), "A=1\nB=from_a\nC=from_a\n").unwrap();
        std::fs::write(dir.join("b.env"), "B=from_b\n").unwrap();

        // A is already resolved (and non-empty) so it must not be clobbered; B is
        // overridden by the later file; C comes from the first file.
        let base: HashMap<String, String> = [("A".to_string(), "existing".to_string())]
            .into_iter()
            .collect();
        let merged =
            load_env_files(&["a.env".to_string(), "b.env".to_string()], &dir, &base).unwrap();
        assert_eq!(merged.get("A").unwrap(), "existing");
        assert_eq!(merged.get("B").unwrap(), "from_b");
        assert_eq!(merged.get("C").unwrap(), "from_a");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn load_env_files_errors_on_missing_file() {
        let err = load_env_files(
            &["nope.env".to_string()],
            Path::new("/proj"),
            &HashMap::new(),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("nope.env"));
    }

    fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn missing_required_env_flags_unset_and_empty_only() {
        let required = vec![
            "API_TOKEN".to_string(),
            "REGION".to_string(),
            "STAGE".to_string(),
        ];
        // API_TOKEN set, REGION empty, STAGE absent → REGION + STAGE missing.
        let missing =
            missing_required_env(&required, &env(&[("API_TOKEN", "abc"), ("REGION", "  ")]));
        assert_eq!(missing, vec!["REGION".to_string(), "STAGE".to_string()]);
    }

    #[test]
    fn missing_required_env_empty_when_all_set() {
        let required = vec!["A".to_string(), "B".to_string()];
        assert!(missing_required_env(&required, &env(&[("A", "1"), ("B", "2")])).is_empty());
        // No requirements → never missing.
        assert!(missing_required_env(&[], &env(&[])).is_empty());
    }

    #[test]
    fn prepare_env_sources_files_and_reports_what_is_still_missing() {
        let dir = std::env::temp_dir().join(format!("ciab_prep_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(".env.prod"), "API_TOKEN=from-file\n").unwrap();

        let resolved = ResolvedRun {
            required_env: vec!["API_TOKEN".to_string(), "REGION".to_string()],
            env_files: vec![".env.{RUN_ENV}".to_string()],
            ..Default::default()
        };

        // RUN_ENV set → the file resolves and is sourced, satisfying API_TOKEN;
        // only REGION (supplied by nothing) is left to ask for.
        let prepared = prepare_env(&resolved, &dir, &env(&[("RUN_ENV", "prod")])).unwrap();
        assert_eq!(prepared.sourced, vec![".env.prod".to_string()]);
        assert_eq!(prepared.env.get("API_TOKEN").unwrap(), "from-file");
        assert!(prepared.unresolved_paths.is_empty());
        assert_eq!(prepared.missing(), vec!["REGION".to_string()]);
        assert!(!prepared.is_ready());

        // Everything supplied → ready, and nothing to prompt for.
        let prepared = prepare_env(
            &resolved,
            &dir,
            &env(&[("RUN_ENV", "prod"), ("REGION", "us-east-1")]),
        )
        .unwrap();
        assert!(prepared.is_ready());
        assert!(prepared.missing().is_empty());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn prepare_env_asks_for_env_file_placeholders_first_and_alone() {
        // RUN_ENV is unset, so `.env.{RUN_ENV}` can't even be named. The file it
        // would resolve to may well supply API_TOKEN, so that isn't reported
        // yet — the caller supplies RUN_ENV and checks again.
        let resolved = ResolvedRun {
            required_env: vec!["API_TOKEN".to_string()],
            env_files: vec![".env.{RUN_ENV}".to_string()],
            ..Default::default()
        };
        let prepared = prepare_env(&resolved, Path::new("/proj"), &HashMap::new()).unwrap();
        assert_eq!(prepared.unresolved_paths, vec!["RUN_ENV".to_string()]);
        assert!(prepared.missing_required.is_empty());
        assert_eq!(prepared.missing(), vec!["RUN_ENV".to_string()]);
        assert!(!prepared.is_ready());
        // Nothing was read, so a missing file isn't an error at this point.
        assert!(prepared.sourced.is_empty());
    }

    fn scratch_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("ciab_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("packages/api")).unwrap();
        dir
    }

    fn level(files: &[&str], default: Option<&str>) -> EnvLevel {
        let rel = files[0]
            .rsplit_once('/')
            .map(|(dir, _)| dir.to_string())
            .unwrap_or_else(|| ".".to_string());
        EnvLevel {
            rel,
            files: files.iter().map(|f| f.to_string()).collect(),
            target: files[0].to_string(),
            default: default.map(str::to_string),
        }
    }

    /// From the workspace the run started in up to the root, and within each:
    /// `.env`, then `.env.default`. First to set wins.
    #[test]
    fn the_environment_is_searched_nearest_first_and_the_first_match_wins() {
        let dir = scratch_dir("env_search");
        std::fs::write(dir.join("packages/api/.env"), "SHARED=api\n").unwrap();
        std::fs::write(
            dir.join("packages/api/.env.default"),
            "SHARED=api-default\nAPI_DEFAULT=yes\n",
        )
        .unwrap();
        std::fs::write(dir.join(".env"), "SHARED=root\nREGION=global\n").unwrap();
        std::fs::write(dir.join(".env.default"), "REGION=us\nTIER=dev\n").unwrap();

        let resolved = ResolvedRun {
            required_env: vec!["TIER".into(), "MISSING".into()],
            env_levels: vec![
                level(&["packages/api/.env"], Some("packages/api/.env.default")),
                level(&[".env"], Some(".env.default")),
            ],
            ..Default::default()
        };
        let prepared = prepare_env(&resolved, &dir, &HashMap::new()).unwrap();

        // The nearest workspace's `.env` beats its own default and the root.
        assert_eq!(prepared.env["SHARED"], "api");
        assert_eq!(
            prepared.sources["SHARED"].files,
            ["packages/api/.env", "packages/api/.env.default", ".env"],
            "every file that offered it, the winner first"
        );
        // A nearer workspace's default still beats the root's `.env`.
        assert_eq!(prepared.env["API_DEFAULT"], "yes");
        assert_eq!(
            prepared.sources["API_DEFAULT"].kind,
            Some(FileKind::Default)
        );
        // What the nearest doesn't set comes from further up.
        assert_eq!(prepared.env["REGION"], "global");
        assert_eq!(prepared.env["TIER"], "dev");
        // A required variable nothing sets stops the run.
        assert_eq!(prepared.missing_required, ["MISSING"]);
        assert!(!prepared.is_ready());
        assert_eq!(
            prepared.sourced,
            [
                "packages/api/.env",
                "packages/api/.env.default",
                ".env",
                ".env.default"
            ]
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// The terminal always wins; a workflow's own `env_file` comes next,
    /// ahead of every workspace.
    #[test]
    fn the_terminal_then_the_workflows_files_come_before_any_workspace() {
        let dir = scratch_dir("env_search_top");
        std::fs::write(dir.join(".env"), "STAGE=from-env\nREGION=from-env\n").unwrap();
        std::fs::write(dir.join(".env.prod"), "STAGE=from-workflow\n").unwrap();

        let resolved = ResolvedRun {
            steps: vec![RunStep {
                name: "deploy".into(),
                run: Some("true".into()),
                env_files: vec![".env.{DEPLOY_ENV}".into(), ".env.{UNSET}".into()],
                ..Default::default()
            }],
            env_levels: vec![level(&[".env"], None)],
            ..Default::default()
        };
        let base = env(&[("DEPLOY_ENV", "prod"), ("REGION", "from-terminal")]);
        let prepared = prepare_env(&resolved, &dir, &base).unwrap();
        assert_eq!(prepared.env["STAGE"], "from-workflow");
        assert_eq!(prepared.env["REGION"], "from-terminal");
        assert!(prepared.sources["REGION"].terminal);
        assert_eq!(
            prepared.sourced,
            [".env.prod", ".env"],
            "the unset placeholder is skipped"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// With a profile elected, each workspace's `.ciabatta/.env.<profile>`
    /// stands in for its `.env` — what the copy will make it — and the
    /// `.env.default` still answers what the profile doesn't set. A workspace
    /// without that profile keeps its own `.env`.
    #[test]
    fn an_elected_profile_stands_in_for_the_env_it_is_copied_into() {
        let dir = scratch_dir("profile_stand_in");
        std::fs::create_dir_all(dir.join(".ciabatta")).unwrap();
        std::fs::write(dir.join(".env"), "REGION=us\nSECRET=mine\n").unwrap();
        std::fs::write(dir.join(".ciabatta/.env.staging"), "REGION=eu\n").unwrap();
        std::fs::write(dir.join(".env.default"), "TIER=dev\n").unwrap();
        std::fs::write(dir.join("packages/api/.env"), "API=own\n").unwrap();

        let resolved = ResolvedRun {
            env_levels: vec![
                level(&["packages/api/.env"], None),
                level(&[".env"], Some(".env.default")),
            ],
            ..Default::default()
        };
        let staging = env(&[(crate::environment::profiles::PROFILE_VAR, "staging")]);
        let prepared = prepare_env(&resolved, &dir, &staging).unwrap();
        assert_eq!(prepared.env["REGION"], "eu");
        assert!(
            !prepared.env.contains_key("SECRET"),
            "the profile replaces the .env, it doesn't layer over it"
        );
        assert_eq!(prepared.env["TIER"], "dev");
        assert_eq!(prepared.env["API"], "own");
        assert_eq!(prepared.sources["REGION"].kind, Some(FileKind::Profile));
        assert_eq!(
            prepared.sourced,
            [
                "packages/api/.env",
                ".ciabatta/.env.staging",
                ".env.default"
            ]
        );

        // Without the profile, the `.env` is read as it is.
        let prepared = prepare_env(&resolved, &dir, &HashMap::new()).unwrap();
        assert_eq!(prepared.env["REGION"], "us");
        assert_eq!(prepared.env["SECRET"], "mine");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// `env_default` names a level's defaults file; without it a `.env.default`
    /// beside the `.env` counts, but a `.env.example` (often placeholders)
    /// doesn't.
    #[test]
    fn a_levels_defaults_file_is_declared_or_named_env_default() {
        use crate::environment::files::defaults_file;
        use crate::workspace::WorkspaceMeta;

        let dir = scratch_dir("defaults_file");
        std::fs::create_dir_all(dir.join(".ciabatta")).unwrap();
        std::fs::write(dir.join(".ciabatta/.env.default"), "A=1\n").unwrap();
        std::fs::write(dir.join(".env.example"), "A=placeholder\n").unwrap();

        let declared = WorkspaceMeta {
            env_default: Some(".ciabatta/.env.default".into()),
            ..Default::default()
        };
        assert_eq!(
            defaults_file(&declared, &dir).as_deref(),
            Some(".ciabatta/.env.default")
        );
        assert_eq!(defaults_file(&WorkspaceMeta::default(), &dir), None);
        std::fs::write(dir.join(".env.default"), "A=2\n").unwrap();
        assert_eq!(
            defaults_file(&WorkspaceMeta::default(), &dir).as_deref(),
            Some(".env.default")
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn prepare_env_is_ready_when_nothing_is_declared() {
        let prepared =
            prepare_env(&ResolvedRun::default(), Path::new("/proj"), &HashMap::new()).unwrap();
        assert!(prepared.is_ready());
        assert!(prepared.missing().is_empty());
    }

    #[test]
    fn unresolved_vars_lists_only_what_is_unset() {
        let vars = env(&[("REGION", "us-east-1")]);
        assert_eq!(
            crate::config::unresolved_vars("{REGION}/{STAGE}/{STAGE}", &vars),
            vec!["STAGE".to_string()]
        );
        assert!(crate::config::unresolved_vars("{REGION}", &vars).is_empty());
    }

    #[test]
    fn eval_condition_covers_comparison_and_truthy_forms() {
        let env: HashMap<String, String> = [
            ("IN_CI", "true"),
            ("RUN_ENV", "prod"),
            ("REGION", "us-east-1"),
            ("FLAG_OFF", "false"),
            ("EMPTY", ""),
        ]
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();

        // Equality / inequality, with and without the `env.` prefix and quotes.
        assert!(eval_condition("env.IN_CI == true", &env).unwrap());
        assert!(eval_condition("IN_CI == \"true\"", &env).unwrap());
        assert!(eval_condition("RUN_ENV != dev", &env).unwrap());
        assert!(!eval_condition("RUN_ENV == dev", &env).unwrap());
        assert!(eval_condition("REGION == us-east-1", &env).unwrap());
        // Unset variable reads as empty.
        assert!(eval_condition("MISSING != something", &env).unwrap());
        assert!(!eval_condition("MISSING == something", &env).unwrap());
        // Bare truthy / negation.
        assert!(eval_condition("IN_CI", &env).unwrap());
        assert!(!eval_condition("FLAG_OFF", &env).unwrap());
        assert!(!eval_condition("EMPTY", &env).unwrap());
        assert!(eval_condition("!FLAG_OFF", &env).unwrap());
        assert!(!eval_condition("!IN_CI", &env).unwrap());
    }

    #[test]
    fn step_skip_reason_applies_when_and_skip_if() {
        let ci: HashMap<String, String> = [("IN_CI".to_string(), "true".to_string())]
            .into_iter()
            .collect();
        let local: HashMap<String, String> = HashMap::new();

        // skip_if fires only when its condition holds.
        let step = RunStep {
            name: "notify".into(),
            run: Some("true".into()),
            skip_if: vec!["env.IN_CI == true".into()],
            ..Default::default()
        };
        assert!(step_skip_reason(&step, &ci).unwrap().is_some());
        assert!(step_skip_reason(&step, &local).unwrap().is_none());

        // when requires ALL conditions; a single false one skips.
        let step = RunStep {
            name: "release".into(),
            run: Some("true".into()),
            when: vec!["IN_CI == true".into(), "MISSING == yes".into()],
            ..Default::default()
        };
        let reason = step_skip_reason(&step, &ci).unwrap().unwrap();
        assert!(reason.contains("MISSING"));

        // All when conditions met and no skip_if → runs.
        let step = RunStep {
            name: "release".into(),
            run: Some("true".into()),
            when: vec!["IN_CI == true".into()],
            ..Default::default()
        };
        assert!(step_skip_reason(&step, &ci).unwrap().is_none());
    }

    #[test]
    fn when_skip_if_accept_string_or_list_and_reject_blank() {
        let step: RunStep = toml::from_str(
            "name = \"a\"\nrun = \"true\"\nwhen = \"IN_CI\"\nskip_if = [\"A == b\", \"C\"]\n",
        )
        .unwrap();
        assert_eq!(step.when, vec!["IN_CI".to_string()]);
        assert_eq!(step.skip_if, vec!["A == b".to_string(), "C".to_string()]);

        let steps = steps_from("[[steps]]\nname=\"a\"\nrun=\"true\"\nwhen=\"  \"\n");
        assert!(
            validate_flowchart(&steps, "web")
                .unwrap_err()
                .to_string()
                .contains("empty `when`")
        );
    }

    #[test]
    fn env_file_accepts_string_or_list() {
        let one: crate::workspace::Workflow = toml::from_str("env_file = \".env\"\n").unwrap();
        assert_eq!(one.env_file, vec![".env".to_string()]);
        let many: crate::workspace::Workflow =
            toml::from_str("env_file = [\".env\", \".env.run\"]\n").unwrap();
        assert_eq!(
            many.env_file,
            vec![".env".to_string(), ".env.run".to_string()]
        );
    }

    #[test]
    fn parse_duration_accepts_the_forms_people_write() {
        use std::time::Duration;
        // Bare numbers are seconds, so `timeout = 90` works as well as "90s".
        assert_eq!(parse_duration("90").unwrap(), Duration::from_secs(90));
        assert_eq!(parse_duration("30s").unwrap(), Duration::from_secs(30));
        assert_eq!(parse_duration("10m").unwrap(), Duration::from_secs(600));
        assert_eq!(parse_duration("2h").unwrap(), Duration::from_secs(7200));
        assert_eq!(parse_duration("500ms").unwrap(), Duration::from_millis(500));
        // Compound and spelled-out forms.
        assert_eq!(parse_duration("1h30m").unwrap(), Duration::from_secs(5400));
        assert_eq!(parse_duration("1h 30m").unwrap(), Duration::from_secs(5400));
        assert_eq!(
            parse_duration("2 minutes").unwrap(),
            Duration::from_secs(120)
        );
        // Case doesn't matter.
        assert_eq!(parse_duration("10M").unwrap(), Duration::from_secs(600));

        // Nonsense is rejected rather than silently read as "no limit".
        assert!(parse_duration("").is_err());
        assert!(parse_duration("soon").is_err());
        assert!(parse_duration("10 parsecs").is_err());
        assert!(parse_duration("-5").is_err());
    }

    #[test]
    fn an_unparseable_timeout_is_caught_by_validation() {
        let steps = steps_from("[[steps]]\nname=\"a\"\nrun=\"true\"\ntimeout=\"whenever\"\n");
        let err = validate_flowchart(&steps, "web").unwrap_err().to_string();
        assert!(err.contains("invalid timeout"), "{err}");
    }

    #[test]
    fn steps_parse_the_full_monorepo_vocabulary() {
        let steps = steps_from(
            r#"
[[steps]]
name             = "compile"
description      = "Build the release binary"
owner            = "Ada"
run              = "cargo build --release"
requires         = ["cargo", "protoc"]
timeout          = "10m"
retries          = 2
continue_on_error = true
kind             = "build"
cwd              = "packages/api"

[[steps]]
name       = "dev-server"
run        = "npm run dev"
persistent = true
"#,
        );
        assert!(validate_flowchart(&steps, "web").is_ok());

        let compile = &steps[0];
        assert_eq!(
            compile.description.as_deref(),
            Some("Build the release binary")
        );
        assert_eq!(compile.owner.as_deref(), Some("Ada"));
        assert_eq!(compile.requires, vec!["cargo", "protoc"]);
        assert_eq!(
            compile.timeout_duration().unwrap(),
            Some(std::time::Duration::from_secs(600))
        );
        assert_eq!(compile.retries, 2);
        assert!(compile.continue_on_error);
        assert_eq!(compile.kind.as_deref(), Some("build"));
        assert_eq!(compile.cwd.as_deref(), Some("packages/api"));
        assert!(!compile.persistent);

        assert!(steps[1].persistent);
    }

    #[test]
    fn detects_dependency_cycle() {
        let steps = steps_from(
            r#"
[[steps]]
name = "a"
script = "a.sh"
needs = ["c"]
[[steps]]
name = "b"
script = "b.sh"
needs = ["a"]
[[steps]]
name = "c"
script = "c.sh"
needs = ["b"]
"#,
        );
        assert!(
            validate_flowchart(&steps, "web")
                .unwrap_err()
                .to_string()
                .contains("cycle")
        );
    }
}
