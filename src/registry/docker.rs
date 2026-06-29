use super::{RegistryOpOptions, run_command};
use anyhow::Result;

pub async fn push(opts: &RegistryOpOptions<'_>, log: &mut Vec<String>) -> Result<()> {
    let image = resolve_image(opts);
    log.push(format!("Docker push: {}", image));

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
    log.push(format!("Docker pull: {}", image));

    if opts.dry_run {
        log.push(format!(
            "[dry-run] would run: {} pull {}",
            opts.container_cmd, image
        ));
        return Ok(());
    }

    run_command(opts.container_cmd, &["pull", &image], opts.env_vars, log).await
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
