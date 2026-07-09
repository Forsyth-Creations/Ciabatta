use clap::{Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(name = "ciabatta", about = "Artifact publishing made easy 🍞", version)]
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

    /// Run a deploy recipe: a DAG of dependent script steps with error-recovery
    /// branches. Add --gui for a live web view, or --build to design a flowchart.
    Deploy {
        /// Recipe names to deploy. Deploys all deploy-capable recipes if omitted.
        #[arg(name = "RECIPE")]
        recipes: Vec<String>,

        /// Deploy only the recipes grouped by the named menu (repeatable).
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

        /// Show the deploy live in a web browser (flowchart + logs + fix-it
        /// buttons for recovery nodes) instead of the terminal TUI.
        #[arg(long)]
        gui: bool,

        /// Open a visual builder in the browser to design a flowchart TOML file.
        /// Runs nothing; you copy the generated TOML into your own file.
        #[arg(long, conflicts_with = "gui")]
        build: bool,

        /// Port for the --gui / --build web view.
        #[arg(short = 'p', long, default_value_t = 8088)]
        port: u16,
    },

    /// Print CIABATTA_* variables (resolved from local git) as shell `export`
    /// lines, so you can load them into your shell: eval "$(ciabatta source)"
    Source {
        /// Set/override a variable (KEY=VALUE) in the printed output.
        #[arg(short = 'e', long = "env", value_name = "KEY=VALUE")]
        env: Vec<String>,
    },

    /// List all available recipes defined in the config.
    List,

    /// Create a .ciabatta/ directory with a starter ciabatta.toml in the current directory.
    Init {
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

        /// Port for the local web view.
        #[arg(short = 'p', long, default_value_t = 8080)]
        port: u16,

        /// Only write the JSON; don't start the web server.
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

    /// Manage a personal todo list.
    ///
    /// With no arguments, launches a small web app to add / complete / remove
    /// tasks. Pass a string to add a task from the command line. Pass -d to run
    /// the web app in the background.
    Todo {
        /// Task text to add. When given, the task is added and ciabatta exits
        /// (the web app is not started).
        #[arg(name = "TASK")]
        task: Option<String>,

        /// Run the web app in the background (detached) instead of the
        /// foreground. Ignored when a TASK is given.
        #[arg(short = 'd', long)]
        detach: bool,

        /// Port for the local web app.
        #[arg(short = 'p', long, default_value_t = 7878)]
        port: u16,
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
            required = true
        )]
        command: Vec<String>,

        /// Notify when a new log line contains this phrase (repeatable).
        #[arg(short = 't', long = "trigger", value_name = "PHRASE")]
        triggers: Vec<String>,

        /// Cap the in-memory log buffer; older lines are dropped past this.
        #[arg(long, default_value_t = 200_000)]
        max_lines: usize,

        /// Port for the local web view.
        #[arg(short = 'p', long, default_value_t = 8090)]
        port: u16,

        /// Don't open the browser automatically.
        #[arg(long)]
        no_open: bool,
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
}

#[derive(Subcommand, Debug)]
pub enum ConfigCommand {
    /// Show the current resolved configuration.
    Show,
    /// Show documentation on the config file format and available options.
    #[command(name = "reference", alias = "ref")]
    Reference,
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
