//! `ciabatta why <target>` — where a target is, and everything it depends on.
//!
//! The question this answers is the one people actually ask about a monorepo,
//! and it has two halves that are usually asked together: *where is this
//! thing?* and *why did it (not) rebuild?*
//!
//! Both are answerable from declarations ciabatta already resolves — the file
//! the target was written in, the directory it runs in, the chain of targets
//! that reach it, its inputs, outputs, variables and commands, and what the
//! cache would do with all of that. They were simply never printed together,
//! which meant answering "why" involved opening three files and guessing at the
//! fourth.
//!
//! A target here is a **node of a run graph**: one step of a workflow
//! (`api:build`) or of a recipe (`release.compile`). Naming a whole workflow or
//! recipe reports every node in it, because "why is `build` slow?" is a fair
//! question too.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use owo_colors::OwoColorize;

use crate::cache::cli::WorkspaceContext;
use crate::cache::graph::{Plan, Planned};
use crate::cache::{Decision, store::human_size};
use crate::config::CiabattaConfig;
use crate::run::RunStep;
use crate::run::deps::TargetDeps;
use crate::workspace::Workspace;

/// One graph a target could live in: a monorepo workflow or a project recipe.
struct Source {
    /// What to type to run it (`ciabatta run <label>`).
    label: String,
    /// `workflow` or `recipe`.
    kind: &'static str,
    /// The root its steps' paths are relative to.
    root: PathBuf,
    /// The config the steps' cache settings resolve against.
    config: CiabattaConfig,
    steps: Vec<RunStep>,
}

/// Everything `why` found out about one target.
struct Answer {
    source_label: String,
    source_kind: &'static str,
    step: RunStep,
    deps: TargetDeps,
    /// Where the target was written down, relative to the project root.
    declared_in: Option<String>,
    /// The chain of targets that reaches it, longest-first.
    path: Vec<String>,
    /// What the cache would do with it, when a plan could be computed.
    planned: Option<Planned>,
}

/// Answer `ciabatta why <target>` for the project (or monorepo) at `cwd`.
///
/// `all` lists every matched input and output file by name rather than counting
/// them. Off by default because a monorepo target's input set runs to thousands
/// of paths and the count is what answers the usual question; on when the count
/// is the thing that looks wrong, which is when you need to see which files it
/// actually picked up.
pub fn run(
    cwd: &Path,
    target: &str,
    vars: &HashMap<String, String>,
    json: bool,
    all: bool,
) -> Result<()> {
    let sources = sources(cwd)?;
    if sources.is_empty() {
        bail!(
            "Nothing runnable was found at or above {}.\n\
             Run `ciabatta init --lib` to opt a package in, or `ciabatta list` to see \
             what this project defines.",
            cwd.display()
        );
    }

    let detail = if all {
        crate::run::deps::Detail::Files
    } else {
        crate::run::deps::Detail::Counts
    };
    let answers = find(&sources, target, vars, detail);
    if answers.is_empty() {
        bail!("{}", not_found(&sources, target));
    }

    if json {
        println!("{}", serde_json::to_string_pretty(&as_json(&answers))?);
        return Ok(());
    }

    for (index, answer) in answers.iter().enumerate() {
        if index > 0 {
            println!();
        }
        print!("{}", render(answer, all));
    }
    Ok(())
}

// ─── Finding the target ─────────────────────────────────────────────────────

/// Every graph the target could be a node of.
///
/// Both kinds are searched, always. Which of the two somebody's target lives in
/// is exactly the thing they don't know when they're asking where it is.
fn sources(cwd: &Path) -> Result<Vec<Source>> {
    let mut sources = Vec::new();

    if let Ok(workspace) = Workspace::discover(cwd) {
        let config = crate::config::load_config(&workspace.root).unwrap_or_default();
        for name in workspace.workflow_names() {
            // A workflow that won't compile is reported by running it, in far
            // more detail than this command could add.
            let Ok(graph) = crate::workspace::graph::build(
                &workspace,
                &name,
                &crate::workspace::graph::Selection::default(),
            ) else {
                continue;
            };
            sources.push(Source {
                label: name,
                kind: "workflow",
                root: workspace.root.clone(),
                config: config.clone(),
                steps: graph.steps,
            });
        }
    }

    // The project's own recipes, from wherever `ciabatta run` would load them.
    if let Some(root) = crate::config::find_root(cwd)
        && let Ok(config) = crate::config::load_config(&root)
    {
        for (name, entry) in &config.recipes {
            let Some(recipe) = entry.run_recipe() else {
                continue;
            };
            let Ok(resolved) = crate::run::resolve_run(recipe, name, &root) else {
                continue;
            };
            sources.push(Source {
                label: name.clone(),
                kind: "recipe",
                root: root.clone(),
                config: config.clone(),
                steps: resolved.steps,
            });
        }
    }

    Ok(sources)
}

/// Every target matching `query`, across every source.
///
/// A name is matched four ways, most specific first, because people refer to a
/// target by whatever part of it they remember: its graph id (`api:build`), its
/// bare step name (`build`, which may well be ambiguous — all of them are then
/// reported), its `<graph>.<step>` spelling, or the name of a whole graph.
fn find(
    sources: &[Source],
    query: &str,
    vars: &HashMap<String, String>,
    detail: crate::run::deps::Detail,
) -> Vec<Answer> {
    let query = query.trim();
    let mut answers = Vec::new();

    for source in sources {
        let whole_graph = source.label.eq_ignore_ascii_case(query);
        let matches: Vec<&RunStep> = source
            .steps
            .iter()
            .filter(|step| whole_graph || names(source, step).iter().any(|n| n == query))
            .filter(|step| !step.recover)
            .collect();

        if matches.is_empty() {
            continue;
        }

        // One plan for the whole graph, not one per match: a step's key depends
        // on what the steps before it produced, so planning it in isolation
        // would answer a different question from the one the run will ask.
        let plan = plan_of(source, vars);
        let deps =
            crate::run::deps::collect_with(&source.config, &source.root, &source.steps, detail);

        for step in matches {
            let target_deps = deps
                .iter()
                .find(|d| d.name == step.name)
                .cloned()
                .unwrap_or_default();
            answers.push(Answer {
                source_label: source.label.clone(),
                source_kind: source.kind,
                declared_in: declared_in(source, step),
                path: longest_path(&source.steps, &step.name),
                planned: plan
                    .as_ref()
                    .and_then(|p| p.steps.iter().find(|s| s.name == step.name).cloned()),
                deps: target_deps,
                step: step.clone(),
            });
        }
    }

    answers
}

/// The names a step answers to.
fn names(source: &Source, step: &RunStep) -> Vec<String> {
    let mut names = vec![step.name.clone()];
    // `api:build` → also answers to `build`, the name written in the file.
    if let Some((_, bare)) = step.name.rsplit_once(':') {
        names.push(bare.to_string());
    }
    names.push(format!("{}.{}", source.label, step.name));
    names
}

/// What the cache would do with this graph, or `None` when it can't be planned.
///
/// Best-effort: `why` still answers the "where is it and what does it depend
/// on" half when the cache store is unreadable, which is more use than an error.
fn plan_of(source: &Source, vars: &HashMap<String, String>) -> Option<Plan> {
    let store = crate::cache::graph::store_for(&source.root).ok()?;
    let workspace = Workspace::discover(&source.root).ok();
    let context = WorkspaceContext {
        workspace: workspace.as_ref(),
        root: source.root.clone(),
        config: &source.config,
        recipe_cache: source.config.cache.clone(),
    };
    let env: BTreeMap<String, String> = vars.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    crate::cache::graph::plan_graph(&source.steps, &context, &env, &store).ok()
}

/// The file the target was written in, relative to the project root.
///
/// A workflow step lives either in `<member>/.ciabatta/workflows/<name>.yaml` or
/// inline in the member's own config; a recipe step lives in its flowchart file
/// or inline in the project config. Resolved by looking, rather than assumed,
/// because "it's in one of these two places" is precisely the answer somebody
/// asking this question already has.
fn declared_in(source: &Source, step: &RunStep) -> Option<String> {
    let root = &source.root;

    if source.kind == "workflow" {
        let workspace = Workspace::discover(root).ok()?;
        let member = workspace.member(step.workspace.as_deref()?)?;
        let bare = step
            .name
            .rsplit_once(':')
            .map_or(step.name.as_str(), |x| x.1);

        // Which of the member's workflows actually defines this step. Usually
        // the one the graph was compiled for, but a member may contribute
        // several, and a step name is only unique within one of them.
        let owning = member
            .workflows
            .iter()
            .find(|(_, workflow)| workflow.steps.iter().any(|s| s.name == bare))
            .map(|(name, _)| name.clone())
            .unwrap_or_else(|| source.label.clone());

        let dir = member.dir.join(crate::config::CIABATTA_DIR);
        let path = crate::format::find(&dir.join(crate::workspace::WORKFLOWS_DIR), &owning)
            .or_else(|| crate::config::config_path(&member.dir))?;
        return Some(relative(root, &path));
    }

    let recipe = source
        .config
        .recipes
        .get(&source.label)
        .and_then(|entry| entry.run_recipe())?;
    match &recipe.flowchart {
        Some(rel) => Some(rel.replace('\\', "/")),
        None => crate::config::config_path(root).map(|path| relative(root, &path)),
    }
}

fn relative(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

/// The longest chain of `needs` edges ending at `target`.
///
/// "What has to happen before this?" is a chain, not a set, and the longest one
/// is the one that decides when the target can start — the critical path
/// through its own dependencies. Cycles are impossible in a resolved graph, but
/// the walk guards against them anyway rather than trusting that at runtime.
fn longest_path(steps: &[RunStep], target: &str) -> Vec<String> {
    let by_name: HashMap<&str, &RunStep> = steps.iter().map(|s| (s.name.as_str(), s)).collect();
    let mut path = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut current = target.to_string();

    loop {
        if !seen.insert(current.clone()) {
            break;
        }
        path.push(current.clone());
        let Some(step) = by_name.get(current.as_str()) else {
            break;
        };
        // Follow whichever dependency has the longest chain behind it.
        let next = step
            .needs
            .iter()
            .filter(|need| !seen.contains(*need))
            .max_by_key(|need| depth(&by_name, need, &mut HashSet::new()));
        match next {
            Some(need) => current = need.clone(),
            None => break,
        }
    }

    path.reverse();
    path
}

fn depth(by_name: &HashMap<&str, &RunStep>, name: &str, seen: &mut HashSet<String>) -> usize {
    if !seen.insert(name.to_string()) {
        return 0;
    }
    let Some(step) = by_name.get(name) else {
        return 0;
    };
    1 + step
        .needs
        .iter()
        .map(|need| depth(by_name, need, seen))
        .max()
        .unwrap_or(0)
}

/// The message for a target nobody has heard of, with what there *is*.
fn not_found(sources: &[Source], query: &str) -> String {
    let mut known: Vec<String> = sources
        .iter()
        .flat_map(|source| source.steps.iter().map(|step| step.name.clone()))
        .collect();
    known.sort();
    known.dedup();

    // Anything sharing a word with the query is worth suggesting; a typo is far
    // more common here than a target that genuinely doesn't exist.
    let close: Vec<&String> = known
        .iter()
        .filter(|name| {
            name.contains(query) || query.contains(name.as_str()) || name.ends_with(query)
        })
        .take(5)
        .collect();

    let mut message = format!("No target called '{query}' in this project.");
    if !close.is_empty() {
        message.push_str(&format!(
            "\nDid you mean: {}?",
            close
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    message.push_str("\nList everything with `ciabatta list`.");
    message
}

// ─── Reporting ──────────────────────────────────────────────────────────────

fn render(answer: &Answer, all: bool) -> String {
    let deps = &answer.deps;
    let mut out = String::new();

    let headline = match &answer.step.description {
        Some(description) => format!("{} — {description}", answer.step.name.bold()),
        None => format!("{}", answer.step.name.bold()),
    };
    out.push_str(&format!("{headline}\n"));
    out.push_str(&format!(
        "  a step of the {} '{}'{}\n",
        answer.source_kind,
        answer.source_label,
        match &deps.workspace {
            Some(workspace) => format!(", in sub-workspace '{workspace}'"),
            None => String::new(),
        }
    ));

    out.push('\n');
    out.push_str(&format!("{}\n", "Where".bold()));
    if let Some(file) = &answer.declared_in {
        out.push_str(&row("declared in", file));
    }
    out.push_str(&row("runs in", &deps.dir));
    out.push_str(&row(
        "run it with",
        &format!("ciabatta run {}", answer.source_label),
    ));
    if let Some(owner) = &answer.step.owner {
        out.push_str(&row("owner", owner));
    }

    if answer.path.len() > 1 {
        out.push('\n');
        out.push_str(&format!("{}\n", "Path to it".bold()));
        out.push_str(&format!("  {}\n", answer.path.join(" → ")));
    }

    out.push('\n');
    out.push_str(&format!("{}\n", "Depends on".bold()));
    out.push_str(&row(
        "targets",
        &list_or(&deps.needs, "nothing — it can start immediately"),
    ));
    out.push_str(&row("files", &files_line(deps)));
    if all {
        out.push_str(&file_list(&deps.input_list));
    }
    out.push_str(&row("env", &env_line(deps)));
    out.push_str(&row(
        "commands",
        &list_or(&deps.commands, "nothing — it only orders other steps"),
    ));

    out.push('\n');
    out.push_str(&format!("{}\n", "Produces".bold()));
    out.push_str(&row("files", &outputs_line(deps)));
    if all {
        out.push_str(&file_list(&deps.output_list));
    }

    out.push('\n');
    out.push_str(&format!("{}\n", "Caching".bold()));
    match (&answer.planned, &deps.why_uncached) {
        (_, Some(why)) => out.push_str(&format!("  not cached — {why}\n")),
        (Some(planned), None) => {
            out.push_str(&format!("  {}\n", verdict(&planned.decision)));
            if let Some(key) = planned.decision.key() {
                out.push_str(&row("key", key));
            }
            if let Some(diff) = &planned.diff {
                out.push_str(&format!("  {}\n", diff.summary()));
            }
        }
        (None, None) => out.push_str("  the cache couldn't be read for this target\n"),
    }

    // The one thing here that is a *problem* rather than a fact.
    let undeclared = deps.undeclared_env();
    if deps.cached && !undeclared.is_empty() {
        out.push_str(&format!(
            "\n  ⚠ it reads {} without declaring {} in `cache.env`, so changing {} \n    \
             would not invalidate its cache entry.\n",
            undeclared.join(", "),
            if undeclared.len() == 1 { "it" } else { "them" },
            if undeclared.len() == 1 { "it" } else { "them" },
        ));
    }

    out
}

fn row(label: &str, value: &str) -> String {
    format!("  {:<13}{value}\n", label)
}

fn list_or(items: &[String], empty: &str) -> String {
    if items.is_empty() {
        empty.to_string()
    } else {
        items.join(", ")
    }
}

fn files_line(deps: &TargetDeps) -> String {
    if deps.inputs.is_empty() {
        return "none declared — nothing about this target's files is known".to_string();
    }
    let mut line = format!(
        "{} file(s), {} matching {}",
        deps.input_files,
        human_size(deps.input_bytes),
        deps.inputs.join(", ")
    );
    if !deps.exclude.is_empty() {
        line.push_str(&format!(" (excluding {})", deps.exclude.join(", ")));
    }
    line
}

fn outputs_line(deps: &TargetDeps) -> String {
    if deps.outputs.is_empty() {
        return "none declared — so there is nothing a cache hit could restore".to_string();
    }
    format!(
        "{} file(s), {} matching {}",
        deps.output_files,
        human_size(deps.output_bytes),
        deps.outputs.join(", ")
    )
}

/// Every file in a set, one per line, in the path order the key is computed in.
///
/// That ordering is the point rather than a detail: these are the files, in the
/// sequence they're hashed, that produce this target's cache key. Reading the
/// list is how you find the one that shouldn't be there — a generated file
/// nobody excluded, a stray editor backup, a vendored tree — which is the whole
/// reason for asking to see it rather than the count.
fn file_list(files: &[crate::cache::FileHash]) -> String {
    if files.is_empty() {
        return String::new();
    }

    // Right-aligned sizes in a fixed column, so the big ones are found by
    // scanning down rather than by reading every line.
    let width = files
        .iter()
        .map(|file| human_size(file.size).len())
        .max()
        .unwrap_or(0);

    files
        .iter()
        .map(|file| {
            format!(
                "  {:>width$}  {}\n",
                human_size(file.size),
                file.path,
                width = width + 13,
            )
        })
        .collect()
}

fn env_line(deps: &TargetDeps) -> String {
    let mut parts: Vec<String> = deps.env.clone();
    for key in deps.undeclared_env() {
        parts.push(format!("{key} (read, not declared)"));
    }
    if parts.is_empty() {
        return "no variables".to_string();
    }
    parts.join(" · ")
}

fn verdict(decision: &Decision) -> String {
    match decision {
        Decision::Fresh { .. } | Decision::Hit { .. } => {
            format!("{} {}", "✓".green(), decision.describe())
        }
        Decision::Rebuild { .. } => format!("{} {}", "●".yellow(), decision.describe()),
        Decision::Uncached { .. } => decision.describe(),
    }
}

/// The machine-readable form, for scripts and for the daemon.
fn as_json(answers: &[Answer]) -> serde_json::Value {
    serde_json::Value::Array(
        answers
            .iter()
            .map(|answer| {
                serde_json::json!({
                    "target": answer.step.name,
                    "graph": answer.source_label,
                    "graph_kind": answer.source_kind,
                    "description": answer.step.description,
                    "owner": answer.step.owner,
                    "declared_in": answer.declared_in,
                    "path": answer.path,
                    "deps": answer.deps,
                    "decision": answer.planned.as_ref().map(|p| &p.decision),
                })
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn step(name: &str, needs: &[&str]) -> RunStep {
        RunStep {
            name: name.to_string(),
            run: Some("make".to_string()),
            needs: needs.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn the_path_to_a_target_is_its_longest_chain_of_dependencies() {
        let steps = vec![
            step("proto", &[]),
            step("lint", &[]),
            step("common", &["proto"]),
            step("api", &["common", "lint"]),
        ];

        assert_eq!(
            longest_path(&steps, "api"),
            vec!["proto", "common", "api"],
            "the chain that decides when the target can start, not just its needs"
        );
        assert_eq!(longest_path(&steps, "proto"), vec!["proto"]);
    }

    /// A resolved graph is acyclic, but the walk must not hang if one ever isn't.
    #[test]
    fn a_cycle_does_not_hang_the_walk() {
        let steps = vec![step("a", &["b"]), step("b", &["a"])];
        let path = longest_path(&steps, "a");
        assert!(path.len() <= 2, "{path:?}");
    }

    #[test]
    fn a_target_answers_to_every_name_someone_might_use_for_it() {
        let source = Source {
            label: "build".into(),
            kind: "workflow",
            root: PathBuf::from("/tmp"),
            config: CiabattaConfig::default(),
            steps: Vec::new(),
        };
        let names = names(&source, &step("api:compile", &[]));
        assert!(names.contains(&"api:compile".to_string()));
        assert!(names.contains(&"compile".to_string()), "{names:?}");
        assert!(names.contains(&"build.api:compile".to_string()));
    }
}
