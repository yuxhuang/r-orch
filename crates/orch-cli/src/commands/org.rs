//! `orch org` — ported from `src/cli/commands/org.ts`
//! (`docs/parity/cli.md` §18).
//!
//! `deploy` is a sequence of ordinary service calls with one unusual
//! property: it is **not** transactional. An agent that fails to create
//! leaves the earlier ones behind, and the error says so by naming the count
//! — replicated, because the half-built tree is observable.

use std::process::ExitCode;

use orch_core::error::Result;
use orch_domain::{
    CreateGoalInput, CreateTeamInput, ORG_TEMPLATES, OrgTemplate, get_org_template_by_key,
    get_shop_template_by_key,
};
use orch_engine::Engine;
use orch_engine::agent_factory::template_to_agent_input;
use serde::Serialize;

use crate::context::CliContext;
use crate::output::{Styles, print_error, print_success, print_table};

/// `{ team, agentIds, goalId }` — camelCase among the snake_case entities,
/// because it is an ad-hoc object rather than a stored one.
#[derive(Debug, Serialize)]
struct DeployJson<'a> {
    team: &'a orch_domain::Team,
    #[serde(rename = "agentIds")]
    agent_ids: &'a [String],
    #[serde(rename = "goalId", skip_serializing_if = "Option::is_none")]
    goal_id: Option<&'a str>,
}

pub async fn list(ctx: &CliContext, styles: &Styles) -> Result<()> {
    if let Some(format) = ctx.output {
        return format.print(&ORG_TEMPLATES);
    }

    println!();
    let rows: Vec<Vec<String>> = ORG_TEMPLATES
        .iter()
        .map(|template| {
            vec![
                template.key.to_owned(),
                template.name.to_owned(),
                template.agents.len().to_string(),
                template.description.to_owned(),
            ]
        })
        .collect();
    print_table(styles, &["KEY", "NAME", "AGENTS", "DESCRIPTION"], &rows, 2);
    println!();
    println!(
        "  {} orch org deploy <key> --goal \"Your objective\"",
        styles.dim("Deploy:")
    );
    println!();
    Ok(())
}

pub async fn deploy(
    engine: &Engine,
    ctx: &CliContext,
    styles: &Styles,
    template_key: &str,
    goal: Option<&str>,
) -> Result<ExitCode> {
    let Some(template) = get_org_template_by_key(template_key) else {
        print_error(
            styles,
            &format!("Unknown template \"{template_key}\""),
            Some("Run: orch org list — to see available templates"),
        );
        return Ok(ExitCode::from(1));
    };

    let Some(agent_ids) = create_agents(engine, styles, template).await? else {
        return Ok(ExitCode::from(1));
    };

    let lead_id = agent_ids[template.lead_index].clone();
    let team = engine
        .team_service
        .create(CreateTeamInput {
            name: template.name.to_owned(),
            description: Some(template.description.to_owned()),
            lead_agent_id: lead_id.clone(),
            member_agent_ids: Some(
                agent_ids
                    .iter()
                    .filter(|id| **id != lead_id)
                    .cloned()
                    .collect(),
            ),
            // No `config`, so the team takes every default — unlike
            // `team create`, which always sends `auto_claim`.
            config: None,
        })
        .await?;

    let goal_id = match goal {
        Some(title) => Some(
            engine
                .goal_service
                .create(CreateGoalInput {
                    title: title.to_owned(),
                    description: None,
                    assignee: Some(lead_id),
                })
                .await?
                .id,
        ),
        None => None,
    };

    if let Some(format) = ctx.output {
        format.print(&DeployJson {
            team: &team,
            agent_ids: &agent_ids,
            goal_id: goal_id.as_deref(),
        })?;
        return Ok(ExitCode::SUCCESS);
    }
    if ctx.quiet {
        println!("{}", team.id);
        return Ok(ExitCode::SUCCESS);
    }

    println!();
    print_success(
        styles,
        &format!(
            "Deployed team \"{}\" — {} agents",
            template.name,
            template.agents.len()
        ),
    );
    println!();
    for (index, (entry, id)) in template.agents.iter().zip(&agent_ids).enumerate() {
        let is_lead = index == template.lead_index;
        let bullet = if is_lead { "★" } else { "•" };
        let role = if is_lead { "lead" } else { "member" };
        println!(
            "  {bullet} {} {}",
            entry.name,
            styles.dim(&format!("({id}, {role})"))
        );
    }
    println!("\n  Team: {}", styles.dim(&team.id));
    if let Some(goal_id) = &goal_id {
        println!(
            "  Goal: {} — \"{}\"",
            styles.dim(goal_id),
            goal.unwrap_or_default()
        );
    }
    println!();
    println!("  {} orch run --all --watch", styles.dim("Next:"));
    println!();
    Ok(ExitCode::SUCCESS)
}

/// Create one agent per template entry, in order.
///
/// `None` means the deployment failed and the error has already been printed;
/// the agents created before the failure are deliberately left in place, which
/// is what the hint tells the user to clean up.
async fn create_agents(
    engine: &Engine,
    styles: &Styles,
    template: &OrgTemplate,
) -> Result<Option<Vec<String>>> {
    let mut agent_ids: Vec<String> = Vec::new();

    for entry in template.agents {
        let Some(shop_template) = get_shop_template_by_key(entry.shop_key) else {
            print_error(
                styles,
                &format!("Agent shop template not found: {}", entry.shop_key),
                None,
            );
            return Ok(None);
        };

        let input = template_to_agent_input(shop_template, &engine.config.defaults.agent.adapter)
            .map(|input| orch_domain::CreateAgentInput {
                name: entry.name.to_owned(),
                ..input
            });
        let created = match input {
            Ok(input) => engine.agent_service.create(input).await,
            Err(err) => Err(err),
        };
        match created {
            Ok(agent) => agent_ids.push(agent.id),
            Err(err) => {
                print_error(
                    styles,
                    &format!("Failed to create agent \"{}\": {err}", entry.name),
                    (!agent_ids.is_empty())
                        .then(|| {
                            format!(
                                "{} agent(s) were already created. Clean up with: orch agent list",
                                agent_ids.len()
                            )
                        })
                        .as_deref(),
                );
                return Ok(None);
            }
        }
    }

    Ok(Some(agent_ids))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_template_has_a_lead_inside_its_own_roster() {
        for template in &ORG_TEMPLATES {
            assert!(
                template.lead_index < template.agents.len(),
                "{} has lead_index {} for {} agents",
                template.key,
                template.lead_index,
                template.agents.len()
            );
        }
    }

    #[test]
    fn the_deploy_json_drops_the_goal_when_there_is_none() {
        let team = orch_domain::Team {
            id: "team_1".to_owned(),
            name: "Data Lab".to_owned(),
            description: None,
            status: orch_domain::TeamStatus::Active,
            members: Vec::new(),
            task_pool: Vec::new(),
            lead_agent_id: "agt_1".to_owned(),
            created_at: "2026-10-13T16:03:00.479Z".to_owned(),
            updated_at: "2026-10-13T16:03:00.479Z".to_owned(),
            config: orch_domain::DEFAULT_TEAM_CONFIG,
        };
        let ids = ["agt_1".to_owned()];
        let without = serde_json::to_string(&DeployJson {
            team: &team,
            agent_ids: &ids,
            goal_id: None,
        })
        .expect("serialize");
        assert!(!without.contains("goalId"), "{without}");

        let with = serde_json::to_string(&DeployJson {
            team: &team,
            agent_ids: &ids,
            goal_id: Some("goal_1"),
        })
        .expect("serialize");
        assert!(with.contains("\"goalId\":\"goal_1\""), "{with}");
    }
}
