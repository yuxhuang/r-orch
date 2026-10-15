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
            .env_remove("RUST_LOG")
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

struct HealthLeaseFixture {
    _dir: tempfile::TempDir,
    paths: orch_infra::storage::paths::Paths,
    publisher: orch_infra::storage::remote::FileRemoteStore,
    status: orch_domain::RemoteStatus,
}

impl HealthLeaseFixture {
    async fn new() -> Self {
        use orch_infra::storage::{lock::acquire_lock, paths::Paths, remote::FileRemoteStore};
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::new(dir.path());
        assert!(acquire_lock(&paths.lock_path()).await.unwrap().acquired);
        let publisher = FileRemoteStore::new(paths.clone());
        let status = orch_domain::RemoteStatus {
            alias: "w1".into(),
            addr: "127.0.0.1:1".into(),
            online: true,
            last_seen: None,
            last_error: None,
            peer_name: None,
            instance_id: None,
            os: None,
            agents: vec![],
        };
        publisher.write_status(&status).await.unwrap();
        Self {
            _dir: dir,
            paths,
            publisher,
            status,
        }
    }

    fn reader(&self) -> orch_infra::storage::remote::FileRemoteStore {
        orch_infra::storage::remote::FileRemoteStore::new(self.paths.clone())
    }

    fn deny_status_writes(&self) -> StatusWriteFault {
        let dir = self.paths.remotes_dir();
        let fault = StatusWriteFault(dir.clone(), fs::metadata(&dir).unwrap().permissions());
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o555)).unwrap();
        assert!(File::create(dir.join("write-probe")).is_err());
        fault
    }
}

struct StatusWriteFault(PathBuf, fs::Permissions);

impl Drop for StatusWriteFault {
    fn drop(&mut self) {
        let _ = fs::set_permissions(&self.0, self.1.clone());
    }
}

#[tokio::test]
async fn s20_health_lease_missing_sidecar_makes_cached_online_status_offline() {
    let fixture = HealthLeaseFixture::new().await;
    fs::remove_file(fixture.paths.remotes_dir().join("w1.health")).unwrap();
    assert!(
        !fixture
            .reader()
            .read_status("w1")
            .await
            .unwrap()
            .unwrap()
            .online
    );
}

#[tokio::test]
async fn s20_health_lease_surviving_clone_keeps_published_status_online() {
    let fixture = HealthLeaseFixture::new().await;
    let reader = fixture.reader();
    let clone = fixture.publisher.clone();
    drop(fixture.publisher);
    let observed = reader.read_status("w1").await.unwrap().unwrap();
    drop(clone);
    assert!(observed.online);
}

#[tokio::test]
async fn s20_health_lease_last_clone_drop_makes_cached_online_status_offline() {
    let fixture = HealthLeaseFixture::new().await;
    let reader = fixture.reader();
    let clone = fixture.publisher.clone();
    drop(fixture.publisher);
    drop(clone);
    assert!(!reader.read_status("w1").await.unwrap().unwrap().online);
}

#[tokio::test]
async fn s20_health_lease_failed_online_save_withdraws_existing_publication() {
    let fixture = HealthLeaseFixture::new().await;
    let _fault = fixture.deny_status_writes();
    assert!(
        fixture
            .publisher
            .write_status(&fixture.status)
            .await
            .is_err()
    );
    assert!(
        !fixture
            .reader()
            .read_status("w1")
            .await
            .unwrap()
            .unwrap()
            .online
    );
}

#[tokio::test]
async fn s20_health_lease_failed_offline_save_withdraws_existing_publication() {
    let mut fixture = HealthLeaseFixture::new().await;
    let _fault = fixture.deny_status_writes();
    fixture.status.online = false;
    assert!(
        fixture
            .publisher
            .write_status(&fixture.status)
            .await
            .is_err()
    );
    assert!(
        !fixture
            .reader()
            .read_status("w1")
            .await
            .unwrap()
            .unwrap()
            .online
    );
}

#[test]
fn s21_removing_link_releases_health_lease_while_controller_owner_stays_live() {
    let worker = Peer::new("worker-lease-lifetime");
    let controller = Peer::new("controller-lease-lifetime");
    worker.add_agent("executor", "echo healthy", "shared");
    connect(&controller, &worker, "w1", port());
    let snapshot = controller.path(".orchestry/remotes/w1.json");
    let cached = fs::read(&snapshot).unwrap();
    let health = controller.path(".orchestry/remotes/w1.health");
    controller.ok(&["remote", "remove", "w1"]);
    wait(
        "removed link releases its kernel lease",
        &[&controller, &worker],
        || File::open(&health).unwrap().try_lock_shared().is_ok(),
    );
    assert!(
        controller
            .owner
            .borrow_mut()
            .as_mut()
            .unwrap()
            .child
            .try_wait()
            .unwrap()
            .is_none(),
        "controller owner must remain live after removing a link"
    );
    // Retained JSON has no authority once the per-link task is gone, even
    // though the manager and engine still own their shared runtime stores.
    fs::write(&snapshot, cached).unwrap();
    assert_eq!(read_json(&snapshot)["online"], true);
    let reader = orch_infra::storage::remote::FileRemoteStore::new(
        orch_infra::storage::paths::Paths::new(controller.dir.path()),
    );
    let observed = tokio::runtime::Runtime::new()
        .unwrap()
        .block_on(reader.read_status("w1"))
        .unwrap()
        .unwrap();
    assert!(!observed.online);
}

#[test]
fn s19_failed_status_save_and_socket_loss_block_new_dispatch_until_recovery() {
    let worker = Peer::new("worker-status-failure");
    let controller = Peer::new("controller-status-failure");
    let agent = worker.add_agent("executor", "echo recovered", "shared");
    let local = controller.add_agent("local", "echo local-unaffected", "shared");
    let addr = port();
    connect(&controller, &worker, "w1", addr);

    struct RestorePermissions(PathBuf, fs::Permissions);
    impl Drop for RestorePermissions {
        fn drop(&mut self) {
            let _ = fs::set_permissions(&self.0, self.1.clone());
        }
    }
    let dir = controller.path(".orchestry/remotes");
    let permissions = RestorePermissions(dir.clone(), fs::metadata(&dir).unwrap().permissions());
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o555)).unwrap();
    assert!(
        File::create(dir.join("write-probe")).is_err(),
        "fault injection must deny writes"
    );
    wait(
        "status persistence failure visible without RUST_LOG",
        &[&controller, &worker],
        || {
            let owner = controller.owner.borrow();
            let owner = owner.as_ref().unwrap();
            format!("{}{}", text(&owner.stdout), text(&owner.stderr)).contains("Write peer status")
        },
    );
    assert_eq!(
        controller.status("w1")["online"],
        true,
        "fault must retain the stale snapshot"
    );
    let status = controller.json(&["remote", "status"]);
    assert_eq!(status["remotes"][0]["online"], false);
    assert!(status["remotes"][0]["last_error"].is_string());
    worker.stop(Signal::SIGKILL);
    wait(
        "socket loss visible without RUST_LOG",
        &[&controller, &worker],
        || {
            let owner = controller.owner.borrow();
            let owner = owner.as_ref().unwrap();
            format!("{}{}", text(&owner.stdout), text(&owner.stderr))
                .contains("Peer w1 disconnected")
        },
    );
    let task = controller.task("wait for healthy link", Some(&remote_id(&agent, "w1")), &[]);
    let local_task = controller.task("local dispatch unaffected", Some(&local), &[]);
    controller.wait_task(&local_task, "done", &[&controller, &worker]);
    remains(
        "no remote attempt or reservation while status is unwritable",
        Duration::from_millis(500),
        || {
            let state = read_json(controller.path(".orchestry/state.json"));
            let valid = controller.task_value(&task)["status"] == "todo"
                && controller.task_value(&task)["attempts"] == 0
                && controller.runs(&task).is_empty()
                && state["running"].get(&task).is_none();
            assert!(valid, "{}", controller.diagnostics());
            valid
        },
    );
    assert!(
        controller
            .ok(&["task", "show", &task])
            .contains("remote w1 offline")
    );
    assert!(
        worker.tasks().is_empty(),
        "no submit may create a worker mirror"
    );

    controller.stop(Signal::SIGKILL);
    controller.start(None);
    let another_local = controller.task("local dispatch after restart", Some(&local), &[]);
    controller.wait_task(&another_local, "done", &[&controller, &worker]);
    assert_eq!(controller.task_value(&task)["status"], "todo");
    assert_eq!(controller.task_value(&task)["attempts"], 0);
    assert!(controller.runs(&task).is_empty());

    drop(permissions);
    worker.start(Some(addr));
    controller.online("w1", &[&controller, &worker]);
    controller.wait_task(&task, "done", &[&controller, &worker]);
    assert_eq!(controller.task_value(&task)["attempts"], 1);
    assert_eq!(controller.runs(&task).len(), 1);
    assert_eq!(worker.tasks().len(), 1);
    assert_eq!(
        controller.json(&["remote", "status"])["remotes"][0]["last_error"],
        Value::Null
    );
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
fn s22_sigterm_serve_drains_connected_remote_cancellation_before_owner_exit() {
    connected_shutdown_cancels_before_exit(false);
}

#[test]
fn s22b_sigterm_run_watch_drains_connected_remote_cancellation_before_owner_exit() {
    connected_shutdown_cancels_before_exit(true);
}

#[test]
fn s22c_sigterm_tui_drains_connected_remote_cancellation_before_owner_exit() {
    use portable_pty::{CommandBuilder, PtySize, native_pty_system};

    struct Dashboard {
        child: Box<dyn portable_pty::Child + Send + Sync>,
        master: Option<Box<dyn portable_pty::MasterPty + Send>>,
    }
    impl Drop for Dashboard {
        fn drop(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
            self.master.take();
        }
    }

    let worker = Peer::new("shutdown-worker");
    let controller = Peer::new("shutdown-controller");
    let agent = worker.add_agent("executor", &shutdown_gated_body(&worker), "shared");
    let worker_addr = port();
    let controller_addr = port();
    controller.ok(&["remote", "add", "w1", &worker_addr.to_string()]);
    worker.start(Some(worker_addr));
    let pair = native_pty_system()
        .openpty(PtySize {
            rows: 30,
            cols: 120,
            pixel_width: 0,
            pixel_height: 0,
        })
        .unwrap();
    let fixture = controller.command(env!("CARGO_BIN_EXE_orch"));
    let mut command = CommandBuilder::new(fixture.get_program());
    for (key, value) in fixture.get_envs() {
        if let Some(value) = value {
            command.env(key, value);
        } else {
            command.env_remove(key);
        }
    }
    command.env("TERM", "xterm-256color");
    command.cwd(controller.dir.path());
    command.args(["tui", "--listen", &controller_addr.to_string()]);
    let mut dashboard = Dashboard {
        child: pair.slave.spawn_command(command).unwrap(),
        master: Some(pair.master),
    };
    drop(pair.slave);
    let mut reader = dashboard
        .master
        .as_ref()
        .unwrap()
        .try_clone_reader()
        .unwrap();
    let capture = controller.path("tui-capture");
    let mut log = File::create(&capture).unwrap();
    let reading = thread::spawn(move || {
        let _ = std::io::copy(&mut reader, &mut log);
    });
    let peers = [&controller, &worker];
    controller.online("w1", &peers);
    let task = controller.task(
        "TUI cancellation before exit",
        Some(&remote_id(&agent, "w1")),
        &[],
    );
    wait("running remote shell under TUI", &peers, || {
        worker.path("started").exists()
    });
    let pid = text(worker.path("started")).trim().parse::<i32>().unwrap();
    let owner = dashboard.child.process_id().unwrap();
    kill(Pid::from_raw(owner as i32), Signal::SIGTERM).unwrap();
    let started = Instant::now();
    let status = loop {
        if let Some(status) = dashboard.child.try_wait().unwrap() {
            break status;
        }
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "TUI SIGTERM timed out: {}",
            text(&capture)
        );
        thread::sleep(POLL);
    };
    eprintln!("TUI SIGTERM exit in {:?}", started.elapsed());
    assert!(status.success(), "TUI exit {status:?}: {}", text(&capture));
    assert!(!controller.path(".orchestry/orchestry.lock").exists());
    assert!(
        kill(Pid::from_raw(pid), None).is_err(),
        "shell survived TUI exit"
    );
    assert_eq!(worker.tasks()[0]["status"], "cancelled");
    assert_eq!(controller.task_value(&task)["status"], "cancelled");
    release(&worker);
    remains(
        "no effect after TUI shutdown",
        Duration::from_millis(500),
        || !worker.path("effect").exists(),
    );
    drop(TcpListener::bind(controller_addr).expect("TUI listener released"));
    dashboard.master.take();
    reading.join().unwrap();
    let captured = text(&capture);
    assert!(
        captured.contains("\x1b[?1049l"),
        "TUI did not restore alternate screen"
    );
    sigterm_owner(&worker);
    drop(TcpListener::bind(worker_addr).expect("worker listener released"));
}

fn shutdown_gated_body(peer: &Peer) -> String {
    // Keep stdout silent while gated: a closed event pipe must not mask a
    // surviving adapter's side effect after the controller has exited.
    format!(
        "echo $$ > {}\nwhile [ ! -f {} ]; do sleep 0.04; done\necho effect >> {}",
        quote(&peer.path("started")),
        quote(&peer.path("release")),
        quote(&peer.path("effect")),
    )
}

fn sigterm_owner(peer: &Peer) -> Output {
    let process = peer.owner.borrow_mut().take().expect("running owner");
    // Signal the controller itself, matching a service manager's SIGTERM.
    kill(Pid::from_raw(process.child.id() as i32), Signal::SIGTERM).unwrap();
    let started = Instant::now();
    let output = process.output(Duration::from_secs(5));
    eprintln!("{} SIGTERM exit in {:?}", peer.name, started.elapsed());
    assert!(
        output.status.success(),
        "SIGTERM was not graceful: {:?}\n{}\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    assert!(!peer.path(".orchestry/orchestry.lock").exists());
    output
}

fn connected_shutdown_cancels_before_exit(watch: bool) {
    let worker = Peer::new("shutdown-worker");
    let controller = Peer::new("shutdown-controller");
    let agent = worker.add_agent("executor", &shutdown_gated_body(&worker), "shared");
    let worker_addr = port();
    let controller_addr = port();
    controller.ok(&["remote", "add", "w1", &worker_addr.to_string()]);
    worker.start(Some(worker_addr));
    let mut command = controller.command(env!("CARGO_BIN_EXE_orch"));
    if watch {
        command.args(["run", "--watch", "--verbose"]);
    } else {
        command.args(["serve", "--verbose"]);
    }
    command.args(["--listen", &controller_addr.to_string()]);
    *controller.owner.borrow_mut() = Some(controller.spawn(command, "shutdown-owner"));
    let peers = [&controller, &worker];
    controller.online("w1", &peers);
    let task = controller.task(
        "cancel before owner exits",
        Some(&remote_id(&agent, "w1")),
        &[],
    );
    wait("running remote shell", &peers, || {
        worker.path("started").exists()
    });
    let pid = text(worker.path("started")).trim().parse::<i32>().unwrap();

    let output = sigterm_owner(&controller);
    let worker_at_exit = worker.tasks();
    let shell_alive_at_exit = kill(Pid::from_raw(pid), None).is_ok();
    release(&worker);
    remains(
        "no effect after controller shutdown",
        Duration::from_millis(500),
        || !worker.path("effect").exists(),
    );
    assert!(
        !shell_alive_at_exit,
        "remote shell survived controller exit"
    );
    assert_eq!(
        worker_at_exit[0]["status"],
        "cancelled",
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert_eq!(controller.task_value(&task)["status"], "cancelled");
    assert!(
        read_json(worker.path(".orchestry/state.json"))["running"]
            .as_object()
            .unwrap()
            .is_empty()
    );
    drop(TcpListener::bind(controller_addr).expect("controller listener released"));
    sigterm_owner(&worker);
    drop(TcpListener::bind(worker_addr).expect("worker listener released"));
}

#[test]
fn s23_sigterm_offline_cancellation_is_bounded_reports_pending_and_sync_cancels() {
    let worker = Peer::new("shutdown-worker");
    let controller = Peer::new("shutdown-controller");
    let agent = worker.add_agent("executor", &shutdown_gated_body(&worker), "shared");
    connect(&controller, &worker, "w1", port());
    let peers = [&controller, &worker];
    let task = controller.task(
        "cancel while disconnected",
        Some(&remote_id(&agent, "w1")),
        &[],
    );
    wait("running remote shell", &peers, || {
        worker.path("started").exists()
    });
    let pid = text(worker.path("started")).trim().parse::<i32>().unwrap();
    fs::write(controller.path("peer-block"), "blocked").unwrap();
    wait("worker link offline", &peers, || {
        controller.status("w1")["online"] == false
    });

    let exec_key = controller.runs(&task)[0]["id"].as_str().unwrap().to_owned();
    let output = sigterm_owner(&controller);
    let log = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        log.contains("w1")
            && log.contains(&exec_key)
            && log.contains("outcome unknown")
            && log.contains("next Sync"),
        "offline shutdown must disclose the unacknowledged cancellation: {log}",
    );
    assert_eq!(controller.task_value(&task)["status"], "cancelled");
    assert!(
        kill(Pid::from_raw(pid), None).is_ok(),
        "disconnected worker should remain gated until Sync"
    );
    fs::remove_file(controller.path("peer-block")).unwrap();
    controller.start(None);
    controller.online("w1", &peers);
    wait("Sync cancels disconnected mirror", &peers, || {
        worker.tasks()[0]["status"] == "cancelled"
    });
    assert!(kill(Pid::from_raw(pid), None).is_err());
    release(&worker);
    remains(
        "no effect after reconnect cancellation",
        Duration::from_millis(500),
        || !worker.path("effect").exists(),
    );
}

#[test]
fn s17_worker_restart_loses_collector_without_repeating_surviving_shell_effect() {
    worker_restart_does_not_repeat(false);
}

#[test]
fn s17b_worker_restart_after_child_exit_does_not_repeat_effect() {
    worker_restart_does_not_repeat(true);
}

fn worker_restart_does_not_repeat(exit_before_restart: bool) {
    let worker = Peer::new("worker");
    let controller = Peer::new("controller");
    // No stdout after the owner dies: a broken pipe must not prevent the
    // original shell's side effect and hide the lost-collector regression.
    let body = format!(
        "echo $$ > {}\nwhile [ ! -f {} ]; do sleep 0.04; done\necho \"$ORCH_TASK_ID\" >> {}",
        quote(&worker.path("started")),
        quote(&worker.path("release")),
        quote(&worker.path("effect")),
    );
    let agent = worker.add_agent("executor", &body, "shared");
    let addr = port();
    connect(&controller, &worker, "w1", addr);
    let task = controller.task("uncertain worker exit", Some(&remote_id(&agent, "w1")), &[]);
    let peers = [&controller, &worker];
    wait("durable running shell", &peers, || {
        worker.path("started").exists()
            && read_json(worker.path(".orchestry/state.json"))["running"]
                .as_object()
                .is_some_and(|entries| entries.len() == 1)
    });
    let pid = text(worker.path("started")).trim().parse::<i32>().unwrap();
    let mirror = worker.tasks()[0]["id"].as_str().unwrap().to_owned();
    worker.stop(Signal::SIGKILL);
    assert!(
        kill(Pid::from_raw(pid), None).is_ok(),
        "shell did not survive owner kill"
    );
    if exit_before_restart {
        release(&worker);
        wait("original shell exits before restart", &peers, || {
            kill(Pid::from_raw(pid), None).is_err()
        });
    }
    worker.start(Some(addr));
    controller.online("w1", &peers);
    if !exit_before_restart {
        remains(
            "surviving shell remains reserved",
            Duration::from_millis(400),
            || {
                worker.task_value(&mirror)["status"] == "in_progress"
                    && controller.task_value(&task)["attempts"] == 1
            },
        );
        release(&worker);
    }
    let value = controller.wait_task(&task, "cancelled", &peers);
    assert_eq!(value["attempts"], 1);
    assert_eq!(controller.runs(&task).len(), 1);
    assert_eq!(controller.runs(&task)[0]["status"], "cancelled");
    assert_eq!(worker.tasks().len(), 1);
    assert_eq!(worker.task_value(&mirror)["status"], "cancelled");
    assert_eq!(text(worker.path("effect")), format!("{mirror}\n"));
    assert!(
        read_json(worker.path(".orchestry/state.json"))["running"]
            .as_object()
            .unwrap()
            .is_empty()
    );
    if !exit_before_restart {
        assert!(
            controller.runs(&task)[0]["error"]
                .as_str()
                .unwrap()
                .contains("outcome unknown")
        );
    }
    // A second owner restart and a new successful task are barriers proving
    // both durable cancellation and release of the original reservation.
    worker.stop(Signal::SIGKILL);
    worker.start(Some(addr));
    controller.online("w1", &peers);
    let next = controller.task("worker reusable", Some(&remote_id(&agent, "w1")), &[]);
    controller.wait_task(&next, "done", &peers);
    assert_eq!(controller.task_value(&task)["status"], "cancelled");
    assert_eq!(controller.runs(&task).len(), 1);
    let effects = text(worker.path("effect"));
    assert_eq!(effects.lines().filter(|line| *line == mirror).count(), 1);
    assert_eq!(effects.lines().count(), 2);
}

#[test]
fn s18_known_worker_application_failure_still_retries_on_controller() {
    let worker = Peer::new("worker");
    let controller = Peer::new("controller");
    let agent = worker.add_agent("executor", "echo known-failure >&2\nexit 7", "shared");
    connect(&controller, &worker, "w1", port());
    let task = controller.task("known failure", Some(&remote_id(&agent, "w1")), &[]);
    let value = controller.wait_task(&task, "failed", &[&controller, &worker]);
    assert_eq!(value["attempts"], 3);
    assert_eq!(controller.runs(&task).len(), 3);
    assert_eq!(worker.tasks().len(), 3);
    assert!(
        controller
            .runs(&task)
            .iter()
            .all(|run| run["status"] == "failed")
    );
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

// Regression: S2 — cap incomplete handshakes and retain the listening socket
// after transient descriptor exhaustion. Found by the security gate 2026-10-15.
// Python is used only at the OS boundary for child rlimits and RSS sampling;
// the actual protocol, sockets, engine and files are real and isolated.
const S2_RESOURCE_PROBE: &str = r###"# Standard library only: Python supplies portable child rlimits without unsafe Rust.
import json, os, re, resource, signal, socket, subprocess, sys, tempfile, threading, time
from pathlib import Path

binary = str(Path(sys.argv[1]).resolve())
mode = sys.argv[2]
result = {'scenario': mode, 'attempts': 80, 'partial_bytes': 1024*1024-64,
          'rss_watchdog_kib': 384*1024, 'wall_deadline_seconds': 55, 'cpu_limit_seconds': 25}

def rss(pid):
    return int(subprocess.check_output(['ps', '-o', 'rss=', '-p', str(pid)], text=True, timeout=2).strip())

def fds(pid):
    proc = Path('/proc')/str(pid)/'fd'
    if proc.exists():
        return len(list(proc.iterdir()))
    lines = subprocess.check_output(['/usr/sbin/lsof', '-nP', '-p', str(pid)], text=True, stderr=subprocess.DEVNULL, timeout=3).splitlines()[1:]
    return len({row.split()[3] for row in lines if len(row.split()) > 3 and re.match(r'^\d', row.split()[3])})

def await_condition(label, check, seconds=8):
    deadline = time.monotonic()+seconds
    while not check():
        assert owner.poll() is None, 'owner exited: '+log_path.read_text()
        assert time.monotonic() < deadline, label+': '+log_path.read_text()
        # Poll a real process/IO boundary, never assume readiness after a sleep.
        stop.wait(.02)

class Controller:
    def __init__(self, port, identity='controller'):
        self.s = socket.create_connection(('127.0.0.1', port), timeout=4)
        sockets.append(self.s)
        self.s.settimeout(4)
        self.f = self.s.makefile('rb')
        files.append(self.f)
        self.send({'t':'hello','proto':1,'instance_id':identity,'name':identity,'os':'linux','agents':[]})
        assert self.next()['t'] == 'welcome', 'valid reconnect did not receive Welcome'
        self.until('agents')
        self.send({'t':'sync','active':[]})
        assert self.until('sync_state')['items'] == []
    def send(self, frame):
        self.s.sendall(json.dumps(frame).encode()+b'\n')
    def next(self):
        line = self.f.readline()
        assert line, 'unexpected controller EOF'
        return json.loads(line)
    def until(self, kind):
        for _ in range(30):
            frame = self.next()
            if frame['t'] == kind:
                return frame
            if frame['t'] == 'ping':
                self.send({'t':'pong'})
        raise AssertionError('missing '+kind)
    def ping(self):
        self.send({'t':'ping'})
        self.until('pong')
    def close(self):
        self.f.close()
        self.s.close()

with tempfile.TemporaryDirectory(prefix='orch-s2-regression-') as temporary:
    root = Path(temporary)
    home = root/'home'
    home.mkdir()
    env = {k:v for k,v in os.environ.items() if not k.startswith(('ORCH','GIT_'))}
    env.update(HOME=str(home), XDG_CONFIG_HOME=str(home/'.config'),
        ORCHESTRY_GLOBAL_CONFIG_PATH=str(home/'global.yml'),
        GIT_CONFIG_NOSYSTEM='1', GIT_CONFIG_GLOBAL='/dev/null',
        RUST_LOG='warn', NO_COLOR='1', TERM='dumb')
    subprocess.run([binary,'init','--name','s2-regression','--adapter','shell'],
        cwd=root, env=env, check=True, capture_output=True, timeout=8)
    # No agents/tasks are created: no adapter or AI CLI can run.
    nofile = 128 if mode == 'flood' else 24
    result['nofile'] = nofile
    def limits():
        resource.setrlimit(resource.RLIMIT_NOFILE, (nofile,nofile))
        resource.setrlimit(resource.RLIMIT_CPU, (25,25))
    log_path = root/'owner.log'
    stop = threading.Event()
    sockets, files, held = [], [], []
    with log_path.open('w') as log:
        # Inherit the runner process group so its timeout guard also kills this child.
        owner = subprocess.Popen([binary,'serve','--listen','127.0.0.1:0','--peer-name','s2-worker'],
            cwd=root, env=env, stdout=log, stderr=log, preexec_fn=limits)
        result['owner_pid'] = owner.pid
        def watchdog():
            deadline = time.monotonic()+55
            while not stop.wait(.1) and owner.poll() is None:
                try:
                    usage = rss(owner.pid)
                    if usage > 384*1024 or time.monotonic() > deadline:
                        result['watchdog_tripped'] = {'rss_kib':usage}
                        owner.kill()
                        return
                except (subprocess.SubprocessError, ValueError):
                    return
        monitor = threading.Thread(target=watchdog)
        monitor.start()
        try:
            status = root/'.orchestry/remotes/_listener.json'
            await_condition('listener readiness', lambda: status.exists())
            port = int(json.loads(status.read_text())['listen_addr'].rsplit(':',1)[1])
            controller = Controller(port) if mode == 'flood' else None
            baseline = {'fds':fds(owner.pid),'rss_kib':rss(owner.pid)}
            result['baseline'] = baseline
            peak_rss = baseline['rss_kib']
            errors = 0
            payload = b'x' * result['partial_bytes']
            begin = time.monotonic()
            for i in range(80):
                try:
                    s = socket.create_connection(('127.0.0.1',port),timeout=.08)
                    sockets.append(s)
                    held.append(s)
                    s.settimeout(.04)
                    s.sendall(payload)
                except OSError:
                    errors += 1
                if i % 8 == 7 and owner.poll() is None:
                    peak_rss = max(peak_rss,rss(owner.pid))
            result.update(flood_seconds=round(time.monotonic()-begin,3),client_errors=errors)
            if mode == 'flood':
                # Ping is a processing barrier on the retained, admitted connection.
                controller.ping()
                result['admitted_controller_survived'] = True
                during = {'fds':fds(owner.pid),'rss_kib':rss(owner.pid)}
                peak_rss = max(peak_rss,during['rss_kib'])
                result.update(during=during,peak_rss_kib=peak_rss)
                assert during['fds']-baseline['fds'] <= 15, 'unbounded admitted sockets: '+str(result)
                assert peak_rss-baseline['rss_kib'] < 32*1024, 'unbounded partial-frame RSS: '+str(result)
            else:
                def retried():
                    return len(re.findall('Peer accept failed; retrying',log_path.read_text())) >= 3
                await_condition('real EMFILE retries',retried)
                logs = log_path.read_text()
                assert 'Too many open files' in logs, logs
                result['accept_errors'] = [line for line in logs.splitlines() if 'Peer accept failed; retrying' in line]
                assert len(result['accept_errors']) <= 8, 'accept retry hot loop'
                result['during'] = {'fds':fds(owner.pid),'rss_kib':rss(owner.pid)}
            for s in held:
                s.close()
            held.clear()
            await_condition('hostile sockets drained',lambda: fds(owner.pid) <= baseline['fds'])
            replacement = Controller(port)
            replacement.ping()
            result['reconnected_without_restart'] = True
            if controller:
                # Skip any keepalive sent just before takeover; require EOF fencing.
                while True:
                    line = controller.f.readline()
                    if not line:
                        break
                    assert json.loads(line)['t'] == 'ping', line
                controller.close()
                result['same_identity_takeover_fenced_old_controller'] = True
            replacement.close()
            if mode == 'flood':
                await_condition('controller disconnected',lambda: json.loads(status.read_text())['controller_name'] is None)
                partial = socket.create_connection(('127.0.0.1',port),timeout=2)
                sockets.append(partial)
                partial.sendall(b'{"t":')
                partial.settimeout(17)
                begin = time.monotonic()
                assert partial.recv(1) == b'', 'partial Hello never timed out'
                result['partial_eof_seconds'] = round(time.monotonic()-begin,3)
                assert 14 <= result['partial_eof_seconds'] <= 17, 'incorrect handshake timeout'
                final = Controller(port,'different-controller')
                final.ping()
                final.close()
                result['new_identity_after_partial_timeout'] = True
            assert owner.poll() is None, 'engine unexpectedly restarted/exited'
            assert 'watchdog_tripped' not in result, result
            result['passed'] = True
        finally:
            for f in files:
                f.close()
            for s in sockets:
                s.close()
            if owner.poll() is None:
                owner.kill()
            owner.wait(timeout=5)
            stop.set()
            monitor.join(timeout=3)
            result['owner_reaped'] = owner.poll() is not None
            result['monitor_stopped'] = not monitor.is_alive()
            result['owner_log'] = log_path.read_text()
            # Emit evidence on assertion failure too.
            print(json.dumps(result,indent=2),flush=True)
assert not root.exists(), 'temporary project leaked'
"###;

fn s2_resource_probe(scenario: &str) {
    let sandbox = Peer::new("s2-resource-supervisor");
    let mut command = sandbox.command("python3");
    command.args([
        "-c",
        S2_RESOURCE_PROBE,
        env!("CARGO_BIN_EXE_orch"),
        scenario,
    ]);
    let output = sandbox
        .spawn(command, "s2-probe")
        .output(Duration::from_secs(65));
    let stdout = String::from_utf8_lossy(&output.stdout);
    eprintln!("{stdout}");
    assert!(
        output.status.success(),
        "S2 {scenario}: {stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn s2_partial_handshake_flood_is_bounded_and_controller_takeover_recovers() {
    s2_resource_probe("flood");
}

#[test]
fn s2_transient_descriptor_exhaustion_retries_and_listener_recovers_without_restart() {
    s2_resource_probe("emfile");
}

fn registry_binary(peer: &Peer) -> PathBuf {
    let binary = peer.bin.join("registry-orch");
    fs::copy(env!("CARGO_BIN_EXE_orch"), &binary).expect("copy registry test binary");
    binary
}

fn registry_command(peer: &Peer, binary: &Path, args: &[&str]) -> Process {
    let mut command = peer.command(binary);
    command.args(args);
    peer.spawn(command, "registry")
}

fn registry_success(process: Process) -> String {
    let output = process.output(Duration::from_secs(15));
    assert!(
        output.status.success(),
        "registry command failed: {:?}\n{}\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

fn registry_aliases(peer: &Peer, binary: &Path) -> HashSet<String> {
    let output = registry_success(registry_command(
        peer,
        binary,
        &["remote", "list", "--json"],
    ));
    serde_json::from_str::<Vec<Value>>(&output)
        .unwrap()
        .iter()
        .map(|entry| entry["alias"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn registry_concurrent_mutations_preserve_successful_adds_and_unrelated_records_with_live_owner() {
    let peer = Peer::new("registry-concurrency");
    let binary = registry_binary(&peer);
    peer.start(None);
    let aliases: Vec<_> = (0..32).map(|n| format!("worker-{n}")).collect();
    let adds: Vec<_> = aliases
        .iter()
        .map(|alias| registry_command(&peer, &binary, &["remote", "add", alias, "127.0.0.1:9"]))
        .collect();
    for add in adds {
        registry_success(add);
    }
    assert_eq!(
        registry_aliases(&peer, &binary),
        aliases.iter().cloned().collect()
    );

    let mut changes = Vec::new();
    for alias in &aliases[..16] {
        changes.push(registry_command(
            &peer,
            &binary,
            &["remote", "remove", alias],
        ));
        changes.push(registry_command(
            &peer,
            &binary,
            &["remote", "add", &format!("new-{alias}"), "127.0.0.1:9"],
        ));
    }
    for change in changes {
        registry_success(change);
    }
    let expected = aliases[16..]
        .iter()
        .cloned()
        .chain(aliases[..16].iter().map(|alias| format!("new-{alias}")))
        .collect();
    assert_eq!(registry_aliases(&peer, &binary), expected);
}

#[test]
fn registry_killed_writer_releases_lock_without_stale_file_cleanup() {
    let peer = Peer::new("registry-crash");
    let binary = registry_binary(&peer);
    let registry = peer.path(".orchestry/remotes.yml");
    // Pause a real mutation after it acquires the lock, inside its read. No
    // timing window or production-only test hook is needed to kill the holder.
    let mut mkfifo = peer.command("mkfifo");
    mkfifo.arg(&registry);
    assert!(peer.spawn(mkfifo, "mkfifo").output(WAIT).status.success());
    let lock_path = peer.path(".orchestry/remotes.lock");
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .unwrap();
    let mut writer = registry_command(
        &peer,
        &binary,
        &["remote", "add", "interrupted", "127.0.0.1:9"],
    );
    wait_for(
        "writer holds registry lock",
        Duration::from_secs(5),
        &[&peer],
        || {
            assert!(writer.running(), "writer exited before acquiring the lock");
            match lock.try_lock() {
                Ok(()) => {
                    lock.unlock().unwrap();
                    false
                }
                Err(std::fs::TryLockError::WouldBlock) => true,
                Err(err) => panic!("observe registry lock: {err}"),
            }
        },
    );
    writer.terminate(Signal::SIGKILL);
    lock.try_lock()
        .expect("kernel releases killed writer's lock");
    lock.unlock().unwrap();
    assert!(lock_path.exists(), "lock file must remain a stable inode");
    fs::remove_file(&registry).unwrap();
    registry_success(registry_command(
        &peer,
        &binary,
        &["remote", "add", "after-crash", "127.0.0.1:9"],
    ));
    assert_eq!(
        registry_aliases(&peer, &binary),
        HashSet::from(["after-crash".into()])
    );
}

#[test]
fn registry_malformed_input_fails_loudly_without_overwrite_and_releases_lock() {
    let peer = Peer::new("registry-malformed");
    let binary = registry_binary(&peer);
    let registry = peer.path(".orchestry/remotes.yml");
    for malformed in [
        "[broken YAML",
        r#"[{"alias":"../bad","addr":"127.0.0.1:9","added_at":"now"}]"#,
        r#"[{"alias":"same","addr":"127.0.0.1:9","added_at":"now"},{"alias":"same","addr":"127.0.0.1:9","added_at":"now"}]"#,
    ] {
        fs::write(&registry, malformed).unwrap();
        for args in [
            vec!["remote", "add", "new", "127.0.0.1:9"],
            vec!["remote", "remove", "same"],
        ] {
            let output = registry_command(&peer, &binary, &args).output(WAIT);
            assert!(!output.status.success(), "accepted malformed registry");
            assert!(!output.stderr.is_empty(), "registry error must be visible");
            assert_eq!(fs::read(&registry).unwrap(), malformed.as_bytes());
        }
    }
    fs::write(&registry, "[]").unwrap();
    registry_success(registry_command(
        &peer,
        &binary,
        &["remote", "add", "repaired", "127.0.0.1:9"],
    ));
    assert_eq!(
        registry_aliases(&peer, &binary),
        HashSet::from(["repaired".into()])
    );
}

#[test]
fn registry_lock_open_failure_is_visible_and_preserves_existing_records() {
    let peer = Peer::new("registry-lock-error");
    let binary = registry_binary(&peer);
    registry_success(registry_command(
        &peer,
        &binary,
        &["remote", "add", "existing", "127.0.0.1:9"],
    ));
    let lock_path = peer.path(".orchestry/remotes.lock");
    if lock_path.exists() {
        fs::remove_file(&lock_path).unwrap();
    }
    fs::create_dir(&lock_path).unwrap();
    let registry = peer.path(".orchestry/remotes.yml");
    let before = fs::read(&registry).unwrap();
    for args in [
        vec!["remote", "add", "new", "127.0.0.1:9"],
        vec!["remote", "remove", "existing"],
    ] {
        let output = registry_command(&peer, &binary, &args).output(WAIT);
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("registry lock"));
        assert_eq!(fs::read(&registry).unwrap(), before);
    }
}
