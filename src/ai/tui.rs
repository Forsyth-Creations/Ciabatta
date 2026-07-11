//! The Ratatui chat front end for the AI assistant.
//!
//! One screen: a header with the live confidence gauge, the conversation, a
//! banner for pending tag confirmations, and an input line.
//!
//! Keys:
//!   Enter        send the typed question
//!   Shift-Tab         cycle the mode: plan → edit → auto-accept
//!   Ctrl-A / Ctrl-X   apply / reject the first pending change proposal
//!   Ctrl-O            open the suggested changes as diffs in VS Code
//!   Ctrl-Y / Ctrl-N   accept / reject the first pending tag proposal
//!   Ctrl-G / Ctrl-B   rate the last answer good / bad (trains confidence)
//!   PageUp/PageDown · Up/Down · mouse wheel   scroll the conversation
//!   Home / End        jump to the oldest / newest message
//!   Esc / Ctrl-C      quit

use std::io;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use crossterm::{
    event::{
        DisableMouseCapture, EnableMouseCapture, Event, EventStream, KeyCode, KeyEventKind,
        KeyModifiers, MouseEventKind,
    },
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use futures::StreamExt;
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Gauge, Paragraph, Wrap},
};
use tokio::sync::mpsc;

use super::tools::{ChangeState, ChangeSuggestion};
use super::{AiEvent, Assistant, Mode};

/// Who "said" a chat entry, which controls its styling.
#[derive(Clone, Copy, PartialEq)]
enum Speaker {
    You,
    Assistant,
    Status,
    Error,
    /// A unified diff of a proposed change; rendered with +/- coloring.
    Diff,
}

struct ChatEntry {
    speaker: Speaker,
    text: String,
}

struct App {
    entries: Vec<ChatEntry>,
    input: String,
    busy: bool,
    /// Lines scrolled up from the bottom of the conversation.
    scroll_up: u16,
    /// Topmost reachable `scroll_up`, measured on the last frame; scrolling is
    /// clamped to it so the view can't run past the oldest message.
    chat_top: u16,
    /// Change proposals seen this session (newest last), for Ctrl-O.
    suggestions: Vec<ChangeSuggestion>,
    graph_url: Option<String>,
}

impl App {
    /// Append a message. The view stays put if the user is reading scrollback
    /// (render_chat re-anchors it); it only follows new messages at the bottom.
    fn push(&mut self, speaker: Speaker, text: impl Into<String>) {
        self.entries.push(ChatEntry {
            speaker,
            text: text.into(),
        });
    }

    /// Scroll up (`+`) or down (`-`), clamped to the conversation.
    fn scroll_by(&mut self, delta: i32) {
        self.scroll_up = (i32::from(self.scroll_up) + delta).clamp(0, i32::from(self.chat_top))
            as u16;
    }
}

/// Run the chat TUI until the user quits.
pub async fn run(assistant: Arc<Assistant>, graph_url: Option<String>) -> Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = chat_loop(&mut terminal, assistant, graph_url).await;

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), DisableMouseCapture, LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    result
}

async fn chat_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    assistant: Arc<Assistant>,
    graph_url: Option<String>,
) -> Result<()> {
    let mut app = App {
        entries: Vec::new(),
        input: String::new(),
        busy: false,
        scroll_up: 0,
        chat_top: 0,
        suggestions: Vec::new(),
        graph_url,
    };
    app.push(
        Speaker::Status,
        format!(
            "connected to {} — ask about this codebase. It will search, read, and \
             tag files as it learns.",
            assistant.provider.label()
        ),
    );
    if let Some(url) = &app.graph_url {
        app.push(Speaker::Status, format!("live mind map: {url}"));
    }

    // Replay a resumed conversation onto the screen so the user picks up where
    // they left off.
    let transcript = assistant.transcript().await;
    if !transcript.is_empty() {
        app.push(
            Speaker::Status,
            format!("resumed conversation {} — {} earlier turns below", assistant.conversation_id().await, transcript.len()),
        );
        for turn in &transcript {
            match turn {
                super::provider::Turn::User(text) => app.push(Speaker::You, text.clone()),
                super::provider::Turn::Assistant(a) => {
                    if !a.text.trim().is_empty() {
                        app.push(Speaker::Assistant, a.text.clone());
                    }
                    for call in &a.tool_calls {
                        app.push(Speaker::Status, super::tools::ToolBox::describe(call));
                    }
                }
                super::provider::Turn::ToolResults(_) => {}
            }
        }
        app.push(Speaker::Status, "— end of resumed history —".to_string());
    }

    let (tx, mut rx) = mpsc::channel::<AiEvent>(64);
    let mut event_stream = EventStream::new();

    loop {
        terminal.draw(|f| render(f, &mut app, &assistant))?;

        let sleep = tokio::time::sleep(Duration::from_millis(120));
        tokio::select! {
            maybe_event = event_stream.next() => {
                let Some(Ok(event)) = maybe_event else { break };
                let key = match event {
                    Event::Key(key) => key,
                    Event::Mouse(mouse) => {
                        match mouse.kind {
                            MouseEventKind::ScrollUp => app.scroll_by(3),
                            MouseEventKind::ScrollDown => app.scroll_by(-3),
                            _ => {}
                        }
                        continue;
                    }
                    _ => continue,
                };
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
                match (key.code, ctrl) {
                    (KeyCode::Esc, _) | (KeyCode::Char('c'), true) => break,

                    (KeyCode::Char('y'), true) => resolve_first_pending(&mut app, &assistant, true),
                    (KeyCode::Char('n'), true) => resolve_first_pending(&mut app, &assistant, false),
                    (KeyCode::Char('g'), true) => rate(&mut app, &assistant, true),
                    (KeyCode::Char('b'), true) => rate(&mut app, &assistant, false),

                    (KeyCode::Char('a'), true) => resolve_first_change(&mut app, &assistant, true),
                    (KeyCode::Char('x'), true) => resolve_first_change(&mut app, &assistant, false),
                    (KeyCode::Char('o'), true) => open_diffs_in_vscode(&mut app),
                    (KeyCode::BackTab, _) => {
                        let mode = assistant.mode().next();
                        assistant.set_mode(mode);
                        app.push(Speaker::Status, format!(
                            "mode → {} ({})",
                            mode.label(),
                            match mode {
                                Mode::Plan => "research only — the assistant plans, no edits",
                                Mode::Edit => "changes wait for Ctrl-A apply / Ctrl-X reject",
                                Mode::AutoAccept => "changes are applied to files immediately",
                            }
                        ));
                    }

                    (KeyCode::PageUp, _) => app.scroll_by(5),
                    (KeyCode::PageDown, _) => app.scroll_by(-5),
                    (KeyCode::Up, _) => app.scroll_by(1),
                    (KeyCode::Down, _) => app.scroll_by(-1),
                    (KeyCode::Home, _) => app.scroll_up = app.chat_top,
                    (KeyCode::End, _) => app.scroll_up = 0,

                    (KeyCode::Enter, _) => {
                        let question = app.input.trim().to_string();
                        if !question.is_empty() && !app.busy {
                            app.input.clear();
                            app.push(Speaker::You, question.clone());
                            app.scroll_up = 0; // sending snaps back to the newest message
                            app.busy = true;
                            let assistant = assistant.clone();
                            let tx = tx.clone();
                            tokio::spawn(async move {
                                // Errors also arrive as AiEvent::Error.
                                let _ = assistant.ask(&question, tx).await;
                            });
                        }
                    }
                    (KeyCode::Backspace, _) => { app.input.pop(); }
                    (KeyCode::Char(c), false) => app.input.push(c),
                    _ => {}
                }
            }
            maybe_ai = rx.recv() => {
                match maybe_ai {
                    Some(AiEvent::Status(s)) => app.push(Speaker::Status, s),
                    Some(AiEvent::Suggestion(s)) => {
                        let head = match s.state {
                            ChangeState::Applied => format!("✏ change applied to {}", s.file),
                            _ => format!(
                                "✏ change proposed for {} — Ctrl-A apply · Ctrl-X reject",
                                s.file
                            ),
                        };
                        let reason = if s.reason.is_empty() {
                            String::new()
                        } else {
                            format!("\n  {}", s.reason)
                        };
                        app.push(Speaker::Status, format!("{head}{reason}"));
                        app.push(Speaker::Diff, s.diff.clone());
                        app.push(Speaker::Status, "Ctrl-O opens the diff in VS Code".to_string());
                        app.suggestions.push(s);
                    }
                    Some(AiEvent::Answer(a)) => {
                        app.busy = false;
                        app.push(Speaker::Assistant, a);
                        let pending = assistant.brain.pending().len();
                        if pending > 0 {
                            app.push(Speaker::Status, format!(
                                "{pending} tag proposal(s) waiting — Ctrl-Y accept / Ctrl-N reject \
                                 (or use the browser view)"
                            ));
                        }
                        app.push(Speaker::Status,
                            "rate the answer: Ctrl-G good · Ctrl-B bad".to_string());
                    }
                    Some(AiEvent::Error(e)) => {
                        app.busy = false;
                        app.push(Speaker::Error, e);
                    }
                    None => break,
                }
            }
            _ = sleep => {}
        }
    }
    Ok(())
}

/// Accept or reject the oldest pending tag proposal.
fn resolve_first_pending(app: &mut App, assistant: &Assistant, accept: bool) {
    let pending = assistant.brain.pending();
    let Some(first) = pending.first() else {
        app.push(Speaker::Status, "no pending tag proposals".to_string());
        return;
    };
    match assistant.brain.confirm(&first.file, accept) {
        Ok(_) => app.push(
            Speaker::Status,
            format!(
                "{} tags [{}] for {}{}",
                if accept { "accepted" } else { "rejected" },
                first.tags.join(", "),
                first.file,
                match pending.len() - 1 {
                    0 => String::new(),
                    n => format!(" — {n} more waiting"),
                }
            ),
        ),
        Err(e) => app.push(Speaker::Error, e.to_string()),
    }
}

/// Apply or reject the oldest pending change proposal. Applying writes the
/// proposed content into the working tree.
fn resolve_first_change(app: &mut App, assistant: &Assistant, accept: bool) {
    let pending = assistant.toolbox.pending_changes();
    let Some(first) = pending.first() else {
        app.push(Speaker::Status, "no pending change proposals".to_string());
        return;
    };
    match assistant.toolbox.resolve_change(&first.file, accept) {
        Ok(c) => app.push(
            Speaker::Status,
            if accept {
                format!("✔ applied change to {}", c.file)
            } else {
                format!(
                    "✘ rejected change to {} — tell the assistant what to do differently",
                    c.file
                )
            },
        ),
        Err(e) => app.push(Speaker::Error, e.to_string()),
    }
}

/// Open every suggested change (latest per file) as a VS Code diff tab.
fn open_diffs_in_vscode(app: &mut App) {
    if app.suggestions.is_empty() {
        app.push(Speaker::Status, "no suggested changes to open yet".to_string());
        return;
    }
    let mut latest: std::collections::BTreeMap<String, &ChangeSuggestion> =
        std::collections::BTreeMap::new();
    for s in &app.suggestions {
        latest.insert(s.file.clone(), s);
    }
    let mut opened = 0;
    let mut failed = None;
    for s in latest.values() {
        match std::process::Command::new("code")
            .arg("--reuse-window")
            .arg("--diff")
            .arg(&s.original)
            .arg(&s.proposed)
            .spawn()
        {
            Ok(_) => opened += 1,
            Err(e) => failed = Some(e),
        }
    }
    if let Some(e) = failed {
        app.push(
            Speaker::Error,
            format!("couldn't launch VS Code ('code' CLI): {e}"),
        );
    } else {
        app.push(
            Speaker::Status,
            format!("opened {opened} diff{} in VS Code", if opened == 1 { "" } else { "s" }),
        );
    }
}

/// Record feedback on the last answer and report the retrained confidence.
fn rate(app: &mut App, assistant: &Assistant, positive: bool) {
    let files = assistant.files_touched();
    match assistant.brain.record_feedback(positive, files, "tui") {
        Ok(c) => app.push(
            Speaker::Status,
            format!(
                "feedback recorded ({}) — confidence is now {c:.0}/100",
                if positive { "good" } else { "bad" }
            ),
        ),
        Err(e) => app.push(Speaker::Error, e.to_string()),
    }
}

fn render(f: &mut Frame, app: &mut App, assistant: &Assistant) {
    let [header, chat, pending_area, input, hints] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(3),
        Constraint::Length(pending_height(assistant)),
        Constraint::Length(3),
        Constraint::Length(1),
    ])
    .areas(f.area());

    render_header(f, header, assistant);
    render_chat(f, chat, app);
    render_pending(f, pending_area, assistant);
    render_input(f, input, app);
    render_hints(f, hints, app);
}

fn render_header(f: &mut Frame, area: Rect, assistant: &Assistant) {
    let [title_area, gauge_area] =
        Layout::horizontal([Constraint::Min(20), Constraint::Length(34)]).areas(area);

    let mode = assistant.mode();
    let mode_color = match mode {
        Mode::Plan => Color::Blue,
        Mode::Edit => Color::Yellow,
        Mode::AutoAccept => Color::Red,
    };
    let title = Paragraph::new(Line::from(vec![
        Span::styled(" ciabatta ai ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
        Span::styled(
            format!("· {} ", assistant.provider.label()),
            Style::default().fg(Color::DarkGray),
        ),
        Span::styled(
            format!("[{}]", mode.label()),
            Style::default().fg(mode_color).add_modifier(Modifier::BOLD),
        ),
    ]));
    f.render_widget(title, title_area);

    let confidence = assistant.brain.confidence();
    let gauge = Gauge::default()
        .ratio((confidence / 100.0).clamp(0.0, 1.0))
        .label(format!("confidence {confidence:.0}/100"))
        .gauge_style(Style::default().fg(gauge_color(confidence)).bg(Color::Black));
    f.render_widget(gauge, gauge_area);
}

fn gauge_color(confidence: f64) -> Color {
    if confidence < 35.0 {
        Color::Red
    } else if confidence < 70.0 {
        Color::Yellow
    } else {
        Color::Green
    }
}

fn render_chat(f: &mut Frame, area: Rect, app: &mut App) {
    let mut lines: Vec<Line> = Vec::new();
    for entry in &app.entries {
        let (prefix, style) = match entry.speaker {
            Speaker::You => ("you ▸ ", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
            Speaker::Assistant => ("ai  ▸ ", Style::default().fg(Color::Yellow)),
            Speaker::Status | Speaker::Diff => ("      ", Style::default().fg(Color::DarkGray)),
            Speaker::Error => ("err ▸ ", Style::default().fg(Color::Red)),
        };
        for (i, raw) in entry.text.lines().enumerate() {
            let head = if i == 0 { prefix } else { "      " };
            let body_style = match entry.speaker {
                Speaker::Status => Style::default().fg(Color::DarkGray),
                Speaker::Error => Style::default().fg(Color::Red),
                // Diff lines get git-style coloring by their first character.
                Speaker::Diff => {
                    if raw.starts_with("+++") || raw.starts_with("---") {
                        Style::default().fg(Color::White).add_modifier(Modifier::BOLD)
                    } else if raw.starts_with('+') {
                        Style::default().fg(Color::Green)
                    } else if raw.starts_with('-') {
                        Style::default().fg(Color::Red)
                    } else if raw.starts_with("@@") {
                        Style::default().fg(Color::Cyan)
                    } else {
                        Style::default().fg(Color::DarkGray)
                    }
                }
                _ => Style::default(),
            };
            lines.push(Line::from(vec![
                Span::styled(head, style),
                Span::styled(raw.to_string(), body_style),
            ]));
        }
        if entry.speaker == Speaker::Assistant {
            lines.push(Line::default());
        }
    }
    if app.busy {
        lines.push(Line::from(Span::styled(
            "      … thinking",
            Style::default().fg(Color::DarkGray).add_modifier(Modifier::ITALIC),
        )));
    }

    // Wrap-aware autoscroll: estimate rendered height, then hold the view
    // `scroll_up` lines above the bottom.
    let width = area.width.saturating_sub(2).max(1) as usize;
    let total: usize = lines
        .iter()
        .map(|l| {
            let len: usize = l.spans.iter().map(|s| s.content.chars().count()).sum();
            len.div_ceil(width).max(1)
        })
        .sum();
    let visible = area.height.saturating_sub(2) as usize;
    let bottom = total.saturating_sub(visible).min(u16::MAX as usize) as u16;
    if app.scroll_up > 0 {
        // The user is reading scrollback: grow `scroll_up` by however many
        // lines just arrived below, so the view stays anchored on the same
        // messages instead of drifting (or snapping) toward the bottom.
        app.scroll_up = (app.scroll_up + bottom.saturating_sub(app.chat_top)).min(bottom);
    }
    app.chat_top = bottom;
    let scroll = bottom.saturating_sub(app.scroll_up);

    let chat = Paragraph::new(lines)
        .block(Block::default().borders(Borders::ALL).border_style(Style::default().fg(Color::DarkGray)))
        .wrap(Wrap { trim: false })
        .scroll((scroll, 0));
    f.render_widget(chat, area);
}

/// Pending banner height: one line per waiting change or tag proposal, capped.
fn pending_height(assistant: &Assistant) -> u16 {
    let n = assistant.toolbox.pending_changes().len() + assistant.brain.pending().len();
    (n as u16).min(4)
}

fn render_pending(f: &mut Frame, area: Rect, assistant: &Assistant) {
    if area.height == 0 {
        return;
    }
    let mut lines: Vec<Line> = assistant
        .toolbox
        .pending_changes()
        .iter()
        .map(|c| {
            Line::from(vec![
                Span::styled(" ✏ ", Style::default()),
                Span::styled(c.file.clone(), Style::default().fg(Color::Blue)),
                Span::styled(" change waiting ", Style::default().fg(Color::Yellow)),
                Span::styled(
                    "Ctrl-A apply · Ctrl-X reject · Ctrl-O view in VS Code",
                    Style::default().fg(Color::DarkGray),
                ),
            ])
        })
        .collect();
    lines.extend(assistant.brain.pending().iter().map(|p| {
        Line::from(vec![
            Span::styled(" 🏷 ", Style::default()),
            Span::styled(p.file.clone(), Style::default().fg(Color::Blue)),
            Span::styled(
                format!(" → [{}] ", p.tags.join(", ")),
                Style::default().fg(Color::Yellow),
            ),
            Span::styled("Ctrl-Y accept · Ctrl-N reject", Style::default().fg(Color::DarkGray)),
        ])
    }));
    lines.truncate(area.height as usize);
    f.render_widget(Paragraph::new(lines), area);
}

fn render_input(f: &mut Frame, area: Rect, app: &App) {
    let input = Paragraph::new(app.input.as_str())
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(if app.busy { " waiting for the model… " } else { " ask " })
                .border_style(Style::default().fg(if app.busy {
                    Color::DarkGray
                } else {
                    Color::Yellow
                })),
        )
        .wrap(Wrap { trim: false });
    f.render_widget(input, area);
    if !app.busy {
        // Put the cursor at the end of the typed text (single-line input).
        let x = area.x + 1 + (app.input.chars().count() as u16).min(area.width.saturating_sub(3));
        f.set_cursor_position((x, area.y + 1));
    }
}

fn render_hints(f: &mut Frame, area: Rect, app: &App) {
    let mut hint = String::from(
        " Enter send · S-Tab mode · ^A/^X change · ^O vscode · ^Y/^N tags · ^G/^B rate · wheel/PgUp scroll · Esc quit",
    );
    if let Some(url) = &app.graph_url {
        hint.push_str(&format!(" · map: {url}"));
    }
    f.render_widget(
        Paragraph::new(Span::styled(hint, Style::default().fg(Color::DarkGray))),
        area,
    );
}
