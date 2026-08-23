//! Build caching: skipping work whose inputs and outputs haven't moved.
//!
//! The bargain is explicit rather than clever. A workspace says what its build
//! *reads* (`inputs`) and what it *writes* (`outputs`); ciabatta hashes the
//! former into a key, and stores the latter under it. Next time, if the inputs
//! hash to the same key and the stored outputs are still byte-for-byte what
//! they were, there is nothing to do.
//!
//! Three decisions are worth stating outright, because they're the ones that
//! make a cache trustworthy rather than merely fast:
//!
//! * **Off by default.** A cache that turns itself on is a cache that will one
//!   day serve somebody a stale artifact they never asked to be cached. Opting
//!   in is one line, and it's the line where you also say what your inputs are
//!   — which is the part that actually has to be right.
//!
//! * **Outputs are verified, not assumed.** A key match says the inputs didn't
//!   change; it says nothing about whether somebody deleted `dist/` or edited a
//!   generated file by hand. So the outputs are hashed too, and a mismatch is a
//!   rebuild. This is the difference between "we think this is current" and
//!   "this is current".
//!
//! * **An undeclared input is a wrong answer, not a slow one.** If a build
//!   reads a file that isn't in `inputs`, changing that file won't change the
//!   key and the cache will confidently hand back the wrong artifact. That's
//!   why [`ciabatta dry-run`] exists, and why `cache init` scaffolds `inputs`
//!   from what's actually in the directory instead of leaving it empty.
//!
//! [`ciabatta dry-run`]: crate::cache::plan

pub mod cli;
pub mod diff;
pub mod graph;
pub mod store;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// The `cache:` section of a workspace's config.
///
/// ```yaml
/// cache:
///   enabled: true
///   inputs:  ["src/**/*.rs", "Cargo.toml"]
///   outputs: ["target/release/app"]
/// ```
#[derive(Debug, Deserialize, Serialize, Clone, Default, PartialEq)]
pub struct CacheConfig {
    /// Whether caching is on. Absent or false means every build runs, which is
    /// the behaviour of every ciabatta before 0.2.0 and the one a project keeps
    /// until it says otherwise.
    #[serde(default)]
    #[serde(skip_serializing_if = "crate::format::is_false")]
    pub enabled: bool,

    /// Glob patterns for the files a build reads, relative to the workspace
    /// directory. Changing any of them changes the key, and so forces a
    /// rebuild.
    ///
    /// A build that reads something not listed here will be handed a stale
    /// result. `ciabatta cache init` scaffolds these from the directory's real
    /// contents for exactly that reason.
    #[serde(default)]
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub inputs: Vec<String>,

    /// Glob patterns for the files a build writes. These are what gets stored
    /// and restored on a hit, and what's verified against the manifest before a
    /// hit is granted.
    #[serde(default)]
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub outputs: Vec<String>,

    /// Environment variables that are part of the key. A build whose result
    /// depends on `PROFILE` must say so, or switching profiles will silently
    /// reuse the other one's artifacts.
    #[serde(default)]
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub env: Vec<String>,

    /// Glob patterns never treated as inputs even when `inputs` would match
    /// them. Build output living under a source tree is the usual case.
    #[serde(default)]
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub exclude: Vec<String>,

    /// The shared cache this workspace reads from and writes to, when it has
    /// one. Local-only caching needs none of this.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote: Option<RemoteRef>,
}

/// How a workspace reaches the shared remote cache, and who it is there.
#[derive(Debug, Deserialize, Serialize, Clone, PartialEq)]
pub struct RemoteRef {
    /// Base URL of the remote cache server (`ciabatta remote-cache start`).
    pub url: String,

    /// The name this project is known by on the server. Defaults to the
    /// workspace's own name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,

    /// The identifier the server assigned when this project first registered.
    ///
    /// Written back into the config by `ciabatta cache init` (or on first
    /// contact) and committed alongside it, so every checkout of the repo — and
    /// every CI runner — resolves to the same project rather than registering a
    /// new one under the same name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,

    /// Read from the remote cache but never write to it. What CI wants for
    /// pull-request builds: they benefit from everyone else's artifacts without
    /// being able to poison the cache for the main branch.
    #[serde(default)]
    #[serde(skip_serializing_if = "crate::format::is_false")]
    pub read_only: bool,

    /// Verify the server's TLS certificate.
    ///
    /// Defaults to on, and should stay on for anything reachable beyond your
    /// own machine. Turn it off for a cache behind a self-signed certificate,
    /// or an internal CA you haven't installed — but know what you're buying:
    /// with verification off, HTTPS is an encrypted channel to whoever
    /// answered, so the build artifacts it hands back are only as trustworthy
    /// as the network between you.
    // Not skipped when false: this defaults to `true`, so omitting `false`
    // would silently turn verification back on.
    #[serde(default = "default_true")]
    pub tls_verify: bool,

    /// Turn the remote off without deleting its settings.
    // Not skipped when false: this defaults to `true`, so omitting `false`
    // would silently turn it back on.
    #[serde(default = "default_true")]
    pub enabled: bool,
}

impl Default for RemoteRef {
    /// Hand-written rather than derived, so `tls_verify` and `enabled` default
    /// to `true` here exactly as they do when a config is parsed. A derived
    /// `Default` would give `false` for both and quietly disagree with the file.
    fn default() -> Self {
        RemoteRef {
            url: String::new(),
            name: None,
            project: None,
            read_only: false,
            tls_verify: true,
            enabled: true,
        }
    }
}

fn default_true() -> bool {
    true
}

impl CacheConfig {
    /// Why this config isn't caching, in one line, or `None` when it is.
    ///
    /// Enabled but with no `inputs` counts as off: with nothing to hash, every
    /// build would key the same and the first result would be served forever.
    /// Treating that as "off" is safer than treating it as "always hit".
    pub fn why_disabled(&self) -> Option<&'static str> {
        if !self.enabled {
            Some("caching is off (set `cache.enabled: true` to turn it on)")
        } else if self.inputs.is_empty() {
            Some("no `cache.inputs` are declared, so there's nothing to key on")
        } else {
            None
        }
    }

    /// Whether the shared cache should be consulted for this workspace.
    pub fn remote(&self) -> Option<&RemoteRef> {
        self.remote
            .as_ref()
            .filter(|r| r.enabled && !r.url.trim().is_empty())
    }

    /// Hash the files this build reads.
    ///
    /// `exclude` applies here and only here. It exists so a build output
    /// directory sitting under a source tree doesn't count as an input — and
    /// since `cache init` proposes exactly that (`outputs: dist/**/*`,
    /// `exclude: dist`), applying it to outputs as well would erase them and
    /// quietly turn caching off. Hence two named methods rather than one
    /// function with a flag.
    pub fn hash_inputs(&self, dir: &Path) -> Result<Vec<FileHash>> {
        hash_matching(dir, &self.inputs, &self.exclude)
    }

    /// Hash the files this build writes. Never filtered by `exclude`.
    pub fn hash_outputs(&self, dir: &Path) -> Result<Vec<FileHash>> {
        hash_matching(dir, &self.outputs, &[])
    }
}

// ─── Hashing ────────────────────────────────────────────────────────────────

/// One file's contribution to a key or a manifest: where it is and what's in it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileHash {
    /// Path relative to the workspace directory, with `/` separators so a key
    /// computed on Windows matches one computed on Linux.
    pub path: String,
    /// Hex SHA-256 of the file's contents.
    pub sha256: String,
    /// Size in bytes, for reporting.
    pub size: u64,
}

/// The SHA-256 of a file's contents, hex encoded.
pub fn hash_file(path: &Path) -> Result<String> {
    let mut file =
        std::fs::File::open(path).with_context(|| format!("Failed to read {}", path.display()))?;
    let mut hasher = Sha256::new();
    std::io::copy(&mut file, &mut hasher)
        .with_context(|| format!("Failed to read {}", path.display()))?;
    Ok(hex(&hasher.finalize()))
}

/// The SHA-256 of a byte string, hex encoded.
pub fn hash_bytes(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex(&hasher.finalize())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Expand `patterns` against `dir` into the matching files, hashed and sorted
/// by path.
///
/// Sorted because the key must not depend on the order the filesystem happened
/// to hand directory entries back in — the same tree on two machines has to
/// produce the same key or the cache is worthless.
///
/// Directories and anything matching `exclude` are skipped. A pattern that
/// matches nothing is not an error: `outputs` legitimately match nothing before
/// the first build, and an `inputs` pattern for an optional file is a normal
/// thing to write.
pub fn hash_matching(dir: &Path, patterns: &[String], exclude: &[String]) -> Result<Vec<FileHash>> {
    let mut seen: BTreeMap<String, FileHash> = BTreeMap::new();

    for pattern in patterns {
        let joined = dir.join(pattern);
        let Some(pattern_str) = joined.to_str() else {
            continue;
        };
        let entries = glob::glob(pattern_str)
            .with_context(|| format!("Invalid cache pattern '{pattern}'"))?;

        for entry in entries.flatten() {
            if !entry.is_file() {
                continue;
            }
            let Ok(rel) = entry.strip_prefix(dir) else {
                continue;
            };
            let rel = rel.to_string_lossy().replace('\\', "/");
            if is_excluded(&rel, dir, exclude) {
                continue;
            }
            if seen.contains_key(&rel) {
                continue;
            }
            let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
            seen.insert(
                rel.clone(),
                FileHash {
                    path: rel,
                    sha256: hash_file(&entry)?,
                    size,
                },
            );
        }
    }

    Ok(seen.into_values().collect())
}

/// Whether `rel` (a `/`-separated path relative to `dir`) matches any exclude
/// pattern.
fn is_excluded(rel: &str, dir: &Path, exclude: &[String]) -> bool {
    exclude.iter().any(|pattern| {
        // Match the pattern both as a glob over the relative path and as a bare
        // prefix, so `exclude: [target]` does what everyone expects without
        // anyone having to write `target/**/*`.
        let as_prefix =
            rel == pattern || rel.starts_with(&format!("{}/", pattern.trim_end_matches('/')));
        if as_prefix {
            return true;
        }
        glob::Pattern::new(pattern).is_ok_and(|p| p.matches(rel))
            || dir
                .join(pattern)
                .to_str()
                .and_then(|full| glob::Pattern::new(full).ok())
                .is_some_and(|p| p.matches_path(&dir.join(rel)))
    })
}

/// Everything that goes into a cache key, in a stable, inspectable form.
///
/// Serialized to JSON and hashed. Keeping it a real struct rather than an ad-hoc
/// string concatenation means the key is explainable: `ciabatta dry-run -v`
/// can print exactly what it hashed, which is the only way anyone ever debugs
/// "why did this miss?".
///
/// A step has exactly **three** dependencies, and all three are here:
///
/// 1. its **input files**,
/// 2. the **environment variables** it declared,
/// 3. the **outputs of the steps it needs**.
///
/// The third is what makes a graph cacheable rather than just a directory. If
/// `api:build` consumes `proto:generate`'s stubs, then a change to the stubs has
/// to invalidate the api's build — even though not one file under `packages/api`
/// moved. Folding each upstream step's output hash in here is what makes that
/// happen, and it propagates: a change at the root of the graph changes every
/// key downstream of it, exactly once.
#[derive(Debug, Clone, Serialize)]
pub struct KeyInputs {
    /// Bumped when the key derivation changes, so an old cache is missed rather
    /// than misread.
    pub version: u32,
    /// What's being cached — the recipe or workflow name.
    pub target: String,
    /// The workspace it belongs to.
    pub workspace: String,
    /// The command(s) the build runs. A changed build command must change the
    /// key even when every source file is identical.
    pub commands: Vec<String>,
    /// The hashed input files.
    pub inputs: Vec<FileHash>,
    /// The environment variables the config declared, and their values.
    pub env: BTreeMap<String, String>,
    /// Upstream step name → the hash of that step's output set.
    #[serde(default)]
    pub upstream: BTreeMap<String, String>,
}

/// Reduce a set of output hashes to one value, so a downstream step can depend
/// on "what this step produced" without carrying the whole file list.
pub fn fingerprint(outputs: &[FileHash]) -> String {
    let joined: String = outputs
        .iter()
        .map(|o| format!("{}:{}", o.path, o.sha256))
        .collect::<Vec<_>>()
        .join("\n");
    hash_bytes(joined.as_bytes())
}

/// The key-derivation version. Bump it when anything about how a key is
/// computed changes.
pub const KEY_VERSION: u32 = 1;

impl KeyInputs {
    /// The cache key: hex SHA-256 of the canonical JSON encoding.
    pub fn key(&self) -> Result<String> {
        let canonical =
            serde_json::to_vec(self).context("Failed to encode the cache key inputs")?;
        Ok(hash_bytes(&canonical))
    }
}

// ─── Deciding what to do ────────────────────────────────────────────────────

/// What ciabatta would do with one cacheable target.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "outcome")]
pub enum Decision {
    /// Everything matches: the stored outputs are already on disk and correct,
    /// so there's nothing to do at all.
    Fresh {
        key: String,
        /// How many output files were verified.
        outputs: usize,
    },
    /// The key matches a stored entry, and its outputs would be restored.
    Hit {
        key: String,
        /// Where the entry came from.
        source: Source,
        outputs: usize,
    },
    /// The build has to run.
    Rebuild { key: String, reason: Reason },
    /// Caching isn't in play for this target.
    Uncached { reason: String },
}

impl Decision {
    /// Whether this target's build can be skipped.
    pub fn is_reuse(&self) -> bool {
        matches!(self, Decision::Fresh { .. } | Decision::Hit { .. })
    }

    /// The cache key, when one was computed.
    pub fn key(&self) -> Option<&str> {
        match self {
            Decision::Fresh { key, .. }
            | Decision::Hit { key, .. }
            | Decision::Rebuild { key, .. } => Some(key),
            Decision::Uncached { .. } => None,
        }
    }

    /// One line for the terminal.
    pub fn describe(&self) -> String {
        match self {
            Decision::Fresh { outputs, .. } => {
                format!("up to date ({outputs} output file(s) already correct)")
            }
            Decision::Hit {
                source, outputs, ..
            } => format!(
                "cache hit from {} ({outputs} file(s) to restore)",
                source.label()
            ),
            Decision::Rebuild { reason, .. } => format!("rebuild — {}", reason.describe()),
            Decision::Uncached { reason } => format!("not cached — {reason}"),
        }
    }
}

/// Where a cache entry was found.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    Local,
    Remote,
}

impl Source {
    pub fn label(self) -> &'static str {
        match self {
            Source::Local => "the local cache",
            Source::Remote => "the remote cache",
        }
    }
}

/// Why a target has to be rebuilt. Every variant names something the user can
/// go and look at — "cache miss" on its own has never helped anybody.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum Reason {
    /// This key has never been built.
    NeverBuilt,
    /// Input files changed since the entry was stored.
    InputsChanged {
        /// Paths that were added, removed, or edited — capped for display.
        changed: Vec<String>,
        /// How many changed in total, when more than `changed` lists.
        total: usize,
    },
    /// The entry exists, but the outputs it promised aren't on disk and can't
    /// be restored.
    OutputsMissing { missing: Vec<String> },
    /// The outputs on disk don't match what was stored — somebody edited a
    /// generated file, or a partial build left something behind.
    OutputsModified { modified: Vec<String> },
    /// The build declares no outputs, so there is nothing to restore and
    /// nothing a hit could save.
    NoOutputs,
}

impl Reason {
    pub fn describe(&self) -> String {
        match self {
            Reason::NeverBuilt => "this input set has never been built".to_string(),
            Reason::InputsChanged { changed, total } => {
                let listed = changed.join(", ");
                if *total > changed.len() {
                    format!("{total} input file(s) changed, including {listed}",)
                } else {
                    format!("input file(s) changed: {listed}")
                }
            }
            Reason::OutputsMissing { missing } => {
                format!("expected output(s) are missing: {}", missing.join(", "))
            }
            Reason::OutputsModified { modified } => format!(
                "output(s) have been modified since they were built: {}",
                modified.join(", ")
            ),
            Reason::NoOutputs => {
                "no `cache.outputs` are declared, so there'd be nothing to restore".to_string()
            }
        }
    }
}

/// How many changed paths to name before summarizing.
const MAX_LISTED: usize = 5;

/// Compare a freshly computed input set against a stored manifest and say what
/// changed, in path order.
pub fn changed_inputs(stored: &[FileHash], current: &[FileHash]) -> Vec<String> {
    let stored_by_path: BTreeMap<&str, &FileHash> =
        stored.iter().map(|f| (f.path.as_str(), f)).collect();
    let current_by_path: BTreeMap<&str, &FileHash> =
        current.iter().map(|f| (f.path.as_str(), f)).collect();

    let mut changed: Vec<String> = Vec::new();
    for (path, file) in &current_by_path {
        match stored_by_path.get(path) {
            None => changed.push(format!("{path} (new)")),
            Some(before) if before.sha256 != file.sha256 => changed.push((*path).to_string()),
            Some(_) => {}
        }
    }
    for path in stored_by_path.keys() {
        if !current_by_path.contains_key(path) {
            changed.push(format!("{path} (gone)"));
        }
    }
    changed.sort();
    changed
}

/// Trim a change list down to something worth printing.
pub fn summarize(changed: Vec<String>) -> Reason {
    let total = changed.len();
    let mut listed = changed;
    listed.truncate(MAX_LISTED);
    Reason::InputsChanged {
        changed: listed,
        total,
    }
}

// ─── Planning a target ──────────────────────────────────────────────────────

/// One cacheable unit: a workspace's build, identified by the target that runs
/// it.
#[derive(Debug, Clone)]
pub struct Target {
    /// The recipe or workflow name.
    pub name: String,
    /// The workspace the build belongs to.
    pub workspace: String,
    /// The directory `inputs`/`outputs` are relative to.
    pub dir: PathBuf,
    /// The commands the build runs, folded into the key.
    pub commands: Vec<String>,
    /// The cache settings in force for it.
    pub config: CacheConfig,
    /// The steps this one needs, and the fingerprint of what each produced.
    ///
    /// Filled in by the runner as the graph executes — a step can't be keyed
    /// until the steps it depends on have produced something to key against.
    pub upstream: BTreeMap<String, String>,
}

/// Work out what would happen to `target`, without running or restoring
/// anything.
///
/// This is the whole of `ciabatta dry-run`, and it's also what the runner calls
/// before a build — so the preview and the real thing cannot disagree.
pub fn plan(
    target: &Target,
    env: &BTreeMap<String, String>,
    store: &store::Store,
) -> Result<Decision> {
    if let Some(reason) = target.config.why_disabled() {
        return Ok(Decision::Uncached {
            reason: reason.to_string(),
        });
    }

    let inputs = target.config.hash_inputs(&target.dir)?;
    let key_inputs = KeyInputs {
        version: KEY_VERSION,
        target: target.name.clone(),
        workspace: target.workspace.clone(),
        commands: target.commands.clone(),
        inputs,
        env: target
            .config
            .env
            .iter()
            .map(|name| (name.clone(), env.get(name).cloned().unwrap_or_default()))
            .collect(),
        upstream: target.upstream.clone(),
    };
    let key = key_inputs.key()?;

    if target.config.outputs.is_empty() {
        return Ok(Decision::Rebuild {
            key,
            reason: Reason::NoOutputs,
        });
    }

    // Remember what was hashed, so a rebuild can record it and the next miss
    // can say which of the three dependencies moved.
    let Some(entry) = store.get(&key)? else {
        // Nothing stored under this key. Say whether that's because the inputs
        // moved (the useful answer) or because it's simply never been built.
        let reason = match store.latest_for(&target.workspace, &target.name)? {
            Some(previous) => summarize(changed_inputs(&previous.inputs, &key_inputs.inputs)),
            None => Reason::NeverBuilt,
        };
        return Ok(Decision::Rebuild { key, reason });
    };

    // The key matched. That only says the inputs are unchanged — check the
    // outputs really are what was stored before calling it a hit.
    let mut missing: Vec<String> = Vec::new();
    let mut modified: Vec<String> = Vec::new();
    for output in &entry.outputs {
        let path = target.dir.join(&output.path);
        if !path.is_file() {
            missing.push(output.path.clone());
            continue;
        }
        if hash_file(&path)? != output.sha256 {
            modified.push(output.path.clone());
        }
    }

    if missing.is_empty() && modified.is_empty() {
        return Ok(Decision::Fresh {
            key,
            outputs: entry.outputs.len(),
        });
    }

    // Outputs are wrong on disk but the store still holds a good copy, so this
    // is a restore rather than a rebuild.
    if store.has_artifacts(&key)? {
        return Ok(Decision::Hit {
            key,
            source: Source::Local,
            outputs: entry.outputs.len(),
        });
    }

    Ok(Decision::Rebuild {
        key,
        reason: if !missing.is_empty() {
            missing.truncate(MAX_LISTED);
            Reason::OutputsMissing { missing }
        } else {
            modified.truncate(MAX_LISTED);
            Reason::OutputsModified { modified }
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ciab_cache_{name}_{}", std::process::id()));
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
    fn caching_is_off_until_it_is_asked_for() {
        let off = CacheConfig::default();
        assert!(off.why_disabled().unwrap().contains("caching is off"));

        // Enabled but with nothing to hash is not a cache — every build would
        // key identically and the first result would be served forever.
        let empty = CacheConfig {
            enabled: true,
            ..Default::default()
        };
        assert!(empty.why_disabled().unwrap().contains("cache.inputs"));

        let real = CacheConfig {
            enabled: true,
            inputs: vec!["src/**/*".into()],
            ..Default::default()
        };
        assert!(real.why_disabled().is_none());
    }

    #[test]
    fn hashing_is_stable_sorted_and_content_addressed() {
        let dir = scratch("hash");
        write(&dir, "src/a.rs", "fn a() {}");
        write(&dir, "src/b.rs", "fn b() {}");
        write(&dir, "target/junk.o", "binary");

        let patterns = vec!["src/**/*.rs".to_string(), "target/**/*".to_string()];
        let all = hash_matching(&dir, &patterns, &[]).unwrap();
        let paths: Vec<&str> = all.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, vec!["src/a.rs", "src/b.rs", "target/junk.o"]);

        // Excluding by bare directory name is what everyone expects to work.
        let excluded = hash_matching(&dir, &patterns, &["target".to_string()]).unwrap();
        let paths: Vec<&str> = excluded.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, vec!["src/a.rs", "src/b.rs"]);

        // Same contents → same hash; changed contents → different hash.
        let again = hash_matching(&dir, &patterns, &["target".to_string()]).unwrap();
        assert_eq!(excluded, again);
        write(&dir, "src/a.rs", "fn a() { changed }");
        let after = hash_matching(&dir, &patterns, &["target".to_string()]).unwrap();
        assert_ne!(excluded[0].sha256, after[0].sha256);
        assert_eq!(excluded[1].sha256, after[1].sha256, "b.rs didn't move");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `cache init` proposes `outputs: dist/**/*` alongside `exclude: dist`,
    /// so applying `exclude` to outputs would erase them — and a build with no
    /// outputs is never cached at all. The two must stay separate.
    #[test]
    fn exclude_filters_inputs_and_never_outputs() {
        let dir = scratch("exclude_scope");
        write(&dir, "src/main.rs", "fn main() {}");
        write(&dir, "dist/app", "compiled");

        let config = CacheConfig {
            enabled: true,
            inputs: vec!["src/**/*".into(), "dist/**/*".into()],
            outputs: vec!["dist/**/*".into()],
            exclude: vec!["dist".into()],
            ..Default::default()
        };

        let inputs = config.hash_inputs(&dir).unwrap();
        assert_eq!(
            inputs.iter().map(|f| f.path.as_str()).collect::<Vec<_>>(),
            vec!["src/main.rs"],
            "build output under an inputs glob must not count as an input"
        );

        let outputs = config.hash_outputs(&dir).unwrap();
        assert_eq!(
            outputs.iter().map(|f| f.path.as_str()).collect::<Vec<_>>(),
            vec!["dist/app"],
            "excluding an output directory must not erase the outputs themselves"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_key_covers_inputs_commands_and_declared_env() {
        let base = KeyInputs {
            version: KEY_VERSION,
            target: "build".into(),
            workspace: "api".into(),
            commands: vec!["cargo build".into()],
            inputs: vec![FileHash {
                path: "src/a.rs".into(),
                sha256: "abc".into(),
                size: 9,
            }],
            env: BTreeMap::new(),
            upstream: BTreeMap::new(),
        };
        let key = base.key().unwrap();
        assert_eq!(key.len(), 64, "a hex SHA-256");

        // Recomputing the same inputs gives the same key.
        assert_eq!(base.key().unwrap(), key);

        // A changed build command must change the key even with identical files.
        let mut other = base.clone();
        other.commands = vec!["cargo build --release".into()];
        assert_ne!(other.key().unwrap(), key);

        // So must a declared environment variable's value.
        let mut with_env = base.clone();
        with_env.env.insert("PROFILE".into(), "release".into());
        assert_ne!(with_env.key().unwrap(), key);

        // And a changed input file.
        let mut edited = base.clone();
        edited.inputs[0].sha256 = "def".into();
        assert_ne!(edited.key().unwrap(), key);
    }

    /// A full pass over the decision table: never built → rebuild, built and
    /// intact → fresh, outputs deleted → restore, inputs edited → rebuild with
    /// the changed file named.
    #[test]
    fn the_plan_walks_the_whole_decision_table() {
        let root = scratch("plan");
        let work = root.join("api");
        std::fs::create_dir_all(&work).unwrap();
        write(&work, "src/main.rs", "fn main() {}");
        let store = store::Store::at(root.join("cache")).unwrap();

        let target = Target {
            name: "build".into(),
            workspace: "api".into(),
            dir: work.clone(),
            commands: vec!["make".into()],
            config: CacheConfig {
                enabled: true,
                inputs: vec!["src/**/*.rs".into()],
                outputs: vec!["dist/**/*".into()],
                ..Default::default()
            },
            upstream: BTreeMap::new(),
        };
        let env = BTreeMap::new();

        // 1. Nothing has ever been built.
        let decision = plan(&target, &env, &store).unwrap();
        let key = decision.key().unwrap().to_string();
        assert_eq!(
            decision,
            Decision::Rebuild {
                key: key.clone(),
                reason: Reason::NeverBuilt
            }
        );
        assert!(!decision.is_reuse());

        // Build it, and store what came out.
        write(&work, "dist/app", "compiled");
        let inputs = hash_matching(&work, &target.config.inputs, &[]).unwrap();
        let outputs = hash_matching(&work, &target.config.outputs, &[]).unwrap();
        store
            .put(
                &key,
                &work,
                store::Build {
                    target: "build".into(),
                    workspace: "api".into(),
                    inputs,
                    outputs,
                    duration_ms: 1234,
                    ..Default::default()
                },
            )
            .unwrap();

        // 2. Everything matches and the outputs are already in place.
        let decision = plan(&target, &env, &store).unwrap();
        assert_eq!(
            decision,
            Decision::Fresh {
                key: key.clone(),
                outputs: 1
            }
        );
        assert!(decision.is_reuse());

        // 3. The output was deleted, but the store still has it — restore.
        std::fs::remove_file(work.join("dist/app")).unwrap();
        let decision = plan(&target, &env, &store).unwrap();
        assert_eq!(
            decision,
            Decision::Hit {
                key: key.clone(),
                source: Source::Local,
                outputs: 1
            }
        );

        // …and restoring really does put it back.
        store.restore(&key, &work).unwrap();
        assert_eq!(
            std::fs::read_to_string(work.join("dist/app")).unwrap(),
            "compiled"
        );
        assert_eq!(
            plan(&target, &env, &store).unwrap(),
            Decision::Fresh {
                key: key.clone(),
                outputs: 1
            }
        );

        // 4. An input changed: a different key, and the miss names the file.
        write(&work, "src/main.rs", "fn main() { println!(); }");
        let decision = plan(&target, &env, &store).unwrap();
        assert_ne!(
            decision.key().unwrap(),
            key,
            "the key must follow the inputs"
        );
        match &decision {
            Decision::Rebuild {
                reason: Reason::InputsChanged { changed, total },
                ..
            } => {
                assert_eq!(total, &1);
                assert_eq!(changed, &vec!["src/main.rs".to_string()]);
            }
            other => panic!("expected an inputs-changed rebuild, got {other:?}"),
        }

        let _ = std::fs::remove_dir_all(&root);
    }

    /// A declared output that was hand-edited must not be served as current —
    /// that's the case where a cache quietly hands back the wrong thing.
    #[test]
    fn a_modified_output_is_restored_rather_than_trusted() {
        let root = scratch("modified");
        let work = root.join("api");
        std::fs::create_dir_all(&work).unwrap();
        write(&work, "src/main.rs", "fn main() {}");
        let store = store::Store::at(root.join("cache")).unwrap();

        let target = Target {
            name: "build".into(),
            workspace: "api".into(),
            dir: work.clone(),
            commands: vec!["make".into()],
            config: CacheConfig {
                enabled: true,
                inputs: vec!["src/**/*.rs".into()],
                outputs: vec!["dist/**/*".into()],
                ..Default::default()
            },
            upstream: BTreeMap::new(),
        };
        let env = BTreeMap::new();
        let key = plan(&target, &env, &store)
            .unwrap()
            .key()
            .unwrap()
            .to_string();

        write(&work, "dist/app", "compiled");
        let inputs = hash_matching(&work, &target.config.inputs, &[]).unwrap();
        let outputs = hash_matching(&work, &target.config.outputs, &[]).unwrap();
        store
            .put(
                &key,
                &work,
                store::Build {
                    target: "build".into(),
                    workspace: "api".into(),
                    inputs,
                    outputs,
                    duration_ms: 1,
                    ..Default::default()
                },
            )
            .unwrap();

        // Somebody edits the generated file by hand.
        write(&work, "dist/app", "hand-edited, definitely not compiled");
        let decision = plan(&target, &env, &store).unwrap();
        assert!(
            !matches!(decision, Decision::Fresh { .. }),
            "an edited output must never read as up to date"
        );
        assert_eq!(
            decision,
            Decision::Hit {
                key: key.clone(),
                source: Source::Local,
                outputs: 1
            }
        );
        store.restore(&key, &work).unwrap();
        assert_eq!(
            std::fs::read_to_string(work.join("dist/app")).unwrap(),
            "compiled",
            "the restore must overwrite the tampered file"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    /// A build with no declared outputs has nothing a hit could give back, so
    /// it always runs rather than pretending to be cached.
    #[test]
    fn declaring_no_outputs_means_no_hits() {
        let root = scratch("nooutputs");
        let work = root.join("api");
        std::fs::create_dir_all(&work).unwrap();
        write(&work, "src/main.rs", "fn main() {}");
        let store = store::Store::at(root.join("cache")).unwrap();

        let target = Target {
            name: "build".into(),
            workspace: "api".into(),
            dir: work.clone(),
            commands: vec!["make".into()],
            config: CacheConfig {
                enabled: true,
                inputs: vec!["src/**/*.rs".into()],
                outputs: vec![],
                ..Default::default()
            },
            upstream: BTreeMap::new(),
        };
        match plan(&target, &BTreeMap::new(), &store).unwrap() {
            Decision::Rebuild {
                reason: Reason::NoOutputs,
                ..
            } => {}
            other => panic!("expected a no-outputs rebuild, got {other:?}"),
        }

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn changed_inputs_names_additions_edits_and_removals() {
        let before = vec![
            FileHash {
                path: "a".into(),
                sha256: "1".into(),
                size: 1,
            },
            FileHash {
                path: "b".into(),
                sha256: "2".into(),
                size: 1,
            },
        ];
        let after = vec![
            FileHash {
                path: "a".into(),
                sha256: "9".into(),
                size: 1,
            },
            FileHash {
                path: "c".into(),
                sha256: "3".into(),
                size: 1,
            },
        ];
        assert_eq!(
            changed_inputs(&before, &after),
            vec![
                "a".to_string(),
                "b (gone)".to_string(),
                "c (new)".to_string()
            ]
        );
        assert_eq!(changed_inputs(&before, &before), Vec::<String>::new());
    }
}
