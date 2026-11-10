#![cfg(feature = "testing")]
//! Subscriptions and push, observed on the wire (T3; spec R16, ruling 14.1,
//! 14.2, brief D2/D3/D5).
//!
//! These vectors speak raw NDJSON to the in-process daemon over its real Unix
//! socket, the way a hostile or future client would, and know nothing of the
//! hub's internals. Every wait is a hang guard ([`GUARD`]) that only fires
//! when the daemon never answers; nothing paces itself with a sleep. "No event
//! arrives" is proved without waiting: a *marker* write that matches the
//! subscription is made last, and the first event read must be the marker.
//!
//! Contract used (frozen API SHA 9214424f, rulings 14.1/14.2):
//!
//! * `subscribe` replies `{subscription, instance, seq: 0}` with the baseline
//!   in the terminal frame's `commit_seq`; the first event has `seq` 1.
//! * An event carries the store, the entity id (absent = store-wide change),
//!   the kind and the commit ordinal of the write that caused it. A matching
//!   event is delivered after its write is published, never before the ack.
//! * Filters are typed ids: `ids` (the entity itself), `goal_id` and
//!   `agent_id` (tasks that belong to the goal / are assigned to the agent,
//!   on **either side** of a change: a task reassigned away from agent A or
//!   deleted still invalidates the subscriber that held it).
//! * Caps: 8 subscriptions per session (the 9th is `overloaded`,
//!   `not_executed`), 256 queued events per subscription, 4 MiB queued output
//!   per session. Overflow becomes an explicit `resync` (or the session is
//!   closed); events are never silently dropped and the daemon never waits on
//!   a slow reader.

use orch_core::process::{
    ExitNotifier, ExitOutcome, ExitSignal, ProcessManager, Signal, SpawnOptions, SpawnedChild,
};
use orch_daemon::testing::DaemonHarness;
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, ReadHalf, WriteHalf};
use tokio::net::UnixStream;

#[macro_use]
mod support;
use support::TestClock;

/// Upper bound for any single wait. A hang guard, never a pacing delay.
const GUARD: Duration = Duration::from_secs(10);
/// Upper bound for an event that must already be on its way. Shorter than
/// [`GUARD`] so a daemon that never pushes fails fast with a clear message.
const EVENT_GUARD: Duration = Duration::from_secs(4);

// ---------------------------------------------------------------------------
// A raw session that keeps the `ev` frames it sees while waiting for replies
// ---------------------------------------------------------------------------

struct Wire {
    reader: BufReader<ReadHalf<UnixStream>>,
    writer: WriteHalf<UnixStream>,
    /// `ev` frames that arrived while a reply was awaited, oldest first.
    events: VecDeque<Value>,
    /// The daemon instance from this session's welcome.
    instance: String,
    next_id: u64,
}

/// What a `subscribe` returned.
#[derive(Debug, Clone)]
struct Sub {
    id: u64,
    ack_instance: String,
    ack_seq: u64,
    /// The terminal frame's `commit_seq`: the baseline.
    baseline: u64,
}

impl Wire {
    async fn open(daemon: &DaemonHarness, kind: &str) -> Wire {
        let socket = daemon.socket().expect("harness bound to a socket");
        let stream = UnixStream::connect(socket).await.expect("connect");
        let (read, writer) = tokio::io::split(stream);
        let mut wire = Wire {
            reader: BufReader::new(read),
            writer,
            events: VecDeque::new(),
            instance: String::new(),
            next_id: 0,
        };
        wire.send(&support::session_hello(
            daemon.project_root(),
            kind,
            "normal",
        ))
        .await;
        let welcome = wire.read_frame().await.expect("a welcome");
        assert_eq!(welcome["welcome"]["compatible"], true, "{welcome}");
        wire.instance = welcome["welcome"]["instance"]
            .as_str()
            .expect("welcome.instance")
            .to_owned();
        wire
    }

    async fn send(&mut self, value: &Value) {
        self.writer.write_all(&support::line(value)).await.unwrap();
        self.writer.flush().await.unwrap();
    }

    /// The next frame; `None` once the daemon closed the connection.
    async fn read_frame(&mut self) -> Option<Value> {
        let mut text = String::new();
        let read = tokio::time::timeout(GUARD, self.reader.read_line(&mut text))
            .await
            .expect("timed out waiting for the daemon");
        match read {
            Ok(0) | Err(_) => None,
            Ok(_) => Some(serde_json::from_str(text.trim_end()).expect("a JSON frame")),
        }
    }

    /// Send a request and return its terminal frame. `ev` frames that arrive
    /// first are kept for [`Wire::event`].
    async fn call(&mut self, op: &str, args: Value) -> Value {
        self.next_id += 1;
        let id = self.next_id;
        self.send(&json!({"id": id, "req": {"op": op, "args": args}}))
            .await;
        loop {
            let frame = self
                .read_frame()
                .await
                .expect("connection closed before the reply");
            if frame.get("ev").is_some() {
                self.events.push_back(frame["ev"].clone());
            } else if frame["id"] == id && (frame.get("ok").is_some() || frame.get("err").is_some())
            {
                return frame;
            }
        }
    }

    async fn subscribe(&mut self, args: Value) -> Sub {
        let reply = self.call("subscribe", args).await;
        let ack = support::ok(&reply);
        Sub {
            id: ack["subscription"].as_u64().expect("subscription id"),
            ack_instance: ack["instance"].as_str().expect("ack.instance").to_owned(),
            ack_seq: ack["seq"].as_u64().expect("ack.seq"),
            baseline: reply["commit_seq"].as_u64().expect("ok.commit_seq"),
        }
    }

    /// The next pushed event of any subscription (kept or freshly read).
    async fn event(&mut self) -> Value {
        if let Some(event) = self.events.pop_front() {
            return event;
        }
        let mut text = String::new();
        let read = tokio::time::timeout(EVENT_GUARD, self.reader.read_line(&mut text))
            .await
            .unwrap_or_else(|_| {
                panic!(
                    "no event was pushed within {EVENT_GUARD:?}: the subscription never delivered"
                )
            });
        assert!(
            matches!(read, Ok(n) if n > 0),
            "the daemon closed the connection while an event was expected"
        );
        let frame: Value = serde_json::from_str(text.trim_end()).expect("a JSON frame");
        frame
            .get("ev")
            .cloned()
            .unwrap_or_else(|| panic!("expected an ev frame, got {frame}"))
    }

    async fn ok(&mut self, op: &str, args: Value) -> Value {
        let reply = self.call(op, args).await;
        support::ok(&reply).clone()
    }

    /// `commit_seq` of the reply to a mutation.
    async fn commit(&mut self, op: &str, args: Value) -> (Value, u64) {
        let reply = self.call(op, args).await;
        let commit_seq = reply["commit_seq"].as_u64().expect("commit_seq");
        (support::ok(&reply).clone(), commit_seq)
    }
}

fn change(event: &Value) -> (&str, Option<&str>, &str) {
    (
        event["store"].as_str().expect("event.store"),
        event["id"].as_str(),
        event["kind"].as_str().expect("event.kind"),
    )
}

async fn harness() -> DaemonHarness {
    DaemonHarness::builder()
        .on_socket()
        .maintenance_timer(false)
        .start()
        .await
        .unwrap()
}

async fn create_task(wire: &mut Wire, title: &str, extra: Value) -> String {
    let mut args = json!({ "title": title });
    for (key, value) in extra.as_object().cloned().unwrap_or_default() {
        args[key] = value;
    }
    wire.ok("task.create", args).await["id"]
        .as_str()
        .unwrap()
        .to_owned()
}

async fn create_agent(wire: &mut Wire, name: &str) -> String {
    wire.ok(
        "agent.create",
        json!({"name": name, "adapter": "shell", "command": "exit 0",
               "workspace_mode": "shared"}),
    )
    .await["agent"]["id"]
        .as_str()
        .unwrap()
        .to_owned()
}

// ---------------------------------------------------------------------------
// The headline: snapshot ordering, cursor, gap and resync
// ---------------------------------------------------------------------------

/// The spec target (R16). One subscriber walks the whole contract:
/// ack-before-event with cursor origin 0; a baseline taken under the
/// coordinator (so a write committed before the subscribe is *not* pushed and
/// the first push is the first write after it); per-subscription sequence
/// numbers 1, 2, 3 for create/update/delete; the commit ordinal of each push
/// equal to the writer's reply; and, for a reader that stops reading while
/// writers keep going, an explicit end of the stream (resync or close) rather
/// than a silent hole.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn subscribe_snapshot_gap_resync() {
    let daemon = harness().await;
    let mut writer = Wire::open(&daemon, "cli").await;
    let mut observer = Wire::open(&daemon, "tui").await;

    let before = create_task(&mut writer, "before the subscription", json!({})).await;
    let sub = observer.subscribe(json!({"stores": ["tasks"]})).await;

    // Ack: cursor origin zero, this daemon's instance, a real baseline.
    assert_eq!(sub.ack_seq, 0, "ack.seq is the cursor origin (ruling 14.1)");
    assert_eq!(sub.ack_instance, observer.instance, "ack.instance");
    assert!(sub.baseline >= 1, "baseline {sub:?}");

    // create -> update -> delete: seq 1, 2, 3 and the writer's commit ordinals.
    let (task, created_at) = writer
        .commit("task.create", json!({"title": "after the subscription"}))
        .await;
    let id = task["id"].as_str().unwrap().to_owned();
    let (_, updated_at) = writer
        .commit("task.update_fields", json!({"id": id, "title": "renamed"}))
        .await;
    let (_, deleted_at) = writer.commit("task.delete", json!({"id": id})).await;

    for (seq, kind, commit_seq) in [
        (1, "created", created_at),
        (2, "updated", updated_at),
        (3, "deleted", deleted_at),
    ] {
        let event = observer.event().await;
        assert_eq!(event["subscription"], sub.id, "{event}");
        assert_eq!(event["instance"], observer.instance, "{event}");
        assert_eq!(event["seq"], seq, "{event}");
        assert_eq!(
            change(&event),
            ("tasks", Some(id.as_str()), kind),
            "{event}"
        );
        assert_eq!(event["commit_seq"], commit_seq, "{event}");
        assert!(commit_seq > sub.baseline, "event not after the baseline");
        assert!(
            event.get("id").is_some() && event.get("bus").is_none(),
            "a change event carries identity only: {event}"
        );
    }
    // The write committed before the subscribe was never pushed: it is in the
    // view the client reads after the ack, not in the stream.
    let listed = observer.ok("task.list", json!({})).await;
    assert!(
        listed
            .as_array()
            .unwrap()
            .iter()
            .any(|task| task["id"] == before.as_str()),
        "{listed}"
    );

    // Gap/resync: stop reading, keep writing. The daemon must neither block
    // the writers nor drop events silently: whatever the stalled reader
    // finally drains is contiguous from seq 4 and ends in a resync marker or
    // a close, never in a hole.
    let mut stalled = Wire::open(&daemon, "tui").await;
    let stalled_sub = stalled.subscribe(json!({"stores": ["context"]})).await;
    let produced = fill_until_stall_overflows(&mut writer).await;
    let frames = stalled.drain_until_end().await;
    assert_stream_is_contiguous_then_ends(&frames, stalled_sub.id, produced);
    daemon.shutdown().await.unwrap();
}

/// Write small `context.set` commits until the stalled subscriber must have
/// overflowed its queue plus every transport buffer in between (bounded:
/// 20 000 events is far beyond 256 queued events + a few hundred KiB of
/// socket buffer, and each event is ~150 bytes). Returns the number written.
async fn fill_until_stall_overflows(writer: &mut Wire) -> usize {
    const WRITES: usize = 6_000;
    for n in 0..WRITES {
        writer
            .ok(
                "context.set",
                json!({"key": format!("k{}", n % 50), "value": format!("v{n}")}),
            )
            .await;
    }
    WRITES
}

impl Wire {
    /// Read everything the daemon sends a reader that did not read for a
    /// while: stop at a resync marker (further frames, if any, are drained
    /// too) or at the close.
    async fn drain_until_end(&mut self) -> Vec<Value> {
        let mut out: Vec<Value> = self.events.drain(..).collect();
        while let Some(frame) = self.read_frame().await {
            let event = frame.get("ev").cloned().unwrap_or(frame);
            let is_resync = event.get("resync").is_some();
            out.push(event);
            if is_resync {
                break;
            }
        }
        out
    }
}

/// Seq numbers are 1, 2, 3, … without a hole; the stream ends with exactly
/// one `lagged` resync marker (whose own seq continues the numbering) or with
/// the close of the connection; and the daemon did not buffer everything.
fn assert_stream_is_contiguous_then_ends(frames: &[Value], subscription: u64, produced: usize) {
    let mut expected = 1;
    let mut ended_by_resync = false;
    for frame in frames {
        if frame.get("resync").is_some() {
            assert_eq!(frame["subscription"], subscription, "{frame}");
            assert_eq!(frame["resync"]["reason"], "lagged", "{frame}");
            assert!(frame["seq"].as_u64().unwrap() >= expected, "{frame}");
            ended_by_resync = true;
            continue;
        }
        assert!(!ended_by_resync, "an event follows the resync: {frame}");
        assert_eq!(frame["seq"], expected, "silent hole before {frame}");
        expected += 1;
    }
    let delivered = expected - 1;
    assert!(
        delivered >= 256,
        "a stalled reader must be able to hold a full queue of 256 events, got {delivered}"
    );
    assert!(
        ended_by_resync || (delivered as usize) < produced,
        "{produced} events produced, {delivered} delivered, no resync marker: \
         the daemon either buffered without bound or lost events silently"
    );
}

/// The ack is the first frame of a subscription even when a writer is
/// committing concurrently, and no pushed event is at or below the baseline.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ack_precedes_every_event_and_nothing_at_or_below_the_baseline_is_pushed() {
    let daemon = harness().await;
    let mut writer = Wire::open(&daemon, "cli").await;
    let mut observer = Wire::open(&daemon, "tui").await;
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flood = {
        let stop = Arc::clone(&stop);
        let mut flooder = Wire::open(&daemon, "cli").await;
        tokio::spawn(async move {
            let mut n = 0u64;
            while !stop.load(Ordering::SeqCst) {
                flooder
                    .ok(
                        "context.set",
                        json!({"key": "race", "value": format!("{n}")}),
                    )
                    .await;
                n += 1;
            }
            n
        })
    };
    // Subscribe in the middle of the flood; read the ack by hand so the very
    // next frame decides the ordering.
    observer.next_id += 1;
    let ack_id = observer.next_id;
    observer
        .send(&json!({"id": ack_id, "req": {"op": "subscribe",
                      "args": {"stores": ["context"]}}}))
        .await;
    let first = observer.read_frame().await.expect("first frame");
    assert_eq!(
        first["id"], ack_id,
        "the ack must precede every event: {first}"
    );
    let baseline = first["commit_seq"].as_u64().expect("baseline");
    assert_eq!(first["ok"]["seq"], 0, "{first}");

    let marker = writer
        .commit("context.set", json!({"key": "marker", "value": "m"}))
        .await
        .1;
    let mut last_seq = 0;
    let mut last_commit = baseline;
    loop {
        let event = observer.event().await;
        let seq = event["seq"].as_u64().unwrap();
        let commit = event["commit_seq"].as_u64().unwrap();
        assert_eq!(seq, last_seq + 1, "{event}");
        assert!(commit > baseline, "event at or below baseline: {event}");
        assert!(commit >= last_commit, "commit ordinals went backwards");
        (last_seq, last_commit) = (seq, commit);
        if commit >= marker {
            break;
        }
    }
    stop.store(true, Ordering::SeqCst);
    flood.await.unwrap();
    daemon.shutdown().await.unwrap();
}

// ---------------------------------------------------------------------------
// Caps
// ---------------------------------------------------------------------------

/// 8 subscriptions per session, the 9th is refused as `overloaded` and
/// `not_executed`; an unsubscribe frees exactly one slot; unsubscribing twice
/// (or an id never issued) is idempotent.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn eight_subscriptions_per_session_the_ninth_is_overloaded_and_unsubscribe_frees_a_slot() {
    let daemon = harness().await;
    let mut wire = Wire::open(&daemon, "tui").await;
    let mut subs = Vec::new();
    for _ in 0..8 {
        subs.push(wire.subscribe(json!({"stores": ["tasks"]})).await);
    }
    let ids: std::collections::BTreeSet<u64> = subs.iter().map(|sub| sub.id).collect();
    assert_eq!(ids.len(), 8, "subscription ids are distinct");

    let ninth = wire.call("subscribe", json!({"stores": ["tasks"]})).await;
    assert_eq!(support::err_code(&ninth), "overloaded", "{ninth}");
    assert_eq!(ninth["err"]["execution"], "not_executed", "{ninth}");

    // Another session has its own budget.
    let mut other = Wire::open(&daemon, "tui").await;
    other.subscribe(json!({"stores": ["tasks"]})).await;

    let freed = wire
        .call("unsubscribe", json!({"subscription": subs[3].id}))
        .await;
    assert_eq!(support::ok(&freed), &Value::Null, "{freed}");
    let again = wire
        .call("unsubscribe", json!({"subscription": subs[3].id}))
        .await;
    assert_eq!(support::ok(&again), &Value::Null, "idempotent: {again}");
    let never = wire
        .call("unsubscribe", json!({"subscription": 999_999}))
        .await;
    assert_eq!(support::ok(&never), &Value::Null, "{never}");

    let replacement = wire.subscribe(json!({"stores": ["tasks"]})).await;
    assert!(!ids.contains(&replacement.id), "ids are never reused");
    let tenth = wire.call("subscribe", json!({"stores": ["tasks"]})).await;
    assert_eq!(support::err_code(&tenth), "overloaded", "{tenth}");
    daemon.shutdown().await.unwrap();
}

/// 256 queued events are held for a reader that has not started reading: all
/// of them arrive, in order, with no resync. (At most the count cap is in
/// flight, so this holds whatever the transport buffers do.)
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_reader_that_falls_exactly_256_events_behind_loses_nothing() {
    let daemon = harness().await;
    let mut writer = Wire::open(&daemon, "cli").await;
    let mut observer = Wire::open(&daemon, "tui").await;
    let sub = observer.subscribe(json!({"stores": ["context"]})).await;
    let mut last = 0;
    for n in 0..256 {
        last = writer
            .commit("context.set", json!({"key": format!("k{n}"), "value": "v"}))
            .await
            .1;
    }
    for expected in 1..=256 {
        let event = observer.event().await;
        assert!(
            event.get("resync").is_none(),
            "resync at {expected}: {event}"
        );
        assert_eq!(event["subscription"], sub.id);
        assert_eq!(event["seq"], expected, "{event}");
    }
    let marker = writer
        .commit("context.set", json!({"key": "marker", "value": "m"}))
        .await
        .1;
    assert!(marker > last);
    let event = observer.event().await;
    assert_eq!(event["seq"], 257, "{event}");
    assert_eq!(event["commit_seq"], marker, "{event}");
    daemon.shutdown().await.unwrap();
}

// ---------------------------------------------------------------------------
// Overflow by bytes (before the count cap) and the responsive control lane
// ---------------------------------------------------------------------------

/// A fake agent process manager: every launch prints `lines` lines of `width`
/// bytes and then waits for the test to let it exit.
#[derive(Default)]
struct Chatty {
    lines: usize,
    width: usize,
    notifiers: Mutex<Vec<ExitNotifier>>,
    pids: AtomicU32,
    spawned: Mutex<Vec<String>>,
}

#[async_trait::async_trait]
impl ProcessManager for Chatty {
    fn is_alive(&self, _: u32) -> bool {
        false
    }
    fn kill(&self, _: u32, _: Signal) {}
    async fn kill_with_grace(&self, _: u32, _: Duration) {}
    fn spawn(
        &self,
        _command: &str,
        _args: &[String],
        opts: SpawnOptions,
    ) -> orch_core::Result<SpawnedChild> {
        let mut out = Vec::new();
        for _ in 0..self.lines {
            out.extend(std::iter::repeat_n(b'x', self.width));
            out.push(b'\n');
        }
        self.spawned
            .lock()
            .unwrap()
            .push(opts.env.get("ORCH_TASK_ID").cloned().unwrap_or_default());
        let (notifier, exit) = ExitSignal::channel();
        self.notifiers.lock().unwrap().push(notifier);
        Ok(SpawnedChild {
            pid: 40_000 + self.pids.fetch_add(1, Ordering::SeqCst),
            stdout: Some(Box::new(std::io::Cursor::new(out))),
            stderr: None,
            stdin: None,
            exit,
        })
    }
}

/// Large pushed events fill the 4 MiB output budget long before 256 of them
/// are queued. The stalled subscriber's budget must be enforced in bytes, the
/// stream must still end explicitly, and a *separate* responsive session on
/// the same daemon (status, a write, a tick) must never wait for it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn big_events_overflow_the_byte_budget_before_the_count_cap_and_control_stays_responsive() {
    let chatty = Arc::new(Chatty {
        lines: 24,
        width: 512 * 1024,
        ..Default::default()
    });
    let daemon = DaemonHarness::builder()
        .on_socket()
        .maintenance_timer(false)
        .processes(chatty.clone() as Arc<dyn ProcessManager>)
        .start()
        .await
        .unwrap();
    let mut control = Wire::open(&daemon, "cli").await;
    let agent = create_agent(&mut control, "chatty").await;
    let task = create_task(&mut control, "talkative", json!({"assignee": agent})).await;

    let mut stalled = Wire::open(&daemon, "tui").await;
    let sub = stalled
        .subscribe(json!({"stores": ["tasks"], "bus": true}))
        .await;
    // A healthy reader of the same stream must be unaffected by the stall.
    let mut healthy = Wire::open(&daemon, "tui").await;
    let healthy_sub = healthy.subscribe(json!({"stores": ["tasks"]})).await;

    let tick = control.ok("dispatch.tick", json!({})).await;
    assert_eq!(tick["launched"], json!([task]), "{tick}");

    // The control lane answers while a subscriber is stalled on megabytes.
    let status = control.ok("daemon.status", json!({})).await;
    assert!(status["dispatch"].is_object(), "{status}");
    let other = create_task(&mut control, "written during the stall", json!({})).await;
    let event = healthy.event().await;
    assert_eq!(event["subscription"], healthy_sub.id, "{event}");
    // The launch itself changed the task first (queued -> running), then the
    // write made during the stall; the healthy reader sees both, in order.
    assert_eq!(change(&event).0, "tasks", "{event}");
    assert_eq!(event["id"], task.as_str(), "{event}");
    let mut seen_other = false;
    while !seen_other {
        let next = healthy.event().await;
        seen_other = next["id"] == other.as_str();
    }

    let mut bytes = 0usize;
    let mut events = 0usize;
    let mut resync = false;
    while let Some(frame) = stalled.read_frame().await {
        let ev = frame.get("ev").cloned().unwrap_or(frame);
        bytes += ev.to_string().len();
        assert_eq!(ev["subscription"], sub.id, "{ev}");
        if ev.get("resync").is_some() {
            assert_eq!(ev["resync"]["reason"], "lagged", "{ev}");
            resync = true;
            break;
        }
        events += 1;
        if events >= 256 {
            break;
        }
    }
    let produced = 24 * 512 * 1024;
    assert!(
        resync || bytes < produced,
        "{bytes} of {produced} bytes delivered with no resync marker: bytes are not bounded"
    );
    assert!(
        events < 256,
        "the byte budget must trip before the 256-event count cap, saw {events} events"
    );
    daemon.shutdown().await.unwrap();
}

/// A subscriber that never reads must not slow a responsive session down, and
/// stopping the daemon with that subscriber still attached completes.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_stalled_subscriber_never_blocks_writers_or_other_subscribers_or_shutdown() {
    let daemon = harness().await;
    let mut writer = Wire::open(&daemon, "cli").await;
    let mut stalled = Wire::open(&daemon, "tui").await;
    stalled.subscribe(json!({"stores": ["context"]})).await;
    let mut healthy = Wire::open(&daemon, "tui").await;
    let sub = healthy.subscribe(json!({"stores": ["context"]})).await;

    // Far more than the 256-event queue, the stalled reader never reads.
    let mut last = 0;
    for n in 0..1_200 {
        last = writer
            .commit(
                "context.set",
                json!({"key": "busy", "value": format!("{n}")}),
            )
            .await
            .1;
    }
    // The healthy reader got every one of them, in order, without a resync.
    for expected in 1..=1_200u64 {
        let event = healthy.event().await;
        assert!(
            event.get("resync").is_none(),
            "healthy reader resynced: {event}"
        );
        assert_eq!(event["subscription"], sub.id);
        assert_eq!(event["seq"], expected, "{event}");
        if expected == 1_200 {
            assert_eq!(event["commit_seq"], last, "{event}");
        }
    }
    let status = writer.call("daemon.status", json!({})).await;
    assert!(status.get("ok").is_some(), "{status}");
    // The stalled subscriber is still attached and unread.
    daemon.shutdown().await.unwrap();
}

// ---------------------------------------------------------------------------
// Store-wide changes
// ---------------------------------------------------------------------------

/// A singleton store has no entity id: the change is an ordinary ordered
/// event with the store and kind `updated` and **no** id (ruling 14.2), and it
/// is not a resync.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_config_commit_is_a_store_wide_change_with_no_id_and_is_not_a_resync() {
    let daemon = harness().await;
    let mut writer = Wire::open(&daemon, "cli").await;
    let mut observer = Wire::open(&daemon, "tui").await;
    let sub = observer
        .subscribe(json!({"stores": ["config", "tasks"]}))
        .await;
    let (_, commit) = writer
        .commit(
            "config.set",
            json!({"key": "project.name", "value": "renamed"}),
        )
        .await;
    let event = observer.event().await;
    assert_eq!(event["subscription"], sub.id, "{event}");
    assert_eq!(event["seq"], 1, "{event}");
    assert_eq!(event["store"], "config", "{event}");
    assert_eq!(event["kind"], "updated", "{event}");
    assert!(
        event.get("id").is_none(),
        "store-wide change has no id: {event}"
    );
    assert!(event.get("resync").is_none(), "{event}");
    assert_eq!(event["commit_seq"], commit, "{event}");

    // And a following entity change continues the same sequence.
    let task = create_task(&mut writer, "after config", json!({})).await;
    let next = observer.event().await;
    assert_eq!(next["seq"], 2, "{next}");
    assert_eq!(
        change(&next),
        ("tasks", Some(task.as_str()), "created"),
        "{next}"
    );
    daemon.shutdown().await.unwrap();
}

// ---------------------------------------------------------------------------
// Filters: matching on either side of a change
// ---------------------------------------------------------------------------

/// `ids` matches only the listed entities, and a change that does not match
/// does not consume a sequence number (a filter must not fake a gap).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_ids_filter_matches_only_listed_entities_and_does_not_fake_a_gap() {
    let daemon = harness().await;
    let mut writer = Wire::open(&daemon, "cli").await;
    let wanted = create_task(&mut writer, "wanted", json!({})).await;
    let other = create_task(&mut writer, "other", json!({})).await;
    let mut observer = Wire::open(&daemon, "tui").await;
    observer
        .subscribe(json!({"stores": ["tasks"], "filter": {"ids": [wanted]}}))
        .await;
    for n in 0..3 {
        writer
            .ok(
                "task.update_fields",
                json!({"id": other, "title": format!("o{n}")}),
            )
            .await;
    }
    writer
        .ok("task.update_fields", json!({"id": wanted, "title": "w"}))
        .await;
    let event = observer.event().await;
    assert_eq!(
        change(&event),
        ("tasks", Some(wanted.as_str()), "updated"),
        "{event}"
    );
    assert_eq!(
        event["seq"], 1,
        "non-matching changes consumed seq: {event}"
    );
    daemon.shutdown().await.unwrap();
}

/// The agent filter holds on **both** sides of a reassignment: the agent that
/// loses the task and the agent that gains it are both told; a third agent is
/// not (proved by its first event being the marker).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reassigning_a_task_invalidates_the_old_and_the_new_assignee_but_not_a_bystander() {
    let daemon = harness().await;
    let mut writer = Wire::open(&daemon, "cli").await;
    let (a, b, c) = (
        create_agent(&mut writer, "alpha").await,
        create_agent(&mut writer, "bravo").await,
        create_agent(&mut writer, "charlie").await,
    );
    let task = create_task(&mut writer, "movable", json!({"assignee": a})).await;
    let mut observer = Wire::open(&daemon, "tui").await;
    let old = observer
        .subscribe(json!({"stores": ["tasks"], "filter": {"agent_id": a}}))
        .await;
    let new = observer
        .subscribe(json!({"stores": ["tasks"], "filter": {"agent_id": b}}))
        .await;
    let bystander = observer
        .subscribe(json!({"stores": ["tasks"], "filter": {"agent_id": c}}))
        .await;

    writer
        .ok("task.assign", json!({"id": task, "agent_id": b}))
        .await;
    let marker = create_task(&mut writer, "for the bystander", json!({"assignee": c})).await;

    let mut seen: Vec<(u64, u64, Value)> = Vec::new();
    // Old + new assignee: one event each for the reassignment; the bystander's
    // first event is the marker, so the reassignment never reached it.
    while seen.len() < 3 {
        let event = observer.event().await;
        seen.push((
            event["subscription"].as_u64().unwrap(),
            event["seq"].as_u64().unwrap(),
            event,
        ));
    }
    let find = |sub: u64| seen.iter().find(|(s, _, _)| *s == sub).unwrap();
    let (_, seq, event) = find(old.id);
    assert_eq!(
        (*seq, change(event)),
        (1, ("tasks", Some(task.as_str()), "updated")),
        "{event}"
    );
    let (_, seq, event) = find(new.id);
    assert_eq!(
        (*seq, change(event)),
        (1, ("tasks", Some(task.as_str()), "updated")),
        "{event}"
    );
    let (_, seq, event) = find(bystander.id);
    assert_eq!(
        (*seq, change(event)),
        (1, ("tasks", Some(marker.as_str()), "created")),
        "the bystander was told about the reassignment: {event}"
    );
    daemon.shutdown().await.unwrap();
}

/// A deleted task leaves no entity to read: the goal subscriber that held it
/// must still be told, with the id and kind `deleted`; an unrelated goal's
/// subscriber is not.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn deleting_a_task_invalidates_the_goal_that_held_it() {
    let daemon = harness().await;
    let mut writer = Wire::open(&daemon, "cli").await;
    let goal = writer.ok("goal.create", json!({"title": "g"})).await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let elsewhere = writer.ok("goal.create", json!({"title": "h"})).await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let task = create_task(&mut writer, "member", json!({"goal_id": goal})).await;
    let mut holder = Wire::open(&daemon, "tui").await;
    holder
        .subscribe(json!({"stores": ["tasks"], "filter": {"goal_id": goal}}))
        .await;
    let mut unrelated = Wire::open(&daemon, "tui").await;
    unrelated
        .subscribe(json!({"stores": ["tasks"], "filter": {"goal_id": elsewhere}}))
        .await;

    writer.ok("task.delete", json!({"id": task})).await;
    let marker = create_task(&mut writer, "other goal", json!({"goal_id": elsewhere})).await;

    let event = holder.event().await;
    assert_eq!(
        change(&event),
        ("tasks", Some(task.as_str()), "deleted"),
        "{event}"
    );
    assert_eq!(event["seq"], 1, "{event}");
    let event = unrelated.event().await;
    assert_eq!(
        change(&event),
        ("tasks", Some(marker.as_str()), "created"),
        "the unrelated goal was told about the deletion: {event}"
    );
    assert_eq!(event["seq"], 1, "{event}");
    daemon.shutdown().await.unwrap();
}

// ---------------------------------------------------------------------------
// Every writer, not only RPC
// ---------------------------------------------------------------------------

/// Context expiry is a daemon-internal writer: a subscriber is told that the
/// key was deleted although no client wrote anything.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn context_expiry_is_pushed_to_context_subscribers() {
    let clock = TestClock::new();
    let daemon = DaemonHarness::builder()
        .on_socket()
        .clock(clock.clone())
        .maintenance_timer(false)
        .start()
        .await
        .unwrap();
    let mut writer = Wire::open(&daemon, "cli").await;
    writer
        .ok(
            "context.set",
            json!({"key": "lapsing", "value": "v", "ttl_ms": 1000}),
        )
        .await;
    let mut observer = Wire::open(&daemon, "tui").await;
    observer.subscribe(json!({"stores": ["context"]})).await;
    clock.advance_ms(2_000);
    assert_eq!(daemon.step_expiry().await.unwrap(), 1);
    let event = observer.event().await;
    assert_eq!(event["store"], "context", "{event}");
    assert_eq!(event["seq"], 1, "{event}");
    assert!(
        event["kind"] == "deleted" && event["id"] == "lapsing"
            || event.get("id").is_none() && event["kind"] == "updated",
        "expiry must invalidate the context store (the key as deleted, or the \
         whole store): {event}"
    );
    daemon.shutdown().await.unwrap();
}

/// A collector completes a run on its own, long after the request that
/// launched it returned. The completion's commits reach subscribers, with no
/// client write in between.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_agent_completing_on_its_own_is_pushed_without_any_client_write() {
    let chatty = Arc::new(Chatty::default());
    let daemon = DaemonHarness::builder()
        .on_socket()
        .maintenance_timer(false)
        .processes(chatty.clone() as Arc<dyn ProcessManager>)
        .start()
        .await
        .unwrap();
    let mut control = Wire::open(&daemon, "cli").await;
    let agent = create_agent(&mut control, "quiet").await;
    let task = control
        .ok(
            "task.create",
            json!({"title": "runs and ends", "assignee": agent}),
        )
        .await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let mut observer = Wire::open(&daemon, "tui").await;
    observer
        .subscribe(json!({"stores": ["tasks", "runs"]}))
        .await;

    let tick = control.ok("dispatch.tick", json!({})).await;
    assert_eq!(tick["launched"], json!([task]), "{tick}");
    // Drain the launch's own events, then let the agent exit by itself.
    let before_exit = control.ok("daemon.status", json!({})).await;
    assert!(before_exit.is_object());
    let launch_events = {
        let mut seen = Vec::new();
        while seen.iter().all(|e: &Value| e["store"] != "runs") {
            seen.push(observer.event().await);
        }
        seen
    };
    assert!(
        launch_events
            .iter()
            .any(|e| e["store"] == "tasks" && e["id"] == task.as_str()),
        "{launch_events:?}"
    );
    let last_seq = launch_events.last().unwrap()["seq"].as_u64().unwrap();

    for notifier in chatty.notifiers.lock().unwrap().iter() {
        notifier.notify(ExitOutcome {
            code: Some(0),
            signal: None,
        });
    }
    // The completion arrives as pushed changes of the task (and its run).
    let mut task_event = None;
    for _ in 0..16 {
        let event = observer.event().await;
        assert!(event["seq"].as_u64().unwrap() > last_seq, "{event}");
        if event["store"] == "tasks" && event["id"] == task.as_str() {
            task_event = Some(event);
            break;
        }
    }
    assert!(
        task_event.is_some(),
        "the completion of {task} was never pushed"
    );
    daemon.shutdown().await.unwrap();
}

// ---------------------------------------------------------------------------
// Lifecycle
// ---------------------------------------------------------------------------

/// After `unsubscribe` is acknowledged, nothing more is pushed for that
/// subscription; the other subscription of the same session keeps its numbers.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn no_event_is_pushed_for_a_subscription_after_its_unsubscribe_is_acknowledged() {
    let daemon = harness().await;
    let mut writer = Wire::open(&daemon, "cli").await;
    let mut observer = Wire::open(&daemon, "tui").await;
    let gone = observer.subscribe(json!({"stores": ["context"]})).await;
    let kept = observer.subscribe(json!({"stores": ["context"]})).await;
    writer
        .ok("context.set", json!({"key": "a", "value": "1"}))
        .await;
    let first = observer.event().await;
    let second = observer.event().await;
    let mut ids = [
        first["subscription"].as_u64(),
        second["subscription"].as_u64(),
    ];
    ids.sort();
    let mut want = [Some(gone.id), Some(kept.id)];
    want.sort();
    assert_eq!(
        ids, want,
        "both subscriptions are told once: {first} {second}"
    );

    observer
        .ok("unsubscribe", json!({"subscription": gone.id}))
        .await;
    writer
        .ok("context.set", json!({"key": "a", "value": "2"}))
        .await;
    for _ in 0..1 {
        let event = observer.event().await;
        assert_eq!(event["subscription"], kept.id, "{event}");
        assert_eq!(event["seq"], 2, "{event}");
    }
    // Nothing else is queued for the removed subscription.
    writer
        .ok("context.set", json!({"key": "a", "value": "3"}))
        .await;
    let event = observer.event().await;
    assert_eq!(event["subscription"], kept.id, "{event}");
    assert_eq!(event["seq"], 3, "{event}");
    daemon.shutdown().await.unwrap();
}

/// A subscriber that disconnects with events still queued releases
/// everything: writers keep committing at full speed and the daemon stops
/// cleanly (a leaked queue or task would hold the stop open).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_subscriber_that_disconnects_with_queued_events_leaves_nothing_behind() {
    let daemon = harness().await;
    let mut writer = Wire::open(&daemon, "cli").await;
    {
        let mut observer = Wire::open(&daemon, "tui").await;
        observer.subscribe(json!({"stores": ["context"]})).await;
        for n in 0..300 {
            writer
                .ok("context.set", json!({"key": "q", "value": format!("{n}")}))
                .await;
        }
        // Dropped with 300 unread events queued.
    }
    eventually!("the observer session to be reaped", {
        let status = writer.ok("daemon.status", json!({})).await;
        status["sessions"] == 1
    });
    writer
        .ok("context.set", json!({"key": "q", "value": "after"}))
        .await;
    daemon.shutdown().await.unwrap();
}

/// A subscription belongs to the daemon lifetime it was made in: its ack and
/// every event carry the instance from the welcome, and a different daemon
/// (here: a second harness) reports a different instance.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn events_carry_the_instance_of_the_daemon_that_pushed_them() {
    let first = harness().await;
    let second = harness().await;
    let mut a = Wire::open(&first, "tui").await;
    let b = Wire::open(&second, "tui").await;
    assert_ne!(a.instance, b.instance, "two daemons, one instance id");
    a.subscribe(json!({"stores": ["context"]})).await;
    let mut writer = Wire::open(&first, "cli").await;
    writer
        .ok("context.set", json!({"key": "i", "value": "1"}))
        .await;
    let event = a.event().await;
    assert_eq!(event["instance"], a.instance, "{event}");
    assert_ne!(event["instance"], b.instance, "{event}");
    first.shutdown().await.unwrap();
    second.shutdown().await.unwrap();
}
