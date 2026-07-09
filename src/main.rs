mod analyze;
mod ci;
mod cli;
mod config;
mod configure;
mod deploy;
mod environment;
mod git;
mod registry;
mod runner;
mod todo;
mod tui;
mod watch;

use std::collections::HashMap;
use std::env;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::Parser;

use cli::{Cli, Commands, ConfigCommand, ConfigureCommand};
use config::{CiabattaConfig, find_root, load_config, load_config_file};
use environment::CiabattaEnv;
use runner::RunMode;
use std::collections::BTreeMap;

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    init_logging(cli.debug);
    tracing::debug!("debug logging enabled");

    match cli.command {
        Commands::Push {
            recipes,
            cookbooks,
            env,
            dry_run,
            no_tui,
            local,
            config,
        } => {
            let (root, cfg) = load_project(config.as_deref())?;
            // Only announce resolved variables when we're not about to take over
            // the screen with the TUI (the output would corrupt/close it).
            let vars = build_env_vars(&cfg, &env, local, &root, no_tui)?;
            let names = select_transfer_names(&cfg, &cookbooks, &recipes)?;
            execute_recipes(&cfg, &root, &names, &vars, dry_run, no_tui, RunMode::Push).await?;
        }

        Commands::Pull {
            recipes,
            cookbooks,
            env,
            dry_run,
            no_tui,
            local,
            config,
        } => {
            let (root, cfg) = load_project(config.as_deref())?;
            let vars = build_env_vars(&cfg, &env, local, &root, no_tui)?;
            let names = select_transfer_names(&cfg, &cookbooks, &recipes)?;
            execute_recipes(&cfg, &root, &names, &vars, dry_run, no_tui, RunMode::Pull).await?;
        }

        Commands::Deploy {
            recipes,
            cookbooks,
            env,
            dry_run,
            no_tui,
            local,
            config,
            gui,
            build,
            port,
        } => {
            // --build is an authoring tool: it needs no project and runs nothing.
            if build {
                deploy::server::serve_builder(port).await?;
            } else {
                let (root, cfg) = load_project(config.as_deref())?;
                let mut vars = build_env_vars(&cfg, &env, local, &root, no_tui || gui)?;
                // Auto-source the CIABATTA_* build variables from local git so
                // every deploy script sees CIABATTA_BRANCH/_COMMIT/_TAG/
                // _BUILD_NUMBER/_PATH, even when the run isn't in explicit
                // `--local` or CI mode. Anything already resolved wins.
                source_ciabatta_vars(&mut vars, &root, no_tui && !gui);
                let names = select_deploy_names(&cfg, &cookbooks, &recipes)?;
                if gui {
                    if names.is_empty() {
                        bail!(
                            "No deploy recipes found. Add a [recipies.<name>.deploy] section, or run `ciabatta deploy --build` to design one."
                        );
                    }
                    runner::validate_recipes(&cfg, &root, &names, &vars, &RunMode::Deploy)?;
                    deploy::server::serve_gui(cfg, root, names, vars, dry_run, port).await?;
                } else {
                    execute_recipes(&cfg, &root, &names, &vars, dry_run, no_tui, RunMode::Deploy)
                        .await?;
                }
            }
        }

        Commands::Source { env } => {
            cmd_source(&env)?;
        }

        Commands::List => {
            let (_, cfg) = load_project(None)?;
            list_recipes(&cfg);
        }

        Commands::Init {
            ci,
            containers,
            force,
        } => {
            cmd_init(ci.as_deref(), containers.as_deref(), force)?;
        }

        Commands::Tui => {
            run_tui_browser().await?;
        }

        Commands::Analyze {
            output,
            port,
            no_serve,
            check_vulns,
            requirements,
            trace,
            config,
        } => {
            cmd_analyze(
                config.as_deref(),
                output,
                port,
                no_serve,
                check_vulns,
                requirements,
                trace,
            )
            .await?;
        }

        Commands::Watch {
            command,
            triggers,
            max_lines,
            port,
            no_open,
        } => {
            cmd_watch(command, triggers, max_lines, port, no_open).await?;
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

        Commands::Configure { subcommand } => {
            cmd_configure(subcommand)?;
        }

        Commands::Todo {
            task,
            detach,
            port,
        } => {
            cmd_todo(task, detach, port).await?;
        }
    }

    Ok(())
}

/// Dispatch `ciabatta todo`:
///   - a TASK string adds the task and exits
///   - `-d` spawns a detached copy that serves the web app in the background
///   - otherwise serve the web app in the foreground until Ctrl-C
async fn cmd_todo(task: Option<String>, detach: bool, port: u16) -> Result<()> {
    let store = std::sync::Arc::new(todo::Store::open()?);

    if let Some(text) = task {
        let added = store.add(&text)?;
        println!("Added task #{}: {}", added.id, added.text);
        return Ok(());
    }

    if detach {
        spawn_detached_todo(port)?;
        println!("Todo app started in the background at http://127.0.0.1:{port}");
        println!("Stop it with: pkill -f 'ciabatta todo'");
        return Ok(());
    }

    todo::server::serve(store, port).await
}

/// Dispatch `ciabatta watch <command>`: run the command through the shell,
/// capturing its output into a searchable, live web view. Bookmarks and triggers
/// persist to disk keyed by the command, so they survive restarts.
async fn cmd_watch(
    command: Vec<String>,
    triggers: Vec<String>,
    max_lines: usize,
    port: u16,
    no_open: bool,
) -> Result<()> {
    let command = command.join(" ");
    if command.trim().is_empty() {
        bail!("No command given. Usage: ciabatta watch <command>");
    }

    let store = std::sync::Arc::new(watch::WatchState::new(&command, max_lines)?);
    // Seed any -t triggers from the command line (deduped against persisted ones).
    for phrase in &triggers {
        store.add_trigger(phrase, false)?;
    }

    watch::server::serve(store, command, port, !no_open).await
}

/// Re-launch this executable as a detached background process serving the todo
/// web app (`ciabatta todo --port <port>`), with its stdio discarded so it
/// keeps running after this process exits.
fn spawn_detached_todo(port: u16) -> Result<()> {
    use std::process::{Command, Stdio};

    let exe = env::current_exe().context("Failed to locate the ciabatta executable")?;
    Command::new(exe)
        .arg("todo")
        .arg("--port")
        .arg(port.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("Failed to start the background todo app")?;
    Ok(())
}

/// Initialize the `tracing` subscriber for stderr logging.
///
/// Debug logging turns on when the `--debug` flag is passed OR the
/// `CIABATTA_DEBUG` environment variable is set to any non-empty value other
/// than `0`/`false`. For finer-grained control the `CIABATTA_LOG` environment
/// variable is honored directly as a `tracing` env-filter (e.g.
/// `CIABATTA_LOG=ciabatta=trace`), overriding the flag-derived default.
fn init_logging(debug_flag: bool) {
    use tracing_subscriber::{EnvFilter, fmt};

    let debug = debug_flag
        || env::var("CIABATTA_DEBUG")
            .map(|v| {
                let v = v.trim();
                !v.is_empty() && v != "0" && !v.eq_ignore_ascii_case("false")
            })
            .unwrap_or(false);

    let default_directive = if debug { "ciabatta=debug" } else { "ciabatta=warn" };
    let filter = EnvFilter::try_from_env("CIABATTA_LOG")
        .unwrap_or_else(|_| EnvFilter::new(default_directive));

    // Best-effort: ignore the error if a subscriber is somehow already set.
    let _ = fmt()
        .with_env_filter(filter)
        .with_target(false)
        .with_writer(std::io::stderr)
        .try_init();
}

/// Dispatch `ciabatta configure` (interactive registry setup) and its `auto`
/// subcommand (analyze the project and suggest recipes).
fn cmd_configure(subcommand: Option<ConfigureCommand>) -> Result<()> {
    let cwd = env::current_dir().context("Failed to get current directory")?;
    // configure works whether or not the project is initialized yet: prefer an
    // existing .ciabatta root, otherwise target the current directory.
    let root = find_root(&cwd).unwrap_or(cwd);
    let cfg = load_config(&root)?;

    match subcommand {
        Some(ConfigureCommand::Auto { yes }) => configure::run_auto(&root, &cfg, yes),
        None => configure::run_interactive(&root, &cfg),
    }
}

fn load_project(config_path: Option<&std::path::Path>) -> Result<(PathBuf, CiabattaConfig)> {
    let cwd = env::current_dir().context("Failed to get current directory")?;

    if let Some(p) = config_path {
        // Explicit path: load exactly this file, and derive the project root
        // (used to resolve relative recipe paths) from its location.
        let cfg = load_config_file(p)?;
        let root = resolve_root_for_config(p, &cwd);
        Ok((root, cfg))
    } else {
        // Walk upward from cwd until a .ciabatta/ directory is found.
        let root = find_root(&cwd).ok_or_else(|| {
            anyhow::anyhow!(
                "No .ciabatta/ directory found in '{}' or any parent directory.\n\
                 Create one and add a ciabatta.toml to get started.\n\
                 Run `ciabatta config reference` for format documentation.",
                cwd.display()
            )
        })?;
        let cfg = load_config(&root)?;
        Ok((root, cfg))
    }
}

/// Determine the project root for an explicit `--config` file: normalize it to
/// an absolute path, then apply [`root_from_config_path`]. Falls back to `cwd`
/// when the file has no usable parent.
fn resolve_root_for_config(config_path: &Path, cwd: &Path) -> PathBuf {
    let abs = std::fs::canonicalize(config_path).unwrap_or_else(|_| cwd.join(config_path));
    root_from_config_path(&abs).unwrap_or_else(|| cwd.to_path_buf())
}

/// Derive the project root from an absolute config-file path. When the file
/// lives in a `.ciabatta/` directory (the standard layout) the root is the
/// directory that contains `.ciabatta`; otherwise it's the file's own parent
/// directory, so relative recipe paths resolve alongside the config.
fn root_from_config_path(config_abs: &Path) -> Option<PathBuf> {
    let parent = config_abs.parent()?;
    if parent.file_name() == Some(std::ffi::OsStr::new(config::CIABATTA_DIR)) {
        Some(parent.parent().unwrap_or(parent).to_path_buf())
    } else {
        Some(parent.to_path_buf())
    }
}

/// Build the final environment variable map:
/// 1. Start with the current process env
/// 2. Merge CIABATTA_* vars from local git (`--local`) or the configured CI
/// 3. Override with CLI -e flags (highest priority)
/// 4. Derive CIABATTA_PATH
///
/// When `announce` is true the resolved variables are echoed to stderr; callers
/// that hand off to the TUI pass `false`, since that output would corrupt the
/// alternate screen.
fn build_env_vars(
    cfg: &CiabattaConfig,
    cli_env: &[String],
    local: bool,
    root: &Path,
    announce: bool,
) -> Result<HashMap<String, String>> {
    let mut vars: HashMap<String, String> = std::env::vars().collect();

    // Local mode is selected by the `--local` flag OR by `CIABATTA_ENV=local`.
    let env = CiabattaEnv::detect_with_flag(local);

    if env.is_local() {
        // Local development: resolve build variables from git history. These
        // take precedence over any stale ambient CIABATTA_* in the environment.
        let git_vars = env.resolve_vars(root)?;
        if announce && !git_vars.is_empty() {
            eprintln!("CIABATTA variables resolved from local git:");
            for (k, v) in sorted(&git_vars) {
                eprintln!("  {k} = {v}");
            }
        }
        vars.extend(git_vars);
        // Record the mode so the runner (pull best-hash fallback) and any child
        // processes see it, even when it was turned on by the `--local` flag.
        vars.insert(environment::ENV_VAR.to_string(), environment::LOCAL.to_string());
    } else if let Some(ref system) = cfg.system
        && let Some(ref ci_name) = system.ci
    {
        // Resolve CI variables and (optionally) print them.
        let ci_system = ci::CiSystem::from(ci_name.as_str());
        let (ci_vars, resolved) = ci::resolve_ci_vars(&ci_system);
        if announce && !resolved.is_empty() {
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

    // CLI -e flags override everything.
    let cli_map = cli::parse_env_flags(cli_env)?;
    vars.extend(cli_map);

    // Derive CIABATTA_PATH from the now-fully-resolved variables, unless the
    // user set it explicitly (via -e or the environment).
    if let Some(path) = derive_ciabatta_path(&vars) {
        vars.entry("CIABATTA_PATH".to_string()).or_insert(path);
    }

    if tracing::enabled!(tracing::Level::DEBUG) {
        for (k, v) in sorted(&vars) {
            if k.starts_with("CIABATTA_") {
                tracing::debug!(var = %k, value = %v, "resolved ciabatta variable");
            }
        }
    }

    Ok(vars)
}

/// Compute the `CIABATTA_PATH` convenience variable:
///   - a tag (CLI/env `CIABATTA_TAG`) wins → `/{CIABATTA_TAG}/`
///   - otherwise → `/{CIABATTA_BRANCH}/{CIABATTA_COMMIT}`
///
/// Returns `None` when there isn't enough information to build it (no tag and no
/// branch), so callers leave `CIABATTA_PATH` unset rather than emitting `//`.
fn derive_ciabatta_path(vars: &HashMap<String, String>) -> Option<String> {
    let non_empty = |key: &str| vars.get(key).filter(|v| !v.is_empty()).cloned();

    if let Some(tag) = non_empty("CIABATTA_TAG") {
        return Some(format!("/{tag}/"));
    }
    let branch = non_empty("CIABATTA_BRANCH")?;
    let commit = non_empty("CIABATTA_COMMIT").unwrap_or_default();
    Some(format!("/{branch}/{commit}"))
}

/// Return a map's entries sorted by key, for stable human-facing output.
fn sorted(vars: &HashMap<String, String>) -> BTreeMap<&String, &String> {
    vars.iter().collect()
}

/// `ciabatta source`: resolve the CIABATTA_* build variables from local git
/// (plus the derived CIABATTA_PATH) and print them as shell `export` lines so a
/// developer can load them with `eval "$(ciabatta source)"`.
fn cmd_source(cli_env: &[String]) -> Result<()> {
    let cwd = env::current_dir().context("Failed to get current directory")?;

    let mut vars = git::local_git_vars(&cwd)?;

    // CLI -e flags override the git-derived values, then derive CIABATTA_PATH.
    vars.extend(cli::parse_env_flags(cli_env)?);
    if let Some(path) = derive_ciabatta_path(&vars) {
        vars.entry("CIABATTA_PATH".to_string()).or_insert(path);
    }

    println!("# ciabatta environment (eval \"$(ciabatta source)\" to load)");
    for (k, v) in sorted(&vars) {
        println!("export {k}={}", shell_quote(v));
    }
    Ok(())
}

/// Auto-source the `CIABATTA_*` build variables from local git into an existing
/// variable map, filling only the ones that aren't already set (so values from
/// CI, the ambient environment, or `-e` win). Used by `ciabatta deploy` so a
/// deploy's scripts always see the resolved build variables — the same set
/// `ciabatta source` prints — without the operator having to `eval` them first.
///
/// A non-git directory (or any git error) is not fatal: the deploy simply runs
/// without git-derived variables, exactly as it would today.
fn source_ciabatta_vars(vars: &mut HashMap<String, String>, root: &Path, announce: bool) {
    let git_vars = match git::local_git_vars(root) {
        Ok(v) => v,
        Err(e) => {
            tracing::debug!(error = %e, "deploy: could not source CIABATTA_* from local git");
            return;
        }
    };
    let mut added: Vec<String> = Vec::new();
    for (k, v) in git_vars {
        if v.is_empty() {
            continue;
        }
        let slot = vars.entry(k.clone()).or_default();
        if slot.is_empty() {
            *slot = v;
            added.push(k);
        }
    }
    // Derive CIABATTA_PATH from the now-augmented set, if it isn't set already.
    if vars.get("CIABATTA_PATH").map(|v| v.is_empty()).unwrap_or(true)
        && let Some(path) = derive_ciabatta_path(vars)
    {
        vars.insert("CIABATTA_PATH".to_string(), path);
        added.push("CIABATTA_PATH".to_string());
    }
    if announce && !added.is_empty() {
        added.sort();
        eprintln!("Sourced CIABATTA variables from local git for deploy:");
        for k in &added {
            if let Some(v) = vars.get(k) {
                eprintln!("  {k} = {v}");
            }
        }
    }
}

/// Single-quote a value for safe inclusion in a shell `export`.
fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', r"'\''"))
}

/// Resolve which recipes a push/pull run targets. Like
/// [`config::select_recipe_names`] but deploy-only recipes — a `[deploy]` section
/// with no push/pull transfer action — are dropped: they're pure deployment
/// tasks, so `ciabatta push`/`pull` skips them instead of failing on
/// "no push/pull action".
fn select_transfer_names(
    cfg: &CiabattaConfig,
    cookbooks: &[String],
    recipes: &[String],
) -> Result<Vec<String>> {
    let names = config::select_recipe_names(cfg, cookbooks, recipes)?;
    Ok(names
        .into_iter()
        .filter(|n| cfg.recipes.get(n).is_none_or(|e| !e.is_deploy_only()))
        .collect())
}

/// Resolve which recipes a deploy run targets. Like [`config::select_recipe_names`]
/// but the "everything" default is narrowed to deploy-capable recipes only, and
/// any explicitly named recipe must actually define a `[deploy]` section.
fn select_deploy_names(
    cfg: &CiabattaConfig,
    cookbooks: &[String],
    recipes: &[String],
) -> Result<Vec<String>> {
    if cookbooks.is_empty() && recipes.is_empty() {
        let mut names: Vec<String> = cfg
            .recipes
            .iter()
            .filter(|(_, e)| e.deploy_recipe().is_some())
            .map(|(n, _)| n.clone())
            .collect();
        names.sort();
        return Ok(names);
    }

    let names = config::select_recipe_names(cfg, cookbooks, recipes)?;
    for name in &names {
        let entry = cfg
            .recipes
            .get(name)
            .ok_or_else(|| anyhow::anyhow!("Recipe '{}' not found", name))?;
        if entry.deploy_recipe().is_none() {
            bail!(
                "Recipe '{}' has no [deploy] definition, so it can't be deployed. \
                 Add a [recipies.{}.deploy] section (see `ciabatta config reference`).",
                name,
                name
            );
        }
    }
    Ok(names)
}

async fn execute_recipes(
    cfg: &CiabattaConfig,
    root: &Path,
    names: &[String],
    vars: &HashMap<String, String>,
    dry_run: bool,
    no_tui: bool,
    mode: RunMode,
) -> Result<()> {
    if names.is_empty() {
        bail!(
            "No recipes found. Run `ciabatta list` to see available recipes, or check your .ciabatta/ciabatta.toml."
        );
    }

    // Validate publish-path variables (push/pull) or the step DAG (deploy)
    // before launching.
    runner::validate_recipes(cfg, root, names, vars, &mode)?;

    // Resolve the container runtime once up front so every recipe shares it and
    // an ambiguous/missing runtime fails fast (before any work starts). Deploys
    // run scripts, not built-in container actions, so a missing runtime there is
    // best-effort rather than fatal.
    let mut cfg = cfg.clone();
    match config::resolve_container_cmd(&cfg) {
        Ok(container_cmd) => {
            cfg.system.get_or_insert_with(Default::default).containers = Some(container_cmd);
        }
        Err(e) if mode == RunMode::Deploy => {
            tracing::debug!("no container runtime resolved for deploy: {e}");
        }
        Err(e) => return Err(e),
    }
    let cfg = &cfg;

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
    root: &Path,
    names: &[String],
    vars: &HashMap<String, String>,
    dry_run: bool,
    mode: RunMode,
) -> Result<()> {
    use runner::ProgressUpdate;
    use tokio::sync::mpsc;

    let (tx, mut rx) = mpsc::channel::<ProgressUpdate>(256);

    let cfg_clone = cfg.clone();
    let root_clone = root.to_path_buf();
    let names_clone = names.to_vec();
    let vars_clone = vars.clone();

    tokio::spawn(async move {
        let _ = runner::run_all(
            &cfg_clone,
            &root_clone,
            &names_clone,
            &vars_clone,
            dry_run,
            mode,
            tx,
        )
        .await;
    });

    let mut any_failed = false;
    while let Some(update) = rx.recv().await {
        match update {
            ProgressUpdate::Started(name) => println!("[{name}] started"),
            ProgressUpdate::StageStarted { recipe, stage } => {
                println!("[{recipe}] ▶ {}", stage.label(mode))
            }
            ProgressUpdate::StageFinished { recipe, stage, ran } => {
                if !ran {
                    println!(
                        "[{recipe}]   {} (default, nothing to do)",
                        stage.label(mode)
                    );
                }
            }
            ProgressUpdate::TransferProgress {
                recipe,
                done,
                total,
            } => {
                let pct = if total > 0 { done * 100 / total } else { 0 };
                println!("[{recipe}]   {done}/{total} files ({pct}%)");
            }
            ProgressUpdate::Log(name, line) => println!("[{name}] {line}"),
            ProgressUpdate::StepStarted { recipe, step } => {
                println!("[{recipe}] ▶ step: {step}")
            }
            ProgressUpdate::StepFinished { recipe, step, ok } => {
                println!("[{recipe}]   {} step: {step}", if ok { "✓" } else { "✗" })
            }
            ProgressUpdate::StepLog { recipe, step, line } => {
                println!("[{recipe}]   [{step}] {line}")
            }
            ProgressUpdate::StepNeedsChoice {
                recipe,
                step,
                message,
                options,
            } => {
                println!("[{recipe}] ⚠ {step}: {message}");
                for (i, opt) in options.iter().enumerate() {
                    println!("[{recipe}]     [{i}] {opt}");
                }
            }
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

fn cmd_init(ci: Option<&str>, containers: Option<&str>, force: bool) -> Result<()> {
    use config::{CIABATTA_DIR, CONFIG_FILE};
    use std::fs;

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

    fs::create_dir_all(&dir).with_context(|| format!("Failed to create {}", dir.display()))?;

    let toml = build_starter_toml(ci, containers);
    fs::write(&config_path, &toml)
        .with_context(|| format!("Failed to write {}", config_path.display()))?;

    println!("Initialized ciabatta project in {}", cwd.display());
    println!("Created: {}", config_path.display());
    println!();
    println!("Next steps:");
    println!("  1. Edit .ciabatta/ciabatta.toml to define your registries and recipes.");
    println!("  2. Run `ciabatta list` to verify your recipes are recognized.");
    println!("  3. Run `ciabatta push --dry-run <recipe>` to preview what will happen.");
    println!("  4. Run `ciabatta tui` to open the interactive browser.");
    println!();
    println!("For config format documentation: ciabatta config reference");

    Ok(())
}

fn build_starter_toml(ci: Option<&str>, containers: Option<&str>) -> String {
    // When the runtime isn't pinned, leave it commented out so ciabatta
    // auto-detects podman/docker at run time.
    let containers_line = match containers {
        Some(c) => format!("containers = {c:?}"),
        None => {
            r#"# containers = "docker"  # docker | podman (auto-detected when unset)"#.to_string()
        }
    };

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

    format!(
        r#"# Ciabatta configuration
# Run `ciabatta config reference` for full documentation.

[system]
{ci_line}
{containers_line}

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
#
# ─── Stages ────────────────────────────────────────────────────────────────────
# Every push runs four stages: login → pre-push → push → post-push
# Every pull runs four stages:  login → pre-pull → pull → post-pull
# Override any stage with an arbitrary command (bash, python, a binary, …):
#   login = "..."   pre = "..."   main = "..."   post = "..."
# Unset stages use their defaults (login uses the registry login_script or
# CIABATTA_<REGISTRY>_USER/PASS credentials; pre/post do nothing; main runs the
# built-in registry action). Commands get all CIABATTA_* vars in their env.
#
# [recipies.frontend.push]
# pre  = "python scripts/bundle.py"
# post = "./scripts/notify.sh deployed"
#
# ─── Deploys ───────────────────────────────────────────────────────────────────
# `ciabatta deploy <recipe>` runs a DAG of dependent script steps (login →
# pre-deploy → deploy → post-deploy). The steps live in a separate flowchart
# file; each step runs a script and may declare `needs` and an `on_error`
# recovery node. See `ciabatta config reference`, or design one visually with
# `ciabatta deploy --build` (and watch a run with `ciabatta deploy <r> --gui`).
#
# [recipies.web.deploy]
# flowchart = ".ciabatta/deploys.toml"   # each entry is a series of steps
#
# ─── Credentials ───────────────────────────────────────────────────────────────
# When a registry has no login_script, ciabatta reads per-registry credentials:
#   CIABATTA_<REGISTRY>_USER  /  CIABATTA_<REGISTRY>_PASS
# e.g. for [registries.nexus]: CIABATTA_NEXUS_USER / CIABATTA_NEXUS_PASS.
# Nexus/Artifactory use them for HTTP basic auth; docker runs `docker login`.
"#,
        ci_line = ci_line,
        containers_line = containers_line,
    )
}

fn detect_ci() -> Option<String> {
    // Check well-known CI environment markers.
    if env::var("GITLAB_CI").is_ok() {
        return Some("gitlab".into());
    }
    if env::var("GITHUB_ACTIONS").is_ok() {
        return Some("github".into());
    }
    if env::var("JENKINS_URL").is_ok() || env::var("BUILD_NUMBER").is_ok() {
        return Some("jenkins".into());
    }
    if env::var("CIRCLECI").is_ok() {
        return Some("circleci".into());
    }
    if env::var("TRAVIS").is_ok() {
        return Some("travis".into());
    }
    if env::var("TF_BUILD").is_ok() {
        return Some("azure".into());
    }
    if env::var("BITBUCKET_BUILD_NUMBER").is_ok() {
        return Some("bitbucket".into());
    }
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
        let push = entry.push_recipe();
        // A recipe can define a deploy alongside a push/pull action; when it's
        // deploy-only, prefer the "deploy" label over the transfer defaults.
        let transfer_kind = if entry.push.is_some() || entry.pull.is_some() {
            Some("push/pull")
        } else if push.main.is_some() || push.bash_script.is_some() {
            Some("command")
        } else if push.registry.is_some() || push.publish_path.is_some() {
            Some("registry")
        } else {
            None
        };
        let kind = match (transfer_kind, entry.deploy.is_some()) {
            (Some(t), true) => format!("{t}, deploy"),
            (Some(t), false) => t.to_string(),
            (None, true) => "deploy".to_string(),
            (None, false) => "registry".to_string(),
        };
        println!("  {:<30} [{}]", name, kind);
    }

    if !cfg.menus.is_empty() {
        println!("\nMenus (run with --cookbook <name>):");
        let mut menus: Vec<_> = cfg.menus.keys().collect();
        menus.sort();
        for name in menus {
            println!("  {:<30} {}", name, cfg.menus[name].join(", "));
        }
    }
}

fn show_config(cfg: &CiabattaConfig, root: &Path) {
    println!("Project root: {}", root.display());

    if let Some(ref sys) = cfg.system {
        println!("\n[system]");
        if let Some(ref ci) = sys.ci {
            println!("  ci = {}", ci);
        }
        if let Some(ref c) = sys.containers {
            println!("  containers = {}", c);
        }
    }

    if !cfg.registries.is_empty() {
        println!("\nRegistries:");
        for (name, reg) in &cfg.registries {
            println!(
                "  {} -> {} (tls_verify: {}, needs_auth: {})",
                name, reg.url, reg.tls_verify, reg.needs_auth
            );
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

    if !cfg.menus.is_empty() {
        println!("\nMenus:");
        let mut names: Vec<_> = cfg.menus.keys().collect();
        names.sort();
        for name in names {
            println!("  {} -> {}", name, cfg.menus[name].join(", "));
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
  containers = "docker"    # Container runtime. Options: docker, podman.
                            # When unset, ciabatta auto-detects what's installed:
                            # it prefers podman, falls back to docker, and asks
                            # you to choose if BOTH are present.

━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

[analyze]                  # Optional inputs for `ciabatta analyze`
  requirements = "reqs.txt"  # File of requirements: `id` or `id, description`
  trace        = "trace.csv" # CSV of `requirement,file` connections
                             # (paths are relative to the project root;
                             #  --requirements / --trace override these)

━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

[registries.<name>]
  url          = "https://..."    # Base URL of the registry (required)
  tls_verify   = true             # Verify TLS certificate (default: true)
  needs_auth   = true             # Whether auth is needed (informational)
  login_script = "./login.sh"     # Optional: run this script before push/pull
  type         = "nexus"          # Override type detection. Options:
                                  # nexus, s3, artifactory, docker, ecr

  Nexus-only fields (select the target repository and format):
  repository   = "raw-hosted"     # Nexus repo name. When set, `url` is the bare
                                  # Nexus host and /repository/<repository> is
                                  # appended automatically. When unset, `url` is
                                  # used as the full repository URL.
  base_path    = "builds"         # raw only: prefix prepended to every recipe's
                                  # publish_path (where raw files land)
  format       = "raw"            # Nexus repository format. Options:
                                  #   raw  → HTTP PUT/GET      (default)
                                  #   npm  → `npm publish`
                                  #   pypi → `twine upload`

  Example — publish an npm package to a Nexus npm repo:
    [registries.npm]
    type       = "nexus"
    url        = "http://localhost:8527"
    repository = "npm-hosted"
    format     = "npm"

  Auth for all formats uses CIABATTA_<NAME>_USER / _PASS (npm also accepts a
  CIABATTA_<NAME>_TOKEN bearer token). npm requires `npm` on PATH; pypi requires
  `twine`. For npm/pypi recipes, `local_artifact_path` is the package tarball or
  the `dist/` directory to publish; `publish_path` is not used.

  The `url` and `login_script` fields expand environment variables, with
  bash-style defaults, so one config can target different environments:

    url = "https://${NEXUS_HOST:-nexus.example.com}/repository/releases/"

    ${VAR}            value of VAR (empty if unset)
    ${VAR:-default}   VAR if set & non-empty, otherwise `default`
    ${VAR-default}    VAR if set (even if empty), otherwise `default`
    {VAR:-default}    the leading `$` is optional

  Supported registry types:
    nexus       — Sonatype Nexus (raw HTTP PUT/GET, or npm/pypi via `format`)
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

[recipies.<name>]                    ← docker/ecr image recipe
  registry     = "myecr"             # a docker- or ecr-type registry
  local_image  = "app:latest"        # a locally-built image (name or name:tag)
  publish_path = "app:{CIABATTA_COMMIT}"   # remote image ref (repo[:tag])
  # ciabatta retags local_image to <registry url>/<publish_path> and pushes it,
  # so you don't bake the registry URL into your build. `pull` retags the pulled
  # image back to local_image. When publish_path is omitted, local_image is
  # reused as the remote reference.

[recipies.<name>.push]               ← push/pull recipe with separate actions
  bash_script = "scripts/push.sh"
[recipies.<name>.pull]
  bash_script = "scripts/pull.sh"

━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

Deploys — a DAG of dependent script steps (`ciabatta deploy`)

  A deploy is a third recipe direction (alongside push/pull) that runs a graph
  of dependent script "steps" instead of a registry transfer. It runs the same
  four phases: login → pre-deploy → deploy → post-deploy. The `deploy` phase
  executes the step DAG; login/pre/post are optional command hooks.

  [recipies.<name>.deploy]
    flowchart = ".ciabatta/deploys.toml"   # separate file holding the steps
    entry     = "web"                       # entry to use (default: recipe name)
    login = "..."   pre = "..."   post = "..."   # optional phase hooks

  The flowchart file lists steps. Each step runs a `script` (a bash file) or an
  inline `run` command, and may declare `needs` (steps that must succeed first)
  and `on_error` (jump to a recovery node on failure). Steps with satisfied
  `needs` are eligible to run; the graph must be acyclic.

    # .ciabatta/deploys.toml
    [web]
      [[web.steps]]
      name = "build"
      script = "scripts/build.sh"

      [[web.steps]]
      name = "migrate"
      script  = "scripts/migrate.sh"
      needs   = ["build"]
      on_error = "fix_migrate"        # on failure, go to the recovery node

      [[web.steps]]                   # a recovery node: offers fix choices
      name = "fix_migrate"
      recover = true
      message = "Migration failed — choose how to recover:"
      retry   = "migrate"             # re-run this step after a successful fix
      options = [
        { label = "Roll back",   script = "scripts/rollback.sh" },
        { label = "Force unlock", run = "make unlock", default = true },
      ]

      [[web.steps]]
      name = "release"
      script = "scripts/release.sh"
      needs  = ["migrate"]

  Recovery: when a step with `on_error` fails, its recovery node offers a choice
  of fix `options`. With `--gui` you pick one in the browser; otherwise (plain /
  CI) the option marked `default = true` runs automatically, or the deploy fails
  if none is. After a fix succeeds, `retry` re-runs the named step. Retry loops
  are bounded so a persistently failing step can't spin forever.

    ciabatta deploy [RECIPE…]        run deploy recipes (all deploy-capable if none named)
    ciabatta deploy web --gui        live web view: flowchart, logs, fix-it buttons
    ciabatta deploy --build          open the visual builder; copy the TOML it emits

━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

[menus]                              ← group recipes so you can run a subset

  A menu names a list of recipes. `ciabatta push --cookbook <menu>` (or
  `--menu <menu>`) runs only the recipes on that menu, instead of naming each
  recipe by hand or pushing everything.

    [menus]
    frontend = ["release_frontend", "release_assets"]
    backend  = ["release_backend"]
    release  = ["release_frontend", "release_assets", "release_backend"]

  Usage:
    ciabatta push --cookbook frontend            # just the frontend menu
    ciabatta push --cookbook frontend --cookbook backend   # both menus
    ciabatta push --cookbook release extra_recipe          # menu + a recipe

  --cookbook is repeatable and combines with any recipe names given on the
  command line; the union runs once (duplicates are de-duplicated). The same
  flag works for `ciabatta pull`. Referencing an undefined menu, or a menu that
  lists a recipe that doesn't exist, is an error.

━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

Stages (state machine)

  Each push runs:  login → pre-push → push → post-push
  Each pull runs:  login → pre-pull → pull → post-pull

  Override any stage with an arbitrary command (bash, python, a compiled
  binary, …). Unset stages fall back to their defaults.

    login = "..."   # default: registry login_script, or CIABATTA_<REG>_USER/PASS
    pre   = "..."   # default: nothing
    main  = "..."   # default: the built-in registry push/pull (or bash_script)
    post  = "..."   # default: nothing

  Stage commands run via `sh -c` from the project root, with every CIABATTA_*
  and CI variable available in their environment (use $CIABATTA_COMMIT, etc.).

  [recipies.frontend.push]      # overrides only apply to the push direction
    pre  = "python scripts/bundle.py"
    post = "./scripts/notify.sh deployed"

━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

Credentials (when a registry has no login_script)

  ciabatta reads per-registry credentials from the environment:
    CIABATTA_<REGISTRY>_USER   CIABATTA_<REGISTRY>_PASS
  where <REGISTRY> is the registry's section name, uppercased. For example,
  [registries.nexus] → CIABATTA_NEXUS_USER / CIABATTA_NEXUS_PASS.

    nexus / artifactory  → sent as HTTP basic auth
    docker               → `docker login <host> -u $USER --password-stdin`
    ecr                  → `aws ecr get-login-password` (credentials not needed)
    s3                   → uses the standard AWS credential chain

━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

Available substitution variables in publish_path:
  {CIABATTA_BRANCH}        Current branch name
  {CIABATTA_COMMIT}        Current commit SHA
  {CIABATTA_TAG}           Current tag (if any)
  {CIABATTA_BUILD_NUMBER}  CI build number
  {CIABATTA_PATH}          Convenience path, derived as:
                             /{CIABATTA_TAG}/                      (when a tag is set)
                             /{CIABATTA_BRANCH}/{CIABATTA_COMMIT}  (otherwise)

These are populated automatically from the CI system defined in [system].
You can override any of them with: ciabatta push -e CIABATTA_BRANCH=my-branch

Working locally? `ciabatta push --local` (or `export CIABATTA_ENV=local`) derives
CIABATTA_BRANCH / _COMMIT / _TAG / _BUILD_NUMBER from your local git history
instead of CI. On any `ciabatta pull` (local or CI), when the exact commit has no
published artifact ciabatta falls back to the newest commit on the branch that
does. Run `ciabatta source` to print the variables as shell `export` lines:

    eval "$(ciabatta source)"

━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

publish_path: a single remote path, or a list of local file globs

  # Single remote destination (supports {VAR} substitution):
  publish_path = "team/app/{CIABATTA_COMMIT}/app.tar.gz"

  # A list of local globs: each matched file uploads under {CIABATTA_PATH},
  # preserving its path relative to the project root. `strip_prefix` trims a
  # leading fragment from that relative path first.
  publish_path = ["dist/*.tar.gz", "build/*.bin"]
  strip_prefix = "dist/"        # dist/app.tar.gz -> {CIABATTA_PATH}/app.tar.gz

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
    let (root, mut cfg) = load_project(None)?;
    // Best-effort: resolve the container runtime so on-demand pushes use the
    // right one. The browser is also useful for non-container registries, so an
    // ambiguous/missing runtime shouldn't block opening it — the push itself
    // will surface the error if a container action is actually invoked.
    if let Ok(c) = config::resolve_container_cmd(&cfg) {
        cfg.system.get_or_insert_with(Default::default).containers = Some(c);
    }
    // announce = false: the browser owns the screen, so don't print var output.
    let vars = build_env_vars(&cfg, &[], false, &root, false)?;
    tui::browser::run_browser(cfg, root, vars).await
}

#[allow(clippy::too_many_arguments)]
async fn cmd_analyze(
    config_path: Option<&Path>,
    output: Option<PathBuf>,
    port: u16,
    no_serve: bool,
    check_vulns: bool,
    requirements: Option<PathBuf>,
    trace: Option<PathBuf>,
) -> Result<()> {
    let cwd = env::current_dir().context("Failed to get current directory")?;

    // Analyze works with or without a .ciabatta project: an explicit config
    // path is loaded directly (root derived from its location); otherwise fall
    // back to the nearest .ciabatta, else the cwd.
    let (root, cfg) = match config_path {
        Some(p) => (resolve_root_for_config(p, &cwd), load_config_file(p)?),
        None => {
            let root = find_root(&cwd).unwrap_or_else(|| cwd.clone());
            let cfg = load_config(&root)?;
            (root, cfg)
        }
    };

    // CLI flags win; otherwise fall back to [analyze] in the config (paths there
    // are relative to the project root).
    let requirements_path = requirements.or_else(|| {
        cfg.analyze
            .as_ref()
            .and_then(|a| a.requirements.as_ref())
            .map(|p| root.join(p))
    });
    let trace_path = trace.or_else(|| {
        cfg.analyze
            .as_ref()
            .and_then(|a| a.trace.as_ref())
            .map(|p| root.join(p))
    });
    let inputs = analyze::RequirementInputs {
        requirements_file: requirements_path.as_deref(),
        trace_file: trace_path.as_deref(),
    };

    let mut graph = analyze::analyze(&root, &cfg, &inputs)?;

    if check_vulns {
        println!("Querying OSV for known vulnerabilities…");
        if let Err(e) = analyze::check_vulnerabilities(&mut graph).await {
            eprintln!("warning: vulnerability check failed: {e}");
        }
    }

    let json = serde_json::to_string_pretty(&graph)?;
    let out = output.unwrap_or_else(|| cwd.join("ciabatta-analyze.json"));
    std::fs::write(&out, &json).with_context(|| format!("Failed to write {}", out.display()))?;

    let externals = graph
        .nodes
        .iter()
        .filter(|n| n.category == analyze::Category::External)
        .count();
    let internals = graph
        .nodes
        .iter()
        .filter(|n| n.category == analyze::Category::Internal)
        .count();
    let publishes = graph
        .nodes
        .iter()
        .filter(|n| n.category == analyze::Category::Publish)
        .count();

    println!("Wrote {}", out.display());
    println!(
        "  {} external · {} internal · {} publish · {} edges",
        externals,
        internals,
        publishes,
        graph.edges.len()
    );

    if !no_serve {
        analyze::server::serve(json, port).await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::root_from_config_path;
    use std::path::{Path, PathBuf};

    #[test]
    fn root_from_ciabatta_layout_is_two_levels_up() {
        assert_eq!(
            root_from_config_path(Path::new("/proj/.ciabatta/ciabatta.toml")),
            Some(PathBuf::from("/proj"))
        );
        assert_eq!(
            root_from_config_path(Path::new("/a/b/.ciabatta/custom.toml")),
            Some(PathBuf::from("/a/b"))
        );
    }

    #[test]
    fn root_from_arbitrary_file_is_its_parent() {
        // A config that isn't inside a `.ciabatta/` dir roots at its own folder,
        // so relative recipe paths resolve alongside it.
        assert_eq!(
            root_from_config_path(Path::new("/proj/ciabatta.toml")),
            Some(PathBuf::from("/proj"))
        );
        assert_eq!(
            root_from_config_path(Path::new("/proj/configs/build.toml")),
            Some(PathBuf::from("/proj/configs"))
        );
    }
}
