//! The goals list.

use orch_domain::{GoalStatus, TaskStatus};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use ratatui::text::Span;

use crate::format::{cap_line, format_age_days, parse_timestamp_ms};
use crate::hit::{PseudoRow, ScrollRegion, ZoneId, ZoneKind};
use crate::state::EntityType;
use crate::theme::{BLOCK, BULLSEYE, CHECK, CROSS, CURSOR, EM_DASH, PAUSE_BARS, SHADE, Token};
use crate::views::{RenderCtx, put_line};

const PROGRESS_W: usize = 14;
const PROGRESS_CELLS: usize = 6;

fn status_chip(status: GoalStatus) -> (String, Token, Token, bool) {
    match status {
        GoalStatus::Active => (
            format!(" {BULLSEYE} ACTIVE "),
            Token::Green,
            Token::SuccessBg,
            true,
        ),
        GoalStatus::Paused => (
            format!(" {PAUSE_BARS} PAUSED "),
            Token::Dim,
            Token::NeutralBg,
            false,
        ),
        GoalStatus::Achieved => (
            format!(" {CHECK} DONE "),
            Token::Amber,
            Token::AccentBg,
            true,
        ),
        GoalStatus::Abandoned => (
            format!(" {CROSS} DROP "),
            Token::Ghost,
            Token::NeutralBg,
            false,
        ),
    }
}

pub fn render(frame: &mut Frame, area: Rect, ctx: &RenderCtx) {
    let state = ctx.state;
    let order = state.sorted_goals();
    let mut y = area.y;

    ctx.zone(
        area,
        area,
        ZoneKind::Scroll(ScrollRegion::MainList),
        ZoneId::ScrollRegion(ScrollRegion::MainList),
    );

    for (selection, goal_index) in order.iter().enumerate().skip(state.goal_scroll) {
        if y >= area.bottom() {
            return;
        }
        let goal = &state.goals[*goal_index];
        let selected = selection == state.goal_selected;
        ctx.row_zone(
            area,
            y,
            ZoneKind::GoalRow { selection },
            ZoneId::Entity(EntityType::Goal, goal.id.clone()),
        );
        let linked = state.tasks_of_goal(&goal.id);
        let done = linked
            .iter()
            .filter(|i| state.tasks[**i].status == TaskStatus::Done)
            .count();
        let progress_w = if linked.is_empty() { 0 } else { PROGRESS_W };
        let fixed_cols = 2 + 11 + progress_w + 14 + 7;
        let title_w = (area.width as usize).saturating_sub(fixed_cols).max(10);

        let (chip_text, chip_fg, chip_bg, chip_bold) = status_chip(goal.status);
        let mut spans = vec![
            Span::styled(
                if selected {
                    format!("{CURSOR} ")
                } else {
                    "  ".to_string()
                },
                ctx.style(Token::Amber),
            ),
            Span::styled(format!("{chip_text:<11}"), {
                let style = ctx.chip(chip_fg, chip_bg);
                if chip_bold {
                    style.add_modifier(Modifier::BOLD)
                } else {
                    style
                }
            }),
            Span::styled(
                format!(
                    "{:<width$}",
                    cap_line(&goal.title, title_w),
                    width = title_w
                ),
                {
                    let token = if selected {
                        Token::White
                    } else {
                        Token::Silver
                    };
                    let style = ctx.style(token);
                    if selected || goal.status == GoalStatus::Active {
                        style.add_modifier(Modifier::BOLD)
                    } else {
                        style
                    }
                },
            ),
        ];

        // The progress bar is hidden entirely at zero linked tasks — no `0/0`.
        if !linked.is_empty() {
            let filled =
                ((done as f64 / linked.len() as f64) * PROGRESS_CELLS as f64).round() as usize;
            let bar = format!(
                "{}{}",
                BLOCK.repeat(filled.min(PROGRESS_CELLS)),
                SHADE.repeat(PROGRESS_CELLS.saturating_sub(filled))
            );
            spans.push(Span::styled(bar, ctx.style(Token::Green)));
            spans.push(Span::styled(
                format!(" {done}/{}", linked.len()),
                ctx.style(Token::Dim),
            ));
        }

        match goal.assignee.as_deref() {
            Some(agent_id) => spans.push(Span::styled(
                format!("{:<14}", cap_line(&state.agent_name(agent_id), 13)),
                ctx.chip(Token::Green, Token::SuccessBg),
            )),
            None => spans.push(Span::styled(
                format!("{EM_DASH:<14}"),
                ctx.style(Token::Ghost),
            )),
        }

        let (time_text, time_token) = match goal.status {
            GoalStatus::Achieved => (CHECK.to_string(), Token::Amber),
            GoalStatus::Abandoned => (CROSS.to_string(), Token::Ghost),
            _ => {
                let created = parse_timestamp_ms(&goal.created_at).unwrap_or(ctx.now_ms);
                (format_age_days(created, ctx.now_ms), Token::Dim)
            }
        };
        spans.push(Span::styled(
            format!("{time_text:>7}"),
            ctx.style(time_token),
        ));

        put_line(frame, area, y, spans);
        y += 1;
    }

    // `+ add goal...` at selection index `goals.len()`.
    if ctx.caps.can_create_goal && y < area.bottom() {
        let selected = state.goal_selected == order.len();
        ctx.row_zone(
            area,
            y,
            ZoneKind::Pseudo(PseudoRow::AddGoal),
            ZoneId::Pseudo(PseudoRow::AddGoal),
        );
        put_line(
            frame,
            area,
            y,
            vec![Span::styled(
                format!(
                    "{}+ add goal...",
                    if selected {
                        format!("  {CURSOR} ")
                    } else {
                        "    ".to_string()
                    }
                ),
                if selected {
                    ctx.style(Token::Amber)
                } else {
                    ctx.style(Token::Dim)
                },
            )],
        );
    }
}
