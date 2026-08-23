//! Which `.env` file a workspace uses, and where it comes from.
//!
//! Three rules, in the order they apply:
//!
//! 1. **`.env` is the default.** A workspace that says nothing gets `.env` from
//!    its own directory. Nobody should have to configure the conventional thing.
//!
//! 2. **`env_file` overrides it.** A workspace that keeps its environment
//!    somewhere else says so once, in its config, and that file replaces `.env`
//!    for everything in that workspace.
//!
//! 3. **`env_default` is where a missing `.env` comes from.** `.env` is
//!    gitignored, so a fresh checkout doesn't have one; the checked-in template
//!    does. Naming it means ciabatta can generate the `.env` rather than
//!    failing on a variable the developer has never heard of.
//!
//! And one requirement that follows from the third: **a workspace whose builds
//! depend on environment variables must declare `env_default`.** Not as
//! bureaucracy — as the thing that makes rule 3 possible. A repo where the
//! required variables are written down somewhere reviewable is a repo where a
//! new person can build it; a repo where they aren't is one where the answer
//! lives in somebody's shell history.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::workspace::WorkspaceMeta;

/// The file ciabatta uses when a workspace doesn't name one.
pub const DEFAULT_ENV_FILE: &str = ".env";

/// The conventional names for the checked-in template, tried in order when
/// looking for one a workspace forgot to declare.
pub const TEMPLATE_NAMES: &[&str] = &[
    ".env.default",
    ".env.example",
    ".env.sample",
    ".env.template",
];

/// Which env files a workspace sources, and where they came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    /// Paths to source, relative to the workspace directory, in order.
    pub files: Vec<String>,
    /// Whether these came from config or from the `.env` default.
    pub explicit: bool,
    /// The checked-in template, when the workspace declared one.
    pub template: Option<String>,
}

/// Work out which env files a workspace sources.
///
/// A configured `env_file` replaces the default outright rather than adding to
/// it — "use this instead of `.env`" is what people mean when they write it,
/// and silently sourcing both would make the override useless for the case it
/// exists for (keeping dev and prod settings in separate files).
pub fn resolve(meta: &WorkspaceMeta, dir: &Path) -> Resolved {
    if !meta.env_file.is_empty() {
        return Resolved {
            files: meta.env_file.clone(),
            explicit: true,
            template: meta.env_default.clone(),
        };
    }

    // The default is only sourced if it's actually there: a project with no
    // `.env` isn't misconfigured, it just doesn't use one.
    let files = if dir.join(DEFAULT_ENV_FILE).is_file() {
        vec![DEFAULT_ENV_FILE.to_string()]
    } else {
        Vec::new()
    };

    Resolved {
        files,
        explicit: false,
        template: meta.env_default.clone(),
    }
}

/// Whether a build depends on environment variables, and so needs a template.
///
/// Deliberately narrow: a run that declares `REQUIRED_ENV` has said, in its own
/// config, that it cannot run without those variables. That's the case where a
/// missing `.env` becomes a confusing failure several minutes into a build, and
/// so the case worth requiring a template for. A step that merely *reads* an
/// optional variable with a default is not that.
pub fn expects_env(required_env: &[String], meta: &WorkspaceMeta) -> bool {
    !required_env.is_empty() || !meta.env.is_empty() && !meta.env_file.is_empty()
}

/// Refuse a build that depends on environment variables from a workspace that
/// never wrote down where they come from.
///
/// The error names the file to create and what to put in it, because "specify
/// env_default" on its own would just move the confusion.
pub fn require_template(
    meta: &WorkspaceMeta,
    dir: &Path,
    required_env: &[String],
    workspace: &str,
) -> Result<()> {
    if !expects_env(required_env, meta) || meta.env_default.is_some() {
        return Ok(());
    }

    // If a conventional template is sitting right there, say so — the fix is
    // one line, and telling them which line is most of the help.
    let found = TEMPLATE_NAMES
        .iter()
        .find(|name| dir.join(name).is_file())
        .copied();

    let variables = if required_env.is_empty() {
        String::new()
    } else {
        format!(
            "\n\nThe variables it needs are: {}.",
            required_env.join(", ")
        )
    };

    match found {
        Some(template) => bail!(
            "'{workspace}' declares environment variables its build can't run without, \
             but its config doesn't say where they're documented.\n\n\
             {template} is right there — point at it:\n\n    \
             workspace:\n      env_default: {template}\n\
             {variables}"
        ),
        None => bail!(
            "'{workspace}' declares environment variables its build can't run without, \
             but there's no checked-in template saying what they are.\n\n\
             Create one (conventionally .env.default) listing each variable with a \
             safe placeholder, commit it, and point at it:\n\n    \
             workspace:\n      env_default: .env.default\n\n\
             `.env` itself stays gitignored — the template is what makes it \
             reproducible.{variables}"
        ),
    }
}

/// Generate the workspace's `.env` from its template, when the `.env` is
/// missing and a template was declared.
///
/// Returns the path written, or `None` when nothing needed doing. This is the
/// payoff for declaring `env_default`: a fresh checkout builds instead of
/// failing on a variable nobody mentioned.
pub fn generate_from_template(
    meta: &WorkspaceMeta,
    dir: &Path,
    target: &str,
) -> Result<Option<PathBuf>> {
    let Some(template) = meta.env_default.as_deref() else {
        return Ok(None);
    };

    let destination = dir.join(target);
    if destination.exists() {
        return Ok(None);
    }

    let source = dir.join(template);
    if !source.is_file() {
        bail!(
            "This workspace's env_default points at {}, which doesn't exist.\n\
             Either create it, or remove env_default from the config.",
            source.display()
        );
    }

    let content = std::fs::read_to_string(&source)
        .with_context(|| format!("Failed to read {}", source.display()))?;

    if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create {}", parent.display()))?;
    }
    std::fs::write(&destination, header(template) + &content)
        .with_context(|| format!("Failed to write {}", destination.display()))?;

    Ok(Some(destination))
}

/// The note at the top of a generated `.env`, saying where it came from.
///
/// Somebody is going to open this file, edit it, and then wonder whether their
/// edits will survive. They will — it's only generated when absent — and saying
/// so here is cheaper than them finding out the hard way.
fn header(template: &str) -> String {
    format!(
        "# Generated by ciabatta from {template} because this file was missing.\n\
         # Edit it freely: ciabatta only creates it when it isn't there.\n\
         # It is gitignored; {template} is the checked-in record of what's needed.\n\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ciab_envfiles_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn dot_env_is_the_default_and_only_when_it_exists() {
        let dir = scratch("default");
        let meta = WorkspaceMeta::default();

        // No `.env` on disk: a project without one isn't misconfigured.
        assert_eq!(resolve(&meta, &dir).files, Vec::<String>::new());

        std::fs::write(dir.join(".env"), "API_URL=http://localhost\n").unwrap();
        let resolved = resolve(&meta, &dir);
        assert_eq!(resolved.files, vec![".env".to_string()]);
        assert!(
            !resolved.explicit,
            "nobody configured this — it's the default"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_configured_env_file_replaces_the_default_rather_than_joining_it() {
        let dir = scratch("override");
        std::fs::write(dir.join(".env"), "FROM_DEFAULT=1\n").unwrap();
        std::fs::write(dir.join("config/dev.env"), "").ok();
        std::fs::create_dir_all(dir.join("config")).unwrap();
        std::fs::write(dir.join("config/dev.env"), "FROM_CONFIG=1\n").unwrap();

        let meta = WorkspaceMeta {
            env_file: vec!["config/dev.env".to_string()],
            ..Default::default()
        };
        let resolved = resolve(&meta, &dir);
        assert_eq!(
            resolved.files,
            vec!["config/dev.env".to_string()],
            "an override that also sourced .env would be useless for keeping \
             dev and prod settings apart"
        );
        assert!(resolved.explicit);

        // Several files are still allowed, layered in order.
        let meta = WorkspaceMeta {
            env_file: vec![".env".to_string(), "config/dev.env".to_string()],
            ..Default::default()
        };
        assert_eq!(resolve(&meta, &dir).files.len(), 2);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_build_that_needs_variables_must_say_where_they_are_documented() {
        let dir = scratch("require");
        let bare = WorkspaceMeta::default();

        // No REQUIRED_ENV → nothing to require.
        assert!(require_template(&bare, &dir, &[], "api").is_ok());

        // REQUIRED_ENV with no template → refused, with the fix spelled out.
        let err = require_template(&bare, &dir, &["API_URL".to_string()], "api")
            .unwrap_err()
            .to_string();
        assert!(err.contains("env_default: .env.default"), "got: {err}");
        assert!(err.contains("API_URL"), "the error must name the variables");

        // A template sitting right there gets pointed at by name.
        std::fs::write(dir.join(".env.example"), "API_URL=\n").unwrap();
        let err = require_template(&bare, &dir, &["API_URL".to_string()], "api")
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("env_default: .env.example"),
            "the fix should name the file that's already there: {err}"
        );

        // Declared → allowed.
        let declared = WorkspaceMeta {
            env_default: Some(".env.example".to_string()),
            ..Default::default()
        };
        assert!(require_template(&declared, &dir, &["API_URL".to_string()], "api").is_ok());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_env_is_generated_from_the_template_and_never_overwritten() {
        let dir = scratch("generate");
        std::fs::write(
            dir.join(".env.default"),
            "# What this service needs\nAPI_URL=http://localhost:8080\nAPI_TOKEN=\n",
        )
        .unwrap();

        let meta = WorkspaceMeta {
            env_default: Some(".env.default".to_string()),
            ..Default::default()
        };

        let written = generate_from_template(&meta, &dir, ".env").unwrap();
        assert_eq!(written, Some(dir.join(".env")));

        let generated = std::fs::read_to_string(dir.join(".env")).unwrap();
        assert!(generated.contains("API_URL=http://localhost:8080"));
        assert!(
            generated.contains("Generated by ciabatta from .env.default"),
            "somebody opening this file should be told where it came from"
        );

        // Edits survive: it's only ever created when absent.
        std::fs::write(dir.join(".env"), "API_URL=http://my-own-thing\n").unwrap();
        assert_eq!(generate_from_template(&meta, &dir, ".env").unwrap(), None);
        assert_eq!(
            std::fs::read_to_string(dir.join(".env")).unwrap(),
            "API_URL=http://my-own-thing\n"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_template_that_is_not_there_is_reported_rather_than_ignored() {
        let dir = scratch("missing_template");
        let meta = WorkspaceMeta {
            env_default: Some(".env.default".to_string()),
            ..Default::default()
        };
        let err = generate_from_template(&meta, &dir, ".env")
            .unwrap_err()
            .to_string();
        assert!(err.contains("env_default points at"), "got: {err}");

        // A workspace that declared no template simply does nothing.
        assert_eq!(
            generate_from_template(&WorkspaceMeta::default(), &dir, ".env").unwrap(),
            None
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
