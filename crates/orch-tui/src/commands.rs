//! `executeCommand` — the command-bar verb surface (docs/parity/tui.md §4.6).
//!
//! The verb and subcommand set is contract (PARITY.md §12.4), as are the
//! confirmation and refusal strings, which the TS tests assert as substrings.

use orch_domain::{AgentStatus, GOAL_STATUSES, GoalStatus, TUI_PALETTE_NAMES, TaskStatus};

use crate::app::{App, parse_palette};
use crate::backend::TuiBackend;
use crate::clock::Clock;
use crate::command_bar::{COMMAND_REGISTRY, CONFIG_SETTINGS, is_config_setting};
use crate::format::cap_line;
use crate::messages::MsgType;
use crate::state::is_runnable;
use crate::theme::Token;
use crate::wizard::configs;

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
        let stripped = raw.trim().trim_start_matches('/');
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
            "quit" => self.state.should_exit = true,
            other => {
                let text = format!("Unknown: {other}. Type /help for commands");
                self.refuse(&text);
            }
        }
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
                    // Open the wizard pre-filled instead of creating.
                    let mut wizard = configs::new_agent_wizard(
                        &self.state.agents,
                        &self.state.teams,
                        &self.model_catalog,
                        &self.state.default_adapter,
                    );
                    let steps = wizard
                        .all_steps()
                        .iter()
                        .cloned()
                        .map(|step| match step.id.as_str() {
                            "name" => step.default_value(&name),
                            "adapter" => step.default_value("shell"),
                            _ => step,
                        })
                        .collect();
                    wizard.replace_steps(steps);
                    let wizard = crate::wizard::Wizard::new(
                        crate::wizard::WizardKind::Agent,
                        "NEW AGENT",
                        wizard.all_steps().to_vec(),
                    );
                    self.open_wizard(wizard);
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
