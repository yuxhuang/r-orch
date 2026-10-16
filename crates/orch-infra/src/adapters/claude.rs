//! Claude Code adapter — `src/infrastructure/adapters/claude.ts`,
//! `kind = "claude"` (`docs/parity/adapters.md` §2.1).
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

use std::io::Write as _;
use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Once};
use std::time::{Duration, SystemTime};

use async_trait::async_trait;
use orch_core::adapter::{Adapter, AdapterError, AdapterTestResult, ExecuteHandle, ExecuteParams};
use orch_core::clock::Clock;
use orch_core::process::{DEFAULT_KILL_GRACE, ProcessManager, SpawnOptions, StdioMode};
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
/// per-agent one. An empty prompt counts as none: claude then uses its default
/// system prompt (the old `--system-prompt ""` replaced it with nothing).
fn effective_system_prompt(params: &ExecuteParams) -> Option<&str> {
    params
        .system_prompt
        .as_deref()
        .or(params.config.system_prompt.as_deref())
        .filter(|prompt| !prompt.is_empty())
}

fn io_failure(what: &str, err: &std::io::Error) -> AdapterError {
    tracing::error!(error = %err, "{what}");
    AdapterError::new(format!("{what}: {err}"), None)
}

/// `$XDG_RUNTIME_DIR/orch` (per-user, tmpfs, 0700) when available, else
/// `<temp dir>/orch-<uid>`.
fn private_dir_path() -> PathBuf {
    match std::env::var_os("XDG_RUNTIME_DIR") {
        Some(runtime) if !runtime.is_empty() => Path::new(&runtime).join("orch"),
        _ => std::env::temp_dir().join(format!("orch-{}", nix::unistd::Uid::current())),
    }
}

/// Create `dir` (mode 0700) if missing and refuse it unless it is a real
/// directory — not a symlink — owned by this user with exactly mode 0700. A
/// predictable path in a shared `/tmp` could otherwise have been pre-created
/// by someone else.
fn ensure_private_dir(dir: &Path) -> Result<(), AdapterError> {
    let fail =
        |err: std::io::Error| io_failure("Failed to prepare the Claude private directory", &err);
    match std::fs::DirBuilder::new().mode(0o700).create(dir) {
        Ok(()) => {}
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
        return Err(AdapterError::new(message, None));
    }
    Ok(())
}

/// Remove prompt files older than [`STALE_AFTER`] from `dir`. Only regular
/// files with our prefix are considered; fresh ones belong to live runs.
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
/// name, created exclusively with mode 0600, never under the (possibly
/// git-tracked) agent workspace. The returned guard deletes the file when
/// dropped.
fn write_system_prompt_file(system_prompt: &str) -> Result<TempPath, AdapterError> {
    static SWEEP: Once = Once::new();

    let dir = private_dir_path();
    ensure_private_dir(&dir)?;
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
/// briefly (in place) for that turn; on a current-thread runtime blocking would
/// stall the reactor itself, so nothing is written here and the spawned task
/// delivers everything.
fn write_first_chunk(stdin: &mut ChildWriter, bytes: &[u8]) -> std::io::Result<usize> {
    let handle = Handle::current();
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
/// A failed write is logged with the adapter name; claude then exits non-zero
/// ("Input must be provided"), which surfaces as the run's exit error with
/// its stderr tail, so the failure is attributable without a separate event.
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
}
