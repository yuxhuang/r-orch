//! The activity-message model: types, icons, colours, the event formatter and
//! the run-id maps (`src/tui/App.tsx`).
//!
//! The caps here are OOM defences, not cosmetics (PARITY.md §12.6).

pub mod format;
pub mod retention;

use indexmap::{IndexMap, IndexSet};
use orch_domain::OrchestratorEvent;

use crate::theme::Token;
use format::{
    Classification, SummaryColor, SummaryKind, classify_agent_summary, format_agent_error,
    format_agent_output,
};

/// Activity-feed ring buffer size.
pub const MAX_MESSAGES: usize = 500;
/// `runId → agentId` / `runId → taskId` map cap (insertion-ordered LRU).
pub const MAX_RUN_MAP_SIZE: usize = 500;
/// Toast queue cap.
pub const MAX_TOASTS: usize = 5;
/// Toasts actually rendered — the **first** two of the queue.
pub const TOAST_MAX_VISIBLE: usize = 2;
/// Collapsed task-list size.
pub const TASK_LIST_LIMIT: usize = 10;
/// Soft-delete undo window.
pub const UNDO_TIMEOUT_MS: i64 = 5_000;
/// Message batch flush interval.
pub const MESSAGE_BATCH_MS: i64 = 80;
/// Event-driven refresh debounce.
pub const REFRESH_DEBOUNCE_MS: i64 = 150;
/// Animation tick.
pub const ANIM_TICK_MS: i64 = 120;
/// Relative-clock tick for the logs view and activity feed.
pub const NOW_TICK_MS: i64 = 5_000;
/// Same-agent gap that starts a new visual session in the logs view.
pub const LOG_SESSION_GAP_MS: i64 = 30_000;
/// Periodic disk poll in observer mode.
pub const OBSERVER_POLL_MS: i64 = 3_000;
/// Periodic disk poll in watch mode.
pub const WATCH_POLL_MS: i64 = 5_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum MsgType {
    System,
    Lifecycle,
    Output,
    Tool,
    Result,
    Error,
    File,
    Info,
}

impl MsgType {
    /// Declaration order, which is also the type-picker row order.
    pub const ALL: [MsgType; 8] = [
        MsgType::System,
        MsgType::Lifecycle,
        MsgType::Output,
        MsgType::Tool,
        MsgType::Result,
        MsgType::Error,
        MsgType::File,
        MsgType::Info,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            MsgType::System => "system",
            MsgType::Lifecycle => "lifecycle",
            MsgType::Output => "output",
            MsgType::Tool => "tool",
            MsgType::Result => "result",
            MsgType::Error => "error",
            MsgType::File => "file",
            MsgType::Info => "info",
        }
    }

    /// `MSG_ICONS`.
    pub fn icon(self) -> &'static str {
        match self {
            MsgType::System => "◆",
            MsgType::Lifecycle => "▶",
            MsgType::Output => "│",
            MsgType::Tool => "⚙",
            MsgType::Result => "←",
            MsgType::Error => "✕",
            MsgType::File => "✎",
            MsgType::Info => "│",
        }
    }

    /// Picker row label.
    pub fn label(self) -> &'static str {
        match self {
            MsgType::System => "System",
            MsgType::Lifecycle => "Lifecycle",
            MsgType::Output => "Output",
            MsgType::Tool => "Tool calls",
            MsgType::Result => "Results",
            MsgType::Error => "Errors",
            MsgType::File => "Files",
            MsgType::Info => "Info",
        }
    }

    /// Picker cursor-row colour.
    pub fn picker_token(self) -> Token {
        match self {
            MsgType::System => Token::Purple,
            MsgType::Lifecycle => Token::Cyan,
            MsgType::Output => Token::White,
            MsgType::Tool => Token::Blue,
            MsgType::Result => Token::Green,
            MsgType::Error => Token::Red,
            MsgType::File => Token::Purple,
            MsgType::Info => Token::Silver,
        }
    }

    /// Picker row icon (differs from the feed icon for `output`/`result`).
    pub fn picker_icon(self) -> &'static str {
        match self {
            MsgType::System => "◆",
            MsgType::Lifecycle => "▶",
            MsgType::Output => "│",
            MsgType::Tool => "⚙",
            MsgType::Result => "←",
            MsgType::Error => "✕",
            MsgType::File => "✎",
            MsgType::Info => "│",
        }
    }

    /// `getMsgTextColor` — `None` means "use the message's own colour".
    pub fn text_token(self) -> Option<Token> {
        match self {
            MsgType::Output => Some(Token::White),
            MsgType::Tool | MsgType::Result | MsgType::Lifecycle | MsgType::System => {
                Some(Token::Dim)
            }
            MsgType::File => Some(Token::Gray),
            MsgType::Error => Some(Token::Red),
            MsgType::Info => None,
        }
    }

    /// `getMsgBg` — only errors get a row background; the feed is deliberately
    /// calm.
    pub fn background_token(self) -> Option<Token> {
        match self {
            MsgType::Error => Some(Token::ErrorBg),
            _ => None,
        }
    }
}

impl std::str::FromStr for MsgType {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        MsgType::ALL.into_iter().find(|t| t.as_str() == s).ok_or(())
    }
}

/// One activity-feed / logs-view entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    pub id: u64,
    pub ts_ms: i64,
    pub text: String,
    pub color: Token,
    pub msg_type: MsgType,
    pub agent_id: Option<String>,
    pub task_id: Option<String>,
    pub detail: Option<String>,
}

impl Message {
    pub fn new(id: u64, ts_ms: i64, text: &str, color: Token, msg_type: MsgType) -> Self {
        Message {
            id,
            ts_ms,
            text: text.to_string(),
            color,
            msg_type,
            agent_id: None,
            task_id: None,
            detail: None,
        }
    }

    pub fn with_agent(mut self, agent_id: Option<String>) -> Self {
        self.agent_id = agent_id;
        self
    }

    pub fn with_task(mut self, task_id: Option<String>) -> Self {
        self.task_id = task_id;
        self
    }

    pub fn with_detail(mut self, detail: Option<String>) -> Self {
        // `addMessage` caps the stored detail.
        self.detail = detail.map(|d| {
            if d.chars().count() > format::MAX_DETAIL_LEN {
                let head: String = d.chars().take(format::MAX_DETAIL_LEN).collect();
                format!("{head}…[truncated]")
            } else {
                d
            }
        });
        self
    }
}

/// Insertion-ordered `runId → value` map with an oldest-first prune, matching
/// the TS `Map` used as a poor man's LRU.
#[derive(Debug, Clone, Default)]
pub struct RunMap {
    inner: IndexMap<String, String>,
}

impl RunMap {
    pub fn insert(&mut self, run_id: &str, value: &str) {
        self.inner.insert(run_id.to_string(), value.to_string());
        while self.inner.len() > MAX_RUN_MAP_SIZE {
            self.inner.shift_remove_index(0);
        }
    }

    pub fn get(&self, run_id: &str) -> Option<&String> {
        self.inner.get(run_id)
    }

    pub fn len(&self) -> usize {
        self.inner.len()
    }

    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }
}

/// The two run maps, pruned together.
#[derive(Debug, Clone, Default)]
pub struct RunMaps {
    pub agent_of_run: RunMap,
    pub task_of_run: RunMap,
}

impl RunMaps {
    /// Recorded on `agent:started`, **before** formatting, so output events for
    /// the same run resolve correctly.
    pub fn record(&mut self, run_id: &str, agent_id: &str, task_id: &str) {
        self.agent_of_run.insert(run_id, agent_id);
        self.task_of_run.insert(run_id, task_id);
    }
}

/// A message about to be appended, before an id and timestamp are assigned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingMessage {
    pub text: String,
    pub color: Token,
    pub msg_type: MsgType,
    pub agent_id: Option<String>,
    pub task_id: Option<String>,
    pub detail: Option<String>,
    /// The message's own instant, when it has one: history entries carry the
    /// time the event was recorded (`ts: new Date(entry.timestamp).getTime()`
    /// in `historyEntryToMsg`), so a backfilled run keeps reading `2h` rather
    /// than `now`. `None` means "stamp it with the flush time", which is what
    /// every live event wants.
    pub ts_ms: Option<i64>,
}

fn pending(text: String, color: Token, msg_type: MsgType) -> PendingMessage {
    PendingMessage {
        text,
        color,
        msg_type,
        agent_id: None,
        task_id: None,
        detail: None,
        ts_ms: None,
    }
}

fn classification_tokens(classification: Classification) -> (MsgType, Token) {
    let msg_type = match classification.kind {
        SummaryKind::Lifecycle => MsgType::Lifecycle,
        SummaryKind::Tool => MsgType::Tool,
        SummaryKind::Result => MsgType::Result,
        SummaryKind::Info => MsgType::Info,
        SummaryKind::Output => MsgType::Output,
    };
    let color = match classification.color {
        SummaryColor::Dim => Token::Dim,
        SummaryColor::Yellow => Token::Yellow,
        SummaryColor::Silver => Token::Silver,
    };
    (msg_type, color)
}

/// `formatEvent` — the event → message mapping (docs/parity/tui.md §7).
///
/// Returns `None` for events that produce no feed entry (an idle
/// `orchestrator:tick`, an `agent:output` whose summary is skipped, and every
/// event type the feed ignores).
pub fn format_event(event: &OrchestratorEvent, runs: &RunMaps) -> Option<PendingMessage> {
    let task_of = |run_id: &str| runs.task_of_run.get(run_id).cloned();
    let agent_of = |run_id: &str| runs.agent_of_run.get(run_id).cloned();

    match event {
        OrchestratorEvent::AgentStarted {
            agent_id,
            task_id,
            run_id: _,
        } => {
            let mut msg = pending("Started task".into(), Token::Green, MsgType::Lifecycle);
            msg.agent_id = Some(agent_id.clone());
            msg.task_id = Some(task_id.clone());
            Some(msg)
        }
        OrchestratorEvent::AgentOutput {
            run_id,
            agent_id,
            data,
        } => {
            let formatted = format_agent_output(data);
            let summary = formatted.summary?;
            let (msg_type, color) = classification_tokens(classify_agent_summary(&summary));
            let mut msg = pending(summary, color, msg_type);
            msg.agent_id = Some(agent_id.clone());
            msg.task_id = task_of(run_id);
            msg.detail = Some(formatted.detail);
            Some(msg)
        }
        OrchestratorEvent::AgentFileChanged {
            run_id,
            agent_id,
            path,
        } => {
            let mut msg = pending(path.clone(), Token::Purple, MsgType::File);
            msg.agent_id = Some(agent_id.clone());
            msg.task_id = task_of(run_id);
            Some(msg)
        }
        OrchestratorEvent::AgentCompleted {
            run_id,
            agent_id,
            success,
        } => {
            let (text, color) = if *success {
                ("Completed successfully", Token::Green)
            } else {
                ("Failed", Token::Red)
            };
            let mut msg = pending(text.into(), color, MsgType::Lifecycle);
            msg.agent_id = Some(agent_id.clone());
            msg.task_id = task_of(run_id);
            Some(msg)
        }
        OrchestratorEvent::AgentError {
            run_id,
            agent_id,
            error,
            ..
        } => {
            let formatted = format_agent_error(error);
            let mut msg = pending(
                formatted.summary.unwrap_or_default(),
                Token::Red,
                MsgType::Error,
            );
            msg.agent_id = Some(agent_id.clone());
            msg.task_id = task_of(run_id);
            msg.detail = Some(formatted.detail);
            Some(msg)
        }
        OrchestratorEvent::TaskStatusChanged {
            task_id, from, to, ..
        } => {
            let mut msg = pending(
                format!("{} → {}", from.as_str(), to.as_str()),
                Token::Cyan,
                MsgType::System,
            );
            msg.task_id = Some(task_id.clone());
            Some(msg)
        }
        OrchestratorEvent::TaskAssigned { task_id, agent_id } => {
            let mut msg = pending(
                format!("Assigned → {agent_id}"),
                Token::Cyan,
                MsgType::System,
            );
            msg.task_id = Some(task_id.clone());
            Some(msg)
        }
        OrchestratorEvent::TaskCreated { task } => {
            let mut msg = pending(
                format!("Created: {}", task.title),
                Token::Amber,
                MsgType::System,
            );
            msg.task_id = Some(task.id.clone());
            Some(msg)
        }
        OrchestratorEvent::RunRetry {
            run_id,
            attempt,
            delay_ms,
        } => {
            // `Math.round(delay_ms / 1000)`.
            let seconds = ((*delay_ms as f64) / 1000.0).round() as i64;
            let mut msg = pending(
                format!("Retry #{attempt} ({seconds}s delay)"),
                Token::Yellow,
                MsgType::Lifecycle,
            );
            msg.agent_id = agent_of(run_id);
            msg.task_id = task_of(run_id);
            Some(msg)
        }
        OrchestratorEvent::OrchestratorTick { running, queued } => {
            if *running > 0 || *queued > 0 {
                Some(pending(
                    format!("{running} running · {queued} queued"),
                    Token::Ghost,
                    MsgType::System,
                ))
            } else {
                None
            }
        }
        OrchestratorEvent::OrchestratorStallDetected { run_id } => {
            let mut msg = pending("Stall detected".into(), Token::Yellow, MsgType::Error);
            msg.agent_id = agent_of(run_id);
            msg.task_id = task_of(run_id);
            Some(msg)
        }
        OrchestratorEvent::TaskCascadeFailed {
            task_id,
            failed_dependency_id,
            reason,
        } => {
            let mut msg = pending(
                format!("Cascade failed (dep: {failed_dependency_id})"),
                Token::Red,
                MsgType::Error,
            );
            msg.task_id = Some(task_id.clone());
            msg.detail = Some(reason.clone());
            Some(msg)
        }
        _ => None,
    }
}

// ── Activity / log type filters ─────────────────────────────────────────────

/// `ACTIVITY_PRESETS`, in cycle order.
pub const ACTIVITY_PRESETS: [&str; 5] = ["all", "text", "tools", "errors", "events"];

pub fn preset_types(preset: &str) -> IndexSet<MsgType> {
    match preset {
        "text" => [MsgType::Output].into_iter().collect(),
        "tools" => [MsgType::Tool, MsgType::Result, MsgType::File]
            .into_iter()
            .collect(),
        "errors" => [MsgType::Error].into_iter().collect(),
        "events" => [MsgType::Lifecycle, MsgType::System].into_iter().collect(),
        // "all" and anything unknown.
        _ => MsgType::ALL.into_iter().collect(),
    }
}

/// `cyclePreset` — find the current preset by **exact set equality** and
/// advance with wrap; an unrecognised set starts from `all`.
pub fn cycle_preset(current: &IndexSet<MsgType>) -> &'static str {
    let index = ACTIVITY_PRESETS
        .iter()
        .position(|p| sets_equal(&preset_types(p), current));
    match index {
        Some(i) => ACTIVITY_PRESETS[(i + 1) % ACTIVITY_PRESETS.len()],
        None => ACTIVITY_PRESETS[0],
    }
}

/// The label for a filter set, derived the same way, defaulting to `all`.
pub fn preset_label(current: &IndexSet<MsgType>) -> &'static str {
    ACTIVITY_PRESETS
        .iter()
        .copied()
        .find(|p| sets_equal(&preset_types(p), current))
        .unwrap_or("all")
}

fn sets_equal(a: &IndexSet<MsgType>, b: &IndexSet<MsgType>) -> bool {
    a.len() == b.len() && a.iter().all(|t| b.contains(t))
}

/// The logs status-bar `f:` label (docs/parity/tui.md §1.4).
pub fn type_filter_label(filter: &IndexSet<MsgType>) -> String {
    if filter.len() >= 8 {
        return "all".into();
    }
    if filter.len() == 1 && filter.contains(&MsgType::Output) {
        return "text".into();
    }
    if filter.len() == 1 && filter.contains(&MsgType::Error) {
        return "errors".into();
    }
    if filter.contains(&MsgType::Tool) && !filter.contains(&MsgType::Output) {
        return "tools".into();
    }
    if filter.contains(&MsgType::Lifecycle) && !filter.contains(&MsgType::Output) {
        return "events".into();
    }
    format!("{} types", filter.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use orch_domain::{TaskStatus, task::Task};

    fn task(id: &str, title: &str) -> Task {
        crate::backend::fake::make_task(id, title, TaskStatus::Todo)
    }

    #[test]
    fn msg_type_icons_and_text_colours() {
        assert_eq!(MsgType::System.icon(), "◆");
        assert_eq!(MsgType::Lifecycle.icon(), "▶");
        assert_eq!(MsgType::Output.icon(), "│");
        assert_eq!(MsgType::Tool.icon(), "⚙");
        assert_eq!(MsgType::Result.icon(), "←");
        assert_eq!(MsgType::Error.icon(), "✕");
        assert_eq!(MsgType::File.icon(), "✎");
        assert_eq!(MsgType::Info.icon(), "│");

        assert_eq!(MsgType::Output.text_token(), Some(Token::White));
        assert_eq!(MsgType::Tool.text_token(), Some(Token::Dim));
        assert_eq!(MsgType::File.text_token(), Some(Token::Gray));
        assert_eq!(MsgType::Error.text_token(), Some(Token::Red));
        assert_eq!(MsgType::Info.text_token(), None);

        assert_eq!(MsgType::Error.background_token(), Some(Token::ErrorBg));
        assert_eq!(MsgType::Output.background_token(), None);
    }

    #[test]
    fn run_maps_prune_oldest_first_at_500() {
        let mut runs = RunMaps::default();
        for i in 0..600 {
            runs.record(
                &format!("run_{i}"),
                &format!("agt_{i}"),
                &format!("tsk_{i}"),
            );
        }
        assert_eq!(runs.agent_of_run.len(), MAX_RUN_MAP_SIZE);
        assert_eq!(runs.task_of_run.len(), MAX_RUN_MAP_SIZE);
        assert_eq!(runs.agent_of_run.get("run_0"), None);
        assert_eq!(
            runs.agent_of_run.get("run_599").map(String::as_str),
            Some("agt_599")
        );
    }

    #[test]
    fn details_are_capped_at_2kb() {
        let long = "x".repeat(5_000);
        let msg =
            Message::new(1, 0, "t", Token::White, MsgType::Output).with_detail(Some(long.clone()));
        let detail = msg.detail.unwrap();
        assert!(detail.ends_with("…[truncated]"));
        assert_eq!(detail.chars().count(), format::MAX_DETAIL_LEN + 12);
    }

    #[test]
    fn event_mapping_agent_lifecycle() {
        let runs = RunMaps::default();
        let msg = format_event(
            &OrchestratorEvent::AgentStarted {
                agent_id: "agt_1".into(),
                task_id: "tsk_1".into(),
                run_id: "run_1".into(),
            },
            &runs,
        )
        .unwrap();
        assert_eq!(msg.text, "Started task");
        assert_eq!(msg.color, Token::Green);
        assert_eq!(msg.msg_type, MsgType::Lifecycle);
        assert_eq!(msg.agent_id.as_deref(), Some("agt_1"));
        assert_eq!(msg.task_id.as_deref(), Some("tsk_1"));
    }

    #[test]
    fn event_mapping_agent_completed_both_ways() {
        let runs = RunMaps::default();
        let ok = format_event(
            &OrchestratorEvent::AgentCompleted {
                run_id: "run_1".into(),
                agent_id: "agt_1".into(),
                success: true,
            },
            &runs,
        )
        .unwrap();
        assert_eq!(ok.text, "Completed successfully");
        assert_eq!(ok.color, Token::Green);
        let failed = format_event(
            &OrchestratorEvent::AgentCompleted {
                run_id: "run_1".into(),
                agent_id: "agt_1".into(),
                success: false,
            },
            &runs,
        )
        .unwrap();
        assert_eq!(failed.text, "Failed");
        assert_eq!(failed.color, Token::Red);
    }

    #[test]
    fn event_mapping_file_changed_is_purple_file_type() {
        let runs = RunMaps::default();
        let msg = format_event(
            &OrchestratorEvent::AgentFileChanged {
                run_id: "run_1".into(),
                agent_id: "agt_1".into(),
                path: "src/main.rs".into(),
            },
            &runs,
        )
        .unwrap();
        assert_eq!(msg.text, "src/main.rs");
        assert_eq!(msg.color, Token::Purple);
        assert_eq!(msg.msg_type, MsgType::File);
    }

    #[test]
    fn event_mapping_task_events() {
        let runs = RunMaps::default();
        let status = format_event(
            &OrchestratorEvent::TaskStatusChanged {
                task_id: "tsk_1".into(),
                from: TaskStatus::Todo,
                to: TaskStatus::InProgress,
            },
            &runs,
        )
        .unwrap();
        assert_eq!(status.text, "todo → in_progress");
        assert_eq!(status.msg_type, MsgType::System);

        let assigned = format_event(
            &OrchestratorEvent::TaskAssigned {
                task_id: "tsk_1".into(),
                agent_id: "agt_9".into(),
            },
            &runs,
        )
        .unwrap();
        assert_eq!(assigned.text, "Assigned → agt_9");

        let created = format_event(
            &OrchestratorEvent::TaskCreated {
                task: Box::new(task("tsk_2", "Write docs")),
            },
            &runs,
        )
        .unwrap();
        assert_eq!(created.text, "Created: Write docs");
        assert_eq!(created.color, Token::Amber);
    }

    #[test]
    fn event_mapping_retry_rounds_the_delay_and_resolves_the_run() {
        let mut runs = RunMaps::default();
        runs.record("run_1", "agt_1", "tsk_1");
        let msg = format_event(
            &OrchestratorEvent::RunRetry {
                run_id: "run_1".into(),
                attempt: 2,
                delay_ms: 2_600,
            },
            &runs,
        )
        .unwrap();
        assert_eq!(msg.text, "Retry #2 (3s delay)");
        assert_eq!(msg.agent_id.as_deref(), Some("agt_1"));
        assert_eq!(msg.task_id.as_deref(), Some("tsk_1"));
    }

    #[test]
    fn idle_ticks_produce_no_message() {
        let runs = RunMaps::default();
        assert!(
            format_event(
                &OrchestratorEvent::OrchestratorTick {
                    running: 0,
                    queued: 0
                },
                &runs
            )
            .is_none()
        );
        let busy = format_event(
            &OrchestratorEvent::OrchestratorTick {
                running: 2,
                queued: 1,
            },
            &runs,
        )
        .unwrap();
        assert_eq!(busy.text, "2 running · 1 queued");
        assert_eq!(busy.color, Token::Ghost);
    }

    #[test]
    fn event_mapping_stall_and_cascade() {
        let mut runs = RunMaps::default();
        runs.record("run_1", "agt_1", "tsk_1");
        let stall = format_event(
            &OrchestratorEvent::OrchestratorStallDetected {
                run_id: "run_1".into(),
            },
            &runs,
        )
        .unwrap();
        assert_eq!(stall.text, "Stall detected");
        assert_eq!(stall.msg_type, MsgType::Error);
        assert_eq!(stall.color, Token::Yellow);

        let cascade = format_event(
            &OrchestratorEvent::TaskCascadeFailed {
                task_id: "tsk_2".into(),
                failed_dependency_id: "tsk_1".into(),
                reason: "dependency failed".into(),
            },
            &runs,
        )
        .unwrap();
        assert_eq!(cascade.text, "Cascade failed (dep: tsk_1)");
        assert_eq!(cascade.detail.as_deref(), Some("dependency failed"));
    }

    #[test]
    fn unmapped_events_are_ignored_by_the_feed() {
        let runs = RunMaps::default();
        assert!(
            format_event(
                &OrchestratorEvent::TeamDisbanded {
                    team_id: "team_1".into()
                },
                &runs
            )
            .is_none()
        );
    }

    #[test]
    fn activity_presets_cycle_with_wrap() {
        let all = preset_types("all");
        assert_eq!(cycle_preset(&all), "text");
        assert_eq!(cycle_preset(&preset_types("text")), "tools");
        assert_eq!(cycle_preset(&preset_types("tools")), "errors");
        assert_eq!(cycle_preset(&preset_types("errors")), "events");
        assert_eq!(cycle_preset(&preset_types("events")), "all");
        // An unrecognised set restarts at `all`.
        let odd: IndexSet<MsgType> = [MsgType::Info, MsgType::File].into_iter().collect();
        assert_eq!(cycle_preset(&odd), "all");
        assert_eq!(preset_label(&odd), "all");
        assert_eq!(preset_label(&preset_types("tools")), "tools");
    }

    #[test]
    fn preset_contents_match_the_contract() {
        assert_eq!(preset_types("all").len(), 8);
        assert_eq!(
            preset_types("text").into_iter().collect::<Vec<_>>(),
            [MsgType::Output]
        );
        assert_eq!(
            preset_types("tools").into_iter().collect::<Vec<_>>(),
            [MsgType::Tool, MsgType::Result, MsgType::File]
        );
        assert_eq!(
            preset_types("errors").into_iter().collect::<Vec<_>>(),
            [MsgType::Error]
        );
        assert_eq!(
            preset_types("events").into_iter().collect::<Vec<_>>(),
            [MsgType::Lifecycle, MsgType::System]
        );
    }

    #[test]
    fn type_filter_labels() {
        assert_eq!(type_filter_label(&preset_types("all")), "all");
        assert_eq!(type_filter_label(&preset_types("text")), "text");
        assert_eq!(type_filter_label(&preset_types("errors")), "errors");
        assert_eq!(type_filter_label(&preset_types("tools")), "tools");
        assert_eq!(type_filter_label(&preset_types("events")), "events");
        let two: IndexSet<MsgType> = [MsgType::Output, MsgType::Info].into_iter().collect();
        assert_eq!(type_filter_label(&two), "2 types");
    }
}
