//! The local cache store: where built outputs are kept, and how they come back.
//!
//! Layout under `.ciabatta/cache/`:
//!
//! ```text
//! entries/<key>.json          the manifest: inputs, outputs, when, by whom
//! artifacts/<key>/<path>      the output files themselves, laid out as they
//!                             were relative to the workspace directory
//! ```
//!
//! Two files per entry rather than one archive, because the manifest is read
//! constantly (every plan, every dry run) and the artifacts only on a restore.
//! Keeping them apart means deciding "hit or miss" never touches the payload.
//!
//! The store is also what the retention policy prunes, and what the remote
//! cache server uses for its own backing storage — the same layout on both
//! sides means an artifact uploaded from a laptop is byte-identical to the one
//! a CI runner pulls back down.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use super::FileHash;
use crate::config::CIABATTA_DIR;

/// Directory under `.ciabatta/` holding everything in this module.
const CACHE_DIR: &str = "cache";
/// Where manifests live.
const ENTRIES_DIR: &str = "entries";
/// Where the output files live.
const ARTIFACTS_DIR: &str = "artifacts";

/// One cached build: what went in, what came out, and when.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entry {
    /// The cache key this entry is stored under.
    pub key: String,
    /// The target that produced it.
    pub target: String,
    /// The workspace it belongs to.
    pub workspace: String,
    /// The hashed inputs, kept so a miss can say *what* changed rather than
    /// only that something did.
    #[serde(default)]
    pub inputs: Vec<FileHash>,
    /// The output files, with the hashes a restore is verified against.
    #[serde(default)]
    pub outputs: Vec<FileHash>,
    /// The declared environment variables and their values at build time — the
    /// second of a step's three dependencies.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// Upstream step → the fingerprint of what it produced: the third.
    ///
    /// Recorded so a miss can say "proto:generate produced something different"
    /// rather than leaving somebody hunting for a source change that isn't there.
    #[serde(default)]
    pub upstream: BTreeMap<String, String>,
    /// RFC 3339 timestamp of when the build finished.
    pub created_at: String,
    /// When this entry was last served. Retention prunes on age since last use
    /// rather than since creation, so a rarely-rebuilt artifact that everyone
    /// depends on doesn't get evicted for being old.
    #[serde(default)]
    pub last_used_at: Option<String>,
    /// Total bytes of the stored outputs.
    #[serde(default)]
    pub size: u64,
    /// How long the build took, so the cache can report what it saved.
    #[serde(default)]
    pub duration_ms: u64,
}

impl Entry {
    /// When this entry was last useful, falling back to when it was created.
    pub fn last_touched(&self) -> &str {
        self.last_used_at.as_deref().unwrap_or(&self.created_at)
    }

    /// How old that is, in seconds, or `None` if the timestamp won't parse.
    pub fn age_seconds(&self) -> Option<i64> {
        let when = chrono::DateTime::parse_from_rfc3339(self.last_touched()).ok()?;
        Some((chrono::Utc::now() - when.with_timezone(&chrono::Utc)).num_seconds())
    }
}

/// Everything a finished build contributes to a cache entry.
///
/// A parameter object rather than eight positional arguments, because five of
/// them are strings and maps and a caller that swaps two of them would compile
/// and then quietly cache the wrong thing.
#[derive(Debug, Clone, Default)]
pub struct Build {
    pub target: String,
    pub workspace: String,
    pub inputs: Vec<FileHash>,
    pub outputs: Vec<FileHash>,
    /// The declared environment variables and their values.
    pub env: BTreeMap<String, String>,
    /// Upstream step → fingerprint of what it produced.
    pub upstream: BTreeMap<String, String>,
    pub duration_ms: u64,
}

/// A cache store rooted at a directory.
#[derive(Debug, Clone)]
pub struct Store {
    root: PathBuf,
}

impl Store {
    /// The store for a project, at `<project>/.ciabatta/cache/`.
    pub fn for_project(project_root: &Path) -> Result<Self> {
        Self::at(project_root.join(CIABATTA_DIR).join(CACHE_DIR))
    }

    /// A store rooted anywhere — the remote cache server's own backing storage
    /// uses this directly.
    pub fn at(root: PathBuf) -> Result<Self> {
        std::fs::create_dir_all(root.join(ENTRIES_DIR))
            .with_context(|| format!("Failed to create {}", root.display()))?;
        std::fs::create_dir_all(root.join(ARTIFACTS_DIR))
            .with_context(|| format!("Failed to create {}", root.display()))?;
        Ok(Store { root })
    }

    /// The directory this store lives in.
    pub fn root(&self) -> &Path {
        &self.root
    }

    fn manifest_path(&self, key: &str) -> PathBuf {
        self.root.join(ENTRIES_DIR).join(format!("{key}.json"))
    }

    /// Where an entry's output files are kept.
    pub fn artifact_dir(&self, key: &str) -> PathBuf {
        self.root.join(ARTIFACTS_DIR).join(key)
    }

    /// The entry stored under `key`, if there is one.
    ///
    /// A manifest that won't parse is treated as absent rather than as an
    /// error: a corrupt cache file should cost a rebuild, not a failed build.
    pub fn get(&self, key: &str) -> Result<Option<Entry>> {
        let path = self.manifest_path(key);
        let Ok(raw) = std::fs::read_to_string(&path) else {
            return Ok(None);
        };
        match serde_json::from_str::<Entry>(&raw) {
            Ok(entry) => Ok(Some(entry)),
            Err(e) => {
                tracing::warn!("discarding unreadable cache entry {}: {e}", path.display());
                let _ = std::fs::remove_file(&path);
                Ok(None)
            }
        }
    }

    /// Whether the stored output files for `key` are all present.
    ///
    /// Checked before a restore is promised: a manifest whose artifacts were
    /// deleted (by a `rm -rf`, by retention, by a half-finished sync) must
    /// read as a miss rather than as a hit that then fails.
    pub fn has_artifacts(&self, key: &str) -> Result<bool> {
        let Some(entry) = self.get(key)? else {
            return Ok(false);
        };
        let dir = self.artifact_dir(key);
        Ok(entry.outputs.iter().all(|o| dir.join(&o.path).is_file()))
    }

    /// Every entry in the store, newest first.
    pub fn list(&self) -> Result<Vec<Entry>> {
        let dir = self.root.join(ENTRIES_DIR);
        let Ok(read) = std::fs::read_dir(&dir) else {
            return Ok(Vec::new());
        };

        let mut entries: Vec<Entry> = read
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e == "json"))
            .filter_map(|p| std::fs::read_to_string(p).ok())
            .filter_map(|raw| serde_json::from_str::<Entry>(&raw).ok())
            .collect();

        entries.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(entries)
    }

    /// The most recent entry for a target, whatever its key.
    ///
    /// This is what turns "cache miss" into "these three files changed": the
    /// previous build's input hashes are the only thing to diff against.
    pub fn latest_for(&self, workspace: &str, target: &str) -> Result<Option<Entry>> {
        Ok(self
            .list()?
            .into_iter()
            .find(|e| e.workspace == workspace && e.target == target))
    }

    /// Store a build's outputs under `key`, copying them out of `dir`.
    ///
    /// Writes the artifacts first and the manifest last, so a crash midway
    /// leaves orphaned files (harmless, and pruned by retention) rather than a
    /// manifest promising artifacts that were never written.
    pub fn put(&self, key: &str, dir: &Path, build: Build) -> Result<Entry> {
        let Build {
            target,
            workspace,
            inputs,
            outputs,
            env,
            upstream,
            duration_ms,
        } = build;

        let artifacts = self.artifact_dir(key);
        // A previous partial write must not leave files behind that the new
        // manifest doesn't account for.
        let _ = std::fs::remove_dir_all(&artifacts);

        let mut size = 0u64;
        for output in &outputs {
            let from = dir.join(&output.path);
            let to = artifacts.join(&output.path);
            if let Some(parent) = to.parent() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("Failed to create {}", parent.display()))?;
            }
            std::fs::copy(&from, &to).with_context(|| {
                format!("Failed to cache {} → {}", from.display(), to.display())
            })?;
            size += output.size;
        }

        // Snapshot the text inputs so the *next* miss can show line-level
        // diffs. Best-effort: failing to write a debugging aid must never fail
        // a build that otherwise succeeded.
        if let Err(e) = self.snapshot_inputs(key, dir, &inputs) {
            tracing::warn!("couldn't snapshot inputs for the diff view: {e:#}");
        }

        let entry = Entry {
            key: key.to_string(),
            target,
            workspace,
            inputs,
            outputs,
            env,
            upstream,
            created_at: now(),
            last_used_at: Some(now()),
            size,
            duration_ms,
        };
        self.write_manifest(&entry)?;
        Ok(entry)
    }

    /// Where a key's input snapshot lives.
    pub fn snapshot_dir(&self, key: &str) -> PathBuf {
        self.root.join(crate::cache::diff::SNAPSHOT_DIR).join(key)
    }

    /// Keep a copy of the text inputs, for the diff view.
    ///
    /// Text only, and capped per file: this exists so somebody can see *why*
    /// their build missed, and nobody reads the diff of a 40MB binary. Binaries
    /// and oversized files are simply not snapshotted — they still show up as
    /// modified, with a note in place of lines.
    pub fn snapshot_inputs(&self, key: &str, dir: &Path, inputs: &[FileHash]) -> Result<()> {
        let snapshots = self.snapshot_dir(key);
        let _ = std::fs::remove_dir_all(&snapshots);

        for input in inputs {
            if input.size > crate::cache::diff::MAX_SNAPSHOT_BYTES {
                continue;
            }
            let from = dir.join(&input.path);
            let Ok(bytes) = std::fs::read(&from) else {
                continue;
            };
            // The same "is it text?" test the diff itself uses.
            if bytes.contains(&0) || String::from_utf8(bytes.clone()).is_err() {
                continue;
            }

            let to = snapshots.join(&input.path);
            if let Some(parent) = to.parent() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("Failed to create {}", parent.display()))?;
            }
            std::fs::write(&to, &bytes)
                .with_context(|| format!("Failed to snapshot {}", input.path))?;
        }
        Ok(())
    }

    /// Explain why a build isn't hitting the cache, by comparing the current
    /// state of `dir` against the most recent run of the same target.
    ///
    /// `None` when there is no previous run to compare against — the honest
    /// answer for a first build, and different from "nothing changed".
    pub fn explain(
        &self,
        workspace: &str,
        target: &str,
        dir: &Path,
        inputs: &[FileHash],
        env: &BTreeMap<String, String>,
        upstream: &BTreeMap<String, String>,
    ) -> Result<Option<crate::cache::diff::Diff>> {
        let Some(previous) = self.latest_for(workspace, target)? else {
            return Ok(None);
        };
        let snapshots = self.snapshot_dir(&previous.key);
        crate::cache::diff::compute(&previous, dir, &snapshots, inputs, env, upstream).map(Some)
    }

    /// Write (or overwrite) an entry's manifest. Used by `put` and by the
    /// remote cache server, which receives artifacts rather than copying them.
    pub fn write_manifest(&self, entry: &Entry) -> Result<()> {
        let path = self.manifest_path(&entry.key);
        let body = serde_json::to_string_pretty(entry)?;
        std::fs::write(&path, body).with_context(|| format!("Failed to write {}", path.display()))
    }

    /// Copy an entry's stored outputs back into `dir`, returning what was
    /// restored.
    ///
    /// Every file is verified against the manifest hash on the way out. A
    /// corrupted store must not quietly become a corrupted build directory —
    /// far better to fail here, where the message can say which file.
    pub fn restore(&self, key: &str, dir: &Path) -> Result<Vec<FileHash>> {
        let entry = self
            .get(key)?
            .with_context(|| format!("No cache entry for key {key}"))?;
        let artifacts = self.artifact_dir(key);

        for output in &entry.outputs {
            let from = artifacts.join(&output.path);
            let actual = super::hash_file(&from).with_context(|| {
                format!(
                    "Cache entry {key} is missing {} — the cache is damaged; \
                     run `ciabatta cache clean` and build again",
                    output.path
                )
            })?;
            anyhow::ensure!(
                actual == output.sha256,
                "Cached file {} does not match its recorded hash — the cache is damaged; \
                 run `ciabatta cache clean` and build again",
                output.path
            );

            let to = dir.join(&output.path);
            if let Some(parent) = to.parent() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("Failed to create {}", parent.display()))?;
            }
            std::fs::copy(&from, &to)
                .with_context(|| format!("Failed to restore {}", to.display()))?;
        }

        self.touch(key)?;
        Ok(entry.outputs)
    }

    /// Record that an entry was used, so retention ages it from now.
    pub fn touch(&self, key: &str) -> Result<()> {
        if let Some(mut entry) = self.get(key)? {
            entry.last_used_at = Some(now());
            self.write_manifest(&entry)?;
        }
        Ok(())
    }

    /// Delete one entry and its artifacts.
    pub fn remove(&self, key: &str) -> Result<()> {
        let _ = std::fs::remove_file(self.manifest_path(key));
        let _ = std::fs::remove_dir_all(self.artifact_dir(key));
        let _ = std::fs::remove_dir_all(self.snapshot_dir(key));
        Ok(())
    }

    /// Delete everything.
    pub fn clear(&self) -> Result<usize> {
        let entries = self.list()?;
        for entry in &entries {
            self.remove(&entry.key)?;
        }
        // Artifacts with no manifest — the debris of an interrupted `put`.
        self.sweep_orphans()?;
        Ok(entries.len())
    }

    /// Remove artifact directories with no surviving manifest.
    pub fn sweep_orphans(&self) -> Result<usize> {
        let known: Vec<String> = self.list()?.into_iter().map(|e| e.key).collect();
        let dir = self.root.join(ARTIFACTS_DIR);
        let Ok(read) = std::fs::read_dir(&dir) else {
            return Ok(0);
        };

        let mut removed = 0;
        for entry in read.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if !known.iter().any(|k| k == name) {
                let _ = std::fs::remove_dir_all(&path);
                removed += 1;
            }
        }
        Ok(removed)
    }

    /// Apply a retention policy, evicting whatever breaches it.
    ///
    /// Age first, then size, then count — so an entry that's simply too old is
    /// reported as too old rather than as a least-recently-used casualty of the
    /// size cap. Within the size and count passes, least recently used goes
    /// first: the artifact nobody has needed in a fortnight is a better thing
    /// to lose than the one three builds pulled this morning.
    pub fn prune(&self, policy: &Retention) -> Result<Pruned> {
        let mut result = Pruned::default();
        if policy.is_unlimited() {
            result.orphans = self.sweep_orphans()?;
            return Ok(result);
        }

        // Least recently used first, so both the size and count passes evict
        // from the front.
        let mut entries = self.list()?;
        entries.sort_by(|a, b| a.last_touched().cmp(b.last_touched()));

        let mut kept: Vec<Entry> = Vec::with_capacity(entries.len());

        if let Some(max_age) = policy.max_age_seconds()? {
            for entry in entries {
                if entry.age_seconds().is_some_and(|age| age > max_age) {
                    result.freed += entry.size;
                    self.remove(&entry.key)?;
                    result.removed.push((entry.key, "too old"));
                } else {
                    kept.push(entry);
                }
            }
        } else {
            kept = entries;
        }

        if let Some(max_size) = policy.max_size_bytes()? {
            let mut total: u64 = kept.iter().map(|e| e.size).sum();
            let mut survivors: Vec<Entry> = Vec::with_capacity(kept.len());
            let mut iter = kept.into_iter();
            for entry in iter.by_ref() {
                if total <= max_size {
                    survivors.push(entry);
                    continue;
                }
                total = total.saturating_sub(entry.size);
                result.freed += entry.size;
                self.remove(&entry.key)?;
                result.removed.push((entry.key, "over the size limit"));
            }
            kept = survivors;
        }

        if let Some(max_entries) = policy.max_entries {
            while kept.len() > max_entries {
                let entry = kept.remove(0);
                result.freed += entry.size;
                self.remove(&entry.key)?;
                result.removed.push((entry.key, "over the entry limit"));
            }
        }

        result.orphans = self.sweep_orphans()?;
        Ok(result)
    }

    /// What the store currently holds.
    pub fn stats(&self) -> Result<Stats> {
        let entries = self.list()?;
        let mut by_workspace: BTreeMap<String, usize> = BTreeMap::new();
        let mut size = 0u64;
        let mut saved_ms = 0u64;
        for entry in &entries {
            *by_workspace.entry(entry.workspace.clone()).or_default() += 1;
            size += entry.size;
            saved_ms += entry.duration_ms;
        }
        Ok(Stats {
            entries: entries.len(),
            size,
            by_workspace,
            build_time_ms: saved_ms,
            oldest: entries.last().map(|e| e.created_at.clone()),
            newest: entries.first().map(|e| e.created_at.clone()),
        })
    }
}

// ─── Retention ──────────────────────────────────────────────────────────────

/// When cached artifacts stop being worth their disk space.
///
/// All three limits are optional and apply together: an entry is evicted if it
/// breaches *any* of them. With none set nothing is ever pruned, which is the
/// right default for a laptop and the wrong one for a shared server — so
/// `ciabatta remote-cache init` writes a policy and a local store doesn't.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Retention {
    /// Evict entries unused for longer than this (`"30d"`, `"12h"`, `"90m"`).
    ///
    /// Age is measured from *last use*, not from creation. An artifact everyone
    /// still depends on shouldn't be evicted just because its inputs happen not
    /// to have changed in a month — that's precisely the entry a cache exists
    /// to keep.
    pub max_age: Option<String>,

    /// Cap the whole store at this many bytes, evicting least-recently-used
    /// entries until it fits. Accepts `"10GB"`, `"500MB"`, or a bare byte count.
    pub max_size: Option<String>,

    /// Cap the number of entries, evicting least-recently-used first.
    pub max_entries: Option<usize>,
}

impl Default for Retention {
    fn default() -> Self {
        // The default a server gets when it writes a config: keep a month, cap
        // at ten gigabytes. Both are easy to reason about and easy to change.
        Retention {
            max_age: Some("30d".to_string()),
            max_size: Some("10GB".to_string()),
            max_entries: None,
        }
    }
}

impl Retention {
    /// A policy that never evicts anything.
    pub fn unlimited() -> Self {
        Retention {
            max_age: None,
            max_size: None,
            max_entries: None,
        }
    }

    /// Whether this policy would ever remove anything.
    pub fn is_unlimited(&self) -> bool {
        self.max_age.is_none() && self.max_size.is_none() && self.max_entries.is_none()
    }

    /// `max_age` in seconds.
    pub fn max_age_seconds(&self) -> Result<Option<i64>> {
        self.max_age.as_deref().map(parse_duration).transpose()
    }

    /// `max_size` in bytes.
    pub fn max_size_bytes(&self) -> Result<Option<u64>> {
        self.max_size.as_deref().map(parse_size).transpose()
    }

    /// One line describing the policy, for `cache status` and the web view.
    pub fn describe(&self) -> String {
        if self.is_unlimited() {
            return "keep everything".to_string();
        }
        let mut parts: Vec<String> = Vec::new();
        if let Some(age) = &self.max_age {
            parts.push(format!("unused for {age}"));
        }
        if let Some(size) = &self.max_size {
            parts.push(format!("over {size}"));
        }
        if let Some(count) = self.max_entries {
            parts.push(format!("beyond {count} entries"));
        }
        format!("evict when {}", parts.join(", or "))
    }
}

/// What a prune pass removed.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Pruned {
    /// The keys evicted, with why.
    pub removed: Vec<(String, &'static str)>,
    /// Bytes reclaimed.
    pub freed: u64,
    /// Orphaned artifact directories swept up on the way through.
    pub orphans: usize,
}

impl Pruned {
    pub fn is_empty(&self) -> bool {
        self.removed.is_empty() && self.orphans == 0
    }
}

/// Parse a duration like `30d`, `12h`, `90m`, `45s`, or a bare number of
/// seconds, into seconds.
pub fn parse_duration(raw: &str) -> Result<i64> {
    let raw = raw.trim();
    anyhow::ensure!(!raw.is_empty(), "empty duration");
    let (number, multiplier) = match raw.chars().last().unwrap().to_ascii_lowercase() {
        'd' => (&raw[..raw.len() - 1], 86_400),
        'h' => (&raw[..raw.len() - 1], 3_600),
        'm' => (&raw[..raw.len() - 1], 60),
        's' => (&raw[..raw.len() - 1], 1),
        _ => (raw, 1),
    };
    let value: i64 = number.trim().parse().with_context(|| {
        format!("could not parse duration '{raw}' (try \"30d\", \"12h\", \"90m\")")
    })?;
    Ok(value * multiplier)
}

/// Parse a size like `10GB`, `500MB`, `2g`, or a bare byte count, into bytes.
pub fn parse_size(raw: &str) -> Result<u64> {
    let raw = raw.trim();
    anyhow::ensure!(!raw.is_empty(), "empty size");
    let lower = raw.to_ascii_lowercase();
    let trimmed = lower.trim_end_matches('b');
    let (number, multiplier) = match trimmed.chars().last() {
        Some('k') => (&trimmed[..trimmed.len() - 1], 1024u64),
        Some('m') => (&trimmed[..trimmed.len() - 1], 1024 * 1024),
        Some('g') => (&trimmed[..trimmed.len() - 1], 1024 * 1024 * 1024),
        Some('t') => (&trimmed[..trimmed.len() - 1], 1024u64.pow(4)),
        _ => (trimmed, 1),
    };
    let value: f64 = number
        .trim()
        .parse()
        .with_context(|| format!("could not parse size '{raw}' (try \"10GB\", \"500MB\")"))?;
    Ok((value * multiplier as f64) as u64)
}

/// A summary of what a store holds, for `ciabatta cache status` and the
/// remote cache's web view.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Stats {
    pub entries: usize,
    /// Total bytes of stored artifacts.
    pub size: u64,
    /// Entry count per workspace.
    pub by_workspace: BTreeMap<String, usize>,
    /// Total build time represented by the stored entries — what a full set of
    /// hits would save.
    pub build_time_ms: u64,
    pub oldest: Option<String>,
    pub newest: Option<String>,
}

/// Current time as an RFC 3339 string, in UTC so entries compare across
/// machines in different timezones.
pub fn now() -> String {
    chrono::Utc::now().to_rfc3339()
}

/// Render a byte count the way a human would say it.
pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ciab_store_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn build_output(dir: &Path, rel: &str, body: &str) -> FileHash {
        let path = dir.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, body).unwrap();
        FileHash {
            path: rel.to_string(),
            sha256: super::super::hash_file(&path).unwrap(),
            size: body.len() as u64,
        }
    }

    #[test]
    fn outputs_round_trip_through_the_store() {
        let root = scratch("roundtrip");
        let workdir = root.join("work");
        std::fs::create_dir_all(&workdir).unwrap();
        let store = Store::at(root.join("cache")).unwrap();

        let outputs = vec![
            build_output(&workdir, "dist/app", "the binary"),
            build_output(&workdir, "dist/nested/lib.so", "the library"),
        ];
        store
            .put(
                "k1",
                &workdir,
                Build {
                    target: "build".into(),
                    workspace: "api".into(),
                    outputs: outputs.clone(),
                    duration_ms: 4200,
                    ..Default::default()
                },
            )
            .unwrap();

        // Blow the build directory away — that's the case a cache exists for.
        std::fs::remove_dir_all(workdir.join("dist")).unwrap();
        assert!(!workdir.join("dist/app").exists());

        let restored = store.restore("k1", &workdir).unwrap();
        assert_eq!(restored.len(), 2);
        assert_eq!(
            std::fs::read_to_string(workdir.join("dist/app")).unwrap(),
            "the binary"
        );
        assert_eq!(
            std::fs::read_to_string(workdir.join("dist/nested/lib.so")).unwrap(),
            "the library",
            "nested output paths must be recreated"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_damaged_cache_fails_loudly_rather_than_corrupting_the_build() {
        let root = scratch("damaged");
        let workdir = root.join("work");
        std::fs::create_dir_all(&workdir).unwrap();
        let store = Store::at(root.join("cache")).unwrap();

        let outputs = vec![build_output(&workdir, "dist/app", "the binary")];
        store
            .put(
                "k1",
                &workdir,
                Build {
                    target: "build".into(),
                    workspace: "api".into(),
                    outputs,
                    duration_ms: 10,
                    ..Default::default()
                },
            )
            .unwrap();

        // Somebody (a sync, a disk, a stray editor) corrupts the stored copy.
        std::fs::write(store.artifact_dir("k1").join("dist/app"), "tampered").unwrap();

        let err = store.restore("k1", &workdir).unwrap_err();
        assert!(
            format!("{err:#}").contains("does not match its recorded hash"),
            "got: {err:#}"
        );

        // And a manifest whose artifacts vanished reads as a miss, not a hit.
        std::fs::remove_dir_all(store.artifact_dir("k1")).unwrap();
        assert!(!store.has_artifacts("k1").unwrap());

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_corrupt_manifest_costs_a_rebuild_not_a_failure() {
        let root = scratch("corrupt");
        let store = Store::at(root.join("cache")).unwrap();
        std::fs::write(
            store.root().join(ENTRIES_DIR).join("k1.json"),
            "{not json at all",
        )
        .unwrap();

        assert!(store.get("k1").unwrap().is_none());
        assert!(store.list().unwrap().is_empty());

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn listing_stats_and_sweeping_orphans() {
        let root = scratch("stats");
        let workdir = root.join("work");
        std::fs::create_dir_all(&workdir).unwrap();
        let store = Store::at(root.join("cache")).unwrap();

        let a = vec![build_output(&workdir, "dist/a", "aaaa")];
        let b = vec![build_output(&workdir, "dist/b", "bb")];
        store
            .put(
                "k1",
                &workdir,
                Build {
                    target: "build".into(),
                    workspace: "api".into(),
                    outputs: a,
                    duration_ms: 1000,
                    ..Default::default()
                },
            )
            .unwrap();
        store
            .put(
                "k2",
                &workdir,
                Build {
                    target: "build".into(),
                    workspace: "web".into(),
                    outputs: b,
                    duration_ms: 2000,
                    ..Default::default()
                },
            )
            .unwrap();

        let stats = store.stats().unwrap();
        assert_eq!(stats.entries, 2);
        assert_eq!(stats.size, 6);
        assert_eq!(stats.build_time_ms, 3000);
        assert_eq!(stats.by_workspace["api"], 1);
        assert_eq!(stats.by_workspace["web"], 1);

        assert_eq!(store.latest_for("api", "build").unwrap().unwrap().key, "k1");
        assert!(store.latest_for("api", "nope").unwrap().is_none());

        // An interrupted `put` leaves artifacts with no manifest.
        std::fs::create_dir_all(store.artifact_dir("orphan")).unwrap();
        assert_eq!(store.sweep_orphans().unwrap(), 1);
        assert!(!store.artifact_dir("orphan").exists());
        assert_eq!(store.list().unwrap().len(), 2, "real entries survive");

        assert_eq!(store.clear().unwrap(), 2);
        assert!(store.list().unwrap().is_empty());

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn durations_and_sizes_parse_the_way_people_write_them() {
        assert_eq!(parse_duration("30d").unwrap(), 30 * 86_400);
        assert_eq!(parse_duration("12h").unwrap(), 12 * 3_600);
        assert_eq!(parse_duration("90m").unwrap(), 5_400);
        assert_eq!(parse_duration("45s").unwrap(), 45);
        assert_eq!(
            parse_duration("600").unwrap(),
            600,
            "a bare number is seconds"
        );
        assert!(parse_duration("soon").is_err());

        assert_eq!(parse_size("10GB").unwrap(), 10 * 1024 * 1024 * 1024);
        assert_eq!(parse_size("500MB").unwrap(), 500 * 1024 * 1024);
        assert_eq!(parse_size("2g").unwrap(), 2 * 1024 * 1024 * 1024);
        assert_eq!(parse_size("1.5MB").unwrap(), (1.5 * 1024.0 * 1024.0) as u64);
        assert_eq!(parse_size("4096").unwrap(), 4096, "a bare number is bytes");
        assert!(parse_size("lots").is_err());
    }

    #[test]
    fn retention_evicts_by_age_size_and_count_least_used_first() {
        let root = scratch("retention");
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let store = Store::at(root.join("cache")).unwrap();

        // Four entries of 100 bytes each, used at four different times.
        for (key, used_days_ago) in [("k1", 40), ("k2", 10), ("k3", 5), ("k4", 1)] {
            let outputs = vec![build_output(
                &work,
                &format!("dist/{key}"),
                &"x".repeat(100),
            )];
            store
                .put(
                    key,
                    &work,
                    Build {
                        target: "build".into(),
                        workspace: "api".into(),
                        outputs,
                        duration_ms: 100,
                        ..Default::default()
                    },
                )
                .unwrap();
            let mut entry = store.get(key).unwrap().unwrap();
            entry.last_used_at =
                Some((chrono::Utc::now() - chrono::Duration::days(used_days_ago)).to_rfc3339());
            store.write_manifest(&entry).unwrap();
        }

        // An unlimited policy keeps everything but still sweeps orphans.
        std::fs::create_dir_all(store.artifact_dir("orphan")).unwrap();
        let pruned = store.prune(&Retention::unlimited()).unwrap();
        assert!(pruned.removed.is_empty());
        assert_eq!(pruned.orphans, 1);
        assert_eq!(store.list().unwrap().len(), 4);

        // Age evicts only the 40-day-old one, and says why.
        let pruned = store
            .prune(&Retention {
                max_age: Some("30d".into()),
                max_size: None,
                max_entries: None,
            })
            .unwrap();
        assert_eq!(pruned.removed.len(), 1);
        assert_eq!(pruned.removed[0].0, "k1");
        assert_eq!(pruned.removed[0].1, "too old");
        assert_eq!(pruned.freed, 100);
        assert!(store.get("k1").unwrap().is_none());
        assert!(
            !store.artifact_dir("k1").exists(),
            "artifacts go with the manifest"
        );

        // A 250-byte cap over 300 bytes of entries drops the least recently
        // used one — k2, at ten days — and keeps this morning's.
        let pruned = store
            .prune(&Retention {
                max_age: None,
                max_size: Some("250".into()),
                max_entries: None,
            })
            .unwrap();
        assert_eq!(pruned.removed.len(), 1);
        assert_eq!(pruned.removed[0].0, "k2");
        assert_eq!(pruned.removed[0].1, "over the size limit");

        // And the count cap does the same, oldest-used first.
        let pruned = store
            .prune(&Retention {
                max_age: None,
                max_size: None,
                max_entries: Some(1),
            })
            .unwrap();
        assert_eq!(pruned.removed.len(), 1);
        assert_eq!(pruned.removed[0].0, "k3");
        let survivors: Vec<String> = store.list().unwrap().into_iter().map(|e| e.key).collect();
        assert_eq!(
            survivors,
            vec!["k4".to_string()],
            "the freshest entry survives"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_default_retention_policy_describes_itself() {
        assert!(Retention::unlimited().is_unlimited());
        assert_eq!(Retention::unlimited().describe(), "keep everything");

        let policy = Retention::default();
        assert!(!policy.is_unlimited());
        assert_eq!(policy.describe(), "evict when unused for 30d, or over 10GB");
    }

    #[test]
    fn human_size_reads_like_a_person_wrote_it() {
        assert_eq!(human_size(0), "0 B");
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(1024), "1.0 KB");
        assert_eq!(human_size(1536), "1.5 KB");
        assert_eq!(human_size(5 * 1024 * 1024), "5.0 MB");
    }
}
