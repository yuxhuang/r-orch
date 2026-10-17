//! RAM-only session seam. B2 implements capture, dirty checks and transitions.
use super::{Values, Wizard, WizardKind};
use crate::{
    app::App,
    backend::TuiBackend,
    clock::Clock,
    keys::{ExitCause, UiEvent},
    views::RenderCtx,
};
use ratatui::{Frame, layout::Rect};

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
/// Plain capture contract; B2 adds editor-library selection/undo state locally.
#[derive(Debug, Clone)]
pub struct WizardDraft {
    pub key: DraftKey,
    pub baseline: Values,
    pub values: Values,
    pub step_id: String,
    pub current_text: String,
    pub attachments: Vec<String>,
    pub cursor: (usize, usize),
}
#[derive(Default)]
pub struct WizardSessionStore {
    pub active_key: Option<DraftKey>,
    pub pending_leave: Option<PendingLeave>,
}
impl WizardSessionStore {
    /// I0 safe rejection: never promise a saved draft before B2 capture exists.
    pub fn save_draft(&mut self, _draft: WizardDraft) -> Result<(), String> {
        Err("Session drafts are not available yet".into())
    }
    pub fn draft_for(&self, _key: &DraftKey) -> Option<&WizardDraft> {
        None
    }
    pub fn discard_draft(&mut self, _key: &DraftKey) {}

    /// I0 inert seed; B2 installs semantic baseline/draft capture before use.
    pub fn request_leave(&mut self, _intent: LeaveIntent) -> bool {
        false
    }
}
/// I0 inert seed; B0 composes this even when no wizard is open.
pub fn render_session_overlay(_frame: &mut Frame, _area: Rect, _ctx: &RenderCtx) {}
/// I0 inert seed; legacy editor behavior remains until B2 replaces this.
pub(crate) fn handle_session_event<B: TuiBackend, C: Clock>(
    _app: &mut App<B, C>,
    _event: &UiEvent,
) -> bool {
    false
}
