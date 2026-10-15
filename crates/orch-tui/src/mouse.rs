//! The mouse dispatch chain (design §3.3, PARITY.md §12.11).
//!
//! Written to be diffed against [`crate::keys`]'s five ordered stages, because
//! the same hazard applies: a click on a target labelled `A` must do what
//! *that label* means, not what the key chain would decide `a` means in this
//! view. So a zone names an action and this module calls it — it never
//! synthesizes a `KeyEvent`. The one exception is `Wizard::handle_key`, the
//! engine's sole entry point, where nothing is overloaded.
//!
//! | # | Key chain | Mouse chain |
//! |---|---|---|
//! | 0 | *(none)* | `F2` toggles capture before every other stage (handled in `keys`, since it is a key) |
//! | 1 | help overlay swallows `?`/`Esc`/F1, anything else dismisses **and acts** | inside the box: nothing; outside: dismiss, and nothing else |
//! | 2 | a picker swallows every global key | only picker zones act; everything else is swallowed |
//! | 3 | Ctrl+S opens the agent shop | no mouse equivalent |
//! | 4 | `input_mode` takes the key | Command: suggestions, the bar's verbs. Wizard: wizard zones and the command-bar row |
//! | 5 | normal mode | the full zone table |
//!
//! Pure over `(AppState, backend, clock)`, exactly like `handle_key`: no
//! terminal I/O, and the capture toggle is expressed as `state.mouse.enabled`
//! for the runner to reconcile.

use orch_domain::{GoalStatus, TaskStatus};
use ratatui::crossterm::event::{
    KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};

use crate::app::App;
use crate::backend::TuiBackend;
use crate::clock::Clock;
use crate::command_bar::resolve_suggestions;
use crate::hit::{
    ChromeId, FooterAction, PseudoRow, RowPart, ScrollRegion, VerbId, Zone, ZoneId, ZoneKind,
};
use crate::messages::MsgType;
use crate::state::{ArmedClick, DetailPanel, InputMode, ViewId};
use crate::theme::Token;
use crate::wizard::engine::{StepKind, WizardOutcome};

/// Two clicks on the same target inside this window are a double click.
pub const DOUBLE_CLICK_MS: i64 = 400;

/// How long `Q quit` stays armed after the first click.
pub const QUIT_ARM_MS: i64 = 2_000;

/// Selection steps the wheel may spend per frame (§3.3.2). A trackpad flick
/// delivers dozens of notches in one drain; the frame is the only place the
/// user sees a result, so the rest are discarded there.
pub const WHEEL_MAX_STEPS_PER_FRAME: u8 = 3;

impl<B: TuiBackend, C: Clock> App<B, C> {
    pub fn handle_mouse(&mut self, event: MouseEvent) {
        if !self.state.mouse.enabled {
            return;
        }
        let (x, y) = (event.column, event.row);
        match event.kind {
            MouseEventKind::Down(MouseButton::Left) => self.mouse_down(x, y),
            MouseEventKind::Up(MouseButton::Left) => self.mouse_up(x, y),
            MouseEventKind::ScrollUp => self.wheel(x, y, -1),
            MouseEventKind::ScrollDown => self.wheel(x, y, 1),
            // `Drag`/`Moved` need 1002/1003, which we never enable, and are
            // ignored defensively. Right and middle buttons are consumed and
            // ignored: macOS reports Ctrl+left *as* a right click, so any
            // right binding would fire on a Ctrl-click. Horizontal scrolling
            // has no state in `AppState` to move.
            _ => {}
        }
    }

    /// A press arms a click; it never acts. Firing on the release is what
    /// gives drag-off-to-cancel, and it closes the legacy-encoding hole: in
    /// X10 a release carries no button identity and crossterm reports `Left`
    /// when it does not know, so a right-button release could otherwise fire
    /// a left action.
    fn mouse_down(&mut self, x: u16, y: u16) {
        self.state.mouse.armed = None;
        let Some(zone) = self.resolve_click(x, y) else {
            return;
        };
        let now = self.now();
        let secondary = self
            .state
            .mouse
            .last_click
            .as_ref()
            .is_some_and(|(id, at)| *id == zone.id && now - at <= DOUBLE_CLICK_MS);
        self.state.mouse.armed = Some(ArmedClick {
            id: zone.id,
            kind: zone.kind,
            secondary,
            at_ms: now,
        });
    }

    fn mouse_up(&mut self, x: u16, y: u16) {
        let Some(armed) = self.state.mouse.armed.take() else {
            return;
        };
        let Some(zone) = self.resolve_click(x, y) else {
            return;
        };
        // Identity, not position: a tick-driven refresh between the press and
        // the release can put a different entity on this row, and firing then
        // would act on the wrong one. Cancelling is the only safe answer.
        if zone.id != armed.id {
            return;
        }
        if armed.secondary {
            // A third click is a fresh primary, not a toggle-closed.
            self.state.mouse.last_click = None;
            self.fire_secondary(&zone.kind);
        } else {
            self.state.mouse.last_click = Some((armed.id, armed.at_ms));
            self.fire_primary(&zone.kind);
        }
    }

    /// Resolve a coordinate to a zone that is allowed to act right now.
    fn resolve_click(&self, x: u16, y: u16) -> Option<Zone> {
        // The help overlay is drawn before the command bar and the toasts, so
        // at a footer cell the topmost zone is a `FooterHint` sitting on top
        // of `HelpOverlayOutside`. Taking the topmost there would have the
        // stage swallow the click and leave the overlay up — which is exactly
        // what a user clicking `? help` again saw. While the overlay is up,
        // only its own two zones can answer.
        if self.state.show_help {
            let chrome = self.state.mouse.hits.resolve_chrome(x, y)?;
            return Some(Zone {
                rect: ratatui::layout::Rect::new(x, y, 1, 1),
                kind: ZoneKind::Chrome(chrome),
                id: ZoneId::Chrome(chrome),
            });
        }
        let zone = self.state.mouse.hits.resolve(x, y)?.clone();
        if self.suppressed(&zone, y) || !self.stage_allows(&zone.kind) {
            return None;
        }
        Some(zone)
    }

    /// The command-bar row is dead for a moment after one of its hints fires:
    /// the hint set is computed from the selection's status
    /// (`widgets/command_bar.rs:70-80`), so approving a task makes `A`/`X`
    /// vanish and everything to their right shift left — a habitual second
    /// click would land on `D delete` or `X reject`. The scope is the row,
    /// not the cell, because the reflow moves every hint right of the one
    /// that fired.
    fn suppressed(&self, zone: &Zone, y: u16) -> bool {
        let Some(until) = self.state.mouse.footer_suppressed_until else {
            return false;
        };
        if self.now() >= until {
            return false;
        }
        let command_bar_row = self.state.height.saturating_sub(1);
        y == command_bar_row || matches!(zone.kind, ZoneKind::FooterHint(_))
    }

    /// Which zones the active modal lets through (§3.3.4, §3.3.5).
    ///
    /// The governing rule for the "outside" cases: only an overlay holding no
    /// user-authored draft dismisses on an outside click. A picker, a wizard
    /// and a half-typed command all hold one, so they swallow and offer a
    /// labelled cancel verb instead — nothing is ever lost by surprise.
    fn stage_allows(&self, kind: &ZoneKind) -> bool {
        if self.state.show_help {
            return matches!(kind, ZoneKind::Chrome(ChromeId::HelpOverlayOutside));
        }
        if self.state.show_agent_picker || self.state.show_type_picker {
            return matches!(
                kind,
                ZoneKind::PickerRow { .. }
                    | ZoneKind::TypePickerRow { .. }
                    | ZoneKind::Scroll(ScrollRegion::AgentPicker | ScrollRegion::TypePicker)
            ) || matches!(
                kind,
                ZoneKind::Verb(
                    VerbId::PickerToggle
                        | VerbId::PickerAll
                        | VerbId::PickerConfirm
                        | VerbId::PickerCancel
                        | VerbId::PickerPreset(_)
                )
            );
        }
        match self.state.input_mode {
            InputMode::Command => {
                matches!(
                    kind,
                    ZoneKind::SuggestionRow { .. }
                        | ZoneKind::CommandGhost
                        | ZoneKind::Scroll(ScrollRegion::CommandSuggestions)
                ) || matches!(
                    kind,
                    ZoneKind::Verb(
                        VerbId::CommandExec | VerbId::CommandComplete | VerbId::CommandCancel
                    )
                )
            }
            InputMode::Wizard => {
                matches!(
                    kind,
                    ZoneKind::WizardOption { .. }
                        | ZoneKind::WizardSuggestion { .. }
                        | ZoneKind::FooterHint(_)
                        | ZoneKind::Scroll(
                            ScrollRegion::WizardOptions | ScrollRegion::WizardTextarea
                        )
                ) || matches!(
                    kind,
                    ZoneKind::Verb(
                        VerbId::WizardConfirm | VerbId::WizardToggle | VerbId::WizardBack
                    )
                )
            }
            InputMode::None => true,
        }
    }

    // ── Firing ─────────────────────────────────────────────────────────────

    fn fire_primary(&mut self, kind: &ZoneKind) {
        match kind {
            ZoneKind::Tab(view) => {
                // A pill click with a detail open closes it and then switches,
                // rather than being inertly swallowed as the key path is
                // (`keys.rs:320`). This is the only place a mouse gesture
                // composes two actions; it collapses the `Esc` then `T` a
                // keyboard user already types.
                if self.state.detail_open() {
                    self.close_detail_or_tail();
                }
                self.switch_view(*view);
                self.invalidate_hits();
            }
            // Changing the selection reflows the footer: its hint *set* is
            // conditional on the selected entity's status
            // (`widgets/command_bar.rs`), so selecting a Review task makes
            // `A approve`/`X reject` appear and `D delete` vanish. Until the
            // next draw those footer zones describe a selection that is no
            // longer current, and a second click in the same drain could fire
            // a `D delete` the footer never offered for this task.
            //
            // Suppressing the footer row is the fix, **not** invalidating the
            // map. Invalidating would also drop the second click of a row
            // double-click when both arrive in one write — which is how a
            // real double-click reaches us over tmux and the `orch web`
            // websocket — so it would trade this narrow hazard for a feature.
            // The reflow is confined to one row, and that row already has a
            // mechanism for exactly this (§3.3.3).
            ZoneKind::TaskRow { selection } => {
                self.state.task_selected = *selection;
                self.suppress_footer();
            }
            ZoneKind::AgentRow { selection } => {
                self.state.agent_selected = *selection;
                self.suppress_footer();
            }
            ZoneKind::GoalRow { selection } => {
                self.state.goal_selected = *selection;
                self.suppress_footer();
            }
            ZoneKind::LogRow { filtered_index } => self.select_log_row(*filtered_index),
            ZoneKind::LogAgentChip { index } => {
                // The one action with no single-key equivalent: the keyboard
                // route is picker → Space → Enter. It mutates one filter field
                // with the picker's own semantics and is instantly reversible.
                if let Some(agent) = self.state.agents.get(*index) {
                    let id = agent.id.clone();
                    if self.state.log_agent_filter.contains(&id) {
                        self.state.log_agent_filter.shift_remove(&id);
                    } else {
                        self.state.log_agent_filter.insert(id);
                    }
                    // This changes the *length* of `filtered_messages`, so
                    // every `LogRow` zone on screen now names a different
                    // message. Without this, a row click later in the same
                    // drain would select the message that used to be drawn
                    // there.
                    self.invalidate_hits();
                }
            }
            // A checkbox toggles the row it is on **and** takes the cursor
            // there, so `Space` or the footer's `Space toggle` next acts on
            // the row the user just touched rather than on wherever the
            // cursor happened to be left.
            ZoneKind::PickerRow { index, part } => {
                if *part == RowPart::Checkbox {
                    self.picker_toggle_at(*index);
                }
                self.state.picker_cursor = *index;
            }
            ZoneKind::TypePickerRow { index, part } => {
                if *part == RowPart::Checkbox {
                    self.type_picker_toggle_at(*index);
                }
                self.state.picker_cursor = *index;
            }
            ZoneKind::SuggestionRow { index } => self.state.command_bar.suggestion_index = *index,
            ZoneKind::WizardOption { index, part } => self.wizard_option_primary(*index, *part),
            ZoneKind::WizardSuggestion { value } => {
                let value = value.clone();
                self.apply_name_suggestion(&value);
                self.invalidate_hits();
            }
            ZoneKind::CommandGhost => self.accept_command_completion(),
            ZoneKind::Pseudo(row) => self.fire_pseudo(*row),
            ZoneKind::Verb(verb) => self.fire_verb(*verb),
            ZoneKind::FooterHint(action) => self.fire_footer(*action),
            ZoneKind::UndoRow { index } => self.undo_deletion_at(*index),
            ZoneKind::Chrome(ChromeId::HelpOverlayOutside) => {
                self.state.show_help = false;
                self.invalidate_hits();
            }
            // Inside the overlay, and the wheel-only regions: nothing.
            ZoneKind::Chrome(ChromeId::HelpOverlayInside) | ZoneKind::Scroll(_) => {}
        }
    }

    fn fire_secondary(&mut self, kind: &ZoneKind) {
        match kind {
            ZoneKind::TaskRow { selection } => {
                self.state.task_selected = *selection;
                self.handle_enter();
                self.invalidate_hits();
            }
            ZoneKind::AgentRow { selection } => {
                self.state.agent_selected = *selection;
                self.handle_enter();
                self.invalidate_hits();
            }
            ZoneKind::GoalRow { selection } => {
                self.state.goal_selected = *selection;
                self.toggle_goal_detail();
                self.invalidate_hits();
            }
            ZoneKind::LogRow { filtered_index } => {
                self.select_log_row(*filtered_index);
                self.state.detail = if self.state.detail == DetailPanel::Log {
                    DetailPanel::None
                } else {
                    DetailPanel::Log
                };
                self.invalidate_hits();
            }
            ZoneKind::PickerRow { index, part } if *part == RowPart::Label => {
                self.picker_toggle_at(*index)
            }
            ZoneKind::TypePickerRow { index, part } if *part == RowPart::Label => {
                self.type_picker_toggle_at(*index)
            }
            ZoneKind::SuggestionRow { index } => {
                self.state.command_bar.suggestion_index = *index;
                self.execute_selected_suggestion();
                self.invalidate_hits();
            }
            ZoneKind::WizardOption { index, part } => self.wizard_option_secondary(*index, *part),
            // Everything else has no secondary action: a button acts once, and
            // a second click on it does the same thing again.
            other => self.fire_primary(other),
        }
    }

    fn fire_pseudo(&mut self, row: PseudoRow) {
        match row {
            PseudoRow::ShowAll => self.toggle_show_all_tasks(),
            PseudoRow::AddTask => self.open_task_wizard(),
            PseudoRow::AddAgent => self.open_agent_wizard(),
            PseudoRow::AddGoal => self.open_goal_wizard(),
            PseudoRow::RunNudge => {
                if let Some(task) = self.state.selected_task().cloned() {
                    self.run_task_by_id(&task.id);
                }
            }
        }
        self.invalidate_hits();
    }

    fn fire_verb(&mut self, verb: VerbId) {
        match verb {
            VerbId::LogsTail => {
                self.state.log_selected_index = -1;
                self.state.log_scroll_offset = 0;
            }
            VerbId::OpenAgentPicker => self.open_agent_picker(),
            VerbId::OpenTypePicker => self.open_type_picker(),
            VerbId::CycleLogTypes => self.cycle_log_types(),
            VerbId::CycleActivityFilter => self.cycle_activity_filter(),
            VerbId::PickerToggle => {
                if self.state.show_type_picker {
                    self.type_picker_toggle_cursor();
                } else {
                    self.picker_toggle_cursor();
                }
            }
            VerbId::PickerAll => {
                if self.state.show_type_picker {
                    self.type_picker_toggle_all();
                } else {
                    self.picker_select_all();
                }
            }
            VerbId::PickerConfirm => {
                if self.state.show_type_picker {
                    self.type_picker_confirm();
                } else {
                    self.picker_confirm();
                }
            }
            VerbId::PickerCancel => {
                if self.state.show_type_picker {
                    self.type_picker_cancel();
                } else {
                    self.picker_cancel();
                }
            }
            VerbId::PickerPreset(digit) => self.type_picker_preset(digit),
            // `Tab`, not `Enter`: on a `Textarea` step plain Enter inserts a
            // newline (`wizard/engine.rs`), so clicking the last step's
            // `Ctrl+Enter/Tab confirm` hint did nothing at all and a
            // mouse-only user could not finish creating a task or a goal.
            // Tab confirms on every step kind — text, textarea, select and
            // multiselect — which is why every one of those hints names it.
            VerbId::WizardConfirm => self.wizard_key(KeyCode::Tab),
            VerbId::WizardToggle => self.wizard_key(KeyCode::Char(' ')),
            VerbId::WizardBack => self.wizard_key(KeyCode::Esc),
            VerbId::CommandExec => self.execute_selected_suggestion(),
            VerbId::CommandComplete => self.accept_command_completion(),
            VerbId::CommandCancel => {
                self.state.input_mode = InputMode::None;
                self.state.command_bar.input.reset("");
                self.state.command_bar.history.reset();
            }
            VerbId::DetailGrow => self.state.grow_detail(),
            VerbId::DetailShrink => self.state.grow_list(),
            VerbId::DetailMax => self.toggle_detail_maximized(),
        }
        self.invalidate_hits();
    }

    /// Kill clicks in the command-bar row for one double-click window,
    /// because its hints have just been recomputed under the pointer.
    fn suppress_footer(&mut self) {
        self.state.mouse.footer_suppressed_until = Some(self.now() + DOUBLE_CLICK_MS);
    }

    fn fire_footer(&mut self, action: FooterAction) {
        // The row goes dead for a double-click window, because the hint set is
        // status-conditional and everything right of the hint that fired
        // shifts left (§3.3.3).
        //
        // `Q quit` is the exception, and it has to be: its whole design is a
        // *second* click in this same row (§3.4), so suppressing the row would
        // swallow the confirmation it just asked for. Safe, because arming
        // only widens `quit` to `quit?` — the `Q` hint itself does not move,
        // and nothing to its left does either. Note the asymmetry: quit does
        // not *set* the suppression, but it is still *subject* to one set by
        // another action, so a reflow after `A approve` cannot drop a stray
        // second click onto it.
        if action != FooterAction::Quit {
            self.suppress_footer();
        }
        match action {
            FooterAction::CommandMode => self.enter_command_mode(),
            FooterAction::New => self.open_new_wizard_for_view(),
            FooterAction::Run => {
                if let Some(task) = self.state.selected_task().cloned() {
                    self.run_task_by_id(&task.id);
                }
            }
            FooterAction::Cancel => {
                if let Some(task) = self.state.selected_task().cloned() {
                    self.cancel_task_by_id(&task.id);
                }
            }
            FooterAction::Approve => {
                if let Some(task) = self.state.selected_task().cloned()
                    && task.status == TaskStatus::Review
                {
                    self.approve_task_by_id(&task.id);
                }
            }
            FooterAction::Reject => {
                if let Some(task) = self.state.selected_task().cloned()
                    && task.status == TaskStatus::Review
                {
                    self.reject_task_by_id(&task.id, None);
                }
            }
            FooterAction::Edit => match self.state.view {
                ViewId::Tasks => self.open_edit_task_wizard(),
                ViewId::Agents => self.open_edit_agent_wizard(),
                ViewId::Goals => self.open_edit_goal_wizard(),
                ViewId::Logs => {}
            },
            FooterAction::StopAgent => self.force_stop_selected_agent(),
            FooterAction::ToggleAutonomous => {
                if let Some(agent) = self.state.selected_agent().cloned() {
                    self.toggle_autonomous_by_id(&agent.id);
                }
            }
            FooterAction::PauseGoal => {
                if let Some(goal) = self.state.selected_goal().cloned()
                    && matches!(goal.status, GoalStatus::Active | GoalStatus::Paused)
                {
                    let next = if goal.status == GoalStatus::Active {
                        GoalStatus::Paused
                    } else {
                        GoalStatus::Active
                    };
                    self.set_goal_status(&goal.id, next, false);
                }
            }
            FooterAction::ShowAllTasks => self.toggle_show_all_tasks(),
            FooterAction::Delete => self.delete_selected(),
            FooterAction::Undo => self.undo_deletion(),
            FooterAction::CloseDetail => self.close_detail_or_tail(),
            FooterAction::Enter => self.handle_enter(),
            FooterAction::Quit => self.click_quit(),
            FooterAction::Help => self.open_help(),
        }
        self.invalidate_hits();
    }

    /// `Q quit` arms on the first click and exits on the second.
    ///
    /// The one exception to "a button acts on a single click": an accidental
    /// exit at the very bottom edge of the terminal is the worst mis-click in
    /// this UI. The arming is rendered in place as `Q quit?` rather than
    /// raised as a toast, because at most two toasts are ever drawn
    /// (`state.rs:985`) and the feedback must not be able to go missing.
    fn click_quit(&mut self) {
        let now = self.now();
        match self.state.mouse.quit_armed_at {
            Some(armed_at) if now - armed_at <= QUIT_ARM_MS => {
                self.state.should_exit = true;
            }
            _ => self.state.mouse.quit_armed_at = Some(now),
        }
    }

    /// Is `Q quit` armed right now? Drives the `Q quit?` rendering.
    pub fn quit_armed(&self) -> bool {
        self.state.mouse.quit_armed(self.now())
    }

    fn delete_selected(&mut self) {
        match self.state.view {
            ViewId::Tasks => {
                if let Some(task) = self.state.selected_task().cloned()
                    && task.status != TaskStatus::InProgress
                {
                    self.delete_task_by_id(&task.id);
                }
            }
            ViewId::Agents => {
                if let Some(agent) = self.state.selected_agent().cloned() {
                    self.delete_agent_by_id(&agent.id);
                }
            }
            ViewId::Goals => {
                if let Some(goal) = self.state.selected_goal().cloned() {
                    self.delete_goal_by_id(&goal.id);
                }
            }
            ViewId::Logs => {}
        }
    }

    /// Select a logs row without moving the window it was clicked in.
    ///
    /// A click points at a row that is already on screen, so scrolling would
    /// move the list out from under the pointer. The subtlety is tail mode:
    /// `log_scroll_offset` reads 0 there while the window actually shows the
    /// **last** `view_h` rows (`visible_log_indices`), so leaving the offset
    /// at 0 — or deriving one from the clicked row — jumps the view. Leaving
    /// tail mode therefore adopts the tail window's own start first.
    fn select_log_row(&mut self, filtered_index: usize) {
        let filtered = self.state.filtered_messages();
        if filtered_index >= filtered.len() {
            return;
        }
        let view_h = self
            .state
            .regions(self.backend.can_create_task())
            .main_h
            .saturating_sub(2) as usize;

        if self.state.log_selected_index < 0 {
            self.state.log_scroll_offset = filtered.len().saturating_sub(view_h);
        }
        // Leaving tail mode changes the footer's `Esc close` / `Enter detail`
        // pair, so the same reflow window applies.
        self.suppress_footer();
        self.state.log_selected_index = filtered_index as i64;

        // Only a row outside the window moves it, which a clicked row cannot
        // be. Kept as a guard rather than an assertion: a stale map is
        // dropped before it gets here, but the arithmetic should still be
        // total.
        let first = self.state.log_scroll_offset;
        if view_h > 0 && filtered_index >= first + view_h {
            self.state.log_scroll_offset = filtered_index + 1 - view_h;
        } else if filtered_index < first {
            self.state.log_scroll_offset = filtered_index;
        }
    }

    fn wizard_option_primary(&mut self, index: usize, part: RowPart) {
        match part {
            RowPart::Checkbox => self.wizard_select_then(index, KeyCode::Char(' ')),
            RowPart::Label => self.wizard_move_to(index),
        }
    }

    /// The secondary action depends on the step kind, which is the whole of
    /// this method's reason to exist.
    ///
    /// On a `Select` step there is one value, so confirming it is the obvious
    /// second click. On a **`Multiselect`** step confirming would *submit the
    /// whole step* — a double click on a label would advance the wizard
    /// instead of ticking a box, which is the opposite of what the inventory
    /// says a multiselect label does and a real way to walk a wizard by
    /// accident. There it toggles, exactly like the picker rows it looks
    /// identical to.
    ///
    /// A checkbox has no separate secondary at all: like a picker's, every
    /// click on it toggles. Making the second click a no-op (as this did)
    /// meant the same gesture on two visually identical checkboxes — a
    /// picker's and a wizard's — did different things.
    fn wizard_option_secondary(&mut self, index: usize, part: RowPart) {
        let multiselect = self
            .state
            .wizard
            .as_ref()
            .is_some_and(|w| w.step().kind == StepKind::Multiselect);
        match part {
            RowPart::Checkbox => self.wizard_option_primary(index, part),
            RowPart::Label if multiselect => self.wizard_select_then(index, KeyCode::Char(' ')),
            RowPart::Label => self.wizard_select_then(index, KeyCode::Enter),
        }
    }

    /// Move the wizard's cursor to `index` by the only means the engine
    /// offers: its own `Up`/`Down` handling. Bounded by the option count so a
    /// stale zone cannot loop.
    fn wizard_move_to(&mut self, index: usize) {
        let Some(wizard) = self.state.wizard.as_ref() else {
            return;
        };
        let total = wizard.options().len();
        let mut current = wizard.select_index();
        if index >= total {
            return;
        }
        for _ in 0..total {
            if current == index {
                break;
            }
            let code = if current < index {
                KeyCode::Down
            } else {
                KeyCode::Up
            };
            self.wizard_key(code);
            let Some(wizard) = self.state.wizard.as_ref() else {
                return;
            };
            let next = wizard.select_index();
            if next == current {
                break;
            }
            current = next;
        }
    }

    fn wizard_select_then(&mut self, index: usize, code: KeyCode) {
        self.wizard_move_to(index);
        self.wizard_key(code);
    }

    /// Forward one key to the wizard engine. This is the sole place a key is
    /// synthesized, and it is correct here: `Wizard::handle_key` is the
    /// engine's only entry point and its step-kind dispatch is internal, so
    /// nothing is overloaded the way the view chain overloads `a` and `g`.
    fn wizard_key(&mut self, code: KeyCode) {
        let now = self.now();
        let Some(wizard) = self.state.wizard.as_mut() else {
            return;
        };
        let before = wizard.step_id().to_string();
        let outcome = wizard.handle_key(KeyEvent::new(code, KeyModifiers::NONE), now);
        let after = self
            .state
            .wizard
            .as_ref()
            .map(|w| w.step_id().to_string())
            .unwrap_or_default();
        if before != after {
            // A step change retires every zone on screen, and must also retire
            // the double-click window: otherwise a third fast click would
            // count as a double against the *new* step's option 0.
            self.state.mouse.last_click = None;
            self.invalidate_hits();
        }
        match outcome {
            WizardOutcome::Consumed | WizardOutcome::Ignored => {}
            WizardOutcome::Cancelled => self.cancel_wizard(),
            WizardOutcome::Complete(values) => self.submit_wizard(values),
            WizardOutcome::SuggestionSelected(value) => self.apply_name_suggestion(&value),
            WizardOutcome::PasteImage => self.paste_image(),
        }
    }

    fn execute_selected_suggestion(&mut self) {
        self.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    }

    fn accept_command_completion(&mut self) {
        self.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
    }

    // ── Wheel ──────────────────────────────────────────────────────────────

    fn wheel(&mut self, x: u16, y: u16, delta: i32) {
        if self.state.mouse.wheel_steps >= WHEEL_MAX_STEPS_PER_FRAME {
            return;
        }
        if !self.state.mouse.hits.contains(x, y) {
            return;
        }
        let Some(region) = self.state.mouse.hits.resolve_scroll(x, y) else {
            return;
        };
        if !self.wheel_allows(region) {
            return;
        }
        self.state.mouse.wheel_steps += 1;
        match region {
            ScrollRegion::MainList => self.wheel_main_list(delta),
            ScrollRegion::AgentPicker => self.picker_move_clamped(delta),
            ScrollRegion::TypePicker => self.type_picker_move_clamped(delta),
            ScrollRegion::CommandSuggestions => self.wheel_suggestions(delta),
            ScrollRegion::WizardOptions => self.wizard_key(if delta < 0 {
                KeyCode::Up
            } else {
                KeyCode::Down
            }),
            ScrollRegion::WizardTextarea => self.wheel_textarea(delta),
        }
    }

    /// The wheel obeys the same modal gating as a click.
    fn wheel_allows(&self, region: ScrollRegion) -> bool {
        if self.state.show_help {
            return false;
        }
        if self.state.show_agent_picker || self.state.show_type_picker {
            return matches!(region, ScrollRegion::AgentPicker | ScrollRegion::TypePicker);
        }
        match self.state.input_mode {
            InputMode::Command => region == ScrollRegion::CommandSuggestions,
            InputMode::Wizard => matches!(
                region,
                ScrollRegion::WizardOptions | ScrollRegion::WizardTextarea
            ),
            InputMode::None => region == ScrollRegion::MainList,
        }
    }

    /// With a detail panel open the wheel only acts where there is something
    /// to scroll. Silently changing the entity *behind* an open detail is the
    /// surprise this avoids; the goal detail is the one panel with scroll
    /// state, and `move_selection_up/down` already scrolls it there.
    fn wheel_main_list(&mut self, delta: i32) {
        if self.state.detail_open() && self.state.view != ViewId::Goals {
            return;
        }
        if delta < 0 {
            self.move_selection_up();
        } else {
            self.move_selection_down();
        }
    }

    /// Only when there are suggestions: `Up`/`Down` in command mode fall back
    /// to shell-style history when there are none, and a wheel must never
    /// walk history.
    fn wheel_suggestions(&mut self, delta: i32) {
        let value = self.state.command_bar.input.value().to_string();
        let suggestions = resolve_suggestions(&value);
        if suggestions.is_empty() {
            return;
        }
        let index = self.state.command_bar.suggestion_index;
        self.state.command_bar.suggestion_index = if delta < 0 {
            index.saturating_sub(1)
        } else {
            (index + 1).min(suggestions.len() - 1)
        };
    }

    fn wheel_textarea(&mut self, delta: i32) {
        let Some(wizard) = self.state.wizard.as_mut() else {
            return;
        };
        wizard.scroll_textarea(delta < 0);
    }

    /// Reset the per-frame wheel budget. Called when a frame is adopted.
    pub(crate) fn start_frame(&mut self) {
        self.state.mouse.wheel_steps = 0;
    }

    /// `F2` and `/mouse`: turn capture on or off, and say so.
    ///
    /// Only the desire changes here; the runner reconciles the terminal. That
    /// keeps the data flow one-way and makes the toggle testable with no
    /// terminal at all.
    pub fn set_mouse_enabled(&mut self, enabled: bool) {
        self.state.mouse.enabled = enabled;
        if !enabled {
            // Nothing is half-pressed or half-armed once capture stops.
            self.state.mouse.armed = None;
            self.state.mouse.last_click = None;
            self.state.mouse.quit_armed_at = None;
        }
        let text = if enabled {
            "Mouse capture on"
        } else {
            "Mouse capture off — drag to select text; F2 or /mouse on to re-enable"
        };
        self.emit(text, Token::Silver, MsgType::System);
    }

    /// `F2` — the runtime toggle, handled before every other key stage.
    pub fn toggle_mouse_enabled(&mut self) {
        self.set_mouse_enabled(!self.state.mouse.enabled);
    }

    /// A terminal resize retires the geometry — **unless** the frame we last
    /// drew was already this size.
    ///
    /// crossterm delivers the SIGWINCH notification on the first `read` after
    /// the signal, and nothing reads until the user does something. By then
    /// the draw loop has already noticed the new size through
    /// `terminal.size()` and redrawn, so the map is current and the
    /// `Event::Resize` is stale news that arrives *in the same drain as the
    /// click that woke us*. Invalidating on it unconditionally threw that
    /// click away — and tmux's resize produces more than one notification, so
    /// a user saw one or two clicks after a resize do nothing at all (QA B3).
    ///
    /// Comparing against the map's own recorded area is what tells the two
    /// cases apart: a map built before the resize has the old dimensions.
    pub fn resize(&mut self, width: u16, height: u16) {
        let area = self.state.mouse.hits.area();
        if area.width != width || area.height != height {
            self.invalidate_hits();
        }
    }

    /// Retire the frame's geometry, and with it anything pinned to it.
    ///
    /// Clearing `armed` matters: a press is only meaningful against the frame
    /// it was pressed on, so if the geometry goes away between the press and
    /// the release — a resize, a keystroke, a layout-changing action — the
    /// click is cancelled rather than fired against rebuilt geometry. Without
    /// this an arm survived indefinitely (code gate finding 6), and input can
    /// arrive in bursts half a second apart — tmux stalls a pane's input
    /// while it resizes — so press and release are far enough apart for it to
    /// matter.
    pub(crate) fn invalidate_hits(&mut self) {
        self.state.mouse.hits.invalidate();
        self.state.mouse.last_click = None;
        self.state.mouse.armed = None;
    }
}
