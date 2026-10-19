//! The tasks list.

use orch_domain::{RemotePhase, RemoteRunRef, TaskStatus};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use ratatui::text::Span;

use crate::format::{cap_line, format_duration, parse_timestamp_ms};
use crate::hit::{PseudoRow, ScrollRegion, ZoneId, ZoneKind};
use crate::onboarding::OnboardingStep;
use crate::state::{EntityType, TaskRow};
use crate::theme::{
    ARROW_RIGHT, CHECK, CIRCLE, CROSS, CURSOR, DIAMOND, EM_DASH, LOZENGE, PAPERCLIP, PLUS_CIRCLE,
    TRIANGLE_DOWN, Token, heavy_rule, light_rule, priority_pips, priority_token, spinner_frame,
};
use crate::views::spans_width;
use crate::views::{RenderCtx, put_line};

const GOAL_BADGE_W: usize = 18;
const GOAL_TITLE_CAP: usize = 13;

/// The status chip: glyph + label, on its status background.
///
/// A task whose run is on a peer shows that peer's phase instead: `RUN` here
/// used to mean nothing more than "dispatched", which is exactly what an
/// operator cannot afford to misread. The alias itself is in the agent
/// column, so the chip only carries the phase.
fn status_chip(
    status: TaskStatus,
    tick: u64,
    remote: Option<&RemoteRunRef>,
) -> (String, Token, Token, bool) {
    match remote.map(|remote| remote.phase) {
        Some(RemotePhase::Dispatched) => (
            format!("{ARROW_RIGHT} SENT"),
            Token::Cyan,
            Token::ToolBg,
            false,
        ),
        Some(RemotePhase::Queued) => (
            format!("{CIRCLE} QUEUE"),
            Token::Dim,
            Token::NeutralBg,
            false,
        ),
        Some(RemotePhase::Unknown) => (
            format!("{LOZENGE} RUN?"),
            Token::Yellow,
            Token::WarnBg,
            true,
        ),
        // Running, terminal and local work all read from the task's status.
        _ => local_status_chip(status, tick),
    }
}

fn local_status_chip(status: TaskStatus, tick: u64) -> (String, Token, Token, bool) {
    match status {
        TaskStatus::InProgress => (
            format!("{} RUN", spinner_frame(tick)),
            Token::Green,
            Token::SuccessBg,
            true,
        ),
        TaskStatus::Retrying => (
            format!("{} RETRY", spinner_frame(tick)),
            Token::Yellow,
            Token::WarnBg,
            false,
        ),
        TaskStatus::Review => (
            format!("{LOZENGE} REVIEW"),
            Token::Blue,
            Token::ToolBg,
            false,
        ),
        TaskStatus::Todo => (
            format!("{CIRCLE} TODO"),
            Token::Dim,
            Token::NeutralBg,
            false,
        ),
        TaskStatus::Done => (
            format!("{CHECK} DONE"),
            Token::Green,
            Token::SuccessBg,
            false,
        ),
        TaskStatus::Failed => (format!("{CROSS} FAIL"), Token::Red, Token::ErrorBg, true),
        TaskStatus::Cancelled => ("─ OFF".to_string(), Token::Dim, Token::NeutralBg, false),
    }
}

pub fn render(frame: &mut Frame, area: Rect, ctx: &RenderCtx) {
    let state = ctx.state;

    // The welcome screen replaces the list on first run.
    if state.onboarding == OnboardingStep::Welcome {
        render_welcome(frame, area, ctx);
        return;
    }

    let rows = state.task_rows(ctx.caps.can_create_task);
    let skip = state.task_scroll;
    let mut y = area.y;

    // First within this area, so a row wins a click while the whole list
    // still answers the wheel (design §3.2).
    ctx.zone(
        area,
        area,
        ZoneKind::Scroll(ScrollRegion::MainList),
        ZoneId::ScrollRegion(ScrollRegion::MainList),
    );

    for row in rows.iter().skip(skip) {
        if y >= area.bottom() {
            break;
        }
        match row {
            TaskRow::GoalSection {
                title, total, done, ..
            } => {
                let percent = if *total > 0 {
                    ((*done as f64 / *total as f64) * 100.0).round() as i64
                } else {
                    0
                };
                let noun = if *total == 1 { "task" } else { "tasks" };
                let label = format!(
                    " {PLUS_CIRCLE} {} · {total} {noun} · {percent}% done ",
                    title.to_uppercase()
                );
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
            TaskRow::UngroupedSection { total } => {
                let noun = if *total == 1 { "task" } else { "tasks" };
                put_line(
                    frame,
                    area,
                    y,
                    vec![Span::styled(
                        format!(" {PLUS_CIRCLE} UNGROUPED · {total} {noun} "),
                        ctx.chip(Token::Dim, Token::NeutralBg),
                    )],
                );
            }
            TaskRow::Task { index, selection } => {
                render_task_row(frame, area, ctx, y, *index, *selection);
                // Identity is the task id, not the row: a refresh between a
                // press and a release must cancel the click, not move it to
                // whatever slid into this row (§3.3).
                if let Some(task) = state.tasks.get(*index) {
                    ctx.row_zone(
                        area,
                        y,
                        ZoneKind::TaskRow {
                            selection: *selection,
                        },
                        ZoneId::Entity(EntityType::Task, task.id.clone()),
                    );
                }
            }
            TaskRow::ShowAll { hidden, selection } => {
                let selected = *selection == state.task_selected;
                ctx.row_zone(
                    area,
                    y,
                    ZoneKind::Pseudo(PseudoRow::ShowAll),
                    ZoneId::Pseudo(PseudoRow::ShowAll),
                );
                put_line(
                    frame,
                    area,
                    y,
                    vec![Span::styled(
                        format!("  {TRIANGLE_DOWN} Show all ({hidden} more) — press S"),
                        if selected {
                            ctx.bold(Token::Amber)
                        } else {
                            ctx.style(Token::Dim)
                        },
                    )],
                );
            }
            TaskRow::AddTask { selection } => {
                let selected = *selection == state.task_selected;
                ctx.row_zone(
                    area,
                    y,
                    ZoneKind::Pseudo(PseudoRow::AddTask),
                    ZoneId::Pseudo(PseudoRow::AddTask),
                );
                put_line(
                    frame,
                    area,
                    y,
                    vec![Span::styled(
                        format!(
                            "{}+ add task...",
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

    // The onboarding nudge sits under the list.
    if matches!(
        state.onboarding,
        OnboardingStep::TaskCreated | OnboardingStep::RunStarted
    ) && y < area.bottom()
    {
        let (text, hint) = match state.onboarding {
            OnboardingStep::TaskCreated => ("Press R to run task", Some(("R", "run task"))),
            _ => ("Agent is running your task...", None),
        };
        let mut spans = vec![
            Span::styled(format!("{DIAMOND} "), ctx.style(Token::Amber)),
            Span::styled(text.to_string(), ctx.style(Token::Silver)),
        ];
        if let Some((key, label)) = hint {
            let x = area.x + spans_width(&spans) + 2;
            spans.push(Span::styled(
                format!("  {key} {label}"),
                ctx.style(Token::Gray),
            ));
            // Exactly what pressing `R` does, no more.
            ctx.zone(
                Rect::new(x, y, (key.len() + 1 + label.len()) as u16, 1),
                area,
                ZoneKind::Pseudo(PseudoRow::RunNudge),
                ZoneId::Pseudo(PseudoRow::RunNudge),
            );
        }
        put_line(frame, area, y, spans);
    } else if state.onboarding == OnboardingStep::Completed && y < area.bottom() {
        put_line(
            frame,
            area,
            y,
            vec![
                Span::styled("First task completed!", ctx.bold(Token::Green)),
                Span::styled(
                    "  Type / to see all commands".to_string(),
                    ctx.style(Token::Silver),
                ),
            ],
        );
    }
}

fn render_task_row(
    frame: &mut Frame,
    area: Rect,
    ctx: &RenderCtx,
    y: u16,
    index: usize,
    selection: usize,
) {
    let state = ctx.state;
    let task = &state.tasks[index];
    let selected =
        selection == state.task_selected && !matches!(state.view, crate::state::ViewId::Logs);
    let running = matches!(task.status, TaskStatus::InProgress | TaskStatus::Retrying);

    let goal_badge = state
        .resolved_goal_id(index)
        .and_then(|id| state.goal_by_id(&id).map(|g| g.title.clone()));
    let goal_badge_w = if goal_badge.is_some() {
        GOAL_BADGE_W
    } else {
        0
    };
    let fixed_cols = 2 + 10 + 4 + 14 + goal_badge_w + 7;
    let title_w = (area.width as usize).saturating_sub(fixed_cols).max(10);

    let (chip_text, chip_fg, chip_bg, chip_bold) =
        status_chip(task.status, state.tick, state.remote_run(&task.id));
    let mut spans = vec![
        Span::styled(
            if selected {
                format!("{CURSOR} ")
            } else {
                "  ".to_string()
            },
            ctx.style(Token::Amber),
        ),
        Span::styled(format!("{chip_text:<9} "), {
            let style = ctx.chip(chip_fg, chip_bg);
            if chip_bold {
                style.add_modifier(Modifier::BOLD)
            } else {
                style
            }
        }),
        Span::styled(format!("{:<4}", priority_pips(task.priority)), {
            let style = ctx.style(priority_token(task.priority));
            if task.priority <= 2 {
                style.add_modifier(Modifier::BOLD)
            } else {
                style
            }
        }),
        Span::styled(
            format!(
                "{:<width$}",
                cap_line(&task.title, title_w),
                width = title_w
            ),
            {
                let token = if selected {
                    Token::White
                } else {
                    Token::Silver
                };
                let style = ctx.style(token);
                if selected || running {
                    style.add_modifier(Modifier::BOLD)
                } else {
                    style
                }
            },
        ),
    ];

    if let Some(attachments) = task.attachments.as_ref()
        && !attachments.is_empty()
    {
        spans.push(Span::styled(
            format!(" {PAPERCLIP}{}", attachments.len()),
            ctx.style(Token::Dim),
        ));
    }

    if let Some(title) = goal_badge {
        spans.push(Span::styled(
            format!(" {PLUS_CIRCLE} {} ", cap_line(&title, GOAL_TITLE_CAP)),
            ctx.chip(Token::AmberDim, Token::AccentBg),
        ));
    }

    match task.assignee.as_deref() {
        Some(agent_id) => spans.push(Span::styled(
            format!("{:<14}", cap_line(&state.agent_name(agent_id), 13)),
            ctx.chip(Token::Green, Token::SuccessBg),
        )),
        None => spans.push(Span::styled(
            format!("{EM_DASH:<14}"),
            ctx.style(Token::Ghost),
        )),
    }

    let (time_text, time_token) = match task.status {
        TaskStatus::Done => (CHECK.to_string(), Token::Green),
        TaskStatus::Failed => (CROSS.to_string(), Token::Red),
        TaskStatus::InProgress | TaskStatus::Retrying => {
            let updated = parse_timestamp_ms(&task.updated_at).unwrap_or(ctx.now_ms);
            (format_duration(ctx.now_ms - updated), Token::Cyan)
        }
        _ => (EM_DASH.to_string(), Token::Dim),
    };
    spans.push(Span::styled(
        format!("{time_text:>7}"),
        ctx.style(time_token),
    ));

    put_line(frame, area, y, spans);
}

/// `WelcomeScreen` — a box at most 50 columns wide.
fn render_welcome(frame: &mut Frame, area: Rect, ctx: &RenderCtx) {
    let width = ((area.width as usize).saturating_sub(4)).min(50);
    let mut y = area.y;
    put_line(
        frame,
        area,
        y,
        vec![Span::styled(
            format!("╭{}╮", heavy_rule(width)),
            ctx.style(Token::Amber),
        )],
    );
    y += 1;
    put_line(
        frame,
        area,
        y,
        vec![Span::styled(
            format!("{DIAMOND} Welcome to Orch"),
            ctx.bold(Token::Amber),
        )],
    );
    y += 1;
    put_line(
        frame,
        area,
        y,
        vec![Span::styled(
            "Press N to create your first task".to_string(),
            ctx.style(Token::Silver),
        )],
    );
    y += 1;
    let spans = vec![
        Span::styled("N", ctx.bold(Token::Amber)),
        Span::styled(" new task".to_string(), ctx.style(Token::Gray)),
    ];
    ctx.zone(
        Rect::new(area.x, y, spans_width(&spans), 1),
        area,
        ZoneKind::Pseudo(PseudoRow::AddTask),
        ZoneId::Pseudo(PseudoRow::AddTask),
    );
    put_line(frame, area, y, spans);
    y += 1;
    put_line(
        frame,
        area,
        y,
        vec![Span::styled(
            format!("╰{}╯", heavy_rule(width)),
            ctx.style(Token::Amber),
        )],
    );
}
