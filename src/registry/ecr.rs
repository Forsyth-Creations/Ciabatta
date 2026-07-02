use super::{RegistryOpOptions, run_command};
use anyhow::Result;

pub async fn push(opts: &RegistryOpOptions<'_>, log: &mut Vec<String>) -> Result<()> {
    let image = resolve_image(opts);
    log.push(format!("ECR push: {}", image));

    if opts.dry_run {
        log.push(format!(
            "[dry-run] would run: {} push {}",
            opts.container_cmd, image
        ));
        return Ok(());
    }

    run_command(opts.container_cmd, &["push", &image], opts.env_vars, log).await
}

pub async fn pull(opts: &RegistryOpOptions<'_>, log: &mut Vec<String>) -> Result<()> {
    let image = resolve_image(opts);
    log.push(format!("ECR pull: {}", image));

    if opts.dry_run {
        log.push(format!(
            "[dry-run] would run: {} pull {}",
            opts.container_cmd, image
        ));
        return Ok(());
    }

    run_command(opts.container_cmd, &["pull", &image], opts.env_vars, log).await
}

/// ECR auto-login via `aws ecr get-login-password`. Used as the default `login`
/// stage when the recipe has no `login` override and the registry has no
/// `login_script`.
pub(super) async fn ecr_login(opts: &RegistryOpOptions<'_>, log: &mut Vec<String>) -> Result<()> {
    if opts.dry_run {
        log.push("[dry-run] would run: aws ecr get-login-password | docker login".to_string());
        return Ok(());
    }
    // Derive registry hostname from URL for `aws ecr get-login-password`.
    let registry = opts
        .registry_config
        .url
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .trim_end_matches('/');

    log.push(format!("ECR auto-login for {}", registry));

    // aws ecr get-login-password | docker login --username AWS --password-stdin <registry>
    use std::process::Stdio;
    use tokio::process::Command;

    let token_output = Command::new("aws")
        .args(["ecr", "get-login-password"])
        .envs(opts.env_vars)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await?;

    if !token_output.status.success() {
        anyhow::bail!(
            "aws ecr get-login-password failed: {}",
            String::from_utf8_lossy(&token_output.stderr)
        );
    }
    let password = String::from_utf8(token_output.stdout)?;

    let mut login_cmd = Command::new(opts.container_cmd);
    login_cmd
        .args(["login", "--username", "AWS", "--password-stdin", registry])
        .envs(opts.env_vars)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = login_cmd.spawn()?;
    use tokio::io::AsyncWriteExt;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(password.trim().as_bytes()).await?;
    }
    let out = child.wait_with_output().await?;
    super::push_output_lines(log, &out.stdout, "");
    if !out.status.success() {
        anyhow::bail!(
            "docker login to ECR failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    Ok(())
}

fn resolve_image(opts: &RegistryOpOptions<'_>) -> String {
    let base = opts.registry_config.url.trim_end_matches('/');
    let path = opts.remote_path.trim_start_matches('/');
    if path.is_empty() {
        base.to_string()
    } else {
        format!("{}/{}", base, path)
    }
}
