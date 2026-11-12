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
