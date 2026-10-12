//! Env profiles: named sets of variables a workspace's `.env` can be switched
//! to.
//!
//! A profile is a file in a workspace's `.ciabatta/` folder: `.env.dev`,
//! `.env.prod`, `packages/api/.ciabatta/.env.staging`. Electing one — with
//! `ciabatta env use <name>`, or `--env-profile <name>` on a run — copies it
//! into the `.env` of every workspace that has it. The `.env` it replaces is
//! backed up first, unless it's an unedited copy of a profile, which can always
//! be made again.
//!
//! No configuration declares them. A file sitting there is the declaration,
//! the same way a `.env.example` is the template without being named in the
//! config: `ciabatta env list` finds them by looking.
//!
//! The profile a run was started with travels as `CIABATTA_ENV_PROFILE`, so it
//! reaches a run started from the browser as well as one started here, and a
//! step can read which profile it's under. While it's set, each profile file
//! stands in for the `.env` it replaces in the search — so a `--dry-run`, which
//! copies nothing, still reports the values the run would see.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use anyhow::{Context, Result};

use crate::config::CIABATTA_DIR;
use crate::workspace::WorkspaceMeta;

/// The variable a run's profile travels in.
pub const PROFILE_VAR: &str = "CIABATTA_ENV_PROFILE";

/// Where the profile in [`PROFILE_VAR`] was chosen, when ciabatta set it:
/// `flag` for `--env-profile`, `launcher` for the browser's run launcher.
/// Absent means it came from the terminal.
pub const PROFILE_FROM_VAR: &str = "CIABATTA_ENV_PROFILE_FROM";

/// The first line of a `.env` made from a profile. It says which, so the next
/// election can tell an unedited copy (safe to replace) from a file somebody
/// wrote (backed up first), and so the report can say which profile is in.
const HEADER: &str = "# ciabatta env profile: ";

/// Suffixes that mark a template or a backup rather than a profile.
const NOT_PROFILES: &[&str] = &["default", "example", "sample", "template", "tmp", "bak"];

/// The profile a run was started with: `CIABATTA_ENV_PROFILE`, when it names
/// one. While set, profile files stand in for the `.env`s they replace.
pub fn active(env: &HashMap<String, String>) -> Option<String> {
    env.get(PROFILE_VAR)
        .map(|p| p.trim().to_string())
        .filter(|p| valid_name(p))
}

/// The profile a run uses and why — or, when it uses none, why not. Shown
/// beside the profile wherever a run reports its environment, so nobody has to
/// guess which file a workspace's `.env` values came from.
pub fn choice(env: &HashMap<String, String>, root: &Path) -> (Option<String>, String) {
    match env
        .get(PROFILE_VAR)
        .map(|p| p.trim())
        .filter(|p| !p.is_empty())
    {
        Some(profile) if !valid_name(profile) => (
            None,
            format!("none — {PROFILE_VAR}={profile} isn't a valid profile name, so it was ignored"),
        ),
        Some(profile) => {
            let chosen = match env.get(PROFILE_FROM_VAR).map(String::as_str) {
                Some("flag") => format!("--env-profile {profile} on the command line"),
                Some("launcher") => "chosen in the run launcher".to_string(),
                _ => format!("{PROFILE_VAR}={profile} in the terminal"),
            };
            (
                Some(profile.to_string()),
                format!(
                    "{profile} — {chosen}, so each workspace's {CIABATTA_DIR}/.env.{profile} \
                     is copied into its .env"
                ),
            )
        }
        // Nothing asked for one on this run — but an earlier election may have
        // left some workspace's `.env` a copy of one, and that's worth saying.
        None => {
            let copies: Vec<(String, String, String)> = workspaces(root)
                .into_iter()
                .filter_map(|(rel, meta)| {
                    let file = crate::environment::files::join_rel(
                        &rel,
                        &crate::environment::files::target_file(&meta),
                    );
                    elected(&root.join(&file)).map(|(name, from)| (file, name, from))
                })
                .collect();
            let Some((_, first, _)) = copies.first() else {
                return (
                    None,
                    "none — no --env-profile given, and no .env is a copy of a profile".to_string(),
                );
            };
            let said: Vec<String> = copies
                .iter()
                .map(|(file, _, from)| format!("{file} is a copy of {from}"))
                .collect();
            let mut names: Vec<&str> = copies.iter().map(|(_, n, _)| n.as_str()).collect();
            names.dedup();
            (
                Some(names.join(", ")),
                format!(
                    "{} — {}, from an earlier `ciabatta env use {first}` (or --env-profile {first})",
                    names.join(", "),
                    said.join("; ")
                ),
            )
        }
    }
}

/// Whether `name` is usable as a profile name: a plain word, so it can't
/// reach outside the directory it's joined onto.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// A workspace's file for profile `name`, relative to the run root:
/// `packages/api` → `packages/api/.ciabatta/.env.staging`.
pub fn profile_file(rel: &str, name: &str) -> String {
    crate::environment::files::join_rel(rel, &format!("{CIABATTA_DIR}/.env.{name}"))
}

/// One profile, and every workspace's file that belongs to it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Profile {
    pub name: String,
    /// The profile's files, relative to the workspace root, outermost first.
    pub files: Vec<String>,
    /// How many variables they set between them.
    pub vars: usize,
}

/// Every profile under `root`, found in the `.ciabatta/` folder of each of
/// `dirs` (workspace directories, relative to the root — `.` for the root).
pub fn list(root: &Path, dirs: &[String]) -> Vec<Profile> {
    let mut found: BTreeMap<String, Profile> = BTreeMap::new();
    let mut dirs: Vec<&String> = dirs.iter().collect();
    // Outermost first, so a profile's files list root-first.
    dirs.sort_by_key(|d| (d.matches('/').count(), (*d).clone()));
    dirs.dedup();

    for rel in dirs {
        let dir = root.join(crate::environment::files::join_rel(rel, CIABATTA_DIR));
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
            let vars = std::fs::read_to_string(dir.join(&file))
                .map(|body| crate::run::parse_env_content(&body).len())
                .unwrap_or(0);
            let entry = found.entry(name.to_string()).or_insert_with(|| Profile {
                name: name.to_string(),
                files: Vec::new(),
                vars: 0,
            });
            entry.files.push(profile_file(rel, name));
            entry.vars += vars;
        }
    }
    found.into_values().collect()
}

/// The workspace directories a project's profiles live in: the root, and
/// every sub-workspace.
pub fn workspace_dirs(root: &Path) -> Vec<String> {
    workspaces(root).into_iter().map(|(rel, _)| rel).collect()
}

/// Every workspace, with its config: the root first, then each member.
fn workspaces(root: &Path) -> Vec<(String, WorkspaceMeta)> {
    let root_meta = crate::config::load_config(root)
        .ok()
        .and_then(|c| c.workspace)
        .unwrap_or_default();
    let mut all = vec![(".".to_string(), root_meta)];
    if let Ok(ws) = crate::workspace::Workspace::load(root) {
        all[0].1 = ws.root_meta.clone();
        for member in &ws.members {
            // The root is often a member too.
            if !all.iter().any(|(rel, _)| *rel == member.rel) {
                all.push((member.rel.clone(), member.meta.clone()));
            }
        }
    }
    all
}

/// Check a requested profile exists, with the list when it doesn't.
pub fn require(root: &Path, name: &str) -> Result<()> {
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
        "No env profile '{name}': no workspace has a {CIABATTA_DIR}/.env.{name}.\n{}",
        if names.is_empty() {
            format!(
                "Create one — `{CIABATTA_DIR}/.env.{name}`, holding the whole .env for that \
                 profile — and run again."
            )
        } else {
            format!(
                "Profiles here: {}. (`ciabatta env list` shows their files.)",
                names.join(", ")
            )
        }
    )
}

/// The profile a `.env` was copied from, and the file it came from — or
/// `None` when it wasn't made by electing one.
pub fn elected(env_file: &Path) -> Option<(String, String)> {
    let content = std::fs::read_to_string(env_file).ok()?;
    let rest = content.lines().next()?.strip_prefix(HEADER)?;
    let (name, from) = rest.split_once(" from ")?;
    Some((name.trim().to_string(), from.trim().to_string()))
}

/// The `.env` a profile's file becomes: the header saying where it came from,
/// then the profile, verbatim.
fn copy_of(name: &str, from: &str, body: &str) -> String {
    format!(
        "{HEADER}{name} from {from}\n\
         # Copied by ciabatta. Edit {from} instead: electing the profile again replaces this\n\
         # file. A .env somebody had written was backed up beside it first.\n\
         {body}"
    )
}

/// What electing a profile did to one workspace's `.env`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Applied {
    /// The `.env` written, relative to the run root.
    pub env_file: String,
    /// The profile file it was copied from.
    pub from: String,
    /// Where the `.env` it replaced was saved, when it needed saving.
    pub backup: Option<String>,
    /// Whether that backup is outside git's ignore rules — `.env` files hold
    /// secrets, and a backup of one is as sensitive as the original.
    pub backup_unignored: bool,
    /// The `.env` already was this copy: nothing written.
    pub unchanged: bool,
}

/// Elect profile `name`: copy each workspace's `.ciabatta/.env.<name>` into
/// that workspace's `.env`, backing up a `.env` that isn't an unedited copy of
/// a profile. With `dry_run`, works out what would happen and writes nothing.
pub fn apply(root: &Path, name: &str, dry_run: bool) -> Result<Vec<Applied>> {
    require(root, name)?;
    let mut applied = Vec::new();
    for (rel, meta) in workspaces(root) {
        let from = profile_file(&rel, name);
        let Ok(body) = std::fs::read_to_string(root.join(&from)) else {
            continue;
        };
        let env_file = crate::environment::files::join_rel(
            &rel,
            &crate::environment::files::target_file(&meta),
        );
        let path = root.join(&env_file);
        let wanted = copy_of(name, &from, &body);
        let current = std::fs::read_to_string(&path).ok();

        if current.as_deref() == Some(wanted.as_str()) {
            applied.push(Applied {
                env_file,
                from,
                backup: None,
                backup_unignored: false,
                unchanged: true,
            });
            continue;
        }

        let backup = match &current {
            Some(content) if !pristine(root, content) => Some(free_backup(root, &env_file)),
            _ => None,
        };
        if !dry_run {
            if let Some(backup) = &backup {
                std::fs::copy(&path, root.join(backup))
                    .with_context(|| format!("Failed to back up {} to {backup}", path.display()))?;
            }
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&path, &wanted)
                .with_context(|| format!("Failed to write {}", path.display()))?;
        }
        applied.push(Applied {
            backup_unignored: backup.as_deref().is_some_and(|b| !git_ignores(root, b)),
            env_file,
            from,
            backup,
            unchanged: false,
        });
    }
    Ok(applied)
}

/// One line saying what electing a profile did to a workspace's `.env` — or,
/// in a dry run, would do. `None` when it was already that copy.
pub fn describe(done: &Applied, dry_run: bool) -> Option<String> {
    if done.unchanged {
        return None;
    }
    let verb = match dry_run {
        true => "would copy",
        false => "copied",
    };
    let mut line = format!("env profile: {verb} {} → {}", done.from, done.env_file);
    if let Some(backup) = &done.backup {
        line.push_str(&format!(
            " ({} the previous one to {backup})",
            match dry_run {
                true => "backing up",
                false => "backed up",
            }
        ));
        if done.backup_unignored {
            line.push_str(&format!(
                " — ⚠ git doesn't ignore {backup}, and it holds what .env held: add `.env.bak*` to .gitignore"
            ));
        }
    }
    Some(line)
}

/// Whether a `.env`'s content is an unedited copy of the profile its header
/// names — the one kind of `.env` that can be replaced without a backup,
/// since the profile file can make it again.
fn pristine(root: &Path, content: &str) -> bool {
    let Some(first) = content.lines().next() else {
        return false;
    };
    let Some((name, from)) = first
        .strip_prefix(HEADER)
        .and_then(|rest| rest.split_once(" from "))
    else {
        return false;
    };
    std::fs::read_to_string(root.join(from.trim()))
        .is_ok_and(|body| content == copy_of(name.trim(), from.trim(), &body))
}

/// The first of `.env.bak`, `.env.bak.1`, `.env.bak.2`, … that doesn't exist
/// yet: an earlier backup may be the only copy of somebody's secrets.
fn free_backup(root: &Path, env_file: &str) -> String {
    let first = format!("{env_file}.bak");
    if !root.join(&first).exists() {
        return first;
    }
    (1..)
        .map(|n| format!("{env_file}.bak.{n}"))
        .find(|candidate| !root.join(candidate).exists())
        .expect("an unused backup name")
}

/// Whether git would ignore `rel`. Outside a repository there's nothing to
/// commit it by accident, which counts as ignored.
fn git_ignores(root: &Path, rel: &str) -> bool {
    let inside = std::process::Command::new("git")
        .args(["rev-parse", "--is-inside-work-tree"])
        .current_dir(root)
        .output()
        .is_ok_and(|o| o.status.success());
    if !inside {
        return true;
    }
    std::process::Command::new("git")
        .args(["check-ignore", "-q", "--no-index", rel])
        .current_dir(root)
        .status()
        .is_ok_and(|s| s.success())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn scratch(name: &str) -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("ciab_profiles_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join(".ciabatta")).unwrap();
        root
    }

    #[test]
    fn profiles_live_in_the_ciabatta_folder_and_templates_are_not_profiles() {
        let root = scratch("list");
        std::fs::create_dir_all(root.join("packages/api/.ciabatta")).unwrap();
        std::fs::write(root.join(".ciabatta/.env.staging"), "# staging\nA=2\nB=3\n").unwrap();
        std::fs::write(root.join(".ciabatta/.env.default"), "A=\n").unwrap();
        std::fs::write(root.join(".ciabatta/.env.staging.example"), "A=\n").unwrap();
        // Beside the `.env` is not where profiles live.
        std::fs::write(root.join(".env.old"), "A=1\n").unwrap();
        std::fs::write(root.join("packages/api/.ciabatta/.env.staging"), "C=4\n").unwrap();
        std::fs::write(root.join("packages/api/.ciabatta/.env.ci"), "D=5\n").unwrap();

        let found = list(&root, &[".".to_string(), "packages/api".to_string()]);
        let names: Vec<&str> = found.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["ci", "staging"]);
        let staging = &found[1];
        assert_eq!(
            staging.files,
            [
                ".ciabatta/.env.staging",
                "packages/api/.ciabatta/.env.staging"
            ]
        );
        assert_eq!(staging.vars, 3);

        let _ = std::fs::remove_dir_all(&root);
    }

    /// Electing replaces the `.env` with the profile, saving a hand-written
    /// `.env` first — and never overwriting an earlier backup.
    #[test]
    fn electing_a_profile_replaces_the_env_and_backs_up_what_was_there() {
        let root = scratch("apply");
        std::fs::write(root.join(".ciabatta/.env.dev"), "STAGE=dev\n").unwrap();
        std::fs::write(root.join(".ciabatta/.env.prod"), "STAGE=prod\n").unwrap();
        std::fs::write(root.join(".env"), "SECRET=only-copy\n").unwrap();

        // A dry run says what it would do, and does nothing.
        let planned = apply(&root, "dev", true).unwrap();
        assert_eq!(planned[0].backup.as_deref(), Some(".env.bak"));
        assert_eq!(
            std::fs::read_to_string(root.join(".env")).unwrap(),
            "SECRET=only-copy\n"
        );

        let done = apply(&root, "dev", false).unwrap();
        assert_eq!(done.len(), 1);
        assert_eq!(done[0].env_file, ".env");
        assert_eq!(done[0].from, ".ciabatta/.env.dev");
        assert_eq!(done[0].backup.as_deref(), Some(".env.bak"));
        assert_eq!(
            std::fs::read_to_string(root.join(".env.bak")).unwrap(),
            "SECRET=only-copy\n"
        );
        let env = std::fs::read_to_string(root.join(".env")).unwrap();
        assert!(env.ends_with("STAGE=dev\n"), "{env}");
        assert_eq!(
            elected(&root.join(".env")),
            Some(("dev".to_string(), ".ciabatta/.env.dev".to_string()))
        );

        // Again: nothing to do.
        assert!(apply(&root, "dev", false).unwrap()[0].unchanged);

        // Switching from an unedited copy needs no backup — the profile file
        // can make it again — so `.env.bak` keeps the original secrets.
        let switched = apply(&root, "prod", false).unwrap();
        assert_eq!(switched[0].backup, None);
        assert_eq!(
            std::fs::read_to_string(root.join(".env.bak")).unwrap(),
            "SECRET=only-copy\n"
        );

        // An edited copy is somebody's work: backed up, beside the first.
        let edited = std::fs::read_to_string(root.join(".env")).unwrap() + "EXTRA=1\n";
        std::fs::write(root.join(".env"), edited).unwrap();
        let again = apply(&root, "dev", false).unwrap();
        assert_eq!(again[0].backup.as_deref(), Some(".env.bak.1"));

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
