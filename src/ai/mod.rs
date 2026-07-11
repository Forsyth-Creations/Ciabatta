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
    /// The final answer for the current question.
    Answer(String),
    /// The loop failed.
    Error(String),
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
    /// current confidence persona and mind map.
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
             Your current ability level for THIS project is {confidence:.0}/100. Act like a \
             {persona}.\n\
             \n\
             {mode_rules}\n\
             \n\
             How to work:\n\
             1. Prefer `suggest_files` (the architecture mind map) to locate code: it returns \
                exactly what is needed. Use `search_code` (grep/ripgrep) when the map is thin \
                or you need everything that mentions a term.\n\
             2. As you traverse files, call `tag_file` to record which architecture(s) each \
                file belongs to (a file can have several tags). The user confirms tags, so \
                keep them short, lowercase, and reusable (e.g. 'auth', 'frontend', 'deploy').\n\
             3. Use `sandbox_run` when you need to execute anything — it is your safe space.\n\
             4. Keep answers grounded in files you actually read; cite paths.\n\
             5. Be concise. Lead with the answer, then the supporting detail.\n\
             \n\
             Boundary: you may only read and write files inside this project workspace and \
             /tmp (your scratch space for bespoke, throwaway files). Anything outside those \
             is off-limits and tool calls to it will be refused.\n\
             \n\
             Current architecture map:\n{map}",
            confidence = self.brain.confidence(),
            persona = self.brain.persona(),
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

        let mut answer = String::new();
        for round in 0..MAX_TOOL_ROUNDS {
            let turn = match self.provider.chat(&self.system_prompt(), history, &specs).await {
                Ok(t) => t,
                Err(e) => {
                    // Drop the failed exchange so the session stays usable.
                    history.pop();
                    let _ = events.send(AiEvent::Error(e.to_string())).await;
                    return Err(e);
                }
            };

            if turn.refused || turn.tool_calls.is_empty() {
                answer = turn.text.clone();
                history.push(Turn::Assistant(turn));
                break;
            }

            let calls = turn.tool_calls.clone();
            history.push(Turn::Assistant(turn));

            let changes_before = self.toolbox.changes_len();
            let mut results = Vec::with_capacity(calls.len());
            for call in &calls {
                let _ = events.send(AiEvent::Status(ToolBox::describe(call))).await;
                results.push(self.toolbox.execute(call).await);
            }
            // Surface any change proposals this round produced, diff and all.
            for suggestion in self.toolbox.changes_since(changes_before) {
                let _ = events.send(AiEvent::Suggestion(suggestion)).await;
            }
            history.push(Turn::ToolResults(results));

            if round == MAX_TOOL_ROUNDS - 1 {
                answer = "I hit the tool-call limit before finishing — try narrowing the \
                          question."
                    .to_string();
            }
        }

        let _ = self.brain.record_interaction();
        // Persist the whole exchange so the session can be resumed later.
        if let Err(e) = conversation.save() {
            tracing::debug!("failed to save conversation: {e}");
        }
        let _ = events.send(AiEvent::Answer(answer.clone())).await;
        Ok(answer)
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
                AiEvent::Answer(_) | AiEvent::Error(_) => {}
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
