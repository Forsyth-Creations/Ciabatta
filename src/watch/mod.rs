//! `ciabatta watch <command>` — run a command and stream its logs into a live,
//! searchable web view.
//!
//! The command runs through the shell (so pipes / `&&` / redirects work). Its
//! stdout and stderr are captured line-by-line into a bounded ring buffer, and
//! the web app streams them live, searches the whole buffer, lets you bookmark
//! ("point at") lines, and notifies when a line matches a trigger phrase.
//!
//! The **daemon** owns these processes (see
//! [`crate::daemon::routes::watch`]), not the CLI invocation that started
//! them, so a watch survives closing the terminal.
//!
//! Bookmarks and triggers **persist to disk** under `~/.ciabatta/watch/`, keyed
//! by the command string, so they survive restarts. Log lines themselves are
//! never persisted — they're transient and potentially huge.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Mutex;

use anyhow::{Context, Result};
use regex::Regex;
use serde::{Deserialize, Serialize};

/// Recent trigger hits kept for the sidebar feed.
const MAX_HITS: usize = 1000;

/// Which stream a captured line came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Stream {
    Stdout,
    Stderr,
}

/// A single captured line of output.
#[derive(Debug, Clone, Serialize)]
pub struct LogLine {
    pub seq: u64,
    pub ts: String,
    pub stream: Stream,
    pub text: String,
}

/// A saved "point" in the output. `snippet` snapshots the line text so a
/// bookmark stays viewable even after its line is evicted from the ring buffer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Bookmark {
    pub id: u64,
    pub seq: u64,
    pub label: String,
    #[serde(default)]
    pub note: Option<String>,
    #[serde(default)]
    pub snippet: String,
    pub created_at: String,
}

/// A trigger phrase (or regex). New lines matching it raise a notification.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Trigger {
    pub id: u64,
    pub pattern: String,
    #[serde(default)]
    pub is_regex: bool,
    /// How many lines have matched this trigger so far this session (not persisted).
    #[serde(default, skip_deserializing)]
    pub hits: u64,
}

/// One line matching a trigger, for the live hit feed.
#[derive(Debug, Clone, Serialize)]
pub struct TriggerHit {
    pub trigger_id: u64,
    pub seq: u64,
    pub ts: String,
    pub text: String,
}

/// The lifecycle of the watched process.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", content = "code", rename_all = "lowercase")]
pub enum RunStatus {
    Running,
    Exited(i32),
    Signaled,
    Failed(String),
}

/// What gets written to / read from the on-disk sidecar file.
#[derive(Debug, Default, Serialize, Deserialize)]
struct Persisted {
    #[serde(default)]
    command: String,
    #[serde(default)]
    bookmarks: Vec<Bookmark>,
    #[serde(default)]
    triggers: Vec<Trigger>,
}

/// The mutable, shared state guarded by a single mutex.
struct Inner {
    command: String,
    max_lines: usize,
    started_at: String,
    lines: VecDeque<LogLine>,
    bookmarks: Vec<Bookmark>,
    triggers: Vec<Trigger>,
    /// Compiled matchers keyed by trigger id, kept in step with `triggers`.
    compiled: HashMap<u64, Regex>,
    hits: VecDeque<TriggerHit>,
    next_seq: u64,
    next_bookmark_id: u64,
    next_trigger_id: u64,
    status: RunStatus,
    /// The watched process, while it's running. Cleared when it's reaped.
    pid: Option<u32>,
}

/// The session identity a transcript is labelled with.
///
/// Lives on the daemon's session record rather than in the log store, so it's
/// passed in rather than read out.
pub struct TranscriptMeta<'a> {
    pub id: u64,
    pub command: &'a str,
    pub label: Option<&'a str>,
    pub created_at: &'a str,
}

impl TranscriptMeta<'_> {
    /// A filename for a downloaded transcript, safe on every platform.
    ///
    /// Named after the step when there is one and the command otherwise, so a
    /// folder of these is still readable after you've saved a few.
    pub fn filename(&self) -> String {
        let mut slug = String::new();
        let mut last_dash = false;
        for c in self.label.unwrap_or(self.command).chars() {
            if c.is_ascii_alphanumeric() {
                slug.push(c.to_ascii_lowercase());
                last_dash = false;
            } else if !last_dash {
                slug.push('-');
                last_dash = true;
            }
        }
        let slug: String = slug.trim_matches('-').chars().take(60).collect();
        let slug = slug.trim_matches('-');
        if slug.is_empty() {
            format!("ciabatta-watch-{}.log", self.id)
        } else {
            format!("ciabatta-watch-{}-{}.log", self.id, slug)
        }
    }
}

/// The watch store: a command's captured output plus its bookmarks and triggers.
pub struct WatchState {
    inner: Mutex<Inner>,
    persist_path: PathBuf,
    /// Notified whenever a line arrives or the process exits, so SSE
    /// subscribers can wake without polling.
    changed: tokio::sync::Notify,
}

impl WatchState {
    /// Create the store for `command`, loading any persisted bookmarks/triggers.
    pub fn new(command: &str, max_lines: usize) -> Result<Self> {
        let persist_path = persist_path_for(command)?;
        let saved = load(&persist_path)?;

        let mut compiled = HashMap::new();
        let mut next_trigger_id = 1;
        let mut next_bookmark_id = 1;
        let mut triggers = Vec::new();

        for mut t in saved.triggers {
            // Re-id on load so ids are dense and monotonic within a session.
            t.id = next_trigger_id;
            t.hits = 0;
            next_trigger_id += 1;
            if let Ok(re) = compile(&t.pattern, t.is_regex) {
                compiled.insert(t.id, re);
                triggers.push(t);
            }
            // A pattern that no longer compiles is dropped rather than fatal.
        }

        let mut bookmarks = saved.bookmarks;
        for b in &mut bookmarks {
            b.id = next_bookmark_id;
            next_bookmark_id += 1;
        }

        let inner = Inner {
            command: command.to_string(),
            max_lines: max_lines.max(1),
            started_at: now_rfc3339(),
            lines: VecDeque::new(),
            bookmarks,
            triggers,
            compiled,
            hits: VecDeque::new(),
            next_seq: 1,
            next_bookmark_id,
            next_trigger_id,
            status: RunStatus::Running,
            pid: None,
        };

        Ok(Self {
            inner: Mutex::new(inner),
            persist_path,
            changed: tokio::sync::Notify::new(),
        })
    }

    /// Spawn the command through the shell, streaming stdout/stderr into the
    /// store on background tasks. Returns once the child has started; the tasks
    /// keep running until the child exits.
    ///
    /// `cwd` is the directory the command runs in. The daemon owns these
    /// processes now, so it can't inherit a useful working directory the way
    /// the old per-invocation server did — the caller must say where.
    ///
    /// `env` is layered over the daemon's own environment. A session started by
    /// hand needs nothing there, but a `persistent` workflow step does: it has
    /// to see the same `CIABATTA_*` variables and per-package settings the rest
    /// of its graph ran with, and the daemon's environment has neither.
    pub fn spawn(
        self: &std::sync::Arc<Self>,
        command: &str,
        cwd: &std::path::Path,
        env: &std::collections::HashMap<String, String>,
    ) -> Result<()> {
        use tokio::io::{AsyncBufReadExt, BufReader};

        let mut cmd = shell_command(command);
        cmd.current_dir(cwd);
        // Ask the command for colour. Its output is a pipe, not a terminal, so
        // every well-behaved tool disables colour on its own — and the web app
        // renders the escapes it does emit, so the default costs the reader the
        // one thing that distinguishes an error line from a note. These are the
        // three conventions the ecosystem actually reads; they go on before the
        // caller's `env` so an explicit `FORCE_COLOR=0` still wins.
        cmd.env("FORCE_COLOR", "1");
        cmd.env("CLICOLOR_FORCE", "1");
        cmd.env("CLICOLOR", "1");
        cmd.envs(env);
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());
        cmd.stdin(Stdio::null());
        // Kill the child if the handle is ever dropped, so a session that goes
        // away can't leave an orphan running.
        cmd.kill_on_drop(true);
        // Lead a new process group, so [`stop`](Self::stop) can signal
        // everything the command started rather than just the shell.
        #[cfg(unix)]
        cmd.process_group(0);

        let mut child = cmd
            .spawn()
            .with_context(|| format!("Failed to start command: {command}"))?;

        // Keep the pid so the session can be stopped on request.
        self.inner.lock().unwrap().pid = child.id();

        let stdout = child.stdout.take();
        let stderr = child.stderr.take();

        if let Some(out) = stdout {
            let state = self.clone();
            tokio::spawn(async move {
                let mut lines = BufReader::new(out).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    state.push_line(Stream::Stdout, line);
                }
            });
        }
        if let Some(err) = stderr {
            let state = self.clone();
            tokio::spawn(async move {
                let mut lines = BufReader::new(err).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    state.push_line(Stream::Stderr, line);
                }
            });
        }

        // Reap the child and record its final status.
        let state = self.clone();
        tokio::spawn(async move {
            let status = match child.wait().await {
                Ok(s) => match s.code() {
                    Some(code) => RunStatus::Exited(code),
                    None => RunStatus::Signaled,
                },
                Err(e) => RunStatus::Failed(e.to_string()),
            };
            {
                let mut inner = state.inner.lock().unwrap();
                inner.status = status;
                inner.pid = None;
            }
            // Wake any SSE subscriber so it reports the exit and closes.
            state.changed.notify_waiters();
        });

        Ok(())
    }

    /// Whether the watched process is still running.
    pub fn is_running(&self) -> bool {
        matches!(self.inner.lock().unwrap().status, RunStatus::Running)
    }

    /// Sequence number of the newest captured line. Cheap change detector for
    /// the SSE loop.
    pub fn seq(&self) -> u64 {
        self.inner.lock().unwrap().next_seq
    }

    /// Wait until something changes (a new line, or the process exiting).
    ///
    /// The SSE stream parks here instead of polling on a timer, so an idle
    /// session costs nothing.
    pub async fn changed(&self) {
        self.changed.notified().await;
    }

    /// Ask the watched process to stop.
    ///
    /// Sends SIGTERM to the session's whole process **group** on Unix, so the
    /// child gets a chance to clean up; Windows has no equivalent, so it's a
    /// hard tree kill there.
    ///
    /// The group matters: the session runs through `sh -c`, which forks rather
    /// than execs for anything non-trivial, so signalling the shell alone would
    /// leave the actual work — `npm run dev`, and the node process under it —
    /// running with nothing left to stop it.
    pub fn stop(&self) -> Result<()> {
        let pid = self.inner.lock().unwrap().pid;
        let Some(pid) = pid else {
            anyhow::bail!("This watch session isn't running.");
        };

        #[cfg(unix)]
        {
            // SAFETY: `killpg` on a pid we spawned as a group leader (see
            // `spawn`). A failure is reported, not fatal. The pid can only be
            // reused after we reap the child, which clears it above.
            let rc = unsafe { libc::killpg(pid as libc::pid_t, libc::SIGTERM) };
            anyhow::ensure!(rc == 0, "Failed to signal pid {pid}");
        }
        #[cfg(windows)]
        {
            std::process::Command::new("taskkill")
                .args(["/PID", &pid.to_string(), "/T", "/F"])
                .output()
                .with_context(|| format!("Failed to stop pid {pid}"))?;
        }

        Ok(())
    }

    /// A full snapshot for the API: lines after `after`, plus everything the
    /// UI needs to render the session header.
    pub fn state_json(&self, after: u64, limit: usize) -> serde_json::Value {
        self.snapshot(after, limit)
    }

    /// Render the whole buffer as a plain-text transcript, for saving or
    /// sending to someone else.
    ///
    /// The header matters as much as the lines: a log pasted into a chat window
    /// with no command, no status, and no timestamps is most of a bug report
    /// short of the useful parts. `stderr` lines are marked, because "which of
    /// these came from stderr" is invariably the next question and the
    /// interleaved stream alone can't answer it.
    ///
    /// `timestamps` prefixes every line with when it arrived — right for
    /// diagnosing a hang, noise for a stack trace, so the caller chooses.
    pub fn transcript(&self, meta: &TranscriptMeta<'_>, timestamps: bool) -> String {
        let inner = self.inner.lock().unwrap();

        let mut out = String::new();
        out.push_str(&format!("# ciabatta watch session {}\n", meta.id));
        out.push_str(&format!("# command:  {}\n", meta.command));
        if let Some(label) = meta.label {
            out.push_str(&format!("# step:     {label}\n"));
        }
        out.push_str(&format!("# started:  {}\n", meta.created_at));
        out.push_str(&format!(
            "# status:   {}\n",
            match &inner.status {
                RunStatus::Running => "still running".to_string(),
                RunStatus::Exited(code) => format!("exited with code {code}"),
                RunStatus::Signaled => "killed by a signal".to_string(),
                RunStatus::Failed(code) => format!("failed to start: {code}"),
            }
        ));
        out.push_str(&format!("# exported: {}\n", now_rfc3339()));
        // A truncated buffer must say so: a transcript that silently starts in
        // the middle sends the reader hunting for a cause that was dropped.
        // `next_seq` is the id the *next* line will get and starts at 1, so the
        // number captured so far is one less than it.
        if inner.next_seq.saturating_sub(1) as usize > inner.lines.len() {
            out.push_str(&format!(
                "# NOTE: only the last {} lines were kept; earlier output was dropped.\n",
                inner.max_lines
            ));
        }
        out.push_str(&format!("# {} line(s) follow.\n\n", inner.lines.len()));

        for line in &inner.lines {
            if timestamps {
                out.push_str(&line.ts);
                out.push(' ');
            }
            if line.stream == Stream::Stderr {
                out.push_str("[stderr] ");
            }
            out.push_str(&strip_ansi(&line.text));
            out.push('\n');
        }

        // Bookmarks are the reader's own annotations of what mattered; they're
        // often the most useful thing in the file, and would otherwise be lost
        // the moment the log leaves the browser.
        if !inner.bookmarks.is_empty() {
            out.push_str("\n# ─── Bookmarks ───\n");
            for mark in &inner.bookmarks {
                out.push_str(&format!("# line {}: {}\n", mark.seq, mark.label));
                if let Some(note) = &mark.note {
                    out.push_str(&format!("#   note: {note}\n"));
                }
            }
        }

        out
    }

    /// Search the buffer. Thin public wrapper so the daemon routes can reach
    /// the existing implementation.
    pub fn search_lines(
        &self,
        terms: &[String],
        all: bool,
        regex: bool,
        stream: Option<Stream>,
        limit: usize,
    ) -> (Vec<LogLine>, usize) {
        self.search(terms, all, regex, stream, limit)
    }
}

impl WatchState {
    /// Append one captured line, evaluate triggers, and (on a match) print the
    /// line with a terminal bell so the console user is also notified.
    fn push_line(&self, stream: Stream, text: String) {
        let mut inner = self.inner.lock().unwrap();

        let seq = inner.next_seq;
        inner.next_seq += 1;
        let ts = now_rfc3339();

        // Check triggers before moving `text` into the buffer, against the line
        // without its colour escapes — a pattern shouldn't have to know that the
        // word it's looking for happens to be printed in red.
        let plain = strip_ansi(&text);
        let mut matched: Vec<(u64, String)> = Vec::new();
        for t in &inner.triggers {
            if let Some(re) = inner.compiled.get(&t.id)
                && re.is_match(&plain)
            {
                matched.push((t.id, t.pattern.clone()));
            }
        }
        for (id, pattern) in &matched {
            if let Some(t) = inner.triggers.iter_mut().find(|t| t.id == *id) {
                t.hits += 1;
            }
            inner.hits.push_back(TriggerHit {
                trigger_id: *id,
                seq,
                ts: ts.clone(),
                text: text.clone(),
            });
            while inner.hits.len() > MAX_HITS {
                inner.hits.pop_front();
            }
            tracing::debug!("watch trigger [{pattern}] matched: {text}");
        }

        inner.lines.push_back(LogLine {
            seq,
            ts,
            stream,
            text,
        });
        let max = inner.max_lines;
        while inner.lines.len() > max {
            inner.lines.pop_front();
        }

        // Release the lock before waking subscribers so they don't immediately
        // block on it.
        drop(inner);
        self.changed.notify_waiters();
    }

    /// Add a trigger (deduping by pattern+kind) and persist. Returns its id.
    pub fn add_trigger(&self, pattern: &str, is_regex: bool) -> Result<u64> {
        let pattern = pattern.trim();
        anyhow::ensure!(!pattern.is_empty(), "trigger pattern is empty");
        let re = compile(pattern, is_regex)
            .with_context(|| format!("Invalid trigger pattern: {pattern}"))?;

        let mut inner = self.inner.lock().unwrap();
        if let Some(existing) = inner
            .triggers
            .iter()
            .find(|t| t.pattern == pattern && t.is_regex == is_regex)
        {
            return Ok(existing.id);
        }

        let id = inner.next_trigger_id;
        inner.next_trigger_id += 1;
        inner.compiled.insert(id, re);
        inner.triggers.push(Trigger {
            id,
            pattern: pattern.to_string(),
            is_regex,
            hits: 0,
        });
        self.save(&inner);
        Ok(id)
    }

    /// Remove a trigger by id and persist.
    pub fn remove_trigger(&self, id: u64) {
        let mut inner = self.inner.lock().unwrap();
        inner.triggers.retain(|t| t.id != id);
        inner.compiled.remove(&id);
        self.save(&inner);
    }

    /// Add a bookmark pointing at `seq` (snapshotting the line's text) and
    /// persist. Returns its id.
    pub fn add_bookmark(&self, seq: u64, label: &str, note: Option<String>) -> u64 {
        let mut inner = self.inner.lock().unwrap();
        // The snippet and the label outlive the buffer — they're persisted and
        // read back as plain text, so they keep the words and not the colours.
        let snippet = inner
            .lines
            .iter()
            .find(|l| l.seq == seq)
            .map(|l| strip_ansi(&l.text).into_owned())
            .unwrap_or_default();
        let label = strip_ansi(label);
        let label = if label.trim().is_empty() {
            format!("line {seq}")
        } else {
            label.trim().to_string()
        };
        let id = inner.next_bookmark_id;
        inner.next_bookmark_id += 1;
        inner.bookmarks.push(Bookmark {
            id,
            seq,
            label,
            note: note.filter(|n| !n.trim().is_empty()),
            snippet,
            created_at: now_rfc3339(),
        });
        self.save(&inner);
        id
    }

    /// Remove a bookmark by id and persist.
    pub fn remove_bookmark(&self, id: u64) {
        let mut inner = self.inner.lock().unwrap();
        inner.bookmarks.retain(|b| b.id != id);
        self.save(&inner);
    }

    /// A snapshot for `/state.json`: lines with `seq > after` (capped at `limit`)
    /// plus the current status, bookmarks, triggers, and any hits after `after`.
    fn snapshot(&self, after: u64, limit: usize) -> serde_json::Value {
        let inner = self.inner.lock().unwrap();
        let lines: Vec<&LogLine> = inner
            .lines
            .iter()
            .filter(|l| l.seq > after)
            .take(limit)
            .collect();
        let hits: Vec<&TriggerHit> = inner.hits.iter().filter(|h| h.seq > after).collect();
        serde_json::json!({
            "command": inner.command,
            "started_at": inner.started_at,
            "status": inner.status,
            "total_lines": inner.next_seq.saturating_sub(1),
            "buffered_lines": inner.lines.len(),
            "next_seq": inner.next_seq,
            "lines": lines,
            "bookmarks": inner.bookmarks,
            "triggers": inner.triggers,
            "hits": hits,
        })
    }

    /// Search the whole buffer. `terms` are matched case-insensitively (as
    /// substrings unless `regex`); `all` requires every term, otherwise any.
    /// `stream` filters to `stdout`/`stderr`/`all`. Returns `(matches, total)`
    /// where `matches` is capped at `limit`.
    fn search(
        &self,
        terms: &[String],
        all: bool,
        regex: bool,
        stream: Option<Stream>,
        limit: usize,
    ) -> (Vec<LogLine>, usize) {
        // Compile each term once (bad regex → matches nothing).
        let matchers: Vec<Option<Regex>> = terms.iter().map(|t| compile(t, regex).ok()).collect();

        let inner = self.inner.lock().unwrap();
        let mut out = Vec::new();
        let mut total = 0;
        for line in &inner.lines {
            if let Some(s) = stream
                && line.stream != s
            {
                continue;
            }
            // Match the words, not the colour escapes around them.
            let plain = strip_ansi(&line.text);
            let hit = |m: &Option<Regex>| m.as_ref().is_some_and(|re| re.is_match(&plain));
            let is_match = if all {
                matchers.iter().all(hit)
            } else {
                matchers.iter().any(hit)
            };
            if is_match {
                total += 1;
                if out.len() < limit {
                    out.push(line.clone());
                }
            }
        }
        (out, total)
    }

    /// Persist bookmarks + triggers (called while holding the lock).
    fn save(&self, inner: &Inner) {
        let data = Persisted {
            command: inner.command.clone(),
            bookmarks: inner.bookmarks.clone(),
            triggers: inner.triggers.clone(),
        };
        if let Err(e) = save(&self.persist_path, &data) {
            tracing::debug!(error = %e, "watch: failed to persist bookmarks/triggers");
        }
    }
}

/// Build the shell command that runs `command` (pipes/&&/redirects supported).
fn shell_command(command: &str) -> tokio::process::Command {
    #[cfg(windows)]
    {
        let mut cmd = tokio::process::Command::new("cmd");
        cmd.arg("/C").arg(command);
        cmd
    }
    #[cfg(not(windows))]
    {
        let mut cmd = tokio::process::Command::new("sh");
        cmd.arg("-c").arg(command);
        cmd
    }
}

/// Remove ANSI escape sequences from `text`.
///
/// Captured lines keep their escapes so the web app can colour them, but
/// everything that reads a line *as text* — trigger and search matching, the
/// exported transcript, a bookmark's label — wants the words without them. A
/// search for "error" must not miss a line that colours the word red, and a
/// transcript pasted into a ticket must not arrive full of `[31m`.
fn strip_ansi(text: &str) -> std::borrow::Cow<'_, str> {
    if !text.contains('\u{1b}') {
        return std::borrow::Cow::Borrowed(text);
    }

    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            out.push(c);
            continue;
        }
        match chars.next() {
            // CSI — parameter and intermediate bytes, then a final byte in
            // `@`..`~`. Covers colour (`m`) and the cursor moves a progress bar
            // emits, which are equally unwanted in text.
            Some('[') => {
                for c in chars.by_ref() {
                    if ('\u{40}'..='\u{7e}').contains(&c) {
                        break;
                    }
                }
            }
            // OSC — a string (a window title, a hyperlink) ended by BEL or ST.
            Some(']') => {
                while let Some(c) = chars.next() {
                    if c == '\u{7}' {
                        break;
                    }
                    if c == '\u{1b}' {
                        chars.next();
                        break;
                    }
                }
            }
            // Any other escape is two characters; both are already consumed.
            _ => {}
        }
    }
    std::borrow::Cow::Owned(out)
}

/// Compile a trigger/search matcher. Substring patterns are escaped and made
/// case-insensitive; regex patterns are used verbatim (the user controls flags).
fn compile(pattern: &str, is_regex: bool) -> Result<Regex> {
    let source = if is_regex {
        pattern.to_string()
    } else {
        format!("(?i){}", regex::escape(pattern))
    };
    Ok(Regex::new(&source)?)
}

/// Path to the sidecar file for `command`: `~/.ciabatta/watch/watch-<hash>.json`.
fn persist_path_for(command: &str) -> Result<PathBuf> {
    let home = home_dir().context("Could not determine your home directory (HOME is unset)")?;
    let dir = home.join(".ciabatta").join("watch");
    std::fs::create_dir_all(&dir).with_context(|| format!("Failed to create {}", dir.display()))?;
    Ok(dir.join(format!("watch-{}.json", stable_hash(command))))
}

/// A stable hex hash of a string, for deriving a filename.
fn stable_hash(s: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut h);
    format!("{:016x}", h.finish())
}

/// Locate the user's home directory without an extra dependency.
fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

/// Read the sidecar, treating a missing/empty file as no saved state.
fn load(path: &PathBuf) -> Result<Persisted> {
    match std::fs::read_to_string(path) {
        Ok(s) if s.trim().is_empty() => Ok(Persisted::default()),
        Ok(s) => {
            serde_json::from_str(&s).with_context(|| format!("Failed to parse {}", path.display()))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Persisted::default()),
        Err(e) => Err(e).with_context(|| format!("Failed to read {}", path.display())),
    }
}

/// Write the sidecar back to disk (pretty-printed for easy hand-editing).
fn save(path: &PathBuf, data: &Persisted) -> Result<()> {
    let json = serde_json::to_string_pretty(data)?;
    std::fs::write(path, json).with_context(|| format!("Failed to write {}", path.display()))
}

/// Current time as an RFC 3339 string.
fn now_rfc3339() -> String {
    chrono::Local::now().to_rfc3339()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn store() -> Arc<WatchState> {
        // Unique per *call*, not just per process: every test in this binary
        // shares one pid, so a pid-only name pointed the whole (parallel) suite
        // at a single sidecar and let one test's lines show up in another's.
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let cmd = format!("test-cmd-{}-{n}", std::process::id());
        Arc::new(WatchState::new(&cmd, 100).unwrap())
    }

    fn meta() -> TranscriptMeta<'static> {
        TranscriptMeta {
            id: 7,
            command: "npm run build",
            label: None,
            created_at: "2026-08-11T09:00:00+01:00",
        }
    }

    #[test]
    fn a_transcript_carries_the_context_a_bare_log_dump_loses() {
        let s = store();
        s.push_line(Stream::Stdout, "compiling".into());
        s.push_line(Stream::Stderr, "warning: unused import".into());
        s.add_bookmark(2, "the warning", Some("this is the one".into()));

        let text = s.transcript(&meta(), false);

        // Who, what, and when — the parts a pasted log is always missing.
        assert!(text.contains("ciabatta watch session 7"), "{text}");
        assert!(text.contains("# command:  npm run build"), "{text}");
        assert!(
            text.contains("# started:  2026-08-11T09:00:00+01:00"),
            "{text}"
        );
        assert!(text.contains("# status:   still running"), "{text}");
        assert!(text.contains("# 2 line(s) follow."), "{text}");

        // stderr is distinguishable, which the interleaved stream alone isn't.
        assert!(text.contains("compiling"), "{text}");
        assert!(text.contains("[stderr] warning: unused import"), "{text}");
        // …and the reader's own annotations survive leaving the browser.
        assert!(text.contains("# line 2: the warning"), "{text}");
        assert!(text.contains("#   note: this is the one"), "{text}");
    }

    #[test]
    fn timestamps_are_opt_in() {
        let s = store();
        s.push_line(Stream::Stdout, "a line".into());

        assert!(!s.transcript(&meta(), false).contains("[stderr]"));
        let plain = s.transcript(&meta(), false);
        let stamped = s.transcript(&meta(), true);
        // The stamped one is longer by a timestamp per line, and the plain one
        // starts its body with the text itself.
        assert!(stamped.len() > plain.len());
        assert!(plain.contains("\na line\n"), "{plain}");
        assert!(!stamped.contains("\na line\n"), "{stamped}");
    }

    #[test]
    fn a_truncated_buffer_says_so_rather_than_starting_mid_story() {
        // A buffer of two, with three lines pushed through it.
        let cmd = format!("test-trunc-{}", std::process::id());
        let s = WatchState::new(&cmd, 2).unwrap();
        for line in ["first", "second", "third"] {
            s.push_line(Stream::Stdout, line.into());
        }

        let text = s.transcript(&meta(), false);
        assert!(text.contains("earlier output was dropped"), "{text}");
        assert!(!text.contains("first"), "{text}");
        assert!(text.contains("third"), "{text}");
    }

    #[test]
    fn an_intact_buffer_does_not_claim_to_be_truncated() {
        let s = store();
        for line in ["one", "two", "three"] {
            s.push_line(Stream::Stdout, line.into());
        }
        let text = s.transcript(&meta(), false);
        assert!(!text.contains("dropped"), "{text}");
    }

    #[test]
    fn a_transcript_filename_is_readable_and_safe() {
        assert_eq!(
            TranscriptMeta {
                id: 3,
                command: "npm run dev -- --port 3000",
                label: None,
                created_at: "",
            }
            .filename(),
            "ciabatta-watch-3-npm-run-dev-port-3000.log"
        );
        // A labelled session is named after its step, which identifies it far
        // better than the command line does.
        assert_eq!(
            TranscriptMeta {
                id: 4,
                command: "sleep 3600",
                label: Some("web:serve"),
                created_at: "",
            }
            .filename(),
            "ciabatta-watch-4-web-serve.log"
        );
        // Nothing usable left after slugging still yields a valid filename.
        assert_eq!(
            TranscriptMeta {
                id: 5,
                command: "!!!",
                label: None,
                created_at: "",
            }
            .filename(),
            "ciabatta-watch-5.log"
        );
    }

    #[test]
    fn search_any_and_all() {
        let s = store();
        s.push_line(Stream::Stdout, "hello world".into());
        s.push_line(Stream::Stderr, "goodbye world".into());
        s.push_line(Stream::Stdout, "hello there".into());

        let terms = vec!["hello".to_string(), "goodbye".to_string()];
        let (any, any_total) = s.search(&terms, false, false, None, 100);
        assert_eq!(any_total, 3);
        assert_eq!(any.len(), 3);

        let terms = vec!["hello".to_string(), "world".to_string()];
        let (all, all_total) = s.search(&terms, true, false, None, 100);
        assert_eq!(all_total, 1);
        assert_eq!(all[0].text, "hello world");
    }

    #[test]
    fn colour_escapes_do_not_hide_a_line_from_search_or_a_trigger() {
        let s = store();
        s.add_trigger("error", false).unwrap();
        // A coloured line, as any build tool emits once it thinks it has a tty.
        s.push_line(Stream::Stderr, "\u{1b}[1;31merror\u{1b}[0m: boom".into());

        let (found, total) = s.search(&["error: boom".to_string()], false, false, None, 100);
        assert_eq!(total, 1);
        // The line keeps its escapes — the web app colours them; only the
        // matching looked at the text underneath.
        assert!(found[0].text.contains('\u{1b}'));

        let snap = s.snapshot(0, 100);
        assert_eq!(snap["hits"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn the_transcript_is_plain_text() {
        let s = store();
        s.push_line(Stream::Stdout, "\u{1b}[32mpassed\u{1b}[0m".into());
        // Cursor moves from a progress bar go too: they are not text either.
        s.push_line(Stream::Stdout, "\u{1b}[2K\u{1b}[1G50%".into());

        let out = s.transcript(
            &TranscriptMeta {
                id: 1,
                command: "make",
                label: None,
                created_at: "",
            },
            false,
        );
        assert!(!out.contains('\u{1b}'));
        assert!(out.contains("passed"));
        assert!(out.contains("50%"));
    }

    #[test]
    fn search_stream_filter() {
        let s = store();
        s.push_line(Stream::Stdout, "on stdout".into());
        s.push_line(Stream::Stderr, "on stderr".into());
        let terms = vec!["on".to_string()];
        let (only_err, total) = s.search(&terms, false, false, Some(Stream::Stderr), 100);
        assert_eq!(total, 1);
        assert_eq!(only_err[0].text, "on stderr");
    }

    #[test]
    fn triggers_count_and_dedupe() {
        let s = store();
        let id = s.add_trigger("error", false).unwrap();
        // Adding the same phrase again returns the same trigger.
        assert_eq!(s.add_trigger("error", false).unwrap(), id);

        s.push_line(Stream::Stdout, "all good".into());
        s.push_line(Stream::Stderr, "ERROR: boom".into()); // case-insensitive
        s.push_line(Stream::Stdout, "another error here".into());

        let snap = s.snapshot(0, 100);
        let hits = snap["hits"].as_array().unwrap();
        assert_eq!(hits.len(), 2);
        let trig = &snap["triggers"].as_array().unwrap()[0];
        assert_eq!(trig["hits"].as_u64().unwrap(), 2);
    }

    #[test]
    fn bookmark_snapshots_line_text() {
        let s = store();
        s.push_line(Stream::Stdout, "important line".into());
        let id = s.add_bookmark(1, "keep me", None);
        let snap = s.snapshot(0, 100);
        let bm = &snap["bookmarks"].as_array().unwrap()[0];
        assert_eq!(bm["snippet"], "important line");
        s.remove_bookmark(id);
        let snap = s.snapshot(0, 100);
        assert!(snap["bookmarks"].as_array().unwrap().is_empty());
    }

    #[test]
    fn ring_buffer_is_bounded() {
        let cmd = format!("ring-{}", std::process::id());
        let s = Arc::new(WatchState::new(&cmd, 3).unwrap());
        for i in 0..10 {
            s.push_line(Stream::Stdout, format!("line {i}"));
        }
        let snap = s.snapshot(0, 100);
        assert_eq!(snap["buffered_lines"].as_u64().unwrap(), 3);
        assert_eq!(snap["total_lines"].as_u64().unwrap(), 10);
        // Only the last three lines remain.
        let lines = snap["lines"].as_array().unwrap();
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0]["text"], "line 7");
    }
}
