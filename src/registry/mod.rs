pub mod artifactory;
pub mod browse;
pub mod docker;
pub mod ecr;
pub mod nexus;
pub mod s3;

use crate::color;
use crate::config::{RegistryConfig, RegistryKind, infer_registry_kind};
use anyhow::{Context, Result};
use std::collections::HashMap;
use std::path::Path;
use tokio::sync::mpsc::UnboundedSender;

/// A destination for command output lines.
///
/// Lines are always accumulated into `lines` (for error context and final
/// display). When `live` is set, each line is *also* forwarded immediately as
/// it is produced, so a watching UI (the run GUI / TUI) can show output while
/// a long-running process is still executing instead of only after it exits.
pub struct LogSink<'a> {
    lines: &'a mut Vec<String>,
    live: Option<UnboundedSender<String>>,
}

impl<'a> LogSink<'a> {
    /// A sink that only accumulates — no live forwarding.
    pub fn buffered(lines: &'a mut Vec<String>) -> Self {
        Self { lines, live: None }
    }

    /// A sink that accumulates and forwards each line to `live` as it arrives.
    pub fn streaming(lines: &'a mut Vec<String>, live: UnboundedSender<String>) -> Self {
        Self {
            lines,
            live: Some(live),
        }
    }

    /// Record one fully-formed log line, forwarding it live if wired.
    pub fn push(&mut self, line: String) {
        if let Some(tx) = &self.live {
            // A closed receiver just means the UI went away; keep accumulating.
            let _ = tx.send(line.clone());
        }
        self.lines.push(line);
    }

    /// Record one raw output line, collapsing carriage-return progress frames
    /// the same way [`push_output_lines`] does, and skipping blanks.
    fn push_raw(&mut self, raw_line: &str, prefix: &str) {
        if let Some(visible) = clean_line(raw_line) {
            self.push(format!("{prefix}{visible}"));
        }
    }
}

/// Reduce one newline-delimited output line to what a terminal would ultimately
/// display, trimmed, or `None` if that comes to nothing.
///
/// A progress bar is one line rewritten over and over, and there are two ways
/// tools do the rewriting. The old one is a carriage return. The other — vite,
/// esbuild, cargo, anything built on a modern terminal library — is to erase
/// the line and move the cursor back to column one with escape sequences, which
/// a `\r` split never sees. Miss those and every frame survives, so a
/// forty-second build arrives as a single line tens of thousands of characters
/// wide, and the one thing anybody wanted from it — the last frame — is at the
/// far end of it.
///
/// So the line is replayed: text accumulates, and anything that returns to the
/// start of the line or wipes it discards what came before. What's left is the
/// state a terminal would be showing when the newline arrived.
fn clean_line(line: &str) -> Option<String> {
    let visible = last_frame(line);
    let visible = visible.trim_end();
    if visible.is_empty() {
        None
    } else {
        Some(visible.to_string())
    }
}

/// The text a terminal would still be displaying at the end of `line`.
///
/// Only the sequences that *destroy* what precedes them are acted on — a
/// carriage return, a move to column one, and an erase covering the start of
/// the line. Everything else, colour included, is left exactly where it was:
/// this collapses progress frames, it does not strip formatting.
fn last_frame(line: &str) -> String {
    let mut kept = String::with_capacity(line.len());
    let mut rest = line;

    while let Some(at) = rest.find(['\r', '\u{1b}']) {
        let (before, from) = rest.split_at(at);
        kept.push_str(before);

        // `\r\n` is a line ending that got this far, not a rewrite.
        if let Some(after) = from.strip_prefix("\r\n") {
            kept.push('\n');
            rest = after;
            continue;
        }
        if let Some(after) = from.strip_prefix('\r') {
            kept.clear();
            rest = after;
            continue;
        }

        let Some(escape) = csi(from) else {
            // Not a CSI (or a truncated one): keep the byte and move on rather
            // than dropping output nobody asked us to interpret.
            kept.push_str(&from[..1]);
            rest = &from[1..];
            continue;
        };

        if escape.wipes_line_start() {
            kept.clear();
        } else {
            kept.push_str(escape.text);
        }
        rest = &from[escape.text.len()..];
    }

    kept.push_str(rest);
    kept
}

/// One CSI escape at the front of a string: `ESC [ params intermediates final`.
struct Csi<'a> {
    /// The whole sequence, as written.
    text: &'a str,
    params: &'a str,
    final_byte: u8,
}

impl Csi<'_> {
    /// Whether a terminal acting on this would destroy the start of the line.
    fn wipes_line_start(&self) -> bool {
        match self.final_byte {
            // Cursor to an absolute column: only column one (the default)
            // starts the line over. `ESC[40G` is a tool laying out a table.
            b'G' | b'`' => matches!(self.params, "" | "0" | "1"),
            // Erase in line: 1 clears to the cursor, 2 clears all of it.
            // 0 (the default) clears to the *end* and leaves the start alone.
            b'K' => matches!(self.params, "1" | "2"),
            _ => false,
        }
    }
}

/// Parse a CSI sequence at the start of `s`, if there is a complete one.
fn csi(s: &str) -> Option<Csi<'_>> {
    let body = s.strip_prefix("\u{1b}[")?;
    let params_len = body
        .bytes()
        .take_while(|b| matches!(b, 0x30..=0x3f))
        .count();
    let intermediates = body[params_len..]
        .bytes()
        .take_while(|b| matches!(b, 0x20..=0x2f))
        .count();
    let final_byte = *body.as_bytes().get(params_len + intermediates)?;
    if !(0x40..=0x7e).contains(&final_byte) {
        return None;
    }
    let len = "\u{1b}[".len() + params_len + intermediates + 1;
    Some(Csi {
        text: &s[..len],
        params: &body[..params_len],
        final_byte,
    })
}

/// Drive a spawned child to completion, streaming its stdout and stderr into
/// `sink` line-by-line as they are produced. Reading both pipes concurrently
/// avoids a deadlock where a child blocks writing to a full stderr pipe while we
/// only drain stdout.
async fn stream_child_output(
    mut child: tokio::process::Child,
    sink: &mut LogSink<'_>,
) -> Result<std::process::ExitStatus> {
    use tokio::io::{AsyncBufReadExt, BufReader};

    let mut out = child.stdout.take().map(|s| BufReader::new(s).lines());
    let mut err = child.stderr.take().map(|s| BufReader::new(s).lines());

    loop {
        tokio::select! {
            res = async { out.as_mut().unwrap().next_line().await }, if out.is_some() => {
                match res? {
                    Some(line) => sink.push_raw(&line, ""),
                    None => out = None,
                }
            }
            res = async { err.as_mut().unwrap().next_line().await }, if err.is_some() => {
                match res? {
                    Some(line) => sink.push_raw(&line, "[stderr] "),
                    None => err = None,
                }
            }
            else => break,
        }
    }

    Ok(child.wait().await?)
}

/// Shared options for a registry operation.
pub struct RegistryOpOptions<'a> {
    pub registry_name: &'a str,
    pub registry_config: &'a RegistryConfig,
    pub local_path: &'a Path,
    pub remote_path: &'a str,
    /// Docker/ECR only: the local image reference to retag to the remote target
    /// before pushing (see [`crate::config::SimpleRecipe::local_image`]).
    pub local_image: Option<&'a str>,
    pub env_vars: &'a HashMap<String, String>,
    pub dry_run: bool,
    pub container_cmd: &'a str,
}

/// Perform the main push (upload/publish) action for a registry.
///
/// Authentication is handled separately by the pipeline's `login` stage, so
/// this only performs the transfer itself.
pub async fn push(opts: &RegistryOpOptions<'_>, log: &mut Vec<String>) -> Result<()> {
    match infer_registry_kind(opts.registry_name, opts.registry_config) {
        RegistryKind::Nexus | RegistryKind::Generic => nexus::push(opts, log).await,
        RegistryKind::S3 => s3::push(opts, log).await,
        RegistryKind::Artifactory => artifactory::push(opts, log).await,
        RegistryKind::Docker => docker::push(opts, log).await,
        RegistryKind::Ecr => ecr::push(opts, log).await,
    }
}

/// Perform the main pull (download) action for a registry.
pub async fn pull(opts: &RegistryOpOptions<'_>, log: &mut Vec<String>) -> Result<()> {
    match infer_registry_kind(opts.registry_name, opts.registry_config) {
        RegistryKind::Nexus | RegistryKind::Generic => nexus::pull(opts, log).await,
        RegistryKind::S3 => s3::pull(opts, log).await,
        RegistryKind::Artifactory => artifactory::pull(opts, log).await,
        RegistryKind::Docker => docker::pull(opts, log).await,
        RegistryKind::Ecr => ecr::pull(opts, log).await,
    }
}

/// Best-effort check for whether the artifact at `opts.remote_path` already
/// exists in the registry.
///
/// Returns `Ok(Some(true|false))` for registries we can cheaply probe over HTTP
/// (Nexus / Artifactory / generic), and `Ok(None)` for kinds we can't (Docker,
/// ECR, S3) — signalling the caller to skip any commit-fallback logic for them.
pub async fn exists(opts: &RegistryOpOptions<'_>) -> Result<Option<bool>> {
    match infer_registry_kind(opts.registry_name, opts.registry_config) {
        // Only raw Nexus repos expose a stable per-artifact URL to probe; npm and
        // pypi resolve by package name+version, so we can't cheaply HEAD them.
        RegistryKind::Nexus
            if opts.registry_config.nexus_format()? != crate::config::NexusFormat::Raw =>
        {
            Ok(None)
        }
        RegistryKind::Nexus | RegistryKind::Artifactory | RegistryKind::Generic => {
            Ok(Some(http_exists(opts).await?))
        }
        _ => Ok(None),
    }
}

/// HEAD the artifact URL to see whether it exists (2xx → yes, 404 → no).
async fn http_exists(opts: &RegistryOpOptions<'_>) -> Result<bool> {
    // For plain Artifactory/Generic registries (no `repository`/`base_path`),
    // this reduces to `<url>/<remote_path>`, matching the transfer URL.
    let url = opts.registry_config.nexus_object_url(opts.remote_path);
    let client = reqwest::Client::builder()
        .danger_accept_invalid_certs(!opts.registry_config.tls_verify)
        .build()?;
    let mut req = client.head(&url);
    if let Some((user, pass)) = registry_credentials(opts.registry_name, opts.env_vars) {
        req = req.basic_auth(user, Some(pass));
    }
    let resp = req
        .send()
        .await
        .with_context(|| format!("HEAD {url} failed"))?;
    tracing::debug!(%url, status = %resp.status(), "existence probe");
    Ok(resp.status().is_success())
}

/// Environment-variable key suffix for a registry's credentials, e.g. the
/// registry named `nexus` yields `NEXUS`, used in `CIABATTA_NEXUS_USER` /
/// `CIABATTA_NEXUS_PASS`.
fn cred_key(registry_name: &str) -> String {
    registry_name
        .to_uppercase()
        .replace(|c: char| !c.is_ascii_alphanumeric(), "_")
}

/// Resolve `CIABATTA_<REGISTRY>_USER` / `CIABATTA_<REGISTRY>_PASS` for a
/// registry, if both are present in the environment.
pub fn registry_credentials(
    registry_name: &str,
    env_vars: &HashMap<String, String>,
) -> Option<(String, String)> {
    let key = cred_key(registry_name);
    let user = env_vars.get(&format!("CIABATTA_{key}_USER"))?.clone();
    let pass = env_vars.get(&format!("CIABATTA_{key}_PASS"))?.clone();
    Some((user, pass))
}

/// The default `login` stage: used when a recipe defines neither a `login`
/// override nor a registry `login_script`.
///
/// Credentials come from `CIABATTA_<REGISTRY>_USER` / `_PASS`:
///   - Nexus / Artifactory: applied as HTTP basic auth at request time, so here
///     we only report whether they're present.
///   - Docker: `docker login` with the credentials.
///   - ECR: `aws ecr get-login-password` auto-login.
///   - S3: defers to the standard AWS credential chain.
///
/// Returns `Ok(true)` if it performed a login action, `Ok(false)` if there was
/// nothing to do.
pub async fn default_login(opts: &RegistryOpOptions<'_>, log: &mut Vec<String>) -> Result<bool> {
    let key = cred_key(opts.registry_name);
    match infer_registry_kind(opts.registry_name, opts.registry_config) {
        RegistryKind::Nexus | RegistryKind::Artifactory | RegistryKind::Generic => {
            if registry_credentials(opts.registry_name, opts.env_vars).is_some() {
                log.push(format!(
                    "Using CIABATTA_{key}_USER / CIABATTA_{key}_PASS for HTTP basic auth"
                ));
                Ok(true)
            } else {
                log.push(format!(
                    "No credentials set (CIABATTA_{key}_USER / CIABATTA_{key}_PASS); \
                     proceeding unauthenticated"
                ));
                Ok(false)
            }
        }
        RegistryKind::Docker => docker_login(opts, log).await,
        RegistryKind::Ecr => {
            ecr::ecr_login(opts, log).await?;
            Ok(true)
        }
        RegistryKind::S3 => {
            log.push(
                "S3 uses the standard AWS credential chain (AWS_ACCESS_KEY_ID, …); \
                 no ciabatta login performed"
                    .to_string(),
            );
            Ok(false)
        }
    }
}

/// `docker login <host> -u <user> --password-stdin` using the registry's
/// `CIABATTA_<REGISTRY>_USER` / `_PASS` credentials.
async fn docker_login(opts: &RegistryOpOptions<'_>, log: &mut Vec<String>) -> Result<bool> {
    let key = cred_key(opts.registry_name);
    let Some((user, pass)) = registry_credentials(opts.registry_name, opts.env_vars) else {
        log.push(format!(
            "No credentials set (CIABATTA_{key}_USER / CIABATTA_{key}_PASS); skipping docker login"
        ));
        return Ok(false);
    };

    let host = opts
        .registry_config
        .url
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .trim_end_matches('/');

    log.push(format!("docker login {host} as {user}"));
    if opts.dry_run {
        log.push(format!(
            "[dry-run] would run: {} login {host} -u {user} --password-stdin",
            opts.container_cmd
        ));
        return Ok(true);
    }

    use std::process::Stdio;
    use tokio::io::AsyncWriteExt;
    use tokio::process::Command;

    let mut child = Command::new(opts.container_cmd)
        .args(["login", host, "-u", &user, "--password-stdin"])
        .envs(opts.env_vars)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("Failed to spawn {} login", opts.container_cmd))?;

    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(pass.as_bytes()).await?;
    }
    let out = child.wait_with_output().await?;
    push_output_lines(log, &out.stdout, "");
    if !out.status.success() {
        anyhow::bail!(
            "docker login to {host} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    Ok(true)
}

/// `<container> tag <from> <to>` — retag a local image to another reference.
///
/// Used by the Docker/ECR push (retag a locally-built image to its remote
/// repository reference before pushing) and pull (retag the pulled remote image
/// back to the recipe's local name).
pub(super) async fn tag_image(
    opts: &RegistryOpOptions<'_>,
    from: &str,
    to: &str,
    log: &mut Vec<String>,
) -> Result<()> {
    log.push(format!("Docker tag: {from} -> {to}"));
    if opts.dry_run {
        log.push(format!(
            "[dry-run] would run: {} tag {from} {to}",
            opts.container_cmd
        ));
        return Ok(());
    }
    run_command(opts.container_cmd, &["tag", from, to], opts.env_vars, log).await
}

pub async fn run_script(
    script: &str,
    env_vars: &HashMap<String, String>,
    sink: &mut LogSink<'_>,
) -> Result<()> {
    use std::process::Stdio;
    use tokio::process::Command;

    let mut command = Command::new("bash");
    // Colour first, the caller's environment second: a script that sets
    // `FORCE_COLOR=0` still means it.
    color::request(&mut command);
    let child = command
        .arg(script)
        .envs(env_vars)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("Failed to spawn script '{script}'"))?;

    let status = stream_child_output(child, sink).await?;

    if !status.success() {
        anyhow::bail!(
            "Script '{}' failed with exit code {:?}",
            script,
            status.code()
        );
    }
    Ok(())
}

/// Run an arbitrary shell command (`sh -c <cmd>`) from `cwd`, with the given
/// environment variables injected. Used by the stage-override mechanism.
pub async fn run_shell_command(
    cmd: &str,
    cwd: &Path,
    env_vars: &HashMap<String, String>,
    sink: &mut LogSink<'_>,
) -> Result<()> {
    run_shell_command_opts(cmd, cwd, env_vars, false, sink).await
}

/// [`run_shell_command`], plus control over what happens to the command when
/// the returned future is dropped.
///
/// `kill_on_drop` matters for the two ways a run can walk away from a command
/// that is still going: a step whose `timeout` expired, and a `persistent` step
/// whose background task is aborted when the graph finishes. In both cases the
/// process tree has to go with it, so the command is spawned into its own
/// process group and the whole group is signalled — see [`ProcessGroup`].
pub async fn run_shell_command_opts(
    cmd: &str,
    cwd: &Path,
    env_vars: &HashMap<String, String>,
    kill_on_drop: bool,
    sink: &mut LogSink<'_>,
) -> Result<()> {
    use std::process::Stdio;
    use tokio::process::Command;

    let mut command = Command::new("sh");
    // The command's stdout is a pipe, so it will assume nobody is watching and
    // turn colour off. Tell it otherwise — before `envs`, so a step that asked
    // for plain output keeps it.
    color::request(&mut command);
    command
        .arg("-c")
        .arg(cmd)
        .current_dir(cwd)
        .envs(env_vars)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    // Tokio's `kill_on_drop` signals only the pid it spawned, which is never
    // enough on its own — see [`ProcessGroup`]. On Windows it's actively
    // harmful: `taskkill /T` finds the tree by walking parent pids, so
    // terminating the shell first orphans everything under it and leaves
    // nothing to walk. There, `ProcessGroup` does the killing by itself.
    #[cfg(not(windows))]
    command.kill_on_drop(kill_on_drop);

    // Lead a new process group, so abandoning the command can take everything
    // it spawned with it rather than just the shell.
    #[cfg(unix)]
    if kill_on_drop {
        command.process_group(0);
    }

    let child = command
        .spawn()
        .with_context(|| format!("Failed to spawn shell for command: {cmd}"))?;

    let group = ProcessGroup::new(child.id(), kill_on_drop);
    let status = stream_child_output(child, sink).await?;
    // Reaped normally: the pid is free to be reused, so stop tracking it.
    group.disarm();

    if !status.success() {
        anyhow::bail!("Command failed (exit {:?}): {}", status.code(), cmd);
    }
    Ok(())
}

/// Kills a command's entire process group if it is dropped while still armed.
///
/// Tokio's `kill_on_drop` signals only the process it spawned. That process is
/// `sh`, which forks rather than execs for anything non-trivial, so a step's
/// real work — the compiler, the dev server — would survive being "killed" and
/// keep running long after ciabatta exited. Spawning into a fresh process group
/// and signalling the group is what actually stops it.
///
/// Windows has no process groups, and the gap is worse there rather than
/// better: `sh` is Git Bash, which runs the script in a `bash` child of its
/// own. Terminating the pid we hold leaves that `bash` — and the dev server or
/// compiler under it — running. Because the survivors inherited the stdout and
/// stderr pipe handles, the read end never reaches EOF either, so anything
/// still reading the abandoned command's output waits forever. `taskkill /T`
/// takes the whole tree and closes the pipes with it.
struct ProcessGroup {
    /// `None` once disarmed, or when there was nothing to track.
    pid: Option<u32>,
}

impl ProcessGroup {
    fn new(pid: Option<u32>, armed: bool) -> Self {
        Self {
            pid: if armed { pid } else { None },
        }
    }

    /// Stop tracking: the command finished and was reaped, so its pid may
    /// already belong to something else.
    fn disarm(mut self) {
        self.pid = None;
    }
}

impl Drop for ProcessGroup {
    fn drop(&mut self) {
        let Some(pid) = self.pid.take() else {
            return;
        };
        #[cfg(unix)]
        // SAFETY: `killpg` on a pid we spawned as a group leader and have not
        // yet reaped. A failure (the group is already gone) is nothing to act
        // on, which is why the result is discarded.
        unsafe {
            libc::killpg(pid as libc::pid_t, libc::SIGKILL);
        }
        #[cfg(windows)]
        {
            // `/T` is the whole point: it walks the tree from this pid and
            // takes the Git Bash shell, the `bash` under it, and whatever that
            // started. Waited on rather than fired and forgotten, so that by
            // the time this returns the pipes really are closed — a reader
            // parked on them is exactly what we're here to release. A failure
            // means the tree is already gone, which is the outcome we wanted.
            let _ = std::process::Command::new("taskkill")
                .args(["/PID", &pid.to_string(), "/T", "/F"])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
        }
        #[cfg(not(any(unix, windows)))]
        let _ = pid;
    }
}

/// Append captured command output to `log`, collapsing carriage-return
/// overwrites.
///
/// Tools like `aws s3 cp` draw a progress bar by rewriting the same line with
/// `\r` and no trailing newline. Rust's `str::lines()` splits only on `\n`, so
/// all those frames would otherwise arrive as one entry full of embedded `\r`s,
/// which the TUI then hands to the terminal and gets a garbled overwrite. For
/// each newline-delimited line we keep only the text after the final `\r` — the
/// state a terminal would ultimately display — dropping any empty result so a
/// bare trailing `\r` doesn't add a blank line.
pub fn push_output_lines(log: &mut Vec<String>, raw: &[u8], prefix: &str) {
    for line in String::from_utf8_lossy(raw).lines() {
        if let Some(visible) = clean_line(line) {
            log.push(format!("{prefix}{visible}"));
        }
    }
}

/// Helper: stream a command, collecting output lines into `log`.
pub async fn run_command(
    program: &str,
    args: &[&str],
    env_vars: &HashMap<String, String>,
    log: &mut Vec<String>,
) -> Result<()> {
    use std::process::Stdio;
    use tokio::process::Command;

    log.push(format!("+ {} {}", program, args.join(" ")));

    let output = Command::new(program)
        .args(args)
        .envs(env_vars)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await?;

    push_output_lines(log, &output.stdout, "");
    push_output_lines(log, &output.stderr, "[stderr] ");

    if !output.status.success() {
        anyhow::bail!(
            "Command '{} {}' failed with exit code {:?}",
            program,
            args.join(" "),
            output.status.code()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn run_shell_command_streams_lines_before_exit() {
        // A command that prints, pauses, then prints again. The first line must
        // reach the live channel well before the whole command finishes —
        // otherwise the run GUI would sit at "(no output yet)" until exit.
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        let env = HashMap::new();
        let cwd = Path::new(".");

        let runner = tokio::spawn(async move {
            let mut lines = Vec::new();
            {
                let mut sink = LogSink::streaming(&mut lines, tx);
                run_shell_command("echo first; sleep 0.4; echo second", cwd, &env, &mut sink)
                    .await
                    .unwrap();
            }
            lines
        });

        // The first line arrives promptly, long before the ~0.4s command ends.
        let first = tokio::time::timeout(std::time::Duration::from_millis(250), rx.recv())
            .await
            .expect("first line should stream before the command exits")
            .expect("live channel stays open while the command runs");
        assert_eq!(first, "first");

        let all = runner.await.unwrap();
        assert_eq!(all, vec!["first".to_string(), "second".to_string()]);
    }

    #[test]
    fn cred_key_uppercases_and_sanitizes() {
        assert_eq!(cred_key("nexus"), "NEXUS");
        assert_eq!(cred_key("my-registry"), "MY_REGISTRY");
        assert_eq!(cred_key("ecr.prod"), "ECR_PROD");
    }

    #[test]
    fn credentials_resolved_by_registry_name() {
        let mut env = HashMap::new();
        env.insert("CIABATTA_NEXUS_USER".to_string(), "u".to_string());
        env.insert("CIABATTA_NEXUS_PASS".to_string(), "p".to_string());

        assert_eq!(
            registry_credentials("nexus", &env),
            Some(("u".to_string(), "p".to_string()))
        );
        // Different registry name → no credentials.
        assert_eq!(registry_credentials("docker", &env), None);
    }

    #[test]
    fn push_output_lines_collapses_carriage_return_progress() {
        let mut log = Vec::new();
        // A typical `aws s3 cp` progress stream: many `\r`-overwritten frames on
        // one line, then a final newline-terminated status.
        let raw = b"Completed 1.0 MiB/2.0 MiB\rCompleted 1.5 MiB/2.0 MiB\rCompleted 2.0 MiB/2.0 MiB\nupload: ./a to s3://b/a\n";
        push_output_lines(&mut log, raw, "");
        assert_eq!(
            log,
            vec![
                "Completed 2.0 MiB/2.0 MiB".to_string(),
                "upload: ./a to s3://b/a".to_string(),
            ]
        );
    }

    /// The modern spelling of a progress bar: erase the line, jump to column
    /// one, draw the next frame. `vite`, `esbuild` and `cargo` all do this, and
    /// missing it turns a whole build into one line thousands of characters
    /// wide with the answer buried at the end.
    #[test]
    fn push_output_lines_collapses_cursor_drawn_progress() {
        let mut log = Vec::new();
        let raw = concat!(
            "transforming (1) index.html",
            "\u{1b}[2K\u{1b}[1G",
            "transforming (94) react/index.js",
            "\u{1b}[2K\u{1b}[1G",
            "\u{1b}[32m✓\u{1b}[39m 1300 modules transformed.\n",
        );
        push_output_lines(&mut log, raw.as_bytes(), "");
        assert_eq!(
            log,
            vec!["\u{1b}[32m✓\u{1b}[39m 1300 modules transformed.".to_string()],
            "only the last frame survives — and its colours survive with it"
        );
    }

    /// Collapsing frames must not become stripping formatting: the escapes that
    /// paint are exactly what the run view is being asked to render.
    #[test]
    fn colour_escapes_are_carried_through_untouched() {
        let mut log = Vec::new();
        push_output_lines(&mut log, "\u{1b}[36mvite v5\u{1b}[39m\n".as_bytes(), "");
        assert_eq!(log, vec!["\u{1b}[36mvite v5\u{1b}[39m".to_string()]);
    }

    /// Only the erases and moves that destroy the start of the line count. A
    /// tool laying a line out in columns, or clearing to the end of it, is not
    /// starting over.
    #[test]
    fn cursor_moves_that_keep_the_line_are_left_alone() {
        assert_eq!(
            clean_line("name\u{1b}[40Gvalue").as_deref(),
            Some("name\u{1b}[40Gvalue"),
            "a jump to column 40 is a table, not a rewrite"
        );
        assert_eq!(
            clean_line("kept\u{1b}[0Kgone-to-the-right").as_deref(),
            Some("kept\u{1b}[0Kgone-to-the-right"),
            "erase-to-end leaves everything before the cursor"
        );
        assert_eq!(
            clean_line("dropped\u{1b}[1Kkept").as_deref(),
            Some("kept"),
            "erase-to-cursor wipes the start of the line"
        );

        // A truncated escape at the end of a chunk must not eat the output.
        assert_eq!(
            clean_line("still here\u{1b}[").as_deref(),
            Some("still here\u{1b}[")
        );
    }

    #[test]
    fn push_output_lines_applies_prefix_and_skips_blanks() {
        let mut log = Vec::new();
        // A bare trailing `\r` (cursor reset with no content) shouldn't add a line.
        push_output_lines(&mut log, b"warn: slow\n\r", "[stderr] ");
        assert_eq!(log, vec!["[stderr] warn: slow".to_string()]);
    }

    #[test]
    fn credentials_require_both_user_and_pass() {
        let mut env = HashMap::new();
        env.insert("CIABATTA_NEXUS_USER".to_string(), "u".to_string());
        assert_eq!(registry_credentials("nexus", &env), None);
    }
}
