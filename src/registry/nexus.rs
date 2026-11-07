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
        NexusFormat::Npm => npm_pull(opts, log).await,
        NexusFormat::Pypi => pypi_pull(opts, log).await,
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
    if opts.remote_path.trim_matches('/').is_empty() {
        anyhow::bail!(
            "A pull from a raw Nexus repository needs `publish_path`: the file or folder to fetch."
        );
    }
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
    let resp = authed(client.get(&url), opts)
        .send()
        .await
        .with_context(|| format!("HTTP GET from {} failed", url))?;

    let status = resp.status();
    // A folder has no URL of its own: Nexus answers 404, and only its search
    // knows what is under it. This is how a directory artifact comes back down,
    // having gone up one file at a time.
    if status == reqwest::StatusCode::NOT_FOUND {
        let found = folder_assets(&client, opts).await?;
        if found.is_empty() {
            anyhow::bail!("Nexus pull failed: nothing found at {url} (HTTP 404)");
        }
        log.push(format!("Downloading {} file(s) from {url}", found.len()));
        for asset in &found {
            let local = opts.local_path.join(&asset.relative);
            download(&client, opts, &asset.url, &local).await?;
        }
        log.push(format!(
            "Downloaded {} file(s) to {}",
            found.len(),
            opts.local_path.display()
        ));
        return Ok(());
    }
    if !status.is_success() {
        anyhow::bail!("Nexus pull failed: HTTP {}", status);
    }

    // Into a folder that already exists: keep the file's own name.
    let local = match opts.local_path.is_dir() {
        true => opts.local_path.join(
            opts.remote_path
                .trim_end_matches('/')
                .rsplit('/')
                .next()
                .unwrap_or("artifact"),
        ),
        false => opts.local_path.to_path_buf(),
    };
    let bytes = resp.bytes().await?;
    write_file(&local, &bytes).await?;
    log.push(format!(
        "Downloaded {} bytes to {}",
        bytes.len(),
        local.display()
    ));
    Ok(())
}

/// Attach the registry's credentials to a request, when it has any.
fn authed(req: reqwest::RequestBuilder, opts: &RegistryOpOptions<'_>) -> reqwest::RequestBuilder {
    match super::registry_credentials(opts.registry_name, opts.env_vars) {
        Some((user, pass)) => req.basic_auth(user, Some(pass)),
        None => req,
    }
}

async fn write_file(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    tokio::fs::write(path, bytes)
        .await
        .with_context(|| format!("Failed to write {}", path.display()))
}

async fn download(
    client: &reqwest::Client,
    opts: &RegistryOpOptions<'_>,
    url: &str,
    local: &Path,
) -> Result<()> {
    let resp = authed(client.get(url), opts)
        .send()
        .await
        .with_context(|| format!("HTTP GET from {url} failed"))?;
    let status = resp.status();
    if !status.is_success() {
        anyhow::bail!("Nexus pull of {url} failed: HTTP {status}");
    }
    write_file(local, &resp.bytes().await?).await
}

/// One file under a folder of a raw repository.
#[derive(Debug, PartialEq, Eq)]
struct FolderAsset {
    /// Where it goes, relative to the folder being pulled.
    relative: PathBuf,
    url: String,
}

/// What the Nexus search API calls a page of assets.
#[derive(serde::Deserialize)]
struct AssetPage {
    items: Vec<AssetItem>,
    #[serde(rename = "continuationToken")]
    continuation: Option<String>,
}

#[derive(serde::Deserialize)]
struct AssetItem {
    path: String,
}

/// Whether anything is published under `opts.remote_path` as a folder.
pub async fn folder_exists(opts: &RegistryOpOptions<'_>) -> Result<bool> {
    let client = build_client(opts.registry_config.tls_verify)?;
    Ok(!folder_assets(&client, opts).await?.is_empty())
}

/// Every file under `opts.remote_path`, asked of the Nexus search API. Empty when
/// there is none, and for a registry that isn't a Nexus repository.
async fn folder_assets(
    client: &reqwest::Client,
    opts: &RegistryOpOptions<'_>,
) -> Result<Vec<FolderAsset>> {
    let config = opts.registry_config;
    let repo_url = config.nexus_repo_url();
    let Some((host, repository)) = repo_url.split_once("/repository/") else {
        return Ok(Vec::new());
    };
    let repository = repository.trim_matches('/');
    let prefix = match config.base_path.as_deref().map(|p| p.trim_matches('/')) {
        Some(base) if !base.is_empty() => {
            format!("{base}/{}", opts.remote_path.trim_matches('/'))
        }
        _ => opts.remote_path.trim_matches('/').to_string(),
    };

    let search = format!("{host}/service/rest/v1/search/assets");
    // Which spelling matches depends on the Nexus version: some store a path
    // with a leading slash and some without.
    for name in [format!("{prefix}/*"), format!("/{prefix}/*")] {
        let mut assets = Vec::new();
        let mut continuation: Option<String> = None;
        loop {
            let mut req = client
                .get(&search)
                .query(&[("repository", repository), ("name", name.as_str())]);
            if let Some(token) = &continuation {
                req = req.query(&[("continuationToken", token.as_str())]);
            }
            let resp = authed(req, opts)
                .send()
                .await
                .with_context(|| format!("HTTP GET from {search} failed"))?;
            let status = resp.status();
            if !status.is_success() {
                anyhow::bail!("Listing {prefix} in Nexus failed: HTTP {status}");
            }
            let page: AssetPage = resp
                .json()
                .await
                .with_context(|| format!("Unexpected answer from {search}"))?;
            for item in page.items {
                if let Some(asset) = folder_asset(&repo_url, &prefix, &item.path)? {
                    assets.push(asset);
                }
            }
            continuation = page.continuation;
            if continuation.is_none() {
                break;
            }
        }
        if !assets.is_empty() {
            return Ok(assets);
        }
    }
    Ok(Vec::new())
}

/// Place one search result under the folder being pulled. A path that would
/// land outside it is refused, since it decides where a file gets written.
fn folder_asset(repo_url: &str, prefix: &str, path: &str) -> Result<Option<FolderAsset>> {
    let path = path.trim_start_matches('/');
    let Some(relative) = path.strip_prefix(prefix).and_then(|r| r.strip_prefix('/')) else {
        return Ok(None);
    };
    let relative = PathBuf::from(relative);
    let inside = relative
        .components()
        .all(|part| matches!(part, std::path::Component::Normal(_)));
    if !inside {
        anyhow::bail!("Refusing to write '{path}' from Nexus: it leads out of the destination");
    }
    Ok(Some(FolderAsset {
        relative,
        url: format!("{}/{path}", repo_url.trim_end_matches('/')),
    }))
}

// ─── Packages, pulled with their own tools ──────────────────────────────────

/// The package a pull names in `publish_path`, for an error that says so when
/// it doesn't. A push reads this from the package itself; a pull has no package.
fn package_spec<'a>(opts: &RegistryOpOptions<'a>, example: &str) -> Result<&'a str> {
    let spec = opts.remote_path.trim();
    if spec.is_empty() {
        anyhow::bail!(
            "A pull from this Nexus repository needs `publish_path` to name the package, \
             e.g. \"{example}\"."
        );
    }
    Ok(spec)
}

/// `name@1.2.3` as pip spells it. `name==1.2.3`, or a bare name for the newest
/// version, passes through.
fn pip_requirement(spec: &str) -> String {
    match spec.split_once('@') {
        Some((name, version)) if !name.is_empty() && !version.trim().is_empty() => {
            format!("{}=={}", name.trim(), version.trim())
        }
        _ => spec.to_string(),
    }
}

/// `https://user:pass@host/repository/pypi-hosted/simple/`: pip's way in, escaped
/// by the URL type so a password holding `@` or `/` survives.
fn pip_index_url(repo_url: &str, creds: Option<(String, String)>) -> Result<reqwest::Url> {
    let mut url = reqwest::Url::parse(&format!("{}/simple/", repo_url.trim_end_matches('/')))
        .with_context(|| format!("'{repo_url}' is not a URL"))?;
    if let Some((user, pass)) = creds {
        let refused = url.set_username(&user).is_err() || url.set_password(Some(&pass)).is_err();
        if refused {
            anyhow::bail!("Can't put credentials on '{repo_url}'");
        }
    }
    Ok(url)
}

async fn pypi_pull(opts: &RegistryOpOptions<'_>, log: &mut Vec<String>) -> Result<()> {
    let spec = pip_requirement(package_spec(opts, "my-package@1.2.3")?);
    let repo_url = opts.registry_config.nexus_repo_url();
    let dest = opts.local_path;
    log.push(format!(
        "Nexus pypi pull: {spec} from {repo_url} -> {}",
        dest.display()
    ));
    if opts.dry_run {
        log.push(format!("[dry-run] would pip download {spec}"));
        return Ok(());
    }
    tokio::fs::create_dir_all(dest)
        .await
        .with_context(|| format!("Failed to create {}", dest.display()))?;

    let index = pip_index_url(
        &repo_url,
        super::registry_credentials(opts.registry_name, opts.env_vars),
    )?;
    let dest_str = dest.to_string_lossy();
    let mut args = vec![
        "download",
        spec.as_str(),
        "--no-deps",
        "--dest",
        dest_str.as_ref(),
    ];
    // pip won't talk to a plain-HTTP index, or one with a bad certificate,
    // unless told to trust the host.
    let host = index.host_str().unwrap_or_default().to_string();
    if index.scheme() == "http" || !opts.registry_config.tls_verify {
        args.push("--trusted-host");
        args.push(&host);
    }
    // In the environment rather than on the command line, which is logged.
    let mut env = opts.env_vars.clone();
    env.insert("PIP_INDEX_URL".to_string(), index.to_string());
    env.insert("PIP_NO_INPUT".to_string(), "1".to_string());
    super::run_command("pip", &args, &env, log)
        .await
        .context("pip download failed")
}

async fn npm_pull(opts: &RegistryOpOptions<'_>, log: &mut Vec<String>) -> Result<()> {
    let spec = package_spec(opts, "@acme/sdk@1.2.3")?;
    let repo_url = opts.registry_config.nexus_repo_url();
    let registry_arg = format!("{}/", repo_url.trim_end_matches('/'));
    log.push(format!(
        "Nexus npm pull: {spec} from {registry_arg} -> {}",
        opts.local_path.display()
    ));
    if opts.dry_run {
        log.push(format!("[dry-run] would npm pack {spec}"));
        return Ok(());
    }

    // `artifact: vendor/sdk.tgz` names the file; anything else is the folder the
    // tarball lands in under npm's own name for it.
    let named = opts.local_path.to_string_lossy();
    let named = named.ends_with(".tgz") || named.ends_with(".tar.gz");
    let dest = match named {
        true => opts.local_path.parent().unwrap_or(Path::new(".")),
        false => opts.local_path,
    };
    tokio::fs::create_dir_all(dest)
        .await
        .with_context(|| format!("Failed to create {}", dest.display()))?;

    let npmrc = write_npm_userconfig(opts, &registry_arg).await?;
    let dest_str = dest.to_string_lossy();
    let npmrc_str = npmrc.to_string_lossy();
    let args = [
        "pack",
        spec,
        "--registry",
        &registry_arg,
        "--userconfig",
        npmrc_str.as_ref(),
        "--pack-destination",
        dest_str.as_ref(),
    ];
    let result = super::run_command("npm", &args, opts.env_vars, log).await;
    let _ = tokio::fs::remove_file(&npmrc).await;
    result.context("npm pack failed")?;

    if named {
        // npm prints the name it chose as the last thing on stdout.
        let packed = log
            .iter()
            .rev()
            .find(|line| line.ends_with(".tgz") && !line.starts_with('[') && !line.starts_with('+'))
            .cloned()
            .context("npm pack didn't say which file it wrote")?;
        tokio::fs::rename(dest.join(packed.trim()), opts.local_path)
            .await
            .with_context(|| {
                format!(
                    "Failed to move the tarball to {}",
                    opts.local_path.display()
                )
            })?;
    }
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

    let mut contents = format!("registry={registry_arg}\n");
    if !opts.registry_config.tls_verify {
        contents.push_str("strict-ssl=false\n");
    }

    let token_key = format!("CIABATTA_{}_TOKEN", super::cred_key(opts.registry_name));
    if let Some(token) = opts.env_vars.get(&token_key) {
        contents.push_str(&format!("//{scoped}:_authToken={token}\n"));
    } else if let Some((user, pass)) =
        super::registry_credentials(opts.registry_name, opts.env_vars)
    {
        let auth = base64_encode(format!("{user}:{pass}").as_bytes());
        contents.push_str(&format!("//{scoped}:_auth={auth}\n"));
    }

    // Unique per call, since steps run side by side and each deletes its own;
    // and readable only by us, since it holds credentials.
    let path = std::env::temp_dir().join(format!("ciabatta-npmrc-{}", uuid::Uuid::new_v4()));
    let mut options = tokio::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    let write = async {
        let mut file = options.open(&path).await?;
        tokio::io::AsyncWriteExt::write_all(&mut file, contents.as_bytes()).await
    };
    write
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
    // In the environment rather than as `-p`: the command line is logged.
    let mut env = opts.env_vars.clone();
    if let Some((user, pass)) = super::registry_credentials(opts.registry_name, opts.env_vars) {
        env.insert("TWINE_USERNAME".to_string(), user);
        env.insert("TWINE_PASSWORD".to_string(), pass);
    }
    args.push("--non-interactive".to_string());
    for f in &files {
        args.push(f.to_string_lossy().into_owned());
    }

    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    super::run_command("twine", &arg_refs, &env, log)
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
/// npm basic-auth `_auth` tokens — and by the AI chat's clipboard, which
/// hands the terminal its text base64-encoded — without pulling in a crate.
pub(crate) fn base64_encode(input: &[u8]) -> String {
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
    use super::*;

    #[test]
    fn a_package_spec_is_spelled_the_way_pip_spells_it() {
        assert_eq!(pip_requirement("sdk@1.2.3"), "sdk==1.2.3");
        assert_eq!(pip_requirement("sdk==1.2.3"), "sdk==1.2.3");
        assert_eq!(pip_requirement("sdk"), "sdk");
    }

    #[test]
    fn pip_credentials_are_escaped_into_the_index_url() {
        let url = pip_index_url(
            "https://nexus.example.com/repository/pypi/",
            Some(("ada".to_string(), "p@ss/word".to_string())),
        )
        .unwrap();
        assert_eq!(
            url.as_str(),
            "https://ada:p%40ss%2Fword@nexus.example.com/repository/pypi/simple/"
        );
    }

    #[test]
    fn a_folder_asset_lands_under_the_folder_and_never_outside_it() {
        let repo = "https://n/repository/raw";
        let asset = folder_asset(repo, "team/app", "/team/app/sub/b.txt")
            .unwrap()
            .unwrap();
        assert_eq!(asset.relative, Path::new("sub/b.txt"));
        assert_eq!(asset.url, "https://n/repository/raw/team/app/sub/b.txt");

        // Merely sharing the prefix isn't being under it.
        assert!(
            folder_asset(repo, "team/app", "team/app2/x")
                .unwrap()
                .is_none()
        );
        assert!(folder_asset(repo, "team/app", "team/app/../../x").is_err());
    }

    /// The directory case end to end, against a stand-in that behaves as Nexus
    /// does: a folder's own URL is a 404, and only the search knows what's in it.
    #[tokio::test]
    async fn a_folder_comes_down_file_by_file_with_credentials() {
        use axum::{Json, Router, extract::Query, http::HeaderMap, routing::get};
        use std::collections::HashMap;

        fn authorised(headers: &HeaderMap) -> bool {
            headers.get("authorization").and_then(|v| v.to_str().ok())
                == Some("Basic YWRhOnNlY3JldA==")
        }
        let app = Router::new()
            .route(
                "/service/rest/v1/search/assets",
                get(
                    |headers: HeaderMap, Query(q): Query<HashMap<String, String>>| async move {
                        assert!(authorised(&headers));
                        assert_eq!(q["repository"], "raw");
                        Json(match q["name"].as_str() {
                            "team/app/*" => serde_json::json!({
                                "items": [{"path": "team/app/a.txt"}, {"path": "team/app/sub/b.txt"}],
                                "continuationToken": null,
                            }),
                            _ => serde_json::json!({"items": [], "continuationToken": null}),
                        })
                    },
                ),
            )
            .route(
                "/repository/raw/team/app/a.txt",
                get(|headers: HeaderMap| async move {
                    assert!(authorised(&headers));
                    "first"
                }),
            )
            .route(
                "/repository/raw/team/app/sub/b.txt",
                get(|headers: HeaderMap| async move {
                    assert!(authorised(&headers));
                    "second"
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await });

        let dest = std::env::temp_dir().join(format!("ciabatta-nexus-{}", uuid::Uuid::new_v4()));
        let config: crate::config::RegistryConfig = serde_json::from_value(serde_json::json!({
            "url": format!("http://{addr}"),
            "repository": "raw",
            "type": "nexus",
        }))
        .unwrap();
        let env = std::collections::HashMap::from([
            ("CIABATTA_NEXUS_USER".to_string(), "ada".to_string()),
            ("CIABATTA_NEXUS_PASS".to_string(), "secret".to_string()),
        ]);
        let opts = RegistryOpOptions {
            registry_name: "nexus",
            registry_config: &config,
            local_path: &dest,
            remote_path: "team/app",
            local_image: None,
            env_vars: &env,
            dry_run: false,
            container_cmd: "docker",
        };

        let mut log = Vec::new();
        pull(&opts, &mut log).await.unwrap();
        assert_eq!(
            std::fs::read_to_string(dest.join("a.txt")).unwrap(),
            "first"
        );
        assert_eq!(
            std::fs::read_to_string(dest.join("sub/b.txt")).unwrap(),
            "second"
        );

        let missing = RegistryOpOptions {
            remote_path: "team/nope",
            ..opts
        };
        let err = pull(&missing, &mut Vec::new()).await.unwrap_err();
        assert!(format!("{err:#}").contains("nothing found"), "{err:#}");

        let _ = std::fs::remove_dir_all(&dest);
    }

    #[tokio::test]
    async fn a_package_pull_without_a_name_says_what_to_add() {
        let config: crate::config::RegistryConfig = serde_json::from_value(serde_json::json!({
            "url": "http://127.0.0.1:9",
            "repository": "npm",
            "format": "npm",
        }))
        .unwrap();
        let env = std::collections::HashMap::new();
        let opts = RegistryOpOptions {
            registry_name: "sdk",
            registry_config: &config,
            local_path: Path::new("vendor"),
            remote_path: "",
            local_image: None,
            env_vars: &env,
            dry_run: true,
            container_cmd: "docker",
        };
        let err = pull(&opts, &mut Vec::new()).await.unwrap_err();
        assert!(format!("{err:#}").contains("publish_path"), "{err:#}");
    }

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
