//! The Ratatui chat front end for the AI assistant.
//!
//! One screen: a header with the live confidence gauge, the conversation, a
//! banner for pending tag confirmations, and an input line.
//!
//! The input box accepts pasted text (including multi-line code) intact via
//! bracketed paste, and grows to fit it. Typing `/` opens a command menu
//! (↑↓ to select, Tab to complete, Enter to run).
//!
//! Keys:
//!   Enter        send the typed question (or run a /command)
//!   Alt/Shift-Enter   insert a newline (compose a multi-line message)
//!   ←/→ · ^←/^→       move the cursor by a character / by a word
//!   Home/End          start/end of the line the cursor is on
//!   ↑/↓               move between the input's lines while composing one
//!   ^W · ^U · ^K      delete the word before the cursor / to the line's
//!                     start / to its end
//!   Shift-Tab         cycle the mode: plan → edit → auto-accept
//!   Ctrl-A / Ctrl-X   apply / reject the first pending change proposal
//!   Ctrl-Z            undo the most recently applied change
//!   Ctrl-O            open the suggested changes as diffs in VS Code
//!   Ctrl-T            release/recapture the mouse for text selection & copy
//!
//! Typed `/commands`: /help, /new, /clear, /plan, /edit, /auto, /mode, /map.
//!   Ctrl-Y / Ctrl-N   accept / reject the first pending tag proposal
//!   Ctrl-G / Ctrl-B   rate the last answer good / bad (trains confidence)
//!   PageUp/PageDown · Up/Down · mouse wheel   scroll the conversation
//!   Home / End        jump to the oldest / newest message
//!   Esc / Ctrl-C      quit

use std::io;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use crossterm::{
    event::{
        DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
        Event, EventStream, KeyCode, KeyEventKind, KeyModifiers, MouseEventKind,
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
use super::{AiEvent, Assistant, BurnProgress, Mode};

/// Who "said" a chat entry, which controls its styling.
#[derive(Clone, Copy, PartialEq)]
enum Speaker {
    You,
    Assistant,
    Status,
    Error,
    /// A unified diff of a proposed change; rendered with +/- coloring.
    Diff,
    /// The startup bread mascot, drawn in warm crust colors with no prefix.
    Banner,
}

/// Mascot palette: golden crust, pale crumb, and dark-baked face features.
/// (Truecolor; terminals without it degrade to the nearest ANSI color.)
const CRUST: Color = Color::Rgb(0xE7, 0xB6, 0x5A);
const CRUMB: Color = Color::Rgb(0xF6, 0xE3, 0xB8);
const FACE: Color = Color::Rgb(0x7A, 0x4A, 0x1E);

/// Split one mascot line into colored spans by classifying each character:
/// letters are crumb, the eyes/smile are the dark face, everything else crust.
fn banner_spans(line: &str) -> Vec<Span<'static>> {
    let class = |c: char| -> Color {
        if c.is_ascii_alphabetic() {
            CRUMB
        } else if matches!(c, '•' | '‿' | '^' | 'ᵕ' | 'o') {
            FACE
        } else {
            CRUST
        }
    };
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut run = String::new();
    let mut run_color: Option<Color> = None;
    for ch in line.chars() {
        let color = class(ch);
        if run_color != Some(color) && !run.is_empty() {
            spans.push(Span::styled(
                std::mem::take(&mut run),
                Style::default().fg(run_color.unwrap()),
            ));
        }
        run_color = Some(color);
        run.push(ch);
    }
    if let Some(color) = run_color {
        spans.push(Span::styled(run, Style::default().fg(color)));
    }
    spans
}

/// Color for inline `code` spans in rendered assistant Markdown.
const CODE: Color = Color::Rgb(0x9C, 0xDC, 0xFE);

/// Frames for the "thinking" spinner (a spinning braille dot).
const SPINNER: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

/// How long the typewriter "wipe" that reveals a fresh answer runs, in seconds.
const REVEAL_SECS: f32 = 0.6;

struct ChatEntry {
    speaker: Speaker,
    text: String,
}

/// The message being composed, and the cursor inside it.
///
/// The cursor is a *character* index rather than a byte offset: every edit and
/// every movement below is expressed in characters, so one conversion at the
/// point of mutation is cheaper to follow than char-boundary arithmetic spread
/// across a dozen key handlers. The input is one message long, so walking it to
/// convert costs nothing worth saving.
#[derive(Default)]
struct Input {
    text: String,
    /// Characters before the cursor. Always ≤ the input's length in chars.
    cursor: usize,
}

impl Input {
    fn text(&self) -> &str {
        &self.text
    }

    fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// Length in characters — the unit the cursor counts in.
    fn len(&self) -> usize {
        self.text.chars().count()
    }

    /// Byte offset of character index `at`, or the end of the string.
    fn byte_of(&self, at: usize) -> usize {
        self.text
            .char_indices()
            .nth(at)
            .map(|(b, _)| b)
            .unwrap_or(self.text.len())
    }

    fn chars(&self) -> Vec<char> {
        self.text.chars().collect()
    }

    fn clear(&mut self) {
        self.text.clear();
        self.cursor = 0;
    }

    /// Replace the whole input and park the cursor at its end — what command
    /// completion wants, so the arguments are typed straight after the name.
    fn set(&mut self, text: impl Into<String>) {
        self.text = text.into();
        self.cursor = self.len();
    }

    fn insert_str(&mut self, s: &str) {
        let at = self.byte_of(self.cursor);
        self.text.insert_str(at, s);
        self.cursor += s.chars().count();
    }

    fn insert_char(&mut self, c: char) {
        let at = self.byte_of(self.cursor);
        self.text.insert(at, c);
        self.cursor += 1;
    }

    /// Delete the half-open character range, leaving the cursor at its start.
    fn delete_range(&mut self, from: usize, to: usize) {
        let (from, to) = (from.min(to), from.max(to).min(self.len()));
        if from == to {
            return;
        }
        let (a, b) = (self.byte_of(from), self.byte_of(to));
        self.text.replace_range(a..b, "");
        self.cursor = from;
    }

    /// Delete the character before the cursor.
    fn backspace(&mut self) {
        if self.cursor > 0 {
            self.delete_range(self.cursor - 1, self.cursor);
        }
    }

    /// Delete the character under the cursor.
    fn delete(&mut self) {
        self.delete_range(self.cursor, self.cursor + 1);
    }

    fn left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    fn right(&mut self) {
        self.cursor = (self.cursor + 1).min(self.len());
    }

    /// Start of the word at or before the cursor: skip any whitespace, then the
    /// word itself. Word-wise movement and `^W` share it so they always agree
    /// about where a word begins.
    fn word_start(&self) -> usize {
        let chars = self.chars();
        let mut at = self.cursor;
        while at > 0 && chars[at - 1].is_whitespace() {
            at -= 1;
        }
        while at > 0 && !chars[at - 1].is_whitespace() {
            at -= 1;
        }
        at
    }

    /// End of the word at or after the cursor: skip any whitespace, then the
    /// word itself.
    fn word_end(&self) -> usize {
        let chars = self.chars();
        let len = chars.len();
        let mut at = self.cursor;
        while at < len && chars[at].is_whitespace() {
            at += 1;
        }
        while at < len && !chars[at].is_whitespace() {
            at += 1;
        }
        at
    }

    /// Start of the logical line (between newlines) the cursor sits on.
    fn line_start(&self) -> usize {
        let chars = self.chars();
        let mut at = self.cursor;
        while at > 0 && chars[at - 1] != '\n' {
            at -= 1;
        }
        at
    }

    /// End of the logical line the cursor sits on.
    fn line_end(&self) -> usize {
        let chars = self.chars();
        let mut at = self.cursor;
        while at < chars.len() && chars[at] != '\n' {
            at += 1;
        }
        at
    }

    /// Move the cursor one *wrapped* row up (`-1`) or down (`+1`), keeping the
    /// column where the new row is long enough to hold it.
    ///
    /// Rows rather than logical lines, because rows are what is on screen: a
    /// pasted paragraph is one logical line and a dozen rows, and pressing ↑
    /// inside it should land on the row above, not skip the whole paragraph.
    fn move_row(&mut self, delta: i32, width: usize) {
        let rows = wrap_input(&self.text, width);
        let (row, col) = cursor_rc(&rows, self.cursor, width);
        let target = match row.checked_add_signed(delta as isize) {
            Some(t) if t < rows.len() => t,
            // Off the top or the bottom: fall back to the ends, which is what
            // every other editor does with ↑ on the first row.
            _ if delta < 0 => {
                self.cursor = 0;
                return;
            }
            _ => {
                self.cursor = self.len();
                return;
            }
        };
        let row = &rows[target];
        self.cursor = (row.start + col).min(drawn_end(row, width));
    }
}

/// Live state for the burn-in progress panel: the latest structured progress
/// plus when the run started, for an elapsed clock.
struct BurnView {
    progress: BurnProgress,
    started: Instant,
}

/// One entry in the `/` command menu.
struct SlashCommand {
    name: &'static str,
    args: &'static str,
    help: &'static str,
}

/// Commands offered in the `/` menu, in display order.
const SLASH_COMMANDS: &[SlashCommand] = &[
    SlashCommand {
        name: "report",
        args: "[days]",
        help: "summarize repo changes (^S saves a PDF)",
    },
    SlashCommand {
        name: "tag",
        args: "<name> [desc]",
        help: "add a mind-map tag; AI connects files",
    },
    SlashCommand {
        name: "burn",
        args: "[review] [n]",
        help: "learn the whole codebase into the mind map",
    },
    SlashCommand {
        name: "analyze",
        args: "",
        help: "add dependency analysis to the mind map",
    },
    SlashCommand {
        name: "new",
        args: "",
        help: "start a fresh conversation",
    },
    SlashCommand {
        name: "list",
        args: "",
        help: "list saved conversations",
    },
    SlashCommand {
        name: "delete",
        args: "[id]",
        help: "delete a conversation (current if no id)",
    },
    SlashCommand {
        name: "clear",
        args: "",
        help: "clear the screen",
    },
    SlashCommand {
        name: "plan",
        args: "",
        help: "switch to plan mode",
    },
    SlashCommand {
        name: "edit",
        args: "",
        help: "switch to edit mode",
    },
    SlashCommand {
        name: "auto",
        args: "",
        help: "switch to auto-accept mode",
    },
    SlashCommand {
        name: "mode",
        args: "",
        help: "show the current mode",
    },
    SlashCommand {
        name: "map",
        args: "",
        help: "show the mind-map URL",
    },
    SlashCommand {
        name: "help",
        args: "",
        help: "list commands",
    },
];

struct App {
    entries: Vec<ChatEntry>,
    input: Input,
    busy: bool,
    /// When the current `busy` request began, for the elapsed "thinking" clock.
    busy_since: Option<Instant>,
    /// Lines scrolled up from the bottom of the conversation.
    scroll_up: u16,
    /// Topmost reachable `scroll_up`, measured on the last frame; scrolling is
    /// clamped to it so the view can't run past the oldest message.
    chat_top: u16,
    /// Columns the conversation last wrapped at, so a resize can be told apart
    /// from messages arriving.
    chat_width: usize,
    /// Change proposals seen this session (newest last), for Ctrl-O.
    suggestions: Vec<ChangeSuggestion>,
    graph_url: Option<String>,
    /// Highlighted row in the `/` command menu.
    slash_index: usize,
    /// The most recent assistant answer, for saving as a PDF with Ctrl-S.
    last_answer: Option<String>,
    /// True after a `/report` is launched, until its answer arrives — used to
    /// show the "save as PDF" hint only for reports.
    report_pending: bool,
    /// Look-back window of the last report, stamped into the PDF title.
    last_report_days: u64,
    /// When the newest assistant answer arrived, driving its typewriter reveal.
    reveal_since: Option<Instant>,
    /// Whether the terminal's mouse is captured (wheel scrolls the chat). Toggle
    /// it off with Ctrl-T to hand the mouse back for native text selection/copy.
    mouse_capture: bool,
    /// Live burn-in progress, driving the dedicated progress panel. `None` when
    /// no burn is running.
    burn: Option<BurnView>,
    /// Columns the input box last drew into, stamped by `render_input`.
    ///
    /// Vertical cursor movement is defined in terms of wrapped rows, and rows
    /// only exist relative to a width — so the key handler has to be told the
    /// one the user is actually looking at, and be told again when the terminal
    /// is resized under it.
    input_width: usize,
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

    /// Insert pasted text at the cursor. Newlines are kept so multi-line code
    /// lands intact; CRLF/CR are normalized to LF so it renders as clean lines.
    /// Ignored while a request is in flight.
    fn paste(&mut self, data: &str) {
        if self.busy {
            return;
        }
        self.input
            .insert_str(&data.replace("\r\n", "\n").replace('\r', "\n"));
        self.slash_index = 0;
    }

    /// Whether the `/` command menu should be shown: the input is a bare command
    /// name being typed (a leading `/`, no whitespace yet) and we're idle.
    fn slash_active(&self) -> bool {
        !self.busy
            && self.input.text().starts_with('/')
            && !self.input.text().contains(char::is_whitespace)
    }

    /// Commands matching the typed prefix, in menu order.
    fn slash_matches(&self) -> Vec<&'static SlashCommand> {
        let token = self.input.text().trim_start_matches('/').to_lowercase();
        SLASH_COMMANDS
            .iter()
            .filter(|c| c.name.starts_with(&token))
            .collect()
    }

    /// The currently highlighted command, if the menu is active and non-empty.
    fn slash_selected(&self) -> Option<&'static SlashCommand> {
        let matches = self.slash_matches();
        matches
            .get(self.slash_index.min(matches.len().saturating_sub(1)))
            .copied()
    }

    /// The command being typed once arguments have started (a leading `/` plus
    /// whitespace). The selectable menu is gone by now, but we keep the matched
    /// command's format on screen so the user can see the arguments it expects.
    fn slash_usage(&self) -> Option<&'static SlashCommand> {
        if self.busy
            || !self.input.text().starts_with('/')
            || !self.input.text().contains(char::is_whitespace)
        {
            return None;
        }
        let name = self
            .input
            .text()
            .trim_start_matches('/')
            .split_whitespace()
            .next()
            .unwrap_or("")
            .to_lowercase();
        SLASH_COMMANDS.iter().find(|c| c.name == name)
    }

    /// Move the menu highlight, wrapping around.
    fn slash_move(&mut self, delta: i32) {
        let n = self.slash_matches().len() as i32;
        if n == 0 {
            return;
        }
        let cur = (self.slash_index.min(n as usize - 1)) as i32;
        self.slash_index = (cur + delta).rem_euclid(n) as usize;
    }

    /// Scroll up (`+`) or down (`-`), clamped to the conversation.
    fn scroll_by(&mut self, delta: i32) {
        self.scroll_up =
            (i32::from(self.scroll_up) + delta).clamp(0, i32::from(self.chat_top)) as u16;
    }

    /// Whether the composed message occupies more than one row at the width the
    /// box was last drawn at — the test for whether ↑/↓ belong to the text.
    fn input_is_multirow(&self) -> bool {
        wrap_input(self.input.text(), self.input_width).len() > 1
    }

    /// True while the newest answer is still wiping in — used to keep redraws
    /// running smoothly for the duration of the animation, then relax.
    fn reveal_active(&self) -> bool {
        self.reveal_since
            .map(|t| t.elapsed().as_secs_f32() < REVEAL_SECS)
            .unwrap_or(false)
    }
}

/// Run the chat TUI until the user quits.
pub async fn run(assistant: Arc<Assistant>, graph_url: Option<String>) -> Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    // Bracketed paste makes the terminal deliver a paste as one `Event::Paste`
    // string instead of a burst of keystrokes — without it, newlines in pasted
    // code arrive as Enter presses and submit the paste line by line.
    execute!(
        stdout,
        EnterAlternateScreen,
        EnableMouseCapture,
        EnableBracketedPaste
    )?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = chat_loop(&mut terminal, assistant, graph_url).await;

    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        DisableBracketedPaste,
        DisableMouseCapture,
        LeaveAlternateScreen
    )?;
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
        input: Input::default(),
        busy: false,
        busy_since: None,
        scroll_up: 0,
        chat_top: 0,
        chat_width: 0,
        suggestions: Vec::new(),
        graph_url,
        slash_index: 0,
        last_answer: None,
        report_pending: false,
        last_report_days: super::DEFAULT_REPORT_DAYS,
        reveal_since: None,
        mouse_capture: true,
        burn: None,
        input_width: 80,
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
            format!(
                "resumed conversation {} — {} earlier turns below",
                assistant.conversation_id().await,
                transcript.len()
            ),
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

        // Redraw briskly while something is animating (the thinking spinner or a
        // fresh answer wiping in), and idle back to a slow tick otherwise.
        let tick = if app.busy || app.reveal_active() {
            66
        } else {
            250
        };
        let sleep = tokio::time::sleep(Duration::from_millis(tick));
        tokio::select! {
            maybe_event = event_stream.next() => {
                let Some(Ok(event)) = maybe_event else { break };
                let key = match event {
                    Event::Key(key) => key,
                    Event::Paste(data) => {
                        app.paste(&data);
                        continue;
                    }
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
                    (KeyCode::Char('z'), true) => revert_last_change(&mut app, &assistant),
                    (KeyCode::Char('s'), true) => save_report_pdf(&mut app, &assistant),
                    (KeyCode::Char('o'), true) => open_diffs_in_vscode(&mut app),
                    // Release/recapture the mouse so text can be selected and
                    // copied with the terminal's own selection (mouse capture
                    // otherwise swallows drags to drive wheel scrolling).
                    (KeyCode::Char('t'), true) => {
                        app.mouse_capture = !app.mouse_capture;
                        let _ = if app.mouse_capture {
                            execute!(terminal.backend_mut(), EnableMouseCapture)
                        } else {
                            execute!(terminal.backend_mut(), DisableMouseCapture)
                        };
                        app.push(
                            Speaker::Status,
                            if app.mouse_capture {
                                "mouse captured — wheel scrolls the chat; Ctrl-T to release for \
                                 text selection".to_string()
                            } else {
                                "mouse released — drag to select & copy; Ctrl-T to re-enable wheel \
                                 scroll (Shift-drag also selects while captured)".to_string()
                            },
                        );
                    }
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

                    // ── editing the typed message ───────────────────────────
                    // Ctrl/Alt move by a word; bare arrows by a character.
                    (KeyCode::Left, true) => app.input.cursor = app.input.word_start(),
                    (KeyCode::Right, true) => app.input.cursor = app.input.word_end(),
                    (KeyCode::Left, _) if key.modifiers.contains(KeyModifiers::ALT) => {
                        app.input.cursor = app.input.word_start()
                    }
                    (KeyCode::Right, _) if key.modifiers.contains(KeyModifiers::ALT) => {
                        app.input.cursor = app.input.word_end()
                    }
                    (KeyCode::Left, _) => app.input.left(),
                    (KeyCode::Right, _) => app.input.right(),
                    (KeyCode::Delete, _) => app.input.delete(),
                    // Kill to the word before the cursor / to either end of the
                    // line, the readline keys every shell already trains.
                    (KeyCode::Char('w'), true) => {
                        let to = app.input.cursor;
                        app.input.delete_range(app.input.word_start(), to);
                    }
                    (KeyCode::Char('u'), true) => {
                        let to = app.input.cursor;
                        app.input.delete_range(app.input.line_start(), to);
                    }
                    (KeyCode::Char('k'), true) => {
                        let from = app.input.cursor;
                        app.input.delete_range(from, app.input.line_end());
                    }

                    // Up/Down drive the slash menu when it's open, step between
                    // the input's rows while a multi-row message is being
                    // composed, and scroll the conversation otherwise. The
                    // wheel and PageUp/PageDown always scroll, so nothing is
                    // out of reach mid-compose.
                    (KeyCode::Up, _) if app.slash_active() => app.slash_move(-1),
                    (KeyCode::Down, _) if app.slash_active() => app.slash_move(1),
                    (KeyCode::Up, _) if app.input_is_multirow() => {
                        app.input.move_row(-1, app.input_width)
                    }
                    (KeyCode::Down, _) if app.input_is_multirow() => {
                        app.input.move_row(1, app.input_width)
                    }
                    (KeyCode::Up, _) => app.scroll_by(1),
                    (KeyCode::Down, _) => app.scroll_by(-1),
                    // Home/End belong to the text while there is text: with the
                    // input empty there is no line to jump around in, so they
                    // stay the conversation's oldest/newest.
                    (KeyCode::Home, _) if app.input.is_empty() => app.scroll_up = app.chat_top,
                    (KeyCode::End, _) if app.input.is_empty() => app.scroll_up = 0,
                    (KeyCode::Home, _) => app.input.cursor = app.input.line_start(),
                    (KeyCode::End, _) => app.input.cursor = app.input.line_end(),

                    // Tab completes the highlighted command (with a trailing
                    // space) so arguments can be typed before sending.
                    (KeyCode::Tab, _) if app.slash_active() => {
                        if let Some(c) = app.slash_selected() {
                            app.input.set(format!("/{} ", c.name));
                        }
                    }

                    // Alt/Shift+Enter inserts a newline instead of sending, so a
                    // multi-line message can be composed by hand (paste keeps its
                    // own newlines regardless).
                    (KeyCode::Enter, _)
                        if key.modifiers.intersects(KeyModifiers::ALT | KeyModifiers::SHIFT) =>
                    {
                        app.input.insert_char('\n');
                    }
                    (KeyCode::Enter, _) => {
                        // With the menu open, Enter runs the highlighted command;
                        // otherwise it sends whatever was typed.
                        let line = match app.slash_selected() {
                            Some(c) if app.slash_active() => format!("/{}", c.name),
                            _ => app.input.text().trim().to_string(),
                        };
                        if line.is_empty() || app.busy {
                            // nothing to send / still working
                        } else if let Some(cmd) = line.strip_prefix('/') {
                            app.input.clear();
                            // /report and /tag drive the agent, so they need the
                            // same streaming path as a question.
                            match cmd.split_whitespace().next() {
                                Some("report") => {
                                    let arg = cmd.trim_start_matches("report").trim();
                                    let days = arg.split_whitespace().next().and_then(|d| d.parse::<u64>().ok());
                                    let days = days.unwrap_or(super::DEFAULT_REPORT_DAYS).clamp(1, 3650);
                                    match super::report_prompt(&assistant.toolbox.root, days) {
                                        Ok(prompt) => {
                                            app.push(Speaker::You, format!("/report {days}"));
                                            app.scroll_up = 0;
                                            app.busy = true;
                                            app.busy_since = Some(Instant::now());
                                            app.report_pending = true;
                                            app.last_report_days = days;
                                            let assistant = assistant.clone();
                                            let tx = tx.clone();
                                            tokio::spawn(async move {
                                                let _ = assistant.ask(&prompt, tx).await;
                                            });
                                        }
                                        Err(e) => app.push(Speaker::Error, e.to_string()),
                                    }
                                }
                                Some("tag") => {
                                    let rest = cmd.trim_start_matches("tag").trim();
                                    let mut it = rest.splitn(2, char::is_whitespace);
                                    let name = it.next().unwrap_or("").trim();
                                    let desc = it.next().unwrap_or("").trim();
                                    if name.is_empty() {
                                        app.push(Speaker::Status, "usage: /tag <name> [description]".to_string());
                                    } else if let Err(e) = assistant.brain.set_architecture(name, desc) {
                                        app.push(Speaker::Error, e.to_string());
                                    } else {
                                        app.push(Speaker::Status, format!("added architecture '{}' — finding files…", name.to_lowercase()));
                                        let prompt = super::tag_pass_prompt(name, desc);
                                        app.push(Speaker::You, format!("/tag {name}"));
                                        app.scroll_up = 0;
                                        app.busy = true;
                                        app.busy_since = Some(Instant::now());
                                        let assistant = assistant.clone();
                                        let tx = tx.clone();
                                        tokio::spawn(async move {
                                            let _ = assistant.ask(&prompt, tx).await;
                                        });
                                    }
                                }
                                Some("burn") | Some("burnin") | Some("burn-in") => {
                                    // Learn the whole codebase in one pass. Flags:
                                    // `review` queues tags for confirmation; a bare
                                    // number caps how many files are scanned.
                                    let rest = cmd.split_once(char::is_whitespace).map(|x| x.1).unwrap_or("");
                                    let review = rest
                                        .split_whitespace()
                                        .any(|t| t == "review" || t == "--review");
                                    let limit = rest.split_whitespace().find_map(|t| t.parse::<usize>().ok());
                                    app.push(Speaker::You, format!("/burn{}", if review { " review" } else { "" }));
                                    app.push(Speaker::Status, format!(
                                        "🔥 burn-in starting — surveying then tagging every source \
                                         file{}. This runs many model calls; progress streams below.",
                                        if review { " (review: tags queue for Ctrl-Y / Ctrl-N)" } else { "" }
                                    ));
                                    app.scroll_up = 0;
                                    app.busy = true;
                                    app.busy_since = Some(Instant::now());
                                    let assistant = assistant.clone();
                                    let tx = tx.clone();
                                    let root = assistant.toolbox.root.clone();
                                    tokio::spawn(async move {
                                        match super::burnin::burn_core(&assistant, &root, review, limit, &tx).await {
                                            Ok(summary) => { let _ = tx.send(AiEvent::Answer(summary)).await; }
                                            Err(e) => { let _ = tx.send(AiEvent::Error(format!("{e:#}"))).await; }
                                        }
                                    });
                                }
                                Some("analyze") | Some("analyse") => {
                                    // Fold the static dependency analysis into the
                                    // mind map on demand (also part of burn-in).
                                    app.push(Speaker::You, "/analyze".to_string());
                                    app.push(Speaker::Status, "🔬 running static analysis to add \
                                        dependencies to the mind map…".to_string());
                                    app.scroll_up = 0;
                                    app.busy = true;
                                    app.busy_since = Some(Instant::now());
                                    let assistant = assistant.clone();
                                    let tx = tx.clone();
                                    let root = assistant.toolbox.root.clone();
                                    tokio::spawn(async move {
                                        match super::burnin::scan_dependencies(&assistant, &root, &tx).await {
                                            Ok(summary) => { let _ = tx.send(AiEvent::Answer(summary)).await; }
                                            Err(e) => { let _ = tx.send(AiEvent::Error(format!("{e:#}"))).await; }
                                        }
                                    });
                                }
                                _ => handle_slash(&mut app, &assistant, cmd).await,
                            }
                        } else {
                            app.input.clear();
                            app.push(Speaker::You, line.clone());
                            app.scroll_up = 0; // sending snaps back to the newest message
                            app.busy = true;
                            app.busy_since = Some(Instant::now());
                            let assistant = assistant.clone();
                            let tx = tx.clone();
                            tokio::spawn(async move {
                                // Errors also arrive as AiEvent::Error.
                                let _ = assistant.ask(&line, tx).await;
                            });
                        }
                    }
                    (KeyCode::Backspace, _) => {
                        app.input.backspace();
                        app.slash_index = 0;
                    }
                    (KeyCode::Char(c), false) => {
                        app.input.insert_char(c);
                        app.slash_index = 0;
                    }
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
                    Some(AiEvent::Plan(items)) => {
                        if !items.is_empty() {
                            let list = super::tools::render_plan(&items);
                            app.push(Speaker::Status, format!("plan\n{list}"));
                        }
                    }
                    Some(AiEvent::Progress(p)) => {
                        // Reuse the run's start time so the elapsed clock is
                        // continuous across updates; stamp it on the first one.
                        let started = app.burn.as_ref().map(|b| b.started).unwrap_or_else(Instant::now);
                        app.burn = Some(BurnView { progress: p, started });
                    }
                    Some(AiEvent::Answer(a)) => {
                        app.busy = false;
                        app.busy_since = None;
                        app.burn = None; // the batch job (if any) is done
                        app.last_answer = Some(a.clone());
                        app.reveal_since = Some(Instant::now());
                        app.push(Speaker::Assistant, a);
                        if app.report_pending {
                            app.report_pending = false;
                            app.push(Speaker::Status, "Ctrl-S saves this report as a PDF".to_string());
                        }
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
                        app.busy_since = None;
                        app.burn = None;
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

/// Handle a `/command` typed into the input. Unknown commands print the help.
async fn handle_slash(app: &mut App, assistant: &Assistant, cmd: &str) {
    let mut parts = cmd.split_whitespace();
    let name = parts.next().unwrap_or("").to_lowercase();
    match name.as_str() {
        "help" | "?" => app.push(
            Speaker::Status,
            "commands:\n\
             /new              start a fresh conversation (keeps the mind map)\n\
             /report [days]    summarize what changed in the repo (default 7 days)\n\
             /burn [review] [n]  learn the whole codebase into the mind map (review = confirm tags)\n\
             /analyze          add static dependency analysis to the mind map (deps tool)\n\
             /list             list saved conversations for this project\n\
             /delete [id]      delete a saved conversation (current if no id)\n\
             /clear            clear the screen (keeps the conversation)\n\
             /plan /edit /auto   switch mode\n\
             /mode             show the current mode\n\
             /map              show the live mind-map URL\n\
             /help             this list\n\
             (resume saved conversations at launch: `ciabatta ai resume`)"
                .to_string(),
        ),
        "new" => {
            let id = assistant.start_new_conversation().await;
            app.entries.clear();
            app.suggestions.clear();
            app.scroll_up = 0;
            app.push(Speaker::Status, format!("started a new conversation ({id})"));
        }
        "clear" => {
            app.entries.clear();
            app.scroll_up = 0;
            app.push(Speaker::Status, "screen cleared".to_string());
        }
        "list" => {
            let root = &assistant.toolbox.root;
            match super::session::list(root) {
                Ok(saved) if saved.is_empty() => {
                    app.push(Speaker::Status, "no saved conversations yet".to_string())
                }
                Ok(saved) => {
                    let current = assistant.conversation_id().await;
                    let mut out = String::from("saved conversations (newest first):");
                    for s in saved {
                        let here = if s.id == current { " ← current" } else { "" };
                        let when = s.updated_at.split('T').next().unwrap_or(&s.updated_at);
                        out.push_str(&format!("\n  {} [{when}] {} — {}{here}", s.id, s.turns, s.title));
                    }
                    app.push(Speaker::Status, out);
                }
                Err(e) => app.push(Speaker::Error, e.to_string()),
            }
        }
        "delete" | "del" | "rm" => {
            let root = assistant.toolbox.root.clone();
            // Delete the named conversation, or the current one (then start anew).
            let (id, was_current) = match parts.next() {
                Some(id) => (id.to_string(), id == assistant.conversation_id().await),
                None => (assistant.conversation_id().await, true),
            };
            match super::session::delete(&root, &id) {
                Ok(true) => {
                    app.push(Speaker::Status, format!("deleted conversation {id}"));
                    if was_current {
                        let new_id = assistant.start_new_conversation().await;
                        app.entries.clear();
                        app.suggestions.clear();
                        app.scroll_up = 0;
                        app.push(Speaker::Status, format!("started a new conversation ({new_id})"));
                    }
                }
                Ok(false) => app.push(Speaker::Status, format!("no saved conversation '{id}'")),
                Err(e) => app.push(Speaker::Error, e.to_string()),
            }
        }
        "plan" | "edit" | "auto" | "auto-accept" => {
            if let Ok(mode) = Mode::parse(&name) {
                assistant.set_mode(mode);
                app.push(Speaker::Status, format!("mode → {}", mode.label()));
            }
        }
        "mode" => app.push(Speaker::Status, format!("mode is {}", assistant.mode().label())),
        "map" => match &app.graph_url {
            Some(url) => app.push(Speaker::Status, format!("live mind map: {url}")),
            None => app.push(Speaker::Status, "the mind-map server isn't running".to_string()),
        },
        other => app.push(
            Speaker::Status,
            format!("unknown command '/{other}' — try /help"),
        ),
    }
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

/// Save the most recent assistant answer (typically a `/report`) to a PDF in
/// the project root.
fn save_report_pdf(app: &mut App, assistant: &Assistant) {
    let Some(answer) = app.last_answer.clone() else {
        app.push(
            Speaker::Status,
            "nothing to save yet — run /report first".to_string(),
        );
        return;
    };
    let name = format!(
        "ciabatta-report-{}.pdf",
        chrono::Local::now().format("%Y%m%d-%H%M%S")
    );
    let path = assistant.toolbox.root.join(&name);
    // Include the git activity as an appendix so the PDF stands on its own.
    let activity = crate::git::changes_since(&assistant.toolbox.root, app.last_report_days)
        .unwrap_or_default();
    match super::pdf::write_report(&path, app.last_report_days, &answer, &activity) {
        Ok(()) => app.push(
            Speaker::Status,
            format!("📄 saved report to {}", path.display()),
        ),
        Err(e) => app.push(Speaker::Error, e.to_string()),
    }
}

/// Undo the most recently applied change, restoring the file from its snapshot.
fn revert_last_change(app: &mut App, assistant: &Assistant) {
    match assistant.toolbox.revert_last_applied() {
        Ok(Some(c)) => app.push(
            Speaker::Status,
            format!("↩ reverted change to {} (restored from snapshot)", c.file),
        ),
        Ok(None) => app.push(
            Speaker::Status,
            "nothing to undo — no applied changes".to_string(),
        ),
        Err(e) => app.push(Speaker::Error, e.to_string()),
    }
}

/// Open every suggested change (latest per file) as a VS Code diff tab.
fn open_diffs_in_vscode(app: &mut App) {
    if app.suggestions.is_empty() {
        app.push(
            Speaker::Status,
            "no suggested changes to open yet".to_string(),
        );
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
            format!(
                "opened {opened} diff{} in VS Code",
                if opened == 1 { "" } else { "s" }
            ),
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

/// Columns the speaker prefix (`you ▸ `) occupies, and so the indent a wrapped
/// message is held at.
const PREFIX_WIDTH: usize = 6;

/// One wrapped row of the input: the half-open *character* range of the input
/// it draws.
type Row = std::ops::Range<usize>;

/// Wrap `text` the way the input box draws it: a new row at every newline, a
/// break after the last space that fits, and a hard break through any word too
/// long to fit a row at all.
///
/// Rows are contiguous and cover every character exactly once, so a character
/// index maps to a screen position by subtraction — which is the whole point.
/// The box renders these rows verbatim rather than handing ratatui a `Wrap`, so
/// what the cursor is computed from and what is on screen are the same layout,
/// and stay the same layout when the terminal is resized under them.
///
/// A row may run one character past `width` when that character is the space
/// the break was taken at; `drawn_end` is where the drawing stops.
fn wrap_input(text: &str, width: usize) -> Vec<Row> {
    let width = width.max(1);
    let chars: Vec<char> = text.chars().collect();
    let mut rows: Vec<Row> = Vec::new();
    let mut line_start = 0usize;

    loop {
        let line_end = chars[line_start..]
            .iter()
            .position(|&c| c == '\n')
            .map(|i| line_start + i)
            .unwrap_or(chars.len());

        let mut start = line_start;
        loop {
            if line_end - start <= width {
                rows.push(start..line_end);
                break;
            }
            // One past the window, so a word ending exactly at the edge breaks
            // after its own space rather than being split down the middle.
            let limit = (start + width + 1).min(line_end);
            let end = match chars[start..limit].iter().rposition(|&c| c == ' ') {
                Some(i) if start + i + 1 > start => start + i + 1,
                _ => start + width,
            };
            rows.push(start..end);
            start = end;
        }

        if line_end >= chars.len() {
            break;
        }
        line_start = line_end + 1;
    }

    rows
}

/// Where a row stops being drawn: its end, or the width, whichever comes first.
fn drawn_end(row: &Row, width: usize) -> usize {
    row.end.min(row.start + width)
}

/// The (row, column) a character index lands on in a wrapped layout.
///
/// A cursor sitting exactly at the right edge belongs at the start of the row
/// below — the position every terminal puts it in, and the one that stays
/// visible.
fn cursor_rc(rows: &[Row], cursor: usize, width: usize) -> (usize, usize) {
    let index = rows.iter().rposition(|r| r.start <= cursor).unwrap_or(0);
    let column = rows.get(index).map(|r| cursor - r.start).unwrap_or(0);
    if column >= width {
        (index + 1, 0)
    } else {
        (index, column)
    }
}

/// The text a row draws.
fn row_text(chars: &[char], row: &Row, width: usize) -> String {
    chars[row.start..drawn_end(row, width).min(chars.len())]
        .iter()
        .collect()
}

/// Split text into "word plus the spaces following it" chunks — the unit a
/// greedy wrap moves between rows.
fn split_words(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut chunk = String::new();
    let mut spacing = false;
    for c in text.chars() {
        if c == ' ' {
            spacing = true;
        } else if spacing {
            out.push(std::mem::take(&mut chunk));
            spacing = false;
        }
        chunk.push(c);
    }
    if !chunk.is_empty() {
        out.push(chunk);
    }
    out
}

/// Wrap one logical line of styled spans to `width`, indenting continuation
/// rows by `indent` so a wrapped message stays under the message it belongs to
/// rather than sliding back to the left margin.
///
/// The conversation wraps itself rather than handing ratatui a `Wrap` for two
/// reasons: that indent, which `Wrap` has no notion of, and the row count — the
/// scroll offset has to come from exactly the rows that get drawn, and a count
/// re-derived separately from the drawing is how a resize ends up clipping the
/// newest message off the bottom.
fn wrap_spans(spans: Vec<Span<'static>>, width: usize, indent: usize) -> Vec<Line<'static>> {
    let width = width.max(1);
    // However deep the indent would like to be, a continuation row on a narrow
    // terminal still needs room to say something.
    let indent = indent.min(width.saturating_sub(4));

    let mut rows: Vec<Vec<Span<'static>>> = Vec::new();
    let mut current: Vec<Span<'static>> = Vec::new();
    let mut used = 0usize;
    // Whether this row holds anything but its indent — the test for "is it
    // worth starting a new row for this word, or is the word simply too long?"
    let mut filled = false;
    // Set by a wrap, so the spaces a break was taken at don't reappear as a
    // ragged left edge on the row below. The *first* row keeps its leading
    // spaces: there, they're the message's own indentation.
    let mut wrapped = false;

    for span in spans {
        let style = span.style;
        for word in split_words(span.content.as_ref()) {
            let mut pending = word;
            loop {
                if wrapped {
                    pending = pending.trim_start().to_string();
                    wrapped = false;
                }
                if pending.is_empty() {
                    break;
                }
                let room = width.saturating_sub(used);
                // Trailing spaces don't have to fit: they land at the edge,
                // where they're invisible either way.
                let needed = pending.trim_end_matches(' ').chars().count();
                if needed <= room {
                    used = (used + pending.chars().count()).min(width);
                    current.push(Span::styled(pending, style));
                    filled = true;
                    break;
                }
                if filled {
                    // Try the word again with a whole row to itself.
                    rows.push(std::mem::take(&mut current));
                    current.push(Span::raw(" ".repeat(indent)));
                    used = indent;
                    filled = false;
                    wrapped = true;
                    continue;
                }
                // A single word longer than a whole row — a path, a URL, a
                // hash. Splitting it is ugly; letting it fall off the edge is
                // worse.
                let take = room.max(1);
                current.push(Span::styled(
                    pending.chars().take(take).collect::<String>(),
                    style,
                ));
                pending = pending.chars().skip(take).collect();
                rows.push(std::mem::take(&mut current));
                current.push(Span::raw(" ".repeat(indent)));
                used = indent;
            }
        }
    }

    rows.push(current);
    rows.into_iter().map(Line::from).collect()
}

/// Most content rows the input box grows to before it scrolls internally, so a
/// big paste can't crowd out the conversation.
const MAX_INPUT_ROWS: u16 = 12;

/// Rows the input box needs at this width — the wrapped message, and the row
/// the cursor sits on when it has run past the last of them.
fn input_rows(app: &App, width: usize) -> u16 {
    let rows = wrap_input(app.input.text(), width);
    let (cursor_row, _) = cursor_rc(&rows, app.input.cursor, width);
    rows.len()
        .max(cursor_row + 1)
        .clamp(1, MAX_INPUT_ROWS as usize) as u16
}

fn render(f: &mut Frame, app: &mut App, assistant: &Assistant) {
    // Grow the input box to fit multi-line/pasted content, up to a cap.
    let input_height = input_rows(app, f.area().width.saturating_sub(2).max(1) as usize) + 2; // borders
    let [header, burn, chat, pending_area, menu, input, hints] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(burn_height(app)),
        Constraint::Min(3),
        Constraint::Length(pending_height(assistant)),
        Constraint::Length(slash_menu_height(app)),
        Constraint::Length(input_height),
        Constraint::Length(1),
    ])
    .areas(f.area());

    render_header(f, header, assistant);
    render_burn(f, burn, app);
    render_chat(f, chat, app);
    render_pending(f, pending_area, assistant);
    render_slash_menu(f, menu, app);
    render_input(f, input, app);
    render_hints(f, hints, app);
}

/// Height of the `/` command area: one row per match (capped) plus a border
/// while selecting a name, a single format row once arguments are being typed,
/// or zero when neither applies.
fn slash_menu_height(app: &App) -> u16 {
    if app.slash_active() {
        match app.slash_matches().len() {
            0 => 0,
            n => (n as u16).min(6) + 2,
        }
    } else if app.slash_usage().is_some() {
        3 // one format line plus a border
    } else {
        0
    }
}

/// Render the `/` command menu with the highlighted row, or — once arguments
/// are being typed — the matched command's format so it stays on screen.
fn render_slash_menu(f: &mut Frame, area: Rect, app: &App) {
    if area.height == 0 {
        return;
    }
    if !app.slash_active() {
        // Argument mode: show just the command being entered and its format.
        if let Some(c) = app.slash_usage() {
            let label = if c.args.is_empty() {
                format!(" /{} ", c.name)
            } else {
                format!(" /{} {} ", c.name, c.args)
            };
            let line = Line::from(vec![
                Span::styled(
                    label,
                    Style::default()
                        .fg(Color::Black)
                        .bg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!("  {}", c.help),
                    Style::default().fg(Color::DarkGray),
                ),
            ]);
            let block = Block::default()
                .borders(Borders::ALL)
                .title(" command format — Enter run ")
                .border_style(Style::default().fg(Color::DarkGray));
            f.render_widget(Paragraph::new(line).block(block), area);
        }
        return;
    }
    let matches = app.slash_matches();
    let sel = app.slash_index.min(matches.len().saturating_sub(1));
    let rows = area.height.saturating_sub(2) as usize;
    let lines: Vec<Line> = matches
        .iter()
        .take(rows)
        .enumerate()
        .map(|(i, c)| {
            let label = format!(
                " /{}{} ",
                c.name,
                if c.args.is_empty() {
                    String::new()
                } else {
                    format!(" {}", c.args)
                }
            );
            let label_style = if i == sel {
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Yellow)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::Yellow)
            };
            Line::from(vec![
                Span::styled(label, label_style),
                Span::styled(
                    format!("  {}", c.help),
                    Style::default().fg(Color::DarkGray),
                ),
            ])
        })
        .collect();
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" commands — ↑↓ select · Tab complete · Enter run ")
        .border_style(Style::default().fg(Color::DarkGray));
    f.render_widget(Paragraph::new(lines).block(block), area);
}

/// Height of the burn-in progress panel: a bordered box with three content
/// rows (phase clock, gauge, current step), or zero when no burn is running.
fn burn_height(app: &App) -> u16 {
    if app.burn.is_some() { 5 } else { 0 }
}

/// Render the live burn-in progress panel: which phase is running, an elapsed
/// clock and spinner, a batch progress gauge, the running tagged-file count,
/// and the current step — so a slow run reads as "working" rather than frozen.
fn render_burn(f: &mut Frame, area: Rect, app: &App) {
    if area.height == 0 {
        return;
    }
    let Some(burn) = &app.burn else { return };
    let p = &burn.progress;

    let elapsed = burn.started.elapsed();
    let secs = elapsed.as_secs();
    let spin = SPINNER[(elapsed.as_millis() / 90) as usize % SPINNER.len()];

    let block = Block::default()
        .borders(Borders::ALL)
        .title(" 🔥 burn-in ")
        .border_style(Style::default().fg(CRUST));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let [head, gauge_area, detail] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(inner);

    // Phase + spinner + elapsed clock + running tally.
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                format!("{spin} "),
                Style::default().fg(CRUST).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                p.phase.label(),
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!(
                    "  ·  {:02}:{:02} elapsed  ·  {} file(s) tagged",
                    secs / 60,
                    secs % 60,
                    p.tagged
                ),
                Style::default().fg(Color::DarkGray),
            ),
        ])),
        head,
    );

    // A real progress bar during the batched tagging phase; an indeterminate
    // note during the single-step dependency/survey phases.
    if p.total > 0 {
        let ratio = (p.done as f64 / p.total as f64).clamp(0.0, 1.0);
        f.render_widget(
            Gauge::default()
                .ratio(ratio)
                .label(format!(
                    "batch {}/{}  ({:.0}%)",
                    p.done,
                    p.total,
                    ratio * 100.0
                ))
                .gauge_style(Style::default().fg(Color::Yellow).bg(Color::Black)),
            gauge_area,
        );
    } else {
        f.render_widget(
            Paragraph::new(Span::styled(
                "working… (a single long step — slow on a local model)",
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::ITALIC),
            )),
            gauge_area,
        );
    }

    // The current step (the file/batch in flight), truncated to fit one row.
    f.render_widget(
        Paragraph::new(Span::styled(
            p.detail.clone(),
            Style::default().fg(Color::Gray),
        ))
        .wrap(Wrap { trim: true }),
        detail,
    );
}

fn render_header(f: &mut Frame, area: Rect, assistant: &Assistant) {
    // The gauge is the first thing to go when the window narrows: the title
    // carries the provider and the mode, which is the part you can't infer.
    let gauge_width = match area.width {
        w if w >= 64 => 34,
        w if w >= 46 => 22,
        _ => 0,
    };
    let [title_area, gauge_area] =
        Layout::horizontal([Constraint::Min(16), Constraint::Length(gauge_width)]).areas(area);

    let mode = assistant.mode();
    let mode_color = match mode {
        Mode::Plan => Color::Blue,
        Mode::Edit => Color::Yellow,
        Mode::AutoAccept => Color::Red,
    };
    let title = Paragraph::new(Line::from(vec![
        Span::styled(
            " ciabatta ai ",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
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

    if gauge_width == 0 {
        return;
    }

    let confidence = assistant.brain.confidence();
    let gauge = Gauge::default()
        .ratio((confidence / 100.0).clamp(0.0, 1.0))
        .label(format!("confidence {confidence:.0}/100"))
        .gauge_style(
            Style::default()
                .fg(gauge_color(confidence))
                .bg(Color::Black),
        );
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

/// Render one line of assistant Markdown into styled spans, applying the
/// inline emphasis the model tends to emit. Block prefixes (`#` headings,
/// `-`/`*`/`+` bullets) are handled first, then the remainder is parsed for
/// inline `**bold**`, `*italic*`/`_italic_`, and `` `code` ``.
fn render_markdown_line(line: &str, base: Style) -> Vec<Span<'static>> {
    let trimmed = line.trim_start();
    let indent = &line[..line.len() - trimmed.len()];

    // `#`..`######` + space → a bold heading with the markers dropped.
    let hashes = trimmed.chars().take_while(|&c| c == '#').count();
    if (1..=6).contains(&hashes) && trimmed[hashes..].starts_with(' ') {
        let mut spans = vec![Span::styled(indent.to_string(), base)];
        spans.extend(inline_md(
            trimmed[hashes..].trim_start(),
            base.add_modifier(Modifier::BOLD),
        ));
        return spans;
    }

    // A leading `-`, `*`, or `+` followed by a space → a bullet glyph. (The
    // trailing-space check keeps `*italic*` and `**bold**` out of this branch.)
    for marker in ['-', '*', '+'] {
        if let Some(rest) = trimmed.strip_prefix(marker)
            && let Some(item) = rest.strip_prefix(' ')
        {
            let mut spans = vec![Span::styled(format!("{indent}• "), base.fg(CRUST))];
            spans.extend(inline_md(item, base));
            return spans;
        }
    }

    inline_md(line, base)
}

/// Parse inline Markdown emphasis in one line into styled spans, stripping the
/// markers. Bold/italic nest (parsed recursively); inline code is verbatim.
/// Unmatched or space-hugging markers are left as literal text, so prose like
/// `2 * 3` or a dangling `**` during the typewriter reveal renders unharmed.
fn inline_md(text: &str, base: Style) -> Vec<Span<'static>> {
    let chars: Vec<char> = text.chars().collect();
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut buf = String::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];

        // Inline code: `...` — verbatim, no emphasis parsed inside.
        if c == '`'
            && let Some(close) = find_delim_close(&chars, i + 1, '`', 1, false)
        {
            push_buf(&mut buf, &mut spans, base);
            spans.push(Span::styled(
                chars[i + 1..close].iter().collect::<String>(),
                base.fg(CODE),
            ));
            i = close + 1;
            continue;
        }

        // Bold: **...** or __...__ (the delimiter must hug non-space text).
        if (c == '*' || c == '_')
            && chars.get(i + 1) == Some(&c)
            && chars
                .get(i + 2)
                .is_some_and(|n| !n.is_whitespace() && *n != c)
            && let Some(close) = find_delim_close(&chars, i + 2, c, 2, true)
        {
            push_buf(&mut buf, &mut spans, base);
            let inner: String = chars[i + 2..close].iter().collect();
            spans.extend(inline_md(&inner, base.add_modifier(Modifier::BOLD)));
            i = close + 2;
            continue;
        }

        // Italic: *...* or _..._.
        if (c == '*' || c == '_')
            && chars
                .get(i + 1)
                .is_some_and(|n| !n.is_whitespace() && *n != c)
            && let Some(close) = find_delim_close(&chars, i + 1, c, 1, true)
        {
            push_buf(&mut buf, &mut spans, base);
            let inner: String = chars[i + 1..close].iter().collect();
            spans.extend(inline_md(&inner, base.add_modifier(Modifier::ITALIC)));
            i = close + 1;
            continue;
        }

        buf.push(c);
        i += 1;
    }
    push_buf(&mut buf, &mut spans, base);
    if spans.is_empty() {
        spans.push(Span::styled(String::new(), base));
    }
    spans
}

/// Find the closing delimiter run (`len` copies of `d`) at/after `from`. When
/// `strict` (emphasis), the run must not be extended by another `d` and its
/// inner neighbor must be non-whitespace; when false (code), any next `d` wins.
fn find_delim_close(
    chars: &[char],
    from: usize,
    d: char,
    len: usize,
    strict: bool,
) -> Option<usize> {
    let mut i = from;
    while i + len <= chars.len() {
        let is_run = chars[i..i + len].iter().all(|&x| x == d);
        let not_extended = !strict || chars.get(i + len) != Some(&d);
        let inner_ok = i > from && (!strict || !chars[i - 1].is_whitespace());
        if is_run && not_extended && inner_ok {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// Flush the plain-text accumulator into a styled span, if non-empty.
fn push_buf(buf: &mut String, spans: &mut Vec<Span<'static>>, style: Style) {
    if !buf.is_empty() {
        spans.push(Span::styled(std::mem::take(buf), style));
    }
}

fn render_chat(f: &mut Frame, area: Rect, app: &mut App) {
    // The newest assistant answer is revealed with a left-to-right typewriter
    // wipe; find it so we can trim its text to the current reveal point.
    let last_assistant = app
        .entries
        .iter()
        .rposition(|e| e.speaker == Speaker::Assistant);
    let reveal = app.reveal_since.map(|t| t.elapsed().as_secs_f32());

    // Each line is paired with whether it may be re-wrapped: the mascot is a
    // drawing, and wrapping a drawing is just breaking it.
    let mut lines: Vec<(Line<'static>, bool)> = Vec::new();
    for (idx, entry) in app.entries.iter().enumerate() {
        let (prefix, style) = match entry.speaker {
            Speaker::You => (
                "you ▸ ",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Speaker::Assistant => ("ai  ▸ ", Style::default().fg(Color::Yellow)),
            Speaker::Status | Speaker::Diff => ("      ", Style::default().fg(Color::DarkGray)),
            Speaker::Error => ("err ▸ ", Style::default().fg(Color::Red)),
            Speaker::Banner => (
                "   ",
                Style::default().fg(CRUST).add_modifier(Modifier::BOLD),
            ),
        };

        // Substitute a partially-revealed copy for the answer being typed in.
        let revealed = match reveal {
            Some(e) if Some(idx) == last_assistant && e < REVEAL_SECS => {
                let total = entry.text.chars().count();
                let shown = ((total as f32) * (e / REVEAL_SECS)).ceil() as usize;
                let mut s: String = entry.text.chars().take(shown).collect();
                s.push('▌'); // a block caret at the leading edge of the wipe
                Some(s)
            }
            _ => None,
        };
        let text = revealed.as_deref().unwrap_or(&entry.text);

        for (i, raw) in text.lines().enumerate() {
            let head = match entry.speaker {
                Speaker::Banner => "   ",
                _ if i == 0 => prefix,
                _ => "      ",
            };
            let body_style = match entry.speaker {
                Speaker::Status => Style::default().fg(Color::DarkGray),
                Speaker::Error => Style::default().fg(Color::Red),
                Speaker::Banner => Style::default().fg(CRUST),
                // Diff lines get git-style coloring by their first character.
                Speaker::Diff => {
                    if raw.starts_with("+++") || raw.starts_with("---") {
                        Style::default()
                            .fg(Color::White)
                            .add_modifier(Modifier::BOLD)
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
            if entry.speaker == Speaker::Banner {
                // The loaf is colored per-character (crust / crumb / face).
                let mut spans = vec![Span::raw(head)];
                spans.extend(banner_spans(raw));
                lines.push((Line::from(spans), false));
            } else if entry.speaker == Speaker::Assistant {
                // The AI answers in Markdown — render its inline emphasis.
                let mut spans = vec![Span::styled(head, style)];
                spans.extend(render_markdown_line(raw, body_style));
                lines.push((Line::from(spans), true));
            } else {
                lines.push((
                    Line::from(vec![
                        Span::styled(head, style),
                        Span::styled(raw.to_string(), body_style),
                    ]),
                    true,
                ));
            }
        }
        if entry.speaker == Speaker::Assistant {
            lines.push((Line::default(), false));
        }
    }
    if app.busy && app.burn.is_none() {
        // Show a live elapsed clock so a stalled request reads as stalled rather
        // than as a frozen UI, and hint at the cause once it runs unusually long.
        // (Skipped during a burn-in, which has its own richer progress panel.)
        let millis = app.busy_since.map(|t| t.elapsed().as_millis()).unwrap_or(0);
        let elapsed = millis / 1000;
        let spin = SPINNER[(millis / 90) as usize % SPINNER.len()];
        let text = if elapsed >= 30 {
            format!(
                "thinking ({elapsed}s) — still waiting on the model; if it's a local one it \
                 may be loading, otherwise check the endpoint is reachable. Times out at 600s."
            )
        } else if elapsed >= 1 {
            format!("thinking ({elapsed}s)")
        } else {
            "thinking".to_string()
        };
        lines.push((
            Line::from(vec![
                Span::styled(
                    format!("   {spin}  "),
                    Style::default().fg(CRUST).add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    text,
                    Style::default()
                        .fg(Color::DarkGray)
                        .add_modifier(Modifier::ITALIC),
                ),
            ]),
            true,
        ));
    }

    // Wrap here rather than leaving it to ratatui, so the rows the scroll is
    // computed from are the rows that get drawn — the two drifting apart is
    // what used to clip the newest message off the bottom — and so a wrapped
    // message keeps its indent under the speaker that said it.
    let inner_width = area.width.saturating_sub(2).max(1) as usize;
    let wrapped: Vec<Line> = lines
        .into_iter()
        .flat_map(|(line, wrap)| {
            if wrap {
                wrap_spans(line.spans, inner_width, PREFIX_WIDTH)
            } else {
                vec![line]
            }
        })
        .collect();

    let total = wrapped.len();
    let visible = area.height.saturating_sub(2) as usize;
    let bottom = total.saturating_sub(visible).min(u16::MAX as usize) as u16;

    if inner_width != app.chat_width {
        // A resize reflows every message, so "lines from the bottom" measures a
        // conversation that no longer exists. Rescaling it in proportion holds
        // the reader roughly where they were, where leaving it alone would walk
        // the view toward one end every time the window is dragged.
        if app.scroll_up > 0 && app.chat_top > 0 {
            let ratio = f64::from(app.scroll_up) / f64::from(app.chat_top);
            app.scroll_up = ((ratio * f64::from(bottom)).round() as u16).min(bottom);
        }
        app.chat_width = inner_width;
    } else if app.scroll_up > 0 {
        // The user is reading scrollback: grow `scroll_up` by however many
        // lines just arrived below, so the view stays anchored on the same
        // messages instead of drifting (or snapping) toward the bottom.
        app.scroll_up = (app.scroll_up + bottom.saturating_sub(app.chat_top)).min(bottom);
    }
    app.chat_top = bottom;
    let scroll = bottom.saturating_sub(app.scroll_up);

    let chat = Paragraph::new(wrapped)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::DarkGray)),
        )
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
            Span::styled(
                "Ctrl-Y accept · Ctrl-N reject",
                Style::default().fg(Color::DarkGray),
            ),
        ])
    }));
    lines.truncate(area.height as usize);
    f.render_widget(Paragraph::new(lines), area);
}

fn render_input(f: &mut Frame, area: Rect, app: &mut App) {
    let inner_w = area.width.saturating_sub(2).max(1) as usize;
    let inner_h = area.height.saturating_sub(2).max(1) as usize;
    // Vertical cursor movement is defined in rows, and rows only exist relative
    // to a width — so tell the key handler the one it is looking at. A resize
    // corrects it on the next frame, before any key can be pressed against it.
    app.input_width = inner_w;

    let chars: Vec<char> = app.input.text().chars().collect();
    let rows = wrap_input(app.input.text(), inner_w);
    let (cursor_row, cursor_col) = cursor_rc(&rows, app.input.cursor, inner_w);

    // Scroll to keep the *cursor's* row in view rather than the last one: with
    // a long paste being edited from the top, the interesting row is the one
    // being typed on.
    let scroll = cursor_row.saturating_sub(inner_h - 1);

    let lines: Vec<Line> = rows
        .iter()
        .skip(scroll)
        .map(|row| Line::from(row_text(&chars, row, inner_w)))
        .collect();

    let input = Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::ALL)
            .title(if app.busy {
                " waiting for the model… "
            } else {
                " ask "
            })
            .border_style(Style::default().fg(if app.busy {
                Color::DarkGray
            } else {
                Color::Yellow
            })),
    );
    f.render_widget(input, area);

    if !app.busy {
        f.set_cursor_position((
            area.x + 1 + cursor_col as u16,
            area.y + 1 + (cursor_row - scroll) as u16,
        ));
    }
}

fn render_hints(f: &mut Frame, area: Rect, app: &App) {
    // Most important first, and only as many as fit: a single row that gets
    // sliced through the middle of a binding teaches nothing.
    let mut hints: Vec<String> = [
        "Enter send",
        "/ commands",
        "A-Enter newline",
        "←→ move · ^←→ word",
        "^W/^U/^K delete",
        "S-Tab mode",
        "^A/^X change",
        "^Z undo",
        "^S pdf",
        "^O vscode",
        "^Y/^N tags",
        "^T select",
        "Esc quit",
    ]
    .iter()
    .map(|h| (*h).to_string())
    .collect();
    if let Some(url) = &app.graph_url {
        hints.push(format!("map: {url}"));
    }

    let mut line = String::new();
    for hint in hints {
        let candidate = if line.is_empty() {
            format!(" {hint}")
        } else {
            format!("{line} · {hint}")
        };
        if candidate.chars().count() > area.width as usize {
            break;
        }
        line = candidate;
    }

    f.render_widget(
        Paragraph::new(Span::styled(line, Style::default().fg(Color::DarkGray))),
        area,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The visible text of a set of spans, with all markup stripped.
    fn plain(spans: &[Span]) -> String {
        spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn emphasis_strips_markers_and_applies_modifiers() {
        let spans = inline_md("a **bold** and *italic* text", Style::default());
        assert_eq!(plain(&spans), "a bold and italic text");
        let bold = spans.iter().find(|s| s.content.as_ref() == "bold").unwrap();
        assert!(bold.style.add_modifier.contains(Modifier::BOLD));
        let italic = spans
            .iter()
            .find(|s| s.content.as_ref() == "italic")
            .unwrap();
        assert!(italic.style.add_modifier.contains(Modifier::ITALIC));
    }

    #[test]
    fn underscore_emphasis_works_too() {
        let spans = inline_md("__b__ and _i_", Style::default());
        assert_eq!(plain(&spans), "b and i");
        assert!(
            spans
                .iter()
                .find(|s| s.content.as_ref() == "b")
                .unwrap()
                .style
                .add_modifier
                .contains(Modifier::BOLD)
        );
        assert!(
            spans
                .iter()
                .find(|s| s.content.as_ref() == "i")
                .unwrap()
                .style
                .add_modifier
                .contains(Modifier::ITALIC)
        );
    }

    #[test]
    fn literal_and_unclosed_markers_are_left_alone() {
        // A lone `*` around spaces is arithmetic, not emphasis.
        assert_eq!(
            plain(&inline_md("2 * 3 = 6", Style::default())),
            "2 * 3 = 6"
        );
        // An unterminated run (e.g. mid-reveal) renders verbatim.
        assert_eq!(
            plain(&inline_md("**oops unclosed", Style::default())),
            "**oops unclosed"
        );
    }

    #[test]
    fn inline_code_is_verbatim_and_colored() {
        let spans = inline_md("run `cargo **test**` now", Style::default());
        assert_eq!(plain(&spans), "run cargo **test** now");
        let code = spans.iter().find(|s| s.content.contains("cargo")).unwrap();
        assert_eq!(code.style.fg, Some(CODE));
        assert!(!code.style.add_modifier.contains(Modifier::BOLD));
    }

    /// The rows a wrap produced, as plain strings.
    fn rows(text: &str, width: usize) -> Vec<String> {
        let chars: Vec<char> = text.chars().collect();
        wrap_input(text, width)
            .iter()
            .map(|row| row_text(&chars, row, width))
            .collect()
    }

    /// An App with nothing in it, for the render tests.
    fn test_app() -> App {
        App {
            entries: Vec::new(),
            input: Input::default(),
            busy: false,
            busy_since: None,
            scroll_up: 0,
            chat_top: 0,
            chat_width: 0,
            suggestions: Vec::new(),
            graph_url: None,
            slash_index: 0,
            last_answer: None,
            report_pending: false,
            last_report_days: 7,
            reveal_since: None,
            mouse_capture: true,
            burn: None,
            input_width: 80,
        }
    }

    /// Draw the input box at a given size and report where the cursor landed.
    fn draw_input(app: &mut App, width: u16, height: u16) -> (u16, u16) {
        let mut terminal =
            Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|f| {
                let area = f.area();
                render_input(f, area, app);
            })
            .unwrap();
        let at = terminal.get_cursor_position().unwrap();
        (at.x, at.y)
    }

    #[test]
    fn the_drawn_cursor_follows_the_text_across_a_resize() {
        let mut app = test_app();
        app.input.set("alpha beta gamma");
        app.input.cursor = 7; // just after "beta"'s b

        // 20 columns wide: two borders leave 18, so the message is one row.
        let (x, y) = draw_input(&mut app, 20, 5);
        assert_eq!((x, y), (1 + 7, 1));

        // Narrow it to eight usable columns and the message reflows to
        // "alpha " / "beta " / "gamma": the same character is now the second on
        // the second row. The box and the cursor wrap from one layout, so they
        // can't disagree about that.
        let (x, y) = draw_input(&mut app, 10, 5);
        assert_eq!(app.input_width, 8);
        assert_eq!((x, y), (1 + 1, 1 + 1));
    }

    #[test]
    fn typing_and_deleting_happen_at_the_cursor() {
        let mut input = Input::default();
        for c in "hello world".chars() {
            input.insert_char(c);
        }
        input.cursor = 5;
        input.insert_str(" there");
        assert_eq!(input.text(), "hello there world");
        assert_eq!(input.cursor, 11);

        input.backspace();
        assert_eq!(input.text(), "hello ther world");
        input.delete();
        assert_eq!(input.text(), "hello therworld");
    }

    #[test]
    fn word_movement_and_kills_agree_about_words() {
        let mut input = Input::default();
        input.set("one two three");
        assert_eq!(input.cursor, 13);
        assert_eq!(input.word_start(), 8);

        let to = input.cursor;
        input.delete_range(input.word_start(), to);
        assert_eq!(input.text(), "one two ");
        assert_eq!(input.cursor, 8);

        // From inside the trailing space, `word_start` skips back over it.
        input.set("alpha beta");
        input.cursor = 0;
        assert_eq!(input.word_end(), 5);
    }

    #[test]
    fn home_and_end_stay_on_the_cursor_s_own_line() {
        let mut input = Input::default();
        input.set("first\nsecond");
        input.cursor = 8; // inside "second"
        assert_eq!(input.line_start(), 6);
        assert_eq!(input.line_end(), 12);
        input.cursor = 3; // inside "first"
        assert_eq!(input.line_start(), 0);
        assert_eq!(input.line_end(), 5);
    }

    #[test]
    fn wrapping_breaks_at_spaces_and_splits_only_what_it_must() {
        assert_eq!(rows("hello world", 5), vec!["hello", "world"]);
        assert_eq!(rows("a b c", 5), vec!["a b c"]);
        // A word with nowhere to break is split rather than dropped.
        assert_eq!(
            rows("supercalifragilistic", 6),
            vec!["superc", "alifra", "gilist", "ic"]
        );
        // Newlines start a row of their own, including empty ones.
        assert_eq!(rows("a\n\nb", 10), vec!["a", "", "b"]);
    }

    #[test]
    fn the_cursor_follows_the_same_wrap_the_box_draws() {
        let text = "hello world";
        let wrapped = wrap_input(text, 5);
        // Start of the second row.
        assert_eq!(cursor_rc(&wrapped, 6, 5), (1, 0));
        // A cursor at the right edge belongs at the start of the row below,
        // which is where a terminal would put it.
        assert_eq!(cursor_rc(&wrap_input("abcde", 5), 5, 5), (1, 0));
    }

    #[test]
    fn wrapped_messages_keep_their_indent() {
        let spans = vec![Span::raw("you ▸ "), Span::raw("alpha beta gamma delta")];
        let lines = wrap_spans(spans, 16, PREFIX_WIDTH);
        let text: Vec<String> = lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect();
        // The space the break was taken at rides along at the edge, where it
        // isn't drawn.
        assert_eq!(text[0].trim_end(), "you ▸ alpha beta");
        // Continuation rows sit under the message, not against the margin.
        for row in &text[1..] {
            assert!(row.starts_with("      "), "{row:?}");
        }
        // Nothing is lost in the reflow.
        let joined: String = text
            .iter()
            .map(|r| r.trim().to_string())
            .collect::<Vec<_>>()
            .join(" ");
        assert!(joined.contains("alpha beta gamma delta"), "{joined:?}");
    }

    #[test]
    fn headings_and_bullets_are_reshaped() {
        let heading = render_markdown_line("## Big Title", Style::default());
        assert_eq!(plain(&heading), "Big Title");
        assert!(
            heading
                .iter()
                .any(|s| s.style.add_modifier.contains(Modifier::BOLD))
        );

        let bullet = render_markdown_line("- an item", Style::default());
        assert_eq!(plain(&bullet), "• an item");
    }
}
