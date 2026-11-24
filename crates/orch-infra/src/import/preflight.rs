//! The check that comes before every write of `orch import` (spec R112): one read-only
//! pass over a 1.0 source that does everything a dry run does and writes nothing.
//!
//! The source is opened once, without following a link, and read only through the
//! confined-directory module by names that a listing gave (R103). Every document is
//! measured for its bytes and its parse events before anything is built from it
//! (R109), event files are streamed (R111), ids and the task and workspace names are
//! checked together (R104, R117), and recorded in-flight runs are asked about with the
//! existence test that delivers nothing (R119). All of it is blocking work, so it runs
//! on a blocking thread, and a flag stops it between reads. Report paths are relative to
//! the source `.orchestry/`; the layer that prints them adds the prefix the spec's
//! "relative to the project" asks for.
//!
//! A caller holds the 1.0 owner lock for as long as it relies on an answer of this
//! check (R119 means something only while no 1.0 daemon can rewrite `state.json`), and
//! stops a check by setting `Options::cancel` and awaiting the result: dropping the
//! future does not stop the blocking thread.
//!
//! `config.yml` and the five `_index.json` files are classified on the bytes already
//! measured, by the contract of C-V1 to C-V14 and X-V1 to X-V14. The report carries the
//! effective `run_cap`, a bounded preview of the order, the size of the unknown keys, and
//! whether the owner must confirm preapproval. Each is a preview: input alone never grants
//! trust (R93, R133), and a durable caller repeats the checked reads under its own lock.
//!
//! Left to the slices that wire what they depend on, and listed here so that nothing
//! reads this module as a finished importer:
//! - nothing is written: no database, no blob, no `.orch-project`, no resume.
//! - nothing outside the tests calls this check: no `orch import` command, and the lock
//!   held across it and the cancellation of a blocked read wait for the first caller
//!   (#192).
mod check;
mod inflight;
mod names;
mod probe;
mod report;
mod source;

pub use probe::{ProcessProbe, SystemProbe};
pub use report::{Capped, Confirmation, Counts, Finding, REPORT_LIMIT, Report};

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

/// R111, "Import bounds": the listing of a source holds at most this many entries.
pub const ENTRY_LIMIT: u64 = 1_000_000;

#[derive(Debug, Clone)]
pub struct Options {
    /// Injectable so a test need not make a million entries (R111).
    pub entry_limit: u64,
    /// `--assume-stopped <run id>`: the owner's statement that a named run has ended.
    pub assume_stopped: BTreeSet<String>,
    /// Set from outside to stop the check between reads.
    pub cancel: Arc<AtomicBool>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            entry_limit: ENTRY_LIMIT,
            assume_stopped: BTreeSet::new(),
            cancel: Arc::new(AtomicBool::new(false)),
        }
    }
}

/// What ends the check without a report. A refusal of the source is a finding, not this.
#[derive(Debug, thiserror::Error)]
pub enum PreflightError {
    #[error("no .orchestry directory in the project directory")]
    NotInitialised,
    #[error("the source is not a plain directory: {0}")]
    Source(String),
    #[error("input or output error: {0}")]
    Io(#[from] std::io::Error),
    #[error("the check was cancelled")]
    Cancelled,
}

/// Runs the whole-source check of the 1.0 project directory `project` on a blocking
/// thread. It creates, changes and removes nothing, and opens no path built from a
/// value inside a document.
pub async fn preflight(
    project: PathBuf,
    options: Options,
    probe: Arc<dyn ProcessProbe>,
) -> Result<Report, PreflightError> {
    tokio::task::spawn_blocking(move || check::run(&project, &options, probe.as_ref()))
        .await
        .map_err(|e| PreflightError::Io(std::io::Error::other(e)))?
}

#[cfg(test)]
mod tests;
