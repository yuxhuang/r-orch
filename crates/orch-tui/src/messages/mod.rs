//! The activity-message model: types, icons, colours, the event formatter and
//! the run-id maps (`src/tui/App.tsx`).
//!
//! The caps here are OOM defences, not cosmetics (PARITY.md §12.6).

pub mod format;
pub mod retention;

use std::sync::Arc;

use indexmap::{IndexMap, IndexSet};
use orch_domain::OrchestratorEvent;

use crate::theme::Token;
use format::{
    Classification, SummaryColor, SummaryKind, classify_agent_summary, format_agent_error,
    format_agent_output,
};

/// Activity-feed ring buffer size.
pub const MAX_MESSAGES: usize = 500;
/// Largest retained message body or document representation, in UTF-8 bytes
/// (design D4). A larger payload is rejected whole into an explicit
/// "Body unavailable" record, never cut.
pub const MAX_BODY_BYTES: usize = 1024 * 1024;
/// Shared budget for every owned text allocation of the pending and live
/// message collections together, counted once per record (design D4).
pub const RETENTION_BUDGET_BYTES: usize = 32 * 1024 * 1024;

const UNAVAILABLE_PREFIX: &str = "Body unavailable: exceeds 1 MiB (";
const UNAVAILABLE_SUFFIX: &str = " bytes received)";

/// The short record stored in place of a body over [`MAX_BODY_BYTES`].
pub fn unavailable_notice(original_bytes: usize) -> String {
    format!("{UNAVAILABLE_PREFIX}{original_bytes}{UNAVAILABLE_SUFFIX}")
}
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
    /// Canonical immutable raw detail, shared with pinned viewer snapshots.
    pub detail: Option<Arc<str>>,
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

    /// Store the complete detail. Size admission is the retention helpers'
    /// job (design D4); nothing here cuts a body.
    pub fn with_detail(mut self, detail: Option<String>) -> Self {
        self.detail = detail.map(Arc::from);
        self
    }

    /// The received size of a body that was rejected whole for exceeding
    /// [`MAX_BODY_BYTES`], or `None` for a retained body.
    ///
    /// A rejection is the pair (notice in the text, empty detail): a real
    /// payload cannot produce it, because an empty payload also has an empty
    /// summary.
    pub fn unavailable_bytes(&self) -> Option<usize> {
        if self.detail.as_deref() != Some("") {
            return None;
        }
        let start = self.text.find(UNAVAILABLE_PREFIX)? + UNAVAILABLE_PREFIX.len();
        let rest = &self.text[start..];
        rest.strip_suffix(UNAVAILABLE_SUFFIX)
            .unwrap_or(rest.split(UNAVAILABLE_SUFFIX).next()?)
            .parse()
            .ok()
    }

    /// Owned text allocation capacity this record holds, as charged against
    /// [`RETENTION_BUDGET_BYTES`]: body, detail, preview and ID strings.
    pub fn owned_bytes(&self) -> usize {
        record_bytes(
            &self.text,
            self.detail.as_ref(),
            self.agent_id.as_ref(),
            self.task_id.as_ref(),
        )
    }
}

/// The frozen viewer document for one retained message (design D3/O1).
///
/// The default is the **complete decoded human text** of a recognised
/// envelope, with the exact received payload pinned beside it as Raw; plain
/// text and unknown or malformed envelopes default to the exact received
/// bytes. A record rejected for size becomes an explicit unavailable
/// document, never an empty "complete" body. A legacy record without a
/// stored detail falls back to its full text and says so.
///
/// Raw shares the immutable retained detail. Human is decoded once on open;
/// switching never re-reads or re-decodes the (possibly evicted) record.
/// Legacy records have no canonical detail, so their text fallback is copied.
pub fn message_document(
    message: &Message,
    initial_anchor: usize,
    captured_at_ms: i64,
) -> crate::widgets::text_viewer::ViewerDocument {
    use crate::widgets::text_viewer::{Completeness, Representation, ViewerDocument, ViewerSource};

    let source = ViewerSource::Message(message.id);
    let legacy = message.detail.is_none();
    let base_title = if legacy {
        format!(
            "Message {} (legacy text source; completeness unknown)",
            message.id
        )
    } else {
        format!("Message {}", message.id)
    };
    let raw = message.detail.as_deref().unwrap_or(&message.text);
    if let Some(original_bytes) = message
        .unavailable_bytes()
        .or_else(|| (raw.len() > MAX_BODY_BYTES).then_some(raw.len()))
    {
        return ViewerDocument {
            source,
            title: base_title,
            body: Arc::from(""),
            representation: Representation::Raw,
            raw_body: None,
            completeness: Completeness::Unavailable {
                reason: "Body unavailable: exceeds 1 MiB".into(),
            },
            original_bytes,
            captured_at_ms,
            initial_anchor: 0,
        };
    }
    let parsed = serde_json::from_str::<serde_json::Value>(raw).ok();
    let human = parsed
        .as_ref()
        .and_then(|value| known_message_text(value, 0))
        .or_else(|| {
            let value = parsed.as_ref()?;
            if message.msg_type != MsgType::Error
                || !["error", "message", "detail", "reason"]
                    .iter()
                    .any(|key| value.get(key).is_some())
            {
                return None;
            }
            let decoded = orch_domain::extract_error_message(value);
            // The domain helper falls back to serializing an unsupported
            // object. That is not decoded human content: keep exact Raw.
            let unchanged = serde_json::from_str::<serde_json::Value>(&decoded)
                .ok()
                .as_ref()
                == Some(value);
            (!decoded.is_empty() && decoded != raw && !unchanged).then_some((decoded, false))
        });
    let raw_body = || message.detail.clone().unwrap_or_else(|| Arc::from(raw));
    let (body, representation, raw_body, title) =
        if let Some((human, joined)) = human.filter(|(text, _)| text.len() <= MAX_BODY_BYTES) {
            (
                Arc::<str>::from(human),
                Representation::Human,
                Some(raw_body()),
                if joined {
                    format!("{base_title} · Human (text blocks joined)")
                } else {
                    base_title
                },
            )
        } else {
            (raw_body(), Representation::Raw, None, base_title)
        };
    let mut anchor = initial_anchor.min(body.len());
    while !body.is_char_boundary(anchor) {
        anchor -= 1;
    }
    ViewerDocument {
        source,
        title,
        body,
        representation,
        raw_body,
        completeness: Completeness::Complete,
        original_bytes: raw.len(),
        captured_at_ms,
        initial_anchor: anchor,
    }
}

fn known_message_text(value: &serde_json::Value, depth: usize) -> Option<(String, bool)> {
    use serde_json::Value;
    if depth >= 8 {
        return None;
    }
    let kind = value.get("type").and_then(Value::as_str);
    if matches!(kind, Some("item.started" | "item.completed")) {
        return known_message_text(value.get("item")?, depth + 1);
    }
    if (kind == Some("agent_message") || (kind.is_none() && value.get("role").is_none()))
        && let Some(text) = value.get("text").and_then(Value::as_str)
    {
        return Some((text.into(), false));
    }
    let content = if matches!(kind, Some("message" | "assistant"))
        || value.get("role").and_then(Value::as_str) == Some("assistant")
        || value.get("subtype").is_some()
    {
        value
            .get("message")
            .and_then(|message| message.get("content"))
            .filter(|content| !content.is_null())
            .or_else(|| value.get("content"))
            .or_else(|| value.get("subtype").and_then(|_| value.get("message")))
    } else {
        value.get("content")
    };
    if let Some(content) = content {
        if let Some(text) = content.as_str() {
            return Some((text.into(), false));
        }
        let blocks: Vec<_> = content
            .as_array()?
            .iter()
            .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|b| b.get("text").and_then(Value::as_str))
            .collect();
        if !blocks.is_empty() {
            return Some((blocks.join("\n"), blocks.len() > 1));
        }
    }
    if (kind == Some("result") || kind.is_none())
        && let Some(result) = value.get("result").and_then(Value::as_str)
    {
        return Some((result.into(), false));
    }
    if kind == Some("command_execution") {
        let command = value.get("command").and_then(Value::as_str)?;
        let output = value
            .get("aggregated_output")
            .or_else(|| value.get("result"))
            .and_then(Value::as_str);
        return Some((
            output.map_or_else(|| command.into(), |output| format!("{command}\n{output}")),
            false,
        ));
    }
    None
}

pub(crate) fn record_bytes(
    text: &String,
    detail: Option<&String>,
    agent: Option<&String>,
    task: Option<&String>,
) -> usize {
    text.capacity()
        .saturating_add(detail.map_or(0, String::capacity))
        .saturating_add(agent.map_or(0, String::capacity))
        .saturating_add(task.map_or(0, String::capacity))
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

impl PendingMessage {
    /// Owned text allocation capacity, charged exactly like [`Message::owned_bytes`].
    pub fn owned_bytes(&self) -> usize {
        record_bytes(
            &self.text,
            self.detail.as_ref(),
            self.agent_id.as_ref(),
            self.task_id.as_ref(),
        )
    }
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
    fn details_are_stored_whole() {
        let long = "x".repeat(50 * 1024);
        let msg =
            Message::new(1, 0, "t", Token::White, MsgType::Output).with_detail(Some(long.clone()));
        assert_eq!(msg.detail.as_deref(), Some(long.as_str()));
        assert_eq!(msg.unavailable_bytes(), None);
    }

    #[test]
    fn the_unavailable_pair_reports_its_received_size() {
        let msg = Message::new(
            1,
            0,
            &unavailable_notice(1_048_577),
            Token::White,
            MsgType::Output,
        )
        .with_detail(Some(String::new()));
        assert_eq!(msg.unavailable_bytes(), Some(1_048_577));
        // The same text with a real detail is just text.
        let forged = Message::new(2, 0, &unavailable_notice(5), Token::White, MsgType::Output)
            .with_detail(Some("x".into()));
        assert_eq!(forged.unavailable_bytes(), None);
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
