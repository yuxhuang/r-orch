//! The logs view, labelled `ACTIONS` (docs/parity/tui.md §1.4).
//!
//! Occupies the whole content height: line 1 is the status bar, line 2 the
//! agent chips, and the rest are message rows.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use ratatui::text::Span;

use crate::format::{cap_line, relative_time};
use crate::hit::{PickerKind, ScrollRegion, VerbId, ZoneId, ZoneKind};
use crate::messages::{LOG_SESSION_GAP_MS, type_filter_label};
use crate::theme::{CURSOR, Token, spinner_frame};
use crate::views::spans_width;
use crate::views::{RenderCtx, put_line};

const EMPTY_BOX: [&str; 7] = [
    "    ╭──────────────────────────╮",
    "    │                          │",
    "    │  ◇ Waiting for activity  │",
    "    │  │  Run tasks or start   │",
    "    │  │  the orchestrator     │",
    "    │  ◇                       │",
    "    ╰──────────────────────────╯",
];

pub fn render(frame: &mut Frame, area: Rect, ctx: &RenderCtx) {
    let state = ctx.state;
    render_status_bar(frame, area, ctx);
    render_agent_chips(frame, area, ctx);

    let rows_area = Rect::new(
        area.x,
        area.y + 2,
        area.width,
        area.height.saturating_sub(2),
    );
    if rows_area.height == 0 {
        return;
    }

    if state.messages.is_empty() {
        for (row, line) in EMPTY_BOX.iter().enumerate() {
            put_line(
                frame,
                rows_area,
                rows_area.y + row as u16,
                vec![Span::styled((*line).to_string(), ctx.style(Token::Ghost))],
            );
        }
        return;
    }

    let filtered = state.filtered_messages();
    if filtered.is_empty() {
        put_line(
            frame,
            rows_area,
            rows_area.y,
            vec![Span::styled(
                "No events for current filter.".to_string(),
                ctx.style(Token::Dim),
            )],
        );
        return;
    }

    ctx.zone(
        rows_area,
        rows_area,
        ZoneKind::Scroll(ScrollRegion::MainList),
        ZoneId::ScrollRegion(ScrollRegion::MainList),
    );

    let visible = state.visible_log_indices(rows_area.height as usize);

    let agent_col_w = state
        .agents
        .iter()
        .map(|a| a.name.chars().count())
        .max()
        .unwrap_or(6)
        .clamp(6, 10);
    let prefix_w = 11 + agent_col_w;

    let mut previous: Option<usize> = None;
    for (row, message_index) in visible.iter().enumerate() {
        let message = &state.messages[*message_index];
        let selected = state
            .log_selected_index
            .try_into()
            .ok()
            .and_then(|i: usize| filtered.get(i))
            == Some(message_index);

        let continuation = previous.is_some_and(|prev_index| {
            let prev = &state.messages[prev_index];
            prev.agent_id.is_some() && prev.agent_id == message.agent_id
        });
        let session_start = !continuation
            || previous.is_some_and(|prev_index| {
                message.ts_ms - state.messages[prev_index].ts_ms > LOG_SESSION_GAP_MS
            });

        let agent_token = message
            .agent_id
            .as_deref()
            .map(|id| state.agent_color(id))
            .unwrap_or(Token::Dim);

        let gutter = if session_start && message.agent_id.is_some() {
            "┌"
        } else if continuation {
            "│"
        } else {
            " "
        };
        let rel = relative_time(message.ts_ms, ctx.now_ms);
        let badge = match (&message.agent_id, continuation) {
            (Some(id), false) => cap_line(&state.agent_name(id), agent_col_w),
            (Some(_), true) => "·".to_string(),
            (None, _) => String::new(),
        };
        let text_w = (area.width as usize)
            .saturating_sub(2)
            .saturating_sub(prefix_w + badge.chars().count())
            .max(10);

        let text_token = message.msg_type.text_token().unwrap_or(message.color);
        let mut spans = vec![
            Span::styled(gutter.to_string(), ctx.style(agent_token)),
            Span::styled(
                if selected {
                    CURSOR.to_string()
                } else {
                    " ".to_string()
                },
                ctx.style(Token::Amber),
            ),
            Span::styled(
                format!("{rel:>4} "),
                ctx.style(if rel == "now" {
                    Token::Green
                } else if selected {
                    Token::Silver
                } else {
                    Token::Ghost
                }),
            ),
            Span::styled(
                format!("{:<width$} ", badge, width = agent_col_w),
                ctx.style(agent_token).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!(" {} ", message.msg_type.icon()),
                ctx.style(if message.msg_type == crate::messages::MsgType::Error {
                    Token::Red
                } else {
                    agent_token
                }),
            ),
            Span::styled(cap_line(&message.text, text_w), {
                let style = if selected {
                    ctx.bold(Token::White)
                } else {
                    ctx.style(text_token)
                };
                if message.msg_type == crate::messages::MsgType::Lifecycle {
                    style.add_modifier(Modifier::BOLD)
                } else {
                    style
                }
            }),
        ];

        // The task-context badge only renders above 80 columns.
        if area.width > 80
            && let Some(task_id) = message.task_id.as_deref()
            && let Some(task) = state.task_by_id(task_id)
        {
            let title: String = task.title.chars().take(20).collect();
            spans.push(Span::styled(
                format!(" #{title} "),
                ctx.chip(Token::Dim, Token::Void),
            ));
        }

        put_line(frame, rows_area, rows_area.y + row as u16, spans);
        // From the same vector the row was drawn from, so the row → message
        // mapping has one implementation (PARITY.md §14.20, §3.2.2). The
        // filtered index is what the action needs; the message id is what
        // identifies the row across frames.
        if let Some(filtered_index) = filtered.iter().position(|i| i == message_index) {
            ctx.row_zone(
                rows_area,
                rows_area.y + row as u16,
                ZoneKind::LogRow { filtered_index },
                ZoneId::LogMessage(message.id),
            );
        }
        previous = Some(*message_index);
    }
}

fn render_status_bar(frame: &mut Frame, area: Rect, ctx: &RenderCtx) {
    let state = ctx.state;
    let filtered = state.filtered_messages();
    let mut spans = Vec::new();

    if state.log_selected_index < 0 {
        spans.push(Span::styled(
            format!(" {} LIVE ", spinner_frame(state.tick)),
            ctx.chip(Token::Green, Token::SuccessBg),
        ));
    } else {
        spans.push(Span::styled(
            format!(" ↑↓ {}/{} ", state.log_selected_index + 1, filtered.len()),
            ctx.chip(Token::Amber, Token::WarnBg),
        ));
    }
    // Both forms of the chip return to tail, which is what `Esc` does here.
    ctx.zone(
        Rect::new(area.x, area.y, spans_width(&spans), 1),
        area,
        ZoneKind::Verb(VerbId::LogsTail),
        ZoneId::Verb(VerbId::LogsTail),
    );
    spans.push(Span::styled(
        format!(" {} events", filtered.len()),
        ctx.style(Token::Dim),
    ));

    let type_label = type_filter_label(&state.log_type_filter);
    if type_label != "all" {
        spans.push(Span::styled(
            format!(" f:{type_label}"),
            ctx.style(Token::Amber),
        ));
    }
    if !state.log_agent_filter.is_empty() {
        spans.push(Span::styled(
            format!(
                " {}/{} agents",
                state.log_agent_filter.len(),
                state.agents.len()
            ),
            ctx.style(Token::Cyan),
        ));
    }

    spans.push(Span::raw("  "));
    for (key, label, verb) in [
        ("a", "filter", VerbId::OpenAgentPicker),
        ("f", "type", VerbId::OpenTypePicker),
        ("F", "cycle", VerbId::CycleLogTypes),
    ] {
        // Measured before the pair is pushed, so the zone covers exactly the
        // `<key> <label>` cells this iteration adds.
        let x = area.x + spans_width(&spans);
        spans.push(Span::styled(key.to_string(), ctx.bold(Token::Amber)));
        spans.push(Span::styled(format!(" {label}  "), ctx.style(Token::Dim)));
        let width = area.x + spans_width(&spans) - x;
        ctx.zone(
            Rect::new(x, area.y, width, 1),
            area,
            ZoneKind::Verb(verb),
            ZoneId::Verb(verb),
        );
    }

    put_line(frame, area, area.y, spans);
}

fn render_agent_chips(frame: &mut Frame, area: Rect, ctx: &RenderCtx) {
    let state = ctx.state;
    if state.agents.is_empty() {
        return;
    }
    let chip_len =
        (((area.width as i64 - 20) / state.agents.len().max(1) as i64 - 1).max(4) as usize).min(10);
    let mut spans = Vec::new();
    for (index, agent) in state.agents.iter().enumerate() {
        let x = area.x + spans_width(&spans);
        let active =
            state.log_agent_filter.is_empty() || state.log_agent_filter.contains(&agent.id);
        let style = if active {
            ctx.style(state.agent_color(&agent.id))
                .add_modifier(Modifier::BOLD)
        } else {
            ctx.style(Token::Ghost)
        };
        spans.push(Span::styled(
            format!("{} ", cap_line(&agent.name, chip_len)),
            style,
        ));
        ctx.zone(
            Rect::new(x, area.y + 1, area.x + spans_width(&spans) - x, 1),
            area,
            ZoneKind::LogAgentChip { index },
            ZoneId::PickerItem(PickerKind::Agent, agent.id.clone()),
        );
    }
    put_line(frame, area, area.y + 1, spans);
}
