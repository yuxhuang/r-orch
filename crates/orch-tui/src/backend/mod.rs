//! [`TuiBackend`] — the narrow facade the dashboard needs from the engine.
//!
//! The dashboard needs about 45 operations and streams from the CLI; this
//! trait groups and types them. `orch-tui` deliberately does **not**
//! depend on `orch-engine`: the CLI implements this trait over the container,
//! and the tests use [`FakeBackend`]. See `crates/orch-tui/BACKEND.md`.
//!
//! Two deliberate shape choices, both documented in
//! BACKEND.md:
//!
//! 1. **Synchronous.** Every method returns a value, not a future. A method is
//!    a *request*: implementations must not block on long-running work
//!    (dispatching an agent run, for example) — they kick it off and let the
//!    outcome arrive through [`TuiBackend::poll_events`].
//! 2. **Polled streams.** Events, history, the model catalog and the update
//!    check are `poll_*` methods drained once per frame, which is what makes
//!    the TUI tests synchronous (ARCHITECTURE §8.4).

pub mod copy;
pub mod fake;

pub use fake::{Call, FakeBackend, make_agent, make_goal, make_task, make_team};

use std::collections::HashMap;

use indexmap::IndexMap;
use orch_domain::{
    Agent, CreateGoalInput, CreateTeamInput, Goal, GoalStatus, NotificationPreferences,
    OrchestratorEvent, OrchestratorState, PromptMode, Run, Task, Team, TuiPaletteName,
};

use crate::format::parse_timestamp_ms;

/// Every fallible facade call reports a human message — the TUI only ever
/// renders `Failed: {err}`.
pub type BackendResult<T> = Result<T, String>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CopyTarget {
    Auto,
    Terminal,
    Native,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CopyTicket(pub u64);
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CopyOutcome {
    NativeWritten,
    TerminalRequested,
    Failed(String),
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CopyResult {
    pub ticket: CopyTicket,
    pub outcome: CopyOutcome,
    pub guidance: Option<String>,
}

/// When a goal or task started, and how long it has run (D3). Derived, never
/// stored: the domain types are unchanged.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EntityTiming {
    /// UTC epoch milliseconds. `None`: never started, or the timestamp is
    /// missing or malformed.
    pub started_at_ms: Option<i64>,
    /// Nonnegative milliseconds, start to the sample time while live and
    /// start to the last update once terminal. `None` when there is no
    /// start, or a terminal entity has no valid last update — never a
    /// fabricated zero.
    pub elapsed_ms: Option<i64>,
}

/// Every goal's and task's [`EntityTiming`] from one refresh, keyed by id.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TimingSnapshot {
    pub tasks: HashMap<String, EntityTiming>,
    pub goals: HashMap<String, EntityTiming>,
    /// The `now_ms` the live spans were computed at.
    pub sampled_at_ms: i64,
}

/// Derive task and goal timings from every retained run record plus the
/// same refresh's tasks, goals and orchestrator state.
///
/// * A task starts at its earliest valid run `started_at`, including the
///   live `orch_state.running` entry, which covers a first run whose record
///   is not written yet. A run's start is recorded while it is still
///   Preparing, so this is not the process launch. No run: no start.
/// * A goal starts at its `created_at`.
/// * Elapsed runs to `now_ms` while the entity is live and to its
///   `updated_at` once it is terminal (Done/Failed/Cancelled,
///   Achieved/Abandoned), clamped at zero. A later metadata edit moves
///   `updated_at`, and so the span; that is the disclosed `Elapsed*`
///   contract, not a completion duration.
///
/// One pass over `runs`; nothing here reads a file.
pub fn derive_timings(
    runs: &[Run],
    tasks: &[Task],
    goals: &[Goal],
    orch_state: Option<&OrchestratorState>,
    now_ms: i64,
) -> TimingSnapshot {
    fn note<'a>(earliest: &mut HashMap<&'a str, i64>, task_id: &'a str, started_at: &str) {
        if let Some(ms) = parse_timestamp_ms(started_at) {
            earliest
                .entry(task_id)
                .and_modify(|current| *current = (*current).min(ms))
                .or_insert(ms);
        }
    }
    let mut earliest: HashMap<&str, i64> = HashMap::new();
    for run in runs {
        note(&mut earliest, &run.task_id, &run.started_at);
    }
    if let Some(state) = orch_state {
        for (task_id, entry) in &state.running {
            note(&mut earliest, task_id, &entry.started_at);
        }
    }

    let span = |start: Option<i64>, terminal: bool, updated_at: Option<&str>| {
        let start = start?;
        let end = if terminal {
            parse_timestamp_ms(updated_at?)?
        } else {
            now_ms
        };
        Some(end.saturating_sub(start).max(0))
    };

    let tasks = tasks
        .iter()
        .map(|task| {
            let started_at_ms = earliest.get(task.id.as_str()).copied();
            let terminal = crate::state::is_terminal_task(task.status);
            let timing = EntityTiming {
                started_at_ms,
                elapsed_ms: span(started_at_ms, terminal, Some(&task.updated_at)),
            };
            (task.id.clone(), timing)
        })
        .collect();
    let goals = goals
        .iter()
        .map(|goal| {
            let started_at_ms = parse_timestamp_ms(&goal.created_at);
            let terminal = crate::state::is_terminal_goal(goal.status);
            let timing = EntityTiming {
                started_at_ms,
                elapsed_ms: span(started_at_ms, terminal, goal.updated_at.as_deref()),
            };
            (goal.id.clone(), timing)
        })
        .collect();
    TimingSnapshot {
        tasks,
        goals,
        sampled_at_ms: now_ms,
    }
}

/// Options for creating a task.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CreateTaskOpts {
    pub priority: Option<i64>,
    pub description: Option<String>,
    pub attachments: Option<Vec<String>>,
}

/// Fields to change on a task.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TaskFields {
    pub title: Option<String>,
    pub description: Option<String>,
    pub priority: Option<i64>,
    pub attachments: Option<Vec<String>>,
}

/// Options for adding an agent.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AgentOpts {
    pub command: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub role: Option<String>,
    pub approval_policy: Option<String>,
    pub skills: Option<Vec<String>>,
    /// `normal` or `eco`; `None` or `""` stores no override (the agent
    /// inherits the project mode).
    pub prompt_mode: Option<String>,
    /// Explicit one-line role hint; `None` or `""` stores none.
    pub role_summary: Option<String>,
}

/// Fields to change on an agent.
///
/// `Some(String::new())` means "clear this field" — the edit-agent mapper
/// relies on it for `command`, `effort`, `prompt_mode` (clearing the override
/// means inheriting the project mode) and `role_summary`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AgentFields {
    pub name: Option<String>,
    pub adapter: Option<String>,
    pub role: Option<String>,
    pub command: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub prompt_mode: Option<String>,
    pub role_summary: Option<String>,
}

/// Fields to change on a goal.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GoalFields {
    pub title: Option<String>,
    pub description: Option<String>,
    pub assignee: Option<String>,
}

/// Who a [`TuiBackend::send_message`] goes to.
///
/// Only the two channels a dashboard can address: a named agent, or everyone.
/// The service's third channel (`lead`) needs a team id the TUI has no verb
/// for, so it is deliberately absent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MessageTarget {
    /// One agent, by **id** — not by name. Resolving a typed name to an id is
    /// the TUI's job, because only it knows what the user typed.
    Agent(String),
    /// Every enabled agent except the sender.
    Broadcast,
}

/// One model choice offered by a wizard `model` step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelOption {
    pub value: String,
    pub label: String,
    pub hint: String,
}

/// `ModelCatalog` — discovered models per adapter. Insertion-ordered because
/// the wizard renders them in arrival order.
pub type ModelCatalog = IndexMap<String, Vec<ModelOption>>;

/// One historical run event, as delivered by the history stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryEntry {
    pub timestamp: String,
    pub run_id: String,
    pub agent_id: String,
    pub task_id: String,
    /// The `RunEventType` string (`output`, `error`, `done`, `tool_call`,
    /// `file_changed`, …).
    pub event_type: String,
    pub data: String,
}

/// Result of a clipboard read (text, an image, or nothing).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum ClipboardPaste {
    /// Clipboard held text, not an image.
    Text,
    /// Clipboard was empty or unreadable.
    #[default]
    Empty,
    /// An image was written to `path`; `bytes` drives the `({n}KB)` message.
    Image { path: String, bytes: usize },
}

/// Immutable launch context — the non-callback launch properties.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchContext {
    pub project_name: String,
    pub version: String,
    pub default_adapter: String,
    pub initial_palette: TuiPaletteName,
    pub initial_activity_filter: String,
    pub initial_notifications: NotificationPreferences,
    pub initial_max_concurrent: u32,
    /// Watch mode started successfully on launch.
    pub initial_watch_active: bool,
    /// Watch mode was refused and a disk observer took over.
    pub observer_mode: bool,
    /// Watch-mode failure message, suppressed in observer mode.
    pub watch_error: Option<String>,
    /// The project prompt mode (`defaults.agent.prompt_mode`) at launch.
    pub initial_prompt_mode: PromptMode,
    /// Read-only configuration diagnostics found at launch (effort requests
    /// dispatch will ignore or cannot confirm, eco/custom-template
    /// conflicts), one line per affected agent.
    pub startup_warnings: Vec<String>,
}

impl Default for LaunchContext {
    fn default() -> Self {
        LaunchContext {
            project_name: "project".into(),
            version: "0.0.0".into(),
            default_adapter: "claude".into(),
            initial_palette: TuiPaletteName::Amber,
            initial_activity_filter: "all".into(),
            initial_notifications: NotificationPreferences {
                toast: true,
                bell: false,
            },
            initial_max_concurrent: 2,
            initial_watch_active: false,
            observer_mode: false,
            watch_error: None,
            initial_prompt_mode: PromptMode::Normal,
            startup_warnings: Vec::new(),
        }
    }
}

/// The operations and streams the dashboard needs. Every method has a default
/// "not available" implementation where the operation is optional, so a partial
/// backend (and the capability flags below) behave like a missing callback.
pub trait TuiBackend {
    fn copy_text(&mut self, text: &str) -> BackendResult<CopyTicket> {
        self.copy_text_via(text, CopyTarget::Auto)
    }
    /// I0 safe default; B1 supplies asynchronous native/terminal transport.
    fn copy_text_via(&mut self, _text: &str, _target: CopyTarget) -> BackendResult<CopyTicket> {
        Err("Copy is not available".into())
    }
    fn poll_copy_results(&mut self) -> Vec<CopyResult> {
        Vec::new()
    }

    // ── Launch context ─────────────────────────────────────────────────────

    fn context(&self) -> LaunchContext;

    fn peer_status(&mut self) -> BackendResult<String> {
        Ok("listen off · controller none".into())
    }

    // ── Capability flags ───────────────────────────────────────────────────
    //
    // Several bindings are keyed off whether an operation is available.
    // These flags are that predicate.

    fn can_create_task(&self) -> bool {
        true
    }
    fn can_add_agent(&self) -> bool {
        true
    }
    fn can_create_goal(&self) -> bool {
        true
    }
    fn can_force_stop_agent(&self) -> bool {
        true
    }
    fn can_paste_image(&self) -> bool {
        true
    }
    fn can_send_message(&self) -> bool {
        true
    }

    // ── Refresh ────────────────────────────────────────────────────────────

    fn refresh_tasks(&mut self) -> BackendResult<Vec<Task>>;
    fn refresh_agents(&mut self) -> BackendResult<Vec<Agent>>;
    fn refresh_goals(&mut self) -> BackendResult<Vec<Goal>>;
    fn refresh_state(&mut self) -> BackendResult<OrchestratorState>;
    fn list_teams(&mut self) -> BackendResult<Vec<Team>>;
    /// Started/Elapsed for the entities this refresh just returned (D3).
    /// `orch_state` is this refresh's state, or `None` when it could not be
    /// read — never an older snapshot. Called once per refresh, never per
    /// frame. A backend without run history reports nothing.
    fn refresh_timings(
        &mut self,
        _tasks: &[Task],
        _goals: &[Goal],
        _orch_state: Option<&OrchestratorState>,
        now_ms: i64,
    ) -> BackendResult<TimingSnapshot> {
        Ok(TimingSnapshot {
            sampled_at_ms: now_ms,
            ..TimingSnapshot::default()
        })
    }

    // ── Tasks ──────────────────────────────────────────────────────────────

    fn create_task(&mut self, title: &str, opts: CreateTaskOpts) -> BackendResult<Task>;
    fn update_task(&mut self, id: &str, fields: TaskFields) -> BackendResult<Task>;
    fn delete_task(&mut self, id: &str) -> BackendResult<()>;
    fn run_task(&mut self, id: &str) -> BackendResult<()>;
    fn run_all(&mut self) -> BackendResult<()>;
    fn cancel_task(&mut self, id: &str) -> BackendResult<()>;
    fn retry_task(&mut self, id: &str) -> BackendResult<()>;
    fn assign_task(&mut self, task_id: &str, agent_id: &str) -> BackendResult<()>;
    fn approve_task(&mut self, id: &str) -> BackendResult<()>;
    fn reject_task(&mut self, id: &str, feedback: Option<&str>) -> BackendResult<()>;

    // ── Agents ─────────────────────────────────────────────────────────────

    fn add_agent(&mut self, name: &str, adapter: &str, opts: AgentOpts) -> BackendResult<Agent>;
    fn update_agent(&mut self, id: &str, fields: AgentFields) -> BackendResult<Agent>;
    fn delete_agent(&mut self, id: &str) -> BackendResult<()>;
    fn force_stop_agent(&mut self, id: &str) -> BackendResult<()>;
    fn toggle_autonomous(&mut self, id: &str, enabled: bool) -> BackendResult<Agent>;
    fn disable_agent(&mut self, id: &str) -> BackendResult<()>;
    fn enable_agent(&mut self, id: &str) -> BackendResult<()>;

    // ── Teams ──────────────────────────────────────────────────────────────

    fn create_team(&mut self, input: CreateTeamInput) -> BackendResult<Team>;
    fn join_team(&mut self, team_id: &str, agent_id: &str) -> BackendResult<Team>;
    fn leave_team(&mut self, team_id: &str, agent_id: &str) -> BackendResult<Team>;
    fn disband_team(&mut self, team_id: &str) -> BackendResult<()>;
    fn set_team_lead(&mut self, team_id: &str, agent_id: &str) -> BackendResult<Team>;

    // ── Goals ──────────────────────────────────────────────────────────────

    fn create_goal(&mut self, input: CreateGoalInput) -> BackendResult<Goal>;
    fn update_goal(&mut self, id: &str, fields: GoalFields) -> BackendResult<Goal>;
    fn update_goal_status(
        &mut self,
        id: &str,
        status: GoalStatus,
        force: bool,
    ) -> BackendResult<Goal>;
    fn delete_goal(&mut self, id: &str) -> BackendResult<()>;
    fn goal_progress(&mut self, id: &str) -> BackendResult<String>;

    // ── Messaging ──────────────────────────────────────────────────────────

    /// `orch msg send` / `orch msg broadcast` with the CLI's own defaults:
    /// sender `cli`, empty subject, default TTL. Returns the id of every
    /// record created, in fan-out order.
    ///
    /// Ids rather than the `Message` records because that is all the dashboard
    /// consumes — the result line names the id or the count, and the
    /// disabled-recipient advisory comes from `state.agents`, not from the
    /// record. Required rather than defaulted, so "messaging missing" is
    /// unrepresentable.
    ///
    /// A direct send always yields exactly one id; a broadcast yields one per
    /// recipient, and **zero** when it reached nobody — which is a success,
    /// not a failure, so the caller reports the count rather than treating an
    /// empty vector as an error.
    fn send_message(&mut self, target: MessageTarget, body: &str) -> BackendResult<Vec<String>>;

    // ── Watch mode ─────────────────────────────────────────────────────────

    fn start_watch(&mut self) -> BackendResult<()>;
    fn stop_watch(&mut self) -> BackendResult<()>;

    // ── Settings persistence ───────────────────────────────────────────────

    fn save_palette(&mut self, palette: TuiPaletteName) -> BackendResult<()>;
    fn save_activity_filter(&mut self, preset: &str) -> BackendResult<()>;
    fn save_notifications(&mut self, notifications: NotificationPreferences) -> BackendResult<()>;
    fn save_max_concurrent(&mut self, value: u32) -> BackendResult<()>;
    fn complete_onboarding(&mut self) -> BackendResult<()>;

    // ── Project prompt mode ────────────────────────────────────────────────

    /// The project prompt mode as stored now. Read on every refresh, so a
    /// change made by `orch config set` in another process reaches the header.
    fn project_prompt_mode(&mut self) -> BackendResult<PromptMode> {
        Ok(self.context().initial_prompt_mode)
    }

    /// Start a write of the project prompt mode: `Some(mode)` sets it, `None`
    /// toggles normal ↔ eco from the value stored at write time.
    ///
    /// The write is asynchronous so the dashboard can show it as pending and
    /// refuse a second one meanwhile; its outcome arrives through
    /// [`TuiBackend::poll_prompt_mode`]. It re-reads the raw file under the
    /// project definition lock, changes one key, validates every agent that
    /// would resolve to eco against custom templates, and writes nothing on
    /// failure. It never touches agent overrides, models, effort, running
    /// agents or the scheduler; the next dispatch reads the new value.
    fn request_prompt_mode(&mut self, _target: Option<PromptMode>) -> BackendResult<()> {
        Err("Switching the prompt mode is not available".into())
    }

    /// The outcome of the last [`TuiBackend::request_prompt_mode`], once:
    /// the mode now stored, or the reason nothing was written.
    fn poll_prompt_mode(&mut self) -> Option<BackendResult<PromptMode>> {
        None
    }

    // ── Polled streams ─────────────────────────────────────────────────────

    /// Orchestrator events since the last poll, oldest first.
    fn poll_events(&mut self) -> Vec<OrchestratorEvent>;

    /// Run-history batches since the last poll.
    fn poll_history(&mut self) -> Vec<Vec<HistoryEntry>> {
        Vec::new()
    }

    /// A newly discovered model catalog, if one arrived since the last poll.
    fn poll_model_catalog(&mut self) -> Option<ModelCatalog> {
        None
    }

    /// A newer available version, if the update check has produced one.
    fn poll_update(&mut self) -> Option<String> {
        None
    }

    /// Install the given version in the background. Fire-and-forget; the header
    /// chip flips when [`TuiBackend::installed_version`] reports it.
    fn background_install(&mut self, _version: &str) -> BackendResult<()> {
        Ok(())
    }

    /// Set once a background install has completed.
    fn installed_version(&self) -> Option<String> {
        None
    }

    // ── Side channels ──────────────────────────────────────────────────────

    /// Read the clipboard, writing any image into a temp file.
    fn paste_image(&mut self) -> BackendResult<ClipboardPaste> {
        Ok(ClipboardPaste::Empty)
    }

    /// Ring the terminal bell (`process.stdout.write('\x07')`) — a direct
    /// terminal write outside the draw loop.
    fn bell(&mut self) {}
}
