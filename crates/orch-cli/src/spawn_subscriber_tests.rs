use super::*;
use orch_infra::models::{DiscoveryRuntime, SnapshotDiscoveryRuntime};
use orch_infra::process::gate::{SpawnObservation, SpawnObserver, SpawnSite, SpawnStage};
use std::io::{BufRead, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

const LIMIT: Duration = Duration::from_secs(20);
const CHILD: &str = "ORCH_TEST_PRODUCTION_SPAWN_SUBSCRIBER";

struct OwnedChild(std::process::Child);
impl std::ops::Deref for OwnedChild {
    type Target = std::process::Child;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
impl std::ops::DerefMut for OwnedChild {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}
impl Drop for OwnedChild {
    fn drop(&mut self) {
        // The unreaped Child handle retains our process identity.
        if !matches!(self.0.try_wait(), Ok(Some(_))) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

struct Park {
    entered: mpsc::Sender<()>,
    release: Mutex<Option<mpsc::Receiver<()>>>,
}
impl SpawnObserver for Park {
    fn observe(&self, event: SpawnObservation) {
        if event.stage == SpawnStage::Entered
            && let Some(release) = self
                .release
                .lock()
                .expect("isolated spawn subscriber fixture")
                .take()
        {
            let _ = self.entered.send(());
            let _ = release.recv_timeout(3 * LIMIT);
        }
    }
}

#[test]
fn production_subscriber_child() {
    let Some(root) = std::env::var_os(CHILD) else {
        return;
    };
    let root = std::path::Path::new(&root);
    let args: Vec<String> = serde_json::from_str(
        &std::env::var("ORCH_TEST_TRACE_ARGS").expect("isolated spawn subscriber fixture"),
    )
    .expect("isolated spawn subscriber fixture");
    let cli = Cli::try_parse_from(args).expect("isolated spawn subscriber fixture");
    // Exactly the production initializer used by dispatch_parsed, before
    // anything can capture a subscriber. No replacement filter/subscriber.
    init_tracing(cli.command.as_ref());
    if std::env::var_os("ORCH_TEST_DETACHED_LOG").is_some() {
        commands::daemon::with_test_detached_stderr(root, || parked(root));
    } else {
        parked(root);
    }
}

fn parked(root: &std::path::Path) {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("isolated spawn subscriber fixture");
    let discovery = Arc::new(SnapshotDiscoveryRuntime::new(
        orch_infra::process::EnvSnapshot {
            base: vec![
                ("HOME".into(), root.join("home").into_os_string()),
                ("PATH".into(), root.join("bin").into_os_string()),
            ],
            forced: vec![],
        },
    ));
    let (entered, did_enter) = mpsc::channel();
    let (release, released) = mpsc::channel();
    discovery.set_spawn_observer(Arc::new(Park {
        entered,
        release: Mutex::new(Some(released)),
    }));
    struct Release(Option<mpsc::Sender<()>>);
    impl Drop for Release {
        fn drop(&mut self) {
            if let Some(tx) = self.0.take() {
                let _ = tx.send(());
            }
        }
    }
    let release = Release(Some(release));
    let diagnostic = runtime.spawn(async move {
        discovery
            .run_command("secret-program", &["sensitive-argument"])
            .await
    });
    did_enter
        .recv_timeout(LIMIT)
        .expect("isolated spawn subscriber fixture");
    let (waiting, did_wait) = mpsc::channel();
    let queued = runtime.spawn(async move {
        // Poll once to register the real waiting observation before the
        // parent starts looking for warnings. The permit remains held.
        let admission = orch_infra::process::gate::reserve_spawn(SpawnSite::Version);
        tokio::pin!(admission);
        assert!(futures::poll!(&mut admission).is_pending());
        waiting.send(()).expect("isolated spawn subscriber fixture");
        admission.await
    });
    did_wait
        .recv_timeout(LIMIT)
        .expect("isolated spawn subscriber fixture");
    tracing::warn!(target: "unrelated-target", "UNRELATED_PAYLOAD");
    tracing::warn!(target: "orch_infra::process::spawn::other", "PREFIX_PAYLOAD");
    tracing::debug!(target: "rmcp::service", "SDK_SECRET_DEBUG");
    tracing::trace!(target: "rmcp::service::client", "SDK_SECRET_TRACE");
    println!("PARKED");
    std::io::stdout()
        .flush()
        .expect("isolated spawn subscriber fixture");
    let mut line = String::new();
    std::io::stdin()
        .read_line(&mut line)
        .expect("isolated spawn subscriber fixture");
    queued.abort();
    drop(release);
    runtime.block_on(async {
        let _ = queued.await;
        assert!(
            diagnostic
                .await
                .expect("isolated spawn subscriber fixture")
                .is_ok()
        );
    });
    assert!(orch_infra::process::gate::pending_spawns().is_empty());
    println!("DRAINED");
}

fn check(args: &[&str], extra: &[(&str, &str)], enabled: bool, narrow: bool, detached: bool) {
    let root = tempfile::tempdir().expect("isolated spawn subscriber fixture");
    for dir in [
        root.path().join("home"),
        root.path().join("bin"),
        root.path().join(".orchestry"),
    ] {
        std::fs::create_dir_all(dir).expect("isolated spawn subscriber fixture");
    }
    let script = root.path().join("bin/secret-program");
    std::fs::write(&script, "#!/bin/sh\nprintf 'SENSITIVE_CHILD_OUTPUT'\n")
        .expect("isolated spawn subscriber fixture");
    std::fs::set_permissions(script, std::fs::Permissions::from_mode(0o700))
        .expect("isolated spawn subscriber fixture");
    let stderr_path = root.path().join(if detached {
        ".orchestry/daemon.log"
    } else {
        "stderr"
    });
    let stderr = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(&stderr_path)
        .expect("isolated spawn subscriber fixture");
    let mut command = std::process::Command::new(
        std::env::current_exe().expect("isolated spawn subscriber fixture"),
    );
    command
        .args([
            "--exact",
            "spawn_subscriber_tests::production_subscriber_child",
            "--nocapture",
        ])
        .env_clear()
        .env(CHILD, root.path())
        .env(
            "ORCH_TEST_TRACE_ARGS",
            serde_json::to_string(args).expect("isolated spawn subscriber fixture"),
        )
        .env("HOME", root.path().join("home"))
        .env("PATH", root.path().join("bin"))
        .env(
            "ORCHESTRY_GLOBAL_CONFIG_PATH",
            root.path().join("home/global.yml"),
        )
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(stderr);
    for (key, value) in extra {
        command.env(key, value);
    }
    if detached {
        command.env("ORCH_TEST_DETACHED_LOG", "1");
    }
    let mut child = OwnedChild(command.spawn().expect("isolated spawn subscriber fixture"));
    let (send, receive) = mpsc::channel();
    let stdout = child
        .stdout
        .take()
        .expect("isolated spawn subscriber fixture");
    let reader = std::thread::spawn(move || {
        for line in std::io::BufReader::new(stdout).lines() {
            let line = line.expect("isolated spawn subscriber fixture");
            let _ = send.send(line);
        }
    });
    let deadline = Instant::now() + LIMIT;
    let mut parked = false;
    while Instant::now() < deadline {
        if let Ok(line) = receive.recv_timeout(Duration::from_millis(100))
            && line == "PARKED"
        {
            parked = true;
            break;
        }
        if child
            .try_wait()
            .expect("isolated spawn subscriber fixture")
            .is_some()
        {
            break;
        }
    }
    let observed = Instant::now();
    let mut log = String::new();
    while parked && Instant::now() < deadline {
        log = std::fs::read_to_string(&stderr_path).expect("isolated spawn subscriber fixture");
        if enabled && log.contains("Waiting") && log.contains("Entered") {
            break;
        }
        if !enabled && observed.elapsed() >= Duration::from_millis(1500) {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    // Release/drain even when the saved parked-stage assertions are RED.
    let _ = child
        .stdin
        .take()
        .expect("isolated spawn subscriber fixture")
        .write_all(b"release\n");
    let drain_deadline = Instant::now() + LIMIT;
    while Instant::now() < drain_deadline
        && child
            .try_wait()
            .expect("isolated spawn subscriber fixture")
            .is_none()
    {
        std::thread::sleep(Duration::from_millis(10));
    }
    let watchdog = child
        .try_wait()
        .expect("isolated spawn subscriber fixture")
        .is_none();
    if watchdog {
        let _ = child.kill();
    }
    let status = child.wait().expect("isolated spawn subscriber fixture");
    reader.join().expect("isolated spawn subscriber fixture");
    let lines: Vec<_> = receive.try_iter().collect();
    assert!(
        !watchdog && status.success(),
        "child failed: {status}; {log}"
    );
    assert!(parked && lines.iter().any(|line| line == "DRAINED"));
    assert_eq!(
        log.contains("Waiting") && log.contains("Entered"),
        enabled,
        "production Waiting/Entered warnings missing before diagnostic release: {args:?} {extra:?}: {log}"
    );
    if narrow {
        assert!(
            !log.contains("UNRELATED_PAYLOAD") && !log.contains("PREFIX_PAYLOAD"),
            "{log}"
        );
    }
    for secret in [
        "secret-program",
        "sensitive-argument",
        "SENSITIVE_CHILD_OUTPUT",
        "SDK_SECRET_DEBUG",
        "SDK_SECRET_TRACE",
    ] {
        assert!(
            !log.contains(secret),
            "sensitive marker {secret} leaked: {log}"
        );
    }
    for line in log.lines().filter(|line| line.contains("spawn_id=")) {
        for field in ["site=", "stage=", "elapsed_ms=", "overflow="] {
            assert!(line.contains(field), "{line}");
        }
    }
    assert!(
        std::fs::metadata(stderr_path)
            .expect("isolated spawn subscriber fixture")
            .len()
            <= 10 * 1024 * 1024
    );
}

#[test]
fn real_default_subscriber_routes_only_parsed_owner_starts() {
    for args in [
        vec!["orch", "daemon", "start"],
        vec!["orch", "daemon", "start", "--detach"],
        vec!["orch", "serve"],
        vec!["orch", "run", "--watch"],
    ] {
        check(&args, &[], true, true, args.contains(&"--detach"));
    }
    // Detached re-exec removes --detach, but retains the parsed start.
    check(
        &["orch", "daemon", "start"],
        &[("ORCH_DAEMON_DETACHED_CHILD", "1")],
        true,
        true,
        true,
    );
    for args in [
        vec!["orch", "status"],
        vec!["orch", "mcp"],
        vec!["orch", "daemon", "status"],
        vec!["orch", "daemon", "stop"],
        vec!["orch", "serve", "--once"],
        vec!["orch", "run", "--once"],
    ] {
        check(&args, &[], false, true, false);
    }
}

#[test]
fn explicit_filters_and_independent_sdk_exclusion_survive() {
    for (extra, enabled) in [
        (vec![("RUST_LOG", "off")], false),
        (vec![("RUST_LOG", "")], false),
        (vec![("RUST_LOG", "warn")], true),
        (vec![("ORCHESTRY_DEBUG", "1")], true),
        (vec![("RUST_LOG", "trace,rmcp::service=trace")], true),
        (
            vec![("RUST_LOG", "debug,rmcp::service::client=trace")],
            true,
        ),
    ] {
        check(&["orch", "daemon", "start"], &extra, enabled, false, false);
    }
}
