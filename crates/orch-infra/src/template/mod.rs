//! Prompt templating.
//!
//! Rendering goes through the `liquid` crate (ARCHITECTURE §4.3, §4.8). Rendered
//! output depends on the context builder, not the engine: every
//! documented variable is always present (absent optionals are explicit `nil`)
//! and `shared_context` is an ordered `[k, v]` pair array. The golden fixtures
//! in `fixtures/prompts/` (V16) are the gate.
//!
//! Eco prompt mode adds a second pair of bundled templates and a pure
//! projection ([`build_eco_prompt_context`], [`eco`]); the normal templates,
//! [`build_prompt_context`] and their output are unchanged. [`render_prompt`]
//! is the production render path for both modes — dispatch calls it, and a
//! measurement harness can call it to get exactly what an agent receives.

mod context;
pub mod eco;
mod engine;
mod lenient;
mod render;

pub use context::{
    BuildPromptOptions, ContextFilterInput, build_prompt_context, extract_role_keywords,
    filter_relevant_context,
};
pub use eco::{
    GOAL_PREVIEW_MAX_BYTES, GOAL_PREVIEW_MAX_TASKS, GOAL_TASK_TITLE_MAX_BYTES,
    RETRY_PREVIEW_HEAD_BYTES, RETRY_PREVIEW_MAX_BYTES, project_eco,
};
pub use engine::LiquidTemplateEngine;
pub use render::{RenderedPrompt, build_eco_prompt_context, render_prompt};

/// Static system prompt template — cacheable between runs.
pub const DEFAULT_SYSTEM_TEMPLATE: &str = include_str!("system.liquid");

/// Dynamic user prompt template — changes every run.
pub const DEFAULT_USER_TEMPLATE: &str = include_str!("user.liquid");

/// Eco system template: stable CLI reference and rules, compact roster, the
/// agent's own full role and skills, then workspace and shared context.
pub const DEFAULT_ECO_SYSTEM_TEMPLATE: &str = include_str!("eco_system.liquid");

/// Eco user template: the task and its evidence, with bounded retry and
/// goal-task previews.
pub const DEFAULT_ECO_USER_TEMPLATE: &str = include_str!("eco_user.liquid");

/// The legacy combined template (`DEFAULT_SYSTEM_TEMPLATE + '\n' +
/// DEFAULT_USER_TEMPLATE`).
pub fn default_prompt_template() -> String {
    format!("{DEFAULT_SYSTEM_TEMPLATE}\n{DEFAULT_USER_TEMPLATE}")
}
