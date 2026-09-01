//! Compiling a named workflow into one cross-workspace graph.
//!
//! `ciabatta build` doesn't run "the build in this directory" — it collects
//! every `build` workflow in the monorepo, follows each one's declared
//! dependencies on other sub-workspaces, and produces a single graph that says
//! exactly what runs, in what order, and where each node came from.
//!
//! The compiled result is an ordinary [`ResolvedRun`], so the existing engine
//! executes it and the existing live view draws it — a workflow graph is just a
//! very well-labelled step DAG.

use std::collections::{BTreeMap, HashMap, HashSet};

use anyhow::{Result, bail};

use crate::run::{RunStep, validate_flowchart};

use super::{Member, Workflow, Workspace};

/// One (sub-workspace, workflow) pair pulled into the graph.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct UnitId {
    /// The sub-workspace's name.
    pub member: String,
    /// The workflow's name within it.
    pub workflow: String,
}

impl std::fmt::Display for UnitId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}", self.member, self.workflow)
    }
}

/// A compiled workflow graph: the steps to run, plus enough provenance to
/// explain each one.
#[derive(Debug, Clone, Default)]
pub struct WorkflowGraph {
    /// The workflow name(s) that were asked for (`build`, or `build` and
    /// `test` together). More than one compiles into a single graph rather
    /// than a sequence of them, so a step shared by both runs once.
    pub workflows: Vec<String>,
    /// Every (member, workflow) unit the closure pulled in, in dependency
    /// order.
    pub units: Vec<UnitId>,
    /// The compiled steps, with graph-wide unique names and cross-unit edges
    /// already wired in.
    pub steps: Vec<RunStep>,
    /// `.env` files to source, as paths relative to the monorepo root.
    pub env_files: Vec<String>,
    /// Variables every contributing workflow insists on.
    pub required_env: Vec<String>,
}

impl WorkflowGraph {
    /// Whether the graph has anything to run.
    pub fn is_empty(&self) -> bool {
        self.steps.is_empty()
    }

    /// What to call this graph in output: the workflow, or all of them.
    pub fn label(&self) -> String {
        self.workflows.join(" + ")
    }

    /// The graph in dependency layers: everything in wave *n* can run once
    /// wave *n-1* is done. This is the shape the rendered graph is drawn in,
    /// and it's what makes the ordering legible at a glance.
    ///
    /// Recovery nodes belong to the step that routes to them rather than to a
    /// wave of their own, so they're left out here.
    pub fn waves(&self) -> Vec<Vec<&RunStep>> {
        let mut placed: HashMap<&str, usize> = HashMap::new();
        let mut waves: Vec<Vec<&RunStep>> = Vec::new();
        // Background tasks are in the graph but not in the order: they are
        // started before the first wave and nothing waits for them, so putting
        // one in a wave would claim the next wave waits for it.
        let normal: Vec<&RunStep> = self
            .steps
            .iter()
            .filter(|s| !s.recover && !s.background)
            .collect();

        let mut remaining: Vec<&RunStep> = normal.clone();
        while !remaining.is_empty() {
            // Everything whose dependencies have already been placed forms the
            // next wave.
            let (ready, rest): (Vec<&RunStep>, Vec<&RunStep>) = remaining
                .iter()
                .partition(|s| s.needs.iter().all(|d| placed.contains_key(d.as_str())));

            if ready.is_empty() {
                // Can only happen if validation was skipped; emit the rest as a
                // final wave rather than looping forever.
                waves.push(rest);
                break;
            }
            let index = waves.len();
            for step in &ready {
                placed.insert(step.name.as_str(), index);
            }
            waves.push(ready);
            remaining = rest;
        }
        waves
    }

    /// The steps contributed by one sub-workspace.
    pub fn steps_of<'a>(&'a self, member: &str) -> Vec<&'a RunStep> {
        self.steps
            .iter()
            .filter(|s| s.workspace.as_deref() == Some(member))
            .collect()
    }
}

/// Compile a workflow graph for the monorepo containing `start`.
///
/// The one call every caller needs: discover the workspace, build the graph,
/// and hand back both — the workspace because callers want to render tool
/// hints and provenance from it.
pub fn prepare(
    start: &std::path::Path,
    workflow: &str,
    selection: &Selection,
) -> Result<(Workspace, WorkflowGraph)> {
    let workspace = Workspace::discover(start)?;
    let graph = build(&workspace, workflow, selection)?;
    Ok((workspace, graph))
}

/// [`prepare`] for several workflows at once, compiled into one graph.
pub fn prepare_many(
    start: &std::path::Path,
    workflows: &[String],
    selection: &Selection,
) -> Result<(Workspace, WorkflowGraph)> {
    let workspace = Workspace::discover(start)?;
    let graph = build_many(&workspace, workflows, selection)?;
    Ok((workspace, graph))
}

/// Turn a compiled graph into the run the engine executes.
///
/// This is what lets a monorepo workflow reuse everything a run already has:
/// the same validation, the same terminal UI, the same live web view, the same
/// daemon-owned execution. A workflow *is* a run — one whose steps happened to
/// come from six different packages.
pub fn into_run(graph: WorkflowGraph) -> Result<crate::run::ResolvedRun> {
    let label = graph.label();
    let mut steps = graph.steps;
    resolve_transfer_refs(&mut steps)?;
    crate::run::validate_flowchart(&steps, &label)?;
    // The units come along: once the graph is flattened into steps, which
    // package's `build` a given step came from is recoverable but which
    // *workflows took part* is not, and that is what a run has to write down.
    let units = graph
        .units
        .iter()
        .map(|unit| crate::run::Unit {
            workspace: unit.member.clone(),
            workflow: unit.workflow.clone(),
        })
        .collect();
    Ok(crate::run::ResolvedRun {
        login: None,
        pre: None,
        post: None,
        required_env: graph.required_env,
        env_files: graph.env_files,
        steps: crate::run::topo_order(&steps),
        units,
    })
}

/// Resolve every `from:` back-reference, so a transfer step is self-contained
/// by the time anything executes it.
///
/// Push and pull are the same artifact in opposite directions, and a pull that
/// restates the registry and path is a pull that will eventually disagree with
/// the push. `from` says "the other end of this one" once.
fn resolve_transfer_refs(steps: &mut [RunStep]) -> Result<()> {
    // Collected up front: a step may reference one declared after it, and the
    // graph's order is dependency order, not declaration order.
    let sources: HashMap<String, RunStep> = steps
        .iter()
        .map(|step| (step.name.clone(), step.clone()))
        .collect();

    for step in steps.iter_mut() {
        let Some(reference) = step.from.clone() else {
            continue;
        };
        if step.direction().is_none() {
            bail!(
                "Step '{}' sets `from: {reference}` but has no transfer `kind`.                  Add `kind: pull` (or `kind: push`), or drop `from`.",
                step.name
            );
        }
        // A compiled graph names steps `<member>:<workflow>:<step>`, but a
        // workflow writes `from:` in its own terms — a sibling step, or
        // `<workflow>:<step>` for one next door. Match on the suffix so both
        // spellings land on the same node.
        let source = sources
            .get(&reference)
            .or_else(|| {
                let suffix = format!(":{reference}");
                let mut hits = sources
                    .iter()
                    .filter(|(name, _)| name.ends_with(&suffix))
                    .map(|(_, step)| step);
                let first = hits.next();
                // Ambiguous suffix → no guess. Better to ask for the full name
                // than to publish to the wrong place.
                match hits.next() {
                    Some(_) => None,
                    None => first,
                }
            })
            .ok_or_else(|| {
                let mut available: Vec<&String> = sources
                    .keys()
                    .filter(|name| {
                        sources[*name].direction() == Some(crate::run::Direction::Push)
                    })
                    .collect();
                available.sort();
                anyhow::anyhow!(
                    "Step '{}' sets `from: {reference}`, which names no step in this graph.                      Push steps available: {}.",
                    step.name,
                    if available.is_empty() {
                        "(none)".to_string()
                    } else {
                        available
                            .iter()
                            .map(|s| s.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    }
                )
            })?
            .clone();

        if source.name == step.name {
            bail!("Step '{}' sets `from` to itself.", step.name);
        }
        step.inherit_transfer(&source);
    }
    Ok(())
}

/// Which sub-workspaces to start from.
#[derive(Debug, Clone, Default)]
pub struct Selection {
    /// Only these members' workflows seed the graph (their dependencies are
    /// still pulled in). Empty means every member that defines the workflow.
    pub only: Vec<String>,
    /// Don't follow dependencies into other sub-workspaces — run just what was
    /// selected, for when you know the rest is already built.
    pub isolated: bool,
}

/// Compile `workflow` across the monorepo into one graph.
///
/// Starts from every member that defines the workflow (or just `selection.only`
/// when given), pulls in each one's declared dependencies transitively, orders
/// the result, and validates it.
pub fn build(
    workspace: &Workspace,
    workflow: &str,
    selection: &Selection,
) -> Result<WorkflowGraph> {
    build_many(workspace, &[workflow.to_string()], selection)
}

/// Compile several workflows across the monorepo into one graph.
///
/// `ciabatta run build test` is not "build, then test" — it's one graph
/// containing both, so a package's build still runs before anything that
/// depends on it and nothing gets executed twice for being wanted twice.
pub fn build_many(
    workspace: &Workspace,
    workflows: &[String],
    selection: &Selection,
) -> Result<WorkflowGraph> {
    if workflows.is_empty() {
        bail!("No workflow named. Run `ciabatta list` to see what this workspace defines.");
    }

    let mut seeds: Vec<UnitId> = Vec::new();
    for workflow in workflows {
        for seed in seed_units(workspace, workflow, selection)? {
            push_unique(&mut seeds, seed);
        }
    }

    // Resolve the transitive closure, recording each unit's direct dependencies
    // as we go so the edge wiring below doesn't have to re-derive them.
    let mut deps: BTreeMap<UnitId, Vec<UnitId>> = BTreeMap::new();
    let mut queue: Vec<UnitId> = seeds.clone();
    while let Some(unit) = queue.pop() {
        if deps.contains_key(&unit) {
            continue;
        }
        let direct = if selection.isolated {
            Vec::new()
        } else {
            unit_dependencies(workspace, &unit)?
        };
        for dep in &direct {
            if !deps.contains_key(dep) {
                queue.push(dep.clone());
            }
        }
        deps.insert(unit, direct);
    }

    let order = topological_order(&deps)?;
    compile(workspace, workflows, &order, &deps)
}

/// The units the graph starts from, honouring `--only` and reporting clearly
/// when nothing matches.
fn seed_units(workspace: &Workspace, workflow: &str, selection: &Selection) -> Result<Vec<UnitId>> {
    if selection.only.is_empty() {
        let seeds: Vec<UnitId> = workspace
            .providers(workflow)
            .into_iter()
            .map(|m| UnitId {
                member: m.name.clone(),
                workflow: workflow.to_string(),
            })
            .collect();
        if seeds.is_empty() {
            let available = workspace.workflow_names();
            bail!(
                "No sub-workspace defines a '{}' workflow.{}",
                workflow,
                if available.is_empty() {
                    "\nNo workflows are defined anywhere yet — run `ciabatta init --lib` in a package to add one.".to_string()
                } else {
                    format!("\nAvailable workflows: {}.", available.join(", "))
                }
            );
        }
        return Ok(seeds);
    }

    let mut seeds = Vec::new();
    for name in &selection.only {
        let member = workspace.member(name).ok_or_else(|| {
            let mut names: Vec<&str> = workspace.members.iter().map(|m| m.name.as_str()).collect();
            names.sort();
            anyhow::anyhow!(
                "No sub-workspace named '{}'. Known sub-workspaces: {}.",
                name,
                if names.is_empty() {
                    "(none)".to_string()
                } else {
                    names.join(", ")
                }
            )
        })?;
        if !member.has_workflow(workflow) {
            let mut have: Vec<&str> = member.workflows.keys().map(|s| s.as_str()).collect();
            have.sort();
            bail!(
                "Sub-workspace '{}' has no '{}' workflow. It defines: {}.",
                name,
                workflow,
                if have.is_empty() {
                    "(none)".to_string()
                } else {
                    have.join(", ")
                }
            );
        }
        seeds.push(UnitId {
            member: member.name.clone(),
            workflow: workflow.to_string(),
        });
    }
    Ok(seeds)
}

/// Look up a unit's member and workflow together, with a message that names
/// both when either is missing.
fn resolve<'a>(workspace: &'a Workspace, unit: &UnitId) -> Result<(&'a Member, &'a Workflow)> {
    let member = workspace
        .member(&unit.member)
        .ok_or_else(|| anyhow::anyhow!("No sub-workspace named '{}'.", unit.member))?;
    let workflow = member.workflows.get(&unit.workflow).ok_or_else(|| {
        anyhow::anyhow!(
            "Sub-workspace '{}' has no '{}' workflow.",
            unit.member,
            unit.workflow
        )
    })?;
    Ok((member, workflow))
}

/// The units a given unit depends on: its `[workspace] depends_on` (which
/// applies to every workflow the member defines) plus the workflow's own
/// `needs`.
fn unit_dependencies(workspace: &Workspace, unit: &UnitId) -> Result<Vec<UnitId>> {
    let (member, workflow) = resolve(workspace, unit)?;
    let mut out: Vec<UnitId> = Vec::new();

    // Sub-workspace-level dependencies are the "we always need these first"
    // declaration; a member that doesn't happen to define this workflow simply
    // has nothing to contribute to it.
    for spec in &member.meta.depends_on {
        if let Some(dep) = parse_dependency(workspace, unit, spec, false)? {
            push_unique(&mut out, dep);
        }
    }
    // Workflow-level `needs` name a specific thing to run, so a missing target
    // is a mistake worth reporting rather than a silent no-op.
    for spec in &workflow.needs {
        if let Some(dep) = parse_dependency(workspace, unit, spec, true)? {
            push_unique(&mut out, dep);
        }
    }

    if out.iter().any(|d| d == unit) {
        bail!(
            "Sub-workspace '{}' declares a dependency on its own '{}' workflow.",
            unit.member,
            unit.workflow
        );
    }
    Ok(out)
}

/// Parse one dependency spec.
///
/// * `"proto"` — that sub-workspace's workflow of the *same name* as the one
///   being run. Missing is fine: it just has no part in this workflow.
/// * `"proto:generate"` — one specific workflow. Missing is an error.
/// * `"self:generate"` — another workflow in the same sub-workspace.
///
/// `strict` controls whether an absent same-name workflow is an error, which is
/// what separates workflow `needs` from blanket `depends_on`.
fn parse_dependency(
    workspace: &Workspace,
    from: &UnitId,
    spec: &str,
    strict: bool,
) -> Result<Option<UnitId>> {
    let spec = spec.trim();
    if spec.is_empty() {
        bail!(
            "Sub-workspace '{}' has an empty dependency entry.",
            from.member
        );
    }

    let (member_name, workflow_name) = match spec.split_once(':') {
        Some((m, w)) => {
            let member = if m.trim() == "self" {
                from.member.as_str()
            } else {
                m.trim()
            };
            (member, w.trim().to_string())
        }
        None => (spec, from.workflow.clone()),
    };

    let Some(member) = workspace.member(member_name) else {
        let mut names: Vec<&str> = workspace.members.iter().map(|m| m.name.as_str()).collect();
        names.sort();
        bail!(
            "Sub-workspace '{}' depends on '{}', but no sub-workspace is named '{}'.\n\
             Known sub-workspaces: {}.",
            from.member,
            spec,
            member_name,
            if names.is_empty() {
                "(none)".to_string()
            } else {
                names.join(", ")
            }
        );
    };

    if !member.has_workflow(&workflow_name) {
        // An explicit `member:workflow` that doesn't exist is always a mistake;
        // a bare `member` that has no workflow of this name is just not
        // involved in it.
        if strict || spec.contains(':') {
            let mut have: Vec<&str> = member.workflows.keys().map(|s| s.as_str()).collect();
            have.sort();
            bail!(
                "Sub-workspace '{}' depends on '{}', but '{}' defines no '{}' workflow. It defines: {}.",
                from.member,
                spec,
                member.name,
                workflow_name,
                if have.is_empty() {
                    "(none)".to_string()
                } else {
                    have.join(", ")
                }
            );
        }
        return Ok(None);
    }

    Ok(Some(UnitId {
        member: member.name.clone(),
        workflow: workflow_name,
    }))
}

fn push_unique(out: &mut Vec<UnitId>, unit: UnitId) {
    if !out.contains(&unit) {
        out.push(unit);
    }
}

/// Order the units so dependencies always come before their dependents,
/// reporting the actual cycle when there isn't such an order.
fn topological_order(deps: &BTreeMap<UnitId, Vec<UnitId>>) -> Result<Vec<UnitId>> {
    #[derive(Clone, Copy, PartialEq)]
    enum Mark {
        Unvisited,
        InProgress,
        Done,
    }

    let mut marks: BTreeMap<&UnitId, Mark> = deps.keys().map(|u| (u, Mark::Unvisited)).collect();
    let mut order: Vec<UnitId> = Vec::with_capacity(deps.len());

    // Depth-first, emitting each unit after everything it depends on. The
    // in-progress stack doubles as the cycle to report.
    fn visit<'a>(
        unit: &'a UnitId,
        deps: &'a BTreeMap<UnitId, Vec<UnitId>>,
        marks: &mut BTreeMap<&'a UnitId, Mark>,
        path: &mut Vec<&'a UnitId>,
        order: &mut Vec<UnitId>,
    ) -> Result<()> {
        match marks.get(unit) {
            Some(Mark::Done) => return Ok(()),
            Some(Mark::InProgress) => {
                let start = path.iter().position(|u| *u == unit).unwrap_or(0);
                let cycle: Vec<String> = path[start..]
                    .iter()
                    .map(|u| u.to_string())
                    .chain(std::iter::once(unit.to_string()))
                    .collect();
                bail!(
                    "Sub-workspace dependencies form a cycle: {}.\n\
                     Break it by removing one of those depends_on / needs entries.",
                    cycle.join(" → ")
                );
            }
            _ => {}
        }

        // `deps` owns the keys, so re-borrow the stored key to get the lifetime
        // the marks map needs.
        let Some((key, edges)) = deps.get_key_value(unit) else {
            return Ok(());
        };
        marks.insert(key, Mark::InProgress);
        path.push(key);
        for dep in edges {
            visit(dep, deps, marks, path, order)?;
        }
        path.pop();
        marks.insert(key, Mark::Done);
        order.push(key.clone());
        Ok(())
    }

    for unit in deps.keys() {
        let mut path: Vec<&UnitId> = Vec::new();
        visit(unit, deps, &mut marks, &mut path, &mut order)?;
    }
    Ok(order)
}

/// Turn ordered units into steps: rename each step to a graph-wide unique id,
/// stamp it with its origin, and wire the cross-unit edges.
fn compile(
    workspace: &Workspace,
    workflow_names: &[String],
    order: &[UnitId],
    deps: &BTreeMap<UnitId, Vec<UnitId>>,
) -> Result<WorkflowGraph> {
    // A member usually contributes one workflow, and `api:compile` reads far
    // better than `api:build:compile`. Only disambiguate where it's needed.
    let mut per_member: HashMap<&str, usize> = HashMap::new();
    for unit in order {
        *per_member.entry(unit.member.as_str()).or_insert(0) += 1;
    }

    let node_id = |unit: &UnitId, step: &str| -> String {
        if per_member.get(unit.member.as_str()).copied().unwrap_or(0) > 1 {
            format!("{}:{}:{}", unit.member, unit.workflow, step)
        } else {
            format!("{}:{}", unit.member, step)
        }
    };

    let mut graph = WorkflowGraph {
        workflows: workflow_names.to_vec(),
        units: order.to_vec(),
        ..Default::default()
    };
    // The exit steps of each unit — what a dependent unit has to wait for.
    let mut exits: HashMap<&UnitId, Vec<String>> = HashMap::new();

    for unit in order {
        let (member, workflow) = resolve(workspace, unit)?;

        if workflow.steps.is_empty() {
            // Background tasks alone are not a workflow: nothing would keep the
            // run alive, so they would be started and stopped in the same
            // breath. Say that rather than the generic "no steps".
            if !workflow.background.is_empty() {
                bail!(
                    "Workflow '{}' in sub-workspace '{}' declares `background:` but no steps. \
                     Background tasks are stopped when the run ends, so a workflow of nothing \
                     but background tasks would start them and immediately stop them again. \
                     Add the step they are there to support.",
                    unit.workflow,
                    unit.member
                );
            }
            bail!(
                "Workflow '{}' in sub-workspace '{}' has no steps. Give it at least one \
                 [[steps]] entry, or remove it.",
                unit.workflow,
                unit.member
            );
        }

        // Background tasks compile into nodes with no edges in either
        // direction. Nothing waits for them (they never finish) and they wait
        // for nothing (a step that needed one to be built first would be a
        // step, not a background task) — so they are neither entries nor exits
        // of the unit, and the wave layering never sees them.
        for task in &workflow.background {
            if workflow.steps.iter().any(|s| s.name == task.name) {
                bail!(
                    "'{}' is both a step and a background task of workflow '{}' in \
                     sub-workspace '{}'. One name, one node.",
                    task.name,
                    unit.workflow,
                    unit.member
                );
            }
            if task.script.is_none() && task.run.is_none() {
                bail!(
                    "Background task '{}' of workflow '{}' in sub-workspace '{}' has nothing \
                     to run. Give it `run:` or `script:`.",
                    task.name,
                    unit.workflow,
                    unit.member
                );
            }
            if !task.needs.is_empty() {
                bail!(
                    "Background task '{}' of workflow '{}' in sub-workspace '{}' declares \
                     `needs`. A background task is started before the first wave and gates \
                     nothing, so it can neither wait nor be waited for — if it has to run in \
                     order, it is a step.",
                    task.name,
                    unit.workflow,
                    unit.member
                );
            }

            let mut compiled = task.clone();
            compiled.name = node_id(unit, &task.name);
            compiled.workspace = Some(member.name.clone());
            compiled.cwd = Some(member.rel.clone());
            compiled.background = true;
            // A background task is persistent by definition — it is what the
            // engine already knows how to start without waiting for.
            compiled.persistent = true;
            compiled.needs = Vec::new();
            compiled.recover = false;
            compiled.on_error = None;
            compiled.retry = None;

            compiled.requires = merge_unique(
                &member.meta.requires,
                &merge_unique(&workflow.requires, &task.requires),
            );
            let mut env = workspace.env.clone();
            env.extend(member.meta.env.clone());
            env.extend(workflow.env.clone());
            env.extend(task.env.clone());
            compiled.env = env;
            compiled.env_files = env_chain(workspace, member);
            for file in &workflow.env_file {
                let path = join_rel(&member.rel, file);
                if !compiled.env_files.contains(&path) {
                    compiled.env_files.push(path);
                }
            }
            compiled.tags =
                merge_unique(&member.meta.tags, &merge_unique(&workflow.tags, &task.tags));
            if compiled.owner.is_none() {
                compiled.owner = workflow.owner.clone().or_else(|| member.meta.owner.clone());
            }

            graph.steps.push(compiled);
        }

        // What this unit waits on: every dependency unit's exit steps.
        let incoming: Vec<String> = deps
            .get(unit)
            .map(|list| {
                list.iter()
                    .filter_map(|dep| exits.get(dep))
                    .flatten()
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();

        // Steps nothing else in this unit depends on are its exits; steps that
        // depend on nothing are its entries, and inherit the incoming edges.
        let depended_on: HashSet<&str> = workflow
            .steps
            .iter()
            .flat_map(|s| s.needs.iter().map(|n| n.as_str()))
            .collect();

        let member_chain = env_chain(workspace, member);

        let mut unit_exits: Vec<String> = Vec::new();
        for step in &workflow.steps {
            let mut compiled = step.clone();
            compiled.name = node_id(unit, &step.name);
            compiled.workspace = Some(member.name.clone());
            compiled.cwd = Some(member.rel.clone());

            // Rewrite in-workflow edges to the new ids, and check them here so
            // the error names the workflow the typo is actually in.
            compiled.needs = step
                .needs
                .iter()
                .map(|dep| {
                    if !workflow.steps.iter().any(|s| &s.name == dep) {
                        bail!(
                            "Step '{}' of workflow '{}' in sub-workspace '{}' needs '{}', which \
                             is not a step in that workflow.\n\
                             Depend on another sub-workspace with [workspace] depends_on or the \
                             workflow's own `needs`.",
                            step.name,
                            unit.workflow,
                            unit.member,
                            dep
                        );
                    }
                    Ok(node_id(unit, dep))
                })
                .collect::<Result<Vec<_>>>()?;
            for target in [step.on_error.as_deref(), step.retry.as_deref()]
                .into_iter()
                .flatten()
            {
                if !workflow.steps.iter().any(|s| s.name == target) {
                    bail!(
                        "Step '{}' of workflow '{}' in sub-workspace '{}' routes to '{}', which \
                         is not a step in that workflow.",
                        step.name,
                        unit.workflow,
                        unit.member,
                        target
                    );
                }
            }
            compiled.on_error = step.on_error.as_deref().map(|t| node_id(unit, t));
            compiled.retry = step.retry.as_deref().map(|t| node_id(unit, t));

            // Entry steps of the unit wait for everything the unit depends on.
            // Recovery nodes are entered through `on_error`, never the success
            // DAG, so they take no incoming edges.
            if compiled.needs.is_empty() && !compiled.recover {
                compiled.needs = incoming.clone();
            }

            // Requirements and environment cascade: sub-workspace → workflow →
            // step, with the most specific written last.
            compiled.requires = merge_unique(
                &member.meta.requires,
                &merge_unique(&workflow.requires, &step.requires),
            );
            // monorepo root → sub-workspace → workflow → step, most specific
            // written last so it wins.
            let mut env = workspace.env.clone();
            env.extend(member.meta.env.clone());
            env.extend(workflow.env.clone());
            env.extend(step.env.clone());
            compiled.env = env;

            // The `.env` files this step resolves through, outermost first: the
            // workspaces above it, then its own, then whatever the workflow
            // names. Its own answers first; anything it doesn't set falls back
            // outward. A sibling package's `.env` is never in the chain.
            compiled.env_files = member_chain.clone();
            for file in &workflow.env_file {
                let path = join_rel(&member.rel, file);
                if !compiled.env_files.contains(&path) {
                    compiled.env_files.push(path);
                }
            }

            // Tags cascade the same way, and unlike env they accumulate: a step
            // in a package tagged "backend", in a workflow tagged "slow", is
            // both — so `--filter tag:backend` finds it without every step
            // having to repeat its package's labels.
            compiled.tags =
                merge_unique(&member.meta.tags, &merge_unique(&workflow.tags, &step.tags));

            // Ownership falls back through the same chain, so a step written by
            // whoever owns the package doesn't have to repeat their name.
            if compiled.owner.is_none() {
                compiled.owner = workflow.owner.clone().or_else(|| member.meta.owner.clone());
            }

            // The workflow's cache settings fold into each of its steps, under
            // whatever the step declared for itself. Doing it here means that
            // by the time anything plans or executes a step, the step carries
            // the whole answer — nothing downstream has to know which workflow
            // it came from to work out what it reads.
            if let Some(from_workflow) = workflow.cache.as_ref() {
                let mut merged = from_workflow.clone();
                if let Some(own) = compiled.cache.as_ref() {
                    crate::cache::graph::layer_over(&mut merged, own);
                }
                compiled.cache = Some(merged);
            }

            if !depended_on.contains(step.name.as_str()) && !compiled.recover {
                unit_exits.push(compiled.name.clone());
            }
            graph.steps.push(compiled);
        }

        // A unit whose every step feeds another (a pure cycle) can't happen
        // here — validation rejects those — but an all-recovery workflow could
        // leave no exits, in which case dependents simply don't wait on it.
        exits.insert(unit, unit_exits);

        // A member's `.env` belongs to that member's steps and to nothing else
        // — it rides on `compiled.env_files` above rather than being poured
        // into one shared map where every other package would read it too.
        // A workflow that can't run without certain variables must say where
        // they're documented — and the workspace that has to say so is the one
        // that declared the requirement, not the monorepo root. Checked here
        // because this is the only place both facts are in scope.
        if !workflow.required_env.is_empty() {
            crate::environment::files::require_template(
                &member.meta,
                &member.dir,
                &workflow.required_env,
                &member.name,
            )?;
        }

        for var in &workflow.required_env {
            if !graph.required_env.contains(var) {
                graph.required_env.push(var.clone());
            }
        }
    }

    validate_flowchart(&graph.steps, &format!("workflow '{}'", graph.label()))?;
    Ok(graph)
}

/// The `.env` files a member's steps resolve through, outermost first.
///
/// The monorepo root, then every sub-workspace between it and this member, then
/// the member itself — so the member's own file answers first and anything it
/// doesn't set falls back outward. Siblings are not in the chain: two packages
/// that need the same variable declare it in the workspace above them, or each
/// declares it for itself.
fn env_chain(workspace: &Workspace, member: &Member) -> Vec<String> {
    use crate::environment::files::Layer;

    let mut layers: Vec<Layer<'_>> = Vec::new();
    // The root is a level even when it's an umbrella — it isn't a package, but
    // its `.env` is still the outermost thing a step falls back to.
    if member.rel != "." {
        layers.push(Layer {
            rel: ".",
            dir: &workspace.root,
            meta: &workspace.root_meta,
        });
    }

    let mut between: Vec<&Member> = workspace
        .members
        .iter()
        .filter(|other| other.rel != "." && encloses(&other.rel, &member.rel))
        .collect();
    // Shallowest first, so the nearest enclosing workspace is layered last.
    between.sort_by_key(|other| other.rel.matches('/').count());
    for other in between {
        layers.push(Layer {
            rel: &other.rel,
            dir: &other.dir,
            meta: &other.meta,
        });
    }

    layers.push(Layer {
        rel: &member.rel,
        dir: &member.dir,
        meta: &member.meta,
    });

    crate::environment::files::chain(&layers)
}

/// Whether `outer` is a workspace directory containing `inner` — a proper
/// ancestor, not the same directory and not a sibling with a shared prefix
/// (`packages/api` does not enclose `packages/api-docs`).
fn encloses(outer: &str, inner: &str) -> bool {
    inner.starts_with(&format!("{}/", outer.trim_end_matches('/')))
}

/// Concatenate two `&[String]` lists, keeping first-seen order and dropping
/// repeats — how requirements cascade without accumulating duplicates.
fn merge_unique(base: &[String], extra: &[String]) -> Vec<String> {
    let mut out: Vec<String> = base.to_vec();
    for value in extra {
        if !out.contains(value) {
            out.push(value.clone());
        }
    }
    out
}

/// Join a member-relative path onto the member's own path, normalizing the
/// root member's `.` so paths stay tidy.
fn join_rel(member_rel: &str, path: &str) -> String {
    if member_rel == "." || member_rel.is_empty() {
        path.to_string()
    } else {
        format!("{}/{}", member_rel.trim_end_matches('/'), path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::CIABATTA_DIR;
    use std::path::{Path, PathBuf};

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ciab_graph_{name}_{}", std::process::id()));
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
        let dir = member.join(CIABATTA_DIR).join(super::super::WORKFLOWS_DIR);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(format!("{name}.toml")), body).unwrap();
    }

    /// A → B: the motivating example. `api`'s build needs `proto`'s generated
    /// stubs, and says so in its own config.
    fn api_and_proto(name: &str) -> (PathBuf, Workspace) {
        let root = scratch(name);
        let api = member(
            &root,
            "packages/api",
            "[workspace]\nname = \"api\"\nowner = \"Ada\"\ndepends_on = [\"proto:generate\"]\n",
        );
        workflow(
            &api,
            "build",
            "[[steps]]\nname = \"compile\"\nrun = \"cargo build\"\n\
             [[steps]]\nname = \"package\"\nrun = \"tar czf app.tgz .\"\nneeds = [\"compile\"]\n",
        );
        let proto = member(&root, "packages/proto", "[workspace]\nname = \"proto\"\n");
        workflow(
            &proto,
            "generate",
            "[[steps]]\nname = \"protoc\"\nrun = \"protoc --rust_out=.\"\n",
        );
        let ws = Workspace::load(&root).unwrap();
        (root, ws)
    }

    /// A workflow's `background:` array compiles into nodes that sit outside
    /// the order entirely — no incoming edge, and never a wave.
    #[test]
    fn background_tasks_compile_outside_the_waves() {
        let root = scratch("background");
        let web = member(&root, "packages/web", "[workspace]\nname = \"web\"\n");
        workflow(
            &web,
            "dev",
            "[[background]]\nname = \"mock-api\"\nrun = \"node mock.js\"\n\
             [[steps]]\nname = \"compile\"\nrun = \"yarn build\"\n\
             [[steps]]\nname = \"integration\"\nrun = \"yarn test\"\nneeds = [\"compile\"]\n",
        );
        let ws = Workspace::load(&root).unwrap();
        let graph = build(&ws, "dev", &Selection::default()).unwrap();

        let task = graph
            .steps
            .iter()
            .find(|s| s.name == "web:mock-api")
            .expect("the background task is a node of the graph");
        assert!(task.background, "it has to be marked as one");
        assert!(task.persistent, "a background task never exits");
        assert!(task.needs.is_empty(), "it waits for nothing");

        // Nothing depends on it, so it can never gate anything.
        assert!(
            !graph
                .steps
                .iter()
                .any(|s| s.needs.iter().any(|n| n == "web:mock-api")),
            "nothing may declare a dependency on a background task"
        );

        // And it is in no wave: the waves are the ordinary two.
        let waves: Vec<Vec<&str>> = graph
            .waves()
            .iter()
            .map(|w| w.iter().map(|s| s.name.as_str()).collect())
            .collect();
        assert_eq!(waves, vec![vec!["web:compile"], vec!["web:integration"]]);

        let _ = std::fs::remove_dir_all(&root);
    }

    /// The mistakes worth catching at compile time rather than at 3am.
    #[test]
    fn a_background_task_that_cannot_work_is_refused() {
        let cases: [(&str, &str); 4] = [
            (
                // `needs` on something that is started before the first wave.
                "[[background]]\nname = \"mock\"\nrun = \"node mock.js\"\nneeds = [\"compile\"]\n\
                 [[steps]]\nname = \"compile\"\nrun = \"yarn build\"\n",
                "gates nothing",
            ),
            (
                // Nothing to run.
                "[[background]]\nname = \"mock\"\n\
                 [[steps]]\nname = \"compile\"\nrun = \"yarn build\"\n",
                "nothing to run",
            ),
            (
                // One name, two nodes.
                "[[background]]\nname = \"compile\"\nrun = \"node mock.js\"\n\
                 [[steps]]\nname = \"compile\"\nrun = \"yarn build\"\n",
                "One name, one node",
            ),
            (
                // Background tasks and nothing to support: they would be
                // started and stopped in the same breath.
                "[[background]]\nname = \"mock\"\nrun = \"node mock.js\"\n",
                "no steps",
            ),
        ];

        for (index, (body, expected)) in cases.iter().enumerate() {
            let root = scratch(&format!("badbackground{index}"));
            let web = member(&root, "packages/web", "[workspace]\nname = \"web\"\n");
            workflow(&web, "dev", body);
            let ws = Workspace::load(&root).unwrap();

            let err = build(&ws, "dev", &Selection::default())
                .expect_err("this configuration cannot work")
                .to_string();
            assert!(
                err.contains(expected),
                "case {index} should explain '{expected}', got: {err}"
            );
            let _ = std::fs::remove_dir_all(&root);
        }
    }

    #[test]
    fn cross_workspace_dependency_is_pulled_in_and_ordered_first() {
        let (root, ws) = api_and_proto("cross");
        let graph = build(&ws, "build", &Selection::default()).unwrap();

        // The `proto` unit came along even though only `api` defines "build".
        assert_eq!(
            graph.units,
            vec![
                UnitId {
                    member: "proto".into(),
                    workflow: "generate".into()
                },
                UnitId {
                    member: "api".into(),
                    workflow: "build".into()
                },
            ]
        );

        let names: Vec<&str> = graph.steps.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["proto:protoc", "api:compile", "api:package"]);

        // api's entry step waits on proto's exit step; its own chain is intact.
        let compile = graph
            .steps
            .iter()
            .find(|s| s.name == "api:compile")
            .unwrap();
        assert_eq!(compile.needs, vec!["proto:protoc".to_string()]);
        let package = graph
            .steps
            .iter()
            .find(|s| s.name == "api:package")
            .unwrap();
        assert_eq!(package.needs, vec!["api:compile".to_string()]);

        // Every node knows where it came from and where to run.
        assert_eq!(compile.workspace.as_deref(), Some("api"));
        assert_eq!(compile.cwd.as_deref(), Some("packages/api"));
        assert_eq!(compile.owner.as_deref(), Some("Ada"));

        // Waves are the render order: proto first, then api's two steps.
        let waves = graph.waves();
        assert_eq!(waves.len(), 3);
        assert_eq!(waves[0][0].name, "proto:protoc");

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn isolated_runs_only_what_was_asked_for() {
        let (root, ws) = api_and_proto("isolated");
        let graph = build(
            &ws,
            "build",
            &Selection {
                only: vec!["api".into()],
                isolated: true,
            },
        )
        .unwrap();
        assert_eq!(graph.units.len(), 1);
        let names: Vec<&str> = graph.steps.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["api:compile", "api:package"]);
        // With nothing upstream, the entry step waits on nothing.
        assert!(graph.steps[0].needs.is_empty());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn bare_member_dependency_means_the_same_workflow_and_tolerates_absence() {
        let root = scratch("bare");
        let api = member(
            &root,
            "api",
            "[workspace]\nname = \"api\"\ndepends_on = [\"common\"]\n",
        );
        workflow(&api, "build", "[[steps]]\nname = \"b\"\nrun = \"true\"\n");
        workflow(&api, "lint", "[[steps]]\nname = \"l\"\nrun = \"true\"\n");
        let common = member(&root, "common", "[workspace]\nname = \"common\"\n");
        workflow(
            &common,
            "build",
            "[[steps]]\nname = \"c\"\nrun = \"true\"\n",
        );
        let ws = Workspace::load(&root).unwrap();

        // "build" exists in both, so the dependency applies.
        let build_graph = build(&ws, "build", &Selection::default()).unwrap();
        let api_b = build_graph
            .steps
            .iter()
            .find(|s| s.name == "api:b")
            .unwrap();
        assert_eq!(api_b.needs, vec!["common:c".to_string()]);

        // "lint" doesn't exist in common, so the dependency simply doesn't apply.
        let lint_graph = build(&ws, "lint", &Selection::default()).unwrap();
        assert_eq!(lint_graph.units.len(), 1);
        assert!(lint_graph.steps[0].needs.is_empty());

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn two_workflows_compile_into_one_graph_with_shared_dependencies_run_once() {
        let root = scratch("multi");
        let api = member(
            &root,
            "api",
            "[workspace]\nname = \"api\"\ndepends_on = [\"proto:generate\"]\n",
        );
        workflow(
            &api,
            "build",
            "[[steps]]\nname = \"compile\"\nrun = \"true\"\n",
        );
        workflow(&api, "test", "[[steps]]\nname = \"unit\"\nrun = \"true\"\n");
        let proto = member(&root, "proto", "[workspace]\nname = \"proto\"\n");
        workflow(
            &proto,
            "generate",
            "[[steps]]\nname = \"protoc\"\nrun = \"true\"\n",
        );
        let ws = Workspace::load(&root).unwrap();

        let graph = build_many(
            &ws,
            &["build".to_string(), "test".to_string()],
            &Selection::default(),
        )
        .unwrap();

        // proto:generate is a dependency of both api workflows, and appears once.
        let names: Vec<&str> = graph.steps.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names.iter().filter(|n| n.contains("protoc")).count(), 1);
        assert_eq!(graph.units.len(), 3);
        assert_eq!(graph.label(), "build + test");

        // api contributes two units now, so its nodes carry the workflow name.
        assert!(names.contains(&"api:build:compile"));
        assert!(names.contains(&"api:test:unit"));

        // Both api workflows still wait on the shared dependency.
        for name in ["api:build:compile", "api:test:unit"] {
            let step = graph.steps.iter().find(|s| s.name == name).unwrap();
            assert_eq!(step.needs, vec!["proto:protoc".to_string()], "{name}");
        }

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn the_monorepo_roots_standard_variables_reach_every_step() {
        let root = scratch("rootenv");
        member(
            &root,
            ".",
            "[workspace]\numbrella = true\n\n[workspace.env]\n\
             LOG_LEVEL = \"info\"\nREGION = \"us-east-1\"\n",
        );
        let api = member(
            &root,
            "api",
            "[workspace]\nname = \"api\"\n\n[workspace.env]\nREGION = \"eu-west-1\"\n",
        );
        workflow(&api, "build", "[[steps]]\nname = \"b\"\nrun = \"true\"\n");
        let ws = Workspace::load(&root).unwrap();
        let graph = build(&ws, "build", &Selection::default()).unwrap();
        let step = &graph.steps[0];

        // The umbrella's variables are inherited…
        assert_eq!(step.env.get("LOG_LEVEL").unwrap(), "info");
        // …but a package may still override one for itself.
        assert_eq!(step.env.get("REGION").unwrap(), "eu-west-1");

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn tags_accumulate_from_workspace_through_workflow_to_step() {
        let root = scratch("tags");
        let api = member(
            &root,
            "api",
            "[workspace]\nname = \"api\"\ntags = [\"backend\"]\n",
        );
        workflow(
            &api,
            "test",
            "tags = [\"slow\"]\n[[steps]]\nname = \"unit\"\nrun = \"true\"\ntags = [\"fast\"]\n\
             [[steps]]\nname = \"e2e\"\nrun = \"true\"\n",
        );
        let ws = Workspace::load(&root).unwrap();
        let graph = build(&ws, "test", &Selection::default()).unwrap();

        // Unlike env, tags accumulate rather than override.
        let unit = graph.steps.iter().find(|s| s.name == "api:unit").unwrap();
        assert_eq!(unit.tags, vec!["backend", "slow", "fast"]);
        // A step with no tags of its own still inherits both outer levels.
        let e2e = graph.steps.iter().find(|s| s.name == "api:e2e").unwrap();
        assert_eq!(e2e.tags, vec!["backend", "slow"]);

        // …which is what makes filtering the compiled graph work.
        let filters = crate::run::filter::parse_all(&["tag:fast".to_string()]).unwrap();
        let (kept, _) = crate::run::filter::apply(&graph.steps, &filters).unwrap();
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].name, "api:unit");

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn requirements_and_env_cascade_from_workspace_to_step() {
        let root = scratch("cascade");
        let api = member(
            &root,
            "api",
            "[workspace]\nname = \"api\"\nrequires = [\"cargo\"]\n\n\
             [workspace.env]\nRUST_LOG = \"info\"\nSHARED = \"from-workspace\"\n",
        );
        workflow(
            &api,
            "build",
            "requires = [\"protoc\"]\n[env]\nSHARED = \"from-workflow\"\n\
             [[steps]]\nname = \"compile\"\nrun = \"cargo build\"\nrequires = [\"cargo\", \"sccache\"]\n\
             [steps.env]\nPROFILE = \"release\"\n",
        );
        let ws = Workspace::load(&root).unwrap();
        let graph = build(&ws, "build", &Selection::default()).unwrap();
        let step = &graph.steps[0];

        // Cascaded in order, de-duplicated (cargo appears once).
        assert_eq!(step.requires, vec!["cargo", "protoc", "sccache"]);
        assert_eq!(step.env.get("RUST_LOG").unwrap(), "info");
        assert_eq!(step.env.get("PROFILE").unwrap(), "release");
        // The more specific level wins the collision.
        assert_eq!(step.env.get("SHARED").unwrap(), "from-workflow");

        std::fs::remove_dir_all(&root).ok();
    }

    /// A member's `.env` belongs to that member's steps. Pouring every
    /// member's file into one run-wide list is how one package's settings end
    /// up being read by another's steps.
    #[test]
    fn env_files_ride_on_the_steps_that_resolve_through_them() {
        let root = scratch("envfiles");
        let api = member(
            &root,
            "packages/api",
            "[workspace]\nname = \"api\"\nenv_file = \".env\"\n\
             env_default = \".env.default\"\n",
        );
        workflow(
            &api,
            "build",
            "env_file = [\".env.build\"]\nREQUIRED_ENV = [\"API_TOKEN\"]\n\
             [[steps]]\nname = \"b\"\nrun = \"true\"\n",
        );
        let ws = Workspace::load(&root).unwrap();
        let graph = build(&ws, "build", &Selection::default()).unwrap();
        assert!(
            graph.env_files.is_empty(),
            "a member's env files are its steps' business, not the whole run's"
        );
        assert_eq!(
            graph.steps[0].env_files,
            vec!["packages/api/.env", "packages/api/.env.build"],
            "rebased onto the monorepo root, nearest last"
        );
        assert_eq!(graph.required_env, vec!["API_TOKEN".to_string()]);
        std::fs::remove_dir_all(&root).ok();
    }

    /// Proximity: a package's own `.env` answers first, the workspaces above it
    /// answer for what it doesn't set, and a sibling's file is never consulted.
    #[test]
    fn a_steps_env_chain_runs_from_the_root_down_to_its_own_workspace() {
        let root = scratch("chain");
        std::fs::create_dir_all(root.join(CIABATTA_DIR)).unwrap();
        std::fs::write(
            root.join(CIABATTA_DIR).join("ciabatta.toml"),
            "[workspace]\nname = \"repo\"\numbrella = true\n",
        )
        .unwrap();
        std::fs::write(root.join(".env"), "SHARED=from-root\n").unwrap();

        let api = member(&root, "packages/api", "[workspace]\nname = \"api\"\n");
        std::fs::write(api.join(".env"), "SHARED=from-api\n").unwrap();
        workflow(&api, "build", "[[steps]]\nname = \"b\"\nrun = \"true\"\n");

        // A sibling with a `.env` of its own, and one with none at all: the
        // first must not leak into the second's chain, and the second falls
        // back to the root.
        let web = member(&root, "packages/web", "[workspace]\nname = \"web\"\n");
        std::fs::write(web.join(".env"), "SHARED=from-web\n").unwrap();
        workflow(&web, "build", "[[steps]]\nname = \"b\"\nrun = \"true\"\n");

        let docs = member(&root, "docs", "[workspace]\nname = \"docs\"\n");
        workflow(&docs, "build", "[[steps]]\nname = \"b\"\nrun = \"true\"\n");

        let ws = Workspace::load(&root).unwrap();
        let graph = build(&ws, "build", &Selection::default()).unwrap();

        let chain = |member: &str| -> Vec<String> {
            graph
                .steps
                .iter()
                .find(|s| s.workspace.as_deref() == Some(member))
                .unwrap()
                .env_files
                .clone()
        };

        assert_eq!(
            chain("api"),
            vec![".env".to_string(), "packages/api/.env".to_string()],
            "the root first, the package last — the nearest file wins"
        );
        assert_eq!(
            chain("docs"),
            vec![".env".to_string()],
            "a package with no `.env` of its own resolves higher"
        );
        assert!(
            !chain("api").iter().any(|f| f.contains("web")),
            "a sibling's `.env` is not a fallback"
        );

        std::fs::remove_dir_all(&root).ok();
    }

    /// A workspace nested inside another is a level of its own: the chain picks
    /// up every enclosing workspace, and only the enclosing ones.
    #[test]
    fn the_chain_picks_up_every_enclosing_workspace() {
        let root = scratch("nested");
        std::fs::create_dir_all(root.join(CIABATTA_DIR)).unwrap();
        std::fs::write(
            root.join(CIABATTA_DIR).join("ciabatta.toml"),
            "[workspace]\nname = \"repo\"\numbrella = true\n",
        )
        .unwrap();

        let services = member(&root, "services", "[workspace]\nname = \"services\"\n");
        std::fs::write(services.join(".env"), "TIER=services\n").unwrap();

        let api = member(&root, "services/api", "[workspace]\nname = \"api\"\n");
        std::fs::write(api.join(".env"), "TIER=api\n").unwrap();
        workflow(&api, "build", "[[steps]]\nname = \"b\"\nrun = \"true\"\n");

        // A near-miss on the path prefix: `services-legacy` encloses nothing.
        let legacy = member(&root, "services-legacy", "[workspace]\nname = \"legacy\"\n");
        std::fs::write(legacy.join(".env"), "TIER=legacy\n").unwrap();

        let ws = Workspace::load(&root).unwrap();
        let graph = build(&ws, "build", &Selection::default()).unwrap();
        assert_eq!(
            graph.steps[0].env_files,
            vec!["services/.env".to_string(), "services/api/.env".to_string()],
            "outermost first, and `services-legacy` is a sibling, not a parent"
        );

        std::fs::remove_dir_all(&root).ok();
    }

    /// A workflow that can't run without certain variables has to say where
    /// they're documented — and it's the *sub-workspace* that declared them
    /// that has to say so, not the monorepo root, which never mentioned them.
    #[test]
    fn a_member_needing_variables_must_name_its_template() {
        let root = scratch("envdefault");
        let api = member(&root, "packages/api", "[workspace]\nname = \"api\"\n");
        workflow(
            &api,
            "build",
            "REQUIRED_ENV = [\"API_TOKEN\"]\n\
             [[steps]]\nname = \"b\"\nrun = \"true\"\n",
        );

        let ws = Workspace::load(&root).unwrap();
        let err = build(&ws, "build", &Selection::default())
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("'api' declares"),
            "the error must name the member: {err}"
        );
        assert!(err.contains("API_TOKEN"), "and the variables: {err}");
        assert!(err.contains("env_default"), "and the fix: {err}");

        // A member with no REQUIRED_ENV is asked for nothing.
        let web = member(&root, "packages/web", "[workspace]\nname = \"web\"\n");
        workflow(&web, "bundle", "[[steps]]\nname = \"b\"\nrun = \"true\"\n");
        let ws = Workspace::load(&root).unwrap();
        assert!(build(&ws, "bundle", &Selection::default()).is_ok());

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn node_ids_disambiguate_when_one_member_contributes_twice() {
        let root = scratch("twounits");
        let api = member(&root, "api", "[workspace]\nname = \"api\"\n");
        // Workflow-level `needs`, so only `build` waits on `codegen` — a
        // blanket depends_on would make codegen depend on itself.
        workflow(
            &api,
            "build",
            "needs = [\"self:codegen\"]\n[[steps]]\nname = \"compile\"\nrun = \"true\"\n",
        );
        workflow(
            &api,
            "codegen",
            "[[steps]]\nname = \"gen\"\nrun = \"true\"\n",
        );
        let ws = Workspace::load(&root).unwrap();
        let graph = build(&ws, "build", &Selection::default()).unwrap();

        let names: Vec<&str> = graph.steps.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["api:codegen:gen", "api:build:compile"]);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_dependency_cycle_names_the_loop() {
        let root = scratch("cycle");
        let a = member(
            &root,
            "a",
            "[workspace]\nname = \"a\"\ndepends_on = [\"b\"]\n",
        );
        workflow(&a, "build", "[[steps]]\nname = \"s\"\nrun = \"true\"\n");
        let b = member(
            &root,
            "b",
            "[workspace]\nname = \"b\"\ndepends_on = [\"a\"]\n",
        );
        workflow(&b, "build", "[[steps]]\nname = \"s\"\nrun = \"true\"\n");
        let ws = Workspace::load(&root).unwrap();

        let err = build(&ws, "build", &Selection::default())
            .unwrap_err()
            .to_string();
        assert!(err.contains("cycle"), "{err}");
        assert!(err.contains("a:build") && err.contains("b:build"), "{err}");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_dependency_on_an_unknown_sub_workspace_is_reported() {
        let root = scratch("unknown");
        let a = member(
            &root,
            "a",
            "[workspace]\nname = \"a\"\ndepends_on = [\"ghost:build\"]\n",
        );
        workflow(&a, "build", "[[steps]]\nname = \"s\"\nrun = \"true\"\n");
        let ws = Workspace::load(&root).unwrap();
        let err = build(&ws, "build", &Selection::default())
            .unwrap_err()
            .to_string();
        assert!(err.contains("no sub-workspace is named 'ghost'"), "{err}");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_missing_workflow_lists_what_does_exist() {
        let root = scratch("nowf");
        let a = member(&root, "a", "[workspace]\nname = \"a\"\n");
        workflow(&a, "build", "[[steps]]\nname = \"s\"\nrun = \"true\"\n");
        let ws = Workspace::load(&root).unwrap();
        let err = build(&ws, "deploy", &Selection::default())
            .unwrap_err()
            .to_string();
        assert!(err.contains("Available workflows: build"), "{err}");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_step_needing_a_non_step_says_which_workflow_it_is_in() {
        let root = scratch("badneed");
        let a = member(&root, "a", "[workspace]\nname = \"a\"\n");
        workflow(
            &a,
            "build",
            "[[steps]]\nname = \"s\"\nrun = \"true\"\nneeds = [\"other:thing\"]\n",
        );
        let ws = Workspace::load(&root).unwrap();
        let err = build(&ws, "build", &Selection::default())
            .unwrap_err()
            .to_string();
        assert!(err.contains("not a step in that workflow"), "{err}");
        assert!(err.contains("depends_on"), "{err}");
        std::fs::remove_dir_all(&root).ok();
    }

    /// What a build reads is a property of that build, so it is declared on the
    /// workflow — and has to reach every step of it without each step repeating
    /// the list.
    #[test]
    fn a_workflows_cache_settings_reach_its_steps() {
        let root = scratch("wfcache");
        let a = member(&root, "a", "[workspace]\nname = \"a\"\n");
        workflow(
            &a,
            "build",
            "[cache]\nenabled = true\ninputs = [\"src/**/*\"]\noutputs = [\"dist/**/*\"]\n\
             [[steps]]\nname = \"compile\"\nrun = \"make\"\n\
             [[steps]]\nname = \"docs\"\nrun = \"make docs\"\n\
             [steps.cache]\ninputs = [\"docs/**/*\"]\n",
        );
        let ws = Workspace::load(&root).unwrap();
        let graph = build(&ws, "build", &Selection::default()).unwrap();

        let compile = graph.steps.iter().find(|s| s.name == "a:compile").unwrap();
        let cache = compile.cache.as_ref().expect("inherited from the workflow");
        assert_eq!(cache.enabled, Some(true));
        assert_eq!(cache.inputs, vec!["src/**/*".to_string()]);
        assert_eq!(cache.outputs, vec!["dist/**/*".to_string()]);

        // A step that narrows one field keeps the rest, and does not silently
        // turn caching off by failing to mention `enabled`.
        let docs = graph.steps.iter().find(|s| s.name == "a:docs").unwrap();
        let cache = docs.cache.as_ref().expect("its own, over the workflow's");
        assert_eq!(cache.inputs, vec!["docs/**/*".to_string()], "stated wins");
        assert_eq!(
            cache.outputs,
            vec!["dist/**/*".to_string()],
            "and the rest is inherited"
        );
        assert_eq!(cache.enabled, Some(true));

        std::fs::remove_dir_all(&root).ok();
    }

    /// Push and pull are the same artifact in opposite directions. `from:` says
    /// so once, rather than leaving two copies to drift apart.
    #[test]
    fn a_pull_step_inherits_the_push_step_it_mirrors() {
        let root = scratch("from");
        let a = member(&root, "a", "[workspace]\nname = \"a\"\n");
        workflow(
            &a,
            "release",
            "[[steps]]\nname = \"publish\"\nkind = \"push\"\n\
             registry = \"nexus\"\nartifact = \"dist/app\"\n\
             publish_path = \"app/{CIABATTA_COMMIT}/app\"\n\
             [[steps]]\nname = \"fetch\"\nkind = \"pull\"\nfrom = \"publish\"\n\
             [[steps]]\nname = \"fetch-elsewhere\"\nkind = \"pull\"\nfrom = \"publish\"\n\
             artifact = \"vendor/app\"\n",
        );
        let ws = Workspace::load(&root).unwrap();
        let graph = build(&ws, "release", &Selection::default()).unwrap();
        let run = into_run(graph).unwrap();

        let fetch = run.steps.iter().find(|s| s.name == "a:fetch").unwrap();
        let transfer = fetch.transfer().expect("a pull step has a transfer");
        assert_eq!(transfer.direction, crate::run::Direction::Pull);
        assert_eq!(transfer.registry, Some("nexus"));
        assert_eq!(transfer.artifact, Some("dist/app"));

        // What the step states for itself wins over what it inherits.
        let elsewhere = run
            .steps
            .iter()
            .find(|s| s.name == "a:fetch-elsewhere")
            .unwrap();
        let transfer = elsewhere.transfer().unwrap();
        assert_eq!(transfer.registry, Some("nexus"), "inherited");
        assert_eq!(transfer.artifact, Some("vendor/app"), "stated wins");

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_from_that_names_nothing_is_refused_with_the_options_listed() {
        let root = scratch("from_missing");
        let a = member(&root, "a", "[workspace]\nname = \"a\"\n");
        workflow(
            &a,
            "release",
            "[[steps]]\nname = \"publish\"\nkind = \"push\"\nregistry = \"nexus\"\n\
             [[steps]]\nname = \"fetch\"\nkind = \"pull\"\nfrom = \"nope\"\n",
        );
        let ws = Workspace::load(&root).unwrap();
        let graph = build(&ws, "release", &Selection::default()).unwrap();
        let err = into_run(graph).unwrap_err().to_string();
        assert!(err.contains("names no step"), "{err}");
        assert!(
            err.contains("a:publish"),
            "the error must list what is available: {err}"
        );

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn push_steps_and_recovery_nodes_survive_compilation() {
        let root = scratch("push");
        let a = member(&root, "a", "[workspace]\nname = \"a\"\n");
        workflow(
            &a,
            "release",
            "[[steps]]\nname = \"build\"\nrun = \"cargo build\"\non_error = \"fix\"\n\
             [[steps]]\nname = \"publish\"\nkind = \"push\"\nneeds = [\"build\"]\n\
             registry = \"nexus\"\nartifact = \"target/release/app\"\n\
             publish_path = \"app/bin\"\n\
             [[steps]]\nname = \"fix\"\nrecover = true\nretry = \"build\"\n\
             options = [ { label = \"clean\", run = \"cargo clean\", default = true } ]\n",
        );
        let ws = Workspace::load(&root).unwrap();
        let graph = build(&ws, "release", &Selection::default()).unwrap();

        let publish = graph.steps.iter().find(|s| s.name == "a:publish").unwrap();
        assert!(publish.is_push());
        // A transfer step counts as having an action: the engine performs the
        // built-in move rather than running a command.
        assert!(publish.has_action());
        let transfer = publish.transfer().expect("a push step has a transfer");
        assert_eq!(transfer.registry, Some("nexus"));
        assert_eq!(transfer.artifact, Some("target/release/app"));

        // Recovery edges were renamed alongside everything else.
        let build_step = graph.steps.iter().find(|s| s.name == "a:build").unwrap();
        assert_eq!(build_step.on_error.as_deref(), Some("a:fix"));
        let fix = graph.steps.iter().find(|s| s.name == "a:fix").unwrap();
        assert_eq!(fix.retry.as_deref(), Some("a:build"));
        // A recovery node is not a wave of its own.
        assert!(graph.waves().iter().flatten().all(|s| s.name != "a:fix"));

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn an_empty_workflow_is_refused() {
        let root = scratch("empty");
        let a = member(&root, "a", "[workspace]\nname = \"a\"\n");
        workflow(&a, "build", "description = \"nothing yet\"\n");
        let ws = Workspace::load(&root).unwrap();
        let err = build(&ws, "build", &Selection::default())
            .unwrap_err()
            .to_string();
        assert!(err.contains("has no steps"), "{err}");
        std::fs::remove_dir_all(&root).ok();
    }
}
