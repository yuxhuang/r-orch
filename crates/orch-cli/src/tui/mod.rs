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
//! [`tokio::runtime::Handle`], and **every method that can fail before an
//! agent starts is awaited and returns the real error** — `refresh_*`, the
//! task/agent/team/goal mutations, the settings writes, and
//! `run_task`/`run_all`/`start_watch`/`stop_watch` alike.
//!
//! That last group looks like it wants to be fire-and-forget, and an earlier
//! version of this module spawned it and reported failures as
//! `orchestrator:error`. That is wrong, and docs/parity/tui.md §12.10 forbids
//! it: **nothing renders `orchestrator:error`** — there is no such arm
//! anywhere in `orch-tui` — so an error delivered only through the event
//! stream is invisible, and the dashboard would report a lock conflict as
//! success. Awaiting costs little, because none of the four waits on an agent
//! *run*: all three dispatch paths return once the child is spawned
//! (collection happens on a later tick), and `stop_watch` is bounded by the
//! kill grace. What happens *after* a run starts still arrives through
//! [`backend::EngineBackend::poll_events`].
//!
//! `orch-engine` asserts the `Send + Sync + 'static` half of that contract
//! statically, and `orch-engine/tests/sync_bridge.rs` proves the mechanic.
//!
//! # Observer mode
//!
//! When `start_watch` fails because another process holds the lock, the
//! dashboard becomes a read-only observer of *that* orchestrator: the error is
//! suppressed in favour of the header badge (`tui.ts:314`), and
//! [`command::attach_observer`] points `serve::disk_observer` at
//! [`backend::EngineBackend::observer_sink`], so disk-derived events reach the
//! same queue as in-process ones. Three differences are visible in that mode,
//! all faithful: the `● OBSERVING` chip, a 3 s poll instead of 5 s, and stale
//! teams — the periodic refresh passes `include_teams = false`
//! (`App.tsx:826`).

pub mod backend;
pub mod command;

pub use backend::EngineBackend;
pub use command::{first_run, run, run_with_peer};
