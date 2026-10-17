//! All dashboard state and the arithmetic over it (`src/tui/App.tsx`).
//!
//! Pure and synchronous: no backend calls, no ratatui. [`crate::app::App`] owns
//! an `AppState` plus a backend and a clock; [`crate::keys`] mutates it.

use indexmap::IndexSet;
use orch_domain::{
    Agent, AgentStatus, Goal, NotificationPreferences, OrchestratorState, RemoteRunRef, Task,
    TaskStatus, Team, TeamStatus, default_state, goal_status_order,
};

use crate::backend::{BackendResult, CopyOutcome, CopyResult, CopyTarget, CopyTicket};
use crate::command_bar::CommandHistory;
use crate::hit::{HitMap, ZoneId, ZoneKind};
use crate::messages::{
    MAX_TOASTS, Message, MsgType, PendingMessage, RunMaps, TASK_LIST_LIMIT, UNDO_TIMEOUT_MS,
    message_document, preset_types,
};
use crate::onboarding::OnboardingStep;
use crate::text::TextInput;
use crate::theme::{Palette, Token};
use crate::widgets::text_viewer::{TextViewer, ViewerDocument, ViewerSource};
use crate::wizard::{DraftKey, Wizard, WizardSessionStore};
/// Pane focus is separate from the selected entity. B0 wires the focus ring.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Focus {
    #[default]
    MainList,
    Activity,
    GoalDetail(GoalSection),
    TaskDetail(TaskSection),
    AgentDetail(AgentSection),
    LogDetail(LogSection),
    TextViewer,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GoalSection {
    Overview,
    Description,
    Progress,
    Tasks,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskSection {
    Overview,
    Attachments,
    Description,
    Result,
    Files,
    Activity,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentSection {
    Overview,
    Error,
    Role,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogSection {
    Metadata,
    Body,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaneEntity {
    Entity(String),
    Message(u64),
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneKey {
    pub view: ViewId,
    pub entity: Option<PaneEntity>,
    pub section: Focus,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CopyOrigin {
    Viewer {
        source: ViewerSource,
        generation: u64,
    },
    Editor {
        draft: DraftKey,
        step_id: String,
        generation: u64,
    },
    Field {
        source: ViewerSource,
        generation: u64,
    },
}
impl CopyOrigin {
    fn slot(&self) -> usize {
        match self {
            Self::Viewer { .. } => 0,
            Self::Editor { .. } => 1,
            Self::Field { .. } => 2,
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CopyRequestMeta {
    pub origin: CopyOrigin,
    pub target: CopyTarget,
    pub request_seq: u64,
    pub at_ms: i64,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CopyFeedback {
    pub ticket: Option<CopyTicket>,
    pub request: CopyRequestMeta,
    pub outcome: CopyOutcome,
    pub guidance: Option<String>,
    pub completed_at_ms: i64,
}
#[derive(Debug, Clone, Default)]
pub struct CopyFeedbackSlot {
    pub latest: Option<CopyRequestMeta>,
    pub feedback: Option<CopyFeedback>,
    pub footer_hidden: bool,
}
#[derive(Debug, Clone, Default)]
pub struct CopyFeedbackStore {
    pub viewer: CopyFeedbackSlot,
    pub editor: CopyFeedbackSlot,
    pub field: CopyFeedbackSlot,
    pending: [Option<(CopyTicket, CopyRequestMeta)>; 2],
    request_seq: u64,
    /// Broken facade contracts are observable, never an excuse to grow/drop.
    pub contract_errors: usize,
    pub unknown_results: usize,
}
impl CopyFeedbackStore {
    fn slot(&self, origin: &CopyOrigin) -> &CopyFeedbackSlot {
        match origin.slot() {
            0 => &self.viewer,
            1 => &self.editor,
            _ => &self.field,
        }
    }
    fn slot_mut(&mut self, origin: &CopyOrigin) -> &mut CopyFeedbackSlot {
        match origin.slot() {
            0 => &mut self.viewer,
            1 => &mut self.editor,
            _ => &mut self.field,
        }
    }
    pub fn pending_count(&self) -> usize {
        self.pending.iter().flatten().count()
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CopyHelpContext {
    pub origin: CopyOrigin,
    pub scroll_row: usize,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextFooterGuard {
    pub origin: CopyOrigin,
    pub until_ms: i64,
}

/// Copy feedback is untrusted display data; never retain terminal controls.
/// Byte caps include the visible ellipsis, and end at UTF-8 boundaries.
fn safe_copy_detail(text: &str, limit: usize) -> String {
    let mut result = String::new();
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        let ch =
            if ch.is_control() || matches!(ch, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}') {
                ' '
            } else {
                ch
            };
        if result.len()
            + ch.len_utf8()
            + if chars.peek().is_some() {
                '…'.len_utf8()
            } else {
                0
            }
            > limit
        {
            result.push('…');
            break;
        }
        result.push(ch);
    }
    result
}

/// `TABS` — the single source of truth for rendering and `Tab`-cycle order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ViewId {
    Goals,
    Tasks,
    Agents,
    Logs,
}

impl ViewId {
    pub const ALL: [ViewId; 4] = [ViewId::Goals, ViewId::Tasks, ViewId::Agents, ViewId::Logs];

    pub fn hotkey(self) -> char {
        match self {
            ViewId::Goals => 'G',
            ViewId::Tasks => 'T',
            ViewId::Agents => 'A',
            ViewId::Logs => 'L',
        }
    }

    /// The rendered label — note `logs` renders as `ACTIONS`.
    pub fn label(self) -> &'static str {
        match self {
            ViewId::Goals => "GOALS",
            ViewId::Tasks => "TASKS",
            ViewId::Agents => "AGENTS",
            ViewId::Logs => "ACTIONS",
        }
    }

    /// The command-bar item label.
    pub fn item_label(self) -> &'static str {
        match self {
            ViewId::Goals => "goals",
            ViewId::Tasks => "tasks",
            ViewId::Agents => "agents",
            ViewId::Logs => "events",
        }
    }

    fn index(self) -> usize {
        ViewId::ALL.iter().position(|v| *v == self).unwrap_or(0)
    }

    pub fn next(self) -> ViewId {
        ViewId::ALL[(self.index() + 1) % ViewId::ALL.len()]
    }

    pub fn prev(self) -> ViewId {
        ViewId::ALL[(self.index() + ViewId::ALL.len() - 1) % ViewId::ALL.len()]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputMode {
    /// Normal mode. (`'new_task'` is not ported — PARITY.md §13.)
    None,
    Command,
    Wizard,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntityType {
    Task,
    Agent,
    Goal,
}

impl EntityType {
    /// The undo-banner label.
    pub fn label(self) -> &'static str {
        match self {
            EntityType::Task => "Task",
            EntityType::Agent => "Agent",
            EntityType::Goal => "Goal",
        }
    }
}

/// One soft-delete awaiting its 5-second window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingDeletion {
    pub key: u64,
    pub entity_type: EntityType,
    pub entity_id: String,
    pub entity_name: String,
    pub expires_at: i64,
    pub needs_force_stop: bool,
}

/// A left press waiting for its release.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArmedClick {
    /// Which target was pressed. The release must resolve to the same one.
    pub id: ZoneId,
    /// What that target does, captured at press time.
    pub kind: ZoneKind,
    /// Whether this press was inside the double-click window of the previous
    /// click on the same target, so the release fires the secondary action.
    pub secondary: bool,
    pub at_ms: i64,
}

/// Mouse bookkeeping. Lives in `AppState` like everything else that holds
/// app state and arithmetic (`lib.rs:10`), so every mouse case is inspectable
/// from a test and `App` stays a thin shell.
#[derive(Debug, Clone)]
pub struct MouseState {
    /// Whether capture is on. `App` owns only this desire; the runner
    /// reconciles the terminal modes from it, so the toggle is testable
    /// without a terminal (design §3.5).
    pub enabled: bool,
    /// What the last drawn frame made clickable. Written only by
    /// `App::set_hits`; invalidated on a resize, on any layout-changing
    /// action, and on every `handle_key` (§3.2.3).
    pub hits: HitMap,
    pub armed: Option<ArmedClick>,
    /// The last completed primary click, for double-click matching. Cleared
    /// when a secondary fires, on any invalidation, and on any step or mode
    /// change — so a third click is a fresh primary, not another double.
    pub last_click: Option<(ZoneId, i64)>,
    /// When `Q quit` was armed. A second click inside the window exits.
    pub quit_armed_at: Option<i64>,
    /// Clicks in the command-bar row are dropped until this time: the hint
    /// set is status-conditional and reflows under the pointer (§3.3.3).
    pub footer_suppressed_until: Option<i64>,
    /// Selection steps the wheel has already spent this frame (§3.3.2).
    pub wheel_steps: u8,
    pub press_cell: Option<(u16, u16)>,
    pub last_pointer: Option<(u16, u16)>,
    pub cancelled_left: bool,
}

impl MouseState {
    /// Is `Q quit` armed at `now_ms`? Read by both the dispatcher and the
    /// footer, so the rendered `Q quit?` cannot disagree with the behaviour.
    pub fn quit_armed(&self, now_ms: i64) -> bool {
        self.quit_armed_at
            .is_some_and(|at| now_ms - at <= crate::mouse::QUIT_ARM_MS)
    }
}

impl Default for MouseState {
    fn default() -> Self {
        MouseState {
            // On by default: the feature was asked for in three environments
            // the user actually uses, and an opt-in default works in none of
            // them unconfigured (§3.5).
            enabled: true,
            hits: HitMap::default(),
            armed: None,
            last_click: None,
            quit_armed_at: None,
            footer_suppressed_until: None,
            wheel_steps: 0,
            press_cell: None,
            last_pointer: None,
            cancelled_left: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToastKind {
    Done,
    Failed,
    Review,
}

impl ToastKind {
    pub fn icon(self) -> &'static str {
        match self {
            ToastKind::Done => "✓",
            ToastKind::Failed => "✕",
            ToastKind::Review => "◈",
        }
    }

    pub fn foreground(self) -> Token {
        match self {
            ToastKind::Done => Token::Green,
            ToastKind::Failed => Token::Red,
            ToastKind::Review => Token::Blue,
        }
    }

    pub fn background(self) -> Token {
        match self {
            ToastKind::Done => Token::SuccessBg,
            ToastKind::Failed => Token::ErrorBg,
            ToastKind::Review => Token::InfoBg,
        }
    }

    /// Auto-dismiss delay; `onDismiss` fires at `delay + 360`.
    pub fn delay_ms(self) -> i64 {
        match self {
            ToastKind::Done => 4_000,
            ToastKind::Failed => 8_000,
            ToastKind::Review => 6_000,
        }
    }

    /// Whether the terminal bell rings — never for `done`.
    pub fn rings_bell(self) -> bool {
        matches!(self, ToastKind::Failed | ToastKind::Review)
    }

    pub fn message(self, agent_name: Option<&str>) -> String {
        match self {
            ToastKind::Done => format!("Task completed by {}", agent_name.unwrap_or("Agent")),
            ToastKind::Failed => "Task failed — press Enter for details".into(),
            ToastKind::Review => "Task ready for review — press A to approve".into(),
        }
    }

    pub fn from_status(status: TaskStatus) -> Option<ToastKind> {
        match status {
            TaskStatus::Done => Some(ToastKind::Done),
            TaskStatus::Failed => Some(ToastKind::Failed),
            TaskStatus::Review => Some(ToastKind::Review),
            _ => None,
        }
    }
}

/// Toast fade window at both ends.
pub const TOAST_FADE_MS: i64 = 360;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Toast {
    pub id: u64,
    pub kind: ToastKind,
    pub title: String,
    pub message: String,
    pub created_at: i64,
}

impl Toast {
    /// Dimmed for the first and last 360 ms.
    pub fn is_dimmed(&self, now_ms: i64) -> bool {
        let age = now_ms - self.created_at;
        age < TOAST_FADE_MS || age >= self.kind.delay_ms()
    }

    pub fn is_expired(&self, now_ms: i64) -> bool {
        now_ms - self.created_at >= self.kind.delay_ms() + TOAST_FADE_MS
    }
}

/// Tab flash: 3 blinks × 2 phases × 2 animation ticks.
pub const FLASH_TICKS: u64 = 12;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TabFlash {
    pub tab: ViewId,
    pub color: Token,
    pub started_tick: u64,
}

impl TabFlash {
    /// Bright on even tick pairs, dim on odd ones.
    pub fn is_bright(&self, tick: u64) -> bool {
        let elapsed = tick.saturating_sub(self.started_tick);
        (elapsed / 2).is_multiple_of(2)
    }

    pub fn is_done(&self, tick: u64) -> bool {
        tick.saturating_sub(self.started_tick) >= FLASH_TICKS
    }
}

/// Command-bar editing state.
#[derive(Debug, Default)]
pub struct CommandBarState {
    pub input: TextInput,
    pub suggestion_index: usize,
    pub history: CommandHistory,
    /// The lines of the `/msg` refusal (or broadcast confirm) the bar is being
    /// kept open for, drawn in the otherwise-empty suggestion rows just above
    /// it.
    ///
    /// They are also in the activity feed, but the feed is **not rendered
    /// while the bar is open** — the bottom panel belongs to the suggestion
    /// list — so without this the one thing the user needs to read is the one
    /// thing off screen (design-v2 §3.6.3 × §3.5, found in the live check).
    /// Cleared by the next keystroke, a send, `Esc` or leaving command mode.
    pub retained_refusal: Vec<String>,
}

/// Which detail panel is open, if any.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetailPanel {
    None,
    Entity,
    Log,
}

/// A rendered row of the task list. Section headers are visual only — they do
/// **not** consume selection indices.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskRow {
    GoalSection {
        goal_id: String,
        title: String,
        total: usize,
        done: usize,
    },
    UngroupedSection {
        total: usize,
    },
    Task {
        index: usize,
        selection: usize,
    },
    ShowAll {
        hidden: usize,
        selection: usize,
    },
    AddTask {
        selection: usize,
    },
}

/// Session-only agent ordering; Original preserves status then source order.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum AgentSortField {
    #[default]
    Original,
    Name,
    Status,
    AdapterModel,
    Location,
}

impl AgentSortField {
    pub fn next(self) -> Self {
        match self {
            Self::Original => Self::Name,
            Self::Name => Self::Status,
            Self::Status => Self::AdapterModel,
            Self::AdapterModel => Self::Location,
            Self::Location => Self::Original,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Original => "Original",
            Self::Name => "Name",
            Self::Status => "Status",
            Self::AdapterModel => "Adapter/Model",
            Self::Location => "Location",
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AgentSort {
    pub field: AgentSortField,
    pub descending: bool,
}

/// A rendered row of the agent list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentRow {
    TeamSection {
        team_id: String,
        name: String,
        count: usize,
        lead_name: Option<String>,
    },
    UnassignedSection {
        count: usize,
    },
    Agent {
        index: usize,
        selection: usize,
    },
    AddAgent {
        selection: usize,
    },
}

/// `STATUS_ORDER`.
pub fn task_status_order(status: TaskStatus) -> u8 {
    match status {
        TaskStatus::InProgress => 0,
        TaskStatus::Retrying => 1,
        TaskStatus::Review => 2,
        TaskStatus::Todo => 3,
        TaskStatus::Done => 4,
        TaskStatus::Failed => 5,
        TaskStatus::Cancelled => 6,
    }
}

/// `AGENT_STATUS_ORDER`.
pub fn agent_status_order(status: AgentStatus) -> u8 {
    match status {
        AgentStatus::Running => 0,
        AgentStatus::Idle => 1,
        AgentStatus::Error => 2,
        AgentStatus::Disabled => 3,
        AgentStatus::Offline => 4,
    }
}

/// `RUNNABLE` — the statuses `r` / `/run` accept.
pub fn is_runnable(status: TaskStatus) -> bool {
    matches!(
        status,
        TaskStatus::Todo | TaskStatus::Failed | TaskStatus::Cancelled
    )
}

pub struct AppState {
    // ── Terminal ───────────────────────────────────────────────────────────
    pub frame_generation: u64,
    pub width: u16,
    pub height: u16,

    // ── Live data ──────────────────────────────────────────────────────────
    pub tasks: Vec<Task>,
    pub agents: Vec<Agent>,
    pub goals: Vec<Goal>,
    pub teams: Vec<Team>,
    pub orch_state: OrchestratorState,
    pub peer_status: String,

    // ── View ───────────────────────────────────────────────────────────────
    pub focus: Focus,
    pub detail_focus: Focus,
    pub pane_states: Vec<(PaneKey, crate::navigation::ScrollState)>,
    pub activity_selected: Option<u64>,
    /// Fallback position only; message IDs remain authoritative.
    pub activity_last_position: usize,
    pub activity_scroll: usize,
    pub activity_follow_tail: bool,
    pub log_selected_id: Option<u64>,
    pub picker_scroll: usize,
    pub suggestion_scroll: usize,
    pub retained_evicted: usize,
    pub retained_rejected: usize,
    pub retained_bytes: usize,
    /// A clicked row anchor and its source snapshot; never applied to changed bytes.
    pub text_anchor: Option<(ViewerSource, usize, u64)>,
    pub viewer: Option<TextViewer>,
    pub wizard_sessions: WizardSessionStore,
    pub copy_feedback: CopyFeedbackStore,
    pub copy_help: Option<CopyHelpContext>,
    /// Fixed viewer/editor slots, keyed by complete origin and generation.
    pub text_footer_guards: [Option<TextFooterGuard>; 2],
    pub(crate) copy_origins: [Option<CopyOrigin>; 3],
    pub(crate) next_copy_generation: u64,
    pub view: ViewId,
    pub goal_selected: usize,
    pub task_selected: usize,
    pub agent_selected: usize,
    pub agent_sort: AgentSort,
    pub goal_scroll: usize,
    pub task_scroll: usize,
    pub agent_scroll: usize,
    pub show_all_tasks: bool,
    pub group_by_goal: bool,

    // ── Detail panel / split ───────────────────────────────────────────────
    pub detail: DetailPanel,
    pub detail_maximized: bool,
    pub split_offset: i32,
    pub goal_detail_scroll: usize,
    pub goal_progress_report: Option<String>,

    // ── Logs view ──────────────────────────────────────────────────────────
    /// `-1` means tail mode.
    pub log_selected_index: i64,
    pub log_scroll_offset: usize,
    /// Empty means "all agents".
    pub log_agent_filter: IndexSet<String>,
    pub log_type_filter: IndexSet<MsgType>,
    pub show_agent_picker: bool,
    pub show_type_picker: bool,
    pub picker_cursor: usize,
    pub picker_draft: IndexSet<String>,
    pub type_picker_draft: IndexSet<MsgType>,

    // ── Activity feed ──────────────────────────────────────────────────────
    pub activity_filter: IndexSet<MsgType>,
    pub messages: Vec<Message>,
    pub pending_messages: Vec<PendingMessage>,
    pub message_flush_at: Option<i64>,
    pub refresh_at: Option<i64>,
    pub runs: RunMaps,
    pub(crate) next_message_id: u64,
    /// `(running, queued)` from the last `orchestrator:tick`, so an unchanged
    /// count does not add a feed line every tick (PARITY.md §12.8 (k)).
    pub(crate) last_tick: Option<(usize, usize)>,

    // ── Mouse ──────────────────────────────────────────────────────────────
    pub mouse: MouseState,

    // ── Modes / overlays ───────────────────────────────────────────────────
    pub input_mode: InputMode,
    pub show_help: bool,
    pub command_bar: CommandBarState,
    pub wizard: Option<Wizard>,
    pub pending_attachments: Vec<String>,

    // ── Soft delete ────────────────────────────────────────────────────────
    pub pending_deletions: Vec<PendingDeletion>,
    pub(crate) next_deletion_key: u64,

    // ── Notifications ──────────────────────────────────────────────────────
    pub toasts: Vec<Toast>,
    pub(crate) next_toast_id: u64,
    pub flash: Option<TabFlash>,
    pub notifications: NotificationPreferences,

    // ── Onboarding / meta ──────────────────────────────────────────────────
    pub onboarding: OnboardingStep,
    pub onboarding_dismiss_at: Option<i64>,
    pub onboarding_completed_sent: bool,
    pub project_name: String,
    pub version: String,
    pub latest_version: Option<String>,
    pub install_started: bool,
    pub watch_active: bool,
    pub observer_mode: bool,
    pub default_adapter: String,
    pub max_concurrent: u32,
    pub palette: Palette,
    pub tick: u64,
    pub should_exit: bool,
}

impl Default for AppState {
    fn default() -> Self {
        AppState {
            frame_generation: 0,
            width: 80,
            height: 24,
            tasks: Vec::new(),
            agents: Vec::new(),
            peer_status: "listen off · controller none".into(),
            goals: Vec::new(),
            teams: Vec::new(),
            orch_state: default_state(),
            focus: Focus::default(),
            detail_focus: Focus::MainList,
            pane_states: Vec::new(),
            activity_selected: None,
            activity_last_position: 0,
            activity_scroll: 0,
            activity_follow_tail: true,
            log_selected_id: None,
            picker_scroll: 0,
            suggestion_scroll: 0,
            retained_evicted: 0,
            retained_rejected: 0,
            retained_bytes: 0,
            text_anchor: None,
            viewer: None,
            wizard_sessions: WizardSessionStore::default(),
            copy_feedback: CopyFeedbackStore::default(),
            copy_help: None,
            text_footer_guards: [None, None],
            copy_origins: [None, None, None],
            next_copy_generation: 0,
            view: ViewId::Tasks,
            goal_selected: 0,
            task_selected: 0,
            agent_selected: 0,
            agent_sort: AgentSort::default(),
            goal_scroll: 0,
            task_scroll: 0,
            agent_scroll: 0,
            show_all_tasks: false,
            group_by_goal: false,
            detail: DetailPanel::None,
            detail_maximized: false,
            split_offset: 0,
            goal_detail_scroll: 0,
            goal_progress_report: None,
            log_selected_index: -1,
            log_scroll_offset: 0,
            log_agent_filter: IndexSet::new(),
            log_type_filter: preset_types("all"),
            show_agent_picker: false,
            show_type_picker: false,
            picker_cursor: 0,
            picker_draft: IndexSet::new(),
            type_picker_draft: IndexSet::new(),
            activity_filter: preset_types("all"),
            messages: Vec::new(),
            pending_messages: Vec::new(),
            message_flush_at: None,
            refresh_at: None,
            runs: RunMaps::default(),
            last_tick: None,
            next_message_id: 1,
            mouse: MouseState::default(),
            input_mode: InputMode::None,
            show_help: false,
            command_bar: CommandBarState::default(),
            wizard: None,
            pending_attachments: Vec::new(),
            pending_deletions: Vec::new(),
            next_deletion_key: 0,
            toasts: Vec::new(),
            next_toast_id: 1,
            flash: None,
            notifications: NotificationPreferences {
                toast: true,
                bell: false,
            },
            onboarding: OnboardingStep::Welcome,
            onboarding_dismiss_at: None,
            onboarding_completed_sent: false,
            project_name: "project".into(),
            version: "0.0.0".into(),
            latest_version: None,
            install_started: false,
            watch_active: false,
            observer_mode: false,
            default_adapter: "claude".into(),
            max_concurrent: 2,
            palette: Palette::default(),
            tick: 0,
            should_exit: false,
        }
    }
}

impl AppState {
    /// Allocate a fresh opening epoch. Exhaustion retires all identities and
    /// returns an error; no generation can silently alias a prior instance.
    fn copy_generation(&mut self) -> BackendResult<u64> {
        let Some(next) = self.next_copy_generation.checked_add(1) else {
            self.copy_origins = [None, None, None];
            self.text_footer_guards = [None, None];
            self.copy_feedback.viewer = CopyFeedbackSlot::default();
            self.copy_feedback.editor = CopyFeedbackSlot::default();
            self.copy_feedback.field = CopyFeedbackSlot::default();
            self.copy_help = None;
            self.invalidate_copy_hits();
            return Err("Copy generation exhausted".into());
        };
        self.next_copy_generation = next;
        Ok(next)
    }
    fn invalidate_copy_hits(&mut self) {
        self.mouse.hits.invalidate();
        self.mouse.armed = None;
        self.mouse.last_click = None;
    }
    fn install_copy_origin(&mut self, origin: CopyOrigin) -> CopyOrigin {
        let index = origin.slot();
        if let Some(old) = self.copy_origins[index].clone() {
            self.retire_copy_origin(&old);
        }
        self.copy_origins[index] = Some(origin.clone());
        self.invalidate_copy_hits();
        origin
    }
    pub fn register_viewer_origin(&mut self, source: ViewerSource) -> BackendResult<CopyOrigin> {
        let generation = self.copy_generation()?;
        Ok(self.install_copy_origin(CopyOrigin::Viewer { source, generation }))
    }
    pub fn register_editor_origin(
        &mut self,
        draft: DraftKey,
        step_id: String,
    ) -> BackendResult<CopyOrigin> {
        let generation = self.copy_generation()?;
        Ok(self.install_copy_origin(CopyOrigin::Editor {
            draft,
            step_id,
            generation,
        }))
    }
    pub fn register_field_origin(&mut self, source: ViewerSource) -> BackendResult<CopyOrigin> {
        let generation = self.copy_generation()?;
        Ok(self.install_copy_origin(CopyOrigin::Field { source, generation }))
    }
    pub fn live_copy_origin(&self, surface: &crate::hit::TextSurfaceId) -> Option<&CopyOrigin> {
        use crate::hit::TextSurfaceId;
        let origin = self.copy_origins[match surface {
            TextSurfaceId::Viewer(_) => 0,
            TextSurfaceId::Editor(..) => 1,
        }]
        .as_ref()?;
        match (surface, origin) {
            (TextSurfaceId::Viewer(source), CopyOrigin::Viewer { source: live, .. })
                if source == live =>
            {
                Some(origin)
            }
            (
                TextSurfaceId::Editor(draft, step),
                CopyOrigin::Editor {
                    draft: live,
                    step_id,
                    ..
                },
            ) if draft == live && step == step_id => Some(origin),
            _ => None,
        }
    }
    pub fn copy_origin_is_live(&self, origin: &CopyOrigin) -> bool {
        self.copy_origins[origin.slot()].as_ref() == Some(origin)
    }
    pub fn retire_copy_origin(&mut self, origin: &CopyOrigin) {
        if !self.copy_origin_is_live(origin) {
            return;
        }
        self.clear_copy_feedback(origin);
        self.copy_origins[origin.slot()] = None;
        if origin.slot() < 2 {
            self.text_footer_guards[origin.slot()] = None;
        }
        if self
            .copy_help
            .as_ref()
            .is_some_and(|help| &help.origin == origin)
        {
            self.copy_help = None;
            self.show_help = false;
        }
        self.invalidate_copy_hits();
    }
    /// Identity only: no editor text or replacement session is cloned.
    pub fn active_copy_origin(&self) -> Option<CopyOrigin> {
        if self.show_help
            || self.show_agent_picker
            || self.show_type_picker
            || self.input_mode == InputMode::Command
            || self.wizard_sessions.pending_leave.is_some()
        {
            return None;
        }
        self.copy_origins.iter().flatten().next().cloned()
    }
    pub fn suppress_text_footer(&mut self, origin: &CopyOrigin, now_ms: i64) {
        if origin.slot() >= 2 || !self.copy_origin_is_live(origin) {
            return;
        }
        self.text_footer_guards[origin.slot()] = Some(TextFooterGuard {
            origin: origin.clone(),
            until_ms: now_ms.saturating_add(crate::mouse::DOUBLE_CLICK_MS),
        });
    }
    pub fn text_footer_suppressed(&self, origin: &CopyOrigin, now_ms: i64) -> bool {
        origin.slot() < 2
            && self.copy_origin_is_live(origin)
            && self.text_footer_guards[origin.slot()]
                .as_ref()
                .is_some_and(|guard| &guard.origin == origin && now_ms < guard.until_ms)
    }
    pub fn register_copy_request(
        &mut self,
        origin: CopyOrigin,
        target: CopyTarget,
        accepted: BackendResult<CopyTicket>,
        now_ms: i64,
    ) {
        let Some(seq) = self.copy_feedback.request_seq.checked_add(1) else {
            self.copy_feedback.viewer = CopyFeedbackSlot::default();
            self.copy_feedback.editor = CopyFeedbackSlot::default();
            self.copy_feedback.field = CopyFeedbackSlot::default();
            self.copy_feedback.contract_errors =
                self.copy_feedback.contract_errors.saturating_add(1);
            return;
        };
        self.copy_feedback.request_seq = seq;
        let meta = CopyRequestMeta {
            origin: origin.clone(),
            target,
            request_seq: seq,
            at_ms: now_ms,
        };
        let accepted = match accepted {
            Ok(ticket)
                if self
                    .copy_feedback
                    .pending
                    .iter()
                    .flatten()
                    .any(|(id, _)| *id == ticket)
                    || self.copy_feedback.pending_count() == 2 =>
            {
                self.copy_feedback.contract_errors =
                    self.copy_feedback.contract_errors.saturating_add(1);
                Err("Copy facade contract violation".into())
            }
            other => other,
        };
        let feedback = match accepted {
            Ok(ticket) => {
                if let Some(entry) = self
                    .copy_feedback
                    .pending
                    .iter_mut()
                    .find(|entry| entry.is_none())
                {
                    *entry = Some((ticket, meta.clone()));
                }
                None
            }
            Err(error) => {
                let destination = match target {
                    CopyTarget::Auto => "Auto",
                    CopyTarget::Terminal => "Terminal",
                    CopyTarget::Native => "Host",
                };
                Some(CopyFeedback {
                    ticket: None,
                    request: meta.clone(),
                    outcome: CopyOutcome::Failed(safe_copy_detail(
                        &format!("{destination}: {error}"),
                        256,
                    )),
                    guidance: None,
                    completed_at_ms: now_ms,
                })
            }
        };
        // A retired caller still drains accepted work but cannot resurrect UI.
        if !self.copy_origin_is_live(&origin) {
            return;
        }
        *self.copy_feedback.slot_mut(&origin) = CopyFeedbackSlot {
            latest: Some(meta),
            feedback,
            footer_hidden: false,
        };
    }
    pub fn apply_copy_result(&mut self, result: CopyResult, now_ms: i64) {
        let Some(index) = self
            .copy_feedback
            .pending
            .iter()
            .position(|entry| entry.as_ref().is_some_and(|(id, _)| *id == result.ticket))
        else {
            self.copy_feedback.unknown_results =
                self.copy_feedback.unknown_results.saturating_add(1);
            return;
        };
        let Some((_, meta)) = self.copy_feedback.pending[index].take() else {
            return;
        };
        if !self.copy_origin_is_live(&meta.origin)
            || self.copy_feedback.slot(&meta.origin).latest.as_ref() != Some(&meta)
        {
            return;
        }
        let outcome = match result.outcome {
            CopyOutcome::Failed(error) => CopyOutcome::Failed(safe_copy_detail(&error, 256)),
            other => other,
        };
        let slot = self.copy_feedback.slot_mut(&meta.origin);
        slot.feedback = Some(CopyFeedback {
            ticket: Some(result.ticket),
            request: meta,
            outcome,
            guidance: result.guidance.as_deref().map(|s| safe_copy_detail(s, 512)),
            completed_at_ms: now_ms,
        });
        slot.footer_hidden = false;
    }
    pub fn feedback_for(&self, origin: &CopyOrigin) -> Option<&CopyFeedback> {
        let slot = self.copy_feedback.slot(origin);
        slot.feedback
            .as_ref()
            .filter(|feedback| &feedback.request.origin == origin)
    }
    pub fn clear_copy_feedback(&mut self, origin: &CopyOrigin) {
        if self
            .copy_feedback
            .slot(origin)
            .latest
            .as_ref()
            .is_some_and(|meta| &meta.origin == origin)
        {
            *self.copy_feedback.slot_mut(origin) = CopyFeedbackSlot::default();
        }
    }
    pub fn dismiss_copy_feedback(&mut self, origin: &CopyOrigin) {
        if self
            .copy_feedback
            .slot(origin)
            .latest
            .as_ref()
            .is_some_and(|meta| &meta.origin == origin)
        {
            self.copy_feedback.slot_mut(origin).footer_hidden = true;
        }
    }
    pub fn copy_footer_visible(&self, origin: &CopyOrigin) -> bool {
        let slot = self.copy_feedback.slot(origin);
        !slot.footer_hidden
            && slot
                .latest
                .as_ref()
                .is_some_and(|meta| &meta.origin == origin)
    }
    pub fn pane_key(&self, focus: Focus) -> PaneKey {
        let entity = match focus {
            Focus::GoalDetail(_) => self
                .selected_goal()
                .map(|g| PaneEntity::Entity(g.id.clone())),
            Focus::TaskDetail(_) => self
                .selected_task()
                .map(|t| PaneEntity::Entity(t.id.clone())),
            Focus::AgentDetail(_) => self
                .selected_agent()
                .map(|a| PaneEntity::Entity(a.id.clone())),
            Focus::LogDetail(_) => self.selected_log().map(|m| PaneEntity::Message(m.id)),
            _ => None,
        };
        PaneKey {
            view: self.view,
            entity,
            section: focus,
        }
    }
    pub fn pane_scroll(&self, key: &PaneKey) -> crate::navigation::ScrollState {
        self.pane_states
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, s)| s.clone())
            .unwrap_or_default()
    }
    pub fn set_pane_scroll(&mut self, key: PaneKey, scroll: crate::navigation::ScrollState) {
        self.pane_states
            .retain(|(saved, _)| saved.view != key.view || saved.entity == key.entity);
        if let Some((_, state)) = self.pane_states.iter_mut().find(|(saved, _)| saved == &key) {
            *state = scroll;
        } else {
            self.pane_states.push((key, scroll));
        }
    }
    pub fn focus_ring(&self) -> Vec<Focus> {
        if self.width < crate::layout::MIN_WIDTH || self.height < crate::layout::MIN_HEIGHT {
            return vec![Focus::MainList];
        }
        let mut ring = Vec::new();
        if !self.detail_maximized || !self.detail_open() && self.view == ViewId::Logs {
            ring.push(Focus::MainList);
        }
        if !self.detail_open() {
            if self.view != ViewId::Logs {
                ring.push(Focus::Activity);
            }
            return ring;
        }
        match self.view {
            ViewId::Goals => {
                if let Some(g) = self.selected_goal() {
                    ring.extend([
                        Focus::GoalDetail(GoalSection::Overview),
                        Focus::GoalDetail(GoalSection::Description),
                    ]);
                    if self.goal_progress_report.is_some() {
                        ring.push(Focus::GoalDetail(GoalSection::Progress));
                    }
                    if !self.tasks_of_goal(&g.id).is_empty() {
                        ring.push(Focus::GoalDetail(GoalSection::Tasks));
                    }
                }
            }
            ViewId::Tasks => {
                if let Some(t) = self.selected_task() {
                    ring.push(Focus::TaskDetail(TaskSection::Overview));
                    if t.attachments.as_ref().is_some_and(|a| !a.is_empty()) {
                        ring.push(Focus::TaskDetail(TaskSection::Attachments));
                    }
                    ring.push(Focus::TaskDetail(TaskSection::Description));
                    if t.proof
                        .as_ref()
                        .and_then(|p| p.agent_summary.as_ref())
                        .is_some_and(|s| !s.is_empty())
                    {
                        ring.push(Focus::TaskDetail(TaskSection::Result));
                    }
                    if t.proof
                        .as_ref()
                        .is_some_and(|p| !p.files_changed.is_empty())
                    {
                        ring.push(Focus::TaskDetail(TaskSection::Files));
                    }
                    if self
                        .messages
                        .iter()
                        .any(|m| m.task_id.as_deref() == Some(t.id.as_str()))
                    {
                        ring.push(Focus::TaskDetail(TaskSection::Activity));
                    }
                }
            }
            ViewId::Agents => {
                if let Some(a) = self.selected_agent() {
                    ring.push(Focus::AgentDetail(AgentSection::Overview));
                    if a.last_error.is_some() {
                        ring.push(Focus::AgentDetail(AgentSection::Error));
                    }
                    ring.push(Focus::AgentDetail(AgentSection::Role));
                }
            }
            ViewId::Logs => {
                if self.selected_log().is_some() {
                    ring.extend([
                        Focus::LogDetail(LogSection::Metadata),
                        Focus::LogDetail(LogSection::Body),
                    ]);
                }
            }
        }
        if ring.is_empty() {
            ring.push(Focus::MainList);
        }
        ring
    }
    pub fn set_focus(&mut self, focus: Focus) {
        if self.focus != focus {
            self.text_anchor = None;
            self.resume_feed_at_bottom(focus);
        }
        self.focus = focus;
        if !matches!(focus, Focus::MainList | Focus::Activity | Focus::TextViewer) {
            self.detail_focus = focus;
        }
        self.sync_field_origin();
    }
    pub fn normalize_focus(&mut self) {
        let log = self
            .log_selected_id
            .or_else(|| self.selected_log().map(|m| m.id));
        self.reconcile_message_selection(log);
        if self.detail_open()
            && match self.view {
                ViewId::Goals => self.selected_goal().is_none(),
                ViewId::Tasks => self.selected_task().is_none(),
                ViewId::Agents => self.selected_agent().is_none(),
                ViewId::Logs => self.selected_log().is_none(),
            }
        {
            self.detail = DetailPanel::None;
            self.detail_maximized = false;
        }
        let ring = self.focus_ring();
        if !ring.contains(&self.detail_focus) {
            self.detail_focus = ring
                .iter()
                .copied()
                .find(|f| *f != Focus::MainList)
                .unwrap_or(Focus::MainList);
        }
        if self.viewer.is_none() && !ring.contains(&self.focus) {
            self.focus = ring[0];
        }
        // Only the current entity's per-section state is retained per view.
        let keys: Vec<_> = ring.iter().map(|f| self.pane_key(*f)).collect();
        self.pane_states
            .retain(|(k, _)| k.view != self.view || keys.contains(k));
        self.sync_field_origin();
    }
    pub fn sync_field_origin(&mut self) {
        let source = if self.viewer.is_none() && self.input_mode == InputMode::None {
            self.focused_source()
        } else {
            None
        };
        let previous = self.copy_origins[2].clone();
        if let Some(source) = source {
            if !matches!(&previous,Some(CopyOrigin::Field{source:s,..}) if s==&source) {
                let _ = self.register_field_origin(source);
            }
        } else if let Some(old) = previous {
            self.retire_copy_origin(&old);
        }
    }
    pub fn focused_source(&self) -> Option<ViewerSource> {
        let entity = |kind, id: String, field: &str| {
            Some(ViewerSource::Entity {
                kind,
                id,
                field: field.into(),
            })
        };
        match self.focus {
            Focus::Activity => self
                .activity_selected
                .or_else(|| {
                    self.activity_messages()
                        .last()
                        .map(|i| self.messages[*i].id)
                })
                .map(ViewerSource::Message),
            Focus::MainList if self.view == ViewId::Logs => {
                self.selected_log().map(|m| ViewerSource::Message(m.id))
            }
            Focus::GoalDetail(GoalSection::Description) => entity(
                EntityType::Goal,
                self.selected_goal()?.id.clone(),
                "description",
            ),
            Focus::GoalDetail(GoalSection::Progress) => entity(
                EntityType::Goal,
                self.selected_goal()?.id.clone(),
                "progress",
            ),
            Focus::TaskDetail(TaskSection::Description) => entity(
                EntityType::Task,
                self.selected_task()?.id.clone(),
                "description",
            ),
            Focus::TaskDetail(TaskSection::Result) => {
                entity(EntityType::Task, self.selected_task()?.id.clone(), "result")
            }
            Focus::AgentDetail(AgentSection::Role) => {
                entity(EntityType::Agent, self.selected_agent()?.id.clone(), "role")
            }
            Focus::AgentDetail(AgentSection::Error) => entity(
                EntityType::Agent,
                self.selected_agent()?.id.clone(),
                "error",
            ),
            Focus::LogDetail(LogSection::Body) => {
                self.selected_log().map(|m| ViewerSource::Message(m.id))
            }
            Focus::GoalDetail(GoalSection::Tasks) => {
                let goal = self.selected_goal()?;
                let rows = self.tasks_of_goal(&goal.id);
                let selected = self
                    .pane_scroll(&self.pane_key(self.focus))
                    .selected
                    .unwrap_or(0);
                let task = self.tasks.get(*rows.get(selected)?)?;
                entity(EntityType::Task, task.id.clone(), "description")
            }
            Focus::TaskDetail(TaskSection::Attachments) => entity(
                EntityType::Task,
                self.selected_task()?.id.clone(),
                "attachments",
            ),
            Focus::TaskDetail(TaskSection::Files) => {
                entity(EntityType::Task, self.selected_task()?.id.clone(), "files")
            }
            Focus::TaskDetail(TaskSection::Activity) => {
                let task = self.selected_task()?;
                let rows: Vec<_> = self
                    .messages
                    .iter()
                    .filter(|m| m.task_id.as_deref() == Some(task.id.as_str()))
                    .collect();
                let selected = self
                    .pane_scroll(&self.pane_key(self.focus))
                    .selected
                    .unwrap_or_else(|| rows.len().saturating_sub(1));
                rows.get(selected).map(|m| ViewerSource::Message(m.id))
            }
            _ => None,
        }
    }
    pub fn selected_log(&self) -> Option<&Message> {
        if self.log_selected_index < 0 {
            return self.filtered_messages().last().map(|i| &self.messages[*i]);
        }
        let index = usize::try_from(self.log_selected_index).ok()?;
        let filtered = self.filtered_messages();
        if let Some(id) = self.log_selected_id
            && let Some(message) = filtered
                .iter()
                .map(|i| &self.messages[*i])
                .find(|m| m.id == id)
        {
            return Some(message);
        }
        filtered
            .get(index.min(filtered.len().saturating_sub(1)))
            .and_then(|i| self.messages.get(*i))
    }
    /// Resolve original bytes before panel clipping; viewers freeze this document.
    pub fn focused_text(&self) -> Option<ViewerDocument> {
        let source = self.focused_source()?;
        let mut document = self.text_for_source(&source, 0, 0)?;
        let fingerprint = Self::source_fingerprint(&document.body);
        if let Some((saved, byte, hash)) = &self.text_anchor
            && saved == &source
            && *hash == fingerprint
        {
            document.initial_anchor = (*byte).min(document.body.len());
            return Some(document);
        }
        let key = self.pane_key(self.focus);
        // Keyboard anchors may reuse the last body geometry after click invalidation.
        if let Some(metrics) = self.mouse.hits.metrics.panes.iter().find(|p| p.key == key) {
            let current_frame = self
                .mouse
                .hits
                .source_versions
                .iter()
                .any(|(saved, hash)| saved == &source && *hash == fingerprint);
            if current_frame
                && let Some((_, _, byte)) = self
                    .mouse
                    .hits
                    .source_anchors
                    .iter()
                    .find(|(r, s, _)| s == &source && r.y >= metrics.body.y)
            {
                document.initial_anchor = *byte;
            } else if matches!(
                self.focus,
                Focus::GoalDetail(GoalSection::Description | GoalSection::Progress)
                    | Focus::TaskDetail(TaskSection::Description | TaskSection::Result)
                    | Focus::AgentDetail(AgentSection::Role | AgentSection::Error)
                    | Focus::LogDetail(LogSection::Body)
            ) {
                let rows =
                    crate::views::detail::wrap_rows(&document.body, metrics.body.width as usize);
                let offset = self
                    .pane_scroll(&key)
                    .offset
                    .min(rows.len().saturating_sub(metrics.body.height as usize));
                document.initial_anchor = rows.get(offset).map(|(_, byte)| *byte).unwrap_or(0);
            }
        }
        Some(document)
    }
    pub fn source_fingerprint(body: &str) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        body.hash(&mut h);
        h.finish()
    }
    pub fn text_for_source(
        &self,
        source: &ViewerSource,
        initial_anchor: usize,
        captured_at_ms: i64,
    ) -> Option<ViewerDocument> {
        use crate::widgets::text_viewer::{Completeness, Representation};
        use std::borrow::Cow;
        let (title, text): (String, Cow<'_, str>) = match source {
            ViewerSource::Message(id) => {
                let message = self.messages.iter().find(|m| m.id == *id)?;
                return Some(message_document(message, initial_anchor, captured_at_ms));
            }
            ViewerSource::Entity { kind, id, field } => match kind {
                EntityType::Goal => {
                    let g = self.goals.iter().find(|g| &g.id == id)?;
                    let text = match field.as_str() {
                        "description" => Cow::Borrowed(g.description.as_str()),
                        "progress"
                            if self
                                .selected_goal()
                                .is_some_and(|selected| &selected.id == id) =>
                        {
                            Cow::Borrowed(self.goal_progress_report.as_deref()?)
                        }
                        _ => return None,
                    };
                    (format!("{} · {field}", g.title), text)
                }
                EntityType::Task => {
                    let t = self.tasks.iter().find(|t| &t.id == id)?;
                    let text = match field.as_str() {
                        "description" => Cow::Borrowed(t.description.as_str()),
                        "result" => Cow::Borrowed(t.proof.as_ref()?.agent_summary.as_deref()?),
                        "attachments" => Cow::Owned(t.attachments.as_ref()?.join("\n")),
                        "files" => Cow::Owned(t.proof.as_ref()?.files_changed.join("\n")),
                        _ => return None,
                    };
                    (format!("{} · {field}", t.title), text)
                }
                EntityType::Agent => {
                    let a = self.agents.iter().find(|a| &a.id == id)?;
                    let text = match field.as_str() {
                        "role" => Cow::Borrowed(a.role.as_deref().unwrap_or_default()),
                        "error" => Cow::Borrowed(a.last_error.as_ref()?.message.as_str()),
                        _ => return None,
                    };
                    (format!("{} · {field}", a.name), text)
                }
            },
        };
        let original_bytes = text.len();
        let unavailable = original_bytes > 1024 * 1024;
        let body: std::sync::Arc<str> = if unavailable {
            "".into()
        } else {
            text.as_ref().into()
        };
        let mut anchor = initial_anchor.min(body.len());
        while !body.is_char_boundary(anchor) {
            anchor -= 1;
        }
        Some(ViewerDocument {
            source: source.clone(),
            title,
            body,
            representation: if matches!(source, ViewerSource::Message(_)) {
                Representation::Raw
            } else {
                Representation::Field
            },
            raw_body: None,
            completeness: if unavailable {
                Completeness::Unavailable {
                    reason: "Body unavailable: exceeds 1 MiB".into(),
                }
            } else {
                Completeness::Complete
            },
            original_bytes,
            captured_at_ms,
            initial_anchor: anchor,
        })
    }

    // ── Derived collections ────────────────────────────────────────────────

    /// `sortedGoals` — by status rank, stable within a rank.
    pub fn sorted_goals(&self) -> Vec<usize> {
        let mut order: Vec<usize> = (0..self.goals.len()).collect();
        order.sort_by_key(|i| goal_status_order(self.goals[*i].status));
        order
    }

    /// `sortedTasks` — by status rank, then re-grouped by goal when
    /// `groupByGoal` is on (groups in first-appearance order, status sort
    /// preserved inside each group, ungrouped tasks last).
    pub fn sorted_tasks(&self) -> Vec<usize> {
        let mut order: Vec<usize> = (0..self.tasks.len()).collect();
        order.sort_by_key(|i| task_status_order(self.tasks[*i].status));
        if !self.group_by_goal {
            return order;
        }

        let mut group_order: Vec<String> = Vec::new();
        for i in &order {
            if let Some(goal_id) = self.resolved_goal_id(*i)
                && !group_order.iter().any(|g| g == &goal_id)
            {
                group_order.push(goal_id);
            }
        }
        let mut grouped = Vec::with_capacity(order.len());
        for goal_id in &group_order {
            for i in &order {
                if self.resolved_goal_id(*i).as_deref() == Some(goal_id.as_str()) {
                    grouped.push(*i);
                }
            }
        }
        for i in &order {
            if self.resolved_goal_id(*i).is_none() {
                grouped.push(*i);
            }
        }
        grouped
    }

    /// The task's `goalId`, but only when it resolves to a live goal — an
    /// orphaned id renders no badge and falls into UNGROUPED.
    pub fn resolved_goal_id(&self, task_index: usize) -> Option<String> {
        let goal_id = self.tasks[task_index].goal_id.as_deref()?;
        self.goals
            .iter()
            .find(|g| g.id == goal_id)
            .map(|g| g.id.clone())
    }

    pub fn goal_by_id(&self, goal_id: &str) -> Option<&Goal> {
        self.goals.iter().find(|g| g.id == goal_id)
    }

    /// `visibleTasks` — the first 10 unless "show all".
    pub fn visible_tasks(&self) -> Vec<usize> {
        let sorted = self.sorted_tasks();
        if self.show_all_tasks {
            sorted
        } else {
            sorted.into_iter().take(TASK_LIST_LIMIT).collect()
        }
    }

    pub fn hidden_task_count(&self) -> usize {
        self.sorted_tasks().len() - self.visible_tasks().len()
    }

    /// Active teams remain grouped; the session sort applies inside each section.
    /// Original is the existing status/source order, not a timestamp order:
    /// Agent carries neither created nor updated timestamps.
    pub fn sorted_agents(&self) -> Vec<usize> {
        let mut order: Vec<usize> = (0..self.agents.len()).collect();
        order.sort_by(|a, b| {
            let (ta, tb) = (self.team_of(*a), self.team_of(*b));
            let has_a = ta.is_some();
            let has_b = tb.is_some();
            has_b
                .cmp(&has_a)
                .then_with(|| {
                    let na = ta.map(|t| t.name.as_str()).unwrap_or("");
                    let nb = tb.map(|t| t.name.as_str()).unwrap_or("");
                    na.cmp(nb)
                })
                .then_with(|| {
                    let (left, right) = (&self.agents[*a], &self.agents[*b]);
                    let order = match self.agent_sort.field {
                        AgentSortField::Original => agent_status_order(left.status)
                            .cmp(&agent_status_order(right.status))
                            .then_with(|| a.cmp(b)),
                        AgentSortField::Name => left.name.cmp(&right.name),
                        AgentSortField::Status => {
                            agent_status_order(left.status).cmp(&agent_status_order(right.status))
                        }
                        AgentSortField::AdapterModel => {
                            left.adapter.cmp(&right.adapter).then_with(|| {
                                left.config
                                    .model
                                    .as_deref()
                                    .unwrap_or("")
                                    .trim()
                                    .cmp(right.config.model.as_deref().unwrap_or("").trim())
                            })
                        }
                        AgentSortField::Location => left
                            .remote
                            .as_ref()
                            .map_or("local", |remote| remote.alias.as_str())
                            .cmp(
                                right
                                    .remote
                                    .as_ref()
                                    .map_or("local", |remote| remote.alias.as_str()),
                            ),
                    };
                    let order = if self.agent_sort.descending {
                        order.reverse()
                    } else {
                        order
                    };
                    // Keep equal field values deterministic in either direction.
                    if self.agent_sort.field == AgentSortField::Original {
                        order
                    } else {
                        order
                            .then_with(|| left.name.cmp(&right.name))
                            .then_with(|| left.id.cmp(&right.id))
                    }
                })
        });
        order
    }

    pub fn set_agent_sort(&mut self, sort: AgentSort) {
        let selected = self.selected_agent().map(|agent| agent.id.clone());
        self.agent_sort = sort;
        if let Some(id) = selected
            && let Some(position) = self
                .sorted_agents()
                .iter()
                .position(|index| self.agents[*index].id == id)
        {
            self.agent_selected = position;
        }
    }

    /// The **active** team an agent belongs to.
    pub fn team_of(&self, agent_index: usize) -> Option<&Team> {
        let agent_id = &self.agents[agent_index].id;
        self.teams.iter().find(|t| {
            t.status == TeamStatus::Active && t.members.iter().any(|m| &m.agent_id == agent_id)
        })
    }

    pub fn agent_by_id(&self, agent_id: &str) -> Option<&Agent> {
        self.agents.iter().find(|a| a.id == agent_id)
    }

    /// Resolve an agent id to its name, falling back to the raw id.
    pub fn agent_name(&self, agent_id: &str) -> String {
        self.agent_by_id(agent_id)
            .map(|a| a.name.clone())
            .unwrap_or_else(|| agent_id.to_string())
    }

    pub fn task_by_id(&self, task_id: &str) -> Option<&Task> {
        self.tasks.iter().find(|t| t.id == task_id)
    }

    /// The peer execution behind a task, when it has one. A task is
    /// `in_progress` from the moment it is dispatched, so for remote work the
    /// phase here is the only honest thing to show.
    pub fn remote_run(&self, task_id: &str) -> Option<&RemoteRunRef> {
        self.orch_state
            .running
            .get(task_id)
            .and_then(|entry| entry.remote.as_ref())
    }

    /// Per-agent identity colour, assigned by position in the live agent list.
    pub fn agent_color(&self, agent_id: &str) -> Token {
        match self.agents.iter().position(|a| a.id == agent_id) {
            Some(index) => Palette::agent_token(index),
            None => Token::Silver,
        }
    }

    pub fn tasks_of_goal(&self, goal_id: &str) -> Vec<usize> {
        (0..self.tasks.len())
            .filter(|i| self.tasks[*i].goal_id.as_deref() == Some(goal_id))
            .collect()
    }

    // ── Selection ──────────────────────────────────────────────────────────

    pub fn selected_goal(&self) -> Option<&Goal> {
        let order = self.sorted_goals();
        order.get(self.goal_selected).map(|i| &self.goals[*i])
    }

    pub fn selected_task(&self) -> Option<&Task> {
        let visible = self.visible_tasks();
        visible.get(self.task_selected).map(|i| &self.tasks[*i])
    }

    pub fn selected_agent(&self) -> Option<&Agent> {
        let order = self.sorted_agents();
        order.get(self.agent_selected).map(|i| &self.agents[*i])
    }

    /// Index of the "show all" pseudo-row, when present.
    pub fn show_all_row_index(&self) -> Option<usize> {
        if self.hidden_task_count() > 0 {
            Some(self.visible_tasks().len())
        } else {
            None
        }
    }

    /// Index of the `+ add task...` pseudo-row, when the capability exists.
    pub fn add_task_row_index(&self, can_create: bool) -> Option<usize> {
        if !can_create {
            return None;
        }
        let base = self.visible_tasks().len();
        Some(base + usize::from(self.hidden_task_count() > 0))
    }

    pub fn task_selection_max(&self, can_create: bool) -> usize {
        let count = self.visible_tasks().len()
            + usize::from(can_create)
            + usize::from(self.hidden_task_count() > 0);
        count.saturating_sub(1)
    }

    pub fn goal_selection_max(&self, can_create: bool) -> usize {
        (self.sorted_goals().len() + usize::from(can_create)).saturating_sub(1)
    }

    pub fn agent_selection_max(&self, can_add: bool) -> usize {
        (self.sorted_agents().len() + usize::from(can_add)).saturating_sub(1)
    }

    // ── Rows (rendering + the section-header bookkeeping) ──────────────────

    pub fn task_rows(&self, can_create: bool) -> Vec<TaskRow> {
        let visible = self.visible_tasks();
        let mut rows = Vec::new();
        let mut prev_goal: Option<Option<String>> = None;

        for (selection, task_index) in visible.iter().enumerate() {
            if self.group_by_goal {
                let goal_id = self.resolved_goal_id(*task_index);
                if prev_goal.as_ref() != Some(&goal_id) {
                    match &goal_id {
                        Some(id) => {
                            let linked = self.tasks_of_goal(id);
                            let done = linked
                                .iter()
                                .filter(|i| self.tasks[**i].status == TaskStatus::Done)
                                .count();
                            if let Some(goal) = self.goal_by_id(id) {
                                rows.push(TaskRow::GoalSection {
                                    goal_id: id.clone(),
                                    title: goal.title.clone(),
                                    total: linked.len(),
                                    done,
                                });
                            }
                        }
                        None => {
                            let total = visible
                                .iter()
                                .filter(|i| self.resolved_goal_id(**i).is_none())
                                .count();
                            rows.push(TaskRow::UngroupedSection { total });
                        }
                    }
                    prev_goal = Some(goal_id);
                }
            }
            rows.push(TaskRow::Task {
                index: *task_index,
                selection,
            });
        }

        if let Some(selection) = self.show_all_row_index() {
            rows.push(TaskRow::ShowAll {
                hidden: self.hidden_task_count(),
                selection,
            });
        }
        if let Some(selection) = self.add_task_row_index(can_create) {
            rows.push(TaskRow::AddTask { selection });
        }
        rows
    }

    pub fn agent_rows(&self, can_add: bool) -> Vec<AgentRow> {
        let order = self.sorted_agents();
        let mut rows = Vec::new();
        let mut prev_team: Option<Option<String>> = None;

        for (selection, agent_index) in order.iter().enumerate() {
            let team_id = self.team_of(*agent_index).map(|t| t.id.clone());
            if prev_team.as_ref() != Some(&team_id) {
                match &team_id {
                    Some(id) => {
                        if let Some(team) = self.teams.iter().find(|t| &t.id == id) {
                            let count = order
                                .iter()
                                .filter(|i| self.team_of(**i).map(|t| &t.id) == Some(id))
                                .count();
                            let lead_name = self
                                .agent_by_id(&team.lead_agent_id)
                                .map(|a| a.name.clone());
                            rows.push(AgentRow::TeamSection {
                                team_id: id.clone(),
                                name: team.name.clone(),
                                count,
                                lead_name,
                            });
                        }
                    }
                    None => {
                        // Only once, on the transition into the unassigned block.
                        let count = order.iter().filter(|i| self.team_of(**i).is_none()).count();
                        rows.push(AgentRow::UnassignedSection { count });
                    }
                }
                prev_team = Some(team_id);
            }
            rows.push(AgentRow::Agent {
                index: *agent_index,
                selection,
            });
        }

        if can_add {
            rows.push(AgentRow::AddAgent {
                selection: order.len(),
            });
        }
        rows
    }

    /// `agentSectionRows` — team headers plus the unassigned header.
    pub fn agent_section_rows(&self) -> usize {
        let active_teams: IndexSet<String> = (0..self.agents.len())
            .filter_map(|i| self.team_of(i).map(|t| t.id.clone()))
            .collect();
        if active_teams.is_empty() {
            return 0;
        }
        let has_unassigned = (0..self.agents.len()).any(|i| self.team_of(i).is_none());
        active_teams.len() + usize::from(has_unassigned)
    }

    fn task_group_section_rows(&self) -> usize {
        if !self.group_by_goal {
            return 0;
        }
        self.task_rows(false)
            .iter()
            .filter(|r| {
                matches!(
                    r,
                    TaskRow::GoalSection { .. } | TaskRow::UngroupedSection { .. }
                )
            })
            .count()
    }

    /// `listItemCount` — feeds the layout's 50 % list cap.
    pub fn list_item_count(&self, _can_create: bool) -> usize {
        match self.view {
            ViewId::Goals => self.sorted_goals().len() + 1,
            // The welcome screen replaces the list, and the onboarding nudge
            // sits under it; both need rows of their own.
            ViewId::Tasks if self.onboarding == OnboardingStep::Welcome => 6,
            ViewId::Tasks => {
                self.visible_tasks().len()
                    + 1
                    + usize::from(self.hidden_task_count() > 0)
                    + self.task_group_section_rows()
                    + usize::from(matches!(
                        self.onboarding,
                        OnboardingStep::TaskCreated
                            | OnboardingStep::RunStarted
                            | OnboardingStep::Completed
                    ))
            }
            ViewId::Agents => self.agents.len() + 2 + self.agent_section_rows(),
            ViewId::Logs => 0,
        }
    }

    pub fn regions(&self, can_create: bool) -> crate::layout::Regions {
        crate::layout::regions(
            self.width,
            self.height,
            self.view,
            self.list_item_count(can_create),
            self.split_offset,
            self.detail_maximized,
        )
    }

    // ── Messages ───────────────────────────────────────────────────────────

    /// `addMessage` — stage a message and arm the 80 ms batch flush. The
    /// staging buffer is itself truncated to `MAX_MESSAGES`.
    pub fn add_message(&mut self, message: PendingMessage, now_ms: i64, batch_ms: i64) {
        let anchors = self.message_viewport_anchors();
        let log = self.selected_log().map(|m| m.id).or(self.log_selected_id);
        let report = crate::messages::retention::stage_message(
            &mut self.pending_messages,
            &mut self.messages,
            message,
        );
        self.record_retention(report);
        self.reconcile_message_selection(log);
        self.restore_message_viewports(anchors);
        if batch_ms <= 0 {
            self.flush_messages(now_ms);
        } else if self.message_flush_at.is_none() {
            self.message_flush_at = Some(now_ms.saturating_add(batch_ms));
        }
    }
    pub fn flush_messages(&mut self, now_ms: i64) {
        self.message_flush_at = None;
        let anchors = self.message_viewport_anchors();
        let log = self.selected_log().map(|m| m.id).or(self.log_selected_id);
        let report = crate::messages::retention::flush_messages(
            &mut self.pending_messages,
            &mut self.messages,
            &mut self.next_message_id,
            now_ms,
        );
        self.record_retention(report);
        self.reconcile_message_selection(log);
        self.restore_message_viewports(anchors);
    }
    pub fn prepend_messages(&mut self, messages: Vec<PendingMessage>, now_ms: i64) {
        let anchors = self.message_viewport_anchors();
        let log = self.selected_log().map(|m| m.id).or(self.log_selected_id);
        let report = crate::messages::retention::prepend_messages(
            &mut self.pending_messages,
            &mut self.messages,
            messages,
            &mut self.next_message_id,
            now_ms,
        );
        self.record_retention(report);
        self.reconcile_message_selection(log);
        self.restore_message_viewports(anchors);
    }
    fn message_viewport_anchors(
        &self,
    ) -> (
        Option<u64>,
        Option<u64>,
        Option<crate::follow::TaskActivityAnchor>,
    ) {
        let logs = self.filtered_messages();
        let activity = self.activity_messages();
        (
            if self.log_selected_index >= 0 {
                logs.get(self.log_scroll_offset)
                    .map(|i| self.messages[*i].id)
            } else {
                None
            },
            if !self.activity_follow_tail {
                activity
                    .get(self.activity_scroll)
                    .map(|i| self.messages[*i].id)
            } else {
                None
            },
            self.task_activity_anchor(),
        )
    }
    fn restore_message_viewports(
        &mut self,
        (log, activity, task): (
            Option<u64>,
            Option<u64>,
            Option<crate::follow::TaskActivityAnchor>,
        ),
    ) {
        self.restore_task_activity_anchor(task);
        if let Some(id) = log {
            self.log_scroll_offset = self
                .filtered_messages()
                .iter()
                .position(|i| self.messages[*i].id == id)
                .unwrap_or(0);
        }
        if let Some(id) = activity {
            self.activity_scroll = self
                .activity_messages()
                .iter()
                .position(|i| self.messages[*i].id == id)
                .unwrap_or(0);
        }
    }
    fn record_retention(&mut self, report: crate::messages::retention::RetentionReport) {
        self.retained_evicted = self.retained_evicted.saturating_add(report.evicted);
        self.retained_rejected = self.retained_rejected.saturating_add(report.rejected);
        self.retained_bytes = report.owned_bytes;
    }
    fn reconcile_message_selection(&mut self, log: Option<u64>) {
        if self.log_selected_index >= 0 {
            let rows = self.filtered_messages();
            self.log_selected_index = log
                .and_then(|id| rows.iter().position(|i| self.messages[*i].id == id))
                .unwrap_or((self.log_selected_index as usize).min(rows.len().saturating_sub(1)))
                as i64;
            self.log_selected_id = self.selected_log().map(|m| m.id);
        }
        let rows = self.activity_messages();
        let position = if self.activity_follow_tail {
            rows.len().checked_sub(1)
        } else {
            rows.iter()
                .position(|i| Some(self.messages[*i].id) == self.activity_selected)
                .or_else(|| {
                    (!rows.is_empty()).then_some(
                        self.activity_last_position
                            .min(rows.len().saturating_sub(1)),
                    )
                })
        };
        self.activity_last_position = position.unwrap_or(0);
        self.activity_selected = position
            .and_then(|p| rows.get(p))
            .map(|i| self.messages[*i].id);
    }

    /// Logs-view filtering: the agent filter never drops a message without an
    /// `agentId`; the type filter always applies.
    pub fn filtered_messages(&self) -> Vec<usize> {
        (0..self.messages.len())
            .filter(|i| {
                let message = &self.messages[*i];
                if !self.log_agent_filter.is_empty()
                    && let Some(agent_id) = &message.agent_id
                    && !self.log_agent_filter.contains(agent_id)
                {
                    return false;
                }
                self.log_type_filter.contains(&message.msg_type)
            })
            .collect()
    }

    /// The message indices the logs view shows, top row first, for a rows
    /// area `view_h` terminal rows tall.
    ///
    /// Row `r` of that area is index `visible_log_indices(view_h)[r]`. The
    /// window is picked two different ways — tail mode
    /// (`log_selected_index < 0`) pins it to the end of the *filtered* list,
    /// a selection scrolls it by `log_scroll_offset` — and both of them index
    /// into `filtered_messages`, not into `messages`. Confusing the two is
    /// the bug PARITY.md §14.20 records, so this is the only implementation:
    /// `views/logs.rs` draws from it, and anything that has to turn a row
    /// back into a message reads the same vector.
    pub fn visible_log_indices(&self, view_h: usize) -> Vec<usize> {
        let filtered = self.filtered_messages();
        if self.log_selected_index < 0 {
            let skip = filtered.len().saturating_sub(view_h);
            filtered.into_iter().skip(skip).collect()
        } else {
            filtered
                .into_iter()
                .skip(self.log_scroll_offset)
                .take(view_h)
                .collect()
        }
    }

    /// Activity-feed filtering; skipped entirely when the set is full-size.
    pub fn activity_messages(&self) -> Vec<usize> {
        if self.activity_filter.len() >= MsgType::ALL.len() {
            return (0..self.messages.len()).collect();
        }
        (0..self.messages.len())
            .filter(|i| self.activity_filter.contains(&self.messages[*i].msg_type))
            .collect()
    }

    // ── Soft delete ────────────────────────────────────────────────────────

    /// `scheduleDeletion` — returns the banner message to emit.
    pub fn schedule_deletion(
        &mut self,
        entity_type: EntityType,
        entity_id: &str,
        entity_name: &str,
        needs_force_stop: bool,
        now_ms: i64,
    ) -> String {
        self.next_deletion_key += 1;
        self.pending_deletions.push(PendingDeletion {
            key: self.next_deletion_key,
            entity_type,
            entity_id: entity_id.to_string(),
            entity_name: entity_name.to_string(),
            expires_at: now_ms + UNDO_TIMEOUT_MS,
            needs_force_stop,
        });
        format!("✗ \"{entity_name}\" will be deleted in 5s — press Z to undo")
    }

    /// `Z` — pop the most recent pending deletion (LIFO).
    pub fn undo_deletion(&mut self) -> Option<PendingDeletion> {
        self.undo_deletion_at(self.pending_deletions.len().checked_sub(1)?)
    }

    /// [`Self::undo_deletion`] for one *named* pending deletion.
    ///
    /// The undo banner renders one row per pending deletion in index order
    /// (`widgets/undo_banner.rs`), so a caller that points at a row — rather
    /// than pressing `Z` — has to address that row and not the last one.
    /// Restoring row 0 of three with the LIFO form would restore row 2.
    ///
    /// The remaining deletions keep their order, so the rows that are still
    /// on screen keep meaning what they meant. Out of range is a no-op.
    pub fn undo_deletion_at(&mut self, index: usize) -> Option<PendingDeletion> {
        (index < self.pending_deletions.len()).then(|| self.pending_deletions.remove(index))
    }

    /// Collect every expired deletion, atomically.
    pub fn take_expired_deletions(&mut self, now_ms: i64) -> Vec<PendingDeletion> {
        let mut expired = Vec::new();
        self.pending_deletions.retain(|d| {
            if d.expires_at <= now_ms {
                expired.push(d.clone());
                false
            } else {
                true
            }
        });
        expired
    }

    // ── Toasts ─────────────────────────────────────────────────────────────

    /// Queue a toast (cap 5, oldest dropped).
    pub fn push_toast(&mut self, kind: ToastKind, title: &str, message: &str, now_ms: i64) {
        let id = self.next_toast_id;
        self.next_toast_id += 1;
        self.toasts.push(Toast {
            id,
            kind,
            title: title.to_string(),
            message: message.to_string(),
            created_at: now_ms,
        });
        if self.toasts.len() > MAX_TOASTS {
            let drop = self.toasts.len() - MAX_TOASTS;
            self.toasts.drain(..drop);
        }
    }

    /// The **first** two of the queue, not the newest.
    pub fn visible_toasts(&self) -> &[Toast] {
        let end = self.toasts.len().min(crate::messages::TOAST_MAX_VISIBLE);
        &self.toasts[..end]
    }

    pub fn expire_toasts(&mut self, now_ms: i64) {
        self.toasts.retain(|t| !t.is_expired(now_ms));
    }

    // ── Split / detail ─────────────────────────────────────────────────────

    pub fn detail_open(&self) -> bool {
        self.detail != DetailPanel::None
    }

    /// `+` / `=` — grow the detail panel.
    pub fn grow_detail(&mut self) {
        let content_h = crate::layout::content_height(self.height);
        if self.detail_maximized {
            self.detail_maximized = false;
            self.split_offset = -((content_h as i32) / 2);
        } else {
            self.split_offset = crate::layout::clamp_split_offset(
                self.split_offset - crate::layout::SPLIT_STEP,
                content_h,
            );
        }
    }

    /// `-` — grow the list.
    pub fn grow_list(&mut self) {
        let content_h = crate::layout::content_height(self.height);
        if self.detail_maximized {
            self.detail_maximized = false;
            self.split_offset = (content_h as i32) / 2;
        } else {
            self.split_offset = crate::layout::clamp_split_offset(
                self.split_offset + crate::layout::SPLIT_STEP,
                content_h,
            );
        }
    }

    /// Re-clamp selections and scroll offsets after the data or size changed.
    pub fn clamp_all(&mut self, can_create: bool) {
        self.goal_selected = self.goal_selected.min(self.goal_selection_max(can_create));
        self.task_selected = self.task_selected.min(self.task_selection_max(can_create));
        self.agent_selected = self
            .agent_selected
            .min(self.agent_selection_max(can_create));
        self.split_offset = crate::layout::clamp_split_offset(
            self.split_offset,
            crate::layout::content_height(self.height),
        );
        let regions = self.regions(can_create);
        let goal_rows: Vec<_> = (0..self.sorted_goals().len() + usize::from(can_create)).collect();
        let task_rows: Vec<_> = self
            .task_rows(can_create)
            .iter()
            .enumerate()
            .filter_map(|(i, r)| {
                matches!(
                    r,
                    TaskRow::Task { .. } | TaskRow::ShowAll { .. } | TaskRow::AddTask { .. }
                )
                .then_some(i)
            })
            .collect();
        let agent_rows: Vec<_> = self
            .agent_rows(can_create)
            .iter()
            .enumerate()
            .filter_map(|(i, r)| {
                matches!(r, AgentRow::Agent { .. } | AgentRow::AddAgent { .. }).then_some(i)
            })
            .collect();
        fn reveal(selected: usize, offset: usize, rows: &[usize], total: usize, h: usize) -> usize {
            let mut offset = offset.min(total.saturating_sub(h));
            if h > 0
                && let Some(row) = rows.get(selected)
            {
                if *row < offset {
                    offset = *row;
                } else if *row >= offset.saturating_add(h) {
                    offset = row.saturating_add(1).saturating_sub(h);
                }
            }
            offset
        }
        let viewport = |view: ViewId, chrome: u16| {
            self.mouse
                .hits
                .metrics
                .panes
                .iter()
                .find(|m| m.key.view == view && m.key.section == Focus::MainList)
                .map_or(regions.main_h.saturating_sub(chrome) as usize, |m| {
                    m.body.height as usize
                })
        };
        self.goal_scroll = reveal(
            self.goal_selected,
            self.goal_scroll,
            &goal_rows,
            goal_rows.len(),
            viewport(ViewId::Goals, 0),
        );
        self.task_scroll = reveal(
            self.task_selected,
            self.task_scroll,
            &task_rows,
            self.task_rows(can_create).len(),
            viewport(ViewId::Tasks, 0),
        );
        self.agent_scroll = reveal(
            self.agent_selected,
            self.agent_scroll,
            &agent_rows,
            self.agent_rows(can_create).len(),
            viewport(ViewId::Agents, 1),
        );
        self.reconcile_message_selection(self.log_selected_id);
        self.normalize_focus();
        self.mouse.hits.invalidate();
        self.mouse.armed = None;
        self.mouse.last_click = None;
    }

    // ── Status helpers used by the header and `/status` ────────────────────

    pub fn running_count(&self) -> usize {
        self.orch_state.running.len()
    }

    pub fn mode_label(&self) -> &'static str {
        if self.watch_active {
            "watching"
        } else if self.observer_mode {
            "observing"
        } else {
            "idle"
        }
    }
}

#[cfg(test)]
// Fixtures build an `AppState` field by field; struct-update syntax would bury
// what each test is actually setting.
#[allow(clippy::field_reassign_with_default)]
mod tests {
    use super::*;
    use crate::backend::{make_agent, make_goal, make_task, make_team};
    use crate::messages::MAX_MESSAGES;
    use orch_domain::GoalStatus;

    fn state_with_tasks(specs: &[(&str, TaskStatus)]) -> AppState {
        let mut state = AppState::default();
        state.tasks = specs
            .iter()
            .map(|(id, status)| make_task(id, id, *status))
            .collect();
        state
    }

    #[test]
    fn tab_order_and_cycling_wraps() {
        assert_eq!(
            ViewId::ALL,
            [ViewId::Goals, ViewId::Tasks, ViewId::Agents, ViewId::Logs]
        );
        assert_eq!(ViewId::Tasks.next(), ViewId::Agents);
        assert_eq!(ViewId::Agents.next(), ViewId::Logs);
        assert_eq!(ViewId::Logs.next(), ViewId::Goals);
        assert_eq!(ViewId::Goals.next(), ViewId::Tasks);
        assert_eq!(ViewId::Tasks.prev(), ViewId::Goals);
        assert_eq!(ViewId::Goals.prev(), ViewId::Logs);
    }

    #[test]
    fn the_logs_tab_renders_as_actions() {
        assert_eq!(ViewId::Logs.label(), "ACTIONS");
        assert_eq!(ViewId::Logs.item_label(), "events");
    }

    #[test]
    fn tasks_sort_by_status_rank() {
        let state = state_with_tasks(&[
            ("done", TaskStatus::Done),
            ("todo", TaskStatus::Todo),
            ("run", TaskStatus::InProgress),
            ("fail", TaskStatus::Failed),
            ("review", TaskStatus::Review),
            ("retry", TaskStatus::Retrying),
            ("cancel", TaskStatus::Cancelled),
        ]);
        let ids: Vec<&str> = state
            .sorted_tasks()
            .iter()
            .map(|i| state.tasks[*i].id.as_str())
            .collect();
        assert_eq!(
            ids,
            ["run", "retry", "review", "todo", "done", "fail", "cancel"]
        );
    }

    #[test]
    fn goals_sort_by_status_rank() {
        let mut state = AppState::default();
        state.goals = vec![
            make_goal("g_abandoned", "A", GoalStatus::Abandoned),
            make_goal("g_active", "B", GoalStatus::Active),
            make_goal("g_achieved", "C", GoalStatus::Achieved),
            make_goal("g_paused", "D", GoalStatus::Paused),
        ];
        let ids: Vec<&str> = state
            .sorted_goals()
            .iter()
            .map(|i| state.goals[*i].id.as_str())
            .collect();
        assert_eq!(ids, ["g_active", "g_paused", "g_achieved", "g_abandoned"]);
    }

    #[test]
    fn agents_sort_by_team_then_status() {
        let mut state = AppState::default();
        state.agents = vec![
            make_agent("a_solo", "solo", AgentStatus::Running),
            make_agent("a_beta", "beta", AgentStatus::Idle),
            make_agent("a_alpha", "alpha", AgentStatus::Error),
            make_agent("a_alpha2", "alpha2", AgentStatus::Running),
        ];
        state.teams = vec![
            make_team("t_b", "beta-team", "a_beta", &["a_beta"]),
            make_team("t_a", "alpha-team", "a_alpha", &["a_alpha", "a_alpha2"]),
        ];
        let ids: Vec<&str> = state
            .sorted_agents()
            .iter()
            .map(|i| state.agents[*i].id.as_str())
            .collect();
        // alpha-team (running before error), then beta-team, then unassigned.
        assert_eq!(ids, ["a_alpha2", "a_alpha", "a_beta", "a_solo"]);
    }

    #[test]
    fn the_ten_task_limit_and_hidden_count() {
        let specs: Vec<(String, TaskStatus)> = (0..11)
            .map(|i| (format!("tsk_{i}"), TaskStatus::Todo))
            .collect();
        let mut state = AppState::default();
        state.tasks = specs
            .iter()
            .map(|(id, status)| make_task(id, id, *status))
            .collect();
        assert_eq!(state.visible_tasks().len(), 10);
        assert_eq!(state.hidden_task_count(), 1);
        assert!(state.show_all_row_index().is_some());
        state.show_all_tasks = true;
        assert_eq!(state.visible_tasks().len(), 11);
        assert_eq!(state.hidden_task_count(), 0);
        assert!(state.show_all_row_index().is_none());
    }

    #[test]
    fn task_selection_space_covers_pseudo_rows() {
        let specs: Vec<(String, TaskStatus)> = (0..11)
            .map(|i| (format!("tsk_{i}"), TaskStatus::Todo))
            .collect();
        let mut state = AppState::default();
        state.tasks = specs
            .iter()
            .map(|(id, status)| make_task(id, id, *status))
            .collect();
        // 10 tasks + show-all + add-task.
        assert_eq!(state.task_selection_max(true), 11);
        assert_eq!(state.show_all_row_index(), Some(10));
        assert_eq!(state.add_task_row_index(true), Some(11));
        assert_eq!(state.add_task_row_index(false), None);
        // Without hidden tasks the add row moves up.
        state.tasks.truncate(3);
        assert_eq!(state.add_task_row_index(true), Some(3));
        assert_eq!(state.task_selection_max(true), 3);
    }

    #[test]
    fn group_by_goal_reorders_and_inserts_headers_without_consuming_selection() {
        let mut state = AppState::default();
        state.goals = vec![make_goal("goal_1", "Ship it", GoalStatus::Active)];
        let mut linked = make_task("tsk_1", "linked", TaskStatus::Todo);
        linked.goal_id = Some("goal_1".into());
        let mut done_linked = make_task("tsk_2", "done linked", TaskStatus::Done);
        done_linked.goal_id = Some("goal_1".into());
        let orphan = make_task("tsk_3", "orphan", TaskStatus::Todo);
        let mut dangling = make_task("tsk_4", "dangling", TaskStatus::Todo);
        dangling.goal_id = Some("goal_gone".into());
        state.tasks = vec![done_linked, orphan, linked, dangling];
        state.group_by_goal = true;

        let rows = state.task_rows(true);
        let sections: Vec<&TaskRow> = rows
            .iter()
            .filter(|r| {
                matches!(
                    r,
                    TaskRow::GoalSection { .. } | TaskRow::UngroupedSection { .. }
                )
            })
            .collect();
        assert_eq!(sections.len(), 2);
        match sections[0] {
            TaskRow::GoalSection {
                title, total, done, ..
            } => {
                assert_eq!(title, "Ship it");
                assert_eq!(*total, 2);
                assert_eq!(*done, 1);
            }
            other => panic!("expected a goal section, got {other:?}"),
        }
        assert!(matches!(
            sections[1],
            TaskRow::UngroupedSection { total: 2 }
        ));

        // Selection indices are contiguous across the headers.
        let selections: Vec<usize> = rows
            .iter()
            .filter_map(|r| match r {
                TaskRow::Task { selection, .. } => Some(*selection),
                _ => None,
            })
            .collect();
        assert_eq!(selections, [0, 1, 2, 3]);
    }

    #[test]
    fn an_orphaned_goal_id_resolves_to_none() {
        let mut state = AppState::default();
        let mut task = make_task("tsk_1", "t", TaskStatus::Todo);
        task.goal_id = Some("goal_gone".into());
        state.tasks = vec![task];
        assert_eq!(state.resolved_goal_id(0), None);
    }

    #[test]
    fn agent_rows_carry_team_and_unassigned_headers() {
        let mut state = AppState::default();
        state.agents = vec![
            make_agent("a1", "alpha", AgentStatus::Running),
            make_agent("a2", "solo", AgentStatus::Idle),
        ];
        state.teams = vec![make_team("t1", "core", "a1", &["a1"])];
        let rows = state.agent_rows(true);
        assert!(
            matches!(rows[0], AgentRow::TeamSection { ref name, count: 1, .. } if name == "core")
        );
        assert!(matches!(rows[1], AgentRow::Agent { selection: 0, .. }));
        assert!(matches!(rows[2], AgentRow::UnassignedSection { count: 1 }));
        assert!(matches!(rows[3], AgentRow::Agent { selection: 1, .. }));
        assert!(matches!(rows[4], AgentRow::AddAgent { selection: 2 }));
        assert_eq!(state.agent_section_rows(), 2);
    }

    #[test]
    fn disbanded_teams_do_not_group_agents() {
        let mut state = AppState::default();
        state.agents = vec![make_agent("a1", "alpha", AgentStatus::Idle)];
        let mut team = make_team("t1", "core", "a1", &["a1"]);
        team.status = TeamStatus::Disbanded;
        state.teams = vec![team];
        assert!(state.team_of(0).is_none());
        assert_eq!(state.agent_section_rows(), 0);
    }

    #[test]
    fn message_batching_holds_until_the_flush() {
        let mut state = AppState::default();
        for i in 0..100 {
            state.add_message(
                PendingMessage {
                    text: format!("m{i}"),
                    color: Token::White,
                    msg_type: MsgType::Output,
                    agent_id: None,
                    task_id: None,
                    detail: None,
                    ts_ms: None,
                },
                0,
                crate::messages::MESSAGE_BATCH_MS,
            );
        }
        assert!(
            state.messages.is_empty(),
            "nothing visible before the flush"
        );
        assert_eq!(state.message_flush_at, Some(80));
        state.flush_messages(80);
        assert_eq!(state.messages.len(), 100);
        assert_eq!(state.message_flush_at, None);
    }

    #[test]
    fn immediate_mode_flushes_synchronously() {
        let mut state = AppState::default();
        state.add_message(
            PendingMessage {
                text: "now".into(),
                color: Token::White,
                msg_type: MsgType::Output,
                agent_id: None,
                task_id: None,
                detail: None,
                ts_ms: None,
            },
            0,
            0,
        );
        assert_eq!(state.messages.len(), 1);
    }

    #[test]
    fn the_feed_is_capped_at_500_with_the_oldest_evicted() {
        let mut state = AppState::default();
        for i in 0..600 {
            state.add_message(
                PendingMessage {
                    text: format!("m{i}"),
                    color: Token::White,
                    msg_type: MsgType::Output,
                    agent_id: None,
                    task_id: None,
                    detail: None,
                    ts_ms: None,
                },
                0,
                0,
            );
        }
        assert_eq!(state.messages.len(), MAX_MESSAGES);
        assert_eq!(state.messages[0].text, "m100");
        assert_eq!(state.messages[MAX_MESSAGES - 1].text, "m599");
    }

    #[test]
    fn history_batches_are_prepended_and_accumulate() {
        let mut state = AppState::default();
        state.add_message(
            PendingMessage {
                text: "live".into(),
                color: Token::White,
                msg_type: MsgType::Output,
                agent_id: None,
                task_id: None,
                detail: None,
                ts_ms: None,
            },
            0,
            0,
        );
        state.prepend_messages(
            vec![PendingMessage {
                text: "old".into(),
                color: Token::White,
                msg_type: MsgType::Output,
                agent_id: None,
                task_id: None,
                detail: None,
                ts_ms: None,
            }],
            0,
        );
        assert_eq!(state.messages[0].text, "old");
        assert_eq!(state.messages[1].text, "live");
    }

    #[test]
    fn the_agent_filter_never_drops_messages_without_an_agent() {
        let mut state = AppState::default();
        state.messages = vec![
            Message::new(1, 0, "with", Token::White, MsgType::Output)
                .with_agent(Some("agt_1".into())),
            Message::new(2, 0, "other", Token::White, MsgType::Output)
                .with_agent(Some("agt_2".into())),
            Message::new(3, 0, "systemic", Token::White, MsgType::Output),
        ];
        state.log_agent_filter = ["agt_1".to_string()].into_iter().collect();
        let kept: Vec<&str> = state
            .filtered_messages()
            .iter()
            .map(|i| state.messages[*i].text.as_str())
            .collect();
        assert_eq!(kept, ["with", "systemic"]);
    }

    #[test]
    fn the_type_filter_drops_unselected_types() {
        let mut state = AppState::default();
        state.messages = vec![
            Message::new(1, 0, "out", Token::White, MsgType::Output),
            Message::new(2, 0, "err", Token::Red, MsgType::Error),
        ];
        state.log_type_filter = preset_types("errors");
        let kept: Vec<&str> = state
            .filtered_messages()
            .iter()
            .map(|i| state.messages[*i].text.as_str())
            .collect();
        assert_eq!(kept, ["err"]);
    }

    #[test]
    fn the_activity_filter_is_skipped_when_full_size() {
        let mut state = AppState::default();
        state.messages = vec![Message::new(1, 0, "x", Token::White, MsgType::Output)];
        assert_eq!(state.activity_messages().len(), 1);
        state.activity_filter = preset_types("errors");
        assert_eq!(state.activity_messages().len(), 0);
    }

    #[test]
    fn deletions_are_lifo_and_coexist() {
        let mut state = AppState::default();
        let msg = state.schedule_deletion(EntityType::Task, "tsk_1", "First", false, 0);
        assert_eq!(msg, "✗ \"First\" will be deleted in 5s — press Z to undo");
        state.schedule_deletion(EntityType::Agent, "agt_1", "Second", true, 0);
        assert_eq!(state.pending_deletions.len(), 2);
        let undone = state.undo_deletion().unwrap();
        assert_eq!(undone.entity_name, "Second");
        assert!(undone.needs_force_stop);
        assert_eq!(state.pending_deletions.len(), 1);
    }

    #[test]
    fn undo_deletion_at_addresses_the_banner_row_and_lifo_still_takes_the_last() {
        let mut state = AppState::default();
        for (id, name) in [("tsk_1", "First"), ("tsk_2", "Second"), ("tsk_3", "Third")] {
            state.schedule_deletion(EntityType::Task, id, name, false, 0);
        }
        // Row 0 is the *first* deletion, which is what `undo_deletion` would
        // have got wrong by restoring "Third".
        let undone = state.undo_deletion_at(0).unwrap();
        assert_eq!(undone.entity_name, "First");
        // The rows that are still on screen keep their order.
        let left: Vec<&str> = state
            .pending_deletions
            .iter()
            .map(|d| d.entity_name.as_str())
            .collect();
        assert_eq!(left, ["Second", "Third"]);
        // `Z` is unchanged: still the most recent one.
        assert_eq!(state.undo_deletion().unwrap().entity_name, "Third");
        assert_eq!(state.undo_deletion_at(1), None);
        assert_eq!(state.undo_deletion_at(0).unwrap().entity_name, "Second");
        assert_eq!(state.undo_deletion(), None);
        assert_eq!(state.undo_deletion_at(0), None);
    }

    #[test]
    fn deletions_expire_after_the_five_second_window() {
        let mut state = AppState::default();
        state.schedule_deletion(EntityType::Task, "tsk_1", "First", false, 1_000);
        assert!(state.take_expired_deletions(5_999).is_empty());
        let expired = state.take_expired_deletions(6_000);
        assert_eq!(expired.len(), 1);
        assert!(state.pending_deletions.is_empty());
    }

    #[test]
    fn toasts_queue_five_and_render_the_first_two() {
        let mut state = AppState::default();
        for i in 0..10 {
            state.push_toast(ToastKind::Done, &format!("t{i}"), "done", 0);
        }
        assert_eq!(state.toasts.len(), MAX_TOASTS);
        // Ten events keep tasks 5..9 and render 5 and 6.
        assert_eq!(state.toasts[0].title, "t5");
        let visible: Vec<&str> = state
            .visible_toasts()
            .iter()
            .map(|t| t.title.as_str())
            .collect();
        assert_eq!(visible, ["t5", "t6"]);
    }

    #[test]
    fn toast_kinds_carry_their_icon_message_delay_and_bell() {
        assert_eq!(ToastKind::Done.icon(), "✓");
        assert_eq!(ToastKind::Failed.icon(), "✕");
        assert_eq!(ToastKind::Review.icon(), "◈");
        assert_eq!(ToastKind::Done.delay_ms(), 4_000);
        assert_eq!(ToastKind::Failed.delay_ms(), 8_000);
        assert_eq!(ToastKind::Review.delay_ms(), 6_000);
        assert!(!ToastKind::Done.rings_bell());
        assert!(ToastKind::Failed.rings_bell());
        assert!(ToastKind::Review.rings_bell());
        assert_eq!(
            ToastKind::Done.message(Some("alpha")),
            "Task completed by alpha"
        );
        assert_eq!(ToastKind::Done.message(None), "Task completed by Agent");
        assert_eq!(
            ToastKind::Failed.message(None),
            "Task failed — press Enter for details"
        );
        assert_eq!(
            ToastKind::Review.message(None),
            "Task ready for review — press A to approve"
        );
        assert_eq!(ToastKind::from_status(TaskStatus::Todo), None);
    }

    #[test]
    fn toasts_fade_at_both_ends_and_then_expire() {
        let mut state = AppState::default();
        state.push_toast(ToastKind::Done, "t", "m", 1_000);
        let toast = state.toasts[0].clone();
        assert!(toast.is_dimmed(1_100));
        assert!(!toast.is_dimmed(2_000));
        assert!(toast.is_dimmed(5_000));
        assert!(!toast.is_expired(5_000));
        assert!(toast.is_expired(5_360));
        state.expire_toasts(5_360);
        assert!(state.toasts.is_empty());
    }

    #[test]
    fn the_tab_flash_blinks_three_times_over_twelve_ticks() {
        let flash = TabFlash {
            tab: ViewId::Tasks,
            color: Token::Green,
            started_tick: 0,
        };
        assert!(flash.is_bright(0));
        assert!(flash.is_bright(1));
        assert!(!flash.is_bright(2));
        assert!(!flash.is_bright(3));
        assert!(flash.is_bright(4));
        assert!(!flash.is_done(11));
        assert!(flash.is_done(12));
    }

    #[test]
    fn resize_keys_interact_with_the_maximized_state() {
        let mut state = AppState::default();
        state.height = 30;
        state.detail_maximized = true;
        state.grow_detail();
        assert!(!state.detail_maximized);
        assert_eq!(state.split_offset, -(21 / 2));

        state.detail_maximized = true;
        state.grow_list();
        assert!(!state.detail_maximized);
        assert_eq!(state.split_offset, 21 / 2);
    }

    #[test]
    fn mode_label_tracks_watch_and_observer() {
        let mut state = AppState::default();
        assert_eq!(state.mode_label(), "idle");
        state.observer_mode = true;
        assert_eq!(state.mode_label(), "observing");
        state.watch_active = true;
        assert_eq!(state.mode_label(), "watching");
    }

    #[test]
    fn runnable_statuses_match_the_contract() {
        assert!(is_runnable(TaskStatus::Todo));
        assert!(is_runnable(TaskStatus::Failed));
        assert!(is_runnable(TaskStatus::Cancelled));
        assert!(!is_runnable(TaskStatus::InProgress));
        assert!(!is_runnable(TaskStatus::Review));
        assert!(!is_runnable(TaskStatus::Done));
        assert!(!is_runnable(TaskStatus::Retrying));
    }
}

#[cfg(test)]
mod copy_foundation_tests {
    use super::*;
    #[test]
    fn exhausted_generation_retires_live_guards_and_does_not_reuse_ids() {
        let mut state = AppState::default();
        let origin = state
            .register_viewer_origin(ViewerSource::Message(1))
            .unwrap();
        state.suppress_text_footer(&origin, 0);
        state.next_copy_generation = u64::MAX;
        assert!(
            state
                .register_viewer_origin(ViewerSource::Message(1))
                .is_err()
        );
        assert!(!state.copy_origin_is_live(&origin));
        assert!(!state.text_footer_suppressed(&origin, 1));
        assert!(
            state
                .register_viewer_origin(ViewerSource::Message(1))
                .is_err()
        );
    }
    #[test]
    fn exhausted_request_sequence_clears_presentation_and_drops_old_completions() {
        let mut state = AppState::default();
        let origin = state
            .register_viewer_origin(ViewerSource::Message(1))
            .unwrap();
        state.register_copy_request(origin.clone(), CopyTarget::Auto, Ok(CopyTicket(1)), 0);
        state.copy_feedback.request_seq = u64::MAX;
        state.register_copy_request(origin.clone(), CopyTarget::Auto, Err("Busy".into()), 1);
        state.apply_copy_result(
            CopyResult {
                ticket: CopyTicket(1),
                outcome: CopyOutcome::NativeWritten,
                guidance: None,
            },
            2,
        );
        assert!(state.feedback_for(&origin).is_none());
        assert!(!state.copy_footer_visible(&origin));
        assert_eq!(state.copy_feedback.pending_count(), 0);
        assert_eq!(state.copy_feedback.contract_errors, 1);
    }
}
