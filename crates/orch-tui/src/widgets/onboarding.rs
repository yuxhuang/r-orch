//! The per-tab empty state.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::Span;

use crate::hit::{FooterAction, PseudoRow, ZoneId, ZoneKind};
use crate::onboarding::{OnboardingBoxMode, box_mode, config_for};
use crate::state::ViewId;
use crate::theme::{DIAMOND, Token};
use crate::views::{RenderCtx, put_line, spans_width};

/// Register a zone for an onboarding hint. `N` opens this view's new-entity
/// wizard and `/` enters command mode — the same two actions the footer
/// offers, so nothing new is invented here.
fn hint_zone(ctx: &RenderCtx, area: Rect, x: u16, y: u16, width: u16, key: &str) {
    let (kind, id) = match key {
        "N" => {
            let pseudo = match ctx.state.view {
                ViewId::Tasks => PseudoRow::AddTask,
                ViewId::Agents => PseudoRow::AddAgent,
                ViewId::Goals => PseudoRow::AddGoal,
                ViewId::Logs => return,
            };
            (ZoneKind::Pseudo(pseudo), ZoneId::Pseudo(pseudo))
        }
        "/" => (
            ZoneKind::FooterHint(FooterAction::CommandMode),
            ZoneId::Footer(FooterAction::CommandMode),
        ),
        _ => return,
    };
    ctx.zone(Rect::new(x, y, width, 1), area, kind, id);
}

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
            let mut spans: Vec<Span<'static>> = vec![
                Span::styled(format!("{DIAMOND} "), ctx.style(Token::Amber)),
                Span::styled(config.nudge.to_string(), ctx.style(Token::Silver)),
            ];
            if let Some(hint) = config.hints.first() {
                spans.push(Span::styled("  ", ctx.style(Token::Ghost)));
                let x = area.x + spans_width(&spans);
                spans.push(Span::styled(hint.key.to_string(), ctx.bold(Token::Amber)));
                spans.push(Span::styled(
                    format!(" {}", hint.label),
                    ctx.style(Token::Gray),
                ));
                hint_zone(
                    ctx,
                    area,
                    x,
                    area.y,
                    spans_width(&spans) + area.x - x,
                    hint.key,
                );
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
            let mut spans: Vec<Span<'static>> = Vec::new();
            for hint in config.hints {
                let x = area.x + spans_width(&spans);
                spans.push(Span::styled(hint.key.to_string(), ctx.bold(Token::Amber)));
                spans.push(Span::styled(
                    format!(" {}  ", hint.label),
                    ctx.style(Token::Gray),
                ));
                // Minus the two spaces of padding, so the gap between two
                // hints belongs to neither.
                let width = (spans_width(&spans) + area.x - x).saturating_sub(2);
                hint_zone(ctx, area, x, y, width, hint.key);
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
