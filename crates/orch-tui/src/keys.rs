//! The ordered key-dispatch chain (docs/parity/tui.md §2, PARITY.md §12.2).
//!
//! The order is load-bearing and the per-view overloads (`a` `g` `s` `c` `x`
//! `f`) are resolved here explicitly — getting one wrong destroys the wrong
//! entity.
//!
//! One deliberate fix (PARITY.md §14.8): **both** filter pickers swallow global
//! keys. In the original the type picker did not, so `q` quit through it.

use orch_domain::{GoalStatus, TaskStatus};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use crate::app::App;
use crate::backend::TuiBackend;
use crate::clock::Clock;
use crate::command_bar::{resolve_completion, resolve_suggestions};
use crate::messages::{MsgType, cycle_preset, preset_types};
use crate::state::{DetailPanel, InputMode, ViewId, is_runnable};
use crate::theme::Token;
use crate::wizard::engine::{WizardKind, WizardOutcome};

/// Strip a trailing ` [...]` args hint from a suggestion's command text.
fn strip_args_hint(cmd: &str) -> &str {
    match cmd.find(" [") {
        Some(idx) if cmd.ends_with(']') => &cmd[..idx],
        _ => cmd,
    }
}

impl<B: TuiBackend, C: Clock> App<B, C> {
    pub fn handle_key(&mut self, key: KeyEvent) {
        // Key *release* and *repeat* events exist under the kitty protocol;
        // only presses drive the UI.
        if key.kind != KeyEventKind::Press {
            return;
        }
        let now = self.now();

        // 1. Help overlay: `?`, `Esc` and F1 are swallowed; anything else
        //    dismisses the overlay *and* performs its normal action.
        if self.state.show_help {
            self.state.show_help = false;
            if matches!(
                key.code,
                KeyCode::Char('?') | KeyCode::Esc | KeyCode::F(1)
            ) {
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

        // 3. Ctrl+S / Cmd+S inside the NEW AGENT wizard opens the Agent Shop.
        if (key.modifiers.contains(KeyModifiers::CONTROL)
            || key.modifiers.contains(KeyModifiers::SUPER))
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
        let can_create_task = self.backend.can_create_task();
        let can_add_agent = self.backend.can_add_agent();
        let can_create_goal = self.backend.can_create_goal();

        match key.code {
            // `q` is tested first, so it can never be typed in normal mode.
            KeyCode::Char('q') | KeyCode::Char('Q') => {
                self.state.should_exit = true;
                return;
            }
            // `Esc` never quits.
            KeyCode::Esc => {
                if detail_open {
                    self.state.detail = DetailPanel::None;
                    self.state.goal_detail_scroll = 0;
                } else if view == ViewId::Logs && self.state.log_selected_index >= 0 {
                    self.state.log_selected_index = -1;
                    self.state.log_scroll_offset = 0;
                }
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
                self.state.detail_maximized = !self.state.detail_maximized;
                return;
            }
            KeyCode::Char('?') | KeyCode::F(1) => {
                self.state.show_help = true;
                return;
            }
            KeyCode::Char('/') if !detail_open => {
                self.state.input_mode = InputMode::Command;
                self.state.command_bar.input.reset("/");
                self.state.command_bar.suggestion_index = 0;
                return;
            }
            _ => {}
        }

        // Logs-only filter keys.
        if view == ViewId::Logs && !detail_open {
            match key.code {
                KeyCode::Char('a') | KeyCode::Char('A') => {
                    self.state.show_agent_picker = true;
                    self.state.picker_cursor = 0;
                    self.state.picker_draft = self.state.log_agent_filter.clone();
                    return;
                }
                KeyCode::Char('f') => {
                    self.state.show_type_picker = true;
                    self.state.picker_cursor = 0;
                    self.state.type_picker_draft = self.state.log_type_filter.clone();
                    return;
                }
                KeyCode::Char('F') => {
                    let next = cycle_preset(&self.state.log_type_filter);
                    self.state.log_type_filter = preset_types(next);
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
            let next = cycle_preset(&self.state.activity_filter);
            self.state.activity_filter = preset_types(next);
            let _ = self.backend.save_activity_filter(next);
            return;
        }

        // `n` — the new-entity wizard for this view.
        if matches!(key.code, KeyCode::Char('n') | KeyCode::Char('N')) && !detail_open {
            match view {
                ViewId::Tasks if can_create_task => self.open_task_wizard(),
                ViewId::Agents if can_add_agent => self.open_agent_wizard(),
                ViewId::Goals if can_create_goal => self.open_goal_wizard(),
                _ => {}
            }
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
                    self.state.show_all_tasks = !self.state.show_all_tasks;
                    self.state.task_selected = 0;
                    self.state.task_scroll = 0;
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

    fn switch_view(&mut self, view: ViewId) {
        self.state.view = view;
    }

    fn handle_enter(&mut self) {
        let can_create_task = self.backend.can_create_task();
        let can_add_agent = self.backend.can_add_agent();
        let can_create_goal = self.backend.can_create_goal();
        match self.state.view {
            ViewId::Tasks => {
                if Some(self.state.task_selected) == self.state.show_all_row_index() {
                    self.state.show_all_tasks = !self.state.show_all_tasks;
                    self.state.task_selected = 0;
                    self.state.task_scroll = 0;
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

    fn toggle_entity_detail(&mut self) {
        self.state.detail = if self.state.detail_open() {
            DetailPanel::None
        } else {
            DetailPanel::Entity
        };
    }

    fn move_selection_up(&mut self) {
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
                    self.state.log_scroll_offset =
                        (last - main_h as i64 + 2).max(0) as usize;
                } else {
                    self.state.log_selected_index = (self.state.log_selected_index - 1).max(0);
                    if (self.state.log_selected_index as usize) < self.state.log_scroll_offset {
                        self.state.log_scroll_offset = self.state.log_selected_index as usize;
                    }
                }
            }
        }
    }

    fn move_selection_down(&mut self) {
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
        let suggestions = resolve_suggestions(&value);

        match key.code {
            KeyCode::Esc => {
                self.state.input_mode = InputMode::None;
                self.state.command_bar.input.reset("");
                self.state.command_bar.history.reset();
            }
            KeyCode::Enter => {
                let trimmed = value.trim().to_string();
                if trimmed.is_empty() {
                    return;
                }
                let mut cmd_to_run = trimmed.clone();
                if let Some(selected) = suggestions.get(self.state.command_bar.suggestion_index) {
                    let cmd_part = strip_args_hint(&selected.cmd).to_string();
                    if cmd_part.starts_with(&trimmed) || trimmed == "/" {
                        cmd_to_run = cmd_part.clone();
                    }
                    // A verb with subcommands fills the buffer instead of
                    // executing.
                    if selected.subs.is_some() && !cmd_part.contains(' ') {
                        self.state
                            .command_bar
                            .input
                            .reset(&format!("{cmd_part} "));
                        self.state.command_bar.suggestion_index = 0;
                        return;
                    }
                }
                self.state.input_mode = InputMode::None;
                self.state.command_bar.input.reset("");
                self.state.command_bar.suggestion_index = 0;
                self.state.command_bar.history.push(&cmd_to_run);
                self.execute_command(&cmd_to_run);
            }
            KeyCode::Tab => {
                if !suggestions.is_empty() {
                    if let Some(selected) =
                        suggestions.get(self.state.command_bar.suggestion_index)
                    {
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
        let total = self.state.agents.len();
        match key.code {
            KeyCode::Up => {
                if total > 0 {
                    self.state.picker_cursor = if self.state.picker_cursor == 0 {
                        total - 1
                    } else {
                        self.state.picker_cursor - 1
                    };
                }
            }
            KeyCode::Down => {
                if total > 0 {
                    self.state.picker_cursor = (self.state.picker_cursor + 1) % total;
                }
            }
            KeyCode::Char(' ') => {
                if let Some(agent) = self.state.agents.get(self.state.picker_cursor) {
                    let id = agent.id.clone();
                    if self.state.picker_draft.contains(&id) {
                        self.state.picker_draft.shift_remove(&id);
                    } else {
                        // From implicit-all this yields a single explicit
                        // selection, not all-minus-one.
                        self.state.picker_draft.insert(id);
                    }
                }
            }
            KeyCode::Char('a') | KeyCode::Char('A') => {
                self.state.picker_draft.clear();
            }
            KeyCode::Enter => {
                // An empty set is a legal confirm value meaning "no filter".
                self.state.log_agent_filter = self.state.picker_draft.clone();
                self.state.show_agent_picker = false;
            }
            KeyCode::Esc => {
                self.state.show_agent_picker = false;
            }
            _ => {}
        }
    }

    fn handle_type_picker_key(&mut self, key: KeyEvent) {
        let total = MsgType::ALL.len();
        match key.code {
            KeyCode::Up => {
                self.state.picker_cursor = if self.state.picker_cursor == 0 {
                    total - 1
                } else {
                    self.state.picker_cursor - 1
                };
            }
            KeyCode::Down => {
                self.state.picker_cursor = (self.state.picker_cursor + 1) % total;
            }
            KeyCode::Char(' ') => {
                let msg_type = MsgType::ALL[self.state.picker_cursor.min(total - 1)];
                if self.state.type_picker_draft.contains(&msg_type) {
                    self.state.type_picker_draft.shift_remove(&msg_type);
                } else {
                    self.state.type_picker_draft.insert(msg_type);
                }
            }
            KeyCode::Char('a') | KeyCode::Char('A') => {
                if self.state.type_picker_draft.len() == total {
                    self.state.type_picker_draft.clear();
                } else {
                    self.state.type_picker_draft = MsgType::ALL.into_iter().collect();
                }
            }
            KeyCode::Char(preset @ '1'..='5') => {
                let name = match preset {
                    '1' => "all",
                    '2' => "text",
                    '3' => "tools",
                    '4' => "errors",
                    _ => "events",
                };
                self.state.type_picker_draft = preset_types(name);
            }
            KeyCode::Enter => {
                // An empty draft normalises to all 8 — unlike the agent picker.
                self.state.log_type_filter = if self.state.type_picker_draft.is_empty() {
                    MsgType::ALL.into_iter().collect()
                } else {
                    self.state.type_picker_draft.clone()
                };
                self.state.show_type_picker = false;
            }
            KeyCode::Esc => {
                self.state.show_type_picker = false;
            }
            _ => {}
        }
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
