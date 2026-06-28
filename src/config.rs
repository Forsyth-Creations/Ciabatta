use std::collections::HashMap;
use std::path::{Path, PathBuf};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

pub const CIABATTA_DIR: &str = ".ciabatta";
pub const CONFIG_FILE: &str = "ciabatta.toml";

#[derive(Debug, Deserialize, Serialize, Clone, Default)]
pub struct CiabattaConfig {
    pub system: Option<SystemConfig>,
    #[serde(default)]
    pub registries: HashMap<String, RegistryConfig>,
    #[serde(rename = "recipies", default)]
    pub recipes: HashMap<String, RecipeEntry>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct SystemConfig {
    pub ci: Option<String>,
    pub containers: Option<String>,
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

/// A recipe entry is either a simple action or a push/pull pair.
#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(untagged)]
pub enum RecipeEntry {
    PushPull(PushPullRecipe),
    Simple(SimpleRecipe),
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct PushPullRecipe {
    pub push: SimpleRecipe,
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
    pub bash_script: Option<String>,
}

impl RecipeEntry {
    pub fn push_recipe(&self) -> &SimpleRecipe {
        match self {
            RecipeEntry::Simple(r) => r,
            RecipeEntry::PushPull(pp) => &pp.push,
        }
    }

    pub fn pull_recipe(&self) -> Option<&SimpleRecipe> {
        match self {
            RecipeEntry::Simple(r) => Some(r),
            RecipeEntry::PushPull(pp) => pp.pull.as_ref(),
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
    let config: CiabattaConfig = toml::from_str(&content)
        .with_context(|| format!("Failed to parse {}", path.display()))?;
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
#[allow(dead_code)]
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
