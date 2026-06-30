use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Gauge, List, ListItem, ListState, Paragraph},
};

use super::app::{App, RecipeStatus, StageStatus};

const LOGO: &str = r#"
  ██████╗██╗ █████╗ ██████╗  █████╗ ████████╗████████╗ █████╗
 ██╔════╝██║██╔══██╗██╔══██╗██╔══██╗╚══██╔══╝╚══██╔══╝██╔══██╗
 ██║     ██║███████║██████╔╝███████║   ██║      ██║   ███████║
 ██║     ██║██╔══██║██╔══██╗██╔══██║   ██║      ██║   ██╔══██║
 ╚██████╗██║██║  ██║██████╔╝██║  ██║   ██║      ██║   ██║  ██║
  ╚═════╝╚═╝╚═╝  ╚═╝╚═════╝ ╚═╝  ╚═╝  ╚═╝      ╚═╝   ╚═╝  ╚═╝
         Artifact Publishing Made Easy  🍞
"#;

pub fn render(f: &mut Frame, app: &App) {
    let area = f.area();

    let logo_lines = LOGO.lines().count() as u16 + 1;

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(logo_lines), // logo
            Constraint::Min(8),             // recipe list + logs
            Constraint::Length(1),          // help bar
        ])
        .split(area);

    render_logo(f, chunks[0], app);
    render_body(f, chunks[1], app);
    render_help(f, chunks[2], app);
}

fn render_logo(f: &mut Frame, area: Rect, _app: &App) {
    let logo = Paragraph::new(LOGO).style(
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
    );
    f.render_widget(logo, area);
}

fn render_body(f: &mut Frame, area: Rect, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(40), Constraint::Percentage(60)])
        .split(area);

    render_recipe_list(f, chunks[0], app);
    render_logs(f, chunks[1], app);
}

fn render_recipe_list(f: &mut Frame, area: Rect, app: &App) {
    let block = Block::default()
        .title(" Recipes ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan));

    let inner = block.inner(area);
    f.render_widget(block, area);

    // Each recipe takes 3 rows: name + status, progress bar, blank
    let items_height = 3u16;
    let visible = (inner.height / items_height) as usize;
    let start = app.selected.saturating_sub(visible.saturating_sub(1));

    let mut y = inner.y;
    for (i, recipe) in app.recipes.iter().enumerate().skip(start) {
        if y + items_height > inner.y + inner.height {
            break;
        }

        let selected = i == app.selected;
        let (status_symbol, status_color) = status_style(&recipe.status);

        let name_style = if selected {
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD | Modifier::REVERSED)
        } else {
            Style::default().fg(Color::White)
        };

        let title_line = Line::from(vec![
            Span::styled(
                format!(" {} ", status_symbol),
                Style::default().fg(status_color),
            ),
            Span::styled(&recipe.name, name_style),
        ]);
        let title = Paragraph::new(title_line);
        f.render_widget(
            title,
            Rect {
                x: inner.x,
                y,
                width: inner.width,
                height: 1,
            },
        );

        // Stage strip: login · pre · push · post, each with a status symbol.
        let labels = app.stage_labels();
        let mut spans: Vec<Span> = Vec::new();
        for (idx, label) in labels.iter().enumerate() {
            let (sym, color) = stage_style(recipe.stages[idx]);
            if idx > 0 {
                spans.push(Span::raw(" "));
            }
            spans.push(Span::styled(
                format!("{sym}{label}"),
                Style::default().fg(color),
            ));
        }
        let strip = Paragraph::new(Line::from(spans));
        f.render_widget(
            strip,
            Rect {
                x: inner.x + 2,
                y: y + 1,
                width: inner.width.saturating_sub(2),
                height: 1,
            },
        );

        let gauge_color = gauge_color_for(&recipe.status);
        let gauge = Gauge::default()
            .gauge_style(Style::default().fg(gauge_color).bg(Color::DarkGray))
            .ratio(recipe.progress());
        f.render_widget(
            gauge,
            Rect {
                x: inner.x + 2,
                y: y + 2,
                width: inner.width.saturating_sub(2),
                height: 1,
            },
        );

        y += items_height;
    }

    // Scrollbar indicator
    if app.recipes.len() > visible {
        let indicator = format!(" {}/{} ", app.selected + 1, app.recipes.len());
        let x = inner.x + inner.width.saturating_sub(indicator.len() as u16);
        let p = Paragraph::new(indicator).style(Style::default().fg(Color::DarkGray));
        f.render_widget(
            p,
            Rect {
                x,
                y: inner.y,
                width: inner.width,
                height: 1,
            },
        );
    }
}

fn render_logs(f: &mut Frame, area: Rect, app: &App) {
    let title = if let Some(r) = app.recipes.get(app.selected) {
        format!(" Logs: {} ", r.name)
    } else {
        " Logs ".to_string()
    };

    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan));

    let logs = app.selected_logs();
    let items: Vec<ListItem> = logs
        .iter()
        .rev()
        .take(area.height as usize)
        .rev()
        .map(|l| {
            let style = if l.starts_with("[stderr]")
                || l.starts_with("✗")
                || l.contains("error")
                || l.contains("Error")
            {
                Style::default().fg(Color::Red)
            } else if l.starts_with("[dry-run]") {
                Style::default().fg(Color::Yellow)
            } else if l.starts_with('+') || l.starts_with('$') {
                Style::default().fg(Color::Cyan)
            } else {
                Style::default().fg(Color::Gray)
            };
            ListItem::new(l.as_str()).style(style)
        })
        .collect();

    let list = List::new(items).block(block);

    let mut state = ListState::default();
    if !logs.is_empty() {
        state.select(Some(logs.len() - 1));
    }
    f.render_stateful_widget(list, area, &mut state);
}

fn render_help(f: &mut Frame, area: Rect, app: &App) {
    let mode = if app.dry_run { " DRY-RUN " } else { "" };
    let status = if app.all_done {
        "All done! "
    } else {
        "Running... "
    };

    let help = format!("{}{}  [↑/↓] select  [q] quit", mode, status);
    let p = Paragraph::new(help).style(Style::default().fg(Color::DarkGray));
    f.render_widget(p, area);
}

fn status_style(status: &RecipeStatus) -> (&'static str, Color) {
    match status {
        RecipeStatus::Pending => ("○", Color::DarkGray),
        RecipeStatus::Running => ("◑", Color::Yellow),
        RecipeStatus::Success => ("✓", Color::Green),
        RecipeStatus::Failed(_) => ("✗", Color::Red),
    }
}

fn stage_style(status: StageStatus) -> (&'static str, Color) {
    match status {
        StageStatus::Pending => ("○", Color::DarkGray),
        StageStatus::Running => ("◑", Color::Yellow),
        StageStatus::Done => ("✓", Color::Green),
        StageStatus::Skipped => ("·", Color::DarkGray),
        StageStatus::Failed => ("✗", Color::Red),
    }
}

fn gauge_color_for(status: &RecipeStatus) -> Color {
    match status {
        RecipeStatus::Pending => Color::DarkGray,
        RecipeStatus::Running => Color::Yellow,
        RecipeStatus::Success => Color::Green,
        RecipeStatus::Failed(_) => Color::Red,
    }
}
