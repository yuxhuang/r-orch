//! The NO-SPAWN PROOF (T1b; spec R3/R15 "clients never start or repair a
//! daemon"; goal row 3). In PR 1 the only clients are `orch daemon status`,
//! `stop` and `dispatch on|off`. For every client-side failure condition
//! (absent socket, stale socket inode, regular file in the way, stale
//! `daemon.json`, protocol skew) each client call must
//!
//! 1. exit 7 with the uniform text,
//! 2. create no process: the process table before/after has no new `orch`
//!    process, no new process in the client's group or child list, and the
//!    client's process group is empty (`killpg(pgid, 0)` → ESRCH),
//! 3. repair nothing: the socket inode, the stale `daemon.json` and the whole
//!    project tree are identical afterwards, and no listener appeared.
//!
//! Serialised: cargo runs test binaries one after another and this binary's
//! cases take a lock, so no other `orch` of this binary is alive while a case
//! compares process tables.

mod daemon_support;

use daemon_support::*;
use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::os::unix::net::UnixListener;
use std::path::Path;
use std::sync::Mutex;

static SERIAL: Mutex<()> = Mutex::new(());

fn serial() -> std::sync::MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(|poison| poison.into_inner())
}

/// `(relative path → (is_dir, size))` of a tree; mtimes are excluded because
/// a live harness daemon may legitimately touch its own files.
fn tree(root: &Path) -> BTreeMap<String, (bool, u64)> {
    fn walk(base: &Path, dir: &Path, out: &mut BTreeMap<String, (bool, u64)>) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let meta = fs::symlink_metadata(&path).unwrap();
            let rel = path
                .strip_prefix(base)
                .unwrap()
                .to_string_lossy()
                .into_owned();
            out.insert(
                rel,
                (meta.is_dir(), if meta.is_dir() { 0 } else { meta.len() }),
            );
            if meta.is_dir() {
                walk(base, &path, out);
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(root, root, &mut out);
    out
}

/// Identity and mode of whatever sits at `socket` (None when absent).
fn endpoint_identity(socket: &Path) -> Option<(u64, u32, u64, i64)> {
    let meta = fs::symlink_metadata(socket).ok()?;
    Some((meta.ino(), meta.mode(), meta.len(), meta.mtime()))
}

struct World<'a> {
    root: &'a Path,
    socket: &'a Path,
    test_root: &'a Path,
}

impl World<'_> {
    /// Run one client call and apply the whole no-spawn checklist.
    fn client_fails_without_spawning(&self, args: &[&str], extra_env: &[(&str, &str)]) {
        let tree_before = tree(self.root);
        let socket_before = endpoint_identity(self.socket);
        let sock_dir_before = tree(self.socket.parent().unwrap());
        let before = process_table();
        let known: HashSet<u32> = before.iter().map(|p| p.pid).collect();

        let mut cmd = orch_command(self.root, self.socket, self.test_root);
        cmd.args(args);
        for (key, value) in extra_env {
            cmd.env(key, value);
        }
        let (out, client_pid) = output_and_pid(cmd, CALL);

        assert_eq!(
            out.status.code(),
            Some(7),
            "{args:?}: {}",
            text(&out.stderr)
        );
        assert!(
            text(&out.stderr).contains("No orch daemon is running for")
                || text(&out.stderr).contains("not running")
                || text(&out.stderr).contains("protocol"),
            "{args:?}: {}",
            text(&out.stderr)
        );

        // (2) process table.
        let after = process_table();
        let markers = [
            self.root.to_string_lossy().into_owned(),
            self.test_root.to_string_lossy().into_owned(),
        ];
        let exe = env!("CARGO_BIN_EXE_orch");
        let offenders: Vec<_> = after
            .iter()
            .filter(|p| !known.contains(&p.pid) && p.pid != client_pid)
            .filter(|p| {
                p.command == exe
                    || p.command.starts_with(&format!("{exe} "))
                    || p.pgid == client_pid
                    || p.ppid == client_pid
                    || markers.iter().any(|m| p.command.contains(m.as_str()))
            })
            .collect();
        assert!(offenders.is_empty(), "{args:?} spawned {offenders:?}");
        assert!(
            process_group_empty(client_pid),
            "{args:?} left a process in the client's group"
        );
        assert!(
            orch_processes(&after)
                .iter()
                .all(|p| known.contains(&p.pid)),
            "{args:?}: a new orch process exists"
        );

        // (3) nothing repaired or created.
        assert_eq!(
            endpoint_identity(self.socket),
            socket_before,
            "{args:?} touched the endpoint"
        );
        assert_eq!(
            tree(self.root),
            tree_before,
            "{args:?} changed the project tree"
        );
        assert_eq!(
            tree(self.socket.parent().unwrap()),
            sock_dir_before,
            "{args:?} changed the socket directory"
        );
    }
}

const CLIENTS: [&[&str]; 4] = [
    &["daemon", "status"],
    &["daemon", "stop"],
    &["daemon", "dispatch", "on"],
    &["daemon", "dispatch", "off"],
];

fn every_client(world: &World<'_>) {
    for args in CLIENTS {
        world.client_fails_without_spawning(args, &[]);
    }
    // The agent-context rendering takes the same path.
    world.client_fails_without_spawning(CLIENTS[0], &[("ORCH_TASK_ID", "t-1")]);
}

fn world(sandbox: &Sandbox) -> (std::path::PathBuf, std::path::PathBuf) {
    (sandbox.root(), sandbox.socket())
}

#[test]
fn absent_socket_spawns_nothing_and_creates_nothing() {
    let _serial = serial();
    let sandbox = Sandbox::new();
    sandbox.init_project();
    let (root, socket) = world(&sandbox);
    every_client(&World {
        root: &root,
        socket: &socket,
        test_root: sandbox.short_dir(),
    });
    assert!(!socket.exists());
    assert!(!root.join(".orchestry/daemon.json").exists());
    assert!(!root.join(".orchestry/.daemon-owner.lock").exists());
    assert!(!root.join(".orchestry/orchestry.lock").exists());
}

#[test]
fn stale_socket_inode_is_neither_started_over_nor_repaired() {
    let _serial = serial();
    let sandbox = Sandbox::new();
    sandbox.init_project();
    let (root, socket) = world(&sandbox);
    drop(UnixListener::bind(&socket).unwrap());
    assert!(socket.exists());
    every_client(&World {
        root: &root,
        socket: &socket,
        test_root: sandbox.short_dir(),
    });
    assert!(socket.exists(), "a client unlinked the stale socket");
}

#[test]
fn regular_file_at_the_socket_path_is_untouched() {
    let _serial = serial();
    let sandbox = Sandbox::new();
    sandbox.init_project();
    let (root, socket) = world(&sandbox);
    fs::write(&socket, "not a socket").unwrap();
    every_client(&World {
        root: &root,
        socket: &socket,
        test_root: sandbox.short_dir(),
    });
    assert_eq!(fs::read_to_string(&socket).unwrap(), "not a socket");
}

#[test]
fn stale_daemon_json_of_a_dead_owner_is_untouched() {
    let _serial = serial();
    let sandbox = Sandbox::new();
    sandbox.init_project();
    let (root, socket) = world(&sandbox);
    // A real daemon's discovery file, preserved past its (killed) owner.
    let mut owner = start_ready(&sandbox, &[], "owner");
    let discovery = fs::read(root.join(".orchestry/daemon.json")).unwrap();
    owner.crash();
    drop(fs::remove_file(&socket));
    assert_eq!(
        fs::read(root.join(".orchestry/daemon.json")).unwrap(),
        discovery
    );
    every_client(&World {
        root: &root,
        socket: &socket,
        test_root: sandbox.short_dir(),
    });
    assert_eq!(
        fs::read(root.join(".orchestry/daemon.json")).unwrap(),
        discovery,
        "a client rewrote or removed daemon.json"
    );
    assert!(!socket.exists(), "a client re-created the socket");
}

#[test]
fn skewed_live_daemon_gets_the_skew_text_and_no_child() {
    let _serial = serial();
    // The harness daemon lives on its own runtime; the clients are blocking
    // child processes driven from this thread.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let daemon = runtime
        .block_on(
            orch_daemon::testing::DaemonHarness::builder()
                .on_socket()
                .versions(orch_proto::LOCAL_PROTOCOL + 1, orch_proto::BINARY_VERSION)
                .start(),
        )
        .unwrap();
    let root = daemon.project_root().to_path_buf();
    let socket = daemon.socket().unwrap().to_path_buf();
    let test_root = socket.parent().unwrap().to_path_buf();
    // `stop` deliberately crosses skew (frozen stop intent) and is covered in
    // daemon_cmd_control.rs; the other clients must refuse without a child.
    let world = World {
        root: &root,
        socket: &socket,
        test_root: &test_root,
    };
    for args in [CLIENTS[0], CLIENTS[2], CLIENTS[3]] {
        world.client_fails_without_spawning(args, &[]);
    }
    let mut cmd = orch_command(&root, &socket, &test_root);
    cmd.args(["daemon", "status"]);
    let skew_text = text(&output_with_deadline(cmd, CALL).stderr);
    assert!(skew_text.contains("orch daemon stop"), "{skew_text}");
    assert!(
        skew_text.contains(orch_proto::BINARY_VERSION),
        "{skew_text}"
    );
    assert!(
        skew_text.contains(&(orch_proto::LOCAL_PROTOCOL + 1).to_string()),
        "{skew_text}"
    );
    runtime.block_on(daemon.shutdown()).unwrap();
}

#[test]
fn refused_starts_spawn_nothing() {
    let _serial = serial();
    let sandbox = Sandbox::new();
    sandbox.init_project();
    let tree_before = tree(&sandbox.root());
    let before = process_table();
    let known: HashSet<u32> = before.iter().map(|p| p.pid).collect();

    let mut pids = Vec::new();
    type Case<'a> = (&'a str, Vec<(&'a str, &'a str)>, Vec<&'a str>);
    let cases: [Case<'_>; 2] = [
        (
            "agent task id",
            vec![("ORCH_TASK_ID", "t-1")],
            vec!["daemon", "start"],
        ),
        (
            "agent id detached",
            vec![("ORCH_AGENT_ID", "a-1")],
            vec!["daemon", "start", "--detach"],
        ),
    ];
    for (name, env, args) in cases {
        let mut cmd = sandbox.orch();
        cmd.args(&args);
        for (key, value) in env {
            cmd.env(key, value);
        }
        let (out, pid) = output_and_pid(cmd, CALL);
        assert_eq!(out.status.code(), Some(2), "{name}: {}", text(&out.stderr));
        pids.push(pid);
    }
    let after = process_table();
    let exe = env!("CARGO_BIN_EXE_orch");
    let spawned: Vec<_> = after
        .iter()
        .filter(|p| !known.contains(&p.pid) && !pids.contains(&p.pid))
        .filter(|p| p.command.starts_with(exe) || pids.contains(&p.pgid))
        .collect();
    assert!(spawned.is_empty(), "{spawned:?}");
    assert!(pids.iter().all(|pid| process_group_empty(*pid)));
    assert_eq!(tree(&sandbox.root()), tree_before);
    assert!(!sandbox.socket().exists());
}

#[test]
fn init_prints_the_hint_and_spawns_and_binds_nothing() {
    let _serial = serial();
    let sandbox = Sandbox::new();
    let before = process_table();
    let known: HashSet<u32> = before.iter().map(|p| p.pid).collect();
    let (out, pid) = output_and_pid(
        {
            let mut cmd = sandbox.orch();
            cmd.args(["init", "--name", "proj", "--adapter", "shell"]);
            cmd
        },
        CALL,
    );
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert!(text(&out.stdout).contains("Next: orch daemon start"));
    let after = process_table();
    assert!(
        orch_processes(&after)
            .iter()
            .all(|p| known.contains(&p.pid)),
        "init started an orch process"
    );
    assert!(process_group_empty(pid));
    assert!(!sandbox.socket().exists());
    assert!(!sandbox.state_dir().join("daemon.json").exists());
    assert!(!sandbox.state_dir().join(".daemon-owner.lock").exists());
    assert!(!sandbox.state_dir().join("orchestry.lock").exists());
}

/// Positive control: the detector used above does flag a real daemon, so a
/// green no-spawn result cannot be an artefact of a blind process filter.
#[test]
fn detector_flags_a_daemon_that_is_started() {
    let _serial = serial();
    let sandbox = Sandbox::new();
    sandbox.init_project();
    let known: HashSet<u32> = process_table().iter().map(|p| p.pid).collect();
    let daemon = start_ready(&sandbox, &[], "control");
    let after = process_table();
    let flagged: Vec<_> = orch_processes(&after)
        .into_iter()
        .filter(|p| !known.contains(&p.pid))
        .collect();
    assert_eq!(flagged.len(), 1, "{flagged:?}");
    assert_eq!(flagged[0].pid, daemon.pid);
    assert_eq!(flagged[0].pgid, daemon.pid, "own process group");
    assert!(!process_group_empty(daemon.pid));
}
