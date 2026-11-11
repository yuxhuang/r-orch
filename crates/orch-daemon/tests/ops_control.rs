#![cfg(feature = "testing")]
//! Direct handler vectors while the S1a transport harness bodies are pending.
//! Same coordinator/service path used by the listener, isolated filesystem.
use orch_core::clock::{Clock, iso_from_epoch_ms};
use orch_daemon::coordinator::ProjectCoordinator;
use orch_daemon::ops::{Handler, OpContext, ServiceHandler};
use orch_engine::container::{EngineSeams, build_full_with};
use orch_proto::handshake::{Caller, CallerKind, Session, SessionMode};
use orch_proto::ops::{args::*, results::BatchResult};
use std::sync::{
    Arc,
    atomic::{AtomicI64, Ordering},
};
use std::time::Duration;

struct TestClock(AtomicI64);
impl Clock for TestClock {
    fn now_ms(&self) -> i64 {
        self.0.load(Ordering::SeqCst)
    }
    fn now_iso(&self) -> String {
        iso_from_epoch_ms(self.now_ms())
    }
}
async fn fixture() -> (
    tempfile::TempDir,
    ServiceHandler,
    OpContext,
    Arc<TestClock>,
    Arc<orch_engine::container::FullEngine>,
) {
    let temp = tempfile::tempdir().unwrap();
    std::fs::create_dir(temp.path().join(".orchestry")).unwrap();
    let clock = Arc::new(TestClock(AtomicI64::new(1_760_000_000_000)));
    let engine = Arc::new(
        build_full_with(
            temp.path(),
            EngineSeams {
                clock: clock.clone(),
                global_config_path: Some(temp.path().join("global.yml")),
                ..Default::default()
            },
        )
        .await
        .unwrap(),
    );
    let coordinator = Arc::new(ProjectCoordinator::new(engine.events.operation_boundary()));
    let handler = ServiceHandler::new(Arc::clone(&engine), coordinator);
    let ctx = OpContext::new(Session {
        caller: Caller {
            kind: CallerKind::Cli,
            agent_id: None,
            pid: None,
        },
        mode: SessionMode::Normal,
    });
    (temp, handler, ctx, clock, engine)
}
async fn task(h: &ServiceHandler, c: &OpContext, title: &str) -> String {
    let args = serde_json::from_value(serde_json::json!({"title":title})).unwrap();
    Handler::task_create(h, c, args).await.unwrap().id
}
async fn wait_batch(h: &ServiceHandler) {
    tokio::time::timeout(Duration::from_secs(2), async {
        while !h.dispatch_state().batch_active {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}
#[tokio::test]
async fn initial_on_rejects_batch_and_later_on_interrupts_without_restoring_off() {
    let (_temp, h, ctx, _clock, _engine) = fixture().await;
    Handler::dispatch_set(
        &h,
        &ctx,
        DispatchSetArgs {
            dispatch: DispatchSwitch::On,
        },
    )
    .await
    .unwrap();
    let err = Handler::dispatch_until_terminal(&h, &ctx, DispatchUntilTerminalArgs {})
        .await
        .unwrap_err();
    assert_eq!(err.exit_code, 2);
    assert!(err.message.contains("needs dispatch OFF"));
    Handler::dispatch_set(
        &h,
        &ctx,
        DispatchSetArgs {
            dispatch: DispatchSwitch::Off,
        },
    )
    .await
    .unwrap();
    task(
        &h,
        &ctx,
        "blocked unassigned work participates in all-task wait",
    )
    .await;
    let hc = h.clone();
    let cc = ctx.clone();
    let batch = tokio::spawn(async move {
        Handler::dispatch_until_terminal(&hc, &cc, DispatchUntilTerminalArgs {}).await
    });
    wait_batch(&h).await;
    let other = OpContext::new(ctx.session.clone());
    let err = Handler::dispatch_until_terminal(&h, &other, DispatchUntilTerminalArgs {})
        .await
        .unwrap_err();
    assert_eq!(err.exit_code, 4);
    Handler::dispatch_set(
        &h,
        &other,
        DispatchSetArgs {
            dispatch: DispatchSwitch::On,
        },
    )
    .await
    .unwrap();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(2), batch)
            .await
            .unwrap()
            .unwrap()
            .unwrap(),
        BatchResult::Interrupted
    );
    assert!(h.dispatch_state().dispatch_on);
    assert!(!h.dispatch_state().batch_active);
    h.disconnect(ctx.session_id).await.unwrap();
    assert!(h.dispatch_state().dispatch_on);
}
#[tokio::test]
async fn empty_batch_finishes_and_off_ends_blocked_batch_without_changing_switch() {
    let (_temp, h, ctx, _clock, _engine) = fixture().await;
    assert_eq!(
        Handler::dispatch_until_terminal(&h, &ctx, DispatchUntilTerminalArgs {})
            .await
            .unwrap(),
        BatchResult::AllDone
    );
    task(&h, &ctx, "blocked").await;
    let hc = h.clone();
    let cc = ctx.clone();
    let batch = tokio::spawn(async move {
        Handler::dispatch_until_terminal(&hc, &cc, DispatchUntilTerminalArgs {}).await
    });
    wait_batch(&h).await;
    let other = OpContext::new(ctx.session.clone());
    Handler::dispatch_set(
        &h,
        &other,
        DispatchSetArgs {
            dispatch: DispatchSwitch::Off,
        },
    )
    .await
    .unwrap();
    assert_eq!(batch.await.unwrap().unwrap(), BatchResult::Interrupted);
    assert!(!h.dispatch_state().dispatch_on);
}
#[tokio::test]
async fn expiry_rechecks_renewal_under_the_shared_boundary_while_dispatch_off() {
    let (_temp, h, ctx, clock, engine) = fixture().await;
    Handler::context_set(
        &h,
        &ctx,
        ContextSetArgs {
            key: "key".try_into().unwrap(),
            value: "old".try_into().unwrap(),
            ttl_ms: Some(1),
        },
    )
    .await
    .unwrap();
    clock.0.fetch_add(2, Ordering::SeqCst);
    let barrier = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let hb = h.clone();
    let entered = barrier.clone();
    let go = release.clone();
    let renewal = tokio::spawn(async move {
        hb.coordinator()
            .run_op(orch_daemon::coordinator::Lane::Ordinary, move |permit| {
                Box::pin(async move {
                    permit.invalidate(orch_proto::frame::Store::Context);
                    engine.context.set("key", "new", Some(100)).await.unwrap();
                    entered.notify_one();
                    go.notified().await;
                    Ok(())
                })
            })
            .await
            .unwrap()
    });
    barrier.notified().await;
    let he = h.clone();
    let expiry = tokio::spawn(async move { he.expire_context().await });
    tokio::task::yield_now().await;
    release.notify_one();
    renewal.await.unwrap();
    expiry.await.unwrap().unwrap();
    assert!(!h.dispatch_state().dispatch_on);
    assert_eq!(
        Handler::context_get(
            &h,
            &ctx,
            ContextGetArgs {
                key: "key".try_into().unwrap()
            }
        )
        .await
        .unwrap()
        .unwrap()
        .value,
        "new"
    );
}

#[tokio::test]
async fn concurrent_manual_ticks_launch_once_and_cancel_waits_for_local_child() {
    let (temp, h, ctx, _clock, engine) = fixture().await;
    let agent=Handler::agent_create(&h,&ctx,serde_json::from_value(serde_json::json!({
        "name":"fixture","adapter":"shell","command":"exec /bin/sleep 60","workspace_mode":"shared",
        "env":[{"name":"HOME","value":temp.path().to_str().unwrap()},{"name":"PATH","value":"/usr/bin:/bin"}]
    })).unwrap()).await.unwrap().agent;
    let t = Handler::task_create(
        &h,
        &ctx,
        serde_json::from_value(serde_json::json!({"title":"sleep fixture","assignee":agent.id}))
            .unwrap(),
    )
    .await
    .unwrap();
    let (first, second) = tokio::join!(
        Handler::dispatch_tick(&h, &ctx, DispatchTickArgs {}),
        Handler::dispatch_tick(&h, &ctx, DispatchTickArgs {})
    );
    first.unwrap();
    second.unwrap();
    let state = engine.state.read().await.unwrap();
    let running = state.running.get(&t.id).expect("one child launched");
    let pid = running.pid;
    struct Cleanup(Arc<dyn orch_core::process::ProcessManager>, u32);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            self.0.kill(self.1, orch_core::process::Signal::Kill);
        }
    }
    let _cleanup = Cleanup(engine.processes.clone(), pid);
    assert!(engine.processes.is_alive(pid));
    assert_eq!(
        Handler::task_get(
            &h,
            &ctx,
            TaskGetArgs {
                id: t.id.clone().try_into().unwrap()
            }
        )
        .await
        .unwrap()
        .attempts,
        1
    );
    let cancelled = tokio::time::timeout(
        Duration::from_secs(20),
        Handler::task_cancel(
            &h,
            &ctx,
            TaskCancelArgs {
                id: t.id.clone().try_into().unwrap(),
            },
        ),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(cancelled.status, orch_domain::TaskStatus::Cancelled);
    assert!(
        !engine.processes.is_alive(pid),
        "success means local child stopped"
    );
    let state = engine.state.read().await.unwrap();
    assert!(!state.running.contains_key(&t.id));
}

#[tokio::test]
async fn transfers_are_connection_local_bounded_and_cleaned_on_disconnect() {
    let (temp, h, ctx, _clock, _engine) = fixture().await;
    assert!(
        Handler::transfer_begin(
            &h,
            &ctx,
            TransferBeginArgs {
                purpose: TransferPurpose::Document,
                total_bytes: 0
            }
        )
        .await
        .is_err()
    );
    let token = Handler::transfer_begin(
        &h,
        &ctx,
        TransferBeginArgs {
            purpose: TransferPurpose::Document,
            total_bytes: 2,
        },
    )
    .await
    .unwrap()
    .token;
    assert!(
        Handler::transfer_begin(
            &h,
            &ctx,
            TransferBeginArgs {
                purpose: TransferPurpose::Document,
                total_bytes: 1
            }
        )
        .await
        .is_err()
    );
    let other = OpContext::new(ctx.session.clone());
    assert!(
        Handler::transfer_chunk(
            &h,
            &other,
            TransferChunkArgs {
                token: token.clone(),
                n: 0,
                data_base64: "e30=".try_into().unwrap()
            }
        )
        .await
        .is_err()
    );
    Handler::transfer_chunk(
        &h,
        &ctx,
        TransferChunkArgs {
            token: token.clone(),
            n: 0,
            data_base64: "e30=".try_into().unwrap(),
        },
    )
    .await
    .unwrap();
    assert_eq!(
        std::fs::read_dir(temp.path().join(".orchestry/run/transfers"))
            .unwrap()
            .count(),
        1
    );
    h.disconnect(ctx.session_id).await.unwrap();
    assert_eq!(
        std::fs::read_dir(temp.path().join(".orchestry/run/transfers"))
            .unwrap()
            .count(),
        0
    );
    assert!(
        Handler::transfer_finish(
            &h,
            &ctx,
            TransferFinishArgs {
                token,
                sha256: "unknown".try_into().unwrap()
            }
        )
        .await
        .is_err()
    );
}

#[tokio::test]
async fn task_filtered_follow_streams_agent_output_with_increasing_part_numbers() {
    let (_temp, h, ctx, _clock, engine) = fixture().await;
    let run = engine
        .run_service
        .create(orch_engine::run_service::CreateRunInput {
            task_id: "tsk_output".into(),
            agent_id: "agt_output".into(),
            attempt: 1,
            prompt: "fixture".into(),
            workspace_path: "/tmp".into(),
        })
        .await
        .unwrap();
    let (tx, mut rx) = tokio::sync::mpsc::channel(16);
    let stream = ctx.for_request(
        orch_proto::frame::RequestId(std::num::NonZeroU64::new(1).unwrap()),
        orch_proto::ops::OpClass::Read,
        Some(tx),
    );
    let hc = h.clone();
    let cc = stream.clone();
    let follow = tokio::spawn(async move {
        Handler::logs_follow(
            &hc,
            &cc,
            LogsFollowArgs {
                run_id: None,
                task_id: Some("tsk_output".try_into().unwrap()),
                agent_id: None,
            },
        )
        .await
    });
    let first = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            engine
                .events
                .emit(orch_domain::OrchestratorEvent::AgentOutput {
                    run_id: run.id.clone(),
                    agent_id: "agt_output".into(),
                    data: "hello".into(),
                });
            tokio::task::yield_now().await;
            if let Ok(part) = rx.try_recv() {
                break part;
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(first.n, 0);
    assert_eq!(first.kind, orch_proto::frame::PartKind::Event);
    let value: serde_json::Value = serde_json::from_str(first.data.get()).unwrap();
    assert_eq!(value["data"], "hello");
    Handler::request_cancel(
        &h,
        &ctx,
        RequestCancelArgs {
            id: stream.request_id.unwrap(),
        },
    )
    .await
    .unwrap();
    tokio::time::timeout(Duration::from_secs(2), follow)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

#[tokio::test(start_paused = true)]
async fn maintenance_timer_expires_context_while_dispatch_is_off() {
    let (_temp, h, ctx, clock, engine) = fixture().await;
    Handler::context_set(
        &h,
        &ctx,
        ContextSetArgs {
            key: "timer".try_into().unwrap(),
            value: "value".try_into().unwrap(),
            ttl_ms: Some(1),
        },
    )
    .await
    .unwrap();
    let path = engine.paths.context_path("timer").unwrap();
    assert!(path.exists());
    clock.0.fetch_add(2, Ordering::SeqCst);
    let hc = h.clone();
    let timer = tokio::spawn(async move { hc.run_maintenance().await });
    tokio::time::timeout(Duration::from_secs(2), async {
        while path.exists() {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .unwrap();
    assert!(!h.dispatch_state().dispatch_on);
    h.coordinator().stop_admission();
    tokio::time::advance(Duration::from_secs(1)).await;
    timer.await.unwrap();
}

#[tokio::test]
async fn mode_enforced_daemon_side_with_raw_client() {
    let (_temp, h, _ctx, _clock, _engine) = fixture().await;
    let ctx = OpContext::new(Session {
        caller: Caller {
            kind: CallerKind::Mcp,
            agent_id: None,
            pid: None,
        },
        mode: SessionMode::ReadOnly,
    });
    let before = h.coordinator().current_commit_seq();
    let err = h
        .execute_request(
            &ctx,
            orch_proto::ops::Request::TaskCancel(TaskCancelArgs {
                id: "tsk_missing".try_into().unwrap(),
            }),
        )
        .await
        .unwrap_err();
    assert_eq!(err.code, orch_proto::error::ErrorCode::NotPermitted);
    assert_eq!(err.execution, orch_proto::error::Execution::NotExecuted);
    assert_eq!(h.coordinator().current_commit_seq(), before);
}

#[tokio::test]
async fn disconnect_ends_batch_scheduling_and_preserves_queued_work_and_dispatch_off() {
    let (_temp, h, ctx, _clock, engine) = fixture().await;
    let first = task(&h, &ctx, "first blocked task").await;
    let request = ctx.for_request(
        orch_proto::frame::RequestId(std::num::NonZeroU64::new(1).unwrap()),
        orch_proto::ops::OpClass::Control,
        None,
    );
    let hc = h.clone();
    let cc = request.clone();
    let batch = tokio::spawn(async move {
        Handler::dispatch_until_terminal(&hc, &cc, DispatchUntilTerminalArgs {}).await
    });
    wait_batch(&h).await;
    let other = OpContext::new(ctx.session.clone());
    let later = task(&h, &other, "created during the batch").await;
    Handler::task_cancel(
        &h,
        &other,
        TaskCancelArgs {
            id: first.try_into().unwrap(),
        },
    )
    .await
    .unwrap();
    assert!(
        !batch.is_finished(),
        "new nonterminal work is included in the batch"
    );
    h.disconnect(ctx.session_id).await.unwrap();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(2), batch)
            .await
            .unwrap()
            .unwrap()
            .unwrap(),
        BatchResult::Interrupted
    );
    assert!(!h.dispatch_state().dispatch_on);
    assert!(!h.dispatch_state().batch_active);
    assert_eq!(
        Handler::task_get(
            &h,
            &other,
            TaskGetArgs {
                id: later.try_into().unwrap()
            }
        )
        .await
        .unwrap()
        .status,
        orch_domain::TaskStatus::Todo
    );
    assert!(engine.state.read().await.unwrap().running.is_empty());
}

async fn short_shell_agent(
    h: &ServiceHandler,
    ctx: &OpContext,
    temp: &tempfile::TempDir,
    approval: &str,
) -> String {
    Handler::agent_create(h,ctx,serde_json::from_value(serde_json::json!({
        "name":"short fixture","adapter":"shell","command":"exec /usr/bin/true","approval_policy":approval,"workspace_mode":"shared",
        "env":[{"name":"HOME","value":temp.path().to_str().unwrap()},{"name":"PATH","value":"/usr/bin:/bin"}]
    })).unwrap()).await.unwrap().agent.id
}

#[tokio::test]
async fn finite_run_task_finishes_at_run_completion_before_human_review() {
    let (temp, h, ctx, _clock, _engine) = fixture().await;
    let agent = short_shell_agent(&h, &ctx, &temp, "manual").await;
    let task = Handler::task_create(
        &h,
        &ctx,
        serde_json::from_value(serde_json::json!({"title":"manual review","assignee":agent}))
            .unwrap(),
    )
    .await
    .unwrap();
    let result = tokio::time::timeout(
        Duration::from_secs(30),
        Handler::dispatch_run_task(
            &h,
            &ctx,
            DispatchRunTaskArgs {
                task_id: task.id.try_into().unwrap(),
            },
        ),
    )
    .await;
    ctx.cancel_observation();
    let outcome = result
        .expect("run RPC must finish before human task approval")
        .unwrap();
    assert_eq!(outcome.exit_code, 0);
    assert_eq!(outcome.tasks[0].status, orch_domain::TaskStatus::Review);
}

#[tokio::test]
async fn finite_run_all_revisits_captured_work_when_capacity_frees() {
    let (temp, h, ctx, _clock, _engine) = fixture().await;
    let agent = short_shell_agent(&h, &ctx, &temp, "auto").await;
    Handler::agent_update_fields(
        &h,
        &ctx,
        serde_json::from_value(serde_json::json!({"id":agent,"command":"exec /bin/sleep 0.2"}))
            .unwrap(),
    )
    .await
    .unwrap();
    Handler::agent_update_fields(
        &h,
        &ctx,
        serde_json::from_value(serde_json::json!({"id":agent,"command":"exec /bin/sleep 0.2"}))
            .unwrap(),
    )
    .await
    .unwrap();
    for title in ["first", "second"] {
        Handler::task_create(
            &h,
            &ctx,
            serde_json::from_value(serde_json::json!({"title":title,"assignee":agent})).unwrap(),
        )
        .await
        .unwrap();
    }
    let outcome = tokio::time::timeout(
        Duration::from_secs(30),
        Handler::dispatch_run_all(&h, &ctx, DispatchRunAllArgs {}),
    )
    .await
    .expect("run-all must revisit work captured while capacity was busy and finish")
    .unwrap();
    assert_eq!(outcome.exit_code, 0);
    assert_eq!(outcome.tasks.len(), 2);
    assert!(
        outcome
            .tasks
            .iter()
            .all(|task| task.status == orch_domain::TaskStatus::Done)
    );
    assert!(!h.dispatch_state().dispatch_on);
}

/// Deterministic F-I1b-1: the first run occupies the agent's only slot while a
/// pass refuses the second task, then finishes before the pass's final
/// snapshot. The refusal is stale; the handler must retry, not report success.
#[tokio::test]
async fn finite_run_all_retries_work_refused_while_capacity_freed_mid_pass() {
    let (temp, h, ctx, _clock, engine) = fixture().await;
    let agent = short_shell_agent(&h, &ctx, &temp, "auto").await;
    let gate = temp.path().join("gate");
    Handler::agent_update_fields(
        &h,
        &ctx,
        serde_json::from_value(serde_json::json!({
            "id":agent,
            "command":format!("while [ ! -e '{}' ]; do sleep 0.01; done", gate.display())
        }))
        .unwrap(),
    )
    .await
    .unwrap();
    for title in ["first", "second"] {
        Handler::task_create(
            &h,
            &ctx,
            serde_json::from_value(serde_json::json!({"title":title,"assignee":agent})).unwrap(),
        )
        .await
        .unwrap();
    }
    let released = Arc::new(std::sync::atomic::AtomicBool::new(false));
    h.set_run_all_pass_hook(Arc::new({
        let (engine, gate, released) = (Arc::clone(&engine), gate.clone(), Arc::clone(&released));
        move |progressed| {
            let (engine, gate, released) =
                (Arc::clone(&engine), gate.clone(), Arc::clone(&released));
            Box::pin(async move {
                // Only the first pass that refused work while the first run
                // held the slot: let that run finish and wait for its commit.
                if progressed || released.swap(true, Ordering::SeqCst) {
                    return;
                }
                assert_eq!(engine.state.read().await.unwrap().running.len(), 1);
                std::fs::write(&gate, b"").unwrap();
                while !engine.state.read().await.unwrap().running.is_empty() {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            })
        }
    }));
    let outcome = tokio::time::timeout(
        Duration::from_secs(30),
        Handler::dispatch_run_all(&h, &ctx, DispatchRunAllArgs {}),
    )
    .await
    .expect("run-all must finish once both runs end")
    .unwrap();
    assert!(released.load(Ordering::SeqCst), "the race was not staged");
    assert_eq!(outcome.exit_code, 0);
    assert_eq!(outcome.tasks.len(), 2, "captured work was left unrun");
    assert!(
        outcome
            .tasks
            .iter()
            .all(|task| task.status == orch_domain::TaskStatus::Done)
    );
}

/// The retry must not turn finite run-all into a wait: work that stays
/// ineligible once the project is idle (here blocked behind a task that ends
/// in human review) finishes the batch without it.
#[tokio::test]
async fn finite_run_all_finishes_when_remaining_work_stays_ineligible() {
    let (temp, h, ctx, _clock, _engine) = fixture().await;
    let agent = short_shell_agent(&h, &ctx, &temp, "manual").await;
    let first = Handler::task_create(
        &h,
        &ctx,
        serde_json::from_value(serde_json::json!({"title":"first","assignee":agent})).unwrap(),
    )
    .await
    .unwrap()
    .id;
    Handler::task_create(
        &h,
        &ctx,
        serde_json::from_value(
            serde_json::json!({"title":"blocked","assignee":agent,"depends_on":[first]}),
        )
        .unwrap(),
    )
    .await
    .unwrap();
    let outcome = tokio::time::timeout(
        Duration::from_secs(30),
        Handler::dispatch_run_all(&h, &ctx, DispatchRunAllArgs {}),
    )
    .await
    .expect("run-all must finish when the rest can never launch")
    .unwrap();
    assert_eq!(outcome.exit_code, 0);
    assert_eq!(outcome.tasks.len(), 1);
    assert_eq!(outcome.tasks[0].id, first);
    assert_eq!(outcome.tasks[0].status, orch_domain::TaskStatus::Review);
}

#[tokio::test]
async fn lost_ownership_rejects_manual_dispatch_but_status_remains_live() {
    let (_temp, h, ctx, _clock, _engine) = fixture().await;
    h.set_ownership_lost().await.unwrap();
    let error = Handler::dispatch_tick(&h, &ctx, DispatchTickArgs {})
        .await
        .unwrap_err();
    assert_eq!(error.execution, orch_proto::error::Execution::NotExecuted);
    let status = Handler::daemon_status(&h, &ctx, DaemonStatusArgs {})
        .await
        .unwrap();
    assert_eq!(
        status.ownership,
        orch_proto::ops::results::OwnershipStatus::Lost
    );
    assert!(!status.dispatch.dispatch_on);
}

#[tokio::test]
async fn staged_images_keep_budget_and_are_cleaned_on_consumption_disconnect_and_startup() {
    use base64::Engine as _;
    use sha2::Digest;
    async fn image(h: &ServiceHandler, ctx: &OpContext) -> std::path::PathBuf {
        let bytes = b"\x89PNG\r\n\x1a\n";
        let token = Handler::transfer_begin(
            h,
            ctx,
            TransferBeginArgs {
                purpose: TransferPurpose::ClipboardImage,
                total_bytes: bytes.len() as u64,
            },
        )
        .await
        .unwrap()
        .token;
        Handler::transfer_chunk(
            h,
            ctx,
            TransferChunkArgs {
                token: token.clone(),
                n: 0,
                data_base64: base64::engine::general_purpose::STANDARD
                    .encode(bytes)
                    .try_into()
                    .unwrap(),
            },
        )
        .await
        .unwrap();
        Handler::transfer_finish(
            h,
            ctx,
            TransferFinishArgs {
                token: token.clone(),
                sha256: format!("{:x}", sha2::Sha256::digest(bytes))
                    .try_into()
                    .unwrap(),
            },
        )
        .await
        .unwrap();
        Handler::attachment_stage_image(
            h,
            ctx,
            AttachmentStageImageArgs {
                input_transfer: token,
                format: ImageFormat::Png,
            },
        )
        .await
        .unwrap()
        .path
        .into_path_buf()
    }
    let (temp, h, ctx, _clock, _engine) = fixture().await;
    let path = image(&h, &ctx).await;
    assert!(path.exists());
    let mut peers = Vec::new();
    for _ in 0..3 {
        let peer = OpContext::new(ctx.session.clone());
        Handler::transfer_begin(
            &h,
            &peer,
            TransferBeginArgs {
                purpose: TransferPurpose::Document,
                total_bytes: orch_proto::frame::limits::MAX_TRANSFER_BYTES as u64,
            },
        )
        .await
        .unwrap();
        peers.push(peer);
    }
    let fourth = OpContext::new(ctx.session.clone());
    assert!(
        Handler::transfer_begin(
            &h,
            &fourth,
            TransferBeginArgs {
                purpose: TransferPurpose::Document,
                total_bytes: orch_proto::frame::limits::MAX_TRANSFER_BYTES as u64
            }
        )
        .await
        .is_err(),
        "staged image still owns its budget"
    );
    h.disconnect(ctx.session_id).await.unwrap();
    assert!(!path.exists());
    Handler::transfer_begin(
        &h,
        &fourth,
        TransferBeginArgs {
            purpose: TransferPurpose::Document,
            total_bytes: orch_proto::frame::limits::MAX_TRANSFER_BYTES as u64,
        },
    )
    .await
    .unwrap();
    for peer in peers {
        h.disconnect(peer.session_id).await.unwrap();
    }
    h.disconnect(fourth.session_id).await.unwrap();
    let owner = OpContext::new(ctx.session.clone());
    let path = image(&h, &owner).await;
    Handler::task_create(
        &h,
        &owner,
        serde_json::from_value(
            serde_json::json!({"title":"copied","attachments":[{"utf8":path.to_str().unwrap()}]}),
        )
        .unwrap(),
    )
    .await
    .unwrap();
    assert!(!path.exists(), "copying into task consumes staging source");
    let stale = temp.path().join(".orchestry/run/staged/stale.png");
    std::fs::write(&stale, b"fixture").unwrap();
    h.cleanup_stale_transfers().unwrap();
    assert!(!stale.exists());
}

#[tokio::test]
async fn disconnect_waits_for_a_started_attachment_copy_under_the_state_permit() {
    use base64::Engine as _;
    use sha2::Digest;
    let (_temp, h, ctx, _clock, engine) = fixture().await;
    let bytes = b"\x89PNG\r\n\x1a\n";
    let token = Handler::transfer_begin(
        &h,
        &ctx,
        TransferBeginArgs {
            purpose: TransferPurpose::ClipboardImage,
            total_bytes: 8,
        },
    )
    .await
    .unwrap()
    .token;
    Handler::transfer_chunk(
        &h,
        &ctx,
        TransferChunkArgs {
            token: token.clone(),
            n: 0,
            data_base64: base64::engine::general_purpose::STANDARD
                .encode(bytes)
                .try_into()
                .unwrap(),
        },
    )
    .await
    .unwrap();
    Handler::transfer_finish(
        &h,
        &ctx,
        TransferFinishArgs {
            token: token.clone(),
            sha256: format!("{:x}", sha2::Sha256::digest(bytes))
                .try_into()
                .unwrap(),
        },
    )
    .await
    .unwrap();
    let path = Handler::attachment_stage_image(
        &h,
        &ctx,
        AttachmentStageImageArgs {
            input_transfer: token,
            format: ImageFormat::Png,
        },
    )
    .await
    .unwrap()
    .path
    .into_path_buf();
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let ready = entered.clone();
    let go = release.clone();
    let source = path.clone();
    let hc = h.clone();
    let write = tokio::spawn(async move {
        hc.coordinator()
            .run_op(orch_daemon::coordinator::Lane::Ordinary, move |permit| {
                Box::pin(async move {
                    permit.invalidate(orch_proto::frame::Store::Tasks);
                    ready.notify_one();
                    go.notified().await;
                    // Same outer permit and real service copying used by task.create.
                    Ok(engine
                        .task_service
                        .create(orch_domain::CreateTaskInput {
                            title: "copy survives disconnect".into(),
                            attachments: Some(vec![source.to_str().unwrap().into()]),
                            ..Default::default()
                        })
                        .await
                        .unwrap())
                })
            })
            .await
            .unwrap()
    });
    entered.notified().await;
    let hc = h.clone();
    let sid = ctx.session_id;
    let cleanup = tokio::spawn(async move { hc.disconnect(sid).await });
    tokio::task::yield_now().await;
    assert!(path.exists(), "started operation retains its source");
    assert!(!cleanup.is_finished(), "cleanup is ordered after the write");
    release.notify_one();
    let task = write.await.unwrap().value;
    cleanup.await.unwrap().unwrap();
    assert_eq!(task.attachments.unwrap().len(), 1);
    assert!(!path.exists());
}
