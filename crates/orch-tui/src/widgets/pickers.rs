//! The two logs filter pickers.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use ratatui::text::Span;

use crate::format::cap_line;
use crate::hit::{PickerKind, RowPart, ScrollRegion, VerbId, ZoneId, ZoneKind};
use crate::messages::MsgType;
use crate::theme::{CURSOR, LOZENGE, Token, light_rule};
use crate::views::{RenderCtx, put_line, spans_width};

/// The footer verbs, one span each so every verb has its own x-range.
/// Concatenated with [`SEPARATOR`] these are byte-for-byte the single
/// `" Space toggle · a all · Enter confirm · Esc cancel"` string they replace
/// (design §3.7) — the leading space belongs to the first verb.
const FOOTER: [(&str, VerbId); 4] = [
    (" Space toggle", VerbId::PickerToggle),
    ("a all", VerbId::PickerAll),
    ("Enter confirm", VerbId::PickerConfirm),
    ("Esc cancel", VerbId::PickerCancel),
];

/// Likewise for `" 1=all · 2=text · 3=tools · 4=errors · 5=events"`.
const PRESETS: [(&str, VerbId); 5] = [
    (" 1=all", VerbId::PickerPreset('1')),
    ("2=text", VerbId::PickerPreset('2')),
    ("3=tools", VerbId::PickerPreset('3')),
    ("4=errors", VerbId::PickerPreset('4')),
    ("5=events", VerbId::PickerPreset('5')),
];

const SEPARATOR: &str = " · ";

/// Draw a verb row and register a zone over each verb's own cells.
///
/// The row is still rendered as the **one capped span** it always was, so the
/// output is byte-identical by construction rather than by inspection. Only
/// the zones are new: each verb's offsets are taken from the uncapped string
/// and then limited to the cells that actually survived the cap, so a verb
/// the cap ate — or the `…` that replaced it — is not clickable (§3.2.1).
///
/// Without these zones a mouse-only user cannot confirm or cancel a picker,
/// which assumption 7 forbids.
fn put_verb_row(
    frame: &mut Frame,
    area: Rect,
    y: u16,
    ctx: &RenderCtx,
    verbs: &[(&str, VerbId)],
    cap: Option<usize>,
) {
    let mut text = String::new();
    let mut spans = Vec::new();
    for (index, (label, verb)) in verbs.iter().enumerate() {
        if index > 0 {
            text.push_str(SEPARATOR);
        }
        let start = text.chars().count() as u16;
        text.push_str(label);
        spans.push((start, text.chars().count() as u16, *verb));
    }

    let rendered = match cap {
        Some(max) => cap_line(&text, max),
        None => text,
    };
    // `cap_line` spends its last cell on `…`, which names no verb.
    let painted = rendered.chars().count() as u16;
    let clickable = if rendered.ends_with('…') {
        painted.saturating_sub(1)
    } else {
        painted
    };

    for (start, end, verb) in spans {
        let end = end.min(clickable);
        if start >= end {
            continue;
        }
        ctx.zone(
            Rect::new(area.x + start, y, end - start, 1),
            area,
            ZoneKind::Verb(verb),
            ZoneId::Verb(verb),
        );
    }
    put_line(
        frame,
        area,
        y,
        vec![Span::styled(rendered, ctx.style(Token::Gray))],
    );
}

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
    ctx.zone(
        area,
        area,
        ZoneKind::Scroll(ScrollRegion::AgentPicker),
        ZoneId::ScrollRegion(ScrollRegion::AgentPicker),
    );
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
        // The checkbox toggles; the label moves the cursor and toggles on a
        // double click. Both identify the agent, not the row.
        let row_y = y + row as u16;
        let checkbox_x = area.x + spans_width(&spans[..1]);
        let checkbox_w = spans_width(&spans[1..2]);
        let id = ZoneId::PickerItem(PickerKind::Agent, agent.id.clone());
        ctx.zone(
            Rect::new(checkbox_x, row_y, checkbox_w, 1),
            area,
            ZoneKind::PickerRow {
                index,
                part: RowPart::Checkbox,
            },
            id.clone(),
        );
        ctx.zone(
            Rect::new(
                checkbox_x + checkbox_w,
                row_y,
                area.width.saturating_sub(checkbox_x + checkbox_w - area.x),
                1,
            ),
            area,
            ZoneKind::PickerRow {
                index,
                part: RowPart::Label,
            },
            id,
        );
        put_line(frame, area, row_y, spans);
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
    put_verb_row(frame, area, y, ctx, &FOOTER, None);
}

pub fn render_type_picker(frame: &mut Frame, area: Rect, ctx: &RenderCtx) {
    let state = ctx.state;
    let selected = state.type_picker_draft.len();
    let mut y = area.y;

    ctx.zone(
        area,
        area,
        ZoneKind::Scroll(ScrollRegion::TypePicker),
        ZoneId::ScrollRegion(ScrollRegion::TypePicker),
    );
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
    put_verb_row(frame, area, y, ctx, &PRESETS, None);
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
        let row_y = y + index as u16;
        let checkbox_x = area.x + spans_width(&spans[..1]);
        let checkbox_w = spans_width(&spans[1..2]);
        // Identity is the type's own name, so it survives a reorder of
        // `MsgType::ALL` as well as a cursor move.
        let id = ZoneId::PickerItem(PickerKind::Type, msg_type.label().to_string());
        ctx.zone(
            Rect::new(checkbox_x, row_y, checkbox_w, 1),
            area,
            ZoneKind::TypePickerRow {
                index,
                part: RowPart::Checkbox,
            },
            id.clone(),
        );
        ctx.zone(
            Rect::new(
                checkbox_x + checkbox_w,
                row_y,
                area.width.saturating_sub(checkbox_x + checkbox_w - area.x),
                1,
            ),
            area,
            ZoneKind::TypePickerRow {
                index,
                part: RowPart::Label,
            },
            id,
        );
        put_line(frame, area, row_y, spans);
    }
    y += MsgType::ALL.len() as u16;

    put_line(
        frame,
        area,
        y,
        vec![Span::styled(light_rule(36), ctx.style(Token::Ghost))],
    );
    y += 1;
    put_verb_row(frame, area, y, ctx, &FOOTER, Some(area.width as usize));
}
