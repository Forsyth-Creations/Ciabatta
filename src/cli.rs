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

        #[arg(short = 'c', long)]
        config: Option<std::path::PathBuf>,
    },

    /// List all available recipes defined in the config.
    List,

    /// Create a .ciabatta/ directory with a starter ciabatta.toml in the current directory.
    Init {
        /// CI/CD system to pre-configure (gitlab, github, jenkins, circleci, azure, bitbucket).
        #[arg(long, value_name = "SYSTEM")]
        ci: Option<String>,

        /// Container runtime to use (docker or podman).
        #[arg(long, default_value = "docker")]
        containers: String,

        /// Overwrite an existing .ciabatta/ciabatta.toml if one exists.
        #[arg(long)]
        force: bool,
    },

    /// Interactive TUI browser — view registries, check artifact paths, push on demand.
    #[command(alias = "browse")]
    Tui,

    /// Configuration helpers.
    Config {
        #[command(subcommand)]
        subcommand: ConfigCommand,
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
