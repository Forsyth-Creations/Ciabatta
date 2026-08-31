//! The Zed half of ciabatta's editor support.
//!
//! Zed extensions don't provide completions themselves — they wire up language
//! servers — which is why the interesting half of this feature lives in the
//! CLI as `ciabatta lsp` and is shared with the VS Code extension. All this
//! does is find the binary and hand Zed a command to run.
//!
//! It deliberately does not download anything. `ciabatta lsp` is the same
//! binary that runs the builds, and an editor quietly fetching a *second* copy
//! at some other version is how you end up with completions that disagree with
//! `ciabatta build`. If it isn't installed, the extension says so and points at
//! the install instructions.

use zed_extension_api::{self as zed, Command, LanguageServerId, Result, Worktree, settings};

/// The setting a project may use to name a specific binary, for a checkout
/// built from source or a version pinned per-repository:
///
/// ```json
/// { "lsp": { "ciabatta": { "binary": { "path": "./target/release/ciabatta" } } } }
/// ```
const SETTINGS_KEY: &str = "ciabatta";

struct CiabattaExtension;

impl zed::Extension for CiabattaExtension {
    fn new() -> Self {
        CiabattaExtension
    }

    fn language_server_command(
        &mut self,
        id: &LanguageServerId,
        worktree: &Worktree,
    ) -> Result<Command> {
        // An explicit `binary` setting wins: someone who has said which one to
        // run has a reason, usually that they are working on ciabatta itself.
        if let Ok(settings) = settings::LspSettings::for_worktree(SETTINGS_KEY, worktree)
            && let Some(binary) = settings.binary
            && let Some(path) = binary.path
        {
            return Ok(Command {
                command: path,
                args: binary.arguments.unwrap_or_else(|| vec!["lsp".into()]),
                env: worktree.shell_env(),
            });
        }

        let Some(path) = worktree.which("ciabatta") else {
            return Err(format!(
                "{id} needs the `ciabatta` CLI on your PATH.\n\
                 Install it with `cargo install ciabatta`, or set \
                 lsp.ciabatta.binary.path to a build of your own.\n\
                 Field completion from the JSON Schema works without it.",
                id = id.as_ref(),
            ));
        };

        Ok(Command {
            command: path,
            args: vec!["lsp".into()],
            env: worktree.shell_env(),
        })
    }
}

zed::register_extension!(CiabattaExtension);
