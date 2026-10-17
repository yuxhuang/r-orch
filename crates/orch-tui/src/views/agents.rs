//! The agents list (docs/parity/tui.md §1.3).

use orch_domain::{AdapterErrorKind, Agent, AgentStatus, error_hint};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use ratatui::text::Span;

use crate::format::{cap_display, cap_line, format_duration, parse_timestamp_ms};
use crate::hit::{PseudoRow, ScrollRegion, ZoneId, ZoneKind};
use crate::state::{AgentRow, EntityType};
use crate::text::text_display_width;
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
        AgentStatus::Offline => (
            format!("{CIRCLE} OFFLINE"),
            Token::Ghost,
            Token::NeutralBg,
            false,
        ),
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

    ctx.zone(
        area,
        area,
        ZoneKind::Scroll(ScrollRegion::MainList),
        ZoneId::ScrollRegion(ScrollRegion::MainList),
    );

    for (y, row) in (area.y..).zip(rows.iter().skip(state.agent_scroll)) {
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
                render_agent_row(frame, area, ctx, y, *index, *selection);
                if let Some(agent) = state.agents.get(*index) {
                    ctx.row_zone(
                        area,
                        y,
                        ZoneKind::AgentRow {
                            selection: *selection,
                        },
                        ZoneId::Entity(EntityType::Agent, agent.id.clone()),
                    );
                }
            }
            AgentRow::AddAgent { selection } => {
                let selected = *selection == state.agent_selected;
                ctx.row_zone(
                    area,
                    y,
                    ZoneKind::Pseudo(PseudoRow::AddAgent),
                    ZoneId::Pseudo(PseudoRow::AddAgent),
                );
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
    }
}

// ── Row layout ──────────────────────────────────────────────────────────────
//
// An agent row is a table row, laid out like the goals and tasks rows: fixed
// lead cells, then columns of a fixed width per frame so every row lines up
// with the one above it, whatever its data. Left to right:
//
//   cursor │ status chip │ marks │ name │ adapter/model │ task │ caps │ where │ runs
//
// The task column is the elastic one in the middle, so the narrow fixed
// columns stay right-aligned to the edge. On a narrow terminal the optional
// columns are dropped lowest-priority first (caps, model, where, task, runs)
// rather than squeezed, because a column that is too narrow to read is worse
// than an absent one; the name is never dropped.

/// Cursor (2) + status chip (10 + gap) + marks (`⟳★` + gap).
const LEAD_W: usize = 2 + 11 + 3;
const NAME_MIN: usize = 10;
const TASK_MIN: usize = 12;
const MODEL_MIN: usize = 10;
const MODEL_MAX: usize = 20;
const CAPS_W: usize = 10;
/// Wide enough for `{alias} offline` with a short alias.
const WHERE_W: usize = 12;
const TIME_W: usize = 9;

/// Column widths for one frame's agent rows, in display cells; `0` hides a
/// column. Every row in a frame uses the same widths — that is the alignment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AgentColumns {
    pub name: usize,
    pub model: usize,
    pub task: usize,
    pub caps: usize,
    pub place: usize,
    pub time: usize,
}

impl AgentColumns {
    /// Lay the columns out for a row `width` cells wide.
    pub fn for_width(width: usize) -> Self {
        let mut avail = width.saturating_sub(LEAD_W);
        let name = NAME_MIN.min(avail);
        avail -= name;
        // Each optional column costs its width plus the one-cell gap before it.
        let mut take = |w: usize| {
            if avail > w {
                avail -= w + 1;
                w
            } else {
                0
            }
        };
        let time = take(TIME_W);
        let task = take(TASK_MIN);
        let place = take(WHERE_W);
        let model = take(MODEL_MIN);
        let caps = take(CAPS_W);
        let mut cols = AgentColumns {
            name,
            model,
            task,
            caps,
            place,
            time,
        };

        // Hand out the slack in steps so no one column hogs it: names first
        // (they are what you scan), then the task column to 22 so every
        // error hint (the longest is 28 chars) reads whole at 100 columns,
        // then the model.
        let mut grow = |field: &mut usize, to: usize| {
            let by = to.saturating_sub(*field).min(avail);
            *field += by;
            avail -= by;
        };
        grow(&mut cols.name, 14);
        if cols.task > 0 {
            grow(&mut cols.task, 22);
        }
        if cols.model > 0 {
            grow(&mut cols.model, 16);
        }
        grow(&mut cols.name, 22);
        if cols.model > 0 {
            grow(&mut cols.model, MODEL_MAX);
        }
        if cols.task > 0 {
            grow(&mut cols.task, usize::MAX);
        } else {
            grow(&mut cols.name, usize::MAX);
        }
        cols
    }
}

/// `text` fitted to exactly `width` display cells: control characters (a
/// newline in a task title) become spaces, an overlong value ends in `…`, a
/// short one is padded. Measured in cells, not `char`s, so a CJK or emoji
/// name cannot push the columns after it out of line.
fn cell(text: &str, width: usize) -> String {
    let clean: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let capped = cap_display(&clean, width);
    let pad = width.saturating_sub(text_display_width(&capped));
    format!("{capped}{}", " ".repeat(pad))
}

/// [`cell`], right-aligned.
fn cell_right(text: &str, width: usize) -> String {
    let capped = cap_display(text, width);
    let pad = width.saturating_sub(text_display_width(&capped));
    format!("{}{capped}", " ".repeat(pad))
}

/// `adapter/model`, or just the adapter when no model is configured.
fn model_text(agent: &Agent) -> String {
    match agent.config.model.as_deref().map(str::trim) {
        Some(model) if !model.is_empty() => format!("{}/{model}", agent.adapter),
        _ => agent.adapter.clone(),
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
    let cols = AgentColumns::for_width(area.width as usize);

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
        Span::styled(format!("{chip_text:<10}"), {
            let style = ctx.chip(chip_fg, chip_bg);
            if chip_bold {
                style.add_modifier(Modifier::BOLD)
            } else {
                style
            }
        }),
        Span::raw(" "),
    ];

    // Marks: a fixed two-cell slot, so a lead or a loop agent's name starts
    // in the same column as everyone else's.
    let autonomous = agent.autonomous == Some(true);
    let lead = team.is_some_and(|t| t.lead_agent_id == agent.id);
    spans.push(Span::styled(
        if autonomous { LOOP } else { " " },
        ctx.style(Token::Cyan),
    ));
    spans.push(Span::styled(
        if lead { STAR } else { " " },
        ctx.style(Token::Amber),
    ));
    spans.push(Span::raw(" "));

    let name_token = if selected {
        Token::White
    } else if running {
        Token::Green
    } else {
        Token::Silver
    };
    spans.push(Span::styled(cell(&agent.name, cols.name), {
        let style = ctx.style(name_token);
        if selected || running {
            style.add_modifier(Modifier::BOLD)
        } else {
            style
        }
    }));

    if cols.model > 0 {
        spans.push(Span::raw(" "));
        spans.push(Span::styled(
            cell(&model_text(agent), cols.model),
            ctx.style(Token::Dim),
        ));
    }

    if cols.task > 0 {
        // An error outranks the task: it is why the agent is not working.
        let (text, token) = if agent.status == AgentStatus::Error
            && let Some(error) = agent.last_error.as_ref()
        {
            (row_error_text(&error.kind, &error.message), Token::Red)
        } else if running && let Some(task_id) = agent.current_task.as_deref() {
            // A remote agent's task may not be mirrored here: show its id
            // rather than nothing.
            let title = state
                .task_by_id(task_id)
                .map(|task| task.title.clone())
                .unwrap_or_else(|| task_id.to_string());
            (title, Token::Gray)
        } else {
            (EM_DASH.to_string(), Token::Ghost)
        };
        spans.push(Span::raw(" "));
        spans.push(Span::styled(cell(&text, cols.task), ctx.style(token)));
    }

    if cols.caps > 0 {
        let (text, token) = if agent.capabilities.is_empty() {
            (EM_DASH.to_string(), Token::Ghost)
        } else {
            (agent.capabilities.join(","), Token::Dim)
        };
        spans.push(Span::raw(" "));
        spans.push(Span::styled(cell(&text, cols.caps), ctx.style(token)));
    }

    if cols.place > 0 {
        // Words, not just colour: offline has to read as offline in a
        // monochrome terminal too.
        let (text, token) = match agent.remote.as_ref() {
            Some(remote) if remote.online => (format!("{} online", remote.alias), Token::Cyan),
            Some(remote) => (format!("{} offline", remote.alias), Token::Yellow),
            None => ("local".to_string(), Token::Dim),
        };
        spans.push(Span::raw(" "));
        spans.push(Span::styled(cell(&text, cols.place), ctx.style(token)));
    }

    if cols.time > 0 {
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
        spans.push(Span::raw(" "));
        spans.push(Span::styled(
            cell_right(&time_text, cols.time),
            ctx.style(time_token),
        ));
    }

    put_line(frame, area, y, spans);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::App;
    use crate::backend::{FakeBackend, make_agent, make_task, make_team};
    use crate::clock::TestClock;
    use crate::state::ViewId;
    use orch_domain::{RemoteRef, TaskStatus};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;

    /// The cells a row of `cols` occupies, by the layout's own arithmetic.
    fn row_width(cols: AgentColumns) -> usize {
        let optional = [cols.model, cols.task, cols.caps, cols.place, cols.time];
        LEAD_W
            + cols.name
            + optional
                .iter()
                .filter(|w| **w > 0)
                .map(|w| w + 1)
                .sum::<usize>()
    }

    #[test]
    fn the_columns_fill_every_supported_width_exactly() {
        for width in 40..=240 {
            let cols = AgentColumns::for_width(width);
            assert_eq!(row_width(cols), width, "{width}: {cols:?}");
            assert!(cols.name >= NAME_MIN, "{width}: {cols:?}");
        }
    }

    #[test]
    fn at_80_columns_caps_is_the_only_column_dropped() {
        let cols = AgentColumns::for_width(80);
        assert_eq!(cols.caps, 0, "{cols:?}");
        assert!(cols.model >= MODEL_MIN, "{cols:?}");
        assert!(cols.task >= TASK_MIN, "{cols:?}");
        assert_eq!(cols.place, WHERE_W, "{cols:?}");
        assert_eq!(cols.time, TIME_W, "{cols:?}");
    }

    #[test]
    fn at_120_columns_every_column_shows_and_the_task_takes_the_slack() {
        let cols = AgentColumns::for_width(120);
        assert_eq!(cols.caps, CAPS_W, "{cols:?}");
        assert_eq!(cols.name, 22, "{cols:?}");
        assert_eq!(cols.model, MODEL_MAX, "{cols:?}");
        assert!(cols.task > 20, "{cols:?}");
    }

    #[test]
    fn at_the_minimum_width_the_name_and_runs_survive() {
        let cols = AgentColumns::for_width(40);
        assert!(cols.name >= NAME_MIN, "{cols:?}");
        assert_eq!(cols.time, TIME_W, "{cols:?}");
        assert_eq!((cols.task, cols.model, cols.caps, cols.place), (0, 0, 0, 0));
    }

    #[test]
    fn a_cell_is_exactly_its_width_in_display_cells() {
        for text in [
            "",
            "ab",
            "数据工程师-数据工程师",
            "🚀🚀🚀🚀🚀🚀🚀",
            "line\nbreak",
        ] {
            for width in [1, 5, 9, 12] {
                let fitted = cell(text, width);
                assert_eq!(text_display_width(&fitted), width, "{text:?} @ {width}");
                assert!(!fitted.contains('\n'), "{fitted:?}");
            }
        }
    }

    /// Hostile names and values: wide glyphs, a name past any column, a
    /// remote agent that is offline, a running task whose title has a
    /// newline, a lead, a loop agent.
    fn hostile_backend() -> FakeBackend {
        let mut lead = make_agent("agt_1", "lead", AgentStatus::Idle);
        lead.autonomous = Some(true);
        let cjk = make_agent("agt_2", "数据工程师", AgentStatus::Idle);
        let mut emoji = make_agent("agt_3", "🚀 rocket \"quoted\"", AgentStatus::Running);
        emoji.current_task = Some("tsk_1".into());
        let long = make_agent(
            "agt_4",
            "an-agent-name-that-is-far-longer-than-any-column-allows",
            AgentStatus::Idle,
        );
        let mut remote = make_agent("agt_5@w1", "worker@w1", AgentStatus::Offline);
        remote.capabilities = vec!["linux".into(), "gpu".into()];
        remote.remote = Some(RemoteRef {
            alias: "w1".into(),
            online: false,
        });
        let mut task = make_task(
            "tsk_1",
            "first line\nsecond line of a title",
            TaskStatus::InProgress,
        );
        task.assignee = Some("agt_3".into());
        FakeBackend::new()
            .with_agents(vec![lead, cjk, emoji, long, remote])
            .with_tasks(vec![task])
            .with_teams(vec![make_team(
                "team_1",
                "core",
                "agt_1",
                &["agt_1", "agt_2"],
            )])
    }

    fn draw_agents(width: u16, height: u16) -> Buffer {
        let mut app = App::new(hostile_backend(), TestClock::new(1_000_000));
        app.state.width = width;
        app.state.height = height;
        app.state.view = ViewId::Agents;
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| _ = crate::views::draw(frame, &app))
            .unwrap();
        terminal.backend().buffer().clone()
    }

    /// The column each row containing `needle` (ASCII) has it at.
    fn columns_of(buffer: &Buffer, needle: &str) -> Vec<u16> {
        let needle: Vec<String> = needle.chars().map(|c| c.to_string()).collect();
        let width = buffer.area.width;
        let mut found = Vec::new();
        for y in 0..buffer.area.height {
            for x in 0..width.saturating_sub(needle.len() as u16) {
                if needle
                    .iter()
                    .enumerate()
                    .all(|(i, ch)| buffer[(x + i as u16, y)].symbol() == ch)
                {
                    found.push(x);
                    break;
                }
            }
        }
        found
    }

    fn text(buffer: &Buffer) -> String {
        let mut out = String::new();
        for y in 0..buffer.area.height {
            for x in 0..buffer.area.width {
                out.push_str(buffer[(x, y)].symbol());
            }
            out.push('\n');
        }
        out
    }

    fn assert_aligned(width: u16, height: u16) {
        let buffer = draw_agents(width, height);
        let screen = text(&buffer);
        // Every agent row's model column starts in the same cell, CJK and
        // emoji names included.
        let models = columns_of(&buffer, "claude");
        assert_eq!(models.len(), 5, "{screen}");
        assert!(
            models.windows(2).all(|w| w[0] == w[1]),
            "{models:?}\n{screen}"
        );
        // And so does the where column, local and remote alike.
        let mut places = columns_of(&buffer, "local");
        places.extend(columns_of(&buffer, "w1 offline"));
        assert_eq!(places.len(), 5, "{screen}");
        assert!(
            places.windows(2).all(|w| w[0] == w[1]),
            "{places:?}\n{screen}"
        );
        // The overlong name is cut with an ellipsis, not wrapped or dropped.
        assert!(screen.contains("an-agent-name"), "{screen}");
        assert!(!screen.contains("any-column-allows"), "{screen}");
        assert!(screen.contains('…'), "{screen}");
        // A running task's title is on its agent's row, newline and all.
        let row = screen.lines().find(|l| l.contains("rocket")).unwrap();
        assert!(row.contains("first line"), "{screen}");
        assert!(
            !screen.lines().any(|l| l.trim_start().starts_with("second")),
            "{screen}"
        );
    }

    #[test]
    fn rows_align_at_80_columns() {
        assert_aligned(80, 24);
    }

    #[test]
    fn rows_align_at_120_columns() {
        assert_aligned(120, 40);
    }

    #[test]
    fn caps_show_at_120_columns() {
        let screen = text(&draw_agents(120, 40));
        assert!(screen.contains("linux,gpu"), "{screen}");
    }
}
