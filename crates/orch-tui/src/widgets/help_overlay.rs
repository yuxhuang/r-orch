//! The help overlay (docs/parity/tui.md §3.2).
//!
//! A centred amber rounded box: title, three column dividers, ten shortcut
//! rows in three 22-column columns, and the dismiss footer.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use ratatui::text::Span;

use crate::theme::Token;
use crate::views::{RenderCtx, put_line};

const COL_W: usize = 22;
/// `COL_W*3 + 3 + 3 + 2`.
const INNER_W: usize = COL_W * 3 + 3 + 3 + 2;
const OUTER_W: usize = INNER_W + 2;
const MAX_ROWS: usize = 10;

pub const NAVIGATION: [(&str, &str); 7] = [
    ("↑↓/j/k", "Navigate"),
    ("Tab/←→", "Switch tabs"),
    ("Enter", "Detail view"),
    ("G", "Goals tab"),
    ("T", "Tasks tab"),
    ("A", "Agents tab"),
    ("L", "Logs tab"),
];

pub const ACTIONS: [(&str, &str); 10] = [
    ("N", "New item"),
    ("E", "Edit"),
    ("D", "Delete"),
    ("R", "Run task"),
    ("S", "Show / Stop"),
    ("C", "Cancel"),
    ("A", "Approve"),
    ("X", "Reject"),
    ("U", "Autonomous"),
    ("Z", "Undo delete"),
];

pub const COMMANDS: [(&str, &str); 7] = [
    ("/", "Command mode"),
    ("/task add", "Create task"),
    ("/run", "Execute"),
    ("/watch", "Auto-dispatch"),
    ("/config", "Settings"),
    ("/help", "Help"),
    ("/quit", "Exit"),
];

pub fn render(frame: &mut Frame, area: Rect, ctx: &RenderCtx) {
    let pad_x = ((area.width as usize).saturating_sub(OUTER_W)) / 2;
    let margin_top = ((area.height as usize).saturating_sub(MAX_ROWS + 10)) / 2;
    let x = area.x + pad_x as u16;
    let width = OUTER_W.min(area.width as usize) as u16;
    let inner = Rect::new(x, area.y, width, area.height);
    let mut y = area.y + margin_top as u16;

    let border = ctx.style(Token::Amber);
    let centre = |text: String| -> String {
        let pad = INNER_W.saturating_sub(text.chars().count()) / 2;
        format!("{}{}", " ".repeat(pad), text)
    };

    put_line(
        frame,
        inner,
        y,
        vec![Span::styled(format!("╭{}╮", "─".repeat(INNER_W)), border)],
    );
    y += 1;
    put_line(frame, inner, y, vec![Span::styled("│", border)]);
    y += 1;
    put_line(
        frame,
        inner,
        y,
        vec![Span::styled(
            centre("KEYBOARD SHORTCUTS".into()),
            ctx.bold(Token::Amber),
        )],
    );
    y += 2;

    let dividers = ["NAVIGATION", "ACTIONS", "COMMANDS"];
    let mut header = String::new();
    for name in dividers {
        let pad = COL_W.saturating_sub(name.chars().count()) / 2;
        header.push_str(&" ".repeat(pad));
        header.push_str(name);
        header.push_str(&" ".repeat(COL_W - pad - name.chars().count()));
        header.push(' ');
    }
    put_line(
        frame,
        inner,
        y,
        vec![Span::styled(
            header,
            ctx.style(Token::Dim).add_modifier(Modifier::BOLD),
        )],
    );
    y += 2;

    for row in 0..MAX_ROWS {
        let mut spans = Vec::new();
        for (column, entries) in [&NAVIGATION[..], &ACTIONS[..], &COMMANDS[..]]
            .into_iter()
            .enumerate()
        {
            if column > 0 {
                spans.push(Span::styled("│", ctx.style(Token::Ghost)));
            }
            match entries.get(row) {
                Some((key, label)) => {
                    spans.push(Span::styled(format!("{key:<10}"), ctx.bold(Token::Amber)));
                    spans.push(Span::styled(
                        format!("{label:<12}"),
                        ctx.style(Token::Silver),
                    ));
                }
                None => spans.push(Span::raw(" ".repeat(COL_W))),
            }
        }
        put_line(frame, inner, y, spans);
        y += 1;
    }

    y += 1;
    put_line(
        frame,
        inner,
        y,
        vec![Span::styled(
            centre("Press any key to dismiss".into()),
            ctx.style(Token::Dim),
        )],
    );
    y += 2;
    put_line(
        frame,
        inner,
        y,
        vec![Span::styled(format!("╰{}╯", "─".repeat(INNER_W)), border)],
    );
}
