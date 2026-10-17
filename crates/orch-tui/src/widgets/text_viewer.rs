//! The immutable full-text viewer overlay (design D3, D10.3–D10.6, D11).
//!
//! A [`ViewerDocument`] is a frozen snapshot: its bytes never change while the
//! viewer is open, whatever happens to the record it came from. The viewer is
//! a *document renderer and source-span selection adapter* over the existing
//! Unicode crates and ratatui, not an editor:
//!
//! * **Wrap index.** Only the start byte of each visual row of the active
//!   representation is kept (`Vec<usize>`, at most one entry per source byte,
//!   capped at [`ROW_INDEX_MAX_BYTES`]). Rows are rebuilt only when the width
//!   or the representation changes; scrolling renders a sliced window.
//! * **Source bytes, not cells.** Selection, the caret and the scroll anchor
//!   are byte offsets on grapheme boundaries. Tabs (4-column stops) and
//!   control placeholders exist only on screen; copy returns the original
//!   bytes, with no soft-wrap newlines, padding or chrome.
//! * **Controls are never rendered.** C0/C1/DEL, bidi overrides and other
//!   zero-width graphemes are drawn as visible placeholders, so untrusted
//!   text cannot drive the terminal.
//!
//! B0 opens the viewer (`AppState::viewer`), supplies the overlay rectangle
//! and owns contextual F1 help; this module owns everything inside the
//! overlay through the single [`handle_ui_event`] hook, plus the one Tick
//! drain of copy completions for every surface.

use std::cell::RefCell;
use std::sync::Arc;

use ratatui::crossterm::event::{
    Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::style::{Modifier, Style};
use ratatui::text::Span;
use ratatui::widgets::{Clear, Widget};
use ratatui::{Frame, layout::Rect};
use unicode_segmentation::{GraphemeCursor, UnicodeSegmentation};
use unicode_width::UnicodeWidthStr;

use crate::backend::{CopyTarget, TuiBackend, copy};
use crate::hit::{TextAction, TextSurfaceId, ZoneId, ZoneKind};
use crate::navigation::{NavAction, ScrollMetrics, ScrollMode, ScrollState, navigate};
use crate::state::{ArmedClick, CopyOrigin, Focus, PaneEntity, PaneKey};
use crate::theme::Token;
use crate::{app::App, clock::Clock, keys::UiEvent, state::EntityType, views::RenderCtx};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ViewerSource {
    Message(u64),
    Entity {
        kind: EntityType,
        id: String,
        field: String,
    },
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Representation {
    Human,
    Raw,
    Field,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Completeness {
    Complete,
    Unavailable { reason: String },
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ViewerDocument {
    pub source: ViewerSource,
    pub title: String,
    pub body: Arc<str>,
    pub representation: Representation,
    pub raw_body: Option<Arc<str>>,
    pub completeness: Completeness,
    pub original_bytes: usize,
    pub captured_at_ms: i64,
    pub initial_anchor: usize,
}

/// Columns between tab stops.
pub const TAB_STOP: usize = 4;
/// Explicit cap on the active representation's row index (design D4).
pub const ROW_INDEX_MAX_BYTES: usize = 8 * 1024 * 1024;
const ROW_INDEX_MAX_ENTRIES: usize = ROW_INDEX_MAX_BYTES / std::mem::size_of::<usize>();
/// Visual rows one accepted wheel notch moves read-only text (design D2).
pub const WHEEL_ROWS: usize = 3;
/// Display cells of the rail's fixed label slot (design D10.4).
pub const RAIL_LABEL_CELLS: usize = 29;
/// The four copy destinations, in rail order.
pub const COPY_ACTIONS: [TextAction; 4] = [
    TextAction::CopyAll,
    TextAction::CopySelection,
    TextAction::CopyTerminal,
    TextAction::CopyNative,
];

const CHIP: &str = "Copy\u{2026} (F6)";
const REOPEN: &str = "Esc closes; reopen for latest";
const CLOSE_CONTROL_CELLS: u16 = 10; // "Esc closes"
const TOGGLE: &str = "r:H/R";
const NOTICE_REPRESENTATION: &str = "Selection cleared: representation changed";
const NOTICE_REPRESENTATION_SHORT: &str = "Selection cleared: view changed; F6";
const NOTICE_UNAVAILABLE: &str = "Body unavailable; nothing to copy";
const NOTICE_NOT_LIVE: &str = "Copy unavailable; reopen viewer";

/// Footer messages that make no transport request and so live in the widget,
/// not in the D11 store.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalNotice {
    /// Copy selection with an empty selection: zero requests.
    NoSelection,
    /// Human/Raw switched: the range and viewport were reset.
    RepresentationChanged,
    /// The snapshot holds no body (over 1 MiB): copy is disabled.
    Unavailable,
    /// The viewer's copy identity is no longer registered.
    NotLive,
}

/// An in-progress mouse drag. The anchor is the pressed grapheme, so the
/// pressed cell is included whichever way the drag goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Drag {
    anchor_start: usize,
    anchor_end: usize,
    body: Rect,
}

/// Visual rows of one representation at one width.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Wrap {
    width: u16,
    /// Start byte of every visual row. `rows[0] == 0`.
    rows: Vec<usize>,
    /// The body ends in a line break, so one more empty row follows the last
    /// stored one. Kept implicit so the index never exceeds one entry per
    /// source byte.
    trailing_empty: bool,
}

impl Wrap {
    fn total(&self) -> usize {
        self.rows.len() + usize::from(self.trailing_empty)
    }
}

/// Render-time geometry. A `RefCell` because rendering takes `&AppState`
/// (the existing RenderCtx convention) yet must record what it laid out.
#[derive(Debug, Clone, Default)]
struct Layout {
    wrap: Option<Wrap>,
    body_rows: usize,
    /// The source byte at the top of the viewport. The source of truth for
    /// scroll position: a reflow keeps the row containing it on top.
    top_byte: usize,
    /// The document's initial anchor, applied once the first width is known.
    pending_anchor: Option<usize>,
}

#[derive(Debug, Clone)]
pub struct TextViewer {
    document: ViewerDocument,
    active: Representation,
    /// Keyboard caret, a source byte on a grapheme boundary.
    caret: usize,
    /// The other end of the selection. A selection exists iff it differs
    /// from the caret.
    anchor: Option<usize>,
    drag: Option<Drag>,
    /// The open copy rail and its selected action (design D3/D10.4).
    rail: Option<TextAction>,
    notice: Option<LocalNotice>,
    layout: RefCell<Layout>,
}

impl TextViewer {
    pub fn new(document: ViewerDocument) -> Self {
        let active = document.representation;
        let anchor = grapheme_floor(&document.body, document.initial_anchor);
        TextViewer {
            document,
            active,
            caret: anchor,
            anchor: None,
            drag: None,
            rail: None,
            notice: None,
            layout: RefCell::new(Layout {
                pending_anchor: Some(anchor),
                top_byte: 0,
                ..Layout::default()
            }),
        }
    }
    pub fn document(&self) -> &ViewerDocument {
        &self.document
    }

    /// The representation currently shown and copied.
    pub fn representation(&self) -> Representation {
        self.active
    }

    /// The exact bytes of the active representation.
    pub fn active_text(&self) -> &Arc<str> {
        match (self.active, &self.document.raw_body) {
            (Representation::Raw, Some(raw))
                if self.document.representation != Representation::Raw =>
            {
                raw
            }
            _ => &self.document.body,
        }
    }

    /// Whether Human/Raw can be switched.
    pub fn can_toggle(&self) -> bool {
        self.document.raw_body.is_some()
            && matches!(
                self.document.representation,
                Representation::Human | Representation::Raw
            )
    }

    pub fn is_complete(&self) -> bool {
        self.document.completeness == Completeness::Complete
    }

    /// The half-open selected byte range of the active text, if non-empty.
    pub fn selection(&self) -> Option<(usize, usize)> {
        let anchor = self.anchor?;
        (anchor != self.caret).then(|| (anchor.min(self.caret), anchor.max(self.caret)))
    }

    /// The selected bytes, exactly as received.
    pub fn selected_text(&self) -> Option<&str> {
        let (start, end) = self.selection()?;
        self.active_text().get(start..end)
    }

    pub fn caret(&self) -> usize {
        self.caret
    }

    pub fn rail(&self) -> Option<TextAction> {
        self.rail
    }

    pub fn notice(&self) -> Option<LocalNotice> {
        self.notice
    }

    pub fn is_dragging(&self) -> bool {
        self.drag.is_some()
    }

    /// The top visual row, as last laid out.
    pub fn top_row(&self) -> usize {
        let layout = self.layout.borrow();
        layout.wrap.as_ref().map_or(0, |wrap| {
            row_of(wrap, self.active_text().len(), layout.top_byte)
        })
    }

    /// Source byte at the top of the viewport.
    pub fn top_byte(&self) -> usize {
        self.layout.borrow().top_byte
    }

    /// Visual rows of the active representation at the last rendered width.
    pub fn total_rows(&self) -> usize {
        self.layout.borrow().wrap.as_ref().map_or(0, Wrap::total)
    }

    /// Allocations this snapshot pins: both representations (counted once
    /// when shared) plus the active row index.
    pub fn pinned_bytes(&self) -> usize {
        let body = self.document.body.len();
        let raw = match &self.document.raw_body {
            Some(raw) if !Arc::ptr_eq(raw, &self.document.body) => raw.len(),
            _ => 0,
        };
        body.saturating_add(raw).saturating_add(self.index_bytes())
    }

    /// Bytes held by the active row index.
    pub fn index_bytes(&self) -> usize {
        self.layout.borrow().wrap.as_ref().map_or(0, |wrap| {
            wrap.rows.capacity() * std::mem::size_of::<usize>()
        })
    }

    /// Lay out for `width`×`rows`, rebuilding the index only on a width or
    /// representation change and keeping the top source byte on top.
    fn ensure_layout(&self, width: u16, rows: usize) {
        let text = Arc::clone(self.active_text());
        let mut layout = self.layout.borrow_mut();
        layout.body_rows = rows;
        if layout.wrap.as_ref().is_none_or(|wrap| wrap.width != width) {
            // Drop the old index before building the new one.
            layout.wrap = None;
            layout.wrap = Some(build_wrap(&text, width));
        }
        let anchor = layout.pending_anchor.take().unwrap_or(layout.top_byte);
        let Some(wrap) = layout.wrap.as_ref() else {
            return;
        };
        let max_top = wrap.total().saturating_sub(rows);
        let row = row_of(wrap, text.len(), anchor).min(max_top);
        layout.top_byte = row_start(wrap, text.len(), row);
    }

    /// Scroll through the shared D2 helper, in visual rows.
    fn scroll(&mut self, action: NavAction, repeat: usize) {
        let text_len = self.active_text().len();
        let keep_range = self.selection().is_some() || self.drag.is_some();
        let layout = self.layout.get_mut();
        let Some(wrap) = layout.wrap.as_ref() else {
            return;
        };
        let mut state = ScrollState {
            selected: None,
            offset: row_of(wrap, text_len, layout.top_byte),
        };
        let metrics = ScrollMetrics {
            items: 0,
            display_rows: wrap.total(),
            viewport_rows: layout.body_rows,
            selection_rows: &[],
            mode: ScrollMode::Text,
        };
        for _ in 0..repeat {
            navigate(&mut state, action, metrics);
        }
        layout.top_byte = row_start(wrap, text_len, state.offset);
        // Plain scrolling with no range moves the caret to the new top; an
        // established range (or one being dragged) is kept as source bytes.
        let top = layout.top_byte;
        if !keep_range {
            self.anchor = None;
            self.caret = top;
        }
    }

    /// Bring the caret's row into view.
    fn reveal_caret(&mut self) {
        let text_len = self.active_text().len();
        let caret = self.caret;
        let layout = self.layout.get_mut();
        let Some(wrap) = layout.wrap.as_ref() else {
            return;
        };
        let rows = layout.body_rows;
        if rows == 0 {
            return;
        }
        let caret_row = row_of(wrap, text_len, caret);
        let top = row_of(wrap, text_len, layout.top_byte);
        let new_top = if caret_row < top {
            caret_row
        } else if caret_row >= top + rows {
            caret_row + 1 - rows
        } else {
            return;
        };
        layout.top_byte = row_start(wrap, text_len, new_top);
    }

    fn switch_representation(&mut self) -> bool {
        if !self.can_toggle() {
            return false;
        }
        self.active = match self.active {
            Representation::Human => Representation::Raw,
            _ => Representation::Human,
        };
        let layout = self.layout.get_mut();
        // Only the active representation has an index: destroy it first.
        layout.wrap = None;
        layout.top_byte = 0;
        layout.pending_anchor = None;
        self.caret = 0;
        self.anchor = None;
        self.drag = None;
        self.notice = Some(LocalNotice::RepresentationChanged);
        true
    }

    fn move_caret(&mut self, to: usize, extend: bool) {
        if extend {
            self.anchor.get_or_insert(self.caret);
        } else {
            self.anchor = None;
        }
        self.caret = to;
        self.reveal_caret();
    }

    /// The caret position one visual row up or down, at the same column.
    fn vertical(&self, down: bool) -> Option<usize> {
        let text = self.active_text();
        let layout = self.layout.borrow();
        let wrap = layout.wrap.as_ref()?;
        let row = row_of(wrap, text.len(), self.caret);
        let col = column_of(text, wrap, row, self.caret);
        let target = if down {
            (row + 1 < wrap.total()).then_some(row + 1)?
        } else {
            row.checked_sub(1)?
        };
        Some(boundary_at_column(text, wrap, target, col))
    }

    fn row_edge(&self, end: bool) -> Option<usize> {
        let text = self.active_text();
        let layout = self.layout.borrow();
        let wrap = layout.wrap.as_ref()?;
        let row = row_of(wrap, text.len(), self.caret);
        Some(if end {
            row_content_end(text, wrap, row)
        } else {
            row_start(wrap, text.len(), row)
        })
    }

    /// The byte range and inclusive-drag semantics of the cell under
    /// `(x, y)`, clamped into `body` (design D3: outside drags clamp).
    fn cell_at(&self, body: Rect, x: u16, y: u16) -> (usize, usize) {
        let text = self.active_text();
        let layout = self.layout.borrow();
        let Some(wrap) = layout.wrap.as_ref() else {
            return (0, 0);
        };
        let top = row_of(wrap, text.len(), layout.top_byte);
        let rows = usize::from(body.height).max(1);
        let rel_y = usize::from(y.saturating_sub(body.y)).min(rows - 1);
        let row = top + rel_y;
        if row >= wrap.total() {
            return (text.len(), text.len());
        }
        let col = if x < body.x {
            0
        } else {
            usize::from(x - body.x)
        };
        if x >= body.right() {
            let end = row_content_end(text, wrap, row);
            return (end, end);
        }
        grapheme_at_column(text, wrap, row, col)
    }

    fn start_drag(&mut self, body: Rect, x: u16, y: u16) {
        let (start, end) = self.cell_at(body, x, y);
        self.drag = Some(Drag {
            anchor_start: start,
            anchor_end: end,
            body,
        });
        self.anchor = Some(start);
        self.caret = start;
        self.notice = None;
    }

    fn extend_drag(&mut self, x: u16, y: u16) {
        let Some(drag) = self.drag else {
            return;
        };
        let (start, end) = self.cell_at(drag.body, x, y);
        if end > drag.anchor_start && start >= drag.anchor_start {
            // Forward (or the pressed cell itself): include the cell.
            self.anchor = Some(drag.anchor_start);
            self.caret = if start == drag.anchor_start && end == drag.anchor_end {
                drag.anchor_start
            } else {
                end
            };
        } else if start < drag.anchor_start {
            // Backward: include the pressed cell and the one under the pointer.
            self.anchor = Some(drag.anchor_end);
            self.caret = start;
        } else {
            self.anchor = Some(drag.anchor_start);
            self.caret = drag.anchor_start;
        }
    }

    fn cancel_drag(&mut self) {
        self.drag = None;
    }

    fn cycle_rail(&mut self, forward: bool) {
        let Some(current) = self.rail else {
            return;
        };
        let index = COPY_ACTIONS.iter().position(|a| *a == current).unwrap_or(0);
        let next = if forward {
            (index + 1) % COPY_ACTIONS.len()
        } else {
            (index + COPY_ACTIONS.len() - 1) % COPY_ACTIONS.len()
        };
        self.rail = Some(COPY_ACTIONS[next]);
    }

    /// The full rail label for `action`, with its visible scope.
    pub fn rail_label(&self, action: TextAction) -> String {
        let has_selection = self.selection().is_some();
        let scope = |selection: &'static str| {
            if has_selection { selection } else { "[all]" }
        };
        match action {
            TextAction::CopyAll => "Copy all [all]".to_string(),
            TextAction::CopySelection => format!(
                "Copy selection {}",
                if has_selection {
                    "[selection]"
                } else {
                    "[none]"
                }
            ),
            TextAction::CopyTerminal => format!("Copy via terminal {}", scope("[selection]")),
            TextAction::CopyNative => format!("Copy on this host {}", scope("[selection]")),
            _ => String::new(),
        }
    }
}

// ── Glyph classification ─────────────────────────────────────────────────────

/// How one grapheme occupies the screen.
enum Glyph<'a> {
    /// A line break (`\n` or `\r\n`): ends the visual row, draws nothing.
    Break,
    Tab,
    Text(&'a str, usize),
    /// Drawn as this safe placeholder instead of its bytes.
    Shown(String),
}

fn is_bidi_control(c: char) -> bool {
    matches!(c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}' | '\u{200e}' | '\u{200f}' | '\u{061c}')
}

fn classify(grapheme: &str) -> Glyph<'_> {
    if grapheme.contains('\n') {
        return Glyph::Break;
    }
    if grapheme == "\t" {
        return Glyph::Tab;
    }
    let Some(first) = grapheme.chars().next() else {
        return Glyph::Shown(String::new());
    };
    let code = first as u32;
    if code < 0x20 {
        return Glyph::Shown(char::from_u32(0x2400 + code).map_or_else(String::new, String::from));
    }
    if code == 0x7f {
        return Glyph::Shown("\u{2421}".into());
    }
    if (0x80..=0x9f).contains(&code) {
        return Glyph::Shown(format!("<{code:02X}>"));
    }
    if let Some(bidi) = grapheme.chars().find(|c| is_bidi_control(*c)) {
        return Glyph::Shown(format!("<{:04X}>", bidi as u32));
    }
    match grapheme.width() {
        0 => Glyph::Shown(format!("<{code:04X}>")),
        width => Glyph::Text(grapheme, width),
    }
}

fn glyph_width(glyph: &Glyph<'_>, col: usize, width: usize) -> usize {
    match glyph {
        Glyph::Break => 0,
        Glyph::Tab => (TAB_STOP - col % TAB_STOP).min(width.max(1)),
        Glyph::Text(_, w) => *w,
        Glyph::Shown(s) => s.width(),
    }
}

/// Whether `text` contains anything drawn as a placeholder.
pub fn contains_controls(text: &str) -> bool {
    text.graphemes(true)
        .any(|g| matches!(classify(g), Glyph::Shown(_)))
}

/// One display-safe line: controls become placeholders, breaks and tabs
/// become spaces. For titles, IDs and error text.
pub fn safe_inline(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for grapheme in text.graphemes(true) {
        match classify(grapheme) {
            Glyph::Break | Glyph::Tab => out.push(' '),
            Glyph::Text(g, _) => out.push_str(g),
            Glyph::Shown(s) => out.push_str(&s),
        }
    }
    out
}

/// Truncate to `cells` display cells, ending with `…` when cut.
fn fit(text: &str, cells: usize) -> String {
    if text.width() <= cells {
        return text.to_string();
    }
    if cells == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut used = 0;
    for grapheme in text.graphemes(true) {
        let w = grapheme.width();
        if used + w + 1 > cells {
            break;
        }
        out.push_str(grapheme);
        used += w;
    }
    out.push('\u{2026}');
    out
}

// ── Wrap index and byte/cell mapping ─────────────────────────────────────────

fn build_wrap(text: &str, width: u16) -> Wrap {
    let cells = usize::from(width).max(1);
    let mut rows = vec![0usize];
    let mut trailing_empty = false;
    let mut col = 0usize;
    for (start, grapheme) in text.grapheme_indices(true) {
        let glyph = classify(grapheme);
        if matches!(glyph, Glyph::Break) {
            let next = start + grapheme.len();
            if next < text.len() {
                if rows.len() >= ROW_INDEX_MAX_ENTRIES {
                    break;
                }
                rows.push(next);
            } else {
                trailing_empty = true;
            }
            col = 0;
            continue;
        }
        let mut w = glyph_width(&glyph, col, cells);
        if col > 0 && col + w > cells {
            if rows.len() >= ROW_INDEX_MAX_ENTRIES {
                break;
            }
            rows.push(start);
            col = 0;
            w = glyph_width(&glyph, 0, cells);
        }
        col += w;
    }
    rows.shrink_to_fit();
    Wrap {
        width,
        rows,
        trailing_empty,
    }
}

fn row_of(wrap: &Wrap, text_len: usize, byte: usize) -> usize {
    if wrap.trailing_empty && byte >= text_len {
        return wrap.rows.len();
    }
    wrap.rows
        .partition_point(|&start| start <= byte)
        .saturating_sub(1)
}

fn row_start(wrap: &Wrap, text_len: usize, row: usize) -> usize {
    wrap.rows.get(row).copied().unwrap_or(text_len)
}

fn row_raw_end(wrap: &Wrap, text_len: usize, row: usize) -> usize {
    wrap.rows.get(row + 1).copied().unwrap_or(text_len)
}

/// Every drawn grapheme of a row: `(start, end, col, width, glyph)`.
fn row_glyphs<'a>(
    text: &'a str,
    wrap: &Wrap,
    row: usize,
) -> impl Iterator<Item = (usize, usize, usize, usize, Glyph<'a>)> + 'a {
    let start = row_start(wrap, text.len(), row);
    let end = row_raw_end(wrap, text.len(), row);
    let cells = usize::from(wrap.width).max(1);
    let mut col = 0usize;
    text[start..end]
        .grapheme_indices(true)
        .filter_map(move |(offset, grapheme)| {
            let glyph = classify(grapheme);
            if matches!(glyph, Glyph::Break) {
                return None;
            }
            let w = glyph_width(&glyph, col, cells);
            let at = col;
            col += w;
            Some((
                start + offset,
                start + offset + grapheme.len(),
                at,
                w,
                glyph,
            ))
        })
}

/// The end of a row's visible content: before its line break, if any.
fn row_content_end(text: &str, wrap: &Wrap, row: usize) -> usize {
    let start = row_start(wrap, text.len(), row);
    row_glyphs(text, wrap, row)
        .last()
        .map_or(start, |(_, end, ..)| end)
}

fn column_of(text: &str, wrap: &Wrap, row: usize, byte: usize) -> usize {
    let mut last_end_col = 0;
    for (start, _, col, w, _) in row_glyphs(text, wrap, row) {
        if start >= byte {
            return col;
        }
        last_end_col = col + w;
    }
    last_end_col
}

/// The grapheme `[start, end)` covering display column `col` of `row`, or
/// the empty range at the row's content end.
fn grapheme_at_column(text: &str, wrap: &Wrap, row: usize, col: usize) -> (usize, usize) {
    for (start, end, at, w, _) in row_glyphs(text, wrap, row) {
        if col < at + w.max(1) {
            return (start, end);
        }
    }
    let end = row_content_end(text, wrap, row);
    (end, end)
}

fn boundary_at_column(text: &str, wrap: &Wrap, row: usize, col: usize) -> usize {
    grapheme_at_column(text, wrap, row, col).0
}

/// The grapheme boundary at or before `byte`.
fn grapheme_floor(text: &str, byte: usize) -> usize {
    let mut at = byte.min(text.len());
    while !text.is_char_boundary(at) {
        at -= 1;
    }
    let mut cursor = GraphemeCursor::new(at, text.len(), true);
    match cursor.is_boundary(text, 0) {
        Ok(true) => at,
        _ => cursor.prev_boundary(text, 0).ok().flatten().unwrap_or(0),
    }
}

fn next_grapheme(text: &str, byte: usize) -> usize {
    text.get(byte..)
        .and_then(|rest| rest.graphemes(true).next())
        .map_or(byte, |g| byte + g.len())
}

fn previous_grapheme(text: &str, byte: usize) -> usize {
    text.get(..byte)
        .and_then(|head| head.grapheme_indices(true).next_back())
        .map_or(0, |(start, _)| start)
}

/// `HH:MM:SSZ` of a millisecond timestamp. UTC, and labelled so: the crate
/// has no time-zone source and must not claim a local time it cannot know.
pub fn capture_clock(ms: i64) -> String {
    let secs = ms.div_euclid(1000).rem_euclid(86_400);
    format!(
        "{:02}:{:02}:{:02}Z",
        secs / 3600,
        (secs / 60) % 60,
        secs % 60
    )
}

fn representation_label(representation: Representation) -> &'static str {
    match representation {
        Representation::Human => "Human",
        Representation::Raw => "Raw",
        Representation::Field => "Field",
    }
}

// ── Rendering ────────────────────────────────────────────────────────────────

/// Inner rectangles of the overlay (design D10.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ViewerRects {
    pub provenance: Rect,
    pub controls: Rect,
    pub body: Rect,
    pub footer: Rect,
}

/// One-cell side margins, provenance row 0, controls row 1, body rows
/// `[2, h-1)`, footer row `h-1`. `None` when the overlay cannot hold a body.
pub fn viewer_rects(area: Rect) -> Option<ViewerRects> {
    if area.height < 4 || area.width < 3 {
        return None;
    }
    let x = area.x + 1;
    let width = area.width - 2;
    Some(ViewerRects {
        provenance: Rect::new(x, area.y, width, 1),
        controls: Rect::new(x, area.y + 1, width, 1),
        body: Rect::new(x, area.y + 2, width, area.height - 3),
        footer: Rect::new(x, area.bottom() - 1, width, 1),
    })
}

fn surface(source: &ViewerSource) -> TextSurfaceId {
    TextSurfaceId::Viewer(source.clone())
}

fn action_id(source: &ViewerSource, action: TextAction) -> ZoneId {
    ZoneId::TextAction(surface(source), action)
}

fn pane_key(view: crate::state::ViewId, source: &ViewerSource) -> PaneKey {
    PaneKey {
        view,
        entity: Some(match source {
            ViewerSource::Message(id) => PaneEntity::Message(*id),
            ViewerSource::Entity { id, .. } => PaneEntity::Entity(id.clone()),
        }),
        section: Focus::TextViewer,
    }
}

/// What the footer row shows, in D10.4 priority order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FooterMode {
    Rail(TextAction),
    Notice(LocalNotice),
    Pending,
    Feedback(&'static str),
    Idle,
}

fn footer_mode(viewer: &TextViewer, state: &crate::state::AppState) -> FooterMode {
    if let Some(action) = viewer.rail {
        return FooterMode::Rail(action);
    }
    if let Some(notice) = viewer.notice {
        return FooterMode::Notice(notice);
    }
    if let Some(origin) = state.live_copy_origin(&surface(&viewer.document.source))
        && state.copy_footer_visible(origin)
    {
        return match state.feedback_for(origin) {
            Some(feedback) => FooterMode::Feedback(copy::footer_summary(feedback)),
            None => FooterMode::Pending,
        };
    }
    FooterMode::Idle
}

fn notice_text(notice: LocalNotice, width: usize) -> &'static str {
    match notice {
        LocalNotice::NoSelection => copy::SUMMARY_NO_SELECTION,
        LocalNotice::RepresentationChanged if NOTICE_REPRESENTATION.width() <= width => {
            NOTICE_REPRESENTATION
        }
        LocalNotice::RepresentationChanged => NOTICE_REPRESENTATION_SHORT,
        LocalNotice::Unavailable => NOTICE_UNAVAILABLE,
        LocalNotice::NotLive => NOTICE_NOT_LIVE,
    }
}

impl TextViewer {
    /// Draw the open viewer, if any, into the B0-supplied overlay.
    pub fn render(frame: &mut Frame, area: Rect, ctx: &RenderCtx) {
        let state = ctx.state;
        let Some(viewer) = state.viewer.as_ref() else {
            return;
        };
        // A leave dialog drawn underneath owns input; do not paint over it.
        if state.wizard_sessions.pending_leave.is_some() {
            return;
        }
        Clear.render(area, frame.buffer_mut());
        let source = &viewer.document.source;
        ctx.zone(
            area,
            area,
            ZoneKind::Pane(Focus::TextViewer),
            ZoneId::Pane(pane_key(state.view, source)),
        );
        let Some(rects) = viewer_rects(area) else {
            return;
        };
        viewer.ensure_layout(rects.body.width, usize::from(rects.body.height));
        viewer.render_provenance(frame, rects.provenance, ctx);
        viewer.render_controls(frame, rects.controls, ctx);
        viewer.render_body(frame, rects.body, ctx);
        viewer.render_footer(frame, rects.footer, ctx);
        let total = viewer.total_rows();
        ctx.publish_pane_metrics(crate::hit::PaneMetrics {
            key: pane_key(state.view, source),
            body: rects.body,
            items: 0,
            display_rows: total,
            selection_rows: Vec::new(),
        });
        if let Some(origin) = state.live_copy_origin(&surface(source)) {
            ctx.publish_text_footer(origin.clone(), rects.footer);
        }
    }

    fn render_provenance(&self, frame: &mut Frame, row: Rect, ctx: &RenderCtx) {
        let width = usize::from(row.width);
        let label = representation_label(self.active);
        let mut text = if self.is_complete() {
            format!(
                "Snapshot {} {label} {}B",
                capture_clock(self.document.captured_at_ms),
                self.active_text().len()
            )
        } else {
            format!(
                "Snapshot {} {label} unavailable",
                capture_clock(self.document.captured_at_ms)
            )
        };
        let mut extras = Vec::new();
        if contains_controls(self.active_text()) {
            extras.push("controls shown".to_string());
        }
        extras.push(safe_inline(&self.document.title));
        for extra in extras {
            let candidate = format!("{text} \u{b7} {extra}");
            if candidate.width() <= width {
                text = candidate;
            } else {
                let room = width.saturating_sub(text.width() + 3);
                if room >= 4 {
                    text = format!("{text} \u{b7} {}", fit(&extra, room));
                }
                break;
            }
        }
        crate::views::put_line(
            frame,
            row,
            row.y,
            vec![Span::styled(fit(&text, width), ctx.bold(Token::Amber))],
        );
    }

    fn render_controls(&self, frame: &mut Frame, row: Rect, ctx: &RenderCtx) {
        let width = usize::from(row.width);
        let source = &self.document.source;
        let mut spans = vec![Span::styled(fit(REOPEN, width), ctx.style(Token::Silver))];
        ctx.zone(
            Rect::new(row.x, row.y, CLOSE_CONTROL_CELLS, 1),
            row,
            ZoneKind::TextAction(TextAction::CloseViewer),
            action_id(source, TextAction::CloseViewer),
        );
        let toggle_x = REOPEN.width() + 2;
        if self.can_toggle() && toggle_x + TOGGLE.width() <= width {
            spans.push(Span::raw("  "));
            let marked = Style::default().add_modifier(Modifier::BOLD | Modifier::UNDERLINED);
            let plain = ctx.style(Token::Silver);
            let (h, r) = if self.active == Representation::Human {
                (marked, plain)
            } else {
                (plain, marked)
            };
            spans.push(Span::styled("r:", plain));
            spans.push(Span::styled("H", h));
            spans.push(Span::styled("/", plain));
            spans.push(Span::styled("R", r));
            ctx.zone(
                Rect::new(row.x + toggle_x as u16, row.y, TOGGLE.width() as u16, 1),
                row,
                ZoneKind::TextAction(TextAction::ToggleRepresentation),
                action_id(source, TextAction::ToggleRepresentation),
            );
        }
        crate::views::put_line(frame, row, row.y, spans);
    }

    fn render_body(&self, frame: &mut Frame, body: Rect, ctx: &RenderCtx) {
        let source = &self.document.source;
        ctx.zone(
            body,
            body,
            ZoneKind::ViewerText,
            ZoneId::ViewerText(source.clone()),
        );
        if !self.is_complete() {
            let reason = match &self.document.completeness {
                Completeness::Unavailable { reason } => safe_inline(reason),
                Completeness::Complete => String::new(),
            };
            let lines = [
                reason,
                format!(
                    "{} bytes received; copy is disabled.",
                    self.document.original_bytes
                ),
                "Nothing here is a partial body.".to_string(),
            ];
            for (i, line) in lines.iter().enumerate().take(usize::from(body.height)) {
                crate::views::put_line(
                    frame,
                    body,
                    body.y + i as u16,
                    vec![Span::styled(
                        fit(line, usize::from(body.width)),
                        ctx.style(Token::Red),
                    )],
                );
            }
            return;
        }
        let text = self.active_text();
        let layout = self.layout.borrow();
        let Some(wrap) = layout.wrap.as_ref() else {
            return;
        };
        let top = row_of(wrap, text.len(), layout.top_byte);
        let selection = self.selection();
        let normal = ctx.style(Token::White);
        let shown = ctx.style(Token::Amber).add_modifier(Modifier::DIM);
        let selected = Style::default().add_modifier(Modifier::REVERSED);
        let caret_style = Style::default().add_modifier(Modifier::UNDERLINED | Modifier::BOLD);
        let in_selection = |start: usize, end: usize| {
            selection.is_some_and(|(s, e)| start >= s && end <= e && start < end)
        };
        for screen_row in 0..usize::from(body.height) {
            let row = top + screen_row;
            if row >= wrap.total() {
                break;
            }
            let mut spans: Vec<Span<'_>> = Vec::new();
            let mut drawn = 0usize;
            let mut caret_drawn = false;
            for (start, end, _, w, glyph) in row_glyphs(text, wrap, row) {
                if drawn + w > usize::from(body.width) {
                    break;
                }
                let (content, base) = match glyph {
                    Glyph::Tab => (" ".repeat(w), normal),
                    Glyph::Text(g, _) => (g.to_string(), normal),
                    Glyph::Shown(s) => (s, shown),
                    Glyph::Break => continue,
                };
                let style = if in_selection(start, end) {
                    base.patch(selected)
                } else if selection.is_none() && start == self.caret {
                    caret_drawn = true;
                    base.patch(caret_style)
                } else {
                    base
                };
                drawn += w;
                spans.push(Span::styled(content, style));
            }
            let row_end = row_content_end(text, wrap, row);
            let next_start = row_start(wrap, text.len(), row + 1);
            let caret_here = selection.is_none()
                && !caret_drawn
                && self.caret == row_end
                && (row + 1 >= wrap.total() || next_start != row_end || row_end == text.len());
            if caret_here && drawn < usize::from(body.width) {
                spans.push(Span::styled(" ", caret_style));
            }
            crate::views::put_line(frame, body, body.y + screen_row as u16, spans);
        }
    }

    fn render_footer(&self, frame: &mut Frame, row: Rect, ctx: &RenderCtx) {
        let state = ctx.state;
        let width = usize::from(row.width);
        let source = &self.document.source;
        let zone = |x: usize, cells: usize, action: TextAction| {
            ctx.zone(
                Rect::new(row.x + x as u16, row.y, cells as u16, 1),
                row,
                ZoneKind::TextAction(action),
                action_id(source, action),
            );
        };
        let dim = ctx.style(Token::Silver);
        let strong = ctx.bold(Token::Amber);
        match footer_mode(self, state) {
            FooterMode::Rail(action) => {
                let label = self.rail_label(action);
                let padded = format!(
                    "{label}{}",
                    " ".repeat(RAIL_LABEL_CELLS.saturating_sub(label.width()))
                );
                let spans = vec![
                    Span::raw(" "),
                    Span::styled("<", strong),
                    Span::raw(" "),
                    Span::styled(padded, strong.add_modifier(Modifier::REVERSED)),
                    Span::raw(" "),
                    Span::styled(">", strong),
                    Span::raw(" "),
                    Span::styled("x", strong),
                    Span::raw(" "),
                ];
                zone(1, 1, TextAction::PreviousCopyAction);
                zone(3, label.width().min(RAIL_LABEL_CELLS), action);
                zone(33, 1, TextAction::NextCopyAction);
                zone(35, 1, TextAction::CloseCopyRail);
                crate::views::put_line(frame, row, row.y, spans);
            }
            FooterMode::Notice(notice) => {
                let text = fit(notice_text(notice, width), width);
                zone(0, text.width(), TextAction::DismissCopyFeedback);
                crate::views::put_line(frame, row, row.y, vec![Span::styled(text, strong)]);
            }
            FooterMode::Pending => {
                let text = fit(copy::SUMMARY_PENDING, width);
                // Only the `x` dismisses; every other pending cell is inert.
                if let Some(x) = copy::SUMMARY_PENDING.rfind('x')
                    && x < width
                {
                    zone(x, 1, TextAction::DismissCopyFeedback);
                }
                crate::views::put_line(frame, row, row.y, vec![Span::styled(text, strong)]);
            }
            FooterMode::Feedback(summary) => {
                let text = fit(summary, width);
                zone(0, text.width(), TextAction::DismissCopyFeedback);
                crate::views::put_line(frame, row, row.y, vec![Span::styled(text, strong)]);
            }
            FooterMode::Idle => {
                let mut spans = vec![Span::styled(fit(CHIP, width), strong)];
                zone(0, CHIP.width().min(width), TextAction::OpenCopyRail);
                let mut used = CHIP.width();
                let scope = match self.selection() {
                    Some((s, e)) => format!("sel {}B", e - s),
                    None => "all".to_string(),
                };
                let total = self.total_rows().max(1);
                let position = format!("{}/{}", (self.top_row() + 1).min(total), total);
                for hint in [scope, position, "F1 help".to_string()] {
                    let cells = hint.width() + 2;
                    if used + cells > width {
                        break;
                    }
                    used += cells;
                    spans.push(Span::raw("  "));
                    spans.push(Span::styled(hint, dim));
                }
                crate::views::put_line(frame, row, row.y, spans);
            }
        }
    }
}

// ── Input ────────────────────────────────────────────────────────────────────

/// The single B1 event hook (design D9). Returns true when the event was
/// consumed. Tick always returns false after draining copy completions.
pub(crate) fn handle_ui_event<B: TuiBackend, C: Clock>(
    app: &mut App<B, C>,
    event: &UiEvent,
) -> bool {
    match event {
        UiEvent::Tick => {
            drain_copy_results(app);
            if !app.state.mouse.enabled
                && let Some(viewer) = app.state.viewer.as_mut()
            {
                viewer.cancel_drag();
            }
            false
        }
        UiEvent::ExitRequested(_) => false,
        UiEvent::Terminal(terminal) => {
            if app.state.viewer.is_none() {
                return false;
            }
            // Higher-priority owners: contextual help (B0) and a leave
            // dialog (B2). Both leave the viewer's state untouched.
            if app.state.show_help || app.state.wizard_sessions.pending_leave.is_some() {
                return false;
            }
            match terminal {
                Event::Key(key) => handle_key(app, *key),
                Event::Mouse(mouse) => handle_mouse(app, *mouse),
                Event::Paste(_) => true,
                Event::Resize(..) => {
                    if let Some(viewer) = app.state.viewer.as_mut() {
                        viewer.cancel_drag();
                    }
                    false
                }
                _ => false,
            }
        }
    }
}

/// The only reader of copy completions, for all three surfaces, whether or
/// not a viewer is open (design D5/D11).
fn drain_copy_results<B: TuiBackend, C: Clock>(app: &mut App<B, C>) {
    let results = app.backend.poll_copy_results();
    if results.is_empty() {
        return;
    }
    let now = app.now();
    for result in results {
        app.state.apply_copy_result(result, now);
    }
}

fn live_origin<B: TuiBackend, C: Clock>(app: &App<B, C>) -> Option<CopyOrigin> {
    let viewer = app.state.viewer.as_ref()?;
    app.state
        .live_copy_origin(&surface(&viewer.document.source))
        .cloned()
}

fn handle_key<B: TuiBackend, C: Clock>(app: &mut App<B, C>, key: KeyEvent) -> bool {
    // F1 belongs to B0's contextual help, rail open or not (design D10.5).
    if key.code == KeyCode::F(1) {
        return false;
    }
    // Every consumed key can change what is drawn where. A held body press
    // keeps its identity, so a drag can page by keyboard and then extend onto
    // the newly drawn text (design V4); the invalid hit map still refuses any
    // Drag until the next frame is adopted, and closing clears the press.
    let press = app.state.mouse.armed.take().filter(|armed| {
        armed.kind == ZoneKind::ViewerText
            && app.state.viewer.as_ref().is_some_and(|v| v.drag.is_some())
    });
    app.invalidate_hits();
    app.state.mouse.armed = press;
    let Some(viewer) = app.state.viewer.as_mut() else {
        return false;
    };
    if viewer.rail.is_some() {
        return handle_rail_key(app, key);
    }
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt =
        key.modifiers.contains(KeyModifiers::ALT) || key.modifiers.contains(KeyModifiers::META);
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);
    viewer.notice = None;
    match key.code {
        KeyCode::Esc => close_viewer(app),
        KeyCode::Char('q' | 'Q') if !ctrl && !alt => close_viewer(app),
        KeyCode::F(6) => {
            viewer.rail = Some(TextAction::CopyAll);
        }
        KeyCode::Char('y' | 'Y') if ctrl && alt => {
            request_copy(app, TextAction::CopySelection, false)
        }
        KeyCode::Char('c' | 'C') if ctrl && alt => {
            request_copy(app, TextAction::CopyTerminal, false)
        }
        KeyCode::Char('y' | 'Y') if ctrl => request_copy(app, TextAction::CopyAll, false),
        KeyCode::Char('y') if !alt => request_copy(app, TextAction::CopySelection, false),
        KeyCode::Char('r') if !ctrl && !alt => {
            viewer.switch_representation();
        }
        KeyCode::Left if shift => {
            let to = previous_grapheme(viewer.active_text(), viewer.caret);
            viewer.move_caret(to, true);
        }
        KeyCode::Right if shift => {
            let to = next_grapheme(viewer.active_text(), viewer.caret);
            viewer.move_caret(to, true);
        }
        KeyCode::Left => {
            let to = previous_grapheme(viewer.active_text(), viewer.caret);
            viewer.move_caret(to, false);
        }
        KeyCode::Right => {
            let to = next_grapheme(viewer.active_text(), viewer.caret);
            viewer.move_caret(to, false);
        }
        KeyCode::Up | KeyCode::Down if shift => {
            if let Some(to) = viewer.vertical(key.code == KeyCode::Down) {
                viewer.move_caret(to, true);
            }
        }
        KeyCode::Home | KeyCode::End if ctrl => {
            let to = if key.code == KeyCode::End {
                viewer.active_text().len()
            } else {
                0
            };
            viewer.move_caret(to, shift);
        }
        KeyCode::Home | KeyCode::End if shift => {
            if let Some(to) = viewer.row_edge(key.code == KeyCode::End) {
                viewer.move_caret(to, true);
            }
        }
        KeyCode::Up => viewer.scroll(NavAction::Up, 1),
        KeyCode::Down => viewer.scroll(NavAction::Down, 1),
        KeyCode::PageUp => viewer.scroll(NavAction::PageUp, 1),
        KeyCode::PageDown => viewer.scroll(NavAction::PageDown, 1),
        KeyCode::Home => viewer.scroll(NavAction::Home, 1),
        KeyCode::End => viewer.scroll(NavAction::End, 1),
        // Background actions are swallowed while the viewer is open.
        _ => {}
    }
    true
}

fn handle_rail_key<B: TuiBackend, C: Clock>(app: &mut App<B, C>, key: KeyEvent) -> bool {
    let Some(viewer) = app.state.viewer.as_mut() else {
        return false;
    };
    match key.code {
        KeyCode::Esc | KeyCode::F(6) => collapse_rail(app),
        KeyCode::Left | KeyCode::BackTab => viewer.cycle_rail(false),
        KeyCode::Right | KeyCode::Tab => viewer.cycle_rail(true),
        KeyCode::Home => viewer.rail = Some(TextAction::CopyAll),
        KeyCode::End => viewer.rail = Some(TextAction::CopyNative),
        KeyCode::Enter => {
            if let Some(action) = viewer.rail {
                request_copy(app, action, false);
            }
        }
        // Reading keys keep working; nothing else leaks past the rail.
        KeyCode::Up => viewer.scroll(NavAction::Up, 1),
        KeyCode::Down => viewer.scroll(NavAction::Down, 1),
        KeyCode::PageUp => viewer.scroll(NavAction::PageUp, 1),
        KeyCode::PageDown => viewer.scroll(NavAction::PageDown, 1),
        _ => {}
    }
    true
}

/// Esc/F6/x: close the rail without copying, and hide this origin's footer
/// result while keeping its pending association (D11 dismissal).
fn collapse_rail<B: TuiBackend, C: Clock>(app: &mut App<B, C>) {
    if let Some(viewer) = app.state.viewer.as_mut() {
        viewer.rail = None;
        viewer.notice = None;
    }
    if let Some(origin) = live_origin(app) {
        app.state.dismiss_copy_feedback(&origin);
    }
}

fn dismiss_feedback<B: TuiBackend, C: Clock>(app: &mut App<B, C>) {
    if let Some(viewer) = app.state.viewer.as_mut() {
        viewer.notice = None;
    }
    if let Some(origin) = live_origin(app) {
        app.state.dismiss_copy_feedback(&origin);
    }
}

/// Close the viewer: retire its identity (feedback, guard, help) and leave
/// the originating pane exactly as it was. Pending tickets keep draining.
pub(crate) fn close_viewer<B: TuiBackend, C: Clock>(app: &mut App<B, C>) {
    if let Some(origin) = live_origin(app) {
        app.state.retire_copy_origin(&origin);
    }
    app.state.viewer = None;
    app.invalidate_hits();
}

/// Run one copy action through the facade and register its acceptance with
/// the exact origin (design D3/D5/D11). `via_mouse` arms the D10.6 footer
/// guard *before* the footer changes.
fn request_copy<B: TuiBackend, C: Clock>(app: &mut App<B, C>, action: TextAction, via_mouse: bool) {
    let now = app.now();
    let origin = live_origin(app);
    if via_mouse && let Some(origin) = origin.as_ref() {
        app.state.suppress_text_footer(origin, now);
    }
    let Some(viewer) = app.state.viewer.as_mut() else {
        return;
    };
    viewer.rail = None;
    viewer.notice = None;
    let Some(origin) = origin else {
        viewer.notice = Some(LocalNotice::NotLive);
        return;
    };
    if !viewer.is_complete() {
        viewer.notice = Some(LocalNotice::Unavailable);
        return;
    }
    let text = Arc::clone(viewer.active_text());
    let range = match (action, viewer.selection()) {
        (TextAction::CopyAll, _) => 0..text.len(),
        (TextAction::CopySelection, None) => {
            viewer.notice = Some(LocalNotice::NoSelection);
            return;
        }
        (_, Some((start, end))) => start..end,
        (_, None) => 0..text.len(),
    };
    let target = match action {
        TextAction::CopyTerminal => CopyTarget::Terminal,
        TextAction::CopyNative => CopyTarget::Native,
        _ => CopyTarget::Auto,
    };
    let accepted = app.backend.copy_text_via(&text[range], target);
    app.state
        .register_copy_request(origin, target, accepted, now);
}

fn handle_mouse<B: TuiBackend, C: Clock>(app: &mut App<B, C>, mouse: MouseEvent) -> bool {
    let Some(source) = app.state.viewer.as_ref().map(|v| v.document.source.clone()) else {
        return false;
    };
    let now = app.now();
    let zone = app
        .state
        .mouse
        .hits
        .resolve(mouse.column, mouse.row)
        .cloned();
    let body_zone = zone
        .as_ref()
        .filter(|z| z.id == ZoneId::ViewerText(source.clone()))
        .cloned();
    let action_zone = zone.as_ref().and_then(|z| match (&z.kind, &z.id) {
        (ZoneKind::TextAction(action), ZoneId::TextAction(TextSurfaceId::Viewer(s), _))
            if *s == source =>
        {
            Some((*action, z.clone()))
        }
        _ => None,
    });
    match mouse.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            app.state.mouse.armed = None;
            if let Some(viewer) = app.state.viewer.as_mut() {
                viewer.cancel_drag();
            }
            if let Some(body) = body_zone {
                app.state.mouse.armed = Some(ArmedClick {
                    id: body.id.clone(),
                    kind: body.kind.clone(),
                    secondary: false,
                    at_ms: now,
                });
                if let Some(viewer) = app.state.viewer.as_mut()
                    && viewer.is_complete()
                {
                    viewer.start_drag(body.rect, mouse.column, mouse.row);
                }
            } else if let Some((action, zone)) = action_zone {
                if footer_suppressed(app, &zone, now) {
                    app.state.mouse.last_click = None;
                    return true;
                }
                app.state.mouse.armed = Some(ArmedClick {
                    id: zone.id.clone(),
                    kind: ZoneKind::TextAction(action),
                    secondary: false,
                    at_ms: now,
                });
            }
        }
        MouseEventKind::Drag(MouseButton::Left) => {
            if !drag_is_live(app, &source) {
                if let Some(viewer) = app.state.viewer.as_mut() {
                    viewer.cancel_drag();
                }
                return true;
            }
            if let Some(viewer) = app.state.viewer.as_mut() {
                viewer.extend_drag(mouse.column, mouse.row);
            }
        }
        MouseEventKind::Up(MouseButton::Left) => {
            let armed = app.state.mouse.armed.take();
            if app.state.viewer.as_ref().is_some_and(|v| v.drag.is_some()) {
                let live = armed
                    .as_ref()
                    .is_some_and(|a| a.id == ZoneId::ViewerText(source.clone()))
                    && drag_is_live_rect(app, &source);
                if let Some(viewer) = app.state.viewer.as_mut() {
                    if live {
                        viewer.extend_drag(mouse.column, mouse.row);
                    }
                    viewer.cancel_drag();
                }
                return true;
            }
            if let (Some(armed), Some((action, zone))) = (armed, action_zone)
                && armed.id == zone.id
            {
                if footer_suppressed(app, &zone, now) {
                    app.state.mouse.last_click = None;
                    return true;
                }
                mouse_action(app, action);
            }
        }
        MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
            if body_zone.is_some()
                && app.state.mouse.wheel_steps < crate::mouse::WHEEL_MAX_STEPS_PER_FRAME =>
        {
            app.state.mouse.wheel_steps += 1;
            let action = if mouse.kind == MouseEventKind::ScrollUp {
                NavAction::Up
            } else {
                NavAction::Down
            };
            if let Some(viewer) = app.state.viewer.as_mut() {
                viewer.notice = None;
                viewer.scroll(action, WHEEL_ROWS);
            }
        }
        // Background zones under the overlay never receive a click.
        _ => {}
    }
    true
}

/// A drag extends only while its press identity is armed and the body it
/// started in is still the one on screen (design D9).
fn drag_is_live<B: TuiBackend, C: Clock>(app: &App<B, C>, source: &ViewerSource) -> bool {
    app.state
        .mouse
        .armed
        .as_ref()
        .is_some_and(|a| a.id == ZoneId::ViewerText(source.clone()))
        && drag_is_live_rect(app, source)
}

fn drag_is_live_rect<B: TuiBackend, C: Clock>(app: &App<B, C>, source: &ViewerSource) -> bool {
    let Some(drag) = app.state.viewer.as_ref().and_then(|v| v.drag) else {
        return false;
    };
    app.state.mouse.hits.is_valid()
        && app
            .state
            .mouse
            .hits
            .zones()
            .iter()
            .any(|z| z.id == ZoneId::ViewerText(source.clone()) && z.rect == drag.body)
}

/// Defence in depth for D10.6: B0's prelude already refuses a suppressed
/// footer, but the viewer never acts on one either.
fn footer_suppressed<B: TuiBackend, C: Clock>(
    app: &App<B, C>,
    zone: &crate::hit::Zone,
    now: i64,
) -> bool {
    let Some(origin) = live_origin(app) else {
        return false;
    };
    let in_footer = app
        .state
        .mouse
        .hits
        .text_footer(&origin)
        .is_some_and(|footer| footer.rect.y == zone.rect.y);
    in_footer && app.state.text_footer_suppressed(&origin, now)
}

/// A validated left-button release on one of the viewer's controls.
fn mouse_action<B: TuiBackend, C: Clock>(app: &mut App<B, C>, action: TextAction) {
    let now = app.now();
    let origin = live_origin(app);
    let guard = |app: &mut App<B, C>| {
        if let Some(origin) = origin.as_ref() {
            app.state.suppress_text_footer(origin, now);
        }
    };
    let rail = app.state.viewer.as_ref().and_then(|v| v.rail);
    match action {
        TextAction::OpenCopyRail if rail.is_none() => {
            guard(app);
            if let Some(viewer) = app.state.viewer.as_mut() {
                viewer.notice = None;
                viewer.rail = Some(TextAction::CopyAll);
            }
        }
        TextAction::PreviousCopyAction | TextAction::NextCopyAction if rail.is_some() => {
            // Navigation keeps fixed identities and never starts a guard.
            if let Some(viewer) = app.state.viewer.as_mut() {
                viewer.cycle_rail(action == TextAction::NextCopyAction);
            }
        }
        TextAction::CloseCopyRail if rail.is_some() => {
            guard(app);
            collapse_rail(app);
        }
        TextAction::DismissCopyFeedback if rail.is_none() => {
            guard(app);
            dismiss_feedback(app);
        }
        TextAction::CopyAll
        | TextAction::CopySelection
        | TextAction::CopyTerminal
        | TextAction::CopyNative
            if rail == Some(action) =>
        {
            request_copy(app, action, true);
        }
        TextAction::CloseViewer => {
            close_viewer(app);
            return;
        }
        TextAction::ToggleRepresentation => {
            if let Some(viewer) = app.state.viewer.as_mut() {
                viewer.switch_representation();
            }
        }
        _ => return,
    }
    app.invalidate_hits();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wrap(text: &str, width: u16) -> Wrap {
        build_wrap(text, width)
    }

    #[test]
    fn rows_break_on_newlines_and_keep_the_trailing_empty_row_implicit() {
        let w = wrap("ab\ncd\n", 10);
        assert_eq!(w.rows, vec![0, 3]);
        assert!(w.trailing_empty);
        assert_eq!(w.total(), 3);
        let w = wrap("", 10);
        assert_eq!((w.rows.clone(), w.total()), (vec![0], 1));
    }

    #[test]
    fn soft_wrap_never_splits_a_wide_or_combined_grapheme() {
        // é as e + combining acute, a CJK char (2 cells) and an emoji ZWJ.
        let text = "abe\u{301}中x";
        let w = wrap(text, 4);
        // "abé" takes 3 cells; 中 needs 2 and moves to the next row.
        assert_eq!(w.rows, vec![0, "abe\u{301}".len()]);
        for &start in &w.rows {
            assert!(text.is_char_boundary(start));
            assert_eq!(grapheme_floor(text, start), start);
        }
    }

    #[test]
    fn tabs_expand_to_four_column_stops() {
        let text = "a\tb";
        let w = wrap(text, 20);
        let cols: Vec<_> = row_glyphs(text, &w, 0).map(|g| (g.2, g.3)).collect();
        assert_eq!(cols, vec![(0, 1), (1, 3), (4, 1)]);
    }

    #[test]
    fn controls_and_bidi_overrides_are_placeholders_not_bytes() {
        assert_eq!(
            safe_inline("a\u{1b}]52\u{7}\u{9b}\u{202e}z"),
            "a\u{241b}]52\u{2407}<9B><202E>z"
        );
        assert!(contains_controls("x\u{7f}"));
        assert!(!contains_controls("plain\ttext\n"));
    }

    #[test]
    fn grapheme_floor_snaps_inside_clusters() {
        let text = "a👩\u{200d}💻b";
        let inside = 1 + "👩".len();
        assert_eq!(grapheme_floor(text, inside), 1);
        assert_eq!(grapheme_floor(text, text.len() + 9), text.len());
    }

    #[test]
    fn the_index_is_one_entry_per_row_and_bounded() {
        let text = "\n".repeat(1000);
        let w = wrap(&text, 10);
        assert_eq!(w.rows.len(), 1000);
        assert!(w.trailing_empty);
        assert!(w.rows.len() <= ROW_INDEX_MAX_ENTRIES);
    }

    #[test]
    fn capture_clock_is_labelled_utc() {
        assert_eq!(capture_clock(0), "00:00:00Z");
        assert_eq!(capture_clock(86_399_999), "23:59:59Z");
        assert_eq!(capture_clock(-1), "23:59:59Z");
    }

    #[test]
    fn fit_truncates_by_display_cells() {
        assert_eq!(fit("中文中文", 5), "中文\u{2026}");
        assert_eq!(fit("short", 9), "short");
    }
}
