use clap::{Args, Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(
    name = "ciabatta",
    about = "Monorepo orchestration and artifact publishing 🍞",
    version,
    // Any subcommand ciabatta doesn't define is taken as a workflow name, so
    // `ciabatta build` runs the monorepo's `build` workflow. See
    // [`Commands::External`].
    allow_external_subcommands = true,
    after_help = "Any name that isn't a command above is a workflow: `ciabatta build`, \
                  `ciabatta test`, `ciabatta lint`. It runs that workflow across every \
                  sub-workspace that defines one, in dependency order.\n\n  \
                  ciabatta list            what workflows exist, and who owns them\n  \
                  ciabatta build --graph   show the graph without running it"
)]
pub struct Cli {
    /// Enable debug logging to stderr. Can also be enabled by setting the
    /// CIABATTA_DEBUG environment variable (to any non-empty value other than
    /// "0"/"false"). For finer control, set CIABATTA_LOG (e.g. `ciabatta=trace`).
    #[arg(long, global = true)]
    pub debug: bool,

    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand, Debug)]
pub enum Commands {
    /// Push one or more recipes in parallel (all if none named).
    Push {
        /// Recipe names to execute. Pushes all if omitted.
        #[arg(name = "RECIPE")]
        recipes: Vec<String>,

        /// Push only the recipes grouped by the named menu (repeatable). Combines
        /// with any RECIPE arguments. See the [menus] config section.
        #[arg(long = "cookbook", visible_alias = "menu", value_name = "MENU")]
        cookbooks: Vec<String>,

        /// Set an environment variable (KEY=VALUE). Overrides CI-derived vars.
        #[arg(short = 'e', long = "env", value_name = "KEY=VALUE")]
        env: Vec<String>,

        /// Show what would happen without actually running anything.
        #[arg(long)]
        dry_run: bool,

        /// Disable the TUI and print progress to stdout.
        #[arg(long)]
        no_tui: bool,

        /// Derive CIABATTA_BRANCH/_COMMIT/_TAG/_BUILD_NUMBER from local git
        /// history instead of the configured CI system.
        #[arg(long)]
        local: bool,

        /// Path to ciabatta.toml (overrides .ciabatta/ciabatta.toml discovery).
        #[arg(short = 'c', long)]
        config: Option<std::path::PathBuf>,
    },

    /// Pull (download) artifacts for one or more recipes.
    Pull {
        #[arg(name = "RECIPE")]
        recipes: Vec<String>,

        /// Pull only the recipes grouped by the named menu (repeatable). Combines
        /// with any RECIPE arguments. See the [menus] config section.
        #[arg(long = "cookbook", visible_alias = "menu", value_name = "MENU")]
        cookbooks: Vec<String>,

        #[arg(short = 'e', long = "env", value_name = "KEY=VALUE")]
        env: Vec<String>,

        #[arg(long)]
        dry_run: bool,

        #[arg(long)]
        no_tui: bool,

        /// Derive CIABATTA_BRANCH/_COMMIT/_TAG/_BUILD_NUMBER from local git
        /// history instead of the configured CI system.
        #[arg(long)]
        local: bool,

        #[arg(short = 'c', long)]
        config: Option<std::path::PathBuf>,
    },

    /// Run a collection of scripts as one graph.
    ///
    /// A TARGET is a monorepo workflow (`build`, `test`, …) or a recipe from
    /// this project's ciabatta.toml — there is no difference in how they run.
    /// Naming several compiles them into a single graph, so shared
    /// dependencies run once and everything else runs in dependency order.
    ///
    ///   ciabatta run build              the whole monorepo's build
    ///   ciabatta run build test         both, as one graph
    ///   ciabatta run test --filter tag:fast     just the fast slice of it
    ///
    /// Add --gui for a live web view, or --build to design a flowchart.
    Run(RunArgs),

    /// Print CIABATTA_* variables (resolved from local git) as shell `export`
    /// lines, so you can load them into your shell: eval "$(ciabatta source)"
    Source {
        /// Set/override a variable (KEY=VALUE) in the printed output.
        #[arg(short = 'e', long = "env", value_name = "KEY=VALUE")]
        env: Vec<String>,
    },

    /// Run a workflow across the monorepo, by name.
    ///
    /// Equivalent to typing the workflow name directly: `ciabatta workflow
    /// build` and `ciabatta build` do the same thing. Use this longer form when
    /// a workflow's name would collide with one of ciabatta's own commands.
    #[command(visible_alias = "wf")]
    Workflow(WorkflowArgs),

    /// List what this workspace can do: every sub-workspace, its workflows,
    /// who owns them, and what they need — plus the recipes in the local config.
    ///
    /// This is the answer to "what scripts exist in this monorepo?" without
    /// opening a single package.
    List {
        /// Show only entries matching this term. Searches names, descriptions,
        /// owners, tags, and the commands steps actually run.
        #[arg(short = 's', long, value_name = "TERM")]
        search: Option<String>,

        /// Also list every step inside each workflow.
        #[arg(short = 'v', long)]
        verbose: bool,

        /// Skip the workspace catalogue and list only this project's recipes.
        #[arg(long)]
        recipes: bool,
    },

    /// Create a .ciabatta/ directory with a starter ciabatta.toml in the current directory.
    ///
    /// With --example, generates a complete worked monorepo instead: four
    /// sub-workspaces that really depend on each other, workflows spanning
    /// them, scripts, tags, timeouts, a recovery node, and a README explaining
    /// every part of it. Every step runs, so it works on a bare machine.
    Init {
        /// Opt this directory in as a sub-workspace of a monorepo: writes a
        /// [workspace] identity plus a starter workflow under
        /// .ciabatta/workflows/, instead of a publishing-only config.
        #[arg(long)]
        lib: bool,

        /// Generate a complete example monorepo to learn from — multiple
        /// sub-workspaces, cross-package dependencies, scripts, and a README
        /// that explains the layout. Written to ./ciabatta-example unless
        /// --into says otherwise.
        #[arg(long, conflicts_with = "lib")]
        example: bool,

        /// Where to write the example (--example). Defaults to
        /// ./ciabatta-example.
        #[arg(long, value_name = "DIR", requires = "example")]
        into: Option<std::path::PathBuf>,

        /// Include a Nexus registry and a `release` workflow that publishes to
        /// it as a step on the graph (--example).
        #[arg(long, requires = "example")]
        nexus: bool,

        /// Include a Dockerfile and a `deploy` workflow that builds and pushes
        /// a container image (--example).
        #[arg(long, requires = "example")]
        docker: bool,

        /// Include every optional part of the example (--example).
        #[arg(long, requires = "example")]
        all: bool,

        /// The sub-workspace's name (--lib). Defaults to the directory name.
        #[arg(long, value_name = "NAME")]
        name: Option<String>,

        /// What this sub-workspace is, in one line (--lib). Every package
        /// should have one — it's what `ciabatta list` shows people.
        #[arg(long, value_name = "TEXT")]
        description: Option<String>,

        /// Who owns this sub-workspace (--lib). Defaults to your git user name,
        /// so the scripts you write have someone to ask about them.
        #[arg(long, value_name = "NAME")]
        owner: Option<String>,

        /// Sub-workspaces this one depends on (--lib, repeatable). Accepts
        /// "other" or "other:workflow".
        #[arg(long = "depends-on", value_name = "MEMBER")]
        depends_on: Vec<String>,

        /// Name of the starter workflow to scaffold (--lib). Defaults to build.
        #[arg(long, value_name = "NAME")]
        workflow: Option<String>,

        /// CI/CD system to pre-configure (gitlab, github, jenkins, circleci, azure, bitbucket).
        #[arg(long, value_name = "SYSTEM")]
        ci: Option<String>,

        /// Container runtime to use (docker or podman). When omitted, ciabatta
        /// auto-detects what's installed at run time.
        #[arg(long, value_name = "RUNTIME")]
        containers: Option<String>,

        /// Overwrite an existing .ciabatta/ciabatta.toml if one exists.
        #[arg(long)]
        force: bool,
    },

    /// Interactive TUI browser — view registries, check artifact paths, push on demand.
    #[command(alias = "browse")]
    Tui,

    /// Analyze the codebase dependency graph and serve an interactive view.
    Analyze {
        /// Write the analysis JSON to this path (default: ciabatta-analyze.json).
        #[arg(short = 'o', long)]
        output: Option<std::path::PathBuf>,

        /// Port the ciabatta daemon listens on (default 8099, or
        /// CIABATTA_DAEMON_PORT).
        #[arg(short = 'p', long)]
        port: Option<u16>,

        /// Only write the JSON; don't open the web view.
        #[arg(long)]
        no_serve: bool,

        /// Query the OSV database for known vulnerabilities (requires network).
        #[arg(long)]
        check_vulns: bool,

        /// Requirements file (adds a "Requirements" column). Overrides config.
        #[arg(long)]
        requirements: Option<std::path::PathBuf>,

        /// Trace CSV (requirement,file) connecting requirements into the graph.
        #[arg(long)]
        trace: Option<std::path::PathBuf>,

        /// Path to ciabatta.toml (overrides .ciabatta/ciabatta.toml discovery).
        #[arg(short = 'c', long)]
        config: Option<std::path::PathBuf>,
    },

    /// Manage a todo list — this project's, or the global one.
    ///
    /// With no arguments, opens the todo page in ciabatta's web app (starting
    /// the background daemon if it isn't already running). Pass a string to add
    /// a task from the command line without opening anything.
    ///
    /// A task added inside a project belongs to that project. `--global` files
    /// it on the global list instead, which is for the things that aren't about
    /// any one repo — that list has its own place on the dashboard.
    Todo {
        /// Task text to add. When given, the task is added and ciabatta exits
        /// (the web app is not opened).
        #[arg(name = "TASK")]
        task: Option<String>,

        /// Add the task to the global list rather than to this project.
        #[arg(short, long)]
        global: bool,

        /// Deprecated and ignored: the daemon already runs in the background,
        /// so the todo app outlives this command either way.
        #[arg(short = 'd', long, hide = true)]
        detach: bool,

        /// Port the ciabatta daemon listens on (default 8099, or
        /// CIABATTA_DAEMON_PORT).
        #[arg(short = 'p', long)]
        port: Option<u16>,
    },

    /// Run a command and stream its logs into a live, searchable web view.
    ///
    /// The command runs through your shell, so pipes, &&, and redirects work —
    /// quote the whole thing when you use them:
    ///   ciabatta watch "npm run dev | grep -i error"
    /// Set trigger phrases with -t to get notified when a matching line appears.
    Watch {
        /// The command to run (and its arguments). Everything after `watch` is
        /// captured, including the command's own flags.
        #[arg(
            name = "COMMAND",
            trailing_var_arg = true,
            allow_hyphen_values = true,
            // The other three modes act on sessions that already exist, so they
            // have no command of their own to take.
            required_unless_present_any = ["stop", "attach", "list"]
        )]
        command: Vec<String>,

        /// Notify when a new log line contains this phrase (repeatable).
        #[arg(short = 't', long = "trigger", value_name = "PHRASE")]
        triggers: Vec<String>,

        /// Cap the in-memory log buffer; older lines are dropped past this.
        #[arg(long, default_value_t = 200_000)]
        max_lines: usize,

        /// Port the ciabatta daemon listens on (default 8099, or
        /// CIABATTA_DAEMON_PORT).
        #[arg(short = 'p', long)]
        port: Option<u16>,

        /// Don't open the browser automatically.
        #[arg(long)]
        no_open: bool,

        /// Stop a running watch session by id and exit. Sessions are listed on
        /// the Watch page; Ctrl-C only detaches from one.
        #[arg(long, value_name = "ID", conflicts_with = "COMMAND")]
        stop: Option<u64>,

        /// Tail an existing session by id instead of starting a command.
        ///
        /// This is how you follow a `persistent` workflow step: the daemon owns
        /// it as a watch session that outlives the run, and the run prints the
        /// id to attach to. `ciabatta watch --list` shows what's running.
        #[arg(
            long,
            visible_alias = "session",
            value_name = "ID",
            conflicts_with_all = ["COMMAND", "stop"]
        )]
        attach: Option<u64>,

        /// List the sessions the daemon is running and exit.
        #[arg(long, conflicts_with_all = ["COMMAND", "stop", "attach"])]
        list: bool,
    },

    /// AI assistant: chat with an LLM that learns your codebase architecture.
    ///
    /// With no subcommand, opens a chat TUI and serves the live architecture
    /// mind map in the browser. The assistant tags files as it works (you
    /// confirm the tags), and your feedback trains a per-project confidence
    /// score stored under .ciabatta/ai/.
    Ai {
        #[command(subcommand)]
        subcommand: Option<AiCommand>,

        /// Port for the live mind-map / daemon web view.
        #[arg(short = 'p', long, default_value_t = 8095, global = true)]
        port: u16,

        /// Don't start the mind-map web server alongside the TUI.
        #[arg(long)]
        no_graph: bool,

        /// Assistant mode: plan (research only, no edits), edit (changes wait
        /// for your approval), or auto (changes apply immediately).
        /// Shift-Tab cycles modes inside the TUI.
        #[arg(long, default_value = "edit", global = true)]
        mode: String,

        /// Resume the most recent saved conversation instead of starting a new
        /// one. Conversations are stored under .ciabatta/ai/conversations/.
        #[arg(short = 'c', long = "continue", global = true)]
        continue_last: bool,
    },

    /// Manage the background daemon that serves ciabatta's web apps.
    ///
    /// You rarely need this: any command with a web view starts the daemon
    /// automatically. These subcommands are for when you want to inspect it,
    /// restart it, or read its log.
    Daemon {
        #[command(subcommand)]
        subcommand: DaemonCommand,
    },

    /// Configuration helpers.
    Config {
        #[command(subcommand)]
        subcommand: ConfigCommand,
    },

    /// Interactively set up your project: add registries, or auto-suggest recipes.
    Configure {
        #[command(subcommand)]
        subcommand: Option<ConfigureCommand>,
    },

    /// Show what a run would reuse from the cache and what it would rebuild.
    ///
    /// Runs nothing. For every step it prints the decision — up to date, a
    /// cache hit, or a rebuild — and for a rebuild, exactly what changed:
    /// which input files (with the lines), which environment variables, and
    /// which upstream stages produced something different.
    ///
    ///   ciabatta dry-run build           what would this build actually do?
    ///   ciabatta dry-run build --diff    …and show me the lines
    #[command(name = "dry-run", visible_alias = "dryrun")]
    DryRun {
        /// Recipes and/or workflows to plan. With none, plans every
        /// run-capable recipe in this project.
        #[arg(name = "TARGET")]
        targets: Vec<String>,

        /// Show the line-by-line diff for every changed input file, not just
        /// which files moved.
        #[arg(long, short)]
        diff: bool,

        /// Print the plan as JSON, for scripting.
        #[arg(long)]
        json: bool,

        /// Set an environment variable (KEY=VALUE). Cached builds key on the
        /// variables their config declares, so this can change the answer.
        #[arg(short = 'e', long = "env", value_name = "KEY=VALUE")]
        env: Vec<String>,

        /// Derive CIABATTA_* variables from local git.
        #[arg(long)]
        local: bool,

        /// Path to the ciabatta config (overrides discovery).
        #[arg(short = 'c', long)]
        config: Option<std::path::PathBuf>,
    },

    /// Set up and inspect this workspace's build cache.
    ///
    /// Caching is off until a workspace opts in, because a cache that turns
    /// itself on is a cache that will one day serve a stale artifact nobody
    /// asked it to keep. `ciabatta cache init` is the way in: it looks at what
    /// is actually in the directory and proposes the inputs and outputs.
    Cache {
        #[command(subcommand)]
        subcommand: CacheCommand,
    },

    /// Run or connect to a shared remote cache.
    ///
    ///   ciabatta remote-cache init                 write a server config
    ///   ciabatta remote-cache start                run the server
    ///   ciabatta remote-cache login <URL>          connect this machine to one
    #[command(name = "remote-cache", visible_alias = "rc")]
    RemoteCache {
        #[command(subcommand)]
        subcommand: RemoteCacheCommand,
    },

    /// Manage this ciabatta installation.
    #[command(name = "self")]
    Zelf {
        #[command(subcommand)]
        subcommand: SelfCommand,
    },

    /// Turn an existing script into a ciabatta recipe.
    ///
    /// A recipe *is* a script — this reads one, works out what it needs and
    /// what it produces, and writes the recipe into this workspace's
    /// `.ciabatta/` so it can join the graph like everything else.
    ///
    ///   ciabatta convert --script scripts/build.sh
    Convert {
        /// The script to convert.
        #[arg(long, short, value_name = "PATH")]
        script: std::path::PathBuf,

        /// Name for the generated recipe. Defaults to the script's filename.
        #[arg(long, short, value_name = "NAME")]
        name: Option<String>,

        /// Also write it as a workflow under `.ciabatta/workflows/`, so
        /// `ciabatta <name>` runs it across the monorepo.
        #[arg(long)]
        workflow: bool,

        /// Print the generated recipe instead of writing it.
        #[arg(long)]
        dry_run: bool,

        /// Overwrite an existing recipe of the same name.
        #[arg(long)]
        force: bool,
    },

    /// Any other name is a workflow: `ciabatta build`, `ciabatta test`, …
    ///
    /// Captured raw and re-parsed as [`WorkflowArgs`], which is why the flags
    /// after the name behave exactly as they do under `ciabatta workflow`.
    #[command(external_subcommand)]
    External(Vec<String>),
}

#[derive(Subcommand, Debug)]
pub enum CacheCommand {
    /// Work out what this workspace reads and writes, and write a `cache:`
    /// section proposing it.
    ///
    /// The proposal comes from the directory's real contents rather than from a
    /// template, because the one thing that has to be right is `inputs` — a
    /// build that reads a file nobody declared will be served a stale artifact.
    Init {
        /// Turn caching on straight away. Without this the section is written
        /// with `enabled: false` for you to review first.
        #[arg(long)]
        enable: bool,

        /// Also point this workspace at a remote cache.
        #[arg(long, value_name = "URL")]
        remote: Option<String>,

        /// Overwrite an existing `cache:` section.
        #[arg(long)]
        force: bool,
    },

    /// What the local cache is holding, and what it has saved.
    Status,

    /// Delete every cached entry for this project.
    Clean {
        /// Skip the confirmation prompt.
        #[arg(long, short)]
        yes: bool,
    },

    /// Apply a retention policy to the local cache.
    Prune {
        /// Evict entries unused for longer than this (`30d`, `12h`).
        #[arg(long, value_name = "DURATION")]
        max_age: Option<String>,

        /// Cap the store at this size (`10GB`, `500MB`).
        #[arg(long, value_name = "SIZE")]
        max_size: Option<String>,

        /// Cap the number of entries.
        #[arg(long, value_name = "N")]
        max_entries: Option<usize>,

        /// Show what would be evicted without removing anything.
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Subcommand, Debug)]
pub enum RemoteCacheCommand {
    /// Write a config for a new remote cache server.
    Init {
        /// Where to write it. Defaults to the current directory.
        #[arg(long, value_name = "DIR")]
        into: Option<std::path::PathBuf>,

        /// Port the server should listen on.
        #[arg(short, long)]
        port: Option<u16>,

        /// Directory for the artifact store, relative to the config.
        #[arg(long, value_name = "DIR", default_value = "storage")]
        storage: String,

        /// Overwrite an existing config.
        #[arg(long)]
        force: bool,
    },

    /// Run the remote cache server in the foreground.
    Start {
        /// Path to the server config. Defaults to `remote-cache.yaml` in the
        /// current directory.
        #[arg(short, long, value_name = "FILE")]
        config: Option<std::path::PathBuf>,

        /// Override the configured port.
        #[arg(short, long)]
        port: Option<u16>,
    },

    /// Log this machine in to a remote cache.
    Login {
        /// The server's base URL, e.g. http://cache.example.com:8380.
        #[arg(name = "URL")]
        url: String,

        /// Don't verify the server's TLS certificate.
        ///
        /// For a cache behind a self-signed certificate, or an internal CA this
        /// machine doesn't have. Remembered for later commands against the same
        /// server. With it off, HTTPS is an encrypted channel to whoever
        /// answered — so the artifacts you're handed are only as trustworthy as
        /// the network.
        #[arg(long)]
        no_tls_verify: bool,

        /// Username. Prompted for when the server needs one and it's omitted.
        #[arg(short, long)]
        username: Option<String>,

        /// Read the password (or token) from this environment variable instead
        /// of prompting — how CI logs in without a terminal.
        #[arg(long, value_name = "VAR")]
        password_env: Option<String>,
    },

    /// Forget the saved session for a remote cache.
    Logout {
        /// The server to log out of. With none, logs out of all of them.
        #[arg(name = "URL")]
        url: Option<String>,
    },

    /// Show a remote cache's stats: hits, misses, storage, and retention.
    Status {
        /// The server to ask. Defaults to the one this workspace is configured
        /// to use.
        #[arg(name = "URL")]
        url: Option<String>,
    },

    /// Mint a token for a user and print the config line to add.
    #[command(name = "add-user")]
    AddUser {
        /// The username.
        #[arg(name = "NAME")]
        name: String,

        /// Give them read access but not write access.
        #[arg(long)]
        read_only: bool,
    },
}

#[derive(Subcommand, Debug)]
pub enum SelfCommand {
    /// Update this ciabatta binary from the remote cache that serves it.
    ///
    /// The cache your workspace is already configured against advertises a
    /// build and its SHA-256; this downloads it, checks the hash, and swaps the
    /// binary over only if it matches.
    Update {
        /// The cache to update from. Defaults to the one this workspace uses.
        #[arg(long, value_name = "URL")]
        from: Option<String>,

        /// Check whether an update is available and exit.
        #[arg(long)]
        check: bool,

        /// Install even when the advertised build is what's already running.
        #[arg(long)]
        force: bool,
    },
}

/// Arguments for `ciabatta run`.
///
/// A target is a monorepo workflow or a recipe from this project's config. The
/// distinction is only about where the steps were written down, not about how
/// they execute — both compile to the same graph and run through the same
/// engine, which is why one command takes either.
#[derive(Args, Debug, Clone)]
pub struct RunArgs {
    /// Workflows and/or recipes to run. With none, runs every run-capable
    /// recipe in this project.
    #[arg(name = "TARGET")]
    pub targets: Vec<String>,

    /// Run only the recipes grouped by the named menu (repeatable).
    #[arg(long = "cookbook", visible_alias = "menu", value_name = "MENU")]
    pub cookbooks: Vec<String>,

    /// Run only the steps matching this term (repeatable), instead of the whole
    /// graph. Accepts tag:<name>, workspace:<name>, kind:<name>, owner:<name>,
    /// step:<name>, or a bare word searching all of them. Prefix with ! to
    /// exclude: --filter tag:fast --filter '!tag:flaky'.
    ///
    /// Filtering prunes the graph rather than expanding a selection, so the
    /// steps that survive assume their pruned dependencies already ran — it's
    /// the fast debug loop, not a first build.
    #[arg(short = 'f', long = "filter", value_name = "TERM")]
    pub filter: Vec<String>,

    /// Start only from these sub-workspaces (repeatable). Their dependencies
    /// still come along unless --isolated is given.
    #[arg(long, value_name = "MEMBER")]
    pub only: Vec<String>,

    /// Don't follow dependencies into other sub-workspaces.
    #[arg(long)]
    pub isolated: bool,

    /// Show the resolved graph without running it: every step, in wave order,
    /// with what it does and where it came from. Reflects --filter, so it
    /// answers "what would this actually run?". With --tui it opens the
    /// interactive explorer instead, one node at a time.
    #[arg(long)]
    pub graph: bool,

    /// Set an environment variable (KEY=VALUE). Overrides CI-derived vars.
    #[arg(short = 'e', long = "env", value_name = "KEY=VALUE")]
    pub env: Vec<String>,

    /// Show what would happen without actually running anything.
    #[arg(long)]
    pub dry_run: bool,

    /// Run inside the live TUI instead of printing plain progress.
    ///
    /// A run prints ordinary text by default: it's what CI wants, it's what
    /// survives in a scrollback, and it's what you can pipe. The TUI is the
    /// richer view when you're watching a run happen.
    #[arg(long)]
    pub tui: bool,

    /// Accepted and ignored — plain output is the default. Kept so existing
    /// scripts and CI jobs that pass it keep working.
    #[arg(long, hide = true)]
    pub no_tui: bool,

    /// Derive CIABATTA_BRANCH/_COMMIT/_TAG/_BUILD_NUMBER from local git history
    /// instead of the configured CI system.
    #[arg(long)]
    pub local: bool,

    /// Path to ciabatta.toml (overrides .ciabatta/ciabatta.toml discovery).
    #[arg(short = 'c', long)]
    pub config: Option<std::path::PathBuf>,

    /// Show the run live in a web browser (flowchart + logs + fix-it buttons
    /// for recovery nodes) instead of the terminal TUI.
    #[arg(long)]
    pub gui: bool,

    /// Open a visual builder in the browser to design a flowchart TOML file.
    /// Runs nothing; you copy the generated TOML into your own file.
    #[arg(long, conflicts_with = "gui")]
    pub build: bool,

    /// Port the ciabatta daemon listens on (default 8099, or
    /// CIABATTA_DAEMON_PORT).
    #[arg(short = 'p', long)]
    pub port: Option<u16>,
}

impl RunArgs {
    /// Whether this invocation takes over the terminal with the TUI.
    ///
    /// Opt-in, and `--no-tui` still wins if both are given — a script that
    /// already asks for plain output must keep getting it.
    pub fn use_tui(&self) -> bool {
        self.tui && !self.no_tui
    }
}

/// Parses a bare `ciabatta <workflow> [flags]` invocation.
///
/// An external subcommand arrives as raw argv, which this re-parses through the
/// exact [`WorkflowArgs`] definition `ciabatta workflow` uses — so the two
/// spellings can't drift apart.
#[derive(Parser, Debug)]
#[command(
    name = "ciabatta",
    about = "Run a workflow across every sub-workspace that defines one"
)]
pub struct WorkflowInvocation {
    #[command(flatten)]
    pub args: WorkflowArgs,
}

/// Arguments for running a workflow across the monorepo.
///
/// Ciabatta collects every sub-workspace's workflow of this name, follows the
/// dependencies each one declares, and runs the resulting graph in order.
#[derive(Args, Debug, Clone)]
pub struct WorkflowArgs {
    /// The workflow to run (`build`, `test`, `lint`, …). Omit it to see what
    /// workflows this workspace defines.
    #[arg(name = "WORKFLOW")]
    pub workflow: Option<String>,

    /// Further workflows to fold into the same graph, so `ciabatta build test`
    /// compiles both at once rather than running one and then the other.
    #[arg(name = "ALSO")]
    pub also: Vec<String>,

    /// Start only from these sub-workspaces (repeatable). Their dependencies
    /// still come along unless --isolated is given.
    #[arg(long, value_name = "MEMBER")]
    pub only: Vec<String>,

    /// Run only the steps matching this term (repeatable). Accepts
    /// tag:<name>, workspace:<name>, kind:<name>, owner:<name>, step:<name>,
    /// or a bare word; prefix with ! to exclude.
    #[arg(short = 'f', long = "filter", value_name = "TERM")]
    pub filter: Vec<String>,

    /// Don't follow dependencies into other sub-workspaces — run just what was
    /// selected, for when everything upstream is already built.
    #[arg(long)]
    pub isolated: bool,

    /// Print the graph and exit without running anything.
    #[arg(long)]
    pub graph: bool,

    /// Set an environment variable (KEY=VALUE) for every step.
    #[arg(short = 'e', long = "env", value_name = "KEY=VALUE")]
    pub env: Vec<String>,

    /// Show the graph and everything it would do, without executing a step.
    #[arg(long)]
    pub dry_run: bool,

    /// Run inside the live TUI instead of printing plain progress. A workflow
    /// is an ordinary run, so it prints plain text unless you ask for the TUI.
    #[arg(long)]
    pub tui: bool,

    /// Accepted and ignored — plain output is the default. Kept so existing
    /// scripts and CI jobs that pass it keep working.
    #[arg(long, hide = true)]
    pub no_tui: bool,

    /// Watch the run live in a browser instead of the terminal.
    #[arg(long)]
    pub gui: bool,

    /// Derive CIABATTA_BRANCH/_COMMIT/_TAG/_BUILD_NUMBER from local git.
    #[arg(long)]
    pub local: bool,

    /// Port the ciabatta daemon listens on (with --gui).
    #[arg(short = 'p', long)]
    pub port: Option<u16>,
}

impl WorkflowArgs {
    /// Whether this invocation takes over the terminal with the TUI. See
    /// [`RunArgs::use_tui`] — the two spellings must agree.
    pub fn use_tui(&self) -> bool {
        self.tui && !self.no_tui
    }
}

#[derive(Subcommand, Debug)]
pub enum AiCommand {
    /// Interactively configure the assistant (Claude or an OpenAI-compatible
    /// endpoint) and write the [ai] section into .ciabatta/ciabatta.toml.
    Setup,

    /// Ask a one-shot question and print the answer (no TUI).
    Ask {
        /// The question. Everything after `ask` is captured.
        #[arg(name = "PROMPT", trailing_var_arg = true, required = true)]
        prompt: Vec<String>,
    },

    /// Run only the AI assistant daemon: the live mind map plus a JSON API
    /// (POST /api/ask, /api/feedback, /api/confirm).
    Serve,

    /// Resume a saved conversation. With no id, lists the saved conversations
    /// (stored under .ciabatta/ai/conversations/) so you can pick one.
    Resume {
        /// The conversation id to resume (see `ciabatta ai resume` with no id).
        id: Option<String>,
    },

    /// Report what changed in the repo over the past N days (default 7) by
    /// summarizing git history with the assistant.
    Report {
        /// How many days back to look (default 7).
        days: Option<u64>,

        /// Also save the report as a PDF. Give a path, or pass the flag alone
        /// to write ciabatta-report-<date>.pdf in the current directory.
        #[arg(long, value_name = "FILE", num_args = 0..=1, default_missing_value = "")]
        pdf: Option<String>,
    },

    /// Add your own architecture tag to the mind map, then let the assistant do
    /// a quick pass to connect the files that belong to it.
    Tag {
        /// The tag / architecture name (e.g. auth, frontend, cli).
        name: String,

        /// An optional one-line description of what this architecture is.
        #[arg(name = "DESCRIPTION", trailing_var_arg = true)]
        description: Vec<String>,
    },

    /// Delete a saved conversation by id (see `ciabatta ai resume` for the list).
    Delete {
        /// The conversation id to delete.
        id: String,
    },

    /// Delete every saved conversation for this project. Prompts for
    /// confirmation unless --yes is given.
    Clear {
        /// Skip the confirmation prompt.
        #[arg(long, short)]
        yes: bool,
    },

    /// Ship a task to the assistant to complete behind the scenes. It runs the
    /// full agent loop autonomously (auto-accept mode) and records the result.
    Ship {
        /// The task to complete. Everything after `ship` is captured. Omit when
        /// using --todo.
        #[arg(name = "TASK", trailing_var_arg = true)]
        task: Vec<String>,

        /// Ship the text of this personal todo (see `ciabatta todo`) instead of
        /// a literal task; the todo is marked done if the job succeeds.
        #[arg(long, value_name = "ID")]
        todo: Option<u64>,
    },

    /// List background AI tasks and their status (see `ciabatta ai ship`).
    Jobs,

    /// Burn-in: traverse the codebase, determine its architecture parts, and
    /// build the mind map in one pass. Watch it happen live in the browser.
    ///
    /// The assistant first surveys the file tree to name the architectures,
    /// then reads files batch by batch and tags each into the map. Tags apply
    /// immediately by default; use --review to queue every tag for your
    /// confirmation instead (shown as ghost nodes on the map).
    BurnIn {
        /// Queue tags as pending proposals for review instead of applying
        /// them to the map immediately.
        #[arg(long)]
        review: bool,

        /// Analyze at most N files (useful for a quick first pass).
        #[arg(long, value_name = "N")]
        limit: Option<usize>,
    },
}

#[derive(Subcommand, Debug)]
pub enum DaemonCommand {
    /// Run the daemon in the foreground. This is what the automatic background
    /// start invokes; run it yourself to watch the log live or debug a daemon
    /// that won't come up.
    Serve {
        /// Port to listen on. Defaults to CIABATTA_DAEMON_PORT, then 8099.
        #[arg(short = 'p', long)]
        port: Option<u16>,
    },

    /// Show whether the daemon is running, and where.
    Status,

    /// Stop the running daemon. Background work it owns (watch sessions,
    /// runs) stops with it.
    Stop,

    /// Stop the daemon if it's running, then start a fresh one.
    Restart {
        /// Port for the new daemon.
        #[arg(short = 'p', long)]
        port: Option<u16>,
    },

    /// Print the daemon log (~/.ciabatta/daemon.log).
    Logs {
        /// Show only the last N lines.
        #[arg(short = 'n', long, default_value_t = 200)]
        lines: usize,

        /// Follow the log as it grows, like `tail -f`.
        #[arg(short = 'f', long)]
        follow: bool,
    },
}

#[derive(Subcommand, Debug)]
pub enum ConfigCommand {
    /// Show the current resolved configuration.
    Show,
    /// Show documentation on the config file format and available options.
    #[command(name = "reference", alias = "ref")]
    Reference,

    /// Convert this checkout's TOML config files to YAML.
    ///
    /// Ciabatta reads both, so this is optional — but YAML is what it writes
    /// and documents from 0.2.0. Every `.ciabatta/` at or below the workspace
    /// root is converted: the project config, its workflows, and any flowchart
    /// files. The originals are left in place for you to delete once you're
    /// happy with the result.
    Migrate {
        /// Show what would be converted without writing anything.
        #[arg(long)]
        dry_run: bool,

        /// Migrate this directory instead of the discovered workspace root.
        #[arg(long, value_name = "DIR")]
        path: Option<std::path::PathBuf>,
    },
}

#[derive(Subcommand, Debug)]
pub enum ConfigureCommand {
    /// Analyze the project and suggest recipes for pushing to registries.
    Auto {
        /// Apply every suggestion without prompting.
        #[arg(long)]
        yes: bool,
    },
}

/// Parse `-e KEY=VALUE` flags into a HashMap.
pub fn parse_env_flags(
    flags: &[String],
) -> anyhow::Result<std::collections::HashMap<String, String>> {
    let mut map = std::collections::HashMap::new();
    for flag in flags {
        let (k, v) = flag
            .split_once('=')
            .ok_or_else(|| anyhow::anyhow!("Invalid env flag '{}': expected KEY=VALUE", flag))?;
        map.insert(k.to_string(), v.to_string());
    }
    Ok(map)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_args(argv: &[&str]) -> RunArgs {
        match Cli::try_parse_from(argv).expect("parses").command {
            Commands::Run(args) => args,
            other => panic!("expected a run, got {other:?}"),
        }
    }

    fn workflow_args(argv: &[&str]) -> WorkflowArgs {
        match Cli::try_parse_from(argv).expect("parses").command {
            Commands::Workflow(args) => args,
            other => panic!("expected a workflow, got {other:?}"),
        }
    }

    #[test]
    fn a_run_prints_plain_text_unless_the_tui_is_asked_for() {
        assert!(!run_args(&["ciabatta", "run", "build"]).use_tui());
        assert!(run_args(&["ciabatta", "run", "build", "--tui"]).use_tui());

        // A workflow is an ordinary run, so the two spellings must agree —
        // `ciabatta build` and `ciabatta run build` can't differ in whether
        // they take over the terminal.
        assert!(!workflow_args(&["ciabatta", "workflow", "build"]).use_tui());
        assert!(workflow_args(&["ciabatta", "workflow", "build", "--tui"]).use_tui());
    }

    #[test]
    fn no_tui_is_still_accepted_and_still_means_plain() {
        // Scripts and CI jobs that already pass it must keep working, and must
        // keep getting plain output even alongside --tui.
        assert!(!run_args(&["ciabatta", "run", "build", "--no-tui"]).use_tui());
        assert!(!run_args(&["ciabatta", "run", "build", "--no-tui", "--tui"]).use_tui());
    }
}
