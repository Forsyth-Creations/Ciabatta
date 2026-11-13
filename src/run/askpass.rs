//! Password prompts from the tools a step runs: `sudo`, `ssh`, `git`.
//!
//! A step has no terminal (see [`crate::registry::process`]), and these tools
//! read a password from the terminal, not from stdin — so under ciabatta they
//! used to fail outright ("a terminal is required to authenticate"). Each of
//! them can instead ask a helper program, named by an environment variable,
//! and print whatever it prints as the password. So every run writes one:
//!
//! * **`askpass`** hands the tool's prompt to [`super::input`] — the same
//!   question `ciabatta input --secret` asks, so it appears wherever the run is
//!   being watched: under the run in a terminal, as the banner in the TUI, as a
//!   dialog in the web app. `SUDO_ASKPASS`, `SSH_ASKPASS` and `GIT_ASKPASS`
//!   point at it.
//! * **`sudo`**, first on the step's `PATH`, runs the real `sudo` with `-A`.
//!   `sudo` only uses its helper when asked to, and a workflow's `sudo apt
//!   install …` shouldn't have to be rewritten to run under ciabatta.
//!
//! Both live in a private directory made for the run and removed after it. The
//! broker's address and token are written into the helper rather than read from
//! the environment, so it still reaches the run when the tool calling it has
//! cleaned out its own environment first.
//!
//! A run with nobody to ask (CI) settles the question at once, so a password
//! prompt there fails the step just as it did before — and a passwordless
//! `sudo` never calls the helper at all. Unix only: on Windows none of these
//! tools prompt this way.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// The variables that name a password helper, and the value each is set to
/// beyond the helper's path. A step (or the shell ciabatta was started from)
/// that already sets one keeps its own.
const HELPER_VARS: &[&str] = &["SUDO_ASKPASS", "SSH_ASKPASS", "GIT_ASKPASS"];

/// The scripts for one run, removed when dropped.
#[derive(Debug)]
pub struct Helpers {
    dir: PathBuf,
}

impl Drop for Helpers {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

impl Helpers {
    /// Write the helpers for a run whose broker listens on `addr`, accepting
    /// `token`. `exe` is the ciabatta that asks the question.
    #[cfg(unix)]
    pub fn write(addr: SocketAddr, token: &str, exe: &Path) -> Result<Self> {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};

        // A name of its own, not one taken from the token: anybody can list
        // the temporary directory.
        let name: String = {
            use rand::Rng;
            let bytes: [u8; 8] = rand::thread_rng().r#gen();
            bytes.iter().map(|b| format!("{b:02x}")).collect()
        };
        let dir = std::env::temp_dir().join(format!("ciabatta-askpass-{name}"));
        // 0700: the helper carries the broker's token, which is what stops
        // another local process from answering on the run's behalf.
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&dir)
            .with_context(|| format!("Failed to create {}", dir.display()))?;
        let helpers = Helpers { dir };

        let askpass = helpers.askpass();
        let sudo = helpers.dir.join("sudo");
        for (path, body) in [
            (&askpass, askpass_script(addr, token, exe)),
            (&sudo, sudo_script(&helpers.dir)),
        ] {
            std::fs::write(path, body)
                .with_context(|| format!("Failed to write {}", path.display()))?;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
        }
        Ok(helpers)
    }

    #[cfg(not(unix))]
    pub fn write(_addr: SocketAddr, _token: &str, _exe: &Path) -> Result<Self> {
        anyhow::bail!("password helpers are only written on Unix")
    }

    /// The directory to put first on a step's `PATH`, for the `sudo` wrapper.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn askpass(&self) -> PathBuf {
        self.dir.join("askpass")
    }

    /// Point the helper variables at `askpass` in `env`, leaving any that are
    /// already set (there or in ciabatta's own environment) as they were.
    pub fn extend(&self, env: &mut HashMap<String, String>) {
        let askpass = self.askpass().to_string_lossy().into_owned();
        for var in HELPER_VARS {
            let set = env.get(*var).is_some_and(|v| !v.is_empty())
                || std::env::var_os(var).is_some_and(|v| !v.is_empty());
            if !set {
                env.insert(var.to_string(), askpass.clone());
            }
        }
        // ssh uses its helper only with no terminal *and* a display, unless
        // told otherwise; a step has neither, and still has a run to ask.
        if !env.contains_key("SSH_ASKPASS_REQUIRE")
            && std::env::var_os("SSH_ASKPASS_REQUIRE").is_none()
        {
            env.insert("SSH_ASKPASS_REQUIRE".to_string(), "force".to_string());
        }
    }
}

/// Single-quote `value` for `sh`.
fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', r"'\''"))
}

/// The helper itself: the tool's prompt is its first argument, and the answer
/// is what it prints. A yes/no question (ssh's "continue connecting?") is
/// asked as one, not masked like a password.
fn askpass_script(addr: SocketAddr, token: &str, exe: &Path) -> String {
    let exe = quote(&exe.to_string_lossy());
    format!(
        r#"#!/bin/sh
# Written by ciabatta for one run: asks for a password through the run, so the
# question appears wherever it's being watched. Removed when the run ends.
CIABATTA_INPUT_ADDR={addr}
CIABATTA_INPUT_TOKEN={token}
export CIABATTA_INPUT_ADDR CIABATTA_INPUT_TOKEN
prompt=$(printf '%s' "${{1:-Password:}}" | tr '\n' ' ')
case $prompt in
  *"(yes/no"*) {exe} input --yes-no -- "$prompt" | sed 's/^y$/yes/; s/^n$/no/' ;;
  *) exec {exe} input --secret -- "$prompt" ;;
esac
"#,
        token = quote(token),
    )
}

/// A `sudo` that is the real one with `-A`, so it asks [`askpass_script`] for
/// its password instead of a terminal it doesn't have.
///
/// Left as it was when it already says how to get a password, or that it
/// mustn't ask for one: `-A`, `-S` (stdin), `-n` (never prompt). Also for
/// `-V`/`--version` and `--help`, which ask for nothing.
fn sudo_script(dir: &Path) -> String {
    let dir = quote(&dir.to_string_lossy());
    format!(
        r#"#!/bin/sh
# Written by ciabatta for one run: the real sudo, told to ask for its password
# through the run (-A) since a step has no terminal. Removed when the run ends.
here={dir}
real=
IFS=:
for d in $PATH; do
  [ "$d" = "$here" ] && continue
  if [ -x "$d/sudo" ]; then real=$d/sudo; break; fi
done
unset IFS
if [ -z "$real" ]; then
  echo "sudo: command not found" >&2
  exit 127
fi
# Look through sudo's own options, up to the command it runs: an option that
# takes a value has it next, and `--` or the first non-option ends them.
skip=
for arg in "$@"; do
  if [ -n "$skip" ]; then skip=; continue; fi
  case $arg in
    --) break ;;
    --askpass|--stdin|--non-interactive|--version|--help) exec "$real" "$@" ;;
    --*=*) ;;
    --user|--group|--prompt|--chdir|--other-user|--close-from|--role|--type|--host) skip=1 ;;
    --*) ;;
    -*)
      case $arg in
        -*[ASnV]*) exec "$real" "$@" ;;
      esac
      case $arg in
        *[ugpDUCrth]) skip=1 ;;
      esac
      ;;
    *) break ;;
  esac
done
exec "$real" -A "$@"
"#
    )
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::process::Command;

    /// A fake `sudo` that prints its arguments, and the wrapper in front of it.
    fn wrapped() -> (tempdir::Dir, Helpers) {
        static FAKE: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = FAKE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let fake = tempdir::Dir::new(&format!("fake-sudo-{n}"));
        let sudo = fake.0.join("sudo");
        std::fs::write(&sudo, "#!/bin/sh\necho \"$*\"\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&sudo, std::fs::Permissions::from_mode(0o755)).unwrap();
        // A token of its own per test: the directory is named after it, and
        // the tests run side by side.
        static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let token = format!("{:08x}{:024x}", std::process::id(), n);
        let helpers = Helpers::write(
            "127.0.0.1:9".parse().unwrap(),
            &token,
            Path::new("/bin/false"),
        )
        .unwrap();
        (fake, helpers)
    }

    fn run_sudo(fake: &Path, helpers: &Helpers, args: &[&str]) -> String {
        let path = format!(
            "{}:{}:/usr/bin:/bin",
            helpers.dir().display(),
            fake.display()
        );
        let out = Command::new(helpers.dir().join("sudo"))
            .args(args)
            .env("PATH", path)
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    mod tempdir {
        pub struct Dir(pub std::path::PathBuf);
        impl Dir {
            pub fn new(name: &str) -> Self {
                let dir = std::env::temp_dir().join(format!(
                    "ciabatta-test-{name}-{}-{:?}",
                    std::process::id(),
                    std::thread::current().id()
                ));
                let _ = std::fs::remove_dir_all(&dir);
                std::fs::create_dir_all(&dir).unwrap();
                Dir(dir)
            }
        }
        impl Drop for Dir {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
    }

    #[test]
    fn sudo_is_told_to_use_the_helper() {
        let (fake, helpers) = wrapped();
        assert_eq!(
            run_sudo(&fake.0, &helpers, &["apt", "install", "-y", "jq"]),
            "-A apt install -y jq"
        );
        // An option's value isn't the command, and the command's own flags
        // aren't sudo's.
        assert_eq!(
            run_sudo(&fake.0, &helpers, &["-u", "root", "ls", "-n"]),
            "-A -u root ls -n"
        );
        assert_eq!(run_sudo(&fake.0, &helpers, &["-E", "make"]), "-A -E make");
    }

    #[test]
    fn sudo_that_already_knows_how_to_get_a_password_is_left_alone() {
        let (fake, helpers) = wrapped();
        for args in [
            &["-n", "true"][..],
            &["-S", "true"],
            &["-u", "root", "-S", "true"],
            &["-kn", "true"],
            &["--non-interactive", "true"],
            &["-A", "true"],
            &["-V"],
        ] {
            assert_eq!(
                run_sudo(&fake.0, &helpers, args),
                args.join(" "),
                "{args:?}"
            );
        }
    }

    #[test]
    fn a_variable_already_set_is_kept() {
        let (_fake, helpers) = wrapped();
        let mut env = HashMap::from([("GIT_ASKPASS".to_string(), "/usr/bin/mine".to_string())]);
        helpers.extend(&mut env);
        assert_eq!(env["GIT_ASKPASS"], "/usr/bin/mine");
        if std::env::var_os("SUDO_ASKPASS").is_none() {
            assert!(env["SUDO_ASKPASS"].ends_with("/askpass"));
        }
    }

    #[test]
    fn the_helpers_go_with_the_run() {
        let (_fake, helpers) = wrapped();
        let dir = helpers.dir().to_path_buf();
        assert!(dir.join("askpass").is_file());
        drop(helpers);
        assert!(!dir.exists());
    }
}
