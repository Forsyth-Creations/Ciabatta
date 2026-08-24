//! Whether this run's output — ours and the commands' — is allowed colour.
//!
//! A step's command writes into a pipe, never a terminal, because that is the
//! only way ciabatta can prefix, tag and route its output. Every well-behaved
//! tool checks for a terminal and turns colour off when it doesn't find one, so
//! a `cargo build` that is red and yellow when run by hand arrives here as flat
//! grey — the tool did the right thing with the wrong information.
//!
//! The fix is to give it the right information: the three environment variables
//! the ecosystem actually reads, set only when ciabatta's *own* output really is
//! going to a terminal that will render the escapes. That last part is why this
//! is a decision made once per process rather than a constant:
//!
//! * the plain runner prints child lines straight through, so escapes work —
//!   when there is a terminal on the other end to render them;
//! * the TUI draws them into a ratatui widget that styles lines itself and
//!   measures their width for wrapping — escapes there are visible garbage that
//!   also breaks the layout;
//! * the daemon ships them to the web app, which parses SGR escapes into styled
//!   spans (`AnsiText`) and drops the rest. That consumer renders colour and is
//!   never a terminal, which is why "is stdout a TTY?" is the wrong question to
//!   ask about it.
//!
//! So the default is off, and each consumer says what it can render.

use std::io::IsTerminal;
use std::sync::atomic::{AtomicBool, Ordering};

use owo_colors::Style;

static ENABLED: AtomicBool = AtomicBool::new(false);

/// Whether colour is wanted for this process's run output.
pub fn enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

/// Who is going to read this run's output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Consumer {
    /// This process's stdout — colour only if something is actually attached to
    /// it, so a redirected run stays greppable.
    Terminal,
    /// The TUI, which styles and measures lines itself.
    Tui,
    /// The web app, which parses the escapes and renders them as spans. Not a
    /// terminal, and none the less the consumer most able to show colour.
    Web,
}

/// Decide, for a run about to start, whether its output should be coloured.
pub fn decide(consumer: Consumer) {
    let wanted = wanted(
        consumer,
        // `NO_COLOR` is honoured by its convention: set at all, to anything,
        // means no colour (<https://no-color.org>).
        std::env::var_os("NO_COLOR").is_some(),
        std::io::stdout().is_terminal(),
    );
    ENABLED.store(wanted, Ordering::Relaxed);
}

/// The decision itself: a consumer that renders escapes, and no request to stop.
///
/// `is_terminal` only constrains the terminal consumer. Asking it of the web app
/// would answer a question about the daemon's stdout — which is a log file, or
/// `/dev/null` — when what matters is the browser at the other end.
fn wanted(consumer: Consumer, no_color: bool, is_terminal: bool) -> bool {
    if no_color {
        return false;
    }
    match consumer {
        Consumer::Terminal => is_terminal,
        Consumer::Tui => false,
        Consumer::Web => true,
    }
}

/// Ask a command for colour, if this run is showing it.
///
/// Call this *before* the caller's own environment goes on, so a step that sets
/// `FORCE_COLOR=0` — or a CI image that already decided — still wins.
pub fn request(command: &mut tokio::process::Command) {
    if !enabled() {
        return;
    }
    // Three spellings because no single one is universal: `FORCE_COLOR` is the
    // Node ecosystem's, `CLICOLOR_FORCE` is the BSD/Rust one, and `CLICOLOR` is
    // read by tools that only ever wanted permission.
    command.env("FORCE_COLOR", "1");
    command.env("CLICOLOR_FORCE", "1");
    command.env("CLICOLOR", "1");
}

/// A style that paints only when this run is in colour.
///
/// Used instead of `OwoColorize`'s methods directly so a redirected run — or a
/// `NO_COLOR` one — writes plain text rather than escapes somebody has to strip
/// back out later.
fn styled(style: Style) -> Style {
    paint(enabled(), style)
}

fn paint(enabled: bool, style: Style) -> Style {
    if enabled { style } else { Style::new() }
}

/// Something worked.
pub fn good() -> Style {
    styled(Style::new().green())
}

/// Something failed.
pub fn bad() -> Style {
    styled(Style::new().red())
}

/// Something needs attention but isn't a failure.
pub fn warn() -> Style {
    styled(Style::new().yellow())
}

/// Structure rather than content — prefixes, labels, counts.
pub fn faint() -> Style {
    styled(Style::new().dimmed())
}

/// Work starting.
pub fn active() -> Style {
    styled(Style::new().cyan())
}

#[cfg(test)]
mod tests {
    use super::*;
    use owo_colors::OwoColorize;

    /// The point of the whole module: a run that isn't showing colour must emit
    /// none, so redirected output stays greppable.
    #[test]
    fn styles_are_inert_until_a_run_asks_for_colour() {
        assert_eq!(
            "ok".style(paint(false, Style::new().green())).to_string(),
            "ok"
        );
        assert!(
            "ok".style(paint(true, Style::new().green()))
                .to_string()
                .contains('\u{1b}')
        );
    }

    /// A TUI run draws the lines itself and measures their width; escapes there
    /// are corruption, whatever else is true.
    #[test]
    fn a_view_that_cannot_render_escapes_never_gets_colour() {
        assert!(!wanted(Consumer::Tui, false, true));
        assert!(!wanted(Consumer::Tui, false, false));
    }

    /// Nobody watching, or asked not to — either answer is no.
    #[test]
    fn a_redirected_or_no_color_run_stays_plain() {
        assert!(!wanted(Consumer::Terminal, false, false));
        assert!(!wanted(Consumer::Terminal, true, true));
        assert!(wanted(Consumer::Terminal, false, true));
    }

    /// The web app renders escapes into spans and is never a terminal. Judging
    /// it by the daemon's stdout — a log file, or nothing at all — is how the
    /// run view ended up the one place in ciabatta with no colour in it.
    #[test]
    fn the_web_app_gets_colour_though_it_is_not_a_terminal() {
        assert!(wanted(Consumer::Web, false, false));
        assert!(
            !wanted(Consumer::Web, true, false),
            "NO_COLOR is still the operator's to set"
        );
    }
}
