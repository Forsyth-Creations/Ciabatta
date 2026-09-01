//! Turning a workflow graph, and the monorepo's catalogue of workflows, into
//! something readable in a terminal.
//!
//! Both views exist to answer questions a monorepo usually can't: *what will
//! actually run, in what order, and from where?* — and *what is there to run at
//! all, and who owns it?* Every node is labelled with the sub-workspace it came
//! from, because "which package is this step from" is the first thing anyone
//! asks when a shared build goes wrong.

use std::collections::BTreeMap;

use crate::run::RunStep;

use super::graph::WorkflowGraph;
use super::{MissingTool, Workspace};

/// Draw the compiled graph: one block per dependency wave, each node labelled
/// with its sub-workspace, description, and anything unusual about how it runs.
pub fn graph(workspace: &Workspace, graph: &WorkflowGraph) -> String {
    let mut out = String::new();
    let waves = graph.waves();
    let members: Vec<&str> = {
        let mut names: Vec<&str> = graph.units.iter().map(|u| u.member.as_str()).collect();
        names.dedup();
        names
    };

    let background: Vec<&RunStep> = graph.steps.iter().filter(|s| s.background).collect();

    out.push_str(&format!(
        "Workflow '{}' — {} step(s) across {} sub-workspace(s), in {} wave(s)\n",
        graph.label(),
        graph
            .steps
            .iter()
            .filter(|s| !s.recover && !s.background)
            .count(),
        members.len(),
        waves.len()
    ));
    out.push_str(&format!("Root: {}\n", workspace.root.display()));

    for (index, wave) in waves.iter().enumerate() {
        out.push_str(&format!("\n  wave {} — runs in parallel\n", index + 1));
        for (position, step) in wave.iter().enumerate() {
            let last = position + 1 == wave.len();
            let branch = if last { "└─" } else { "├─" };
            let gutter = if last { "   " } else { "│  " };
            out.push_str(&format!(
                "  {branch} {}{}\n",
                step.name,
                badges(step).map(|b| format!("  {b}")).unwrap_or_default()
            ));

            let indent = format!("  {gutter}   ");
            if let Some(desc) = step.description.as_deref() {
                out.push_str(&format!("{indent}{desc}\n"));
            }
            out.push_str(&format!(
                "{indent}from {} ({}), owner {}\n",
                step.workspace.as_deref().unwrap_or("—"),
                step.cwd.as_deref().unwrap_or("."),
                step.owner.as_deref().unwrap_or("unowned"),
            ));
            if !step.needs.is_empty() {
                out.push_str(&format!("{indent}after {}\n", step.needs.join(", ")));
            }
            if !step.requires.is_empty() {
                out.push_str(&format!(
                    "{indent}needs tools {}\n",
                    step.requires.join(", ")
                ));
            }
            if let Some(target) = step.on_error.as_deref() {
                out.push_str(&format!("{indent}on failure → {target}\n"));
            }
        }
    }

    // Background targets are in the graph but not in the order — a wave means
    // "the next one waits for these", and nothing waits for these — so they get
    // their own section rather than being silently dropped from the picture.
    if !background.is_empty() {
        out.push_str(
            "\n  ⚡ background — up before wave 1, nothing waits for them, \
             stopped when the run ends\n",
        );
        for node in &background {
            out.push_str(&format!(
                "  ⚡ {}   from {} ({})\n",
                node.name,
                node.workspace.as_deref().unwrap_or("—"),
                node.cwd.as_deref().unwrap_or("."),
            ));
            if let Some(desc) = node.description.as_deref() {
                out.push_str(&format!("       {desc}\n"));
            }
            if !node.needs.is_empty() {
                out.push_str(&format!("       after {}\n", node.needs.join(", ")));
            }
        }
    }

    // Recovery nodes hang off a step rather than sitting in a wave, so they get
    // their own section instead of being silently dropped from the picture.
    let recoveries: Vec<&RunStep> = graph.steps.iter().filter(|s| s.recover).collect();
    if !recoveries.is_empty() {
        out.push_str("\n  recovery nodes — entered only when a step fails\n");
        for node in recoveries {
            let options: Vec<&str> = node.options.iter().map(|o| o.label.as_str()).collect();
            out.push_str(&format!("  ◆ {} — {}\n", node.name, options.join(" | ")));
        }
    }

    out
}

/// The short markers after a node's name: its phase, and any behaviour that
/// changes what "this step finished" means.
fn badges(step: &RunStep) -> Option<String> {
    let mut tags: Vec<String> = Vec::new();
    if step.is_push() {
        tags.push("⇧ push".to_string());
    } else if let Some(kind) = step.kind.as_deref() {
        tags.push(kind.to_string());
    }
    if step.background {
        tags.push("⚡ background".to_string());
    } else if step.persistent {
        tags.push("persistent".to_string());
    }
    if let Some(timeout) = step.timeout.as_deref() {
        tags.push(format!("timeout {timeout}"));
    }
    if step.retries > 0 {
        tags.push(format!("{} retries", step.retries));
    }
    if step.continue_on_error {
        tags.push("non-blocking".to_string());
    }
    if !step.when.is_empty() || !step.skip_if.is_empty() {
        tags.push("conditional".to_string());
    }
    if tags.is_empty() {
        None
    } else {
        Some(format!("[{}]", tags.join(", ")))
    }
}

/// One workflow's "when did this last run", phrased for a person.
///
/// Three distinct answers, kept distinct on purpose. **Never run here** is not
/// evidence of anything — a fresh checkout has no history and a colleague may
/// run it hourly — so it must not read like a verdict. **Stale** is the verdict.
/// Anything else is just a fact about a workflow that is plainly in use.
fn last_run(
    history: &crate::run::history::History,
    member: &str,
    workflow: &str,
    stale_after: std::time::Duration,
) -> String {
    let Some(record) = history.get(member, workflow) else {
        return "last run: never run here (no history for it on this machine)".to_string();
    };

    let when = match record.days_since() {
        Some(0) => "today".to_string(),
        Some(1) => "yesterday".to_string(),
        Some(days) if days > 0 => format!("{days} days ago"),
        // A record from the future is a clock disagreement, not a lie worth
        // arithmetic. Show the timestamp and let the reader judge.
        _ => record.last_run_at.clone(),
    };

    let mut line = format!(
        "last run: {when} ({}, {} run(s))",
        record.last_outcome.label(),
        record.runs
    );
    if record.is_stale(stale_after) {
        line.push_str("  ← STALE");
    }
    line
}

/// The whole monorepo's catalogue: every sub-workspace, its workflows, and
/// their steps — with descriptions and owners, so nobody has to open a script
/// to learn what it does.
///
/// `search` filters to entries matching the term anywhere that matters (names,
/// descriptions, owners, tags, commands); a sub-workspace with no surviving
/// workflow is dropped entirely.
pub fn catalogue(
    workspace: &Workspace,
    search: Option<&str>,
    verbose: bool,
    history: &crate::run::history::History,
    stale_after: std::time::Duration,
) -> String {
    let needle = search.map(|s| s.to_lowercase());
    let mut out = String::new();
    let mut shown = 0usize;

    out.push_str(&format!(
        "Workspace: {}\n{} sub-workspace(s), {} workflow name(s){}\n",
        workspace.root.display(),
        workspace.members.len(),
        workspace.workflow_names().len(),
        match search {
            Some(term) => format!(", filtered by \"{term}\""),
            None => String::new(),
        }
    ));

    for member in &workspace.members {
        let workflows: Vec<(&String, &super::Workflow)> = member
            .workflows
            .iter()
            .filter(|(name, workflow)| {
                needle.as_deref().is_none_or(|term| {
                    matches_member(member, term) || matches_workflow(name, workflow, term)
                })
            })
            .collect();
        if workflows.is_empty() {
            continue;
        }
        shown += workflows.len();

        out.push_str(&format!("\n▪ {}  ({})\n", member.name, member.rel));
        if let Some(desc) = member.meta.description.as_deref() {
            out.push_str(&format!("  {desc}\n"));
        }
        out.push_str(&format!("  owner: {}", member.owner()));
        if !member.meta.tags.is_empty() {
            out.push_str(&format!("  ·  tags: {}", member.meta.tags.join(", ")));
        }
        if !member.meta.depends_on.is_empty() {
            out.push_str(&format!(
                "  ·  depends on: {}",
                member.meta.depends_on.join(", ")
            ));
        }
        out.push('\n');

        for (name, workflow) in workflows {
            out.push_str(&format!(
                "\n    {:<16} {}\n",
                name,
                workflow
                    .description
                    .as_deref()
                    .unwrap_or("(no description)")
            ));
            // A long owner name must not shove the rest of the line out of
            // alignment, so the details go on their own indented lines.
            let owner = workflow.owner.as_deref().unwrap_or_else(|| member.owner());
            out.push_str(&format!("    {:<16} owner: {owner}\n", ""));
            if !workflow.needs.is_empty() {
                out.push_str(&format!(
                    "    {:<16} after: {}\n",
                    "",
                    workflow.needs.join(", ")
                ));
            }
            out.push_str(&format!(
                "    {:<16} run: ciabatta {name} --only {}\n",
                "", member.name
            ));
            out.push_str(&format!(
                "    {:<16} {}\n",
                "",
                last_run(history, &member.name, name, stale_after)
            ));
            if verbose {
                for step in &workflow.steps {
                    out.push_str(&format!(
                        "      · {:<20} {}\n",
                        step.name,
                        step.description
                            .as_deref()
                            .or(step.run.as_deref())
                            .or(step.script.as_deref())
                            .unwrap_or("(no description)")
                    ));
                }
            }
        }
    }

    if shown == 0 {
        match search {
            Some(term) => out.push_str(&format!("\nNothing matches \"{term}\".\n")),
            None => out.push_str(
                "\nNo workflows are defined yet.\n\
                 Run `ciabatta init --lib` in a package to add one.\n",
            ),
        }
    } else {
        out.push_str(&format!(
            "\n{shown} workflow(s). Run one across the whole workspace with `ciabatta <workflow>`,\n\
             or see the graph first with `ciabatta <workflow> --graph`.\n"
        ));

        // Named, not just counted. "3 stale workflows" sends somebody hunting
        // through the list above; naming them is the difference between a
        // number and something to act on.
        let stale: Vec<&crate::run::history::Record> = history
            .records()
            .into_iter()
            .filter(|record| record.is_stale(stale_after))
            .collect();
        if !stale.is_empty() {
            out.push_str(&format!(
                "\n{} workflow(s) not run in over {}:\n",
                stale.len(),
                humanize(stale_after)
            ));
            for record in stale {
                out.push_str(&format!(
                    "  {:<28} {} days ago\n",
                    record.id(),
                    record.days_since().unwrap_or_default()
                ));
            }
            out.push_str(
                "Each is either worth running or worth deleting — a workflow nobody runs is\n\
                 one nobody has noticed is broken.\n",
            );
        }
    }

    out
}

/// A duration as somebody would have written it in the config.
fn humanize(d: std::time::Duration) -> String {
    let secs = d.as_secs();
    match (secs / 86_400, secs / 3600) {
        (0, 0) => format!("{secs}s"),
        (0, hours) => format!("{hours}h"),
        (1, _) => "a day".to_string(),
        (days, _) => format!("{days} days"),
    }
}

/// Every workflow name in the monorepo with the sub-workspaces that define it —
/// the one-screen answer to "what can I run here?".
pub fn summary(workspace: &Workspace) -> String {
    let mut by_name: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for member in &workspace.members {
        for name in member.workflows.keys() {
            by_name
                .entry(name.as_str())
                .or_default()
                .push(member.name.as_str());
        }
    }
    if by_name.is_empty() {
        return String::new();
    }

    let mut out = String::from("Workflows (run any of these with `ciabatta <name>`):\n");
    for (name, members) in by_name {
        out.push_str(&format!(
            "  {:<16} {} sub-workspace(s): {}\n",
            name,
            members.len(),
            members.join(", ")
        ));
    }
    out
}

/// Report tools the graph needs that this machine doesn't have, with whatever
/// install command the repo documented in `[toolchain]`.
pub fn missing_tools(missing: &[MissingTool]) -> String {
    if missing.is_empty() {
        return String::new();
    }
    let mut out = format!("Missing {} build tool(s):\n", missing.len());
    for tool in missing {
        out.push_str(&format!(
            "  • {} — needed by {}\n",
            tool.tool,
            tool.needed_by.join(", ")
        ));
        match tool.hint.as_deref() {
            Some(hint) => out.push_str(&format!("    install it with: {hint}\n")),
            None => out.push_str(&format!(
                "    no install hint recorded — add one:  [toolchain.{}] hint = \"...\"\n",
                tool.tool
            )),
        }
    }
    out
}

fn matches_member(member: &super::Member, term: &str) -> bool {
    contains(&member.name, term)
        || contains(member.meta.description.as_deref().unwrap_or(""), term)
        || contains(member.owner(), term)
        || member.meta.tags.iter().any(|t| contains(t, term))
}

fn matches_workflow(name: &str, workflow: &super::Workflow, term: &str) -> bool {
    contains(name, term)
        || contains(workflow.description.as_deref().unwrap_or(""), term)
        || contains(workflow.owner.as_deref().unwrap_or(""), term)
        || workflow.tags.iter().any(|t| contains(t, term))
        || workflow.steps.iter().any(|step| {
            contains(&step.name, term)
                || contains(step.description.as_deref().unwrap_or(""), term)
                || contains(step.run.as_deref().unwrap_or(""), term)
                || contains(step.script.as_deref().unwrap_or(""), term)
        })
}

fn contains(haystack: &str, needle: &str) -> bool {
    haystack.to_lowercase().contains(needle)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::CIABATTA_DIR;
    use crate::workspace::graph::Selection;
    use std::path::{Path, PathBuf};

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ciab_render_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn member(root: &Path, rel: &str, config: &str) -> PathBuf {
        let dir = root.join(rel);
        std::fs::create_dir_all(dir.join(CIABATTA_DIR)).unwrap();
        std::fs::write(dir.join(CIABATTA_DIR).join("ciabatta.toml"), config).unwrap();
        dir
    }

    fn workflow(member: &Path, name: &str, body: &str) {
        let dir = member
            .join(CIABATTA_DIR)
            .join(crate::workspace::WORKFLOWS_DIR);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(format!("{name}.toml")), body).unwrap();
    }

    fn sample(name: &str) -> (PathBuf, Workspace) {
        let root = scratch(name);
        let api = member(
            &root,
            "packages/api",
            "[workspace]\nname = \"api\"\ndescription = \"REST API\"\nowner = \"Ada\"\n\
             tags = [\"backend\"]\ndepends_on = [\"proto:generate\"]\n",
        );
        workflow(
            &api,
            "build",
            "description = \"Build the API\"\n\
             [[steps]]\nname = \"compile\"\ndescription = \"cargo build\"\nrun = \"cargo build\"\n\
             requires = [\"cargo\"]\ntimeout = \"10m\"\n",
        );
        let proto = member(
            &root,
            "packages/proto",
            "[workspace]\nname = \"proto\"\nowner = \"Grace\"\n",
        );
        workflow(
            &proto,
            "generate",
            "description = \"Generate stubs\"\n[[steps]]\nname = \"protoc\"\nrun = \"protoc\"\n",
        );
        let ws = Workspace::load(&root).unwrap();
        (root, ws)
    }

    /// No history: what a fresh checkout has, and the state most of these
    /// assertions are indifferent to.
    fn empty_history() -> crate::run::history::History {
        crate::run::history::History::default()
    }

    fn month() -> std::time::Duration {
        std::time::Duration::from_secs(30 * 24 * 60 * 60)
    }

    #[test]
    fn the_graph_labels_every_node_with_its_sub_workspace() {
        let (root, ws) = sample("graph");
        let compiled = crate::workspace::graph::build(&ws, "build", &Selection::default()).unwrap();
        let text = graph(&ws, &compiled);

        assert!(text.contains("Workflow 'build'"));
        assert!(text.contains("2 sub-workspace(s)"));
        assert!(text.contains("proto:protoc"));
        assert!(text.contains("api:compile"));
        // Provenance, ownership, dependency and toolchain are all on the node.
        assert!(text.contains("from api (packages/api)"));
        assert!(text.contains("owner Ada"));
        assert!(text.contains("after proto:protoc"));
        assert!(text.contains("needs tools cargo"));
        assert!(text.contains("timeout 10m"));

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn the_catalogue_lists_owners_and_descriptions() {
        let (root, ws) = sample("catalogue");
        let text = catalogue(&ws, None, true, &empty_history(), month());
        assert!(text.contains("▪ api  (packages/api)"));
        assert!(text.contains("owner: Ada"));
        assert!(text.contains("Build the API"));
        assert!(text.contains("Generate stubs"));
        // Verbose mode drills into the steps.
        assert!(text.contains("compile"));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn search_filters_to_matching_sub_workspaces_and_steps() {
        let (root, ws) = sample("search");

        // Matches a workflow description.
        let text = catalogue(&ws, Some("stubs"), false, &empty_history(), month());
        assert!(text.contains("proto"));
        assert!(!text.contains("Build the API"));

        // Matches an owner.
        let text = catalogue(&ws, Some("ada"), false, &empty_history(), month());
        assert!(text.contains("api"));
        assert!(!text.contains("Generate stubs"));

        // Matches nothing.
        let text = catalogue(&ws, Some("kubernetes"), false, &empty_history(), month());
        assert!(text.contains("Nothing matches"));

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn the_summary_groups_workflow_names_by_provider() {
        let (root, ws) = sample("summary");
        let text = summary(&ws);
        assert!(text.contains("build"));
        assert!(text.contains("generate"));
        assert!(text.contains("1 sub-workspace(s): api"));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn missing_tool_output_nudges_for_a_hint_when_none_exists() {
        let report = missing_tools(&[MissingTool {
            tool: "protoc".into(),
            hint: None,
            needed_by: vec!["api:compile".into()],
        }]);
        assert!(report.contains("protoc — needed by api:compile"));
        assert!(report.contains("[toolchain.protoc]"));

        let report = missing_tools(&[MissingTool {
            tool: "protoc".into(),
            hint: Some("brew install protobuf".into()),
            needed_by: vec!["api:compile".into()],
        }]);
        assert!(report.contains("brew install protobuf"));

        // Nothing missing prints nothing at all.
        assert!(missing_tools(&[]).is_empty());
    }
}
