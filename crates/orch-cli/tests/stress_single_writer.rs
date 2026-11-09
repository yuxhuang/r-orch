//! The concurrency proof (T2a; spec R13, R24, R27; design D12
//! `daemon_stress::all_clients_share_state_order`; goal "Tests and
//! acceptance"): D10 is closed because ONE daemon is the only writer.
//!
//! One project, one explicit daemon (dispatch ON, a `true` worker so the
//! tick, the run collector and the 1 s context-expiry sweep all write while
//! the clients do). For `ORCH_STRESS_SECS` seconds (default 10; the D10
//! closure evidence is the 60 s soak, `ORCH_STRESS_SECS=60`) these mutate it
//! at once:
//!
//! * a real `orch tui` in a private tmux, creating tasks through the wizard;
//! * four CLI clients (real `orch` processes, looping): three write ONE field
//!   each of one shared hot task (title / description / labels), the fourth
//!   creates tasks, goals, messages and short-lived context keys;
//! * two MCP clients (real `orch mcp` bridges): one writes the hot task's
//!   priority, the other adds tasks and context and **renews** a short-TTL
//!   key every ~150 ms, right across the expiry sweeps;
//! * an in-process `orch-client` observer (the web-equivalent reader) that
//!   re-reads the hot task and the daemon's `commit_seq` all the time.
//!
//! Then the clients stop, the daemon is stopped, and the files are judged
//! offline:
//!
//! 1. **entity files == index** for every indexed store (tasks, goals, agents,
//!    messages, context), id for id and byte-for-meaning;
//! 2. **no lost field update, observed in order**: every counter the observer
//!    ever read is >= what its writer had acknowledged before the read
//!    started, never > what it had attempted, never decreasing, and the
//!    daemon's `commit_seq` never decreases; the final value of each field is
//!    its writer's last acknowledged write;
//! 3. **nothing acknowledged is missing**: every id a client was told about
//!    exists on disk, in the entity file and the index;
//! 4. **no context key lost on renewal**: the renewed key survives every
//!    sweep, while keys nobody renewed are gone (expiry really ran);
//! 5. **no swallowed errors**: no client operation failed and the daemon's
//!    log carries no error line.

mod daemon_support;

use daemon_support::frontends::*;
use daemon_support::*;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

fn stress_duration() -> Duration {
    let secs = std::env::var("ORCH_STRESS_SECS")
        .ok()
        .and_then(|raw| raw.parse().ok())
        .unwrap_or(10);
    Duration::from_secs(secs)
}

/// The renewed key's TTL; the sweep runs every 1 s, so it is swept ~once
/// between renewals' lifetimes and must never be collected while renewed.
const LIVE_TTL_MS: u64 = 1500;
const SHORT_TTL_MS: u64 = 1000;

// ── shared bookkeeping ─────────────────────────────────────────────────────

#[derive(Default)]
struct Writer {
    /// Highest counter whose write was sent / acknowledged.
    attempted: AtomicU64,
    acked: AtomicU64,
}

struct Shared {
    stop: AtomicBool,
    title: Writer,
    description: Writer,
    labels: Writer,
    priority: Writer,
    live: Writer,
    failures: Mutex<Vec<String>>,
    created: Mutex<Created>,
}

#[derive(Default)]
struct Created {
    tasks: BTreeSet<String>,
    goals: BTreeSet<String>,
    messages: BTreeSet<String>,
    context: BTreeSet<String>,
    /// (key, unix-ms when set) of the never-renewed short keys.
    short_keys: Vec<(String, Instant)>,
    tui_titles: Vec<String>,
}

impl Shared {
    fn fail(&self, what: impl Into<String>) {
        self.failures.lock().unwrap().push(what.into());
    }
    fn stopped(&self) -> bool {
        self.stop.load(Ordering::SeqCst)
    }
}

fn priority_of(n: u64) -> u64 {
    1 + (n % 4)
}

fn patch_for(client: &str, n: u64) -> Value {
    match client {
        "title" => json!({"title": format!("c1-{n}")}),
        "description" => json!({"description": format!("c2-{n}")}),
        "labels" => json!({"labels": [format!("c3-{n}")]}),
        "priority" => json!({"priority": priority_of(n)}),
        other => panic!("unknown writer {other}"),
    }
}

// ── CLI clients ────────────────────────────────────────────────────────────

fn cli(sandbox: &Sandbox, args: &[&str]) -> Result<String, String> {
    let out = sandbox.run(args);
    if out.status.success() {
        Ok(text(&out.stdout).trim().to_owned())
    } else {
        Err(format!("orch {args:?}: {}", text(&out.stderr)))
    }
}

fn spawn_field_writer(
    sandbox: Arc<Sandbox>,
    shared: Arc<Shared>,
    patch_dir: PathBuf,
    hot: String,
    field: &'static str,
    counter: fn(&Shared) -> &Writer,
) -> JoinHandle<()> {
    std::thread::spawn(move || {
        let file = patch_dir.join(format!("{field}.json"));
        let mut n = 0_u64;
        while !shared.stopped() {
            n += 1;
            fs::write(&file, patch_for(field, n).to_string()).unwrap();
            counter(&shared).attempted.store(n, Ordering::SeqCst);
            match cli(
                &sandbox,
                &[
                    "task",
                    "patch",
                    &hot,
                    "--file",
                    file.to_str().unwrap(),
                    "--quiet",
                ],
            ) {
                Ok(_) => counter(&shared).acked.store(n, Ordering::SeqCst),
                Err(err) => {
                    shared.fail(format!("cli {field} #{n}: {err}"));
                    return;
                }
            }
        }
    })
}

fn spawn_cli_creator(sandbox: Arc<Sandbox>, shared: Arc<Shared>, worker: String) -> JoinHandle<()> {
    std::thread::spawn(move || {
        let mut n = 0_u64;
        while !shared.stopped() {
            n += 1;
            let step = |what: &str, result: Result<String, String>| -> Option<String> {
                result
                    .map_err(|err| shared.fail(format!("cli creator {what} #{n}: {err}")))
                    .ok()
            };
            // Every third task is runnable by the worker, so the tick and
            // the collector write too.
            let title = format!("c4-{n}");
            let mut task_args = vec!["task", "add", &title, "-d", "created by cli 4", "--quiet"];
            let assignee_title = format!("w-{n}");
            if n.is_multiple_of(3) {
                task_args = vec![
                    "task",
                    "add",
                    &assignee_title,
                    "--assignee",
                    &worker,
                    "--quiet",
                ];
            }
            let Some(task) = step("task add", cli(&sandbox, &task_args)) else {
                return;
            };
            let goal_title = format!("g-{n}");
            let Some(goal) = step(
                "goal add",
                cli(&sandbox, &["goal", "add", &goal_title, "--quiet"]),
            ) else {
                return;
            };
            let body = format!("message {n}");
            let Some(message) = step(
                "msg send",
                cli(
                    &sandbox,
                    &["msg", "send", &worker, &body, "--from", "cli", "--quiet"],
                ),
            ) else {
                return;
            };
            let key = format!("short-{n}");
            let ttl = SHORT_TTL_MS.to_string();
            let set = cli(&sandbox, &["context", "set", &key, "v", "--ttl", &ttl]);
            if step("context set", set).is_none() {
                return;
            }
            let mut created = shared.created.lock().unwrap();
            created.tasks.insert(task);
            created.goals.insert(goal);
            created.messages.insert(message);
            created.short_keys.push((key, Instant::now()));
        }
    })
}

// ── MCP clients ────────────────────────────────────────────────────────────

fn mcp_ok(response: &Value) -> Result<Value, String> {
    if response.get("error").is_some() || response["result"]["isError"] == json!(true) {
        return Err(response.to_string());
    }
    let text = response["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or("null");
    Ok(serde_json::from_str(text).unwrap_or(Value::Null))
}

fn mcp_client(sandbox: &Sandbox) -> Mcp {
    let mut mcp = Mcp::spawn(sandbox.orch(), &[]);
    let init = mcp.initialize();
    assert!(
        init.get("error").is_none(),
        "mcp initialize failed: {init}; {}",
        mcp.stderr()
    );
    mcp.notify("notifications/initialized", json!({}));
    mcp
}

fn spawn_mcp_priority_writer(
    sandbox: Arc<Sandbox>,
    shared: Arc<Shared>,
    patch_dir: PathBuf,
    hot: String,
) -> JoinHandle<()> {
    std::thread::spawn(move || {
        let mut mcp = mcp_client(&sandbox);
        let file = patch_dir.join("priority.json");
        let mut n = 0_u64;
        while !shared.stopped() {
            n += 1;
            fs::write(&file, patch_for("priority", n).to_string()).unwrap();
            shared.priority.attempted.store(n, Ordering::SeqCst);
            let response = mcp.call_tool(
                "task_patch",
                json!({"id": hot, "file": file.to_str().unwrap()}),
            );
            match mcp_ok(&response) {
                Ok(_) => shared.priority.acked.store(n, Ordering::SeqCst),
                Err(err) => {
                    shared.fail(format!("mcp priority #{n}: {err}"));
                    return;
                }
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        mcp.finish();
    })
}

fn spawn_mcp_creator(sandbox: Arc<Sandbox>, shared: Arc<Shared>) -> JoinHandle<()> {
    std::thread::spawn(move || {
        let mut mcp = mcp_client(&sandbox);
        let mut n = 0_u64;
        while !shared.stopped() {
            n += 1;
            let result = (|| -> Result<(), String> {
                let task = mcp_ok(&mcp.call_tool("task_add", json!({"title": format!("m2-{n}")})))?;
                let id = task["id"]
                    .as_str()
                    .ok_or("task_add returned no id")?
                    .to_owned();
                let key = format!("keep-m2-{n}");
                mcp_ok(&mcp.call_tool("context_set", json!({"key": key, "value": "kept"})))?;
                // The renewal under test: a TTL'd key re-set before each sweep
                // could collect it.
                shared.live.attempted.store(n, Ordering::SeqCst);
                mcp_ok(&mcp.call_tool(
                    "context_set",
                    json!({"key": "live", "value": format!("live-{n}"), "ttl": LIVE_TTL_MS}),
                ))?;
                shared.live.acked.store(n, Ordering::SeqCst);
                let mut created = shared.created.lock().unwrap();
                created.tasks.insert(id);
                created.context.insert(key);
                Ok(())
            })();
            if let Err(err) = result {
                shared.fail(format!("mcp creator #{n}: {err}"));
                return;
            }
            std::thread::sleep(Duration::from_millis(150));
        }
        mcp.finish();
    })
}

// ── in-process orch-client writers (the daemon-only variant) ──────────────

fn client_config(
    sandbox: &Sandbox,
    kind: orch_proto::handshake::CallerKind,
) -> orch_client::ClientConfig {
    let root = sandbox.root();
    orch_client::ClientConfig {
        cwd: root.clone(),
        project_root: Some(root),
        socket: Some(sandbox.socket()),
        test_socket_root: Some(sandbox.short_dir().to_path_buf()),
        caller: orch_proto::handshake::Caller {
            kind,
            agent_id: None,
            pid: None,
        },
        mode: orch_proto::handshake::SessionMode::Normal,
        agent_context: false,
    }
}

/// Run `work` on its own thread with its own `orch-client` connection and a
/// current-thread runtime: one more kind of client on the same socket.
fn spawn_client_thread<F>(
    sandbox: &Sandbox,
    shared: Arc<Shared>,
    name: &'static str,
    work: F,
) -> JoinHandle<()>
where
    F: for<'a> FnOnce(
            &'a mut orch_client::Client,
            &'a Shared,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<(), orch_client::ClientError>> + 'a>,
        > + Send
        + 'static,
{
    let config = client_config(sandbox, orch_proto::handshake::CallerKind::Cli);
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async move {
            let result = async {
                let mut client = orch_client::Client::connect(&config).await?;
                work(&mut client, &shared).await
            }
            .await;
            if let Err(err) = result {
                shared.fail(format!("{name}: {err:?}"));
            }
        });
    })
}

fn new_task(title: &str, assignee: Option<&str>) -> orch_proto::ops::args::TaskCreateArgs {
    orch_proto::ops::args::TaskCreateArgs {
        title: short(title),
        description: None,
        priority: None,
        assignee: assignee.map(|id| orch_proto::bounded::Id::new(id).unwrap()),
        labels: None,
        depends_on: None,
        max_attempts: None,
        workspace_mode: None,
        review_criteria: None,
        scope: None,
        goal_id: None,
        attachments: None,
    }
}

fn short(text: &str) -> orch_proto::bounded::ShortText {
    orch_proto::bounded::ShortText::new(text).unwrap()
}

fn spawn_client_field_writer(
    sandbox: &Sandbox,
    shared: Arc<Shared>,
    hot: String,
    field: &'static str,
    counter: fn(&Shared) -> &Writer,
) -> JoinHandle<()> {
    use orch_proto::bounded::{BoundedVec, Id, Text};
    use orch_proto::ops::args::TaskUpdateFieldsArgs;
    spawn_client_thread(sandbox, shared, field, move |client, shared| {
        Box::pin(async move {
            let mut n = 0_u64;
            while !shared.stopped() {
                n += 1;
                let mut args = TaskUpdateFieldsArgs {
                    id: Id::new(hot.clone()).unwrap(),
                    title: None,
                    description: None,
                    priority: None,
                    labels: None,
                    attachments: None,
                };
                match field {
                    "title" => args.title = Some(short(&format!("c1-{n}"))),
                    "description" => {
                        args.description = Some(Text::new(format!("c2-{n}")).unwrap());
                    }
                    "labels" => {
                        args.labels =
                            Some(BoundedVec::new(vec![short(&format!("c3-{n}"))]).unwrap());
                    }
                    _ => args.priority = Some(i64::try_from(priority_of(n)).unwrap()),
                }
                counter(shared).attempted.store(n, Ordering::SeqCst);
                client.task_update_fields(args).await?;
                counter(shared).acked.store(n, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(3)).await;
            }
            Ok(())
        })
    })
}

fn spawn_client_creator(sandbox: &Sandbox, shared: Arc<Shared>, worker: String) -> JoinHandle<()> {
    use orch_proto::bounded::{Id, Text};
    use orch_proto::ops::args::{ContextSetArgs, GoalCreateArgs, MessageSendArgs};
    spawn_client_thread(sandbox, shared, "creator", move |client, shared| {
        Box::pin(async move {
            let mut n = 0_u64;
            while !shared.stopped() {
                n += 1;
                let task = client
                    .task_create(new_task(
                        &format!("c4-{n}"),
                        n.is_multiple_of(3).then_some(worker.as_str()),
                    ))
                    .await?;
                let goal = client
                    .goal_create(GoalCreateArgs {
                        title: short(&format!("g-{n}")),
                        description: None,
                        assignee: None,
                    })
                    .await?;
                let messages = client
                    .message_send(MessageSendArgs {
                        from_agent_id: Id::new("cli").unwrap(),
                        to_agent_id: Id::new(worker.clone()).unwrap(),
                        subject: short(""),
                        body: Text::new(format!("message {n}")).unwrap(),
                        ttl_ms: None,
                        reply_to: None,
                    })
                    .await?;
                let key = format!("short-{n}");
                client
                    .context_set(ContextSetArgs {
                        key: Id::new(key.clone()).unwrap(),
                        value: Text::new("v").unwrap(),
                        ttl_ms: Some(SHORT_TTL_MS),
                    })
                    .await?;
                {
                    let mut created = shared.created.lock().unwrap();
                    created.tasks.insert(task.id);
                    created.goals.insert(goal.id);
                    created
                        .messages
                        .extend(messages.into_iter().map(|message| message.id));
                    created.short_keys.push((key, Instant::now()));
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            Ok(())
        })
    })
}

fn spawn_client_renewer(sandbox: &Sandbox, shared: Arc<Shared>) -> JoinHandle<()> {
    use orch_proto::bounded::{Id, Text};
    use orch_proto::ops::args::ContextSetArgs;
    spawn_client_thread(sandbox, shared, "renewer", move |client, shared| {
        Box::pin(async move {
            let mut n = 0_u64;
            while !shared.stopped() {
                n += 1;
                let task = client
                    .task_create(new_task(&format!("m2-{n}"), None))
                    .await?;
                let key = format!("keep-m2-{n}");
                client
                    .context_set(ContextSetArgs {
                        key: Id::new(key.clone()).unwrap(),
                        value: Text::new("kept").unwrap(),
                        ttl_ms: None,
                    })
                    .await?;
                shared.live.attempted.store(n, Ordering::SeqCst);
                client
                    .context_set(ContextSetArgs {
                        key: Id::new("live").unwrap(),
                        value: Text::new(format!("live-{n}")).unwrap(),
                        ttl_ms: Some(LIVE_TTL_MS),
                    })
                    .await?;
                shared.live.acked.store(n, Ordering::SeqCst);
                {
                    let mut created = shared.created.lock().unwrap();
                    created.tasks.insert(task.id);
                    created.context.insert(key);
                }
                tokio::time::sleep(Duration::from_millis(150)).await;
            }
            Ok(())
        })
    })
}

// ── the observer (a web-equivalent reader over orch-client) ───────────────

#[derive(Default)]
struct ObserverReport {
    reads: u64,
    violations: Vec<String>,
    last_commit_seq: u64,
}

fn parse_counter(prefix: &str, value: &str) -> Option<u64> {
    value.strip_prefix(prefix)?.parse().ok()
}

fn spawn_observer(
    sandbox: &Sandbox,
    shared: Arc<Shared>,
    hot: String,
) -> JoinHandle<ObserverReport> {
    let config = client_config(sandbox, orch_proto::handshake::CallerKind::Web);
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async move {
            let mut report = ObserverReport::default();
            let mut client = match orch_client::Client::connect(&config).await {
                Ok(client) => client,
                Err(err) => {
                    report.violations.push(format!("observer connect: {err:?}"));
                    return report;
                }
            };
            let id = orch_proto::bounded::Id::new(hot).unwrap();
            let mut floor = (0_u64, 0_u64, 0_u64);
            while !shared.stopped() {
                // What was already acknowledged BEFORE this read started.
                let acked = (
                    shared.title.acked.load(Ordering::SeqCst),
                    shared.description.acked.load(Ordering::SeqCst),
                    shared.labels.acked.load(Ordering::SeqCst),
                    shared.priority.acked.load(Ordering::SeqCst),
                );
                let task = match client
                    .task_get(orch_proto::ops::args::TaskGetArgs { id: id.clone() })
                    .await
                {
                    Ok(task) => task,
                    Err(err) => {
                        report.violations.push(format!("observer read: {err:?}"));
                        return report;
                    }
                };
                report.reads += 1;
                let seq = client.last_commit_seq().unwrap_or(0);
                if seq < report.last_commit_seq {
                    report.violations.push(format!(
                        "commit_seq went backwards: {} after {}",
                        seq, report.last_commit_seq
                    ));
                }
                report.last_commit_seq = report.last_commit_seq.max(seq);
                let seen = (
                    parse_counter("c1-", &task.title),
                    parse_counter("c2-", &task.description),
                    task.labels.first().and_then(|l| parse_counter("c3-", l)),
                );
                let attempted = (
                    shared.title.attempted.load(Ordering::SeqCst),
                    shared.description.attempted.load(Ordering::SeqCst),
                    shared.labels.attempted.load(Ordering::SeqCst),
                );
                for (name, seen, acked, attempted, floor) in [
                    ("title", seen.0, acked.0, attempted.0, &mut floor.0),
                    ("description", seen.1, acked.1, attempted.1, &mut floor.1),
                    ("labels", seen.2, acked.2, attempted.2, &mut floor.2),
                ] {
                    let Some(seen) = seen else {
                        report.violations.push(format!("{name}: unparsable value"));
                        continue;
                    };
                    if seen < acked {
                        report.violations.push(format!(
                            "LOST UPDATE: {name} reads {seen} after {acked} was acknowledged (commit_seq {seq})"
                        ));
                    }
                    if seen < *floor {
                        report.violations.push(format!(
                            "{name} went backwards: {seen} after {floor} (commit_seq {seq})"
                        ));
                    }
                    if seen > attempted {
                        report.violations.push(format!(
                            "{name} reads {seen}, a value never sent (attempted {attempted})"
                        ));
                    }
                    *floor = (*floor).max(seen);
                }
                let allowed = [priority_of(acked.3), priority_of(acked.3 + 1)];
                let priority = u64::try_from(task.priority).unwrap_or(0);
                if !allowed.contains(&priority) && !(acked.3 == 0 && priority == priority_of(0)) {
                    report.violations.push(format!(
                        "LOST UPDATE: priority reads {priority} after write #{} was acknowledged \
                         (expected one of {allowed:?}, commit_seq {seq})",
                        acked.3
                    ));
                }
                tokio::time::sleep(Duration::from_millis(15)).await;
            }
            report
        })
    })
}

// ── the TUI client ─────────────────────────────────────────────────────────

fn spawn_tui_driver(pane: Arc<TuiPane>, shared: Arc<Shared>) -> JoinHandle<()> {
    std::thread::spawn(move || {
        pane.keys(&["t"]);
        let mut n = 0_u64;
        while !shared.stopped() {
            n += 1;
            let title = format!("tui-{n}");
            let step = |ready: &dyn Fn(&str) -> bool, what: &str| pane.wait_screen(what, ready);
            pane.keys(&["n"]);
            step(&|s| s.contains("Task title"), "the title step");
            pane.type_text(&title);
            step(&|s| s.contains(&format!("> {title}")), "the typed title");
            pane.keys(&["Enter"]);
            step(&|s| s.contains("Priority"), "the priority step");
            pane.keys(&["Enter"]);
            step(&|s| s.contains("Assignee"), "the assignee step");
            pane.keys(&["Enter"]);
            step(&|s| s.contains("Description"), "the description step");
            pane.keys(&["Tab"]);
            step(
                &|s| !s.contains("NEW TASK — step") && !s.contains("No orch daemon"),
                "the wizard to close",
            );
            shared.created.lock().unwrap().tui_titles.push(title);
            std::thread::sleep(Duration::from_millis(800));
        }
    })
}

// ── offline judgement ──────────────────────────────────────────────────────

/// `(id → record)` of an entity directory's files.
fn entities(dir: &Path, ext: &str) -> BTreeMap<String, Value> {
    let mut out = BTreeMap::new();
    for entry in fs::read_dir(dir).unwrap().flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') || name == "_index.json" || !name.ends_with(ext) {
            continue;
        }
        let body = fs::read_to_string(entry.path()).unwrap();
        let value: Value = if ext == ".yml" {
            serde_yaml_ng::from_str(&body).unwrap_or_else(|e| panic!("{name}: {e}"))
        } else {
            serde_json::from_str(&body).unwrap_or_else(|e| panic!("{name}: {e}"))
        };
        out.insert(name.trim_end_matches(ext).to_owned(), value);
    }
    out
}

fn index(dir: &Path, key: &str) -> BTreeMap<String, Value> {
    let body = fs::read_to_string(dir.join("_index.json")).unwrap();
    let rows: Vec<Value> = serde_json::from_str(&body).unwrap();
    rows.into_iter()
        .map(|row| (row[key].as_str().unwrap().to_owned(), row))
        .collect()
}

/// Entity files and index agree, id for id, and each indexed record is the
/// record in the file.
fn assert_store_consistent(
    state: &Path,
    store: &str,
    ext: &str,
    key: &str,
) -> BTreeMap<String, Value> {
    let dir = state.join(store);
    let files = entities(&dir, ext);
    let indexed = index(&dir, key);
    let only_files: Vec<_> = files.keys().filter(|k| !indexed.contains_key(*k)).collect();
    let only_index: Vec<_> = indexed.keys().filter(|k| !files.contains_key(*k)).collect();
    assert!(
        only_files.is_empty() && only_index.is_empty(),
        "{store}: entity files and index disagree: {} files not indexed {only_files:?}; \
         {} indexed without a file {only_index:?}",
        only_files.len(),
        only_index.len()
    );
    for (id, record) in &files {
        assert_eq!(
            &indexed[id], record,
            "{store}/{id}: index row != entity file"
        );
    }
    files
}

/// Which clients mutate the project.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Fleet {
    /// The real thing: `orch tui` in tmux, four `orch` CLI processes, two
    /// `orch mcp` bridges.
    Processes,
    /// The same operations from in-process `orch-client` connections: four
    /// field writers, a creator and a renewer. No process fan-out, so it is
    /// quick and does not depend on the frontend cutover.
    InProcess,
}

struct Fixtures {
    worker: String,
    parked: String,
    hot: String,
}

fn fixtures(sandbox: &Sandbox) -> Fixtures {
    let worker = cli(
        sandbox,
        &[
            "agent",
            "add",
            "worker",
            "--adapter",
            "shell",
            "--command",
            "true",
            "--quiet",
        ],
    )
    .unwrap();
    let parked = cli(
        sandbox,
        &[
            "agent",
            "add",
            "parked",
            "--adapter",
            "shell",
            "--command",
            "true",
            "--quiet",
        ],
    )
    .unwrap();
    cli(sandbox, &["agent", "disable", &parked]).unwrap();
    // The hot task is assigned to the disabled agent, so the dispatcher never
    // runs it: only the clients write its fields.
    let hot = cli(
        sandbox,
        &[
            "task",
            "add",
            "c1-0",
            "-d",
            "c2-0",
            "-l",
            "c3-0",
            "-p",
            "1",
            "--assignee",
            &parked,
            "--quiet",
        ],
    )
    .unwrap();
    for i in 0..4 {
        cli(sandbox, &["context", "set", &format!("keep-{i}"), "seed"]).unwrap();
    }
    cli(
        sandbox,
        &[
            "context",
            "set",
            "live",
            "live-0",
            "--ttl",
            &LIVE_TTL_MS.to_string(),
        ],
    )
    .unwrap();
    Fixtures {
        worker,
        parked,
        hot,
    }
}

fn run_stress(fleet: Fleet) {
    let sandbox = Arc::new(Sandbox::new());
    sandbox.init_project();
    let mut daemon = start_ready(&sandbox, &["--dispatch", "on"], "owner");
    let Fixtures {
        worker,
        parked,
        hot,
    } = fixtures(&sandbox);

    let shared = Arc::new(Shared {
        stop: AtomicBool::new(false),
        title: Writer::default(),
        description: Writer::default(),
        labels: Writer::default(),
        priority: Writer::default(),
        live: Writer::default(),
        failures: Mutex::default(),
        created: Mutex::default(),
    });
    let patch_dir = sandbox.root().join("stress-patches");
    fs::create_dir(&patch_dir).unwrap();

    // ── launch every client ──
    let mut pane = None;
    let mut threads = Vec::new();
    match fleet {
        Fleet::Processes => {
            let tui = Arc::new(TuiPane::launch_at(
                &sandbox.root(),
                &sandbox.socket(),
                sandbox.short_dir(),
                "tui",
                &[],
            ));
            tui.wait_screen("the dashboard", |s| {
                s.contains("ORCH") && !s.contains("No orch daemon")
            });
            for (field, counter) in [
                ("title", (|s: &Shared| &s.title) as fn(&Shared) -> &Writer),
                ("description", |s| &s.description),
                ("labels", |s| &s.labels),
            ] {
                threads.push(spawn_field_writer(
                    sandbox.clone(),
                    shared.clone(),
                    patch_dir.clone(),
                    hot.clone(),
                    field,
                    counter,
                ));
            }
            threads.push(spawn_cli_creator(
                sandbox.clone(),
                shared.clone(),
                worker.clone(),
            ));
            threads.push(spawn_mcp_priority_writer(
                sandbox.clone(),
                shared.clone(),
                patch_dir.clone(),
                hot.clone(),
            ));
            threads.push(spawn_mcp_creator(sandbox.clone(), shared.clone()));
            threads.push(spawn_tui_driver(tui.clone(), shared.clone()));
            pane = Some(tui);
        }
        Fleet::InProcess => {
            for (field, counter) in [
                ("title", (|s: &Shared| &s.title) as fn(&Shared) -> &Writer),
                ("description", |s| &s.description),
                ("labels", |s| &s.labels),
                ("priority", |s| &s.priority),
            ] {
                threads.push(spawn_client_field_writer(
                    &sandbox,
                    shared.clone(),
                    hot.clone(),
                    field,
                    counter,
                ));
            }
            threads.push(spawn_client_creator(
                &sandbox,
                shared.clone(),
                worker.clone(),
            ));
            threads.push(spawn_client_renewer(&sandbox, shared.clone()));
        }
    }
    let started = Instant::now();
    let observer = spawn_observer(&sandbox, shared.clone(), hot.clone());

    // ── let them fight ──
    let duration = stress_duration();
    while started.elapsed() < duration && shared.failures.lock().unwrap().is_empty() {
        std::thread::sleep(Duration::from_millis(100));
    }
    shared.stop.store(true, Ordering::SeqCst);
    let mut panicked = Vec::new();
    for thread in threads {
        if let Err(payload) = thread.join() {
            panicked.push(format!("{payload:?}"));
        }
    }
    let report = observer.join().unwrap();

    // The TUI survived the whole run on the daemon.
    if let Some(pane) = &pane {
        let screen = pane.screen();
        assert!(
            !screen.contains("No orch daemon"),
            "the TUI lost the daemon:\n{screen}"
        );
        pane.keys(&["q"]);
        assert_eq!(
            pane.exit_code(),
            0,
            "the TUI did not quit cleanly:\n{}",
            pane.screen()
        );
    }

    // ── stop the owner so every write is on disk, then judge offline ──
    let stop = sandbox.run(&["daemon", "stop"]);
    assert!(stop.status.success(), "{}", text(&stop.stderr));
    let status = daemon.wait_exit(READY);
    assert_eq!(status.code(), Some(0), "daemon: {}", daemon.stderr());

    let failures = shared.failures.lock().unwrap().clone();
    assert!(panicked.is_empty(), "client threads panicked: {panicked:?}");
    assert!(
        failures.is_empty(),
        "client operations failed: {failures:#?}"
    );
    assert!(
        report.reads > 20,
        "the observer barely ran: {} reads",
        report.reads
    );
    assert!(
        report.violations.is_empty(),
        "{} ordering violations in {} reads, first: {:#?}",
        report.violations.len(),
        report.reads,
        &report.violations[..report.violations.len().min(10)]
    );

    judge(&sandbox, &shared, &hot, &worker, &parked, duration, fleet);
    let log = daemon.stderr();
    let bad: Vec<_> = log
        .lines()
        .filter(|line| {
            let line = line.to_ascii_lowercase();
            line.contains("error") || line.contains("panicked")
        })
        .collect();
    assert!(bad.is_empty(), "daemon log carries errors: {bad:#?}");
}

fn judge(
    sandbox: &Sandbox,
    shared: &Shared,
    hot: &str,
    worker: &str,
    parked: &str,
    duration: Duration,
    fleet: Fleet,
) {
    let state = sandbox.state_dir();
    let tasks = assert_store_consistent(&state, "tasks", ".yml", "id");
    let goals = assert_store_consistent(&state, "goals", ".yml", "id");
    let agents = assert_store_consistent(&state, "agents", ".yml", "id");
    let messages = assert_store_consistent(&state, "messages", ".json", "id");
    let context = assert_store_consistent(&state, "context", ".json", "key");

    // The hot task: the last acknowledged write of each field is its value.
    let acked = |writer: &Writer| writer.acked.load(Ordering::SeqCst);
    let hot_task = &tasks[hot];
    assert_eq!(hot_task["title"], format!("c1-{}", acked(&shared.title)));
    assert_eq!(
        hot_task["description"],
        format!("c2-{}", acked(&shared.description))
    );
    assert_eq!(
        hot_task["labels"],
        json!([format!("c3-{}", acked(&shared.labels))])
    );
    assert_eq!(
        hot_task["priority"],
        json!(priority_of(acked(&shared.priority)))
    );
    assert_eq!(
        hot_task["status"], "todo",
        "the dispatcher touched the hot task"
    );
    for (name, writer) in [
        ("title", &shared.title),
        ("description", &shared.description),
        ("labels", &shared.labels),
        ("priority", &shared.priority),
        ("live", &shared.live),
    ] {
        assert!(acked(writer) > 3, "{name} writer made no progress");
    }

    // Everything any client was told about exists.
    let created = shared.created.lock().unwrap();
    for id in &created.tasks {
        assert!(tasks.contains_key(id), "acknowledged task {id} is missing");
    }
    for id in &created.goals {
        assert!(goals.contains_key(id), "acknowledged goal {id} is missing");
    }
    for id in &created.messages {
        assert!(
            messages.contains_key(id),
            "acknowledged message {id} is missing"
        );
    }
    assert!(
        agents.contains_key(worker) && agents.contains_key(parked),
        "seeded agents are missing"
    );
    assert!(created.tasks.len() > 5, "creators made no progress");
    if fleet == Fleet::Processes {
        let titles: BTreeSet<_> = tasks.values().filter_map(|t| t["title"].as_str()).collect();
        assert!(!created.tui_titles.is_empty(), "the TUI created nothing");
        for title in &created.tui_titles {
            assert!(
                titles.contains(title.as_str()),
                "TUI task {title} is missing"
            );
        }
    }

    // Context: nothing lost on renewal, expiry really ran.
    for i in 0..4 {
        assert!(
            context.contains_key(&format!("keep-{i}")),
            "seed key keep-{i} lost"
        );
    }
    for key in &created.context {
        assert!(context.contains_key(key), "no-TTL key {key} lost");
    }
    let live_acked = acked(&shared.live);
    assert_eq!(
        context.get("live").map(|record| record["value"].clone()),
        Some(json!(format!("live-{live_acked}"))),
        "the renewed key was lost or rolled back"
    );
    let ended = Instant::now();
    let expired: Vec<_> = created
        .short_keys
        .iter()
        .filter(|(_, set_at)| ended.duration_since(*set_at) > Duration::from_secs(4))
        .collect();
    for (key, _) in &expired {
        assert!(
            !context.contains_key(key),
            "never-renewed {key} survived its TTL"
        );
    }
    if duration >= Duration::from_secs(8) {
        assert!(!expired.is_empty(), "no short key was old enough to expire");
    }
}

#[test]
fn tui_cli_and_mcp_clients_mutating_one_project_leave_files_equal_to_the_index_and_lose_nothing() {
    run_stress(Fleet::Processes);
}

#[test]
fn daemon_client_writers_mutating_one_project_leave_files_equal_to_the_index_and_lose_nothing() {
    run_stress(Fleet::InProcess);
}
