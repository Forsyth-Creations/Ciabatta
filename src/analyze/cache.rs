//! A content-hash cache for the analyze file traversal.
//!
//! Each scanned file's parsed contribution (its [`ScanOutput`]) is stored under
//! `.ciabatta/.cache/analyze.json`, keyed by the file's relative path and a hash
//! of its contents. On the next run, a file whose hash is unchanged is served
//! from the cache instead of being re-parsed.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::ScanOutput;

/// Sub-directory (under `.ciabatta/`) where caches live.
pub const CACHE_DIR: &str = ".cache";
/// Cache file name within [`CACHE_DIR`].
pub const CACHE_FILE: &str = "analyze.json";
/// Bumped when the on-disk format or scanning logic changes incompatibly.
const CACHE_VERSION: u32 = 1;

#[derive(Serialize, Deserialize)]
struct CacheFile {
    version: u32,
    entries: BTreeMap<String, CacheEntry>,
}

#[derive(Serialize, Deserialize, Clone)]
struct CacheEntry {
    hash: String,
    output: ScanOutput,
}

/// FNV-1a (64-bit) hash of some content, as a stable hex string. Not
/// cryptographic — just a fast, deterministic change detector.
pub fn hash_content(content: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &byte in content.as_bytes() {
        h ^= byte as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{h:016x}")
}

pub struct Cache {
    dir: PathBuf,
    /// Entries loaded from the previous run.
    previous: BTreeMap<String, CacheEntry>,
    /// Entries seen this run; this becomes the next on-disk cache (so files that
    /// no longer exist are pruned automatically).
    current: BTreeMap<String, CacheEntry>,
    hits: usize,
    misses: usize,
}

impl Cache {
    /// Load the cache from `dir` (an empty cache if it's missing or unreadable).
    pub fn load(dir: &Path) -> Self {
        let previous = std::fs::read_to_string(dir.join(CACHE_FILE))
            .ok()
            .and_then(|s| serde_json::from_str::<CacheFile>(&s).ok())
            .filter(|c| c.version == CACHE_VERSION)
            .map(|c| c.entries)
            .unwrap_or_default();

        Cache {
            dir: dir.to_path_buf(),
            previous,
            current: BTreeMap::new(),
            hits: 0,
            misses: 0,
        }
    }

    /// Return the cached output for `rel` if its hash matches `hash`.
    pub fn get(&mut self, rel: &str, hash: &str) -> Option<ScanOutput> {
        match self.previous.get(rel) {
            Some(entry) if entry.hash == hash => {
                self.hits += 1;
                Some(entry.output.clone())
            }
            _ => {
                self.misses += 1;
                None
            }
        }
    }

    /// Record the output for `rel` (cached or freshly parsed) for this run.
    pub fn put(&mut self, rel: &str, hash: String, output: ScanOutput) {
        self.current
            .insert(rel.to_string(), CacheEntry { hash, output });
    }

    pub fn hits(&self) -> usize {
        self.hits
    }

    pub fn misses(&self) -> usize {
        self.misses
    }

    /// Persist the entries seen this run.
    pub fn save(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.dir)?;
        let data = CacheFile {
            version: CACHE_VERSION,
            entries: self.current.clone(),
        };
        let json = serde_json::to_string_pretty(&data)
            .unwrap_or_else(|_| "{\"version\":1,\"entries\":{}}".to_string());
        std::fs::write(self.dir.join(CACHE_FILE), json)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_is_deterministic_and_sensitive() {
        assert_eq!(hash_content("hello"), hash_content("hello"));
        assert_ne!(hash_content("hello"), hash_content("hello!"));
        assert_eq!(hash_content("hello").len(), 16);
    }

    #[test]
    fn get_hits_only_on_matching_hash() {
        let dir = std::env::temp_dir().join(format!("ciabatta-cache-{}", std::process::id()));
        let mut c = Cache::load(&dir);
        assert!(c.get("Cargo.toml", "abc").is_none()); // miss: not present
        c.put("Cargo.toml", "abc".into(), ScanOutput::default());
        // `put` records into `current`, not `previous`, so a same-run get misses.
        assert!(c.get("Cargo.toml", "abc").is_none());
    }
}
