//! The goals list.

use orch_domain::{GoalStatus, TaskStatus};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use ratatui::text::Span;

use crate::format::{cap_display, format_duration, format_started};
use crate::hit::{PseudoRow, ScrollRegion, ZoneId, ZoneKind};
use crate::state::{EntityType, GoalSortField, SortColumn, goal_status_word};
use crate::text::text_display_width;
use crate::theme::{BLOCK, BULLSEYE, CHECK, CROSS, CURSOR, EM_DASH, PAUSE_BARS, SHADE, Token};
use crate::views::{
    HeaderCell, RenderCtx, cell, cell_right, put_line, render_header_row, sanitize,
};

const PROGRESS_W: usize = 14;
const PROGRESS_CELLS: usize = 6;

fn status_chip(status: GoalStatus) -> (String, Token, Token, bool) {
    match status {
        GoalStatus::Active => (
            format!(" {BULLSEYE} {} ", goal_status_word(status)),
            Token::Green,
            Token::SuccessBg,
            true,
        ),
        GoalStatus::Paused => (
            format!(" {PAUSE_BARS} {} ", goal_status_word(status)),
            Token::Dim,
            Token::NeutralBg,
            false,
        ),
        GoalStatus::Achieved => (
            format!(" {CHECK} {} ", goal_status_word(status)),
            Token::Amber,
            Token::AccentBg,
            true,
        ),
        GoalStatus::Abandoned => (
            format!(" {CROSS} {} ", goal_status_word(status)),
            Token::Ghost,
            Token::NeutralBg,
            false,
        ),
    }
}

// ── Column layout ─────────────────────────────────────────────────────────
//
//   cursor │ status chip │ title │ progress │ assignee │ started │ elapsed*
//
// One width per column per frame, shared by the header and every row, so the
// labels sit over their values. Title is elastic; on a narrow terminal the
// optional columns drop lowest-priority first (progress, assignee, started)
// rather than squeezing.

/// Cursor (2) + status chip (11).
const LEAD_W: usize = 2 + 11;
const TITLE_MIN: usize = 10;
const ASSIGNEE_W: usize = 14;
const STARTED_W: usize = 8;
/// `ELAPSED*↓` fits exactly.
const ELAPSED_W: usize = 9;

/// Column widths for one frame's goal rows; `0` hides a column. Each optional
/// column is preceded by a one-cell gap that is not counted here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GoalColumns {
    pub title: usize,
    pub progress: usize,
    pub assignee: usize,
    pub started: usize,
    pub elapsed: usize,
}

impl GoalColumns {
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
        let progress = take(PROGRESS_W);
        GoalColumns {
            title: title + avail,
            progress,
            assignee,
            started,
            elapsed,
        }
    }

    fn header(self) -> Vec<HeaderCell> {
        let goal = SortColumn::Goal;
        vec![
            HeaderCell::gap(2),
            HeaderCell::sort("STATUS", 11, goal(GoalSortField::Status)),
            HeaderCell::sort("TITLE", self.title, goal(GoalSortField::Title)),
            HeaderCell::gap(usize::from(self.progress > 0)),
            HeaderCell::sort("PROGRESS", self.progress, goal(GoalSortField::Progress)),
            HeaderCell::gap(usize::from(self.assignee > 0)),
            HeaderCell::sort("ASSIGNEE", self.assignee, goal(GoalSortField::Assignee)),
            HeaderCell::gap(usize::from(self.started > 0)),
            HeaderCell::sort("STARTED", self.started, goal(GoalSortField::Started)).right(),
            HeaderCell::gap(usize::from(self.elapsed > 0)),
            HeaderCell::sort("ELAPSED*", self.elapsed, goal(GoalSortField::Elapsed)).right(),
        ]
    }
}

pub fn render(frame: &mut Frame, area: Rect, ctx: &RenderCtx) {
    let state = ctx.state;
    let cols = GoalColumns::for_width(area.width as usize);
    render_header_row(
        frame,
        area,
        area.y,
        ctx,
        &cols.header(),
        (
            SortColumn::Goal(state.goal_sort.field),
            state.goal_sort.descending,
        ),
    );
    let area = Rect::new(
        area.x,
        area.y.saturating_add(1),
        area.width,
        area.height.saturating_sub(1),
    );
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
            Span::styled(cell(&chip_text, 11), {
                let style = ctx.chip(chip_fg, chip_bg);
                if chip_bold {
                    style.add_modifier(Modifier::BOLD)
                } else {
                    style
                }
            }),
            Span::styled(cell(&goal.title, cols.title), {
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
            }),
        ];

        // The progress cell is blank at zero linked tasks — no `0/0`.
        if cols.progress > 0 {
            spans.push(Span::raw(" "));
            if linked.is_empty() {
                spans.push(Span::raw(" ".repeat(cols.progress)));
            } else {
                let filled =
                    ((done as f64 / linked.len() as f64) * PROGRESS_CELLS as f64).round() as usize;
                let bar = format!(
                    "{}{}",
                    BLOCK.repeat(filled.min(PROGRESS_CELLS)),
                    SHADE.repeat(PROGRESS_CELLS.saturating_sub(filled))
                );
                let count = format!(" {done}/{}", linked.len());
                let count = cell(&count, cols.progress.saturating_sub(PROGRESS_CELLS));
                spans.push(Span::styled(bar, ctx.style(Token::Green)));
                spans.push(Span::styled(count, ctx.style(Token::Dim)));
            }
        }

        if cols.assignee > 0 {
            spans.push(Span::raw(" "));
            match goal.assignee.as_deref() {
                Some(agent_id) => {
                    let name = cap_display(&sanitize(&state.agent_name(agent_id)), cols.assignee);
                    let pad = cols.assignee.saturating_sub(text_display_width(&name));
                    spans.push(Span::styled(name, ctx.chip(Token::Green, Token::SuccessBg)));
                    spans.push(Span::raw(" ".repeat(pad)));
                }
                None => spans.push(Span::styled(
                    cell(EM_DASH, cols.assignee),
                    ctx.style(Token::Ghost),
                )),
            }
        }

        let unknown = state.timing_error.is_some();
        if cols.started > 0 {
            let text = match state.goal_started_ms(*goal_index) {
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
            let (text, token) = match state.goal_elapsed_ms(*goal_index, ctx.now_ms) {
                Some(ms) if goal.status == GoalStatus::Active => (format_duration(ms), Token::Cyan),
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
        y += 1;
    }

    // `+ add goal...` at selection index `goals.len()`.
    let add_y = y;
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
    let below = add_y + u16::from(ctx.caps.can_create_goal);
    super::render_no_match_at(frame, area, below, ctx);
}
