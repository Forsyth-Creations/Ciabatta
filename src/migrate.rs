//! `ciabatta config migrate` — convert a checkout's TOML config to YAML.
//!
//! Ciabatta reads both formats, so this is a convenience rather than a
//! requirement. It exists because the alternative — asking people to hand-port
//! a monorepo's worth of workflow files — is how a format change turns into a
//! version nobody upgrades to.
//!
//! The conversion goes through ciabatta's own types rather than a generic
//! TOML→YAML transform. That costs the comments in the old file, and buys
//! something worth more: a file that round-trips through the same parser the
//! build uses, so a config that migrates is a config that loads.
//!
//! Nothing is deleted. The TOML file stays where it was; ciabatta simply stops
//! reading it, because [`crate::format::find`] prefers the YAML one. Deleting
//! it is a `git rm` the user makes when they're satisfied, not something a
//! migration should do behind their back.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::config::{CIABATTA_DIR, CONFIG_STEM, CiabattaConfig};
use crate::format::{self, Format};
use crate::workspace::{WORKFLOWS_DIR, Workflow};

/// One file the migration would convert, or did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Conversion {
    /// The TOML file read.
    pub from: PathBuf,
    /// The YAML file written (or that would be).
    pub to: PathBuf,
    /// Whether the destination already existed, in which case it's left alone.
    pub skipped: bool,
}

/// The outcome of a migration pass.
#[derive(Debug, Default)]
pub struct Report {
    pub conversions: Vec<Conversion>,
}

impl Report {
    /// Whether there was anything to do.
    pub fn is_empty(&self) -> bool {
        self.conversions.is_empty()
    }

    /// How many files were actually written.
    pub fn written(&self) -> usize {
        self.conversions.iter().filter(|c| !c.skipped).count()
    }
}

/// Convert every ciabatta TOML config at or below `root` to YAML.
///
/// `dry_run` reports what would happen without writing anything — the same
/// courtesy every other ciabatta command extends before it touches your files.
pub fn migrate(root: &Path, dry_run: bool) -> Result<Report> {
    let mut report = Report::default();

    for dir in ciabatta_dirs(root) {
        // The project config itself.
        let toml = dir.join(format!("{CONFIG_STEM}.toml"));
        if toml.is_file() {
            convert::<CiabattaConfig>(&toml, dry_run, &mut report)?;
        }

        // Its workflows, one file each.
        let workflows = dir.join(WORKFLOWS_DIR);
        if workflows.is_dir() {
            for path in toml_files_in(&workflows) {
                convert::<Workflow>(&path, dry_run, &mut report)?;
            }
        }

        // Flowchart files live loose in `.ciabatta/`, named by whoever wrote
        // them, so they're found by extension rather than by name.
        for path in toml_files_in(&dir) {
            if path == toml {
                continue;
            }
            convert::<crate::run::FlowchartFile>(&path, dry_run, &mut report)?;
        }
    }

    Ok(report)
}

/// Read one TOML file as `T` and write it back out as YAML.
///
/// Parsing into the real type is the point: a file that survives the round trip
/// is one ciabatta can definitely load, and one that doesn't is a config that
/// was already subtly wrong — better to hear about it now, by name, than during
/// a build.
fn convert<T>(path: &Path, dry_run: bool, report: &mut Report) -> Result<()>
where
    T: serde::de::DeserializeOwned + serde::Serialize,
{
    let destination = format::migrated_path(path);
    if destination.exists() {
        report.conversions.push(Conversion {
            from: path.to_path_buf(),
            to: destination,
            skipped: true,
        });
        return Ok(());
    }

    let content = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read {}", path.display()))?;
    let value: T = format::from_str(&content, Format::Toml)
        .with_context(|| format!("Failed to parse {} as TOML", path.display()))?;
    let yaml = format::to_string(&value, Format::Yaml)
        .with_context(|| format!("Failed to render {} as YAML", destination.display()))?;

    if !dry_run {
        std::fs::write(&destination, header(path) + &yaml)
            .with_context(|| format!("Failed to write {}", destination.display()))?;
    }

    report.conversions.push(Conversion {
        from: path.to_path_buf(),
        to: destination,
        skipped: false,
    });
    Ok(())
}

/// A note at the top of every converted file saying where it came from.
///
/// Serializing through the types drops the old file's comments, and somebody
/// opening the result deserves to know why their careful annotations aren't in
/// it — and where to find them.
fn header(from: &Path) -> String {
    let name = from
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| from.display().to_string());
    format!(
        "# Converted from {name} by `ciabatta config migrate`.\n\
         # Comments in the original weren't carried over — {name} is still there\n\
         # if you want them back. Ciabatta now reads this file instead.\n"
    )
}

/// Every `.ciabatta/` directory at or below `root`, in path order.
///
/// Reuses the workspace member scan so migration covers exactly the files a
/// build would load — no more, and crucially no less.
fn ciabatta_dirs(root: &Path) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = crate::workspace::member_dirs(root)
        .into_iter()
        .map(|d| d.join(CIABATTA_DIR))
        .collect();

    // The root's own `.ciabatta/` counts even when it holds no config — a
    // monorepo root often carries only flowcharts and shared workflows.
    let root_dir = root.join(CIABATTA_DIR);
    if root_dir.is_dir() && !dirs.contains(&root_dir) {
        dirs.insert(0, root_dir);
    }
    dirs
}

/// The `.toml` files directly inside `dir`, sorted.
fn toml_files_in(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.is_file()
                && p.extension()
                    .is_some_and(|e| e.eq_ignore_ascii_case("toml"))
        })
        .collect();
    files.sort();
    files
}

/// Print a migration report the way the rest of the CLI reports work.
pub fn print_report(report: &Report, dry_run: bool) -> Result<()> {
    if report.is_empty() {
        println!("Nothing to migrate — no TOML config files found.");
        return Ok(());
    }

    for conversion in &report.conversions {
        if conversion.skipped {
            println!(
                "  skip   {}  ({} already exists)",
                conversion.from.display(),
                conversion
                    .to
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
            );
        } else {
            println!(
                "  {}  {} → {}",
                if dry_run { "would" } else { "wrote" },
                conversion.from.display(),
                conversion
                    .to
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
            );
        }
    }

    println!();
    if dry_run {
        println!(
            "{} file(s) would be converted. Run without --dry-run to write them.",
            report.written()
        );
        return Ok(());
    }

    if report.written() == 0 {
        bail!("Every TOML config already has a YAML counterpart; nothing was written.");
    }

    println!(
        "Converted {} file(s). Ciabatta reads the YAML from now on.\n\
         The .toml files are untouched — check the YAML looks right, then delete them.",
        report.written()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ciab_migrate_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_whole_checkout_converts_and_still_loads() {
        let root = scratch("checkout");
        let pkg = root.join("packages/api");
        std::fs::create_dir_all(pkg.join(CIABATTA_DIR).join(WORKFLOWS_DIR)).unwrap();
        std::fs::write(
            pkg.join(CIABATTA_DIR).join("ciabatta.toml"),
            "[workspace]\nname = \"api\"\nowner = \"Ada\"\ndepends_on = [\"proto\"]\n\
             [recipies.binary]\nregistry = \"nexus\"\npublish_path = \"a/b\"\n",
        )
        .unwrap();
        std::fs::write(
            pkg.join(CIABATTA_DIR)
                .join(WORKFLOWS_DIR)
                .join("build.toml"),
            "description = \"Build it\"\n[[steps]]\nname = \"compile\"\nrun = \"cargo build\"\n",
        )
        .unwrap();

        // A dry run writes nothing.
        let report = migrate(&root, true).unwrap();
        assert_eq!(report.written(), 2);
        assert!(!pkg.join(CIABATTA_DIR).join("ciabatta.yaml").exists());

        let report = migrate(&root, false).unwrap();
        assert_eq!(report.written(), 2);

        // The original is left alone, and the workspace now loads from YAML.
        assert!(pkg.join(CIABATTA_DIR).join("ciabatta.toml").exists());
        let ws = crate::workspace::Workspace::load(&root).unwrap();
        let member = ws.member("api").expect("api still loads");
        assert_eq!(member.owner(), "Ada");
        assert_eq!(member.meta.depends_on, vec!["proto".to_string()]);
        assert_eq!(member.config.recipes.len(), 1);
        let build = member.workflows.get("build").expect("build loads");
        assert_eq!(build.steps[0].run.as_deref(), Some("cargo build"));

        // And the file it now reads is the converted one.
        let yaml = std::fs::read_to_string(pkg.join(CIABATTA_DIR).join("ciabatta.yaml")).unwrap();
        assert!(yaml.starts_with("# Converted from ciabatta.toml"));
    }

    /// The property that actually matters: nothing a config said before the
    /// migration stops being true after it.
    ///
    /// The dangerous case is a setting whose default is `true` — omit `false`
    /// on the way out and it silently turns back on. `tls_verify: false` is
    /// exactly that, and getting it wrong would quietly stop checking
    /// certificates.
    #[test]
    fn migration_is_lossless_including_settings_that_default_to_true() {
        let root = scratch("lossless");
        std::fs::create_dir_all(root.join(CIABATTA_DIR)).unwrap();
        std::fs::write(
            root.join(CIABATTA_DIR).join("ciabatta.toml"),
            r#"
[workspace]
name = "api"
owner = "Ada"
env_default = ".env.default"

[system]
ci = "github"
containers = "podman"

[registries.nexus]
url = "https://nexus.example.com"
tls_verify = false
needs_auth = true
repository = "raw-hosted"

[recipies.binary]
registry = "nexus"
local_artifact_path = "target/release/app"
publish_path = "app/{CIABATTA_COMMIT}/app"

[recipies.binary.push]
pre = "cargo build --release"

[ai]
provider = "openai"
tls_verify = false

[cache]
enabled = true
inputs = ["src/**/*"]
outputs = ["target/release/app"]

[cache.remote]
url = "http://cache:8380"
project = "abc-123"
enabled = false
"#,
        )
        .unwrap();

        let before: CiabattaConfig =
            crate::format::load(&root.join(CIABATTA_DIR).join("ciabatta.toml")).unwrap();
        migrate(&root, false).unwrap();
        let after: CiabattaConfig =
            crate::format::load(&root.join(CIABATTA_DIR).join("ciabatta.yaml")).unwrap();

        let (b, a) = (before.workspace.unwrap(), after.workspace.unwrap());
        assert_eq!(a.name, b.name);
        assert_eq!(a.owner, b.owner);
        assert_eq!(a.env_default, b.env_default);

        let (b, a) = (before.system.unwrap(), after.system.unwrap());
        assert_eq!(a.ci, b.ci);
        assert_eq!(a.containers, b.containers);

        let (b, a) = (&before.registries["nexus"], &after.registries["nexus"]);
        assert_eq!(a.url, b.url);
        assert_eq!(a.repository, b.repository);
        assert_eq!(a.needs_auth, b.needs_auth);
        assert!(
            !a.tls_verify,
            "tls_verify: false must survive — its default is true, so losing it \
             would silently start trusting any certificate"
        );

        let (b, a) = (&before.recipes["binary"], &after.recipes["binary"]);
        assert_eq!(a.push_recipe().registry, b.push_recipe().registry);
        assert_eq!(a.push_recipe().publish_path, b.push_recipe().publish_path);
        assert_eq!(a.push_recipe().pre, b.push_recipe().pre);

        assert!(
            !after.ai.unwrap().tls_verify,
            "the same trap on the ai section"
        );

        let (b, a) = (before.cache.unwrap(), after.cache.unwrap());
        assert_eq!(a.enabled, b.enabled);
        assert_eq!(a.inputs, b.inputs);
        assert_eq!(a.outputs, b.outputs);
        let remote = a.remote.unwrap();
        assert_eq!(remote.url, "http://cache:8380");
        assert_eq!(remote.project.as_deref(), Some("abc-123"));
        assert!(
            !remote.enabled,
            "a deliberately disabled remote must not come back enabled"
        );

        // And the file is legible: no `null`s or empty collections padding it.
        let yaml = std::fs::read_to_string(root.join(CIABATTA_DIR).join("ciabatta.yaml")).unwrap();
        assert!(
            !yaml.contains(": null"),
            "a migrated config full of nulls is worse to read than the TOML it \
             replaced:\n{yaml}"
        );
        assert!(
            !yaml.contains("{}"),
            "…and so is one full of empty maps:\n{yaml}"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn an_existing_yaml_file_is_never_clobbered() {
        let root = scratch("no_clobber");
        std::fs::create_dir_all(root.join(CIABATTA_DIR)).unwrap();
        std::fs::write(
            root.join(CIABATTA_DIR).join("ciabatta.toml"),
            "[workspace]\nname = \"old\"\n",
        )
        .unwrap();
        std::fs::write(
            root.join(CIABATTA_DIR).join("ciabatta.yaml"),
            "workspace:\n  name: mine\n",
        )
        .unwrap();

        let report = migrate(&root, false).unwrap();
        assert_eq!(report.written(), 0);
        assert!(report.conversions.iter().all(|c| c.skipped));
        assert_eq!(
            std::fs::read_to_string(root.join(CIABATTA_DIR).join("ciabatta.yaml")).unwrap(),
            "workspace:\n  name: mine\n",
            "a hand-written YAML config must survive a migration run"
        );
    }

    #[test]
    fn a_config_that_does_not_parse_is_reported_by_name() {
        let root = scratch("bad_config");
        std::fs::create_dir_all(root.join(CIABATTA_DIR)).unwrap();
        std::fs::write(
            root.join(CIABATTA_DIR).join("ciabatta.toml"),
            "[workspace\nname = broken",
        )
        .unwrap();

        let err = migrate(&root, false).unwrap_err();
        let message = format!("{err:#}");
        assert!(
            message.contains("ciabatta.toml"),
            "the error must name the file: {message}"
        );
    }
}
