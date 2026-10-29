use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Margin, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{
        Block, Borders, Clear, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState, Wrap,
    },
};

use super::app::{App, Pane, Prompt, RunStatus, StepStatus};

/// The narrowest and widest the step list is allowed to be, as columns and as
/// a share of the screen: wide enough for a monorepo's `package#step` names,
/// never so wide the output has no room.
const LIST_MIN: u16 = 24;
const LIST_MAX_PERCENT: u16 = 40;

pub fn render(f: &mut Frame, app: &mut App) {
    let area = f.area();
    let banner_rows = app.banner.as_ref().map_or(0, |b| {
        // Wrapped to the screen, plus its border.
        let width = area.width.saturating_sub(4).max(1) as usize;
        (b.chars().count().div_ceil(width) as u16).clamp(1, 4) + 2
    });

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),           // header
            Constraint::Length(banner_rows), // warning banner, if any
            Constraint::Min(3),              // steps + output
            Constraint::Length(1),           // help bar
        ])
        .split(area);

    render_header(f, chunks[0], app);
    if app.selecting {
        // Just the text: a drag across the screen copies nothing else.
        let body = chunks[1].union(chunks[2]);
        render_logs(f, body, app);
        render_help(f, chunks[3], app);
        render_prompt(f, body, app);
        return;
    }
    if let Some(message) = &app.banner {
        let banner = Paragraph::new(message.as_str())
            .wrap(Wrap { trim: true })
            .style(Style::default().fg(Color::Yellow))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::Yellow)),
            );
        f.render_widget(banner, chunks[1]);
    }
    render_body(f, chunks[2], app);
    render_help(f, chunks[3], app);
    render_prompt(f, chunks[2], app);
}

/// The question at the front of the queue, in a box over the body.
fn render_prompt(f: &mut Frame, area: Rect, app: &App) {
    let Some(prompt) = app.prompts.front() else {
        return;
    };
    let bold = Style::default()
        .fg(Color::White)
        .add_modifier(Modifier::BOLD);
    let faint = Style::default().fg(Color::DarkGray);
    let mut lines: Vec<Line> = Vec::new();
    let title = match prompt {
        Prompt::Input { pending, typed } => {
            lines.push(Line::styled(pending.prompt.clone(), bold));
            if let Some(details) = &pending.details {
                lines.extend(details.lines().map(|l| Line::styled(l.to_string(), faint)));
            }
            lines.push(Line::raw(""));
            let shown = match (pending.enter, pending.yes_no, pending.secret) {
                (true, _, _) => "⏎ continue".to_string(),
                (false, true, _) => match pending.default.as_deref() {
                    Some("y") => "[Y/n]".to_string(),
                    Some("n") => "[y/N]".to_string(),
                    _ => "[y/n]".to_string(),
                },
                (false, false, true) => "•".repeat(typed.chars().count()),
                (false, false, false) => typed.clone(),
            };
            let mut field = vec![Span::styled("› ", Style::default().fg(Color::Yellow))];
            field.push(Span::styled(shown, Style::default().fg(Color::White)));
            if !pending.enter && !pending.yes_no {
                field.push(Span::styled("▏", Style::default().fg(Color::Yellow)));
            }
            lines.push(Line::from(field));
            let mut hints: Vec<String> = Vec::new();
            match (&pending.default, pending.enter) {
                (_, true) => hints.push("Enter to continue".into()),
                (Some(default), false) => hints.push(match pending.secret {
                    true => "Enter for the default".into(),
                    false => format!("Enter for '{default}'"),
                }),
                (None, false) => {}
            }
            if let Some(left) = remaining(pending.deadline.as_deref()) {
                hints.push(format!("{left} left"));
            }
            if let Some(var) = &pending.env {
                hints.push(format!("set {var} to skip this"));
            }
            if !hints.is_empty() {
                lines.push(Line::styled(hints.join(" · "), faint));
            }
            format!(" ❓ {} asks ", pending.step)
        }
        Prompt::Choice {
            step,
            message,
            options,
            selected,
            ..
        } => {
            lines.push(Line::styled(message.clone(), bold));
            lines.push(Line::raw(""));
            for (i, option) in options.iter().enumerate() {
                let style = match i == *selected {
                    true => Style::default().fg(Color::Black).bg(Color::Yellow),
                    false => Style::default().fg(Color::Gray),
                };
                lines.push(Line::styled(format!(" [{i}] {option} "), style));
            }
            lines.push(Line::styled(
                "↑↓ or a number to pick · Enter to run it",
                faint,
            ));
            format!(" ⚠ {step} needs a fix chosen ")
        }
    };
    let waiting = app.prompts.len() - 1;
    if waiting > 0 {
        lines.push(Line::styled(
            format!("{waiting} more waiting after this"),
            faint,
        ));
    }

    let width = area.width.saturating_sub(4).min(80);
    let inner = width.saturating_sub(2).max(1) as usize;
    let rows: u16 = lines
        .iter()
        .map(|l| (l.width().max(1).div_ceil(inner)) as u16)
        .sum();
    let height = (rows + 2).min(area.height);
    if width < 4 || height < 3 {
        return;
    }
    let rect = Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    };
    f.render_widget(Clear, rect);
    f.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: false }).block(
            Block::default()
                .title(title)
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Yellow)),
        ),
        rect,
    );
}

/// How long until an RFC 3339 `deadline`, as the prompt's countdown shows it.
fn remaining(deadline: Option<&str>) -> Option<String> {
    let at = chrono::DateTime::parse_from_rfc3339(deadline?).ok()?;
    let left = (at.with_timezone(&chrono::Utc) - chrono::Utc::now())
        .to_std()
        .unwrap_or_default();
    Some(crate::run::input::clock(left))
}

fn render_header(f: &mut Frame, area: Rect, app: &App) {
    let (word, color) = match (&app.run.status, app.finished) {
        (RunStatus::Failed(_), _) => ("failed", Color::Red),
        (RunStatus::Succeeded, _) if app.failed() => ("failed", Color::Red),
        (RunStatus::Succeeded, _) => ("succeeded", Color::Green),
        (RunStatus::Running, true) => ("stopped", Color::Red),
        (RunStatus::Running, false) if app.stopping => ("stopping…", Color::Yellow),
        (RunStatus::Running, false) => ("running", Color::Yellow),
    };
    let mut spans = vec![
        Span::styled(
            " 🍞 ciabatta ",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            app.run.name.clone(),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled(word, Style::default().fg(color)),
        Span::styled(
            format!(
                "  ·  {}/{} steps  ·  {}",
                app.settled(),
                app.steps.len(),
                crate::runner::elapsed(app.run.elapsed())
            ),
            Style::default().fg(Color::DarkGray),
        ),
    ];
    if app.dry_run {
        spans.push(Span::styled(
            "  DRY-RUN",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn render_body(f: &mut Frame, area: Rect, app: &mut App) {
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(list_width(app, area.width)),
            Constraint::Min(10),
        ])
        .split(area);

    render_steps(f, chunks[0], app);
    render_logs(f, chunks[1], app);
}

/// As wide as the longest name with its emoji and clock, within bounds.
fn list_width(app: &App, screen: u16) -> u16 {
    let longest = std::iter::once(app.run.name.chars().count())
        .chain(app.steps.iter().map(|s| s.name.chars().count()))
        .max()
        .unwrap_or(0) as u16;
    // Border, space, emoji, space, name, space, clock, scrollbar.
    let wanted = longest.saturating_add(16);
    let most = (screen * LIST_MAX_PERCENT / 100).max(LIST_MIN);
    wanted.clamp(LIST_MIN, most)
}

fn render_steps(f: &mut Frame, area: Rect, app: &mut App) {
    let block = Block::default()
        .title(format!(" Steps · {} ", app.sort.word()))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let rows = inner.height as usize;
    if rows == 0 {
        return;
    }
    // Scroll the list only as far as it takes to keep the selection in view.
    if app.selected < app.list_offset {
        app.list_offset = app.selected;
    } else if app.selected >= app.list_offset + rows {
        app.list_offset = app.selected + 1 - rows;
    }
    app.list_offset = app.list_offset.min(app.entries().saturating_sub(rows));

    let width = inner.width.saturating_sub(1) as usize; // room for the scrollbar
    for row in 0..rows {
        let index = app.list_offset + row;
        if index >= app.entries() {
            break;
        }
        let line = if index == 0 {
            entry_line(
                run_emoji(app),
                &app.run.name,
                Some(app.run.elapsed()),
                Style::default().add_modifier(Modifier::BOLD),
                width,
            )
        } else {
            let step = app.listed(index - 1);
            entry_line(
                step.status.emoji(),
                &step.name,
                step.elapsed(),
                name_style(step.status),
                width,
            )
        };
        let line = if index == app.selected {
            line.style(Style::default().bg(Color::DarkGray))
        } else {
            line
        };
        f.render_widget(
            Paragraph::new(line),
            Rect {
                x: inner.x,
                y: inner.y + row as u16,
                width: inner.width.saturating_sub(1),
                height: 1,
            },
        );
    }

    if app.entries() > rows {
        scrollbar(
            f,
            area,
            app.entries().saturating_sub(rows),
            app.list_offset,
            rows,
        );
    }
}

/// A scrollbar down the right-hand border of `area`. ratatui panics on an
/// empty track, which a small enough terminal produces, so that's skipped.
fn scrollbar(f: &mut Frame, area: Rect, content: usize, position: usize, viewport: usize) {
    let track = area.inner(Margin {
        vertical: 1,
        horizontal: 0,
    });
    if track.width == 0 || track.height == 0 {
        return;
    }
    let mut state = ScrollbarState::new(content)
        .position(position)
        .viewport_content_length(viewport);
    f.render_stateful_widget(
        Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(None)
            .end_symbol(None),
        track,
        &mut state,
    );
}

/// One row of the list: emoji, name (cut short with … if it must be), and the
/// clock right-aligned.
fn entry_line(
    emoji: &str,
    name: &str,
    elapsed: Option<std::time::Duration>,
    style: Style,
    width: usize,
) -> Line<'static> {
    let clock = elapsed.map(crate::runner::elapsed).unwrap_or_default();
    // " " + emoji (2 columns) + " " before the name; " " + clock after it.
    let fixed = 4 + if clock.is_empty() { 0 } else { clock.len() + 1 };
    let room = width.saturating_sub(fixed);
    let name = truncate(name, room);
    let pad = room.saturating_sub(name.chars().count());
    let mut spans = vec![
        Span::raw(format!(" {emoji} ")),
        Span::styled(name, style),
        Span::raw(" ".repeat(pad)),
    ];
    if !clock.is_empty() {
        spans.push(Span::styled(
            format!(" {clock}"),
            Style::default().fg(Color::DarkGray),
        ));
    }
    Line::from(spans)
}

fn truncate(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        return text.to_string();
    }
    if width == 0 {
        return String::new();
    }
    let mut cut: String = text.chars().take(width - 1).collect();
    cut.push('…');
    cut
}

fn run_emoji(app: &App) -> &'static str {
    match &app.run.status {
        RunStatus::Failed(_) => "❌",
        RunStatus::Succeeded if app.failed() => "❌",
        RunStatus::Succeeded => "✅",
        RunStatus::Running if app.finished => "⛔",
        RunStatus::Running => "🍞",
    }
}

fn name_style(status: StepStatus) -> Style {
    let color = match status {
        StepStatus::Failed => Color::Red,
        StepStatus::Warned | StepStatus::Asking => Color::Yellow,
        s if s.is_active() => Color::White,
        StepStatus::Pending | StepStatus::NotRun | StepStatus::NotNeeded => Color::DarkGray,
        _ => Color::Gray,
    };
    let style = Style::default().fg(color);
    if status.is_active() {
        style.add_modifier(Modifier::BOLD)
    } else {
        style
    }
}

fn render_logs(f: &mut Frame, area: Rect, app: &mut App) {
    let mut title = match app.selected_step() {
        _ if app.pane == Pane::Env => " .env · the environment this run resolved".to_string(),
        Some(step) => {
            let mut t = format!(
                " {} {} · {}",
                step.status.emoji(),
                step.name,
                step.status.word()
            );
            if let Some(d) = step.elapsed() {
                t.push_str(&format!(" · {}", crate::runner::elapsed(d)));
            }
            t
        }
        None => format!(" {} {} · all output", run_emoji(app), app.run.name),
    };

    // Borders and a scrollbar, unless the text is being selected.
    let (inner_width, height) = match app.selecting {
        true => (area.width as usize, area.height as usize),
        false => (
            area.width.saturating_sub(3) as usize,
            area.height.saturating_sub(2) as usize,
        ),
    };

    // Each wrapped segment is one row, so scrolling counts what's on screen.
    // The env report arrives coloured, as the terminal shows it; step output
    // is styled here, line by line.
    let coloured = app.pane == Pane::Env;
    let rows: Vec<Line> = app
        .selected_logs()
        .iter()
        .flat_map(|l| match coloured {
            true => ansi_rows(l, inner_width),
            false => {
                let style = log_style(l);
                wrap_line(l, inner_width)
                    .into_iter()
                    .map(move |seg| Line::styled(seg, style))
                    .collect()
            }
        })
        .collect();
    let total = rows.len();
    let most = total.saturating_sub(height);
    app.saw_rows(total);
    // Scrolled further than there is to scroll: hold at the top.
    if let Some(n) = app.scroll
        && n > most
    {
        app.scroll = if most == 0 { None } else { Some(most) };
    }
    let back = app.scroll.unwrap_or(0);
    let start = most - back;

    if back > 0 {
        title.push_str(&format!(" · ↑{back} (f to follow)"));
    }
    title.push(' ');

    let empty = rows.is_empty();
    let visible: Vec<Line> = rows.into_iter().skip(start).take(height).collect();
    let body = if empty {
        let note = match app.selected_step().map(|s| s.status) {
            Some(StepStatus::Pending) => "Waiting for what it needs.",
            Some(StepStatus::NotRun) => "Never started.",
            Some(_) => "No output.",
            None => "Starting…",
        };
        Paragraph::new(Line::styled(note, Style::default().fg(Color::DarkGray)))
    } else {
        Paragraph::new(visible)
    };
    if app.selecting {
        f.render_widget(body, area);
        return;
    }
    f.render_widget(
        body.block(
            Block::default()
                .title(title)
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan)),
        ),
        area,
    );

    if total > height {
        scrollbar(f, area, most, start, height);
    }
}

fn log_style(line: &str) -> Style {
    // Not `[stderr]`: tools log progress and warnings there as a matter of
    // course, and a step's output is no more an error for having used it. The
    // step's outcome says whether something went wrong.
    if line.starts_with('✗') || line.contains("error") || line.contains("Error") {
        Style::default().fg(Color::Red)
    } else if line.starts_with("[dry-run]") || line.starts_with('⚑') || line.starts_with('⚠') {
        // ⚑ marks a custom argument from `...`: for this run only.
        Style::default().fg(Color::Yellow)
    } else if line.starts_with('▶') || line.starts_with('✓') {
        Style::default().fg(Color::Green)
    } else if line.starts_with('+') || line.starts_with('$') {
        Style::default().fg(Color::Cyan)
    } else if line.starts_with("cache:") || line.starts_with('⊘') {
        Style::default().fg(Color::DarkGray)
    } else {
        Style::default().fg(Color::Gray)
    }
}

/// A line carrying SGR colour escapes, as styled rows no wider than `width`.
///
/// Understands what the env report writes — the eight colours and their
/// bright forms, bold, dim, and the resets — and drops any other escape rather
/// than drawing it, since an escape on screen is garbage and also throws the
/// wrapping off. Dim is drawn dark grey, as the TUI draws everything faint.
pub(super) fn ansi_rows(line: &str, width: usize) -> Vec<Line<'static>> {
    let plain = Style::default().fg(Color::Gray);
    let mut style = plain;
    let mut cells: Vec<(char, Style)> = Vec::new();
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\x1b' {
            cells.push((c, style));
            continue;
        }
        if chars.peek() != Some(&'[') {
            continue;
        }
        chars.next();
        let mut params = String::new();
        let mut end = None;
        for c in chars.by_ref() {
            if c.is_ascii_digit() || c == ';' {
                params.push(c);
            } else {
                end = Some(c);
                break;
            }
        }
        if end != Some('m') {
            continue;
        }
        let codes: Vec<u16> = params.split(';').map(|p| p.parse().unwrap_or(0)).collect();
        let mut codes = codes.into_iter();
        while let Some(code) = codes.next() {
            style = match code {
                0 => plain,
                1 => style.add_modifier(Modifier::BOLD),
                2 => style.fg(Color::DarkGray),
                22 => style
                    .remove_modifier(Modifier::BOLD)
                    .fg(plain.fg.unwrap_or(Color::Gray)),
                30..=37 => style.fg(ansi_colour(code - 30, false)),
                90..=97 => style.fg(ansi_colour(code - 90, true)),
                39 => style.fg(Color::Gray),
                // 256-colour and truecolour forms: skip their arguments.
                38 => {
                    match codes.next() {
                        Some(5) => {
                            codes.next();
                        }
                        Some(2) => {
                            codes.nth(2);
                        }
                        _ => {}
                    }
                    style
                }
                _ => style,
            };
        }
    }

    let chunks: Vec<&[(char, Style)]> = match cells.is_empty() {
        true => vec![&[]],
        false => cells.chunks(width.max(1)).collect(),
    };
    chunks
        .into_iter()
        .map(|chunk| {
            let mut spans: Vec<Span<'static>> = Vec::new();
            let mut run = String::new();
            let mut current = plain;
            for &(c, s) in chunk {
                if s != current && !run.is_empty() {
                    spans.push(Span::styled(std::mem::take(&mut run), current));
                }
                current = s;
                run.push(c);
            }
            if !run.is_empty() {
                spans.push(Span::styled(run, current));
            }
            Line::from(spans)
        })
        .collect()
}

fn ansi_colour(index: u16, bright: bool) -> Color {
    match (index, bright) {
        (0, false) => Color::Black,
        (1, false) => Color::Red,
        (2, false) => Color::Green,
        (3, false) => Color::Yellow,
        (4, false) => Color::Blue,
        (5, false) => Color::Magenta,
        (6, false) => Color::Cyan,
        (7, false) => Color::Gray,
        (0, true) => Color::DarkGray,
        (1, true) => Color::LightRed,
        (2, true) => Color::LightGreen,
        (3, true) => Color::LightYellow,
        (4, true) => Color::LightBlue,
        (5, true) => Color::LightMagenta,
        (6, true) => Color::LightCyan,
        _ => Color::White,
    }
}

/// Split `line` into segments no wider than `width` characters. Returns at
/// least one segment (possibly empty) so blank log lines are preserved.
fn wrap_line(line: &str, width: usize) -> Vec<String> {
    if width == 0 || line.chars().count() <= width {
        return vec![line.to_string()];
    }
    let chars: Vec<char> = line.chars().collect();
    chars
        .chunks(width)
        .map(|chunk| chunk.iter().collect())
        .collect()
}

fn render_help(f: &mut Frame, area: Rect, app: &App) {
    let state = if app.stopping && !app.finished {
        "Stopping the run — [q] again to leave now.  "
    } else if app.finished && !app.succeeded() {
        "Finished with failures — [q] to close.  "
    } else if app.finished && !app.browsing {
        "All done — closing shortly, any key to stay.  "
    } else if app.finished {
        "All done — [q] to close.  "
    } else {
        ""
    };
    let help = match (app.prompts.is_empty(), app.pane) {
        (false, _) => format!(" {state}Answering a question · Esc stops the run"),
        (true, _) if app.selecting => {
            " Selecting — drag to select, then copy as usual · ↑↓ scroll · c back".to_string()
        }
        (true, Pane::Env) => {
            format!(" {state}e back to output · wheel scrolls · c select text · q quit")
        }
        (true, Pane::Output) => format!(
            " {state}↑↓ step · wheel scrolls · c select text · f follow · s sort · \
             n next failure · e .env · q quit"
        ),
    };
    f.render_widget(
        Paragraph::new(help).style(Style::default().fg(Color::DarkGray)),
        area,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_names_are_cut_with_an_ellipsis() {
        assert_eq!(truncate("compile", 10), "compile");
        assert_eq!(truncate("packages/api#compile", 8), "package…");
        assert_eq!(truncate("x", 0), "");
    }

    #[test]
    fn the_list_fits_its_names_within_bounds() {
        let app = App::new("build", ["a"], false);
        assert_eq!(list_width(&app, 200), LIST_MIN);
        let long = "x".repeat(200);
        let app = App::new("build", [long.as_str()], false);
        assert_eq!(list_width(&app, 200), 80);
    }

    #[test]
    fn the_screen_draws_at_any_size() {
        use ratatui::{Terminal, backend::TestBackend};
        let mut app = App::new("build", ["compile", "test"], true);
        app.banner = Some("ciabatta 0.7.6, remote cache 0.7.0".into());
        app.apply_update(crate::runner::ProgressUpdate::StepNeedsChoice {
            workflow: "build".into(),
            step: "test".into(),
            message: "tests failed, and this message is long enough to wrap".into(),
            options: vec!["retry".into(), "skip".into()],
        });
        for (w, h) in [(120, 40), (40, 10), (10, 4)] {
            let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
            terminal.draw(|f| render(f, &mut app)).unwrap();
        }
    }

    #[test]
    fn the_env_report_keeps_its_colours_without_its_escapes() {
        // A key and a faint note, as the env report writes its rows.
        let line = format!(
            "{}  {}",
            owo_colors::OwoColorize::style(&"GREETING", owo_colors::Style::new().green().bold()),
            owo_colors::OwoColorize::style(&"[env]", owo_colors::Style::new().dimmed()),
        );
        let rows = ansi_rows(&line, 80);
        assert_eq!(rows.len(), 1);
        let text: String = rows[0].spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "GREETING  [env]", "no escape survives");
        let key = &rows[0].spans[0];
        assert_eq!(key.content, "GREETING");
        assert_eq!(key.style.fg, Some(Color::Green));
        assert!(key.style.add_modifier.contains(Modifier::BOLD));
        let note = rows[0].spans.last().unwrap();
        assert_eq!(note.content, "[env]");
        assert_eq!(note.style.fg, Some(Color::DarkGray));
    }

    #[test]
    fn coloured_lines_wrap_by_what_is_visible() {
        let line = format!("\x1b[31m{}\x1b[0m", "x".repeat(25));
        let rows = ansi_rows(&line, 10);
        assert_eq!(rows.len(), 3, "25 visible characters, 10 to a row");
        assert!(rows.iter().all(|r| r.spans[0].style.fg == Some(Color::Red)));
        assert_eq!(ansi_rows("", 10).len(), 1, "a blank line stays a row");
    }

    #[test]
    fn selecting_draws_the_text_alone() {
        use ratatui::{Terminal, backend::TestBackend};
        let mut app = App::new("build", ["compile"], false);
        app.run.logs.push("copy me".into());
        app.selecting = true;
        let mut terminal = Terminal::new(TestBackend::new(60, 8)).unwrap();
        terminal.draw(|f| render(f, &mut app)).unwrap();
        let screen: Vec<String> = (0..8)
            .map(|y| {
                (0..60)
                    .map(|x| terminal.backend().buffer()[(x, y)].symbol())
                    .collect()
            })
            .collect();
        // The line starts the row, with no border or step list beside it.
        assert!(
            screen.iter().any(|row| row.trim_end() == "copy me"),
            "{screen:#?}"
        );
        assert!(!screen.concat().contains('│'), "{screen:#?}");
    }
}
