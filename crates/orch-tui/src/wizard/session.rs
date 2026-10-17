//! In-memory recovery and the single guarded editor transition path.
use super::{StepKind, Values, Wizard, WizardKind, WizardOutcome};
use crate::{
    app::App,
    backend::{CopyTarget, TuiBackend},
    clock::Clock,
    hit::{TextAction, TextSurfaceId, VerbId, ZoneId, ZoneKind},
    keys::{ExitCause, UiEvent},
    state::{CopyOrigin, InputMode},
    theme::Token,
    views::{RenderCtx, put_line},
};
use ratatui::crossterm::event::{Event, KeyCode, KeyModifiers, MouseButton, MouseEventKind};
use ratatui::{
    Frame,
    layout::{Position, Rect},
    text::Span,
    widgets::Clear,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubmitOutcome {
    Failed,
    Committed,
    Advanced,
}

pub const MAX_DRAFTS: usize = 20;
pub const MAX_DRAFT_BYTES: usize = 256 * 1024;
pub const MAX_TOTAL_DRAFT_BYTES: usize = 2 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DraftKey {
    pub kind: WizardKind,
    pub target_id: Option<String>,
    pub setting: Option<String>,
    pub template_key: Option<String>,
}
impl DraftKey {
    pub fn from_wizard(wizard: &Wizard) -> Self {
        Self {
            kind: wizard.kind.clone(),
            target_id: wizard.target_id.clone(),
            setting: wizard.setting.clone(),
            template_key: wizard.template_key.clone(),
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeaveChoice {
    Continue,
    KeepDraft,
    Discard,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LeaveIntent {
    Back,
    Cancel,
    Replace(DraftKey),
    Exit(ExitCause),
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingLeave {
    pub intent: LeaveIntent,
    pub choice: LeaveChoice,
}

#[derive(Clone)]
pub struct WizardDraft {
    pub key: DraftKey,
    pub baseline: Values,
    pub values: Values,
    pub step_id: String,
    pub current_text: String,
    pub attachments: Vec<String>,
    pub cursor: (usize, usize),
    // Retain library selection, caret and per-step scratch without translating
    // private textarea coordinates or serializing closure-bearing step configs.
    snapshot: Wizard,
}
impl std::fmt::Debug for WizardDraft {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WizardDraft")
            .field("key", &self.key)
            .field("step_id", &self.step_id)
            .field("cursor", &self.cursor)
            .field("bytes", &self.bytes())
            .finish_non_exhaustive()
    }
}
impl WizardDraft {
    pub fn capture(wizard: &Wizard, attachments: &[String]) -> Self {
        Self {
            key: DraftKey::from_wizard(wizard),
            baseline: wizard.baseline.clone(),
            values: wizard.semantic_values(),
            step_id: wizard.step_id().to_string(),
            current_text: wizard.current_value(),
            attachments: attachments.to_vec(),
            cursor: if wizard.step().kind == StepKind::Textarea {
                wizard.textarea().cursor()
            } else {
                (0, wizard.text_input().cursor().pos())
            },
            snapshot: wizard.draft_snapshot(),
        }
    }
    pub(crate) fn source_defaults(&self) -> &Values {
        &self.snapshot.source_defaults
    }
    pub(crate) fn restore(&self) -> Wizard {
        self.snapshot.clone()
    }
    pub fn bytes(&self) -> usize {
        self.baseline
            .iter()
            .chain(self.values.iter())
            .map(|(k, v)| k.len() + v.len())
            .sum::<usize>()
            + self.step_id.len()
            + self.current_text.len()
            + self.attachments.iter().map(String::len).sum::<usize>()
            + self.key.target_id.as_ref().map_or(0, String::len)
            + self.key.setting.as_ref().map_or(0, String::len)
            + self.key.template_key.as_ref().map_or(0, String::len)
            + self.snapshot.retained_bytes()
    }
}
#[derive(Default)]
pub struct WizardSessionStore {
    pub active_key: Option<DraftKey>,
    pub pending_leave: Option<PendingLeave>,
    drafts: Vec<WizardDraft>,
    dirty: bool,
    pub error: Option<String>,
    pub(crate) replacement: Option<Wizard>,
    pub(crate) recovery: bool,
    pressed: Option<ZoneId>,
    drag: Option<(DraftKey, String, Rect)>,
    copy_presentation: Option<String>,
}
impl WizardSessionStore {
    pub fn save_draft(&mut self, draft: WizardDraft) -> Result<(), String> {
        let previous = self.drafts.iter().position(|d| d.key == draft.key);
        let size = draft.bytes();
        let total = self
            .drafts
            .iter()
            .enumerate()
            .filter(|(i, _)| Some(*i) != previous)
            .map(|(_, d)| d.bytes())
            .sum::<usize>();
        if size > MAX_DRAFT_BYTES {
            return Err(
                "Draft exceeds 256 KiB; continue editing, submit, or explicitly discard".into(),
            );
        }
        if previous.is_none() && self.drafts.len() >= MAX_DRAFTS {
            return Err(
                "20 session drafts already saved; submit or explicitly discard a draft".into(),
            );
        }
        if total.saturating_add(size) > MAX_TOTAL_DRAFT_BYTES {
            return Err("Session drafts exceed 2 MiB; submit or explicitly discard a draft".into());
        }
        if let Some(i) = previous {
            self.drafts[i] = draft;
        } else {
            self.drafts.push(draft);
        }
        Ok(())
    }
    pub fn draft_for(&self, key: &DraftKey) -> Option<&WizardDraft> {
        self.drafts.iter().find(|d| &d.key == key)
    }
    pub fn discard_draft(&mut self, key: &DraftKey) {
        self.drafts.retain(|d| &d.key != key);
    }
    pub fn draft_count(&self) -> usize {
        self.drafts.len()
    }
    pub fn total_bytes(&self) -> usize {
        self.drafts.iter().map(WizardDraft::bytes).sum()
    }
    pub fn request_leave(&mut self, intent: LeaveIntent) -> bool {
        if self.pending_leave.is_some() {
            return true;
        }
        let exit = matches!(intent, LeaveIntent::Exit(ExitCause::User));
        if self.dirty || (exit && !self.drafts.is_empty()) {
            self.error = None;
            self.pending_leave = Some(PendingLeave {
                intent,
                choice: LeaveChoice::Continue,
            });
            true
        } else {
            false
        }
    }
}

impl<B: TuiBackend, C: Clock> App<B, C> {
    pub(crate) fn guard_wizard_leave(&mut self, intent: LeaveIntent) -> bool {
        self.state.wizard_sessions.dirty = self
            .state
            .wizard
            .as_ref()
            .is_some_and(|w| w.semantic_dirty(&self.state.pending_attachments));
        let guarded = self.state.wizard_sessions.request_leave(intent);
        if guarded {
            self.invalidate_hits();
        }
        guarded
    }

    pub(crate) fn close_wizard(&mut self) {
        if let Some(w) = self.state.wizard.as_ref() {
            let surface = TextSurfaceId::Editor(DraftKey::from_wizard(w), w.step_id().into());
            if let Some(origin) = self.state.live_copy_origin(&surface).cloned() {
                self.state.retire_copy_origin(&origin);
            }
        }
        self.state.wizard = None;
        self.state.pending_attachments.clear();
        self.state.input_mode = InputMode::None;
        self.state.wizard_sessions.active_key = None;
        self.state.wizard_sessions.drag = None;
        self.state.wizard_sessions.recovery = false;
        self.invalidate_hits();
    }

    fn editor_origin(&mut self) -> Option<CopyOrigin> {
        let w = self.state.wizard.as_ref()?;
        let key = DraftKey::from_wizard(w);
        let step = w.step_id().to_string();
        let surface = TextSurfaceId::Editor(key.clone(), step.clone());
        if let Some(origin) = self.state.live_copy_origin(&surface) {
            return Some(origin.clone());
        }
        self.state.register_editor_origin(key, step).ok()
    }

    pub(crate) fn resolve_wizard_leave(&mut self, choice: LeaveChoice) {
        let Some(pending) = self.state.wizard_sessions.pending_leave.clone() else {
            return;
        };
        if choice == LeaveChoice::Continue {
            self.state.wizard_sessions.pending_leave = None;
            if !self.state.wizard_sessions.recovery {
                self.state.wizard_sessions.replacement = None;
            }
            self.state.wizard_sessions.error = None;
            self.invalidate_hits();
            return;
        }
        let exit = matches!(pending.intent, LeaveIntent::Exit(_));
        if exit && choice == LeaveChoice::KeepDraft {
            return;
        }
        if choice == LeaveChoice::KeepDraft {
            if let Some(w) = self.state.wizard.as_ref() {
                let draft = WizardDraft::capture(w, &self.state.pending_attachments);
                if let Err(error) = self.state.wizard_sessions.save_draft(draft) {
                    self.state.wizard_sessions.error = Some(error);
                    self.invalidate_hits();
                    return;
                }
            }
        } else if let Some(key) = self.state.wizard_sessions.active_key.clone() {
            self.state.wizard_sessions.discard_draft(&key);
        }
        self.state.wizard_sessions.pending_leave = None;
        self.state.wizard_sessions.error = None;
        match pending.intent {
            LeaveIntent::Back => {
                if let Some(w) = self.state.wizard.as_mut() {
                    if choice == LeaveChoice::Discard {
                        w.discard_current();
                    }
                    if w.go_back() == WizardOutcome::Cancelled {
                        self.close_wizard();
                    }
                }
                self.editor_origin();
            }
            LeaveIntent::Cancel => self.close_wizard(),
            LeaveIntent::Replace(_) => {
                if let Some(wizard) = self.state.wizard_sessions.replacement.take() {
                    self.close_wizard();
                    self.install_wizard(wizard);
                }
            }
            LeaveIntent::Exit(_) => {
                self.close_wizard();
                self.state.wizard_sessions = WizardSessionStore::default();
                // Exactly one normalized re-dispatch, now with no dirty state.
                self.request_exit(ExitCause::User);
            }
        }
        self.invalidate_hits();
    }

    fn copy_editor(&mut self, action: TextAction) {
        let Some(origin) = self.editor_origin() else {
            return;
        };
        let Some(w) = self.state.wizard.as_ref() else {
            return;
        };
        let selection = w.selected_text();
        let text = match action {
            TextAction::CopyAll => w.current_value(),
            TextAction::CopySelection => match selection {
                Some(text) => text,
                None => {
                    if let Some(w) = self.state.wizard.as_mut() {
                        w.copy_notice = Some("No selection; F6".into());
                    }
                    self.invalidate_hits();
                    return;
                }
            },
            _ => selection.unwrap_or_else(|| w.current_value()),
        };
        let target = match action {
            TextAction::CopyTerminal => CopyTarget::Terminal,
            TextAction::CopyNative => CopyTarget::Native,
            _ => CopyTarget::Auto,
        };
        if let Some(w) = self.state.wizard.as_mut() {
            w.copy_notice = None;
        }
        let accepted = self.backend.copy_text_via(&text, target);
        self.state
            .register_copy_request(origin, target, accepted, self.clock.now_ms());
        self.invalidate_hits();
    }

    fn editor_action(&mut self, action: TextAction, mouse: bool) {
        let Some(origin) = self.editor_origin() else {
            return;
        };
        if mouse
            && !matches!(
                action,
                TextAction::PreviousCopyAction | TextAction::NextCopyAction
            )
        {
            self.state
                .suppress_text_footer(&origin, self.clock.now_ms());
        }
        let Some(w) = self.state.wizard.as_mut() else {
            return;
        };
        match action {
            TextAction::OpenCopyRail => w.copy_rail = Some(0),
            TextAction::PreviousCopyAction => {
                w.copy_rail = Some((w.copy_rail.unwrap_or(0) + 3) % 4)
            }
            TextAction::NextCopyAction => w.copy_rail = Some((w.copy_rail.unwrap_or(0) + 1) % 4),
            TextAction::CloseCopyRail | TextAction::DismissCopyFeedback => {
                w.copy_rail = None;
                w.copy_notice = None;
                w.suppress_validation_footer = true;
                self.state.dismiss_copy_feedback(&origin);
            }
            TextAction::CopyAll
            | TextAction::CopySelection
            | TextAction::CopyTerminal
            | TextAction::CopyNative => {
                w.copy_rail = None;
                self.copy_editor(action);
            }
            _ => return,
        }
        self.invalidate_hits();
    }
}

pub const COPY_ACTIONS: [TextAction; 4] = [
    TextAction::CopyAll,
    TextAction::CopySelection,
    TextAction::CopyTerminal,
    TextAction::CopyNative,
];
pub fn copy_label(action: TextAction, selected: bool) -> String {
    let (name, scope) = match action {
        TextAction::CopyAll => ("Copy all", "all"),
        TextAction::CopySelection => (
            "Copy selection",
            if selected { "selection" } else { "none" },
        ),
        TextAction::CopyTerminal => (
            "Copy via terminal",
            if selected { "selection" } else { "all" },
        ),
        _ => (
            "Copy on this host",
            if selected { "selection" } else { "all" },
        ),
    };
    format!("{name} [{scope}]")
}

pub fn render_session_overlay(frame: &mut Frame, area: Rect, ctx: &RenderCtx) {
    let Some(pending) = ctx.state.wizard_sessions.pending_leave.as_ref() else {
        return;
    };
    let width = area.width.min(76);
    let height = area.height.min(9);
    let rect = Rect::new(
        area.x + (area.width - width) / 2,
        area.y + (area.height - height) / 2,
        width,
        height,
    );
    frame.render_widget(Clear, rect);
    let exit = matches!(pending.intent, LeaveIntent::Exit(_));
    let title = if exit {
        "Quit? All session drafts will be lost (RAM only)."
    } else if matches!(pending.intent, LeaveIntent::Back) {
        "Leave this step? Keep unsaved input as a session draft."
    } else {
        "Leave editor? Keep a draft for this session or discard it."
    };
    put_line(
        frame,
        rect,
        rect.y,
        vec![Span::styled(
            crate::format::cap_display(title, width as usize),
            ctx.bold(Token::Amber),
        )],
    );
    let choices: &[(LeaveChoice, &str)] = if exit || ctx.state.wizard_sessions.recovery {
        &[
            (LeaveChoice::Continue, "Continue editing"),
            (
                LeaveChoice::Discard,
                if exit {
                    "Discard and quit"
                } else {
                    "Discard draft and restart"
                },
            ),
        ]
    } else {
        &[
            (LeaveChoice::Continue, "Continue editing"),
            (LeaveChoice::KeepDraft, "Keep draft and leave"),
            (LeaveChoice::Discard, "Discard and leave"),
        ]
    };
    for (i, (choice, label)) in choices.iter().enumerate() {
        let y = rect.y + 1 + i as u16;
        let marker = if pending.choice == *choice {
            "> "
        } else {
            "  "
        };
        put_line(
            frame,
            rect,
            y,
            vec![Span::styled(
                format!("{marker}{label}"),
                ctx.style(Token::White),
            )],
        );
        ctx.row_zone(
            rect,
            y,
            ZoneKind::LeaveChoice(*choice),
            ZoneId::LeaveChoice(*choice),
        );
    }
    if let Some(error) = ctx.state.wizard_sessions.error.as_ref() {
        put_line(
            frame,
            rect,
            rect.bottom().saturating_sub(2),
            vec![Span::styled(
                crate::format::cap_display(error, width as usize),
                ctx.style(Token::Red),
            )],
        );
    }
    put_line(
        frame,
        rect,
        rect.bottom().saturating_sub(1),
        vec![Span::styled(
            "↑↓/Tab choose  Enter confirm  Esc continue",
            ctx.style(Token::Dim),
        )],
    );
}

pub(crate) fn handle_session_event<B: TuiBackend, C: Clock>(
    app: &mut App<B, C>,
    event: &UiEvent,
) -> bool {
    if let UiEvent::ExitRequested(cause) = event {
        if *cause == ExitCause::Signal {
            return false;
        }
        return app.guard_wizard_leave(LeaveIntent::Exit(*cause));
    }
    if app.state.wizard_sessions.pending_leave.is_some() {
        match event {
            UiEvent::Terminal(Event::Key(key)) => {
                if key.code == KeyCode::Esc {
                    app.resolve_wizard_leave(LeaveChoice::Continue);
                } else if key.code == KeyCode::Enter {
                    let choice = app
                        .state
                        .wizard_sessions
                        .pending_leave
                        .as_ref()
                        .unwrap()
                        .choice;
                    app.resolve_wizard_leave(choice);
                } else if matches!(
                    key.code,
                    KeyCode::Up
                        | KeyCode::Down
                        | KeyCode::Tab
                        | KeyCode::BackTab
                        | KeyCode::Left
                        | KeyCode::Right
                ) {
                    let recovery = app.state.wizard_sessions.recovery;
                    let pending = app.state.wizard_sessions.pending_leave.as_mut().unwrap();
                    let choices: &[LeaveChoice] =
                        if matches!(pending.intent, LeaveIntent::Exit(_)) || recovery {
                            &[LeaveChoice::Continue, LeaveChoice::Discard]
                        } else {
                            &[
                                LeaveChoice::Continue,
                                LeaveChoice::KeepDraft,
                                LeaveChoice::Discard,
                            ]
                        };
                    let i = choices
                        .iter()
                        .position(|c| *c == pending.choice)
                        .unwrap_or(0);
                    let back = matches!(key.code, KeyCode::Up | KeyCode::BackTab | KeyCode::Left);
                    pending.choice =
                        choices[(i + if back { choices.len() - 1 } else { 1 }) % choices.len()];
                }
            }
            UiEvent::Terminal(Event::Mouse(mouse)) => {
                let zone = app
                    .state
                    .mouse
                    .hits
                    .resolve(mouse.column, mouse.row)
                    .cloned();
                if mouse.kind == MouseEventKind::Down(MouseButton::Left) {
                    app.state.wizard_sessions.pressed = zone.map(|z| z.id);
                } else if mouse.kind == MouseEventKind::Up(MouseButton::Left)
                    && let Some(zone) = zone
                    && app.state.wizard_sessions.pressed.take() == Some(zone.id)
                    && let ZoneKind::LeaveChoice(choice) = zone.kind
                {
                    app.resolve_wizard_leave(choice);
                }
            }
            UiEvent::Tick => return false,
            _ => {}
        }
        return true;
    }
    if app.state.show_help
        || app.state.copy_help.is_some()
        || app.state.viewer.is_some()
        || app.state.show_agent_picker
        || app.state.show_type_picker
        || app.state.input_mode == InputMode::Command
    {
        return false;
    }
    if app.state.wizard.is_none() {
        return false;
    }
    let origin = app.editor_origin();
    if let Some(origin) = origin.as_ref() {
        let presentation = app
            .state
            .copy_footer_visible(origin)
            .then(|| format!("{:?}", app.state.feedback_for(origin)));
        if presentation != app.state.wizard_sessions.copy_presentation {
            app.state.wizard_sessions.copy_presentation = presentation;
            app.invalidate_hits();
        }
    }
    match event {
        UiEvent::Terminal(Event::Key(key)) => {
            if key.code == KeyCode::F(1) {
                return false;
            }
            let rail = app.state.wizard.as_ref().and_then(|w| w.copy_rail);
            if key.code == KeyCode::F(6) {
                let action = if rail.is_some() {
                    TextAction::CloseCopyRail
                } else {
                    TextAction::OpenCopyRail
                };
                app.editor_action(action, false);
                return true;
            }
            if let Some(index) = rail {
                match key.code {
                    KeyCode::Esc => app.editor_action(TextAction::CloseCopyRail, false),
                    KeyCode::Home => {
                        app.state.wizard.as_mut().unwrap().copy_rail = Some(0);
                        app.invalidate_hits();
                    }
                    KeyCode::End => {
                        app.state.wizard.as_mut().unwrap().copy_rail = Some(3);
                        app.invalidate_hits();
                    }
                    KeyCode::Left | KeyCode::Up | KeyCode::BackTab => {
                        app.editor_action(TextAction::PreviousCopyAction, false)
                    }
                    KeyCode::Right | KeyCode::Down | KeyCode::Tab => {
                        app.editor_action(TextAction::NextCopyAction, false)
                    }
                    KeyCode::Enter => app.editor_action(COPY_ACTIONS[index], false),
                    _ => {}
                }
                return true;
            }
            if key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::META)
                && matches!(key.code, KeyCode::Char('s' | 'S'))
                && app
                    .state
                    .wizard
                    .as_ref()
                    .is_some_and(|w| w.kind == WizardKind::Agent)
            {
                app.open_shop_wizard();
                return true;
            }
            let w = app.state.wizard.as_ref().unwrap();
            if key.modifiers.contains(KeyModifiers::CONTROL)
                && matches!(key.code, KeyCode::Char('y' | 'Y'))
            {
                let action = if key.modifiers.contains(KeyModifiers::ALT) {
                    TextAction::CopySelection
                } else {
                    TextAction::CopyAll
                };
                app.copy_editor(action);
                return true;
            }
            if app.state.wizard_sessions.recovery {
                if key.code == KeyCode::Esc
                    && let Some(fresh) = app.state.wizard_sessions.replacement.as_ref()
                {
                    app.guard_wizard_leave(LeaveIntent::Replace(DraftKey::from_wizard(fresh)));
                }
                return true;
            }
            let back = key.code == KeyCode::Esc
                || (matches!(key.code, KeyCode::Backspace | KeyCode::Delete)
                    && !key.modifiers.intersects(
                        KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::META,
                    )
                    && ((w.step().kind == StepKind::Text
                        && w.text_value().is_empty()
                        && w.active_index() > 0)
                        || matches!(w.step().kind, StepKind::Select | StepKind::Multiselect)));
            if back {
                if !app.guard_wizard_leave(LeaveIntent::Back) {
                    let outcome = app.state.wizard.as_mut().unwrap().go_back();
                    if outcome == WizardOutcome::Cancelled {
                        app.close_wizard();
                    }
                    app.editor_origin();
                    app.invalidate_hits();
                }
                return true;
            }
            let before = app.state.wizard.as_ref().unwrap().current_value();
            if let Some(w) = app.state.wizard.as_mut() {
                w.suppress_validation_footer = false;
            }
            // All ordinary editor keys are owned here so a step transition can
            // retire its exact copy epoch before the next frame/help request.
            let outcome = app
                .state
                .wizard
                .as_mut()
                .unwrap()
                .handle_key(*key, app.clock.now_ms());
            if app
                .state
                .wizard
                .as_ref()
                .is_some_and(|w| w.current_value() != before)
            {
                if let Some(origin) = origin {
                    app.state.clear_copy_feedback(&origin);
                }
                if let Some(w) = app.state.wizard.as_mut() {
                    w.copy_notice = None;
                    w.session_error = None;
                }
            }
            match outcome {
                WizardOutcome::Cancelled => app.cancel_wizard(),
                WizardOutcome::Complete(values) => app.submit_wizard(values),
                WizardOutcome::SuggestionSelected(value) => app.apply_name_suggestion(&value),
                WizardOutcome::PasteImage => app.paste_image(),
                _ => {}
            }
            app.editor_origin();
            app.invalidate_hits();
            true
        }
        UiEvent::Terminal(Event::Paste(_)) if app.state.wizard_sessions.recovery => true,
        UiEvent::Terminal(Event::Mouse(mouse)) => {
            let handled = handle_editor_mouse(app, *mouse);
            handled || app.state.wizard_sessions.recovery
        }
        UiEvent::Terminal(Event::Resize(_, _)) => {
            app.state.wizard_sessions.drag = None;
            false
        }
        _ => false,
    }
}

fn handle_editor_mouse<B: TuiBackend, C: Clock>(
    app: &mut App<B, C>,
    mouse: ratatui::crossterm::event::MouseEvent,
) -> bool {
    let Some(w) = app.state.wizard.as_ref() else {
        return false;
    };
    let key = DraftKey::from_wizard(w);
    let step = w.step_id().to_string();
    let zone = app
        .state
        .mouse
        .hits
        .resolve(mouse.column, mouse.row)
        .cloned();
    let position = Position::new(mouse.column, mouse.row);
    match mouse.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            app.state.wizard_sessions.pressed = zone.as_ref().map(|z| z.id.clone());
            if let Some(z) = zone.as_ref() {
                app.state.mouse.armed = Some(crate::state::ArmedClick {
                    id: z.id.clone(),
                    kind: z.kind.clone(),
                    secondary: false,
                    at_ms: app.clock.now_ms(),
                });
            }
            if let Some(z) = zone.as_ref()
                && z.id == ZoneId::EditorText(key.clone(), step.clone())
            {
                app.state.wizard_sessions.drag = Some((key, step, z.rect));
                app.state
                    .wizard
                    .as_mut()
                    .unwrap()
                    .pointer(position, z.rect, true);
                return true;
            }
            matches!(
                zone.map(|z| z.kind),
                Some(ZoneKind::TextAction(_) | ZoneKind::Verb(VerbId::WizardBack))
            )
        }
        MouseEventKind::Drag(MouseButton::Left) => {
            if let Some((draft, id, rect)) = app.state.wizard_sessions.drag.as_ref()
                && *draft == key
                && *id == step
                && app
                    .state
                    .mouse
                    .armed
                    .as_ref()
                    .is_some_and(|a| a.id == ZoneId::EditorText(key.clone(), step.clone()))
                && app.state.mouse.hits.zones().iter().any(|z| {
                    z.id == ZoneId::EditorText(key.clone(), step.clone()) && z.rect == *rect
                })
            {
                app.state
                    .wizard
                    .as_mut()
                    .unwrap()
                    .pointer(position, *rect, false);
                return true;
            }
            app.state.wizard_sessions.drag = None;
            false
        }
        MouseEventKind::Up(MouseButton::Left) => {
            if let Some((draft, id, rect)) = app.state.wizard_sessions.drag.take()
                && draft == key
                && id == step
            {
                if app
                    .state
                    .mouse
                    .armed
                    .take()
                    .is_some_and(|a| a.id == ZoneId::EditorText(key.clone(), step.clone()))
                    && app.state.mouse.hits.zones().iter().any(|z| {
                        z.id == ZoneId::EditorText(key.clone(), step.clone()) && z.rect == rect
                    })
                {
                    app.state
                        .wizard
                        .as_mut()
                        .unwrap()
                        .pointer(position, rect, false);
                }
                return true;
            }
            let pressed = app
                .state
                .wizard_sessions
                .pressed
                .take()
                .filter(|id| app.state.mouse.armed.as_ref().is_some_and(|a| a.id == *id));
            if let Some(z) = zone
                && pressed == Some(z.id.clone())
            {
                match (z.kind, z.id) {
                    (
                        ZoneKind::TextAction(action),
                        ZoneId::TextAction(TextSurfaceId::Editor(draft, id), _),
                    ) if draft == key && id == step => {
                        app.state.mouse.armed = None;
                        app.editor_action(action, true);
                        return true;
                    }
                    (ZoneKind::Verb(VerbId::WizardBack), _) => {
                        app.state.mouse.armed = None;
                        if !app.guard_wizard_leave(LeaveIntent::Back) {
                            let outcome = app.state.wizard.as_mut().unwrap().go_back();
                            if outcome == WizardOutcome::Cancelled {
                                app.close_wizard();
                            }
                            app.editor_origin();
                            app.invalidate_hits();
                        }
                        return true;
                    }
                    _ => {}
                }
            }
            false
        }
        _ => false,
    }
}
