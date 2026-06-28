use anyhow::Result;
use super::{RegistryOpOptions, run_command};

pub async fn push(opts: &RegistryOpOptions<'_>, log: &mut Vec<String>) -> Result<()> {
    let dest = format_s3_url(opts.registry_config.url.trim_end_matches('/'), opts.remote_path);
    log.push(format!("S3 push: {} -> {}", opts.local_path.display(), dest));

    if opts.dry_run {
        log.push(format!("[dry-run] would run: aws s3 cp {} {}", opts.local_path.display(), dest));
        return Ok(());
    }

    run_command(
        "aws",
        &["s3", "cp", &opts.local_path.to_string_lossy(), &dest],
        opts.env_vars,
        log,
    )
    .await
}

pub async fn pull(opts: &RegistryOpOptions<'_>, log: &mut Vec<String>) -> Result<()> {
    let src = format_s3_url(opts.registry_config.url.trim_end_matches('/'), opts.remote_path);
    log.push(format!("S3 pull: {} -> {}", src, opts.local_path.display()));

    if opts.dry_run {
        log.push(format!("[dry-run] would run: aws s3 cp {} {}", src, opts.local_path.display()));
        return Ok(());
    }

    if let Some(parent) = opts.local_path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }

    run_command(
        "aws",
        &["s3", "cp", &src, &opts.local_path.to_string_lossy()],
        opts.env_vars,
        log,
    )
    .await
}

fn format_s3_url(base: &str, path: &str) -> String {
    if path.starts_with("s3://") {
        return path.to_string();
    }
    // If the base URL looks like a bucket (s3://bucket or https://bucket.s3...)
    // use aws s3 cp s3://bucket/path style
    if base.starts_with("s3://") {
        format!("{}/{}", base, path.trim_start_matches('/'))
    } else {
        // Assume path is a full S3 key; prepend s3://
        format!("s3://{}", path.trim_start_matches('/'))
    }
}
