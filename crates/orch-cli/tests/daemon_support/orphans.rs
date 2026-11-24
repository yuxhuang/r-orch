//! Linux-only SIGKILL backstop. Normal teardown still owns child reaping.
//! A pipe, rather than a parent PID poll, also handles PID reuse and zombies.
//! Python is already required by the Linux matrix's synthetic controls. The
//! standard library provides pidfds without adding unsafe code or a dependency.

use super::*;

pub struct Watchdog {
    child: Child,
    pipe: Option<std::process::ChildStdin>,
}

impl Watchdog {
    pub fn new(root: &Path, short: &Path) -> Self {
        let tmux = frontends::tmux_command().map(|cmd| PathBuf::from(cmd.get_program()));
        let mut cmd = Command::new("/usr/bin/python3");
        cmd.env_clear()
            .env("HOME", root.join("home"))
            .current_dir("/")
            .args(["-I", "-S", "-c", include_str!("orphan_watchdog.py")])
            .arg(root)
            .arg(short)
            .arg(env!("CARGO_BIN_EXE_orch"))
            .args(["/bin/sh", "/bin/sleep"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .process_group(0);
        if let Some(tmux) = tmux {
            cmd.arg(tmux);
        }
        let mut child = cmd.spawn().expect("launch Linux fixture watchdog");
        let pipe = child.stdin.take();
        let stdout = child.stdout.take().unwrap();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut line = String::new();
            let _ = BufReader::new(stdout).read_line(&mut line);
            let _ = tx.send(line);
        });
        let mut guard = Self { child, pipe };
        if rx.recv_timeout(CALL).as_deref() != Ok("ready\n") {
            let _ = guard.child.kill();
            panic!("Linux fixture watchdog requires Python pidfd support");
        }
        guard
    }
}

impl Drop for Watchdog {
    fn drop(&mut self) {
        self.pipe.take();
        let deadline = Instant::now() + Duration::from_secs(6);
        while matches!(self.child.try_wait(), Ok(None)) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        if matches!(self.child.try_wait(), Ok(None)) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

#[test]
fn watchdog_cleans_after_sigkill_without_signalling_foreign_fixtures() {
    let temp = tempfile::tempdir().unwrap();
    let mut cmd = Command::new("/usr/bin/python3");
    cmd.env_clear()
        .env("TMPDIR", temp.path())
        .env("HOME", temp.path())
        .args(["-I", "-S"])
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/daemon_support/orphan_watchdog_test.py"
        ))
        .process_group(0);
    let out = output_with_deadline(cmd, Duration::from_secs(60));
    assert!(out.status.success(), "{}", text(&out.stderr));
}
