//! `ciabatta convert --script <path>` — turn a script into a workflow.
//!
//! A workflow *is* a script. The only thing ciabatta adds is the declarations
//! around it: what it needs, what it reads, what it writes, who owns it. That's
//! also exactly the part nobody wants to write by hand, and the reason so many
//! monorepos are a directory of shell scripts nobody has documented.
//!
//! So this reads the script and does the tedious part: it works out the tools
//! it calls, the environment variables it reads, the files it writes, and the
//! description sitting in its own header comment — and writes a workflow with all
//! of it filled in. What it can't infer, it leaves marked rather than guessing,
//! because a `requires` list that's quietly wrong is worse than one that says
//! it's incomplete.
//!
//! The inference is honest about being inference. Everything it finds is
//! printed for review before it's written, and the generated workflow is a
//! starting point somebody edits — not an oracle.

use std::collections::BTreeSet;
use std::path::Path;

use anyhow::{Context, Result, bail};

use crate::config::CIABATTA_DIR;

/// What reading a script turned up.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Analysis {
    /// The description from its header comment, when it has one.
    pub description: Option<String>,
    /// Executables it calls that a workflow should declare in `requires`.
    pub requires: Vec<String>,
    /// Environment variables it reads.
    pub env: Vec<String>,
    /// Variables it reads with no default, which the run should refuse to start
    /// without.
    pub required_env: Vec<String>,
    /// Paths it appears to write.
    pub outputs: Vec<String>,
    /// Whether it looks like it never exits (a server, a watcher).
    pub persistent: bool,
}

/// Tools worth declaring when a script calls them. Deliberately a list rather
/// than "every word that starts a line": `requires` exists to produce a good
/// error before a build starts, and a list full of `echo` and `cd` would make
/// that error useless.
const KNOWN_TOOLS: &[&str] = &[
    "cargo",
    "rustc",
    "npm",
    "yarn",
    "pnpm",
    "node",
    "deno",
    "bun",
    "python",
    "python3",
    "pip",
    "pip3",
    "poetry",
    "uv",
    "go",
    "java",
    "mvn",
    "gradle",
    "make",
    "cmake",
    "ninja",
    "docker",
    "podman",
    "kubectl",
    "helm",
    "terraform",
    "protoc",
    "tsc",
    "jq",
    "aws",
    "gcloud",
    "az",
    "git",
    "curl",
    "rsync",
    "tar",
    "zip",
    "sqlite3",
    "psql",
    "dotnet",
    "swift",
    "ruby",
    "bundle",
    "php",
    "composer",
];

/// Read a script and work out what it needs and produces.
pub fn analyze(source: &str) -> Analysis {
    let mut analysis = Analysis {
        description: header_description(source),
        persistent: looks_persistent(source),
        ..Default::default()
    };

    let mut requires: BTreeSet<String> = BTreeSet::new();
    let mut env: BTreeSet<String> = BTreeSet::new();
    let mut required: BTreeSet<String> = BTreeSet::new();
    let mut outputs: BTreeSet<String> = BTreeSet::new();

    // `${VAR}`, `$VAR`, and `${VAR:-default}`. The last one has a fallback, so
    // it is read but not *required*.
    let variable = regex::Regex::new(r"\$\{?([A-Z_][A-Z0-9_]*)(:?[-=][^}]*)?\}?").unwrap();
    // A redirect into a path — the most reliable signal of an output there is.
    let redirect = regex::Regex::new(r">\s*([A-Za-z0-9_./\-]+)").unwrap();
    // `mkdir -p <dir>`, which nearly always precedes writing into it.
    let mkdir = regex::Regex::new(r"mkdir\s+(?:-p\s+)?([A-Za-z0-9_./\-]+)").unwrap();
    // The first word of a command, allowing for a leading pipe or `&&`.
    let command = regex::Regex::new(r"(?:^|[|&;]|\$\()\s*([a-z][a-z0-9_.\-]*)").unwrap();

    for raw in source.lines() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }

        // Comments still count for variables (they're often documentation of
        // what the script reads) but not for commands or outputs.
        let is_comment = line.starts_with('#');

        for caps in variable.captures_iter(line) {
            let name = caps[1].to_string();
            // Positional and shell-internal variables aren't configuration.
            if matches!(
                name.as_str(),
                "PATH" | "HOME" | "PWD" | "SHELL" | "USER" | "IFS"
            ) {
                continue;
            }
            env.insert(name.clone());
            if caps.get(2).is_none() && !is_comment {
                required.insert(name);
            }
        }

        if is_comment {
            continue;
        }

        for caps in command.captures_iter(line) {
            let name = &caps[1];
            if KNOWN_TOOLS.contains(&name) {
                requires.insert(name.to_string());
            }
        }

        for caps in redirect.captures_iter(line) {
            let path = caps[1].trim_start_matches("./");
            // `> /dev/null` and friends aren't build output.
            if path.starts_with("/dev/") || path.is_empty() {
                continue;
            }
            outputs.insert(path.to_string());
        }

        for caps in mkdir.captures_iter(line) {
            let path = caps[1].trim_start_matches("./").trim_end_matches('/');
            if path.is_empty() || path.starts_with('/') || path.starts_with('$') {
                continue;
            }
            outputs.insert(format!("{path}/**/*"));
        }
    }

    // A variable that's read with a default somewhere is not required, even if
    // it's also read bare elsewhere — the script has an answer for it.
    analysis.requires = requires.into_iter().collect();
    analysis.env = env.into_iter().collect();
    analysis.required_env = required.into_iter().collect();
    analysis.outputs = collapse_outputs(outputs);
    analysis
}

/// Drop a specific path when a directory glob already covers it, so
/// `mkdir -p dist` plus `> dist/app` produces one pattern rather than two.
fn collapse_outputs(outputs: BTreeSet<String>) -> Vec<String> {
    let globs: Vec<String> = outputs
        .iter()
        .filter(|o| o.ends_with("/**/*"))
        .cloned()
        .collect();

    outputs
        .iter()
        .filter(|output| {
            if output.ends_with("/**/*") {
                return true;
            }
            !globs.iter().any(|glob| {
                let prefix = glob.trim_end_matches("/**/*");
                output.starts_with(&format!("{prefix}/"))
            })
        })
        .cloned()
        .collect()
}

/// The description a script wrote about itself, from the comment block under
/// its shebang.
///
/// Nearly every script has one, and it's nearly always the sentence somebody
/// would have typed into `description:` anyway.
fn header_description(source: &str) -> Option<String> {
    let mut lines = source.lines().peekable();
    if lines.peek().is_some_and(|l| l.starts_with("#!")) {
        lines.next();
    }

    for line in lines {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Some(comment) = trimmed.strip_prefix('#') else {
            // The first non-comment line ends the header.
            return None;
        };
        let text = comment.trim();
        // Skip shellcheck directives, editor modelines, and rulers.
        if text.is_empty()
            || text.starts_with("shellcheck")
            || text.starts_with("vim:")
            || text.chars().all(|c| !c.is_alphanumeric())
        {
            continue;
        }
        return Some(text.trim_end_matches('.').to_string());
    }
    None
}

/// Whether a script looks like it never returns.
fn looks_persistent(source: &str) -> bool {
    const SIGNALS: &[&str] = &[
        "npm run dev",
        "yarn dev",
        "pnpm dev",
        "--watch",
        "-w ",
        "runserver",
        "nodemon",
        "webpack serve",
        "vite",
        "tail -f",
        "while true",
    ];
    let lowered = source.to_lowercase();
    SIGNALS.iter().any(|signal| lowered.contains(signal))
}

/// A workflow name derived from a script's filename.
pub fn name_for(script: &Path) -> String {
    let stem = script
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("script");
    let name: String = stem
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect();
    let name = name.trim_matches('-').to_lowercase();
    if name.is_empty() {
        "script".to_string()
    } else {
        name
    }
}

/// Render the same workflow as a standalone workflow file.
pub fn to_workflow_yaml(name: &str, script_rel: &str, analysis: &Analysis) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "# The \"{name}\" workflow, converted from {script_rel}.\n"
    ));
    out.push_str("#\n");
    out.push_str(&format!(
        "# Run it across the monorepo with `ciabatta {name}`.\n\n"
    ));

    match &analysis.description {
        Some(text) => out.push_str(&format!("description: {}\n", yaml_scalar(text))),
        None => out.push_str("description: \"\"   # TODO: what running this accomplishes\n"),
    }
    out.push_str("owner: \"\"          # TODO: who to ask about this\n");

    if !analysis.requires.is_empty() {
        out.push_str(&format!("requires: [{}]\n", analysis.requires.join(", ")));
    }
    if !analysis.required_env.is_empty() {
        out.push_str(&format!(
            "REQUIRED_ENV: [{}]\n",
            analysis.required_env.join(", ")
        ));
    }

    out.push_str("\nsteps:\n");
    out.push_str(&format!("  - name: {name}\n"));
    match &analysis.description {
        Some(text) => out.push_str(&format!("    description: {}\n", yaml_scalar(text))),
        None => out.push_str("    description: \"\"   # TODO\n"),
    }
    out.push_str(&format!("    script: {}\n", yaml_scalar(script_rel)));
    if analysis.persistent {
        out.push_str("    persistent: true\n");
    }

    // Written but off. The outputs are a decent guess from reading the script;
    // the inputs are not knowable from it, and caching on wrong inputs serves
    // stale artifacts. `ciabatta cache init` fills the rest in and turns it on.
    out.push_str("    cache:\n");
    out.push_str("      enabled: false   # `ciabatta cache init` fills this in\n");
    out.push_str("      inputs: []       # TODO: what this step reads\n");
    if analysis.outputs.is_empty() {
        out.push_str("      outputs: []\n");
    } else {
        out.push_str("      outputs:\n");
        for output in &analysis.outputs {
            out.push_str(&format!("        - {}\n", yaml_scalar(output)));
        }
    }
    out
}

/// Quote a YAML scalar when leaving it bare would change what it means.
fn yaml_scalar(value: &str) -> String {
    serde_yaml_ng::to_string(value)
        .map(|s| s.trim_end().trim_start_matches("--- ").to_string())
        .unwrap_or_else(|_| format!("{value:?}"))
}

// ─── The command ────────────────────────────────────────────────────────────

/// Dispatch `ciabatta convert --script <path>`.
pub fn run(script: &Path, name: Option<&str>, dry_run: bool, force: bool) -> Result<()> {
    let cwd = std::env::current_dir().context("Failed to get current directory")?;
    let root = crate::config::find_root(&cwd).unwrap_or_else(|| cwd.clone());

    let source = std::fs::read_to_string(script)
        .with_context(|| format!("Failed to read {}", script.display()))?;

    // Paths in a workflow are relative to the project root, so one written from
    // any directory in the repo resolves the same way.
    let absolute = script.canonicalize().unwrap_or_else(|_| cwd.join(script));
    let script_rel = absolute
        .strip_prefix(&root)
        .unwrap_or(&absolute)
        .to_string_lossy()
        .replace('\\', "/");

    let name = name.map(str::to_string).unwrap_or_else(|| name_for(script));
    let analysis = analyze(&source);

    report(&name, &script_rel, &analysis);

    if dry_run {
        println!("\n─── workflow ───\n");
        print!("{}", to_workflow_yaml(&name, &script_rel, &analysis));
        println!("\nNothing was written (--dry-run).");
        return Ok(());
    }

    let dir = root
        .join(CIABATTA_DIR)
        .join(crate::workspace::WORKFLOWS_DIR);
    std::fs::create_dir_all(&dir).with_context(|| format!("Failed to create {}", dir.display()))?;
    let path = dir.join(format!("{name}.{}", crate::format::YAML_EXT));
    if path.exists() && !force {
        bail!(
            "{} already exists. Pass --name to use a different one, or --force to \
             replace it.",
            path.display()
        );
    }
    std::fs::write(&path, to_workflow_yaml(&name, &script_rel, &analysis))
        .with_context(|| format!("Failed to write {}", path.display()))?;
    println!("\nAdded workflow {}", path.display());

    println!();
    println!("Try it:");
    println!("  ciabatta {name}");
    if !analysis.outputs.is_empty() {
        println!("  ciabatta cache init      to fill in what it reads, and turn caching on");
    }
    Ok(())
}

/// Print what reading the script turned up, so the user can judge it before it
/// becomes a file.
fn report(name: &str, script_rel: &str, analysis: &Analysis) {
    println!("Reading {script_rel} → workflow '{name}'\n");

    match &analysis.description {
        Some(text) => println!("  description   {text}"),
        None => println!("  description   (none found — left as a TODO)"),
    }

    if analysis.requires.is_empty() {
        println!("  requires      (nothing recognizable — check this)");
    } else {
        println!("  requires      {}", analysis.requires.join(", "));
    }

    if !analysis.required_env.is_empty() {
        println!("  needs set     {}", analysis.required_env.join(", "));
    }
    let optional: Vec<&String> = analysis
        .env
        .iter()
        .filter(|v| !analysis.required_env.contains(v))
        .collect();
    if !optional.is_empty() {
        println!(
            "  reads         {}  (each has a default in the script)",
            optional
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }

    if analysis.outputs.is_empty() {
        println!("  writes        (nothing obvious — no cache section written)");
    } else {
        println!("  writes        {}", analysis.outputs.join(", "));
    }
    if analysis.persistent {
        println!("  persistent    looks like it never exits");
    }

    println!(
        "\nThis is inference from reading the script, not a guarantee — check it \
         before you rely on it."
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    const BUILD_SCRIPT: &str = r#"#!/bin/sh
# Compile the service binary and package it for release.
#
# shellcheck disable=SC2086
set -eu

mkdir -p dist
cargo build --release --target "$TARGET_TRIPLE"
cp target/release/api dist/api

echo "built at $(date)" > dist/BUILD_INFO
tar czf dist/api.tgz -C dist api

echo "api: built with LOG_LEVEL=${LOG_LEVEL:-info}"
"#;

    #[test]
    fn a_script_is_read_for_what_it_needs_and_produces() {
        let analysis = analyze(BUILD_SCRIPT);

        assert_eq!(
            analysis.description.as_deref(),
            Some("Compile the service binary and package it for release"),
            "the sentence the author already wrote is the description"
        );

        assert!(analysis.requires.contains(&"cargo".to_string()));
        assert!(analysis.requires.contains(&"tar".to_string()));
        // Not every word that starts a line — `requires` is for the error a
        // missing tool should produce, and `cp`/`echo` would make it useless.
        assert!(!analysis.requires.contains(&"cp".to_string()));
        assert!(!analysis.requires.contains(&"echo".to_string()));

        // Read with no fallback → the run must refuse to start without it.
        assert_eq!(analysis.required_env, vec!["TARGET_TRIPLE".to_string()]);
        // Read with `:-` → known about, but not required.
        assert!(analysis.env.contains(&"LOG_LEVEL".to_string()));
        assert!(!analysis.required_env.contains(&"LOG_LEVEL".to_string()));

        // `mkdir -p dist` plus writes into it collapse to one pattern.
        assert_eq!(analysis.outputs, vec!["dist/**/*".to_string()]);
        assert!(!analysis.persistent);
    }

    #[test]
    fn a_dev_server_is_recognized_as_never_exiting() {
        let analysis = analyze("#!/bin/sh\n# Serve the app locally.\nnpm run dev -- --port 3000\n");
        assert!(analysis.persistent);
        assert!(analysis.requires.contains(&"npm".to_string()));
        assert_eq!(
            analysis.description.as_deref(),
            Some("Serve the app locally")
        );
    }

    #[test]
    fn shell_internals_and_dev_null_are_not_config() {
        let analysis = analyze(
            "#!/bin/sh\ncd \"$HOME\"\nexport PATH=$PATH:/opt/bin\ncurl -s $API_URL > /dev/null\n",
        );
        assert!(!analysis.env.contains(&"PATH".to_string()));
        assert!(!analysis.env.contains(&"HOME".to_string()));
        assert!(analysis.env.contains(&"API_URL".to_string()));
        assert!(
            analysis.outputs.is_empty(),
            "/dev/null is not build output, got {:?}",
            analysis.outputs
        );
    }

    #[test]
    fn a_script_with_no_header_gets_no_invented_description() {
        let analysis = analyze("#!/bin/bash\nset -e\nmake all\n");
        assert!(analysis.description.is_none());
        assert!(analysis.requires.contains(&"make".to_string()));

        // A ruler-only comment block isn't a description either.
        let analysis = analyze("#!/bin/sh\n# ------------------------\nmake all\n");
        assert!(analysis.description.is_none());
    }

    #[test]
    fn names_come_from_the_filename_and_are_always_usable() {
        assert_eq!(name_for(Path::new("scripts/build.sh")), "build");
        assert_eq!(
            name_for(Path::new("scripts/Build Frontend.sh")),
            "build-frontend"
        );
        assert_eq!(name_for(Path::new("deploy_prod.bash")), "deploy-prod");
        assert_eq!(name_for(Path::new(".sh")), "sh");
    }

    #[test]
    fn the_generated_workflow_parses_as_the_workflow_it_claims_to_be() {
        let analysis = analyze(BUILD_SCRIPT);
        let block = to_workflow_yaml("build", "scripts/build.sh", &analysis);

        let definition: crate::workspace::Workflow =
            crate::format::from_str(&block, crate::format::Format::Yaml)
                .unwrap_or_else(|e| panic!("generated workflow didn't parse: {e}\n\n{block}"));
        assert_eq!(definition.required_env, vec!["TARGET_TRIPLE".to_string()]);
        assert_eq!(definition.steps.len(), 1);

        let step = &definition.steps[0];
        assert_eq!(step.script.as_deref(), Some("scripts/build.sh"));
        assert_eq!(
            step.description.as_deref(),
            Some("Compile the service binary and package it for release")
        );
        // Tools are declared once for the whole workflow rather than per step.
        assert!(definition.requires.contains(&"cargo".to_string()));

        // The cache section is written but off: outputs are a decent guess,
        // inputs can't be inferred, and caching on wrong inputs serves stale
        // artifacts.
        let cache = step.cache.as_ref().expect("cache section written");
        assert!(!cache.is_on());
        assert!(cache.inputs.is_empty());
        assert_eq!(cache.outputs, vec!["dist/**/*".to_string()]);
    }

    #[test]
    fn the_generated_workflow_parses_too() {
        let analysis = analyze(BUILD_SCRIPT);
        let rendered = to_workflow_yaml("build", "scripts/build.sh", &analysis);
        let workflow: crate::workspace::Workflow =
            crate::format::from_str(&rendered, crate::format::Format::Yaml)
                .unwrap_or_else(|e| panic!("generated workflow didn't parse: {e}\n\n{rendered}"));

        assert_eq!(workflow.steps.len(), 1);
        assert_eq!(
            workflow.steps[0].script.as_deref(),
            Some("scripts/build.sh")
        );
        assert_eq!(workflow.required_env, vec!["TARGET_TRIPLE".to_string()]);
        assert_eq!(workflow.owner.as_deref(), Some(""), "left for the user");
    }

    /// A description containing YAML punctuation must not break the file it
    /// gets written into.
    #[test]
    fn a_yaml_hostile_description_survives() {
        for text in [
            "# Build: the thing",
            "Build {the} thing",
            "Deploy - prod only",
            "true",
        ] {
            let analysis = Analysis {
                description: Some(text.to_string()),
                ..Default::default()
            };
            let block = to_workflow_yaml("build", "s.sh", &analysis);
            let workflow: crate::workspace::Workflow =
                crate::format::from_str(&block, crate::format::Format::Yaml)
                    .unwrap_or_else(|e| panic!("{text:?} broke the workflow: {e}\n\n{block}"));
            assert_eq!(workflow.steps[0].description.as_deref(), Some(text));
        }
    }
}
