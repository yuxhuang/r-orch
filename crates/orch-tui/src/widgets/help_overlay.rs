//! The help overlay.
//!
//! A centred amber rounded box: title, three column dividers, ten shortcut
//! rows in three 22-column columns, and the dismiss footer.
//!
//! The box is [`FULL_H`] rows tall. Where the overlay area below the header is
//! shorter than that (an 80×24 terminal), it first goes compact (it drops its
//! four spacer rows and draws the title and the dismiss hint into its top and
//! bottom borders), then draws over the whole frame, so the footer lines (the
//! dismiss hint, the R15 Elapsed* disclosure and the Agents lines) stay on
//! screen at the default terminal size.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::Span;

use crate::backend::CopyOutcome;
use crate::hit::{ChromeId, ZoneId, ZoneKind};
use crate::state::{AppState, CopyOrigin};
use crate::theme::Token;
use crate::views::{RenderCtx, put_line};
use ratatui::widgets::{Clear, Widget};

const COL_W: usize = 22;
/// `COL_W*3 + 3 + 3 + 2`.
const INNER_W: usize = COL_W * 3 + 3 + 3 + 2;
const OUTER_W: usize = INNER_W + 2;
const MAX_ROWS: usize = 10;

// Nine entries against `MAX_ROWS = 10`, so the two r-orch additions (`F2` and
// `m`) do not change the overlay's layout. They sit in this column because
// `ACTIONS` is already *at* `MAX_ROWS` and growing it would move the box;
// `m` belongs with the keys rather than with the `/msg` verb, which `COMMANDS`
// already lists.
pub const NAVIGATION: [(&str, &str); 10] = [
    ("↑↓/j/k", "Navigate"),
    ("←→", "Switch tabs"),
    ("Enter", "Detail view"),
    ("PgUp/PgDn", "Page"),
    ("m", "Message agent"),
    ("Ctrl+Y", "Copy all"),
    ("F6", "Copy controls"),
    ("F2", "Mouse on/off"),
    ("Tab", "Next pane"),
    ("i", "Inspector"),
];

pub const ACTIONS: [(&str, &str); 10] = [
    ("N", "New item"),
    ("E", "Edit"),
    ("D", "Delete"),
    ("R", "Run task"),
    ("S", "Show / Stop"),
    ("C", "Cancel"),
    ("A", "Approve"),
    ("X", "Reject"),
    ("U", "Autonomous"),
    ("Z", "Undo delete"),
];

/// `Ctrl+K`/`Ctrl+U` sit beside `/` because they are its neighbour on the
/// lists: `/` stays the command bar and `Ctrl+K` is the keyword filter
/// (R5). Ten entries, the column's `MAX_ROWS`.
pub const COMMANDS: [(&str, &str); 10] = [
    ("/", "Command mode"),
    ("Ctrl+K", "Filter list"),
    ("Ctrl+U", "Clear filter"),
    ("/task add", "Create task"),
    ("/msg", "Message agent"),
    ("/run", "Execute"),
    ("/watch", "Auto-dispatch"),
    ("/config", "Settings"),
    ("/help", "Help"),
    ("/quit", "Exit"),
];

/// The Elapsed* disclosure, exactly as R15 words it, then when the order
/// catches up, exactly as D1 words it. Both sentences are wrapped at word
/// boundaries to fit the box.
pub const ELAPSED_HELP: [&str; 3] = [
    "Elapsed*: start to now; terminal rows end at last update (edits change it).",
    "Includes waiting/retries. Elapsed order updates on refresh or",
    "list-control changes; displayed time is live.",
];

/// The Agents grouping keys (the footer's `b <Mode>` names the mode), what
/// Caps grouping means (RT-D3), and what UPDATED and its `R` / `T` markers
/// mean (Issue 1), in the words adjudication-v2 rules, wrapped at word
/// boundaries to fit the box.
pub const AGENTS_HELP: [&str; 4] = [
    "Agents: b/B change the grouping. Caps: grouped by first capability; use",
    "the filter to find every agent with a capability. UPDATED: which agent's",
    "run or assignment record changed most recently. R: run; T: assigned task",
    "record updated; may be an administrative change, not proof the agent ran.",
];

/// [`ELAPSED_HELP`]'s first sentence, the one the README carries too.
pub const ELAPSED_DISCLOSURE: &str = "Elapsed*: start to now; terminal rows end at last update (edits change it). Includes waiting/retries.";

/// [`ELAPSED_HELP`]'s second sentence: D1's rule for when the Elapsed order
/// moves, against a time that is drawn live.
pub const ELAPSED_ORDER: &str =
    "Elapsed order updates on refresh or list-control changes; displayed time is live.";

/// Rows above the shortcut columns that are only spacing: the one under the
/// top border, under the title, and under the column dividers.
const SPACERS_ABOVE: usize = 3;
/// Every row of the box: top border, spacer, title, spacer, dividers, spacer,
/// the shortcut rows, spacer, dismiss, panes hint, detail Activity hint,
/// lists hint, the [`ELAPSED_HELP`] and [`AGENTS_HELP`] lines, bottom border.
pub const FULL_H: usize =
    SPACERS_ABOVE + 3 + MAX_ROWS + 1 + 4 + ELAPSED_HELP.len() + AGENTS_HELP.len() + 1;
/// [`FULL_H`] without its four spacer rows, and with the title and the
/// dismiss hint drawn into the top and bottom borders instead of rows of
/// their own: 23 rows, so the whole box fits an 80×24 terminal, and fits
/// under the header (`height - 7` rows) from 30 rows.
pub const COMPACT_H: usize = FULL_H - SPACERS_ABOVE - 1 - 2;

/// Where the box goes and whether it keeps its spacers: the overlay area
/// below the header if the box fits there (spacers first), else the whole
/// frame, which a modal overlay may cover.
fn placement(overlay: Rect, frame: Rect) -> (Rect, bool) {
    let fits = |area: Rect, rows: usize| area.height as usize >= rows;
    if fits(overlay, FULL_H) {
        (overlay, true)
    } else if fits(overlay, COMPACT_H) {
        (overlay, false)
    } else if fits(frame, FULL_H) {
        (frame, true)
    } else {
        (frame, false)
    }
}

/// Draws the overlay in `overlay`, the area below the header, or over all of
/// `frame` when the box does not fit there (see [`placement`]).
pub fn render(frame: &mut Frame, overlay: Rect, full: Rect, ctx: &RenderCtx) {
    if ctx.state.copy_help.is_some() {
        render_contextual(frame, overlay, ctx);
        return;
    }
    let (area, spaced) = placement(overlay, full);
    let spacer = u16::from(spaced);
    let box_h = if spaced { FULL_H } else { COMPACT_H };
    Clear.render(area, frame.buffer_mut());
    let pad_x = ((area.width as usize).saturating_sub(OUTER_W)) / 2;
    let margin_top = ((area.height as usize).saturating_sub(box_h)) / 2;
    let x = area.x + pad_x as u16;
    let width = OUTER_W.min(area.width as usize) as u16;
    let inner = Rect::new(x, area.y, width, area.height);
    let mut y = area.y + margin_top as u16;

    // A click on the box itself does nothing; the "outside" half is
    // registered by `views::render` over the **whole frame** before this runs,
    // because outside includes the header and command-bar rows, which are not
    // in this widget's area (design §3.3.5).
    ctx.zone(
        inner,
        area,
        ZoneKind::Chrome(ChromeId::HelpOverlayInside),
        ZoneId::Chrome(ChromeId::HelpOverlayInside),
    );

    let border = ctx.style(Token::Amber);
    let centre = |text: String| -> String {
        let pad = INNER_W.saturating_sub(text.chars().count()) / 2;
        format!("{}{}", " ".repeat(pad), text)
    };

    const TITLE: &str = "KEYBOARD SHORTCUTS";
    const DISMISS: &str = "Press any key to dismiss";
    if spaced {
        put_line(
            frame,
            inner,
            y,
            vec![Span::styled(format!("╭{}╮", "─".repeat(INNER_W)), border)],
        );
        put_line(frame, inner, y + 1, vec![Span::styled("│", border)]);
        put_line(
            frame,
            inner,
            y + 2,
            vec![Span::styled(centre(TITLE.into()), ctx.bold(Token::Amber))],
        );
        y += 4;
    } else {
        put_line(
            frame,
            inner,
            y,
            labelled_rule(('╭', '╮'), TITLE, border, ctx.bold(Token::Amber)),
        );
        y += 1;
    }

    let dividers = ["NAVIGATION", "ACTIONS", "COMMANDS"];
    let mut header = String::new();
    for name in dividers {
        let pad = COL_W.saturating_sub(name.chars().count()) / 2;
        header.push_str(&" ".repeat(pad));
        header.push_str(name);
        header.push_str(&" ".repeat(COL_W - pad - name.chars().count()));
        header.push(' ');
    }
    put_line(
        frame,
        inner,
        y,
        vec![Span::styled(
            header,
            ctx.style(Token::Dim).add_modifier(Modifier::BOLD),
        )],
    );
    y += 1 + spacer;

    for row in 0..MAX_ROWS {
        let mut spans = Vec::new();
        for (column, entries) in [&NAVIGATION[..], &ACTIONS[..], &COMMANDS[..]]
            .into_iter()
            .enumerate()
        {
            if column > 0 {
                spans.push(Span::styled("│", ctx.style(Token::Ghost)));
            }
            match entries.get(row) {
                Some((key, label)) => {
                    spans.push(Span::styled(format!("{key:<10}"), ctx.bold(Token::Amber)));
                    spans.push(Span::styled(
                        format!("{label:<12}"),
                        ctx.style(Token::Silver),
                    ));
                }
                None => spans.push(Span::raw(" ".repeat(COL_W))),
            }
        }
        put_line(frame, inner, y, spans);
        y += 1;
    }

    if spaced {
        put_line(
            frame,
            inner,
            y + 1,
            vec![Span::styled(centre(DISMISS.into()), ctx.style(Token::Dim))],
        );
        y += 2;
    }
    put_line(
        frame,
        inner,
        y,
        vec![Span::styled(
            "Tab panes; ←→ views; Home/End edges. Feeds: End follows; Up pauses.",
            ctx.style(Token::Dim),
        )],
    );
    y += 1;
    put_line(
        frame,
        inner,
        y,
        vec![Span::styled(
            "Goal/agent Activity, newest first: Home follows; Down pauses.",
            ctx.style(Token::Dim),
        )],
    );
    y += 1;
    put_line(
        frame,
        inner,
        y,
        vec![Span::styled(
            "Lists: o sort; O reverse; 0 default order; click a header to sort.",
            ctx.style(Token::Dim),
        )],
    );
    y += 1;
    // R15's disclosure, D1's ordering rule and the Agents rulings,
    // word-wrapped at the box: the sentences are too long for one 74-cell
    // row, and every word of them must stay on screen.
    for line in ELAPSED_HELP.into_iter().chain(AGENTS_HELP) {
        put_line(
            frame,
            inner,
            y,
            vec![Span::styled(line, ctx.style(Token::Dim))],
        );
        y += 1;
    }
    let bottom = if spaced {
        vec![Span::styled(format!("╰{}╯", "─".repeat(INNER_W)), border)]
    } else {
        labelled_rule(('╰', '╯'), DISMISS, border, ctx.style(Token::Dim))
    };
    put_line(frame, inner, y, bottom);
}

/// A border row of the box with `label` centred in it, `─` either side:
/// how the compact box keeps its title and dismiss hint without a row each.
fn labelled_rule(
    (left, right): (char, char),
    label: &str,
    border: Style,
    text: Style,
) -> Vec<Span<'static>> {
    let label = format!(" {label} ");
    let dashes = INNER_W.saturating_sub(label.chars().count());
    let before = dashes / 2;
    vec![
        Span::styled(format!("{left}{}", "─".repeat(before)), border),
        Span::styled(label, text),
        Span::styled(format!("{}{right}", "─".repeat(dashes - before)), border),
    ]
}

// The inverse of format::parse_timestamp_ms's civil-epoch conversion keeps
// this display-only formatter within the slice's no-new-dependencies contract.
fn capture_timestamp(ms: i64) -> String {
    let days = ms.div_euclid(86_400_000) + 719_468;
    let era = days.div_euclid(146_097);
    let doe = days - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let mut year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    year += i64::from(month <= 2);
    let time = ms.rem_euclid(86_400_000);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z",
        time / 3_600_000,
        time / 60_000 % 60,
        time / 1000 % 60,
        time % 1000
    )
}

fn safe_help(text: &str) -> String {
    text.chars()
        .map(|ch| {
            if (ch.is_control() && ch != '\n')
                || matches!(ch, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
            {
                ' '
            } else {
                ch
            }
        })
        .collect()
}

/// The exact visual rows used both by help rendering and its navigation.
pub(crate) fn contextual_rows(state: &AppState, width: usize) -> Vec<String> {
    let Some(context) = &state.copy_help else {
        return Vec::new();
    };
    let mut lines = Vec::new();
    if let Some(feedback) = state.feedback_for(&context.origin) {
        lines.push(match &feedback.outcome {
            CopyOutcome::NativeWritten => "Copied on this host.".into(),
            CopyOutcome::TerminalRequested => {
                "Requested from terminal; delivery is not confirmed.".into()
            }
            CopyOutcome::Failed(error) => format!("Copy failed: {error}"),
        });
        if let Some(guidance) = &feedback.guidance {
            lines.push(guidance.clone());
        }
    } else {
        let slot = match &context.origin {
            CopyOrigin::Viewer { .. } => &state.copy_feedback.viewer,
            CopyOrigin::Editor { .. } => &state.copy_feedback.editor,
            CopyOrigin::Field { .. } => &state.copy_feedback.field,
        };
        if slot
            .latest
            .as_ref()
            .is_some_and(|m| m.origin == context.origin)
        {
            lines.push("Copy pending; acceptance does not confirm a write.".into());
        }
    }
    match &context.origin {
        CopyOrigin::Editor { draft, step_id, .. } => {
            if let Some(wizard) = state.wizard.as_ref().filter(|w| {
                crate::wizard::DraftKey::from_wizard(w) == *draft && w.step_id() == step_id
            }) {
                lines.push(format!("{}: {}", wizard.title, wizard.step().label));
                if let Some(description) = &wizard.step().description {
                    lines.push(description.clone());
                }
                if let Some(error) = wizard.visible_error() {
                    lines.push(format!("Validation: {error}"));
                }
                if wizard.enter_block_hint() {
                    lines.push("Enter adds a newline. Use Tab to confirm the field.".into());
                }
            }
        }
        CopyOrigin::Viewer { source, .. } => {
            if let Some(viewer) = state
                .viewer
                .as_ref()
                .filter(|v| &v.document().source == source)
            {
                let document = viewer.document();
                lines.push(document.title.clone());
                lines.push(format!("Snapshot captured at {} UTC; {:?}; {} original bytes. Esc closes; reopen for latest.", capture_timestamp(document.captured_at_ms), document.representation, document.original_bytes));
                lines.push(format!(
                    "Source: {:?}; completeness: {:?}",
                    document.source, document.completeness
                ));
                if document
                    .body
                    .chars()
                    .any(|c| c.is_control() && c != '\n' && c != '\t')
                {
                    lines.push("Source contains control characters; display is sanitized. Copy preserves source bytes.".into());
                }
            }
        }
        CopyOrigin::Field { source, .. } => {
            lines.push(format!("Focused read-only field: {source:?}. Ctrl+Y copies the complete field using the default route without opening the viewer."));
        }
    }
    lines.extend([
        "Copy all [all]: copies the complete immutable source or current editor field.",
        "Copy selection [selection]: copies the established range; [none] makes no request.",
        "Copy to terminal [selection/all]: requests OSC 52 delivery; it never confirms receipt.",
        "Copy on this host [selection/all]: writes to the clipboard of the machine running Orch, which may differ from the browser or SSH client.",
        "F6 opens copy controls. Left/Right or Tab/Shift-Tab wrap through all four actions. Home selects Copy all; End selects Copy on this host; Enter activates the selected action. Esc/F6 or mouse x closes the rail without copying.",
        "Ctrl+Y copies the complete field/document using the default route. Viewer y copies only the nonempty selection. Ctrl+Alt+C requests the terminal route; Ctrl+Alt+Y is an optional selection-only alias. macOS Option may collapse to Ctrl+Y. F6, End, Enter explicitly requests Native without Alt/Option.",
        "Selection: Left/Right move the viewer caret by grapheme; Shift+Left/Right/Up/Down extends a range; Shift+Home/End extends to the visual row edge. Ctrl+Home/End moves to the document ends; adding Shift extends there. Up/Down, PageUp/PageDown, Home/End scroll; an established range is preserved.",
        "Ctrl+Shift+C is not an application copy binding; terminal/browser copy delivery depends on the client. Use native selection with capture disabled (F2) where supported.",
        "TMUX requires allow-passthrough on and OSC 52 support in the client. STY/screen passthrough is unsupported. With both TMUX and STY both limitations apply. No terminal request confirms delivery and no native write is also made automatically.",
        "Click completed feedback (or pending x) to dismiss its presentation. Pending work remains associated with its original surface and may show its matching completion later.",
        "Tab: next pane; Left/Right: view. A one-pane screen keeps Tab on that pane. i opens the Actions inspector. Return to the list with Tab/Esc to act; M restores a maximized detail to the list.",
    ].into_iter().map(str::to_owned));
    lines
        .into_iter()
        .flat_map(|line| {
            crate::views::detail::wrap_rows(&safe_help(&line), width.max(1))
                .into_iter()
                .map(|(row, _)| row)
        })
        .collect()
}

fn render_contextual(frame: &mut Frame, area: Rect, ctx: &RenderCtx) {
    let Some(context) = &ctx.state.copy_help else {
        return;
    };
    Clear.render(area, frame.buffer_mut());
    ctx.zone(
        area,
        area,
        ZoneKind::Chrome(ChromeId::HelpOverlayInside),
        ZoneId::Chrome(ChromeId::HelpOverlayInside),
    );
    let title = match &context.origin {
        CopyOrigin::Viewer { .. } => "COPY HELP — viewer".to_string(),
        CopyOrigin::Editor { step_id, .. } => format!(
            "COPY HELP — {}",
            ctx.state
                .wizard
                .as_ref()
                .filter(|w| w.step_id() == step_id)
                .map(|w| w.step().label.as_str())
                .unwrap_or(step_id)
        ),
        CopyOrigin::Field { .. } => "COPY HELP — read-only field".into(),
    };
    put_line(
        frame,
        area,
        area.y,
        vec![Span::styled(
            crate::views::fit_cells(&safe_help(&title), area.width as usize),
            ctx.bold(Token::Amber),
        )],
    );
    let body = Rect::new(
        area.x + 1,
        area.y + 1,
        area.width.saturating_sub(2),
        area.height.saturating_sub(2),
    );
    let rows = contextual_rows(ctx.state, body.width as usize);
    let offset = context
        .scroll_row
        .min(rows.len().saturating_sub(body.height as usize));
    for (index, row) in rows
        .iter()
        .skip(offset)
        .take(body.height as usize)
        .enumerate()
    {
        put_line(
            frame,
            body,
            body.y + index as u16,
            vec![Span::styled(row, ctx.style(Token::Silver))],
        );
    }
    put_line(
        frame,
        area,
        area.bottom().saturating_sub(1),
        vec![Span::styled(
            crate::views::fit_cells("Esc/F1 closes; outside click closes", area.width as usize),
            ctx.style(Token::Dim),
        )],
    );
}
