//! The undo banner — one row per pending soft deletion.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use ratatui::text::Span;

use crate::format::cap_line;
use crate::hit::{ZoneId, ZoneKind};
use crate::messages::UNDO_TIMEOUT_MS;
use crate::theme::{BALLOT_X, PIPE, Token};
use crate::views::{RenderCtx, put_line};

pub fn render(frame: &mut Frame, area: Rect, ctx: &RenderCtx) {
    for (row, deletion) in ctx.state.pending_deletions.iter().enumerate() {
        // The row index is what `undo_deletion_at` needs; the deletion's key
        // is what identifies it. Pointing at the row rather than the last
        // deletion is the whole reason `undo_deletion_at` exists (§3.2.2).
        ctx.row_zone(
            area,
            area.y + row as u16,
            ZoneKind::UndoRow { index: row },
            ZoneId::UndoRow(deletion.key),
        );
        let label = deletion.entity_type.label();
        let remaining_ms = (deletion.expires_at - ctx.now_ms).clamp(0, UNDO_TIMEOUT_MS);
        let seconds = (remaining_ms as f64 / 1000.0).ceil() as i64;
        let name_max =
            ((area.width as usize).saturating_sub(4 + label.chars().count() + 30)).max(10);
        let spans = vec![
            Span::styled(format!("{BALLOT_X} "), ctx.style(Token::Yellow)),
            Span::styled(label.to_string(), ctx.bold(Token::Yellow)),
            Span::styled(
                format!(" \"{}\" — ", cap_line(&deletion.entity_name, name_max)),
                ctx.style(Token::Yellow),
            ),
            Span::styled(format!("{seconds}s"), ctx.bold(Token::Amber)),
            Span::styled(format!(" {PIPE} "), ctx.style(Token::Yellow)),
            Span::styled("Z", ctx.style(Token::Gray).add_modifier(Modifier::BOLD)),
            Span::styled(" undo", ctx.style(Token::Yellow)),
        ];
        put_line(frame, area, area.y + row as u16, spans);
    }
}
