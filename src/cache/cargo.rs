//! `cache.cargo`: a Rust build's inputs and outputs, read from Cargo rather
//! than written out by hand.
//!
//! A multi-crate workspace is exactly where a hand-written `inputs` list goes
//! wrong. Every crate a binary is built from has to be named, with its
//! manifest, its `build.rs`, the lockfile and the `.cargo/config.toml` above it
//! — and the list is stale the day somebody adds a path dependency, which is
//! the silent kind of stale: the new crate's sources aren't in the key, so
//! changing them changes nothing, and the old binary is served.
//!
//! Cargo already knows all of it. So a workflow names the package and the
//! profile, and this module asks `cargo metadata` for the rest:
//!
//! ```yaml
//! cache:
//!   enabled: true
//!   cargo:
//!     package: api
//!     profile: release        # or debug, or ${CARGO_PROFILE:-release}
//!     artifacts: [bin]
//! ```
//!
//! The profile is named by the directory its artifacts land in — `debug` or
//! `release` — and can be read from an environment variable instead, so one
//! workflow can build either way. A variable named there keys the cache, so a
//! release build is never handed a debug build's binary.
//!
//! **Inputs** are every package directory in the path-dependency closure of
//! what was selected — the whole directory, not just `src/`, because a build
//! script or an `include_str!` can read anything in it, and an over-broad input
//! costs a rebuild where a missing one costs a wrong answer. `.gitignore` is
//! always honoured for these, so `target/` and generated files stay out. Around
//! them go the files Cargo itself reads: the workspace manifest, `Cargo.lock`,
//! and any `rust-toolchain{,.toml}` or `.cargo/config{,.toml}` between the
//! manifest and the workspace root. Registry and git dependencies are covered
//! by `Cargo.lock`, which pins them by checksum.
//!
//! **Outputs** are the artifacts of the selected packages' targets of the
//! requested kinds, under the target directory Cargo reports — so
//! `CARGO_TARGET_DIR` and `build.target-dir` are respected rather than assumed
//! away — with each platform's file naming.
//!
//! `cargo metadata` runs with `--no-deps --offline`: nothing is downloaded and
//! no registry needs to be reachable, which matters for a cache that is often
//! consulted on a machine mid-build or offline. Path dependencies are followed
//! from each package's `dependencies` list instead of from the resolve graph.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, OnceLock};
use std::time::SystemTime;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

/// The `cache.cargo` section.
#[derive(Debug, Deserialize, Serialize, Clone, Default, PartialEq)]
pub struct CargoSpec {
    /// The package to build. Omitted, the selection is what `cargo build`
    /// would build from the manifest's directory: that package, for a package
    /// manifest; the workspace's default members, for a virtual one.
    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub package: Option<String>,

    /// Several packages, for a step that builds more than one (`-p a -p b`).
    #[serde(default)]
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub packages: Vec<String>,

    /// The manifest to read, relative to the workspace root like every other
    /// cache path. Defaults to the step's own sub-workspace's `Cargo.toml`,
    /// then the root's — which is how two independent Cargo workspaces in one
    /// repository each get their own lockfile and target directory.
    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manifest: Option<String>,

    /// The profile the step builds with: `debug` (the default, as for
    /// `cargo build`) or `release` — named by the directory under `target/`
    /// its artifacts land in. Or an environment variable holding one of the
    /// two, as `$VAR`, `${VAR}` or `${VAR:-release}`: see [`ProfileRef`].
    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,

    /// A target triple, for a step that passes `--target`. Artifacts then live
    /// under `target/<triple>/<profile>/`, named the way that platform names
    /// them.
    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,

    /// Which artifacts are outputs. Empty means Cargo supplies inputs only and
    /// `outputs` is written by hand (or the step declares `no_outputs`).
    #[serde(default)]
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub artifacts: Vec<ArtifactKind>,
}

/// The profiles `cache.cargo.profile` can name, each by the directory under
/// `target/` its artifacts land in. Cargo's own name for the first is `dev`,
/// but the directory is what this section is about, and `debug` is what a
/// person looking at `target/` sees.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Profile {
    Debug,
    Release,
}

impl Profile {
    fn parse(text: &str) -> Option<Profile> {
        match text.trim() {
            "debug" => Some(Profile::Debug),
            "release" => Some(Profile::Release),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Profile::Debug => "debug",
            Profile::Release => "release",
        }
    }
}

/// What `cache.cargo.profile` says, before any variable in it is read.
#[derive(Debug, Clone, PartialEq)]
pub enum ProfileRef {
    /// `debug` or `release`, written out.
    Fixed(Profile),
    /// `$VAR`, `${VAR}` or `${VAR:-default}`: the profile is the variable's
    /// value, from the step's environment (its `.env`, the elected env
    /// profile, or the shell). Unset or empty, it is `default`, and without
    /// one `debug` — what `cargo build` builds with no flags.
    Env {
        name: String,
        default: Option<Profile>,
    },
}

impl ProfileRef {
    /// Read what was written. Anything other than the two profile names or a
    /// variable reference is an error that says which forms are accepted.
    pub fn parse(text: &str) -> Result<ProfileRef> {
        let text = text.trim();
        if let Some(profile) = Profile::parse(text) {
            return Ok(ProfileRef::Fixed(profile));
        }
        let re = regex::Regex::new(
            r"^\$(?:([A-Za-z_][A-Za-z0-9_]*)|\{([A-Za-z_][A-Za-z0-9_]*)(?::-([^}]*))?\})$",
        )
        .expect("a valid pattern");
        let Some(caps) = re.captures(text) else {
            let hint = match text {
                "dev" | "test" => " (`dev` and `test` build into target/debug: write `debug`)",
                "bench" => " (`bench` builds into target/release: write `release`)",
                _ => "",
            };
            bail!(
                "`cache.cargo.profile`: `{text}` isn't a profile — write `debug` or \
                 `release`, or name a variable holding one: `${{CARGO_PROFILE}}`, or \
                 `${{CARGO_PROFILE:-release}}` for a default{hint}"
            );
        };
        let name = caps
            .get(1)
            .or_else(|| caps.get(2))
            .map(|m| m.as_str().to_string())
            .unwrap_or_default();
        let default = match caps.get(3) {
            None => None,
            Some(m) => Some(Profile::parse(m.as_str()).with_context(|| {
                format!(
                    "`cache.cargo.profile`: the default in `{text}` is `{}` — it has to be \
                     `debug` or `release`",
                    m.as_str()
                )
            })?),
        };
        Ok(ProfileRef::Env { name, default })
    }
}

impl CargoSpec {
    /// The variable [`Self::profile`] is read from, when it names one. It
    /// keys the cache like any declared variable: the same sources built
    /// both ways are two different builds.
    pub fn profile_var(&self) -> Option<String> {
        match ProfileRef::parse(self.profile.as_deref()?) {
            Ok(ProfileRef::Env { name, .. }) => Some(name),
            _ => None,
        }
    }

    /// The profile this spec builds with. A variable is looked up in
    /// `values` — the step's environment — and then the process's own.
    pub fn resolve_profile(&self, values: &BTreeMap<String, String>) -> Result<Profile> {
        let Some(written) = self.profile.as_deref() else {
            return Ok(Profile::Debug);
        };
        match ProfileRef::parse(written)? {
            ProfileRef::Fixed(profile) => Ok(profile),
            ProfileRef::Env { name, default } => {
                let value = values
                    .get(&name)
                    .cloned()
                    .or_else(|| std::env::var(&name).ok())
                    .filter(|v| !v.trim().is_empty());
                match value {
                    None => Ok(default.unwrap_or(Profile::Debug)),
                    Some(value) => Profile::parse(&value).with_context(|| {
                        format!(
                            "`cache.cargo.profile` reads ${name}, which is `{value}` — it has \
                             to be `debug` or `release`"
                        )
                    }),
                }
            }
        }
    }
}

/// A kind of artifact a Cargo target produces.
#[derive(Debug, Deserialize, Serialize, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
pub enum ArtifactKind {
    /// An executable: `app`, `app.exe`.
    Bin,
    /// A Rust library: `libcore.rlib`.
    Lib,
    /// A C-ABI dynamic library: `libcore.so`, `libcore.dylib`, `core.dll`.
    Cdylib,
    /// A Rust dynamic library, named like a `cdylib`.
    Dylib,
    /// A static library: `libcore.a`, `core.lib`.
    Staticlib,
}

/// One package in the closure a spec resolved to.
#[derive(Debug, Clone, PartialEq)]
pub struct Package {
    pub name: String,
    /// Its directory, relative to the workspace root (`""` for the root).
    pub dir: String,
    /// Named by the spec (or selected by default), rather than reached through
    /// a path dependency.
    pub selected: bool,
}

/// What a [`CargoSpec`] means on disk, every path relative to the workspace
/// root.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Resolved {
    /// Paths to hash: each package directory in the closure, and the files
    /// Cargo reads around them.
    pub inputs: Vec<String>,
    /// Kept out of [`Self::inputs`]: the target directory, and packages nested
    /// inside the closure's directories that aren't part of it (a `fuzz/`
    /// crate, an example workspace).
    pub exclude: Vec<String>,
    /// The closure itself. A ciabatta sub-workspace holding one of these
    /// directories is not excluded from this build's inputs the way a sibling
    /// sub-workspace otherwise is: its sources are compiled into this build's
    /// artifact.
    pub packages: Vec<Package>,
    /// The artifacts [`CargoSpec::artifacts`] asked for.
    pub outputs: Vec<String>,
    /// The dep-info file rustc writes next to each output, listing every
    /// source file it read.
    pub dep_info: Vec<String>,
}

impl Resolved {
    /// Whether `rel` is a closure package directory or an ancestor of one —
    /// a ciabatta sub-workspace the closure has to reach into.
    pub fn reaches_into(&self, rel: &str) -> bool {
        self.packages
            .iter()
            .any(|p| p.dir == rel || p.dir.starts_with(&format!("{rel}/")))
    }
}

/// Work out what `spec` means for a workspace rooted at `root`.
///
/// `home` is the step's own sub-workspace, relative to `root`, which is where
/// the manifest is looked for when the spec doesn't name one.
///
/// Every failure is an error rather than an empty answer. A spec that resolves
/// to nothing would key the build on nothing, and a build keyed on nothing is
/// a hit forever.
pub fn resolve(root: &Path, home: Option<&str>, spec: &CargoSpec) -> Result<Resolved> {
    resolve_with(root, home, spec, &BTreeMap::new())
}

/// [`resolve`], reading a variable in `profile` from `values` (the step's
/// environment) before the process's own.
pub fn resolve_with(
    root: &Path,
    home: Option<&str>,
    spec: &CargoSpec,
    values: &BTreeMap<String, String>,
) -> Result<Resolved> {
    // Read first, so a mistyped profile is reported even for a spec that
    // asks for no artifacts.
    let profile = spec.resolve_profile(values)?;
    let root = root
        .canonicalize()
        .with_context(|| format!("Failed to resolve {}", root.display()))?;
    let manifest = manifest_for(&root, home, spec)?;
    let meta = metadata(&manifest)?;

    let workspace_root = canonical(&meta.workspace_root);
    if !workspace_root.starts_with(&root) {
        bail!(
            "`cache.cargo`: {} belongs to the Cargo workspace at {}, which is outside this \
             project ({}). Cached paths are relative to the project root, so the Cargo \
             workspace has to live inside it.",
            manifest.display(),
            workspace_root.display(),
            root.display(),
        );
    }

    // Packages by manifest, across every metadata read the closure needed.
    let mut known: HashMap<PathBuf, MetaPackage> = HashMap::new();
    for package in &meta.packages {
        known.insert(canonical(&package.manifest_path), package.clone());
    }

    let selected = select(&meta, &manifest, spec)?;

    // The path-dependency closure, breadth first.
    let mut closure: BTreeMap<PathBuf, bool> = BTreeMap::new();
    let mut queue: Vec<PathBuf> = selected
        .iter()
        .map(|p| canonical(&p.manifest_path))
        .collect();
    for manifest_path in &queue {
        closure.insert(manifest_path.clone(), true);
    }
    while let Some(manifest_path) = queue.pop() {
        if !known.contains_key(&manifest_path) {
            // A path dependency outside the workspace: a package of its own.
            let other = metadata(&manifest_path)?;
            for package in other.packages {
                known.insert(canonical(&package.manifest_path), package);
            }
        }
        let Some(package) = known.get(&manifest_path) else {
            bail!(
                "`cache.cargo`: a path dependency points at {}, which Cargo doesn't report \
                 as a package",
                manifest_path.display()
            );
        };
        for dep in &package.dependencies {
            let Some(path) = &dep.path else { continue };
            let dep_manifest = canonical(&path.join("Cargo.toml"));
            if !closure.contains_key(&dep_manifest) {
                closure.insert(dep_manifest.clone(), false);
                queue.push(dep_manifest);
            }
        }
    }

    let mut packages = Vec::new();
    let mut dirs: BTreeSet<PathBuf> = BTreeSet::new();
    for (manifest_path, is_selected) in &closure {
        let package = &known[manifest_path];
        let dir = manifest_path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_default();
        let rel = relative(&root, &dir).with_context(|| {
            format!(
                "`cache.cargo`: package `{}` is a path dependency at {}, outside this project — \
                 its sources can't be part of a cache key whose paths are relative to {}",
                package.name,
                dir.display(),
                root.display(),
            )
        })?;
        packages.push(Package {
            name: package.name.clone(),
            dir: rel,
            selected: *is_selected,
        });
        dirs.insert(dir);
    }
    packages.sort_by(|a, b| a.dir.cmp(&b.dir));

    // `.` rather than `""` for a package at the project root: the matcher
    // reads both as the whole tree, but only one of them reads as a path.
    let mut inputs: BTreeSet<String> = packages
        .iter()
        .map(|p| {
            if p.dir.is_empty() {
                ".".to_string()
            } else {
                p.dir.clone()
            }
        })
        .collect();
    for file in cargo_files(&root, &workspace_root, &manifest) {
        inputs.insert(relative(&root, &file)?);
    }

    let target_dir = canonical(&meta.target_directory);
    let mut exclude: BTreeSet<String> = BTreeSet::new();
    if let Ok(rel) = relative(&root, &target_dir) {
        exclude.insert(rel);
    }
    for dir in &dirs {
        for nested in nested_packages(dir, &dirs) {
            exclude.insert(relative(&root, &nested)?);
        }
    }

    let mut resolved = Resolved {
        inputs: inputs.into_iter().collect(),
        exclude: exclude.into_iter().collect(),
        packages,
        outputs: Vec::new(),
        dep_info: Vec::new(),
    };

    if !spec.artifacts.is_empty() {
        let base = relative(&root, &target_dir).with_context(|| {
            format!(
                "`cache.cargo`: Cargo's target directory is {}, outside this project — \
                 artifacts there can't be stored under paths relative to {}. Unset \
                 CARGO_TARGET_DIR (or `build.target-dir`) for this build, or drop \
                 `artifacts` and list outputs by hand.",
                target_dir.display(),
                root.display(),
            )
        })?;
        let platform = Platform::of(spec.target.as_deref());
        let mut out_dir = base;
        if let Some(triple) = &spec.target {
            out_dir = join(&out_dir, triple);
        }
        out_dir = join(&out_dir, profile.as_str());

        let mut outputs = BTreeSet::new();
        let mut dep_info = BTreeSet::new();
        for package in &selected {
            for target in &package.targets {
                for kind in &spec.artifacts {
                    if let Some((file, stem)) = artifact_name(*kind, target, platform) {
                        outputs.insert(join(&out_dir, &file));
                        dep_info.insert(join(&out_dir, &format!("{stem}.d")));
                    }
                }
            }
        }
        if outputs.is_empty() {
            let names: Vec<&str> = selected.iter().map(|p| p.name.as_str()).collect();
            bail!(
                "`cache.cargo`: asked for {:?} artifacts, but {} has no target producing one",
                spec.artifacts,
                names.join(", "),
            );
        }
        resolved.outputs = outputs.into_iter().collect();
        resolved.dep_info = dep_info.into_iter().collect();
    }

    Ok(resolved)
}

/// Every source file under `root` that rustc reports having read for the
/// artifacts in `resolved`, from the dep-info files it left next to them.
///
/// This is the compiler's own account, so it catches what no glob can: an
/// `include_str!("../../schemas/x.json")` reaching outside the package, a
/// `#[path]` module, a build script's `rerun-if-changed`. Files under an
/// excluded directory (the target directory, where `OUT_DIR` code is
/// generated) are derived rather than read, and files outside the project
/// (the registry, the toolchain's standard library) are pinned by
/// `Cargo.lock` and the toolchain file — neither is reported. A dep-info file
/// that isn't there yet, before a first build, simply has nothing to say.
pub fn dep_info_reads(root: &Path, resolved: &Resolved) -> BTreeSet<String> {
    let Ok(root) = root.canonicalize() else {
        return BTreeSet::new();
    };
    let mut reads = BTreeSet::new();
    for file in &resolved.dep_info {
        let Ok(text) = std::fs::read_to_string(root.join(file)) else {
            continue;
        };
        for path in parse_dep_info(&text) {
            let path = Path::new(&path);
            let path = if path.is_absolute() {
                path.to_path_buf()
            } else {
                root.join(path)
            };
            let Ok(rel) = relative(&root, &path) else {
                continue;
            };
            let derived = resolved
                .exclude
                .iter()
                .any(|e| rel == *e || rel.starts_with(&format!("{e}/")));
            // rustc lists directories a build script watches, too; only files
            // can be inputs.
            if !derived && !rel.is_empty() && root.join(&rel).is_file() {
                reads.insert(rel);
            }
        }
    }
    reads
}

/// The prerequisites in a Makefile-style dep-info file: everything after each
/// rule's `target:`, split on unescaped whitespace, `\ ` being a space inside
/// a name.
fn parse_dep_info(text: &str) -> Vec<String> {
    let joined = text.replace("\\\r\n", " ").replace("\\\n", " ");
    let mut out = Vec::new();
    for line in joined.lines() {
        // `: ` rather than `:`, so a Windows drive letter in the target isn't
        // taken for the separator.
        let Some((_, deps)) = line.split_once(": ") else {
            continue;
        };
        let mut current = String::new();
        let mut chars = deps.chars().peekable();
        while let Some(c) = chars.next() {
            match c {
                '\\' if chars.peek() == Some(&' ') => {
                    current.push(' ');
                    chars.next();
                }
                ' ' | '\t' => {
                    if !current.is_empty() {
                        out.push(std::mem::take(&mut current));
                    }
                }
                c => current.push(c),
            }
        }
        if !current.is_empty() {
            out.push(current);
        }
    }
    out
}

/// The manifest a spec reads: the one it names, else the step's own
/// sub-workspace's, else the root's.
fn manifest_for(root: &Path, home: Option<&str>, spec: &CargoSpec) -> Result<PathBuf> {
    if let Some(manifest) = &spec.manifest {
        let path = root.join(manifest);
        if !path.is_file() {
            bail!("`cache.cargo.manifest`: {} doesn't exist", path.display());
        }
        return Ok(canonical(&path));
    }
    let candidates = home
        .filter(|h| !h.is_empty() && *h != ".")
        .map(|h| root.join(h).join("Cargo.toml"))
        .into_iter()
        .chain(std::iter::once(root.join("Cargo.toml")));
    for candidate in candidates {
        if candidate.is_file() {
            return Ok(canonical(&candidate));
        }
    }
    bail!(
        "`cache.cargo`: no Cargo.toml in {}{} — name one with `cache.cargo.manifest`",
        home.map(|h| format!("{h} or ")).unwrap_or_default(),
        root.display(),
    )
}

/// The packages a spec selects, as `cargo build` in the manifest's directory
/// would choose them when it names none.
fn select(meta: &Metadata, manifest: &Path, spec: &CargoSpec) -> Result<Vec<MetaPackage>> {
    let members: Vec<&MetaPackage> = meta
        .packages
        .iter()
        .filter(|p| meta.workspace_members.contains(&p.id))
        .collect();

    let names: Vec<&String> = spec.package.iter().chain(spec.packages.iter()).collect();
    if !names.is_empty() {
        let mut found = Vec::new();
        for name in names {
            match members.iter().find(|p| &p.name == name) {
                Some(package) => found.push((*package).clone()),
                None => {
                    let available: Vec<&str> = members.iter().map(|p| p.name.as_str()).collect();
                    bail!(
                        "`cache.cargo`: no package `{name}` in the Cargo workspace at {} \
                         (it has: {})",
                        meta.workspace_root.display(),
                        available.join(", "),
                    );
                }
            }
        }
        return Ok(found);
    }

    if let Some(own) = members
        .iter()
        .find(|p| canonical(&p.manifest_path) == manifest)
    {
        return Ok(vec![(*own).clone()]);
    }

    // A virtual manifest: its default members, or every member.
    let defaults = meta
        .workspace_default_members
        .as_ref()
        .filter(|d| !d.is_empty())
        .unwrap_or(&meta.workspace_members);
    Ok(members
        .into_iter()
        .filter(|p| defaults.contains(&p.id))
        .cloned()
        .collect())
}

/// The files Cargo and rustup read besides the packages themselves, that
/// exist: the workspace manifest and lockfile, and any toolchain or config
/// file from the manifest's directory up to the project root.
fn cargo_files(root: &Path, workspace_root: &Path, manifest: &Path) -> Vec<PathBuf> {
    let mut files = vec![
        workspace_root.join("Cargo.toml"),
        workspace_root.join("Cargo.lock"),
    ];
    let mut dir = manifest.parent();
    while let Some(at) = dir {
        if !at.starts_with(root) {
            break;
        }
        for name in [
            "rust-toolchain",
            "rust-toolchain.toml",
            ".cargo/config",
            ".cargo/config.toml",
        ] {
            files.push(at.join(name));
        }
        dir = at.parent();
    }
    files.retain(|f| f.is_file());
    files.sort();
    files.dedup();
    files
}

/// Directories below `dir` holding a `Cargo.toml` of their own that aren't in
/// `closure`, stopping at the first one down each branch.
fn nested_packages(dir: &Path, closure: &BTreeSet<PathBuf>) -> Vec<PathBuf> {
    fn walk(at: &Path, closure: &BTreeSet<PathBuf>, depth: usize, found: &mut Vec<PathBuf>) {
        if depth > 6 {
            return;
        }
        let Ok(entries) = std::fs::read_dir(at) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with('.') || ["target", "node_modules"].contains(&name.as_ref()) {
                continue;
            }
            let path = canonical(&path);
            if path.join("Cargo.toml").is_file() && !closure.contains(&path) {
                found.push(path);
                continue;
            }
            // A closure package nested in another is walked on its own turn.
            if closure.contains(&path) {
                continue;
            }
            walk(&path, closure, depth + 1, found);
        }
    }
    let mut found = Vec::new();
    walk(dir, closure, 0, &mut found);
    found
}

/// The platform whose naming an artifact follows.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Platform {
    Windows,
    Mac,
    Unix,
}

impl Platform {
    fn of(triple: Option<&str>) -> Platform {
        let os = match triple {
            Some(triple) if triple.contains("windows") => "windows",
            Some(triple) if triple.contains("apple") => "macos",
            Some(_) => "linux",
            None => std::env::consts::OS,
        };
        match os {
            "windows" => Platform::Windows,
            "macos" | "ios" => Platform::Mac,
            _ => Platform::Unix,
        }
    }
}

/// The file `target` produces for `kind`, and the stem of its dep-info file;
/// `None` when the target doesn't produce that kind.
fn artifact_name(
    kind: ArtifactKind,
    target: &MetaTarget,
    platform: Platform,
) -> Option<(String, String)> {
    let has = |t: &str| target.crate_types.iter().any(|c| c == t);
    let is_bin = target.kind.iter().any(|k| k == "bin");
    let is_lib = target.kind.iter().any(|k| {
        ["lib", "rlib", "cdylib", "dylib", "staticlib", "proc-macro"].contains(&k.as_str())
    });
    // Libraries are named with underscores, binaries as written.
    let lib = target.name.replace('-', "_");
    let dylib = |stem: &str| match platform {
        Platform::Windows => format!("{stem}.dll"),
        Platform::Mac => format!("lib{stem}.dylib"),
        Platform::Unix => format!("lib{stem}.so"),
    };
    let named = |file: String, stem: String| Some((file, stem));
    match kind {
        ArtifactKind::Bin if is_bin => {
            let file = match platform {
                Platform::Windows => format!("{}.exe", target.name),
                _ => target.name.clone(),
            };
            named(file, target.name.clone())
        }
        ArtifactKind::Lib if is_lib && (has("lib") || has("rlib")) => {
            named(format!("lib{lib}.rlib"), format!("lib{lib}"))
        }
        ArtifactKind::Cdylib if is_lib && has("cdylib") => {
            named(dylib(&lib), dylib_stem(&lib, platform))
        }
        ArtifactKind::Dylib if is_lib && has("dylib") => {
            named(dylib(&lib), dylib_stem(&lib, platform))
        }
        ArtifactKind::Staticlib if is_lib && has("staticlib") => {
            let file = match platform {
                Platform::Windows => format!("{lib}.lib"),
                _ => format!("lib{lib}.a"),
            };
            named(file, dylib_stem(&lib, platform))
        }
        _ => None,
    }
}

/// The dep-info stem for a library: Cargo writes `lib<name>.d` everywhere but
/// Windows, where the `lib` prefix isn't part of the artifact's name.
fn dylib_stem(lib: &str, platform: Platform) -> String {
    match platform {
        Platform::Windows => lib.to_string(),
        _ => format!("lib{lib}"),
    }
}

fn join(dir: &str, name: &str) -> String {
    if dir.is_empty() {
        name.to_string()
    } else {
        format!("{dir}/{name}")
    }
}

/// `path` relative to `root`, `/`-separated; an error outside it.
fn relative(root: &Path, path: &Path) -> Result<String> {
    let path = canonical(path);
    let rel = path
        .strip_prefix(root)
        .with_context(|| format!("{} is outside {}", path.display(), root.display()))?;
    Ok(rel.to_string_lossy().replace('\\', "/"))
}

/// Canonicalize what exists, and the longest existing prefix of what doesn't —
/// a target directory before the first build is still a path worth naming.
fn canonical(path: &Path) -> PathBuf {
    if let Ok(real) = path.canonicalize() {
        return real;
    }
    match (path.parent(), path.file_name()) {
        (Some(parent), Some(name)) => canonical(parent).join(name),
        _ => path.to_path_buf(),
    }
}

// ─── cargo metadata ─────────────────────────────────────────────────────────

/// The parts of `cargo metadata --no-deps` this reads.
#[derive(Debug, Clone, Deserialize)]
struct Metadata {
    packages: Vec<MetaPackage>,
    workspace_members: Vec<String>,
    #[serde(default)]
    workspace_default_members: Option<Vec<String>>,
    target_directory: PathBuf,
    workspace_root: PathBuf,
}

#[derive(Debug, Clone, Deserialize)]
struct MetaPackage {
    id: String,
    name: String,
    manifest_path: PathBuf,
    #[serde(default)]
    dependencies: Vec<MetaDependency>,
    #[serde(default)]
    targets: Vec<MetaTarget>,
}

#[derive(Debug, Clone, Deserialize)]
struct MetaDependency {
    /// Set for a path dependency: the directory it points at.
    #[serde(default)]
    path: Option<PathBuf>,
}

#[derive(Debug, Clone, Deserialize)]
struct MetaTarget {
    name: String,
    kind: Vec<String>,
    #[serde(default)]
    crate_types: Vec<String>,
}

/// A metadata read, and what would have to change for it to be stale.
struct Memo {
    stamps: Vec<(PathBuf, Option<(SystemTime, u64)>)>,
    metadata: Metadata,
}

/// Reads of `cargo metadata`, by manifest.
///
/// The daemon lives for days and plans many runs, and every step of every plan
/// asks for its inputs, outputs and listings separately; without this a
/// workspace of twenty crates would run Cargo a few hundred times to draw one
/// graph. Each entry is checked against the files whose change would change
/// its answer — every member manifest, the directory each sits in (where a new
/// sibling crate would appear), and the workspace's own config — before it is
/// trusted, so an edited `Cargo.toml` is noticed on the next look.
fn memo() -> &'static Mutex<HashMap<PathBuf, Memo>> {
    static MEMO: OnceLock<Mutex<HashMap<PathBuf, Memo>>> = OnceLock::new();
    MEMO.get_or_init(Default::default)
}

fn stamp(path: &Path) -> Option<(SystemTime, u64)> {
    let meta = std::fs::metadata(path).ok()?;
    Some((meta.modified().ok()?, meta.len()))
}

fn metadata(manifest: &Path) -> Result<Metadata> {
    let key = canonical(manifest);
    if let Ok(memo) = memo().lock()
        && let Some(entry) = memo.get(&key)
        && entry.stamps.iter().all(|(path, was)| stamp(path) == *was)
    {
        return Ok(entry.metadata.clone());
    }

    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let dir = key.parent().unwrap_or(Path::new("."));
    let output = Command::new(&cargo)
        .args([
            "metadata",
            "--format-version",
            "1",
            "--no-deps",
            "--offline",
        ])
        .arg("--manifest-path")
        .arg(&key)
        .current_dir(dir)
        .output()
        .with_context(|| {
            format!(
                "`cache.cargo` needs Cargo to read {}, and `{}` couldn't be run",
                key.display(),
                cargo.to_string_lossy()
            )
        })?;
    if !output.status.success() {
        bail!(
            "`cache.cargo`: `cargo metadata` failed for {}:\n{}",
            key.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let metadata: Metadata = serde_json::from_slice(&output.stdout)
        .with_context(|| format!("Failed to parse `cargo metadata` for {}", key.display()))?;

    let mut watched: BTreeSet<PathBuf> = BTreeSet::new();
    watched.insert(key.clone());
    watched.insert(metadata.workspace_root.clone());
    watched.insert(metadata.workspace_root.join("Cargo.toml"));
    watched.insert(metadata.workspace_root.join(".cargo/config.toml"));
    for package in &metadata.packages {
        watched.insert(package.manifest_path.clone());
        if let Some(parent) = package.manifest_path.parent().and_then(Path::parent) {
            watched.insert(parent.to_path_buf());
        }
    }
    let stamps = watched.into_iter().map(|p| {
        let s = stamp(&p);
        (p, s)
    });
    if let Ok(mut memo) = memo().lock() {
        memo.insert(
            key,
            Memo {
                stamps: stamps.collect(),
                metadata: metadata.clone(),
            },
        );
    }
    Ok(metadata)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::CacheConfig;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ciab_cargo_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.canonicalize().unwrap()
    }

    fn write(dir: &Path, rel: &str, body: &str) {
        let path = dir.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }

    fn package(name: &str, extra: &str) -> String {
        format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n{extra}")
    }

    /// A virtual workspace: `api` (bin) path-depends on `core` (lib); `cli`
    /// (bin) depends on nothing; `api/fuzz` is a package of its own nested in
    /// `api` and part of no workspace member's closure.
    fn workspace(name: &str) -> PathBuf {
        let root = scratch(name);
        write(
            &root,
            "Cargo.toml",
            "[workspace]\nmembers = [\"crates/*\"]\nresolver = \"2\"\n",
        );
        write(&root, "Cargo.lock", "version = 4\n");
        write(&root, ".gitignore", "target/\n");
        write(
            &root,
            "crates/api/Cargo.toml",
            &package("api", "[dependencies]\ncore = { path = \"../core\" }\n"),
        );
        write(&root, "crates/api/src/main.rs", "fn main() {}");
        write(
            &root,
            "crates/api/fuzz/Cargo.toml",
            &package("api-fuzz", "[workspace]\n"),
        );
        write(&root, "crates/api/fuzz/src/lib.rs", "");
        write(&root, "crates/core/Cargo.toml", &package("core", ""));
        write(&root, "crates/core/src/lib.rs", "pub fn f() {}");
        write(&root, "crates/cli/Cargo.toml", &package("cli", ""));
        write(&root, "crates/cli/src/main.rs", "fn main() {}");
        write(&root, "target/release/stale", "derived");
        root
    }

    fn release_bins(package: Option<&str>) -> CargoSpec {
        CargoSpec {
            package: package.map(str::to_string),
            profile: Some("release".to_string()),
            artifacts: vec![ArtifactKind::Bin],
            ..Default::default()
        }
    }

    fn names(resolved: &Resolved) -> Vec<(&str, bool)> {
        resolved
            .packages
            .iter()
            .map(|p| (p.name.as_str(), p.selected))
            .collect()
    }

    #[test]
    fn a_package_brings_its_path_dependencies_and_cargos_own_files() {
        let root = workspace("closure");
        let resolved = resolve(&root, None, &release_bins(Some("api"))).unwrap();

        assert_eq!(names(&resolved), vec![("api", true), ("core", false)]);
        assert_eq!(
            resolved.inputs,
            vec!["Cargo.lock", "Cargo.toml", "crates/api", "crates/core"]
        );
        assert!(resolved.exclude.contains(&"target".to_string()));
        assert!(resolved.exclude.contains(&"crates/api/fuzz".to_string()));
        let exe = if cfg!(windows) { "api.exe" } else { "api" };
        assert_eq!(resolved.outputs, vec![format!("target/release/{exe}")]);
        assert_eq!(resolved.dep_info, vec!["target/release/api.d"]);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_profile_variable_picks_the_outputs_and_keys_the_build() {
        let root = workspace("profile_var");
        let var = "CIABATTA_TEST_CARGO_PROFILE_VAR";
        let step = crate::run::RunStep {
            name: "compile".into(),
            ..Default::default()
        };
        let config = CacheConfig {
            enabled: Some(true),
            cargo: Some(CargoSpec {
                package: Some("api".into()),
                profile: Some(format!("${{{var}:-release}}")),
                artifacts: vec![ArtifactKind::Bin],
                ..Default::default()
            }),
            ..Default::default()
        }
        .with_step_env(&step);
        assert_eq!(config.env, vec![var.to_string()]);

        let exe = if cfg!(windows) { "api.exe" } else { "api" };
        let outputs = |value: Option<&str>| {
            let values = value
                .map(|v| BTreeMap::from([(var.to_string(), v.to_string())]))
                .unwrap_or_default();
            config
                .clone()
                .with_values(&values)
                .cargo_resolved(&root)
                .unwrap()
                .unwrap()
                .outputs
        };
        assert_eq!(outputs(None), vec![format!("target/release/{exe}")]);
        assert_eq!(outputs(Some("debug")), vec![format!("target/debug/{exe}")]);
        let wrong = config
            .clone()
            .with_values(&BTreeMap::from([(var.to_string(), "fast".to_string())]))
            .cargo_resolved(&root)
            .unwrap_err();
        assert!(wrong.to_string().contains("`fast`"), "{wrong}");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn no_package_means_what_cargo_build_would_build_there() {
        let root = workspace("default");
        // From the package's own directory: that package.
        let own = resolve(&root, Some("crates/api"), &CargoSpec::default()).unwrap();
        assert_eq!(names(&own), vec![("api", true), ("core", false)]);
        // From a virtual manifest: every member.
        let all = resolve(&root, None, &CargoSpec::default()).unwrap();
        assert_eq!(
            names(&all),
            vec![("api", true), ("cli", true), ("core", true)]
        );

        let unknown = resolve(&root, None, &release_bins(Some("nope"))).unwrap_err();
        let message = format!("{unknown:#}");
        assert!(message.contains("no package `nope`"), "{message}");
        assert!(message.contains("it has: "), "{message}");
        for name in ["api", "cli", "core"] {
            assert!(message.contains(name), "{message}");
        }

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn asking_for_an_artifact_nothing_produces_is_an_error() {
        let root = workspace("nobin");
        let spec = CargoSpec {
            package: Some("core".to_string()),
            artifacts: vec![ArtifactKind::Bin],
            ..Default::default()
        };
        assert!(resolve(&root, None, &spec).is_err());
        let lib = CargoSpec {
            artifacts: vec![ArtifactKind::Lib],
            ..spec
        };
        assert_eq!(
            resolve(&root, None, &lib).unwrap().outputs,
            vec!["target/debug/libcore.rlib"]
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_configured_target_dir_and_its_config_file_are_respected() {
        let root = workspace("targetdir");
        write(
            &root,
            ".cargo/config.toml",
            "[build]\ntarget-dir = \"out/cargo\"\n",
        );
        let resolved = resolve(&root, None, &release_bins(Some("cli"))).unwrap();
        assert!(resolved.inputs.contains(&".cargo/config.toml".to_string()));
        assert!(resolved.exclude.contains(&"out/cargo".to_string()));
        assert!(resolved.outputs[0].starts_with("out/cargo/release/cli"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn independent_cargo_workspaces_each_get_their_own_lockfile_and_target() {
        let root = workspace("independent");
        write(
            &root,
            "editors/zed/Cargo.toml",
            &package(
                "zed-ext",
                "[workspace]\n\n[lib]\ncrate-type = [\"cdylib\"]\n",
            ),
        );
        write(&root, "editors/zed/Cargo.lock", "version = 4\n");
        write(&root, "editors/zed/src/lib.rs", "");
        let spec = CargoSpec {
            profile: Some("release".to_string()),
            artifacts: vec![ArtifactKind::Cdylib],
            ..Default::default()
        };
        let resolved = resolve(&root, Some("editors/zed"), &spec).unwrap();
        assert_eq!(names(&resolved), vec![("zed-ext", true)]);
        assert!(
            resolved
                .inputs
                .contains(&"editors/zed/Cargo.lock".to_string())
        );
        assert!(!resolved.inputs.contains(&"Cargo.lock".to_string()));
        assert!(resolved.outputs[0].starts_with("editors/zed/target/release/"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_path_dependency_outside_the_project_is_refused() {
        let outer = scratch("outside");
        let root = outer.join("repo");
        write(
            &outer,
            "shared/Cargo.toml",
            &package("shared", "[workspace]\n"),
        );
        write(&outer, "shared/src/lib.rs", "");
        write(
            &root,
            "Cargo.toml",
            &package("app", "[dependencies]\nshared = { path = \"../shared\" }\n"),
        );
        write(&root, "src/main.rs", "fn main() {}");
        let error = format!(
            "{:#}",
            resolve(&root, None, &CargoSpec::default()).unwrap_err()
        );
        assert!(error.contains("outside this project"), "{error}");
        let _ = std::fs::remove_dir_all(&outer);
    }

    #[test]
    fn an_edited_manifest_is_noticed_without_restarting() {
        let root = workspace("memo");
        let before = resolve(&root, None, &release_bins(Some("cli"))).unwrap();
        assert_eq!(names(&before), vec![("cli", true)]);
        write(
            &root,
            "crates/cli/Cargo.toml",
            &package("cli", "[dependencies]\ncore = { path = \"../core\" }\n"),
        );
        let after = resolve(&root, None, &release_bins(Some("cli"))).unwrap();
        assert_eq!(names(&after), vec![("cli", true), ("core", false)]);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The bug this section exists for: a crate path-depending on a crate in
    /// another ciabatta sub-workspace. Hand-written globs into the sibling are
    /// still excluded (it owns its own entry); Cargo's closure reaches in,
    /// because those sources are compiled into this build's artifact.
    #[test]
    fn a_path_dependency_in_a_sibling_sub_workspace_keys_the_build() {
        let root = workspace("sibling");
        std::fs::create_dir_all(root.join("crates/api/.ciabatta")).unwrap();
        std::fs::create_dir_all(root.join("crates/core/.ciabatta")).unwrap();
        std::fs::create_dir_all(root.join("crates/cli/.ciabatta")).unwrap();
        write(&root, "crates/api/target/debug/api", "derived");

        let by_hand = CacheConfig {
            enabled: Some(true),
            inputs: vec!["crates/api/**/*".into(), "crates/core/**/*".into()],
            ..Default::default()
        }
        .with_home(Some("crates/api".into()));
        let paths = |c: &CacheConfig| -> Vec<String> {
            c.hash_inputs(&root, Some("crates/api"))
                .unwrap()
                .into_iter()
                .map(|f| f.path)
                .collect()
        };
        assert!(
            !paths(&by_hand)
                .iter()
                .any(|p| p.starts_with("crates/core/"))
        );

        let cargo = CacheConfig {
            enabled: Some(true),
            cargo: Some(release_bins(None)),
            ..Default::default()
        }
        .with_home(Some("crates/api".into()));
        let keyed = paths(&cargo);
        assert!(
            keyed.contains(&"crates/core/src/lib.rs".to_string()),
            "{keyed:?}"
        );
        assert!(keyed.contains(&"crates/api/src/main.rs".to_string()));
        assert!(keyed.contains(&"Cargo.lock".to_string()));
        // Not the unrelated sibling, not the nested fuzz crate, not build output.
        assert!(
            !keyed.iter().any(|p| p.starts_with("crates/cli/")),
            "{keyed:?}"
        );
        assert!(!keyed.iter().any(|p| p.starts_with("crates/api/fuzz/")));
        assert!(!keyed.iter().any(|p| p.contains("target/")), "{keyed:?}");

        // And editing the dependency changes the key.
        let before = cargo.hash_inputs(&root, Some("crates/api")).unwrap();
        write(&root, "crates/core/src/lib.rs", "pub fn g() {}");
        let after = cargo.hash_inputs(&root, Some("crates/api")).unwrap();
        assert_ne!(before, after);

        assert!(cargo.accounts_for_its_outputs());
        assert!(cargo.why_disabled().is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn dep_info_names_every_file_rustc_read_that_no_input_covers() {
        let root = workspace("depinfo");
        write(&root, "schemas/api.json", "{}");
        let resolved = resolve(&root, None, &release_bins(Some("api"))).unwrap();
        let abs = |rel: &str| root.join(rel).to_string_lossy().to_string();
        write(
            &root,
            "target/release/api.d",
            &format!(
                "{}: {} {} \\\n {} {} /usr/lib/rustlib/core.rs\n",
                abs("target/release/api"),
                abs("crates/api/src/main.rs"),
                abs("crates/core/src/lib.rs"),
                abs("schemas/api.json"),
                abs("target/release/build/api-1/out/gen.rs"),
            ),
        );
        let reads = dep_info_reads(&root, &resolved);
        assert_eq!(
            reads.into_iter().collect::<Vec<_>>(),
            vec![
                "crates/api/src/main.rs",
                "crates/core/src/lib.rs",
                "schemas/api.json"
            ]
        );

        let config = CacheConfig {
            enabled: Some(true),
            cargo: Some(release_bins(Some("api"))),
            ..Default::default()
        };
        let nothing = std::collections::HashSet::new();
        assert_eq!(
            config.undeclared_reads(&root, None, &nothing).unwrap(),
            vec!["schemas/api.json"]
        );
        // A file an upstream step produced is that step's to key, through
        // `needs`, not this one's to declare.
        let upstream: std::collections::HashSet<String> =
            ["schemas/api.json".to_string()].into_iter().collect();
        assert!(
            config
                .undeclared_reads(&root, None, &upstream)
                .unwrap()
                .is_empty()
        );
        let declared = CacheConfig {
            inputs: vec!["schemas/**/*".to_string()],
            ..config
        };
        assert!(
            declared
                .undeclared_reads(&root, None, &nothing)
                .unwrap()
                .is_empty()
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn dep_info_escapes_spaces_and_spans_lines() {
        let parsed = parse_dep_info("C:\\t\\app.exe: C:\\a\\my\\ file.rs \\\n  b.rs\n\nb.rs:\n");
        assert_eq!(parsed, vec!["C:\\a\\my file.rs", "b.rs"]);
    }

    #[test]
    fn artifacts_are_named_the_way_each_platform_names_them() {
        let target = |name: &str, kind: &[&str]| MetaTarget {
            name: name.to_string(),
            kind: kind.iter().map(|k| k.to_string()).collect(),
            crate_types: kind.iter().map(|k| k.to_string()).collect(),
        };
        let bin = target("my-app", &["bin"]);
        let cdylib = target("my-ext", &["cdylib", "rlib"]);
        let staticlib = target("my-c", &["staticlib"]);
        let name = |kind, t: &MetaTarget, p| artifact_name(kind, t, p).map(|(f, _)| f);

        assert_eq!(
            name(ArtifactKind::Bin, &bin, Platform::Unix).unwrap(),
            "my-app"
        );
        assert_eq!(
            name(ArtifactKind::Bin, &bin, Platform::Windows).unwrap(),
            "my-app.exe"
        );
        assert_eq!(
            name(ArtifactKind::Cdylib, &cdylib, Platform::Unix).unwrap(),
            "libmy_ext.so"
        );
        assert_eq!(
            name(ArtifactKind::Cdylib, &cdylib, Platform::Mac).unwrap(),
            "libmy_ext.dylib"
        );
        assert_eq!(
            name(ArtifactKind::Cdylib, &cdylib, Platform::Windows).unwrap(),
            "my_ext.dll"
        );
        assert_eq!(
            name(ArtifactKind::Lib, &cdylib, Platform::Unix).unwrap(),
            "libmy_ext.rlib"
        );
        assert_eq!(
            name(ArtifactKind::Staticlib, &staticlib, Platform::Unix).unwrap(),
            "libmy_c.a"
        );
        assert_eq!(
            name(ArtifactKind::Staticlib, &staticlib, Platform::Windows).unwrap(),
            "my_c.lib"
        );
        assert!(name(ArtifactKind::Bin, &cdylib, Platform::Unix).is_none());

        assert_eq!(
            Platform::of(Some("x86_64-pc-windows-msvc")),
            Platform::Windows
        );
        assert_eq!(Platform::of(Some("aarch64-apple-darwin")), Platform::Mac);
        assert_eq!(
            Platform::of(Some("x86_64-unknown-linux-gnu")),
            Platform::Unix
        );
    }

    #[test]
    fn a_profile_is_debug_or_release_or_a_variable_holding_one() {
        use super::{Profile, ProfileRef};
        assert_eq!(
            ProfileRef::parse("release").unwrap(),
            ProfileRef::Fixed(Profile::Release)
        );
        assert_eq!(
            ProfileRef::parse("debug").unwrap(),
            ProfileRef::Fixed(Profile::Debug)
        );
        for written in ["$CARGO_PROFILE", "${CARGO_PROFILE}"] {
            assert_eq!(
                ProfileRef::parse(written).unwrap(),
                ProfileRef::Env {
                    name: "CARGO_PROFILE".into(),
                    default: None
                }
            );
        }
        assert_eq!(
            ProfileRef::parse("${CARGO_PROFILE:-release}").unwrap(),
            ProfileRef::Env {
                name: "CARGO_PROFILE".into(),
                default: Some(Profile::Release)
            }
        );

        // Cargo's own names are refused with the one to write instead.
        let dev = ProfileRef::parse("dev").unwrap_err().to_string();
        assert!(dev.contains("write `debug`"), "{dev}");
        let bench = ProfileRef::parse("bench").unwrap_err().to_string();
        assert!(bench.contains("write `release`"), "{bench}");
        assert!(ProfileRef::parse("ci").is_err());
        assert!(ProfileRef::parse("${CARGO_PROFILE:-dev}").is_err());
    }

    #[test]
    fn a_profile_variable_is_read_from_the_steps_environment() {
        let var = "CIABATTA_TEST_CARGO_PROFILE_UNSET";
        let spec = |written: &str| CargoSpec {
            profile: Some(written.to_string()),
            ..Default::default()
        };
        let values = |v: &str| BTreeMap::from([(var.to_string(), v.to_string())]);
        let none = BTreeMap::new();

        assert_eq!(
            CargoSpec::default().resolve_profile(&none).unwrap(),
            Profile::Debug
        );
        let plain = spec(&format!("${{{var}}}"));
        assert_eq!(plain.profile_var().as_deref(), Some(var));
        assert_eq!(plain.resolve_profile(&none).unwrap(), Profile::Debug);
        assert_eq!(
            plain.resolve_profile(&values("release")).unwrap(),
            Profile::Release
        );
        // Empty is unset, as for `:-` in a shell.
        let defaulted = spec(&format!("${{{var}:-release}}"));
        assert_eq!(
            defaulted.resolve_profile(&values("")).unwrap(),
            Profile::Release
        );
        assert_eq!(
            defaulted.resolve_profile(&values("debug")).unwrap(),
            Profile::Debug
        );

        let wrong = plain
            .resolve_profile(&values("fast"))
            .unwrap_err()
            .to_string();
        assert!(wrong.contains(var) && wrong.contains("`fast`"), "{wrong}");
        assert!(spec("release").profile_var().is_none());
    }
}
