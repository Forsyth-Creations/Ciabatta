mod cli;
mod ci;
mod config;
mod registry;
mod runner;
mod tui;

use std::collections::HashMap;
use std::env;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::Parser;

use cli::{Cli, Commands, ConfigCommand};
use config::{CiabattaConfig, find_root, load_config};
use runner::RunMode;

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Run { recipes, env, dry_run, no_tui, config } => {
            let (root, cfg) = load_project(config.as_deref())?;
            let vars = build_env_vars(&cfg, &env)?;
            let names = resolve_recipe_names(&cfg, &recipes);
            execute_recipes(&cfg, &root, &names, &vars, dry_run, no_tui, RunMode::Push).await?;
        }

        Commands::Pull { recipes, env, dry_run, no_tui, config } => {
            let (root, cfg) = load_project(config.as_deref())?;
            let vars = build_env_vars(&cfg, &env)?;
            let names = resolve_recipe_names(&cfg, &recipes);
            execute_recipes(&cfg, &root, &names, &vars, dry_run, no_tui, RunMode::Pull).await?;
        }

        Commands::List => {
            let (_, cfg) = load_project(None)?;
            list_recipes(&cfg);
        }

        Commands::Init { ci, containers, force } => {
            cmd_init(ci.as_deref(), &containers, force)?;
        }

        Commands::Tui => {
            run_tui_browser().await?;
        }

        Commands::Config { subcommand } => match subcommand {
            ConfigCommand::Show => {
                let (root, cfg) = load_project(None)?;
                show_config(&cfg, &root);
            }
            ConfigCommand::Reference => {
                print_config_help();
            }
        },
    }

    Ok(())
}

fn load_project(config_path: Option<&std::path::Path>) -> Result<(PathBuf, CiabattaConfig)> {
    let cwd = env::current_dir().context("Failed to get current directory")?;

    let root = if let Some(p) = config_path {
        // Explicit path: <root>/.ciabatta/ciabatta.toml → root is two levels up.
        p.parent()
            .and_then(|d| d.parent())
            .unwrap_or(&cwd)
            .to_path_buf()
    } else {
        // Walk upward from cwd until a .ciabatta/ directory is found.
        find_root(&cwd).ok_or_else(|| {
            anyhow::anyhow!(
                "No .ciabatta/ directory found in '{}' or any parent directory.\n\
                 Create one and add a ciabatta.toml to get started.\n\
                 Run `ciabatta config reference` for format documentation.",
                cwd.display()
            )
        })?
    };

    let cfg = load_config(&root)?;
    Ok((root, cfg))
}

/// Build the final environment variable map:
/// 1. Start with CI-derived vars
/// 2. Merge current process env
/// 3. Override with CLI -e flags (highest priority)
fn build_env_vars(cfg: &CiabattaConfig, cli_env: &[String]) -> Result<HashMap<String, String>> {
    let mut vars: HashMap<String, String> = std::env::vars().collect();

    // Resolve CI variables and print them.
    if let Some(ref system) = cfg.system {
        if let Some(ref ci_name) = system.ci {
            let ci_system = ci::CiSystem::from(ci_name.as_str());
            let (ci_vars, resolved) = ci::resolve_ci_vars(&ci_system);
            if !resolved.is_empty() {
                eprintln!("CI variables resolved from {}:", ci_system);
                for rv in &resolved {
                    eprintln!(
                        "  {} = {} (from {})",
                        rv.ciabatta_name, rv.value, rv.source_name
                    );
                }
            }
            // Merge CI vars; they DON'T override existing env vars set by the user.
            for (k, v) in ci_vars {
                vars.entry(k).or_insert(v);
            }
        }
    }

    // CLI -e flags override everything.
    let cli_map = cli::parse_env_flags(cli_env)?;
    vars.extend(cli_map);

    Ok(vars)
}

fn resolve_recipe_names(cfg: &CiabattaConfig, requested: &[String]) -> Vec<String> {
    if requested.is_empty() {
        cfg.recipes.keys().cloned().collect()
    } else {
        requested.to_vec()
    }
}

async fn execute_recipes(
    cfg: &CiabattaConfig,
    root: &PathBuf,
    names: &[String],
    vars: &HashMap<String, String>,
    dry_run: bool,
    no_tui: bool,
    mode: RunMode,
) -> Result<()> {
    if names.is_empty() {
        bail!("No recipes found. Run `ciabatta list` to see available recipes, or check your .ciabatta/ciabatta.toml.");
    }

    // Validate that all publish-path variables are present before launching.
    runner::validate_recipes(cfg, names, vars, &mode)?;

    if no_tui {
        run_plain(cfg, root, names, vars, dry_run, mode).await
    } else {
        let success = tui::run(cfg, root, names, vars, dry_run, mode).await?;
        if !success {
            bail!("One or more recipes failed.");
        }
        Ok(())
    }
}

async fn run_plain(
    cfg: &CiabattaConfig,
    root: &PathBuf,
    names: &[String],
    vars: &HashMap<String, String>,
    dry_run: bool,
    mode: RunMode,
) -> Result<()> {
    use tokio::sync::mpsc;
    use runner::ProgressUpdate;

    let (tx, mut rx) = mpsc::channel::<ProgressUpdate>(256);

    let cfg_clone = cfg.clone();
    let root_clone = root.clone();
    let names_clone = names.to_vec();
    let vars_clone = vars.clone();

    tokio::spawn(async move {
        let _ = runner::run_all(&cfg_clone, &root_clone, &names_clone, &vars_clone, dry_run, mode, tx).await;
    });

    let mut any_failed = false;
    while let Some(update) = rx.recv().await {
        match update {
            ProgressUpdate::Started(name) => println!("[{name}] started"),
            ProgressUpdate::Log(name, line) => println!("[{name}] {line}"),
            ProgressUpdate::Completed(name) => println!("[{name}] ✓ completed"),
            ProgressUpdate::Failed(name, err) => {
                eprintln!("[{name}] ✗ failed: {err}");
                any_failed = true;
            }
        }
    }

    if any_failed {
        bail!("One or more recipes failed.");
    }
    Ok(())
}

fn cmd_init(ci: Option<&str>, containers: &str, force: bool) -> Result<()> {
    use std::fs;
    use config::{CIABATTA_DIR, CONFIG_FILE};

    let cwd = env::current_dir().context("Failed to get current directory")?;

    // Don't walk upward — init always targets the cwd.
    let dir = cwd.join(CIABATTA_DIR);
    let config_path = dir.join(CONFIG_FILE);

    if config_path.exists() && !force {
        bail!(
            ".ciabatta/ciabatta.toml already exists.\n\
             Use --force to overwrite, or edit it directly."
        );
    }

    fs::create_dir_all(&dir)
        .with_context(|| format!("Failed to create {}", dir.display()))?;

    let toml = build_starter_toml(ci, containers);
    fs::write(&config_path, &toml)
        .with_context(|| format!("Failed to write {}", config_path.display()))?;

    println!("Initialized ciabatta project in {}", cwd.display());
    println!("Created: {}", config_path.display());
    println!();
    println!("Next steps:");
    println!("  1. Edit .ciabatta/ciabatta.toml to define your registries and recipes.");
    println!("  2. Run `ciabatta list` to verify your recipes are recognized.");
    println!("  3. Run `ciabatta run --dry-run <recipe>` to preview what will happen.");
    println!("  4. Run `ciabatta tui` to open the interactive browser.");
    println!();
    println!("For config format documentation: ciabatta config reference");

    Ok(())
}

fn build_starter_toml(ci: Option<&str>, containers: &str) -> String {
    let ci_line = match ci {
        Some(s) => format!("ci = {:?}", s),
        None => {
            // Auto-detect from environment.
            let detected = detect_ci();
            if let Some(ref name) = detected {
                format!("ci = {:?}  # auto-detected", name)
            } else {
                r#"# ci = "github"  # Uncomment and set: gitlab | github | jenkins | circleci | azure | bitbucket"#.to_string()
            }
        }
    };

    format!(r#"# Ciabatta configuration
# Run `ciabatta config reference` for full documentation.

[system]
{ci_line}
containers = {containers:?}

# ─── Registries ────────────────────────────────────────────────────────────────
# Define each registry you publish to. The section name is used as the registry
# identifier in recipes. Supported types (auto-detected from name):
#   nexus, artifactory → HTTP PUT/GET
#   s3                 → aws s3 cp
#   docker             → docker push/pull
#   ecr                → AWS ECR (auto-login)
#
# [registries.nexus]
# url          = "https://nexus.example.com/repository/releases/"
# tls_verify   = true
# needs_auth   = true
# login_script = ".ciabatta/nexus_login.sh"
#
# [registries.ecr]
# url        = "123456789.dkr.ecr.us-east-1.amazonaws.com"
# needs_auth = false   # ciabatta auto-fetches the ECR token

# ─── Recipes ───────────────────────────────────────────────────────────────────
# Each recipe describes how to push (and optionally pull) one artifact.
# Variables available in publish_path: {{CIABATTA_BRANCH}}, {{CIABATTA_COMMIT}},
#                                      {{CIABATTA_TAG}}, {{CIABATTA_BUILD_NUMBER}}
#
# Registry-based recipe (HTTP or S3 upload):
# [recipies.my_artifact]
# registry            = "nexus"
# local_artifact_path = "dist/app.tar.gz"
# publish_path        = "myteam/app/{{CIABATTA_BRANCH}}/{{CIABATTA_COMMIT}}/app.tar.gz"
#
# Bash script recipe (full control):
# [recipies.my_script]
# bash_script = "scripts/publish.sh"
#
# Push/pull pair (different actions for each direction):
# [recipies.my_docker.push]
# bash_script = "scripts/docker_push.sh"
# [recipies.my_docker.pull]
# bash_script = "scripts/docker_pull.sh"
"#,
        ci_line = ci_line,
        containers = containers,
    )
}

fn detect_ci() -> Option<String> {
    // Check well-known CI environment markers.
    if env::var("GITLAB_CI").is_ok() { return Some("gitlab".into()); }
    if env::var("GITHUB_ACTIONS").is_ok() { return Some("github".into()); }
    if env::var("JENKINS_URL").is_ok() || env::var("BUILD_NUMBER").is_ok() { return Some("jenkins".into()); }
    if env::var("CIRCLECI").is_ok() { return Some("circleci".into()); }
    if env::var("TRAVIS").is_ok() { return Some("travis".into()); }
    if env::var("TF_BUILD").is_ok() { return Some("azure".into()); }
    if env::var("BITBUCKET_BUILD_NUMBER").is_ok() { return Some("bitbucket".into()); }
    None
}

fn list_recipes(cfg: &CiabattaConfig) {
    if cfg.recipes.is_empty() {
        println!("No recipes defined. Add [recipies.<name>] sections to .ciabatta/ciabatta.toml.");
        return;
    }

    println!("Available recipes:");
    let mut names: Vec<_> = cfg.recipes.keys().collect();
    names.sort();
    for name in names {
        let entry = &cfg.recipes[name];
        let kind = match entry {
            config::RecipeEntry::PushPull(_) => "push/pull",
            config::RecipeEntry::Simple(r) => {
                if r.bash_script.is_some() { "bash" } else { "registry" }
            }
        };
        println!("  {:<30} [{}]", name, kind);
    }
}

fn show_config(cfg: &CiabattaConfig, root: &PathBuf) {
    println!("Project root: {}", root.display());

    if let Some(ref sys) = cfg.system {
        println!("\n[system]");
        if let Some(ref ci) = sys.ci { println!("  ci = {}", ci); }
        if let Some(ref c) = sys.containers { println!("  containers = {}", c); }
    }

    if !cfg.registries.is_empty() {
        println!("\nRegistries:");
        for (name, reg) in &cfg.registries {
            println!("  {} -> {} (tls_verify: {}, needs_auth: {})", name, reg.url, reg.tls_verify, reg.needs_auth);
        }
    }

    if !cfg.recipes.is_empty() {
        println!("\nRecipes:");
        let mut names: Vec<_> = cfg.recipes.keys().collect();
        names.sort();
        for name in names {
            println!("  {}", name);
        }
    }
}

fn print_config_help() {
    println!("{}", CONFIG_HELP);
}

const CONFIG_HELP: &str = r#"
Ciabatta Configuration Reference
=================================

Location: <project-root>/.ciabatta/ciabatta.toml

The project root is the directory that CONTAINS the .ciabatta directory.
All paths in recipes are relative to this root.

━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

[system]
  ci         = "gitlab"    # CI/CD system for auto-resolving build variables.
                            # Options: gitlab, github, jenkins, circleci,
                            #          travis, azure, bitbucket
  containers = "docker"    # Container runtime. Options: docker, podman

━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

[registries.<name>]
  url          = "https://..."    # Base URL of the registry (required)
  tls_verify   = true             # Verify TLS certificate (default: true)
  needs_auth   = true             # Whether auth is needed (informational)
  login_script = "./login.sh"     # Optional: run this script before push/pull
  type         = "nexus"          # Override type detection. Options:
                                  # nexus, s3, artifactory, docker, ecr

  Supported registry types:
    nexus       — HTTP PUT/GET to Sonatype Nexus
    s3          — AWS S3 via `aws s3 cp`
    artifactory — HTTP PUT/GET to JFrog Artifactory
    docker      — `docker push` / `docker pull`
    ecr         — AWS ECR (auto-fetches ECR login token if no login_script)

━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

[recipies.<name>]                    ← simple recipe (push and pull use same action)
  registry           = "nexus"       # Registry name from [registries]
  local_artifact_path = "dist/"      # Local path relative to project root
  publish_path       = "group/{CIABATTA_BRANCH}/{CIABATTA_COMMIT}/artifact"
  bash_script        = "scripts/publish.sh"   # Alternative: run a script

[recipies.<name>.push]               ← push/pull recipe with separate actions
  bash_script = "scripts/push.sh"
[recipies.<name>.pull]
  bash_script = "scripts/pull.sh"

━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

Available substitution variables in publish_path:
  {CIABATTA_BRANCH}        Current branch name
  {CIABATTA_COMMIT}        Current commit SHA
  {CIABATTA_TAG}           Current tag (if any)
  {CIABATTA_BUILD_NUMBER}  CI build number

These are populated automatically from the CI system defined in [system].
You can override any of them with: ciabatta run -e CIABATTA_BRANCH=my-branch

━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

Example:

  [system]
  ci         = "github"
  containers = "docker"

  [registries.nexus]
  url          = "https://nexus.example.com/repository/releases/"
  tls_verify   = true
  needs_auth   = true
  login_script = ".ciabatta/nexus_login.sh"

  [recipies.frontend]
  registry            = "nexus"
  local_artifact_path = "frontend/dist"
  publish_path        = "frontend/{CIABATTA_BRANCH}/{CIABATTA_COMMIT}/dist.tar.gz"

  [recipies.backend.push]
  bash_script = "scripts/build_and_push.sh"
  [recipies.backend.pull]
  bash_script = "scripts/pull_backend.sh"
"#;

async fn run_tui_browser() -> Result<()> {
    let (root, cfg) = load_project(None)?;
    let vars = build_env_vars(&cfg, &[])?;
    tui::browser::run_browser(cfg, root, vars).await
}
