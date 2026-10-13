//! `orch tui` — the dashboard's engine side.
//!
//! The TUI talks to the engine through a narrow facade
//! (`orch_tui::backend::TuiBackend`). This module implements the engine half of
//! it: [`backend::EngineBackend`] wraps a [`orch_engine::FullEngine`] and turns
//! each of the TS `registerTuiCommand` callbacks
//! (`src/cli/commands/tui.ts`) into one method.
//!
//! It lives in `orch-cli` rather than in either neighbour because that is the
//! only placement that keeps the dependency graph honest: `orch-engine` must
//! not know about the TUI, and `orch-tui` deliberately does not depend on
//! `orch-engine` so its own tests can run against a fake. `orch-cli` already
//! depends on both.
//!
//! # The bridge, and why it is shaped this way
//!
//! `TuiBackend` is **synchronous** — every method returns a value, not a
//! future — while every engine service is `async`, and [`crate::run`] already
//! drives the whole command tree inside `Runtime::block_on`. A ratatui draw
//! loop placed directly in `dispatch` would therefore sit on a thread that is
//! *driving* the runtime, and `Handle::block_on` **panics** there.
//!
//! So the draw loop runs on a plain `std::thread` holding a cloned
//! [`tokio::runtime::Handle`]:
//!
//! - short calls (`refresh_*`, task/agent/team/goal CRUD, the settings writes)
//!   use `handle.block_on` and return the real result, because the TUI renders
//!   `Failed: {err}` and needs it synchronously;
//! - long calls (`run_task`, `run_all`, `start_watch`, `stop_watch`) are
//!   `handle.spawn`ed and report `Ok(())` at once, because a frame must never
//!   wait on an agent. Their outcome — including a lock conflict — arrives
//!   through [`backend::EngineBackend::poll_events`] as an
//!   `orchestrator:error`.
//!
//! `orch-engine` asserts the `Send + Sync + 'static` half of that contract
//! statically, and `orch-engine/tests/sync_bridge.rs` proves the mechanic.
//!
//! # What is not here yet
//!
//! The **observer fallback**. When `start_watch` fails because another process
//! holds the lock, the TS switches `onSubscribeEvents` to a `DiskObserver` that
//! polls `state.json` and the run JSONLs. WP-5a built that observer
//! (`serve::disk_observer`) and it has not reached `master` yet.
//! [`backend::EngineBackend`] already records the mode and routes every event
//! through one queue, so attaching it is one call — see
//! [`backend::EngineBackend::observer_sink`] and the TODO in [`command`].

pub mod backend;
pub mod command;

pub use backend::EngineBackend;
pub use command::run;
