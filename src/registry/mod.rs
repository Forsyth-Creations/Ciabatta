pub mod nexus;
pub mod s3;
pub mod artifactory;
pub mod docker;
pub mod ecr;

use std::collections::HashMap;
use std::path::Path;
use anyhow::Result;
use crate::config::{RegistryConfig, RegistryKind};

/// Shared options for a registry operation.
pub struct RegistryOpOptions<'a> {
    pub registry_name: &'a str,
    pub registry_config: &'a RegistryConfig,
    pub local_path: &'a Path,
    pub remote_path: &'a str,
    pub env_vars: &'a HashMap<String, String>,
    pub dry_run: bool,
    pub container_cmd: &'a str,
}

/// Perform a push (upload/publish) to a registry.
pub async fn push(opts: RegistryOpOptions<'_>, log: &mut Vec<String>) -> Result<()> {
    run_login_script(&opts, log).await?;

    let kind = RegistryKind::from(opts.registry_name);
    match kind {
        RegistryKind::Nexus | RegistryKind::Generic => {
            nexus::push(&opts, log).await
        }
        RegistryKind::S3 => {
            s3::push(&opts, log).await
        }
        RegistryKind::Artifactory => {
            artifactory::push(&opts, log).await
        }
        RegistryKind::Docker => {
            docker::push(&opts, log).await
        }
        RegistryKind::Ecr => {
            ecr::push(&opts, log).await
        }
    }
}

/// Perform a pull (download) from a registry.
pub async fn pull(opts: RegistryOpOptions<'_>, log: &mut Vec<String>) -> Result<()> {
    run_login_script(&opts, log).await?;

    let kind = RegistryKind::from(opts.registry_name);
    match kind {
        RegistryKind::Nexus | RegistryKind::Generic => {
            nexus::pull(&opts, log).await
        }
        RegistryKind::S3 => {
            s3::pull(&opts, log).await
        }
        RegistryKind::Artifactory => {
            artifactory::pull(&opts, log).await
        }
        RegistryKind::Docker => {
            docker::pull(&opts, log).await
        }
        RegistryKind::Ecr => {
            ecr::pull(&opts, log).await
        }
    }
}

async fn run_login_script(opts: &RegistryOpOptions<'_>, log: &mut Vec<String>) -> Result<()> {
    let Some(ref script) = opts.registry_config.login_script else {
        return Ok(());
    };

    log.push(format!("Running login script: {}", script));
    if opts.dry_run {
        log.push(format!("[dry-run] would run: {}", script));
        return Ok(());
    }

    run_script(script, opts.env_vars, log).await
}

pub async fn run_script(
    script: &str,
    env_vars: &HashMap<String, String>,
    log: &mut Vec<String>,
) -> Result<()> {
    use tokio::process::Command;
    use std::process::Stdio;

    let mut cmd = Command::new("bash");
    cmd.arg(script)
        .envs(env_vars)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let output = cmd.output().await?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    for line in stdout.lines() {
        log.push(line.to_string());
    }
    for line in stderr.lines() {
        log.push(format!("[stderr] {}", line));
    }

    if !output.status.success() {
        anyhow::bail!(
            "Script '{}' failed with exit code {:?}",
            script,
            output.status.code()
        );
    }
    Ok(())
}

/// Helper: stream a command, collecting output lines into `log`.
pub async fn run_command(
    program: &str,
    args: &[&str],
    env_vars: &HashMap<String, String>,
    log: &mut Vec<String>,
) -> Result<()> {
    use tokio::process::Command;
    use std::process::Stdio;

    log.push(format!("+ {} {}", program, args.join(" ")));

    let output = Command::new(program)
        .args(args)
        .envs(env_vars)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await?;

    for line in String::from_utf8_lossy(&output.stdout).lines() {
        log.push(line.to_string());
    }
    for line in String::from_utf8_lossy(&output.stderr).lines() {
        log.push(format!("[stderr] {}", line));
    }

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
