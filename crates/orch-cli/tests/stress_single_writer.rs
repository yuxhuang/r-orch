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
//! 4. **no context key lost on renewal**: while the sweeps run, the renewed
//!    key is readable whenever its last renewal is provably within its TTL
//!    (judged live by the observer, and once more after the stop), while
//!    keys nobody renewed are gone (expiry really ran). The clock starts only
//!    when every leg (TUI, short key, renewer) has done its first piece of
//!    work, and the renewer keeps renewing until just before the daemon stop;
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
/// Slack when deciding that a renewal is "certainly still alive".
const LIVE_MARGIN: Duration = Duration::from_millis(250);

/// A renewal sent at `sent` cannot have expired at `at`: the daemon stamps
/// the expiry when it RECEIVES the request (>= `sent`) and removes a key
/// only after that expiry, so `sent + TTL` is a lower bound of the expiry.
/// Past this window a missing key is legitimate expiry, not a lost renewal.
fn certainly_alive(sent: Instant, at: Instant) -> bool {
    at.saturating_duration_since(sent) + LIVE_MARGIN < Duration::from_millis(LIVE_TTL_MS)
}

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
    /// Set after the other clients have finished, just before the daemon is
    /// stopped: the renewer outlives `stop` so the renewed key's last
    /// renewal is as close to the stop as possible (see `judge`).
    renew_stop: AtomicBool,
    /// Evidence counters the run waits on (see `evidence_missing`): hot-task
    /// reads the observer finished and `live` reads that were judged.
    reads: AtomicU64,
    live_checks: AtomicU64,
    /// The priority writer is paced by the observer (see `PriorityGate`).
    gate: PriorityGate,
    /// What the CLI creator is doing and since when (for a stuck-run report).
    creator_stage: Mutex<(String, Instant)>,
    /// `(n, sent, acked)` of the last acknowledged renewal of `live`: the
    /// renewal number and the instants it was sent and acknowledged.
    live_times: Mutex<Option<(u64, Instant, Instant)>>,
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
    fn renewing(&self) -> bool {
        !self.renew_stop.load(Ordering::SeqCst)
    }
    /// Record that renewal `n`, sent at `sent`, has just been acknowledged.
    /// The pair is published under one lock so a reader never combines
    /// renewal `n`'s number with another renewal's send time.
    fn live_acked(&self, n: u64, sent: Instant) {
        *self.live_times.lock().unwrap() = Some((n, sent, Instant::now()));
        self.live.acked.store(n, Ordering::SeqCst);
    }
}

fn priority_of(n: u64) -> u64 {
    1 + (n % 4)
}

/// May `observed` be the hot task's priority, given that write #`acked` of the
/// (single, sequential) priority writer was acknowledged BEFORE the read began
/// and write #`attempted` had been sent AT MOST by the time the read ended?
///
/// The read ran inside that interval, so the committed priority was the one of
/// some write `k` with `acked <= k <= attempted` (write 0 is the seed). A
/// window decided from `acked` alone is unsound: with the writer pacing at a
/// few ms, writes `acked+1` and `acked+2` can both commit during one slow read
/// (F-S2: "priority reads 4 after write #5 was acknowledged").
fn priority_may_read(observed: u64, acked: u64, attempted: u64) -> bool {
    (acked..=attempted.max(acked)).any(|k| priority_of(k) == observed)
}

/// Paces the priority writer by the observer so the window above stays at
/// one write wide and the check carries information. `priority` cycles
/// through only four values, so with a wider window (4+ writes) every value
/// would be "allowed" and a lost update would pass.
///
/// After write #n is acknowledged the writer waits until the observer has
/// COMPLETED a read that began after that acknowledgement; only then is #n+1
/// sent. A read that loaded `acked == a` therefore ends before #a+2 is sent
/// (`#a+2` waits for a read that began after `#a+1` was acknowledged, which is
/// after this read's `acked` load, so a later read of the sequential
/// observer).
#[derive(Default)]
struct PriorityGate {
    /// Reads begun / finished by the observer.
    begun: AtomicU64,
    finished: AtomicU64,
    /// 0: observer not started, 1: running, 2: gone.
    observer: std::sync::atomic::AtomicU8,
}

impl PriorityGate {
    /// The observer begins a read: call BEFORE loading the `acked` counters.
    fn read_begins(&self) {
        self.begun.fetch_add(1, Ordering::SeqCst);
    }
    /// The observer finished a read: call AFTER loading the `attempted`
    /// counters.
    fn read_ends(&self) {
        self.finished.fetch_add(1, Ordering::SeqCst);
    }
    fn observer_running(&self) {
        self.observer.store(1, Ordering::SeqCst);
    }
    fn observer_gone(&self) {
        self.observer.store(2, Ordering::SeqCst);
    }
    /// Call right after write #n was acknowledged: the number of reads that
    /// must be finished before the next write may go.
    fn target_after_ack(&self) -> u64 {
        self.begun.load(Ordering::SeqCst) + 1
    }
    /// May the next write go? `target` is `None` before the first write.
    fn open(&self, target: Option<u64>) -> bool {
        match self.observer.load(Ordering::SeqCst) {
            0 => false,
            1 => target.is_none_or(|t| self.finished.load(Ordering::SeqCst) >= t),
            _ => true,
        }
    }
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
                *shared.creator_stage.lock().unwrap() =
                    (format!("#{n} after {what}"), Instant::now());
                result
                    .map_err(|err| shared.fail(format!("cli creator {what} #{n}: {err}")))
                    .ok()
            };
            *shared.creator_stage.lock().unwrap() = (format!("#{n} starting"), Instant::now());
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
        let mut target = None;
        while !shared.stopped() {
            while !shared.gate.open(target) && !shared.stopped() {
                std::thread::sleep(Duration::from_millis(1));
            }
            if shared.stopped() {
                break;
            }
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
            target = Some(shared.gate.target_after_ack());
        }
        mcp.finish();
    })
}

fn spawn_mcp_creator(sandbox: Arc<Sandbox>, shared: Arc<Shared>) -> JoinHandle<()> {
    std::thread::spawn(move || {
        let mut mcp = mcp_client(&sandbox);
        let mut n = 0_u64;
        // Outlives `stop`: it renews until just before the daemon is stopped.
        while shared.renewing() {
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
                let sent = Instant::now();
                mcp_ok(&mcp.call_tool(
                    "context_set",
                    json!({"key": "live", "value": format!("live-{n}"), "ttl": LIVE_TTL_MS}),
                ))?;
                shared.live_acked(n, sent);
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
            let mut target = None;
            while !shared.stopped() {
                if field == "priority" {
                    while !shared.gate.open(target) && !shared.stopped() {
                        tokio::time::sleep(Duration::from_millis(1)).await;
                    }
                    if shared.stopped() {
                        break;
                    }
                }
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
                if field == "priority" {
                    target = Some(shared.gate.target_after_ack());
                } else {
                    tokio::time::sleep(Duration::from_millis(3)).await;
                }
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
            // Outlives `stop`: it renews until just before the daemon is stopped.
            while shared.renewing() {
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
                let sent = Instant::now();
                client
                    .context_set(ContextSetArgs {
                        key: Id::new("live").unwrap(),
                        value: Text::new(format!("live-{n}")).unwrap(),
                        ttl_ms: Some(LIVE_TTL_MS),
                    })
                    .await?;
                shared.live_acked(n, sent);
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
    /// Reads whose priority window was wider than one write: the priority
    /// check could not tell a lost update from a legitimate value there.
    priority_wide: u64,
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
            // However this ends, the priority writer must not wait for us.
            struct Gone<'a>(&'a PriorityGate);
            impl Drop for Gone<'_> {
                fn drop(&mut self) {
                    self.0.observer_gone();
                }
            }
            let _gone = Gone(&shared.gate);
            shared.gate.observer_running();
            while !shared.stopped() {
                shared.gate.read_begins();
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
                // What can have been sent by the time this read ended: loaded
                // AFTER the read, so every write that could have committed
                // during it is inside [acked, attempted].
                let attempted = (
                    shared.title.attempted.load(Ordering::SeqCst),
                    shared.description.attempted.load(Ordering::SeqCst),
                    shared.labels.attempted.load(Ordering::SeqCst),
                    shared.priority.attempted.load(Ordering::SeqCst),
                );
                shared.gate.read_ends();
                report.reads += 1;
                shared.reads.store(report.reads, Ordering::SeqCst);
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
                let priority = u64::try_from(task.priority).unwrap_or(0);
                if attempted.3.saturating_sub(acked.3) > 1 {
                    report.priority_wide += 1;
                }
                if !priority_may_read(priority, acked.3, attempted.3) {
                    let allowed: Vec<_> = (acked.3..=attempted.3.max(acked.3))
                        .map(priority_of)
                        .collect();
                    report.violations.push(format!(
                        "LOST UPDATE: priority reads {priority}; write #{} was acknowledged before \
                         the read and #{} had been sent by its end (expected one of {allowed:?}, \
                         commit_seq {seq})",
                        acked.3, attempted.3
                    ));
                }
                tokio::time::sleep(Duration::from_millis(15)).await;
            }
            report
        })
    })
}

/// No context key lost on renewal, judged while the daemon's sweeps run:
/// `live` is renewed far inside its TTL, so whenever the last acknowledged
/// renewal is provably still alive the key must be readable, at that renewal
/// or a newer one. Returns how many reads were judged and the violations.
fn spawn_live_observer(sandbox: &Sandbox, shared: Arc<Shared>) -> JoinHandle<(u64, Vec<String>)> {
    let config = client_config(sandbox, orch_proto::handshake::CallerKind::Web);
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async move {
            let (mut checks, mut violations) = (0_u64, Vec::new());
            let mut client = match orch_client::Client::connect(&config).await {
                Ok(client) => client,
                Err(err) => return (checks, vec![format!("live observer connect: {err:?}")]),
            };
            while !shared.stopped() {
                let renewal = *shared.live_times.lock().unwrap();
                let read = client
                    .context_get(orch_proto::ops::args::ContextGetArgs {
                        key: orch_proto::bounded::Id::new("live").unwrap(),
                    })
                    .await;
                let read_done = Instant::now();
                let entry = match read {
                    Ok(entry) => entry,
                    Err(err) => {
                        violations.push(format!("live observer read: {err:?}"));
                        break;
                    }
                };
                if let Some((n, sent, _)) = renewal
                    && certainly_alive(sent, read_done)
                {
                    checks += 1;
                    shared.live_checks.store(checks, Ordering::SeqCst);
                    let age = read_done.saturating_duration_since(sent).as_millis();
                    let seq = client.last_commit_seq().unwrap_or(0);
                    match entry
                        .as_ref()
                        .and_then(|e| parse_counter("live-", &e.value))
                    {
                        None => violations.push(format!(
                            "LOST RENEWAL: `live` reads {entry:?} {age}ms after renewal #{n} \
                             was sent (ttl {LIVE_TTL_MS}ms, commit_seq {seq})"
                        )),
                        Some(m) if m < n => violations.push(format!(
                            "LOST RENEWAL: `live` rolled back to live-{m} after renewal #{n} \
                             was acknowledged (commit_seq {seq})"
                        )),
                        Some(m) if m > shared.live.attempted.load(Ordering::SeqCst) => {
                            violations.push(format!("`live` reads live-{m}, a value never sent"));
                        }
                        Some(_) => {}
                    }
                }
                tokio::time::sleep(Duration::from_millis(15)).await;
            }
            (checks, violations)
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

/// The least evidence a run must collect before it may end: hot-task reads by
/// the observer (more than) and `live` reads judged inside its TTL.
const MIN_READS: u64 = 20;
const MIN_LIVE_CHECKS: u64 = 10;
/// How far past `ORCH_STRESS_SECS` a run is extended to collect that evidence.
const EXTENSION_CAP: Duration = Duration::from_secs(120);

/// `None` when the observers have the evidence the assertions demand.
fn evidence_missing(shared: &Shared) -> Option<String> {
    let (reads, live) = (
        shared.reads.load(Ordering::SeqCst),
        shared.live_checks.load(Ordering::SeqCst),
    );
    (reads <= MIN_READS || live < MIN_LIVE_CHECKS)
        .then(|| format!("{reads} reads, {live} judged live reads"))
}

/// How long a leg may take to get going before the run is declared broken.
const LEGS_READY: Duration = Duration::from_secs(60);

/// Block until the run has evidence from every leg its assertions speak
/// about: the TUI has created a task (processes fleet), a never-renewed short
/// key has been set, and `live` has been renewed at least once.
fn wait_until_every_leg_works(
    shared: &Shared,
    pane: Option<&TuiPane>,
    fleet: Fleet,
    daemon: &DaemonChild,
) {
    let deadline = Instant::now() + LEGS_READY;
    loop {
        let (tui, short, live) = {
            let created = shared.created.lock().unwrap();
            (
                !created.tui_titles.is_empty(),
                !created.short_keys.is_empty(),
                shared.live_times.lock().unwrap().is_some(),
            )
        };
        let tui = tui || fleet != Fleet::Processes;
        if tui && short && live {
            return;
        }
        let failures = shared.failures.lock().unwrap().clone();
        assert!(
            failures.is_empty(),
            "a client failed before every leg was running: {failures:#?}"
        );
        if let Some(pane) = pane
            && pane.has_exited()
        {
            panic!(
                "the TUI exited with status {} before it created a task; screen:\n{}",
                pane.exit_code(),
                pane.screen()
            );
        }
        assert!(
            Instant::now() < deadline,
            "after {LEGS_READY:?} not every leg was working (tui created a task: {tui}, \
             short key set: {short}, live renewed: {live}); TUI screen:\n{}\n\
             CLI creator: {}; orch processes:\n{}\ndaemon log tail:\n{}",
            pane.map_or_else(|| "(no TUI)".to_owned(), TuiPane::screen),
            {
                let (stage, since) = &*shared.creator_stage.lock().unwrap();
                format!("{stage} {}ms ago", since.elapsed().as_millis())
            },
            orch_processes(&process_table())
                .iter()
                .map(|p| format!("  pid {} ppid {} {}", p.pid, p.ppid, p.command))
                .collect::<Vec<_>>()
                .join("\n"),
            daemon
                .stderr()
                .lines()
                .rev()
                .take(30)
                .collect::<Vec<_>>()
                .join("\n")
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Named instants of one run, for the failure messages: a judgement that
/// depends on elapsed time must say how much elapsed.
struct Marks {
    started: Instant,
    marks: Vec<(&'static str, Instant)>,
}

impl Marks {
    fn new(started: Instant) -> Self {
        Self {
            started,
            marks: Vec::new(),
        }
    }
    fn note(&mut self, name: &'static str) {
        self.marks.push((name, Instant::now()));
    }
    fn at(&self, name: &str) -> Instant {
        self.marks
            .iter()
            .find(|(n, _)| *n == name)
            .unwrap_or_else(|| panic!("mark {name} was never taken"))
            .1
    }
    /// `name=+ms` since the clients started, in the order taken.
    fn render(&self) -> String {
        self.marks
            .iter()
            .map(|(name, at)| format!("{name}=+{}ms", at.duration_since(self.started).as_millis()))
            .collect::<Vec<_>>()
            .join(" ")
    }
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

/// Whatever way `run_stress` ends, stop the client threads and kill the TUI
/// leg. A failed assertion unwinds only the test thread: the TUI driver
/// thread still holds the pane, so its `Drop` never runs and `orch tui`
/// (with its pane script and tmux server) would outlive the test and fail
/// the process-table assertions of unrelated tests (F-B2b-3).
struct RunGuard {
    shared: Arc<Shared>,
    pane: Option<Arc<TuiPane>>,
}

impl Drop for RunGuard {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::SeqCst);
        self.shared.renew_stop.store(true, Ordering::SeqCst);
        if let Some(pane) = &self.pane {
            pane.terminate();
        }
    }
}

fn run_stress(fleet: Fleet) {
    let sandbox = Arc::new(Sandbox::new());
    sandbox.init_project();
    let mut daemon = start_ready(&sandbox, &["--dispatch", "on"], "owner");
    let fixtures = fixtures(&sandbox);
    let (worker, hot) = (fixtures.worker.clone(), fixtures.hot.clone());

    let shared = Arc::new(Shared {
        stop: AtomicBool::new(false),
        title: Writer::default(),
        description: Writer::default(),
        labels: Writer::default(),
        priority: Writer::default(),
        live: Writer::default(),
        renew_stop: AtomicBool::new(false),
        reads: AtomicU64::new(0),
        live_checks: AtomicU64::new(0),
        gate: PriorityGate::default(),
        creator_stage: Mutex::new(("not started".into(), Instant::now())),
        live_times: Mutex::new(None),
        failures: Mutex::default(),
        created: Mutex::default(),
    });
    let patch_dir = sandbox.root().join("stress-patches");
    fs::create_dir(&patch_dir).unwrap();

    // ── launch every client ──
    let mut marks = Marks::new(Instant::now());
    let mut guard = RunGuard {
        shared: shared.clone(),
        pane: None,
    };
    let mut threads = Vec::new();
    let renewer;
    match fleet {
        Fleet::Processes => {
            let tui = Arc::new(TuiPane::launch_at(
                &sandbox.root(),
                &sandbox.socket(),
                sandbox.short_dir(),
                "tui",
                &[],
            ));
            guard.pane = Some(tui.clone());
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
            renewer = spawn_mcp_creator(sandbox.clone(), shared.clone());
            threads.push(spawn_tui_driver(tui.clone(), shared.clone()));
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
            renewer = spawn_client_renewer(&sandbox, shared.clone());
        }
    }

    // ── the clock starts when every leg is really running ──
    // A leg that is slow to start (a TUI in a cold tmux, an emulated CPU)
    // must not eat the run: the assertions below claim that the TUI created
    // a task, that a short key outlived its TTL and that `live` was renewed,
    // so the run is `duration` of ALL of them working, not of whoever was
    // ready first.
    wait_until_every_leg_works(&shared, guard.pane.as_deref(), fleet, &daemon);
    marks.note("legs_ready");
    let started = Instant::now();
    let observer = spawn_observer(&sandbox, shared.clone(), hot.clone());
    let live_observer = spawn_live_observer(&sandbox, shared.clone());

    // ── let them fight ──
    // At least `duration`, and until the observers have the evidence the
    // assertions below demand. How many reads fit in a second is the host's
    // business (a read queues behind every durable write, and a loaded or
    // emulated machine makes both slow); how many are JUDGED is the test's.
    // So the run is extended, up to `EXTENSION_CAP`, rather than failed for
    // being slow, and it still fails if the evidence never comes.
    let duration = stress_duration();
    while shared.failures.lock().unwrap().is_empty() {
        let elapsed = started.elapsed();
        if elapsed >= duration + EXTENSION_CAP
            || (elapsed >= duration && evidence_missing(&shared).is_none())
        {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    shared.stop.store(true, Ordering::SeqCst);
    marks.note("stop_flag");
    let mut panicked = Vec::new();
    for thread in threads {
        if let Err(payload) = thread.join() {
            panicked.push(format!("{payload:?}"));
        }
    }
    let report = observer.join().unwrap();
    let (live_checks, live_violations) = live_observer.join().unwrap();
    marks.note("clients_joined");

    // The TUI survived the whole run on the daemon.
    if let Some(pane) = &guard.pane {
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

    marks.note("tui_quit");

    // The renewer ran through all of the above; stop it only now so `live`
    // was renewed as close to the daemon stop as it can be.
    shared.renew_stop.store(true, Ordering::SeqCst);
    if let Err(payload) = renewer.join() {
        panicked.push(format!("{payload:?}"));
    }
    marks.note("renewer_joined");

    // ── stop the owner so every write is on disk, then judge offline ──
    marks.note("daemon_stop_sent");
    let stop = sandbox.run(&["daemon", "stop"]);
    assert!(stop.status.success(), "{}", text(&stop.stderr));
    marks.note("daemon_stop_acked");
    let status = daemon.wait_exit(READY);
    marks.note("daemon_exited");
    assert_eq!(status.code(), Some(0), "daemon: {}", daemon.stderr());

    let failures = shared.failures.lock().unwrap().clone();
    assert!(panicked.is_empty(), "client threads panicked: {panicked:?}");
    assert!(
        failures.is_empty(),
        "client operations failed: {failures:#?}"
    );
    assert!(
        report.reads > MIN_READS,
        "the observer barely ran: {} reads in {}ms (needed more than {MIN_READS}; the run was \
         extended to at most {}ms before giving up); {}",
        report.reads,
        started.elapsed().as_millis(),
        (duration + EXTENSION_CAP).as_millis(),
        marks.render()
    );
    assert!(
        report.violations.is_empty(),
        "{} ordering violations in {} reads, first: {:#?}",
        report.violations.len(),
        report.reads,
        &report.violations[..report.violations.len().min(10)]
    );
    assert!(
        live_violations.is_empty(),
        "{} lost renewals in {live_checks} judged reads, first: {:#?}",
        live_violations.len(),
        &live_violations[..live_violations.len().min(10)]
    );
    assert!(
        live_checks >= MIN_LIVE_CHECKS,
        "`live` was judged only {live_checks}/{MIN_LIVE_CHECKS} times in {}ms (the run was \
         extended to at most {}ms): the renewals never kept it provably alive; \
         last renewal #{}; {}",
        started.elapsed().as_millis(),
        (duration + EXTENSION_CAP).as_millis(),
        shared.live.acked.load(Ordering::SeqCst),
        marks.render()
    );
    // The priority check says something only where its window is one write
    // wide; the gate makes that every read, so most reads must be.
    assert!(
        report.priority_wide * 5 <= report.reads,
        "the priority check was blind: {} of {} reads had a window wider than one write",
        report.priority_wide,
        report.reads
    );

    let strong = judge(&sandbox, &shared, &fixtures, duration, fleet, &marks);
    // Greppable run summary (`--nocapture`): the strong branch is the
    // post-stop "renewed key present" assertion, which only applies when the
    // stop was acknowledged inside the key's TTL; its rate over a batch of
    // runs is the evidence that the assertion is not vacuous.
    eprintln!(
        "STRESS_SUMMARY fleet={} elapsed_ms={} reads={} priority_wide={} live_checks={} \
         strong_branch={strong}",
        if fleet == Fleet::Processes {
            "processes"
        } else {
            "in-process"
        },
        started.elapsed().as_millis(),
        report.reads,
        report.priority_wide,
        live_checks
    );
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
    fixtures: &Fixtures,
    duration: Duration,
    fleet: Fleet,
    marks: &Marks,
) -> bool {
    let Fixtures {
        worker,
        parked,
        hot,
    } = fixtures;
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
    // `live` after the stop. The daemon's sweep stops when the stop is
    // handled (before the stop command returns), so a renewal sent at `sent`
    // is certain to be there only if the stop was acknowledged inside its TTL.
    // Later than that it may legitimately have expired (a slow join, a slow
    // emulated CPU): then it may be gone, but it must never be a value that
    // is not the last acknowledged renewal. The in-run check in the observer
    // is what proves no renewal is lost while the sweeps run.
    let live_acked = acked(&shared.live);
    let (n, live_sent, live_ack_at) = shared
        .live_times
        .lock()
        .unwrap()
        .expect("live was never renewed");
    assert_eq!(n, live_acked, "the renewal record and the counter disagree");
    let rel = |at: Instant| at.duration_since(marks.started).as_millis();
    let stop_acked = marks.at("daemon_stop_acked");
    let live_now = context.get("live").map(|record| record["value"].clone());
    let timeline = format!(
        "last renewal live-{live_acked} sent +{}ms acked +{}ms (ttl {LIVE_TTL_MS}ms); \
         stop acked {}ms after it was sent; {}",
        rel(live_sent),
        rel(live_ack_at),
        stop_acked.saturating_duration_since(live_sent).as_millis(),
        marks.render()
    );
    let strong = certainly_alive(live_sent, stop_acked);
    if strong {
        assert_eq!(
            live_now,
            Some(json!(format!("live-{live_acked}"))),
            "the renewed key was lost or rolled back inside its TTL: {timeline}"
        );
    } else if let Some(value) = live_now {
        assert_eq!(
            value,
            json!(format!("live-{live_acked}")),
            "the renewed key was rolled back: {timeline}"
        );
    }
    // The sweep stopped when the daemon was stopped, not when we looked.
    let ended = stop_acked;
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
    strong
}

#[test]
fn tui_cli_and_mcp_clients_mutating_one_project_leave_files_equal_to_the_index_and_lose_nothing() {
    run_stress(Fleet::Processes);
}

#[test]
fn daemon_client_writers_mutating_one_project_leave_files_equal_to_the_index_and_lose_nothing() {
    run_stress(Fleet::InProcess);
}

// ── the checker's own contract (pure, no daemon) ───────────────────────────

#[test]
fn priority_window_accepts_every_write_that_can_commit_during_the_read() {
    // F-S2: write #5 acknowledged before the read, #6 and #7 committed while
    // it ran. priority_of(7) = 4 is legitimate.
    assert!(priority_may_read(priority_of(7), 5, 7));
    assert!(priority_may_read(priority_of(5), 5, 7));
    assert!(priority_may_read(priority_of(6), 5, 7));
}

#[test]
fn priority_window_accepts_the_seed_before_any_write() {
    assert!(priority_may_read(priority_of(0), 0, 0));
    assert!(priority_may_read(priority_of(1), 0, 1));
}

#[test]
fn priority_window_rejects_a_value_older_than_the_last_acknowledged_write() {
    // Rolled back to write #4 after #5 was acknowledged and #6 sent.
    assert!(!priority_may_read(priority_of(4), 5, 6));
    // A value of a write never sent.
    assert!(!priority_may_read(priority_of(6), 5, 5));
}

#[test]
fn priority_window_is_blind_only_once_it_spans_a_full_cycle() {
    let values = |acked, attempted| -> BTreeSet<u64> {
        (1..=4)
            .filter(|v| priority_may_read(*v, acked, attempted))
            .collect()
    };
    assert_eq!(values(5, 5).len(), 1);
    assert_eq!(values(5, 6).len(), 2);
    assert_eq!(values(5, 8).len(), 4, "a 4-write window allows everything");
}

#[test]
fn priority_gate_holds_the_writer_until_a_read_that_began_after_its_ack_finished() {
    let gate = PriorityGate::default();
    assert!(!gate.open(None), "no write before the observer runs");
    gate.observer_running();
    assert!(gate.open(None), "the first write needs no read");
    gate.read_begins(); // read 1 is in flight when write #n is acknowledged
    let target = Some(gate.target_after_ack());
    assert!(!gate.open(target));
    gate.read_ends(); // read 1 began before the ack: not enough
    assert!(!gate.open(target));
    gate.read_begins(); // read 2 began after the ack
    assert!(!gate.open(target));
    gate.read_ends();
    assert!(gate.open(target));
    gate.observer_gone();
    assert!(
        gate.open(Some(u64::MAX)),
        "a dead observer never blocks the writer"
    );
}
