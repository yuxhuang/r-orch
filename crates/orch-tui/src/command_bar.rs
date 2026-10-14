//! Command registry, tab completion, suggestions and history
//! (`src/tui/commandBar.ts`).
//!
//! Registry **insertion order** is contract: it drives suggestion order and the
//! first-match semantics of [`resolve_completion`] (PARITY.md §12.4).

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
}

/// `COMMAND_REGISTRY`, in insertion order.
pub const COMMAND_REGISTRY: [CommandSpec; 12] = [
    CommandSpec {
        verb: "task",
        sub: &[
            "add", "list", "show", "cancel", "retry", "assign", "approve", "reject", "delete",
        ],
        args: None,
        help: "Manage tasks",
        category: CAT_MANAGE,
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
    },
    CommandSpec {
        verb: "team",
        sub: &["create", "list", "join", "leave", "disband", "set-lead"],
        args: None,
        help: "Manage teams",
        category: CAT_MANAGE,
    },
    CommandSpec {
        verb: "goal",
        sub: &["add", "list", "show", "status", "delete"],
        args: None,
        help: "Manage goals",
        category: CAT_MANAGE,
    },
    CommandSpec {
        verb: "run",
        sub: &[],
        args: Some("[id]"),
        help: "Run task (or selected)",
        category: CAT_MONITOR,
    },
    CommandSpec {
        verb: "run-all",
        sub: &[],
        args: None,
        help: "Run all todo tasks",
        category: CAT_MONITOR,
    },
    CommandSpec {
        verb: "watch",
        sub: &[],
        args: None,
        help: "Start watch mode (auto-dispatch)",
        category: CAT_MONITOR,
    },
    CommandSpec {
        verb: "pause",
        sub: &[],
        args: None,
        help: "Pause watch mode",
        category: CAT_MONITOR,
    },
    CommandSpec {
        verb: "status",
        sub: &[],
        args: None,
        help: "Show orchestrator status",
        category: CAT_MONITOR,
    },
    CommandSpec {
        verb: "config",
        sub: &CONFIG_SETTINGS,
        args: None,
        help: "TUI settings",
        category: CAT_SETTINGS,
    },
    CommandSpec {
        verb: "help",
        sub: &[],
        args: None,
        help: "List all commands",
        category: CAT_SETTINGS,
    },
    CommandSpec {
        verb: "quit",
        sub: &[],
        args: None,
        help: "Exit the TUI",
        category: CAT_SETTINGS,
    },
];

pub fn spec_for(verb: &str) -> Option<&'static CommandSpec> {
    COMMAND_REGISTRY.iter().find(|s| s.verb == verb)
}

/// One suggestion row. A separator row has an empty `cmd`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Suggestion {
    pub cmd: String,
    pub desc: String,
    pub subs: Option<String>,
}

impl Suggestion {
    pub fn is_separator(&self) -> bool {
        self.cmd.is_empty()
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

/// `resolveCompletion(input)` — the ghost **suffix** only.
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

/// `resolveSuggestions(input)`.
pub fn resolve_suggestions(input: &str) -> Vec<Suggestion> {
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
                        subs: None,
                    });
                    for spec in COMMAND_REGISTRY.iter().filter(|s| s.category == cat) {
                        results.push(Suggestion {
                            cmd: format!("/{}{}", spec.verb, args_hint(spec)),
                            desc: spec.help.to_string(),
                            subs: subs_hint(spec),
                        });
                    }
                }
                return results;
            }

            for spec in COMMAND_REGISTRY.iter() {
                if spec.verb.starts_with(&prefix) {
                    results.push(Suggestion {
                        cmd: format!("/{}{}", spec.verb, args_hint(spec)),
                        desc: spec.help.to_string(),
                        subs: subs_hint(spec),
                    });
                    if spec.verb == prefix && !spec.sub.is_empty() {
                        for sub in spec.sub {
                            results.push(Suggestion {
                                cmd: format!("/{} {}", spec.verb, sub),
                                desc: format!("{}: {}", spec.help, sub),
                                subs: None,
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
                    subs: None,
                })
                .collect()
        }
    }
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
    /// Named after the TS `CommandHistory.next()` it ports; it is not an
    /// iterator.
    #[allow(clippy::should_implement_trait)]
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
            [
                "task", "agent", "team", "goal", "run", "run-all", "watch", "pause", "status",
                "config", "help", "quit"
            ]
        );
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
        // PARITY.md §12.4.
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
        // 12 verbs + 3 separators.
        assert_eq!(suggestions.len(), 15);
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
