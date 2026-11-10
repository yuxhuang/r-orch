//! Each collector input releases its own operation before awaiting another.
use super::harness::{Harness, NOW_ISO};
use futures::StreamExt;
use orch_core::adapter::{AgentEvent, AgentEventKind};
use orch_core::remote::RemoteRunEvent;
use orch_core::test_support::make_run;
use orch_domain::OrchestratorEvent;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

async fn drive_stream(remote: bool) {
    let temp = tempfile::tempdir().unwrap();
    let h = Harness::builder()
        .project_root(temp.path().to_owned())
        .build();
    h.runs.seed(orch_domain::Run {
        id: "run_stream".into(),
        ..make_run()
    });
    let boundary = h.bus.operation_boundary();
    let (observed, mut observations) = tokio::sync::mpsc::unbounded_channel();
    let callback_boundary = boundary.clone();
    let _listener = h.bus.on_any(move |event| {
        if let OrchestratorEvent::AgentOutput { data, .. } = event {
            let _ = observed.send((data.clone(), callback_boundary.current_commit_seq()));
        }
    });
    let (input, receiver) = tokio::sync::mpsc::channel(1);
    let stream = futures::stream::unfold(receiver, |mut receiver| async move {
        receiver.recv().await.map(|event| (event, receiver))
    })
    .boxed();
    let orchestrator = h.orchestrator.clone();
    let collector = tokio::spawn(async move {
        if remote {
            orchestrator
                .collect_remote_events(
                    stream.map(RemoteRunEvent::Event).boxed(),
                    "run_stream",
                    "tsk_stream",
                    "agent@worker",
                    false,
                    CancellationToken::new(),
                )
                .await
        } else {
            orchestrator
                .collect_events(stream.map(Ok).boxed(), "run_stream", "tsk_stream", "agent")
                .await
        }
    });
    let mut before = boundary.current_commit_seq();
    for k in 0..3 {
        tokio::time::timeout(
            Duration::from_secs(2),
            input.send(AgentEvent::new(
                AgentEventKind::Output,
                NOW_ISO,
                serde_json::json!({"text": format!("output_{k}")}),
            )),
        )
        .await
        .expect("collector stopped reading")
        .unwrap();
        let (data, seq) = tokio::time::timeout(Duration::from_secs(2), observations.recv())
            .await
            .expect("collector held the operation across the next input wait")
            .unwrap();
        assert!(data.contains(&format!("output_{k}")));
        assert!(
            seq > before,
            "input must commit before next input is supplied"
        );
        assert!(!collector.is_finished(), "stream remains open");
        assert!(observations.try_recv().is_err(), "one output per input");
        before = seq;
    }
    collector.abort();
    assert!(
        tokio::time::timeout(Duration::from_secs(2), collector)
            .await
            .expect("collector abort")
            .unwrap_err()
            .is_cancelled()
    );
}

#[tokio::test]
async fn local_collector_releases_each_input_before_awaiting_the_next() {
    drive_stream(false).await;
}

#[tokio::test]
async fn remote_collector_releases_each_input_before_awaiting_the_next() {
    drive_stream(true).await;
}
