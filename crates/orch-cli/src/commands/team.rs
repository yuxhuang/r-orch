//! `orch team`.
//!
//! The membership subcommands (`join`, `leave`, `add-task`, `set-lead`,
//! `disband`) ignore `--json` and `--quiet` entirely: they always print their
//! success line, which is intended and not an oversight.

use orch_core::error::Result;
use orch_domain::{CreateTeamInput, PartialTeamConfig, Team};
use orch_engine::Engine;

use crate::context::CliContext;
use crate::output::{Styles, print_key_value, print_success, print_table};

pub struct TeamCreateArgs<'a> {
    pub name: &'a str,
    pub lead: &'a str,
    pub members: Option<&'a str>,
    pub description: Option<&'a str>,
    pub no_auto_claim: bool,
}

pub async fn create(
    engine: &Engine,
    ctx: &CliContext,
    styles: &Styles,
    args: TeamCreateArgs<'_>,
) -> Result<()> {
    let team = engine
        .team_service
        .create(CreateTeamInput {
            name: args.name.to_owned(),
            description: args.description.map(str::to_owned),
            lead_agent_id: args.lead.to_owned(),
            member_agent_ids: args.members.map(|raw| {
                raw.split(',')
                    .map(|part| part.trim().to_owned())
                    .collect::<Vec<_>>()
            }),
            config: Some(PartialTeamConfig {
                auto_claim: Some(!args.no_auto_claim),
                ..PartialTeamConfig::default()
            }),
        })
        .await?;

    if let Some(format) = ctx.output {
        format.print(&team)?;
    } else if ctx.quiet {
        println!("{}", team.id);
    } else {
        print_success(
            styles,
            &format!("Created team \"{}\" → {}", team.name, team.id),
        );
    }
    Ok(())
}

pub async fn list(engine: &Engine, ctx: &CliContext, styles: &Styles) -> Result<()> {
    let teams = engine.team_service.list().await?;

    if let Some(format) = ctx.output {
        return format.print(&teams);
    }
    if teams.is_empty() {
        println!(
            "{}",
            styles.dim("\n  No teams. Create one: orch team create <name> --lead <agent-id>\n")
        );
        return Ok(());
    }

    let rows: Vec<Vec<String>> = teams.iter().map(row).collect();
    println!();
    print_table(
        styles,
        &["ID", "NAME", "STATUS", "LEAD", "MEMBERS", "POOL"],
        &rows,
        2,
    );
    println!();
    Ok(())
}

pub async fn show(engine: &Engine, ctx: &CliContext, styles: &Styles, id: &str) -> Result<()> {
    let team = engine.team_service.get(id).await?;

    if let Some(format) = ctx.output {
        return format.print(&team);
    }

    println!();
    print_key_value(
        styles,
        &[
            ("ID".to_owned(), team.id.clone()),
            ("Name".to_owned(), team.name.clone()),
            ("Status".to_owned(), team.status.as_str().to_owned()),
            ("Lead".to_owned(), team.lead_agent_id.clone()),
            (
                "Members".to_owned(),
                team.members
                    .iter()
                    .map(|member| format!("{} ({})", member.agent_id, member.role))
                    .collect::<Vec<_>>()
                    .join(", "),
            ),
            (
                "Pool".to_owned(),
                if team.task_pool.is_empty() {
                    styles.dim("empty")
                } else {
                    team.task_pool.join(", ")
                },
            ),
            ("Auto-claim".to_owned(), team.config.auto_claim.to_string()),
            ("Created".to_owned(), team.created_at.clone()),
        ],
    );
    println!();
    Ok(())
}

pub async fn join(engine: &Engine, styles: &Styles, team_id: &str, agent_id: &str) -> Result<()> {
    engine.team_service.join(team_id, agent_id).await?;
    print_success(styles, &format!("Agent {agent_id} joined team {team_id}"));
    Ok(())
}

pub async fn leave(engine: &Engine, styles: &Styles, team_id: &str, agent_id: &str) -> Result<()> {
    engine.team_service.leave(team_id, agent_id).await?;
    print_success(styles, &format!("Agent {agent_id} left team {team_id}"));
    Ok(())
}

pub async fn add_task(
    engine: &Engine,
    styles: &Styles,
    team_id: &str,
    task_id: &str,
) -> Result<()> {
    engine.team_service.add_task(team_id, task_id).await?;
    print_success(
        styles,
        &format!("Task {task_id} added to team {team_id} pool"),
    );
    Ok(())
}

pub async fn set_lead(
    engine: &Engine,
    styles: &Styles,
    team_id: &str,
    agent_id: &str,
) -> Result<()> {
    engine.team_service.set_lead(team_id, agent_id).await?;
    print_success(styles, &format!("{agent_id} is now lead of team {team_id}"));
    Ok(())
}

pub async fn disband(engine: &Engine, styles: &Styles, id: &str) -> Result<()> {
    engine.team_service.disband(id).await?;
    print_success(styles, &format!("Team {id} disbanded"));
    Ok(())
}

fn row(team: &Team) -> Vec<String> {
    vec![
        team.id.clone(),
        team.name.clone(),
        team.status.as_str().to_owned(),
        team.lead_agent_id.clone(),
        team.members.len().to_string(),
        team.task_pool.len().to_string(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use orch_domain::{DEFAULT_TEAM_CONFIG, TeamMember, TeamMemberRole, TeamStatus};

    #[test]
    fn a_list_row_counts_members_and_the_pool() {
        let team = Team {
            id: "team_1".to_owned(),
            name: "squad".to_owned(),
            description: None,
            status: TeamStatus::Active,
            members: vec![TeamMember {
                agent_id: "agt_1".to_owned(),
                role: TeamMemberRole::Lead,
                joined_at: "2026-10-13T16:03:00.479Z".to_owned(),
            }],
            task_pool: vec!["tsk_1".to_owned(), "tsk_2".to_owned()],
            lead_agent_id: "agt_1".to_owned(),
            created_at: "2026-10-13T16:03:00.479Z".to_owned(),
            updated_at: "2026-10-13T16:03:00.479Z".to_owned(),
            config: DEFAULT_TEAM_CONFIG,
        };
        assert_eq!(
            row(&team),
            vec!["team_1", "squad", "active", "agt_1", "1", "2"]
        );
    }
}
