//! Burn-in: teach the assistant a codebase in one pass.
//!
//! `ciabatta ai burn-in` traverses the project and builds the architecture
//! mind map without waiting for questions to trickle knowledge in:
//!
//!   1. **Survey** — the model sees the file tree plus the heads of the
//!      README/manifests and names the project's architecture parts (each
//!      with a one-line description).
//!   2. **Traverse** — source files go to the model in batches (path + the
//!      first lines of each); it tags every file into one or more of those
//!      architectures.
//!
//! Tags apply to the map immediately by default (watch them appear live on
//! the mind-map page); with `--review` they queue as pending proposals
//! instead, rendered as ghost nodes until you accept or reject them.

use std::io::Read;
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use serde_json::Value;
use tokio::sync::mpsc;

use super::provider::Turn;
use super::{AiEvent, Assistant, server, tools};

/// Files per model request. Big enough to amortize the round trip, small
/// enough that every file's head fits comfortably.
const BATCH_SIZE: usize = 15;
/// How much of each file the model sees.
const HEAD_LINES: usize = 30;
const HEAD_BYTES: usize = 1600;
/// Cap on the paths listed in the survey prompt.
const SURVEY_PATHS: usize = 500;

/// Run the burn-in from the CLI: spawn the live map, stream progress to
/// stdout, then keep the map up until Ctrl-C so the result can be explored.
pub async fn run(
    assistant: Arc<Assistant>,
    root: &Path,
    port: u16,
    review: bool,
    limit: Option<usize>,
) -> Result<()> {
    let jobs = super::jobs::Jobs::open(root, &assistant.toolbox.config)?;
    let url = server::spawn(assistant.clone(), jobs, port).await?;
    println!("🔥 Burn-in: teaching the assistant this codebase");
    println!("   provider: {}", assistant.provider.label());
    println!("   live map: {url}");
    println!(
        "   mode:     {}\n",
        if review {
            "review — tags wait for your confirmation (ghosts on the map)"
        } else {
            "apply — tags land on the map as they're determined"
        }
    );

    // Drain the burn's progress events to stdout while it runs.
    let (tx, mut rx) = mpsc::channel::<AiEvent>(64);
    let printer = tokio::spawn(async move {
        while let Some(ev) = rx.recv().await {
            if let AiEvent::Status(s) = ev {
                println!("  {s}");
            }
        }
    });

    let summary = burn_core(&assistant, root, review, limit, &tx).await;
    drop(tx);
    let _ = printer.await;
    let summary = summary?;

    println!("\n{summary}");
    println!("\nThe map stays live at {url} — press Ctrl-C to finish.");
    tokio::signal::ctrl_c().await?;
    Ok(())
}

/// The burn-in itself: survey the project, then tag every source file batch by
/// batch, streaming human-readable progress as `AiEvent::Status`. Returns a
/// final summary line. Shared by the CLI (`run`) and the chat `/burn` command,
/// neither of which this function assumes — it spawns no server and never
/// blocks on Ctrl-C.
pub async fn burn_core(
    assistant: &Assistant,
    root: &Path,
    review: bool,
    limit: Option<usize>,
    events: &mpsc::Sender<AiEvent>,
) -> Result<String> {
    let mut files = source_files(root);
    if let Some(n) = limit {
        files.truncate(n);
    }
    anyhow::ensure!(
        !files.is_empty(),
        "no source files found under {} to analyze",
        root.display()
    );

    // ── 0. static analysis: fold the dependency graph into the knowledge map ─
    let _ = events
        .send(AiEvent::Progress(super::BurnProgress {
            phase: super::BurnPhase::Dependencies,
            done: 0,
            total: 0,
            tagged: 0,
            detail: "static dependency analysis…".to_string(),
        }))
        .await;
    match scan_dependencies(assistant, root, events).await {
        Ok(summary) => {
            let _ = events.send(AiEvent::Status(summary)).await;
        }
        Err(e) => {
            let _ = events
                .send(AiEvent::Status(format!("dependency scan skipped ({e})")))
                .await;
        }
    }

    // ── 1. survey: name the architecture parts ──────────────────────────────
    assistant.set_activity(Some(format!(
        "burn-in: surveying the project layout ({} files)",
        files.len()
    )));
    let _ = events
        .send(AiEvent::Status(format!(
            "surveying the project layout ({} files)…",
            files.len()
        )))
        .await;
    let _ = events
        .send(AiEvent::Progress(super::BurnProgress {
            phase: super::BurnPhase::Survey,
            done: 0,
            total: 0,
            tagged: 0,
            detail: format!(
                "reading {} files to name the architecture parts…",
                files.len()
            ),
        }))
        .await;
    match survey(assistant, root, &files).await {
        Ok(arches) => {
            for (name, description) in &arches {
                assistant.brain.set_architecture(name, description)?;
                let _ = events
                    .send(AiEvent::Status(format!("◆ {name} — {description}")))
                    .await;
            }
        }
        // The survey improves tag consistency but isn't load-bearing: the
        // traversal can still mint architecture names on its own.
        Err(e) => {
            let _ = events
                .send(AiEvent::Status(format!(
                    "survey failed ({e}) — continuing with per-file tagging"
                )))
                .await;
        }
    }

    // ── 2. traverse: tag every file, batch by batch ─────────────────────────
    let total_batches = files.len().div_ceil(BATCH_SIZE);
    let _ = events
        .send(AiEvent::Status(format!(
            "tagging {} files in {total_batches} batches…",
            files.len()
        )))
        .await;
    let mut tagged = 0usize;
    let mut failed_batches = 0usize;
    for (i, batch) in files.chunks(BATCH_SIZE).enumerate() {
        assistant.set_activity(Some(format!(
            "burn-in: analyzing files {}–{} of {}",
            i * BATCH_SIZE + 1,
            (i * BATCH_SIZE + batch.len()),
            files.len()
        )));
        // Announce the batch BEFORE the (often slow) model call, so the live
        // panel shows what's in flight instead of looking stalled.
        let _ = events
            .send(AiEvent::Progress(super::BurnProgress {
                phase: super::BurnPhase::Tagging,
                done: i,
                total: total_batches,
                tagged,
                detail: format!("batch {}/{total_batches}: {}", i + 1, batch[0]),
            }))
            .await;
        match tag_batch(assistant, root, batch, review).await {
            Ok(n) => {
                tagged += n;
                let _ = events
                    .send(AiEvent::Status(format!(
                        "[{}/{total_batches}] {} → {n} tagged",
                        i + 1,
                        batch[0]
                    )))
                    .await;
            }
            Err(e) => {
                failed_batches += 1;
                let _ = events
                    .send(AiEvent::Status(format!(
                        "[{}/{total_batches}] batch failed: {e}",
                        i + 1
                    )))
                    .await;
            }
        }
        // Advance the bar once the batch has landed.
        let _ = events
            .send(AiEvent::Progress(super::BurnProgress {
                phase: super::BurnPhase::Tagging,
                done: i + 1,
                total: total_batches,
                tagged,
                detail: format!("{tagged} file(s) tagged so far"),
            }))
            .await;
    }
    assistant.set_activity(None);
    let _ = events
        .send(AiEvent::Progress(super::BurnProgress {
            phase: super::BurnPhase::Done,
            done: total_batches,
            total: total_batches,
            tagged,
            detail: "building the summary…".to_string(),
        }))
        .await;

    // ── summary ─────────────────────────────────────────────────────────────
    let arch_count = assistant.brain.graph_json()["nodes"]
        .as_array()
        .map(|n| n.iter().filter(|v| v["kind"] == "architecture").count())
        .unwrap_or(0);
    let mut summary = format!(
        "Burn-in complete: {tagged} of {} files tagged across {arch_count} architectures{}.",
        files.len(),
        if failed_batches > 0 {
            format!(" ({failed_batches} batch(es) failed)")
        } else {
            String::new()
        }
    );
    if review {
        let pending = assistant.brain.pending().len();
        summary.push_str(&format!(
            "\n{pending} tag proposal(s) await review — accept or reject them on the map \
             or with Ctrl-Y / Ctrl-N."
        ));
    }
    Ok(summary)
}

/// Run ciabatta's static dependency analysis and fold the resulting graph into
/// the knowledge graph (the brain), so the assistant — and the `deps` tool —
/// can traverse third-party and cross-package dependencies. Returns a one-line
/// summary of what was captured. Shared by burn-in and the `/analyze` command.
pub async fn scan_dependencies(
    assistant: &Assistant,
    root: &Path,
    events: &mpsc::Sender<AiEvent>,
) -> Result<String> {
    use crate::analyze::Category;

    let _ = events
        .send(AiEvent::Status(
            "scanning dependencies (static analysis)…".to_string(),
        ))
        .await;

    // Run the (synchronous) analysis off the async worker so a large repo scan
    // doesn't stall other tasks on this runtime thread.
    let root = root.to_path_buf();
    let config = assistant.toolbox.config.clone();
    let graph = tokio::task::spawn_blocking(move || {
        crate::analyze::analyze_quiet(
            &root,
            &config,
            &crate::analyze::RequirementInputs::default(),
        )
    })
    .await
    .context("dependency analysis task panicked")??;

    let internal = graph
        .nodes
        .iter()
        .filter(|n| n.category == Category::Internal && n.id != "int:root")
        .count();
    let external = graph
        .nodes
        .iter()
        .filter(|n| n.category == Category::External)
        .count();
    let publish = graph
        .nodes
        .iter()
        .filter(|n| n.category == Category::Publish)
        .count();
    let files = graph.files.len();

    assistant.brain.set_dependencies(graph)?;

    Ok(format!(
        "static analysis added to the mind map: {internal} internal package(s), {external} \
         external dependency(ies), {publish} publish point(s), {files} manifest/source file(s) \
         scanned — traverse them with the `deps` tool"
    ))
}

/// Ask the model to name the project's architecture parts from the file tree
/// and manifest heads. Returns `(name, description)` pairs.
async fn survey(
    assistant: &Assistant,
    root: &Path,
    files: &[String],
) -> Result<Vec<(String, String)>> {
    let mut prompt = String::from("Project file tree (truncated):\n");
    for f in files.iter().take(SURVEY_PATHS) {
        prompt.push_str(f);
        prompt.push('\n');
    }
    for manifest in [
        "README.md",
        "Cargo.toml",
        "package.json",
        "pyproject.toml",
        "go.mod",
        "pom.xml",
    ] {
        if let Some(head) = file_head(&root.join(manifest)) {
            prompt.push_str(&format!("\n--- {manifest} (head) ---\n{head}\n"));
        }
    }
    prompt.push_str(
        "\nName the architecture parts of this software: the 3–10 major areas a \
         developer would recognize (e.g. 'cli', 'auth', 'deploy', 'frontend'). \
         Use short, lowercase, reusable names. Reply with ONLY a JSON array: \
         [{\"name\": \"...\", \"description\": \"one line on what it is\"}]",
    );

    let system = "You analyze codebases and answer with strict JSON only — no prose, \
                  no markdown fences.";
    let turn = assistant
        .provider
        .chat(system, &[Turn::User(prompt)], &[])
        .await?;
    let items = extract_json_array(&turn.text)
        .context("the model did not return a JSON array of architectures")?;

    let mut out = Vec::new();
    for item in items {
        let name = item["name"].as_str().unwrap_or("").trim().to_lowercase();
        if name.is_empty() {
            continue;
        }
        let description = item["description"]
            .as_str()
            .unwrap_or("")
            .trim()
            .to_string();
        out.push((name, description));
    }
    anyhow::ensure!(!out.is_empty(), "the survey returned no architectures");
    Ok(out)
}

/// Tag one batch of files: send their heads, apply (or queue) the returned
/// tags. Returns how many files were tagged.
async fn tag_batch(
    assistant: &Assistant,
    root: &Path,
    batch: &[String],
    review: bool,
) -> Result<usize> {
    let known: Vec<String> = assistant.brain.graph_json()["nodes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|n| n["kind"] == "architecture")
        .filter_map(|n| n["label"].as_str().map(str::to_string))
        .collect();

    let mut prompt = format!(
        "Known architectures: [{}]. Prefer these tags; introduce a new lowercase \
         tag only when a file clearly belongs to an area not listed.\n\n",
        known.join(", ")
    );
    for path in batch {
        let head = file_head(&root.join(path)).unwrap_or_default();
        prompt.push_str(&format!("--- {path} ---\n{head}\n\n"));
    }
    prompt.push_str(
        "Tag each file with the architecture(s) it belongs to (1–3 tags per file; \
         a file can belong to several). Reply with ONLY a JSON array: \
         [{\"path\": \"...\", \"tags\": [\"...\"], \"reason\": \"one short line\"}]",
    );

    let system = "You classify source files into a codebase's architecture areas and \
                  answer with strict JSON only — no prose, no markdown fences.";
    let turn = assistant
        .provider
        .chat(system, &[Turn::User(prompt)], &[])
        .await?;
    let items = extract_json_array(&turn.text)
        .context("the model did not return a JSON array of file tags")?;

    let mut tagged = 0;
    for item in items {
        let Some(path) = item["path"].as_str() else {
            continue;
        };
        // Only accept paths we actually sent — the map must never gain
        // entries the model invented.
        if !batch.iter().any(|b| b == path) {
            continue;
        }
        let tags: Vec<String> = item["tags"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|t| t.as_str().map(str::to_string))
            .take(3)
            .collect();
        if tags.is_empty() {
            continue;
        }
        let reason = item["reason"].as_str().unwrap_or("");
        let applied = if review {
            assistant.brain.propose_tags(path, &tags, reason)
        } else {
            assistant.brain.install_tags(path, &tags)
        };
        if applied.is_ok() {
            tagged += 1;
        }
    }
    Ok(tagged)
}

// ─── helpers ─────────────────────────────────────────────────────────────────

/// Extensions (and exact names) worth showing the model during burn-in.
const SOURCE_EXTS: &[&str] = &[
    "rs", "go", "py", "js", "jsx", "ts", "tsx", "mjs", "cjs", "java", "kt", "kts", "rb", "php",
    "c", "h", "cc", "cpp", "hpp", "cs", "swift", "m", "scala", "sh", "bash", "zsh", "ps1", "sql",
    "html", "css", "scss", "vue", "svelte", "toml", "yaml", "yml", "ini", "cfg", "conf", "md",
    "proto", "tf",
];
const SOURCE_NAMES: &[&str] = &["Dockerfile", "Makefile", "Justfile", "Rakefile"];

/// The project's analyzable source files, in walk order.
fn source_files(root: &Path) -> Vec<String> {
    tools::project_files(root)
        .into_iter()
        .filter(|p| {
            let name = p.rsplit('/').next().unwrap_or(p);
            if name.ends_with(".lock") || name.ends_with("-lock.json") || name.ends_with(".min.js")
            {
                return false;
            }
            if SOURCE_NAMES.contains(&name) {
                return true;
            }
            name.rsplit_once('.')
                .map(|(_, ext)| SOURCE_EXTS.contains(&ext.to_lowercase().as_str()))
                .unwrap_or(false)
        })
        .collect()
}

/// The first lines of a file (bounded in bytes and lines); `None` when the
/// file can't be read or is binary-ish.
fn file_head(path: &Path) -> Option<String> {
    let mut buf = vec![0u8; HEAD_BYTES];
    let mut file = std::fs::File::open(path).ok()?;
    let n = file.read(&mut buf).ok()?;
    buf.truncate(n);
    if buf.contains(&0) {
        return None; // binary
    }
    let text = String::from_utf8_lossy(&buf);
    let mut head: Vec<&str> = text.lines().take(HEAD_LINES).collect();
    // A byte-bounded read usually cuts the last line mid-way; drop it.
    if n == HEAD_BYTES && head.len() > 1 {
        head.pop();
    }
    Some(head.join("\n"))
}

/// Pull the first JSON array out of a model reply, tolerating markdown fences
/// and surrounding prose.
fn extract_json_array(text: &str) -> Option<Vec<Value>> {
    let start = text.find('[')?;
    let end = text.rfind(']')?;
    if end <= start {
        return None;
    }
    serde_json::from_str::<Value>(&text[start..=end])
        .ok()?
        .as_array()
        .cloned()
}

#[cfg(test)]
mod tests {
    use super::extract_json_array;

    #[test]
    fn extracts_array_from_fenced_and_prosey_replies() {
        let fenced = "Here you go:\n```json\n[{\"path\": \"a.rs\", \"tags\": [\"cli\"]}]\n```";
        let items = extract_json_array(fenced).unwrap();
        assert_eq!(items[0]["path"], "a.rs");

        let bare = "[{\"name\": \"auth\", \"description\": \"login\"}]";
        assert_eq!(extract_json_array(bare).unwrap().len(), 1);

        assert!(extract_json_array("no json here").is_none());
    }
}
