//! `orch tui` — the dashboard's daemon side.
//!
//! The TUI talks to the project through a narrow facade
//! (`orch_tui::backend::TuiBackend`). This module implements it as a client
//! of the project's daemon: [`backend::DaemonBackend`] turns each backend
//! callback into one typed request (design D7) over sessions a
//! [`connect::Connector`] opens. It never builds an engine, opens a store or
//! starts a process; when no daemon answers, [`command`] shows the
//! [`no_daemon`] screen (`r retry · q quit`) and starts nothing (goal row 3).
//!
//! It lives in `orch-cli` rather than in `orch-tui` because `orch-tui`
//! deliberately depends on the domain only, so its own tests run against a
//! fake.
//!
//! # The bridge, and why it is shaped this way
//!
//! `TuiBackend` is **synchronous** — every method returns a value, not a
//! future — while the client is `async`, and [`crate::run`] already drives the
//! whole command tree inside `Runtime::block_on`. A ratatui draw loop placed
//! directly in `dispatch` would therefore sit on a thread that is *driving*
//! the runtime, and `Handle::block_on` **panics** there.
//!
//! So the draw loop runs on a plain `std::thread` holding a cloned
//! [`tokio::runtime::Handle`], and every method that can fail is awaited (with
//! a deadline) and returns the real error. Long-running work — the
//! observation pump, the history and model-catalogue loads, a prompt-mode
//! write, and the observation of a `run` — happens on sessions of its own, so
//! no frame waits on it.
//!
//! # Push, not polling (PR 3)
//!
//! The dashboard subscribes to every store and the event bus before its
//! first read, on one dedicated observation session; the frame loop drains
//! what that session pushed from memory and reads only the views a pushed
//! change made stale. There is no periodic state read and no directory
//! observer. A gap or a daemon resync subscribes again and then re-reads
//! every view; a different daemon instance or an ended observation is a lost
//! session and brings back the no-daemon screen, where `r` opens a new
//! subscription and bootstrap with nothing carried over. There is no
//! observer mode: every dashboard is a client, and the header's WATCHING /
//! IDLE chip is the daemon's dispatch switch, read again on every pushed
//! state change.

pub mod backend;
pub mod command;
pub mod connect;
pub mod no_daemon;

#[cfg(test)]
mod push_tests;

pub use backend::DaemonBackend;
pub use command::{first_run, run, run_as_agent, run_dashboard, run_with_peer};
pub use connect::Connector;
