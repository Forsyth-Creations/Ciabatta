//! What a run depends on, environment-wise.
//!
//! A run's steps are shell scripts, and the thing that most often makes one
//! behave differently on two machines is not the graph — it's an environment
//! variable that was set here and isn't there. The graph is drawn before it
//! runs; this does the same for the environment, so `REQUIRED_ENV`, the
//! `.env` files a run sources, the `[env]` tables that cascade from
//! sub-workspace to step, and the bare `$VAR` references inside the commands
//! are all one list with values attached.
//!
//! One collector serves both surfaces: the terminal prints it before a run
//! starts, and the daemon serves it so the web app can draw each variable as
//! what it actually is — a dependency of the steps that read it.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;

use serde::Serialize;

use super::{ResolvedRun, RunStep, parse_env_content, prepare_env};

/// Where a variable's effective value comes from.
#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    /// The environment ciabatta itself was launched with — your shell, the CI
    /// system, or a `-e` flag. Beats every declared source.
    Environment,
    /// A `.env` file the run sources (see `file`).
    EnvFile,
    /// A declared `[env]` table: a sub-workspace's, a workflow's, or a step's.
    Config,
    /// Nothing supplies it. Steps reading it will see an empty string.
    Unset,
}

impl Origin {
    /// A short word for the terminal report.
    pub fn label(self) -> &'static str {
        match self {
            Origin::Environment => "environment",
            Origin::EnvFile => "env file",
            Origin::Config => "config",
            Origin::Unset => "unset",
        }
    }
}

/// One environment variable a run depends on.
#[derive(Serialize, Clone, Debug)]
pub struct EnvVar {
    pub key: String,
    /// The value the steps will see, masked when the name says it's a secret.
    /// `None` when nothing supplies it, or when steps disagree (`varies`).
    pub value: Option<String>,
    /// Whether `value` was masked rather than printed.
    pub secret: bool,
    /// Declared in `REQUIRED_ENV`: the run refuses to start without it.
    pub required: bool,
    pub origin: Origin,
    /// The `.env` file that supplied it, when `origin` is `EnvFile`.
    pub file: Option<String>,
    /// Steps that read it or declare it — the edges to draw on a graph.
    pub steps: Vec<String>,
    /// Set by several steps to different values, so there is no single one.
    pub varies: bool,
}

/// Every environment variable a run depends on, resolved as far as the inputs
/// allow.
#[derive(Serialize, Clone, Debug, Default)]
pub struct EnvReport {
    /// The `.env` files sourced, in application order.
    pub files: Vec<String>,
    /// `REQUIRED_ENV`, as declared.
    pub required: Vec<String>,
    /// Required variables that are still empty or unset — the run can't start
    /// until they are supplied.
    pub missing: Vec<String>,
    /// Every variable, required ones first, then by name.
    pub vars: Vec<EnvVar>,
}

impl EnvReport {
    pub fn is_empty(&self) -> bool {
        self.vars.is_empty()
    }

    /// The report as a block of terminal text, or `None` when a run depends on
    /// no variables at all (in which case there is nothing worth saying).
    pub fn render(&self, workflow: &str) -> Option<String> {
        if self.is_empty() {
            return None;
        }

        let mut out = String::new();
        out.push_str(&format!(
            "Environment for '{workflow}' — {} variable(s) this run depends on\n",
            self.vars.len()
        ));
        if !self.files.is_empty() {
            out.push_str(&format!("  sourcing {}\n", self.files.join(", ")));
        }

        let rows: Vec<(&EnvVar, String)> = self
            .vars
            .iter()
            .map(|var| {
                let value = match (&var.value, var.varies) {
                    (Some(value), _) => value.clone(),
                    (None, true) => "(set per step)".to_string(),
                    (None, false) => "(unset)".to_string(),
                };
                (var, value)
            })
            .collect();

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

        for (var, value) in &rows {
            let mut notes: Vec<String> = vec![var.origin.label().to_string()];
            if let Some(file) = &var.file {
                notes.push(file.clone());
            }
            if var.required {
                notes.push("REQUIRED_ENV".to_string());
            }
            if !var.steps.is_empty() {
                notes.push(format!("used by {}", var.steps.join(", ")));
            }
            out.push_str(&format!(
                "  {:width$}  {:value_width$}  [{}]\n",
                var.key,
                value,
                notes.join(" · "),
                width = width,
                value_width = value_width
            ));
        }

        if !self.missing.is_empty() {
            out.push_str(&format!(
                "  ✗ still needed: {} — set them with -e KEY=VALUE\n",
                self.missing.join(", ")
            ));
        }
        Some(out)
    }
}

/// Collect everything a resolved run depends on, environment-wise.
///
/// `base` is what the run starts from — ciabatta's own environment plus
/// whatever CI, git, and `-e` resolved. Never fails: an unreadable `.env` file
/// (which the engine reports properly a moment later) degrades the report
/// rather than replacing the real error with one about drawing a table.
pub fn collect(resolved: &ResolvedRun, root: &Path, base: &HashMap<String, String>) -> EnvReport {
    // The engine's own resolution, so the values reported are the values the
    // steps will actually see.
    let prepared = prepare_env(resolved, root, base).ok();
    let env = prepared
        .as_ref()
        .map(|p| p.env.clone())
        .unwrap_or_else(|| base.clone());
    let files = prepared
        .as_ref()
        .map(|p| p.all_sourced())
        .unwrap_or_default();
    let missing = prepared.as_ref().map(|p| p.missing()).unwrap_or_default();

    // Which `.env` file last defined each key, and to what. Read once per file,
    // then consulted per scope: the run's own chain, and each step's.
    let mut parsed: HashMap<String, Vec<(String, String)>> = HashMap::new();
    for rel in &files {
        let Ok(content) = std::fs::read_to_string(root.join(rel)) else {
            continue;
        };
        parsed.insert(rel.clone(), parse_env_content(&content));
    }
    let supplier = |chain: &[String], key: &str| -> Option<(String, String)> {
        // Last file in the chain that defines it — the nearest workspace's,
        // which is the one whose value the step actually sees.
        chain.iter().rev().find_map(|rel| {
            parsed.get(rel).and_then(|pairs| {
                pairs
                    .iter()
                    .rev()
                    .find(|(k, _)| k == key)
                    .map(|(_, v)| (rel.clone(), v.clone()))
            })
        })
    };

    let run_chain = prepared
        .as_ref()
        .map(|p| p.sourced.clone())
        .unwrap_or_default();
    // The scope each step resolves in: its own chain when it has one, the
    // run's otherwise.
    let scope_of = |step: &str| -> (Vec<String>, HashMap<String, String>) {
        match prepared.as_ref() {
            Some(p) if !p.files_for(step).is_empty() => {
                (p.files_for(step).to_vec(), p.for_step(step).clone())
            }
            _ => (run_chain.clone(), env.clone()),
        }
    };

    // Which steps touch which variable, and what a step's own `[env]` sets it
    // to. `BTreeMap`s keep both the variable list and each variable's step list
    // stable, so the report doesn't reshuffle between two identical runs.
    let mut users: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut declared: BTreeMap<String, HashSet<String>> = BTreeMap::new();
    for step in &resolved.steps {
        for key in step_refs(step) {
            let list = users.entry(key).or_default();
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
    for pairs in parsed.values() {
        keys.extend(pairs.iter().map(|(key, _)| key.clone()));
    }
    keys.extend(users.keys().cloned());
    // The `{VAR}` placeholders that decide *which* `.env` file gets sourced are
    // a dependency of the run before any step exists.
    for path in &resolved.env_files {
        keys.extend(placeholder_refs(path));
    }

    let mut vars: Vec<EnvVar> = keys
        .into_iter()
        .map(|key| {
            let step_values = declared.get(&key);

            // What the steps that read this variable actually see. With `.env`
            // files resolved by proximity, two steps can read the same name and
            // get different values — so the scopes are asked one by one rather
            // than one run-wide map being taken as the answer.
            let readers = users.get(&key).cloned().unwrap_or_default();
            let mut scopes: Vec<(Vec<String>, HashMap<String, String>)> =
                readers.iter().map(|step| scope_of(step)).collect();
            if scopes.is_empty() {
                scopes.push((run_chain.clone(), env.clone()));
            }

            let mut seen: Vec<(Option<String>, Option<String>)> = Vec::new();
            for (chain, resolved_env) in &scopes {
                let value = resolved_env
                    .get(&key)
                    .filter(|v| !v.trim().is_empty())
                    .cloned();
                // A file only supplied the value if `base` didn't already have
                // one — a sourced file never overrides the real environment.
                let file = match base.get(&key) {
                    Some(v) if !v.trim().is_empty() => None,
                    _ => supplier(chain, &key).map(|(file, _)| file),
                };
                let entry = (value, file);
                if !seen.contains(&entry) {
                    seen.push(entry);
                }
            }

            let disagree = seen.len() > 1;
            let (ambient, from_file) = seen.into_iter().next().unwrap_or((None, None));

            // Precedence mirrors the engine: what the step's own chain resolved
            // to is what it sees, unless the step declares its own value, which
            // is layered on top.
            let (origin, file, value, varies) = match (ambient, step_values) {
                (_, Some(values)) if values.len() > 1 => (Origin::Config, None, None, true),
                (_, Some(values)) => (
                    Origin::Config,
                    None,
                    values.iter().next().cloned().filter(|v| !v.is_empty()),
                    false,
                ),
                // The readers disagree: there is no single value to report, and
                // claiming one would be worse than saying so.
                (_, None) if disagree => (Origin::EnvFile, None, None, true),
                (Some(value), None) => match from_file {
                    Some(file) => (Origin::EnvFile, Some(file), Some(value), false),
                    None => (Origin::Environment, None, Some(value), false),
                },
                (None, None) => (Origin::Unset, None, None, false),
            };

            let secret = value.is_some() && looks_secret(&key);
            EnvVar {
                value: value.map(|v| if secret { mask(&v) } else { v }),
                secret,
                required: resolved.required_env.contains(&key),
                origin,
                file,
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
    }
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
    const NEEDLES: [&str; 8] = [
        "SECRET",
        "TOKEN",
        "PASSWORD",
        "PASSWD",
        "CREDENTIAL",
        "PRIVATE",
        "APIKEY",
        "AUTH",
    ];
    NEEDLES.iter().any(|needle| key.contains(needle))
        || key.ends_with("_KEY")
        || key.ends_with("_PASS")
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
        let report = collect(&resolved, Path::new("."), &base);

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
        let report = collect(&resolved, Path::new("."), &env(&[]));

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
        let report = collect(&resolved, &root, &env(&[]));

        let url = find(&report, "DATABASE_URL");
        assert_eq!(url.origin, Origin::EnvFile);
        assert_eq!(url.file.as_deref(), Some(".env"));
        assert_eq!(url.value.as_deref(), Some("postgres://local"));
        assert_eq!(report.files, vec![".env".to_string()]);

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn secret_looking_names_are_masked() {
        for key in [
            "API_TOKEN",
            "AWS_SECRET_ACCESS_KEY",
            "CIABATTA_NEXUS_PASS",
            "db_password",
        ] {
            assert!(looks_secret(key), "{key} should be treated as a secret");
        }
        for key in ["AWS_REGION", "STAGE", "CIABATTA_BRANCH", "PASSENGERS"] {
            assert!(!looks_secret(key), "{key} should not be masked");
        }
    }
}
