//! `orch org`.
//!
//! `deploy` is a sequence of ordinary service calls with one unusual
//! property: it is **not** transactional. An agent that fails to create
//! leaves the earlier ones behind, and the error says so by naming the count
//! — the half-built tree is observable.

use std::path::Path;
use std::process::ExitCode;

use clap::{Args, Subcommand};

use orch_core::error::Result;
use orch_domain::{
    CreateGoalInput, CreateTeamInput, ORG_TEMPLATES, OrgTemplate, get_org_template_by_key,
    get_shop_template_by_key,
};
use orch_engine::Engine;
use orch_engine::agent_factory::template_to_agent_input;
use serde::Serialize;

use crate::context::{CliContext, EnvFlags};
use crate::output::{Styles, print_error, print_success, print_table};
use crate::tree::Cli;

#[derive(Debug, Args)]
pub struct OrgCmd {
    #[command(subcommand)]
    pub command: Option<OrgSub>,
}

#[derive(Debug, Subcommand)]
pub enum OrgSub {
    /// List available company templates
    #[command(alias = "ls")]
    List,
    /// Deploy a pre-built AI company
    Template(super::org_template::TemplateCmd),
    Deploy {
        #[arg(value_name = "template", conflicts_with_all=["template_name","template_dir"], required_unless_present_any=["template_name","template_dir"])]
        template: Option<String>,
        #[arg(long = "template", conflicts_with = "template_dir")]
        template_name: Option<String>,
        #[arg(long, value_name = "PATH")]
        template_dir: Option<std::path::PathBuf>,
        #[arg(long)]
        adapter: Option<String>,
        #[arg(long)]
        model: Option<String>,
        #[arg(long, value_name = "goal", help = "Set a goal for the team")]
        goal: Option<String>,
    },
}

/// Pre-parse initialization policy for the nested org command owner.
/// Unknown org operations retain the existing project guard.
pub(crate) fn requires_initialized_project(args: &[String]) -> bool {
    match args.iter().map(String::as_str).find(|arg| {
        !matches!(
            *arg,
            "--json" | "--yaml" | "--quiet" | "--no-color" | "--ascii"
        )
    }) {
        Some("template") => super::org_template::requires_initialized_project(&args[1..]),
        _ => true,
    }
}

/// Keep nested dispatch here so template operations can choose whether they
/// need an engine before the project container is built.
pub(crate) async fn dispatch(
    cli: &Cli,
    env: EnvFlags,
    cwd: &Path,
    args: &OrgCmd,
) -> Result<ExitCode> {
    let Some(sub) = &args.command else {
        return Ok(crate::print_group_help(&["org"]));
    };
    if let OrgSub::Template(args) = sub {
        return super::org_template::dispatch(cli, env, cwd, args).await;
    }
    let (ctx, styles, engine) = crate::build(cli, env, cwd).await?;
    match sub {
        OrgSub::List => {
            list(&ctx, &styles).await?;
            Ok(ExitCode::SUCCESS)
        }
        OrgSub::Deploy {
            template,
            template_name,
            template_dir,
            adapter,
            model,
            goal,
        } => {
            if let Some(key) = template.as_deref()
                && key != "a-team"
                && !key.contains(':')
                && adapter.is_none()
                && model.is_none()
                && !orch_infra::storage::org_templates::user_template_exists(key)?
            {
                return deploy(&engine, &ctx, &styles, key, goal.as_deref()).await;
            }
            let name = template_name.as_deref().or(template.as_deref());
            let loaded =
                orch_infra::storage::org_templates::resolve(name, template_dir.as_deref()).await?;
            let deployed = orch_engine::org_templates::deploy_bundle(
                &engine,
                &loaded.bundle,
                adapter.as_deref(),
                model.as_deref(),
                goal.as_deref(),
                orch_infra::storage::org_templates::validate_record_size,
            )
            .await?;
            if ctx.quiet && ctx.output.is_none() {
                println!("{}", deployed.team.id);
            } else {
                super::org_template::emit(&ctx, &deployed)?;
            }
            Ok(ExitCode::SUCCESS)
        }
        OrgSub::Template(_) => unreachable!("dispatched before engine construction"),
    }
}

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
    let builtin = orch_infra::storage::org_templates::builtin("a-team").await?;
    if let Some(format) = ctx.output {
        let mut values = serde_json::to_value(&ORG_TEMPLATES)
            .map_err(|e| orch_core::Error::other(e.to_string()))?;
        values.as_array_mut().expect("catalogue array").push(serde_json::json!({"key":"a-team","name":"a-team","description":builtin.org.description,"agents":builtin.agents.iter().map(|a|serde_json::json!({"shop_key":format!("a-team/{}",a.name),"name":a.name})).collect::<Vec<_>>(),"lead_index":builtin.agents.iter().position(|a|a.name==builtin.org.lead).expect("lead")}));
        return format.print(&values);
    }

    println!();
    let mut rows: Vec<Vec<String>> = ORG_TEMPLATES
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
    rows.push(vec![
        "a-team".into(),
        "a-team".into(),
        builtin.agents.len().to_string(),
        builtin.org.description,
    ]);
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

#[cfg(test)]
mod resolver_tests {
    use orch_engine::org_templates::resolve_inputs;
    use serde_json::json;

    #[tokio::test]
    async fn skills_presence_and_adapter_bound_models_resolve_once() {
        let mut bundle = orch_infra::storage::org_templates::builtin("a-team")
            .await
            .unwrap();
        bundle.org.defaults = Some(
            serde_json::from_value(
                json!({"adapter":"claude","model":"claude-model","skills":["review"]}),
            )
            .unwrap(),
        );
        bundle.agents.truncate(1);
        bundle.org.lead = bundle.agents[0].name.clone();
        let agent = &mut bundle.agents[0];
        agent.adapter = None;
        agent.skills = None;
        agent.model = None;
        let config = orch_domain::default_config();
        let inputs = resolve_inputs(
            &bundle,
            &config,
            None,
            None,
            orch_infra::storage::org_templates::validate_record_size,
        )
        .unwrap();
        assert_eq!(inputs[0].skills, Some(vec!["review".into()]));
        assert_eq!(inputs[0].model.as_deref(), Some("claude-model"));
        bundle.agents[0].skills = Some(vec![]);
        let inputs = resolve_inputs(
            &bundle,
            &config,
            Some("codex"),
            None,
            orch_infra::storage::org_templates::validate_record_size,
        )
        .unwrap();
        assert_eq!(inputs[0].skills, Some(vec![]));
        assert_eq!(inputs[0].model, None);
        assert!(
            resolve_inputs(
                &bundle,
                &config,
                Some("shell"),
                None,
                orch_infra::storage::org_templates::validate_record_size
            )
            .is_err()
        );
    }
}

#[cfg(test)]
mod deployment_tests {
    use orch_core::storage::AgentStore;
    use orch_core::{Error, Result};
    use orch_domain::Agent;
    use orch_engine::org_templates::deploy_bundle;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    struct PersistThenFail {
        inner: orch_core::test_support::FakeAgentStore,
        saves: AtomicUsize,
    }
    #[async_trait::async_trait]
    impl AgentStore for PersistThenFail {
        async fn list(&self) -> Result<Vec<Agent>> {
            self.inner.list().await
        }
        async fn get(&self, id: &str) -> Result<Option<Agent>> {
            self.inner.get(id).await
        }
        async fn get_by_name(&self, name: &str) -> Result<Option<Agent>> {
            self.inner.get_by_name(name).await
        }
        async fn save(&self, agent: &Agent) -> Result<()> {
            self.inner.save(agent).await?;
            if self.saves.fetch_add(1, Ordering::SeqCst) == 3 {
                Err(Error::other("injected index publication failure"))
            } else {
                Ok(())
            }
        }
        async fn delete(&self, id: &str) -> Result<()> {
            self.inner.delete(id).await
        }
    }
    #[tokio::test]
    async fn partial_failure_includes_the_entity_persisted_before_index_failure() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(".orchestry")).unwrap();
        let mut engine = orch_engine::build_light_with(
            dir.path(),
            orch_engine::EngineSeams {
                clock: Arc::new(orch_core::test_support::FixedClock::new()),
                ids: Arc::new(orch_core::test_support::SeqIdGenerator::new()),
                global_config_path: Some(dir.path().join("global.yml")),
            },
        )
        .await
        .unwrap();
        engine.agents = Arc::new(PersistThenFail {
            inner: Default::default(),
            saves: AtomicUsize::new(0),
        });
        engine.agent_service = Arc::new(orch_engine::AgentService::new(
            engine.agents.clone(),
            engine.state.clone(),
            engine.events.clone(),
            engine.config.clone(),
            engine.ids.clone(),
        ));
        let bundle = orch_infra::storage::org_templates::builtin("a-team")
            .await
            .unwrap();
        let error = deploy_bundle(
            &engine,
            &bundle,
            None,
            None,
            None,
            orch_infra::storage::org_templates::validate_record_size,
        )
        .await
        .unwrap_err()
        .to_string();
        let survivors = engine.agents.list().await.unwrap();
        assert_eq!(survivors.len(), 4);
        for agent in survivors {
            assert!(
                error.contains(&format!("\"{}\"", agent.id)),
                "missing {} in {error}",
                agent.id
            );
        }
    }
}

#[cfg(test)]
mod measurement_tests {
    use orch_core::template::{PromptContext, TemplateEngine};
    use orch_engine::org_templates::resolve_inputs;
    #[tokio::test]
    async fn measure_actual_formation_roles_and_default_liquid_prompts() {
        let bundle = orch_infra::storage::org_templates::builtin("a-team")
            .await
            .unwrap();
        let inputs = resolve_inputs(
            &bundle,
            &orch_domain::default_config(),
            None,
            None,
            orch_infra::storage::org_templates::validate_record_size,
        )
        .unwrap();
        let renderer = orch_infra::template::LiquidTemplateEngine::new();
        let mut metrics = Vec::new();
        for (index, input) in inputs.iter().enumerate() {
            let mut context:PromptContext=serde_json::from_value(serde_json::json!({
                "project":{"name":"template-probe","description":null},
                "task":{"id":"tsk_probe","title":"Inspect the implementation","description":"Report verified findings.","priority":2,"labels":[],"scope":null,"is_autonomous":false,"goal_id":null},
                "agent":{"id":format!("agt_{index:07}"),"name":input.name,"role":input.role},
                "agents":inputs.iter().enumerate().map(|(i,a)|serde_json::json!({"id":format!("agt_{i:07}"),"name":a.name,"role":if i==index{None}else{a.role.as_deref()},"adapter":a.adapter,"capabilities":[],"remote":null})).collect::<Vec<_>>(),
                "attempt":null,"workspace_path":"<workspace root>/.orchestry/workspaces/tsk_probe","retry":null,"feedback":null,"shared_context":null,"messages":null,"goal":null
            })).unwrap();
            let system = renderer
                .render(orch_infra::template::DEFAULT_SYSTEM_TEMPLATE, &context)
                .await
                .unwrap();
            let user = renderer
                .render(orch_infra::template::DEFAULT_USER_TEMPLATE, &context)
                .await
                .unwrap();
            context.agent.role = Some(bundle.agents[index].role.clone());
            for (i, entry) in context.agents.iter_mut().enumerate() {
                entry.role = if i == index {
                    None
                } else {
                    Some(bundle.agents[i].role.clone())
                };
            }
            let raw_system = renderer
                .render(orch_infra::template::DEFAULT_SYSTEM_TEMPLATE, &context)
                .await
                .unwrap();
            let raw_user = renderer
                .render(orch_infra::template::DEFAULT_USER_TEMPLATE, &context)
                .await
                .unwrap();
            assert!(system.len() + user.len() > raw_system.len() + raw_user.len());
            metrics.push(serde_json::json!({"name":input.name,"authored_role_bytes":bundle.agents[index].role.len(),"rendered_role_bytes":input.role.as_ref().unwrap().len(),"system_prompt_bytes":system.len(),"user_prompt_bytes":user.len(),"total_prompt_bytes":system.len()+user.len(),"unexpanded_prompt_bytes":raw_system.len()+raw_user.len(),"expansion_prompt_delta":system.len()+user.len()-raw_system.len()-raw_user.len()}));
        }
        assert_eq!(metrics.len(), 27);
        println!("P1_METRICS={}", serde_json::to_string(&metrics).unwrap());
    }

    #[tokio::test]
    async fn oversized_expanded_roles_fail_before_service_creation() {
        let mut bundle = orch_infra::storage::org_templates::builtin("a-team")
            .await
            .unwrap();
        bundle
            .rules
            .insert("seniority".into(), "x".repeat(1024 * 1024));
        assert!(
            resolve_inputs(
                &bundle,
                &orch_domain::default_config(),
                None,
                None,
                orch_infra::storage::org_templates::validate_record_size
            )
            .unwrap_err()
            .to_string()
            .contains("1 MiB")
        );
    }
}

#[cfg(test)]
mod selector_tests {
    use super::*;
    use clap::Parser;
    #[tokio::test]
    async fn explicit_legacy_template_selector_uses_the_same_inputs_as_preview_and_shop() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(".orchestry")).unwrap();
        std::fs::write(
            dir.path().join(".orchestry/config.yml"),
            "defaults:\n  agent:\n    adapter: shell\n",
        )
        .unwrap();
        let cli = Cli::try_parse_from([
            "orch",
            "--json",
            "org",
            "deploy",
            "--template",
            "builtin:data-lab",
        ])
        .unwrap();
        let Some(crate::tree::Command::Org(args)) = &cli.command else {
            panic!("org command");
        };
        let code = dispatch(&cli, EnvFlags::default(), dir.path(), args)
            .await
            .unwrap();
        assert_eq!(code, ExitCode::SUCCESS);
        let engine = orch_engine::build_light(dir.path()).await.unwrap();
        let loaded = orch_infra::storage::org_templates::resolve(Some("builtin:data-lab"), None)
            .await
            .unwrap();
        let expected = orch_engine::org_templates::resolve_inputs(
            &loaded.bundle,
            &engine.config,
            None,
            None,
            orch_infra::storage::org_templates::validate_record_size,
        )
        .unwrap();
        let agents = engine.agent_service.list().await.unwrap();
        assert_eq!(agents.len(), expected.len());
        for (actual, expected) in agents.iter().zip(expected) {
            assert_eq!(actual.name, expected.name);
            assert_eq!(actual.role, expected.role);
            assert_eq!(actual.adapter, expected.adapter);
        }
    }
}
