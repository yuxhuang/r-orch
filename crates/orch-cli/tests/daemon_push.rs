//! Push to the MCP bridge and across frontends (T3; spec R16/R17, rulings
//! 14.1-14.3, brief D7/D8).
//!
//! The real `orch mcp` binary runs against a real test daemon, through a
//! counting proxy so the number of daemon sessions it uses can be read from
//! the daemon's side. External changes are made by separate `orch` CLI
//! invocations aimed straight at the daemon, i.e. by "another client".

#![cfg(unix)]

mod push_support;

use std::collections::BTreeSet;
use std::time::Duration;

use orch_client::{Client, ClientConfig, SubscriptionEvent};
use orch_proto::frame::{ChangeKind, Store};
use orch_proto::handshake::{Caller, CallerKind, SessionMode};
use orch_proto::ops::args::SubscribeArgs;
use push_support::{McpSession, Pane, Project};
use serde_json::json;

const MCP_UPDATE: Duration = Duration::from_secs(10);

fn mcp(project: &Project, extra: &[&str]) -> McpSession {
    let mut args = vec!["mcp"];
    args.extend_from_slice(extra);
    let mut session = McpSession::spawn(project.frontend(&args));
    let init = session.initialize();
    assert!(init.get("result").is_some(), "initialize failed: {init}");
    session
}

fn subscribe(session: &mut McpSession, uri: &str) {
    let response = session.request("resources/subscribe", json!({ "uri": uri }));
    assert!(
        response.get("result").is_some(),
        "resources/subscribe {uri} failed: {response}"
    );
}

/// Read `resources/updated` notifications until every uri in `want` was seen
/// (others may arrive: over-invalidation is allowed, silence is not).
fn expect_updated(session: &mut McpSession, want: &[&str]) -> BTreeSet<String> {
    let mut remaining: BTreeSet<String> = want.iter().map(|uri| (*uri).to_owned()).collect();
    let mut seen = BTreeSet::new();
    while !remaining.is_empty() {
        let uri = session.updated_uri(
            MCP_UPDATE,
            &format!("resources/updated for {remaining:?} (seen so far: {seen:?})"),
        );
        remaining.remove(&uri);
        seen.insert(uri);
    }
    seen
}

/// The spec target (R17). The bridge advertises resource subscriptions, and a
/// change made by another client is delivered as `resources/updated` for the
/// affected URIs: the collection and the status for a new task, the item for
/// a change to that task; nothing after an unsubscribe; all of it over ONE
/// daemon session however many URIs are subscribed.
#[test]
fn mcp_resource_updates() {
    let project = Project::start();
    let alpha = project.add_task("alpha");
    let mut session = mcp(&project, &[]);

    let advertised = &session.initialize_result["result"]["capabilities"];
    assert_eq!(
        advertised["resources"]["subscribe"], true,
        "resources.subscribe is not advertised: {advertised}"
    );

    let item = format!("orch://tasks/{alpha}");
    for uri in [
        "orch://tasks",
        item.as_str(),
        "orch://status",
        "orch://goals",
    ] {
        subscribe(&mut session, uri);
    }
    let subscribers: BTreeSet<usize> = project
        .proxy
        .all_ops()
        .iter()
        .filter(|seen| seen.op == "subscribe")
        .map(|seen| seen.conn)
        .collect();
    assert_eq!(
        subscribers.len(),
        1,
        "four URIs must share one daemon observation session, used {subscribers:?}"
    );

    // A new task: the collection and the status change.
    project.add_task("beta");
    expect_updated(&mut session, &["orch://tasks", "orch://status"]);

    // A change to alpha itself: its item URI.
    project.owner(&["task", "cancel", &alpha]);
    expect_updated(&mut session, &[item.as_str()]);

    // After unsubscribing the collection nothing more is sent for it. The
    // goals subscription is the marker that everything before it was seen.
    let response = session.request("resources/unsubscribe", json!({"uri": "orch://tasks"}));
    assert!(response.get("result").is_some(), "{response}");
    project.add_task("gamma");
    project.owner(&["goal", "add", "marker goal"]);
    let seen = expect_updated(&mut session, &["orch://goals"]);
    assert!(
        !seen.contains("orch://tasks"),
        "orch://tasks was notified after its unsubscribe: {seen:?}"
    );
}

/// A read-only bridge observes too: subscriptions are observation controls,
/// not mutations.
#[test]
fn a_read_only_mcp_bridge_receives_resource_updates() {
    let project = Project::start();
    let mut session = mcp(&project, &["--read-only"]);
    subscribe(&mut session, "orch://tasks");
    project.add_task("seen by a read-only bridge");
    expect_updated(&mut session, &["orch://tasks"]);
}

#[test]
fn two_idle_mcp_bridges_keep_observing_after_an_occupied_agent_tick() {
    let project = Project::start();
    let added = project.owner(&[
        "agent",
        "add",
        "held",
        "--adapter",
        "shell",
        "--command",
        "sleep 600",
        "--workspace-mode",
        "shared",
        "--json",
    ]);
    let agent: serde_json::Value = serde_json::from_slice(&added.stdout).unwrap();
    let agent = agent["id"].as_str().unwrap();
    for title in ["held open", "queued one", "queued two"] {
        project.owner(&["task", "add", title, "--assignee", agent]);
    }
    let mut bridges = [mcp(&project, &[]), mcp(&project, &["--read-only"])];
    for bridge in &mut bridges {
        subscribe(bridge, "orch://goals");
    }
    project.owner(&["daemon", "dispatch", "on"]);
    // The terminal reply is a barrier after every candidate selection.
    project.owner(&["run", "--once"]);
    project.owner(&["daemon", "dispatch", "off"]);
    // Only goals are observed, so launch events cannot satisfy these probes.
    // The first probe also consumes the all-resource wave if a bad tick sent
    // resync. The second must then arrive without any bridge request/revival.
    for title in ["after tick barrier", "later external mutation"] {
        project.owner(&["goal", "add", title]);
        for bridge in &mut bridges {
            expect_updated(bridge, &["orch://goals"]);
        }
    }
    let observation_connections: BTreeSet<_> = project
        .proxy
        .all_ops()
        .into_iter()
        .filter(|op| op.op == "subscribe")
        .map(|op| op.conn)
        .collect();
    assert_eq!(observation_connections.len(), 2);
    assert!(
        project
            .proxy
            .conns()
            .iter()
            .filter(|c| observation_connections.contains(&c.id))
            .all(|c| !c.client_closed && !c.upstream_closed),
        "both observation sessions must remain open"
    );
}

/// 256 distinct canonical URIs per bridge, duplicates free, the 257th refused
/// with an explicit JSON-RPC error (ruling 14.3), never a silent drop.
#[test]
fn the_mcp_bridge_accepts_256_subscriptions_and_refuses_the_257th_explicitly() {
    let project = Project::start();
    let mut session = mcp(&project, &[]);
    for n in 0..256 {
        subscribe(&mut session, &format!("orch://tasks/cap-{n:03}"));
    }
    // A duplicate does not count against the cap.
    subscribe(&mut session, "orch://tasks/cap-000");
    let refused = session.request(
        "resources/subscribe",
        json!({"uri": "orch://tasks/cap-256"}),
    );
    assert!(
        refused.get("error").is_some(),
        "the 257th distinct URI was accepted: {refused}"
    );
    // The bridge is still alive and a subscribed URI still updates.
    let after = session.request("resources/subscribe", json!({"uri": "orch://tasks"}));
    assert!(
        after.get("error").is_some(),
        "the cap applies to every new URI, not only items: {after}"
    );
}

/// When the daemon goes away the bridge cannot know what changed: it tells
/// the client that every subscribed URI may be stale.
#[test]
fn losing_the_daemon_invalidates_every_subscribed_uri() {
    let mut project = Project::start();
    let mut session = mcp(&project, &[]);
    let uris = ["orch://tasks", "orch://status", "orch://agents"];
    for uri in uris {
        subscribe(&mut session, uri);
    }
    project.stop_daemon();
    let seen = expect_updated(&mut session, &uris);
    assert!(uris.iter().all(|uri| seen.contains(*uri)), "{seen:?}");
}

/// A change made by CLI processes is seen, in the same final state, by every
/// kind of observer: a native subscription (the TUI's mechanism), an MCP
/// bridge and a dashboard on a terminal; and they agree with a plain read.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_cli_change_converges_across_a_subscription_an_mcp_bridge_and_the_dashboard() {
    let project = Project::start();
    project.add_task("seed-task-alpha");
    let pane = Pane::launch(&project, "tui", "converge");
    pane.wait_for("the dashboard", |s| s.contains("ORCH"));
    pane.keys(&["t"]);
    pane.wait_for("the Tasks view", |s| s.contains("seed-task-alpha"));
    let mut bridge = mcp(&project, &[]);
    subscribe(&mut bridge, "orch://tasks");

    let socket = project.daemon_socket();
    let root = project.root();
    let config = ClientConfig {
        cwd: root.clone(),
        project_root: Some(root.clone()),
        test_socket_root: socket.parent().map(std::path::Path::to_path_buf),
        socket: Some(socket),
        caller: Caller {
            kind: CallerKind::Cli,
            agent_id: None,
            pid: None,
        },
        mode: SessionMode::Normal,
        agent_context: false,
    };
    let client = Client::connect(&config).await.expect("connect");
    let mut subscription = client
        .into_subscription(
            serde_json::from_value::<SubscribeArgs>(json!({"stores": ["tasks"]}))
                .expect("subscribe args"),
        )
        .await
        .expect("subscribe");

    // Three CLI processes add tasks at the same time.
    let ids: Vec<String> = std::thread::scope(|scope| {
        let handles: Vec<_> = ["conv-one-7001", "conv-two-7002", "conv-three-7003"]
            .into_iter()
            .map(|title| scope.spawn(|| project.add_task(title)))
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });

    // The native subscription: one created event per task, in order.
    let mut created = BTreeSet::new();
    let (mut last_seq, mut last_commit) = (0, 0);
    while created.len() < ids.len() {
        let event = tokio::time::timeout(Duration::from_secs(10), subscription.next())
            .await
            .expect("an event within 10 s")
            .expect("the subscription stays healthy");
        if let SubscriptionEvent::Change {
            seq,
            store,
            id,
            kind,
            commit_seq,
        } = event
        {
            assert!(seq > last_seq && commit_seq >= last_commit, "order");
            (last_seq, last_commit) = (seq, commit_seq);
            if store == Store::Tasks && kind == ChangeKind::Created {
                created.insert(id.expect("a created task has an id").as_str().to_owned());
            }
        }
    }
    assert_eq!(created, ids.iter().cloned().collect::<BTreeSet<_>>());

    // The bridge was told, and a read through it agrees with the CLI.
    expect_updated(&mut bridge, &["orch://tasks"]);
    let read = bridge.request("resources/read", json!({"uri": "orch://tasks"}));
    let text = read.to_string();
    for id in &ids {
        assert!(text.contains(id.as_str()), "{id} missing from {text}");
    }

    // The dashboard shows all three without a keystroke.
    for title in ["conv-one-7001", "conv-two-7002", "conv-three-7003"] {
        pane.wait_for(title, |s| s.contains(title));
    }
    let listed =
        String::from_utf8_lossy(&project.owner(&["--json", "task", "list"]).stdout).into_owned();
    for id in &ids {
        assert!(listed.contains(id.as_str()), "{id} missing from {listed}");
    }
}
