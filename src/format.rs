//! Reading and writing ciabatta's own config files, in either of the two
//! formats it understands.
//!
//! From 0.2.0 ciabatta writes **YAML**: `ciabatta.yaml`, `.ciabatta/workflows/
//! *.yaml`, flowchart files. YAML is what a monorepo's config should be — it
//! nests without repeating a path on every table header, it takes multi-line
//! shell commands without escaping them, and it's the format the people writing
//! CI pipelines already read all day.
//!
//! TOML still parses. A repo that predates 0.2.0 keeps working exactly as it
//! did, with one deprecation notice pointing at `ciabatta config migrate`.
//! Breaking every existing checkout to change a file extension would be a poor
//! trade, and the reader that makes it unnecessary is thirty lines long.
//!
//! Note this is only about *ciabatta's* files. `Cargo.toml`, `pyproject.toml`
//! and friends are other tools' formats, and `ciabatta analyze` goes on reading
//! them as TOML because that is what they are.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{Context, Result};
use serde::Serialize;
use serde::de::DeserializeOwned;

/// Which of the two config syntaxes a file is written in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// The format ciabatta writes.
    Yaml,
    /// The pre-0.2.0 format, still read.
    Toml,
}

/// The extension ciabatta gives the files it writes.
pub const YAML_EXT: &str = "yaml";

/// Config file extensions, in the order they're looked for. `.yaml` wins over
/// `.yml`, and both win over `.toml`, so a half-migrated directory resolves to
/// the migrated file rather than to whichever the filesystem listed first.
pub const CONFIG_EXTS: &[&str] = &["yaml", "yml", "toml"];

impl Format {
    /// The format a path's extension implies. An unrecognized (or absent)
    /// extension is treated as YAML — new files default to the new format.
    pub fn of_path(path: &Path) -> Format {
        match path.extension().and_then(|e| e.to_str()) {
            Some(ext) if ext.eq_ignore_ascii_case("toml") => Format::Toml,
            _ => Format::Yaml,
        }
    }

    /// The file extension for this format.
    pub fn ext(self) -> &'static str {
        match self {
            Format::Yaml => "yaml",
            Format::Toml => "toml",
        }
    }
}

/// Parse `content` as `T` in the given format.
pub fn from_str<T: DeserializeOwned>(content: &str, format: Format) -> Result<T> {
    match format {
        Format::Yaml => serde_yaml_ng::from_str(content).map_err(Into::into),
        Format::Toml => toml::from_str(content).map_err(Into::into),
    }
}

/// Serialize `value` in the given format.
pub fn to_string<T: Serialize>(value: &T, format: Format) -> Result<String> {
    match format {
        Format::Yaml => serde_yaml_ng::to_string(value).map_err(Into::into),
        Format::Toml => toml::to_string_pretty(value).map_err(Into::into),
    }
}

/// Read and parse a config file, choosing the parser from its extension.
///
/// The error names the file and the format it was read as, because "expected a
/// mapping" is a great deal less useful than knowing ciabatta tried to read
/// your TOML as YAML.
pub fn load<T: DeserializeOwned>(path: &Path) -> Result<T> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read {}", path.display()))?;
    let format = Format::of_path(path);
    if format == Format::Toml {
        deprecation_notice(path);
    }
    from_str(&content, format)
        .with_context(|| format!("Failed to parse {} as {}", path.display(), format.ext()))
}

/// Look for `<dir>/<stem>.<ext>` across [`CONFIG_EXTS`], returning the first
/// that exists.
///
/// This is what makes the migration a non-event: every call site asks for "the
/// config in this directory" rather than for a filename, so a directory holding
/// either format resolves the same way.
pub fn find(dir: &Path, stem: &str) -> Option<PathBuf> {
    CONFIG_EXTS
        .iter()
        .map(|ext| dir.join(format!("{stem}.{ext}")))
        .find(|p| p.is_file())
}

/// Whether this path is a ciabatta config file by extension — used when
/// scanning a directory (`.ciabatta/workflows/`) for every config in it.
pub fn is_config_file(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|ext| CONFIG_EXTS.iter().any(|c| ext.eq_ignore_ascii_case(c)))
}

/// Every config file in `dir`, sorted, de-duplicated by stem so a directory
/// mid-migration doesn't load `build.yaml` and `build.toml` as two workflows.
///
/// The YAML one wins, matching [`find`].
pub fn config_files_in(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };

    let mut files: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file() && is_config_file(p))
        .collect();

    // Sort by (stem, extension rank) so the preferred format of each stem sorts
    // first and the dedup below keeps it.
    files.sort_by_key(|p| {
        let stem = p
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or_default()
            .to_string();
        let rank = CONFIG_EXTS
            .iter()
            .position(|ext| p.extension().is_some_and(|e| e.eq_ignore_ascii_case(ext)))
            .unwrap_or(CONFIG_EXTS.len());
        (stem, rank)
    });
    files.dedup_by_key(|p| {
        p.file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or_default()
            .to_string()
    });
    files
}

/// Paths already warned about, so a workspace scan that loads the same legacy
/// file from several call sites says so once rather than once per read.
static WARNED: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

/// Tell the user, once per path per process, that a file is in the old format.
///
/// Deliberately a note on stderr rather than an error: their build works, and
/// interrupting it to complain about a file extension would be the wrong trade.
fn deprecation_notice(path: &Path) {
    let mut warned = WARNED.lock().unwrap();
    if warned.iter().any(|p| p == path) {
        return;
    }
    warned.push(path.to_path_buf());
    drop(warned);

    eprintln!(
        "note: {} is TOML. Ciabatta writes YAML from 0.2.0 — \
         run `ciabatta config migrate` to convert (the TOML still works).",
        path.display()
    );
}

// ─── Editing a document in place ────────────────────────────────────────────
//
// Both `ciabatta configure` and `ciabatta ai setup` add to a config the user
// already owns. Round-tripping it through the parser would be simpler and would
// throw away every comment in the file — including the ones ciabatta itself
// scaffolded to explain the schema. So these two splice text instead, and the
// callers check the result still parses.

/// Whether `line` opens the top-level mapping `key` — `key:` at column 0 with
/// nothing but an optional comment after the colon.
fn opens_top_level(line: &str, key: &str) -> bool {
    let Some(rest) = line.strip_prefix(key) else {
        return false;
    };
    let Some(rest) = rest.strip_prefix(':') else {
        return false;
    };
    let rest = rest.trim();
    rest.is_empty() || rest.starts_with('#') || rest == "{}" || rest == "null" || rest == "~"
}

/// Whether `line` starts a new top-level construct, ending whatever block came
/// before it. Blank lines and indented lines continue the current block;
/// comments at column 0 are treated as belonging to what follows, so replacing
/// a block never eats the comment introducing the next one.
fn starts_top_level(line: &str) -> bool {
    !line.is_empty() && !line.starts_with([' ', '\t'])
}

/// The line range of the top-level `key` block in `document`, if it has one.
fn top_level_span(document: &str, key: &str) -> Option<(usize, usize)> {
    let lines: Vec<&str> = document.lines().collect();
    let start = lines.iter().position(|l| opens_top_level(l, key))?;
    let end = lines
        .iter()
        .enumerate()
        .skip(start + 1)
        .find(|(_, l)| starts_top_level(l))
        .map(|(i, _)| i)
        .unwrap_or(lines.len());
    Some((start, end))
}

/// Replace the top-level `key` block of `document` with `block`, or append
/// `block` when the document has no such key.
///
/// `block` is the whole rendered mapping, `key:` line included.
pub fn set_top_level(document: &str, key: &str, block: &str) -> String {
    let block = block.trim_end();

    let Some((start, end)) = top_level_span(document, key) else {
        let mut out = document.trim_end().to_string();
        if !out.is_empty() {
            out.push_str("\n\n");
        }
        out.push_str(block);
        out.push('\n');
        return out;
    };

    let lines: Vec<&str> = document.lines().collect();
    let mut out: Vec<String> = lines[..start].iter().map(|s| s.to_string()).collect();
    out.extend(block.lines().map(|s| s.to_string()));
    out.extend(lines[end..].iter().map(|s| s.to_string()));

    let mut rendered = out.join("\n");
    rendered.push('\n');
    rendered
}

/// Add `entry` — an already-indented child mapping — under the top-level `key`,
/// creating `key:` at the end of the document when it isn't there yet.
///
/// Returns an error when `key` exists but holds an inline value there's no safe
/// way to extend; rewriting that by hand is the user's call, not ciabatta's.
pub fn insert_under(document: &str, key: &str, entry: &str) -> Result<String> {
    let entry = entry.trim_end_matches('\n');

    // An existing key with an inline value other than an empty mapping can't be
    // extended by splicing, and silently replacing it would lose data.
    if let Some(line) = document
        .lines()
        .find(|l| l.starts_with(key) && l[key.len()..].starts_with(':'))
        && !opens_top_level(line, key)
    {
        anyhow::bail!(
            "`{key}:` in this config holds an inline value ({}), so ciabatta can't \
             add to it automatically. Add the entry by hand, or move `{key}` onto \
             its own lines first.",
            line.trim()
        );
    }

    let Some((start, _)) = top_level_span(document, key) else {
        let mut out = document.trim_end().to_string();
        if !out.is_empty() {
            out.push_str("\n\n");
        }
        out.push_str(key);
        out.push_str(":\n");
        out.push_str(entry);
        out.push('\n');
        return Ok(out);
    };

    let lines: Vec<&str> = document.lines().collect();
    // An empty inline mapping is a placeholder; the children replace it.
    let opener = if lines[start].trim_end().ends_with(':') {
        lines[start].to_string()
    } else {
        format!("{key}:")
    };

    let mut out: Vec<String> = lines[..start].iter().map(|s| s.to_string()).collect();
    out.push(opener);
    out.extend(entry.lines().map(|s| s.to_string()));
    out.extend(lines[start + 1..].iter().map(|s| s.to_string()));

    let mut rendered = out.join("\n");
    rendered.push('\n');
    Ok(rendered)
}

/// Insert `line` as the first child of the `child` mapping inside the top-level
/// `parent` mapping.
///
/// Scoped on purpose. Scanning the whole document for an indented `url:` finds
/// whichever one comes first — a registry's, most likely — and splices into the
/// wrong place. Narrowing to the parent block first is the difference between
/// editing `cache.remote` and editing whatever happened to look similar.
///
/// `line` is written at the child's indentation plus two spaces, and needs no
/// leading whitespace of its own.
pub fn insert_nested(document: &str, parent: &str, child: &str, line: &str) -> Result<String> {
    let (start, end) = top_level_span(document, parent)
        .ok_or_else(|| anyhow::anyhow!("this config has no top-level `{parent}:` section"))?;

    let lines: Vec<&str> = document.lines().collect();
    let child_line = (start + 1..end)
        .find(|&i| lines[i].trim() == format!("{child}:"))
        .ok_or_else(|| anyhow::anyhow!("`{parent}:` has no `{child}:` mapping to add to"))?;

    let indent: String = lines[child_line]
        .chars()
        .take_while(|c| c.is_whitespace())
        .collect();

    let mut out: Vec<String> = lines[..=child_line].iter().map(|s| s.to_string()).collect();
    out.push(format!("{indent}  {line}"));
    out.extend(lines[child_line + 1..].iter().map(|s| s.to_string()));

    let mut rendered = out.join("\n");
    rendered.push('\n');
    Ok(rendered)
}

/// Render `value` as the body of a top-level `key` mapping, indented two spaces
/// — the shape [`insert_under`] and [`set_top_level`] splice in.
pub fn yaml_block<T: Serialize>(value: &T) -> Result<String> {
    let body = serde_yaml_ng::to_string(value)?;
    Ok(indent(&body, "  "))
}

/// Indent every non-empty line of `text` by `prefix`.
pub fn indent(text: &str, prefix: &str) -> String {
    text.lines()
        .map(|line| {
            if line.trim().is_empty() {
                line.to_string()
            } else {
                format!("{prefix}{line}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Whether a bool is `false` — a `skip_serializing_if` predicate.
///
/// A config written back out should read like one somebody would type. Every
/// `persistent: false` and `recover: false` serde emits by default is a line
/// the reader has to skip past to find the two that matter.
pub fn is_false(value: &bool) -> bool {
    !*value
}

/// Whether a count is zero — the same idea for `retries` and friends.
pub fn is_zero<T: PartialEq + Default>(value: &T) -> bool {
    *value == T::default()
}

/// The YAML path a legacy TOML config would migrate to.
pub fn migrated_path(path: &Path) -> PathBuf {
    path.with_extension(YAML_EXT)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Debug, Deserialize, Serialize, PartialEq)]
    struct Sample {
        name: String,
        #[serde(default)]
        tags: Vec<String>,
    }

    #[test]
    fn format_follows_the_extension_and_defaults_to_yaml() {
        assert_eq!(Format::of_path(Path::new("a/ciabatta.toml")), Format::Toml);
        assert_eq!(Format::of_path(Path::new("a/ciabatta.TOML")), Format::Toml);
        assert_eq!(Format::of_path(Path::new("a/ciabatta.yaml")), Format::Yaml);
        assert_eq!(Format::of_path(Path::new("a/ciabatta.yml")), Format::Yaml);
        // No extension at all is a new file, so it gets the new format.
        assert_eq!(Format::of_path(Path::new("a/ciabatta")), Format::Yaml);
    }

    #[test]
    fn both_formats_round_trip_through_the_same_type() {
        let value = Sample {
            name: "api".into(),
            tags: vec!["fast".into()],
        };

        for format in [Format::Yaml, Format::Toml] {
            let text = to_string(&value, format).unwrap();
            let back: Sample = from_str(&text, format).unwrap();
            assert_eq!(back, value, "{format:?} did not round-trip");
        }
    }

    #[test]
    fn a_directory_mid_migration_resolves_to_the_yaml_file() {
        let dir = std::env::temp_dir().join("ciabatta-format-dedup");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("build.toml"), "name = \"old\"\n").unwrap();
        std::fs::write(dir.join("build.yaml"), "name: new\n").unwrap();
        std::fs::write(dir.join("test.toml"), "name = \"only\"\n").unwrap();
        std::fs::write(dir.join("README.md"), "not config").unwrap();

        let files = config_files_in(&dir);
        let names: Vec<String> = files
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().to_string())
            .collect();
        assert_eq!(names, vec!["build.yaml", "test.toml"]);

        assert_eq!(find(&dir, "build").unwrap(), dir.join("build.yaml"));
        assert_eq!(find(&dir, "test").unwrap(), dir.join("test.toml"));
        assert!(find(&dir, "nothing").is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The failure this exists to prevent: a document with several `url:`
    /// lines, only one of which is the one being edited.
    #[test]
    fn nested_inserts_go_into_the_named_parent_and_no_other() {
        let document = "registries:
  nexus:
    url: https://nexus.example.com
  ghcr:
    url: ghcr.io/example

cache:
  enabled: true
  remote:
    url: http://cache:8380
";

        let out = insert_nested(document, "cache", "remote", "project: abc-123").unwrap();

        // First child of `remote:` — order within a mapping doesn't matter, and
        // inserting right after the key is the one position whose indentation
        // can't be got wrong.
        assert!(
            out.contains("  remote:\n    project: abc-123\n    url: http://cache:8380"),
            "the line must land under cache.remote, got:\n{out}"
        );
        assert!(
            out.contains("  nexus:\n    url: https://nexus.example.com\n  ghcr:"),
            "the registries must be untouched, got:\n{out}"
        );
        assert_eq!(out.matches("project: abc-123").count(), 1);

        // And the result is still the config it claims to be.
        let parsed: crate::config::CiabattaConfig = from_str(&out, Format::Yaml).unwrap();
        let remote = parsed.cache.unwrap().remote.unwrap();
        assert_eq!(remote.url, "http://cache:8380");
        assert_eq!(remote.project.as_deref(), Some("abc-123"));
        assert_eq!(parsed.registries.len(), 2);

        // A missing parent or child is reported rather than guessed at.
        assert!(insert_nested(document, "nope", "remote", "x: 1").is_err());
        assert!(insert_nested(document, "cache", "nope", "x: 1").is_err());
    }

    #[test]
    fn migrated_path_only_changes_the_extension() {
        assert_eq!(
            migrated_path(Path::new("/a/.ciabatta/ciabatta.toml")),
            Path::new("/a/.ciabatta/ciabatta.yaml")
        );
    }
}
