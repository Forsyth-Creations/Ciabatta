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
//! Rule 3 applies to the conventional templates too: a workspace with a
//! `.env.example` sitting in it and no `.env` gets one generated from it,
//! declared or not. Rule 1's reasoning is the same reasoning — nobody should
//! have to configure the conventional thing.
//!
//! And one requirement that follows from the third: **a build that depends on
//! environment variables must be able to get them from somewhere.** Not as
//! bureaucracy — as the thing that makes rule 3 possible. A repo where the
//! required variables are written down somewhere reviewable is a repo where a
//! new person can build it; a repo where they aren't is one where the answer
//! lives in somebody's shell history.
//!
//! **"Somewhere" means anywhere up the chain.** A sub-library does not have to
//! document a variable its parent already documents — that is what resolving
//! upwards means, and demanding a template from every package that reads a
//! shared `API_URL` would be asking the same question once per package, and
//! would make declaring that variable once impossible. So
//! [`unaccounted_for`] looks through every enclosing workspace's env files and
//! templates, and through the environment the command is running in, and only
//! objects to a variable that none of them provides. A plain project with
//! nothing above it has a chain of one, and [`require_template`] still holds it
//! to writing its variables down — there is nowhere else they could come from.

use std::collections::BTreeSet;
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

/// The checked-in template a workspace's `.env` comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Template {
    /// Path to the template, relative to the workspace directory.
    pub file: String,
    /// Whether the workspace declared it (`env_default`) or it was found by
    /// its conventional name.
    pub declared: bool,
}

/// The template a workspace's `.env` would be generated from.
///
/// A declared `env_default` wins; failing that, a conventional template that
/// is actually sitting there counts. Somebody who committed a `.env.example`
/// has already said what the variables are — making them also say it in the
/// config before ciabatta will use it is asking twice.
pub fn template_for(meta: &WorkspaceMeta, dir: &Path) -> Option<Template> {
    if let Some(file) = meta.env_default.clone() {
        return Some(Template {
            file,
            declared: true,
        });
    }
    TEMPLATE_NAMES
        .iter()
        .find(|name| dir.join(name).is_file())
        .map(|name| Template {
            file: (*name).to_string(),
            declared: false,
        })
}

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

/// Which of `required` nothing in the chain accounts for.
///
/// A sub-library does not have to document a variable its parent already
/// documents — that is what resolving upwards *means*, and demanding a template
/// from every package that reads a shared `API_URL` would be asking the same
/// question once per package. So a variable is accounted for when any of these
/// is true, anywhere in the chain from the monorepo root down to the member:
///
/// * it is set, and non-empty, in the environment this process is running in —
///   which is how CI supplies secrets, and how a developer overrides one for a
///   single command;
/// * it is named in an env file some enclosing workspace sources;
/// * it is listed in some enclosing workspace's checked-in template, which is
///   what makes a fresh checkout — where the gitignored `.env` does not exist
///   yet — able to build.
///
/// Anything left over is a variable a build has declared it cannot run without
/// and that nothing, anywhere, provides. That is worth refusing over: the
/// alternative is failing several minutes into a build on a variable nobody
/// mentioned.
pub fn unaccounted_for<'a>(layers: &[Layer<'_>], required: &'a [String]) -> Vec<&'a str> {
    let mut known: BTreeSet<String> = BTreeSet::new();

    for layer in layers {
        // Whatever this layer sources, plus whatever it documents. Both, not
        // either: the file is what makes it work here, the template is what
        // makes it work for the next person.
        let resolved = resolve(layer.meta, layer.dir);
        let files = resolved
            .files
            .iter()
            .cloned()
            .chain(template_for(layer.meta, layer.dir).map(|t| t.file));

        for file in files {
            let Ok(content) = std::fs::read_to_string(layer.dir.join(&file)) else {
                continue;
            };
            known.extend(
                crate::run::parse_env_content(&content)
                    .into_iter()
                    .map(|(key, _)| key),
            );
        }
    }

    required
        .iter()
        .map(String::as_str)
        .filter(|name| {
            if known.contains(*name) {
                return false;
            }
            // Set and non-empty. An empty value is the same as unset for a
            // variable a build says it cannot run without.
            !std::env::var(name).is_ok_and(|value| !value.trim().is_empty())
        })
        .collect()
}

/// Explain a variable nothing in the chain provides.
///
/// The point of the message is *where to put it*: a variable one package needs
/// belongs in that package, and one several packages need belongs in the
/// workspace above them — which is the decision the reader actually has to
/// make, and the one a bare "missing API_URL" leaves them to guess at.
pub fn describe_unaccounted(workspace: &str, rel: &str, missing: &[&str]) -> String {
    let list = missing.join(", ");
    let plural = if missing.len() == 1 { "it" } else { "them" };
    let own = format!("{rel}/.env");
    // The two paths line up, whatever the package is called.
    let width = own.chars().count().max(".env".len());
    format!(
        "'{workspace}' declares environment variable(s) its build can't run without, and \
         nothing provides {plural}: {list}.\n\n\
         Looked in this workspace and every one enclosing it — their `.env` files, their \
         checked-in templates, and the environment this command is running in.\n\n\
         Set {plural} in the environment, or write {plural} down where whoever needs \
         {plural} will find {plural}:\n\n    \
         {own:<width$}   just this package\n    \
         {root:<width$}   every package under the root\n\n\
         Commit a template beside it (conventionally `.env.default`, or name one with \
         `workspace.env_default`) so a fresh checkout — where `.env` is gitignored and \
         absent — knows what to fill in.",
        root = ".env",
    )
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
    let Some(template) = template_for(meta, dir) else {
        return Ok(None);
    };

    let destination = dir.join(target);
    if destination.exists() {
        return Ok(None);
    }

    let source = dir.join(&template.file);
    if !source.is_file() {
        // Only a declared template can be missing here — an undeclared one was
        // found by looking at the disk a moment ago. A pointer at a file that
        // isn't there is a config error worth naming; anything else is a race,
        // and a race is not worth failing a build over.
        if !template.declared {
            return Ok(None);
        }
        bail!(
            "This workspace's env_default points at {}, which doesn't exist.\n\
             Either create it, or remove env_default from the config.",
            source.display()
        );
    }
    let template = template.file;

    let content = std::fs::read_to_string(&source)
        .with_context(|| format!("Failed to read {}", source.display()))?;

    if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create {}", parent.display()))?;
    }
    std::fs::write(&destination, header(&template) + &content)
        .with_context(|| format!("Failed to write {}", destination.display()))?;

    Ok(Some(destination))
}

/// One layer of the chain a step resolves its environment through: a workspace
/// directory, and the config that says what it sources.
pub struct Layer<'a> {
    /// The workspace directory, relative to the run root (`.` for the root
    /// itself). Env file paths are reported joined onto this.
    pub rel: &'a str,
    /// Its absolute directory, for deciding whether the default `.env` is
    /// actually there.
    pub dir: &'a Path,
    pub meta: &'a WorkspaceMeta,
}

/// The env files a step resolves through, outermost first.
///
/// **Proximity, with fallback outward.** A step in `packages/api` reads
/// `packages/api/.env` — and, for anything that file doesn't set, the values
/// from the workspace above it, up to the monorepo root. Layered in that order
/// so the nearest file wins, which is what "this package's settings" has to
/// mean to be worth writing.
///
/// The chain is the *ancestry* of the step's own workspace and nothing else. A
/// sibling package's `.env` is not a fallback: two packages that need the same
/// variable say so in the workspace above them, or each says it for itself.
/// The chain is a *search path*, not a list of requirements: a level with no
/// `.env` simply falls through to the next, and a level whose file hasn't been
/// generated yet (see [`generate_from_template`]) is listed all the same,
/// because by the time the run sources it, it will be there.
pub fn chain(layers: &[Layer<'_>]) -> Vec<String> {
    let mut files: Vec<String> = Vec::new();
    for layer in layers {
        let mut found = resolve(layer.meta, layer.dir).files;
        // A workspace with a checked-in template but no `.env` yet still
        // belongs in the chain: a fresh checkout generates the file before it
        // sources anything, and leaving the level out would send its steps
        // looking one workspace too far up.
        if found.is_empty() && template_for(layer.meta, layer.dir).is_some() {
            found.push(target_file(layer.meta));
        }
        for file in found {
            let path = join_rel(layer.rel, &file);
            if !files.contains(&path) {
                files.push(path);
            }
        }
    }
    files
}

/// The file a workspace's `.env` is generated as: whatever `env_file` names
/// first, or the conventional `.env`.
pub fn target_file(meta: &WorkspaceMeta) -> String {
    meta.env_file
        .first()
        .cloned()
        .unwrap_or_else(|| DEFAULT_ENV_FILE.to_string())
}

/// Join a workspace-relative env path onto the workspace's own path, keeping
/// the root's `.` from turning into `./.env`.
pub fn join_rel(rel: &str, file: &str) -> String {
    if rel.is_empty() || rel == "." {
        file.to_string()
    } else {
        format!("{}/{}", rel.trim_end_matches('/'), file)
    }
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

    /// Rule 1's reasoning, applied to rule 3: somebody who committed a
    /// `.env.example` has already said what the variables are.
    #[test]
    fn a_conventional_template_is_used_without_being_declared() {
        let dir = scratch("conventional");
        std::fs::write(dir.join(".env.example"), "API_URL=http://localhost\n").unwrap();

        let bare = WorkspaceMeta::default();
        assert_eq!(
            template_for(&bare, &dir),
            Some(Template {
                file: ".env.example".to_string(),
                declared: false,
            })
        );

        let written = generate_from_template(&bare, &dir, ".env").unwrap();
        assert_eq!(written, Some(dir.join(".env")));
        let generated = std::fs::read_to_string(dir.join(".env")).unwrap();
        assert!(generated.contains("API_URL=http://localhost"));
        assert!(generated.contains("Generated by ciabatta from .env.example"));

        // A declared template still wins over whatever else is lying around.
        std::fs::write(dir.join(".env.default"), "API_URL=declared\n").unwrap();
        let declared = WorkspaceMeta {
            env_default: Some(".env.default".to_string()),
            ..Default::default()
        };
        assert_eq!(template_for(&declared, &dir).unwrap().file, ".env.default");

        // Nothing to copy from, nothing to do — not an error.
        let empty = scratch("conventional_empty");
        assert_eq!(template_for(&bare, &empty), None);
        assert_eq!(generate_from_template(&bare, &empty, ".env").unwrap(), None);

        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&empty);
    }

    /// The chain is the ancestry of a step's own workspace: nearest last, and a
    /// level with nothing to source simply isn't in it.
    #[test]
    fn a_chain_lists_each_level_that_has_something_to_source() {
        let root = scratch("chain");
        std::fs::create_dir_all(root.join("packages/api")).unwrap();
        std::fs::write(root.join(".env"), "SHARED=root\n").unwrap();
        std::fs::write(root.join("packages/api/.env"), "SHARED=api\n").unwrap();
        let api = root.join("packages/api");
        let middle = root.join("packages");
        std::fs::create_dir_all(&middle).unwrap();

        let bare = WorkspaceMeta::default();
        let files = chain(&[
            Layer {
                rel: ".",
                dir: &root,
                meta: &bare,
            },
            Layer {
                rel: "packages",
                dir: &middle,
                meta: &bare,
            },
            Layer {
                rel: "packages/api",
                dir: &api,
                meta: &bare,
            },
        ]);
        assert_eq!(
            files,
            vec![".env".to_string(), "packages/api/.env".to_string()],
            "`packages` has no `.env`, so it contributes nothing"
        );

        // A level with only a template still belongs in the chain: the run
        // generates the file before it sources anything.
        let fresh = scratch("chain_fresh");
        std::fs::create_dir_all(fresh.join("packages/api")).unwrap();
        std::fs::write(fresh.join("packages/api/.env.default"), "A=1\n").unwrap();
        let api = fresh.join("packages/api");
        assert_eq!(
            chain(&[Layer {
                rel: "packages/api",
                dir: &api,
                meta: &bare,
            }]),
            vec!["packages/api/.env".to_string()]
        );

        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&fresh);
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
