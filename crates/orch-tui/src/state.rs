//! All dashboard state and the arithmetic over it (`src/tui/App.tsx`).
//!
//! Pure and synchronous: no backend calls, no ratatui. [`crate::app::App`] owns
//! an `AppState` plus a backend and a clock; [`crate::keys`] mutates it.

use indexmap::IndexSet;
use orch_domain::{
    Agent, AgentStatus, Goal, NotificationPreferences, OrchestratorState, Task, TaskStatus, Team,
    TeamStatus, default_state, goal_status_order,
};

use crate::command_bar::CommandHistory;
use crate::hit::{HitMap, ZoneId, ZoneKind};
use crate::messages::{
    MAX_MESSAGES, MAX_TOASTS, Message, MsgType, PendingMessage, RunMaps, TASK_LIST_LIMIT,
    UNDO_TIMEOUT_MS, preset_types,
};
use crate::onboarding::OnboardingStep;
use crate::text::TextInput;
use crate::theme::{Palette, Token};
use crate::wizard::Wizard;

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
    pub view: ViewId,
    pub goal_selected: usize,
    pub task_selected: usize,
    pub agent_selected: usize,
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
            width: 80,
            height: 24,
            tasks: Vec::new(),
            agents: Vec::new(),
            peer_status: "listen off · controller none".into(),
            goals: Vec::new(),
            teams: Vec::new(),
            orch_state: default_state(),
            view: ViewId::Tasks,
            goal_selected: 0,
            task_selected: 0,
            agent_selected: 0,
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

    /// `sortedAgents` — active-team members first, then team name, then status.
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
                    agent_status_order(self.agents[*a].status)
                        .cmp(&agent_status_order(self.agents[*b].status))
                })
        });
        order
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
            ViewId::Agents => self.agents.len() + 1 + self.agent_section_rows(),
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
        self.pending_messages.push(message);
        if self.pending_messages.len() > MAX_MESSAGES {
            let overflow = self.pending_messages.len() - MAX_MESSAGES;
            self.pending_messages.drain(..overflow);
        }
        if batch_ms <= 0 {
            self.flush_messages(now_ms);
        } else if self.message_flush_at.is_none() {
            self.message_flush_at = Some(now_ms + batch_ms);
        }
    }

    /// Drain the staging buffer into the feed, honouring the 500-entry cap.
    pub fn flush_messages(&mut self, now_ms: i64) {
        self.message_flush_at = None;
        if self.pending_messages.is_empty() {
            return;
        }
        let pending = std::mem::take(&mut self.pending_messages);
        let mut batch: Vec<Message> = Vec::with_capacity(pending.len());
        for p in pending {
            // A line identical to the latest one from the same source adds
            // nothing: keep the first, whose timestamp then reads as how long
            // the state has lasted (PARITY.md §12.8 (k)).
            if repeats_latest(&self.messages, &batch, &p) {
                continue;
            }
            let id = self.next_message_id;
            self.next_message_id += 1;
            // A message with its own instant (a history entry) keeps it;
            // everything else is stamped with the flush time.
            batch.push(
                Message::new(id, p.ts_ms.unwrap_or(now_ms), &p.text, p.color, p.msg_type)
                    .with_agent(p.agent_id)
                    .with_task(p.task_id)
                    .with_detail(p.detail),
            );
        }
        if batch.is_empty() {
            return;
        }

        if batch.len() >= MAX_MESSAGES {
            let start = batch.len() - MAX_MESSAGES;
            self.messages = batch[start..].to_vec();
        } else {
            let keep = MAX_MESSAGES - batch.len();
            if self.messages.len() > keep {
                let drop = self.messages.len() - keep;
                self.messages.drain(..drop);
            }
            self.messages.extend(batch);
        }
    }

    /// `onLoadHistory` — history batches are **prepended**, then truncated.
    pub fn prepend_messages(&mut self, messages: Vec<PendingMessage>, now_ms: i64) {
        let mut prefix: Vec<Message> = messages
            .into_iter()
            .map(|p| {
                let id = self.next_message_id;
                self.next_message_id += 1;
                // A message with its own instant (a history entry) keeps it;
                // everything else is stamped with the flush time.
                Message::new(id, p.ts_ms.unwrap_or(now_ms), &p.text, p.color, p.msg_type)
                    .with_agent(p.agent_id)
                    .with_task(p.task_id)
                    .with_detail(p.detail)
            })
            .collect();
        prefix.extend(std::mem::take(&mut self.messages));
        if prefix.len() > MAX_MESSAGES {
            let drop = prefix.len() - MAX_MESSAGES;
            prefix.drain(..drop);
        }
        self.messages = prefix;
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
        let regions = self.regions(can_create);
        let main_h = regions.main_h as usize;
        let clamp = |len: usize| len.saturating_sub(main_h);
        self.goal_scroll = self.goal_scroll.min(clamp(self.sorted_goals().len()));
        self.task_scroll = self.task_scroll.min(clamp(self.visible_tasks().len()));
        self.agent_scroll = self.agent_scroll.min(clamp(self.sorted_agents().len()));
        self.split_offset = crate::layout::clamp_split_offset(self.split_offset, regions.content_h);
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

/// How far back [`repeats_latest`] looks for the previous line from the same
/// source. A source silent for longer than this simply gets a fresh line.
const REPEAT_LOOKBACK: usize = 64;

/// Whether `p` repeats the most recent feed line from the same source — the
/// same agent and task — with the same type and text.
///
/// Agent-less system lines are exempt: the orchestrator's own status line is
/// deduplicated by its counts in `App::handle_event`, which also knows when
/// work resumed after an idle spell and a fresh line is wanted.
fn repeats_latest(feed: &[Message], batch: &[Message], p: &PendingMessage) -> bool {
    if p.msg_type == MsgType::System && p.agent_id.is_none() {
        return false;
    }
    batch
        .iter()
        .rev()
        .chain(feed.iter().rev())
        .take(REPEAT_LOOKBACK)
        .find(|m| m.agent_id == p.agent_id && m.task_id == p.task_id)
        .is_some_and(|m| m.msg_type == p.msg_type && m.text == p.text)
}

#[cfg(test)]
// Fixtures build an `AppState` field by field; struct-update syntax would bury
// what each test is actually setting.
#[allow(clippy::field_reassign_with_default)]
mod tests {
    use super::*;
    use crate::backend::{make_agent, make_goal, make_task, make_team};
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
