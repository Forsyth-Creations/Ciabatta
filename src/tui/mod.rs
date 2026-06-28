pub mod app;
pub mod browser;
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
use crate::runner::{self, ProgressUpdate, RunMode};
use app::App;

pub async fn run(
    config: &CiabattaConfig,
    root: &std::path::Path,
    recipe_names: &[String],
    env_vars: &HashMap<String, String>,
    dry_run: bool,
    mode: RunMode,
) -> Result<bool> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = tui_loop(&mut terminal, config, root, recipe_names, env_vars, dry_run, mode).await;

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    result
}

async fn tui_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    config: &CiabattaConfig,
    root: &std::path::Path,
    recipe_names: &[String],
    env_vars: &HashMap<String, String>,
    dry_run: bool,
    mode: RunMode,
) -> Result<bool> {
    let mut app = App::new(recipe_names, dry_run);
    let (tx, mut rx) = mpsc::channel::<ProgressUpdate>(256);

    // Spawn all recipe runners.
    let config_clone = config.clone();
    let root_clone = root.to_path_buf();
    let names_clone = recipe_names.to_vec();
    let vars_clone = env_vars.clone();
    let tx_clone = tx.clone();

    tokio::spawn(async move {
        let _ = runner::run_all(
            &config_clone,
            &root_clone,
            &names_clone,
            &vars_clone,
            dry_run,
            mode,
            tx_clone,
        )
        .await;
        // tx dropped here → rx.recv() returns None → signals completion
    });
    drop(tx);

    let mut event_stream = EventStream::new();
    let done_linger = Duration::from_secs(3);
    let mut done_at: Option<tokio::time::Instant> = None;

    loop {
        terminal.draw(|f| ui::render(f, &app))?;

        // When all recipes finish, linger briefly then exit automatically.
        if app.all_done && done_at.is_none() {
            done_at = Some(tokio::time::Instant::now());
        }
        if let Some(t) = done_at {
            if t.elapsed() >= done_linger {
                break;
            }
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
                        app.all_done = app.recipes.iter().all(|r| r.status.is_terminal());
                        terminal.draw(|f| ui::render(f, &app))?;
                        if done_at.is_none() {
                            done_at = Some(tokio::time::Instant::now());
                        }
                    }
                }
            }
            _ = sleep => {}
        }
    }

    let success = app.recipes.iter().all(|r| {
        matches!(r.status, crate::tui::app::RecipeStatus::Success)
    });
    Ok(success)
}
