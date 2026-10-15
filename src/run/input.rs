//! Asking the operator a question in the middle of a run: `ciabatta input`.
//!
//! A step's script runs as its own process, so it can't reach the terminal or
//! the browser that is watching the run. Instead every run starts a small
//! broker on a loopback port and hands its address to each step through the
//! environment ([`ADDR_VAR`], [`TOKEN_VAR`]). `ciabatta input` connects to it,
//! sends one [`Request`], and blocks until a [`Reply`] comes back:
//!
//! ```bash
//! region=$(ciabatta input "Which region?" --default us-east-1 --env REGION)
//! ```
//!
//! The broker turns the request into a [`ProgressUpdate::InputRequested`],
//! which every viewer of the run already receives — the plain terminal prompts
//! for it with a countdown, the web app puts up a modal — and waits for an
//! [`Answer`] on the run's answer bus, for the deadline, or for the asking
//! process to go away, whichever comes first. A run nobody can answer (CI, the
//! TUI) settles each question immediately: its `--default`, or an error that
//! says how to supply the value instead.
//!
//! Loopback TCP rather than a Unix socket so the same code serves Windows, and
//! a per-run random token so another local process can't answer on a step's
//! behalf by guessing the port.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{broadcast, mpsc};

use crate::runner::ProgressUpdate;

/// Where a step finds its run's broker.
pub const ADDR_VAR: &str = "CIABATTA_INPUT_ADDR";
/// The secret a request must carry for the broker to accept it.
pub const TOKEN_VAR: &str = "CIABATTA_INPUT_TOKEN";
/// The name of the step a process is running for, so a question can say who
/// asked it.
pub const STEP_VAR: &str = "CIABATTA_STEP";

/// The longest request line the broker will read. A question is a sentence or
/// two; anything near this is a client gone wrong, not a long prompt.
const MAX_REQUEST: u64 = 64 * 1024;

/// What `ciabatta input` sends to the broker.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Request {
    pub token: String,
    /// The step asking, from [`STEP_VAR`].
    #[serde(default)]
    pub step: Option<String>,
    /// The question, one line.
    pub prompt: String,
    /// Longer text shown with the question: what the value is for, where to
    /// find it.
    #[serde(default)]
    pub details: Option<String>,
    /// What an empty answer — or no answer at all — means.
    #[serde(default)]
    pub default: Option<String>,
    /// Mask the answer as it is typed, and in the logs.
    #[serde(default)]
    pub secret: bool,
    /// The variable that would have answered this without asking, so the
    /// question can say how to skip it next time.
    #[serde(default)]
    pub env: Option<String>,
    /// Overrides the workspace's `input_timeout` for this one question.
    #[serde(default)]
    pub timeout: Option<String>,
    /// Ask for nothing but a go-ahead: Enter in a terminal, a Continue button
    /// in the web app. The answer is empty.
    #[serde(default)]
    pub enter: bool,
}

/// The broker's answer: a value, or why there isn't one.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Reply {
    Value(String),
    Error(String),
}

/// A question waiting on the operator, as viewers see it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Pending {
    /// Unique within the run; an [`Answer`] names it.
    pub id: u64,
    pub step: String,
    pub prompt: String,
    #[serde(default)]
    pub details: Option<String>,
    #[serde(default)]
    pub default: Option<String>,
    #[serde(default)]
    pub secret: bool,
    #[serde(default)]
    pub env: Option<String>,
    /// Only a go-ahead is wanted — viewers show a Continue button, not a field.
    #[serde(default)]
    pub enter: bool,
    /// When it was asked, RFC 3339.
    pub asked_at: String,
    /// When it stops waiting, RFC 3339 — absent for a question that waits
    /// forever. Viewers count down to it.
    #[serde(default)]
    pub deadline: Option<String>,
}

/// An operator's answer to [`Pending`] `id`.
#[derive(Debug, Clone)]
pub struct Answer {
    pub id: u64,
    pub value: String,
}

/// How a question was settled, for viewers to take it down.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Answered,
    TimedOut,
    /// The process asking went away — its step was stopped or killed.
    Abandoned,
}

impl Outcome {
    pub fn label(self) -> &'static str {
        match self {
            Outcome::Answered => "answered",
            Outcome::TimedOut => "timed out",
            Outcome::Abandoned => "abandoned",
        }
    }
}

/// Parse an `input_timeout`/`--timeout` value. `None` means wait forever,
/// which is what `0`, `none`, `never` and `unlimited` all say.
pub fn parse_timeout(raw: &str) -> Result<Option<Duration>> {
    let text = raw.trim().to_ascii_lowercase();
    if matches!(text.as_str(), "none" | "never" | "unlimited" | "infinite") {
        return Ok(None);
    }
    let limit = super::parse_duration(&text)?;
    Ok((!limit.is_zero()).then_some(limit))
}

/// The workspace's default limit on how long a question waits, from
/// `workspace.input_timeout` in the root config. Unset — the default — is
/// unlimited. A value that doesn't parse is an error rather than "unlimited",
/// so a typo can't quietly turn a 5-minute CI guard into a hang.
pub fn workspace_timeout(config: &crate::config::CiabattaConfig) -> Result<Option<Duration>> {
    match config
        .workspace
        .as_ref()
        .and_then(|w| w.input_timeout.as_deref())
    {
        None => Ok(None),
        Some(raw) => parse_timeout(raw)
            .with_context(|| format!("workspace.input_timeout ('{raw}') isn't a duration")),
    }
}

/// `4:59`, `1:02:03` — a countdown as a clock reads, not as a limit is written.
pub fn clock(remaining: Duration) -> String {
    let secs = remaining.as_secs();
    let (h, m, s) = (secs / 3600, (secs % 3600) / 60, secs % 60);
    match h {
        0 => format!("{m}:{s:02}"),
        _ => format!("{h}:{m:02}:{s:02}"),
    }
}

/// How an answer appears in a log: as given, or masked when it's a secret.
fn shown(value: &str, secret: bool) -> String {
    match secret {
        true => "••••••".to_string(),
        false => format!("'{value}'"),
    }
}

/// How `request`'s answer `value` reads in the step's log.
fn logged(request: &Request, value: &str) -> String {
    match request.enter {
        true => "continued".to_string(),
        false => shown(value, request.secret),
    }
}

// ─── The broker ─────────────────────────────────────────────────────────────

/// What every connection handler shares.
struct Ctx {
    workflow: String,
    token: String,
    timeout: Option<Duration>,
    answers: Option<broadcast::Sender<Answer>>,
    tx: mpsc::Sender<ProgressUpdate>,
    next: AtomicU64,
}

/// A run's question broker. Stops listening when dropped.
pub struct Broker {
    addr: SocketAddr,
    token: String,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Broker {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Broker {
    /// Listen on a fresh loopback port for `workflow`'s questions.
    ///
    /// `answers` is the bus operators' answers arrive on; `None` makes the run
    /// non-interactive, so every question settles at once. `timeout` is the
    /// default wait, which a request may override.
    pub async fn start(
        workflow: &str,
        timeout: Option<Duration>,
        answers: Option<broadcast::Sender<Answer>>,
        tx: mpsc::Sender<ProgressUpdate>,
    ) -> Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0))
            .await
            .context("Failed to open the input broker's port")?;
        let addr = listener.local_addr()?;
        let token = {
            use rand::Rng;
            let bytes: [u8; 16] = rand::thread_rng().r#gen();
            bytes.iter().map(|b| format!("{b:02x}")).collect::<String>()
        };
        let ctx = Arc::new(Ctx {
            workflow: workflow.to_string(),
            token: token.clone(),
            timeout,
            answers,
            tx,
            next: AtomicU64::new(1),
        });
        let task = tokio::spawn(async move {
            while let Ok((conn, _)) = listener.accept().await {
                let ctx = ctx.clone();
                tokio::spawn(async move {
                    if let Err(e) = serve(conn, &ctx).await {
                        tracing::debug!("input request failed: {e:#}");
                    }
                });
            }
        });
        Ok(Self { addr, token, task })
    }

    /// The variables a step needs to reach this broker.
    pub fn env(&self) -> [(String, String); 2] {
        [
            (ADDR_VAR.to_string(), self.addr.to_string()),
            (TOKEN_VAR.to_string(), self.token.clone()),
        ]
    }

    /// `env_vars` with this broker's variables added, for handing to the engine.
    ///
    /// The running binary's directory also goes to the front of `PATH`, so the
    /// `ciabatta` a step calls is this one — not an older install that has
    /// never heard of `input`, or of this broker's protocol.
    pub fn extend(&self, env_vars: &HashMap<String, String>) -> HashMap<String, String> {
        let mut env = env_vars.clone();
        env.extend(self.env());
        if let Some(dir) = std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(|d| d.to_path_buf()))
        {
            let path = env.get("PATH").cloned().unwrap_or_default();
            let mut dirs = vec![dir];
            dirs.extend(std::env::split_paths(&path));
            if let Ok(joined) = std::env::join_paths(dirs) {
                env.insert("PATH".to_string(), joined.to_string_lossy().into_owned());
            }
        }
        env
    }
}

/// Handle one connection: read its request, settle it, write the reply.
async fn serve(conn: TcpStream, ctx: &Ctx) -> Result<()> {
    let (read, mut write) = conn.into_split();
    let mut reader = BufReader::new(tokio::io::AsyncReadExt::take(read, MAX_REQUEST));
    let mut line = String::new();
    reader.read_line(&mut line).await?;
    let request: Request = serde_json::from_str(&line).context("malformed input request")?;

    let reply = if request.token != ctx.token {
        Reply::Error("this question didn't come from a step of the run".to_string())
    } else {
        settle(request, ctx, &mut reader).await
    };

    let mut out = serde_json::to_string(&reply)?;
    out.push('\n');
    write.write_all(out.as_bytes()).await?;
    write.shutdown().await.ok();
    Ok(())
}

/// Turn a request into a value: put the question to whoever is watching and
/// wait — or, with nobody watching, answer at once.
async fn settle<R>(request: Request, ctx: &Ctx, conn: &mut R) -> Reply
where
    R: tokio::io::AsyncBufRead + Unpin,
{
    let step = request.step.clone().unwrap_or_else(|| "?".to_string());
    let timeout = match request.timeout.as_deref() {
        Some(raw) => match parse_timeout(raw) {
            Ok(limit) => limit,
            Err(e) => return Reply::Error(format!("--timeout '{raw}': {e}")),
        },
        None => ctx.timeout,
    };

    let log = |line: String| {
        let tx = ctx.tx.clone();
        let workflow = ctx.workflow.clone();
        let step = step.clone();
        async move {
            let _ = tx
                .send(ProgressUpdate::StepLog {
                    workflow,
                    step,
                    line,
                })
                .await;
        }
    };

    let Some(bus) = ctx.answers.as_ref() else {
        // Nobody to ask. The default is the unattended answer, as a recovery
        // node's `default` option is.
        return match &request.default {
            Some(value) => {
                log(format!(
                    "input: {} → {} (default; this run can't ask)",
                    request.prompt,
                    logged(&request, value)
                ))
                .await;
                Reply::Value(value.clone())
            }
            None => Reply::Error(unattended(&request)),
        };
    };

    let id = ctx.next.fetch_add(1, Ordering::Relaxed);
    let deadline = timeout.map(|limit| Instant::now() + limit);
    let pending = Pending {
        id,
        step: step.clone(),
        prompt: request.prompt.clone(),
        details: request.details.clone(),
        default: request.default.clone(),
        secret: request.secret,
        env: request.env.clone(),
        enter: request.enter,
        asked_at: chrono::Local::now().to_rfc3339(),
        deadline: timeout.map(|limit| {
            (chrono::Local::now() + chrono::Duration::from_std(limit).unwrap_or_default())
                .to_rfc3339()
        }),
    };

    // Subscribe before announcing, so a fast answer can't arrive unheard.
    let mut answers = bus.subscribe();
    let _ = ctx
        .tx
        .send(ProgressUpdate::InputRequested {
            workflow: ctx.workflow.clone(),
            step: step.clone(),
            input: Box::new(pending),
        })
        .await;

    let answered = async {
        loop {
            match answers.recv().await {
                Ok(answer) if answer.id == id => return Some(answer.value),
                Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => return None,
            }
        }
    };
    let expired = async {
        match deadline {
            Some(at) => tokio::time::sleep_until(at.into()).await,
            None => std::future::pending().await,
        }
    };
    // The client sends nothing after its request, so a read that returns is
    // the client hanging up: its step was stopped, or it was killed.
    let gone = async {
        let mut rest = String::new();
        let _ = conn.read_line(&mut rest).await;
    };

    let (outcome, reply) = tokio::select! {
        value = answered => match value {
            Some(value) => {
                // An empty answer takes the default, as pressing Enter at a
                // prompt with one does.
                let value = match (value.is_empty(), &request.default) {
                    (true, Some(default)) => default.clone(),
                    _ => value,
                };
                log(format!("input: {} → {}", request.prompt, logged(&request, &value))).await;
                (Outcome::Answered, Reply::Value(value))
            }
            None => (Outcome::Abandoned, Reply::Error("the run stopped listening for answers".into())),
        },
        () = expired => {
            let limit = timeout.map(super::engine::format_duration).unwrap_or_default();
            match &request.default {
                Some(value) => {
                    log(format!(
                        "input: {} — no answer after {limit}, using the default: {}",
                        request.prompt,
                        logged(&request, value)
                    ))
                    .await;
                    (Outcome::TimedOut, Reply::Value(value.clone()))
                }
                None => {
                    let message = format!("no answer to '{}' after {limit}", request.prompt);
                    log(format!("input: {message}")).await;
                    (Outcome::TimedOut, Reply::Error(message))
                }
            }
        }
        () = gone => (Outcome::Abandoned, Reply::Error("abandoned".into())),
    };

    let _ = ctx
        .tx
        .send(ProgressUpdate::InputResolved {
            workflow: ctx.workflow.clone(),
            step,
            id,
            outcome,
        })
        .await;
    reply
}

/// Why an unattended run can't answer `request`, and how to make it able to.
fn unattended(request: &Request) -> String {
    let mut ways = vec!["pass --default".to_string()];
    if let Some(var) = &request.env {
        ways.insert(0, format!("set {var}"));
    }
    ways.push("run it from a terminal or with --gui".to_string());
    format!(
        "'{}' needs an answer, but nobody can be asked in this run — {}",
        request.prompt,
        ways.join(", or ")
    )
}

// ─── The terminal prompt ────────────────────────────────────────────────────

/// How a terminal prompt ended without an answer.
#[derive(Debug, PartialEq, Eq)]
pub enum NoAnswer {
    /// The deadline passed, or the question was settled elsewhere.
    Expired,
    /// Ctrl-C.
    Interrupted,
}

/// Ask `pending` on this terminal, with a live countdown to `deadline`.
///
/// Blocking: call it from `spawn_blocking`. Draws on stderr in raw mode so the
/// countdown can tick while the operator types. `abort` ends it early — the
/// question was settled some other way. Enter on an empty line takes the
/// default when there is one.
pub fn ask_terminal(
    pending: &Pending,
    deadline: Option<Instant>,
    abort: &AtomicBool,
) -> Result<std::result::Result<String, NoAnswer>> {
    use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
    use crossterm::{cursor, queue, style, terminal};
    use std::io::Write;

    let mut err = std::io::stderr();
    let bold = |s: &str| format!("\x1b[1m{s}\x1b[0m");
    let faint = |s: &str| format!("\x1b[2m{s}\x1b[0m");

    writeln!(err)?;
    writeln!(err, "? {}", bold(&pending.prompt))?;
    if let Some(details) = pending.details.as_deref() {
        for line in details.lines() {
            writeln!(err, "  {}", faint(line))?;
        }
    }
    let mut hints = Vec::new();
    match (&pending.default, pending.enter) {
        (_, true) => hints.push("press Enter to continue".to_string()),
        (Some(default), false) => {
            hints.push(format!("Enter for {}", shown(default, pending.secret)))
        }
        (None, false) => {}
    }
    if let Some(var) = &pending.env {
        hints.push(format!("set {var} to skip this question"));
    }
    if !hints.is_empty() {
        writeln!(err, "  {}", faint(&format!("({})", hints.join(" · "))))?;
    }

    terminal::enable_raw_mode().context("Failed to read from the terminal")?;
    // Raw mode is undone however this ends, error included.
    struct Cooked;
    impl Drop for Cooked {
        fn drop(&mut self) {
            let _ = crossterm::terminal::disable_raw_mode();
        }
    }
    let _cooked = Cooked;

    let mut typed = String::new();
    let mut drawn = String::new();
    let outcome = loop {
        if abort.load(Ordering::SeqCst) {
            break Err(NoAnswer::Expired);
        }
        let remaining = deadline.map(|at| at.saturating_duration_since(Instant::now()));
        if remaining.is_some_and(|r| r.is_zero()) {
            break Err(NoAnswer::Expired);
        }

        let shown_text = match (pending.enter, pending.secret) {
            (true, _) => faint("⏎ continue"),
            (false, true) => "•".repeat(typed.chars().count()),
            (false, false) => typed.clone(),
        };
        let line = match remaining {
            Some(r) => format!("  {} › {shown_text}", faint(&format!("[{}]", clock(r)))),
            None => format!("  › {shown_text}"),
        };
        if line != drawn {
            queue!(
                err,
                cursor::MoveToColumn(0),
                terminal::Clear(terminal::ClearType::CurrentLine),
                style::Print(&line)
            )?;
            err.flush()?;
            drawn = line;
        }

        if !event::poll(Duration::from_millis(200))? {
            continue;
        }
        let Event::Key(key) = event::read()? else {
            continue;
        };
        if key.kind == KeyEventKind::Release {
            continue;
        }
        match key.code {
            KeyCode::Enter => break Ok(typed.clone()),
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                break Err(NoAnswer::Interrupted);
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => typed.clear(),
            // A go-ahead takes no text.
            KeyCode::Char(_) if pending.enter => {}
            KeyCode::Char(c) => typed.push(c),
            KeyCode::Backspace => {
                typed.pop();
            }
            _ => {}
        }
    };
    drop(_cooked);
    writeln!(err)?;
    if outcome == Err(NoAnswer::Expired) && deadline.is_some_and(|at| Instant::now() >= at) {
        writeln!(err, "  {}", faint("(no answer in time)"))?;
    }

    Ok(
        outcome.map(|value| match (value.is_empty(), &pending.default) {
            (true, Some(default)) => default.clone(),
            _ => value,
        }),
    )
}

// ─── The client: `ciabatta input` ───────────────────────────────────────────

/// What `ciabatta input` was asked.
#[derive(Debug, Clone, Default)]
pub struct Ask {
    pub prompt: String,
    pub details: Option<String>,
    pub default: Option<String>,
    pub secret: bool,
    pub env: Option<String>,
    pub timeout: Option<String>,
    pub enter: bool,
}

/// Get the answer to `ask`: from its variable when that is set, from the run's
/// broker inside a step, or straight from this terminal otherwise.
pub async fn ask(mut ask: Ask, env: &HashMap<String, String>) -> Result<String> {
    // A go-ahead's default is to go ahead: a pause that times out, or that
    // nobody can see, carries on rather than failing the build.
    if ask.enter && ask.default.is_none() {
        ask.default = Some(String::new());
    }
    if let Some(var) = &ask.env
        && let Some(value) = env.get(var).filter(|v| !v.is_empty())
    {
        return Ok(value.clone());
    }

    match (env.get(ADDR_VAR), env.get(TOKEN_VAR)) {
        (Some(addr), Some(token)) => {
            let request = Request {
                token: token.clone(),
                step: env.get(STEP_VAR).cloned(),
                prompt: ask.prompt,
                details: ask.details,
                default: ask.default,
                secret: ask.secret,
                env: ask.env,
                timeout: ask.timeout,
                enter: ask.enter,
            };
            match request_answer(addr, &request).await? {
                Reply::Value(value) => Ok(value),
                Reply::Error(message) => bail!("{message}"),
            }
        }
        _ => ask_here(ask).await,
    }
}

/// Send `request` to the broker at `addr` and wait for its reply.
pub async fn request_answer(addr: &str, request: &Request) -> Result<Reply> {
    let conn = TcpStream::connect(addr)
        .await
        .with_context(|| format!("Failed to reach this run's input broker at {addr}"))?;
    let (read, mut write) = conn.into_split();
    let mut line = serde_json::to_string(request)?;
    line.push('\n');
    write.write_all(line.as_bytes()).await?;

    let mut reply = String::new();
    BufReader::new(read).read_line(&mut reply).await?;
    if reply.trim().is_empty() {
        bail!("the run ended before answering");
    }
    serde_json::from_str(&reply).context("malformed reply from the input broker")
}

/// Outside a run: ask on this terminal, or read a line from a piped stdin.
async fn ask_here(ask: Ask) -> Result<String> {
    use std::io::IsTerminal;

    let timeout = match ask.timeout.as_deref() {
        Some(raw) => parse_timeout(raw)?,
        None => None,
    };
    let pending = Pending {
        id: 0,
        step: String::new(),
        prompt: ask.prompt.clone(),
        details: ask.details,
        default: ask.default.clone(),
        secret: ask.secret,
        env: ask.env,
        enter: ask.enter,
        asked_at: String::new(),
        deadline: None,
    };

    if !std::io::stdin().is_terminal() {
        let mut line = String::new();
        std::io::stdin().read_line(&mut line)?;
        let line = line.trim_end_matches(['\r', '\n']).to_string();
        return match (line.is_empty(), ask.default) {
            (true, Some(default)) => Ok(default),
            (true, None) => bail!("'{}' needs an answer and stdin had none", ask.prompt),
            (false, _) => Ok(line),
        };
    }

    let deadline = timeout.map(|limit| Instant::now() + limit);
    let outcome = tokio::task::spawn_blocking(move || {
        ask_terminal(&pending, deadline, &AtomicBool::new(false))
    })
    .await??;
    match outcome {
        Ok(value) => Ok(value),
        Err(NoAnswer::Interrupted) => bail!("interrupted"),
        Err(NoAnswer::Expired) => match ask.default {
            Some(default) => Ok(default),
            None => bail!("no answer to '{}' in time", ask.prompt),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timeouts_parse_with_unlimited_spellings() {
        assert_eq!(parse_timeout("5m").unwrap(), Some(Duration::from_secs(300)));
        assert_eq!(parse_timeout("90").unwrap(), Some(Duration::from_secs(90)));
        for forever in ["0", "none", "never", "unlimited"] {
            assert_eq!(parse_timeout(forever).unwrap(), None, "{forever}");
        }
        assert!(parse_timeout("soon").is_err());
    }

    #[test]
    fn the_workspace_timeout_reads_from_the_root_config() {
        let read = |yaml: &str| {
            let config: crate::config::CiabattaConfig =
                crate::format::from_str(yaml, crate::format::Format::Yaml).unwrap();
            workspace_timeout(&config).unwrap()
        };
        assert_eq!(
            read("workspace:\n  input_timeout: 5m\n"),
            Some(Duration::from_secs(300))
        );
        assert_eq!(
            read("workspace:\n  input_timeout: 90\n"),
            Some(Duration::from_secs(90))
        );
        assert_eq!(read("workspace:\n  input_timeout: 0\n"), None);
        assert_eq!(read("workspace: {}\n"), None, "unlimited by default");
    }

    #[test]
    fn clock_reads_like_a_clock() {
        assert_eq!(clock(Duration::from_secs(5)), "0:05");
        assert_eq!(clock(Duration::from_secs(299)), "4:59");
        assert_eq!(clock(Duration::from_secs(3723)), "1:02:03");
    }

    async fn broker(
        timeout: Option<Duration>,
        interactive: bool,
    ) -> (
        Broker,
        mpsc::Receiver<ProgressUpdate>,
        Option<broadcast::Sender<Answer>>,
    ) {
        let (tx, rx) = mpsc::channel(64);
        let bus = interactive.then(|| broadcast::channel(8).0);
        let broker = Broker::start("build", timeout, bus.clone(), tx)
            .await
            .unwrap();
        (broker, rx, bus)
    }

    fn env_of(broker: &Broker) -> HashMap<String, String> {
        let mut env: HashMap<String, String> = broker.env().into_iter().collect();
        env.insert(STEP_VAR.into(), "deploy".into());
        env
    }

    fn question(prompt: &str) -> Ask {
        Ask {
            prompt: prompt.into(),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn a_set_variable_answers_without_asking() {
        let env = HashMap::from([("REGION".to_string(), "eu-west-1".to_string())]);
        let mut q = question("Region?");
        q.env = Some("REGION".into());
        assert_eq!(ask(q, &env).await.unwrap(), "eu-west-1");
    }

    #[tokio::test]
    async fn an_answer_from_the_bus_reaches_the_step() {
        let (broker, mut rx, bus) = broker(None, true).await;
        let env = env_of(&broker);
        let mut q = question("Region?");
        q.details = Some("Where to deploy".into());
        let asking = tokio::spawn(async move { ask(q, &env).await });

        let pending = loop {
            match rx.recv().await.unwrap() {
                ProgressUpdate::InputRequested { step, input, .. } => {
                    assert_eq!(step, "deploy");
                    break input;
                }
                _ => continue,
            }
        };
        assert_eq!(pending.prompt, "Region?");
        assert_eq!(pending.details.as_deref(), Some("Where to deploy"));
        assert!(pending.deadline.is_none(), "unlimited by default");
        bus.unwrap()
            .send(Answer {
                id: pending.id,
                value: "us-east-2".into(),
            })
            .unwrap();
        assert_eq!(asking.await.unwrap().unwrap(), "us-east-2");

        let resolved = loop {
            if let ProgressUpdate::InputResolved { id, outcome, .. } = rx.recv().await.unwrap() {
                break (id, outcome);
            }
        };
        assert_eq!(resolved, (pending.id, Outcome::Answered));
    }

    #[tokio::test]
    async fn a_timeout_falls_back_to_the_default_or_fails() {
        let (broker, mut rx, _bus) = broker(Some(Duration::from_millis(100)), true).await;
        let env = env_of(&broker);

        let mut q = question("Region?");
        q.default = Some("us-east-1".into());
        assert_eq!(ask(q, &env).await.unwrap(), "us-east-1");

        let err = ask(question("Name?"), &env).await.unwrap_err();
        assert!(err.to_string().contains("no answer"), "{err}");

        let mut deadlines = 0;
        while let Ok(update) = rx.try_recv() {
            if let ProgressUpdate::InputRequested { input, .. } = update {
                assert!(input.deadline.is_some());
                deadlines += 1;
            }
        }
        assert_eq!(deadlines, 2);
    }

    #[tokio::test]
    async fn an_unattended_run_uses_the_default_or_says_how_to_answer() {
        let (broker, _rx, _bus) = broker(None, false).await;
        let env = env_of(&broker);

        let mut q = question("Region?");
        q.default = Some("us-east-1".into());
        assert_eq!(ask(q, &env).await.unwrap(), "us-east-1");

        let mut q = question("Region?");
        q.env = Some("REGION".into());
        let err = ask(q, &env).await.unwrap_err().to_string();
        assert!(err.contains("set REGION"), "{err}");
    }

    #[tokio::test]
    async fn a_go_ahead_carries_on_when_nobody_presses_it() {
        let (broker, mut rx, _bus) = broker(Some(Duration::from_millis(50)), true).await;
        let env = env_of(&broker);
        let mut q = question("Ready?");
        q.enter = true;
        assert_eq!(ask(q, &env).await.unwrap(), "");
        let enter = loop {
            if let ProgressUpdate::InputRequested { input, .. } = rx.recv().await.unwrap() {
                break input.enter;
            }
        };
        assert!(enter, "viewers are told to show a Continue button");
    }

    #[tokio::test]
    async fn a_wrong_token_is_refused() {
        let (broker, _rx, _bus) = broker(None, true).await;
        let mut env = env_of(&broker);
        env.insert(TOKEN_VAR.into(), "guess".into());
        let err = ask(question("Region?"), &env).await.unwrap_err();
        assert!(err.to_string().contains("didn't come from"), "{err}");
    }
}
