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
//! `logs.follow` feed, the history and model-catalogue loads, a prompt-mode
//! write, and the observation of a `run` — happens on sessions of its own, so
//! no frame waits on it.
//!
//! # PR 2 scope
//!
//! Reads are RPCs on the dashboard's existing refresh tick (5 s, or after a
//! local action); live events come from `logs.follow`. Push subscriptions
//! replace the polling in PR 3. There is no observer mode: every dashboard is
//! a client, and the header's WATCHING / IDLE chip is the daemon's dispatch
//! switch as it was when the dashboard opened or last toggled it.

pub mod backend;
pub mod command;
pub mod connect;
pub mod no_daemon;

pub use backend::DaemonBackend;
pub use command::{first_run, run, run_dashboard, run_with_peer};
pub use connect::Connector;
