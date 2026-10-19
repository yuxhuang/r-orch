//! Workspace preparation, cleanup and the merge strategy.
//!
//! Everything shells out to `git` (and `rsync`) with an exact argv, through the
//! [`ProcessManager`] port, with the project root as the working directory. Spawn failures, spawn errors and signal exits all
//! surface as exit code 1 and nothing here throws on them.

mod manager;
mod merge;

#[cfg(test)]
mod test_support;

use std::path::Path;
use std::sync::{Mutex, PoisonError};

pub use manager::GitWorkspaceManager;
pub use merge::MergeStrategy;

use orch_core::process::{ProcessManager, SpawnOptions};
use tokio::io::{AsyncRead, AsyncReadExt};

/// Cap on the interleaved stdout+stderr buffer (checked before each append, so
/// it can overshoot by one chunk).
const MAX_COMBINED_LEN: usize = 2000;

/// What a finished process produced.
#[derive(Debug, Default)]
pub(crate) struct Captured {
    pub stdout: String,
    /// stdout and stderr chunks interleaved in arrival order, capped.
    pub combined: String,
    pub code: i32,
}

#[derive(Default)]
struct Buffers {
    stdout: String,
    combined: String,
}

async fn pump<R: AsyncRead + Unpin>(reader: Option<R>, buffers: &Mutex<Buffers>, is_stdout: bool) {
    let Some(mut reader) = reader else { return };
    let mut chunk = vec![0u8; 8192];
    loop {
        match reader.read(&mut chunk).await {
            Ok(0) | Err(_) => return,
            Ok(n) => {
                let text = String::from_utf8_lossy(&chunk[..n]);
                let mut b = buffers.lock().unwrap_or_else(PoisonError::into_inner);
                if is_stdout {
                    b.stdout.push_str(&text);
                }
                if b.combined.encode_utf16().count() < MAX_COMBINED_LEN {
                    b.combined.push_str(&text);
                }
            }
        }
    }
}

/// Spawn, drain both pipes (an undrained pipe could stall the child) and wait
/// for the exit. `Err` carries the spawn error message.
pub(crate) async fn run(
    pm: &dyn ProcessManager,
    cwd: &Path,
    command: &str,
    args: &[String],
) -> Result<Captured, String> {
    let child = pm
        .spawn(
            command,
            args,
            SpawnOptions {
                cwd: cwd.to_path_buf(),
                ..Default::default()
            },
        )
        .map_err(|e| e.to_string())?;
    let buffers = Mutex::new(Buffers::default());
    tokio::join!(
        pump(child.stdout, &buffers, true),
        pump(child.stderr, &buffers, false),
    );
    let outcome = child.exit.wait().await;
    let b = buffers.into_inner().unwrap_or_else(PoisonError::into_inner);
    Ok(Captured {
        stdout: b.stdout,
        combined: b.combined,
        code: outcome.code.unwrap_or(1),
    })
}

/// Exit code only; a spawn failure is exit code 1.
pub(crate) async fn run_for_code(
    pm: &dyn ProcessManager,
    cwd: &Path,
    command: &str,
    args: &[&str],
) -> i32 {
    let args: Vec<String> = args.iter().map(|a| (*a).to_owned()).collect();
    match run(pm, cwd, command, &args).await {
        Ok(captured) => captured.code,
        Err(error) => {
            tracing::debug!(command, %error, "spawn failed");
            1
        }
    }
}

#[cfg(test)]
mod tests;
