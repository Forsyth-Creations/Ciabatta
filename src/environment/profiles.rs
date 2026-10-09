//! Env profiles: named sets of variables a run can be started with.
//!
//! A profile is a file next to a `.env`, named after it: `.env.staging`,
//! `.env.ci`, `packages/api/.env.staging`. Running with `--env-profile staging`
//! sources each `.env` the run already reads and then its `.staging` sibling on
//! top — so a profile only has to say what's *different*, and everything it
//! doesn't mention falls back to the ordinary `.env` (and from there outward,
//! exactly as without a profile).
//!
//! No configuration declares them. A file sitting there is the declaration,
//! the same way a `.env.example` is the template without being named in the
//! config: `ciabatta env list` finds them by looking.
//!
//! The profile in force travels with the run as `CIABATTA_ENV_PROFILE`, so it
//! reaches a run started from the browser as well as one started here, and a
//! step can read which profile it's under.

use std::collections::BTreeMap;
use std::path::Path;

/// The variable a run's profile travels in.
pub const PROFILE_VAR: &str = "CIABATTA_ENV_PROFILE";

/// Suffixes that mark a template rather than a profile — `.env.example` is
/// what a `.env` is generated from, not a set of values to run with.
const NOT_PROFILES: &[&str] = &["default", "example", "sample", "template", "tmp", "bak"];

/// One profile, and every file in the workspace that belongs to it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Profile {
    pub name: String,
    /// The profile's files, relative to the workspace root, outermost first.
    pub files: Vec<String>,
    /// How many variables they set between them.
    pub vars: usize,
}

/// Whether `name` is usable as a profile name: a plain word, so it can't
/// reach outside the directory it's joined onto.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// The overlay for one env file: `.env` → `.env.staging`.
pub fn overlay(file: &str, profile: &str) -> String {
    format!("{file}.{profile}")
}

/// Every profile under `root`, found in `dirs` (workspace directories,
/// relative to the root — `.` for the root itself).
pub fn list(root: &Path, dirs: &[String]) -> Vec<Profile> {
    let mut found: BTreeMap<String, Profile> = BTreeMap::new();
    let mut dirs: Vec<&String> = dirs.iter().collect();
    // Outermost first, so a profile's files list in the order they apply.
    dirs.sort_by_key(|d| (d.matches('/').count(), (*d).clone()));
    dirs.dedup();

    for rel in dirs {
        let dir = if rel == "." {
            root.to_path_buf()
        } else {
            root.join(rel)
        };
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut names: Vec<String> = entries
            .flatten()
            .filter(|e| e.path().is_file())
            .filter_map(|e| e.file_name().to_str().map(str::to_string))
            .collect();
        names.sort();
        for file in names {
            let Some(name) = file.strip_prefix(".env.") else {
                continue;
            };
            // `.env.staging.example` is staging's template; anything dotted
            // further is something other than a profile.
            if !valid_name(name) || NOT_PROFILES.contains(&name) {
                continue;
            }
            let path = super::files::join_rel(rel, &file);
            let vars = std::fs::read_to_string(dir.join(&file))
                .map(|body| {
                    body.lines()
                        .map(str::trim)
                        .filter(|l| !l.is_empty() && !l.starts_with('#') && l.contains('='))
                        .count()
                })
                .unwrap_or(0);
            let entry = found.entry(name.to_string()).or_insert_with(|| Profile {
                name: name.to_string(),
                files: Vec::new(),
                vars: 0,
            });
            entry.files.push(path);
            entry.vars += vars;
        }
    }
    found.into_values().collect()
}

/// The workspace directories a project's profiles live in: the root, and
/// every sub-workspace.
pub fn workspace_dirs(root: &Path) -> Vec<String> {
    let mut dirs = vec![".".to_string()];
    if let Ok(ws) = crate::workspace::Workspace::load(root) {
        for member in &ws.members {
            // The root is often a member too.
            if !dirs.contains(&member.rel) {
                dirs.push(member.rel.clone());
            }
        }
    }
    dirs
}

/// Check a requested profile exists, with the list when it doesn't.
pub fn require(root: &Path, name: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        valid_name(name),
        "'{name}' isn't a usable profile name — use letters, digits, '-' and '_'."
    );
    let available = list(root, &workspace_dirs(root));
    if available.iter().any(|p| p.name == name) {
        return Ok(());
    }
    let names: Vec<&str> = available.iter().map(|p| p.name.as_str()).collect();
    anyhow::bail!(
        "No env profile '{name}': there is no .env.{name} in this workspace.\n{}",
        if names.is_empty() {
            format!(
                "Create one next to your .env — `.env.{name}`, holding just the variables \
                 that differ — and run again."
            )
        } else {
            format!(
                "Profiles here: {}. (`ciabatta env list` shows their files.)",
                names.join(", ")
            )
        }
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profiles_are_found_next_to_every_env_and_templates_are_not_profiles() {
        let root = std::env::temp_dir().join(format!("ciab_profiles_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("packages/api")).unwrap();
        std::fs::write(root.join(".env"), "A=1\n").unwrap();
        std::fs::write(root.join(".env.staging"), "# staging\nA=2\nB=3\n").unwrap();
        std::fs::write(root.join(".env.example"), "A=\n").unwrap();
        std::fs::write(root.join(".env.staging.example"), "A=\n").unwrap();
        std::fs::write(root.join("packages/api/.env.staging"), "C=4\n").unwrap();
        std::fs::write(root.join("packages/api/.env.ci"), "D=5\n").unwrap();

        let found = list(&root, &[".".to_string(), "packages/api".to_string()]);
        let names: Vec<&str> = found.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["ci", "staging"]);
        let staging = &found[1];
        assert_eq!(staging.files, [".env.staging", "packages/api/.env.staging"]);
        assert_eq!(staging.vars, 3);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_profile_name_cannot_walk_out_of_its_directory() {
        assert!(valid_name("staging-eu_1"));
        assert!(!valid_name("../secrets"));
        assert!(!valid_name("a/b"));
        assert!(!valid_name(""));
    }
}
