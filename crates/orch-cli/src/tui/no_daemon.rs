//! The dashboard's no-daemon screen (design D5, plan Q0).
//!
//! When no daemon answers — or the one that did goes away, or a session is
//! poisoned by a reply that never came — the dashboard shows the frozen
//! two-line message full-screen with `r retry · q quit`. `r` only reconnects;
//! `q` exits 7. **Nothing here starts anything** (goal row 3, spec R4).
//!
//! The layout is a pure function ([`render`]) so it can be snapshotted at any
//! size; [`Screen`] puts it on the terminal with crossterm. The mouse is
//! deliberately *not* captured: the message names the command that starts the
//! daemon, and the terminal's own selection is how a user copies it.

use std::io::{self, IsTerminal, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::style::{Attribute, Print, SetAttribute};
use crossterm::terminal::{self, ClearType};
use crossterm::{cursor, execute, queue};
use orch_tui::text::{display_width, grapheme_segments};

/// The footer, the screen's whole key map.
pub const FOOTER: &str = "r retry · q quit";
/// Shown in place of the footer while a retry is connecting.
pub const CONNECTING: &str = "Connecting…";

/// Continuation indent for a wrapped message line: under the text after
/// `  ✕ `, and under `Start it with:` / `Detail:` on the second line.
const CONTINUATION: &str = "    ";

/// What the user chose.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Choice {
    Retry,
    Quit,
}

/// One cell row's role, which [`Screen`] maps to an attribute.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Blank,
    Headline,
    Body,
    Footer,
}

/// The screen as rows, top to bottom, for a `width` × `height` terminal.
///
/// The message block (the frozen text, wrapped to the width with a
/// continuation indent) and the footer two rows below it are centred
/// vertically; text stays left-aligned so the indent the message carries
/// still lines up. Rows past `height` are dropped from the *middle* of the
/// message, never the footer: a 2-row terminal still shows how to leave.
pub fn render(message: &str, footer: &str, width: u16, height: u16) -> Vec<(Role, String)> {
    let width = usize::from(width.max(1));
    let height = usize::from(height.max(1));
    let mut block: Vec<(Role, String)> = Vec::new();
    for (n, line) in message.lines().enumerate() {
        let role = if n == 0 { Role::Headline } else { Role::Body };
        for wrapped in wrap(line, width) {
            block.push((role, wrapped));
        }
    }
    let footer_rows = wrap(&format!("  {footer}"), width);
    // Keep the footer and as much of the message as fits, headline first.
    let room_for_message = height.saturating_sub(footer_rows.len() + 1);
    block.truncate(room_for_message);
    let gap = usize::from(!block.is_empty() && height > footer_rows.len() + block.len());
    let used = block.len() + gap + footer_rows.len();
    let top = height.saturating_sub(used) / 2;

    let mut rows = vec![(Role::Blank, String::new()); top];
    rows.extend(block);
    if gap == 1 {
        rows.push((Role::Blank, String::new()));
    }
    rows.extend(footer_rows.into_iter().map(|row| (Role::Footer, row)));
    rows.resize(height, (Role::Blank, String::new()));
    rows.truncate(height);
    rows
}

/// Greedy word wrap by display width (graphemes, so emoji and CJK in a
/// project path count as the two cells they take). A word wider than the line
/// is split. Continuations carry [`CONTINUATION`] when it fits.
fn wrap(line: &str, width: usize) -> Vec<String> {
    let indent = if width > CONTINUATION.len() + 8 {
        CONTINUATION
    } else {
        ""
    };
    let mut out = Vec::new();
    let mut current = String::new();
    let mut current_width = 0;
    let leading: String = line.chars().take_while(|c| *c == ' ').collect();
    let body = &line[leading.len()..];
    current.push_str(&leading);
    current_width += leading.len().min(width);
    let mut first_word = true;
    for word in body.split(' ') {
        let word_width: usize = grapheme_segments(word)
            .iter()
            .map(|g| display_width(g))
            .sum();
        let sep = usize::from(!first_word);
        if current_width + sep + word_width <= width {
            if !first_word {
                current.push(' ');
            }
            current.push_str(word);
            current_width += sep + word_width;
        } else {
            if current.trim().is_empty() {
                current.clear();
                current_width = 0;
            } else {
                out.push(std::mem::take(&mut current));
                current.push_str(indent);
                current_width = indent.len();
            }
            for grapheme in grapheme_segments(word) {
                let cells = display_width(&grapheme);
                if current_width + cells > width && current_width > indent.len() {
                    out.push(std::mem::take(&mut current));
                    current.push_str(indent);
                    current_width = indent.len();
                }
                current.push_str(&grapheme);
                current_width += cells;
            }
        }
        first_word = false;
    }
    if !current.trim().is_empty() || out.is_empty() {
        out.push(current);
    }
    out
}

/// The full-screen view on a real terminal. Holds raw mode and the alternate
/// screen until dropped, so a retry that fails again redraws without a flash.
pub struct Screen {
    out: io::Stdout,
}

impl Screen {
    /// Is there a terminal to show the screen on? Without one (stdin or
    /// stdout redirected) the caller prints the message and exits 7.
    pub fn available() -> bool {
        io::stdin().is_terminal() && io::stdout().is_terminal()
    }

    pub fn open() -> io::Result<Screen> {
        terminal::enable_raw_mode()?;
        let mut out = io::stdout();
        if let Err(err) = execute!(out, terminal::EnterAlternateScreen, cursor::Hide) {
            let _ = terminal::disable_raw_mode();
            return Err(err);
        }
        Ok(Screen { out })
    }

    /// Show `message` with `footer` once.
    pub fn draw(&mut self, message: &str, footer: &str) -> io::Result<()> {
        let (width, height) = terminal::size().unwrap_or((80, 24));
        queue!(self.out, terminal::Clear(ClearType::All))?;
        for (row, (role, text)) in render(message, footer, width, height).iter().enumerate() {
            if text.is_empty() {
                continue;
            }
            let attribute = match role {
                Role::Headline => Attribute::Bold,
                Role::Footer => Attribute::Dim,
                Role::Body | Role::Blank => Attribute::Reset,
            };
            queue!(
                self.out,
                cursor::MoveTo(0, u16::try_from(row).unwrap_or(u16::MAX)),
                SetAttribute(attribute),
                Print(text),
                SetAttribute(Attribute::Reset),
            )?;
        }
        self.out.flush()
    }

    /// Show `message` and wait for `r` or `q`. `stop` (SIGHUP/SIGTERM) quits.
    pub fn ask(&mut self, message: &str, stop: &AtomicBool) -> io::Result<Choice> {
        self.draw(message, FOOTER)?;
        loop {
            if stop.load(Ordering::SeqCst) {
                return Ok(Choice::Quit);
            }
            if !event::poll(Duration::from_millis(100))? {
                continue;
            }
            match event::read()? {
                Event::Key(key) => {
                    if let Some(choice) = choice_for(key) {
                        return Ok(choice);
                    }
                }
                Event::Resize(..) => self.draw(message, FOOTER)?,
                _ => {}
            }
        }
    }
}

impl Drop for Screen {
    fn drop(&mut self) {
        let _ = execute!(self.out, cursor::Show, terminal::LeaveAlternateScreen);
        let _ = terminal::disable_raw_mode();
    }
}

/// The key map: `r` retries; `q`, Esc and Ctrl-C quit. Everything else is
/// ignored, so a stray key cannot dismiss the message.
pub fn choice_for(key: KeyEvent) -> Option<Choice> {
    if key.kind == KeyEventKind::Release {
        return None;
    }
    match key.code {
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => Some(Choice::Quit),
        KeyCode::Char('r' | 'R') => Some(Choice::Retry),
        KeyCode::Char('q' | 'Q') | KeyCode::Esc => Some(Choice::Quit),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MESSAGE: &str = "  ✕ No orch daemon is running for /Users/me/projects/demo (no socket)\n    Start it with: orch daemon start";

    fn text(rows: &[(Role, String)]) -> String {
        rows.iter()
            .map(|(_, row)| row.trim_end())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn the_screen_at_80_by_24_is_the_frozen_message_and_the_footer() {
        let rows = render(MESSAGE, FOOTER, 80, 24);
        assert_eq!(rows.len(), 24);
        insta::assert_snapshot!("no_daemon_80x24", text(&rows));
        let headline = rows.iter().find(|(role, _)| *role == Role::Headline);
        assert!(headline.is_some_and(|(_, t)| t.starts_with("  ✕ No orch daemon")));
        let footer = rows
            .iter()
            .filter(|(role, _)| *role == Role::Footer)
            .count();
        assert_eq!(footer, 1);
    }

    #[test]
    fn a_narrow_terminal_wraps_under_the_text_and_keeps_the_footer() {
        let rows = render(MESSAGE, FOOTER, 30, 10);
        insta::assert_snapshot!("no_daemon_30x10", text(&rows));
        for (_, row) in &rows {
            let width: usize = grapheme_segments(row)
                .iter()
                .map(|g| display_width(g))
                .sum();
            assert!(width <= 30, "{row:?} is {width} cells");
        }
        assert!(
            rows.iter()
                .any(|(role, t)| *role == Role::Footer && t.contains("q quit"))
        );
    }

    #[test]
    fn a_two_row_terminal_still_says_how_to_leave() {
        let rows = render(MESSAGE, FOOTER, 80, 2);
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().any(|(role, _)| *role == Role::Footer));
    }

    #[test]
    fn wide_graphemes_in_a_project_path_wrap_by_cells() {
        let message = "  ✕ No orch daemon is running for /tmp/プロジェクト🧪-名前 (no socket)\n    Start it with: orch daemon start";
        let rows = render(message, FOOTER, 24, 12);
        for (_, row) in &rows {
            let width: usize = grapheme_segments(row)
                .iter()
                .map(|g| display_width(g))
                .sum();
            assert!(width <= 24, "{row:?} is {width} cells");
        }
        let joined: String = rows.iter().map(|(_, r)| r.trim()).collect();
        assert!(joined.contains("🧪"), "no grapheme is lost: {joined}");
    }

    #[test]
    fn only_r_and_q_answer() {
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        assert_eq!(choice_for(key(KeyCode::Char('r'))), Some(Choice::Retry));
        assert_eq!(choice_for(key(KeyCode::Char('q'))), Some(Choice::Quit));
        assert_eq!(choice_for(key(KeyCode::Esc)), Some(Choice::Quit));
        assert_eq!(
            choice_for(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            Some(Choice::Quit)
        );
        assert_eq!(choice_for(key(KeyCode::Enter)), None);
        assert_eq!(
            choice_for(key(KeyCode::Char('s'))),
            None,
            "nothing starts a daemon"
        );
    }
}
