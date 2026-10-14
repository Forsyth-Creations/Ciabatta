//! What a run depends on, environment-wise.
//!
//! A run's steps are shell scripts, and the thing that most often makes one
//! behave differently on two machines is not the graph — it's an environment
//! variable that was set here and isn't there. The graph is drawn before it
//! runs; this does the same for the environment, so `required_env`, the
//! `.env` files a run sources, the `[env]` tables that cascade from
//! sub-workspace to step, and the bare `$VAR` references inside the commands
//! are all one list with values attached.
//!
//! One collector serves both surfaces: the terminal prints it before a run
//! starts, and the daemon serves it so the web app can draw each variable as
//! what it actually is — a dependency of the steps that read it.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::{FileKind, ResolvedRun, RunStep, prepare_env, with_env_levels};
use crate::config::CiabattaConfig;

/// Where a variable's effective value comes from.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    /// The environment ciabatta itself was launched with — your shell, the CI
    /// system, or a `-e` flag. Beats every declared source.
    Environment,
    /// The first `.env` file in the search that sets it (see `file`).
    EnvFile,
    /// A workspace's defaults file (see `file`): nothing searched before it
    /// sets it.
    Default,
    /// A declared `[env]` table: a sub-workspace's, a workflow's, or a step's.
    Config,
    /// A custom argument given after `...` on the command line, which became a
    /// `CIABATTA_ARG_*` variable for this run only.
    Argument,
    /// Nothing supplies it. Steps reading it will see an empty string.
    Unset,
}

/// One environment variable a run depends on.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct EnvVar {
    pub key: String,
    /// The value the steps will see, masked when the name says it's a secret.
    /// `None` when nothing supplies it, or when steps disagree (`varies`).
    pub value: Option<String>,
    /// Whether `value` was masked rather than printed.
    pub secret: bool,
    /// Declared in `required_env`: the run refuses to start without it.
    pub required: bool,
    /// Named in a workflow's `env` list: a variable the workflow says it uses,
    /// whose value comes from the environment, and which may be unset.
    #[serde(default, skip_serializing_if = "crate::format::is_false")]
    pub declared: bool,
    pub origin: Origin,
    /// The file that supplied it, when `origin` is `EnvFile` or `Default`.
    pub file: Option<String>,
    /// What kind of file that is — a workflow's, a profile's, a `.env`, or a
    /// defaults file — which is why it was searched where it was.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub via: Option<FileKind>,
    /// Why the value won, in a sentence: shown beside it on the run page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// The files that also set it, later in the search, and so lost to the
    /// winner — the terminal, or the first file to set it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub overrides: Vec<String>,
    /// Steps that read it or declare it — the edges to draw on a graph.
    pub steps: Vec<String>,
    /// Set by several steps to different values, so there is no single one.
    pub varies: bool,
    /// Why a step needs it, when that isn't visible in the step itself — a
    /// registry credential, a placeholder in a publish path. Defaulted so a run
    /// recorded before this existed still loads.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub purpose: Option<String>,
    /// Whether every step that needs it carries on without it, so being unset
    /// is a fact rather than a problem: a push that goes unauthenticated, an
    /// AWS credential found another way.
    #[serde(default, skip_serializing_if = "crate::format::is_false")]
    pub optional: bool,
    /// Part of a step's cache key: declared in an `env:` table, in
    /// `required_env`, or under `cache.env`. Changing it rebuilds that step.
    #[serde(default, skip_serializing_if = "crate::format::is_false")]
    pub in_cache_key: bool,
}

/// Every environment variable a run depends on, resolved as far as the inputs
/// allow.
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct EnvReport {
    /// The env files searched, in order: for each variable the first that
    /// sets it wins, after the terminal.
    pub files: Vec<String>,
    /// `required_env`, as declared.
    pub required: Vec<String>,
    /// Required variables that are still empty or unset — the run can't start
    /// until they are supplied.
    pub missing: Vec<String>,
    /// Every variable, required ones first, then by name.
    pub vars: Vec<EnvVar>,
    /// The env profile the run was started with (`--env-profile`), if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    /// Why that profile is the one in use — or why there is none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile_reason: Option<String>,
}

impl EnvReport {
    pub fn is_empty(&self) -> bool {
        self.vars.is_empty()
    }

    /// The report as a block of terminal text, or `None` when a run depends on
    /// no variables at all (in which case there is nothing worth saying).
    pub fn render(&self, workflow: &str, colour: bool) -> Option<String> {
        use owo_colors::{OwoColorize, Style};

        if self.is_empty() {
            return None;
        }
        let paint = |style: Style| crate::color::paint(colour, style);
        let faint = paint(Style::new().dimmed());

        let mut out = String::new();
        out.push_str(&format!(
            "Environment for '{workflow}' — {} variable(s) this run depends on\n",
            self.vars.len()
        ));
        if let Some(reason) = &self.profile_reason {
            out.push_str(&format!("  profile: {reason}\n"));
        }
        // The rule every value below was decided by, so the notes beside each
        // one read as an application of it rather than as trivia.
        match self.files.is_empty() {
            true => out.push_str("  precedence: terminal only (no env files found)\n"),
            false => out.push_str(&format!(
                "  precedence: terminal > {} (first to set it wins)\n",
                self.files.join(" > ")
            )),
        }

        // Custom arguments are shown on their own, by `render_arguments`.
        let rows: Vec<(&EnvVar, String)> = self
            .vars
            .iter()
            .filter(|var| var.origin != Origin::Argument)
            .map(|var| {
                let value = match (&var.value, var.varies) {
                    (Some(value), _) => value.clone(),
                    (None, true) => "(set per step)".to_string(),
                    (None, false) => "(unset)".to_string(),
                };
                (var, value)
            })
            .collect();

        // One width for every group, so the columns line up down the report.
        let width = rows.iter().map(|(v, _)| v.key.len()).max().unwrap_or(0);
        // Long values (a connection string, a path list) shouldn't push every
        // annotation off the right of the terminal, so the value column is
        // padded only up to a readable width.
        let value_width = rows
            .iter()
            .map(|(_, value)| value.len())
            .filter(|len| *len <= 32)
            .max()
            .unwrap_or(0);

        // Grouped by where the value came from — each source file its own
        // group — in the order the search consults them, so the report reads
        // top to bottom the way the values were decided. The header says where
        // and why once; the rows say only what's particular to each.
        let mut groups: Vec<(Group, Option<String>)> = Vec::new();
        for (var, _) in &rows {
            let key = (
                Group::of(var),
                var.file.clone().filter(|_| Group::of(var).has_file()),
            );
            if !groups.contains(&key) {
                groups.push(key);
            }
        }
        let searched = |file: &Option<String>| {
            file.as_ref()
                .and_then(|f| self.files.iter().position(|x| x == f))
                .unwrap_or(0)
        };
        groups.sort_by_key(|(group, file)| (*group, searched(file)));

        for (group, file) in &groups {
            let style = paint(group.colour());
            let heading = paint(group.colour().bold());
            let (title, why) =
                group.header(file.as_deref(), self.profile.as_deref(), self.files.len());
            out.push_str(&format!(
                "  {} {}\n",
                title.style(heading),
                format!("— {why}").style(faint),
            ));
            for (var, value) in rows.iter().filter(|(var, _)| {
                Group::of(var) == *group && (!group.has_file() || var.file == *file)
            }) {
                let mut notes: Vec<String> = Vec::new();
                if !var.overrides.is_empty() {
                    notes.push(format!("overrides {}", var.overrides.join(", ")));
                }
                if var.required {
                    notes.push("required_env".to_string());
                }
                if var.declared {
                    notes.push("env".to_string());
                }
                if let Some(purpose) = &var.purpose {
                    notes.push(purpose.clone());
                }
                if var.optional && !var.declared {
                    notes.push("optional".to_string());
                }
                if var.varies {
                    notes.push("set differently by different steps".to_string());
                }
                if !var.steps.is_empty() {
                    notes.push(format!("used by {}", var.steps.join(", ")));
                }
                // A required variable nothing set is the one line that stops
                // the run; it's red whatever else it is.
                let key_style = match *group == Group::Unset && !var.optional {
                    true => paint(Style::new().red().bold()),
                    false => style,
                };
                // Padded only when notes follow, so a bare row has no
                // trailing whitespace.
                let rest = match notes.is_empty() {
                    true => value.clone(),
                    false => format!(
                        "{value:value_width$}  {}",
                        format!("[{}]", notes.join(" · ")).style(faint)
                    ),
                };
                out.push_str(&format!(
                    "    {}  {rest}\n",
                    format!("{:width$}", var.key).style(key_style),
                ));
            }
        }

        if !self.missing.is_empty() {
            out.push_str(&format!(
                "  {}\n",
                format!(
                    "✗ still needed: {} — set them with -e KEY=VALUE",
                    self.missing.join(", ")
                )
                .style(paint(Style::new().red()))
            ));
        }
        Some(out)
    }
}

/// Where a variable's value came from, as the report groups them — in the
/// order the search consults them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Group {
    Terminal,
    Workflow,
    Profile,
    EnvFile,
    Default,
    Config,
    Unset,
}

impl Group {
    fn of(var: &EnvVar) -> Group {
        match (var.origin, var.via) {
            (Origin::Environment | Origin::Argument, _) => Group::Terminal,
            (Origin::Config, _) => Group::Config,
            (Origin::Unset, _) => Group::Unset,
            (Origin::Default, _) | (_, Some(FileKind::Default)) => Group::Default,
            (_, Some(FileKind::Workflow)) => Group::Workflow,
            (_, Some(FileKind::Profile)) => Group::Profile,
            (Origin::EnvFile, _) => Group::EnvFile,
        }
    }

    /// Whether the group is split by source file.
    fn has_file(self) -> bool {
        matches!(
            self,
            Group::Workflow | Group::Profile | Group::EnvFile | Group::Default
        )
    }

    /// The header over a group: where its values came from, and why that
    /// source won.
    fn header(
        self,
        file: Option<&str>,
        profile: Option<&str>,
        searched: usize,
    ) -> (String, String) {
        let file = file.unwrap_or("?");
        match self {
            Group::Terminal => (
                "terminal".into(),
                "set in the terminal, which always wins".into(),
            ),
            Group::Workflow => (
                format!("{file} (workflow env_file)"),
                "named by the workflow, searched before every workspace's files".into(),
            ),
            Group::Profile => (
                format!("{file} (profile {})", profile.unwrap_or("?")),
                "the elected profile, copied into this workspace's .env and searched in its place"
                    .into(),
            ),
            Group::EnvFile => (
                file.to_string(),
                "the first .env in the search (from where the run started, up to the root) \
                 to set these"
                    .into(),
            ),
            Group::Default => (
                format!("{file} (defaults)"),
                "nothing searched before it sets these".into(),
            ),
            Group::Config => (
                "step [env] tables".into(),
                "set on the step itself, on top of the run's environment".into(),
            ),
            Group::Unset => (
                "unset".into(),
                format!("not set in the terminal or in any of the {searched} env file(s) searched"),
            ),
        }
    }

    fn colour(self) -> owo_colors::Style {
        use owo_colors::Style;
        match self {
            Group::Terminal => Style::new().magenta(),
            Group::Workflow => Style::new().blue(),
            Group::Profile => Style::new().cyan(),
            Group::EnvFile => Style::new().green(),
            Group::Default => Style::new().dimmed(),
            Group::Config => Style::new().yellow(),
            Group::Unset => Style::new().dimmed(),
        }
    }
}

impl EnvReport {
    /// The custom arguments this run was given after `...`, with the nudge
    /// to keep them somewhere better — or `None` when there were none.
    ///
    /// Separate from [`Self::render`] so the terminal can colour it: these are
    /// the one part of a run's environment that exists nowhere but in the
    /// command somebody typed, and so the part the next run won't have unless
    /// it's typed again.
    pub fn render_arguments(&self) -> Option<String> {
        let args: Vec<&EnvVar> = self
            .vars
            .iter()
            .filter(|var| var.origin == Origin::Argument)
            .collect();
        if args.is_empty() {
            return None;
        }
        let width = args.iter().map(|v| v.key.len()).max().unwrap_or(0);
        let mut out = format!("⚑ Custom arguments ({}) — for this run only\n", args.len());
        for var in &args {
            let readers = if var.steps.is_empty() {
                "no step reads it".to_string()
            } else {
                format!("used by {}", var.steps.join(", "))
            };
            out.push_str(&format!(
                "  {:width$}  {}  [{readers}]\n",
                var.key,
                var.value.as_deref().unwrap_or(""),
                width = width
            ));
        }
        out.push_str(&format!(
            "  tip: if this run needs these every time, keep them in an env profile —\n  \
             put them in .env.<name> next to your .env and run with --env-profile <name>.{}\n",
            self.profile
                .as_ref()
                .map(|p| format!(" (This run uses '{p}': add them to .env.{p}.)"))
                .unwrap_or_default()
        ));
        Some(out)
    }
}

/// Collect everything a resolved run depends on, environment-wise.
///
/// `base` is what the run starts from — ciabatta's own environment plus
/// whatever CI, git, and `-e` resolved. Never fails: an unreadable `.env` file
/// (which the engine reports properly a moment later) degrades the report
/// rather than replacing the real error with one about drawing a table.
///
/// `config` is the run's configuration, for the registries its push and pull
/// steps go through: what they read from the environment is decided there, not
/// in the step. Without it, a registry's kind is guessed from its name.
pub fn collect(
    resolved: &ResolvedRun,
    root: &Path,
    base: &HashMap<String, String>,
    config: Option<&CiabattaConfig>,
) -> EnvReport {
    // Where the environment is searched, settled exactly as the engine will
    // — or a variable only the root `.env` sets would be reported as unset. A
    // run the CLI already settled keeps its levels.
    let meta = config
        .and_then(|config| config.workspace.clone())
        .unwrap_or_default();
    let resolved = &with_env_levels(resolved.clone(), root, root, &meta);

    // The engine's own resolution, so the values reported are the values the
    // steps will actually see.
    let prepared = prepare_env(resolved, root, base).ok();
    let env = prepared
        .as_ref()
        .map(|p| p.env.clone())
        .unwrap_or_else(|| base.clone());
    let files = prepared
        .as_ref()
        .map(|p| p.sourced.clone())
        .unwrap_or_default();
    let missing = prepared.as_ref().map(|p| p.missing()).unwrap_or_default();
    let sources = prepared
        .as_ref()
        .map(|p| p.sources.clone())
        .unwrap_or_default();
    let (profile, profile_reason) = crate::environment::profiles::choice(base, root);

    // Which steps touch which variable, and what a step's own `[env]` sets it
    // to. `BTreeMap`s keep both the variable list and each variable's step list
    // stable, so the report doesn't reshuffle between two identical runs.
    let mut users: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut declared: BTreeMap<String, HashSet<String>> = BTreeMap::new();
    // For variables a step needs without saying so: why, and whether *every*
    // step needing it could do without. A variable some step reads outright is
    // never optional, however many others treat it as a nice-to-have.
    let mut purposes: BTreeMap<String, String> = BTreeMap::new();
    let mut needed: HashSet<String> = HashSet::new();
    // Every name a workflow's `env` list declares.
    let mut listed: HashSet<String> = HashSet::new();
    let mut env_listed: HashSet<String> = HashSet::new();
    // What keys the cache — see `CacheConfig::with_step_env`. The member-level
    // `cache.env` isn't on the step, but a workflow's (and the step's own) is.
    let mut keyed: HashSet<String> = resolved.required_env.iter().cloned().collect();
    for step in &resolved.steps {
        keyed.extend(step.env.keys().cloned());
        keyed.extend(step.required_env.iter().cloned());
        keyed.extend(step.declared_env.iter().cloned());
        if let Some(cache) = &step.cache {
            keyed.extend(cache.env.iter().cloned());
        }
    }
    for step in &resolved.steps {
        for key in step_refs(step) {
            needed.insert(key.clone());
            let list = users.entry(key).or_default();
            if !list.contains(&step.name) {
                list.push(step.name.clone());
            }
        }
        for implied in implied_refs(step, config, &env) {
            if !implied.optional {
                needed.insert(implied.key.clone());
            }
            purposes
                .entry(implied.key.clone())
                .or_insert(implied.purpose);
            let list = users.entry(implied.key).or_default();
            if !list.contains(&step.name) {
                list.push(step.name.clone());
            }
        }
        // What the step's workflow declares, by name — `required_env` and
        // `env` — is a dependency of each of its steps whether or not a
        // command spells it out: a build tool reads its variables itself.
        env_listed.extend(step.declared_env.iter().cloned());
        for key in step.required_env.iter().chain(&step.declared_env) {
            listed.insert(key.clone());
            let list = users.entry(key.clone()).or_default();
            if !list.contains(&step.name) {
                list.push(step.name.clone());
            }
        }
        for (key, value) in &step.env {
            declared
                .entry(key.clone())
                .or_default()
                .insert(value.clone());
            let list = users.entry(key.clone()).or_default();
            if !list.contains(&step.name) {
                list.push(step.name.clone());
            }
        }
    }

    // The variables worth reporting: everything the run declares, everything it
    // sources, and everything its steps read.
    let mut keys: HashSet<String> = HashSet::new();
    keys.extend(resolved.required_env.iter().cloned());
    keys.extend(sources.keys().cloned());
    keys.extend(users.keys().cloned());
    // Custom arguments are reported whether or not a step reads them: they
    // were typed for this run, and one nothing reads is probably misspelled.
    keys.extend(
        base.keys()
            .filter(|k| k.starts_with(crate::cli::CUSTOM_ARG_PREFIX))
            .cloned(),
    );
    // The `{VAR}` placeholders that decide *which* `.env` file gets sourced are
    // a dependency of the run before any step exists.
    for path in &resolved.env_files {
        keys.extend(placeholder_refs(path));
    }

    let mut vars: Vec<EnvVar> = keys
        .into_iter()
        .map(|key| {
            let step_values = declared.get(&key);
            let source = sources.get(&key).cloned().unwrap_or_default();
            let value = env.get(&key).filter(|v| !v.trim().is_empty()).cloned();
            let terminal = base.get(&key).is_some_and(|v| !v.trim().is_empty());

            // Every step sees one environment, decided by one search: the
            // terminal, else the first env file to set it. A step's own `[env]`
            // is layered on top.
            let mut overrides: Vec<String> = Vec::new();
            let (origin, file, value, varies) = match step_values {
                Some(values) if values.len() > 1 => (Origin::Config, None, None, true),
                Some(values) => (
                    Origin::Config,
                    None,
                    values.iter().next().cloned().filter(|v| !v.is_empty()),
                    false,
                ),
                None if terminal && key.starts_with(crate::cli::CUSTOM_ARG_PREFIX) => {
                    (Origin::Argument, None, value, false)
                }
                None if terminal => {
                    overrides.extend(source.files.iter().cloned());
                    (Origin::Environment, None, value, false)
                }
                None => match source.files.split_first() {
                    Some((winner, rest)) => {
                        overrides.extend(rest.iter().cloned());
                        let origin = match source.kind {
                            Some(FileKind::Default) => Origin::Default,
                            _ => Origin::EnvFile,
                        };
                        (origin, Some(winner.clone()), value, false)
                    }
                    None => (Origin::Unset, None, None, false),
                },
            };

            let via = match origin {
                Origin::EnvFile | Origin::Default => source.kind,
                _ => None,
            };
            let reason = reason(
                origin,
                via,
                file.as_deref(),
                &overrides,
                profile.as_deref(),
                files.len(),
            );

            let secret = value.is_some() && looks_secret(&key);
            let required = resolved.required_env.contains(&key);
            EnvVar {
                via,
                reason,
                in_cache_key: keyed.contains(&key),
                purpose: purposes.get(&key).cloned(),
                // A variable a workflow's `env` lists may be unset by design;
                // only `required_env` says otherwise.
                optional: (env_listed.contains(&key) && !required)
                    || purposes.contains_key(&key)
                        && !listed.contains(&key)
                        && !needed.contains(&key)
                        && !required
                        && !declared.contains_key(&key),
                value: value.map(|v| if secret { mask(&v) } else { v }),
                secret,
                required,
                declared: env_listed.contains(&key) && !required,
                origin,
                file,
                overrides,
                steps: users.get(&key).cloned().unwrap_or_default(),
                varies,
                key,
            }
        })
        .collect();

    // Required variables lead: they're the ones that stop a run, and the ones
    // an operator is looking for when they read this at all.
    vars.sort_by(|a, b| b.required.cmp(&a.required).then_with(|| a.key.cmp(&b.key)));

    EnvReport {
        files,
        required: resolved.required_env.clone(),
        missing,
        vars,
        profile,
        profile_reason: Some(profile_reason),
    }
}

/// Why a variable has the value it has, as a sentence: what supplied it, why
/// that source comes first, and what it overrode.
fn reason(
    origin: Origin,
    via: Option<FileKind>,
    file: Option<&str>,
    overrides: &[String],
    profile: Option<&str>,
    searched: usize,
) -> Option<String> {
    let file = file.unwrap_or("an env file");
    let why = match origin {
        Origin::Environment => "set in the terminal, which always wins".to_string(),
        Origin::Config => {
            "set by value in its workspace's ciabatta.yaml (`workspace.env`, deprecated: move it \
             to the .env) — on top of the run's environment"
                .to_string()
        }
        Origin::Argument => return None,
        Origin::Unset => {
            return Some(format!(
                "not set in the terminal or in any of the {searched} env file(s) searched"
            ));
        }
        Origin::EnvFile | Origin::Default => match via {
            Some(FileKind::Workflow) => format!(
                "{file} is named by the workflow (env_file), which is searched before every \
                 workspace's files"
            ),
            Some(FileKind::Profile) => format!(
                "{file} is the elected profile ({}), copied into its workspace's .env and \
                 searched in its place",
                profile.unwrap_or("?")
            ),
            Some(FileKind::Default) => {
                format!("{file} is a defaults file, and nothing searched before it sets this")
            }
            _ => format!(
                "{file} is the first .env in the search (from where the run started, up to \
                 the root) to set it"
            ),
        },
    };
    Some(match overrides.is_empty() {
        true => why,
        false => format!("{why}; it overrides {}", overrides.join(", ")),
    })
}

/// A variable a step needs that its own text doesn't mention.
pub struct Implied {
    pub key: String,
    pub purpose: String,
    pub optional: bool,
}

/// What a step needs from the environment beyond what [`step_refs`] can see.
///
/// A push or pull step with no command of its own runs the built-in registry
/// transfer, and that reads variables the workflow file never names: the
/// credentials the registry is configured to use, and the `{CIABATTA_*}`
/// placeholders in where it publishes to. A step that only says
/// `registry: nexus` used to show no dependencies at all, and then failed on
/// one.
pub fn implied_refs(
    step: &RunStep,
    config: Option<&CiabattaConfig>,
    env: &HashMap<String, String>,
) -> Vec<Implied> {
    let Some(transfer) = step.transfer() else {
        return Vec::new();
    };
    // A transfer step that names a `run`/`script` runs that instead of the
    // built-in move, and that command's references are already counted.
    if step.run.is_some() || step.script.is_some() {
        return Vec::new();
    }

    let mut out: Vec<Implied> = Vec::new();
    let mut add = |key: String, purpose: String, optional: bool| {
        if !out.iter().any(|i| i.key == key) {
            out.push(Implied {
                key,
                purpose,
                optional,
            });
        }
    };

    let direction = transfer.direction.label();
    match transfer.publish_path {
        Some(crate::config::PublishPath::Single(path)) => {
            for key in placeholder_refs(path) {
                add(key, format!("substituted into the {direction} path"), false);
            }
        }
        // Each glob is uploaded under `{CIABATTA_PATH}`.
        Some(crate::config::PublishPath::Many(_)) => add(
            "CIABATTA_PATH".into(),
            format!("where each file is {direction}ed under"),
            false,
        ),
        None => {}
    }
    for text in [transfer.artifact, transfer.local_image]
        .into_iter()
        .flatten()
    {
        for key in placeholder_refs(text) {
            add(
                key,
                format!("substituted into the {direction}ed artifact"),
                false,
            );
        }
    }

    if let Some(name) = transfer.registry {
        let registry = config.and_then(|c| c.registries.get(name));
        for var in crate::registry::credential_env(name, registry, env) {
            add(var.key, var.purpose, var.optional);
        }
    }
    out
}

/// Every variable one step reads: in its command, the script path, its working
/// directory, its `when` / `skip_if` conditions, and the values of its own
/// `[env]` table (which may themselves interpolate).
pub fn step_refs(step: &RunStep) -> Vec<String> {
    let mut keys: Vec<String> = Vec::new();
    let mut push = |text: &str| {
        for key in shell_refs(text) {
            if !keys.contains(&key) {
                keys.push(key);
            }
        }
    };

    if let Some(run) = step.run.as_deref() {
        push(run);
    }
    if let Some(script) = step.script.as_deref() {
        push(script);
    }
    if let Some(cwd) = step.cwd.as_deref() {
        push(cwd);
    }
    for value in step.env.values() {
        push(value);
    }
    for option in &step.options {
        if let Some(run) = option.run.as_deref() {
            push(run);
        }
        if let Some(script) = option.script.as_deref() {
            push(script);
        }
    }
    // Conditions name a variable directly (`RUN_ENV == prod`) rather than
    // interpolating it, so they're read the way the evaluator reads them.
    for cond in step.when.iter().chain(step.skip_if.iter()) {
        if let Some(key) = condition_ref(cond)
            && !keys.contains(&key)
        {
            keys.push(key);
        }
    }
    keys
}

/// The `$VAR` / `${VAR}` / `${VAR:-default}` references in a shell fragment.
fn shell_refs(text: &str) -> Vec<String> {
    static PATTERN: &str = r"\$\{([A-Za-z_][A-Za-z0-9_]*)[^}]*\}|\$([A-Za-z_][A-Za-z0-9_]*)";
    let re = regex::Regex::new(PATTERN).expect("shell variable pattern compiles");
    re.captures_iter(text)
        .filter_map(|caps| caps.get(1).or_else(|| caps.get(2)))
        .map(|m| m.as_str().to_string())
        .collect()
}

/// The `{VAR}` placeholders in a config path — how `env_file = ".env.{STAGE}"`
/// picks which file to source.
fn placeholder_refs(text: &str) -> Vec<String> {
    let re = regex::Regex::new(r"\{([A-Za-z_][A-Za-z0-9_]*)\}").expect("placeholder pattern");
    re.captures_iter(text)
        .map(|caps| caps[1].to_string())
        .collect()
}

/// The variable a step condition tests, if it names one.
fn condition_ref(cond: &str) -> Option<String> {
    let lhs = cond
        .split("!=")
        .next()
        .and_then(|s| s.split("==").next())
        .unwrap_or(cond)
        .trim()
        .trim_start_matches('!')
        .trim();
    let name = lhs.strip_prefix("env.").unwrap_or(lhs);
    let valid = !name.is_empty()
        && name.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    valid.then(|| name.to_string())
}

/// Whether a variable's name says its value shouldn't be printed.
///
/// Names, not values: a run report is read in terminals and pasted into CI
/// logs, and the cost of masking a variable that turns out to be harmless is
/// far below the cost of printing one that isn't.
pub fn looks_secret(key: &str) -> bool {
    let key = key.to_ascii_uppercase();
    const NEEDLES: [&str; 9] = [
        "SECRET",
        "TOKEN",
        "PASSWORD",
        "PASSWD",
        "CREDENTIAL",
        "PRIVATE",
        "APIKEY",
        "AUTH",
        // A webhook URL carries its own credential in the path: whoever has
        // it can post as you.
        "WEBHOOK",
    ];
    // `_PASS` and `_PWD` as suffixes only: as substrings they'd catch
    // `BYPASS_CACHE` and `PWD`, which are neither.
    NEEDLES.iter().any(|needle| key.contains(needle))
        || key.ends_with("_PASS")
        || key.ends_with("_PWD")
        || key.ends_with("_KEY")
        || key == "KEY"
        || key == "PASS"
}

/// A masked stand-in for a secret's value.
fn mask(_value: &str) -> String {
    "••••••••".to_string()
}

/// A value as it may be shown: itself, or a mask when the name says it's a
/// secret. Everything that puts a declared value in front of someone — the
/// report, a step's own `[env]` table in the web app — goes through here.
pub fn shown(key: &str, value: &str) -> String {
    if looks_secret(key) && !value.is_empty() {
        mask(value)
    } else {
        value.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::run::RunStep;

    fn step(name: &str, run: &str) -> RunStep {
        RunStep {
            name: name.to_string(),
            run: Some(run.to_string()),
            ..Default::default()
        }
    }

    fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    fn find<'a>(report: &'a EnvReport, key: &str) -> &'a EnvVar {
        report
            .vars
            .iter()
            .find(|v| v.key == key)
            .unwrap_or_else(|| panic!("no {key} in the report: {:?}", report.vars))
    }

    #[test]
    fn shell_references_are_found_in_every_form() {
        assert_eq!(
            shell_refs("deploy --region $AWS_REGION --stage ${STAGE} --to ${TARGET:-dev}"),
            vec!["AWS_REGION", "STAGE", "TARGET"]
        );
        // Positional and special parameters aren't variables anyone configures.
        assert!(shell_refs("echo $1 $@ $?").is_empty());
    }

    #[test]
    fn a_step_declares_what_it_reads() {
        let mut s = step("build", "make -j$JOBS");
        s.cwd = Some("packages/$PKG".into());
        s.when = vec!["env.RUN_ENV == prod".into()];
        s.env
            .insert("PROFILE".to_string(), "$BUILD_PROFILE".to_string());

        let refs = step_refs(&s);
        for key in ["JOBS", "PKG", "RUN_ENV", "BUILD_PROFILE"] {
            assert!(
                refs.contains(&key.to_string()),
                "{key} missing from {refs:?}"
            );
        }
    }

    #[test]
    fn values_are_reported_with_where_they_came_from() {
        let resolved = ResolvedRun {
            required_env: vec!["API_TOKEN".to_string()],
            steps: vec![step("build", "deploy --region $AWS_REGION")],
            ..Default::default()
        };
        let base = env(&[("AWS_REGION", "eu-west-1"), ("API_TOKEN", "s3cret")]);
        let report = collect(&resolved, Path::new("."), &base, None);

        let region = find(&report, "AWS_REGION");
        assert_eq!(region.value.as_deref(), Some("eu-west-1"));
        assert_eq!(region.origin, Origin::Environment);
        assert_eq!(region.steps, vec!["build".to_string()]);

        // A secret's presence is reported; its value is not.
        let token = find(&report, "API_TOKEN");
        assert!(token.required && token.secret);
        assert_ne!(token.value.as_deref(), Some("s3cret"));
        assert!(report.missing.is_empty());
    }

    #[test]
    fn an_unset_required_variable_is_listed_as_missing() {
        let resolved = ResolvedRun {
            required_env: vec!["STAGE".to_string()],
            steps: vec![step("build", "true")],
            ..Default::default()
        };
        let report = collect(&resolved, Path::new("."), &env(&[]), None);

        let stage = find(&report, "STAGE");
        assert_eq!(stage.origin, Origin::Unset);
        assert!(stage.value.is_none());
        assert_eq!(report.missing, vec!["STAGE".to_string()]);
        // Required variables lead the list.
        assert_eq!(report.vars[0].key, "STAGE");
    }

    #[test]
    fn a_step_env_table_is_a_configured_dependency() {
        let mut first = step("a", "true");
        first.env.insert("PROFILE".into(), "release".into());
        let mut second = step("b", "true");
        second.env.insert("PROFILE".into(), "debug".into());

        let one = collect(
            &ResolvedRun {
                steps: vec![first.clone()],
                ..Default::default()
            },
            Path::new("."),
            &env(&[]),
            None,
        );
        let profile = find(&one, "PROFILE");
        assert_eq!(profile.origin, Origin::Config);
        assert_eq!(profile.value.as_deref(), Some("release"));

        // Two steps setting it differently have no single value to show.
        let both = collect(
            &ResolvedRun {
                steps: vec![first, second],
                ..Default::default()
            },
            Path::new("."),
            &env(&[]),
            None,
        );
        let profile = find(&both, "PROFILE");
        assert!(profile.varies && profile.value.is_none());
        assert_eq!(profile.steps, vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn env_files_are_sourced_and_credited() {
        let root = std::env::temp_dir().join(format!("ciab_envdeps_{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join(".env"), "DATABASE_URL=postgres://local\n").unwrap();

        let resolved = ResolvedRun {
            env_files: vec![".env".to_string()],
            steps: vec![step("migrate", "psql $DATABASE_URL")],
            ..Default::default()
        };
        let report = collect(&resolved, &root, &env(&[]), None);

        let url = find(&report, "DATABASE_URL");
        assert_eq!(url.origin, Origin::EnvFile);
        assert_eq!(url.file.as_deref(), Some(".env"));
        assert_eq!(url.value.as_deref(), Some("postgres://local"));
        assert_eq!(report.files, vec![".env".to_string()]);

        std::fs::remove_dir_all(&root).ok();
    }

    /// The Padua case: a `required_env` no step's command mentions, set only
    /// in the root `.env`, used to be reported unset. The report also says
    /// how each value was decided.
    #[test]
    fn a_required_variable_nobody_reads_is_found_in_the_root_env() {
        let root = std::env::temp_dir().join(format!("ciab_envdeps_padua_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join(".ciabatta")).unwrap();
        std::fs::write(root.join(".env"), "VERSION=0.0.0\n").unwrap();
        std::fs::write(
            root.join(".ciabatta/.env.default"),
            "VERSION=9.9.9\nTARGET=dev\n",
        )
        .unwrap();

        let resolved = ResolvedRun {
            required_env: vec!["VERSION".to_string()],
            steps: vec![step("build", "make")],
            ..Default::default()
        };
        let config = CiabattaConfig {
            workspace: Some(crate::workspace::WorkspaceMeta {
                env_default: Some(".ciabatta/.env.default".into()),
                ..Default::default()
            }),
            ..Default::default()
        };
        let report = collect(&resolved, &root, &env(&[]), Some(&config));

        let version = find(&report, "VERSION");
        assert_eq!(version.value.as_deref(), Some("0.0.0"));
        assert_eq!(version.origin, Origin::EnvFile);
        assert_eq!(version.file.as_deref(), Some(".env"));
        assert_eq!(version.overrides, [".ciabatta/.env.default"]);
        assert!(report.missing.is_empty());

        let target = find(&report, "TARGET");
        assert_eq!(target.origin, Origin::Default);
        assert_eq!(target.value.as_deref(), Some("dev"));

        let text = report.render("build-checks", false).unwrap();
        assert!(
            text.contains(
                "precedence: terminal > .env > .ciabatta/.env.default (first to set it wins)"
            ),
            "{text}"
        );
        // Grouped under a header per source, in search order; each row keeps
        // only what's its own.
        let env_at = text
            .find("  .env — the first .env in the search")
            .expect(&text);
        let default_at = text
            .find("  .ciabatta/.env.default (defaults) — nothing searched before it")
            .expect(&text);
        assert!(env_at < default_at, "{text}");
        assert!(
            text.contains("overrides .ciabatta/.env.default · required_env"),
            "{text}"
        );
        assert!(text.contains("    TARGET   dev\n"), "{text}");
        assert!(!text.contains('\u{1b}'), "no colour asked for, none given");
        assert!(
            report
                .render("build-checks", true)
                .unwrap()
                .contains('\u{1b}')
        );

        // The terminal beats every file, and the report says which it beat.
        let report = collect(
            &resolved,
            &root,
            &env(&[("VERSION", "1.0.0")]),
            Some(&config),
        );
        let version = find(&report, "VERSION");
        assert_eq!(version.origin, Origin::Environment);
        assert_eq!(version.overrides, [".env", ".ciabatta/.env.default"]);

        std::fs::remove_dir_all(&root).ok();
    }

    /// Every value carries the reason it won, and the report names the
    /// profile and why it's the one in use.
    #[test]
    fn each_value_and_the_profile_come_with_a_reason() {
        let root = std::env::temp_dir().join(format!("ciab_envdeps_reason_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join(".ciabatta")).unwrap();
        std::fs::write(root.join(".env"), "REGION=us\n").unwrap();
        std::fs::write(root.join(".ciabatta/.env.staging"), "REGION=eu\n").unwrap();

        let resolved = ResolvedRun {
            required_env: vec!["MISSING".to_string()],
            steps: vec![step("build", "deploy $REGION")],
            ..Default::default()
        };
        use crate::environment::profiles::{PROFILE_FROM_VAR, PROFILE_VAR};
        let base = env(&[(PROFILE_VAR, "staging"), (PROFILE_FROM_VAR, "flag")]);
        let report = collect(&resolved, &root, &base, None);

        assert_eq!(report.profile.as_deref(), Some("staging"));
        let why = report.profile_reason.clone().unwrap();
        assert!(why.contains("--env-profile staging"), "{why}");

        let region = find(&report, "REGION");
        assert_eq!(region.via, Some(FileKind::Profile));
        let reason = region.reason.clone().unwrap();
        assert!(
            reason.contains(".ciabatta/.env.staging is the elected profile (staging)"),
            "{reason}"
        );
        let missing = find(&report, "MISSING").reason.clone().unwrap();
        assert!(missing.contains("not set in the terminal"), "{missing}");

        // The terminal's own profile, and no profile at all, say so too.
        let report = collect(&resolved, &root, &env(&[(PROFILE_VAR, "staging")]), None);
        assert!(report.profile_reason.unwrap().contains("in the terminal"));
        let report = collect(&resolved, &root, &env(&[]), None);
        assert!(report.profile.is_none());
        assert!(
            report
                .profile_reason
                .as_deref()
                .unwrap()
                .starts_with("none")
        );
        let text = report.render("build", false).unwrap();
        assert!(
            text.contains("unset — not set in the terminal or in any of the 1 env file(s)"),
            "{text}"
        );

        // After `ciabatta env use staging`, a run with no profile asked for
        // still says which profile .env is a copy of.
        crate::environment::profiles::apply(&root, "staging", false).unwrap();
        let report = collect(&resolved, &root, &env(&[]), None);
        assert_eq!(report.profile.as_deref(), Some("staging"));
        let why = report.profile_reason.clone().unwrap();
        assert!(
            why.contains(".env is a copy of .ciabatta/.env.staging"),
            "{why}"
        );
        assert_eq!(find(&report, "REGION").value.as_deref(), Some("eu"));

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn secret_looking_names_are_masked() {
        for key in [
            "API_TOKEN",
            "AWS_SECRET_ACCESS_KEY",
            "CIABATTA_NEXUS_PASS",
            "db_password",
            "TF_VAR_discord_webhook",
        ] {
            assert!(looks_secret(key), "{key} should be treated as a secret");
        }
        for key in ["AWS_REGION", "STAGE", "CIABATTA_BRANCH", "PASSENGERS"] {
            assert!(!looks_secret(key), "{key} should not be masked");
        }
    }

    fn push(registry: &str, publish_path: &str) -> RunStep {
        RunStep {
            name: "publish".into(),
            kind: Some("push".into()),
            registry: Some(registry.into()),
            artifact: Some("dist/app.tgz".into()),
            publish_path: Some(crate::config::PublishPath::Single(publish_path.into())),
            ..Default::default()
        }
    }

    #[test]
    fn a_push_depends_on_its_registry_credentials_and_path_placeholders() {
        let resolved = ResolvedRun {
            steps: vec![push("nexus", "app/{CIABATTA_COMMIT}/app.tgz")],
            ..Default::default()
        };
        let base = env(&[("CIABATTA_COMMIT", "abc123"), ("CIABATTA_NEXUS_USER", "ci")]);
        let report = collect(&resolved, Path::new("."), &base, None);

        let commit = find(&report, "CIABATTA_COMMIT");
        assert_eq!(commit.steps, vec!["publish".to_string()]);
        assert!(!commit.optional, "the path can't be built without it");

        let user = find(&report, "CIABATTA_NEXUS_USER");
        assert_eq!(user.value.as_deref(), Some("ci"));
        assert!(user.purpose.as_deref().unwrap().contains("nexus"));

        // Unset, but the push goes ahead unauthenticated: a fact, not a fault.
        let pass = find(&report, "CIABATTA_NEXUS_PASS");
        assert_eq!(pass.origin, Origin::Unset);
        assert!(pass.optional);
    }

    #[test]
    fn a_registry_that_needs_auth_makes_its_credentials_required() {
        let mut config = CiabattaConfig::default();
        config.registries.insert(
            "artifacts".into(),
            crate::config::RegistryConfig {
                url: "https://repo.example".into(),
                tls_verify: true,
                needs_auth: true,
                login_script: None,
                registry_type: Some("nexus".into()),
                repository: None,
                base_path: None,
                format: None,
            },
        );
        let resolved = ResolvedRun {
            steps: vec![push("artifacts", "app.tgz")],
            ..Default::default()
        };
        let report = collect(&resolved, Path::new("."), &env(&[]), Some(&config));
        assert!(!find(&report, "CIABATTA_ARTIFACTS_PASS").optional);
    }

    #[test]
    fn an_aws_registry_shows_only_the_credential_route_in_use() {
        let resolved = ResolvedRun {
            steps: vec![push("s3-releases", "app.tgz")],
            ..Default::default()
        };
        let report = collect(
            &resolved,
            Path::new("."),
            &env(&[("AWS_PROFILE", "ci"), ("AWS_DEFAULT_REGION", "eu-west-1")]),
            None,
        );
        assert_eq!(find(&report, "AWS_PROFILE").value.as_deref(), Some("ci"));
        assert!(find(&report, "AWS_DEFAULT_REGION").optional);
        assert!(report.vars.iter().all(|v| v.key != "AWS_ACCESS_KEY_ID"));
    }

    #[test]
    fn a_push_with_its_own_command_is_judged_by_that_command() {
        let mut step = push("nexus", "app/{CIABATTA_COMMIT}");
        step.run = Some("./publish.sh $RELEASE_CHANNEL".into());
        let resolved = ResolvedRun {
            steps: vec![step],
            ..Default::default()
        };
        let report = collect(&resolved, Path::new("."), &env(&[]), None);
        find(&report, "RELEASE_CHANNEL");
        assert!(
            report
                .vars
                .iter()
                .all(|v| !v.key.starts_with("CIABATTA_NEXUS"))
        );
    }
}
