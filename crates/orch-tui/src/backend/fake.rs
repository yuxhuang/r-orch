//! In-crate fake backend plus entity factories.
//!
//! Compiled unconditionally (not behind `cfg(test)`) so integration tests under
//! `tests/` can use it. It is a small in-memory store: enough state for the
//! views to render and every call recorded for assertions.

use orch_domain::{
    Agent, AgentConfig, AgentStats, AgentStatus, CreateGoalInput, CreateTeamInput, Goal,
    GoalStatus, NotificationPreferences, OrchestratorEvent, OrchestratorState, Task, TaskStatus,
    Team, TeamConfig, TeamMember, TeamMemberRole, TeamStatus, TuiPaletteName, default_state,
};

use super::{
    AgentFields, AgentOpts, BackendResult, ClipboardPaste, CreateTaskOpts, GoalFields,
    HistoryEntry, LaunchContext, ModelCatalog, TaskFields, TuiBackend,
};

/// A recorded facade call. Tests assert on this log rather than on side effects.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Call {
    CreateTask {
        title: String,
        opts: CreateTaskOpts,
    },
    UpdateTask {
        id: String,
        fields: TaskFields,
    },
    DeleteTask(String),
    RunTask(String),
    RunAll,
    CancelTask(String),
    RetryTask(String),
    AssignTask {
        task_id: String,
        agent_id: String,
    },
    ApproveTask(String),
    RejectTask {
        id: String,
        feedback: Option<String>,
    },
    AddAgent {
        name: String,
        adapter: String,
        opts: AgentOpts,
    },
    UpdateAgent {
        id: String,
        fields: AgentFields,
    },
    DeleteAgent(String),
    ForceStopAgent(String),
    ToggleAutonomous {
        id: String,
        enabled: bool,
    },
    DisableAgent(String),
    EnableAgent(String),
    CreateTeam(CreateTeamInput),
    JoinTeam {
        team_id: String,
        agent_id: String,
    },
    LeaveTeam {
        team_id: String,
        agent_id: String,
    },
    DisbandTeam(String),
    SetTeamLead {
        team_id: String,
        agent_id: String,
    },
    CreateGoal(CreateGoalInput),
    UpdateGoal {
        id: String,
        fields: GoalFields,
    },
    UpdateGoalStatus {
        id: String,
        status: GoalStatus,
        force: bool,
    },
    DeleteGoal(String),
    GoalProgress(String),
    StartWatch,
    StopWatch,
    SavePalette(TuiPaletteName),
    SaveActivityFilter(String),
    SaveNotifications(NotificationPreferences),
    SaveMaxConcurrent(u32),
    CompleteOnboarding,
    PasteImage,
    BackgroundInstall(String),
    Bell,
}

#[derive(Debug, Default)]
pub struct FakeBackend {
    pub context: LaunchContext,
    pub tasks: Vec<Task>,
    pub agents: Vec<Agent>,
    pub goals: Vec<Goal>,
    pub teams: Vec<Team>,
    pub state: OrchestratorState,
    pub calls: Vec<Call>,
    /// Queued events handed out by the next [`TuiBackend::poll_events`].
    pub pending_events: Vec<OrchestratorEvent>,
    pub pending_history: Vec<Vec<HistoryEntry>>,
    pub pending_catalog: Option<ModelCatalog>,
    pub pending_update: Option<String>,
    pub installed_version: Option<String>,
    pub goal_progress_report: String,
    pub clipboard: ClipboardPaste,
    /// When set, the next fallible call fails with this message.
    pub fail_next: Option<String>,
    pub can_create_task: bool,
    pub can_add_agent: bool,
    pub can_create_goal: bool,
    pub can_force_stop_agent: bool,
    pub can_paste_image: bool,
    next_id: usize,
}

impl FakeBackend {
    pub fn new() -> Self {
        FakeBackend {
            context: LaunchContext::default(),
            tasks: Vec::new(),
            agents: Vec::new(),
            goals: Vec::new(),
            teams: Vec::new(),
            state: default_state(),
            calls: Vec::new(),
            pending_events: Vec::new(),
            pending_history: Vec::new(),
            pending_catalog: None,
            pending_update: None,
            installed_version: None,
            goal_progress_report: String::new(),
            clipboard: ClipboardPaste::Empty,
            fail_next: None,
            can_create_task: true,
            can_add_agent: true,
            can_create_goal: true,
            can_force_stop_agent: true,
            can_paste_image: true,
            next_id: 1,
        }
    }

    pub fn with_tasks(mut self, tasks: Vec<Task>) -> Self {
        self.tasks = tasks;
        self
    }

    pub fn with_agents(mut self, agents: Vec<Agent>) -> Self {
        self.agents = agents;
        self
    }

    pub fn with_goals(mut self, goals: Vec<Goal>) -> Self {
        self.goals = goals;
        self
    }

    pub fn with_teams(mut self, teams: Vec<Team>) -> Self {
        self.teams = teams;
        self
    }

    pub fn with_context(mut self, context: LaunchContext) -> Self {
        self.context = context;
        self
    }

    /// Did any recorded call match `pred`?
    pub fn called(&self, pred: impl Fn(&Call) -> bool) -> bool {
        self.calls.iter().any(pred)
    }

    pub fn count(&self, pred: impl Fn(&Call) -> bool) -> usize {
        self.calls.iter().filter(|c| pred(c)).count()
    }

    fn fresh_id(&mut self, prefix: &str) -> String {
        let id = format!("{prefix}_{:03}", self.next_id);
        self.next_id += 1;
        id
    }

    fn check_failure(&mut self) -> BackendResult<()> {
        match self.fail_next.take() {
            Some(err) => Err(err),
            None => Ok(()),
        }
    }
}

// ── Entity factories ───────────────────────────────────────────────────────

pub const TS: &str = "2026-10-13T12:00:00.000Z";

pub fn make_task(id: &str, title: &str, status: TaskStatus) -> Task {
    Task {
        id: id.into(),
        title: title.into(),
        description: String::new(),
        status,
        priority: 3,
        assignee: None,
        labels: vec![],
        depends_on: vec![],
        created_at: TS.into(),
        updated_at: TS.into(),
        attempts: 0,
        max_attempts: 3,
        workspace_mode: None,
        workspace: None,
        proof: None,
        review_criteria: None,
        review_results: None,
        scope: None,
        feedback: None,
        goal_id: None,
        attachments: None,
    }
}

pub fn make_agent(id: &str, name: &str, status: AgentStatus) -> Agent {
    Agent {
        id: id.into(),
        name: name.into(),
        adapter: "claude".into(),
        role: None,
        config: AgentConfig::default(),
        status,
        current_task: None,
        autonomous: None,
        stats: AgentStats::default(),
        last_error: None,
    }
}

pub fn make_goal(id: &str, title: &str, status: GoalStatus) -> Goal {
    Goal {
        id: id.into(),
        title: title.into(),
        description: String::new(),
        status,
        assignee: None,
        created_at: TS.into(),
        updated_at: Some(TS.into()),
    }
}

pub fn make_team(id: &str, name: &str, lead_agent_id: &str, members: &[&str]) -> Team {
    Team {
        id: id.into(),
        name: name.into(),
        description: None,
        status: TeamStatus::Active,
        members: members
            .iter()
            .enumerate()
            .map(|(i, agent_id)| TeamMember {
                agent_id: (*agent_id).into(),
                role: if i == 0 {
                    TeamMemberRole::Lead
                } else {
                    TeamMemberRole::Member
                },
                joined_at: TS.into(),
            })
            .collect(),
        task_pool: vec![],
        lead_agent_id: lead_agent_id.into(),
        created_at: TS.into(),
        updated_at: TS.into(),
        config: TeamConfig {
            max_concurrent_tasks: None,
            auto_claim: false,
            message_ttl_ms: None,
        },
    }
}

// ── Trait impl ─────────────────────────────────────────────────────────────

impl TuiBackend for FakeBackend {
    fn context(&self) -> LaunchContext {
        self.context.clone()
    }

    fn can_create_task(&self) -> bool {
        self.can_create_task
    }
    fn can_add_agent(&self) -> bool {
        self.can_add_agent
    }
    fn can_create_goal(&self) -> bool {
        self.can_create_goal
    }
    fn can_force_stop_agent(&self) -> bool {
        self.can_force_stop_agent
    }
    fn can_paste_image(&self) -> bool {
        self.can_paste_image
    }

    fn refresh_tasks(&mut self) -> BackendResult<Vec<Task>> {
        Ok(self.tasks.clone())
    }

    fn refresh_agents(&mut self) -> BackendResult<Vec<Agent>> {
        Ok(self.agents.clone())
    }

    fn refresh_goals(&mut self) -> BackendResult<Vec<Goal>> {
        Ok(self.goals.clone())
    }

    fn refresh_state(&mut self) -> BackendResult<OrchestratorState> {
        Ok(self.state.clone())
    }

    fn list_teams(&mut self) -> BackendResult<Vec<Team>> {
        Ok(self.teams.clone())
    }

    fn create_task(&mut self, title: &str, opts: CreateTaskOpts) -> BackendResult<Task> {
        self.calls.push(Call::CreateTask {
            title: title.into(),
            opts: opts.clone(),
        });
        self.check_failure()?;
        let id = self.fresh_id("tsk");
        let mut task = make_task(&id, title, TaskStatus::Todo);
        if let Some(priority) = opts.priority {
            task.priority = priority;
        }
        if let Some(description) = opts.description {
            task.description = description;
        }
        task.attachments = opts.attachments;
        self.tasks.push(task.clone());
        Ok(task)
    }

    fn update_task(&mut self, id: &str, fields: TaskFields) -> BackendResult<Task> {
        self.calls.push(Call::UpdateTask {
            id: id.into(),
            fields: fields.clone(),
        });
        self.check_failure()?;
        let task = self
            .tasks
            .iter_mut()
            .find(|t| t.id == id)
            .ok_or_else(|| format!("Task not found: {id}"))?;
        if let Some(title) = fields.title {
            task.title = title;
        }
        if let Some(description) = fields.description {
            task.description = description;
        }
        if let Some(priority) = fields.priority {
            task.priority = priority;
        }
        if let Some(attachments) = fields.attachments {
            task.attachments = Some(attachments);
        }
        Ok(task.clone())
    }

    fn delete_task(&mut self, id: &str) -> BackendResult<()> {
        self.calls.push(Call::DeleteTask(id.into()));
        self.check_failure()?;
        self.tasks.retain(|t| t.id != id);
        Ok(())
    }

    fn run_task(&mut self, id: &str) -> BackendResult<()> {
        self.calls.push(Call::RunTask(id.into()));
        self.check_failure()
    }

    fn run_all(&mut self) -> BackendResult<()> {
        self.calls.push(Call::RunAll);
        self.check_failure()
    }

    fn cancel_task(&mut self, id: &str) -> BackendResult<()> {
        self.calls.push(Call::CancelTask(id.into()));
        self.check_failure()
    }

    fn retry_task(&mut self, id: &str) -> BackendResult<()> {
        self.calls.push(Call::RetryTask(id.into()));
        self.check_failure()
    }

    fn assign_task(&mut self, task_id: &str, agent_id: &str) -> BackendResult<()> {
        self.calls.push(Call::AssignTask {
            task_id: task_id.into(),
            agent_id: agent_id.into(),
        });
        self.check_failure()?;
        if let Some(task) = self.tasks.iter_mut().find(|t| t.id == task_id) {
            task.assignee = Some(agent_id.into());
        }
        Ok(())
    }

    fn approve_task(&mut self, id: &str) -> BackendResult<()> {
        self.calls.push(Call::ApproveTask(id.into()));
        self.check_failure()
    }

    fn reject_task(&mut self, id: &str, feedback: Option<&str>) -> BackendResult<()> {
        self.calls.push(Call::RejectTask {
            id: id.into(),
            feedback: feedback.map(str::to_string),
        });
        self.check_failure()
    }

    fn add_agent(&mut self, name: &str, adapter: &str, opts: AgentOpts) -> BackendResult<Agent> {
        self.calls.push(Call::AddAgent {
            name: name.into(),
            adapter: adapter.into(),
            opts: opts.clone(),
        });
        self.check_failure()?;
        let id = self.fresh_id("agt");
        let mut agent = make_agent(&id, name, AgentStatus::Idle);
        agent.adapter = adapter.into();
        agent.role = opts.role;
        agent.config.command = opts.command;
        agent.config.model = opts.model;
        agent.config.skills = opts.skills;
        self.agents.push(agent.clone());
        Ok(agent)
    }

    fn update_agent(&mut self, id: &str, fields: AgentFields) -> BackendResult<Agent> {
        self.calls.push(Call::UpdateAgent {
            id: id.into(),
            fields: fields.clone(),
        });
        self.check_failure()?;
        let agent = self
            .agents
            .iter_mut()
            .find(|a| a.id == id)
            .ok_or_else(|| format!("Agent not found: {id}"))?;
        if let Some(name) = fields.name {
            agent.name = name;
        }
        if let Some(adapter) = fields.adapter {
            agent.adapter = adapter;
        }
        if let Some(role) = fields.role {
            agent.role = if role.is_empty() { None } else { Some(role) };
        }
        if let Some(command) = fields.command {
            agent.config.command = if command.is_empty() {
                None
            } else {
                Some(command)
            };
        }
        if let Some(model) = fields.model {
            agent.config.model = if model.is_empty() { None } else { Some(model) };
        }
        Ok(agent.clone())
    }

    fn delete_agent(&mut self, id: &str) -> BackendResult<()> {
        self.calls.push(Call::DeleteAgent(id.into()));
        self.check_failure()?;
        self.agents.retain(|a| a.id != id);
        Ok(())
    }

    fn force_stop_agent(&mut self, id: &str) -> BackendResult<()> {
        self.calls.push(Call::ForceStopAgent(id.into()));
        self.check_failure()
    }

    fn toggle_autonomous(&mut self, id: &str, enabled: bool) -> BackendResult<Agent> {
        self.calls.push(Call::ToggleAutonomous {
            id: id.into(),
            enabled,
        });
        self.check_failure()?;
        let agent = self
            .agents
            .iter_mut()
            .find(|a| a.id == id)
            .ok_or_else(|| format!("Agent not found: {id}"))?;
        agent.autonomous = Some(enabled);
        Ok(agent.clone())
    }

    fn disable_agent(&mut self, id: &str) -> BackendResult<()> {
        self.calls.push(Call::DisableAgent(id.into()));
        self.check_failure()
    }

    fn enable_agent(&mut self, id: &str) -> BackendResult<()> {
        self.calls.push(Call::EnableAgent(id.into()));
        self.check_failure()
    }

    fn create_team(&mut self, input: CreateTeamInput) -> BackendResult<Team> {
        self.calls.push(Call::CreateTeam(input.clone()));
        self.check_failure()?;
        let id = self.fresh_id("team");
        let mut members = vec![input.lead_agent_id.clone()];
        members.extend(input.member_agent_ids.clone().unwrap_or_default());
        let refs: Vec<&str> = members.iter().map(String::as_str).collect();
        let team = make_team(&id, &input.name, &input.lead_agent_id, &refs);
        self.teams.push(team.clone());
        Ok(team)
    }

    fn join_team(&mut self, team_id: &str, agent_id: &str) -> BackendResult<Team> {
        self.calls.push(Call::JoinTeam {
            team_id: team_id.into(),
            agent_id: agent_id.into(),
        });
        self.check_failure()?;
        let team = self
            .teams
            .iter_mut()
            .find(|t| t.id == team_id)
            .ok_or_else(|| format!("Team not found: {team_id}"))?;
        team.members.push(TeamMember {
            agent_id: agent_id.into(),
            role: TeamMemberRole::Member,
            joined_at: TS.into(),
        });
        Ok(team.clone())
    }

    fn leave_team(&mut self, team_id: &str, agent_id: &str) -> BackendResult<Team> {
        self.calls.push(Call::LeaveTeam {
            team_id: team_id.into(),
            agent_id: agent_id.into(),
        });
        self.check_failure()?;
        let team = self
            .teams
            .iter_mut()
            .find(|t| t.id == team_id)
            .ok_or_else(|| format!("Team not found: {team_id}"))?;
        team.members.retain(|m| m.agent_id != agent_id);
        Ok(team.clone())
    }

    fn disband_team(&mut self, team_id: &str) -> BackendResult<()> {
        self.calls.push(Call::DisbandTeam(team_id.into()));
        self.check_failure()?;
        self.teams.retain(|t| t.id != team_id);
        Ok(())
    }

    fn set_team_lead(&mut self, team_id: &str, agent_id: &str) -> BackendResult<Team> {
        self.calls.push(Call::SetTeamLead {
            team_id: team_id.into(),
            agent_id: agent_id.into(),
        });
        self.check_failure()?;
        let team = self
            .teams
            .iter_mut()
            .find(|t| t.id == team_id)
            .ok_or_else(|| format!("Team not found: {team_id}"))?;
        team.lead_agent_id = agent_id.into();
        Ok(team.clone())
    }

    fn create_goal(&mut self, input: CreateGoalInput) -> BackendResult<Goal> {
        self.calls.push(Call::CreateGoal(input.clone()));
        self.check_failure()?;
        let id = self.fresh_id("goal");
        let mut goal = make_goal(&id, &input.title, GoalStatus::Active);
        goal.description = input.description.unwrap_or_default();
        goal.assignee = input.assignee;
        self.goals.push(goal.clone());
        Ok(goal)
    }

    fn update_goal(&mut self, id: &str, fields: GoalFields) -> BackendResult<Goal> {
        self.calls.push(Call::UpdateGoal {
            id: id.into(),
            fields: fields.clone(),
        });
        self.check_failure()?;
        let goal = self
            .goals
            .iter_mut()
            .find(|g| g.id == id)
            .ok_or_else(|| format!("Goal not found: {id}"))?;
        if let Some(title) = fields.title {
            goal.title = title;
        }
        if let Some(description) = fields.description {
            goal.description = description;
        }
        if let Some(assignee) = fields.assignee {
            goal.assignee = if assignee.is_empty() {
                None
            } else {
                Some(assignee)
            };
        }
        Ok(goal.clone())
    }

    fn update_goal_status(
        &mut self,
        id: &str,
        status: GoalStatus,
        force: bool,
    ) -> BackendResult<Goal> {
        self.calls.push(Call::UpdateGoalStatus {
            id: id.into(),
            status,
            force,
        });
        self.check_failure()?;
        let goal = self
            .goals
            .iter_mut()
            .find(|g| g.id == id)
            .ok_or_else(|| format!("Goal not found: {id}"))?;
        goal.status = status;
        Ok(goal.clone())
    }

    fn delete_goal(&mut self, id: &str) -> BackendResult<()> {
        self.calls.push(Call::DeleteGoal(id.into()));
        self.check_failure()?;
        self.goals.retain(|g| g.id != id);
        Ok(())
    }

    fn goal_progress(&mut self, id: &str) -> BackendResult<String> {
        self.calls.push(Call::GoalProgress(id.into()));
        self.check_failure()?;
        Ok(self.goal_progress_report.clone())
    }

    fn start_watch(&mut self) -> BackendResult<()> {
        self.calls.push(Call::StartWatch);
        self.check_failure()
    }

    fn stop_watch(&mut self) -> BackendResult<()> {
        self.calls.push(Call::StopWatch);
        self.check_failure()
    }

    fn save_palette(&mut self, palette: TuiPaletteName) -> BackendResult<()> {
        self.calls.push(Call::SavePalette(palette));
        self.check_failure()
    }

    fn save_activity_filter(&mut self, preset: &str) -> BackendResult<()> {
        self.calls.push(Call::SaveActivityFilter(preset.into()));
        self.check_failure()
    }

    fn save_notifications(&mut self, notifications: NotificationPreferences) -> BackendResult<()> {
        self.calls.push(Call::SaveNotifications(notifications));
        self.check_failure()
    }

    fn save_max_concurrent(&mut self, value: u32) -> BackendResult<()> {
        self.calls.push(Call::SaveMaxConcurrent(value));
        self.check_failure()
    }

    fn complete_onboarding(&mut self) -> BackendResult<()> {
        self.calls.push(Call::CompleteOnboarding);
        self.check_failure()
    }

    fn poll_events(&mut self) -> Vec<OrchestratorEvent> {
        std::mem::take(&mut self.pending_events)
    }

    fn poll_history(&mut self) -> Vec<Vec<HistoryEntry>> {
        std::mem::take(&mut self.pending_history)
    }

    fn poll_model_catalog(&mut self) -> Option<ModelCatalog> {
        self.pending_catalog.take()
    }

    fn poll_update(&mut self) -> Option<String> {
        self.pending_update.take()
    }

    fn background_install(&mut self, version: &str) -> BackendResult<()> {
        self.calls.push(Call::BackgroundInstall(version.into()));
        self.check_failure()
    }

    fn installed_version(&self) -> Option<String> {
        self.installed_version.clone()
    }

    fn paste_image(&mut self) -> BackendResult<ClipboardPaste> {
        self.calls.push(Call::PasteImage);
        self.check_failure()?;
        Ok(self.clipboard.clone())
    }

    fn bell(&mut self) {
        self.calls.push(Call::Bell);
    }
}
