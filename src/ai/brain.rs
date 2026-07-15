//! The assistant's per-project memory ("brain").
//!
//! Everything the assistant learns about a project lives in one JSON file,
//! `.ciabatta/ai/brain.json`, so it travels with the project cache and stays
//! hand-editable:
//!
//!   * `confidence` — 1..=100, loosely trained by user feedback. A low score
//!     behaves like a junior dev (cautious, confirms more); a high score like
//!     a senior dev.
//!   * `architectures` — the mind map: each architecture tracks a knowledge
//!     level and a per-file path score. Scores grow as files are used, so the
//!     map converges on "exactly what is needed" for each area.
//!   * `tags` — file → architecture tags (a file can carry several).
//!   * `pending` — AI-proposed tags awaiting user confirmation.
//!   * `feedback` — the raw feedback log the confidence score is derived from.
//!
//! Every mutation persists to disk and bumps a sequence number so the live
//! browser graph can poll cheaply for changes.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// One architecture in the mind map.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Architecture {
    /// How well the assistant knows this architecture; grows with confirmed
    /// tags and file usage.
    #[serde(default)]
    pub knowledge: f64,
    /// One line on what this architecture is (written by the AI during
    /// burn-in; shown in the mind-map tooltips).
    #[serde(default)]
    pub description: String,
    /// Path score per file: the more a file is used under this architecture,
    /// the higher its score (and the earlier it's suggested).
    #[serde(default)]
    pub files: BTreeMap<String, f64>,
}

/// An AI-proposed set of tags for a file, awaiting the user's confirmation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingTag {
    pub file: String,
    pub tags: Vec<String>,
    pub reason: String,
    pub proposed_at: String,
}

/// One user-feedback event; the confidence score is derived from these.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeedbackEntry {
    pub at: String,
    pub positive: bool,
    /// How many files the assistant touched while producing the rated answer.
    /// Fewer, more accurate file choices earn a bigger confidence boost.
    pub files_used: usize,
    #[serde(default)]
    pub note: String,
}

/// A local tool command the assistant has run, remembered so later sessions
/// know how this project builds / tests / lints / formats / runs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolingCommand {
    pub command: String,
    /// Coarse category: build | test | lint | format | run | other.
    pub category: String,
    pub last_run: String,
    /// Whether the command last exited successfully.
    pub last_ok: bool,
    #[serde(default)]
    pub runs: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrainState {
    /// Overall AI ability for this project, 1..=100.
    pub confidence: f64,
    #[serde(default)]
    pub interactions: u64,
    #[serde(default)]
    pub architectures: BTreeMap<String, Architecture>,
    #[serde(default)]
    pub tags: BTreeMap<String, BTreeSet<String>>,
    #[serde(default)]
    pub pending: Vec<PendingTag>,
    #[serde(default)]
    pub feedback: Vec<FeedbackEntry>,
    /// Local tool commands the assistant has run (builds, lints, formats,
    /// tests), remembered across sessions so it reuses what already works.
    #[serde(default)]
    pub commands: Vec<ToolingCommand>,
    /// A snapshot of the project's dependency graph (from `ciabatta analyze`),
    /// captured during burn-in or via `/analyze`, so the assistant can traverse
    /// third-party and cross-package dependencies.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dependencies: Option<crate::analyze::AnalysisGraph>,
}

impl Default for BrainState {
    fn default() -> Self {
        Self {
            confidence: 30.0, // a new assistant starts as a junior dev
            interactions: 0,
            architectures: BTreeMap::new(),
            tags: BTreeMap::new(),
            pending: Vec::new(),
            feedback: Vec::new(),
            commands: Vec::new(),
            dependencies: None,
        }
    }
}

/// The on-disk brain plus a change counter, shareable across the TUI, the
/// agent loop, and the graph server.
pub struct Brain {
    path: PathBuf,
    inner: Mutex<BrainState>,
    seq: AtomicU64,
}

impl Brain {
    /// Open (or create) `.ciabatta/ai/brain.json` under the project root.
    pub fn open(root: &Path) -> Result<Self> {
        let dir = root.join(crate::config::CIABATTA_DIR).join("ai");
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("Failed to create {}", dir.display()))?;
        let path = dir.join("brain.json");

        let state = match std::fs::read_to_string(&path) {
            Ok(s) if s.trim().is_empty() => BrainState::default(),
            Ok(s) => serde_json::from_str(&s)
                .with_context(|| format!("Failed to parse {}", path.display()))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => BrainState::default(),
            Err(e) => return Err(e).with_context(|| format!("Failed to read {}", path.display())),
        };

        Ok(Self {
            path,
            inner: Mutex::new(state),
            seq: AtomicU64::new(1),
        })
    }

    /// The current change sequence number (bumped by every mutation).
    pub fn seq(&self) -> u64 {
        self.seq.load(Ordering::Relaxed)
    }

    /// The current confidence score. Derived from the mind map and user
    /// feedback; it is surfaced in the UI only and is deliberately NOT fed
    /// back into the system prompt (doing so worsened the model's output).
    pub fn confidence(&self) -> f64 {
        self.inner.lock().unwrap().confidence
    }

    /// Run a mutation against the state, then persist and bump the sequence.
    fn mutate<T>(&self, op: impl FnOnce(&mut BrainState) -> T) -> Result<T> {
        let mut state = self.inner.lock().unwrap();
        let out = op(&mut state);
        let jsonned = serde_json::to_string_pretty(&*state)?;
        std::fs::write(&self.path, jsonned)
            .with_context(|| format!("Failed to write {}", self.path.display()))?;
        self.seq.fetch_add(1, Ordering::Relaxed);
        Ok(out)
    }

    /// Record AI-proposed tags for a file as pending user confirmation.
    /// Re-proposing the same file replaces the earlier pending entry.
    pub fn propose_tags(&self, file: &str, tags: &[String], reason: &str) -> Result<()> {
        let file = file.to_string();
        let tags: Vec<String> = tags
            .iter()
            .map(|t| t.trim().to_lowercase())
            .filter(|t| !t.is_empty())
            .collect();
        anyhow::ensure!(!tags.is_empty(), "no tags given");
        self.mutate(|s| {
            s.pending.retain(|p| p.file != file);
            s.pending.push(PendingTag {
                file,
                tags,
                reason: reason.to_string(),
                proposed_at: now(),
            });
        })
    }

    /// A snapshot of the pending tag confirmations.
    pub fn pending(&self) -> Vec<PendingTag> {
        self.inner.lock().unwrap().pending.clone()
    }

    /// Resolve the pending confirmation for `file`. Accepting installs the
    /// tags into the map and grows each architecture's knowledge; rejecting
    /// simply drops the proposal (and nudges confidence down — a wrong guess
    /// is feedback too).
    pub fn confirm(&self, file: &str, accept: bool) -> Result<bool> {
        self.mutate(|s| {
            let Some(idx) = s.pending.iter().position(|p| p.file == file) else {
                return false;
            };
            let p = s.pending.remove(idx);
            if accept {
                let entry = s.tags.entry(p.file.clone()).or_default();
                for tag in &p.tags {
                    entry.insert(tag.clone());
                    let arch = s.architectures.entry(tag.clone()).or_default();
                    arch.knowledge += 1.0;
                    *arch.files.entry(p.file.clone()).or_insert(0.0) += 1.0;
                }
                nudge(&mut s.confidence, 0.4);
            } else {
                nudge(&mut s.confidence, -0.6);
            }
            true
        })
    }

    /// Resolve every pending proposal at once (burn-in review, "accept all").
    /// The confidence nudge is aggregated and capped so a bulk decision can't
    /// swing the score the way per-answer feedback does. Returns how many
    /// proposals were resolved.
    pub fn confirm_all(&self, accept: bool) -> Result<usize> {
        self.mutate(|s| {
            let pending = std::mem::take(&mut s.pending);
            let n = pending.len();
            if accept {
                for p in &pending {
                    let entry = s.tags.entry(p.file.clone()).or_default();
                    for tag in &p.tags {
                        entry.insert(tag.clone());
                        let arch = s.architectures.entry(tag.clone()).or_default();
                        arch.knowledge += 1.0;
                        *arch.files.entry(p.file.clone()).or_insert(0.0) += 1.0;
                    }
                }
            }
            let delta = if accept { 0.4 } else { -0.6 } * n as f64;
            nudge(&mut s.confidence, delta.clamp(-3.0, 3.0));
            n
        })
    }

    /// Create an architecture (or update its description) without touching
    /// files — burn-in's survey pass registers the architecture parts it
    /// found before any file is tagged.
    pub fn set_architecture(&self, name: &str, description: &str) -> Result<()> {
        let name = name.trim().to_lowercase();
        anyhow::ensure!(!name.is_empty(), "empty architecture name");
        self.mutate(|s| {
            let arch = s.architectures.entry(name).or_default();
            if !description.trim().is_empty() {
                arch.description = description.trim().to_string();
            }
        })
    }

    /// Store (or replace) the project dependency graph in the knowledge graph.
    /// Populated by `ciabatta analyze` during burn-in or the `/analyze` command.
    pub fn set_dependencies(&self, graph: crate::analyze::AnalysisGraph) -> Result<()> {
        self.mutate(|s| s.dependencies = Some(graph))
    }

    /// The stored dependency graph, if one has been captured.
    pub fn dependencies(&self) -> Option<crate::analyze::AnalysisGraph> {
        self.inner.lock().unwrap().dependencies.clone()
    }

    /// Install tags directly into the map, bypassing the pending queue and
    /// the confidence nudge. Burn-in uses this: it's the AI teaching itself
    /// breadth, not the user rating answers, so knowledge grows more gently
    /// than a hand-confirmed tag.
    pub fn install_tags(&self, file: &str, tags: &[String]) -> Result<()> {
        let tags: Vec<String> = tags
            .iter()
            .map(|t| t.trim().to_lowercase())
            .filter(|t| !t.is_empty())
            .collect();
        anyhow::ensure!(!tags.is_empty(), "no tags given");
        self.mutate(|s| {
            let entry = s.tags.entry(file.to_string()).or_default();
            for tag in tags {
                entry.insert(tag.clone());
                let arch = s.architectures.entry(tag).or_default();
                arch.knowledge += 0.5;
                *arch.files.entry(file.to_string()).or_insert(0.0) += 1.0;
            }
        })
    }

    /// Record that a file was used (read/suggested-and-accepted) during work.
    /// Usage strengthens the file's path score in every architecture it's
    /// tagged with, and slightly deepens those architectures' knowledge.
    pub fn record_file_use(&self, file: &str) -> Result<()> {
        self.mutate(|s| {
            let tags: Vec<String> = s.tags.get(file).into_iter().flatten().cloned().collect();
            for tag in tags {
                let arch = s.architectures.entry(tag).or_default();
                *arch.files.entry(file.to_string()).or_insert(0.0) += 0.25;
                arch.knowledge += 0.05;
            }
        })
    }

    /// Count one completed interaction (a question answered).
    pub fn record_interaction(&self) -> Result<()> {
        self.mutate(|s| s.interactions += 1)
    }

    /// Remember that a local tool `command` (classified into `category`) was
    /// run, and whether it succeeded — so later sessions know how this project
    /// builds, lints, formats, and tests. Re-running a known command updates it
    /// in place; the memory is bounded so it can't grow without limit.
    pub fn record_command(&self, command: &str, category: &str, success: bool) -> Result<()> {
        let command = command.trim().to_string();
        if command.is_empty() {
            return Ok(());
        }
        let category = category.trim().to_lowercase();
        self.mutate(|s| {
            if let Some(existing) = s.commands.iter_mut().find(|c| c.command == command) {
                existing.category = category;
                existing.last_run = now();
                existing.last_ok = success;
                existing.runs += 1;
            } else {
                s.commands.push(ToolingCommand {
                    command,
                    category,
                    last_run: now(),
                    last_ok: success,
                    runs: 1,
                });
                // Keep the memory bounded: drop the oldest entries past a cap.
                let len = s.commands.len();
                if len > 100 {
                    s.commands.drain(0..len - 100);
                }
            }
        })
    }

    /// Whether the mind map already knows a file (it has tags or a path score
    /// under some architecture). Used to gauge how "related" a change is.
    pub fn knows_file(&self, file: &str) -> bool {
        let s = self.inner.lock().unwrap();
        s.tags.contains_key(file) || s.architectures.values().any(|a| a.files.contains_key(file))
    }

    /// Architecture tags carried by other files in the SAME directory as
    /// `file`, most common first — used to suggest tags for a newly
    /// created or as-yet-untagged file so the mind map keeps up with changes.
    pub fn sibling_tags(&self, file: &str) -> Vec<String> {
        let dir = parent_dir(file);
        let s = self.inner.lock().unwrap();
        let mut counts: BTreeMap<String, usize> = BTreeMap::new();
        for (f, tags) in &s.tags {
            if f == file || parent_dir(f) != dir {
                continue;
            }
            for t in tags {
                *counts.entry(t.clone()).or_insert(0) += 1;
            }
        }
        let mut ranked: Vec<(String, usize)> = counts.into_iter().collect();
        ranked.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        ranked.into_iter().map(|(t, _)| t).take(3).collect()
    }

    /// Record user feedback on the assistant's last answer and retrain the
    /// confidence score.
    ///
    /// Positive feedback moves confidence toward 100; the move is bigger when
    /// the assistant used few files (i.e. chose precisely). Negative feedback
    /// decays it proportionally, so a "senior" assistant that starts missing
    /// falls back toward junior quickly.
    pub fn record_feedback(&self, positive: bool, files_used: usize, note: &str) -> Result<f64> {
        self.mutate(|s| {
            s.feedback.push(FeedbackEntry {
                at: now(),
                positive,
                files_used,
                note: note.to_string(),
            });
            if positive {
                let mut gain = (100.0 - s.confidence) * 0.08;
                // Precision bonus: answering from a handful of well-chosen
                // files is the behavior we want to reinforce.
                if (1..=5).contains(&files_used) {
                    gain += (100.0 - s.confidence) * 0.04;
                }
                s.confidence += gain.max(0.5);
            } else {
                s.confidence -= (s.confidence * 0.10).max(1.0);
            }
            s.confidence = s.confidence.clamp(1.0, 100.0);
            s.confidence
        })
    }

    /// Mind-map lookup: rank known files for a topic. Architecture names and
    /// tags matching the topic words contribute their path scores; the best
    /// matches come back first. This is the "exactly what is needed" path —
    /// grep is the "everything" path.
    pub fn suggest(&self, topic: &str, limit: usize) -> Vec<(String, f64, Vec<String>)> {
        let state = self.inner.lock().unwrap();
        let words: Vec<String> = topic
            .to_lowercase()
            .split(|c: char| !c.is_alphanumeric())
            .filter(|w| w.len() > 1)
            .map(str::to_string)
            .collect();

        let mut scores: BTreeMap<String, f64> = BTreeMap::new();
        for (name, arch) in &state.architectures {
            let name_lc = name.to_lowercase();
            let hit = words
                .iter()
                .any(|w| name_lc.contains(w.as_str()) || w.contains(&name_lc));
            if !hit {
                continue;
            }
            // Weight each file by its path score, scaled by how deep the
            // architecture's knowledge runs.
            let depth = 1.0 + (arch.knowledge / 10.0).min(2.0);
            for (file, score) in &arch.files {
                *scores.entry(file.clone()).or_insert(0.0) += score * depth;
            }
        }
        // File paths that literally mention a topic word get a small boost.
        for (file, score) in scores.iter_mut() {
            let f = file.to_lowercase();
            if words.iter().any(|w| f.contains(w.as_str())) {
                *score += 1.0;
            }
        }

        let mut ranked: Vec<(String, f64)> = scores.into_iter().collect();
        ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        ranked
            .into_iter()
            .take(limit)
            .map(|(file, score)| {
                let tags = state.tags.get(&file).into_iter().flatten().cloned().collect();
                (file, score, tags)
            })
            .collect()
    }

    /// Prune a file from the map entirely: its tags, its path score in every
    /// architecture, and any pending proposal for it. Returns whether the map
    /// knew the file at all.
    pub fn forget_file(&self, file: &str) -> Result<bool> {
        self.mutate(|s| {
            let mut known = s.tags.remove(file).is_some();
            for arch in s.architectures.values_mut() {
                known |= arch.files.remove(file).is_some();
            }
            let before = s.pending.len();
            s.pending.retain(|p| p.file != file);
            known | (s.pending.len() != before)
        })
    }

    /// Prune an architecture and all its file links (files keep any other
    /// tags they carry). Returns whether the architecture existed.
    pub fn forget_architecture(&self, name: &str) -> Result<bool> {
        let name = name.trim().to_lowercase();
        self.mutate(|s| {
            let known = s.architectures.remove(&name).is_some();
            s.tags.retain(|_, tags| {
                tags.remove(&name);
                !tags.is_empty()
            });
            for p in &mut s.pending {
                p.tags.retain(|t| t != &name);
            }
            s.pending.retain(|p| !p.tags.is_empty());
            known
        })
    }

    /// Remove one tag from one file (and the file's path score in that
    /// architecture). Returns whether the link existed.
    pub fn untag_file(&self, file: &str, tag: &str) -> Result<bool> {
        let tag = tag.trim().to_lowercase();
        self.mutate(|s| {
            let mut known = false;
            if let Some(tags) = s.tags.get_mut(file) {
                known = tags.remove(&tag);
                if tags.is_empty() {
                    s.tags.remove(file);
                }
            }
            if let Some(arch) = s.architectures.get_mut(&tag) {
                known |= arch.files.remove(file).is_some();
            }
            known
        })
    }

    /// A compact plain-text summary of the mind map for the system prompt.
    pub fn summary_for_prompt(&self) -> String {
        let state = self.inner.lock().unwrap();
        let mut out = String::new();
        if state.architectures.is_empty() {
            out.push_str("The architecture map is empty — nothing has been tagged yet.\n");
        } else {
            for (name, arch) in &state.architectures {
                let mut files: Vec<(&String, &f64)> = arch.files.iter().collect();
                files.sort_by(|a, b| b.1.partial_cmp(a.1).unwrap_or(std::cmp::Ordering::Equal));
                let top: Vec<String> = files
                    .iter()
                    .take(6)
                    .map(|(f, s)| format!("{f} ({s:.1})"))
                    .collect();
                out.push_str(&format!(
                    "- {name} (knowledge {:.1}): {}\n",
                    arch.knowledge,
                    top.join(", ")
                ));
            }
        }
        out.push_str(&tooling_overview(&state.commands));
        out.push_str(&dependency_overview(state.dependencies.as_ref()));
        out
    }

    /// The full graph as JSON for the live browser view: architecture and
    /// file nodes, edges weighted by path score, plus confidence and any
    /// pending confirmations.
    pub fn graph_json(&self) -> Value {
        let state = self.inner.lock().unwrap();

        let mut nodes: Vec<Value> = Vec::new();
        let mut edges: Vec<Value> = Vec::new();
        let mut seen_files: BTreeSet<String> = BTreeSet::new();
        let mut seen_arches: BTreeSet<String> = BTreeSet::new();

        for (name, arch) in &state.architectures {
            seen_arches.insert(name.clone());
            nodes.push(json!({
                "id": format!("arch:{name}"),
                "label": name,
                "kind": "architecture",
                "knowledge": arch.knowledge,
                "description": arch.description,
            }));
            for (file, score) in &arch.files {
                seen_files.insert(file.clone());
                edges.push(json!({
                    "from": format!("arch:{name}"),
                    "to": format!("file:{file}"),
                    "score": score,
                }));
            }
        }
        for file in &seen_files {
            nodes.push(json!({
                "id": format!("file:{file}"),
                "label": file,
                "kind": "file",
                "tags": state.tags.get(file).into_iter().flatten().collect::<Vec<_>>(),
            }));
        }

        // Pending proposals appear as ghosts: provisional nodes and dashed
        // edges the user hasn't confirmed yet, so a burn-in in review mode is
        // visible on the map as it happens.
        for p in &state.pending {
            if seen_files.insert(p.file.clone()) {
                nodes.push(json!({
                    "id": format!("file:{}", p.file),
                    "label": p.file,
                    "kind": "file",
                    "tags": p.tags,
                    "provisional": true,
                }));
            }
            for tag in &p.tags {
                if seen_arches.insert(tag.clone()) {
                    nodes.push(json!({
                        "id": format!("arch:{tag}"),
                        "label": tag,
                        "kind": "architecture",
                        "knowledge": 0.0,
                        "provisional": true,
                    }));
                }
                edges.push(json!({
                    "from": format!("arch:{tag}"),
                    "to": format!("file:{}", p.file),
                    "score": 0.5,
                    "provisional": true,
                }));
            }
        }

        let mut graph = json!({
            "seq": self.seq(),
            "confidence": state.confidence,
            "interactions": state.interactions,
            "nodes": nodes,
            "edges": edges,
            "pending": state.pending,
        });
        // Expose the captured dependency graph under its own key so the browser
        // view / API can traverse dependencies without disturbing the mind-map
        // nodes and edges above.
        if let Some(dep) = &state.dependencies {
            graph["dependencies"] = serde_json::to_value(dep).unwrap_or(Value::Null);
        }
        graph
    }
}

/// A compact dependency-graph summary for the system prompt: package counts
/// and each internal package's third-party dependency count. Empty when no
/// dependency graph has been captured yet.
fn dependency_overview(graph: Option<&crate::analyze::AnalysisGraph>) -> String {
    use crate::analyze::Category;
    let Some(g) = graph else {
        return String::new();
    };
    let internals: Vec<&crate::analyze::Node> = g
        .nodes
        .iter()
        .filter(|n| n.category == Category::Internal && n.id != "int:root")
        .collect();
    let ext = g.nodes.iter().filter(|n| n.category == Category::External).count();
    let mut out = format!(
        "\nDependency graph (use the `deps` tool to traverse it): {} internal package(s), \
         {ext} external dependency(ies).\n",
        internals.len()
    );
    for pkg in internals.iter().take(12) {
        let dep_count = g
            .inputs(&pkg.id)
            .into_iter()
            .filter(|n| n.category == Category::External)
            .count();
        out.push_str(&format!(
            "  - {} ({dep_count} dep{})\n",
            pkg.label,
            if dep_count == 1 { "" } else { "s" }
        ));
    }
    if internals.len() > 12 {
        out.push_str(&format!("  … and {} more package(s)\n", internals.len() - 12));
    }
    out
}

/// A compact list of remembered project commands for the system prompt, so the
/// assistant reuses the build/lint/format/test invocations it already learned
/// instead of guessing. Empty when nothing has been run yet.
fn tooling_overview(commands: &[ToolingCommand]) -> String {
    if commands.is_empty() {
        return String::new();
    }
    let mut out =
        String::from("\nRemembered project commands (reuse these to build/lint/format/test):\n");
    let mut shown = 0;
    for cat in ["build", "test", "lint", "format", "run", "other"] {
        let mut cmds: Vec<&ToolingCommand> = commands.iter().filter(|c| c.category == cat).collect();
        if cmds.is_empty() {
            continue;
        }
        // Prefer commands that last succeeded, then the most-run ones.
        cmds.sort_by(|a, b| b.last_ok.cmp(&a.last_ok).then(b.runs.cmp(&a.runs)));
        let list: Vec<String> = cmds
            .iter()
            .take(3)
            .map(|c| format!("`{}`{}", c.command, if c.last_ok { "" } else { " (last failed)" }))
            .collect();
        out.push_str(&format!("  - {cat}: {}\n", list.join(", ")));
        shown += 1;
    }
    if shown == 0 { String::new() } else { out }
}

/// The directory portion of a relative path (everything before the last `/`),
/// or "" for a top-level file. Shared by the sibling-tag lookup.
fn parent_dir(path: &str) -> &str {
    match path.rfind('/') {
        Some(i) => &path[..i],
        None => "",
    }
}

/// Nudge confidence by a small delta, keeping it in 1..=100.
fn nudge(confidence: &mut f64, delta: f64) {
    *confidence = (*confidence + delta).clamp(1.0, 100.0);
}

fn now() -> String {
    chrono::Local::now().to_rfc3339()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_brain() -> (tempdir::TempDir, Brain) {
        let dir = tempdir::TempDir::new();
        let brain = Brain::open(dir.path()).unwrap();
        (dir, brain)
    }

    // A minimal temp-dir helper so the tests don't need a new dependency.
    mod tempdir {
        use std::path::{Path, PathBuf};

        pub struct TempDir(PathBuf);

        impl TempDir {
            pub fn new() -> Self {
                let path = std::env::temp_dir().join(format!(
                    "ciabatta-brain-test-{}-{:?}",
                    std::process::id(),
                    std::thread::current().id(),
                ));
                std::fs::create_dir_all(&path).unwrap();
                TempDir(path)
            }
            pub fn path(&self) -> &Path {
                &self.0
            }
        }

        impl Drop for TempDir {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
    }

    #[test]
    fn confirm_installs_tags_and_scores() {
        let (_dir, brain) = temp_brain();
        brain
            .propose_tags("src/auth.rs", &["auth".into(), "backend".into()], "handles login")
            .unwrap();
        assert_eq!(brain.pending().len(), 1);

        assert!(brain.confirm("src/auth.rs", true).unwrap());
        assert!(brain.pending().is_empty());

        let suggestions = brain.suggest("how does auth work", 5);
        assert_eq!(suggestions[0].0, "src/auth.rs");
        assert!(suggestions[0].2.contains(&"auth".to_string()));
    }

    #[test]
    fn usage_increases_path_score() {
        let (_dir, brain) = temp_brain();
        brain.propose_tags("src/a.rs", &["core".into()], "").unwrap();
        brain.confirm("src/a.rs", true).unwrap();
        let before = brain.suggest("core", 1)[0].1;
        brain.record_file_use("src/a.rs").unwrap();
        brain.record_file_use("src/a.rs").unwrap();
        let after = brain.suggest("core", 1)[0].1;
        assert!(after > before, "usage should grow the path score");
    }

    #[test]
    fn feedback_trains_confidence_both_ways() {
        let (_dir, brain) = temp_brain();
        let start = brain.confidence();
        let up = brain.record_feedback(true, 2, "good answer").unwrap();
        assert!(up > start);
        let down = brain.record_feedback(false, 12, "missed").unwrap();
        assert!(down < up);
        assert!((1.0..=100.0).contains(&down));
    }

    #[test]
    fn pruning_forgets_files_tags_and_architectures() {
        let (_dir, brain) = temp_brain();
        brain
            .propose_tags("src/a.rs", &["core".into(), "ui".into()], "")
            .unwrap();
        brain.confirm("src/a.rs", true).unwrap();
        brain.propose_tags("src/b.rs", &["core".into()], "").unwrap();
        brain.confirm("src/b.rs", true).unwrap();

        // Untag: a.rs keeps 'core' but loses 'ui'.
        assert!(brain.untag_file("src/a.rs", "ui").unwrap());
        let suggestions = brain.suggest("core", 5);
        let tags = &suggestions.iter().find(|s| s.0 == "src/a.rs").unwrap().2;
        assert!(!tags.contains(&"ui".to_string()));

        // Forget a file: it disappears from every suggestion.
        assert!(brain.forget_file("src/b.rs").unwrap());
        assert!(brain.suggest("core", 5).iter().all(|s| s.0 != "src/b.rs"));

        // Forget an architecture: nothing suggests for it anymore.
        assert!(brain.forget_architecture("core").unwrap());
        assert!(brain.suggest("core", 5).is_empty());

        // Pruning the unknown reports false.
        assert!(!brain.forget_file("nope.rs").unwrap());
        assert!(!brain.forget_architecture("nope").unwrap());
    }

    #[test]
    fn remembers_tooling_commands_and_surfaces_them() {
        let (_dir, brain) = temp_brain();
        brain.record_command("cargo build", "build", true).unwrap();
        brain.record_command("cargo clippy", "lint", false).unwrap();
        // Re-running a known command updates it in place, not a duplicate.
        brain.record_command("cargo build", "build", true).unwrap();

        let prompt = brain.summary_for_prompt();
        assert!(prompt.contains("Remembered project commands"));
        assert!(prompt.contains("cargo build"));
        // A last-failed command is flagged as such.
        assert!(prompt.contains("cargo clippy` (last failed)"));

        let state = brain.inner.lock().unwrap();
        assert_eq!(state.commands.len(), 2);
        assert_eq!(state.commands.iter().find(|c| c.command == "cargo build").unwrap().runs, 2);
    }

    #[test]
    fn sibling_tags_are_drawn_from_the_same_directory() {
        let (_dir, brain) = temp_brain();
        brain.propose_tags("src/ai/tui.rs", &["ai".into(), "ui".into()], "").unwrap();
        brain.confirm("src/ai/tui.rs", true).unwrap();
        brain.propose_tags("src/ai/brain.rs", &["ai".into()], "").unwrap();
        brain.confirm("src/ai/brain.rs", true).unwrap();
        brain.propose_tags("frontend/app.tsx", &["frontend".into()], "").unwrap();
        brain.confirm("frontend/app.tsx", true).unwrap();

        // A new file under src/ai inherits its siblings' tags, not frontend's.
        let tags = brain.sibling_tags("src/ai/new_thing.rs");
        assert!(tags.contains(&"ai".to_string()));
        assert!(!tags.contains(&"frontend".to_string()));

        assert!(brain.knows_file("src/ai/tui.rs"));
        assert!(!brain.knows_file("src/ai/new_thing.rs"));
    }

    #[test]
    fn rejecting_a_proposal_drops_it() {
        let (_dir, brain) = temp_brain();
        brain.propose_tags("src/b.rs", &["ui".into()], "").unwrap();
        assert!(brain.confirm("src/b.rs", false).unwrap());
        assert!(brain.pending().is_empty());
        assert!(brain.suggest("ui", 5).is_empty());
    }
}
