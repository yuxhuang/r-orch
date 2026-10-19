//! The tasks list.

use orch_domain::{RemotePhase, RemoteRunRef, TaskStatus};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use ratatui::text::Span;

use crate::format::{cap_display, cap_line, format_duration, format_started};
use crate::hit::{PseudoRow, ScrollRegion, ZoneId, ZoneKind};
use crate::onboarding::OnboardingStep;
use crate::state::{EntityType, SortColumn, TaskRow, TaskSortField, task_status_word};
use crate::text::text_display_width;
use crate::theme::{
    ARROW_RIGHT, CHECK, CIRCLE, CROSS, CURSOR, DIAMOND, EM_DASH, LOZENGE, PAPERCLIP, PLUS_CIRCLE,
    TRIANGLE_DOWN, Token, heavy_rule, light_rule, priority_pips, priority_token, spinner_frame,
};
use crate::views::spans_width;
use crate::views::{
    HeaderCell, RenderCtx, cell, cell_right, put_line, render_header_row, sanitize,
};

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
            format!("{} {}", spinner_frame(tick), task_status_word(status)),
            Token::Green,
            Token::SuccessBg,
            true,
        ),
        TaskStatus::Retrying => (
            format!("{} {}", spinner_frame(tick), task_status_word(status)),
            Token::Yellow,
            Token::WarnBg,
            false,
        ),
        TaskStatus::Review => (
            format!("{LOZENGE} {}", task_status_word(status)),
            Token::Blue,
            Token::ToolBg,
            false,
        ),
        TaskStatus::Todo => (
            format!("{CIRCLE} {}", task_status_word(status)),
            Token::Dim,
            Token::NeutralBg,
            false,
        ),
        TaskStatus::Done => (
            format!("{CHECK} {}", task_status_word(status)),
            Token::Green,
            Token::SuccessBg,
            false,
        ),
        TaskStatus::Failed => (
            format!("{CROSS} {}", task_status_word(status)),
            Token::Red,
            Token::ErrorBg,
            true,
        ),
        TaskStatus::Cancelled => (
            format!("─ {}", task_status_word(status)),
            Token::Dim,
            Token::NeutralBg,
            false,
        ),
    }
}

pub fn render(frame: &mut Frame, area: Rect, ctx: &RenderCtx) {
    let state = ctx.state;

    // The welcome screen replaces the list on first run.
    if state.onboarding == OnboardingStep::Welcome {
        render_welcome(frame, area, ctx);
        return;
    }

    let cols = TaskColumns::for_width(area.width as usize);
    render_header_row(
        frame,
        area,
        area.y,
        ctx,
        &cols.header(),
        (
            SortColumn::Task(state.task_sort.field),
            state.task_sort.descending,
        ),
    );
    let area = Rect::new(
        area.x,
        area.y.saturating_add(1),
        area.width,
        area.height.saturating_sub(1),
    );
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
                title,
                total,
                done,
                matched,
                ..
            } => {
                let percent = if *total > 0 {
                    ((*done as f64 / *total as f64) * 100.0).round() as i64
                } else {
                    0
                };
                let noun = if *total == 1 { "task" } else { "tasks" };
                // Filtered, the count says how many of the goal's tasks are
                // listed (`1/2 tasks`); the percentage stays the goal's.
                let count = match matched {
                    Some(matched) => format!("{matched}/{total}"),
                    None => total.to_string(),
                };
                let label = format!(
                    " {PLUS_CIRCLE} {} · {count} {noun} · {percent}% done ",
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
                render_task_row(frame, area, ctx, y, cols, *index, *selection);
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
    if y < area.bottom() && super::render_no_match_at(frame, area, y, ctx) {
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

// ── Column layout ─────────────────────────────────────────────────────────
//
//   cursor │ status chip │ pri │ title │ goal │ assignee │ started │ elapsed*
//
// One width per column per frame, shared by the header and every row. Title
// is elastic and holds the attachment mark; on a narrow terminal the optional
// columns drop lowest-priority first (goal, assignee, started).

/// Cursor (2) + status chip and its gap (10) + priority pips (4).
const LEAD_W: usize = 2 + 10 + 4;
const TITLE_MIN: usize = 10;
const ASSIGNEE_W: usize = 14;
const STARTED_W: usize = 8;
/// `ELAPSED*↓` fits exactly.
const ELAPSED_W: usize = 9;

/// Column widths for one frame's task rows; `0` hides a column. Each optional
/// column is preceded by a one-cell gap that is not counted here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TaskColumns {
    pub title: usize,
    pub goal: usize,
    pub assignee: usize,
    pub started: usize,
    pub elapsed: usize,
}

impl TaskColumns {
    pub fn for_width(width: usize) -> Self {
        let mut avail = width.saturating_sub(LEAD_W);
        let title = TITLE_MIN.min(avail);
        avail -= title;
        let mut take = |w: usize| {
            if avail > w {
                avail -= w + 1;
                w
            } else {
                0
            }
        };
        let elapsed = take(ELAPSED_W);
        let started = take(STARTED_W);
        let assignee = take(ASSIGNEE_W);
        let goal = take(GOAL_BADGE_W);
        TaskColumns {
            title: title + avail,
            goal,
            assignee,
            started,
            elapsed,
        }
    }

    fn header(self) -> Vec<HeaderCell> {
        let task = SortColumn::Task;
        let gap = |width: usize| HeaderCell::gap(usize::from(width > 0));
        vec![
            HeaderCell::gap(2),
            HeaderCell::sort("STATUS", 10, task(TaskSortField::Status)),
            HeaderCell::sort("PRI", 4, task(TaskSortField::Priority)),
            HeaderCell::sort("TITLE", self.title, task(TaskSortField::Title)),
            gap(self.goal),
            HeaderCell::sort("GOAL", self.goal, task(TaskSortField::Goal)),
            gap(self.assignee),
            HeaderCell::sort("ASSIGNEE", self.assignee, task(TaskSortField::Assignee)),
            gap(self.started),
            HeaderCell::sort("STARTED", self.started, task(TaskSortField::Started)).right(),
            gap(self.elapsed),
            HeaderCell::sort("ELAPSED*", self.elapsed, task(TaskSortField::Elapsed)).right(),
        ]
    }
}

/// `text` as a chip exactly `width` cells wide: the chip colours only the
/// text, and plain padding fills the rest of the column.
fn chip_cell(
    ctx: &RenderCtx,
    text: &str,
    width: usize,
    fg: Token,
    bg: Token,
) -> Vec<Span<'static>> {
    let text = cap_display(&sanitize(text), width);
    let pad = width.saturating_sub(text_display_width(&text));
    vec![
        Span::styled(text, ctx.chip(fg, bg)),
        Span::raw(" ".repeat(pad)),
    ]
}

fn render_task_row(
    frame: &mut Frame,
    area: Rect,
    ctx: &RenderCtx,
    y: u16,
    cols: TaskColumns,
    index: usize,
    selection: usize,
) {
    let state = ctx.state;
    let task = &state.tasks[index];
    let selected =
        selection == state.task_selected && !matches!(state.view, crate::state::ViewId::Logs);
    let running = matches!(task.status, TaskStatus::InProgress | TaskStatus::Retrying);

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
        Span::styled(cell(&chip_text, 9), {
            let style = ctx.chip(chip_fg, chip_bg);
            if chip_bold {
                style.add_modifier(Modifier::BOLD)
            } else {
                style
            }
        }),
        Span::raw(" "),
        Span::styled(format!("{:<4}", priority_pips(task.priority)), {
            let style = ctx.style(priority_token(task.priority));
            if task.priority <= 2 {
                style.add_modifier(Modifier::BOLD)
            } else {
                style
            }
        }),
    ];

    // The attachment mark lives inside the title column, so it never pushes
    // the columns after it out of line.
    let mark = match task.attachments.as_ref() {
        Some(attachments) if !attachments.is_empty() => {
            format!(" {PAPERCLIP}{}", attachments.len())
        }
        _ => String::new(),
    };
    let mark_w = text_display_width(&mark).min(cols.title.saturating_sub(TITLE_MIN));
    spans.push(Span::styled(cell(&task.title, cols.title - mark_w), {
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
    }));
    if mark_w > 0 {
        spans.push(Span::styled(cell(&mark, mark_w), ctx.style(Token::Dim)));
    }

    if cols.goal > 0 {
        spans.push(Span::raw(" "));
        match state
            .resolved_goal_id(index)
            .and_then(|id| state.goal_by_id(&id).map(|g| g.title.clone()))
        {
            Some(title) => spans.extend(chip_cell(
                ctx,
                &format!(" {PLUS_CIRCLE} {} ", cap_line(&title, GOAL_TITLE_CAP)),
                cols.goal,
                Token::AmberDim,
                Token::AccentBg,
            )),
            None => spans.push(Span::raw(" ".repeat(cols.goal))),
        }
    }

    if cols.assignee > 0 {
        spans.push(Span::raw(" "));
        match task.assignee.as_deref() {
            Some(agent_id) => spans.extend(chip_cell(
                ctx,
                &state.agent_name(agent_id),
                cols.assignee,
                Token::Green,
                Token::SuccessBg,
            )),
            None => spans.push(Span::styled(
                cell(EM_DASH, cols.assignee),
                ctx.style(Token::Ghost),
            )),
        }
    }

    let unknown = state.timing_error.is_some();
    if cols.started > 0 {
        let text = match state.task_started_ms(index) {
            Some(start) => format_started(start, ctx.now_ms),
            None if unknown => "?".to_string(),
            None => String::new(),
        };
        spans.push(Span::raw(" "));
        spans.push(Span::styled(
            cell_right(&text, cols.started),
            ctx.style(Token::Dim),
        ));
    }
    if cols.elapsed > 0 {
        let (text, token) = match state.task_elapsed_ms(index, ctx.now_ms) {
            Some(ms) if running => (format_duration(ms), Token::Cyan),
            Some(ms) => (format_duration(ms), Token::Dim),
            None if unknown => ("?".to_string(), Token::Dim),
            None => (String::new(), Token::Dim),
        };
        spans.push(Span::raw(" "));
        spans.push(Span::styled(
            cell_right(&text, cols.elapsed),
            ctx.style(token),
        ));
    }

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
