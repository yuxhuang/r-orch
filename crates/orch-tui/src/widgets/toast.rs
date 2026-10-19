//! The toast banner.
//!
//! At most two rows — the **first** two of the queue, not the newest.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use ratatui::text::Span;

use crate::format::cap_line;
use crate::views::{RenderCtx, put_line};

/// Toast titles truncate at 40 (39 + `…`).
const TITLE_MAX: usize = 40;

pub fn render(frame: &mut Frame, area: Rect, ctx: &RenderCtx) {
    for (row, toast) in ctx.state.visible_toasts().iter().enumerate() {
        let dimmed = toast.is_dimmed(ctx.now_ms);
        let base = ctx.chip(toast.kind.foreground(), toast.kind.background());
        let style = if dimmed {
            base.add_modifier(Modifier::DIM)
        } else {
            base
        };
        let spans = vec![
            Span::styled(format!(" {} ", toast.kind.icon()), style),
            Span::styled(
                cap_line(&toast.title, TITLE_MAX),
                style.add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!(" {} ", toast.message), style),
        ];
        put_line(frame, area, area.y + row as u16, spans);
    }
}
