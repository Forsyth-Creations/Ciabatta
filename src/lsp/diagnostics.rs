//! Reporting the references that don't resolve.
//!
//! The counterpart to completion: the same knowledge that can offer every
//! `"<member>:<workflow>"` in the repo can say when one doesn't exist. That is
//! worth reporting in the editor rather than at `ciabatta build` time, because
//! by then the person who wrote it has moved on and the person reading the
//! error hasn't.
//!
//! Only cross-file references are checked here. The shape of the file — which
//! keys exist, what type each takes — is the JSON Schema's job, and the editor
//! is already running it.

use serde_json::{Value, json};

use super::context::step_names;
use super::index::{Index, Role};

/// Severity 2, `Warning`: an unresolved reference is a real problem, but a repo
/// mid-rename hits it constantly and a wall of red would just get ignored.
const WARNING: i64 = 2;

/// Every unresolved reference in the document, as protocol `Diagnostic`s.
pub fn check(lines: &[&str], role: &Role, index: &Index) -> Value {
    // Nothing to compare against — the workspace didn't load, or this file
    // isn't in one. Claiming every reference is broken would be worse than
    // saying nothing.
    if index.members.is_empty() {
        return json!([]);
    }

    let mut out = Vec::new();
    let steps = step_names(lines);

    for (line, raw) in lines.iter().enumerate() {
        let Some(entry) = sequence_entry(raw) else {
            continue;
        };
        let Some(field) = field_for(lines, line) else {
            continue;
        };

        let problem = match (field, role) {
            // A workflow's `needs:` and a workspace's `depends_on:` both name
            // other packages.
            ("needs", Role::Workflow(_)) | ("depends_on", Role::Config)
                if !in_a_step(lines, line) =>
            {
                (!index.resolves(entry.text)).then(|| {
                    format!(
                        "No sub-workspace here defines `{}`. {}",
                        entry.text,
                        suggest(entry.text, &index.workflow_refs(None))
                    )
                })
            }
            // A step's `needs:` names steps in this same file.
            ("needs", Role::Workflow(_)) => (!steps.iter().any(|s| s == entry.text)).then(|| {
                format!(
                    "No step named `{}` in this workflow. {}",
                    entry.text,
                    suggest(
                        entry.text,
                        &steps
                            .iter()
                            .map(|s| (s.clone(), String::new()))
                            .collect::<Vec<_>>()
                    )
                )
            }),
            ("requires", _) => (!index.tools.contains_key(entry.text)).then(|| {
                format!(
                    "`{}` has no `toolchain:` entry, so a missing install has no hint to offer. \
                     Add one at the monorepo root.",
                    entry.text
                )
            }),
            _ => None,
        };

        if let Some(message) = problem {
            out.push(diagnostic(line, entry.start, entry.end, &message));
        }
    }

    Value::Array(out)
}

/// A scalar entry in a block sequence, with the columns it occupies.
struct Entry<'a> {
    text: &'a str,
    start: usize,
    end: usize,
}

fn sequence_entry(raw: &str) -> Option<Entry<'_>> {
    let indent = raw.len() - raw.trim_start().len();
    let after = raw[indent..].strip_prefix("- ")?;
    let text = after.split('#').next()?.trim().trim_matches(['"', '\'']);
    if text.is_empty() || text.contains(':') && text.contains(' ') {
        return None; // a mapping entry, not a scalar
    }
    let start = raw.find(text)?;
    Some(Entry {
        text,
        start,
        end: start + text.len(),
    })
}

/// The mapping key a sequence entry belongs to: the nearest preceding line
/// indented less than it.
fn field_for(lines: &[&str], line: usize) -> Option<&'static str> {
    let indent = lines[line].len() - lines[line].trim_start().len();
    for raw in lines[..line].iter().rev() {
        let trimmed = raw.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let own = raw.len() - raw.trim_start().len();
        if own >= indent {
            continue;
        }
        let key = trimmed.trim_start_matches("- ").split(':').next()?.trim();
        return ["needs", "depends_on", "requires"]
            .into_iter()
            .find(|k| *k == key);
    }
    None
}

/// Whether this line sits inside the `steps:` list, which is what decides
/// whether a `needs:` means a step or another package.
fn in_a_step(lines: &[&str], line: usize) -> bool {
    for raw in lines[..line].iter().rev() {
        let trimmed = raw.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let indent = raw.len() - raw.trim_start().len();
        if indent == 0 {
            return trimmed.starts_with("steps:");
        }
    }
    false
}

/// "Did you mean X?" when one candidate is close enough to be worth naming.
///
/// A typo is one or two edits away; anything further apart is a different word,
/// and guessing at it wastes the reader's attention.
fn suggest(typo: &str, candidates: &[(String, String)]) -> String {
    let best = candidates
        .iter()
        .map(|(name, _)| (distance(typo, name), name))
        .filter(|(d, _)| *d <= 2)
        .min_by_key(|(d, _)| *d);
    match best {
        Some((_, name)) => format!("Did you mean `{name}`?"),
        None => String::new(),
    }
}

/// Levenshtein distance, two rows at a time.
fn distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0; b.len() + 1];

    for (i, ca) in a.iter().enumerate() {
        cur[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let substitute = prev[j] + usize::from(ca != cb);
            cur[j + 1] = substitute.min(prev[j + 1] + 1).min(cur[j] + 1);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

fn diagnostic(line: usize, start: usize, end: usize, message: &str) -> Value {
    json!({
        "range": {
            "start": { "line": line, "character": start },
            "end":   { "line": line, "character": end },
        },
        "severity": WARNING,
        "source": "ciabatta",
        "message": message.trim(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lsp::index::MemberInfo;

    fn index() -> Index {
        Index {
            members: vec![MemberInfo {
                name: "proto".into(),
                description: None,
                owner: "Henry Forsyth".into(),
                workflows: [("generate".to_string(), None)].into_iter().collect(),
            }],
            tools: [("cargo".to_string(), None)].into_iter().collect(),
            ..Index::default()
        }
    }

    fn messages(src: &str, role: Role) -> Vec<String> {
        let lines: Vec<&str> = src.lines().collect();
        check(&lines, &role, &index())
            .as_array()
            .unwrap()
            .iter()
            .map(|d| d["message"].as_str().unwrap().to_string())
            .collect()
    }

    #[test]
    fn a_workflow_reference_that_resolves_is_quiet() {
        assert!(
            messages(
                "needs:\n  - proto:generate\n",
                Role::Workflow("build".into())
            )
            .is_empty()
        );
    }

    #[test]
    fn a_workflow_reference_that_does_not_resolve_is_reported_with_a_guess() {
        let got = messages("needs:\n  - protos\n", Role::Workflow("build".into()));
        assert_eq!(got.len(), 1);
        assert!(got[0].contains("No sub-workspace"), "{}", got[0]);
        assert!(got[0].contains("Did you mean `proto`?"), "{}", got[0]);
    }

    #[test]
    fn a_step_needs_is_checked_against_this_file_not_the_repo() {
        let src = "steps:\n  - name: format\n    run: cargo fmt\n  - name: lint\n    needs:\n      - format\n";
        assert!(messages(src, Role::Workflow("test".into())).is_empty());

        let broken = src.replace("      - format", "      - formatt");
        let got = messages(&broken, Role::Workflow("test".into()));
        assert_eq!(got.len(), 1);
        assert!(got[0].contains("No step named"), "{}", got[0]);
    }

    #[test]
    fn a_tool_with_no_toolchain_entry_is_flagged() {
        let got = messages("requires:\n  - protoc\n", Role::Workflow("build".into()));
        assert_eq!(got.len(), 1);
        assert!(got[0].contains("toolchain:"), "{}", got[0]);
    }

    #[test]
    fn depends_on_is_checked_in_the_config_file() {
        let got = messages(
            "workspace:\n  name: api\n  depends_on:\n    - nope\n",
            Role::Config,
        );
        assert_eq!(got.len(), 1);
    }

    #[test]
    fn an_unloadable_workspace_reports_nothing_rather_than_everything() {
        let lines = vec!["needs:", "  - anything"];
        let empty = Index::default();
        assert!(
            check(&lines, &Role::Workflow("build".into()), &empty)
                .as_array()
                .unwrap()
                .is_empty()
        );
    }
}
