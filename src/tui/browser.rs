/// Interactive registry browser TUI.
///
/// Shows the registries this project can reach and lets you walk what has been
/// published to them. Publishing itself is a workflow step (`kind: push`), so
/// it is run the way every other step is — `ciabatta <workflow>` — rather than
/// from here.
///
/// Layout:
///   ┌─ Logo ───────────────────────────────────────────┐
///   ├─ Registries ─────────────────────────────────────┤
///   │  ○ nexus                                         │
///   │  ○ docker                                        │
///   │  ○ ecr                                           │
///   ├──────────────────────────────────────────────────┤
///   │  Detail panel                                     │
///   ├──────────────────────────────────────────────────┤
///   │  [e] explore  [↑/↓] select  [q] quit              │
///   └──────────────────────────────────────────────────┘
use std::collections::HashMap;
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

// ─── State ───────────────────────────────────────────────────────────────────

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
    pub reg_state: ListState,
    pub log_scroll: usize,
    pub status_msg: Option<String>,
    pub explorer: Option<Explorer>,
}

impl BrowserApp {
    pub fn new(config: &CiabattaConfig) -> Self {
        let registries = build_registry_rows(config);

        let mut reg_state = ListState::default();
        if !registries.is_empty() {
            reg_state.select(Some(0));
        }

        BrowserApp {
            registries,
            reg_state,
            log_scroll: 0,
            status_msg: None,
            explorer: None,
        }
    }

    pub fn selected_registry_idx(&self) -> Option<usize> {
        self.reg_state.selected()
    }

    pub fn move_down(&mut self) {
        list_next(&mut self.reg_state, self.registries.len());
        self.log_scroll = 0;
    }

    pub fn move_up(&mut self) {
        list_prev(&mut self.reg_state);
        self.log_scroll = 0;
    }

    pub fn scroll_log_down(&mut self) {
        self.log_scroll = self.log_scroll.saturating_add(1);
    }
    pub fn scroll_log_up(&mut self) {
        self.log_scroll = self.log_scroll.saturating_sub(1);
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
    let title = format!(
        " Explore {}{}  {} ",
        explorer.registry_name, auth, path_display
    );

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
    render_registries(f, area, app);
}

fn render_registries(f: &mut Frame, area: Rect, app: &BrowserApp) {
    let focused = true;
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

fn render_detail(f: &mut Frame, area: Rect, app: &BrowserApp) {
    render_registry_detail(f, area, app);
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

fn render_help(f: &mut Frame, area: Rect, app: &BrowserApp) {
    let status = app.status_msg.as_deref().unwrap_or("");
    let text = format!(" [↑↓/jk] nav  [e] explore  [q] quit   {status}");
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

// ─── Event loop ──────────────────────────────────────────────────────────────

pub async fn run_browser(
    config: CiabattaConfig,
    _root: std::path::PathBuf,
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

    let result = browser_loop(&mut terminal, config, env_vars).await;

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    result
}

async fn browser_loop(
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    config: CiabattaConfig,
    env_vars: HashMap<String, String>,
) -> Result<()> {
    let mut app = BrowserApp::new(&config);
    let mut event_stream = EventStream::new();

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
                    KeyCode::Down | KeyCode::Char('j') => app.move_down(),
                    KeyCode::Up   | KeyCode::Char('k') => app.move_up(),
                    KeyCode::PageDown | KeyCode::Char('J') => app.scroll_log_down(),
                    KeyCode::PageUp   | KeyCode::Char('K') => app.scroll_log_up(),

                    KeyCode::Char('e') => {
                        open_explorer(&mut app, &env_vars, &explore_tx);
                    }

                    _ => {}
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
            let descend = explorer
                .selected()
                .filter(|e| e.is_dir)
                .map(|e| e.name.clone());
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
