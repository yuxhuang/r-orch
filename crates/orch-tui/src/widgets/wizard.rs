//! Form-wizard rendering (docs/parity/tui.md §5.1).

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::Span;

use crate::format::cap_line;
use crate::hit::{RowPart, ScrollRegion, VerbId, ZoneId, ZoneKind};
use crate::text::cursor::{Cursor, display_width, text_display_width};
use crate::theme::{BLOCK, CHECK, CIRCLE, CIRCLE_FILLED, CURSOR, Token, light_rule};
use crate::views::{RenderCtx, put_line, spans_width};
use crate::wizard::engine::{StepKind, Wizard};

/// `CMD_KEY` — `⌘` on macOS, `Ctrl` elsewhere.
pub const CMD_KEY: &str = if cfg!(target_os = "macos") {
    "⌘"
} else {
    "Ctrl"
};

/// Inner geometry only: B0 supplies the actual reserved wizard rectangle.
pub fn field_rect(area: Rect, wizard: &Wizard) -> Rect {
    if area.height < 2 {
        return Rect::new(area.x, area.y, area.width, 0);
    }
    let error = wizard.visible_error().is_some() || wizard.session_error.is_some();
    let description = wizard.step().description.is_some();
    let compact = area.height < 8 + u16::from(description) + u16::from(error);
    let header = if compact {
        u16::from(area.height >= 5)
    } else {
        3 + u16::from(description)
    };
    let error_rows = if compact { 0 } else { u16::from(error) };
    Rect::new(
        area.x,
        area.y + header,
        area.width,
        area.height.saturating_sub(header + error_rows + 1),
    )
}

pub fn render(frame: &mut Frame, area: Rect, ctx: &RenderCtx, wizard: &Wizard) {
    if area.height < 2 || area.width == 0 {
        return;
    }
    let field = field_rect(area, wizard);
    wizard.viewport_rows.set(field.height as usize);
    let compact = field.y <= area.y + 1;
    if compact && area.height >= 5 {
        let summary = wizard
            .session_notice
            .as_deref()
            .or(wizard.step().description.as_deref())
            .unwrap_or(&wizard.title);
        put_line(
            frame,
            area,
            area.y,
            vec![Span::styled(
                fit(summary, area.width as usize),
                ctx.style(Token::Dim),
            )],
        );
    } else if !compact {
        let title = format!(
            "{} — step {}/{}{}",
            wizard.title,
            wizard.active_index() + 1,
            wizard.active_count(),
            wizard
                .session_notice
                .as_ref()
                .map(|n| format!(" — {n}"))
                .unwrap_or_default()
        );
        put_line(
            frame,
            area,
            area.y,
            vec![Span::styled(
                fit(&title, area.width as usize),
                ctx.bold(Token::Amber),
            )],
        );
        let dots = (0..wizard.active_count())
            .map(|i| {
                Span::styled(
                    if i == wizard.active_index() {
                        CIRCLE_FILLED
                    } else if i < wizard.active_index() {
                        CHECK
                    } else {
                        CIRCLE
                    },
                    ctx.style(Token::Amber),
                )
            })
            .collect();
        put_line(frame, area, area.y + 1, dots);
        put_line(
            frame,
            area,
            area.y + 2,
            vec![Span::styled(
                fit(&wizard.step().label, area.width as usize),
                ctx.bold(Token::White),
            )],
        );
        if let Some(description) = wizard.step().description.as_deref() {
            put_line(
                frame,
                area,
                area.y + 3,
                vec![Span::styled(
                    fit(description, area.width as usize),
                    ctx.style(Token::Dim),
                )],
            );
        }
    }
    if field.height > 0 {
        match wizard.step().kind {
            StepKind::Text => {
                render_text_field(frame, field, ctx, wizard);
            }
            StepKind::Textarea => {
                render_textarea_field(frame, field, ctx, wizard);
            }
            StepKind::Select => {
                render_select_field(frame, field, ctx, wizard);
            }
            StepKind::Multiselect => {
                render_multiselect_field(frame, field, ctx, wizard);
            }
        }
        if matches!(wizard.step().kind, StepKind::Text | StepKind::Textarea) {
            let body = if wizard.step().kind == StepKind::Text {
                Rect::new(field.x, field.y, field.width, 1)
            } else {
                field
            };
            ctx.zone(
                body,
                area,
                ZoneKind::EditorText,
                ZoneId::EditorText(
                    crate::wizard::DraftKey::from_wizard(wizard),
                    wizard.step_id().into(),
                ),
            );
        }
    }
    if !compact && let Some(error) = wizard.session_error.as_deref().or(wizard.visible_error()) {
        put_line(
            frame,
            area,
            area.bottom() - 2,
            vec![Span::styled(
                fit(error, area.width as usize),
                ctx.style(Token::Red),
            )],
        );
    }
    render_footer(
        frame,
        Rect::new(area.x, area.bottom() - 1, area.width, 1),
        ctx,
        wizard,
    );
}

/// Display-cell truncation shared by compact chrome and status rows.
fn fit(text: &str, width: usize) -> String {
    let safe: String = text
        .chars()
        .map(|ch| {
            if ch.is_control() || matches!(ch,'\u{202a}'..='\u{202e}'|'\u{2066}'..='\u{2069}') {
                '�'
            } else {
                ch
            }
        })
        .collect();
    crate::format::cap_display(&safe, width)
}

fn render_footer(frame: &mut Frame, area: Rect, ctx: &RenderCtx, wizard: &Wizard) {
    use crate::hit::{TextAction, TextSurfaceId};
    use crate::wizard::session::{COPY_ACTIONS, copy_label};
    let surface = TextSurfaceId::Editor(
        crate::wizard::DraftKey::from_wizard(wizard),
        wizard.step_id().into(),
    );
    let origin = ctx.state.live_copy_origin(&surface);
    if let Some(origin) = origin {
        ctx.publish_text_footer(origin.clone(), area);
    }
    let action_zone = |offset: u16, width: u16, action: TextAction| {
        ctx.zone(
            Rect::new(area.x + offset, area.y, width, 1),
            area,
            ZoneKind::TextAction(action),
            ZoneId::TextAction(surface.clone(), action),
        );
    };
    let hint = if let Some(index) = wizard.copy_rail {
        let label = copy_label(COPY_ACTIONS[index], wizard.selected_text().is_some());
        let cells = text_display_width(&label);
        action_zone(1, 1, TextAction::PreviousCopyAction);
        action_zone(3, cells as u16, COPY_ACTIONS[index]);
        action_zone(33, 1, TextAction::NextCopyAction);
        action_zone(35, 1, TextAction::CloseCopyRail);
        format!(
            " < {label}{} > x ",
            " ".repeat(29usize.saturating_sub(cells))
        )
    } else if let Some(notice) = wizard.copy_notice.as_deref() {
        action_zone(
            0,
            text_display_width(notice).min(area.width as usize) as u16,
            TextAction::DismissCopyFeedback,
        );
        notice.into()
    } else if let Some(origin) = origin.filter(|o| ctx.state.copy_footer_visible(o)) {
        if let Some(feedback) = ctx.state.feedback_for(origin) {
            use crate::backend::{CopyOutcome, CopyTarget};
            let text = match &feedback.outcome {
                CopyOutcome::NativeWritten => "Copied on this host; F6",
                CopyOutcome::TerminalRequested => {
                    let guidance = feedback.guidance.as_deref().unwrap_or("");
                    match (guidance.contains("tmux"), guidance.contains("screen")) {
                        (true, true) => "Screen unsupported; tmux on+OSC; F6",
                        (true, false) => "Requested; tmux needs on+OSC; F6 host",
                        (false, true) => "Requested; screen unsupported; F6 host",
                        _ => "Requested from terminal; F6",
                    }
                }
                CopyOutcome::Failed(_) if feedback.request.target == CopyTarget::Native => {
                    "Host failed; F6 retry; F1 help"
                }
                CopyOutcome::Failed(_) => "Copy failed; F6 retry; F1 help",
            };
            action_zone(
                0,
                text_display_width(text).min(area.width as usize) as u16,
                TextAction::DismissCopyFeedback,
            );
            text.into()
        } else {
            action_zone(18, 1, TextAction::DismissCopyFeedback);
            "Copy pending; F6  x".into()
        }
    } else if !wizard.suppress_validation_footer
        && (wizard.session_error.is_some() || wizard.visible_error().is_some())
    {
        format!(
            "{}; F1 help",
            wizard
                .session_error
                .as_deref()
                .or(wizard.visible_error())
                .unwrap_or("")
        )
    } else {
        // Mandatory controls fit the 38-cell minimum without a modifier mapping.
        let editable = matches!(wizard.step().kind, StepKind::Text | StepKind::Textarea);
        let back = if wizard.active_index() > 0 {
            "Esc back"
        } else {
            "Esc cancel"
        };
        let prefix = if editable { "Copy… (F6)  " } else { "" };
        if editable {
            action_zone(0, 10, TextAction::OpenCopyRail);
        }
        let hint = format!("{prefix}Tab OK  {back}");
        for (label, verb) in [
            ("Tab OK", VerbId::WizardConfirm),
            (back, VerbId::WizardBack),
        ] {
            let offset = text_display_width(
                &hint[..hint
                    .find(label)
                    .expect("the hint is built from these labels")],
            ) as u16;
            ctx.zone(
                Rect::new(area.x + offset, area.y, text_display_width(label) as u16, 1),
                area,
                ZoneKind::Verb(verb),
                ZoneId::Verb(verb),
            );
        }
        if wizard.step().kind == StepKind::Multiselect
            && area.width >= text_display_width(&hint) as u16 + 14
        {
            let offset = text_display_width(&hint) as u16 + 2;
            ctx.zone(
                Rect::new(area.x + offset, area.y, 12, 1),
                area,
                ZoneKind::Verb(VerbId::WizardToggle),
                ZoneId::Verb(VerbId::WizardToggle),
            );
            format!("{hint}  Space toggle")
        } else {
            let scope = if wizard.selected_text().is_some() {
                "  range selected; Ctrl+Y all"
            } else {
                "  Ctrl+Y all"
            };
            let mut hint = hint;
            if editable
                && text_display_width(&hint) + text_display_width(scope) <= area.width as usize
            {
                hint.push_str(scope);
            }
            // A list longer than its window says where the selection is, so
            // the rows below the fold are known to exist.
            if wizard.step().kind == StepKind::Select
                && wizard.options().len() > wizard.viewport_rows.get()
            {
                let position = format!(
                    "  {} of {}",
                    wizard.select_index() + 1,
                    wizard.options().len()
                );
                if text_display_width(&hint) + text_display_width(&position) <= area.width as usize
                {
                    hint.push_str(&position);
                }
            }
            for extra in ["  Enter newline", "  arrows move", "  paste image"] {
                let applicable = match extra {
                    "  Enter newline" => wizard.step().kind == StepKind::Textarea,
                    "  paste image" => wizard.paste_capable,
                    _ => editable,
                };
                if applicable
                    && text_display_width(&hint) + text_display_width(extra) <= area.width as usize
                {
                    hint.push_str(extra);
                }
            }
            hint
        }
    };
    put_line(
        frame,
        area,
        area.y,
        vec![Span::styled(
            fit(&hint, area.width as usize),
            ctx.style(Token::Gray),
        )],
    );
}

/// Identity for a wizard row: the wizard, the step and the option's own
/// value. `WizardOption { index }` alone was identical on every step, so
/// three fast clicks could walk a wizard to "create" with defaults — the
/// `step_id` is what stops a double click surviving a step change (§3.3.1).
fn option_id(wizard: &Wizard, value: &str) -> ZoneId {
    ZoneId::WizardOption {
        kind: wizard.kind.clone(),
        step_id: wizard.step_id().to_string(),
        value: value.to_string(),
    }
}

fn render_text_field(frame: &mut Frame, area: Rect, ctx: &RenderCtx, wizard: &Wizard) -> u16 {
    let step = wizard.step();
    let value = wizard.text_value();
    let max_w = (area.width as usize).saturating_sub(2);
    let cursor = wizard.text_input().cursor();
    wizard.text_render.set(Some((
        Rect::new(area.x, area.y, area.width, 1),
        viewport_start(cursor, max_w.max(1)),
        cursor.pos(),
    )));
    let mut spans = vec![Span::styled("> ", ctx.style(Token::Ghost))];
    if value.is_empty() && !step.placeholder.is_empty() {
        spans.push(Span::styled(
            cap_line(&step.placeholder, max_w),
            ctx.style(Token::Ghost),
        ));
    } else {
        // A display-width-aware sliding viewport that keeps the cursor visible
        // (docs/parity/tui.md §5.1); CJK and emoji count as two cells, so the
        // window is measured in cells, not characters.
        let cursor = wizard.text_input().cursor();
        let field_w = max_w.max(1);
        let (before, after) = slide_viewport(cursor, field_w);
        let start = viewport_start(cursor, field_w);
        let selection = wizard.text_input().selection_range();
        for (i, segment) in crate::text::grapheme_segments(&before).iter().enumerate() {
            let style = if selection.is_some_and(|(a, b)| (a..b).contains(&(start + i))) {
                ctx.style(Token::White).reversed()
            } else {
                ctx.style(Token::White)
            };
            spans.push(Span::styled(segment.clone(), style));
        }
        spans.push(Span::styled(BLOCK, ctx.style(Token::Amber)));
        let after_start = cursor.pos();
        for (i, segment) in crate::text::grapheme_segments(&after).iter().enumerate() {
            let style = if selection.is_some_and(|(a, b)| (a..b).contains(&(after_start + i))) {
                ctx.style(Token::White).reversed()
            } else {
                ctx.style(Token::White)
            };
            spans.push(Span::styled(segment.clone(), style));
        }
    }
    put_line(frame, area, area.y, spans);
    let mut used = 1;

    let suggestions = wizard.filtered_suggestions();
    if !suggestions.is_empty() && area.height > 1 {
        let label = step
            .suggestions_label
            .clone()
            .unwrap_or_else(|| "or browse templates".to_string());
        put_line(
            frame,
            area,
            area.y + used,
            vec![Span::styled(
                format!("  ─── {label} ───{}", light_rule(4)),
                ctx.style(Token::Ghost),
            )],
        );
        used += 1;
        let visible = (area.height as usize).saturating_sub(used as usize);
        let selected = wizard.suggestion_index();
        let start = selected.saturating_sub(visible.saturating_sub(1));
        for (row, index) in (start..suggestions.len()).take(visible).enumerate() {
            let choice = &suggestions[index];
            let is_selected = wizard.is_browsing() && index == selected;
            ctx.row_zone(
                area,
                area.y + used + row as u16,
                ZoneKind::WizardSuggestion {
                    value: choice.value.clone(),
                },
                ZoneId::WizardSuggestion {
                    kind: wizard.kind.clone(),
                    step_id: wizard.step_id().to_string(),
                    value: choice.value.clone(),
                },
            );
            let spans = vec![
                Span::styled(
                    if is_selected {
                        format!("  {CURSOR} ")
                    } else {
                        "    ".to_string()
                    },
                    ctx.style(Token::Amber),
                ),
                Span::styled(
                    choice.label.clone(),
                    if is_selected {
                        ctx.bold(Token::White)
                    } else {
                        ctx.style(Token::Silver)
                    },
                ),
                Span::styled(
                    if choice.hint.is_empty() {
                        String::new()
                    } else {
                        format!(" ─ {}", choice.hint)
                    },
                    ctx.style(Token::Dim),
                ),
            ];
            put_line(frame, area, area.y + used + row as u16, spans);
        }
        used += visible.min(suggestions.len() - start) as u16;
    }
    used
}

fn render_textarea_field(frame: &mut Frame, area: Rect, ctx: &RenderCtx, wizard: &Wizard) -> u16 {
    // `tui-textarea-2` owns the buffer, the line numbers, the soft wrapping and
    // the cursor (ARCHITECTURE §4.8).
    let height = area.height;
    let widget_area = Rect::new(area.x, area.y, area.width, height);
    // The wheel over a textarea is forwarded to the library, which supports
    // exactly `MouseScrollUp`/`Down` and nothing else.
    ctx.zone(
        widget_area,
        area,
        ZoneKind::Scroll(ScrollRegion::WizardTextarea),
        ZoneId::ScrollRegion(ScrollRegion::WizardTextarea),
    );
    frame.render_widget(wizard.textarea(), widget_area);
    height
}

fn render_select_field(frame: &mut Frame, area: Rect, ctx: &RenderCtx, wizard: &Wizard) -> u16 {
    let options = wizard.options();
    let visible = area.height as usize;
    let selected = wizard.select_index();
    let start = selected.saturating_sub(visible.saturating_sub(1));
    // Numbers are right-aligned in a column as wide as the largest, with a
    // space after it, so `10 Content Creator` never runs together. Below ten
    // options this is the familiar `  1 `.
    let number_w = options.len().to_string().len().max(2);

    ctx.zone(
        area,
        area,
        ZoneKind::Scroll(ScrollRegion::WizardOptions),
        ZoneId::ScrollRegion(ScrollRegion::WizardOptions),
    );

    for (row, index) in (start..options.len()).take(visible).enumerate() {
        let choice = &options[index];
        let is_selected = index == selected;
        ctx.row_zone(
            area,
            area.y + row as u16,
            ZoneKind::WizardOption {
                index,
                part: RowPart::Label,
            },
            option_id(wizard, &choice.value),
        );
        let marker = if is_selected {
            format!("  {CURSOR}{} ", " ".repeat(number_w - 2))
        } else {
            format!(" {:>number_w$} ", index + 1)
        };
        // The label and then the hint are cut to the row with a visible
        // ellipsis; a one-row Paragraph would otherwise clip them silently.
        let room = (area.width as usize).saturating_sub(text_display_width(&marker));
        let label = fit(&choice.label, room);
        let room = room.saturating_sub(text_display_width(&label));
        let hint = if choice.hint.is_empty() || room == 0 {
            String::new()
        } else {
            fit(&format!(" ─ {}", choice.hint.replace('\n', " ")), room)
        };
        let spans = vec![
            Span::styled(
                marker,
                ctx.style(if is_selected {
                    Token::Amber
                } else {
                    Token::Ghost
                }),
            ),
            Span::styled(
                label,
                if is_selected {
                    ctx.bold(Token::White)
                } else {
                    ctx.style(Token::Silver)
                },
            ),
            Span::styled(hint, ctx.style(Token::Dim)),
        ];
        put_line(frame, area, area.y + row as u16, spans);
    }
    visible.min(options.len().saturating_sub(start)).max(1) as u16
}

fn render_multiselect_field(
    frame: &mut Frame,
    area: Rect,
    ctx: &RenderCtx,
    wizard: &Wizard,
) -> u16 {
    let options = wizard.options();
    let visible = area.height as usize;
    let selected = wizard.select_index();
    let start = selected.saturating_sub(visible.saturating_sub(1));

    ctx.zone(
        area,
        area,
        ZoneKind::Scroll(ScrollRegion::WizardOptions),
        ZoneId::ScrollRegion(ScrollRegion::WizardOptions),
    );

    for (row, index) in (start..options.len()).take(visible).enumerate() {
        let choice = &options[index];
        let is_cursor = index == selected;
        let checked = wizard.multi_selected().contains(&choice.value);
        let spans = vec![
            Span::styled(
                if is_cursor {
                    format!("  {CURSOR} ")
                } else {
                    "    ".to_string()
                },
                ctx.style(Token::Amber),
            ),
            Span::styled(
                if checked { "[✓] " } else { "[ ] " }.to_string(),
                ctx.style(if checked { Token::Green } else { Token::Dim }),
            ),
            Span::styled(choice.label.clone(), ctx.style(Token::Silver)),
            Span::styled(
                if choice.hint.is_empty() {
                    String::new()
                } else {
                    format!(" ─ {}", choice.hint)
                },
                ctx.style(Token::Dim),
            ),
        ];
        let row_y = area.y + row as u16;
        let checkbox_x = area.x + spans_width(&spans[..1]);
        let checkbox_w = spans_width(&spans[1..2]);
        let id = option_id(wizard, &choice.value);
        ctx.zone(
            Rect::new(checkbox_x, row_y, checkbox_w, 1),
            area,
            ZoneKind::WizardOption {
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
            ZoneKind::WizardOption {
                index,
                part: RowPart::Label,
            },
            id,
        );
        put_line(frame, area, row_y, spans);
    }
    let mut used = visible.min(options.len().saturating_sub(start)).max(1) as u16;
    let checked = wizard.multi_selected().len();
    if checked > 0 && used < area.height {
        put_line(
            frame,
            area,
            area.y + used,
            vec![Span::styled(
                format!("  └ {checked} selected"),
                ctx.style(Token::Dim),
            )],
        );
        used += 1;
    }
    used
}

/// The text field's sliding viewport: drop whole graphemes from the left until
/// the text before the cursor fits in `width` cells, then fill the remainder
/// with what follows. Widths come from [`text_display_width`], so a CJK
/// character or an emoji costs two cells.
///
/// `width` is the cell budget for the value itself; the block cursor sits in
/// the last cell, so the text before it gets `width - 1`.
pub(crate) fn viewport_start(cursor: &Cursor, width: usize) -> usize {
    let budget = width.saturating_sub(1).max(1);
    let segs = cursor.before_segs();
    let mut start = 0;
    while start < segs.len() && text_display_width(&segs[start..].concat()) > budget {
        start += 1;
    }
    start
}

fn slide_viewport(cursor: &Cursor, width: usize) -> (String, String) {
    let budget = width.saturating_sub(1).max(1);
    let before_segs = cursor.before_segs();
    let start = viewport_start(cursor, width);
    let before = before_segs[start..].concat();

    let remaining = budget.saturating_sub(text_display_width(&before));
    let mut after = String::new();
    for segment in cursor.after_segs() {
        if text_display_width(&after) + display_width(segment) > remaining {
            break;
        }
        after.push_str(segment);
    }
    (before, after)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_short_value_is_shown_whole() {
        let cursor = Cursor::new("hello");
        assert_eq!(slide_viewport(&cursor, 20), ("hello".into(), String::new()));
    }

    #[test]
    fn the_window_slides_to_keep_the_cursor_visible() {
        let cursor = Cursor::new("abcdefghij");
        let (before, after) = slide_viewport(&cursor, 5);
        assert_eq!(before, "ghij");
        assert!(after.is_empty());
    }

    #[test]
    fn text_after_the_cursor_fills_the_remaining_cells() {
        let cursor = Cursor::at("abcdefghij", 2);
        let (before, after) = slide_viewport(&cursor, 5);
        assert_eq!(before, "ab");
        assert_eq!(after, "cd");
    }

    #[test]
    fn wide_clusters_cost_two_cells_each() {
        // Four CJK characters are eight cells; a five-cell field keeps two.
        let cursor = Cursor::new("你好世界");
        let (before, after) = slide_viewport(&cursor, 5);
        assert_eq!(before, "世界");
        assert!(after.is_empty());
    }

    #[test]
    fn a_wide_cluster_is_never_split_in_half() {
        let cursor = Cursor::at("你好世界", 0);
        let (before, after) = slide_viewport(&cursor, 4);
        assert!(before.is_empty());
        // Three cells of budget fit one two-cell character, not one and a half.
        assert_eq!(after, "你");
    }

    #[test]
    fn an_empty_value_yields_empty_halves() {
        let cursor = Cursor::new("");
        assert_eq!(slide_viewport(&cursor, 10), (String::new(), String::new()));
    }
}
