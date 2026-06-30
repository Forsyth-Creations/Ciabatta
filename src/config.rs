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

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct SystemConfig {
    pub ci: Option<String>,
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

#[derive(Debug, Deserialize, Serialize, Clone, Default)]
pub struct SimpleRecipe {
    /// Named registry from [registries] section.
    pub registry: Option<String>,
    /// Local filesystem path for the artifact.
    pub local_artifact_path: Option<String>,
    /// Destination path in the registry; supports {CIABATTA_*} variable substitution.
    pub publish_path: Option<String>,
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

pub fn load_config(root: &Path) -> Result<CiabattaConfig> {
    let path = root.join(CIABATTA_DIR).join(CONFIG_FILE);
    if !path.exists() {
        return Ok(CiabattaConfig::default());
    }
    let content = std::fs::read_to_string(&path)
        .with_context(|| format!("Failed to read {}", path.display()))?;
    let config: CiabattaConfig =
        toml::from_str(&content).with_context(|| format!("Failed to parse {}", path.display()))?;
    Ok(config)
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
            push.publish_path.as_deref(),
            Some("front/{CIABATTA_COMMIT}/dist")
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
