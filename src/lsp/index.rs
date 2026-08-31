//! What the editor needs to know about the repository, and when to look again.
//!
//! Everything here comes from [`crate::workspace::Workspace::load`] — the same
//! discovery `ciabatta build` runs — so a completion list and the graph that
//! actually executes can't disagree about what exists. The only thing this
//! module adds is a cache, because a keystroke is not a reason to re-read every
//! `.ciabatta/` in the monorepo.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::config::CIABATTA_DIR;
use crate::workspace::{WORKFLOWS_DIR, Workspace, find_workspace_root};

/// How long a scan stays good. Long enough that typing never triggers one,
/// short enough that adding a sub-workspace shows up without restarting the
/// editor. Any edit to a `.ciabatta/` file invalidates it immediately anyway.
const TTL: Duration = Duration::from_secs(30);

/// Which ciabatta file a document is, which decides what its fields mean.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Role {
    /// A member's `.ciabatta/ciabatta.yaml`.
    Config,
    /// `.ciabatta/workflows/<name>.yaml`, carrying the workflow's name.
    Workflow(String),
}

/// A document's place in the monorepo.
#[derive(Debug, Clone)]
pub struct Location {
    pub role: Role,
    /// The directory that owns the `.ciabatta/` this file lives in.
    pub member_dir: PathBuf,
}

/// Classify a path, returning `None` for a YAML file that isn't ciabatta's.
///
/// The shape is the whole test: `<member>/.ciabatta/ciabatta.yaml` and
/// `<member>/.ciabatta/workflows/<name>.yaml`. Anything else in `.ciabatta/`
/// (the cache, the AI transcripts) is ours but not authored, so it's left
/// alone.
pub fn classify(path: &Path) -> Option<Location> {
    if !crate::format::is_config_file(path) {
        return None;
    }
    let stem = path.file_stem()?.to_str()?;
    let parent = path.parent()?;

    if parent.file_name()?.to_str()? == CIABATTA_DIR && stem == "ciabatta" {
        return Some(Location {
            role: Role::Config,
            member_dir: parent.parent()?.to_path_buf(),
        });
    }

    if parent.file_name()?.to_str()? == WORKFLOWS_DIR {
        let ciabatta = parent.parent()?;
        if ciabatta.file_name()?.to_str()? == CIABATTA_DIR {
            return Some(Location {
                role: Role::Workflow(stem.to_string()),
                member_dir: ciabatta.parent()?.to_path_buf(),
            });
        }
    }

    None
}

/// One sub-workspace, flattened to what a completion list shows.
#[derive(Debug, Clone)]
pub struct MemberInfo {
    pub name: String,
    pub description: Option<String>,
    pub owner: String,
    /// Its workflows by name, with each one's description for the detail line.
    pub workflows: BTreeMap<String, Option<String>>,
}

/// The repository, as the language server sees it.
#[derive(Debug, Clone, Default)]
pub struct Index {
    pub members: Vec<MemberInfo>,
    /// `toolchain:` entries: tool name to its one-line description.
    pub tools: BTreeMap<String, Option<String>>,
    /// `registries:` entries: name to URL.
    pub registries: BTreeMap<String, String>,
    /// Every tag used anywhere, so a repo's vocabulary stays a small set
    /// instead of growing a synonym per package.
    pub tags: BTreeSet<String>,
    /// Environment variable names the repo already mentions — from `env:`
    /// blocks and from checked-in `.env.default` templates.
    pub env: BTreeSet<String>,
}

impl Index {
    /// Every `"<member>"` and `"<member>:<workflow>"` reference that resolves,
    /// paired with what it points at. This is the answer to "what can a
    /// `needs:` or `depends_on:` legitimately say?".
    pub fn workflow_refs(&self, exclude_member: Option<&str>) -> Vec<(String, String)> {
        let mut refs = Vec::new();
        for m in &self.members {
            if Some(m.name.as_str()) == exclude_member {
                continue;
            }
            for (workflow, description) in &m.workflows {
                refs.push((
                    format!("{}:{}", m.name, workflow),
                    description
                        .clone()
                        .unwrap_or_else(|| format!("{}'s {workflow} workflow", m.name)),
                ));
            }
            // The bare form means "their workflow of this same name", which is
            // only meaningful for a member that has more than the one.
            if !m.workflows.is_empty() {
                refs.push((
                    m.name.clone(),
                    m.description
                        .clone()
                        .unwrap_or_else(|| format!("owned by {}", m.owner)),
                ));
            }
        }
        refs
    }

    /// Whether a `"<member>"` / `"<member>:<workflow>"` reference resolves.
    pub fn resolves(&self, reference: &str) -> bool {
        let (member, workflow) = match reference.split_once(':') {
            Some((m, w)) => (m, Some(w)),
            None => (reference, None),
        };
        self.members
            .iter()
            .any(|m| m.name == member && workflow.is_none_or(|w| m.workflows.contains_key(w)))
    }
}

/// Holds the last scan and decides when to redo it.
#[derive(Default)]
pub struct Cache {
    entry: Option<(PathBuf, Instant, Index)>,
}

impl Cache {
    /// The index for the monorepo containing `dir`, scanning only when the
    /// cached one is stale or belongs to a different repo.
    pub fn get(&mut self, dir: &Path) -> Index {
        let Some(root) = find_workspace_root(dir) else {
            return Index::default();
        };
        if let Some((cached_root, at, index)) = &self.entry
            && cached_root == &root
            && at.elapsed() < TTL
        {
            return index.clone();
        }
        let index = scan(&root);
        self.entry = Some((root, Instant::now(), index.clone()));
        index
    }

    /// Throw the cached scan away — a `.ciabatta/` file changed on disk.
    pub fn invalidate(&mut self) {
        self.entry = None;
    }
}

/// Read the monorepo at `root` into an index.
///
/// A workspace that doesn't load — two members claiming the same name, a
/// half-written config — yields an empty index rather than an error. The editor
/// is where a repo is *mid-edit*; refusing to complete anything until it parses
/// would withhold help exactly when it's wanted.
fn scan(root: &Path) -> Index {
    let Ok(workspace) = Workspace::load(root) else {
        // A workspace that doesn't load yields an empty index, which every
        // caller reads as "nothing to compare against" and stays quiet about.
        return Index::default();
    };

    let mut index = Index::default();

    for (tool, spec) in &workspace.toolchain {
        index.tools.insert(tool.clone(), spec.description.clone());
    }
    index.env.extend(workspace.env.keys().cloned());
    index.tags.extend(workspace.root_meta.tags.iter().cloned());

    for member in &workspace.members {
        for (name, registry) in &member.config.registries {
            index.registries.insert(name.clone(), registry.url.clone());
        }
        index.tags.extend(member.meta.tags.iter().cloned());
        index.env.extend(member.meta.env.keys().cloned());

        let mut workflows = BTreeMap::new();
        for (name, workflow) in &member.workflows {
            index.tags.extend(workflow.tags.iter().cloned());
            index.env.extend(workflow.env.keys().cloned());
            index.env.extend(workflow.required_env.iter().cloned());
            for step in &workflow.steps {
                index.tags.extend(step.tags.iter().cloned());
                index.env.extend(step.env.keys().cloned());
            }
            workflows.insert(name.clone(), said(workflow.description.as_deref()));
        }

        index.env.extend(env_template_keys(member));

        index.members.push(MemberInfo {
            name: member.name.clone(),
            description: said(member.meta.description.as_deref()),
            owner: member.owner().to_string(),
            workflows,
        });
    }

    index
}

/// Whether a description was actually written.
///
/// `ciabatta init --lib` scaffolds `description: ""` with a TODO beside it, so
/// an empty string means "nobody has filled this in yet" rather than "this has
/// no description" — and showing it as a blank detail line is worse than
/// showing the fallback.
fn said(text: Option<&str>) -> Option<String> {
    text.map(str::trim)
        .filter(|t| !t.is_empty())
        .map(str::to_string)
}

/// Variable names from a member's checked-in `.env` template.
///
/// The template is the one file that says what a build needs without saying
/// what the values are, which makes it the right place to learn names from.
fn env_template_keys(member: &crate::workspace::Member) -> Vec<String> {
    let template = member.meta.env_default.as_deref().unwrap_or(".env.default");
    let Ok(text) = std::fs::read_to_string(member.dir.join(template)) else {
        return Vec::new();
    };
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter_map(|l| l.split_once('=').map(|(k, _)| k.trim().to_string()))
        .filter(|k| !k.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_the_two_authored_file_shapes() {
        let cfg = classify(Path::new("/repo/api/.ciabatta/ciabatta.yaml")).unwrap();
        assert_eq!(cfg.role, Role::Config);
        assert_eq!(cfg.member_dir, Path::new("/repo/api"));

        let wf = classify(Path::new("/repo/api/.ciabatta/workflows/build.yaml")).unwrap();
        assert_eq!(wf.role, Role::Workflow("build".into()));
        assert_eq!(wf.member_dir, Path::new("/repo/api"));
    }

    #[test]
    fn legacy_toml_is_still_classified() {
        assert_eq!(
            classify(Path::new("/repo/.ciabatta/workflows/test.toml"))
                .unwrap()
                .role,
            Role::Workflow("test".into())
        );
    }

    #[test]
    fn other_yaml_is_not_ours() {
        assert!(classify(Path::new("/repo/docker-compose.yml")).is_none());
        assert!(classify(Path::new("/repo/.ciabatta/cache/entries/a.json")).is_none());
        assert!(classify(Path::new("/repo/.github/workflows/ci.yaml")).is_none());
    }

    fn index_with(member: &str, workflows: &[&str]) -> Index {
        Index {
            members: vec![MemberInfo {
                name: member.into(),
                description: None,
                owner: "unowned".into(),
                workflows: workflows.iter().map(|w| ((*w).into(), None)).collect(),
            }],
            ..Index::default()
        }
    }

    #[test]
    fn references_resolve_in_both_spellings() {
        let index = index_with("proto", &["generate"]);
        assert!(index.resolves("proto"));
        assert!(index.resolves("proto:generate"));
        assert!(!index.resolves("proto:build"));
        assert!(!index.resolves("protos"));
    }

    #[test]
    fn a_member_does_not_offer_itself_as_a_dependency() {
        let mut index = index_with("api", &["build"]);
        index.members.push(MemberInfo {
            name: "proto".into(),
            description: None,
            owner: "unowned".into(),
            workflows: [("generate".to_string(), None)].into_iter().collect(),
        });
        let offered: Vec<String> = index
            .workflow_refs(Some("api"))
            .into_iter()
            .map(|(r, _)| r)
            .collect();
        assert!(offered.contains(&"proto:generate".to_string()));
        assert!(!offered.iter().any(|r| r.starts_with("api")));
    }
}
