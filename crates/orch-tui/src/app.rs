//! The application: state plus a backend plus a clock.
//!
//! Owns everything that *acts* — event ingestion, refresh, the soft-delete
//! sweep, notifications and wizard submission. Key dispatch lives in
//! [`crate::keys`] and command execution in [`crate::commands`], both as
//! `impl` blocks on this type.

use orch_domain::{Agent, Goal, GoalStatus, OrchestratorEvent, Task, TaskStatus, TuiPaletteName};

use crate::backend::{ClipboardPaste, HistoryEntry, ModelCatalog, TuiBackend};
use crate::clock::{Clock, SystemClock};
use crate::format::parse_timestamp_ms;
use crate::messages::{
    ANIM_TICK_MS, MESSAGE_BATCH_MS, MsgType, OBSERVER_POLL_MS, PendingMessage, REFRESH_DEBOUNCE_MS,
    WATCH_POLL_MS, format, format_event,
};
use crate::onboarding::{COMPLETED_DISMISS_MS, OnboardingStep};
use crate::state::{
    AppState, DetailPanel, EntityType, InputMode, PendingDeletion, TabFlash, ToastKind, ViewId,
};
use crate::theme::{Palette, Token};
use crate::wizard::engine::{Values, WizardKind};
use crate::wizard::session::{DraftKey, LeaveIntent, SubmitOutcome, WizardDraft};
use crate::wizard::{Wizard, configs};

/// A broadcast waiting for its confirming second Enter (design-v2 §3.4.4).
///
/// Keyed to the command-bar text **and** to the bar's edit generation, so the
/// arm belongs to one edit state rather than to a string: a backspace and
/// retype, an `↑` recall of the identical command, or any other path that
/// rewrites the bar all land on a new generation and have to arm again. The
/// text alone was not enough — the arm outlived the bar itself through the
/// mouse `✕` (gate S2, frontend F2, QA info 8).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BroadcastArm {
    /// The `/msg` tail the arm was created for, byte for byte.
    pub tail: String,
    /// `TextInput::generation` at the moment of arming.
    pub generation: u64,
}

pub struct App<B: TuiBackend, C: Clock = SystemClock> {
    pub state: AppState,
    pub backend: B,
    pub clock: C,
    pub model_catalog: ModelCatalog,
    /// Message batch window; `0` flushes synchronously.
    pub batch_ms: i64,
    /// Set by `cmd_msg` on a refusal or when a broadcast arms: the Enter
    /// handler then restores command mode with the text intact instead of
    /// clearing it. Cleared unconditionally once honoured.
    pub retain_command_bar: bool,
    /// The pending broadcast confirmation, if any.
    pub armed_broadcast: Option<BroadcastArm>,
    /// `TextInput::generation` of the command bar as the running command was
    /// submitted — the stamp [`BroadcastArm`] is keyed to. `0` for a command
    /// that did not come from the bar at all (`execute_command` directly,
    /// which no interactive path uses).
    pub(crate) submitted_generation: u64,
    last_anim_at: i64,
    last_refresh_at: i64,
}

impl<B: TuiBackend, C: Clock> App<B, C> {
    pub fn new(mut backend: B, clock: C) -> Self {
        let context = backend.context();
        let mut state = AppState {
            project_name: context.project_name,
            version: context.version,
            default_adapter: context.default_adapter,
            palette: Palette::new(context.initial_palette),
            notifications: context.initial_notifications,
            max_concurrent: context.initial_max_concurrent,
            watch_active: context.initial_watch_active,
            observer_mode: context.observer_mode,
            activity_filter: crate::messages::preset_types(&context.initial_activity_filter),
            ..AppState::default()
        };
        state.log_type_filter = crate::messages::preset_types("all");

        state.tasks = backend.refresh_tasks().unwrap_or_default();
        state.agents = backend.refresh_agents().unwrap_or_default();
        state.peer_status = backend.peer_status().unwrap_or_default();
        state.goals = backend.refresh_goals().unwrap_or_default();
        state.teams = backend.list_teams().unwrap_or_default();
        if let Ok(orch_state) = backend.refresh_state() {
            state.orch_state = orch_state;
        }
        state.onboarding = OnboardingStep::initial(&state.orch_state, state.tasks.len());

        let now = clock.now_ms();
        let mut app = App {
            state,
            backend,
            clock,
            model_catalog: ModelCatalog::new(),
            batch_ms: MESSAGE_BATCH_MS,
            retain_command_bar: false,
            armed_broadcast: None,
            submitted_generation: 0,
            last_anim_at: now,
            last_refresh_at: now,
        };

        // Launch banner.
        if app.state.observer_mode {
            app.emit(
                "Observer mode: watching external orchestrator via disk polling.",
                Token::Amber,
                MsgType::System,
            );
        } else if let Some(err) = app.backend.context().watch_error {
            app.emit(
                &format!("Watch mode failed: {err}. Tasks will not auto-dispatch."),
                Token::Red,
                MsgType::Error,
            );
        }
        app
    }

    pub fn now(&self) -> i64 {
        self.clock.now_ms()
    }

    /// Emit a local (not event-derived) feed message.
    pub fn emit(&mut self, text: &str, color: Token, msg_type: MsgType) {
        let now = self.now();
        let batch = self.batch_ms;
        self.state.add_message(
            PendingMessage {
                text: text.to_string(),
                color,
                msg_type,
                agent_id: None,
                task_id: None,
                detail: None,
                ts_ms: None,
            },
            now,
            batch,
        );
    }

    fn can_create_task(&self) -> bool {
        self.backend.can_create_task()
    }

    // ── Frame tick ─────────────────────────────────────────────────────────

    /// Drain the backend streams and fire every due timer. Called once per
    /// frame, and by the tests directly after advancing the clock.
    pub fn tick(&mut self) {
        self.dispatch_ui_event(&crate::keys::UiEvent::Tick);
        let now = self.now();

        // Animation tick, used by spinners, the scanner bar and the tab flash.
        while now - self.last_anim_at >= ANIM_TICK_MS {
            self.last_anim_at += ANIM_TICK_MS;
            self.state.tick += 1;
        }

        for event in self.backend.poll_events() {
            self.handle_event(&event);
        }
        for batch in self.backend.poll_history() {
            self.ingest_history(batch);
        }
        if let Some(catalog) = self.backend.poll_model_catalog() {
            self.model_catalog = catalog;
            self.refresh_open_agent_wizard();
        }
        if let Some(version) = self.backend.poll_update() {
            if self.state.latest_version.is_none() && !self.state.install_started {
                self.state.install_started = true;
                let _ = self.backend.background_install(&version);
            }
            self.state.latest_version = Some(version);
        }

        if let Some(deadline) = self.state.message_flush_at
            && now >= deadline
        {
            self.state.flush_messages(now);
        }
        if let Some(deadline) = self.state.refresh_at
            && now >= deadline
        {
            self.state.refresh_at = None;
            self.refresh_all(false);
        }

        // Periodic disk poll — 3 s in observer mode (where no events arrive at
        // all), 5 s otherwise, skipped while a refresh is already recent.
        let poll_interval = if self.state.observer_mode {
            OBSERVER_POLL_MS
        } else {
            WATCH_POLL_MS
        };
        if now - self.last_refresh_at >= poll_interval {
            self.refresh_all(false);
        }

        for deletion in self.state.take_expired_deletions(now) {
            self.execute_deletion(deletion);
        }
        self.state.expire_toasts(now);

        if let Some(flash) = self.state.flash
            && flash.is_done(self.state.tick)
        {
            self.state.flash = None;
        }

        if self.state.onboarding == OnboardingStep::Completed
            && let Some(deadline) = self.state.onboarding_dismiss_at
            && now >= deadline
        {
            self.state.onboarding = OnboardingStep::Dismissed;
            self.state.onboarding_dismiss_at = None;
            if !self.state.onboarding_completed_sent {
                self.state.onboarding_completed_sent = true;
                let _ = self.backend.complete_onboarding();
            }
        }

        if let Some(wizard) = self.state.wizard.as_mut() {
            wizard.poll(now);
        }
        if self.state.input_mode == InputMode::Command {
            self.state.command_bar.input.poll(now);
        }
    }

    /// The 150 ms debounce.
    pub fn schedule_refresh(&mut self) {
        if self.state.refresh_at.is_none() {
            self.state.refresh_at = Some(self.now() + REFRESH_DEBOUNCE_MS);
        }
    }

    // ── Events ─────────────────────────────────────────────────────────────

    pub fn handle_event(&mut self, event: &OrchestratorEvent) {
        // (a) record the run maps *before* formatting so output events resolve.
        if let OrchestratorEvent::AgentStarted {
            agent_id,
            task_id,
            run_id,
        } = event
        {
            self.state.runs.record(run_id, agent_id, task_id);
        }

        // (b) feed message. A tick whose counts have not changed adds no line;
        // the existing one's timestamp keeps counting.
        let repeated_tick = match event {
            OrchestratorEvent::OrchestratorTick { running, queued } => {
                let counts = (*running, *queued);
                self.state.last_tick.replace(counts) == Some(counts)
            }
            _ => false,
        };
        if !repeated_tick && let Some(message) = format_event(event, &self.state.runs) {
            let now = self.now();
            let batch = self.batch_ms;
            self.state.add_message(message, now, batch);
        }

        // (c) onboarding.
        if let Some(next) = self.state.onboarding.next_for(event) {
            self.state.onboarding = next;
            if next == OnboardingStep::Completed {
                self.state.onboarding_dismiss_at = Some(self.now() + COMPLETED_DISMISS_MS);
            }
        }

        // (d) toast + bell + tab flash.
        if let OrchestratorEvent::TaskStatusChanged { task_id, to, .. } = event {
            self.notify_status_change(task_id, *to);
        }

        // (e) debounced refresh.
        if refresh_triggering(event) {
            self.schedule_refresh();
        }
    }

    fn notify_status_change(&mut self, task_id: &str, to: TaskStatus) {
        let Some(kind) = ToastKind::from_status(to) else {
            return;
        };
        if !self.state.notifications.toast {
            return;
        }
        let task = self.state.task_by_id(task_id);
        let title = task
            .map(|t| t.title.clone())
            .unwrap_or_else(|| task_id.to_string());
        let agent_name = task
            .and_then(|t| t.assignee.clone())
            .map(|id| self.state.agent_name(&id));
        let message = kind.message(agent_name.as_deref());
        let now = self.now();
        self.state.push_toast(kind, &title, &message, now);

        if self.state.notifications.bell && kind.rings_bell() {
            self.backend.bell();
        }
        // The active tab never flashes.
        if self.state.view != ViewId::Tasks {
            self.state.flash = Some(TabFlash {
                tab: ViewId::Tasks,
                color: kind.foreground(),
                started_tick: self.state.tick,
            });
        }
    }

    /// Map a history batch onto feed messages.
    fn ingest_history(&mut self, batch: Vec<HistoryEntry>) {
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut messages = Vec::new();
        for entry in batch {
            // The entry's own instant, so a backfilled run reads its real age.
            let ts_ms = parse_timestamp_ms(&entry.timestamp);
            let pending = match entry.event_type.as_str() {
                "error" => {
                    let formatted = format::format_agent_error(&entry.data);
                    let summary = formatted.summary.unwrap_or_default();
                    // De-duplicate repeated nested provider errors per run.
                    let key = format!("{}\0{}", entry.run_id, summary);
                    if !seen.insert(key) {
                        continue;
                    }
                    if orch_domain::is_model_metadata_warning(&entry.data) {
                        PendingMessage {
                            text: format!("⚠ {}", summary),
                            color: Token::Yellow,
                            msg_type: MsgType::Info,
                            agent_id: Some(entry.agent_id.clone()),
                            task_id: Some(entry.task_id.clone()),
                            detail: Some(formatted.detail),
                            ts_ms,
                        }
                    } else {
                        PendingMessage {
                            text: summary,
                            color: Token::Red,
                            msg_type: MsgType::Error,
                            agent_id: Some(entry.agent_id.clone()),
                            task_id: Some(entry.task_id.clone()),
                            detail: Some(formatted.detail),
                            ts_ms,
                        }
                    }
                }
                "file_changed" => PendingMessage {
                    text: entry.data.clone(),
                    color: Token::Purple,
                    msg_type: MsgType::File,
                    agent_id: Some(entry.agent_id.clone()),
                    task_id: Some(entry.task_id.clone()),
                    detail: None,
                    ts_ms,
                },
                "done" => PendingMessage {
                    text: "Completed".into(),
                    color: Token::Green,
                    msg_type: MsgType::Lifecycle,
                    agent_id: Some(entry.agent_id.clone()),
                    task_id: Some(entry.task_id.clone()),
                    detail: None,
                    ts_ms,
                },
                "tool_call" => PendingMessage {
                    // `⚙ ${d?.name ?? 'tool'}()` — the payload is a JSON
                    // object for every adapter that records one, and a bare
                    // name only for the ones that do not.
                    text: format!("⚙ {}()", tool_call_name(&entry.data)),
                    color: Token::Cyan,
                    msg_type: MsgType::Tool,
                    agent_id: Some(entry.agent_id.clone()),
                    task_id: Some(entry.task_id.clone()),
                    detail: None,
                    ts_ms,
                },
                _ => {
                    let formatted = format::format_agent_output(&entry.data);
                    let Some(summary) = formatted.summary else {
                        continue;
                    };
                    let classification = format::classify_agent_summary(&summary);
                    let (msg_type, color) = classification_tokens(classification);
                    PendingMessage {
                        text: summary,
                        color,
                        msg_type,
                        agent_id: Some(entry.agent_id.clone()),
                        task_id: Some(entry.task_id.clone()),
                        detail: Some(formatted.detail),
                        ts_ms,
                    }
                }
            };
            messages.push(pending);
        }
        // `now` only stamps entries whose own timestamp did not parse.
        let now = self.now();
        self.state.prepend_messages(messages, now);
    }

    // ── Refresh ────────────────────────────────────────────────────────────

    /// Commit only what actually changed.
    pub fn refresh_all(&mut self, include_teams: bool) {
        self.last_refresh_at = self.now();
        if let Ok(status) = self.backend.peer_status() {
            self.state.peer_status = status;
        }
        if let Ok(tasks) = self.backend.refresh_tasks()
            && tasks_changed(&self.state.tasks, &tasks)
        {
            self.state.tasks = tasks;
        }
        if let Ok(agents) = self.backend.refresh_agents()
            && agents_changed(&self.state.agents, &agents)
        {
            self.state.agents = agents;
        }
        if let Ok(goals) = self.backend.refresh_goals()
            && goals_changed(&self.state.goals, &goals)
        {
            self.state.goals = goals;
        }
        if include_teams && let Ok(teams) = self.backend.list_teams() {
            self.state.teams = teams;
        }
        // State is always committed.
        if let Ok(orch_state) = self.backend.refresh_state() {
            self.state.orch_state = orch_state;
        }
        let can_create = self.can_create_task();
        self.state.clamp_all(can_create);
    }

    // ── Mouse ──────────────────────────────────────────────────────────────

    /// Adopt the hit map for the frame just drawn (design §3.1).
    ///
    /// A frame is also where the wheel's per-frame budget resets: the drain
    /// can deliver dozens of notches from one flick, and the frame is the
    /// only place the user sees a result.
    pub fn set_hits(&mut self, hits: crate::hit::HitMap) {
        self.state.frame_generation = hits.metrics.generation;
        self.state.mouse.hits = hits;
        self.start_frame();
    }

    // ── Soft delete ────────────────────────────────────────────────────────

    /// `Z` — restore the most recent pending deletion.
    pub fn undo_deletion(&mut self) {
        if let Some(last) = self.state.pending_deletions.len().checked_sub(1) {
            self.undo_deletion_at(last);
        }
    }

    /// [`Self::undo_deletion`] for one named undo-banner row; see
    /// [`AppState::undo_deletion_at`](crate::state::AppState::undo_deletion_at).
    pub fn undo_deletion_at(&mut self, index: usize) {
        if let Some(deletion) = self.state.undo_deletion_at(index) {
            let text = format!("↶ Undo: \"{}\" restored", deletion.entity_name);
            self.emit(&text, Token::Green, MsgType::System);
        }
    }

    fn execute_deletion(&mut self, deletion: PendingDeletion) {
        let result = match deletion.entity_type {
            EntityType::Task => self.backend.delete_task(&deletion.entity_id),
            EntityType::Agent => {
                // Force-stop happens only at execution time, never while the
                // deletion is still pending.
                if deletion.needs_force_stop {
                    let _ = self.backend.force_stop_agent(&deletion.entity_id);
                }
                self.backend.delete_agent(&deletion.entity_id)
            }
            EntityType::Goal => self.backend.delete_goal(&deletion.entity_id),
        };
        match result {
            Ok(()) => {
                let text = format!("✓ Deleted \"{}\"", deletion.entity_name);
                self.emit(&text, Token::Green, MsgType::System);
                self.refresh_all(false);
            }
            Err(err) => {
                let text = format!("Failed to delete \"{}\": {err}", deletion.entity_name);
                self.emit(&text, Token::Red, MsgType::Error);
            }
        }
    }

    // ── Wizards ────────────────────────────────────────────────────────────

    pub fn open_wizard(&mut self, wizard: Wizard) {
        if self.state.wizard_sessions.pending_leave.is_some() {
            return;
        }
        let key = DraftKey::from_wizard(&wizard);
        if self.state.wizard.is_some() && self.guard_wizard_leave(LeaveIntent::Replace(key)) {
            self.state.wizard_sessions.replacement = Some(wizard);
            return;
        }
        self.close_wizard();
        self.install_wizard(wizard);
    }

    pub(crate) fn install_wizard(&mut self, mut wizard: Wizard) {
        let key = DraftKey::from_wizard(&wizard);
        self.state.wizard_sessions.recovery = false;
        if let Some(draft) = self.state.wizard_sessions.draft_for(&key).cloned() {
            let exists = match key.kind {
                WizardKind::EditTask => self
                    .state
                    .tasks
                    .iter()
                    .any(|t| Some(&t.id) == key.target_id.as_ref()),
                WizardKind::EditGoal => self
                    .state
                    .goals
                    .iter()
                    .any(|g| Some(&g.id) == key.target_id.as_ref()),
                WizardKind::EditAgent => self
                    .state
                    .agents
                    .iter()
                    .any(|a| Some(&a.id) == key.target_id.as_ref()),
                _ => true,
            };
            let stale = matches!(
                key.kind,
                WizardKind::EditTask
                    | WizardKind::EditGoal
                    | WizardKind::EditAgent
                    | WizardKind::Config
            ) && (!exists || draft.source_defaults() != &wizard.source_defaults);
            if stale {
                self.state.wizard_sessions.replacement = Some(wizard);
                self.state.wizard_sessions.recovery = true;
            }
            wizard = draft.restore();
            wizard.session_notice = Some(if stale {
                "Stale session draft — recovery only. Esc, Discard to restart from current data."
            } else { "Session draft restored" }.into());
            wizard.copy_notice = Some(
                if stale {
                    "Stale draft; Esc restart; F6"
                } else {
                    "Session draft restored; F6"
                }
                .into(),
            );
            self.state.pending_attachments = draft.attachments;
        }
        let attachments = self.state.pending_attachments.len();
        if attachments > 0 && wizard.paste_capable {
            wizard.footer_extra = Some(format!("📎{attachments}"));
        }
        let _ = self
            .state
            .register_editor_origin(key.clone(), wizard.step_id().into());
        self.state.wizard_sessions.active_key = Some(key);
        self.state.wizard = Some(wizard);
        self.state.input_mode = InputMode::Wizard;
        if self.state.wizard_sessions.recovery {
            use crate::widgets::text_viewer::{
                Completeness, Representation, TextViewer, ViewerDocument, ViewerSource,
            };
            let wizard = self.state.wizard.as_ref().expect("installed wizard");
            let body = wizard
                .semantic_values()
                .iter()
                .map(|(key, value)| format!("{key}:\n{value}\n"))
                .collect::<Vec<_>>()
                .join("\n")
                + &format!(
                    "\nAttachments:\n{}",
                    self.state.pending_attachments.join("\n")
                );
            let kind = match wizard.kind {
                WizardKind::EditAgent => EntityType::Agent,
                WizardKind::EditGoal => EntityType::Goal,
                _ => EntityType::Task,
            };
            let source = ViewerSource::Entity {
                kind,
                id: wizard
                    .target_id
                    .clone()
                    .unwrap_or_else(|| "session-recovery".into()),
                field: format!(
                    "session-draft:{:?}:{:?}:{:?}",
                    wizard.kind, wizard.setting, wizard.template_key
                ),
            };
            let document = ViewerDocument {
                source: source.clone(),
                title:
                    "Stale session draft — recovery only; close viewer, Esc then Discard to restart"
                        .into(),
                original_bytes: body.len(),
                body: body.into(),
                representation: Representation::Field,
                raw_body: None,
                completeness: Completeness::Complete,
                captured_at_ms: self.clock.now_ms(),
                initial_anchor: 0,
            };
            if self.state.register_viewer_origin(source).is_ok() {
                self.state.viewer = Some(TextViewer::new(document));
            }
        }
        self.invalidate_hits();
    }

    pub fn cancel_wizard(&mut self) {
        if self.guard_wizard_leave(LeaveIntent::Cancel) {
            return;
        }
        self.close_wizard();
    }

    pub fn open_task_wizard(&mut self) {
        self.open_wizard(configs::new_task_wizard(&self.state.agents));
    }

    pub fn open_edit_task_wizard(&mut self) {
        let Some(task) = self.state.selected_task().cloned() else {
            return;
        };
        let wizard = configs::edit_task_wizard(&task, &self.state.agents);
        self.open_wizard(wizard);
    }

    pub fn open_agent_wizard(&mut self) {
        let wizard = configs::new_agent_wizard(
            &self.state.agents,
            &self.state.teams,
            &self.model_catalog,
            &self.state.default_adapter,
        );
        self.open_wizard(wizard);
    }

    pub fn open_edit_agent_wizard(&mut self) {
        let Some(agent) = self.state.selected_agent().cloned() else {
            return;
        };
        let wizard = configs::edit_agent_wizard(
            &agent,
            &self.state.agents,
            &self.state.teams,
            &self.model_catalog,
        );
        self.open_wizard(wizard);
    }

    pub fn open_goal_wizard(&mut self) {
        let wizard = configs::new_goal_wizard(&self.state.agents);
        self.open_wizard(wizard);
    }

    pub fn open_edit_goal_wizard(&mut self) {
        let Some(goal) = self.state.selected_goal().cloned() else {
            return;
        };
        let wizard = configs::edit_goal_wizard(&goal, &self.state.agents);
        self.open_wizard(wizard);
    }

    pub fn open_team_wizard(&mut self) {
        let wizard = configs::new_team_wizard(&self.state.agents, &self.state.teams);
        self.open_wizard(wizard);
    }

    /// `/agent shop` and `Ctrl+S` inside the agent wizard.
    pub fn open_shop_wizard(&mut self) {
        if self.state.default_adapter == "shell" {
            self.emit(
                "Agent Shop requires an AI provider. Press N to create a Shell agent.",
                Token::Yellow,
                MsgType::System,
            );
            return;
        }
        self.open_wizard(configs::agent_shop_wizard());
    }

    pub fn open_config_wizard(&mut self, setting: &str) {
        if !matches!(
            setting,
            "palette"
                | "activity-filter"
                | "max-concurrent"
                | "notifications-toast"
                | "notifications-bell"
        ) {
            self.submission_failed("Unknown setting");
            return;
        }
        let palette = self.state.palette.name.as_str().to_string();
        let filter = crate::messages::preset_label(&self.state.activity_filter).to_string();
        let wizard = configs::config_wizard(
            setting,
            &palette,
            &filter,
            self.state.max_concurrent,
            self.state.notifications,
        );
        self.open_wizard(wizard);
    }

    /// A `callback`-mode suggestion was chosen on the agent-name step: open the
    /// pre-filled shop wizard for that template.
    pub fn apply_name_suggestion(&mut self, template_key: &str) {
        // The guard precedes the template lookup, so an unknown key still
        // refuses rather than silently doing nothing. The wording differs from
        // the `/agent shop` refusal.
        if self.state.default_adapter == "shell" {
            self.emit(
                "Agent Shop requires an AI provider. Choose Shell and enter a command instead.",
                Token::Yellow,
                MsgType::System,
            );
            return;
        }
        let Some(template) = orch_domain::get_shop_template_by_key(template_key) else {
            return;
        };
        let wizard = configs::agent_from_shop_wizard(
            template,
            &self.state.agents,
            &self.state.teams,
            &self.model_catalog,
            &self.state.default_adapter,
        );
        self.open_wizard(wizard);
    }

    /// Rebuild an open agent wizard's steps when a fresher catalog arrives,
    /// preserving the adapter, name and template.
    fn refresh_open_agent_wizard(&mut self) {
        let Some(wizard) = self.state.wizard.as_ref() else {
            return;
        };
        let steps = match wizard.kind {
            WizardKind::Agent | WizardKind::AgentFromShop => configs::agent_wizard_steps(
                &self.state.agents,
                &self.state.teams,
                &self.model_catalog,
                &self.state.default_adapter,
            ),
            WizardKind::EditAgent => {
                let target = wizard.target_id.clone().unwrap_or_default();
                let Some(agent) = self.state.agent_by_id(&target).cloned() else {
                    return;
                };
                configs::edit_agent_wizard_steps(
                    &agent,
                    &self.state.agents,
                    &self.state.teams,
                    &self.model_catalog,
                )
            }
            _ => return,
        };
        // Re-apply the shop template defaults when this wizard came from one.
        let steps = match wizard
            .template_key
            .as_deref()
            .and_then(orch_domain::get_shop_template_by_key)
        {
            Some(template) => {
                configs::apply_shop_template(steps, template, &self.state.default_adapter)
            }
            None => steps,
        };
        if let Some(wizard) = self.state.wizard.as_mut() {
            wizard.replace_steps(steps);
        }
    }

    /// Dispatch a completed wizard to the backend.
    pub fn submit_wizard(&mut self, values: Values) {
        if self.state.wizard_sessions.recovery {
            self.submission_failed("Stale draft is recovery only; explicitly discard and restart");
            return;
        }
        let Some(wizard) = self.state.wizard.as_ref() else {
            return;
        };
        let kind = wizard.kind.clone();
        let target = wizard.target_id.clone();
        let setting = wizard.setting.clone();
        let key = DraftKey::from_wizard(wizard);
        let surface = crate::hit::TextSurfaceId::Editor(key.clone(), wizard.step_id().into());
        if let Some(origin) = self.state.live_copy_origin(&surface).cloned() {
            self.state.clear_copy_feedback(&origin);
        }
        let outcome = match kind {
            WizardKind::Task => self.submit_task(&values),
            WizardKind::EditTask => self.submit_edit_task(&values, target.as_deref()),
            WizardKind::Agent | WizardKind::AgentFromShop => self.submit_agent(&values),
            WizardKind::EditAgent => self.submit_edit_agent(&values, target.as_deref()),
            WizardKind::AgentShop => self.submit_shop(&values),
            WizardKind::Team => self.submit_team(&values),
            WizardKind::Goal => self.submit_goal(&values),
            WizardKind::EditGoal => self.submit_edit_goal(&values, target.as_deref()),
            WizardKind::Config => self.submit_config(&values, setting.as_deref()),
        };
        match outcome {
            SubmitOutcome::Failed => {
                if let Some(wizard) = self.state.wizard.as_ref() {
                    let draft = WizardDraft::capture(wizard, &self.state.pending_attachments);
                    if let Err(error) = self.state.wizard_sessions.save_draft(draft) {
                        // The active form is retained regardless of saved-draft capacity.
                        self.state.wizard_sessions.error = Some(error);
                    }
                }
                self.state.input_mode = InputMode::Wizard;
            }
            SubmitOutcome::Committed => {
                self.state.wizard_sessions.discard_draft(&key);
                self.close_wizard();
            }
            SubmitOutcome::Advanced => self.state.wizard_sessions.discard_draft(&key),
        }
        self.invalidate_hits();
    }

    fn submission_failed(&mut self, error: &str) -> SubmitOutcome {
        if let Some(w) = self.state.wizard.as_mut() {
            w.session_error = Some(error.into());
        }
        self.fail(error);
        SubmitOutcome::Failed
    }

    fn submit_task(&mut self, values: &Values) -> SubmitOutcome {
        let (title, mut opts, assignee) = configs::task_wizard_to_input(values);
        let attachments = self.state.pending_attachments.clone();
        let count = attachments.len();
        if count > 0 {
            opts.attachments = Some(attachments);
        }
        self.emit(
            &format!("Creating \"{title}\"..."),
            Token::Amber,
            MsgType::System,
        );
        match self.backend.create_task(&title, opts) {
            Ok(task) => {
                let clip = if count > 0 {
                    format!(" 📎{count}")
                } else {
                    String::new()
                };
                self.emit(
                    &format!("✓ Created \"{}\" ({}){clip}", task.title, task.id),
                    Token::Green,
                    MsgType::System,
                );
                if let Some(agent_id) = assignee
                    && let Err(error) = self.backend.assign_task(&task.id, &agent_id)
                {
                    self.fail(&format!("Task {} committed; assignment failed: {error}. Use explicit assignment; do not create again",task.id));
                }
                self.refresh_all(false);
                SubmitOutcome::Committed
            }
            Err(err) => self.submission_failed(&err),
        }
    }

    fn submit_edit_task(&mut self, values: &Values, target: Option<&str>) -> SubmitOutcome {
        let Some(target) = target else {
            return self.submission_failed("Missing target");
        };
        let (mut fields, assignee) = configs::edit_task_wizard_to_fields(values);
        let attachments = self.state.pending_attachments.clone();
        let count = attachments.len();
        if count > 0 {
            fields.attachments = Some(attachments);
        }
        self.emit("Updating task...", Token::Amber, MsgType::System);
        match self.backend.update_task(target, fields) {
            Ok(task) => {
                let clip = if count > 0 {
                    format!(" 📎{count}")
                } else {
                    String::new()
                };
                self.emit(
                    &format!("✓ Updated \"{}\"{clip}", task.title),
                    Token::Green,
                    MsgType::System,
                );
                if let Some(agent_id) = assignee
                    && let Err(error) = self.backend.assign_task(target, &agent_id)
                {
                    self.fail(&format!(
                        "Task {target} updated; assignment failed: {error}. Use explicit assignment"
                    ));
                }
                self.refresh_all(false);
                SubmitOutcome::Committed
            }
            Err(err) => self.submission_failed(&err),
        }
    }

    fn submit_agent(&mut self, values: &Values) -> SubmitOutcome {
        let default_adapter = self.state.default_adapter.clone();
        let input = configs::agent_wizard_to_input(values, &default_adapter);
        self.emit(
            &format!("Creating agent \"{}\"...", input.name),
            Token::Amber,
            MsgType::System,
        );
        let command = input.opts.command.clone();
        match self
            .backend
            .add_agent(&input.name, &input.adapter, input.opts)
        {
            Ok(agent) => {
                let suffix = match command {
                    Some(command) if !command.is_empty() => {
                        format!(" · {}", crate::format::cap_line(&command, 60))
                    }
                    _ => String::new(),
                };
                self.emit(
                    &format!(
                        "✓ Created agent \"{}\" ({}, {}){suffix}",
                        agent.name, agent.id, agent.adapter
                    ),
                    Token::Green,
                    MsgType::System,
                );
                match input.team_id {
                    Some(team_id) => {
                        match self.backend.join_team(&team_id, &agent.id) {
                            Ok(team) => {
                            self.emit(
                                &format!("✓ Joined team \"{}\"", team.name),
                                Token::Green,
                                MsgType::System,
                            );
                            }
                            Err(error) => self.fail(&format!("Agent {} committed; team assignment failed: {error}. Use explicit team assignment",agent.id)),
                        }
                        self.refresh_all(true);
                    }
                    None => self.refresh_all(false),
                }
                SubmitOutcome::Committed
            }
            Err(err) => self.submission_failed(&err),
        }
    }

    fn submit_edit_agent(&mut self, values: &Values, target: Option<&str>) -> SubmitOutcome {
        let Some(target) = target else {
            return self.submission_failed("Missing target");
        };
        let (fields, team_id) = configs::edit_agent_wizard_to_fields(values);
        let old_team = self
            .state
            .teams
            .iter()
            .find(|t| t.members.iter().any(|m| m.agent_id == target))
            .map(|t| t.id.clone());
        self.emit("Updating agent...", Token::Amber, MsgType::System);
        match self.backend.update_agent(target, fields) {
            Ok(agent) => {
                self.emit(
                    &format!("✓ Updated agent \"{}\"", agent.name),
                    Token::Green,
                    MsgType::System,
                );
                // A *skipped* Team step means "unchanged", not "leave".
                let new_team = if values.contains_key("team") {
                    team_id.clone().unwrap_or_default()
                } else {
                    old_team.clone().unwrap_or_default()
                };
                let mut team_ops = 0;
                if let Some(old) = old_team.as_deref()
                    && !old.is_empty()
                    && old != new_team
                {
                    match self.backend.leave_team(old, target) { Ok(team) => {
                        self.emit(
                            &format!("✓ Left team \"{}\"", team.name),
                            Token::Green,
                            MsgType::System,
                        );
                    }, Err(error) => self.fail(&format!("Agent {target} updated; team operation failed: {error}. Use explicit team assignment")) }
                    team_ops += 1;
                }
                if !new_team.is_empty() && Some(new_team.as_str()) != old_team.as_deref() {
                    match self.backend.join_team(&new_team, target) { Ok(team) => {
                        self.emit(
                            &format!("✓ Joined team \"{}\"", team.name),
                            Token::Green,
                            MsgType::System,
                        );
                    }, Err(error) => self.fail(&format!("Agent {target} updated; team operation failed: {error}. Use explicit team assignment")) }
                    team_ops += 1;
                }
                self.refresh_all(team_ops > 0);
                SubmitOutcome::Committed
            }
            Err(err) => self.submission_failed(&err),
        }
    }

    fn submit_shop(&mut self, values: &Values) -> SubmitOutcome {
        let key = values.get("shop_template").cloned().unwrap_or_default();
        match orch_domain::get_shop_template_by_key(&key) {
            Some(template) => {
                let wizard = configs::agent_from_shop_wizard(
                    template,
                    &self.state.agents,
                    &self.state.teams,
                    &self.model_catalog,
                    &self.state.default_adapter,
                );
                self.close_wizard();
                self.install_wizard(wizard);
                SubmitOutcome::Advanced
            }
            None => self.submission_failed("No template selected"),
        }
    }

    fn submit_team(&mut self, values: &Values) -> SubmitOutcome {
        let input = configs::team_wizard_to_input(values);
        self.emit(
            &format!("Creating team \"{}\"...", input.name),
            Token::Amber,
            MsgType::System,
        );
        match self.backend.create_team(input) {
            Ok(team) => {
                self.emit(
                    &format!(
                        "✓ Created team \"{}\" ({}, {} members)",
                        team.name,
                        team.id,
                        team.members.len()
                    ),
                    Token::Green,
                    MsgType::System,
                );
                self.refresh_all(true);
                SubmitOutcome::Committed
            }
            Err(err) => self.submission_failed(&err),
        }
    }

    fn submit_goal(&mut self, values: &Values) -> SubmitOutcome {
        let input = configs::goal_wizard_to_input(values);
        self.emit(
            &format!("Creating goal \"{}\"...", input.title),
            Token::Amber,
            MsgType::System,
        );
        match self.backend.create_goal(input) {
            Ok(goal) => {
                self.emit(
                    &format!("✓ Created goal \"{}\" ({})", goal.title, goal.id),
                    Token::Green,
                    MsgType::System,
                );
                self.refresh_all(false);
                SubmitOutcome::Committed
            }
            Err(err) => self.submission_failed(&err),
        }
    }

    fn submit_edit_goal(&mut self, values: &Values, target: Option<&str>) -> SubmitOutcome {
        let Some(target) = target else {
            return self.submission_failed("Missing target");
        };
        let fields = configs::edit_goal_wizard_to_fields(values);
        self.emit("Updating goal...", Token::Amber, MsgType::System);
        match self.backend.update_goal(target, fields) {
            Ok(goal) => {
                self.emit(
                    &format!("✓ Updated goal \"{}\"", goal.title),
                    Token::Green,
                    MsgType::System,
                );
                self.refresh_all(false);
                SubmitOutcome::Committed
            }
            Err(err) => self.submission_failed(&err),
        }
    }

    fn submit_config(&mut self, values: &Values, setting: Option<&str>) -> SubmitOutcome {
        let Some(setting) = setting else {
            return self.submission_failed("Missing setting");
        };
        let result = match setting {
            "palette" => {
                let Some(palette) = values.get("palette").and_then(|v| parse_palette(v)) else {
                    return self.submission_failed("Invalid palette");
                };
                self.backend
                    .save_palette(palette)
                    .map(|()| self.state.palette = Palette::new(palette))
            }
            "activity-filter" => {
                let Some(preset) = values.get("activity_filter") else {
                    return self.submission_failed("Missing activity filter");
                };
                self.backend
                    .save_activity_filter(preset)
                    .map(|()| self.state.activity_filter = crate::messages::preset_types(preset))
            }
            "max-concurrent" => {
                let Some(parsed) = values
                    .get("max_concurrent")
                    .and_then(|v| v.parse::<u32>().ok())
                    .filter(|v| *v > 0)
                else {
                    return self.submission_failed("Invalid maximum concurrency");
                };
                self.backend
                    .save_max_concurrent(parsed)
                    .map(|()| self.state.max_concurrent = parsed)
            }
            "notifications-toast" | "notifications-bell" => {
                let field = if setting == "notifications-toast" {
                    "notifications_toast"
                } else {
                    "notifications_bell"
                };
                let Some(value) = values
                    .get(field)
                    .filter(|v| matches!(v.as_str(), "true" | "false"))
                else {
                    return self.submission_failed("Invalid notification setting");
                };
                let mut notifications = self.state.notifications;
                if setting == "notifications-toast" {
                    notifications.toast = value == "true";
                } else {
                    notifications.bell = value == "true";
                }
                self.backend
                    .save_notifications(notifications)
                    .map(|()| self.state.notifications = notifications)
            }
            _ => return self.submission_failed("Unknown setting"),
        };
        if let Err(error) = result {
            return self.submission_failed(&error);
        }
        self.emit("Settings saved", Token::Green, MsgType::System);
        SubmitOutcome::Committed
    }

    pub fn change_palette(&mut self, palette: TuiPaletteName) {
        self.state.palette = Palette::new(palette);
        let _ = self.backend.save_palette(palette);
        self.emit(
            &format!("Palette: {}", palette.as_str()),
            Token::Green,
            MsgType::System,
        );
    }

    pub fn fail(&mut self, err: &str) {
        let text = format!("Failed: {err}");
        self.emit(&text, Token::Red, MsgType::Error);
    }

    // ── Clipboard ──────────────────────────────────────────────────────────

    /// Paste an image from the clipboard.
    pub fn paste_image(&mut self) {
        match self.backend.paste_image() {
            Ok(ClipboardPaste::Text) => {
                self.emit("Clipboard has text, not image", Token::Dim, MsgType::System)
            }
            Ok(ClipboardPaste::Empty) => {
                self.emit("Clipboard is empty", Token::Dim, MsgType::System)
            }
            Ok(ClipboardPaste::Image { path, bytes }) => {
                self.state.pending_attachments.push(path);
                let kb = (bytes as f64 / 1024.0).round() as i64;
                self.emit(
                    &format!("📎 Image attached ({kb}KB)"),
                    Token::Green,
                    MsgType::System,
                );
                let count = self.state.pending_attachments.len();
                if let Some(wizard) = self.state.wizard.as_mut() {
                    wizard.footer_extra = Some(format!("📎{count}"));
                }
            }
            Err(_) => self.emit("Clipboard paste failed", Token::Red, MsgType::Error),
        }
    }

    // ── Entity actions shared by keys and commands ─────────────────────────

    pub fn run_task_by_id(&mut self, id: &str) {
        let Some(task) = self.state.task_by_id(id).cloned() else {
            return;
        };
        if !crate::state::is_runnable(task.status) {
            let text = format!("Cannot run \"{}\" — status is {}", task.title, task.status);
            self.emit(&text, Token::Yellow, MsgType::System);
            return;
        }
        let text = format!("Running {id}...");
        self.emit(&text, Token::Amber, MsgType::System);
        match self.backend.run_task(id) {
            Ok(()) => {
                let text = format!("✓ Dispatched {id}");
                self.emit(&text, Token::Green, MsgType::System);
            }
            Err(err) => self.fail(&err),
        }
    }

    pub fn cancel_task_by_id(&mut self, id: &str) {
        let Some(task) = self.state.task_by_id(id).cloned() else {
            return;
        };
        if matches!(
            task.status,
            TaskStatus::Done | TaskStatus::Failed | TaskStatus::Cancelled
        ) {
            let text = format!("Cannot cancel — status is {}", task.status);
            self.emit(&text, Token::Yellow, MsgType::System);
            return;
        }
        let text = format!("Cancelling \"{}\"...", task.title);
        self.emit(&text, Token::Amber, MsgType::System);
        match self.backend.cancel_task(id) {
            Ok(()) => {
                let text = format!("✓ Cancelled \"{}\"", task.title);
                self.emit(&text, Token::Green, MsgType::System);
                self.refresh_all(false);
            }
            Err(err) => self.fail(&err),
        }
    }

    pub fn approve_task_by_id(&mut self, id: &str) {
        let Some(task) = self.state.task_by_id(id).cloned() else {
            return;
        };
        if task.status != TaskStatus::Review {
            let text = format!("Cannot approve — status is {}", task.status);
            self.emit(&text, Token::Yellow, MsgType::System);
            return;
        }
        match self.backend.approve_task(id) {
            Ok(()) => {
                let text = format!("✓ Approved \"{}\"", task.title);
                self.emit(&text, Token::Green, MsgType::System);
                self.refresh_all(false);
            }
            Err(err) => self.fail(&err),
        }
    }

    pub fn reject_task_by_id(&mut self, id: &str, feedback: Option<&str>) {
        let Some(task) = self.state.task_by_id(id).cloned() else {
            return;
        };
        if task.status != TaskStatus::Review {
            let text = format!("Cannot reject — status is {}", task.status);
            self.emit(&text, Token::Yellow, MsgType::System);
            return;
        }
        match self.backend.reject_task(id, feedback) {
            Ok(()) => {
                let text = format!("✓ Rejected \"{}\" → todo", task.title);
                self.emit(&text, Token::Green, MsgType::System);
                self.refresh_all(false);
            }
            Err(err) => self.fail(&err),
        }
    }

    pub fn delete_task_by_id(&mut self, id: &str) {
        let Some(task) = self.state.task_by_id(id).cloned() else {
            return;
        };
        if task.status == TaskStatus::InProgress {
            self.emit(
                "Cannot delete — task is running",
                Token::Yellow,
                MsgType::System,
            );
            return;
        }
        let now = self.now();
        let message =
            self.state
                .schedule_deletion(EntityType::Task, &task.id, &task.title, false, now);
        self.emit(&message, Token::Yellow, MsgType::System);
    }

    pub fn delete_agent_by_id(&mut self, id: &str) {
        let Some(agent) = self.state.agent_by_id(id).cloned() else {
            return;
        };
        let running = agent.status == orch_domain::AgentStatus::Running
            || self
                .state
                .orch_state
                .running
                .values()
                .any(|r| r.agent_id == agent.id);
        if running && !self.backend.can_force_stop_agent() {
            let text = format!(
                "Cannot delete — agent \"{}\" is running. Press S to stop first.",
                agent.name
            );
            self.emit(&text, Token::Yellow, MsgType::System);
            return;
        }
        let now = self.now();
        let message =
            self.state
                .schedule_deletion(EntityType::Agent, &agent.id, &agent.name, running, now);
        self.emit(&message, Token::Yellow, MsgType::System);
    }

    pub fn delete_goal_by_id(&mut self, id: &str) {
        let Some(goal) = self.state.goal_by_id(id).cloned() else {
            return;
        };
        let now = self.now();
        let message =
            self.state
                .schedule_deletion(EntityType::Goal, &goal.id, &goal.title, false, now);
        self.emit(&message, Token::Yellow, MsgType::System);
    }

    pub fn force_stop_selected_agent(&mut self) {
        let Some(agent) = self.state.selected_agent().cloned() else {
            return;
        };
        let running = agent.status == orch_domain::AgentStatus::Running
            || self
                .state
                .orch_state
                .running
                .values()
                .any(|r| r.agent_id == agent.id);
        if !running {
            let text = format!("Agent \"{}\" is not running", agent.name);
            self.emit(&text, Token::Yellow, MsgType::System);
            return;
        }
        match self.backend.force_stop_agent(&agent.id) {
            Ok(()) => {
                let text = format!("✓ Stopped \"{}\"", agent.name);
                self.emit(&text, Token::Green, MsgType::System);
                self.refresh_all(false);
            }
            Err(err) => self.fail(&err),
        }
    }

    pub fn toggle_autonomous_by_id(&mut self, id: &str) {
        let Some(agent) = self.state.agent_by_id(id).cloned() else {
            return;
        };
        let enabled = !agent.autonomous.unwrap_or(false);
        match self.backend.toggle_autonomous(&agent.id, enabled) {
            Ok(_) => {
                let text = format!(
                    "⟳ {} autonomous {}",
                    agent.name,
                    if enabled { "ON" } else { "OFF" }
                );
                self.emit(&text, Token::Cyan, MsgType::System);
                self.refresh_all(false);
            }
            Err(err) => self.fail(&err),
        }
    }

    pub fn set_goal_status(&mut self, id: &str, status: GoalStatus, force: bool) {
        match self.backend.update_goal_status(id, status, force) {
            Ok(goal) => {
                let text = format!("✓ Goal \"{}\" → {}", goal.title, status.as_str());
                self.emit(&text, Token::Green, MsgType::System);
                self.refresh_all(false);
            }
            Err(err) => self.fail(&err),
        }
    }

    /// Open or close the goal detail panel, loading the progress report.
    pub fn toggle_goal_detail(&mut self) {
        if self.state.detail_open() {
            self.state.detail = DetailPanel::None;
            self.state.goal_detail_scroll = 0;
            return;
        }
        let Some(goal) = self.state.selected_goal().cloned() else {
            return;
        };
        self.state.detail = DetailPanel::Entity;
        self.state.goal_detail_scroll = 0;
        // An empty report means "nothing recorded yet", not a failure, and
        // renders no `progress` section (BACKEND.md).
        self.state.goal_progress_report = self
            .backend
            .goal_progress(&goal.id)
            .ok()
            .filter(|report| !report.trim().is_empty());
    }
}

/// The tool name from a `tool_call` history payload: the `name` field when the
/// payload is a JSON object, the raw string when it is a bare name, and `tool`
/// when neither (`d?.name ?? 'tool'`).
fn tool_call_name(data: &str) -> String {
    if let Ok(serde_json::Value::Object(map)) = serde_json::from_str::<serde_json::Value>(data) {
        return match map.get("name").and_then(|v| v.as_str()) {
            Some(name) if !name.is_empty() => name.to_string(),
            _ => "tool".to_string(),
        };
    }
    if data.trim().is_empty() {
        return "tool".to_string();
    }
    data.to_string()
}

fn classification_tokens(classification: format::Classification) -> (MsgType, Token) {
    let msg_type = match classification.kind {
        format::SummaryKind::Lifecycle => MsgType::Lifecycle,
        format::SummaryKind::Tool => MsgType::Tool,
        format::SummaryKind::Result => MsgType::Result,
        format::SummaryKind::Info => MsgType::Info,
        format::SummaryKind::Output => MsgType::Output,
    };
    let color = match classification.color {
        format::SummaryColor::Dim => Token::Dim,
        format::SummaryColor::Yellow => Token::Yellow,
        format::SummaryColor::Silver => Token::Silver,
    };
    (msg_type, color)
}

pub fn parse_palette(value: &str) -> Option<TuiPaletteName> {
    orch_domain::TUI_PALETTE_NAMES
        .into_iter()
        .find(|p| p.as_str() == value)
}

/// Which events arm the debounced refresh.
fn refresh_triggering(event: &OrchestratorEvent) -> bool {
    matches!(
        event,
        OrchestratorEvent::TaskStatusChanged { .. }
            | OrchestratorEvent::TaskCreated { .. }
            | OrchestratorEvent::TaskAssigned { .. }
            | OrchestratorEvent::AgentStarted { .. }
            | OrchestratorEvent::AgentCompleted { .. }
            | OrchestratorEvent::RunRetry { .. }
            | OrchestratorEvent::GoalCreated { .. }
            | OrchestratorEvent::GoalStatusChanged { .. }
            | OrchestratorEvent::GoalUpdated { .. }
            | OrchestratorEvent::GoalDeleted { .. }
    )
}

/// Whether the list changed, for entities that carry `updated_at`.
pub fn tasks_changed(prev: &[Task], next: &[Task]) -> bool {
    if prev.len() != next.len() {
        return true;
    }
    prev.iter()
        .zip(next)
        .any(|(a, b)| a.id != b.id || a.updated_at != b.updated_at)
}

pub fn goals_changed(prev: &[Goal], next: &[Goal]) -> bool {
    if prev.len() != next.len() {
        return true;
    }
    prev.iter()
        .zip(next)
        .any(|(a, b)| a.id != b.id || a.updated_at != b.updated_at)
}

/// Agents have no `updated_at`, so the comparison must be **deep** — no
/// reference-equality shortcut.
pub fn agents_changed(prev: &[Agent], next: &[Agent]) -> bool {
    prev.len() != next.len() || prev != next
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::{FakeBackend, make_agent, make_task};
    use crate::clock::TestClock;
    use orch_domain::AgentStatus;

    fn app() -> App<FakeBackend, TestClock> {
        let clock = TestClock::new(0);
        App::new(FakeBackend::new(), clock)
    }

    #[test]
    fn entity_list_changed_detects_a_timestampless_field_change() {
        let mut before = make_agent("agt_1", "alpha", AgentStatus::Idle);
        let after_same = before.clone();
        assert!(!agents_changed(
            std::slice::from_ref(&before),
            std::slice::from_ref(&after_same)
        ));
        before.stats.total_runs = 5;
        assert!(agents_changed(
            std::slice::from_ref(&before),
            std::slice::from_ref(&after_same)
        ));
    }

    #[test]
    fn entity_list_changed_uses_ids_and_timestamps_for_tasks() {
        let a = make_task("tsk_1", "a", TaskStatus::Todo);
        let mut b = a.clone();
        assert!(!tasks_changed(
            std::slice::from_ref(&a),
            std::slice::from_ref(&b)
        ));
        b.updated_at = "2026-10-13T13:00:00.000Z".into();
        assert!(tasks_changed(
            std::slice::from_ref(&a),
            std::slice::from_ref(&b)
        ));
        assert!(tasks_changed(std::slice::from_ref(&a), &[]));
    }

    #[test]
    fn events_arm_the_150ms_refresh_debounce() {
        let mut app = app();
        app.handle_event(&OrchestratorEvent::TaskAssigned {
            task_id: "tsk_1".into(),
            agent_id: "agt_1".into(),
        });
        assert_eq!(app.state.refresh_at, Some(REFRESH_DEBOUNCE_MS));
        // A non-triggering event does not re-arm it.
        app.state.refresh_at = None;
        app.handle_event(&OrchestratorEvent::TeamDisbanded {
            team_id: "t".into(),
        });
        assert_eq!(app.state.refresh_at, None);
    }

    #[test]
    fn agent_started_records_the_run_maps_before_formatting() {
        let mut app = app();
        app.handle_event(&OrchestratorEvent::AgentStarted {
            agent_id: "agt_1".into(),
            task_id: "tsk_1".into(),
            run_id: "run_1".into(),
        });
        app.handle_event(&OrchestratorEvent::AgentFileChanged {
            run_id: "run_1".into(),
            agent_id: "agt_1".into(),
            path: "src/lib.rs".into(),
        });
        app.state.flush_messages(0);
        let last = app.state.messages.last().unwrap();
        assert_eq!(last.task_id.as_deref(), Some("tsk_1"));
    }

    #[test]
    fn a_done_transition_raises_a_toast_without_a_bell() {
        let mut backend = FakeBackend::new();
        backend.tasks = vec![make_task("tsk_1", "Ship it", TaskStatus::Done)];
        let mut app = App::new(backend, TestClock::new(0));
        app.handle_event(&OrchestratorEvent::TaskStatusChanged {
            task_id: "tsk_1".into(),
            from: TaskStatus::InProgress,
            to: TaskStatus::Done,
        });
        assert_eq!(app.state.toasts.len(), 1);
        assert_eq!(app.state.toasts[0].title, "Ship it");
        assert!(
            !app.backend
                .called(|c| matches!(c, crate::backend::Call::Bell))
        );
    }

    #[test]
    fn the_bell_rings_for_failed_and_review_only_when_enabled() {
        let mut backend = FakeBackend::new();
        backend.tasks = vec![make_task("tsk_1", "Ship it", TaskStatus::Failed)];
        backend.context.initial_notifications.bell = true;
        let mut app = App::new(backend, TestClock::new(0));
        app.handle_event(&OrchestratorEvent::TaskStatusChanged {
            task_id: "tsk_1".into(),
            from: TaskStatus::InProgress,
            to: TaskStatus::Failed,
        });
        assert_eq!(
            app.backend
                .count(|c| matches!(c, crate::backend::Call::Bell)),
            1
        );
        app.handle_event(&OrchestratorEvent::TaskStatusChanged {
            task_id: "tsk_1".into(),
            from: TaskStatus::InProgress,
            to: TaskStatus::Done,
        });
        assert_eq!(
            app.backend
                .count(|c| matches!(c, crate::backend::Call::Bell)),
            1,
            "done never rings"
        );
    }

    #[test]
    fn toast_false_suppresses_both_the_toast_and_the_flash() {
        let mut backend = FakeBackend::new();
        backend.context.initial_notifications.toast = false;
        let mut app = App::new(backend, TestClock::new(0));
        app.state.view = ViewId::Agents;
        app.handle_event(&OrchestratorEvent::TaskStatusChanged {
            task_id: "tsk_1".into(),
            from: TaskStatus::InProgress,
            to: TaskStatus::Done,
        });
        assert!(app.state.toasts.is_empty());
        assert!(app.state.flash.is_none());
    }

    #[test]
    fn the_active_tasks_tab_never_flashes() {
        let mut app = app();
        app.state.view = ViewId::Tasks;
        app.handle_event(&OrchestratorEvent::TaskStatusChanged {
            task_id: "tsk_1".into(),
            from: TaskStatus::InProgress,
            to: TaskStatus::Done,
        });
        assert!(app.state.flash.is_none());
        app.state.view = ViewId::Logs;
        app.handle_event(&OrchestratorEvent::TaskStatusChanged {
            task_id: "tsk_1".into(),
            from: TaskStatus::InProgress,
            to: TaskStatus::Done,
        });
        assert_eq!(app.state.flash.unwrap().tab, ViewId::Tasks);
    }

    #[test]
    fn soft_delete_executes_only_after_the_window_and_the_sweep() {
        let mut backend = FakeBackend::new();
        backend.tasks = vec![make_task("tsk_1", "Doomed", TaskStatus::Todo)];
        let clock = TestClock::new(0);
        let mut app = App::new(backend, clock.clone());
        app.delete_task_by_id("tsk_1");
        clock.advance_ms(200);
        app.tick();
        assert!(
            !app.backend
                .called(|c| matches!(c, crate::backend::Call::DeleteTask(_)))
        );
        clock.advance_ms(6_000);
        app.tick();
        assert!(
            app.backend
                .called(|c| matches!(c, crate::backend::Call::DeleteTask(id) if id == "tsk_1"))
        );
    }

    #[test]
    fn undo_cancels_the_deletion_entirely() {
        let mut backend = FakeBackend::new();
        backend.tasks = vec![make_task("tsk_1", "Doomed", TaskStatus::Todo)];
        let clock = TestClock::new(0);
        let mut app = App::new(backend, clock.clone());
        app.delete_task_by_id("tsk_1");
        app.undo_deletion();
        clock.advance_ms(10_000);
        app.tick();
        assert!(
            !app.backend
                .called(|c| matches!(c, crate::backend::Call::DeleteTask(_)))
        );
        app.state.flush_messages(clock.now_ms());
        assert!(
            app.state
                .messages
                .iter()
                .any(|m| m.text.contains("restored"))
        );
    }

    #[test]
    fn undo_deletion_at_restores_the_banner_row_and_lets_the_others_expire() {
        let mut backend = FakeBackend::new();
        backend.tasks = vec![
            make_task("tsk_1", "First", TaskStatus::Todo),
            make_task("tsk_2", "Second", TaskStatus::Todo),
        ];
        let clock = TestClock::new(0);
        let mut app = App::new(backend, clock.clone());
        app.delete_task_by_id("tsk_1");
        app.delete_task_by_id("tsk_2");
        assert_eq!(app.state.pending_deletions.len(), 2);

        // Banner row 0 is "First"; `undo_deletion`'s LIFO would restore
        // "Second" instead.
        app.undo_deletion_at(0);
        app.state.flush_messages(clock.now_ms());
        assert!(
            app.state
                .messages
                .iter()
                .any(|m| m.text.contains("\"First\" restored"))
        );
        clock.advance_ms(10_000);
        app.tick();
        assert!(
            !app.backend
                .called(|c| matches!(c, crate::backend::Call::DeleteTask(id) if id == "tsk_1"))
        );
        assert!(
            app.backend
                .called(|c| matches!(c, crate::backend::Call::DeleteTask(id) if id == "tsk_2"))
        );
    }

    #[test]
    fn undo_deletion_with_nothing_pending_is_a_no_op() {
        let mut app = App::new(FakeBackend::new(), TestClock::new(0));
        app.undo_deletion();
        app.undo_deletion_at(0);
        assert!(app.state.messages.is_empty());
    }

    #[test]
    fn a_running_task_is_never_deletable() {
        let mut backend = FakeBackend::new();
        backend.tasks = vec![make_task("tsk_1", "Busy", TaskStatus::InProgress)];
        let mut app = App::new(backend, TestClock::new(0));
        app.delete_task_by_id("tsk_1");
        assert!(app.state.pending_deletions.is_empty());
        app.state.flush_messages(0);
        assert!(
            app.state
                .messages
                .iter()
                .any(|m| m.text == "Cannot delete — task is running")
        );
    }

    #[test]
    fn a_running_agent_carries_needs_force_stop_and_stops_only_at_execution() {
        let mut backend = FakeBackend::new();
        backend.agents = vec![make_agent("agt_1", "alpha", AgentStatus::Running)];
        let clock = TestClock::new(0);
        let mut app = App::new(backend, clock.clone());
        app.delete_agent_by_id("agt_1");
        assert!(app.state.pending_deletions[0].needs_force_stop);
        assert!(
            !app.backend
                .called(|c| matches!(c, crate::backend::Call::ForceStopAgent(_)))
        );
        clock.advance_ms(6_000);
        app.tick();
        assert!(
            app.backend
                .called(|c| matches!(c, crate::backend::Call::ForceStopAgent(_)))
        );
        assert!(
            app.backend
                .called(|c| matches!(c, crate::backend::Call::DeleteAgent(_)))
        );
    }

    #[test]
    fn deleting_a_running_agent_without_the_stop_capability_is_refused() {
        let mut backend = FakeBackend::new();
        backend.agents = vec![make_agent("agt_1", "alpha", AgentStatus::Running)];
        backend.can_force_stop_agent = false;
        let mut app = App::new(backend, TestClock::new(0));
        app.delete_agent_by_id("agt_1");
        assert!(app.state.pending_deletions.is_empty());
        app.state.flush_messages(0);
        assert!(
            app.state
                .messages
                .iter()
                .any(|m| m.text.contains("is running. Press S to stop first."))
        );
    }

    #[test]
    fn onboarding_dismisses_five_seconds_after_completion_exactly_once() {
        let clock = TestClock::new(0);
        let mut app = App::new(FakeBackend::new(), clock.clone());
        app.state.onboarding = OnboardingStep::RunStarted;
        app.handle_event(&OrchestratorEvent::TaskStatusChanged {
            task_id: "tsk_1".into(),
            from: TaskStatus::InProgress,
            to: TaskStatus::Done,
        });
        assert_eq!(app.state.onboarding, OnboardingStep::Completed);
        clock.advance_ms(4_999);
        app.tick();
        assert_eq!(app.state.onboarding, OnboardingStep::Completed);
        clock.advance_ms(1);
        app.tick();
        assert_eq!(app.state.onboarding, OnboardingStep::Dismissed);
        clock.advance_ms(10_000);
        app.tick();
        assert_eq!(
            app.backend
                .count(|c| matches!(c, crate::backend::Call::CompleteOnboarding)),
            1
        );
    }

    #[test]
    fn the_animation_tick_advances_every_120ms() {
        let clock = TestClock::new(0);
        let mut app = App::new(FakeBackend::new(), clock.clone());
        app.tick();
        assert_eq!(app.state.tick, 0);
        clock.advance_ms(360);
        app.tick();
        assert_eq!(app.state.tick, 3);
    }

    #[test]
    fn an_update_check_installs_once_in_the_background() {
        let mut backend = FakeBackend::new();
        backend.pending_update = Some("1.2.3".into());
        let mut app = App::new(backend, TestClock::new(0));
        app.tick();
        assert_eq!(app.state.latest_version.as_deref(), Some("1.2.3"));
        app.backend.pending_update = Some("1.2.3".into());
        app.tick();
        assert_eq!(
            app.backend
                .count(|c| matches!(c, crate::backend::Call::BackgroundInstall(_))),
            1
        );
    }

    #[test]
    fn observer_mode_announces_itself_on_launch() {
        let mut backend = FakeBackend::new();
        backend.context.observer_mode = true;
        let mut app = App::new(backend, TestClock::new(0));
        app.state.flush_messages(0);
        assert!(app.state.messages[0].text.starts_with("Observer mode:"));
    }

    #[test]
    fn a_watch_error_is_announced_when_not_in_observer_mode() {
        let mut backend = FakeBackend::new();
        backend.context.watch_error = Some("lock held".into());
        let mut app = App::new(backend, TestClock::new(0));
        app.state.flush_messages(0);
        assert_eq!(
            app.state.messages[0].text,
            "Watch mode failed: lock held. Tasks will not auto-dispatch."
        );
    }

    #[test]
    fn pasting_an_image_attaches_it_and_updates_the_footer() {
        let mut backend = FakeBackend::new();
        backend.clipboard = ClipboardPaste::Image {
            path: "/tmp/orch-paste/clipboard-1.png".into(),
            bytes: 2_048,
        };
        let mut app = App::new(backend, TestClock::new(0));
        app.open_task_wizard();
        app.paste_image();
        assert_eq!(app.state.pending_attachments.len(), 1);
        assert_eq!(
            app.state.wizard.as_ref().unwrap().footer_extra.as_deref(),
            Some("📎1")
        );
        app.state.flush_messages(0);
        assert!(
            app.state
                .messages
                .iter()
                .any(|m| m.text == "📎 Image attached (2KB)")
        );
    }

    #[test]
    fn clipboard_text_and_empty_report_without_attaching() {
        let mut backend = FakeBackend::new();
        backend.clipboard = ClipboardPaste::Text;
        let mut app = App::new(backend, TestClock::new(0));
        app.paste_image();
        assert!(app.state.pending_attachments.is_empty());
        app.state.flush_messages(0);
        assert!(
            app.state
                .messages
                .iter()
                .any(|m| m.text == "Clipboard has text, not image")
        );
    }

    #[test]
    fn confirming_discard_clears_pending_attachments() {
        let mut backend = FakeBackend::new();
        backend.clipboard = ClipboardPaste::Image {
            path: "/tmp/x.png".into(),
            bytes: 10,
        };
        let mut app = App::new(backend, TestClock::new(0));
        app.open_task_wizard();
        app.paste_image();
        app.cancel_wizard();
        assert_eq!(app.state.pending_attachments, ["/tmp/x.png"]);
        assert!(app.state.wizard_sessions.pending_leave.is_some());
        app.resolve_wizard_leave(crate::wizard::LeaveChoice::Discard);
        assert!(app.state.pending_attachments.is_empty());
        assert!(app.state.wizard.is_none());
    }
}
