//! How a step's command is wired up: what it reads, how its output is read, and
//! what a debug run says about it when it goes quiet.
//!
//! Three things make a command that works in a terminal stall under ciabatta,
//! and each has its answer here:
//!
//! * **Its stdin.** A child inherits ours, which is a terminal — but a step
//!   runs in a process group of its own (so stopping it takes everything it
//!   started), and a background group that reads the terminal is sent
//!   `SIGTTIN` and suspended by the kernel. No error, no output: the step just
//!   stops, forever. Under the TUI the terminal is in raw mode with the TUI
//!   reading every key anyway, and under the daemon there is nobody there.
//!   So a step's stdin is empty ([`stdin`]): a command that asks a question
//!   sees end-of-file at once, and fails or takes its default instead of
//!   hanging. `ciabatta input` is how a step asks a question, in every view.
//! * **Its last line.** Output used to be read a whole line at a time, so a
//!   prompt — `Password: `, `Continue? [y/N] ` — never appeared: it has no
//!   newline. [`read_pipe`] passes on whatever has been written once the
//!   command has been quiet for [`PARTIAL_AFTER`].
//! * **Its buffering.** With a pipe for stdout, C's stdio and Python buffer in
//!   blocks rather than lines, so output arrives in bursts long after it was
//!   printed. Python is told not to ([`unbuffer`]); for C programs, `stdbuf
//!   -oL` is the fix, and the docs say so.

use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::sync::mpsc::UnboundedSender;

/// The stdin a step's command gets: nothing at all. See the module docs for
/// why a terminal is never an option.
pub fn stdin() -> std::process::Stdio {
    std::process::Stdio::null()
}

/// Ask runtimes that buffer a piped stdout not to. Call it before the step's
/// own environment goes on, so a step that sets the variable itself wins.
pub fn unbuffer(command: &mut tokio::process::Command) {
    command.env("PYTHONUNBUFFERED", "1");
}

/// How long an unfinished line waits for the rest of itself before it is
/// passed on as it stands. Long enough that a progress bar redrawing itself
/// isn't split into a line per frame; short enough that a prompt reads as one.
pub const PARTIAL_AFTER: Duration = Duration::from_millis(750);

/// Which of a command's pipes a line came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pipe {
    Out,
    Err,
}

/// Read `pipe` to its end, sending each line to `tx` as it completes — and a
/// line still waiting for its newline once nothing more has arrived for
/// [`PARTIAL_AFTER`].
///
/// Reading goes on after `tx`'s receiver has gone: a process the command left
/// running in the background still holds the pipe, and would die of a closed
/// one the next time it wrote.
pub async fn read_pipe(
    mut pipe: impl AsyncRead + Unpin,
    which: Pipe,
    tx: UnboundedSender<(Pipe, String)>,
) {
    let mut pending: Vec<u8> = Vec::new();
    let mut chunk = vec![0u8; 8 * 1024];
    let send = |bytes: &[u8]| {
        let text = String::from_utf8_lossy(bytes);
        let text = text.strip_suffix('\r').unwrap_or(&text);
        let _ = tx.send((which, text.to_string()));
    };
    loop {
        // A pipe read is cancel-safe: timing it out loses nothing.
        let read = if pending.is_empty() {
            pipe.read(&mut chunk).await
        } else {
            match tokio::time::timeout(PARTIAL_AFTER, pipe.read(&mut chunk)).await {
                Ok(read) => read,
                Err(_) => {
                    send(&pending);
                    pending.clear();
                    continue;
                }
            }
        };
        match read {
            Ok(0) | Err(_) => {
                if !pending.is_empty() {
                    send(&pending);
                }
                return;
            }
            Ok(n) => {
                pending.extend_from_slice(&chunk[..n]);
                let mut start = 0;
                while let Some(at) = pending[start..].iter().position(|&b| b == b'\n') {
                    send(&pending[start..start + at]);
                    start += at + 1;
                }
                pending.drain(..start);
            }
        }
    }
}

/// How long a command may go without printing anything before a debug run
/// says what it is doing.
pub const QUIET_NOTE: Duration = Duration::from_secs(30);

/// A debug note about a command that has printed nothing for `quiet`: its
/// processes, and what each is waiting on — enough to tell a slow build from a
/// command stopped at a prompt.
pub fn describe_quiet(pid: Option<u32>, quiet: Duration, cmd: &str) -> String {
    let mut out = format!(
        "no output for {} from `{}`",
        crate::runner::elapsed(quiet),
        first_line(cmd)
    );
    match pid {
        Some(pid) => {
            let tree = process_tree(pid);
            if tree.is_empty() {
                out.push_str(&format!(" (pid {pid})"));
            }
            for line in tree {
                out.push_str("\n    ");
                out.push_str(&line);
            }
        }
        None => out.push_str(" (no pid)"),
    }
    out
}

fn first_line(cmd: &str) -> String {
    let line = cmd.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
    let more = cmd.trim().lines().count() > 1;
    match more {
        true => format!("{} …", line.trim()),
        false => line.trim().to_string(),
    }
}

/// `pid` and everything under it, one line each: pid, state, what the kernel
/// says it is blocked in, and its command line. Linux only — elsewhere, empty.
#[cfg(target_os = "linux")]
fn process_tree(pid: u32) -> Vec<String> {
    use std::collections::HashMap;

    let read = |pid: u32, file: &str| std::fs::read_to_string(format!("/proc/{pid}/{file}")).ok();

    // Parent of every process, from `/proc/<pid>/stat`. The command name is in
    // parentheses and may hold spaces, so the fields are read after the last `)`.
    let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
    let mut states: HashMap<u32, (String, String)> = HashMap::new();
    if let Ok(entries) = std::fs::read_dir("/proc") {
        for entry in entries.flatten() {
            let Some(p) = entry
                .file_name()
                .to_str()
                .and_then(|s| s.parse::<u32>().ok())
            else {
                continue;
            };
            let Some(stat) = read(p, "stat") else {
                continue;
            };
            let Some(close) = stat.rfind(')') else {
                continue;
            };
            let name = stat[..close]
                .split_once('(')
                .map(|(_, n)| n.to_string())
                .unwrap_or_default();
            let mut fields = stat[close + 1..].split_whitespace();
            let state = fields.next().unwrap_or("?").to_string();
            let ppid = fields
                .next()
                .and_then(|f| f.parse::<u32>().ok())
                .unwrap_or(0);
            children.entry(ppid).or_default().push(p);
            states.insert(p, (state, name));
        }
    }

    let mut lines = Vec::new();
    let mut stack = vec![(pid, 0usize)];
    while let Some((p, depth)) = stack.pop() {
        let Some((state, name)) = states.get(&p) else {
            continue;
        };
        let wchan = read(p, "wchan").filter(|w| !w.is_empty() && w != "0");
        let cmdline = read(p, "cmdline")
            .map(|c| c.replace('\0', " ").trim().to_string())
            .filter(|c| !c.is_empty())
            .unwrap_or_else(|| name.clone());
        let stdin = std::fs::read_link(format!("/proc/{p}/fd/0"))
            .ok()
            .map(|t| t.to_string_lossy().into_owned());
        let mut line = format!("{}pid {p} [{}]", "  ".repeat(depth), state_word(state));
        if let Some(wchan) = &wchan {
            line.push_str(&format!(" in {wchan}"));
        }
        // A process asleep reading a terminal is waiting for somebody to type;
        // one stopped by a signal most likely tried to read one it doesn't own.
        if let Some(stdin) = &stdin
            && wchan.as_deref().is_some_and(|w| w.contains("tty"))
        {
            line.push_str(&format!(" — reading its stdin ({stdin})"));
        } else if matches!(state.as_str(), "T" | "t") {
            line.push_str(" — suspended by a signal (SIGTTIN if it read the terminal)");
        }
        line.push_str(&format!(": {}", truncate(&cmdline, 160)));
        lines.push(line);
        if let Some(kids) = children.get(&p) {
            for kid in kids.iter().rev() {
                stack.push((*kid, depth + 1));
            }
        }
    }
    lines
}

#[cfg(not(target_os = "linux"))]
fn process_tree(_pid: u32) -> Vec<String> {
    Vec::new()
}

#[cfg(target_os = "linux")]
fn state_word(state: &str) -> &'static str {
    match state {
        "R" => "running",
        "S" => "sleeping",
        "D" => "waiting on disk/IO",
        "T" | "t" => "stopped",
        "Z" => "zombie",
        _ => "other",
    }
}

#[cfg(target_os = "linux")]
fn truncate(text: &str, max: usize) -> String {
    match text.char_indices().nth(max) {
        Some((at, _)) => format!("{}…", &text[..at]),
        None => text.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn lines_of(input: &'static [u8]) -> Vec<String> {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        read_pipe(input, Pipe::Out, tx).await;
        let mut out = Vec::new();
        while let Ok((_, line)) = rx.try_recv() {
            out.push(line);
        }
        out
    }

    #[tokio::test]
    async fn complete_lines_arrive_whole_and_without_their_endings() {
        assert_eq!(
            lines_of(b"one\r\ntwo\nthree").await,
            ["one", "two", "three"]
        );
    }

    #[tokio::test]
    async fn a_prompt_without_a_newline_is_passed_on_once_the_command_goes_quiet() {
        let (mut writer, reader) = tokio::io::duplex(64);
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let reading = tokio::spawn(read_pipe(reader, Pipe::Err, tx));
        use tokio::io::AsyncWriteExt;
        writer.write_all(b"Password: ").await.unwrap();
        let got = tokio::time::timeout(PARTIAL_AFTER * 4, rx.recv())
            .await
            .expect("the prompt should arrive while the pipe is still open");
        assert_eq!(got, Some((Pipe::Err, "Password: ".to_string())));
        drop(writer);
        reading.await.unwrap();
    }

    #[test]
    fn a_long_script_is_named_by_its_first_line() {
        assert_eq!(first_line("\n  make all\n  make test\n"), "make all …");
        assert_eq!(first_line("cargo build"), "cargo build");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn this_process_shows_up_in_its_own_tree() {
        let tree = process_tree(std::process::id());
        assert!(
            tree[0].starts_with(&format!("pid {}", std::process::id())),
            "{tree:?}"
        );
    }
}
