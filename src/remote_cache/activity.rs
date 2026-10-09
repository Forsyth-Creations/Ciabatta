//! What the remote cache has been doing, kept long enough to act on.
//!
//! The counters used to live in memory and reset with the server, and they
//! only ever said *how many*: 40% of lookups missed. That number can tell an
//! operator something is wrong and nothing about what — which project, which
//! target, since when, and whether anybody is even writing to the cache any
//! more. This keeps the answers to those:
//!
//! * per project and per **target**, hits, misses, and uploads, with the last
//!   time each happened — so "`api:compile` has missed 30 times and never hit"
//!   is something the server can say;
//! * when the cache was last used and last saved to, overall and per project;
//! * a short log of recent traffic, and what the last retention sweep evicted;
//!
//! and from those, [`insights`]: the handful of things worth doing something
//! about, each phrased as the thing to do.
//!
//! Persisted next to the store, flushed every few seconds rather than on every
//! request — a cache serving a CI fleet answers a lot of lookups, and losing a
//! few seconds of counts in a crash costs nothing anyone would notice.

use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use super::projects::Counters;

/// How many recent events are kept.
const RECENT: usize = 200;

/// How many targets a project keeps statistics for. Bounded so a client that
/// sends a fresh target name with every lookup can't grow the file forever;
/// past it, the least recently active target makes room.
const MAX_TARGETS: usize = 500;

/// Everything recorded, as it is written to disk.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Recorded {
    /// When this server started keeping these numbers.
    #[serde(default)]
    pub since: Option<String>,
    #[serde(default)]
    pub projects: BTreeMap<String, ProjectActivity>,
    #[serde(default)]
    pub recent: VecDeque<Event>,
    /// What the most recent retention sweep that evicted anything did.
    #[serde(default)]
    pub last_eviction: Option<Eviction>,
}

/// One project's traffic.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProjectActivity {
    #[serde(flatten)]
    pub counters: Counters,
    #[serde(default)]
    pub touches: u64,
    #[serde(default)]
    pub last_hit_at: Option<String>,
    #[serde(default)]
    pub last_miss_at: Option<String>,
    #[serde(default)]
    pub last_upload_at: Option<String>,
    #[serde(default)]
    pub last_touch_at: Option<String>,
    #[serde(default)]
    pub targets: BTreeMap<String, TargetActivity>,
}

/// One target's traffic within a project.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct TargetActivity {
    pub hits: u64,
    pub misses: u64,
    pub uploads: u64,
    #[serde(default)]
    pub bytes_stored: u64,
    #[serde(default)]
    pub last_hit_at: Option<String>,
    #[serde(default)]
    pub last_miss_at: Option<String>,
    #[serde(default)]
    pub last_upload_at: Option<String>,
}

impl TargetActivity {
    fn last_active(&self) -> Option<&str> {
        [&self.last_hit_at, &self.last_miss_at, &self.last_upload_at]
            .into_iter()
            .flatten()
            .map(String::as_str)
            .max()
    }
}

impl ProjectActivity {
    /// When anything last read from or wrote to this project's cache.
    pub fn last_used_at(&self) -> Option<&str> {
        [&self.last_hit_at, &self.last_upload_at, &self.last_touch_at]
            .into_iter()
            .flatten()
            .map(String::as_str)
            .max()
    }
}

/// One thing that happened.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Event {
    pub at: String,
    pub project: String,
    /// `hit` · `miss` · `upload` · `touch`.
    pub kind: String,
    #[serde(default)]
    pub target: Option<String>,
    /// The key, shortened — enough to match against a client's log.
    #[serde(default)]
    pub key: Option<String>,
    #[serde(default)]
    pub user: Option<String>,
    #[serde(default)]
    pub bytes: u64,
    /// For a touch: how many entries it refreshed.
    #[serde(default)]
    pub count: u64,
}

/// What a retention sweep removed.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Eviction {
    pub at: String,
    pub removed: usize,
    pub freed: u64,
}

/// The live record, shared by every request handler.
#[derive(Debug)]
pub struct Activity {
    path: PathBuf,
    inner: Mutex<Recorded>,
    dirty: AtomicBool,
}

/// What a request did, for [`Activity::record`].
pub struct Traffic<'a> {
    pub project: &'a str,
    pub kind: Kind,
    pub target: Option<&'a str>,
    pub key: Option<&'a str>,
    pub user: Option<&'a str>,
    pub bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Hit,
    Miss,
    Upload,
    /// Entries a client reused locally, reported so they age from now.
    Touch(u64),
}

impl Kind {
    fn label(self) -> &'static str {
        match self {
            Kind::Hit => "hit",
            Kind::Miss => "miss",
            Kind::Upload => "upload",
            Kind::Touch(_) => "touch",
        }
    }
}

impl Activity {
    /// Open (or start) the record under `storage`.
    ///
    /// A file that won't parse starts the record afresh rather than refusing
    /// to serve: these are statistics, and a cache that wouldn't start over
    /// its own bookkeeping would be the wrong way round.
    pub fn open(storage: &Path) -> Result<Self> {
        std::fs::create_dir_all(storage)
            .with_context(|| format!("Failed to create {}", storage.display()))?;
        let path = storage.join("activity.json");
        let mut recorded: Recorded = std::fs::read_to_string(&path)
            .ok()
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_default();
        if recorded.since.is_none() {
            recorded.since = Some(crate::cache::store::now());
        }
        Ok(Activity {
            path,
            inner: Mutex::new(recorded),
            dirty: AtomicBool::new(true),
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Recorded> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Record one request's worth of traffic.
    pub fn record(&self, traffic: Traffic<'_>) {
        let at = crate::cache::store::now();
        let mut recorded = self.lock();
        let project = recorded
            .projects
            .entry(traffic.project.to_string())
            .or_default();

        match traffic.kind {
            Kind::Hit => {
                project.counters.hits += 1;
                project.counters.bytes_served += traffic.bytes;
                project.last_hit_at = Some(at.clone());
            }
            Kind::Miss => {
                project.counters.misses += 1;
                project.last_miss_at = Some(at.clone());
            }
            Kind::Upload => {
                project.counters.uploads += 1;
                project.counters.bytes_stored += traffic.bytes;
                project.last_upload_at = Some(at.clone());
            }
            Kind::Touch(count) => {
                project.touches += count;
                project.last_touch_at = Some(at.clone());
            }
        }

        if let Some(name) = traffic.target.filter(|t| !t.is_empty()) {
            if !project.targets.contains_key(name) && project.targets.len() >= MAX_TARGETS {
                // Make room by forgetting whichever target went quiet longest.
                if let Some(oldest) = project
                    .targets
                    .iter()
                    .min_by(|a, b| a.1.last_active().cmp(&b.1.last_active()))
                    .map(|(name, _)| name.clone())
                {
                    project.targets.remove(&oldest);
                }
            }
            let target = project.targets.entry(name.to_string()).or_default();
            match traffic.kind {
                Kind::Hit => {
                    target.hits += 1;
                    target.last_hit_at = Some(at.clone());
                }
                Kind::Miss => {
                    target.misses += 1;
                    target.last_miss_at = Some(at.clone());
                }
                Kind::Upload => {
                    target.uploads += 1;
                    target.bytes_stored += traffic.bytes;
                    target.last_upload_at = Some(at.clone());
                }
                Kind::Touch(_) => {}
            }
        }

        recorded.recent.push_front(Event {
            at,
            project: traffic.project.to_string(),
            kind: traffic.kind.label().to_string(),
            target: traffic.target.map(str::to_string),
            key: traffic.key.map(|k| k.chars().take(12).collect()),
            user: traffic.user.map(str::to_string),
            bytes: traffic.bytes,
            count: match traffic.kind {
                Kind::Touch(count) => count,
                _ => 0,
            },
        });
        recorded.recent.truncate(RECENT);
        drop(recorded);
        self.dirty.store(true, Ordering::Relaxed);
    }

    /// Note what a retention sweep evicted.
    pub fn record_eviction(&self, removed: usize, freed: u64) {
        self.lock().last_eviction = Some(Eviction {
            at: crate::cache::store::now(),
            removed,
            freed,
        });
        self.dirty.store(true, Ordering::Relaxed);
    }

    /// Forget a project's traffic, when the project itself is forgotten.
    pub fn forget(&self, project: &str) {
        let mut recorded = self.lock();
        recorded.projects.remove(project);
        recorded.recent.retain(|e| e.project != project);
        drop(recorded);
        self.dirty.store(true, Ordering::Relaxed);
    }

    /// A copy of everything, for the stats endpoint.
    pub fn snapshot(&self) -> Recorded {
        self.lock().clone()
    }

    /// Every project's counters, summed.
    pub fn totals(&self) -> Counters {
        self.lock()
            .projects
            .values()
            .fold(Counters::default(), |mut acc, p| {
                acc.hits += p.counters.hits;
                acc.misses += p.counters.misses;
                acc.uploads += p.counters.uploads;
                acc.bytes_served += p.counters.bytes_served;
                acc.bytes_stored += p.counters.bytes_stored;
                acc
            })
    }

    /// Write the record to disk, if anything changed since the last write.
    pub fn flush(&self) -> Result<()> {
        if !self.dirty.swap(false, Ordering::Relaxed) {
            return Ok(());
        }
        let body = serde_json::to_string(&*self.lock())?;
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, body).with_context(|| format!("Failed to write {}", tmp.display()))?;
        std::fs::rename(&tmp, &self.path)
            .with_context(|| format!("Failed to write {}", self.path.display()))
    }
}

/// Something about the cache worth doing something about.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Insight {
    /// `warn` for a problem costing builds now, `info` for housekeeping.
    pub severity: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    /// What's wrong, in one sentence.
    pub message: String,
    /// What to do about it.
    pub action: String,
}

/// The facts [`insights`] reasons from that aren't in the activity record.
pub struct InsightContext<'a> {
    /// Project id → its display name.
    pub names: &'a BTreeMap<String, String>,
    pub stored_bytes: u64,
    /// The retention policy's size limit, when it has one.
    pub max_bytes: Option<u64>,
    pub retention: &'a str,
}

/// Lookups a target must have seen before a pattern in them means anything.
const ENOUGH: u64 = 5;

/// How long without an upload before a busy cache is called unwritten.
const QUIET_WRITES_DAYS: i64 = 7;

/// What to look at, most expensive first.
///
/// Every line is something an operator — or the owner of a target — can go
/// and change. Nothing here is a restatement of a number already on screen.
pub fn insights(recorded: &Recorded, context: &InsightContext<'_>) -> Vec<Insight> {
    let mut out: Vec<Insight> = Vec::new();
    let name = |id: &str| {
        context
            .names
            .get(id)
            .cloned()
            .unwrap_or_else(|| id.to_string())
    };

    for (id, project) in &recorded.projects {
        for (target, stats) in &project.targets {
            // Built and published over and over, and never once reused: its key
            // can't be stable. The single most valuable thing on this list,
            // because every one of those uploads was a full build.
            if stats.uploads >= ENOUGH && stats.hits == 0 {
                out.push(Insight {
                    severity: "warn",
                    project: Some(name(id)),
                    target: Some(target.clone()),
                    message: format!(
                        "{target} has been uploaded {} times and never reused.",
                        stats.uploads
                    ),
                    action: "Its cache key changes on every build. Open a run of it in cache \
                             inspect mode to see what moved — usually an undeclared input that \
                             changes each time, a timestamp written into an output it depends \
                             on, or an upstream step with no `cache.outputs`."
                        .to_string(),
                });
                continue;
            }
            // Asked for repeatedly, and nobody has ever put it there.
            if stats.misses >= ENOUGH && stats.uploads == 0 && stats.hits == 0 {
                out.push(Insight {
                    severity: "warn",
                    project: Some(name(id)),
                    target: Some(target.clone()),
                    message: format!(
                        "{target} has missed {} times and has never been uploaded.",
                        stats.misses
                    ),
                    action: "Nobody is writing it. Either every client that builds it is \
                             read-only (point a CI job on the main branch at the cache without \
                             `read_only`), or the step declares no `cache.outputs`, so there is \
                             nothing to publish."
                        .to_string(),
                });
                continue;
            }
            // Mostly missing, though it is uploaded: entries aren't matching
            // across machines.
            let lookups = stats.hits + stats.misses;
            if lookups >= ENOUGH * 2 && stats.hits * 4 < stats.misses && stats.uploads > 0 {
                out.push(Insight {
                    severity: "warn",
                    project: Some(name(id)),
                    target: Some(target.clone()),
                    message: format!("{target} hits only {} of {lookups} lookups.", stats.hits),
                    action: "Machines are building it with different keys. Compare a missing \
                             run's inputs with the uploader's: an OS-specific file, an absolute \
                             path, or a variable that differs between CI and laptops is usually \
                             to blame."
                        .to_string(),
                });
            }
        }

        // Plenty of traffic, nothing written for a week: whatever used to
        // populate the cache has stopped.
        let lookups = project.counters.hits + project.counters.misses;
        if project.counters.misses >= ENOUGH
            && let Some(last_miss) = project.last_miss_at.as_deref()
            && days_between(project.last_upload_at.as_deref(), last_miss)
                .is_none_or(|d| d >= QUIET_WRITES_DAYS)
            && project.last_upload_at.is_some()
        {
            out.push(Insight {
                severity: "warn",
                project: Some(name(id)),
                target: None,
                message: format!(
                    "{} is still being asked for builds, but nothing has been uploaded to it \
                     for over {QUIET_WRITES_DAYS} days.",
                    name(id)
                ),
                action: "Whatever used to publish to this cache has stopped — check that the \
                         CI job that writes to it still runs, and still has a session \
                         (`ciabatta remote-cache login`)."
                    .to_string(),
            });
        }
        if lookups >= ENOUGH * 4
            && project.counters.hits * 4 < lookups
            && !out.iter().any(|i| i.project.as_deref() == Some(&name(id)))
        {
            out.push(Insight {
                severity: "warn",
                project: Some(name(id)),
                target: None,
                message: format!(
                    "{} reuses only {} of {lookups} lookups.",
                    name(id),
                    project.counters.hits
                ),
                action: "Turn on cache inspect mode on a run's page to see, step by step, \
                         why each missed."
                    .to_string(),
            });
        }
    }

    if let Some(max) = context.max_bytes
        && max > 0
        && context.stored_bytes * 10 >= max * 8
    {
        out.push(Insight {
            severity: if context.stored_bytes >= max {
                "warn"
            } else {
                "info"
            },
            project: None,
            target: None,
            message: format!(
                "The store is at {}% of its size limit ({} of {}).",
                context.stored_bytes * 100 / max,
                crate::cache::store::human_size(context.stored_bytes),
                crate::cache::store::human_size(max)
            ),
            action: "Retention will start evicting the least recently used entries. Raise \
                     `retention.max_size` if those are still wanted."
                .to_string(),
        });
    }

    if let Some(eviction) = &recorded.last_eviction
        && eviction.removed > 0
        && days_between(Some(&eviction.at), &crate::cache::store::now()).is_some_and(|d| d < 2)
    {
        out.push(Insight {
            severity: "info",
            project: None,
            target: None,
            message: format!(
                "The last retention sweep evicted {} entr{} ({}).",
                eviction.removed,
                if eviction.removed == 1 { "y" } else { "ies" },
                crate::cache::store::human_size(eviction.freed)
            ),
            action: format!(
                "Current policy: {}. If evicted builds are missed afterwards, loosen it.",
                context.retention
            ),
        });
    }

    // Problems before housekeeping.
    out.sort_by_key(|i| i.severity != "warn");
    out
}

/// Whole days from `from` to `to`, or `None` when `from` is absent or unparsable.
fn days_between(from: Option<&str>, to: &str) -> Option<i64> {
    let from = chrono::DateTime::parse_from_rfc3339(from?).ok()?;
    let to = chrono::DateTime::parse_from_rfc3339(to).ok()?;
    Some((to - from).num_days())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ciab_activity_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn traffic<'a>(kind: Kind, target: &'a str) -> Traffic<'a> {
        Traffic {
            project: "p1",
            kind,
            target: Some(target),
            key: Some("abcdef0123456789"),
            user: Some("ada"),
            bytes: 100,
        }
    }

    #[test]
    fn counts_survive_a_restart_with_their_timestamps() {
        let dir = scratch("persist");
        {
            let activity = Activity::open(&dir).unwrap();
            activity.record(traffic(Kind::Upload, "api:build"));
            activity.record(traffic(Kind::Hit, "api:build"));
            activity.record(traffic(Kind::Miss, "web:build"));
            activity.flush().unwrap();
        }

        let reopened = Activity::open(&dir).unwrap();
        let recorded = reopened.snapshot();
        let counters = &recorded.projects["p1"].counters;
        assert_eq!(
            (counters.hits, counters.misses, counters.uploads),
            (1, 1, 1)
        );

        let project = &recorded.projects["p1"];
        assert!(project.last_hit_at.is_some() && project.last_upload_at.is_some());
        assert!(project.last_used_at().is_some());
        assert_eq!(project.targets["web:build"].misses, 1);
        assert_eq!(recorded.recent.len(), 3);
        assert_eq!(recorded.recent[0].kind, "miss", "newest first");
        assert_eq!(recorded.recent[0].key.as_deref(), Some("abcdef012345"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_target_uploaded_again_and_again_but_never_hit_is_called_out() {
        let dir = scratch("unstable");
        let activity = Activity::open(&dir).unwrap();
        for _ in 0..5 {
            activity.record(traffic(Kind::Miss, "api:build"));
            activity.record(traffic(Kind::Upload, "api:build"));
        }
        let names = [("p1".to_string(), "monorepo".to_string())]
            .into_iter()
            .collect();
        let found = insights(
            &activity.snapshot(),
            &InsightContext {
                names: &names,
                stored_bytes: 0,
                max_bytes: None,
                retention: "",
            },
        );
        let insight = found
            .iter()
            .find(|i| i.target.as_deref() == Some("api:build"))
            .expect("an unstable key is the thing to report");
        assert!(insight.message.contains("never reused"));
        assert_eq!(insight.project.as_deref(), Some("monorepo"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_nearly_full_store_is_reported() {
        let names = BTreeMap::new();
        let found = insights(
            &Recorded::default(),
            &InsightContext {
                names: &names,
                stored_bytes: 90,
                max_bytes: Some(100),
                retention: "max size 100 B",
            },
        );
        assert!(found[0].message.contains("90%"), "{found:?}");
    }
}
