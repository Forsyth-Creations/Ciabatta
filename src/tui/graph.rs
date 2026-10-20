//! An interactive view of a resolved graph, for `--graph`.
//!
//! Printing the graph answers "what runs?" only as fast as you can scroll. A
//! real monorepo build is a hundred nodes across a dozen packages, and the
//! questions people actually have about it are per-node: *what does this step
//! do, who owns it, what is it waiting for, why is it even in my graph?*
//!
//! ```text
//! ┌─ build + test — 14 steps, 5 sub-workspaces ─────────────────────────┐
//! │ wave 1                            │  api:compile                    │
//! │   proto:protoc                    │  Compile the service binary     │
//! │ wave 2                            │                                 │
//! │ ▸ api:compile                     │  from     api (packages/api)    │
//! │   web:bundle                      │  owner    Ada                   │
//! │ wave 3                            │  runs     cargo build --release │
//! │   api:package                     │  after    proto:protoc          │
//! └───────────────────────────────────┴─────────────────────────────────┘
//! ```
//!
//! Nothing here executes anything — it's the same compiled graph the run would
//! have taken, shown before you commit to it.
//!
//! A long graph is a lot of waves to scroll through, so a wave folds: `←` on
//! one of its steps folds it to its heading, `→` (or Enter) on a folded heading
//! opens it again, and `z` folds or opens every wave at once.

use std::collections::BTreeSet;
use std::io;
use std::time::Duration;

use anyhow::Result;
use crossterm::{
    event::{Event, EventStream, KeyCode, KeyModifiers},
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
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap},
};

use crate::run::RunStep;
use crate::run::filter::Outcome;
use crate::workspace::Workspace;
use crate::workspace::graph::WorkflowGraph;

/// One row of the left-hand list: either a wave heading or a step under it.
enum Row<'a> {
    Wave {
        index: usize,
        count: usize,
        /// Folded to its heading: its steps aren't listed under it.
        folded: bool,
    },
    Step(&'a RunStep),
    /// The heading above the recovery nodes, which belong to no wave.
    RecoveryHeading,
}

impl Row<'_> {
    fn step(&self) -> Option<&RunStep> {
        match self {
            Row::Step(step) => Some(step),
            _ => None,
        }
    }

    /// Whether the cursor can land here: a step, or a folded wave's heading —
    /// the one thing left of the wave to open it from. An open wave's heading
    /// is scenery.
    fn selectable(&self) -> bool {
        matches!(self, Row::Step(_) | Row::Wave { folded: true, .. })
    }
}

/// Open the graph viewer and block until the user closes it.
///
/// Takes over the alternate screen the same way a run's TUI does, and restores
/// the terminal on every exit path — including the error one, since leaving a
/// terminal in raw mode is a far worse failure than whatever caused it.
pub async fn explore(workspace: &Workspace, graph: &WorkflowGraph, pruned: &Outcome) -> Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = view_loop(&mut terminal, workspace, graph, pruned).await;

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    result
}

async fn view_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    workspace: &Workspace,
    graph: &WorkflowGraph,
    pruned: &Outcome,
) -> Result<()> {
    let mut folded: BTreeSet<usize> = BTreeSet::new();
    let mut rows = rows(graph, &folded);
    // Start on the first real step rather than the wave heading above it.
    let mut selected = rows.iter().position(Row::selectable).unwrap_or(0);
    let mut state = ListState::default();
    let mut events = EventStream::new();

    loop {
        state.select(Some(selected));
        terminal.draw(|frame| draw(frame, workspace, graph, pruned, &rows, &mut state))?;

        // A poll rather than a bare `next()`: without it a resize between
        // frames wouldn't redraw until the next keypress.
        let Ok(Some(Ok(event))) =
            tokio::time::timeout(Duration::from_millis(250), events.next()).await
        else {
            continue;
        };
        let Event::Key(key) = event else { continue };
        if key.kind != crossterm::event::KeyEventKind::Press {
            continue;
        }

        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => return Ok(()),
            KeyCode::Down | KeyCode::Char('j') => selected = next_step(&rows, selected, 1),
            KeyCode::Up | KeyCode::Char('k') => selected = next_step(&rows, selected, -1),
            KeyCode::Home | KeyCode::Char('g') => {
                selected = rows.iter().position(Row::selectable).unwrap_or(0)
            }
            KeyCode::End | KeyCode::Char('G') => {
                selected = rows.iter().rposition(Row::selectable).unwrap_or(0)
            }
            KeyCode::Left | KeyCode::Char('h') => {
                if let Some(wave) = wave_of(&rows, selected) {
                    folded.insert(wave);
                    rows = self::rows(graph, &folded);
                    selected = heading_of(&rows, wave);
                }
            }
            KeyCode::Right | KeyCode::Char('l') | KeyCode::Enter => {
                if let Some(Row::Wave {
                    index,
                    folded: true,
                    ..
                }) = rows.get(selected)
                {
                    let wave = *index;
                    folded.remove(&wave);
                    rows = self::rows(graph, &folded);
                    selected = next_step(&rows, heading_of(&rows, wave), 1);
                }
            }
            KeyCode::Char('z') => {
                let wave = wave_of(&rows, selected);
                let waves = graph.waves().len();
                folded = match folded.len() == waves {
                    true => BTreeSet::new(),
                    false => (0..waves).collect(),
                };
                rows = self::rows(graph, &folded);
                selected = match wave {
                    Some(wave) if folded.contains(&wave) => heading_of(&rows, wave),
                    Some(wave) => next_step(&rows, heading_of(&rows, wave), 1),
                    None => selected.min(rows.len().saturating_sub(1)),
                };
            }
            _ => {}
        }
    }
}

/// Flatten the graph into display rows: each wave, its steps, then the recovery
/// nodes that hang off them.
fn rows<'a>(graph: &'a WorkflowGraph, folded: &BTreeSet<usize>) -> Vec<Row<'a>> {
    let mut rows: Vec<Row> = Vec::new();
    for (index, wave) in graph.waves().iter().enumerate() {
        let folded = folded.contains(&index);
        rows.push(Row::Wave {
            index,
            count: wave.len(),
            folded,
        });
        if !folded {
            rows.extend(wave.iter().map(|step| Row::Step(step)));
        }
    }

    let recoveries: Vec<&RunStep> = graph.steps.iter().filter(|s| s.recover).collect();
    if !recoveries.is_empty() {
        rows.push(Row::RecoveryHeading);
        rows.extend(recoveries.into_iter().map(Row::Step));
    }
    rows
}

/// Move the selection by `delta` rows, skipping open waves' headings so the
/// cursor only ever lands on something with a detail pane to show.
fn next_step(rows: &[Row], from: usize, delta: isize) -> usize {
    let mut index = from as isize;
    loop {
        index += delta;
        if index < 0 || index as usize >= rows.len() {
            return from;
        }
        if rows[index as usize].selectable() {
            return index as usize;
        }
    }
}

/// The wave the row at `at` belongs to: its own, for a heading, or the one
/// whose heading is above it. `None` for the recovery nodes, which belong to
/// no wave.
fn wave_of(rows: &[Row], at: usize) -> Option<usize> {
    rows.get(..=at)?.iter().rev().find_map(|row| match row {
        Row::Wave { index, .. } => Some(Some(*index)),
        Row::RecoveryHeading => Some(None),
        Row::Step(_) => None,
    })?
}

/// Where wave `wave`'s heading is.
fn heading_of(rows: &[Row], wave: usize) -> usize {
    rows.iter()
        .position(|row| matches!(row, Row::Wave { index, .. } if *index == wave))
        .unwrap_or(0)
}

fn draw(
    frame: &mut Frame,
    workspace: &Workspace,
    graph: &WorkflowGraph,
    pruned: &Outcome,
    rows: &[Row],
    state: &mut ListState,
) {
    let chunks = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(3),
        Constraint::Length(1),
    ])
    .split(frame.area());

    header(frame, chunks[0], workspace, graph, pruned);

    let panes = Layout::horizontal([Constraint::Percentage(45), Constraint::Percentage(55)])
        .split(chunks[1]);
    step_list(frame, panes[0], rows, state);

    match state.selected().and_then(|at| rows.get(at)) {
        Some(Row::Wave {
            index,
            folded: true,
            ..
        }) => folded_detail(frame, panes[1], graph, *index),
        _ => detail(frame, panes[1], state.and_then_selected(rows), graph),
    }

    let hint = "  ↑/↓ move   ←/→ fold/open a wave   z fold all   g/G first/last   q quit — nothing here runs anything";
    frame.render_widget(
        Paragraph::new(hint).style(Style::default().fg(Color::DarkGray)),
        chunks[2],
    );
}

/// A one-line summary of what this graph is.
fn header(
    frame: &mut Frame,
    area: Rect,
    workspace: &Workspace,
    graph: &WorkflowGraph,
    pruned: &Outcome,
) {
    let members = {
        let mut names: Vec<&str> = graph
            .steps
            .iter()
            .filter_map(|s| s.workspace.as_deref())
            .collect();
        names.sort();
        names.dedup();
        names.len()
    };

    let mut spans = vec![
        Span::styled(
            format!(" {} ", graph.label()),
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(format!(
            "  {} step(s) across {} sub-workspace(s), in {} wave(s)  ",
            graph.steps.iter().filter(|s| !s.recover).count(),
            members,
            graph.waves().len(),
        )),
    ];
    // A filtered graph must never look like the whole one.
    if !pruned.dropped.is_empty() {
        spans.push(Span::styled(
            format!("filtered: {} step(s) pruned ", pruned.dropped.len()),
            Style::default().fg(Color::Yellow),
        ));
    }
    spans.push(Span::styled(
        workspace.root.display().to_string(),
        Style::default().fg(Color::DarkGray),
    ));
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn step_list(frame: &mut Frame, area: Rect, rows: &[Row], state: &mut ListState) {
    let items: Vec<ListItem> = rows
        .iter()
        .map(|row| match row {
            Row::Wave {
                index,
                count,
                folded,
            } => ListItem::new(Line::from(Span::styled(
                format!(
                    " {} wave {} — {count} in parallel",
                    if *folded { "▸" } else { "▾" },
                    index + 1
                ),
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::BOLD),
            ))),
            Row::RecoveryHeading => ListItem::new(Line::from(Span::styled(
                " recovery — entered only on failure",
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::BOLD),
            ))),
            Row::Step(step) => {
                let mut spans = vec![Span::raw("   "), Span::raw(step.name.clone())];
                if step.is_push() {
                    spans.push(Span::styled(
                        "  ⇧ push",
                        Style::default().fg(Color::Magenta),
                    ));
                }
                if step.persistent {
                    spans.push(Span::styled(
                        "  persistent",
                        Style::default().fg(Color::Blue),
                    ));
                }
                ListItem::new(Line::from(spans))
            }
        })
        .collect();

    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title(" steps "))
        .highlight_style(
            Style::default()
                .bg(Color::Cyan)
                .fg(Color::Black)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▸ ");
    frame.render_stateful_widget(list, area, state);
}

/// The right-hand pane for a folded wave: the steps it is hiding.
fn folded_detail(frame: &mut Frame, area: Rect, graph: &WorkflowGraph, wave: usize) {
    let waves = graph.waves();
    let mut lines = vec![
        Line::from(Span::styled(
            format!("wave {} — folded", wave + 1),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            "→ or Enter to open it",
            Style::default().fg(Color::DarkGray),
        )),
        Line::raw(""),
    ];
    lines.extend(
        waves
            .get(wave)
            .into_iter()
            .flatten()
            .map(|step| Line::raw(format!("  {}", step.name))),
    );
    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .block(Block::default().borders(Borders::ALL).title(" detail ")),
        area,
    );
}

/// The right-hand pane: everything known about the selected step.
fn detail(frame: &mut Frame, area: Rect, step: Option<&RunStep>, graph: &WorkflowGraph) {
    let Some(step) = step else {
        frame.render_widget(
            Paragraph::new("This graph has no steps.")
                .block(Block::default().borders(Borders::ALL).title(" detail ")),
            area,
        );
        return;
    };

    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(Span::styled(
        step.name.clone(),
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::from(Span::styled(
        step.description
            .clone()
            .unwrap_or_else(|| "(no description — add one, everyone else has to guess)".into()),
        Style::default().fg(if step.description.is_some() {
            Color::Reset
        } else {
            Color::Yellow
        }),
    )));
    lines.push(Line::raw(""));

    let mut field = |label: &str, value: String| {
        lines.push(Line::from(vec![
            Span::styled(format!("{label:<10}"), Style::default().fg(Color::DarkGray)),
            Span::raw(value),
        ]));
    };

    field(
        "from",
        format!(
            "{} ({})",
            step.workspace.as_deref().unwrap_or("—"),
            step.cwd.as_deref().unwrap_or(".")
        ),
    );
    field(
        "owner",
        step.owner
            .clone()
            .unwrap_or_else(|| "unowned — nobody to ask".into()),
    );
    if let Some(kind) = step.kind.as_deref() {
        field("phase", kind.to_string());
    }
    if !step.tags.is_empty() {
        field("tags", step.tags.join(", "));
    }

    // What it actually does, which is the reason anyone opened this pane.
    let (script, run) = step.action();
    if let Some(script) = script {
        field("script", script.to_string());
    }
    if let Some(command) = run {
        field("runs", command);
    }
    if step.recover {
        field(
            "options",
            step.options
                .iter()
                .map(|o| o.label.as_str())
                .collect::<Vec<_>>()
                .join(" | "),
        );
    }

    if !step.needs.is_empty() {
        field("after", step.needs.join(", "));
    }
    // The inverse edge is not stored on the step, so it's derived — "what is
    // waiting on this?" is exactly what you want to know before skipping it.
    let dependents: Vec<&str> = graph
        .steps
        .iter()
        .filter(|s| s.needs.iter().any(|n| n == &step.name))
        .map(|s| s.name.as_str())
        .collect();
    if !dependents.is_empty() {
        field("blocks", dependents.join(", "));
    }
    if !step.requires.is_empty() {
        field("tools", step.requires.join(", "));
    }
    if let Some(target) = step.on_error.as_deref() {
        field("on error", format!("→ {target}"));
    }
    if let Some(target) = step.retry.as_deref() {
        field("then retry", format!("→ {target}"));
    }
    if let Some(timeout) = step.timeout.as_deref() {
        field("timeout", timeout.to_string());
    }
    if step.retries > 0 {
        field("retries", step.retries.to_string());
    }
    if step.persistent {
        field("persistent", "keeps running; the graph won't wait".into());
    }
    if step.continue_on_error {
        field("on failure", "the rest of the graph carries on".into());
    }
    if step.warn_on_error {
        field("on failure", "a warning; dependents still run".into());
    }
    if !step.when.is_empty() {
        field("when", step.when.join(" and "));
    }
    if !step.skip_if.is_empty() {
        field("skip if", step.skip_if.join(" or "));
    }
    if !step.env.is_empty() {
        field(
            "env",
            step.env
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>()
                .join(", "),
        );
    }

    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .block(Block::default().borders(Borders::ALL).title(" detail ")),
        area,
    );
}

/// Resolve the selected row to the step it holds, if any.
trait SelectedStep {
    fn and_then_selected<'a>(&self, rows: &'a [Row]) -> Option<&'a RunStep>;
}

impl SelectedStep for ListState {
    fn and_then_selected<'a>(&self, rows: &'a [Row]) -> Option<&'a RunStep> {
        rows.get(self.selected()?)?.step()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn graph() -> WorkflowGraph {
        WorkflowGraph {
            workflows: vec!["build".into()],
            steps: vec![
                RunStep {
                    name: "proto:gen".into(),
                    run: Some("protoc".into()),
                    ..Default::default()
                },
                RunStep {
                    name: "api:compile".into(),
                    run: Some("cargo build".into()),
                    needs: vec!["proto:gen".into()],
                    on_error: Some("api:fix".into()),
                    ..Default::default()
                },
                RunStep {
                    name: "api:fix".into(),
                    recover: true,
                    ..Default::default()
                },
            ],
            ..Default::default()
        }
    }

    #[test]
    fn rows_interleave_wave_headings_with_their_steps() {
        let graph = graph();
        let rows = rows(&graph, &BTreeSet::new());
        // wave 1, proto:gen, wave 2, api:compile, recovery heading, api:fix
        assert_eq!(rows.len(), 6);
        assert!(matches!(
            rows[0],
            Row::Wave {
                index: 0,
                count: 1,
                folded: false
            }
        ));
        assert_eq!(rows[1].step().unwrap().name, "proto:gen");
        assert!(matches!(rows[2], Row::Wave { index: 1, .. }));
        assert!(matches!(rows[4], Row::RecoveryHeading));
        assert_eq!(rows[5].step().unwrap().name, "api:fix");
    }

    #[test]
    fn navigation_skips_headings_and_stops_at_the_ends() {
        let graph = graph();
        let rows = rows(&graph, &BTreeSet::new());

        // Down from the first step lands on the next one, not the heading.
        assert_eq!(next_step(&rows, 1, 1), 3);
        assert_eq!(next_step(&rows, 3, 1), 5);
        // Past the last step, the selection holds rather than falling off.
        assert_eq!(next_step(&rows, 5, 1), 5);
        // And the same going up.
        assert_eq!(next_step(&rows, 3, -1), 1);
        assert_eq!(next_step(&rows, 1, -1), 1);
    }

    /// A folded wave lists no steps, and its heading becomes somewhere the
    /// cursor can stop — the place to open it again from.
    #[test]
    fn a_folded_wave_hides_its_steps_behind_a_selectable_heading() {
        let graph = graph();
        let rows = rows(&graph, &BTreeSet::from([0]));
        // wave 1 (folded), wave 2, api:compile, recovery heading, api:fix
        assert_eq!(rows.len(), 5);
        assert!(rows[0].selectable(), "the folded heading can be selected");
        assert!(!rows[1].selectable(), "an open heading is scenery");
        assert_eq!(next_step(&rows, 2, -1), 0);
        assert_eq!(wave_of(&rows, 2), Some(1));
        assert_eq!(wave_of(&rows, 0), Some(0));
        assert_eq!(wave_of(&rows, 4), None, "recovery nodes belong to no wave");
        assert_eq!(heading_of(&rows, 1), 1);
    }
}
