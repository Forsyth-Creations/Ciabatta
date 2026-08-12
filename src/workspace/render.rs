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

    out.push_str(&format!(
        "Workflow '{}' — {} step(s) across {} sub-workspace(s), in {} wave(s)\n",
        graph.label(),
        graph.steps.iter().filter(|s| !s.recover).count(),
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
    if step.persistent {
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

/// The whole monorepo's catalogue: every sub-workspace, its workflows, and
/// their steps — with descriptions and owners, so nobody has to open a script
/// to learn what it does.
///
/// `search` filters to entries matching the term anywhere that matters (names,
/// descriptions, owners, tags, commands); a sub-workspace with no surviving
/// workflow is dropped entirely.
pub fn catalogue(workspace: &Workspace, search: Option<&str>, verbose: bool) -> String {
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
    }

    out
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
    use crate::config::{CIABATTA_DIR, CONFIG_FILE};
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
        std::fs::write(dir.join(CIABATTA_DIR).join(CONFIG_FILE), config).unwrap();
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
        let text = catalogue(&ws, None, true);
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
        let text = catalogue(&ws, Some("stubs"), false);
        assert!(text.contains("proto"));
        assert!(!text.contains("Build the API"));

        // Matches an owner.
        let text = catalogue(&ws, Some("ada"), false);
        assert!(text.contains("api"));
        assert!(!text.contains("Generate stubs"));

        // Matches nothing.
        let text = catalogue(&ws, Some("kubernetes"), false);
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
