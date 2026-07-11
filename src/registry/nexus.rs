use super::RegistryOpOptions;
use crate::config::NexusFormat;
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

pub async fn push(opts: &RegistryOpOptions<'_>, log: &mut Vec<String>) -> Result<()> {
    match opts.registry_config.nexus_format()? {
        NexusFormat::Raw => raw_push(opts, log).await,
        NexusFormat::Npm => npm_push(opts, log).await,
        NexusFormat::Pypi => pypi_push(opts, log).await,
    }
}

pub async fn pull(opts: &RegistryOpOptions<'_>, log: &mut Vec<String>) -> Result<()> {
    match opts.registry_config.nexus_format()? {
        NexusFormat::Raw => raw_pull(opts, log).await,
        // npm/pypi are pulled with their native clients (npm install / pip),
        // which resolve by package name+version rather than a fixed path, so
        // ciabatta doesn't manage those downloads.
        format => anyhow::bail!(
            "`ciabatta pull` supports only raw Nexus repositories; \
             this registry is format '{}'. Pull {} packages with their native \
             client (npm install / pip install).",
            format_name(format),
            format_name(format),
        ),
    }
}

fn format_name(f: NexusFormat) -> &'static str {
    match f {
        NexusFormat::Raw => "raw",
        NexusFormat::Npm => "npm",
        NexusFormat::Pypi => "pypi",
    }
}

// ─── Raw repositories (HTTP PUT / GET) ──────────────────────────────────────

async fn raw_push(opts: &RegistryOpOptions<'_>, log: &mut Vec<String>) -> Result<()> {
    let url = opts.registry_config.nexus_object_url(opts.remote_path);
    log.push(format!(
        "Nexus raw push: {} -> {}",
        opts.local_path.display(),
        url
    ));

    if opts.dry_run {
        log.push(format!(
            "[dry-run] would HTTP PUT {} to {}",
            opts.local_path.display(),
            url
        ));
        return Ok(());
    }

    let data = tokio::fs::read(opts.local_path)
        .await
        .with_context(|| format!("Failed to read {}", opts.local_path.display()))?;

    let client = build_client(opts.registry_config.tls_verify)?;
    let mut req = client.put(&url).body(data);
    if let Some((user, pass)) = super::registry_credentials(opts.registry_name, opts.env_vars) {
        req = req.basic_auth(user, Some(pass));
    }
    let resp = req
        .send()
        .await
        .with_context(|| format!("HTTP PUT to {} failed", url))?;

    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("Nexus push failed: HTTP {} - {}", status, body);
    }
    log.push(format!("Nexus push succeeded: HTTP {}", status));
    Ok(())
}

async fn raw_pull(opts: &RegistryOpOptions<'_>, log: &mut Vec<String>) -> Result<()> {
    let url = opts.registry_config.nexus_object_url(opts.remote_path);
    log.push(format!(
        "Nexus raw pull: {} -> {}",
        url,
        opts.local_path.display()
    ));

    if opts.dry_run {
        log.push(format!(
            "[dry-run] would HTTP GET {} -> {}",
            url,
            opts.local_path.display()
        ));
        return Ok(());
    }

    let client = build_client(opts.registry_config.tls_verify)?;
    let mut req = client.get(&url);
    if let Some((user, pass)) = super::registry_credentials(opts.registry_name, opts.env_vars) {
        req = req.basic_auth(user, Some(pass));
    }
    let resp = req
        .send()
        .await
        .with_context(|| format!("HTTP GET from {} failed", url))?;

    let status = resp.status();
    if !status.is_success() {
        anyhow::bail!("Nexus pull failed: HTTP {}", status);
    }

    let bytes = resp.bytes().await?;
    if let Some(parent) = opts.local_path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    tokio::fs::write(opts.local_path, &bytes).await?;
    log.push(format!(
        "Downloaded {} bytes to {}",
        bytes.len(),
        opts.local_path.display()
    ));
    Ok(())
}

// ─── npm repositories (`npm publish`) ───────────────────────────────────────

async fn npm_push(opts: &RegistryOpOptions<'_>, log: &mut Vec<String>) -> Result<()> {
    let repo_url = opts.registry_config.nexus_repo_url();
    // npm wants the registry URL with a trailing slash.
    let registry_arg = format!("{}/", repo_url.trim_end_matches('/'));
    let artifact = opts.local_path;

    log.push(format!(
        "Nexus npm publish: {} -> {}",
        artifact.display(),
        registry_arg
    ));

    if opts.dry_run {
        log.push(format!(
            "[dry-run] would run: npm publish {} --registry {}",
            artifact.display(),
            registry_arg
        ));
        return Ok(());
    }

    // npm reads auth from an npmrc keyed by the registry host+path; write a
    // throwaway userconfig so we don't touch the user's ~/.npmrc.
    let npmrc = write_npm_userconfig(opts, &registry_arg).await?;

    let artifact_str = artifact.to_string_lossy();
    let npmrc_str = npmrc.to_string_lossy();
    let args = [
        "publish",
        artifact_str.as_ref(),
        "--registry",
        &registry_arg,
        "--userconfig",
        npmrc_str.as_ref(),
    ];
    let result = super::run_command("npm", &args, opts.env_vars, log).await;

    // Best-effort cleanup of the temp npmrc (it holds credentials).
    let _ = tokio::fs::remove_file(&npmrc).await;

    result.context("npm publish failed")
}

/// Write a temporary npmrc carrying registry + auth for `registry_arg`, and
/// return its path. Prefers a `CIABATTA_<REG>_TOKEN` bearer token, falling back
/// to basic-auth `CIABATTA_<REG>_USER` / `_PASS`.
async fn write_npm_userconfig(opts: &RegistryOpOptions<'_>, registry_arg: &str) -> Result<PathBuf> {
    // npm auth keys are scoped by the registry URL with the scheme stripped.
    let scoped = registry_arg
        .trim_start_matches("https://")
        .trim_start_matches("http://");

    let mut contents = format!("registry={registry_arg}\n//{scoped}:always-auth=true\n");

    let token_key = format!("CIABATTA_{}_TOKEN", super::cred_key(opts.registry_name));
    if let Some(token) = opts.env_vars.get(&token_key) {
        contents.push_str(&format!("//{scoped}:_authToken={token}\n"));
    } else if let Some((user, pass)) =
        super::registry_credentials(opts.registry_name, opts.env_vars)
    {
        let auth = base64_encode(format!("{user}:{pass}").as_bytes());
        contents.push_str(&format!("//{scoped}:_auth={auth}\n"));
    }

    let path = std::env::temp_dir().join(format!(
        "ciabatta-npmrc-{}-{}",
        std::process::id(),
        opts.registry_name
    ));
    tokio::fs::write(&path, contents)
        .await
        .with_context(|| format!("Failed to write temporary npmrc at {}", path.display()))?;
    Ok(path)
}

// ─── pypi repositories (`twine upload`) ─────────────────────────────────────

async fn pypi_push(opts: &RegistryOpOptions<'_>, log: &mut Vec<String>) -> Result<()> {
    let repo_url = opts.registry_config.nexus_repo_url();
    let upload_url = format!("{}/", repo_url.trim_end_matches('/'));

    // A directory artifact (the usual `dist/`) uploads all the distributions it
    // contains; a single file uploads just that one.
    let files = distribution_files(opts.local_path)?;
    if files.is_empty() {
        anyhow::bail!(
            "No distribution files found at {} to upload with twine",
            opts.local_path.display()
        );
    }

    log.push(format!(
        "Nexus pypi upload: {} file(s) -> {}",
        files.len(),
        upload_url
    ));

    if opts.dry_run {
        for f in &files {
            log.push(format!("[dry-run] would twine upload {}", f.display()));
        }
        return Ok(());
    }

    let mut args: Vec<String> = vec![
        "upload".to_string(),
        "--repository-url".to_string(),
        upload_url,
    ];
    if let Some((user, pass)) = super::registry_credentials(opts.registry_name, opts.env_vars) {
        args.push("-u".to_string());
        args.push(user);
        args.push("-p".to_string());
        args.push(pass);
    }
    for f in &files {
        args.push(f.to_string_lossy().into_owned());
    }

    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    super::run_command("twine", &arg_refs, opts.env_vars, log)
        .await
        .context("twine upload failed")
}

/// The distribution files to hand to twine: the immediate files inside a
/// directory artifact, or the single file itself.
fn distribution_files(local: &Path) -> Result<Vec<PathBuf>> {
    if local.is_dir() {
        let mut files = Vec::new();
        for entry in std::fs::read_dir(local)
            .with_context(|| format!("Failed to read directory {}", local.display()))?
        {
            let path = entry?.path();
            if path.is_file() {
                files.push(path);
            }
        }
        files.sort();
        Ok(files)
    } else {
        Ok(vec![local.to_path_buf()])
    }
}

fn build_client(tls_verify: bool) -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .danger_accept_invalid_certs(!tls_verify)
        .build()
        .map_err(Into::into)
}

/// Minimal standard-alphabet base64 encoder (no padding shortcuts), used for
/// npm basic-auth `_auth` tokens without pulling in a crate.
fn base64_encode(input: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(ALPHABET[(n >> 18 & 0x3f) as usize] as char);
        out.push(ALPHABET[(n >> 12 & 0x3f) as usize] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[(n >> 6 & 0x3f) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[(n & 0x3f) as usize] as char
        } else {
            '='
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::base64_encode;

    #[test]
    fn base64_matches_known_vectors() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foob"), "Zm9vYg==");
        assert_eq!(base64_encode(b"user:pass"), "dXNlcjpwYXNz");
    }
}
