//! The blocking body of the check. See the module header for what it promises.
use super::names::{escape, fold, is_id, relative};
use super::probe::ProcessProbe;
use super::report::{Counts, Finding, OrderedEntity, Report};
use super::source::{kind, list_bounded};
use super::{Options, PreflightError};
use crate::data_dir::confined::{ConfinedDir, ConfinedError, FileAccess};
use crate::import::document::{
    Limits, Refusal, check_json, decode_json, decode_yaml, read_bounded,
};
use crate::import::event::{EventItem, EventStream};
use serde::de::DeserializeOwned;
use serde_json::Value;
use sha2::{Digest, Sha256};
#[path = "compatibility.rs"]
mod compatibility;
use compatibility::{legacy_defaults, unknown_config};
#[path = "comparison.rs"]
mod comparison;
use comparison::DocumentContent;
use serde_json::value::RawValue;
use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io::{self, Read};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

/// "Import bounds": an attachment and a template.
const ATTACHMENT_BYTES: u64 = 67_108_864;
const TEMPLATE_BYTES: usize = 65_536;
/// A detail text keeps at most this many characters of what a parser said.
const DETAIL_CHARS: usize = 256;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Store {
    Tasks,
    Agents,
    Goals,
    Teams,
    Messages,
    Context,
    Runs,
    Templates,
}

impl Store {
    fn dir(self) -> &'static str {
        match self {
            Self::Tasks => "tasks",
            Self::Agents => "agents",
            Self::Goals => "goals",
            Self::Teams => "teams",
            Self::Messages => "messages",
            Self::Context => "context",
            Self::Runs => "runs",
            Self::Templates => "templates",
        }
    }
    /// The five directories whose files 1.0 keeps an `_index.json` for.
    fn has_index(self) -> bool {
        matches!(
            self,
            Self::Tasks | Self::Agents | Self::Goals | Self::Messages | Self::Context
        )
    }
}

/// What a name in the listing of the source's top level is, by the table of R99.
enum Top {
    Store(Store),
    State,
    Config,
    Attachments,
    Workspaces,
    /// Used in place by another program: not read, not entered, not judged.
    InPlace,
    Unknown,
}

fn top(name: &OsStr) -> Top {
    match name.as_bytes() {
        b"tasks" => Top::Store(Store::Tasks),
        b"agents" => Top::Store(Store::Agents),
        b"goals" => Top::Store(Store::Goals),
        b"teams" => Top::Store(Store::Teams),
        b"messages" => Top::Store(Store::Messages),
        b"context" => Top::Store(Store::Context),
        b"runs" => Top::Store(Store::Runs),
        b"templates" => Top::Store(Store::Templates),
        b"state.json" => Top::State,
        b"config.yml" => Top::Config,
        b"attachments" => Top::Attachments,
        b"workspaces" => Top::Workspaces,
        b"remotes"
        | b"remotes.yml"
        | b"peer-id"
        | b"run"
        | b"logs"
        | b"workspace-exclude"
        | b".gitignore"
        | b"orchestry.lock"
        | b".daemon-owner.lock"
        | b"daemon.json" => Top::InPlace,
        _ => Top::Unknown,
    }
}

enum Name<'a> {
    Index,
    Document(&'a [u8]),
    Events(&'a [u8]),
    Template,
    Other,
}

fn classify(store: Store, name: &OsStr) -> Name<'_> {
    let bytes = name.as_bytes();
    if store.has_index() && bytes == b"_index.json" {
        return Name::Index;
    }
    let ending = |suffix: &[u8]| bytes.strip_suffix(suffix);
    match store {
        Store::Tasks | Store::Agents | Store::Goals | Store::Teams => {
            ending(b".yml").map_or(Name::Other, Name::Document)
        }
        Store::Messages | Store::Context => ending(b".json").map_or(Name::Other, Name::Document),
        Store::Runs => match (ending(b".jsonl"), ending(b".json")) {
            (Some(stem), _) if stem.starts_with(b"run_") => Name::Events(stem),
            (_, Some(stem)) if stem.starts_with(b"run_") => Name::Document(stem),
            _ => Name::Other,
        },
        Store::Templates => {
            if ending(b".md").is_some() {
                Name::Template
            } else {
                Name::Other
            }
        }
    }
}

fn order_key(store: Store, name: &OsStr) -> &[u8] {
    match classify(store, name) {
        Name::Document(stem) => stem,
        _ => name.as_bytes(),
    }
}

struct Level {
    dir: ConfinedDir,
    names: Vec<OsString>,
}

struct AttachDir {
    name: OsString,
    /// False when the entry was refused as not a directory: it is not entered.
    ok: bool,
    files: Vec<OsString>,
}

struct Plan {
    has_state: bool,
    has_config: bool,
    stores: Vec<(Store, Level)>,
    attachments: Option<(ConfinedDir, Vec<AttachDir>)>,
    workspaces: Vec<OsString>,
}

enum Listed {
    Names(Vec<OsString>),
    /// An error was recorded as a finding and the directory is not read.
    Skipped,
    /// The entry limit was passed and recorded; nothing more is listed or read.
    TooMany,
}

/// A reader that stops at the next call once the flag is set, so a long file or
/// attachment cannot hold a cancelled check.
struct Cancelable<'a, R> {
    inner: R,
    flag: &'a AtomicBool,
}

impl<R: Read> Read for Cancelable<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.flag.load(Ordering::Relaxed) {
            return Err(io::Error::other("cancelled"));
        }
        self.inner.read(buf)
    }
}

/// Streams an event file to its end, holding one line at a time, and counts the events
/// and the gaps (R111).
fn count_events(reader: impl Read) -> Result<(u64, u64), Refusal> {
    let (mut events, mut gaps) = (0u64, 0u64);
    for item in EventStream::<_, orch_domain::run::RunEvent>::new(reader, Limits::EVENT) {
        match item? {
            EventItem::Event { .. } => events += 1,
            EventItem::Gap { .. } => gaps += 1,
        }
    }
    Ok((events, gaps))
}

fn source_error(error: ConfinedError) -> PreflightError {
    match error {
        ConfinedError::Io(e) => PreflightError::Io(e),
        other => PreflightError::Source(phrase(&other).to_owned()),
    }
}

/// Fixed wording: the error's own text would carry the unescaped name.
fn phrase(error: &ConfinedError) -> &'static str {
    match error {
        ConfinedError::Symlink(_) => "a symbolic link",
        ConfinedError::WrongType(_) => "not the kind of entry that was expected",
        ConfinedError::HardLink(_) => "a file with more than one hard link",
        ConfinedError::InvalidName(_) => "not one path component",
        ConfinedError::Io(_) => "an input or output error",
    }
}

fn bounded(text: &str) -> String {
    let cut: String = text.chars().take(DETAIL_CHARS).collect();
    escape(OsStr::new(&cut))
}

pub(super) fn run(
    project: &Path,
    options: &Options,
    probe: &dyn ProcessProbe,
) -> Result<Report, PreflightError> {
    let root = ConfinedDir::open(project).map_err(source_error)?;
    let orch = match root.open_dir(".orchestry") {
        Ok(dir) => dir,
        Err(ConfinedError::Io(e)) if e.kind() == io::ErrorKind::NotFound => {
            return Err(PreflightError::NotInitialised);
        }
        Err(e) => return Err(source_error(e)),
    };
    Ok(run_on(&orch, options, probe)?.0)
}

pub(super) fn run_on(
    orch: &ConfinedDir,
    options: &Options,
    probe: &dyn ProcessProbe,
) -> Result<
    (
        Report,
        Vec<super::TemplateSource>,
        Vec<super::NativeSource>,
        super::ConfigSource,
    ),
    PreflightError,
> {
    // The guarded legacy YAML reader parks its parse on a blocking thread and awaits it.
    // This thread is itself a worker of the caller's blocking pool, so a pool of one would
    // wait for itself. The decode therefore runs on a runtime of its own, whose blocking
    // pool the caller cannot exhaust.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .map_err(PreflightError::Io)?;
    let mut check = Check {
        options,
        probe,
        runtime: &runtime,
        report: Report::default(),
        listed: 0,
        tasks: BTreeMap::new(),
        auto_agent: false,
        inherited_agent_policy: false,
        templates: Vec::new(),
        native: Vec::new(),
        config: super::ConfigSource::default(),
    };
    if let Some(plan) = check.plan(orch)? {
        check.examine(orch, plan)?;
    }
    check.report.counts.entries = check.listed;
    check
        .native
        .sort_by_key(|input| (input.store, input.position));
    Ok((check.report, check.templates, check.native, check.config))
}

pub(super) struct Check<'a> {
    pub(super) options: &'a Options,
    pub(super) probe: &'a dyn ProcessProbe,
    runtime: &'a tokio::runtime::Runtime,
    pub(super) report: Report,
    listed: u64,
    templates: Vec<super::TemplateSource>,
    native: Vec<super::NativeSource>,
    config: super::ConfigSource,
    /// One fold key for each task id of the listing (R117): fold key to the id.
    tasks: BTreeMap<Vec<u8>, Vec<u8>>,
    auto_agent: bool,
    inherited_agent_policy: bool,
}

impl Check<'_> {
    pub(super) fn cancelled(&self) -> Result<(), PreflightError> {
        if self.options.cancel.load(Ordering::Relaxed) {
            Err(PreflightError::Cancelled)
        } else {
            Ok(())
        }
    }

    pub(super) fn counts(&mut self) -> &mut Counts {
        &mut self.report.counts
    }

    pub(super) fn refuse(
        &mut self,
        path: &str,
        rule: &'static str,
        limit: Option<u64>,
        observed: Option<String>,
        detail: &str,
    ) {
        self.report.refusals.push(Finding {
            path: path.to_owned(),
            rule,
            limit,
            observed,
            detail: bounded(detail),
        });
    }

    /// A finding whose text the caller has already escaped and bounded part by part.
    pub(super) fn refuse_safe(&mut self, path: &str, rule: &'static str, detail: String) {
        self.report.refusals.push(Finding {
            path: path.to_owned(),
            rule,
            limit: None,
            observed: None,
            detail,
        });
    }

    pub(super) fn note(
        &mut self,
        path: &str,
        rule: &'static str,
        observed: Option<String>,
        detail: &str,
    ) {
        self.report.notes.push(Finding {
            path: path.to_owned(),
            rule,
            limit: None,
            observed,
            detail: bounded(detail),
        });
    }

    pub(super) fn io(&mut self, path: &str, error: &io::Error) {
        self.refuse(path, "io", None, None, &error.to_string());
    }

    pub(super) fn refusal(&mut self, path: &str, refusal: Refusal) {
        let (rule, limit, observed) = match refusal.rule {
            "parse" | "type" => ("unreadable", None, None),
            "io" => ("io", None, None),
            rule => (
                rule,
                Some(refusal.limit),
                Some(refusal.observed.to_string()),
            ),
        };
        self.refuse(path, rule, limit, observed, &refusal.detail);
    }

    // ---- Listing: no document is read until every directory has been listed.

    fn list(&mut self, dir: &ConfinedDir, path: &str) -> Listed {
        let budget = self.options.entry_limit.saturating_sub(self.listed);
        match list_bounded(dir, budget) {
            Ok(listing) if listing.over => {
                let limit = self.options.entry_limit;
                self.refuse(
                    path,
                    "entries",
                    Some(limit),
                    Some(limit.saturating_add(1).to_string()),
                    "the listing of the source holds more entries than the limit; no document was read",
                );
                Listed::TooMany
            }
            Ok(listing) => {
                self.listed += listing.names.len() as u64;
                Listed::Names(listing.names)
            }
            Err(e) => {
                self.io(path, &e);
                Listed::Skipped
            }
        }
    }

    /// A listed directory entry that the table expects to be a directory.
    fn open_dir(&mut self, parent: &ConfinedDir, name: &OsStr, path: &str) -> Option<ConfinedDir> {
        match kind(parent, name) {
            Ok(k) => {
                if let Some(why) = k.why_not_directory() {
                    self.refuse(path, "file_kind", None, None, why);
                    return None;
                }
            }
            Err(e) => {
                self.io(path, &e);
                return None;
            }
        }
        match parent.open_dir(name) {
            Ok(dir) => Some(dir),
            Err(ConfinedError::Io(e)) => {
                self.io(path, &e);
                None
            }
            Err(other) => {
                self.refuse(path, "file_kind", None, None, phrase(&other));
                None
            }
        }
    }

    fn plan(&mut self, orch: &ConfinedDir) -> Result<Option<Plan>, PreflightError> {
        let names = match self.list(orch, ".") {
            Listed::Names(names) => names,
            Listed::TooMany => return Ok(None),
            Listed::Skipped => {
                // The cause was recorded as a finding; the check cannot go on without it.
                let cause = self
                    .report
                    .refusals
                    .listed
                    .last()
                    .map_or_else(String::new, |f| f.detail.clone());
                return Err(PreflightError::Io(io::Error::other(format!(
                    "the source directory could not be listed: {cause}"
                ))));
            }
        };
        let mut plan = Plan {
            has_state: false,
            has_config: false,
            stores: Vec::new(),
            attachments: None,
            workspaces: Vec::new(),
        };
        for name in &names {
            self.cancelled()?;
            let path = relative(&[name]);
            match top(name) {
                Top::State => plan.has_state = true,
                Top::Config => plan.has_config = true,
                Top::InPlace => self.counts().left_in_place += 1,
                Top::Unknown => self.note(
                    &path,
                    "not_imported",
                    None,
                    "a path with no rule: not imported, and not entered if it is a directory",
                ),
                Top::Store(store) => {
                    let Some(dir) = self.open_dir(orch, name, &path) else {
                        continue;
                    };
                    match self.list(&dir, &path) {
                        Listed::Names(names) => plan.stores.push((store, Level { dir, names })),
                        Listed::Skipped => {}
                        Listed::TooMany => return Ok(None),
                    }
                }
                Top::Workspaces => {
                    // R117: its names alone. No entry of it is opened, entered or judged.
                    let Some(dir) = self.open_dir(orch, name, &path) else {
                        continue;
                    };
                    match self.list(&dir, &path) {
                        Listed::Names(names) => plan.workspaces = names,
                        Listed::Skipped => {}
                        Listed::TooMany => return Ok(None),
                    }
                }
                Top::Attachments => {
                    let Some(dir) = self.open_dir(orch, name, &path) else {
                        continue;
                    };
                    let tasks = match self.list(&dir, &path) {
                        Listed::Names(tasks) => tasks,
                        Listed::Skipped => continue,
                        Listed::TooMany => return Ok(None),
                    };
                    let mut dirs = Vec::with_capacity(tasks.len());
                    for task in tasks {
                        self.cancelled()?;
                        let inner = relative(&[name, &task]);
                        let Some(sub) = self.open_dir(&dir, &task, &inner) else {
                            dirs.push(AttachDir {
                                name: task,
                                ok: false,
                                files: Vec::new(),
                            });
                            continue;
                        };
                        match self.list(&sub, &inner) {
                            Listed::Names(files) => dirs.push(AttachDir {
                                name: task,
                                ok: true,
                                files,
                            }),
                            Listed::Skipped => dirs.push(AttachDir {
                                name: task,
                                ok: false,
                                files: Vec::new(),
                            }),
                            Listed::TooMany => return Ok(None),
                        }
                    }
                    plan.attachments = Some((dir, dirs));
                }
            }
        }
        Ok(Some(plan))
    }

    // ---- Reading: each document is measured on the bytes that are then decoded.

    pub(super) fn acquire(&mut self, dir: &ConfinedDir, name: &OsStr, path: &str) -> Option<File> {
        match kind(dir, name) {
            Ok(k) => {
                if let Some(why) = k.why_not_file() {
                    self.refuse(path, "file_kind", None, None, why);
                    return None;
                }
            }
            Err(e) => {
                self.io(path, &e);
                return None;
            }
        }
        match dir.open_file(name, FileAccess::Untrusted) {
            Ok(file) => Some(file),
            Err(ConfinedError::Io(e)) => {
                self.io(path, &e);
                None
            }
            // Replaced between the listing's stat and the open: still never read.
            Err(other) => {
                self.refuse(path, "file_kind", None, None, phrase(&other));
                None
            }
        }
    }

    pub(super) fn read(
        &mut self,
        file: impl Read,
        limit: usize,
        path: &str,
    ) -> Result<Option<Vec<u8>>, PreflightError> {
        let reader = Cancelable {
            inner: file,
            flag: &self.options.cancel,
        };
        match read_bounded(reader, limit) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(refusal) => {
                self.cancelled()?;
                self.refusal(path, refusal);
                Ok(None)
            }
        }
    }

    fn yaml<T: DeserializeOwned>(&mut self, bytes: &[u8], path: &str) -> Option<T> {
        match self.runtime.block_on(decode_yaml::<T>(bytes, Limits::YAML)) {
            Ok(value) => Some(value),
            Err(refusal) => {
                self.refusal(path, refusal);
                None
            }
        }
    }

    pub(super) fn json<T: DeserializeOwned>(&mut self, bytes: &[u8], path: &str) -> Option<T> {
        match decode_json::<T>(bytes, Limits::JSON) {
            Ok(value) => Some(value),
            Err(refusal) => {
                self.refusal(path, refusal);
                None
            }
        }
    }

    /// R104: the document's own id is the stem of its file.
    fn own_id(&mut self, path: &str, own: &str, stem: &[u8]) -> bool {
        if own.as_bytes() == stem {
            return true;
        }
        self.refuse(
            path,
            "id",
            None,
            Some(escape(OsStr::new(own))),
            "the document's own id is not the stem of its file",
        );
        false
    }

    fn examine(&mut self, orch: &ConfinedDir, plan: Plan) -> Result<(), PreflightError> {
        for (store, level) in &plan.stores {
            self.store(*store, level)?;
        }
        self.cancelled()?;
        self.config(orch, plan.has_config)?;
        self.state(orch, plan.has_state)?;
        if let Some((dir, attachments)) = &plan.attachments {
            self.attachments(dir, attachments)?;
        }
        self.workspaces(&plan.workspaces);
        Ok(())
    }

    fn store(&mut self, store: Store, level: &Level) -> Result<(), PreflightError> {
        let mut index = if store.has_index() {
            Some(self.index_plan(store, level)?)
        } else {
            None
        };
        let mut names: Vec<_> = level.names.iter().collect();
        if store.has_index() {
            // Removing the extension changes prefix order: a-b.json precedes
            // a.json as a file name, but the entity id a precedes a-b.
            names.sort_by(|a, b| order_key(store, a).cmp(order_key(store, b)));
        }
        for name in names {
            self.cancelled()?;
            let path = relative(&[OsStr::new(store.dir()), name]);
            match classify(store, name) {
                Name::Other => self.note(
                    &path,
                    "not_imported",
                    None,
                    "a path with no rule: not imported",
                ),
                Name::Index => {}
                Name::Template => {
                    let Some(file) = self.acquire(&level.dir, name, &path) else {
                        continue;
                    };
                    if let Some(bytes) = self.read(file, TEMPLATE_BYTES, &path)? {
                        if name.to_str().is_none() || std::str::from_utf8(&bytes).is_err() {
                            self.refuse(
                                &path,
                                "utf8",
                                None,
                                None,
                                "template stem and body must be UTF-8",
                            );
                            continue;
                        }
                        self.templates.push(super::TemplateSource {
                            name: name.to_owned(),
                            hash: format!("{:x}", Sha256::digest(&bytes)),
                        });
                        self.counts().templates += 1;
                    }
                }
                Name::Events(stem) => {
                    if is_id(stem) {
                        self.events(&level.dir, name, &path)?;
                    } else {
                        self.bad_stem(&path);
                    }
                }
                Name::Document(stem) => {
                    if !is_id(stem) {
                        self.bad_stem(&path);
                        if let Some(index) = &mut index {
                            self.index_invalid_file(index, stem);
                        }
                        continue;
                    }
                    if store == Store::Tasks {
                        // R104: `workspaces/<task id>` is the one place an id still names a
                        // directory, and these two would name the directory itself or its parent.
                        if matches!(stem, b"." | b"..") {
                            self.refuse(
                                &path,
                                "id",
                                None,
                                None,
                                "a task id may not be `.` or `..`",
                            );
                            if let Some(index) = &mut index {
                                self.index_invalid_file(index, stem);
                            }
                            continue;
                        }
                        self.register_task(stem, &path);
                    }
                    let value = self.document(store, &level.dir, name, stem, &path)?;
                    if let Some(index) = &mut index {
                        match value {
                            Some(value) => {
                                if let Some(position) =
                                    self.index_file(store.dir(), index, stem, &value)
                                {
                                    self.native.push(super::NativeSource {
                                        store: store.dir(),
                                        name: name.to_owned(),
                                        id: String::from_utf8(stem.to_vec())
                                            .expect("validated stem"),
                                        hash: value.1,
                                        position,
                                    });
                                }
                            }
                            None => self.index_invalid_file(index, stem),
                        }
                    }
                }
            }
        }
        if let Some(index) = index {
            self.finish_index(store.dir(), index);
        }
        Ok(())
    }

    fn bad_stem(&mut self, path: &str) {
        self.refuse(
            path,
            "id",
            None,
            None,
            "the stem of the file is outside the alphabet A-Z a-z 0-9 . _ -",
        );
    }

    /// R117: no two task ids with one fold key. Neither id is changed.
    fn register_task(&mut self, stem: &[u8], path: &str) {
        let key = fold(stem);
        match self.tasks.get(&key) {
            Some(first) if first != stem => {
                let first = escape(OsStr::from_bytes(first));
                self.refuse(
                    path,
                    "id_alias",
                    None,
                    Some(first),
                    "two task ids differ only in the case of ASCII letters",
                );
            }
            Some(_) => {}
            None => {
                self.tasks.insert(key, stem.to_vec());
            }
        }
    }

    fn document(
        &mut self,
        store: Store,
        dir: &ConfinedDir,
        name: &OsStr,
        stem: &[u8],
        path: &str,
    ) -> Result<Option<DocumentContent>, PreflightError> {
        let Some(file) = self.acquire(dir, name, path) else {
            return Ok(None);
        };
        let yaml = matches!(
            store,
            Store::Tasks | Store::Agents | Store::Goals | Store::Teams
        );
        let limit = if yaml {
            Limits::YAML.bytes
        } else {
            Limits::JSON.bytes
        };
        let Some(bytes) = self.read(file, limit, path)? else {
            return Ok(None);
        };
        // Keep the native typed decoders: building a JSON Value first would
        // silently accept repeated known members of a JSON entity.
        let mut agent_policy = None;
        let (own, value) = if yaml {
            let Some(value) = self.yaml::<serde_yaml_ng::Value>(&bytes, path) else {
                return Ok(None);
            };
            // A YAML-only key cannot equal a JSON index entry. It is still legal
            // in an ignored entity field, as it was to 1.0's typed YAML reader.
            let comparison = serde_json::to_value(&value)
                .ok()
                .and_then(|v| serde_json::value::to_raw_value(&v).ok());
            let own = match store {
                Store::Tasks => serde_yaml_ng::from_value::<orch_domain::Task>(value).map(|d| d.id),
                Store::Agents => serde_yaml_ng::from_value::<orch_domain::Agent>(value).map(|d| {
                    agent_policy = Some(d.config.approval_policy);
                    d.id
                }),
                Store::Goals => serde_yaml_ng::from_value::<orch_domain::Goal>(value).map(|d| d.id),
                Store::Teams => serde_yaml_ng::from_value::<orch_domain::Team>(value).map(|d| d.id),
                _ => unreachable!("YAML store"),
            }
            .map_err(|e| e.to_string());
            (own, comparison)
        } else {
            if let Err(refusal) = check_json(&bytes, Limits::JSON) {
                self.refusal(path, refusal);
                return Ok(None);
            }
            let own = match store {
                Store::Messages => {
                    serde_json::from_slice::<orch_domain::Message>(&bytes).map(|d| d.id)
                }
                Store::Context => {
                    serde_json::from_slice::<orch_core::storage::ContextEntry>(&bytes)
                        .map(|d| d.key)
                }
                Store::Runs => serde_json::from_slice::<orch_domain::Run>(&bytes).map(|d| d.id),
                _ => unreachable!("JSON store"),
            }
            .map_err(|e| e.to_string());
            let comparison = serde_json::from_slice::<Box<RawValue>>(&bytes)
                .expect("JSON budget pass checked the syntax");
            (own, Some(comparison))
        };
        let own = match own {
            Ok(own) => own,
            Err(e) => {
                self.refusal(path, Refusal::invalid("type", e));
                return Ok(None);
            }
        };
        if !self.own_id(path, &own, stem) {
            return Ok(None);
        }
        match agent_policy {
            Some(Some(orch_domain::agent::ApprovalPolicy::Auto)) => self.auto_agent = true,
            Some(None) => self.inherited_agent_policy = true,
            _ => {}
        }
        let counts = self.counts();
        match store {
            Store::Tasks => counts.tasks += 1,
            Store::Agents => counts.agents += 1,
            Store::Goals => counts.goals += 1,
            Store::Teams => counts.teams += 1,
            Store::Messages => counts.messages += 1,
            Store::Context => counts.context += 1,
            Store::Runs => counts.runs += 1,
            Store::Templates => {}
        }
        Ok(Some(DocumentContent(
            value,
            format!("{:x}", Sha256::digest(&bytes)),
        )))
    }

    fn config(&mut self, orch: &ConfinedDir, present: bool) -> Result<(), PreflightError> {
        let path = "config.yml";
        let value = if present {
            let Some(file) = self.acquire(orch, OsStr::new(path), path) else {
                return Ok(());
            };
            let Some(bytes) = self.read(file, Limits::YAML.bytes, path)? else {
                return Ok(());
            };
            self.config.hash = Some(format!("{:x}", Sha256::digest(&bytes)));
            let value = match self
                .runtime
                .block_on(decode_yaml::<serde_yaml_ng::Value>(&bytes, Limits::YAML))
            {
                Ok(value) => value,
                Err(mut refusal) => {
                    if refusal.rule == "documents" && refusal.observed == 0 {
                        refusal.detail.push_str("; 1.0 read this file as its defaults; after taking a copy, write {} to keep those defaults");
                    }
                    self.refusal(path, refusal);
                    return Ok(());
                }
            };
            let value = match serde_json::to_value(&value) {
                Ok(value) => value,
                Err(e) => {
                    self.refusal(path, Refusal::invalid("type", e));
                    return Ok(());
                }
            };
            if !value.is_object() {
                self.refusal(path, Refusal::invalid("type", "expected a mapping; 1.0 read this file as its defaults; after taking a copy, write {} to keep those defaults"));
                return Ok(());
            }
            value
        } else {
            serde_json::json!({})
        };
        let defaults = legacy_defaults();
        let unknown = unknown_config(&value, &defaults);
        let size = serde_json::to_vec(&unknown).expect("JSON value").len() as u64;
        self.report.legacy_config_bytes = size;
        if size > 65_536 {
            self.refuse(
                path,
                "legacy_config_bytes",
                Some(65_536),
                Some(size.to_string()),
                "unknown configuration keys exceed the legacy.config byte limit",
            );
            return Ok(());
        }
        self.config_unknown_notes(path, &unknown, "");
        let merged = orch_domain::deep_merge(&defaults, &value);
        let config = match serde_json::from_value::<orch_domain::OrchestratorConfig>(merged.clone())
        {
            Ok(config) => config,
            Err(e) => {
                self.refusal(path, Refusal::invalid("type", e));
                return Ok(());
            }
        };
        self.config.rows = compatibility::native_config(&merged, &defaults, &unknown);
        self.report.run_cap = Some(config.scheduling.max_concurrent_agents as u64);
        if value.pointer("/scheduling/max_concurrent_agents").is_none() {
            self.note(
                path,
                "config_default",
                Some("6".into()),
                "run_cap 6 is 1.0's default, not a value set by the source file",
            );
        }
        self.report.preapproval_requires_confirmation = self.auto_agent
            || (self.inherited_agent_policy
                && config.defaults.agent.approval_policy
                    == orch_domain::agent::ApprovalPolicy::Auto);
        if self.report.preapproval_requires_confirmation {
            self.note(path, "preapproval_confirmation", None, "the owner must confirm preapproval: auto agents are implicitly trusted to run unattended; source input alone never grants trust");
        }
        Ok(())
    }

    fn events(
        &mut self,
        dir: &ConfinedDir,
        name: &OsStr,
        path: &str,
    ) -> Result<(), PreflightError> {
        let Some(file) = self.acquire(dir, name, path) else {
            return Ok(());
        };
        let reader = Cancelable {
            inner: file,
            flag: &self.options.cancel,
        };
        let (events, gaps) = match count_events(reader) {
            Ok(counted) => counted,
            Err(refusal) => {
                self.cancelled()?;
                self.refusal(path, refusal);
                return Ok(());
            }
        };
        let counts = self.counts();
        counts.run_event_files += 1;
        counts.run_events += events;
        counts.run_event_gaps += gaps;
        if gaps > 0 {
            self.note(
                path,
                "run_event_gaps",
                Some(gaps.to_string()),
                "lines that were not stored; lines like them that follow count once",
            );
        }
        Ok(())
    }

    fn attachments(
        &mut self,
        root: &ConfinedDir,
        dirs: &[AttachDir],
    ) -> Result<(), PreflightError> {
        for entry in dirs.iter().filter(|d| d.ok) {
            self.cancelled()?;
            let area = OsStr::new("attachments");
            let path = relative(&[area, &entry.name]);
            let Some(dir) = self.open_dir(root, &entry.name, &path) else {
                continue;
            };
            self.counts().attachment_dirs += 1;
            let key = fold(entry.name.as_bytes());
            if !self
                .tasks
                .get(&key)
                .is_some_and(|id| id.as_slice() == entry.name.as_bytes())
            {
                self.note(
                    &path,
                    "dangling_reference",
                    None,
                    "no task has this exact id",
                );
            }
            for name in &entry.files {
                self.cancelled()?;
                let path = relative(&[area, &entry.name, name]);
                let Some(file) = self.acquire(&dir, name, &path) else {
                    continue;
                };
                self.measure(file, &path)?;
            }
        }
        Ok(())
    }

    /// An attachment is measured by reading it to the bound and one byte more, and is
    /// not parsed or kept: what the import reads, not what `fstat` says.
    fn measure(&mut self, file: File, path: &str) -> Result<(), PreflightError> {
        let reader = Cancelable {
            inner: file,
            flag: &self.options.cancel,
        };
        match io::copy(&mut reader.take(ATTACHMENT_BYTES + 1), &mut io::sink()) {
            Ok(n) if n > ATTACHMENT_BYTES => self.refusal(
                path,
                Refusal {
                    rule: "bytes",
                    limit: ATTACHMENT_BYTES,
                    observed: n,
                    detail: format!("bytes: limit {ATTACHMENT_BYTES}, observed at least {n}"),
                },
            ),
            Ok(n) => {
                let counts = self.counts();
                counts.attachments += 1;
                counts.attachment_bytes += n;
            }
            Err(e) => {
                self.cancelled()?;
                self.io(path, &e);
            }
        }
        Ok(())
    }

    /// R117: the names of `workspaces/` against the task ids of the source.
    fn workspaces(&mut self, names: &[OsString]) {
        for name in names {
            self.counts().workspaces_listed += 1;
            let path = relative(&[OsStr::new("workspaces"), name]);
            match self.tasks.get(&fold(name.as_bytes())) {
                Some(id) if id.as_slice() == name.as_bytes() => {}
                Some(id) => {
                    let id = escape(OsStr::from_bytes(id));
                    self.refuse(
                        &path,
                        "workspace_alias",
                        None,
                        Some(id),
                        "this name differs only in ASCII case from a task's id and is not that id",
                    );
                }
                None => self.note(
                    &path,
                    "workspace_unclaimed",
                    None,
                    "no task names this workspace; it is left in place",
                ),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fails(&'static [u8]);
    impl Read for Fails {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            if self.0.is_empty() {
                return Err(io::Error::from(io::ErrorKind::PermissionDenied));
            }
            let n = self.0.len().min(buf.len());
            buf[..n].copy_from_slice(&self.0[..n]);
            self.0 = &self.0[n..];
            Ok(n)
        }
    }

    #[test]
    fn a_read_error_in_the_middle_of_an_event_file_is_an_error_and_not_a_gap() {
        let event = br#"{"timestamp":"t","type":"done","data":"x"}"#;
        let mut text = event.to_vec();
        text.extend_from_slice(b"\n");
        let leaked: &'static [u8] = Box::leak(text.into_boxed_slice());
        let refusal = count_events(Fails(leaked)).unwrap_err();
        assert_eq!(refusal.rule, "io");
        assert_eq!(count_events(leaked).unwrap(), (1, 0));
    }
}
