//! The monorepo layer: sub-workspaces, their workflows, and the one graph that
//! ties them together.
//!
//! A monorepo accumulates scripts nobody owns, published somewhere nobody
//! remembers, that quietly depend on each other in ways nobody wrote down.
//! Ciabatta's answer is that every sub-workspace opts in with a `.ciabatta/`
//! directory (`ciabatta init --lib`) declaring three things: **who owns it**,
//! **what its workflows do**, and **which other sub-workspaces they need**.
//!
//! From those declarations `ciabatta build` — or any other workflow name —
//! resolves one cross-workspace graph, so a build that needs another package's
//! generated protobufs simply says so and gets them, in order, every time.
//!
//! * [`Workspace::discover`] finds the monorepo root and every member in it.
//! * [`graph`] compiles a named workflow into a single dependency graph.
//! * [`render`] draws that graph, and lists what's available across the repo.

pub mod graph;
pub mod render;

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::config::{CIABATTA_DIR, CiabattaConfig, config_path, load_config_file};
use crate::run::RunStep;

/// Directory inside a member's `.ciabatta/` holding one file per workflow.
pub const WORKFLOWS_DIR: &str = "workflows";

/// How deep below the workspace root to look for members. Deep enough for the
/// usual `packages/<group>/<name>` layouts, shallow enough that scanning a
/// large repo stays instant.
const MAX_SCAN_DEPTH: usize = 6;

/// Directories never worth descending into when looking for members: build
/// output and dependency trees, which can hold thousands of files and never
/// contain a hand-written `.ciabatta/`.
const SKIP_DIRS: &[&str] = &[
    "node_modules",
    "target",
    "dist",
    "build",
    "out",
    "vendor",
    "venv",
    ".venv",
    "__pycache__",
    "coverage",
    "tmp",
];

// ─── Schema ─────────────────────────────────────────────────────────────────

/// The `[workspace]` table of a member's `.ciabatta/ciabatta.toml`: what this
/// sub-workspace is, who to ask about it, and what it depends on.
///
/// ```toml
/// [workspace]
/// name        = "api"
/// description = "The public REST API"
/// owner       = "Henry Forsyth"
/// depends_on  = ["proto:generate", "common"]
/// requires    = ["cargo"]
/// ```
#[derive(Debug, Deserialize, Serialize, Clone, Default)]
pub struct WorkspaceMeta {
    /// This sub-workspace's name across the monorepo. Defaults to the directory
    /// name, which is usually what you'd have typed anyway.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// One line on what lives here. `ciabatta list` prints it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Who owns this sub-workspace — a name, handle, or team.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    /// Other sub-workspaces this one depends on, as `"<member>"` (their
    /// same-named workflow, when they have one) or `"<member>:<workflow>"` (one
    /// specific workflow). Applies to every workflow defined here, so the
    /// common case — "we need whatever `proto` does first" — is one line.
    #[serde(default)]
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<String>,
    /// Free-form labels for search (`ciabatta list --search backend`).
    #[serde(default)]
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// Tools every workflow here needs on `PATH`; see [`ToolSpec`].
    #[serde(default)]
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub requires: Vec<String>,
    /// `.env` file path(s), relative to this sub-workspace, sourced before any
    /// of its workflows run.
    ///
    /// Unset means `.env` — the conventional thing needs no configuration.
    /// Setting it *replaces* that default rather than adding to it, which is
    /// what "use this file instead" has to mean for it to be useful.
    #[serde(default, deserialize_with = "crate::run::string_or_vec")]
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub env_file: Vec<String>,
    /// The checked-in template the `.env` is generated from when it's missing —
    /// conventionally `.env.default`.
    ///
    /// Required of any workspace whose builds declare `REQUIRED_ENV`: those
    /// builds cannot run without those variables, and a repo that doesn't write
    /// down what they are is one a new person can't build. See
    /// [`crate::environment::files`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub env_default: Option<String>,
    /// Environment variables applied to every step defined here — the standard
    /// set a sub-workspace wants its scripts to be able to count on.
    #[serde(default)]
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    /// Set on the monorepo root's own config when the root is only an umbrella
    /// (shared `[toolchain]` and `[env]`) rather than a package of its own.
    #[serde(default)]
    #[serde(skip_serializing_if = "crate::format::is_false")]
    pub umbrella: bool,
}

/// One workflow: a named, described, owned set of steps — `build`, `test`,
/// `lint`, `deploy`, whatever the repo agrees on.
///
/// Written either as its own file in `.ciabatta/workflows/<name>.toml` (the
/// name comes from the filename) or inline as `[workflows.<name>]`.
///
/// ```toml
/// description = "Compile the service binary"
/// owner       = "Henry Forsyth"
/// needs       = ["proto:generate"]
///
/// [[steps]]
/// name        = "compile"
/// description = "cargo build --release"
/// run         = "cargo build --release"
/// requires    = ["cargo"]
/// timeout     = "10m"
/// ```
#[derive(Debug, Deserialize, Serialize, Clone, Default)]
pub struct Workflow {
    /// What running this workflow accomplishes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Who owns it. Falls back to the sub-workspace's owner.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    /// Workflows in other sub-workspaces that must finish first, on top of
    /// whatever `[workspace] depends_on` already declares. Same
    /// `"<member>"` / `"<member>:<workflow>"` spelling.
    #[serde(default)]
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub needs: Vec<String>,
    /// Tools this workflow needs on `PATH`, applied to all of its steps.
    #[serde(default)]
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub requires: Vec<String>,
    /// `.env` file path(s) relative to the sub-workspace, sourced before the
    /// graph runs.
    #[serde(default, deserialize_with = "crate::run::string_or_vec")]
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub env_file: Vec<String>,
    /// Variables that must be set and non-empty, or the run is refused up front.
    #[serde(default, rename = "REQUIRED_ENV")]
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub required_env: Vec<String>,
    /// Environment variables applied to every step of this workflow.
    #[serde(default)]
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    /// Free-form labels for search.
    #[serde(default)]
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// The steps themselves — the same node type the run engine executes, so a
    /// workflow gets `needs`, `on_error` recovery, conditions, timeouts and
    /// persistence for free.
    #[serde(default)]
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub steps: Vec<RunStep>,
}

/// How to satisfy a build tool a workflow declares in `requires`.
///
/// ```toml
/// [toolchain.protoc]
/// hint  = "brew install protobuf"
/// check = "protoc --version"
/// ```
///
/// Declared once at the monorepo root and inherited by every member, so
/// "install X" is written down where the person hitting the error will see it.
#[derive(Debug, Deserialize, Serialize, Clone, Default)]
pub struct ToolSpec {
    /// The command that installs it, printed verbatim when it's missing.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
    /// An alternative way to detect the tool when a bare `PATH` lookup won't
    /// do (a plugin, a specific version). Run through the shell; a zero exit
    /// means present.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub check: Option<String>,
    /// What the tool is for, in one line.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

// ─── Discovery ──────────────────────────────────────────────────────────────

/// One sub-workspace: a directory that opted in with its own `.ciabatta/`.
#[derive(Debug, Clone)]
pub struct Member {
    /// The name other members refer to it by.
    pub name: String,
    /// Absolute path to the sub-workspace directory (the one holding
    /// `.ciabatta/`).
    pub dir: PathBuf,
    /// Its path relative to the monorepo root — what the graph shows, and what
    /// step `cwd`s are expressed as.
    pub rel: String,
    /// The `[workspace]` table, defaulted when the member didn't write one.
    pub meta: WorkspaceMeta,
    /// Its full config, so registries and recipes stay available to `push`
    /// steps and `ciabatta list`.
    pub config: CiabattaConfig,
    /// Its workflows by name, from `.ciabatta/workflows/*.toml` and
    /// `[workflows.<name>]` together.
    pub workflows: BTreeMap<String, Workflow>,
}

impl Member {
    /// Who to ask about this sub-workspace, or a nudge to write it down.
    pub fn owner(&self) -> &str {
        self.meta.owner.as_deref().unwrap_or("unowned")
    }

    /// Whether this member defines a workflow by that name.
    pub fn has_workflow(&self, name: &str) -> bool {
        self.workflows.contains_key(name)
    }
}

/// A whole monorepo: its root, its members, and the toolchain hints they share.
#[derive(Debug, Clone)]
pub struct Workspace {
    /// The monorepo root every member path is relative to.
    pub root: PathBuf,
    /// Every sub-workspace found beneath it, sorted by path so output is
    /// stable between runs and machines.
    pub members: Vec<Member>,
    /// `[toolchain]` entries, merged root-first so a member may add its own.
    pub toolchain: BTreeMap<String, ToolSpec>,
    /// The umbrella root's `[workspace.env]`: the standard variables the whole
    /// monorepo agrees on, inherited by every member's every step.
    ///
    /// This is the "one set of variables everyone can count on" a monorepo
    /// otherwise re-declares in each package and then lets drift. A member — or
    /// a single step — can still override any of them locally.
    pub env: BTreeMap<String, String>,
}

impl Workspace {
    /// Find the monorepo root from `start` and load every member in it.
    pub fn discover(start: &Path) -> Result<Self> {
        let root = find_workspace_root(start).ok_or_else(|| {
            anyhow::anyhow!(
                "No ciabatta workspace found at or above '{}'.\n\
                 Run `ciabatta init --lib` in a package to opt it in.",
                start.display()
            )
        })?;
        Self::load(&root)
    }

    /// Load the workspace rooted at `root`.
    pub fn load(root: &Path) -> Result<Self> {
        let mut dirs: Vec<PathBuf> = Vec::new();
        collect_member_dirs(root, 0, &mut dirs)?;
        dirs.sort();

        let mut members: Vec<Member> = Vec::new();
        let mut toolchain: BTreeMap<String, ToolSpec> = BTreeMap::new();
        let mut env: BTreeMap<String, String> = BTreeMap::new();

        for dir in dirs {
            let member = load_member(root, &dir)?;
            // Root-level toolchain entries land first and are the ones a member
            // may then specialize, so iterate in path order and let later
            // (deeper) definitions win for their own tools.
            for (tool, spec) in &member.config.toolchain {
                toolchain.insert(tool.clone(), spec.clone());
            }
            // An umbrella root contributes its toolchain and its standard
            // variables, and nothing else: it isn't a package, so it shouldn't
            // show up as one.
            if member.meta.umbrella {
                env.extend(member.meta.env.clone());
                continue;
            }
            members.push(member);
        }

        let workspace = Workspace {
            root: root.to_path_buf(),
            members,
            toolchain,
            env,
        };
        workspace.check_unique_names()?;
        Ok(workspace)
    }

    /// Look up a member by name.
    pub fn member(&self, name: &str) -> Option<&Member> {
        self.members.iter().find(|m| m.name == name)
    }

    /// Every distinct workflow name defined anywhere in the monorepo, sorted —
    /// the answer to "what can I actually run here?".
    pub fn workflow_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .members
            .iter()
            .flat_map(|m| m.workflows.keys().cloned())
            .collect();
        names.sort();
        names.dedup();
        names
    }

    /// The members that define a workflow of this name.
    pub fn providers(&self, workflow: &str) -> Vec<&Member> {
        self.members
            .iter()
            .filter(|m| m.has_workflow(workflow))
            .collect()
    }

    /// The install hint for a tool, when the repo documented one.
    pub fn hint(&self, tool: &str) -> Option<&str> {
        self.toolchain.get(tool).and_then(|t| t.hint.as_deref())
    }

    /// Two members answering to the same name would make every `depends_on`
    /// referring to it ambiguous, so refuse the whole workspace and say which
    /// directories collided.
    fn check_unique_names(&self) -> Result<()> {
        let mut seen: HashSet<&str> = HashSet::new();
        for member in &self.members {
            if !seen.insert(member.name.as_str()) {
                let dirs: Vec<&str> = self
                    .members
                    .iter()
                    .filter(|m| m.name == member.name)
                    .map(|m| m.rel.as_str())
                    .collect();
                bail!(
                    "Two sub-workspaces are both named '{}' ({}).\n\
                     Give one of them a different [workspace] name in its .ciabatta/ciabatta.toml.",
                    member.name,
                    dirs.join(" and ")
                );
            }
        }
        Ok(())
    }
}

/// Find the monorepo root: the git repository root when there is one (every
/// member of the repo is then in scope), otherwise the highest ancestor that
/// has a `.ciabatta/` directory.
///
/// The git root wins even when it has no `.ciabatta/` of its own — running
/// `ciabatta build` from inside one package should still see its siblings,
/// which is the entire point of a monorepo-wide graph.
pub fn find_workspace_root(start: &Path) -> Option<PathBuf> {
    let start = std::fs::canonicalize(start).unwrap_or_else(|_| start.to_path_buf());
    let mut highest_ciabatta: Option<PathBuf> = None;
    let mut git_root: Option<PathBuf> = None;

    let mut current = start.clone();
    loop {
        if current.join(CIABATTA_DIR).is_dir() {
            highest_ciabatta = Some(current.clone());
        }
        if current.join(".git").exists() && git_root.is_none() {
            git_root = Some(current.clone());
        }
        if !current.pop() {
            break;
        }
    }

    git_root.or(highest_ciabatta)
}

/// Every directory at or below `root` that holds a ciabatta config, in path
/// order — the sub-workspaces a build would load.
///
/// Public because migration has to cover exactly the set of files discovery
/// covers: a converter that finds fewer files than the loader does leaves a
/// repo half-migrated in a way nobody notices until a build.
pub fn member_dirs(root: &Path) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    let _ = collect_member_dirs(root, 0, &mut dirs);
    dirs.sort();
    dirs
}

/// Walk down from `dir` collecting every directory that holds a ciabatta
/// config. Nested members are allowed, so the walk keeps descending past one it
/// just found.
fn collect_member_dirs(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) -> Result<()> {
    if config_path(dir).is_some() {
        out.push(dir.to_path_buf());
    }
    if depth >= MAX_SCAN_DEPTH {
        return Ok(());
    }

    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        // An unreadable directory (permissions, a broken symlink) shouldn't
        // sink the whole scan — there may be perfectly good members elsewhere.
        Err(_) => return Ok(()),
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        // Hidden directories never hold a member: `.ciabatta` is looked for on
        // the parent, not descended into.
        if name.starts_with('.') || SKIP_DIRS.contains(&name) {
            continue;
        }
        collect_member_dirs(&path, depth + 1, out)?;
    }
    Ok(())
}

/// Load one member: its config, its `[workspace]` identity, and its workflows.
fn load_member(root: &Path, dir: &Path) -> Result<Member> {
    let config_file = config_path(dir).ok_or_else(|| {
        anyhow::anyhow!("No ciabatta config in {}", dir.join(CIABATTA_DIR).display())
    })?;
    let config = load_config_file(&config_file)?;
    let meta = config.workspace.clone().unwrap_or_default();

    let rel = dir
        .strip_prefix(root)
        .unwrap_or(dir)
        .to_string_lossy()
        .replace('\\', "/");
    let rel = if rel.is_empty() { ".".to_string() } else { rel };

    let name = meta
        .name
        .clone()
        .or_else(|| {
            dir.file_name()
                .and_then(|n| n.to_str())
                .map(|s| s.to_string())
        })
        .unwrap_or_else(|| rel.clone());

    let workflows = load_workflows(dir, &config, &name)?;

    Ok(Member {
        name,
        dir: dir.to_path_buf(),
        rel,
        meta,
        config,
        workflows,
    })
}

/// Collect a member's workflows from both places they can be written: one file
/// per workflow under `.ciabatta/workflows/`, and inline `workflows:` entries
/// in its `ciabatta.yaml`.
///
/// A name defined in both is an error rather than a silent precedence rule —
/// two definitions of "build" that drift apart is the exact confusion this tool
/// exists to remove.
fn load_workflows(
    dir: &Path,
    config: &CiabattaConfig,
    member: &str,
) -> Result<BTreeMap<String, Workflow>> {
    let mut workflows: BTreeMap<String, Workflow> = config
        .workflows
        .iter()
        .map(|(name, workflow)| (name.clone(), workflow.clone()))
        .collect();

    let workflow_dir = dir.join(CIABATTA_DIR).join(WORKFLOWS_DIR);

    for path in crate::format::config_files_in(&workflow_dir) {
        let Some(name) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        let workflow: Workflow = crate::format::load(&path)
            .with_context(|| format!("Failed to load workflow file '{}'", path.display()))?;

        if config.workflows.contains_key(name) {
            bail!(
                "Sub-workspace '{}' defines the workflow '{}' twice: in {} and under \
                 `workflows:` in its ciabatta config. Keep one.",
                member,
                name,
                path.display(),
            );
        }
        workflows.insert(name.to_string(), workflow);
    }

    Ok(workflows)
}

// ─── Toolchain checks ───────────────────────────────────────────────────────

/// Whether an executable of this name is on `PATH`.
///
/// On Windows a bare name won't be found without its extension, so the usual
/// `PATHEXT` suffixes are tried too.
pub fn tool_on_path(tool: &str) -> bool {
    let tool = tool.trim();
    if tool.is_empty() {
        return true;
    }
    // An absolute or relative path is checked as given rather than searched.
    if tool.contains('/') || tool.contains('\\') {
        return Path::new(tool).is_file();
    }

    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    let exts: Vec<String> = if cfg!(windows) {
        std::env::var("PATHEXT")
            .unwrap_or_else(|_| ".EXE;.CMD;.BAT;.COM".to_string())
            .split(';')
            .map(|e| e.to_string())
            .collect()
    } else {
        vec![String::new()]
    };

    std::env::split_paths(&path).any(|dir| {
        exts.iter()
            .any(|ext| is_executable_file(&dir.join(format!("{tool}{ext}"))))
    })
}

#[cfg(unix)]
fn is_executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.metadata()
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable_file(path: &Path) -> bool {
    path.is_file()
}

/// The `[toolchain]` install hints reachable from `root`, as tool → hint.
///
/// Used by the run engine's preflight, which has a project root but no loaded
/// workspace; a repo without one simply has no hints to offer.
pub fn toolchain_hints(root: &Path) -> BTreeMap<String, String> {
    let mut hints = BTreeMap::new();
    let Some(ws_root) = find_workspace_root(root) else {
        return hints;
    };
    let Ok(workspace) = Workspace::load(&ws_root) else {
        return hints;
    };
    for (tool, spec) in &workspace.toolchain {
        if let Some(hint) = &spec.hint {
            hints.insert(tool.clone(), hint.clone());
        }
    }
    hints
}

/// A build tool a workflow needs but this machine doesn't have.
#[derive(Debug, Clone)]
pub struct MissingTool {
    pub tool: String,
    /// How to install it, when the repo wrote it down.
    pub hint: Option<String>,
    /// The graph nodes that need it, so it's clear what won't run without it.
    pub needed_by: Vec<String>,
}

/// Check every tool the given steps require, resolving hints from `workspace`.
///
/// Reports all of them together: finding out about three missing tools one
/// failed build at a time is precisely the experience worth avoiding.
pub fn missing_tools(workspace: &Workspace, steps: &[RunStep]) -> Vec<MissingTool> {
    let mut missing: Vec<MissingTool> = Vec::new();
    for step in steps {
        for tool in &step.requires {
            if let Some(entry) = missing.iter_mut().find(|m| &m.tool == tool) {
                entry.needed_by.push(step.name.clone());
                continue;
            }
            if tool_present(workspace, tool) {
                continue;
            }
            missing.push(MissingTool {
                tool: tool.clone(),
                hint: workspace.hint(tool).map(str::to_string),
                needed_by: vec![step.name.clone()],
            });
        }
    }
    missing
}

/// Whether a tool counts as installed: a `[toolchain.<tool>] check` command
/// when one is configured, otherwise a plain `PATH` lookup.
fn tool_present(workspace: &Workspace, tool: &str) -> bool {
    if let Some(check) = workspace
        .toolchain
        .get(tool)
        .and_then(|t| t.check.as_deref())
    {
        return std::process::Command::new(if cfg!(windows) { "cmd" } else { "sh" })
            .args(if cfg!(windows) { ["/C"] } else { ["-c"] })
            .arg(check)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
    }
    tool_on_path(tool)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a throwaway monorepo on disk and load it.
    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ciab_ws_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Write a member whose config is in the pre-0.2.0 format. Most fixtures
    /// here are TOML, which keeps the back-compat reader exercised by every
    /// discovery test rather than by one token case.
    fn write_member(root: &Path, rel: &str, config: &str) -> PathBuf {
        write_member_as(root, rel, "ciabatta.toml", config)
    }

    fn write_member_as(root: &Path, rel: &str, file: &str, config: &str) -> PathBuf {
        let dir = root.join(rel);
        std::fs::create_dir_all(dir.join(CIABATTA_DIR)).unwrap();
        std::fs::write(dir.join(CIABATTA_DIR).join(file), config).unwrap();
        dir
    }

    fn write_workflow(member: &Path, name: &str, body: &str) {
        write_workflow_as(member, &format!("{name}.toml"), body);
    }

    fn write_workflow_as(member: &Path, file: &str, body: &str) {
        let dir = member.join(CIABATTA_DIR).join(WORKFLOWS_DIR);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(file), body).unwrap();
    }

    /// The format ciabatta writes from 0.2.0. Discovery, workflow loading and
    /// the `depends_on` wiring must all behave identically to the TOML fixtures
    /// the rest of this module uses.
    #[test]
    fn discovers_yaml_members_and_workflows() {
        let root = scratch("discover_yaml");
        let api = write_member_as(
            &root,
            "packages/api",
            "ciabatta.yaml",
            "workspace:\n  name: api\n  owner: Ada\n  depends_on: [proto]\n",
        );
        write_workflow_as(
            &api,
            "build.yaml",
            "description: Build the API\nsteps:\n  - name: compile\n    run: cargo build\n",
        );
        write_member_as(
            &root,
            "packages/proto",
            "ciabatta.yml",
            "workspace:\n  name: proto\n",
        );

        let ws = Workspace::load(&root).unwrap();
        let names: Vec<&str> = ws.members.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(names, vec!["api", "proto"]);

        let api = ws.member("api").unwrap();
        assert_eq!(api.owner(), "Ada");
        assert_eq!(api.meta.depends_on, vec!["proto".to_string()]);
        let build = api.workflows.get("build").expect("build workflow loaded");
        assert_eq!(build.description.as_deref(), Some("Build the API"));
        assert_eq!(build.steps.len(), 1);
        assert_eq!(build.steps[0].run.as_deref(), Some("cargo build"));
    }

    /// A member mid-migration has both files; the YAML one is the config.
    #[test]
    fn yaml_wins_over_a_leftover_toml_config() {
        let root = scratch("both_formats");
        let dir = root.join("pkg");
        std::fs::create_dir_all(dir.join(CIABATTA_DIR)).unwrap();
        std::fs::write(
            dir.join(CIABATTA_DIR).join("ciabatta.toml"),
            "[workspace]\nname = \"old\"\n",
        )
        .unwrap();
        std::fs::write(
            dir.join(CIABATTA_DIR).join("ciabatta.yaml"),
            "workspace:\n  name: new\n",
        )
        .unwrap();

        let ws = Workspace::load(&root).unwrap();
        let names: Vec<&str> = ws.members.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(names, vec!["new"], "the migrated file must win");
    }

    #[test]
    fn discovers_members_and_their_workflows() {
        let root = scratch("discover");
        let api = write_member(
            &root,
            "packages/api",
            "[workspace]\nname = \"api\"\nowner = \"Ada\"\ndepends_on = [\"proto\"]\n",
        );
        write_workflow(
            &api,
            "build",
            "description = \"Build the API\"\n[[steps]]\nname = \"compile\"\nrun = \"cargo build\"\n",
        );
        let proto = write_member(&root, "packages/proto", "[workspace]\nname = \"proto\"\n");
        write_workflow(
            &proto,
            "build",
            "[[steps]]\nname = \"generate\"\nrun = \"protoc\"\n",
        );
        // Build output must not be scanned, even when it looks like a member.
        write_member(&root, "packages/api/target/junk", "[workspace]\n");

        let ws = Workspace::load(&root).unwrap();
        let names: Vec<&str> = ws.members.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(names, vec!["api", "proto"]);
        assert_eq!(ws.workflow_names(), vec!["build".to_string()]);
        assert_eq!(ws.member("api").unwrap().owner(), "Ada");
        assert_eq!(
            ws.member("api").unwrap().workflows["build"]
                .description
                .as_deref(),
            Some("Build the API")
        );
        assert_eq!(ws.member("api").unwrap().rel, "packages/api");

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn member_name_defaults_to_its_directory() {
        let root = scratch("defaultname");
        write_member(&root, "libs/common", "[workspace]\nowner = \"Team\"\n");
        let ws = Workspace::load(&root).unwrap();
        assert_eq!(ws.members[0].name, "common");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn duplicate_member_names_are_refused() {
        let root = scratch("dupes");
        write_member(&root, "a/thing", "[workspace]\nname = \"thing\"\n");
        write_member(&root, "b/thing", "[workspace]\nname = \"thing\"\n");
        let err = Workspace::load(&root).unwrap_err().to_string();
        assert!(err.contains("both named 'thing'"));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_workflow_defined_twice_is_refused() {
        let root = scratch("twice");
        let m = write_member(
            &root,
            "api",
            "[workspace]\nname = \"api\"\n\n[workflows.build]\n[[workflows.build.steps]]\nname = \"a\"\nrun = \"true\"\n",
        );
        write_workflow(&m, "build", "[[steps]]\nname = \"b\"\nrun = \"true\"\n");
        let err = Workspace::load(&root).unwrap_err().to_string();
        assert!(err.contains("twice"), "{err}");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn umbrella_root_contributes_toolchain_but_is_not_a_member() {
        let root = scratch("umbrella");
        write_member(
            &root,
            ".",
            "[workspace]\numbrella = true\n\n[toolchain.protoc]\nhint = \"brew install protobuf\"\n",
        );
        write_member(&root, "api", "[workspace]\nname = \"api\"\n");

        let ws = Workspace::load(&root).unwrap();
        assert_eq!(ws.members.len(), 1);
        assert_eq!(ws.members[0].name, "api");
        assert_eq!(ws.hint("protoc"), Some("brew install protobuf"));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn tool_on_path_finds_a_real_binary_and_misses_a_made_up_one() {
        // `sh` exists on every unix runner; the CI image for windows has cmd.
        let present = if cfg!(windows) { "cmd" } else { "sh" };
        assert!(tool_on_path(present));
        assert!(!tool_on_path("ciabatta-definitely-not-a-real-tool"));
        // An empty requirement is vacuously satisfied rather than always failing.
        assert!(tool_on_path("  "));
    }

    #[test]
    fn missing_tools_groups_by_tool_and_carries_the_hint() {
        let root = scratch("tools");
        write_member(
            &root,
            ".",
            "[workspace]\numbrella = true\n[toolchain.nope-not-real]\nhint = \"install nope\"\n",
        );
        write_member(&root, "api", "[workspace]\nname = \"api\"\n");
        let ws = Workspace::load(&root).unwrap();

        let steps = vec![
            RunStep {
                name: "one".into(),
                run: Some("true".into()),
                requires: vec!["nope-not-real".into()],
                ..Default::default()
            },
            RunStep {
                name: "two".into(),
                run: Some("true".into()),
                requires: vec!["nope-not-real".into()],
                ..Default::default()
            },
        ];
        let missing = missing_tools(&ws, &steps);
        assert_eq!(missing.len(), 1);
        assert_eq!(missing[0].needed_by, vec!["one", "two"]);
        assert_eq!(missing[0].hint.as_deref(), Some("install nope"));

        std::fs::remove_dir_all(&root).ok();
    }
}
