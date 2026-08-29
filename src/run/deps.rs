//! What a target depends on, in one place.
//!
//! A ciabatta target — one node of a run graph — is defined by five things, and
//! until now each of them lived somewhere different: the files it reads and
//! writes in its `cache:` section, the variables it needs half in that section
//! and half in the commands themselves, the commands in `run`/`script`/`workflow`,
//! and the targets it needs in `needs`. Anyone asking "why did this rebuild?" or
//! "what does this actually touch?" had to assemble that answer by hand.
//!
//! This module assembles it once, from the same resolution the cache itself
//! uses, and hands the result to the three places that should have had it all
//! along: the summary `ciabatta run` prints before it starts, the web viewer's
//! step panel, and `ciabatta dry-run`'s explanation of a miss.
//!
//! Listing is deliberately cheap — no file is read, only found (see
//! [`crate::cache::list_matching`]) — because a summary printed before every
//! run must not cost what the run costs.

use std::collections::HashMap;
use std::path::Path;

use serde::Serialize;

use crate::cache::cli::WorkspaceContext;
use crate::cache::graph::StepContext;
use crate::config::CiabattaConfig;
use crate::run::RunStep;
use crate::workspace::Workspace;

/// Everything one target depends on, and everything it produces.
#[derive(Debug, Clone, Serialize, Default)]
pub struct TargetDeps {
    /// The target's name in the graph.
    pub name: String,
    /// The sub-workspace it came from, when the run is a monorepo graph.
    pub workspace: Option<String>,
    /// Where its `inputs` and `outputs` are resolved from, relative to the
    /// project root (`.` for the root itself).
    pub dir: String,

    /// The commands it runs to produce its outputs, as they go into its cache
    /// key — an inline `run`, a `script:<path>`, or a `workflow:<name>`.
    pub commands: Vec<String>,

    /// The input globs it declares (its own, or the ones it inherited).
    pub inputs: Vec<String>,
    /// The output globs it declares.
    pub outputs: Vec<String>,
    /// Globs excluded from its inputs, including the sub-workspaces excluded
    /// automatically.
    pub exclude: Vec<String>,

    /// The input files those globs currently match.
    pub input_files: usize,
    /// Their total size in bytes.
    pub input_bytes: u64,
    /// The output files currently on disk.
    pub output_files: usize,
    pub output_bytes: u64,

    /// The input files themselves, when the caller asked for [`Detail::Files`].
    ///
    /// Empty by default, and skipped when serialized, because the callers that
    /// want a *count* are the frequent ones — a run summary and a live web view,
    /// once per step per run — and a monorepo's input set is thousands of paths
    /// that would be carried through both for nothing. `ciabatta why --all` is
    /// the caller that wants them, once, because somebody asked.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub input_list: Vec<crate::cache::FileHash>,
    /// The output files themselves, on the same terms.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub output_list: Vec<crate::cache::FileHash>,

    /// The environment variables it declares as dependencies — the ones folded
    /// into its cache key.
    pub env: Vec<String>,
    /// Variables it *reads* without declaring: `$VAR` in its command, its
    /// working directory, or its conditions. Not part of the key, which is
    /// exactly why they're worth showing — an undeclared variable that changes
    /// the build is how a cache serves the wrong answer.
    pub env_refs: Vec<String>,

    /// The other targets it depends on.
    pub needs: Vec<String>,

    /// Whether the cache is in play for it.
    pub cached: bool,
    /// Why it isn't, when it isn't.
    pub why_uncached: Option<String>,
}

impl TargetDeps {
    /// The variables it reads but never declared, so a change to one of them
    /// would not invalidate its cache entry.
    pub fn undeclared_env(&self) -> Vec<&str> {
        self.env_refs
            .iter()
            .filter(|key| !self.env.contains(*key))
            .map(String::as_str)
            .collect()
    }
}

/// How much of the file sets to carry back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Detail {
    /// How many files there are and how big they are, and nothing more.
    Counts,
    /// Every matched path as well, in `input_list` / `output_list`.
    Files,
}

/// Assemble the dependency view for every step in a resolved graph.
///
/// `steps` are the run's steps in graph order. Recovery nodes are left out:
/// they aren't part of the success graph and produce nothing.
pub fn collect(config: &CiabattaConfig, root: &Path, steps: &[RunStep]) -> Vec<TargetDeps> {
    collect_with(config, root, steps, Detail::Counts)
}

/// [`collect`], keeping the matched paths as well as counting them.
pub fn collect_with(
    config: &CiabattaConfig,
    root: &Path,
    steps: &[RunStep],
    detail: Detail,
) -> Vec<TargetDeps> {
    // A run started inside one package still needs its siblings' settings, since
    // a compiled workflow graph spans them. No workspace is an ordinary state:
    // then every step resolves against the root project's own config.
    let workspace = Workspace::discover(root).ok();
    let context = WorkspaceContext {
        workspace: workspace.as_ref(),
        root: root.to_path_buf(),
        config,
    };

    steps
        .iter()
        .filter(|step| !step.recover)
        .map(|step| one(&context, root, step, detail))
        .collect()
}

fn one(context: &WorkspaceContext<'_>, root: &Path, step: &RunStep, detail: Detail) -> TargetDeps {
    let cache = context.cache_config(step);
    let dir = context.dir(step);

    // Best-effort: a directory that can't be walked costs a count in a summary,
    // not the run itself.
    let inputs = cache.list_inputs(&dir).unwrap_or_default();
    let outputs = cache.list_outputs(&dir).unwrap_or_default();

    let mut exclude = cache.exclude.clone();
    exclude.extend(crate::cache::nested_workspaces(&dir));
    exclude.sort();
    exclude.dedup();

    TargetDeps {
        name: step.name.clone(),
        workspace: step.workspace.clone(),
        dir: relative(root, &dir),
        commands: crate::cache::graph::commands_of(step),
        inputs: cache.inputs.clone(),
        outputs: cache.outputs.clone(),
        exclude,
        input_files: inputs.len(),
        input_bytes: inputs.iter().map(|f| f.size).sum::<u64>(),
        output_files: outputs.len(),
        output_bytes: outputs.iter().map(|f| f.size).sum(),
        input_list: match detail {
            Detail::Files => inputs,
            Detail::Counts => Vec::new(),
        },
        output_list: match detail {
            Detail::Files => outputs,
            Detail::Counts => Vec::new(),
        },
        env: cache.env.clone(),
        env_refs: super::envdeps::step_refs(step),
        needs: step.needs.clone(),
        cached: cache.why_disabled().is_none(),
        why_uncached: cache.why_disabled().map(str::to_string),
    }
}

/// `dir` as the user would type it: relative to the project root, `.` for the
/// root itself, and absolute only when it lies outside the project entirely.
fn relative(root: &Path, dir: &Path) -> String {
    match dir.strip_prefix(root) {
        Ok(rel) if rel.as_os_str().is_empty() => ".".to_string(),
        Ok(rel) => rel.to_string_lossy().replace('\\', "/"),
        Err(_) => dir.to_string_lossy().to_string(),
    }
}

// ─── Terminal summary ───────────────────────────────────────────────────────

/// The block `ciabatta run` prints before the first step: what is about to be
/// built, and what each of it depends on.
///
/// The point is that a run states its terms up front. A monorepo graph reaches
/// into packages nobody typed, reads files nobody listed, and keys on variables
/// nobody mentioned — and every one of those is a thing that can be wrong in a
/// way the build itself will never tell you about. Printing the summation costs
/// a directory walk and answers most of "why did that rebuild?" before it is
/// asked.
///
/// Returns `None` when there is nothing to say.
pub fn render(targets: &[TargetDeps], workflow: &str) -> Option<String> {
    if targets.is_empty() {
        return None;
    }

    let mut out = String::new();
    out.push_str(&format!(
        "Targets for '{workflow}' — {} to build\n",
        targets.len()
    ));

    let name_width = targets.iter().map(|t| label(t).len()).max().unwrap_or(0);

    for target in targets {
        out.push_str(&format!("  {:width$}", label(target), width = name_width));

        let mut notes: Vec<String> = Vec::new();
        if target.cached {
            notes.push(format!(
                "inputs {}",
                files_and_size(target.input_files, target.input_bytes)
            ));
            notes.push(format!(
                "outputs {}",
                files_and_size(target.output_files, target.output_bytes)
            ));
        } else if let Some(why) = &target.why_uncached {
            notes.push(format!("uncached — {why}"));
        }
        if !target.env.is_empty() {
            notes.push(format!("env {}", target.env.join(", ")));
        }
        if !target.needs.is_empty() {
            notes.push(format!("needs {}", target.needs.join(", ")));
        }

        out.push_str(&format!("  {}\n", notes.join(" · ")));

        // Only worth a line when it says something the target's own name
        // doesn't: an undeclared variable is a cache that can be wrong.
        let undeclared = target.undeclared_env();
        if target.cached && !undeclared.is_empty() {
            out.push_str(&format!(
                "  {:width$}  ⚠ reads {} without declaring {} in `cache.env`\n",
                "",
                undeclared.join(", "),
                if undeclared.len() == 1 { "it" } else { "them" },
                width = name_width
            ));
        }
    }

    Some(out)
}

/// A target's name, prefixed with its sub-workspace when it has one — the same
/// `<member>:<step>` spelling the graph drawing uses.
fn label(target: &TargetDeps) -> String {
    match (&target.workspace, target.name.contains(':')) {
        (Some(workspace), false) => format!("{workspace}:{}", target.name),
        _ => target.name.clone(),
    }
}

fn files_and_size(count: usize, bytes: u64) -> String {
    if count == 0 {
        return "none".to_string();
    }
    format!(
        "{count} file(s), {}",
        crate::cache::store::human_size(bytes)
    )
}

/// Collect and render in one go, for the caller that only wants the text.
///
/// `_vars` is the run's environment; it isn't needed to list dependencies, but
/// taking it keeps the call site symmetrical with the environment report next
/// to it and leaves room for value-aware reporting later.
pub fn report(
    config: &CiabattaConfig,
    root: &Path,
    workflow: &str,
    steps: &[RunStep],
    _vars: &HashMap<String, String>,
) -> Option<String> {
    render(&collect(config, root, steps), workflow)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::CacheConfig;

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("ciab_deps_{name}_{}", std::process::id()));
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
    fn a_target_reports_all_five_of_its_dependencies() {
        let dir = scratch("five");
        write(&dir, "src/main.rs", "fn main() {}");
        write(&dir, "src/nested/deep/mod.rs", "// deep");
        write(&dir, "dist/app", "built");

        let config = CiabattaConfig {
            cache: Some(CacheConfig {
                enabled: Some(true),
                inputs: vec!["src".into()],
                outputs: vec!["dist/**/*".into()],
                env: vec!["PROFILE".into()],
                ..Default::default()
            }),
            ..Default::default()
        };

        let step = RunStep {
            name: "build".into(),
            run: Some("cargo build --release --target $TARGET".into()),
            needs: vec!["generate".into()],
            ..Default::default()
        };

        let deps = collect(&config, &dir, &[step]);
        let target = &deps[0];

        assert_eq!(
            target.commands,
            vec!["cargo build --release --target $TARGET"]
        );
        assert_eq!(
            target.input_files, 2,
            "a bare directory input must reach files nested below it"
        );
        assert_eq!(target.output_files, 1);
        assert_eq!(target.env, vec!["PROFILE".to_string()]);
        assert_eq!(target.needs, vec!["generate".to_string()]);
        assert!(target.cached);

        // The variable the command reads but nobody declared: not in the key,
        // and so the one worth saying out loud.
        assert_eq!(target.undeclared_env(), vec!["TARGET"]);

        let text = render(&deps, "build").expect("a target to report");
        assert!(text.contains("build"));
        assert!(text.contains("needs generate"));
        assert!(text.contains("TARGET"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `why --all` exists to name the file that shouldn't be there, so the list
    /// has to be the same set the counts came from — and has to be absent when
    /// nobody asked, because a run summary carries one of these per step.
    #[test]
    fn the_file_lists_are_collected_only_when_asked_for() {
        let dir = scratch("detail");
        write(&dir, "src/main.rs", "fn main() {}");
        write(&dir, "src/nested/deep/mod.rs", "// deep");
        write(&dir, "dist/app", "built");

        let config = CiabattaConfig {
            cache: Some(CacheConfig {
                enabled: Some(true),
                inputs: vec!["src".into()],
                outputs: vec!["dist/**/*".into()],
                ..Default::default()
            }),
            ..Default::default()
        };
        let step = RunStep {
            name: "build".into(),
            run: Some("make".into()),
            ..Default::default()
        };

        let counted = &collect(&config, &dir, std::slice::from_ref(&step))[0];
        assert_eq!(counted.input_files, 2);
        assert!(
            counted.input_list.is_empty() && counted.output_list.is_empty(),
            "the default must not carry a monorepo's worth of paths"
        );

        let listed = &collect_with(&config, &dir, &[step], Detail::Files)[0];
        assert_eq!(listed.input_files, listed.input_list.len());
        assert_eq!(listed.output_files, listed.output_list.len());
        assert_eq!(
            listed
                .input_list
                .iter()
                .map(|f| f.path.as_str())
                .collect::<Vec<_>>(),
            vec!["src/main.rs", "src/nested/deep/mod.rs"],
            "in the order they are hashed into the key, nested files included"
        );
        assert_eq!(
            listed.input_bytes,
            listed.input_list.iter().map(|f| f.size).sum::<u64>()
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_uncached_target_says_so_instead_of_counting_files() {
        let dir = scratch("uncached");
        write(&dir, "src/main.rs", "fn main() {}");

        let step = RunStep {
            name: "build".into(),
            run: Some("make".into()),
            ..Default::default()
        };
        let deps = collect(&CiabattaConfig::default(), &dir, &[step]);
        assert!(!deps[0].cached);
        assert!(deps[0].why_uncached.as_deref().unwrap().contains("off"));

        let text = render(&deps, "build").unwrap();
        assert!(text.contains("uncached"));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
