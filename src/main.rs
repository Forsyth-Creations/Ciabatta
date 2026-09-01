mod ai;
mod analyze;
mod cache;
mod ci;
mod cli;
mod color;
mod config;
mod configure;
mod convert;
mod daemon;
mod environment;
mod example;
mod format;
mod git;
mod lsp;
mod migrate;
mod registry;
mod remote_cache;
mod run;
mod runner;
mod todo;
mod tui;
mod watch;
mod workspace;

use std::collections::HashMap;
use std::env;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::Parser;
use owo_colors::OwoColorize;

use cli::{
    AiCommand, CacheCommand, Cli, Commands, ConfigCommand, ConfigureCommand, DaemonCommand,
    RemoteCacheCommand, SelfCommand,
};
use config::{CiabattaConfig, find_root, load_config, load_config_file};
use environment::CiabattaEnv;
use std::collections::BTreeMap;

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    // `daemon serve` installs its own subscriber, writing to
    // ~/.ciabatta/daemon.log. It has to be the only one: `try_init` cannot
    // replace a subscriber, so installing the CLI's stderr one here first meant
    // the daemon's file logger silently lost the race and every daemon log line
    // went to a stderr that is /dev/null in the background.
    let daemon_serve = matches!(
        &cli.command,
        Commands::Daemon {
            subcommand: DaemonCommand::Serve { .. }
        }
    );
    // A server run in the foreground is expected to narrate what it serves:
    // its request log is the whole point of starting it in a terminal, and
    // `warn` would show an operator nothing at all. Every other command keeps
    // the quiet default — a CLI that logs at itself is noise.
    let server_foreground = matches!(
        &cli.command,
        Commands::RemoteCache {
            subcommand: RemoteCacheCommand::Start { .. }
        }
    );
    if !daemon_serve {
        init_logging(cli.debug, server_foreground);
    }
    tracing::debug!("debug logging enabled");

    match cli.command {
        Commands::Source { env } => {
            cmd_source(&env)?;
        }

        Commands::Workflow(args) => {
            cmd_workflow(args, false).await?;
        }

        // An unrecognized subcommand is a workflow name. Re-parse the raw argv
        // through the very same parser `ciabatta workflow` uses, so the flags
        // behave identically either way.
        Commands::External(argv) => {
            let invocation = cli::WorkflowInvocation::try_parse_from(
                std::iter::once("ciabatta".to_string()).chain(argv),
            )
            .map_err(|e| anyhow::anyhow!(e.to_string()))?;
            cmd_workflow(invocation.args, true).await?;
        }

        Commands::List {
            search,
            verbose,
            workflows,
        } => {
            cmd_list(search.as_deref(), verbose, workflows)?;
        }

        Commands::Init {
            lib,
            example,
            into,
            nexus,
            docker,
            all,
            name,
            description,
            owner,
            depends_on,
            workflow,
            ci,
            containers,
            force,
            no_register,
            port,
        } => {
            // Where the new workspace ended up. `--example` writes into a
            // sub-directory, so it reports its own root rather than the cwd.
            let root = if example {
                example::generate(&example::Options {
                    into,
                    nexus: nexus || all,
                    docker: docker || all,
                    force,
                })?
            } else if lib {
                cmd_init_lib(
                    name.as_deref(),
                    description.as_deref(),
                    owner.as_deref(),
                    &depends_on,
                    workflow.as_deref().unwrap_or("build"),
                    force,
                )?
            } else {
                cmd_init(ci.as_deref(), containers.as_deref(), force)?
            };

            if !no_register {
                register_quietly(&root, port).await;
            }
        }

        Commands::Register { path, quiet, port } => {
            cmd_register(path.as_deref(), quiet, port).await?;
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

        Commands::Lsp { stdio: _ } => {
            // The protocol owns stdout from here on: anything else printed to
            // it would be read as a malformed message.
            lsp::serve()?;
        }

        Commands::Watch {
            command,
            triggers,
            max_lines,
            port,
            no_open,
            stop,
            attach,
            list,
        } => {
            cmd_watch(
                command, triggers, max_lines, port, no_open, stop, attach, list,
            )
            .await?;
        }

        Commands::Ai {
            subcommand,
            port,
            no_graph,
            mode,
            continue_last,
        } => {
            cmd_ai(subcommand, port, no_graph, mode, continue_last).await?;
        }

        Commands::Daemon { subcommand } => {
            cmd_daemon(subcommand).await?;
        }

        Commands::Config { subcommand } => match subcommand {
            ConfigCommand::Show => {
                let (root, cfg) = load_project(None)?;
                show_config(&cfg, &root);
            }
            ConfigCommand::Reference => {
                print_config_help();
            }
            ConfigCommand::Migrate { dry_run, path } => {
                cmd_config_migrate(path.as_deref(), dry_run)?;
            }
        },

        Commands::Configure { subcommand } => {
            cmd_configure(subcommand)?;
        }

        Commands::Todo {
            task,
            global,
            detach,
            port,
        } => {
            cmd_todo(task, global, detach, port).await?;
        }

        Commands::Why {
            target,
            all,
            json,
            env,
            local,
        } => {
            let cwd = env::current_dir().context("Failed to get current directory")?;
            // The same variables a run would resolve, so the cache verdict this
            // prints is the verdict that run would get.
            let cfg = find_root(&cwd)
                .and_then(|root| load_config(&root).ok())
                .unwrap_or_default();
            let root = find_root(&cwd).unwrap_or_else(|| cwd.clone());
            let vars = build_env_vars(&cfg, &env, local, &root, false)?;
            run::why::run(&cwd, &target, &vars, json, all)?;
        }

        Commands::DryRun {
            targets,
            diff,
            json,
            env,
            local,
            config,
        } => {
            cmd_dry_run(&targets, diff, json, &env, local, config.as_deref()).await?;
        }

        Commands::Cache { subcommand } => {
            cmd_cache(subcommand)?;
        }

        Commands::RemoteCache { subcommand } => {
            cmd_remote_cache(subcommand).await?;
        }

        Commands::Zelf { subcommand } => {
            cmd_self(subcommand).await?;
        }

        Commands::Convert {
            script,
            name,
            dry_run,
            force,
        } => {
            convert::run(&script, name.as_deref(), dry_run, force)?;
        }
    }

    Ok(())
}

/// Dispatch `ciabatta <workflow>` (and `ciabatta workflow <name>`): compile one
/// graph across every sub-workspace that takes part, show it, then run it.
///
/// Showing the graph is not a debugging aid bolted on the side — it *is* the
/// output. A monorepo build that silently reaches into four other packages is
/// the problem; one that prints exactly which four, in what order, and who owns
/// them, is the fix.
///
/// `bare_name` says the workflow was typed as a bare subcommand rather than
/// after `ciabatta workflow`. That's also how a mistyped command arrives here,
/// so an unknown name gets an extra line pointing at `--help` — otherwise
/// `ciabatta pusj` would only ever complain about workflows.
async fn cmd_workflow(args: cli::WorkflowArgs, bare_name: bool) -> Result<()> {
    let cwd = env::current_dir().context("Failed to get current directory")?;

    // No workflow named: show what there is to run rather than erroring out.
    let Some(first) = args.workflow.clone() else {
        let ws = workspace::Workspace::discover(&cwd)?;
        let summary = workspace::render::summary(&ws);
        if summary.is_empty() {
            println!(
                "No workflows are defined in {}.\n\
                 Run `ciabatta init --lib` in a package to add one, or \
                 `ciabatta init --example` to generate a worked example monorepo.",
                ws.root.display()
            );
        } else {
            print!("{summary}");
        }
        return Ok(());
    };

    let mut workflows = vec![first.clone()];
    workflows.extend(args.also.iter().cloned());

    let selection = workspace::graph::Selection {
        only: args.only.clone(),
        isolated: args.isolated,
    };
    let (ws, mut graph) = workspace::graph::prepare_many(&cwd, &workflows, &selection).map_err(
        |err| match bare_name {
            true => anyhow::anyhow!(
                "{err}\n'{first}' is not a ciabatta command either — run \
                 `ciabatta --help` for the list."
            ),
            false => err,
        },
    )?;

    // Narrowing happens after the whole graph is compiled, so a filter is
    // always evaluated against the real dependency structure rather than
    // against whatever subset happened to be loaded.
    let filters = run::filter::parse_all(&args.filter)?;
    let (steps, pruned) = run::filter::apply(&graph.steps, &filters)?;
    graph.steps = steps;

    // The graph goes to stderr when a TUI is about to take the screen, so it
    // survives in the scrollback either way.
    let takes_over_screen = args.use_tui() && !args.gui;
    if !(args.graph && takes_over_screen) {
        let drawing = workspace::render::graph(&ws, &graph);
        if takes_over_screen {
            eprint!("{drawing}");
        } else {
            print!("{drawing}");
        }
        if let Some(report) = pruned.report() {
            if takes_over_screen {
                eprintln!("\n{report}");
            } else {
                println!("\n{report}");
            }
        }
    }

    // Missing toolchains are reported before anything runs, with the install
    // command the repo wrote down — the whole point of `[toolchain]`.
    let missing = workspace::missing_tools(&ws, &graph.steps);
    if !missing.is_empty() {
        let report = workspace::render::missing_tools(&missing);
        if args.graph || args.dry_run {
            eprintln!("\n{report}");
        } else {
            bail!("\n{report}\nInstall them and try again, or preview the graph with --graph.");
        }
    }

    if args.graph {
        // With --tui the graph is worth exploring rather than scrolling: the
        // viewer shows one node at a time in full. Otherwise the plain drawing
        // printed above is the answer.
        if takes_over_screen {
            tui::graph::explore(&ws, &graph, &pruned).await?;
        } else {
            println!(
                "\nNothing was run (--graph). Drop the flag to execute it, or add --dry-run \
                 to walk every step without side effects."
            );
        }
        return Ok(());
    }

    // From here a workflow is an ordinary run: the compiled graph becomes the
    // resolved DAG, and the existing machinery — TUI, live view, recovery
    // prompts — does the rest.
    let cfg = load_config(&ws.root)?;
    // Resolved variables are echoed only when this terminal is going to keep
    // showing text: the TUI would be corrupted by it, and a --gui run reports
    // in the browser instead.
    let announce = !args.use_tui() && !args.gui;
    let mut vars = build_env_vars(&cfg, &args.env, args.local, &ws.root, announce)?;
    source_ciabatta_vars(&mut vars, &ws.root, announce);
    report_env_drift(&ws.root, &graph.env_files, announce);

    let name = graph.label();
    let resolved = workspace::graph::into_run(graph)?;

    if args.gui {
        // The daemon owns the run, so it compiles the graph itself from the
        // same declarations rather than being handed our copy.
        report_run_dependencies(&resolved, &name, &ws.root, &vars, false);
        return cmd_workflow_gui(&args, &workflows, &ws.root, vars).await;
    }

    execute_workflow(
        &name,
        &resolved,
        &cfg,
        &ws.root,
        &vars,
        args.dry_run,
        args.use_tui(),
        args.authoritative,
        &args.sandbox_also,
    )
    .await
}

/// Print the summation of what the named runs depend on, before any of them
/// starts: the targets that will be built, and the environment they'll be
/// built in.
///
/// Both halves exist for the same reason. A run's steps are shell scripts, and
/// the difference between "works here" and "fails there" is far more often a
/// variable, an undeclared input, or a package pulled in from three directories
/// away than it is the graph itself. Ciabatta has already resolved all of it
/// before the first step runs — `REQUIRED_ENV`, the `.env` files it sources, the
/// `[env]` tables that cascade to each step, each target's inputs, outputs and
/// declared variables — so it may as well say so, in the same spirit as printing
/// the graph before executing it.
///
/// Secret-looking names are listed with their values masked: this output goes
/// into CI logs.
fn report_run_dependencies(
    resolved: &run::ResolvedRun,
    name: &str,
    root: &Path,
    vars: &HashMap<String, String>,
    to_stderr: bool,
) {
    let say = |text: String| {
        if to_stderr {
            eprintln!("{text}");
        } else {
            println!("{text}");
        }
    };

    let cfg = load_config(root).unwrap_or_default();
    if let Some(text) = run::deps::report(&cfg, root, name, &resolved.steps, vars) {
        say(text);
    }
    if let Some(text) = run::envdeps::collect(resolved, root, vars).render(name) {
        say(text);
    }
}

/// Tell the operator when the `.env` files a run depends on have moved since
/// the last time it ran here.
///
/// A pulled branch that adds, drops, or changes an environment variable is one
/// of the reliably confusing ways for a build to break: nothing in the diff
/// looks related, and the failure surfaces somewhere else entirely. Ciabatta
/// already knows which files a run sources, so it can simply say so up front.
///
/// Never fatal, and never a prompt — the run continues either way.
fn report_env_drift(root: &Path, env_files: &[String], announce: bool) {
    if env_files.is_empty() {
        return;
    }
    let drift = environment::cache::check(root, env_files);
    if !announce {
        return;
    }
    if let Some(report) = drift.report() {
        eprintln!("{report}\n");
    }
}

/// Hand a workflow run to the daemon and open the live graph in a browser.
async fn cmd_workflow_gui(
    args: &cli::WorkflowArgs,
    workflows: &[String],
    root: &Path,
    vars: HashMap<String, String>,
) -> Result<()> {
    let session = daemon::connect(args.port).await?;

    let response = session
        .daemon
        .client()?
        .post(session.daemon.url("/api/run/runs"))
        .json(&serde_json::json!({
            "project": session.project.id,
            "workflows": workflows,
            "only": args.only,
            "isolated": args.isolated,
            "filter": args.filter,
            "env": vars,
            "dry_run": args.dry_run,
        }))
        .send()
        .await?;

    if !response.status().is_success() {
        let status = response.status();
        let body: serde_json::Value = response.json().await.unwrap_or_default();
        let message = body["error"].as_str().unwrap_or("no reason given");
        bail!("The daemon refused to start the workflow ({status}): {message}");
    }
    let run: serde_json::Value = response.json().await?;
    let id = run["id"]
        .as_u64()
        .context("The daemon returned no run id")?;
    let url = format!("{}/run/{id}", session.daemon.base_url);

    println!(
        "Running workflow '{}' from {}",
        workflows.join(" + "),
        root.display()
    );
    println!("Live view: {url}");
    println!("The daemon owns this run — it keeps going if you close this terminal.");
    daemon::open_browser(&url);
    Ok(())
}

/// Dispatch `ciabatta list`: the monorepo's catalogue of workflows, then this
/// project's own workflows.
fn cmd_list(search: Option<&str>, verbose: bool, recipes_only: bool) -> Result<()> {
    let cwd = env::current_dir().context("Failed to get current directory")?;

    if !recipes_only {
        // A standalone publishing project isn't a monorepo and shouldn't be
        // told it has no workflows — the catalogue only earns its space once
        // there is something in it, or more than one package to tell apart.
        match workspace::Workspace::discover(&cwd) {
            Ok(ws)
                if ws.members.len() > 1 || ws.members.iter().any(|m| !m.workflows.is_empty()) =>
            {
                // Read straight off disk: the shared picture was merged in
                // the last time this checkout ran anything, so `list` never
                // waits on a network call to say when a workflow last ran.
                let history = run::history::History::load(&ws.root);
                // The threshold is the root's to set: "how long is too long" is
                // a property of how the team works, not of any one package.
                let root_config = config::load_config(&ws.root).unwrap_or_default();
                let stale_after = run::history::stale_after(&root_config);
                print!(
                    "{}",
                    workspace::render::catalogue(&ws, search, verbose, &history, stale_after)
                );
                println!();
            }
            _ => {}
        }
    }

    // The workflow list is about the project you're standing in, so it's loaded
    // the same way `push` would load it.
    match load_project(None) {
        Ok((_, cfg)) => list_workflows(&cfg),
        Err(err) if recipes_only => return Err(err),
        Err(_) => {}
    }
    Ok(())
}

/// Dispatch `ciabatta daemon <subcommand>`.
///
/// Only `serve` does real work; the rest are conveniences for inspecting a
/// daemon that commands normally start and manage on your behalf.
async fn cmd_daemon(subcommand: DaemonCommand) -> Result<()> {
    match subcommand {
        DaemonCommand::Serve { port } => {
            let port = daemon::resolve_port(port);
            init_daemon_logging()?;
            daemon::app::serve(port).await
        }

        DaemonCommand::Status => {
            match daemon::find_running().await {
                Some(handle) => {
                    println!("ciabatta daemon is running");
                    println!("  url:  {}", handle.base_url);
                    println!("  pid:  {}", handle.pid);
                    println!("  log:  {}", daemon::log_path()?.display());
                }
                None => {
                    println!("ciabatta daemon is not running.");
                    println!("Any command with a web view will start it automatically.");
                }
            }
            Ok(())
        }

        DaemonCommand::Stop => {
            let Some(record) = daemon::read_record() else {
                println!("ciabatta daemon is not running.");
                return Ok(());
            };
            if daemon::find_running().await.is_none() {
                daemon::clear_record()?;
                println!("ciabatta daemon is not running (cleared a stale record).");
                return Ok(());
            }
            daemon::shutdown(&record).await?;
            println!("Stopped the ciabatta daemon (pid {}).", record.pid);
            Ok(())
        }

        DaemonCommand::Restart { port } => {
            if let Some(record) = daemon::read_record()
                && daemon::find_running().await.is_some()
            {
                daemon::shutdown(&record).await?;
            }
            let handle = daemon::ensure_running(port).await?;
            println!(
                "ciabatta daemon restarted at {} (pid {}).",
                handle.base_url, handle.pid
            );
            Ok(())
        }

        DaemonCommand::Logs { lines, follow } => cmd_daemon_logs(lines, follow).await,
    }
}

/// Print (and optionally follow) the daemon log.
async fn cmd_daemon_logs(lines: usize, follow: bool) -> Result<()> {
    use tokio::io::{AsyncBufReadExt, AsyncSeekExt, BufReader};

    let path = daemon::log_path()?;
    if !path.exists() {
        println!("No daemon log yet at {}.", path.display());
        return Ok(());
    }

    let contents = std::fs::read_to_string(&path)
        .with_context(|| format!("Failed to read {}", path.display()))?;
    let all: Vec<&str> = contents.lines().collect();
    for line in all.iter().skip(all.len().saturating_sub(lines)) {
        println!("{line}");
    }

    if !follow {
        return Ok(());
    }

    // Follow from the end of what we just printed, polling for appends.
    let mut offset = contents.len() as u64;
    println!("--- following {} (Ctrl-C to stop) ---", path.display());
    loop {
        tokio::time::sleep(std::time::Duration::from_millis(400)).await;
        let mut file = match tokio::fs::File::open(&path).await {
            Ok(f) => f,
            Err(_) => continue,
        };
        let len = file.metadata().await.map(|m| m.len()).unwrap_or(offset);
        if len < offset {
            // The log was truncated or rotated; start over from the top.
            offset = 0;
        }
        if len == offset {
            continue;
        }
        file.seek(std::io::SeekFrom::Start(offset)).await?;
        let mut reader = BufReader::new(file);
        let mut line = String::new();
        while reader.read_line(&mut line).await? > 0 {
            print!("{line}");
            offset += line.len() as u64;
            line.clear();
        }
    }
}

/// Send the daemon's `tracing` output to `~/.ciabatta/daemon.log`.
///
/// The daemon is normally spawned detached with its stdio pointed at /dev/null,
/// so anything written to stderr would be lost. The individual servers this
/// replaced just used `println!` and accepted that.
///
/// Which is also why this installs a panic hook. The default one writes to
/// stderr, so a daemon that panics — in a request handler, in a run's engine
/// task, anywhere — leaves *no* trace at all: the process either dies or the
/// work silently stops, and the log ends mid-sentence. Routing panics through
/// `tracing` is the difference between "the daemon crashed" and a file, a line,
/// and a stack.
fn init_daemon_logging() -> Result<()> {
    use std::io::IsTerminal;
    use tracing_subscriber::EnvFilter;
    use tracing_subscriber::fmt::writer::MakeWriterExt;

    let path = daemon::log_path()?;
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .with_context(|| format!("Failed to open {}", path.display()))?;

    let directives = std::env::var("CIABATTA_LOG")
        .unwrap_or_else(|_| "ciabatta=info,tower_http=warn".to_string());

    // Run in the foreground and the terminal is where you are looking, so log
    // there as well. In the background stderr *is* this file (see
    // `spawn_detached`), and teeing would write every line twice.
    let tee = std::io::stderr().is_terminal();

    // `try_init` rather than `init`: losing a race with another subscriber
    // shouldn't be fatal. `main` deliberately doesn't install one for this
    // command — see the note there.
    //
    // Which task a line came from matters here in a way it doesn't on the CLI:
    // a run, its progress fold, and the HTTP handlers all interleave.
    macro_rules! subscriber {
        ($writer:expr) => {
            tracing_subscriber::fmt()
                .with_env_filter(EnvFilter::new(&directives))
                .with_thread_names(true)
                .with_target(true)
                .with_writer($writer)
                .with_ansi(false)
                .try_init()
        };
    }
    let _ = if tee {
        subscriber!(file.and(std::io::stderr))
    } else {
        subscriber!(file)
    };

    install_panic_logger();

    tracing::info!(
        log = %path.display(),
        filter = %directives,
        "daemon logging started (raise it with CIABATTA_LOG=ciabatta=debug)"
    );

    Ok(())
}

/// Log panics instead of losing them to a discarded stderr.
///
/// The backtrace is captured with `force_capture`, so it is there without the
/// operator having thought to set `RUST_BACKTRACE` before the crash they didn't
/// know was coming — which is the only time it would have helped.
fn install_panic_logger() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let location = info
            .location()
            .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
            .unwrap_or_else(|| "unknown location".to_string());

        // `payload_as_str` isn't stable, so unwrap the two shapes `panic!`
        // actually produces.
        let message = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "<non-string panic payload>".to_string());

        let thread = std::thread::current();
        let thread = thread.name().unwrap_or("unnamed").to_string();

        tracing::error!(
            target: "ciabatta::panic",
            %location,
            %thread,
            "panic: {message}\n{}",
            std::backtrace::Backtrace::force_capture()
        );

        // Still run whatever was there before — in the foreground
        // (`ciabatta daemon serve`) that is the default hook, and its stderr
        // message is what the operator watching the terminal expects to see.
        previous(info);
    }));
}

/// Dispatch `ciabatta todo`:
///   - a TASK string adds the task to the current project and exits
///   - otherwise open the todo page in the daemon's web app
///
/// Adding from the command line scopes the task to whichever project the
/// working directory is in, so `ciabatta todo "fix the flaky test"` typed in a
/// repo lands on that repo's list rather than on one shared pile. Typed outside
/// any project it's unscoped, and shows up everywhere.
///
/// The list lives in `~/.ciabatta/todos.json`, so adding doesn't need the
/// daemon — but the project *id* comes from the daemon's registry, which is
/// what the web app's switcher selects on. Registering is a local computation
/// (a hash of the path), so this works with the daemon down.
async fn cmd_todo(
    task: Option<String>,
    global: bool,
    detach: bool,
    port: Option<u16>,
) -> Result<()> {
    if let Some(text) = task {
        // `--global` files it on the global list; otherwise it belongs to
        // whichever project the working directory is in — and outside any
        // project there is nothing else it could be.
        let project = if global { None } else { current_project_id() };
        let added = add_todo(&text, project.as_deref()).await?;

        match (&project, global) {
            (Some(_), _) => println!("Added task #{}: {}", added.id, added.text),
            (None, true) => println!(
                "Added task #{} to the global list: {}",
                added.id, added.text
            ),
            (None, false) => println!(
                "Added task #{} to the global list: {}\n\
                 (this directory isn't inside a ciabatta project, so there's no \
                 project list to add it to)",
                added.id, added.text
            ),
        }
        return Ok(());
    }

    if detach {
        eprintln!(
            "note: -d/--detach no longer does anything — the ciabatta daemon is \
             already a background process, and it keeps serving the todo app \
             after this command exits."
        );
    }

    let session = daemon::connect(port).await?;
    let url = format!("{}/todo", session.daemon.base_url);
    println!("Todo app: {url}");
    daemon::open_browser(&url);
    Ok(())
}

/// Dispatch `ciabatta ai`:
///   - no subcommand: the chat TUI plus the live mind-map server
///   - `setup`: interactively write the [ai] config section
///   - `ask PROMPT…`: one-shot question, plain output
///   - `serve`: just the daemon (mind map + JSON API)
///
/// Like `configure`, this works before the project is initialized: it prefers
/// an existing .ciabatta root and otherwise targets the current directory
/// (setup/first use creates .ciabatta/ai/ as needed).
async fn cmd_ai(
    subcommand: Option<AiCommand>,
    port: u16,
    no_graph: bool,
    mode: String,
    continue_last: bool,
) -> Result<()> {
    let cwd = env::current_dir().context("Failed to get current directory")?;
    let root = find_root(&cwd).unwrap_or(cwd);

    if let Some(AiCommand::Setup) = subcommand {
        return ai::run_setup(&root);
    }

    let mode = ai::Mode::parse(&mode)?;
    let cfg = load_config(&root)?;
    if cfg.ai.is_none() {
        eprintln!("note: no [ai] section in ciabatta.toml — run `ciabatta ai setup` to configure");
        eprintln!(
            "      a provider (Claude, an OpenAI-compatible endpoint, or vLLM). Trying defaults…\n"
        );
    }

    // `--continue` resumes the latest conversation for the TUI and one-shot ask.
    let resume = if continue_last {
        ai::Resume::Latest
    } else {
        ai::Resume::None
    };

    match subcommand {
        None => ai::run_tui(&root, &cfg, port, no_graph, mode, resume).await,
        Some(AiCommand::Serve) => ai::run_serve(&root, &cfg, port, mode).await,
        Some(AiCommand::Resume { id }) => {
            ai::run_resume(&root, &cfg, port, no_graph, mode, id).await
        }
        Some(AiCommand::Report { days, pdf }) => ai::run_report(&root, &cfg, days, mode, pdf).await,
        Some(AiCommand::Tag { name, description }) => {
            ai::run_tag(&root, &cfg, &name, &description.join(" "), mode).await
        }
        Some(AiCommand::Delete { id }) => ai::run_delete(&root, &id),
        Some(AiCommand::Clear { yes }) => ai::run_clear(&root, yes),
        Some(AiCommand::Ship { task, todo }) => {
            // The task text is either given inline or pulled from a todo.
            let prompt = if let Some(id) = todo {
                let store = todo::Store::open()?;
                let text = store
                    .text_of(id)
                    .with_context(|| format!("no todo #{id} — see `ciabatta todo`"))?;
                if !task.is_empty() {
                    // Allow appending extra guidance after --todo.
                    format!("{text}\n\n{}", task.join(" "))
                } else {
                    text
                }
            } else {
                let p = task.join(" ");
                if p.trim().is_empty() {
                    bail!("Nothing to ship. Usage: ciabatta ai ship <task>  (or --todo <id>)");
                }
                p
            };
            ai::run_ship(&root, &cfg, &prompt, todo).await
        }
        Some(AiCommand::Jobs) => ai::run_jobs(&root, &cfg),
        Some(AiCommand::Ask { prompt }) => {
            ai::run_ask(&root, &cfg, &prompt.join(" "), mode, resume).await
        }
        Some(AiCommand::BurnIn { review, limit }) => {
            ai::run_burn_in(&root, &cfg, port, review, limit).await
        }
        Some(AiCommand::Setup) => unreachable!("handled above"),
    }
}

/// Dispatch `ciabatta watch <command>`: hand the command to the daemon, open
/// the live view, and tail the output in this terminal.
///
/// The daemon runs and owns the process, so **Ctrl-C detaches rather than
/// kills** — the session keeps running and stays open in the browser. Use
/// `ciabatta watch --stop <ID>` (or the Stop button) to actually end it.
#[allow(clippy::too_many_arguments)]
async fn cmd_watch(
    command: Vec<String>,
    triggers: Vec<String>,
    max_lines: usize,
    port: Option<u16>,
    no_open: bool,
    stop: Option<u64>,
    attach: Option<u64>,
    list: bool,
) -> Result<()> {
    let session = daemon::connect(port).await?;
    let client = session.daemon.client()?;

    if let Some(id) = stop {
        let response = client
            .post(
                session
                    .daemon
                    .url(&format!("/api/watch/sessions/{id}/stop")),
            )
            .send()
            .await?;
        if response.status().is_success() {
            println!("Stopped watch session {id}.");
        } else {
            bail!("Failed to stop session {id}: {}", response.text().await?);
        }
        return Ok(());
    }

    if list {
        return list_watch_sessions(&client, &session).await;
    }

    // Attaching is how you follow a `persistent` workflow step: the run left it
    // behind as a session and printed its id.
    if let Some(id) = attach {
        let url = format!("{}/watch/{id}", session.daemon.base_url);
        println!("Attaching to watch session {id}.");
        println!("Live view: {url}");
        println!(
            "Ctrl-C detaches — the session keeps running. Stop it with `ciabatta watch --stop {id}`."
        );
        println!();
        if !no_open {
            daemon::open_browser(&url);
        }
        return tail_watch_session(&client, &session, id).await;
    }

    let command = command.join(" ");
    if command.trim().is_empty() {
        bail!("No command given. Usage: ciabatta watch <command>");
    }

    // A watched command must see the same environment a run would: the
    // workspace's `.env` (or whatever `env_file` names instead), plus the
    // CIABATTA_* variables. Watching `npm run dev` and having it behave
    // differently from `ciabatta run dev` because one sourced `.env` and the
    // other didn't is the kind of difference that costs an afternoon.
    let watch_env = resolve_watch_env()?;
    print_watch_env(&watch_env);

    let created: serde_json::Value = client
        .post(session.daemon.url("/api/watch/sessions"))
        .json(&serde_json::json!({
            "project": session.project.id,
            "command": command,
            "triggers": triggers,
            "max_lines": max_lines,
            "env": watch_env,
        }))
        .send()
        .await?
        .error_for_status()
        .context("The daemon refused to start the watch session")?
        .json()
        .await?;

    let id = created["id"]
        .as_u64()
        .context("The daemon returned no session id")?;
    let url = format!("{}/watch/{id}", session.daemon.base_url);

    println!("Watching: {command}");
    println!("Live view: {url}");
    println!(
        "Ctrl-C detaches — the session keeps running. Stop it with `ciabatta watch --stop {id}`."
    );
    println!();

    if !no_open {
        daemon::open_browser(&url);
    }

    tail_watch_session(&client, &session, id).await
}

/// Add a task, through the daemon when one is already running.
///
/// The daemon keeps the list in memory and rewrites the file on every change,
/// so a CLI process that wrote the file directly would have its task
/// disappear the next time the daemon saved. Going through the API when
/// there's a daemon to go through keeps one writer.
///
/// With no daemon running, this writes the file itself — and deliberately does
/// *not* start one. `ciabatta todo "…"` is a one-line note; spawning a
/// background web server to take it would be a strange thing to do.
async fn add_todo(text: &str, project: Option<&str>) -> Result<todo::Todo> {
    if let Some(daemon) = daemon::find_running().await
        && let Ok(client) = daemon.client()
    {
        let response = client
            .post(daemon.url("/api/todos"))
            .json(&serde_json::json!({ "text": text, "project": project }))
            .send()
            .await;

        // A daemon that's up but unhappy shouldn't lose the task: fall through
        // and write it locally rather than reporting a failure.
        match response {
            Ok(response) if response.status().is_success() => {
                let todos: Vec<todo::Todo> = response.json().await?;
                if let Some(added) = todos.iter().max_by_key(|t| t.id) {
                    return Ok(added.clone());
                }
            }
            Ok(response) => {
                tracing::debug!(
                    "the daemon refused the todo ({}); writing it locally",
                    response.status()
                );
            }
            Err(e) => tracing::debug!("couldn't reach the daemon ({e}); writing the todo locally"),
        }
    }

    todo::Store::open()?.add(text, project)
}

/// The project id for the current directory, if it's inside one.
///
/// Computed rather than looked up, so this doesn't touch the registry file the
/// running daemon holds in memory — writing it here would leave the daemon's
/// copy stale and the two disagreeing about which projects exist. Registration
/// stays where it belongs: the `POST /api/projects` every command that opens
/// the web app already makes.
fn current_project_id() -> Option<String> {
    let cwd = env::current_dir().ok()?;
    let root = find_root(&cwd)?;
    let canonical = root.canonicalize().unwrap_or(root);
    Some(daemon::projects::project_id(&canonical))
}

/// The environment a watched command runs with.
///
/// The same resolution a run does — CIABATTA_* variables from git or CI, then
/// the workspace's env files layered underneath — so `ciabatta watch "npm run
/// dev"` and a `dev` workflow step see the same thing.
///
/// Never fatal: a watch is somebody looking at output, and refusing to start
/// one because a project's config didn't load would be the wrong trade. A
/// problem is reported and the command runs with the ambient environment.
fn resolve_watch_env() -> Result<BTreeMap<String, String>> {
    let cwd = env::current_dir().context("Failed to get current directory")?;
    let Some(root) = find_root(&cwd) else {
        return Ok(BTreeMap::new());
    };

    let config = match load_config(&root) {
        Ok(config) => config,
        Err(e) => {
            eprintln!(
                "note: couldn't read this project's config ({e:#}); watching with the ambient environment"
            );
            return Ok(BTreeMap::new());
        }
    };

    // Build variables first, so an explicit CIABATTA_* value wins over a file.
    let mut vars = build_env_vars(&config, &[], false, &root, false).unwrap_or_default();

    let meta = config.workspace.clone().unwrap_or_default();
    let resolved = environment::files::resolve(&meta, &root);

    // Which keys the env files define. Kept separately because `vars` is seeded
    // from the whole ambient environment, and handing back all of it would make
    // both the printed list and the daemon payload mostly noise.
    let mut from_files: Vec<String> = Vec::new();
    if !resolved.files.is_empty() {
        for file in &resolved.files {
            if let Ok(content) = std::fs::read_to_string(root.join(file)) {
                from_files.extend(run::parse_env_content(&content).into_iter().map(|(k, _)| k));
            }
        }
        match run::load_env_files(&resolved.files, &root, &vars) {
            Ok(merged) => vars = merged,
            Err(e) => eprintln!("note: couldn't source this workspace's env files ({e:#})"),
        }
    }

    // Only what ciabatta contributes: the CIABATTA_* variables it resolved, and
    // whatever this workspace's env files define. Everything else the command
    // sees is inherited from the shell, and re-listing the shell's environment
    // back at the user would bury the two lines that matter.
    Ok(vars
        .into_iter()
        .filter(|(key, _)| key.starts_with("CIABATTA_") || from_files.contains(key))
        .collect())
}

/// Show what the watched command will see.
///
/// Values are printed, not hidden: this is the user's own terminal, showing
/// their own project's environment, and a redacted list would defeat the point
/// — which is being able to see at a glance that `API_URL` is pointing at the
/// wrong thing. What is *not* printed is the ambient environment the process
/// already had; only what ciabatta resolved and is adding.
fn print_watch_env(vars: &BTreeMap<String, String>) {
    if vars.is_empty() {
        println!("Environment: nothing resolved — running with your shell's environment.");
        println!();
        return;
    }

    println!("Environment ({} variable(s)):", vars.len());
    let width = vars.keys().map(|k| k.len()).max().unwrap_or(0);
    for (key, value) in vars {
        println!("  {key:<width$}  {value}");
    }
    println!();
}

/// Print the watch sessions the daemon owns, newest first.
///
/// The list matters more now that a `persistent` workflow step leaves one
/// behind: after a build, this is how you find the dev server it started and
/// the id to attach to.
async fn list_watch_sessions(client: &reqwest::Client, session: &daemon::Session) -> Result<()> {
    let sessions: Vec<serde_json::Value> = client
        .get(session.daemon.url("/api/watch/sessions"))
        .send()
        .await?
        .error_for_status()
        .context("The daemon refused to list watch sessions")?
        .json()
        .await?;

    if sessions.is_empty() {
        println!("No watch sessions. Start one with `ciabatta watch <command>`.");
        return Ok(());
    }

    println!("Watch sessions ({}):", sessions.len());
    for entry in &sessions {
        let id = entry["id"].as_u64().unwrap_or_default();
        let running = entry["running"].as_bool().unwrap_or(false);
        // A session left behind by a persistent step is named after its graph
        // node, which identifies it far better than its command line does.
        let what = entry["label"]
            .as_str()
            .map(|label| format!("{label}  ({})", entry["command"].as_str().unwrap_or("")))
            .unwrap_or_else(|| entry["command"].as_str().unwrap_or("").to_string());
        println!(
            "  #{id:<4} {:<9} {:>9} lines   {what}",
            if running { "running" } else { "finished" },
            entry["lines"].as_u64().unwrap_or_default(),
        );
    }
    println!();
    println!("Follow one with `ciabatta watch --attach <ID>`, stop it with `--stop <ID>`.");
    Ok(())
}

/// Follow a session's output in the terminal until Ctrl-C.
///
/// Polls the snapshot endpoint rather than consuming the SSE stream: this is a
/// short, simple consumer and adding an SSE parser to the CLI for it would
/// earn nothing. The browser gets the push stream.
async fn tail_watch_session(
    client: &reqwest::Client,
    session: &daemon::Session,
    id: u64,
) -> Result<()> {
    let mut after = 0u64;

    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                println!();
                println!("Detached. Session {id} is still running at {}/watch/{id}", session.daemon.base_url);
                return Ok(());
            }
            _ = tokio::time::sleep(std::time::Duration::from_millis(300)) => {}
        }

        let url = session
            .daemon
            .url(&format!("/api/watch/sessions/{id}?after={after}"));
        let Ok(response) = client.get(url).send().await else {
            continue;
        };
        let Ok(snapshot) = response.json::<serde_json::Value>().await else {
            continue;
        };

        if let Some(lines) = snapshot["lines"].as_array() {
            for line in lines {
                if let Some(seq) = line["seq"].as_u64() {
                    after = after.max(seq);
                }
                let text = line["text"].as_str().unwrap_or_default();
                if line["stream"] == "stderr" {
                    eprintln!("{text}");
                } else {
                    println!("{text}");
                }
            }
        }

        // The terminal notification channel the watch store used to own:
        // a bell plus the matching line, now driven from here so it reaches
        // the user's terminal rather than the daemon's log file.
        if let Some(hits) = snapshot["hits"].as_array() {
            for hit in hits {
                print!("\x07");
                println!("⚑ trigger → {}", hit["text"].as_str().unwrap_or_default());
            }
        }

        if snapshot["session"]["running"] == serde_json::Value::Bool(false) {
            println!();
            println!(
                "Command finished. Output stays available at {}/watch/{id}",
                session.daemon.base_url
            );
            return Ok(());
        }
    }
}

/// Initialize the `tracing` subscriber for stderr logging.
///
/// Debug logging turns on when the `--debug` flag is passed OR the
/// `CIABATTA_DEBUG` environment variable is set to any non-empty value other
/// than `0`/`false`. For finer-grained control the `CIABATTA_LOG` environment
/// variable is honored directly as a `tracing` env-filter (e.g.
/// `CIABATTA_LOG=ciabatta=trace`), overriding the flag-derived default.
///
/// `verbose` raises the floor from `warn` to `info` for commands that exist to
/// keep running and report — today that is `remote-cache start`, whose request
/// log is the reason anyone runs it in a terminal.
fn init_logging(debug_flag: bool, verbose: bool) {
    use tracing_subscriber::{EnvFilter, fmt};

    let debug = debug_flag
        || env::var("CIABATTA_DEBUG")
            .map(|v| {
                let v = v.trim();
                !v.is_empty() && v != "0" && !v.eq_ignore_ascii_case("false")
            })
            .unwrap_or(false);

    let default_directive = match (debug, verbose) {
        (true, _) => "ciabatta=debug",
        (false, true) => "ciabatta=info",
        (false, false) => "ciabatta=warn",
    };
    let filter = EnvFilter::try_from_env("CIABATTA_LOG")
        .unwrap_or_else(|_| EnvFilter::new(default_directive));

    // Best-effort: ignore the error if a subscriber is somehow already set.
    let _ = fmt()
        .with_env_filter(filter)
        .with_target(false)
        .with_writer(std::io::stderr)
        .try_init();
}

/// Turn a rejected remote-cache request into an answer somebody can act on.
///
/// A credential the server says is expired or unknown is dead for good — no
/// retry brings it back. Two things follow, and neither happened before:
///
/// * **Drop it.** Keeping a credential you've been told is finished means every
///   later command fails the same way, and the file quietly accumulates logins
///   that can never work again.
/// * **Say if you're logged in to the same server under another name.** One
///   machine answers to `localhost` and to `127.0.0.1`, credentials are keyed by
///   the URL that was typed, and a workspace config naming one while the login
///   used the other produces a server that has genuinely never heard of you.
///   That is a confusing thing to work out from first principles and a cheap
///   thing to be told.
///
/// Anything that isn't an authentication failure is passed straight through.
fn explain_auth_failure(url: &str, error: anyhow::Error) -> anyhow::Error {
    use remote_cache::client::{Credentials, ServerError};

    let Some(server_error) = error.downcast_ref::<ServerError>() else {
        return error;
    };
    if !server_error.is_auth_failure() {
        return error;
    }

    let mut credentials = Credentials::load();
    let siblings = credentials.same_server_as(url);
    // Only a credential the server has actively disowned gets thrown away.
    // "No credential" means there was nothing there to begin with.
    if server_error.session_is_dead() && credentials.remove(url) {
        // Best-effort: failing to write the file must not replace the server's
        // explanation with one about the filesystem.
        let _ = credentials.save();
    }

    let mut message = format!("{server_error}");
    if !siblings.is_empty() {
        message.push_str(&format!(
            "\n\nYou are logged in to {} — the same machine under a different name. \
             This command used {url}, which is what your workspace config \
             (cache.remote.url) names.\n\
             Either log in to it:  ciabatta remote-cache login {url}\n\
             or point the workspace at the one you already have.",
            siblings.join(", "),
        ));
    }
    anyhow::anyhow!(message)
}

/// Dispatch `ciabatta configure` (interactive registry setup) and its `auto`
/// subcommand (analyze the project and suggest workflows).
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
        // (used to resolve relative workflow paths) from its location.
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
/// directory, so relative workflow paths resolve alongside the config.
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
        vars.insert(
            environment::ENV_VAR.to_string(),
            environment::LOCAL.to_string(),
        );
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
///   - a tag (CLI/env `CIABATTA_TAG`) wins → `/{CIABATTA_TAG}`
///   - otherwise → `/{CIABATTA_BRANCH}/{CIABATTA_COMMIT}`
///
/// Returns `None` when there isn't enough information to build it (no tag and no
/// branch), so callers leave `CIABATTA_PATH` unset rather than emitting `//`.
fn derive_ciabatta_path(vars: &HashMap<String, String>) -> Option<String> {
    let non_empty = |key: &str| vars.get(key).filter(|v| !v.is_empty()).cloned();

    if let Some(tag) = non_empty("CIABATTA_TAG") {
        return Some(format!("/{tag}"));
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
/// CI, the ambient environment, or `-e` win). Used by `ciabatta run` so a
/// run's scripts always see the resolved build variables — the same set
/// `ciabatta source` prints — without the operator having to `eval` them first.
///
/// A non-git directory (or any git error) is not fatal: the run simply proceeds
/// without git-derived variables, exactly as it would today.
fn source_ciabatta_vars(vars: &mut HashMap<String, String>, root: &Path, announce: bool) {
    let git_vars = match git::local_git_vars(root) {
        Ok(v) => v,
        Err(e) => {
            tracing::debug!(error = %e, "run: could not source CIABATTA_* from local git");
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
    if vars
        .get("CIABATTA_PATH")
        .map(|v| v.is_empty())
        .unwrap_or(true)
        && let Some(path) = derive_ciabatta_path(vars)
    {
        vars.insert("CIABATTA_PATH".to_string(), path);
        added.push("CIABATTA_PATH".to_string());
    }
    if announce && !added.is_empty() {
        added.sort();
        eprintln!("Sourced CIABATTA variables from local git for the run:");
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

/// Run one compiled workflow, in the terminal or the TUI.
#[allow(clippy::too_many_arguments)]
async fn execute_workflow(
    name: &str,
    resolved: &run::ResolvedRun,
    cfg: &CiabattaConfig,
    root: &Path,
    vars: &HashMap<String, String>,
    dry_run: bool,
    use_tui: bool,
    authoritative: bool,
    sandbox_also: &[String],
) -> Result<()> {
    // What the run depends on, environment-wise, before a step touches it. It
    // goes to stderr when the TUI is about to take the screen, so it survives
    // in the scrollback the same way the graph drawing does.
    report_run_dependencies(resolved, name, root, vars, use_tui);

    // Resolve the container runtime once up front so every transfer step shares
    // it. A workflow that never touches a container registry doesn't need one,
    // so a missing runtime is noted rather than fatal.
    let mut cfg = cfg.clone();
    match config::resolve_container_cmd(&cfg) {
        Ok(container_cmd) => {
            cfg.system.get_or_insert_with(Default::default).containers = Some(container_cmd);
        }
        Err(e) => tracing::debug!("no container runtime resolved: {e}"),
    }
    let cfg = &cfg;

    // What a step's command is allowed to emit depends on who ends up reading
    // it: the plain runner prints its lines through to this terminal, the TUI
    // styles and wraps them itself.
    color::decide(if use_tui {
        color::Consumer::Tui
    } else {
        color::Consumer::Terminal
    });

    if !use_tui {
        run_plain(
            name,
            resolved,
            cfg,
            root,
            vars,
            dry_run,
            authoritative,
            sandbox_also,
        )
        .await
    } else {
        let success = tui::run(
            name,
            resolved,
            cfg,
            root,
            vars,
            dry_run,
            authoritative,
            sandbox_also,
        )
        .await?;
        if !success {
            bail!("The workflow failed.");
        }
        Ok(())
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_plain(
    name: &str,
    resolved: &run::ResolvedRun,
    cfg: &CiabattaConfig,
    root: &Path,
    vars: &HashMap<String, String>,
    dry_run: bool,
    authoritative: bool,
    sandbox_also: &[String],
) -> Result<()> {
    use runner::ProgressUpdate;
    use tokio::sync::mpsc;

    let (tx, mut rx) = mpsc::channel::<ProgressUpdate>(256);

    let name_clone = name.to_string();
    let resolved_clone = resolved.clone();
    let cfg_clone = cfg.clone();
    let root_clone = root.to_path_buf();
    let vars_clone = vars.clone();
    let sandbox_also = sandbox_also.to_vec();

    tokio::spawn(async move {
        let _ = runner::run_workflow_ctl(
            &name_clone,
            &resolved_clone,
            &cfg_clone,
            &root_clone,
            &vars_clone,
            dry_run,
            runner::RunCtl {
                authoritative,
                sandbox_also,
                ..Default::default()
            },
            tx,
        )
        .await;
    });

    // Which workflow a line belongs to is structure, not content: dimmed, so what
    // the eye lands on is the command output it prefixes. A run that isn't in
    // colour gets the same text with no escapes at all — see [`color`].
    let tag = |name: &str| format!("[{name}]").style(color::faint()).to_string();

    let mut any_failed = false;
    while let Some(update) = rx.recv().await {
        match update {
            ProgressUpdate::Started(name) => println!("{} started", tag(&name)),
            ProgressUpdate::StageStarted { workflow, stage } => {
                println!(
                    "{} {} {}",
                    tag(&workflow),
                    "▶".style(color::active()),
                    stage.label()
                )
            }
            ProgressUpdate::StageFinished {
                workflow,
                stage,
                ran,
            } => {
                if !ran {
                    println!(
                        "{}   {}",
                        tag(&workflow),
                        format!("{} (default, nothing to do)", stage.label()).style(color::faint())
                    );
                }
            }
            ProgressUpdate::TransferProgress {
                workflow,
                done,
                total,
            } => {
                let pct = (done * 100).checked_div(total).unwrap_or(0);
                println!("{}   {done}/{total} files ({pct}%)", tag(&workflow));
            }
            ProgressUpdate::Log(name, line) => println!("{} {line}", tag(&name)),
            ProgressUpdate::StepStarted { workflow, step } => {
                println!(
                    "{} {} step: {step}",
                    tag(&workflow),
                    "▶".style(color::active())
                )
            }
            ProgressUpdate::StepFinished { workflow, step, ok } => {
                let mark = if ok {
                    "✓".style(color::good())
                } else {
                    "✗".style(color::bad())
                };
                println!("{}   {mark} step: {step}", tag(&workflow))
            }
            ProgressUpdate::StepSkipped {
                workflow,
                step,
                reason,
            } => {
                println!(
                    "{}   {} skipped step: {step} ({reason})",
                    tag(&workflow),
                    "⊘".style(color::warn())
                )
            }
            // The command's own output, escapes and all — it was asked for
            // colour precisely because these lines end up here unaltered.
            ProgressUpdate::StepLog {
                workflow,
                step,
                line,
            } => {
                println!(
                    "{}   {} {line}",
                    tag(&workflow),
                    format!("[{step}]").style(color::faint())
                )
            }
            ProgressUpdate::StepNeedsChoice {
                workflow,
                step,
                message,
                options,
            } => {
                println!(
                    "{} {} {step}: {message}",
                    tag(&workflow),
                    "⚠".style(color::warn())
                );
                for (i, opt) in options.iter().enumerate() {
                    println!("{}     [{i}] {opt}", tag(&workflow));
                }
            }
            ProgressUpdate::Completed(name) => {
                println!("{} {} completed", tag(&name), "✓".style(color::good()))
            }
            ProgressUpdate::Failed(name, err) => {
                eprintln!("{} {} failed: {err}", tag(&name), "✗".style(color::bad()));
                any_failed = true;
            }
        }
    }

    if any_failed {
        bail!("One or more workflows failed.");
    }
    Ok(())
}

// ─── The remote cache ───────────────────────────────────────────────────────

/// Dispatch `ciabatta remote-cache …`.
async fn cmd_remote_cache(subcommand: RemoteCacheCommand) -> Result<()> {
    use remote_cache::client::{Client, Credential, Credentials};

    match subcommand {
        RemoteCacheCommand::Init {
            into,
            port,
            storage,
            force,
        } => {
            let dir =
                into.unwrap_or(env::current_dir().context("Failed to get current directory")?);
            std::fs::create_dir_all(&dir)
                .with_context(|| format!("Failed to create {}", dir.display()))?;

            if let Some(existing) = remote_cache::ServerConfig::find(&dir)
                && !force
            {
                bail!(
                    "{} already exists. Use --force to overwrite it.",
                    existing.display()
                );
            }

            let port = port.unwrap_or(remote_cache::DEFAULT_PORT);
            let path = dir.join(format!(
                "{}.{}",
                remote_cache::CONFIG_STEM,
                format::YAML_EXT
            ));
            std::fs::write(&path, remote_cache::starter_config(port, &storage))
                .with_context(|| format!("Failed to write {}", path.display()))?;

            println!("Wrote {}", path.display());
            println!();
            println!("Next:");
            println!("  1. Read it — `auth.mode` is `open`, which means anyone who can reach");
            println!("     the port can read and overwrite cached artifacts. Fine on a trusted");
            println!("     network; set `token` or `ldap` before exposing it further.");
            println!("  2. ciabatta remote-cache start");
            println!("  3. On each developer's machine:");
            println!("       ciabatta remote-cache login http://<this-host>:{port}");
            println!("       ciabatta cache init --remote http://<this-host>:{port}");
            Ok(())
        }

        RemoteCacheCommand::Start { config, port } => {
            let cwd = env::current_dir().context("Failed to get current directory")?;
            let path = match config {
                Some(path) => path,
                None => remote_cache::ServerConfig::find(&cwd).ok_or_else(|| {
                    anyhow::anyhow!(
                        "No remote-cache config in {}. Create one with \
                         `ciabatta remote-cache init`.",
                        cwd.display()
                    )
                })?,
            };

            let (mut server_config, _) = remote_cache::ServerConfig::load(&path)?;
            if let Some(port) = port {
                server_config.server.port = port;
            }
            remote_cache::server::serve(server_config).await
        }

        RemoteCacheCommand::Login {
            url,
            username,
            password_env,
            no_tls_verify,
        } => {
            let tls_verify = !no_tls_verify;
            if !tls_verify {
                eprintln!(
                    "warning: certificate verification is off for {url}. The connection is \
                     encrypted but the server's identity is not checked, so the build \
                     artifacts it serves are only as trustworthy as the network."
                );
            }

            let client = Client::with_token(&url, tls_verify, None)?;

            // Ask the server what it wants before prompting for anything, so an
            // open cache doesn't demand credentials it will ignore.
            let health = client.health().await?;
            let needs_credentials = health.auth != "open" && health.auth != "none";

            let (username, password) = if needs_credentials {
                let username = match username {
                    Some(name) => name,
                    None => prompt_line(&format!("Username for {url}: "))?,
                };
                let password = match &password_env {
                    Some(var) => env::var(var)
                        .with_context(|| format!("--password-env names {var}, but it isn't set"))?,
                    None => prompt_secret(&format!(
                        "{} for {url}: ",
                        if health.auth == "token" {
                            "Token"
                        } else {
                            "Password"
                        }
                    ))?,
                };
                (username, password)
            } else {
                println!("{url} accepts anyone who can reach it (auth.mode: open).");
                (username.unwrap_or_default(), String::new())
            };

            let session = client.login(&username, &password).await?;

            let mut credentials = Credentials::load();
            credentials.set(
                &url,
                Credential {
                    token: session.token,
                    user: session.user.name.clone(),
                    expires_at: session.expires_at.clone(),
                    release: health.release.clone(),
                    tls_verify,
                },
            );
            credentials.save()?;

            anyhow::ensure!(
                health.ok,
                "{url} answered, but reported itself unhealthy — check its log before \
                 pointing builds at it."
            );

            println!(
                "Logged in to {url} as {}{} (ciabatta {} on the server).",
                session.user.name,
                if session.user.can_write {
                    ""
                } else {
                    " (read-only)"
                },
                health.version,
            );
            if let Some(when) = session.expires_at {
                println!("The session lasts until {when}.");
            }
            if !session.user.groups.is_empty() {
                println!("Groups: {}", session.user.groups.join(", "));
            }

            // Mention a newer ciabatta here rather than on every build.
            if let Some(release) = &health.release
                && let Some(notice) = remote_cache::releases::UpdateStatus::compare(
                    release,
                    remote_cache::releases::current_platform(),
                )
                .notice()
            {
                println!();
                println!("{notice}");
            }

            println!();
            println!("Point a workspace at it with:");
            println!("  ciabatta cache init --remote {url}");
            Ok(())
        }

        RemoteCacheCommand::Logout { url } => {
            let mut credentials = Credentials::load();
            match url {
                Some(url) => {
                    // Tell the server too, so the session really ends rather
                    // than merely being forgotten locally.
                    if let Ok(client) = Client::saved(&url) {
                        let _ = client.logout().await;
                    }
                    if credentials.remove(&url) {
                        credentials.save()?;
                        println!("Logged out of {url}.");
                    } else {
                        println!("Wasn't logged in to {url}.");
                    }
                }
                None => {
                    let urls: Vec<String> = credentials.servers.keys().cloned().collect();
                    if urls.is_empty() {
                        println!("Not logged in to any remote cache.");
                        return Ok(());
                    }
                    for url in &urls {
                        if let Ok(client) = Client::saved(url) {
                            let _ = client.logout().await;
                        }
                        credentials.remove(url);
                    }
                    credentials.save()?;
                    println!("Logged out of {} remote cache(s).", urls.len());
                }
            }
            Ok(())
        }

        RemoteCacheCommand::Status { url } => {
            let url = match url {
                Some(url) => url,
                None => configured_remote()?.ok_or_else(|| {
                    anyhow::anyhow!(
                        "No remote cache is configured for this workspace, and none was named.\n\
                         Usage: ciabatta remote-cache status <URL>"
                    )
                })?,
            };

            let client = Client::saved(&url)?;
            let stats = match client.stats().await {
                Ok(stats) => stats,
                Err(e) => return Err(explain_auth_failure(&url, e)),
            };
            print_remote_status(&url, &stats);
            Ok(())
        }

        RemoteCacheCommand::AddUser { name, read_only } => {
            let token = remote_cache::auth::generate_token();
            let hash = cache::hash_bytes(token.as_bytes());

            println!("Token for {name} (shown once — the server only ever stores its hash):");
            println!();
            println!("  {token}");
            println!();
            println!("Add this under `auth.users` in the server's config:");
            println!();
            println!("  - name: {name}");
            println!("    token_sha256: \"{hash}\"");
            if read_only {
                println!("    read_only: true");
            }
            println!();
            println!("Then set `auth.mode: token` and restart the server.");
            println!("They log in with:");
            println!("  ciabatta remote-cache login <URL> --username {name}");
            Ok(())
        }
    }
}

/// The remote cache URL this workspace is configured to use, if any.
fn configured_remote() -> Result<Option<String>> {
    let cwd = env::current_dir().context("Failed to get current directory")?;
    let Some(root) = find_root(&cwd) else {
        return Ok(None);
    };
    let config = load_config(&root)?;
    Ok(config
        .cache
        .as_ref()
        .and_then(|c| c.remote())
        .map(|r| r.url.clone()))
}

/// Print a remote cache's stats.
fn print_remote_status(url: &str, stats: &serde_json::Value) {
    println!("{url}");

    let storage = &stats["storage"];
    println!(
        "  storage    {} entr(ies), {}",
        storage["entries"].as_u64().unwrap_or(0),
        storage["human"].as_str().unwrap_or("0 B"),
    );

    let counters = &stats["counters"];
    let hits = counters["hits"].as_u64().unwrap_or(0);
    let misses = counters["misses"].as_u64().unwrap_or(0);
    match stats["hit_rate"].as_f64() {
        Some(rate) => println!("  hit rate   {rate:.1}%  ({hits} hit, {misses} miss)"),
        None => println!("  hit rate   nothing looked up yet"),
    }
    println!(
        "  served     {}",
        cache::store::human_size(counters["bytes_served"].as_u64().unwrap_or(0))
    );
    println!(
        "  retention  {}",
        stats["retention"]["description"].as_str().unwrap_or("—")
    );
    println!(
        "  sessions   {} live",
        stats["sessions"].as_u64().unwrap_or(0)
    );

    if let Some(projects) = stats["projects"].as_array()
        && !projects.is_empty()
    {
        println!();
        println!("Projects:");
        for entry in projects {
            let project = &entry["project"];
            let counters = &entry["counters"];
            let stale = entry["stale_workflows"].as_u64().unwrap_or(0);
            println!(
                "  {:<24} {:<38} {} hit / {} miss{}",
                project["name"].as_str().unwrap_or("?"),
                project["id"].as_str().unwrap_or("?"),
                counters["hits"].as_u64().unwrap_or(0),
                counters["misses"].as_u64().unwrap_or(0),
                match stale {
                    0 => String::new(),
                    n => format!("  ·  {n} stale workflow(s)"),
                },
            );
        }
    }

    // The question only this server can answer: not "have I run this lately"
    // but "has anybody, on any machine, in any checkout". A workflow nobody
    // anywhere has run is the one worth deleting; one you personally haven't
    // run may be the one CI runs hourly.
    let workflows = &stats["workflows"];
    if let Some(stale) = workflows["stale"].as_array() {
        let tracked = workflows["tracked"].as_u64().unwrap_or(0);
        println!();
        println!(
            "Workflows: {tracked} tracked, {} not run by anyone in over {}",
            stale.len(),
            workflows["stale_after"].as_str().unwrap_or("30d"),
        );
        // Named by project, not just by workflow: one cache serves many
        // repositories, and half of them call their workflow `build`. A list of
        // bare workflow names would be unactionable the moment there are two.
        let names: std::collections::HashMap<&str, &str> = stats["projects"]
            .as_array()
            .map(|projects| {
                projects
                    .iter()
                    .filter_map(|entry| {
                        Some((
                            entry["project"]["id"].as_str()?,
                            entry["project"]["name"].as_str()?,
                        ))
                    })
                    .collect()
            })
            .unwrap_or_default();

        for entry in stale.iter().take(20) {
            let id = entry["project"].as_str().unwrap_or("?");
            println!(
                "  {:<20} {:<24} {} days ago  ({} run(s) ever)",
                names.get(id).copied().unwrap_or(id),
                entry["workflow"].as_str().unwrap_or("?"),
                entry["days"].as_i64().unwrap_or(0),
                entry["runs"].as_u64().unwrap_or(0),
            );
        }
        if stale.len() > 20 {
            println!("  … and {} more", stale.len() - 20);
        }
    }
}

// ─── self update ────────────────────────────────────────────────────────────

/// Dispatch `ciabatta self update`.
///
/// The binary comes from the cache the workspace already trusts for artifacts,
/// and is verified against the SHA-256 that cache advertised before anything on
/// disk is touched.
async fn cmd_self(subcommand: SelfCommand) -> Result<()> {
    let SelfCommand::Update { from, check, force } = subcommand;

    let url = match from {
        Some(url) => url,
        None => configured_remote()?.ok_or_else(|| {
            anyhow::anyhow!(
                "No remote cache is configured for this workspace, so there's nowhere to \
                 update from.\nName one with `ciabatta self update --from <URL>`."
            )
        })?,
    };

    let client = remote_cache::client::Client::saved(&url)?;
    let release = client.release().await?;
    let platform = remote_cache::releases::current_platform();

    let status = remote_cache::releases::UpdateStatus::compare(&release, platform);
    match &status {
        remote_cache::releases::UpdateStatus::Unavailable => {
            println!("{url} has no ciabatta build for {platform}.");
            if !release.builds.is_empty() {
                let have: Vec<&str> = release.builds.keys().map(|s| s.as_str()).collect();
                println!("It carries builds for: {}.", have.join(", "));
            }
            return Ok(());
        }
        remote_cache::releases::UpdateStatus::Unknown(why) => {
            bail!("Couldn't work out what's currently installed: {why}");
        }
        remote_cache::releases::UpdateStatus::UpToDate if !force => {
            println!(
                "Already running what {url} serves (ciabatta {}).",
                release.version
            );
            return Ok(());
        }
        _ => {}
    }

    let build = release
        .build(platform)
        .context("the release check said there was a build for this platform")?;

    if check {
        println!("An update is available from {url}:");
        println!("  version  {}", release.version);
        println!("  size     {}", cache::store::human_size(build.size));
        println!("  sha256   {}", build.sha256);
        if let Some(notes) = &release.notes {
            println!("  notes    {notes}");
        }
        println!("\nInstall it with `ciabatta self update`.");
        return Ok(());
    }

    println!(
        "Downloading ciabatta {} for {platform} from {url} ({})…",
        release.version,
        cache::store::human_size(build.size)
    );
    let bytes = client.download_release(platform).await?;

    // `install` re-checks the hash before it touches anything, so a truncated
    // download costs a retry rather than a broken installation.
    let path = remote_cache::releases::install(&bytes, &build.sha256)?;
    println!(
        "Installed ciabatta {} to {}",
        release.version,
        path.display()
    );
    if let Some(notes) = &release.notes {
        println!("\n{notes}");
    }
    Ok(())
}

/// Read a line from the terminal.
fn prompt_line(message: &str) -> Result<String> {
    use std::io::Write;
    print!("{message}");
    std::io::stdout().flush()?;
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    Ok(line.trim().to_string())
}

/// Read a secret from the terminal without echoing it.
///
/// Falls back to a visible read when the terminal won't turn echo off (a pipe,
/// a CI job) — with a warning, because silently echoing a password would be
/// worse than saying so.
fn prompt_secret(message: &str) -> Result<String> {
    use std::io::Write;

    print!("{message}");
    std::io::stdout().flush()?;

    match read_without_echo() {
        Ok(secret) => {
            println!();
            Ok(secret)
        }
        Err(_) => {
            println!();
            eprintln!(
                "note: this terminal won't hide input, so what you type will be visible. \
                 Use --password-env to avoid typing it at all."
            );
            let mut line = String::new();
            std::io::stdin().read_line(&mut line)?;
            Ok(line.trim().to_string())
        }
    }
}

/// Read a line with terminal echo disabled.
fn read_without_echo() -> Result<String> {
    use crossterm::terminal;

    terminal::enable_raw_mode().context("this terminal won't switch to raw mode")?;
    let mut secret = String::new();
    let result = (|| -> Result<()> {
        loop {
            match crossterm::event::read()? {
                crossterm::event::Event::Key(key) => match key.code {
                    crossterm::event::KeyCode::Enter => return Ok(()),
                    crossterm::event::KeyCode::Backspace => {
                        secret.pop();
                    }
                    crossterm::event::KeyCode::Char('c')
                        if key
                            .modifiers
                            .contains(crossterm::event::KeyModifiers::CONTROL) =>
                    {
                        bail!("cancelled");
                    }
                    crossterm::event::KeyCode::Char(c) => secret.push(c),
                    _ => {}
                },
                _ => continue,
            }
        }
    })();
    let _ = terminal::disable_raw_mode();
    result?;
    Ok(secret)
}

// ─── Caching ────────────────────────────────────────────────────────────────

/// Dispatch `ciabatta dry-run`: plan what a run would reuse, and run nothing.
///
/// A cache is a promise that skipping work is safe. Nobody should have to take
/// that on faith, so this answers the question directly — for every step,
/// whether it would be reused, and when it wouldn't, which of its three
/// dependencies moved.
async fn cmd_dry_run(
    targets: &[String],
    show_diff: bool,
    as_json: bool,
    env_flags: &[String],
    local: bool,
    config: Option<&Path>,
) -> Result<()> {
    let cwd = env::current_dir().context("Failed to get current directory")?;
    let (root, cfg) = load_project(config)?;

    // Quiet when producing JSON: a stray "resolved variables" banner on stdout
    // would make the output unparseable.
    let vars = build_env_vars(&cfg, env_flags, local, &root, !as_json)?;
    let store = cache::graph::store_for(&root)?;

    // Compile the same graph a real run would, so the preview can't disagree
    // with what happens next.
    let workspace = workspace::Workspace::discover(&cwd).ok();
    let (steps, _) = resolve_dry_run_steps(&cwd, &root, &cfg, targets, &workspace)?;

    let context = cache::cli::WorkspaceContext {
        workspace: workspace.as_ref(),
        root: root.clone(),
        config: &cfg,
    };
    let plan = cache::graph::plan_graph(&steps, &context, &cache::cli::env_map(&vars), &store)?;

    if as_json {
        println!(
            "{}",
            serde_json::to_string_pretty(&cache::cli::plan_json(&plan))?
        );
        return Ok(());
    }

    cache::cli::print_plan(&plan, &store, show_diff);
    Ok(())
}

/// Resolve the steps a dry run should plan, from workflow names, workflow names,
/// or (with neither) every run-capable workflow in the project.
///
/// Returns the steps plus the workflow-level cache settings when exactly one
/// workflow was named — a workflow's own `cache:` applies to its whole graph, and
/// with several named there's no single answer.
fn resolve_dry_run_steps(
    cwd: &Path,
    _root: &Path,
    _cfg: &CiabattaConfig,
    targets: &[String],
    workspace: &Option<workspace::Workspace>,
) -> Result<(Vec<run::RunStep>, Option<cache::CacheConfig>)> {
    // A named workflow compiles across the whole monorepo.
    if let (Some(ws), Some(first)) = (workspace.as_ref(), targets.first())
        && ws.workflow_names().iter().any(|name| name == first)
    {
        let selection = workspace::graph::Selection::default();
        let (_, graph) = workspace::graph::prepare_many(cwd, targets, &selection)?;
        return Ok((graph.steps, None));
    }

    bail!(
        "'{}' is not a workflow in this project. Run `ciabatta list` to see what is.",
        targets
            .first()
            .map(String::as_str)
            .unwrap_or("(nothing named)")
    )
}

/// Which workflow `ciabatta cache init` should describe.
///
/// A package with one workflow doesn't need to say which — that's the common
/// case and asking would be noise. With several, there is no sensible default:
/// what a `build` reads is not what a `test` reads, and guessing would write
/// the wrong answer into the wrong file.
fn resolve_cache_workflow(root: &Path, named: Option<&str>) -> Result<String> {
    let dir = root
        .join(config::CIABATTA_DIR)
        .join(workspace::WORKFLOWS_DIR);
    let mut available = workspace::workflow_names_in(&dir);
    available.sort();

    if let Some(name) = named {
        if !available.iter().any(|n| n == name) {
            bail!(
                "No workflow called '{name}' in {}.\n\
                 {}",
                dir.display(),
                describe_available(&available)
            );
        }
        return Ok(name.to_string());
    }

    match available.len() {
        1 => Ok(available.remove(0)),
        0 => bail!(
            "No workflows in {}.\n\
             Cache settings describe what a build reads, so there has to be a build \
             to describe. Run `ciabatta init --lib` to scaffold one.",
            dir.display()
        ),
        _ => bail!(
            "This package has several workflows, so name the one to describe:\n  {}\n\n\
             They read different files — a `build` and a `test` shouldn't share one \
             cache section.",
            available.join("\n  ")
        ),
    }
}

/// "Available: a, b." or a nudge when there are none.
fn describe_available(names: &[String]) -> String {
    if names.is_empty() {
        "This package defines none yet.".to_string()
    } else {
        format!("Available: {}.", names.join(", "))
    }
}

/// `ciabatta register`: tell the daemon this directory exists.
///
/// Every web-facing command already registers the directory it ran in, so the
/// switcher usually fills itself in. This is for the case it can't: a checkout
/// you haven't run anything in yet, a machine where the web app is the way you
/// work, or a setup script that wants the project listed before anyone opens
/// the page.
async fn cmd_register(path: Option<&Path>, quiet: bool, port: Option<u16>) -> Result<()> {
    let cwd = match path {
        Some(path) => path.to_path_buf(),
        None => env::current_dir().context("Failed to get current directory")?,
    };
    let dir = cwd
        .canonicalize()
        .with_context(|| format!("No such directory: {}", cwd.display()))?;

    // Registering a directory with no config would put a row in the switcher
    // that every page then fails to load. Say so here, where the fix is one
    // command away.
    if config::config_path(&dir).is_none() {
        bail!(
            "{} has no .ciabatta/ directory, so there's nothing to register.\n\
             Run `ciabatta init` here first (it registers for you), or point at \
             a checkout that has one with --path.",
            dir.display()
        );
    }

    let session = daemon::connect_at(port, &dir).await?;

    if quiet {
        println!("{}", session.project.id);
        return Ok(());
    }

    println!("Registered {} with the ciabatta daemon.", dir.display());
    println!(
        "  project: {} ({})",
        session.project.name, session.project.id
    );
    println!("  web app: {}", session.page_url("/"));
    Ok(())
}

/// Register a freshly-created workspace, without letting it fail the `init`.
///
/// Registration starts the daemon, and a daemon that won't start is a bad
/// reason for `ciabatta init` to report failure — the config it just wrote is
/// on disk and correct either way. So this reports what went wrong and returns.
async fn register_quietly(root: &Path, port: Option<u16>) {
    match daemon::connect_at(port, root).await {
        Ok(session) => {
            println!();
            println!(
                "Registered with the daemon — open it at {}",
                session.page_url("/")
            );
        }
        Err(e) => {
            println!();
            println!("note: couldn't register with the daemon ({e:#}).");
            println!("      Everything else works; run `ciabatta register` to retry.");
        }
    }
}

/// Dispatch `ciabatta cache …`.
fn cmd_cache(subcommand: CacheCommand) -> Result<()> {
    let cwd = env::current_dir().context("Failed to get current directory")?;
    let root = find_root(&cwd).unwrap_or_else(|| cwd.clone());

    match subcommand {
        CacheCommand::Init {
            workflow,
            enable,
            remote,
            force,
        } => {
            let workflow = resolve_cache_workflow(&root, workflow.as_deref())?;
            // Look in this package, but write the patterns relative to the
            // monorepo root — that is what the cache resolves them against.
            let workspace_root =
                workspace::find_workspace_root(&cwd).unwrap_or_else(|| root.clone());
            let rel = root
                .strip_prefix(&workspace_root)
                .ok()
                .map(|rel| rel.to_string_lossy().replace('\\', "/"))
                .filter(|rel| !rel.is_empty());
            let proposal = cache::cli::propose_under(&root, rel.as_deref());

            println!("Looking at {} …\n", root.display());
            if proposal.inputs.is_empty() {
                println!(
                    "Nothing recognizable as source was found here. The `cache:` section \
                     will be written with the parts you need to fill in marked TODO —\n\
                     `inputs` is the one that has to be right."
                );
            } else {
                println!("Inputs (a change to any of these means a rebuild):");
                for pattern in &proposal.inputs {
                    let why = proposal
                        .reasons
                        .iter()
                        .find(|(p, _)| p == pattern)
                        .map(|(_, why)| *why)
                        .unwrap_or("");
                    println!("  {pattern:<20} {why}");
                }
            }
            if proposal.outputs.is_empty() {
                println!(
                    "\nNo build output directory was found yet, so `outputs` is left for you \
                     to fill in.\nWith none declared there's nothing to restore, and every \
                     build runs."
                );
            } else {
                println!("\nOutputs (restored on a hit, and verified before one is granted):");
                for pattern in &proposal.outputs {
                    println!("  {pattern}");
                }
            }

            // Enabling a cache whose inputs or outputs are still TODO would
            // turn caching "on" and then quietly never hit, which reads as the
            // feature being broken rather than as the config being unfinished.
            let enable = if enable && !proposal.is_usable() {
                println!(
                    "\nNot turning caching on yet: with the gaps above, every build would \
                     still run.\nFill them in, then set `enabled: true`."
                );
                false
            } else {
                enable
            };

            let written = cache::cli::write_cache_section(
                &root,
                &workflow,
                &proposal,
                enable,
                remote.as_deref(),
                force,
            )?;
            println!();
            for path in &written {
                println!("Wrote {}", path.display());
            }

            // A step with its own `cache:` wins over what was just written. Say
            // so, or "caching is on" followed by nothing being cached reads as
            // the feature being broken.
            let overrides = cache::cli::overriding_steps(&root, &workflow);
            if !overrides.is_empty() {
                println!();
                println!(
                    "Note: these declare their own `cache:`, which wins over the section \
                     above —"
                );
                for (name, enabled) in &overrides {
                    println!(
                        "  {name:<32} {}",
                        if *enabled {
                            "cached with its own settings"
                        } else {
                            "still off"
                        }
                    );
                }
                println!(
                    "Remove those to inherit the workspace's settings, or fill them in \
                     where they are."
                );
            }

            if enable {
                println!("\nCaching is on. Check it with `ciabatta dry-run <workflow>`.");
            } else {
                println!(
                    "Caching is still off — review the section, then set `enabled: true`.\n\
                     Preview what it would do first with `ciabatta dry-run <workflow>`."
                );
            }
            if remote.is_some() {
                println!(
                    "The remote's project id is filled in the first time this workspace \
                     connects. Commit it."
                );
            }
            Ok(())
        }

        CacheCommand::Status => {
            let store = cache::graph::store_for(&root)?;
            cache::cli::print_status(&store)
        }

        CacheCommand::Clean { yes } => {
            let store = cache::graph::store_for(&root)?;
            let stats = store.stats()?;
            if stats.entries == 0 {
                println!("The local cache is already empty.");
                return Ok(());
            }
            if !yes {
                print!(
                    "Delete {} cached entr(ies) ({})? [y/N]: ",
                    stats.entries,
                    cache::store::human_size(stats.size)
                );
                use std::io::Write;
                std::io::stdout().flush()?;
                let mut answer = String::new();
                std::io::stdin().read_line(&mut answer)?;
                if !matches!(answer.trim().to_lowercase().as_str(), "y" | "yes") {
                    println!("Left it alone.");
                    return Ok(());
                }
            }
            let removed = store.clear()?;
            println!("Removed {removed} entr(ies).");
            Ok(())
        }

        CacheCommand::Prune {
            max_age,
            max_size,
            max_entries,
            dry_run,
        } => {
            let store = cache::graph::store_for(&root)?;
            let policy = cache::cli::retention_from_flags(max_age, max_size, max_entries);
            cache::cli::print_prune(&store, &policy, dry_run)
        }
    }
}

/// Dispatch `ciabatta config migrate`: convert this checkout's TOML config to
/// YAML.
///
/// Defaults to the whole workspace rather than the current directory. A
/// monorepo's config is spread across every member, and converting one package
/// while leaving its siblings behind is the state most likely to confuse
/// somebody later.
fn cmd_config_migrate(path: Option<&Path>, dry_run: bool) -> Result<()> {
    let cwd = env::current_dir().context("Failed to get current directory")?;
    let root = match path {
        Some(p) => p.to_path_buf(),
        None => workspace::find_workspace_root(&cwd)
            .or_else(|| find_root(&cwd))
            .unwrap_or(cwd),
    };

    println!(
        "Migrating ciabatta config under {} to YAML…\n",
        root.display()
    );
    let report = migrate::migrate(&root, dry_run)?;
    migrate::print_report(&report, dry_run)
}

fn cmd_init(ci: Option<&str>, containers: Option<&str>, force: bool) -> Result<PathBuf> {
    use config::{CIABATTA_DIR, CONFIG_FILE};
    use std::fs;

    let cwd = env::current_dir().context("Failed to get current directory")?;

    // Don't walk upward — init always targets the cwd.
    let dir = cwd.join(CIABATTA_DIR);
    let config_path = dir.join(CONFIG_FILE);

    if let Some(existing) = config::config_path(&cwd)
        && !force
    {
        bail!(
            "{} already exists.\n\
             Use --force to overwrite, or edit it directly.",
            existing.display()
        );
    }

    fs::create_dir_all(&dir).with_context(|| format!("Failed to create {}", dir.display()))?;

    fs::write(&config_path, build_starter_config(ci, containers))
        .with_context(|| format!("Failed to write {}", config_path.display()))?;

    println!("Initialized ciabatta project in {}", cwd.display());
    println!("Created: {}", config_path.display());
    println!();
    println!("Next steps:");
    println!("  1. Edit .ciabatta/ciabatta.yaml to define your registries and workflows.");
    println!("  2. Run `ciabatta list` to verify your workflows are recognized.");
    println!("  3. Run `ciabatta <workflow> --dry-run` to preview what would happen.");
    println!("  4. Run `ciabatta tui` to browse your registries.");
    println!();
    println!("For config format documentation: ciabatta config reference");

    Ok(cwd)
}

/// Dispatch `ciabatta init --lib`: opt this directory in as a sub-workspace of
/// the monorepo.
///
/// Writes two things: a `[workspace]` identity — name, description, owner,
/// dependencies — and a starter workflow with a described, owned step. The
/// prompts for description and owner are not politeness; an unowned script
/// nobody can describe is precisely what this schema exists to stop
/// accumulating.
fn cmd_init_lib(
    name: Option<&str>,
    description: Option<&str>,
    owner: Option<&str>,
    depends_on: &[String],
    workflow: &str,
    force: bool,
) -> Result<PathBuf> {
    use config::{CIABATTA_DIR, CONFIG_FILE};
    use std::fs;

    let cwd = env::current_dir().context("Failed to get current directory")?;
    let dir = cwd.join(CIABATTA_DIR);
    let config_path = dir.join(CONFIG_FILE);
    let workflow_path = dir
        .join(workspace::WORKFLOWS_DIR)
        .join(format!("{workflow}.{}", format::YAML_EXT));

    if let Some(existing) = config::config_path(&cwd)
        && !force
    {
        bail!(
            "{} already exists.\n\
             Use --force to overwrite, or add a workflow by hand under .ciabatta/{}/.",
            existing.display(),
            workspace::WORKFLOWS_DIR
        );
    }

    let name = name
        .map(str::to_string)
        .or_else(|| {
            cwd.file_name()
                .and_then(|n| n.to_str())
                .map(|s| s.to_string())
        })
        .unwrap_or_else(|| "package".to_string());
    // Defaulting the owner to the git user means the common case needs no flag
    // and still ends up attributed to a real person.
    let owner = owner.map(str::to_string).or_else(git_user_name);

    fs::create_dir_all(workflow_path.parent().unwrap())
        .with_context(|| format!("Failed to create {}", dir.display()))?;

    fs::write(
        &config_path,
        build_lib_config(&name, description, owner.as_deref(), depends_on),
    )
    .with_context(|| format!("Failed to write {}", config_path.display()))?;

    if workflow_path.exists() && !force {
        println!("Kept the existing workflow at {}.", workflow_path.display());
    } else {
        fs::write(
            &workflow_path,
            build_starter_workflow(workflow, &name, owner.as_deref()),
        )
        .with_context(|| format!("Failed to write {}", workflow_path.display()))?;
    }

    println!("Opted '{name}' into the ciabatta workspace.");
    println!("Created: {}", config_path.display());
    println!("Created: {}", workflow_path.display());
    println!();
    if description.is_none() || owner.is_none() {
        println!("Before you commit this, fill in:");
        if description.is_none() {
            println!("  • [workspace] description — what lives in this package");
        }
        if owner.is_none() {
            println!("  • [workspace] owner — who to ask about it");
        }
        println!(
            "  Both show up in `ciabatta list`, which is how everyone else finds your scripts."
        );
        println!();
    }
    println!("Next steps:");
    println!(
        "  1. Edit .ciabatta/{}/{workflow}.toml — describe each step and what it needs.",
        workspace::WORKFLOWS_DIR
    );
    println!("  2. Declare cross-package dependencies with [workspace] depends_on.");
    println!("  3. Run `ciabatta {workflow} --graph` to see the whole monorepo graph.");
    println!("  4. Run `ciabatta {workflow} --dry-run` to walk it without side effects.");
    println!();
    println!("For the full schema: ciabatta config reference");

    Ok(cwd)
}

/// The current git user's name, used to default a new sub-workspace's owner.
fn git_user_name() -> Option<String> {
    let output = std::process::Command::new("git")
        .args(["config", "user.name"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let name = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!name.is_empty()).then_some(name)
}

/// The `ciabatta.yaml` written by `init --lib`: a sub-workspace's identity, with
/// the parts it should fill in left visible rather than omitted.
fn build_lib_config(
    name: &str,
    description: Option<&str>,
    owner: Option<&str>,
    depends_on: &[String],
) -> String {
    let description_line = match description {
        Some(text) => format!("  description: {}", yaml_scalar(text)),
        None => "  description: \"\"          # TODO: one line on what lives here".to_string(),
    };
    let owner_line = match owner {
        Some(text) => format!("  owner: {}", yaml_scalar(text)),
        None => "  owner: \"\"                # TODO: who to ask about this package".to_string(),
    };
    let depends_line = if depends_on.is_empty() {
        "  depends_on: []           # e.g. [\"proto:generate\", \"common\"]".to_string()
    } else {
        format!(
            "  depends_on: [{}]",
            depends_on
                .iter()
                .map(|d| yaml_scalar(d))
                .collect::<Vec<_>>()
                .join(", ")
        )
    };

    format!(
        r#"# This package's ciabatta configuration.
# Run `ciabatta config reference` for the full schema.

# ─── Identity ──────────────────────────────────────────────────────────────────
# Who this package is in the monorepo, and what it needs before it can build.
# `ciabatta list` shows all of this, so nobody has to open your scripts to find
# out what they do or who owns them.
workspace:
  name: {name}
{description_line}
{owner_line}
{depends_line}
  tags: []                 # free-form labels, searchable with `ciabatta list -s`
  requires: []             # tools every workflow here needs on PATH
  # env_file: .env         # sourced before any workflow here runs
  # env_default: .env.default   # the template .env is generated from

  # Standard environment variables for every step defined in this package.
  # env:
  #   RUST_LOG: info

# ─── Caching ───────────────────────────────────────────────────────────────────
# Off by default. Opt in, declare what a build reads and what it produces, and
# ciabatta will skip the work when neither has changed. `ciabatta cache init`
# walks you through it; `ciabatta dry-run <workflow>` shows what would be reused.
#
# cache:
#   enabled: true
#   inputs:  ["src/**/*.rs", "Cargo.toml"]
#   outputs: ["target/release/{name}"]

# ─── Workflows ─────────────────────────────────────────────────────────────────
# One file per workflow, under .ciabatta/workflows/. Any sub-workspace that
# defines a workflow of the same name joins that graph: `ciabatta build` runs
# every `build` workflow in the monorepo, in dependency order.
#
# Small packages can write them inline instead:
#
# workflows:
#   test:
#     description: Run the unit tests
#     steps:
#       - name: unit
#         description: cargo test
#         run: cargo test

# ─── Registries and workflows ────────────────────────────────────────────────────
# Publishing targets for this package, if it publishes anything. A workflow step
# with `kind: push` and `workflow: <name>` runs one of these as a graph node.
#
# registries:
#   nexus:
#     url: https://nexus.example.com
#     repository: raw-hosted
"#
    )
}

/// The starter workflow file: a real, runnable step that models the habits the
/// schema is trying to establish — describe it, own it, declare what it needs.
fn build_starter_workflow(workflow: &str, member: &str, owner: Option<&str>) -> String {
    let owner_line = match owner {
        Some(text) => format!("owner: {}", yaml_scalar(text)),
        None => "owner: \"\"          # TODO: who owns this workflow".to_string(),
    };

    format!(
        r#"# The "{workflow}" workflow for {member}.
#
# Run it across the whole monorepo with `ciabatta {workflow}`, or just this
# package with `ciabatta {workflow} --only {member}`. See the graph first with
# `ciabatta {workflow} --graph`.

description: "TODO: what running this accomplishes"
{owner_line}

# Workflows in other sub-workspaces that must finish first. "other" means their
# workflow of this same name; "other:generate" names a specific one.
needs: []

# Tools every step here needs on PATH. Missing ones are reported before anything
# runs, with the install command from the root's `toolchain:` section.
requires: []

# REQUIRED_ENV: [API_TOKEN]   # refuse to start unless these are set
# env_file: .env              # sourced before the run

steps:
  - name: {workflow}
    description: "TODO: what this step does, and what it expects to be true first"
    run: echo 'replace me with the real {workflow} command'
    # script: scripts/{workflow}.sh   # …or a script, run from this package
    # requires: [cargo]               # tools this step in particular needs
    # timeout: 10m                    # kill it past this; the graph carries on
    # retries: 1                      # extra attempts, for flaky steps
    # needs: [some-earlier-step]      # ordering within this workflow

  # A long-running step — a dev server, a watcher — that the graph must not wait
  # for. It starts, everything downstream is released, and the ciabatta daemon
  # takes ownership of it as a watch session, so it keeps running after the run
  # finishes. Follow it with `ciabatta watch --attach <ID>` (the run prints the
  # id), or find it later with `ciabatta watch --list`.
  #
  # - name: dev-server
  #   description: Serve the app for the e2e steps
  #   run: npm run dev
  #   persistent: true

  # Publishing is just another node on the graph: a step whose kind is "push",
  # naming a workflow from this package's ciabatta.yaml.
  #
  # - name: publish
  #   description: Publish the built artifact
  #   kind: push
  #   workflow: binary
  #   needs: [{workflow}]
"#
    )
}

/// Quote a value for YAML when it needs it, and leave it bare when it doesn't.
///
/// Bare scalars read far better in a scaffolded file, but a value starting with
/// a special character — or one that YAML would read as a bool, null, or number
/// — has to be quoted or it changes meaning.
fn yaml_scalar(value: &str) -> String {
    const SPECIAL_LEAD: &[char] = &[
        '-', '?', ':', ',', '[', ']', '{', '}', '#', '&', '*', '!', '|', '>', '\'', '"', '%', '@',
        '`',
    ];
    let looks_special = value.is_empty()
        || value.starts_with(SPECIAL_LEAD)
        || value.trim() != value
        || value.contains(": ")
        || value.contains(" #")
        || value.contains('\n')
        || matches!(
            value.to_ascii_lowercase().as_str(),
            "true" | "false" | "yes" | "no" | "on" | "off" | "null" | "~"
        )
        || value.parse::<f64>().is_ok();

    if looks_special {
        // serde_yaml_ng quotes and escapes exactly as the parser expects.
        serde_yaml_ng::to_string(value)
            .map(|s| s.trim_end().trim_start_matches("--- ").to_string())
            .unwrap_or_else(|_| format!("{value:?}"))
    } else {
        value.to_string()
    }
}

fn build_starter_config(ci: Option<&str>, containers: Option<&str>) -> String {
    // When the runtime isn't pinned, leave it commented out so ciabatta
    // auto-detects podman/docker at run time.
    let containers_line = match containers {
        Some(c) => format!("  containers: {}", yaml_scalar(c)),
        None => {
            r#"  # containers: docker   # docker | podman (auto-detected when unset)"#.to_string()
        }
    };

    let ci_line = match ci {
        Some(s) => format!("  ci: {}", yaml_scalar(s)),
        None => {
            // Auto-detect from environment.
            match detect_ci() {
                Some(name) => format!("  ci: {}  # auto-detected", yaml_scalar(&name)),
                None => {
                    r#"  # ci: github   # gitlab | github | jenkins | circleci | azure | bitbucket"#
                        .to_string()
                }
            }
        }
    };

    format!(
        r#"# Ciabatta configuration
# Run `ciabatta config reference` for full documentation.

system:
{ci_line}
{containers_line}

# ─── Registries ────────────────────────────────────────────────────────────────
# Define each registry you publish to. The key is the registry identifier used
# in workflows. Supported types (auto-detected from the name):
#   nexus, artifactory → HTTP PUT/GET
#   s3                 → aws s3 cp
#   docker             → docker push/pull
#   ecr                → AWS ECR (auto-login)
#
# registries:
#   nexus:
#     url: https://nexus.example.com/repository/releases/
#     tls_verify: true
#     needs_auth: true
#     login_script: .ciabatta/nexus_login.sh
#
#   ecr:
#     url: 123456789.dkr.ecr.us-east-1.amazonaws.com
#     needs_auth: false   # ciabatta auto-fetches the ECR token

# ─── Workflows ─────────────────────────────────────────────────────────────────
# A workflow is a named DAG of steps, and it is the only thing ciabatta runs.
# `ciabatta <name>` compiles every package's workflow of that name into one
# graph and runs it in dependency order.
#
# The usual home is one file per workflow — .ciabatta/workflows/<name>.yaml,
# where the filename IS the name. A small project can write them inline here.
#
# workflows:
#   release:
#     description: Build the app and publish it
#     REQUIRED_ENV: [API_TOKEN]        # refuse to start unless these are set
#     steps:
#       - name: build
#         run: make dist
#         requires: [make]             # reported up front if missing
#
#       # Publishing is a step, not a separate command: it declares what it
#       # needs, so it cannot run before the artifact exists.
#       - name: publish
#         kind: push
#         needs: [build]
#         registry: nexus
#         artifact: dist/app.tar.gz
#         publish_path: "myteam/app/{{CIABATTA_BRANCH}}/{{CIABATTA_COMMIT}}/app.tar.gz"
#
#       # A step with its own command runs that instead of the built-in move.
#       - name: publish-by-hand
#         kind: push
#         script: scripts/publish.sh
#
#   fetch:
#     steps:
#       # Pull is the same artifact the other way. `from` names the push step it
#       # mirrors, so the registry and path are stated once.
#       - name: fetch
#         kind: pull
#         from: release:publish
#
# Variables available in publish_path: {{CIABATTA_BRANCH}}, {{CIABATTA_COMMIT}},
#                                      {{CIABATTA_TAG}}, {{CIABATTA_BUILD_NUMBER}}
#
# There are no stages: what a `pre` command used to do is a step with an edge to
# it, and what a `post` command did is a step that `needs` the transfer. Each is
# now a node you can see on the graph, filter, cache and fail on its own.
#
# See `ciabatta config reference` for the full field listing, and watch a run in
# the browser with `ciabatta <name> --gui`.
#
# ─── Environment ───────────────────────────────────────────────────────────────
# Ciabatta sources `.env` from the project root by default — you don't have to
# say so. Point `env_file` somewhere else to override that for this workspace,
# and name the checked-in template every `.env` is generated from:
#
# workspace:
#   env_file: config/dev.env
#   env_default: .env.default
#
# ─── Caching ───────────────────────────────────────────────────────────────────
# Off by default. Declare what a build reads and what it writes, and ciabatta
# reuses the previous result when neither changed. `ciabatta cache init` sets
# this up for you; `ciabatta dry-run <workflow>` previews hits and rebuilds.
#
# cache:
#   enabled: true
#   inputs:  ["src/**/*", "package.json"]
#   outputs: ["dist/**/*"]
#   remote:  true          # also read/write the shared remote cache
#
# ─── Credentials ───────────────────────────────────────────────────────────────
# When a registry has no login_script, ciabatta reads per-registry credentials:
#   CIABATTA_<REGISTRY>_USER  /  CIABATTA_<REGISTRY>_PASS
# e.g. for `registries.nexus`: CIABATTA_NEXUS_USER / CIABATTA_NEXUS_PASS.
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

/// The workflows this project's own config declares inline, for `ciabatta list`
/// when there is no monorepo around it to enumerate.
fn list_workflows(cfg: &CiabattaConfig) {
    if cfg.workflows.is_empty() {
        println!(
            "No workflows defined here.\n\
             Add one at .ciabatta/workflows/<name>.yaml, or run `ciabatta init --lib` \
             to start this package off with one."
        );
        return;
    }

    println!("Workflows:");
    let mut names: Vec<_> = cfg.workflows.keys().collect();
    names.sort();
    for name in names {
        let workflow = &cfg.workflows[name];
        let summary = workflow.description.as_deref().unwrap_or("");
        println!("  {name:<30} {summary}");
        for step in &workflow.steps {
            let what = match step.direction() {
                Some(direction) => format!(
                    "{} → {} {}",
                    direction.label(),
                    step.registry.as_deref().unwrap_or("(no registry)"),
                    step.publish_path
                        .as_ref()
                        .map(|p| p.display())
                        .unwrap_or_default()
                ),
                None => step
                    .run
                    .clone()
                    .or_else(|| step.script.clone())
                    .unwrap_or_else(|| "(no action)".to_string()),
            };
            println!("      · {:<24} {}", step.name, what);
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

    if !cfg.workflows.is_empty() {
        println!("\nWorkflows (inline):");
        let mut names: Vec<_> = cfg.workflows.keys().collect();
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

Location: <project-root>/.ciabatta/ciabatta.yaml

The project root is the directory that CONTAINS the .ciabatta directory.
All paths in workflows are relative to this root.

Ciabatta writes YAML from 0.2.0. Older `.toml` files still load exactly as they
did — `ciabatta config migrate` converts a whole checkout when you're ready.

━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
THE MONOREPO SCHEMA
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

A monorepo accumulates scripts nobody owns, publishing to places nobody
remembers, quietly depending on each other in ways nobody wrote down.
Ciabatta's answer: every package opts in with `ciabatta init --lib`, and
declares three things — who owns it, what its workflows do, and which other
packages they need.

Any workflow name then becomes a command:

  ciabatta build              # every `build` workflow in the repo, in order
  ciabatta build --graph      # show the graph, run nothing
  ciabatta build --dry-run    # walk every step, execute nothing
  ciabatta build --only api   # start from one package (deps still come along)
  ciabatta dry-run build      # what would be reused from the cache, and why not
  ciabatta list               # every workflow, its owner, and what it does
  ciabatta list -s proto      # ...filtered

The monorepo root is your git root; every directory beneath it with a
.ciabatta/ciabatta.yaml is a sub-workspace.

workspace:                   # this package's identity (ciabatta init --lib)
  name: api                    # what other packages refer to it by
                               # (defaults to the directory name)
  description: REST API        # shown by `ciabatta list`
  owner: Ada                   # who to ask about it
  depends_on: [proto:generate, common]
                               # other sub-workspaces this one needs, applied
                               # to EVERY workflow here. "common" means their
                               # workflow of the same name (skipped if they
                               # have none); "proto:generate" names one exactly.
  tags: [backend]              # free-form labels, searchable with `list -s`
  requires: [cargo]            # tools every workflow here needs on PATH
  env_file: .env               # sourced before any workflow here runs.
                               # Unset means `.env`; setting it REPLACES that
                               # default rather than adding to it.
  env_default: .env.default    # the checked-in template `.env` is generated
                               # from. REQUIRED of any package whose workflows
                               # declare REQUIRED_ENV — see ENVIRONMENT below.
  umbrella: true               # on the ROOT config only: "I'm not a package,
                               # just shared toolchain and settings"

  env:                         # standard variables for every step defined here
    RUST_LOG: info

toolchain:                   # how to install what workflows `require`
  protoc:
    hint: brew install protobuf     # printed when the tool is missing
    check: protoc --version         # optional: a smarter test than PATH
    description: Protocol buffer compiler

  Usually written once at the monorepo root and inherited by every package.
  Missing tools are reported together, BEFORE the first step runs, with these
  hints attached — not as "command not found" ten minutes into a build.

━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
WORKFLOWS
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

One file per workflow: .ciabatta/workflows/<name>.yaml — the filename IS the
workflow name. Small packages can write them inline under `workflows:` instead
(but not both: a name defined twice is an error).

  description: Compile the service binary   # what running this achieves
  owner: Ada                                # falls back to the package's
  needs: [proto:generate]                   # cross-package deps, on top of
                                            # workspace.depends_on
  requires: [cargo]                         # tools all its steps need
  env_file: .env.build                      # relative to this package
  REQUIRED_ENV: [API_TOKEN]                 # refuse to start unless set
  tags: [rust]

  env:                                      # vars for all its steps
    PROFILE: release

  steps:
    - name: compile
      description: Build the release binary # what it does...
      owner: Ada                            # ...and who to ask
      run: cargo build --release            # an inline shell command
      script: scripts/build.sh              # ...or a script, run from THIS
                                            # package's directory
      needs: [fetch]                        # ordering WITHIN this workflow
                                            # (cross-package deps go on the
                                            #  workflow, not the step)
      requires: [cargo, protoc]             # tools this step needs
      timeout: 10m                          # "30s" "10m" "1h30m", or seconds.
                                            # Past it the step is killed (with
                                            # everything it spawned) and marked
                                            # failed — and the graph keeps going.
      retries: 2                            # extra attempts, for flaky steps
      persistent: true                      # a dev server that never exits:
                                            # started, dependents released
                                            # immediately, and handed to the
                                            # daemon as a watch session, so it
                                            # OUTLIVES the run. The run prints
                                            # its id:
                                            #   ciabatta watch --attach <ID>
                                            #   ciabatta watch --stop   <ID>
                                            #   ciabatta watch --list
      continue_on_error: true               # its failure skips dependents but
                                            # doesn't stop the run
      kind: push                            # push | pull select the built-in
                                            # registry transfer; setup | build |
                                            # test | deploy | anything else is
                                            # a free-form label on the node
      registry: nexus                       # with kind: push/pull — see
      artifact: dist/app                    # "Publishing" below for the whole
      publish_path: "app/{CIABATTA_COMMIT}" # set of transfer fields
      from: release:publish                 # a pull mirroring a push step
      when: RUN_ENV == prod                 # conditions (see Runs, below)
      skip_if: IN_CI
      on_error: fix                         # route failures to a recovery node,
                                            # same as a run flowchart

      env:
        CARGO_TERM_COLOR: always

      cache:                                # per-step cache override; most
        inputs: ["proto/**/*"]              # steps inherit the workspace's

Cross-package wiring, in full:

  Every step of workflow A with no `needs` of its own waits for every terminal
  step of each workflow A depends on. So `api`'s build declaring
  depends_on: [proto:generate] means api's first step doesn't start until
  proto's last one has finished — and `ciabatta build --graph` shows you exactly
  that, node by node, labelled with the package each came from.

━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
CACHING
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

Off until a workspace opts in. A cache that turns itself on is a cache that
will one day serve somebody a stale artifact they never asked to be kept.

  ciabatta cache init          look at the directory, propose inputs and outputs
  ciabatta dry-run <target>    what would be reused, and why not — runs nothing
  ciabatta dry-run <t> --diff  ...with the lines that changed
  ciabatta cache status        what the local store is holding
  ciabatta cache prune --max-age 30d
  ciabatta cache clean

cache:
  enabled: true
  inputs:  ["src/**/*", "Cargo.toml"]   # what the build READS
  outputs: ["target/release/app"]       # what the build WRITES
  exclude: [target]                     # never counted as an input, so a build
                                        # can't invalidate itself with its own
                                        # output. Does NOT filter `outputs`.
  env: [PROFILE]                        # variables the RESULT depends on

  remote:                               # the shared cache (project-level: put
    url: http://cache.example.com:8380  # this in the MONOREPO ROOT's config)
    project: 7f3a-…                     # assigned by the server on first
                                        # contact and written back here. COMMIT
                                        # IT: it's what makes every checkout and
                                        # every CI runner resolve to the same
                                        # project rather than registering a new
                                        # one under the same name.
    read_only: true                     # read the cache, never write to it —
                                        # what a fork's CI should get
    tls_verify: true                    # verify the server's certificate.
                                        # Turn it off for a self-signed or
                                        # internal CA cert this machine doesn't
                                        # have — but with it off, HTTPS is an
                                        # encrypted channel to whoever answered,
                                        # so the artifacts are only as
                                        # trustworthy as the network.
    enabled: true                       # turn it off without deleting settings

A stage has exactly THREE dependencies, and any of them changing is a rebuild:

  1. its input files,
  2. the environment variables it declared in `cache.env`,
  3. the outputs of the stages it `needs`.

The third is what makes a graph cacheable rather than just a directory: change
a .proto file and `proto:generate` misses, its outputs change, and every stage
downstream of it misses too — each for a reason it can name.

Two things worth knowing:

  * An undeclared input is a WRONG ANSWER, not a slow one. If a build reads a
    file that isn't in `inputs`, changing that file won't change the key and the
    cache will confidently hand back the wrong artifact. That's why `cache init`
    scaffolds `inputs` from the directory's real contents, and why `dry-run`
    exists.

  * Outputs are verified, not assumed. A key match says the inputs didn't
    change; it says nothing about whether somebody deleted `dist/` or edited a
    generated file by hand. So the outputs are hashed too, and a mismatch is a
    restore or a rebuild.

Cache settings live WITH THE WORKFLOW that reads those files —
`.ciabatta/workflows/<name>.yaml`, next to the steps — because what a build
reads is a property of that build. A `build` and a `test` in one package read
different files, and one section per config gave them no way to say so.

  # packages/api/.ciabatta/workflows/build.yaml
  cache:
    enabled: true
    inputs:  ["src/**/*"]
    outputs: ["target/release/app"]

  steps:
    - name: compile
      run: cargo build --release

A step may narrow it further with its own `cache:`, layered over the workflow's
FIELD BY FIELD, so it declares only what differs:

  steps:
    - name: build
      cache:
        env: [PROFILE]     # keeps the workflow's inputs, outputs and exclude

`ciabatta cache init [WORKFLOW]` writes that section for you, from what is
actually in the directory. With one workflow in the package the name is
optional; with several you name the one you mean.

The shared cache SERVER is the exception and stays in `ciabatta.yaml`: it is one
endpoint per checkout, not a property of any one build.

  # .ciabatta/ciabatta.yaml
  cache:
    remote:
      url: http://cache.example.com:8380

A list a target does declare replaces the inherited one whole — half-merged
input globs would be very hard to reason about. `enabled` is only ever decided
by a level that says it explicitly, so declaring a dependency can neither turn
caching off nor turn it on; a single target opts out with `enabled: false`.

An `inputs`/`outputs` entry naming a DIRECTORY means everything under it, at any
depth: `inputs: [src]`, `inputs: ["src/"]` and `inputs: ["src/**/*"]` are the
same declaration, and `inputs: [.]` is the whole workspace.

Sub-workspaces are excluded from a super-workspace's inputs automatically: any
directory below it with its own `.ciabatta/` owns its files, and its own cache
entry covers them. Without that, a root whose inputs are `packages/**/*` would
never hit — every package's change would be its change too.

  ciabatta why <target>        where a target is declared, what it depends on,
                               and what the cache would do with it
  ciabatta why <target> --all  ...naming every input and output file, in the
                               order they are hashed into the key

━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
THE REMOTE CACHE
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

A small server anyone can stand up, so a team's builds stop repeating each
other's work. It stores artifacts on its own filesystem, in the same layout the
local cache uses.

  ciabatta remote-cache init                 write a server config
  ciabatta remote-cache start                run it
  ciabatta remote-cache login <URL>          connect this machine
  ciabatta remote-cache status               hits, misses, storage, retention
  ciabatta remote-cache add-user <name>      mint a token
  ciabatta cache init --remote <URL>         point a workspace at it

Its config (remote-cache.yaml, read by `remote-cache start`):

server:
  bind: 0.0.0.0            # a shared cache only loopback can reach is useless
  port: 8380
  storage: storage         # artifact store + project registry, relative to
                           # this file
  sweep_every: 1h          # how often retention runs, and binaries are rescanned

retention:                 # age is measured from LAST USE, not from when an
  max_age: 30d             # artifact was built — the thing everyone still
  max_size: 10GB           # depends on shouldn't be evicted for being old
  max_entries: 50000       # remove all three to keep everything forever

auth:
  mode: open               # open | token | ldap
  session_ttl: 30d

  users:                   # token mode
    - name: ci
      token_sha256: "…"    # from `remote-cache add-user`; the token itself is
      read_only: true      # shown once and never stored
    - name: root
      token_sha256: "…"
      admin: true          # may manage users. Only ever granted here, or by
                           # another admin — never by a request to the server.

  ldap:                    # ldap mode — bind against your directory over LDAPS
    url: ldaps://ldap.example.com:636
    bind_dn: "uid={username},ou=people,dc=example,dc=com"   # a DN template…
    base_dn: "dc=example,dc=com"                            # …or search for it
    user_filter: "(uid={username})"
    search_dn: "cn=ciabatta,ou=services,dc=example,dc=com"  # service account
    search_password_env: CIABATTA_LDAP_PASSWORD
    required_group: "cn=engineering,ou=groups,dc=example,dc=com"
    group_attribute: memberOf
    write_groups: ["cn=ci,ou=groups,dc=example,dc=com"]     # others read-only
    tls_verify: true       # leave this on. LDAPS without verification is an
                           # encrypted channel to whoever answered.

releases:                  # the ciabatta builds this cache hands out
  version: "0.2.0"
  notes: "What changed"
  binaries:
    linux:   /srv/ciabatta/ciabatta-linux-x86_64
    windows: /srv/ciabatta/ciabatta-windows-x86_64.exe
    macos:   /srv/ciabatta/ciabatta-macos-aarch64

log:                       # one line as a request arrives, one as it leaves
  requests: true
  headers: true            # credential-bearing headers are logged <redacted>

`remote-cache start` logs at info by default — the request log is the reason to
run it in a terminal. Raise it with CIABATTA_LOG=ciabatta=debug.

Sessions live in `storage/sessions.json` and survive a restart, so a config edit
or a new binary doesn't sign the whole team out. Only the SHA-256 of each bearer
token is stored, exactly as for `users.json`: the file says who is signed in and
when their session lapses, never how to be them.

  The server hashes these and mentions the version in every reply, so a client
  on something older is told. `ciabatta self update` fetches the new build from
  the same server it already trusts for artifacts, checks it against the
  advertised SHA-256, and only then replaces the binary.

    ciabatta self update --check     is there one?
    ciabatta self update             install it

  The HASH decides, not the version string: rebuild and copy a new binary over
  the same path and your team still gets updated, because what's advertised is
  the content — which is also what the client verifies.

  Read access is a convenience; WRITE access is trust. Whoever can write to a
  cache decides what everyone else's build produces, which is why `read_only`
  exists on both a token user and an LDAP group.

  The server serves an admin page at its root — open http://<host>:8380/ in a
  browser. It shows the hit rate and what's stored, and it mints credentials:
  the token is displayed once, and only its SHA-256 is kept, so a lost one is
  reissued rather than recovered. Server-managed users live in
  <storage>/users.json alongside the artifacts; the `auth.users` in this file
  stay yours, and the page will neither shadow nor delete them.

  Who may mint one:
    token / ldap  an admin — a user with `admin: true`, granted in this config
                  or by an existing admin
    open          anyone who can reach it, because open mode already means "I
                  trust this network" and refusing would leave no way to mint
                  the first credential. But a user created on an OPEN server is
                  never an admin, or somebody could grant themselves lasting
                  control while the door was open and keep it after it was shut.

  So the migration from open to authenticated is: create the users you want on
  the page, add one `admin: true` user to `auth.users` below, set
  `auth.mode: token`, and restart.

━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
ENVIRONMENT
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

Three rules, in the order they apply:

  1. `.env` is the default. A workspace that says nothing gets `.env` from its
     own directory. Nobody should have to configure the conventional thing.

  2. `workspace.env_file` overrides it — and REPLACES it rather than adding to
     it, which is what "use this file instead" has to mean to be useful for
     keeping dev and prod settings apart. It accepts a list, applied in order.

  3. `workspace.env_default` is where a missing `.env` comes from. `.env` is
     gitignored, so a fresh checkout doesn't have one; the checked-in template
     does. Naming it means ciabatta GENERATES the `.env` rather than failing on
     a variable the developer has never heard of. It's only ever created when
     absent, so your edits survive.

And one requirement that follows from the third: a workspace whose workflows
declare REQUIRED_ENV must declare `env_default`. Not bureaucracy — it's what
makes rule 3 possible. A repo where the required variables are written down
somewhere reviewable is a repo a new person can build.

`ciabatta watch` sources the same files a run would and prints exactly what it
resolved before the command starts, so a watched dev server and a `dev` workflow
step can't quietly see different environments.

━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

system:
  ci: gitlab               # CI/CD system for auto-resolving build variables.
                           # Options: gitlab, github, jenkins, circleci,
                           #          travis, azure, bitbucket
  containers: docker       # Container runtime. Options: docker, podman.
                           # When unset, ciabatta auto-detects what's installed:
                           # it prefers podman, falls back to docker, and asks
                           # you to choose if BOTH are present.

━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

analyze:                   # Optional inputs for `ciabatta analyze`
  requirements: reqs.txt   # File of requirements: `id` or `id, description`
  trace: trace.csv         # CSV of `requirement,file` connections
                           # (paths are relative to the project root;
                           #  --requirements / --trace override these)

━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

ai:                        # Settings for `ciabatta ai` (run `ciabatta ai setup`)
  provider: claude         # claude | openai | vllm (openai & vllm both speak
                           # the OpenAI format; vllm defaults to localhost:8000)
  endpoint: https://api.anthropic.com     # or e.g. http://localhost:8000
  model: claude-opus-4-8
  api_key_env: ANTHROPIC_API_KEY   # env var holding the API key
  tls_verify: true         # false to skip cert checks for a self-signed
                           # vLLM/OpenAI dev endpoint
  images: ["python:3.12", "node:22"]      # sandbox base images the AI may spin
                           # up via podman/docker (system.containers)

  The assistant's learned state (architecture tags, the file→architecture
  mind map, and its 1-100 confidence score) lives in .ciabatta/ai/brain.json.
  Chat sessions are saved under .ciabatta/ai/conversations/ — resume the most
  recent with `ciabatta ai -c`, or list/pick one with `ciabatta ai resume`.
  Hand off background work with `ciabatta ai ship "<task>"` (or `--todo <id>`);
  track it with `ciabatta ai jobs`, saved in .ciabatta/ai/jobs.json. The AI may
  only read and write the project workspace and /tmp — nothing else.

━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

registries:
  <name>:
    url: https://...              # Base URL of the registry (required)
    tls_verify: true              # Verify TLS certificate (default: true)
    needs_auth: true              # Whether auth is needed (informational)
    login_script: ./login.sh      # Optional: run this before push/pull
    type: nexus                   # Override type detection. Options:
                                  # nexus, s3, artifactory, docker, ecr

    # Nexus-only fields (select the target repository and format):
    repository: raw-hosted        # Nexus repo name. When set, `url` is the bare
                                  # Nexus host and /repository/<repository> is
                                  # appended automatically. When unset, `url` is
                                  # used as the full repository URL.
    base_path: builds             # raw only: prefix prepended to every step's
                                  # publish_path (where raw files land)
    format: raw                   # Nexus repository format. Options:
                                  #   raw  → HTTP PUT/GET      (default)
                                  #   npm  → `npm publish`
                                  #   pypi → `twine upload`

  Example — publish an npm package to a Nexus npm repo:

    registries:
      npm:
        type: nexus
        url: http://localhost:8527
        repository: npm-hosted
        format: npm

  Auth for all formats uses CIABATTA_<NAME>_USER / _PASS (npm also accepts a
  CIABATTA_<NAME>_TOKEN bearer token). npm requires `npm` on PATH; pypi requires
  `twine`. For npm/pypi, a push step's `artifact` is the package tarball or the
  `dist/` directory to publish; `publish_path` is not used.

  The `url` and `login_script` fields expand environment variables, with
  bash-style defaults, so one config can target different environments:

    url: "https://${NEXUS_HOST:-nexus.example.com}/repository/releases/"

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

Publishing — a step that moves an artifact (`kind: push` / `kind: pull`)

  Publishing is not a separate schema or a separate command. A step declares
  `kind: push`, says what moves and where, and sits on the graph like any other
  node: it declares what it `needs`, it appears in the dependency report, and it
  is cached on the same terms as everything else.

    # packages/api/.ciabatta/workflows/release.yaml
    description: Build and publish the API binary

    steps:
      - name: build
        run: cargo build --release
        requires: [cargo]

      - name: publish
        kind: push
        needs: [build]
        registry: nexus                 # a name from `registries:`
        artifact: target/release/api    # the local file or directory to send
        publish_path: "api/{CIABATTA_BRANCH}/{CIABATTA_COMMIT}/api"

  Fields on a transfer step:

    registry       which `registries:` entry to move through
    artifact       the local file or directory (a directory uploads file by
                   file, recreating its structure under the publish path)
    publish_path   the destination, with {CIABATTA_*} substitution — or a list
                   of local globs, each uploaded under {CIABATTA_PATH}
    strip_prefix   for the glob-list form: a leading fragment to remove from
                   each matched file before joining it under {CIABATTA_PATH}
    local_image    docker/ecr only: a locally-built image to retag and push,
                   so the registry URL is never baked into your build. Then
                   `publish_path` is the remote image reference; omit it and
                   the local reference is reused verbatim.

  Pull is the same artifact in the other direction. Rather than restate it,
  name the push step it mirrors — `<workflow>:<step>`, or a bare `<step>` in
  the same workflow:

    - name: fetch
      kind: pull
      from: release:publish     # same registry and path, opposite direction

  `from` is a default, not an override: anything the pull step sets for itself
  wins over what it inherits, so a pull that differs in one field says only
  that field.

    - name: vendor
      kind: pull
      from: release:publish
      artifact: vendor/api      # same source, different destination

  A transfer step may also just run a command. Give it `run:` or `script:` and
  that runs instead of the built-in move — which is how a project keeps its own
  publishing script while still being a node on the graph.

━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

Steps — the DAG every workflow is

  Each step runs a `script` (a bash file), an inline `run` command, or a
  built-in transfer (above). It may declare `needs` (steps that must succeed
  first) and `on_error` (jump to a recovery node on failure). Steps with
  satisfied `needs` are eligible to run; the graph must be acyclic.

  Already have a script? `ciabatta convert --script scripts/build.sh` reads it,
  works out the tools it calls, the variables it reads, the files it writes and
  the description in its header comment, and writes the workflow for you.

  A step may be skipped by condition (evaluated against the run's env):
    when: env.RUN_ENV == prod        # run ONLY if all conditions hold
    skip_if: env.IN_CI == true       # skip if ANY condition holds
  Each takes one condition or a list (multiple criteria). Conditions are
  `VAR == value`, `VAR != value`, bare `VAR` (truthy), or `!VAR`; the `env.`
  prefix is optional. A skipped step counts as satisfied, so its dependents
  still run.

    # packages/api/.ciabatta/workflows/deploy.yaml
    steps:
      - name: build
        script: scripts/build.sh

      - name: migrate
        script: scripts/migrate.sh
        needs: [build]
        on_error: fix_migrate       # on failure, go to the recovery node

      - name: fix_migrate           # a recovery node: offers fix choices
        recover: true
        message: "Migration failed — choose how to recover:"
        retry: migrate              # re-run this step after a successful fix
        options:
          - label: Roll back
            script: scripts/rollback.sh
          - label: Force unlock
            run: make unlock
            default: true

      - name: release
        script: scripts/release.sh
        needs: [migrate]
        when: env.RUN_ENV == prod   # only release in prod

  Recovery: when a step with `on_error` fails, its recovery node offers a choice
  of fix `options`. With `--gui` you pick one in the browser; otherwise (plain /
  CI) the option marked `default: true` runs automatically, or the run fails
  if none is. After a fix succeeds, `retry` re-runs the named step. Retry loops
  are bounded so a persistently failing step can't spin forever.

    ciabatta deploy                  run it across every package that defines it
    ciabatta deploy --gui            live web view: graph, logs, fix-it buttons
    ciabatta deploy --filter kind:push    just the publishing steps
    ciabatta dry-run deploy          what would be reused, and why not

━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

Selecting part of a graph — `--filter`

  `--filter` prunes a compiled graph instead of expanding a selection, so what
  survives is a real subgraph rather than a list of names to run.

    ciabatta release --filter kind:push        # only the transfer steps
    ciabatta release --filter workspace:api    # only one package's steps
    ciabatta release --filter tag:fast
    ciabatta release --filter tag:fast --filter '!tag:flaky'

  Terms are tag:, workspace:, kind:, owner:, step:, or a bare word searching
  all of them; prefix with ! to exclude. Repeatable. The surviving steps assume
  their pruned dependencies already ran — it's the fast debug loop, not a first
  build.

━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

Ordering — there are no stages, only edges

  Publishing used to run through a fixed `login → pre → main → post` pipeline
  with per-direction overrides. It doesn't any more: what those stages were for
  is what a graph already does better.

    a `pre` command    → a step, and a `needs` edge to it
    a `post` command   → a step that `needs` the transfer
    a `login` override → the registry's own `login_script`
    a `main` override  → a `run:` on the transfer step itself

  The difference is that each of those is now a node you can see on the graph,
  filter, cache, and fail independently — rather than an opaque hook attached
  to one artifact.

    steps:
      - name: bundle
        run: python scripts/bundle.py

      - name: publish
        kind: push
        needs: [bundle]
        registry: nexus
        artifact: dist/

      - name: notify
        run: ./scripts/notify.sh deployed
        needs: [publish]

━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
THE MONOREPO SCHEMA
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

A monorepo accumulates scripts nobody owns, publishing to places nobody
remembers, quietly depending on each other in ways nobody wrote down.
Ciabatta's answer: every package opts in with `ciabatta init --lib`, and
declares three things — who owns it, what its workflows do, and which other
packages they need.

Any workflow name then becomes a command:

  ciabatta build              # every `build` workflow in the repo, in order
  ciabatta build --graph      # show the graph, run nothing
  ciabatta build --dry-run    # walk every step, execute nothing
  ciabatta build --only api   # start from one package (deps still come along)
  ciabatta dry-run build      # what would be reused from the cache, and why not
  ciabatta list               # every workflow, its owner, and what it does
  ciabatta list -s proto      # ...filtered

The monorepo root is your git root; every directory beneath it with a
.ciabatta/ciabatta.yaml is a sub-workspace.

workspace:                   # this package's identity (ciabatta init --lib)
  name: api                    # what other packages refer to it by
                               # (defaults to the directory name)
  description: REST API        # shown by `ciabatta list`
  owner: Ada                   # who to ask about it
  depends_on: [proto:generate, common]
                               # other sub-workspaces this one needs, applied
                               # to EVERY workflow here. "common" means their
                               # workflow of the same name (skipped if they
                               # have none); "proto:generate" names one exactly.
  tags: [backend]              # free-form labels, searchable with `list -s`
  requires: [cargo]            # tools every workflow here needs on PATH
  env_file: .env               # sourced before any workflow here runs.
                               # Unset means `.env`; setting it REPLACES that
                               # default rather than adding to it.
  env_default: .env.default    # the checked-in template `.env` is generated
                               # from. REQUIRED of any package whose workflows
                               # declare REQUIRED_ENV — see ENVIRONMENT below.
  umbrella: true               # on the ROOT config only: "I'm not a package,
                               # just shared toolchain and settings"

  env:                         # standard variables for every step defined here
    RUST_LOG: info

toolchain:                   # how to install what workflows `require`
  protoc:
    hint: brew install protobuf     # printed when the tool is missing
    check: protoc --version         # optional: a smarter test than PATH
    description: Protocol buffer compiler

  Usually written once at the monorepo root and inherited by every package.
  Missing tools are reported together, BEFORE the first step runs, with these
  hints attached — not as "command not found" ten minutes into a build.

━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
WORKFLOWS
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

One file per workflow: .ciabatta/workflows/<name>.yaml — the filename IS the
workflow name. Small packages can write them inline under `workflows:` instead
(but not both: a name defined twice is an error).

  description: Compile the service binary   # what running this achieves
  owner: Ada                                # falls back to the package's
  needs: [proto:generate]                   # cross-package deps, on top of
                                            # workspace.depends_on
  requires: [cargo]                         # tools all its steps need
  env_file: .env.build                      # relative to this package
  REQUIRED_ENV: [API_TOKEN]                 # refuse to start unless set
  tags: [rust]

  env:                                      # vars for all its steps
    PROFILE: release

  steps:
    - name: compile
      description: Build the release binary # what it does...
      owner: Ada                            # ...and who to ask
      run: cargo build --release            # an inline shell command
      script: scripts/build.sh              # ...or a script, run from THIS
                                            # package's directory
      needs: [fetch]                        # ordering WITHIN this workflow
                                            # (cross-package deps go on the
                                            #  workflow, not the step)
      requires: [cargo, protoc]             # tools this step needs
      timeout: 10m                          # "30s" "10m" "1h30m", or seconds.
                                            # Past it the step is killed (with
                                            # everything it spawned) and marked
                                            # failed — and the graph keeps going.
      retries: 2                            # extra attempts, for flaky steps
      persistent: true                      # a dev server that never exits:
                                            # started, dependents released
                                            # immediately, and handed to the
                                            # daemon as a watch session, so it
                                            # OUTLIVES the run. The run prints
                                            # its id:
                                            #   ciabatta watch --attach <ID>
                                            #   ciabatta watch --stop   <ID>
                                            #   ciabatta watch --list
      continue_on_error: true               # its failure skips dependents but
                                            # doesn't stop the run
      kind: push                            # push | pull select the built-in
                                            # registry transfer; setup | build |
                                            # test | deploy | anything else is
                                            # a free-form label on the node
      registry: nexus                       # with kind: push/pull — see
      artifact: dist/app                    # "Publishing" below for the whole
      publish_path: "app/{CIABATTA_COMMIT}" # set of transfer fields
      from: release:publish                 # a pull mirroring a push step
      when: RUN_ENV == prod                 # conditions (see Runs, below)
      skip_if: IN_CI
      on_error: fix                         # route failures to a recovery node,
                                            # same as a run flowchart

      env:
        CARGO_TERM_COLOR: always

      cache:                                # per-step cache override; most
        inputs: ["proto/**/*"]              # steps inherit the workspace's

Cross-package wiring, in full:

  Every step of workflow A with no `needs` of its own waits for every terminal
  step of each workflow A depends on. So `api`'s build declaring
  depends_on: [proto:generate] means api's first step doesn't start until
  proto's last one has finished — and `ciabatta build --graph` shows you exactly
  that, node by node, labelled with the package each came from.

━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
CACHING
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

Off until a workspace opts in. A cache that turns itself on is a cache that
will one day serve somebody a stale artifact they never asked to be kept.

  ciabatta cache init          look at the directory, propose inputs and outputs
  ciabatta dry-run <target>    what would be reused, and why not — runs nothing
  ciabatta dry-run <t> --diff  ...with the lines that changed
  ciabatta cache status        what the local store is holding
  ciabatta cache prune --max-age 30d
  ciabatta cache clean

cache:
  enabled: true
  inputs:  ["src/**/*", "Cargo.toml"]   # what the build READS
  outputs: ["target/release/app"]       # what the build WRITES
  exclude: [target]                     # never counted as an input, so a build
                                        # can't invalidate itself with its own
                                        # output. Does NOT filter `outputs`.
  env: [PROFILE]                        # variables the RESULT depends on

  remote:                               # the shared cache (project-level: put
    url: http://cache.example.com:8380  # this in the MONOREPO ROOT's config)
    project: 7f3a-…                     # assigned by the server on first
                                        # contact and written back here. COMMIT
                                        # IT: it's what makes every checkout and
                                        # every CI runner resolve to the same
                                        # project rather than registering a new
                                        # one under the same name.
    read_only: true                     # read the cache, never write to it —
                                        # what a fork's CI should get
    tls_verify: true                    # verify the server's certificate.
                                        # Turn it off for a self-signed or
                                        # internal CA cert this machine doesn't
                                        # have — but with it off, HTTPS is an
                                        # encrypted channel to whoever answered,
                                        # so the artifacts are only as
                                        # trustworthy as the network.
    enabled: true                       # turn it off without deleting settings

A stage has exactly THREE dependencies, and any of them changing is a rebuild:

  1. its input files,
  2. the environment variables it declared in `cache.env`,
  3. the outputs of the stages it `needs`.

The third is what makes a graph cacheable rather than just a directory: change
a .proto file and `proto:generate` misses, its outputs change, and every stage
downstream of it misses too — each for a reason it can name.

Two things worth knowing:

  * An undeclared input is a WRONG ANSWER, not a slow one. If a build reads a
    file that isn't in `inputs`, changing that file won't change the key and the
    cache will confidently hand back the wrong artifact. That's why `cache init`
    scaffolds `inputs` from the directory's real contents, and why `dry-run`
    exists.

  * Outputs are verified, not assumed. A key match says the inputs didn't
    change; it says nothing about whether somebody deleted `dist/` or edited a
    generated file by hand. So the outputs are hashed too, and a mismatch is a
    restore or a rebuild.

Cache settings live WITH THE WORKFLOW that reads those files —
`.ciabatta/workflows/<name>.yaml`, next to the steps — because what a build
reads is a property of that build. A `build` and a `test` in one package read
different files, and one section per config gave them no way to say so.

  # packages/api/.ciabatta/workflows/build.yaml
  cache:
    enabled: true
    inputs:  ["src/**/*"]
    outputs: ["target/release/app"]

  steps:
    - name: compile
      run: cargo build --release

A step may narrow it further with its own `cache:`, layered over the workflow's
FIELD BY FIELD, so it declares only what differs:

  steps:
    - name: build
      cache:
        env: [PROFILE]     # keeps the workflow's inputs, outputs and exclude

`ciabatta cache init [WORKFLOW]` writes that section for you, from what is
actually in the directory. With one workflow in the package the name is
optional; with several you name the one you mean.

The shared cache SERVER is the exception and stays in `ciabatta.yaml`: it is one
endpoint per checkout, not a property of any one build.

  # .ciabatta/ciabatta.yaml
  cache:
    remote:
      url: http://cache.example.com:8380

A list a target does declare replaces the inherited one whole — half-merged
input globs would be very hard to reason about. `enabled` is only ever decided
by a level that says it explicitly, so declaring a dependency can neither turn
caching off nor turn it on; a single target opts out with `enabled: false`.

An `inputs`/`outputs` entry naming a DIRECTORY means everything under it, at any
depth: `inputs: [src]`, `inputs: ["src/"]` and `inputs: ["src/**/*"]` are the
same declaration, and `inputs: [.]` is the whole workspace.

Sub-workspaces are excluded from a super-workspace's inputs automatically: any
directory below it with its own `.ciabatta/` owns its files, and its own cache
entry covers them. Without that, a root whose inputs are `packages/**/*` would
never hit — every package's change would be its change too.

  ciabatta why <target>        where a target is declared, what it depends on,
                               and what the cache would do with it
  ciabatta why <target> --all  ...naming every input and output file, in the
                               order they are hashed into the key

━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
THE REMOTE CACHE
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

A small server anyone can stand up, so a team's builds stop repeating each
other's work. It stores artifacts on its own filesystem, in the same layout the
local cache uses.

  ciabatta remote-cache init                 write a server config
  ciabatta remote-cache start                run it
  ciabatta remote-cache login <URL>          connect this machine
  ciabatta remote-cache status               hits, misses, storage, retention
  ciabatta remote-cache add-user <name>      mint a token
  ciabatta cache init --remote <URL>         point a workspace at it

Its config (remote-cache.yaml, read by `remote-cache start`):

server:
  bind: 0.0.0.0            # a shared cache only loopback can reach is useless
  port: 8380
  storage: storage         # artifact store + project registry, relative to
                           # this file
  sweep_every: 1h          # how often retention runs, and binaries are rescanned

retention:                 # age is measured from LAST USE, not from when an
  max_age: 30d             # artifact was built — the thing everyone still
  max_size: 10GB           # depends on shouldn't be evicted for being old
  max_entries: 50000       # remove all three to keep everything forever

auth:
  mode: open               # open | token | ldap
  session_ttl: 30d

  users:                   # token mode
    - name: ci
      token_sha256: "…"    # from `remote-cache add-user`; the token itself is
      read_only: true      # shown once and never stored
    - name: root
      token_sha256: "…"
      admin: true          # may manage users. Only ever granted here, or by
                           # another admin — never by a request to the server.

  ldap:                    # ldap mode — bind against your directory over LDAPS
    url: ldaps://ldap.example.com:636
    bind_dn: "uid={username},ou=people,dc=example,dc=com"   # a DN template…
    base_dn: "dc=example,dc=com"                            # …or search for it
    user_filter: "(uid={username})"
    search_dn: "cn=ciabatta,ou=services,dc=example,dc=com"  # service account
    search_password_env: CIABATTA_LDAP_PASSWORD
    required_group: "cn=engineering,ou=groups,dc=example,dc=com"
    group_attribute: memberOf
    write_groups: ["cn=ci,ou=groups,dc=example,dc=com"]     # others read-only
    tls_verify: true       # leave this on. LDAPS without verification is an
                           # encrypted channel to whoever answered.

releases:                  # the ciabatta builds this cache hands out
  version: "0.2.0"
  notes: "What changed"
  binaries:
    linux:   /srv/ciabatta/ciabatta-linux-x86_64
    windows: /srv/ciabatta/ciabatta-windows-x86_64.exe
    macos:   /srv/ciabatta/ciabatta-macos-aarch64

log:                       # one line as a request arrives, one as it leaves
  requests: true
  headers: true            # credential-bearing headers are logged <redacted>

`remote-cache start` logs at info by default — the request log is the reason to
run it in a terminal. Raise it with CIABATTA_LOG=ciabatta=debug.

Sessions live in `storage/sessions.json` and survive a restart, so a config edit
or a new binary doesn't sign the whole team out. Only the SHA-256 of each bearer
token is stored, exactly as for `users.json`: the file says who is signed in and
when their session lapses, never how to be them.

  The server hashes these and mentions the version in every reply, so a client
  on something older is told. `ciabatta self update` fetches the new build from
  the same server it already trusts for artifacts, checks it against the
  advertised SHA-256, and only then replaces the binary.

    ciabatta self update --check     is there one?
    ciabatta self update             install it

  The HASH decides, not the version string: rebuild and copy a new binary over
  the same path and your team still gets updated, because what's advertised is
  the content — which is also what the client verifies.

  Read access is a convenience; WRITE access is trust. Whoever can write to a
  cache decides what everyone else's build produces, which is why `read_only`
  exists on both a token user and an LDAP group.

  The server serves an admin page at its root — open http://<host>:8380/ in a
  browser. It shows the hit rate and what's stored, and it mints credentials:
  the token is displayed once, and only its SHA-256 is kept, so a lost one is
  reissued rather than recovered. Server-managed users live in
  <storage>/users.json alongside the artifacts; the `auth.users` in this file
  stay yours, and the page will neither shadow nor delete them.

  Who may mint one:
    token / ldap  an admin — a user with `admin: true`, granted in this config
                  or by an existing admin
    open          anyone who can reach it, because open mode already means "I
                  trust this network" and refusing would leave no way to mint
                  the first credential. But a user created on an OPEN server is
                  never an admin, or somebody could grant themselves lasting
                  control while the door was open and keep it after it was shut.

  So the migration from open to authenticated is: create the users you want on
  the page, add one `admin: true` user to `auth.users` below, set
  `auth.mode: token`, and restart.

━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
ENVIRONMENT
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

Three rules, in the order they apply:

  1. `.env` is the default. A workspace that says nothing gets `.env` from its
     own directory. Nobody should have to configure the conventional thing.

  2. `workspace.env_file` overrides it — and REPLACES it rather than adding to
     it, which is what "use this file instead" has to mean to be useful for
     keeping dev and prod settings apart. It accepts a list, applied in order.

  3. `workspace.env_default` is where a missing `.env` comes from. `.env` is
     gitignored, so a fresh checkout doesn't have one; the checked-in template
     does. Naming it means ciabatta GENERATES the `.env` rather than failing on
     a variable the developer has never heard of. It's only ever created when
     absent, so your edits survive.

And one requirement that follows from the third: a workspace whose workflows
declare REQUIRED_ENV must declare `env_default`. Not bureaucracy — it's what
makes rule 3 possible. A repo where the required variables are written down
somewhere reviewable is a repo a new person can build.

`ciabatta watch` sources the same files a run would and prints exactly what it
resolved before the command starts, so a watched dev server and a `dev` workflow
step can't quietly see different environments.

━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

system:
  ci: gitlab               # CI/CD system for auto-resolving build variables.
                           # Options: gitlab, github, jenkins, circleci,
                           #          travis, azure, bitbucket
  containers: docker       # Container runtime. Options: docker, podman.
                           # When unset, ciabatta auto-detects what's installed:
                           # it prefers podman, falls back to docker, and asks
                           # you to choose if BOTH are present.

━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

analyze:                   # Optional inputs for `ciabatta analyze`
  requirements: reqs.txt   # File of requirements: `id` or `id, description`
  trace: trace.csv         # CSV of `requirement,file` connections
                           # (paths are relative to the project root;
                           #  --requirements / --trace override these)

━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

ai:                        # Settings for `ciabatta ai` (run `ciabatta ai setup`)
  provider: claude         # claude | openai | vllm (openai & vllm both speak
                           # the OpenAI format; vllm defaults to localhost:8000)
  endpoint: https://api.anthropic.com     # or e.g. http://localhost:8000
  model: claude-opus-4-8
  api_key_env: ANTHROPIC_API_KEY   # env var holding the API key
  tls_verify: true         # false to skip cert checks for a self-signed
                           # vLLM/OpenAI dev endpoint
  images: ["python:3.12", "node:22"]      # sandbox base images the AI may spin
                           # up via podman/docker (system.containers)

  The assistant's learned state (architecture tags, the file→architecture
  mind map, and its 1-100 confidence score) lives in .ciabatta/ai/brain.json.
  Chat sessions are saved under .ciabatta/ai/conversations/ — resume the most
  recent with `ciabatta ai -c`, or list/pick one with `ciabatta ai resume`.
  Hand off background work with `ciabatta ai ship "<task>"` (or `--todo <id>`);
  track it with `ciabatta ai jobs`, saved in .ciabatta/ai/jobs.json. The AI may
  only read and write the project workspace and /tmp — nothing else.

━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

registries:
  <name>:
    url: https://...              # Base URL of the registry (required)
    tls_verify: true              # Verify TLS certificate (default: true)
    needs_auth: true              # Whether auth is needed (informational)
    login_script: ./login.sh      # Optional: run this before push/pull
    type: nexus                   # Override type detection. Options:
                                  # nexus, s3, artifactory, docker, ecr

    # Nexus-only fields (select the target repository and format):
    repository: raw-hosted        # Nexus repo name. When set, `url` is the bare
                                  # Nexus host and /repository/<repository> is
                                  # appended automatically. When unset, `url` is
                                  # used as the full repository URL.
    base_path: builds             # raw only: prefix prepended to every step's
                                  # publish_path (where raw files land)
    format: raw                   # Nexus repository format. Options:
                                  #   raw  → HTTP PUT/GET      (default)
                                  #   npm  → `npm publish`
                                  #   pypi → `twine upload`

  Example — publish an npm package to a Nexus npm repo:

    registries:
      npm:
        type: nexus
        url: http://localhost:8527
        repository: npm-hosted
        format: npm

  Auth for all formats uses CIABATTA_<NAME>_USER / _PASS (npm also accepts a
  CIABATTA_<NAME>_TOKEN bearer token). npm requires `npm` on PATH; pypi requires
  `twine`. For npm/pypi, a push step's `artifact` is the package tarball or the
  `dist/` directory to publish; `publish_path` is not used.

  The `url` and `login_script` fields expand environment variables, with
  bash-style defaults, so one config can target different environments:

    url: "https://${NEXUS_HOST:-nexus.example.com}/repository/releases/"

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

Publishing — a step that moves an artifact (`kind: push` / `kind: pull`)

  Publishing is not a separate schema or a separate command. A step declares
  `kind: push`, says what moves and where, and sits on the graph like any other
  node: it declares what it `needs`, it appears in the dependency report, and it
  is cached on the same terms as everything else.

    # packages/api/.ciabatta/workflows/release.yaml
    description: Build and publish the API binary

    steps:
      - name: build
        run: cargo build --release
        requires: [cargo]

      - name: publish
        kind: push
        needs: [build]
        registry: nexus                 # a name from `registries:`
        artifact: target/release/api    # the local file or directory to send
        publish_path: "api/{CIABATTA_BRANCH}/{CIABATTA_COMMIT}/api"

  Fields on a transfer step:

    registry       which `registries:` entry to move through
    artifact       the local file or directory (a directory uploads file by
                   file, recreating its structure under the publish path)
    publish_path   the destination, with {CIABATTA_*} substitution — or a list
                   of local globs, each uploaded under {CIABATTA_PATH}
    strip_prefix   for the glob-list form: a leading fragment to remove from
                   each matched file before joining it under {CIABATTA_PATH}
    local_image    docker/ecr only: a locally-built image to retag and push,
                   so the registry URL is never baked into your build. Then
                   `publish_path` is the remote image reference; omit it and
                   the local reference is reused verbatim.

  Pull is the same artifact in the other direction. Rather than restate it,
  name the push step it mirrors — `<workflow>:<step>`, or a bare `<step>` in
  the same workflow:

    - name: fetch
      kind: pull
      from: release:publish     # same registry and path, opposite direction

  `from` is a default, not an override: anything the pull step sets for itself
  wins over what it inherits, so a pull that differs in one field says only
  that field.

    - name: vendor
      kind: pull
      from: release:publish
      artifact: vendor/api      # same source, different destination

  A transfer step may also just run a command. Give it `run:` or `script:` and
  that runs instead of the built-in move — which is how a project keeps its own
  publishing script while still being a node on the graph.

━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

Steps — the DAG every workflow is

  Each step runs a `script` (a bash file), an inline `run` command, or a
  built-in transfer (above). It may declare `needs` (steps that must succeed
  first) and `on_error` (jump to a recovery node on failure). Steps with
  satisfied `needs` are eligible to run; the graph must be acyclic.

  Already have a script? `ciabatta convert --script scripts/build.sh` reads it,
  works out the tools it calls, the variables it reads, the files it writes and
  the description in its header comment, and writes the workflow for you.

  A step may be skipped by condition (evaluated against the run's env):
    when: env.RUN_ENV == prod        # run ONLY if all conditions hold
    skip_if: env.IN_CI == true       # skip if ANY condition holds
  Each takes one condition or a list (multiple criteria). Conditions are
  `VAR == value`, `VAR != value`, bare `VAR` (truthy), or `!VAR`; the `env.`
  prefix is optional. A skipped step counts as satisfied, so its dependents
  still run.

    # packages/api/.ciabatta/workflows/deploy.yaml
    steps:
      - name: build
        script: scripts/build.sh

      - name: migrate
        script: scripts/migrate.sh
        needs: [build]
        on_error: fix_migrate       # on failure, go to the recovery node

      - name: fix_migrate           # a recovery node: offers fix choices
        recover: true
        message: "Migration failed — choose how to recover:"
        retry: migrate              # re-run this step after a successful fix
        options:
          - label: Roll back
            script: scripts/rollback.sh
          - label: Force unlock
            run: make unlock
            default: true

      - name: release
        script: scripts/release.sh
        needs: [migrate]
        when: env.RUN_ENV == prod   # only release in prod

  Recovery: when a step with `on_error` fails, its recovery node offers a choice
  of fix `options`. With `--gui` you pick one in the browser; otherwise (plain /
  CI) the option marked `default: true` runs automatically, or the run fails
  if none is. After a fix succeeds, `retry` re-runs the named step. Retry loops
  are bounded so a persistently failing step can't spin forever.

    ciabatta deploy                  run it across every package that defines it
    ciabatta deploy --gui            live web view: graph, logs, fix-it buttons
    ciabatta deploy --filter kind:push    just the publishing steps
    ciabatta dry-run deploy          what would be reused, and why not

━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

Selecting part of a graph — `--filter`

  `--filter` prunes a compiled graph instead of expanding a selection, so what
  survives is a real subgraph rather than a list of names to run.

    ciabatta release --filter kind:push        # only the transfer steps
    ciabatta release --filter workspace:api    # only one package's steps
    ciabatta release --filter tag:fast
    ciabatta release --filter tag:fast --filter '!tag:flaky'

  Terms are tag:, workspace:, kind:, owner:, step:, or a bare word searching
  all of them; prefix with ! to exclude. Repeatable. The surviving steps assume
  their pruned dependencies already ran — it's the fast debug loop, not a first
  build.

━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

Ordering — there are no stages, only edges

  Publishing used to run through a fixed `login → pre → main → post` pipeline.
  It doesn't any more: what those stages were for is what a graph already does,
  and each piece is now a node you can see, filter, cache and fail on its own.

    a `pre` command    → a step, and a `needs` edge to it
    a `post` command   → a step that `needs` the transfer
    a `login` override → the registry's own `login_script`
    a `main` override  → a `run:` on the transfer step itself

  Step commands run via `sh -c` from the step's own directory, with every
  CIABATTA_* and CI variable in their environment (use $CIABATTA_COMMIT, etc.).

    steps:
      - name: bundle
        run: python scripts/bundle.py
      - name: publish
        kind: push
        needs: [bundle]
        registry: nexus
        artifact: dist/
      - name: notify
        run: ./scripts/notify.sh deployed
        needs: [publish]

━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

Credentials (when a registry has no login_script)

  ciabatta reads per-registry credentials from the environment:
    CIABATTA_<REGISTRY>_USER   CIABATTA_<REGISTRY>_PASS
  where <REGISTRY> is the registry's key, uppercased. For example,
  `registries.nexus` → CIABATTA_NEXUS_USER / CIABATTA_NEXUS_PASS.

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
                             /{CIABATTA_TAG}                       (when a tag is set)
                             /{CIABATTA_BRANCH}/{CIABATTA_COMMIT}  (otherwise)

These are populated automatically from the CI system defined in `system`.
You can override any of them with: ciabatta <workflow> -e CIABATTA_BRANCH=my-branch

Note that a `{VAR}` placeholder must be QUOTED in YAML — a bare `{` starts a
flow mapping. `publish_path: "app/{CIABATTA_COMMIT}"`, not `publish_path:
app/{CIABATTA_COMMIT}`.

Working locally? `ciabatta <workflow> --local` (or `export CIABATTA_ENV=local`)
derives CIABATTA_BRANCH / _COMMIT / _TAG / _BUILD_NUMBER from your local git
history instead of CI. On any `kind: pull` step (local or CI), when the exact
commit has no published artifact ciabatta falls back to the newest commit on
the branch that does. Run `ciabatta source` to print the variables as shell `export` lines:

    eval "$(ciabatta source)"

━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

publish_path: a single remote path, or a list of local file globs

  # Single remote destination (supports {VAR} substitution):
  publish_path: "team/app/{CIABATTA_COMMIT}/app.tar.gz"

  # A list of local globs: each matched file uploads under {CIABATTA_PATH},
  # preserving its path relative to the project root. `strip_prefix` trims a
  # leading fragment from that relative path first.
  publish_path: ["dist/*.tar.gz", "build/*.bin"]
  strip_prefix: "dist/"        # dist/app.tar.gz -> {CIABATTA_PATH}/app.tar.gz

━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

Example:

  system:
    ci: github
    containers: docker

  registries:
    nexus:
      url: https://nexus.example.com/repository/releases/
      tls_verify: true
      needs_auth: true
      login_script: .ciabatta/nexus_login.sh

  workflows:
    release:
      description: Build and publish the frontend bundle
      steps:
        - name: build
          run: npm run build

        - name: publish
          kind: push
          needs: [build]
          registry: nexus
          artifact: frontend/dist
          publish_path: "frontend/{CIABATTA_BRANCH}/{CIABATTA_COMMIT}/dist.tar.gz"

  cache:
    enabled: true
    inputs:  ["frontend/src/**/*", "frontend/package.json"]
    outputs: ["frontend/dist/**/*"]
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
    port: Option<u16>,
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
        // The graph was just written where the daemon reads it from, so the
        // page has fresh data the moment it loads — no second scan needed.
        let session = daemon::connect(port).await?;
        let url = session.page_url("/analyze");
        println!("\nAnalyze view: {url}");
        daemon::open_browser(&url);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        build_lib_config, build_starter_config, build_starter_workflow, root_from_config_path,
        yaml_scalar,
    };
    use std::path::{Path, PathBuf};

    /// A scaffold that doesn't parse is worse than no scaffold: the first thing
    /// the user does is run a command against it.
    #[test]
    fn every_scaffold_ciabatta_writes_is_valid_yaml() {
        let config = build_starter_config(Some("github"), Some("podman"));
        let parsed: crate::config::CiabattaConfig =
            crate::format::from_str(&config, crate::format::Format::Yaml)
                .expect("the starter config parses");
        assert_eq!(
            parsed.system.as_ref().and_then(|s| s.ci.as_deref()),
            Some("github")
        );
        assert_eq!(
            parsed.system.as_ref().and_then(|s| s.containers.as_deref()),
            Some("podman")
        );

        // The un-pinned form leaves both commented out, so it must still parse
        // and must not invent values.
        let bare: crate::config::CiabattaConfig = crate::format::from_str(
            &build_starter_config(None, None),
            crate::format::Format::Yaml,
        )
        .expect("the un-pinned starter config parses");
        assert!(bare.registries.is_empty() && bare.workflows.is_empty());

        let lib = build_lib_config(
            "api",
            Some("The public REST API"),
            Some("Ada"),
            &["proto:generate".to_string()],
        );
        let parsed: crate::config::CiabattaConfig =
            crate::format::from_str(&lib, crate::format::Format::Yaml)
                .expect("the --lib config parses");
        let meta = parsed.workspace.expect("workspace identity written");
        assert_eq!(meta.name.as_deref(), Some("api"));
        assert_eq!(meta.description.as_deref(), Some("The public REST API"));
        assert_eq!(meta.owner.as_deref(), Some("Ada"));
        assert_eq!(meta.depends_on, vec!["proto:generate".to_string()]);

        // The TODO form leaves empty strings behind for the user to fill in.
        let todo: crate::config::CiabattaConfig = crate::format::from_str(
            &build_lib_config("api", None, None, &[]),
            crate::format::Format::Yaml,
        )
        .expect("the un-filled --lib config parses");
        let meta = todo.workspace.expect("workspace identity written");
        assert_eq!(meta.description.as_deref(), Some(""));
        assert!(meta.depends_on.is_empty());

        let workflow: crate::workspace::Workflow = crate::format::from_str(
            &build_starter_workflow("build", "api", Some("Ada")),
            crate::format::Format::Yaml,
        )
        .expect("the starter workflow parses");
        assert_eq!(workflow.owner.as_deref(), Some("Ada"));
        assert_eq!(
            workflow.steps.len(),
            1,
            "only the live step, not the commented ones"
        );
        assert_eq!(workflow.steps[0].name, "build");
    }

    /// Names come from `git config user.name` and directory names, so they can
    /// hold anything. A value YAML would read as something else has to be quoted.
    #[test]
    fn scaffolded_values_survive_being_yaml_hostile() {
        for value in [
            "Ada",
            "O'Brien, Ada",
            "yes",
            "no",
            "true",
            "1.5",
            "#1 fan",
            "- dash",
        ] {
            let config = build_lib_config("pkg", Some(value), Some(value), &[]);
            let parsed: crate::config::CiabattaConfig =
                crate::format::from_str(&config, crate::format::Format::Yaml)
                    .unwrap_or_else(|e| panic!("{value:?} broke the scaffold: {e}"));
            let meta = parsed.workspace.expect("workspace identity written");
            assert_eq!(meta.description.as_deref(), Some(value));
            assert_eq!(meta.owner.as_deref(), Some(value));
        }
    }

    #[test]
    fn yaml_scalar_only_quotes_what_needs_it() {
        assert_eq!(yaml_scalar("api"), "api");
        assert_eq!(yaml_scalar("Ada Lovelace"), "Ada Lovelace");
        // Values YAML would otherwise read as a bool/number/null get quoted.
        assert_ne!(yaml_scalar("true"), "true");
        assert_ne!(yaml_scalar("42"), "42");
        assert_ne!(yaml_scalar(""), "");
    }

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
        // so relative workflow paths resolve alongside it.
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
