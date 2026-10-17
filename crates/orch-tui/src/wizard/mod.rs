//! The form wizard: the engine plus the catalogue of eleven wizards.

pub mod configs;
pub mod engine;
pub mod session;
pub(crate) use session::handle_session_event;
pub use session::{
    DraftKey, LeaveChoice, LeaveIntent, PendingLeave, WizardDraft, WizardSessionStore,
    render_session_overlay,
};

pub use engine::{
    Choice, StepKind, SuggestionMode, Values, Wizard, WizardKind, WizardOutcome, WizardStep,
};
