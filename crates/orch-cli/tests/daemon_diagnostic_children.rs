//! Diagnostic children use the daemon snapshot; their output stays off logs.
mod daemon_support;

use daemon_support::{CALL, DaemonChild, Sandbox, output_with_deadline, text};
use serde_json::{Value, json};
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::os::unix::net::UnixStream;
use std::path::Path;

const CANARY: &str = "DETACHED_STDERR_CANARY";
const DENIED: &[&str] = &[
    "DEBUG",
    "PWDEBUG",
    "NODE_OPTIONS",
    "ORCH_WEB_PASSWORD",
    "ORCH_AGENT_ID",
    "ORCH_AGENT_NAME",
    "ORCH_TASK_ID",
    "ORCH_TUI_CLIENT",
    "ORCH_DAEMON_DETACHED_CHILD",
];

fn script(path: &Path, body: &str) {
    fs::write(path, format!("#!/bin/sh\n{body}\n")).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
}

struct Client(BufReader<UnixStream>);
impl Client {
    fn open(sandbox: &Sandbox) -> Self {
        let socket = UnixStream::connect(sandbox.socket()).unwrap();
        socket.set_read_timeout(Some(CALL)).unwrap();
        let mut client = Self(BufReader::new(socket));
        let state = fs::metadata(sandbox.state_dir()).unwrap();
        client.send(json!({"hello": {
            "protocol": orch_proto::LOCAL_PROTOCOL,
            "binary": orch_proto::BINARY_VERSION,
            "project_root": {"utf8": sandbox.root()},
            "project_id": {"dev": state.dev(), "ino": state.ino()},
            "caller": {"kind": "cli"}, "mode": "normal"
        }}));
        assert_eq!(client.read()["welcome"]["compatible"], true);
        client
    }
    fn send(&mut self, value: Value) {
        writeln!(self.0.get_mut(), "{value}").unwrap();
    }
    fn read(&mut self) -> Value {
        let mut line = String::new();
        assert_ne!(self.0.read_line(&mut line).unwrap(), 0);
        serde_json::from_str(&line).unwrap()
    }
    fn request(&mut self, id: u64, op: &str, args: Value) -> Value {
        self.send(json!({"id": id, "req": {"op": op, "args": args}}));
        loop {
            let reply = self.read();
            if reply["id"] == id && (reply.get("ok").is_some() || reply.get("err").is_some()) {
                assert!(reply.get("ok").is_some(), "{reply}");
                return reply["ok"].clone();
            }
        }
    }
}

#[test]
fn catalog_children_use_the_sanitised_environment() {
    diagnostic_environment(false);
}

#[test]
fn doctor_children_use_the_sanitised_environment() {
    diagnostic_environment(true);
}

fn diagnostic_environment(doctor: bool) {
    let sandbox = Sandbox::new();
    sandbox.init_project();
    let bin = sandbox.root().join("bin");
    fs::create_dir(&bin).unwrap();
    // Every diagnostic executable is a stand-in, including git and bash.
    for name in [
        "claude",
        "codex",
        "cursor-agent",
        "grok",
        "agy",
        "opencode",
        "pi",
        "bash",
        "git",
    ] {
        script(
            &bin.join(name),
            &format!(
                "/usr/bin/env > '{}/{name}-env'\nprintf 'diagnostic-ok\\n'",
                sandbox.root().display()
            ),
        );
    }
    let mut command = sandbox.start_cmd(&["--from-agent"]);
    command
        .env("PATH", &bin)
        .env("KEPT_OWNER_VALUE", "retained");
    for name in DENIED
        .iter()
        .filter(|name| **name != "ORCH_DAEMON_DETACHED_CHILD")
    {
        command.env(
            name,
            if *name == "NODE_OPTIONS" {
                "--inspect=127.0.0.1:9229"
            } else {
                CANARY
            },
        );
    }
    let mut daemon = DaemonChild::spawn(command, sandbox.root().join("stderr"));
    daemon.expect_ready(&sandbox.socket());
    let mut client = Client::open(&sandbox);
    if doctor {
        client.request(1, "doctor.run", json!({}));
        for name in [
            "git",
            "claude",
            "codex",
            "cursor-agent",
            "grok",
            "agy",
            "opencode",
            "pi",
            "bash",
        ] {
            assert_environment(&sandbox.root().join(format!("{name}-env")));
        }
    } else {
        client.request(1, "models.catalog", json!({"adapters": ["opencode"]}));
        assert_environment(&sandbox.root().join("opencode-env"));
    }
    drop(client);
    assert!(sandbox.run(&["daemon", "stop"]).status.success());
    assert!(daemon.wait_exit(CALL).success());
}

fn assert_environment(path: &Path) {
    let env = fs::read_to_string(path).unwrap();
    for name in DENIED {
        assert!(
            !env.lines()
                .any(|line| line.starts_with(&format!("{name}="))),
            "{path:?}: leaked {name}"
        );
    }
    assert!(env.contains("KEPT_OWNER_VALUE=retained"), "{env}");
    let root = path.parent().unwrap();
    assert!(
        env.lines()
            .any(|line| line == format!("ORCH_PROJECT_ROOT={}", root.display())),
        "trusted project routing missing"
    );
    assert!(
        env.lines().any(|line| line.starts_with("ORCH_SOCKET=")),
        "trusted socket routing missing"
    );
}

#[test]
fn detached_diagnostic_stderr_never_reaches_daemon_log() {
    let sandbox = Sandbox::new();
    sandbox.init_project();
    let bin = sandbox.root().join("bin");
    fs::create_dir(&bin).unwrap();
    script(
        &bin.join("opencode"),
        &format!("printf {CANARY} >&2\nexit 2"),
    );
    let mut command = sandbox.start_cmd(&["--detach"]);
    command.env("PATH", &bin).env("RUST_LOG", "trace");
    let started = output_with_deadline(command, CALL);
    assert!(started.status.success(), "{}", text(&started.stderr));
    // Own cleanup even if an assertion fails after detached READY.
    struct Stop<'a>(&'a Sandbox);
    impl Drop for Stop<'_> {
        fn drop(&mut self) {
            let _ = self.0.run(&["daemon", "stop"]);
        }
    }
    let _stop = Stop(&sandbox);
    let mut client = Client::open(&sandbox);
    client.request(1, "models.catalog", json!({"adapters": ["opencode"]}));
    // Reuse the reviewer's 12 MiB noisy-stderr vector.
    script(
        &bin.join("opencode"),
        &format!("/usr/bin/yes {CANARY} | /usr/bin/head -c 12582912 >&2\nexit 2"),
    );
    client.request(2, "models.catalog", json!({"adapters": ["opencode"]}));
    client.request(3, "doctor.run", json!({}));
    drop(client);
    let log = fs::read_to_string(sandbox.state_dir().join("daemon.log")).unwrap();
    assert!(
        !log.contains(CANARY),
        "diagnostic stderr reached daemon.log"
    );
    assert!(log.len() < 64 * 1024, "unexpected log size: {}", log.len());
}
