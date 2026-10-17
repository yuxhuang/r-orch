//! Frame composition (docs/parity/tui.md §6).
//!
//! Regions, top to bottom: Header (5 rows), a blank spacer, the help overlay
//! **or** the main content region, a blank spacer, the bottom panel, a flexible
//! spacer, toasts, the undo banner, and the command bar.
//!
//! Every view and widget takes a [`RenderCtx`] — the state plus the backend
//! capability flags plus the frame clock — so rendering never touches the
//! backend.

pub mod activity;
pub mod agents;
pub mod detail;
pub mod goals;
pub mod logs;
pub mod tasks;

use std::cell::RefCell;

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Widget};

use crate::app::App;
use crate::backend::TuiBackend;
use crate::clock::Clock;
use crate::hit::{ChromeId, HitMap, VerbId, ZoneId, ZoneKind};
use crate::layout::{MIN_HEIGHT, MIN_WIDTH, regions};
use crate::state::{AppState, DetailPanel, InputMode, ViewId};
use crate::theme::{HEAVY_RULE, Palette, Token};
use crate::widgets;

/// Which optional actions the backend supports — the Rust form of the TS
/// "was this callback passed?" checks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Caps {
    pub can_create_task: bool,
    pub can_add_agent: bool,
    pub can_create_goal: bool,
    pub can_force_stop_agent: bool,
    pub can_send_message: bool,
}

impl Caps {
    /// The capability relevant to the active view's `+ add …` pseudo-row.
    pub fn can_create_for(&self, view: ViewId) -> bool {
        match view {
            ViewId::Goals => self.can_create_goal,
            ViewId::Tasks => self.can_create_task,
            ViewId::Agents => self.can_add_agent,
            ViewId::Logs => false,
        }
    }
}

pub struct RenderCtx<'a> {
    pub state: &'a AppState,
    pub caps: Caps,
    pub now_ms: i64,
    /// What this frame drew where, filled in as it is drawn (design §3.2).
    ///
    /// A `RefCell` rather than a `&mut HitMap` threaded through every
    /// signature: this ctx is already the one parameter all ~20 `render`
    /// functions take, it is built once per frame and never escapes it, so
    /// there is no aliasing hazard and the borrow is taken and dropped inside
    /// [`RenderCtx::zone`].
    pub hits: RefCell<HitMap>,
}

impl<'a> RenderCtx<'a> {
    /// A ctx that records into a map for `area`.
    pub fn new(state: &'a AppState, caps: Caps, now_ms: i64, area: Rect) -> Self {
        let mut hits = HitMap::new(area);
        hits.metrics.generation = state.frame_generation.saturating_add(1);
        RenderCtx {
            state,
            caps,
            now_ms,
            hits: RefCell::new(hits),
        }
    }

    /// Record a clickable zone. `rect` is the cells the caller painted and
    /// `area` the region it painted into; the zone is clipped to the
    /// intersection and dropped if nothing is left (§3.2.1).
    pub fn zone(&self, rect: Rect, area: Rect, kind: ZoneKind, id: ZoneId) {
        self.hits.borrow_mut().push(rect, area, kind, id);
    }

    /// Publish actual inner-body geometry through the existing frame cell.
    pub fn publish_pane_metrics(&self, metrics: crate::hit::PaneMetrics) {
        self.hits.borrow_mut().metrics.panes.push(metrics);
    }
    /// B1/B2 supply the complete reserved footer row, including inert cells.
    pub fn publish_text_footer(&self, origin: crate::state::CopyOrigin, rect: Rect) {
        if !self.state.copy_origin_is_live(&origin) {
            return;
        }
        let mut hits = self.hits.borrow_mut();
        let rect = rect.intersection(hits.area());
        if rect.width == 0 || rect.height == 0 {
            return;
        }
        let metrics = crate::hit::TextFooterMetrics {
            rect,
            origin: origin.clone(),
        };
        match origin {
            crate::state::CopyOrigin::Viewer { .. } => hits.metrics.viewer_footer = Some(metrics),
            crate::state::CopyOrigin::Editor { .. } => hits.metrics.editor_footer = Some(metrics),
            crate::state::CopyOrigin::Field { .. } => {}
        }
    }
    /// A whole-row zone: `rect` spanning `area`'s width at row `y`.
    pub fn row_zone(&self, area: Rect, y: u16, kind: ZoneKind, id: ZoneId) {
        self.zone(Rect::new(area.x, y, area.width, 1), area, kind, id);
    }
    pub fn palette(&self) -> &Palette {
        &self.state.palette
    }

    pub fn style(&self, token: Token) -> Style {
        Style::default().fg(self.state.palette.get(token))
    }

    pub fn bold(&self, token: Token) -> Style {
        self.style(token).add_modifier(Modifier::BOLD)
    }

    /// Foreground on a saturated fill.
    pub fn chip(&self, fg: Token, bg: Token) -> Style {
        Style::default()
            .fg(self.state.palette.get(fg))
            .bg(self.state.palette.get(bg))
    }
}

/// Draw one frame and return what it made clickable.
///
/// Still takes `&App` and still mutates nothing outside the frame: the map is
/// built here and handed back, rather than written into `App`, so its
/// lifetime is obvious and the read-only rendering contract is unchanged.
pub fn draw<B: TuiBackend, C: Clock>(frame: &mut Frame, app: &App<B, C>) -> HitMap {
    let caps = Caps {
        can_create_task: app.backend.can_create_task(),
        can_add_agent: app.backend.can_add_agent(),
        can_create_goal: app.backend.can_create_goal(),
        can_force_stop_agent: app.backend.can_force_stop_agent(),
        can_send_message: app.backend.can_send_message(),
    };
    let ctx = RenderCtx::new(&app.state, caps, app.now(), frame.area());
    render(frame, frame.area(), &ctx);
    ctx.hits.into_inner()
}

/// Render into an explicit area — the entry point the tests drive.
pub fn render(frame: &mut Frame, area: Rect, ctx: &RenderCtx) {
    if area.width < MIN_WIDTH || area.height < MIN_HEIGHT {
        render_too_small(frame, area, ctx);
        return;
    }

    let state = ctx.state;
    let can_create = ctx.caps.can_create_for(state.view);
    let r = regions(
        area.width,
        area.height,
        state.view,
        state.list_item_count(can_create),
        state.split_offset,
        state.detail_maximized,
    );

    let mut y = area.y;
    // Header — exactly five rows.
    widgets::header::render(frame, Rect::new(area.x, y, area.width, 5), ctx);
    y += 5;
    // Blank spacer.
    y += 1;

    if state.show_help {
        // Everything outside the overlay's box dismisses it — including the
        // header and command-bar rows, which the overlay widget never sees.
        // Registered first so the box's own zone lands on top of it.
        ctx.zone(
            area,
            area,
            ZoneKind::Chrome(ChromeId::HelpOverlayOutside),
            ZoneId::Chrome(ChromeId::HelpOverlayOutside),
        );
        // The overlay replaces the content area and the bottom panel.
        let overlay_h = area.height.saturating_sub(7);
        widgets::help_overlay::render(frame, Rect::new(area.x, y, area.width, overlay_h), ctx);
    } else {
        // The main region's section label is one of the nine fixed rows, so it
        // sits *above* `main_h` rather than inside it (docs/parity/tui.md §6).
        render_main_label(frame, Rect::new(area.x, y, area.width, 1), ctx);
        y += 1;
        if r.main_h > 0 {
            let mut main_h = r.main_h.min(area.bottom().saturating_sub(y));
            // A logs picker takes the lower part of the content area: the logs
            // list shrinks to `max(3, mainH - 16)` (docs/parity/tui.md §6).
            let picker_open = state.show_agent_picker || state.show_type_picker;
            if state.view == ViewId::Logs && picker_open {
                let list_h = main_h.saturating_sub(16).max(3);
                let picker_h = main_h.saturating_sub(list_h);
                main_h = list_h;
                if picker_h > 0 {
                    let picker = Rect::new(area.x, y + list_h, area.width, picker_h);
                    if state.show_agent_picker {
                        widgets::pickers::render_agent_picker(frame, picker, ctx);
                    } else {
                        widgets::pickers::render_type_picker(frame, picker, ctx);
                    }
                }
            }
            let main = Rect::new(area.x, y, area.width, main_h);
            render_main(frame, main, ctx);
            y += r.main_h.min(area.bottom().saturating_sub(y));
        }
        // Blank spacer between the regions.
        if y < area.bottom() {
            y += 1;
        }
        // Toasts and the undo banner are bottom-anchored above the command
        // bar. Ink let them push the layout; with fixed regions they take their
        // rows from the bottom panel, which is the only elastic region.
        let banner_rows = (state.visible_toasts().len() + state.pending_deletions.len()) as u16;
        let available = area.bottom().saturating_sub(y + 1);
        let feed_h = r
            .feed_h
            .min(available)
            .saturating_sub(banner_rows.min(available.saturating_sub(1)));
        if feed_h > 0 {
            render_bottom(frame, Rect::new(area.x, y, area.width, feed_h), ctx);
        }
    }

    // The command bar always owns the last row.
    let mut bottom = area.bottom() - 1;
    widgets::command_bar::render(frame, Rect::new(area.x, bottom, area.width, 1), ctx);

    let undo_rows = (state.pending_deletions.len() as u16).min(bottom.saturating_sub(area.y));
    if undo_rows > 0 {
        bottom -= undo_rows;
        widgets::undo_banner::render(frame, Rect::new(area.x, bottom, area.width, undo_rows), ctx);
    }

    let toast_rows = (state.visible_toasts().len() as u16).min(bottom.saturating_sub(area.y));
    if toast_rows > 0 {
        bottom -= toast_rows;
        widgets::toast::render(
            frame,
            Rect::new(area.x, bottom, area.width, toast_rows),
            ctx,
        );
    }
}

fn render_too_small(frame: &mut Frame, area: Rect, ctx: &RenderCtx) {
    // Deliberate addition, not parity (PARITY.md §12.2).
    let text = format!("terminal too small — need at least {MIN_WIDTH}×{MIN_HEIGHT}");
    let line = Line::from(Span::styled(text, ctx.bold(Token::Amber)));
    let y = area.y + area.height / 2;
    Paragraph::new(line)
        .centered()
        .render(Rect::new(area.x, y, area.width, 1), frame.buffer_mut());
}

fn render_main_label(frame: &mut Frame, area: Rect, ctx: &RenderCtx) {
    section_label(frame, area, ctx, ctx.state.view.label(), None);
}

fn render_main(frame: &mut Frame, area: Rect, ctx: &RenderCtx) {
    match ctx.state.view {
        ViewId::Goals => goals::render(frame, area, ctx),
        ViewId::Tasks => tasks::render(frame, area, ctx),
        ViewId::Agents => agents::render(frame, area, ctx),
        ViewId::Logs => logs::render(frame, area, ctx),
    }
}

fn render_bottom(frame: &mut Frame, area: Rect, ctx: &RenderCtx) {
    let state = ctx.state;
    if state.input_mode == InputMode::Wizard
        && let Some(wizard) = state.wizard.as_ref()
    {
        widgets::wizard::render(frame, area, ctx, wizard);
        return;
    }
    if state.input_mode == InputMode::Command {
        let label_area = Rect::new(area.x, area.y, area.width, 1);
        section_label(frame, label_area, ctx, "COMMANDS", None);
        widgets::command_bar::render_suggestions(
            frame,
            Rect::new(
                area.x,
                area.y + 1,
                area.width,
                area.height.saturating_sub(1),
            ),
            ctx,
        );
        return;
    }
    if state.show_agent_picker {
        widgets::pickers::render_agent_picker(frame, area, ctx);
        return;
    }
    if state.show_type_picker {
        widgets::pickers::render_type_picker(frame, area, ctx);
        return;
    }
    if state.detail != DetailPanel::None {
        detail::render(frame, area, ctx);
        return;
    }
    if state.messages.is_empty()
        && let Some(_config) = crate::onboarding::config_for(state.view)
    {
        widgets::onboarding::render(frame, area, ctx);
        return;
    }
    activity::render(frame, area, ctx);
}

/// `SectionLabel`: `━━━` + ` {LABEL} ` + a trailing rule, with an optional
/// suffix before the rule.
/// Draw a `━━━ LABEL ━━<suffix>━━━…` section rule.
///
/// Returns the column the suffix starts at, so a caller that wants the suffix
/// clickable does not have to re-derive this layout — the drift bug the hit
/// map exists to avoid (design §3.2).
pub fn section_label(
    frame: &mut Frame,
    area: Rect,
    ctx: &RenderCtx,
    label: &str,
    suffix: Option<Vec<Span<'static>>>,
) -> u16 {
    let mut spans = vec![
        Span::styled(HEAVY_RULE.to_string().repeat(3), ctx.style(Token::Ghost)),
        Span::styled(
            format!(" {label} "),
            ctx.chip(Token::Dim, Token::NeutralBg)
                .add_modifier(Modifier::BOLD),
        ),
    ];
    let mut used = 3 + label.chars().count() + 2;
    let mut suffix_x = area.x + used as u16;
    if let Some(suffix) = suffix {
        spans.push(Span::styled(
            HEAVY_RULE.to_string().repeat(2),
            ctx.style(Token::Ghost),
        ));
        used += 2;
        suffix_x = area.x + used as u16;
        for span in suffix {
            used += span.content.chars().count();
            spans.push(span);
        }
    }
    let remaining = (area.width as usize).saturating_sub(used + 1);
    spans.push(Span::styled(
        HEAVY_RULE.to_string().repeat(remaining),
        ctx.style(Token::Ghost),
    ));
    Paragraph::new(Line::from(spans)).render(area, frame.buffer_mut());
    suffix_x
}

/// The detail-panel resize hint.
pub fn resize_hint(maximized: bool) -> &'static str {
    if maximized {
        "+/- exit max"
    } else {
        "+/- resize │ M max"
    }
}

/// Register one zone per verb in a resize hint drawn at `(x, y)`.
///
/// The hint stays a single span — the rendering is untouched — and the verbs
/// are located by name inside it, so `+`, `-` and `M` each get their own cell
/// without a hardcoded offset (design §3.7).
pub fn resize_hint_zones(ctx: &RenderCtx, area: Rect, x: u16, y: u16, hint: &str) {
    for (needle, verb) in [
        ("+", VerbId::DetailGrow),
        ("-", VerbId::DetailShrink),
        ("M", VerbId::DetailMax),
    ] {
        if let Some(byte_offset) = hint.find(needle) {
            let offset = hint[..byte_offset].chars().count() as u16;
            ctx.zone(
                Rect::new(x + offset, y, 1, 1),
                area,
                ZoneKind::Verb(verb),
                ZoneId::Verb(verb),
            );
        }
    }
}

/// Render a single line of spans at `y`, clipped to `area`.
/// The display width of a run of spans, in terminal cells.
///
/// Zone x-ranges are measured the same way ratatui lays the spans out, so a
/// glyph that is two cells wide (or zero) cannot shift a zone off its label.
pub fn spans_width(spans: &[Span<'_>]) -> u16 {
    spans
        .iter()
        .map(|span| unicode_width::UnicodeWidthStr::width(span.content.as_ref()) as u16)
        .sum()
}

pub fn put_line(frame: &mut Frame, area: Rect, y: u16, spans: Vec<Span<'_>>) {
    if y >= area.bottom() {
        return;
    }
    Paragraph::new(Line::from(spans))
        .render(Rect::new(area.x, y, area.width, 1), frame.buffer_mut());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::FakeBackend;
    use crate::clock::TestClock;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    pub fn buffer_text(terminal: &Terminal<TestBackend>) -> String {
        let buffer = terminal.backend().buffer();
        let mut out = String::new();
        for y in 0..buffer.area.height {
            for x in 0..buffer.area.width {
                out.push_str(buffer[(x, y)].symbol());
            }
            out.push('\n');
        }
        out
    }

    #[test]
    fn a_tiny_terminal_renders_the_minimum_size_guard() {
        let app = App::new(FakeBackend::new(), TestClock::new(0));
        let mut terminal = Terminal::new(TestBackend::new(30, 10)).unwrap();
        terminal.draw(|frame| _ = draw(frame, &app)).unwrap();
        let text = buffer_text(&terminal);
        assert!(text.contains("terminal too small"), "{text}");
    }

    #[test]
    fn a_normal_terminal_renders_the_dashboard_chrome() {
        let app = App::new(FakeBackend::new(), TestClock::new(0));
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
        terminal.draw(|frame| _ = draw(frame, &app)).unwrap();
        let text = buffer_text(&terminal);
        assert!(!text.contains("terminal too small"), "{text}");
        assert!(text.contains("ORCH"), "{text}");
        assert!(text.contains("TASKS"), "{text}");
    }
}
