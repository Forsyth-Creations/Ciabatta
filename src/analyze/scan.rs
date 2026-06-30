//! Scanners that turn project manifests and ciabatta config into graph nodes.

use anyhow::Result;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::cache::{Cache, hash_content};
use super::{Category, FileInfo, GraphBuilder, Node, ScanOutput};
use crate::config::{CiabattaConfig, infer_registry_kind};

/// Build a `FileInfo` skeleton; `bytes`/`hash` are filled in by `scan_manifests`.
fn file_info(
    rel: &str,
    kind: &str,
    ecosystem: &str,
    package: Option<String>,
    members: Vec<String>,
) -> FileInfo {
    FileInfo {
        path: rel.to_string(),
        kind: kind.to_string(),
        ecosystem: ecosystem.to_string(),
        bytes: 0,
        hash: String::new(),
        package,
        workspace_members: members,
    }
}

/// The package node id an `add_package` owner refers to, unless it's the repo root.
fn package_id(owner: &str) -> Option<String> {
    (owner != ROOT_NODE_ID).then(|| owner.to_string())
}

/// Id of the synthetic node representing the repository itself.
pub const ROOT_NODE_ID: &str = "int:root";

/// Directories never worth descending into when looking for manifests.
const SKIP_DIRS: &[&str] = &[
    "node_modules",
    "target",
    "dist",
    "build",
    "vendor",
    ".git",
    ".yarn",
    ".turbo",
    ".ciabatta",
];

const MAX_DEPTH: usize = 4;

/// Walk the tree (bounded depth, skipping vendor dirs) and scan every manifest
/// and shell script, reusing cached parses for files whose contents are
/// unchanged. The `.ciabatta/` directory is also swept for `*.sh` (login/publish
/// scripts live there even though it's otherwise skipped).
pub fn scan_manifests(root: &Path, builder: &mut GraphBuilder, cache: &mut Cache) -> Result<()> {
    let mut paths = find_scan_targets(root, 0);
    paths.extend(shell_scripts_in(&root.join(crate::config::CIABATTA_DIR)));
    paths.sort();
    paths.dedup();
    for path in paths {
        scan_one(&path, root, builder, cache);
    }
    Ok(())
}

/// Scan shell scripts referenced by the ciabatta config (recipe `bash_script`,
/// stage commands ending in `.sh`, and registry `login_script`) that weren't
/// already picked up by the directory sweep.
pub fn scan_config_scripts(
    cfg: &CiabattaConfig,
    root: &Path,
    builder: &mut GraphBuilder,
    cache: &mut Cache,
) {
    for rel in config_script_paths(cfg) {
        if builder.has_file(&rel) {
            continue;
        }
        let path = root.join(&rel);
        if path.is_file() {
            scan_one(&path, root, builder, cache);
        }
    }
}

/// Read, hash, (cache-)scan, and merge a single file into the builder.
fn scan_one(path: &Path, root: &Path, builder: &mut GraphBuilder, cache: &mut Cache) {
    let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
    let rel = path
        .strip_prefix(root)
        .unwrap_or(path)
        .display()
        .to_string()
        .replace('\\', "/");

    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("warning: failed to read {rel}: {e}");
            return;
        }
    };
    let hash = hash_content(&content);

    let output = match cache.get(&rel, &hash) {
        Some(cached) => cached,
        None => {
            let parsed = match name {
                "Cargo.toml" => scan_cargo(&content, &rel),
                "package.json" => scan_package_json(&content, &rel),
                "requirements.txt" => scan_requirements(&content, &rel),
                "pyproject.toml" => scan_pyproject(&content, &rel),
                "Dockerfile" => scan_dockerfile(&content, &rel),
                ".gitlab-ci.yml" => scan_gitlab_ci(&content, &rel),
                _ if name.ends_with(".sh") => scan_shell_script(&content, &rel),
                _ => Ok(ScanOutput::default()),
            };
            let mut out = parsed.unwrap_or_else(|e| {
                // A single malformed manifest shouldn't abort the analysis.
                eprintln!("warning: failed to scan {rel}: {e}");
                ScanOutput::default()
            });
            // Stamp file metadata that's known here (not in the scanner).
            for file in &mut out.files {
                file.bytes = content.len() as u64;
                file.hash = hash.clone();
            }
            out
        }
    };

    cache.put(&rel, hash, output.clone());
    builder.merge(&output);
}

fn is_scan_target(name: &str) -> bool {
    matches!(
        name,
        "Cargo.toml"
            | "package.json"
            | "requirements.txt"
            | "pyproject.toml"
            | "Dockerfile"
            | ".gitlab-ci.yml"
    ) || name.ends_with(".sh")
}

fn find_scan_targets(dir: &Path, depth: usize) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return found;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
            if depth < MAX_DEPTH && !SKIP_DIRS.contains(&name) && !name.starts_with('.') {
                found.extend(find_scan_targets(&path, depth + 1));
            }
        } else if let Some(name) = path.file_name().and_then(|s| s.to_str())
            && is_scan_target(name)
        {
            found.push(path);
        }
    }
    found
}

/// Top-level `*.sh` files directly inside `dir` (used to sweep `.ciabatta/`).
fn shell_scripts_in(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file() && p.extension().is_some_and(|e| e == "sh"))
        .collect()
}

/// Shell-script paths referenced by the config, relative to the project root.
fn config_script_paths(cfg: &CiabattaConfig) -> Vec<String> {
    let mut paths = Vec::new();
    let mut push = |s: &str| {
        let s = s.trim_start_matches("./");
        if s.ends_with(".sh") {
            paths.push(s.to_string());
        }
    };
    for reg in cfg.registries.values() {
        if let Some(script) = &reg.login_script {
            push(script);
        }
    }
    for entry in cfg.recipes.values() {
        let mut recipes = vec![entry.push_recipe()];
        if let Some(pull) = entry.pull_recipe() {
            recipes.push(pull);
        }
        for recipe in recipes {
            if let Some(s) = &recipe.bash_script {
                push(s);
            }
            // Stage commands may invoke a script, e.g. `./scripts/push.sh`.
            for cmd in [&recipe.login, &recipe.pre, &recipe.main, &recipe.post]
                .into_iter()
                .flatten()
            {
                for token in cmd.split_whitespace() {
                    push(token);
                }
            }
        }
    }
    paths
}

/// Add an internal package node, an external node per dependency, and an edge
/// from each external dep to the package that declares it. Returns the package
/// node id that dependencies should attach to.
fn add_package(
    builder: &mut GraphBuilder,
    ecosystem: &str,
    pkg_name: Option<&str>,
    version: Option<String>,
    license: Option<String>,
    source: &str,
) -> String {
    match pkg_name {
        Some(name) => {
            let id = format!("int:{ecosystem}:{name}");
            builder.add_node(Node {
                id: id.clone(),
                label: name.to_string(),
                category: Category::Internal,
                ecosystem: Some(ecosystem.to_string()),
                version,
                license,
                source: Some(source.to_string()),
                ..Default::default()
            });
            // A named package belongs to the repository.
            builder.add_edge(id.clone(), ROOT_NODE_ID);
            id
        }
        None => ROOT_NODE_ID.to_string(),
    }
}

fn add_dependency(
    builder: &mut GraphBuilder,
    ecosystem: &str,
    name: &str,
    version: Option<String>,
    owner_id: &str,
) {
    let id = format!("ext:{ecosystem}:{name}");
    builder.add_node(Node {
        id: id.clone(),
        label: name.to_string(),
        category: Category::External,
        ecosystem: Some(ecosystem.to_string()),
        version,
        ..Default::default()
    });
    builder.add_edge(id, owner_id.to_string());
}

// ─── Cargo ──────────────────────────────────────────────────────────────────

#[derive(serde::Deserialize)]
struct CargoManifest {
    package: Option<CargoPackage>,
    workspace: Option<CargoWorkspace>,
    #[serde(default)]
    dependencies: BTreeMap<String, toml::Value>,
    #[serde(rename = "dev-dependencies", default)]
    dev_dependencies: BTreeMap<String, toml::Value>,
    #[serde(rename = "build-dependencies", default)]
    build_dependencies: BTreeMap<String, toml::Value>,
}

#[derive(serde::Deserialize)]
struct CargoPackage {
    name: String,
    version: Option<String>,
    license: Option<String>,
    publish: Option<toml::Value>,
}

#[derive(serde::Deserialize)]
struct CargoWorkspace {
    #[serde(default)]
    members: Vec<String>,
}

fn scan_cargo(content: &str, rel: &str) -> Result<ScanOutput> {
    let manifest: CargoManifest = toml::from_str(content)?;
    let pkg = manifest.package.as_ref();
    let mut b = GraphBuilder::default();

    let owner = add_package(
        &mut b,
        "rust",
        pkg.map(|p| p.name.as_str()),
        pkg.and_then(|p| p.version.clone()),
        pkg.and_then(|p| p.license.clone()),
        rel,
    );

    for deps in [
        &manifest.dependencies,
        &manifest.dev_dependencies,
        &manifest.build_dependencies,
    ] {
        for (name, spec) in deps {
            add_dependency(&mut b, "crates.io", name, cargo_dep_version(spec), &owner);
        }
    }

    // A publishable crate publishes to crates.io.
    if let Some(p) = pkg
        && !matches!(p.publish, Some(toml::Value::Boolean(false)))
    {
        let pub_id = "pub:crates.io".to_string();
        b.add_node(Node {
            id: pub_id.clone(),
            label: "crates.io".to_string(),
            category: Category::Publish,
            ecosystem: Some("crates.io".to_string()),
            source: Some(rel.to_string()),
            ..Default::default()
        });
        b.add_edge(owner.clone(), pub_id);
    }

    let members = manifest.workspace.map(|w| w.members).unwrap_or_default();
    let mut out = b.take_output();
    out.files.push(file_info(
        rel,
        "Cargo.toml",
        "rust",
        package_id(&owner),
        members,
    ));
    Ok(out)
}

fn cargo_dep_version(spec: &toml::Value) -> Option<String> {
    match spec {
        toml::Value::String(s) => Some(s.clone()),
        toml::Value::Table(t) => t.get("version").and_then(|v| v.as_str()).map(String::from),
        _ => None,
    }
}

// ─── npm ────────────────────────────────────────────────────────────────────

#[derive(serde::Deserialize)]
struct PackageJson {
    name: Option<String>,
    version: Option<String>,
    license: Option<String>,
    #[serde(default)]
    dependencies: BTreeMap<String, String>,
    #[serde(rename = "devDependencies", default)]
    dev_dependencies: BTreeMap<String, String>,
    workspaces: Option<NpmWorkspaces>,
}

/// npm/yarn `workspaces` is either an array of globs or `{ "packages": [...] }`.
#[derive(serde::Deserialize)]
#[serde(untagged)]
enum NpmWorkspaces {
    List(Vec<String>),
    Object {
        #[serde(default)]
        packages: Vec<String>,
    },
}

impl NpmWorkspaces {
    fn members(self) -> Vec<String> {
        match self {
            NpmWorkspaces::List(v) => v,
            NpmWorkspaces::Object { packages } => packages,
        }
    }
}

fn scan_package_json(content: &str, rel: &str) -> Result<ScanOutput> {
    let pkg: PackageJson = serde_json::from_str(content)?;
    let mut b = GraphBuilder::default();
    let owner = add_package(
        &mut b,
        "npm",
        pkg.name.as_deref(),
        pkg.version.clone(),
        pkg.license.clone(),
        rel,
    );
    for deps in [&pkg.dependencies, &pkg.dev_dependencies] {
        for (name, version) in deps {
            add_dependency(&mut b, "npm", name, Some(version.clone()), &owner);
        }
    }
    let members = pkg
        .workspaces
        .map(NpmWorkspaces::members)
        .unwrap_or_default();
    let mut out = b.take_output();
    out.files.push(file_info(
        rel,
        "package.json",
        "npm",
        package_id(&owner),
        members,
    ));
    Ok(out)
}

// ─── pip ────────────────────────────────────────────────────────────────────

fn scan_requirements(content: &str, rel: &str) -> Result<ScanOutput> {
    let mut b = GraphBuilder::default();
    for line in content.lines() {
        let line = line.split('#').next().unwrap_or("").trim();
        if line.is_empty() || line.starts_with('-') {
            continue;
        }
        let (name, version) = split_pip_requirement(line);
        add_dependency(&mut b, "pip", name, version, ROOT_NODE_ID);
    }
    let mut out = b.take_output();
    out.files.push(file_info(
        rel,
        "requirements.txt",
        "python",
        None,
        Vec::new(),
    ));
    Ok(out)
}

#[derive(serde::Deserialize)]
struct PyProject {
    project: Option<PyProjectTable>,
    tool: Option<PyTool>,
}

#[derive(serde::Deserialize)]
struct PyProjectTable {
    name: Option<String>,
    version: Option<String>,
    #[serde(default)]
    dependencies: Vec<String>,
}

#[derive(serde::Deserialize)]
struct PyTool {
    uv: Option<PyUv>,
}

#[derive(serde::Deserialize)]
struct PyUv {
    workspace: Option<PyUvWorkspace>,
}

#[derive(serde::Deserialize)]
struct PyUvWorkspace {
    #[serde(default)]
    members: Vec<String>,
}

fn scan_pyproject(content: &str, rel: &str) -> Result<ScanOutput> {
    let parsed: PyProject = toml::from_str(content)?;

    // uv workspaces live under [tool.uv.workspace].
    let members = parsed
        .tool
        .and_then(|t| t.uv)
        .and_then(|u| u.workspace)
        .map(|w| w.members)
        .unwrap_or_default();

    let mut b = GraphBuilder::default();
    let owner = match &parsed.project {
        Some(project) => {
            let owner = add_package(
                &mut b,
                "python",
                project.name.as_deref(),
                project.version.clone(),
                None,
                rel,
            );
            for req in &project.dependencies {
                let (name, version) = split_pip_requirement(req);
                add_dependency(&mut b, "pip", name, version, &owner);
            }
            owner
        }
        None => ROOT_NODE_ID.to_string(),
    };

    let mut out = b.take_output();
    out.files.push(file_info(
        rel,
        "pyproject.toml",
        "python",
        package_id(&owner),
        members,
    ));
    Ok(out)
}

/// Split `flask>=2.0` into ("flask", Some(">=2.0")).
fn split_pip_requirement(req: &str) -> (&str, Option<String>) {
    let req = req.trim();
    match req.find(['=', '>', '<', '~', '!', ' ', '[']) {
        Some(idx) => {
            let name = req[..idx].trim();
            let ver = req[idx..].trim();
            (name, (!ver.is_empty()).then(|| ver.to_string()))
        }
        None => (req, None),
    }
}

// ─── Docker ─────────────────────────────────────────────────────────────────

fn scan_dockerfile(content: &str, rel: &str) -> Result<ScanOutput> {
    let mut b = GraphBuilder::default();
    // Track build-stage aliases so `FROM builder` doesn't become a dependency.
    let mut stages = std::collections::HashSet::new();
    for line in content.lines() {
        let line = line.trim();
        let lower = line.to_ascii_lowercase();
        let Some(rest) = lower.strip_prefix("from ") else {
            continue;
        };
        // Preserve original case for the image name.
        let rest_orig = line[5..].trim();
        let mut parts = rest_orig.split_whitespace();
        let Some(image) = parts.next() else {
            continue;
        };
        // `FROM x AS y` → remember y as a stage alias.
        if let Some(pos) = lower.find(" as ") {
            stages.insert(rest[pos - 4..].trim().to_string()); // best-effort
            let alias = line[5..].to_ascii_lowercase();
            if let Some(a) = alias.split(" as ").nth(1) {
                stages.insert(a.trim().to_string());
            }
        }
        if stages.contains(&image.to_ascii_lowercase()) {
            continue;
        }
        let (name, tag) = split_image(image);
        add_dependency(&mut b, "dockerhub", &name, tag, ROOT_NODE_ID);
    }
    let mut out = b.take_output();
    out.files
        .push(file_info(rel, "Dockerfile", "docker", None, Vec::new()));
    Ok(out)
}

/// Split a container image reference into (name, tag).
fn split_image(image: &str) -> (String, Option<String>) {
    match image.rsplit_once(':') {
        // A '/' after the ':' means it was a registry port, not a tag.
        Some((n, t)) if !n.is_empty() && !t.contains('/') => (n.to_string(), Some(t.to_string())),
        _ => (image.to_string(), None),
    }
}

/// Strip quotes/whitespace from a YAML scalar; reject empties, comments, and
/// values containing CI variables (`$VAR` / `${VAR}`).
fn clean_image(s: &str) -> Option<String> {
    let s = s.trim().trim_matches('"').trim_matches('\'').trim();
    if s.is_empty() || s.starts_with('#') || s.contains('$') {
        return None;
    }
    Some(s.to_string())
}

// ─── GitLab CI ───────────────────────────────────────────────────────────────

/// Extract container images referenced by `.gitlab-ci.yml` (`image:` defaults
/// and per-job, and `services:`) as dockerhub dependencies. This is a focused
/// line scanner, not a full YAML parser.
fn scan_gitlab_ci(content: &str, rel: &str) -> Result<ScanOutput> {
    let mut images: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let lines: Vec<&str> = content.lines().collect();
    let indent = |l: &str| l.len() - l.trim_start().len();

    for (i, raw) in lines.iter().enumerate() {
        let line = raw.trim_start();

        // `image:` — either an inline scalar or an object with a `name:` key.
        if let Some(rest) = line.strip_prefix("image:") {
            match clean_image(rest) {
                Some(img) => {
                    images.insert(img);
                }
                None => {
                    // Object form: find a more-indented `name:` below.
                    for next in &lines[i + 1..] {
                        if next.trim().is_empty() {
                            continue;
                        }
                        if indent(next) <= indent(raw) {
                            break;
                        }
                        if let Some(n) = next.trim_start().strip_prefix("name:")
                            && let Some(img) = clean_image(n)
                        {
                            images.insert(img);
                            break;
                        }
                    }
                }
            }
        }

        // `services:` — a list of images (each `- image` or `- name: image`).
        if line.trim_end() == "services:" {
            for next in &lines[i + 1..] {
                if next.trim().is_empty() {
                    continue;
                }
                if indent(next) <= indent(raw) {
                    break;
                }
                if let Some(item) = next.trim_start().strip_prefix('-') {
                    let item = item.trim();
                    let candidate = item.strip_prefix("name:").unwrap_or(item);
                    if let Some(img) = clean_image(candidate) {
                        images.insert(img);
                    }
                }
            }
        }
    }

    let mut b = GraphBuilder::default();
    for image in images {
        let (name, tag) = split_image(&image);
        add_dependency(&mut b, "dockerhub", &name, tag, ROOT_NODE_ID);
    }
    let mut out = b.take_output();
    out.files.push(file_info(
        rel,
        ".gitlab-ci.yml",
        "gitlab-ci",
        None,
        Vec::new(),
    ));
    Ok(out)
}

// ─── Shell scripts (publish detection) ───────────────────────────────────────

/// A publish target discovered in a script: (node id, label, ecosystem).
type PubTarget = (String, String, &'static str);

/// Scan a shell script for registry-push commands and turn each into a publish
/// point. The edge to the owning package is deferred (see `resolve_script_owners`)
/// because ownership is cross-file; here we emit a `script::<path>` placeholder.
fn scan_shell_script(content: &str, rel: &str) -> Result<ScanOutput> {
    let mut targets: std::collections::BTreeSet<PubTarget> = std::collections::BTreeSet::new();

    for raw in content.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        detect_push_targets(line, &mut targets);
    }

    let mut b = GraphBuilder::default();
    for (id, label, eco) in &targets {
        b.add_node(Node {
            id: id.clone(),
            label: label.clone(),
            category: Category::Publish,
            ecosystem: Some(eco.to_string()),
            source: Some(rel.to_string()),
            ..Default::default()
        });
        // Placeholder edge; rewritten to the owning package after all files load.
        b.add_edge(format!("script::{rel}"), id.clone());
    }

    let kind = std::path::Path::new(rel)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("script.sh")
        .to_string();
    let mut out = b.take_output();
    out.files
        .push(file_info(rel, &kind, "shell", None, Vec::new()));
    Ok(out)
}

/// Recognize common "publish to a registry" commands on a single line.
fn detect_push_targets(line: &str, out: &mut std::collections::BTreeSet<PubTarget>) {
    // docker / podman push <image>
    for kw in ["docker push", "podman push"] {
        if let Some(arg) = arg_after(line, kw)
            && let Some(image) = clean_image(&arg)
        {
            let host = registry_host(&image);
            let label = host.clone().unwrap_or_else(|| "Docker Hub".to_string());
            let key = host.unwrap_or_else(|| "dockerhub".to_string());
            out.insert((format!("pub:script:docker:{key}"), label, "docker"));
        }
    }
    // aws s3 cp / sync … s3://bucket/…
    if line.contains("aws s3 cp") || line.contains("aws s3 sync") {
        match s3_bucket(line) {
            Some(bucket) => out.insert((
                format!("pub:script:s3:{bucket}"),
                format!("s3://{bucket}"),
                "s3",
            )),
            None => out.insert(("pub:script:s3".into(), "S3".into(), "s3")),
        };
    }
    // cargo publish → crates.io (dedupes with the manifest-derived node)
    if line.contains("cargo publish") {
        out.insert(("pub:crates.io".into(), "crates.io".into(), "crates.io"));
    }
    // npm / yarn publish
    if line.contains("npm publish") || line.contains("yarn publish") {
        out.insert(("pub:script:npm".into(), "npm registry".into(), "npm"));
    }
    // twine upload → PyPI
    if line.contains("twine upload") {
        out.insert(("pub:script:pypi".into(), "PyPI".into(), "pip"));
    }
    // helm push … oci://host/…
    if line.contains("helm push") {
        let host = url_host(line, "oci://").unwrap_or_else(|| "OCI registry".to_string());
        out.insert((format!("pub:script:oci:{host}"), host, "oci"));
    }
    // curl upload (PUT) → e.g. Nexus / Artifactory
    if line.contains("curl")
        && (line.contains("--upload-file")
            || line.contains(" -T ")
            || line.contains("-X PUT")
            || line.contains("--request PUT"))
        && let Some(host) = url_host(line, "http://").or_else(|| url_host(line, "https://"))
    {
        out.insert((format!("pub:script:http:{host}"), host, "http"));
    }
}

/// The first non-flag argument appearing after `keyword` on a line.
fn arg_after(line: &str, keyword: &str) -> Option<String> {
    let rest = &line[line.find(keyword)? + keyword.len()..];
    rest.split_whitespace()
        .find(|t| !t.starts_with('-'))
        .map(|s| s.to_string())
}

/// The registry host of an image ref, or `None` for a Docker Hub image.
fn registry_host(image: &str) -> Option<String> {
    let first = image.split('/').next().unwrap_or(image);
    // A registry host has a dot or a port, or is localhost.
    if first.contains('.') || first.contains(':') || first == "localhost" {
        Some(first.to_string())
    } else {
        None
    }
}

/// Extract the bucket name from the first `s3://bucket/...` token on a line.
fn s3_bucket(line: &str) -> Option<String> {
    let after = &line[line.find("s3://")? + 5..];
    let bucket: String = after
        .chars()
        .take_while(|c| !c.is_whitespace() && *c != '/' && *c != '"' && *c != '\'')
        .collect();
    (!bucket.is_empty()).then_some(bucket)
}

/// Extract the host from the first `<scheme>host/...` occurrence on a line.
fn url_host(line: &str, scheme: &str) -> Option<String> {
    let after = &line[line.find(scheme)? + scheme.len()..];
    let host: String = after
        .chars()
        .take_while(|c| !c.is_whitespace() && *c != '/' && *c != '"' && *c != '\'')
        .collect();
    (!host.is_empty() && !host.contains('$')).then_some(host)
}

// ─── Publish points from ciabatta config ────────────────────────────────────

/// Each registry referenced by a recipe becomes a publish node; the recipe's
/// artifact is linked from the internal package it lives under (or the repo).
pub fn scan_publish_points(cfg: &CiabattaConfig, builder: &mut GraphBuilder) {
    for (recipe_name, entry) in &cfg.recipes {
        let recipe = entry.push_recipe();
        let Some(reg_name) = recipe.registry.as_deref() else {
            continue;
        };
        let Some(reg_cfg) = cfg.registries.get(reg_name) else {
            continue;
        };
        let kind = infer_registry_kind(reg_name, reg_cfg);
        let pub_id = format!("pub:registry:{reg_name}");
        builder.add_node(Node {
            id: pub_id.clone(),
            label: reg_name.to_string(),
            category: Category::Publish,
            ecosystem: Some(format!("{kind:?}").to_lowercase()),
            source: Some(format!("recipe:{recipe_name}")),
            // Publish points from ciabatta recipes are managed by ciabatta.
            ciabatta_managed: true,
            ..Default::default()
        });

        let owner = recipe
            .local_artifact_path
            .as_deref()
            .and_then(|p| builder.owner_for_file(p))
            .unwrap_or_else(|| ROOT_NODE_ID.to_string());
        builder.add_edge(owner, pub_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh, unique temp directory (safe under parallel test execution).
    fn unique_dir() -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("ciabatta-analyze-{}-{}", std::process::id(), id));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn root_builder() -> GraphBuilder {
        let mut b = GraphBuilder::default();
        b.add_node(Node {
            id: ROOT_NODE_ID.to_string(),
            label: "root".into(),
            category: Category::Internal,
            ecosystem: Some("repository".into()),
            ..Default::default()
        });
        b
    }

    fn build(setup: impl FnOnce(&Path)) -> super::super::AnalysisGraph {
        let dir = unique_dir();
        setup(&dir);
        let mut b = root_builder();
        let mut cache = Cache::load(&dir.join(".cache"));
        scan_manifests(&dir, &mut b, &mut cache).unwrap();
        let g = b.finish(&dir);
        let _ = std::fs::remove_dir_all(&dir);
        g
    }

    #[test]
    fn scans_cargo_manifest() {
        let g = build(|dir| {
            std::fs::write(
                dir.join("Cargo.toml"),
                r#"
[package]
name = "demo"
version = "0.2.0"
license = "MIT"

[dependencies]
serde = "1"
clap = { version = "4.5", features = ["derive"] }
"#,
            )
            .unwrap();
        });

        let pkg = g.nodes.iter().find(|n| n.label == "demo").unwrap();
        assert_eq!(pkg.category, Category::Internal);
        assert_eq!(pkg.version.as_deref(), Some("0.2.0"));

        let serde = g.nodes.iter().find(|n| n.label == "serde").unwrap();
        assert_eq!(serde.category, Category::External);
        assert_eq!(serde.ecosystem.as_deref(), Some("crates.io"));

        let clap = g.nodes.iter().find(|n| n.label == "clap").unwrap();
        assert_eq!(clap.version.as_deref(), Some("4.5"));

        // Publishable crate → crates.io publish node.
        assert!(g.nodes.iter().any(|n| n.id == "pub:crates.io"));
        // Edge external → package exists.
        assert!(
            g.edges
                .iter()
                .any(|e| e.from == "ext:crates.io:serde" && e.to == "int:rust:demo")
        );
    }

    #[test]
    fn cargo_publish_false_has_no_publish_node() {
        let g = build(|dir| {
            std::fs::write(
                dir.join("Cargo.toml"),
                "[package]\nname = \"x\"\nversion = \"1.0.0\"\npublish = false\n",
            )
            .unwrap();
        });
        assert!(!g.nodes.iter().any(|n| n.id == "pub:crates.io"));
    }

    #[test]
    fn scans_package_json_and_dockerfile() {
        let g = build(|dir| {
            std::fs::write(
                dir.join("package.json"),
                r#"{ "name": "web", "version": "1.2.3",
                    "dependencies": { "react": "^18.0.0" },
                    "devDependencies": { "vite": "^5.0.0" } }"#,
            )
            .unwrap();
            std::fs::write(
                dir.join("Dockerfile"),
                "FROM rust:1-bookworm AS builder\nFROM debian:bookworm\n",
            )
            .unwrap();
        });

        assert!(
            g.nodes
                .iter()
                .any(|n| n.label == "react" && n.ecosystem.as_deref() == Some("npm"))
        );
        assert!(g.nodes.iter().any(|n| n.label == "web"));
        // Dockerhub base image (with build-stage alias filtered out).
        assert!(
            g.nodes
                .iter()
                .any(|n| n.label == "rust" && n.ecosystem.as_deref() == Some("dockerhub"))
        );
        assert!(g.nodes.iter().any(|n| n.label == "debian"));
        assert!(!g.nodes.iter().any(|n| n.label == "builder"));
    }

    #[test]
    fn scans_gitlab_ci_images_and_services() {
        let g = build(|dir| {
            std::fs::write(
                dir.join(".gitlab-ci.yml"),
                r#"
image: rust:1.83

build:
  image:
    name: node:20-alpine
    entrypoint: [""]
  services:
    - postgres:15
    - name: redis:7
  script:
    - cargo build

deploy:
  image: "$CI_REGISTRY_IMAGE:latest"
"#,
            )
            .unwrap();
        });

        let images: Vec<&str> = g
            .nodes
            .iter()
            .filter(|n| n.ecosystem.as_deref() == Some("dockerhub"))
            .map(|n| n.label.as_str())
            .collect();
        assert!(images.contains(&"rust"));
        assert!(images.contains(&"node"));
        assert!(images.contains(&"postgres"));
        assert!(images.contains(&"redis"));
        // CI-variable images are skipped.
        assert!(!g.nodes.iter().any(|n| n.label.contains("CI_REGISTRY")));
        // The file is tracked.
        assert!(
            g.files
                .iter()
                .any(|f| f.kind == ".gitlab-ci.yml" && f.ecosystem == "gitlab-ci")
        );
    }

    #[test]
    fn scans_shell_script_publish_targets() {
        let dir = unique_dir();
        std::fs::write(
            dir.join("Cargo.toml"),
            "[package]\nname = \"svc\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        std::fs::create_dir_all(dir.join("scripts")).unwrap();
        std::fs::write(
            dir.join("scripts/publish.sh"),
            r#"#!/usr/bin/env bash
set -e
docker push registry.example.com/team/svc:latest
aws s3 cp dist/app.tar.gz s3://my-artifacts/app.tar.gz
cargo publish
docker push $SECRET_IMAGE   # CI variable, ignored
"#,
        )
        .unwrap();

        let mut b = root_builder();
        let mut cache = Cache::load(&dir.join(".cache"));
        scan_manifests(&dir, &mut b, &mut cache).unwrap();
        b.resolve_script_owners();
        let g = b.finish(&dir);
        let _ = std::fs::remove_dir_all(&dir);

        let pubs: Vec<&str> = g
            .nodes
            .iter()
            .filter(|n| n.category == Category::Publish)
            .map(|n| n.label.as_str())
            .collect();
        assert!(pubs.contains(&"registry.example.com")); // docker registry host
        assert!(pubs.contains(&"s3://my-artifacts"));
        assert!(pubs.contains(&"crates.io"));
        // The CI-variable image is not turned into a publish point.
        assert!(!pubs.iter().any(|p| p.contains("SECRET")));
        // Script publish points are NOT flagged as ciabatta-managed.
        assert!(
            g.nodes
                .iter()
                .filter(|n| n.id.starts_with("pub:script:"))
                .all(|n| !n.ciabatta_managed)
        );
        // The publish point is wired to the package that owns the script.
        assert!(
            g.edges
                .iter()
                .any(|e| e.from == "int:rust:svc" && e.to.starts_with("pub:script:docker"))
        );
        // The script is tracked as a file.
        assert!(
            g.files
                .iter()
                .any(|f| f.path == "scripts/publish.sh" && f.ecosystem == "shell")
        );
    }

    #[test]
    fn pip_requirement_splitting() {
        assert_eq!(
            split_pip_requirement("flask>=2.0"),
            ("flask", Some(">=2.0".into()))
        );
        assert_eq!(split_pip_requirement("requests"), ("requests", None));
    }

    #[test]
    fn detects_npm_workspace_membership_and_files() {
        let dir = unique_dir();
        std::fs::write(
            dir.join("package.json"),
            r#"{ "name": "root", "version": "1.0.0",
                "workspaces": ["frontend", "src"] }"#,
        )
        .unwrap();
        std::fs::create_dir_all(dir.join("frontend")).unwrap();
        std::fs::write(
            dir.join("frontend/package.json"),
            r#"{ "name": "web", "version": "1.0.0" }"#,
        )
        .unwrap();
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(
            dir.join("src/package.json"),
            r#"{ "name": "lib", "version": "1.0.0" }"#,
        )
        .unwrap();

        let mut b = root_builder();
        let mut cache = Cache::load(&dir.join(".cache"));
        scan_manifests(&dir, &mut b, &mut cache).unwrap();
        b.resolve_workspaces();
        let g = b.finish(&dir);
        let _ = std::fs::remove_dir_all(&dir);

        // Root package is flagged as a workspace.
        let root = g.nodes.iter().find(|n| n.label == "root").unwrap();
        assert!(root.is_workspace);

        // Members are tagged with their workspace and linked from the root.
        let web = g.nodes.iter().find(|n| n.label == "web").unwrap();
        assert_eq!(web.workspace.as_deref(), Some("int:npm:root"));
        assert!(
            g.edges
                .iter()
                .any(|e| e.from == "int:npm:root" && e.to == "int:npm:web")
        );

        // File metadata is captured, including the declared members.
        let root_file = g.files.iter().find(|f| f.path == "package.json").unwrap();
        assert_eq!(root_file.kind, "package.json");
        assert_eq!(root_file.ecosystem, "npm");
        assert!(root_file.bytes > 0 && !root_file.hash.is_empty());
        assert_eq!(root_file.workspace_members, vec!["frontend", "src"]);
        // Three package.json files tracked.
        assert_eq!(
            g.files.iter().filter(|f| f.kind == "package.json").count(),
            3
        );
    }

    #[test]
    fn cache_reuses_unchanged_files_and_reparses_changes() {
        let dir = unique_dir();
        let cargo = dir.join("Cargo.toml");
        std::fs::write(
            &cargo,
            "[package]\nname = \"c\"\nversion = \"0.1.0\"\n[dependencies]\nserde = \"1\"\n",
        )
        .unwrap();
        let cache_dir = dir.join(".cache");

        // First run: nothing cached → a miss, then it's persisted.
        let mut c1 = Cache::load(&cache_dir);
        let mut b1 = root_builder();
        scan_manifests(&dir, &mut b1, &mut c1).unwrap();
        c1.save().unwrap();
        assert_eq!(c1.hits(), 0);
        assert_eq!(c1.misses(), 1);
        let g1 = b1.finish(&dir);

        // Second run, file unchanged: served from cache, identical result.
        let mut c2 = Cache::load(&cache_dir);
        let mut b2 = root_builder();
        scan_manifests(&dir, &mut b2, &mut c2).unwrap();
        assert_eq!(c2.hits(), 1);
        assert_eq!(c2.misses(), 0);
        let g2 = b2.finish(&dir);
        assert_eq!(g1.nodes.len(), g2.nodes.len());
        assert_eq!(g1.edges.len(), g2.edges.len());

        // Change the file: hash differs → re-parsed (miss), new version shows up.
        std::fs::write(&cargo, "[package]\nname = \"c\"\nversion = \"0.2.0\"\n").unwrap();
        let mut c3 = Cache::load(&cache_dir);
        let mut b3 = root_builder();
        scan_manifests(&dir, &mut b3, &mut c3).unwrap();
        assert_eq!(c3.hits(), 0);
        assert_eq!(c3.misses(), 1);
        let g3 = b3.finish(&dir);
        assert!(
            g3.nodes
                .iter()
                .any(|n| n.label == "c" && n.version.as_deref() == Some("0.2.0"))
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
