//! The form wizard: the engine plus the catalogue of eleven wizards.

pub mod configs;
pub mod engine;

pub use engine::{
    Choice, StepKind, SuggestionMode, Values, Wizard, WizardKind, WizardOutcome, WizardStep,
};
