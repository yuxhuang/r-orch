//! The bottom bar: navigate-mode hints, the command input line and the
//! suggestion panel.

use orch_domain::{GoalStatus, TaskStatus};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use ratatui::text::Span;

use crate::command_bar::{
    MSG_VERB, Suggestion, SuggestionCtx, resolve_completion, resolve_suggestions_in,
};
use crate::format::{cap_display, cap_line};
use crate::hit::{FooterAction, ScrollRegion, VerbId, ZoneId, ZoneKind};
use crate::msg::{Chip, chip, parse_partial};
use crate::state::{InputMode, ViewId};
use crate::text::cursor::single_line_grapheme;
use crate::text::{display_width, text_display_width};
use crate::theme::{CROSS, CURSOR, RETURN_GLYPH, Token};
use crate::views::{RenderCtx, put_line, spans_width};

const HINT_SUGGESTIONS: &str = "  ↑↓ select  Tab fill  Esc ✕";
const HINT_COMMAND: &str = "  Enter exec  ↑↓ history  Tab complete  Esc ✕";
/// What is left of the hint when a recipient chip is taking the room: the
/// cancel, because it is the only clickable way out of the command bar.
const HINT_CHIP: &str = "  Esc ✕";

/// The rows for the current input. `/msg` needs the agent list, so the widget
/// cannot use the `&'static` shim.
fn suggestions_for(ctx: &RenderCtx, value: &str) -> Vec<Suggestion> {
    let order = ctx.state.sorted_agents_unfiltered();
    resolve_suggestions_in(
        value,
        &SuggestionCtx {
            agents: &ctx.state.agents,
            order: &order,
        },
    )
}

pub fn render(frame: &mut Frame, area: Rect, ctx: &RenderCtx) {
    match ctx.state.input_mode {
        InputMode::Command => render_input(frame, area, ctx),
        InputMode::Keyword => render_keyword_input(frame, area, ctx),
        _ => render_navigate(frame, area, ctx),
    }
}

const HINT_KEYWORD: &str = "  Enter keep  Esc clear";

/// The keyword prompt (Ctrl+K): `Filter tasks › ship   2/5  Enter keep  Esc
/// clear`. The list above narrows as it is typed; the count says by how
/// much, including to `0/5`. Drawn with the command bar's own window, so a
/// long keyword scrolls around the cursor instead of pushing it off screen.
fn render_keyword_input(frame: &mut Frame, area: Rect, ctx: &RenderCtx) {
    let view = ctx.state.view;
    let Some(input) = ctx.state.keyword_input(view) else {
        return;
    };
    let label = format!("Filter {} › ", view.item_label());
    let (matches, total) = ctx.state.keyword_counts(view);
    let count = format!("  {matches}/{total}");
    let mut hint = HINT_KEYWORD;
    let at_end = input.cursor().pos() == input.cursor().len();
    let fixed = |hint: &str| {
        label.chars().count() + usize::from(at_end) + count.chars().count() + hint.chars().count()
    };
    // The hint goes before the value's window does; the clickable `Esc
    // clear` then goes with it, but the key still works.
    if (area.width as usize).saturating_sub(fixed(hint)) < MIN_VALUE_COLUMNS {
        hint = "";
    }
    let value_cols = (area.width as usize).saturating_sub(fixed(hint)).max(4);
    let window = value_window(input.cursor(), value_cols);

    let mut spans = vec![Span::styled(label, ctx.style(Token::Amber))];
    if window.clipped_left {
        spans.push(Span::styled("…".to_string(), ctx.style(Token::Ghost)));
    }
    spans.extend(value_spans(&window.before, ctx));
    spans.extend(cursor_spans(&window.after, ctx));
    spans.push(Span::styled(
        count,
        ctx.style(if matches == 0 {
            Token::Red
        } else {
            Token::Ghost
        }),
    ));
    let hint_at = spans.len();
    spans.push(Span::styled(hint.to_string(), ctx.style(Token::Gray)));

    let hint_x = area.x + spans_width(&spans[..hint_at]);
    for (needle, verb) in [
        ("Enter keep", VerbId::KeywordKeep),
        ("Esc clear", VerbId::KeywordClear),
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

/// Columns the editable value keeps even on a narrow terminal. Below this the
/// hint is dropped, then the chip is shortened — the chip is never dropped
/// while a recipient is resolved, because it is the only thing on screen that
/// says who is about to be messaged.
const MIN_VALUE_COLUMNS: usize = 6;

/// Columns the editable value keeps **while a chip is shown**, below which the
/// 46-column hint goes first.
///
/// With both the chip and the full hint, 80 columns leaves 8 for the body —
/// about one word of what the user is typing (gate F1). The chip is the
/// recipient they just chose and cannot be re-read anywhere else; the hint
/// names four keys they are already pressing. So the hint is cut back to
/// [`HINT_CHIP`], and then dropped, as soon as the window would fall below
/// what an uncropped bar has at 80 columns. Without a chip the old floor
/// stands, so no existing verb's hint moves.
const MIN_VALUE_COLUMNS_WITH_CHIP: usize = 28;

/// Columns the shortened chip keeps.
const MIN_CHIP_COLUMNS: usize = 8;

fn render_input(frame: &mut Frame, area: Rect, ctx: &RenderCtx) {
    let input = &ctx.state.command_bar.input;
    let value = input.value();
    let suggestions = suggestions_for(ctx, value);
    let hint = if suggestions.is_empty() {
        HINT_COMMAND
    } else {
        HINT_SUGGESTIONS
    };
    let completion = resolve_completion(value).unwrap_or_default();

    // The chip pins the recipient in front of the body, so the body truncates
    // and the recipient never does (design-v2 §3.6.1): at 80 columns the
    // editable window is 29 and `/msg "Code Reviewer" ` is already 21, so
    // without this the recipient scrolls out of view after ~8 body
    // characters — exactly when the `m` prefill made it the thing the user
    // chose deliberately.
    let chip = recipient_chip(ctx, value);

    // Everything but the value, the hint and the chip, as today: `/ `, the
    // ghost completion and, only at end of value, a blank cursor cell.
    let at_end = input.cursor().pos() == input.cursor().len();
    let fixed = 6 + completion.chars().count() + usize::from(at_end);
    let mut hint_cols = hint.chars().count();
    let mut chip_cols = chip
        .as_ref()
        .map(|chip| text_display_width(&chip.text) + 3)
        .unwrap_or(0);
    let budget = |hint_cols: usize, chip_cols: usize| {
        (area.width as usize)
            .saturating_sub(fixed + hint_cols + chip_cols)
            .max(4)
    };
    let mut value_cols = budget(hint_cols, chip_cols);
    let mut chip_text = chip.as_ref().map(|chip| chip.text.clone());
    let mut hint = hint;
    if chip.is_some() {
        // Widest hint that still leaves the body a readable window. The short
        // one keeps `Esc ✕`, which is the only *clickable* way out of the bar
        // — dropping the hint outright would leave a mouse-only user no
        // cancel at 80 columns.
        for candidate in [hint, HINT_CHIP, ""] {
            hint = candidate;
            hint_cols = candidate.chars().count();
            value_cols = budget(hint_cols, chip_cols);
            if value_cols >= MIN_VALUE_COLUMNS_WITH_CHIP || candidate.is_empty() {
                break;
            }
        }
    } else if value_cols < MIN_VALUE_COLUMNS && hint_cols > 0 {
        hint_cols = 0;
        value_cols = budget(hint_cols, chip_cols);
    }
    if value_cols < MIN_VALUE_COLUMNS
        && let Some(text) = chip_text.as_mut()
    {
        let spare = chip_cols.saturating_sub(MIN_VALUE_COLUMNS - value_cols);
        *text = cap_display(text, spare.saturating_sub(3).max(MIN_CHIP_COLUMNS));
        chip_cols = text_display_width(text) + 3;
        value_cols = budget(hint_cols, chip_cols);
    }

    let window = value_window(input.cursor(), value_cols);

    let mut spans = vec![Span::styled("/ ", ctx.style(Token::Amber))];
    if let (Some(text), Some(chip)) = (&chip_text, &chip) {
        spans.push(Span::styled(text.clone(), ctx.style(chip.tone)));
        spans.push(Span::styled(" │ ", ctx.style(Token::Ghost)));
    }
    if window.clipped_left {
        spans.push(Span::styled("…".to_string(), ctx.style(Token::Ghost)));
    }
    spans.extend(value_spans(&window.before, ctx));
    // The ghost belongs at the **end of the value**, which is off screen when
    // the window clipped the text after the cursor — drawn there it would read
    // as a completion of whatever the clip stopped at, so it is dropped
    // instead (gate F4).
    let ghost = if window.clipped_right {
        String::new()
    } else {
        completion.clone()
    };
    spans.extend(cursor_spans(&window.after, ctx));
    let ghost_at = spans.len();
    spans.push(Span::styled(ghost.clone(), ctx.style(Token::Ghost)));
    let hint_at = spans.len();
    spans.push(Span::styled(hint.to_string(), ctx.style(Token::Gray)));

    // The ghost completion: clicking it accepts it, which is what `Tab` does
    // when there are no suggestions.
    let ghost_x = area.x + spans_width(&spans[..ghost_at]);
    let ghost_w = unicode_width::UnicodeWidthStr::width(ghost.as_str()) as u16;
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
    let hint_x = area.x + spans_width(&spans[..hint_at]);
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

/// The recipient chip for a `/msg` input, or `None` for every other verb.
///
/// Live-resolved against `state.agents` through the *same* `msg::resolve`
/// path the send uses, so the chip and the refusal cannot disagree.
fn recipient_chip(ctx: &RenderCtx, value: &str) -> Option<Chip> {
    let tail = value.strip_prefix('/')?;
    let (verb, rest) = tail.split_once(char::is_whitespace)?;
    if verb != MSG_VERB {
        return None;
    }
    let token = parse_partial(rest)?;
    // Mid-quote the name is not knowable yet, so nothing is claimed about it.
    if !token.complete {
        return None;
    }
    let body = token.rest.clone();
    chip(&ctx.state.agents, &token, &body)
}

/// The slice of the value to draw, split at the cursor.
struct ValueWindow {
    /// The kept text before the cursor.
    before: String,
    /// The kept text from the cursor on.
    after: String,
    /// Text was dropped from the left, so the bar draws a leading `…`.
    clipped_left: bool,
    /// Text was dropped from the right — past the cursor, so the end of the
    /// value is not on screen.
    clipped_right: bool,
}

/// The window of the value to show, split at the cursor.
///
/// The bar's analogue of the wizard's `slide_viewport`, differing in two ways
/// it needs: a leading `…` so hidden text is visible as hidden, and a window
/// **anchored on the cursor** rather than on the end — otherwise editing the
/// middle of a long command moves the insertion point off screen, which is
/// what made the `\"` escapes in a quoted name unworkable to edit by hand.
///
/// `budget` is the whole window, the `…` included. A mid-line cursor
/// overlays its grapheme; only an end-of-value blank is reserved by the
/// caller's `fixed`. The tail must keep the whole cursor grapheme visible.
///
/// **Linear in the columns drawn, not in the length of the value.** Both walks
/// start at the cursor and stop at `budget`, so a pasted stack trace costs the
/// same per frame as a short command. The form this replaced re-concatenated
/// and re-measured the whole suffix once per grapheme — O(n²) on *every* frame
/// and for every verb, 5 s at 20k characters (gate S1/F1).
fn value_window(cursor: &crate::text::Cursor, budget: usize) -> ValueWindow {
    let before_segs = cursor.before_segs();
    let after_segs = cursor.after_segs();

    // With the cursor mid-line, the text *after* it is what the edit is being
    // made against — the `"` being closed, the word being pushed along — so
    // the window holds a slice of it back rather than letting the text before
    // the cursor spend the whole budget.
    let tail_reserve = if after_segs.is_empty() {
        0
    } else {
        (budget / 3).max(display_width(&after_segs[0])).min(budget)
    };
    let head = budget - tail_reserve;

    let mut start = before_segs.len();
    let mut used = 0;
    while start > 0 {
        let width = display_width(&before_segs[start - 1]);
        if used + width > head {
            break;
        }
        used += width;
        start -= 1;
    }
    // Dropping text from the left costs a column for the `…`, which is only
    // known once the walk has stopped short — so give one back here.
    let clipped_left = start > 0;
    while clipped_left && start < before_segs.len() && used + 1 > head {
        used -= display_width(&before_segs[start]);
        start += 1;
    }

    let mut remaining = budget.saturating_sub(used + usize::from(clipped_left));
    let mut after = String::new();
    let mut kept = 0;
    for segment in after_segs {
        let width = display_width(segment);
        if width > remaining {
            break;
        }
        remaining -= width;
        after.push_str(segment);
        kept += 1;
    }

    ValueWindow {
        before: before_segs[start..].concat(),
        after,
        clipped_left,
        clipped_right: kept < after_segs.len(),
    }
}

/// Display-only input spans: visible zero-width placeholders and dim newlines.
fn value_spans(text: &str, ctx: &RenderCtx) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    for (index, part) in text.split('\n').enumerate() {
        if index > 0 {
            spans.push(Span::styled(RETURN_GLYPH, ctx.style(Token::Dim)));
        }
        if !part.is_empty() {
            let displayed: String = crate::text::grapheme_segments(part)
                .iter()
                .map(|g| single_line_grapheme(g))
                .collect();
            spans.push(Span::styled(displayed, ctx.style(Token::White)));
        }
    }
    spans
}

/// Overlay the first displayed grapheme, or a blank at end of value. The
/// cursor uses the same text colours as the editor, reversed by the terminal.
fn cursor_spans(after: &str, ctx: &RenderCtx) -> Vec<Span<'static>> {
    let segments = crate::text::grapheme_segments(after);
    let Some(segment) = segments.first() else {
        return vec![Span::styled(" ", ctx.style(Token::White).reversed())];
    };
    let mut spans = value_spans(segment, ctx);
    for span in &mut spans {
        span.style = span.style.reversed();
    }
    spans.extend(value_spans(&after[segment.len()..], ctx));
    spans
}

/// The default footer — conditional key hints plus the item count.
fn render_navigate(frame: &mut Frame, area: Rect, ctx: &RenderCtx) {
    let state = ctx.state;
    let view = state.view;
    let detail_open = state.detail_open();
    let selected_task = state.selected_task();
    let selected_agent = state.selected_agent();
    let selected_goal = state.selected_goal();

    if state.focus != crate::state::Focus::MainList {
        // The hint names the feed's live edge: newest-first detail Activity
        // follows at the top (Home), every other feed at the bottom (End).
        let text = if crate::follow::detail_activity_newest_first(state.focus) {
            "↑↓ scroll; Home follows; Down pauses; Tab panes; Enter expands; F1 help"
        } else if state.is_live_feed(state.focus) {
            "↑↓ scroll; End follows; Up pauses; Tab panes; Enter expands; F1 help"
        } else if state.detail_maximized {
            "M restores to act; Esc closes detail; Ctrl+Y copy; F1 help"
        } else if detail_open {
            "Tab/Esc to list to act; Enter expands; Ctrl+Y copy; F1 help"
        } else {
            "Tab next pane; Enter expands; Ctrl+Y copy; F1 help"
        };
        put_line(
            frame,
            area,
            area.y,
            vec![Span::styled(text, ctx.style(Token::Dim))],
        );
        if let Some(byte) = text.find("Esc") {
            ctx.zone(
                Rect::new(area.x + byte as u16, area.y, 3, 1),
                area,
                ZoneKind::FooterHint(FooterAction::CloseDetail),
                ZoneId::Footer(FooterAction::CloseDetail),
            );
        }
        if text.starts_with('M') {
            ctx.zone(
                Rect::new(area.x, area.y, 1, 1),
                area,
                ZoneKind::Verb(VerbId::DetailMax),
                ZoneId::Verb(VerbId::DetailMax),
            );
        }
        return;
    }
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

    // The Agents grouping leads, at every width: the header's `GROUP:` chip
    // only fits from `GROUP_CHIP_FROM` columns, and this is where the mode
    // shows below that. A sort whose column is hidden at this width is named
    // here too, so it is never a silent mode, and the UPDATED markers are
    // spelled out while they are what the list is sorted by.
    if view == ViewId::Agents {
        // Padded to the longest mode word, so a click never moves anything
        // to its right: the hint can fire again at once (see `fire_footer`).
        hint(
            &mut spans,
            "b",
            &format!("{:<7}", state.agent_group_by.label()),
            Token::Cyan,
            Some(FooterAction::AgentGroupBy),
        );
        let sort = state.agent_sort;
        if !crate::views::agents::AgentColumns::for_width(area.width as usize).shows(sort.field) {
            let arrow = if sort.descending { "↓" } else { "↑" };
            hint(
                &mut spans,
                "o",
                &format!("{}{arrow}", sort.field.label()),
                Token::Amber,
                Some(FooterAction::AgentSortField),
            );
        }
        if sort.field == crate::state::AgentSortField::Updated {
            hint(&mut spans, "R", "run · T task", Token::Gray, None);
        }
    }

    // `↑↓` and `Tab/←→` stay unclickable: a 2-cell span with two opposite
    // meanings would have to guess the direction from the sub-cell.
    hint(&mut spans, "↑↓", "", Token::Gray, None);
    hint(&mut spans, "Tab panes; ←→ views", "", Token::Gray, None);
    hint(
        &mut spans,
        "/",
        "cmd",
        Token::Gray,
        Some(FooterAction::CommandMode),
    );
    if matches!(view, ViewId::Goals | ViewId::Tasks | ViewId::Agents) {
        hint(
            &mut spans,
            "^K",
            "filter",
            Token::Gray,
            Some(FooterAction::Filter),
        );
    }
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
        // `m` is advertised here, and in the help overlay's NAVIGATION column
        // rather than its ACTIONS column, which is already at `MAX_ROWS`
        // (design-v2 §6). Clickable like its neighbours.
        if ctx.caps.can_send_message {
            hint(
                &mut spans,
                "m",
                "msg",
                Token::Cyan,
                Some(FooterAction::Message),
            );
        }
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
    if view == ViewId::Logs {
        hint(&mut spans, "i", "inspector", Token::Dim, None);
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

    // A filtered list counts its matches against the whole: `2/5 tasks`.
    let (matches, count) = state.keyword_counts(view);
    if count > 0 {
        let shown = if state.keyword(view).is_empty() {
            count.to_string()
        } else {
            format!("{matches}/{count}")
        };
        spans.push(Span::styled(
            format!("{shown} {}", view.item_label()),
            ctx.style(Token::Ghost),
        ));
    }

    put_line(frame, area, area.y, spans);
}

/// `SuggestionsPanel`.
pub fn render_suggestions(frame: &mut Frame, area: Rect, ctx: &RenderCtx) {
    // A retained refusal owns the panel while it lasts. These rows are the
    // only place it can be read — the activity feed, where the lines also
    // land, is not drawn at all in command mode — and the suggestion list for
    // the retained text would cover it: `/msg Alice ` with no body is refused
    // and leaves a bar that lists every agent again (QA repro B). The next
    // keystroke clears the refusal and the list comes back.
    if !ctx.state.command_bar.retained_refusal.is_empty() {
        render_retained_refusal(frame, area, ctx);
        return;
    }
    let suggestions = suggestions_for(ctx, ctx.state.command_bar.input.value());
    if suggestions.is_empty() || area.height == 0 {
        return;
    }
    let selected = ctx.state.command_bar.suggestion_index;
    let max_visible = area.height as usize;
    let mut scroll_start = ctx
        .state
        .suggestion_scroll
        .min(suggestions.len().saturating_sub(max_visible));
    if selected < scroll_start {
        scroll_start = selected;
    } else if selected >= scroll_start.saturating_add(max_visible) {
        scroll_start = selected.saturating_add(1).saturating_sub(max_visible);
    }

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
            // The agent id for an agent row, so two identically named agents
            // get distinct press/release identities — release is matched on
            // `ZoneId`, not position.
            ZoneId::Suggestion(suggestion.zone_key().to_string()),
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

/// The refusal kept on screen with the bar, drawn **bottom-aligned** so it
/// sits directly above the command it is about rather than a panel's height
/// away from it.
fn render_retained_refusal(frame: &mut Frame, area: Rect, ctx: &RenderCtx) {
    let lines = &ctx.state.command_bar.retained_refusal;
    if lines.is_empty() || area.height == 0 {
        return;
    }
    let shown = lines.len().min(area.height as usize);
    let top = area.y + area.height - shown as u16;
    for (row, line) in lines[lines.len() - shown..].iter().enumerate() {
        // `⚠` on the first line only: the rest are continuations of it, and
        // for the shadow refusal they are the two commands to choose between.
        let marker = if row == 0 && shown == lines.len() {
            "  ⚠ "
        } else {
            "    "
        };
        // Measured in display columns, like the chip: an agent name in a
        // refusal may be CJK or emoji — and so is the marker itself, whose
        // `⚠` is three bytes and one column.
        let budget = (area.width as usize).saturating_sub(text_display_width(marker));
        put_line(
            frame,
            area,
            top + row as u16,
            vec![
                Span::styled(marker, ctx.style(Token::Yellow)),
                Span::styled(cap_display(line, budget), ctx.style(Token::Yellow)),
            ],
        );
    }
}

/// The `✕` glyph used by both hint strings.
pub const HINT_CLOSE_GLYPH: &str = CROSS;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text::Cursor;

    #[test]
    fn command_and_keyword_cursors_overlay_wide_graphemes_without_shifting_text() {
        use crate::{App, backend::FakeBackend, clock::TestClock};
        use ratatui::{Terminal, backend::TestBackend};
        for mode in [InputMode::Command, InputMode::Keyword] {
            for (value, pos, x, symbol) in [
                ("hi", 0, 0, "h"),
                ("a界🙂z", 1, 1, "界"),
                ("a界🙂z", 2, 3, "🙂"),
            ] {
                let mut app = App::new(FakeBackend::new(), TestClock::new(1000));
                app.state.input_mode = mode;
                let input = if mode == InputMode::Command {
                    &mut app.state.command_bar.input
                } else {
                    app.state.keyword_input_mut(app.state.view).unwrap()
                };
                *input.cursor_mut() = Cursor::at(value, pos);
                let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
                terminal
                    .draw(|frame| {
                        crate::views::draw(frame, &app);
                    })
                    .unwrap();
                let prefix = if mode == InputMode::Command {
                    2
                } else {
                    text_display_width(&format!("Filter {} › ", app.state.view.item_label())) as u16
                };
                let buffer = terminal.backend().buffer();
                let y = (0..30)
                    .find(|&y| {
                        buffer[(0, y)].symbol()
                            == if mode == InputMode::Command { "/" } else { "F" }
                    })
                    .unwrap();
                let cell = &buffer[(prefix + x, y)];
                assert_eq!(cell.symbol(), symbol, "{mode:?}");
                assert!(cell.modifier.contains(Modifier::REVERSED), "{mode:?}");
                if value == "hi" {
                    assert_eq!(buffer[(prefix + 1, y)].symbol(), "i");
                } else {
                    assert_eq!(buffer[(prefix + 5, y)].symbol(), "z");
                }
            }
        }
    }

    #[test]
    fn moving_short_inputs_preserves_columns_on_every_palette() {
        use crate::{App, backend::FakeBackend, clock::TestClock};
        use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        use ratatui::{Terminal, backend::TestBackend};
        for palette in orch_domain::TUI_PALETTE_NAMES {
            for mode in [InputMode::Command, InputMode::Keyword] {
                let mut app = App::new(FakeBackend::new(), TestClock::new(1000));
                app.state.palette = crate::theme::Palette::new(palette);
                app.state.input_mode = mode;
                let input = if mode == InputMode::Command {
                    &mut app.state.command_bar.input
                } else {
                    app.state.keyword_input_mut(app.state.view).unwrap()
                };
                *input.cursor_mut() = Cursor::new("hi");
                let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
                for (key, mods, pos) in [
                    (KeyCode::Left, KeyModifiers::NONE, 1),
                    (KeyCode::Left, KeyModifiers::NONE, 0),
                    (KeyCode::Right, KeyModifiers::NONE, 1),
                    (KeyCode::Home, KeyModifiers::NONE, 0),
                    (KeyCode::End, KeyModifiers::NONE, 2),
                    (KeyCode::Char('a'), KeyModifiers::CONTROL, 0),
                    (KeyCode::Char('e'), KeyModifiers::CONTROL, 2),
                ] {
                    app.handle_key(KeyEvent::new(key, mods));
                    terminal
                        .draw(|frame| {
                            crate::views::draw(frame, &app);
                        })
                        .unwrap();
                    let prefix = if mode == InputMode::Command {
                        2
                    } else {
                        text_display_width(&format!("Filter {} › ", app.state.view.item_label()))
                            as u16
                    };
                    let buffer = terminal.backend().buffer();
                    let y = (0..24)
                        .find(|&y| {
                            buffer[(0, y)].symbol()
                                == if mode == InputMode::Command { "/" } else { "F" }
                        })
                        .unwrap();
                    assert_eq!(
                        buffer[(prefix, y)].symbol(),
                        "h",
                        "{palette:?} {mode:?} {key:?}"
                    );
                    assert_eq!(buffer[(prefix + 1, y)].symbol(), "i");
                    let caret = &buffer[(prefix + pos, y)];
                    assert!(
                        caret.modifier.contains(Modifier::REVERSED),
                        "{palette:?} {mode:?} {key:?}"
                    );
                    assert_eq!(caret.fg, app.state.palette.get(Token::White));
                }
            }
        }
    }

    #[test]
    fn a_wide_cursor_keeps_its_whole_grapheme_at_a_clipped_boundary() {
        for cluster in ["界", "🙂", "👩‍💻"] {
            for budget in 4..24 {
                let value = format!("{}{}z", "a".repeat(100), cluster);
                let window = value_window(&Cursor::at(&value, 100), budget);
                assert!(window.clipped_left);
                assert!(window.after.starts_with(cluster));
                assert!(text_display_width(&drawn(&window)) <= budget);
            }
        }
        // A mid-line overlay spends the entire budget on the value.
        let window = value_window(&Cursor::at("hi", 0), 2);
        assert_eq!(drawn(&window), "hi");
        let window = value_window(&Cursor::at("界🙂", 0), 4);
        assert_eq!(drawn(&window), "界🙂");
    }

    /// What the bar draws for the value, `…` included — the whole window.
    fn drawn(window: &ValueWindow) -> String {
        let mut out = String::new();
        if window.clipped_left {
            out.push('…');
        }
        out.push_str(&window.before);
        out.push_str(&window.after);
        out
    }

    #[test]
    fn the_window_spends_the_whole_budget_with_the_cursor_at_the_end() {
        // Master's window for a long command was `max_len` columns including
        // the `…`; the cursor cell is counted by the caller, not here, so a
        // long `/task add …` shows exactly as many columns as it always did.
        let value = format!("/task add {}", "x".repeat(400));
        let window = value_window(&Cursor::new(&value), 20);
        assert_eq!(text_display_width(&drawn(&window)), 20);
        assert!(window.clipped_left);
        assert!(!window.clipped_right, "nothing follows the cursor");
        assert!(window.after.is_empty());
        assert!(value.ends_with(&window.before));
    }

    #[test]
    fn a_short_value_is_shown_whole() {
        let window = value_window(&Cursor::new("/task add x"), 20);
        assert_eq!(drawn(&window), "/task add x");
        assert!(!window.clipped_left);
        assert!(!window.clipped_right);
    }

    #[test]
    fn the_window_follows_the_cursor_and_reports_both_clips() {
        let value = format!("{}MIDDLE{}", "a".repeat(100), "b".repeat(100));
        let window = value_window(&Cursor::at(&value, 103), 10);
        let drawn = drawn(&window);
        assert_eq!(text_display_width(&drawn), 10);
        assert!(window.clipped_left && window.clipped_right);
        // The cursor sits inside `MIDDLE`, so both halves of it are on screen.
        assert!(window.before.ends_with("MID"), "{:?}", window.before);
        assert!(window.after.starts_with("DLE"), "{:?}", window.after);
    }

    #[test]
    fn a_double_width_grapheme_never_overflows_the_budget() {
        // The give-back for the `…` has to drop a whole cluster, so an odd
        // budget full of CJK lands a column short rather than one over.
        for budget in 4..24 {
            let window = value_window(&Cursor::new(&"名".repeat(50)), budget);
            let width = text_display_width(&drawn(&window));
            assert!(width <= budget, "budget {budget} drew {width} columns");
            assert!(width >= budget - 1, "budget {budget} drew {width} columns");
        }
    }

    #[test]
    fn the_walk_is_linear_in_the_columns_drawn_not_the_value() {
        // The regression guard for the O(n²) trim (gate S1): the work for a
        // 200k-character value must be the work for a 40-column window. A
        // ratio, not a wall-clock bound, so it holds in debug and under load.
        let budget = 40;
        let short = format!("/msg all {}", "x".repeat(1_000));
        let long = format!("/msg all {}", "x".repeat(200_000));
        let bench = |value: &str| {
            let cursor = Cursor::new(value);
            let start = std::time::Instant::now();
            for _ in 0..50 {
                let window = value_window(&cursor, budget);
                assert_eq!(text_display_width(&drawn(&window)), budget);
            }
            start.elapsed()
        };
        // Warm up, then measure: the first allocation in a test process is
        // not representative.
        bench(&short);
        let short = bench(&short).max(std::time::Duration::from_micros(1));
        let long = bench(&long);
        assert!(
            long < short * 50,
            "200× the value took {long:?} against {short:?} for 1k — the \
             window is not anchored on the cursor"
        );
    }
}
