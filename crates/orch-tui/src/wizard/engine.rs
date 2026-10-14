//! The `FormWizard` engine (`src/tui/components/FormWizard.tsx`).
//!
//! Step sequencing, skip predicates, per-step pre-fill, validation gating and
//! the key map are L0/L1 (PARITY.md §12.5): a wrong skip predicate writes
//! corrupt entity data through the UI.
//!
//! Two deliberate decisions where the TS behaviour is an Ink accident
//! (docs/parity/tui.md §5.1, §10):
//!
//! * **`Esc`** goes back one step and cancels only at step 0 — the documented
//!   `FormWizard` design, and what its unit tests assert. In the shipped app
//!   Ink delivered `Esc` to *both* the wizard and the App handler and the App
//!   won, so `Esc` always closed the wizard. With a single dispatcher there is
//!   no reason to reproduce the collision.
//! * **Cancelling always clears pending attachments.** The TS App's `Esc` path
//!   skipped `handleWizardCancel`, leaking them into the next wizard.
//!
//! The `textarea` step delegates to `tui-textarea-2` (ARCHITECTURE §4.8) for
//! the buffer, undo, word motions and soft wrapping.

use indexmap::{IndexMap, IndexSet};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use tui_textarea::{CursorMove, TextArea, WrapMode};

use crate::text::TextInput;

/// Collected wizard answers, keyed by step id, in step order.
pub type Values = IndexMap<String, String>;

/// `runValidation` debounce for text and textarea steps.
pub const VALIDATION_DEBOUNCE_MS: i64 = 300;
/// How long `Fix the error above` stays up.
pub const ENTER_BLOCK_HINT_MS: i64 = 2_000;
/// The blocked-confirm hint text.
pub const FIX_THE_ERROR_ABOVE: &str = "Fix the error above";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepKind {
    Text,
    Textarea,
    Select,
    Multiselect,
}

/// `suggestionMode` — `value` advances with the chosen value, `callback` hands
/// it to the host (which opens the pre-filled shop wizard).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SuggestionMode {
    #[default]
    Callback,
    Value,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Choice {
    pub value: String,
    pub label: String,
    pub hint: String,
}

impl Choice {
    pub fn new(value: &str, label: &str, hint: &str) -> Self {
        Choice {
            value: value.into(),
            label: label.into(),
            hint: hint.into(),
        }
    }
}

type ChoiceFn = std::rc::Rc<dyn Fn(&Values) -> Vec<Choice>>;
type DefaultFn = std::rc::Rc<dyn Fn(&Values) -> String>;
type SkipFn = std::rc::Rc<dyn Fn(&Values) -> bool>;
type ValidateFn = std::rc::Rc<dyn Fn(&str) -> Option<String>>;

#[derive(Clone, Default)]
pub enum Choices {
    #[default]
    None,
    Static(Vec<Choice>),
    Dynamic(ChoiceFn),
}

impl Choices {
    pub fn resolve(&self, values: &Values) -> Vec<Choice> {
        match self {
            Choices::None => Vec::new(),
            Choices::Static(list) => list.clone(),
            Choices::Dynamic(f) => f(values),
        }
    }

    pub fn is_none(&self) -> bool {
        matches!(self, Choices::None)
    }
}

#[derive(Clone, Default)]
pub enum DefaultValue {
    #[default]
    Empty,
    Static(String),
    Dynamic(DefaultFn),
}

impl DefaultValue {
    /// `resolveDefaultValue(step, values)`.
    pub fn resolve(&self, values: &Values) -> String {
        match self {
            DefaultValue::Empty => String::new(),
            DefaultValue::Static(s) => s.clone(),
            DefaultValue::Dynamic(f) => f(values),
        }
    }
}

#[derive(Clone)]
pub struct WizardStep {
    pub id: String,
    pub label: String,
    pub kind: StepKind,
    pub placeholder: String,
    pub description: Option<String>,
    pub required: bool,
    pub default_value: DefaultValue,
    pub options: Choices,
    pub suggestions: Choices,
    pub suggestion_mode: SuggestionMode,
    pub suggestions_label: Option<String>,
    pub skip: Option<SkipFn>,
    pub validate: Option<ValidateFn>,
}

impl WizardStep {
    pub fn new(id: &str, label: &str, kind: StepKind) -> Self {
        WizardStep {
            id: id.into(),
            label: label.into(),
            kind,
            placeholder: String::new(),
            description: None,
            required: false,
            default_value: DefaultValue::Empty,
            options: Choices::None,
            suggestions: Choices::None,
            suggestion_mode: SuggestionMode::Callback,
            suggestions_label: None,
            skip: None,
            validate: None,
        }
    }

    pub fn placeholder(mut self, placeholder: &str) -> Self {
        self.placeholder = placeholder.into();
        self
    }

    pub fn description(mut self, description: &str) -> Self {
        self.description = Some(description.into());
        self
    }

    pub fn required(mut self) -> Self {
        self.required = true;
        self
    }

    pub fn options(mut self, options: Vec<Choice>) -> Self {
        self.options = Choices::Static(options);
        self
    }

    pub fn get_options(mut self, f: impl Fn(&Values) -> Vec<Choice> + 'static) -> Self {
        self.options = Choices::Dynamic(std::rc::Rc::new(f));
        self
    }

    pub fn suggestions(mut self, suggestions: Vec<Choice>) -> Self {
        self.suggestions = Choices::Static(suggestions);
        self
    }

    pub fn get_suggestions(mut self, f: impl Fn(&Values) -> Vec<Choice> + 'static) -> Self {
        self.suggestions = Choices::Dynamic(std::rc::Rc::new(f));
        self
    }

    pub fn suggestion_mode(mut self, mode: SuggestionMode) -> Self {
        self.suggestion_mode = mode;
        self
    }

    pub fn suggestions_label(mut self, label: &str) -> Self {
        self.suggestions_label = Some(label.into());
        self
    }

    pub fn default_value(mut self, value: &str) -> Self {
        self.default_value = DefaultValue::Static(value.into());
        self
    }

    pub fn get_default_value(mut self, f: impl Fn(&Values) -> String + 'static) -> Self {
        self.default_value = DefaultValue::Dynamic(std::rc::Rc::new(f));
        self
    }

    pub fn skip(mut self, f: impl Fn(&Values) -> bool + 'static) -> Self {
        self.skip = Some(std::rc::Rc::new(f));
        self
    }

    pub fn always_skip(self) -> Self {
        self.skip(|_| true)
    }

    pub fn validate(mut self, f: impl Fn(&str) -> Option<String> + 'static) -> Self {
        self.validate = Some(std::rc::Rc::new(f));
        self
    }

    pub fn is_skipped(&self, values: &Values) -> bool {
        self.skip.as_ref().is_some_and(|f| f(values))
    }
}

/// What the host must do after a key reached the wizard.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WizardOutcome {
    /// Handled; nothing for the host to do.
    Consumed,
    /// The key was not a wizard key.
    Ignored,
    /// `Esc` at step 0.
    Cancelled,
    /// All steps answered.
    Complete(Values),
    /// A `callback`-mode suggestion was chosen (the shop-template path).
    SuggestionSelected(String),
    /// `Ctrl+V` / `Cmd+V` / `Ctrl+I` on a text or textarea step.
    PasteImage,
}

/// Which wizard this is — drives the submit handler and the re-key behaviour.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WizardKind {
    Agent,
    AgentShop,
    AgentFromShop,
    EditAgent,
    Task,
    EditTask,
    Team,
    Goal,
    EditGoal,
    Config,
}

pub struct Wizard {
    pub kind: WizardKind,
    pub title: String,
    /// Edit wizards carry the entity id they update.
    pub target_id: Option<String>,
    /// Set for `agent_from_shop`.
    pub template_key: Option<String>,
    /// `config` wizards carry the setting they edit.
    pub setting: Option<String>,
    /// Rendered in the footer — `📎{n}` for pending attachments.
    pub footer_extra: Option<String>,
    /// `onPasteImage` was provided (task / edit_task only).
    pub paste_capable: bool,

    steps: Vec<WizardStep>,
    values: Values,
    /// Index into the **full** step list.
    index: usize,

    text: TextInput,
    textarea: TextArea<'static>,
    select_index: usize,
    multi_selected: IndexSet<String>,

    browsing: bool,
    suggestion_index: usize,
    dirty: bool,
    /// `(validated value, error)` — an error only renders while the validated
    /// value still equals the current one.
    validated: Option<(String, Option<String>)>,
    validate_deadline: Option<i64>,
    enter_block_until: Option<i64>,
}

impl Wizard {
    pub fn new(kind: WizardKind, title: &str, steps: Vec<WizardStep>) -> Self {
        let mut wizard = Wizard {
            kind,
            title: title.into(),
            target_id: None,
            template_key: None,
            setting: None,
            footer_extra: None,
            paste_capable: false,
            steps,
            values: Values::new(),
            index: 0,
            text: TextInput::new(""),
            textarea: new_textarea(""),
            select_index: 0,
            multi_selected: IndexSet::new(),
            browsing: false,
            suggestion_index: 0,
            dirty: false,
            validated: None,
            validate_deadline: None,
            enter_block_until: None,
        };
        wizard.index = wizard.first_active_index();
        wizard.prefill();
        wizard
    }

    pub fn with_target(mut self, target_id: &str) -> Self {
        self.target_id = Some(target_id.into());
        self
    }

    pub fn with_template(mut self, template_key: &str) -> Self {
        self.template_key = Some(template_key.into());
        self
    }

    pub fn with_setting(mut self, setting: &str) -> Self {
        self.setting = Some(setting.into());
        self
    }

    pub fn with_paste(mut self, capable: bool) -> Self {
        self.paste_capable = capable;
        self
    }

    pub fn with_footer_extra(mut self, extra: Option<String>) -> Self {
        self.footer_extra = extra;
        self
    }

    // ── Step bookkeeping ───────────────────────────────────────────────────

    /// The steps the user will actually see, given the current answers.
    pub fn active_steps(&self) -> Vec<&WizardStep> {
        self.steps
            .iter()
            .filter(|s| !s.is_skipped(&self.values))
            .collect()
    }

    pub fn all_steps(&self) -> &[WizardStep] {
        &self.steps
    }

    pub fn step(&self) -> &WizardStep {
        &self.steps[self.index]
    }

    pub fn step_id(&self) -> &str {
        &self.steps[self.index].id
    }

    /// 0-based position within the active steps (the `step {i+1}/{n}` display).
    pub fn active_index(&self) -> usize {
        let id = self.step_id();
        self.active_steps()
            .iter()
            .position(|s| s.id == id)
            .unwrap_or(0)
    }

    pub fn active_count(&self) -> usize {
        self.active_steps().len()
    }

    pub fn values(&self) -> &Values {
        &self.values
    }

    pub fn is_browsing(&self) -> bool {
        self.browsing
    }

    pub fn suggestion_index(&self) -> usize {
        self.suggestion_index
    }

    pub fn select_index(&self) -> usize {
        self.select_index
    }

    pub fn multi_selected(&self) -> &IndexSet<String> {
        &self.multi_selected
    }

    pub fn text_value(&self) -> &str {
        self.text.value()
    }

    pub fn text_input(&self) -> &TextInput {
        &self.text
    }

    pub fn textarea(&self) -> &TextArea<'static> {
        &self.textarea
    }

    pub fn textarea_value(&self) -> String {
        self.textarea.lines().join("\n")
    }

    /// The current step's options, resolved against the current answers.
    pub fn options(&self) -> Vec<Choice> {
        self.step().options.resolve(&self.values)
    }

    /// Suggestions filtered by the typed text, as the browse list shows them.
    pub fn filtered_suggestions(&self) -> Vec<Choice> {
        let all = self.step().suggestions.resolve(&self.values);
        let typed = self.text.value().trim().to_lowercase();
        if typed.is_empty() {
            return all;
        }
        all.into_iter()
            .filter(|c| {
                c.label.to_lowercase().contains(&typed) || c.value.to_lowercase().contains(&typed)
            })
            .collect()
    }

    fn first_active_index(&self) -> usize {
        self.steps
            .iter()
            .position(|s| !s.is_skipped(&self.values))
            .unwrap_or(0)
    }

    /// Per-step pre-fill (`text` → reset, `textarea` → split, `select` → index
    /// of the default, `multiselect` → set of comma-separated values).
    fn prefill(&mut self) {
        let step = self.steps[self.index].clone();
        let default = step.default_value.resolve(&self.values);
        match step.kind {
            StepKind::Text => self.text.reset(&default),
            StepKind::Textarea => {
                self.textarea = new_textarea(&default);
                self.textarea.move_cursor(CursorMove::Bottom);
                self.textarea.move_cursor(CursorMove::End);
            }
            StepKind::Select => {
                let options = step.options.resolve(&self.values);
                self.select_index = options.iter().position(|o| o.value == default).unwrap_or(0);
            }
            StepKind::Multiselect => {
                self.multi_selected = default
                    .split(',')
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .collect();
            }
        }
        self.reset_step_state();
    }

    /// Everything that resets on both forward and backward navigation.
    fn reset_step_state(&mut self) {
        self.dirty = false;
        self.validated = None;
        self.validate_deadline = None;
        self.enter_block_until = None;
        self.browsing = false;
        self.suggestion_index = 0;
    }

    /// Replace the step list in place, keeping the answers and position — the
    /// live model-catalog refresh (docs/parity/tui.md §5.2).
    pub fn replace_steps(&mut self, steps: Vec<WizardStep>) {
        let current_id = self.step_id().to_string();
        self.steps = steps;
        self.index = self
            .steps
            .iter()
            .position(|s| s.id == current_id)
            .unwrap_or_else(|| self.first_active_index());
    }

    // ── Validation ─────────────────────────────────────────────────────────

    /// The error to render, or `None`. Only surfaces once the step is dirty and
    /// the validated value is still the current one, which suppresses a stale
    /// debounced error.
    pub fn visible_error(&self) -> Option<&str> {
        if !self.dirty {
            return None;
        }
        let (validated_value, error) = self.validated.as_ref()?;
        if *validated_value != self.current_value() {
            return None;
        }
        error.as_deref()
    }

    pub fn enter_block_hint(&self) -> bool {
        self.enter_block_until.is_some()
    }

    fn current_value(&self) -> String {
        match self.step().kind {
            StepKind::Text => self.text.value().to_string(),
            StepKind::Textarea => self.textarea_value(),
            StepKind::Select => self
                .options()
                .get(self.select_index)
                .map(|o| o.value.clone())
                .unwrap_or_default(),
            StepKind::Multiselect => self
                .multi_selected
                .iter()
                .cloned()
                .collect::<Vec<_>>()
                .join(","),
        }
    }

    fn run_validation(&mut self, value: &str) {
        let error = self.step().validate.as_ref().and_then(|f| f(value));
        self.validated = Some((value.to_string(), error));
    }

    fn schedule_validation(&mut self, now_ms: i64) {
        self.validate_deadline = Some(now_ms + VALIDATION_DEBOUNCE_MS);
    }

    /// Fire due timers: the debounced validation and the `enterBlockHint`
    /// auto-clear. Called once per frame and before every key.
    pub fn poll(&mut self, now_ms: i64) {
        if let Some(deadline) = self.validate_deadline
            && now_ms >= deadline
        {
            self.validate_deadline = None;
            let value = self.current_value();
            self.run_validation(&value);
        }
        if let Some(until) = self.enter_block_until
            && now_ms >= until
        {
            self.enter_block_until = None;
        }
        self.text.poll(now_ms);
    }

    // ── Navigation ─────────────────────────────────────────────────────────

    /// Store `value` for the current step and move to the next non-skipped step
    /// resolved against the **merged** answers. Returns `Complete` past the end.
    fn advance(&mut self, value: String) -> WizardOutcome {
        let id = self.step_id().to_string();
        self.values.insert(id, value);
        let next = self.steps[self.index + 1..]
            .iter()
            .position(|s| !s.is_skipped(&self.values))
            .map(|offset| self.index + 1 + offset);
        match next {
            Some(idx) => {
                self.index = idx;
                self.prefill();
                WizardOutcome::Consumed
            }
            None => WizardOutcome::Complete(self.values.clone()),
        }
    }

    /// `Esc` / `Backspace` on an empty field — the previous non-skipped step,
    /// restoring its stored answer.
    fn go_back(&mut self) -> WizardOutcome {
        let prev = self.steps[..self.index]
            .iter()
            .rposition(|s| !s.is_skipped(&self.values));
        match prev {
            Some(idx) => {
                self.index = idx;
                let id = self.step_id().to_string();
                let stored = self.values.get(&id).cloned();
                self.prefill();
                if let Some(stored) = stored {
                    self.set_current_value(&stored);
                    self.dirty = true;
                }
                WizardOutcome::Consumed
            }
            None => WizardOutcome::Cancelled,
        }
    }

    fn set_current_value(&mut self, value: &str) {
        match self.step().kind {
            StepKind::Text => self.text.reset(value),
            StepKind::Textarea => {
                self.textarea = new_textarea(value);
                self.textarea.move_cursor(CursorMove::Bottom);
                self.textarea.move_cursor(CursorMove::End);
            }
            StepKind::Select => {
                if let Some(idx) = self.options().iter().position(|o| o.value == value) {
                    self.select_index = idx;
                }
            }
            StepKind::Multiselect => {
                self.multi_selected = value
                    .split(',')
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .collect();
            }
        }
    }

    // ── Key dispatch ───────────────────────────────────────────────────────

    pub fn handle_key(&mut self, key: KeyEvent, now_ms: i64) -> WizardOutcome {
        self.poll(now_ms);
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let meta = key.modifiers.contains(KeyModifiers::ALT)
            || key.modifiers.contains(KeyModifiers::SUPER);
        let kind = self.step().kind;

        // Paste image, text and textarea steps only. One condition, as in TS:
        // `(key.ctrl || key.meta) && (input === 'v' || input === 'i')` — the
        // `i` arm takes the modifier set the `v` arm does, so Cmd/Option+I
        // pastes too.
        if (ctrl || meta)
            && matches!(
                key.code,
                KeyCode::Char('v') | KeyCode::Char('V') | KeyCode::Char('i') | KeyCode::Char('I')
            )
            && matches!(kind, StepKind::Text | StepKind::Textarea)
        {
            return if self.paste_capable {
                WizardOutcome::PasteImage
            } else {
                WizardOutcome::Consumed
            };
        }

        if key.code == KeyCode::Esc {
            return self.go_back();
        }

        match kind {
            StepKind::Text => self.handle_text_key(key, now_ms, ctrl, meta),
            StepKind::Textarea => self.handle_textarea_key(key, ctrl, meta),
            StepKind::Select => self.handle_select_key(key),
            StepKind::Multiselect => self.handle_multiselect_key(key),
        }
    }

    fn confirm_text(&mut self, now_ms: i64) -> WizardOutcome {
        let value = self.text.value().trim().to_string();
        if self.step().required && value.is_empty() {
            self.dirty = true;
            self.run_validation(&value);
            if self.validated.as_ref().is_some_and(|(_, e)| e.is_none()) {
                // A required step with no validator still blocks on empty.
                self.validated = Some((value.clone(), Some("Required".into())));
            }
            self.enter_block_until = Some(now_ms + ENTER_BLOCK_HINT_MS);
            return WizardOutcome::Consumed;
        }
        // Always re-validate synchronously on confirm — this is what defeats a
        // stale debounced error.
        self.validate_deadline = None;
        self.run_validation(&value);
        if self.validated.as_ref().is_some_and(|(_, e)| e.is_some()) {
            self.dirty = true;
            self.enter_block_until = Some(now_ms + ENTER_BLOCK_HINT_MS);
            return WizardOutcome::Consumed;
        }
        self.advance(value)
    }

    fn handle_text_key(
        &mut self,
        key: KeyEvent,
        now_ms: i64,
        ctrl: bool,
        meta: bool,
    ) -> WizardOutcome {
        let suggestions = self.filtered_suggestions();

        if self.browsing {
            match key.code {
                KeyCode::Up => {
                    if self.suggestion_index == 0 {
                        self.browsing = false;
                    } else {
                        self.suggestion_index -= 1;
                    }
                    return WizardOutcome::Consumed;
                }
                KeyCode::Down => {
                    if !suggestions.is_empty() {
                        self.suggestion_index =
                            (self.suggestion_index + 1).min(suggestions.len() - 1);
                    }
                    return WizardOutcome::Consumed;
                }
                KeyCode::Enter => {
                    let Some(selected) = suggestions.get(self.suggestion_index).cloned() else {
                        return WizardOutcome::Consumed;
                    };
                    return match self.step().suggestion_mode {
                        SuggestionMode::Value => self.advance(selected.value),
                        SuggestionMode::Callback => {
                            WizardOutcome::SuggestionSelected(selected.value)
                        }
                    };
                }
                _ => {
                    // Any other key leaves browse mode and is processed normally.
                    self.browsing = false;
                }
            }
        }

        match key.code {
            KeyCode::Enter | KeyCode::Tab => return self.confirm_text(now_ms),
            KeyCode::Down if !suggestions.is_empty() => {
                self.browsing = true;
                self.suggestion_index = 0;
                return WizardOutcome::Consumed;
            }
            // Backspace on an empty field at step > 0 goes back a step; on the
            // first step it falls through to the editor, which consumes it and
            // marks the step dirty.
            KeyCode::Backspace | KeyCode::Delete
                if self.text.value().is_empty() && !ctrl && !meta && self.active_index() > 0 =>
            {
                return self.go_back();
            }
            _ => {}
        }

        if self.text.handle_key(key, now_ms) {
            self.dirty = true;
            self.browsing = false;
            self.schedule_validation(now_ms);
            return WizardOutcome::Consumed;
        }
        WizardOutcome::Ignored
    }

    fn handle_textarea_key(&mut self, key: KeyEvent, ctrl: bool, meta: bool) -> WizardOutcome {
        // Ctrl+Enter / Cmd+Enter / Tab confirm; plain and Shift+Enter insert a
        // newline (docs/parity/tui.md §5.1).
        if key.code == KeyCode::Tab || (key.code == KeyCode::Enter && (ctrl || meta)) {
            let value = self.textarea_value().trim().to_string();
            self.validate_deadline = None;
            self.run_validation(&value);
            if self.validated.as_ref().is_some_and(|(_, e)| e.is_some()) {
                self.dirty = true;
                return WizardOutcome::Consumed;
            }
            if self.step().required && value.is_empty() {
                self.dirty = true;
                return WizardOutcome::Consumed;
            }
            return self.advance(value);
        }

        if ctrl {
            match key.code {
                KeyCode::Char('a') | KeyCode::Char('A') => {
                    self.textarea.move_cursor(CursorMove::Head)
                }
                KeyCode::Char('e') | KeyCode::Char('E') => {
                    self.textarea.move_cursor(CursorMove::End)
                }
                KeyCode::Char('k') | KeyCode::Char('K') => {
                    self.textarea.delete_line_by_end();
                }
                KeyCode::Char('u') | KeyCode::Char('U') => {
                    self.textarea.delete_line_by_head();
                }
                KeyCode::Char('w') | KeyCode::Char('W') => {
                    self.textarea.delete_word();
                }
                _ => return WizardOutcome::Ignored,
            }
            self.dirty = true;
            return WizardOutcome::Consumed;
        }

        if meta {
            match key.code {
                KeyCode::Left | KeyCode::Char('b') | KeyCode::Char('B') => {
                    self.textarea.move_cursor(CursorMove::WordBack)
                }
                KeyCode::Right | KeyCode::Char('f') | KeyCode::Char('F') => {
                    self.textarea.move_cursor(CursorMove::WordForward)
                }
                _ => return WizardOutcome::Ignored,
            }
            return WizardOutcome::Consumed;
        }

        match key.code {
            KeyCode::Enter => {
                self.textarea.insert_newline();
                self.dirty = true;
            }
            KeyCode::Backspace | KeyCode::Delete => {
                self.textarea.delete_char();
                self.dirty = true;
            }
            KeyCode::Left => self.textarea.move_cursor(CursorMove::Back),
            KeyCode::Right => self.textarea.move_cursor(CursorMove::Forward),
            KeyCode::Up => self.textarea.move_cursor(CursorMove::Up),
            KeyCode::Down => self.textarea.move_cursor(CursorMove::Down),
            KeyCode::Home => self.textarea.move_cursor(CursorMove::Head),
            KeyCode::End => self.textarea.move_cursor(CursorMove::End),
            KeyCode::Char(ch) => {
                self.textarea.insert_char(ch);
                self.dirty = true;
            }
            _ => return WizardOutcome::Ignored,
        }
        WizardOutcome::Consumed
    }

    /// Multi-line paste: `\r\n` normalises to `\n` and the cursor lands at the
    /// end of the last pasted part.
    pub fn insert_paste(&mut self, text: &str, now_ms: i64) {
        match self.step().kind {
            StepKind::Textarea => {
                let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
                self.textarea.insert_str(&normalized);
                self.dirty = true;
            }
            StepKind::Text => {
                let flat = text.replace(['\r', '\n'], " ");
                self.text.insert_str(&flat, now_ms);
                self.dirty = true;
                self.schedule_validation(now_ms);
            }
            _ => {}
        }
    }

    fn handle_select_key(&mut self, key: KeyEvent) -> WizardOutcome {
        let options = self.options();
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.select_index = self.select_index.saturating_sub(1);
                WizardOutcome::Consumed
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if !options.is_empty() {
                    self.select_index = (self.select_index + 1).min(options.len() - 1);
                }
                WizardOutcome::Consumed
            }
            KeyCode::Backspace | KeyCode::Delete => self.go_back(),
            KeyCode::Enter | KeyCode::Tab => {
                let value = options
                    .get(self.select_index)
                    .map(|o| o.value.clone())
                    .unwrap_or_default();
                // Select validation is synchronous — no debounce.
                self.run_validation(&value);
                if self.validated.as_ref().is_some_and(|(_, e)| e.is_some()) {
                    self.dirty = true;
                    return WizardOutcome::Consumed;
                }
                self.advance(value)
            }
            KeyCode::Char(ch @ '1'..='9') => {
                // Number keys jump-select and confirm, bypassing `validate`.
                let idx = (ch as u8 - b'1') as usize;
                match options.get(idx) {
                    Some(choice) => {
                        self.select_index = idx;
                        let value = choice.value.clone();
                        self.advance(value)
                    }
                    None => WizardOutcome::Consumed,
                }
            }
            _ => WizardOutcome::Ignored,
        }
    }

    fn handle_multiselect_key(&mut self, key: KeyEvent) -> WizardOutcome {
        let options = self.options();
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.select_index = self.select_index.saturating_sub(1);
                WizardOutcome::Consumed
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if !options.is_empty() {
                    self.select_index = (self.select_index + 1).min(options.len() - 1);
                }
                WizardOutcome::Consumed
            }
            KeyCode::Backspace | KeyCode::Delete => self.go_back(),
            KeyCode::Char(' ') => {
                if let Some(choice) = options.get(self.select_index) {
                    if self.multi_selected.contains(&choice.value) {
                        self.multi_selected.shift_remove(&choice.value);
                    } else {
                        self.multi_selected.insert(choice.value.clone());
                    }
                    self.dirty = true;
                }
                WizardOutcome::Consumed
            }
            KeyCode::Enter | KeyCode::Tab => {
                // The submitted value is a comma-joined string, not an array.
                let value = self
                    .multi_selected
                    .iter()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(",");
                self.advance(value)
            }
            _ => WizardOutcome::Ignored,
        }
    }
}

fn new_textarea(initial: &str) -> TextArea<'static> {
    let lines: Vec<String> = if initial.is_empty() {
        vec![String::new()]
    } else {
        initial.split('\n').map(str::to_string).collect()
    };
    let mut textarea = TextArea::new(lines);
    textarea.set_wrap_mode(WrapMode::Word);
    textarea
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::CONTROL)
    }

    fn two_text_steps() -> Vec<WizardStep> {
        vec![
            WizardStep::new("a", "A", StepKind::Text)
                .required()
                .validate(|v| {
                    if v.trim().is_empty() {
                        Some("A is required".into())
                    } else {
                        None
                    }
                }),
            WizardStep::new("b", "B", StepKind::Text),
        ]
    }

    fn type_text(wizard: &mut Wizard, text: &str, now: i64) {
        for ch in text.chars() {
            wizard.handle_key(key(KeyCode::Char(ch)), now);
        }
    }

    #[test]
    fn text_steps_confirm_with_enter_and_tab_and_trim() {
        let mut wizard = Wizard::new(WizardKind::Task, "T", two_text_steps());
        type_text(&mut wizard, "  hello  ", 0);
        assert_eq!(
            wizard.handle_key(key(KeyCode::Enter), 0),
            WizardOutcome::Consumed
        );
        assert_eq!(wizard.step_id(), "b");
        type_text(&mut wizard, "world", 0);
        let outcome = wizard.handle_key(key(KeyCode::Tab), 0);
        match outcome {
            WizardOutcome::Complete(values) => {
                assert_eq!(values["a"], "hello");
                assert_eq!(values["b"], "world");
            }
            other => panic!("expected completion, got {other:?}"),
        }
    }

    #[test]
    fn a_required_empty_step_blocks_with_the_fix_hint() {
        let mut wizard = Wizard::new(WizardKind::Task, "T", two_text_steps());
        assert_eq!(
            wizard.handle_key(key(KeyCode::Enter), 0),
            WizardOutcome::Consumed
        );
        assert_eq!(wizard.step_id(), "a");
        assert!(wizard.enter_block_hint());
        wizard.poll(ENTER_BLOCK_HINT_MS);
        assert!(!wizard.enter_block_hint());
    }

    #[test]
    fn validation_is_debounced_by_300ms_for_text() {
        let mut wizard = Wizard::new(WizardKind::Task, "T", two_text_steps());
        type_text(&mut wizard, "x", 0);
        wizard.handle_key(key(KeyCode::Backspace), 10);
        assert_eq!(wizard.visible_error(), None);
        wizard.poll(200);
        assert_eq!(wizard.visible_error(), None, "still inside the debounce");
        wizard.poll(310);
        assert_eq!(wizard.visible_error(), Some("A is required"));
    }

    #[test]
    fn confirm_revalidates_and_defeats_a_stale_debounced_error() {
        let mut wizard = Wizard::new(WizardKind::Task, "T", two_text_steps());
        // Let the empty-value error land.
        wizard.handle_key(key(KeyCode::Backspace), 0);
        wizard.poll(400);
        assert_eq!(wizard.visible_error(), Some("A is required"));
        // Type a valid value and confirm only 20 ms later.
        type_text(&mut wizard, "ok", 400);
        let outcome = wizard.handle_key(key(KeyCode::Enter), 420);
        assert_eq!(outcome, WizardOutcome::Consumed);
        assert_eq!(wizard.step_id(), "b", "confirm must not be blocked");
    }

    #[test]
    fn errors_clear_on_forward_and_backward_navigation() {
        let mut wizard = Wizard::new(WizardKind::Task, "T", two_text_steps());
        wizard.handle_key(key(KeyCode::Enter), 0);
        wizard.poll(400);
        assert!(wizard.visible_error().is_some());
        type_text(&mut wizard, "ok", 400);
        wizard.handle_key(key(KeyCode::Enter), 800);
        assert_eq!(wizard.visible_error(), None);
        wizard.handle_key(key(KeyCode::Esc), 800);
        assert_eq!(wizard.step_id(), "a");
        assert_eq!(wizard.visible_error(), None);
    }

    #[test]
    fn esc_at_step_zero_cancels_and_later_steps_go_back() {
        let mut wizard = Wizard::new(WizardKind::Task, "T", two_text_steps());
        type_text(&mut wizard, "a", 0);
        wizard.handle_key(key(KeyCode::Enter), 0);
        assert_eq!(wizard.step_id(), "b");
        assert_eq!(
            wizard.handle_key(key(KeyCode::Esc), 0),
            WizardOutcome::Consumed
        );
        assert_eq!(wizard.step_id(), "a");
        assert_eq!(wizard.text_value(), "a", "the stored answer is restored");
        assert_eq!(
            wizard.handle_key(key(KeyCode::Esc), 0),
            WizardOutcome::Cancelled
        );
    }

    #[test]
    fn backspace_on_an_empty_field_goes_back_a_step() {
        let mut wizard = Wizard::new(WizardKind::Task, "T", two_text_steps());
        type_text(&mut wizard, "a", 0);
        wizard.handle_key(key(KeyCode::Enter), 0);
        assert_eq!(wizard.step_id(), "b");
        wizard.handle_key(key(KeyCode::Backspace), 0);
        assert_eq!(wizard.step_id(), "a");
    }

    #[test]
    fn backspace_on_an_empty_first_step_never_cancels() {
        let mut wizard = Wizard::new(WizardKind::Task, "T", two_text_steps());
        assert_eq!(
            wizard.handle_key(key(KeyCode::Backspace), 0),
            WizardOutcome::Consumed
        );
        assert_eq!(wizard.step_id(), "a");
    }

    #[test]
    fn skipped_steps_are_invisible_to_sequencing_and_counting() {
        let steps = vec![
            WizardStep::new("one", "One", StepKind::Text),
            WizardStep::new("two", "Two", StepKind::Text)
                .skip(|v| v.get("one").map(String::as_str) == Some("skip")),
            WizardStep::new("three", "Three", StepKind::Text),
        ];
        let mut wizard = Wizard::new(WizardKind::Task, "T", steps);
        assert_eq!(wizard.active_count(), 3);
        type_text(&mut wizard, "skip", 0);
        wizard.handle_key(key(KeyCode::Enter), 0);
        // The skip predicate is evaluated against the merged answers.
        assert_eq!(wizard.step_id(), "three");
        assert_eq!(wizard.active_count(), 2);
        assert_eq!(wizard.active_index(), 1);
    }

    #[test]
    fn select_steps_navigate_confirm_and_validate_synchronously() {
        let steps = vec![
            WizardStep::new("pick", "Pick", StepKind::Select)
                .options(vec![
                    Choice::new("1", "One", ""),
                    Choice::new("2", "Two", ""),
                    Choice::new("3", "Three", ""),
                ])
                .default_value("2")
                .validate(|v| {
                    if v == "3" {
                        Some("not three".into())
                    } else {
                        None
                    }
                }),
        ];
        let mut wizard = Wizard::new(WizardKind::Config, "C", steps);
        assert_eq!(wizard.select_index(), 1, "pre-filled from defaultValue");
        wizard.handle_key(key(KeyCode::Down), 0);
        assert_eq!(wizard.select_index(), 2);
        // Synchronous validation blocks immediately, with no poll().
        assert_eq!(
            wizard.handle_key(key(KeyCode::Enter), 0),
            WizardOutcome::Consumed
        );
        assert_eq!(wizard.visible_error(), Some("not three"));
        wizard.handle_key(key(KeyCode::Up), 0);
        match wizard.handle_key(key(KeyCode::Enter), 0) {
            WizardOutcome::Complete(values) => assert_eq!(values["pick"], "2"),
            other => panic!("expected completion, got {other:?}"),
        }
    }

    #[test]
    fn select_number_keys_jump_and_bypass_validation() {
        let steps = vec![
            WizardStep::new("pick", "Pick", StepKind::Select)
                .options(vec![
                    Choice::new("1", "One", ""),
                    Choice::new("3", "Three", ""),
                ])
                .validate(|_| Some("always invalid".into())),
        ];
        let mut wizard = Wizard::new(WizardKind::Config, "C", steps);
        match wizard.handle_key(key(KeyCode::Char('2')), 0) {
            WizardOutcome::Complete(values) => assert_eq!(values["pick"], "3"),
            other => panic!("expected completion, got {other:?}"),
        }
    }

    #[test]
    fn select_j_and_k_navigate_and_backspace_goes_back() {
        let steps = vec![
            WizardStep::new("t", "T", StepKind::Text),
            WizardStep::new("pick", "Pick", StepKind::Select)
                .options(vec![Choice::new("a", "A", ""), Choice::new("b", "B", "")]),
        ];
        let mut wizard = Wizard::new(WizardKind::Config, "C", steps);
        wizard.handle_key(key(KeyCode::Enter), 0);
        wizard.handle_key(key(KeyCode::Char('j')), 0);
        assert_eq!(wizard.select_index(), 1);
        wizard.handle_key(key(KeyCode::Char('k')), 0);
        assert_eq!(wizard.select_index(), 0);
        wizard.handle_key(key(KeyCode::Backspace), 0);
        assert_eq!(wizard.step_id(), "t");
    }

    #[test]
    fn multiselect_toggles_with_space_and_submits_a_comma_joined_string() {
        let steps = vec![
            WizardStep::new("members", "Members", StepKind::Multiselect)
                .options(vec![Choice::new("a1", "A", ""), Choice::new("a2", "B", "")]),
        ];
        let mut wizard = Wizard::new(WizardKind::Team, "T", steps);
        wizard.handle_key(key(KeyCode::Char(' ')), 0);
        wizard.handle_key(key(KeyCode::Down), 0);
        wizard.handle_key(key(KeyCode::Char(' ')), 0);
        assert_eq!(wizard.multi_selected().len(), 2);
        wizard.handle_key(key(KeyCode::Char(' ')), 0);
        assert_eq!(wizard.multi_selected().len(), 1);
        match wizard.handle_key(key(KeyCode::Enter), 0) {
            WizardOutcome::Complete(values) => assert_eq!(values["members"], "a1"),
            other => panic!("expected completion, got {other:?}"),
        }
    }

    #[test]
    fn textarea_enter_inserts_a_newline_and_ctrl_enter_confirms() {
        let steps = vec![WizardStep::new("body", "Body", StepKind::Textarea)];
        let mut wizard = Wizard::new(WizardKind::Task, "T", steps);
        type_text(&mut wizard, "line1", 0);
        wizard.handle_key(key(KeyCode::Enter), 0);
        type_text(&mut wizard, "line2", 0);
        assert_eq!(wizard.textarea_value(), "line1\nline2");
        match wizard.handle_key(ctrl(KeyCode::Enter), 0) {
            WizardOutcome::Complete(values) => assert_eq!(values["body"], "line1\nline2"),
            other => panic!("expected completion, got {other:?}"),
        }
    }

    #[test]
    fn textarea_tab_also_confirms() {
        let steps = vec![WizardStep::new("body", "Body", StepKind::Textarea)];
        let mut wizard = Wizard::new(WizardKind::Task, "T", steps);
        type_text(&mut wizard, "x", 0);
        assert!(matches!(
            wizard.handle_key(key(KeyCode::Tab), 0),
            WizardOutcome::Complete(_)
        ));
    }

    #[test]
    fn textarea_backspace_at_column_zero_merges_with_the_previous_line() {
        let steps = vec![WizardStep::new("body", "Body", StepKind::Textarea)];
        let mut wizard = Wizard::new(WizardKind::Task, "T", steps);
        type_text(&mut wizard, "Hello", 0);
        wizard.handle_key(key(KeyCode::Enter), 0);
        type_text(&mut wizard, "World", 0);
        // Cursor to the head of "World", then merge.
        wizard.handle_key(key(KeyCode::Home), 0);
        wizard.handle_key(key(KeyCode::Backspace), 0);
        assert_eq!(wizard.textarea_value(), "HelloWorld");
        // The cursor column must be prevLine.length — typing lands between.
        type_text(&mut wizard, "X", 0);
        assert_eq!(wizard.textarea_value(), "HelloXWorld");
    }

    #[test]
    fn textarea_backspace_at_the_origin_never_navigates_back() {
        let steps = vec![
            WizardStep::new("t", "T", StepKind::Text),
            WizardStep::new("body", "Body", StepKind::Textarea),
        ];
        let mut wizard = Wizard::new(WizardKind::Task, "T", steps);
        wizard.handle_key(key(KeyCode::Enter), 0);
        assert_eq!(wizard.step_id(), "body");
        wizard.handle_key(key(KeyCode::Backspace), 0);
        wizard.handle_key(key(KeyCode::Backspace), 0);
        assert_eq!(wizard.step_id(), "body");
    }

    #[test]
    fn textarea_emacs_kills_and_word_motions() {
        let steps = vec![WizardStep::new("body", "Body", StepKind::Textarea)];
        let mut wizard = Wizard::new(WizardKind::Task, "T", steps);
        type_text(&mut wizard, "alpha beta", 0);
        wizard.handle_key(ctrl(KeyCode::Char('w')), 0);
        assert_eq!(wizard.textarea_value(), "alpha ");
        wizard.handle_key(ctrl(KeyCode::Char('u')), 0);
        assert_eq!(wizard.textarea_value(), "");
        type_text(&mut wizard, "abc", 0);
        wizard.handle_key(ctrl(KeyCode::Char('a')), 0);
        wizard.handle_key(ctrl(KeyCode::Char('k')), 0);
        assert_eq!(wizard.textarea_value(), "");
    }

    #[test]
    fn textarea_multiline_paste_normalises_crlf() {
        let steps = vec![WizardStep::new("body", "Body", StepKind::Textarea)];
        let mut wizard = Wizard::new(WizardKind::Task, "T", steps);
        type_text(&mut wizard, "start", 0);
        wizard.insert_paste("one\r\ntwo\nthree", 0);
        assert_eq!(wizard.textarea_value(), "startone\ntwo\nthree");
        type_text(&mut wizard, "!", 0);
        assert!(wizard.textarea_value().ends_with("three!"));
    }

    #[test]
    fn paste_image_fires_on_text_and_textarea_only_when_capable() {
        let steps = vec![
            WizardStep::new("t", "T", StepKind::Text),
            WizardStep::new("pick", "Pick", StepKind::Select)
                .options(vec![Choice::new("a", "A", "")]),
        ];
        let mut wizard = Wizard::new(WizardKind::Task, "T", steps.clone()).with_paste(true);
        assert_eq!(
            wizard.handle_key(ctrl(KeyCode::Char('v')), 0),
            WizardOutcome::PasteImage
        );
        assert_eq!(
            wizard.handle_key(ctrl(KeyCode::Char('i')), 0),
            WizardOutcome::PasteImage
        );
        wizard.handle_key(key(KeyCode::Enter), 0);
        assert_eq!(wizard.step_id(), "pick");
        assert_ne!(
            wizard.handle_key(ctrl(KeyCode::Char('v')), 0),
            WizardOutcome::PasteImage,
            "select steps ignore paste"
        );

        // Without the capability it is a consumed no-op, never a crash.
        let mut wizard = Wizard::new(WizardKind::Task, "T", steps);
        assert_eq!(
            wizard.handle_key(ctrl(KeyCode::Char('v')), 0),
            WizardOutcome::Consumed
        );
    }

    #[test]
    fn paste_takes_the_same_modifier_set_for_v_and_i() {
        // TS is one condition — `(key.ctrl || key.meta) && (input === 'v' || input
        // === 'i')` — so Cmd/Option+I pastes just like Cmd/Option+V.
        let steps = vec![
            WizardStep::new("t", "T", StepKind::Text),
            WizardStep::new("body", "Body", StepKind::Textarea),
        ];
        for code in [
            KeyCode::Char('v'),
            KeyCode::Char('V'),
            KeyCode::Char('i'),
            KeyCode::Char('I'),
        ] {
            for modifiers in [
                KeyModifiers::CONTROL,
                KeyModifiers::ALT,
                KeyModifiers::SUPER,
            ] {
                let mut wizard = Wizard::new(WizardKind::Task, "T", steps.clone()).with_paste(true);
                assert_eq!(
                    wizard.handle_key(KeyEvent::new(code, modifiers), 0),
                    WizardOutcome::PasteImage,
                    "text step: {code:?} + {modifiers:?}"
                );
                // And again on the textarea step.
                wizard.handle_key(key(KeyCode::Enter), 0);
                assert_eq!(wizard.step_id(), "body");
                assert_eq!(
                    wizard.handle_key(KeyEvent::new(code, modifiers), 0),
                    WizardOutcome::PasteImage,
                    "textarea step: {code:?} + {modifiers:?}"
                );
            }
        }
    }

    #[test]
    fn suggestions_browse_mode_filters_and_submits_the_value() {
        let steps = vec![
            WizardStep::new("model", "Model", StepKind::Text)
                .suggestions(vec![
                    Choice::new("opus", "Opus", "big"),
                    Choice::new("sonnet", "Sonnet", "mid"),
                ])
                .suggestion_mode(SuggestionMode::Value),
        ];
        let mut wizard = Wizard::new(WizardKind::Agent, "A", steps);
        type_text(&mut wizard, "son", 0);
        assert_eq!(wizard.filtered_suggestions().len(), 1);
        wizard.handle_key(key(KeyCode::Down), 0);
        assert!(wizard.is_browsing());
        match wizard.handle_key(key(KeyCode::Enter), 0) {
            WizardOutcome::Complete(values) => assert_eq!(values["model"], "sonnet"),
            other => panic!("expected completion, got {other:?}"),
        }
    }

    #[test]
    fn callback_suggestions_report_the_selection_to_the_host() {
        let steps = vec![
            WizardStep::new("name", "Name", StepKind::Text).suggestions(vec![Choice::new(
                "backend-dev",
                "Backend Developer",
                "builds APIs",
            )]),
        ];
        let mut wizard = Wizard::new(WizardKind::Agent, "A", steps);
        wizard.handle_key(key(KeyCode::Down), 0);
        assert_eq!(
            wizard.handle_key(key(KeyCode::Enter), 0),
            WizardOutcome::SuggestionSelected("backend-dev".into())
        );
    }

    #[test]
    fn up_at_the_first_suggestion_leaves_browse_mode() {
        let steps = vec![
            WizardStep::new("name", "Name", StepKind::Text)
                .suggestions(vec![Choice::new("a", "A", ""), Choice::new("b", "B", "")]),
        ];
        let mut wizard = Wizard::new(WizardKind::Agent, "A", steps);
        wizard.handle_key(key(KeyCode::Down), 0);
        wizard.handle_key(key(KeyCode::Down), 0);
        assert_eq!(wizard.suggestion_index(), 1);
        wizard.handle_key(key(KeyCode::Up), 0);
        assert_eq!(wizard.suggestion_index(), 0);
        wizard.handle_key(key(KeyCode::Up), 0);
        assert!(!wizard.is_browsing());
    }

    #[test]
    fn typing_exits_browse_mode() {
        let steps = vec![
            WizardStep::new("name", "Name", StepKind::Text)
                .suggestions(vec![Choice::new("alpha", "Alpha", "")]),
        ];
        let mut wizard = Wizard::new(WizardKind::Agent, "A", steps);
        wizard.handle_key(key(KeyCode::Down), 0);
        assert!(wizard.is_browsing());
        wizard.handle_key(key(KeyCode::Char('a')), 0);
        assert!(!wizard.is_browsing());
        assert_eq!(wizard.text_value(), "a");
    }

    #[test]
    fn replace_steps_keeps_the_answers_and_position() {
        let mut wizard = Wizard::new(WizardKind::Agent, "A", two_text_steps());
        type_text(&mut wizard, "kept", 0);
        wizard.handle_key(key(KeyCode::Enter), 0);
        assert_eq!(wizard.step_id(), "b");
        wizard.replace_steps(two_text_steps());
        assert_eq!(wizard.step_id(), "b");
        assert_eq!(wizard.values()["a"], "kept");
    }
}
