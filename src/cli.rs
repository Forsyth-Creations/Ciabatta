use clap::{Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(name = "ciabatta", about = "Artifact publishing made easy 🍞", version)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand, Debug)]
pub enum Commands {
    /// Run (push) one or more recipes in parallel.
    Run {
        /// Recipe names to execute. Runs all if omitted.
        #[arg(name = "RECIPE")]
        recipes: Vec<String>,

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
