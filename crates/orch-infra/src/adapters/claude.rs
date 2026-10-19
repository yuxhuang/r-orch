//! Claude Code adapter for `kind = "claude"`.
//!
//! Spawns `claude --print --output-format stream-json` in headless mode and
//! parses JSON lines from stdout.
//!
//! Neither prompt travels in argv: Linux caps a single argv string at 128 KiB
//! (`MAX_ARG_STRLEN`) and the orchestrator system prompt, which embeds every
//! agent's role, can exceed it, failing `exec` with `E2BIG`. The system prompt
//! goes through a private file (`--system-prompt-file`) in a 0700 per-user
//! directory, deleted when the event stream is dropped, and the task prompt
//! through stdin.

use std::ffi::OsStr;
use std::io::Write as _;
use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Once};
use std::time::{Duration, SystemTime};

use async_trait::async_trait;
use orch_core::adapter::{Adapter, AdapterError, AdapterTestResult, ExecuteHandle, ExecuteParams};
use orch_core::clock::Clock;
use orch_core::process::{DEFAULT_KILL_GRACE, ProcessManager, SpawnOptions, StdioMode};
use orch_domain::AdapterErrorKind;
use tempfile::TempPath;
use tokio::io::AsyncWriteExt;
use tokio::runtime::{Handle, RuntimeFlavor};

use super::util::{
    parse_stream_json_line, probe_version, streaming_events, system_clock, version_probe_failure,
};
use crate::process::spawn::{ChildWriter, StderrPolicy, spawn_child};

const INSTALL_HINT: &str = "Claude Code CLI not found. Install: npm i -g @anthropic-ai/claude-code";

pub struct ClaudeAdapter {
    process_manager: Arc<dyn ProcessManager>,
    clock: Arc<dyn Clock>,
}

impl ClaudeAdapter {
    pub fn new(process_manager: Arc<dyn ProcessManager>) -> Self {
        ClaudeAdapter {
            process_manager,
            clock: system_clock(),
        }
    }

    /// Replace the timestamp source, so tests can assert on exact events.
    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }
}

impl std::fmt::Debug for ClaudeAdapter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClaudeAdapter").finish_non_exhaustive()
    }
}

const SYSTEM_PROMPT_FILE_PREFIX: &str = "orch-claude-system-prompt-";

/// Files older than this are leftovers of a run that never got to clean up.
/// Generous, so a sweep can never delete the file of a long-running agent
/// that another orch engine on the same machine is still using.
const STALE_AFTER: Duration = Duration::from_secs(24 * 60 * 60);

/// `claude --print` gives up on stdin after 3 s without a first byte
/// ("proceeding without it"), so the first chunk is written before `execute`
/// returns instead of leaving it to a task the scheduler may delay. A fresh
/// pipe holds ~64 KiB; the rest follows from a spawned task.
const FIRST_CHUNK_BYTES: usize = 32 * 1024;
const FIRST_CHUNK_TIMEOUT: Duration = Duration::from_secs(1);

/// `--max-turns` is **always** passed, defaulting to 50. The prompt is not an
/// argument: `--print` reads it from stdin.
fn build_args(
    params: &ExecuteParams,
    system_prompt_file: Option<&Path>,
) -> Result<Vec<String>, AdapterError> {
    let mut args = vec![
        "--print".to_owned(),
        "--output-format".to_owned(),
        "stream-json".to_owned(),
        // Explicit, so the task prompt is always plain data: with
        // `stream-json` input claude would parse it as protocol messages.
        "--input-format".to_owned(),
        "text".to_owned(),
        "--max-turns".to_owned(),
        params.config.max_turns.unwrap_or(50).to_string(),
        "--verbose".to_owned(),
        // Agents run autonomously, so an interactive permission prompt would
        // hang forever.
        "--dangerously-skip-permissions".to_owned(),
    ];

    if let Some(model) = &params.config.model {
        args.push("--model".to_owned());
        args.push(model.clone());
    }
    if let Some(effort) = params.config.effort {
        args.push("--effort".to_owned());
        args.push(effort.as_str().to_owned());
    }
    if let Some(path) = system_prompt_file {
        let path = path.to_str().ok_or_else(|| {
            AdapterError::new(
                format!(
                    "Failed to spawn Claude: the system prompt file path is not valid UTF-8: {}",
                    path.display()
                ),
                None,
            )
        })?;
        args.push("--system-prompt-file".to_owned());
        args.push(path.to_owned());
    }
    Ok(args)
}

/// The orchestrator-generated (cacheable) system prompt wins over the
/// per-agent one. A blank source (empty or whitespace) counts as absent and
/// falls through to the next; if both are blank there is no system prompt and
/// claude uses its default (the old `--system-prompt ""` replaced it with
/// nothing).
fn effective_system_prompt(params: &ExecuteParams) -> Option<&str> {
    let non_blank = |prompt: &&str| !prompt.trim().is_empty();
    params.system_prompt.as_deref().filter(non_blank).or(params
        .config
        .system_prompt
        .as_deref()
        .filter(non_blank))
}

fn io_failure(what: &str, err: &std::io::Error) -> AdapterError {
    tracing::error!(error = %err, "{what}");
    AdapterError::with_kind(
        format!("{what}: {err}"),
        AdapterErrorKind::SpawnFailed,
        None,
    )
}

/// Candidate private directories, most preferred first: an absolute
/// `$XDG_RUNTIME_DIR/orch` (per-user tmpfs), then `<temp dir>/orch-<uid>`.
/// A relative `XDG_RUNTIME_DIR` is ignored: it would resolve against the
/// process cwd, which may be a git-tracked directory.
fn private_dir_candidates(
    xdg_runtime_dir: Option<&OsStr>,
    temp_dir: &Path,
    uid: u32,
) -> Vec<PathBuf> {
    let mut candidates = Vec::with_capacity(2);
    if let Some(runtime) = xdg_runtime_dir.map(Path::new)
        && runtime.is_absolute()
    {
        candidates.push(runtime.join("orch"));
    }
    candidates.push(temp_dir.join(format!("orch-{uid}")));
    candidates
}

/// The first candidate that passes [`ensure_private_dir`]; the error of the
/// last one if none does. The XDG directory is a preference, not a
/// requirement, so its failures are only logged at debug.
fn resolve_private_dir(candidates: &[PathBuf]) -> Result<PathBuf, AdapterError> {
    let mut last_error = None;
    for (index, dir) in candidates.iter().enumerate() {
        match ensure_private_dir(dir) {
            Ok(()) => return Ok(dir.clone()),
            Err(err) => {
                if index + 1 < candidates.len() {
                    tracing::debug!(dir = %dir.display(), error = %err, "private directory unusable, trying the next");
                }
                last_error = Some(err);
            }
        }
    }
    Err(last_error.expect("at least one candidate"))
}

/// Create `dir` (mode 0700) if missing and refuse it unless it is a real
/// directory — not a symlink — owned by this user with exactly mode 0700. A
/// predictable path in a shared `/tmp` could otherwise have been pre-created
/// by someone else.
fn ensure_private_dir(dir: &Path) -> Result<(), AdapterError> {
    ensure_private_dir_created_with(dir, 0o700)
}

/// `create_mode` is a seam for tests simulating a restrictive umask, which
/// masks the mode given at creation.
fn ensure_private_dir_created_with(dir: &Path, create_mode: u32) -> Result<(), AdapterError> {
    let fail =
        |err: std::io::Error| io_failure("Failed to prepare the Claude private directory", &err);
    match std::fs::DirBuilder::new().mode(create_mode).create(dir) {
        // Only a directory we just created is repaired (a umask such as 0277
        // would otherwise leave it unusable forever); a pre-existing one with
        // the wrong mode is refused below, never chmod-ed.
        Ok(()) => {
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).map_err(fail)?
        }
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(err) => return Err(fail(err)),
    }
    let meta = std::fs::symlink_metadata(dir).map_err(fail)?;
    let mode = meta.permissions().mode() & 0o7777;
    if !meta.is_dir() || meta.uid() != nix::unistd::Uid::current().as_raw() || mode != 0o700 {
        let message = format!(
            "Refusing to use {}: it must be a real directory owned by the current user with mode 0700",
            dir.display()
        );
        tracing::error!("{message}");
        return Err(AdapterError::with_kind(
            message,
            AdapterErrorKind::SpawnFailed,
            None,
        ));
    }
    Ok(())
}

/// Remove prompt files older than [`STALE_AFTER`] from `dir`. Only regular
/// files with our prefix are considered; fresh ones may belong to another
/// running engine. A file of a run that has lasted over 24 h can be removed
/// too, which is harmless: claude reads it once at startup.
fn sweep_stale_files(dir: &Path, now: SystemTime) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        if !entry
            .file_name()
            .to_string_lossy()
            .starts_with(SYSTEM_PROMPT_FILE_PREFIX)
        {
            continue;
        }
        let Ok(meta) = std::fs::symlink_metadata(entry.path()) else {
            continue;
        };
        let stale = meta.is_file()
            && meta
                .modified()
                .ok()
                .and_then(|modified| now.duration_since(modified).ok())
                .is_some_and(|age| age > STALE_AFTER);
        if stale {
            match std::fs::remove_file(entry.path()) {
                Ok(()) => {
                    tracing::debug!(path = %entry.path().display(), "removed a stale claude system prompt file")
                }
                Err(err) => {
                    tracing::warn!(path = %entry.path().display(), error = %err, "failed to remove a stale claude system prompt file")
                }
            }
        }
    }
}

/// Write the system prompt to a fresh file in the private directory: random
/// name, created exclusively with mode 0600. The directory is an absolute
/// `$XDG_RUNTIME_DIR` subdirectory or one under the OS temp dir, never under
/// the (possibly git-tracked) agent workspace. The returned guard deletes the file when
/// dropped.
fn write_system_prompt_file(system_prompt: &str) -> Result<TempPath, AdapterError> {
    static SWEEP: Once = Once::new();

    let dir = resolve_private_dir(&private_dir_candidates(
        std::env::var_os("XDG_RUNTIME_DIR").as_deref(),
        &std::env::temp_dir(),
        nix::unistd::Uid::current().as_raw(),
    ))?;
    SWEEP.call_once(|| sweep_stale_files(&dir, SystemTime::now()));

    let fail =
        |err: std::io::Error| io_failure("Failed to write the Claude system prompt file", &err);
    let mut file = tempfile::Builder::new()
        .prefix(SYSTEM_PROMPT_FILE_PREFIX)
        .suffix(".txt")
        .tempfile_in(&dir)
        .map_err(fail)?;
    file.write_all(system_prompt.as_bytes()).map_err(fail)?;
    file.flush().map_err(fail)?;
    Ok(file.into_temp_path())
}

/// Write the first chunk before returning. Tokio pipes only report
/// writability after a reactor turn, so on a multi-thread runtime this blocks
/// briefly (in place) for that turn. `block_in_place` panics on a
/// current-thread runtime, and with no runtime there is nothing to block on;
/// in both cases nothing is written here and the spawned task delivers
/// everything.
fn write_first_chunk(stdin: &mut ChildWriter, bytes: &[u8]) -> std::io::Result<usize> {
    let Ok(handle) = Handle::try_current() else {
        return Ok(0);
    };
    if handle.runtime_flavor() != RuntimeFlavor::MultiThread {
        return Ok(0);
    }
    let chunk = &bytes[..bytes.len().min(FIRST_CHUNK_BYTES)];
    let written = tokio::task::block_in_place(|| {
        handle.block_on(tokio::time::timeout(
            FIRST_CHUNK_TIMEOUT,
            stdin.write(chunk),
        ))
    });
    // A timeout means the pipe was not writable yet; the task retries.
    written.unwrap_or(Ok(0))
}

/// Deliver the task prompt on stdin, then close it. `claude --print` waits up
/// to 3 s for the first byte, after which it proceeds without a prompt, and
/// then reads until EOF.
///
/// A failed write is logged at `warn` with the adapter name, but is not
/// surfaced as an event. A failure after the first chunk can leave claude with
/// a truncated prompt and a run that still succeeds; only the log shows it.
fn write_prompt(stdin: Option<ChildWriter>, prompt: String) {
    let Some(mut stdin) = stdin else {
        return;
    };
    let bytes = prompt.into_bytes();
    let first = match write_first_chunk(&mut stdin, &bytes) {
        Ok(written) => written,
        Err(err) => {
            tracing::warn!(adapter = "claude", error = %err, "failed to write the prompt to stdin");
            return;
        }
    };
    if first == bytes.len() {
        // Dropping the pipe is what delivers EOF.
        return;
    }
    tokio::spawn(async move {
        if let Err(err) = stdin.write_all(&bytes[first..]).await {
            tracing::warn!(
                adapter = "claude",
                error = %err,
                written = first,
                total = bytes.len(),
                "failed to write the rest of the prompt to stdin"
            );
            return;
        }
        if let Err(err) = stdin.shutdown().await {
            tracing::debug!(adapter = "claude", error = %err, "failed to close stdin");
        }
    });
}

#[async_trait]
impl Adapter for ClaudeAdapter {
    fn kind(&self) -> &'static str {
        "claude"
    }

    async fn test(&self) -> AdapterTestResult {
        match probe_version(self.process_manager.as_ref(), "claude").await {
            Ok(version) => AdapterTestResult::ok(version),
            Err(message) => version_probe_failure(INSTALL_HINT, &message),
        }
    }

    fn execute(&self, params: ExecuteParams) -> Result<ExecuteHandle, AdapterError> {
        // Dropped (deleting the file) on any early return, including a failed
        // spawn.
        let system_prompt_file = effective_system_prompt(&params)
            .map(write_system_prompt_file)
            .transpose()?;
        let args = build_args(&params, system_prompt_file.as_deref())?;
        let mut io = spawn_child(
            &self.process_manager,
            "claude",
            &args,
            SpawnOptions {
                cwd: params.workspace,
                env: params.env,
                // stdin must be a pipe to carry the prompt.
                stdin: StdioMode::Pipe,
                inherit_env: params.inherit_env,
            },
            StderrPolicy::Tail,
            &params.cancel,
        )?;

        write_prompt(io.stdin.take(), params.prompt);

        let pid = io.pid;
        let clock = Arc::clone(&self.clock);
        let events = streaming_events(
            io,
            move |line| {
                // Owning the file here ties its deletion to the event stream
                // being dropped; `the_system_prompt_file_is_removed_after_the_run`
                // and `..._when_the_run_is_dropped` depend on this capture.
                let _keep_alive = &system_prompt_file;
                parse_stream_json_line(line, clock.as_ref(), true)
            },
            "Claude",
            params.cancel,
        );
        Ok(ExecuteHandle { pid, events })
    }

    async fn stop(&self, pid: u32) -> orch_core::Result<()> {
        self.process_manager
            .kill_with_grace(pid, DEFAULT_KILL_GRACE)
            .await;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn touch(dir: &Path, name: &str, age: Duration) -> PathBuf {
        let path = dir.join(name);
        let file = std::fs::File::create(&path).expect("create");
        file.set_modified(SystemTime::now() - age)
            .expect("set mtime");
        path
    }

    #[test]
    fn the_sweep_removes_only_old_prompt_files() {
        let dir = tempfile::tempdir().expect("tempdir");
        let old = touch(
            dir.path(),
            "orch-claude-system-prompt-old.txt",
            STALE_AFTER + Duration::from_secs(60),
        );
        let fresh = touch(
            dir.path(),
            "orch-claude-system-prompt-fresh.txt",
            Duration::from_secs(60),
        );
        let unrelated = touch(
            dir.path(),
            "unrelated.txt",
            STALE_AFTER + Duration::from_secs(60),
        );

        sweep_stale_files(dir.path(), SystemTime::now());

        assert!(!old.exists(), "the stale prompt file should be removed");
        assert!(fresh.exists(), "a fresh file may belong to a live run");
        assert!(unrelated.exists(), "only our own files are swept");
    }

    #[test]
    fn the_private_dir_is_created_with_mode_0700() {
        let parent = tempfile::tempdir().expect("tempdir");
        let dir = parent.path().join("orch");
        ensure_private_dir(&dir).expect("create");
        ensure_private_dir(&dir).expect("accept it again");
        let mode = std::fs::metadata(&dir).expect("meta").permissions().mode();
        assert_eq!(mode & 0o7777, 0o700);
    }

    #[test]
    fn a_loose_or_symlinked_private_dir_is_refused() {
        let parent = tempfile::tempdir().expect("tempdir");

        let loose = parent.path().join("loose");
        std::fs::create_dir(&loose).expect("mkdir");
        std::fs::set_permissions(&loose, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        assert!(ensure_private_dir(&loose).is_err());

        let target = parent.path().join("target");
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&target)
            .expect("mkdir");
        let link = parent.path().join("link");
        std::os::unix::fs::symlink(&target, &link).expect("symlink");
        assert!(ensure_private_dir(&link).is_err());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_first_chunk_is_written_before_returning() {
        let mut child = tokio::process::Command::new("sleep")
            .arg("5")
            .stdin(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .expect("spawn");
        let mut stdin: ChildWriter = Box::new(child.stdin.take().expect("stdin"));
        let bytes = vec![b'x'; 200 * 1024];
        let written = write_first_chunk(&mut stdin, &bytes).expect("write");
        assert_eq!(written, FIRST_CHUNK_BYTES);
    }

    #[test]
    fn a_missing_xdg_dir_falls_back_to_the_temp_dir() {
        let temp = tempfile::tempdir().expect("tempdir");
        let candidates = private_dir_candidates(
            Some(OsStr::new("/nonexistent-orch-test/run/user/1")),
            temp.path(),
            4242,
        );
        assert_eq!(candidates.len(), 2);
        let dir = resolve_private_dir(&candidates).expect("fallback");
        assert_eq!(dir, temp.path().join("orch-4242"));
    }

    #[test]
    fn it_errors_only_when_every_candidate_fails() {
        let candidates = [
            PathBuf::from("/nonexistent-orch-test/a/orch"),
            PathBuf::from("/nonexistent-orch-test/b/orch"),
        ];
        let err = resolve_private_dir(&candidates).expect_err("both fail");
        assert_eq!(err.kind, AdapterErrorKind::SpawnFailed);
    }

    #[test]
    fn a_relative_xdg_dir_is_ignored() {
        let candidates = private_dir_candidates(Some(OsStr::new("run/user")), Path::new("/tmp"), 7);
        assert_eq!(candidates, [PathBuf::from("/tmp/orch-7")]);
        let candidates = private_dir_candidates(Some(OsStr::new("")), Path::new("/tmp"), 7);
        assert_eq!(candidates, [PathBuf::from("/tmp/orch-7")]);
        let candidates =
            private_dir_candidates(Some(OsStr::new("/run/user/7")), Path::new("/tmp"), 7);
        assert_eq!(candidates[0], PathBuf::from("/run/user/7/orch"));
    }

    #[test]
    fn a_dir_created_under_a_restrictive_umask_is_repaired() {
        let parent = tempfile::tempdir().expect("tempdir");
        let dir = parent.path().join("orch");
        ensure_private_dir_created_with(&dir, 0o600).expect("repaired");
        let mode = std::fs::metadata(&dir).expect("meta").permissions().mode();
        assert_eq!(mode & 0o7777, 0o700);
    }

    #[test]
    fn a_pre_existing_dir_is_never_repaired() {
        let parent = tempfile::tempdir().expect("tempdir");
        let dir = parent.path().join("orch");
        std::fs::create_dir(&dir).expect("mkdir");
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o600)).expect("chmod");
        assert!(ensure_private_dir_created_with(&dir, 0o700).is_err());
        let mode = std::fs::metadata(&dir).expect("meta").permissions().mode();
        assert_eq!(mode & 0o7777, 0o600, "its mode must be left alone");
    }

    #[test]
    fn no_runtime_means_no_synchronous_write() {
        // `tokio::process` needs a runtime to build the pipe, so use one only
        // for setup and call the function from outside it.
        let rt = tokio::runtime::Runtime::new().expect("runtime");
        let mut stdin: ChildWriter = rt.block_on(async {
            let mut child = tokio::process::Command::new("sleep")
                .arg("5")
                .stdin(std::process::Stdio::piped())
                .kill_on_drop(true)
                .spawn()
                .expect("spawn");
            Box::new(child.stdin.take().expect("stdin")) as ChildWriter
        });
        assert_eq!(write_first_chunk(&mut stdin, b"hello").expect("ok"), 0);
    }
}
