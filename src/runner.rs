use anyhow::Result;
use std::collections::HashMap;
use std::path::Path;
use tokio::sync::mpsc;

use crate::config::{CiabattaConfig, SimpleRecipe, substitute_vars, validate_publish_path};
use crate::registry::{self, RegistryOpOptions};

#[derive(Debug, Clone)]
pub enum ProgressUpdate {
    Started(String),
    Log(String, String),
    Completed(String),
    Failed(String, String),
}

#[derive(Clone)]
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

        if let Some(ref path) = recipe.publish_path {
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
        let mode = mode.clone();

        let handle = tokio::spawn(async move {
            run_one(name, &config, &root, &env_vars, dry_run, &mode, tx).await
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
    mode: &RunMode,
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

async fn execute_recipe(
    name: &str,
    config: &CiabattaConfig,
    root: &Path,
    env_vars: &HashMap<String, String>,
    dry_run: bool,
    mode: &RunMode,
    tx: &mpsc::Sender<ProgressUpdate>,
) -> Result<()> {
    let entry = config
        .recipes
        .get(name)
        .ok_or_else(|| anyhow::anyhow!("Recipe '{}' not found", name))?;

    let recipe: &SimpleRecipe = match mode {
        RunMode::Push => entry.push_recipe(),
        RunMode::Pull => entry
            .pull_recipe()
            .ok_or_else(|| anyhow::anyhow!("Recipe '{}' has no pull action", name))?,
    };

    let mut log: Vec<String> = Vec::new();

    if let Some(ref script) = recipe.bash_script {
        run_bash_script(script, root, env_vars, dry_run, &mut log).await?;
    } else {
        run_registry_action(
            name, recipe, config, root, env_vars, dry_run, mode, &mut log,
        )
        .await?;
    }

    // Flush logs to channel
    for line in log {
        let _ = tx.send(ProgressUpdate::Log(name.to_string(), line)).await;
    }

    Ok(())
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

#[allow(clippy::too_many_arguments)]
async fn run_registry_action(
    name: &str,
    recipe: &SimpleRecipe,
    config: &CiabattaConfig,
    root: &Path,
    env_vars: &HashMap<String, String>,
    dry_run: bool,
    mode: &RunMode,
    log: &mut Vec<String>,
) -> Result<()> {
    let registry_name = recipe.registry.as_deref().ok_or_else(|| {
        anyhow::anyhow!("Recipe '{}' has no registry or bash_script defined", name)
    })?;

    let registry_config = config
        .registries
        .get(registry_name)
        .ok_or_else(|| anyhow::anyhow!("Registry '{}' not found in config", registry_name))?;

    let publish_path = recipe
        .publish_path
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("Recipe '{}' has no publish_path", name))?;

    let resolved_path = substitute_vars(publish_path, env_vars)?;

    let local_artifact = recipe.local_artifact_path.as_deref().unwrap_or(".");
    let local_path = root.join(local_artifact);

    let container_cmd = config
        .system
        .as_ref()
        .and_then(|s| s.containers.as_deref())
        .unwrap_or("docker");

    let opts = RegistryOpOptions {
        registry_name,
        registry_config,
        local_path: &local_path,
        remote_path: &resolved_path,
        env_vars,
        dry_run,
        container_cmd,
    };

    match mode {
        RunMode::Push => registry::push(opts, log).await,
        RunMode::Pull => registry::pull(opts, log).await,
    }
}
