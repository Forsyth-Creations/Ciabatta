//! Build caching: skipping work whose inputs and outputs haven't moved.
//!
//! The bargain is explicit rather than clever. A workspace says what its build
//! *reads* (`inputs`) and what it *writes* (`outputs`); ciabatta hashes the
//! former into a key, and stores the latter under it. Next time, if the inputs
//! hash to the same key and the stored outputs are still byte-for-byte what
//! they were, there is nothing to do.
//!
//! Three decisions are worth stating outright, because they're the ones that
//! make a cache trustworthy rather than merely fast:
//!
//! * **Off by default.** A cache that turns itself on is a cache that will one
//!   day serve somebody a stale artifact they never asked to be cached. Opting
//!   in is one line, and it's the line where you also say what your inputs are
//!   — which is the part that actually has to be right.
//!
//! * **Outputs are verified, not assumed.** A key match says the inputs didn't
//!   change; it says nothing about whether somebody deleted `dist/` or edited a
//!   generated file by hand. So the outputs are hashed too, and a mismatch is a
//!   rebuild. This is the difference between "we think this is current" and
//!   "this is current".
//!
//! * **An undeclared input is a wrong answer, not a slow one.** If a build
//!   reads a file that isn't in `inputs`, changing that file won't change the
//!   key and the cache will confidently hand back the wrong artifact. That's
//!   why [`ciabatta dry-run`] exists, and why `cache init` scaffolds `inputs`
//!   from what's actually in the directory instead of leaving it empty.
//!
//! * **Every path is relative to the workspace root.** Not to the
//!   sub-workspace a step came from — to the root, the same place `cache.remote`
//!   is configured. One project is one cache, so one directory has to be what
//!   its paths mean; anything else needs `../` to reach a sibling, and a `../`
//!   in a stored path escapes the entry it belongs to and collides with every
//!   other entry for that target. So `tool_frontend/dist/**/*`, never `dist/**/*`
//!   written inside `tool_frontend`.
//!
//! [`ciabatta dry-run`]: crate::cache::plan

pub mod cargo;
pub mod cli;
pub mod diff;
pub mod graph;
pub mod scope;
pub mod store;

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// The `cache:` section of a workspace's config.
///
/// ```yaml
/// cache:
///   enabled: true
///   inputs:  ["src/**/*.rs", "Cargo.toml"]
///   outputs: ["target/release/app"]
/// ```
#[derive(Debug, Deserialize, Serialize, Clone, Default, PartialEq)]
pub struct CacheConfig {
    /// Whether caching is on. Absent or false means every build runs, which is
    /// the behaviour of every ciabatta before 0.2.0 and the one a project keeps
    /// until it says otherwise.
    ///
    /// Tri-state on purpose. Cache settings are declared at three levels —
    /// workspace, workflow, target — and a target that writes `inputs:` without
    /// mentioning `enabled` means "these are my files", not "turn caching off".
    /// A plain `bool` cannot tell that apart from an explicit `enabled: false`,
    /// which is exactly the difference [`crate::cache::graph::effective`] has to
    /// respect for per-target dependencies to be usable. Use [`Self::is_on`]
    /// rather than reading this directly.
    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,

    /// Glob patterns for the files a build reads, relative to the **workspace
    /// root** — not to the sub-workspace the step came from, even when the
    /// `cache:` section is written there. Changing any of them changes the key,
    /// and so forces a rebuild.
    ///
    /// A build that reads something not listed here will be handed a stale
    /// result. `ciabatta cache init` scaffolds these from the directory's real
    /// contents for exactly that reason.
    #[serde(default)]
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub inputs: Vec<String>,

    /// Earlier steps whose `outputs` this build reads: each one's output
    /// patterns are added to this build's inputs, so a change to what it wrote
    /// rebuilds this one — and the list never goes stale when its outputs are
    /// edited, which a copy of them would.
    ///
    /// Each entry is a step's cache id, `<member>:<workflow>:<step>`, or the
    /// shorter `<workflow>:<step>` / `<step>` when that is unambiguous in the
    /// graph. The step has to come before this one — something it `needs`,
    /// directly or through its workflow — so its outputs are written before
    /// they are read. Resolved when the graph compiles, into
    /// [`Self::upstream_outputs`].
    #[serde(default)]
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub inputs_from: Vec<String>,

    /// What [`Self::inputs_from`] resolved to: the output patterns of the
    /// steps it names. Filled in by the workflow compiler — whatever a config
    /// writes here is replaced — and serialized only so a step handed to the
    /// daemon arrives with it.
    ///
    /// Kept apart from `inputs` for two reasons: a level declaring
    /// `inputs_from` must not replace the `inputs` it inherited, and
    /// `gitignore` doesn't apply to these — build output is usually ignored,
    /// and it is exactly what was asked for.
    #[serde(default)]
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub upstream_outputs: Vec<String>,

    /// Glob patterns for the files a build writes, relative to the workspace
    /// root like [`Self::inputs`]. These are what gets stored and restored on a
    /// hit, and what's verified against the manifest before a hit is granted.
    #[serde(default)]
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub outputs: Vec<String>,

    /// This build writes nothing a later step reads: a test, a lint, a type
    /// check, a notification.
    ///
    /// Only meaningful with no `outputs`. A step that declares neither is one
    /// the cache can't account for — it might have written anything — so it
    /// runs every time and so does every step that needs it, which is how one
    /// `lint` at the root of a graph stops everything behind it from ever being
    /// reused. Saying it writes nothing makes it accountable: with its inputs
    /// unchanged it is skipped like any other hit, and its dependents key on it
    /// normally.
    ///
    /// Tri-state for the same reason as `enabled`: a level that doesn't
    /// mention it inherits it rather than switching it off.
    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub no_outputs: Option<bool>,

    /// Extra environment variables for the key. Optional: every variable a
    /// step gets from a deprecated `workspace.env`, its workflow's `env` list and its workflow's
    /// `required_env` already keys it — see [`Self::with_step_env`]. List here
    /// only what a build reads from the ambient environment without declaring
    /// it, like a `PROFILE` set in the shell.
    #[serde(default)]
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub env: Vec<String>,

    /// Of [`Self::env`], the names whose values go into the key and the
    /// stored manifest as a digest rather than as written: a workflow's
    /// `required_env`, which is where tokens and credentials are declared.
    /// Never written in config — filled in from the workflow when a step's
    /// effective settings are worked out.
    #[serde(skip)]
    pub secret_env: Vec<String>,

    /// Glob patterns never treated as inputs even when `inputs` would match
    /// them. Build output living under a source tree is the usual case.
    #[serde(default)]
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub exclude: Vec<String>,

    /// Leave out of `inputs` whatever git would ignore: every `.gitignore` in
    /// the tree, nested ones included, and `.git/info/exclude`. The user's
    /// global excludes file is deliberately not read — it differs from machine
    /// to machine, and a key that depends on whose laptop computed it can never
    /// be shared through the remote.
    ///
    /// This is what lets `inputs: ["crates/**/*"]` mean "the sources" rather
    /// than "the sources and every build artifact sitting next to them". Only
    /// inputs: outputs live in ignored directories almost by definition.
    ///
    /// Tri-state like `enabled`, so a level that doesn't mention it inherits
    /// it. Off when unset, because turning it on moves every existing key.
    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gitignore: Option<bool>,

    /// A Rust build's inputs and outputs, worked out by Cargo: see
    /// [`cargo`]. Added to whatever `inputs` and `outputs` say, never instead
    /// of them, so a build that also reads `assets/` lists just that.
    ///
    /// Replaced wholesale by a level that declares its own, like `inputs`.
    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cargo: Option<cargo::CargoSpec>,

    /// The step's own sub-workspace, relative to the workspace root, which is
    /// where `cargo` looks for a manifest when it names none. Never written in
    /// config — filled in by [`Self::with_home`] when a step's effective
    /// settings are worked out.
    #[serde(skip)]
    pub home: Option<String>,

    /// The step's environment, where a variable in `cargo.profile` is read
    /// before the process's own. Never written in config — filled in by
    /// [`Self::with_values`] where a step's environment is known.
    #[serde(skip)]
    pub values: BTreeMap<String, String>,

    /// The shared cache this workspace reads from and writes to, when it has
    /// one. Local-only caching needs none of this.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote: Option<RemoteRef>,
}

/// How a workspace reaches the shared remote cache, and who it is there.
#[derive(Debug, Deserialize, Serialize, Clone, PartialEq)]
pub struct RemoteRef {
    /// Base URL of the remote cache server (`ciabatta remote-cache start`).
    pub url: String,

    /// The name this project is known by on the server. Defaults to the
    /// workspace's own name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,

    /// The identifier the server assigned when this project first registered.
    ///
    /// Written back into the config by `ciabatta cache init` (or on first
    /// contact) and committed alongside it, so every checkout of the repo — and
    /// every CI runner — resolves to the same project rather than registering a
    /// new one under the same name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,

    /// Read from the remote cache but never write to it. What CI wants for
    /// pull-request builds: they benefit from everyone else's artifacts without
    /// being able to poison the cache for the main branch.
    #[serde(default)]
    #[serde(skip_serializing_if = "crate::format::is_false")]
    pub read_only: bool,

    /// Verify the server's TLS certificate.
    ///
    /// Defaults to on, and should stay on for anything reachable beyond your
    /// own machine. Turn it off for a cache behind a self-signed certificate,
    /// or an internal CA you haven't installed — but know what you're buying:
    /// with verification off, HTTPS is an encrypted channel to whoever
    /// answered, so the build artifacts it hands back are only as trustworthy
    /// as the network between you.
    // Not skipped when false: this defaults to `true`, so omitting `false`
    // would silently turn verification back on.
    #[serde(default = "default_true")]
    pub tls_verify: bool,

    /// Turn the remote off without deleting its settings.
    // Not skipped when false: this defaults to `true`, so omitting `false`
    // would silently turn it back on.
    #[serde(default = "default_true")]
    pub enabled: bool,
}

impl Default for RemoteRef {
    /// Hand-written rather than derived, so `tls_verify` and `enabled` default
    /// to `true` here exactly as they do when a config is parsed. A derived
    /// `Default` would give `false` for both and quietly disagree with the file.
    fn default() -> Self {
        RemoteRef {
            url: String::new(),
            name: None,
            project: None,
            read_only: false,
            tls_verify: true,
            enabled: true,
        }
    }
}

fn default_true() -> bool {
    true
}

impl CacheConfig {
    /// The value a declared variable contributes to the key: as written, or
    /// as `sha256:<digest>` for one of [`Self::secret_env`] — so a changed
    /// token still changes the key, but the token itself never lands in a
    /// manifest that is uploaded to a shared server. Unset is empty either way.
    pub fn env_value(&self, name: &str, env: &BTreeMap<String, String>) -> String {
        let value = env.get(name).cloned().unwrap_or_default();
        if !value.is_empty() && self.secret_env.iter().any(|s| s == name) {
            format!("sha256:{}", hash_bytes(value.as_bytes()))
        } else {
            value
        }
    }

    /// Fold a step's own environment into what keys it: every variable its
    /// deprecated `workspace.env` gives it, every name in its workflow's `env`, and every name in its
    /// workflow's `required_env`. A build's result depends on the variables it
    /// was run with, so declaring them once in `env:` / `required_env` is
    /// enough — `cache.env` only needs whatever else a build reads.
    pub fn with_step_env(mut self, step: &crate::run::RunStep) -> Self {
        let mut names: BTreeSet<String> = self.env.iter().cloned().collect();
        names.extend(step.env.keys().cloned());
        names.extend(step.required_env.iter().cloned());
        names.extend(step.declared_env.iter().cloned());
        // A profile read from a variable keys on it: the same sources built
        // as debug and as release are different builds.
        names.extend(self.cargo.as_ref().and_then(|c| c.profile_var()));
        // And so does what Cargo reads from the environment to decide how to
        // compile: flags change the code generated, and a target changes
        // which platform it's for.
        if self.cargo.is_some() {
            names.extend(cargo::CARGO_ENV.iter().map(|n| n.to_string()));
        }
        self.env = names.into_iter().collect();
        // A workflow's `env` names its values rather than writing them, so they
        // come from a `.env` — and one whose name says it's a credential is
        // keyed by digest, the same as a `required_env` token.
        for name in &step.declared_env {
            if crate::run::envdeps::looks_secret(name)
                && !step.env.contains_key(name)
                && !self.secret_env.contains(name)
            {
                self.secret_env.push(name.clone());
            }
        }
        for name in &step.required_env {
            // A value the file itself writes down is no secret.
            if !step.env.contains_key(name) && !self.secret_env.contains(name) {
                self.secret_env.push(name.clone());
            }
        }
        self
    }

    /// Whether caching was asked for. An unset `enabled` is off, the same as
    /// `false` — the difference between the two only matters when one level's
    /// settings are layered over another's.
    pub fn is_on(&self) -> bool {
        self.enabled.unwrap_or(false)
    }

    /// Why this config isn't caching, in one line, or `None` when it is.
    ///
    /// Enabled but with no `inputs` counts as off: with nothing to hash, every
    /// build would key the same and the first result would be served forever.
    /// Treating that as "off" is safer than treating it as "always hit".
    pub fn why_disabled(&self) -> Option<&'static str> {
        if !self.is_on() {
            Some("caching is off (set `cache.enabled: true` to turn it on)")
        } else if self.inputs.is_empty()
            && self.inputs_from.is_empty()
            && self.upstream_outputs.is_empty()
            && self.cargo.is_none()
        {
            Some("no `cache.inputs` are declared, so there's nothing to key on")
        } else {
            None
        }
    }

    /// Whether the shared cache should be consulted for this workspace.
    pub fn remote(&self) -> Option<&RemoteRef> {
        self.remote
            .as_ref()
            .filter(|r| r.enabled && !r.url.trim().is_empty())
    }

    /// Hash the files this build reads.
    ///
    /// `exclude` applies here and only here. It exists so a build output
    /// directory sitting under a source tree doesn't count as an input — and
    /// since `cache init` proposes exactly that (`outputs: dist/**/*`,
    /// `exclude: dist`), applying it to outputs as well would erase them and
    /// quietly turn caching off. Hence two named methods rather than one
    /// function with a flag.
    /// Another sub-workspace's files are never this build's inputs: see
    /// [`nested_workspaces_except`]. `own` is the step's own sub-workspace,
    /// relative to `dir` — the one member whose files it is precisely the
    /// point of this build to read. `None` for a step belonging to the root.
    pub fn hash_inputs(&self, dir: &Path, own: Option<&str>) -> Result<Vec<FileHash>> {
        self.inputs_matching(dir, own, true)
    }

    /// The files this build reads, listed but not hashed — see
    /// [`list_matching`].
    pub fn list_inputs(&self, dir: &Path, own: Option<&str>) -> Result<Vec<FileHash>> {
        self.inputs_matching(dir, own, false)
    }

    /// Record which sub-workspace this config's step belongs to — `None` or
    /// `"."` for the root — so `cargo` can find that step's manifest.
    pub fn with_home(mut self, home: Option<String>) -> Self {
        self.home = home.filter(|h| !h.is_empty() && h != ".");
        self
    }

    /// Record the step's environment, so a variable `cargo.profile` names is
    /// read from the same values the step runs with and keys on.
    pub fn with_values(mut self, values: &BTreeMap<String, String>) -> Self {
        self.values = values.clone();
        self
    }

    /// What [`Self::cargo`] resolves to, when it is set.
    pub fn cargo_resolved(&self, dir: &Path) -> Result<Option<cargo::Resolved>> {
        self.cargo
            .as_ref()
            .map(|spec| {
                // A step asks for its closure several times over while it is
                // planned — for its inputs, its outputs, its warnings.
                let key = format!(
                    "{}\0{:?}\0{spec:?}\0{:?}",
                    dir.display(),
                    self.home,
                    self.values
                );
                scope::resolved(key, || {
                    cargo::resolve_with(dir, self.home.as_deref(), spec, &self.values)
                })
            })
            .transpose()
    }

    fn inputs_matching(&self, dir: &Path, own: Option<&str>, hash: bool) -> Result<Vec<FileHash>> {
        let kept = self.kept_by_gitignore(dir)?;
        let mut files = matching(
            dir,
            &self.inputs,
            &self.input_exclude(dir, own),
            kept.as_ref(),
            hash,
        )?;
        // An upstream step's outputs, unfiltered by `gitignore` and by the
        // sibling-workspace exclusion: they are usually build output, often in
        // another sub-workspace, and naming the step is asking for them.
        if !self.upstream_outputs.is_empty() {
            let exclude = self.exclude.clone();
            let mut merged: BTreeMap<String, FileHash> =
                files.drain(..).map(|f| (f.path.clone(), f)).collect();
            for file in matching(dir, &self.upstream_outputs, &exclude, None, hash)? {
                merged.entry(file.path.clone()).or_insert(file);
            }
            files = merged.into_values().collect();
        }

        let Some(resolved) = self.cargo_resolved(dir)? else {
            return Ok(files);
        };
        // Cargo's half has its own exclusions. A sibling sub-workspace is
        // still somebody else's entry — unless a crate in it is a path
        // dependency, in which case its sources are compiled into this build's
        // artifact and leaving them out of the key is the stale-hit bug this
        // whole section exists to prevent. And `.gitignore` always applies:
        // a package directory walked whole would otherwise sweep up its own
        // `target/`.
        let mut exclude = self.exclude.clone();
        exclude.extend(resolved.exclude.iter().cloned());
        exclude.extend(nested_workspaces_through(dir, |rel| {
            Some(rel) == own || resolved.reaches_into(rel)
        }));
        let kept = match kept {
            Some(kept) => kept,
            None => not_ignored(dir)?,
        };
        let from_cargo = matching(dir, &resolved.inputs, &exclude, Some(&kept), hash)?;

        let mut merged: BTreeMap<String, FileHash> =
            files.drain(..).map(|f| (f.path.clone(), f)).collect();
        for file in from_cargo {
            merged.entry(file.path.clone()).or_insert(file);
        }
        Ok(merged.into_values().collect())
    }

    /// Every output pattern: what `outputs` says, and the artifacts `cargo`
    /// resolves to.
    fn output_patterns(&self, dir: &Path) -> Result<Vec<String>> {
        let mut patterns = self.outputs.clone();
        if let Some(resolved) = self.cargo_resolved(dir)? {
            patterns.extend(resolved.outputs);
        }
        Ok(patterns)
    }

    /// Files rustc reports reading for this build's `cargo` artifacts that its
    /// inputs don't cover — each one a change that would not invalidate its
    /// cache entry. Empty without `cargo`, and before a first build.
    ///
    /// `upstream` is what the steps this one needs produced: a build reading
    /// those is keyed on them already, through their output fingerprints.
    pub fn undeclared_reads(
        &self,
        dir: &Path,
        own: Option<&str>,
        upstream: &HashSet<String>,
    ) -> Result<Vec<String>> {
        let Some(resolved) = self.cargo_resolved(dir)? else {
            return Ok(Vec::new());
        };
        let reads = cargo::dep_info_reads(dir, &resolved);
        if reads.is_empty() {
            return Ok(Vec::new());
        }
        let covered: HashSet<String> = self
            .list_inputs(dir, own)?
            .into_iter()
            .map(|f| f.path)
            .collect();
        Ok(reads
            .into_iter()
            .filter(|r| !covered.contains(r) && !upstream.contains(r))
            .collect())
    }

    /// Variables this build's build scripts asked to be rerun on
    /// (`rerun-if-env-changed`) that don't key it. Empty without `cargo`, and
    /// before a first build.
    pub fn undeclared_build_env(&self, dir: &Path) -> Result<Vec<String>> {
        let Some(resolved) = self.cargo_resolved(dir)? else {
            return Ok(Vec::new());
        };
        Ok(cargo::build_script_env(dir, &resolved)
            .into_iter()
            .filter(|name| !self.env.contains(name))
            .collect())
    }

    /// Whether `cargo` will supply outputs, without running Cargo to find out.
    fn cargo_declares_outputs(&self) -> bool {
        self.cargo.as_ref().is_some_and(|c| !c.artifacts.is_empty())
    }

    /// With `gitignore: true`, every path under `dir` git would keep; `None`
    /// when the setting is off and nothing is filtered.
    fn kept_by_gitignore(&self, dir: &Path) -> Result<Option<HashSet<String>>> {
        if self.gitignore.unwrap_or(false) {
            not_ignored(dir).map(Some)
        } else {
            Ok(None)
        }
    }

    /// What is kept out of this build's inputs: what it declared, plus every
    /// sub-workspace that owns its own cache entry.
    ///
    /// And every output pattern: **an output is never an input.** A file the
    /// build writes that is also hashed into its key can never hit — the first
    /// run keys on a tree without it, the build writes it, and the next run's
    /// key includes it. Matched on the patterns rather than on the files that
    /// exist, so the input set is the same before the first build as after it.
    /// What this drops is reported by [`Self::outputs_in_inputs`], so the
    /// config can be fixed rather than quietly corrected forever.
    pub fn input_exclude(&self, dir: &Path, own: Option<&str>) -> Vec<String> {
        let mut exclude = self.exclude.clone();
        exclude.extend(nested_workspaces_except(dir, own));
        exclude.extend(self.outputs.iter().cloned());
        exclude
    }

    /// The files `inputs` would match that an output pattern also covers, and
    /// so are treated as outputs only — sorted, for a warning to name.
    pub fn outputs_in_inputs(&self, dir: &Path, own: Option<&str>) -> Result<Vec<String>> {
        if self.inputs.is_empty() || self.outputs.is_empty() {
            return Ok(Vec::new());
        }
        let mut exclude = self.exclude.clone();
        exclude.extend(nested_workspaces_except(dir, own));
        let outputs = Excluder::new(dir, &self.outputs);
        Ok(list_matching(dir, &self.inputs, &exclude)?
            .into_iter()
            .map(|f| f.path)
            .filter(|path| outputs.matches(path))
            .collect())
    }

    /// The files this build writes, listed but not hashed.
    pub fn list_outputs(&self, dir: &Path) -> Result<Vec<FileHash>> {
        list_matching(dir, &self.output_patterns(dir)?, &[])
    }

    /// Whether a run of this build can be shown to have produced what it did.
    ///
    /// A step that declares outputs is accountable for them: they're hashed
    /// after it runs, so its dependents' keys move exactly when its result
    /// moved, and only then. A step that declares none fingerprints to the same
    /// (empty) value whatever it just did, so nothing downstream can tell a
    /// rerun that changed everything from one that changed nothing — which is
    /// why a step behind one of those has to run too. See
    /// [`Reason::UpstreamReran`].
    pub fn accounts_for_its_outputs(&self) -> bool {
        !self.outputs.is_empty() || self.cargo_declares_outputs() || self.writes_nothing()
    }

    /// Whether this build declared that it writes nothing anyone reads
    /// (`no_outputs: true`, with no `outputs` to contradict it).
    pub fn writes_nothing(&self) -> bool {
        self.outputs.is_empty()
            && !self.cargo_declares_outputs()
            && self.no_outputs.unwrap_or(false)
    }

    /// Hash the files this build writes. Never filtered by `exclude`.
    ///
    /// Nor by [`nested_workspaces`]: a step that legitimately writes into a
    /// sub-workspace's tree (a generator putting stubs where the package that
    /// consumes them lives) has to be able to record what it produced, and
    /// unlike an undeclared input an over-broad output can only ever cost a
    /// rebuild.
    pub fn hash_outputs(&self, dir: &Path) -> Result<Vec<FileHash>> {
        hash_matching(dir, &self.output_patterns(dir)?, &[])
    }
}

/// What each step of a graph produced, and what it needs — so a file a build
/// read can be told apart as one an upstream step made (keyed already,
/// through `needs`) or one it should have declared.
#[derive(Debug, Default, Clone)]
pub struct Produced {
    steps: std::collections::HashMap<String, (Vec<String>, Vec<String>)>,
}

impl Produced {
    pub fn record(&mut self, name: &str, needs: &[String], outputs: &[FileHash]) {
        self.steps.insert(
            name.to_string(),
            (
                needs.to_vec(),
                outputs.iter().map(|f| f.path.clone()).collect(),
            ),
        );
    }

    /// Every output of the steps in `needs`, and of the steps they need, all
    /// the way up.
    pub fn upstream_of(&self, needs: &[String]) -> HashSet<String> {
        let mut out = HashSet::new();
        let mut seen: HashSet<&str> = HashSet::new();
        let mut queue: Vec<&str> = needs.iter().map(String::as_str).collect();
        while let Some(name) = queue.pop() {
            if !seen.insert(name) {
                continue;
            }
            if let Some((needs, outputs)) = self.steps.get(name) {
                out.extend(outputs.iter().cloned());
                queue.extend(needs.iter().map(String::as_str));
            }
        }
        out
    }
}

/// Sub-workspaces nested under `dir`, as paths relative to it.
///
/// A monorepo root's `inputs` are written as globs like `packages/**/*`, and
/// those globs cannot help but sweep up every member package underneath. That
/// is the wrong answer twice over: the root's key changes whenever any package
/// changes (so the root never hits), and the package's own key already covers
/// those files (so the work is duplicated). Each member owns its own cache
/// entry; the super-workspace's job is the files nobody else claimed.
///
/// So any directory below `dir` that opted in with its own `.ciabatta/` is
/// excluded from `dir`'s inputs automatically, and — because the scan stops at
/// the first one it finds down each branch — a sub-sub-workspace is excluded by
/// its own parent rather than counted twice.
///
/// A member can still opt back in by naming files under it explicitly: exclude
/// patterns are matched against the relative path, so `inputs` reaching into a
/// nested workspace loses only what that workspace already owns.
///
/// `own` is the exception: the member whose build this is. Every path is
/// relative to the workspace root, so a member's own step names its own files
/// through the root — `tool_frontend:build` declares
/// `tool_frontend/src/**/*`. Excluding every member from every step would
/// therefore exclude that step's entire reason for existing, leaving it keyed
/// on nothing and hitting forever — the exact failure this auto-exclusion was
/// written to prevent, aimed at the wrong target. So `own` is kept in, and the
/// scan descends *through* it to exclude what is nested below it. Everybody
/// else's subtree is still somebody else's entry.
pub fn nested_workspaces_except(dir: &Path, own: Option<&str>) -> Vec<String> {
    nested_workspaces_through(dir, |rel| Some(rel) == own)
}

/// [`nested_workspaces_except`], descending through every sub-workspace
/// `through` says yes to rather than only the step's own — the ones a
/// `cache.cargo` closure has to reach into.
pub fn nested_workspaces_through(dir: &Path, through: impl Fn(&str) -> bool) -> Vec<String> {
    let all = scope::members(dir.to_path_buf(), || {
        let mut all = Vec::new();
        collect_members(dir, dir, 0, &mut all);
        all
    });
    // A member is somebody else's when `through` says no to it and yes to
    // every member it sits inside: below one that is somebody else's, it
    // belongs to that one, and is excluded along with it.
    let mut found: Vec<String> = Vec::new();
    for member in &all {
        let inside_through = all
            .iter()
            .filter(|outer| member.starts_with(&format!("{outer}/")))
            .all(|outer| through(outer));
        if inside_through && !through(member) {
            found.push(member.clone());
        }
    }
    found
}

/// How deep to look for nested sub-workspaces. Matches the workspace scanner's
/// own limit, so the two agree about what counts as a member.
const MAX_NEST_DEPTH: usize = 6;

/// Every sub-workspace under `root`, at any depth up to [`MAX_NEST_DEPTH`],
/// parents before what is nested in them. The same for every step, so it is
/// scanned once per plan; which of them a step excludes is worked out from it.
fn collect_members(root: &Path, dir: &Path, depth: usize, found: &mut Vec<String>) {
    if depth >= MAX_NEST_DEPTH {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let name = entry.file_name();
        let name = name.to_string_lossy();
        // A dot-directory is never a member, and descending into build output
        // and dependency trees is thousands of directories for no answer.
        if name.starts_with('.') || SKIP_SCAN_DIRS.contains(&name.as_ref()) {
            continue;
        }
        if path.join(crate::config::CIABATTA_DIR).is_dir()
            && let Ok(rel) = path.strip_prefix(root)
        {
            found.push(rel.to_string_lossy().replace('\\', "/"));
        }
        collect_members(root, &path, depth + 1, found);
    }
}

/// Directories never worth descending into when looking for nested members.
const SKIP_SCAN_DIRS: &[&str] = &[
    "node_modules",
    "target",
    "dist",
    "build",
    "out",
    "vendor",
    "venv",
    "__pycache__",
    "coverage",
    "tmp",
];

// ─── Hashing ────────────────────────────────────────────────────────────────

/// One file's contribution to a key or a manifest: where it is and what's in it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileHash {
    /// Path relative to the workspace root, with `/` separators so a key
    /// computed on Windows matches one computed on Linux. Never begins `../`:
    /// the matcher refuses a pattern that reaches outside the root, because
    /// such a path escapes the artifact directory it is stored under.
    pub path: String,
    /// Hex SHA-256 of the file's contents.
    ///
    /// Empty for a file that was *listed* rather than hashed — see
    /// [`list_matching`], which is what `ciabatta why --all` and the run summary
    /// use. Omitted from JSON when empty so a consumer sees a missing field
    /// rather than a blank hash it might mistake for a real one. Never empty in
    /// a stored manifest or a cache key, where it is the whole point.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub sha256: String,
    /// Size in bytes, for reporting.
    pub size: u64,
}

/// The SHA-256 of a file's contents, hex encoded.
pub fn hash_file(path: &Path) -> Result<String> {
    scope::hash(path.to_path_buf(), || hash_file_now(path))
}

fn hash_file_now(path: &Path) -> Result<String> {
    let mut file =
        std::fs::File::open(path).with_context(|| format!("Failed to read {}", path.display()))?;
    let mut hasher = Sha256::new();
    std::io::copy(&mut file, &mut hasher)
        .with_context(|| format!("Failed to read {}", path.display()))?;
    Ok(hex(&hasher.finalize()))
}

/// The SHA-256 of a byte string, hex encoded.
pub fn hash_bytes(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex(&hasher.finalize())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Expand `patterns` against `dir` into the matching files, hashed and sorted
/// by path.
///
/// Sorted because the key must not depend on the order the filesystem happened
/// to hand directory entries back in — the same tree on two machines has to
/// produce the same key or the cache is worthless.
///
/// A pattern that resolves to a **directory** means everything under it, at
/// any depth. `inputs: [src]`, `inputs: [src/]` and `inputs: ["src/**/*"]` are
/// the same declaration, and `assets/**` — which globs to the directories under
/// `assets/` rather than to their files — collects the files inside them
/// instead of quietly collecting nothing. Getting this wrong is the expensive
/// kind of wrong: a pattern that silently matches no files leaves a build
/// keyed on an empty input set, which hits forever.
///
/// Anything matching `exclude` is skipped, directories included — excluding a
/// directory excludes the tree below it. A pattern that matches nothing is not
/// an error: `outputs` legitimately match nothing before the first build, and
/// an `inputs` pattern for an optional file is a normal thing to write.
pub fn hash_matching(dir: &Path, patterns: &[String], exclude: &[String]) -> Result<Vec<FileHash>> {
    matching(dir, patterns, exclude, None, true)
}

/// The same expansion as [`hash_matching`], without reading a single byte.
///
/// For the times somebody wants to know *what* a target depends on rather than
/// what it currently hashes to — the run summary and the web viewer both list
/// every input of every step, and hashing a monorepo's worth of sources to
/// print a count would make simply looking at the graph as expensive as
/// building it. The `sha256` of each returned file is empty.
pub fn list_matching(dir: &Path, patterns: &[String], exclude: &[String]) -> Result<Vec<FileHash>> {
    matching(dir, patterns, exclude, None, false)
}

/// Every file and directory under `dir` that git would not ignore, as
/// `/`-separated paths relative to it.
///
/// Walked once up front rather than asked per path, because ignore rules are
/// inherited down the tree — whether `a/b/c` is ignored depends on every
/// `.gitignore` between it and `dir` — and the walk already prunes an ignored
/// `target/` or `node_modules/` without descending into it. Dot-files are kept
/// (`.cargo/config.toml` is an input like any other); `.git` itself never is.
///
/// Works outside a git checkout too: a `.gitignore` says what's derived
/// whether or not anyone has run `git init`, and a CI job building from a
/// source tarball should get the same key as one building from a clone.
pub fn not_ignored(dir: &Path) -> Result<HashSet<String>> {
    scope::not_ignored(dir.to_path_buf(), || not_ignored_now(dir))
}

fn not_ignored_now(dir: &Path) -> Result<HashSet<String>> {
    let mut kept = HashSet::new();
    let walk = ignore::WalkBuilder::new(dir)
        .hidden(false)
        .parents(false)
        .ignore(false)
        .git_global(false)
        .git_ignore(true)
        .git_exclude(true)
        .require_git(false)
        .filter_entry(|entry| entry.file_name() != ".git" && !is_state_dir(entry.path()))
        .build();
    for entry in walk {
        let entry = entry.with_context(|| format!("Failed to walk {}", dir.display()))?;
        let Ok(rel) = entry.path().strip_prefix(dir) else {
            continue;
        };
        let rel = rel.to_string_lossy().replace('\\', "/");
        if !rel.is_empty() {
            kept.insert(rel);
        }
    }
    Ok(kept)
}

/// Every path under `dir`, ignored or not, each with whether it is a
/// directory — what the web app's file picker browses. `.git` is left out;
/// nothing else is.
pub fn every_path(dir: &Path) -> Result<Vec<(String, bool)>> {
    let mut all = Vec::new();
    let walk = ignore::WalkBuilder::new(dir)
        .standard_filters(false)
        .filter_entry(|entry| entry.file_name() != ".git" && !is_state_dir(entry.path()))
        .build();
    for entry in walk {
        let entry = entry.with_context(|| format!("Failed to walk {}", dir.display()))?;
        let Ok(rel) = entry.path().strip_prefix(dir) else {
            continue;
        };
        let rel = rel.to_string_lossy().replace('\\', "/");
        if !rel.is_empty() {
            let is_dir = entry.file_type().is_some_and(|t| t.is_dir());
            all.push((rel, is_dir));
        }
    }
    Ok(all)
}

/// Where ciabatta keeps its own state inside a project — the local cache
/// store, run history — under any sub-workspace's `.ciabatta/`.
const STATE_DIRS: &[&str] = &["cache", "history", ".cache"];

/// Whether `path` is one of [`STATE_DIRS`]: never an input, and never worth
/// walking. A project that doesn't list them in `.gitignore` would otherwise
/// have every step walk the store — tens of thousands of files after a few
/// hundred builds, and more with each one — and a step reading the whole
/// tree would key on its own cache, and never hit.
fn is_state_dir(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| STATE_DIRS.contains(&name))
        && path
            .parent()
            .and_then(Path::file_name)
            .is_some_and(|parent| parent == crate::config::CIABATTA_DIR)
}

/// Rewrite a `**` glued to other text into the spelling the matcher accepts.
///
/// `**.json`, `src/**.rs` and `assets/img**` are how most tools let you write
/// "at any depth", but the `glob` crate only treats `**` as recursive when it
/// is a whole path component, and refuses anything else as an invalid pattern.
/// So `**.json` becomes `**/*.json` — every `.json` file at any depth — and a
/// component like `img**` becomes `img*/**`: whatever starts with `img`, and
/// everything below it. A pattern already written the strict way is returned
/// untouched.
pub fn normalize_glob(pattern: &str) -> String {
    if !pattern.contains("**") {
        return pattern.to_string();
    }
    let parts: Vec<String> = pattern
        .split('/')
        .map(|component| {
            if component == "**" || !component.contains("**") {
                return component.to_string();
            }
            // A run of stars longer than two means the same as two.
            let mut squeezed = component.to_string();
            while squeezed.contains("***") {
                squeezed = squeezed.replace("***", "**");
            }
            let pieces: Vec<&str> = squeezed.split("**").collect();
            let mut out: Vec<String> = Vec::new();
            for (i, piece) in pieces.iter().enumerate() {
                let first = i == 0;
                let last = i == pieces.len() - 1;
                match (piece.is_empty(), first, last) {
                    (true, _, _) => {}
                    // Text before the first `**`: the start of a name.
                    (false, true, _) => out.push(format!("{piece}*")),
                    // Text after the last `**`: the end of a name.
                    (false, _, true) => out.push(format!("*{piece}")),
                    (false, _, _) => out.push(format!("*{piece}*")),
                }
                if !last {
                    out.push("**".to_string());
                }
            }
            out.dedup_by(|a, b| a == "**" && b == "**");
            out.join("/")
        })
        .collect();
    parts.join("/")
}

/// `kept`, when given, is [`not_ignored`]'s answer: anything outside it is
/// skipped exactly as if it were excluded.
fn matching(
    dir: &Path,
    patterns: &[String],
    exclude: &[String],
    kept: Option<&HashSet<String>>,
    hash: bool,
) -> Result<Vec<FileHash>> {
    let mut seen: BTreeMap<String, FileHash> = BTreeMap::new();
    let exclude = Excluder::new(dir, exclude);

    for pattern in patterns {
        let normalized = normalize_glob(pattern);
        // `glob` has no notion of a trailing slash; strip it so `src/` reaches
        // the directory itself and is then walked like any other. A leading
        // `./` goes too, or it would survive into every relative path the walk
        // produces and no stored manifest would ever match.
        let trimmed = normalized
            .trim_end_matches(['/', '\\'])
            .trim_start_matches("./")
            .trim_start_matches(".\\");
        // A trailing `/**` means "everything under here" to anyone who writes
        // it, but `glob` yields nothing for it — so `schemas/**` silently
        // keyed on no files, and an edit under `schemas/` was served from the
        // cache. Walking the directory is what it means.
        let trimmed = match trimmed {
            "**" => "",
            other => other.strip_suffix("/**").unwrap_or(other),
        };
        // `.` — the whole workspace — is a pattern people write and glob does
        // not resolve to anything useful.
        let joined = match trimmed {
            "" | "." => dir.to_path_buf(),
            trimmed => dir.join(trimmed),
        };
        let Some(pattern_str) = joined.to_str() else {
            continue;
        };
        let entries = glob::glob(pattern_str)
            .with_context(|| format!("Invalid cache pattern '{pattern}'"))?;

        for entry in entries.flatten() {
            collect_into(dir, &entry, &exclude, kept, hash, &mut seen)?;
        }
    }

    Ok(seen.into_values().collect())
}

/// Hash `path` into `seen` — or, when it is a directory, everything beneath it.
fn collect_into(
    dir: &Path,
    path: &Path,
    exclude: &Excluder,
    kept: Option<&HashSet<String>>,
    hash: bool,
    seen: &mut BTreeMap<String, FileHash>,
) -> Result<()> {
    let Ok(rel) = path.strip_prefix(dir) else {
        return Ok(());
    };
    // An empty `rel` is `dir` itself — what `inputs: [.]` resolves to. It is a
    // directory to walk, never a file to hash.
    let rel = rel.to_string_lossy().replace('\\', "/");
    // `strip_prefix` is lexical, so `dir/../sibling` strips to `../sibling`
    // rather than failing. Such a path is stored in the manifest and then
    // joined onto the entry's artifact directory, where it escapes into a
    // location every other key for the same target also writes to — one entry
    // silently overwriting another's outputs. The remote refuses it outright.
    // Nothing needs it now that paths are relative to the workspace root.
    if rel.starts_with("../") || rel == ".." {
        bail!(
            "cache path '{rel}' reaches outside {} — write it relative to the \
             workspace root instead (that is where `cache.remote` lives, so it \
             is what every cached path is resolved against)",
            dir.display(),
        );
    }
    if !rel.is_empty() && exclude.matches(&rel) {
        return Ok(());
    }
    if !rel.is_empty() && kept.is_some_and(|kept| !kept.contains(&rel)) {
        return Ok(());
    }

    if path.is_dir() {
        if is_state_dir(path) {
            return Ok(());
        }
        // Sorted so the walk order — and so any error a caller sees — doesn't
        // depend on the filesystem. The hashes end up in a BTreeMap either way.
        let mut children: Vec<PathBuf> = std::fs::read_dir(path)
            .with_context(|| format!("Failed to read {}", path.display()))?
            .flatten()
            .map(|entry| entry.path())
            .collect();
        children.sort();
        for child in children {
            collect_into(dir, &child, exclude, kept, hash, seen)?;
        }
        return Ok(());
    }

    if rel.is_empty() || !path.is_file() || seen.contains_key(&rel) {
        return Ok(());
    }
    let size = path.metadata().map(|m| m.len()).unwrap_or(0);
    seen.insert(
        rel.clone(),
        FileHash {
            path: rel,
            sha256: if hash {
                hash_file(path)?
            } else {
                String::new()
            },
            size,
        },
    );
    Ok(())
}

/// Exclude patterns, compiled once for a whole walk.
///
/// Every path a walk visits is checked against every pattern, and a step in
/// a monorepo excludes each sibling sub-workspace — eighty siblings is eighty
/// patterns. Compiling them per check made that the bulk of planning a
/// `cache.cargo` step: two glob compiles per pattern per file.
struct Excluder {
    dir: PathBuf,
    patterns: Vec<ExcludePattern>,
}

struct ExcludePattern {
    /// As written: matched exactly, and as a directory prefix.
    raw: String,
    /// `raw` without trailing slashes, plus one: the prefix test.
    prefix: String,
    /// The glob over the relative path.
    relative: Option<glob::Pattern>,
    /// The glob joined onto `dir`, matched against the full path.
    full: Option<glob::Pattern>,
}

impl Excluder {
    fn new(dir: &Path, exclude: &[String]) -> Self {
        let patterns = exclude
            .iter()
            .map(|pattern| {
                let normalized = normalize_glob(pattern);
                // A plain path — every sibling sub-workspace is one — matches
                // only itself and what is under it, which the prefix test
                // answers; a glob is only worth running for a glob. Joined
                // onto `dir`, a relative pattern matches just what it matches
                // on its own, so the full-path form is for absolute ones.
                let is_glob = normalized.contains(['*', '?', '[', ']', '{', '}']);
                let absolute = Path::new(&normalized).is_absolute();
                ExcludePattern {
                    raw: pattern.clone(),
                    prefix: format!("{}/", pattern.trim_end_matches('/')),
                    relative: is_glob
                        .then(|| glob::Pattern::new(&normalized).ok())
                        .flatten(),
                    full: absolute
                        .then(|| {
                            dir.join(&normalized)
                                .to_str()
                                .and_then(|full| glob::Pattern::new(full).ok())
                        })
                        .flatten(),
                }
            })
            .collect();
        Excluder {
            dir: dir.to_path_buf(),
            patterns,
        }
    }

    /// Whether `rel` (a `/`-separated path relative to `dir`) matches any
    /// pattern.
    fn matches(&self, rel: &str) -> bool {
        // Match each pattern both as a glob over the relative path and as a
        // bare prefix, so `exclude: [target]` does what everyone expects
        // without anyone having to write `target/**/*`.
        self.patterns.iter().any(|p| {
            rel == p.raw
                || rel.starts_with(&p.prefix)
                || p.relative.as_ref().is_some_and(|g| g.matches(rel))
                || p.full
                    .as_ref()
                    .is_some_and(|g| g.matches_path(&self.dir.join(rel)))
        })
    }
}

/// One sentence about files matched by both `inputs` and `outputs`, naming the
/// first few — the same wording wherever the overlap is reported.
pub fn describe_outputs_in_inputs(files: &[String]) -> String {
    const SHOWN: usize = 5;
    let mut named = files
        .iter()
        .take(SHOWN)
        .cloned()
        .collect::<Vec<_>>()
        .join(", ");
    if files.len() > SHOWN {
        named.push_str(&format!(", and {} more", files.len() - SHOWN));
    }
    format!(
        "{} file(s) match both `cache.inputs` and `cache.outputs`, so they are treated as \
         outputs only — an output is never part of the key. Narrow `inputs` (or add them to \
         `exclude`) to say so: {named}",
        files.len()
    )
}

/// Everything that goes into a cache key, in a stable, inspectable form.
///
/// Serialized to JSON and hashed. Keeping it a real struct rather than an ad-hoc
/// string concatenation means the key is explainable: `ciabatta dry-run -v`
/// can print exactly what it hashed, which is the only way anyone ever debugs
/// "why did this miss?".
///
/// A step has exactly **three** dependencies, and all three are here:
///
/// 1. its **input files**,
/// 2. the **environment variables** it declared,
/// 3. the **outputs of the steps it needs**.
///
/// The third is what makes a graph cacheable rather than just a directory. If
/// `api:build` consumes `proto:generate`'s stubs, then a change to the stubs has
/// to invalidate the api's build — even though not one file under `packages/api`
/// moved. Folding each upstream step's output hash in here is what makes that
/// happen, and it propagates: a change at the root of the graph changes every
/// key downstream of it, exactly once.
#[derive(Debug, Clone, Serialize)]
pub struct KeyInputs {
    /// Bumped when the key derivation changes, so an old cache is missed rather
    /// than misread.
    pub version: u32,
    /// What's being cached — the workflow or workflow name.
    pub target: String,
    /// The workspace it belongs to.
    pub workspace: String,
    /// The command(s) the build runs. A changed build command must change the
    /// key even when every source file is identical.
    pub commands: Vec<String>,
    /// The hashed input files.
    pub inputs: Vec<FileHash>,
    /// The environment variables the config declared, and their values.
    pub env: BTreeMap<String, String>,
    /// The build features that were enabled — every `CIABATTA_FEAT_*` set to a
    /// truthy value, by normalized name.
    ///
    /// Unlike `env` this needs no declaring. A feature changes what a build
    /// produces, so an artifact built with one is not reusable by a build
    /// without it, and leaving that to be remembered per-workspace under
    /// `cache.env` meant one forgotten line served the wrong artifacts.
    #[serde(default)]
    #[serde(skip_serializing_if = "BTreeSet::is_empty")]
    pub features: BTreeSet<String>,
    /// Upstream step name → the hash of that step's output set.
    #[serde(default)]
    pub upstream: BTreeMap<String, String>,
    /// For a `cache.cargo` build, `rustc -vV`: the compiler and the host it
    /// runs on. Absent for every other build, so their keys are unchanged.
    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub toolchain: Option<String>,
}

/// Reduce a set of output hashes to one value, so a downstream step can depend
/// on "what this step produced" without carrying the whole file list.
pub fn fingerprint(outputs: &[FileHash]) -> String {
    let joined: String = outputs
        .iter()
        .map(|o| format!("{}:{}", o.path, o.sha256))
        .collect::<Vec<_>>()
        .join("\n");
    hash_bytes(joined.as_bytes())
}

/// The key-derivation version. Bump it when anything about how a key is
/// computed changes.
///
/// 2: build features joined the key. An entry stored by an earlier ciabatta
/// can't say which features produced it, so it is missed rather than trusted.
///
/// 3: input and output paths became relative to the workspace root rather than
/// to the sub-workspace. The same file is spelled differently now, so a v2
/// entry's manifest would be read against the wrong directory.
pub const KEY_VERSION: u32 = 3;

impl KeyInputs {
    /// The cache key: hex SHA-256 of the canonical JSON encoding.
    pub fn key(&self) -> Result<String> {
        let canonical =
            serde_json::to_vec(self).context("Failed to encode the cache key inputs")?;
        Ok(hash_bytes(&canonical))
    }
}

// ─── Deciding what to do ────────────────────────────────────────────────────

/// What ciabatta would do with one cacheable target.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "outcome")]
pub enum Decision {
    /// Everything matches: the stored outputs are already on disk and correct,
    /// so there's nothing to do at all.
    Fresh {
        key: String,
        /// How many output files were verified.
        outputs: usize,
    },
    /// The key matches a stored entry, and its outputs would be restored.
    Hit {
        key: String,
        /// Where the entry came from.
        source: Source,
        outputs: usize,
    },
    /// The build has to run.
    Rebuild { key: String, reason: Reason },
    /// Caching isn't in play for this target.
    Uncached { reason: String },
}

impl Decision {
    /// Whether this target's build can be skipped.
    pub fn is_reuse(&self) -> bool {
        matches!(self, Decision::Fresh { .. } | Decision::Hit { .. })
    }

    /// The cache key, when one was computed.
    pub fn key(&self) -> Option<&str> {
        match self {
            Decision::Fresh { key, .. }
            | Decision::Hit { key, .. }
            | Decision::Rebuild { key, .. } => Some(key),
            Decision::Uncached { .. } => None,
        }
    }

    /// One line for the terminal.
    pub fn describe(&self) -> String {
        match self {
            Decision::Fresh { outputs, .. } => {
                format!("up to date ({outputs} output file(s) already correct)")
            }
            Decision::Hit {
                source, outputs, ..
            } => format!(
                "cache hit from {} ({outputs} file(s) to restore)",
                source.label()
            ),
            Decision::Rebuild { reason, .. } => format!("rebuild — {}", reason.describe()),
            Decision::Uncached { reason } => format!("not cached — {reason}"),
        }
    }
}

/// Where a cache entry was found.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    Local,
    Remote,
}

impl Source {
    pub fn label(self) -> &'static str {
        match self {
            Source::Local => "the local cache",
            Source::Remote => "the remote cache",
        }
    }
}

/// Why a target has to be rebuilt. Every variant names something the user can
/// go and look at — "cache miss" on its own has never helped anybody.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum Reason {
    /// This key has never been built.
    NeverBuilt,
    /// Input files changed since the entry was stored.
    InputsChanged {
        /// Paths that were added, removed, or edited — capped for display.
        changed: Vec<String>,
        /// How many changed in total, when more than `changed` lists.
        total: usize,
    },
    /// No input file changed, but variables that key it did — names only,
    /// since values can be secrets.
    EnvChanged { vars: Vec<String> },
    /// The entry exists, but the outputs it promised aren't on disk and can't
    /// be restored.
    OutputsMissing { missing: Vec<String> },
    /// The outputs on disk don't match what was stored — somebody edited a
    /// generated file, or a partial build left something behind.
    OutputsModified { modified: Vec<String> },
    /// The build declares no outputs, so there is nothing to restore and
    /// nothing a hit could save.
    NoOutputs,
    /// A step this one needs ran, and declares no outputs — so there is no
    /// telling whether what this step consumes moved. See
    /// [`CacheConfig::accounts_for_its_outputs`].
    UpstreamReran { steps: Vec<String> },
    /// The run was started with `--force`, which ignores the cache.
    Forced,
}

impl Reason {
    pub fn describe(&self) -> String {
        match self {
            Reason::NeverBuilt => "this input set has never been built".to_string(),
            // Neither a file nor a variable: the command, an upstream step's
            // output, or the cache settings themselves.
            Reason::InputsChanged { changed, .. } if changed.is_empty() => {
                "its key changed though no input file did — its command, an upstream step's \
                 output, or its cache settings moved"
                    .to_string()
            }
            Reason::EnvChanged { vars } => {
                format!("variable(s) it keys on changed: {}", vars.join(", "))
            }
            Reason::InputsChanged { changed, total } => {
                let listed = changed.join(", ");
                if *total > changed.len() {
                    format!("{total} input file(s) changed, including {listed}",)
                } else {
                    format!("input file(s) changed: {listed}")
                }
            }
            Reason::OutputsMissing { missing } => {
                format!("expected output(s) are missing: {}", missing.join(", "))
            }
            Reason::OutputsModified { modified } => format!(
                "output(s) have been modified since they were built: {}",
                modified.join(", ")
            ),
            Reason::NoOutputs => "no `cache.outputs` are declared, so there'd be nothing to \
                 restore (set `cache.no_outputs: true` if it writes nothing)"
                .to_string(),
            Reason::UpstreamReran { steps } => format!(
                "{} ran and declares no `cache.outputs`, so there's no telling whether what \
                 this step consumes changed (give {} `cache.outputs`, or `cache.no_outputs: \
                 true` if it writes nothing)",
                steps.join(", "),
                if steps.len() == 1 { "it" } else { "them" },
            ),
            Reason::Forced => "the run was started with --force".to_string(),
        }
    }
}

/// How many changed paths to name before summarizing.
const MAX_LISTED: usize = 5;

/// Compare a freshly computed input set against a stored manifest and say what
/// changed, in path order.
pub fn changed_inputs(stored: &[FileHash], current: &[FileHash]) -> Vec<String> {
    let stored_by_path: BTreeMap<&str, &FileHash> =
        stored.iter().map(|f| (f.path.as_str(), f)).collect();
    let current_by_path: BTreeMap<&str, &FileHash> =
        current.iter().map(|f| (f.path.as_str(), f)).collect();

    let mut changed: Vec<String> = Vec::new();
    for (path, file) in &current_by_path {
        match stored_by_path.get(path) {
            None => changed.push(format!("{path} (new)")),
            Some(before) if before.sha256 != file.sha256 => changed.push((*path).to_string()),
            Some(_) => {}
        }
    }
    for path in stored_by_path.keys() {
        if !current_by_path.contains_key(path) {
            changed.push(format!("{path} (gone)"));
        }
    }
    changed.sort();
    changed
}

/// Trim a change list down to something worth printing.
pub fn summarize(changed: Vec<String>) -> Reason {
    let total = changed.len();
    let mut listed = changed;
    listed.truncate(MAX_LISTED);
    Reason::InputsChanged {
        changed: listed,
        total,
    }
}

// ─── Planning a target ──────────────────────────────────────────────────────

/// One cacheable unit: a workspace's build, identified by the target that runs
/// it.
#[derive(Debug, Clone)]
pub struct Target {
    /// The workflow or workflow name.
    pub name: String,
    /// The workspace the build belongs to.
    pub workspace: String,
    /// The workspace root. Every `inputs`/`outputs` path is relative to it —
    /// see the module docs on why it is the root and not the member's own
    /// directory.
    pub dir: PathBuf,
    /// The step's own sub-workspace, relative to `dir`, so its files are not
    /// excluded from its own inputs. `None` for a step belonging to the root.
    pub member: Option<String>,
    /// The commands the build runs, folded into the key.
    pub commands: Vec<String>,
    /// The cache settings in force for it.
    pub config: CacheConfig,
    /// The steps this one needs, and the fingerprint of what each produced.
    ///
    /// Filled in by the runner as the graph executes — a step can't be keyed
    /// until the steps it depends on have produced something to key against.
    pub upstream: BTreeMap<String, String>,
}

/// Work out what would happen to `target`, without running or restoring
/// anything.
///
/// This is the whole of `ciabatta dry-run`, and it's also what the runner calls
/// before a build — so the preview and the real thing cannot disagree.
pub fn plan(
    target: &Target,
    env: &BTreeMap<String, String>,
    store: &store::Store,
) -> Result<Decision> {
    if let Some(reason) = target.config.why_disabled() {
        return Ok(Decision::Uncached {
            reason: reason.to_string(),
        });
    }

    let inputs = target
        .config
        .hash_inputs(&target.dir, target.member.as_deref())?;
    // Inputs were declared and matched files, but every one of them is an
    // output. Keying on the empty set that leaves would key identically
    // forever — the one way dropping outputs from inputs could turn a slow
    // cache into a wrong one.
    if inputs.is_empty()
        && !target
            .config
            .outputs_in_inputs(&target.dir, target.member.as_deref())?
            .is_empty()
    {
        return Ok(Decision::Uncached {
            reason: "every file its `cache.inputs` match is also one of its `cache.outputs`, \
                     so there is nothing left to key on"
                .to_string(),
        });
    }
    let key_inputs = KeyInputs {
        version: KEY_VERSION,
        target: target.name.clone(),
        workspace: target.workspace.clone(),
        commands: target.commands.clone(),
        inputs,
        env: target
            .config
            .env
            .iter()
            .map(|name| (name.clone(), target.config.env_value(name, env)))
            .collect(),
        // Not declared and not per-target: every `CIABATTA_FEAT_*` in the
        // environment, read straight out of the map every caller already
        // passes. A feature is a property of the build, so deriving it here
        // means no caller can construct a `Target` that forgets it.
        features: crate::environment::features::Features::from_pairs(
            env.iter().map(|(k, v)| (k.as_str(), v.as_str())),
        )
        .key_material(),
        upstream: target.upstream.clone(),
        toolchain: target
            .config
            .cargo_resolved(&target.dir)?
            .map(|resolved| resolved.toolchain),
    };
    let key = key_inputs.key()?;

    if !target.config.accounts_for_its_outputs() {
        return Ok(Decision::Rebuild {
            key,
            reason: Reason::NoOutputs,
        });
    }

    // Remember what was hashed, so a rebuild can record it and the next miss
    // can say which of the three dependencies moved.
    let Some(entry) = store.get(&key)? else {
        // Nothing stored under this key. Say whether that's because the inputs
        // moved (the useful answer) or because it's simply never been built.
        let reason = match store.latest_for(&target.workspace, &target.name)? {
            Some(previous) => {
                let files = changed_inputs(&previous.inputs, &key_inputs.inputs);
                let vars: Vec<String> = key_inputs
                    .env
                    .keys()
                    .chain(previous.env.keys())
                    .filter(|name| key_inputs.env.get(*name) != previous.env.get(*name))
                    .cloned()
                    .collect::<BTreeSet<_>>()
                    .into_iter()
                    .collect();
                if files.is_empty() && !vars.is_empty() {
                    Reason::EnvChanged { vars }
                } else {
                    summarize(files)
                }
            }
            None => Reason::NeverBuilt,
        };
        return Ok(Decision::Rebuild { key, reason });
    };

    // The key matched. That only says the inputs are unchanged — check the
    // outputs really are what was stored before calling it a hit.
    let mut missing: Vec<String> = Vec::new();
    let mut modified: Vec<String> = Vec::new();
    for output in &entry.outputs {
        let path = target.dir.join(&output.path);
        if !path.is_file() {
            missing.push(output.path.clone());
            continue;
        }
        if hash_file(&path)? != output.sha256 {
            modified.push(output.path.clone());
        }
    }

    if missing.is_empty() && modified.is_empty() {
        return Ok(Decision::Fresh {
            key,
            outputs: entry.outputs.len(),
        });
    }

    // Outputs are wrong on disk but the store still holds a good copy, so this
    // is a restore rather than a rebuild.
    if store.has_artifacts(&key)? {
        return Ok(Decision::Hit {
            key,
            source: Source::Local,
            outputs: entry.outputs.len(),
        });
    }

    Ok(Decision::Rebuild {
        key,
        reason: if !missing.is_empty() {
            missing.truncate(MAX_LISTED);
            Reason::OutputsMissing { missing }
        } else {
            modified.truncate(MAX_LISTED);
            Reason::OutputsModified { modified }
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ciab_cache_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write(dir: &Path, rel: &str, body: &str) {
        let path = dir.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }

    #[test]
    fn a_directory_pattern_reaches_every_file_below_it() {
        let dir = scratch("nested");
        write(&dir, "src/main.rs", "fn main() {}");
        write(&dir, "src/a/b/c/deep.rs", "// deep");
        write(&dir, "src/a/b/other.rs", "// other");

        // The three spellings of "everything under src" must agree. A pattern
        // that silently matched nothing would leave a build keyed on an empty
        // input set — which hits forever.
        let bare = hash_matching(&dir, &["src".to_string()], &[]).unwrap();
        let slashed = hash_matching(&dir, &["src/".to_string()], &[]).unwrap();
        let globbed = hash_matching(&dir, &["src/**/*".to_string()], &[]).unwrap();

        assert_eq!(bare.len(), 3, "a bare directory means the tree below it");
        assert_eq!(bare, slashed);
        assert_eq!(bare, globbed);
        assert!(bare.iter().any(|f| f.path == "src/a/b/c/deep.rs"));

        // Paths are `/`-separated whatever the platform, so a key computed on
        // Windows matches one computed on Linux.
        assert!(bare.iter().all(|f| !f.path.contains('\\')));

        // Excluding a directory excludes the tree below it.
        let trimmed = hash_matching(&dir, &["src".to_string()], &["src/a/b".to_string()]).unwrap();
        assert_eq!(
            trimmed.iter().map(|f| f.path.as_str()).collect::<Vec<_>>(),
            vec!["src/main.rs"]
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A super-workspace's globs cannot help but sweep up its members. Each
    /// member owns its own cache entry, so counting its files again upstream
    /// means the root never hits and the work is done twice.
    #[test]
    fn a_super_workspace_ignores_its_sub_workspaces() {
        let dir = scratch("members");
        write(&dir, "tools/shared.sh", "# shared");
        write(
            &dir,
            "packages/api/.ciabatta/ciabatta.yaml",
            "workspace:\n  name: api\n",
        );
        write(&dir, "packages/api/src/main.rs", "fn main() {}");
        write(&dir, "packages/api/nested/deep.rs", "// still the api's");
        write(&dir, "packages/loose/notes.md", "not a workspace");

        assert_eq!(
            nested_workspaces_except(&dir, None),
            vec!["packages/api".to_string()]
        );

        let config = CacheConfig {
            enabled: Some(true),
            inputs: vec![".".to_string()],
            ..Default::default()
        };
        let inputs = config.hash_inputs(&dir, None).unwrap();
        let paths: Vec<&str> = inputs.iter().map(|f| f.path.as_str()).collect();

        assert!(paths.contains(&"tools/shared.sh"));
        assert!(
            paths.contains(&"packages/loose/notes.md"),
            "a plain directory is still the super-workspace's to claim"
        );
        assert!(
            !paths.iter().any(|p| p.starts_with("packages/api/")),
            "a member's files belong to the member: {paths:?}"
        );

        // Outputs are not filtered the same way: a generator writing stubs into
        // the package that consumes them must still be able to record them.
        let producing = CacheConfig {
            outputs: vec!["packages/api/nested/**/*".to_string()],
            ..Default::default()
        };
        assert_eq!(producing.hash_outputs(&dir).unwrap().len(), 1);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The member's *own* build is the exception to that exclusion.
    ///
    /// Its inputs are named through the root (`packages/api/src/**/*`), so
    /// excluding every member from every step would exclude the step's entire
    /// reason for existing — leaving it keyed on nothing, which hits forever.
    /// Members nested in members: inside the step's own one they are still
    /// somebody else's; inside somebody else's they go with it. The scan is
    /// shared by every step of a plan, so this is worked out from one list.
    #[test]
    fn nested_members_are_excluded_by_whoever_contains_them() {
        let dir = scratch("nestednest");
        for member in ["api", "api/plugins/auth", "web", "web/extras/lib"] {
            write(
                &dir,
                &format!("{member}/.ciabatta/ciabatta.yaml"),
                "workspace: {}\n",
            );
        }
        let mut except_api = nested_workspaces_except(&dir, Some("api"));
        except_api.sort();
        assert_eq!(except_api, ["api/plugins/auth", "web"]);
        let mut root = nested_workspaces_except(&dir, None);
        root.sort();
        assert_eq!(root, ["api", "web"]);
        // Planning remembers the scan; the answer is the same through it.
        scope::planning(|| {
            let mut again = nested_workspaces_except(&dir, Some("api"));
            again.sort();
            assert_eq!(again, except_api);
            assert_eq!(nested_workspaces_except(&dir, Some("web")).len(), 2);
        });
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// ciabatta's own store and history are never inputs or worth walking —
    /// whether or not the project's `.gitignore` says so.
    #[test]
    fn the_cache_store_is_never_an_input() {
        let dir = scratch("statedirs");
        write(&dir, "src/main.rs", "fn main() {}");
        write(&dir, ".ciabatta/ciabatta.yaml", "workspace: {}\n");
        write(&dir, ".ciabatta/cache/entries/abc.json", "{}");
        write(&dir, ".ciabatta/history/workflows.json", "{}");
        write(&dir, "api/.ciabatta/cache/artifacts/x", "blob");
        let config = CacheConfig {
            enabled: Some(true),
            inputs: vec![".".to_string()],
            ..Default::default()
        };
        let read: Vec<String> = config
            .list_inputs(&dir, None)
            .unwrap()
            .into_iter()
            .map(|f| f.path)
            .collect();
        assert!(
            read.contains(&".ciabatta/ciabatta.yaml".to_string()),
            "{read:?}"
        );
        assert!(
            read.iter()
                .all(|p| !p.contains("/cache/") && !p.contains("history")),
            "{read:?}"
        );
        let kept = not_ignored(&dir).unwrap();
        assert!(
            !kept.iter().any(|p| p.contains(".ciabatta/cache")),
            "{kept:?}"
        );
        assert!(
            every_path(&dir)
                .unwrap()
                .iter()
                .all(|(p, _)| !p.contains(".ciabatta/cache"))
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Compiled once, exclude patterns still mean what they did: a plain path
    /// and everything under it, a glob, and an absolute pattern.
    #[test]
    fn exclude_patterns_match_as_paths_globs_and_absolute_paths() {
        let dir = Path::new("/work");
        let exclude = Excluder::new(
            dir,
            &[
                "dist".to_string(),
                "gen/".to_string(),
                "**/*.log".to_string(),
                "/work/vendor/*".to_string(),
            ],
        );
        for (rel, excluded) in [
            ("dist", true),
            ("dist/app.js", true),
            ("distant/x", false),
            ("gen/a.rs", true),
            ("src/debug.log", true),
            ("src/main.rs", false),
            ("vendor/lib", true),
        ] {
            assert_eq!(exclude.matches(rel), excluded, "{rel}");
        }
    }

    #[test]
    fn a_members_own_files_survive_the_nested_workspace_exclusion() {
        let dir = scratch("ownmember");
        write(
            &dir,
            "packages/api/.ciabatta/ciabatta.yaml",
            "workspace:\n  name: api\n",
        );
        write(
            &dir,
            "packages/web/.ciabatta/ciabatta.yaml",
            "workspace:\n  name: web\n",
        );
        write(&dir, "packages/api/src/main.rs", "fn main() {}");
        write(&dir, "packages/web/src/app.ts", "export {}");

        // Everyone but the api is still somebody else's entry.
        assert_eq!(
            nested_workspaces_except(&dir, Some("packages/api")),
            vec!["packages/web".to_string()]
        );

        let config = CacheConfig {
            enabled: Some(true),
            inputs: vec!["packages/api/src/**/*".to_string()],
            ..Default::default()
        };
        let paths: Vec<String> = config
            .hash_inputs(&dir, Some("packages/api"))
            .unwrap()
            .into_iter()
            .map(|f| f.path)
            .collect();
        assert_eq!(paths, vec!["packages/api/src/main.rs".to_string()]);

        // Without it, the same declaration matches nothing at all — the silent
        // failure this exists to prevent.
        assert!(config.hash_inputs(&dir, None).unwrap().is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A path that climbs out of the workspace root is refused.
    ///
    /// `strip_prefix` is lexical, so `root/../sibling` used to strip to
    /// `../sibling` and be stored in the manifest verbatim. Joining that onto
    /// the entry's artifact directory walked back out of it, so every key for
    /// that target wrote its output to one shared location and restoring one
    /// entry handed back another's file. The remote cache rejects such a path
    /// outright, so those steps could never be shared at all.
    #[test]
    fn a_pattern_climbing_out_of_the_root_is_refused_not_stored() {
        let dir = scratch("escape");
        write(&dir, "workspace/src/main.rs", "fn main() {}");
        write(&dir, "outside/secret.txt", "not ours");

        let root = dir.join("workspace");
        let err = hash_matching(&root, &["../outside/*.txt".to_string()], &[])
            .expect_err("a path leaving the root must not be silently stored");
        let message = format!("{err:#}");
        assert!(
            message.contains("reaches outside"),
            "the error has to name the problem: {message}"
        );

        // And the ordinary case is untouched.
        assert_eq!(
            hash_matching(&root, &["src/**/*".to_string()], &[])
                .unwrap()
                .len(),
            1
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A workflow's `env:` and `required_env` key the cache without being
    /// repeated under `cache.env`, and a required value — a token, usually —
    /// goes in as a digest, never as written.
    #[test]
    fn a_steps_env_and_required_env_key_it_without_cache_env() {
        let step = crate::run::RunStep {
            name: "build".into(),
            env: [("MODE".to_string(), "release".to_string())]
                .into_iter()
                .collect(),
            required_env: vec!["API_TOKEN".into(), "MODE".into()],
            ..Default::default()
        };
        let config = CacheConfig {
            env: vec!["PROFILE".into()],
            ..Default::default()
        }
        .with_step_env(&step);
        assert_eq!(config.env, vec!["API_TOKEN", "MODE", "PROFILE"]);
        // MODE's value is written in the file, so it isn't treated as secret.
        assert_eq!(config.secret_env, vec!["API_TOKEN"]);

        let env: BTreeMap<String, String> = [
            ("API_TOKEN".to_string(), "hunter2".to_string()),
            ("MODE".to_string(), "release".to_string()),
        ]
        .into_iter()
        .collect();
        let token = config.env_value("API_TOKEN", &env);
        assert!(token.starts_with("sha256:") && !token.contains("hunter2"));
        assert_eq!(config.env_value("MODE", &env), "release");
        assert_eq!(config.env_value("PROFILE", &env), "");

        // And the key moves when either changes.
        let dir = scratch("stepenv");
        write(&dir, "src/main.rs", "fn main() {}");
        let target = |config: &CacheConfig| Target {
            name: "build".into(),
            workspace: ".".into(),
            dir: dir.clone(),
            member: None,
            commands: vec!["make".into()],
            config: CacheConfig {
                enabled: Some(true),
                inputs: vec!["src".into()],
                outputs: vec!["out".into()],
                ..config.clone()
            },
            upstream: BTreeMap::new(),
        };
        let store = store::Store::at(dir.join(".store")).unwrap();
        let key = |env: &BTreeMap<String, String>| {
            plan(&target(&config), env, &store)
                .unwrap()
                .key()
                .unwrap()
                .to_string()
        };
        let base = key(&env);
        let mut other_token = env.clone();
        other_token.insert("API_TOKEN".into(), "different".into());
        let mut other_mode = env.clone();
        other_mode.insert("MODE".into(), "debug".into());
        assert_ne!(base, key(&other_token));
        assert_ne!(base, key(&other_mode));
        assert_eq!(base, key(&env));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_double_star_glued_to_text_is_rewritten_into_whole_components() {
        assert_eq!(normalize_glob("**.json"), "**/*.json");
        assert_eq!(normalize_glob("config/**.json"), "config/**/*.json");
        assert_eq!(normalize_glob("assets/img**"), "assets/img*/**");
        assert_eq!(normalize_glob("a**b"), "a*/**/*b");
        assert_eq!(normalize_glob("***.json"), "**/*.json");
        // Already strict: untouched.
        assert_eq!(normalize_glob("src/**/*.rs"), "src/**/*.rs");
        assert_eq!(normalize_glob("dist/**"), "dist/**");
        assert_eq!(normalize_glob("*.json"), "*.json");
    }

    /// `**.json` used to be refused outright as an invalid pattern.
    #[test]
    fn a_double_star_extension_pattern_matches_at_every_depth() {
        let dir = scratch("double-star-ext");
        write(&dir, "package.json", "{}");
        write(&dir, "config/app.json", "{}");
        write(&dir, "config/deep/more.json", "{}");
        write(&dir, "config/readme.md", "# no");

        let all = hash_matching(&dir, &["**.json".to_string()], &[]).unwrap();
        let paths: Vec<&str> = all.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(
            paths,
            vec!["config/app.json", "config/deep/more.json", "package.json"]
        );

        let under = hash_matching(&dir, &["config/**.json".to_string()], &[]).unwrap();
        assert_eq!(under.len(), 2);

        // And as an exclude.
        let kept = hash_matching(&dir, &["config".to_string()], &["**.json".to_string()]).unwrap();
        let kept: Vec<&str> = kept.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(kept, vec!["config/readme.md"]);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Listing must see exactly what hashing sees, or a summary would describe
    /// a different build from the one that runs.
    #[test]
    fn listing_matches_hashing_without_reading_a_byte() {
        let dir = scratch("listing");
        write(&dir, "src/a.rs", "fn a() {}");
        write(&dir, "src/b/c.rs", "fn c() {}");

        let patterns = vec!["src".to_string()];
        let hashed = hash_matching(&dir, &patterns, &[]).unwrap();
        let listed = list_matching(&dir, &patterns, &[]).unwrap();

        assert_eq!(
            hashed.iter().map(|f| &f.path).collect::<Vec<_>>(),
            listed.iter().map(|f| &f.path).collect::<Vec<_>>()
        );
        assert_eq!(
            hashed.iter().map(|f| f.size).sum::<u64>(),
            listed.iter().map(|f| f.size).sum::<u64>()
        );
        assert!(listed.iter().all(|f| f.sha256.is_empty()));
        assert!(hashed.iter().all(|f| !f.sha256.is_empty()));

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `gitignore: true` drops what git would ignore — nested `.gitignore`s
    /// and negations included — and keeps dot-files that aren't ignored.
    #[test]
    fn gitignore_keeps_build_output_out_of_inputs() {
        let dir = scratch("gitignore");
        write(&dir, ".gitignore", "target/\n*.log\n!keep.log\n");
        write(&dir, "crates/api/src/main.rs", "fn main() {}");
        write(&dir, "crates/api/target/debug/api", "binary");
        write(&dir, "crates/api/.gitignore", "generated/\n");
        write(&dir, "crates/api/generated/out.rs", "// derived");
        write(&dir, "crates/api/build.log", "noise");
        write(&dir, "crates/api/keep.log", "wanted");
        write(&dir, ".cargo/config.toml", "[build]");

        let mut config = CacheConfig {
            enabled: Some(true),
            inputs: vec!["crates/**/*".to_string(), ".cargo/**/*".to_string()],
            ..Default::default()
        };
        let everything: Vec<String> = config
            .hash_inputs(&dir, None)
            .unwrap()
            .into_iter()
            .map(|f| f.path)
            .collect();
        assert!(everything.contains(&"crates/api/target/debug/api".to_string()));

        config.gitignore = Some(true);
        let hashed: Vec<String> = config
            .hash_inputs(&dir, None)
            .unwrap()
            .into_iter()
            .map(|f| f.path)
            .collect();
        assert_eq!(
            hashed,
            vec![
                ".cargo/config.toml",
                "crates/api/.gitignore",
                "crates/api/keep.log",
                "crates/api/src/main.rs",
            ]
        );
        let listed: Vec<String> = config
            .list_inputs(&dir, None)
            .unwrap()
            .into_iter()
            .map(|f| f.path)
            .collect();
        assert_eq!(hashed, listed);

        // Outputs are never filtered: they live in ignored directories.
        config.outputs = vec!["crates/api/target/debug/api".to_string()];
        assert_eq!(config.hash_outputs(&dir).unwrap().len(), 1);

        // Nor is what an upstream step wrote, read through `inputs_from`.
        config.outputs.clear();
        config.upstream_outputs = vec!["crates/api/generated".to_string()];
        let hashed: Vec<String> = config
            .hash_inputs(&dir, None)
            .unwrap()
            .into_iter()
            .map(|f| f.path)
            .collect();
        assert!(
            hashed.contains(&"crates/api/generated/out.rs".to_string()),
            "{hashed:?}"
        );
        assert!(!hashed.contains(&"crates/api/build.log".to_string()));

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A level that doesn't mention `gitignore` inherits it.
    #[test]
    fn gitignore_is_inherited_by_a_step_that_does_not_mention_it() {
        let workflow = CacheConfig {
            gitignore: Some(true),
            ..Default::default()
        };
        let step = CacheConfig {
            env: vec!["PROFILE".to_string()],
            ..Default::default()
        };
        assert_eq!(
            graph::effective(Some(&workflow), Some(&step)).gitignore,
            Some(true)
        );
    }

    #[test]
    fn caching_is_off_until_it_is_asked_for() {
        let off = CacheConfig::default();
        assert!(off.why_disabled().unwrap().contains("caching is off"));

        // Enabled but with nothing to hash is not a cache — every build would
        // key identically and the first result would be served forever.
        let empty = CacheConfig {
            enabled: Some(true),
            ..Default::default()
        };
        assert!(empty.why_disabled().unwrap().contains("cache.inputs"));

        let real = CacheConfig {
            enabled: Some(true),
            inputs: vec!["src/**/*".into()],
            ..Default::default()
        };
        assert!(real.why_disabled().is_none());
    }

    #[test]
    fn hashing_is_stable_sorted_and_content_addressed() {
        let dir = scratch("hash");
        write(&dir, "src/a.rs", "fn a() {}");
        write(&dir, "src/b.rs", "fn b() {}");
        write(&dir, "target/junk.o", "binary");

        let patterns = vec!["src/**/*.rs".to_string(), "target/**/*".to_string()];
        let all = hash_matching(&dir, &patterns, &[]).unwrap();
        let paths: Vec<&str> = all.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, vec!["src/a.rs", "src/b.rs", "target/junk.o"]);

        // Excluding by bare directory name is what everyone expects to work.
        let excluded = hash_matching(&dir, &patterns, &["target".to_string()]).unwrap();
        let paths: Vec<&str> = excluded.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, vec!["src/a.rs", "src/b.rs"]);

        // Same contents → same hash; changed contents → different hash.
        let again = hash_matching(&dir, &patterns, &["target".to_string()]).unwrap();
        assert_eq!(excluded, again);
        write(&dir, "src/a.rs", "fn a() { changed }");
        let after = hash_matching(&dir, &patterns, &["target".to_string()]).unwrap();
        assert_ne!(excluded[0].sha256, after[0].sha256);
        assert_eq!(excluded[1].sha256, after[1].sha256, "b.rs didn't move");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `cache init` proposes `outputs: dist/**/*` alongside `exclude: dist`,
    /// so applying `exclude` to outputs would erase them — and a build with no
    /// outputs is never cached at all. The two must stay separate.
    #[test]
    fn exclude_filters_inputs_and_never_outputs() {
        let dir = scratch("exclude_scope");
        write(&dir, "src/main.rs", "fn main() {}");
        write(&dir, "dist/app", "compiled");

        let config = CacheConfig {
            enabled: Some(true),
            inputs: vec!["src/**/*".into(), "dist/**/*".into()],
            outputs: vec!["dist/**/*".into()],
            exclude: vec!["dist".into()],
            ..Default::default()
        };

        let inputs = config.hash_inputs(&dir, None).unwrap();
        assert_eq!(
            inputs.iter().map(|f| f.path.as_str()).collect::<Vec<_>>(),
            vec!["src/main.rs"],
            "build output under an inputs glob must not count as an input"
        );

        let outputs = config.hash_outputs(&dir).unwrap();
        assert_eq!(
            outputs.iter().map(|f| f.path.as_str()).collect::<Vec<_>>(),
            vec!["dist/app"],
            "excluding an output directory must not erase the outputs themselves"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_trailing_double_star_matches_everything_under_the_directory() {
        let dir = scratch("double_star");
        write(&dir, "schemas/storage.json", "{}");
        write(&dir, "schemas/nested/more.json", "{}");
        write(&dir, "other.txt", "x");
        let config = CacheConfig {
            inputs: vec!["schemas/**".into()],
            ..Default::default()
        };
        let paths: Vec<String> = config
            .list_inputs(&dir, None)
            .unwrap()
            .into_iter()
            .map(|f| f.path)
            .collect();
        assert_eq!(
            paths,
            vec!["schemas/nested/more.json", "schemas/storage.json"]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_key_covers_inputs_commands_and_declared_env() {
        let base = KeyInputs {
            version: KEY_VERSION,
            target: "build".into(),
            workspace: "api".into(),
            commands: vec!["cargo build".into()],
            inputs: vec![FileHash {
                path: "src/a.rs".into(),
                sha256: "abc".into(),
                size: 9,
            }],
            env: BTreeMap::new(),
            features: BTreeSet::new(),
            upstream: BTreeMap::new(),
            toolchain: None,
        };
        let key = base.key().unwrap();
        assert_eq!(key.len(), 64, "a hex SHA-256");

        // Recomputing the same inputs gives the same key.
        assert_eq!(base.key().unwrap(), key);

        // A changed build command must change the key even with identical files.
        let mut other = base.clone();
        other.commands = vec!["cargo build --release".into()];
        assert_ne!(other.key().unwrap(), key);

        // So must a declared environment variable's value.
        let mut with_env = base.clone();
        with_env.env.insert("PROFILE".into(), "release".into());
        assert_ne!(with_env.key().unwrap(), key);

        // And a changed input file.
        let mut edited = base.clone();
        edited.inputs[0].sha256 = "def".into();
        assert_ne!(edited.key().unwrap(), key);
    }

    /// An output is never an input. With `inputs: ["."]` and no `exclude`,
    /// the build's own output used to land in its key: the first run keyed on
    /// a tree without it, and every later run on a tree with it, so the step
    /// never hit. Now the output is left out — the key is the same before and
    /// after the build — and the overlap is reported.
    #[test]
    fn an_output_is_never_part_of_the_key_and_the_overlap_is_reported() {
        let root = scratch("outputs_not_inputs");
        let work = root.join("api");
        std::fs::create_dir_all(&work).unwrap();
        write(&work, "src/main.rs", "fn main() {}");
        let store = store::Store::at(root.join("cache")).unwrap();
        let target = Target {
            name: "build".into(),
            workspace: "api".into(),
            dir: work.clone(),
            member: None,
            commands: vec!["make".into()],
            config: CacheConfig {
                enabled: Some(true),
                inputs: vec![".".into()],
                outputs: vec!["dist/**/*".into(), "bin/app".into()],
                ..Default::default()
            },
            upstream: BTreeMap::new(),
        };
        let env = BTreeMap::new();

        let before = plan(&target, &env, &store).unwrap();
        let key = before.key().unwrap().to_string();
        assert!(
            target
                .config
                .outputs_in_inputs(&work, None)
                .unwrap()
                .is_empty()
        );

        // The build writes its outputs inside what `inputs` covers.
        write(&work, "dist/app.js", "built");
        write(&work, "dist/app.css", "built");
        write(&work, "bin/app", "built");
        let hashed: Vec<String> = target
            .config
            .hash_inputs(&work, None)
            .unwrap()
            .into_iter()
            .map(|f| f.path)
            .collect();
        assert_eq!(hashed, vec!["src/main.rs"]);
        assert_eq!(
            target.config.outputs_in_inputs(&work, None).unwrap(),
            vec!["bin/app", "dist/app.css", "dist/app.js"]
        );

        // Same key after the build as before it — the bug this fixes.
        let after = plan(&target, &env, &store).unwrap();
        assert_eq!(after.key(), Some(key.as_str()));

        let said =
            describe_outputs_in_inputs(&target.config.outputs_in_inputs(&work, None).unwrap());
        assert!(said.starts_with("3 file(s) match both"), "{said}");
        assert!(said.contains("dist/app.js"), "{said}");

        let _ = std::fs::remove_dir_all(&root);
    }

    /// When every input is also an output there is nothing left to key on,
    /// and an empty key would hit forever — so the step isn't cached at all.
    #[test]
    fn a_step_whose_inputs_are_all_outputs_is_not_cached() {
        let root = scratch("all_outputs");
        let work = root.join("api");
        write(&work, "dist/app.js", "built");
        let store = store::Store::at(root.join("cache")).unwrap();
        let target = Target {
            name: "build".into(),
            workspace: "api".into(),
            dir: work.clone(),
            member: None,
            commands: vec!["make".into()],
            config: CacheConfig {
                enabled: Some(true),
                inputs: vec!["dist/**/*".into()],
                outputs: vec!["dist".into()],
                ..Default::default()
            },
            upstream: BTreeMap::new(),
        };
        match plan(&target, &BTreeMap::new(), &store).unwrap() {
            Decision::Uncached { reason } => {
                assert!(reason.contains("nothing left to key on"), "{reason}")
            }
            other => panic!("expected uncached, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The listing, many: the warning names the first few and counts the rest.
    #[test]
    fn the_overlap_warning_names_a_few_and_counts_the_rest() {
        let files: Vec<String> = (0..8).map(|i| format!("dist/{i}.js")).collect();
        let said = describe_outputs_in_inputs(&files);
        assert!(said.contains("dist/4.js, and 3 more"), "{said}");
        assert!(!said.contains("dist/5.js"), "{said}");
    }

    /// A full pass over the decision table: never built → rebuild, built and
    /// intact → fresh, outputs deleted → restore, inputs edited → rebuild with
    /// the changed file named.
    #[test]
    fn the_plan_walks_the_whole_decision_table() {
        let root = scratch("plan");
        let work = root.join("api");
        std::fs::create_dir_all(&work).unwrap();
        write(&work, "src/main.rs", "fn main() {}");
        let store = store::Store::at(root.join("cache")).unwrap();

        let target = Target {
            name: "build".into(),
            workspace: "api".into(),
            dir: work.clone(),
            member: None,
            commands: vec!["make".into()],
            config: CacheConfig {
                enabled: Some(true),
                inputs: vec!["src/**/*.rs".into()],
                outputs: vec!["dist/**/*".into()],
                ..Default::default()
            },
            upstream: BTreeMap::new(),
        };
        let env = BTreeMap::new();

        // 1. Nothing has ever been built.
        let decision = plan(&target, &env, &store).unwrap();
        let key = decision.key().unwrap().to_string();
        assert_eq!(
            decision,
            Decision::Rebuild {
                key: key.clone(),
                reason: Reason::NeverBuilt
            }
        );
        assert!(!decision.is_reuse());

        // Build it, and store what came out.
        write(&work, "dist/app", "compiled");
        let inputs = hash_matching(&work, &target.config.inputs, &[]).unwrap();
        let outputs = hash_matching(&work, &target.config.outputs, &[]).unwrap();
        store
            .put(
                &key,
                &work,
                store::Build {
                    target: "build".into(),
                    workspace: "api".into(),
                    inputs,
                    outputs,
                    duration_ms: 1234,
                    ..Default::default()
                },
            )
            .unwrap();

        // 2. Everything matches and the outputs are already in place.
        let decision = plan(&target, &env, &store).unwrap();
        assert_eq!(
            decision,
            Decision::Fresh {
                key: key.clone(),
                outputs: 1
            }
        );
        assert!(decision.is_reuse());

        // 3. The output was deleted, but the store still has it — restore.
        std::fs::remove_file(work.join("dist/app")).unwrap();
        let decision = plan(&target, &env, &store).unwrap();
        assert_eq!(
            decision,
            Decision::Hit {
                key: key.clone(),
                source: Source::Local,
                outputs: 1
            }
        );

        // …and restoring really does put it back.
        store.restore(&key, &work).unwrap();
        assert_eq!(
            std::fs::read_to_string(work.join("dist/app")).unwrap(),
            "compiled"
        );
        assert_eq!(
            plan(&target, &env, &store).unwrap(),
            Decision::Fresh {
                key: key.clone(),
                outputs: 1
            }
        );

        // 4. An input changed: a different key, and the miss names the file.
        write(&work, "src/main.rs", "fn main() { println!(); }");
        let decision = plan(&target, &env, &store).unwrap();
        assert_ne!(
            decision.key().unwrap(),
            key,
            "the key must follow the inputs"
        );
        match &decision {
            Decision::Rebuild {
                reason: Reason::InputsChanged { changed, total },
                ..
            } => {
                assert_eq!(total, &1);
                assert_eq!(changed, &vec!["src/main.rs".to_string()]);
            }
            other => panic!("expected an inputs-changed rebuild, got {other:?}"),
        }

        let _ = std::fs::remove_dir_all(&root);
    }

    /// A declared output that was hand-edited must not be served as current —
    /// that's the case where a cache quietly hands back the wrong thing.
    #[test]
    fn a_modified_output_is_restored_rather_than_trusted() {
        let root = scratch("modified");
        let work = root.join("api");
        std::fs::create_dir_all(&work).unwrap();
        write(&work, "src/main.rs", "fn main() {}");
        let store = store::Store::at(root.join("cache")).unwrap();

        let target = Target {
            name: "build".into(),
            workspace: "api".into(),
            dir: work.clone(),
            member: None,
            commands: vec!["make".into()],
            config: CacheConfig {
                enabled: Some(true),
                inputs: vec!["src/**/*.rs".into()],
                outputs: vec!["dist/**/*".into()],
                ..Default::default()
            },
            upstream: BTreeMap::new(),
        };
        let env = BTreeMap::new();
        let key = plan(&target, &env, &store)
            .unwrap()
            .key()
            .unwrap()
            .to_string();

        write(&work, "dist/app", "compiled");
        let inputs = hash_matching(&work, &target.config.inputs, &[]).unwrap();
        let outputs = hash_matching(&work, &target.config.outputs, &[]).unwrap();
        store
            .put(
                &key,
                &work,
                store::Build {
                    target: "build".into(),
                    workspace: "api".into(),
                    inputs,
                    outputs,
                    duration_ms: 1,
                    ..Default::default()
                },
            )
            .unwrap();

        // Somebody edits the generated file by hand.
        write(&work, "dist/app", "hand-edited, definitely not compiled");
        let decision = plan(&target, &env, &store).unwrap();
        assert!(
            !matches!(decision, Decision::Fresh { .. }),
            "an edited output must never read as up to date"
        );
        assert_eq!(
            decision,
            Decision::Hit {
                key: key.clone(),
                source: Source::Local,
                outputs: 1
            }
        );
        store.restore(&key, &work).unwrap();
        assert_eq!(
            std::fs::read_to_string(work.join("dist/app")).unwrap(),
            "compiled",
            "the restore must overwrite the tampered file"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    /// A build with no declared outputs has nothing a hit could give back, so
    /// it always runs rather than pretending to be cached.
    #[test]
    fn declaring_no_outputs_means_no_hits() {
        let root = scratch("nooutputs");
        let work = root.join("api");
        std::fs::create_dir_all(&work).unwrap();
        write(&work, "src/main.rs", "fn main() {}");
        let store = store::Store::at(root.join("cache")).unwrap();

        let target = Target {
            name: "build".into(),
            workspace: "api".into(),
            dir: work.clone(),
            member: None,
            commands: vec!["make".into()],
            config: CacheConfig {
                enabled: Some(true),
                inputs: vec!["src/**/*.rs".into()],
                outputs: vec![],
                ..Default::default()
            },
            upstream: BTreeMap::new(),
        };
        match plan(&target, &BTreeMap::new(), &store).unwrap() {
            Decision::Rebuild {
                reason: Reason::NoOutputs,
                ..
            } => {}
            other => panic!("expected a no-outputs rebuild, got {other:?}"),
        }

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn changed_inputs_names_additions_edits_and_removals() {
        let before = vec![
            FileHash {
                path: "a".into(),
                sha256: "1".into(),
                size: 1,
            },
            FileHash {
                path: "b".into(),
                sha256: "2".into(),
                size: 1,
            },
        ];
        let after = vec![
            FileHash {
                path: "a".into(),
                sha256: "9".into(),
                size: 1,
            },
            FileHash {
                path: "c".into(),
                sha256: "3".into(),
                size: 1,
            },
        ];
        assert_eq!(
            changed_inputs(&before, &after),
            vec![
                "a".to_string(),
                "b (gone)".to_string(),
                "c (new)".to_string()
            ]
        );
        assert_eq!(changed_inputs(&before, &before), Vec::<String>::new());
    }
}
