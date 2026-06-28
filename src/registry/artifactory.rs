use anyhow::{Context, Result};
use super::RegistryOpOptions;

/// Artifactory uses the same HTTP PUT/GET pattern as Nexus.
pub async fn push(opts: &RegistryOpOptions<'_>, log: &mut Vec<String>) -> Result<()> {
    let url = format!("{}/{}", opts.registry_config.url.trim_end_matches('/'), opts.remote_path.trim_start_matches('/'));
    log.push(format!("Artifactory push: {} -> {}", opts.local_path.display(), url));

    if opts.dry_run {
        log.push(format!("[dry-run] would HTTP PUT {} to {}", opts.local_path.display(), url));
        return Ok(());
    }

    let data = tokio::fs::read(opts.local_path)
        .await
        .with_context(|| format!("Failed to read {}", opts.local_path.display()))?;

    let client = build_client(opts.registry_config.tls_verify)?;
    let resp = client
        .put(&url)
        .body(data)
        .send()
        .await
        .with_context(|| format!("HTTP PUT to {} failed", url))?;

    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("Artifactory push failed: HTTP {} - {}", status, body);
    }
    log.push(format!("Artifactory push succeeded: HTTP {}", status));
    Ok(())
}

pub async fn pull(opts: &RegistryOpOptions<'_>, log: &mut Vec<String>) -> Result<()> {
    let url = format!("{}/{}", opts.registry_config.url.trim_end_matches('/'), opts.remote_path.trim_start_matches('/'));
    log.push(format!("Artifactory pull: {} -> {}", url, opts.local_path.display()));

    if opts.dry_run {
        log.push(format!("[dry-run] would HTTP GET {} -> {}", url, opts.local_path.display()));
        return Ok(());
    }

    let client = build_client(opts.registry_config.tls_verify)?;
    let resp = client
        .get(&url)
        .send()
        .await
        .with_context(|| format!("HTTP GET from {} failed", url))?;

    let status = resp.status();
    if !status.is_success() {
        anyhow::bail!("Artifactory pull failed: HTTP {}", status);
    }

    let bytes = resp.bytes().await?;
    if let Some(parent) = opts.local_path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    tokio::fs::write(opts.local_path, &bytes).await?;
    log.push(format!("Downloaded {} bytes to {}", bytes.len(), opts.local_path.display()));
    Ok(())
}

fn build_client(tls_verify: bool) -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .danger_accept_invalid_certs(!tls_verify)
        .build()
        .map_err(Into::into)
}
