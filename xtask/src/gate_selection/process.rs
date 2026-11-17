//! Bounded read-only Git/metadata commands; no shell, hooks, builds or fetches.
// Compatibility exemption (ARCHITECTURE §4.8): F0 pins this slice to std,
// clap, anyhow and serde_json with no manifest changes. Unix sockets provide
// bounded nonblocking capture without a new process-runtime dependency.

use std::process::Command;
use std::time::Instant;

use anyhow::Result;

#[cfg(unix)]
pub(super) fn capture(mut command: Command, deadline: Instant) -> Result<Vec<u8>> {
    use std::io::{ErrorKind, Read};
    use std::os::fd::OwnedFd;
    use std::os::unix::net::UnixStream;
    use std::os::unix::process::CommandExt;
    use std::process::Stdio;
    use std::time::Duration;

    use anyhow::{Context, bail, ensure};

    ensure!(
        Instant::now() < deadline,
        "selector process deadline exceeded"
    );
    let (mut stdout, child_stdout) = UnixStream::pair()?;
    let (mut stderr, child_stderr) = UnixStream::pair()?;
    stdout.set_nonblocking(true)?;
    stderr.set_nonblocking(true)?;
    command
        .stdin(Stdio::null())
        .stdout(Stdio::from(OwnedFd::from(child_stdout)))
        .stderr(Stdio::from(OwnedFd::from(child_stderr)))
        .process_group(0);
    let mut child = Running {
        child: command.spawn().context("cannot start selector command")?,
        finished: false,
    };
    drop(command); // Close the parent's copies of the child's socket endpoints.
    let mut output = Vec::new();
    let mut stderr_count = 0usize;
    let mut stdout_closed = false;
    let mut stderr_closed = false;
    loop {
        ensure!(
            Instant::now() < deadline,
            "selector process deadline exceeded"
        );
        let mut buffer = [0u8; 8192];
        // A continuous writer cannot starve the deadline or the other stream.
        for _ in 0..32 {
            match stdout.read(&mut buffer) {
                Ok(0) => {
                    stdout_closed = true;
                    break;
                }
                Ok(count) => {
                    ensure!(
                        output.len() + count <= super::MAX_INPUT,
                        "selector stdout exceeds input bound"
                    );
                    output.extend_from_slice(&buffer[..count]);
                }
                Err(err) if err.kind() == ErrorKind::WouldBlock => break,
                Err(err) if err.kind() == ErrorKind::Interrupted => continue,
                Err(err) => return Err(err).context("cannot read selector stdout"),
            }
        }
        for _ in 0..32 {
            match stderr.read(&mut buffer) {
                Ok(0) => {
                    stderr_closed = true;
                    break;
                }
                Ok(count) => {
                    stderr_count += count;
                    ensure!(
                        stderr_count <= 64 * 1024,
                        "selector stderr exceeds input bound"
                    );
                }
                Err(err) if err.kind() == ErrorKind::WouldBlock => break,
                Err(err) if err.kind() == ErrorKind::Interrupted => continue,
                Err(err) => return Err(err).context("cannot read selector stderr"),
            }
        }
        // Do not reap the leader while inherited output sockets are open. Its
        // unreaped PID reserves the process-group identity for error cleanup.
        if stdout_closed
            && stderr_closed
            && let Some(status) = child.child.try_wait()?
        {
            child.finished = true;
            if !status.success() {
                // Raw tool diagnostics may contain paths or terminal commands.
                bail!("selector command failed ({status})");
            }
            return Ok(output);
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[cfg(unix)]
struct Running {
    child: std::process::Child,
    finished: bool,
}

#[cfg(unix)]
impl Drop for Running {
    fn drop(&mut self) {
        use std::process::Stdio;
        use std::time::Duration;

        if self.finished {
            return;
        }
        // The command is an unreaped leader of our own process group. Git is
        // invoked without hooks/external diffs, metadata without builds/network.
        // This is not a supervisor for arbitrary detached test processes.
        let group = format!("-{}", self.child.id());
        if let Ok(mut killer) = Command::new("/bin/kill")
            .args(["-KILL", "--", &group])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            let until = Instant::now() + Duration::from_secs(1);
            loop {
                match killer.try_wait() {
                    Ok(Some(_)) => break,
                    _ if Instant::now() < until => std::thread::sleep(Duration::from_millis(5)),
                    _ => {
                        let _ = killer.kill();
                        let reap_until = Instant::now() + Duration::from_secs(1);
                        while Instant::now() < reap_until {
                            if matches!(killer.try_wait(), Ok(Some(_))) {
                                break;
                            }
                            std::thread::sleep(Duration::from_millis(5));
                        }
                        break;
                    }
                }
            }
        }
        let _ = self.child.kill();
        let until = Instant::now() + Duration::from_secs(1);
        loop {
            match self.child.try_wait() {
                Ok(Some(_)) => break,
                _ if Instant::now() < until => std::thread::sleep(Duration::from_millis(5)),
                _ => break,
            }
        }
    }
}

#[cfg(not(unix))]
pub(super) fn capture(_command: Command, _deadline: Instant) -> Result<Vec<u8>> {
    anyhow::bail!("bounded selector commands require a Unix host")
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::time::Duration;

    fn shell(script: &str) -> Command {
        let mut command = Command::new("/bin/sh");
        command
            .env_clear()
            .env("HOME", "/nonexistent")
            .args(["-c", script]);
        command
    }

    #[test]
    fn capture_keeps_stdout_separate_and_refuses_failed_status() {
        let deadline = Instant::now() + Duration::from_secs(2);
        assert_eq!(
            capture(shell("printf 'a b\\n'; printf diagnostic >&2"), deadline).unwrap(),
            b"a b\n"
        );
        let error = capture(shell("printf data; exit 7"), deadline).unwrap_err();
        assert!(error.to_string().contains("command failed"));
    }

    #[test]
    fn an_unfinished_command_is_an_error_instead_of_partial_selection_input() {
        let error = capture(
            shell("printf partial; /bin/sleep 10"),
            Instant::now() + Duration::from_millis(100),
        )
        .unwrap_err();
        assert!(error.to_string().contains("deadline"));
    }
}
