//! `--authoritative`: run each step against only what it declared it reads.
//!
//! Ciabatta's cache is built on a promise the config file makes and nothing
//! checks: that `cache.inputs` lists every file a build reads. The comment in
//! every scaffolded config says so — "a build that reads a file not listed here
//! will be handed a stale result" — and it is true, but the day you find out is
//! the day a colleague's build produces something yours doesn't, from the same
//! commit, and neither of you can reproduce it.
//!
//! This is the check. Each step is run in a directory containing its declared
//! inputs and nothing else, laid out exactly as the project root is, so paths
//! that reach sideways (`../schemas/*.json`) or write upward
//! (`../dist/thing.vsix`) land where they would have. A step that reads
//! something it never declared doesn't find it, and fails — loudly, now, with
//! the sandbox left on disk to look at, rather than silently six weeks later.
//!
//! **Opt-in, and it stays that way.** This is not how ciabatta builds by
//! default and it is not trying to become Bazel: there is no hermetic
//! toolchain, no attempt to isolate `$HOME`, the network, or the clock. What
//! comes into the sandbox is the declared inputs; the compiler, the package
//! manager and its global store are whatever the machine has. That makes it a
//! sharp tool for one question — *are my inputs complete?* — and the honest way
//! to offer it is a flag nobody is made to use.
//!
//! Two things are deliberately not done. Inputs are **copied**, not
//! hardlinked: a link is faster, and a build that rewrites one of its own
//! inputs in place would silently corrupt the real tree through it. And a step
//! that declares no inputs is **not** sandboxed — an empty directory would fail
//! it for reasons that have nothing to do with its declarations — so it runs
//! normally and is counted as unverified, which the summary says out loud.
//!
//! ## The escape hatch, and why it is on the command line
//!
//! Some steps genuinely need state that is not, and should not be, an input.
//! `yarn run check` has to sit inside its yarn project; a cargo build wants the
//! shared `target/`. Listing `node_modules` under `cache.inputs` would be a lie
//! twice over — it would put a hundred thousand derived files into the cache
//! key, and it would claim they are the build's sources.
//!
//! So `--sandbox-also <GLOB>` stages extra paths, and it is a flag rather than
//! a config field on purpose. Everything it names is outside the guarantee, and
//! a weakening you retype at the call site stays visible in a way that one line
//! added to a config file two years ago does not. These paths are **symlinked**
//! rather than copied — they are ambient toolchain state, often enormous, and
//! not the thing under test — which is the other reason to keep them separate
//! from inputs, which are copied precisely because they *are*.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::cache::CacheConfig;
use crate::cache::graph::StepContext;
use crate::run::RunStep;

/// Where sandboxes are made, under the project root.
///
/// Inside the project rather than the system temp directory for two reasons:
/// it is the same filesystem, so copying a source tree is a cheap
/// same-device operation rather than a trip through `/tmp`, which is often a
/// RAM-backed tmpfs that a large build would fill; and it is already
/// gitignored, so a kept sandbox never shows up as uncommitted work.
const SANDBOX_DIR: &str = ".ciabatta/.cache/authoritative";

/// What one step declared, and where it really lives.
struct Declared {
    /// The real directory its `inputs`/`outputs` are relative to: the
    /// workspace root, for every step.
    dir: PathBuf,
    /// Its own sub-workspace, relative to `dir` — see
    /// [`crate::cache::Target::member`].
    member: Option<String>,
    config: CacheConfig,
}

/// The isolation plan for a run: what every step declared, resolved once.
pub struct Isolation {
    root: PathBuf,
    sandboxes: PathBuf,
    steps: HashMap<String, Declared>,
    /// Globs from `--sandbox-also`, relative to the project root: ambient state
    /// symlinked into every sandbox and covered by no guarantee.
    also: Vec<String>,
    /// Sandboxes left behind because their step failed, for the operator to
    /// inspect. Reported at the end of the run.
    pub kept: Vec<PathBuf>,
    /// Steps that ran outside a sandbox because they declared no inputs.
    pub unverified: Vec<String>,
}

impl Isolation {
    /// Resolve what each step declares, using the same context the cache uses
    /// — so "its inputs" means exactly what it means everywhere else.
    pub fn plan(
        root: &Path,
        steps: &[RunStep],
        context: &dyn StepContext,
        also: &[String],
    ) -> Self {
        let declared = steps
            .iter()
            .filter(|step| !step.recover)
            .map(|step| {
                (
                    step.name.clone(),
                    Declared {
                        dir: context.dir(step),
                        member: context.member(step),
                        config: context.cache_config(step),
                    },
                )
            })
            .collect();

        Isolation {
            root: root.to_path_buf(),
            sandboxes: root.join(SANDBOX_DIR),
            steps: declared,
            also: also.to_vec(),
            kept: Vec::new(),
            unverified: Vec::new(),
        }
    }

    /// Build a sandbox for one step, or `None` when it declares no inputs and
    /// so has nothing to be held to.
    pub fn prepare(&mut self, step: &RunStep) -> Result<Option<Sandbox>> {
        let Some(declared) = self.steps.get(&step.name) else {
            return Ok(None);
        };
        if declared.config.inputs.is_empty() {
            self.unverified.push(step.name.clone());
            return Ok(None);
        }

        // Never treat the sandbox area as somebody's source. A step whose
        // `inputs` reach broadly (`.`, `**/*`) would otherwise copy the last
        // sandbox into the next one, and each run would cost more than the one
        // before it.
        let mut config = declared.config.clone();
        config.exclude.push(SANDBOX_DIR.to_string());

        let root = self.sandboxes.join(sanitize(&step.name));
        // A leftover from an earlier run is not this run's evidence.
        if root.exists() {
            std::fs::remove_dir_all(&root)
                .with_context(|| format!("Failed to clear {}", root.display()))?;
        }

        // The sandbox mirrors the project root, which is what every declared
        // path is relative to — so a step reaching sideways for a shared file
        // or upward to publish finds it at the same relative place it sits in
        // the real tree.
        let rel = declared
            .dir
            .strip_prefix(&self.root)
            .unwrap_or(Path::new(""));
        let dir = root.join(rel);
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("Failed to create {}", dir.display()))?;

        let inputs = config.list_inputs(&declared.dir, declared.member.as_deref())?;
        let mut copied = 0usize;
        for input in &inputs {
            let from = declared.dir.join(&input.path);
            let to = dir.join(&input.path);
            // Paths are root-relative and the matcher refuses any that escape,
            // so this holds by construction — checked anyway, because what it
            // is guarding is a write outside the sandbox.
            if !within(&root, &to) {
                anyhow::bail!(
                    "input '{}' of step '{}' resolves outside the project root",
                    input.path,
                    step.name,
                );
            }
            if let Some(parent) = to.parent() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("Failed to create {}", parent.display()))?;
            }
            std::fs::copy(&from, &to)
                .with_context(|| format!("Failed to stage {}", from.display()))?;
            copied += 1;
        }

        let linked = self.link_ambient(&root)?;

        Ok(Some(Sandbox {
            root,
            dir,
            real: declared.dir.clone(),
            config,
            staged: copied,
            linked,
        }))
    }

    /// Symlink the `--sandbox-also` paths into a sandbox, mirroring where they
    /// sit relative to the project root.
    ///
    /// Failures are reported rather than fatal: a path that doesn't exist is a
    /// typo in a flag, not a reason to abandon the run, and the step is about
    /// to fail with a far more specific message anyway.
    fn link_ambient(&self, sandbox: &Path) -> Result<usize> {
        let mut linked = 0usize;
        for pattern in &self.also {
            let joined = self.root.join(pattern);
            let Some(as_str) = joined.to_str() else {
                continue;
            };
            for real in glob::glob(as_str)
                .with_context(|| format!("Invalid --sandbox-also pattern '{pattern}'"))?
                .flatten()
            {
                let Ok(rel) = real.strip_prefix(&self.root) else {
                    continue;
                };
                let link = sandbox.join(rel);
                if link.exists() {
                    continue;
                }
                if let Some(parent) = link.parent() {
                    std::fs::create_dir_all(parent)
                        .with_context(|| format!("Failed to create {}", parent.display()))?;
                }
                if symlink(&real, &link).is_ok() {
                    linked += 1;
                }
            }
        }
        Ok(linked)
    }

    /// Take the outputs a finished step produced back into the real tree, and
    /// remove the sandbox.
    ///
    /// Copying back is what makes the flag usable rather than merely
    /// diagnostic: a run with `--authoritative` leaves the same artifacts in
    /// the same places as a run without it, having proved something extra
    /// along the way.
    pub fn collect(&mut self, sandbox: Sandbox) -> Result<usize> {
        let outputs = sandbox.config.list_outputs(&sandbox.dir)?;
        for output in &outputs {
            let from = sandbox.dir.join(&output.path);
            let to = sandbox.real.join(&output.path);
            if !within(&self.root, &to) {
                anyhow::bail!(
                    "output '{}' would be written outside the project root",
                    output.path,
                );
            }
            if let Some(parent) = to.parent() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("Failed to create {}", parent.display()))?;
            }
            std::fs::copy(&from, &to)
                .with_context(|| format!("Failed to collect {}", from.display()))?;
        }
        let _ = std::fs::remove_dir_all(&sandbox.root);
        Ok(outputs.len())
    }

    /// Keep a failed step's sandbox, and say where it is.
    ///
    /// The directory *is* the diagnosis: what a step could see when it failed
    /// is exactly the question, and a sandbox deleted on the way out takes the
    /// answer with it.
    pub fn keep(&mut self, sandbox: Sandbox) -> PathBuf {
        self.kept.push(sandbox.root.clone());
        sandbox.root
    }

    /// The line a run prints at the end, or `None` when there is nothing to
    /// report.
    pub fn summary(&self) -> Option<String> {
        if self.kept.is_empty() && self.unverified.is_empty() {
            return None;
        }
        let mut parts = Vec::new();
        if !self.unverified.is_empty() {
            parts.push(format!(
                "{} step(s) ran unverified (no cache.inputs declared): {}",
                self.unverified.len(),
                self.unverified.join(", "),
            ));
        }
        if !self.kept.is_empty() {
            parts.push(format!(
                "{} sandbox(es) kept for inspection",
                self.kept.len()
            ));
        }
        Some(format!("authoritative: {}", parts.join("; ")))
    }
}

/// One step's isolated directory.
pub struct Sandbox {
    /// The sandbox's own root — the mirror of the project root.
    pub root: PathBuf,
    /// Where the step runs: the sandbox's copy of its working directory.
    pub dir: PathBuf,
    /// The real directory outputs are copied back into.
    real: PathBuf,
    config: CacheConfig,
    /// How many declared input files were staged.
    pub staged: usize,
    /// How many `--sandbox-also` paths were linked in. Reported separately
    /// because they are exactly the part this run does *not* vouch for.
    pub linked: usize,
}

/// Link `real` into the sandbox at `link`.
///
/// Symlinks on Unix. Windows needs a privilege for those that a developer
/// shell usually lacks, so it falls back to nothing rather than to a copy: a
/// silent multi-gigabyte copy of `node_modules` would look like a hang, and the
/// step's own error is a better thing to read than a wait.
#[cfg(unix)]
fn symlink(real: &Path, link: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(real, link)
}

#[cfg(not(unix))]
fn symlink(real: &Path, link: &Path) -> std::io::Result<()> {
    match real.is_dir() {
        true => std::os::windows::fs::symlink_dir(real, link),
        false => std::os::windows::fs::symlink_file(real, link),
    }
}

/// Whether `path` stays inside `root` once `..` components are folded out.
///
/// Purely lexical, and deliberately so: the paths being checked are built from
/// config globs against directories that may not exist yet, and a check that
/// needed the filesystem would have nothing to canonicalize.
fn within(root: &Path, path: &Path) -> bool {
    use std::path::Component;
    let mut depth = 0i32;
    let Ok(rel) = path.strip_prefix(root) else {
        return false;
    };
    for component in rel.components() {
        match component {
            Component::ParentDir => depth -= 1,
            Component::CurDir => {}
            _ => depth += 1,
        }
        if depth < 0 {
            return false;
        }
    }
    true
}

/// A step name as a directory name. Step names carry `:` from the workflow
/// compiler, which is a path separator on Windows and awkward everywhere.
fn sanitize(name: &str) -> String {
    name.chars()
        .map(
            |c| match c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                true => c,
                false => '_',
            },
        )
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::graph::StepContext;

    /// A context that gives every step the same directory and settings.
    struct Fixed {
        dir: PathBuf,
        config: CacheConfig,
    }

    impl StepContext for Fixed {
        fn cache_config(&self, _step: &RunStep) -> CacheConfig {
            self.config.clone()
        }
        fn dir(&self, _step: &RunStep) -> PathBuf {
            self.dir.clone()
        }

        fn member(&self, _step: &RunStep) -> Option<String> {
            None
        }
        fn workspace(&self, _step: &RunStep) -> String {
            ".".to_string()
        }
    }

    fn step(name: &str) -> RunStep {
        RunStep {
            name: name.to_string(),
            run: Some("true".to_string()),
            ..Default::default()
        }
    }

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "ciab_iso_{tag}_{}_{:?}",
            std::process::id(),
            std::thread::current().id(),
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn only_the_declared_inputs_are_staged() {
        let root = scratch("staged");
        std::fs::write(root.join("declared.txt"), "in").unwrap();
        std::fs::write(root.join("undeclared.txt"), "out").unwrap();

        let context = Fixed {
            dir: root.clone(),
            config: CacheConfig {
                inputs: vec!["declared.txt".to_string()],
                ..Default::default()
            },
        };
        let steps = vec![step("build")];
        let mut isolation = Isolation::plan(&root, &steps, &context, &[]);
        let sandbox = isolation.prepare(&steps[0]).unwrap().expect("sandboxed");

        assert_eq!(sandbox.staged, 1);
        assert!(sandbox.dir.join("declared.txt").is_file());
        // The whole point: what wasn't declared isn't there, so a step that
        // reads it fails instead of quietly succeeding.
        assert!(!sandbox.dir.join("undeclared.txt").exists());
    }

    /// A step reading and writing outside its own package has to keep working —
    /// shared schemas are read that way and packaged output is written that
    /// way, and a sandbox that broke both would be unusable on a real repo.
    ///
    /// Paths are relative to the workspace root, so "outside its own package"
    /// needs no `../`: the sibling is simply named from the root. That is what
    /// makes the sandbox's containment check trivially true rather than a
    /// judgement call about how far up a `../` chain is allowed to climb.
    #[test]
    fn a_step_reaching_into_a_sibling_package_still_resolves() {
        let root = scratch("sideways");
        let member = root.join("editors/vscode");
        std::fs::create_dir_all(root.join("editors/schemas")).unwrap();
        std::fs::create_dir_all(&member).unwrap();
        std::fs::write(root.join("editors/schemas/a.json"), "{}").unwrap();
        std::fs::write(member.join("main.ts"), "source").unwrap();

        let context = Fixed {
            dir: root.clone(),
            config: CacheConfig {
                inputs: vec![
                    "editors/vscode/main.ts".to_string(),
                    "editors/schemas/*.json".to_string(),
                ],
                outputs: vec!["editors/dist/out.vsix".to_string()],
                ..Default::default()
            },
        };
        let steps = vec![step("package")];
        let mut isolation = Isolation::plan(&root, &steps, &context, &[]);
        let sandbox = isolation.prepare(&steps[0]).unwrap().expect("sandboxed");

        assert_eq!(sandbox.staged, 2);
        assert!(sandbox.dir.join("editors/vscode/main.ts").is_file());
        assert!(sandbox.dir.join("editors/schemas/a.json").is_file());

        // The step "produces" its output, which must come back to the real tree.
        std::fs::create_dir_all(sandbox.dir.join("editors/dist")).unwrap();
        std::fs::write(sandbox.dir.join("editors/dist/out.vsix"), "packaged").unwrap();
        let collected = isolation.collect(sandbox).unwrap();

        assert_eq!(collected, 1);
        assert_eq!(
            std::fs::read_to_string(root.join("editors/dist/out.vsix")).unwrap(),
            "packaged",
        );
    }

    #[test]
    fn a_step_that_declares_no_inputs_is_reported_rather_than_failed() {
        let root = scratch("undeclared");
        let context = Fixed {
            dir: root.clone(),
            config: CacheConfig::default(),
        };
        let steps = vec![step("mystery")];
        let mut isolation = Isolation::plan(&root, &steps, &context, &[]);

        // No sandbox — an empty one would fail it for the wrong reason.
        assert!(isolation.prepare(&steps[0]).unwrap().is_none());
        assert_eq!(isolation.unverified, vec!["mystery".to_string()]);
        assert!(isolation.summary().unwrap().contains("ran unverified"));
    }

    #[test]
    fn a_failed_step_keeps_its_sandbox_to_look_at() {
        let root = scratch("kept");
        std::fs::write(root.join("in.txt"), "x").unwrap();
        let context = Fixed {
            dir: root.clone(),
            config: CacheConfig {
                inputs: vec!["in.txt".to_string()],
                ..Default::default()
            },
        };
        let steps = vec![step("failing")];
        let mut isolation = Isolation::plan(&root, &steps, &context, &[]);
        let sandbox = isolation.prepare(&steps[0]).unwrap().unwrap();

        let kept = isolation.keep(sandbox);
        assert!(
            kept.is_dir(),
            "the sandbox is the diagnosis; it must survive"
        );
        assert!(isolation.summary().unwrap().contains("kept for inspection"));
    }

    #[test]
    fn containment_folds_out_parent_components() {
        let root = Path::new("/project");
        assert!(within(root, Path::new("/project/a/b")));
        assert!(within(root, Path::new("/project/a/../b")));
        assert!(!within(root, Path::new("/project/../etc/passwd")));
        assert!(!within(root, Path::new("/elsewhere/a")));
    }

    #[test]
    fn a_step_name_becomes_a_usable_directory_name() {
        assert_eq!(
            sanitize("vscode-extension:package"),
            "vscode-extension_package"
        );
    }
}
