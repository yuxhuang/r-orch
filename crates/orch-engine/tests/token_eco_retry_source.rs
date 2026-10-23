//! T1 / F4 — the retry evidence reference (D3-10) and its consumers.
//!
//! A retried task's eco prompt must (1) name the *same* failed run whose
//! error/output it quotes, (2) keep full output when no valid source reference
//! exists, and (3) degrade visibly, never by guessing, when events are
//! missing or corrupt.

mod token_eco_support;

use orch_domain::{RunEvent, RunEventType, TaskStatus};
use serde_json::{Value, json};
use token_eco_support::{Rig, RigSpec, claude_agent, config_with, failed_run, task};

fn ev(ts: &str, kind: RunEventType, data: Value) -> RunEvent {
    RunEvent {
        timestamp: ts.into(),
        event_type: kind,
        data,
    }
}

async fn seed_run_with_output(rig: &Rig, id: &str, finished: &str, error: &str, chunks: &[String]) {
    rig.engine
        .runs
        .save(&failed_run(id, "tsk_r", "agt_a", finished, error))
        .await
        .unwrap();
    for (i, chunk) in chunks.iter().enumerate() {
        rig.engine
            .runs
            .append_event(
                id,
                &ev(
                    &format!("2025-10-13T16:00:{i:02}.000Z"),
                    RunEventType::AgentOutput,
                    Value::String(chunk.clone()),
                ),
            )
            .await
            .unwrap();
    }
}

fn retry_task() -> orch_domain::Task {
    let mut t = task("tsk_r", Some("agt_a"));
    t.attempts = 1;
    t.status = TaskStatus::Todo;
    t
}

async fn retry_rig(mode: &str) -> Rig {
    Rig::new(RigSpec {
        raw_config: config_with(Some(mode), json!({})),
        agents: vec![claude_agent("agt_a", "Alpha")],
        tasks: vec![retry_task()],
        skills: vec![],
        config_store: true,
    })
    .await
}

/// 12 chunks of 1,000 bytes; the decisive clue sits in chunk 5 — beyond
/// character 80, and neither in the 2,048-byte head nor in the retained tail.
fn chunks_with_mid_clue() -> Vec<String> {
    (0..12)
        .map(|i| {
            let mut s = format!("chunk{i:02}-");
            if i == 5 {
                s.push_str("DECISIVE-CLUE-IN-THE-MIDDLE-");
            }
            while s.len() < 999 {
                s.push('.');
            }
            s.push('|');
            s
        })
        .collect()
}

fn fenced_output(prompt: &str) -> &str {
    let start = prompt.find("```\n").expect("opening fence") + 4;
    let end = prompt[start..].find("\n```").expect("closing fence") + start;
    &prompt[start..end]
}

#[tokio::test(flavor = "multi_thread")]
async fn eco_names_the_newest_failed_run_not_an_older_failure() {
    let rig = retry_rig("eco").await;
    seed_run_with_output(
        &rig,
        "run_old",
        "2025-10-13T16:10:00.000Z",
        "OLD-ERROR",
        &["old output".into()],
    )
    .await;
    seed_run_with_output(
        &rig,
        "run_new",
        "2025-10-13T16:20:00.000Z",
        "NEW-ERROR",
        &chunks_with_mid_clue(),
    )
    .await;

    rig.dispatch().await;

    let call = rig.adapter.for_task("tsk_r");
    assert!(call.is_eco());
    assert!(
        call.prompt.contains("NEW-ERROR"),
        "the full terminal error is kept"
    );
    assert!(!call.prompt.contains("OLD-ERROR"));
    assert!(
        call.prompt.contains("orch --json logs run_new"),
        "the recovery command names the run the preview came from"
    );
    assert!(
        !call.prompt.contains("run_old"),
        "an older failure is never referenced"
    );
    assert!(call.prompt.contains("preview of failed run `run_new`"));
}

#[tokio::test(flavor = "multi_thread")]
async fn an_oversized_preview_keeps_head_and_tail_counts_the_middle_and_omits_the_clue() {
    let chunks = chunks_with_mid_clue();
    let full = chunks.join("\n");

    // Normal mode is the oracle: it carries the whole output, clue included.
    let normal = retry_rig("normal").await;
    seed_run_with_output(&normal, "run_new", "2025-10-13T16:20:00.000Z", "E", &chunks).await;
    normal.dispatch().await;
    let normal_text = normal.adapter.for_task("tsk_r").all_text();
    assert!(
        normal_text.contains("DECISIVE-CLUE-IN-THE-MIDDLE"),
        "normal keeps everything"
    );
    assert!(normal_text.contains(&full));

    let eco = retry_rig("eco").await;
    seed_run_with_output(&eco, "run_new", "2025-10-13T16:20:00.000Z", "E", &chunks).await;
    eco.dispatch().await;
    let prompt = eco.adapter.for_task("tsk_r").prompt;
    let preview = fenced_output(&prompt);

    assert!(preview.len() <= 8_192, "bounded: {} bytes", preview.len());
    assert!(
        preview.starts_with(&full[..2_048]),
        "up to 2,048 head bytes"
    );
    assert!(
        full.ends_with(
            preview
                .rsplit_once("more bytes]")
                .expect("marker")
                .1
                .trim_start_matches('\n')
        ),
        "the remaining budget is spent on the tail"
    );
    assert!(!preview.contains("DECISIVE-CLUE-IN-THE-MIDDLE"));
    let marker_at = preview.find("[truncated, ").expect("marker");
    let n: usize = preview[marker_at + 12..]
        .split(' ')
        .next()
        .unwrap()
        .parse()
        .expect("byte count");
    let (head, rest) = preview.split_at(marker_at);
    let tail = rest
        .split_once("more bytes]")
        .unwrap()
        .1
        .trim_start_matches('\n');
    assert_eq!(
        n,
        full.len() - head.trim_end_matches('\n').len() - tail.len(),
        "N counts exactly the omitted source bytes"
    );
    assert!(
        prompt.contains("Required before any retry diagnosis, plan or side effect"),
        "fetch is mandatory before acting"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn output_that_fits_is_shown_whole_with_no_marker_and_no_fetch_requirement() {
    let rig = retry_rig("eco").await;
    seed_run_with_output(
        &rig,
        "run_new",
        "2025-10-13T16:20:00.000Z",
        "E",
        &["short output".into()],
    )
    .await;
    rig.dispatch().await;
    let prompt = rig.adapter.for_task("tsk_r").prompt;
    assert!(prompt.contains("short output"));
    assert!(!prompt.contains("[truncated"));
    assert!(!prompt.contains("Required before any retry diagnosis"));
}

#[tokio::test(flavor = "multi_thread")]
async fn the_service_reports_the_exact_run_its_error_and_output_came_from() {
    let rig = retry_rig("eco").await;
    seed_run_with_output(
        &rig,
        "run_a",
        "2025-10-13T16:10:00.000Z",
        "ERR-A",
        &["from a".into()],
    )
    .await;
    seed_run_with_output(
        &rig,
        "run_b",
        "2025-10-13T16:20:00.000Z",
        "ERR-B",
        &["from b".into()],
    )
    .await;
    // A newer run that is not failed must never be selected.
    let mut ok = failed_run("run_c", "tsk_r", "agt_a", "2025-10-13T16:30:00.000Z", "x");
    ok.status = orch_domain::RunStatus::Succeeded;
    rig.engine.runs.save(&ok).await.unwrap();

    let ctx = rig
        .engine
        .run_service
        .get_last_failed_run_context("tsk_r")
        .await
        .unwrap()
        .expect("a failed run exists");

    assert_eq!(ctx.run_id, "run_b");
    assert_eq!(ctx.error, "ERR-B");
    assert_eq!(ctx.output, "from b");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_run_with_no_finish_time_loses_to_one_with_a_finish_time_and_is_still_named_consistently()
 {
    let rig = retry_rig("eco").await;
    seed_run_with_output(
        &rig,
        "run_fin",
        "2025-10-13T16:10:00.000Z",
        "ERR-FIN",
        &["fin".into()],
    )
    .await;
    let mut unfinished = failed_run("run_nofin", "tsk_r", "agt_a", "", "ERR-NOFIN");
    unfinished.finished_at = None;
    rig.engine.runs.save(&unfinished).await.unwrap();
    rig.engine
        .runs
        .append_event(
            "run_nofin",
            &ev(
                "2025-10-13T16:40:00.000Z",
                RunEventType::AgentOutput,
                json!("nofin out"),
            ),
        )
        .await
        .unwrap();

    let ctx = rig
        .engine
        .run_service
        .get_last_failed_run_context("tsk_r")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (ctx.run_id.as_str(), ctx.error.as_str(), ctx.output.as_str()),
        ("run_fin", "ERR-FIN", "fin")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn extraction_matches_the_documented_rules_for_string_and_structured_events() {
    // Documented in the prompt: string data as-is, any other data as compact
    // JSON, agent_output and error events only, joined with newlines.
    let rig = retry_rig("normal").await;
    rig.engine
        .runs
        .save(&failed_run(
            "run_x",
            "tsk_r",
            "agt_a",
            "2025-10-13T16:20:00.000Z",
            "E",
        ))
        .await
        .unwrap();
    for (i, (kind, data)) in [
        (RunEventType::AgentOutput, json!("plain text")),
        (RunEventType::ToolCall, json!({"ignored": true})),
        (
            RunEventType::AgentOutput,
            json!({"text": "structured", "n": 2}),
        ),
        (RunEventType::Error, json!("boom")),
        (RunEventType::Error, json!({"message": "object error"})),
    ]
    .into_iter()
    .enumerate()
    {
        rig.engine
            .runs
            .append_event(
                "run_x",
                &ev(&format!("2025-10-13T16:00:0{i}.000Z"), kind, data),
            )
            .await
            .unwrap();
    }

    let ctx = rig
        .engine
        .run_service
        .get_last_failed_run_context("tsk_r")
        .await
        .unwrap()
        .unwrap();

    assert_eq!(
        ctx.output,
        "plain text\n{\"text\":\"structured\",\"n\":2}\nboom\n{\"message\":\"object error\"}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn corrupt_event_storage_never_blocks_the_retry_and_never_invents_output() {
    let rig = retry_rig("eco").await;
    rig.engine
        .runs
        .save(&failed_run(
            "run_bad",
            "tsk_r",
            "agt_a",
            "2025-10-13T16:20:00.000Z",
            "THE-ERROR",
        ))
        .await
        .unwrap();
    let events_path = rig.paths.run_events_path("run_bad").unwrap();
    assert!(!events_path.exists(), "nothing recorded yet");
    std::fs::write(&events_path, "{not json\n\u{0}garbage\n").unwrap();

    rig.dispatch().await;

    let prompt = rig.adapter.for_task("tsk_r").prompt;
    assert!(events_path.exists());
    assert!(
        prompt.contains("THE-ERROR"),
        "the terminal error is still complete"
    );
    assert!(
        !prompt.contains("garbage"),
        "corrupt bytes are never presented as output"
    );
    assert!(!prompt.contains("{not json"));
}

#[tokio::test(flavor = "multi_thread")]
async fn the_prompt_tells_the_agent_to_stop_when_the_evidence_cannot_be_recovered() {
    let rig = retry_rig("eco").await;
    seed_run_with_output(
        &rig,
        "run_new",
        "2025-10-13T16:20:00.000Z",
        "E",
        &chunks_with_mid_clue(),
    )
    .await;
    rig.dispatch().await;
    let prompt = rig.adapter.for_task("tsk_r").prompt;
    assert!(prompt.contains("report the retry evidence as unavailable"));
    assert!(prompt.contains("do not continue from a guess"));
    assert!(prompt.contains("last 50 stored events"));
}

#[tokio::test(flavor = "multi_thread")]
async fn first_attempts_carry_no_retry_section_in_either_mode() {
    for mode in ["normal", "eco"] {
        let rig = Rig::new(RigSpec {
            raw_config: config_with(Some(mode), json!({})),
            agents: vec![claude_agent("agt_a", "Alpha")],
            tasks: vec![task("tsk_1", Some("agt_a"))],
            skills: vec![],
            config_store: true,
        })
        .await;
        rig.dispatch().await;
        assert!(
            !rig.adapter
                .for_task("tsk_1")
                .prompt
                .contains("Previous attempt failed"),
            "{mode}"
        );
    }
}
