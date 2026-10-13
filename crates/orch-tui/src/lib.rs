//! WP-6 — the ratatui dashboard.
//!
//! Module layout follows ARCHITECTURE §3.3. Every module is a stub. Tests use
//! `ratatui::backend::TestBackend`: push a key, draw, assert — synchronously,
//! never with a sleep (ARCHITECTURE §8.4).

pub mod app;
pub mod command_bar;
pub mod keys;
pub mod state;
pub mod theme;
pub mod views;
pub mod widgets;
pub mod wizard;
