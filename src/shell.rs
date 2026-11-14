//! Which shell runs a step.
//!
//! Every `run:` and `script:` step is a POSIX shell command, so on Linux and
//! macOS it goes to `sh`, which is always there. Windows has no `sh`, and the
//! obvious substitute, "whatever `bash` is on `PATH`", is usually wrong: Git
//! for Windows only puts `Git\cmd` on `PATH` (no shell in it), and the `bash`
//! that *is* often on `PATH` is `System32\bash.exe`, WSL's launcher, which
//! runs the step inside a Linux VM that can't see the Windows tools it needs.
//!
//! So on Windows the shell is looked for, in order:
//!
//! 1. `CIABATTA_SHELL`, when set: the user's word is final.
//! 2. `git\bin\bash.exe` beside `ciabatta.exe`: the PortableGit the installer
//!    downloads when there's no Git Bash on the machine.
//! 3. The Git Bash that belongs to the `git.exe` on `PATH`.
//! 4. Git for Windows' standard install locations.
//! 5. Any other `bash.exe` / `sh.exe` on `PATH`, except WSL's.
//!
//! and if none turns up, the step fails with an error that says how to get one
//! rather than "program not found".

use anyhow::{Result, bail};
use std::collections::HashMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Overrides the shell on every platform.
pub const OVERRIDE_VAR: &str = "CIABATTA_SHELL";

/// What the shell is spawned as: a bare name to look up on `PATH`, or a file.
pub fn program() -> Result<PathBuf> {
    static FOUND: OnceLock<Option<PathBuf>> = OnceLock::new();
    let found = FOUND.get_or_init(|| {
        let probe = Probe::from_env();
        if cfg!(windows) {
            find_windows(&probe)
        } else {
            Some(probe.overridden.unwrap_or_else(|| PathBuf::from("sh")))
        }
    });
    match found {
        Some(shell) => Ok(shell.clone()),
        None => bail!(
            "ciabatta runs each step with Git Bash on Windows, and couldn't find it.\n\
             Fix it with one of:\n  \
             • winget install --id Git.Git -e        (then open a new terminal)\n  \
             • rerun the ciabatta installer, which offers to fetch a private copy\n  \
             • set {OVERRIDE_VAR} to the full path of a bash.exe"
        ),
    }
}

/// The `PATH` a step's shell should run with, as `(name, value)`, or `None`
/// to leave the step's environment as it is.
///
/// Only Windows needs this. A `script:` step is `bash <file>` and a step can
/// call `grep` or `sed`; inside a Git Bash those have to be Git's, ahead of
/// WSL's `bash.exe` and Windows' own `find.exe` and `sort.exe` — the order an
/// interactive Git Bash puts them in. The name the step's environment already
/// uses for the variable (`Path`, on Windows) is kept, so it doesn't end up with
/// two.
pub fn child_path(shell: &Path, env: &HashMap<String, String>) -> Option<(String, String)> {
    if !cfg!(windows) {
        return None;
    }
    let dirs = shell_dirs(shell);
    if dirs.is_empty() {
        return None;
    }
    let (key, current) = match env.keys().find(|k| k.eq_ignore_ascii_case("PATH")) {
        Some(key) => (key.clone(), OsString::from(&env[key])),
        None => (
            "PATH".to_string(),
            std::env::var_os("PATH").unwrap_or_default(),
        ),
    };
    let rest: Vec<PathBuf> = std::env::split_paths(&current).collect();
    let joined = std::env::join_paths(dirs.into_iter().chain(rest)).ok()?;
    Some((key, joined.to_string_lossy().into_owned()))
}

/// The directories to put ahead of `PATH` for `shell`: Git's `usr\bin` (the
/// Unix tools) and `bin` (bash, sh, git) when it's a Git Bash, otherwise just
/// the directory it lives in.
fn shell_dirs(shell: &Path) -> Vec<PathBuf> {
    let Some(dir) = shell.parent().filter(|d| !d.as_os_str().is_empty()) else {
        return Vec::new();
    };
    if let Some(root) = git_root(dir) {
        let tools = root.join("usr").join("bin");
        let bin = root.join("bin");
        return [tools, bin].into_iter().filter(|d| d.is_dir()).collect();
    }
    vec![dir.to_path_buf()]
}

/// The top of a Git for Windows install that `dir` (`<root>\bin` or
/// `<root>\usr\bin`) belongs to.
fn git_root(dir: &Path) -> Option<PathBuf> {
    let is = |p: &Path, name: &str| {
        p.file_name()
            .is_some_and(|n| n.to_string_lossy().eq_ignore_ascii_case(name))
    };
    let mut root = dir.parent()?;
    if is(dir, "bin") && is(root, "usr") {
        root = root.parent()?;
    } else if !is(dir, "bin") {
        return None;
    }
    root.join("usr")
        .join("bin")
        .join("bash.exe")
        .is_file()
        .then(|| root.to_path_buf())
}

/// Everything [`find_windows`] looks at, gathered up front so tests can hand
/// it a made-up machine.
#[derive(Default)]
struct Probe {
    overridden: Option<PathBuf>,
    /// Where `ciabatta.exe` is.
    exe_dir: Option<PathBuf>,
    /// The directories on `PATH`, in order.
    path: Vec<PathBuf>,
    /// Git for Windows' default install directories (`...\Git`).
    install_roots: Vec<PathBuf>,
    /// `%SystemRoot%`, under which `bash.exe` is WSL's.
    system_root: Option<PathBuf>,
}

impl Probe {
    fn from_env() -> Self {
        let var = |name: &str| {
            std::env::var_os(name)
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
        };
        let install_roots = [
            var("ProgramFiles").map(|p| p.join("Git")),
            var("ProgramW6432").map(|p| p.join("Git")),
            var("ProgramFiles(x86)").map(|p| p.join("Git")),
            var("LOCALAPPDATA").map(|p| p.join("Programs").join("Git")),
        ]
        .into_iter()
        .flatten()
        .collect();
        Self {
            overridden: var(OVERRIDE_VAR),
            exe_dir: std::env::current_exe()
                .ok()
                .and_then(|exe| exe.parent().map(Path::to_path_buf)),
            path: std::env::var_os("PATH")
                .map(|p| std::env::split_paths(&p).collect())
                .unwrap_or_default(),
            install_roots,
            system_root: var("SystemRoot"),
        }
    }
}

fn find_windows(probe: &Probe) -> Option<PathBuf> {
    if let Some(shell) = &probe.overridden {
        return Some(shell.clone());
    }
    let bash_in = |root: &Path| Some(root.join("bin").join("bash.exe")).filter(|b| b.is_file());

    // A private PortableGit beside ciabatta.exe.
    if let Some(found) = probe
        .exe_dir
        .as_deref()
        .and_then(|d| bash_in(&d.join("git")))
    {
        return Some(found);
    }

    // The Git Bash that goes with the `git` on PATH. git.exe sits in `cmd\`,
    // `bin\` or `mingw64\bin\` under the install's root, so the root is one or
    // two levels up.
    for dir in &probe.path {
        if !dir.join("git.exe").is_file() {
            continue;
        }
        if let Some(found) = dir.ancestors().skip(1).take(2).find_map(bash_in) {
            return Some(found);
        }
    }

    if let Some(found) = probe.install_roots.iter().find_map(|r| bash_in(r)) {
        return Some(found);
    }

    // Any other bash or sh on PATH — but not WSL's launcher, nor the Store's
    // app-execution aliases, which open an installer prompt instead of a shell.
    let is_under = |path: &Path, root: &Path| {
        let lower = |p: &Path| p.to_string_lossy().to_lowercase().replace('/', "\\");
        lower(path).starts_with(&lower(root))
    };
    probe
        .path
        .iter()
        .filter(|dir| {
            !probe
                .system_root
                .as_deref()
                .is_some_and(|r| is_under(dir, r))
        })
        .filter(|dir| !dir.to_string_lossy().to_lowercase().contains("windowsapps"))
        .flat_map(|dir| [dir.join("bash.exe"), dir.join("sh.exe")])
        .find(|candidate| candidate.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch directory with the given files created (empty) inside it.
    struct Machine(PathBuf);

    impl Machine {
        fn new(files: &[&str]) -> Self {
            static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
            let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let root =
                std::env::temp_dir().join(format!("ciabatta-shell-{}-{n}", std::process::id()));
            for file in files {
                let path = root.join(file);
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                std::fs::write(path, b"").unwrap();
            }
            Self(root)
        }

        fn at(&self, rel: &str) -> PathBuf {
            self.0.join(rel)
        }
    }

    impl Drop for Machine {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn the_override_wins_even_when_it_names_nothing_found() {
        let probe = Probe {
            overridden: Some(PathBuf::from("my-bash")),
            ..Probe::default()
        };
        assert_eq!(find_windows(&probe), Some(PathBuf::from("my-bash")));
    }

    #[test]
    fn a_private_copy_beside_ciabatta_comes_before_the_system_git() {
        let m = Machine::new(&[
            "ciabatta/git/bin/bash.exe",
            "Git/cmd/git.exe",
            "Git/bin/bash.exe",
        ]);
        let probe = Probe {
            exe_dir: Some(m.at("ciabatta")),
            path: vec![m.at("Git/cmd")],
            ..Probe::default()
        };
        assert_eq!(
            find_windows(&probe),
            Some(m.at("ciabatta/git/bin/bash.exe"))
        );
    }

    #[test]
    fn git_on_path_leads_to_its_own_bash() {
        for git_dir in ["Git/cmd", "Git/bin", "Git/mingw64/bin"] {
            let m = Machine::new(&[&format!("{git_dir}/git.exe"), "Git/bin/bash.exe"]);
            let probe = Probe {
                path: vec![m.at("elsewhere"), m.at(git_dir)],
                ..Probe::default()
            };
            assert_eq!(
                find_windows(&probe),
                Some(m.at("Git/bin/bash.exe")),
                "git.exe in {git_dir}"
            );
        }
    }

    #[test]
    fn the_standard_install_is_found_without_git_on_path() {
        let m = Machine::new(&["Program Files/Git/bin/bash.exe"]);
        let probe = Probe {
            install_roots: vec![m.at("Program Files/Git"), m.at("missing/Git")],
            ..Probe::default()
        };
        assert_eq!(
            find_windows(&probe),
            Some(m.at("Program Files/Git/bin/bash.exe"))
        );
    }

    #[test]
    fn wsl_bash_is_never_picked() {
        let m = Machine::new(&["Windows/System32/bash.exe", "msys64/usr/bin/sh.exe"]);
        let probe = Probe {
            path: vec![m.at("Windows/System32"), m.at("msys64/usr/bin")],
            system_root: Some(m.at("Windows")),
            ..Probe::default()
        };
        assert_eq!(find_windows(&probe), Some(m.at("msys64/usr/bin/sh.exe")));

        let only_wsl = Probe {
            path: vec![m.at("Windows/System32")],
            system_root: Some(m.at("Windows")),
            ..Probe::default()
        };
        assert_eq!(find_windows(&only_wsl), None);
    }

    #[test]
    fn a_git_bash_contributes_its_unix_tools_and_its_bin() {
        let m = Machine::new(&["Git/bin/bash.exe", "Git/usr/bin/bash.exe"]);
        let expected = vec![m.at("Git/usr/bin"), m.at("Git/bin")];
        assert_eq!(shell_dirs(&m.at("Git/bin/bash.exe")), expected);
        assert_eq!(shell_dirs(&m.at("Git/usr/bin/bash.exe")), expected);
    }

    #[test]
    fn any_other_shell_contributes_its_own_directory() {
        let m = Machine::new(&["tools/bin/bash.exe"]);
        assert_eq!(
            shell_dirs(&m.at("tools/bin/bash.exe")),
            vec![m.at("tools/bin")]
        );
        assert!(shell_dirs(Path::new("sh")).is_empty());
    }
}
