//! The `FormWizard` engine.
//!
//! Step sequencing, skip predicates, per-step pre-fill, validation gating and
//! the key map are all contract: a wrong skip predicate writes corrupt entity
//! data through the UI.
//!
//! Two deliberate decisions:
//!
//! * **`Esc`** goes back one step and cancels only at step 0. With a single
//!   dispatcher the wizard and the App handler never both see `Esc`.
//! * **Cancelling always clears pending attachments**, so none leak into the
//!   next wizard.
//!
//! The `textarea` step delegates to `tui-textarea-2` (ARCHITECTURE §4.8) for
//! the buffer, undo, word motions and soft wrapping.

use indexmap::{IndexMap, IndexSet};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use tui_textarea::{CursorMove, TextArea, WrapMode};

use crate::text::{TextInput, is_meta};
use crate::wizard::picker::{AgentPicker, PickerOrder, push_query};

/// Collected wizard answers, keyed by step id, in step order.
pub type Values = IndexMap<String, String>;

/// Validation debounce for text and textarea steps.
pub const VALIDATION_DEBOUNCE_MS: i64 = 300;
/// How long `Fix the error above` stays up.
pub const ENTER_BLOCK_HINT_MS: i64 = 2_000;
/// The blocked-confirm hint text.
pub const FIX_THE_ERROR_ABOVE: &str = "Fix the error above";
/// Confirming an agent step whose filter hides every agent.
pub const NO_AGENT_MATCHES: &str = "No agent matches the filter";
/// Confirming an agent step that has no agent to list, with nothing typed:
/// every agent is disabled, or none exists yet.
pub const NO_AGENT_TO_CHOOSE: &str = "No agent to choose — add or enable one first";
/// Confirming a members step that still ticks a refused agent.
pub const UNTICK_UNAVAILABLE: &str = "Untick the unavailable agents to continue";

/// One agent choice the final-submit check refused (R7, D7): the step that
/// holds it, the agent id, and the row to list it as while it is still
/// ticked on a `Multiselect` step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefusedAgent {
    pub step_id: String,
    pub choice: Choice,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepKind {
    Text,
    Textarea,
    Select,
    Multiselect,
}

/// `value` advances with the chosen value, `callback` hands
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
    /// The default value, resolved against the values entered so far.
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
    /// Set on agent steps: typing filters the listed agents and `Ctrl+O`
    /// reorders them (R7, R8). See [`crate::wizard::picker`].
    pub picker: Option<AgentPicker>,
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
            picker: None,
        }
    }

    /// Make a `Select` / `Multiselect` agent step filterable and orderable.
    pub fn agent_picker(mut self, picker: AgentPicker) -> Self {
        self.picker = Some(picker);
        self
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

#[derive(Clone)]
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
    /// The backend can paste an image (task / edit_task only).
    pub paste_capable: bool,

    pub(crate) baseline: Values,
    pub(crate) source_defaults: Values,
    scratch: IndexMap<String, FieldState>,
    pub session_notice: Option<String>,
    pub session_error: Option<String>,
    pub copy_rail: Option<usize>,
    pub copy_notice: Option<String>,
    pub suppress_validation_footer: bool,
    pub(crate) viewport_rows: std::cell::Cell<usize>,
    missing_selection: Option<String>,
    pub(crate) text_render: std::cell::Cell<Option<(ratatui::layout::Rect, usize, usize)>>,
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

    /// The agent step's type-ahead filter. Cleared on every step change.
    picker_query: String,
    /// The agent steps' order; kept for the wizard's lifetime.
    picker_order: PickerOrder,
    /// Ticked members the final-submit check refused, by step id. Each is
    /// listed first on its step while still ticked, so it can be seen and
    /// unticked whatever the filter; it cannot be ticked again.
    refused: IndexMap<String, Vec<Choice>>,
}

#[derive(Clone)]
struct FieldState {
    text: TextInput,
    textarea: TextArea<'static>,
    select_index: usize,
    value: String,
    multi_selected: IndexSet<String>,
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
            baseline: Values::new(),
            source_defaults: Values::new(),
            scratch: IndexMap::new(),
            session_notice: None,
            session_error: None,
            copy_rail: None,
            copy_notice: None,
            suppress_validation_footer: false,
            viewport_rows: std::cell::Cell::new(1),
            missing_selection: None,
            text_render: std::cell::Cell::new(None),
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
            picker_query: String::new(),
            picker_order: PickerOrder::default(),
            refused: IndexMap::new(),
        };
        wizard.index = wizard.first_active_index();
        wizard.prefill();
        wizard.source_defaults = wizard.default_values();
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

    /// Scroll the textarea one wheel notch.
    ///
    /// `tui-textarea-2` supports exactly `MouseScrollUp`/`MouseScrollDown`
    /// and nothing else from a mouse, so the wheel is forwarded to it rather
    /// than reimplemented here (ARCHITECTURE §4.8).
    pub fn scroll_textarea(&mut self, up: bool) {
        self.textarea.input(tui_textarea::Input {
            key: if up {
                tui_textarea::Key::MouseScrollUp
            } else {
                tui_textarea::Key::MouseScrollDown
            },
            ctrl: false,
            alt: false,
            shift: false,
        });
    }

    pub fn textarea_value(&self) -> String {
        self.textarea.lines().join("\n")
    }

    /// The current step's options as listed: resolved against the current
    /// answers and, on an agent step, filtered and ordered by the picker.
    /// Row numbers, digit picks, clicks and the cursor all index this list.
    pub fn options(&self) -> Vec<Choice> {
        let options = self.step().options.resolve(&self.values);
        let arranged = match &self.step().picker {
            Some(picker) => picker.arrange(options, &self.picker_query, self.picker_order),
            None => options,
        };
        let refused = self.refused_ticked();
        if refused.is_empty() {
            return arranged;
        }
        refused.into_iter().chain(arranged).collect()
    }

    /// The refused agents this `Multiselect` step still ticks.
    fn refused_ticked(&self) -> Vec<Choice> {
        if self.step().kind != StepKind::Multiselect {
            return Vec::new();
        }
        self.refused
            .get(self.step_id())
            .map(|rows| {
                rows.iter()
                    .filter(|c| self.multi_selected.contains(&c.value))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }

    /// A refused single choice is waiting to be replaced: no row is chosen
    /// and confirming is refused until one is.
    pub fn selection_missing(&self) -> bool {
        self.missing_selection.is_some()
    }

    /// Mark the current `Select` step's answer as no longer offered. The
    /// cursor parks past the last row, so no row is drawn as the answer and
    /// a click on any row moves onto it (the mouse moves by `Up`/`Down`).
    fn mark_missing(&mut self, value: String, error: &str) {
        self.missing_selection = Some(value);
        self.session_error = Some(error.into());
        self.select_index = self.options().len();
    }

    /// The final-submit check refused some agent choices (R7, D7).
    ///
    /// `steps` are rebuilt from the roster that check read, so they offer
    /// only agents eligible now. Each refused single choice is reopened with
    /// nothing chosen; it never becomes the sentinel or another row. Each
    /// refused member stays ticked and is listed on its step until it is
    /// unticked. The wizard opens the earliest refused step with the filter
    /// cleared and `message` shown; every other answer is kept.
    ///
    /// With no `steps` (an edited entity no longer listed) only `message` is
    /// shown; the next submission is checked again all the same.
    pub fn reject_agents(
        &mut self,
        steps: Vec<WizardStep>,
        refused: &[RefusedAgent],
        message: &str,
    ) {
        if steps.is_empty() {
            self.session_error = Some(message.into());
            return;
        }
        self.save_step();
        self.steps = steps;
        self.refused.clear();
        for agent in refused {
            if self
                .steps
                .iter()
                .any(|s| s.id == agent.step_id && s.kind == StepKind::Multiselect)
            {
                self.refused
                    .entry(agent.step_id.clone())
                    .or_default()
                    .push(agent.choice.clone());
            }
        }
        let target = self
            .steps
            .iter()
            .position(|s| refused.iter().any(|r| r.step_id == s.id))
            .unwrap_or_else(|| self.first_active_index());
        self.index = target;
        self.prefill();
        self.session_error = Some(message.into());
    }

    /// The current step's agent picker, if it is an agent step.
    pub fn picker(&self) -> Option<&AgentPicker> {
        self.step().picker.as_ref()
    }

    /// The agent step's filter text.
    pub fn picker_query(&self) -> &str {
        &self.picker_query
    }

    /// The agent steps' current order.
    pub fn picker_order(&self) -> PickerOrder {
        self.picker_order
    }

    /// A typed filter that lists no agent: nothing is chosen, so confirming
    /// is refused rather than falling through to the sentinel. A sentinel row
    /// is still listed and still picked by its digit.
    pub fn picker_blocked(&self) -> bool {
        self.picker().is_some_and(|picker| {
            !self.picker_query.trim().is_empty()
                && !self.options().iter().any(|c| picker.is_agent(&c.value))
        })
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
        self.missing_selection = None;
        // Before any index is computed: the cursor indexes the listed rows.
        self.picker_query.clear();
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
                let options = self.options();
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
        let effective = self.current_value();
        self.baseline.entry(step.id.clone()).or_insert(effective);
        if let Some(saved) = self.scratch.get(&step.id).cloned() {
            self.text = saved.text;
            self.textarea = saved.textarea;
            self.select_index = saved.select_index;
            if step.kind == StepKind::Select {
                if let Some(at) = self.options().iter().position(|o| o.value == saved.value) {
                    self.select_index = at;
                } else {
                    self.mark_missing(
                        saved.value,
                        "Saved option is no longer available; choose again",
                    );
                }
            }
            self.multi_selected = saved.multi_selected;
        } else if let Some(value) = self.values.get(&step.id).cloned() {
            self.set_current_value(&value);
        }
        self.reset_step_state();
        self.flag_refused_ticked();
    }

    /// Say why a members step will not confirm while it ticks a refused
    /// agent, before the user tries.
    fn flag_refused_ticked(&mut self) {
        if !self.refused_ticked().is_empty() {
            self.dirty = true;
            self.validated = Some((self.current_value(), Some(UNTICK_UNAVAILABLE.into())));
        }
    }

    pub(crate) fn save_step(&mut self) {
        self.scratch.insert(
            self.step_id().to_string(),
            FieldState {
                text: self.text.clone(),
                textarea: self.textarea.clone(),
                select_index: self.select_index,
                value: self.current_value(),
                multi_selected: self.multi_selected.clone(),
            },
        );
    }

    pub fn default_values(&self) -> Values {
        let mut values = Values::new();
        for step in &self.steps {
            values.insert(step.id.clone(), step.default_value.resolve(&values));
        }
        values
    }

    pub fn semantic_values(&self) -> Values {
        let mut values = self.values.clone();
        for (id, field) in &self.scratch {
            if let Some(step) = self.steps.iter().find(|s| s.id == *id) {
                let value = match step.kind {
                    StepKind::Text => field.text.value().to_string(),
                    StepKind::Textarea => field.textarea.lines().join("\n"),
                    StepKind::Select => field.value.clone(),
                    StepKind::Multiselect => field
                        .multi_selected
                        .iter()
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(","),
                };
                values.insert(id.clone(), value);
            }
        }
        values.insert(self.step_id().to_string(), self.current_value());
        values
    }

    pub fn semantic_dirty(&self, attachments: &[String]) -> bool {
        !attachments.is_empty()
            || self.semantic_values().iter().any(|(id, value)| {
                let value = normalized(value);
                let baseline = normalized(self.baseline.get(id).map(String::as_str).unwrap_or(""));
                if self
                    .steps
                    .iter()
                    .any(|s| s.id == *id && s.kind == StepKind::Multiselect)
                {
                    // Compare membership only; drafts and submission keep insertion order.
                    value
                        .split(',')
                        .filter(|s| !s.is_empty())
                        .collect::<IndexSet<_>>()
                        != baseline
                            .split(',')
                            .filter(|s| !s.is_empty())
                            .collect::<IndexSet<_>>()
                } else {
                    value != baseline
                }
            })
    }

    pub(crate) fn discard_current(&mut self) {
        let id = self.step_id().to_string();
        let value = self.baseline.get(&id).cloned().unwrap_or_default();
        self.scratch.shift_remove(&id);
        self.values.shift_remove(&id);
        self.set_current_value(&value);
    }

    pub(crate) fn draft_snapshot(&self) -> Self {
        let mut snapshot = self.clone();
        // Drafts promise source/caret/selection recovery, not unbounded hidden
        // undo allocations. The active editor's library history is untouched.
        snapshot.text.clear_history();
        snapshot.textarea = snapshot_textarea(&snapshot.textarea);
        for field in snapshot.scratch.values_mut() {
            field.text.clear_history();
            field.textarea = snapshot_textarea(&field.textarea);
        }
        snapshot
    }

    pub(crate) fn retained_bytes(&self) -> usize {
        let values_size = |v: &Values| v.iter().map(|(k, v)| k.len() + v.len()).sum::<usize>();
        values_size(&self.baseline)
            + values_size(&self.source_defaults)
            + values_size(&self.values)
            + self.text.value().len()
            + self.picker_query.len()
            + self.textarea.lines().iter().map(String::len).sum::<usize>()
            + self
                .scratch
                .iter()
                .map(|(k, f)| {
                    k.len()
                        + f.text.value().len()
                        + f.textarea.lines().iter().map(String::len).sum::<usize>()
                        + f.value.len()
                        + f.multi_selected.iter().map(String::len).sum::<usize>()
                })
                .sum::<usize>()
            + self
                .refused
                .iter()
                .map(|(k, rows)| {
                    k.len()
                        + rows
                            .iter()
                            .map(|c| c.value.len() + c.label.len() + c.hint.len())
                            .sum::<usize>()
                })
                .sum::<usize>()
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
    /// live model-catalog refresh.
    pub fn replace_steps(&mut self, steps: Vec<WizardStep>) {
        if steps.is_empty() {
            return;
        }
        let current_id = self.step_id().to_string();
        let current = self.current_value();
        let untouched = self
            .baseline
            .get(&current_id)
            .is_some_and(|v| normalized(v) == normalized(&current));
        self.steps = steps;
        if let Some(index) = self.steps.iter().position(|s| s.id == current_id) {
            self.index = index;
            // Catalog reordering must not select a different value by index.
            if self.step().kind == StepKind::Select {
                // An agent that is no longer offered is never swapped for the
                // default, which on an agent step is the sentinel (R7, D7).
                let lost_agent = self.step().picker.is_some() && !current.is_empty();
                if let Some(at) = self.options().iter().position(|c| c.value == current) {
                    self.select_index = at;
                } else if untouched && !lost_agent {
                    let default = self.step().default_value.resolve(&self.values);
                    self.select_index = self
                        .options()
                        .iter()
                        .position(|c| c.value == default)
                        .unwrap_or(0);
                    self.baseline.insert(current_id, self.current_value());
                } else {
                    self.mark_missing(
                        current,
                        "Selected option is no longer available; choose an option again",
                    );
                }
                self.save_step();
            }
        } else {
            self.index = self.first_active_index();
            self.prefill();
        }
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

    pub fn current_value(&self) -> String {
        match self.step().kind {
            StepKind::Text => self.text.value().to_string(),
            StepKind::Textarea => self.textarea_value(),
            StepKind::Select if self.missing_selection.is_some() => {
                self.missing_selection.clone().unwrap_or_default()
            }
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

    /// Fire due timers: the debounced validation and the enter-block hint
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
        self.save_step();
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
    pub(crate) fn go_back(&mut self) -> WizardOutcome {
        self.save_step();
        let prev = self.steps[..self.index]
            .iter()
            .rposition(|s| !s.is_skipped(&self.values));
        match prev {
            Some(idx) => {
                self.index = idx;
                self.prefill();
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
                self.missing_selection = None;
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
        let meta = is_meta(key);
        let kind = self.step().kind;

        // Paste image, text and textarea steps only. One condition for both
        // keys, so the `i` arm takes the modifier set the `v` arm does and
        // Alt/Option+I pastes too. Meta is Alt, never Cmd — see
        // `text::is_meta`.
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
            // Esc clears an agent filter before it leaves the step.
            if self.picker().is_some() && !self.picker_query.is_empty() {
                let before = self.cursor_value();
                self.picker_query.clear();
                self.validated = None;
                self.reanchor(before, false);
                return WizardOutcome::Consumed;
            }
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
            if self.option_navigation(key.code, suggestions.len(), true) {
                return WizardOutcome::Consumed;
            }
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

        if self.text.handle_editor_key(key, now_ms) {
            self.dirty = true;
            self.browsing = false;
            self.schedule_validation(now_ms);
            return WizardOutcome::Consumed;
        }
        WizardOutcome::Ignored
    }

    fn handle_textarea_key(&mut self, key: KeyEvent, ctrl: bool, meta: bool) -> WizardOutcome {
        // Ctrl+Enter / Cmd+Enter / Tab confirm; plain and Shift+Enter insert a
        // newline.
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

        if key.modifiers.contains(KeyModifiers::SHIFT)
            && matches!(
                key.code,
                KeyCode::Left
                    | KeyCode::Right
                    | KeyCode::Up
                    | KeyCode::Down
                    | KeyCode::Home
                    | KeyCode::End
            )
        {
            self.textarea.input(key);
            return WizardOutcome::Consumed;
        }
        if ctrl {
            match key.code {
                KeyCode::Char('z') | KeyCode::Char('Z') => {
                    self.textarea.undo();
                }
                KeyCode::Char('r') | KeyCode::Char('R') => {
                    self.textarea.redo();
                }
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

        if matches!(
            key.code,
            KeyCode::Left
                | KeyCode::Right
                | KeyCode::Up
                | KeyCode::Down
                | KeyCode::Home
                | KeyCode::End
        ) {
            self.textarea.cancel_selection();
        }
        match key.code {
            KeyCode::PageUp | KeyCode::PageDown => {
                self.textarea.input(key);
            }
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

    pub fn selected_text(&self) -> Option<String> {
        match self.step().kind {
            StepKind::Text => self.text.selected_text(),
            StepKind::Textarea => {
                let ((sr, sc), (er, ec)) = self.textarea.selection_range()?;
                if (sr, sc) == (er, ec) {
                    return None;
                }
                Some(
                    self.textarea.lines()[sr..=er]
                        .iter()
                        .enumerate()
                        .map(|(i, line)| {
                            let start = if i == 0 { sc } else { 0 };
                            let end = if sr + i == er {
                                ec
                            } else {
                                line.chars().count()
                            };
                            line.chars()
                                .skip(start)
                                .take(end.saturating_sub(start))
                                .collect::<String>()
                        })
                        .collect::<Vec<_>>()
                        .join("\n"),
                )
            }
            _ => None,
        }
    }

    pub(crate) fn pointer(
        &mut self,
        position: ratatui::layout::Position,
        body: ratatui::layout::Rect,
        start: bool,
    ) {
        let position = ratatui::layout::Position::new(
            position.x.clamp(body.x, body.right().saturating_sub(1)),
            position.y.clamp(body.y, body.bottom().saturating_sub(1)),
        );
        match self.step().kind {
            StepKind::Textarea => {
                if let Some((row, col)) = self.textarea.cursor_at_position(position) {
                    if start {
                        self.textarea.cancel_selection();
                    }
                    self.textarea.move_cursor(CursorMove::Jump(
                        row.min(u16::MAX as usize) as u16,
                        col.min(u16::MAX as usize) as u16,
                    ));
                    if start {
                        self.textarea.start_selection();
                    }
                }
            }
            StepKind::Text => {
                let Some((rendered, start_at, draw_cursor)) = self.text_render.get() else {
                    return;
                };
                if rendered.y != body.y || rendered.x != body.x || rendered.width != body.width {
                    return;
                }
                let mut cells = 0;
                let mut at = start_at;
                let mut target = position.x.saturating_sub(body.x + 2) as usize;
                let segments = crate::text::grapheme_segments(self.text.value());
                let before_width =
                    crate::text::text_display_width(&segments[start_at..draw_cursor].concat());
                // Map against the last drawn block/viewport, not the moving caret.
                if target > before_width {
                    target -= 1;
                }
                for segment in crate::text::grapheme_segments(self.text.value())
                    .iter()
                    .skip(start_at)
                {
                    let w = crate::text::display_width(segment);
                    if cells + w > target {
                        break;
                    }
                    cells += w;
                    at += 1;
                }
                self.text.select_to(at, start);
            }
            _ => {}
        }
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
                self.text.insert_editor_str(&flat, now_ms);
                self.dirty = true;
                self.schedule_validation(now_ms);
            }
            // A paste on an agent step is filter text, digits included.
            StepKind::Select | StepKind::Multiselect if self.picker().is_some() => {
                let before = self.cursor_value();
                if push_query(&mut self.picker_query, text) {
                    self.clear_missing_selection();
                    self.reanchor(before, true);
                }
            }
            _ => {}
        }
    }

    /// The value under the list cursor, if any row is listed.
    fn cursor_value(&self) -> Option<String> {
        self.options()
            .get(self.select_index)
            .map(|c| c.value.clone())
    }

    fn clear_missing_selection(&mut self) {
        if self.missing_selection.take().is_some() {
            self.session_error = None;
        }
    }

    /// Put the cursor back on a row after the agent filter or order changed.
    ///
    /// The cursor follows the agent it was on while that agent is listed. A
    /// query edit that hid it, or that was typed with the sentinel under the
    /// cursor, lands on the first matching agent, so typing a name and
    /// pressing Enter picks that agent rather than `Auto-assign`. With no
    /// match the cursor rests on the first row (the sentinel, if any).
    fn reanchor(&mut self, before: Option<String>, query_edit: bool) {
        let options = self.options();
        let Some(picker) = self.picker() else {
            return;
        };
        let at = |value: &str| options.iter().position(|c| c.value == value);
        let kept = before.as_deref().and_then(|value| {
            let is_agent = picker.is_agent(value);
            (!query_edit || self.picker_query.trim().is_empty() || is_agent)
                .then(|| at(value))
                .flatten()
        });
        self.select_index = kept
            .or_else(|| {
                query_edit
                    .then(|| options.iter().position(|c| picker.is_agent(&c.value)))
                    .flatten()
            })
            .unwrap_or(0);
    }

    /// The agent-step keys that come before list navigation: printable
    /// characters and `Backspace` edit the filter, `Ctrl+U` clears it and
    /// `Ctrl+O` cycles the order. Digits `1`–`9` stay row picks on a `Select`
    /// step and `Space` stays the toggle on a `Multiselect` one.
    fn handle_picker_key(&mut self, key: KeyEvent) -> Option<WizardOutcome> {
        self.picker()?;
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let plain = !ctrl && !is_meta(key);
        let multiselect = self.step().kind == StepKind::Multiselect;
        let before = self.cursor_value();
        match key.code {
            KeyCode::Char('o' | 'O') if ctrl => {
                self.picker_order = self.picker_order.next();
                self.reanchor(before, false);
            }
            KeyCode::Char('u' | 'U') if ctrl && !self.picker_query.is_empty() => {
                self.picker_query.clear();
                self.reanchor(before, true);
            }
            KeyCode::Char('1'..='9') if !multiselect => return None,
            KeyCode::Char(' ') if multiselect => return None,
            KeyCode::Char(ch) if plain => {
                if push_query(&mut self.picker_query, ch.encode_utf8(&mut [0; 4])) {
                    self.reanchor(before, true);
                }
            }
            KeyCode::Backspace | KeyCode::Delete if plain && !self.picker_query.is_empty() => {
                self.picker_query.pop();
                self.reanchor(before, true);
            }
            _ => return None,
        }
        self.clear_missing_selection();
        // A refusal belonged to the previous filter.
        self.validated = None;
        Some(WizardOutcome::Consumed)
    }

    fn option_navigation(&mut self, key: KeyCode, len: usize, suggestions: bool) -> bool {
        let current = if suggestions {
            self.suggestion_index
        } else {
            self.select_index
        };
        let page = self.viewport_rows.get().saturating_sub(1).max(1);
        let next = match key {
            KeyCode::Home => 0,
            KeyCode::End => len.saturating_sub(1),
            KeyCode::PageUp => current.saturating_sub(page),
            KeyCode::PageDown => (current + page).min(len.saturating_sub(1)),
            _ => return false,
        };
        if suggestions {
            self.suggestion_index = next;
        } else {
            self.select_index = next;
        }
        true
    }

    fn handle_select_key(&mut self, key: KeyEvent) -> WizardOutcome {
        if self.missing_selection.is_some() {
            if matches!(key.code, KeyCode::Enter | KeyCode::Tab) {
                return WizardOutcome::Consumed;
            }
            if matches!(
                key.code,
                KeyCode::Up
                    | KeyCode::Down
                    | KeyCode::Home
                    | KeyCode::End
                    | KeyCode::PageUp
                    | KeyCode::PageDown
                    | KeyCode::Char('1'..='9' | 'j' | 'k')
            ) {
                self.missing_selection = None;
                self.session_error = None;
            }
        }
        if let Some(outcome) = self.handle_picker_key(key) {
            return outcome;
        }
        let options = self.options();
        if self.option_navigation(key.code, options.len(), false) {
            return WizardOutcome::Consumed;
        }
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.select_index = self.select_index.saturating_sub(1);
                WizardOutcome::Consumed
            }
            KeyCode::Down | KeyCode::Char('j') => {
                // From the parked cursor of a missing selection, Down enters
                // the list at the top.
                if self.select_index >= options.len() {
                    self.select_index = 0;
                } else {
                    self.select_index = (self.select_index + 1).min(options.len() - 1);
                }
                WizardOutcome::Consumed
            }
            KeyCode::Backspace | KeyCode::Delete => self.go_back(),
            // A filter that hides every agent, or an agent step with no agent
            // at all, leaves nothing to choose; the step stays open rather
            // than submitting an empty answer. Only the first blames the filter.
            KeyCode::Enter | KeyCode::Tab
                if self.picker_blocked()
                    || (self.picker().is_some() && options.get(self.select_index).is_none()) =>
            {
                let error = if self.picker_query.trim().is_empty() {
                    NO_AGENT_TO_CHOOSE
                } else {
                    NO_AGENT_MATCHES
                };
                self.dirty = true;
                self.validated = Some((self.current_value(), Some(error.into())));
                WizardOutcome::Consumed
            }
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
        if let Some(outcome) = self.handle_picker_key(key) {
            return outcome;
        }
        let options = self.options();
        if self.option_navigation(key.code, options.len(), false) {
            return WizardOutcome::Consumed;
        }
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
                    // An unticked refused row leaves the list.
                    let len = self.options().len();
                    self.select_index = self.select_index.min(len.saturating_sub(1));
                }
                WizardOutcome::Consumed
            }
            KeyCode::Enter | KeyCode::Tab if !self.refused_ticked().is_empty() => {
                self.dirty = true;
                self.validated = Some((self.current_value(), Some(UNTICK_UNAVAILABLE.into())));
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

fn snapshot_textarea(source: &TextArea<'static>) -> TextArea<'static> {
    let mut target = new_textarea(&source.lines().join("\n"));
    let cursor = source.cursor();
    if let Some((start, end)) = source.selection_range() {
        let anchor = if cursor == start { end } else { start };
        target.move_cursor(CursorMove::Jump(
            anchor.0.min(u16::MAX as usize) as u16,
            anchor.1.min(u16::MAX as usize) as u16,
        ));
        target.start_selection();
    }
    target.move_cursor(CursorMove::Jump(
        cursor.0.min(u16::MAX as usize) as u16,
        cursor.1.min(u16::MAX as usize) as u16,
    ));
    target
}

fn normalized(value: &str) -> String {
    value.replace("\r\n", "\n").trim().to_string()
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

    fn lead_step_wizard(agents: &[orch_domain::Agent]) -> Wizard {
        let options = agents
            .iter()
            .map(|a| Choice::new(&a.id, &a.name, ""))
            .collect();
        let step = WizardStep::new("lead", "Team lead", StepKind::Select)
            .options(options)
            .agent_picker(AgentPicker::new(agents));
        Wizard::new(WizardKind::Team, "T", vec![step])
    }

    #[test]
    fn confirming_an_agent_step_blames_the_filter_only_when_one_is_typed() {
        let solo = [crate::backend::make_agent(
            "agt_1",
            "Solo",
            orch_domain::AgentStatus::Idle,
        )];
        let mut filtered = lead_step_wizard(&solo);
        type_text(&mut filtered, "zzz", 0);
        filtered.handle_key(key(KeyCode::Enter), 0);
        let mut empty = lead_step_wizard(&[]);
        empty.handle_key(key(KeyCode::Enter), 0);
        assert_eq!(
            (filtered.visible_error(), empty.visible_error()),
            (Some(NO_AGENT_MATCHES), Some(NO_AGENT_TO_CHOOSE))
        );
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
        // One condition for both keys, so the `i` arm takes exactly the
        // modifiers the `v` arm does. Meta is Alt/Option (or the Kitty `meta`
        // bit), never Cmd.
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
            for modifiers in [KeyModifiers::CONTROL, KeyModifiers::ALT, KeyModifiers::META] {
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
    fn cmd_is_not_meta_so_super_never_pastes() {
        // The Kitty `super` bit is never mapped to meta, so Cmd+V/Cmd+I
        // must not reach the paste handler.
        let steps = vec![WizardStep::new("t", "T", StepKind::Text)];
        for code in [KeyCode::Char('v'), KeyCode::Char('i')] {
            let mut wizard = Wizard::new(WizardKind::Task, "T", steps.clone()).with_paste(true);
            let outcome = wizard.handle_key(KeyEvent::new(code, KeyModifiers::SUPER), 0);
            assert_ne!(outcome, WizardOutcome::PasteImage, "{code:?} + SUPER");
            // It falls through to the editor as ordinary input.
            assert_eq!(wizard.text_value(), code_char(code).to_string());
        }
    }

    fn code_char(code: KeyCode) -> char {
        match code {
            KeyCode::Char(c) => c,
            other => panic!("not a character key: {other:?}"),
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
