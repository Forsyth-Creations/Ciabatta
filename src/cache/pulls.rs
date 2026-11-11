//! The pull cache: the last copy of everything a `kind: pull` step fetched.
//!
//! Every pull from a file registry — Nexus (raw, npm, pypi), Artifactory, S3,
//! a generic HTTP endpoint — lands here first and is copied into the working
//! tree from here, so the cache always holds the newest copy that was fetched.
//! When the registry can't be reached the pull is served from that copy
//! instead, with a warning that says so: a laptop off the VPN still builds, and
//! nobody mistakes what it built for something fresh.
//!
//! `ciabatta <workflow> --fetch-only` fills it ahead of time without running
//! the workflow — the thing to do before getting on a plane — and
//! `--prefetch` fills it as the first node of a run.
//!
//! Docker and ECR pulls aren't kept here: an image goes into the container
//! engine's own store, which already serves it once it's been pulled.
//!
//! Layout under `<global root>/pulls/`:
//!
//! ```text
//! <key>/meta.json     where it came from and when
//! <key>/data/<name>   what the pull wrote: a file, or a folder of them
//! ```
//!
//! The key is the registry and the URL pulled from, nothing about the
//! checkout, so every project on the machine shares one copy of an artifact.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::config::{RegistryConfig, RegistryKind, infer_registry_kind};
use crate::registry::{self, RegistryOpOptions};

/// Under the global cache root.
const PULLS_DIR: &str = "pulls";
const META_FILE: &str = "meta.json";
const DATA_DIR: &str = "data";

/// What's recorded beside a cached pull.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Meta {
    pub registry: String,
    pub url: String,
    /// The name the pull's payload was written under, inside `data/`.
    pub name: String,
    /// RFC 3339: when it was fetched.
    pub fetched_at: String,
}

/// One cached pull, found on disk.
#[derive(Debug, Clone)]
pub struct Cached {
    pub meta: Meta,
    dir: PathBuf,
}

impl Cached {
    /// The file or folder the pull wrote.
    fn payload(&self) -> PathBuf {
        self.dir.join(DATA_DIR).join(&self.meta.name)
    }
}

/// `<global root>/pulls/`.
pub fn dir() -> Result<PathBuf> {
    Ok(super::store::global_root()?.join(PULLS_DIR))
}

/// Whether pulls from this registry go through the cache. Images don't: they
/// live in the container engine's store.
pub fn cacheable(registry_name: &str, config: &RegistryConfig) -> bool {
    !matches!(
        infer_registry_kind(registry_name, config),
        RegistryKind::Docker | RegistryKind::Ecr
    )
}

/// Whether a step is a pull ciabatta itself fetches into this cache: a
/// `kind: pull` with no `run:`/`script:` of its own, through a registry that
/// is defined and isn't an image registry. These are what `--fetch-only` runs
/// and what the `--prefetch` node fetches. A pull with its own command fetches
/// however that command does, which ciabatta can't do on its behalf.
pub fn fetchable(step: &crate::run::RunStep, config: &crate::config::CiabattaConfig) -> bool {
    if step.direction() != Some(crate::run::Direction::Pull)
        || step.script.is_some()
        || step.run.is_some()
    {
        return false;
    }
    step.registry
        .as_deref()
        .and_then(|name| config.registries.get(name).map(|cfg| cacheable(name, cfg)))
        .unwrap_or(false)
}

/// The URL a pull of `remote_path` fetches from, as the key sees it. For npm
/// and pypi the remote path is a package spec, which this folds in the same
/// way.
fn source_url(config: &RegistryConfig, remote_path: &str) -> String {
    config.nexus_object_url(remote_path)
}

/// The cache key for pulling `remote_path` from a registry.
fn key(registry_name: &str, config: &RegistryConfig, remote_path: &str) -> String {
    let source = format!("{registry_name}\n{}", source_url(config, remote_path));
    super::hash_bytes(source.as_bytes())[..32].to_string()
}

/// The cached copy of `remote_path`, if there is one.
pub fn lookup(registry_name: &str, config: &RegistryConfig, remote_path: &str) -> Option<Cached> {
    let dir = dir().ok()?.join(key(registry_name, config, remote_path));
    let raw = std::fs::read_to_string(dir.join(META_FILE)).ok()?;
    let meta: Meta = serde_json::from_str(&raw).ok()?;
    let cached = Cached { meta, dir };
    cached.payload().exists().then_some(cached)
}

/// The cached copy for a transfer's options.
fn lookup_for(opts: &RegistryOpOptions<'_>) -> Option<Cached> {
    lookup(opts.registry_name, opts.registry_config, opts.remote_path)
}

/// Pull from the registry into the cache, replacing what was there. The working
/// tree isn't touched.
pub async fn fetch(opts: &RegistryOpOptions<'_>, log: &mut Vec<String>) -> Result<Cached> {
    let root = dir()?;
    let key = key(opts.registry_name, opts.registry_config, opts.remote_path);
    // Unique per attempt, so two runs fetching the same thing don't write into
    // each other's half-finished copy.
    let nonce = chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default();
    let staging = root.join(format!("{key}.partial-{}-{nonce}", std::process::id()));
    let name = opts
        .local_path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "artifact".to_string());
    let data = staging.join(DATA_DIR);
    std::fs::create_dir_all(&data)
        .with_context(|| format!("Failed to create {}", data.display()))?;

    let local = data.join(&name);
    let staged = RegistryOpOptions {
        registry_name: opts.registry_name,
        registry_config: opts.registry_config,
        local_path: &local,
        remote_path: opts.remote_path,
        local_image: opts.local_image,
        env_vars: opts.env_vars,
        dry_run: false,
        container_cmd: opts.container_cmd,
    };
    let result = async {
        registry::pull(&staged, log).await?;
        if !local.exists() {
            bail!("the pull finished but wrote nothing to {}", local.display());
        }
        let meta = Meta {
            registry: opts.registry_name.to_string(),
            url: source_url(opts.registry_config, opts.remote_path),
            name: name.clone(),
            fetched_at: chrono::Utc::now().to_rfc3339(),
        };
        std::fs::write(
            staging.join(META_FILE),
            serde_json::to_string_pretty(&meta)?,
        )?;
        let dest = root.join(&key);
        let _ = std::fs::remove_dir_all(&dest);
        std::fs::rename(&staging, &dest)
            .with_context(|| format!("Failed to move the pull into {}", dest.display()))?;
        Ok(Cached { meta, dir: dest })
    }
    .await;
    if result.is_err() {
        let _ = std::fs::remove_dir_all(&staging);
    }
    result
}

/// Copy a cached pull to where the transfer wanted it, the way the pull itself
/// would have written it: a file into a folder that already exists keeps its
/// remote name, and a folder's files are merged into what's there.
pub fn restore(cached: &Cached, opts: &RegistryOpOptions<'_>) -> Result<()> {
    let payload = cached.payload();
    let target = opts.local_path;
    if payload.is_dir() {
        return copy_tree(&payload, target);
    }
    let target = match target.is_dir() {
        true => target.join(
            opts.remote_path
                .trim_end_matches('/')
                .rsplit('/')
                .next()
                .filter(|n| !n.is_empty())
                .unwrap_or(&cached.meta.name),
        ),
        false => target.to_path_buf(),
    };
    copy_file(&payload, &target)
}

/// Pull through the cache: fetch into it and copy out, or — when the registry
/// can't be reached — copy out what it already has, with a warning.
pub async fn pull(opts: &RegistryOpOptions<'_>, log: &mut Vec<String>) -> Result<()> {
    if opts.dry_run || !cacheable(opts.registry_name, opts.registry_config) {
        return registry::pull(opts, log).await;
    }
    match fetch(opts, log).await {
        Ok(cached) => {
            restore(&cached, opts)?;
            log.push(format!(
                "Copied to {} (a copy is kept in the pull cache)",
                opts.local_path.display()
            ));
            Ok(())
        }
        Err(e) => match lookup_for(opts) {
            Some(cached) => {
                log.push(stale_warning(
                    opts.registry_name,
                    &cached,
                    &format!("{e:#}"),
                ));
                restore(&cached, opts)
            }
            None => Err(e),
        },
    }
}

/// Serve a pull from the cache without trying the registry, because something
/// before it (logging in) already showed the registry can't be reached.
pub fn from_cache(opts: &RegistryOpOptions<'_>, reason: &str, log: &mut Vec<String>) -> Result<()> {
    let Some(cached) = lookup_for(opts) else {
        bail!(
            "couldn't reach '{}' ({reason}), and nothing from {} is in the pull cache — \
             run with --fetch-only (or --prefetch) while on the network to keep a copy",
            opts.registry_name,
            source_url(opts.registry_config, opts.remote_path)
        );
    };
    log.push(stale_warning(opts.registry_name, &cached, reason));
    restore(&cached, opts)
}

/// When `opts` can't be fetched but the cache has a copy: the warning to log
/// for leaving it at that copy. `None` when nothing is cached.
pub fn keep_cached(opts: &RegistryOpOptions<'_>, reason: &str) -> Option<String> {
    lookup_for(opts).map(|cached| stale_warning(opts.registry_name, &cached, reason))
}

/// How the warning on a pull served from the cache starts, so the terminal can
/// pick it out of the step's log and draw it as a warning.
pub const STALE_WARNING: &str = "warning: couldn't pull from '";

/// The warning a pull served from the cache carries.
fn stale_warning(registry_name: &str, cached: &Cached, reason: &str) -> String {
    format!(
        "{STALE_WARNING}{registry_name}' ({reason}). Using ONLY the cached copy \
         of {}, fetched {} — it may be out of date",
        cached.meta.url, cached.meta.fetched_at
    )
}

/// What the pull cache holds: how many artifacts, and how many bytes.
pub fn stats() -> Result<(usize, u64)> {
    let root = dir()?;
    let Ok(entries) = std::fs::read_dir(&root) else {
        return Ok((0, 0));
    };
    let mut count = 0;
    let mut bytes = 0;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.join(META_FILE).is_file() {
            count += 1;
        }
        bytes += size_of(&path);
    }
    Ok((count, bytes))
}

/// `ciabatta cache clean --pulls`: delete the whole pull cache — every
/// project's, since it's shared. A pull that can't reach its registry
/// afterwards has nothing to fall back on until it's fetched again.
pub fn clear() -> Result<usize> {
    let (count, _) = stats()?;
    let root = dir()?;
    match std::fs::remove_dir_all(&root) {
        Ok(()) => Ok(count),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(0),
        Err(e) => Err(e).with_context(|| format!("Failed to remove {}", root.display())),
    }
}

fn size_of(path: &Path) -> u64 {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() => std::fs::read_dir(path)
            .map(|entries| entries.flatten().map(|e| size_of(&e.path())).sum())
            .unwrap_or(0),
        Ok(meta) => meta.len(),
        Err(_) => 0,
    }
}

fn copy_file(from: &Path, to: &Path) -> Result<()> {
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create {}", parent.display()))?;
    }
    std::fs::copy(from, to)
        .with_context(|| format!("Failed to copy {} to {}", from.display(), to.display()))?;
    Ok(())
}

fn copy_tree(from: &Path, to: &Path) -> Result<()> {
    std::fs::create_dir_all(to).with_context(|| format!("Failed to create {}", to.display()))?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let dest = to.join(entry.file_name());
        match entry.file_type()?.is_dir() {
            true => copy_tree(&entry.path(), &dest)?,
            false => copy_file(&entry.path(), &dest)?,
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ciab_pulls_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn registry(url: &str) -> RegistryConfig {
        serde_yaml_ng::from_str(&format!(
            "url: {url}\ntype: nexus\nrepository: raw-hosted\n"
        ))
        .unwrap()
    }

    /// Put a payload in the cache by hand, as a successful fetch would have.
    fn seed(config: &RegistryConfig, remote: &str, name: &str, write: impl Fn(&Path)) -> Cached {
        let dir = dir().unwrap().join(key("nexus", config, remote));
        let _ = std::fs::remove_dir_all(&dir);
        let payload = dir.join(DATA_DIR).join(name);
        write(&payload);
        let meta = Meta {
            registry: "nexus".into(),
            url: source_url(config, remote),
            name: name.into(),
            fetched_at: "2026-11-01T00:00:00Z".into(),
        };
        std::fs::write(dir.join(META_FILE), serde_json::to_string(&meta).unwrap()).unwrap();
        lookup("nexus", config, remote).expect("seeded entry is found")
    }

    fn opts<'a>(
        config: &'a RegistryConfig,
        local: &'a Path,
        remote: &'a str,
        env: &'a HashMap<String, String>,
    ) -> RegistryOpOptions<'a> {
        RegistryOpOptions {
            registry_name: "nexus",
            registry_config: config,
            local_path: local,
            remote_path: remote,
            local_image: None,
            env_vars: env,
            dry_run: false,
            container_cmd: "docker",
        }
    }

    #[test]
    fn the_key_is_the_registry_and_what_was_pulled() {
        let a = registry("https://nexus.example");
        let b = registry("https://other.example");
        assert_eq!(key("nexus", &a, "x/app.tgz"), key("nexus", &a, "x/app.tgz"));
        assert_ne!(key("nexus", &a, "x/app.tgz"), key("nexus", &a, "y/app.tgz"));
        assert_ne!(key("nexus", &a, "x/app.tgz"), key("nexus", &b, "x/app.tgz"));
    }

    #[test]
    fn a_file_restores_to_its_path_or_into_an_existing_folder() {
        let config = registry("https://keyed-file.example");
        let remote = "team/app/abc/app.tgz";
        let cached = seed(&config, remote, "app.tgz", |p| {
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, b"payload").unwrap();
        });
        let tmp = scratch("file");
        let env = HashMap::new();

        let named = tmp.join("vendor/app.tgz");
        restore(&cached, &opts(&config, &named, remote, &env)).unwrap();
        assert_eq!(std::fs::read(&named).unwrap(), b"payload");

        let folder = tmp.join("drop");
        std::fs::create_dir_all(&folder).unwrap();
        restore(&cached, &opts(&config, &folder, remote, &env)).unwrap();
        assert_eq!(std::fs::read(folder.join("app.tgz")).unwrap(), b"payload");
    }

    #[test]
    fn a_folder_merges_into_whats_there() {
        let config = registry("https://keyed-folder.example");
        let remote = "team/site";
        let cached = seed(&config, remote, "site", |p| {
            std::fs::create_dir_all(p.join("assets")).unwrap();
            std::fs::write(p.join("index.html"), b"<html>").unwrap();
            std::fs::write(p.join("assets/app.js"), b"js").unwrap();
        });
        let tmp = scratch("folder");
        let site = tmp.join("site");
        std::fs::create_dir_all(&site).unwrap();
        std::fs::write(site.join("keep.txt"), b"mine").unwrap();

        restore(&cached, &opts(&config, &site, remote, &HashMap::new())).unwrap();
        assert_eq!(std::fs::read(site.join("assets/app.js")).unwrap(), b"js");
        assert_eq!(std::fs::read(site.join("keep.txt")).unwrap(), b"mine");
    }

    /// Off the network: the pull fails to connect, and the cached copy is used
    /// with a warning rather than failing the step.
    #[tokio::test]
    async fn an_unreachable_registry_falls_back_to_the_cache_with_a_warning() {
        // Port 9 on localhost: nothing listens, so the connection is refused.
        let config = registry("http://127.0.0.1:9");
        let remote = "team/app/offline.tgz";
        seed(&config, remote, "offline.tgz", |p| {
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, b"cached").unwrap();
        });
        let tmp = scratch("offline");
        let local = tmp.join("offline.tgz");
        let env = HashMap::new();
        let mut log = Vec::new();

        pull(&opts(&config, &local, remote, &env), &mut log)
            .await
            .unwrap();
        assert_eq!(std::fs::read(&local).unwrap(), b"cached");
        assert!(
            log.iter()
                .any(|l| l.starts_with("warning:") && l.contains("ONLY the cached copy")),
            "{log:?}"
        );
    }

    #[tokio::test]
    async fn an_unreachable_registry_with_nothing_cached_still_fails() {
        let config = registry("http://127.0.0.1:9");
        let tmp = scratch("miss");
        let local = tmp.join("never.tgz");
        let env = HashMap::new();
        let err = pull(
            &opts(&config, &local, "team/never-cached.tgz", &env),
            &mut Vec::new(),
        )
        .await
        .unwrap_err();
        assert!(format!("{err:#}").contains("127.0.0.1:9"), "{err:#}");
        assert!(!local.exists());
    }
}
