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
}

impl ChangeState {
    pub fn label(self) -> &'static str {
        match self {
            ChangeState::Pending => "pending",
            ChangeState::Applied => "applied",
            ChangeState::Rejected => "rejected",
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
    pub state: ChangeState,
}

/// Everything tool execution needs: the project, the brain, and the config.
pub struct ToolBox {
    pub root: PathBuf,
    pub brain: Arc<Brain>,
    pub config: CiabattaConfig,
    /// Relative paths of files touched during the current question, used for
    /// the precision component of the feedback loop.
    pub files_touched: std::sync::Mutex<std::collections::BTreeSet<String>>,
    /// The assistant's working mode (plan / edit / auto-accept), which gates
    /// what `propose_change` is allowed to do.
    mode: std::sync::Mutex<Mode>,
    /// Change proposals for this session, oldest first.
    changes: std::sync::Mutex<Vec<ChangeSuggestion>>,
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
            mode: std::sync::Mutex::new(Mode::default()),
            changes: std::sync::Mutex::new(Vec::new()),
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

    /// The tool specs advertised to the model. In plan mode `propose_change`
    /// is left out entirely, so the model can't even try to edit.
    pub fn specs(&self) -> Vec<ToolSpec> {
        let images = self.sandbox_images();
        let mut specs = vec![
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
        ];
        if self.mode() != Mode::Plan {
            specs.push(ToolSpec {
                name: "propose_change",
                description: "Propose a change to one project file (or a new file) by \
                              giving its COMPLETE new content. The user sees the change \
                              as a diff. In edit mode it is applied only after the user \
                              accepts it — never assume a pending change landed. In \
                              auto-accept mode it is written to the file immediately."
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
            "search_code" => self.search_code(&call.args).await,
            "suggest_files" => self.suggest_files(&call.args),
            "list_files" => self.list_files(&call.args),
            "read_file" => self.read_file(&call.args),
            "tag_file" => self.tag_file(&call.args),
            "propose_change" => self.propose_change(&call.args).await,
            "sandbox_run" => self.sandbox_run(&call.args).await,
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

    /// A one-line human-readable description of a call, for status displays.
    pub fn describe(call: &ToolCall) -> String {
        let arg = |k: &str| call.args[k].as_str().unwrap_or("").to_string();
        match call.name.as_str() {
            "search_code" => format!("🔎 search: {}", arg("pattern")),
            "suggest_files" => format!("🧠 mind map: {}", arg("topic")),
            "list_files" => "🗂  list files".to_string(),
            "read_file" => format!("📄 read: {}", arg("path")),
            "tag_file" => format!("🏷  tag: {} ({})", arg("path"), join_tags(&call.args["tags"])),
            "propose_change" => format!("✏  change: {}", arg("path")),
            "sandbox_run" => format!("📦 sandbox [{}]: {}", arg("image"), arg("command")),
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
        let mode = self.mode();
        if mode == Mode::Plan {
            bail!("propose_change is disabled in plan mode — present your plan instead");
        }
        let raw_path = args["path"]
            .as_str()
            .context("propose_change needs a 'path'")?;
        let new_content = args["new_content"]
            .as_str()
            .context("propose_change needs 'new_content'")?;
        let reason = args["reason"].as_str().unwrap_or("").to_string();

        // Confine the target to the workspace or /tmp (it may not exist yet).
        let target = self.resolve_new(raw_path)?;
        // Display relative paths for workspace files, absolute for /tmp scratch.
        let shown = match target.strip_prefix(&self.bounds[0]) {
            Ok(rel) => rel.display().to_string(),
            Err(_) => target.display().to_string(),
        };

        let old_content = match std::fs::read_to_string(&target) {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
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
}
