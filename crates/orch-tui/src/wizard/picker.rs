//! Type-ahead filtering and ordering for the wizards' agent steps (R7, R8).
//!
//! An agent step stays a `Select` (or, for team members, a `Multiselect`)
//! over the eligible agents, so whatever it submits is one of the ids it
//! listed, or the sentinel. The picker only changes which of those choices
//! are shown and in what order:
//!
//! ```text
//! step options (sentinel first, then eligible agents, catalogue order)
//!   -> sentinel rows: kept first, never filtered
//!   -> agent rows: keep if the query is in the name, adapter or role
//!   -> stable sort by the chosen order (ties keep catalogue order)
//!   -> displayed rows; numbers, digits and clicks index this list
//! ```
//!
//! Typed text is a filter, never a value: there is no path from the query to
//! a submitted id other than choosing a listed row.

use std::collections::HashMap;
use std::rc::Rc;

use orch_domain::{Agent, AgentStatus};

use crate::wizard::engine::Choice;

/// The longest filter query, in Unicode scalar values (the same bound the
/// table keyword filter uses).
pub const QUERY_MAX_CHARS: usize = 256;

/// The order an agent step lists its agents in. `Ctrl+O` cycles it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PickerOrder {
    /// The catalogue order the agents were loaded in.
    #[default]
    Original,
    Name,
    Adapter,
    Status,
}

impl PickerOrder {
    /// The next order in the cycle Original → Name → Adapter → Status.
    pub fn next(self) -> Self {
        match self {
            PickerOrder::Original => PickerOrder::Name,
            PickerOrder::Name => PickerOrder::Adapter,
            PickerOrder::Adapter => PickerOrder::Status,
            PickerOrder::Status => PickerOrder::Original,
        }
    }

    /// The name the step hint shows.
    pub fn label(self) -> &'static str {
        match self {
            PickerOrder::Original => "Original",
            PickerOrder::Name => "Name",
            PickerOrder::Adapter => "Adapter",
            PickerOrder::Status => "Status",
        }
    }
}

/// The case fold both the agent text and the query go through: lowercase,
/// then drop the combining dot above (U+0307). `İ` lowercases to `i` plus
/// that dot, so without the drop a plain `istanbul` query would miss an agent
/// named `İstanbul`. Hand-rolled (AGENTS.md, library-first): this one-code-point
/// rule is the whole need, and no full case-folding crate is in the graph.
fn fold(text: &str) -> String {
    text.to_lowercase()
        .chars()
        .filter(|&c| c != '\u{0307}')
        .collect()
}

/// What the picker knows about one listed agent, folded once at build
/// time so filtering and sorting never allocate per comparison.
#[derive(Debug, Clone)]
struct Entry {
    name: String,
    adapter: String,
    role: String,
    status: AgentStatus,
    status_rank: usize,
}

/// The filter and order metadata for an agent step, keyed by agent id.
///
/// Choices whose value is not a known agent id (the `Auto-assign` / `None`
/// sentinels) are treated as sentinels: listed first, in their own order,
/// whatever the query.
#[derive(Debug, Clone, Default)]
pub struct AgentPicker {
    entries: Rc<HashMap<String, Entry>>,
}

impl AgentPicker {
    pub fn new(agents: &[Agent]) -> Self {
        let entries = agents
            .iter()
            .map(|a| {
                (
                    a.id.clone(),
                    Entry {
                        name: fold(&a.name),
                        adapter: fold(&a.adapter),
                        role: fold(a.role.as_deref().unwrap_or("")),
                        status: a.status,
                        status_rank: AgentStatus::ALL
                            .iter()
                            .position(|s| *s == a.status)
                            .unwrap_or(AgentStatus::ALL.len()),
                    },
                )
            })
            .collect();
        AgentPicker {
            entries: Rc::new(entries),
        }
    }

    /// Whether `value` is an agent row (as opposed to a sentinel).
    pub fn is_agent(&self, value: &str) -> bool {
        self.entries.contains_key(value)
    }

    /// The agent's status, shown in its row, if `value` is an agent.
    pub fn status(&self, value: &str) -> Option<AgentStatus> {
        self.entries.get(value).map(|e| e.status)
    }

    /// The rows to show for `query` in `order`: sentinels first, then the
    /// matching agents. Matching is a case-insensitive ([`fold`]) substring test on the
    /// name, the adapter and the role; an empty query matches every agent.
    pub fn arrange(&self, choices: Vec<Choice>, query: &str, order: PickerOrder) -> Vec<Choice> {
        let query = fold(query.trim());
        let (sentinels, agents): (Vec<Choice>, Vec<Choice>) =
            choices.into_iter().partition(|c| !self.is_agent(&c.value));
        let mut agents: Vec<(&Entry, Choice)> = agents
            .into_iter()
            .filter_map(|c| {
                let entry = self.entries.get(&c.value)?;
                let hit = query.is_empty()
                    || entry.name.contains(&query)
                    || entry.adapter.contains(&query)
                    || entry.role.contains(&query);
                hit.then_some((entry, c))
            })
            .collect();
        // Stable: equal keys keep the catalogue order, which is behaviour
        // (AGENTS.md, "Stable sorts only").
        match order {
            PickerOrder::Original => {}
            PickerOrder::Name => agents.sort_by(|a, b| a.0.name.cmp(&b.0.name)),
            PickerOrder::Adapter => agents.sort_by(|a, b| a.0.adapter.cmp(&b.0.adapter)),
            PickerOrder::Status => agents.sort_by_key(|a| a.0.status_rank),
        }
        sentinels
            .into_iter()
            .chain(agents.into_iter().map(|(_, c)| c))
            .collect()
    }
}

/// Append `text` to `query`, dropping control characters and anything past
/// [`QUERY_MAX_CHARS`]. Returns whether the query changed.
pub(crate) fn push_query(query: &mut String, text: &str) -> bool {
    let room = QUERY_MAX_CHARS.saturating_sub(query.chars().count());
    let before = query.len();
    query.extend(
        text.chars()
            .map(|ch| {
                if matches!(ch, '\n' | '\r' | '\t') {
                    ' '
                } else {
                    ch
                }
            })
            .filter(|ch| !ch.is_control() && !is_bidi_control(*ch))
            .take(room),
    );
    query.len() != before
}

/// Bidirectional overrides would let a pasted query reorder the row it is
/// drawn in.
fn is_bidi_control(ch: char) -> bool {
    matches!(ch, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent(id: &str, name: &str, adapter: &str, role: &str, status: AgentStatus) -> Agent {
        let mut a = crate::backend::make_agent(id, name, status);
        a.adapter = adapter.into();
        a.role = (!role.is_empty()).then(|| role.to_string());
        a
    }

    fn choices(agents: &[Agent]) -> Vec<Choice> {
        let mut out = vec![Choice::new("", "Auto-assign", "")];
        out.extend(agents.iter().map(|a| Choice::new(&a.id, &a.name, "")));
        out
    }

    fn labels(choices: &[Choice]) -> Vec<&str> {
        choices.iter().map(|c| c.label.as_str()).collect()
    }

    #[test]
    fn the_sentinel_stays_first_and_unfiltered() {
        let agents = [agent("a", "zed", "codex", "", AgentStatus::Idle)];
        let picker = AgentPicker::new(&agents);
        let rows = picker.arrange(choices(&agents), "nothing-matches", PickerOrder::Name);
        assert_eq!(labels(&rows), ["Auto-assign"]);
    }

    #[test]
    fn equal_keys_keep_the_catalogue_order() {
        let agents = [
            agent("1", "b", "shell", "", AgentStatus::Running),
            agent("2", "a", "shell", "", AgentStatus::Idle),
            agent("3", "c", "claude", "", AgentStatus::Running),
        ];
        let picker = AgentPicker::new(&agents);
        let rows = picker.arrange(choices(&agents), "", PickerOrder::Adapter);
        assert_eq!(labels(&rows), ["Auto-assign", "c", "b", "a"]);
        let rows = picker.arrange(choices(&agents), "", PickerOrder::Status);
        assert_eq!(labels(&rows), ["Auto-assign", "a", "b", "c"]);
    }

    #[test]
    fn the_query_is_bounded_and_drops_control_characters() {
        let mut query = String::new();
        assert!(push_query(&mut query, "a\u{1b}[2Jb\u{202e}c\nd"));
        assert_eq!(query, "a[2Jbc d");
        let mut long = String::new();
        push_query(&mut long, &"x".repeat(QUERY_MAX_CHARS + 10));
        assert_eq!(long.chars().count(), QUERY_MAX_CHARS);
        assert!(!push_query(&mut long, "y"), "a full query takes nothing");
    }
}
