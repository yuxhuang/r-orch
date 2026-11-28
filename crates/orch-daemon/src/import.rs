//! Offline bounded import. No daemon, protocol operation or storage handle escapes.
use orch_core::store::{Caller, Principal};
use orch_infra::data_dir::confined::{ConfinedDir, ConfinedError};
use orch_infra::data_dir::{DataDir, Holder, ImportGuard, LockKind, paths};
use orch_infra::import::{ImportBatch, preflight};
use orch_infra::sqlite::{SqliteStorage, StorageOptions, WriteOutcome};
use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

/// Environment choices are captured by the CLI before entering the operation.
pub struct ImportRequest {
    pub source: PathBuf,
    pub data_home: PathBuf,
    pub data_dir: Option<PathBuf>,
    pub tmp_dir: PathBuf,
    pub socket: Option<PathBuf>,
    pub dry_run: bool,
    pub resume: bool,
    pub assume_stopped: Vec<String>,
}

/// JSON is the bounded preflight report, not a storage or execution interface.
pub struct ImportOutcome {
    pub exit_code: u8,
    pub report_json: String,
    pub imported: u64,
    pub unfinished: bool,
    pub procedural_exclusion: bool,
    pub detail: String,
}

/// Set cancellation and keep awaiting this future. The owned task also drains
/// blocking work, admitted writes and storage shutdown if its waiter is dropped.
pub async fn import_offline(request: ImportRequest, cancel: Arc<AtomicBool>) -> ImportOutcome {
    match tokio::spawn(execute(request, cancel)).await {
        Ok(outcome) => outcome,
        Err(_) => {
            let mut result = outcome(1, "offline import worker failed; write outcome unknown");
            result.unfinished = true;
            result
        }
    }
}

fn outcome(exit_code: u8, detail: &str) -> ImportOutcome {
    ImportOutcome {
        exit_code,
        report_json: "null".into(),
        imported: 0,
        unfinished: false,
        procedural_exclusion: false,
        detail: detail.into(),
    }
}
fn cancelled(cancel: &AtomicBool) -> bool {
    cancel.load(Ordering::Relaxed)
}

async fn no_listener(source: &Path, request: &ImportRequest) -> Result<(), ()> {
    use crate::discovery::{SocketState, classify_socket, endpoint, read_record};
    let mut sockets = vec![endpoint(source, None, &request.tmp_dir).map_err(|_| ())?];
    if let Some(socket) = &request.socket {
        sockets.push(socket.clone());
    }
    if let Some(record) = read_record(source).map_err(|_| ())? {
        sockets.push(record.socket.as_path().to_owned());
    }
    for socket in sockets {
        match classify_socket(&socket).await.map_err(|_| ())? {
            SocketState::Absent | SocketState::Stale { .. } => {}
            _ => return Err(()),
        }
    }
    Ok(())
}

fn render_report(report: &preflight::Report) -> Result<String, serde_json::Error> {
    let mut value = serde_json::to_value(report)?;
    for class in ["refusals", "notes"] {
        if let Some(listed) = value
            .get_mut(class)
            .and_then(|v| v.get_mut("listed"))
            .and_then(|v| v.as_array_mut())
        {
            for finding in listed {
                if let Some(path) = finding.get("path").and_then(|v| v.as_str()) {
                    finding["path"] = format!(".orchestry/{path}").into();
                }
            }
        }
    }
    serde_json::to_string(&value)
}

fn future_destination(path: &Path) -> std::io::Result<PathBuf> {
    let mut anchor = path;
    let mut tail = Vec::new();
    loop {
        match std::fs::symlink_metadata(anchor) {
            Ok(metadata) => {
                // Reject a final symlink before canonicalization touches it.
                // Looking up a missing descendant can already traverse an
                // ancestor symlink and advance that ancestor's atime.
                if metadata.file_type().is_symlink() {
                    return Err(std::io::Error::other("destination alias refused"));
                }
                let mut resolved = anchor.canonicalize()?;
                for part in tail.into_iter().rev() {
                    resolved.push(part);
                }
                return Ok(resolved);
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                tail.push(
                    anchor
                        .file_name()
                        .ok_or_else(|| std::io::Error::other("invalid destination component"))?
                        .to_owned(),
                );
                anchor = anchor
                    .parent()
                    .ok_or_else(|| std::io::Error::other("destination has no existing ancestor"))?;
            }
            Err(e) => return Err(e),
        }
    }
}

fn acquire_data(
    path: &Path,
    existing: Option<ImportGuard>,
    wrote: &mut bool,
) -> Result<DataDir, ()> {
    let holder = Holder {
        kind: LockKind::Import,
        pid: std::process::id(),
        started_at: orch_core::clock::Clock::now_iso(&orch_infra::clock::SystemClock),
        binary_version: env!("CARGO_PKG_VERSION").into(),
        executable: "orch import".into(),
    };
    match existing {
        Some(guard) => match guard.into_data_dir(&holder) {
            Ok(data) => {
                *wrote = true;
                Ok(data)
            }
            Err(error) => {
                *wrote |= error.may_have_written;
                Err(())
            }
        },
        None => {
            // Exclusive creation cannot repair a destination that appeared since
            // preflight. Mark the actual mkdir before any subsequent fallible I/O.
            let parent = ConfinedDir::open(path.parent().ok_or(())?).map_err(|_| ())?;
            create_private_dir(&parent, path.file_name().ok_or(())?, wrote).map_err(|_| ())?;
            DataDir::acquire(path, &holder).map_err(|_| ())
        }
    }
}

fn create_private_dir(
    parent: &ConfinedDir,
    name: &std::ffi::OsStr,
    wrote: &mut bool,
) -> Result<ConfinedDir, ConfinedError> {
    use nix::sys::stat::{Mode, fchmod, mkdirat};
    // R112 needs the mkdir outcome before open/chmod can fail. The confined
    // create helper intentionally hides that checkpoint; use the same nix API.
    mkdirat(parent, name, Mode::S_IRWXU).map_err(std::io::Error::from)?;
    *wrote = true;
    let dir = parent.open_dir(name)?;
    fchmod(&dir, Mode::S_IRWXU).map_err(std::io::Error::from)?;
    Ok(dir)
}

fn ensure_data_home(path: &Path, wrote: &mut bool) -> Result<ConfinedDir, ()> {
    match ConfinedDir::open(path) {
        Ok(dir) => Ok(dir),
        Err(ConfinedError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => {
            let parent = ensure_data_home(path.parent().ok_or(())?, wrote)?;
            let name = path.file_name().ok_or(())?;
            match create_private_dir(&parent, name, wrote) {
                Ok(dir) => Ok(dir),
                Err(ConfinedError::Io(e)) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    parent.open_dir(name).map_err(|_| ())
                }
                Err(_) => Err(()),
            }
        }
        Err(_) => Err(()),
    }
}

fn disk_floor(data: &DataDir) -> Result<(), ()> {
    let space = orch_infra::sqlite::migrations::DiskSpace::read(data).map_err(|_| ())?;
    if space.available < (5 * 1024 * 1024 * 1024).max(space.total / 20) {
        return Err(());
    }
    Ok(())
}

async fn execute(request: ImportRequest, cancel: Arc<AtomicBool>) -> ImportOutcome {
    if std::env::var_os("ORCH_TASK_ID").is_some() || std::env::var_os("ORCH_AGENT_ID").is_some() {
        return outcome(2, "import is an owner-only offline operation");
    }
    let source = match request.source.canonicalize() {
        Ok(p) => p,
        Err(_) => return outcome(2, "cannot resolve source project"),
    };
    let project = match paths::default_name(&source) {
        Ok(id) => id,
        Err(_) => return outcome(2, "cannot derive project identity"),
    };
    if !request.data_home.is_absolute()
        || request.data_dir.as_ref().is_some_and(|p| !p.is_absolute())
    {
        return outcome(2, "data home and data directory must be absolute");
    }
    // Do not reinterpret an interrupted record as absence and retarget it.
    let dest = if let Some(path) = &request.data_dir {
        path.clone()
    } else {
        let records = request.data_home.join("projects");
        let hash = {
            use sha2::{Digest, Sha256};
            format!("{:x}", Sha256::digest(project.as_bytes()))
        };
        match std::fs::symlink_metadata(records.join(hash)) {
            Ok(_) => match paths::read_project_record_for_import(&request.data_home, &project) {
                Ok(path) => path,
                Err(_) => return outcome(2, "project record is invalid; refusing to retarget it"),
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => request.data_home.join(&project),
            Err(_) => return outcome(2, "cannot inspect project record"),
        }
    };
    // Resolve the deepest existing ancestor, including aliases above a missing
    // data home. A failed resolution is not permission to create below source.
    let resolved_dest = match future_destination(&dest) {
        Ok(path) => path,
        Err(_) => return outcome(2, "cannot resolve destination before writing"),
    };
    if resolved_dest.starts_with(&source) {
        return outcome(2, "destination must be outside the source project");
    }
    if source.to_str().is_none() {
        return outcome(
            2,
            "this bounded caller cannot represent a non-UTF8 source identity",
        );
    }
    let mut result = outcome(2, "");
    let existing = match std::fs::symlink_metadata(&dest) {
        Ok(_) => {
            let path = dest.clone();
            match tokio::task::spawn_blocking(move || ImportGuard::acquire(&path)).await {
                Ok(Ok(guard)) => Some(guard),
                _ => {
                    return outcome(
                        2,
                        "cannot acquire existing destination without modification",
                    );
                }
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(_) => return outcome(2, "cannot inspect destination"),
    };
    let options = preflight::Options {
        assume_stopped: request.assume_stopped.iter().cloned().collect(),
        cancel: cancel.clone(),
        ..Default::default()
    };
    let prepared =
        match preflight::prepare_offline(source.clone(), options, Arc::new(preflight::SystemProbe))
            .await
        {
            Ok(prepared) => Arc::new(prepared),
            Err(_) => {
                return outcome(
                    2,
                    "source check failed or was cancelled; no import batches admitted",
                );
            }
        };
    result.procedural_exclusion = prepared.procedural_exclusion();
    if no_listener(&source, &request).await.is_err() {
        return outcome(
            2,
            "a daemon may still be listening; stop both daemons before import",
        );
    }
    if prepared.verify_source().is_err() {
        return outcome(2, "source owner guard or directory changed");
    }
    result.report_json = match render_report(&prepared.report) {
        Ok(text) => text,
        Err(_) => return outcome(2, "cannot encode source report"),
    };
    if prepared.report.refused() || !prepared.report.confirmations.is_empty() {
        result.detail = "source refused; no import batches admitted".into();
        return result;
    }
    if cancelled(&cancel) {
        result.detail = "cancelled before writing".into();
        return result;
    }
    if request.dry_run {
        result.exit_code = 0;
        result.detail = "dry run; no import batches admitted".into();
        return result;
    }
    if existing.is_some() && !request.resume {
        result.detail = "existing destination requires --resume; no import batches admitted".into();
        return result;
    }
    if prepared.template_count() == 0 {
        result.detail =
            "bounded caller has no templates to import; other families remain unsupported".into();
        return result;
    }
    // Perform the first second-read before creating the destination.
    let input = prepared.clone();
    let first = match tokio::task::spawn_blocking(move || input.load_template(0)).await {
        Ok(Ok(row)) => row,
        _ => {
            result.detail = "template changed or load was cancelled before writing".into();
            return result;
        }
    };
    if cancelled(&cancel) {
        result.detail = "cancelled before writing".into();
        return result;
    }
    // Only a successful mkdir or holder publication crosses the write boundary.
    // Unknown publication/startup outcomes remain conservatively unfinished.
    let data = {
        let home = request.data_home.clone();
        let create_home = request.data_dir.is_none();
        let path = dest.clone();
        match tokio::task::spawn_blocking(move || {
            let mut wrote = false;
            let result = (|| {
                if existing.is_none() && create_home {
                    ensure_data_home(&home, &mut wrote)?;
                }
                acquire_data(&path, existing, &mut wrote)
            })();
            (result, wrote)
        })
        .await
        {
            Ok((Ok(data), _)) => Arc::new(data),
            Ok((Err(()), wrote)) => {
                result.exit_code = if wrote { 1 } else { 2 };
                result.unfinished = wrote;
                result.detail =
                    "cannot create private destination; existing entries are never repaired".into();
                return result;
            }
            Err(_) => {
                result.exit_code = 1;
                result.unfinished = true;
                result.detail = "destination setup failed; write outcome unknown".into();
                return result;
            }
        }
    };
    result.exit_code = 1;
    result.unfinished = true;
    let owned = data.clone();
    let storage = match tokio::task::spawn_blocking(move || {
        disk_floor(&owned)?;
        SqliteStorage::open(owned, StorageOptions::default()).map_err(|_| ())
    })
    .await
    {
        Ok(Ok(storage)) => storage,
        _ => {
            result.detail = "cannot open native writer or disk is below floor".into();
            return result;
        }
    };
    let caller = Caller {
        project,
        principal: Principal::Owner {
            identity: "local_owner".into(),
        },
    };
    let checked = storage
        .check_import_source(
            caller.clone(),
            source.to_string_lossy().into_owned(),
            prepared.template_inventory(),
        )
        .await;
    if !matches!(checked, Ok(ref attempt) if attempt.outcome == WriteOutcome::Committed && attempt.result.is_ok())
    {
        result.detail = "native source/manifest check refused resume; import unfinished".into();
        if storage.close().await.is_err() {
            result.detail.push_str("; writer shutdown failed");
        }
        return result;
    }
    let mut first = Some(first);
    for index in 0..prepared.template_count() {
        if cancelled(&cancel) {
            result.detail = "cancelled; import unfinished".into();
            break;
        }
        let row = if let Some(row) = first.take() {
            row
        } else {
            let input = prepared.clone();
            match tokio::task::spawn_blocking(move || input.load_template(index)).await {
                Ok(Ok(row)) => row,
                _ => {
                    result.detail = "source changed or load cancelled; import unfinished".into();
                    break;
                }
            }
        };
        if cancelled(&cancel) || prepared.verify_source().is_err() || disk_floor(&data).is_err() {
            result.detail =
                "cancelled, source guard changed or disk below floor; import unfinished".into();
            break;
        }
        // Never select cancellation against this admitted future: retain and
        // drain the authoritative writer receipt before close/unlock.
        match storage
            .import_batch(
                caller.clone(),
                ImportBatch {
                    source: source.to_string_lossy().into_owned(),
                    rows: vec![row],
                },
            )
            .await
        {
            Ok(attempt) if attempt.outcome == WriteOutcome::Committed && attempt.result.is_ok() => {
                result.imported += 1
            }
            _ => {
                result.detail =
                    "native batch did not confirm success; import unfinished, no automatic retry"
                        .into();
                break;
            }
        }
    }
    if storage.close().await.is_err() {
        result.detail = "writer shutdown failed; import unfinished".into();
    }
    if result.detail.is_empty() {
        result.detail = "templates retained; import unfinished: config, state, other entity families, events, attachments, R121 and R125 are not implemented".into();
    }
    // `storage.close` has drained before these held guards leave scope.
    drop(storage);
    drop(data);
    drop(prepared);
    result
}
