pub mod artifactory;
pub mod browse;
pub mod docker;
pub mod ecr;
pub mod nexus;
pub mod s3;

use crate::config::{RegistryConfig, RegistryKind, infer_registry_kind};
use anyhow::{Context, Result};
use std::collections::HashMap;
use std::path::Path;

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
    log: &mut Vec<String>,
) -> Result<()> {
    use std::process::Stdio;
    use tokio::process::Command;

    let mut cmd = Command::new("bash");
    cmd.arg(script)
        .envs(env_vars)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let output = cmd.output().await?;
    push_output_lines(log, &output.stdout, "");
    push_output_lines(log, &output.stderr, "[stderr] ");

    if !output.status.success() {
        anyhow::bail!(
            "Script '{}' failed with exit code {:?}",
            script,
            output.status.code()
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
    log: &mut Vec<String>,
) -> Result<()> {
    use std::process::Stdio;
    use tokio::process::Command;

    let output = Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .current_dir(cwd)
        .envs(env_vars)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .with_context(|| format!("Failed to spawn shell for command: {cmd}"))?;

    push_output_lines(log, &output.stdout, "");
    push_output_lines(log, &output.stderr, "[stderr] ");

    if !output.status.success() {
        anyhow::bail!("Command failed (exit {:?}): {}", output.status.code(), cmd);
    }
    Ok(())
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
        let visible = line.rsplit('\r').next().unwrap_or(line).trim_end();
        if visible.is_empty() {
            continue;
        }
        log.push(format!("{prefix}{visible}"));
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
