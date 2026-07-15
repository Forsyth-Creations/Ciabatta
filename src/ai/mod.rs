//! `ciabatta ai` — an AI assistant that learns your codebase.
//!
//! The assistant is a small daemon plus two front ends:
//!
//!   * an HTTP server (the "AI assistant" daemon) that renders the live
//!     architecture mind map in the browser and exposes a JSON API
//!   * a Ratatui chat TUI (the default way to talk to it)
//!   * `ciabatta ai ask "..."` for one-shot questions from the shell
//!
//! As it works, the assistant tags files with architecture labels (pending
//! your confirmation), builds a file→architecture map whose path scores grow
//! with use, and keeps a 1–100 confidence score trained by your feedback —
//! low scores behave like a junior dev, high scores like a senior dev. All of
//! that state lives in `.ciabatta/ai/brain.json`.

pub mod brain;
pub mod burnin;
pub mod compaction;
pub mod edit;
pub mod pdf;
pub mod jobs;
pub mod provider;
pub mod server;
pub mod session;
pub mod tools;
pub mod tui;

use std::io::Write as _;
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use tokio::sync::mpsc;

use crate::config::{AiConfig, CIABATTA_DIR, CONFIG_FILE, CiabattaConfig};
use brain::Brain;
use provider::{Provider, Turn};
use session::Conversation;
use tools::ToolBox;

/// Progress events emitted while the agent loop runs, consumed by whichever
/// front end is active (TUI, CLI, or the HTTP API).
#[derive(Debug, Clone)]
pub enum AiEvent {
    /// A tool is being executed (one line, already human-readable).
    Status(String),
    /// The assistant proposed a code change (diff included), for the front
    /// end to display — and, in edit mode, to collect the user's verdict on.
    Suggestion(tools::ChangeSuggestion),
    /// The assistant's working plan changed (the full checklist), for the front
    /// end to render live progress on a multi-step task.
    Plan(Vec<tools::PlanItem>),
    /// Structured progress for a long batch job (burn-in), driving a dedicated
    /// progress panel. Distinct from `Status`, which is free-form log text.
    Progress(BurnProgress),
    /// The final answer for the current question.
    Answer(String),
    /// The loop failed.
    Error(String),
}

/// Which phase of the burn-in is running, for the live progress panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BurnPhase {
    /// Static dependency analysis (before any model calls).
    Dependencies,
    /// The survey pass that names the architecture parts.
    Survey,
    /// The per-file tagging batches.
    Tagging,
    /// Everything is finished.
    Done,
}

impl BurnPhase {
    /// A short human-readable label for the panel.
    pub fn label(self) -> &'static str {
        match self {
            BurnPhase::Dependencies => "scanning dependencies",
            BurnPhase::Survey => "surveying architecture",
            BurnPhase::Tagging => "tagging files",
            BurnPhase::Done => "complete",
        }
    }
}

/// A snapshot of the burn-in's progress, so the front end can show exactly what
/// is happening (which phase, how far through, how many files tagged) rather
/// than a bare spinner that reads as a freeze on a slow model.
#[derive(Debug, Clone)]
pub struct BurnProgress {
    pub phase: BurnPhase,
    /// Batches completed and the total. `total == 0` means indeterminate (the
    /// dependency/survey phases, which are a single long step).
    pub done: usize,
    pub total: usize,
    /// Files tagged so far across the whole run.
    pub tagged: usize,
    /// One short line on the current step (e.g. the file being analyzed).
    pub detail: String,
}

/// How much freedom the assistant has to change code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    /// Research only: no change proposals; answers end in a concrete plan.
    Plan,
    /// Changes are proposed as diffs and wait for the user to accept each one.
    #[default]
    Edit,
    /// Proposed changes are written to the working tree immediately.
    AutoAccept,
}

impl Mode {
    pub fn label(self) -> &'static str {
        match self {
            Mode::Plan => "plan",
            Mode::Edit => "edit",
            Mode::AutoAccept => "auto-accept",
        }
    }

    /// The next mode in the Shift-Tab cycle: plan → edit → auto-accept → plan.
    pub fn next(self) -> Self {
        match self {
            Mode::Plan => Mode::Edit,
            Mode::Edit => Mode::AutoAccept,
            Mode::AutoAccept => Mode::Plan,
        }
    }

    pub fn parse(s: &str) -> Result<Self> {
        match s.trim().to_lowercase().as_str() {
            "plan" => Ok(Mode::Plan),
            "edit" => Ok(Mode::Edit),
            "auto" | "auto-accept" | "autoaccept" => Ok(Mode::AutoAccept),
            other => anyhow::bail!("unknown mode '{other}' (expected plan, edit, or auto)"),
        }
    }
}

/// The assistant: a provider, its tools, and the shared brain.
pub struct Assistant {
    pub provider: Provider,
    pub toolbox: Arc<ToolBox>,
    pub brain: Arc<Brain>,
    /// The current conversation, persisted to `.ciabatta/ai/conversations/`
    /// after every answer so it can be resumed later.
    conversation: tokio::sync::Mutex<Conversation>,
    /// One line on what the assistant is doing right now (burn-in progress
    /// etc.), surfaced live on the mind-map page's status pill.
    activity: std::sync::Mutex<Option<String>>,
}

/// Cap on model⇄tool round trips per question, so a confused model can't spin.
const MAX_TOOL_ROUNDS: usize = 20;

/// Cap on fix-then-reverify cycles after a code change, so a task whose
/// verification can't be made to pass can't loop forever or exhaust the round
/// budget. After this many failed verifications the answer is accepted as-is.
const MAX_VERIFY_ATTEMPTS: usize = 3;

/// System prompt for generating a short conversation title (adapted from
/// opencode). The model must reply with only the title, one line.
const TITLE_SYSTEM: &str = "You generate a short title for a coding conversation, to help the user \
    find it later. Output ONLY the title: a single line, at most 50 characters, no quotes, no \
    trailing punctuation, no explanation. Focus on what the user wants to do. Keep filenames, \
    technical terms, and numbers exact. Never mention tools. Always output something meaningful.";

impl Assistant {
    pub fn new(root: &Path, config: &CiabattaConfig) -> Result<Arc<Self>> {
        Self::with_conversation(root, config, Conversation::new(root))
    }

    /// Build an assistant that continues an existing conversation.
    pub fn with_conversation(
        root: &Path,
        config: &CiabattaConfig,
        conversation: Conversation,
    ) -> Result<Arc<Self>> {
        let ai_cfg = config.ai.clone().unwrap_or_default();
        let provider = Provider::from_config(&ai_cfg)?;
        let brain = Arc::new(Brain::open(root)?);
        let toolbox = Arc::new(ToolBox::new(
            root.to_path_buf(),
            brain.clone(),
            config.clone(),
        ));
        Ok(Arc::new(Self {
            provider,
            toolbox,
            brain,
            conversation: tokio::sync::Mutex::new(conversation),
            activity: std::sync::Mutex::new(None),
        }))
    }

    /// The conversation transcript so far (a clone of its turns), for a front
    /// end resuming a session to replay onto the screen.
    pub async fn transcript(&self) -> Vec<Turn> {
        self.conversation.lock().await.turns.clone()
    }

    /// The id the current conversation persists under.
    pub async fn conversation_id(&self) -> String {
        self.conversation.lock().await.id.clone()
    }

    /// Abandon the current conversation and start a fresh one, keeping the same
    /// brain and mind map. Returns the new conversation id. Used by `/new`.
    pub async fn start_new_conversation(&self) -> String {
        let mut conv = self.conversation.lock().await;
        *conv = Conversation::new(&self.toolbox.root);
        conv.id.clone()
    }

    /// The current working mode (plan / edit / auto-accept).
    pub fn mode(&self) -> Mode {
        self.toolbox.mode()
    }

    /// Switch the working mode; takes effect on the next model round.
    pub fn set_mode(&self, mode: Mode) {
        self.toolbox.set_mode(mode);
    }

    /// Set (or clear) the live activity line shown on the mind-map page.
    pub fn set_activity(&self, line: Option<String>) {
        *self.activity.lock().unwrap() = line;
    }

    /// The current activity line, if any.
    pub fn activity(&self) -> Option<String> {
        self.activity.lock().unwrap().clone()
    }

    /// The system prompt, rebuilt per question so it always reflects the
    /// current mind map.
    fn system_prompt(&self) -> String {
        let mode_rules = match self.mode() {
            Mode::Plan => {
                "MODE: plan. Research only — read, search, and tag, but do NOT propose \
                 code changes (the tool is unavailable). Finish with a concrete numbered \
                 plan: for each step, the file and what would change and why."
            }
            Mode::Edit => {
                "MODE: edit. Make code changes with `propose_change`, giving the complete \
                 new file content. Each change is shown to the user as a diff and lands \
                 only after they accept it; if they reject one, they will tell you why — \
                 take that guidance and re-propose. Briefly explain every change."
            }
            Mode::AutoAccept => {
                "MODE: auto-accept. Changes you make with `propose_change` are written to \
                 the working tree immediately. Be surgical: keep every file consistent \
                 and compilable, and summarize what you changed."
            }
        };
        format!(
            "You are the ciabatta AI assistant: an AI pair developer embedded in a \
             command-line tool, working inside one project on the user's machine.\n\
             \n\
             {mode_rules}\n\
             \n\
             How to work:\n\
             1. Prefer `suggest_files` (the architecture mind map) to locate code: it returns \
                exactly what is needed. Use `search_code` (grep/ripgrep) when the map is thin \
                or you need everything that mentions a term. To understand a data structure that \
                spans files — an object built by composition, whose fields are themselves types \
                declared elsewhere — use `find_definition` on the type to open its declaration, \
                then call `find_definition` again on each field's type and repeat, following the \
                structure across files instead of guessing its shape. To follow third-party or \
                cross-package dependencies, use `deps` — it traverses the dependency graph built \
                by static analysis (which package a file belongs to, a package's dependencies, \
                and what depends on a given library).\n\
             2. As you traverse files, call `tag_file` to record which architecture(s) each \
                file belongs to (a file can have several tags). The user confirms tags, so \
                keep them short, lowercase, and reusable (e.g. 'auth', 'frontend', 'deploy').\n\
             3. For any task that takes 3+ distinct steps, call `update_plan` to lay out a \
                checklist, and update it as you go — one step 'in_progress' at a time, and \
                mark steps done the moment they are actually done. Skip it for simple \
                questions.\n\
             4. To change an existing file, use `edit_file` (replace just the text that \
                changes) — do NOT re-emit the whole file. Reserve `propose_change` for new \
                files or a genuine full rewrite. Read a file before you edit it. Never create \
                a file when editing an existing one will do. Only touch files that are part of \
                the current task — a change to a file you never read and the map doesn't know \
                is flagged to the user as unrelated, so don't edit off-task files.\n\
             5. To build, test, lint, or run the project for real, use `run_command` — it runs \
                locally in the project root with the machine's installed toolchains (cargo, \
                python, node, npm, make, …). Reserve `sandbox_run` for untrusted or throwaway \
                experiments in an isolated container.\n\
             6. After you change code, VERIFY before you claim to be done: run the project's \
                build/tests with `run_command` and fix anything that breaks. When a change \
                touches a symbol, search for every other use of it (call sites, tests, docs, \
                error paths) and update them too — a task is finished only when the whole \
                project still builds and passes. If verification keeps failing you will be \
                told; use its output to find and fix the real cause.\n\
             7. When several tool calls are independent (e.g. reading three files), issue \
                them in ONE turn so they run together; only serialize calls that depend on an \
                earlier result.\n\
             \n\
             Judgment: prioritize technical accuracy over agreement. If the user is mistaken \
             or an approach is flawed, say so plainly and explain why — respectful correction \
             beats false validation. When unsure, investigate with the tools before \
             asserting; don't guess.\n\
             \n\
             Answers: keep answers grounded in files you actually read, and cite paths as \
             `path:line`. Be concise — lead with the answer, then supporting detail. Your \
             output is shown in a terminal; use plain, compact Markdown.\n\
             \n\
             Boundary: you may only read and write files inside this project workspace and \
             /tmp (your scratch space for bespoke, throwaway files). Anything outside those \
             is off-limits and tool calls to it will be refused.\n\
             \n\
             Current architecture map:\n{map}",
            map = self.brain.summary_for_prompt(),
        )
    }

    /// Answer one question, streaming progress over `events`. Tool calls are
    /// executed locally between model rounds until the model stops calling
    /// tools. The final answer is both sent as an event and returned.
    pub async fn ask(&self, question: &str, events: mpsc::Sender<AiEvent>) -> Result<String> {
        self.toolbox.reset_touched();
        let specs = self.toolbox.specs();

        let mut conversation = self.conversation.lock().await;
        let history = &mut conversation.turns;
        history.push(Turn::User(question.to_string()));

        // Keep long sessions inside the context window: fold older turns into a
        // summary before we start spending model rounds on this question.
        if let Some(note) = compaction::maybe_compact(&self.provider, history).await {
            let _ = events.send(AiEvent::Status(note)).await;
        }

        let mut answer = String::new();
        // The number of change proposals before this question, so we can tell
        // whether the model actually touched code (and thus needs verifying).
        let changes_at_start = self.toolbox.changes_len();
        // Bounded fix-then-reverify cycles, so a task that can't be made to pass
        // can't burn the whole tool-round budget or loop forever.
        let mut verify_attempts = 0usize;
        for round in 0..MAX_TOOL_ROUNDS {
            let turn = match self.provider.chat(&self.system_prompt(), history, &specs).await {
                Ok(t) => t,
                Err(e) => {
                    // Drop the failed exchange so the session stays usable.
                    history.pop();
                    // `{:#}` walks the whole context chain, so the UI shows the
                    // underlying cause (timeout, refused connection, bad body)
                    // rather than just a generic "request failed".
                    let _ = events.send(AiEvent::Error(format!("{e:#}"))).await;
                    return Err(e);
                }
            };

            if turn.refused || turn.tool_calls.is_empty() {
                // The model wants to finish. If it changed code this session,
                // prove the project still builds/tests before we accept the
                // answer — and if it doesn't, hand the failure back so the model
                // fixes it rather than shipping a broken tree.
                let changed = self.toolbox.changes_len() > changes_at_start;
                if changed
                    && verify_attempts < MAX_VERIFY_ATTEMPTS
                    && let Some(cmd) = self.toolbox.verify_command()
                {
                    verify_attempts += 1;
                    let _ = events
                        .send(AiEvent::Status(format!("verifying changes: {cmd}")))
                        .await;
                    let (passed, output) = self.toolbox.run_verify(&cmd).await;
                    if !passed {
                        let _ = events
                            .send(AiEvent::Status(format!(
                                "verification failed — asking the assistant to fix it \
                                 (attempt {verify_attempts}/{MAX_VERIFY_ATTEMPTS})"
                            )))
                            .await;
                        // Keep the model's turn in history, then feed the
                        // failure back as a user message (a synthetic tool
                        // result would have no matching tool_use and break the
                        // provider's request shape).
                        history.push(Turn::Assistant(turn));
                        history.push(Turn::User(format!(
                            "The verification command `{cmd}` failed after your changes. You are \
                             not done. Read the output below, find EVERY place affected \
                             (including other call sites, tests, and error paths), fix them, and \
                             continue until it passes.\n\n=== `{cmd}` output ===\n{output}"
                        )));
                        continue;
                    }
                    let _ = events
                        .send(AiEvent::Status("verification passed".to_string()))
                        .await;
                }
                answer = turn.text.clone();
                history.push(Turn::Assistant(turn));
                break;
            }

            let calls = turn.tool_calls.clone();
            history.push(Turn::Assistant(turn));

            let changes_before = self.toolbox.changes_len();
            for call in &calls {
                let _ = events.send(AiEvent::Status(ToolBox::describe(call))).await;
            }
            // Independent read-only calls in one round can run together; anything
            // that mutates state (edits, tags, the plan, the sandbox) stays
            // sequential so ordering and diffs are deterministic.
            let results = if calls.len() > 1 && calls.iter().all(|c| ToolBox::is_read_only(&c.name)) {
                futures::future::join_all(calls.iter().map(|c| self.toolbox.execute(c))).await
            } else {
                let mut results = Vec::with_capacity(calls.len());
                for call in &calls {
                    results.push(self.toolbox.execute(call).await);
                }
                results
            };
            // Surface any change proposals this round produced, diff and all.
            for suggestion in self.toolbox.changes_since(changes_before) {
                let _ = events.send(AiEvent::Suggestion(suggestion)).await;
            }
            // If the plan changed this round, push the updated checklist.
            if calls.iter().any(|c| c.name == "update_plan") {
                let _ = events.send(AiEvent::Plan(self.toolbox.plan())).await;
            }
            history.push(Turn::ToolResults(results));

            if round == MAX_TOOL_ROUNDS - 1 {
                answer = "I hit the tool-call limit before finishing — try narrowing the \
                          question."
                    .to_string();
            }
        }

        let _ = self.brain.record_interaction();
        let _ = events.send(AiEvent::Answer(answer.clone())).await;

        // On the first exchange, generate a concise title so the conversation is
        // easy to find in `ciabatta ai resume`. Best-effort: a failure just
        // leaves the fallback (first user line) that `save` derives.
        let is_first = conversation
            .turns
            .iter()
            .filter(|t| matches!(t, Turn::User(_)))
            .count()
            == 1;
        if is_first
            && conversation.title.is_empty()
            && !answer.is_empty()
            && let Some(title) = self.generate_title(question).await
        {
            conversation.title = title;
        }

        // Persist the whole exchange so the session can be resumed later.
        if let Err(e) = conversation.save() {
            tracing::debug!("failed to save conversation: {e}");
        }
        Ok(answer)
    }

    /// Ask the model for a short title summarizing the opening question.
    /// Returns `None` on any error or an empty result.
    async fn generate_title(&self, first_question: &str) -> Option<String> {
        let prompt: String = first_question.chars().take(2000).collect();
        let turn = Turn::User(prompt);
        let reply = self.provider.chat(TITLE_SYSTEM, &[turn], &[]).await.ok()?;
        let line = reply.text.lines().find(|l| !l.trim().is_empty())?;
        let title: String = line.trim().trim_matches('"').chars().take(60).collect();
        (!title.is_empty()).then_some(title)
    }

    /// Files the assistant touched while answering the last question.
    pub fn files_touched(&self) -> usize {
        self.toolbox.files_touched_count()
    }
}

// ─── Command entry points ───────────────────────────────────────────────────

/// `ciabatta ai` (default): the chat TUI plus the live graph server. When
/// `resume` names (or defaults to) a saved conversation, the session continues
/// where it left off; otherwise a fresh conversation is started.
pub async fn run_tui(
    root: &Path,
    config: &CiabattaConfig,
    port: u16,
    no_graph: bool,
    mode: Mode,
    resume: Resume,
) -> Result<()> {
    let conversation = resume.load(root)?;
    let assistant = match conversation {
        Some(c) => Assistant::with_conversation(root, config, c)?,
        None => Assistant::new(root, config)?,
    };
    assistant.set_mode(mode);

    let graph_url = if no_graph {
        None
    } else {
        let jobs = jobs::Jobs::open(root, config)?;
        let handle = server::spawn(assistant.clone(), jobs, port).await?;
        Some(handle)
    };

    tui::run(assistant, graph_url).await
}

/// Which past conversation to resume, if any.
pub enum Resume {
    /// Start a brand-new conversation.
    None,
    /// Resume the most recently updated conversation (if one exists).
    Latest,
    /// Resume the conversation with this id.
    Id(String),
}

impl Resume {
    fn load(&self, root: &Path) -> Result<Option<Conversation>> {
        match self {
            Resume::None => Ok(None),
            Resume::Latest => session::Conversation::latest(root),
            Resume::Id(id) => session::Conversation::load(root, id).map(Some),
        }
    }
}

/// `ciabatta ai resume [id]`: with an id, open that saved conversation in the
/// TUI; with none, print the saved conversations so the user can pick one.
pub async fn run_resume(
    root: &Path,
    config: &CiabattaConfig,
    port: u16,
    no_graph: bool,
    mode: Mode,
    id: Option<String>,
) -> Result<()> {
    if let Some(id) = id {
        return run_tui(root, config, port, no_graph, mode, Resume::Id(id)).await;
    }

    let saved = session::list(root)?;
    if saved.is_empty() {
        println!("No saved conversations yet. Start one with `ciabatta ai`.");
        return Ok(());
    }
    println!("Saved conversations (most recent first):\n");
    for s in &saved {
        let when = s.updated_at.split('T').next().unwrap_or(&s.updated_at);
        println!("  {}  [{when}] {} msg — {}", s.id, s.turns, s.title);
    }
    println!("\nResume one with:  ciabatta ai resume <id>");
    println!("Or continue the latest with:  ciabatta ai --continue");
    println!("Delete one with:  ciabatta ai delete <id>   ·   clear all:  ciabatta ai clear");
    Ok(())
}

/// Default look-back window for `/report` and `ciabatta ai report`.
pub const DEFAULT_REPORT_DAYS: u64 = 7;

/// Build the agent prompt for a "what changed" report over the past `days`
/// days, embedding the repo's git activity. Public so the TUI `/report` command
/// can reuse it.
pub fn report_prompt(root: &Path, days: u64) -> Result<String> {
    let activity = crate::git::changes_since(root, days)?;
    Ok(format!(
        "Report on what changed in this repository over the past {days} day(s). Base it on the \
         git activity below. Group the changes by theme or area (feature, fix, refactor, docs, \
         etc.), lead with the most significant, and call out anything risky or worth a closer \
         look. Be concise. If a change needs context to explain, read the relevant file.\n\n\
         === git activity ===\n{activity}"
    ))
}

/// `ciabatta ai report [days] [--pdf [FILE]]`: summarize what changed in the
/// repo over the past N days (default 7), from git history plus the assistant.
/// Prints the summary, and — when `pdf` is set — also writes it to a PDF.
pub async fn run_report(
    root: &Path,
    config: &CiabattaConfig,
    days: Option<u64>,
    mode: Mode,
    pdf: Option<String>,
) -> Result<()> {
    let days = days.unwrap_or(DEFAULT_REPORT_DAYS).clamp(1, 3650);
    let activity = crate::git::changes_since(root, days)?;
    let prompt = report_prompt(root, days)?;

    let assistant = Assistant::new(root, config)?;
    assistant.set_mode(mode);
    eprintln!(
        "provider: {} · report over the past {days} day(s)",
        assistant.provider.label()
    );

    let (tx, mut rx) = mpsc::channel::<AiEvent>(64);
    let printer = tokio::spawn(async move {
        while let Some(ev) = rx.recv().await {
            if let AiEvent::Status(s) = ev {
                eprintln!("  {s}");
            }
        }
    });
    let answer = assistant.ask(&prompt, tx).await?;
    let _ = printer.await;
    println!("\n{answer}");

    if let Some(spec) = pdf {
        let path = pdf_report_path(&spec, days);
        pdf::write_report(&path, days, &answer, &activity)?;
        println!("\nSaved PDF report to {}", path.display());
    }
    Ok(())
}

/// Resolve where to write a report PDF: an explicit path if given, otherwise a
/// dated default in the current directory.
fn pdf_report_path(spec: &str, days: u64) -> std::path::PathBuf {
    let spec = spec.trim();
    if !spec.is_empty() {
        return std::path::PathBuf::from(spec);
    }
    let _ = days;
    let name = format!("ciabatta-report-{}.pdf", chrono::Local::now().format("%Y%m%d-%H%M%S"));
    std::env::current_dir().unwrap_or_default().join(name)
}

/// Build the prompt for a quick "connect this tag" pass. The named architecture
/// has already been registered on the map; the agent's job is to find the files
/// that belong to it and propose the connections with `tag_file`.
pub fn tag_pass_prompt(name: &str, description: &str) -> String {
    let name = name.trim().to_lowercase();
    let desc = if description.trim().is_empty() {
        String::new()
    } else {
        format!(" It is described as: {}.", description.trim())
    };
    format!(
        "The user just added a new architecture tag to the mind map: '{name}'.{desc}\n\n\
         Do a QUICK pass over this codebase to find the files that belong to '{name}', and call \
         `tag_file` on each one to connect it (with '{name}' among its tags). Use `suggest_files` \
         and `search_code` to locate candidates; only read a file when you must to be sure. Aim \
         for the clearly-relevant files, not everything — precision over volume. Your tags are \
         proposed for the user to confirm. When done, briefly list the files you tagged and why."
    )
}

/// `ciabatta ai tag <name> [description...]`: register a user-submitted
/// architecture tag on the mind map, then run a quick AI pass to connect the
/// files that belong to it (proposed for confirmation).
pub async fn run_tag(
    root: &Path,
    config: &CiabattaConfig,
    name: &str,
    description: &str,
    mode: Mode,
) -> Result<()> {
    let name = name.trim();
    if name.is_empty() {
        anyhow::bail!("give a tag name, e.g. `ciabatta ai tag auth \"login and sessions\"`");
    }
    // Register the architecture up front so it appears on the map immediately,
    // even before the AI finds files for it.
    Brain::open(root)?.set_architecture(name, description)?;
    println!("Added architecture '{}' to the mind map.", name.to_lowercase());
    println!("Running a quick pass to find files that belong to it…\n");

    let prompt = tag_pass_prompt(name, description);
    run_ask(root, config, &prompt, mode, Resume::None).await
}

/// `ciabatta ai delete <id>`: remove one saved conversation.
pub fn run_delete(root: &Path, id: &str) -> Result<()> {
    if session::delete(root, id)? {
        println!("Deleted conversation {id}.");
    } else {
        println!("No saved conversation '{id}'. See `ciabatta ai resume` for the list.");
    }
    Ok(())
}

/// `ciabatta ai clear [--yes]`: remove every saved conversation for the project.
pub fn run_clear(root: &Path, yes: bool) -> Result<()> {
    let saved = session::list(root)?;
    if saved.is_empty() {
        println!("No saved conversations to clear.");
        return Ok(());
    }
    if !yes {
        eprint!(
            "Delete all {} saved conversation(s) for this project? This cannot be undone. [y/N] ",
            saved.len()
        );
        std::io::stderr().flush()?;
        let mut line = String::new();
        std::io::stdin().read_line(&mut line)?;
        if !line.trim().eq_ignore_ascii_case("y") {
            println!("Cancelled — nothing deleted.");
            return Ok(());
        }
    }
    let removed = session::clear(root)?;
    println!("Deleted {removed} conversation(s).");
    Ok(())
}

/// `ciabatta ai serve`: run just the daemon (mind map + JSON API) in the
/// foreground until interrupted.
pub async fn run_serve(root: &Path, config: &CiabattaConfig, port: u16, mode: Mode) -> Result<()> {
    let assistant = Assistant::new(root, config)?;
    assistant.set_mode(mode);
    let jobs = jobs::Jobs::open(root, config)?;
    let url = server::spawn(assistant, jobs, port).await?;
    println!("\nAI assistant daemon ready at {url}");
    println!("  {url}           live architecture mind map");
    println!("  POST {url}api/ask       {{\"prompt\": \"...\"}}");
    println!("  POST {url}api/ship      {{\"prompt\": \"...\"}}   (background task)");
    println!("  GET  {url}api/jobs      background task status");
    println!("  POST {url}api/feedback  {{\"positive\": true}}");
    println!("Press Ctrl-C to stop.");
    // The server runs on spawned tasks; park this one until interrupted.
    tokio::signal::ctrl_c().await?;
    Ok(())
}

/// `ciabatta ai ship "..."`: hand the assistant a task to complete behind the
/// scenes. It runs the full agent loop in auto-accept mode to completion, then
/// prints a summary. With `--todo <id>`, the task text is pulled from your
/// personal todo list and the todo is marked done if the job succeeds.
pub async fn run_ship(
    root: &Path,
    config: &CiabattaConfig,
    prompt: &str,
    todo_id: Option<u64>,
) -> Result<()> {
    let jobs = jobs::Jobs::open(root, config)?;
    let source = todo_id.map(|id| format!("todo:{id}")).unwrap_or_else(|| "cli".to_string());

    eprintln!("shipping task to the assistant (running in the background)…");
    eprintln!("  task: {prompt}\n");
    let job = jobs.ship_and_wait(prompt, &source).await?;

    for step in &job.steps {
        eprintln!("  {step}");
    }
    match job.status {
        jobs::JobStatus::Done => {
            if !job.changed_files.is_empty() {
                println!("\nchanged files: {}", job.changed_files.join(", "));
            }
            println!("\n{}", job.answer.as_deref().unwrap_or("(done)"));
            // Reflect completed AI work back into the personal todo list.
            if let Some(id) = todo_id {
                if let Ok(store) = crate::todo::Store::open() {
                    let _ = store.set_done(id, true);
                    eprintln!("\nmarked todo #{id} done.");
                }
            }
            eprintln!("\njob #{} saved — see `ciabatta ai jobs`.", job.id);
            Ok(())
        }
        _ => {
            let err = job.error.as_deref().unwrap_or("unknown error");
            anyhow::bail!("the background task failed: {err}");
        }
    }
}

/// `ciabatta ai jobs`: list background tasks (most recent first).
pub fn run_jobs(root: &Path, config: &CiabattaConfig) -> Result<()> {
    let jobs = jobs::Jobs::open(root, config)?;
    let all = jobs.list();
    if all.is_empty() {
        println!("No background tasks yet. Start one with `ciabatta ai ship \"...\"`.");
        return Ok(());
    }
    println!("Background AI tasks (most recent first):\n");
    for j in &all {
        let when = j.created_at.split('T').next().unwrap_or(&j.created_at);
        println!("  #{:<3} [{}] {when}  {}", j.id, j.status.label(), j.prompt);
        if !j.changed_files.is_empty() {
            println!("        changed: {}", j.changed_files.join(", "));
        }
        if let Some(err) = &j.error {
            println!("        error: {err}");
        }
    }
    Ok(())
}

/// `ciabatta ai burn-in`: traverse the codebase, determine its architectures,
/// and build the mind map in one supervised pass (see [`burnin`]).
pub async fn run_burn_in(
    root: &Path,
    config: &CiabattaConfig,
    port: u16,
    review: bool,
    limit: Option<usize>,
) -> Result<()> {
    let assistant = Assistant::new(root, config)?;
    burnin::run(assistant, root, port, review, limit).await
}

/// `ciabatta ai ask "..."`: one-shot question, plain output. Progress goes to
/// stderr, the answer to stdout; afterwards any pending tag confirmations and
/// a feedback prompt run interactively when stdin is a TTY.
pub async fn run_ask(
    root: &Path,
    config: &CiabattaConfig,
    question: &str,
    mode: Mode,
    resume: Resume,
) -> Result<()> {
    let assistant = match resume.load(root)? {
        Some(c) => Assistant::with_conversation(root, config, c)?,
        None => Assistant::new(root, config)?,
    };
    assistant.set_mode(mode);
    eprintln!("provider: {} · mode: {}", assistant.provider.label(), mode.label());

    let (tx, mut rx) = mpsc::channel::<AiEvent>(64);
    let printer = tokio::spawn(async move {
        while let Some(ev) = rx.recv().await {
            match ev {
                AiEvent::Status(s) => eprintln!("  {s}"),
                AiEvent::Suggestion(c) => {
                    eprintln!("\n  ✏ {} change for {}:\n{}", c.state.label(), c.file, c.diff);
                }
                AiEvent::Plan(items) if !items.is_empty() => {
                    eprintln!("\n  📋 plan:\n{}", tools::render_plan(&items));
                }
                AiEvent::Progress(_) | AiEvent::Plan(_) | AiEvent::Answer(_) | AiEvent::Error(_) => {}
            }
        }
    });

    let answer = assistant.ask(question, tx).await?;
    let _ = printer.await;
    println!("\n{answer}");

    if std::io::IsTerminal::is_terminal(&std::io::stdin()) {
        confirm_changes_on_stdin(&assistant)?;
        confirm_pending_on_stdin(&assistant)?;
        feedback_on_stdin(&assistant)?;
    }
    Ok(())
}

/// Walk the pending change proposals on stdin (y/n per file). Accepting
/// writes the proposed content into the working tree.
fn confirm_changes_on_stdin(assistant: &Assistant) -> Result<()> {
    let pending = assistant.toolbox.pending_changes();
    if pending.is_empty() {
        return Ok(());
    }
    eprintln!("\nThe assistant proposed code changes (view one in VS Code with");
    eprintln!("  code --diff <suggestions>/<file>.orig <suggestions>/<file>):");
    for c in pending {
        let reason = if c.reason.is_empty() {
            String::new()
        } else {
            format!(" — {}", c.reason)
        };
        eprint!("  apply change to {}{reason}? [y/N] ", c.file);
        std::io::stderr().flush()?;
        let mut line = String::new();
        std::io::stdin().read_line(&mut line)?;
        let accept = line.trim().eq_ignore_ascii_case("y");
        let resolved = assistant.toolbox.resolve_change(&c.file, accept)?;
        eprintln!("    {} {}", resolved.state.label(), resolved.file);
    }
    Ok(())
}

/// Walk the pending tag confirmations on stdin (y/n per file).
fn confirm_pending_on_stdin(assistant: &Assistant) -> Result<()> {
    let pending = assistant.brain.pending();
    if pending.is_empty() {
        return Ok(());
    }
    eprintln!("\nThe assistant proposed architecture tags:");
    for p in pending {
        let reason = if p.reason.is_empty() {
            String::new()
        } else {
            format!(" — {}", p.reason)
        };
        eprint!("  {} → [{}]{reason}  accept? [y/N] ", p.file, p.tags.join(", "));
        std::io::stderr().flush()?;
        let mut line = String::new();
        std::io::stdin().read_line(&mut line)?;
        let accept = line.trim().eq_ignore_ascii_case("y");
        assistant.brain.confirm(&p.file, accept)?;
    }
    Ok(())
}

/// One-key feedback prompt (g = good, b = bad, anything else skips).
fn feedback_on_stdin(assistant: &Assistant) -> Result<()> {
    eprint!("\nRate this answer — [g]ood / [b]ad / skip: ");
    std::io::stderr().flush()?;
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    let files_used = assistant.files_touched();
    match line.trim().to_lowercase().as_str() {
        "g" => {
            let c = assistant.brain.record_feedback(true, files_used, "cli")?;
            eprintln!("confidence → {c:.0}/100");
        }
        "b" => {
            let c = assistant.brain.record_feedback(false, files_used, "cli")?;
            eprintln!("confidence → {c:.0}/100");
        }
        _ => {}
    }
    Ok(())
}

/// `ciabatta ai setup`: interactively write the `[ai]` section into
/// `.ciabatta/ciabatta.toml` — pick Claude, an OpenAI-compatible endpoint, or
/// a self-hosted vLLM server.
pub fn run_setup(root: &Path) -> Result<()> {
    println!("Configure the ciabatta AI assistant.\n");

    let provider = prompt_default("Provider (claude / openai / vllm)", "claude")?;
    provider::ProviderKind::parse(&provider)?; // validate before writing
    let provider_lc = provider.trim().to_lowercase();

    // vLLM speaks the OpenAI wire format but is typically self-hosted, so it
    // gets a local default endpoint and no required API key.
    let (default_endpoint, default_model, default_key_env) = if provider_lc.starts_with("claude") {
        ("https://api.anthropic.com", "claude-opus-4-8", "ANTHROPIC_API_KEY")
    } else if provider_lc == "vllm" {
        ("http://localhost:8000", "", "OPENAI_API_KEY")
    } else {
        ("https://api.openai.com", "gpt-4o", "OPENAI_API_KEY")
    };

    let endpoint = prompt_default("Endpoint", default_endpoint)?;
    let model = prompt_default("Model", default_model)?;
    let api_key_env = prompt_default("Environment variable holding the API key", default_key_env)?;

    // Only ask about TLS verification for endpoints that might use a
    // self-signed certificate (i.e. not Anthropic's hosted API over https).
    let tls_verify = if provider_lc.starts_with("claude") {
        true
    } else {
        let answer = prompt_default(
            "Verify the endpoint's TLS certificate? (y/n — say n for self-signed dev certs)",
            "y",
        )?;
        !answer.trim().eq_ignore_ascii_case("n")
    };

    let images_raw = prompt_default(
        "Sandbox base images (comma-separated, run via podman/docker; empty for none)",
        "",
    )?;
    let images: Vec<String> = images_raw
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();

    let ai = AiConfig {
        provider: Some(provider),
        endpoint: Some(endpoint),
        model: Some(model),
        api_key_env: Some(api_key_env),
        tls_verify,
        images,
        verify: None,
    };

    write_ai_config(root, &ai)?;
    let path = root.join(CIABATTA_DIR).join(CONFIG_FILE);
    println!("\nWrote the [ai] section to {}", path.display());
    println!("Try it: ciabatta ai ask \"what does this project do?\"");
    Ok(())
}

/// Write the `[ai]` table into the project's ciabatta.toml by splicing the
/// section textually — replacing an existing `[ai]` block or appending a new
/// one — so the user's comments and formatting elsewhere survive. Creates the
/// file if the project has none.
fn write_ai_config(root: &Path, ai: &AiConfig) -> Result<()> {
    let dir = root.join(CIABATTA_DIR);
    std::fs::create_dir_all(&dir).with_context(|| format!("Failed to create {}", dir.display()))?;
    let path = dir.join(CONFIG_FILE);

    let existing = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e).with_context(|| format!("Failed to read {}", path.display())),
    };
    if !existing.trim().is_empty() {
        // Validate before we touch anything.
        existing
            .parse::<toml::Table>()
            .with_context(|| format!("Failed to parse {}", path.display()))?;
    }

    // Render just the [ai] section.
    let mut wrapper = toml::Table::new();
    wrapper.insert(
        "ai".to_string(),
        toml::Value::try_from(ai).context("Failed to serialize the [ai] section")?,
    );
    let ai_block = toml::to_string_pretty(&wrapper).context("Failed to render the [ai] section")?;

    let rendered = splice_ai_section(&existing, &ai_block);
    std::fs::write(&path, rendered).with_context(|| format!("Failed to write {}", path.display()))
}

/// Replace the `[ai]` block in `existing` with `ai_block` (or append it when
/// absent). A block runs from the `[ai]` header line to the next `[section]`
/// header. The `[ai]` config has no nested tables, so this simple scan holds.
fn splice_ai_section(existing: &str, ai_block: &str) -> String {
    let lines: Vec<&str> = existing.lines().collect();
    let start = lines.iter().position(|l| l.trim() == "[ai]");

    let Some(start) = start else {
        // No [ai] yet: append at the end, separated by a blank line.
        let mut out = existing.trim_end().to_string();
        if !out.is_empty() {
            out.push_str("\n\n");
        }
        out.push_str(ai_block);
        return out;
    };

    let mut end = lines[start + 1..]
        .iter()
        .position(|l| l.trim_start().starts_with('['))
        .map(|off| start + 1 + off)
        .unwrap_or(lines.len());
    // Blank and comment lines right before the next header usually document
    // that next section — leave them out of the replaced block.
    while end > start + 1 {
        let prev = lines[end - 1].trim();
        if prev.is_empty() || prev.starts_with('#') {
            end -= 1;
        } else {
            break;
        }
    }

    let mut out = String::new();
    for l in &lines[..start] {
        out.push_str(l);
        out.push('\n');
    }
    out.push_str(ai_block.trim_end());
    out.push('\n');
    if end < lines.len() {
        out.push('\n');
        for l in &lines[end..] {
            out.push_str(l);
            out.push('\n');
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::splice_ai_section;

    const AI_BLOCK: &str = "[ai]\nprovider = \"claude\"\n";

    #[test]
    fn splice_appends_when_absent_and_keeps_comments() {
        let existing = "# my precious comment\n[system]\nci = \"github\"\n";
        let out = splice_ai_section(existing, AI_BLOCK);
        assert!(out.contains("# my precious comment"));
        assert!(out.contains("[system]"));
        assert!(out.trim_end().ends_with("provider = \"claude\""));
    }

    #[test]
    fn splice_replaces_existing_ai_block_only() {
        let existing = "[ai]\nprovider = \"openai\"\nmodel = \"old\"\n\n# keep me\n[system]\nci = \"github\"\n";
        let out = splice_ai_section(existing, AI_BLOCK);
        assert!(out.contains("provider = \"claude\""));
        assert!(!out.contains("old"));
        assert!(out.contains("# keep me"));
        assert!(out.contains("[system]"));
        // still valid TOML with both sections
        let doc: toml::Table = out.parse().unwrap();
        assert!(doc.contains_key("ai") && doc.contains_key("system"));
    }

    #[test]
    fn splice_into_empty_file_is_just_the_block() {
        assert_eq!(splice_ai_section("", AI_BLOCK), AI_BLOCK);
    }
}

fn prompt_default(label: &str, default: &str) -> Result<String> {
    if default.is_empty() {
        print!("{label}: ");
    } else {
        print!("{label} [{default}]: ");
    }
    std::io::stdout().flush()?;
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    let line = line.trim();
    Ok(if line.is_empty() {
        default.to_string()
    } else {
        line.to_string()
    })
}
