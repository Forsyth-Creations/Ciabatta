pub mod app;
pub mod browser;
pub mod graph;
pub mod ui;

use std::collections::HashMap;
use std::io;
use std::time::Duration;

use anyhow::Result;
use crossterm::{
    event::{Event, EventStream, KeyCode, KeyModifiers},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use futures::StreamExt;
use ratatui::{Terminal, backend::CrosstermBackend};
use tokio::sync::mpsc;

use crate::config::CiabattaConfig;
use crate::runner::{self, ProgressUpdate};
use app::App;

#[allow(clippy::too_many_arguments)]
pub async fn run(
    name: &str,
    resolved: &crate::run::ResolvedRun,
    config: &CiabattaConfig,
    root: &std::path::Path,
    env_vars: &HashMap<String, String>,
    dry_run: bool,
    authoritative: bool,
    sandbox_also: &[String],
) -> Result<bool> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = tui_loop(
        &mut terminal,
        name,
        resolved,
        config,
        root,
        env_vars,
        dry_run,
        authoritative,
        sandbox_also,
    )
    .await;

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    result
}

#[allow(clippy::too_many_arguments)]
async fn tui_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    name: &str,
    resolved: &crate::run::ResolvedRun,
    config: &CiabattaConfig,
    root: &std::path::Path,
    env_vars: &HashMap<String, String>,
    dry_run: bool,
    authoritative: bool,
    sandbox_also: &[String],
) -> Result<bool> {
    let mut app = App::new(name, dry_run);
    let (tx, mut rx) = mpsc::channel::<ProgressUpdate>(256);

    let name_clone = name.to_string();
    let resolved_clone = resolved.clone();
    let config_clone = config.clone();
    let root_clone = root.to_path_buf();
    let vars_clone = env_vars.clone();
    let tx_clone = tx.clone();
    let sandbox_also = sandbox_also.to_vec();

    tokio::spawn(async move {
        let _ = runner::run_workflow_ctl(
            &name_clone,
            &resolved_clone,
            &config_clone,
            &root_clone,
            &vars_clone,
            dry_run,
            runner::RunCtl {
                authoritative,
                sandbox_also,
                ..Default::default()
            },
            tx_clone,
        )
        .await;
        // tx dropped here → rx.recv() returns None → signals completion
    });
    drop(tx);

    let mut event_stream = EventStream::new();
    let done_linger = Duration::from_secs(10);
    let mut done_at: Option<tokio::time::Instant> = None;

    loop {
        terminal.draw(|f| ui::render(f, &app))?;

        // When all workflows finish successfully, keep the results on screen for
        // `done_linger` so they can be read, then exit automatically (a keypress
        // quits sooner). If any workflow failed, stay open so the errors remain
        // visible until the user quits with a keypress.
        if app.all_done && !app.any_failed() && done_at.is_none() {
            done_at = Some(tokio::time::Instant::now());
        }
        if let Some(t) = done_at
            && t.elapsed() >= done_linger
        {
            break;
        }

        let sleep = tokio::time::sleep(Duration::from_millis(50));

        tokio::select! {
            maybe_event = event_stream.next() => {
                match maybe_event {
                    Some(Ok(Event::Key(key))) => {
                        let quit = key.code == KeyCode::Char('q')
                            || key.code == KeyCode::Esc
                            || (key.code == KeyCode::Char('c')
                                && key.modifiers.contains(KeyModifiers::CONTROL));
                        if quit {
                            break;
                        }
                        match key.code {
                            KeyCode::Down | KeyCode::Char('j') => app.select_next(),
                            KeyCode::Up | KeyCode::Char('k') => app.select_prev(),
                            _ => {}
                        }
                    }
                    Some(Err(e)) => return Err(e.into()),
                    None => break,
                    _ => {}
                }
            }
            maybe_update = rx.recv() => {
                match maybe_update {
                    Some(update) => app.apply_update(update),
                    None => {
                        // All senders dropped; give UI a final render cycle.
                        app.all_done = app.workflows.iter().all(|r| r.status.is_terminal());
                        terminal.draw(|f| ui::render(f, &app))?;
                        // Only auto-close on success; on failure wait for a keypress.
                        if done_at.is_none() && !app.any_failed() {
                            done_at = Some(tokio::time::Instant::now());
                        }
                    }
                }
            }
            _ = sleep => {}
        }
    }

    let success = app
        .workflows
        .iter()
        .all(|r| matches!(r.status, crate::tui::app::WorkflowStatus::Success));
    Ok(success)
}
