//! The activity feed in the bottom panel (docs/parity/tui.md §6
//! "`ActivityFeed`").
//!
//! Rows are **top-padded** with blanks so the region always renders exactly
//! `height` rows.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use ratatui::text::Span;

use crate::format::{cap_line, relative_time};
use crate::hit::{VerbId, ZoneId, ZoneKind};
use crate::messages::{MsgType, preset_label};
use crate::theme::{ACCENT_BAR, THIN_BAR, Token};
use crate::views::{RenderCtx, put_line, section_label, spans_width};

/// 9 spaces — `AGENT_INDENT`.
const AGENT_INDENT: &str = "         ";

pub fn render(frame: &mut Frame, area: Rect, ctx: &RenderCtx) {
    let state = ctx.state;
    let filtered = state.activity_messages();
    let suffix = vec![
        Span::styled("F:".to_string(), ctx.style(Token::Dim)),
        Span::styled(
            preset_label(&state.activity_filter).to_uppercase(),
            ctx.style(Token::Amber),
        ),
        Span::styled(
            format!(" │ {}/{}", filtered.len(), state.messages.len()),
            ctx.style(Token::Ghost),
        ),
    ];
    // The `F:PRESET │ n/m` suffix cycles the preset, which is what `f`/`F`
    // does off the logs view. `section_label` reports where it put the suffix
    // rather than this view re-deriving that layout.
    let suffix_width = spans_width(&suffix);
    let suffix_x = section_label(
        frame,
        Rect::new(area.x, area.y, area.width, 1),
        ctx,
        "ACTIVITY",
        Some(suffix),
    );
    ctx.zone(
        Rect::new(suffix_x, area.y, suffix_width, 1),
        area,
        ZoneKind::Verb(VerbId::CycleActivityFilter),
        ZoneId::Verb(VerbId::CycleActivityFilter),
    );

    let rows_area = Rect::new(
        area.x,
        area.y + 1,
        area.width,
        area.height.saturating_sub(1),
    );
    let height = rows_area.height as usize;
    if height == 0 {
        return;
    }

    let visible: Vec<usize> = filtered
        .iter()
        .skip(filtered.len().saturating_sub(height))
        .copied()
        .collect();
    // Top padding keeps the feed bottom-aligned.
    let pad = height.saturating_sub(visible.len());

    let text_w = (area.width as usize).saturating_sub(2 + 17).max(10);
    let mut previous_agent: Option<String> = None;
    let mut group_index = 0usize;

    for (row, message_index) in visible.iter().enumerate() {
        let message = &state.messages[*message_index];
        let new_group = message.agent_id != previous_agent;
        if new_group {
            group_index += 1;
            previous_agent = message.agent_id.clone();
        }
        let agent_token = message
            .agent_id
            .as_deref()
            .map(|id| state.agent_color(id))
            .unwrap_or(Token::Dim);

        let accent = match (&message.agent_id, new_group) {
            (Some(_), true) => ACCENT_BAR,
            (Some(_), false) => THIN_BAR,
            (None, _) => " ",
        };
        let rel = if new_group {
            relative_time(message.ts_ms, ctx.now_ms)
        } else {
            String::new()
        };
        let badge = match (&message.agent_id, new_group) {
            (Some(id), true) => {
                let name = state.agent_name(id);
                format!("{:<9}", name.chars().take(8).collect::<String>())
            }
            _ => AGENT_INDENT.to_string(),
        };

        // Zebra striping: odd agent groups get the alternating background;
        // error rows always get the error background and override the stripe.
        let background = if message.msg_type == MsgType::Error {
            Some(Token::ErrorBg)
        } else if group_index % 2 == 1 {
            Some(Token::AlternatingRowBg)
        } else {
            None
        };
        let text_token = message.msg_type.text_token().unwrap_or(message.color);
        let text_style = match background {
            Some(bg) => ctx.chip(text_token, bg),
            None => ctx.style(text_token),
        };

        let spans = vec![
            Span::styled(accent.to_string(), ctx.style(agent_token)),
            Span::styled(
                format!("{rel:>5}"),
                ctx.style(if rel == "now" {
                    Token::Green
                } else {
                    Token::Dim
                }),
            ),
            Span::styled(badge, ctx.style(agent_token).add_modifier(Modifier::BOLD)),
            Span::styled(
                format!("{} ", message.msg_type.icon()),
                ctx.style(agent_token),
            ),
            Span::styled(cap_line(&message.text, text_w), text_style),
        ];
        put_line(frame, rows_area, rows_area.y + (pad + row) as u16, spans);
    }
}
