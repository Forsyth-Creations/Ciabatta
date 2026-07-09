//! `ciabatta watch <command>` — run a command and stream its logs into a live,
//! searchable web view.
//!
//! The command runs through the shell (so pipes / `&&` / redirects work). Its
//! stdout and stderr are captured line-by-line into a bounded ring buffer, and a
//! tiny web server (see [`server`]) serves a single-page UI that polls for new
//! lines, searches the whole buffer, lets you bookmark ("point at") lines, and
//! fires notifications when a line matches a trigger phrase.
//!
//! Bookmarks and triggers **persist to disk** under `~/.ciabatta/watch/`, keyed
//! by the command string, so they survive restarts. Log lines themselves are
//! never persisted — they're transient and potentially huge.

pub mod server;

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
}

/// The watch store: a command's captured output plus its bookmarks and triggers.
pub struct WatchState {
    inner: Mutex<Inner>,
    persist_path: PathBuf,
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
        };

        Ok(Self {
            inner: Mutex::new(inner),
            persist_path,
        })
    }

    /// Spawn the command through the shell, streaming stdout/stderr into the
    /// store on background tasks. Returns once the child has started; the tasks
    /// keep running until the child exits.
    pub fn spawn(self: &std::sync::Arc<Self>, command: &str) -> Result<()> {
        use tokio::io::{AsyncBufReadExt, BufReader};

        let mut cmd = shell_command(command);
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());
        cmd.stdin(Stdio::null());

        let mut child = cmd
            .spawn()
            .with_context(|| format!("Failed to start command: {command}"))?;

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
            state.inner.lock().unwrap().status = status;
        });

        Ok(())
    }

    /// Append one captured line, evaluate triggers, and (on a match) print the
    /// line with a terminal bell so the console user is also notified.
    fn push_line(&self, stream: Stream, text: String) {
        let mut inner = self.inner.lock().unwrap();

        let seq = inner.next_seq;
        inner.next_seq += 1;
        let ts = now_rfc3339();

        // Check triggers before moving `text` into the buffer.
        let mut matched: Vec<(u64, String)> = Vec::new();
        for t in &inner.triggers {
            if let Some(re) = inner.compiled.get(&t.id)
                && re.is_match(&text)
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
            // Terminal notification channel: bell + the matching line.
            print!("\x07");
            println!("⚑ trigger [{pattern}] → {text}");
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
        let snippet = inner
            .lines
            .iter()
            .find(|l| l.seq == seq)
            .map(|l| l.text.clone())
            .unwrap_or_default();
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
        let matchers: Vec<Option<Regex>> = terms
            .iter()
            .map(|t| compile(t, regex).ok())
            .collect();

        let inner = self.inner.lock().unwrap();
        let mut out = Vec::new();
        let mut total = 0;
        for line in &inner.lines {
            if let Some(s) = stream
                && line.stream != s
            {
                continue;
            }
            let hit = |m: &Option<Regex>| m.as_ref().is_some_and(|re| re.is_match(&line.text));
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
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("Failed to create {}", dir.display()))?;
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
        Ok(s) => serde_json::from_str(&s)
            .with_context(|| format!("Failed to parse {}", path.display())),
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
        // Use a unique command so the persisted sidecar doesn't collide.
        let cmd = format!("test-cmd-{}", std::process::id());
        Arc::new(WatchState::new(&cmd, 100).unwrap())
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
