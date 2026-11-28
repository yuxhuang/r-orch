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
//! - the offline daemon facade retains this check for its bounded template writer;
//!   full-family loading, existing-destination resume and integrated #146/#192
//!   qualification remain separate obligations.
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

struct TemplateSource {
    name: std::ffi::OsString,
    hash: String,
}

/// A check and its confined source, retained until all admitted work drains.
/// Only templates are loadable in this bounded first-writer slice.
pub struct PreparedImport {
    pub report: Report,
    orch: crate::data_dir::confined::ConfinedDir,
    root: crate::data_dir::confined::ConfinedDir,
    project: PathBuf,
    legacy: Option<crate::data_dir::LegacyGuard>,
    templates: Vec<TemplateSource>,
    cancel: Arc<AtomicBool>,
}
impl PreparedImport {
    pub fn procedural_exclusion(&self) -> bool {
        self.legacy.is_none()
    }

    /// A by-name replacement must not detach the check from its owner guard.
    pub fn verify_source(&self) -> Result<(), PreflightError> {
        use crate::data_dir::confined::ConfinedDir;
        use nix::sys::stat::fstat;
        let same = |a: &ConfinedDir, b: &ConfinedDir| -> Result<(), PreflightError> {
            let a = fstat(a).map_err(std::io::Error::from)?;
            let b = fstat(b).map_err(std::io::Error::from)?;
            if (a.st_dev, a.st_ino) != (b.st_dev, b.st_ino) {
                return Err(PreflightError::Source("source directory changed".into()));
            }
            Ok(())
        };
        let named = ConfinedDir::open(&self.project)
            .map_err(|_| PreflightError::Source("source directory changed".into()))?;
        same(&self.root, &named)?;
        let orch = named
            .open_dir(".orchestry")
            .map_err(|_| PreflightError::Source("source state directory changed".into()))?;
        same(&self.orch, &orch)?;
        if let Some(legacy) = &self.legacy {
            legacy
                .verify(&self.orch)
                .map_err(|_| PreflightError::Source("source owner lock changed".into()))?;
        }
        Ok(())
    }

    /// Bounded names and hashes already measured by whole-source preflight.
    pub fn template_inventory(&self) -> std::collections::BTreeMap<String, String> {
        self.templates
            .iter()
            .filter_map(|input| {
                input
                    .name
                    .to_str()
                    .and_then(|name| name.strip_suffix(".md"))
                    .map(|name| (name.to_owned(), input.hash.clone()))
            })
            .collect()
    }

    pub fn template_count(&self) -> usize {
        self.templates.len()
    }

    /// Blocking descriptor-relative second read, with the preflight hash.
    /// Never call this on the async runtime and never drop its blocking waiter.
    pub fn load_template(&self, index: usize) -> Result<super::ImportRow, PreflightError> {
        use crate::data_dir::confined::FileAccess;
        use sha2::{Digest, Sha256};
        use std::sync::atomic::Ordering;
        if self.cancel.load(Ordering::Relaxed) {
            return Err(PreflightError::Cancelled);
        }
        self.verify_source()?;
        let input = self
            .templates
            .get(index)
            .ok_or_else(|| PreflightError::Source("template index out of range".into()))?;
        let dir = self
            .orch
            .open_dir("templates")
            .map_err(|_| PreflightError::Source("template directory changed".into()))?;
        let file = dir
            .open_file(&input.name, FileAccess::Untrusted)
            .map_err(|_| {
                PreflightError::Source("template is no longer a plain singly linked file".into())
            })?;
        let bytes = super::document::read_bounded(file, 65_536)
            .map_err(|_| PreflightError::Source("template load exceeded its bound".into()))?;
        let hash = format!("{:x}", Sha256::digest(&bytes));
        if hash != input.hash {
            return Err(PreflightError::Source(
                "template changed since preflight".into(),
            ));
        }
        let name = input
            .name
            .to_str()
            .and_then(|s| s.strip_suffix(".md"))
            .ok_or_else(|| PreflightError::Source("template stem is not UTF-8".into()))?;
        let body = String::from_utf8(bytes)
            .map_err(|_| PreflightError::Source("template body is not UTF-8".into()))?;
        if self.cancel.load(Ordering::Relaxed) {
            return Err(PreflightError::Cancelled);
        }
        Ok(super::ImportRow {
            entity: orch_core::store::Entity::Template(orch_core::store::TemplateEntry {
                name: name.into(),
                body,
            }),
            source_sha256: hash,
        })
    }
}

/// Run the same whole-source preflight while retaining the source descriptor and
/// hashes of the bytes measured there. No destination is opened or created.
pub async fn prepare(
    project: PathBuf,
    options: Options,
    probe: Arc<dyn ProcessProbe>,
) -> Result<PreparedImport, PreflightError> {
    prepare_inner(project, options, probe, false).await
}

/// The genuine offline caller acquires the legacy guard from the same pinned
/// state directory that the check and all subsequent loads use.
pub async fn prepare_offline(
    project: PathBuf,
    options: Options,
    probe: Arc<dyn ProcessProbe>,
) -> Result<PreparedImport, PreflightError> {
    prepare_inner(project, options, probe, true).await
}

async fn prepare_inner(
    project: PathBuf,
    options: Options,
    probe: Arc<dyn ProcessProbe>,
    guarded: bool,
) -> Result<PreparedImport, PreflightError> {
    tokio::task::spawn_blocking(move || {
        use crate::data_dir::confined::ConfinedDir;
        let root = ConfinedDir::open(&project)
            .map_err(|_| PreflightError::Source("source is not a plain directory".into()))?;
        let orch = root
            .open_dir(".orchestry")
            .map_err(|_| PreflightError::NotInitialised)?;
        let legacy = if guarded {
            crate::data_dir::LegacyGuard::acquire_from(&orch)
                .map_err(|_| PreflightError::Source("cannot acquire source owner lock".into()))?
        } else {
            None
        };
        let (report, templates) = check::run_on(&orch, &options, probe.as_ref())?;
        Ok(PreparedImport {
            report,
            orch,
            root,
            project,
            legacy,
            templates,
            cancel: options.cancel,
        })
    })
    .await
    .map_err(|e| PreflightError::Io(std::io::Error::other(e)))?
}
