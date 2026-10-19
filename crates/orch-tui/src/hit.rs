//! The per-frame hit map: what was drawn where, recorded by the code that
//! drew it (design §3.2).
//!
//! Every zone is pushed by the widget that rendered the cells it covers, so
//! the map cannot disagree with the frame. The alternative — recomputing the
//! layout at click time — would need a second copy of the brand-bar
//! arithmetic that positions the tab pills (`widgets/header.rs:45-162`), of
//! the pickers' two draw sites (`views/mod.rs:143` vs `:239`), and of the logs
//! visible-window math.
//!
//! Two separate things are recorded per zone, and keeping them apart is the
//! point:
//!
//! * [`ZoneKind`] is **what to do**, including the row index an action needs.
//! * [`ZoneId`] is **which target this is**, built from entity ids and step
//!   ids rather than positions. A press and a release are the same click only
//!   if their `ZoneId`s match, so a list that reorders between them — a
//!   tick-driven refresh, `app.rs:121` → `app.rs:376` — cancels the click
//!   instead of firing it on whatever moved into that row.

use ratatui::layout::Rect;

use crate::state::{CopyOrigin, EntityType, Focus, PaneKey, ViewId};
use crate::widgets::text_viewer::ViewerSource;
use crate::wizard::engine::WizardKind;
use crate::wizard::{DraftKey, LeaveChoice};

/// Which part of a two-part row was hit. A checkbox toggles; a label moves
/// the cursor and toggles on a double click.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowPart {
    Checkbox,
    Label,
}

/// The pseudo-rows: list entries that are buttons rather than entities.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PseudoRow {
    ShowAll,
    AddTask,
    AddAgent,
    AddGoal,
    RunNudge,
}

/// A labelled verb — a picker or wizard footer hint, a logs status-bar key, a
/// detail resize hint. Named for what the label means, never for its key, so
/// the per-view key overloads cannot leak in (§3.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerbId {
    LogsTail,
    OpenAgentPicker,
    OpenTypePicker,
    CycleLogTypes,
    CycleActivityFilter,
    CycleAgentSort,
    ReverseAgentSort,
    ResetAgentSort,
    PickerToggle,
    PickerAll,
    PickerConfirm,
    PickerCancel,
    PickerPreset(char),
    WizardConfirm,
    WizardToggle,
    WizardBack,
    CommandExec,
    CommandComplete,
    CommandCancel,
    DetailGrow,
    DetailShrink,
    DetailMax,
}

/// A command-bar hint in navigate mode. Every one of these is subject to the
/// post-action row suppression in §3.3.3, because the hint set is
/// status-conditional and reflows (`widgets/command_bar.rs:70-89`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FooterAction {
    CommandMode,
    New,
    Run,
    Cancel,
    Approve,
    Reject,
    Edit,
    StopAgent,
    ToggleAutonomous,
    /// `m` on the Agents tab: compose a message to the selection.
    Message,
    PauseGoal,
    ShowAllTasks,
    Delete,
    Undo,
    CloseDetail,
    Enter,
    Quit,
    Help,
}

/// Frame furniture that answers a click without being a control.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChromeId {
    /// Inside the help overlay's box: swallows the click (§3.3.5).
    HelpOverlayInside,
    /// Outside it: dismisses, and does nothing else.
    HelpOverlayOutside,
}

/// A region the wheel can act on. Registered first within its own area so a
/// precise target still wins a click (§3.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScrollRegion {
    /// The active view's list — tasks, agents, goals or log rows.
    MainList,
    AgentPicker,
    TypePicker,
    CommandSuggestions,
    WizardOptions,
    WizardTextarea,
}

/// Which picker a row belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickerKind {
    Agent,
    Type,
}

/// What a zone does when clicked. Carries the row index an action needs,
/// which is deliberately **not** part of [`ZoneId`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ZoneKind {
    Pane(Focus),
    DetailSection(Focus),
    ActivityRow { message_id: u64 },
    ExpandText,
    ViewerText,
    EditorText,
    TextAction(TextAction),
    LeaveChoice(LeaveChoice),
    Tab(ViewId),
    TaskRow { selection: usize },
    AgentRow { selection: usize },
    GoalRow { selection: usize },
    LogRow { filtered_index: usize },
    LogAgentChip { index: usize },
    PickerRow { index: usize, part: RowPart },
    TypePickerRow { index: usize, part: RowPart },
    SuggestionRow { index: usize },
    WizardOption { index: usize, part: RowPart },
    WizardSuggestion { value: String },
    CommandGhost,
    Pseudo(PseudoRow),
    Verb(VerbId),
    FooterHint(FooterAction),
    UndoRow { index: usize },
    Chrome(ChromeId),
    Scroll(ScrollRegion),
}

/// Which target a zone is, across frames (§3.3). Equality here is what pairs
/// a press with a release and what matches a double click, so every variant
/// is built from something stable — an entity id, a message id, a wizard step
/// id — and never from a row position.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ZoneId {
    Pane(PaneKey),
    DetailSection(PaneKey),
    ActivityMessage(u64),
    ExpandText(ViewerSource),
    ViewerText(ViewerSource),
    EditorText(DraftKey, String),
    TextAction(TextSurfaceId, TextAction),
    LeaveChoice(LeaveChoice),
    Tab(ViewId),
    Entity(EntityType, String),
    LogMessage(u64),
    PickerItem(PickerKind, String),
    WizardOption {
        kind: WizardKind,
        step_id: String,
        value: String,
    },
    WizardSuggestion {
        kind: WizardKind,
        step_id: String,
        value: String,
    },
    Suggestion(String),
    Pseudo(PseudoRow),
    Footer(FooterAction),
    Verb(VerbId),
    /// The `PendingDeletion.key`, not the banner row — the row is what
    /// `undo_deletion_at` needs, the key is what identifies the deletion.
    UndoRow(u64),
    Chrome(ChromeId),
    ScrollRegion(ScrollRegion),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Zone {
    pub rect: Rect,
    pub kind: ZoneKind,
    pub id: ZoneId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextAction {
    CopyAll,
    CopySelection,
    CopyTerminal,
    CopyNative,
    ToggleRepresentation,
    OpenCopyRail,
    PreviousCopyAction,
    NextCopyAction,
    CloseCopyRail,
    DismissCopyFeedback,
    CloseViewer,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TextSurfaceId {
    Viewer(ViewerSource),
    Editor(DraftKey, String),
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextFooterMetrics {
    pub rect: Rect,
    pub origin: CopyOrigin,
}
#[derive(Debug, Clone)]
pub struct PaneMetrics {
    pub key: PaneKey,
    pub body: Rect,
    pub items: usize,
    pub display_rows: usize,
    pub selection_rows: Vec<usize>,
}
#[derive(Debug, Clone, Default)]
pub struct FrameMetrics {
    pub generation: u64,
    pub panes: Vec<PaneMetrics>,
    pub viewer_footer: Option<TextFooterMetrics>,
    pub editor_footer: Option<TextFooterMetrics>,
}

/// Everything clickable in one frame.
#[derive(Debug, Clone, Default)]
pub struct HitMap {
    /// The frame area this map was built for. A coordinate outside it is
    /// dropped rather than guessed at.
    area: Rect,
    /// Draw order, back to front.
    zones: Vec<Zone>,
    valid: bool,
    pub metrics: FrameMetrics,
    /// Source byte anchors for the visible read-only rows, in draw order.
    pub source_anchors: Vec<(Rect, ViewerSource, usize)>,
    pub source_versions: Vec<(ViewerSource, u64)>,
}

impl HitMap {
    /// An empty map for `area`, ready to record a frame.
    pub fn new(area: Rect) -> Self {
        HitMap {
            area,
            zones: Vec::new(),
            valid: true,
            metrics: FrameMetrics::default(),
            source_anchors: Vec::new(),
            source_versions: Vec::new(),
        }
    }

    pub fn source_anchor(&self, x: u16, y: u16) -> Option<(&ViewerSource, usize)> {
        if !self.contains(x, y) {
            return None;
        }
        self.source_anchors
            .iter()
            .rev()
            .find(|(rect, _, _)| rect.contains((x, y).into()))
            .map(|(_, source, byte)| (source, *byte))
    }

    /// Absent or invalidated frame metrics never authorize a footer hit.
    pub fn text_footer(&self, origin: &CopyOrigin) -> Option<&TextFooterMetrics> {
        if !self.valid {
            return None;
        }
        let metrics = match origin {
            CopyOrigin::Viewer { .. } => self.metrics.viewer_footer.as_ref(),
            CopyOrigin::Editor { .. } => self.metrics.editor_footer.as_ref(),
            CopyOrigin::Field { .. } => None,
        }?;
        (&metrics.origin == origin).then_some(metrics)
    }
    pub fn pane_metrics(&self, key: &PaneKey) -> Option<&PaneMetrics> {
        if !self.valid {
            return None;
        }
        self.metrics.panes.iter().find(|pane| &pane.key == key)
    }
    pub fn area(&self) -> Rect {
        self.area
    }

    pub fn is_valid(&self) -> bool {
        self.valid
    }

    pub fn zones(&self) -> &[Zone] {
        &self.zones
    }

    /// Mark the geometry as no longer describing anything the user can see.
    /// Called on a resize, on any layout-changing action, and on every
    /// `handle_key` (§3.2.3).
    pub fn invalidate(&mut self) {
        self.valid = false;
    }

    /// Record a zone, clipped to `area` (§3.2.1).
    ///
    /// Widths here are computed from `area.width` and then rendered through
    /// `put_line`, which hands ratatui a 1-row Rect and lets it clip
    /// (`views/mod.rs:306-312`). A zone built from a span's computed x-range
    /// can therefore extend past what was actually painted — at 80 columns
    /// the footer's conditional hints are clipped — so recording it unclipped
    /// would make invisible cells clickable. A zone that clips away entirely
    /// is not recorded at all.
    pub fn push(&mut self, rect: Rect, area: Rect, kind: ZoneKind, id: ZoneId) {
        let clipped = rect.intersection(area).intersection(self.area);
        if clipped.width == 0 || clipped.height == 0 {
            return;
        }
        self.zones.push(Zone {
            rect: clipped,
            kind,
            id,
        });
    }

    /// The topmost non-`Scroll` zone covering `(x, y)`.
    ///
    /// Zones are pushed back to front, so the last match is the one on top —
    /// no explicit layer numbers to keep in sync with the draw order.
    pub fn resolve(&self, x: u16, y: u16) -> Option<&Zone> {
        self.hit(x, y)
            .find(|zone| !matches!(zone.kind, ZoneKind::Scroll(_)))
    }

    /// The innermost `Scroll` region covering `(x, y)`. Registered first
    /// within each area, so "innermost" is again the last match.
    pub fn resolve_scroll(&self, x: u16, y: u16) -> Option<ScrollRegion> {
        self.hit(x, y).find_map(|zone| match zone.kind {
            ZoneKind::Scroll(region) => Some(region),
            _ => None,
        })
    }

    /// The topmost [`ChromeId`] zone covering `(x, y)`, ignoring everything
    /// drawn on top of it.
    ///
    /// The help overlay registers "outside" over the whole frame and then its
    /// own box on top, but the command bar and toasts are drawn *after* the
    /// overlay, so their zones are above both. While the overlay is up the
    /// only question is inside-or-outside the box, and this answers it.
    pub fn resolve_chrome(&self, x: u16, y: u16) -> Option<ChromeId> {
        self.hit(x, y).find_map(|zone| match zone.kind {
            ZoneKind::Chrome(id) => Some(id),
            _ => None,
        })
    }

    /// Is `(x, y)` a cell this map describes at all?
    pub fn contains(&self, x: u16, y: u16) -> bool {
        self.valid && self.area.contains(ratatui::layout::Position { x, y })
    }

    fn hit(&self, x: u16, y: u16) -> impl Iterator<Item = &Zone> {
        let inside = self.contains(x, y);
        self.zones
            .iter()
            .rev()
            .filter(move |zone| inside && zone.rect.contains(ratatui::layout::Position { x, y }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map() -> HitMap {
        HitMap::new(Rect::new(0, 0, 20, 10))
    }

    fn tab(x: u16, width: u16, view: ViewId) -> (Rect, ZoneKind, ZoneId) {
        (
            Rect::new(x, 0, width, 1),
            ZoneKind::Tab(view),
            ZoneId::Tab(view),
        )
    }

    #[test]
    fn the_last_matching_zone_wins_so_draw_order_is_z_order() {
        let mut hits = map();
        let area = hits.area();
        let (rect, kind, id) = tab(0, 20, ViewId::Goals);
        hits.push(rect, area, kind, id);
        let (rect, kind, id) = tab(5, 5, ViewId::Agents);
        hits.push(rect, area, kind, id);

        assert_eq!(
            hits.resolve(6, 0).map(|z| z.id.clone()),
            Some(ZoneId::Tab(ViewId::Agents)),
            "the zone drawn later is on top"
        );
        assert_eq!(
            hits.resolve(1, 0).map(|z| z.id.clone()),
            Some(ZoneId::Tab(ViewId::Goals))
        );
    }

    #[test]
    fn a_zone_is_clipped_to_the_area_it_was_drawn_into() {
        let mut hits = map();
        // A hint whose computed x-range runs past a narrow panel: ratatui
        // painted only the first 4 cells, so only those are clickable.
        hits.push(
            Rect::new(2, 1, 30, 1),
            Rect::new(0, 1, 6, 1),
            ZoneKind::Verb(VerbId::PickerConfirm),
            ZoneId::Verb(VerbId::PickerConfirm),
        );
        let zone = hits.resolve(5, 1).expect("inside the painted part");
        assert_eq!(zone.rect, Rect::new(2, 1, 4, 1));
        assert!(
            hits.resolve(6, 1).is_none(),
            "a cell that was never painted must not be clickable"
        );
    }

    #[test]
    fn a_zone_that_clips_away_entirely_is_not_recorded() {
        let mut hits = map();
        hits.push(
            Rect::new(10, 1, 8, 1),
            Rect::new(0, 1, 6, 1),
            ZoneKind::Verb(VerbId::PickerCancel),
            ZoneId::Verb(VerbId::PickerCancel),
        );
        assert!(hits.zones().is_empty());
    }

    #[test]
    fn a_zone_outside_the_frame_area_is_clipped_against_it_too() {
        let mut hits = HitMap::new(Rect::new(0, 0, 8, 2));
        let huge = Rect::new(0, 0, 40, 40);
        hits.push(
            Rect::new(4, 1, 20, 1),
            huge,
            ZoneKind::Chrome(ChromeId::HelpOverlayInside),
            ZoneId::Chrome(ChromeId::HelpOverlayInside),
        );
        assert_eq!(hits.zones()[0].rect, Rect::new(4, 1, 4, 1));
    }

    #[test]
    fn scroll_regions_answer_the_wheel_but_never_a_click() {
        let mut hits = map();
        let area = hits.area();
        hits.push(
            Rect::new(0, 2, 20, 5),
            area,
            ZoneKind::Scroll(ScrollRegion::MainList),
            ZoneId::ScrollRegion(ScrollRegion::MainList),
        );
        hits.push(
            Rect::new(0, 3, 20, 1),
            area,
            ZoneKind::TaskRow { selection: 1 },
            ZoneId::Entity(EntityType::Task, "tsk_1".into()),
        );

        assert_eq!(
            hits.resolve(4, 3).map(|z| z.kind.clone()),
            Some(ZoneKind::TaskRow { selection: 1 }),
            "a click takes the row, not the region under it"
        );
        assert_eq!(hits.resolve_scroll(4, 3), Some(ScrollRegion::MainList));
        // A cell inside the region but on no row still scrolls.
        assert!(hits.resolve(4, 5).is_none());
        assert_eq!(hits.resolve_scroll(4, 5), Some(ScrollRegion::MainList));
    }

    #[test]
    fn an_invalid_map_resolves_nothing() {
        let mut hits = map();
        let area = hits.area();
        let (rect, kind, id) = tab(0, 6, ViewId::Tasks);
        hits.push(rect, area, kind, id);
        assert!(hits.resolve(1, 0).is_some());

        hits.invalidate();
        assert!(!hits.is_valid());
        assert!(
            hits.resolve(1, 0).is_none(),
            "geometry that no longer describes the screen must answer nothing"
        );
        assert!(hits.resolve_scroll(1, 0).is_none());
    }

    #[test]
    fn a_coordinate_outside_the_frame_is_dropped() {
        let mut hits = map();
        let area = hits.area();
        let (rect, kind, id) = tab(0, 6, ViewId::Tasks);
        hits.push(rect, area, kind, id);
        assert!(!hits.contains(20, 0), "x == width is past the last column");
        assert!(hits.resolve(20, 0).is_none());
        assert!(hits.resolve(0, 10).is_none());
    }

    #[test]
    fn identity_ignores_the_row_index_the_action_carries() {
        // The same task, drawn at a different row after a scroll: the action
        // payload changes, the identity does not.
        let before = ZoneId::Entity(EntityType::Task, "tsk_1".into());
        let after = ZoneId::Entity(EntityType::Task, "tsk_1".into());
        assert_eq!(before, after);
        assert_ne!(
            ZoneKind::TaskRow { selection: 3 },
            ZoneKind::TaskRow { selection: 4 }
        );
    }
}
