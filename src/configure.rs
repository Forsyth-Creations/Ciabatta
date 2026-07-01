//! `ciabatta configure` — friendlier project setup.
//!
//! Two entry points:
//!   * [`run_interactive`] walks you through adding a registry (and optionally a
//!     recipe) without hand-editing TOML.
//!   * [`run_auto`] analyzes the project, then suggests ready-to-paste recipes
//!     for pushing to the registries you already have configured (plus crates.io
//!     for publishable Rust crates) and offers to add the ones you pick.

use anyhow::{Context, Result, bail};
use std::collections::BTreeSet;
use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};

use crate::config::{
    CIABATTA_DIR, CONFIG_FILE, CiabattaConfig, RegistryKind, infer_registry_kind,
    resolve_container_cmd,
};

const SKIP_DIRS: &[&str] = &[
    "node_modules",
    "target",
    "dist",
    "build",
    "vendor",
    ".git",
    ".yarn",
    ".turbo",
    ".ciabatta",
];
const MAX_DEPTH: usize = 4;

// ─── Interactive registry setup ──────────────────────────────────────────────

pub fn run_interactive(root: &Path, cfg: &CiabattaConfig) -> Result<()> {
    println!("ciabatta configure — add a registry (repository)\n");
    println!("Tip: URLs may use environment variables, e.g. ${{NEXUS_HOST:-nexus.example.com}}\n");

    let name = loop {
        let n = prompt("Registry name (e.g. nexus, ecr, dockerhub): ")?;
        if n.is_empty() {
            println!("  A name is required.");
            continue;
        }
        if cfg.registries.contains_key(&n) {
            let ow = prompt(&format!(
                "  '{n}' already exists. Add another registry block anyway? [y/N]: "
            ))?;
            if !is_yes(&ow) {
                continue;
            }
        }
        break n;
    };

    let inferred = format!("{:?}", RegistryKind::from(name.as_str())).to_lowercase();
    let kind = {
        let k = prompt(&format!(
            "Type [nexus|artifactory|s3|docker|ecr|generic] (default: {inferred}): "
        ))?;
        if k.is_empty() { inferred.clone() } else { k }
    };

    let url = loop {
        let u = prompt("URL: ")?;
        if u.is_empty() {
            println!("  A URL is required.");
            continue;
        }
        break u;
    };

    // ECR fetches its own login token, so auth defaults differ by type.
    let auth_default = !matches!(kind.as_str(), "ecr" | "s3");
    let needs_auth = prompt_yes_no("Needs authentication?", auth_default)?;
    let login_script = prompt("Login script path (blank for none): ")?;

    let mut snippet = String::new();
    snippet.push_str(&format!("\n[registries.{name}]\n"));
    snippet.push_str(&format!("url = {}\n", toml_basic(&url)));
    // Only emit `type` when it isn't already implied by the registry name, to
    // keep the generated config tidy.
    if kind != inferred {
        snippet.push_str(&format!("type = {}\n", toml_basic(&kind)));
    }
    snippet.push_str("tls_verify = true\n");
    snippet.push_str(&format!("needs_auth = {needs_auth}\n"));
    if !login_script.is_empty() {
        snippet.push_str(&format!("login_script = {}\n", toml_basic(&login_script)));
    }

    let mut blocks = vec![snippet];

    // Offer to wire up a recipe that publishes to this registry.
    if prompt_yes_no("\nAdd a recipe that publishes to this registry now?", false)? {
        let recipe = loop {
            let r = prompt("  Recipe name: ")?;
            if r.is_empty() {
                println!("    A name is required.");
                continue;
            }
            break r;
        };
        let local = prompt("  Local artifact path (relative to project root): ")?;
        let publish = prompt("  Publish path (supports {CIABATTA_BRANCH}/{CIABATTA_COMMIT}/…): ")?;

        let mut r = String::new();
        r.push_str(&format!("\n[recipies.{recipe}]\n"));
        r.push_str(&format!("registry = {}\n", toml_basic(&name)));
        if !local.is_empty() {
            r.push_str(&format!("local_artifact_path = {}\n", toml_basic(&local)));
        }
        if !publish.is_empty() {
            r.push_str(&format!("publish_path = {}\n", toml_basic(&publish)));
        }
        blocks.push(r);
    }

    let path = append_blocks(root, &blocks)?;
    println!("\nUpdated {}", path.display());
    println!("Run `ciabatta config show` to review, or `ciabatta list` to see recipes.");
    Ok(())
}

// ─── Auto suggestions ────────────────────────────────────────────────────────

struct Suggestion {
    summary: String,
    recipe: String,
    snippet: String,
}

pub fn run_auto(root: &Path, cfg: &CiabattaConfig, assume_yes: bool) -> Result<()> {
    let found = discover(root);
    let (suggestions, ecr, nexus) = build_suggestions(root, cfg, &found);

    if suggestions.is_empty() {
        println!("No suggestions for this project.\n");
        report_gaps(&found, &ecr, &nexus);
        return Ok(());
    }

    // Choose which suggestions to apply. With a real terminal, drive an
    // interactive checklist; otherwise (CI / piped input) print a numbered list
    // and read a selection from stdin.
    let chosen: Vec<usize> = if assume_yes {
        (0..suggestions.len()).collect()
    } else if io::stdout().is_terminal() && io::stdin().is_terminal() {
        match select_via_tui(&suggestions)? {
            Some(sel) => sel,
            None => {
                println!("Cancelled — nothing added.");
                return Ok(());
            }
        }
    } else {
        print_suggestions_text(&suggestions);
        report_gaps(&found, &ecr, &nexus);
        let answer = prompt("Add which? (e.g. 1,3 · 'all' · blank to cancel): ")?;
        parse_selection(&answer, suggestions.len())
    };

    if chosen.is_empty() {
        println!("Nothing added.");
        return Ok(());
    }

    let blocks: Vec<String> = chosen
        .iter()
        .map(|&i| suggestions[i].snippet.clone())
        .collect();
    let path = append_blocks(root, &blocks)?;
    println!("\nAdded {} recipe(s) to {}", chosen.len(), path.display());
    for &i in &chosen {
        println!("  • {}", suggestions[i].recipe);
    }
    println!("Review with `ciabatta config show`, then `ciabatta push --dry-run`.");
    Ok(())
}

/// Build the list of suggested recipes for the project, returning the configured
/// ECR and Nexus registry names alongside (used to flag gaps).
fn build_suggestions(
    root: &Path,
    cfg: &CiabattaConfig,
    found: &Discovered,
) -> (Vec<Suggestion>, Vec<String>, Vec<String>) {
    let container = resolve_container_cmd(cfg).unwrap_or_else(|_| "docker".to_string());

    // Registries already configured, grouped by the kinds auto cares about.
    let by_kind = |k: RegistryKind| -> Vec<String> {
        cfg.registries
            .iter()
            .filter(|(n, c)| infer_registry_kind(n, c) == k)
            .map(|(n, _)| n.clone())
            .collect::<Vec<_>>()
    };
    let ecr = by_kind(RegistryKind::Ecr);
    let nexus = by_kind(RegistryKind::Nexus);
    // Registries that take an uploaded file (registry + local_artifact_path +
    // publish_path). Nexus/Artifactory HTTP PUT and S3 `aws s3 cp` all share this
    // shape, so a built binary can target any of them.
    let blob_regs: Vec<String> = cfg
        .registries
        .iter()
        .filter(|(n, c)| {
            matches!(
                infer_registry_kind(n, c),
                RegistryKind::Nexus | RegistryKind::S3 | RegistryKind::Artifactory
            )
        })
        .map(|(n, _)| n.clone())
        .collect();

    // Recipe names already taken (existing + everything we generate this run).
    let mut used: BTreeSet<String> = cfg.recipes.keys().cloned().collect();
    let mut suggestions: Vec<Suggestion> = Vec::new();

    // ── Dockerfiles → ECR / Nexus ──
    for df in &found.dockerfiles {
        let image = image_name(df, root);
        let ctx = parent_or_dot(df);
        for reg in &ecr {
            let url = cfg.registries[reg].url.trim_end_matches('/').to_string();
            let recipe = uniquify(&format!("{image}_to_{reg}"), &mut used);
            let snippet = format!(
                "\n# Build the Docker image from {df} and push it to the \"{reg}\" ECR registry.\n\
                 [recipies.{recipe}]\n\
                 registry     = {reg_q}\n\
                 publish_path = {tag}\n\
                 pre          = '{container} build -t {url}/{image}:$CIABATTA_COMMIT -f {df} {ctx}'\n",
                reg_q = toml_basic(reg),
                tag = toml_basic(&format!("{image}:{{CIABATTA_COMMIT}}")),
            );
            suggestions.push(Suggestion {
                summary: format!("Push image from {df} to ECR registry \"{reg}\""),
                recipe,
                snippet,
            });
        }
        for reg in &nexus {
            // The nexus registry is an HTTP PUT endpoint, so save the image to a
            // tarball and upload it via the built-in nexus push (which handles
            // CIABATTA_<REG>_USER/PASS auth for us).
            let recipe = uniquify(&format!("{image}_to_{reg}"), &mut used);
            let snippet = format!(
                "\n# Build the Docker image from {df}, save it as a tarball, and upload it to nexus (\"{reg}\").\n\
                 [recipies.{recipe}]\n\
                 registry            = {reg_q}\n\
                 local_artifact_path = {local}\n\
                 publish_path        = {publish}\n\
                 pre                 = '{container} build -t {image}:$CIABATTA_COMMIT -f {df} {ctx} && {container} save -o {image}.tar {image}:$CIABATTA_COMMIT'\n",
                reg_q = toml_basic(reg),
                local = toml_basic(&format!("{image}.tar")),
                publish = toml_basic(&format!("docker/{image}/{{CIABATTA_COMMIT}}/{image}.tar")),
            );
            suggestions.push(Suggestion {
                summary: format!("Upload image tarball from {df} to nexus registry \"{reg}\""),
                recipe,
                snippet,
            });
        }
    }

    // ── Rust crates → crates.io · binaries → S3 / Nexus / Artifactory ──
    for pkg in &found.rust_pkgs {
        let mflag = manifest_flag(&pkg.manifest);
        if pkg.publishable {
            let recipe = uniquify(&format!("{}_crate", pkg.name), &mut used);
            let snippet = format!(
                "\n# Publish the \"{name}\" crate to crates.io (needs `cargo login` / CARGO_REGISTRY_TOKEN).\n\
                 [recipies.{recipe}]\n\
                 main = 'cargo publish{mflag}'\n",
                name = pkg.name,
            );
            suggestions.push(Suggestion {
                summary: format!("Publish crate \"{}\" to crates.io", pkg.name),
                recipe,
                snippet,
            });
        }
        for bin in &pkg.bins {
            for reg in &blob_regs {
                let kind =
                    format!("{:?}", infer_registry_kind(reg, &cfg.registries[reg])).to_lowercase();
                let recipe = uniquify(&format!("{bin}_binary_to_{reg}"), &mut used);
                let snippet = format!(
                    "\n# Build the \"{bin}\" release binary and upload it to the \"{reg}\" {kind} registry.\n\
                     [recipies.{recipe}]\n\
                     registry            = {reg_q}\n\
                     local_artifact_path = {local}\n\
                     publish_path        = {publish}\n\
                     pre                 = 'cargo build --release{mflag}'\n",
                    reg_q = toml_basic(reg),
                    local = toml_basic(&format!("target/release/{bin}")),
                    publish = toml_basic(&format!(
                        "{name}/{{CIABATTA_BRANCH}}/{{CIABATTA_COMMIT}}/{bin}",
                        name = pkg.name
                    )),
                );
                suggestions.push(Suggestion {
                    summary: format!("Upload binary \"{bin}\" to {kind} registry \"{reg}\""),
                    recipe,
                    snippet,
                });
            }
        }
    }

    (suggestions, ecr, nexus)
}

/// Print suggestions as a numbered list (the non-interactive text fallback).
fn print_suggestions_text(suggestions: &[Suggestion]) {
    println!("Found {} suggestion(s):\n", suggestions.len());
    for (i, s) in suggestions.iter().enumerate() {
        println!("  [{}] {}", i + 1, s.summary);
        for line in s.snippet.trim_matches('\n').lines() {
            println!("        {line}");
        }
        println!();
    }
}

// ─── Interactive checklist (TUI) ─────────────────────────────────────────────

/// Present the suggestions as an interactive checklist. Returns the chosen
/// indices, or `None` if the user cancelled.
fn select_via_tui(suggestions: &[Suggestion]) -> Result<Option<Vec<usize>>> {
    use crossterm::{
        execute,
        terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
    };
    use ratatui::{Terminal, backend::CrosstermBackend};

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout))?;

    // Run the loop, then always restore the terminal — even on error.
    let result = selector_loop(&mut terminal, suggestions);

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    result
}

fn selector_loop(
    terminal: &mut ratatui::Terminal<ratatui::backend::CrosstermBackend<io::Stdout>>,
    suggestions: &[Suggestion],
) -> Result<Option<Vec<usize>>> {
    use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};

    let len = suggestions.len();
    let mut cursor = 0usize;
    // Nothing is selected by default; the user opts recipes in.
    let mut checked = vec![false; len];

    loop {
        terminal.draw(|f| render_selector(f, suggestions, cursor, &checked))?;

        let Event::Key(key) = event::read()? else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => return Ok(None),
            KeyCode::Char('c') if ctrl => return Ok(None),
            KeyCode::Down | KeyCode::Char('j') => cursor = (cursor + 1) % len,
            KeyCode::Up | KeyCode::Char('k') => cursor = cursor.checked_sub(1).unwrap_or(len - 1),
            // Enter toggles the highlighted suggestion.
            KeyCode::Enter => checked[cursor] = !checked[cursor],
            KeyCode::Char('a') => checked.iter_mut().for_each(|c| *c = true),
            KeyCode::Char('n') => checked.iter_mut().for_each(|c| *c = false),
            // 's' saves the current selection and applies it.
            KeyCode::Char('s') => {
                let chosen = checked
                    .iter()
                    .enumerate()
                    .filter_map(|(i, c)| c.then_some(i))
                    .collect();
                return Ok(Some(chosen));
            }
            _ => {}
        }
    }
}

fn render_selector(
    f: &mut ratatui::Frame,
    suggestions: &[Suggestion],
    cursor: usize,
    checked: &[bool],
) {
    use ratatui::{
        layout::{Constraint, Layout},
        style::{Color, Modifier, Style},
        text::{Line, Span},
        widgets::{Block, Borders, List, ListItem, ListState, Paragraph},
    };

    let rows = Layout::vertical([
        Constraint::Length(1), // title
        Constraint::Min(4),    // body
        Constraint::Length(1), // help
    ])
    .split(f.area());

    let count = checked.iter().filter(|c| **c).count();
    let title = Line::from(vec![
        Span::styled(
            " ciabatta configure auto ",
            Style::default()
                .fg(Color::Black)
                .bg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(format!("  {count}/{} selected", suggestions.len())),
    ]);
    f.render_widget(Paragraph::new(title), rows[0]);

    let cols =
        Layout::horizontal([Constraint::Percentage(45), Constraint::Percentage(55)]).split(rows[1]);

    // Left: the checklist.
    let items: Vec<ListItem> = suggestions
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let (mark, color) = if checked[i] {
                ("[x] ", Color::Green)
            } else {
                ("[ ] ", Color::DarkGray)
            };
            ListItem::new(Line::from(vec![
                Span::styled(mark, Style::default().fg(color)),
                Span::raw(s.summary.clone()),
            ]))
        })
        .collect();
    let list = List::new(items)
        .block(
            Block::default()
                .title(" Suggestions ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan)),
        )
        .highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▶ ");
    let mut state = ListState::default();
    state.select(Some(cursor));
    f.render_stateful_widget(list, cols[0], &mut state);

    // Right: a preview of the TOML that will be appended.
    let preview: Vec<Line> = suggestions
        .get(cursor)
        .map(|s| {
            s.snippet
                .trim_matches('\n')
                .lines()
                .map(|l| {
                    let style = if l.starts_with('#') {
                        Style::default().fg(Color::DarkGray)
                    } else if l.starts_with('[') {
                        Style::default()
                            .fg(Color::Cyan)
                            .add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(Color::Gray)
                    };
                    Line::from(Span::styled(l.to_string(), style))
                })
                .collect()
        })
        .unwrap_or_default();
    let preview = Paragraph::new(preview).block(
        Block::default()
            .title(" Preview (appended to ciabatta.toml) ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray)),
    );
    f.render_widget(preview, cols[1]);

    let help =
        " [↑↓/jk] move   [enter] select   [a] all   [n] none   [s] save   [q/esc] cancel";
    f.render_widget(
        Paragraph::new(help).style(Style::default().fg(Color::DarkGray)),
        rows[2],
    );
}

/// Point out things auto could help with if the project were configured differently.
fn report_gaps(found: &Discovered, ecr: &[String], nexus: &[String]) {
    if !found.dockerfiles.is_empty() && ecr.is_empty() && nexus.is_empty() {
        println!(
            "note: found Dockerfile(s) but no ECR or Nexus registry is configured.\n      \
             Run `ciabatta configure` to add one, then re-run `ciabatta configure auto`.\n"
        );
    }
}

// ─── Project discovery ───────────────────────────────────────────────────────

struct Discovered {
    dockerfiles: Vec<String>,
    rust_pkgs: Vec<RustPkg>,
}

struct RustPkg {
    manifest: String,
    name: String,
    publishable: bool,
    bins: Vec<String>,
}

fn discover(root: &Path) -> Discovered {
    let mut found = Discovered {
        dockerfiles: Vec::new(),
        rust_pkgs: Vec::new(),
    };
    walk(root, root, 0, &mut found);
    found.dockerfiles.sort();
    found.rust_pkgs.sort_by(|a, b| a.manifest.cmp(&b.manifest));
    found
}

fn walk(dir: &Path, root: &Path, depth: usize, out: &mut Discovered) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
            if depth < MAX_DEPTH && !SKIP_DIRS.contains(&name) && !name.starts_with('.') {
                walk(&path, root, depth + 1, out);
            }
        } else if let Some(name) = path.file_name().and_then(|s| s.to_str()) {
            match name {
                "Dockerfile" => out.dockerfiles.push(rel(&path, root)),
                "Cargo.toml" => {
                    if let Some(pkg) = scan_cargo(&path, root) {
                        out.rust_pkgs.push(pkg);
                    }
                }
                _ => {}
            }
        }
    }
}

fn scan_cargo(path: &Path, root: &Path) -> Option<RustPkg> {
    let content = std::fs::read_to_string(path).ok()?;
    let val: toml::Value = toml::from_str(&content).ok()?;
    let pkg = val.get("package")?;
    let name = pkg.get("name")?.as_str()?.to_string();
    let publishable = !matches!(pkg.get("publish"), Some(toml::Value::Boolean(false)));

    let mut bins = Vec::new();
    if let Some(arr) = val.get("bin").and_then(|b| b.as_array()) {
        for b in arr {
            if let Some(n) = b.get("name").and_then(|x| x.as_str()) {
                bins.push(n.to_string());
            }
        }
    }
    // A crate with a src/main.rs but no explicit [[bin]] builds a binary named
    // after the package.
    if bins.is_empty()
        && path
            .parent()
            .map(|d| d.join("src/main.rs").exists())
            .unwrap_or(false)
    {
        bins.push(name.clone());
    }

    Some(RustPkg {
        manifest: rel(path, root),
        name,
        publishable,
        bins,
    })
}

// ─── Small helpers ───────────────────────────────────────────────────────────

fn rel(path: &Path, root: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .display()
        .to_string()
        .replace('\\', "/")
}

/// Derive an image name from a Dockerfile's location: its directory name, or the
/// project's directory name when the Dockerfile is at the root.
fn image_name(dockerfile: &str, root: &Path) -> String {
    let parent = Path::new(dockerfile).parent();
    let from_dir = parent
        .and_then(|p| p.file_name())
        .and_then(|s| s.to_str())
        .filter(|s| !s.is_empty());
    let name = from_dir
        .map(String::from)
        .or_else(|| {
            root.file_name()
                .and_then(|s| s.to_str())
                .map(str::to_lowercase)
        })
        .unwrap_or_else(|| "app".to_string());
    sanitize(&name)
}

fn parent_or_dot(file: &str) -> String {
    match Path::new(file)
        .parent()
        .map(|p| p.to_string_lossy().to_string())
    {
        Some(p) if !p.is_empty() => p,
        _ => ".".to_string(),
    }
}

/// `--manifest-path <p>` for a non-root Cargo.toml, else empty.
fn manifest_flag(manifest: &str) -> String {
    if manifest == "Cargo.toml" {
        String::new()
    } else {
        format!(" --manifest-path {manifest}")
    }
}

/// Lowercase a string down to `[a-z0-9_]`, collapsing everything else to `_`.
fn sanitize(s: &str) -> String {
    s.to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect()
}

/// Ensure a recipe name is unique against `used`, suffixing `_2`, `_3`, … if not.
fn uniquify(base: &str, used: &mut BTreeSet<String>) -> String {
    let base = sanitize(base);
    if used.insert(base.clone()) {
        return base;
    }
    for n in 2.. {
        let candidate = format!("{base}_{n}");
        if used.insert(candidate.clone()) {
            return candidate;
        }
    }
    unreachable!()
}

/// Format a value as a TOML basic string with the necessary escaping.
fn toml_basic(s: &str) -> String {
    let escaped = s.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{escaped}\"")
}

/// Parse a selection like `1,3`, `all`, or blank into 0-based indices.
fn parse_selection(answer: &str, count: usize) -> Vec<usize> {
    let answer = answer.trim();
    if answer.is_empty() {
        return Vec::new();
    }
    if answer.eq_ignore_ascii_case("all") {
        return (0..count).collect();
    }
    let mut out: BTreeSet<usize> = BTreeSet::new();
    for tok in answer.split([',', ' ']) {
        let tok = tok.trim();
        if tok.is_empty() {
            continue;
        }
        if let Ok(n) = tok.parse::<usize>()
            && (1..=count).contains(&n)
        {
            out.insert(n - 1);
        }
    }
    out.into_iter().collect()
}

// ─── Config-file writing ─────────────────────────────────────────────────────

/// Path to the project's config file, creating `.ciabatta/ciabatta.toml` with a
/// minimal header if it doesn't exist yet.
fn ensure_config_exists(root: &Path) -> Result<PathBuf> {
    let dir = root.join(CIABATTA_DIR);
    let path = dir.join(CONFIG_FILE);
    if !path.exists() {
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("Failed to create {}", dir.display()))?;
        let header = "# Ciabatta configuration\n\
                      # Run `ciabatta config reference` for full documentation.\n\n\
                      [system]\n\
                      # containers = \"docker\"  # docker | podman (auto-detected when unset)\n";
        std::fs::write(&path, header)
            .with_context(|| format!("Failed to write {}", path.display()))?;
        println!("Created {}", path.display());
    }
    Ok(path)
}

/// Append config blocks to the project's config file.
fn append_blocks(root: &Path, blocks: &[String]) -> Result<PathBuf> {
    if blocks.is_empty() {
        bail!("nothing to write");
    }
    let path = ensure_config_exists(root)?;
    let mut content = std::fs::read_to_string(&path)
        .with_context(|| format!("Failed to read {}", path.display()))?;
    if !content.ends_with('\n') {
        content.push('\n');
    }
    for block in blocks {
        content.push_str(block);
    }
    std::fs::write(&path, &content)
        .with_context(|| format!("Failed to write {}", path.display()))?;
    Ok(path)
}

// ─── Prompting ───────────────────────────────────────────────────────────────

fn prompt(message: &str) -> Result<String> {
    print!("{message}");
    io::stdout().flush()?;
    let mut line = String::new();
    let n = io::stdin().read_line(&mut line)?;
    if n == 0 {
        // EOF (e.g. piped/non-interactive): treat as an empty answer.
        return Ok(String::new());
    }
    Ok(line.trim().to_string())
}

fn prompt_yes_no(message: &str, default: bool) -> Result<bool> {
    let hint = if default { "[Y/n]" } else { "[y/N]" };
    let answer = prompt(&format!("{message} {hint}: "))?;
    if answer.is_empty() {
        return Ok(default);
    }
    Ok(is_yes(&answer))
}

fn is_yes(s: &str) -> bool {
    matches!(s.trim().to_lowercase().as_str(), "y" | "yes")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selection_parsing() {
        assert_eq!(parse_selection("", 3), Vec::<usize>::new());
        assert_eq!(parse_selection("all", 3), vec![0, 1, 2]);
        assert_eq!(parse_selection("1,3", 3), vec![0, 2]);
        assert_eq!(parse_selection("2 2 9", 3), vec![1]); // dedup + out-of-range drop
    }

    #[test]
    fn uniquify_avoids_collisions() {
        let mut used: BTreeSet<String> = ["app_to_ecr".to_string()].into_iter().collect();
        assert_eq!(uniquify("app_to_ecr", &mut used), "app_to_ecr_2");
        assert_eq!(uniquify("app_to_ecr", &mut used), "app_to_ecr_3");
        assert_eq!(uniquify("Fresh-Name", &mut used), "fresh_name");
    }

    #[test]
    fn manifest_flag_only_for_non_root() {
        assert_eq!(manifest_flag("Cargo.toml"), "");
        assert_eq!(
            manifest_flag("crates/foo/Cargo.toml"),
            " --manifest-path crates/foo/Cargo.toml"
        );
    }
}
