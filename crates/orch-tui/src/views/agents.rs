//! The agents list (docs/parity/tui.md §1.3).

use orch_domain::{AdapterErrorKind, AgentStatus, error_hint};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use ratatui::text::Span;

use crate::format::{cap_line, format_duration, parse_timestamp_ms};
use crate::state::AgentRow;
use crate::theme::{
    CHECK, CIRCLE, CROSS, CURSOR, DIAMOND_OUTLINE, EM_DASH, LOOP, LOZENGE, STAR, Token, light_rule,
    spinner_frame,
};
use crate::views::{RenderCtx, put_line};

/// The agent-row error hint is capped at 30 characters.
const ROW_ERROR_CAP: usize = 30;

fn status_chip(status: AgentStatus, tick: u64) -> (String, Token, Token, bool) {
    match status {
        AgentStatus::Running => (
            format!("{} ACTIVE", spinner_frame(tick)),
            Token::Green,
            Token::SuccessBg,
            true,
        ),
        AgentStatus::Idle => (
            format!("{CIRCLE} IDLE"),
            Token::Dim,
            Token::NeutralBg,
            false,
        ),
        AgentStatus::Error => (format!("{CROSS} ERROR"), Token::Red, Token::ErrorBg, true),
        AgentStatus::Disabled => (
            format!("{CIRCLE} OFF"),
            Token::Ghost,
            Token::NeutralBg,
            false,
        ),
    }
}

/// The mapped hint for a `last_error`, falling back to the raw message for an
/// unrecognised kind.
pub fn row_error_text(kind: &str, raw: &str) -> String {
    match kind.parse::<AdapterErrorKind>() {
        Ok(kind) => cap_line(error_hint(kind).message, ROW_ERROR_CAP),
        Err(()) => cap_line(raw, ROW_ERROR_CAP),
    }
}

pub fn render(frame: &mut Frame, area: Rect, ctx: &RenderCtx) {
    let state = ctx.state;
    let rows = state.agent_rows(ctx.caps.can_add_agent);
    let mut y = area.y;

    for row in rows.iter().skip(state.agent_scroll) {
        if y >= area.bottom() {
            return;
        }
        match row {
            AgentRow::TeamSection {
                name,
                count,
                lead_name,
                ..
            } => {
                let noun = if *count == 1 { "agent" } else { "agents" };
                let lead = match lead_name {
                    Some(lead) => format!(" · {STAR} {lead}"),
                    None => String::new(),
                };
                let label = format!(" {LOZENGE} {} · {count} {noun}{lead} ", name.to_uppercase());
                let rule = (area.width as usize).saturating_sub(label.chars().count() + 3);
                put_line(
                    frame,
                    area,
                    y,
                    vec![
                        Span::styled(light_rule(3), ctx.style(Token::Ghost)),
                        Span::styled(
                            label,
                            ctx.chip(Token::Amber, Token::AccentBg)
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(light_rule(rule), ctx.style(Token::Ghost)),
                    ],
                );
            }
            AgentRow::UnassignedSection { count } => {
                let noun = if *count == 1 { "agent" } else { "agents" };
                put_line(
                    frame,
                    area,
                    y,
                    vec![Span::styled(
                        format!(" {DIAMOND_OUTLINE} UNASSIGNED · {count} {noun} "),
                        ctx.chip(Token::Dim, Token::NeutralBg),
                    )],
                );
            }
            AgentRow::Agent { index, selection } => {
                render_agent_row(frame, area, ctx, y, *index, *selection)
            }
            AgentRow::AddAgent { selection } => {
                let selected = *selection == state.agent_selected;
                put_line(
                    frame,
                    area,
                    y,
                    vec![Span::styled(
                        format!(
                            "{}+ add agent...",
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
        y += 1;
    }
}

fn render_agent_row(
    frame: &mut Frame,
    area: Rect,
    ctx: &RenderCtx,
    y: u16,
    index: usize,
    selection: usize,
) {
    let state = ctx.state;
    let agent = &state.agents[index];
    let selected = selection == state.agent_selected;
    let running = agent.status == AgentStatus::Running;
    let team = state.team_of(index);
    let team_col_w = team
        .map(|t| (t.name.chars().count() + 2).min(12))
        .unwrap_or(0);
    let fixed_cols = 2 + 11 + 8 + team_col_w + 10;
    let name_w = (area.width as usize).saturating_sub(fixed_cols).max(8);

    let (chip_text, chip_fg, chip_bg, chip_bold) = status_chip(agent.status, state.tick);
    let mut spans = vec![
        Span::styled(
            if selected {
                format!("{CURSOR} ")
            } else {
                "  ".to_string()
            },
            ctx.style(Token::Amber),
        ),
        Span::styled(format!("{chip_text:<10} "), {
            let style = ctx.chip(chip_fg, chip_bg);
            if chip_bold {
                style.add_modifier(Modifier::BOLD)
            } else {
                style
            }
        }),
    ];

    if agent.autonomous == Some(true) {
        spans.push(Span::styled(format!("{LOOP} "), ctx.style(Token::Cyan)));
    }
    if team.is_some_and(|t| t.lead_agent_id == agent.id) {
        spans.push(Span::styled(format!("{STAR} "), ctx.style(Token::Amber)));
    }

    let name_token = if selected {
        Token::White
    } else if running {
        Token::Green
    } else {
        Token::Silver
    };
    spans.push(Span::styled(cap_line(&agent.name, name_w), {
        let style = ctx.style(name_token);
        if selected || running {
            style.add_modifier(Modifier::BOLD)
        } else {
            style
        }
    }));

    if running
        && let Some(task_id) = agent.current_task.as_deref()
        && let Some(task) = state.task_by_id(task_id)
    {
        spans.push(Span::styled(
            format!(" · {}", cap_line(&task.title, name_w)),
            ctx.style(Token::Dim),
        ));
    }
    if agent.status == AgentStatus::Error
        && let Some(error) = agent.last_error.as_ref()
    {
        spans.push(Span::styled(
            format!(" · {}", row_error_text(&error.kind, &error.message)),
            ctx.style(Token::Red),
        ));
    }

    spans.push(Span::styled(
        format!(" {:<8}", cap_line(&agent.adapter, 8)),
        ctx.style(Token::Dim),
    ));
    if let Some(team) = team {
        spans.push(Span::styled(
            format!(
                "{:<width$}",
                cap_line(&team.name, team_col_w),
                width = team_col_w
            ),
            ctx.style(Token::Amber),
        ));
    }

    let stats = &agent.stats;
    let running_entry = state
        .orch_state
        .running
        .values()
        .find(|entry| entry.agent_id == agent.id);
    let (time_text, time_token) = if let Some(entry) = running_entry {
        let started = parse_timestamp_ms(&entry.started_at).unwrap_or(ctx.now_ms);
        (format_duration(ctx.now_ms - started), Token::Cyan)
    } else if stats.total_runs > 0 {
        let ratio = stats.tasks_completed as f64 / stats.total_runs as f64;
        let token = if ratio >= 0.8 {
            Token::Green
        } else if ratio >= 0.5 {
            Token::Yellow
        } else {
            Token::Red
        };
        (
            format!("{}/{} {CHECK}", stats.tasks_completed, stats.total_runs),
            token,
        )
    } else {
        (EM_DASH.to_string(), Token::Dim)
    };
    spans.push(Span::styled(
        format!("{time_text:>10}"),
        ctx.style(time_token),
    ));

    put_line(frame, area, y, spans);
}
