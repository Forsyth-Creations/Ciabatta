//! The shell integration that actually sources/pops a workspace's env vars.
//!
//! No process — daemon or otherwise — can inject or remove environment
//! variables from an already-running parent shell; that's an OS-level
//! limitation, not a ciabatta one (a child process never mutates its
//! parent's `environ`). So, like direnv/asdf/mise, the mechanism is:
//!
//!   1. `ciabatta daemon init <shell>` prints a small function wired to run
//!      on every prompt (bash `PROMPT_COMMAND`, zsh `precmd_functions`, fish
//!      `fish_prompt` event). Add its output to your shell rc file once.
//!   2. That function calls `ciabatta daemon hook <shell>` and `eval`s
//!      whatever it prints.
//!   3. [`run`] does the actual work: find the nearest workspace root,
//!      compare it to the `CIABATTA_DAEMON_ROOT` bookkeeping variable already
//!      in the shell, and print an `unset`/`export` script that pops the
//!      previous workspace's variables and sources the new one's — or prints
//!      nothing at all when nothing has changed, since this runs on *every*
//!      prompt and has to be cheap.
//!
//! `ciabatta daemon export` uses the same resolution + quoting logic for a
//! one-shot `eval "$(ciabatta daemon export)"`, without needing the hook
//! wired up at all — mirroring the existing `ciabatta source` command.

use std::path::Path;

use anyhow::Result;

use super::{binding, config, load_profile, resolve};

/// Bookkeeping variable holding the workspace root the hook last exported
/// variables for.
pub const ROOT_VAR: &str = "CIABATTA_DAEMON_ROOT";
/// Bookkeeping variable holding a `:`-joined list of keys the hook exported,
/// so the next run knows what to unset when leaving that workspace.
pub const KEYS_VAR: &str = "CIABATTA_DAEMON_KEYS";

/// Recognized shells for quoting/statement syntax.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shell {
    /// bash and zsh share POSIX-ish `export`/`unset` syntax.
    Posix,
    Fish,
}

impl Shell {
    pub fn parse(name: &str) -> Self {
        match name.to_ascii_lowercase().as_str() {
            "fish" => Shell::Fish,
            _ => Shell::Posix,
        }
    }

    fn export_line(self, key: &str, value: &str) -> String {
        match self {
            Shell::Posix => format!("export {key}={}", posix_quote(value)),
            Shell::Fish => format!("set -gx {key} {}", fish_quote(value)),
        }
    }

    fn unset_line(self, key: &str) -> String {
        match self {
            Shell::Posix => format!("unset {key}"),
            Shell::Fish => format!("set -e {key}"),
        }
    }
}

/// Single-quote `value` for bash/zsh, escaping embedded `'` the standard
/// POSIX way: close the quote, emit an escaped quote, reopen it.
fn posix_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

/// Single-quote `value` for fish, where `\` and `'` are the two characters
/// that need escaping inside a single-quoted string.
fn fish_quote(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for c in value.chars() {
        if c == '\\' || c == '\'' {
            escaped.push('\\');
        }
        escaped.push(c);
    }
    format!("'{escaped}'")
}

/// The rc-file snippet for `ciabatta daemon init <shell>`.
pub fn init_snippet(shell: Shell) -> String {
    match shell {
        Shell::Posix => {
            "\
_ciabatta_daemon_hook() {
  eval \"$(ciabatta daemon hook posix)\"
}
case \";${PROMPT_COMMAND:-};\" in
  *\";_ciabatta_daemon_hook;\"*) ;;
  *) PROMPT_COMMAND=\"_ciabatta_daemon_hook${PROMPT_COMMAND:+;$PROMPT_COMMAND}\" ;;
esac
if [ -n \"$ZSH_VERSION\" ]; then
  autoload -Uz add-zsh-hook
  add-zsh-hook precmd _ciabatta_daemon_hook
fi"
                .to_string()
        }
        Shell::Fish => "\
function _ciabatta_daemon_hook --on-event fish_prompt
  ciabatta daemon hook fish | source
end"
        .to_string(),
    }
}

/// The per-prompt hook logic: find the nearest workspace, diff against the
/// bookkeeping env vars already in the calling shell, and return the script
/// to `eval` (empty when nothing changed — this must be cheap since it runs
/// on every prompt).
pub fn run(shell: Shell, cwd: &Path) -> String {
    let new_root = config::find_root(cwd);
    let new_root_str = new_root.as_ref().map(|p| p.display().to_string());

    let prev_root = std::env::var(ROOT_VAR).ok();
    if prev_root == new_root_str {
        return String::new();
    }

    let mut lines = Vec::new();

    // Pop whatever the previous workspace exported.
    if let Ok(prev_keys) = std::env::var(KEYS_VAR) {
        for key in prev_keys.split(':').filter(|k| !k.is_empty()) {
            lines.push(shell.unset_line(key));
        }
    }

    match new_root.as_deref() {
        None => {
            lines.push(shell.unset_line(ROOT_VAR));
            lines.push(shell.unset_line(KEYS_VAR));
        }
        Some(root) => match export_for_workspace(shell, root) {
            Some(mut export_lines) => lines.append(&mut export_lines),
            None => {
                // No binding here, or it failed to resolve: still update
                // bookkeeping so we don't keep re-diffing on every prompt.
                lines.push(shell.export_line(ROOT_VAR, &new_root_str.unwrap()));
                lines.push(shell.unset_line(KEYS_VAR));
            }
        },
    }

    lines.join("\n")
}

/// Resolve `root`'s bound profile and build the export lines plus updated
/// bookkeeping, or `None` if there's no binding / it failed to resolve.
fn export_for_workspace(shell: Shell, root: &Path) -> Option<Vec<String>> {
    let bound = binding(root).ok().flatten()?;
    let profile_file = load_profile(root, &bound.profile).ok()?;
    let resolved = resolve(&profile_file.profile).ok()?;

    let mut lines: Vec<String> = resolved
        .iter()
        .map(|(k, v)| shell.export_line(k, v))
        .collect();

    let keys = resolved
        .iter()
        .map(|(k, _)| k.as_str())
        .collect::<Vec<_>>()
        .join(":");
    lines.push(shell.export_line(ROOT_VAR, &root.display().to_string()));
    lines.push(shell.export_line(KEYS_VAR, &keys));
    Some(lines)
}

/// `ciabatta daemon export [PROFILE]`: resolve `profile` (or the workspace's
/// bound profile when omitted) and print the same export script the hook
/// would, for `eval "$(ciabatta daemon export)"` without wiring up the hook.
pub fn export_script(root: &Path, profile: Option<&str>, shell: Shell) -> Result<String> {
    let profile_name = match profile {
        Some(p) => p.to_string(),
        None => binding(root)?
            .map(|b| b.profile)
            .ok_or_else(|| anyhow::anyhow!(
                "no profile bound here — run `ciabatta daemon use <profile>` or pass one explicitly"
            ))?,
    };

    let profile_file = load_profile(root, &profile_name)?;
    let resolved = resolve(&profile_file.profile)?;

    let mut lines: Vec<String> = resolved
        .iter()
        .map(|(k, v)| shell.export_line(k, v))
        .collect();
    let keys = resolved
        .iter()
        .map(|(k, _)| k.as_str())
        .collect::<Vec<_>>()
        .join(":");
    lines.push(shell.export_line(ROOT_VAR, &root.display().to_string()));
    lines.push(shell.export_line(KEYS_VAR, &keys));
    Ok(lines.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn posix_quoting_escapes_embedded_single_quotes() {
        assert_eq!(posix_quote("it's"), "'it'\\''s'");
        assert_eq!(Shell::Posix.export_line("FOO", "it's"), "export FOO='it'\\''s'");
    }

    #[test]
    fn fish_quoting_escapes_quotes_and_backslashes() {
        assert_eq!(fish_quote(r"a\b'c"), r"'a\\b\'c'");
    }

    #[test]
    fn no_op_when_no_bookkeeping_and_no_workspace_found() {
        // SAFETY: test-only; make sure a leftover value from another test
        // isn't set before asserting the true no-op case.
        unsafe {
            std::env::remove_var(ROOT_VAR);
            std::env::remove_var(KEYS_VAR);
        }
        // /tmp has no `.ciabatta` ancestor, and no bookkeeping var is set, so
        // both "previous root" and "current root" are `None` — nothing to do.
        let script = run(Shell::Posix, Path::new("/tmp"));
        assert!(script.is_empty());
    }

    #[test]
    fn leaving_every_workspace_clears_bookkeeping() {
        unsafe {
            std::env::set_var(ROOT_VAR, "/some/workspace");
            std::env::set_var(KEYS_VAR, "FOO:BAR");
        }
        let script = run(Shell::Posix, Path::new("/tmp"));
        assert!(script.contains("unset FOO"));
        assert!(script.contains("unset BAR"));
        assert!(script.contains(&format!("unset {ROOT_VAR}")));
        assert!(script.contains(&format!("unset {KEYS_VAR}")));
        unsafe {
            std::env::remove_var(ROOT_VAR);
            std::env::remove_var(KEYS_VAR);
        }
    }
}
