//! The bottom bar: navigate-mode hints, the command input line and the
//! suggestion panel (docs/parity/tui.md §3.1, §4.4).

use orch_domain::{GoalStatus, TaskStatus};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use ratatui::text::Span;

use crate::command_bar::{resolve_completion, resolve_suggestions};
use crate::format::cap_line;
use crate::hit::{FooterAction, ScrollRegion, VerbId, ZoneId, ZoneKind};
use crate::state::{InputMode, ViewId};
use crate::theme::{BLOCK, CROSS, CURSOR, Token};
use crate::views::{RenderCtx, put_line, spans_width};

const HINT_SUGGESTIONS: &str = "  ↑↓ select  Tab fill  Esc ✕";
const HINT_COMMAND: &str = "  Enter exec  ↑↓ history  Tab complete  Esc ✕";

pub fn render(frame: &mut Frame, area: Rect, ctx: &RenderCtx) {
    if ctx.state.input_mode == InputMode::Command {
        render_input(frame, area, ctx);
    } else {
        render_navigate(frame, area, ctx);
    }
}

fn render_input(frame: &mut Frame, area: Rect, ctx: &RenderCtx) {
    let value = ctx.state.command_bar.input.value().to_string();
    let suggestions = resolve_suggestions(&value);
    let hint = if suggestions.is_empty() {
        HINT_COMMAND
    } else {
        HINT_SUGGESTIONS
    };
    let completion = resolve_completion(&value).unwrap_or_default();

    // The value truncates **from the left** with a leading `…`. Counted in
    // `char`s where TS counts UTF-16 code units, so an astral character (an
    // emoji, say) costs one here and two there; the window differs by at most
    // a character on such input, which is L2 cosmetic (PARITY §12).
    let max_len = (area.width as usize)
        .saturating_sub(6 + hint.chars().count() + completion.chars().count() + 1)
        .max(4);
    let shown = if value.chars().count() > max_len {
        let skip = value.chars().count() - (max_len - 1);
        format!("…{}", value.chars().skip(skip).collect::<String>())
    } else {
        value
    };

    let spans = vec![
        Span::styled("/ ", ctx.style(Token::Amber)),
        Span::styled(shown, ctx.style(Token::White)),
        Span::styled(completion, ctx.style(Token::Ghost)),
        Span::styled(BLOCK, ctx.style(Token::Amber)),
        Span::styled(hint.to_string(), ctx.style(Token::Gray)),
    ];

    // The ghost completion: clicking it accepts it, which is what `Tab` does
    // when there are no suggestions.
    let ghost_x = area.x + spans_width(&spans[..2]);
    let ghost_w = spans_width(&spans[2..3]);
    if ghost_w > 0 {
        ctx.zone(
            Rect::new(ghost_x, area.y, ghost_w, 1),
            area,
            ZoneKind::CommandGhost,
            ZoneId::Verb(VerbId::CommandComplete),
        );
    }
    // The hint row is one span; its verbs are found by name rather than by a
    // hardcoded offset, so editing a hint string cannot silently move a zone.
    // `↑↓ history` / `↑↓ select` stay unclickable — two meanings, one span.
    let hint_x = area.x + spans_width(&spans[..4]);
    for (needle, verb) in [
        ("Enter exec", VerbId::CommandExec),
        ("Tab complete", VerbId::CommandComplete),
        ("Tab fill", VerbId::CommandComplete),
        ("Esc ✕", VerbId::CommandCancel),
    ] {
        if let Some(byte_offset) = hint.find(needle) {
            let offset = hint[..byte_offset].chars().count() as u16;
            let width = unicode_width::UnicodeWidthStr::width(needle) as u16;
            ctx.zone(
                Rect::new(hint_x + offset, area.y, width, 1),
                area,
                ZoneKind::Verb(verb),
                ZoneId::Verb(verb),
            );
        }
    }
    put_line(frame, area, area.y, spans);
}

/// The default footer — conditional key hints plus the item count.
fn render_navigate(frame: &mut Frame, area: Rect, ctx: &RenderCtx) {
    let state = ctx.state;
    let view = state.view;
    let detail_open = state.detail_open();
    let selected_task = state.selected_task();
    let selected_agent = state.selected_agent();
    let selected_goal = state.selected_goal();

    let can_new = !detail_open && ctx.caps.can_create_for(view);
    let can_run =
        view == ViewId::Tasks && selected_task.is_some_and(|t| crate::state::is_runnable(t.status));
    let can_approve =
        view == ViewId::Tasks && selected_task.is_some_and(|t| t.status == TaskStatus::Review);
    let can_delete = match view {
        ViewId::Goals => selected_goal.is_some(),
        ViewId::Tasks => selected_task.is_some_and(|t| t.status != TaskStatus::InProgress),
        ViewId::Agents => selected_agent.is_some(),
        ViewId::Logs => false,
    };

    let mut spans: Vec<Span<'static>> = Vec::new();
    // Each hint is two spans and, when it names an action, one zone over its
    // own `<key> <label>` cells — the two spaces of padding are excluded, so
    // the gap between two hints belongs to neither.
    let hint = |spans: &mut Vec<Span<'static>>,
                key: &str,
                label: &str,
                token: Token,
                action: Option<FooterAction>| {
        let x = area.x + spans_width(spans);
        spans.push(Span::styled(
            key.to_string(),
            ctx.style(token).add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled(format!(" {label}  "), ctx.style(Token::Gray)));
        if let Some(action) = action {
            let width = (area.x + spans_width(spans) - x).saturating_sub(2);
            ctx.zone(
                Rect::new(x, area.y, width, 1),
                area,
                ZoneKind::FooterHint(action),
                ZoneId::Footer(action),
            );
        }
    };

    // The mouse-state indicator, rendered **only** while capture is off — the
    // exceptional state, so it costs zero cells in the common case. It goes
    // **first**, not last: this footer already overflows 100 columns inside
    // `? help`, so an appended marker would be clipped away exactly when the
    // user needs to be told why their clicks stopped working. It is not
    // clickable; the mouse is off.
    if !state.mouse.enabled {
        // Reads as a *state*, not a key hint: "⌨ F2 mouse" could be mistaken
        // for "press F2 for mouse things" by someone who never turned it off.
        spans.push(Span::styled(
            "⌨ mouse off · F2  ".to_string(),
            ctx.style(Token::Amber),
        ));
    }

    // `↑↓` and `Tab/←→` stay unclickable: a 2-cell span with two opposite
    // meanings would have to guess the direction from the sub-cell.
    hint(&mut spans, "↑↓", "", Token::Gray, None);
    hint(&mut spans, "Tab/←→", "", Token::Gray, None);
    hint(
        &mut spans,
        "/",
        "cmd",
        Token::Gray,
        Some(FooterAction::CommandMode),
    );
    if can_new {
        hint(&mut spans, "N", "new", Token::Gray, Some(FooterAction::New));
    }
    if can_run {
        hint(&mut spans, "R", "run", Token::Gray, Some(FooterAction::Run));
    }
    if view == ViewId::Tasks && selected_task.is_some() {
        hint(
            &mut spans,
            "C",
            "cancel",
            Token::Amber,
            Some(FooterAction::Cancel),
        );
    }
    if can_approve {
        hint(
            &mut spans,
            "A",
            "approve",
            Token::Green,
            Some(FooterAction::Approve),
        );
        hint(
            &mut spans,
            "X",
            "reject",
            Token::Red,
            Some(FooterAction::Reject),
        );
    }
    if matches!(view, ViewId::Tasks | ViewId::Agents | ViewId::Goals) {
        hint(
            &mut spans,
            "E",
            "edit",
            Token::Cyan,
            Some(FooterAction::Edit),
        );
    }
    if view == ViewId::Agents && selected_agent.is_some() {
        hint(
            &mut spans,
            "S",
            "stop",
            Token::Red,
            Some(FooterAction::StopAgent),
        );
        let autonomous = selected_agent.and_then(|a| a.autonomous).unwrap_or(false);
        hint(
            &mut spans,
            "U",
            if autonomous { "auto off" } else { "auto on" },
            Token::Cyan,
            Some(FooterAction::ToggleAutonomous),
        );
    }
    if view == ViewId::Goals
        && let Some(goal) = selected_goal
    {
        hint(
            &mut spans,
            "P",
            if goal.status == GoalStatus::Active {
                "pause"
            } else {
                "resume"
            },
            Token::Amber,
            Some(FooterAction::PauseGoal),
        );
    }
    if view == ViewId::Tasks {
        hint(
            &mut spans,
            "S",
            if state.show_all_tasks {
                "collapse"
            } else {
                "show all"
            },
            Token::Gray,
            Some(FooterAction::ShowAllTasks),
        );
    }
    if can_delete && !can_approve {
        hint(
            &mut spans,
            "D",
            "delete",
            Token::Gray,
            Some(FooterAction::Delete),
        );
    }
    if !state.pending_deletions.is_empty() {
        hint(
            &mut spans,
            "Z",
            "undo",
            Token::Yellow,
            Some(FooterAction::Undo),
        );
    }
    if detail_open {
        hint(
            &mut spans,
            "Esc",
            "close",
            Token::Gray,
            Some(FooterAction::CloseDetail),
        );
    } else if matches!(view, ViewId::Tasks | ViewId::Agents | ViewId::Goals) {
        hint(
            &mut spans,
            "Enter",
            "detail",
            Token::Gray,
            Some(FooterAction::Enter),
        );
    }
    // `Q quit` arms before it exits, and the arming is shown in place rather
    // than as a toast: at most two toasts are ever drawn (`state.rs:985`), so
    // a toast could be invisible exactly when it matters (design §3.5).
    if state.mouse.quit_armed(ctx.now_ms) {
        hint(
            &mut spans,
            "Q",
            "quit?",
            Token::Amber,
            Some(FooterAction::Quit),
        );
    } else {
        hint(
            &mut spans,
            "Q",
            "quit",
            Token::Gray,
            Some(FooterAction::Quit),
        );
    }
    hint(
        &mut spans,
        "?",
        "help",
        if state.orch_state.onboarding_completed == Some(true) {
            Token::Gray
        } else {
            Token::Amber
        },
        Some(FooterAction::Help),
    );

    let count = match view {
        ViewId::Goals => state.goals.len(),
        ViewId::Tasks => state.tasks.len(),
        ViewId::Agents => state.agents.len(),
        ViewId::Logs => state.messages.len(),
    };
    if count > 0 {
        spans.push(Span::styled(
            format!("{count} {}", view.item_label()),
            ctx.style(Token::Ghost),
        ));
    }

    put_line(frame, area, area.y, spans);
}

/// `SuggestionsPanel`.
pub fn render_suggestions(frame: &mut Frame, area: Rect, ctx: &RenderCtx) {
    let value = ctx.state.command_bar.input.value();
    let suggestions = resolve_suggestions(value);
    if suggestions.is_empty() || area.height == 0 {
        return;
    }
    let selected = ctx.state.command_bar.suggestion_index;
    let max_visible = area.height as usize;
    let scroll_start = selected
        .saturating_sub(max_visible.saturating_sub(1))
        .min(suggestions.len().saturating_sub(1));

    ctx.zone(
        area,
        area,
        ZoneKind::Scroll(ScrollRegion::CommandSuggestions),
        ZoneId::ScrollRegion(ScrollRegion::CommandSuggestions),
    );

    let cmd_pad = suggestions
        .iter()
        .map(|s| s.cmd.chars().count() + 1)
        .max()
        .unwrap_or(14)
        .clamp(14, 32);

    for (row, index) in (scroll_start..suggestions.len())
        .take(max_visible)
        .enumerate()
    {
        let suggestion = &suggestions[index];
        let is_selected = index == selected;
        // Identity is the command text, so a list that re-resolves between a
        // press and a release cannot fire the wrong verb.
        ctx.row_zone(
            area,
            area.y + row as u16,
            ZoneKind::SuggestionRow { index },
            ZoneId::Suggestion(suggestion.cmd.to_string()),
        );
        let marker = if is_selected {
            format!("  {CURSOR} ")
        } else {
            "    ".to_string()
        };
        let subs = suggestion
            .subs
            .as_deref()
            .map(|s| format!("  {s}"))
            .unwrap_or_default();
        let desc_width = (area.width as usize)
            .saturating_sub(cmd_pad + subs.chars().count() + 9)
            .max(4);
        let spans = vec![
            Span::styled(
                marker,
                ctx.style(if is_selected {
                    Token::Amber
                } else {
                    Token::Ghost
                }),
            ),
            Span::styled(
                format!("{:<width$}", suggestion.cmd, width = cmd_pad),
                if is_selected {
                    ctx.bold(Token::White)
                } else {
                    ctx.style(Token::Silver)
                },
            ),
            Span::styled(
                cap_line(&suggestion.desc, desc_width),
                ctx.style(Token::Dim),
            ),
            Span::styled(subs, ctx.style(Token::Ghost)),
        ];
        put_line(frame, area, area.y + row as u16, spans);
    }
}

/// The `✕` glyph used by both hint strings.
pub const HINT_CLOSE_GLYPH: &str = CROSS;
