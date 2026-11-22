//! Issue 114: the probe adapter's release gate must not carry credit from one
//! dispatch pass into the next.
//!
//! `ProbeAdapter` holds every scripted stream until the test calls
//! `release()`, so a pass never races a run that finishes instantly. A release
//! that left credit behind let a later pass's stream run as soon as its
//! collector was scheduled, which can be before `daemon_tick` publishes that
//! pass's `orchestrator:tick`. The invariance suite compares the complete
//! ordered event array, so either order of the two events is a failure.
//!
//! These tests poll a stream once, with no waiting, before its own release.
//! They are deterministic: no sleeps, no timeouts, no scheduler timing.

mod token_eco_support;

use futures::{FutureExt, StreamExt};
use orch_core::adapter::{Adapter, AgentEvent, ExecuteHandle, ExecuteParams};
use orch_core::test_support::make_agent;
use token_eco_support::{ProbeAdapter, Rig, RigSpec, claude_agent, task};
use tokio_util::sync::CancellationToken;

fn params(cancel: CancellationToken) -> ExecuteParams {
    ExecuteParams {
        prompt: "p".into(),
        system_prompt: None,
        workspace: std::path::PathBuf::new(),
        env: Default::default(),
        inherit_env: Default::default(),
        config: make_agent().config,
        cancel,
    }
}

fn start(adapter: &ProbeAdapter) -> ExecuteHandle {
    adapter
        .execute(params(CancellationToken::new()))
        .expect("execute")
}

/// One poll with no waiting: `None` means the stream is not ready.
fn ready_now(
    handle: &mut ExecuteHandle,
) -> Option<Option<Result<AgentEvent, orch_core::adapter::AdapterError>>> {
    handle.events.next().now_or_never()
}

async fn drain(handle: ExecuteHandle) -> usize {
    handle.events.count().await
}

#[tokio::test]
async fn a_later_dispatchs_stream_waits_for_its_own_release() {
    let adapter = ProbeAdapter::new();

    let first = start(&adapter);
    adapter.release();
    assert_eq!(drain(first).await, 2, "the first pass ran when released");

    let mut second = start(&adapter);
    assert!(
        ready_now(&mut second).is_none(),
        "the first pass's release must not unblock a stream the next pass creates"
    );
    adapter.release();
    assert_eq!(drain(second).await, 2, "its own release lets it run");
}

#[tokio::test]
async fn one_release_accounts_for_every_stream_of_the_pass() {
    let adapter = ProbeAdapter::new();
    let mut pass: Vec<_> = (0..3).map(|_| start(&adapter)).collect();
    for handle in &mut pass {
        assert!(ready_now(handle).is_none(), "gated until released");
    }

    adapter.release();
    for handle in pass {
        assert_eq!(drain(handle).await, 2, "every stream of the pass ran");
    }

    let mut next_pass = start(&adapter);
    assert!(
        ready_now(&mut next_pass).is_none(),
        "streams created after the release wait for the next one"
    );
}

#[tokio::test]
async fn releasing_an_empty_pass_banks_nothing() {
    let adapter = ProbeAdapter::new();
    adapter.release(); // nothing is waiting: a tick that dispatched nothing
    adapter.release();

    let mut later = start(&adapter);
    assert!(
        ready_now(&mut later).is_none(),
        "a release with no stream waiting must not pre-open a later stream"
    );
    adapter.release();
    assert_eq!(drain(later).await, 2);
}

#[tokio::test]
async fn a_hung_execution_ignores_release_and_banks_nothing() {
    let adapter = ProbeAdapter::new();

    // A long-running agent: it ignores the gate and ends only on cancel.
    adapter.hang_next(1);
    let cancel = CancellationToken::new();
    let mut hung = adapter.execute(params(cancel.clone())).expect("hung");
    let gated = start(&adapter);

    adapter.release();
    assert_eq!(drain(gated).await, 2, "the gated stream of the pass ran");
    assert!(
        ready_now(&mut hung).is_none(),
        "release does not end a hung run"
    );

    let mut later = start(&adapter);
    assert!(
        ready_now(&mut later).is_none(),
        "a pass with a hung run leaves no credit for the next"
    );

    cancel.cancel();
    assert!(
        hung.events.next().await.is_none(),
        "cancel ends the hung stream"
    );
    adapter.release();
    assert_eq!(drain(later).await, 2);
}

#[tokio::test]
async fn a_failed_execution_leaves_the_pass_releasable_and_banks_nothing() {
    let adapter = ProbeAdapter::new();
    let before = start(&adapter);

    adapter.fail_execute("spawn failed");
    assert!(
        adapter.execute(params(CancellationToken::new())).is_err(),
        "the failed execution yields no stream"
    );

    adapter.release();
    assert_eq!(drain(before).await, 2, "the pass still releases");
}

#[tokio::test]
async fn a_stream_dropped_before_release_does_not_bank_credit() {
    let adapter = ProbeAdapter::new();
    drop(start(&adapter)); // cancelled run whose collector dropped the stream
    adapter.release();

    let mut later = start(&adapter);
    assert!(
        ready_now(&mut later).is_none(),
        "the dropped stream's release must not carry over"
    );
    adapter.release();
    assert_eq!(drain(later).await, 2);
}

/// The same property through the real tick and collector. A collector is
/// spawned before its tick is published, so the order of the two events inside
/// a pass is decided only by the gate: the tick must come first.
#[tokio::test(flavor = "multi_thread")]
async fn every_dispatch_pass_publishes_its_tick_before_its_collector_emits() {
    let rig = Rig::new(RigSpec {
        agents: vec![claude_agent("agt_a", "Alpha")],
        tasks: vec![
            task("tsk_one", Some("agt_a")),
            task("tsk_two", Some("agt_a")),
        ],
        ..Default::default()
    })
    .await;

    for pass in 1..=2 {
        let seen = rig.events().len();
        let _ = rig.orchestrator.daemon_tick(true).await;
        assert_eq!(rig.adapter.calls().len(), pass, "one task per pass");
        assert_eq!(
            rig.adapter.gated_streams(),
            1,
            "pass {pass}: its stream is held until the pass is released"
        );
        rig.adapter.release();
        assert_eq!(rig.adapter.gated_streams(), 0);
        rig.orchestrator.wait_for_collectors().await;

        let types: Vec<String> = rig
            .events()
            .iter()
            .skip(seen)
            .map(|e| serde_json::to_value(e).unwrap()["type"].to_string())
            .collect();
        let tick = types
            .iter()
            .position(|t| t == "\"orchestrator:tick\"")
            .unwrap_or_else(|| panic!("pass {pass}: no tick in {types:?}"));
        let output = types
            .iter()
            .position(|t| t == "\"agent:output\"")
            .unwrap_or_else(|| panic!("pass {pass}: no output in {types:?}"));
        assert!(
            tick < output,
            "pass {pass}: the collector emitted before the tick: {types:?}"
        );
    }
}
