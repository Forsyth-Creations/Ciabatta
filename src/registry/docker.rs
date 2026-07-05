use super::{RegistryOpOptions, run_command, tag_image};
use anyhow::Result;

pub async fn push(opts: &RegistryOpOptions<'_>, log: &mut Vec<String>) -> Result<()> {
    let image = resolve_image(opts);

    // When a local image is configured, retag it to the remote reference first,
    // so a locally-built `name:tag` can be pushed without hardcoding the
    // registry URL into the build.
    if let Some(local) = opts.local_image {
        tag_image(opts, local, &image, log).await?;
    }

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

    if !opts.dry_run {
        run_command(opts.container_cmd, &["pull", &image], opts.env_vars, log).await?;
    } else {
        log.push(format!(
            "[dry-run] would run: {} pull {}",
            opts.container_cmd, image
        ));
    }

    // Retag the pulled remote image back to the configured local reference.
    if let Some(local) = opts.local_image {
        tag_image(opts, &image, local, log).await?;
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
