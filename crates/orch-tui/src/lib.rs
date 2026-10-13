//! WP-6 — the ratatui dashboard (`src/tui/**`, `src/cli/commands/tui.ts`).
//!
//! Layering:
//! * [`backend`] is the narrow facade the TUI needs from the engine. The TUI
//!   never sees `orch-engine`; the CLI wires a real implementation and the
//!   tests use [`backend::FakeBackend`]. See `BACKEND.md`.
//! * [`state`] owns every piece of app state and all the arithmetic; it is
//!   pure, synchronous and independent of ratatui.
//! * [`keys`] is the ordered dispatch chain (docs/parity/tui.md §2.1) and
//!   [`commands`] the command-bar verb surface.
//! * [`views`] and [`widgets`] draw [`state`] into a ratatui frame.
//!
//! Tests use `ratatui::backend::TestBackend`: push a key, draw, assert —
//! synchronously, never with a sleep (ARCHITECTURE §8.4).

pub mod app;
pub mod backend;
pub mod clock;
pub mod command_bar;
pub mod commands;
pub mod format;
pub mod keys;
pub mod layout;
pub mod messages;
pub mod onboarding;
pub mod runner;
pub mod state;
pub mod text;
pub mod theme;
pub mod views;
pub mod widgets;
pub mod wizard;

pub use app::App;
pub use backend::{FakeBackend, TuiBackend};
pub use clock::{Clock, SystemClock, TestClock};
pub use runner::run;
pub use state::{AppState, ViewId};
