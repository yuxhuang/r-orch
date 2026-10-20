//! The agents list.

use orch_domain::{AdapterErrorKind, Agent, AgentStatus, RemoteRef, error_hint};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use ratatui::text::Span;

use crate::format::{cap_display, cap_line, format_started};
use crate::hit::{PseudoRow, ScrollRegion, VerbId, ZoneId, ZoneKind};
use crate::state::{
    AgentRow, AgentSortField, AgentUpdated, EntityType, SortColumn, UpdatedSource, agent_caps_text,
    agent_status_section_word, agent_status_word,
};
use crate::text::text_display_width;
use crate::theme::{
    CHECK, CIRCLE, CIRCLE_FILLED, CROSS, CURSOR, DIAMOND_OUTLINE, EM_DASH, LOOP, LOZENGE, STAR,
    Token, light_rule, spinner_frame,
};
use crate::views::{
    HeaderCell, RenderCtx, cell, cell_right, put_line, render_header_row, sanitize,
};

/// The agent-row error hint is capped at 30 characters. The task column
/// grows to this before the model column takes any slack, so from
/// [`ERROR_WHOLE_FROM`] columns up every row error reads whole.
const ROW_ERROR_CAP: usize = 30;

fn status_chip(status: AgentStatus, tick: u64) -> (String, Token, Token, bool) {
    match status {
        AgentStatus::Running => (
            format!("{} {}", spinner_frame(tick), agent_status_word(status)),
            Token::Green,
            Token::SuccessBg,
            true,
        ),
        AgentStatus::Idle => (
            format!("{CIRCLE} {}", agent_status_word(status)),
            Token::Dim,
            Token::NeutralBg,
            false,
        ),
        AgentStatus::Error => (
            format!("{CROSS} {}", agent_status_word(status)),
            Token::Red,
            Token::ErrorBg,
            true,
        ),
        AgentStatus::Offline => (
            format!("{CIRCLE} {}", agent_status_word(status)),
            Token::Ghost,
            Token::NeutralBg,
            false,
        ),
        AgentStatus::Disabled => (
            format!("{CIRCLE} {}", agent_status_word(status)),
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
    let cols = AgentColumns::for_width(area.width as usize);
    // One header row, like Goals and Tasks; the grouping chip ends it when
    // there is room for it (`GROUP_CHIP_FROM`), else the footer names the
    // grouping.
    render_header_row(
        frame,
        area,
        area.y,
        ctx,
        &cols.header(),
        (
            SortColumn::Agent(state.agent_sort.field),
            state.agent_sort.descending,
        ),
    );
    if cols.chip {
        render_group_chip(frame, area, ctx);
    }
    let area = Rect::new(
        area.x,
        area.y.saturating_add(1),
        area.width,
        area.height.saturating_sub(1),
    );
    let rows = state.agent_rows(ctx.caps.can_add_agent);
    // Once per frame, not once per row: it scans every task and run.
    let updated = if cols.updated > 0 {
        state.agent_updated_observations()
    } else {
        Vec::new()
    };

    ctx.zone(
        area,
        area,
        ZoneKind::Scroll(ScrollRegion::MainList),
        ZoneId::ScrollRegion(ScrollRegion::MainList),
    );

    let drawn = rows.len().saturating_sub(state.agent_scroll);
    super::render_no_match_at(
        frame,
        area,
        area.y.saturating_add(drawn.min(u16::MAX as usize) as u16),
        ctx,
    );
    let width = area.width as usize;
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
            } => put_line(
                frame,
                area,
                y,
                section_rule(
                    ctx,
                    width,
                    &sanitize(name).to_uppercase(),
                    *count,
                    lead_name.as_deref(),
                ),
            ),
            AgentRow::UnassignedSection { count } => {
                put_line(frame, area, y, outline_section(ctx, "UNASSIGNED", *count));
            }
            AgentRow::NodeSection {
                alias,
                online,
                count,
            } => {
                put_line(
                    frame,
                    area,
                    y,
                    node_header(ctx, width, alias.as_deref(), *online, *count),
                );
            }
            AgentRow::StatusSection { status, count } => put_line(
                frame,
                area,
                y,
                section_rule(ctx, width, agent_status_section_word(*status), *count, None),
            ),
            AgentRow::AdapterSection { adapter, count } => {
                let name = sanitize(adapter);
                let name = if name.trim().is_empty() {
                    EM_DASH.to_string()
                } else {
                    name
                };
                put_line(
                    frame,
                    area,
                    y,
                    section_rule(ctx, width, &name, *count, None),
                );
            }
            AgentRow::CapsSection { cap, count } => match cap {
                Some(cap) => put_line(
                    frame,
                    area,
                    y,
                    section_rule(ctx, width, &sanitize(cap), *count, None),
                ),
                None => put_line(frame, area, y, outline_section(ctx, "no caps", *count)),
            },
            AgentRow::Agent { index, selection } => {
                let seen = updated.get(*index).copied().flatten();
                render_agent_row(frame, area, ctx, y, *index, *selection, seen);
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

/// The `GROUP: <Mode>` chip at the right end of the header row: one
/// unclickable gap cell, then [`GROUP_CHIP_W`] cells holding the label
/// right-aligned. The label is a click target that cycles the grouping like
/// `b`; it never overlaps the last sortable header.
fn render_group_chip(frame: &mut Frame, area: Rect, ctx: &RenderCtx) {
    let chip_w = GROUP_CHIP_W as u16;
    if area.height == 0 || area.width < chip_w {
        return;
    }
    let text = format!("GROUP: {}", ctx.state.agent_group_by.label());
    let text_w = text_display_width(&text).min(GROUP_CHIP_W) as u16;
    let x = area.right() - text_w;
    put_line(
        frame,
        Rect::new(x, area.y, text_w, 1),
        area.y,
        vec![Span::styled(text, ctx.chip(Token::Cyan, Token::NeutralBg))],
    );
    let verb = VerbId::ToggleAgentGroupBy;
    ctx.zone(
        Rect::new(x, area.y, text_w, 1),
        area,
        ZoneKind::Verb(verb),
        ZoneId::Verb(verb),
    );
}

/// A section header drawn as a rule, as the team headers always were:
/// `─── ◈ NAME · 3 agents · ★ lead ───`. `name` must already be sanitized.
/// It is cut before the count, and the lead is cut, then dropped, before
/// the name is.
fn section_rule(
    ctx: &RenderCtx,
    width: usize,
    name: &str,
    count: usize,
    lead: Option<&str>,
) -> Vec<Span<'static>> {
    let noun = if count == 1 { "agent" } else { "agents" };
    let tail = format!(" · {count} {noun}");
    // Lead rule (3), ` ◈ ` (3), the tail, a trailing space and one cell of
    // trailing rule always stay.
    let fixed = 3 + 3 + text_display_width(&tail) + 2;
    let room = width.saturating_sub(fixed);
    let name_w = text_display_width(name);
    // ` · ★ ` and at least three cells of the lead's name.
    let lead = lead
        .map(|lead| (lead, room.saturating_sub(name_w)))
        .filter(|(_, left)| *left >= 8)
        .map(|(lead, left)| format!(" · {STAR} {}", cap_display(&sanitize(lead), left - 5)))
        .unwrap_or_default();
    let name = cap_display(name, room.saturating_sub(text_display_width(&lead)).max(1));
    let label = format!(" {LOZENGE} {name}{tail}{lead} ");
    let rule = width.saturating_sub(text_display_width(&label) + 3);
    vec![
        Span::styled(light_rule(3), ctx.style(Token::Ghost)),
        Span::styled(
            label,
            ctx.chip(Token::Amber, Token::AccentBg)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(light_rule(rule), ctx.style(Token::Ghost)),
    ]
}

/// The dim header of the rest-of section that ends a grouping (`◇
/// UNASSIGNED`, `◇ no caps`). `label` is a fixed word, never user text.
fn outline_section(ctx: &RenderCtx, label: &str, count: usize) -> Vec<Span<'static>> {
    let noun = if count == 1 { "agent" } else { "agents" };
    vec![Span::styled(
        format!(" {DIAMOND_OUTLINE} {label} · {count} {noun} "),
        ctx.chip(Token::Dim, Token::NeutralBg),
    )]
}

/// A By-node section header, drawn like a team header: `─── ◈ LOCAL · 3
/// agents ───`, or for a peer `─── ◈ w1 ● online · 2 agents ───` with the
/// state in green, or dim `○ offline`. The alias is the user's own
/// identifier (`orch remote add <alias>`), so it is shown as typed, not
/// upper-cased like a team name, and cut before the state and count are.
fn node_header(
    ctx: &RenderCtx,
    width: usize,
    alias: Option<&str>,
    online: bool,
    count: usize,
) -> Vec<Span<'static>> {
    let noun = if count == 1 { "agent" } else { "agents" };
    let tail = format!(" · {count} {noun} ");
    let (state_text, state_token) = match (alias, online) {
        (None, _) => (String::new(), Token::Green),
        (Some(_), true) => (format!(" {CIRCLE_FILLED} online"), Token::Green),
        (Some(_), false) => (format!(" {CIRCLE} offline"), Token::Dim),
    };
    // Lead rule (3) and at least one cell of trailing rule stay visible.
    let fixed = 3 + 3 + state_text.chars().count() + tail.chars().count() + 1;
    let name = match alias {
        None => "LOCAL".to_string(),
        Some(alias) => cap_display(&sanitize(alias), width.saturating_sub(fixed).max(1)),
    };
    let label = format!(" {LOZENGE} {name}");
    let used = 3
        + text_display_width(&label)
        + text_display_width(&state_text)
        + text_display_width(&tail);
    let mut spans = vec![
        Span::styled(light_rule(3), ctx.style(Token::Ghost)),
        Span::styled(
            label,
            ctx.chip(Token::Amber, Token::AccentBg)
                .add_modifier(Modifier::BOLD),
        ),
    ];
    if !state_text.is_empty() {
        spans.push(Span::styled(
            state_text,
            ctx.chip(state_token, Token::AccentBg),
        ));
    }
    spans.push(Span::styled(tail, ctx.chip(Token::Amber, Token::AccentBg)));
    spans.push(Span::styled(
        light_rule(width.saturating_sub(used)),
        ctx.style(Token::Ghost),
    ));
    spans
}

// ── Row layout ──────────────────────────────────────────────────────────────
//
// An agent row is a table row, laid out like the goals and tasks rows: fixed
// lead cells, then columns of a fixed width per frame so every row lines up
// with the one above it, whatever its data. Left to right:
//
//   cursor │ status chip │ marks │ name │ adapter/model │ task │ caps │ where │ runs │ updated │ [GROUP chip]
//
// The task column is the elastic one in the middle, so the narrow fixed
// columns stay right-aligned to the edge. On a narrow terminal the optional
// columns are granted in the order runs, task, where, model, caps, updated
// while each still fits, rather than squeezed, because a column that is too
// narrow to read is worse than an absent one; the name is never dropped.
// UPDATED comes last, so it never takes a cell from a column that was there
// before it existed. The `GROUP: <Mode>` chip is chrome, budgeted after every
// column and before any slack: it shows only when its 15 cells are spare
// (from [`GROUP_CHIP_FROM`]); narrower, the footer's `b <Mode>` names the
// grouping.

/// Cursor (2) + status chip (10 + gap) + marks (`⟳★` + gap).
const LEAD_W: usize = 2 + 11 + 3;
const NAME_MIN: usize = 10;
const TASK_MIN: usize = 12;
const MODEL_MIN: usize = 10;
const MODEL_MAX: usize = 20;
const CAPS_W: usize = 10;
/// Wide enough for `{alias} offline` with a 4-cell alias; a longer alias is
/// cut so the state word always survives.
const WHERE_W: usize = 12;
const TIME_W: usize = 9;
/// `UPDATED↑`, and `R 59m` / `T 12d` with room to spare.
const UPDATED_W: usize = 8;
/// The `GROUP: <Mode>` chip: `GROUP: Adapter`, the longest, is 14 cells.
const GROUP_CHIP_W: usize = 14;
/// The first table width with the group chip: today's columns at their
/// minimums plus UPDATED leave its 15 cells (gap + chip) spare from here.
pub const GROUP_CHIP_FROM: usize = 108;
/// The narrowest row at which the task column reaches [`ROW_ERROR_CAP`], so
/// no error hint (mapped or raw) is cut. Narrower, the hint ends in `…` and
/// the full text is in the detail panel's error box.
pub const ERROR_WHOLE_FROM: usize = 130;

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
    pub updated: usize,
    /// Whether the header row ends with the `GROUP: <Mode>` chip; the rows
    /// leave its cells (and the gap before it) blank.
    pub chip: bool,
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
        let updated = take(UPDATED_W);
        // The chip and the gap before it, only out of what the columns left.
        let chip = avail > GROUP_CHIP_W;
        if chip {
            avail -= GROUP_CHIP_W + 1;
        }
        let mut cols = AgentColumns {
            name,
            model,
            task,
            caps,
            place,
            time,
            updated,
            chip,
        };

        // Hand out the slack in steps so no one column hogs it: names first
        // (they are what you scan), then the task column to ROW_ERROR_CAP so
        // every error hint reads whole from ERROR_WHOLE_FROM columns (the
        // longest mapped hint is 28 chars), then the model.
        let mut grow = |field: &mut usize, to: usize| {
            let by = to.saturating_sub(*field).min(avail);
            *field += by;
            avail -= by;
        };
        grow(&mut cols.name, 14);
        if cols.task > 0 {
            grow(&mut cols.task, ROW_ERROR_CAP);
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

    /// Whether the column `field` sorts by is drawn at this width; STATUS
    /// and NAME always are.
    pub fn shows(self, field: AgentSortField) -> bool {
        let width = match field {
            AgentSortField::Status | AgentSortField::Name => return true,
            AgentSortField::AdapterModel => self.model,
            AgentSortField::Task => self.task,
            AgentSortField::Caps => self.caps,
            AgentSortField::Location => self.place,
            AgentSortField::Runs => self.time,
            AgentSortField::Updated => self.updated,
        };
        width > 0
    }

    /// The header labels over these columns; every one sorts.
    fn header(self) -> Vec<HeaderCell> {
        let agent = SortColumn::Agent;
        let gap = |width: usize| HeaderCell::gap(usize::from(width > 0));
        vec![
            HeaderCell::gap(2),
            HeaderCell::sort("STATUS", 10, agent(AgentSortField::Status)),
            // The gap, the marks slot and the gap after it.
            HeaderCell::gap(4),
            HeaderCell::sort("NAME", self.name, agent(AgentSortField::Name)),
            gap(self.model),
            HeaderCell::sort(
                "ADAPTER/MODEL",
                self.model,
                agent(AgentSortField::AdapterModel),
            ),
            gap(self.task),
            HeaderCell::sort("TASK", self.task, agent(AgentSortField::Task)),
            gap(self.caps),
            HeaderCell::sort("CAPS", self.caps, agent(AgentSortField::Caps)),
            gap(self.place),
            HeaderCell::sort("WHERE", self.place, agent(AgentSortField::Location)),
            gap(self.time),
            HeaderCell::sort("RUNS", self.time, agent(AgentSortField::Runs)).right(),
            gap(self.updated),
            HeaderCell::sort("UPDATED", self.updated, agent(AgentSortField::Updated)),
        ]
    }
}

/// The UPDATED cell: `R <age>` (run evidence) or `T <age>` (an assigned
/// task's record), the age cut so the marker always shows.
fn updated_text(seen: AgentUpdated, now_ms: i64, width: usize) -> String {
    let marker = match seen.source {
        UpdatedSource::Run => 'R',
        UpdatedSource::Task => 'T',
    };
    let age = format_started(seen.at_ms, now_ms);
    format!("{marker} {}", cap_display(&age, width.saturating_sub(2)))
}

/// The where cell's text: `local`, or `{alias} online|offline` with the
/// alias cut to fit so the state word always shows in full.
fn place_text(remote: Option<&RemoteRef>, width: usize) -> String {
    let Some(remote) = remote else {
        return "local".to_string();
    };
    let state = if remote.online { "online" } else { "offline" };
    let room = width.saturating_sub(state.len() + 1);
    format!("{} {state}", cap_display(&sanitize(&remote.alias), room))
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
    updated: Option<AgentUpdated>,
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
        let caps = agent_caps_text(agent);
        let (text, token) = if caps.is_empty() {
            (EM_DASH.to_string(), Token::Ghost)
        } else {
            (caps, Token::Dim)
        };
        spans.push(Span::raw(" "));
        spans.push(Span::styled(cell(&text, cols.caps), ctx.style(token)));
    }

    if cols.place > 0 {
        // Words, not just colour: offline has to read as offline in a
        // monochrome terminal too.
        let token = match agent.remote.as_ref() {
            Some(remote) if remote.online => Token::Cyan,
            Some(_) => Token::Yellow,
            None => Token::Dim,
        };
        let text = place_text(agent.remote.as_ref(), cols.place);
        spans.push(Span::raw(" "));
        spans.push(Span::styled(cell(&text, cols.place), ctx.style(token)));
    }

    if cols.time > 0 {
        // Lifetime statistics on every row, active or not: they are what the
        // RUNS sort orders by. A live run's elapsed time is in the Overview.
        let stats = &agent.stats;
        let (time_text, time_token) = if stats.total_runs > 0 {
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

    if cols.updated > 0 {
        let (text, token) = match updated {
            Some(seen) => (
                updated_text(seen, ctx.now_ms, cols.updated),
                match seen.source {
                    UpdatedSource::Run => Token::Cyan,
                    UpdatedSource::Task => Token::Dim,
                },
            ),
            None => (EM_DASH.to_string(), Token::Ghost),
        };
        spans.push(Span::raw(" "));
        spans.push(Span::styled(cell(&text, cols.updated), ctx.style(token)));
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
    use orch_domain::{AgentLastError, TaskStatus};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;

    /// The cells a row of `cols` occupies, by the layout's own arithmetic,
    /// plus the group chip and its gap when the header carries it.
    fn row_width(cols: AgentColumns) -> usize {
        let optional = [
            cols.model,
            cols.task,
            cols.caps,
            cols.place,
            cols.time,
            cols.updated,
        ];
        LEAD_W
            + cols.name
            + optional
                .iter()
                .filter(|w| **w > 0)
                .map(|w| w + 1)
                .sum::<usize>()
            + if cols.chip { GROUP_CHIP_W + 1 } else { 0 }
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
    fn at_80_columns_caps_is_the_only_column_dropped_and_there_is_no_chip() {
        let cols = AgentColumns::for_width(80);
        assert_eq!(cols.caps, 0, "{cols:?}");
        assert!(cols.model >= MODEL_MIN, "{cols:?}");
        assert!(cols.task >= TASK_MIN, "{cols:?}");
        assert_eq!(cols.place, WHERE_W, "{cols:?}");
        assert_eq!(cols.time, TIME_W, "{cols:?}");
        assert_eq!(cols.updated, 0, "{cols:?}");
        assert!(!cols.chip, "{cols:?}");
    }

    #[test]
    fn at_120_columns_every_column_and_the_chip_show() {
        let cols = AgentColumns::for_width(120);
        assert_eq!(cols.caps, CAPS_W, "{cols:?}");
        assert_eq!(cols.updated, UPDATED_W, "{cols:?}");
        assert!(cols.chip, "{cols:?}");
    }

    /// Pins the column-width table (adjudication v2, Issue 2): today's
    /// allocation for the five old columns, UPDATED granted after them, the
    /// chip only out of what is left.
    #[test]
    fn the_documented_column_widths_hold() {
        // (width, name, model, task, caps, where, runs, updated, chip)
        let table = [
            (60, 10, 10, 12, 0, 0, 9, 0, false),
            (70, 14, 0, 16, 0, 12, 9, 0, false),
            (80, 14, 10, 15, 0, 12, 9, 0, false),
            (90, 14, 10, 14, 10, 12, 9, 0, false),
            (93, 10, 10, 12, 10, 12, 9, 8, false),
            (100, 14, 10, 15, 10, 12, 9, 8, false),
            (107, 14, 10, 22, 10, 12, 9, 8, false),
            (108, 10, 10, 12, 10, 12, 9, 8, true),
            (120, 14, 10, 20, 10, 12, 9, 8, true),
            (130, 14, 10, 30, 10, 12, 9, 8, true),
            (140, 18, 16, 30, 10, 12, 9, 8, true),
            (160, 22, 20, 42, 10, 12, 9, 8, true),
        ];
        for (width, name, model, task, caps, place, time, updated, chip) in table {
            assert_eq!(
                AgentColumns::for_width(width),
                AgentColumns {
                    name,
                    model,
                    task,
                    caps,
                    place,
                    time,
                    updated,
                    chip,
                },
                "{width}"
            );
        }
    }

    #[test]
    fn the_chip_shows_from_its_documented_width() {
        for width in 40..=240 {
            assert_eq!(
                AgentColumns::for_width(width).chip,
                width >= GROUP_CHIP_FROM,
                "{width}"
            );
        }
    }

    #[test]
    fn past_the_model_maximum_the_task_takes_the_slack() {
        let cols = AgentColumns::for_width(160);
        assert_eq!(cols.model, MODEL_MAX, "{cols:?}");
        assert!(cols.task > ROW_ERROR_CAP, "{cols:?}");
    }

    #[test]
    fn at_the_minimum_width_the_name_and_runs_survive() {
        let cols = AgentColumns::for_width(40);
        assert!(cols.name >= NAME_MIN, "{cols:?}");
        assert_eq!(cols.time, TIME_W, "{cols:?}");
        assert_eq!(
            (cols.task, cols.model, cols.caps, cols.place, cols.updated),
            (0, 0, 0, 0, 0)
        );
        assert!(!cols.chip, "{cols:?}");
    }

    #[test]
    fn the_updated_marker_survives_a_cut_age() {
        let seen = |source| AgentUpdated { at_ms: 0, source };
        // 100 000 days: `100000d` does not fit beside the marker.
        let now = 100_000 * 86_400_000;
        for (source, marker) in [(UpdatedSource::Run, "R "), (UpdatedSource::Task, "T ")] {
            let text = updated_text(seen(source), now, UPDATED_W);
            assert!(text.starts_with(marker), "{text:?}");
            assert!(text_display_width(&text) <= UPDATED_W, "{text:?}");
        }
        assert_eq!(
            updated_text(seen(UpdatedSource::Run), 300_000, UPDATED_W),
            "R 5m"
        );
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

    /// Where ratatui's cursor ends up after drawing `text` from x = 0: the
    /// width the terminal will actually give it.
    fn rendered_width(text: &str) -> usize {
        let mut buffer = Buffer::empty(Rect::new(0, 0, 80, 1));
        let (x, _) = buffer.set_stringn(0, 0, text, usize::MAX, ratatui::style::Style::default());
        x as usize
    }

    /// Graphemes `text_display_width` clamps to one cell but ratatui draws
    /// in zero: each used to shift the columns after it left by one.
    const ZERO_WIDTH_NAMES: [&str; 6] = [
        "zw\u{200B}sp\u{200B}ace",
        "rtl\u{202E}evil",
        "\u{00AD}soft",
        "lrm\u{200E}rlm\u{200F}",
        "\u{FEFF}bom\u{2060}wj",
        "\u{0301}lone-mark",
    ];

    #[test]
    fn a_cell_renders_exactly_its_width_in_terminal_cells() {
        let wide = [
            "ab",
            "数据工程师-数据工程师",
            "🚀🚀🚀🚀🚀🚀🚀",
            "e\u{301}e\u{301}acute",
            "❤\u{FE0F}heart",
            "🇺🇸flag",
            "👨\u{200D}👩\u{200D}👧fam",
            "שלום עולם",
            "line\nbreak",
        ];
        for text in wide.iter().chain(ZERO_WIDTH_NAMES.iter()) {
            for width in [1, 5, 9, 12, 22] {
                let left = cell(text, width);
                assert_eq!(rendered_width(&left), width, "{text:?} @ {width}: {left:?}");
                let right = cell_right(text, width);
                assert_eq!(
                    rendered_width(&right),
                    width,
                    "{text:?} @ {width}: {right:?}"
                );
            }
        }
    }

    #[test]
    fn zero_width_and_bidi_characters_never_reach_the_terminal() {
        for text in ZERO_WIDTH_NAMES {
            let fitted = cell(text, 20);
            assert!(
                !fitted.chars().any(|c| matches!(
                    c,
                    '\u{200B}'
                        | '\u{202E}'
                        | '\u{00AD}'
                        | '\u{200E}'
                        | '\u{200F}'
                        | '\u{FEFF}'
                        | '\u{2060}'
                )),
                "{text:?} -> {fitted:?}"
            );
        }
        // A combining mark on a base character is kept: it is part of a
        // visible grapheme, not a zero-width one.
        assert!(cell("e\u{301}", 3).starts_with("e\u{301}"));
    }

    #[test]
    fn rows_with_zero_width_names_align_by_rendered_cell() {
        let agents: Vec<Agent> = std::iter::once("plain")
            .chain(ZERO_WIDTH_NAMES)
            .enumerate()
            .map(|(i, name)| make_agent(&format!("agt_{i}"), name, AgentStatus::Idle))
            .collect();
        // Taller than the bare list needs: the column header sits above
        // the rows.
        for (width, height) in [(80, 28), (100, 28), (120, 40)] {
            let buffer = draw(
                FakeBackend::new().with_agents(agents.clone()),
                width,
                height,
            );
            let screen = text(&buffer);
            let models = columns_of(&buffer, "claude");
            assert_eq!(models.len(), ZERO_WIDTH_NAMES.len() + 1, "{screen}");
            assert!(
                models.windows(2).all(|w| w[0] == w[1]),
                "{models:?}\n{screen}"
            );
            let places = columns_of(&buffer, "local");
            assert_eq!(places.len(), ZERO_WIDTH_NAMES.len() + 1, "{screen}");
            assert!(
                places.windows(2).all(|w| w[0] == w[1]),
                "{places:?}\n{screen}"
            );
            assert!(!screen.contains('\u{202E}'), "{screen}");
        }
    }

    #[test]
    fn every_mapped_error_hint_fits_the_row_error_cap() {
        for kind in AdapterErrorKind::ALL {
            let message = error_hint(kind).message;
            assert!(
                text_display_width(message) <= ROW_ERROR_CAP,
                "{kind:?}: {message:?}"
            );
        }
    }

    #[test]
    fn the_task_column_holds_a_whole_error_from_the_documented_width() {
        for width in ERROR_WHOLE_FROM..=240 {
            let cols = AgentColumns::for_width(width);
            assert!(cols.task >= ROW_ERROR_CAP, "{width}: {cols:?}");
        }
        // And not one column sooner, so the constant cannot drift from the
        // layout unnoticed.
        let cols = AgentColumns::for_width(ERROR_WHOLE_FROM - 1);
        assert!(cols.task < ROW_ERROR_CAP, "{cols:?}");
    }

    fn spawn_failed_backend() -> FakeBackend {
        let mut agent = make_agent("agt_1", "broken", AgentStatus::Error);
        agent.last_error = Some(AgentLastError {
            message: "spawn ENOENT: the raw message the row must not show".into(),
            kind: "spawn_failed".into(),
            timestamp: "2026-10-16T00:00:00.000Z".into(),
        });
        FakeBackend::new().with_agents(vec![agent])
    }

    #[test]
    fn the_longest_error_hint_reads_whole_from_the_documented_width() {
        let hint = error_hint(AdapterErrorKind::SpawnFailed).message;
        let screen = text(&draw(spawn_failed_backend(), ERROR_WHOLE_FROM as u16, 24));
        assert!(screen.contains(hint), "{screen}");
        // At 120 columns, where the group chip takes its cells, it is cut,
        // and says so with an ellipsis.
        let narrower = 120;
        let screen = text(&draw(spawn_failed_backend(), narrower as u16, 24));
        assert!(!screen.contains(hint), "{screen}");
        let cut = cap_display(hint, AgentColumns::for_width(narrower).task);
        assert!(cut.ends_with('…'), "{cut:?}");
        assert!(screen.contains(&cut), "{screen}");
    }

    fn remote_agent(id: &str, alias: &str, online: bool) -> Agent {
        let mut agent = make_agent(
            id,
            &format!("worker@{alias}"),
            if online {
                AgentStatus::Idle
            } else {
                AgentStatus::Offline
            },
        );
        agent.remote = Some(RemoteRef {
            alias: alias.into(),
            online,
        });
        agent
    }

    #[test]
    fn the_where_cell_cuts_the_alias_not_the_state() {
        let alias = "a-very-long-alias";
        let offline = RemoteRef {
            alias: alias.into(),
            online: false,
        };
        let online = RemoteRef {
            alias: alias.into(),
            online: true,
        };
        assert_eq!(place_text(Some(&offline), WHERE_W), "a-v… offline");
        assert_eq!(place_text(Some(&online), WHERE_W), "a-ve… online");
        assert_eq!(place_text(None, WHERE_W), "local");
        // A short alias is left alone.
        let short = RemoteRef {
            alias: "w1".into(),
            online: false,
        };
        assert_eq!(place_text(Some(&short), WHERE_W), "w1 offline");
    }

    #[test]
    fn a_long_remote_alias_keeps_its_online_and_offline_words_at_80_columns() {
        let alias = "a-very-long-alias";
        assert_eq!(alias.len(), 17);
        let backend = FakeBackend::new().with_agents(vec![
            make_agent("agt_1", "home", AgentStatus::Idle),
            remote_agent("agt_2@x", alias, false),
            remote_agent("agt_3@y", alias, true),
        ]);
        let buffer = draw(backend, 80, 24);
        let screen = text(&buffer);
        let offline = screen.lines().find(|l| l.contains("OFFLINE")).unwrap();
        assert!(offline.contains(" offline"), "{screen}");
        let online = screen
            .lines()
            .find(|l| l.contains("IDLE") && l.contains("worker@"))
            .unwrap();
        assert!(online.contains(" online"), "{screen}");
        // And the where column still starts in the same cell on every row.
        let mut places = columns_of(&buffer, "local");
        places.extend(columns_of(&buffer, "a-v… offline"));
        places.extend(columns_of(&buffer, "a-ve… online"));
        assert_eq!(places.len(), 3, "{screen}");
        assert!(
            places.windows(2).all(|w| w[0] == w[1]),
            "{places:?}\n{screen}"
        );
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
        draw(hostile_backend(), width, height)
    }

    fn draw(backend: FakeBackend, width: u16, height: u16) -> Buffer {
        let mut app = App::new(backend, TestClock::new(1_000_000));
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
        // 26 rather than 24: the column header takes a row of the list.
        assert_aligned(80, 26);
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
