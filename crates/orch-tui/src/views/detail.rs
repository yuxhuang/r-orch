//! Sectioned inspectors. The rail and optional overview pin are chrome;
//! metrics describe only the independently navigable section body.

use orch_domain::{AdapterErrorKind, TaskStatus, error_hint};
use ratatui::{Frame, layout::Rect, text::Span};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::backend::CopyOutcome;
use crate::format::{format_duration, parse_timestamp_ms};
use crate::hit::{FooterAction, PaneMetrics, VerbId, ZoneId, ZoneKind};
use crate::messages::Message;
use crate::state::{
    AgentSection, AppState, CopyOrigin, EntityType, Focus, GoalSection, LogSection, TaskSection,
};
use crate::theme::{EM_DASH, Token};
use crate::views::activity::{RowGroups, row_spans};
use crate::views::{RenderCtx, fit_cells, put_line};
use crate::widgets::text_viewer::ViewerSource;

pub fn section_name(focus: Focus) -> &'static str {
    match focus {
        Focus::GoalDetail(GoalSection::Overview)
        | Focus::TaskDetail(TaskSection::Overview)
        | Focus::AgentDetail(AgentSection::Overview) => "Overview",
        Focus::GoalDetail(GoalSection::Description)
        | Focus::TaskDetail(TaskSection::Description) => "Description",
        Focus::GoalDetail(GoalSection::Progress) => "Progress",
        Focus::GoalDetail(GoalSection::Tasks) => "Tasks",
        Focus::TaskDetail(TaskSection::Attachments) => "Attachments",
        Focus::TaskDetail(TaskSection::Result) => "Result",
        Focus::TaskDetail(TaskSection::Files) => "Files",
        Focus::TaskDetail(TaskSection::Activity)
        | Focus::GoalDetail(GoalSection::Activity)
        | Focus::AgentDetail(AgentSection::Activity) => "Activity",
        Focus::AgentDetail(AgentSection::Error) => "Error",
        Focus::AgentDetail(AgentSection::Role) => "Role",
        Focus::LogDetail(LogSection::Metadata) => "Metadata",
        Focus::LogDetail(LogSection::Body) => "Body",
        _ => "Overview",
    }
}

fn entity_source(kind: EntityType, id: &str, field: &str) -> ViewerSource {
    ViewerSource::Entity {
        kind,
        id: id.into(),
        field: field.into(),
    }
}

/// Source text shared with navigation/source resolution. No panel-length
/// cap is applied: scrolling and expansion retain the complete field.
pub(crate) fn section_text(state: &AppState, focus: Focus) -> Vec<String> {
    match focus {
        Focus::GoalDetail(section) => {
            let Some(goal) = state.selected_goal() else {
                return Vec::new();
            };
            let linked = state.tasks_of_goal(&goal.id);
            match section {
                GoalSection::Overview => overview_text(state, focus),
                GoalSection::Description => field_lines(&goal.description, "No description."),
                GoalSection::Progress => field_lines(
                    state.goal_progress_report.as_deref().unwrap_or(""),
                    "No progress report.",
                ),
                GoalSection::Tasks => linked
                    .iter()
                    .map(|i| {
                        format!(
                            "{:<12}{}",
                            state.tasks[*i].status.as_str(),
                            state.tasks[*i].title
                        )
                    })
                    .collect(),
                GoalSection::Activity => activity_lines(state, focus),
            }
        }
        Focus::TaskDetail(section) => {
            let Some(task) = state.selected_task() else {
                return Vec::new();
            };
            match section {
                TaskSection::Overview => overview_text(state, focus),
                TaskSection::Attachments => task.attachments.clone().unwrap_or_default(),
                TaskSection::Description => field_lines(&task.description, "No description."),
                TaskSection::Result => field_lines(
                    task.proof
                        .as_ref()
                        .and_then(|p| p.agent_summary.as_deref())
                        .unwrap_or(""),
                    "No result summary.",
                ),
                TaskSection::Files => {
                    let Some(proof) = &task.proof else {
                        return Vec::new();
                    };
                    proof.files_changed.clone()
                }
                TaskSection::Activity => activity_lines(state, focus),
            }
        }
        Focus::AgentDetail(section) => {
            let Some(agent) = state.selected_agent() else {
                return Vec::new();
            };
            match section {
                AgentSection::Overview => overview_text(state, focus),
                AgentSection::Role => {
                    field_lines(agent.role.as_deref().unwrap_or(""), "No role description.")
                }
                AgentSection::Error => {
                    let Some(error) = &agent.last_error else {
                        return Vec::new();
                    };
                    let mut lines = vec!["⚠ Error".into()];
                    if let Ok(kind) = error.kind.parse::<AdapterErrorKind>() {
                        let hint = error_hint(kind);
                        lines.push(hint.message.into());
                        lines.push(hint.fix.into());
                        if hint.doctor_hint == Some(true) {
                            lines.push("Diagnostics: orch doctor".into());
                        }
                    }
                    if error
                        .kind
                        .parse::<AdapterErrorKind>()
                        .ok()
                        .is_none_or(|kind| kind == AdapterErrorKind::Unknown)
                    {
                        lines.extend(field_lines(&error.message, "No error message."));
                    }
                    lines.push(error.timestamp.clone());
                    lines
                }
                AgentSection::Activity => activity_lines(state, focus),
            }
        }
        Focus::LogDetail(section) => {
            let Some(message) = state.selected_log() else {
                return Vec::new();
            };
            match section {
                LogSection::Metadata => vec![
                    format!("{} {}", message.msg_type.icon(), message.msg_type.as_str()),
                    format!(
                        "Agent: {}",
                        message
                            .agent_id
                            .as_deref()
                            .map(|id| state.agent_name(id))
                            .unwrap_or_else(|| EM_DASH.into())
                    ),
                    format!("Task: {}", message.task_id.as_deref().unwrap_or(EM_DASH)),
                    format!("Timestamp: {}", message.ts_ms),
                    message.text.clone(),
                ],
                LogSection::Body => state
                    .text_for_source(&ViewerSource::Message(message.id), 0, 0)
                    .map(|document| field_lines(&document.body, "No message body."))
                    .unwrap_or_default(),
            }
        }
        _ => Vec::new(),
    }
}

// ── Overview block (R6) ─────────────────────────────────────────────
//
// One model feeds both the styled block and the plain text that navigation
// and copy read, so the two cannot disagree about what the Overview shows:
//
//   entity -> OverviewBlock (title + rows of label/value fields)
//          -> overview_text   plain lines (section_text, key-side row counts)
//          -> overview_lines  styled rows for the focused body and the pin

/// Cells between a label and its value, past the widest label.
const LABEL_GAP: usize = 2;
/// Cells between a row's left value and its right label.
const COLUMN_GAP: usize = 3;
/// The fewest value cells a column keeps before the block goes single-column:
/// room for every local status word (`in_progress` is the longest).
const MIN_VALUE: usize = 12;

/// A label and its value. The value is a run of coloured segments so a
/// status, a priority or a per-status task count keeps its own token.
struct OverviewField {
    label: &'static str,
    value: Vec<(String, Token)>,
}

impl OverviewField {
    fn new(label: &'static str, value: impl Into<String>, token: Token) -> Self {
        let value = value.into();
        let token = if value == EM_DASH { Token::Dim } else { token };
        OverviewField {
            label,
            value: vec![(value, token)],
        }
    }

    fn plain(&self) -> String {
        self.value.iter().map(|(text, _)| text.as_str()).collect()
    }
}

/// An entity's Overview: `KIND: title` over rows of one or two fields. The
/// first row is the identity line the two-row pin keeps.
struct OverviewBlock {
    kind: &'static str,
    title: String,
    rows: Vec<Vec<OverviewField>>,
}

fn dash_or(text: String) -> String {
    if text.is_empty() {
        EM_DASH.into()
    } else {
        text
    }
}

fn overview_block(state: &AppState, focus: Focus) -> Option<OverviewBlock> {
    use crate::theme::{
        agent_status_token, goal_status_token, priority_token, remote_phase_token,
        task_status_token,
    };
    let assignee = |id: Option<&str>| id.map(|id| state.agent_name(id)).unwrap_or(EM_DASH.into());
    let date = |ts: &str| ts.chars().take(10).collect::<String>();
    match focus {
        Focus::GoalDetail(GoalSection::Overview) => {
            let goal = state.selected_goal()?;
            let linked = state.tasks_of_goal(&goal.id);
            let mut rows = vec![
                vec![
                    OverviewField::new(
                        "status",
                        goal.status.to_string().to_uppercase(),
                        goal_status_token(goal.status),
                    ),
                    OverviewField::new(
                        "assignee",
                        assignee(goal.assignee.as_deref()),
                        Token::Silver,
                    ),
                ],
                vec![
                    OverviewField::new("id", goal.id.clone(), Token::Silver),
                    OverviewField::new("created", date(&goal.created_at), Token::Silver),
                ],
            ];
            if let Some(updated) = goal.updated_at.as_ref().filter(|u| *u != &goal.created_at) {
                rows.push(vec![OverviewField::new(
                    "updated",
                    date(updated),
                    Token::Silver,
                )]);
            }
            let mut tasks = OverviewField::new("tasks", linked.len().to_string(), Token::Silver);
            let breakdown: Vec<_> = TaskStatus::ALL
                .into_iter()
                .filter_map(|status| {
                    let count = linked
                        .iter()
                        .filter(|i| state.tasks[**i].status == status)
                        .count();
                    (count > 0).then(|| (format!("{count} {}", status.as_str()), status))
                })
                .collect();
            if !breakdown.is_empty() {
                tasks.value.push((" (".into(), Token::Dim));
                for (n, (text, status)) in breakdown.into_iter().enumerate() {
                    if n > 0 {
                        tasks.value.push((" · ".into(), Token::Dim));
                    }
                    tasks.value.push((text, task_status_token(status)));
                }
                tasks.value.push((")".into(), Token::Dim));
            }
            rows.push(vec![tasks]);
            Some(OverviewBlock {
                kind: "GOAL",
                title: goal.title.clone(),
                rows,
            })
        }
        Focus::TaskDetail(TaskSection::Overview) => {
            let task = state.selected_task()?;
            let remote = state.remote_run(&task.id);
            let status = match remote.and_then(|r| r.status_label().map(|l| (l, r.phase))) {
                Some((label, phase)) => {
                    OverviewField::new("status", label, remote_phase_token(phase))
                }
                None => OverviewField::new(
                    "status",
                    task.status.to_string(),
                    task_status_token(task.status),
                ),
            };
            let depends: Vec<_> = task
                .depends_on
                .iter()
                .map(|id| {
                    state
                        .task_by_id(id)
                        .map(|t| t.title.clone())
                        .unwrap_or_else(|| id.clone())
                })
                .collect();
            let mut rows = vec![
                vec![
                    status,
                    OverviewField::new(
                        "assignee",
                        assignee(task.assignee.as_deref()),
                        Token::Silver,
                    ),
                ],
                vec![
                    OverviewField::new(
                        "priority",
                        format!("P{}", task.priority),
                        priority_token(task.priority),
                    ),
                    OverviewField::new(
                        "attempts",
                        format!("{}/{}", task.attempts, task.max_attempts),
                        Token::Silver,
                    ),
                ],
                vec![
                    OverviewField::new("labels", dash_or(task.labels.join(", ")), Token::Silver),
                    OverviewField::new("depends", dash_or(depends.join(", ")), Token::Silver),
                ],
            ];
            if let Some(hint) = remote.and_then(|r| r.hint()) {
                rows.push(vec![OverviewField::new("remote", hint, Token::Yellow)]);
            }
            Some(OverviewBlock {
                kind: "DETAIL",
                title: task.title.clone(),
                rows,
            })
        }
        Focus::AgentDetail(AgentSection::Overview) => {
            let agent = state.selected_agent()?;
            let task = agent
                .current_task
                .as_deref()
                .and_then(|id| state.task_by_id(id))
                .map(|t| t.title.as_str())
                .unwrap_or(EM_DASH);
            let team = state
                .teams
                .iter()
                .find(|t| t.members.iter().any(|m| m.agent_id == agent.id))
                .map(|t| t.name.as_str())
                .unwrap_or(EM_DASH);
            let task = OverviewField::new("task", task, Token::Silver);
            let mut rows = vec![vec![
                OverviewField::new(
                    "status",
                    agent.status.to_string(),
                    agent_status_token(agent.status),
                ),
                OverviewField::new("adapter", agent.adapter.clone(), Token::Silver),
            ]];
            if agent.adapter == "shell" {
                rows.push(vec![
                    task,
                    OverviewField::new(
                        "command",
                        agent.config.command.as_deref().unwrap_or(EM_DASH),
                        Token::Silver,
                    ),
                ]);
            } else {
                rows.push(vec![
                    OverviewField::new(
                        "model",
                        agent.config.model.as_deref().unwrap_or(EM_DASH),
                        Token::Silver,
                    ),
                    task,
                ]);
            }
            rows.push(vec![
                OverviewField::new(
                    "runs",
                    format!(
                        "{} ({}/{})",
                        agent.stats.total_runs,
                        agent.stats.tasks_completed,
                        agent.stats.tasks_failed
                    ),
                    Token::Silver,
                ),
                OverviewField::new("team", team, Token::Silver),
            ]);
            if agent.autonomous == Some(true) {
                rows.push(vec![OverviewField::new("auto", "↻ ON", Token::Green)]);
            }
            if let Some(skills) = agent.config.skills.as_ref().filter(|s| !s.is_empty()) {
                rows.push(vec![OverviewField::new(
                    "skills",
                    skills.join(", "),
                    Token::Silver,
                )]);
            }
            Some(OverviewBlock {
                kind: "AGENT",
                title: agent.name.clone(),
                rows,
            })
        }
        _ => None,
    }
}

impl OverviewBlock {
    /// Label column width: the widest label plus the gap to its value.
    fn label_cells(&self) -> usize {
        self.rows
            .iter()
            .flatten()
            .map(|f| f.label.width())
            .max()
            .unwrap_or(0)
            + LABEL_GAP
    }

    /// Where a two-field row's right label starts, as an offset from the
    /// block's left edge, or `None` when `width` cannot give both columns a
    /// label and [`MIN_VALUE`] cells. The left values take their widest
    /// entry's cells, but never more than half the width, so one long value
    /// cannot push every right field to the edge.
    fn right_column(&self, width: usize) -> Option<usize> {
        let label = self.label_cells();
        let widest = self
            .rows
            .iter()
            .filter(|row| row.len() == 2)
            .map(|row| row[0].plain().width())
            .max()?;
        let half = width.saturating_sub(COLUMN_GAP) / 2;
        let cap = half.checked_sub(label).filter(|cap| *cap >= MIN_VALUE)?;
        Some(label + widest.min(cap) + COLUMN_GAP)
    }
}

/// Plain lines for `section_text`: what the block says, without styling.
fn overview_text(state: &AppState, focus: Focus) -> Vec<String> {
    let Some(block) = overview_block(state, focus) else {
        return Vec::new();
    };
    let label = block.label_cells();
    let mut lines = vec![format!("{}: {}", block.kind, block.title)];
    for row in &block.rows {
        let line: Vec<_> = row
            .iter()
            .map(|f| format!("{:<label$}{}", f.label, f.plain()))
            .collect();
        lines.push(line.join("    "));
    }
    lines
}

/// Clip a segmented value to `width` cells by display width, ending in `…`
/// when anything was cut, and pad it to exactly `width` when `pad` is set so
/// the next column always starts on the same cell.
fn clip_value(value: &[(String, Token)], width: usize, pad: bool) -> Vec<(String, Token)> {
    let total: usize = value.iter().map(|(text, _)| text.width()).sum();
    let mut out: Vec<(String, Token)> = Vec::new();
    let mut used = 0;
    if total <= width {
        out.extend(value.iter().cloned());
        used = total;
    } else if width > 0 {
        let budget = width - 1;
        let mut last = value.first().map_or(Token::Silver, |(_, t)| *t);
        'segments: for (text, token) in value {
            let mut kept = String::new();
            for grapheme in text.graphemes(true) {
                let cells = grapheme.width();
                if used + cells > budget {
                    if !kept.is_empty() {
                        out.push((kept, *token));
                    }
                    last = *token;
                    break 'segments;
                }
                used += cells;
                kept.push_str(grapheme);
            }
            if !kept.is_empty() {
                out.push((kept, *token));
            }
        }
        out.push(("…".into(), last));
        used += 1;
    }
    if pad && used < width {
        out.push((" ".repeat(width - used), Token::Silver));
    }
    out
}

/// The block as styled rows at `width` cells: a `━` rule carrying the
/// Amber `KIND: title` chip, then Amber labels over status-coloured values,
/// two fields a row when they fit and one a row when they do not.
fn overview_lines(ctx: &RenderCtx, block: &OverviewBlock, width: usize) -> Vec<Vec<Span<'static>>> {
    use crate::theme::HEAVY_RULE;
    let rule = |cells: usize| HEAVY_RULE.to_string().repeat(cells);
    let mut lines = Vec::new();
    let chip = format!("{}: {}", block.kind, block.title);
    // `━━━` + ` chip ` + at least one trailing `━`.
    let chip = fit_cells(&chip, width.saturating_sub(3 + 2 + 1));
    let used = 3 + chip.width() + 2;
    lines.push(vec![
        Span::styled(rule(3), ctx.style(Token::Ghost)),
        Span::styled(
            format!(" {chip} "),
            ctx.chip(Token::Amber, Token::AccentBg)
                .add_modifier(ratatui::style::Modifier::BOLD),
        ),
        Span::styled(rule(width.saturating_sub(used)), ctx.style(Token::Ghost)),
    ]);
    let label = block.label_cells();
    let field = |f: &OverviewField, cells: usize, pad: bool| {
        let mut spans = vec![Span::styled(
            fit_cells(&format!("{:<label$}", f.label), cells.min(label)),
            ctx.style(Token::Amber),
        )];
        spans.extend(
            clip_value(&f.value, cells.saturating_sub(label), pad)
                .into_iter()
                .map(|(text, token)| Span::styled(text, ctx.style(token))),
        );
        spans
    };
    let right = block.right_column(width);
    for row in &block.rows {
        match (row.as_slice(), right) {
            ([left, second], Some(right)) => {
                let mut spans = field(left, right - COLUMN_GAP, true);
                spans.push(Span::raw(" ".repeat(COLUMN_GAP)));
                spans.extend(field(second, width - right, false));
                lines.push(spans);
            }
            _ => lines.extend(row.iter().map(|f| field(f, width, false))),
        }
    }
    lines
}

fn is_overview(focus: Focus) -> bool {
    matches!(
        focus,
        Focus::GoalDetail(GoalSection::Overview)
            | Focus::TaskDetail(TaskSection::Overview)
            | Focus::AgentDetail(AgentSection::Overview)
    )
}

/// The messages an Activity section lists, in display order, or `None` for a
/// section that is not an Activity section. Rows come from the state-owned
/// `detail_activity_rows`, so order and membership match keys and mouse
/// exactly; membership is structural (task, agent or goal-of-task id), never
/// a text match.
pub(crate) fn activity_messages(state: &AppState, focus: Focus) -> Option<Vec<&Message>> {
    crate::follow::is_detail_activity(focus).then(|| {
        state
            .detail_activity_rows(&state.pane_key(focus))
            .into_iter()
            .map(|i| &state.messages[i])
            .collect()
    })
}

fn activity_lines(state: &AppState, focus: Focus) -> Vec<String> {
    activity_messages(state, focus)
        .unwrap_or_default()
        .into_iter()
        .map(|m| m.text.clone())
        .collect()
}

fn field_lines(text: &str, empty: &str) -> Vec<String> {
    if text.is_empty() {
        vec![empty.into()]
    } else {
        text.split('\n').map(str::to_owned).collect()
    }
}

pub(crate) fn section_sources(state: &AppState, focus: Focus) -> Vec<Option<ViewerSource>> {
    match focus {
        Focus::GoalDetail(GoalSection::Overview)
        | Focus::TaskDetail(TaskSection::Overview)
        | Focus::AgentDetail(AgentSection::Overview)
        | Focus::LogDetail(LogSection::Metadata) => vec![None; section_text(state, focus).len()],
        Focus::AgentDetail(AgentSection::Error) => {
            let lines = section_text(state, focus);
            let mut sources = vec![None; lines.len()];
            if let Some(agent) = state.selected_agent()
                && let Some(error) = &agent.last_error
                && error
                    .kind
                    .parse::<AdapterErrorKind>()
                    .ok()
                    .is_none_or(|kind| kind == AdapterErrorKind::Unknown)
            {
                let raw_lines = field_lines(&error.message, "No error message.");
                let start = lines.len().saturating_sub(raw_lines.len() + 1);
                for source in sources.iter_mut().skip(start).take(raw_lines.len()) {
                    *source = Some(entity_source(EntityType::Agent, &agent.id, "error"));
                }
            }
            sources
        }

        Focus::GoalDetail(GoalSection::Tasks) => state
            .selected_goal()
            .map(|g| {
                state
                    .tasks_of_goal(&g.id)
                    .into_iter()
                    .map(|i| {
                        Some(entity_source(
                            EntityType::Task,
                            &state.tasks[i].id,
                            "description",
                        ))
                    })
                    .collect()
            })
            .unwrap_or_default(),
        focus if crate::follow::is_detail_activity(focus) => activity_messages(state, focus)
            .unwrap_or_default()
            .into_iter()
            .map(|m| Some(ViewerSource::Message(m.id)))
            .collect(),
        _ => {
            let source = match focus {
                Focus::GoalDetail(section) => state.selected_goal().map(|g| {
                    entity_source(
                        EntityType::Goal,
                        &g.id,
                        match section {
                            GoalSection::Description => "description",
                            GoalSection::Progress => "progress",
                            _ => "overview",
                        },
                    )
                }),
                Focus::TaskDetail(section) => state.selected_task().map(|t| {
                    entity_source(
                        EntityType::Task,
                        &t.id,
                        match section {
                            TaskSection::Description => "description",
                            TaskSection::Result => "result",
                            TaskSection::Attachments => "attachments",
                            TaskSection::Files => "files",
                            _ => "overview",
                        },
                    )
                }),
                Focus::AgentDetail(section) => state.selected_agent().map(|a| {
                    entity_source(
                        EntityType::Agent,
                        &a.id,
                        match section {
                            AgentSection::Role => "role",
                            AgentSection::Error => "error",
                            _ => "overview",
                        },
                    )
                }),
                Focus::LogDetail(_) => state.selected_log().map(|m| ViewerSource::Message(m.id)),
                _ => None,
            };
            vec![source; section_text(state, focus).len()]
        }
    }
}

fn selectable(focus: Focus) -> bool {
    crate::follow::is_detail_activity(focus)
        || matches!(
            focus,
            Focus::GoalDetail(GoalSection::Tasks)
                | Focus::TaskDetail(TaskSection::Attachments | TaskSection::Files)
        )
}

/// Visual rows and byte offsets refer to the unmodified source string.
/// Controls are replaced only in displayed cells, never in copy bytes.
pub(crate) fn wrap_rows(text: &str, width: usize) -> Vec<(String, usize)> {
    let mut rows = Vec::new();
    let mut start = 0;
    let mut cells = 0;
    let mut visible = String::new();
    for (byte, grapheme) in text.grapheme_indices(true) {
        if grapheme == "\n" || grapheme == "\r\n" {
            rows.push((std::mem::take(&mut visible), start));
            start = byte + grapheme.len();
            cells = 0;
            continue;
        }
        let mut displayed: String = grapheme
            .chars()
            .map(|ch| {
                if ch.is_control()
                    || matches!(ch, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
                {
                    ' '
                } else {
                    ch
                }
            })
            .collect();
        if grapheme == "\t" {
            displayed = " ".repeat(4 - cells % 4);
        }
        let mut w = displayed.width();
        if cells + w > width.max(1) && !visible.is_empty() {
            rows.push((std::mem::take(&mut visible), start));
            start = byte;
            cells = 0;
            if grapheme == "\t" {
                displayed = " ".repeat(4);
                w = 4;
            }
        }
        visible.push_str(&displayed);
        cells += w;
    }
    rows.push((visible, start));
    rows
}

/// Rail entries in `focus_ring` order, so moving focus never reorders the
/// sections; only the focused entry's label and style change. When the rail
/// is too narrow, the entries farthest from the focused one are dropped
/// first, and the focused label itself is clipped last.
fn rail_labels(
    ring: &[Focus],
    focus: Focus,
    focused_name: &str,
    active: bool,
    width: usize,
) -> Vec<(Focus, String)> {
    let label = |section: Focus| {
        if section == focus {
            format!("{}{focused_name}", if active { "> " } else { "" })
        } else {
            section_name(section).to_owned()
        }
    };
    let mut kept: Vec<Focus> = ring.to_vec();
    let Some(at) = kept.iter().position(|f| *f == focus) else {
        return vec![(focus, fit_cells(&label(focus), width))];
    };
    let mut at = at;
    let total = |kept: &[Focus]| -> usize {
        kept.iter().map(|f| label(*f).width()).sum::<usize>() + 3 * kept.len().saturating_sub(1)
    };
    while kept.len() > 1 && total(&kept) > width {
        // Farthest from the focused entry; on a tie the later one goes.
        let drop = if kept.len() - 1 - at >= at {
            kept.len() - 1
        } else {
            0
        };
        kept.remove(drop);
        if drop < at {
            at -= 1;
        }
    }
    kept.into_iter()
        .map(|section| {
            let text = label(section);
            if section == focus {
                (section, fit_cells(&text, width))
            } else {
                (section, text)
            }
        })
        .collect()
}

pub fn render(frame: &mut Frame, area: Rect, ctx: &RenderCtx) {
    let state = ctx.state;
    let ring: Vec<_> = state
        .focus_ring()
        .into_iter()
        .filter(|f| !matches!(f, Focus::MainList | Focus::Activity | Focus::TextViewer))
        .collect();
    let focus = if ring.contains(&state.focus) {
        state.focus
    } else if ring.contains(&state.detail_focus) {
        state.detail_focus
    } else if let Some(first) = ring.first() {
        *first
    } else {
        return;
    };
    let key = state.pane_key(focus);
    ctx.zone(area, area, ZoneKind::Pane(focus), ZoneId::Pane(key.clone()));
    let active = state.focus == focus;
    let name = match focus {
        focus if crate::follow::is_detail_activity(focus) => format!(
            "Activity · {}",
            if state.detail_activity_following(&key) {
                "following"
            } else if crate::follow::detail_activity_newest_first(focus) {
                "paused · Home follows"
            } else {
                "paused · End follows"
            }
        ),
        Focus::TaskDetail(TaskSection::Files) => state
            .selected_task()
            .and_then(|task| task.proof.as_ref())
            .map(|proof| {
                format!(
                    "Files ({} changed{})",
                    proof.files_changed.len(),
                    proof
                        .branch
                        .as_deref()
                        .map(|b| format!(" · {b}"))
                        .unwrap_or_default()
                )
            })
            .unwrap_or_else(|| "Files".into()),
        _ => section_name(focus).into(),
    };
    let rail = Rect::new(area.x, area.y, area.width, 1);
    let mut x = area.x;
    for (index, (section, text)) in rail_labels(&ring, focus, &name, active, area.width as usize)
        .into_iter()
        .enumerate()
    {
        let text = if index == 0 {
            text
        } else {
            format!(" │ {text}")
        };
        let width = (text.width() as u16).min(area.right().saturating_sub(x));
        let style = if section == focus {
            ctx.bold(if active { Token::Amber } else { Token::Dim })
        } else {
            ctx.style(Token::Dim)
        };
        put_line(
            frame,
            Rect::new(x, rail.y, width, 1),
            rail.y,
            vec![Span::styled(text, style)],
        );
        ctx.zone(
            Rect::new(x, rail.y, width, 1),
            rail,
            ZoneKind::DetailSection(section),
            ZoneId::DetailSection(state.pane_key(section)),
        );
        x += width;
    }
    let footer_rows = u16::from(area.height >= 3);
    let mut body = Rect::new(
        area.x,
        area.y + 1,
        area.width,
        area.height.saturating_sub(1 + footer_rows),
    );
    let overview = ring.iter().copied().find(|f| {
        matches!(
            f,
            Focus::GoalDetail(GoalSection::Overview)
                | Focus::TaskDetail(TaskSection::Overview)
                | Focus::AgentDetail(AgentSection::Overview)
                | Focus::LogDetail(LogSection::Metadata)
        )
    });
    if let Some(overview) = overview.filter(|o| *o != focus && body.height >= 6) {
        // The pin is the block's first two rows: the title rule and the
        // identity row, styled as in the focused block.
        let pinned = match overview_block(state, overview) {
            Some(block) => overview_lines(ctx, &block, body.width as usize),
            None => section_text(state, overview)
                .iter()
                .map(|line| {
                    vec![Span::styled(
                        fit_cells(line, body.width as usize),
                        ctx.style(Token::Dim),
                    )]
                })
                .collect(),
        };
        for (row, spans) in pinned.into_iter().take(2).enumerate() {
            put_line(frame, body, body.y + row as u16, spans);
        }
        let pin = Rect::new(body.x, body.y, body.width, 2);
        ctx.zone(
            pin,
            area,
            ZoneKind::DetailSection(overview),
            ZoneId::DetailSection(state.pane_key(overview)),
        );
        body.y += 2;
        body.height -= 2;
    }
    let mut lines = section_text(state, focus);
    if focus == Focus::AgentDetail(AgentSection::Error)
        && let Some(ts) = state
            .selected_agent()
            .and_then(|agent| agent.last_error.as_ref())
            .and_then(|error| parse_timestamp_ms(&error.timestamp))
    {
        lines.push(format!(
            "{} ago",
            format_duration(ctx.now_ms.saturating_sub(ts))
        ));
    }
    let sources = section_sources(state, focus);
    let is_list = selectable(focus);
    let mut rows = Vec::new();
    let mut byte_offset = 0;
    for (index, line) in lines.iter().enumerate() {
        let source = sources.get(index).cloned().flatten();
        if is_list {
            let continuous = matches!(
                focus,
                Focus::TaskDetail(TaskSection::Attachments | TaskSection::Files)
            );
            rows.push((
                line.clone(),
                source,
                if continuous { byte_offset } else { 0 },
            ));
            if continuous {
                byte_offset += line.len() + 1;
            }
        } else {
            for (text, byte) in wrap_rows(line, body.width as usize) {
                rows.push((text, source.clone(), byte_offset + byte));
            }
            if source.is_some() {
                byte_offset += line.len() + 1;
            }
        }
    }
    let activity = activity_messages(state, focus);
    let styled = is_overview(focus)
        .then(|| overview_block(state, focus))
        .flatten()
        .map(|block| overview_lines(ctx, &block, body.width as usize));
    if let Some(styled) = &styled {
        rows = vec![(String::new(), None, 0); styled.len()];
    }
    if rows.is_empty() {
        let empty = if activity.is_some() {
            "No activity."
        } else {
            "No entries."
        };
        rows.push((empty.into(), None, 0));
    }
    let scroll = if crate::follow::is_detail_activity(focus) {
        state.detail_activity_scroll(&key, body.height as usize)
    } else {
        state.pane_scroll(&key)
    };
    let offset = scroll
        .offset
        .min(rows.len().saturating_sub(body.height as usize));
    let items = if is_list { lines.len() } else { 0 };
    ctx.publish_pane_metrics(PaneMetrics {
        key,
        body,
        items,
        display_rows: rows.len(),
        selection_rows: (0..items).collect(),
    });
    ctx.zone(
        body,
        area,
        ZoneKind::Pane(focus),
        ZoneId::Pane(state.pane_key(focus)),
    );
    // Activity rows group by agent from the top of the window, as the feed does.
    let mut groups = RowGroups::default();
    for (screen_row, (text, source, byte)) in rows
        .iter()
        .skip(offset)
        .take(body.height as usize)
        .enumerate()
    {
        let row = Rect::new(body.x, body.y + screen_row as u16, body.width, 1);
        let selected = is_list && scroll.selected == Some(offset + screen_row);
        if let Some(spans) = styled
            .as_ref()
            .and_then(|lines| lines.get(offset + screen_row))
        {
            put_line(frame, row, row.y, spans.clone());
        } else if let Some(message) = activity
            .as_ref()
            .and_then(|messages| messages.get(offset + screen_row))
        {
            let spans = row_spans(ctx, message, groups.next(message), selected, body.width);
            put_line(frame, row, row.y, spans);
        } else {
            let visible = if selected {
                format!(
                    "> {}",
                    fit_cells(text, body.width.saturating_sub(2) as usize)
                )
            } else {
                fit_cells(text, body.width as usize)
            };
            put_line(
                frame,
                row,
                row.y,
                vec![Span::styled(
                    visible,
                    ctx.style(if selected {
                        Token::Amber
                    } else {
                        Token::Silver
                    }),
                )],
            );
        }
        if let Some(source) = source {
            ctx.zone(
                row,
                body,
                ZoneKind::ExpandText,
                ZoneId::ExpandText(source.clone()),
            );
            ctx.publish_text_anchor(source.clone(), row, *byte);
        }
    }
    if footer_rows > 0 {
        let footer = Rect::new(area.x, area.bottom() - 1, area.width, 1);
        let hint = if state.detail_maximized {
            "M restores to act; Esc closes detail"
        } else {
            "Tab/Esc to list to act; Enter expands; Ctrl+Y copies"
        };
        let feedback = state
            .active_copy_origin()
            .filter(|o| matches!(o, CopyOrigin::Field { .. }) && state.copy_footer_visible(o));
        let idle = feedback.is_none();
        let text = if let Some(origin) = feedback {
            field_feedback(ctx, &origin)
        } else {
            let resize = crate::views::resize_hint(state.detail_maximized);
            if hint.width() + resize.width() + 2 <= footer.width as usize {
                let x = footer.x + hint.width() as u16 + 2;
                crate::views::resize_hint_zones(ctx, footer, x, footer.y, resize);
                format!("{hint}  {resize}")
            } else {
                let compact = if state.detail_maximized {
                    hint
                } else {
                    "Tab/Esc to list to act; M max"
                };
                if let Some(byte) = compact.find('M') {
                    let x = footer.x + compact[..byte].width() as u16;
                    ctx.zone(
                        Rect::new(x, footer.y, 1, 1),
                        footer,
                        ZoneKind::Verb(VerbId::DetailMax),
                        ZoneId::Verb(VerbId::DetailMax),
                    );
                }
                compact.into()
            }
        };
        if idle && let Some(byte) = text.find("Esc") {
            ctx.zone(
                Rect::new(footer.x + text[..byte].width() as u16, footer.y, 3, 1),
                footer,
                ZoneKind::FooterHint(FooterAction::CloseDetail),
                ZoneId::Footer(FooterAction::CloseDetail),
            );
        }
        put_line(
            frame,
            footer,
            footer.y,
            vec![Span::styled(
                fit_cells(&text, footer.width as usize),
                ctx.style(Token::Dim),
            )],
        );
    }
}

pub(crate) fn field_feedback(ctx: &RenderCtx, origin: &CopyOrigin) -> String {
    match ctx.state.feedback_for(origin).map(|f| &f.outcome) {
        None => "Copy pending; F1 help".into(),
        Some(CopyOutcome::NativeWritten) => "Copied on this host; F1 help".into(),
        Some(CopyOutcome::TerminalRequested) => "Requested from terminal; F1 help".into(),
        Some(CopyOutcome::Failed(_)) => "Copy failed; Ctrl+Y retry; F1 help".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(section: TaskSection) -> Focus {
        Focus::TaskDetail(section)
    }

    #[test]
    fn a_narrow_rail_drops_the_entries_farthest_from_focus_and_keeps_order() {
        let ring = [
            task(TaskSection::Overview),
            task(TaskSection::Description),
            task(TaskSection::Result),
            task(TaskSection::Files),
            task(TaskSection::Activity),
        ];
        let focus = task(TaskSection::Files);
        let wide: Vec<_> = rail_labels(&ring, focus, "Files", true, 200)
            .into_iter()
            .map(|(f, _)| f)
            .collect();
        assert_eq!(wide, ring);
        // "Result │ > Files │ Activity" is 27 cells.
        let narrow = rail_labels(&ring, focus, "Files", true, 27);
        assert_eq!(
            narrow,
            [
                (task(TaskSection::Result), "Result".to_owned()),
                (focus, "> Files".to_owned()),
                (task(TaskSection::Activity), "Activity".to_owned()),
            ]
        );
        let alone = rail_labels(&ring, focus, "Files (3 changed · main)", false, 6);
        assert_eq!(alone.len(), 1);
        assert_eq!(alone[0].0, focus);
        assert!(alone[0].1.width() <= 6, "{:?}", alone[0].1);
    }
}
