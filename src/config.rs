use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
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
    /// Named menus. A menu groups recipes so they can be pushed/pulled together
    /// with `--cookbook <menu>`; each value lists the recipe names it contains.
    #[serde(default)]
    pub menus: HashMap<String, Vec<String>>,
    pub analyze: Option<AnalyzeConfig>,
    /// Settings for the `ciabatta ai` assistant.
    pub ai: Option<AiConfig>,
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

/// Settings for the `ciabatta ai` assistant (see `ciabatta ai setup`).
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct AiConfig {
    /// Which wire format to speak: `claude` (default), `openai`, or `vllm` —
    /// the latter two both cover any OpenAI-compatible endpoint (OpenAI, vLLM,
    /// Ollama, LM Studio, …); `vllm` just defaults the endpoint to
    /// http://localhost:8000.
    pub provider: Option<String>,
    /// Base URL of the API. Defaults per provider (api.anthropic.com /
    /// api.openai.com / localhost:8000 for vLLM); point it at a local or
    /// remote server for self-hosted models.
    pub endpoint: Option<String>,
    /// Model name. Defaults per provider.
    pub model: Option<String>,
    /// Name of the environment variable holding the API key
    /// (default: ANTHROPIC_API_KEY or OPENAI_API_KEY).
    pub api_key_env: Option<String>,
    /// Verify the endpoint's TLS certificate. Defaults to true; set to false
    /// for a self-hosted vLLM/OpenAI endpoint behind a self-signed cert.
    #[serde(default = "default_true")]
    pub tls_verify: bool,
    /// Container base images the assistant may spin up as sandboxes via the
    /// configured runtime ([system].containers → podman/docker). Any number of
    /// images; the assistant can only use images listed here.
    #[serde(default)]
    pub images: Vec<String>,
}

impl Default for AiConfig {
    fn default() -> Self {
        Self {
            provider: None,
            endpoint: None,
            model: None,
            api_key_env: None,
            tls_verify: true,
            images: Vec::new(),
        }
    }
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
    /// Nexus only: the repository to publish into (e.g. `raw-hosted`,
    /// `npm-hosted`). When set, `url` is treated as the bare Nexus host and the
    /// `/repository/<repository>` segment is appended automatically. When unset,
    /// `url` is used as the full repository URL (backwards compatible).
    pub repository: Option<String>,
    /// Nexus raw only: an optional path prefix prepended to every recipe's
    /// `publish_path`, so raw artifacts land under a common folder.
    pub base_path: Option<String>,
    /// Nexus only: the repository format, selecting how the main push happens.
    /// One of `raw` (HTTP PUT, the default), `npm` (`npm publish`), or `pypi`
    /// (`twine upload`).
    pub format: Option<String>,
}

fn default_true() -> bool {
    true
}

/// The format of a Nexus repository, which determines the publish mechanism.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NexusFormat {
    /// Plain file upload/download over HTTP PUT/GET (Nexus `raw` repositories).
    Raw,
    /// Native `npm publish` against a Nexus `npm` hosted repository.
    Npm,
    /// Native `twine upload` against a Nexus `pypi` hosted repository.
    Pypi,
}

impl RegistryConfig {
    /// The base URL of the target Nexus repository, without a trailing slash.
    ///
    /// If `repository` is set, it's `<url>/repository/<repository>`; otherwise
    /// `url` is assumed to already point at the repository.
    pub fn nexus_repo_url(&self) -> String {
        let base = self.url.trim_end_matches('/');
        match self.repository.as_deref() {
            Some(repo) => format!("{base}/repository/{}", repo.trim_matches('/')),
            None => base.to_string(),
        }
    }

    /// The full object URL for a raw upload/download of `remote_path`, applying
    /// the optional `base_path` prefix.
    pub fn nexus_object_url(&self, remote_path: &str) -> String {
        let base = self.nexus_repo_url();
        let mut segments: Vec<&str> = Vec::new();
        if let Some(bp) = self.base_path.as_deref() {
            let bp = bp.trim_matches('/');
            if !bp.is_empty() {
                segments.push(bp);
            }
        }
        let rp = remote_path.trim_matches('/');
        if !rp.is_empty() {
            segments.push(rp);
        }
        format!("{base}/{}", segments.join("/"))
    }

    /// Parse the configured Nexus repository format (defaults to `raw`).
    pub fn nexus_format(&self) -> Result<NexusFormat> {
        match self.format.as_deref() {
            None => Ok(NexusFormat::Raw),
            Some(s) => match s.trim().to_lowercase().as_str() {
                "raw" => Ok(NexusFormat::Raw),
                "npm" => Ok(NexusFormat::Npm),
                "pypi" | "pip" => Ok(NexusFormat::Pypi),
                other => bail!(
                    "Unknown nexus format '{other}' for registry (expected: raw, npm, or pypi)"
                ),
            },
        }
    }
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
    /// Deploy-direction definition: a DAG of dependent script steps (usually in
    /// a separate flowchart file). Unlike push/pull this is not a `SimpleRecipe`.
    pub deploy: Option<crate::deploy::DeployRecipe>,
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
    /// Docker/ECR only: a local image reference (`name` or `name:tag`) to push.
    /// ciabatta retags it to the registry's target reference before pushing
    /// (`docker tag <local_image> <url>/<publish_path>`), and on pull retags the
    /// pulled image back to this name. When set, `publish_path` is the remote
    /// image reference; if omitted, the local reference is reused verbatim.
    pub local_image: Option<String>,
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
            local_image: over
                .local_image
                .clone()
                .or_else(|| self.local_image.clone()),
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

    /// The deploy definition for this recipe, if it declares a `[deploy]`
    /// sub-table. Deploys are opt-in per recipe (no implicit default).
    pub fn deploy_recipe(&self) -> Option<&crate::deploy::DeployRecipe> {
        self.deploy.as_ref()
    }

    /// Whether the push/pull direction has something to transfer or run. A recipe
    /// needs a registry, a path/image to move, or a `main`/`bash_script` command;
    /// with none of these the push runner has nothing to do and errors with
    /// "no push/pull action".
    pub fn has_transfer_action(&self) -> bool {
        let push = self.push_recipe();
        push.registry.is_some()
            || push.publish_path.is_some()
            || push.local_image.is_some()
            || push.local_artifact_path.is_some()
            || push.main.is_some()
            || push.bash_script.is_some()
    }

    /// A recipe that declares a `[deploy]` section but has no push/pull transfer
    /// action is deploy-only: it exists solely as a deployment task, so `ciabatta
    /// push`/`pull` skips it rather than failing on "no push/pull action".
    pub fn is_deploy_only(&self) -> bool {
        self.deploy.is_some() && !self.has_transfer_action()
    }
}

/// Resolve which recipes to run from `--cookbook` menu selections and explicitly
/// named recipes.
///
/// - Neither given → every recipe (the "push all" default).
/// - Each cookbook expands to the recipe names its menu lists.
/// - Explicit recipe names are appended.
/// Results are de-duplicated in first-seen order, so a recipe shared by two
/// selected menus (or named alongside a menu) runs once. Errors when a named
/// menu is undefined or lists a recipe that doesn't exist.
pub fn select_recipe_names(
    config: &CiabattaConfig,
    cookbooks: &[String],
    recipes: &[String],
) -> Result<Vec<String>> {
    if cookbooks.is_empty() && recipes.is_empty() {
        return Ok(config.recipes.keys().cloned().collect());
    }

    let mut names: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();

    for menu in cookbooks {
        let members = config.menus.get(menu).ok_or_else(|| {
            let mut available: Vec<&String> = config.menus.keys().collect();
            available.sort();
            let hint = if available.is_empty() {
                "No menus are defined; add a [menus] section to ciabatta.toml.".to_string()
            } else {
                format!(
                    "Available menus: {}.",
                    available
                        .iter()
                        .map(|s| s.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            };
            anyhow::anyhow!("Menu '{}' is not defined. {}", menu, hint)
        })?;

        for recipe in members {
            if !config.recipes.contains_key(recipe) {
                bail!(
                    "Menu '{}' references recipe '{}', which is not defined in [recipies].",
                    menu,
                    recipe
                );
            }
            if seen.insert(recipe.clone()) {
                names.push(recipe.clone());
            }
        }
    }

    for recipe in recipes {
        if seen.insert(recipe.clone()) {
            names.push(recipe.clone());
        }
    }

    Ok(names)
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
                 Set CIABATTA_ENV=local (or pass --local) to resolve branch/commit \
                 from git, pass it with -e {}=<value>, or let your CI system provide it.",
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
    fn nexus_repo_url_composes_from_host_and_repository() {
        let cfg = parse(
            r#"
[registries.nexus]
url = "http://localhost:8527"
repository = "raw-hosted"
"#,
        );
        let rc = &cfg.registries["nexus"];
        assert_eq!(
            rc.nexus_repo_url(),
            "http://localhost:8527/repository/raw-hosted"
        );
        assert_eq!(
            rc.nexus_object_url("group/app.bin"),
            "http://localhost:8527/repository/raw-hosted/group/app.bin"
        );
    }

    #[test]
    fn nexus_object_url_applies_base_path() {
        let cfg = parse(
            r#"
[registries.nexus]
url = "http://localhost:8527/"
repository = "raw-hosted"
base_path = "/builds/"
"#,
        );
        let rc = &cfg.registries["nexus"];
        assert_eq!(
            rc.nexus_object_url("/app.bin"),
            "http://localhost:8527/repository/raw-hosted/builds/app.bin"
        );
    }

    #[test]
    fn nexus_url_without_repository_is_used_verbatim() {
        // Backwards-compatible: the pre-existing full-repo-URL form still works.
        let cfg = parse(
            r#"
[registries.nexus]
url = "http://localhost:8527/repository/raw-hosted/"
"#,
        );
        let rc = &cfg.registries["nexus"];
        assert_eq!(
            rc.nexus_object_url("a/b"),
            "http://localhost:8527/repository/raw-hosted/a/b"
        );
    }

    #[test]
    fn nexus_format_parses_and_rejects_unknown() {
        let cfg = parse(
            r#"
[registries.raw]
url = "http://h"
[registries.npm]
url = "http://h"
format = "npm"
[registries.pypi]
url = "http://h"
format = "PyPI"
[registries.bad]
url = "http://h"
format = "maven"
"#,
        );
        assert_eq!(
            cfg.registries["raw"].nexus_format().unwrap(),
            NexusFormat::Raw
        );
        assert_eq!(
            cfg.registries["npm"].nexus_format().unwrap(),
            NexusFormat::Npm
        );
        assert_eq!(
            cfg.registries["pypi"].nexus_format().unwrap(),
            NexusFormat::Pypi
        );
        assert!(cfg.registries["bad"].nexus_format().is_err());
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
    fn parses_local_image_and_overlays_it() {
        let cfg = parse(
            r#"
[recipies.app]
registry = "ecr"
local_image = "app:latest"
publish_path = "app:{CIABATTA_COMMIT}"

[recipies.app.push]
local_image = "app:release"
"#,
        );
        let entry = &cfg.recipes["app"];
        // Base value is visible on a plain read.
        assert_eq!(entry.base.local_image.as_deref(), Some("app:latest"));
        // Push override wins over the shared base.
        assert_eq!(
            entry.push_recipe().local_image.as_deref(),
            Some("app:release")
        );
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
    fn select_recipe_names_expands_menus_and_dedupes() {
        let cfg = parse(
            r#"
[recipies.a]
registry = "nexus"
[recipies.b]
registry = "nexus"
[recipies.c]
registry = "nexus"

[menus]
frontend = ["a", "b"]
backend  = ["b", "c"]
"#,
        );

        // No selection → all recipes (order-independent).
        let mut all = select_recipe_names(&cfg, &[], &[]).unwrap();
        all.sort();
        assert_eq!(all, vec!["a", "b", "c"]);

        // Single menu expands to its members, in order.
        assert_eq!(
            select_recipe_names(&cfg, &["frontend".into()], &[]).unwrap(),
            vec!["a", "b"]
        );

        // Two menus sharing "b" run it once, first-seen order preserved.
        assert_eq!(
            select_recipe_names(&cfg, &["frontend".into(), "backend".into()], &[]).unwrap(),
            vec!["a", "b", "c"]
        );

        // Menu + an explicit recipe already on the menu → no duplicate.
        assert_eq!(
            select_recipe_names(&cfg, &["frontend".into()], &["a".into()]).unwrap(),
            vec!["a", "b"]
        );

        // Menu + a distinct explicit recipe → appended after the menu members.
        assert_eq!(
            select_recipe_names(&cfg, &["frontend".into()], &["c".into()]).unwrap(),
            vec!["a", "b", "c"]
        );
    }

    #[test]
    fn select_recipe_names_rejects_bad_menus() {
        let cfg = parse(
            r#"
[recipies.a]
registry = "nexus"

[menus]
good = ["a"]
broken = ["a", "missing"]
"#,
        );

        // Undefined menu.
        let err = select_recipe_names(&cfg, &["nope".into()], &[]).unwrap_err();
        assert!(err.to_string().contains("is not defined"));
        assert!(err.to_string().contains("Available menus: broken, good"));

        // Menu that references a missing recipe.
        let err = select_recipe_names(&cfg, &["broken".into()], &[]).unwrap_err();
        assert!(err.to_string().contains("references recipe 'missing'"));
    }

    #[test]
    fn parses_deploy_sub_table() {
        let cfg = parse(
            r#"
[recipies.web]
registry = "nexus"

[recipies.web.deploy]
flowchart = ".ciabatta/deploys.toml"
pre  = "scripts/notify.sh"

[recipies.web.push]
bash_script = "scripts/push.sh"
"#,
        );
        let entry = &cfg.recipes["web"];
        let deploy = entry.deploy_recipe().expect("deploy present");
        assert_eq!(deploy.flowchart.as_deref(), Some(".ciabatta/deploys.toml"));
        assert_eq!(deploy.pre.as_deref(), Some("scripts/notify.sh"));
        // Deploy coexists with a push action on the same recipe.
        assert_eq!(
            entry.push_recipe().bash_script.as_deref(),
            Some("scripts/push.sh")
        );
    }

    #[test]
    fn is_deploy_only_distinguishes_pure_deploy_recipes() {
        let cfg = parse(
            r#"
# Deploy-only: a [deploy] section and nothing to push/pull.
[recipies.migrate.deploy]
flowchart = ".ciabatta/deploys.toml"

# Deploy alongside a real push action → still pushable.
[recipies.web]
registry = "nexus"
publish_path = "web/{CIABATTA_COMMIT}/dist"
[recipies.web.deploy]
flowchart = ".ciabatta/deploys.toml"

# A plain transfer recipe with no deploy → never deploy-only.
[recipies.assets]
registry = "nexus"

# A command recipe (no registry/path) is a push action, not deploy-only.
[recipies.script.deploy]
flowchart = ".ciabatta/deploys.toml"
[recipies.script.push]
main = "make ship"
"#,
        );

        assert!(cfg.recipes["migrate"].is_deploy_only());
        assert!(!cfg.recipes["web"].is_deploy_only());
        assert!(!cfg.recipes["assets"].is_deploy_only());
        assert!(!cfg.recipes["script"].is_deploy_only());
    }

    #[test]
    fn parses_inline_deploy_steps() {
        let cfg = parse(
            r#"
[recipies.svc.deploy]
[[recipies.svc.deploy.steps]]
name = "build"
script = "b.sh"
[[recipies.svc.deploy.steps]]
name = "ship"
run = "make ship"
needs = ["build"]
on_error = "fix"
[[recipies.svc.deploy.steps]]
name = "fix"
recover = true
options = [ { label = "retry", run = "true", default = true } ]
"#,
        );
        let deploy = cfg.recipes["svc"].deploy_recipe().unwrap();
        assert_eq!(deploy.steps.len(), 3);
        assert_eq!(deploy.steps[1].on_error.as_deref(), Some("fix"));
        assert!(deploy.steps[2].recover);
        assert_eq!(deploy.steps[2].options[0].label, "retry");
        assert!(deploy.steps[2].options[0].default);
    }

    #[test]
    fn recipe_without_deploy_has_none() {
        let cfg = parse(
            r#"
[recipies.a]
registry = "nexus"
publish_path = "x/y"
"#,
        );
        assert!(cfg.recipes["a"].deploy_recipe().is_none());
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
