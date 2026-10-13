//! Prompt templating — ported from `src/infrastructure/template/template-engine.ts`.
//!
//! Rendering goes through the `liquid` crate (ARCHITECTURE §4.3, §4.8). Parity
//! with liquidjs depends on the context builder, not the engine: every
//! documented variable is always present (absent optionals are explicit `nil`)
//! and `shared_context` is an ordered `[k, v]` pair array. The golden fixtures
//! in `fixtures/prompts/` (V16) are the gate.

mod context;
mod engine;

pub use context::{
    BuildPromptOptions, ContextFilterInput, build_prompt_context, extract_role_keywords,
    filter_relevant_context,
};
pub use engine::LiquidTemplateEngine;

/// Static system prompt template — cacheable between runs.
pub const DEFAULT_SYSTEM_TEMPLATE: &str = include_str!("system.liquid");

/// Dynamic user prompt template — changes every run.
pub const DEFAULT_USER_TEMPLATE: &str = include_str!("user.liquid");

/// The legacy combined template (`DEFAULT_SYSTEM_TEMPLATE + '\n' +
/// DEFAULT_USER_TEMPLATE`).
pub fn default_prompt_template() -> String {
    format!("{DEFAULT_SYSTEM_TEMPLATE}\n{DEFAULT_USER_TEMPLATE}")
}
