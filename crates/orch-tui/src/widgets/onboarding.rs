//! The per-tab empty state (`OnboardingBox`, docs/parity/tui.md §8.3).

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::Span;

use crate::onboarding::{OnboardingBoxMode, box_mode, config_for};
use crate::state::ViewId;
use crate::theme::{DIAMOND, Token};
use crate::views::{RenderCtx, put_line};

pub fn render(frame: &mut Frame, area: Rect, ctx: &RenderCtx) {
    let state = ctx.state;
    let Some(config) = config_for(state.view) else {
        return;
    };
    let count = match state.view {
        ViewId::Goals => state.goals.len(),
        ViewId::Tasks => state.tasks.len(),
        ViewId::Agents => state.agents.len(),
        ViewId::Logs => 0,
    };

    match box_mode(count) {
        OnboardingBoxMode::Hidden => {}
        OnboardingBoxMode::Nudge => {
            let mut spans = vec![
                Span::styled(format!("{DIAMOND} "), ctx.style(Token::Amber)),
                Span::styled(config.nudge.to_string(), ctx.style(Token::Silver)),
            ];
            if let Some(hint) = config.hints.first() {
                spans.push(Span::styled("  ", ctx.style(Token::Ghost)));
                spans.push(Span::styled(hint.key.to_string(), ctx.bold(Token::Amber)));
                spans.push(Span::styled(
                    format!(" {}", hint.label),
                    ctx.style(Token::Gray),
                ));
            }
            put_line(frame, area, area.y, spans);
        }
        OnboardingBoxMode::Full => {
            let box_w = ((area.width as usize).saturating_sub(4)).min(50);
            let mut y = area.y;
            put_line(
                frame,
                area,
                y,
                vec![Span::styled(
                    format!("╭{}╮", "─".repeat(box_w)),
                    ctx.style(Token::Ghost),
                )],
            );
            y += 1;
            put_line(
                frame,
                area,
                y,
                vec![
                    Span::styled(format!("{DIAMOND} "), ctx.style(Token::Amber)),
                    Span::styled(config.title.to_string(), ctx.bold(Token::Amber)),
                ],
            );
            y += 1;
            for line in config.description {
                put_line(
                    frame,
                    area,
                    y,
                    vec![Span::styled(line.to_string(), ctx.style(Token::Silver))],
                );
                y += 1;
            }
            let mut spans = Vec::new();
            for hint in config.hints {
                spans.push(Span::styled(hint.key.to_string(), ctx.bold(Token::Amber)));
                spans.push(Span::styled(
                    format!(" {}  ", hint.label),
                    ctx.style(Token::Gray),
                ));
            }
            put_line(frame, area, y, spans);
            y += 1;
            put_line(
                frame,
                area,
                y,
                vec![Span::styled(
                    format!("╰{}╯", "─".repeat(box_w)),
                    ctx.style(Token::Ghost),
                )],
            );
        }
    }
}
