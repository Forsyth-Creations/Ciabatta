//! Finding the dangling parts of a workspace: workflows nothing connects to,
//! and steps nothing in their workflow orders.
//!
//! `ciabatta self graph` prints this, and the web app's whole-workspace graph
//! works the same thing out live, as the user edits — the rules here and in
//! `tool_frontend/src/components/dangling.ts` are the same rules and have to
//! stay that way.
//!
//! A workflow **dangles** when nothing reaches it and it reaches nothing: it
//! names no other workflow in `needs`, `background` or `after` (and its
//! sub-workspace's `depends_on` brings none in), no other workflow names it,
//! and no other sub-workspace has a workflow of the same name — which would
//! run alongside it under `ciabatta <name>`.
//!
//! A step is **loose** when its workflow orders some of its steps but not
//! that one: it needs nothing, nothing needs it, and it isn't a recovery step,
//! a background step, or the target or source of an `on_error` / `retry`
//! route. A workflow whose steps are all independent is left alone — running
//! everything at once is a choice, not an oversight.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

use super::edit::DeclaredUnit;

/// What dangles in a workspace.
#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub struct Report {
    /// How many workflows were looked at.
    pub workflows: usize,
    /// `<member>:<workflow>`s nothing connects to.
    pub dangling: Vec<String>,
    /// Loose steps, by workflow.
    pub loose_steps: BTreeMap<String, Vec<String>>,
}

impl Report {
    pub fn is_clean(&self) -> bool {
        self.dangling.is_empty() && self.loose_steps.is_empty()
    }
}

/// `need`, resolved to the `<member>:<workflow>` it means from `member`'s
/// workflow called `workflow`: a bare member names its workflow of the same
/// name, and `self` names `member`.
pub fn resolve(need: &str, member: &str, workflow: &str) -> String {
    let (who, what) = match need.split_once(':') {
        Some((who, what)) => (who.trim(), what.trim()),
        None => (need.trim(), workflow),
    };
    let who = if who == "self" { member } else { who };
    format!("{who}:{what}")
}

/// Every workflow `unit` names, that exists.
fn links_out(unit: &DeclaredUnit, known: &BTreeSet<String>) -> BTreeSet<String> {
    let own = format!("{}:{}", unit.member, unit.workflow);
    unit.needs
        .iter()
        .chain(&unit.depends_on)
        .chain(&unit.background)
        .chain(&unit.after)
        .map(|n| resolve(n, &unit.member, &unit.workflow))
        .filter(|t| known.contains(t) && *t != own)
        .collect()
}

pub fn find(units: &[DeclaredUnit]) -> Report {
    let known: BTreeSet<String> = units
        .iter()
        .map(|u| format!("{}:{}", u.member, u.workflow))
        .collect();
    let outs: Vec<BTreeSet<String>> = units.iter().map(|u| links_out(u, &known)).collect();
    let reached: BTreeSet<&String> = outs.iter().flatten().collect();

    let mut report = Report {
        workflows: units.len(),
        ..Report::default()
    };

    for (unit, out) in units.iter().zip(&outs) {
        let key = format!("{}:{}", unit.member, unit.workflow);
        let shares_name = units
            .iter()
            .any(|o| o.workflow == unit.workflow && o.member != unit.member);
        if out.is_empty() && !reached.contains(&key) && !shares_name {
            report.dangling.push(key.clone());
        }

        let loose = loose_steps(unit);
        if !loose.is_empty() {
            report.loose_steps.insert(key, loose);
        }
    }
    report
}

/// The steps of `unit` nothing in it orders, when it orders any.
pub fn loose_steps(unit: &DeclaredUnit) -> Vec<String> {
    if !unit.steps.iter().any(|s| !s.needs.is_empty()) {
        return Vec::new();
    }
    let needed: BTreeSet<&str> = unit
        .steps
        .iter()
        .flat_map(|s| s.needs.iter().map(String::as_str))
        .collect();
    let routed: BTreeSet<&str> = unit
        .steps
        .iter()
        .flat_map(|s| s.routes.iter().map(String::as_str))
        .collect();
    unit.steps
        .iter()
        .filter(|s| {
            s.needs.is_empty()
                && s.routes.is_empty()
                && !s.recover
                && !s.background
                && !needed.contains(s.name.as_str())
                && !routed.contains(s.name.as_str())
        })
        .map(|s| s.name.clone())
        .collect()
}

/// The report as `ciabatta self graph` prints it.
pub fn render(report: &Report) -> String {
    let mut out = String::new();
    if report.is_clean() {
        out.push_str(&format!(
            "Nothing dangles: every one of the {} workflow{} is connected.\n",
            report.workflows,
            if report.workflows == 1 { "" } else { "s" }
        ));
        return out;
    }
    if !report.dangling.is_empty() {
        out.push_str(&format!(
            "{} of {} workflows dangle — nothing needs them, they need nothing, \
             and no other package has a workflow of that name:\n",
            report.dangling.len(),
            report.workflows
        ));
        for unit in &report.dangling {
            out.push_str(&format!("  {unit}\n"));
        }
    }
    if !report.loose_steps.is_empty() {
        out.push_str("Loose steps — their workflow orders its other steps, but not these:\n");
        for (unit, steps) in &report.loose_steps {
            out.push_str(&format!("  {unit}: {}\n", steps.join(", ")));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::edit::DeclaredStep;

    fn step(name: &str, needs: &[&str]) -> DeclaredStep {
        DeclaredStep {
            name: name.into(),
            needs: needs.iter().map(|s| s.to_string()).collect(),
            routes: vec![],
            recover: false,
            background: false,
        }
    }

    fn unit(
        member: &str,
        workflow: &str,
        needs: &[&str],
        steps: Vec<DeclaredStep>,
    ) -> DeclaredUnit {
        DeclaredUnit {
            member: member.into(),
            workflow: workflow.into(),
            file: None,
            locked: None,
            needs: needs.iter().map(|s| s.to_string()).collect(),
            depends_on: vec![],
            background: vec![],
            after: vec![],
            steps,
        }
    }

    #[test]
    fn an_unconnected_uniquely_named_workflow_dangles() {
        let units = vec![
            unit("api", "build", &["proto:generate"], vec![step("a", &[])]),
            unit("proto", "generate", &[], vec![step("a", &[])]),
            unit("api", "old-deploy", &[], vec![step("a", &[])]),
        ];
        let report = find(&units);
        assert_eq!(report.dangling, vec!["api:old-deploy".to_string()]);
        assert!(report.loose_steps.is_empty());
    }

    #[test]
    fn sharing_a_name_or_any_link_keeps_a_workflow_connected() {
        let mut after = unit("api", "test", &[], vec![step("a", &[])]);
        after.after = vec!["db:teardown".into()];
        let mut bg = unit("web", "e2e", &[], vec![step("a", &[])]);
        bg.background = vec!["mock".into()]; // bare member: mock:e2e
        let mut dep = unit("web", "lint", &[], vec![step("a", &[])]);
        dep.depends_on = vec!["api".into()]; // api:lint
        let units = vec![
            after,
            unit("db", "teardown", &[], vec![step("a", &[])]),
            bg,
            unit("mock", "e2e", &[], vec![step("a", &[])]),
            dep,
            unit("api", "lint", &[], vec![step("a", &[])]),
            // Shares its name with nothing linked, but with another member.
            unit("x", "fmt", &[], vec![step("a", &[])]),
            unit("y", "fmt", &[], vec![step("a", &[])]),
        ];
        assert_eq!(find(&units).dangling, Vec::<String>::new());
    }

    #[test]
    fn self_names_the_workflows_own_package() {
        let units = vec![
            unit("api", "test", &["self:build"], vec![step("a", &[])]),
            unit("api", "build", &[], vec![step("a", &[])]),
        ];
        assert_eq!(find(&units).dangling, Vec::<String>::new());
        assert_eq!(resolve("self:build", "api", "test"), "api:build");
        assert_eq!(resolve("proto", "api", "test"), "proto:test");
    }

    #[test]
    fn a_need_on_a_missing_workflow_is_no_connection() {
        let units = vec![unit("api", "build", &["gone:build"], vec![step("a", &[])])];
        assert_eq!(find(&units).dangling, vec!["api:build".to_string()]);
    }

    #[test]
    fn loose_steps_only_count_in_a_workflow_that_orders_others() {
        let mut recover = step("rollback", &[]);
        recover.recover = true;
        let mut routing = step("deploy", &["build"]);
        routing.routes = vec!["rollback".into()];
        let ordered = unit(
            "api",
            "ship",
            &[],
            vec![step("build", &[]), routing, recover, step("stray", &[])],
        );
        assert_eq!(loose_steps(&ordered), vec!["stray".to_string()]);

        let parallel = unit(
            "api",
            "check",
            &[],
            vec![step("lint", &[]), step("test", &[])],
        );
        assert!(loose_steps(&parallel).is_empty());
    }

    #[test]
    fn render_names_everything_it_found() {
        let units = vec![unit(
            "api",
            "old",
            &[],
            vec![step("a", &[]), step("b", &["a"]), step("c", &[])],
        )];
        let text = render(&find(&units));
        assert!(text.contains("api:old"), "{text}");
        assert!(text.contains("api:old: c"), "{text}");
        assert!(render(&Report::default()).contains("Nothing dangles"));
    }
}
