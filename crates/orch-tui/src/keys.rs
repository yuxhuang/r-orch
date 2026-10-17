//! The ordered key-dispatch chain (docs/parity/tui.md §2, PARITY.md §12.2).
//!
//! The order is load-bearing and the per-view overloads (`a` `g` `s` `c` `x`
//! `f`) are resolved here explicitly — getting one wrong destroys the wrong
//! entity.
//!
//! One deliberate fix (PARITY.md §14.8): **both** filter pickers swallow global
//! keys. In the original the type picker did not, so `q` quit through it.

use orch_domain::{GoalStatus, TaskStatus};
use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use crate::app::App;
use crate::backend::TuiBackend;
use crate::clock::Clock;
use crate::command_bar::{SuggestionCtx, resolve_completion, resolve_suggestions_in};
use crate::messages::{MsgType, cycle_preset, preset_types};
use crate::state::{DetailPanel, InputMode, ViewId, is_runnable};
use crate::text::is_meta;
use crate::theme::Token;
use crate::wizard::engine::{WizardKind, WizardOutcome};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitCause {
    User,
    Signal,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UiEvent {
    Terminal(Event),
    Tick,
    ExitRequested(ExitCause),
}

/// One clamped step within `0..total`.
fn clamp_step(cursor: usize, delta: i32, total: usize) -> usize {
    let last = total.saturating_sub(1);
    if delta < 0 {
        cursor.saturating_sub(1).min(last)
    } else {
        (cursor + 1).min(last)
    }
}

/// The most a single bracketed paste may insert.
///
/// A terminal hands over whatever was on the clipboard — a log file, a whole
/// source tree listing — in one `Event::Paste`, and every frame after that
/// re-measures the value. 64 KiB is sixteen times the engine's own body cap
/// (4000 UTF-16 units), so nothing a user can actually *send* is lost.
pub const MAX_PASTE_BYTES: usize = 64 * 1024;

/// A registered verb and nothing else — `/msg`, not `/msg alice`.
fn is_bare_verb(trimmed: &str) -> bool {
    trimmed
        .strip_prefix('/')
        .is_some_and(|verb| crate::command_bar::spec_for(verb).is_some())
}

/// Strip a trailing ` [...]` args hint from a suggestion's command text.
fn strip_args_hint(cmd: &str) -> &str {
    match cmd.find(" [") {
        Some(idx) if cmd.ends_with(']') => &cmd[..idx],
        _ => cmd,
    }
}

impl<B: TuiBackend, C: Clock> App<B, C> {
    pub fn handle_key(&mut self, key: KeyEvent) {
        self.dispatch_ui_event(&UiEvent::Terminal(Event::Key(key)));
    }
    pub fn dispatch_ui_event(&mut self, event: &UiEvent) {
        if let UiEvent::Terminal(Event::Key(key)) = event {
            if key.kind != KeyEventKind::Press {
                return;
            }
            if key.code == KeyCode::F(2) {
                self.toggle_mouse_enabled();
                return;
            }
        }
        // B0 extends this prelude with modal/footer guards before consumers.
        if matches!(event, UiEvent::Terminal(Event::Mouse(_)))
            && (!self.state.mouse.enabled || !self.state.mouse.hits.is_valid())
        {
            return;
        }
        let viewer_handled = crate::widgets::text_viewer::handle_ui_event(self, event);
        let session_handled = if !viewer_handled || matches!(event, UiEvent::Tick) {
            crate::wizard::handle_session_event(self, event)
        } else {
            false
        };
        if viewer_handled || session_handled {
            return;
        }
        match event {
            UiEvent::Terminal(Event::Key(key)) => self.handle_key_internal(*key),
            UiEvent::Terminal(Event::Mouse(mouse)) => self.handle_mouse_internal(*mouse),
            UiEvent::Terminal(Event::Paste(text)) => self.handle_paste_internal(text),
            UiEvent::Terminal(Event::Resize(w, h)) => self.resize(*w, *h),
            UiEvent::ExitRequested(_) => self.state.should_exit = true,
            UiEvent::Tick | UiEvent::Terminal(_) => {}
        }
    }
    pub(crate) fn handle_key_internal(&mut self, key: KeyEvent) {
        // Key *release* and *repeat* events exist under the kitty protocol;
        // only presses drive the UI.
        if key.kind != KeyEventKind::Press {
            return;
        }
        // Any key can move what is on screen, and the runner drains up to 64
        // events or 40 ms per frame (`runner.rs:31-32`) — so over tmux or the
        // `orch web` websocket a key and a click can arrive in the same batch,
        // and the click must not resolve against the pre-key frame. The cost
        // is that a click landing within one drain of your own keystroke is
        // dropped, which no human can usefully intend (design §3.2.3).
        self.state.mouse.hits.invalidate();
        let now = self.now();

        // 0. `F2` toggles mouse capture **before** every other stage, so it
        //    works from inside the help overlay, a picker, a wizard and
        //    command mode — the places where selecting an error message
        //    matters most. Nothing else is bound to it (`F1` is help), so no
        //    existing binding moves.
        if key.code == KeyCode::F(2) {
            self.toggle_mouse_enabled();
            return;
        }

        // 1. Help overlay: `?`, `Esc` and F1 are swallowed; anything else
        //    dismisses the overlay *and* performs its normal action.
        if self.state.show_help {
            self.state.show_help = false;
            if matches!(key.code, KeyCode::Char('?') | KeyCode::Esc | KeyCode::F(1)) {
                return;
            }
        }

        // 2. A filter picker swallows every global key.
        if self.state.show_agent_picker {
            self.handle_agent_picker_key(key);
            return;
        }
        if self.state.show_type_picker {
            self.handle_type_picker_key(key);
            return;
        }

        // 3. Ctrl+S / Alt+S inside the NEW AGENT wizard opens the Agent Shop.
        // TS is `(key.ctrl || key.meta) && input === 's'`; see [`is_meta`] for
        // why `key.meta` excludes Cmd.
        if (key.modifiers.contains(KeyModifiers::CONTROL) || is_meta(key))
            && matches!(key.code, KeyCode::Char('s') | KeyCode::Char('S'))
            && self.state.input_mode == InputMode::Wizard
            && self
                .state
                .wizard
                .as_ref()
                .is_some_and(|w| w.kind == WizardKind::Agent)
        {
            self.open_shop_wizard();
            return;
        }

        // 4. Any input mode takes the key.
        match self.state.input_mode {
            InputMode::Command => {
                self.handle_command_key(key, now);
                return;
            }
            InputMode::Wizard => {
                self.handle_wizard_key(key, now);
                return;
            }
            InputMode::None => {}
        }

        // 5. Normal mode, in source order.
        self.handle_normal_key(key);
    }

    // ── Normal mode ────────────────────────────────────────────────────────

    fn handle_normal_key(&mut self, key: KeyEvent) {
        let view = self.state.view;
        let detail_open = self.state.detail_open();

        match key.code {
            // `q` is tested first, so it can never be typed in normal mode.
            KeyCode::Char('q') | KeyCode::Char('Q') => {
                self.request_quit();
                return;
            }
            // `Esc` never quits.
            KeyCode::Esc => {
                self.close_detail_or_tail();
                return;
            }
            KeyCode::Char('+') | KeyCode::Char('=') if view != ViewId::Logs => {
                self.state.grow_detail();
                return;
            }
            KeyCode::Char('-') if view != ViewId::Logs => {
                self.state.grow_list();
                return;
            }
            // Uppercase only — there is no lowercase `m` binding.
            KeyCode::Char('M') if view != ViewId::Logs => {
                self.toggle_detail_maximized();
                return;
            }
            KeyCode::Char('?') | KeyCode::F(1) => {
                self.open_help();
                return;
            }
            KeyCode::Char('/') if !detail_open => {
                self.enter_command_mode();
                return;
            }
            _ => {}
        }

        // Logs-only filter keys.
        if view == ViewId::Logs && !detail_open {
            match key.code {
                KeyCode::Char('a') | KeyCode::Char('A') => {
                    self.open_agent_picker();
                    return;
                }
                KeyCode::Char('f') => {
                    self.open_type_picker();
                    return;
                }
                KeyCode::Char('F') => {
                    self.cycle_log_types();
                    return;
                }
                _ => {}
            }
        }

        // `z` — LIFO undo of a pending deletion.
        if matches!(key.code, KeyCode::Char('z') | KeyCode::Char('Z'))
            && !self.state.pending_deletions.is_empty()
        {
            self.undo_deletion();
            return;
        }

        // `f`/`F` on the entity views cycles the activity-feed preset.
        if matches!(key.code, KeyCode::Char('f') | KeyCode::Char('F'))
            && view != ViewId::Logs
            && !detail_open
        {
            self.cycle_activity_filter();
            return;
        }

        // `n` — the new-entity wizard for this view.
        if matches!(key.code, KeyCode::Char('n') | KeyCode::Char('N')) && !detail_open {
            self.open_new_wizard_for_view();
            return;
        }

        // Goal-view actions.
        if view == ViewId::Goals {
            let selected = self.state.selected_goal().cloned();
            match (key.code, selected) {
                (KeyCode::Char('e') | KeyCode::Char('E'), Some(_)) => {
                    self.open_edit_goal_wizard();
                    return;
                }
                (KeyCode::Char('d') | KeyCode::Char('D'), Some(goal)) => {
                    self.delete_goal_by_id(&goal.id);
                    return;
                }
                (KeyCode::Char('c') | KeyCode::Char('C'), Some(goal))
                    if matches!(goal.status, GoalStatus::Active | GoalStatus::Paused) =>
                {
                    // `C` achieves with force, cancelling cancellable tasks.
                    self.set_goal_status(&goal.id, GoalStatus::Achieved, true);
                    return;
                }
                (KeyCode::Char('x') | KeyCode::Char('X'), Some(goal))
                    if matches!(goal.status, GoalStatus::Active | GoalStatus::Paused) =>
                {
                    self.set_goal_status(&goal.id, GoalStatus::Abandoned, false);
                    return;
                }
                (KeyCode::Char('p') | KeyCode::Char('P'), Some(goal))
                    if matches!(goal.status, GoalStatus::Active | GoalStatus::Paused) =>
                {
                    let next = if goal.status == GoalStatus::Active {
                        GoalStatus::Paused
                    } else {
                        GoalStatus::Active
                    };
                    self.set_goal_status(&goal.id, next, false);
                    return;
                }
                _ => {}
            }
        }

        // Task-view actions. `a` approves a review task — this overrides the
        // "A = agents tab" binding.
        if view == ViewId::Tasks {
            let selected = self.state.selected_task().cloned();
            match (key.code, selected.as_ref()) {
                (KeyCode::Char('a') | KeyCode::Char('A'), Some(task))
                    if task.status == TaskStatus::Review =>
                {
                    let id = task.id.clone();
                    self.approve_task_by_id(&id);
                    return;
                }
                (KeyCode::Char('x') | KeyCode::Char('X'), Some(task))
                    if task.status == TaskStatus::Review =>
                {
                    let id = task.id.clone();
                    self.reject_task_by_id(&id, None);
                    return;
                }
                (KeyCode::Char('c') | KeyCode::Char('C'), Some(task)) => {
                    let id = task.id.clone();
                    self.cancel_task_by_id(&id);
                    return;
                }
                (KeyCode::Char('e') | KeyCode::Char('E'), Some(_)) => {
                    self.open_edit_task_wizard();
                    return;
                }
                (KeyCode::Char('s') | KeyCode::Char('S'), _) => {
                    self.toggle_show_all_tasks();
                    return;
                }
                // `g` toggles grouping — overrides "G = goals tab" here.
                (KeyCode::Char('g') | KeyCode::Char('G'), _) if !detail_open => {
                    self.state.group_by_goal = !self.state.group_by_goal;
                    self.state.task_selected = 0;
                    self.state.task_scroll = 0;
                    return;
                }
                (KeyCode::Char('d') | KeyCode::Char('D'), Some(task))
                    if task.status != TaskStatus::InProgress =>
                {
                    let id = task.id.clone();
                    self.delete_task_by_id(&id);
                    return;
                }
                (KeyCode::Char('r') | KeyCode::Char('R'), Some(task)) => {
                    let id = task.id.clone();
                    self.run_task_by_id(&id);
                    return;
                }
                _ => {}
            }
        }

        // Agent-view actions.
        if view == ViewId::Agents {
            let selected = self.state.selected_agent().cloned();
            match (key.code, selected) {
                (KeyCode::Char('e') | KeyCode::Char('E'), Some(_)) => {
                    self.open_edit_agent_wizard();
                    return;
                }
                (KeyCode::Char('s') | KeyCode::Char('S'), Some(_)) => {
                    self.force_stop_selected_agent();
                    return;
                }
                (KeyCode::Char('d') | KeyCode::Char('D'), Some(agent)) => {
                    self.delete_agent_by_id(&agent.id);
                    return;
                }
                (KeyCode::Char('u') | KeyCode::Char('U'), Some(agent)) => {
                    self.toggle_autonomous_by_id(&agent.id);
                    return;
                }
                // Lowercase only: `M` keeps meaning *maximize*, and `m` was
                // unbound, so no existing binding moves. Inert without a
                // selection or on a backend that cannot send.
                (KeyCode::Char('m'), Some(agent)) if self.backend.can_send_message() => {
                    let name = agent.name.clone();
                    self.compose_message_to(&name);
                    return;
                }
                _ => {}
            }
        }

        // View switching — blocked while a detail panel is open.
        if !detail_open {
            match key.code {
                KeyCode::Char('g') | KeyCode::Char('G') if view != ViewId::Tasks => {
                    self.switch_view(ViewId::Goals);
                    return;
                }
                KeyCode::Char('t') | KeyCode::Char('T') => {
                    self.switch_view(ViewId::Tasks);
                    return;
                }
                KeyCode::Char('a') | KeyCode::Char('A') if view != ViewId::Logs => {
                    self.switch_view(ViewId::Agents);
                    return;
                }
                KeyCode::Char('l') | KeyCode::Char('L') => {
                    self.switch_view(ViewId::Logs);
                    return;
                }
                KeyCode::Tab | KeyCode::Right => {
                    self.switch_view(view.next());
                    return;
                }
                KeyCode::Left => {
                    self.switch_view(view.prev());
                    return;
                }
                _ => {}
            }
        }

        match key.code {
            KeyCode::Enter => self.handle_enter(),
            KeyCode::Up | KeyCode::Char('k') => self.move_selection_up(),
            KeyCode::Down | KeyCode::Char('j') => self.move_selection_down(),
            _ => {}
        }
    }

    // ── Named actions ──────────────────────────────────────────────────────
    //
    // One body per labelled action, so the key path and anything else that
    // offers the same label share an implementation. Nothing here consults
    // `key.code`: the caller has already decided *which* action this is, which
    // is what keeps the per-view overloads (`a` `g` `s` `c` `x` `f`) resolved
    // in exactly one place — the chain above.

    /// `q` / `Q`.
    pub(crate) fn request_quit(&mut self) {
        self.request_exit(ExitCause::User);
    }

    pub fn request_exit(&mut self, cause: ExitCause) {
        self.dispatch_ui_event(&UiEvent::ExitRequested(cause));
    }

    /// `Esc` in normal mode: close the detail panel, else leave the logs
    /// selection and return to tail.
    pub(crate) fn close_detail_or_tail(&mut self) {
        if self.state.detail_open() {
            self.state.detail = DetailPanel::None;
            self.state.goal_detail_scroll = 0;
        } else if self.state.view == ViewId::Logs && self.state.log_selected_index >= 0 {
            self.state.log_selected_index = -1;
            self.state.log_scroll_offset = 0;
        }
    }

    /// A bracketed paste, routed by the same stage order [`App::handle_key`]
    /// uses (design-v2 §3.9.2).
    ///
    /// Inserted through the text APIs rather than replayed as key events: a
    /// synthesised `Enter` is exactly the bug bracketed paste exists to fix.
    /// In **normal mode it is dropped** — that is the dangerous half, where a
    /// pasted `q` quits and a pasted `d` deletes an agent. Both pickers drop
    /// it explicitly because they are reachable *from* an input mode; the help
    /// overlay needs no guard, being reachable only from normal mode, where
    /// the match below drops the paste anyway.
    ///
    /// `\r\n` and a lone `\r` normalise to `\n`, and the newlines are *kept*:
    /// the dominant paste into a message body is a stack trace or multi-line
    /// instructions, and flattening would corrupt the data at the moment the
    /// user is trying to hand an agent something precise. The command bar
    /// renders each one as a dim `⏎` (`widgets/command_bar.rs`).
    ///
    /// Capped at [`MAX_PASTE_BYTES`], with a line in the feed when the cut
    /// happens: a terminal will hand over a pasted file of any size, and
    /// nothing downstream of here wants it (`MessageService` cuts a body at
    /// 4000 UTF-16 units, and a wizard field is one line).
    pub fn handle_paste(&mut self, text: &str) {
        self.dispatch_ui_event(&UiEvent::Terminal(Event::Paste(text.to_owned())));
    }
    pub(crate) fn handle_paste_internal(&mut self, text: &str) {
        if text.is_empty() || self.state.show_agent_picker || self.state.show_type_picker {
            return;
        }
        let now = self.now();
        let mut normalized = text.replace("\r\n", "\n").replace('\r', "\n");
        let mut advisory = None;
        if normalized.len() > MAX_PASTE_BYTES {
            let mut cut = MAX_PASTE_BYTES;
            while !normalized.is_char_boundary(cut) {
                cut -= 1;
            }
            normalized.truncate(cut);
            let text = format!(
                "Pasted text cut at {} KiB — paste the rest separately",
                MAX_PASTE_BYTES / 1024
            );
            self.emit(&text, Token::Amber, MsgType::System);
            advisory = Some(text);
        }
        match self.state.input_mode {
            InputMode::Command => {
                self.state.command_bar.input.insert_str(&normalized, now);
                self.state.command_bar.suggestion_index = 0;
                // The activity feed is not drawn while the bar is open, so an
                // advisory about the text that just landed *in* the bar goes
                // above it, where the refusals go (design-v2 §3.6.3). Cleared
                // by the next keystroke, like them. `retain_command_bar` is
                // **not** set: only `/msg` keeps the bar open.
                self.state.command_bar.retained_refusal = advisory.into_iter().collect();
                // A paste is an edit like any other, so it cannot stand in for
                // the confirming Enter of a broadcast (gate S2).
                self.disarm_broadcast();
            }
            InputMode::Wizard => {
                if let Some(wizard) = self.state.wizard.as_mut() {
                    wizard.insert_paste(&normalized, now);
                }
            }
            InputMode::None => {}
        }
    }

    /// `M` — maximize the detail panel, or restore the split.
    pub(crate) fn toggle_detail_maximized(&mut self) {
        self.state.detail_maximized = !self.state.detail_maximized;
    }

    /// `?` / `F1`.
    pub(crate) fn open_help(&mut self) {
        self.state.show_help = true;
    }

    /// `m` on the Agents tab: the command bar, prefilled with the recipient
    /// already chosen and the cursor ready for the body.
    ///
    /// The name is quoted here rather than by the caller, so an awkward one
    /// (`say "hi"`, `🚀 deploy`) round-trips through the grammar.
    pub(crate) fn compose_message_to(&mut self, name: &str) {
        self.enter_command_mode();
        let prefill = format!("/msg {} ", crate::msg::quote_recipient(name));
        self.state.command_bar.input.reset(&prefill);
        self.state.command_bar.suggestion_index = 0;
    }

    /// The suggestion rows for an input, with the agent list `/msg` needs.
    ///
    /// The borrow is a statement-scoped temporary, so `&mut self.state` uses
    /// after it still compile.
    pub(crate) fn suggestions(&self, value: &str) -> Vec<crate::command_bar::Suggestion> {
        let order = self.state.sorted_agents();
        resolve_suggestions_in(
            value,
            &SuggestionCtx {
                agents: &self.state.agents,
                order: &order,
            },
        )
    }

    /// `/` — the command bar, pre-filled with its own prefix.
    pub(crate) fn enter_command_mode(&mut self) {
        self.state.input_mode = InputMode::Command;
        self.state.command_bar.input.reset("/");
        self.state.command_bar.suggestion_index = 0;
        self.state.command_bar.retained_refusal.clear();
        // A fresh bar inherits nothing from the last one.
        self.disarm_broadcast();
    }

    /// Leave command mode, dropping everything that belonged to that bar: its
    /// text, its history cursor, the refusal drawn above it and any pending
    /// broadcast confirmation.
    ///
    /// **Every** exit goes through here. An arm that outlives the bar is one
    /// Enter away from an unconfirmed broadcast, and the `✕` zone in the hint
    /// row used to leave one behind (gate S2 / code F2).
    pub(crate) fn leave_command_mode(&mut self) {
        self.state.input_mode = InputMode::None;
        self.state.command_bar.input.reset("");
        self.state.command_bar.history.reset();
        self.state.command_bar.retained_refusal.clear();
        self.disarm_broadcast();
    }

    /// Drop a pending broadcast confirmation and the bar retention that goes
    /// with it. `retain_command_bar` is consumed by a `mem::take` in the Enter
    /// handler, so a leaked `true` would send the *next*, unrelated command
    /// down the retaining branch — the branch that keeps the arm alive.
    pub(crate) fn disarm_broadcast(&mut self) {
        self.armed_broadcast = None;
        self.retain_command_bar = false;
    }

    /// Logs `a` — the agent filter picker, drafting the live filter.
    pub(crate) fn open_agent_picker(&mut self) {
        self.state.show_agent_picker = true;
        self.state.picker_cursor = 0;
        self.state.picker_draft = self.state.log_agent_filter.clone();
    }

    /// Logs `f` — the message-type filter picker.
    pub(crate) fn open_type_picker(&mut self) {
        self.state.show_type_picker = true;
        self.state.picker_cursor = 0;
        self.state.type_picker_draft = self.state.log_type_filter.clone();
    }

    /// Logs `F` — the next type preset, with no picker.
    pub(crate) fn cycle_log_types(&mut self) {
        let next = cycle_preset(&self.state.log_type_filter);
        self.state.log_type_filter = preset_types(next);
    }

    /// `f` / `F` off the logs view — the next activity-feed preset, persisted.
    pub(crate) fn cycle_activity_filter(&mut self) {
        let next = cycle_preset(&self.state.activity_filter);
        self.state.activity_filter = preset_types(next);
        let _ = self.backend.save_activity_filter(next);
    }

    /// `n` — the new-entity wizard this view creates, if the backend allows it.
    pub(crate) fn open_new_wizard_for_view(&mut self) {
        match self.state.view {
            ViewId::Tasks if self.backend.can_create_task() => self.open_task_wizard(),
            ViewId::Agents if self.backend.can_add_agent() => self.open_agent_wizard(),
            ViewId::Goals if self.backend.can_create_goal() => self.open_goal_wizard(),
            _ => {}
        }
    }

    /// Tasks `s` — show every task or collapse back, from the top.
    pub(crate) fn toggle_show_all_tasks(&mut self) {
        self.state.show_all_tasks = !self.state.show_all_tasks;
        self.state.task_selected = 0;
        self.state.task_scroll = 0;
    }

    pub(crate) fn switch_view(&mut self, view: ViewId) {
        self.state.view = view;
    }

    pub(crate) fn handle_enter(&mut self) {
        let can_create_task = self.backend.can_create_task();
        let can_add_agent = self.backend.can_add_agent();
        let can_create_goal = self.backend.can_create_goal();
        match self.state.view {
            ViewId::Tasks => {
                if Some(self.state.task_selected) == self.state.show_all_row_index() {
                    self.toggle_show_all_tasks();
                } else if Some(self.state.task_selected)
                    == self.state.add_task_row_index(can_create_task)
                {
                    self.open_task_wizard();
                } else {
                    self.toggle_entity_detail();
                }
            }
            ViewId::Goals => {
                if can_create_goal && self.state.task_selected_is_goal_add_row() {
                    self.open_goal_wizard();
                } else {
                    self.toggle_goal_detail();
                }
            }
            ViewId::Agents => {
                if can_add_agent && self.state.agent_selected == self.state.sorted_agents().len() {
                    self.open_agent_wizard();
                } else {
                    self.toggle_entity_detail();
                }
            }
            ViewId::Logs => {
                if self.state.log_selected_index >= 0 {
                    self.state.detail = if self.state.detail == DetailPanel::Log {
                        DetailPanel::None
                    } else {
                        DetailPanel::Log
                    };
                }
            }
        }
    }

    pub(crate) fn toggle_entity_detail(&mut self) {
        self.state.detail = if self.state.detail_open() {
            DetailPanel::None
        } else {
            DetailPanel::Entity
        };
    }

    pub(crate) fn move_selection_up(&mut self) {
        let regions = self.state.regions(self.backend.can_create_task());
        let main_h = regions.main_h as usize;
        match self.state.view {
            ViewId::Goals if self.state.detail_open() => {
                self.state.goal_detail_scroll = self.state.goal_detail_scroll.saturating_sub(1);
            }
            ViewId::Goals => {
                self.state.goal_selected = self.state.goal_selected.saturating_sub(1);
                if self.state.goal_selected < self.state.goal_scroll {
                    self.state.goal_scroll = self.state.goal_selected;
                }
            }
            ViewId::Tasks => {
                self.state.task_selected = self.state.task_selected.saturating_sub(1);
                if self.state.task_selected < self.state.task_scroll {
                    self.state.task_scroll = self.state.task_selected;
                }
            }
            ViewId::Agents => {
                self.state.agent_selected = self.state.agent_selected.saturating_sub(1);
                if self.state.agent_selected < self.state.agent_scroll {
                    self.state.agent_scroll = self.state.agent_selected;
                }
            }
            ViewId::Logs => {
                let filtered = self.state.filtered_messages();
                if filtered.is_empty() {
                    return;
                }
                if self.state.log_selected_index < 0 {
                    // From tail: jump to the last message.
                    let last = filtered.len() as i64 - 1;
                    self.state.log_selected_index = last;
                    self.state.log_scroll_offset = (last - main_h as i64 + 2).max(0) as usize;
                } else {
                    self.state.log_selected_index = (self.state.log_selected_index - 1).max(0);
                    if (self.state.log_selected_index as usize) < self.state.log_scroll_offset {
                        self.state.log_scroll_offset = self.state.log_selected_index as usize;
                    }
                }
            }
        }
    }

    pub(crate) fn move_selection_down(&mut self) {
        let can_create_task = self.backend.can_create_task();
        let can_add_agent = self.backend.can_add_agent();
        let can_create_goal = self.backend.can_create_goal();
        let regions = self.state.regions(can_create_task);
        let main_h = regions.main_h as usize;
        match self.state.view {
            ViewId::Goals if self.state.detail_open() => {
                self.state.goal_detail_scroll += 1;
            }
            ViewId::Goals => {
                let max = self.state.goal_selection_max(can_create_goal);
                let next = (self.state.goal_selected + 1).min(max);
                self.state.goal_selected = next;
                if next >= self.state.goal_scroll + main_h {
                    self.state.goal_scroll = next + 1 - main_h;
                }
            }
            ViewId::Tasks => {
                let max = self.state.task_selection_max(can_create_task);
                let next = (self.state.task_selected + 1).min(max);
                self.state.task_selected = next;
                if next >= self.state.task_scroll + main_h {
                    self.state.task_scroll = next + 1 - main_h;
                }
            }
            ViewId::Agents => {
                let max = self.state.agent_selection_max(can_add_agent);
                let next = (self.state.agent_selected + 1).min(max);
                self.state.agent_selected = next;
                if next >= self.state.agent_scroll + main_h {
                    self.state.agent_scroll = next + 1 - main_h;
                }
            }
            ViewId::Logs => {
                let filtered = self.state.filtered_messages();
                if self.state.log_selected_index < 0 {
                    // Already at tail — stay there.
                    return;
                }
                let next = self.state.log_selected_index + 1;
                if next as usize >= filtered.len() {
                    // Past the last index returns to tail.
                    self.state.log_selected_index = -1;
                    self.state.log_scroll_offset = 0;
                } else {
                    self.state.log_selected_index = next;
                    self.state.log_scroll_offset = (next - main_h as i64 + 2).max(0) as usize;
                }
            }
        }
    }

    // ── Command mode ───────────────────────────────────────────────────────

    fn handle_command_key(&mut self, key: KeyEvent, now: i64) {
        let value = self.state.command_bar.input.value().to_string();
        let suggestions = self.suggestions(&value);
        // Any key but Enter means the user has moved on from the refusal —
        // editing, history, Tab or Esc. Enter is excluded because it is what
        // sets these lines, and `cmd_msg` clears them itself on every run.
        //
        // The broadcast arm goes with it: the confirm prompt *is* that
        // refusal, so leaving the arm behind would hide an armed broadcast
        // behind a bar that says nothing — one Enter from sending (gate S2,
        // frontend F2). Only the Enter that immediately follows the arming
        // Enter can confirm.
        if key.code != KeyCode::Enter {
            self.state.command_bar.retained_refusal.clear();
            self.disarm_broadcast();
        }

        match key.code {
            KeyCode::Esc => self.leave_command_mode(),
            KeyCode::Enter => {
                let trimmed = value.trim().to_string();
                if trimmed.is_empty() {
                    return;
                }
                let mut cmd_to_run = trimmed.clone();
                if let Some(selected) = suggestions.get(self.state.command_bar.suggestion_index) {
                    // A separator row carries no command. Inert for Enter and
                    // Tab alike — and above all it must not clear the input,
                    // which is what the category headers did.
                    if selected.is_separator() {
                        return;
                    }
                    // A row that knows its own text fills with it verbatim.
                    // Before the legacy path, because that path derives the
                    // command from the *displayed* text, which for an agent
                    // row is the unescaped name.
                    //
                    // Unless the fill would only add the trailing space the
                    // user is already past: on a **bare verb** whose own row
                    // is selected the fill adds only the trailing space and
                    // says nothing about what is missing, where executing it
                    // shows the verb's Usage line (§3.2, QA note). A recipient
                    // row always fills — including a finished name, whose
                    // space is the one the body goes after.
                    if let Some(insert) = selected.insert.clone()
                        && !(is_bare_verb(&trimmed) && insert.trim() == trimmed)
                    {
                        self.state.command_bar.input.reset(&insert);
                        self.state.command_bar.suggestion_index = 0;
                        return;
                    }
                    if selected.insert.is_none() {
                        let cmd_part = strip_args_hint(&selected.cmd).to_string();
                        if cmd_part.starts_with(&trimmed) || trimmed == "/" {
                            cmd_to_run = cmd_part.clone();
                        }
                        // A verb with subcommands fills the buffer instead of
                        // executing.
                        if selected.subs.is_some() && !cmd_part.contains(' ') {
                            self.state.command_bar.input.reset(&format!("{cmd_part} "));
                            self.state.command_bar.suggestion_index = 0;
                            return;
                        }
                    }
                }
                self.state.input_mode = InputMode::None;
                // The bar's edit state as the user submitted it, for the
                // verbs that run against it — `cmd_msg`'s broadcast arm.
                // Read before the clear below, which is a mutation like any
                // other.
                self.submitted_generation = self.state.command_bar.input.generation();
                self.state.command_bar.input.reset("");
                self.state.command_bar.suggestion_index = 0;
                self.state.command_bar.history.push(&cmd_to_run);
                self.execute_command(&cmd_to_run);
                // `/msg` refusals and the broadcast arm keep the bar open with
                // their text, so a recipient the user just chose — or an
                // ambiguity error asking for an id — does not have to be
                // retyped. History was pushed above either way, so `↑` works
                // too. Only `cmd_msg` sets this.
                if std::mem::take(&mut self.retain_command_bar) {
                    self.state.input_mode = InputMode::Command;
                    // The text **as typed**, not the trimmed command: `/msg
                    // Alice ` with no body is a refusal whose trailing space
                    // is what keeps the suggestion panel closed, and a
                    // reopened panel covers the refusal (QA repro B).
                    let retained = if trimmed == cmd_to_run {
                        &value
                    } else {
                        &cmd_to_run
                    };
                    self.state.command_bar.input.reset(retained);
                    // Putting the bar back is not an edit of it, so the
                    // generation goes back too: the arm this run may have just
                    // created was stamped with it, and anything the user now
                    // does to the bar moves past it.
                    self.state
                        .command_bar
                        .input
                        .restore_generation(self.submitted_generation);
                } else {
                    // Command mode was left, so any pending broadcast is
                    // disarmed — otherwise running some other verb in between
                    // would leave an arm that fires on the *first* Enter the
                    // next time that same text is typed.
                    self.disarm_broadcast();
                    self.state.command_bar.retained_refusal.clear();
                }
            }
            KeyCode::Tab => {
                if !suggestions.is_empty() {
                    if let Some(selected) = suggestions.get(self.state.command_bar.suggestion_index)
                    {
                        if selected.is_separator() {
                            return;
                        }
                        if let Some(insert) = selected.insert.clone() {
                            self.state.command_bar.input.reset(&insert);
                            self.state.command_bar.suggestion_index = 0;
                            return;
                        }
                        let cmd_part = strip_args_hint(&selected.cmd);
                        let filled = if selected.subs.is_some() {
                            format!("{cmd_part} ")
                        } else {
                            cmd_part.to_string()
                        };
                        self.state.command_bar.input.reset(&filled);
                        self.state.command_bar.suggestion_index = 0;
                    }
                } else if let Some(suffix) = resolve_completion(&value) {
                    self.state
                        .command_bar
                        .input
                        .reset(&format!("{value}{suffix}"));
                }
            }
            KeyCode::Up => {
                if !suggestions.is_empty() {
                    self.state.command_bar.suggestion_index =
                        self.state.command_bar.suggestion_index.saturating_sub(1);
                } else if let Some(prev) = self.state.command_bar.history.prev() {
                    self.state.command_bar.input.reset(&prev);
                }
            }
            KeyCode::Down => {
                if !suggestions.is_empty() {
                    self.state.command_bar.suggestion_index =
                        (self.state.command_bar.suggestion_index + 1).min(suggestions.len() - 1);
                } else {
                    let next = self.state.command_bar.history.next().unwrap_or_default();
                    self.state.command_bar.input.reset(&next);
                }
            }
            _ => {
                if self.state.command_bar.input.handle_key(key, now) {
                    self.state.command_bar.suggestion_index = 0;
                }
            }
        }
    }

    // ── Wizard mode ────────────────────────────────────────────────────────

    fn handle_wizard_key(&mut self, key: KeyEvent, now: i64) {
        let Some(wizard) = self.state.wizard.as_mut() else {
            self.state.input_mode = InputMode::None;
            return;
        };
        match wizard.handle_key(key, now) {
            WizardOutcome::Consumed | WizardOutcome::Ignored => {}
            WizardOutcome::Cancelled => self.cancel_wizard(),
            WizardOutcome::Complete(values) => self.submit_wizard(values),
            WizardOutcome::SuggestionSelected(value) => self.apply_name_suggestion(&value),
            WizardOutcome::PasteImage => self.paste_image(),
        }
    }

    // ── Pickers ────────────────────────────────────────────────────────────

    fn handle_agent_picker_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Up => self.picker_move(-1),
            KeyCode::Down => self.picker_move(1),
            KeyCode::Char(' ') => self.picker_toggle_cursor(),
            KeyCode::Char('a') | KeyCode::Char('A') => self.picker_select_all(),
            KeyCode::Enter => self.picker_confirm(),
            KeyCode::Esc => self.picker_cancel(),
            _ => {}
        }
    }

    fn handle_type_picker_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Up => self.type_picker_move(-1),
            KeyCode::Down => self.type_picker_move(1),
            KeyCode::Char(' ') => self.type_picker_toggle_cursor(),
            KeyCode::Char('a') | KeyCode::Char('A') => self.type_picker_toggle_all(),
            KeyCode::Char(preset @ '1'..='5') => self.type_picker_preset(preset),
            KeyCode::Enter => self.type_picker_confirm(),
            KeyCode::Esc => self.type_picker_cancel(),
            _ => {}
        }
    }

    // ── Named picker actions ───────────────────────────────────────────────

    /// Agent picker `Up`/`Down`: one step, wrapping at both ends. Only the
    /// sign of `delta` is read.
    pub(crate) fn picker_move(&mut self, delta: i32) {
        let total = self.state.agents.len();
        if total == 0 {
            return;
        }
        self.state.picker_cursor = if delta < 0 {
            if self.state.picker_cursor == 0 {
                total - 1
            } else {
                self.state.picker_cursor - 1
            }
        } else {
            (self.state.picker_cursor + 1) % total
        };
    }

    /// The wheel's picker movement: one step, **clamped** at both ends.
    ///
    /// The key path wraps (`Up` at the top goes to the bottom) and keeps
    /// doing so; a wheel flick that wraps bottom→top is disorienting, so the
    /// two differ deliberately (design §3.3.2).
    pub(crate) fn picker_move_clamped(&mut self, delta: i32) {
        let total = self.state.agents.len();
        if total == 0 {
            return;
        }
        self.state.picker_cursor = clamp_step(self.state.picker_cursor, delta, total);
    }

    /// [`Self::picker_move_clamped`] for the type picker.
    pub(crate) fn type_picker_move_clamped(&mut self, delta: i32) {
        self.state.picker_cursor = clamp_step(self.state.picker_cursor, delta, MsgType::ALL.len());
    }

    /// Agent picker `Space` — toggle the agent under the cursor.
    pub(crate) fn picker_toggle_cursor(&mut self) {
        self.picker_toggle_at(self.state.picker_cursor);
    }

    /// [`Self::picker_toggle_cursor`] for an arbitrary row.
    pub(crate) fn picker_toggle_at(&mut self, index: usize) {
        if let Some(agent) = self.state.agents.get(index) {
            let id = agent.id.clone();
            if self.state.picker_draft.contains(&id) {
                self.state.picker_draft.shift_remove(&id);
            } else {
                // From implicit-all this yields a single explicit selection,
                // not all-minus-one.
                self.state.picker_draft.insert(id);
            }
        }
    }

    /// Agent picker `a all` — an empty draft is the "every agent" value.
    pub(crate) fn picker_select_all(&mut self) {
        self.state.picker_draft.clear();
    }

    /// Agent picker `Enter`.
    pub(crate) fn picker_confirm(&mut self) {
        // An empty set is a legal confirm value meaning "no filter".
        self.state.log_agent_filter = self.state.picker_draft.clone();
        self.state.show_agent_picker = false;
    }

    /// Agent picker `Esc` — the draft is discarded.
    pub(crate) fn picker_cancel(&mut self) {
        self.state.show_agent_picker = false;
    }

    /// Type picker `Up`/`Down`; see [`Self::picker_move`].
    pub(crate) fn type_picker_move(&mut self, delta: i32) {
        let total = MsgType::ALL.len();
        self.state.picker_cursor = if delta < 0 {
            if self.state.picker_cursor == 0 {
                total - 1
            } else {
                self.state.picker_cursor - 1
            }
        } else {
            (self.state.picker_cursor + 1) % total
        };
    }

    /// Type picker `Space` — toggle the type under the cursor.
    pub(crate) fn type_picker_toggle_cursor(&mut self) {
        self.type_picker_toggle_at(self.state.picker_cursor);
    }

    /// [`Self::type_picker_toggle_cursor`] for an arbitrary row.
    pub(crate) fn type_picker_toggle_at(&mut self, index: usize) {
        // Ignore an out-of-range index rather than clamping it onto the last
        // type, matching `picker_toggle_at`'s `.get()`. The key path can only
        // pass a cursor that is already in range, but this is also a click
        // entry point now, and "toggled the wrong target" is the failure the
        // whole `ZoneId` design exists to prevent.
        let Some(&msg_type) = MsgType::ALL.get(index) else {
            return;
        };
        if self.state.type_picker_draft.contains(&msg_type) {
            self.state.type_picker_draft.shift_remove(&msg_type);
        } else {
            self.state.type_picker_draft.insert(msg_type);
        }
    }

    /// Type picker `a all` — all eight, or none if they are already all on.
    pub(crate) fn type_picker_toggle_all(&mut self) {
        if self.state.type_picker_draft.len() == MsgType::ALL.len() {
            self.state.type_picker_draft.clear();
        } else {
            self.state.type_picker_draft = MsgType::ALL.into_iter().collect();
        }
    }

    /// Type picker `1`..`5` — the preset that digit names.
    pub(crate) fn type_picker_preset(&mut self, preset: char) {
        let name = match preset {
            '1' => "all",
            '2' => "text",
            '3' => "tools",
            '4' => "errors",
            _ => "events",
        };
        self.state.type_picker_draft = preset_types(name);
    }

    /// Type picker `Enter`.
    pub(crate) fn type_picker_confirm(&mut self) {
        // An empty draft normalises to all 8 — unlike the agent picker.
        self.state.log_type_filter = if self.state.type_picker_draft.is_empty() {
            MsgType::ALL.into_iter().collect()
        } else {
            self.state.type_picker_draft.clone()
        };
        self.state.show_type_picker = false;
    }

    /// Type picker `Esc` — the draft is discarded.
    pub(crate) fn type_picker_cancel(&mut self) {
        self.state.show_type_picker = false;
    }

    /// Does the navigate-mode bar offer `R run` for the selection?
    pub fn can_run_selected(&self) -> bool {
        self.state.view == ViewId::Tasks
            && self
                .state
                .selected_task()
                .is_some_and(|t| is_runnable(t.status))
    }

    /// Is the selection a review task (drives `A approve` / `X reject`)?
    pub fn can_approve_selected(&self) -> bool {
        self.state.view == ViewId::Tasks
            && self
                .state
                .selected_task()
                .is_some_and(|t| t.status == TaskStatus::Review)
    }

    /// Emit a refusal message in the shared style.
    pub(crate) fn refuse(&mut self, text: &str) {
        self.emit(text, Token::Yellow, MsgType::System);
    }
}

impl crate::state::AppState {
    /// Goals view: is the selection the `+ add goal...` pseudo-row?
    pub fn task_selected_is_goal_add_row(&self) -> bool {
        self.goal_selected == self.sorted_goals().len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::{FakeBackend, make_agent, make_task};
    use crate::clock::TestClock;
    use orch_domain::AgentStatus;

    type TestApp = App<FakeBackend, TestClock>;

    fn app() -> TestApp {
        App::new(FakeBackend::new(), TestClock::new(0))
    }

    /// The named actions exist so that a caller which is not a key — the
    /// command bar, a mouse click — can perform exactly what the key performs.
    /// Each case asserts that: `key` and `action` leave `probe` equal, and
    /// that neither of them was a no-op against the untouched state.
    fn same<T: std::fmt::Debug + PartialEq>(
        setup: impl Fn(&mut TestApp),
        key: KeyCode,
        action: impl Fn(&mut TestApp),
        probe: impl Fn(&TestApp) -> T,
    ) {
        let mut pressed = app();
        let mut called = app();
        let mut untouched = app();
        for target in [&mut pressed, &mut called, &mut untouched] {
            setup(target);
        }
        pressed.handle_key(KeyEvent::new(key, KeyModifiers::NONE));
        action(&mut called);

        assert_eq!(
            probe(&pressed),
            probe(&called),
            "the key and the named action diverged"
        );
        assert_ne!(
            probe(&untouched),
            probe(&called),
            "neither path changed anything — the case proves nothing"
        );
    }

    fn logs(app: &mut TestApp) {
        app.state.view = ViewId::Logs;
    }

    fn agent_picker(app: &mut TestApp) {
        app.state.agents = vec![
            make_agent("agt_1", "alpha", AgentStatus::Idle),
            make_agent("agt_2", "beta", AgentStatus::Idle),
            make_agent("agt_3", "gamma", AgentStatus::Idle),
        ];
        app.state.show_agent_picker = true;
        app.state.picker_cursor = 0;
    }

    fn type_picker(app: &mut TestApp) {
        app.state.show_type_picker = true;
        app.state.picker_cursor = 0;
        app.state.type_picker_draft = [MsgType::Output].into_iter().collect();
    }

    #[test]
    fn request_quit_matches_q() {
        same(
            |_| {},
            KeyCode::Char('q'),
            TestApp::request_quit,
            |app| app.state.should_exit,
        );
    }

    #[test]
    fn close_detail_or_tail_matches_esc_for_both_of_its_branches() {
        same(
            |app| {
                app.state.detail = DetailPanel::Entity;
                app.state.goal_detail_scroll = 4;
            },
            KeyCode::Esc,
            TestApp::close_detail_or_tail,
            |app| (app.state.detail, app.state.goal_detail_scroll),
        );
        same(
            |app| {
                logs(app);
                app.state.log_selected_index = 3;
                app.state.log_scroll_offset = 2;
            },
            KeyCode::Esc,
            TestApp::close_detail_or_tail,
            |app| (app.state.log_selected_index, app.state.log_scroll_offset),
        );
    }

    #[test]
    fn toggle_detail_maximized_matches_uppercase_m() {
        same(
            |_| {},
            KeyCode::Char('M'),
            TestApp::toggle_detail_maximized,
            |app| app.state.detail_maximized,
        );
    }

    #[test]
    fn open_help_matches_question_mark() {
        same(
            |_| {},
            KeyCode::Char('?'),
            TestApp::open_help,
            |app| app.state.show_help,
        );
    }

    #[test]
    fn enter_command_mode_matches_slash() {
        same(
            |app| app.state.command_bar.suggestion_index = 7,
            KeyCode::Char('/'),
            TestApp::enter_command_mode,
            |app| {
                (
                    app.state.input_mode,
                    app.state.command_bar.input.value().to_string(),
                    app.state.command_bar.suggestion_index,
                )
            },
        );
    }

    #[test]
    fn open_agent_picker_matches_logs_a() {
        same(
            |app| {
                logs(app);
                app.state.log_agent_filter = ["agt_1".to_string()].into_iter().collect();
                app.state.picker_cursor = 5;
            },
            KeyCode::Char('a'),
            TestApp::open_agent_picker,
            |app| {
                (
                    app.state.show_agent_picker,
                    app.state.picker_cursor,
                    app.state.picker_draft.clone(),
                )
            },
        );
    }

    #[test]
    fn open_type_picker_matches_logs_f() {
        same(
            |app| {
                logs(app);
                app.state.log_type_filter = [MsgType::Error].into_iter().collect();
                app.state.picker_cursor = 5;
            },
            KeyCode::Char('f'),
            TestApp::open_type_picker,
            |app| {
                (
                    app.state.show_type_picker,
                    app.state.picker_cursor,
                    app.state.type_picker_draft.clone(),
                )
            },
        );
    }

    #[test]
    fn cycle_log_types_matches_logs_uppercase_f() {
        same(logs, KeyCode::Char('F'), TestApp::cycle_log_types, |app| {
            app.state.log_type_filter.clone()
        });
    }

    #[test]
    fn cycle_activity_filter_matches_f_off_the_logs_view_and_persists_once() {
        same(
            |_| {},
            KeyCode::Char('f'),
            TestApp::cycle_activity_filter,
            |app| (app.state.activity_filter.clone(), app.backend.calls.len()),
        );
    }

    #[test]
    fn open_new_wizard_for_view_matches_n() {
        same(
            |_| {},
            KeyCode::Char('n'),
            TestApp::open_new_wizard_for_view,
            |app| {
                (
                    app.state.input_mode,
                    app.state.wizard.as_ref().map(|w| w.kind.clone()),
                )
            },
        );
        same(
            |app| app.state.view = ViewId::Agents,
            KeyCode::Char('n'),
            TestApp::open_new_wizard_for_view,
            |app| app.state.wizard.as_ref().map(|w| w.kind.clone()),
        );
    }

    #[test]
    fn open_new_wizard_for_view_respects_a_backend_that_refuses() {
        let mut backend = FakeBackend::new();
        backend.can_create_task = false;
        let mut app = App::new(backend, TestClock::new(0));
        app.open_new_wizard_for_view();
        assert_eq!(app.state.input_mode, InputMode::None);
        assert!(app.state.wizard.is_none());
    }

    #[test]
    fn toggle_show_all_tasks_matches_tasks_s_and_the_show_all_row() {
        let setup = |app: &mut TestApp| {
            app.state.tasks = vec![make_task("tsk_1", "one", TaskStatus::Todo)];
            app.state.task_selected = 3;
            app.state.task_scroll = 2;
        };
        same(
            setup,
            KeyCode::Char('s'),
            TestApp::toggle_show_all_tasks,
            |app| {
                (
                    app.state.show_all_tasks,
                    app.state.task_selected,
                    app.state.task_scroll,
                )
            },
        );
    }

    #[test]
    fn switch_view_matches_the_tab_letters() {
        same(
            logs,
            KeyCode::Char('t'),
            |app| app.switch_view(ViewId::Tasks),
            |app| app.state.view,
        );
    }

    #[test]
    fn handle_enter_and_toggle_entity_detail_match_enter() {
        let setup = |app: &mut TestApp| {
            app.state.tasks = vec![make_task("tsk_1", "one", TaskStatus::Todo)];
        };
        same(setup, KeyCode::Enter, TestApp::handle_enter, |app| {
            app.state.detail
        });
        same(
            setup,
            KeyCode::Enter,
            TestApp::toggle_entity_detail,
            |app| app.state.detail,
        );
    }

    #[test]
    fn move_selection_matches_the_arrow_keys() {
        let setup = |app: &mut TestApp| {
            app.state.tasks = vec![
                make_task("tsk_1", "one", TaskStatus::Todo),
                make_task("tsk_2", "two", TaskStatus::Todo),
            ];
        };
        same(setup, KeyCode::Down, TestApp::move_selection_down, |app| {
            app.state.task_selected
        });
        same(
            |app| {
                setup(app);
                app.state.task_selected = 2;
                app.state.task_scroll = 2;
            },
            KeyCode::Up,
            TestApp::move_selection_up,
            |app| (app.state.task_selected, app.state.task_scroll),
        );
    }

    #[test]
    fn picker_move_matches_the_wrapping_arrow_keys() {
        same(
            agent_picker,
            KeyCode::Up,
            |app| app.picker_move(-1),
            |app| app.state.picker_cursor,
        );
        same(
            agent_picker,
            KeyCode::Down,
            |app| app.picker_move(1),
            |app| app.state.picker_cursor,
        );
    }

    #[test]
    fn picker_toggle_matches_space_and_addresses_an_arbitrary_row() {
        same(
            agent_picker,
            KeyCode::Char(' '),
            TestApp::picker_toggle_cursor,
            |app| app.state.picker_draft.clone(),
        );
        // `picker_toggle_at` is the row-addressed form the cursor form wraps.
        let mut app = app();
        agent_picker(&mut app);
        app.picker_toggle_at(2);
        assert_eq!(
            app.state.picker_draft.iter().collect::<Vec<_>>(),
            vec!["agt_3"]
        );
        app.picker_toggle_at(2);
        assert!(app.state.picker_draft.is_empty());
    }

    #[test]
    fn picker_select_all_matches_a() {
        same(
            |app| {
                agent_picker(app);
                app.state.picker_draft = ["agt_2".to_string()].into_iter().collect();
            },
            KeyCode::Char('a'),
            TestApp::picker_select_all,
            |app| app.state.picker_draft.clone(),
        );
    }

    #[test]
    fn picker_confirm_and_cancel_match_enter_and_esc() {
        same(
            |app| {
                agent_picker(app);
                app.state.picker_draft = ["agt_2".to_string()].into_iter().collect();
            },
            KeyCode::Enter,
            TestApp::picker_confirm,
            |app| {
                (
                    app.state.show_agent_picker,
                    app.state.log_agent_filter.clone(),
                )
            },
        );
        same(agent_picker, KeyCode::Esc, TestApp::picker_cancel, |app| {
            app.state.show_agent_picker
        });
    }

    #[test]
    fn type_picker_move_matches_the_wrapping_arrow_keys() {
        same(
            type_picker,
            KeyCode::Up,
            |app| app.type_picker_move(-1),
            |app| app.state.picker_cursor,
        );
        same(
            type_picker,
            KeyCode::Down,
            |app| app.type_picker_move(1),
            |app| app.state.picker_cursor,
        );
    }

    #[test]
    fn type_picker_toggle_matches_space_and_addresses_an_arbitrary_row() {
        same(
            type_picker,
            KeyCode::Char(' '),
            TestApp::type_picker_toggle_cursor,
            |app| app.state.type_picker_draft.clone(),
        );
        let mut app = app();
        type_picker(&mut app);
        app.type_picker_toggle_at(3);
        assert!(app.state.type_picker_draft.contains(&MsgType::ALL[3]));
    }

    #[test]
    fn type_picker_toggle_all_matches_a_in_both_directions() {
        same(
            type_picker,
            KeyCode::Char('a'),
            TestApp::type_picker_toggle_all,
            |app| app.state.type_picker_draft.clone(),
        );
        same(
            |app| {
                type_picker(app);
                app.state.type_picker_draft = MsgType::ALL.into_iter().collect();
            },
            KeyCode::Char('a'),
            TestApp::type_picker_toggle_all,
            |app| app.state.type_picker_draft.clone(),
        );
    }

    #[test]
    fn type_picker_preset_matches_the_digits() {
        // An empty draft equals no preset, so every digit is a real change.
        let empty_draft = |app: &mut TestApp| {
            type_picker(app);
            app.state.type_picker_draft.clear();
        };
        for digit in ['1', '2', '3', '4', '5'] {
            same(
                empty_draft,
                KeyCode::Char(digit),
                move |app| app.type_picker_preset(digit),
                |app| app.state.type_picker_draft.clone(),
            );
        }
    }

    #[test]
    fn type_picker_confirm_and_cancel_match_enter_and_esc() {
        same(
            type_picker,
            KeyCode::Enter,
            TestApp::type_picker_confirm,
            |app| {
                (
                    app.state.show_type_picker,
                    app.state.log_type_filter.clone(),
                )
            },
        );
        // An empty draft normalises to all eight on confirm, which is also the
        // default filter — so the probe carries the panel flag as well.
        same(
            |app| {
                type_picker(app);
                app.state.type_picker_draft.clear();
                app.state.log_type_filter = [MsgType::Error].into_iter().collect();
            },
            KeyCode::Enter,
            TestApp::type_picker_confirm,
            |app| {
                (
                    app.state.show_type_picker,
                    app.state.log_type_filter.clone(),
                )
            },
        );
        same(
            type_picker,
            KeyCode::Esc,
            TestApp::type_picker_cancel,
            |app| app.state.show_type_picker,
        );
    }
}
