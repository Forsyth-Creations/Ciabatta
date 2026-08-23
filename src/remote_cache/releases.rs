//! Handing out ciabatta itself.
//!
//! A team on a shared cache is a team that already trusts one server and
//! already talks to it on every build. That makes it the obvious place to
//! answer "is everyone on the same ciabatta?" — so the server is pointed at the
//! binaries it wants people running, hashes them, and mentions the version in
//! every reply. A client on something older says so, once, and
//! `ciabatta self update` fetches the new build from the server it was already
//! talking to.
//!
//! The hash is the point, not the version string. An operator who rebuilds and
//! copies a new binary over the same path without bumping `version:` still gets
//! their team updated, because what's advertised is the content — which is also
//! what the client verifies after downloading, before it replaces anything.
//!
//! Nothing here updates anybody automatically. A build tool that swaps its own
//! binary out from under a running CI job is a bad build tool; this notices,
//! tells you, and waits to be asked.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

/// The binaries a server hands out.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct ReleaseConfig {
    /// The version these binaries are. Shown to clients; the hash is what
    /// actually decides whether an update is available.
    pub version: Option<String>,

    /// Platform → path to the binary on the server's filesystem.
    ///
    /// Keys are platform names as [`current_platform`] reports them: `linux`,
    /// `windows`, `macos`. A server may carry any subset.
    #[serde(default)]
    pub binaries: BTreeMap<String, PathBuf>,

    /// Notes shown alongside the update prompt — what changed, or a link.
    pub notes: Option<String>,
}

impl ReleaseConfig {
    /// Resolve relative binary paths against the config file's directory.
    pub fn resolve_relative(&mut self, base: &Path) {
        for path in self.binaries.values_mut() {
            if path.is_relative() {
                *path = base.join(&*path);
            }
        }
    }

    /// Hash every configured binary that's actually there.
    ///
    /// A configured-but-missing binary is a warning rather than an error: an
    /// operator who's set up all three platforms but has only copied Linux in
    /// so far should get a working server for Linux users, not a refusal.
    pub fn scan(&self) -> Release {
        let mut builds: BTreeMap<String, Build> = BTreeMap::new();

        for (platform, path) in &self.binaries {
            match hash_binary(path) {
                Ok(build) => {
                    builds.insert(platform.clone(), build);
                }
                Err(e) => {
                    tracing::warn!(
                        "releases.binaries.{platform} points at {} which can't be read ({e}); \
                         clients on {platform} won't be offered an update",
                        path.display()
                    );
                }
            }
        }

        Release {
            version: self
                .version
                .clone()
                .unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_string()),
            notes: self.notes.clone(),
            builds,
        }
    }
}

/// One platform's binary, as advertised to clients.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Build {
    /// Hex SHA-256 of the file. This is the identity of a release — a rebuilt
    /// binary at the same path and the same version is still a new release.
    pub sha256: String,
    pub size: u64,
    /// RFC 3339 timestamp of the file's last modification.
    pub modified_at: Option<String>,
}

/// What a server advertises: a version, and a build per platform.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Release {
    pub version: String,
    pub notes: Option<String>,
    /// Platform → build. A platform with no entry has nothing to offer.
    #[serde(default)]
    pub builds: BTreeMap<String, Build>,
}

impl Release {
    /// The build for a platform, if this server carries one.
    pub fn build(&self, platform: &str) -> Option<&Build> {
        self.builds.get(platform)
    }

    /// Whether this server has anything at all to hand out.
    pub fn is_empty(&self) -> bool {
        self.builds.is_empty()
    }
}

/// Read a binary and describe it.
pub fn hash_binary(path: &Path) -> Result<Build> {
    let metadata =
        std::fs::metadata(path).with_context(|| format!("Failed to read {}", path.display()))?;
    anyhow::ensure!(metadata.is_file(), "{} is not a file", path.display());

    Ok(Build {
        sha256: crate::cache::hash_file(path)?,
        size: metadata.len(),
        modified_at: metadata
            .modified()
            .ok()
            .map(|t| chrono::DateTime::<chrono::Utc>::from(t).to_rfc3339()),
    })
}

/// The platform name this build of ciabatta identifies as.
///
/// Deliberately coarse — `linux`, `windows`, `macos` — because it's the key an
/// operator types into a config file by hand. A server that needs to
/// distinguish architectures can carry separate servers or separate paths; that
/// is a rarer problem than a config nobody can guess the spelling of.
pub fn current_platform() -> &'static str {
    if cfg!(target_os = "windows") {
        "windows"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else {
        "linux"
    }
}

/// What this ciabatta binary's own file hashes to, so it can tell whether the
/// server is advertising something it isn't already running.
pub fn own_hash() -> Result<String> {
    let exe = std::env::current_exe().context("Could not find this ciabatta binary on disk")?;
    crate::cache::hash_file(&exe)
}

/// Whether a server's advertised build differs from what's running here.
///
/// Comparing hashes rather than version strings is what makes this useful: an
/// operator who rebuilds without bumping the version still gets their team onto
/// the new binary, and a client that already has the advertised bytes is never
/// nagged.
#[derive(Debug, Clone, PartialEq)]
pub enum UpdateStatus {
    /// The running binary is what the server advertises.
    UpToDate,
    /// The server has something different.
    Available { version: String, build: Build },
    /// The server has nothing for this platform.
    Unavailable,
    /// The check couldn't be made — usually because this binary's own file
    /// isn't readable (running from a deleted path, an odd container).
    Unknown(String),
}

impl UpdateStatus {
    /// Compare a server's release against the running binary.
    pub fn compare(release: &Release, platform: &str) -> UpdateStatus {
        let Some(build) = release.build(platform) else {
            return UpdateStatus::Unavailable;
        };
        match own_hash() {
            Ok(mine) if mine == build.sha256 => UpdateStatus::UpToDate,
            Ok(_) => UpdateStatus::Available {
                version: release.version.clone(),
                build: build.clone(),
            },
            Err(e) => UpdateStatus::Unknown(format!("{e:#}")),
        }
    }

    /// The one-line notice to print, or `None` when there's nothing to say.
    pub fn notice(&self) -> Option<String> {
        match self {
            UpdateStatus::Available { version, .. } => Some(format!(
                "A newer ciabatta ({version}) is available from your remote cache. \
                 Run `ciabatta self update` to install it."
            )),
            _ => None,
        }
    }
}

/// Replace the running ciabatta binary with `bytes`, having checked they hash
/// to `expected`.
///
/// The dance is the same one every self-updater has to do, for the same
/// reasons:
///
/// 1. **Verify before touching anything.** A truncated download that gets
///    written over the binary leaves the user with no working ciabatta and no
///    way to run the command that would fix it.
/// 2. **Write a temp file beside the target, then rename.** Same filesystem, so
///    the rename is atomic; there is no moment where the binary is half-written.
/// 3. **Move the old one aside rather than deleting it.** Windows won't let you
///    replace a running executable, but it will let you rename it — so the
///    running binary gets moved out of the way and the new one takes its place.
pub fn install(bytes: &[u8], expected: &str) -> Result<PathBuf> {
    let actual = crate::cache::hash_bytes(bytes);
    if actual != expected {
        bail!(
            "The downloaded binary doesn't match the hash the server advertised \
             (expected {expected}, got {actual}). Nothing was changed."
        );
    }

    let exe = std::env::current_exe().context("Could not find this ciabatta binary on disk")?;
    let exe = exe.canonicalize().unwrap_or(exe);
    let dir = exe
        .parent()
        .context("This ciabatta binary has no parent directory")?;

    let staged = dir.join(format!(".ciabatta-update-{}", std::process::id()));
    std::fs::write(&staged, bytes).with_context(|| {
        format!(
            "Failed to write {} — is the directory writable?",
            staged.display()
        )
    })?;
    copy_permissions(&exe, &staged)?;

    // The running binary can't be removed on Windows, but it can be renamed.
    let previous = dir.join(format!(
        ".{}.previous",
        exe.file_name().unwrap_or_default().to_string_lossy()
    ));
    let _ = std::fs::remove_file(&previous);
    std::fs::rename(&exe, &previous).with_context(|| {
        format!(
            "Failed to move {} aside — you may need to run this with permission \
             to write to {}",
            exe.display(),
            dir.display()
        )
    })?;

    if let Err(e) = std::fs::rename(&staged, &exe) {
        // Put things back rather than leaving the user with no ciabatta at all.
        let _ = std::fs::rename(&previous, &exe);
        let _ = std::fs::remove_file(&staged);
        return Err(e).with_context(|| format!("Failed to install the new {}", exe.display()));
    }

    // Best effort: the old binary is only kept until the new one has landed.
    let _ = std::fs::remove_file(&previous);
    Ok(exe)
}

#[cfg(unix)]
fn copy_permissions(from: &Path, to: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(from)
        .map(|m| m.permissions().mode())
        // If the original is somehow unreadable, an executable default beats
        // installing a binary nobody can run.
        .unwrap_or(0o755);
    std::fs::set_permissions(to, std::fs::Permissions::from_mode(mode))
        .with_context(|| format!("Failed to make {} executable", to.display()))
}

#[cfg(not(unix))]
fn copy_permissions(_from: &Path, _to: &Path) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ciab_rel_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn scanning_hashes_what_is_there_and_skips_what_is_not() {
        let dir = scratch("scan");
        std::fs::write(dir.join("ciabatta-linux"), b"a linux binary").unwrap();

        let mut config = ReleaseConfig {
            version: Some("0.2.0".into()),
            notes: Some("Adds the remote cache".into()),
            binaries: BTreeMap::from([
                ("linux".to_string(), PathBuf::from("ciabatta-linux")),
                ("windows".to_string(), PathBuf::from("ciabatta-windows.exe")),
            ]),
        };
        config.resolve_relative(&dir);
        assert_eq!(config.binaries["linux"], dir.join("ciabatta-linux"));

        let release = config.scan();
        assert_eq!(release.version, "0.2.0");
        assert_eq!(release.notes.as_deref(), Some("Adds the remote cache"));
        assert_eq!(
            release.builds.len(),
            1,
            "the missing one is skipped, not fatal"
        );

        let build = release.build("linux").unwrap();
        assert_eq!(build.size, 14);
        assert_eq!(
            build.sha256,
            crate::cache::hash_bytes(b"a linux binary"),
            "the advertised hash must be of the file's real contents"
        );
        assert!(release.build("windows").is_none());
        assert!(!release.is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_server_with_no_binaries_advertises_its_own_version() {
        let release = ReleaseConfig::default().scan();
        assert_eq!(release.version, env!("CARGO_PKG_VERSION"));
        assert!(release.is_empty());
        assert_eq!(
            UpdateStatus::compare(&release, "linux"),
            UpdateStatus::Unavailable
        );
        assert!(UpdateStatus::Unavailable.notice().is_none());
    }

    /// The hash decides, not the version string — an operator who rebuilds
    /// without bumping `version:` still gets their team updated.
    #[test]
    fn the_update_check_compares_content_not_version_strings() {
        let running = own_hash().expect("this test binary is readable");

        let same = Release {
            version: "0.0.1-ancient".into(),
            notes: None,
            builds: BTreeMap::from([(
                "linux".to_string(),
                Build {
                    sha256: running.clone(),
                    size: 1,
                    modified_at: None,
                },
            )]),
        };
        assert_eq!(
            UpdateStatus::compare(&same, "linux"),
            UpdateStatus::UpToDate,
            "an old version string with our exact bytes is not an update"
        );
        assert!(UpdateStatus::UpToDate.notice().is_none());

        let different = Release {
            version: "0.2.0".into(),
            notes: None,
            builds: BTreeMap::from([(
                "linux".to_string(),
                Build {
                    sha256: crate::cache::hash_bytes(b"something else entirely"),
                    size: 1,
                    modified_at: None,
                },
            )]),
        };
        match UpdateStatus::compare(&different, "linux") {
            UpdateStatus::Available { version, .. } => assert_eq!(version, "0.2.0"),
            other => panic!("expected an available update, got {other:?}"),
        }
        assert!(
            UpdateStatus::compare(&different, "linux")
                .notice()
                .unwrap()
                .contains("ciabatta self update")
        );

        // A platform the server doesn't carry is never an update.
        assert_eq!(
            UpdateStatus::compare(&different, "plan9"),
            UpdateStatus::Unavailable
        );
    }

    /// A truncated or tampered download must never reach the binary.
    #[test]
    fn installing_refuses_bytes_that_do_not_match_the_advertised_hash() {
        let err = install(b"a partial download", "0000deadbeef")
            .unwrap_err()
            .to_string();
        assert!(err.contains("doesn't match the hash"), "got: {err}");
        assert!(err.contains("Nothing was changed"));
    }

    #[test]
    fn the_platform_name_is_one_of_the_three_a_config_can_spell() {
        assert!(matches!(current_platform(), "linux" | "windows" | "macos"));
    }
}
