//! Selecting a subset of a graph to run.
//!
//! A monorepo graph is the whole truth — every step, in every package, in
//! dependency order. That's the right default and the wrong thing to sit
//! through when you're debugging one package's tests. `--filter` narrows the
//! graph to the nodes you care about:
//!
//! ```text
//! ciabatta run build --filter workspace:api
//! ciabatta run test  --filter tag:fast --filter !tag:flaky
//! ciabatta run release --filter kind:push
//! ```
//!
//! Filtering **prunes**, it does not select-and-expand: a filtered run executes
//! exactly the nodes that matched, on the assumption that whatever they used to
//! depend on is already built. That is the whole point — it's the fast loop you
//! reach for when the slow, complete graph has already run once. Use
//! `--filter` with no `--isolated` on a cold tree at your own risk; the pruned
//! edges are reported so it's never a silent surprise.

use std::collections::HashSet;

use anyhow::{Result, bail};

use super::RunStep;

/// One `--filter` term: a field to look in, the text to look for, and whether
/// a match includes or excludes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Filter {
    field: Field,
    value: String,
    /// `!tag:flaky` — matching nodes are removed rather than kept.
    negated: bool,
}

/// Which part of a step a filter term looks at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Field {
    /// `tag:fast` — the step's own tags plus the ones it inherited.
    Tag,
    /// `workspace:api` (alias `member:`) — which sub-workspace it came from.
    Workspace,
    /// `kind:push` — the special phase a step declares.
    Kind,
    /// `owner:ada` — who to ask about it.
    Owner,
    /// `step:compile` — the node name.
    Step,
    /// A bare word: anything above, plus the description.
    Any,
}

impl Filter {
    /// Parse one `--filter` term.
    ///
    /// `[!]<field>:<value>`, or a bare `[!]<value>` that searches everything.
    /// An unknown prefix is treated as a bare term rather than an error —
    /// `--filter build:latest` should search for that text, not complain about
    /// a "build" field that was never meant as one.
    pub fn parse(raw: &str) -> Result<Self> {
        let (negated, rest) = match raw.trim().strip_prefix('!') {
            Some(rest) => (true, rest.trim()),
            None => (false, raw.trim()),
        };
        if rest.is_empty() {
            bail!(
                "Empty --filter. Use tag:<name>, workspace:<name>, kind:<name>, \
                 owner:<name>, step:<name>, or a bare word to search all of them."
            );
        }

        let (field, value) = match rest.split_once(':') {
            Some((prefix, value)) if !value.trim().is_empty() => {
                let field = match prefix.trim().to_ascii_lowercase().as_str() {
                    "tag" | "tags" => Some(Field::Tag),
                    "workspace" | "member" | "package" | "pkg" => Some(Field::Workspace),
                    "kind" | "phase" => Some(Field::Kind),
                    "owner" => Some(Field::Owner),
                    "step" | "name" => Some(Field::Step),
                    _ => None,
                };
                match field {
                    Some(field) => (field, value.trim()),
                    None => (Field::Any, rest),
                }
            }
            _ => (Field::Any, rest),
        };

        Ok(Filter {
            field,
            value: value.to_ascii_lowercase(),
            negated,
        })
    }

    /// Whether this term matches a step, ignoring its sign.
    fn matches(&self, step: &RunStep) -> bool {
        let has = |text: Option<&str>| text.is_some_and(|t| contains(t, &self.value));
        let tagged = || step.tags.iter().any(|t| contains(t, &self.value));

        match self.field {
            Field::Tag => tagged(),
            Field::Workspace => has(step.workspace.as_deref()),
            Field::Kind => has(step.kind.as_deref()),
            Field::Owner => has(step.owner.as_deref()),
            Field::Step => contains(&step.name, &self.value),
            Field::Any => {
                tagged()
                    || contains(&step.name, &self.value)
                    || has(step.workspace.as_deref())
                    || has(step.kind.as_deref())
                    || has(step.owner.as_deref())
                    || has(step.description.as_deref())
            }
        }
    }
}

fn contains(haystack: &str, needle_lower: &str) -> bool {
    haystack.to_ascii_lowercase().contains(needle_lower)
}

/// What a filter did, so the operator can be told rather than left guessing why
/// two thirds of the graph didn't run.
#[derive(Debug, Default, Clone)]
pub struct Outcome {
    /// Nodes the filter removed.
    pub dropped: Vec<String>,
    /// `needs` edges cut because their target was dropped, as
    /// `(step, dependency)`. These are the steps now running without something
    /// that used to come first.
    pub cut_edges: Vec<(String, String)>,
}

impl Outcome {
    /// A short report of what was pruned, or `None` when nothing was.
    pub fn report(&self) -> Option<String> {
        if self.dropped.is_empty() {
            return None;
        }
        let mut out = format!(
            "Filtered out {} step{}.",
            self.dropped.len(),
            if self.dropped.len() == 1 { "" } else { "s" }
        );
        if !self.cut_edges.is_empty() {
            let one = self.cut_edges.len() == 1;
            out.push_str(&format!(
                "\n{} dependenc{} {} cut — {} now assume{} the inputs already exist:",
                self.cut_edges.len(),
                if one { "y" } else { "ies" },
                if one { "was" } else { "were" },
                if one { "this step" } else { "these steps" },
                if one { "s" } else { "" },
            ));
            for (step, dep) in &self.cut_edges {
                out.push_str(&format!("\n  {step} no longer waits for {dep}"));
            }
        }
        Some(out)
    }
}

/// Parse every `--filter` flag given on the command line.
pub fn parse_all(raw: &[String]) -> Result<Vec<Filter>> {
    raw.iter().map(|f| Filter::parse(f)).collect()
}

/// Narrow `steps` to the ones the filters select.
///
/// Positive terms are OR'd — `--filter tag:fast --filter tag:smoke` runs both
/// families — because a filter list reads as "the things I want". Negative
/// terms then subtract, so `!tag:flaky` wins over any positive match. With only
/// negative terms everything survives except what they exclude, which is what
/// "run it all, minus the flaky ones" should mean.
///
/// The recovery nodes reachable from a surviving step come along regardless:
/// they aren't part of the success graph, they're what happens when it breaks,
/// and losing them to a filter would quietly disarm a step's error handling.
pub fn apply(steps: &[RunStep], filters: &[Filter]) -> Result<(Vec<RunStep>, Outcome)> {
    if filters.is_empty() {
        return Ok((steps.to_vec(), Outcome::default()));
    }

    let (positive, negative): (Vec<&Filter>, Vec<&Filter>) =
        filters.iter().partition(|f| !f.negated);

    let selected = |step: &RunStep| -> bool {
        if negative.iter().any(|f| f.matches(step)) {
            return false;
        }
        positive.is_empty() || positive.iter().any(|f| f.matches(step))
    };

    // Recovery nodes are reached through `on_error`/`retry`, never the success
    // DAG, so they're kept by association with a surviving step instead of on
    // their own merits.
    let mut keep: HashSet<&str> = steps
        .iter()
        .filter(|s| !s.recover && selected(s))
        .map(|s| s.name.as_str())
        .collect();
    if keep.is_empty() {
        bail!("{}", nothing_matched(steps, filters));
    }
    keep_recovery_nodes(steps, &mut keep);

    let mut outcome = Outcome::default();
    let mut kept: Vec<RunStep> = Vec::with_capacity(keep.len());
    for step in steps {
        if !keep.contains(step.name.as_str()) {
            outcome.dropped.push(step.name.clone());
            continue;
        }
        let mut step = step.clone();
        // An edge to a pruned node can't be waited on, so it's cut — and
        // reported, because "this step ran without its input" is exactly the
        // kind of thing that should never be silent.
        step.needs.retain(|dep| {
            let alive = keep.contains(dep.as_str());
            if !alive {
                outcome.cut_edges.push((step.name.clone(), dep.clone()));
            }
            alive
        });
        kept.push(step);
    }

    Ok((kept, outcome))
}

/// Walk `on_error` / `retry` edges out of the kept set, adding the recovery
/// nodes they land on until the set stops growing (a fix can route to another).
fn keep_recovery_nodes<'a>(steps: &'a [RunStep], keep: &mut HashSet<&'a str>) {
    loop {
        let reachable: Vec<&str> = steps
            .iter()
            .filter(|s| keep.contains(s.name.as_str()))
            .flat_map(|s| [s.on_error.as_deref(), s.retry.as_deref()])
            .flatten()
            .filter(|target| !keep.contains(target))
            .collect();
        if reachable.is_empty() {
            return;
        }
        for target in reachable {
            // Re-borrow from `steps` so the kept set stays tied to its lifetime
            // rather than the iterator's.
            if let Some(step) = steps.iter().find(|s| s.name == target) {
                keep.insert(step.name.as_str());
            }
        }
    }
}

/// The error for a filter that selected nothing, listing what it could have
/// matched instead — a filter typo should end in the answer, not a shrug.
fn nothing_matched(steps: &[RunStep], filters: &[Filter]) -> String {
    let terms: Vec<String> = filters
        .iter()
        .map(|f| {
            let sign = if f.negated { "!" } else { "" };
            match f.field {
                Field::Any => format!("{sign}{}", f.value),
                Field::Tag => format!("{sign}tag:{}", f.value),
                Field::Workspace => format!("{sign}workspace:{}", f.value),
                Field::Kind => format!("{sign}kind:{}", f.value),
                Field::Owner => format!("{sign}owner:{}", f.value),
                Field::Step => format!("{sign}step:{}", f.value),
            }
        })
        .collect();

    let mut out = format!("No step matches --filter {}.", terms.join(" "));
    for (label, values) in [
        ("Tags", collect(steps, |s| s.tags.clone())),
        (
            "Sub-workspaces",
            collect(steps, |s| s.workspace.clone().into_iter().collect()),
        ),
        (
            "Kinds",
            collect(steps, |s| s.kind.clone().into_iter().collect()),
        ),
    ] {
        if !values.is_empty() {
            out.push_str(&format!("\n{label} in this graph: {}.", values.join(", ")));
        }
    }
    out
}

/// Every distinct value some accessor yields across the graph, sorted.
fn collect(steps: &[RunStep], get: impl Fn(&RunStep) -> Vec<String>) -> Vec<String> {
    let mut values: Vec<String> = steps.iter().flat_map(get).collect();
    values.sort();
    values.dedup();
    values
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A three-package graph with tags, kinds and a recovery node — enough
    /// shape for every selector to have something to bite on.
    fn graph() -> Vec<RunStep> {
        vec![
            RunStep {
                name: "proto:generate".into(),
                run: Some("protoc".into()),
                workspace: Some("proto".into()),
                tags: vec!["codegen".into()],
                owner: Some("Ada".into()),
                ..Default::default()
            },
            RunStep {
                name: "api:compile".into(),
                run: Some("cargo build".into()),
                workspace: Some("api".into()),
                needs: vec!["proto:generate".into()],
                tags: vec!["backend".into(), "slow".into()],
                on_error: Some("api:fix".into()),
                ..Default::default()
            },
            RunStep {
                name: "api:test".into(),
                run: Some("cargo test".into()),
                workspace: Some("api".into()),
                needs: vec!["api:compile".into()],
                tags: vec!["backend".into(), "fast".into()],
                ..Default::default()
            },
            RunStep {
                name: "api:fix".into(),
                recover: true,
                retry: Some("api:compile".into()),
                workspace: Some("api".into()),
                ..Default::default()
            },
            RunStep {
                name: "web:publish".into(),
                kind: Some("push".into()),
                registry: Some("nexus".into()),
                workspace: Some("web".into()),
                needs: vec!["api:compile".into()],
                tags: vec!["frontend".into()],
                ..Default::default()
            },
        ]
    }

    fn filtered(terms: &[&str]) -> (Vec<String>, Outcome) {
        let raw: Vec<String> = terms.iter().map(|t| t.to_string()).collect();
        let filters = parse_all(&raw).unwrap();
        let (steps, outcome) = apply(&graph(), &filters).unwrap();
        (steps.iter().map(|s| s.name.clone()).collect(), outcome)
    }

    #[test]
    fn no_filters_is_the_whole_graph() {
        let (steps, outcome) = apply(&graph(), &[]).unwrap();
        assert_eq!(steps.len(), 5);
        assert!(outcome.report().is_none());
    }

    #[test]
    fn a_workspace_filter_keeps_only_that_package() {
        let (names, outcome) = filtered(&["workspace:api"]);
        // api's own steps, plus the recovery node one of them routes to.
        assert_eq!(names, vec!["api:compile", "api:test", "api:fix"]);
        // The edge into proto was cut, and said so.
        assert_eq!(
            outcome.cut_edges,
            vec![("api:compile".to_string(), "proto:generate".to_string())]
        );
        assert!(outcome.report().unwrap().contains("no longer waits for"));
    }

    #[test]
    fn member_and_pkg_are_the_same_selector() {
        assert_eq!(filtered(&["member:api"]).0, filtered(&["workspace:api"]).0);
        assert_eq!(filtered(&["pkg:api"]).0, filtered(&["workspace:api"]).0);
    }

    #[test]
    fn positive_terms_are_ored_together() {
        let (names, _) = filtered(&["tag:codegen", "tag:frontend"]);
        assert_eq!(names, vec!["proto:generate", "web:publish"]);
    }

    #[test]
    fn a_negated_term_subtracts_from_everything_else() {
        let (names, _) = filtered(&["!tag:slow"]);
        // Everything except api:compile — and api:fix survives on its own,
        // since a recovery node is kept when anything it serves is kept…
        assert!(!names.contains(&"api:compile".to_string()));
        assert!(names.contains(&"api:test".to_string()));
        assert!(names.contains(&"proto:generate".to_string()));
    }

    #[test]
    fn a_negated_term_beats_a_positive_one() {
        let (names, _) = filtered(&["tag:backend", "!tag:slow"]);
        assert_eq!(names, vec!["api:test"]);
    }

    #[test]
    fn kind_selects_the_push_phase() {
        let (names, _) = filtered(&["kind:push"]);
        assert_eq!(names, vec!["web:publish"]);
    }

    #[test]
    fn a_bare_term_searches_every_field() {
        assert_eq!(filtered(&["ada"]).0, vec!["proto:generate"]);
        // Node names too, which is the one-step-only case.
        assert_eq!(filtered(&["api:test"]).0, vec!["api:test"]);
    }

    #[test]
    fn matching_is_case_insensitive() {
        assert_eq!(filtered(&["TAG:Backend"]).0, filtered(&["tag:backend"]).0);
    }

    #[test]
    fn a_recovery_node_comes_along_with_the_step_it_serves() {
        let (names, _) = filtered(&["step:api:compile"]);
        assert_eq!(names, vec!["api:compile", "api:fix"]);
    }

    #[test]
    fn a_filter_matching_nothing_lists_what_was_available() {
        let raw = vec!["tag:nope".to_string()];
        let err = apply(&graph(), &parse_all(&raw).unwrap())
            .unwrap_err()
            .to_string();
        assert!(err.contains("No step matches --filter tag:nope"), "{err}");
        assert!(err.contains("backend"), "{err}");
        assert!(
            err.contains("Sub-workspaces in this graph: api, proto, web"),
            "{err}"
        );
    }

    #[test]
    fn an_unknown_prefix_is_searched_as_text_not_rejected() {
        // "bundle:x" isn't a field; it should be looked for, not complained about.
        let filter = Filter::parse("nosuchfield:value").unwrap();
        assert_eq!(filter.field, Field::Any);
        assert_eq!(filter.value, "nosuchfield:value");
    }

    #[test]
    fn an_empty_filter_is_refused() {
        assert!(Filter::parse("  ").is_err());
        assert!(Filter::parse("!").is_err());
    }
}
