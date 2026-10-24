//! Editing a workflow's graph from the web app: pruning steps, and adding or
//! removing what each step — and each workflow — needs.
//!
//! [`declared`] reads what the workflow files say; [`save`] takes the whole
//! graph as it should be and writes it back. Save is a *set*: a step left out
//! is removed, a `needs` left empty is cleared. Working out what pruning a
//! step does to the steps after it is the editor's business, so that what
//! somebody saw on screen is exactly what lands in the file.
//!
//! The files are edited as text, not re-serialised: a workflow file is
//! something its owner wrote, with comments and an order of keys they chose,
//! and only the lines that change are touched. Every edit is checked by parsing
//! the result back and comparing it with what was asked for; a file that
//! doesn't round-trip is left alone, and so is every other file in the save.

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use super::Workspace;

/// One workflow of one sub-workspace, as its file declares it.
#[derive(Debug, Clone, Serialize)]
pub struct DeclaredUnit {
    pub member: String,
    pub workflow: String,
    /// The workflow file, relative to the workspace root — when there is one
    /// that can be edited.
    pub file: Option<String>,
    /// Why it can't be edited, when it can't.
    pub locked: Option<String>,
    /// The workflow's own `needs`: other sub-workspaces' workflows.
    pub needs: Vec<String>,
    /// `[workspace] depends_on`, which also orders this workflow after
    /// others' but lives in the sub-workspace's config: shown, not edited.
    pub depends_on: Vec<String>,
    pub steps: Vec<DeclaredStep>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DeclaredStep {
    pub name: String,
    pub needs: Vec<String>,
    /// Steps this one's `on_error` and `retry` route to. A step that is
    /// routed to can't be pruned without breaking the step routing to it.
    pub routes: Vec<String>,
    pub recover: bool,
    pub background: bool,
}

/// A workflow's graph as it should be saved.
#[derive(Debug, Clone, Deserialize)]
pub struct UnitEdit {
    pub member: String,
    pub workflow: String,
    pub needs: Vec<String>,
    /// The steps to keep, each with everything it needs. A step of the file
    /// not listed here is removed.
    pub steps: Vec<StepEdit>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct StepEdit {
    pub name: String,
    pub needs: Vec<String>,
}

/// Every `<member>:<workflow>` in the workspace: what a workflow's `needs` may
/// name.
pub fn targets(workspace: &Workspace) -> Vec<String> {
    workspace
        .members
        .iter()
        .flat_map(|m| m.workflows.keys().map(move |w| format!("{}:{w}", m.name)))
        .collect()
}

/// What the files say about `units` (`<member>:<workflow>` each).
pub fn declared(workspace: &Workspace, units: &[String]) -> Result<Vec<DeclaredUnit>> {
    units
        .iter()
        .map(|unit| {
            let (member_name, workflow_name) = split_unit(unit)?;
            let member = workspace
                .member(member_name)
                .with_context(|| format!("no sub-workspace called '{member_name}'"))?;
            let workflow = member.workflows.get(workflow_name).with_context(|| {
                format!("'{member_name}' has no workflow called '{workflow_name}'")
            })?;
            let (file, locked) = match file_of(workspace, member_name, workflow_name) {
                Ok((_, relative)) => (Some(relative), None),
                Err(why) => (None, Some(why.to_string())),
            };
            Ok(DeclaredUnit {
                member: member_name.to_string(),
                workflow: workflow_name.to_string(),
                file,
                locked,
                needs: workflow.needs.clone(),
                depends_on: member.meta.depends_on.clone(),
                steps: workflow
                    .steps
                    .iter()
                    .map(|s| DeclaredStep {
                        name: s.name.clone(),
                        needs: s.needs.clone(),
                        routes: [s.on_error.clone(), s.retry.clone()]
                            .into_iter()
                            .flatten()
                            .collect(),
                        recover: s.recover,
                        background: s.background,
                    })
                    .collect(),
            })
        })
        .collect()
}

/// Write `edits` back to their workflow files, all or none. Returns the files
/// written, relative to the workspace root.
pub fn save(workspace: &Workspace, edits: &[UnitEdit]) -> Result<Vec<String>> {
    let known = targets(workspace);
    let mut planned: Vec<(PathBuf, String, String)> = Vec::new();

    for edit in edits {
        let unit = format!("{}:{}", edit.member, edit.workflow);
        let (path, relative) = file_of(workspace, &edit.member, &edit.workflow)?;
        let current: super::Workflow = crate::format::load(&path)?;

        // Everything the edit names has to exist, and nothing it keeps may
        // point at something it removes.
        let kept: Vec<&str> = edit.steps.iter().map(|s| s.name.as_str()).collect();
        for step in &edit.steps {
            let Some(was) = current.steps.iter().find(|s| s.name == step.name) else {
                bail!("{relative} has no step called '{}'", step.name);
            };
            if let Some(dep) = step.needs.iter().find(|d| !kept.contains(&d.as_str())) {
                bail!(
                    "Step '{}' of {unit} would need '{dep}', which isn't a step of that workflow \
                     (or is one being removed). To wait on another sub-workspace, add it to the \
                     workflow's needs instead.",
                    step.name
                );
            }
            if step.needs.contains(&step.name) {
                bail!("Step '{}' of {unit} can't need itself", step.name);
            }
            for route in [was.on_error.as_deref(), was.retry.as_deref()]
                .into_iter()
                .flatten()
            {
                if !kept.contains(&route) {
                    bail!(
                        "Step '{}' of {unit} routes to '{route}' (on_error / retry), so '{route}' \
                         can't be removed. Remove '{}' too, or change its routing by hand.",
                        step.name,
                        step.name
                    );
                }
            }
        }
        if kept.is_empty() {
            bail!("{unit} would be left with no steps; a workflow needs at least one");
        }
        for need in &edit.needs {
            let names_member = workspace.member(need).is_some();
            if !names_member && !known.contains(need) {
                bail!(
                    "{unit} would need '{need}', which is neither a sub-workspace nor a \
                     `<sub-workspace>:<workflow>` in this workspace"
                );
            }
            if *need == unit || *need == edit.member {
                bail!("{unit} can't need itself");
            }
        }

        let existing = std::fs::read_to_string(&path)
            .with_context(|| format!("Failed to read {}", path.display()))?;
        let rendered = render(&existing, edit)
            .with_context(|| format!("Couldn't edit {relative}; it was left alone"))?;

        // The file has to read back as exactly what was asked for.
        let parsed: super::Workflow =
            crate::format::from_str(&rendered, crate::format::Format::Yaml).with_context(|| {
                format!("Saving would have broken {relative}, so it was left alone")
            })?;
        let names: Vec<&str> = parsed.steps.iter().map(|s| s.name.as_str()).collect();
        let needs_match = edit.steps.iter().all(|want| {
            parsed
                .steps
                .iter()
                .find(|s| s.name == want.name)
                .is_some_and(|s| s.needs == want.needs)
        });
        anyhow::ensure!(
            names == kept && needs_match && parsed.needs == edit.needs,
            "The edit didn't survive a round trip through {relative}; it was left alone"
        );
        planned.push((path, relative, rendered));
    }

    // Only once every file has an edit that works does any of them change.
    let mut written = Vec::new();
    for (path, relative, rendered) in planned {
        std::fs::write(&path, rendered)
            .with_context(|| format!("Failed to write {}", path.display()))?;
        written.push(relative);
    }
    Ok(written)
}

fn split_unit(unit: &str) -> Result<(&str, &str)> {
    unit.rsplit_once(':')
        .with_context(|| format!("'{unit}' doesn't name a workflow (`<member>:<workflow>`)"))
}

/// The YAML file `workflow` of `member` lives in, and its path relative to the
/// root — or why there's none the web app can edit.
fn file_of(workspace: &Workspace, member: &str, workflow: &str) -> Result<(PathBuf, String)> {
    let found = workspace
        .member(member)
        .with_context(|| format!("no sub-workspace called '{member}'"))?;
    let dir = found
        .dir
        .join(crate::config::CIABATTA_DIR)
        .join(super::WORKFLOWS_DIR);
    let Some(path) = crate::format::find(&dir, workflow) else {
        bail!(
            "The '{workflow}' workflow of '{member}' is written inside its ciabatta config \
             rather than in .ciabatta/workflows/{workflow}.yaml, so there's no workflow file \
             to save to. Move it into its own file to edit it here."
        );
    };
    let relative = path
        .strip_prefix(&workspace.root)
        .unwrap_or(&path)
        .to_string_lossy()
        .replace('\\', "/");
    if crate::format::Format::of_path(&path) != crate::format::Format::Yaml {
        bail!("{relative} is TOML; only YAML workflow files can be edited from the web app");
    }
    Ok((path, relative))
}

/// `document` with `edit` applied: steps not kept removed, every kept step's
/// `needs` set, and the workflow's own `needs` set.
fn render(document: &str, edit: &UnitEdit) -> Result<String> {
    let lines: Vec<String> = document.lines().map(String::from).collect();
    let Some(start) = lines.iter().position(|l| top_level_key(l) == Some("steps")) else {
        bail!("there's no top-level `steps:` list");
    };
    let end = (start + 1..lines.len())
        .find(|&i| starts_top_level(&lines[i]))
        .unwrap_or(lines.len());

    let items = list_items(&lines[start + 1..end])?
        .into_iter()
        .map(|(a, b)| (a + start + 1, b + start + 1))
        .collect::<Vec<_>>();

    // Back to front, so the spans still to visit keep their line numbers.
    let mut out = lines.clone();
    for &(from, to) in items.iter().rev() {
        let name = item_name(&out[from..to])
            .with_context(|| format!("couldn't find the name of the step at line {}", from + 1))?;
        match edit.steps.iter().find(|s| s.name == name) {
            None => {
                out.drain(from..to);
            }
            Some(step) => {
                let item = set_item_needs(&out[from..to], &step.needs)?;
                out.splice(from..to, item);
            }
        }
    }

    // The workflow's own `needs`: replaced where it is, or put just above
    // `steps:` when it's new.
    let span = top_level_span(&out, "needs");
    let line = (!edit.needs.is_empty()).then(|| format!("needs: {}", flow_list(&edit.needs)));
    match (span, line) {
        (Some((from, to)), line) => {
            out.splice(from..to, line);
        }
        (None, Some(line)) => {
            let steps = out
                .iter()
                .position(|l| top_level_key(l) == Some("steps"))
                .unwrap_or(out.len());
            out.insert(steps, line);
        }
        (None, None) => {}
    }

    let mut rendered = out.join("\n");
    rendered.push('\n');
    Ok(rendered)
}

/// The `[from, to)` lines of the top-level `key`, its value included.
fn top_level_span(lines: &[String], key: &str) -> Option<(usize, usize)> {
    let start = lines.iter().position(|l| top_level_key(l) == Some(key))?;
    let end = (start + 1..lines.len())
        .find(|&i| starts_top_level(&lines[i]) || is_filler(&lines[i]))
        .unwrap_or(lines.len());
    Some((start, end))
}

fn indent_of(line: &str) -> usize {
    line.len() - line.trim_start_matches(' ').len()
}

fn starts_top_level(line: &str) -> bool {
    !line.is_empty() && !line.starts_with([' ', '\t', '#'])
}

/// `key` when `line` opens a top-level `key:`.
fn top_level_key(line: &str) -> Option<&str> {
    if !starts_top_level(line) {
        return None;
    }
    let (key, _) = line.split_once(':')?;
    Some(key.trim())
}

fn is_item(line: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed == "-" || trimmed.starts_with("- ")
}

fn is_filler(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed.is_empty() || trimmed.starts_with('#')
}

/// The `[from, to)` line span of each item of a block list. Comments and blank
/// lines just above an item belong to it, so removing a step takes the comment
/// introducing it along.
fn list_items(block: &[String]) -> Result<Vec<(usize, usize)>> {
    let Some(first) = block.iter().position(|l| is_item(l)) else {
        bail!("`steps:` isn't a block list (`- name: …` on lines of their own)");
    };
    let indent = indent_of(&block[first]);
    let starts: Vec<usize> = (0..block.len())
        .filter(|&i| is_item(&block[i]) && indent_of(&block[i]) == indent)
        .collect();
    // Each item ends at its last line of content; the filler after it goes
    // with the next item, as that item's lead-in.
    let ends: Vec<usize> = starts
        .iter()
        .enumerate()
        .map(|(n, &at)| {
            let mut to = starts.get(n + 1).copied().unwrap_or(block.len());
            while to > at + 1 && is_filler(&block[to - 1]) {
                to -= 1;
            }
            to
        })
        .collect();
    // The first item's lead-in stays where it is, under `steps:`.
    let spans = (0..starts.len())
        .map(|n| (if n == 0 { starts[0] } else { ends[n - 1] }, ends[n]))
        .collect();
    Ok(spans)
}

/// The column an item's keys start at: past its `- `.
fn key_indent(item: &[String]) -> Option<usize> {
    let line = item.iter().find(|l| is_item(l))?;
    let dash = indent_of(line);
    let after = &line[dash + 1..];
    Some(dash + 1 + (after.len() - after.trim_start().len()))
}

/// The value of `key` among an item's own keys, and the line it's on.
fn item_key<'a>(item: &'a [String], key: &str) -> Option<(usize, &'a str)> {
    let column = key_indent(item)?;
    item.iter().enumerate().find_map(|(i, line)| {
        let rest = line.get(column..)?;
        let at_column = indent_of(line) == column || (is_item(line) && line.len() > column);
        let value = rest.strip_prefix(key)?.strip_prefix(':')?;
        at_column.then_some((i, value))
    })
}

fn item_name(item: &[String]) -> Option<String> {
    let (_, value) = item_key(item, "name")?;
    let value = value.split(" #").next().unwrap_or(value).trim();
    Some(value.trim_matches(['"', '\'']).to_string())
}

/// An item with its `needs` set to `needs`: rewritten in place, removed when
/// empty, or added under the item's first line when it had none.
fn set_item_needs(item: &[String], needs: &[String]) -> Result<Vec<String>> {
    let column = key_indent(item).context("not a list item")?;
    let pad = " ".repeat(column);
    let mut out = item.to_vec();
    match item_key(item, "needs") {
        Some((0, _)) => {
            bail!("a step whose first key is `needs` can't be edited here; put `name:` first")
        }
        Some((at, _)) => {
            // The value may run on: a block list under the key, at its column
            // or deeper.
            let mut end = at + 1;
            while end < out.len()
                && !is_filler(&out[end])
                && (indent_of(&out[end]) > column
                    || (indent_of(&out[end]) == column && is_item(&out[end])))
            {
                end += 1;
            }
            match needs.is_empty() {
                true => {
                    out.drain(at..end);
                }
                false => {
                    out.splice(at..end, [format!("{pad}needs: {}", flow_list(needs))]);
                }
            }
        }
        None if needs.is_empty() => {}
        None => out.insert(1, format!("{pad}needs: {}", flow_list(needs))),
    }
    Ok(out)
}

/// `[a, b]`, quoting any name YAML would otherwise read as something else.
fn flow_list(names: &[String]) -> String {
    let quoted: Vec<String> = names
        .iter()
        .map(|n| {
            let plain = !n.is_empty()
                && n.chars()
                    .all(|c| c.is_alphanumeric() || matches!(c, '-' | '_' | '.' | '/' | ':'))
                && !n.contains(": ");
            match plain {
                true => n.clone(),
                false => format!("{n:?}"),
            }
        })
        .collect();
    format!("[{}]", quoted.join(", "))
}

#[cfg(test)]
mod tests {
    use super::*;

    const FILE: &str = "\
# Builds the API.
description: Build it
needs: [proto]

steps:
  # Fetch first.
  - name: fetch
    run: npm ci

  - name: build
    run: npm run build # the slow one
    needs:
      - fetch

  # Then the checks.
  - name: test
    needs: [build]
    run: npm test
";

    fn edit(needs: &[&str], steps: &[(&str, &[&str])]) -> UnitEdit {
        UnitEdit {
            member: "api".into(),
            workflow: "build".into(),
            needs: needs.iter().map(|s| s.to_string()).collect(),
            steps: steps
                .iter()
                .map(|(name, needs)| StepEdit {
                    name: name.to_string(),
                    needs: needs.iter().map(|s| s.to_string()).collect(),
                })
                .collect(),
        }
    }

    fn parse(text: &str) -> super::super::Workflow {
        crate::format::from_str(text, crate::format::Format::Yaml).unwrap()
    }

    #[test]
    fn pruning_a_step_removes_it_and_its_comment_and_keeps_the_rest() {
        let out = render(
            FILE,
            &edit(&["proto"], &[("fetch", &[]), ("build", &["fetch"])]),
        )
        .unwrap();
        assert!(!out.contains("npm test"), "{out}");
        assert!(!out.contains("Then the checks"), "{out}");
        assert!(out.contains("# Fetch first."), "{out}");
        assert!(
            out.contains("# the slow one"),
            "untouched lines stay as written: {out}"
        );
        let parsed = parse(&out);
        assert_eq!(parsed.steps.len(), 2);
        assert_eq!(parsed.steps[1].needs, vec!["fetch"]);
    }

    #[test]
    fn needs_are_set_not_added_to() {
        let out = render(
            FILE,
            &edit(
                &[],
                &[
                    ("fetch", &[]),
                    ("build", &[]),
                    ("test", &["fetch", "build"]),
                ],
            ),
        )
        .unwrap();
        let parsed = parse(&out);
        assert!(
            parsed.steps[1].needs.is_empty(),
            "a block list is cleared: {out}"
        );
        assert_eq!(parsed.steps[2].needs, vec!["fetch", "build"]);
        assert!(
            parsed.needs.is_empty(),
            "and the workflow's own needs: {out}"
        );
        assert!(!out.contains("needs: [proto]"), "{out}");
        assert!(out.contains("# Builds the API."), "{out}");
    }

    #[test]
    fn a_step_without_needs_gets_them_under_its_name() {
        let out = render(
            FILE,
            &edit(
                &["proto", "web:build"],
                &[("fetch", &[]), ("build", &["fetch"]), ("test", &["build"])],
            ),
        )
        .unwrap();
        let parsed = parse(&out);
        assert_eq!(parsed.needs, vec!["proto", "web:build"]);
        let out = render(
            &out,
            &edit(
                &[],
                &[("fetch", &[]), ("build", &["fetch"]), ("test", &["fetch"])],
            ),
        )
        .unwrap();
        assert!(out.contains("  - name: fetch\n    run: npm ci"), "{out}");
        let out = render(
            FILE,
            &edit(&[], &[("fetch", &["test"]), ("build", &[]), ("test", &[])]),
        )
        .unwrap();
        assert!(
            out.contains("  - name: fetch\n    needs: [test]\n"),
            "{out}"
        );
    }

    #[test]
    fn saving_checks_what_it_writes_and_writes_all_or_nothing() {
        let root = std::env::temp_dir().join(format!("ciab_graph_edit_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let flows = root.join(".ciabatta").join("workflows");
        std::fs::create_dir_all(&flows).unwrap();
        std::fs::write(
            root.join(".ciabatta").join("ciabatta.yaml"),
            "workspace:\n  name: api\n",
        )
        .unwrap();
        std::fs::write(
            flows.join("build.yaml"),
            FILE.replace("needs: [proto]\n", ""),
        )
        .unwrap();
        let ws = Workspace::discover(&root).unwrap();
        let declared = declared(&ws, &["api:build".to_string()]).unwrap();
        assert_eq!(declared[0].steps.len(), 3);
        assert_eq!(
            declared[0].file.as_deref(),
            Some(".ciabatta/workflows/build.yaml")
        );

        // Keeping a step that needs a removed one is refused, untouched.
        let before = std::fs::read_to_string(flows.join("build.yaml")).unwrap();
        let err = save(&ws, &[edit(&[], &[("test", &["build"])])]).unwrap_err();
        assert!(err.to_string().contains("isn't a step"), "{err}");
        let err = save(&ws, &[edit(&["nowhere"], &[("fetch", &[])])]).unwrap_err();
        assert!(err.to_string().contains("neither a sub-workspace"), "{err}");
        assert_eq!(
            std::fs::read_to_string(flows.join("build.yaml")).unwrap(),
            before
        );

        let written = save(&ws, &[edit(&[], &[("fetch", &[]), ("test", &["fetch"])])]).unwrap();
        assert_eq!(written, vec![".ciabatta/workflows/build.yaml"]);
        let parsed = parse(&std::fs::read_to_string(flows.join("build.yaml")).unwrap());
        assert_eq!(parsed.steps.len(), 2);
        assert_eq!(parsed.steps[1].needs, vec!["fetch"]);
        std::fs::remove_dir_all(&root).ok();
    }
}
