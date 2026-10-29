pub mod app;
pub mod browser;
pub mod graph;
pub mod ui;

use std::collections::HashMap;
use std::io;
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
use ratatui::{Terminal, backend::CrosstermBackend};
use tokio::sync::mpsc;

use crate::config::CiabattaConfig;
use crate::runner::{self, ProgressUpdate};
use app::{App, Reply, StepStatus};

/// Set while the TUI has the screen. A log line written to stderr then would
/// land in the middle of its drawing, so the CLI's logger holds off.
static OWNS_SCREEN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Whether the TUI is drawing on the terminal right now.
pub fn owns_screen() -> bool {
    OWNS_SCREEN.load(std::sync::atomic::Ordering::Relaxed)
}

/// Notes raised while the TUI has the screen, waiting for it to show them.
static NOTES: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

/// Tell whoever is watching something in passing — "the remote cache is
/// unavailable". Printed to stderr, unless the TUI has the screen: written
/// there it would land in the middle of the drawing, so the TUI puts it in the
/// run's log instead.
pub fn note(message: impl Into<String>) {
    let message = message.into();
    if owns_screen()
        && let Ok(mut notes) = NOTES.lock()
    {
        notes.push(message);
        return;
    }
    eprintln!("{message}");
}

/// The notes waiting to be shown, oldest first.
fn take_notes() -> Vec<String> {
    NOTES
        .lock()
        .map(|mut notes| std::mem::take(&mut *notes))
        .unwrap_or_default()
}

/// Run a workflow inside the TUI. Returns whether it succeeded.
///
/// Leaving the alternate screen takes everything on it along, so once it
/// closes a short summary — each step's outcome, and the end of any failed
/// step's output — is printed to stderr where the scrollback keeps it.
#[allow(clippy::too_many_arguments)]
pub async fn run(
    name: &str,
    resolved: &crate::run::ResolvedRun,
    config: &CiabattaConfig,
    root: &std::path::Path,
    env_vars: &HashMap<String, String>,
    dry_run: bool,
    ctl: runner::RunCtl,
) -> Result<bool> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    // The wheel scrolls the output pane, so the TUI takes the mouse. `c`
    // hands it back for selecting text; most terminals also select with Shift
    // held.
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    // ratatui only draws what differs from the last frame, and its first
    // "last frame" is blank — so start from a screen that really is.
    terminal.clear()?;
    OWNS_SCREEN.store(true, std::sync::atomic::Ordering::Relaxed);

    let mut app = App::new(
        name,
        resolved.steps.iter().map(|s| s.name.as_str()),
        dry_run,
    );
    // The `.env` tab: the same report `--viz plain` prints before a run, in
    // the same colours. The escapes are kept and turned into styles as the
    // pane is drawn (see `ui::ansi_rows`), so there is one set of colours to
    // keep in step rather than two.
    let report = crate::run::envdeps::collect(resolved, root, env_vars, Some(config));
    app.env = match report.render(name, true) {
        Some(text) => text.lines().map(str::to_string).collect(),
        None => vec!["This run depends on no environment variables.".to_string()],
    };
    if let Some(text) = report.render_arguments() {
        use owo_colors::OwoColorize;
        // Yellow, as in the terminal: these exist for this run only.
        let warn = owo_colors::Style::new().yellow();
        app.env.push(String::new());
        app.env
            .extend(text.lines().map(|line| line.style(warn).to_string()));
    }
    let result = tui_loop(
        &mut terminal,
        &mut app,
        name,
        resolved,
        config,
        root,
        env_vars,
        dry_run,
        ctl,
    )
    .await;

    OWNS_SCREEN.store(false, std::sync::atomic::Ordering::Relaxed);
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;

    // Anything raised as the TUI was closing, too late for it to show.
    for note in take_notes() {
        app.note(note);
    }
    eprint!("{}", summary(&app));
    result.map(|()| app.succeeded())
}

/// How many of a failed step's last lines the summary repeats.
const FAILED_TAIL: usize = 20;

fn summary(app: &App) -> String {
    use std::fmt::Write;
    let mut out = String::new();
    let outcome = if app.succeeded() {
        "succeeded"
    } else if app.finished && app.run.status == app::RunStatus::Running {
        "stopped"
    } else if !app.finished {
        "left running"
    } else {
        "failed"
    };
    let _ = writeln!(
        out,
        "\n{} {outcome} in {}",
        app.run.name,
        runner::elapsed(app.run.elapsed())
    );
    for step in &app.steps {
        let took = step
            .elapsed()
            .map(|d| format!(" ({})", runner::elapsed(d)))
            .unwrap_or_default();
        let _ = writeln!(
            out,
            "  {} {} — {}{took}",
            step.status.emoji(),
            step.name,
            step.status.word()
        );
    }
    if !app.notes.is_empty() {
        out.push('\n');
        for note in &app.notes {
            let _ = writeln!(out, "{note}");
        }
    }
    for step in app.steps.iter().filter(|s| s.status == StepStatus::Failed) {
        let _ = writeln!(out, "\n── {} (last {FAILED_TAIL} lines) ──", step.name);
        let from = step.logs.len().saturating_sub(FAILED_TAIL);
        for line in &step.logs[from..] {
            let _ = writeln!(out, "{line}");
        }
    }
    out
}

/// How many rows of output one notch of the mouse wheel moves.
const WHEEL_ROWS: usize = 3;

/// How long a run that succeeded stays on screen before closing itself, unless
/// somebody starts looking around.
const DONE_LINGER: Duration = Duration::from_secs(10);

#[allow(clippy::too_many_arguments)]
async fn tui_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut App,
    name: &str,
    resolved: &crate::run::ResolvedRun,
    config: &CiabattaConfig,
    root: &std::path::Path,
    env_vars: &HashMap<String, String>,
    dry_run: bool,
    mut ctl: runner::RunCtl,
) -> Result<()> {
    let (tx, mut rx) = mpsc::channel::<ProgressUpdate>(256);
    // Quitting mid-run asks the engine to stop, so the steps it started are
    // killed rather than orphaned.
    let cancel = ctl.cancel.get_or_insert_with(Default::default).clone();
    // Somebody is at the keyboard: `ciabatta input` questions and recovery
    // choices are put to them rather than settled on their defaults.
    let inputs = tokio::sync::broadcast::channel::<crate::run::input::Answer>(16).0;
    let choices = tokio::sync::broadcast::channel::<runner::StepChoice>(16).0;
    ctl.inputs = Some(inputs.clone());
    ctl.choices = Some(choices.clone());
    ctl.interactive = true;

    let name_clone = name.to_string();
    let resolved_clone = resolved.clone();
    let config_clone = config.clone();
    let root_clone = root.to_path_buf();
    let vars_clone = env_vars.clone();

    tokio::spawn(async move {
        let _ = runner::run_workflow_ctl(
            &name_clone,
            &resolved_clone,
            &config_clone,
            &root_clone,
            &vars_clone,
            dry_run,
            ctl,
            tx,
        )
        .await;
        // tx dropped here → rx.recv() returns None → signals completion
    });

    let mut event_stream = EventStream::new();
    let mut done_at: Option<tokio::time::Instant> = None;
    let mut open = true;

    loop {
        for note in take_notes() {
            app.note(note);
        }
        terminal.draw(|f| ui::render(f, app))?;

        // A run that succeeded stays up long enough to be read, then closes;
        // one that failed stays until it's dismissed, and so does one that
        // somebody has started looking through.
        if app.finished && app.succeeded() && !app.browsing {
            let at = *done_at.get_or_insert_with(tokio::time::Instant::now);
            if at.elapsed() >= DONE_LINGER {
                break;
            }
        }

        // Half the log pane, for paging.
        let page = (terminal.size()?.height as usize / 2).max(1);

        tokio::select! {
            maybe_event = event_stream.next() => {
                match maybe_event {
                    Some(Ok(Event::Key(key))) if key.kind == KeyEventKind::Press => {
                        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
                        // A question on screen takes the keyboard, `q`
                        // included — it may be part of the answer. Esc and
                        // Ctrl-C still stop the run.
                        let asking = !app.prompts.is_empty();
                        // Selecting text: the keys that would leave the TUI
                        // leave select mode instead, and take the mouse back.
                        if app.selecting
                            && !asking
                            && !ctrl
                            && matches!(key.code, KeyCode::Char('c' | 'q') | KeyCode::Esc)
                        {
                            app.selecting = false;
                            execute!(terminal.backend_mut(), EnableMouseCapture)?;
                            continue;
                        }
                        let quit = (!asking && key.code == KeyCode::Char('q'))
                            || key.code == KeyCode::Esc
                            || (ctrl && key.code == KeyCode::Char('c'));
                        if quit {
                            if app.finished || app.stopping {
                                break;
                            }
                            app.stopping = true;
                            cancel.stop();
                            continue;
                        }
                        app.browsing = true;
                        if asking {
                            match app.prompt_key(key.code) {
                                Some(Reply::Input(answer)) => {
                                    let _ = inputs.send(answer);
                                }
                                Some(Reply::Choice(choice)) => {
                                    let _ = choices.send(choice);
                                }
                                None => {}
                            }
                            continue;
                        }
                        match key.code {
                            // The pane is all there is on screen, so the arrows
                            // scroll it — and a terminal that turns the wheel
                            // into arrows once it has the mouse back scrolls
                            // it too.
                            KeyCode::Down | KeyCode::Char('j') if app.selecting => {
                                app.scroll_down(1)
                            }
                            KeyCode::Up | KeyCode::Char('k') if app.selecting => app.scroll_up(1),
                            KeyCode::Char('c') => {
                                app.selecting = true;
                                execute!(terminal.backend_mut(), DisableMouseCapture)?;
                            }
                            KeyCode::Down | KeyCode::Char('j') => app.select_next(),
                            KeyCode::Up | KeyCode::Char('k') => app.select_prev(),
                            KeyCode::Home | KeyCode::Char('g') => app.select_first(),
                            KeyCode::Char('G') => app.select_last(),
                            KeyCode::Char('n') => app.select_next_interesting(),
                            KeyCode::Char('s') => app.toggle_sort(),
                            KeyCode::PageUp => app.scroll_up(page),
                            KeyCode::PageDown => app.scroll_down(page),
                            KeyCode::Char('u') if ctrl => app.scroll_up(page),
                            KeyCode::Char('d') if ctrl => app.scroll_down(page),
                            KeyCode::End | KeyCode::Char('f') => app.follow(),
                            KeyCode::Char('e') | KeyCode::Tab => app.toggle_env(),
                            _ => {}
                        }
                    }
                    Some(Ok(Event::Mouse(mouse))) => match mouse.kind {
                        MouseEventKind::ScrollUp => {
                            app.browsing = true;
                            app.scroll_up(WHEEL_ROWS);
                        }
                        MouseEventKind::ScrollDown => {
                            app.browsing = true;
                            app.scroll_down(WHEEL_ROWS);
                        }
                        _ => {}
                    },
                    Some(Err(e)) => return Err(e.into()),
                    None => break,
                    _ => {}
                }
            }
            maybe_update = rx.recv(), if open => {
                match maybe_update {
                    Some(update) => app.apply_update(update),
                    None => {
                        open = false;
                        app.finish();
                        // Stopped on request: there's nothing left to watch.
                        if app.stopping {
                            break;
                        }
                    }
                }
            }
            // Redraw for the clocks, even when nothing arrives.
            _ = tokio::time::sleep(Duration::from_millis(250)) => {}
        }
    }
    Ok(())
}
