//! Requirements + trace support for `ciabatta analyze`.
//!
//! A *requirements file* lists project requirements (one per line, `id` or
//! `id, description`). A *trace file* is a CSV of `requirement,file` connections
//! linking requirements to the source files that satisfy them. Together they add
//! a leftmost "Requirements" column to the graph and wire each requirement to
//! the internal package that owns its traced file(s) — so requirements connect
//! through to the rest of the graph.

use anyhow::{Context, Result};
use std::collections::BTreeMap;

use super::{Category, GraphBuilder, Node, RequirementInputs};

/// Parse + apply the requirements/trace inputs onto the graph.
pub fn apply(builder: &mut GraphBuilder, inputs: &RequirementInputs<'_>) -> Result<()> {
    // Requirement id → description.
    let mut requirements: BTreeMap<String, Option<String>> = BTreeMap::new();
    // Requirement id → traced source files.
    let mut traced: BTreeMap<String, Vec<String>> = BTreeMap::new();

    if let Some(path) = inputs.requirements_file {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read requirements file {}", path.display()))?;
        for (id, desc) in parse_requirements(&text) {
            requirements.insert(id, desc);
        }
    }

    if let Some(path) = inputs.trace_file {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read trace file {}", path.display()))?;
        for (req, file) in parse_trace(&text) {
            // A traced requirement implicitly exists even without a requirements file.
            requirements.entry(req.clone()).or_insert(None);
            traced.entry(req).or_default().push(file);
        }
    }

    if requirements.is_empty() {
        return Ok(());
    }

    let root = builder.root_node_id().to_string();
    for (id, desc) in &requirements {
        let files = traced.get(id).cloned().unwrap_or_default();
        builder.add_node(Node {
            id: format!("req:{id}"),
            label: id.clone(),
            category: Category::Requirement,
            description: desc.clone(),
            traced_files: files.clone(),
            ..Default::default()
        });

        // Link the requirement to the internal package owning each traced file
        // (or the repo root when none matches), so it reaches the whole graph.
        let mut targets: Vec<String> = files
            .iter()
            .map(|f| builder.owner_for_file(f).unwrap_or_else(|| root.clone()))
            .collect();
        targets.sort();
        targets.dedup();
        for target in targets {
            builder.add_requirement_edge(&format!("req:{id}"), &target);
        }
    }

    Ok(())
}

/// Parse a requirements file: one requirement per line, `id` or `id, description`.
fn parse_requirements(text: &str) -> Vec<(String, Option<String>)> {
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        match line.split_once(',') {
            Some((id, desc)) => {
                let desc = desc.trim();
                out.push((
                    id.trim().to_string(),
                    (!desc.is_empty()).then(|| desc.to_string()),
                ));
            }
            None => out.push((line.to_string(), None)),
        }
    }
    out
}

/// Parse a trace CSV into `(requirement, file)` pairs. A header row naming the
/// columns (containing "requirement" and "file") is honored; otherwise the first
/// two columns are assumed to be requirement, file.
fn parse_trace(text: &str) -> Vec<(String, String)> {
    let mut lines = text.lines().filter(|l| !l.trim().is_empty()).peekable();
    let mut req_col = 0usize;
    let mut file_col = 1usize;

    if let Some(first) = lines.peek() {
        let cells: Vec<String> = first.split(',').map(|c| c.trim().to_lowercase()).collect();
        let req = cells.iter().position(|c| c.contains("requirement"));
        let file = cells.iter().position(|c| c.contains("file"));
        if let (Some(r), Some(f)) = (req, file) {
            req_col = r;
            file_col = f;
            lines.next(); // consume header
        }
    }

    let mut out = Vec::new();
    for line in lines {
        let cells: Vec<&str> = line.split(',').map(|c| c.trim()).collect();
        let (Some(req), Some(file)) = (cells.get(req_col), cells.get(file_col)) else {
            continue;
        };
        if !req.is_empty() && !file.is_empty() {
            out.push((req.to_string(), file.to_string()));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_requirements_with_and_without_descriptions() {
        let reqs = parse_requirements("# heading\nREQ-1, must do X\nREQ-2\n\n");
        assert_eq!(reqs.len(), 2);
        assert_eq!(reqs[0], ("REQ-1".into(), Some("must do X".into())));
        assert_eq!(reqs[1], ("REQ-2".into(), None));
    }

    #[test]
    fn parses_trace_with_header_in_any_column_order() {
        let csv = "file,requirement\nsrc/runner.rs,REQ-1\nfrontend/main.js,REQ-2\n";
        let rows = parse_trace(csv);
        assert_eq!(
            rows,
            vec![
                ("REQ-1".into(), "src/runner.rs".into()),
                ("REQ-2".into(), "frontend/main.js".into()),
            ]
        );
    }

    #[test]
    fn parses_trace_without_header() {
        let csv = "REQ-1,src/a.rs\nREQ-1,src/b.rs\n";
        let rows = parse_trace(csv);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0], ("REQ-1".into(), "src/a.rs".into()));
    }
}
