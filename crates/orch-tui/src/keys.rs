//! The ordered key-dispatch chain.
//!
//! The order is load-bearing and the per-view overloads (`a` `g` `s` `c` `x`
//! `f`) are resolved here explicitly — getting one wrong destroys the wrong
//! entity.
//!
//! One deliberate fix: **both** filter pickers swallow global
//! keys, so `q` does not quit through either.

use orch_domain::{GoalStatus, TaskStatus};
use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use crate::app::App;
use crate::backend::TuiBackend;
use crate::clock::Clock;
use crate::command_bar::{SuggestionCtx, resolve_completion, resolve_suggestions_in};
use crate::messages::{MsgType, cycle_preset, preset_types};
use crate::navigation::{NavAction, ScrollMetrics, ScrollMode, ScrollState, navigate};
use crate::state::{
    AgentSection, CopyHelpContext, CopyOrigin, DetailPanel, Focus, GoalSection, InputMode,
    KEYWORD_MAX_CHARS, LogSection, TaskSection, ViewId, is_runnable, sanitize_keyword,
};
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

fn selection_rows_identity(view: ViewId) -> bool {
    matches!(view, ViewId::Goals | ViewId::Logs)
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
        if let UiEvent::Terminal(Event::Mouse(mouse)) = event
            && self.guard_surface_mouse(*mouse)
        {
            return;
        }
        if matches!(event, UiEvent::Tick) {
            self.state.normalize_focus();
        }
        if let UiEvent::Terminal(Event::Resize(width, height)) = event {
            self.resize(*width, *height);
        }
        if matches!(event, UiEvent::Terminal(Event::Mouse(_)))
            && (!self.state.mouse.enabled || !self.state.mouse.hits.is_valid())
        {
            return;
        }
        if let UiEvent::Terminal(Event::Key(key)) = event
            && self.page_wizard_options(*key)
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
        // Contextual F1 sits after the two surface owners and before legacy
        // input modes; opening it never recreates either underlying session.
        if let UiEvent::Terminal(Event::Key(key)) = event {
            if self.state.show_help && self.state.copy_help.is_some() {
                self.handle_copy_help_key(*key);
                return;
            }
            if key.code == KeyCode::F(1) && !self.state.show_help {
                self.state.sync_field_origin();
                if let Some(origin) = self.state.active_copy_origin() {
                    self.state.copy_help = Some(CopyHelpContext {
                        origin,
                        scroll_row: 0,
                    });
                    self.state.show_help = true;
                    self.invalidate_hits();
                    return;
                }
            }
        }
        match event {
            UiEvent::Terminal(Event::Key(key)) => self.handle_key_internal(*key),
            UiEvent::Terminal(Event::Mouse(mouse)) => self.handle_mouse_internal(*mouse),
            UiEvent::Terminal(Event::Paste(text)) => self.handle_paste_internal(text),
            UiEvent::Terminal(Event::Resize(_, _)) => {}
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
        // See [`is_meta`] for why meta excludes Cmd.
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
            InputMode::Keyword => {
                self.handle_keyword_key(key);
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

        if self.copy_focused_key(key) {
            return;
        }
        // The keyword filter (D1): Ctrl+K opens the prompt and Ctrl+U clears
        // the keyword, on an entity list only. `/` stays the command bar.
        if key.modifiers == KeyModifiers::CONTROL
            && matches!(view, ViewId::Goals | ViewId::Tasks | ViewId::Agents)
            && self.state.focus == Focus::MainList
        {
            match key.code {
                KeyCode::Char('k' | 'K') => {
                    self.open_keyword_prompt();
                    return;
                }
                KeyCode::Char('u' | 'U') => {
                    self.clear_keyword();
                    return;
                }
                _ => {}
            }
        }
        // Letters are entity actions only with plain/Shift modifiers. Copy
        // shortcuts were handled above; Ctrl+C is deliberately unbound.
        if !(key.modifiers - KeyModifiers::SHIFT).is_empty() {
            return;
        }
        match key.code {
            KeyCode::Tab | KeyCode::BackTab => {
                let ring = self.state.focus_ring();
                let current = ring
                    .iter()
                    .position(|f| *f == self.state.focus)
                    .unwrap_or(0);
                let backwards =
                    key.code == KeyCode::BackTab || key.modifiers.contains(KeyModifiers::SHIFT);
                let next = if backwards {
                    (current + ring.len() - 1) % ring.len()
                } else {
                    (current + 1) % ring.len()
                };
                self.state.set_focus(ring[next]);
                return;
            }
            KeyCode::Char('i') if view == ViewId::Logs && !detail_open => {
                if let Some(id) = self.state.selected_log().map(|m| m.id) {
                    // Inspect the current entry without following incoming messages.
                    if self.state.log_selected_index < 0 {
                        self.state.log_selected_index =
                            self.state.filtered_messages().len().saturating_sub(1) as i64;
                        self.state.log_selected_id = Some(id);
                    }
                    self.state.detail = DetailPanel::Log;
                    self.state.set_focus(Focus::LogDetail(LogSection::Metadata));
                }
                return;
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.navigate_focus(NavAction::Up, false);
                return;
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.navigate_focus(NavAction::Down, false);
                return;
            }
            KeyCode::PageUp => {
                self.navigate_focus(NavAction::PageUp, false);
                return;
            }
            KeyCode::PageDown => {
                self.navigate_focus(NavAction::PageDown, false);
                return;
            }
            KeyCode::Home => {
                self.navigate_focus(NavAction::Home, false);
                return;
            }
            KeyCode::End => {
                self.navigate_focus(NavAction::End, false);
                return;
            }
            KeyCode::Enter if self.state.focus != Focus::MainList || view == ViewId::Logs => {
                self.open_focused_viewer();
                return;
            }
            _ => {}
        }
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
            KeyCode::Char('M') if view != ViewId::Logs || detail_open => {
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

        // The list sort: `o` next field, `O` other direction, `0` default
        // order. Each list has its own; none of them touches grouping.
        if matches!(view, ViewId::Goals | ViewId::Tasks | ViewId::Agents) {
            match key.code {
                KeyCode::Char('o') => {
                    self.cycle_sort();
                    return;
                }
                KeyCode::Char('O') => {
                    self.reverse_sort();
                    return;
                }
                KeyCode::Char('0') => {
                    self.reset_sort();
                    return;
                }
                _ => {}
            }
        }

        // A refresh moved the cursor off a vanished entity onto one that is
        // not on screen yet: entity actions wait for that frame (D1).
        if self.state.entity_actions_deferred
            && self.state.focus == Focus::MainList
            && is_entity_action_key(view, key.code)
        {
            return;
        }

        if view == ViewId::Agents {
            match key.code {
                // `b`: group *by*. Free in every view; `g`, the Tasks
                // grouping key, means "goals tab" here.
                KeyCode::Char('b') | KeyCode::Char('B') => {
                    self.toggle_agent_group_by();
                    return;
                }
                _ => {}
            }
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
        if matches!(key.code, KeyCode::Char('n') | KeyCode::Char('N'))
            && self.state.focus == Focus::MainList
        {
            self.open_new_wizard_for_view();
            return;
        }

        // Goal-view actions.
        if view == ViewId::Goals && self.state.focus == Focus::MainList {
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
        if view == ViewId::Tasks && self.state.focus == Focus::MainList {
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
        if view == ViewId::Agents && self.state.focus == Focus::MainList {
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
                KeyCode::Right => {
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
            self.state.detail_maximized = false;
            self.state.set_focus(Focus::MainList);
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
                self.state.suggestion_scroll = 0;
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
            // Control characters (the newline of a multi-line paste, an
            // `ESC`) are dropped and the rest cut to the keyword's bound.
            InputMode::Keyword => {
                let room = KEYWORD_MAX_CHARS
                    .saturating_sub(self.state.keyword(self.state.view).chars().count());
                let text = sanitize_keyword(&normalized, room);
                if !text.is_empty() {
                    self.edit_keyword(|input, now| input.insert_str(&text, now));
                }
            }
            InputMode::None => {}
        }
    }

    /// `M` — maximize the detail panel, or restore the split.
    pub(crate) fn toggle_detail_maximized(&mut self) {
        self.state.detail_maximized = !self.state.detail_maximized;
        if self.state.detail_maximized {
            let focus = self
                .state
                .focus_ring()
                .into_iter()
                .find(|f| *f != Focus::MainList)
                .unwrap_or(Focus::MainList);
            self.state.set_focus(focus);
        } else {
            self.state.set_focus(Focus::MainList);
        }
    }

    /// `?` / `F1`.
    pub(crate) fn open_help(&mut self) {
        self.state.copy_help = None;
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
        self.state.suggestion_scroll = 0;
    }

    /// The suggestion rows for an input, with the agent list `/msg` needs.
    ///
    /// The borrow is a statement-scoped temporary, so `&mut self.state` uses
    /// after it still compile.
    pub(crate) fn suggestions(&self, value: &str) -> Vec<crate::command_bar::Suggestion> {
        let order = self.state.sorted_agents_unfiltered();
        resolve_suggestions_in(
            value,
            &SuggestionCtx {
                agents: &self.state.agents,
                order: &order,
            },
        )
    }

    // ── Keyword filter (D1, R3–R5) ─────────────────────────────────────────

    /// Ctrl+K: the visible entity list's keyword prompt, over its current
    /// keyword with the cursor at the end, so a second Ctrl+K refines rather
    /// than retypes.
    pub(crate) fn open_keyword_prompt(&mut self) {
        let view = self.state.view;
        let Some(input) = self.state.keyword_input_mut(view) else {
            return;
        };
        input.cursor_mut().move_to_end();
        self.state.input_mode = InputMode::Keyword;
    }

    /// `Enter` in the prompt: close it and keep filtering.
    pub(crate) fn close_keyword_prompt(&mut self) {
        if self.state.input_mode == InputMode::Keyword {
            self.state.input_mode = InputMode::None;
        }
    }

    /// `Esc` in the prompt, Ctrl+U on the list, or a click on the keyword
    /// chip: drop the visible list's keyword and close the prompt.
    pub(crate) fn clear_keyword(&mut self) {
        if !self.state.keyword(self.state.view).is_empty() {
            self.edit_keyword(|input, _| input.reset(""));
        }
        self.close_keyword_prompt();
    }

    /// Edit the visible list's keyword, keeping every cursor on its entity.
    ///
    /// The selection is captured from the order on screen before the edit
    /// and found again after it (D1), so a row that still matches keeps the
    /// cursor; one the keyword hid leaves it at the nearest surviving row,
    /// and entity actions wait until that row has been drawn. The bound is
    /// enforced here too, because a yank (`Ctrl+Y`) inserts text no key
    /// check saw.
    fn edit_keyword(&mut self, edit: impl FnOnce(&mut crate::text::TextInput, i64)) {
        let view = self.state.view;
        let now = self.now();
        let can_create = self.can_create_task();
        let prior = self.state.capture_list_selection(can_create);
        let Some(input) = self.state.keyword_input_mut(view) else {
            return;
        };
        let before = input.value().to_string();
        edit(input, now);
        if input.value().chars().count() > KEYWORD_MAX_CHARS {
            let bounded = sanitize_keyword(input.value(), KEYWORD_MAX_CHARS);
            input.reset(&bounded);
        }
        // A cursor move is not a filter change: it must not resample the
        // Elapsed order (D1).
        if input.value() == before {
            return;
        }
        self.state.reconcile_list_selection(&prior, now, can_create);
        self.state.clamp_all(can_create);
    }

    /// Every key in the prompt. Typing filters live; `Enter` keeps the
    /// keyword, `Esc` clears it; `↑↓` move the list's cursor so a match can
    /// be picked without leaving. Nothing here reaches a list or global
    /// binding — `q` is a letter of the keyword, not quit.
    fn handle_keyword_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.clear_keyword();
                return;
            }
            KeyCode::Enter => {
                self.close_keyword_prompt();
                return;
            }
            KeyCode::Up => {
                self.navigate_focus(NavAction::Up, false);
                return;
            }
            KeyCode::Down => {
                self.navigate_focus(NavAction::Down, false);
                return;
            }
            KeyCode::Char(ch)
                if !key.modifiers.contains(KeyModifiers::CONTROL) && !is_meta(key) =>
            {
                let full = self.state.keyword(self.state.view).chars().count() >= KEYWORD_MAX_CHARS;
                if ch.is_control() || full {
                    return;
                }
            }
            _ => {}
        }
        self.edit_keyword(|input, now| {
            input.handle_key(key, now);
        });
    }

    /// `/` — the command bar, pre-filled with its own prefix.
    pub(crate) fn enter_command_mode(&mut self) {
        self.state.input_mode = InputMode::Command;
        self.state.command_bar.input.reset("/");
        self.state.command_bar.suggestion_index = 0;
        self.state.suggestion_scroll = 0;
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

    /// `o` in the active list.
    pub(crate) fn cycle_sort(&mut self) {
        match self.state.view {
            ViewId::Goals => self.change_goal_sort(self.state.goal_sort.cycled()),
            ViewId::Tasks => self.change_task_sort(self.state.task_sort.cycled()),
            ViewId::Agents => self.cycle_agent_sort(),
            ViewId::Logs => {}
        }
    }

    /// `O` in the active list.
    pub(crate) fn reverse_sort(&mut self) {
        match self.state.view {
            ViewId::Goals => self.change_goal_sort(self.state.goal_sort.reversed()),
            ViewId::Tasks => self.change_task_sort(self.state.task_sort.reversed()),
            ViewId::Agents => self.reverse_agent_sort(),
            ViewId::Logs => {}
        }
    }

    /// `0` in the active list: its default order, and nothing else.
    pub(crate) fn reset_sort(&mut self) {
        match self.state.view {
            ViewId::Goals => self.change_goal_sort(crate::state::GoalSort::default()),
            ViewId::Tasks => self.change_task_sort(crate::state::TaskSort::default()),
            ViewId::Agents => self.reset_agent_sort(),
            ViewId::Logs => {}
        }
    }

    /// A column header click: that field ascending, or the other direction
    /// if it is the active field already.
    pub(crate) fn click_sort_header(&mut self, column: crate::state::SortColumn) {
        use crate::state::SortColumn;
        match column {
            SortColumn::Goal(field) => self.change_goal_sort(self.state.goal_sort.clicked(field)),
            SortColumn::Task(field) => self.change_task_sort(self.state.task_sort.clicked(field)),
            SortColumn::Agent(field) => {
                self.change_agent_sort(self.state.agent_sort.clicked(field));
            }
        }
    }

    fn change_goal_sort(&mut self, sort: crate::state::GoalSort) {
        let now = self.now();
        let can_create = self.can_create_task();
        self.state.set_goal_sort(sort, now, can_create);
        self.state.clamp_all(can_create);
    }

    fn change_task_sort(&mut self, sort: crate::state::TaskSort) {
        let now = self.now();
        let can_create = self.can_create_task();
        self.state.set_task_sort(sort, now, can_create);
        self.state.clamp_all(can_create);
    }

    pub(crate) fn cycle_agent_sort(&mut self) {
        let mut sort = self.state.agent_sort;
        sort.field = sort.field.next();
        self.change_agent_sort(sort);
    }

    pub(crate) fn reverse_agent_sort(&mut self) {
        let mut sort = self.state.agent_sort;
        sort.descending = !sort.descending;
        self.change_agent_sort(sort);
    }

    pub(crate) fn reset_agent_sort(&mut self) {
        self.change_agent_sort(crate::state::AgentSort::default());
    }

    fn change_agent_sort(&mut self, sort: crate::state::AgentSort) {
        self.state.set_agent_sort(sort);
        self.state.clamp_all(self.backend.can_add_agent());
    }

    /// `b` — By team ⇄ By node. The sort is left as it is: it applies inside
    /// whichever sections are drawn.
    pub(crate) fn toggle_agent_group_by(&mut self) {
        let next = self.state.agent_group_by.next();
        self.state.set_agent_group_by(next);
        self.state.clamp_all(self.backend.can_add_agent());
    }

    /// Logs `a` — the agent filter picker, drafting the live filter.
    pub(crate) fn open_agent_picker(&mut self) {
        self.state.show_agent_picker = true;
        self.state.picker_cursor = 0;
        self.state.picker_scroll = 0;
        self.state.picker_draft = self.state.log_agent_filter.clone();
    }

    /// Logs `f` — the message-type filter picker.
    pub(crate) fn open_type_picker(&mut self) {
        self.state.show_type_picker = true;
        self.state.picker_cursor = 0;
        self.state.picker_scroll = 0;
        self.state.type_picker_draft = self.state.log_type_filter.clone();
    }

    /// Logs `F` — the next type preset, with no picker.
    pub(crate) fn cycle_log_types(&mut self) {
        self.state.log_selected_id = self.state.selected_log().map(|m| m.id);
        let next = cycle_preset(&self.state.log_type_filter);
        self.state.log_type_filter = preset_types(next);
        self.state.normalize_focus();
    }

    /// `f` / `F` off the logs view — the next activity-feed preset, persisted.
    pub(crate) fn cycle_activity_filter(&mut self) {
        let next = cycle_preset(&self.state.activity_filter);
        self.state.activity_filter = preset_types(next);
        self.state.normalize_focus();
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
        self.state.set_focus(Focus::MainList);
        self.state.normalize_focus();
    }

    pub(crate) fn handle_enter(&mut self) {
        if self.state.entity_actions_deferred {
            return;
        }
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
        if self.state.detail_open() {
            self.focus_initial_detail();
        }
    }

    pub(crate) fn toggle_entity_detail(&mut self) {
        self.state.detail = if self.state.detail_open() {
            DetailPanel::None
        } else {
            DetailPanel::Entity
        };
        if self.state.detail_open() {
            self.focus_initial_detail();
        } else {
            self.state.set_focus(Focus::MainList);
        }
    }

    pub(crate) fn focus_initial_detail(&mut self) {
        let focus = match self.state.view {
            ViewId::Goals => Focus::GoalDetail(GoalSection::Overview),
            ViewId::Tasks => Focus::TaskDetail(TaskSection::Overview),
            ViewId::Agents => Focus::AgentDetail(AgentSection::Overview),
            ViewId::Logs => Focus::LogDetail(LogSection::Metadata),
        };
        self.state.set_focus(focus);
    }
    pub(crate) fn open_focused_viewer(&mut self) {
        let Some(mut document) = self.state.focused_text() else {
            return;
        };
        document.captured_at_ms = self.now();
        if self
            .state
            .register_viewer_origin(document.source.clone())
            .is_err()
        {
            return;
        }
        self.state.viewer = Some(crate::widgets::text_viewer::TextViewer::new(document));
        self.invalidate_hits();
    }
    fn copy_focused_key(&mut self, key: KeyEvent) -> bool {
        use crate::backend::CopyTarget;
        let target = match (key.code, key.modifiers) {
            (KeyCode::Char('y' | 'Y'), KeyModifiers::CONTROL) => CopyTarget::Auto,
            (KeyCode::Char('c' | 'C'), m) if m == KeyModifiers::CONTROL | KeyModifiers::ALT => {
                CopyTarget::Terminal
            }
            _ => return false,
        };
        let Some(document) = self.state.focused_text() else {
            return true;
        };
        self.state.sync_field_origin();
        let Some(origin @ CopyOrigin::Field { .. }) = self.state.active_copy_origin() else {
            return true;
        };
        let accepted = if matches!(
            document.completeness,
            crate::widgets::text_viewer::Completeness::Complete
        ) {
            self.backend.copy_text_via(&document.body, target)
        } else {
            Err("Body unavailable".into())
        };
        self.state
            .register_copy_request(origin, target, accepted, self.now());
        true
    }
    fn handle_copy_help_key(&mut self, key: KeyEvent) {
        if self
            .state
            .copy_help
            .as_ref()
            .is_some_and(|h| !self.state.copy_origin_is_live(&h.origin))
        {
            self.state.show_help = false;
            self.state.copy_help = None;
            self.invalidate_hits();
            return;
        }
        let action = match key.code {
            KeyCode::Esc | KeyCode::F(1) | KeyCode::Char('?') => {
                self.state.show_help = false;
                self.state.copy_help = None;
                self.invalidate_hits();
                return;
            }
            KeyCode::Up => NavAction::Up,
            KeyCode::Down => NavAction::Down,
            KeyCode::PageUp => NavAction::PageUp,
            KeyCode::PageDown => NavAction::PageDown,
            KeyCode::Home => NavAction::Home,
            KeyCode::End => NavAction::End,
            _ => return,
        };
        self.navigate_copy_help(action);
        self.invalidate_hits();
    }
    pub(crate) fn navigate_copy_help(&mut self, action: NavAction) {
        let rows = crate::widgets::help_overlay::contextual_rows(
            &self.state,
            self.state.width.saturating_sub(2) as usize,
        )
        .len();
        if let Some(help) = self.state.copy_help.as_mut() {
            let mut state = ScrollState {
                selected: None,
                offset: help.scroll_row,
            };
            navigate(
                &mut state,
                action,
                ScrollMetrics {
                    items: 0,
                    display_rows: rows,
                    viewport_rows: self.state.height.saturating_sub(9) as usize,
                    selection_rows: &[],
                    mode: ScrollMode::Text,
                },
            );
            help.scroll_row = state.offset;
        }
    }
    /// Keys reuse the last drawn body metric; a layout invalidation clears it.
    /// Before the first draw, the baseline split provides the initial window.
    pub(crate) fn navigate_focus(&mut self, action: NavAction, wheel: bool) {
        let focus = self.state.focus;
        let key = self.state.pane_key(focus);
        let published = self
            .state
            .mouse
            .hits
            .metrics
            .panes
            .iter()
            .find(|m| m.key == key)
            .cloned();
        let fallback = self.state.regions(self.backend.can_create_task());
        let h = published.as_ref().map_or_else(
            || {
                if focus == Focus::MainList {
                    fallback.main_h.saturating_sub(match self.state.view {
                        ViewId::Goals => 0,
                        ViewId::Tasks => 0,
                        ViewId::Agents => 1,
                        ViewId::Logs => 2,
                    }) as usize
                } else {
                    fallback.feed_h.saturating_sub(2) as usize
                }
            },
            |m| m.body.height as usize,
        );
        let (mut scroll, items, display_rows, selection_rows, mode) = match focus {
            Focus::MainList => {
                let (selected, offset, count, map) = match self.state.view {
                    ViewId::Goals => (
                        Some(self.state.goal_selected),
                        self.state.goal_scroll,
                        self.state.sorted_goals().len()
                            + usize::from(self.backend.can_create_goal()),
                        Vec::new(),
                    ),
                    ViewId::Tasks => {
                        let rows = self.state.task_rows(self.backend.can_create_task());
                        let map: Vec<_> = rows
                            .iter()
                            .enumerate()
                            .filter_map(|(i, r)| {
                                matches!(
                                    r,
                                    crate::state::TaskRow::Task { .. }
                                        | crate::state::TaskRow::ShowAll { .. }
                                        | crate::state::TaskRow::AddTask { .. }
                                )
                                .then_some(i)
                            })
                            .collect();
                        (
                            Some(self.state.task_selected),
                            self.state.task_scroll,
                            rows.len(),
                            map,
                        )
                    }
                    ViewId::Agents => {
                        let rows = self.state.agent_rows(self.backend.can_add_agent());
                        let map: Vec<_> = rows
                            .iter()
                            .enumerate()
                            .filter_map(|(i, r)| {
                                matches!(
                                    r,
                                    crate::state::AgentRow::Agent { .. }
                                        | crate::state::AgentRow::AddAgent { .. }
                                )
                                .then_some(i)
                            })
                            .collect();
                        (
                            Some(self.state.agent_selected),
                            self.state.agent_scroll,
                            rows.len(),
                            map,
                        )
                    }
                    ViewId::Logs => {
                        let n = self.state.filtered_messages().len();
                        let tail = self.state.log_selected_index < 0;
                        let selected = if tail {
                            n.checked_sub(1)
                        } else {
                            usize::try_from(self.state.log_selected_index).ok()
                        };
                        let offset = if tail {
                            n.saturating_sub(h)
                        } else {
                            self.state.log_scroll_offset
                        };
                        (selected, offset, n, Vec::new())
                    }
                };
                let items = if selection_rows_identity(self.state.view) {
                    count
                } else {
                    map.len()
                };
                (
                    ScrollState { selected, offset },
                    items,
                    count,
                    map,
                    ScrollMode::Selectable,
                )
            }
            Focus::Activity => {
                let rows = self.state.activity_messages();
                let n = rows.len();
                let selected = if self.state.activity_follow_tail {
                    n.checked_sub(1)
                } else {
                    rows.iter().position(|i| {
                        Some(self.state.messages[*i].id) == self.state.activity_selected
                    })
                };
                let offset = if self.state.activity_follow_tail {
                    n.saturating_sub(h)
                } else {
                    self.state.activity_scroll
                };
                (
                    ScrollState { selected, offset },
                    n,
                    n,
                    Vec::new(),
                    ScrollMode::Selectable,
                )
            }
            _ => {
                let text = crate::views::detail::section_text(&self.state, focus);
                let list = matches!(
                    focus,
                    Focus::GoalDetail(GoalSection::Tasks)
                        | Focus::TaskDetail(
                            TaskSection::Activity | TaskSection::Attachments | TaskSection::Files
                        )
                );
                let rows = published.as_ref().map_or_else(
                    || {
                        text.iter()
                            .map(|s| {
                                crate::views::detail::wrap_rows(s, self.state.width as usize).len()
                            })
                            .sum()
                    },
                    |m| m.display_rows,
                );
                (
                    if focus == Focus::TaskDetail(TaskSection::Activity) {
                        self.state.task_activity_scroll(&key, h)
                    } else {
                        self.state.pane_scroll(&key)
                    },
                    text.len(),
                    rows,
                    Vec::new(),
                    if list {
                        ScrollMode::Selectable
                    } else {
                        ScrollMode::Text
                    },
                )
            }
        };
        navigate(
            &mut scroll,
            action,
            ScrollMetrics {
                items,
                display_rows,
                viewport_rows: h,
                selection_rows: &selection_rows,
                mode,
            },
        );
        match focus {
            Focus::MainList => match self.state.view {
                ViewId::Goals => {
                    self.state.goal_selected = scroll.selected.unwrap_or(0);
                    self.state.goal_scroll = scroll.offset;
                }
                ViewId::Tasks => {
                    self.state.task_selected = scroll.selected.unwrap_or(0);
                    self.state.task_scroll = scroll.offset;
                }
                ViewId::Agents => {
                    self.state.agent_selected = scroll.selected.unwrap_or(0);
                    self.state.agent_scroll = scroll.offset;
                }
                ViewId::Logs => {
                    let tail = crate::follow::resumes_tail(action, &scroll, items, h, wheel);
                    self.state.log_selected_index = if tail {
                        -1
                    } else {
                        scroll.selected.map_or(-1, |i| i as i64)
                    };
                    self.state.log_scroll_offset = if tail { 0 } else { scroll.offset };
                    self.state.log_selected_id = usize::try_from(self.state.log_selected_index)
                        .ok()
                        .and_then(|i| self.state.filtered_messages().get(i).copied())
                        .map(|i| self.state.messages[i].id);
                }
            },
            Focus::Activity => {
                self.state.activity_follow_tail =
                    crate::follow::resumes_tail(action, &scroll, items, h, wheel);
                let rows = self.state.activity_messages();
                self.state.activity_selected = scroll
                    .selected
                    .and_then(|i| rows.get(i))
                    .map(|i| self.state.messages[*i].id);
                self.state.activity_scroll = scroll.offset;
                self.state.activity_last_position = scroll.selected.unwrap_or(0);
            }
            _ => {
                if focus == Focus::TaskDetail(TaskSection::Activity)
                    && crate::follow::resumes_tail(action, &scroll, items, h, wheel)
                {
                    scroll.selected = None;
                }
                self.state.set_pane_scroll(key, scroll);
            }
        }
        self.state.sync_field_origin();
    }
    pub(crate) fn move_selection_up(&mut self) {
        self.navigate_focus(NavAction::Up, false);
    }
    pub(crate) fn move_selection_down(&mut self) {
        self.navigate_focus(NavAction::Down, false);
    }

    fn page_wizard_options(&mut self, key: KeyEvent) -> bool {
        if self.state.viewer.is_some()
            || self.state.show_help
            || self.state.show_agent_picker
            || self.state.show_type_picker
            || self.state.wizard_sessions.pending_leave.is_some()
            || self.state.input_mode != InputMode::Wizard
        {
            return false;
        }
        let Some(wizard) = self.state.wizard.as_ref() else {
            return false;
        };
        use crate::wizard::engine::StepKind;
        if !key.modifiers.is_empty() {
            return false;
        }
        let suggestions = wizard.step().kind == StepKind::Text;
        if !matches!(
            wizard.step().kind,
            StepKind::Select | StepKind::Multiselect | StepKind::Text
        ) {
            return false;
        }
        // A text input retains Home/End cursor semantics. Only its browse
        // list takes page keys, and a rendered copy rail takes precedence.
        if suggestions
            && self.state.mouse.hits.zones().iter().any(|z| {
                matches!(
                    z.kind,
                    crate::hit::ZoneKind::TextAction(
                        crate::hit::TextAction::CopyAll
                            | crate::hit::TextAction::CopySelection
                            | crate::hit::TextAction::CopyTerminal
                            | crate::hit::TextAction::CopyNative
                    )
                )
            })
        {
            return false;
        }
        let action = match key.code {
            KeyCode::Home if !suggestions => NavAction::Home,
            KeyCode::End if !suggestions => NavAction::End,
            KeyCode::PageUp => NavAction::PageUp,
            KeyCode::PageDown => NavAction::PageDown,
            _ => return false,
        };
        let h = self.option_page_rows(crate::hit::ScrollRegion::WizardOptions);
        let (total, current) = if suggestions {
            (
                wizard.filtered_suggestions().len(),
                wizard.suggestion_index(),
            )
        } else {
            (wizard.options().len(), wizard.select_index())
        };
        if total == 0 {
            return false;
        }
        let mut scroll = ScrollState {
            selected: Some(current),
            offset: current.saturating_sub(h.saturating_sub(1)),
        };
        navigate(
            &mut scroll,
            action,
            ScrollMetrics {
                items: total,
                display_rows: total,
                viewport_rows: h,
                selection_rows: &[],
                mode: ScrollMode::Selectable,
            },
        );
        if let Some(target) = scroll.selected
            && let Some(w) = self.state.wizard.as_mut()
        {
            for _ in 0..target.abs_diff(current) {
                let _ = w.handle_key(
                    KeyEvent::new(
                        if target < current {
                            KeyCode::Up
                        } else {
                            KeyCode::Down
                        },
                        KeyModifiers::NONE,
                    ),
                    self.clock.now_ms(),
                );
            }
        }
        self.invalidate_hits();
        true
    }
    pub(crate) fn option_page_rows(&self, region: crate::hit::ScrollRegion) -> usize {
        use crate::hit::{ScrollRegion, ZoneKind};
        let mut rows = Vec::new();
        for z in self.state.mouse.hits.zones() {
            if matches!(
                (&z.kind, region),
                (ZoneKind::PickerRow { .. }, ScrollRegion::AgentPicker)
                    | (ZoneKind::TypePickerRow { .. }, ScrollRegion::TypePicker)
                    | (
                        ZoneKind::WizardOption { .. } | ZoneKind::WizardSuggestion { .. },
                        ScrollRegion::WizardOptions
                    )
            ) && !rows.contains(&z.rect.y)
            {
                rows.push(z.rect.y);
            }
        }
        if !rows.is_empty() {
            return rows.len();
        }
        self.state
            .mouse
            .hits
            .zones()
            .iter()
            .find(|z| z.kind == ZoneKind::Scroll(region))
            .map_or(0, |z| z.rect.height as usize)
    }
    fn page_picker(&mut self, action: NavAction, types: bool) {
        let region = if types {
            crate::hit::ScrollRegion::TypePicker
        } else {
            crate::hit::ScrollRegion::AgentPicker
        };
        let n = if types {
            MsgType::ALL.len()
        } else {
            self.state.agents.len()
        };
        let mut scroll = ScrollState {
            selected: Some(self.state.picker_cursor),
            offset: self.state.picker_scroll,
        };
        navigate(
            &mut scroll,
            action,
            ScrollMetrics {
                items: n,
                display_rows: n,
                viewport_rows: self.option_page_rows(region),
                selection_rows: &[],
                mode: ScrollMode::Selectable,
            },
        );
        self.state.picker_cursor = scroll.selected.unwrap_or(0);
        self.state.picker_scroll = scroll.offset;
    }
    pub(crate) fn navigate_suggestions(&mut self, action: NavAction) {
        let suggestions = self.suggestions(self.state.command_bar.input.value());
        let rows: Vec<_> = suggestions
            .iter()
            .enumerate()
            .filter_map(|(i, s)| (!s.is_separator()).then_some(i))
            .collect();
        let current = rows
            .iter()
            .position(|i| *i == self.state.command_bar.suggestion_index)
            .unwrap_or(0);
        let h = self
            .state
            .mouse
            .hits
            .zones()
            .iter()
            .find(|z| {
                z.kind == crate::hit::ZoneKind::Scroll(crate::hit::ScrollRegion::CommandSuggestions)
            })
            .map_or(0, |z| z.rect.height as usize);
        let mut scroll = ScrollState {
            selected: Some(current),
            offset: self.state.suggestion_scroll,
        };
        navigate(
            &mut scroll,
            action,
            ScrollMetrics {
                items: rows.len(),
                display_rows: suggestions.len(),
                viewport_rows: h,
                selection_rows: &rows,
                mode: ScrollMode::Selectable,
            },
        );
        self.state.command_bar.suggestion_index = scroll
            .selected
            .and_then(|i| rows.get(i))
            .copied()
            .unwrap_or(0);
        self.state.suggestion_scroll = scroll.offset;
    }

    // ── Command mode ───────────────────────────────────────────────────────

    fn handle_command_key(&mut self, key: KeyEvent, now: i64) {
        let value = self.state.command_bar.input.value().to_string();
        let suggestions = self.suggestions(&value);
        if !suggestions.is_empty() {
            let action = match key.code {
                KeyCode::Up => Some(NavAction::Up),
                KeyCode::Down => Some(NavAction::Down),
                KeyCode::PageUp => Some(NavAction::PageUp),
                KeyCode::PageDown => Some(NavAction::PageDown),
                KeyCode::Home if key.modifiers.is_empty() => Some(NavAction::Home),
                KeyCode::End if key.modifiers.is_empty() => Some(NavAction::End),
                _ => None,
            };
            if let Some(action) = action {
                self.navigate_suggestions(action);
                self.state.command_bar.retained_refusal.clear();
                self.disarm_broadcast();
                return;
            }
        }
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
                        self.state.suggestion_scroll = 0;
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
                            self.state.suggestion_scroll = 0;
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
                self.state.suggestion_scroll = 0;
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
                            self.state.suggestion_scroll = 0;
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
                        self.state.suggestion_scroll = 0;
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
                    self.state.suggestion_scroll = 0;
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
            KeyCode::Home => {
                self.page_picker(NavAction::Home, false);
                return;
            }
            KeyCode::End => {
                self.page_picker(NavAction::End, false);
                return;
            }
            KeyCode::PageUp => {
                self.page_picker(NavAction::PageUp, false);
                return;
            }
            KeyCode::PageDown => {
                self.page_picker(NavAction::PageDown, false);
                return;
            }
            _ => {}
        }
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
            KeyCode::Home => {
                self.page_picker(NavAction::Home, true);
                return;
            }
            KeyCode::End => {
                self.page_picker(NavAction::End, true);
                return;
            }
            KeyCode::PageUp => {
                self.page_picker(NavAction::PageUp, true);
                return;
            }
            KeyCode::PageDown => {
                self.page_picker(NavAction::PageDown, true);
                return;
            }
            _ => {}
        }
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
        self.state.log_selected_id = self.state.selected_log().map(|m| m.id);
        self.state.log_agent_filter = self.state.picker_draft.clone();
        self.state.normalize_focus();
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
        self.state.log_selected_id = self.state.selected_log().map(|m| m.id);
        self.state.log_type_filter = if self.state.type_picker_draft.is_empty() {
            MsgType::ALL.into_iter().collect()
        } else {
            self.state.type_picker_draft.clone()
        };
        self.state.show_type_picker = false;
        self.state.normalize_focus();
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

/// The single-key entity actions of each list, as dispatched in normal mode.
/// Navigation, sorting, grouping and show-all are not entity actions.
fn is_entity_action_key(view: ViewId, code: KeyCode) -> bool {
    let KeyCode::Char(ch) = code else {
        return false;
    };
    match view {
        ViewId::Goals => matches!(ch.to_ascii_lowercase(), 'e' | 'd' | 'c' | 'x' | 'p'),
        ViewId::Tasks => matches!(ch.to_ascii_lowercase(), 'a' | 'x' | 'c' | 'e' | 'd' | 'r'),
        ViewId::Agents => matches!(ch.to_ascii_lowercase(), 'e' | 's' | 'd' | 'u' | 'm'),
        ViewId::Logs => false,
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
