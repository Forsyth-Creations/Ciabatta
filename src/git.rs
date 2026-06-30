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
