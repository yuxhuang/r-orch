//! Form-wizard rendering (docs/parity/tui.md §5.1).

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::Span;

use crate::format::cap_line;
use crate::theme::{BLOCK, CHECK, CIRCLE, CIRCLE_FILLED, CURSOR, Token, light_rule};
use crate::views::{RenderCtx, put_line};
use crate::wizard::engine::{StepKind, Wizard};

/// `CMD_KEY` — `⌘` on macOS, `Ctrl` elsewhere.
pub const CMD_KEY: &str = if cfg!(target_os = "macos") {
    "⌘"
} else {
    "Ctrl"
};

pub fn render(frame: &mut Frame, area: Rect, ctx: &RenderCtx, wizard: &Wizard) {
    if area.height == 0 {
        return;
    }
    let step = wizard.step();
    let total = wizard.active_count();
    let index = wizard.active_index();
    let mut y = area.y;

    // 1. Title and step counter.
    put_line(
        frame,
        area,
        y,
        vec![
            Span::styled(wizard.title.clone(), ctx.bold(Token::Amber)),
            Span::styled(" ── ".to_string(), ctx.style(Token::Ghost)),
            Span::styled(
                format!("step {}/{}", index + 1, total),
                ctx.style(Token::Dim),
            ),
        ],
    );
    y += 1;

    // 2. Progress dots.
    let dots: Vec<Span<'static>> = (0..total)
        .map(|i| {
            if i == index {
                Span::styled(CIRCLE_FILLED, ctx.style(Token::Amber))
            } else if i < index {
                Span::styled(CHECK, ctx.style(Token::Green))
            } else {
                Span::styled(CIRCLE, ctx.style(Token::Ghost))
            }
        })
        .collect();
    put_line(frame, area, y, dots);
    y += 1;

    // 3. Step label.
    let mut label_spans = vec![Span::styled(step.label.clone(), ctx.bold(Token::White))];
    if step.required {
        label_spans.push(Span::styled(" *", ctx.style(Token::Red)));
    } else {
        let skip_key = match step.kind {
            StepKind::Textarea => format!("{CMD_KEY}+Enter/Tab"),
            _ => "Enter/Tab".to_string(),
        };
        label_spans.push(Span::styled(
            format!(" (optional, {skip_key} to skip)"),
            ctx.style(Token::Dim),
        ));
    }
    put_line(frame, area, y, label_spans);
    y += 1;

    // 4. Description.
    if let Some(description) = step.description.as_deref() {
        put_line(
            frame,
            area,
            y,
            vec![Span::styled(description.to_string(), ctx.style(Token::Dim))],
        );
        y += 1;
    }

    // 5. The field.
    let field_h = area.bottom().saturating_sub(y + 2).max(1);
    let field_area = Rect::new(area.x, y, area.width, field_h);
    let used = match step.kind {
        StepKind::Text => render_text_field(frame, field_area, ctx, wizard),
        StepKind::Textarea => render_textarea_field(frame, field_area, wizard),
        StepKind::Select => render_select_field(frame, field_area, ctx, wizard),
        StepKind::Multiselect => render_multiselect_field(frame, field_area, ctx, wizard),
    };
    y += used;

    // 6. Inline error.
    if let Some(error) = wizard.visible_error() {
        put_line(
            frame,
            area,
            y,
            vec![Span::styled(error.to_string(), ctx.style(Token::Red))],
        );
        y += 1;
        if wizard.enter_block_hint() {
            put_line(
                frame,
                area,
                y,
                vec![Span::styled(
                    crate::wizard::engine::FIX_THE_ERROR_ABOVE.to_string(),
                    ctx.style(Token::Red),
                )],
            );
        }
    }

    // 7. Hint bar.
    let mut hint = match step.kind {
        StepKind::Select => "↑↓ select  Enter/Tab confirm".to_string(),
        StepKind::Multiselect => "↑↓ move  Space toggle  Enter/Tab confirm".to_string(),
        StepKind::Textarea => {
            format!("Enter newline  {CMD_KEY}+Enter/Tab confirm  ←↑→↓ navigate")
        }
        StepKind::Text if wizard.is_browsing() => {
            "↑↓ browse  Enter select  Tab confirm  ↑ back to input".to_string()
        }
        StepKind::Text if !wizard.filtered_suggestions().is_empty() => {
            "←→ move  Enter/Tab confirm  ↓ browse templates".to_string()
        }
        StepKind::Text => "←→ move  Enter/Tab confirm".to_string(),
    };
    if wizard.paste_capable && matches!(step.kind, StepKind::Text | StepKind::Textarea) {
        hint.push_str(&format!("  {CMD_KEY}+V paste image"));
    }
    hint.push_str(if index > 0 {
        "  Esc back"
    } else {
        "  Esc cancel"
    });
    let mut hint_spans = vec![Span::styled(hint, ctx.style(Token::Gray))];
    if let Some(extra) = wizard.footer_extra.as_deref() {
        hint_spans.push(Span::styled(format!("  {extra}"), ctx.style(Token::Amber)));
    }
    put_line(frame, area, area.bottom().saturating_sub(1), hint_spans);
}

fn render_text_field(frame: &mut Frame, area: Rect, ctx: &RenderCtx, wizard: &Wizard) -> u16 {
    let step = wizard.step();
    let value = wizard.text_value();
    let max_w = (area.width as usize).saturating_sub(6).max(20);
    let mut spans = vec![Span::styled("> ", ctx.style(Token::Ghost))];
    if value.is_empty() && !step.placeholder.is_empty() {
        spans.push(Span::styled(
            cap_line(&step.placeholder, max_w),
            ctx.style(Token::Ghost),
        ));
    } else {
        let cursor = wizard.text_input().cursor();
        spans.push(Span::styled(cursor.before(), ctx.style(Token::White)));
        spans.push(Span::styled(BLOCK, ctx.style(Token::Amber)));
        spans.push(Span::styled(cursor.after(), ctx.style(Token::White)));
    }
    put_line(frame, area, area.y, spans);
    let mut used = 1;

    let suggestions = wizard.filtered_suggestions();
    if !suggestions.is_empty() {
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
        let visible = (area.height as usize)
            .saturating_sub(used as usize + 1)
            .max(2);
        let selected = wizard.suggestion_index();
        let start = selected.saturating_sub(visible.saturating_sub(1));
        for (row, index) in (start..suggestions.len()).take(visible).enumerate() {
            let choice = &suggestions[index];
            let is_selected = wizard.is_browsing() && index == selected;
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

fn render_textarea_field(frame: &mut Frame, area: Rect, wizard: &Wizard) -> u16 {
    // `tui-textarea-2` owns the buffer, the line numbers, the soft wrapping and
    // the cursor (ARCHITECTURE §4.8).
    let height = area.height.max(3);
    let widget_area = Rect::new(area.x, area.y, area.width, height);
    frame.render_widget(wizard.textarea(), widget_area);
    height
}

fn render_select_field(frame: &mut Frame, area: Rect, ctx: &RenderCtx, wizard: &Wizard) -> u16 {
    let options = wizard.options();
    let visible = (area.height as usize).saturating_sub(1).max(2);
    let selected = wizard.select_index();
    let start = selected.saturating_sub(visible.saturating_sub(1));
    let numbered = options.len() >= 10;

    for (row, index) in (start..options.len()).take(visible).enumerate() {
        let choice = &options[index];
        let is_selected = index == selected;
        let marker = if is_selected {
            format!("  {CURSOR} ")
        } else if numbered {
            format!("  {:<2}", index + 1)
        } else {
            format!("  {} ", index + 1)
        };
        let hint = choice.hint.replace('\n', " ");
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
                choice.label.clone(),
                if is_selected {
                    ctx.bold(Token::White)
                } else {
                    ctx.style(Token::Silver)
                },
            ),
            Span::styled(
                if hint.is_empty() {
                    String::new()
                } else {
                    format!(" ─ {hint}")
                },
                ctx.style(Token::Dim),
            ),
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
    let visible = (area.height as usize).saturating_sub(1).max(2);
    let selected = wizard.select_index();
    let start = selected.saturating_sub(visible.saturating_sub(1));

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
        put_line(frame, area, area.y + row as u16, spans);
    }
    let mut used = visible.min(options.len().saturating_sub(start)).max(1) as u16;
    let checked = wizard.multi_selected().len();
    if checked > 0 {
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
