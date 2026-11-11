//! Synthetic domains are only for non-forking managers. These controls catch
//! accidental Process fallback, lost cancellation cleanup and token misuse.
#![cfg(target_os = "macos")]

use super::*;
use crate::models::{DiscoveryRuntime, SnapshotDiscoveryRuntime};
use crate::process::{EnvSnapshot, SnapshotEnvProcessManager};
use orch_core::adapter::{Adapter, ExecuteParams};
use orch_core::process::{ProcessManager, SpawnOptions};
use orch_core::test_support::{FakeProcessManager, make_agent, make_task};
use orch_core::workspace::WorkspaceManager;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, mpsc};
use std::time::Duration;

const BOUND: Duration = Duration::from_secs(4);
const REJECTION: &str = "synthetic spawn reservation cannot authorize a real process";

fn fresh() -> SpawnDomain {
    SpawnDomain::for_synthetic_processes()
}

async fn granted(domain: &SpawnDomain) -> SpawnReservation {
    tokio::time::timeout(BOUND, domain.reserve(SpawnSite::Launch))
        .await
        .expect("domain admission was not released")
}

// Factory reservations intentionally have no injected observer. Capture their
// own private diagnostic dispatch to identify records without a global count.
#[derive(Clone)]
struct Records(mpsc::Sender<u64>);
impl tracing::Subscriber for Records {
    fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
        true
    }
    fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }
    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
    fn enter(&self, _: &tracing::span::Id) {}
    fn exit(&self, _: &tracing::span::Id) {}
    fn event(&self, event: &tracing::Event<'_>) {
        struct Id(Option<u64>);
        impl tracing::field::Visit for Id {
            fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
                if field.name() == "spawn_id" {
                    self.0 = Some(value);
                }
            }
            fn record_debug(&mut self, _: &tracing::field::Field, _: &dyn std::fmt::Debug) {}
        }
        let mut id = Id(None);
        event.record(&mut id);
        if let Some(id) = id.0 {
            let _ = self.0.send(id);
        }
    }
}

async fn record_id(receive: mpsc::Receiver<u64>) -> u64 {
    tokio::task::spawn_blocking(move || receive.recv_timeout(BOUND).expect("own diagnostic record"))
        .await
        .unwrap()
}

#[tokio::test]
async fn same_domain_queues_until_the_holder_is_dropped() {
    let domain = fresh();
    let holder = granted(&domain).await;
    let mut queued = Box::pin(domain.reserve(SpawnSite::Launch));
    assert!(
        futures::poll!(&mut queued).is_pending(),
        "same domain admitted two holders"
    );
    drop(holder);
    tokio::time::timeout(BOUND, queued)
        .await
        .expect("queued reservation did not progress");
}

#[tokio::test]
async fn dropping_a_queued_reservation_removes_its_own_record() {
    let domain = fresh();
    let holder = granted(&domain).await;
    let (send, receive) = mpsc::channel();
    let mut queued = Box::pin(domain.reserve(SpawnSite::Workspace));
    let dispatch = tracing::subscriber::set_default(Records(send));
    assert!(futures::poll!(&mut queued).is_pending());
    drop(dispatch);
    let id = record_id(receive).await;
    assert!(
        pending_spawns()
            .iter()
            .any(|r| r.id == id && r.stage == SpawnStage::Waiting)
    );
    drop(queued);
    assert!(
        !pending_spawns().iter().any(|r| r.id == id),
        "cancelled reservation retained its record"
    );
    drop(holder);
    drop(granted(&domain).await);
}

#[tokio::test]
async fn different_domains_progress_while_the_first_holder_remains_live() {
    let first = fresh();
    let second = fresh();
    let _holder = granted(&first).await;
    let _independent = granted(&second).await;
}

#[tokio::test]
async fn successful_fake_scope_releases_its_permit() {
    let domain = fresh();
    assert_eq!(granted(&domain).await.run(|| 42), 42);
    drop(granted(&domain).await);
}

#[tokio::test]
async fn failed_fake_scope_releases_its_permit() {
    let domain = fresh();
    let result: Result<(), &str> = granted(&domain).await.run(|| Err("controlled failure"));
    assert_eq!(result, Err("controlled failure"));
    drop(granted(&domain).await);
}

#[tokio::test]
async fn unwinding_fake_scope_releases_its_permit_and_thread_slot() {
    let domain = fresh();
    let reservation = granted(&domain).await;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        reservation.run(|| panic!("controlled fake unwind"));
    }));
    assert!(result.is_err());
    granted(&domain).await.run(|| ());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn synthetic_handoff_clears_the_previous_matching_record_before_next_grant() {
    let domain = fresh();
    let (send, receive) = mpsc::channel();
    let mut first = Box::pin(domain.reserve(SpawnSite::Launch));
    let dispatch = tracing::subscriber::set_default(Records(send));
    let first = match futures::poll!(&mut first) {
        std::task::Poll::Ready(reservation) => reservation,
        std::task::Poll::Pending => panic!("fresh synthetic domain was occupied"),
    };
    drop(dispatch);
    let old_id = record_id(receive).await;
    let next_domain = domain.clone();
    let mut next = Box::pin(async move { next_domain.reserve(SpawnSite::Launch).await });
    assert!(futures::poll!(&mut next).is_pending());
    let job = tokio::spawn(async move {
        let next = tokio::time::timeout(BOUND, next).await.unwrap();
        assert!(
            !pending_spawns().iter().any(|r| r.id == old_id),
            "handoff exposed the old matching record"
        );
        next.run(|| ());
    });
    drop(first);
    job.await.unwrap();
}

// This manager supplies bytes only; it cannot reach a Command builder. Version
// and help responses are deliberately distinct, so bypassing either probe fails.
#[derive(Default)]
struct WireProcess(Mutex<Vec<Vec<String>>>);
#[async_trait::async_trait]
impl ProcessManager for WireProcess {
    fn is_alive(&self, _: u32) -> bool {
        false
    }
    fn kill(&self, _: u32, _: orch_core::process::Signal) {}
    async fn kill_with_grace(&self, _: u32, _: Duration) {}
    fn spawn(
        &self,
        command: &str,
        args: &[String],
        _: SpawnOptions,
    ) -> orch_core::Result<orch_core::process::SpawnedChild> {
        assert_eq!(command, "opencode");
        self.0.lock().unwrap().push(args.to_vec());
        let output = match args
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .as_slice()
        {
            ["--version"] => "opencode v2.1.2\n",
            ["run", "--help"] => "--model provider/model#variant\n",
            _ => panic!("unexpected effort-wire argv"),
        };
        Ok(orch_core::process::SpawnedChild {
            pid: 0,
            stdout: Some(Box::new(std::io::Cursor::new(output.as_bytes().to_vec()))),
            stderr: None,
            stdin: None,
            exit: orch_core::process::ExitSignal::resolved(orch_core::process::ExitOutcome {
                code: Some(0),
                signal: None,
            }),
        })
    }
}

#[tokio::test]
async fn opencode_effort_wire_uses_the_injected_domain_while_process_admission_is_held() {
    let holder = tokio::time::timeout(Duration::from_secs(20), reserve_spawn(SpawnSite::Launch))
        .await
        .unwrap();
    let pm = Arc::new(WireProcess::default());
    let adapter = crate::adapters::OpenCodeAdapter::new(pm.clone()).with_spawn_domain(fresh());
    let root = tempfile::tempdir().unwrap();
    let params = ExecuteParams {
        prompt: "private fixture".into(),
        system_prompt: None,
        workspace: root.path().to_owned(),
        env: Default::default(),
        inherit_env: Default::default(),
        config: orch_domain::AgentConfig {
            model: Some("openrouter/anthropic/claude-opus-5.5".into()),
            effort: Some(orch_domain::ReasoningEffort::Xhigh),
            ..Default::default()
        },
        cancel: Default::default(),
    };
    // Polling once drives both already-resolved in-memory probes. A fallback to
    // Process is Pending; no timer or scheduling assumption is the oracle.
    let mut preparation = Box::pin(adapter.prepare_effort(&params));
    let result = futures::poll!(&mut preparation);
    drop(preparation);
    drop(holder);
    let std::task::Poll::Ready(Ok(prepared)) = result else {
        panic!("effort preparation waited on foreign Process admission");
    };
    assert_eq!(
        prepared.config.effort,
        Some(orch_domain::ReasoningEffort::Xhigh)
    );
    assert_eq!(
        *pm.0.lock().unwrap(),
        vec![
            vec!["--version".to_owned()],
            vec!["run".to_owned(), "--help".to_owned()]
        ]
    );
}

#[tokio::test]
async fn workspace_preparation_uses_the_injected_domain_while_process_admission_is_held() {
    let root = tempfile::tempdir().unwrap();
    let pm = Arc::new(
        FakeProcessManager::new().with_exit(orch_core::process::ExitOutcome {
            code: Some(1),
            signal: None,
        }),
    );
    let manager = crate::workspace::GitWorkspaceManager::new(
        root.path().into(),
        root.path().join(".orchestry"),
        pm.clone(),
    )
    .with_spawn_domain(fresh());
    let mut task = make_task();
    task.workspace_mode = Some(orch_domain::WorkspaceMode::Worktree);
    let agent = make_agent();
    let config = orch_domain::default_config();
    let holder = tokio::time::timeout(Duration::from_secs(20), reserve_spawn(SpawnSite::Launch))
        .await
        .unwrap();
    let mut preparation = Box::pin(manager.prepare(&task, &agent, &config));
    let result = futures::poll!(&mut preparation);
    drop(preparation);
    drop(holder);
    let std::task::Poll::Ready(Err(error)) = result else {
        panic!("workspace preparation waited on foreign Process admission");
    };
    assert!(
        error.to_string().contains("requires a git repository"),
        "{error}"
    );
    assert_eq!(
        pm.spawned(),
        vec![(
            "git".into(),
            vec!["rev-parse".into(), "--is-inside-work-tree".into()]
        )]
    );
}

#[derive(Default)]
struct Observations(Mutex<Vec<SpawnObservation>>);
impl SpawnObserver for Observations {
    fn observe(&self, record: SpawnObservation) {
        self.0.lock().unwrap().push(record);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn real_process_handoff_removes_the_outgoing_id_before_successor_enters() {
    struct Successor {
        previous: u64,
        events: Arc<Observations>,
    }
    impl SpawnObserver for Successor {
        fn observe(&self, record: SpawnObservation) {
            if record.stage == SpawnStage::Entered {
                assert!(
                    !pending_spawns().iter().any(|r| r.id == self.previous),
                    "real Process handoff overlapped the outgoing matching record"
                );
            }
            self.events.observe(record);
        }
    }
    let outgoing = Arc::new(Observations::default());
    let first = tokio::time::timeout(
        Duration::from_secs(20),
        reserve_observed(SpawnSite::Launch, Some(outgoing.clone())),
    )
    .await
    .unwrap();
    let previous = outgoing.0.lock().unwrap()[0].id;
    let incoming = Arc::new(Observations::default());
    let observer = Arc::new(Successor {
        previous,
        events: incoming.clone(),
    });
    let mut next = Box::pin(reserve_observed(SpawnSite::Launch, Some(observer)));
    assert!(futures::poll!(&mut next).is_pending());
    let successor_id = incoming.0.lock().unwrap()[0].id;
    let job = tokio::spawn(async move {
        tokio::time::timeout(Duration::from_secs(20), next)
            .await
            .unwrap()
            .run(|| with_spawn_gate(|| ()));
    });
    first.run(|| {
        with_spawn_gate(|| {
            assert!(
                outgoing
                    .0
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|r| r.id == previous && r.stage == SpawnStage::Entered)
            );
        })
    });
    job.await.unwrap();
    let records = incoming.0.lock().unwrap();
    assert_eq!(
        records.iter().map(|r| (r.id, r.stage)).collect::<Vec<_>>(),
        vec![
            (successor_id, SpawnStage::Waiting),
            (successor_id, SpawnStage::Entered)
        ]
    );
    assert!(
        !pending_spawns()
            .iter()
            .any(|r| r.id == previous || r.id == successor_id)
    );
}

fn rejected(payload: Box<dyn std::any::Any + Send>) {
    let message = payload
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| payload.downcast_ref::<String>().map(String::as_str));
    assert_eq!(message, Some(REJECTION));
}

fn executable(root: &Path) -> EnvSnapshot {
    use std::os::unix::fs::PermissionsExt;
    let file = root.join("fake-spawn");
    std::fs::write(&file, "#!/bin/sh\nprintf started > \"$1\"\n").unwrap();
    std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o700)).unwrap();
    EnvSnapshot {
        base: vec![
            ("HOME".into(), root.as_os_str().to_owned()),
            ("PATH".into(), root.as_os_str().to_owned()),
        ],
        forced: vec![],
    }
}

#[derive(Clone, Copy)]
enum Builder {
    Helper,
    Manager,
    Hostname,
}

// For the fail-closed reservation attach an observer to its existing Notice,
// without changing the real builder or the production admission implementation.
async fn fail_closed(builder: Builder) {
    let root = tempfile::tempdir().unwrap();
    let marker = root.path().join("started");
    let manager = SnapshotEnvProcessManager::new(executable(root.path()));
    let domain = fresh();
    let events = Arc::new(Observations::default());
    let mut reservation = granted(&domain).await;
    // Use an observed Notice for the *same permit*. Dropping the original
    // unobserved notice avoids inventing or guessing a process-global id.
    reservation.notice = observe_spawn(SpawnSite::Launch, Some(events.clone())).0;
    let id = events.0.lock().unwrap()[0].id;
    let called = AtomicBool::new(false);
    let mut escaped_child = None;
    let action = || match builder {
        Builder::Helper => {
            with_spawn_gate(|| {
                called.store(true, Ordering::SeqCst);
            });
        }
        Builder::Manager => {
            let child = manager
                .spawn(
                    "fake-spawn",
                    &[marker.to_str().unwrap().into()],
                    SpawnOptions::default(),
                )
                .unwrap();
            escaped_child = Some(child);
        }
        Builder::Hostname => {
            assert!(
                crate::storage::remote::hostname_output()
                    .unwrap()
                    .status
                    .success()
            );
        }
    };
    let unwind = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| reservation.run(action)));
    // Also drain/reap when the disposable assertion-removal mutation escapes.
    if let Some(child) = escaped_child {
        assert!(child.exit.wait().await.success());
    }
    assert!(
        !called.load(Ordering::SeqCst),
        "helper closure ran with a synthetic token"
    );
    assert!(!marker.exists(), "manager spawned with a synthetic token");
    rejected(unwind.expect_err("real builder accepted a synthetic token"));
    assert!(
        !events
            .0
            .lock()
            .unwrap()
            .iter()
            .any(|r| r.id == id && r.stage == SpawnStage::Entered)
    );
    assert!(!pending_spawns().iter().any(|r| r.id == id));
    drop(granted(&domain).await);
    // Same route, ordinary Process admission, succeeds after the unwind.
    match builder {
        Builder::Helper => {
            with_spawn_gate(|| called.store(true, Ordering::SeqCst));
            assert!(called.load(Ordering::SeqCst));
        }
        Builder::Manager => {
            let child = manager
                .spawn(
                    "fake-spawn",
                    &[marker.to_str().unwrap().into()],
                    SpawnOptions::default(),
                )
                .unwrap();
            assert!(child.exit.wait().await.success());
            assert!(marker.exists());
        }
        Builder::Hostname => assert!(
            crate::storage::remote::hostname_output()
                .unwrap()
                .status
                .success()
        ),
    }
}

#[tokio::test]
async fn helper_rejects_synthetic_before_entering_and_recovers() {
    fail_closed(Builder::Helper).await;
}
#[tokio::test]
async fn manager_rejects_synthetic_before_spawning_and_recovers() {
    fail_closed(Builder::Manager).await;
}
#[tokio::test]
async fn hostname_rejects_synthetic_before_output_and_recovers() {
    fail_closed(Builder::Hostname).await;
}

#[test]
fn discovery_does_not_consume_the_calling_threads_synthetic_token() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let root = tempfile::tempdir().unwrap();
    let marker = root.path().join("started");
    let discovery = SnapshotDiscoveryRuntime::new(executable(root.path()));
    let reservation = runtime.block_on(granted(&fresh()));
    reservation.run(|| {
        runtime
            .block_on(discovery.run_command("fake-spawn", &[marker.to_str().unwrap()]))
            .unwrap();
        assert!(marker.exists());
        rejected(
            std::panic::catch_unwind(|| with_spawn_gate(|| ()))
                .expect_err("discovery consumed caller token"),
        );
    });
}

#[test]
fn discovery_enters_only_after_the_process_wide_holder_is_released() {
    struct Ordered {
        released: Arc<AtomicBool>,
        records: mpsc::Sender<(SpawnObservation, bool)>,
    }
    impl SpawnObserver for Ordered {
        fn observe(&self, record: SpawnObservation) {
            self.records
                .send((record, self.released.load(Ordering::SeqCst)))
                .unwrap();
        }
    }
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let root = tempfile::tempdir().unwrap();
    let marker = root.path().join("started");
    let discovery = SnapshotDiscoveryRuntime::new(executable(root.path()));
    let holder = runtime.block_on(async {
        tokio::time::timeout(Duration::from_secs(20), reserve_spawn(SpawnSite::Launch))
            .await
            .unwrap()
    });
    let released = Arc::new(AtomicBool::new(false));
    let (send, receive) = mpsc::channel();
    discovery.set_spawn_observer(Arc::new(Ordered {
        released: released.clone(),
        records: send,
    }));
    std::thread::scope(|scope| {
        let holder = holder; // release before scoped-thread join on every early unwind
        let job = scope.spawn(|| {
            let reservation = runtime.block_on(granted(&fresh()));
            reservation
                .run(|| {
                    runtime
                        .block_on(discovery.run_command("fake-spawn", &[marker.to_str().unwrap()]))
                })
                .unwrap();
        });
        let (waiting, _) = receive.recv_timeout(BOUND).unwrap();
        assert_eq!(waiting.stage, SpawnStage::Waiting);
        // Only the negative half uses a bounded observation window. The
        // observer at actual entry supplies the decisive ordering assertion.
        let premature = receive.recv_timeout(Duration::from_millis(50)).ok();
        let absent = !marker.exists();
        let own_waiting = pending_spawns()
            .iter()
            .any(|r| r.id == waiting.id && r.stage == SpawnStage::Waiting);
        released.store(true, Ordering::SeqCst);
        drop(holder);
        job.join().unwrap();
        let (entered, after_release) =
            premature.unwrap_or_else(|| receive.recv_timeout(BOUND).unwrap());
        assert_eq!(entered.id, waiting.id);
        assert_eq!(entered.stage, SpawnStage::Entered);
        assert!(
            after_release,
            "Discovery entered before the Process holder was released"
        );
        assert!(
            absent && own_waiting,
            "Discovery did not wait on its own Process record"
        );
    });
    assert!(marker.exists());
}
