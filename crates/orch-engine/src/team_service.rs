//! Team service — ported from `src/application/team-service.ts`.
//!
//! Teams are a group of agents with one lead, plus a task pool. The pool and
//! `TeamConfig` are stored and surfaced by the CLI but never read by the
//! orchestrator, and `team:task_claimed` is never emitted by any code path —
//! the data model is preserved and the absent behaviour is not invented
//! (PARITY.md §7.18).
//!
//! Three membership errors are `InvalidArgumentsError` (exit **2**) even
//! though an identically-worded `AgentNotFoundError` / `TaskNotFoundError`
//! (exit **1**) exists in the domain: `Agent not found: …` and
//! `Task not found: …` raised from here keep exit 2, because that is what the
//! TS service throws and the exit code is contract (PARITY.md §3.1).

use std::sync::Arc;

use orch_core::clock::Clock;
use orch_core::error::{Error, Result};
use orch_core::ids::IdGenerator;
use orch_core::storage::{AgentStore, TaskStore, TeamStore};
use orch_domain::{
    CreateTeamInput, DomainError, OrchestratorEvent, Team, TeamMember, TeamMemberRole, TeamStatus,
};

use crate::events::EventBus;

/// Team lifecycle: creation, membership, the task pool and lead transfer.
pub struct TeamService {
    teams: Arc<dyn TeamStore>,
    agents: Arc<dyn AgentStore>,
    tasks: Arc<dyn TaskStore>,
    events: Arc<EventBus>,
    clock: Arc<dyn Clock>,
    ids: Arc<dyn IdGenerator>,
}

impl TeamService {
    pub fn new(
        teams: Arc<dyn TeamStore>,
        agents: Arc<dyn AgentStore>,
        tasks: Arc<dyn TaskStore>,
        events: Arc<EventBus>,
        clock: Arc<dyn Clock>,
        ids: Arc<dyn IdGenerator>,
    ) -> Self {
        TeamService {
            teams,
            agents,
            tasks,
            events,
            clock,
            ids,
        }
    }

    /// Create a team with `lead_agent_id` as its first member.
    ///
    /// Every validation happens before the first write, so a rejected create
    /// leaves no team behind. Additional members are de-duplicated against the
    /// lead only — a repeated id inside `member_agent_ids` is appended twice,
    /// as in the original.
    pub async fn create(&self, input: CreateTeamInput) -> Result<Team> {
        let name = input.name.trim();
        if name.is_empty() {
            return Err(Error::invalid_arguments("Team name is required"));
        }

        if self.agents.get(&input.lead_agent_id).await?.is_none() {
            return Err(Error::invalid_arguments(format!(
                "Lead agent not found: {}",
                input.lead_agent_id
            )));
        }

        if self.teams.get_by_name(name).await?.is_some() {
            // The lookup is by the trimmed name but the message quotes the raw
            // input, so `orch team add "  squad  "` reports `"  squad  "`.
            // Replicated rather than tidied: the string is contract.
            return Err(Error::invalid_arguments(format!(
                "Team \"{}\" already exists",
                input.name
            )));
        }

        let now = self.clock.now_iso();
        let lead_member = TeamMember {
            agent_id: input.lead_agent_id.clone(),
            role: TeamMemberRole::Lead,
            joined_at: now.clone(),
        };

        let mut additional_members = Vec::new();
        for agent_id in input.member_agent_ids.unwrap_or_default() {
            if agent_id == input.lead_agent_id {
                continue;
            }
            if self.agents.get(&agent_id).await?.is_none() {
                return Err(Error::invalid_arguments(format!(
                    "Member agent not found: {agent_id}"
                )));
            }
            additional_members.push(TeamMember {
                agent_id,
                role: TeamMemberRole::Member,
                joined_at: now.clone(),
            });
        }

        let mut members = Vec::with_capacity(1 + additional_members.len());
        members.push(lead_member);
        members.extend(additional_members.iter().cloned());

        let team = Team {
            id: self.ids.generate("team"),
            name: name.to_owned(),
            description: input.description,
            status: TeamStatus::Active,
            members,
            task_pool: Vec::new(),
            lead_agent_id: input.lead_agent_id,
            created_at: now.clone(),
            updated_at: now,
            config: input.config.unwrap_or_default().resolve(),
        };

        self.teams.save(&team).await?;

        self.events.emit(OrchestratorEvent::TeamCreated {
            team_id: team.id.clone(),
            name: team.name.clone(),
            lead_agent_id: team.lead_agent_id.clone(),
        });
        // The lead's own membership is reported by `team:created`, so only the
        // additional members get a join event.
        for member in &additional_members {
            self.events.emit(OrchestratorEvent::TeamMemberJoined {
                team_id: team.id.clone(),
                agent_id: member.agent_id.clone(),
            });
        }

        Ok(team)
    }

    pub async fn get(&self, id: &str) -> Result<Team> {
        self.teams
            .get(id)
            .await?
            .ok_or_else(|| Error::Domain(DomainError::TeamNotFound(id.to_owned())))
    }

    /// Directory enumeration order — `TeamStore` has no `_index.json`, so the
    /// order is not contract.
    pub async fn list(&self) -> Result<Vec<Team>> {
        self.teams.list().await
    }

    /// Add an existing agent to the team as a `member`.
    pub async fn join(&self, team_id: &str, agent_id: &str) -> Result<Team> {
        let mut team = self.get(team_id).await?;
        if team.members.iter().any(|m| m.agent_id == agent_id) {
            return Err(Error::invalid_arguments(format!(
                "Agent {agent_id} is already a member of team {team_id}"
            )));
        }
        if self.agents.get(agent_id).await?.is_none() {
            return Err(Error::invalid_arguments(format!(
                "Agent not found: {agent_id}"
            )));
        }

        let now = self.clock.now_iso();
        team.members.push(TeamMember {
            agent_id: agent_id.to_owned(),
            role: TeamMemberRole::Member,
            joined_at: now.clone(),
        });
        team.updated_at = now;
        self.teams.save(&team).await?;

        self.events.emit(OrchestratorEvent::TeamMemberJoined {
            team_id: team_id.to_owned(),
            agent_id: agent_id.to_owned(),
        });
        Ok(team)
    }

    /// Remove an agent from the team. The lead cannot leave.
    ///
    /// Removing an agent who is not a member is **not** an error: the team is
    /// still re-saved with a new `updated_at` and `team:member_left` is still
    /// emitted, exactly as in the original.
    pub async fn leave(&self, team_id: &str, agent_id: &str) -> Result<Team> {
        let mut team = self.get(team_id).await?;
        if agent_id == team.lead_agent_id {
            return Err(Error::invalid_arguments(
                "Lead cannot leave team. Disband the team or transfer lead first.",
            ));
        }

        team.members.retain(|m| m.agent_id != agent_id);
        team.updated_at = self.clock.now_iso();
        self.teams.save(&team).await?;

        self.events.emit(OrchestratorEvent::TeamMemberLeft {
            team_id: team_id.to_owned(),
            agent_id: agent_id.to_owned(),
        });
        Ok(team)
    }

    /// Append a task to the team's pool. Idempotent: a task already in the
    /// pool is neither re-saved nor re-announced.
    pub async fn add_task(&self, team_id: &str, task_id: &str) -> Result<Team> {
        let mut team = self.get(team_id).await?;
        if self.tasks.get(task_id).await?.is_none() {
            return Err(Error::invalid_arguments(format!(
                "Task not found: {task_id}"
            )));
        }

        if !team.task_pool.iter().any(|id| id == task_id) {
            team.task_pool.push(task_id.to_owned());
            team.updated_at = self.clock.now_iso();
            self.teams.save(&team).await?;
            self.events.emit(OrchestratorEvent::TeamTaskAdded {
                team_id: team_id.to_owned(),
                task_id: task_id.to_owned(),
            });
        }
        Ok(team)
    }

    /// Drop a task from the pool. Unlike [`TeamService::add_task`] this always
    /// saves, and it emits nothing — there is no `team:task_removed` event.
    pub async fn remove_task(&self, team_id: &str, task_id: &str) -> Result<Team> {
        let mut team = self.get(team_id).await?;
        team.task_pool.retain(|id| id != task_id);
        team.updated_at = self.clock.now_iso();
        self.teams.save(&team).await?;
        Ok(team)
    }

    /// Transfer the lead role to an existing member, demoting the current lead
    /// to `member`. Emits nothing.
    pub async fn set_lead(&self, team_id: &str, agent_id: &str) -> Result<Team> {
        let mut team = self.get(team_id).await?;
        if !team.members.iter().any(|m| m.agent_id == agent_id) {
            return Err(Error::invalid_arguments(format!(
                "Agent {agent_id} is not a member of team {team_id}"
            )));
        }

        // Demotion first: when the new lead *is* the current lead, the promotion
        // below has to win, which is also the TS statement order.
        for member in &mut team.members {
            if member.agent_id == team.lead_agent_id {
                member.role = TeamMemberRole::Member;
            }
        }
        for member in &mut team.members {
            if member.agent_id == agent_id {
                member.role = TeamMemberRole::Lead;
            }
        }

        team.lead_agent_id = agent_id.to_owned();
        team.updated_at = self.clock.now_iso();
        self.teams.save(&team).await?;
        Ok(team)
    }

    /// Mark the team `disbanded`. The record is kept, not deleted, so history
    /// and the task pool survive (PARITY.md §7.18).
    pub async fn disband(&self, team_id: &str) -> Result<()> {
        let mut team = self.get(team_id).await?;
        team.status = TeamStatus::Disbanded;
        team.updated_at = self.clock.now_iso();
        self.teams.save(&team).await?;

        self.events.emit(OrchestratorEvent::TeamDisbanded {
            team_id: team_id.to_owned(),
        });
        Ok(())
    }

    /// The first `active` team containing this agent, or `None`.
    ///
    /// An agent can be a member of several teams; this returns the first in
    /// store order rather than erroring, which is what the orchestrator's
    /// team-mate lookup depends on.
    pub async fn find_team_for_agent(&self, agent_id: &str) -> Result<Option<Team>> {
        Ok(self.teams.list().await?.into_iter().find(|t| {
            t.status == TeamStatus::Active && t.members.iter().any(|m| m.agent_id == agent_id)
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use orch_core::test_support::{
        FIXED_NOW_ISO, FakeAgentStore, FakeTaskStore, FakeTeamStore, FixedClock, SeqIdGenerator,
        TeamStoreCall, make_agent, make_task,
    };
    use orch_domain::{
        Agent, DEFAULT_TEAM_CONFIG, OrchestratorEventType, PartialTeamConfig, Task, TeamConfig,
    };

    const SEEDED_AT: &str = "2025-01-01T00:00:00Z";

    struct Harness {
        service: TeamService,
        teams: Arc<FakeTeamStore>,
        bus: Arc<EventBus>,
    }

    impl Harness {
        /// `teamStore.save` call count — the Rust stand-in for
        /// `expect(teamStore.save).toHaveBeenCalledOnce()`.
        fn saves(&self) -> usize {
            self.teams
                .calls
                .count_matching(|c| matches!(c, TeamStoreCall::Save(_)))
        }
    }

    /// The three agents and the one task every TS case gets from `beforeEach`.
    fn harness() -> Harness {
        let teams = Arc::new(FakeTeamStore::new());
        let agents = Arc::new(FakeAgentStore::with_agents([
            Agent {
                id: "agt_lead".into(),
                name: "Lead".into(),
                ..make_agent()
            },
            Agent {
                id: "agt_m1".into(),
                name: "Member1".into(),
                ..make_agent()
            },
            Agent {
                id: "agt_m2".into(),
                name: "Member2".into(),
                ..make_agent()
            },
        ]));
        let tasks = Arc::new(FakeTaskStore::with_tasks([Task {
            id: "tsk_1".into(),
            title: "Task 1".into(),
            ..make_task()
        }]));
        let bus = Arc::new(EventBus::new());
        let service = TeamService::new(
            Arc::clone(&teams) as Arc<dyn TeamStore>,
            agents as Arc<dyn AgentStore>,
            tasks as Arc<dyn TaskStore>,
            Arc::clone(&bus),
            Arc::new(FixedClock::new()),
            Arc::new(SeqIdGenerator::new()),
        );
        Harness {
            service,
            teams,
            bus,
        }
    }

    /// The TS `makeTeam()` fixture: `agt_lead` as lead plus `agt_m1`.
    fn team() -> Team {
        Team {
            id: "team_t1".into(),
            name: "Alpha Squad".into(),
            description: None,
            status: TeamStatus::Active,
            members: vec![
                TeamMember {
                    agent_id: "agt_lead".into(),
                    role: TeamMemberRole::Lead,
                    joined_at: SEEDED_AT.into(),
                },
                TeamMember {
                    agent_id: "agt_m1".into(),
                    role: TeamMemberRole::Member,
                    joined_at: SEEDED_AT.into(),
                },
            ],
            task_pool: vec![],
            lead_agent_id: "agt_lead".into(),
            created_at: SEEDED_AT.into(),
            updated_at: SEEDED_AT.into(),
            config: TeamConfig {
                max_concurrent_tasks: None,
                auto_claim: true,
                message_ttl_ms: None,
            },
        }
    }

    fn input(name: &str) -> CreateTeamInput {
        CreateTeamInput {
            name: name.into(),
            lead_agent_id: "agt_lead".into(),
            ..Default::default()
        }
    }

    // ── create ─────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn create_builds_the_team_with_the_lead_first() {
        let h = harness();
        let team = h
            .service
            .create(CreateTeamInput {
                member_agent_ids: Some(vec!["agt_m1".into()]),
                ..input("New Team")
            })
            .await
            .unwrap();

        assert!(team.id.starts_with("team_"), "{}", team.id);
        assert_eq!(team.name, "New Team");
        assert_eq!(team.status, TeamStatus::Active);
        assert_eq!(team.lead_agent_id, "agt_lead");
        assert_eq!(team.members.len(), 2);
        assert_eq!(team.members[0].agent_id, "agt_lead");
        assert_eq!(team.members[0].role, TeamMemberRole::Lead);
        assert_eq!(team.members[1].agent_id, "agt_m1");
        assert_eq!(team.members[1].role, TeamMemberRole::Member);
        assert_eq!(h.saves(), 1);
        // Timestamps come from the clock, and both are the same instant.
        assert_eq!(team.created_at, FIXED_NOW_ISO);
        assert_eq!(team.updated_at, FIXED_NOW_ISO);
        assert_eq!(team.members[0].joined_at, FIXED_NOW_ISO);
        assert_eq!(team.config, DEFAULT_TEAM_CONFIG);
    }

    #[tokio::test]
    async fn create_rejects_an_unknown_lead_agent() {
        let h = harness();
        let err = h
            .service
            .create(CreateTeamInput {
                lead_agent_id: "agt_nonexistent".into(),
                ..input("Bad Team")
            })
            .await
            .expect_err("rejected");
        assert_eq!(err.to_string(), "Lead agent not found: agt_nonexistent");
        assert_eq!(err.exit_code(), 2);
        assert_eq!(h.saves(), 0);
    }

    #[tokio::test]
    async fn create_rejects_a_duplicate_name() {
        let h = harness();
        h.teams.seed(team());

        let err = h
            .service
            .create(input("Alpha Squad"))
            .await
            .expect_err("rejected");
        assert_eq!(err.to_string(), "Team \"Alpha Squad\" already exists");
        assert_eq!(err.exit_code(), 2);
    }

    #[tokio::test]
    async fn create_rejects_an_empty_name() {
        let h = harness();
        for name in ["   ", "", "\t\n"] {
            let err = h.service.create(input(name)).await.expect_err("rejected");
            assert_eq!(err.to_string(), "Team name is required");
            assert_eq!(err.exit_code(), 2);
        }
        assert_eq!(h.saves(), 0);
    }

    #[tokio::test]
    async fn create_emits_team_created() {
        let h = harness();
        let mut sub = h.bus.subscribe();

        h.service.create(input("EventTeam")).await.unwrap();

        let events = sub.try_drain();
        assert_eq!(events.len(), 1);
        match events[0].as_ref() {
            OrchestratorEvent::TeamCreated {
                team_id,
                name,
                lead_agent_id,
            } => {
                assert!(team_id.starts_with("team_"));
                assert_eq!(name, "EventTeam");
                assert_eq!(lead_agent_id, "agt_lead");
            }
            other => panic!("expected team:created, got {other:?}"),
        }
    }

    /// Not a ported case: PARITY.md §7.18 says members are de-duplicated
    /// against the lead, and nothing in the TS suite covers it.
    #[tokio::test]
    async fn create_skips_a_member_who_is_also_the_lead() {
        let h = harness();
        let mut sub = h.bus.subscribe();

        let team = h
            .service
            .create(CreateTeamInput {
                member_agent_ids: Some(vec!["agt_lead".into(), "agt_m1".into()]),
                ..input("Dedupe")
            })
            .await
            .unwrap();

        assert_eq!(team.members.len(), 2);
        let types: Vec<_> = sub.try_drain().iter().map(|e| e.event_type()).collect();
        assert_eq!(
            types,
            vec![
                OrchestratorEventType::TeamCreated,
                OrchestratorEventType::TeamMemberJoined
            ]
        );
    }

    /// Not a ported case: `member_agent_ids` is validated before the first
    /// write, so a bad member leaves nothing behind.
    #[tokio::test]
    async fn create_rejects_an_unknown_member_before_saving() {
        let h = harness();
        let err = h
            .service
            .create(CreateTeamInput {
                member_agent_ids: Some(vec!["agt_ghost".into()]),
                ..input("Ghost Team")
            })
            .await
            .expect_err("rejected");
        assert_eq!(err.to_string(), "Member agent not found: agt_ghost");
        assert_eq!(err.exit_code(), 2);
        assert_eq!(h.saves(), 0);
    }

    /// Not a ported case: `config` is a TS `Partial<TeamConfig>` overlaid on
    /// the defaults, so an unset key keeps its default rather than becoming
    /// absent.
    #[tokio::test]
    async fn create_overlays_a_partial_config_on_the_defaults() {
        let h = harness();
        let team = h
            .service
            .create(CreateTeamInput {
                config: Some(PartialTeamConfig {
                    auto_claim: Some(false),
                    ..Default::default()
                }),
                ..input("Configured")
            })
            .await
            .unwrap();
        assert!(!team.config.auto_claim);
        assert_eq!(
            team.config.message_ttl_ms,
            DEFAULT_TEAM_CONFIG.message_ttl_ms
        );
    }

    // ── join ───────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn join_adds_a_member() {
        let h = harness();
        h.teams.seed(team());

        let updated = h.service.join("team_t1", "agt_m2").await.unwrap();

        assert_eq!(updated.members.len(), 3);
        let new_member = updated
            .members
            .iter()
            .find(|m| m.agent_id == "agt_m2")
            .expect("joined");
        assert_eq!(new_member.role, TeamMemberRole::Member);
        assert_eq!(new_member.joined_at, FIXED_NOW_ISO);
        assert_eq!(updated.updated_at, FIXED_NOW_ISO);
        assert_eq!(h.saves(), 1);
    }

    #[tokio::test]
    async fn join_rejects_an_existing_member() {
        let h = harness();
        h.teams.seed(team());

        let err = h
            .service
            .join("team_t1", "agt_m1")
            .await
            .expect_err("rejected");
        assert_eq!(
            err.to_string(),
            "Agent agt_m1 is already a member of team team_t1"
        );
        assert_eq!(err.exit_code(), 2);
        assert_eq!(h.saves(), 0);
    }

    #[tokio::test]
    async fn join_emits_team_member_joined() {
        let h = harness();
        h.teams.seed(team());
        let mut sub = h.bus.subscribe();

        h.service.join("team_t1", "agt_m2").await.unwrap();

        let events = sub.try_drain();
        assert_eq!(events.len(), 1);
        assert_eq!(
            *events[0],
            OrchestratorEvent::TeamMemberJoined {
                team_id: "team_t1".into(),
                agent_id: "agt_m2".into(),
            }
        );
    }

    /// Not a ported case: an unknown agent cannot join, and the message is the
    /// `AgentNotFoundError` wording at exit 2 (see the module docs).
    #[tokio::test]
    async fn join_rejects_an_unknown_agent_with_exit_2() {
        let h = harness();
        h.teams.seed(team());

        let err = h
            .service
            .join("team_t1", "agt_ghost")
            .await
            .expect_err("rejected");
        assert_eq!(err.to_string(), "Agent not found: agt_ghost");
        assert_eq!(err.exit_code(), 2);
    }

    // ── leave ──────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn leave_removes_a_member() {
        let h = harness();
        h.teams.seed(team());

        let updated = h.service.leave("team_t1", "agt_m1").await.unwrap();

        assert_eq!(updated.members.len(), 1);
        assert_eq!(updated.members[0].agent_id, "agt_lead");
        assert_eq!(h.saves(), 1);
    }

    #[tokio::test]
    async fn leave_rejects_the_lead() {
        let h = harness();
        h.teams.seed(team());

        let err = h
            .service
            .leave("team_t1", "agt_lead")
            .await
            .expect_err("rejected");
        assert_eq!(
            err.to_string(),
            "Lead cannot leave team. Disband the team or transfer lead first."
        );
        assert_eq!(err.exit_code(), 2);
        assert_eq!(h.saves(), 0);
    }

    #[tokio::test]
    async fn leave_emits_team_member_left() {
        let h = harness();
        h.teams.seed(team());
        let mut sub = h.bus.subscribe();

        h.service.leave("team_t1", "agt_m1").await.unwrap();

        let events = sub.try_drain();
        assert_eq!(events.len(), 1);
        assert_eq!(
            *events[0],
            OrchestratorEvent::TeamMemberLeft {
                team_id: "team_t1".into(),
                agent_id: "agt_m1".into(),
            }
        );
    }

    /// Not a ported case: leaving a team the agent never joined still saves and
    /// still emits. Replicated from the TS, which filters unconditionally.
    #[tokio::test]
    async fn leave_is_not_an_error_for_a_non_member() {
        let h = harness();
        h.teams.seed(team());
        let mut sub = h.bus.subscribe();

        let updated = h.service.leave("team_t1", "agt_m2").await.unwrap();

        assert_eq!(updated.members.len(), 2);
        assert_eq!(h.saves(), 1);
        assert_eq!(sub.try_drain().len(), 1);
    }

    // ── add_task / remove_task ─────────────────────────────────────────────

    #[tokio::test]
    async fn add_task_appends_to_the_pool() {
        let h = harness();
        h.teams.seed(team());

        let updated = h.service.add_task("team_t1", "tsk_1").await.unwrap();

        assert_eq!(updated.task_pool, vec!["tsk_1".to_owned()]);
        assert_eq!(h.saves(), 1);
    }

    #[tokio::test]
    async fn add_task_rejects_an_unknown_task() {
        let h = harness();
        h.teams.seed(team());

        let err = h
            .service
            .add_task("team_t1", "tsk_nonexistent")
            .await
            .expect_err("rejected");
        assert_eq!(err.to_string(), "Task not found: tsk_nonexistent");
        assert_eq!(err.exit_code(), 2);
        assert_eq!(h.saves(), 0);
    }

    #[tokio::test]
    async fn add_task_emits_team_task_added() {
        let h = harness();
        h.teams.seed(team());
        let mut sub = h.bus.subscribe();

        h.service.add_task("team_t1", "tsk_1").await.unwrap();

        let events = sub.try_drain();
        assert_eq!(events.len(), 1);
        assert_eq!(
            *events[0],
            OrchestratorEvent::TeamTaskAdded {
                team_id: "team_t1".into(),
                task_id: "tsk_1".into(),
            }
        );
    }

    #[tokio::test]
    async fn add_task_does_not_duplicate_a_pooled_task() {
        let h = harness();
        h.teams.seed(Team {
            task_pool: vec!["tsk_1".into()],
            ..team()
        });
        let mut sub = h.bus.subscribe();

        let updated = h.service.add_task("team_t1", "tsk_1").await.unwrap();

        assert_eq!(
            updated.task_pool.iter().filter(|id| *id == "tsk_1").count(),
            1
        );
        // Idempotent means no write and no event, not just no duplicate.
        assert_eq!(h.saves(), 0);
        assert!(sub.try_drain().is_empty());
    }

    /// Not a ported case: `remove_task` has no TS test at all.
    #[tokio::test]
    async fn remove_task_drops_the_task_and_emits_nothing() {
        let h = harness();
        h.teams.seed(Team {
            task_pool: vec!["tsk_1".into(), "tsk_2".into()],
            ..team()
        });
        let mut sub = h.bus.subscribe();

        let updated = h.service.remove_task("team_t1", "tsk_1").await.unwrap();

        assert_eq!(updated.task_pool, vec!["tsk_2".to_owned()]);
        assert_eq!(updated.updated_at, FIXED_NOW_ISO);
        assert_eq!(h.saves(), 1);
        assert!(sub.try_drain().is_empty());
    }

    // ── set_lead ───────────────────────────────────────────────────────────

    #[tokio::test]
    async fn set_lead_transfers_the_role() {
        let h = harness();
        h.teams.seed(team());

        let updated = h.service.set_lead("team_t1", "agt_m1").await.unwrap();

        assert_eq!(updated.lead_agent_id, "agt_m1");
        let role = |id: &str| {
            updated
                .members
                .iter()
                .find(|m| m.agent_id == id)
                .map(|m| m.role)
        };
        assert_eq!(role("agt_m1"), Some(TeamMemberRole::Lead));
        assert_eq!(role("agt_lead"), Some(TeamMemberRole::Member));
        assert_eq!(h.saves(), 1);
    }

    #[tokio::test]
    async fn set_lead_rejects_a_non_member() {
        let h = harness();
        h.teams.seed(team());

        let err = h
            .service
            .set_lead("team_t1", "agt_m2")
            .await
            .expect_err("rejected");
        assert_eq!(
            err.to_string(),
            "Agent agt_m2 is not a member of team team_t1"
        );
        assert_eq!(err.exit_code(), 2);
        assert_eq!(h.saves(), 0);
    }

    /// Not a ported case: re-electing the sitting lead must not demote them,
    /// which is why the demotion runs before the promotion.
    #[tokio::test]
    async fn set_lead_on_the_current_lead_keeps_the_role() {
        let h = harness();
        h.teams.seed(team());

        let updated = h.service.set_lead("team_t1", "agt_lead").await.unwrap();

        assert_eq!(updated.lead_agent_id, "agt_lead");
        assert_eq!(updated.members[0].role, TeamMemberRole::Lead);
    }

    // ── disband ────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn disband_sets_the_status_to_disbanded() {
        let h = harness();
        h.teams.seed(team());

        h.service.disband("team_t1").await.unwrap();

        let saved = h.teams.saved();
        assert_eq!(saved.len(), 1);
        assert_eq!(saved[0].status, TeamStatus::Disbanded);
        // The record survives — disband is not a delete (PARITY.md §7.18).
        assert!(h.teams.peek("team_t1").is_some());
    }

    #[tokio::test]
    async fn disband_emits_team_disbanded() {
        let h = harness();
        h.teams.seed(team());
        let mut sub = h.bus.subscribe();

        h.service.disband("team_t1").await.unwrap();

        let events = sub.try_drain();
        assert_eq!(events.len(), 1);
        assert_eq!(
            *events[0],
            OrchestratorEvent::TeamDisbanded {
                team_id: "team_t1".into(),
            }
        );
    }

    // ── find_team_for_agent ────────────────────────────────────────────────

    #[tokio::test]
    async fn find_team_for_agent_returns_the_team() {
        let h = harness();
        h.teams.seed(team());

        let found = h.service.find_team_for_agent("agt_m1").await.unwrap();
        assert_eq!(found.map(|t| t.id), Some("team_t1".to_owned()));
    }

    #[tokio::test]
    async fn find_team_for_agent_returns_none_when_unaffiliated() {
        let h = harness();
        assert!(
            h.service
                .find_team_for_agent("agt_m2")
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn find_team_for_agent_ignores_disbanded_teams() {
        let h = harness();
        h.teams.seed(Team {
            status: TeamStatus::Disbanded,
            ..team()
        });

        assert!(
            h.service
                .find_team_for_agent("agt_m1")
                .await
                .unwrap()
                .is_none()
        );
    }

    // ── get ────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn get_reports_team_not_found_with_exit_1() {
        let h = harness();
        let err = h
            .service
            .get("team_nonexistent")
            .await
            .expect_err("missing");
        assert_eq!(err.to_string(), "Team not found: team_nonexistent");
        assert_eq!(err.exit_code(), 1);
    }
}
