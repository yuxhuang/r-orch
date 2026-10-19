//! Command registry, tab completion, suggestions and history.
//!
//! Registry **insertion order** is contract: it drives suggestion order and the
//! first-match semantics of [`resolve_completion`]. `msg` is
//! registered **after** `mouse` for exactly that reason: `/m` + Tab must keep
//! ghost-completing `mouse`, the text-selection escape hatch users reach for
//! when clicks stop working (design-v2 §3.5.2, C8).

use orch_domain::Agent;

use crate::format::cap_display;
use crate::msg::{
    BROADCAST_KEYWORD, Candidate, enabled_count, parse_partial, quote_recipient, suggest_recipients,
};

/// The one verb whose arguments are agent names rather than `&'static` words.
pub const MSG_VERB: &str = "msg";

/// Agent rows per panel, past which an inert row says how many were hidden.
const MAX_AGENT_ROWS: usize = 40;

/// Display columns for a row's command text, matching the panel's own pad.
const ROW_COLUMNS: usize = 32;

/// `CONFIG_SETTINGS`.
pub const CONFIG_SETTINGS: [&str; 5] = [
    "palette",
    "activity-filter",
    "max-concurrent",
    "notifications-toast",
    "notifications-bell",
];

pub fn is_config_setting(value: &str) -> bool {
    CONFIG_SETTINGS.contains(&value)
}

pub const CAT_MANAGE: &str = "MANAGEMENT";
pub const CAT_MONITOR: &str = "MONITORING";
pub const CAT_SETTINGS: &str = "SETTINGS";

/// Category order used for the bare-`/` grouped suggestion list.
pub const CATEGORY_ORDER: [&str; 3] = [CAT_MANAGE, CAT_MONITOR, CAT_SETTINGS];

pub struct CommandSpec {
    pub verb: &'static str,
    pub sub: &'static [&'static str],
    pub args: Option<&'static str>,
    pub help: &'static str,
    pub category: &'static str,
    /// The verb's own suggestion row **fills** the bar with `/{verb} ` instead
    /// of executing, because the verb alone does nothing useful.
    ///
    /// `true` only for `msg`: giving it to the other argument-taking verbs
    /// would change what Enter does on rows that have always executed, and
    /// those are existing behaviour (`/run` with no id runs the selected task).
    pub insert_trailing_space: bool,
}

/// `COMMAND_REGISTRY`, in insertion order.
pub const COMMAND_REGISTRY: [CommandSpec; 14] = [
    CommandSpec {
        verb: "task",
        sub: &[
            "add", "list", "show", "cancel", "retry", "assign", "approve", "reject", "delete",
        ],
        args: None,
        help: "Manage tasks",
        category: CAT_MANAGE,
        insert_trailing_space: false,
    },
    CommandSpec {
        verb: "agent",
        sub: &[
            "add",
            "list",
            "disable",
            "enable",
            "delete",
            "autonomous",
            "shop",
        ],
        args: None,
        help: "Manage agents",
        category: CAT_MANAGE,
        insert_trailing_space: false,
    },
    CommandSpec {
        verb: "team",
        sub: &["create", "list", "join", "leave", "disband", "set-lead"],
        args: None,
        help: "Manage teams",
        category: CAT_MANAGE,
        insert_trailing_space: false,
    },
    CommandSpec {
        verb: "goal",
        sub: &["add", "list", "show", "status", "delete"],
        args: None,
        help: "Manage goals",
        category: CAT_MANAGE,
        insert_trailing_space: false,
    },
    CommandSpec {
        verb: "run",
        sub: &[],
        args: Some("[id]"),
        help: "Run task (or selected)",
        category: CAT_MONITOR,
        insert_trailing_space: false,
    },
    CommandSpec {
        verb: "run-all",
        sub: &[],
        args: None,
        help: "Run all todo tasks",
        category: CAT_MONITOR,
        insert_trailing_space: false,
    },
    CommandSpec {
        verb: "watch",
        sub: &[],
        args: None,
        help: "Start watch mode (auto-dispatch)",
        category: CAT_MONITOR,
        insert_trailing_space: false,
    },
    CommandSpec {
        verb: "pause",
        sub: &[],
        args: None,
        help: "Pause watch mode",
        category: CAT_MONITOR,
        insert_trailing_space: false,
    },
    CommandSpec {
        verb: "status",
        sub: &[],
        args: None,
        help: "Show orchestrator status",
        category: CAT_MONITOR,
        insert_trailing_space: false,
    },
    CommandSpec {
        verb: "config",
        sub: &CONFIG_SETTINGS,
        args: None,
        help: "TUI settings",
        category: CAT_SETTINGS,
        insert_trailing_space: false,
    },
    CommandSpec {
        verb: "help",
        sub: &[],
        args: None,
        help: "List all commands",
        category: CAT_SETTINGS,
        insert_trailing_space: false,
    },
    CommandSpec {
        verb: "mouse",
        sub: &["on", "off", "status"],
        args: None,
        help: "Mouse capture on/off",
        category: CAT_SETTINGS,
        insert_trailing_space: false,
    },
    // After `mouse`, so `resolve_completion("/m")` still yields `ouse`.
    // `/msg` lets the command bar message an agent.
    CommandSpec {
        verb: "msg",
        sub: &[],
        args: Some("[agent] [message]"),
        help: "Message an agent",
        category: CAT_MANAGE,
        insert_trailing_space: true,
    },
    CommandSpec {
        verb: "quit",
        sub: &[],
        args: None,
        help: "Exit the TUI",
        category: CAT_SETTINGS,
        insert_trailing_space: false,
    },
];

pub fn spec_for(verb: &str) -> Option<&'static CommandSpec> {
    COMMAND_REGISTRY.iter().find(|s| s.verb == verb)
}

/// One suggestion row. A separator row has an empty `cmd`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Suggestion {
    pub cmd: String,
    pub desc: String,
    pub subs: Option<String>,
    /// The exact text to place in the bar when this row is accepted, replacing
    /// the whole input. `Some(_)` also means **fill, never execute**.
    ///
    /// This is what stops a row being reconstructed from its own display text:
    /// an agent row shows the *unescaped* name (`/msg say "hi"`) and inserts
    /// the quoted form (`/msg "say \"hi\"" `), computed from the agent record.
    /// The legacy accept path trims a trailing ` [...]`, which would mangle a
    /// name like `foo [bar]`.
    pub insert: Option<String>,
    /// Click identity, falling back to `cmd`. The agent id, so two agents with
    /// the same display name still get distinct press/release identities —
    /// mouse release is matched on `ZoneId`, not position.
    pub zone_key: Option<String>,
}

impl Suggestion {
    pub fn is_separator(&self) -> bool {
        self.cmd.is_empty()
    }

    /// What a click on this row identifies: its `zone_key`, else its text.
    pub fn zone_key(&self) -> &str {
        self.zone_key.as_deref().unwrap_or(&self.cmd)
    }
}

/// A verb's own row. `insert` is set only where the verb alone does nothing,
/// which makes Enter fill rather than execute.
fn verb_row(spec: &CommandSpec) -> Suggestion {
    Suggestion {
        cmd: format!("/{}{}", spec.verb, args_hint(spec)),
        desc: spec.help.to_string(),
        subs: subs_hint(spec),
        insert: spec
            .insert_trailing_space
            .then(|| format!("/{} ", spec.verb)),
        zone_key: None,
    }
}

fn args_hint(spec: &CommandSpec) -> String {
    match spec.args {
        Some(a) => format!(" {a}"),
        None => String::new(),
    }
}

fn subs_hint(spec: &CommandSpec) -> Option<String> {
    if spec.sub.is_empty() {
        None
    } else {
        Some(spec.sub.join(" · "))
    }
}

/// The ghost **suffix** only.
pub fn resolve_completion(input: &str) -> Option<String> {
    let body = input.strip_prefix('/')?;
    match body.find(' ') {
        None => {
            if body.is_empty() {
                return None;
            }
            COMMAND_REGISTRY
                .iter()
                .find(|s| s.verb.starts_with(body) && s.verb != body)
                .map(|s| s.verb[body.len()..].to_string())
        }
        Some(space_idx) => {
            let verb = &body[..space_idx];
            let spec = spec_for(verb)?;
            if spec.sub.is_empty() {
                return None;
            }
            let sub_prefix = &body[space_idx + 1..];
            if sub_prefix.is_empty() {
                return None;
            }
            spec.sub
                .iter()
                .find(|s| s.starts_with(sub_prefix) && **s != sub_prefix)
                .map(|s| s[sub_prefix.len()..].to_string())
        }
    }
}

/// What the suggestion list needs from app state: the agents `/msg` can name,
/// and the order the Agents tab shows them in.
///
/// Everything else in the registry is `&'static`, which is why this is the
/// only state the list has ever needed.
#[derive(Debug, Clone, Copy)]
pub struct SuggestionCtx<'a> {
    pub agents: &'a [Agent],
    /// Indices into `agents` — `AppState::sorted_agents`.
    pub order: &'a [usize],
}

impl SuggestionCtx<'_> {
    pub const EMPTY: SuggestionCtx<'static> = SuggestionCtx {
        agents: &[],
        order: &[],
    };
}

/// For every verb but `msg`, which needs the
/// agent list ([`resolve_suggestions_in`]).
pub fn resolve_suggestions(input: &str) -> Vec<Suggestion> {
    resolve_suggestions_in(input, &SuggestionCtx::EMPTY)
}

/// Suggestions for `input`, with the state `/msg` needs.
pub fn resolve_suggestions_in(input: &str, ctx: &SuggestionCtx<'_>) -> Vec<Suggestion> {
    let Some(body) = input.strip_prefix('/') else {
        return Vec::new();
    };

    match body.find(' ') {
        None => {
            let prefix = body.to_lowercase();
            let mut results = Vec::new();

            if prefix.is_empty() {
                for cat in CATEGORY_ORDER {
                    results.push(Suggestion {
                        cmd: String::new(),
                        desc: format!("── {cat} ──"),
                        ..Suggestion::default()
                    });
                    for spec in COMMAND_REGISTRY.iter().filter(|s| s.category == cat) {
                        results.push(verb_row(spec));
                    }
                }
                return results;
            }

            for spec in COMMAND_REGISTRY.iter() {
                if spec.verb.starts_with(&prefix) {
                    results.push(verb_row(spec));
                    if spec.verb == prefix && !spec.sub.is_empty() {
                        for sub in spec.sub {
                            results.push(Suggestion {
                                cmd: format!("/{} {}", spec.verb, sub),
                                desc: format!("{}: {}", spec.help, sub),
                                ..Suggestion::default()
                            });
                        }
                    }
                }
            }
            results
        }
        Some(space_idx) => {
            let verb = &body[..space_idx];
            let Some(spec) = spec_for(verb) else {
                return Vec::new();
            };
            if spec.verb == MSG_VERB {
                return msg_suggestions(&body[space_idx + 1..], ctx);
            }
            if spec.sub.is_empty() {
                return Vec::new();
            }
            let sub_prefix = body[space_idx + 1..].to_lowercase();
            spec.sub
                .iter()
                .filter(|s| sub_prefix.is_empty() || s.starts_with(&sub_prefix))
                .map(|sub| Suggestion {
                    cmd: format!("/{verb} {sub}"),
                    desc: format!("{}: {}", spec.help, sub),
                    ..Suggestion::default()
                })
                .collect()
        }
    }
}

/// The rows offered after `/msg `, in panel order.
///
/// Shown **only** while the input is still inside or at the end of the
/// recipient token. One rule, three jobs: the panel closes once the body
/// starts, Enter cannot be hijacked mid-body, and the bottom panel stays out
/// of the way while a long body is typed.
fn msg_suggestions(tail: &str, ctx: &SuggestionCtx<'_>) -> Vec<Suggestion> {
    let (partial, body_started) = if tail.trim().is_empty() {
        // `/msg ` — nothing typed yet, so every agent is a candidate.
        (String::new(), false)
    } else {
        match parse_partial(tail) {
            Some(token) => (token.text, token.body_started),
            // Junk after a closing quote (`"Alice"x`): malformed, so offer
            // nothing rather than the whole fleet.
            None => return Vec::new(),
        }
    };
    if body_started {
        return Vec::new();
    }

    let candidates = suggest_recipients(ctx.agents, ctx.order, &partial);
    let agents: Vec<usize> = candidates
        .iter()
        .filter_map(|candidate| match candidate {
            Candidate::Agent(index) => Some(*index),
            Candidate::Broadcast => None,
        })
        .collect();
    let shown = agents.len().min(MAX_AGENT_ROWS);

    let mut rows: Vec<Suggestion> = Vec::with_capacity(shown + 2);
    for index in &agents[..shown] {
        let agent = &ctx.agents[*index];
        // Two rows that read the same are marked, so the user knows to look at
        // the id — which the description carries.
        //
        // **Byte equality only**, which is exactly the set of rows resolution
        // cannot tell apart either: a byte-duplicate name is an `Ambiguous`
        // refusal (§3.4.2), and an NFC/NFD twin folds to one and is refused
        // too. A homoglyph pair (Cyrillic `а` against Latin `a`) is *not*
        // marked and does resolve, because the two names are different names;
        // picking the wrong row needs an attacker who can already write the
        // agent config, and NFKC/confusable folding was rejected with reasons
        // (dissent C9). Not a protection this comment should claim.
        let twin = agents[..shown]
            .iter()
            .any(|other| *other != *index && ctx.agents[*other].name == agent.name);
        let display = if twin {
            format!("!{}", agent.name)
        } else {
            agent.name.clone()
        };
        rows.push(Suggestion {
            cmd: cap_display(&format!("/msg {display}"), ROW_COLUMNS),
            desc: format!("{} · {} · {}", agent.adapter, agent.status, agent.id),
            subs: None,
            // The quoted form, from the record — never re-derived from `cmd`.
            insert: Some(format!("/msg {} ", quote_recipient(&agent.name))),
            zone_key: Some(agent.id.clone()),
        });
    }
    if agents.len() > shown {
        rows.push(Suggestion {
            desc: format!("… {} more — keep typing to filter", agents.len() - shown),
            ..Suggestion::default()
        });
    }
    // Last, always: broadcast is the one irreversible action here, so
    // `suggestion_index = 0` must be an agent row whenever any agent matches.
    if candidates.contains(&Candidate::Broadcast) {
        rows.push(Suggestion {
            cmd: format!("/msg {BROADCAST_KEYWORD}"),
            desc: format!("broadcast · {} enabled agent(s)", enabled_count(ctx.agents)),
            subs: None,
            insert: Some(format!("/msg {BROADCAST_KEYWORD} ")),
            zone_key: Some(BROADCAST_KEYWORD.to_owned()),
        });
    }
    rows
}

/// `CommandHistory` — 100 entries, consecutive duplicates collapsed.
#[derive(Debug, Clone, Default)]
pub struct CommandHistory {
    entries: Vec<String>,
    cursor: usize,
}

const HISTORY_CAP: usize = 100;

impl CommandHistory {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn entries(&self) -> &[String] {
        &self.entries
    }

    pub fn push(&mut self, cmd: &str) {
        if cmd.is_empty() {
            return;
        }
        if self.entries.last().map(String::as_str) != Some(cmd) {
            self.entries.push(cmd.to_string());
            if self.entries.len() > HISTORY_CAP {
                self.entries.remove(0);
            }
        }
        self.cursor = self.entries.len();
    }

    /// `↑` — the previous entry, or `None` on an empty history.
    pub fn prev(&mut self) -> Option<String> {
        if self.entries.is_empty() {
            return None;
        }
        if self.cursor > 0 {
            self.cursor -= 1;
        }
        self.entries.get(self.cursor).cloned()
    }

    /// `↓` — the next entry, or `None` past the end (meaning "clear the input").
    ///
    /// Named `next` for the history cursor; it is not an iterator.
    #[allow(
        clippy::should_implement_trait,
        reason = "moves the history cursor; it is not an iterator"
    )]
    pub fn next(&mut self) -> Option<String> {
        if !self.entries.is_empty() && self.cursor + 1 < self.entries.len() {
            self.cursor += 1;
            return self.entries.get(self.cursor).cloned();
        }
        self.cursor = self.entries.len();
        None
    }

    pub fn reset(&mut self) {
        self.cursor = self.entries.len();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_order_and_shape_match_the_contract() {
        let verbs: Vec<&str> = COMMAND_REGISTRY.iter().map(|s| s.verb).collect();
        assert_eq!(
            verbs,
            // `mouse` is a deliberate addition to this verb set: `F2` alone is
            // a weak escape hatch for mouse capture, and a verb is the TUI's
            // own discovery surface.
            [
                "task", "agent", "team", "goal", "run", "run-all", "watch", "pause", "status",
                "config", "help", "mouse", "msg", "quit"
            ]
        );
        // `msg` sits **after** `mouse` so `/m` keeps completing `mouse`
        // (design-v2 §3.5.2). Its position is the contract, not an accident.
        let verbs: Vec<&str> = COMMAND_REGISTRY.iter().map(|s| s.verb).collect();
        assert_eq!(
            verbs.iter().position(|v| *v == "msg"),
            verbs.iter().position(|v| *v == "mouse").map(|i| i + 1)
        );
        assert_eq!(spec_for("msg").unwrap().args, Some("[agent] [message]"));
        assert!(spec_for("msg").unwrap().sub.is_empty());
        assert_eq!(
            spec_for("task").unwrap().sub,
            [
                "add", "list", "show", "cancel", "retry", "assign", "approve", "reject", "delete"
            ]
        );
        assert_eq!(
            spec_for("agent").unwrap().sub,
            [
                "add",
                "list",
                "disable",
                "enable",
                "delete",
                "autonomous",
                "shop"
            ]
        );
        assert_eq!(
            spec_for("team").unwrap().sub,
            ["create", "list", "join", "leave", "disband", "set-lead"]
        );
        assert_eq!(
            spec_for("goal").unwrap().sub,
            ["add", "list", "show", "status", "delete"]
        );
        assert_eq!(spec_for("config").unwrap().sub, CONFIG_SETTINGS);
        assert_eq!(spec_for("run").unwrap().args, Some("[id]"));
        assert!(spec_for("run-all").unwrap().sub.is_empty());
    }

    #[test]
    fn resolve_completion_pinned_vectors() {
        assert_eq!(resolve_completion("task"), None);
        assert_eq!(resolve_completion("/"), None);
        assert_eq!(resolve_completion("/ta").as_deref(), Some("sk"));
        assert_eq!(resolve_completion("/task"), None);
        assert_eq!(resolve_completion("/task c").as_deref(), Some("ancel"));
        assert_eq!(resolve_completion("/task cancel"), None);
        assert_eq!(resolve_completion("/xyz"), None);
        assert_eq!(resolve_completion("/config pal").as_deref(), Some("ette"));
    }

    #[test]
    fn msg_does_not_steal_the_mouse_completion() {
        // The text-selection escape hatch keeps its habit (C8).
        assert_eq!(resolve_completion("/m").as_deref(), Some("ouse"));
        assert_eq!(resolve_completion("/ms").as_deref(), Some("g"));
        assert_eq!(resolve_completion("/mo").as_deref(), Some("use"));
        assert_eq!(resolve_completion("/msg"), None);
        assert_eq!(resolve_completion("/msg x"), None);
    }

    #[test]
    fn resolve_completion_extra_vectors() {
        assert_eq!(resolve_completion("/he").as_deref(), Some("lp"));
        assert_eq!(resolve_completion("/run").as_deref(), Some("-all"));
        assert_eq!(resolve_completion("/task "), None);
        assert_eq!(resolve_completion("/run x"), None);
    }

    #[test]
    fn bare_slash_is_category_grouped_with_three_separators() {
        let suggestions = resolve_suggestions("/");
        let separators: Vec<&Suggestion> =
            suggestions.iter().filter(|s| s.is_separator()).collect();
        assert_eq!(separators.len(), 3);
        assert_eq!(separators[0].desc, "── MANAGEMENT ──");
        assert_eq!(separators[1].desc, "── MONITORING ──");
        assert_eq!(separators[2].desc, "── SETTINGS ──");
        // 14 verbs + 3 separators.
        assert_eq!(suggestions.len(), 17);
        // `msg` is in MANAGEMENT, so it lists with the other management verbs
        // rather than where its registry index would put it.
        let management: Vec<&str> = suggestions
            .iter()
            .skip(1)
            .take_while(|s| !s.is_separator())
            .map(|s| s.cmd.as_str())
            .collect();
        assert!(
            management.contains(&"/msg [agent] [message]"),
            "{management:?}"
        );
        // The `[id]` args hint is baked into the command text.
        assert!(suggestions.iter().any(|s| s.cmd == "/run [id]"));
        assert_eq!(
            suggestions
                .iter()
                .find(|s| s.cmd == "/goal")
                .unwrap()
                .subs
                .as_deref(),
            Some("add · list · show · status · delete")
        );
    }

    #[test]
    fn a_prefix_is_flat_with_no_separators() {
        let suggestions = resolve_suggestions("/ta");
        assert!(suggestions.iter().all(|s| !s.is_separator()));
        assert_eq!(suggestions.len(), 1);
        assert_eq!(suggestions[0].cmd, "/task");
    }

    #[test]
    fn an_exact_verb_prefix_appends_its_subcommands() {
        let suggestions = resolve_suggestions("/goal");
        assert_eq!(suggestions[0].cmd, "/goal");
        assert_eq!(suggestions[1].cmd, "/goal add");
        assert_eq!(suggestions[1].desc, "Manage goals: add");
        assert_eq!(suggestions.len(), 6);
    }

    #[test]
    fn inside_a_command_only_subcommands_are_listed() {
        let all = resolve_suggestions("/task ");
        assert_eq!(all.len(), 9);
        assert_eq!(all[0].cmd, "/task add");
        let filtered = resolve_suggestions("/task a");
        assert_eq!(
            filtered.iter().map(|s| s.cmd.as_str()).collect::<Vec<_>>(),
            ["/task add", "/task assign", "/task approve"]
        );
        assert!(resolve_suggestions("/run ").is_empty());
        assert!(resolve_suggestions("/nope ").is_empty());
    }

    #[test]
    fn non_slash_input_yields_no_suggestions() {
        assert!(resolve_suggestions("").is_empty());
        assert!(resolve_suggestions("task").is_empty());
    }

    // ── `/msg` rows (design-v2 §3.5.3) ────────────────────────────────────

    fn fleet() -> Vec<Agent> {
        use orch_domain::AgentStatus;
        vec![
            crate::backend::make_agent("agt_rev", "Code Reviewer", AgentStatus::Idle),
            crate::backend::make_agent("agt_al", "alice", AgentStatus::Idle),
            crate::backend::make_agent("agt_off", "say \"hi\"", AgentStatus::Disabled),
        ]
    }

    fn rows(input: &str, agents: &[Agent]) -> Vec<Suggestion> {
        let order: Vec<usize> = (0..agents.len()).collect();
        resolve_suggestions_in(
            input,
            &SuggestionCtx {
                agents,
                order: &order,
            },
        )
    }

    fn texts(rows: &[Suggestion]) -> Vec<String> {
        rows.iter().map(|r| r.cmd.clone()).collect()
    }

    #[test]
    fn the_empty_context_is_byte_identical_to_the_old_behaviour() {
        // The shim keeps every pinned vector above meaningful: with no agents
        // the only `/msg` rows are the verb row and the broadcast row.
        for input in [
            "/", "/ta", "/goal", "/task ", "/task a", "", "task", "/nope ",
        ] {
            assert_eq!(
                resolve_suggestions(input),
                resolve_suggestions_in(input, &SuggestionCtx::EMPTY),
                "{input:?}"
            );
        }
        assert_eq!(
            texts(&resolve_suggestions("/msg ")),
            ["/msg all"],
            "a backend with no agents still offers the keyword"
        );
        assert_eq!(
            resolve_suggestions("/msg ")[0].desc,
            "broadcast · 0 enabled agent(s)"
        );
    }

    #[test]
    fn the_verb_row_fills_and_the_agent_rows_follow_the_given_order() {
        let fleet = fleet();
        // `/ms` and `/msg` are still the verb row, which fills `/msg `.
        for input in ["/ms", "/msg"] {
            let rows = rows(input, &fleet);
            assert_eq!(texts(&rows), ["/msg [agent] [message]"], "{input:?}");
            assert_eq!(rows[0].insert.as_deref(), Some("/msg "));
        }
        // `/msg ` lists every agent in order, then the broadcast row.
        assert_eq!(
            texts(&rows("/msg ", &fleet)),
            [
                "/msg Code Reviewer",
                "/msg alice",
                "/msg say \"hi\"",
                "/msg all"
            ]
        );
    }

    #[test]
    fn an_agent_row_displays_the_name_and_inserts_the_quoted_form() {
        let fleet = fleet();
        let rows = rows("/msg say", &fleet);
        assert_eq!(rows[0].cmd, "/msg say \"hi\"", "the row reads naturally");
        assert_eq!(
            rows[0].insert.as_deref(),
            Some("/msg \"say \\\"hi\\\"\" "),
            "the inserted text is what parses back"
        );
        // Click identity is the id, not the display text.
        assert_eq!(rows[0].zone_key(), "agt_off");
        assert_eq!(rows[0].desc, "claude · disabled · agt_off");
    }

    #[test]
    fn a_name_with_an_args_hint_shape_is_not_mangled() {
        // `strip_args_hint` trims a trailing ` [...]`; `insert` bypasses it.
        use orch_domain::AgentStatus;
        let fleet = vec![crate::backend::make_agent(
            "agt_odd",
            "foo [bar]",
            AgentStatus::Idle,
        )];
        let rows = rows("/msg foo", &fleet);
        assert_eq!(rows[0].cmd, "/msg foo [bar]");
        assert_eq!(rows[0].insert.as_deref(), Some("/msg \"foo [bar]\" "));
    }

    #[test]
    fn the_broadcast_row_is_last_and_only_when_it_could_still_match() {
        let fleet = fleet();
        // `a` is a prefix of every id, so every agent lists — `all` last.
        let listed = rows("/msg a", &fleet);
        assert_eq!(listed.last().unwrap().cmd, "/msg all");
        assert_ne!(listed[0].cmd, "/msg all", "index 0 must be an agent row");
        // `al` is a prefix of both, and the agent still comes first.
        assert_eq!(texts(&rows("/msg al", &fleet)), ["/msg alice", "/msg all"]);
        // `alice` cannot become `all` any more, so the keyword drops out.
        assert_eq!(texts(&rows("/msg alice", &fleet)), ["/msg alice"]);
        // …and `all` matches only the keyword.
        assert_eq!(texts(&rows("/msg all", &fleet)), ["/msg all"]);
    }

    #[test]
    fn the_panel_closes_once_the_body_starts_and_on_malformed_input() {
        let fleet = fleet();
        assert!(rows("/msg alice ", &fleet).is_empty());
        assert!(rows("/msg alice hello", &fleet).is_empty());
        assert!(rows("/msg \"Code Reviewer\" hi", &fleet).is_empty());
        // Junk after a closing quote offers nothing rather than everything.
        assert!(rows("/msg \"alice\"x", &fleet).is_empty());
        // A partial quoted name still filters.
        assert_eq!(
            texts(&rows("/msg \"Code R", &fleet)),
            ["/msg Code Reviewer"]
        );
        // A complete quoted name keeps its row, so Tab adds the space.
        assert_eq!(texts(&rows("/msg \"alice\"", &fleet)), ["/msg alice"]);
    }

    #[test]
    fn identically_displayed_rows_are_marked_and_kept_distinct() {
        use orch_domain::AgentStatus;
        let fleet = vec![
            crate::backend::make_agent("agt_1", "dup", AgentStatus::Idle),
            crate::backend::make_agent("agt_2", "dup", AgentStatus::Idle),
        ];
        let rows = rows("/msg dup", &fleet);
        assert_eq!(texts(&rows), ["/msg !dup", "/msg !dup"]);
        assert_ne!(rows[0].zone_key(), rows[1].zone_key());
        assert!(rows[0].desc.ends_with("agt_1"));
        assert!(rows[1].desc.ends_with("agt_2"));
    }

    #[test]
    fn the_agent_rows_are_capped_with_an_inert_row_naming_the_remainder() {
        use orch_domain::AgentStatus;
        let fleet: Vec<Agent> = (0..41)
            .map(|i| {
                crate::backend::make_agent(
                    &format!("agt_{i:02}"),
                    &format!("agent-{i:02}"),
                    AgentStatus::Idle,
                )
            })
            .collect();
        let rows = rows("/msg agent", &fleet);
        assert_eq!(rows.len(), 41, "40 agents plus the overflow row");
        assert_eq!(rows[40].desc, "… 1 more — keep typing to filter");
        assert!(rows[40].is_separator(), "the overflow row must be inert");
    }

    #[test]
    fn a_wide_name_is_capped_in_display_columns() {
        use orch_domain::AgentStatus;
        let fleet = vec![crate::backend::make_agent(
            "agt_cjk",
            &"名".repeat(30),
            AgentStatus::Idle,
        )];
        let row = &rows("/msg 名", &fleet)[0];
        assert!(
            crate::text::text_display_width(&row.cmd) <= ROW_COLUMNS,
            "{:?} is {} columns",
            row.cmd,
            crate::text::text_display_width(&row.cmd)
        );
        // The insert is never capped — it has to parse back.
        assert_eq!(
            row.insert.as_deref(),
            Some(format!("/msg {} ", "名".repeat(30)).as_str())
        );
    }

    #[test]
    fn only_msg_rows_carry_an_insert() {
        // The fill-vs-execute rule, over the whole registry: a row with
        // `insert` fills, and giving it to the other argument-taking verbs
        // would change what Enter has always done on their rows (`/run` with
        // no id runs the selection). `subs` keeps driving the legacy fill.
        for spec in COMMAND_REGISTRY.iter() {
            let row = verb_row(spec);
            assert_eq!(
                row.insert.is_some(),
                spec.verb == MSG_VERB,
                "/{} must {} fill",
                spec.verb,
                if spec.verb == MSG_VERB { "" } else { "not" }
            );
            assert_eq!(row.insert.is_some(), spec.insert_trailing_space);
        }
    }

    #[test]
    fn history_push_dedupes_consecutively_and_caps_at_100() {
        let mut history = CommandHistory::new();
        history.push("");
        assert_eq!(history.len(), 0);
        history.push("/a");
        history.push("/a");
        assert_eq!(history.len(), 1);
        history.push("/b");
        history.push("/a");
        assert_eq!(history.len(), 3);

        let mut history = CommandHistory::new();
        for i in 0..150 {
            history.push(&format!("/cmd{i}"));
        }
        assert_eq!(history.len(), 100);
        assert_eq!(history.entries()[0], "/cmd50");
    }

    #[test]
    fn history_prev_next_and_reset() {
        let mut history = CommandHistory::new();
        assert_eq!(history.prev(), None);
        history.push("/one");
        history.push("/two");
        assert_eq!(history.prev().as_deref(), Some("/two"));
        assert_eq!(history.prev().as_deref(), Some("/one"));
        assert_eq!(history.prev().as_deref(), Some("/one"));
        assert_eq!(history.next().as_deref(), Some("/two"));
        assert_eq!(history.next(), None);
        assert_eq!(history.next(), None);
        history.reset();
        assert_eq!(history.prev().as_deref(), Some("/two"));
    }
}
