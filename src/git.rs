use anyhow::{Context, Result, bail};
use std::collections::HashMap;
use std::path::Path;
use std::process::Command;

/// Resolve the CIABATTA_* build variables from the local git repository rooted
/// at (or containing) `root`. Used by `--local` and `ciabatta source` so the
/// same recipes work on a developer machine without a CI system.
///
///   CIABATTA_BRANCH        current branch (omitted when HEAD is detached)
///   CIABATTA_COMMIT        full HEAD commit SHA
///   CIABATTA_TAG           tag pointing exactly at HEAD, if any
///   CIABATTA_BUILD_NUMBER  commit count reachable from HEAD (a local stand-in)
pub fn local_git_vars(root: &Path) -> Result<HashMap<String, String>> {
    if run_git(root, &["rev-parse", "--is-inside-work-tree"]).is_err() {
        bail!(
            "'{}' is not inside a git repository; local mode needs local git history.",
            root.display()
        );
    }

    let mut vars = HashMap::new();

    // Branch: skip the literal "HEAD" you get in a detached-HEAD checkout.
    if let Ok(branch) = run_git(root, &["rev-parse", "--abbrev-ref", "HEAD"])
        && !branch.is_empty()
        && branch != "HEAD"
    {
        vars.insert("CIABATTA_BRANCH".to_string(), branch);
    }

    if let Ok(commit) = run_git(root, &["rev-parse", "HEAD"])
        && !commit.is_empty()
    {
        vars.insert("CIABATTA_COMMIT".to_string(), commit);
    }

    // Only present when HEAD is exactly a tag (the command fails otherwise).
    if let Ok(tag) = run_git(root, &["describe", "--tags", "--exact-match"])
        && !tag.is_empty()
    {
        vars.insert("CIABATTA_TAG".to_string(), tag);
    }

    if let Ok(count) = run_git(root, &["rev-list", "--count", "HEAD"])
        && !count.is_empty()
    {
        vars.insert("CIABATTA_BUILD_NUMBER".to_string(), count);
    }

    Ok(vars)
}

/// List up to `limit` commit SHAs reachable from `refname` (a branch, tag, or
/// commit), most recent first. Used by the pull best-hash fallback to walk a
/// branch's history looking for the newest commit that has a published artifact.
pub fn branch_commits(root: &Path, refname: &str, limit: usize) -> Result<Vec<String>> {
    let max = limit.to_string();
    let out = run_git(root, &["rev-list", "--max-count", &max, refname])?;
    Ok(out
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect())
}

/// The well-known SHA of git's empty tree, used as a diff base when the repo is
/// younger than the requested window (so we can still show the full change set).
const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

/// Summarize git activity over the past `days` days as plain text: the commit
/// list, the committed diffstat for the window, and any uncommitted changes.
/// Used by `ciabatta ai report` and the TUI `/report` command to give the
/// assistant something to explain.
pub fn changes_since(root: &Path, days: u64) -> Result<String> {
    if run_git(root, &["rev-parse", "--is-inside-work-tree"]).is_err() {
        bail!(
            "'{}' is not inside a git repository — a report needs local git history.",
            root.display()
        );
    }
    let since = format!("--since={days} days ago");
    let before = format!("--before={days} days ago");

    // Commit list within the window (short hash, date, subject, author).
    let log = run_git(
        root,
        &[
            "log",
            &since,
            "--pretty=format:%h %ad %s (%an)",
            "--date=short",
        ],
    )
    .unwrap_or_default();

    // Committed changes: diff from the last commit *before* the window to HEAD.
    // If nothing predates the window, diff from the empty tree (full history).
    let base = run_git(root, &["rev-list", "-1", &before, "HEAD"]).unwrap_or_default();
    let base = if base.is_empty() {
        EMPTY_TREE.to_string()
    } else {
        base
    };
    let stat = run_git(root, &["diff", "--stat", &base, "HEAD"]).unwrap_or_default();

    // Uncommitted working-tree changes (staged + unstaged) relative to HEAD.
    let uncommitted = run_git(root, &["diff", "--stat", "HEAD"]).unwrap_or_default();

    if log.is_empty() && stat.is_empty() && uncommitted.is_empty() {
        return Ok(format!("No git activity in the past {days} day(s)."));
    }

    let mut out = String::new();
    out.push_str(&format!("Commits (past {days} day(s)):\n"));
    out.push_str(if log.is_empty() { "(none)" } else { &log });
    out.push_str("\n\nFiles changed (committed in window):\n");
    out.push_str(if stat.is_empty() { "(none)" } else { &stat });
    if !uncommitted.is_empty() {
        out.push_str("\n\nUncommitted working-tree changes:\n");
        out.push_str(&uncommitted);
    }
    Ok(clip(&out, 16_000))
}

/// Cap report text so an enormous diffstat can't blow up the model's context.
fn clip(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut cut = max;
    while !s.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{}\n… [truncated]", &s[..cut])
}

/// Run `git <args>` in `root`, returning trimmed stdout on success.
fn run_git(root: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .current_dir(root)
        .args(args)
        .output()
        .context("Failed to run git (is it installed and on PATH?)")?;
    if !output.status.success() {
        bail!("git {} failed", args.join(" "));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_repo() -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!(
            "ciabatta-git-test-{}-{:?}",
            std::process::id(),
            std::thread::current().id(),
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        // A self-contained repo with a committed identity, so the test doesn't
        // depend on the machine's global git config.
        run_git(&root, &["init", "-q"]).unwrap();
        run_git(&root, &["config", "user.email", "t@t.test"]).unwrap();
        run_git(&root, &["config", "user.name", "Tester"]).unwrap();
        run_git(&root, &["config", "commit.gpgsign", "false"]).unwrap();
        root
    }

    #[test]
    fn changes_since_reports_commits_files_and_uncommitted() {
        let root = temp_repo();

        std::fs::write(root.join("hello.txt"), "hi\n").unwrap();
        run_git(&root, &["add", "hello.txt"]).unwrap();
        run_git(&root, &["commit", "-q", "-m", "add greeting"]).unwrap();

        // A wide window captures the just-made commit.
        let report = changes_since(&root, 3650).unwrap();
        assert!(
            report.contains("add greeting"),
            "commit subject missing:\n{report}"
        );
        assert!(
            report.contains("hello.txt"),
            "changed file missing:\n{report}"
        );

        // Uncommitted edits show up in their own section.
        std::fs::write(root.join("hello.txt"), "hi there\n").unwrap();
        let report = changes_since(&root, 3650).unwrap();
        assert!(
            report.contains("Uncommitted"),
            "uncommitted section missing:\n{report}"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn changes_since_errors_outside_a_repo() {
        let dir = std::env::temp_dir().join(format!("ciabatta-nongit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        assert!(changes_since(&dir, 7).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
