//! WP-2 — the eight adapters plus their shared plumbing.
//!
//! Each module is one adapter. The shared
//! pieces live in [`util`] (prompt assembly, token extraction, the
//! stream-json dispatch, the streaming generator) and [`buffer`] (the
//! backpressured queue the shell adapter merges its two streams through).
//!
//! `data` shapes are **not** uniform across adapters: there is a canonical
//! shape but claude and cursor emit the whole `parsed.message`,
//! codex emits `{ text, raw }`, opencode bare strings and shell bare line
//! strings. They are kept as they are rather than normalized, so consumers stay defensive.

pub mod antigravity;
pub mod buffer;
pub mod claude;
pub mod codex;
pub mod cursor;
mod effort;
pub mod grok;
pub mod opencode;
pub mod pi;
pub mod registry;
pub mod shell;
pub mod util;

pub use antigravity::AntigravityAdapter;
pub use buffer::EventBuffer;
pub use claude::ClaudeAdapter;
pub use codex::CodexAdapter;
pub use cursor::CursorAdapter;
pub use grok::GrokAdapter;
pub use opencode::OpenCodeAdapter;
pub use pi::PiAdapter;
pub use registry::{REGISTRATION_ORDER, build_registry, build_registry_with_clock};
pub use shell::ShellAdapter;
pub use util::{SystemClock, build_full_prompt, extract_tokens};
