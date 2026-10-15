//! Real, isolated installations exercising design rev 2 §7. Run with:
//! `cargo test -p orch-cli --test remote_peers -- --test-threads=1`.
//!
//! No AI CLI, user configuration, fixed worker port, or network service is
//! involved. All waits have deadlines. Owners and adapter process groups are
//! killed on assertion failure as well as success.
#![cfg(unix)]

use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;
use serde_json::{Value, json};

const WAIT: Duration = Duration::from_secs(25);
const POLL: Duration = Duration::from_millis(40);
const TICK_MS: u64 = 100;

fn read_json(path: impl AsRef<Path>) -> Value {
    fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or(Value::Null)
}

fn text(path: impl AsRef<Path>) -> String {
    fs::read_to_string(path).unwrap_or_default()
}

fn quote(path: &Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', "'\\''"))
}

fn signal_group(pid: u32, signal: Signal) {
    if let Ok(pid) = i32::try_from(pid)
        && pid > 1
    {
        let _ = kill(Pid::from_raw(-pid), signal);
    }
}

struct Process {
    child: Child,
    stdout: PathBuf,
    stderr: PathBuf,
}

impl Process {
    fn spawn(mut command: Command, stdout: PathBuf, stderr: PathBuf) -> Self {
        command
            .process_group(0)
            .stdin(Stdio::null())
            .stdout(File::create(&stdout).expect("create stdout capture"))
            .stderr(File::create(&stderr).expect("create stderr capture"));
        Self {
            child: command.spawn().expect("spawn isolated child"),
            stdout,
            stderr,
        }
    }

    fn running(&mut self) -> bool {
        self.child.try_wait().expect("poll child").is_none()
    }

    fn terminate(&mut self, signal: Signal) {
        signal_group(self.child.id(), signal);
        let deadline = Instant::now() + Duration::from_secs(5);
        while self.running() && Instant::now() < deadline {
            thread::sleep(POLL);
        }
        // Always sweep the group, even if its leader exited before a child.
        signal_group(self.child.id(), Signal::SIGKILL);
        let _ = self.child.kill();
        let _ = self.child.wait();
    }

    fn output(mut self, limit: Duration) -> Output {
        let deadline = Instant::now() + limit;
        let status = loop {
            if let Some(status) = self.child.try_wait().expect("poll command") {
                break status;
            }
            assert!(
                Instant::now() < deadline,
                "command timed out\nstdout: {}\nstderr: {}",
                text(&self.stdout),
                text(&self.stderr)
            );
            thread::sleep(POLL);
        };
        Output {
            status,
            stdout: fs::read(&self.stdout).unwrap_or_default(),
            stderr: fs::read(&self.stderr).unwrap_or_default(),
        }
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        self.terminate(Signal::SIGKILL);
    }
}

struct Peer {
    dir: tempfile::TempDir,
    name: String,
    os: String,
    home: PathBuf,
    bin: PathBuf,
    serial: Cell<usize>,
    owner: RefCell<Option<Process>>,
}

impl Peer {
    fn new(name: &str) -> Self {
        let dir = tempfile::Builder::new()
            .prefix("orch-peer-e2e-")
            .tempdir()
            .expect("sandbox project");
        let home = dir.path().join("test-home");
        let bin = home.join("bin");
        fs::create_dir_all(&bin).expect("sandbox home/bin");
        let peer = Self {
            dir,
            name: name.into(),
            os: std::env::consts::OS.into(),
            home,
            bin,
            serial: Cell::new(0),
            owner: RefCell::new(None),
        };
        peer.ok(&["init", "--name", name, "--adapter", "shell"]);
        peer.config("shared", 30_000);
        peer
    }

    fn path(&self, relative: &str) -> PathBuf {
        self.dir.path().join(relative)
    }

    fn config(&self, workspace: &str, stall_ms: u64) {
        let mut config = serde_json::to_value(orch_domain::default_config()).unwrap();
        config["project"]["name"] = json!(self.name);
        config["defaults"]["agent"]["adapter"] = json!("shell");
        config["defaults"]["agent"]["workspace_mode"] = json!(workspace);
        config["defaults"]["agent"]["stall_timeout_ms"] = json!(stall_ms);
        config["defaults"]["agent"]["timeout_ms"] = json!(60_000);
        config["scheduling"]["poll_interval_ms"] = json!(TICK_MS);
        config["scheduling"]["retry_base_delay_ms"] = json!(100);
        // JSON is a YAML subset; avoid a test-only YAML dependency.
        fs::write(
            self.path(".orchestry/config.yml"),
            serde_json::to_vec_pretty(&config).unwrap(),
        )
        .expect("fixture config");
    }

    fn command(&self, program: impl AsRef<std::ffi::OsStr>) -> Command {
        let mut command = Command::new(program);
        // The test runner can itself be an orchestrated agent. Do not inherit
        // its identity, config redirects, hooks or task environment.
        for (key, _) in std::env::vars_os() {
            let name = key.to_string_lossy();
            if name.starts_with("ORCH") || name.starts_with("GIT_") {
                command.env_remove(key);
            }
        }
        let mut paths = vec![self.bin.clone()];
        paths.extend(std::env::split_paths(
            &std::env::var_os("PATH").unwrap_or_default(),
        ));
        command
            .current_dir(self.dir.path())
            .env("HOME", &self.home)
            .env("XDG_CONFIG_HOME", self.home.join(".config"))
            .env("ORCHESTRY_GLOBAL_CONFIG_PATH", self.home.join("global.yml"))
            .env("ORCH_TEST_OS_CAP", &self.os)
            .env("ORCH_TEST_PEER_BLOCK_FILE", self.path("peer-block"))
            .env("PATH", std::env::join_paths(paths).unwrap())
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_AUTHOR_NAME", "Remote peers test")
            .env("GIT_AUTHOR_EMAIL", "peer-test@example.invalid")
            .env("GIT_COMMITTER_NAME", "Remote peers test")
            .env("GIT_COMMITTER_EMAIL", "peer-test@example.invalid")
            .env("NO_COLOR", "1")
            .env("TERM", "dumb");
        command
    }

    fn spawn(&self, command: Command, label: &str) -> Process {
        let id = self.serial.get();
        self.serial.set(id + 1);
        Process::spawn(
            command,
            self.path(&format!("{label}-{id}.stdout")),
            self.path(&format!("{label}-{id}.stderr")),
        )
    }

    fn cli(&self, args: &[&str]) -> Output {
        let mut command = self.command(env!("CARGO_BIN_EXE_orch"));
        command.args(args);
        self.spawn(command, "cli").output(Duration::from_secs(15))
    }

    fn ok(&self, args: &[&str]) -> String {
        let output = self.cli(args);
        assert!(
            output.status.success(),
            "{} orch {args:?} failed: {:?}\nstdout: {}\nstderr: {}",
            self.name,
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).expect("UTF-8 CLI output")
    }

    fn json(&self, args: &[&str]) -> Value {
        let mut args = args.to_vec();
        args.push("--json");
        let output = self.ok(&args);
        serde_json::from_str(&output)
            .unwrap_or_else(|err| panic!("{} invalid CLI JSON: {err}: {output}", self.name))
    }

    fn git(&self, args: &[&str]) -> String {
        let mut command = self.command("git");
        command.args(args);
        let output = self.spawn(command, "git").output(Duration::from_secs(10));
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    }

    fn start(&self, listen: Option<SocketAddr>) {
        assert!(self.owner.borrow().is_none(), "already running");
        let mut command = self.command(env!("CARGO_BIN_EXE_orch"));
        command.args(["serve", "--peer-name", &self.name, "--verbose"]);
        if let Some(addr) = listen {
            command.args(["--listen", &addr.to_string()]);
        }
        let process = self.spawn(command, "owner");
        let pid = process.child.id().to_string();
        *self.owner.borrow_mut() = Some(process);
        wait("engine owner lock", &[self], || {
            self.owner
                .borrow_mut()
                .as_mut()
                .is_some_and(Process::running)
                && text(self.path(".orchestry/orchestry.lock")).trim() == pid
        });
    }

    fn stop(&self, signal: Signal) {
        if let Some(mut process) = self.owner.borrow_mut().take() {
            process.terminate(signal);
        }
    }

    fn signal(&self, signal: Signal) {
        let owner = self.owner.borrow();
        signal_group(owner.as_ref().expect("running owner").child.id(), signal);
    }

    fn add_agent(&self, name: &str, body: &str, workspace: &str) -> String {
        let id = self.serial.get();
        self.serial.set(id + 1);
        let script = self.path(&format!("agent-{id}.sh"));
        fs::write(
            &script,
            format!(
                "#!/bin/sh\nset -eu\nps -o pgid= -p $$ | tr -d ' ' >> {}\n{body}\n",
                quote(&self.path("adapter-groups"))
            ),
        )
        .expect("shell agent fixture");
        let command = format!("/bin/sh {}", quote(&script));
        self.json(&[
            "agent",
            "add",
            name,
            "--adapter",
            "shell",
            "--command",
            &command,
            "--approval-policy",
            "auto",
            "--workspace-mode",
            workspace,
        ])["id"]
            .as_str()
            .expect("agent id")
            .into()
    }

    fn task(&self, title: &str, assignee: Option<&str>, extra: &[&str]) -> String {
        let mut args = vec!["task", "add", title, "--max-attempts", "3"];
        if let Some(assignee) = assignee {
            args.extend(["--assignee", assignee]);
        }
        args.extend_from_slice(extra);
        self.json(&args)["id"].as_str().expect("task id").into()
    }

    fn tasks(&self) -> Vec<Value> {
        read_json(self.path(".orchestry/tasks/_index.json"))
            .as_array()
            .cloned()
            .unwrap_or_default()
    }

    fn task_value(&self, id: &str) -> Value {
        self.tasks()
            .into_iter()
            .find(|task| task["id"] == id)
            .unwrap_or(Value::Null)
    }

    fn wait_task(&self, id: &str, status: &str, peers: &[&Peer]) -> Value {
        wait(&format!("{id} becomes {status}"), peers, || {
            self.task_value(id)["status"] == status
        });
        self.json(&["task", "show", id])
    }

    fn runs(&self, task: &str) -> Vec<Value> {
        fs::read_dir(self.path(".orchestry/runs"))
            .expect("runs dir")
            .filter_map(Result::ok)
            .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "json"))
            .map(|entry| read_json(entry.path()))
            .filter(|run| run["task_id"] == task)
            .collect()
    }

    fn status(&self, alias: &str) -> Value {
        read_json(self.path(&format!(".orchestry/remotes/{alias}.json")))
    }

    fn online(&self, alias: &str, peers: &[&Peer]) {
        wait(&format!("{alias} online"), peers, || {
            self.status(alias)["online"] == true
        });
    }

    fn message(&self, id: &str) -> Value {
        read_json(self.path(&format!(".orchestry/messages/{id}.json")))
    }

    fn descendant_groups(&self, owner: u32) -> Vec<u32> {
        let mut command = self.command("/bin/ps");
        command.args(["-axo", "pid=,ppid=,pgid="]);
        let mut process = self.spawn(command, "cleanup-ps");
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            match process.child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) if Instant::now() < deadline => thread::sleep(POLL),
                _ => return Vec::new(),
            }
        }
        let rows: Vec<(u32, u32, u32)> = text(&process.stdout)
            .lines()
            .filter_map(|line| {
                let fields: Vec<u32> = line
                    .split_whitespace()
                    .filter_map(|word| word.parse().ok())
                    .collect();
                (fields.len() == 3).then(|| (fields[0], fields[1], fields[2]))
            })
            .collect();
        let mut descendants = HashSet::from([owner]);
        loop {
            let before = descendants.len();
            for (pid, parent, _) in &rows {
                if descendants.contains(parent) {
                    descendants.insert(*pid);
                }
            }
            if descendants.len() == before {
                break;
            }
        }
        rows.into_iter()
            .filter_map(|(pid, _, group)| descendants.contains(&pid).then_some(group))
            .collect()
    }

    fn diagnostics(&self) -> String {
        let owner = self.owner.borrow();
        let logs = owner
            .as_ref()
            .map(|owner| format!("{}\n{}", text(&owner.stdout), text(&owner.stderr)))
            .unwrap_or_default();
        let tail: String = logs
            .chars()
            .rev()
            .take(6000)
            .collect::<String>()
            .chars()
            .rev()
            .collect();
        format!(
            "{} at {}\nstate: {}\ntasks: {:?}\nlogs: {}",
            self.name,
            self.dir.path().display(),
            text(self.path(".orchestry/state.json")),
            self.tasks(),
            tail
        )
    }
}

impl Drop for Peer {
    fn drop(&mut self) {
        // Freeze dispatch before taking the process-tree snapshot: no new
        // detached git/adapter group can appear between inspection and kill.
        let owner = self.owner.borrow().as_ref().map(|p| p.child.id());
        let mut groups = Vec::new();
        if let Some(owner) = owner {
            signal_group(owner, Signal::SIGSTOP);
            groups.extend(self.descendant_groups(owner));
        }
        if let Some(running) = read_json(self.path(".orchestry/state.json"))["running"].as_object()
        {
            groups.extend(running.values().filter_map(|entry| {
                entry["pid"]
                    .as_u64()
                    .and_then(|pid| u32::try_from(pid).ok())
            }));
        }
        // Adapters deliberately start their own process groups. Each fixture
        // records its actual group before doing work, retaining the address
        // even after an owner crash reparents the shell to init.
        groups.extend(
            text(self.path("adapter-groups"))
                .lines()
                .filter_map(|line| line.trim().parse::<u32>().ok()),
        );
        self.stop(Signal::SIGKILL);
        for group in groups {
            signal_group(group, Signal::SIGKILL);
        }
    }
}

fn wait(label: &str, peers: &[&Peer], predicate: impl FnMut() -> bool) {
    wait_for(label, WAIT, peers, predicate);
}

fn wait_for(label: &str, within: Duration, peers: &[&Peer], mut predicate: impl FnMut() -> bool) {
    let deadline = Instant::now() + within;
    loop {
        if predicate() {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {label}\n{}",
            peers
                .iter()
                .map(|peer| peer.diagnostics())
                .collect::<Vec<_>>()
                .join("\n")
        );
        thread::sleep(POLL);
    }
}

/// Observe several real engine ticks while an invariant must remain true.
fn remains(label: &str, duration: Duration, mut predicate: impl FnMut() -> bool) {
    let deadline = Instant::now() + duration;
    while Instant::now() < deadline {
        assert!(predicate(), "invariant failed: {label}");
        thread::sleep(POLL);
    }
}

fn port() -> SocketAddr {
    TcpListener::bind("127.0.0.1:0")
        .expect("ephemeral port")
        .local_addr()
        .unwrap()
}

fn connect(controller: &Peer, worker: &Peer, alias: &str, addr: SocketAddr) {
    controller.ok(&["remote", "add", alias, &addr.to_string()]);
    worker.start(Some(addr));
    controller.start(None);
    controller.online(alias, &[controller, worker]);
}

fn gated_body(peer: &Peer) -> String {
    format!(
        "echo started > {}\nwhile [ ! -f {} ]; do echo working; sleep 0.1; done\necho effect >> {}\necho remote-completed",
        quote(&peer.path("started")),
        quote(&peer.path("release")),
        quote(&peer.path("effect"))
    )
}

fn release(peer: &Peer) {
    fs::write(peer.path("release"), "go").unwrap();
}

fn remote_id(agent: &str, alias: &str) -> String {
    format!("{agent}@{alias}")
}

#[test]
fn s01_agents_snapshot_exposes_online_agents_and_host_capabilities() {
    let worker = Peer::new("worker-one");
    let controller = Peer::new("controller");
    let agent = worker.add_agent("executor", "echo done", "shared");
    connect(&controller, &worker, "w1", port());
    let agents = controller.json(&["agent", "list"]);
    let agent = agents
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["id"] == remote_id(&agent, "w1"))
        .expect("remote agent visible");
    assert_eq!(agent["remote"]["online"], true);
    assert_eq!(agent["adapter"], "remote");
    assert!(
        agent["capabilities"]
            .as_array()
            .unwrap()
            .contains(&json!(std::env::consts::OS))
    );
    assert!(
        worker
            .json(&["agent", "list"])
            .as_array()
            .unwrap()
            .iter()
            .all(|agent| !agent["id"].as_str().unwrap().contains('@'))
    );
    let status = controller.json(&["remote", "status"]);
    assert_eq!(status["remotes"][0]["peer_name"], "worker-one");
    let inbound = worker.json(&["remote", "status"]);
    assert_eq!(inbound["inbound"]["controller_name"], "controller");
}

#[test]
fn s02_remote_task_executes_on_worker_and_persists_proof_and_forwarded_events() {
    let worker = Peer::new("worker-one");
    let controller = Peer::new("controller");
    let body = format!(
        "pwd > {}\nprintf '%s' \"$ORCHESTRY_TASK_PROMPT\" > {}\necho remote-event-marker",
        quote(&worker.path("effect")),
        quote(&worker.path("captured-prompt"))
    );
    let agent = worker.add_agent("executor", &body, "shared");
    connect(&controller, &worker, "w1", port());
    let task = controller.task(
        "remote execution",
        Some(&remote_id(&agent, "w1")),
        &["--description", "delegated-input-marker"],
    );
    let task_value = controller.wait_task(&task, "done", &[&controller, &worker]);
    assert_eq!(task_value["proof"]["remote"], "w1");
    assert_eq!(
        fs::canonicalize(text(worker.path("effect")).trim()).unwrap(),
        fs::canonicalize(worker.dir.path()).unwrap()
    );
    assert!(!controller.path("effect").exists());
    let runs = controller.runs(&task);
    assert_eq!(runs.len(), 1);
    let run_id = runs[0]["id"].as_str().unwrap();
    let mirror = worker
        .tasks()
        .into_iter()
        .find(|task| {
            task["labels"]
                .as_array()
                .unwrap()
                .contains(&json!(format!("exec:{run_id}")))
        })
        .expect("worker mirror task");
    assert_eq!(mirror["assignee"], agent);
    assert_eq!(mirror["max_attempts"], 1);
    assert_eq!(mirror["description"], runs[0]["prompt"]);
    let prompt = text(worker.path("captured-prompt"));
    assert!(prompt.contains("delegated-input-marker"));
    assert!(prompt.contains("Remote execution"));
    assert!(prompt.contains("controller files are not available"));
    wait(
        "forwarded output in controller run log",
        &[&controller, &worker],
        || {
            text(controller.path(&format!(".orchestry/runs/{run_id}.jsonl")))
                .contains("remote-event-marker")
        },
    );
}

#[test]
fn s03_second_controller_is_busy_and_same_instance_can_take_over_and_restart() {
    let worker = Peer::new("worker-one");
    let controller = Peer::new("controller");
    let other = Peer::new("other-controller");
    worker.add_agent("executor", "echo done", "shared");
    let addr = port();
    connect(&controller, &worker, "w1", addr);
    other.ok(&["remote", "add", "w1", &addr.to_string()]);
    other.start(None);
    wait(
        "different controller busy",
        &[&controller, &worker, &other],
        || {
            other.status("w1")["last_error"]
                .as_str()
                .is_some_and(|error| error.to_lowercase().contains("busy"))
        },
    );
    assert_eq!(controller.status("w1")["online"], true);
    assert_ne!(other.status("w1")["online"], true);
    other.stop(Signal::SIGTERM);
    // Freeze the old owner so its socket is genuinely half-open, rather than
    // relying on EOF racing a restart. A same-id hello must replace it.
    controller.signal(Signal::SIGSTOP);
    let instance = text(controller.path(".orchestry/peer-id"));
    {
        let mut stream = TcpStream::connect_timeout(&addr, Duration::from_secs(2)).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        writeln!(stream, "{}", json!({"t":"hello", "proto":1, "instance_id":instance.trim(), "name":"controller", "os":"test", "agents":[]})).unwrap();
        let mut line = String::new();
        BufReader::new(&stream).read_line(&mut line).unwrap();
        let welcome: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(welcome["t"], "welcome", "same-instance takeover: {welcome}");
    }
    controller.signal(Signal::SIGCONT);
    controller.stop(Signal::SIGKILL);
    controller.start(None);
    controller.online("w1", &[&controller, &worker, &other]);
    assert_eq!(text(controller.path(".orchestry/peer-id")), instance);
}

#[test]
fn s04_cross_peer_messages_and_replies_preserve_addresses_and_ack_delivery() {
    let worker = Peer::new("worker-one");
    let controller = Peer::new("controller");
    let executor = worker.add_agent("executor", "echo done", "shared");
    let lead = controller.add_agent("lead", "echo done", "shared");
    connect(&controller, &worker, "w1", port());
    let sent = controller.json(&[
        "msg",
        "send",
        &remote_id(&executor, "w1"),
        "question-marker",
        "--from",
        &lead,
        "--subject",
        "remote question",
    ]);
    let id = sent[0]["id"].as_str().unwrap();
    wait(
        "message persisted on worker and acked on controller",
        &[&controller, &worker],
        || {
            worker.message(id)["body"] == "question-marker"
                && controller.message(id)["status"] == "delivered"
        },
    );
    assert_eq!(
        worker.message(id)["from_agent_id"],
        remote_id(&lead, "controller")
    );
    assert_eq!(worker.message(id)["to_agent_id"], executor);
    let reply = worker.json(&[
        "msg",
        "send",
        &remote_id(&lead, "controller"),
        "answer-marker",
        "--from",
        &executor,
        "--reply-to",
        id,
    ]);
    let reply_id = reply[0]["id"].as_str().unwrap();
    wait("reply persisted and acked", &[&controller, &worker], || {
        controller.message(reply_id)["body"] == "answer-marker"
            && worker.message(reply_id)["status"] == "delivered"
    });
    assert_eq!(
        controller.message(reply_id)["from_agent_id"],
        remote_id(&executor, "w1")
    );
    assert_eq!(controller.message(reply_id)["reply_to"], id);
}

#[test]
fn s05_worker_outage_marks_offline_and_queued_task_dispatches_after_restart() {
    let worker = Peer::new("worker-one");
    let controller = Peer::new("controller");
    let agent = worker.add_agent("executor", "echo restarted", "shared");
    let addr = port();
    connect(&controller, &worker, "w1", addr);
    worker.stop(Signal::SIGKILL);
    wait_for(
        "worker offline within 20 seconds",
        Duration::from_secs(20),
        &[&controller, &worker],
        || controller.status("w1")["online"] == false,
    );
    let task = controller.task("wait while offline", Some(&remote_id(&agent, "w1")), &[]);
    remains(
        "offline task stays todo without a run",
        Duration::from_millis(500),
        || controller.task_value(&task)["status"] == "todo" && controller.runs(&task).is_empty(),
    );
    assert!(
        controller
            .ok(&["task", "show", &task])
            .contains("remote w1 offline")
    );
    let agents = controller.json(&["agent", "list"]);
    let cached = agents
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["id"] == remote_id(&agent, "w1"))
        .unwrap();
    assert_eq!(cached["remote"]["online"], false);
    worker.start(Some(addr));
    controller.online("w1", &[&controller, &worker]);
    controller.wait_task(&task, "done", &[&controller, &worker]);
    assert_eq!(controller.runs(&task).len(), 1);
}

#[test]
fn s06_inflight_link_outage_beyond_stall_timeout_never_retries_or_duplicates_effect() {
    let worker = Peer::new("worker-one");
    let controller = Peer::new("controller");
    controller.config("shared", 500);
    let agent = worker.add_agent("executor", &gated_body(&worker), "shared");
    connect(&controller, &worker, "w1", port());
    let task = controller.task(
        "exactly one worker effect",
        Some(&remote_id(&agent, "w1")),
        &[],
    );
    wait("worker shell started", &[&controller, &worker], || {
        worker.path("started").exists()
    });
    fs::write(controller.path("peer-block"), "block").unwrap();
    wait(
        "test hook disconnects link",
        &[&controller, &worker],
        || controller.status("w1")["online"] == false,
    );
    assert!(
        controller
            .ok(&["task", "show", &task])
            .contains("outcome pending")
    );
    // Four times the configured controller stall timeout. The worker still
    // emits progress; only the network is unavailable.
    remains(
        "outage cannot fail or retry a remote run",
        Duration::from_secs(2),
        || {
            let value = controller.task_value(&task);
            value["status"] == "in_progress"
                && value["attempts"] == 1
                && controller.runs(&task).len() == 1
        },
    );
    release(&worker);
    wait(
        "effect completed while disconnected",
        &[&controller, &worker],
        || worker.path("effect").exists(),
    );
    remains(
        "finished effect remains outcome-pending offline",
        Duration::from_millis(300),
        || controller.task_value(&task)["status"] == "in_progress",
    );
    fs::remove_file(controller.path("peer-block")).unwrap();
    controller.online("w1", &[&controller, &worker]);
    let value = controller.wait_task(&task, "done", &[&controller, &worker]);
    assert_eq!(value["attempts"], 1);
    assert_eq!(controller.runs(&task).len(), 1);
    assert_eq!(worker.tasks().len(), 1);
    assert_eq!(text(worker.path("effect")), "effect\n");
}

#[test]
fn s07_controller_restart_cancels_without_retry_and_sync_cancels_worker_mirror() {
    let worker = Peer::new("worker-one");
    let controller = Peer::new("controller");
    let agent = worker.add_agent("executor", &gated_body(&worker), "shared");
    connect(&controller, &worker, "w1", port());
    let task = controller.task(
        "cancel when controller restarts",
        Some(&remote_id(&agent, "w1")),
        &[],
    );
    wait(
        "running shell before restart",
        &[&controller, &worker],
        || worker.path("started").exists(),
    );
    controller.stop(Signal::SIGKILL);
    controller.start(None);
    let value = controller.wait_task(&task, "cancelled", &[&controller, &worker]);
    controller.online("w1", &[&controller, &worker]);
    wait(
        "sync cancels untracked mirror",
        &[&controller, &worker],
        || {
            worker
                .tasks()
                .first()
                .is_some_and(|t| t["status"] == "cancelled")
        },
    );
    assert_eq!(value["attempts"], 1);
    let runs = controller.runs(&task);
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0]["status"], "cancelled");
    assert!(!worker.path("effect").exists());
}

#[test]
fn s08a_worker_review_failure_cannot_autoapprove_or_release_a_dependent() {
    let worker = Peer::new("worker-one");
    let controller = Peer::new("controller");
    // Execute a real failing review child, without requiring npm or any
    // external package installation on the test host.
    let npm = worker.bin.join("npm");
    fs::write(
        &npm,
        "#!/bin/sh\necho worker-review-failure-marker\nexit 1\n",
    )
    .unwrap();
    fs::set_permissions(&npm, fs::Permissions::from_mode(0o700)).unwrap();
    let agent = worker.add_agent("executor", "echo successful execution", "shared");
    connect(&controller, &worker, "w1", port());
    let task = controller.task(
        "failing worker review",
        Some(&remote_id(&agent, "w1")),
        &["--review-criteria", "test_pass"],
    );
    let dependent = controller.task(
        "must remain blocked",
        Some(&remote_id(&agent, "w1")),
        &["--depends-on", &task],
    );
    let value = controller.wait_task(&task, "review", &[&controller, &worker]);
    assert!(
        value["proof"]["agent_summary"]
            .as_str()
            .unwrap()
            .contains("REMOTE REVIEW FAILED on w1")
    );
    assert_eq!(value["review_results"][0]["passed"], false);
    assert!(
        value["review_results"][0]["output"]
            .as_str()
            .unwrap()
            .contains("worker-review-failure-marker")
    );
    remains(
        "review failure blocks dependent",
        Duration::from_millis(500),
        || {
            controller.task_value(&task)["status"] == "review"
                && controller.task_value(&dependent)["status"] == "todo"
                && controller.runs(&dependent).is_empty()
        },
    );
    let mirror = worker
        .tasks()
        .into_iter()
        .find(|task| task["review_criteria"][0] == "test_pass")
        .unwrap();
    assert_eq!(mirror["status"], "review");
    assert_eq!(mirror["review_results"][0]["passed"], false);
}

#[test]
fn s08b_real_worker_git_merge_conflict_requires_controller_review_and_blocks_dependent() {
    let worker = Peer::new("worker-one");
    let controller = Peer::new("controller");
    worker.config("worktree", 30_000);
    fs::write(worker.path("conflict.txt"), "base\n").unwrap();
    worker.git(&["add", "conflict.txt"]);
    worker.git(&["commit", "-m", "base conflict fixture"]);
    let body = format!(
        "printf 'worker-branch\\n' > conflict.txt\ngit add conflict.txt\ngit commit -m worker-branch\necho started > {}\nwhile [ ! -f {} ]; do echo working; sleep 0.1; done\necho worker branch committed",
        quote(&worker.path("started")),
        quote(&worker.path("release"))
    );
    let agent = worker.add_agent("executor", &body, "worktree");
    connect(&controller, &worker, "w1", port());
    let task = controller.task(
        "actual worker merge conflict",
        Some(&remote_id(&agent, "w1")),
        &[],
    );
    let dependent = controller.task(
        "blocked by integration",
        Some(&remote_id(&agent, "w1")),
        &["--depends-on", &task],
    );
    wait(
        "agent committed its divergent branch",
        &[&controller, &worker],
        || worker.path("started").exists(),
    );
    fs::write(worker.path("conflict.txt"), "worker-main\n").unwrap();
    worker.git(&["add", "conflict.txt"]);
    worker.git(&["commit", "-m", "diverge worker main"]);
    release(&worker);
    let value = controller.wait_task(&task, "review", &[&controller, &worker]);
    let summary = value["proof"]["agent_summary"].as_str().unwrap();
    assert!(summary.contains("REMOTE MERGE CONFLICT on w1"), "{value}");
    assert!(
        summary.contains("CONFLICT"),
        "expected real git conflict: {summary}"
    );
    assert_eq!(text(worker.path("conflict.txt")), "worker-main\n");
    assert!(
        !worker.path(".git/MERGE_HEAD").exists(),
        "worker merge was not aborted"
    );
    remains(
        "integration failure blocks dependent",
        Duration::from_millis(500),
        || {
            controller.task_value(&task)["status"] == "review"
                && controller.task_value(&dependent)["status"] == "todo"
                && controller.runs(&dependent).is_empty()
        },
    );
    let mirror = worker
        .tasks()
        .into_iter()
        .find(|t| t["status"] == "review")
        .expect("worker mirror requires review");
    assert!(
        mirror["proof"]["agent_summary"]
            .as_str()
            .unwrap()
            .contains("MERGE CONFLICT")
    );
}

#[test]
fn s09_capability_labels_route_to_matching_worker_and_unsatisfied_tasks_wait() {
    let mut worker_one = Peer::new("worker-one");
    worker_one.os = "macos".into();
    let mut worker_two = Peer::new("worker-two");
    worker_two.os = "linux".into();
    let mut controller = Peer::new("controller");
    controller.os = "controller-test".into();
    worker_one.add_agent("mac-executor", "echo mac", "shared");
    let linux = worker_two.add_agent("linux-executor", "echo linux", "shared");
    let addr_one = port();
    let addr_two = port();
    controller.ok(&["remote", "add", "w1", &addr_one.to_string()]);
    controller.ok(&["remote", "add", "w2", &addr_two.to_string()]);
    worker_one.start(Some(addr_one));
    worker_two.start(Some(addr_two));
    controller.start(None);
    controller.online("w1", &[&controller, &worker_one, &worker_two]);
    controller.online("w2", &[&controller, &worker_one, &worker_two]);
    let impossible = controller.task(
        "no freebsd worker",
        None,
        &["--labels", "cap:freebsd", "--priority", "1"],
    );
    let task = controller.task("linux work", None, &["--labels", "cap:linux"]);
    let value = controller.wait_task(&task, "done", &[&controller, &worker_one, &worker_two]);
    assert_eq!(value["assignee"], remote_id(&linux, "w2"));
    assert_eq!(value["proof"]["remote"], "w2");
    assert_eq!(controller.task_value(&impossible)["status"], "todo");
    assert!(
        controller
            .ok(&["task", "show", &impossible])
            .contains("no online agent with cap freebsd")
    );
    assert!(worker_one.tasks().is_empty());
}

#[test]
fn s10_one_remote_agent_is_reserved_until_its_first_mirror_finishes() {
    let worker = Peer::new("worker-one");
    let controller = Peer::new("controller");
    let agent = worker.add_agent("executor", &gated_body(&worker), "shared");
    connect(&controller, &worker, "w1", port());
    // Freeze after discovery, so both valid explicit assignments exist before
    // the same next dispatch tick. A cleanup SIGKILL also handles a panic here.
    controller.signal(Signal::SIGSTOP);
    let first = controller.task("first reservation", Some(&remote_id(&agent, "w1")), &[]);
    let second = controller.task("second reservation", Some(&remote_id(&agent, "w1")), &[]);
    controller.signal(Signal::SIGCONT);
    wait("one mirror shell starts", &[&controller, &worker], || {
        worker.path("started").exists()
    });
    remains(
        "only one submit while the agent is reserved",
        Duration::from_millis(600),
        || {
            worker.tasks().len() == 1
                && controller.runs(&first).len() + controller.runs(&second).len() == 1
        },
    );
    release(&worker);
    controller.wait_task(&first, "done", &[&controller, &worker]);
    controller.wait_task(&second, "done", &[&controller, &worker]);
    assert_eq!(worker.tasks().len(), 2);
    assert_eq!(text(worker.path("effect")).lines().count(), 2);
}

#[test]
fn s11_registry_survives_restart_and_removal_closes_link_and_fails_inflight_without_retry() {
    let worker = Peer::new("worker-one");
    let controller = Peer::new("controller");
    let agent = worker.add_agent("executor", &gated_body(&worker), "shared");
    connect(&controller, &worker, "w1", port());
    controller.stop(Signal::SIGTERM);
    controller.start(None);
    controller.online("w1", &[&controller, &worker]);
    assert_eq!(controller.json(&["remote", "list"])[0]["alias"], "w1");
    let task = controller.task(
        "unknown outcome on removal",
        Some(&remote_id(&agent, "w1")),
        &[],
    );
    wait(
        "running task before removal",
        &[&controller, &worker],
        || worker.path("started").exists(),
    );
    controller.ok(&["remote", "remove", "w1"]);
    controller.wait_task(&task, "failed", &[&controller, &worker]);
    wait(
        "worker sees disconnected controller",
        &[&controller, &worker],
        || read_json(worker.path(".orchestry/remotes/_listener.json"))["controller_name"].is_null(),
    );
    assert!(
        controller
            .json(&["agent", "list"])
            .as_array()
            .unwrap()
            .is_empty()
    );
    let runs = controller.runs(&task);
    assert_eq!(runs.len(), 1);
    assert!(
        runs[0]["error"]
            .as_str()
            .unwrap()
            .contains("outcome unknown")
    );
    controller.stop(Signal::SIGTERM);
    controller.start(None);
    assert!(
        controller
            .json(&["remote", "list"])
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(controller.task_value(&task)["status"], "failed");
}

#[test]
fn s12_listening_defaults_off_and_nonowner_tui_cannot_enable_it() {
    let peer = Peer::new("default-off");
    // Occupying the default port makes an accidental default listener fail
    // deterministically, without connecting to an unrelated local service.
    let _default_port_guard = TcpListener::bind("127.0.0.1:7690").ok();
    peer.start(None);
    assert!(peer.json(&["remote", "status"])["listen"].is_null());
    remains(
        "default-off owner stays alive",
        Duration::from_millis(400),
        || peer.owner.borrow_mut().as_mut().unwrap().running(),
    );
    let output = peer.cli(&[
        "tui",
        "--listen",
        &port().to_string(),
        "--peer-name",
        "observer",
    ]);
    assert!(!output.status.success(), "non-owner TUI accepted listening");
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(error.contains("engine owner"), "{error}");
    assert!(peer.owner.borrow_mut().as_mut().unwrap().running());
    assert!(read_json(peer.path(".orchestry/remotes/_inbound.json")).is_null());
}

#[test]
fn s13_registry_metadata_change_preserves_inflight_execution_and_exactly_one_effect() {
    let worker = Peer::new("worker-one");
    let controller = Peer::new("controller");
    let agent = worker.add_agent("executor", &gated_body(&worker), "shared");
    connect(&controller, &worker, "w1", port());
    let task = controller.task(
        "preserve stream across registry update",
        Some(&remote_id(&agent, "w1")),
        &[],
    );
    wait(
        "worker starts before registry update",
        &[&controller, &worker],
        || worker.path("started").exists(),
    );
    // A registry edit is not removal. Older link management dropped the
    // execution hub for any changed config, then synced an empty active set
    // and cancelled a live mirror. Change only metadata to isolate that path.
    let mut registry = controller.json(&["remote", "list"]);
    registry[0]["added_at"] = json!("2000-01-01T00:00:00.000Z");
    let staged = controller.path(".orchestry/remotes-fixture.tmp");
    fs::write(&staged, serde_json::to_vec_pretty(&registry).unwrap()).unwrap();
    fs::rename(staged, controller.path(".orchestry/remotes.yml")).unwrap();
    remains(
        "registry reread preserves controller stream and worker mirror",
        Duration::from_secs(1),
        || {
            let value = controller.task_value(&task);
            value["status"] == "in_progress"
                && value["attempts"] == 1
                && controller.runs(&task).len() == 1
                && worker.tasks().len() == 1
                && worker.tasks()[0]["status"] == "in_progress"
        },
    );
    release(&worker);
    let value = controller.wait_task(&task, "done", &[&controller, &worker]);
    assert_eq!(value["attempts"], 1);
    assert_eq!(controller.runs(&task).len(), 1);
    assert_eq!(worker.tasks().len(), 1);
    assert_eq!(text(worker.path("effect")), "effect\n");
}

#[test]
fn s14_two_remotes_route_each_os_and_offline_capability_does_not_block_other_worker() {
    let mut mac = Peer::new("mac-worker");
    mac.os = "macos".into();
    let mut linux = Peer::new("linux-worker");
    linux.os = "linux".into();
    let mut controller = Peer::new("controller");
    controller.os = "controller-test".into();
    let mac_agent = mac.add_agent("mac", "echo mac", "shared");
    let linux_agent = linux.add_agent("linux", "echo linux", "shared");
    let mac_addr = port();
    let linux_addr = port();
    controller.ok(&["remote", "add", "mac", &mac_addr.to_string()]);
    controller.ok(&["remote", "add", "linux", &linux_addr.to_string()]);
    mac.start(Some(mac_addr));
    linux.start(Some(linux_addr));
    controller.start(None);
    let peers = [&controller, &mac, &linux];
    controller.online("mac", &peers);
    controller.online("linux", &peers);
    for (cap, agent) in [
        ("macos", remote_id(&mac_agent, "mac")),
        ("linux", remote_id(&linux_agent, "linux")),
    ] {
        let task = controller.task(
            "route while both online",
            None,
            &["--labels", &format!("cap:{cap}")],
        );
        let done = controller.wait_task(&task, "done", &peers);
        assert_eq!(done["assignee"], agent);
    }
    mac.stop(Signal::SIGKILL);
    wait("mac offline", &peers, || {
        controller.status("mac")["online"] == false
    });
    let waiting = controller.task(
        "mac waits",
        None,
        &["--labels", "cap:macos", "--priority", "1"],
    );
    let available = controller.task(
        "linux still dispatches",
        None,
        &["--labels", "cap:linux", "--priority", "2"],
    );
    let done = controller.wait_task(&available, "done", &peers);
    assert_eq!(done["assignee"], remote_id(&linux_agent, "linux"));
    assert_eq!(done["proof"]["remote"], "linux");
    assert_eq!(controller.task_value(&waiting)["status"], "todo");
    assert!(controller.runs(&waiting).is_empty());
    assert!(
        controller
            .ok(&["task", "show", &waiting])
            .contains("no online agent with cap macos")
    );
    mac.start(Some(mac_addr));
    controller.online("mac", &peers);
    let done = controller.wait_task(&waiting, "done", &peers);
    assert_eq!(done["assignee"], remote_id(&mac_agent, "mac"));
    assert_eq!(done["proof"]["remote"], "mac");
    assert_eq!(mac.tasks().len(), 2);
    assert_eq!(linux.tasks().len(), 2);
}

#[test]
fn s15_offline_message_survives_controller_restart_and_delivers_after_worker_reconnect() {
    let worker = Peer::new("worker");
    let controller = Peer::new("controller");
    let agent = worker.add_agent("recipient", "echo unused", "shared");
    let addr = port();
    connect(&controller, &worker, "w1", addr);
    worker.stop(Signal::SIGKILL);
    wait(
        "worker offline before send",
        &[&controller, &worker],
        || controller.status("w1")["online"] == false,
    );
    let sent = controller.json(&[
        "msg",
        "send",
        &remote_id(&agent, "w1"),
        "offline-message",
        "--subject",
        "queued",
    ]);
    let id = sent[0]["id"].as_str().unwrap();
    assert_eq!(controller.message(id)["status"], "pending");
    assert!(worker.message(id).is_null());
    controller.stop(Signal::SIGTERM);
    controller.start(None);
    assert_eq!(controller.message(id)["status"], "pending");
    worker.start(Some(addr));
    controller.online("w1", &[&controller, &worker]);
    wait(
        "offline message acknowledged after reconnect",
        &[&controller, &worker],
        || {
            controller.message(id)["status"] == "delivered"
                && worker.message(id)["body"] == "offline-message"
        },
    );
    assert_eq!(worker.message(id)["to_agent_id"], agent);
    assert_eq!(worker.message(id)["from_agent_id"], "cli@controller");
    let messages = worker.json(&["msg", "inbox", &agent]);
    assert_eq!(
        messages
            .as_array()
            .unwrap()
            .iter()
            .filter(|m| m["id"] == id)
            .count(),
        1
    );
}

#[test]
fn s16_removing_remote_with_pending_message_preserves_outbox_without_delivering_to_other_peer() {
    let removed = Peer::new("removed-worker");
    let survivor = Peer::new("surviving-worker");
    let controller = Peer::new("controller");
    let recipient = removed.add_agent("recipient", "echo unused", "shared");
    let other = survivor.add_agent("other", "echo unused", "shared");
    let removed_addr = port();
    let survivor_addr = port();
    controller.ok(&["remote", "add", "removed", &removed_addr.to_string()]);
    controller.ok(&["remote", "add", "survivor", &survivor_addr.to_string()]);
    removed.start(Some(removed_addr));
    survivor.start(Some(survivor_addr));
    controller.start(None);
    let peers = [&controller, &removed, &survivor];
    controller.online("removed", &peers);
    controller.online("survivor", &peers);
    removed.stop(Signal::SIGKILL);
    wait("removed worker offline", &peers, || {
        controller.status("removed")["online"] == false
    });
    let sent = controller.json(&[
        "msg",
        "send",
        &remote_id(&recipient, "removed"),
        "pending-removal",
    ]);
    let id = sent[0]["id"].as_str().unwrap();
    assert_eq!(controller.message(id)["status"], "pending");
    controller.ok(&["remote", "remove", "removed"]);
    controller.stop(Signal::SIGTERM);
    removed.start(Some(removed_addr));
    controller.start(None);
    controller.online("survivor", &peers);
    // An acknowledged message on the surviving link is a polling barrier.
    let barrier = controller.json(&["msg", "send", &remote_id(&other, "survivor"), "barrier"]);
    let barrier_id = barrier[0]["id"].as_str().unwrap();
    wait("surviving link still forwards messages", &peers, || {
        controller.message(barrier_id)["status"] == "delivered"
    });
    assert_eq!(
        controller
            .json(&["remote", "list"])
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(controller.message(id)["status"], "pending");
    assert_eq!(
        controller.message(id)["to_agent_id"],
        remote_id(&recipient, "removed")
    );
    assert!(removed.message(id).is_null());
    assert!(survivor.message(id).is_null());
    assert!(
        read_json(removed.path(".orchestry/remotes/_listener.json"))["controller_name"].is_null()
    );
    assert!(!controller.path(".orchestry/remotes/removed.json").exists());
}
