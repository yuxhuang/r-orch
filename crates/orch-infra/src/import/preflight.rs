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
//!   config and indexed-family loading now share that guard. Full import completion
//!   and final integrated #146/#192 qualification remain separate obligations.
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
mod native_tests;
#[cfg(test)]
mod tests;

#[derive(Default)]
struct ConfigSource {
    hash: Option<String>,
    rows: Vec<orch_core::store::ConfigEntry>,
}

struct NativeSource {
    store: &'static str,
    name: std::ffi::OsString,
    id: String,
    hash: String,
    position: u64,
}

struct TemplateSource {
    name: std::ffi::OsString,
    hash: String,
}

/// A check and its confined source, retained until all admitted work drains.
/// Templates, config and the five indexed families are loadable one row at a time.
pub struct PreparedImport {
    pub report: Report,
    orch: crate::data_dir::confined::ConfinedDir,
    root: crate::data_dir::confined::ConfinedDir,
    project: PathBuf,
    legacy: Option<crate::data_dir::LegacyGuard>,
    templates: Vec<TemplateSource>,
    native: Vec<NativeSource>,
    config: ConfigSource,
    cancel: Arc<AtomicBool>,
}
impl PreparedImport {
    /// Full checked inventory, independent of the bounded report preview.
    pub fn inventory(&self) -> super::ImportInventory {
        use orch_core::store::EntityKind;
        let mut result: super::ImportInventory = self
            .template_inventory()
            .into_iter()
            .map(|(id, hash)| ((EntityKind::Template, id), (hash, None)))
            .collect();
        let mut family = "";
        let mut rank = 0;
        for input in &self.native {
            if input.store != family {
                family = input.store;
                rank = 0;
            }
            rank += 1;
            result.insert(
                (native_kind(input.store), input.id.clone()),
                (input.hash.clone(), Some(rank)),
            );
        }
        for row in &self.config.rows {
            result.insert(
                (EntityKind::Config, row.key.clone()),
                (self.config_hash(), None),
            );
        }
        result
    }

    fn config_hash(&self) -> String {
        use sha2::{Digest, Sha256};
        self.config
            .hash
            .clone()
            .unwrap_or_else(|| format!("{:x}", Sha256::digest(b"missing config.yml")))
    }

    pub fn row_count(&self) -> usize {
        self.templates.len() + self.native.len() + self.config.rows.len()
    }

    /// One descriptor-relative, bounded second read. Never load report paths.
    pub fn load_row(&self, index: usize) -> Result<super::ImportRow, PreflightError> {
        use super::document::{Limits, decode_json, decode_yaml, read_bounded};
        use crate::data_dir::confined::FileAccess;
        use orch_core::store::Entity;
        use sha2::{Digest, Sha256};
        use std::sync::atomic::Ordering;
        if index < self.templates.len() {
            return self.load_template(index);
        }
        if self.cancel.load(Ordering::Relaxed) {
            return Err(PreflightError::Cancelled);
        }
        self.verify_source()?;
        let native_index = index - self.templates.len();
        if native_index >= self.native.len() {
            let row = self
                .config
                .rows
                .get(native_index - self.native.len())
                .ok_or_else(|| PreflightError::Source("config index out of range".into()))?;
            let file = self
                .orch
                .open_file(std::ffi::OsStr::new("config.yml"), FileAccess::Untrusted);
            match (&self.config.hash, file) {
                (None, Err(crate::data_dir::confined::ConfinedError::Io(e)))
                    if e.kind() == std::io::ErrorKind::NotFound => {}
                (Some(expected), Ok(file)) => {
                    let bytes = read_bounded(
                        CancelableRead {
                            inner: file,
                            cancel: &self.cancel,
                        },
                        Limits::YAML.bytes,
                    )
                    .map_err(|_| PreflightError::Source("config load refused".into()))?;
                    if format!("{:x}", Sha256::digest(&bytes)) != *expected {
                        return Err(PreflightError::Source(
                            "config changed since preflight".into(),
                        ));
                    }
                }
                _ => {
                    return Err(PreflightError::Source(
                        "config changed since preflight".into(),
                    ));
                }
            }
            if self.cancel.load(Ordering::Relaxed) {
                return Err(PreflightError::Cancelled);
            }
            return Ok(super::ImportRow {
                entity: Entity::Config(row.clone()),
                source_sha256: self.config_hash(),
            });
        }
        let input = self
            .native
            .get(native_index)
            .ok_or_else(|| PreflightError::Source("row index out of range".into()))?;
        let dir = self
            .orch
            .open_dir(input.store)
            .map_err(|_| PreflightError::Source("entity directory changed".into()))?;
        let file = dir
            .open_file(&input.name, FileAccess::Untrusted)
            .map_err(|_| PreflightError::Source("entity file changed".into()))?;
        let yaml = matches!(input.store, "tasks" | "agents" | "goals");
        let limits = if yaml { Limits::YAML } else { Limits::JSON };
        let bytes = read_bounded(
            CancelableRead {
                inner: file,
                cancel: &self.cancel,
            },
            limits.bytes,
        )
        .map_err(|_| PreflightError::Source("entity load exceeded bound".into()))?;
        let hash = format!("{:x}", Sha256::digest(&bytes));
        if hash != input.hash {
            return Err(PreflightError::Source(
                "entity changed since preflight".into(),
            ));
        }
        // YAML uses its guarded decoder on a separate runtime, as preflight does.
        let runtime = tokio::runtime::Builder::new_current_thread().build()?;
        let entity = match input.store {
            "tasks" => runtime
                .block_on(decode_yaml(&bytes, limits))
                .map(|v| Entity::Task(Box::new(v))),
            "agents" => runtime
                .block_on(decode_yaml(&bytes, limits))
                .map(|v| Entity::Agent(Box::new(v))),
            "goals" => runtime
                .block_on(decode_yaml(&bytes, limits))
                .map(|v| Entity::Goal(Box::new(v))),
            "messages" => decode_json(&bytes, limits).map(|v| Entity::Message(Box::new(v))),
            "context" => decode_json(&bytes, limits).map(Entity::Context),
            _ => unreachable!("indexed source family"),
        }
        .map_err(|_| PreflightError::Source("entity no longer decodes".into()))?;
        if entity.id() != input.id {
            return Err(PreflightError::Source("entity identity changed".into()));
        }
        if self.cancel.load(Ordering::Relaxed) {
            return Err(PreflightError::Cancelled);
        }
        Ok(super::ImportRow {
            entity,
            source_sha256: hash,
        })
    }

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
        let (report, templates, native, config) = check::run_on(&orch, &options, probe.as_ref())?;
        Ok(PreparedImport {
            report,
            orch,
            root,
            project,
            legacy,
            templates,
            native,
            config,
            cancel: options.cancel,
        })
    })
    .await
    .map_err(|e| PreflightError::Io(std::io::Error::other(e)))?
}

fn native_kind(store: &str) -> orch_core::store::EntityKind {
    use orch_core::store::EntityKind;
    match store {
        "tasks" => EntityKind::Task,
        "agents" => EntityKind::Agent,
        "goals" => EntityKind::Goal,
        "messages" => EntityKind::Message,
        "context" => EntityKind::Context,
        _ => unreachable!("indexed source family"),
    }
}

/// A load retains its blocking waiter even when a read cannot yet return.
struct CancelableRead<'a, R> {
    inner: R,
    cancel: &'a AtomicBool,
}
impl<R: std::io::Read> std::io::Read for CancelableRead<'_, R> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        if self.cancel.load(std::sync::atomic::Ordering::Relaxed) {
            return Err(std::io::Error::other("cancelled"));
        }
        self.inner.read(buffer)
    }
}
