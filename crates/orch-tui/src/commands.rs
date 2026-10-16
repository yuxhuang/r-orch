//! `executeCommand` — the command-bar verb surface (docs/parity/tui.md §4.6).
//!
//! The verb and subcommand set is contract (PARITY.md §12.4), as are the
//! confirmation and refusal strings, which the TS tests assert as substrings.

use orch_domain::{AgentStatus, GOAL_STATUSES, GoalStatus, TUI_PALETTE_NAMES, TaskStatus};

use crate::app::{App, BroadcastArm, parse_palette};
use crate::backend::MessageTarget;
use crate::backend::TuiBackend;
use crate::clock::Clock;
use crate::command_bar::{COMMAND_REGISTRY, CONFIG_SETTINGS, is_config_setting};
use crate::format::cap_line;
use crate::messages::MsgType;
use crate::msg::{Recipient, parse_recipient, resolve_recipient};
use crate::state::is_runnable;
use crate::theme::Token;
use crate::wizard::configs;

/// `MessageService` cuts a body at this many UTF-16 units, silently
/// (PARITY §14.14). The TUI only reports it.
const MESSAGE_BODY_UNITS: usize = 4000;

const NO_TASK_SELECTED: &str = "No task selected";
const NO_AGENT_SELECTED: &str = "No agent selected";
const NO_TASK_OR_ID: &str = "No task selected or id given";
const NO_AGENT_OR_ID: &str = "No agent selected or id given";
const NO_GOAL_OR_ID: &str = "No goal selected or id given";

impl<B: TuiBackend, C: Clock> App<B, C> {
    fn info(&mut self, text: &str) {
        self.emit(text, Token::Cyan, MsgType::System);
    }

    fn pending(&mut self, text: &str) {
        self.emit(text, Token::Amber, MsgType::System);
    }

    fn ok(&mut self, text: &str) {
        self.emit(text, Token::Green, MsgType::System);
    }

    fn dim(&mut self, text: &str) {
        self.emit(text, Token::Dim, MsgType::System);
    }

    /// Resolve a task argument to an id: the given id when it matches, else the
    /// current selection.
    fn resolve_task(&self, arg: Option<&str>) -> Option<String> {
        match arg {
            Some(id) if self.state.task_by_id(id).is_some() => Some(id.to_string()),
            Some(_) | None => self.state.selected_task().map(|t| t.id.clone()),
        }
    }

    /// `/agent` resolves **by id or name**.
    fn resolve_agent(&self, arg: Option<&str>) -> Option<String> {
        match arg {
            Some(key) => self
                .state
                .agents
                .iter()
                .find(|a| a.id == key || a.name == key)
                .map(|a| a.id.clone())
                .or_else(|| self.state.selected_agent().map(|a| a.id.clone())),
            None => self.state.selected_agent().map(|a| a.id.clone()),
        }
    }

    fn resolve_goal(&self, arg: Option<&str>) -> Option<String> {
        match arg {
            Some(id) if self.state.goal_by_id(id).is_some() => Some(id.to_string()),
            Some(_) | None => self.state.selected_goal().map(|g| g.id.clone()),
        }
    }

    pub fn execute_command(&mut self, raw: &str) {
        // `replace(/^\//, '')` — exactly one leading slash, so `//task` keeps
        // its second one and lands in the unknown-verb branch.
        let trimmed = raw.trim();
        let stripped = trimmed.strip_prefix('/').unwrap_or(trimmed);
        let parts: Vec<String> = stripped.split_whitespace().map(str::to_string).collect();
        let Some(verb) = parts.first().map(|v| v.to_lowercase()) else {
            return;
        };
        let arg = |index: usize| parts.get(index).map(String::as_str);

        match verb.as_str() {
            // ── Legacy bare shortcuts ──────────────────────────────────────
            "cancel" => match self.state.selected_task().map(|t| t.id.clone()) {
                Some(id) => self.cancel_task_by_id(&id),
                None => self.refuse(NO_TASK_SELECTED),
            },
            "retry" => match self.state.selected_task().cloned() {
                Some(task) => self.retry_task_by_id(&task.id, &task.title),
                None => self.refuse(NO_TASK_SELECTED),
            },
            "assign" => {
                let Some(task) = self.state.selected_task().cloned() else {
                    self.refuse(NO_TASK_SELECTED);
                    return;
                };
                match arg(1) {
                    Some(agent) => {
                        let agent = agent.to_string();
                        self.assign(&task.id, &task.title, &agent);
                    }
                    None => self.refuse("Usage: assign <agent>"),
                }
            }
            "disable" => match self.state.selected_agent().cloned() {
                Some(agent) => self.disable(&agent.id, &agent.name),
                None => self.refuse(NO_AGENT_SELECTED),
            },
            "enable" => match self.state.selected_agent().cloned() {
                Some(agent) => self.enable(&agent.id, &agent.name),
                None => self.refuse(NO_AGENT_SELECTED),
            },

            "task" => self.cmd_task(&parts),
            "agent" => self.cmd_agent(&parts),
            "team" => self.cmd_team(&parts),
            "goal" => self.cmd_goal(&parts),

            "run" => {
                let id = match arg(1) {
                    Some(id) => Some(id.to_string()),
                    None => self.state.selected_task().map(|t| t.id.clone()),
                };
                let Some(id) = id else {
                    self.refuse(NO_TASK_OR_ID);
                    return;
                };
                if let Some(task) = self.state.task_by_id(&id).cloned()
                    && !is_runnable(task.status)
                {
                    let text = format!("Cannot run — status is {}", task.status);
                    self.refuse(&text);
                    return;
                }
                self.pending(&format!("Running {id}..."));
                match self.backend.run_task(&id) {
                    Ok(()) => {
                        self.ok(&format!("✓ Dispatched {id}"));
                        self.refresh_all(false);
                    }
                    Err(err) => self.fail(&err),
                }
            }
            "run-all" => {
                self.pending("Running all todo tasks...");
                match self.backend.run_all() {
                    Ok(()) => {
                        self.ok("✓ Dispatched all todo tasks");
                        self.refresh_all(false);
                    }
                    Err(err) => self.fail(&err),
                }
            }
            "watch" => {
                if self.state.watch_active {
                    self.refuse("Watch mode already active");
                    return;
                }
                self.pending("Starting watch mode...");
                match self.backend.start_watch() {
                    Ok(()) => {
                        self.state.watch_active = true;
                        self.ok("✓ Watch mode started");
                    }
                    Err(err) => self.fail(&err),
                }
            }
            "pause" => {
                if !self.state.watch_active {
                    self.refuse("Watch mode not active");
                    return;
                }
                self.pending("Pausing watch mode...");
                match self.backend.stop_watch() {
                    Ok(()) => {
                        self.state.watch_active = false;
                        self.ok("✓ Watch mode paused");
                    }
                    Err(err) => self.fail(&err),
                }
            }
            "config" => self.cmd_config(&parts),
            "status" => {
                let running = self
                    .state
                    .tasks
                    .iter()
                    .filter(|t| t.status == TaskStatus::InProgress)
                    .count();
                let text = format!(
                    "{}  {running} running  {} tasks  {} agents",
                    self.state.mode_label(),
                    self.state.tasks.len(),
                    self.state.agents.len()
                );
                self.info(&text);
            }
            "help" => {
                for spec in COMMAND_REGISTRY.iter() {
                    let subs = if !spec.sub.is_empty() {
                        format!(" {}", spec.sub.join("|"))
                    } else if let Some(args) = spec.args {
                        format!(" {args}")
                    } else {
                        String::new()
                    };
                    let text = format!("  /{}{subs}  — {}", spec.verb, spec.help);
                    self.emit(&text, Token::Silver, MsgType::System);
                }
            }
            // A second route to the capture toggle, because `F2` is a weak
            // sole escape hatch: on many laptops it is brightness or media
            // without `Fn`, and some terminal menus claim it. This one is
            // typable and discoverable — it appears in the `/` suggestion
            // list, which is the TUI's own discovery surface (design §3.5).
            // Argument matching is lowercased, so `/mouse OFF` behaves like
            // `ORCH_TUI_MOUSE=OFF` — the feature's two switches should not
            // disagree about case.
            "mouse" => match arg(1).map(str::to_ascii_lowercase).as_deref() {
                Some("on") => self.set_mouse_enabled(true),
                Some("off") => self.set_mouse_enabled(false),
                Some("status") | None => {
                    let text = if self.state.mouse.enabled {
                        "Mouse capture is on — /mouse off to release the pointer"
                    } else {
                        "Mouse capture is off — /mouse on, or F2"
                    };
                    self.emit(text, Token::Silver, MsgType::System);
                }
                Some(other) => {
                    let text = format!("Usage: mouse on|off|status (got {other})");
                    self.refuse(&text);
                }
            },
            // The one verb whose argument can contain a space, so it takes the
            // **raw tail** rather than `parts`: `split_whitespace` is left
            // exactly as it is for every existing verb, whose parsing is L1
            // and position-sensitive (design-v2 §3.2, R1).
            "msg" => {
                let tail = stripped
                    .split_once(char::is_whitespace)
                    .map(|(_, tail)| tail)
                    .unwrap_or("")
                    .to_string();
                self.cmd_msg(&tail);
            }
            "quit" => self.state.should_exit = true,
            other => {
                let text = format!("Unknown: {other}. Type /help for commands");
                self.refuse(&text);
            }
        }
    }

    // ── /msg ───────────────────────────────────────────────────────────────

    /// `/msg <agent|all> <message>` (design-v2 §3.2–§3.8).
    ///
    /// Every exit that is not a send sets [`App::retain_command_bar`], so the
    /// bar stays open with its text: a refusal that asks for an id, or a
    /// broadcast waiting for its confirming Enter, is useless if the text it
    /// is about has been cleared.
    fn cmd_msg(&mut self, tail: &str) {
        // Every run starts clean, so a success never leaves the previous
        // refusal on screen.
        self.state.command_bar.retained_refusal.clear();
        if !self.backend.can_send_message() {
            // Retained like every other refusal: the typed body is the one
            // thing the user cannot get back.
            self.refuse_retaining("Messaging not available");
            return;
        }
        let token = match parse_recipient(tail) {
            Ok(token) => token,
            Err(err) => return self.refuse_retaining(err.message()),
        };
        if token.rest.trim().is_empty() {
            // Named, not a bare usage line: `m` chose this recipient.
            let text = format!("Missing message for \"{}\"", token.text);
            return self.refuse_retaining(&text);
        }
        let body = token.rest.clone();
        let recipient = match resolve_recipient(&self.state.agents, &token, &body) {
            Ok(recipient) => recipient,
            Err(err) => {
                let lines = err.lines();
                for line in &lines {
                    self.refuse(line);
                }
                self.retain(lines);
                return;
            }
        };

        let (target, name, disabled) = match recipient {
            Recipient::Broadcast { enabled } => {
                // The one irreversible, fan-out action in the TUI: agents are
                // LLMs that act on what they read. The confirmation is the
                // Enter that immediately follows the arming one — same text
                // *and* same edit generation, so retyping the identical
                // command, recalling it from history or leaving the bar and
                // coming back all have to arm again (gate S2).
                let armed = self.armed_broadcast.as_ref().is_some_and(|arm| {
                    arm.tail == tail && arm.generation == self.submitted_generation
                });
                if !armed {
                    self.armed_broadcast = Some(BroadcastArm {
                        tail: tail.to_string(),
                        generation: self.submitted_generation,
                    });
                    let text = format!("Broadcast to {enabled} agents — press Enter again to send");
                    return self.refuse_retaining(&text);
                }
                self.armed_broadcast = None;
                (MessageTarget::Broadcast, None, false)
            }
            Recipient::Agent { id, name, disabled } => {
                (MessageTarget::Agent(id), Some(name), disabled)
            }
        };

        let whom = name.clone().unwrap_or_else(|| "all agents".to_owned());
        self.pending(&format!("Sending to {whom}..."));
        let ids = match self.backend.send_message(target, &body) {
            Ok(ids) => ids,
            // Retained like a refusal: the body may be a pasted stack trace,
            // and a transient store error is not a reason to make the user
            // find it again (gate code F5).
            Err(err) => {
                self.fail(&err);
                self.retain(vec![format!("Failed: {err}")]);
                return;
            }
        };

        // `Queued`, not `sent`: the write is immediate but delivery happens on
        // the recipient's next dispatch, and an idle agent never drains.
        // Observer mode *is* dispatching — in another process — so the case
        // worth warning about is neither watch nor observer.
        let hint = if self.state.watch_active || self.state.observer_mode {
            String::new()
        } else {
            " — nothing is dispatching right now; /watch to start".to_string()
        };
        // The seam hands back the ids, which is all these two lines need: the
        // first id for a direct send, the count for a broadcast.
        let text = match &name {
            Some(name) => {
                let id = ids.first().map(String::as_str).unwrap_or("");
                format!("✓ Queued for {name} ({id}) — delivered on its next run{hint}")
            }
            None => format!(
                "✓ Queued for {} agents — delivered on their next run{hint}",
                ids.len()
            ),
        };
        self.ok(&text);
        if disabled && let Some(name) = &name {
            let text = format!("{name} is disabled — the message waits in its inbox");
            self.pending(&text);
        }
        // The engine cuts at 4000 UTF-16 units and says nothing; the TUI makes
        // that visible without pre-truncating — `truncate_utf16` stays the one
        // place the cut happens.
        if body.chars().map(char::len_utf16).sum::<usize>() > MESSAGE_BODY_UNITS {
            let text = format!("Body truncated to {MESSAGE_BODY_UNITS} characters");
            self.pending(&text);
        }
        // No `refresh_all`: nothing in `AppState` derives from messages.
    }

    /// A `/msg` refusal: the message, and the bar kept open with its text.
    fn refuse_retaining(&mut self, text: &str) {
        self.refuse(text);
        self.retain(vec![text.to_owned()]);
    }

    /// Keep the bar open, and keep `lines` on screen above it — the feed is
    /// not drawn while the bar is open.
    fn retain(&mut self, lines: Vec<String>) {
        self.retain_command_bar = true;
        self.state.command_bar.retained_refusal = lines;
    }

    // ── /task ──────────────────────────────────────────────────────────────

    fn cmd_task(&mut self, parts: &[String]) {
        let sub = parts.get(1).map(|s| s.to_lowercase());
        match sub.as_deref() {
            Some("add") => {
                let title = parts[2..].join(" ");
                if title.is_empty() {
                    self.open_task_wizard();
                    return;
                }
                if !self.backend.can_create_task() {
                    self.refuse("Create not available");
                    return;
                }
                self.pending(&format!("Creating \"{title}\"..."));
                match self
                    .backend
                    .create_task(&title, crate::backend::CreateTaskOpts::default())
                {
                    Ok(task) => {
                        self.ok(&format!("✓ Created \"{}\" ({})", task.title, task.id));
                        self.refresh_all(false);
                    }
                    Err(err) => self.fail(&err),
                }
            }
            Some("list") => {
                let order = self.state.sorted_tasks();
                if order.is_empty() {
                    self.dim("No tasks");
                    return;
                }
                let lines: Vec<String> = order
                    .iter()
                    .map(|i| {
                        let task = &self.state.tasks[*i];
                        format!("  {}  {:<11} {}", task.id, task.status.as_str(), task.title)
                    })
                    .collect();
                for line in lines {
                    self.info(&line);
                }
            }
            Some("show") => {
                let Some(id) = self.resolve_task(parts.get(2).map(String::as_str)) else {
                    self.refuse(NO_TASK_OR_ID);
                    return;
                };
                let Some(task) = self.state.task_by_id(&id).cloned() else {
                    self.refuse(NO_TASK_OR_ID);
                    return;
                };
                self.info(&format!(
                    "{}  {}  P{}  \"{}\"",
                    task.id, task.status, task.priority, task.title
                ));
                if let Some(assignee) = &task.assignee {
                    let text = format!("  agent: {assignee}");
                    self.dim(&text);
                }
                if !task.description.is_empty() {
                    let head: String = task.description.chars().take(100).collect();
                    let text = format!("  {head}");
                    self.dim(&text);
                }
            }
            Some("cancel") => match self.resolve_task(parts.get(2).map(String::as_str)) {
                Some(id) => self.cancel_task_by_id(&id),
                None => self.refuse(NO_TASK_OR_ID),
            },
            Some("retry") => match self.resolve_task(parts.get(2).map(String::as_str)) {
                Some(id) => {
                    let title = self
                        .state
                        .task_by_id(&id)
                        .map(|t| t.title.clone())
                        .unwrap_or_default();
                    self.retry_task_by_id(&id, &title);
                }
                None => self.refuse(NO_TASK_OR_ID),
            },
            Some("assign") => {
                // `/task assign [id] <agent>`: when parts[2] is a known task id
                // the agent is parts[3], otherwise parts[2].
                let by_id = parts
                    .get(2)
                    .filter(|key| self.state.task_by_id(key).is_some())
                    .cloned();
                let task_id = match &by_id {
                    Some(id) => Some(id.clone()),
                    None => self.state.selected_task().map(|t| t.id.clone()),
                };
                let agent_arg = if by_id.is_some() {
                    parts.get(3).cloned()
                } else {
                    parts.get(2).cloned()
                };
                let Some(task_id) = task_id else {
                    self.refuse(NO_TASK_OR_ID);
                    return;
                };
                let Some(agent_arg) = agent_arg else {
                    self.refuse("Usage: /task assign [id] <agent>");
                    return;
                };
                let title = self
                    .state
                    .task_by_id(&task_id)
                    .map(|t| t.title.clone())
                    .unwrap_or_default();
                self.assign(&task_id, &title, &agent_arg);
            }
            Some("approve") => match self.resolve_task(parts.get(2).map(String::as_str)) {
                Some(id) => self.approve_task_by_id(&id),
                None => self.refuse(NO_TASK_OR_ID),
            },
            Some("reject") => {
                let by_id = parts
                    .get(2)
                    .filter(|key| self.state.task_by_id(key).is_some())
                    .cloned();
                let Some(id) = (match &by_id {
                    Some(id) => Some(id.clone()),
                    None => self.state.selected_task().map(|t| t.id.clone()),
                }) else {
                    self.refuse(NO_TASK_OR_ID);
                    return;
                };
                let skip = if by_id.is_some() { 3 } else { 2 };
                let feedback = parts[skip.min(parts.len())..].join(" ");
                let feedback = if feedback.trim().is_empty() {
                    None
                } else {
                    Some(feedback)
                };
                self.reject_task_by_id(&id, feedback.as_deref());
            }
            Some("delete") => match self.resolve_task(parts.get(2).map(String::as_str)) {
                Some(id) => self.delete_task_by_id(&id),
                None => self.refuse(NO_TASK_OR_ID),
            },
            _ => {
                self.refuse("Usage: /task add|list|show|cancel|retry|assign|approve|reject|delete")
            }
        }
    }

    // ── /agent ─────────────────────────────────────────────────────────────

    fn cmd_agent(&mut self, parts: &[String]) {
        let sub = parts.get(1).map(|s| s.to_lowercase());
        match sub.as_deref() {
            Some("add") => {
                let Some(name) = parts.get(2).cloned() else {
                    self.open_agent_wizard();
                    return;
                };
                if !self.backend.can_add_agent() {
                    self.refuse("Agent creation not available");
                    return;
                }
                let adapter = parts
                    .get(3)
                    .cloned()
                    .unwrap_or_else(|| self.state.default_adapter.clone());
                let command = if adapter == "shell" {
                    parts[4.min(parts.len())..].join(" ").trim().to_string()
                } else {
                    String::new()
                };
                if adapter == "shell" && command.is_empty() {
                    // A shell agent with no command opens the wizard pre-filled
                    // with the name and adapter instead of creating anything.
                    let steps = configs::agent_wizard_steps(
                        &self.state.agents,
                        &self.state.teams,
                        &self.model_catalog,
                        &self.state.default_adapter,
                    )
                    .into_iter()
                    .map(|step| match step.id.as_str() {
                        "name" => step.default_value(&name),
                        "adapter" => step.default_value("shell"),
                        _ => step,
                    })
                    .collect();
                    self.open_wizard(crate::wizard::Wizard::new(
                        crate::wizard::WizardKind::Agent,
                        "NEW AGENT",
                        steps,
                    ));
                    return;
                }
                self.pending(&format!("Creating agent \"{name}\"..."));
                let opts = if adapter == "shell" {
                    crate::backend::AgentOpts {
                        command: Some(command),
                        approval_policy: Some("auto".into()),
                        ..Default::default()
                    }
                } else {
                    crate::backend::AgentOpts::default()
                };
                match self.backend.add_agent(&name, &adapter, opts) {
                    Ok(agent) => {
                        let hint = match (&agent.adapter[..], agent.config.command.as_deref()) {
                            ("shell", Some(command)) if !command.is_empty() => {
                                format!(" · {}", cap_line(command, 60))
                            }
                            _ => String::new(),
                        };
                        self.ok(&format!(
                            "✓ Created agent \"{}\" ({}, {}){hint}",
                            agent.name, agent.id, agent.adapter
                        ));
                        self.refresh_all(false);
                    }
                    Err(err) => self.fail(&err),
                }
            }
            Some("list") => {
                let order = self.state.sorted_agents();
                if order.is_empty() {
                    self.dim("No agents");
                    return;
                }
                let lines: Vec<String> = order
                    .iter()
                    .map(|i| {
                        let agent = &self.state.agents[*i];
                        format!(
                            "  {}  {:<8} {} ({})",
                            agent.id,
                            agent.status.as_str(),
                            agent.name,
                            agent.adapter
                        )
                    })
                    .collect();
                for line in lines {
                    self.info(&line);
                }
            }
            Some("disable") => match self.resolve_agent(parts.get(2).map(String::as_str)) {
                Some(id) => {
                    let name = self.state.agent_name(&id);
                    self.disable(&id, &name);
                }
                None => self.refuse(NO_AGENT_OR_ID),
            },
            Some("enable") => match self.resolve_agent(parts.get(2).map(String::as_str)) {
                Some(id) => {
                    let name = self.state.agent_name(&id);
                    self.enable(&id, &name);
                }
                None => self.refuse(NO_AGENT_OR_ID),
            },
            Some("delete") | Some("remove") => {
                let Some(id) = self.resolve_agent(parts.get(2).map(String::as_str)) else {
                    self.refuse(NO_AGENT_OR_ID);
                    return;
                };
                let Some(agent) = self.state.agent_by_id(&id).cloned() else {
                    self.refuse(NO_AGENT_OR_ID);
                    return;
                };
                if agent.status == AgentStatus::Running {
                    self.refuse("Cannot delete — agent is running");
                    return;
                }
                self.delete_agent_by_id(&id);
            }
            Some("autonomous") | Some("auto") => {
                match self.resolve_agent(parts.get(2).map(String::as_str)) {
                    Some(id) => self.toggle_autonomous_by_id(&id),
                    None => self.refuse(NO_AGENT_OR_ID),
                }
            }
            Some("shop") => self.open_shop_wizard(),
            _ => self.refuse("Usage: /agent add|list|disable|enable|delete|autonomous|shop"),
        }
    }

    // ── /team ──────────────────────────────────────────────────────────────

    fn cmd_team(&mut self, parts: &[String]) {
        let sub = parts.get(1).map(|s| s.to_lowercase());
        match sub.as_deref() {
            Some("create") | Some("add") => self.open_team_wizard(),
            Some("list") => {
                if self.state.teams.is_empty() {
                    self.dim("No teams");
                    return;
                }
                let lines: Vec<String> = self
                    .state
                    .teams
                    .iter()
                    .map(|team| {
                        format!(
                            "  {}  {:<8} {} ({} members)",
                            team.id,
                            team.status.as_str(),
                            team.name,
                            team.members.len()
                        )
                    })
                    .collect();
                for line in lines {
                    self.info(&line);
                }
            }
            Some("join") => {
                let team_arg = parts.get(2).cloned();
                let agent_arg = parts
                    .get(3)
                    .cloned()
                    .or_else(|| self.state.selected_agent().map(|a| a.id.clone()));
                let (Some(team_id), Some(agent_id)) = (team_arg, agent_arg) else {
                    self.refuse("Usage: /team join <teamId> [agentId]");
                    return;
                };
                self.pending(&format!("Joining team {team_id}..."));
                match self.backend.join_team(&team_id, &agent_id) {
                    Ok(team) => {
                        self.ok(&format!("✓ Agent joined team \"{}\"", team.name));
                        self.refresh_all(true);
                    }
                    Err(err) => self.fail(&err),
                }
            }
            Some("leave") => {
                let team_arg = parts.get(2).cloned();
                let agent_arg = parts
                    .get(3)
                    .cloned()
                    .or_else(|| self.state.selected_agent().map(|a| a.id.clone()));
                let (Some(team_id), Some(agent_id)) = (team_arg, agent_arg) else {
                    self.refuse("Usage: /team leave <teamId> [agentId]");
                    return;
                };
                self.pending(&format!("Leaving team {team_id}..."));
                match self.backend.leave_team(&team_id, &agent_id) {
                    Ok(team) => {
                        self.ok(&format!("✓ Agent left team \"{}\"", team.name));
                        self.refresh_all(true);
                    }
                    Err(err) => self.fail(&err),
                }
            }
            Some("disband") => {
                let Some(team_id) = parts.get(2).cloned() else {
                    self.refuse("Usage: /team disband <teamId>");
                    return;
                };
                self.pending(&format!("Disbanding team {team_id}..."));
                match self.backend.disband_team(&team_id) {
                    Ok(()) => {
                        self.ok("✓ Team disbanded");
                        self.refresh_all(true);
                    }
                    Err(err) => self.fail(&err),
                }
            }
            Some("set-lead") => {
                let (Some(team_id), Some(agent_id)) =
                    (parts.get(2).cloned(), parts.get(3).cloned())
                else {
                    self.refuse("Usage: /team set-lead <teamId> <agentId>");
                    return;
                };
                self.pending(&format!("Setting lead for team {team_id}..."));
                match self.backend.set_team_lead(&team_id, &agent_id) {
                    Ok(team) => {
                        self.ok(&format!("✓ New lead for team \"{}\"", team.name));
                        self.refresh_all(true);
                    }
                    Err(err) => self.fail(&err),
                }
            }
            _ => self.refuse("Usage: /team create|list|join|leave|disband|set-lead"),
        }
    }

    // ── /goal ──────────────────────────────────────────────────────────────

    fn cmd_goal(&mut self, parts: &[String]) {
        let sub = parts.get(1).map(|s| s.to_lowercase());
        match sub.as_deref() {
            Some("add") | Some("create") => {
                let title = parts[2.min(parts.len())..].join(" ").trim().to_string();
                if title.is_empty() {
                    self.open_goal_wizard();
                    return;
                }
                if !self.backend.can_create_goal() {
                    self.refuse("Goal creation not available");
                    return;
                }
                self.pending(&format!("Creating goal \"{title}\"..."));
                let input = orch_domain::CreateGoalInput {
                    title: title.clone(),
                    description: None,
                    assignee: None,
                };
                match self.backend.create_goal(input) {
                    Ok(goal) => {
                        self.ok(&format!("✓ Created goal \"{}\" ({})", goal.title, goal.id));
                        self.refresh_all(false);
                    }
                    Err(err) => self.fail(&err),
                }
            }
            Some("list") => {
                let order = self.state.sorted_goals();
                if order.is_empty() {
                    self.dim("No goals");
                    return;
                }
                let lines: Vec<String> = order
                    .iter()
                    .map(|i| {
                        let goal = &self.state.goals[*i];
                        format!("  {}  {:<8} {}", goal.id, goal.status.as_str(), goal.title)
                    })
                    .collect();
                for line in lines {
                    self.info(&line);
                }
            }
            Some("show") => {
                let Some(id) = self.resolve_goal(parts.get(2).map(String::as_str)) else {
                    self.refuse(NO_GOAL_OR_ID);
                    return;
                };
                let Some(goal) = self.state.goal_by_id(&id).cloned() else {
                    self.refuse(NO_GOAL_OR_ID);
                    return;
                };
                self.info(&format!("{}  {}  \"{}\"", goal.id, goal.status, goal.title));
                if !goal.description.is_empty() {
                    let head: String = goal.description.chars().take(100).collect();
                    let text = format!("  {head}");
                    self.dim(&text);
                }
            }
            Some("status") => {
                let by_id = parts
                    .get(2)
                    .filter(|key| self.state.goal_by_id(key).is_some())
                    .cloned();
                let Some(id) = (match &by_id {
                    Some(id) => Some(id.clone()),
                    None => self.state.selected_goal().map(|g| g.id.clone()),
                }) else {
                    self.refuse(NO_GOAL_OR_ID);
                    return;
                };
                let status_arg = if by_id.is_some() {
                    parts.get(3)
                } else {
                    parts.get(2)
                };
                let status = status_arg.and_then(|value| {
                    GOAL_STATUSES
                        .into_iter()
                        .find(|s| s.as_str() == value.as_str())
                });
                let Some(status) = status else {
                    self.refuse("Usage: /goal status [id] <active|paused|achieved|abandoned>");
                    return;
                };
                self.pending(&format!("Updating goal status to {}...", status.as_str()));
                self.set_goal_status(&id, status, false);
            }
            Some("delete") => match self.resolve_goal(parts.get(2).map(String::as_str)) {
                Some(id) => self.delete_goal_by_id(&id),
                None => self.refuse(NO_GOAL_OR_ID),
            },
            _ => self.refuse("Usage: /goal add|list|show|status|delete"),
        }
    }

    // ── /config ────────────────────────────────────────────────────────────

    fn cmd_config(&mut self, parts: &[String]) {
        let sub = parts.get(1).map(|s| s.to_lowercase());
        match (sub.as_deref(), parts.get(2)) {
            // `config palette <name>` applies directly — no wizard.
            (Some("palette"), Some(requested)) => {
                let requested = requested.to_lowercase();
                match parse_palette(&requested) {
                    Some(palette) => {
                        self.state.palette = crate::theme::Palette::new(palette);
                        let _ = self.backend.save_palette(palette);
                        let text = format!("Palette: {requested}");
                        self.pending(&text);
                    }
                    None => {
                        let available: Vec<&str> =
                            TUI_PALETTE_NAMES.iter().map(|p| p.as_str()).collect();
                        let text = format!(
                            "Unknown palette: {requested}. Available: {}",
                            available.join(", ")
                        );
                        self.refuse(&text);
                    }
                }
            }
            (Some(setting), _) if is_config_setting(setting) => {
                self.open_config_wizard(setting);
            }
            (None, _) => {
                let text = format!("Choose a setting: {}", CONFIG_SETTINGS.join(", "));
                self.info(&text);
            }
            (Some(unknown), _) => {
                let text = format!(
                    "Unknown setting: {unknown}. Available: {}",
                    CONFIG_SETTINGS.join(", ")
                );
                self.refuse(&text);
            }
        }
    }

    // ── Shared action helpers ──────────────────────────────────────────────

    fn retry_task_by_id(&mut self, id: &str, title: &str) {
        self.pending(&format!("Retrying \"{title}\"..."));
        match self.backend.retry_task(id) {
            Ok(()) => {
                self.ok(&format!("✓ Retried \"{title}\""));
                self.refresh_all(false);
            }
            Err(err) => self.fail(&err),
        }
    }

    fn assign(&mut self, task_id: &str, title: &str, agent: &str) {
        self.pending(&format!("Assigning \"{title}\" to {agent}..."));
        match self.backend.assign_task(task_id, agent) {
            Ok(()) => {
                self.ok(&format!("✓ Assigned \"{title}\" to {agent}"));
                self.refresh_all(false);
            }
            Err(err) => self.fail(&err),
        }
    }

    fn disable(&mut self, id: &str, name: &str) {
        self.pending(&format!("Disabling {name}..."));
        match self.backend.disable_agent(id) {
            Ok(()) => {
                self.ok(&format!("✓ Disabled {name}"));
                self.refresh_all(false);
            }
            Err(err) => self.fail(&err),
        }
    }

    fn enable(&mut self, id: &str, name: &str) {
        self.pending(&format!("Enabling {name}..."));
        match self.backend.enable_agent(id) {
            Ok(()) => {
                self.ok(&format!("✓ Enabled {name}"));
                self.refresh_all(false);
            }
            Err(err) => self.fail(&err),
        }
    }
}

/// Every goal status name, for the `/goal status` usage string.
pub fn goal_status_names() -> Vec<&'static str> {
    GOAL_STATUSES.iter().map(GoalStatus::as_str).collect()
}
