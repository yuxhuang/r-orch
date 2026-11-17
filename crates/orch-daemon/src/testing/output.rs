//! Process capture for scenarios whose real diagnostics bypass libtest's capture.

use std::os::unix::fs::{DirBuilderExt, PermissionsExt};

const CHILD: &str = "ORCH_TEST_OUTPUT_CAPTURE";

/// Run this exact test in a private process and capture both OS output streams.
///
/// Put `if capture_test_output("exact::test::name") { return; }` before the
/// scenario. The child runs the unchanged body, including its runtime, assertions
/// and cleanup. The parent waits/reaps it and includes both complete streams in
/// a failed assertion; successful direct writes never reach outer result lines.
/// This changes no process-global descriptor, environment or diagnostic sink.
pub(super) fn capture_test_output(name: &str) -> bool {
    if std::env::var_os(CHILD).as_deref() == Some(std::ffi::OsStr::new(name)) {
        return false;
    }
    let root = tempfile::Builder::new()
        .prefix("orch-test-output-")
        .tempdir_in("/tmp")
        .expect("private output-capture fixture");
    for directory in ["home", "tmp", "bin"] {
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(root.path().join(directory))
            .expect("private output-capture directory");
    }
    for tool in [
        "claude",
        "codex",
        "cursor",
        "cursor-agent",
        "agent",
        "opencode",
        "pi",
        "grok",
        "agy",
        "gemini",
    ] {
        let path = root.path().join("bin").join(tool);
        std::fs::write(
            &path,
            "#!/bin/sh\nprintf '%s\\n' \"$0 $*\" >> \"$HOME/ai-cli-calls\"\nexit 127\n",
        )
        .expect("AI blocker fixture");
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
            .expect("executable AI blocker fixture");
    }
    let config = root.path().join("global.yml");
    std::fs::write(&config, "{}\n").expect("private global configuration");
    // Reuse the serving daemon's gated constructor rather than introducing a
    // fourth process builder. A private runtime thread also works when the
    // caller is already inside a paused or current-thread Tokio test runtime.
    let output = std::thread::scope(|scope| {
        scope
            .spawn(|| {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("output-capture runtime");
                runtime.block_on(async {
                    let runner = orch_infra::models::SnapshotDiscoveryRuntime::new(
                        orch_infra::process::EnvSnapshot {
                            base: vec![
                                (CHILD.into(), name.into()),
                                ("HOME".into(), root.path().join("home").into_os_string()),
                                ("TMPDIR".into(), root.path().join("tmp").into_os_string()),
                                (
                                    "ORCHESTRY_GLOBAL_CONFIG_PATH".into(),
                                    config.into_os_string(),
                                ),
                                (
                                    "PATH".into(),
                                    format!("{}:/usr/bin:/bin", root.path().join("bin").display())
                                        .into(),
                                ),
                            ],
                            forced: Vec::new(),
                        },
                    );
                    let executable = std::env::current_exe().expect("test executable");
                    let output = runner
                        .output(
                            executable.to_str().expect("UTF-8 test executable"),
                            &["--exact", name, "--nocapture"],
                            root.path(),
                            std::time::Duration::from_secs(60),
                            // These are test diagnostics: retain every byte,
                            // including concurrent output and failed assertions.
                            usize::MAX,
                        )
                        .await;
                    // A timeout owns asynchronous kill/reap work. Keep its
                    // runtime alive until that owned work has really finished.
                    runner.spawn_work().wait_idle().await;
                    output
                })
            })
            .join()
            .expect("output-capture thread")
    })
    .expect("captured scenario process");
    let stdout = String::from_utf8_lossy(&output.stdout);
    // Read before TempDir cleanup: even a successful child must not hide a
    // private-HOME writer. Listing entries does not follow planted symlinks.
    let home_entries = std::fs::read_dir(root.path().join("home")).and_then(|entries| {
        entries
            .map(|entry| entry.map(|entry| entry.file_name()))
            .collect::<std::io::Result<Vec<_>>>()
    });
    let passed = output.status.success()
        && stdout
            .lines()
            .rev()
            .find(|line| line.starts_with("test result:"))
            .is_some_and(|line| {
                line.starts_with("test result: ok. 1 passed; 0 failed; 0 ignored;")
            })
        && home_entries
            .as_ref()
            .is_ok_and(|entries| entries.is_empty());
    if !passed {
        let retained = root.keep();
        panic!(
            "isolated test {name} failed ({}); private HOME entries: {home_entries:?}; fixture retained: {}\n{}{}",
            output.status,
            retained.display(),
            labelled_output("stdout", &output.stdout),
            labelled_output("stderr", &output.stderr)
        );
    }
    true
}

// Child libtest framing is data in the outer panic, including on a failed test.
// Invalid UTF-8 is represented as a complete byte list rather than discarded.
fn labelled_output(stream: &str, bytes: &[u8]) -> String {
    let text = match std::str::from_utf8(bytes) {
        Ok(text) => std::borrow::Cow::Borrowed(text),
        Err(_) => std::borrow::Cow::Owned(format!("{bytes:?}")),
    };
    text.split('\n')
        .map(|line| format!("child {stream} | {line}\n"))
        .collect()
}
