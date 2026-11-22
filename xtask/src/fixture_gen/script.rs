//! The seeded scenario script: a fixed sequence of operations on the real 1.0 file backend.
//!
//! Everything here is invented. Names come from the word lists below, ids and every choice from the
//! seeded generator, and time from a clock the script moves itself, so one seed and profile always
//! write the same bytes. No live project, home directory, daemon or agent CLI is read or started.

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use anyhow::{Context, Result, bail};
use orch_core::clock::{Clock, iso_from_epoch_ms};
use orch_core::storage::{
    AgentStore, ConfigStore, ContextStore, GoalStore, MessageStore, RunStore, StateStore,
    TaskStore, TeamStore,
};
use orch_domain::{
    AdapterErrorKind, Agent, AgentConfig, AgentLastError, AgentStats, AgentStatus, ApprovalPolicy,
    Goal, GoalStatus, Message, MessageChannel, MessageStatus, RetryEntry, Run, RunEvent,
    RunEventType, RunStatus, Task, TaskStatus, Team, TeamConfig, TeamMember, TeamMemberRole,
    TeamStatus, TokenUsage, TokenUsageOpts, create_token_usage, default_config, default_state,
};
use orch_infra::storage::fs_utils::atomic_write;
use orch_infra::storage::{
    FileAgentStore, FileConfigStore, FileContextStore, FileGoalStore, FileMessageStore,
    FileRunStore, FileStateStore, FileTaskStore, FileTeamStore, Paths,
};

/// 2025-01-01T00:00:00.000Z: where every scenario's clock starts.
const START_MS: i64 = 1_735_689_600_000;

/// The ids' alphabet: the 64 characters 1.0's id generator draws from (`A-Z a-z 0-9 _ -`).
const ID_ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789_-";

const ADJECTIVES: &[&str] = &[
    "quiet", "bright", "narrow", "steady", "amber", "hollow", "brisk", "plain",
];
const NOUNS: &[&str] = &[
    "ledger", "harbor", "orchard", "lantern", "meadow", "compass", "kettle", "bridge",
];
const VERBS: &[&str] = &[
    "Sort", "Tally", "Polish", "Index", "Trim", "Label", "Weigh", "Pack",
];
const AGENT_NAMES: &[&str] = &[
    "alpha", "bravo", "cedar", "delta", "ember", "fjord", "gable", "heron",
];
const ADAPTERS: &[&str] = &["claude", "codex", "opencode", "shell"];
const LABELS: &[&str] = &["synthetic", "docs", "backend", "frontend", "chore"];

/// How big a scenario is. Counts only: nothing here is a measurement of a real project.
#[derive(Debug, Clone, Copy)]
pub(super) struct Profile {
    pub(super) agents: usize,
    pub(super) goals: usize,
    /// Tasks that stay in the project, each with the runs its outcome needs.
    pub(super) tasks: usize,
    /// Extra tasks created and then deleted, so the indexes see a delete.
    pub(super) deleted_tasks: usize,
    pub(super) messages: usize,
    pub(super) context_entries: usize,
    pub(super) max_events_per_run: u64,
}

/// SplitMix64, written out here on purpose: a fixture must never change because a dependency
/// changed its generator (`rand`'s `StdRng` is documented as not stable across versions), and the
/// whole algorithm is three lines. ARCHITECTURE §4.8 exemption: no maintained crate promises a
/// frozen stream.
#[derive(Debug, Clone)]
pub(super) struct Rng(u64);

impl Rng {
    pub(super) fn new(seed: u64) -> Self {
        Rng(seed)
    }

    pub(super) fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// A value in `0..n`. The modulo bias is irrelevant: only reproducibility matters.
    fn below(&mut self, n: u64) -> u64 {
        self.next_u64() % n.max(1)
    }

    fn range(&mut self, low: u64, high_inclusive: u64) -> u64 {
        low + self.below(high_inclusive - low + 1)
    }

    fn index(&mut self, len: usize) -> usize {
        usize::try_from(self.below(len as u64)).unwrap_or(0)
    }

    fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.index(items.len())]
    }
}

/// A clock only the script moves. `orch_core::test_support::FixedClock` does the same job, but
/// `test-support` may only reach a crate through a dev edge (`cargo xtask crate-graph`).
#[derive(Debug)]
struct ScriptClock(AtomicI64);

impl ScriptClock {
    fn advance(&self, rng: &mut Rng, low_ms: u64, high_ms: u64) {
        let step = i64::try_from(rng.range(low_ms, high_ms)).unwrap_or(1);
        self.0.fetch_add(step, Ordering::SeqCst);
    }

    fn iso_after(&self, delta_ms: i64) -> String {
        iso_from_epoch_ms(self.now_ms() + delta_ms)
    }
}

impl Clock for ScriptClock {
    fn now_ms(&self) -> i64 {
        self.0.load(Ordering::SeqCst)
    }

    fn now_iso(&self) -> String {
        iso_from_epoch_ms(self.now_ms())
    }
}

/// Makes ids of the 1.0 shape (`<prefix>_` and seven characters of the alphabet), refusing two ids
/// that differ only in ASCII case: R117 forbids such a pair, and a case-folding filesystem would
/// make them one file.
struct Ids(BTreeSet<String>);

impl Ids {
    fn make(&mut self, rng: &mut Rng, prefix: &str) -> String {
        loop {
            let suffix: String = (0..7)
                .map(|_| char::from(ID_ALPHABET[rng.index(ID_ALPHABET.len())]))
                .collect();
            let id = format!("{prefix}_{suffix}");
            if self.0.insert(id.to_ascii_lowercase()) {
                return id;
            }
        }
    }
}

/// How a task ends. Each outcome fixes the runs it needs.
#[derive(Debug, Clone, Copy)]
enum Outcome {
    Todo,
    Done,
    DoneAfterFailure,
    Failed,
    Retrying,
    Review,
    Cancelled,
}

impl Outcome {
    /// Every outcome once, so even the smallest profile holds a task in each status.
    const ALL: [Outcome; 7] = [
        Outcome::Todo,
        Outcome::Done,
        Outcome::DoneAfterFailure,
        Outcome::Failed,
        Outcome::Retrying,
        Outcome::Review,
        Outcome::Cancelled,
    ];

    /// The first seven tasks take the outcomes in turn from a seeded start; later ones are drawn,
    /// weighted toward the common endings.
    fn pick(rng: &mut Rng, n: usize, start: usize) -> Outcome {
        if n < Outcome::ALL.len() {
            return Outcome::ALL[(start + n) % Outcome::ALL.len()];
        }
        match rng.below(10) {
            0 | 1 => Outcome::Todo,
            2..=4 => Outcome::Done,
            5 => Outcome::DoneAfterFailure,
            6 => Outcome::Failed,
            7 => Outcome::Retrying,
            8 => Outcome::Review,
            _ => Outcome::Cancelled,
        }
    }

    fn attempts(self) -> &'static [RunStatus] {
        match self {
            Outcome::Todo => &[],
            Outcome::Done | Outcome::Review => &[RunStatus::Succeeded],
            Outcome::DoneAfterFailure => &[RunStatus::Failed, RunStatus::Succeeded],
            Outcome::Failed => &[RunStatus::Failed, RunStatus::TimedOut],
            Outcome::Retrying => &[RunStatus::Failed],
            Outcome::Cancelled => &[RunStatus::Cancelled],
        }
    }

    fn final_status(self) -> TaskStatus {
        match self {
            Outcome::Todo => TaskStatus::Todo,
            Outcome::Done | Outcome::DoneAfterFailure => TaskStatus::Done,
            Outcome::Failed => TaskStatus::Failed,
            Outcome::Retrying => TaskStatus::Retrying,
            Outcome::Review => TaskStatus::Review,
            Outcome::Cancelled => TaskStatus::Cancelled,
        }
    }
}

/// Run the script for `seed` and `profile` into `<root>/.orchestry`.
pub(super) async fn generate(root: &Path, seed: u64, profile: &Profile) -> Result<()> {
    let paths = Paths::new(root);
    let clock = Arc::new(ScriptClock(AtomicI64::new(START_MS)));
    let mut rng = Rng::new(seed);
    let mut ids = Ids(BTreeSet::new());

    let tasks = FileTaskStore::new(paths.clone());
    let agents = FileAgentStore::new(paths.clone());
    let goals = FileGoalStore::new(paths.clone());
    let teams = FileTeamStore::new(paths.clone());
    let runs = FileRunStore::new(paths.clone());
    let messages = FileMessageStore::new(paths.clone(), clock.clone());
    let context = FileContextStore::new(paths.clone(), clock.clone());
    let config = FileConfigStore::new(paths.clone());
    let state_store = FileStateStore::new(paths.clone());

    // Config: the defaults with a synthetic name, plus one key 1.0 does not know. The importer's
    // dry run lists unknown keys, so the corpus holds one.
    let mut project = default_config();
    project.project.name = format!("synthetic-seed-{seed}");
    project.project.description =
        Some("Generated by cargo xtask fixture-gen; invented content.".into());
    config.write(&project).await?;
    config
        .set(
            "x_synthetic.note",
            serde_json::json!("an unknown key, kept as written"),
        )
        .await?;

    let mut roster: Vec<Agent> = Vec::new();
    for n in 0..profile.agents {
        let adapter = ADAPTERS[n % ADAPTERS.len()];
        let name = AGENT_NAMES[n % AGENT_NAMES.len()];
        let agent = Agent {
            id: ids.make(&mut rng, "agt"),
            name: if n < AGENT_NAMES.len() {
                name.to_owned()
            } else {
                format!("{name}-{n}")
            },
            adapter: adapter.to_owned(),
            role: Some(format!(
                "Synthetic {adapter} worker for generated fixtures."
            )),
            config: AgentConfig {
                command: Some(format!("synthetic-{adapter}")),
                model: Some("synthetic-model".into()),
                approval_policy: Some(*rng.pick(&ApprovalPolicy::ALL)),
                max_turns: Some(u32::try_from(rng.range(10, 60)).unwrap_or(10)),
                timeout_ms: Some(rng.range(60, 3_600) * 1_000),
                ..AgentConfig::default()
            },
            status: AgentStatus::Idle,
            current_task: None,
            autonomous: None,
            stats: AgentStats::default(),
            last_error: None,
            capabilities: Vec::new(),
            remote: None,
        };
        clock.advance(&mut rng, 100, 2_000);
        agents.save(&agent).await?;
        roster.push(agent);
    }
    if roster.is_empty() {
        bail!("a profile needs at least one agent");
    }

    let mut goal_ids: Vec<String> = Vec::new();
    for n in 0..profile.goals {
        let goal = Goal {
            id: ids.make(&mut rng, "goal"),
            title: format!("Synthetic goal {}", n + 1),
            description: format!("Invented goal number {} for generated fixtures.", n + 1),
            status: [GoalStatus::Active, GoalStatus::Paused, GoalStatus::Achieved][n % 3],
            assignee: Some(roster[n % roster.len()].id.clone()),
            created_at: clock.now_iso(),
            updated_at: (n % 2 == 0).then(|| clock.iso_after(5_000)),
        };
        clock.advance(&mut rng, 1_000, 30_000);
        goals.save(&goal).await?;
        goal_ids.push(goal.id);
    }

    // Tasks: created in order, then taken through their runs, so every save moves the task to the
    // end of the index exactly as the 1.0 engine's saves did.
    let mut task_ids: Vec<String> = Vec::new();
    let mut retries: Vec<RetryEntry> = Vec::new();
    let mut totals = Totals::default();
    let first_outcome = rng.index(Outcome::ALL.len());
    for n in 0..profile.tasks + profile.deleted_tasks {
        let doomed = n >= profile.tasks;
        let outcome = if doomed {
            Outcome::Todo
        } else {
            Outcome::pick(&mut rng, n, first_outcome)
        };
        let id = ids.make(&mut rng, "tsk");
        let depends_on: Vec<String> = if task_ids.is_empty() || rng.below(3) != 0 {
            Vec::new()
        } else {
            (0..rng.range(1, 2))
                .map(|_| rng.pick(&task_ids).clone())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect()
        };
        let mut task = Task {
            id: id.clone(),
            title: format!(
                "{} the {} {}",
                rng.pick(VERBS),
                rng.pick(ADJECTIVES),
                rng.pick(NOUNS)
            ),
            description: format!(
                "Invented work item {}; nothing here describes a real project.",
                n + 1
            ),
            status: TaskStatus::Todo,
            priority: i64::try_from(rng.range(1, 4)).unwrap_or(3),
            assignee: None,
            labels: vec![(*rng.pick(LABELS)).to_owned()],
            depends_on,
            created_at: clock.now_iso(),
            updated_at: clock.now_iso(),
            attempts: 0,
            max_attempts: if matches!(outcome, Outcome::Failed) {
                2
            } else {
                3
            },
            workspace_mode: None,
            workspace: None,
            proof: None,
            review_criteria: None,
            review_results: None,
            scope: None,
            feedback: None,
            goal_id: (!goal_ids.is_empty() && rng.below(3) != 0)
                .then(|| rng.pick(&goal_ids).clone()),
            attachments: None,
        };
        if n == 0 {
            // One task carries a file, which lives beside the entity files, not inside them.
            let dir = paths.task_attachments_dir(&id)?;
            atomic_write(&dir.join("note.txt"), "A synthetic attachment.\n").await?;
            task.attachments = Some(vec!["note.txt".into()]);
        }
        clock.advance(&mut rng, 500, 5_000);
        tasks.save(&task).await?;

        let attempts = outcome.attempts();
        for (index, status) in attempts.iter().enumerate() {
            let agent = rng.index(roster.len());
            task.status = TaskStatus::InProgress;
            task.assignee = Some(roster[agent].id.clone());
            task.attempts += 1;
            task.updated_at = clock.now_iso();
            tasks.save(&task).await?;

            let run = run_attempt(
                &mut rng,
                &clock,
                &runs,
                &mut ids,
                &task,
                &roster[agent],
                *status,
                profile,
            )
            .await?;
            totals.add(&run, &mut roster[agent].stats);
            if let Some(error) = &run.error {
                roster[agent].last_error = Some(AgentLastError {
                    message: error.clone(),
                    kind: run
                        .error_kind
                        .map_or("unknown", |kind| kind.as_str())
                        .to_owned(),
                    timestamp: clock.now_iso(),
                });
            }

            let last = index + 1 == attempts.len();
            task.status = if last {
                outcome.final_status()
            } else {
                TaskStatus::Retrying
            };
            task.updated_at = clock.now_iso();
            tasks.save(&task).await?;
        }
        if matches!(outcome, Outcome::Retrying) {
            retries.push(RetryEntry {
                task_id: id.clone(),
                attempt: task.attempts + 1,
                due_at: clock.iso_after(10_000),
                error: "synthetic failure".into(),
            });
        }
        if matches!(outcome, Outcome::Done | Outcome::DoneAfterFailure) {
            totals.completed += 1;
        }
        if matches!(outcome, Outcome::Failed) {
            totals.failed_tasks += 1;
        }
        if doomed {
            tasks.delete(&id).await?;
        } else {
            task_ids.push(id);
        }
    }

    // Agents are saved again, in an order the seed picks: the file index then holds them in
    // last-write order, which is what 1.0's tie-breaking between equal agents reads.
    let mut order: Vec<usize> = (0..roster.len()).collect();
    for i in (1..order.len()).rev() {
        order.swap(i, rng.index(i + 1));
    }
    for i in order {
        clock.advance(&mut rng, 100, 1_000);
        agents.save(&roster[i]).await?;
    }

    let team = Team {
        id: ids.make(&mut rng, "team"),
        name: "synthetic-team".into(),
        description: Some("Invented team.".into()),
        status: TeamStatus::Active,
        members: roster
            .iter()
            .enumerate()
            .map(|(n, agent)| TeamMember {
                agent_id: agent.id.clone(),
                role: if n == 0 {
                    TeamMemberRole::Lead
                } else {
                    TeamMemberRole::Member
                },
                joined_at: clock.now_iso(),
            })
            .collect(),
        task_pool: task_ids.iter().take(3).cloned().collect(),
        lead_agent_id: roster[0].id.clone(),
        created_at: clock.now_iso(),
        updated_at: clock.now_iso(),
        config: TeamConfig::default(),
    };
    teams.save(&team).await?;

    let mut message_ids: Vec<String> = Vec::new();
    for n in 0..profile.messages {
        let channel = [
            MessageChannel::Direct,
            MessageChannel::Broadcast,
            MessageChannel::Lead,
        ][n % 3];
        let message = Message {
            id: ids.make(&mut rng, "msg"),
            channel,
            from_agent_id: roster[rng.index(roster.len())].id.clone(),
            to_agent_id: match channel {
                MessageChannel::Broadcast => None,
                _ => Some(roster[rng.index(roster.len())].id.clone()),
            },
            subject: format!("Synthetic subject {}", n + 1),
            body: format!("Invented message body {}.", n + 1),
            created_at: clock.now_iso(),
            expires_at: (n % 2 == 1).then(|| clock.iso_after(86_400_000)),
            status: MessageStatus::Pending,
            delivered_at: None,
            team_id: matches!(channel, MessageChannel::Lead).then(|| team.id.clone()),
            reply_to: (n % 4 == 3).then(|| message_ids[n - 1].clone()),
        };
        clock.advance(&mut rng, 1_000, 60_000);
        messages.save(&message).await?;
        if n % 2 == 0 {
            messages.mark_delivered(&message.id).await?;
        }
        message_ids.push(message.id);
    }

    for n in 0..profile.context_entries {
        clock.advance(&mut rng, 1_000, 60_000);
        let ttl = (n % 3 == 2).then_some(86_400_000);
        context
            .set(
                &format!("synthetic.{}", NOUNS[n % NOUNS.len()]),
                &format!("Invented value {}.", n + 1),
                ttl,
            )
            .await?;
    }

    atomic_write(
        &paths.default_template_path(),
        "# Synthetic template\n\nInvented prompt text for generated fixtures.\n",
    )
    .await?;

    // Nothing is running when the script ends: a running entry names a process id, and the import
    // refuses a source whose process still exists (R119). That case belongs to the hostile corpus.
    let mut state = default_state();
    state.onboarding_completed = Some(true);
    state.retry_queue = retries;
    state.stats.total_runs = totals.runs;
    state.stats.total_tasks_completed = totals.completed;
    state.stats.total_tasks_failed = totals.failed_tasks;
    state.stats.total_tokens = totals.tokens;
    state.stats.total_runtime_ms = totals.runtime_ms;
    state_store
        .write(&state)
        .await
        .context("write state.json")?;
    Ok(())
}

#[derive(Debug, Default)]
struct Totals {
    runs: u64,
    completed: u64,
    failed_tasks: u64,
    runtime_ms: u64,
    tokens: TokenUsage,
}

impl Totals {
    fn add(&mut self, run: &Run, agent: &mut AgentStats) {
        let tokens = run.tokens.unwrap_or_default();
        let runtime = 60_000;
        self.runs += 1;
        self.runtime_ms += runtime;
        self.tokens.input += tokens.input;
        self.tokens.output += tokens.output;
        self.tokens.reasoning += tokens.reasoning;
        self.tokens.total += tokens.total;
        self.tokens.cache_read += tokens.cache_read;
        self.tokens.cache_write += tokens.cache_write;
        agent.total_runs += 1;
        agent.total_runtime_ms += runtime;
        agent.tokens_used = Some(agent.tokens_used.unwrap_or(0) + tokens.total);
        match run.status {
            RunStatus::Succeeded => agent.tasks_completed += 1,
            RunStatus::Failed | RunStatus::TimedOut => agent.tasks_failed += 1,
            _ => {}
        }
    }
}

/// One attempt: the run is saved at each step of its life and its event file grows line by line.
#[allow(
    clippy::too_many_arguments,
    reason = "a flat scripted step; a parameter struct would only rename the arguments"
)]
async fn run_attempt(
    rng: &mut Rng,
    clock: &ScriptClock,
    runs: &FileRunStore,
    ids: &mut Ids,
    task: &Task,
    agent: &Agent,
    status: RunStatus,
    profile: &Profile,
) -> Result<Run> {
    let mut run = Run {
        id: ids.make(rng, "run"),
        task_id: task.id.clone(),
        agent_id: agent.id.clone(),
        attempt: task.attempts,
        status: RunStatus::Preparing,
        started_at: clock.now_iso(),
        finished_at: None,
        workspace_path: format!("workspaces/{}", task.id),
        prompt: format!(
            "Synthetic prompt for {}, attempt {}.",
            task.id, task.attempts
        ),
        pid: None,
        error: None,
        error_kind: None,
        tokens: None,
    };
    runs.save(&run).await?;
    clock.advance(rng, 100, 1_500);
    run.status = RunStatus::Running;
    run.pid = Some(u32::try_from(rng.range(2_000, 60_000)).unwrap_or(2_000));
    runs.save(&run).await?;

    let lines = rng.range(2, profile.max_events_per_run.max(2));
    for n in 0..lines {
        clock.advance(rng, 100, 4_000);
        let (event_type, data) = match rng.below(4) {
            0 => (RunEventType::ToolCall, format!("synthetic tool call {n}")),
            1 => (RunEventType::FileChanged, format!("synthetic/file-{n}.txt")),
            2 => (
                RunEventType::CommandRun,
                format!("synthetic-command --step {n}"),
            ),
            _ => (
                RunEventType::AgentOutput,
                format!("synthetic output line {n} for {}", run.id),
            ),
        };
        runs.append_event(
            &run.id,
            &RunEvent {
                timestamp: clock.now_iso(),
                event_type,
                data: serde_json::Value::String(data),
            },
        )
        .await?;
    }
    let (closing, data) = if status == RunStatus::Succeeded {
        (RunEventType::Done, "synthetic run finished")
    } else {
        (RunEventType::Error, "synthetic run ended without success")
    };
    clock.advance(rng, 100, 1_000);
    runs.append_event(
        &run.id,
        &RunEvent {
            timestamp: clock.now_iso(),
            event_type: closing,
            data: serde_json::Value::String(data.into()),
        },
    )
    .await?;
    runs.close_run_events(&run.id);

    run.status = status;
    run.finished_at = Some(clock.now_iso());
    run.tokens = Some(create_token_usage(
        rng.range(500, 20_000),
        rng.range(100, 5_000),
        Some(TokenUsageOpts {
            reasoning: Some(rng.range(0, 2_000)),
            cache_read: Some(rng.range(0, 10_000)),
            cache_write: Some(rng.range(0, 1_000)),
        }),
    ));
    match status {
        RunStatus::Failed => {
            run.error = Some("synthetic process crash".into());
            run.error_kind = Some(AdapterErrorKind::ProcessCrash);
        }
        RunStatus::TimedOut => {
            run.error = Some("synthetic timeout".into());
            run.error_kind = Some(AdapterErrorKind::Timeout);
        }
        _ => {}
    }
    runs.save(&run).await?;
    Ok(run)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_generator_follows_the_published_splitmix64_stream() {
        // The reference outputs of SplitMix64 for seed 0 (Vigna, public domain implementation).
        let mut rng = Rng::new(0);
        assert_eq!(rng.next_u64(), 0xE220_A839_7B1D_CDAF);
        assert_eq!(rng.next_u64(), 0x6E78_9E6A_A1B9_65F4);
        assert_eq!(rng.next_u64(), 0x06C4_5D18_8009_454F);
    }

    #[test]
    fn ids_have_the_1_0_shape_and_never_collide_under_case_folding() {
        let mut rng = Rng::new(7);
        let mut ids = Ids(BTreeSet::new());
        let made: Vec<String> = (0..2_000).map(|_| ids.make(&mut rng, "tsk")).collect();
        let folded: BTreeSet<String> = made.iter().map(|id| id.to_ascii_lowercase()).collect();
        assert_eq!(folded.len(), made.len());
        for id in &made {
            assert!(orch_core::ids::is_well_formed_id(id, "tsk"), "{id}");
        }
    }
}
