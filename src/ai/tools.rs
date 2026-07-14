//! Local tools the assistant can call.
//!
//! Every tool runs on the developer's machine and is deliberately small:
//!
//!   * `search_code`   — grep/ack/ripgrep passthrough ("find everything")
//!   * `suggest_files` — mind-map lookup ("find exactly what is needed")
//!   * `list_files`    — bounded project tree listing
//!   * `read_file`     — read a file (records usage into the mind map)
//!   * `tag_file`      — propose architecture tags (pending user confirmation)
//!   * `propose_change`— suggest new content for a file; shown to the user as
//!                       a diff and applied per the current [`Mode`]
//!   * `sandbox_run`   — run a command in a configured base image via
//!                       podman/docker, giving the AI a safe space to work
//!
//! All paths are confined to the project root; sandbox images are restricted
//! to the ones listed in the `[ai]` config section. Change proposals (and a
//! snapshot of each file before the change) live under
//! `.ciabatta/ai/suggestions/` — the working tree is only touched when a
//! change is accepted (or immediately in auto-accept mode).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::Mode;
use super::brain::Brain;
use super::provider::{ToolCall, ToolOutput, ToolSpec};
use crate::config::CiabattaConfig;

/// Lifecycle of a proposed code change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeState {
    /// Shown to the user, not yet applied to the working tree.
    Pending,
    /// Written to the real file (user accepted it, or auto-accept mode).
    Applied,
    /// The user turned it down.
    Rejected,
    /// Was applied, then undone — the file was restored from its snapshot.
    Reverted,
}

impl ChangeState {
    pub fn label(self) -> &'static str {
        match self {
            ChangeState::Pending => "pending",
            ChangeState::Applied => "applied",
            ChangeState::Rejected => "rejected",
            ChangeState::Reverted => "reverted",
        }
    }
}

/// One proposed code change. The proposal and a pre-change snapshot both live
/// under `.ciabatta/ai/suggestions/`, so the diff stays viewable (and openable
/// in an editor) even after the change is applied.
#[derive(Debug, Clone)]
pub struct ChangeSuggestion {
    /// Display path: relative to the project root for workspace files, or an
    /// absolute path for a bespoke file under /tmp.
    pub file: String,
    /// The resolved absolute destination the change writes to when accepted.
    pub target: PathBuf,
    /// Snapshot of the file before the change (empty for new files).
    pub original: PathBuf,
    /// The proposed full content.
    pub proposed: PathBuf,
    /// Unified diff, `a/<file>` → `b/<file>`.
    pub diff: String,
    /// The model's one-line rationale.
    pub reason: String,
    /// True when this change created a file that did not exist before, so
    /// reverting it removes the file rather than restoring empty content.
    pub created: bool,
    pub state: ChangeState,
}

/// One step in the assistant's working plan for the current task.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanItem {
    pub step: String,
    pub status: PlanStatus,
}

/// Where a plan step stands. Exactly one step should be `InProgress` at a time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanStatus {
    Pending,
    InProgress,
    Completed,
    Cancelled,
}

impl PlanStatus {
    fn parse(s: &str) -> Self {
        match s.trim().to_lowercase().as_str() {
            "in_progress" | "in-progress" | "active" | "doing" => PlanStatus::InProgress,
            "completed" | "done" | "complete" => PlanStatus::Completed,
            "cancelled" | "canceled" | "skip" | "skipped" => PlanStatus::Cancelled,
            _ => PlanStatus::Pending,
        }
    }

    /// A checkbox-style glyph for terminal rendering.
    pub fn glyph(self) -> &'static str {
        match self {
            PlanStatus::Pending => "[ ]",
            PlanStatus::InProgress => "[~]",
            PlanStatus::Completed => "[x]",
            PlanStatus::Cancelled => "[-]",
        }
    }
}

/// Everything tool execution needs: the project, the brain, and the config.
pub struct ToolBox {
    pub root: PathBuf,
    pub brain: Arc<Brain>,
    pub config: CiabattaConfig,
    /// Relative paths of files touched during the current question, used for
    /// the precision component of the feedback loop.
    pub files_touched: std::sync::Mutex<std::collections::BTreeSet<String>>,
    /// Files read this session, so `edit_file` can enforce read-before-edit —
    /// a string edit against an unseen file is almost always a guess.
    read_files: std::sync::Mutex<std::collections::BTreeSet<String>>,
    /// The assistant's working mode (plan / edit / auto-accept), which gates
    /// what `propose_change` is allowed to do.
    mode: std::sync::Mutex<Mode>,
    /// Change proposals for this session, oldest first.
    changes: std::sync::Mutex<Vec<ChangeSuggestion>>,
    /// The assistant's current working plan (the `update_plan` tool's state).
    plan: std::sync::Mutex<Vec<PlanItem>>,
    /// The only directories the assistant may read from or write to: the
    /// project workspace and /tmp (its scratch space for bespoke files).
    /// Anything outside these is refused.
    bounds: Vec<PathBuf>,
}

impl ToolBox {
    pub fn new(root: PathBuf, brain: Arc<Brain>, config: CiabattaConfig) -> Self {
        let bounds = vec![
            root.canonicalize().unwrap_or_else(|_| root.clone()),
            Path::new("/tmp").canonicalize().unwrap_or_else(|_| PathBuf::from("/tmp")),
        ];
        Self {
            root,
            brain,
            config,
            files_touched: Default::default(),
            read_files: Default::default(),
            mode: std::sync::Mutex::new(Mode::default()),
            changes: std::sync::Mutex::new(Vec::new()),
            plan: std::sync::Mutex::new(Vec::new()),
            bounds,
        }
    }

    pub fn mode(&self) -> Mode {
        *self.mode.lock().unwrap()
    }

    pub fn set_mode(&self, mode: Mode) {
        *self.mode.lock().unwrap() = mode;
    }

    /// How many change proposals exist (used to spot new ones after a round).
    pub fn changes_len(&self) -> usize {
        self.changes.lock().unwrap().len()
    }

    /// Clones of the proposals recorded at index `from` and later.
    pub fn changes_since(&self, from: usize) -> Vec<ChangeSuggestion> {
        self.changes.lock().unwrap()[from..].to_vec()
    }

    /// Clones of the proposals still waiting on the user, oldest first.
    pub fn pending_changes(&self) -> Vec<ChangeSuggestion> {
        self.changes
            .lock()
            .unwrap()
            .iter()
            .filter(|c| c.state == ChangeState::Pending)
            .cloned()
            .collect()
    }

    /// The assistant's current working plan, if it has set one.
    pub fn plan(&self) -> Vec<PlanItem> {
        self.plan.lock().unwrap().clone()
    }

    /// Apply (write to the working tree) or reject the oldest pending change
    /// for `file`, returning its updated record.
    pub fn resolve_change(&self, file: &str, accept: bool) -> Result<ChangeSuggestion> {
        let mut changes = self.changes.lock().unwrap();
        let change = changes
            .iter_mut()
            .find(|c| c.state == ChangeState::Pending && c.file == file)
            .with_context(|| format!("no pending change for '{file}'"))?;
        if accept {
            if let Some(parent) = change.target.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::copy(&change.proposed, &change.target)
                .with_context(|| format!("failed to write '{}'", change.file))?;
            change.state = ChangeState::Applied;
        } else {
            change.state = ChangeState::Rejected;
        }
        Ok(change.clone())
    }

    /// Undo the most recently applied change, restoring the file from its
    /// pre-change snapshot (or deleting it if the change had created it).
    /// Returns the reverted record, or `None` if nothing is applied.
    pub fn revert_last_applied(&self) -> Result<Option<ChangeSuggestion>> {
        let mut changes = self.changes.lock().unwrap();
        let Some(change) = changes.iter_mut().rev().find(|c| c.state == ChangeState::Applied) else {
            return Ok(None);
        };
        if change.created {
            // The change made a new file; undoing it removes the file.
            match std::fs::remove_file(&change.target) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e).with_context(|| format!("failed to remove '{}'", change.file)),
            }
        } else {
            std::fs::copy(&change.original, &change.target)
                .with_context(|| format!("failed to restore '{}'", change.file))?;
        }
        change.state = ChangeState::Reverted;
        Ok(Some(change.clone()))
    }

    /// The tool specs advertised to the model. In plan mode `propose_change`
    /// is left out entirely, so the model can't even try to edit.
    pub fn specs(&self) -> Vec<ToolSpec> {
        let images = self.sandbox_images();
        let mut specs = vec![
            ToolSpec {
                name: "update_plan",
                description: "Record or update your working plan for a multi-step task as a \
                              checklist. Call this when a task needs 3+ distinct steps, and \
                              again each time a step's status changes — keep exactly one step \
                              'in_progress' at a time and mark steps 'completed' as soon as \
                              they are truly done, not in a batch. Each call REPLACES the \
                              whole list, so always pass every step. Skip it for simple, \
                              single-step questions."
                    .into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "steps": {
                            "type": "array",
                            "description": "The complete, ordered list of steps",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "step": {"type": "string", "description": "Short, specific, actionable description"},
                                    "status": {"type": "string", "enum": ["pending", "in_progress", "completed", "cancelled"]}
                                },
                                "required": ["step", "status"]
                            }
                        }
                    },
                    "required": ["steps"]
                }),
            },
            ToolSpec {
                name: "search_code",
                description: "Search file contents across the project with a regex pattern \
                              (uses ripgrep/ack/grep, whichever is installed). Use this to \
                              find everything that mentions something. Returns matching \
                              lines as path:line:text."
                    .into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "pattern": {"type": "string", "description": "Regex to search for"},
                        "path": {"type": "string", "description": "Optional subdirectory or file to limit the search to"}
                    },
                    "required": ["pattern"]
                }),
            },
            ToolSpec {
                name: "suggest_files",
                description: "Look up the project's architecture mind map for the files most \
                              relevant to a topic. Use this FIRST, before searching: search \
                              finds everything, the map finds exactly what is needed. Returns \
                              ranked files with their path scores and tags."
                    .into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "topic": {"type": "string", "description": "What you are looking for, e.g. 'authentication' or 'deploy DAG'"}
                    },
                    "required": ["topic"]
                }),
            },
            ToolSpec {
                name: "list_files",
                description: "List project files (relative paths), skipping build output and \
                              VCS internals. Optionally filter with a substring."
                    .into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "filter": {"type": "string", "description": "Optional substring a path must contain"}
                    },
                    "required": []
                }),
            },
            ToolSpec {
                name: "glob",
                description: "Find files whose PATH matches a glob pattern (e.g. '**/*.rs', \
                              'src/**/mod.rs', 'frontend/*.tsx'). Use this to locate files by \
                              name/extension; use `search_code` to match file CONTENTS. \
                              Returns matching relative paths."
                    .into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "pattern": {"type": "string", "description": "Glob pattern, relative to the project root"}
                    },
                    "required": ["pattern"]
                }),
            },
            ToolSpec {
                name: "read_file",
                description: "Read a project file (optionally a line range). Reading a file \
                              records it as 'used', strengthening its place in the \
                              architecture map."
                    .into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "path": {"type": "string", "description": "Path relative to the project root"},
                        "start_line": {"type": "integer", "description": "1-based first line (optional)"},
                        "end_line": {"type": "integer", "description": "1-based last line (optional)"}
                    },
                    "required": ["path"]
                }),
            },
            ToolSpec {
                name: "tag_file",
                description: "Tag a file as belonging to one or more architectures (e.g. \
                              'frontend', 'auth', 'deploy'). A file can have multiple tags. \
                              Tags are NOT final until the user confirms them, so tag as you \
                              traverse and explain your reasoning briefly."
                    .into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "path": {"type": "string", "description": "Path relative to the project root"},
                        "tags": {"type": "array", "items": {"type": "string"}, "description": "Architecture tags for this file"},
                        "reason": {"type": "string", "description": "One line on why these tags fit"}
                    },
                    "required": ["path", "tags"]
                }),
            },
            ToolSpec {
                name: "sandbox_run",
                description: format!(
                    "Run a shell command inside a disposable container (a safe space to \
                     build, test, or experiment). Allowed base images: {}. The project is \
                     mounted read-only at /workspace.",
                    if images.is_empty() {
                        "none configured — add `images = [\"...\"]` to [ai] in ciabatta.toml".to_string()
                    } else {
                        images.join(", ")
                    }
                ),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "image": {"type": "string", "description": "One of the configured base images"},
                        "command": {"type": "string", "description": "Shell command to run inside the container"}
                    },
                    "required": ["image", "command"]
                }),
            },
            ToolSpec {
                name: "run_command",
                description: "Run a shell command locally in the project root, on the user's \
                              machine, with the real installed toolchains (cargo, rustc, python, \
                              pip, node, npm, pytest, make, git, …). Use this to build, test, \
                              lint, or run the project for real — e.g. `cargo build`, `cargo \
                              test`, `python -m pytest`, `npm run build`. Combined stdout+stderr \
                              and the exit status are returned. Commands run with your own \
                              permissions and can modify the working tree, so prefer `sandbox_run` \
                              for anything untrusted or throwaway. Times out after 300s."
                    .to_string(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "command": {"type": "string", "description": "Shell command to run in the project root"}
                    },
                    "required": ["command"]
                }),
            },
            ToolSpec {
                name: "deps",
                description: "Traverse the project's dependency graph (built by static analysis \
                              during burn-in or the `/analyze` command): third-party dependencies, \
                              internal packages, and publish points. Call with no query for an \
                              overview of every internal package and its dependency count; pass a \
                              file path, package name, or dependency name to see what it depends on \
                              (inputs) and what depends on it (outputs). Returns a 'no dependency \
                              graph yet' message until an analysis has run."
                    .to_string(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "query": {"type": "string", "description": "Optional: a file path, internal package, or dependency name to focus on"}
                    }
                }),
            },
        ];
        if self.mode() != Mode::Plan {
            specs.push(ToolSpec {
                name: "edit_file",
                description: "Change part of an EXISTING file by replacing an exact string. \
                              PREFER THIS over propose_change for edits: you give only the \
                              text to replace (`old`) and its replacement (`new`), not the \
                              whole file. You must have read the file first. `old` must \
                              appear exactly once (whitespace/indentation are matched \
                              leniently) — include enough surrounding lines to make it \
                              unique, or set `replace_all` to change every occurrence. Like \
                              propose_change, the edit is shown as a diff and (in edit mode) \
                              waits for the user to accept it."
                    .into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "path": {"type": "string", "description": "Path (relative to the project root, or absolute under /tmp) of the file to edit"},
                        "old": {"type": "string", "description": "The exact text to replace, with enough context to be unique"},
                        "new": {"type": "string", "description": "The text to replace it with"},
                        "replace_all": {"type": "boolean", "description": "Replace every occurrence of `old` (default false)"},
                        "reason": {"type": "string", "description": "One line on what this change does and why"}
                    },
                    "required": ["path", "old", "new"]
                }),
            });
            specs.push(ToolSpec {
                name: "propose_change",
                description: "Propose a change to one project file by giving its COMPLETE \
                              new content. Use this for NEW files or a full rewrite; for a \
                              localized change to an existing file, prefer `edit_file`. The \
                              user sees the change as a diff. In edit mode it is applied \
                              only after the user accepts it — never assume a pending change \
                              landed. In auto-accept mode it is written immediately."
                    .into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "path": {"type": "string", "description": "Path relative to the project root (may be a new file), or an absolute path under /tmp for a bespoke scratch file"},
                        "new_content": {"type": "string", "description": "The complete new content of the file"},
                        "reason": {"type": "string", "description": "One line on what this change does and why"}
                    },
                    "required": ["path", "new_content"]
                }),
            });
        }
        specs
    }

    /// Execute one tool call, never propagating errors to the caller — the
    /// model gets the error text back instead so it can adapt.
    pub async fn execute(&self, call: &ToolCall) -> ToolOutput {
        let result = match call.name.as_str() {
            "update_plan" => self.update_plan(&call.args),
            "search_code" => self.search_code(&call.args).await,
            "suggest_files" => self.suggest_files(&call.args),
            "list_files" => self.list_files(&call.args),
            "glob" => self.glob(&call.args),
            "read_file" => self.read_file(&call.args),
            "tag_file" => self.tag_file(&call.args),
            "edit_file" => self.edit_file(&call.args).await,
            "propose_change" => self.propose_change(&call.args).await,
            "sandbox_run" => self.sandbox_run(&call.args).await,
            "run_command" => self.run_command(&call.args).await,
            "deps" => self.deps(&call.args),
            other => Err(anyhow::anyhow!("unknown tool '{other}'")),
        };
        match result {
            Ok(content) => ToolOutput {
                call_id: call.id.clone(),
                content,
                is_error: false,
            },
            Err(e) => ToolOutput {
                call_id: call.id.clone(),
                content: format!("error: {e}"),
                is_error: true,
            },
        }
    }

    /// Whether a tool only reads state, so calls to it are safe to run
    /// concurrently with each other in one round.
    pub fn is_read_only(name: &str) -> bool {
        matches!(name, "search_code" | "suggest_files" | "list_files" | "glob" | "read_file" | "deps")
    }

    /// A one-line human-readable description of a call, for status displays.
    pub fn describe(call: &ToolCall) -> String {
        let arg = |k: &str| call.args[k].as_str().unwrap_or("").to_string();
        match call.name.as_str() {
            "update_plan" => "📋 update plan".to_string(),
            "search_code" => format!("🔎 search: {}", arg("pattern")),
            "suggest_files" => format!("🧠 mind map: {}", arg("topic")),
            "list_files" => "🗂  list files".to_string(),
            "glob" => format!("🗂  glob: {}", arg("pattern")),
            "read_file" => format!("📄 read: {}", arg("path")),
            "tag_file" => format!("🏷  tag: {} ({})", arg("path"), join_tags(&call.args["tags"])),
            "edit_file" => format!("✏  edit: {}", arg("path")),
            "propose_change" => format!("✏  change: {}", arg("path")),
            "sandbox_run" => format!("📦 sandbox [{}]: {}", arg("image"), arg("command")),
            "run_command" => format!("⚙ run: {}", arg("command")),
            "deps" => {
                let q = arg("query");
                if q.is_empty() { "🔗 dependency overview".to_string() } else { format!("🔗 deps: {q}") }
            }
            other => format!("⚙ {other}"),
        }
    }

    /// The number of distinct files touched since the last reset.
    pub fn files_touched_count(&self) -> usize {
        self.files_touched.lock().unwrap().len()
    }

    /// Forget the touched-file set (called at the start of each question).
    pub fn reset_touched(&self) {
        self.files_touched.lock().unwrap().clear();
        self.read_files.lock().unwrap().clear();
    }

    fn sandbox_images(&self) -> Vec<String> {
        self.config
            .ai
            .as_ref()
            .map(|a| a.images.clone())
            .unwrap_or_default()
    }

    /// True when `path` sits inside one of the assistant's allowed roots (the
    /// project workspace or /tmp).
    fn within_bounds(&self, path: &Path) -> bool {
        self.bounds.iter().any(|b| path.starts_with(b))
    }

    /// The one-line reason a path was refused, for a consistent error message.
    fn out_of_bounds(&self, shown: &str) -> anyhow::Error {
        anyhow::anyhow!(
            "'{shown}' is outside the assistant's allowed area — it may only read or write \
             the project workspace and /tmp"
        )
    }

    /// Turn a model-supplied path into an absolute path: relative paths hang
    /// off the workspace root; absolute paths (e.g. under /tmp) are taken as-is.
    fn absolutize(&self, path: &str) -> PathBuf {
        let p = path.trim();
        if Path::new(p).is_absolute() {
            PathBuf::from(p)
        } else {
            self.root.join(p.trim_start_matches("./"))
        }
    }

    /// Resolve a path that must already exist, confined to the workspace or
    /// /tmp. Symlinks and `..` are resolved before the bounds check.
    fn resolve(&self, path: &str) -> Result<PathBuf> {
        if path.trim().is_empty() {
            bail!("empty path");
        }
        let canonical = self
            .absolutize(path)
            .canonicalize()
            .with_context(|| format!("'{path}' does not exist"))?;
        if !self.within_bounds(&canonical) {
            return Err(self.out_of_bounds(path));
        }
        Ok(canonical)
    }

    /// Resolve a path that may not exist yet (a file to be created), confined
    /// to the workspace or /tmp. The deepest existing ancestor is canonicalized
    /// for symlink safety and the remaining components are resolved lexically,
    /// so `..` can't be used to climb out of bounds.
    fn resolve_new(&self, path: &str) -> Result<PathBuf> {
        if path.trim().is_empty() {
            bail!("empty path");
        }
        let resolved = normalize_lexical(&resolve_existing_prefix(&self.absolutize(path)));
        if !self.within_bounds(&resolved) {
            return Err(self.out_of_bounds(path));
        }
        Ok(resolved)
    }

    fn note_touch(&self, rel: &str) {
        self.files_touched
            .lock()
            .unwrap()
            .insert(rel.trim_start_matches("./").to_string());
    }

    // ─── update_plan ──────────────────────────────────────────────────────────

    fn update_plan(&self, args: &Value) -> Result<String> {
        let raw = args["steps"]
            .as_array()
            .context("update_plan needs a 'steps' array")?;
        let mut items = Vec::with_capacity(raw.len());
        let mut in_progress = 0;
        for entry in raw {
            let step = entry["step"].as_str().unwrap_or("").trim().to_string();
            if step.is_empty() {
                continue;
            }
            let status = PlanStatus::parse(entry["status"].as_str().unwrap_or("pending"));
            if status == PlanStatus::InProgress {
                in_progress += 1;
            }
            items.push(PlanItem { step, status });
        }
        if items.is_empty() {
            bail!("update_plan needs at least one non-empty step");
        }
        if in_progress > 1 {
            bail!("keep exactly one step 'in_progress' at a time (found {in_progress})");
        }
        let rendered = render_plan(&items);
        *self.plan.lock().unwrap() = items;
        Ok(rendered)
    }

    // ─── search_code ────────────────────────────────────────────────────────

    async fn search_code(&self, args: &Value) -> Result<String> {
        let pattern = args["pattern"]
            .as_str()
            .context("search_code needs a 'pattern'")?;
        let scope = args["path"].as_str().unwrap_or(".");
        // Keep the search inside the project.
        let scope_abs = self.resolve(scope).unwrap_or_else(|_| self.root.clone());

        let (bin, argv) = search_command(pattern, &scope_abs);
        let output = tokio::process::Command::new(&bin)
            .args(&argv)
            .current_dir(&self.root)
            .output()
            .await
            .with_context(|| format!("failed to run {bin}"))?;

        // grep-family tools exit 1 for "no matches" — that's a result, not an error.
        let stdout = String::from_utf8_lossy(&output.stdout);
        if stdout.trim().is_empty() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            if !output.status.success() && !stderr.trim().is_empty() {
                bail!("{bin}: {}", stderr.trim());
            }
            return Ok(format!("no matches for /{pattern}/"));
        }
        Ok(clip(&relativize(&stdout, &self.root), 12_000))
    }

    // ─── suggest_files ───────────────────────────────────────────────────────

    fn suggest_files(&self, args: &Value) -> Result<String> {
        let topic = args["topic"]
            .as_str()
            .context("suggest_files needs a 'topic'")?;
        let suggestions = self.brain.suggest(topic, 10);
        if suggestions.is_empty() {
            return Ok(format!(
                "the architecture map has nothing for '{topic}' yet — fall back to \
                 search_code, and tag_file what you find so next time the map knows"
            ));
        }
        let mut out = String::new();
        for (file, score, tags) in suggestions {
            out.push_str(&format!("{file}  (score {score:.1}; tags: {})\n", tags.join(", ")));
        }
        Ok(out)
    }

    // ─── list_files ─────────────────────────────────────────────────────────

    fn list_files(&self, args: &Value) -> Result<String> {
        let filter = args["filter"].as_str().unwrap_or("").to_lowercase();
        let files = project_files(&self.root);
        let listed: Vec<String> = files
            .into_iter()
            .filter(|f| filter.is_empty() || f.to_lowercase().contains(&filter))
            .collect();
        if listed.is_empty() {
            return Ok("no files matched".to_string());
        }
        Ok(clip(&listed.join("\n"), 12_000))
    }

    // ─── glob ─────────────────────────────────────────────────────────────────

    fn glob(&self, args: &Value) -> Result<String> {
        let pattern = args["pattern"].as_str().context("glob needs a 'pattern'")?.trim();
        if pattern.is_empty() {
            bail!("empty glob pattern");
        }
        // Anchor the pattern at the project root and match paths there.
        let rooted = self.root.join(pattern.trim_start_matches("./"));
        let entries = glob::glob(&rooted.to_string_lossy())
            .with_context(|| format!("invalid glob pattern '{pattern}'"))?;

        let mut matches = Vec::new();
        for entry in entries.flatten() {
            // Files only, kept inside the workspace, skipping build/VCS noise.
            if !entry.is_file() {
                continue;
            }
            let Ok(rel) = entry.strip_prefix(&self.root) else {
                continue;
            };
            let rel = rel.display().to_string();
            if rel.split('/').any(|c| SKIP_DIRS.contains(&c)) {
                continue;
            }
            matches.push(rel);
            if matches.len() >= 500 {
                break;
            }
        }
        if matches.is_empty() {
            return Ok(format!("no files match '{pattern}'"));
        }
        matches.sort();
        Ok(clip(&matches.join("\n"), 12_000))
    }

    // ─── read_file ──────────────────────────────────────────────────────────

    fn read_file(&self, args: &Value) -> Result<String> {
        let rel = args["path"].as_str().context("read_file needs a 'path'")?;
        let abs = self.resolve(rel)?;
        if !abs.is_file() {
            bail!("'{rel}' is not a file");
        }
        let content =
            std::fs::read_to_string(&abs).with_context(|| format!("failed to read '{rel}'"))?;

        // Track usage: reads feed the mind map's path scores.
        self.note_touch(rel);
        self.read_files
            .lock()
            .unwrap()
            .insert(rel.trim_start_matches("./").to_string());
        let _ = self.brain.record_file_use(rel.trim_start_matches("./"));

        let start = args["start_line"].as_u64().map(|n| n.max(1) as usize);
        let end = args["end_line"].as_u64().map(|n| n as usize);
        let lines: Vec<&str> = content.lines().collect();
        let (from, to) = (
            start.unwrap_or(1).min(lines.len().max(1)),
            end.unwrap_or(lines.len()).min(lines.len()),
        );
        let mut out = String::new();
        for (i, line) in lines.iter().enumerate().take(to).skip(from.saturating_sub(1)) {
            out.push_str(&format!("{:>5} {line}\n", i + 1));
        }
        Ok(clip(&out, 24_000))
    }

    // ─── tag_file ───────────────────────────────────────────────────────────

    fn tag_file(&self, args: &Value) -> Result<String> {
        let rel = args["path"].as_str().context("tag_file needs a 'path'")?;
        // Only real files can be tagged; this also confines the path.
        self.resolve(rel)?;
        let tags: Vec<String> = args["tags"]
            .as_array()
            .context("tag_file needs a 'tags' array")?
            .iter()
            .filter_map(|t| t.as_str().map(str::to_string))
            .collect();
        let reason = args["reason"].as_str().unwrap_or("");
        self.brain
            .propose_tags(rel.trim_start_matches("./"), &tags, reason)?;
        Ok(format!(
            "tags proposed for {rel}: [{}] — pending the user's confirmation",
            tags.join(", ")
        ))
    }

    // ─── propose_change ─────────────────────────────────────────────────────

    async fn propose_change(&self, args: &Value) -> Result<String> {
        if self.mode() == Mode::Plan {
            bail!("propose_change is disabled in plan mode — present your plan instead");
        }
        let raw_path = args["path"]
            .as_str()
            .context("propose_change needs a 'path'")?;
        let new_content = args["new_content"]
            .as_str()
            .context("propose_change needs 'new_content'")?;
        let reason = args["reason"].as_str().unwrap_or("").to_string();
        self.record_proposal(raw_path, new_content, reason).await
    }

    // ─── edit_file ────────────────────────────────────────────────────────────

    /// Splice a string replacement into an existing file (see [`super::edit`]),
    /// then record the result as a normal change proposal so it flows through
    /// the same diff/accept path as `propose_change`.
    async fn edit_file(&self, args: &Value) -> Result<String> {
        if self.mode() == Mode::Plan {
            bail!("edit_file is disabled in plan mode — present your plan instead");
        }
        let raw_path = args["path"].as_str().context("edit_file needs a 'path'")?;
        let old = args["old"].as_str().context("edit_file needs 'old'")?;
        let new = args["new"].as_str().context("edit_file needs 'new'")?;
        let replace_all = args["replace_all"].as_bool().unwrap_or(false);
        let reason = args["reason"].as_str().unwrap_or("").to_string();

        // The file must exist and have been read this session — a string edit
        // against unseen content is a guess.
        let abs = self.resolve(raw_path)?;
        if !abs.is_file() {
            bail!("'{raw_path}' is not a file — use propose_change to create a new file");
        }
        let rel = raw_path.trim_start_matches("./");
        if !self.read_files.lock().unwrap().contains(rel) {
            bail!("read '{raw_path}' first, then edit — I won't edit a file I haven't seen this session");
        }

        let current = std::fs::read_to_string(&abs)
            .with_context(|| format!("failed to read '{raw_path}'"))?;
        let updated = super::edit::replace(&current, old, new, replace_all)?;
        self.record_proposal(raw_path, &updated, reason).await
    }

    // ─── shared change recording ──────────────────────────────────────────────

    /// Turn a resolved (path, full new content) into a pending change proposal:
    /// snapshot the original, write the proposal and a unified diff under
    /// `.ciabatta/ai/suggestions/`, apply immediately in auto-accept mode, and
    /// record it for the front end. Shared by `propose_change` and `edit_file`.
    async fn record_proposal(&self, raw_path: &str, new_content: &str, reason: String) -> Result<String> {
        let mode = self.mode();
        // Confine the target to the workspace or /tmp (it may not exist yet).
        let target = self.resolve_new(raw_path)?;
        // Display relative paths for workspace files, absolute for /tmp scratch.
        let shown = match target.strip_prefix(&self.bounds[0]) {
            Ok(rel) => rel.display().to_string(),
            Err(_) => target.display().to_string(),
        };

        let mut created = false;
        let old_content = match std::fs::read_to_string(&target) {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                created = true;
                String::new()
            }
            Err(e) => return Err(e).with_context(|| format!("failed to read '{shown}'")),
        };
        if old_content == new_content {
            return Ok(format!("no change — '{shown}' already has exactly that content"));
        }

        // Keep the proposal and a pre-change snapshot out of the working tree.
        // The snapshot key strips any leading '/' so an absolute /tmp target
        // still nests safely under the suggestions directory.
        let store = self
            .root
            .join(crate::config::CIABATTA_DIR)
            .join("ai")
            .join("suggestions");
        let key = shown.trim_start_matches('/');
        let proposed = store.join(key);
        let original = store.join(format!("{key}.orig"));
        if let Some(parent) = proposed.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&proposed, new_content)?;
        std::fs::write(&original, &old_content)?;

        let diff = unified_diff(&original, &proposed, &shown).await;

        let applied = mode == Mode::AutoAccept;
        if applied {
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&target, new_content)
                .with_context(|| format!("failed to write '{shown}'"))?;
        }

        self.note_touch(&shown);
        self.changes.lock().unwrap().push(ChangeSuggestion {
            file: shown.clone(),
            target,
            original,
            proposed,
            diff,
            reason,
            created,
            state: if applied { ChangeState::Applied } else { ChangeState::Pending },
        });

        Ok(if applied {
            format!("change applied to {shown} (auto-accept mode); the user saw the diff")
        } else {
            format!(
                "change proposed for {shown} — the user sees it as a diff and must accept it \
                 before it lands. Do NOT assume it is applied; if they reject it, they will \
                 tell you what to do differently."
            )
        })
    }

    // ─── sandbox_run ────────────────────────────────────────────────────────

    async fn sandbox_run(&self, args: &Value) -> Result<String> {
        let image = args["image"].as_str().context("sandbox_run needs an 'image'")?;
        let command = args["command"]
            .as_str()
            .context("sandbox_run needs a 'command'")?;

        let images = self.sandbox_images();
        if !images.iter().any(|i| i == image) {
            bail!(
                "image '{image}' is not in the configured sandbox list ({}). \
                 The user controls this list via `images` under [ai] in ciabatta.toml.",
                if images.is_empty() { "empty".to_string() } else { images.join(", ") }
            );
        }

        let runtime = crate::config::resolve_container_cmd(&self.config)
            .context("no container runtime available for the sandbox")?;

        let root = self.root.display().to_string();
        let output = tokio::process::Command::new(&runtime)
            .args([
                "run",
                "--rm",
                "--network=none",
                "-v",
                &format!("{root}:/workspace:ro"),
                "-w",
                "/workspace",
                image,
                "sh",
                "-c",
                command,
            ])
            .output()
            .await
            .with_context(|| format!("failed to run {runtime}"))?;

        let mut out = String::new();
        out.push_str(&String::from_utf8_lossy(&output.stdout));
        let stderr = String::from_utf8_lossy(&output.stderr);
        if !stderr.trim().is_empty() {
            out.push_str("\n[stderr]\n");
            out.push_str(&stderr);
        }
        out.push_str(&format!("\n[exit status: {}]", output.status));
        Ok(clip(&out, 16_000))
    }

    // ─── run_command ──────────────────────────────────────────────────────────

    /// Run a shell command locally in the project root, so the assistant can use
    /// the machine's real toolchains (cargo, python, node, …) to build and test.
    /// Unlike `sandbox_run` this is not isolated and can modify the working tree.
    async fn run_command(&self, args: &Value) -> Result<String> {
        let command = args["command"]
            .as_str()
            .context("run_command needs a 'command'")?;

        let fut = tokio::process::Command::new("sh")
            .arg("-c")
            .arg(command)
            .current_dir(&self.root)
            .output();
        // Cap runtime so a hung build/test can't wedge the whole agent loop.
        let output = match tokio::time::timeout(std::time::Duration::from_secs(300), fut).await {
            Ok(res) => res.with_context(|| format!("failed to run: {command}"))?,
            Err(_) => bail!("command timed out after 300s: {command}"),
        };

        let mut out = String::new();
        out.push_str(&String::from_utf8_lossy(&output.stdout));
        let stderr = String::from_utf8_lossy(&output.stderr);
        if !stderr.trim().is_empty() {
            out.push_str("\n[stderr]\n");
            out.push_str(&stderr);
        }
        out.push_str(&format!("\n[exit status: {}]", output.status));
        Ok(clip(&out, 16_000))
    }

    // ─── deps ─────────────────────────────────────────────────────────────────

    /// Traverse the dependency graph captured in the brain. With no query,
    /// summarize every internal package and its third-party dependency count;
    /// with one, resolve it (a file resolves to its owning package) and list
    /// what flows in (dependencies) and out (consumers / publish points).
    fn deps(&self, args: &Value) -> Result<String> {
        use crate::analyze::{Category, Node};

        let Some(graph) = self.brain.dependencies() else {
            bail!(
                "no dependency graph yet — run a burn-in (`/burn`) or `/analyze` to build it \
                 (or `ciabatta ai burn-in` / `ciabatta analyze` from the shell)"
            );
        };
        // "name" or "name x.y.z" for a node.
        let label = |n: &Node| match &n.version {
            Some(v) => format!("{} {v}", n.label),
            None => n.label.clone(),
        };
        let query = args["query"].as_str().unwrap_or("").trim();

        if query.is_empty() {
            let internals: Vec<&Node> = graph
                .nodes
                .iter()
                .filter(|n| n.category == Category::Internal && n.id != "int:root")
                .collect();
            let external = graph.nodes.iter().filter(|n| n.category == Category::External).count();
            let mut out = format!(
                "Dependency graph: {} internal package(s), {external} external dependency(ies). \
                 Query a file, package, or dependency name for detail.\n",
                internals.len()
            );
            for pkg in internals {
                let deps: Vec<String> = graph
                    .inputs(&pkg.id)
                    .into_iter()
                    .filter(|n| n.category == Category::External)
                    .map(&label)
                    .collect();
                let pubs: Vec<String> = graph
                    .outputs(&pkg.id)
                    .into_iter()
                    .filter(|n| n.category == Category::Publish)
                    .map(|n| n.label.clone())
                    .collect();
                out.push_str(&format!(
                    "\n• {}{}",
                    pkg.label,
                    if pkg.is_workspace { " (workspace root)" } else { "" }
                ));
                if !deps.is_empty() {
                    out.push_str(&format!("\n    depends on ({}): {}", deps.len(), join_capped(&deps, 25)));
                }
                if !pubs.is_empty() {
                    out.push_str(&format!("\n    publishes to: {}", pubs.join(", ")));
                }
            }
            return Ok(clip(&out, 12_000));
        }

        // Focused query: resolve a node by name, or a file path to its package.
        let (node, via_file) = match graph.find_node(query) {
            Some(n) => (n, false),
            None => match graph.owner_for_file(query) {
                Some(n) => (n, true),
                None => bail!(
                    "nothing in the dependency graph matches '{query}' — try a package name, \
                     dependency name, or a file path"
                ),
            },
        };

        let mut out = String::new();
        if via_file {
            out.push_str(&format!("File '{query}' belongs to package '{}'.\n", node.label));
        }
        out.push_str(&format!(
            "{} · {:?}{}",
            node.label,
            node.category,
            node.version.as_deref().map(|v| format!(" {v}")).unwrap_or_default()
        ));
        if let Some(eco) = &node.ecosystem {
            out.push_str(&format!(" · {eco}"));
        }
        out.push_str(
            "\n(inputs = what it depends on / flows in · outputs = what depends on it / \
             publish points)",
        );

        let tagged = |n: &Node| format!("{} [{:?}]", label(n), n.category);
        let inputs: Vec<String> = graph
            .inputs(&node.id)
            .into_iter()
            .filter(|n| n.id != "int:root")
            .map(&tagged)
            .collect();
        let outputs: Vec<String> = graph
            .outputs(&node.id)
            .into_iter()
            .filter(|n| n.id != "int:root")
            .map(&tagged)
            .collect();

        if !inputs.is_empty() {
            out.push_str(&format!("\n  inputs ({}): {}", inputs.len(), join_capped(&inputs, 40)));
        }
        if !outputs.is_empty() {
            out.push_str(&format!("\n  outputs ({}): {}", outputs.len(), join_capped(&outputs, 40)));
        }
        if inputs.is_empty() && outputs.is_empty() {
            out.push_str("\n  (no dependency edges recorded for this node)");
        }
        if !node.vulnerabilities.is_empty() {
            let ids: Vec<String> = node.vulnerabilities.iter().map(|v| v.id.clone()).collect();
            out.push_str(&format!("\n  ⚠ known vulnerabilities: {}", ids.join(", ")));
        }
        Ok(clip(&out, 12_000))
    }
}

/// Join items with commas, capping the count with a "+N more" suffix.
fn join_capped(items: &[String], max: usize) -> String {
    if items.len() <= max {
        items.join(", ")
    } else {
        format!("{}, +{} more", items[..max].join(", "), items.len() - max)
    }
}

/// Unified diff between two files via the system `diff` (labels rendered as
/// `a/<rel>` / `b/<rel>`, git-style). Falls back to a note if diff is missing.
async fn unified_diff(original: &Path, proposed: &Path, rel: &str) -> String {
    let out = tokio::process::Command::new("diff")
        .arg("-u")
        .arg("--label")
        .arg(format!("a/{rel}"))
        .arg("--label")
        .arg(format!("b/{rel}"))
        .arg(original)
        .arg(proposed)
        .output()
        .await;
    match out {
        Ok(o) if !o.stdout.is_empty() => clip(&String::from_utf8_lossy(&o.stdout), 10_000),
        _ => format!(
            "(no diff tool available — proposed content saved at {})",
            proposed.display()
        ),
    }
}

/// Resolve the deepest existing ancestor of `path` (following symlinks) and
/// re-append the not-yet-existing components. Used to bounds-check a file that
/// is about to be created.
fn resolve_existing_prefix(path: &Path) -> PathBuf {
    let mut existing = path.to_path_buf();
    let mut tail: Vec<std::ffi::OsString> = Vec::new();
    while !existing.exists() {
        let Some(name) = existing.file_name().map(|n| n.to_os_string()) else {
            break;
        };
        tail.push(name);
        existing = existing.parent().map(Path::to_path_buf).unwrap_or_default();
        if existing.as_os_str().is_empty() {
            break;
        }
    }
    let mut base = existing.canonicalize().unwrap_or(existing);
    for name in tail.into_iter().rev() {
        base.push(name);
    }
    base
}

/// Lexically resolve `.` and `..` in an absolute path without touching disk,
/// so a `..` in a to-be-created path can't escape its bounds. Never pops past
/// the root.
fn normalize_lexical(path: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for comp in path.components() {
        match comp {
            Component::ParentDir => {
                // Only climb if the last kept component is a normal directory.
                if out.components().next_back().is_some_and(|c| matches!(c, Component::Normal(_))) {
                    out.pop();
                }
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Pick the best available search binary: ripgrep, then ack, then grep — the
/// standard tools, in preference order.
fn search_command(pattern: &str, scope: &Path) -> (String, Vec<String>) {
    let scope = scope.display().to_string();
    if binary_on_path("rg") {
        (
            "rg".into(),
            vec![
                "--line-number".into(),
                "--no-heading".into(),
                "--max-count=50".into(),
                "--max-filesize=1M".into(),
                pattern.into(),
                scope,
            ],
        )
    } else if binary_on_path("ack") {
        ("ack".into(), vec!["--nogroup".into(), "-H".into(), pattern.into(), scope])
    } else {
        (
            "grep".into(),
            vec![
                "-rn".into(),
                "-I".into(),
                "--exclude-dir=.git".into(),
                "--exclude-dir=target".into(),
                "--exclude-dir=node_modules".into(),
                "-E".into(),
                pattern.into(),
                scope,
            ],
        )
    }
}

fn binary_on_path(name: &str) -> bool {
    let Some(paths) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&paths).any(|dir| dir.join(name).is_file())
}

/// Strip the absolute project prefix out of tool output so the model (and the
/// mind map) always deals in relative paths.
fn relativize(text: &str, root: &Path) -> String {
    let prefix = format!("{}/", root.display());
    text.replace(&prefix, "")
}

/// Directories nobody wants the assistant crawling.
const SKIP_DIRS: &[&str] = &[
    ".git",
    "target",
    "node_modules",
    ".venv",
    "venv",
    "__pycache__",
    "dist",
    "build",
    ".idea",
    ".vscode",
];

/// Every project file (relative paths, sorted), skipping build output and
/// VCS internals. Shared by the `list_files` tool and the burn-in traversal.
pub fn project_files(root: &Path) -> Vec<String> {
    let mut files = Vec::new();
    walk(root, root, &mut files, 0);
    files.sort();
    files
}

fn walk(root: &Path, dir: &Path, out: &mut Vec<String>, depth: usize) {
    if depth > 12 || out.len() > 5_000 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if path.is_dir() {
            if SKIP_DIRS.contains(&name.as_str()) || name.starts_with('.') && name != ".ciabatta" {
                continue;
            }
            walk(root, &path, out, depth + 1);
        } else if let Ok(rel) = path.strip_prefix(root) {
            out.push(rel.display().to_string());
        }
    }
}

/// Cap tool output so a huge file or noisy search can't blow up the context.
fn clip(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut cut = max;
    while !s.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{}\n… [truncated: {} of {} bytes shown]", &s[..cut], cut, s.len())
}

/// Render a plan as a checklist, one line per step.
pub fn render_plan(items: &[PlanItem]) -> String {
    items
        .iter()
        .map(|i| format!("{} {}", i.status.glyph(), i.step))
        .collect::<Vec<_>>()
        .join("\n")
}

fn join_tags(v: &Value) -> String {
    v.as_array()
        .map(|a| {
            a.iter()
                .filter_map(|t| t.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::CiabattaConfig;

    fn toolbox() -> (PathBuf, ToolBox) {
        let root = std::env::temp_dir().join(format!(
            "ciabatta-tools-test-{}-{:?}",
            std::process::id(),
            std::thread::current().id(),
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/a.rs"), "fn a() {}\n").unwrap();
        let brain = Arc::new(Brain::open(&root).unwrap());
        let tb = ToolBox::new(root.clone(), brain, CiabattaConfig::default());
        (root, tb)
    }

    #[test]
    fn resolve_allows_workspace_and_tmp_but_refuses_outside() {
        let (root, tb) = toolbox();

        // Workspace file resolves.
        assert!(tb.resolve("src/a.rs").is_ok());

        // A real /tmp file resolves (the assistant's scratch space).
        let tmp_file =
            std::env::temp_dir().join(format!("ciabatta-bounds-{}.txt", std::process::id()));
        std::fs::write(&tmp_file, "scratch").unwrap();
        assert!(tb.resolve(tmp_file.to_str().unwrap()).is_ok());
        let _ = std::fs::remove_file(&tmp_file);

        // Climbing out of the workspace is refused.
        assert!(tb.resolve("../../etc/passwd").is_err());

        // An absolute path outside workspace and /tmp is refused.
        assert!(tb.resolve("/etc/passwd").is_err());

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn resolve_new_confines_creatable_paths() {
        let (root, tb) = toolbox();

        // A new file inside the workspace is allowed.
        assert!(tb.resolve_new("src/new_module.rs").is_ok());

        // A new file under /tmp is allowed.
        assert!(tb.resolve_new("/tmp/ciabatta-bespoke.txt").is_ok());

        // `..` in a not-yet-existing path can't climb out to a forbidden root.
        // (The test workspace itself lives under /tmp, so we climb clear past it
        // to /etc, which is outside both allowed roots.)
        assert!(tb.resolve_new("../../../../../../../../etc/evil.rs").is_err());
        assert!(tb.resolve_new("/etc/evil.conf").is_err());

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn normalize_lexical_resolves_dotdot_without_underflow() {
        assert_eq!(normalize_lexical(Path::new("/tmp/../etc/x")), PathBuf::from("/etc/x"));
        assert_eq!(normalize_lexical(Path::new("/a/b/../c")), PathBuf::from("/a/c"));
        // Never climbs past the root.
        assert_eq!(normalize_lexical(Path::new("/../../x")), PathBuf::from("/x"));
    }

    #[test]
    fn update_plan_records_steps_and_rejects_double_in_progress() {
        let (root, tb) = toolbox();

        // A well-formed plan is stored and rendered as a checklist.
        let out = tb
            .update_plan(&json!({"steps": [
                {"step": "read the config", "status": "completed"},
                {"step": "add the flag", "status": "in_progress"},
                {"step": "test it", "status": "pending"}
            ]}))
            .unwrap();
        assert!(out.contains("[x] read the config"));
        assert!(out.contains("[~] add the flag"));
        assert_eq!(tb.plan().len(), 3);

        // Two in-progress steps are refused.
        assert!(
            tb.update_plan(&json!({"steps": [
                {"step": "a", "status": "in_progress"},
                {"step": "b", "status": "in_progress"}
            ]}))
            .is_err()
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn edit_file_requires_a_prior_read_then_applies() {
        let (root, tb) = toolbox();
        tb.set_mode(Mode::AutoAccept); // apply immediately so we can assert on disk
        let rt = tokio::runtime::Runtime::new().unwrap();

        // Editing before reading is refused.
        let args = json!({"path": "src/a.rs", "old": "fn a() {}", "new": "fn a() { todo!() }"});
        let err = rt.block_on(tb.edit_file(&args)).unwrap_err();
        assert!(err.to_string().contains("read"));

        // Read it, then the same edit lands.
        tb.read_file(&json!({"path": "src/a.rs"})).unwrap();
        rt.block_on(tb.edit_file(&args)).unwrap();
        let content = std::fs::read_to_string(root.join("src/a.rs")).unwrap();
        assert!(content.contains("todo!()"));

        let _ = std::fs::remove_dir_all(&root);
    }
}
