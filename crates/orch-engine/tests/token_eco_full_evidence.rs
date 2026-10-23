//! T1 / F4 — D3-8 through the real dispatch path: huge task, own role,
//! skills, inbox, review feedback, goal text/progress and the terminal retry
//! error are delivered in full in eco — there is no hidden total prompt cap and
//! none of the "own evidence" classes is truncated — and every unit of that
//! evidence that normal mode delivers is also delivered by eco.

mod token_eco_support;

use orch_domain::{CreateGoalInput, CreateMessageInput, MessageChannel, PromptMode, RunStatus};
use serde_json::json;
use token_eco_support::{Rig, RigSpec, claude_agent, config_with, failed_run, task};

fn blob(label: &str, len: usize) -> String {
    let mut s = format!("{label}-START ");
    while s.len() + label.len() + 6 < len {
        s.push('x');
    }
    s.push_str(&format!(" {label}-END"));
    s
}

const LABELS: [&str; 7] = [
    "TASKDESC",
    "OWNROLE",
    "SKILLTEXT",
    "INBOXBODY",
    "FEEDBACK",
    "GOALDESC",
    "RETRYERR",
];

async fn rig_for(mode: &str) -> Rig {
    let mut agent = claude_agent("agt_a", "Alpha");
    agent.role = Some(blob("OWNROLE", 60_000));
    agent.config.skills = Some(vec!["big-skill".into()]);
    let rig = Rig::new(RigSpec {
        raw_config: config_with(Some(mode), json!({})),
        agents: vec![agent],
        tasks: vec![],
        skills: vec![("big-skill".into(), blob("SKILLTEXT", 80_000))],
        config_store: true,
    })
    .await;
    let goal = rig
        .engine
        .goal_service
        .create(CreateGoalInput {
            title: "G".into(),
            description: Some(blob("GOALDESC", 40_000)),
            ..Default::default()
        })
        .await
        .unwrap();
    let mut t = task("tsk_big", Some("agt_a"));
    t.description = blob("TASKDESC", 300_000);
    t.feedback = Some(blob("FEEDBACK", 50_000));
    t.goal_id = Some(goal.id.clone());
    t.attempts = 1;
    rig.reseed_task(t).await;
    rig.engine
        .runs
        .save(&failed_run(
            "run_prev",
            "tsk_big",
            "agt_a",
            "2025-10-13T16:20:00.000Z",
            &blob("RETRYERR", 30_000),
        ))
        .await
        .unwrap();
    rig.engine
        .message_service
        .send(CreateMessageInput {
            channel: MessageChannel::Direct,
            from_agent_id: "cli".into(),
            to_agent_id: Some("agt_a".into()),
            subject: "s".into(),
            // 4,000 UTF-16 units is the message service's own existing storage cap.
            body: blob("INBOXBODY", 3_900),
            ..Default::default()
        })
        .await
        .unwrap();
    rig
}

#[tokio::test(flavor = "multi_thread")]
async fn eco_delivers_every_own_evidence_class_in_full_with_no_total_cap() {
    let rig = rig_for("eco").await;
    rig.dispatch().await;
    let call = rig.adapter.for_task("tsk_big");
    assert!(call.is_eco());
    let text = call.all_text();
    for label in LABELS {
        assert!(
            text.contains(&format!("{label}-START")) && text.contains(&format!("{label}-END")),
            "{label}: both ends must be present (no truncation)"
        );
    }
    // The three largest classes are present as complete contiguous blobs.
    for (label, len) in [
        ("TASKDESC", 300_000),
        ("OWNROLE", 60_000),
        ("SKILLTEXT", 80_000),
    ] {
        assert!(text.contains(&blob(label, len)), "{label}: complete blob");
    }
    assert!(
        text.len() > 560_000,
        "no hidden total cap: {} bytes",
        text.len()
    );
    assert_eq!(
        rig.runs()
            .await
            .iter()
            .filter(|r| r.status == RunStatus::Failed)
            .count(),
        1
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn normal_and_eco_deliver_the_same_set_of_evidence_labels() {
    let mut sets = Vec::new();
    for mode in ["normal", "eco"] {
        let rig = rig_for(mode).await;
        rig.dispatch().await;
        let call = rig.adapter.for_task("tsk_big");
        assert_eq!(call.is_eco(), mode == "eco");
        let text = call.all_text();
        let present: Vec<&str> = LABELS
            .into_iter()
            .filter(|l| text.contains(&format!("{l}-END")))
            .collect();
        sets.push(present);
        assert_eq!(
            rig.agent("agt_a").await.config.prompt_mode,
            None::<PromptMode>,
            "{mode}"
        );
    }
    assert_eq!(sets[0], LABELS.to_vec(), "normal is the oracle");
    assert_eq!(
        sets[0], sets[1],
        "eco loses no evidence class that normal carries"
    );
}
