//! Releases: build outputs published under a name and a version, and fetched
//! back by that version later.
//!
//! A cache entry is found by what went *into* a build — change one input and
//! it's a different entry, and the old one ages out when nobody asks for it.
//! That's the right rule for skipping work and the wrong one for shipping it:
//! a deploy wants "the files of v1.4.2", whatever the tree looks like today,
//! and wants them still to be there next month. So a release is a **labelled
//! cache entry** — the same manifest, the same verified artifacts, the same
//! remote cache to share them through — stored under a key derived from its
//! name and version instead of its inputs, and exempt from retention.
//!
//! Two step kinds put that on a workflow graph:
//!
//! ```yaml
//! # build.yaml — on a tagged commit in CI
//! steps:
//!   - name: compile
//!     run: make dist
//!   - name: publish
//!     kind: release-push
//!     needs: [compile]
//!     release:
//!       files: ["dist/**"]        # version: the git tag on HEAD, by default
//!
//! # deploy.yaml — any time later: `ciabatta deploy --release v1.4.2`
//! steps:
//!   - name: fetch
//!     kind: release-pull          # version: $CIABATTA_RELEASE, by default
//!   - name: ship
//!     needs: [fetch]
//!     run: ./scripts/deploy.sh dist/
//! ```
//!
//! The version is explicit by design. A push takes `release.version` (with
//! `{VAR}` substitution, so `"{CIABATTA_TAG}"` works), else
//! `$CIABATTA_RELEASE`, else the git tag pointing at HEAD — so a tag build in
//! CI publishes under its tag with no configuration at all. A pull takes
//! `release.version`, else `$CIABATTA_RELEASE` (`--release` on the command
//! line sets it) and never guesses: deploying "whatever was latest" is how the
//! wrong build ends up in production.
//!
//! A release is immutable unless asked otherwise. Pushing a version that
//! already exists with the same files is a no-op (a re-run CI job must not
//! fail); pushing it with *different* files is refused without
//! `release.overwrite: true`.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::cache::FileHash;
use crate::cache::store::{Build, Entry, Store};
use crate::config::CiabattaConfig;
use crate::registry::LogSink;
use crate::remote_cache::client::Client;

/// The variable a run's release version is read from — what `--release` sets.
pub const RELEASE_VAR: &str = "CIABATTA_RELEASE";

/// Where a release keeps its name, in the entry's recorded variables.
const NAME_VAR: &str = "CIABATTA_RELEASE_NAME";

/// The target every release entry is stored under starts with this, which is
/// how a release is told apart from an ordinary cache entry — and kept out of
/// retention's reach.
pub const TARGET_PREFIX: &str = "release:";

/// The `release:` block of a `kind: release-push` / `kind: release-pull` step.
#[derive(Debug, Deserialize, Serialize, Clone, Default, PartialEq)]
pub struct ReleaseSpec {
    /// Which release. Defaults to the step's sub-workspace, or the project's
    /// name for a plain project — so a package's build and deploy workflows
    /// agree without saying so, and a monorepo's packages don't collide.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The version, with `{VAR}` substitution. See the module docs for what
    /// it defaults to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// Push: globs for the files to publish, relative to the step's working
    /// directory. A directory means everything under it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<String>,
    /// Pull: where to write the files, relative to the step's working
    /// directory. Defaults to the directory itself, so files land where the
    /// push found them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub into: Option<String>,
    /// Push: replace an existing release of this version whose files differ.
    #[serde(default, skip_serializing_if = "crate::format::is_false")]
    pub overwrite: bool,
}

/// Which way a release step moves files.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Push,
    Pull,
}

impl Direction {
    /// The `kind:` that selects it, if `kind` is one of the two.
    pub fn of_kind(kind: &str) -> Option<Self> {
        if kind.eq_ignore_ascii_case("release-push") {
            Some(Direction::Push)
        } else if kind.eq_ignore_ascii_case("release-pull") {
            Some(Direction::Pull)
        } else {
            None
        }
    }
}

/// One release, as listed.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ReleaseInfo {
    pub name: String,
    pub version: String,
    pub created_at: String,
    pub files: usize,
    pub size: u64,
    #[serde(default)]
    pub commit: Option<String>,
}

impl ReleaseInfo {
    /// The release an entry holds, or `None` for an ordinary cache entry.
    pub fn of(entry: &Entry) -> Option<Self> {
        if !is_release(entry) {
            return None;
        }
        Some(ReleaseInfo {
            name: entry
                .env
                .get(NAME_VAR)
                .cloned()
                .unwrap_or_else(|| entry.target[TARGET_PREFIX.len()..].to_string()),
            version: entry.env.get(RELEASE_VAR).cloned().unwrap_or_default(),
            created_at: entry.created_at.clone(),
            files: entry.outputs.len(),
            size: entry.size,
            commit: entry.env.get("CIABATTA_COMMIT").cloned(),
        })
    }
}

/// Whether a stored entry is a release rather than a cache entry.
pub fn is_release(entry: &Entry) -> bool {
    entry.target.starts_with(TARGET_PREFIX)
}

/// The key a release is stored under: a function of its name and version
/// alone, so every machine computes the same one, and hex like every other
/// key so it is safe wherever keys become file names.
pub fn key(name: &str, version: &str) -> String {
    crate::cache::hash_bytes(format!("ciabatta-release\0{name}\0{version}").as_bytes())
}

/// The version a release step works with. See the module docs for the order.
pub fn resolve_version(
    declared: Option<&str>,
    env: &HashMap<String, String>,
    root: &Path,
    direction: Direction,
) -> Result<String> {
    if let Some(template) = declared.filter(|v| !v.trim().is_empty()) {
        let version = crate::config::substitute_vars(template, env)
            .with_context(|| format!("release.version '{template}'"))?;
        return check_version(version);
    }
    if let Some(version) = env.get(RELEASE_VAR).filter(|v| !v.trim().is_empty()) {
        return check_version(version.clone());
    }
    if direction == Direction::Push
        && let Some(tag) = crate::git::exact_tag(root)
    {
        return check_version(tag);
    }
    match direction {
        Direction::Push => bail!(
            "no release version: set `release.version` on the step, pass `--release <VERSION>` \
             (or set ${RELEASE_VAR}), or tag the commit being built"
        ),
        Direction::Pull => bail!(
            "no release version to pull: pass `--release <VERSION>` (or set ${RELEASE_VAR}), \
             or set `release.version` on the step"
        ),
    }
}

fn check_version(version: String) -> Result<String> {
    let version = version.trim().to_string();
    if version.is_empty() || version.len() > 128 || version.chars().any(char::is_control) {
        bail!("'{version}' is not a usable release version");
    }
    Ok(version)
}

/// The release name a step works with when it doesn't name one.
pub fn default_name(step_workspace: Option<&str>, config: &CiabattaConfig, root: &Path) -> String {
    step_workspace
        .filter(|w| !w.is_empty() && *w != ".")
        .map(str::to_string)
        .or_else(|| config.workspace.as_ref().and_then(|w| w.name.clone()))
        .or_else(|| root.file_name().map(|n| n.to_string_lossy().to_string()))
        .unwrap_or_else(|| "project".to_string())
}

/// The project's remote cache, connected, when it has one.
pub struct Shared {
    pub client: Client,
    pub project: String,
    pub url: String,
    pub read_only: bool,
}

/// Connect to the project's remote cache. `Ok(None)` when none is
/// configured; an error when one is configured and can't be reached — a
/// release is the one thing that must not quietly fall back to "local only".
pub async fn connect(config: &CiabattaConfig, root: &Path) -> Result<Option<Shared>> {
    let Some(remote) = config.cache.as_ref().and_then(|c| c.remote()).cloned() else {
        return Ok(None);
    };
    let client = Client::new(&remote.url, remote.tls_verify)?;
    let name = remote
        .name
        .clone()
        .or_else(|| config.workspace.as_ref().and_then(|w| w.name.clone()))
        .or_else(|| root.file_name().map(|n| n.to_string_lossy().to_string()))
        .unwrap_or_else(|| "project".to_string());
    let project = client
        .register(&name, remote.project.as_deref())
        .await
        .with_context(|| format!("couldn't reach the remote cache at {}", remote.url))?;
    Ok(Some(Shared {
        client,
        project: project.id,
        url: remote.url,
        read_only: remote.read_only,
    }))
}

/// What a push did.
#[derive(Debug, PartialEq)]
pub enum Pushed {
    /// Stored (and shared, when there's a remote).
    Published { files: usize, shared: bool },
    /// The same files were already released under this version.
    Unchanged,
}

/// Publish the files `patterns` match under `dir` as `name@version`.
#[allow(clippy::too_many_arguments)]
pub async fn push(
    store: &Store,
    shared: Option<&Shared>,
    name: &str,
    version: &str,
    dir: &Path,
    patterns: &[String],
    overwrite: bool,
    env: &HashMap<String, String>,
) -> Result<Pushed> {
    if patterns.is_empty() {
        bail!("a release push needs `release.files` — the globs of what to publish");
    }
    let files = crate::cache::hash_matching(dir, patterns, &[])?;
    if files.is_empty() {
        bail!(
            "release.files ({}) matched nothing under {} — has the build that makes them run?",
            patterns.join(", "),
            dir.display()
        );
    }
    let key = key(name, version);
    let target = format!("{TARGET_PREFIX}{name}");
    let fingerprint = crate::cache::fingerprint(&files);

    // What's already there, here or on the remote.
    let mut existing: Option<Entry> = store.get(&key)?;
    if existing.is_none()
        && let Some(shared) = shared
    {
        existing = shared
            .client
            .lookup_for(&shared.project, &key, Some(&target))
            .await?;
    }
    if let Some(existing) = &existing {
        if crate::cache::fingerprint(&existing.outputs) == fingerprint {
            return Ok(Pushed::Unchanged);
        }
        if !overwrite {
            bail!(
                "{name}@{version} is already released with different files. A release is \
                 immutable: publish a new version, or set `release.overwrite: true` \
                 (`--overwrite`) to replace it"
            );
        }
    }

    if let Some(shared) = shared
        && shared.read_only
    {
        bail!(
            "the remote cache at {} is read-only for this checkout, so a release can't be \
             published to it",
            shared.url
        );
    }

    let mut recorded: BTreeMap<String, String> = BTreeMap::new();
    recorded.insert(RELEASE_VAR.to_string(), version.to_string());
    recorded.insert(NAME_VAR.to_string(), name.to_string());
    for var in ["CIABATTA_COMMIT", "CIABATTA_BRANCH", "CIABATTA_TAG"] {
        if let Some(value) = env.get(var).filter(|v| !v.is_empty()) {
            recorded.insert(var.to_string(), value.clone());
        }
    }

    let entry = store.put(
        &key,
        dir,
        Build {
            target,
            workspace: name.to_string(),
            inputs: Vec::new(),
            outputs: files.clone(),
            env: recorded,
            upstream: BTreeMap::new(),
            duration_ms: 0,
        },
    )?;

    if let Some(shared) = shared {
        shared
            .client
            .upload(&shared.project, &key, &entry, dir)
            .await
            .with_context(|| format!("couldn't publish {name}@{version} to {}", shared.url))?;
    }
    Ok(Pushed::Published {
        files: files.len(),
        shared: shared.is_some(),
    })
}

/// Fetch `name@version` into `into`, from the local store when it has the
/// release and from the remote cache otherwise. Returns the files written.
pub async fn pull(
    store: &Store,
    shared: Option<&Shared>,
    name: &str,
    version: &str,
    into: &Path,
) -> Result<(Vec<FileHash>, &'static str)> {
    let key = key(name, version);
    std::fs::create_dir_all(into)
        .with_context(|| format!("Failed to create {}", into.display()))?;

    if store.get(&key)?.is_some_and(|e| is_release(&e)) && store.has_artifacts(&key)? {
        return Ok((store.restore(&key, into)?, "the local cache"));
    }

    let Some(shared) = shared else {
        bail!(
            "{name}@{version} isn't in the local cache, and no remote cache is configured to \
             fetch it from (`cache.remote` in ciabatta.yaml)"
        );
    };
    let target = format!("{TARGET_PREFIX}{name}");
    let Some(entry) = shared
        .client
        .lookup_for(&shared.project, &key, Some(&target))
        .await?
    else {
        bail!(
            "there is no release {name}@{version} on {} — `ciabatta releases list` shows what \
             there is",
            shared.url
        );
    };
    shared
        .client
        .download(&shared.project, &key, &entry, into)
        .await?;
    Ok((entry.outputs, "the remote cache"))
}

/// Every release this project has, from the local store and the remote cache
/// together, newest first.
pub async fn list(store: &Store, shared: Option<&Shared>) -> Result<Vec<(ReleaseInfo, String)>> {
    let mut seen: BTreeMap<(String, String), (ReleaseInfo, String)> = BTreeMap::new();
    for entry in store.list()? {
        if let Some(info) = ReleaseInfo::of(&entry) {
            seen.insert(
                (info.name.clone(), info.version.clone()),
                (info, "local".to_string()),
            );
        }
    }
    if let Some(shared) = shared {
        for info in shared.client.releases(&shared.project).await? {
            let id = (info.name.clone(), info.version.clone());
            match seen.get_mut(&id) {
                Some(found) => found.1 = "local + remote".to_string(),
                None => {
                    seen.insert(id, (info, "remote".to_string()));
                }
            }
        }
    }
    let mut all: Vec<(ReleaseInfo, String)> = seen.into_values().collect();
    all.sort_by(|a, b| b.0.created_at.cmp(&a.0.created_at));
    Ok(all)
}

/// Run a `kind: release-push` / `kind: release-pull` step.
#[allow(clippy::too_many_arguments)]
pub async fn run_step(
    step: &crate::run::RunStep,
    direction: Direction,
    config: &CiabattaConfig,
    root: &Path,
    cwd: &Path,
    env: &HashMap<String, String>,
    dry_run: bool,
    sink: &mut LogSink<'_>,
) -> Result<()> {
    let spec = step.release.clone().unwrap_or_default();
    let name = spec
        .name
        .clone()
        .unwrap_or_else(|| default_name(step.workspace.as_deref(), config, root));
    let version = resolve_version(spec.version.as_deref(), env, root, direction)?;

    if dry_run {
        match direction {
            Direction::Push => sink.push(format!(
                "[dry-run] would release {} as {name}@{version}",
                spec.files.join(", ")
            )),
            Direction::Pull => sink.push(format!(
                "[dry-run] would fetch {name}@{version} into {}",
                spec.into.as_deref().unwrap_or(".")
            )),
        }
        return Ok(());
    }

    let store = Store::for_project(root)?;
    let shared = connect(config, root).await?;

    match direction {
        Direction::Push => {
            sink.push(format!("releasing {name}@{version}"));
            match push(
                &store,
                shared.as_ref(),
                &name,
                &version,
                cwd,
                &spec.files,
                spec.overwrite,
                env,
            )
            .await?
            {
                Pushed::Unchanged => sink.push(format!(
                    "{name}@{version} is already released with exactly these files — nothing to do"
                )),
                Pushed::Published {
                    files,
                    shared: true,
                } => sink.push(format!(
                    "released {name}@{version}: {files} file(s), shared through {}",
                    shared.as_ref().map(|s| s.url.as_str()).unwrap_or_default()
                )),
                Pushed::Published {
                    files,
                    shared: false,
                } => sink.push(format!(
                    "released {name}@{version}: {files} file(s), in this machine's cache only — \
                     configure `cache.remote` to share releases"
                )),
            }
        }
        Direction::Pull => {
            let into: PathBuf = cwd.join(spec.into.as_deref().unwrap_or("."));
            let (files, from) = pull(&store, shared.as_ref(), &name, &version, &into).await?;
            sink.push(format!(
                "fetched {name}@{version} from {from}: {} file(s) into {}",
                files.len(),
                into.display()
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ciab_release_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write(dir: &Path, rel: &str, body: &str) {
        let path = dir.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }

    #[test]
    fn the_key_depends_on_name_and_version_only() {
        assert_eq!(key("api", "v1"), key("api", "v1"));
        assert_ne!(key("api", "v1"), key("api", "v2"));
        assert_ne!(key("api", "v1"), key("web", "v1"));
        assert!(key("api", "v1").chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn the_version_comes_from_the_step_then_the_variable() {
        let root = scratch("version");
        let mut env = HashMap::new();
        env.insert("CIABATTA_TAG".to_string(), "v9".to_string());
        env.insert(RELEASE_VAR.to_string(), "v2".to_string());

        let declared =
            resolve_version(Some("{CIABATTA_TAG}"), &env, &root, Direction::Push).unwrap();
        assert_eq!(declared, "v9");
        let from_var = resolve_version(None, &env, &root, Direction::Pull).unwrap();
        assert_eq!(from_var, "v2");

        // A pull never guesses.
        let none = resolve_version(None, &HashMap::new(), &root, Direction::Pull);
        assert!(none.unwrap_err().to_string().contains("--release"));
        // A reference to an unset variable is an error, not an empty version.
        assert!(resolve_version(Some("{NOPE}"), &HashMap::new(), &root, Direction::Push).is_err());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn a_release_round_trips_through_the_local_store() {
        let root = scratch("roundtrip");
        let store = Store::at(root.join("store")).unwrap();
        let work = root.join("work");
        write(&work, "dist/app", "binary v1");
        write(&work, "dist/assets/logo.svg", "<svg/>");
        let files = vec!["dist".to_string()];
        let env = HashMap::new();

        let pushed = push(&store, None, "api", "v1", &work, &files, false, &env)
            .await
            .unwrap();
        assert_eq!(
            pushed,
            Pushed::Published {
                files: 2,
                shared: false
            }
        );

        // Re-running the same push is fine; changing the files is not…
        assert_eq!(
            push(&store, None, "api", "v1", &work, &files, false, &env)
                .await
                .unwrap(),
            Pushed::Unchanged
        );
        write(&work, "dist/app", "binary v1, rebuilt differently");
        let refused = push(&store, None, "api", "v1", &work, &files, false, &env).await;
        assert!(refused.unwrap_err().to_string().contains("immutable"));

        // …and the original files are what come back.
        let into = root.join("deploy");
        let (pulled, from) = pull(&store, None, "api", "v1", &into).await.unwrap();
        assert_eq!(from, "the local cache");
        assert_eq!(pulled.len(), 2);
        assert_eq!(
            std::fs::read_to_string(into.join("dist/app")).unwrap(),
            "binary v1"
        );

        // Overwriting is allowed when asked for.
        push(&store, None, "api", "v1", &work, &files, true, &env)
            .await
            .unwrap();
        pull(&store, None, "api", "v1", &into).await.unwrap();
        assert_eq!(
            std::fs::read_to_string(into.join("dist/app")).unwrap(),
            "binary v1, rebuilt differently"
        );

        let listed = list(&store, None).await.unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].0.name, "api");
        assert_eq!(listed[0].0.version, "v1");

        assert!(pull(&store, None, "api", "v2", &into).await.is_err());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn retention_never_evicts_a_release() {
        let root = scratch("retention");
        let store = Store::at(root.join("store")).unwrap();
        let work = root.join("work");
        write(&work, "dist/app", "binary");
        push(
            &store,
            None,
            "api",
            "v1",
            &work,
            &["dist".to_string()],
            false,
            &HashMap::new(),
        )
        .await
        .unwrap();

        let policy = crate::cache::store::Retention {
            max_entries: Some(0),
            ..Default::default()
        };
        store.prune(&policy).unwrap();
        assert_eq!(list(&store, None).await.unwrap().len(), 1);
        let _ = std::fs::remove_dir_all(&root);
    }
}
