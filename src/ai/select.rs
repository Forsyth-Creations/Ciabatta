//! Highlight-to-copy for the AI chat.
//!
//! The chat captures the mouse so the wheel scrolls the conversation, and a
//! captured mouse also swallows the drag a terminal would use to select text.
//! The way round it used to be Ctrl-T (release the mouse, lose the wheel) or
//! Shift-drag (which half the terminals people use don't support), so copying a
//! command the assistant suggested took a mode switch.
//!
//! So the chat does the selecting itself: a drag highlights cells the way a
//! terminal would, and letting go copies them. The selection is confined to the
//! pane it started in, so a drag across the conversation picks up its text and
//! not the border characters and hint bar around it.

use std::io::Write;

use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::Span;

/// A drag in progress, or finished and still on screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Selection {
    /// The pane it started in; the head is held inside it.
    region: Rect,
    anchor: Position,
    head: Position,
    /// Whether the mouse moved after going down. A click selects nothing.
    pub dragged: bool,
}

impl Selection {
    /// Start a selection at `at`, if it falls in one of `regions`.
    pub fn start(regions: &[Rect], at: Position) -> Option<Self> {
        let region = *regions.iter().find(|r| r.contains(at))?;
        Some(Self {
            region,
            anchor: at,
            head: at,
            dragged: false,
        })
    }

    /// Move the free end to `at`, held inside the starting pane — so dragging
    /// past its edge selects to the edge, as a terminal does.
    pub fn extend(&mut self, at: Position) {
        let r = self.region;
        self.head = Position {
            x: at.x.clamp(r.x, r.right().saturating_sub(1)),
            y: at.y.clamp(r.y, r.bottom().saturating_sub(1)),
        };
        self.dragged |= self.head != self.anchor;
    }

    /// The two ends in reading order.
    fn ends(&self) -> (Position, Position) {
        let (a, b) = (self.anchor, self.head);
        if (a.y, a.x) <= (b.y, b.x) {
            (a, b)
        } else {
            (b, a)
        }
    }

    /// The columns selected on row `y`: from the start column on the first row,
    /// to the end column on the last, and the pane's full width between —
    /// selection runs like text, not like a rectangle.
    fn columns(&self, y: u16) -> Option<std::ops::RangeInclusive<u16>> {
        let (start, end) = self.ends();
        if y < start.y || y > end.y {
            return None;
        }
        let first = if y == start.y { start.x } else { self.region.x };
        let last = if y == end.y {
            end.x
        } else {
            self.region.right().saturating_sub(1)
        };
        Some(first..=last)
    }

    /// Paint the selection over a drawn frame.
    pub fn highlight(&self, buffer: &mut Buffer) {
        if !self.dragged {
            return;
        }
        let (start, end) = self.ends();
        for y in start.y..=end.y {
            for x in self.columns(y).into_iter().flatten() {
                if let Some(cell) = buffer.cell_mut(Position { x, y }) {
                    cell.set_style(Style::default().add_modifier(Modifier::REVERSED));
                }
            }
        }
    }

    /// The selected text, read off a drawn frame: one line per row, trailing
    /// blanks trimmed, as a terminal's own copy would give it.
    pub fn text(&self, buffer: &Buffer) -> String {
        let (start, end) = self.ends();
        let mut lines = Vec::new();
        for y in start.y..=end.y {
            let mut line = String::new();
            // A wide character occupies its own cell and blanks the next; the
            // blank is padding, not a space somebody typed.
            let mut skip = 0;
            for x in self.columns(y).into_iter().flatten() {
                if skip > 0 {
                    skip -= 1;
                    continue;
                }
                let Some(cell) = buffer.cell(Position { x, y }) else {
                    continue;
                };
                let symbol = cell.symbol();
                skip = Span::raw(symbol).width().saturating_sub(1);
                line.push_str(symbol);
            }
            lines.push(line.trim_end().to_string());
        }
        lines.join("\n").trim_matches('\n').to_string()
    }
}

/// Put `text` on the clipboard, every way that might work, and say which.
///
/// Two routes, both taken. OSC 52 asks the terminal itself to set the
/// clipboard, which is the only route that works over SSH — but several
/// terminals ignore it. A native tool (`wl-copy`, `xclip`, `pbcopy`, …) works
/// locally whatever the terminal. Neither can report whether it landed, so the
/// return value is what was *tried*, for the status line to be honest about.
pub fn copy(text: &str) -> Vec<&'static str> {
    let mut tried = Vec::new();

    let osc = format!(
        "\x1b]52;c;{}\x07",
        crate::registry::nexus::base64_encode(text.as_bytes())
    );
    let mut stdout = std::io::stdout();
    if stdout
        .write_all(osc.as_bytes())
        .and_then(|_| stdout.flush())
        .is_ok()
    {
        tried.push("terminal");
    }

    if let Some(tool) = native_tool()
        && pipe_to(tool, text)
    {
        tried.push(tool[0]);
    }
    tried
}

/// The clipboard command for this platform and session, if there is one.
fn native_tool() -> Option<&'static [&'static str]> {
    let present = |var: &str| std::env::var_os(var).is_some_and(|v| !v.is_empty());
    let candidates: &[&'static [&'static str]] = if cfg!(target_os = "macos") {
        &[&["pbcopy"]]
    } else if cfg!(windows) {
        &[&["clip"]]
    } else if present("WAYLAND_DISPLAY") {
        &[&["wl-copy"], &["xclip", "-selection", "clipboard"]]
    } else if present("DISPLAY") {
        &[
            &["xclip", "-selection", "clipboard"],
            &["xsel", "--clipboard", "--input"],
        ]
    } else {
        &[]
    };
    candidates.iter().copied().find(|tool| on_path(tool[0]))
}

fn on_path(program: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|paths| {
        std::env::split_paths(&paths).any(|dir| {
            let path = dir.join(program);
            path.is_file() || path.with_extension("exe").is_file()
        })
    })
}

/// Start `tool` with `text` on its stdin. Not waited for: `wl-copy` and `xclip`
/// stay running to serve the clipboard until something else takes it, and a
/// chat that blocked on them would freeze until then.
fn pipe_to(tool: &[&str], text: &str) -> bool {
    use std::process::{Command, Stdio};
    let Ok(mut child) = Command::new(tool[0])
        .args(&tool[1..])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    else {
        return false;
    };
    let wrote = child
        .stdin
        .take()
        .is_some_and(|mut stdin| stdin.write_all(text.as_bytes()).is_ok());
    // Reap it in the background so it doesn't linger as a zombie.
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    wrote
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::widgets::Widget;

    fn frame(lines: &[&str]) -> Buffer {
        let area = Rect::new(0, 0, 12, lines.len() as u16);
        let mut buffer = Buffer::empty(area);
        ratatui::widgets::Paragraph::new(lines.join("\n")).render(area, &mut buffer);
        buffer
    }

    #[test]
    fn a_drag_copies_like_text_not_like_a_rectangle() {
        let buffer = frame(&["first line", "second line", "third"]);
        let mut sel = Selection::start(&[buffer.area], Position { x: 6, y: 0 }).unwrap();
        sel.extend(Position { x: 2, y: 2 });
        assert_eq!(sel.text(&buffer), "line\nsecond line\nthi");
    }

    #[test]
    fn dragging_backwards_selects_the_same_text() {
        let buffer = frame(&["first line", "second line"]);
        let mut sel = Selection::start(&[buffer.area], Position { x: 5, y: 1 }).unwrap();
        sel.extend(Position { x: 0, y: 0 });
        assert_eq!(sel.text(&buffer), "first line\nsecond");
    }

    #[test]
    fn a_selection_stays_inside_the_pane_it_started_in() {
        let buffer = frame(&["|abc def|", "|ghi jkl|"]);
        let pane = Rect::new(1, 0, 7, 2);
        let mut sel = Selection::start(&[pane], Position { x: 1, y: 0 }).unwrap();
        // Dragged well past the pane's right and bottom edges.
        sel.extend(Position { x: 40, y: 9 });
        assert_eq!(sel.text(&buffer), "abc def\nghi jkl");
        // Somewhere that isn't a pane starts nothing.
        assert!(Selection::start(&[pane], Position { x: 0, y: 0 }).is_none());
    }

    #[test]
    fn a_click_is_not_a_selection() {
        let mut sel = Selection::start(&[Rect::new(0, 0, 5, 1)], Position { x: 2, y: 0 }).unwrap();
        sel.extend(Position { x: 2, y: 0 });
        assert!(!sel.dragged);
    }

    #[test]
    fn wide_characters_are_not_followed_by_a_space() {
        let buffer = frame(&["日本 ok"]);
        let mut sel = Selection::start(&[buffer.area], Position { x: 0, y: 0 }).unwrap();
        sel.extend(Position { x: 11, y: 0 });
        assert_eq!(sel.text(&buffer), "日本 ok");
    }
}
