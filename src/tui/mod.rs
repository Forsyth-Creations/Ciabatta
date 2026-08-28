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
use crate::runner::{self, Cancel, ProgressUpdate, RunCtl, RunMode};
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

    let result = tui_loop(
        &mut terminal,
        config,
        root,
        recipe_names,
        env_vars,
        dry_run,
        mode,
    )
    .await;

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
    let mut app = App::new(recipe_names, dry_run, mode);
    let (tx, mut rx) = mpsc::channel::<ProgressUpdate>(256);

    // Spawn all recipe runners.
    let config_clone = config.clone();
    let root_clone = root.to_path_buf();
    let names_clone = recipe_names.to_vec();
    let vars_clone = env_vars.clone();
    let tx_clone = tx.clone();

    // The TUI is in raw mode, so Ctrl-C arrives as a keystroke and no signal
    // ever reaches the steps. Without a stop switch, quitting left the compiler
    // — or the deploy — running with nothing left on screen to say so.
    let cancel = Cancel::new();
    let ctl = RunCtl {
        cancel: Some(cancel.clone()),
        ..Default::default()
    };

    let mut runner_task = tokio::spawn(async move {
        let _ = runner::run_all_ctl(
            &config_clone,
            &root_clone,
            &names_clone,
            &vars_clone,
            dry_run,
            mode,
            ctl,
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

        // When all recipes finish successfully, keep the results on screen for
        // `done_linger` so they can be read, then exit automatically (a keypress
        // quits sooner). If any recipe failed, stay open so the errors remain
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
                            // Quitting a finished run is just closing the
                            // window. Quitting a live one stops it, and waits
                            // long enough to say so — see `wind_down`.
                            if !app.all_done {
                                app.stopping = true;
                                cancel.stop();
                                terminal.draw(|f| ui::render(f, &app))?;
                            }
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

    if app.stopping {
        wind_down(&mut runner_task).await;
    }

    let success = app
        .recipes
        .iter()
        .all(|r| matches!(r.status, crate::tui::app::RecipeStatus::Success));
    Ok(success)
}

/// How long to let a stopped run tidy up before it is taken out by force.
const WIND_DOWN: Duration = Duration::from_secs(3);

/// Wait for a stopped run to finish dying, then abort it if it won't.
///
/// The stop switch is cooperative: the engine notices it, drops the step's
/// action, and the drop is what kills the process group. Returning from the TUI
/// before that has happened would restore the terminal and exit with the build
/// still running — the exact bug the switch exists to fix. Aborting the task
/// drops the same future, so the backstop kills the process group too; it's a
/// backstop only because a run that reports what it stopped is nicer than one
/// that vanishes.
async fn wind_down(task: &mut tokio::task::JoinHandle<()>) {
    if tokio::time::timeout(WIND_DOWN, &mut *task).await.is_err() {
        task.abort();
        let _ = task.await;
    }
}
