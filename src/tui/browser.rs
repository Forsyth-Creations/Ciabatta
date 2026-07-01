/// Interactive registry/recipe browser TUI.
///
/// Layout:
///   ┌─ Logo ───────────────────────────────────────────┐
///   ├─ Registries ──────────┬─ Recipes ────────────────┤
///   │  ○ nexus              │  ○ release_frontend  ✓   │
///   │  ○ docker             │  ○ release_backend   -   │
///   │  ○ ecr                │                          │
///   ├───────────────────────┴──────────────────────────┤
///   │  Detail / Log panel                               │
///   ├──────────────────────────────────────────────────┤
///   │  [Tab] switch pane  [p] push  [d] dry-run  [q] quit│
///   └──────────────────────────────────────────────────┘
use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

use anyhow::Result;
use crossterm::event::{Event, EventStream, KeyCode, KeyModifiers};
use futures::StreamExt;
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap},
};
use tokio::sync::mpsc;

use crate::config::CiabattaConfig;
use crate::registry::browse::{self, Entry};
use crate::runner::{self, ProgressUpdate, RunMode};

// ─── State ───────────────────────────────────────────────────────────────────

#[derive(Clone, PartialEq)]
enum Pane {
    Registries,
    Recipes,
}

#[derive(Clone, PartialEq)]
pub enum RecipeRunStatus {
    Idle,
    Running,
    Success,
    Failed(String),
}

pub struct RecipeRow {
    pub name: String,
    /// Registry this recipe targets (if any).
    pub registry: Option<String>,
    /// Resolved local artifact path exists on disk.
    pub local_exists: Option<bool>,
    /// Publish path template (unresolved).
    pub publish_path: Option<String>,
    pub kind: &'static str,
    pub run_status: RecipeRunStatus,
    pub logs: Vec<String>,
}

pub struct RegistryRow {
    pub name: String,
    pub url: String,
    pub needs_auth: bool,
    pub has_login_script: bool,
    pub kind: String,
    /// Verify TLS when browsing (mirrors the registry config).
    pub tls_verify: bool,
    /// Whether this registry exposes a browsable HTTP directory index
    /// (Nexus / Artifactory / generic HTTP registries).
    pub browsable: bool,
}

/// Overlay state for browsing a registry's remote contents.
pub struct Explorer {
    /// Registry section name (for credential lookup and the title).
    pub registry_name: String,
    /// Registry base URL (trailing slash trimmed at use).
    pub base_url: String,
    pub tls_verify: bool,
    /// HTTP basic-auth credentials, if the registry has them configured.
    pub creds: Option<(String, String)>,
    /// Current path relative to the registry root (no leading/trailing slash).
    pub path: String,
    pub entries: Vec<Entry>,
    pub state: ListState,
    pub loading: bool,
    pub error: Option<String>,
}

impl Explorer {
    fn move_down(&mut self) {
        list_next(&mut self.state, self.entries.len());
    }
    fn move_up(&mut self) {
        list_prev(&mut self.state);
    }
    fn selected(&self) -> Option<&Entry> {
        self.state.selected().and_then(|i| self.entries.get(i))
    }
    /// Ascend one path segment. Returns true if the path actually changed.
    fn go_up(&mut self) -> bool {
        if self.path.is_empty() {
            return false;
        }
        self.path = match self.path.rsplit_once('/') {
            Some((parent, _)) => parent.to_string(),
            None => String::new(),
        };
        true
    }
}

/// Result of a background listing fetch, routed back to the event loop.
struct ExploreResult {
    /// The path this listing is for (guards against stale responses).
    path: String,
    result: Result<Vec<Entry>, String>,
}

pub struct BrowserApp {
    pub registries: Vec<RegistryRow>,
    pub recipes: Vec<RecipeRow>,
    pub reg_state: ListState,
    pub rec_state: ListState,
    active_pane: Pane,
    pub log_scroll: usize,
    pub status_msg: Option<String>,
    pub explorer: Option<Explorer>,
}

impl BrowserApp {
    pub fn new(config: &CiabattaConfig, root: &Path, env_vars: &HashMap<String, String>) -> Self {
        let registries = build_registry_rows(config);
        let recipes = build_recipe_rows(config, root, env_vars);

        let mut reg_state = ListState::default();
        if !registries.is_empty() {
            reg_state.select(Some(0));
        }
        let mut rec_state = ListState::default();
        if !recipes.is_empty() {
            rec_state.select(Some(0));
        }

        BrowserApp {
            registries,
            recipes,
            reg_state,
            rec_state,
            active_pane: Pane::Registries,
            log_scroll: 0,
            status_msg: None,
            explorer: None,
        }
    }

    pub fn selected_recipe_idx(&self) -> Option<usize> {
        self.rec_state.selected()
    }

    pub fn selected_registry_idx(&self) -> Option<usize> {
        self.reg_state.selected()
    }

    pub fn toggle_pane(&mut self) {
        self.active_pane = match self.active_pane {
            Pane::Registries => Pane::Recipes,
            Pane::Recipes => Pane::Registries,
        };
    }

    pub fn move_down(&mut self) {
        match self.active_pane {
            Pane::Registries => list_next(&mut self.reg_state, self.registries.len()),
            Pane::Recipes => list_next(&mut self.rec_state, self.recipes.len()),
        }
        self.log_scroll = 0;
    }

    pub fn move_up(&mut self) {
        match self.active_pane {
            Pane::Registries => list_prev(&mut self.reg_state),
            Pane::Recipes => list_prev(&mut self.rec_state),
        }
        self.log_scroll = 0;
    }

    pub fn scroll_log_down(&mut self) {
        self.log_scroll = self.log_scroll.saturating_add(1);
    }
    pub fn scroll_log_up(&mut self) {
        self.log_scroll = self.log_scroll.saturating_sub(1);
    }

    pub fn apply_update(&mut self, update: ProgressUpdate) {
        match update {
            ProgressUpdate::Started(ref name) => {
                if let Some(r) = self.recipes.iter_mut().find(|r| &r.name == name) {
                    r.run_status = RecipeRunStatus::Running;
                    r.logs.clear();
                }
            }
            ProgressUpdate::StageStarted { ref recipe, stage } => {
                self.status_msg = Some(format!("{} → {}", recipe, stage.label(RunMode::Push)));
            }
            ProgressUpdate::StageFinished { .. } => {}
            ProgressUpdate::Log(ref name, ref line) => {
                if let Some(r) = self.recipes.iter_mut().find(|r| &r.name == name) {
                    r.logs.push(line.clone());
                }
            }
            ProgressUpdate::Completed(ref name) => {
                if let Some(r) = self.recipes.iter_mut().find(|r| &r.name == name) {
                    r.run_status = RecipeRunStatus::Success;
                    r.local_exists = Some(true); // optimistic after push
                }
                self.status_msg = Some(format!("✓ {} completed", name));
            }
            ProgressUpdate::Failed(ref name, ref err) => {
                if let Some(r) = self.recipes.iter_mut().find(|r| &r.name == name) {
                    r.run_status = RecipeRunStatus::Failed(err.clone());
                }
                self.status_msg = Some(format!("✗ {} failed: {}", name, err));
            }
        }
    }
}

fn list_next(state: &mut ListState, len: usize) {
    if len == 0 {
        return;
    }
    let next = state.selected().map(|i| (i + 1) % len).unwrap_or(0);
    state.select(Some(next));
}

fn list_prev(state: &mut ListState) {
    if let Some(i) = state.selected() {
        state.select(Some(i.saturating_sub(1)));
    }
}

fn build_registry_rows(config: &CiabattaConfig) -> Vec<RegistryRow> {
    let mut rows: Vec<_> = config
        .registries
        .iter()
        .map(|(name, cfg)| {
            let kind = crate::config::infer_registry_kind(name, cfg);
            let browsable = matches!(
                kind,
                crate::config::RegistryKind::Nexus
                    | crate::config::RegistryKind::Artifactory
                    | crate::config::RegistryKind::Generic
            );
            RegistryRow {
                name: name.clone(),
                url: cfg.url.clone(),
                needs_auth: cfg.needs_auth,
                has_login_script: cfg.login_script.is_some(),
                kind: format!("{:?}", kind).to_lowercase(),
                tls_verify: cfg.tls_verify,
                browsable,
            }
        })
        .collect();
    rows.sort_by(|a, b| a.name.cmp(&b.name));
    rows
}

fn build_recipe_rows(
    config: &CiabattaConfig,
    root: &Path,
    _env: &HashMap<String, String>,
) -> Vec<RecipeRow> {
    let mut rows: Vec<_> = config
        .recipes
        .iter()
        .map(|(name, entry)| {
            let push = entry.push_recipe();
            let kind: &'static str = if entry.push.is_some() || entry.pull.is_some() {
                "push/pull"
            } else if push.main.is_some() || push.bash_script.is_some() {
                "command"
            } else {
                "registry"
            };
            let local_path = push.local_artifact_path.as_deref();
            let local_exists = local_path.map(|p| root.join(p).exists());
            RecipeRow {
                name: name.clone(),
                registry: push.registry.clone(),
                local_exists,
                publish_path: push.publish_path.as_ref().map(|p| p.display()),
                kind,
                run_status: RecipeRunStatus::Idle,
                logs: Vec::new(),
            }
        })
        .collect();
    rows.sort_by(|a, b| a.name.cmp(&b.name));
    rows
}

// ─── Rendering ───────────────────────────────────────────────────────────────

const LOGO: &str = concat!(
    " ██████╗██╗ █████╗ ██████╗  █████╗ ████████╗████████╗ █████╗ \n",
    "██╔════╝██║██╔══██╗██╔══██╗██╔══██╗╚══██╔══╝╚══██╔══╝██╔══██╗\n",
    "██║     ██║███████║██████╔╝███████║   ██║      ██║   ███████║ \n",
    "██║     ██║██╔══██║██╔══██╗██╔══██║   ██║      ██║   ██╔══██║ \n",
    "╚██████╗██║██║  ██║██████╔╝██║  ██║   ██║      ██║   ██║  ██║ \n",
    " ╚═════╝╚═╝╚═╝  ╚═╝╚═════╝ ╚═╝  ╚═╝  ╚═╝      ╚═╝   ╚═╝  ╚═╝ \n",
);

pub fn render(f: &mut Frame, app: &BrowserApp) {
    let area = f.area();

    let logo_h = 7u16;
    let help_h = 1u16;
    let detail_h = 8u16;

    let chunks = Layout::vertical([
        Constraint::Length(logo_h),
        Constraint::Min(6),
        Constraint::Length(detail_h),
        Constraint::Length(help_h),
    ])
    .split(area);

    render_logo(f, chunks[0]);
    render_middle(f, chunks[1], app);
    render_detail(f, chunks[2], app);
    render_help(f, chunks[3], app);

    // The repository explorer, when open, floats above everything else.
    if let Some(ref explorer) = app.explorer {
        render_explorer(f, area, explorer);
    }
}

/// Draw the registry repository explorer as a centered floating popup.
fn render_explorer(f: &mut Frame, area: Rect, explorer: &Explorer) {
    let popup = centered_rect(80, 80, area);
    f.render_widget(Clear, popup);

    let path_display = if explorer.path.is_empty() {
        "/".to_string()
    } else {
        format!("/{}/", explorer.path)
    };
    let auth = if explorer.creds.is_some() {
        " 🔐"
    } else {
        ""
    };
    let title = format!(" Explore {}{}  {} ", explorer.registry_name, auth, path_display);

    let block = Block::default()
        .title(Span::styled(
            title,
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Yellow));

    let inner = block.inner(popup);
    f.render_widget(block, popup);

    let rows = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).split(inner);

    if explorer.loading {
        f.render_widget(
            Paragraph::new("  Loading…").style(Style::default().fg(Color::Yellow)),
            rows[0],
        );
    } else if let Some(ref err) = explorer.error {
        f.render_widget(
            Paragraph::new(format!("  Error: {err}"))
                .style(Style::default().fg(Color::Red))
                .wrap(Wrap { trim: true }),
            rows[0],
        );
    } else if explorer.entries.is_empty() {
        f.render_widget(
            Paragraph::new("  (empty)").style(Style::default().fg(Color::DarkGray)),
            rows[0],
        );
    } else {
        let items: Vec<ListItem> = explorer
            .entries
            .iter()
            .map(|e| {
                let (icon, color) = if e.is_dir {
                    ("📁 ", Color::Blue)
                } else {
                    ("📄 ", Color::Gray)
                };
                let name = if e.is_dir {
                    format!("{}/", e.name)
                } else {
                    e.name.clone()
                };
                ListItem::new(Line::from(vec![
                    Span::raw(icon),
                    Span::styled(name, Style::default().fg(color)),
                ]))
            })
            .collect();

        let list = List::new(items)
            .highlight_style(
                Style::default()
                    .bg(Color::DarkGray)
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            )
            .highlight_symbol("▶ ");
        let mut state = explorer.state.clone();
        f.render_stateful_widget(list, rows[0], &mut state);
    }

    let footer =
        " [↑↓/jk] navigate  [Enter/→] open folder  [←/Backspace] up  [r] refresh  [Esc] close";
    f.render_widget(
        Paragraph::new(footer).style(Style::default().fg(Color::DarkGray)),
        rows[1],
    );
}

/// Compute a rectangle centered within `r`, sized as a percentage of it.
fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let vertical = Layout::vertical([
        Constraint::Percentage((100 - percent_y) / 2),
        Constraint::Percentage(percent_y),
        Constraint::Percentage((100 - percent_y) / 2),
    ])
    .split(r);
    Layout::horizontal([
        Constraint::Percentage((100 - percent_x) / 2),
        Constraint::Percentage(percent_x),
        Constraint::Percentage((100 - percent_x) / 2),
    ])
    .split(vertical[1])[1]
}

fn render_logo(f: &mut Frame, area: Rect) {
    let p = Paragraph::new(LOGO).style(
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
    );
    f.render_widget(p, area);
}

fn render_middle(f: &mut Frame, area: Rect, app: &BrowserApp) {
    let cols =
        Layout::horizontal([Constraint::Percentage(30), Constraint::Percentage(70)]).split(area);

    render_registries(f, cols[0], app);
    render_recipes(f, cols[1], app);
}

fn render_registries(f: &mut Frame, area: Rect, app: &BrowserApp) {
    let focused = matches!(app.active_pane, Pane::Registries);
    let border_style = pane_border_style(focused);

    let block = Block::default()
        .title(Span::styled(
            " Registries ",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL)
        .border_style(border_style);

    let items: Vec<ListItem> = app
        .registries
        .iter()
        .map(|r| {
            let auth_icon = if r.needs_auth { "🔐" } else { "🔓" };
            let line = Line::from(vec![
                Span::styled(
                    format!(" {:12} ", r.name),
                    Style::default()
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!("[{}] ", r.kind),
                    Style::default().fg(Color::DarkGray),
                ),
                Span::raw(auth_icon),
            ]);
            ListItem::new(line)
        })
        .collect();

    let list = List::new(items)
        .block(block)
        .highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▶ ");

    let mut state = app.reg_state.clone();
    f.render_stateful_widget(list, area, &mut state);
}

fn render_recipes(f: &mut Frame, area: Rect, app: &BrowserApp) {
    let focused = matches!(app.active_pane, Pane::Recipes);
    let border_style = pane_border_style(focused);

    let block = Block::default()
        .title(Span::styled(
            " Recipes ",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL)
        .border_style(border_style);

    let items: Vec<ListItem> = app
        .recipes
        .iter()
        .map(|r| {
            let (status_sym, status_color) = run_status_style(&r.run_status);
            let local_sym = match r.local_exists {
                Some(true) => Span::styled("✓ ", Style::default().fg(Color::Green)),
                Some(false) => Span::styled("✗ ", Style::default().fg(Color::Red)),
                None => Span::styled("? ", Style::default().fg(Color::DarkGray)),
            };
            let reg_label = r.registry.as_deref().unwrap_or("-");

            let line = Line::from(vec![
                Span::styled(
                    format!(" {:28} ", r.name),
                    Style::default()
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!("[{:10}] ", reg_label),
                    Style::default().fg(Color::Blue),
                ),
                Span::styled(
                    format!("[{:9}] ", r.kind),
                    Style::default().fg(Color::DarkGray),
                ),
                Span::raw("local:"),
                local_sym,
                Span::styled(status_sym, Style::default().fg(status_color)),
            ]);
            ListItem::new(line)
        })
        .collect();

    let list = List::new(items)
        .block(block)
        .highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▶ ");

    let mut state = app.rec_state.clone();
    f.render_stateful_widget(list, area, &mut state);
}

fn render_detail(f: &mut Frame, area: Rect, app: &BrowserApp) {
    match app.active_pane {
        Pane::Registries => render_registry_detail(f, area, app),
        Pane::Recipes => render_recipe_detail(f, area, app),
    }
}

fn render_registry_detail(f: &mut Frame, area: Rect, app: &BrowserApp) {
    let block = Block::default()
        .title(" Registry Detail ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray));

    let text = if let Some(idx) = app.selected_registry_idx() {
        if let Some(reg) = app.registries.get(idx) {
            let auth_status = if reg.needs_auth {
                if reg.has_login_script {
                    "login script configured"
                } else {
                    "needs auth — no login script!"
                }
            } else {
                "no auth required"
            };
            format!(
                "  Name:    {}\n  URL:     {}\n  Type:    {}\n  Auth:    {}\n",
                reg.name, reg.url, reg.kind, auth_status
            )
        } else {
            String::new()
        }
    } else {
        "  No registry selected.".to_string()
    };

    let p = Paragraph::new(text)
        .block(block)
        .style(Style::default().fg(Color::Gray));
    f.render_widget(p, area);
}

fn render_recipe_detail(f: &mut Frame, area: Rect, app: &BrowserApp) {
    let idx = match app.selected_recipe_idx() {
        Some(i) => i,
        None => {
            let p = Paragraph::new("  No recipe selected.").block(
                Block::default()
                    .title(" Recipe Detail ")
                    .borders(Borders::ALL),
            );
            f.render_widget(p, area);
            return;
        }
    };

    let recipe = match app.recipes.get(idx) {
        Some(r) => r,
        None => return,
    };

    let inner_area = area;
    let chunks = Layout::horizontal([Constraint::Percentage(40), Constraint::Percentage(60)])
        .split(inner_area);

    // Left: metadata
    let meta_block = Block::default()
        .title(format!(" {} ", recipe.name))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray));

    let local_status = match recipe.local_exists {
        Some(true) => "✓ exists".to_string(),
        Some(false) => "✗ missing".to_string(),
        None => "(bash script)".to_string(),
    };
    let publish = recipe.publish_path.as_deref().unwrap_or("(none)");
    let (status_sym, _) = run_status_style(&recipe.run_status);
    let meta_text = format!(
        "  Kind:         {}\n  Registry:     {}\n  Local path:   {}\n  Publish path: {}\n  Status:       {}",
        recipe.kind,
        recipe.registry.as_deref().unwrap_or("-"),
        local_status,
        publish,
        status_sym.trim(),
    );
    let meta = Paragraph::new(meta_text)
        .block(meta_block)
        .style(Style::default().fg(Color::Gray));
    f.render_widget(meta, chunks[0]);

    // Right: logs
    let log_block = Block::default()
        .title(" Logs ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray));

    let log_inner_h = chunks[1].height.saturating_sub(2) as usize;
    let logs = &recipe.logs;
    let skip = if logs.len() > log_inner_h {
        logs.len() - log_inner_h
    } else {
        0
    };
    let skip = skip
        .saturating_add(app.log_scroll)
        .min(logs.len().saturating_sub(1));

    let log_items: Vec<ListItem> = logs
        .iter()
        .skip(skip)
        .map(|line| {
            let style = if line.starts_with("[stderr]") || line.to_lowercase().contains("error") {
                Style::default().fg(Color::Red)
            } else if line.starts_with("[dry-run]") || line.starts_with('+') {
                Style::default().fg(Color::Yellow)
            } else {
                Style::default().fg(Color::Gray)
            };
            ListItem::new(Line::from(Span::styled(line.as_str(), style)))
        })
        .collect();

    let log_list = List::new(log_items).block(log_block);
    f.render_widget(log_list, chunks[1]);
}

fn render_help(f: &mut Frame, area: Rect, app: &BrowserApp) {
    let status = app.status_msg.as_deref().unwrap_or("");
    let focused_pane = match app.active_pane {
        Pane::Registries => "Registries",
        Pane::Recipes => "Recipes",
    };
    let text = format!(
        " [Tab] switch pane ({})  [↑↓/jk] nav  [e] explore  [p] push  [P] dry-run  [r] refresh  [q] quit   {}",
        focused_pane, status
    );
    let style = if status.starts_with('✗') {
        Style::default().fg(Color::Red)
    } else if status.starts_with('✓') {
        Style::default().fg(Color::Green)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    f.render_widget(Paragraph::new(text).style(style), area);
}

fn pane_border_style(focused: bool) -> Style {
    if focused {
        Style::default().fg(Color::Yellow)
    } else {
        Style::default().fg(Color::DarkGray)
    }
}

fn run_status_style(status: &RecipeRunStatus) -> (&'static str, Color) {
    match status {
        RecipeRunStatus::Idle => ("idle", Color::DarkGray),
        RecipeRunStatus::Running => ("running…", Color::Yellow),
        RecipeRunStatus::Success => ("✓ done", Color::Green),
        RecipeRunStatus::Failed(_) => ("✗ failed", Color::Red),
    }
}

// ─── Event loop ──────────────────────────────────────────────────────────────

pub async fn run_browser(
    config: CiabattaConfig,
    root: std::path::PathBuf,
    env_vars: HashMap<String, String>,
) -> Result<()> {
    use crossterm::{
        execute,
        terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
    };
    use std::io;

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = browser_loop(&mut terminal, config, root, env_vars).await;

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    result
}

async fn browser_loop(
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    config: CiabattaConfig,
    root: std::path::PathBuf,
    env_vars: HashMap<String, String>,
) -> Result<()> {
    let mut app = BrowserApp::new(&config, &root, &env_vars);
    let mut event_stream = EventStream::new();

    // Channel for progress updates from background push tasks.
    let (prog_tx, mut prog_rx) = mpsc::channel::<ProgressUpdate>(256);
    // Channel for directory listings from background browse tasks.
    let (explore_tx, mut explore_rx) = mpsc::channel::<ExploreResult>(16);

    loop {
        terminal.draw(|f| render(f, &app))?;

        tokio::select! {
            maybe_event = event_stream.next() => {
                let Some(Ok(Event::Key(key))) = maybe_event else { continue; };
                let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

                // When the explorer overlay is open it captures all keys.
                if app.explorer.is_some() {
                    if ctrl && key.code == KeyCode::Char('c') {
                        break;
                    }
                    handle_explorer_key(&mut app, key.code, &explore_tx);
                    continue;
                }

                match key.code {
                    KeyCode::Char('q') | KeyCode::Esc => break,
                    KeyCode::Char('c') if ctrl => break,
                    KeyCode::Tab => app.toggle_pane(),
                    KeyCode::Down | KeyCode::Char('j') => app.move_down(),
                    KeyCode::Up   | KeyCode::Char('k') => app.move_up(),
                    KeyCode::PageDown | KeyCode::Char('J') => app.scroll_log_down(),
                    KeyCode::PageUp   | KeyCode::Char('K') => app.scroll_log_up(),

                    KeyCode::Char('e') => {
                        open_explorer(&mut app, &env_vars, &explore_tx);
                    }

                    KeyCode::Char('p') => {
                        if let Some(name) = selected_recipe_name(&app) {
                            push_recipe(name, &config, &root, &env_vars, false, prog_tx.clone());
                            app.status_msg = Some("Pushing…".to_string());
                        }
                    }
                    KeyCode::Char('P') => {
                        if let Some(name) = selected_recipe_name(&app) {
                            push_recipe(name, &config, &root, &env_vars, true, prog_tx.clone());
                            app.status_msg = Some("[dry-run] Pushing…".to_string());
                        }
                    }
                    KeyCode::Char('r') => {
                        // Refresh local-path existence checks.
                        for recipe in &mut app.recipes {
                            if let Some(ref local) = {
                                let cfg_recipe = config.recipes.get(&recipe.name);
                                cfg_recipe.and_then(|e| e.push_recipe().local_artifact_path.clone())
                            } {
                                recipe.local_exists = Some(root.join(local).exists());
                            }
                        }
                        app.status_msg = Some("Refreshed.".to_string());
                    }
                    _ => {}
                }
            }

            maybe_update = prog_rx.recv() => {
                if let Some(update) = maybe_update {
                    app.apply_update(update);
                }
            }

            maybe_listing = explore_rx.recv() => {
                if let Some(listing) = maybe_listing {
                    apply_listing(&mut app, listing);
                }
            }

            _ = tokio::time::sleep(Duration::from_millis(50)) => {}
        }
    }

    Ok(())
}

/// Open the repository explorer overlay for the currently selected registry.
fn open_explorer(
    app: &mut BrowserApp,
    env_vars: &HashMap<String, String>,
    tx: &mpsc::Sender<ExploreResult>,
) {
    let Some(reg) = app
        .selected_registry_idx()
        .and_then(|i| app.registries.get(i))
    else {
        return;
    };

    if !reg.browsable {
        app.status_msg = Some(format!(
            "'{}' ({}) isn't an HTTP-browsable registry",
            reg.name, reg.kind
        ));
        return;
    }

    let creds = crate::registry::registry_credentials(&reg.name, env_vars);
    let explorer = Explorer {
        registry_name: reg.name.clone(),
        base_url: reg.url.clone(),
        tls_verify: reg.tls_verify,
        creds,
        path: String::new(),
        entries: Vec::new(),
        state: ListState::default(),
        loading: true,
        error: None,
    };
    fetch_listing(&explorer, tx);
    app.explorer = Some(explorer);
    app.status_msg = None;
}

/// Route a key press to the open explorer overlay.
fn handle_explorer_key(app: &mut BrowserApp, code: KeyCode, tx: &mpsc::Sender<ExploreResult>) {
    let Some(explorer) = app.explorer.as_mut() else {
        return;
    };

    match code {
        KeyCode::Esc | KeyCode::Char('q') => {
            app.explorer = None;
        }
        KeyCode::Down | KeyCode::Char('j') => explorer.move_down(),
        KeyCode::Up | KeyCode::Char('k') => explorer.move_up(),
        KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => {
            // Descend into the selected folder.
            let descend = explorer.selected().filter(|e| e.is_dir).map(|e| e.name.clone());
            if let Some(seg) = descend {
                explorer.path = if explorer.path.is_empty() {
                    seg
                } else {
                    format!("{}/{}", explorer.path, seg)
                };
                start_navigation(explorer, tx);
            }
        }
        KeyCode::Backspace | KeyCode::Left | KeyCode::Char('h') | KeyCode::Char('u') => {
            if explorer.go_up() {
                start_navigation(explorer, tx);
            }
        }
        KeyCode::Char('r') => {
            explorer.loading = true;
            explorer.error = None;
            fetch_listing(explorer, tx);
        }
        _ => {}
    }
}

/// Reset listing state and kick off a fetch after the path changed.
fn start_navigation(explorer: &mut Explorer, tx: &mpsc::Sender<ExploreResult>) {
    explorer.loading = true;
    explorer.error = None;
    explorer.entries.clear();
    explorer.state.select(None);
    fetch_listing(explorer, tx);
}

/// Spawn a background task to fetch the listing for the explorer's current path.
fn fetch_listing(explorer: &Explorer, tx: &mpsc::Sender<ExploreResult>) {
    let url = browse::listing_url(&explorer.base_url, &explorer.path);
    let tls_verify = explorer.tls_verify;
    let creds = explorer.creds.clone();
    let path = explorer.path.clone();
    let tx = tx.clone();
    tokio::spawn(async move {
        let result = browse::list_http(&url, tls_verify, creds)
            .await
            .map_err(|e| e.to_string());
        let _ = tx.send(ExploreResult { path, result }).await;
    });
}

/// Apply a fetched listing to the explorer, ignoring responses for a path the
/// user has already navigated away from.
fn apply_listing(app: &mut BrowserApp, listing: ExploreResult) {
    let Some(explorer) = app.explorer.as_mut() else {
        return;
    };
    if explorer.path != listing.path {
        return;
    }

    explorer.loading = false;
    match listing.result {
        Ok(entries) => {
            explorer.error = None;
            explorer.state.select((!entries.is_empty()).then_some(0));
            explorer.entries = entries;
        }
        Err(e) => {
            explorer.error = Some(e);
            explorer.entries.clear();
            explorer.state.select(None);
        }
    }
}

fn selected_recipe_name(app: &BrowserApp) -> Option<String> {
    app.rec_state
        .selected()
        .and_then(|i| app.recipes.get(i))
        .map(|r| r.name.clone())
}

fn push_recipe(
    name: String,
    config: &CiabattaConfig,
    root: &std::path::Path,
    env_vars: &HashMap<String, String>,
    dry_run: bool,
    tx: mpsc::Sender<ProgressUpdate>,
) {
    let config = config.clone();
    let root = root.to_path_buf();
    let env_vars = env_vars.clone();

    tokio::spawn(async move {
        let names = vec![name];
        let _ = runner::run_all(
            &config,
            &root,
            &names,
            &env_vars,
            dry_run,
            RunMode::Push,
            tx,
        )
        .await;
    });
}
