use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub const CIABATTA_DIR: &str = ".ciabatta";
pub const CONFIG_FILE: &str = "ciabatta.toml";

#[derive(Debug, Deserialize, Serialize, Clone, Default)]
pub struct CiabattaConfig {
    pub system: Option<SystemConfig>,
    #[serde(default)]
    pub registries: HashMap<String, RegistryConfig>,
    #[serde(rename = "recipies", default)]
    pub recipes: HashMap<String, RecipeEntry>,
    pub analyze: Option<AnalyzeConfig>,
}

#[derive(Debug, Deserialize, Serialize, Clone, Default)]
pub struct SystemConfig {
    pub ci: Option<String>,
    /// Container runtime (`docker` or `podman`). When unset, ciabatta auto-detects
    /// what's installed at run time (see [`resolve_container_cmd`]).
    pub containers: Option<String>,
}

/// Optional inputs for `ciabatta analyze` (paths relative to the project root).
#[derive(Debug, Deserialize, Serialize, Clone, Default)]
pub struct AnalyzeConfig {
    /// A file listing project requirements, one per line (`id` or `id, description`).
    pub requirements: Option<String>,
    /// A CSV tracing requirements to source files (columns: requirement, file).
    pub trace: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct RegistryConfig {
    pub url: String,
    #[serde(default = "default_true")]
    pub tls_verify: bool,
    #[serde(default)]
    pub needs_auth: bool,
    pub login_script: Option<String>,
    /// Optional explicit type; inferred from registry name if absent.
    #[serde(rename = "type")]
    pub registry_type: Option<String>,
}

fn default_true() -> bool {
    true
}

/// A recipe: shared fields at the top level, with optional `push` / `pull`
/// sub-tables that override those fields for one direction.
///
/// ```toml
/// [recipies.frontend]
/// registry = "nexus"            # shared by push and pull
/// publish_path = "front/{CIABATTA_COMMIT}/dist"
///
///   [recipies.frontend.push]    # push-only stage overrides
///   pre  = "python bundle.py"
///   post = "./notify.sh"
/// ```
#[derive(Debug, Deserialize, Serialize, Clone, Default)]
pub struct RecipeEntry {
    /// Fields shared across both directions.
    #[serde(flatten)]
    pub base: SimpleRecipe,
    /// Push-direction overrides (any field set here wins over `base`).
    pub push: Option<SimpleRecipe>,
    /// Pull-direction overrides (any field set here wins over `base`).
    pub pull: Option<SimpleRecipe>,
}

/// Where a recipe publishes to. Either a single remote path (the classic form,
/// supporting `{CIABATTA_*}` substitution) or a list of local file globs whose
/// matched files are uploaded under `{CIABATTA_PATH}` preserving their relative
/// path (with `strip_prefix` removed from the front).
///
/// ```toml
/// publish_path = "team/app/{CIABATTA_COMMIT}/app.tar.gz"   # single
/// publish_path = ["dist/*.tar.gz", "build/*.bin"]          # list of globs
/// ```
#[derive(Debug, Deserialize, Serialize, Clone, PartialEq)]
#[serde(untagged)]
pub enum PublishPath {
    /// One remote destination path.
    Single(String),
    /// A list of local file globs, each uploaded under `{CIABATTA_PATH}`.
    Many(Vec<String>),
}

impl PublishPath {
    /// A human-readable rendering for display (TUI/config show).
    pub fn display(&self) -> String {
        match self {
            PublishPath::Single(s) => s.clone(),
            PublishPath::Many(v) => v.join(", "),
        }
    }
}

#[derive(Debug, Deserialize, Serialize, Clone, Default)]
pub struct SimpleRecipe {
    /// Named registry from [registries] section.
    pub registry: Option<String>,
    /// Local filesystem path for the artifact.
    pub local_artifact_path: Option<String>,
    /// Destination path in the registry; supports {CIABATTA_*} variable
    /// substitution, or a list of local file globs (see [`PublishPath`]).
    pub publish_path: Option<PublishPath>,
    /// For list-form `publish_path`: a leading path fragment stripped from each
    /// matched file's relative path before it's joined under `{CIABATTA_PATH}`.
    pub strip_prefix: Option<String>,
    /// Path to a bash script to run instead of the built-in registry action.
    /// Legacy alias for the `main` stage (kept for backwards compatibility).
    pub bash_script: Option<String>,

    // ─── Stage overrides ────────────────────────────────────────────────────
    // Each is an arbitrary shell command (bash, python, a compiled binary, …)
    // run via `sh -c` with all CIABATTA_* / CI variables in its environment.
    // When unset, the stage falls back to its built-in default.
    /// Override the `login` stage (default: registry login_script or credentials).
    pub login: Option<String>,
    /// Override the `pre-push` / `pre-pull` stage (default: no-op).
    pub pre: Option<String>,
    /// Override the `push` / `pull` stage (default: the built-in registry action).
    pub main: Option<String>,
    /// Override the `post-push` / `post-pull` stage (default: no-op).
    pub post: Option<String>,
}

impl SimpleRecipe {
    /// Layer `over` on top of `self`: any field set in `over` wins, otherwise
    /// the value from `self` (the shared base) is kept.
    fn overlaid_with(&self, over: &SimpleRecipe) -> SimpleRecipe {
        SimpleRecipe {
            registry: over.registry.clone().or_else(|| self.registry.clone()),
            local_artifact_path: over
                .local_artifact_path
                .clone()
                .or_else(|| self.local_artifact_path.clone()),
            publish_path: over
                .publish_path
                .clone()
                .or_else(|| self.publish_path.clone()),
            strip_prefix: over
                .strip_prefix
                .clone()
                .or_else(|| self.strip_prefix.clone()),
            bash_script: over
                .bash_script
                .clone()
                .or_else(|| self.bash_script.clone()),
            login: over.login.clone().or_else(|| self.login.clone()),
            pre: over.pre.clone().or_else(|| self.pre.clone()),
            main: over.main.clone().or_else(|| self.main.clone()),
            post: over.post.clone().or_else(|| self.post.clone()),
        }
    }
}

impl RecipeEntry {
    /// The effective recipe for the push direction (base + push overrides).
    pub fn push_recipe(&self) -> SimpleRecipe {
        match &self.push {
            Some(over) => self.base.overlaid_with(over),
            None => self.base.clone(),
        }
    }

    /// The effective recipe for the pull direction, if pulling is supported.
    ///
    /// - An explicit `[recipe.pull]` table → base + pull overrides.
    /// - A plain recipe with no push/pull tables → pulls using the shared base.
    /// - A recipe with only a `push` table → no pull action.
    pub fn pull_recipe(&self) -> Option<SimpleRecipe> {
        match (&self.pull, &self.push) {
            (Some(over), _) => Some(self.base.overlaid_with(over)),
            (None, None) => Some(self.base.clone()),
            (None, Some(_)) => None,
        }
    }
}

/// Walk up from `start` until a `.ciabatta` directory is found.
pub fn find_root(start: &Path) -> Option<PathBuf> {
    let mut current = start.to_path_buf();
    loop {
        if current.join(CIABATTA_DIR).is_dir() {
            return Some(current);
        }
        if !current.pop() {
            return None;
        }
    }
}

/// Load the config discovered at `<root>/.ciabatta/ciabatta.toml`. Returns the
/// default (empty) config when that file doesn't exist.
pub fn load_config(root: &Path) -> Result<CiabattaConfig> {
    let path = root.join(CIABATTA_DIR).join(CONFIG_FILE);
    if !path.exists() {
        return Ok(CiabattaConfig::default());
    }
    load_config_file(&path)
}

/// Load and parse a specific config file (used by the `--config` flag),
/// expanding environment references in registry URLs and login scripts. Unlike
/// [`load_config`], a missing or unparseable file is an error — the caller
/// pointed at this file explicitly.
pub fn load_config_file(path: &Path) -> Result<CiabattaConfig> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read {}", path.display()))?;
    let mut config: CiabattaConfig =
        toml::from_str(&content).with_context(|| format!("Failed to parse {}", path.display()))?;

    // Registries may reference environment variables (with bash-style defaults)
    // so the same config can target different endpoints per environment.
    for reg in config.registries.values_mut() {
        reg.url = expand_env(&reg.url);
        if let Some(script) = reg.login_script.take() {
            reg.login_script = Some(expand_env(&script));
        }
    }

    Ok(config)
}

/// Expand shell-style environment references in a config value, supporting the
/// bash default syntax. Recognized forms (a leading `$` is optional):
///
/// ```text
/// ${VAR}            → value of VAR, or empty if unset
/// ${VAR:-default}   → value of VAR if set and non-empty, else `default`
/// ${VAR-default}    → value of VAR if set (even if empty), else `default`
/// {VAR:-default}    → same as ${VAR:-default} (matches the documented syntax)
/// ```
///
/// A bare `{VAR}` with neither a `$` nor a default operator is left untouched, so
/// ordinary braces in a URL are never clobbered.
pub fn expand_env(input: &str) -> String {
    let re = regex::Regex::new(r"(\$?)\{([A-Za-z_][A-Za-z0-9_]*)(?:(:?-)([^}]*))?\}").unwrap();
    re.replace_all(input, |caps: &regex::Captures| {
        let had_dollar = !caps[1].is_empty();
        let name = &caps[2];
        let op = caps.get(3).map(|m| m.as_str());

        // Without a `$` and without a default operator this isn't an env
        // reference — leave the original text in place.
        if !had_dollar && op.is_none() {
            return caps[0].to_string();
        }

        let value = std::env::var(name).ok();
        match op {
            Some(op) => {
                let default = caps.get(4).map(|m| m.as_str()).unwrap_or("");
                let use_default = if op == ":-" {
                    // `:-` falls back when the variable is unset *or* empty.
                    value.as_deref().map(str::is_empty).unwrap_or(true)
                } else {
                    // `-` falls back only when the variable is entirely unset.
                    value.is_none()
                };
                if use_default {
                    default.to_string()
                } else {
                    value.unwrap_or_default()
                }
            }
            None => value.unwrap_or_default(),
        }
    })
    .into_owned()
}

/// Resolve the container runtime command (`docker` or `podman`).
///
/// If `[system].containers` is set in the config, that always wins. Otherwise
/// ciabatta auto-detects what's installed on `PATH`:
///   - both available → ambiguous, the user must pick one (error)
///   - only one       → use it
///   - podman + docker preference order is podman first, then docker
///   - neither        → error
pub fn resolve_container_cmd(config: &CiabattaConfig) -> Result<String> {
    if let Some(c) = config.system.as_ref().and_then(|s| s.containers.as_deref()) {
        let c = c.trim();
        if !c.is_empty() {
            return Ok(c.to_string());
        }
    }

    let podman = binary_on_path("podman");
    let docker = binary_on_path("docker");
    match (podman, docker) {
        (true, true) => bail!(
            "Both podman and docker are installed, so ciabatta can't pick one for you.\n\
             Set the runtime explicitly in .ciabatta/ciabatta.toml:\n\n    \
             [system]\n    containers = \"podman\"   # or \"docker\""
        ),
        (true, false) => Ok("podman".to_string()),
        (false, true) => Ok("docker".to_string()),
        (false, false) => bail!(
            "Neither podman nor docker was found on PATH.\n\
             Install one, or set [system] containers in .ciabatta/ciabatta.toml."
        ),
    }
}

/// Whether an executable named `name` exists on the `PATH`.
fn binary_on_path(name: &str) -> bool {
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|dir| is_executable(&dir.join(name)))
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.metadata()
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}

/// Validate that all `{VAR}` placeholders in a publish path are present in `vars`.
pub fn validate_publish_path(path: &str, vars: &HashMap<String, String>) -> Result<()> {
    let re = regex::Regex::new(r"\{([A-Z_][A-Z0-9_]*)\}").unwrap();
    for cap in re.captures_iter(path) {
        let var_name = &cap[1];
        if !vars.contains_key(var_name) {
            bail!(
                "Variable '{{{}}}' referenced in publish path '{}' is not set. \
                 Pass it with -e {}=<value> or ensure the CI system provides it.",
                var_name,
                path,
                var_name
            );
        }
    }
    Ok(())
}

/// Substitute `{VAR}` placeholders in a string with values from `vars`.
pub fn substitute_vars(template: &str, vars: &HashMap<String, String>) -> Result<String> {
    let re = regex::Regex::new(r"\{([A-Z_][A-Z0-9_]*)\}").unwrap();
    let mut error: Option<String> = None;
    let result = re.replace_all(template, |caps: &regex::Captures| {
        let name = &caps[1];
        match vars.get(name) {
            Some(v) => v.clone(),
            None => {
                error = Some(format!("Variable '{{{}}}' not set", name));
                String::new()
            }
        }
    });
    if let Some(e) = error {
        bail!("{}", e);
    }
    Ok(result.into_owned())
}

/// Infer the registry kind from its name and config.
pub fn infer_registry_kind(name: &str, config: &RegistryConfig) -> RegistryKind {
    if let Some(ref t) = config.registry_type {
        return RegistryKind::from(t.as_str());
    }
    RegistryKind::from(name)
}

#[derive(Debug, Clone, PartialEq)]
pub enum RegistryKind {
    Nexus,
    S3,
    Artifactory,
    Docker,
    Ecr,
    Generic,
}

impl From<&str> for RegistryKind {
    fn from(s: &str) -> Self {
        let lower = s.to_lowercase();
        if lower.contains("nexus") {
            RegistryKind::Nexus
        } else if lower.contains("s3") {
            RegistryKind::S3
        } else if lower.contains("artifactory") {
            RegistryKind::Artifactory
        } else if lower.contains("ecr") {
            RegistryKind::Ecr
        } else if lower.contains("docker") || lower.contains("container") {
            RegistryKind::Docker
        } else {
            RegistryKind::Generic
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &str) -> CiabattaConfig {
        toml::from_str(s).expect("config should parse")
    }

    #[test]
    fn parses_stage_overrides_on_simple_recipe() {
        let cfg = parse(
            r#"
[recipies.frontend]
registry = "nexus"
publish_path = "a/{CIABATTA_COMMIT}/b"
pre = "python prep.py"
post = "./notify.sh"
"#,
        );
        let entry = &cfg.recipes["frontend"];
        assert!(entry.push.is_none() && entry.pull.is_none());
        let r = entry.push_recipe();
        assert_eq!(r.pre.as_deref(), Some("python prep.py"));
        assert_eq!(r.post.as_deref(), Some("./notify.sh"));
        assert!(r.login.is_none() && r.main.is_none());
    }

    #[test]
    fn shared_base_is_merged_with_push_overrides() {
        // The schema users actually write: shared registry/publish_path at the
        // top level, with a push-only stage override sub-table.
        let cfg = parse(
            r#"
[recipies.frontend]
registry = "nexus"
publish_path = "front/{CIABATTA_COMMIT}/dist"

[recipies.frontend.push]
pre  = "python bundle.py"
post = "./notify.sh"
"#,
        );
        let entry = &cfg.recipes["frontend"];
        let push = entry.push_recipe();
        // Shared base survives into the push direction.
        assert_eq!(push.registry.as_deref(), Some("nexus"));
        assert_eq!(
            push.publish_path,
            Some(PublishPath::Single(
                "front/{CIABATTA_COMMIT}/dist".to_string()
            ))
        );
        // Push-only overrides are applied.
        assert_eq!(push.pre.as_deref(), Some("python bundle.py"));
        assert_eq!(push.post.as_deref(), Some("./notify.sh"));
        // A push-only recipe has no pull action.
        assert!(entry.pull_recipe().is_none());
    }

    #[test]
    fn parses_pushpull_with_per_direction_stages() {
        let cfg = parse(
            r#"
[recipies.app.push]
main = "make push"
post = "echo done"

[recipies.app.pull]
main = "make pull"
"#,
        );
        let entry = &cfg.recipes["app"];
        assert!(entry.push.is_some() && entry.pull.is_some());
        assert_eq!(entry.push_recipe().main.as_deref(), Some("make push"));
        assert_eq!(entry.push_recipe().post.as_deref(), Some("echo done"));
        assert_eq!(
            entry.pull_recipe().unwrap().main.as_deref(),
            Some("make pull")
        );
    }

    #[test]
    fn publish_path_parses_single_and_list_forms() {
        let single = parse(
            r#"
[recipies.a]
registry = "nexus"
publish_path = "team/app/{CIABATTA_COMMIT}/app.tar.gz"
"#,
        );
        assert_eq!(
            single.recipes["a"].push_recipe().publish_path,
            Some(PublishPath::Single(
                "team/app/{CIABATTA_COMMIT}/app.tar.gz".to_string()
            ))
        );

        let list = parse(
            r#"
[recipies.b]
registry = "nexus"
publish_path = ["dist/*.tar.gz", "build/app.bin"]
strip_prefix = "dist/"
"#,
        );
        let r = list.recipes["b"].push_recipe();
        assert_eq!(
            r.publish_path,
            Some(PublishPath::Many(vec![
                "dist/*.tar.gz".to_string(),
                "build/app.bin".to_string()
            ]))
        );
        assert_eq!(r.strip_prefix.as_deref(), Some("dist/"));
    }

    #[test]
    fn validate_and_substitute_publish_path() {
        let mut vars = HashMap::new();
        assert!(validate_publish_path("a/{CIABATTA_COMMIT}/b", &vars).is_err());
        vars.insert("CIABATTA_COMMIT".to_string(), "abc".to_string());
        assert!(validate_publish_path("a/{CIABATTA_COMMIT}/b", &vars).is_ok());
        assert_eq!(
            substitute_vars("a/{CIABATTA_COMMIT}", &vars).unwrap(),
            "a/abc"
        );
    }

    #[test]
    fn expand_env_handles_defaults_and_presence() {
        // SAFETY: single-threaded test; we set/unset our own scoped vars.
        unsafe {
            std::env::set_var("CIABATTA_TEST_HOST", "nexus.internal");
            std::env::remove_var("CIABATTA_TEST_MISSING");
            std::env::set_var("CIABATTA_TEST_EMPTY", "");
        }

        // Set variable wins over its default.
        assert_eq!(
            expand_env("https://${CIABATTA_TEST_HOST:-fallback}/repo"),
            "https://nexus.internal/repo"
        );
        // Unset variable falls back to the default.
        assert_eq!(
            expand_env("https://${CIABATTA_TEST_MISSING:-fallback}/repo"),
            "https://fallback/repo"
        );
        // `:-` treats empty as unset; plain `-` keeps the empty value.
        assert_eq!(expand_env("${CIABATTA_TEST_EMPTY:-d}"), "d");
        assert_eq!(expand_env("${CIABATTA_TEST_EMPTY-d}"), "");
        // The `$`-less brace form is supported too.
        assert_eq!(
            expand_env("{CIABATTA_TEST_HOST:-fallback}"),
            "nexus.internal"
        );
        // A bare `{VAR}` with no `$` and no default is left untouched.
        assert_eq!(
            expand_env("path/{CIABATTA_COMMIT}/x"),
            "path/{CIABATTA_COMMIT}/x"
        );
        // `${VAR}` with no default expands to the value (empty if unset).
        assert_eq!(expand_env("${CIABATTA_TEST_HOST}"), "nexus.internal");
    }

    #[test]
    fn infer_kind_respects_type_override() {
        let cfg = parse(
            r#"
[registries.store]
url = "https://x"
type = "nexus"
"#,
        );
        assert_eq!(
            infer_registry_kind("store", &cfg.registries["store"]),
            RegistryKind::Nexus
        );
        assert_eq!(RegistryKind::from("my-ecr"), RegistryKind::Ecr);
    }
}
