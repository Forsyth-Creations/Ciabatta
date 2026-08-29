use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub const CIABATTA_DIR: &str = ".ciabatta";
/// The stem of a project's config file. The extension is whatever format it's
/// written in — see [`crate::format`], which resolves the two.
pub const CONFIG_STEM: &str = "ciabatta";
/// The config file ciabatta writes for a new project.
pub const CONFIG_FILE: &str = "ciabatta.yaml";

/// The config file inside `dir`'s `.ciabatta/`, in whichever format it's
/// written in, or `None` when that directory holds no config at all.
pub fn config_path(dir: &Path) -> Option<PathBuf> {
    crate::format::find(&dir.join(CIABATTA_DIR), CONFIG_STEM)
}

/// Environment variable overriding the interface every ciabatta web server binds
/// to. Defaults to loopback; set it to `0.0.0.0` to expose the servers outside a
/// container (e.g. `CIABATTA_BIND_HOST=0.0.0.0`).
pub const BIND_HOST_ENV: &str = "CIABATTA_BIND_HOST";
/// The default bind interface: loopback only, so servers aren't exposed unless
/// the operator opts in via [`BIND_HOST_ENV`].
pub const DEFAULT_BIND_HOST: &str = "127.0.0.1";

/// The interface ciabatta's web servers (run `--gui`/`--build`, analyze,
/// todo, and the ai daemon) bind to.
///
/// Reads [`BIND_HOST_ENV`], falling back to [`DEFAULT_BIND_HOST`] when it's unset
/// or empty. Set `CIABATTA_BIND_HOST=0.0.0.0` to reach the servers from outside a
/// container.
pub fn bind_host() -> String {
    match std::env::var(BIND_HOST_ENV) {
        Ok(v) if !v.trim().is_empty() => v.trim().to_string(),
        _ => DEFAULT_BIND_HOST.to_string(),
    }
}

#[derive(Debug, Deserialize, Serialize, Clone, Default)]
pub struct CiabattaConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system: Option<SystemConfig>,
    /// The `[workspace]` table: this directory's identity as a sub-workspace of
    /// a monorepo — its name, owner, and what it depends on. Written by
    /// `ciabatta init --lib`; absent for a standalone project.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace: Option<crate::workspace::WorkspaceMeta>,
    /// Workflows written inline as `[workflows.<name>]`, for a sub-workspace
    /// small enough not to want a file each. The usual home is
    /// `.ciabatta/workflows/<name>.toml`.
    #[serde(default)]
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    pub workflows: HashMap<String, crate::workspace::Workflow>,
    /// How to install the build tools workflows declare in `requires`. Usually
    /// written once at the monorepo root and inherited by every sub-workspace.
    #[serde(default)]
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    pub toolchain: HashMap<String, crate::workspace::ToolSpec>,
    #[serde(default)]
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    pub registries: HashMap<String, RegistryConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub analyze: Option<AnalyzeConfig>,
    /// Settings for the `ciabatta ai` assistant.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ai: Option<AiConfig>,
    /// Build caching for this workspace: what its builds read, what they write,
    /// and whether to reuse the result. Off unless the workspace opts in — see
    /// [`crate::cache`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache: Option<crate::cache::CacheConfig>,
}

#[derive(Debug, Deserialize, Serialize, Clone, Default)]
pub struct SystemConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ci: Option<String>,
    /// Container runtime (`docker` or `podman`). When unset, ciabatta auto-detects
    /// what's installed at run time (see [`resolve_container_cmd`]).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub containers: Option<String>,
}

/// Optional inputs for `ciabatta analyze` (paths relative to the project root).
#[derive(Debug, Deserialize, Serialize, Clone, Default)]
pub struct AnalyzeConfig {
    /// A file listing project requirements, one per line (`id` or `id, description`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub requirements: Option<String>,
    /// A CSV tracing requirements to source files (columns: requirement, file).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trace: Option<String>,
}

/// Settings for the `ciabatta ai` assistant (see `ciabatta ai setup`).
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct AiConfig {
    /// Which wire format to speak: `claude` (default), `openai`, or `vllm` —
    /// the latter two both cover any OpenAI-compatible endpoint (OpenAI, vLLM,
    /// Ollama, LM Studio, …); `vllm` just defaults the endpoint to
    /// http://localhost:8000.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// Base URL of the API. Defaults per provider (api.anthropic.com /
    /// api.openai.com / localhost:8000 for vLLM); point it at a local or
    /// remote server for self-hosted models.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
    /// Model name. Defaults per provider.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Name of the environment variable holding the API key
    /// (default: ANTHROPIC_API_KEY or OPENAI_API_KEY).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_key_env: Option<String>,
    /// Verify the endpoint's TLS certificate. Defaults to true; set to false
    /// for a self-hosted vLLM/OpenAI endpoint behind a self-signed cert.
    // Not skipped when false: this defaults to `true`, so omitting `false`
    // would silently turn it back on.
    #[serde(default = "default_true")]
    pub tls_verify: bool,
    /// Container base images the assistant may spin up as sandboxes via the
    /// configured runtime ([system].containers → podman/docker). Any number of
    /// images; the assistant can only use images listed here.
    #[serde(default)]
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<String>,
    /// Command that proves the project still builds/tests after the assistant
    /// changes code (e.g. `cargo build`, `cargo test`, `npm run build`). When
    /// set, the agent loop won't finish a code-changing task until this passes,
    /// feeding any failure back so the model fixes it. When omitted, the loop
    /// auto-detects a sensible command from the project's manifests; set it to
    /// an empty string to disable the verification gate entirely.
    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verify: Option<String>,
    /// Ceiling on the model's reply length per request. Claude requires an
    /// explicit value (default 8192); for OpenAI-compatible endpoints it is only
    /// sent when set, since some local servers reject it. Raise it if large
    /// edits or plans are being truncated mid-tool-call.
    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u64>,
    /// Cap on model⇄tool round trips per question (default 50). A large refactor
    /// spanning many files can exceed the default; raise it for long autonomous
    /// tasks, lower it to fail fast on a confused model.
    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tool_rounds: Option<usize>,
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
            verify: None,
            max_tokens: None,
            max_tool_rounds: None,
        }
    }
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct RegistryConfig {
    pub url: String,
    // Not skipped when false: this defaults to `true`, so omitting `false`
    // would silently turn it back on.
    #[serde(default = "default_true")]
    pub tls_verify: bool,
    #[serde(default)]
    #[serde(skip_serializing_if = "crate::format::is_false")]
    pub needs_auth: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub login_script: Option<String>,
    /// Optional explicit type; inferred from registry name if absent.
    #[serde(rename = "type")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub registry_type: Option<String>,
    /// Nexus only: the repository to publish into (e.g. `raw-hosted`,
    /// `npm-hosted`). When set, `url` is treated as the bare Nexus host and the
    /// `/repository/<repository>` segment is appended automatically. When unset,
    /// `url` is used as the full repository URL (backwards compatible).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repository: Option<String>,
    /// Nexus raw only: an optional path prefix prepended to every workflow's
    /// `publish_path`, so raw artifacts land under a common folder.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_path: Option<String>,
    /// Nexus only: the repository format, selecting how the main push happens.
    /// One of `raw` (HTTP PUT, the default), `npm` (`npm publish`), or `pypi`
    /// (`twine upload`).
    #[serde(skip_serializing_if = "Option::is_none")]
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

/// Where a workflow publishes to. Either a single remote path (the classic form,
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

/// Load the config discovered in `<root>/.ciabatta/`, in whichever format it's
/// written in. Returns the default (empty) config when there is no config file.
pub fn load_config(root: &Path) -> Result<CiabattaConfig> {
    match config_path(root) {
        Some(path) => load_config_file(&path),
        None => Ok(CiabattaConfig::default()),
    }
}

/// Load and parse a specific config file (used by the `--config` flag),
/// expanding environment references in registry URLs and login scripts. Unlike
/// [`load_config`], a missing or unparseable file is an error — the caller
/// pointed at this file explicitly.
pub fn load_config_file(path: &Path) -> Result<CiabattaConfig> {
    let mut config: CiabattaConfig = crate::format::load(path)?;

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

/// The shape of a `{VAR}` placeholder, shared by substitution and the
/// "which placeholders can't be filled yet" check below.
const VAR_PLACEHOLDER: &str = r"\{([A-Z_][A-Z0-9_]*)\}";

/// The `{VAR}` placeholders in `template` that `vars` has no value for, in the
/// order they appear (de-duplicated).
///
/// [`substitute_vars`] reports only the first one, as an error string. Callers
/// that want to *fix* the problem rather than report it — the run launcher
/// prompting for what a run is missing — need the names instead.
pub fn unresolved_vars(template: &str, vars: &HashMap<String, String>) -> Vec<String> {
    let re = regex::Regex::new(VAR_PLACEHOLDER).unwrap();
    let mut missing: Vec<String> = Vec::new();
    for caps in re.captures_iter(template) {
        let name = caps[1].to_string();
        if !vars.contains_key(&name) && !missing.contains(&name) {
            missing.push(name);
        }
    }
    missing
}

/// Substitute `{VAR}` placeholders in a string with values from `vars`.
pub fn substitute_vars(template: &str, vars: &HashMap<String, String>) -> Result<String> {
    let re = regex::Regex::new(VAR_PLACEHOLDER).unwrap();
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
    fn publish_path_parses_single_and_list_forms() {
        let single = parse(
            r#"
[workflows.release]
[[workflows.release.steps]]
name = "publish"
kind = "push"
registry = "nexus"
publish_path = "team/app/{CIABATTA_COMMIT}/app.tar.gz"
"#,
        );
        let step = &single.workflows["release"].steps[0];
        assert_eq!(
            step.publish_path,
            Some(PublishPath::Single(
                "team/app/{CIABATTA_COMMIT}/app.tar.gz".to_string()
            ))
        );

        let list = parse(
            r#"
[workflows.release]
[[workflows.release.steps]]
name = "publish"
kind = "push"
registry = "nexus"
publish_path = ["dist/*.tar.gz", "build/app.bin"]
strip_prefix = "dist/"
"#,
        );
        let step = &list.workflows["release"].steps[0];
        assert_eq!(
            step.publish_path,
            Some(PublishPath::Many(vec![
                "dist/*.tar.gz".to_string(),
                "build/app.bin".to_string()
            ]))
        );
        assert_eq!(step.strip_prefix.as_deref(), Some("dist/"));
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

#[cfg(test)]
mod yaml_tests {
    use super::*;

    fn yaml(s: &str) -> CiabattaConfig {
        crate::format::from_str(s, crate::format::Format::Yaml).expect("yaml config should parse")
    }

    /// The schema leans on `#[serde(flatten)]` (a workflow's shared base) wrapped
    /// around an untagged enum (`publish_path`, one path or a list of globs).
    /// That pairing is the one thing most likely to behave differently between
    /// the two parsers, so it gets a test of its own.
    #[test]
    fn yaml_handles_transfer_steps_and_untagged_publish_paths() {
        let cfg = yaml(
            r#"
registries:
  nexus:
    url: http://localhost:8527
    repository: raw-hosted
workflows:
  release:
    steps:
      - name: publish
        kind: push
        registry: nexus
        artifact: dist/app.tar.gz
        publish_path: team/app/{CIABATTA_COMMIT}/app.tar.gz
      - name: publish-many
        kind: push
        registry: nexus
        publish_path:
          - dist/*.tar.gz
          - build/app.bin
        strip_prefix: dist/
      - name: fetch
        kind: pull
        from: publish
"#,
        );

        let steps = &cfg.workflows["release"].steps;
        assert_eq!(steps[0].registry.as_deref(), Some("nexus"));
        assert_eq!(steps[0].artifact.as_deref(), Some("dist/app.tar.gz"));
        assert_eq!(
            steps[0].publish_path,
            Some(PublishPath::Single(
                "team/app/{CIABATTA_COMMIT}/app.tar.gz".to_string()
            ))
        );
        assert_eq!(
            steps[1].publish_path,
            Some(PublishPath::Many(vec![
                "dist/*.tar.gz".to_string(),
                "build/app.bin".to_string()
            ]))
        );
        assert_eq!(steps[1].strip_prefix.as_deref(), Some("dist/"));
        assert_eq!(steps[2].from.as_deref(), Some("publish"));
    }

    /// `env_file`/`when`/`skip_if` accept one-or-many through a custom
    /// deserializer, and steps carry bools and ints with `#[serde(default)]`.
    #[test]
    fn yaml_handles_workflow_steps_and_one_or_many_fields() {
        let cfg = yaml(
            r#"
workflows:
  svc:
    env_file: .env
    REQUIRED_ENV: [API_URL]
    steps:
      - name: build
        run: cargo build
        retries: 2
        tags: [fast]
      - name: ship
        run: make ship
        needs: [build]
        when:
          - env.RUN_ENV == prod
        persistent: false
"#,
        );

        let wf = &cfg.workflows["svc"];
        assert_eq!(wf.env_file, vec![".env".to_string()]);
        assert_eq!(wf.required_env, vec!["API_URL".to_string()]);
        assert_eq!(wf.steps.len(), 2);
        assert_eq!(wf.steps[0].retries, 2);
        assert_eq!(wf.steps[1].when, vec!["env.RUN_ENV == prod".to_string()]);
        assert_eq!(wf.steps[1].needs, vec!["build".to_string()]);
    }

    /// A config written in either format must produce the same value, or the
    /// migration isn't a migration.
    #[test]
    fn the_two_formats_agree() {
        let from_toml: CiabattaConfig = crate::format::from_str(
            r#"
[workspace]
name = "api"
depends_on = ["proto:generate"]

[workflows.release]
[[workflows.release.steps]]
name = "publish"
kind = "push"
registry = "ecr"
local_image = "app:latest"
publish_path = "app:{CIABATTA_COMMIT}"
"#,
            crate::format::Format::Toml,
        )
        .unwrap();

        let from_yaml = yaml(
            r#"
workspace:
  name: api
  depends_on: [proto:generate]
workflows:
  release:
    steps:
      - name: publish
        kind: push
        registry: ecr
        local_image: app:latest
        publish_path: app:{CIABATTA_COMMIT}
"#,
        );

        assert_eq!(
            from_toml.workspace.as_ref().unwrap().name,
            from_yaml.workspace.as_ref().unwrap().name
        );
        assert_eq!(
            from_toml.workspace.as_ref().unwrap().depends_on,
            from_yaml.workspace.as_ref().unwrap().depends_on
        );
        let toml_step = &from_toml.workflows["release"].steps[0];
        let yaml_step = &from_yaml.workflows["release"].steps[0];
        assert_eq!(toml_step.publish_path, yaml_step.publish_path);
        assert_eq!(toml_step.local_image, yaml_step.local_image);
    }
}
