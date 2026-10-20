//! Frame composition.
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
use crate::state::{AppState, DetailPanel, InputMode, SortColumn, ViewId};
use crate::theme::{CROSS, HEAVY_RULE, Palette, Token};
use crate::widgets;

/// Which optional actions the backend supports.
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
    pub fn publish_text_anchor(
        &self,
        source: crate::widgets::text_viewer::ViewerSource,
        rect: Rect,
        byte: usize,
    ) {
        let mut hits = self.hits.borrow_mut();
        let rect = rect.intersection(hits.area());
        if rect.width > 0 && rect.height > 0 {
            if !hits
                .source_versions
                .iter()
                .any(|(saved, _)| saved == &source)
                && let Some(document) = self.state.text_for_source(&source, 0, self.now_ms)
            {
                hits.source_versions
                    .push((source.clone(), AppState::source_fingerprint(&document.body)));
            }
            hits.source_anchors.push((rect, source, byte));
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

    widgets::header::render(frame, Rect::new(area.x, area.y, area.width, 5), ctx);
    let work = area.height.saturating_sub(8);
    let wizard_open = state.input_mode == InputMode::Wizard && state.wizard.is_some();
    let banner_capacity = if wizard_open {
        work.saturating_sub(2)
    } else {
        work
    };
    let undo_rows = state.pending_deletions.len().min(banner_capacity as usize) as u16;
    let toast_rows = state
        .visible_toasts()
        .len()
        .min(banner_capacity.saturating_sub(undo_rows) as usize) as u16;
    let banners = undo_rows + toast_rows;
    let (main_h, gap, panel_h) = if wizard_open {
        let main = r.main_h.min(work.saturating_sub(banners + 2));
        let gap = u16::from(work.saturating_sub(banners + 2) > main);
        (main, gap, work.saturating_sub(banners + main + gap))
    } else if state.view == ViewId::Logs && state.detail == DetailPanel::Log {
        let remaining = work.saturating_sub(banners);
        let main = if state.detail_maximized {
            0
        } else {
            (remaining / 2).min(remaining.saturating_sub(3))
        };
        let gap = u16::from(main > 0 && remaining > main);
        (main, gap, remaining.saturating_sub(main + gap))
    } else {
        let main = r.main_h.min(work.saturating_sub(banners));
        let gap = u16::from(work.saturating_sub(banners) > main);
        (
            main,
            gap,
            r.feed_h.min(work.saturating_sub(banners + main + gap)),
        )
    };
    let label = Rect::new(area.x, area.y + 6, area.width, 1);
    if let Some(wizard) = state.wizard.as_ref().filter(|_| wizard_open) {
        let field = fit_cells(
            &format!(
                "{} {}/{}",
                wizard.step().label,
                wizard.active_index() + 1,
                wizard.active_count()
            ),
            area.width as usize,
        );
        let room = area
            .width
            .saturating_sub(unicode_width::UnicodeWidthStr::width(field.as_str()) as u16 + 3)
            as usize;
        put_line(
            frame,
            label,
            label.y,
            vec![Span::styled(
                if room == 0 {
                    field
                } else {
                    format!("{} · {}", fit_cells(&wizard.title, room), field)
                },
                ctx.bold(Token::Amber),
            )],
        );
    } else {
        render_main_label(frame, label, ctx);
    }
    let mut main = Rect::new(area.x, area.y + 7, area.width, main_h);
    // Pickers occupy their actual lower content rows; no hidden list rows
    // are published as scrollable or clickable underneath them.
    if state.view == ViewId::Logs && (state.show_agent_picker || state.show_type_picker) {
        let list_h = if main.height < 7 {
            0
        } else {
            main.height.saturating_sub(16).max(3).min(main.height)
        };
        let picker = Rect::new(main.x, main.y + list_h, main.width, main.height - list_h);
        main.height = list_h;
        if state.show_agent_picker {
            render_picker(frame, picker, ctx, true);
        } else {
            render_picker(frame, picker, ctx, false);
        }
    }
    if main.height > 0 {
        render_main(frame, main, ctx);
    }
    if panel_h > 0 {
        render_bottom(
            frame,
            Rect::new(area.x, area.y + 7 + main_h + gap, area.width, panel_h),
            ctx,
        );
    }
    let command_y = area.bottom() - 1;
    widgets::command_bar::render(frame, Rect::new(area.x, command_y, area.width, 1), ctx);
    if undo_rows > 0 {
        widgets::undo_banner::render(
            frame,
            Rect::new(area.x, command_y - undo_rows, area.width, undo_rows),
            ctx,
        );
    }
    if toast_rows > 0 {
        widgets::toast::render(
            frame,
            Rect::new(area.x, command_y - banners, area.width, toast_rows),
            ctx,
        );
    }
    if let Some(origin) = state.active_copy_origin().filter(|origin| {
        matches!(origin, crate::state::CopyOrigin::Field { .. })
            && state.copy_footer_visible(origin)
    }) && (state.detail == DetailPanel::None || panel_h < 3)
    {
        put_line(
            frame,
            Rect::new(area.x, command_y, area.width, 1),
            command_y,
            vec![Span::styled(
                fit_cells(&detail::field_feedback(ctx, &origin), area.width as usize),
                ctx.style(Token::Amber),
            )],
        );
    }
    // Feedback replaces both the painted footer and its hit ownership.
    if state.active_copy_origin().is_some_and(|origin| {
        matches!(origin, crate::state::CopyOrigin::Field { .. })
            && state.copy_footer_visible(&origin)
    }) && (state.detail == DetailPanel::None || panel_h < 3)
    {
        let footer = Rect::new(area.x, command_y, area.width, 1);
        ctx.zone(
            footer,
            footer,
            ZoneKind::Pane(state.focus),
            ZoneId::Pane(state.pane_key(state.focus)),
        );
    }
    // These are peer-owned render hooks. I0 intentionally draws nothing;
    // the same calls compose the real B1/B2 surfaces after integration.
    let overlay = Rect::new(area.x, area.y + 6, area.width, area.height - 7);
    crate::wizard::session::render_session_overlay(frame, overlay, ctx);
    widgets::text_viewer::TextViewer::render(frame, overlay, ctx);
    if state.show_help {
        ctx.zone(
            area,
            area,
            ZoneKind::Chrome(ChromeId::HelpOverlayOutside),
            ZoneId::Chrome(ChromeId::HelpOverlayOutside),
        );
        widgets::help_overlay::render(frame, overlay, area, ctx);
    }
    mark_zwj_trailing_cells(frame.buffer_mut());
}

/// xterm's default Unicode provider can store a ZWJ suffix in its own cell.
/// Ratatui stores that cell as a blank continuation and would otherwise omit
/// it when the grapheme is replaced by blank content. Retain a diff marker on
/// the hidden cells so the next frame explicitly repaints them. While the
/// grapheme is present, ratatui skips these cells as usual.
fn mark_zwj_trailing_cells(buffer: &mut ratatui::buffer::Buffer) {
    use ratatui::buffer::{CellDiffOption, CellWidth};

    for row in buffer.content.chunks_mut(usize::from(buffer.area.width)) {
        let mut x = 0;
        while x < row.len() {
            let width = usize::from(row[x].cell_width()).max(1);
            if row[x].symbol().contains('\u{200d}') {
                let end = (x + width).min(row.len());
                for cell in &mut row[x + 1..end] {
                    if cell.diff_option == CellDiffOption::None {
                        cell.set_diff_option(CellDiffOption::AlwaysUpdate);
                    }
                }
            }
            x += width;
        }
    }
}

/// Fit a label by terminal columns, including a visible elision marker.
pub(crate) fn fit_cells(text: &str, width: usize) -> String {
    use unicode_width::UnicodeWidthChar;
    if unicode_width::UnicodeWidthStr::width(text) <= width {
        return text.to_string();
    }
    if width == 0 {
        return String::new();
    }
    let mut used = 0;
    let mut result = String::new();
    for ch in text.chars() {
        let cells = ch.width().unwrap_or(0);
        if used + cells >= width {
            break;
        }
        used += cells;
        result.push(ch);
    }
    result.push('…');
    result
}

fn render_too_small(frame: &mut Frame, area: Rect, ctx: &RenderCtx) {
    let text = format!("terminal too small — need at least {MIN_WIDTH}×{MIN_HEIGHT}");
    let line = Line::from(Span::styled(text, ctx.bold(Token::Amber)));
    let y = area.y + area.height / 2;
    Paragraph::new(line)
        .centered()
        .render(Rect::new(area.x, y, area.width, 1), frame.buffer_mut());
}

fn render_main_label(frame: &mut Frame, area: Rect, ctx: &RenderCtx) {
    let focused = ctx.state.focus == crate::state::Focus::MainList;
    let title = format!(
        "{}{}",
        if focused { "> " } else { "" },
        ctx.state.view.label()
    );
    let chip = keyword_chip(ctx, area.width as usize, title.chars().count());
    let width = chip.as_ref().map_or(0, |chip| spans_width(&chip[..1]));
    let x = section_label(frame, area, ctx, &title, chip);
    // The chip is the mouse path to Ctrl+U: clicking it clears the keyword.
    if width > 0 {
        ctx.zone(
            Rect::new(x, area.y, width, 1),
            area,
            ZoneKind::Verb(VerbId::KeywordClear),
            ZoneId::Verb(VerbId::KeywordClear),
        );
    }
}

/// The widest keyword the chip shows before it is cut with `…`.
const KEYWORD_CHIP_MAX_COLS: usize = 32;

/// The active keyword as a chip on the list's title rule — `filter: ship ·
/// 2/5 ^U ✕` — so a filtered list never looks like the whole list. `None`
/// while the list is unfiltered. The keyword is drawn safely (no control or
/// zero-width characters) and cut to fit what the rule has left.
fn keyword_chip(ctx: &RenderCtx, width: usize, title_cols: usize) -> Option<Vec<Span<'static>>> {
    let view = ctx.state.view;
    let keyword = ctx.state.keyword(view);
    if keyword.is_empty() {
        return None;
    }
    let (matches, total) = ctx.state.keyword_counts(view);
    let tail = format!(" · {matches}/{total} ^U {CROSS} ");
    // `━━━ title ━━` before the chip and a `━` after it.
    let room = width.saturating_sub(3 + title_cols + 2 + 2 + 1);
    let fixed = " filter: ".len() + crate::text::text_display_width(&tail);
    let keyword_cols = room.saturating_sub(fixed).min(KEYWORD_CHIP_MAX_COLS);
    if keyword_cols == 0 {
        return None;
    }
    let shown = crate::format::cap_display(&sanitize(keyword), keyword_cols);
    Some(vec![Span::styled(
        format!(" filter: {shown}{tail}"),
        ctx.chip(Token::Amber, Token::AccentBg)
            .add_modifier(Modifier::BOLD),
    )])
}

/// The line a filtered list draws under its rows when nothing matches, so
/// an empty list reads as "filtered out", with the way back, rather than as
/// "no data". Not a row: it has no selection and no zone.
///
/// Returns whether it drew, so a caller can place what follows below it.
pub(crate) fn render_no_match_at(frame: &mut Frame, area: Rect, y: u16, ctx: &RenderCtx) -> bool {
    let view = ctx.state.view;
    let keyword = ctx.state.keyword(view);
    if keyword.is_empty() || ctx.state.keyword_counts(view).0 > 0 || y >= area.bottom() {
        return false;
    }
    let shown = crate::format::cap_display(&sanitize(keyword), KEYWORD_CHIP_MAX_COLS);
    put_line(
        frame,
        area,
        y,
        vec![Span::styled(
            fit_cells(
                &format!(
                    "    No {} match \u{201c}{shown}\u{201d} · Ctrl+U clears the filter",
                    view.item_label()
                ),
                area.width as usize,
            ),
            ctx.style(Token::Dim),
        )],
    );
    true
}

fn render_main(frame: &mut Frame, area: Rect, ctx: &RenderCtx) {
    use crate::state::{AgentRow, Focus, TaskRow};
    let state = ctx.state;
    let key = state.pane_key(Focus::MainList);
    ctx.zone(
        area,
        area,
        ZoneKind::Pane(Focus::MainList),
        ZoneId::Pane(key.clone()),
    );
    let (items, display_rows, selection_rows) = match state.view {
        ViewId::Tasks => {
            let rows = state.task_rows(ctx.caps.can_create_task);
            let selected = rows
                .iter()
                .enumerate()
                .filter_map(|(row, item)| {
                    matches!(
                        item,
                        TaskRow::Task { .. } | TaskRow::ShowAll { .. } | TaskRow::AddTask { .. }
                    )
                    .then_some(row)
                })
                .collect::<Vec<_>>();
            (selected.len(), rows.len(), selected)
        }
        ViewId::Agents => {
            let rows = state.agent_rows(ctx.caps.can_add_agent);
            let selected = rows
                .iter()
                .enumerate()
                .filter_map(|(row, item)| {
                    matches!(item, AgentRow::Agent { .. } | AgentRow::AddAgent { .. })
                        .then_some(row)
                })
                .collect::<Vec<_>>();
            (selected.len(), rows.len(), selected)
        }
        ViewId::Goals => {
            let count = state.sorted_goals().len() + usize::from(ctx.caps.can_create_goal);
            (count, count, (0..count).collect())
        }
        ViewId::Logs => {
            let count = state.filtered_messages().len();
            (count, count, (0..count).collect())
        }
    };
    let body = if state.view == ViewId::Logs {
        Rect::new(
            area.x,
            area.y + 2,
            area.width,
            area.height.saturating_sub(2),
        )
    } else {
        // Sort bars and column headers are chrome above the scrolled rows.
        let chrome = state.list_chrome_rows();
        Rect::new(
            area.x,
            area.y.saturating_add(chrome),
            area.width,
            area.height.saturating_sub(chrome),
        )
    };
    ctx.publish_pane_metrics(crate::hit::PaneMetrics {
        key,
        body,
        items,
        display_rows,
        selection_rows,
    });
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
        let suggestion_rows: Vec<_> = ctx
            .hits
            .borrow()
            .zones()
            .iter()
            .filter(|z| matches!(z.kind, ZoneKind::WizardSuggestion { .. }))
            .map(|z| z.rect)
            .collect();
        if let (Some(first), Some(last)) = (suggestion_rows.first(), suggestion_rows.last()) {
            let body = Rect::new(
                area.x,
                first.y,
                area.width,
                last.bottom().saturating_sub(first.y),
            );
            ctx.zone(
                body,
                area,
                ZoneKind::Scroll(crate::hit::ScrollRegion::WizardOptions),
                ZoneId::ScrollRegion(crate::hit::ScrollRegion::WizardOptions),
            );
        }
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
        render_picker(frame, area, ctx, true);
        return;
    }
    if state.show_type_picker {
        render_picker(frame, area, ctx, false);
        return;
    }
    if state.detail != DetailPanel::None {
        detail::render(frame, area, ctx);
        return;
    }
    if state.messages.is_empty() {
        let key = state.pane_key(crate::state::Focus::Activity);
        ctx.zone(
            area,
            area,
            ZoneKind::Pane(crate::state::Focus::Activity),
            ZoneId::Pane(key.clone()),
        );
        ctx.publish_pane_metrics(crate::hit::PaneMetrics {
            key,
            body: Rect::new(
                area.x,
                area.y + 1,
                area.width,
                area.height.saturating_sub(1),
            ),
            items: 0,
            display_rows: 0,
            selection_rows: Vec::new(),
        });
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
            ctx.chip(
                if label.starts_with("> ") {
                    Token::Amber
                } else {
                    Token::Dim
                },
                Token::NeutralBg,
            )
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

// ── Table cells and column headers ─────────────────────────────────────────

/// `text` made safe to measure: a grapheme holding a control character (a
/// newline in a task title) becomes a space, and a zero-width one (ZWSP, a
/// bidi mark or override, a soft hyphen, a lone combining mark) is dropped.
///
/// ratatui drops zero-width graphemes when it draws, but
/// [`crate::text::text_display_width`] clamps every grapheme to at least one cell (the
/// editor cursor relies on that), so a zero-width grapheme left in would pad
/// the cell one short and shift every later column left. Dropping them here
/// also keeps a bidi override in a peer's agent name off the terminal.
pub(crate) fn sanitize(text: &str) -> String {
    unicode_segmentation::UnicodeSegmentation::graphemes(text, true)
        .filter_map(|g| {
            if g.contains(char::is_control) {
                Some(" ")
            } else if unicode_width::UnicodeWidthStr::width(g) == 0 {
                None
            } else {
                Some(g)
            }
        })
        .collect()
}

/// `text` fitted to exactly `width` display cells: [`sanitize`]d, an
/// overlong value ends in `…`, a short one is padded. Measured in cells, not
/// `char`s, so a CJK or emoji name cannot push the columns after it out of
/// line.
pub(crate) fn cell(text: &str, width: usize) -> String {
    let capped = crate::format::cap_display(&sanitize(text), width);
    let pad = width.saturating_sub(crate::text::text_display_width(&capped));
    format!("{capped}{}", " ".repeat(pad))
}

/// [`cell`], right-aligned.
pub(crate) fn cell_right(text: &str, width: usize) -> String {
    let capped = crate::format::cap_display(&sanitize(text), width);
    let pad = width.saturating_sub(crate::text::text_display_width(&capped));
    format!("{}{capped}", " ".repeat(pad))
}

/// One column of a list's header row, laid out left to right. A gap is a
/// cell with an empty label and no column.
#[derive(Debug, Clone, Copy)]
pub(crate) struct HeaderCell {
    pub label: &'static str,
    pub width: usize,
    pub right: bool,
    pub column: Option<SortColumn>,
}

impl HeaderCell {
    /// A label over a column that sorts.
    pub(crate) fn sort(label: &'static str, width: usize, column: SortColumn) -> Self {
        HeaderCell {
            label,
            width,
            right: false,
            column: Some(column),
        }
    }

    /// A label over a column that does not sort.
    pub(crate) fn plain(label: &'static str, width: usize) -> Self {
        HeaderCell {
            label,
            width,
            right: false,
            column: None,
        }
    }

    pub(crate) fn gap(width: usize) -> Self {
        HeaderCell::plain("", width)
    }

    /// Right-aligned, over a right-aligned value.
    pub(crate) fn right(mut self) -> Self {
        self.right = true;
        self
    }
}

/// A list's column header row at `y`, from the same widths its rows use, so
/// each label sits over its column. The active sort column reads
/// `LABEL↑` / `LABEL↓` in amber; every sortable label is a
/// [`ZoneKind::SortHeader`] the width of its column. The header is chrome:
/// it is never a selectable row.
pub(crate) fn render_header_row(
    frame: &mut Frame,
    area: Rect,
    y: u16,
    ctx: &RenderCtx,
    cells: &[HeaderCell],
    active: (SortColumn, bool),
) {
    if y >= area.bottom() {
        return;
    }
    let (active_column, descending) = active;
    let mut spans = Vec::new();
    let mut x = area.x as usize;
    for header in cells {
        if header.width == 0 {
            continue;
        }
        let is_active = header.column == Some(active_column);
        let label = if is_active {
            let arrow = if descending { "↓" } else { "↑" };
            let full = format!("{}{arrow}", header.label);
            if crate::text::text_display_width(&full) <= header.width {
                full
            } else {
                // Too narrow for the whole label: cut the label, never the
                // arrow, so the active column is always marked.
                let cut = crate::format::cap_display(header.label, header.width - 1);
                format!("{cut}{arrow}")
            }
        } else {
            header.label.to_string()
        };
        let text = if header.right {
            cell_right(&label, header.width)
        } else {
            cell(&label, header.width)
        };
        let style = if is_active {
            ctx.bold(Token::Amber)
        } else {
            ctx.style(Token::Dim).add_modifier(Modifier::BOLD)
        };
        if let Some(column) = header.column {
            ctx.zone(
                Rect::new(
                    x.min(u16::MAX as usize) as u16,
                    y,
                    header.width.min(u16::MAX as usize) as u16,
                    1,
                ),
                area,
                ZoneKind::SortHeader(column),
                ZoneId::SortHeader(column),
            );
        }
        x += header.width;
        spans.push(Span::styled(text, style));
    }
    put_line(frame, area, y, spans);
}

/// The original picker remains useful at normal sizes. At smaller sizes a
/// bounded window reserves controls before rows so its last option remains
/// reachable without borrowing cells from the command bar.
fn render_picker(frame: &mut Frame, area: Rect, ctx: &RenderCtx, agents: bool) {
    use crate::hit::{PickerKind, RowPart, ScrollRegion};
    use crate::messages::MsgType;
    if area.height >= if agents { 12 } else { 14 } {
        if agents {
            widgets::pickers::render_agent_picker(frame, area, ctx);
        } else {
            widgets::pickers::render_type_picker(frame, area, ctx);
        }
        return;
    }
    if area.height == 0 {
        return;
    }
    let region = if agents {
        ScrollRegion::AgentPicker
    } else {
        ScrollRegion::TypePicker
    };
    ctx.zone(
        area,
        area,
        ZoneKind::Scroll(region),
        ZoneId::ScrollRegion(region),
    );
    put_line(
        frame,
        area,
        area.y,
        vec![Span::styled(
            if agents {
                "Agent Filter"
            } else {
                "Type Filter"
            },
            ctx.bold(Token::Amber),
        )],
    );
    let preset_rows = u16::from(!agents && area.height >= 5);
    if preset_rows > 0 {
        let mut x = area.x;
        for (key, name) in [
            ('1', "all"),
            ('2', "text"),
            ('3', "tools"),
            ('4', "errors"),
            ('5', "events"),
        ] {
            let label = format!("{key}={name} ");
            let width = unicode_width::UnicodeWidthStr::width(label.as_str()) as u16;
            put_line(
                frame,
                Rect::new(x, area.y + 1, width.min(area.right().saturating_sub(x)), 1),
                area.y + 1,
                vec![Span::styled(label, ctx.style(Token::Dim))],
            );
            ctx.zone(
                Rect::new(x, area.y + 1, width, 1),
                area,
                ZoneKind::Verb(VerbId::PickerPreset(key)),
                ZoneId::Verb(VerbId::PickerPreset(key)),
            );
            x = x.saturating_add(width);
        }
    }
    let rows_h = area.height.saturating_sub(2 + preset_rows) as usize;
    let total = if agents {
        ctx.state.agents.len()
    } else {
        MsgType::ALL.len()
    };
    let mut offset = ctx.state.picker_scroll.min(total.saturating_sub(rows_h));
    if ctx.state.picker_cursor < offset {
        offset = ctx.state.picker_cursor;
    } else if rows_h > 0 && ctx.state.picker_cursor >= offset + rows_h {
        offset = ctx.state.picker_cursor + 1 - rows_h;
    }
    for (row, index) in (offset..total).take(rows_h).enumerate() {
        let (label, checked, id) = if agents {
            let agent = &ctx.state.agents[index];
            (
                agent.name.clone(),
                ctx.state.picker_draft.is_empty() || ctx.state.picker_draft.contains(&agent.id),
                ZoneId::PickerItem(PickerKind::Agent, agent.id.clone()),
            )
        } else {
            let kind = MsgType::ALL[index];
            (
                format!("{} {}", kind.picker_icon(), kind.label()),
                ctx.state.type_picker_draft.contains(&kind),
                ZoneId::PickerItem(PickerKind::Type, kind.label().into()),
            )
        };
        let y = area.y + 1 + preset_rows + row as u16;
        let selected = index == ctx.state.picker_cursor;
        let line = format!(
            " {} [{}] {}",
            if selected { ">" } else { " " },
            if checked { "✓" } else { " " },
            label
        );
        put_line(
            frame,
            area,
            y,
            vec![Span::styled(
                fit_cells(&line, area.width as usize),
                ctx.style(if selected {
                    Token::Amber
                } else {
                    Token::Silver
                }),
            )],
        );
        for (part, x, width) in [
            (RowPart::Checkbox, area.x + 3, 3),
            (RowPart::Label, area.x + 6, area.width.saturating_sub(6)),
        ] {
            let kind = if agents {
                ZoneKind::PickerRow { index, part }
            } else {
                ZoneKind::TypePickerRow { index, part }
            };
            ctx.zone(Rect::new(x, y, width, 1), area, kind, id.clone());
        }
    }
    if area.height > 1 {
        let y = area.bottom() - 1;
        let mut x = area.x;
        for (label, verb) in [
            ("Enter confirm", VerbId::PickerConfirm),
            ("Esc cancel", VerbId::PickerCancel),
            ("Space toggle", VerbId::PickerToggle),
        ] {
            let width = unicode_width::UnicodeWidthStr::width(label) as u16;
            if x + width > area.right() {
                continue;
            }
            put_line(
                frame,
                Rect::new(x, y, width, 1),
                y,
                vec![Span::styled(label, ctx.style(Token::Dim))],
            );
            ctx.zone(
                Rect::new(x, y, width, 1),
                area,
                ZoneKind::Verb(verb),
                ZoneId::Verb(verb),
            );
            x += width + 2;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::FakeBackend;
    use crate::clock::TestClock;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    #[test]
    fn closing_a_zwj_viewer_repaints_its_blank_trailing_cells() {
        use crate::widgets::text_viewer::{
            Completeness, Representation, TextViewer, ViewerDocument, ViewerSource,
        };

        let mut app = App::new(FakeBackend::new(), TestClock::new(0));
        let body = "prefix 👩‍💻 suffix\n".repeat(12);
        app.state.viewer = Some(TextViewer::new(ViewerDocument {
            source: ViewerSource::Message(1),
            title: "Unicode".into(),
            body: body.clone().into(),
            representation: Representation::Raw,
            raw_body: None,
            completeness: Completeness::Complete,
            original_bytes: body.len(),
            captured_at_ms: 0,
            initial_anchor: 0,
        }));
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        let before = terminal
            .draw(|frame| _ = draw(frame, &app))
            .unwrap()
            .buffer
            .clone();
        let (x, y) = (8, 14);
        assert_eq!(before[(x, y)].symbol(), "👩‍💻");
        app.state.viewer = None;
        let after = terminal
            .draw(|frame| _ = draw(frame, &app))
            .unwrap()
            .buffer
            .clone();
        assert_eq!(after[(x + 1, y)].symbol(), " ");
        assert!(
            before
                .diff(&after)
                .iter()
                .any(|(col, row, cell)| (*col, *row) == (x + 1, y) && cell.symbol() == " "),
            "clearing the leading cell alone leaves a suffix in terminals that split ZWJ emoji"
        );
    }

    pub(super) fn buffer_text(terminal: &Terminal<TestBackend>) -> String {
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
