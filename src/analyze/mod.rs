//! `ciabatta analyze` — build a dependency graph of the project and serve an
//! interactive view of it.
//!
//! The graph has three node categories that lay out left → right:
//!   external  — third-party deps (crates.io, npm, pip, dockerhub, …)
//!   internal  — packages/modules within this repository
//!   publish   — where artifacts go (crates.io, ciabatta registries, …)
//!
//! Edges flow external → internal → publish.

pub mod cache;
pub mod requirements;
pub mod scan;
pub mod server;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

use crate::config::CiabattaConfig;

/// Which column a node belongs to (laid out left → right in this order).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Category {
    /// Project requirements (only present when a requirements file is given).
    Requirement,
    #[default]
    External,
    Internal,
    Publish,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Vulnerability {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
}

fn is_false(b: &bool) -> bool {
    !*b
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Node {
    pub id: String,
    pub label: String,
    pub category: Category,
    /// Package ecosystem / registry kind, e.g. "crates.io", "npm", "nexus".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ecosystem: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// The version *requirement* declared in the manifest (e.g. `"1"`, `"^0.4"`),
    /// kept alongside the resolved `version` when the two differ.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub req: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub license: Option<String>,
    /// Where this node came from (a manifest path, a recipe name, …).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// Free-text description (used for requirement nodes).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// True for a publish point that ciabatta itself manages (a recipe's
    /// registry), as opposed to one merely inferred (e.g. crates.io).
    #[serde(default, skip_serializing_if = "is_false")]
    pub ciabatta_managed: bool,
    /// True if this internal package declares a workspace.
    #[serde(default, skip_serializing_if = "is_false")]
    pub is_workspace: bool,
    /// For an internal package, the id of the workspace root it belongs to.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
    /// Source files a requirement is traced to (for requirement nodes).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub traced_files: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub vulnerabilities: Vec<Vulnerability>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Edge {
    pub from: String,
    pub to: String,
}

/// Metadata about a single manifest/source file discovered in the workspace.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FileInfo {
    /// Path relative to the project root.
    pub path: String,
    /// File name, e.g. "Cargo.toml", "package.json", "Dockerfile".
    pub kind: String,
    /// Ecosystem this file belongs to: rust | npm | python | docker.
    pub ecosystem: String,
    pub bytes: u64,
    /// Content hash (same value used by the analyze cache).
    pub hash: String,
    /// The internal package node this file defines, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub package: Option<String>,
    /// Workspace member globs declared by this file (empty if not a workspace root).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub workspace_members: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AnalysisGraph {
    pub root: String,
    pub generated_at: String,
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
    #[serde(default)]
    pub files: Vec<FileInfo>,
}

/// The nodes, edges, and file metadata contributed by scanning a single file.
/// This is the unit that gets cached (keyed by the file's content hash).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ScanOutput {
    pub nodes: Vec<Node>,
    pub edges: Vec<(String, String)>,
    #[serde(default)]
    pub files: Vec<FileInfo>,
}

/// Accumulates nodes and edges while scanning, de-duplicating by id.
#[derive(Default)]
pub struct GraphBuilder {
    nodes: BTreeMap<String, Node>,
    edges: std::collections::BTreeSet<(String, String)>,
    files: BTreeMap<String, FileInfo>,
}

impl GraphBuilder {
    /// Insert a node, keeping the richest version of it if it already exists.
    pub fn add_node(&mut self, node: Node) {
        self.nodes
            .entry(node.id.clone())
            .and_modify(|existing| {
                existing.version = existing.version.take().or_else(|| node.version.clone());
                existing.req = existing.req.take().or_else(|| node.req.clone());
                existing.license = existing.license.take().or_else(|| node.license.clone());
                existing.ecosystem = existing.ecosystem.take().or_else(|| node.ecosystem.clone());
                existing.source = existing.source.take().or_else(|| node.source.clone());
            })
            .or_insert(node);
    }

    pub fn add_edge(&mut self, from: impl Into<String>, to: impl Into<String>) {
        self.edges.insert((from.into(), to.into()));
    }

    pub fn add_file(&mut self, file: FileInfo) {
        self.files.insert(file.path.clone(), file);
    }

    /// Consume this builder, returning its nodes, edges, and files as a
    /// `ScanOutput` (used to capture a single file's contribution for caching).
    pub fn take_output(self) -> ScanOutput {
        ScanOutput {
            nodes: self.nodes.into_values().collect(),
            edges: self.edges.into_iter().collect(),
            files: self.files.into_values().collect(),
        }
    }

    /// Merge a previously-produced (or cached) `ScanOutput` into this builder.
    pub fn merge(&mut self, output: &ScanOutput) {
        for node in &output.nodes {
            self.add_node(node.clone());
        }
        for (from, to) in &output.edges {
            self.add_edge(from.clone(), to.clone());
        }
        for file in &output.files {
            self.add_file(file.clone());
        }
    }

    /// After all files are merged, wire up workspace membership: mark workspace
    /// roots, link each member package to its root, and tag members with the
    /// workspace they belong to. Cross-file, so it runs once over the whole set.
    pub fn resolve_workspaces(&mut self) {
        // Map a package node id → the directory of the manifest that defines it.
        let pkg_dir: BTreeMap<String, String> = self
            .files
            .values()
            .filter_map(|f| {
                let pkg = f.package.clone()?;
                Some((pkg, parent_dir(&f.path)))
            })
            .collect();

        // Collect (root_pkg_id, member_dir) pairs from every workspace manifest.
        let mut links: Vec<(String, String)> = Vec::new();
        let mut roots: Vec<String> = Vec::new();
        for file in self.files.values() {
            if file.workspace_members.is_empty() {
                continue;
            }
            let Some(root_pkg) = file.package.clone() else {
                continue;
            };
            roots.push(root_pkg.clone());
            let base = parent_dir(&file.path);
            for member_glob in &file.workspace_members {
                let member_dir = join_rel(&base, member_glob);
                for (pkg_id, dir) in &pkg_dir {
                    if pkg_id != &root_pkg && glob_dir_matches(&member_dir, dir) {
                        links.push((root_pkg.clone(), pkg_id.clone()));
                    }
                }
            }
        }

        for root in roots {
            if let Some(n) = self.nodes.get_mut(&root) {
                n.is_workspace = true;
                n.workspace = Some(root.clone());
            }
        }
        for (root, member) in links {
            self.add_edge(root.clone(), member.clone());
            if let Some(n) = self.nodes.get_mut(&member) {
                n.workspace = Some(root);
            }
        }
    }

    /// Resolve Rust dependency versions and internal/external classification
    /// using the workspace `Cargo.lock`:
    ///
    ///   * every `crates.io` external node gets its loose manifest requirement
    ///     replaced by the concrete version cargo actually locked (the old
    ///     requirement is preserved in `req`);
    ///   * dependencies that are really *internal* crates (a workspace member or
    ///     a path/git crate that we also scanned as a package) are reclassified:
    ///     their external node is dropped and the edges are rewired onto the
    ///     internal package node so they no longer masquerade as crates.io deps.
    pub fn resolve_rust_dependencies(&mut self, root: &Path) {
        let lock = scan::load_rust_lock(root);

        // Internal rust crates we scanned, by crate name → node id.
        let internal_ids: BTreeMap<String, String> = self
            .nodes
            .values()
            .filter(|n| n.category == Category::Internal && n.ecosystem.as_deref() == Some("rust"))
            .map(|n| (n.label.clone(), n.id.clone()))
            .collect();

        let mut reclassify: Vec<String> = Vec::new();
        for node in self.nodes.values_mut() {
            let Some(name) = node.id.strip_prefix("ext:crates.io:").map(String::from) else {
                continue;
            };
            // A dependency naming one of our own crates is an internal edge.
            if internal_ids.contains_key(&name) {
                reclassify.push(node.id.clone());
                continue;
            }
            // Otherwise it's a real external crate: swap the requirement for the
            // concrete locked version, keeping the requirement in `req`.
            if let Some(lock) = &lock
                && let Some(resolved) = lock.resolve(&name, node.version.as_deref())
                && node.version.as_deref() != Some(resolved.as_str())
            {
                node.req = node.version.take();
                node.version = Some(resolved);
            }
        }

        // Rewire edges off each reclassified external node onto the internal one,
        // then drop the now-empty external node.
        for ext_id in reclassify {
            let name = ext_id
                .strip_prefix("ext:crates.io:")
                .expect("ext id")
                .to_string();
            let Some(int_id) = internal_ids.get(&name).cloned() else {
                continue;
            };
            let moved: Vec<(String, String)> = self
                .edges
                .iter()
                .filter(|(from, _)| from == &ext_id)
                .cloned()
                .collect();
            for (from, to) in moved {
                self.edges.remove(&(from, to.clone()));
                if int_id != to {
                    self.edges.insert((int_id.clone(), to));
                }
            }
            self.nodes.remove(&ext_id);
        }
    }

    /// Internal package nodes that came from a manifest, as `(id, source_path)`.
    pub fn internal_packages(&self) -> Vec<(String, String)> {
        self.nodes
            .values()
            .filter(|n| n.category == Category::Internal)
            .filter_map(|n| n.source.clone().map(|src| (n.id.clone(), src)))
            .collect()
    }

    /// The internal package node whose directory most closely contains `file`
    /// (longest matching manifest directory wins).
    pub fn owner_for_file(&self, file: &str) -> Option<String> {
        let file = file.trim_start_matches("./");
        self.internal_packages()
            .into_iter()
            .filter_map(|(id, src)| {
                let dir = parent_dir(&src);
                if dir.is_empty() || file == dir || file.starts_with(&format!("{dir}/")) {
                    Some((dir.len(), id))
                } else {
                    None
                }
            })
            .max_by_key(|(len, _)| *len)
            .map(|(_, id)| id)
    }

    pub fn add_requirement_edge(&mut self, req_id: &str, target: &str) {
        self.add_edge(req_id.to_string(), target.to_string());
    }

    pub fn root_node_id(&self) -> &'static str {
        scan::ROOT_NODE_ID
    }

    pub fn has_file(&self, path: &str) -> bool {
        self.files.contains_key(path)
    }

    /// Resolve placeholder edges left by the shell-script scanner. A script that
    /// pushes to a registry emits an edge `script::<path> → <publish node>`; here
    /// we rewrite `script::<path>` to the internal package that owns the script.
    pub fn resolve_script_owners(&mut self) {
        const MARK: &str = "script::";
        let pending: Vec<(String, String)> = self
            .edges
            .iter()
            .filter(|(from, _)| from.starts_with(MARK))
            .cloned()
            .collect();
        for (from, to) in pending {
            self.edges.remove(&(from.clone(), to.clone()));
            let owner = self
                .owner_for_file(&from[MARK.len()..])
                .unwrap_or_else(|| scan::ROOT_NODE_ID.to_string());
            self.edges.insert((owner, to));
        }
    }

    pub fn finish(self, root: &Path) -> AnalysisGraph {
        // Drop dangling edges so the view never references a missing node.
        let edges = self
            .edges
            .into_iter()
            .filter(|(f, t)| self.nodes.contains_key(f) && self.nodes.contains_key(t))
            .map(|(from, to)| Edge { from, to })
            .collect();

        AnalysisGraph {
            root: root.display().to_string(),
            generated_at: chrono::Utc::now().to_rfc3339(),
            nodes: self.nodes.into_values().collect(),
            edges,
            files: self.files.into_values().collect(),
        }
    }
}

/// Directory portion of a relative path (`""` for a top-level file).
fn parent_dir(path: &str) -> String {
    match Path::new(path).parent() {
        Some(p) => p.to_string_lossy().replace('\\', "/"),
        None => String::new(),
    }
}

/// Join a workspace base dir with a member glob, normalizing separators.
fn join_rel(base: &str, member: &str) -> String {
    let member = member.trim_end_matches('/');
    if base.is_empty() {
        member.to_string()
    } else {
        format!("{base}/{member}")
    }
}

/// Does a workspace member pattern (supporting a trailing `*`) match `dir`?
fn glob_dir_matches(pattern: &str, dir: &str) -> bool {
    if let Some(prefix) = pattern.strip_suffix("/*") {
        // `crates/*` matches `crates/<one-segment>`.
        match dir.strip_prefix(prefix).and_then(|r| r.strip_prefix('/')) {
            Some(rest) => !rest.is_empty() && !rest.contains('/'),
            None => false,
        }
    } else if pattern == "*" {
        !dir.is_empty() && !dir.contains('/')
    } else {
        pattern == dir
    }
}

/// Optional requirements/trace inputs for [`analyze`].
#[derive(Default)]
pub struct RequirementInputs<'a> {
    pub requirements_file: Option<&'a Path>,
    pub trace_file: Option<&'a Path>,
}

/// Build the dependency graph for `root`, using `cfg` to discover publish points.
pub fn analyze(
    root: &Path,
    cfg: &CiabattaConfig,
    inputs: &RequirementInputs<'_>,
) -> Result<AnalysisGraph> {
    let mut builder = GraphBuilder::default();

    // Every repository gets a root node that other things can anchor to.
    let root_label = root
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "project".to_string());
    builder.add_node(Node {
        id: scan::ROOT_NODE_ID.to_string(),
        label: root_label,
        category: Category::Internal,
        ecosystem: Some("repository".to_string()),
        ..Default::default()
    });

    // Reuse cached parses of unchanged files via a content-hash cache kept in
    // .ciabatta/.cache, so re-running analyze only re-parses what changed.
    let cache_dir = root
        .join(crate::config::CIABATTA_DIR)
        .join(cache::CACHE_DIR);
    let mut file_cache = cache::Cache::load(&cache_dir);

    scan::scan_manifests(root, &mut builder, &mut file_cache)?;
    scan::scan_config_scripts(cfg, root, &mut builder, &mut file_cache);
    scan::scan_publish_points(cfg, &mut builder);
    builder.resolve_workspaces();
    builder.resolve_rust_dependencies(root);
    builder.resolve_script_owners();
    requirements::apply(&mut builder, inputs)?;

    if let Err(e) = file_cache.save() {
        eprintln!("warning: failed to write analyze cache: {e}");
    }
    eprintln!(
        "cache: {} reused, {} parsed (.ciabatta/{}/{})",
        file_cache.hits(),
        file_cache.misses(),
        cache::CACHE_DIR,
        cache::CACHE_FILE,
    );

    Ok(builder.finish(root))
}

/// Best-effort: query the OSV database for known vulnerabilities affecting the
/// external dependencies, annotating the graph in place. Network failures are
/// returned as errors for the caller to downgrade to a warning.
pub async fn check_vulnerabilities(graph: &mut AnalysisGraph) -> Result<()> {
    use serde_json::json;

    // Build OSV queries for external nodes with a supported ecosystem + version.
    let mut indices = Vec::new();
    let mut queries = Vec::new();
    for (i, node) in graph.nodes.iter().enumerate() {
        if node.category != Category::External {
            continue;
        }
        let (Some(eco), Some(ver)) = (osv_ecosystem(node), node.version.as_deref()) else {
            continue;
        };
        let version = normalize_version(ver);
        if version.is_empty() {
            continue;
        }
        indices.push(i);
        queries.push(json!({
            "package": { "name": node.label, "ecosystem": eco },
            "version": version,
        }));
    }

    if queries.is_empty() {
        return Ok(());
    }

    let client = reqwest::Client::builder().build()?;
    let resp = client
        .post("https://api.osv.dev/v1/querybatch")
        .json(&json!({ "queries": queries }))
        .send()
        .await?
        .error_for_status()?;

    let body: OsvBatchResponse = resp.json().await?;
    for (slot, result) in indices.iter().zip(body.results.iter()) {
        graph.nodes[*slot].vulnerabilities = result
            .vulns
            .iter()
            .map(|v| Vulnerability {
                id: v.id.clone(),
                summary: v.summary.clone(),
            })
            .collect();
    }
    Ok(())
}

#[derive(Deserialize, Default)]
struct OsvBatchResponse {
    #[serde(default)]
    results: Vec<OsvResult>,
}

#[derive(Deserialize, Default)]
struct OsvResult {
    #[serde(default)]
    vulns: Vec<OsvVuln>,
}

#[derive(Deserialize)]
struct OsvVuln {
    id: String,
    #[serde(default)]
    summary: Option<String>,
}

/// Map our ecosystem label to the name OSV expects, or `None` if unsupported.
fn osv_ecosystem(node: &Node) -> Option<&'static str> {
    match node.ecosystem.as_deref() {
        Some("crates.io") => Some("crates.io"),
        Some("npm") => Some("npm"),
        Some("pip") => Some("PyPI"),
        _ => None,
    }
}

/// Strip requirement operators so a bare version remains (best-effort).
fn normalize_version(v: &str) -> String {
    v.trim()
        .trim_start_matches(['^', '~', '>', '<', '=', ' '])
        .trim()
        .to_string()
}
