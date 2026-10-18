//! Portable authoring data, rule closure and literal built-in bundles.

use serde::{Deserialize, Deserializer, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub const MAX_DOCUMENT_BYTES: usize = 1024 * 1024;
pub const MAX_BUNDLE_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_AGENTS: usize = 128;
pub const MAX_RULES: usize = 64;

// Called only for present fields: null cannot silently become absence.
fn present<'de, D, T>(d: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(d).map(Some)
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Defaults {
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present"
    )]
    pub adapter: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present"
    )]
    pub model: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present"
    )]
    pub command: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present"
    )]
    pub effort: Option<crate::ReasoningEffort>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present"
    )]
    pub approval_policy: Option<crate::ApprovalPolicy>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present"
    )]
    pub max_turns: Option<u32>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present"
    )]
    pub timeout_ms: Option<u64>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present"
    )]
    pub stall_timeout_ms: Option<u64>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present"
    )]
    pub env: Option<indexmap::IndexMap<String, String>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present"
    )]
    pub system_prompt: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present"
    )]
    pub workspace_mode: Option<crate::WorkspaceMode>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present"
    )]
    pub skills: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentDefinition {
    pub name: String,
    pub role: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub rules: Vec<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present"
    )]
    pub adapter: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present"
    )]
    pub model: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present"
    )]
    pub command: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present"
    )]
    pub effort: Option<crate::ReasoningEffort>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present"
    )]
    pub approval_policy: Option<crate::ApprovalPolicy>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present"
    )]
    pub max_turns: Option<u32>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present"
    )]
    pub timeout_ms: Option<u64>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present"
    )]
    pub stall_timeout_ms: Option<u64>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present"
    )]
    pub env: Option<indexmap::IndexMap<String, String>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present"
    )]
    pub system_prompt: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present"
    )]
    pub workspace_mode: Option<crate::WorkspaceMode>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present"
    )]
    pub skills: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OrgDefinition {
    pub schema_version: u32,
    pub name: String,
    pub lead: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub rules: Vec<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present"
    )]
    pub defaults: Option<Defaults>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Bundle {
    pub org: OrgDefinition,
    pub agents: Vec<AgentDefinition>,
    pub rules: BTreeMap<String, String>,
}

pub fn validate_id(id: &str) -> Result<(), String> {
    let reserved = [
        "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8",
        "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
    ];
    if id.is_empty()
        || id.len() > 64
        || !id.as_bytes()[0].is_ascii_lowercase()
        || reserved.contains(&id)
        || id.split('-').any(str::is_empty)
        || !id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    {
        return Err(format!(
            "Invalid template identifier {id:?}; use a lowercase portable slug (1–64 ASCII bytes)"
        ));
    }
    Ok(())
}

const RULE_PREFIX: &str = ".claude/agent-rules/";
fn token_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.' | '/' | '\\')
}

/// Canonical references only; Markdown delimiters and sentence punctuation
/// delimit tokens, while identifiers and path components do not.
pub fn rule_references(text: &str, registry: &[String]) -> Result<Vec<String>, String> {
    let mut refs = Vec::new();
    for token in text.split(|ch| !token_char(ch)).filter(|s| !s.is_empty()) {
        let token = token.trim_end_matches('.');
        if let Some(id) = token
            .strip_prefix(RULE_PREFIX)
            .and_then(|s| s.strip_suffix(".md"))
        {
            validate_id(id)?;
            if !registry.iter().any(|r| r == id) {
                return Err(format!("Unregistered rule reference {token}"));
            }
            refs.push(id.to_owned());
        } else if token.contains(RULE_PREFIX) {
            return Err(format!(
                "Invalid rule reference {token}; use {RULE_PREFIX}<id>.md"
            ));
        } else if let Some(id) = token.strip_suffix(".md")
            && (registry.iter().any(|r| r == id)
                || ["seniority", "collaboration-protocol", "stance"].contains(&id))
        {
            return Err(format!(
                "Ambiguous rule reference {token}; use {RULE_PREFIX}{id}.md"
            ));
        }
    }
    Ok(refs)
}

fn rewrite_references(text: &str, registry: &[String]) -> String {
    let mut out = String::with_capacity(text.len());
    let mut start = 0;
    for (end, ch) in text
        .char_indices()
        .chain(std::iter::once((text.len(), ' ')))
    {
        if token_char(ch) {
            continue;
        }
        if start < end {
            let token = &text[start..end];
            let trimmed = token.trim_end_matches('.');
            if let Some(id) = trimmed
                .strip_prefix(RULE_PREFIX)
                .and_then(|s| s.strip_suffix(".md"))
                && registry.iter().any(|r| r == id)
            {
                out.push_str(&format!("embedded rule «{id}»"));
                out.push_str(&token[trimmed.len()..]);
            } else {
                out.push_str(token);
            }
        }
        if end < text.len() {
            out.push(ch);
        }
        start = end + ch.len_utf8();
    }
    out.replace(
        "Rules (relative to your workspace root):",
        "Rules (embedded in this role):",
    )
}

impl Bundle {
    pub fn validate(&self) -> Result<(), String> {
        validate_id(&self.org.name)?;
        validate_id(&self.org.lead)?;
        if self.org.schema_version != 1 {
            return Err("Unsupported schema_version; expected integer 1".into());
        }
        if self.agents.is_empty() || self.agents.len() > MAX_AGENTS {
            return Err("Template requires 1–128 agents".into());
        }
        if self.org.rules.len() > MAX_RULES {
            return Err("Template permits at most 64 rules".into());
        }
        if let Some(defaults) = &self.org.defaults {
            validate_settings(&serde_json::to_value(defaults).map_err(|e| e.to_string())?)?;
        }
        let mut ids = BTreeSet::new();
        for id in &self.org.rules {
            validate_id(id)?;
            if !ids.insert(id) {
                return Err(format!("Duplicate rule {id}"));
            }
            let text = self
                .rules
                .get(id)
                .ok_or_else(|| format!("Missing rules/{id}.md"))?;
            rule_references(text, &self.org.rules).map_err(|e| format!("rules/{id}.md: {e}"))?;
        }
        if self.rules.keys().any(|id| !ids.contains(id)) {
            return Err("Unregistered rule asset".into());
        }
        let mut names = BTreeSet::new();
        for agent in &self.agents {
            validate_id(&agent.name)?;
            validate_settings(&serde_json::to_value(agent).map_err(|e| e.to_string())?)?;
            if !names.insert(&agent.name) {
                return Err(format!("Duplicate agent {}", agent.name));
            }
            if agent.role.trim().is_empty() {
                return Err(format!("{}: role must not be blank", agent.name));
            }
            for id in &agent.rules {
                if !ids.contains(id) {
                    return Err(format!("{}: unregistered rule {id}", agent.name));
                }
            }
            rule_references(&agent.role, &self.org.rules)
                .map_err(|e| format!("{}: {e}", agent.name))?;
        }
        if !names.contains(&self.org.lead) {
            return Err(format!("Lead {} is missing", self.org.lead));
        }
        Ok(())
    }

    pub fn render_role(&self, agent: &AgentDefinition) -> Result<String, String> {
        let mut pending = agent.rules.clone();
        pending.extend(rule_references(&agent.role, &self.org.rules)?);
        let mut selected = BTreeSet::new();
        while let Some(id) = pending.pop() {
            if !selected.insert(id.clone()) {
                continue;
            }
            let text = self
                .rules
                .get(&id)
                .ok_or_else(|| format!("Missing rule {id}"))?;
            pending.extend(rule_references(text, &self.org.rules)?);
        }
        let mut role = String::from(
            "Embedded shared-rule sections are authoritative; no rule-file lookup is required. <workspace root> means the owning orch project root (the directory containing .orchestry), not the per-task checkout. Establish it from task/context before using destination placeholders. If unavailable, report it to the lead instead of guessing from cwd. Sibling ../pensieve destinations and named skills are deployment prerequisites; edit the template for other layouts.\n\n",
        );
        role.push_str(&rewrite_references(&agent.role, &self.org.rules));
        for id in &self.org.rules {
            if selected.contains(id) {
                role.push_str(&format!("\n\n## Shared rule: {id}\n\n"));
                role.push_str(&rewrite_references(&self.rules[id], &self.org.rules));
            }
        }
        Ok(role)
    }
}

pub mod builtin;
#[cfg(test)]
mod tests;

fn validate_settings(value: &serde_json::Value) -> Result<(), String> {
    if let Some(adapter) = value.get("adapter").and_then(|v| v.as_str())
        && !crate::SUPPORTED_ADAPTERS
            .iter()
            .any(|a| a.as_str() == adapter)
    {
        return Err(format!("Unsupported adapter {adapter}"));
    }
    if value
        .get("model")
        .and_then(|v| v.as_str())
        .is_some_and(|s| s.trim().is_empty())
    {
        return Err("model must not be blank".into());
    }
    for field in ["max_turns", "timeout_ms", "stall_timeout_ms"] {
        if value.get(field).and_then(|v| v.as_u64()) == Some(0) {
            return Err(format!("{field} must be positive"));
        }
    }
    Ok(())
}
