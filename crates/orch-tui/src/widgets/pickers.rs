//! The two logs filter pickers (docs/parity/tui.md §3.3, §3.4).

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use ratatui::text::Span;

use crate::format::cap_line;
use crate::messages::MsgType;
use crate::theme::{CURSOR, LOZENGE, Token, light_rule};
use crate::views::{RenderCtx, put_line};

const FOOTER: &str = " Space toggle · a all · Enter confirm · Esc cancel";
const PRESETS: &str = " 1=all · 2=text · 3=tools · 4=errors · 5=events";

pub fn render_agent_picker(frame: &mut Frame, area: Rect, ctx: &RenderCtx) {
    let state = ctx.state;
    let total = state.agents.len();
    // An empty selection renders as all-checked.
    let implicit_all = state.picker_draft.is_empty();
    let selected_count = if implicit_all {
        total
    } else {
        state.picker_draft.len()
    };

    let max_height = ((area.height as usize).saturating_sub(4)).min(18);
    let list_h = max_height.saturating_sub(5).max(3);
    let max_offset = total.saturating_sub(list_h);
    let offset = max_offset.min(state.picker_cursor.saturating_sub(list_h / 2));

    let mut y = area.y;
    put_line(
        frame,
        area,
        y,
        vec![
            Span::styled(format!(" {LOZENGE} Agent Filter"), ctx.bold(Token::Amber)),
            Span::styled(
                format!(" · {selected_count}/{total} selected"),
                ctx.style(Token::Dim),
            ),
        ],
    );
    y += 1;
    put_line(
        frame,
        area,
        y,
        vec![Span::styled(light_rule(36), ctx.style(Token::Ghost))],
    );
    y += 1;

    for (row, index) in (offset..total).take(list_h).enumerate() {
        let agent = &state.agents[index];
        let checked = implicit_all || state.picker_draft.contains(&agent.id);
        let is_cursor = index == state.picker_cursor;
        let count = state
            .messages
            .iter()
            .filter(|m| m.agent_id.as_deref() == Some(agent.id.as_str()))
            .count();
        let name_token = if is_cursor {
            state.agent_color(&agent.id)
        } else if checked {
            Token::Silver
        } else {
            Token::Dim
        };
        let mut spans = vec![
            Span::styled(
                if is_cursor {
                    format!(" {CURSOR} ")
                } else {
                    "   ".to_string()
                },
                ctx.style(Token::Amber),
            ),
            Span::styled(
                if checked { "[✓]" } else { "[ ]" }.to_string(),
                ctx.style(if checked { Token::Green } else { Token::Ghost }),
            ),
            Span::styled(format!(" {}", agent.name), ctx.style(name_token)),
        ];
        if count > 0 {
            spans.push(Span::styled(format!(" ·{count}"), ctx.style(Token::Ghost)));
        }
        put_line(frame, area, y + row as u16, spans);
    }
    y += list_h.min(total.saturating_sub(offset)) as u16;

    if total > list_h {
        put_line(
            frame,
            area,
            y,
            vec![Span::styled(
                format!("   ↑ ↓ {}/{total}", state.picker_cursor + 1),
                ctx.style(Token::Ghost),
            )],
        );
        y += 1;
    }
    put_line(
        frame,
        area,
        y,
        vec![Span::styled(light_rule(36), ctx.style(Token::Ghost))],
    );
    y += 1;
    put_line(
        frame,
        area,
        y,
        vec![Span::styled(FOOTER.to_string(), ctx.style(Token::Gray))],
    );
}

pub fn render_type_picker(frame: &mut Frame, area: Rect, ctx: &RenderCtx) {
    let state = ctx.state;
    let selected = state.type_picker_draft.len();
    let mut y = area.y;

    put_line(
        frame,
        area,
        y,
        vec![
            Span::styled(format!(" {LOZENGE} Type Filter"), ctx.bold(Token::Amber)),
            Span::styled(format!(" · {selected}/8 selected"), ctx.style(Token::Dim)),
        ],
    );
    y += 1;
    put_line(
        frame,
        area,
        y,
        vec![Span::styled(PRESETS.to_string(), ctx.style(Token::Gray))],
    );
    y += 1;
    put_line(
        frame,
        area,
        y,
        vec![Span::styled(light_rule(36), ctx.style(Token::Ghost))],
    );
    y += 1;

    for (index, msg_type) in MsgType::ALL.into_iter().enumerate() {
        let checked = state.type_picker_draft.contains(&msg_type);
        let is_cursor = index == state.picker_cursor;
        let count = state
            .messages
            .iter()
            .filter(|m| m.msg_type == msg_type)
            .count();
        let label_token = if is_cursor {
            msg_type.picker_token()
        } else if checked {
            Token::Silver
        } else {
            Token::Dim
        };
        let mut spans = vec![
            Span::styled(
                if is_cursor {
                    format!(" {CURSOR} ")
                } else {
                    "   ".to_string()
                },
                ctx.style(Token::Amber),
            ),
            Span::styled(
                if checked { "[✓]" } else { "[ ]" }.to_string(),
                ctx.style(if checked { Token::Green } else { Token::Ghost }),
            ),
            Span::styled(
                format!(" {} {}", msg_type.picker_icon(), msg_type.label()),
                ctx.style(label_token).add_modifier(if is_cursor {
                    Modifier::BOLD
                } else {
                    Modifier::empty()
                }),
            ),
        ];
        if count > 0 {
            spans.push(Span::styled(format!(" ·{count}"), ctx.style(Token::Ghost)));
        }
        put_line(frame, area, y + index as u16, spans);
    }
    y += MsgType::ALL.len() as u16;

    put_line(
        frame,
        area,
        y,
        vec![Span::styled(light_rule(36), ctx.style(Token::Ghost))],
    );
    y += 1;
    put_line(
        frame,
        area,
        y,
        vec![Span::styled(
            cap_line(FOOTER, area.width as usize),
            ctx.style(Token::Gray),
        )],
    );
}
