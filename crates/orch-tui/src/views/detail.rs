//! The four detail panels (docs/parity/tui.md §1.1–§1.4).

use orch_domain::{AdapterErrorKind, TaskStatus, error_hint};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Widget};

use crate::format::{MAX_PANEL_TEXT, cap_line, cap_text, format_duration, parse_timestamp_ms};
use crate::messages::MsgType;
use crate::state::{DetailPanel, ViewId};
use crate::theme::{
    EM_DASH, HEAVY_RULE, LOOP, Token, agent_status_token, goal_status_token, light_rule,
    task_status_token,
};
use crate::views::{RenderCtx, put_line, resize_hint, section_label};

/// `col1Width` in the goal and agent panels.
const COL1_W: usize = 24;

pub fn render(frame: &mut Frame, area: Rect, ctx: &RenderCtx) {
    let state = ctx.state;
    match (state.detail, state.view) {
        (DetailPanel::Log, _) => render_log_detail(frame, area, ctx),
        (DetailPanel::Entity, ViewId::Tasks) => render_task_detail(frame, area, ctx),
        (DetailPanel::Entity, ViewId::Agents) => render_agent_detail(frame, area, ctx),
        (DetailPanel::Entity, ViewId::Goals) => render_goal_detail(frame, area, ctx),
        _ => {}
    }
}

/// `DetailSectionLabel` — a chip, the entity name and the resize hint.
fn detail_label(
    frame: &mut Frame,
    area: Rect,
    ctx: &RenderCtx,
    chip: &str,
    chip_fg: Token,
    chip_bg: Token,
    title: &str,
) {
    let hint = resize_hint(ctx.state.detail_maximized);
    let title_w = (area.width as usize)
        .saturating_sub(chip.chars().count() + hint.chars().count() + 10)
        .max(4);
    let spans = vec![
        Span::styled(HEAVY_RULE.to_string().repeat(3), ctx.style(Token::Ghost)),
        Span::styled(
            chip.to_string(),
            ctx.chip(chip_fg, chip_bg).add_modifier(Modifier::BOLD),
        ),
        Span::styled(format!("{HEAVY_RULE} "), ctx.style(Token::Ghost)),
        Span::styled(cap_line(title, title_w), ctx.bold(Token::White)),
        Span::styled(format!("  {hint}"), ctx.style(Token::Gray)),
    ];
    put_line(frame, area, area.y, spans);
}

/// A two-column key/value row.
fn kv_row<'a>(
    ctx: &RenderCtx,
    key1: &'a str,
    value1: Span<'a>,
    key2: Option<(&'a str, Span<'a>)>,
) -> Vec<Span<'a>> {
    let mut spans = vec![
        Span::styled(format!("{key1:<10}"), ctx.style(Token::Dim)),
        value1,
    ];
    if let Some((key2, value2)) = key2 {
        let used = 10 + 1;
        spans.push(Span::raw(" ".repeat(COL1_W.saturating_sub(used))));
        spans.push(Span::styled(format!("{key2:<10}"), ctx.style(Token::Dim)));
        spans.push(value2);
    }
    spans
}

// ── Task ───────────────────────────────────────────────────────────────────

fn render_task_detail(frame: &mut Frame, area: Rect, ctx: &RenderCtx) {
    let state = ctx.state;
    let Some(task) = state.selected_task() else {
        return;
    };
    detail_label(
        frame,
        area,
        ctx,
        " DETAIL ",
        Token::Amber,
        Token::AccentBg,
        &task.title,
    );

    let height = area.height.saturating_sub(1) as usize;
    let width = area.width as usize;
    let task_logs: Vec<usize> = (0..state.messages.len())
        .filter(|i| state.messages[*i].task_id.as_deref() == Some(task.id.as_str()))
        .collect();
    let summary = task
        .proof
        .as_ref()
        .and_then(|p| p.agent_summary.as_deref())
        .filter(|s| !s.is_empty());
    let attachments = task.attachments.as_deref().unwrap_or(&[]);
    let description_lines: Vec<&str> = if task.description.is_empty() {
        Vec::new()
    } else {
        task.description.lines().collect()
    };
    let files = task
        .proof
        .as_ref()
        .map(|p| p.files_changed.len())
        .unwrap_or(0);

    // Row budget (ported verbatim).
    let mut used_rows = 3usize;
    if !attachments.is_empty() {
        used_rows += 2 + attachments.len();
    }
    let desc_cap = (((height as f64 - 10.0) * 0.3).ceil() as i64).max(1) as usize;
    if !description_lines.is_empty() {
        used_rows += 1 + description_lines.len().min(desc_cap);
    } else if summary.is_none() {
        used_rows += 2;
    }
    if summary.is_some() {
        used_rows += 2;
    }
    if files > 0 {
        used_rows += 1;
    }
    if !task_logs.is_empty() {
        used_rows += 2;
    }
    let remaining = height.saturating_sub(used_rows);
    let (summary_max, log_max) = match (summary.is_some(), !task_logs.is_empty()) {
        (true, true) => {
            let s = ((remaining as f64 * 0.4).floor() as usize).max(1);
            (s, remaining.saturating_sub(s).max(1))
        }
        (true, false) => (remaining, 0),
        (false, true) => (0, remaining),
        (false, false) => (0, 0),
    };

    let mut y = area.y + 1;
    let assignee = match task.assignee.as_deref() {
        Some(id) => Span::styled(state.agent_name(id), ctx.style(Token::Green)),
        None => Span::styled(EM_DASH.to_string(), ctx.style(Token::Dim)),
    };
    put_line(
        frame,
        area,
        y,
        kv_row(
            ctx,
            "status",
            Span::styled(
                task.status.to_string(),
                ctx.style(task_status_token(task.status)),
            ),
            Some(("assignee", assignee)),
        ),
    );
    y += 1;
    put_line(
        frame,
        area,
        y,
        kv_row(
            ctx,
            "priority",
            Span::styled(format!("P{}", task.priority), {
                let style = ctx.style(crate::theme::priority_token(task.priority));
                if task.priority <= 2 {
                    style.add_modifier(Modifier::BOLD)
                } else {
                    style
                }
            }),
            Some((
                "attempts",
                Span::styled(
                    format!("{}/{}", task.attempts, task.max_attempts),
                    ctx.style(Token::Silver),
                ),
            )),
        ),
    );
    y += 1;
    let labels = if task.labels.is_empty() {
        Span::styled(EM_DASH.to_string(), ctx.style(Token::Dim))
    } else {
        Span::styled(task.labels.join(", "), ctx.style(Token::Purple))
    };
    let depends = if task.depends_on.is_empty() {
        Span::styled(EM_DASH.to_string(), ctx.style(Token::Dim))
    } else {
        let titles: Vec<String> = task
            .depends_on
            .iter()
            .map(|id| {
                state
                    .task_by_id(id)
                    .map(|t| t.title.clone())
                    .unwrap_or_else(|| id.clone())
            })
            .collect();
        Span::styled(titles.join(", "), ctx.style(Token::Dim))
    };
    put_line(
        frame,
        area,
        y,
        kv_row(ctx, "labels", labels, Some(("depends", depends))),
    );
    y += 1;

    if !attachments.is_empty() {
        y += 1;
        divider(
            frame,
            area,
            ctx,
            y,
            &format!("attachments ({})", attachments.len()),
        );
        y += 1;
        for path in attachments {
            put_line(
                frame,
                area,
                y,
                vec![Span::styled(path.clone(), ctx.style(Token::Cyan))],
            );
            y += 1;
        }
    }

    if !description_lines.is_empty() {
        y += 1;
        for line in description_lines.iter().take(desc_cap) {
            put_line(
                frame,
                area,
                y,
                vec![Span::styled(
                    cap_line(line, width.saturating_sub(8)),
                    ctx.style(Token::Silver),
                )],
            );
            y += 1;
        }
    } else if summary.is_none() {
        y += 1;
        put_line(
            frame,
            area,
            y,
            vec![Span::styled(
                "No description.".to_string(),
                ctx.style(Token::Dim),
            )],
        );
        y += 1;
    }

    if let Some(summary) = summary {
        y += 1;
        divider(frame, area, ctx, y, "result");
        y += 1;
        for line in summary.lines().take(summary_max.max(1)) {
            put_line(
                frame,
                area,
                y,
                vec![Span::styled(
                    cap_line(line, width.saturating_sub(8)),
                    ctx.style(Token::White),
                )],
            );
            y += 1;
        }
    }

    if files > 0 {
        let branch = task
            .proof
            .as_ref()
            .and_then(|p| p.branch.as_deref())
            .map(|b| format!(" · {b}"))
            .unwrap_or_default();
        put_line(
            frame,
            area,
            y,
            vec![Span::styled(
                format!("files {files} changed{branch}"),
                ctx.style(Token::Dim),
            )],
        );
        y += 1;
    }

    if !task_logs.is_empty() && log_max > 0 {
        y += 1;
        divider(frame, area, ctx, y, "activity");
        y += 1;
        let text_w = width.saturating_sub(12).max(10);
        for index in task_logs.iter().rev().take(log_max).rev() {
            let message = &state.messages[*index];
            let token = match message.msg_type {
                MsgType::Tool => Token::Cyan,
                MsgType::File => Token::Purple,
                MsgType::Error => Token::Red,
                MsgType::Lifecycle => Token::Green,
                MsgType::System => Token::Dim,
                _ => message.color,
            };
            put_line(
                frame,
                area,
                y,
                vec![
                    Span::styled(
                        format!(
                            "{} {} ",
                            crate::format::relative_time(message.ts_ms, ctx.now_ms),
                            message.msg_type.icon()
                        ),
                        ctx.style(Token::Ghost),
                    ),
                    Span::styled(cap_line(&message.text, text_w), ctx.style(token)),
                ],
            );
            y += 1;
        }
    }
}

// ── Agent ──────────────────────────────────────────────────────────────────

fn render_agent_detail(frame: &mut Frame, area: Rect, ctx: &RenderCtx) {
    let state = ctx.state;
    let Some(agent) = state.selected_agent() else {
        return;
    };
    detail_label(
        frame,
        area,
        ctx,
        " AGENT ",
        Token::Green,
        Token::SuccessBg,
        &agent.name,
    );
    let mut y = area.y + 1;
    let is_shell = agent.adapter == "shell";

    put_line(
        frame,
        area,
        y,
        kv_row(
            ctx,
            "status",
            Span::styled(
                agent.status.to_string(),
                ctx.style(agent_status_token(agent.status)),
            ),
            Some((
                "adapter",
                Span::styled(agent.adapter.clone(), ctx.style(Token::Cyan)),
            )),
        ),
    );
    y += 1;

    let current_task = agent
        .current_task
        .as_deref()
        .and_then(|id| state.task_by_id(id))
        .map(|t| t.title.clone())
        .unwrap_or_else(|| EM_DASH.to_string());
    if is_shell {
        // Shell agents swap the two: task first, then the command.
        put_line(
            frame,
            area,
            y,
            kv_row(
                ctx,
                "task",
                Span::styled(current_task, ctx.style(Token::Silver)),
                Some((
                    "command",
                    Span::styled(
                        agent
                            .config
                            .command
                            .clone()
                            .unwrap_or_else(|| EM_DASH.to_string()),
                        ctx.style(Token::Cyan),
                    ),
                )),
            ),
        );
    } else {
        put_line(
            frame,
            area,
            y,
            kv_row(
                ctx,
                "model",
                Span::styled(
                    agent
                        .config
                        .model
                        .clone()
                        .unwrap_or_else(|| EM_DASH.to_string()),
                    ctx.style(Token::Silver),
                ),
                Some(("task", Span::styled(current_task, ctx.style(Token::Silver)))),
            ),
        );
    }
    y += 1;

    let stats = &agent.stats;
    put_line(
        frame,
        area,
        y,
        kv_row(
            ctx,
            "runs",
            Span::styled(
                format!(
                    "{} ({}/{})",
                    stats.total_runs, stats.tasks_completed, stats.tasks_failed
                ),
                ctx.style(if stats.tasks_failed > 0 {
                    Token::Red
                } else {
                    Token::Green
                }),
            ),
            Some((
                "team",
                match state
                    .teams
                    .iter()
                    .find(|t| t.members.iter().any(|m| m.agent_id == agent.id))
                {
                    Some(team) => Span::styled(team.name.clone(), ctx.style(Token::Amber)),
                    None => Span::styled(EM_DASH.to_string(), ctx.style(Token::Dim)),
                },
            )),
        ),
    );
    y += 1;

    if agent.autonomous == Some(true) {
        put_line(
            frame,
            area,
            y,
            kv_row(
                ctx,
                "auto",
                Span::styled(format!("{LOOP} ON"), ctx.style(Token::Cyan)),
                None,
            ),
        );
        y += 1;
    }

    if let Some(skills) = agent.config.skills.as_ref()
        && !skills.is_empty()
    {
        put_line(
            frame,
            area,
            y,
            kv_row(
                ctx,
                "skills",
                Span::styled(cap_line(&skills.join(", "), 500), ctx.style(Token::Cyan)),
                None,
            ),
        );
        y += 1;
    }

    if let Some(error) = agent.last_error.as_ref() {
        let kind = error.kind.parse::<AdapterErrorKind>().ok();
        let hint = kind.map(error_hint);
        put_line(
            frame,
            area,
            y,
            vec![Span::styled(
                format!("╭{}╮", light_rule((area.width as usize).saturating_sub(4))),
                ctx.style(Token::Red),
            )],
        );
        y += 1;
        put_line(
            frame,
            area,
            y,
            vec![Span::styled("⚠ Error".to_string(), ctx.bold(Token::Red))],
        );
        y += 1;
        if let Some(hint) = hint {
            put_line(
                frame,
                area,
                y,
                vec![Span::styled(
                    hint.message.to_string(),
                    ctx.style(Token::White),
                )],
            );
            y += 1;
            put_line(
                frame,
                area,
                y,
                vec![Span::styled(hint.fix.to_string(), ctx.style(Token::Cyan))],
            );
            y += 1;
            if hint.doctor_hint == Some(true) {
                put_line(
                    frame,
                    area,
                    y,
                    vec![Span::styled(
                        "Diagnostics: orch doctor".to_string(),
                        ctx.style(Token::Yellow),
                    )],
                );
                y += 1;
            }
        }
        // The raw message shows only when the hint is missing or `unknown`.
        if hint.is_none() || kind == Some(AdapterErrorKind::Unknown) {
            put_line(
                frame,
                area,
                y,
                vec![Span::styled(
                    cap_line(&error.message, 120),
                    ctx.style(Token::Dim),
                )],
            );
            y += 1;
        }
        if let Some(ts) = parse_timestamp_ms(&error.timestamp) {
            put_line(
                frame,
                area,
                y,
                vec![Span::styled(
                    format!("{} ago", format_duration(ctx.now_ms - ts)),
                    ctx.style(Token::Dim),
                )],
            );
            y += 1;
        }
        put_line(
            frame,
            area,
            y,
            vec![Span::styled(
                format!("╰{}╯", light_rule((area.width as usize).saturating_sub(4))),
                ctx.style(Token::Red),
            )],
        );
        y += 1;
    }

    y += 1;
    let role_cap = (area.height as usize)
        .saturating_sub(if agent.last_error.is_some() { 10 } else { 4 })
        .max(1);
    match agent.role.as_deref().filter(|r| !r.is_empty()) {
        Some(role) => {
            for line in role.lines().take(role_cap) {
                put_line(
                    frame,
                    area,
                    y,
                    vec![Span::styled(cap_line(line, 500), ctx.style(Token::Silver))],
                );
                y += 1;
            }
        }
        None => put_line(
            frame,
            area,
            y,
            vec![Span::styled(
                "No role description.".to_string(),
                ctx.style(Token::Dim),
            )],
        ),
    }
}

// ── Goal ───────────────────────────────────────────────────────────────────

fn render_goal_detail(frame: &mut Frame, area: Rect, ctx: &RenderCtx) {
    let state = ctx.state;
    let Some(goal) = state.selected_goal() else {
        return;
    };
    let hint = resize_hint(state.detail_maximized);
    put_line(
        frame,
        area,
        area.y,
        vec![
            Span::styled(HEAVY_RULE.to_string().repeat(3), ctx.style(Token::Ghost)),
            Span::styled(
                format!(" GOAL: {} ", cap_line(&goal.title, 40)),
                ctx.chip(Token::Amber, Token::AccentBg)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!("  {hint}"), ctx.style(Token::Gray)),
        ],
    );

    // Built as a virtual line list so scrolling never rebuilds the rows.
    let width = area.width as usize;
    let mut lines: Vec<Line<'static>> = Vec::new();
    let assignee = match goal.assignee.as_deref() {
        Some(id) => Span::styled(state.agent_name(id), ctx.style(Token::Green)),
        None => Span::styled(EM_DASH.to_string(), ctx.style(Token::Dim)),
    };
    lines.push(Line::from(kv_row(
        ctx,
        "status",
        Span::styled(
            goal.status.to_string().to_uppercase(),
            ctx.style(goal_status_token(goal.status))
                .add_modifier(Modifier::BOLD),
        ),
        Some(("assignee", assignee)),
    )));
    lines.push(Line::from(kv_row(
        ctx,
        "id",
        Span::styled(goal.id.clone(), ctx.style(Token::Dim)),
        Some((
            "created",
            Span::styled(
                goal.created_at.chars().take(10).collect::<String>(),
                ctx.style(Token::Silver),
            ),
        )),
    )));
    if let Some(updated) = goal.updated_at.as_deref()
        && updated != goal.created_at
    {
        lines.push(Line::from(kv_row(
            ctx,
            "Updated",
            Span::styled(
                updated.chars().take(10).collect::<String>(),
                ctx.style(Token::Silver),
            ),
            None,
        )));
    }

    let linked = state.tasks_of_goal(&goal.id);
    let mut breakdown: Vec<String> = Vec::new();
    for status in TaskStatus::ALL {
        let count = linked
            .iter()
            .filter(|i| state.tasks[**i].status == status)
            .count();
        if count > 0 {
            breakdown.push(format!("{count} {}", status.as_str()));
        }
    }
    lines.push(Line::from(kv_row(
        ctx,
        "tasks",
        Span::styled(linked.len().to_string(), ctx.style(Token::Cyan)),
        Some((
            "",
            Span::styled(breakdown.join(" · "), ctx.style(Token::Dim)),
        )),
    )));

    lines.push(Line::from(""));
    let description = cap_text(&goal.description, MAX_PANEL_TEXT);
    if description.is_empty() {
        lines.push(Line::from(Span::styled(
            "No description.".to_string(),
            ctx.style(Token::Dim),
        )));
    } else {
        let wrap_w = width.saturating_sub(6).max(20);
        for line in description.lines() {
            for chunk in wrap(line, wrap_w) {
                lines.push(Line::from(Span::styled(chunk, ctx.style(Token::Silver))));
            }
        }
    }

    if let Some(report) = state.goal_progress_report.as_deref() {
        lines.push(Line::from(""));
        lines.push(divider_line(ctx, "progress", width));
        // No line cap — the full report renders.
        for line in cap_text(report, MAX_PANEL_TEXT).lines() {
            lines.push(Line::from(Span::styled(
                line.to_string(),
                ctx.style(Token::White),
            )));
        }
    }

    if !linked.is_empty() {
        lines.push(Line::from(""));
        lines.push(divider_line(
            ctx,
            &format!("tasks ({})", linked.len()),
            width,
        ));
        let title_w = width.saturating_sub(22).max(10);
        for index in &linked {
            let task = &state.tasks[*index];
            lines.push(Line::from(vec![
                Span::styled(
                    format!("{:<12}", task.status.as_str()),
                    ctx.style(task_status_token(task.status)),
                ),
                Span::styled(
                    task.title.chars().take(title_w).collect::<String>(),
                    ctx.style(Token::Silver),
                ),
            ]));
        }
    }

    let height = area.height.saturating_sub(1) as usize;
    let max_scroll = lines.len().saturating_sub(height);
    let offset = state.goal_detail_scroll.min(max_scroll);
    let remaining = lines.len().saturating_sub(offset + height);
    let rows = if remaining > 0 {
        height.saturating_sub(1)
    } else {
        height
    };

    for (row, line) in lines.iter().skip(offset).take(rows).enumerate() {
        Paragraph::new(line.clone()).render(
            Rect::new(area.x, area.y + 1 + row as u16, area.width, 1),
            frame.buffer_mut(),
        );
    }
    if remaining > 0 {
        put_line(
            frame,
            area,
            area.y + 1 + rows as u16,
            vec![Span::styled(
                format!("  ↓ {remaining} more — ↑↓ to scroll"),
                ctx.style(Token::Ghost),
            )],
        );
    }
}

fn wrap(text: &str, width: usize) -> Vec<String> {
    if text.chars().count() <= width {
        return vec![text.to_string()];
    }
    let chars: Vec<char> = text.chars().collect();
    chars
        .chunks(width.max(1))
        .map(|chunk| chunk.iter().collect())
        .collect()
}

// ── Log ────────────────────────────────────────────────────────────────────

fn render_log_detail(frame: &mut Frame, area: Rect, ctx: &RenderCtx) {
    let state = ctx.state;
    let filtered = state.filtered_messages();
    let Some(index) = usize::try_from(state.log_selected_index)
        .ok()
        .and_then(|i| filtered.get(i))
    else {
        return;
    };
    let message = &state.messages[*index];
    section_label(
        frame,
        Rect::new(area.x, area.y, area.width, 1),
        ctx,
        "LOG",
        None,
    );

    let max_w = (area.width as usize).saturating_sub(6).max(4);
    let mut y = area.y + 1;
    let agent = message
        .agent_id
        .as_deref()
        .map(|id| state.agent_name(id))
        .unwrap_or_else(|| EM_DASH.to_string());
    let task = message
        .task_id
        .as_deref()
        .and_then(|id| state.task_by_id(id))
        .map(|t| format!(" │ #{}", t.title.chars().take(30).collect::<String>()))
        .unwrap_or_default();
    put_line(
        frame,
        area,
        y,
        vec![Span::styled(
            format!(
                "{} │ {agent} │ {} {}{task}",
                crate::format::relative_time(message.ts_ms, ctx.now_ms),
                message.msg_type.icon(),
                message.msg_type.as_str()
            ),
            ctx.style(Token::Dim),
        )],
    );
    y += 1;
    put_line(
        frame,
        area,
        y,
        vec![Span::styled(
            message.text.chars().take(max_w).collect::<String>(),
            ctx.bold(message.color),
        )],
    );
    y += 1;
    put_line(
        frame,
        area,
        y,
        vec![Span::styled(light_rule(max_w), ctx.style(Token::Ghost))],
    );
    y += 1;

    let body = message
        .detail
        .clone()
        .unwrap_or_else(|| message.text.clone());
    let body_rows = (area.height as usize).saturating_sub(4).max(1);
    match serde_json::from_str::<serde_json::Value>(&body) {
        Ok(parsed) => {
            let pretty = serde_json::to_string_pretty(&parsed).unwrap_or(body);
            for (row, line) in pretty.lines().take(body_rows).enumerate() {
                let token = if line.contains('"') {
                    Token::Cyan
                } else if line.trim_start().starts_with(['}', ']']) {
                    Token::Ghost
                } else {
                    Token::Silver
                };
                put_line(
                    frame,
                    area,
                    y + row as u16,
                    vec![
                        Span::styled(format!("{:<3}", row + 1), ctx.style(Token::Ghost)),
                        Span::styled(
                            line.chars()
                                .take(max_w.saturating_sub(4))
                                .collect::<String>(),
                            ctx.style(token),
                        ),
                    ],
                );
            }
        }
        Err(_) => {
            for (row, line) in body.lines().take(body_rows).enumerate() {
                let token = if line.starts_with("error") || line.starts_with("Error") {
                    Token::Red
                } else {
                    Token::Silver
                };
                put_line(
                    frame,
                    area,
                    y + row as u16,
                    vec![Span::styled(
                        line.chars().take(max_w).collect::<String>(),
                        ctx.style(token),
                    )],
                );
            }
        }
    }
}

// ── Shared ─────────────────────────────────────────────────────────────────

fn divider(frame: &mut Frame, area: Rect, ctx: &RenderCtx, y: u16, label: &str) {
    put_line(
        frame,
        area,
        y,
        divider_line(ctx, label, area.width as usize).spans,
    );
}

fn divider_line(ctx: &RenderCtx, label: &str, width: usize) -> Line<'static> {
    let rule = width.saturating_sub(label.chars().count() + 6);
    Line::from(vec![
        Span::styled(light_rule(3), ctx.style(Token::Ghost)),
        Span::styled(format!(" {label} "), ctx.style(Token::Dim)),
        Span::styled(light_rule(rule), ctx.style(Token::Ghost)),
    ])
}
