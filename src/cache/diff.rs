//! Why a build didn't hit the cache, in enough detail to believe the answer.
//!
//! "Cache miss" is not a useful thing to tell somebody. What they need is the
//! same thing a pull request shows them: *these files changed, and here are the
//! lines*. Until a cache can show that, people don't trust it — and a build
//! cache nobody trusts gets turned off, which is worse than not having one.
//!
//! A step has three dependencies, so a miss has three possible explanations and
//! this module reports all three:
//!
//! * **input files** — added, removed, or edited, with line-level hunks;
//! * **environment variables** — the declared ones, and what they were before;
//! * **upstream stages** — a step this one needs produced something different.
//!
//! To show lines rather than just filenames, the previous run's *text* inputs
//! are snapshotted alongside the entry. That's a deliberate, bounded cost: text
//! only, capped per file, and kept in the local store rather than uploaded — the
//! diff is a debugging aid for the person at the keyboard, not something a CI
//! runner needs to pull over the network.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use super::FileHash;

/// Per-file cap on what gets snapshotted for diffing.
///
/// A source file over a quarter of a megabyte is either generated or not really
/// source, and in both cases nobody is going to read its diff.
pub const MAX_SNAPSHOT_BYTES: u64 = 256 * 1024;

/// Directory inside the store holding the snapshots.
pub const SNAPSHOT_DIR: &str = "snapshots";

/// What happened to one file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeKind {
    Added,
    Removed,
    Modified,
}

/// One line of a rendered diff.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "op")]
pub enum Line {
    /// Unchanged, shown for context.
    Context {
        old: usize,
        new: usize,
        text: String,
    },
    Added {
        new: usize,
        text: String,
    },
    Removed {
        old: usize,
        text: String,
    },
}

/// A contiguous run of changes plus the context around it — a `@@` block.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hunk {
    pub old_start: usize,
    pub old_lines: usize,
    pub new_start: usize,
    pub new_lines: usize,
    pub lines: Vec<Line>,
}

impl Hunk {
    /// The `@@ -a,b +c,d @@` header, for a terminal rendering.
    pub fn header(&self) -> String {
        format!(
            "@@ -{},{} +{},{} @@",
            self.old_start, self.old_lines, self.new_start, self.new_lines
        )
    }
}

/// One changed input file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileDiff {
    pub path: String,
    pub kind: ChangeKind,
    /// Lines added, across every hunk.
    pub additions: usize,
    /// Lines removed.
    pub deletions: usize,
    /// The hunks, when the file could be diffed line by line.
    #[serde(default)]
    pub hunks: Vec<Hunk>,
    /// Why there are no hunks, when there aren't — binary, too large, or not
    /// snapshotted by the previous run.
    #[serde(default)]
    pub note: Option<String>,
}

/// One declared environment variable that moved.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvDiff {
    pub name: String,
    pub kind: ChangeKind,
    /// What it was. Absent when the variable is new.
    #[serde(default)]
    pub before: Option<String>,
    /// What it is now. Absent when the variable has gone.
    #[serde(default)]
    pub after: Option<String>,
}

/// One upstream stage whose output changed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpstreamDiff {
    /// The step this one depends on.
    pub step: String,
    pub kind: ChangeKind,
    #[serde(default)]
    pub before: Option<String>,
    #[serde(default)]
    pub after: Option<String>,
}

/// Everything that changed between two runs of a step.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Diff {
    /// The key of the run being compared against, when there was one.
    #[serde(default)]
    pub previous_key: Option<String>,
    /// When that run happened.
    #[serde(default)]
    pub previous_at: Option<String>,
    #[serde(default)]
    pub files: Vec<FileDiff>,
    #[serde(default)]
    pub env: Vec<EnvDiff>,
    #[serde(default)]
    pub upstream: Vec<UpstreamDiff>,
}

impl Diff {
    /// Whether anything moved at all.
    pub fn is_empty(&self) -> bool {
        self.files.is_empty() && self.env.is_empty() && self.upstream.is_empty()
    }

    /// Total lines added and removed across every file.
    pub fn totals(&self) -> (usize, usize) {
        self.files
            .iter()
            .fold((0, 0), |(a, d), f| (a + f.additions, d + f.deletions))
    }

    /// A one-line summary, for the terminal.
    pub fn summary(&self) -> String {
        if self.is_empty() {
            return "nothing changed".to_string();
        }
        let mut parts: Vec<String> = Vec::new();
        if !self.files.is_empty() {
            let (added, removed) = self.totals();
            parts.push(format!(
                "{} file(s) (+{added} −{removed})",
                self.files.len()
            ));
        }
        if !self.env.is_empty() {
            parts.push(format!("{} environment variable(s)", self.env.len()));
        }
        if !self.upstream.is_empty() {
            parts.push(format!("{} upstream stage(s)", self.upstream.len()));
        }
        parts.join(", ")
    }
}

// ─── Building one ───────────────────────────────────────────────────────────

/// Compare a previous run against the current state of a workspace.
///
/// `snapshots` is where the previous run's text inputs were kept; a file with
/// no snapshot still shows up as modified, just without its lines.
pub fn compute(
    previous: &super::store::Entry,
    dir: &Path,
    snapshots: &Path,
    current_inputs: &[FileHash],
    current_env: &BTreeMap<String, String>,
    current_upstream: &BTreeMap<String, String>,
) -> Result<Diff> {
    let before: BTreeMap<&str, &FileHash> = previous
        .inputs
        .iter()
        .map(|f| (f.path.as_str(), f))
        .collect();
    let after: BTreeMap<&str, &FileHash> = current_inputs
        .iter()
        .map(|f| (f.path.as_str(), f))
        .collect();

    let mut files: Vec<FileDiff> = Vec::new();

    for (path, current) in &after {
        match before.get(path) {
            None => files.push(file_diff(
                path,
                ChangeKind::Added,
                None,
                read_text(&dir.join(path), current.size),
            )),
            Some(previous) if previous.sha256 != current.sha256 => files.push(file_diff(
                path,
                ChangeKind::Modified,
                read_text(&snapshots.join(path), MAX_SNAPSHOT_BYTES),
                read_text(&dir.join(path), current.size),
            )),
            Some(_) => {}
        }
    }

    for (path, previous) in &before {
        if !after.contains_key(path) {
            files.push(file_diff(
                path,
                ChangeKind::Removed,
                read_text(&snapshots.join(path), previous.size.max(MAX_SNAPSHOT_BYTES)),
                None,
            ));
        }
    }

    files.sort_by(|a, b| a.path.cmp(&b.path));

    Ok(Diff {
        previous_key: Some(previous.key.clone()),
        previous_at: Some(previous.created_at.clone()),
        files,
        env: map_diff(&previous.env, current_env)
            .into_iter()
            .map(|(name, kind, before, after)| EnvDiff {
                name,
                kind,
                before,
                after,
            })
            .collect(),
        upstream: map_diff(&previous.upstream, current_upstream)
            .into_iter()
            .map(|(step, kind, before, after)| UpstreamDiff {
                step,
                kind,
                before,
                after,
            })
            .collect(),
    })
}

/// Compare two string maps, reporting additions, removals, and changes in key
/// order. Shared by the environment and upstream views, which have the same
/// shape and want the same answer.
fn map_diff(
    before: &BTreeMap<String, String>,
    after: &BTreeMap<String, String>,
) -> Vec<(String, ChangeKind, Option<String>, Option<String>)> {
    let mut out = Vec::new();

    for (key, value) in after {
        match before.get(key) {
            None => out.push((key.clone(), ChangeKind::Added, None, Some(value.clone()))),
            Some(old) if old != value => out.push((
                key.clone(),
                ChangeKind::Modified,
                Some(old.clone()),
                Some(value.clone()),
            )),
            Some(_) => {}
        }
    }
    for (key, value) in before {
        if !after.contains_key(key) {
            out.push((key.clone(), ChangeKind::Removed, Some(value.clone()), None));
        }
    }

    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// Build one file's diff, falling back to a note when there are no lines to
/// show.
fn file_diff(
    path: &str,
    kind: ChangeKind,
    before: Option<String>,
    after: Option<String>,
) -> FileDiff {
    let (hunks, note) = match (&before, &after) {
        (Some(before), Some(after)) => (hunks(before, after), None),
        (None, Some(after)) if kind == ChangeKind::Added => (hunks("", after), None),
        (Some(before), None) if kind == ChangeKind::Removed => (hunks(before, ""), None),
        _ => (
            Vec::new(),
            Some(
                "no line-by-line view: the file is binary, too large to snapshot, \
                 or predates this cache entry"
                    .to_string(),
            ),
        ),
    };

    let additions = hunks
        .iter()
        .flat_map(|h| &h.lines)
        .filter(|l| matches!(l, Line::Added { .. }))
        .count();
    let deletions = hunks
        .iter()
        .flat_map(|h| &h.lines)
        .filter(|l| matches!(l, Line::Removed { .. }))
        .count();

    FileDiff {
        path: path.to_string(),
        kind,
        additions,
        deletions,
        hunks,
        note,
    }
}

/// Read a file as text, or `None` if it's binary, absent, or too big.
///
/// "Binary" is decided by whether it's valid UTF-8 containing no NUL — the same
/// heuristic git uses, and for the same reason: it's cheap and it's right about
/// every file anyone actually wants to diff.
fn read_text(path: &Path, size_hint: u64) -> Option<String> {
    if size_hint > MAX_SNAPSHOT_BYTES {
        return None;
    }
    let bytes = std::fs::read(path).ok()?;
    if bytes.len() as u64 > MAX_SNAPSHOT_BYTES || bytes.contains(&0) {
        return None;
    }
    String::from_utf8(bytes).ok()
}

// ─── Line diffing ───────────────────────────────────────────────────────────

/// How many unchanged lines to show either side of a change.
const CONTEXT: usize = 3;

/// Diff two texts into hunks.
pub fn hunks(before: &str, after: &str) -> Vec<Hunk> {
    let old: Vec<&str> = before.lines().collect();
    let new: Vec<&str> = after.lines().collect();
    let script = lcs_script(&old, &new);

    // Walk the edit script, collecting runs of changes with `CONTEXT` unchanged
    // lines either side, and merging runs that overlap.
    let mut hunks: Vec<Hunk> = Vec::new();
    let mut index = 0;

    while index < script.len() {
        if matches!(script[index], Line::Context { .. }) {
            index += 1;
            continue;
        }

        // Back up over the leading context.
        let start = index.saturating_sub(CONTEXT);

        // Find the end of this run: the last change, plus trailing context. A
        // gap of up to 2*CONTEXT unchanged lines keeps two nearby changes in one
        // hunk rather than producing two hunks that would overlap.
        let mut end = index;
        let mut run = index;
        while run < script.len() {
            if matches!(script[run], Line::Context { .. }) {
                if run - end > CONTEXT * 2 {
                    break;
                }
            } else {
                end = run;
            }
            run += 1;
        }
        let end = (end + CONTEXT + 1).min(script.len());

        hunks.push(build_hunk(&script[start..end]));
        index = end;
    }

    hunks
}

/// Assemble a hunk from a slice of the edit script, computing its `@@` ranges.
fn build_hunk(lines: &[Line]) -> Hunk {
    let mut old_start = 0;
    let mut new_start = 0;
    let mut old_lines = 0;
    let mut new_lines = 0;

    for line in lines {
        match line {
            Line::Context { old, new, .. } => {
                if old_start == 0 {
                    old_start = *old;
                }
                if new_start == 0 {
                    new_start = *new;
                }
                old_lines += 1;
                new_lines += 1;
            }
            Line::Added { new, .. } => {
                if new_start == 0 {
                    new_start = *new;
                }
                new_lines += 1;
            }
            Line::Removed { old, .. } => {
                if old_start == 0 {
                    old_start = *old;
                }
                old_lines += 1;
            }
        }
    }

    Hunk {
        old_start,
        old_lines,
        new_start,
        new_lines,
        lines: lines.to_vec(),
    }
}

/// The full edit script between two line sequences, via a longest-common-
/// subsequence table.
///
/// Quadratic in the number of lines, which is fine for what this diffs: source
/// files capped at [`MAX_SNAPSHOT_BYTES`]. A Myers implementation would be
/// faster and considerably harder to read, and nothing here is on a hot path —
/// this runs once, when somebody asks why their build missed.
fn lcs_script(old: &[&str], new: &[&str]) -> Vec<Line> {
    let (rows, cols) = (old.len(), new.len());

    // table[i][j] = length of the LCS of old[i..] and new[j..].
    let mut table = vec![vec![0usize; cols + 1]; rows + 1];
    for i in (0..rows).rev() {
        for j in (0..cols).rev() {
            table[i][j] = if old[i] == new[j] {
                table[i + 1][j + 1] + 1
            } else {
                table[i + 1][j].max(table[i][j + 1])
            };
        }
    }

    let mut script = Vec::new();
    let (mut i, mut j) = (0, 0);
    while i < rows && j < cols {
        if old[i] == new[j] {
            script.push(Line::Context {
                old: i + 1,
                new: j + 1,
                text: old[i].to_string(),
            });
            i += 1;
            j += 1;
        } else if table[i + 1][j] >= table[i][j + 1] {
            script.push(Line::Removed {
                old: i + 1,
                text: old[i].to_string(),
            });
            i += 1;
        } else {
            script.push(Line::Added {
                new: j + 1,
                text: new[j].to_string(),
            });
            j += 1;
        }
    }
    while i < rows {
        script.push(Line::Removed {
            old: i + 1,
            text: old[i].to_string(),
        });
        i += 1;
    }
    while j < cols {
        script.push(Line::Added {
            new: j + 1,
            text: new[j].to_string(),
        });
        j += 1;
    }

    script
}

/// Render a diff the way `git diff` would, for the terminal.
pub fn render(diff: &Diff) -> String {
    let mut out = String::new();

    for file in &diff.files {
        let marker = match file.kind {
            ChangeKind::Added => "new file",
            ChangeKind::Removed => "deleted",
            ChangeKind::Modified => "modified",
        };
        out.push_str(&format!(
            "{} {}  (+{} −{})\n",
            marker, file.path, file.additions, file.deletions
        ));

        if let Some(note) = &file.note {
            out.push_str(&format!("    {note}\n"));
            continue;
        }
        for hunk in &file.hunks {
            out.push_str(&format!("  {}\n", hunk.header()));
            for line in &hunk.lines {
                match line {
                    Line::Context { text, .. } => out.push_str(&format!("   {text}\n")),
                    Line::Added { text, .. } => out.push_str(&format!("  +{text}\n")),
                    Line::Removed { text, .. } => out.push_str(&format!("  -{text}\n")),
                }
            }
        }
    }

    for env in &diff.env {
        match env.kind {
            ChangeKind::Added => out.push_str(&format!(
                "env {} is new (= {})\n",
                env.name,
                env.after.as_deref().unwrap_or("")
            )),
            ChangeKind::Removed => out.push_str(&format!("env {} is gone\n", env.name)),
            ChangeKind::Modified => out.push_str(&format!(
                "env {}: {} → {}\n",
                env.name,
                env.before.as_deref().unwrap_or(""),
                env.after.as_deref().unwrap_or("")
            )),
        }
    }

    for upstream in &diff.upstream {
        match upstream.kind {
            ChangeKind::Added => {
                out.push_str(&format!("stage {} is a new dependency\n", upstream.step))
            }
            ChangeKind::Removed => out.push_str(&format!(
                "stage {} is no longer a dependency\n",
                upstream.step
            )),
            ChangeKind::Modified => out.push_str(&format!(
                "stage {} produced different output\n",
                upstream.step
            )),
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_edit_in_the_middle_produces_one_hunk_with_context() {
        let before = "one\ntwo\nthree\nfour\nfive\nsix\nseven\neight\nnine\nten\n";
        let after = "one\ntwo\nthree\nfour\nFIVE\nsix\nseven\neight\nnine\nten\n";

        let hunks = hunks(before, after);
        assert_eq!(hunks.len(), 1);

        let hunk = &hunks[0];
        // Three lines of context either side of the one changed line.
        assert_eq!(hunk.lines.len(), 3 + 2 + 3);
        assert!(hunk.lines.iter().any(|l| matches!(
            l,
            Line::Removed { text, .. } if text == "five"
        )));
        assert!(hunk.lines.iter().any(|l| matches!(
            l,
            Line::Added { text, .. } if text == "FIVE"
        )));
        assert_eq!(hunk.header(), "@@ -2,7 +2,7 @@");
    }

    #[test]
    fn distant_changes_get_their_own_hunks() {
        let before: String = (1..=40).map(|n| format!("line {n}\n")).collect();
        let mut lines: Vec<String> = (1..=40).map(|n| format!("line {n}")).collect();
        lines[2] = "CHANGED near the top".into();
        lines[35] = "CHANGED near the bottom".into();
        let after: String = lines.join("\n") + "\n";

        let hunks = hunks(&before, &after);
        assert_eq!(
            hunks.len(),
            2,
            "changes 30 lines apart should not be merged into one hunk"
        );
        assert!(hunks[0].old_start < hunks[1].old_start);
    }

    #[test]
    fn additions_and_deletions_are_counted_and_whole_files_handled() {
        // A brand-new file is all additions.
        let added = file_diff("new.rs", ChangeKind::Added, None, Some("a\nb\nc\n".into()));
        assert_eq!(added.additions, 3);
        assert_eq!(added.deletions, 0);
        assert!(added.note.is_none());

        // A deleted file is all deletions.
        let removed = file_diff("old.rs", ChangeKind::Removed, Some("a\nb\n".into()), None);
        assert_eq!(removed.additions, 0);
        assert_eq!(removed.deletions, 2);

        // A modified file with no snapshot still reports the change, with a
        // note instead of lines — silently dropping it would be worse.
        let opaque = file_diff("app.bin", ChangeKind::Modified, None, None);
        assert!(opaque.hunks.is_empty());
        assert!(opaque.note.as_deref().unwrap().contains("binary"));
    }

    #[test]
    fn environment_and_upstream_changes_are_reported_in_key_order() {
        let before = BTreeMap::from([
            ("PROFILE".to_string(), "debug".to_string()),
            ("STALE".to_string(), "yes".to_string()),
        ]);
        let after = BTreeMap::from([
            ("NEW".to_string(), "1".to_string()),
            ("PROFILE".to_string(), "release".to_string()),
        ]);

        let changes = map_diff(&before, &after);
        assert_eq!(changes.len(), 3);
        assert_eq!(changes[0].0, "NEW");
        assert_eq!(changes[0].1, ChangeKind::Added);
        assert_eq!(changes[1].0, "PROFILE");
        assert_eq!(changes[1].1, ChangeKind::Modified);
        assert_eq!(changes[1].2.as_deref(), Some("debug"));
        assert_eq!(changes[1].3.as_deref(), Some("release"));
        assert_eq!(changes[2].0, "STALE");
        assert_eq!(changes[2].1, ChangeKind::Removed);

        // Nothing moved → nothing reported.
        assert!(map_diff(&before, &before).is_empty());
    }

    #[test]
    fn a_diff_summarizes_all_three_kinds_of_dependency() {
        let diff = Diff {
            previous_key: Some("k".into()),
            previous_at: None,
            files: vec![FileDiff {
                path: "src/main.rs".into(),
                kind: ChangeKind::Modified,
                additions: 4,
                deletions: 2,
                hunks: vec![],
                note: None,
            }],
            env: vec![EnvDiff {
                name: "PROFILE".into(),
                kind: ChangeKind::Modified,
                before: Some("debug".into()),
                after: Some("release".into()),
            }],
            upstream: vec![UpstreamDiff {
                step: "proto:generate".into(),
                kind: ChangeKind::Modified,
                before: Some("aaa".into()),
                after: Some("bbb".into()),
            }],
        };

        assert!(!diff.is_empty());
        assert_eq!(diff.totals(), (4, 2));
        assert_eq!(
            diff.summary(),
            "1 file(s) (+4 −2), 1 environment variable(s), 1 upstream stage(s)"
        );

        let rendered = render(&diff);
        assert!(rendered.contains("modified src/main.rs"));
        assert!(rendered.contains("env PROFILE: debug → release"));
        assert!(rendered.contains("stage proto:generate produced different output"));

        assert!(Diff::default().is_empty());
        assert_eq!(Diff::default().summary(), "nothing changed");
    }

    #[test]
    fn identical_texts_produce_no_hunks() {
        assert!(hunks("same\ncontent\n", "same\ncontent\n").is_empty());
        assert!(hunks("", "").is_empty());
    }

    #[test]
    fn binary_and_oversized_files_are_never_read_as_text() {
        let dir = std::env::temp_dir().join(format!("ciab_diff_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        std::fs::write(dir.join("text.rs"), "fn main() {}\n").unwrap();
        std::fs::write(dir.join("binary.o"), [0x7f, 0x45, 0x00, 0x01]).unwrap();
        std::fs::write(
            dir.join("huge.txt"),
            "x".repeat(MAX_SNAPSHOT_BYTES as usize + 1),
        )
        .unwrap();

        assert_eq!(
            read_text(&dir.join("text.rs"), 13).as_deref(),
            Some("fn main() {}\n")
        );
        assert!(
            read_text(&dir.join("binary.o"), 4).is_none(),
            "a NUL means binary"
        );
        assert!(read_text(&dir.join("huge.txt"), MAX_SNAPSHOT_BYTES + 1).is_none());
        assert!(read_text(&dir.join("missing.rs"), 10).is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
