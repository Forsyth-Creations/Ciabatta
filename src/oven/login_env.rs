//! The environment the executor's user gets when they log in, for its jobs.
//!
//! An executor is usually started by systemd, launchd, cron or `nohup`, and
//! none of those read `~/.profile` or `~/.bashrc`. So the toolchains a person
//! installs for themselves — rustup's `~/.cargo/bin`, nvm, pyenv, Homebrew's
//! `/opt/homebrew/bin` — are on their terminal's `PATH` and missing from every
//! job's: `cargo: not found` on a machine where `cargo` plainly works.
//!
//! So before each job the executor asks the user's login shell, started
//! interactively as a terminal would start it, what its environment is, and
//! runs the job's git and its workflow with that. Asking per job, rather than
//! once at start-up, means a tool installed since doesn't need the executor
//! restarted to be found.
//!
//! Best-effort: a shell that is missing, fails, or takes longer than
//! [`TIMEOUT`] leaves the job with the executor's own environment, and says so.

// Only unix asks a login shell; elsewhere the helpers below go unused (but
// stay, so the parser's tests run on every platform).
#![cfg_attr(not(unix), allow(dead_code))]

use std::collections::HashMap;
use std::time::Duration;

/// The longest a login shell may take to report its environment. Generous,
/// for a `.bashrc` that loads nvm or conda; short enough that one which waits
/// for input can't hold a job up for long.
pub const TIMEOUT: Duration = Duration::from_secs(10);

/// Variables that describe the shell that was asked rather than the user's
/// setup, and that a job is better off without.
const SKIPPED: &[&str] = &["PWD", "OLDPWD", "SHLVL", "_", "PS1", "PS2", "TERM"];

const START: &str = "__CIABATTA_LOGIN_ENV_START__";
const END: &str = "__CIABATTA_LOGIN_ENV_END__";

/// What came of asking.
pub struct LoginEnv {
    /// The shell that was asked.
    pub shell: String,
    /// The variables to run jobs with, or why there are none.
    pub vars: Result<HashMap<String, String>, String>,
}

/// The executor user's login environment: their shell, started as a login,
/// interactive shell (`-l -i`), so it reads `/etc/profile`, `~/.profile` or
/// `~/.bash_profile`, and `~/.bashrc` — or zsh's equivalents.
#[cfg(unix)]
pub async fn load() -> LoginEnv {
    let shell = user_shell();
    let vars = ask(&shell).await;
    LoginEnv { shell, vars }
}

#[cfg(not(unix))]
pub async fn load() -> LoginEnv {
    LoginEnv {
        shell: String::new(),
        vars: Err("not loaded on this platform".to_string()),
    }
}

/// `$SHELL`, else the user's shell from the password database (a systemd
/// service often has no `$SHELL`), else `/bin/sh`.
#[cfg(unix)]
fn user_shell() -> String {
    if let Ok(shell) = std::env::var("SHELL")
        && !shell.trim().is_empty()
    {
        return shell;
    }
    // SAFETY: getpwuid returns a pointer into static storage or null; the
    // string is copied out before anything else could call it again.
    unsafe {
        let entry = libc::getpwuid(libc::getuid());
        if !entry.is_null() && !(*entry).pw_shell.is_null() {
            let shell = std::ffi::CStr::from_ptr((*entry).pw_shell)
                .to_string_lossy()
                .into_owned();
            if !shell.trim().is_empty() {
                return shell;
            }
        }
    }
    "/bin/sh".to_string()
}

#[cfg(unix)]
async fn ask(shell: &str) -> Result<HashMap<String, String>, String> {
    // NUL-separated, so a value with a newline in it survives; between
    // markers, so whatever an rc file prints on its own is left out.
    let script = format!("printf '\\0{START}\\0'; env -0; printf '\\0{END}\\0'");
    let mut cmd = tokio::process::Command::new(shell);
    cmd.args(["-l", "-i", "-c", &script])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    // SAFETY: `setsid` is async-signal-safe, and it is the only call made
    // between fork and exec. Without a terminal of its own, an interactive
    // shell can't take over the executor's.
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let output = match tokio::time::timeout(TIMEOUT, cmd.output()).await {
        Err(_) => return Err(format!("no answer within {}s", TIMEOUT.as_secs())),
        Ok(Err(e)) => return Err(format!("couldn't start it: {e}")),
        Ok(Ok(output)) => output,
    };
    parse(&output.stdout).ok_or_else(|| match output.status.code() {
        Some(code) => format!("it exited {code} without printing its environment"),
        None => "it was stopped before printing its environment".to_string(),
    })
}

/// The variables between the markers in `out`, less [`SKIPPED`].
fn parse(out: &[u8]) -> Option<HashMap<String, String>> {
    let text = String::from_utf8_lossy(out);
    let mut fields = text.split('\0');
    fields.find(|f| *f == START)?;
    let mut vars = HashMap::new();
    for field in fields {
        if field == END {
            return Some(vars);
        }
        if let Some((name, value)) = field.split_once('=')
            && !name.is_empty()
            && !SKIPPED.contains(&name)
        {
            vars.insert(name.to_string(), value.to_string());
        }
    }
    // No end marker: the output was cut short, so don't trust it.
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_whats_between_the_markers_counts() {
        let out = format!(
            "Welcome!\n\0{START}\0PATH=/home/me/.cargo/bin:/usr/bin\0MULTI=a\nb\0PWD=/tmp\0\
             {END}\0bye\n"
        );
        let vars = parse(out.as_bytes()).unwrap();
        assert_eq!(vars["PATH"], "/home/me/.cargo/bin:/usr/bin");
        assert_eq!(vars["MULTI"], "a\nb");
        assert!(!vars.contains_key("PWD"));
        assert_eq!(vars.len(), 2);
    }

    #[test]
    fn output_cut_short_is_not_trusted() {
        assert!(parse(format!("\0{START}\0PATH=/usr/bin\0").as_bytes()).is_none());
        assert!(parse(b"PATH=/usr/bin\0").is_none());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_login_shell_reports_what_its_profile_sets() {
        // A scratch HOME whose ~/.profile adds a directory, as rustup's
        // installer does — and sh started as a login shell reads it.
        let home = std::env::temp_dir().join(format!("ciabatta-login-env-{}", std::process::id()));
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(
            home.join(".profile"),
            "export PATH=\"$HOME/.cargo/bin:$PATH\"\nexport FROM_PROFILE=yes\n",
        )
        .unwrap();
        let mut cmd = tokio::process::Command::new("/bin/sh");
        cmd.env("HOME", &home);
        let script = format!("printf '\\0{START}\\0'; env -0; printf '\\0{END}\\0'");
        let out = cmd
            .args(["-l", "-c", &script])
            .stdin(std::process::Stdio::null())
            .output()
            .await
            .unwrap();
        let vars = parse(&out.stdout).expect("the shell's environment");
        assert_eq!(vars["FROM_PROFILE"], "yes");
        assert!(
            vars["PATH"].starts_with(&format!("{}/.cargo/bin:", home.display())),
            "{}",
            vars["PATH"]
        );
        let _ = std::fs::remove_dir_all(&home);
    }
}
