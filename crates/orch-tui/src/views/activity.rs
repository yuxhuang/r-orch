//! The activity feed in the bottom panel.
//!
//! Rows are **top-padded** with blanks so the region always renders exactly
//! `height` rows.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use ratatui::text::Span;

use crate::format::{cap_line, relative_time};
use crate::hit::{PaneMetrics, VerbId, ZoneId, ZoneKind};
use crate::messages::{Message, MsgType, preset_label};
use crate::state::Focus;
use crate::theme::{ACCENT_BAR, THIN_BAR, Token};
use crate::views::{RenderCtx, put_line, section_label, spans_width};
use crate::widgets::text_viewer::ViewerSource;

/// 9 spaces — `AGENT_INDENT`.
const AGENT_INDENT: &str = "         ";

pub fn render(frame: &mut Frame, area: Rect, ctx: &RenderCtx) {
    let state = ctx.state;
    let filtered = state.activity_messages();
    let key = state.pane_key(Focus::Activity);
    ctx.zone(
        area,
        area,
        ZoneKind::Pane(Focus::Activity),
        ZoneId::Pane(key.clone()),
    );
    let label = if state.focus == Focus::Activity {
        "> ACTIVITY"
    } else {
        "ACTIVITY"
    };
    // section_label adds three leading rules, two spaces and two suffix rules.
    let suffix_budget =
        (area.width as usize).saturating_sub(7 + unicode_width::UnicodeWidthStr::width(label));
    let mut suffix = vec![
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
    let follow_label = if state.activity_follow_tail {
        " · following"
    } else {
        " · paused · End follows"
    };
    let follow_width = unicode_width::UnicodeWidthStr::width(follow_label);
    let mut filter_width = spans_width(&suffix);
    if state.retained_evicted > 0 || state.retained_rejected > 0 {
        let mut losses = format!(
            "evicted{} rejected{}",
            state.retained_evicted, state.retained_rejected
        );
        if spans_width(&suffix) as usize
            + unicode_width::UnicodeWidthStr::width(losses.as_str())
            + 1
            + follow_width
            > suffix_budget
        {
            suffix.clear();
            filter_width = 0;
        }
        // Removing the filter alone may still leave large counters crowding
        // out the recovery hint. Keep both counts with shorter labels then.
        if 1 + unicode_width::UnicodeWidthStr::width(losses.as_str()) + follow_width > suffix_budget
        {
            losses = format!(
                "ev{} rej{}",
                state.retained_evicted, state.retained_rejected
            );
        }
        suffix.push(Span::styled(
            format!(" {}", losses),
            ctx.style(Token::Amber),
        ));
    }
    // The `F:PRESET │ n/m` suffix cycles the preset, which is what `f`/`F`
    // does off the logs view. `section_label` reports where it put the suffix
    // rather than this view re-deriving that layout.
    let suffix_width = filter_width;
    suffix.push(Span::styled(
        follow_label,
        ctx.style(if state.activity_follow_tail {
            Token::Green
        } else {
            Token::Amber
        }),
    ));
    let suffix_x = section_label(
        frame,
        Rect::new(area.x, area.y, area.width, 1),
        ctx,
        label,
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
    ctx.publish_pane_metrics(PaneMetrics {
        key,
        body: rows_area,
        items: filtered.len(),
        display_rows: filtered.len(),
        selection_rows: (0..filtered.len()).collect(),
    });
    let height = rows_area.height as usize;
    if height == 0 {
        return;
    }

    let offset = if state.activity_follow_tail {
        filtered.len().saturating_sub(height)
    } else {
        state
            .activity_scroll
            .min(filtered.len().saturating_sub(height))
    };
    let visible: Vec<usize> = filtered.iter().skip(offset).take(height).copied().collect();
    // Top padding keeps the feed bottom-aligned.
    let pad = height.saturating_sub(visible.len());

    let mut groups = RowGroups::default();
    for (row, message_index) in visible.iter().enumerate() {
        let message = &state.messages[*message_index];
        let spans = row_spans(
            ctx,
            message,
            groups.next(message),
            state.activity_selected == Some(message.id),
            area.width,
        );
        let y = rows_area.y + (pad + row) as u16;
        put_line(frame, rows_area, y, spans);
        ctx.row_zone(
            rows_area,
            y,
            ZoneKind::ActivityRow {
                message_id: message.id,
            },
            ZoneId::ActivityMessage(message.id),
        );
        ctx.publish_text_anchor(
            ViewerSource::Message(message.id),
            Rect::new(rows_area.x, y, rows_area.width, 1),
            0,
        );
    }
}

/// Agent grouping across consecutive rows: a run of messages from one agent
/// shows the time and badge once, and alternate runs get the stripe.
#[derive(Debug, Default)]
pub(crate) struct RowGroups {
    /// `None` before the first row, so the window's top row always starts a
    /// group (and shows its time) even when it has no agent.
    previous_agent: Option<Option<String>>,
    index: usize,
}

/// Where a row sits in its agent group, from [`RowGroups::next`].
#[derive(Debug, Clone, Copy)]
pub(crate) struct RowGroup {
    /// First row of its group: carries the time and the agent badge.
    first: bool,
    /// Odd groups are striped.
    striped: bool,
}

impl RowGroups {
    pub(crate) fn next(&mut self, message: &Message) -> RowGroup {
        let first = self.previous_agent.as_ref() != Some(&message.agent_id);
        if first {
            self.index += 1;
            self.previous_agent = Some(message.agent_id.clone());
        }
        RowGroup {
            first,
            striped: self.index % 2 == 1,
        }
    }
}

/// One activity row, shared by the bottom feed and the detail Activity
/// sections so they cannot drift: accent or selection marker, relative time,
/// agent badge, type icon and the text capped to `width`, with error and
/// group-stripe backgrounds. Callers own the row's zones and anchors.
pub(crate) fn row_spans(
    ctx: &RenderCtx,
    message: &Message,
    group: RowGroup,
    selected: bool,
    width: u16,
) -> Vec<Span<'static>> {
    let state = ctx.state;
    let text_w = (width as usize).saturating_sub(2 + 17).max(10);
    let agent_token = message
        .agent_id
        .as_deref()
        .map(|id| state.agent_color(id))
        .unwrap_or(Token::Dim);
    let accent = match (&message.agent_id, group.first) {
        (Some(_), true) => ACCENT_BAR,
        (Some(_), false) => THIN_BAR,
        (None, _) => " ",
    };
    let rel = if group.first {
        relative_time(message.ts_ms, ctx.now_ms)
    } else {
        String::new()
    };
    let badge = match (&message.agent_id, group.first) {
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
    } else if group.striped {
        Some(Token::AlternatingRowBg)
    } else {
        None
    };
    let text_token = message.msg_type.text_token().unwrap_or(message.color);
    let text_style = match background {
        Some(bg) => ctx.chip(text_token, bg),
        None => ctx.style(text_token),
    };

    vec![
        Span::styled(
            if selected {
                ">".into()
            } else {
                accent.to_string()
            },
            ctx.style(if selected { Token::Amber } else { agent_token }),
        ),
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
    ]
}
