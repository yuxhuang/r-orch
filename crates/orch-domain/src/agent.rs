//! Agent domain model — a configured AI tool.

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

use crate::config::PromptMode;
use crate::task::WorkspaceMode;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentStatus {
    Idle,
    Running,
    Error,
    Disabled,
    Offline,
}

impl AgentStatus {
    pub const ALL: [AgentStatus; 5] = [
        AgentStatus::Idle,
        AgentStatus::Running,
        AgentStatus::Error,
        AgentStatus::Disabled,
        AgentStatus::Offline,
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
            AgentStatus::Idle => "idle",
            AgentStatus::Running => "running",
            AgentStatus::Error => "error",
            AgentStatus::Disabled => "disabled",
            AgentStatus::Offline => "offline",
        }
    }
}

impl std::fmt::Display for AgentStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for AgentStatus {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        AgentStatus::ALL
            .into_iter()
            .find(|v| v.as_str() == s)
            .ok_or(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalPolicy {
    Suggest,
    Auto,
    Manual,
}

impl ApprovalPolicy {
    pub const ALL: [ApprovalPolicy; 3] = [
        ApprovalPolicy::Suggest,
        ApprovalPolicy::Auto,
        ApprovalPolicy::Manual,
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
            ApprovalPolicy::Suggest => "suggest",
            ApprovalPolicy::Auto => "auto",
            ApprovalPolicy::Manual => "manual",
        }
    }
}

impl std::fmt::Display for ApprovalPolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for ApprovalPolicy {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        ApprovalPolicy::ALL
            .into_iter()
            .find(|v| v.as_str() == s)
            .ok_or(())
    }
}

/// A requested reasoning-effort level, spelled exactly as the provider CLIs
/// spell it.
///
/// This is a request for the adapter's native selector, never a statement of
/// the reasoning the provider actually applies: providers may clamp, override
/// or reject it (see [`crate::effort`]). Which spellings an adapter accepts is
/// the capability table's business, not this enum's: every spelling parses so
/// a stored value can always be listed and repaired. An absent value means the
/// provider or its own config decides; `default` is a clearing command
/// ([`crate::effort::EffortSelection`]), never serialized data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ReasoningEffort {
    #[serde(rename = "none")]
    None,
    #[serde(rename = "off")]
    Off,
    #[serde(rename = "minimal")]
    Minimal,
    #[serde(rename = "low")]
    Low,
    #[serde(rename = "medium")]
    Medium,
    #[serde(rename = "high")]
    High,
    #[serde(rename = "xhigh")]
    Xhigh,
    #[serde(rename = "max")]
    Max,
    #[serde(rename = "ultra")]
    Ultra,
}

impl ReasoningEffort {
    pub const ALL: [ReasoningEffort; 9] = [
        ReasoningEffort::None,
        ReasoningEffort::Off,
        ReasoningEffort::Minimal,
        ReasoningEffort::Low,
        ReasoningEffort::Medium,
        ReasoningEffort::High,
        ReasoningEffort::Xhigh,
        ReasoningEffort::Max,
        ReasoningEffort::Ultra,
    ];

    /// The three spellings r-orch ≤ 0.9.4 could write. A stored legacy value
    /// never makes a dispatch fail (lead ruling 2); every other spelling is
    /// validated strictly.
    pub const LEGACY: [ReasoningEffort; 3] = [
        ReasoningEffort::Low,
        ReasoningEffort::Medium,
        ReasoningEffort::High,
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
            ReasoningEffort::None => "none",
            ReasoningEffort::Off => "off",
            ReasoningEffort::Minimal => "minimal",
            ReasoningEffort::Low => "low",
            ReasoningEffort::Medium => "medium",
            ReasoningEffort::High => "high",
            ReasoningEffort::Xhigh => "xhigh",
            ReasoningEffort::Max => "max",
            ReasoningEffort::Ultra => "ultra",
        }
    }

    /// Whether an old binary could have written this value.
    pub fn is_legacy(&self) -> bool {
        Self::LEGACY.contains(self)
    }
}

impl std::fmt::Display for ReasoningEffort {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for ReasoningEffort {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        ReasoningEffort::ALL
            .into_iter()
            .find(|v| v.as_str() == s)
            .ok_or(())
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<ReasoningEffort>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval_policy: Option<ApprovalPolicy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_turns: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stall_timeout_ms: Option<u64>,
    /// Insertion-ordered to mirror the JS object this came from; the spawn
    /// environment is built from it in order.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<IndexMap<String, String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_mode: Option<WorkspaceMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skills: Option<Vec<String>>,
    /// Per-agent prompt-mode override. Absent (or `null`) means the agent
    /// inherits the project's `defaults.agent.prompt_mode` live, so the
    /// project default is never copied in. See [`crate::config::effective_prompt_mode`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_mode: Option<PromptMode>,
    /// Explicit one-line routing hint shown in eco rosters instead of a derived
    /// one. Never replaces `role`. Authoring normalizes it with
    /// [`normalize_role_summary`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role_summary: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentStats {
    pub tasks_completed: u64,
    pub tasks_failed: u64,
    pub total_runs: u64,
    pub total_runtime_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens_used: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentLastError {
    /// Truncated to 500 characters by the writer.
    pub message: String,
    /// An [`crate::error::AdapterErrorKind`] string value.
    pub kind: String,
    pub timestamp: String,
}

/// Field order is the serialized field order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Agent {
    pub id: String,
    pub name: String,
    pub adapter: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    pub config: AgentConfig,
    pub status: AgentStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_task: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub autonomous: Option<bool>,
    pub stats: AgentStats,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<AgentLastError>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capabilities: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote: Option<crate::remote::RemoteRef>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateAgentInput {
    pub name: String,
    pub adapter: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<ReasoningEffort>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval_policy: Option<ApprovalPolicy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_turns: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stall_timeout_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<IndexMap<String, String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_mode: Option<WorkspaceMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skills: Option<Vec<String>>,
    /// Absent stores no override: the agent inherits the project default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_mode: Option<PromptMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role_summary: Option<String>,
}

// ── Role summaries (eco roster routing hints) ───────────────────────────────

/// Maximum UTF-8 bytes of a roster role summary, truncation marker included.
/// Bytes, not provider tokens or UTF-16 units.
pub const ROLE_SUMMARY_MAX_BYTES: usize = 192;

/// The fixed embedded-rule preamble that opens most shipped role documents.
/// A derived summary skips a leading paragraph that begins with this text and
/// ends with [`ROLE_PREAMBLE_END`]; nothing else is skipped by keyword.
pub const ROLE_PREAMBLE_START: &str =
    "Embedded shared-rule sections are authoritative; no rule-file lookup is required.";
/// The last sentence of the known preamble paragraph. See [`ROLE_PREAMBLE_START`].
pub const ROLE_PREAMBLE_END: &str = "edit the template for other layouts.";

/// Why an explicit `role_summary` was rejected at authoring.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RoleSummaryError {
    #[error("role summary is empty after whitespace normalization")]
    Empty,
    #[error(
        "role summary is {bytes} UTF-8 bytes after whitespace normalization; the limit is {max} bytes",
        max = ROLE_SUMMARY_MAX_BYTES
    )]
    TooLong { bytes: usize },
}

/// Collapse every run of whitespace (spaces, tabs, CR, LF, Unicode spaces) to
/// one ASCII space and trim both ends, so a summary is one display line.
/// Other characters, control characters included, are kept as data: the
/// renderer escapes them.
pub fn normalize_whitespace(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Validate and normalize an explicit `role_summary` for storage.
///
/// Explicit prose is never silently shortened: over
/// [`ROLE_SUMMARY_MAX_BYTES`] after normalization is an error, as is empty.
pub fn normalize_role_summary(raw: &str) -> Result<String, RoleSummaryError> {
    let normalized = normalize_whitespace(raw);
    if normalized.is_empty() {
        return Err(RoleSummaryError::Empty);
    }
    if normalized.len() > ROLE_SUMMARY_MAX_BYTES {
        return Err(RoleSummaryError::TooLong {
            bytes: normalized.len(),
        });
    }
    Ok(normalized)
}

const MARKER_START: &str = "[truncated, ";
const MARKER_END: &str = " more bytes]";

/// The visible marker a clipped presentation copy ends with. `omitted` counts
/// the source bytes not shown.
pub fn truncation_marker(omitted: usize) -> String {
    format!("{MARKER_START}{omitted}{MARKER_END}")
}

/// Clip `text` to at most `max_bytes` UTF-8 bytes, marker included.
///
/// Text that fits is returned unchanged with no marker. Otherwise the longest
/// prefix that leaves room for `" "` plus [`truncation_marker`] is kept, cut
/// at a char boundary and with trailing whitespace dropped, and the marker's
/// count is the original length minus the retained prefix length. The marker
/// width depends on that count, so the reservation is recomputed until it is
/// stable. If `max_bytes` cannot hold even the marker, only the marker is
/// returned.
pub fn clip_with_marker(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_owned();
    }
    let mut digits = 1;
    loop {
        // The separating space, then the marker with a `digits`-wide count.
        let reserved = 1 + MARKER_START.len() + digits + MARKER_END.len();
        let cut = text.floor_char_boundary(max_bytes.saturating_sub(reserved));
        let prefix = text[..cut].trim_end();
        let omitted = text.len() - prefix.len();
        let needed = omitted.to_string().len();
        if needed <= digits {
            return if prefix.is_empty() {
                truncation_marker(omitted)
            } else {
                format!("{prefix} {}", truncation_marker(omitted))
            };
        }
        digits = needed;
    }
}

/// A deterministic one-line routing hint derived from a role document, used
/// when no explicit `role_summary` exists. This is a display heuristic, not a
/// summarizer:
///
/// 1. split `role` into blank-line-separated paragraphs;
/// 2. skip paragraphs made only of Markdown ATX heading lines;
/// 3. skip the first remaining paragraph only if it is the known embedded-rule
///    preamble (begins with [`ROLE_PREAMBLE_START`], ends with
///    [`ROLE_PREAMBLE_END`]);
/// 4. take the next paragraph, normalize whitespace, and keep its first
///    sentence: up to and including the first `.`, `!` or `?` followed by
///    whitespace or the end;
/// 5. with no such paragraph, use the normalized agent name.
///
/// The result is clipped to [`ROLE_SUMMARY_MAX_BYTES`] with
/// [`clip_with_marker`].
pub fn derive_role_summary(name: &str, role: Option<&str>) -> String {
    let paragraph = role.and_then(first_summary_paragraph);
    let summary = match paragraph {
        Some(paragraph) => first_sentence(&paragraph).to_owned(),
        None => normalize_whitespace(name),
    };
    clip_with_marker(&summary, ROLE_SUMMARY_MAX_BYTES)
}

/// The summary the eco roster shows for `agent`: the explicit
/// `config.role_summary` when present and non-empty, else
/// [`derive_role_summary`]. A hand-edited explicit value is normalized and
/// clipped for display; the stored text is not changed.
pub fn role_summary_for(agent: &Agent) -> String {
    match agent
        .config
        .role_summary
        .as_deref()
        .map(normalize_whitespace)
    {
        Some(explicit) if !explicit.is_empty() => {
            clip_with_marker(&explicit, ROLE_SUMMARY_MAX_BYTES)
        }
        _ => derive_role_summary(&agent.name, agent.role.as_deref()),
    }
}

fn is_heading_line(line: &str) -> bool {
    // CommonMark ATX heading: up to three spaces of indentation, then 1–6 '#'
    // followed by whitespace or the end of the line.
    let indent = line.len() - line.trim_start_matches(' ').len();
    if indent > 3 {
        return false;
    }
    let rest = &line[indent..];
    let hashes = rest.len() - rest.trim_start_matches('#').len();
    (1..=6).contains(&hashes)
        && rest[hashes..]
            .chars()
            .next()
            .is_none_or(char::is_whitespace)
}

fn first_summary_paragraph(role: &str) -> Option<String> {
    let mut paragraphs: Vec<Vec<&str>> = Vec::new();
    let mut current: Vec<&str> = Vec::new();
    for line in role.lines() {
        if line.trim().is_empty() {
            if !current.is_empty() {
                paragraphs.push(std::mem::take(&mut current));
            }
        } else {
            current.push(line);
        }
    }
    if !current.is_empty() {
        paragraphs.push(current);
    }

    let mut candidates = paragraphs
        .into_iter()
        .filter(|lines| !lines.iter().all(|line| is_heading_line(line)))
        .map(|lines| normalize_whitespace(&lines.join(" ")));
    let first = candidates.next()?;
    if first.starts_with(ROLE_PREAMBLE_START) && first.ends_with(ROLE_PREAMBLE_END) {
        candidates.next()
    } else {
        Some(first)
    }
}

fn first_sentence(paragraph: &str) -> &str {
    let mut chars = paragraph.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        if matches!(c, '.' | '!' | '?') && chars.peek().is_none_or(|(_, next)| next.is_whitespace())
        {
            return &paragraph[..i + c.len_utf8()];
        }
    }
    paragraph
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_agent_has_the_expected_shape() {
        let agent = Agent {
            id: "agt_veWG_kK".into(),
            name: "shelly".into(),
            adapter: "shell".into(),
            role: None,
            config: AgentConfig {
                command: Some("echo hello".into()),
                approval_policy: Some(ApprovalPolicy::Auto),
                max_turns: Some(50),
                timeout_ms: Some(3_600_000),
                stall_timeout_ms: Some(600_000),
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

        let json = serde_json::to_string_pretty(&agent).unwrap();
        assert_eq!(
            json,
            r#"{
  "id": "agt_veWG_kK",
  "name": "shelly",
  "adapter": "shell",
  "config": {
    "command": "echo hello",
    "approval_policy": "auto",
    "max_turns": 50,
    "timeout_ms": 3600000,
    "stall_timeout_ms": 600000
  },
  "status": "idle",
  "stats": {
    "tasks_completed": 0,
    "tasks_failed": 0,
    "total_runs": 0,
    "total_runtime_ms": 0
  }
}"#
        );
    }

    #[test]
    fn enum_strings_round_trip() {
        for p in ApprovalPolicy::ALL {
            assert_eq!(
                serde_json::to_string(&p).unwrap(),
                format!("\"{}\"", p.as_str())
            );
            assert_eq!(p.as_str().parse::<ApprovalPolicy>(), Ok(p));
        }
        for s in AgentStatus::ALL {
            assert_eq!(
                serde_json::to_string(&s).unwrap(),
                format!("\"{}\"", s.as_str())
            );
            assert_eq!(s.as_str().parse::<AgentStatus>(), Ok(s));
        }
        for e in ReasoningEffort::ALL {
            assert_eq!(
                serde_json::to_string(&e).unwrap(),
                format!("\"{}\"", e.as_str())
            );
            assert_eq!(e.as_str().parse::<ReasoningEffort>(), Ok(e));
        }
    }

    #[test]
    fn empty_agent_config_serializes_as_an_empty_map() {
        assert_eq!(
            serde_json::to_string(&AgentConfig::default()).unwrap(),
            "{}"
        );
    }

    #[test]
    fn every_effort_spelling_parses_and_only_three_are_legacy() {
        let spellings: Vec<&str> = ReasoningEffort::ALL.iter().map(|e| e.as_str()).collect();
        assert_eq!(
            spellings,
            [
                "none", "off", "minimal", "low", "medium", "high", "xhigh", "max", "ultra"
            ]
        );
        let legacy: Vec<ReasoningEffort> = ReasoningEffort::ALL
            .into_iter()
            .filter(ReasoningEffort::is_legacy)
            .collect();
        assert_eq!(legacy, ReasoningEffort::LEGACY.to_vec());
        for bad in ["default", "inherit", "High", "ultracode", ""] {
            assert_eq!(bad.parse::<ReasoningEffort>(), Err(()), "{bad}");
        }
    }

    const OLD_AGENT_YAML: &str = "id: agt_old\nname: old\nadapter: claude\nconfig:\n  model: claude-sonnet-4-6\n  effort: high\nstatus: idle\nstats:\n  tasks_completed: 0\n  tasks_failed: 0\n  total_runs: 0\n  total_runtime_ms: 0\n";

    #[test]
    fn an_old_agent_file_inherits_the_mode_and_is_not_backfilled() {
        let agent: Agent = serde_yaml_ng::from_str(OLD_AGENT_YAML).unwrap();
        assert_eq!(agent.config.prompt_mode, None);
        assert_eq!(agent.config.role_summary, None);
        assert_eq!(agent.config.effort, Some(ReasoningEffort::High));
        assert_eq!(serde_yaml_ng::to_string(&agent).unwrap(), OLD_AGENT_YAML);
    }

    #[test]
    fn new_agent_fields_round_trip_and_null_means_inherit() {
        let yaml = "id: a\nname: n\nadapter: codex\nconfig:\n  effort: ultra\n  prompt_mode: eco\n  role_summary: Reviews Rust.\nstatus: idle\nstats:\n  tasks_completed: 0\n  tasks_failed: 0\n  total_runs: 0\n  total_runtime_ms: 0\n";
        let agent: Agent = serde_yaml_ng::from_str(yaml).unwrap();
        assert_eq!(agent.config.prompt_mode, Some(PromptMode::Eco));
        assert_eq!(agent.config.effort, Some(ReasoningEffort::Ultra));
        assert_eq!(agent.config.role_summary.as_deref(), Some("Reviews Rust."));
        assert_eq!(serde_yaml_ng::to_string(&agent).unwrap(), yaml);

        let inherit: AgentConfig = serde_yaml_ng::from_str("prompt_mode: null\n").unwrap();
        assert_eq!(inherit.prompt_mode, None);
        // `inherit` and `default` are commands, never stored spellings.
        assert!(serde_yaml_ng::from_str::<AgentConfig>("prompt_mode: inherit\n").is_err());
        assert!(serde_yaml_ng::from_str::<AgentConfig>("effort: default\n").is_err());
    }

    // ── Role summaries ──

    /// `n` ASCII bytes.
    fn ascii(n: usize) -> String {
        "a".repeat(n)
    }

    #[test]
    fn explicit_summaries_are_normalized_to_one_line_and_bounded_at_192_bytes() {
        assert_eq!(
            normalize_role_summary("  Reviews\r\nRust\tcode.\n\n  Carefully. "),
            Ok("Reviews Rust code. Carefully.".to_owned())
        );
        assert_eq!(normalize_role_summary(&ascii(191)), Ok(ascii(191)));
        assert_eq!(normalize_role_summary(&ascii(192)), Ok(ascii(192)));
        assert_eq!(
            normalize_role_summary(&ascii(193)),
            Err(RoleSummaryError::TooLong { bytes: 193 })
        );
        // Measured after normalization: surrounding whitespace does not count.
        assert_eq!(
            normalize_role_summary(&format!("  {}\n", ascii(192))),
            Ok(ascii(192))
        );
        // A 4-byte emoji ending at byte 193 is over the limit in bytes, not chars.
        let emoji = format!("{}😀", ascii(189));
        assert_eq!(emoji.len(), 193);
        assert_eq!(
            normalize_role_summary(&emoji),
            Err(RoleSummaryError::TooLong { bytes: 193 })
        );
        assert_eq!(
            normalize_role_summary(" \t\r\n "),
            Err(RoleSummaryError::Empty)
        );
        assert_eq!(normalize_role_summary(""), Err(RoleSummaryError::Empty));
    }

    #[test]
    fn clipping_keeps_fitting_text_and_marks_clipped_text_exactly() {
        assert_eq!(clip_with_marker(&ascii(191), 192), ascii(191));
        assert_eq!(clip_with_marker(&ascii(192), 192), ascii(192));

        // 193 bytes: marker "[truncated, 26 more bytes]" (26 bytes) plus the
        // separating space leaves a 165-byte prefix; 193 - 165 = 28 omitted.
        let clipped = clip_with_marker(&ascii(193), 192);
        assert_eq!(
            clipped,
            format!("{} [truncated, 28 more bytes]", ascii(165))
        );
        assert_eq!(clipped.len(), 192);
    }

    #[test]
    fn clipping_never_splits_a_character_and_counts_what_it_omits() {
        // Emoji straddling the cut point: dropped whole.
        let text = format!("{}😀{}", ascii(163), ascii(100));
        let clipped = clip_with_marker(&text, 192);
        assert!(clipped.len() <= 192);
        assert_eq!(
            clipped,
            format!(
                "{} [truncated, {} more bytes]",
                ascii(163),
                text.len() - 163
            )
        );

        // Multi-byte everywhere: every cut is still on a boundary.
        for len in 180..200 {
            let text: String = "é".repeat(len);
            let clipped = clip_with_marker(&text, 192);
            assert!(clipped.len() <= 192, "{len}");
            let kept = clipped.split(" [truncated, ").next().unwrap();
            assert!(text.starts_with(kept));
            if text.len() > 192 {
                assert_eq!(
                    clipped,
                    format!("{kept} {}", truncation_marker(text.len() - kept.len()))
                );
            }
        }
    }

    #[test]
    fn the_marker_count_widens_with_the_omitted_byte_count() {
        let text = ascii(100_000);
        let clipped = clip_with_marker(&text, 192);
        // "[truncated, 99835 more bytes]" is 29 bytes; 192 - 30 = 162 kept.
        assert_eq!(
            clipped,
            format!("{} [truncated, 99838 more bytes]", ascii(162))
        );
        assert_eq!(clipped.len(), 192);
        // Too small to hold any text: the marker alone.
        assert_eq!(clip_with_marker(&ascii(50), 5), truncation_marker(50));
    }

    const PREAMBLE: &str = "Embedded shared-rule sections are authoritative; no rule-file lookup is required. <workspace root> means the owning orch project root (the directory containing .orchestry), not the per-task checkout.\nEstablish it from task/context before using destination placeholders. Sibling ../pensieve destinations and named skills are deployment prerequisites; edit the template for other layouts.";

    #[test]
    fn a_derived_summary_skips_headings_and_the_known_preamble() {
        let role = format!(
            "# Senior engineer\n\n{PREAMBLE}\n\nStaff software engineer with decades\nof shipping production code. You write code that reads well.\n\n## Rules\n- one"
        );
        assert_eq!(
            derive_role_summary("senior-eng", Some(&role)),
            "Staff software engineer with decades of shipping production code."
        );
        // Without the preamble the first paragraph is used.
        assert_eq!(
            derive_role_summary("x", Some("Senior data engineer! Your job is data.")),
            "Senior data engineer!"
        );
        // No sentence terminator: the whole (normalized) paragraph.
        assert_eq!(
            derive_role_summary("x", Some("Backend   developer")),
            "Backend developer"
        );
        // A '.' not followed by whitespace does not end a sentence.
        assert_eq!(
            derive_role_summary("x", Some("Runs pi 1.4.0 agents. Then more.")),
            "Runs pi 1.4.0 agents."
        );
    }

    #[test]
    fn a_near_match_preamble_is_not_stripped() {
        let near = PREAMBLE.replacen("Embedded", "Embedded-ish", 1);
        let role = format!("{near}\n\nSenior tester.");
        assert_eq!(
            derive_role_summary("x", Some(&role)),
            "Embedded-ish shared-rule sections are authoritative; no rule-file lookup is required."
        );
        // Only the leading paragraph is the preamble's slot.
        let later = format!("Senior tester.\n\n{PREAMBLE}");
        assert_eq!(derive_role_summary("x", Some(&later)), "Senior tester.");
    }

    #[test]
    fn empty_or_headings_only_roles_fall_back_to_the_agent_name() {
        assert_eq!(
            derive_role_summary("  test\tengineer ", None),
            "test engineer"
        );
        assert_eq!(derive_role_summary("qa", Some("")), "qa");
        assert_eq!(
            derive_role_summary("qa", Some("# Title\n## Sub\n\n   \n# Again")),
            "qa"
        );
        assert_eq!(derive_role_summary("qa", Some(PREAMBLE)), "qa");
        // "#hashtag" is not a heading.
        assert_eq!(
            derive_role_summary("qa", Some("#hashtag role.")),
            "#hashtag role."
        );
    }

    #[test]
    fn a_long_derived_sentence_is_clipped_to_the_roster_limit() {
        let role = format!("{}😀{} done.", ascii(150), ascii(100));
        let summary = derive_role_summary("x", Some(&role));
        assert!(summary.len() <= ROLE_SUMMARY_MAX_BYTES);
        assert!(summary.ends_with(" more bytes]"), "{summary}");
        assert!(!summary.contains('\n'));
    }

    #[test]
    fn an_explicit_summary_wins_over_the_derived_one() {
        let mut agent: Agent = serde_yaml_ng::from_str(OLD_AGENT_YAML).unwrap();
        agent.role = Some("Derived sentence. More.".into());
        assert_eq!(role_summary_for(&agent), "Derived sentence.");
        agent.config.role_summary = Some("  Explicit\nhint ".into());
        assert_eq!(role_summary_for(&agent), "Explicit hint");
        // A blank hand-edited value falls back rather than showing nothing.
        agent.config.role_summary = Some(" \n ".into());
        assert_eq!(role_summary_for(&agent), "Derived sentence.");
        // An oversized hand-edited value is clipped for display only.
        agent.config.role_summary = Some(ascii(300));
        assert_eq!(role_summary_for(&agent).len(), ROLE_SUMMARY_MAX_BYTES);
        assert_eq!(
            agent.config.role_summary.as_deref().map(str::len),
            Some(300)
        );
    }
}
