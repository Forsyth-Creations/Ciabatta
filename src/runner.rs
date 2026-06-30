use anyhow::{Result, bail};
use std::collections::HashMap;
use std::path::Path;
use tokio::sync::mpsc;

use crate::config::{CiabattaConfig, SimpleRecipe, substitute_vars, validate_publish_path};
use crate::registry::{self, RegistryOpOptions};

/// The four ordered stages of a push or pull pipeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StageKind {
    Login,
    Pre,
    Main,
    Post,
}

impl StageKind {
    /// All stages in execution order.
    pub const ALL: [StageKind; 4] = [
        StageKind::Login,
        StageKind::Pre,
        StageKind::Main,
        StageKind::Post,
    ];

    /// Position in the pipeline (0..4).
    pub fn index(self) -> usize {
        match self {
            StageKind::Login => 0,
            StageKind::Pre => 1,
            StageKind::Main => 2,
            StageKind::Post => 3,
        }
    }

    /// Full, mode-aware label, e.g. "pre-push" / "post-pull".
    pub fn label(self, mode: RunMode) -> &'static str {
        match (self, mode) {
            (StageKind::Login, _) => "login",
            (StageKind::Pre, RunMode::Push) => "pre-push",
            (StageKind::Pre, RunMode::Pull) => "pre-pull",
            (StageKind::Main, RunMode::Push) => "push",
            (StageKind::Main, RunMode::Pull) => "pull",
            (StageKind::Post, RunMode::Push) => "post-push",
            (StageKind::Post, RunMode::Pull) => "post-pull",
        }
    }

    /// Compact label for cramped UI (drops the push/pull suffix on pre/post).
    pub fn short(self, mode: RunMode) -> &'static str {
        match (self, mode) {
            (StageKind::Login, _) => "login",
            (StageKind::Pre, _) => "pre",
            (StageKind::Main, RunMode::Push) => "push",
            (StageKind::Main, RunMode::Pull) => "pull",
            (StageKind::Post, _) => "post",
        }
    }
}

#[derive(Debug, Clone)]
pub enum ProgressUpdate {
    Started(String),
    StageStarted {
        recipe: String,
        stage: StageKind,
    },
    /// A stage finished successfully. `ran` is false when it fell through to a
    /// default no-op (nothing to do), true when it actually executed something.
    StageFinished {
        recipe: String,
        stage: StageKind,
        ran: bool,
    },
    Log(String, String),
    Completed(String),
    Failed(String, String),
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum RunMode {
    Push,
    Pull,
}

/// Pre-flight validation: all publish-path vars must be set.
pub fn validate_recipes(
    config: &CiabattaConfig,
    recipe_names: &[String],
    env_vars: &HashMap<String, String>,
    mode: &RunMode,
) -> Result<()> {
    for name in recipe_names {
        let entry = config
            .recipes
            .get(name)
            .ok_or_else(|| anyhow::anyhow!("Recipe '{}' not found in config", name))?;

        let recipe = match mode {
            RunMode::Push => entry.push_recipe(),
            RunMode::Pull => entry
                .pull_recipe()
                .ok_or_else(|| anyhow::anyhow!("Recipe '{}' has no pull action defined", name))?,
        };

        // Only the built-in main action consumes publish_path; if the user
        // overrode `main` (or uses a bash_script) the placeholders are theirs.
        if recipe.main.is_none()
            && recipe.bash_script.is_none()
            && let Some(ref path) = recipe.publish_path
        {
            validate_publish_path(path, env_vars)?;
        }
    }
    Ok(())
}

pub async fn run_all(
    config: &CiabattaConfig,
    root: &Path,
    recipe_names: &[String],
    env_vars: &HashMap<String, String>,
    dry_run: bool,
    mode: RunMode,
    tx: mpsc::Sender<ProgressUpdate>,
) -> Result<()> {
    let mut handles = Vec::new();
    for name in recipe_names {
        let name = name.clone();
        let config = config.clone();
        let root = root.to_path_buf();
        let env_vars = env_vars.clone();
        let tx = tx.clone();

        let handle = tokio::spawn(async move {
            run_one(name, &config, &root, &env_vars, dry_run, mode, tx).await
        });
        handles.push(handle);
    }

    for handle in handles {
        handle.await??;
    }

    Ok(())
}

async fn run_one(
    name: String,
    config: &CiabattaConfig,
    root: &Path,
    env_vars: &HashMap<String, String>,
    dry_run: bool,
    mode: RunMode,
    tx: mpsc::Sender<ProgressUpdate>,
) -> Result<()> {
    let _ = tx.send(ProgressUpdate::Started(name.clone())).await;

    let result = execute_recipe(&name, config, root, env_vars, dry_run, mode, &tx).await;

    match result {
        Ok(()) => {
            let _ = tx.send(ProgressUpdate::Completed(name)).await;
        }
        Err(ref e) => {
            let _ = tx.send(ProgressUpdate::Failed(name, e.to_string())).await;
        }
    }

    result
}

/// Drive a single recipe through its four-stage pipeline.
async fn execute_recipe(
    name: &str,
    config: &CiabattaConfig,
    root: &Path,
    env_vars: &HashMap<String, String>,
    dry_run: bool,
    mode: RunMode,
    tx: &mpsc::Sender<ProgressUpdate>,
) -> Result<()> {
    let entry = config
        .recipes
        .get(name)
        .ok_or_else(|| anyhow::anyhow!("Recipe '{}' not found", name))?;

    let recipe: SimpleRecipe = match mode {
        RunMode::Push => entry.push_recipe(),
        RunMode::Pull => entry
            .pull_recipe()
            .ok_or_else(|| anyhow::anyhow!("Recipe '{}' has no pull action", name))?,
    };

    // Resolve the registry (if any) and the artifact paths once up front, so the
    // login and main stages share them.
    let registry_config = match recipe.registry.as_deref() {
        Some(rn) => Some(
            config
                .registries
                .get(rn)
                .ok_or_else(|| anyhow::anyhow!("Registry '{}' not found in config", rn))?,
        ),
        None => None,
    };

    let container_cmd = config
        .system
        .as_ref()
        .and_then(|s| s.containers.as_deref())
        .unwrap_or("docker");

    let resolved_remote = match recipe.publish_path.as_deref() {
        Some(p) => substitute_vars(p, env_vars)?,
        None => String::new(),
    };
    let local_path = root.join(recipe.local_artifact_path.as_deref().unwrap_or("."));

    let opts = registry_config.map(|rc| RegistryOpOptions {
        registry_name: recipe.registry.as_deref().unwrap_or_default(),
        registry_config: rc,
        local_path: &local_path,
        remote_path: &resolved_remote,
        env_vars,
        dry_run,
        container_cmd,
    });

    for stage in StageKind::ALL {
        let _ = tx
            .send(ProgressUpdate::StageStarted {
                recipe: name.to_string(),
                stage,
            })
            .await;

        let mut log: Vec<String> = Vec::new();
        let result = run_stage(
            stage,
            name,
            &recipe,
            opts.as_ref(),
            root,
            env_vars,
            dry_run,
            mode,
            &mut log,
        )
        .await;

        for line in &log {
            let _ = tx
                .send(ProgressUpdate::Log(name.to_string(), line.clone()))
                .await;
        }

        match result {
            Ok(ran) => {
                let _ = tx
                    .send(ProgressUpdate::StageFinished {
                        recipe: name.to_string(),
                        stage,
                        ran,
                    })
                    .await;
            }
            // The Failed update (with the current stage) is emitted by run_one.
            Err(e) => return Err(e),
        }
    }

    Ok(())
}

/// Execute a single stage. Returns `Ok(true)` if it actually ran a command,
/// `Ok(false)` if it fell through to a default no-op.
#[allow(clippy::too_many_arguments)]
async fn run_stage(
    stage: StageKind,
    name: &str,
    recipe: &SimpleRecipe,
    opts: Option<&RegistryOpOptions<'_>>,
    root: &Path,
    env_vars: &HashMap<String, String>,
    dry_run: bool,
    mode: RunMode,
    log: &mut Vec<String>,
) -> Result<bool> {
    match stage {
        StageKind::Login => {
            if let Some(cmd) = recipe.login.as_deref() {
                run_shell(cmd, root, env_vars, dry_run, log).await?;
                Ok(true)
            } else if let Some(opts) = opts {
                if let Some(script) = opts.registry_config.login_script.as_deref() {
                    run_login_script(script, root, env_vars, dry_run, log).await?;
                    Ok(true)
                } else {
                    registry::default_login(opts, log).await
                }
            } else {
                Ok(false)
            }
        }
        StageKind::Pre => run_optional(recipe.pre.as_deref(), root, env_vars, dry_run, log).await,
        StageKind::Main => {
            if let Some(cmd) = recipe.main.as_deref() {
                run_shell(cmd, root, env_vars, dry_run, log).await?;
                Ok(true)
            } else if let Some(script) = recipe.bash_script.as_deref() {
                run_bash_script(script, root, env_vars, dry_run, log).await?;
                Ok(true)
            } else if let Some(opts) = opts {
                match mode {
                    RunMode::Push => registry::push(opts, log).await?,
                    RunMode::Pull => registry::pull(opts, log).await?,
                }
                Ok(true)
            } else {
                bail!(
                    "Recipe '{}' has no push/pull action. Define a registry + publish_path, \
                     a bash_script, or a `main` command.",
                    name
                )
            }
        }
        StageKind::Post => run_optional(recipe.post.as_deref(), root, env_vars, dry_run, log).await,
    }
}

/// Run an optional stage override command; no-op (Ok(false)) when absent.
async fn run_optional(
    cmd: Option<&str>,
    root: &Path,
    env_vars: &HashMap<String, String>,
    dry_run: bool,
    log: &mut Vec<String>,
) -> Result<bool> {
    match cmd {
        Some(c) => {
            run_shell(c, root, env_vars, dry_run, log).await?;
            Ok(true)
        }
        None => Ok(false),
    }
}

/// Run an arbitrary shell command (the stage-override mechanism).
async fn run_shell(
    cmd: &str,
    root: &Path,
    env_vars: &HashMap<String, String>,
    dry_run: bool,
    log: &mut Vec<String>,
) -> Result<()> {
    log.push(format!("$ {cmd}"));
    if dry_run {
        log.push(format!("[dry-run] would run: {cmd}"));
        return Ok(());
    }
    registry::run_shell_command(cmd, root, env_vars, log).await
}

async fn run_bash_script(
    script: &str,
    root: &Path,
    env_vars: &HashMap<String, String>,
    dry_run: bool,
    log: &mut Vec<String>,
) -> Result<()> {
    let script_path = root.join(script);
    log.push(format!("Running script: {}", script_path.display()));

    if dry_run {
        log.push(format!(
            "[dry-run] would run: bash {}",
            script_path.display()
        ));
        return Ok(());
    }

    registry::run_script(&script_path.to_string_lossy(), env_vars, log).await
}

async fn run_login_script(
    script: &str,
    root: &Path,
    env_vars: &HashMap<String, String>,
    dry_run: bool,
    log: &mut Vec<String>,
) -> Result<()> {
    let script_path = root.join(script);
    log.push(format!("Running login script: {}", script_path.display()));

    if dry_run {
        log.push(format!(
            "[dry-run] would run: bash {}",
            script_path.display()
        ));
        return Ok(());
    }

    registry::run_script(&script_path.to_string_lossy(), env_vars, log).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stage_order_and_indices() {
        let idx: Vec<usize> = StageKind::ALL.iter().map(|s| s.index()).collect();
        assert_eq!(idx, vec![0, 1, 2, 3]);
    }

    #[test]
    fn stage_labels_are_mode_aware() {
        assert_eq!(StageKind::Login.label(RunMode::Push), "login");
        assert_eq!(StageKind::Pre.label(RunMode::Push), "pre-push");
        assert_eq!(StageKind::Pre.label(RunMode::Pull), "pre-pull");
        assert_eq!(StageKind::Main.label(RunMode::Push), "push");
        assert_eq!(StageKind::Main.label(RunMode::Pull), "pull");
        assert_eq!(StageKind::Post.label(RunMode::Pull), "post-pull");
        // Compact forms drop the direction suffix on pre/post.
        assert_eq!(StageKind::Pre.short(RunMode::Push), "pre");
        assert_eq!(StageKind::Post.short(RunMode::Pull), "post");
        assert_eq!(StageKind::Main.short(RunMode::Push), "push");
    }

    #[test]
    fn validate_skips_publish_path_when_main_overridden() {
        let vars = HashMap::new();

        // main override: publish_path placeholders are the user's concern.
        let cfg: CiabattaConfig = toml::from_str(
            r#"
[recipies.a]
registry = "nexus"
publish_path = "x/{MISSING_VAR}/y"
main = "echo hi"
"#,
        )
        .unwrap();
        assert!(validate_recipes(&cfg, &["a".to_string()], &vars, &RunMode::Push).is_ok());

        // built-in main: the missing variable must be caught up front.
        let cfg2: CiabattaConfig = toml::from_str(
            r#"
[recipies.a]
registry = "nexus"
publish_path = "x/{MISSING_VAR}/y"
"#,
        )
        .unwrap();
        assert!(validate_recipes(&cfg2, &["a".to_string()], &vars, &RunMode::Push).is_err());
    }
}
