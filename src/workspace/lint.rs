//! `ciabatta self lint`: every workflow in the workspace, checked at once.
//!
//! A run only compiles the graph it was asked for, so a workflow nobody has
//! run since a sub-workspace was renamed — a `needs: proto:generate` pointing
//! at a workflow that's now called `codegen` — stays broken until the day
//! somebody needs it. This compiles every (sub-workspace, workflow) pair the
//! way a run would, and reports each one that wouldn't start, all together,
//! with the file to go and fix.
//!
//! Each unit is checked in three passes, so a problem is reported against the
//! unit that has it rather than every unit that happens to depend on it:
//!
//! 1. **On its own** (`isolated`): its steps, their `needs` and `on_error`
//!    targets, its background targets.
//! 2. **Its references**: every `depends_on` / `needs` entry names a
//!    sub-workspace and workflow that exist.
//! 3. **With everything it pulls in**: what only shows up across units — a
//!    dependency cycle, a `from:` naming a push step elsewhere in the graph, a
//!    registry nobody defines. Skipped when a unit it pulls in already failed,
//!    since the answer would be that unit's problem again.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use anyhow::Result;
use serde::Serialize;

use super::graph::{self, Selection, UnitId};
use super::{Member, WORKFLOWS_DIR, Workspace};
use crate::config::{CIABATTA_DIR, config_path, load_config_file};

/// One thing that's broken.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Problem {
    /// The workflow it's in, as `member:workflow` — or `None` for a problem in
    /// a sub-workspace's own config: a file that didn't load, or a
    /// `depends_on` entry, which every one of its workflows shares.
    pub unit: Option<String>,
    /// The file to fix, relative to the workspace root.
    pub file: String,
    /// What's wrong, as the run would have said it.
    pub message: String,
}

/// What a lint found.
#[derive(Debug, Clone, Serialize, Default)]
pub struct Report {
    /// Sub-workspaces whose config loaded.
    pub members: usize,
    /// (sub-workspace, workflow) pairs compiled.
    pub workflows: usize,
    pub problems: Vec<Problem>,
}

impl Report {
    pub fn is_clean(&self) -> bool {
        self.problems.is_empty()
    }
}

/// Lint the workspace rooted at `root`.
pub fn lint(root: &Path) -> Result<Report> {
    let (workspace, broken) = Workspace::load_lenient(root)?;
    let mut report = Report {
        members: workspace.members.len(),
        ..Default::default()
    };

    for (dir, error) in broken {
        report.problems.push(Problem {
            unit: None,
            file: config_file_of(root, &dir),
            message: format!("{error:#}"),
        });
    }

    let units: Vec<(&Member, UnitId)> = workspace
        .members
        .iter()
        .flat_map(|member| {
            member.workflows.keys().map(move |workflow| {
                (
                    member,
                    UnitId {
                        member: member.name.clone(),
                        workflow: workflow.clone(),
                    },
                )
            })
        })
        .collect();
    report.workflows = units.len();

    let selection = |unit: &UnitId, isolated: bool| Selection {
        only: vec![unit.member.clone()],
        isolated,
    };

    // Pass 1: each unit on its own. Pass 2: what it names — a sub-workspace's
    // `depends_on` is checked against its config file, once, since it's
    // written there and applies to every workflow the sub-workspace defines;
    // a workflow's own `needs` against the workflow's file.
    let mut failed: BTreeSet<UnitId> = BTreeSet::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    for (member, unit) in &units {
        let mut problems: Vec<(String, anyhow::Error)> = Vec::new();
        let own_file = workflow_file_of(root, member, &unit.workflow);
        if let Err(error) = graph::build(&workspace, &unit.workflow, &selection(unit, true)) {
            problems.push((own_file.clone(), error));
        }
        for spec in &member.meta.depends_on {
            if let Err(error) = graph::parse_dependency(&workspace, unit, spec, false) {
                problems.push((config_file_of(root, &member.dir), error));
            }
        }
        if let Some(workflow) = member.workflows.get(&unit.workflow) {
            for spec in &workflow.needs {
                if let Err(error) = graph::parse_dependency(&workspace, unit, spec, true) {
                    problems.push((own_file.clone(), error));
                }
            }
        }
        if problems.is_empty()
            && let Err(error) = graph::unit_dependencies(&workspace, unit)
        {
            // What the entries can't say one at a time: a self-dependency.
            problems.push((own_file.clone(), error));
        }
        if !problems.is_empty() {
            failed.insert(unit.clone());
        }
        for (file, error) in problems {
            let message = format!("{error:#}");
            if !seen.insert(message.clone()) {
                continue;
            }
            // A `depends_on` problem belongs to the sub-workspace, not to the
            // workflow it was found through.
            let unit = (file == own_file).then(|| unit.to_string());
            report.problems.push(Problem {
                unit,
                file,
                message,
            });
        }
    }

    // Pass 3: each healthy unit with everything it pulls in.
    for (member, unit) in &units {
        if failed.contains(unit) || pulls_in_failed(&workspace, unit, &failed) {
            continue;
        }
        let result = graph::build(&workspace, &unit.workflow, &selection(unit, false))
            .and_then(|compiled| {
                let steps = compiled.steps.clone();
                graph::into_run(compiled)?;
                Ok(steps)
            })
            .and_then(|steps| {
                missing_registries(&workspace, root, &steps)
                    .into_iter()
                    .next()
                    .map_or(Ok(()), Err)
            });
        if let Err(error) = result {
            let message = format!("{error:#}");
            // A cycle fails every unit on it the same way: say it once.
            if seen.insert(message.clone()) {
                report.problems.push(Problem {
                    unit: Some(unit.to_string()),
                    file: workflow_file_of(root, member, &unit.workflow),
                    message,
                });
            }
        }
    }

    Ok(report)
}

/// Whether anything `unit` depends on, transitively, already failed — in
/// which case compiling it would only report that failure again.
fn pulls_in_failed(workspace: &Workspace, unit: &UnitId, failed: &BTreeSet<UnitId>) -> bool {
    let mut queue = vec![unit.clone()];
    let mut visited: BTreeSet<UnitId> = BTreeSet::new();
    while let Some(current) = queue.pop() {
        if !visited.insert(current.clone()) {
            continue;
        }
        let Ok(deps) = graph::unit_dependencies(workspace, &current) else {
            return true;
        };
        for dep in deps {
            if failed.contains(&dep) {
                return true;
            }
            queue.push(dep);
        }
    }
    false
}

/// A push or pull step naming a registry neither its own sub-workspace nor
/// the root defines. A run finds out only when it reaches the step, which is
/// after everything before it has already run.
fn missing_registries(
    workspace: &Workspace,
    root: &Path,
    steps: &[crate::run::RunStep],
) -> Vec<anyhow::Error> {
    let root_registries: BTreeSet<String> = config_path(root)
        .and_then(|path| load_config_file(&path).ok())
        .map(|config| config.registries.keys().cloned().collect())
        .unwrap_or_default();
    let mut errors = Vec::new();
    for step in steps {
        // A step bringing its own command doesn't use the built-in transfer.
        if step.run.is_some() || step.script.is_some() {
            continue;
        }
        let Some(name) = step.transfer().and_then(|t| t.registry) else {
            continue;
        };
        let own = step
            .workspace
            .as_deref()
            .and_then(|m| workspace.member(m))
            .is_some_and(|m| m.config.registries.contains_key(name));
        if !own && !root_registries.contains(name) {
            errors.push(anyhow::anyhow!(
                "Step '{}' pushes through registry '{name}', which neither its sub-workspace \
                 nor the root defines under `registries:`.",
                step.name
            ));
        }
    }
    errors
}

/// The config file a broken member's error is in.
fn config_file_of(root: &Path, dir: &Path) -> String {
    let file = config_path(dir).unwrap_or_else(|| dir.join(CIABATTA_DIR));
    relative(root, &file)
}

/// Where a workflow is written: its own file under `.ciabatta/workflows/`, or
/// inline in the sub-workspace's config.
fn workflow_file_of(root: &Path, member: &Member, workflow: &str) -> String {
    let dir = member.dir.join(CIABATTA_DIR).join(WORKFLOWS_DIR);
    let own = crate::format::config_files_in(&dir)
        .into_iter()
        .find(|path| path.file_stem().and_then(|s| s.to_str()) == Some(workflow));
    match own {
        Some(path) => relative(root, &path),
        None => config_file_of(root, &member.dir),
    }
}

fn relative(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

/// The report as the terminal shows it: each problem under the file to fix.
pub fn render(report: &Report) -> String {
    let mut out = format!(
        "Checked {} workflow{} in {} sub-workspace{}.\n",
        report.workflows,
        if report.workflows == 1 { "" } else { "s" },
        report.members,
        if report.members == 1 { "" } else { "s" },
    );
    if report.is_clean() {
        out.push_str("✓ Every workflow compiles, and everything they reference exists.\n");
        return out;
    }
    let mut by_file: BTreeMap<&str, Vec<&Problem>> = BTreeMap::new();
    for problem in &report.problems {
        by_file.entry(&problem.file).or_default().push(problem);
    }
    for (file, problems) in by_file {
        out.push_str(&format!("\n✗ {file}\n"));
        for problem in problems {
            if let Some(unit) = &problem.unit {
                out.push_str(&format!("  {unit}\n"));
            }
            for line in problem.message.lines() {
                out.push_str(&format!("    {line}\n"));
            }
        }
    }
    let n = report.problems.len();
    out.push_str(&format!(
        "\n{n} problem{} found.\n",
        if n == 1 { "" } else { "s" }
    ));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("ciab_lint_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write(root: &Path, rel: &str, body: &str) {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }

    fn member(root: &Path, dir: &str, name: &str) {
        write(
            root,
            &format!("{dir}/.ciabatta/ciabatta.yaml"),
            &format!("workspace:\n  name: {name}\n"),
        );
    }

    #[test]
    fn a_healthy_workspace_is_clean() {
        let root = scratch("clean");
        member(&root, "proto", "proto");
        member(&root, "api", "api");
        write(
            &root,
            "proto/.ciabatta/workflows/generate.yaml",
            "steps:\n  - name: gen\n    run: echo gen\n",
        );
        write(
            &root,
            "api/.ciabatta/workflows/build.yaml",
            "needs: [proto:generate]\nsteps:\n  - name: compile\n    run: echo c\n  \
             - name: package\n    run: echo p\n    needs: [compile]\n",
        );
        let report = lint(&root).unwrap();
        assert!(report.is_clean(), "{:?}", report.problems);
        assert_eq!(report.workflows, 2);
        assert!(render(&report).contains("Every workflow compiles"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn every_broken_reference_is_reported_against_its_own_file() {
        let root = scratch("broken");
        member(&root, "proto", "proto");
        member(&root, "api", "api");
        member(&root, "web", "web");
        member(&root, "bad", "bad");
        write(
            &root,
            "proto/.ciabatta/workflows/generate.yaml",
            "steps:\n  - name: gen\n    run: echo gen\n",
        );
        // A workflow-level dependency on a workflow that doesn't exist.
        write(
            &root,
            "api/.ciabatta/workflows/build.yaml",
            "needs: [proto:codegen]\nsteps:\n  - name: compile\n    run: echo c\n",
        );
        // A step needing a step that isn't there.
        write(
            &root,
            "web/.ciabatta/workflows/build.yaml",
            "steps:\n  - name: bundle\n    run: echo b\n    needs: [compil]\n",
        );
        // Depends on `web:build`, which is broken: reported once, against web.
        write(
            &root,
            "web/.ciabatta/workflows/test.yaml",
            "needs: [self:build]\nsteps:\n  - name: unit\n    run: echo t\n",
        );
        // A config file that doesn't parse at all.
        write(&root, "bad/.ciabatta/workflows/build.yaml", "steps: [\n");

        let report = lint(&root).unwrap();
        let files: Vec<&str> = report.problems.iter().map(|p| p.file.as_str()).collect();
        assert!(
            files.contains(&"api/.ciabatta/workflows/build.yaml"),
            "{files:?}"
        );
        assert!(
            files.contains(&"web/.ciabatta/workflows/build.yaml"),
            "{files:?}"
        );
        assert!(
            report.problems.iter().any(|p| p.unit.is_none()),
            "the unparseable member is reported: {:?}",
            report.problems
        );
        assert!(
            !files.contains(&"web/.ciabatta/workflows/test.yaml"),
            "a unit is not blamed for its dependency's problem: {:?}",
            report.problems
        );
        let api = report
            .problems
            .iter()
            .find(|p| p.unit.as_deref() == Some("api:build"))
            .unwrap();
        assert!(api.message.contains("codegen"), "{}", api.message);
        let web = report
            .problems
            .iter()
            .find(|p| p.unit.as_deref() == Some("web:build"))
            .unwrap();
        assert!(web.message.contains("compil"), "{}", web.message);
        assert!(render(&report).contains("problems found"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_bad_depends_on_is_reported_once_against_the_config_file() {
        let root = scratch("depends_on");
        write(
            &root,
            "web/.ciabatta/ciabatta.yaml",
            "workspace:\n  name: web\n  depends_on: [protoo]\n",
        );
        for name in ["build", "test", "dev"] {
            write(
                &root,
                &format!("web/.ciabatta/workflows/{name}.yaml"),
                "steps:\n  - name: x\n    run: echo x\n",
            );
        }
        let report = lint(&root).unwrap();
        assert_eq!(report.problems.len(), 1, "{:?}", report.problems);
        assert_eq!(report.problems[0].file, "web/.ciabatta/ciabatta.yaml");
        assert!(report.problems[0].unit.is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_dependency_cycle_is_reported_once() {
        let root = scratch("cycle");
        member(&root, "a", "a");
        member(&root, "b", "b");
        write(
            &root,
            "a/.ciabatta/workflows/build.yaml",
            "needs: [b:build]\nsteps:\n  - name: x\n    run: echo x\n",
        );
        write(
            &root,
            "b/.ciabatta/workflows/build.yaml",
            "needs: [a:build]\nsteps:\n  - name: y\n    run: echo y\n",
        );
        let report = lint(&root).unwrap();
        let cycles: Vec<&Problem> = report
            .problems
            .iter()
            .filter(|p| p.message.contains("cycle"))
            .collect();
        assert_eq!(cycles.len(), 1, "{:?}", report.problems);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_push_to_an_undefined_registry_is_reported() {
        let root = scratch("registry");
        member(&root, "api", "api");
        write(
            &root,
            "api/.ciabatta/workflows/publish.yaml",
            "steps:\n  - name: ship\n    kind: push\n    registry: nexus\n    artifact: dist/app\n",
        );
        let report = lint(&root).unwrap();
        assert!(
            report.problems.iter().any(|p| p.message.contains("nexus")),
            "{:?}",
            report.problems
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
