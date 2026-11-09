//! Adapters for the peer registry, diagnostics, catalogues and org templates.

use super::authored::{definition_candidate, definition_document, read_document};
use super::{OpContext, ServiceHandler, service_error};
use crate::coordinator::OpResult;
use orch_core::Error;
use orch_domain::org_templates::{AgentDefinition, Bundle, OrgDefinition, validate_id};
use orch_domain::{RemoteConfig, RemoteStatus};
use orch_infra::storage::org_templates::{
    self as templates, LockedTemplate, Mutation, Source, Target,
};
use orch_infra::storage::remote::FileRemoteStore;
use orch_proto::error::{ErrorBody, ErrorCode, Execution};
use orch_proto::ops::{args::*, results::*};
use orch_proto::path::WirePath;
use serde_json::Value;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// Doctor and review commands share the daemon's startup snapshot. Capture
/// at most the review budget plus one byte to detect overflow while preserving
/// the existing review failure semantics. No child output is logged.
pub(crate) struct SnapshotCommandRunner(
    pub(crate) std::sync::Arc<orch_infra::models::SnapshotDiscoveryRuntime>,
);

#[async_trait::async_trait]
impl orch_engine::review_runner::CommandRunner for SnapshotCommandRunner {
    async fn run(
        &self,
        command: &str,
        args: &[&str],
        cwd: &Path,
        timeout: std::time::Duration,
    ) -> orch_engine::review_runner::CommandOutcome {
        use orch_engine::review_runner::{CommandOutcome, MAX_OUTPUT_BYTES};
        match self
            .0
            .output(command, args, cwd, timeout, MAX_OUTPUT_BYTES + 1)
            .await
        {
            Ok(output) => CommandOutcome {
                failed: !output.status.success()
                    || output.stdout.len() > MAX_OUTPUT_BYTES
                    || output.stderr.len() > MAX_OUTPUT_BYTES,
                stdout: String::from_utf8_lossy(
                    &output.stdout[..output.stdout.len().min(MAX_OUTPUT_BYTES)],
                )
                .into_owned(),
                stderr: String::from_utf8_lossy(
                    &output.stderr[..output.stderr.len().min(MAX_OUTPUT_BYTES)],
                )
                .into_owned(),
            },
            Err(error) => CommandOutcome {
                failed: true,
                stdout: String::new(),
                stderr: error.to_string(),
            },
        }
    }
}

impl ServiceHandler {
    pub(super) async fn run_history(
        &self,
        _ctx: &OpContext,
        _args: RunHistoryArgs,
    ) -> OpResult<RunHistory> {
        let mut runs = self
            .engine
            .run_service
            .list_all()
            .await
            .map_err(service_error)?;
        runs.sort_by(|left, right| right.started_at.cmp(&left.started_at));
        runs.retain(|run| {
            matches!(
                run.status,
                orch_domain::RunStatus::Succeeded | orch_domain::RunStatus::Failed
            )
        });
        let mut batches = Vec::new();
        for batch in [
            runs.iter().take(3).collect::<Vec<_>>(),
            runs.iter().skip(3).take(7).collect::<Vec<_>>(),
        ] {
            if batch.is_empty() {
                continue;
            }
            let mut entries = Vec::new();
            for run in batch {
                let events = self
                    .engine
                    .run_service
                    .read_events_tail(&run.id, 30)
                    .await
                    .map_err(service_error)?;
                entries.extend(events.into_iter().map(|event| HistoryEntry {
                    timestamp: event.timestamp,
                    run_id: run.id.clone(),
                    agent_id: run.agent_id.clone(),
                    task_id: run.task_id.clone(),
                    event_type: event.event_type.as_str().into(),
                    data: match event.data {
                        Value::String(text) => text,
                        other => other.to_string(),
                    },
                }));
            }
            entries.sort_by(|left, right| left.timestamp.cmp(&right.timestamp));
            if entries.len() > 200 {
                entries.drain(..entries.len() - 200);
            }
            batches.push(entries);
        }
        Ok(RunHistory { batches })
    }

    pub(super) async fn run_timings(
        &self,
        _ctx: &OpContext,
        _args: RunTimingsArgs,
    ) -> OpResult<TimingSnapshot> {
        let runs = self
            .engine
            .run_service
            .list_all()
            .await
            .map_err(service_error)?;
        let tasks = self
            .engine
            .tasks
            .list(&Default::default())
            .await
            .map_err(service_error)?;
        let goals = self
            .engine
            .goals
            .list(&Default::default())
            .await
            .map_err(service_error)?;
        let state = self.engine.state.read().await.map_err(service_error)?;
        let now = self.engine.clock.now_ms();
        Ok(derive_timings(&runs, &tasks, &goals, &state, now))
    }

    pub(super) async fn remote_add(
        &self,
        _ctx: &OpContext,
        args: RemoteAddArgs,
    ) -> OpResult<RemoteConfig> {
        let entry = RemoteConfig {
            alias: args.alias.into_inner(),
            addr: normalize_addr(&args.addr).map_err(service_error)?,
            added_at: self.engine.clock.now_iso(),
        };
        FileRemoteStore::new(self.engine.paths.clone())
            .add(entry.clone())
            .await
            .map_err(service_error)?;
        Ok(entry)
    }

    pub(super) async fn remote_remove(
        &self,
        _ctx: &OpContext,
        args: RemoteRemoveArgs,
    ) -> OpResult<()> {
        // The live peer runtime reconciles removed registrations and their runs.
        FileRemoteStore::new(self.engine.paths.clone())
            .remove(&args.alias)
            .await
            .map_err(service_error)
    }

    pub(super) async fn remote_list(
        &self,
        _ctx: &OpContext,
        _args: RemoteListArgs,
    ) -> OpResult<Vec<RemoteConfig>> {
        FileRemoteStore::new(self.engine.paths.clone())
            .list()
            .await
            .map_err(service_error)
    }

    pub(super) async fn remote_status(
        &self,
        _ctx: &OpContext,
        _args: RemoteStatusArgs,
    ) -> OpResult<RemoteStatusReport> {
        let store = FileRemoteStore::new(self.engine.paths.clone());
        let config = self
            .engine
            .config_store
            .read()
            .await
            .map_err(service_error)?;
        let inbound = store.read_inbound().await.map_err(service_error)?;
        let mut remotes = Vec::new();
        for entry in store.list().await.map_err(service_error)? {
            remotes.push(
                store
                    .read_status(&entry.alias)
                    .await
                    .map_err(service_error)?
                    .unwrap_or_else(|| offline(entry)),
            );
        }
        #[derive(serde::Deserialize)]
        struct Listener {
            listen_addr: String,
            controller_name: Option<String>,
            pid: Option<u32>,
            #[serde(default)]
            last_session_error: Option<String>,
        }
        let listener: Option<Listener> = orch_infra::storage::fs_utils::read_json(
            &self.engine.paths.remotes_dir().join("_listener.json"),
        )
        .await
        .map_err(service_error)?;
        let listener = listener.filter(|value| value.pid == Some(std::process::id()));
        Ok(RemoteStatusReport {
            listen: listener.as_ref().map(|value| value.listen_addr.clone()),
            listen_configured: config.peer.listen,
            controller: listener
                .as_ref()
                .and_then(|value| value.controller_name.clone()),
            last_session_error: listener.and_then(|value| value.last_session_error),
            inbound,
            remotes,
        })
    }

    pub(super) async fn remote_listen(
        &self,
        _ctx: &OpContext,
        args: RemoteListenArgs,
    ) -> OpResult<()> {
        if !args.enabled && args.addr.is_some() {
            return Err(invalid("remote listen off does not accept an address"));
        }
        let listen = if args.enabled {
            Some(
                args.addr
                    .as_ref()
                    .map_or("127.0.0.1:7690", |value| value.as_str())
                    .parse::<std::net::SocketAddr>()
                    .map_err(|_| invalid("Listen address must be an IP address and port"))?
                    .to_string(),
            )
        } else {
            None
        };
        self.engine
            .config_store
            .set("peer.listen", listen.map_or(Value::Null, Value::String))
            .await
            .map_err(service_error)?;
        self.engine.refresh_config().await.map_err(service_error)?;
        Ok(())
    }

    // Probes execute outside the project permit; they do not mutate project stores.
    pub(super) async fn doctor_run(
        &self,
        _ctx: &OpContext,
        _args: DoctorRunArgs,
    ) -> OpResult<DoctorReport> {
        let report = self.engine.doctor_service.run_all().await;
        Ok(DoctorReport {
            checks: report
                .checks
                .into_iter()
                .map(|check| DoctorCheck {
                    name: check.name,
                    status: match check.status {
                        orch_engine::doctor_service::CheckStatus::Ok => CheckStatus::Ok,
                        orch_engine::doctor_service::CheckStatus::Fail => CheckStatus::Fail,
                        orch_engine::doctor_service::CheckStatus::Skip => CheckStatus::Skip,
                    },
                    detail: check.detail,
                })
                .collect(),
            adapters_ready: report.adapters_ready as u64,
            adapters_total: report.adapters_total as u64,
        })
    }

    pub(super) async fn models_catalog(
        &self,
        _ctx: &OpContext,
        args: ModelsCatalogArgs,
    ) -> OpResult<Vec<AdapterModels>> {
        let adapters = args
            .adapters
            .map(|values| values.into_inner())
            .unwrap_or_else(|| orch_domain::SUPPORTED_ADAPTERS.to_vec());
        let catalog = orch_infra::models::ModelDiscovery::with_runtime(self.diagnostics.clone())
            .load_catalog(&adapters, |_, _| {})
            .await;
        Ok(catalog
            .into_iter()
            .map(|(adapter, models)| AdapterModels {
                adapter,
                models: models
                    .into_iter()
                    .map(|model| ModelOption {
                        value: model.value,
                        label: model.label,
                        hint: model.hint,
                    })
                    .collect(),
            })
            .collect())
    }

    pub(super) async fn catalog_agents(
        &self,
        _ctx: &OpContext,
        _args: CatalogAgentsArgs,
    ) -> OpResult<Vec<AgentShopTemplate>> {
        Ok(orch_domain::AGENT_SHOP_TEMPLATES
            .iter()
            .map(|entry| AgentShopTemplate {
                key: entry.key.into(),
                name: entry.name.into(),
                description: entry.description.into(),
                tier: entry.tier,
                approval_policy: entry.approval_policy,
                skills: entry.skills.iter().map(|value| (*value).into()).collect(),
                role: entry.role.into(),
            })
            .collect())
    }

    pub(super) async fn catalog_templates(
        &self,
        _ctx: &OpContext,
        _args: CatalogTemplatesArgs,
    ) -> OpResult<Vec<OrgTemplate>> {
        org_catalog().await
    }

    pub(super) async fn org_list(
        &self,
        _ctx: &OpContext,
        _args: OrgListArgs,
    ) -> OpResult<Vec<OrgTemplate>> {
        org_catalog().await
    }

    pub(super) async fn org_deploy(
        &self,
        _ctx: &OpContext,
        args: OrgDeployArgs,
    ) -> OpResult<Deployment> {
        let loaded = load_template(&args.selector).await?;
        // The wire field preserves the current CLI --goal argument: a new goal title.
        let deployment = orch_engine::org_templates::deploy_bundle(
            &self.engine,
            &loaded.bundle,
            args.adapter.as_ref().map(|value| value.as_str()),
            args.model.as_ref().map(|value| value.as_str()),
            args.goal_id.as_ref().map(|value| value.as_str()),
            templates::validate_record_size,
        )
        .await
        .map_err(service_error)?;
        Ok(Deployment {
            team: deployment.team,
            agent_ids: deployment.agent_ids,
            goal_id: deployment.goal_id,
        })
    }

    pub(super) async fn org_template_list(
        &self,
        _ctx: &OpContext,
        args: OrgTemplateListArgs,
    ) -> OpResult<Vec<TemplateListEntry>> {
        let root = args
            .template_dir
            .as_ref()
            .map(|path| absolute(path).map(Path::to_path_buf))
            .transpose()?
            .unwrap_or_else(templates::user_root);
        template_list(&root, args.template_dir.is_none()).await
    }

    pub(super) async fn org_template_show(
        &self,
        _ctx: &OpContext,
        args: OrgTemplateShowArgs,
    ) -> OpResult<TemplateView> {
        let loaded = load_template(&args.selector).await?;
        let resolved_agents = if args.resolved {
            let config = self
                .engine
                .config_store
                .read()
                .await
                .map_err(service_error)?;
            Some(
                orch_engine::org_templates::resolve_inputs(
                    &loaded.bundle,
                    &config,
                    None,
                    None,
                    templates::validate_record_size,
                )
                .map_err(service_error)?,
            )
        } else {
            None
        };
        Ok(TemplateView {
            source: source(loaded.source)?,
            bundle: loaded.bundle,
            resolved_agents,
        })
    }

    pub(super) async fn org_template_init(
        &self,
        _ctx: &OpContext,
        args: OrgTemplateInitArgs,
    ) -> OpResult<TemplateInit> {
        validate_id(&args.name).map_err(invalid)?;
        let mut bundle = match &args.from {
            Some(from) => {
                templates::resolve(Some(from), None)
                    .await
                    .map_err(service_error)?
                    .bundle
            }
            None => minimal(&args.name)?,
        };
        bundle.org.name = args.name.to_string();
        let destination = args
            .template_dir
            .as_ref()
            .map(|path| absolute(path).map(Path::to_path_buf))
            .transpose()?
            .unwrap_or_else(|| templates::user_root().join(args.name.as_str()));
        templates::init_bundle(&destination, &bundle)
            .await
            .map_err(service_error)?;
        Ok(TemplateInit {
            name: args.name.into_inner(),
            path: wire_path(destination)?,
        })
    }

    pub(super) async fn org_template_definition_get(
        &self,
        _ctx: &OpContext,
        args: OrgTemplateDefinitionGetArgs,
    ) -> OpResult<DefinitionDocument> {
        let loaded = load_template(&args.selector).await?;
        definition_document(&serde_json::to_value(&loaded.bundle.org).map_err(json_error)?)
    }

    pub(super) async fn org_template_definition_apply(
        &self,
        ctx: &OpContext,
        args: OrgTemplateDefinitionApplyArgs,
    ) -> OpResult<OrgDefinition> {
        Ok(self
            .template_apply(ctx, args.selector, Target::Org, args.definition)
            .await?
            .org)
    }

    pub(super) async fn org_template_agent_list(
        &self,
        _ctx: &OpContext,
        args: OrgTemplateAgentListArgs,
    ) -> OpResult<Vec<AgentDefinition>> {
        Ok(load_template(&args.selector).await?.bundle.agents)
    }

    pub(super) async fn org_template_agent_add(
        &self,
        ctx: &OpContext,
        args: OrgTemplateAgentAddArgs,
    ) -> OpResult<AgentDefinition> {
        let input: Value = read_document(self, ctx, args.input).await?;
        let agent: AgentDefinition =
            serde_json::from_value(input.clone()).map_err(|error| invalid(error.to_string()))?;
        let (root, expected) = writable(&args.selector)?;
        templates::mutate(
            &root,
            expected.as_deref(),
            Target::Agent(agent.name.clone()),
            Mutation::Add(serde_json::to_vec(&input).map_err(json_error)?),
        )
        .await
        .map_err(service_error)?;
        Ok(agent)
    }

    pub(super) async fn org_template_agent_definition_get(
        &self,
        _ctx: &OpContext,
        args: OrgTemplateAgentDefinitionGetArgs,
    ) -> OpResult<DefinitionDocument> {
        let loaded = load_template(&args.selector).await?;
        let agent = loaded
            .bundle
            .agents
            .iter()
            .find(|agent| agent.name == args.agent.as_str())
            .ok_or_else(|| invalid("Template agent not found"))?;
        definition_document(&serde_json::to_value(agent).map_err(json_error)?)
    }

    pub(super) async fn org_template_agent_definition_apply(
        &self,
        ctx: &OpContext,
        args: OrgTemplateAgentDefinitionApplyArgs,
    ) -> OpResult<AgentDefinition> {
        validate_id(&args.agent).map_err(invalid)?;
        let name = args.agent.into_inner();
        let bundle = self
            .template_apply(
                ctx,
                args.selector,
                Target::Agent(name.clone()),
                args.definition,
            )
            .await?;
        bundle
            .agents
            .into_iter()
            .find(|agent| agent.name == name)
            .ok_or_else(|| invalid("Template agent name is immutable"))
    }

    pub(super) async fn org_template_agent_delete(
        &self,
        _ctx: &OpContext,
        args: OrgTemplateAgentDeleteArgs,
    ) -> OpResult<TemplateAgentDeleted> {
        validate_id(&args.agent).map_err(invalid)?;
        let (root, expected) = writable(&args.selector)?;
        let name = args.agent.into_inner();
        templates::mutate(
            &root,
            expected.as_deref(),
            Target::Agent(name.clone()),
            Mutation::Delete,
        )
        .await
        .map_err(service_error)?;
        Ok(TemplateAgentDeleted { deleted: name })
    }

    pub(super) async fn update_check(
        &self,
        _ctx: &OpContext,
        _args: UpdateCheckArgs,
    ) -> OpResult<UpdateInfo> {
        Ok(UpdateInfo {
            current_version: orch_proto::BINARY_VERSION.into(),
            instructions: "This build does not update itself.\nTo install the latest version:\n  Homebrew: brew upgrade r-orch\n  Source: git pull && cargo install --path crates/orch-cli\n--check: no registry query is made.".into(),
        })
    }

    async fn template_apply(
        &self,
        ctx: &OpContext,
        selector: TemplateSelector,
        target: Target,
        definition: DefinitionInput,
    ) -> OpResult<Bundle> {
        let (root, expected) = writable(&selector)?;
        let locked = LockedTemplate::open(&root, expected.as_deref())
            .await
            .map_err(service_error)?;
        let current = if definition.kind == MutationKind::Patch || definition.base_digest.is_some()
        {
            let value = locked.document(&target).await.map_err(service_error)?;
            if definition.base_digest.is_some() {
                canonical_definition(value, &target)?
            } else {
                value
            }
        } else {
            Value::Null
        };
        let input = read_document(self, ctx, definition.input.clone()).await?;
        validate_fields(&input, &target)?;
        if definition.kind == MutationKind::Update {
            reject_nulls(&input)?;
        }
        let candidate = definition_candidate(&current, &input, &definition)?;
        reject_nulls(&candidate)?;
        locked
            .publish(
                target,
                Mutation::Replace(serde_json::to_vec(&candidate).map_err(json_error)?),
            )
            .await
            .map_err(service_error)
    }
}

fn derive_timings(
    runs: &[orch_domain::Run],
    tasks: &[orch_domain::Task],
    goals: &[orch_domain::Goal],
    state: &orch_domain::OrchestratorState,
    now: i64,
) -> TimingSnapshot {
    let mut earliest = std::collections::BTreeMap::<&str, i64>::new();
    for (task, started) in runs
        .iter()
        .map(|run| (run.task_id.as_str(), run.started_at.as_str()))
        .chain(
            state
                .running
                .iter()
                .map(|(task, run)| (task.as_str(), run.started_at.as_str())),
        )
    {
        if let Some(start) = orch_core::clock::epoch_ms_from_iso(started) {
            earliest
                .entry(task)
                .and_modify(|old| *old = (*old).min(start))
                .or_insert(start);
        }
    }
    TimingSnapshot {
        tasks: tasks
            .iter()
            .map(|task| {
                let start = earliest.get(task.id.as_str()).copied();
                let terminal = matches!(
                    task.status,
                    orch_domain::TaskStatus::Done
                        | orch_domain::TaskStatus::Failed
                        | orch_domain::TaskStatus::Cancelled
                );
                (
                    task.id.clone(),
                    timing(start, terminal, Some(&task.updated_at), now),
                )
            })
            .collect(),
        goals: goals
            .iter()
            .map(|goal| {
                let start = orch_core::clock::epoch_ms_from_iso(&goal.created_at);
                (
                    goal.id.clone(),
                    timing(
                        start,
                        orch_domain::is_goal_terminal(goal.status),
                        goal.updated_at.as_deref(),
                        now,
                    ),
                )
            })
            .collect(),
        sampled_at_ms: now,
    }
}

fn timing(start: Option<i64>, terminal: bool, updated: Option<&str>, now: i64) -> EntityTiming {
    let end = if terminal {
        updated.and_then(orch_core::clock::epoch_ms_from_iso)
    } else {
        Some(now)
    };
    EntityTiming {
        started_at_ms: start,
        elapsed_ms: start
            .zip(end)
            .map(|(start, end)| end.saturating_sub(start).max(0)),
    }
}

fn canonical_definition(value: Value, target: &Target) -> OpResult<Value> {
    match target {
        Target::Org => serde_json::to_value(
            serde_json::from_value::<OrgDefinition>(value)
                .map_err(|error| invalid(error.to_string()))?,
        )
        .map_err(json_error),
        Target::Agent(_) => serde_json::to_value(
            serde_json::from_value::<AgentDefinition>(value)
                .map_err(|error| invalid(error.to_string()))?,
        )
        .map_err(json_error),
    }
}

fn invalid(message: impl Into<String>) -> ErrorBody {
    service_error(Error::invalid_arguments(message))
}

fn json_error(error: serde_json::Error) -> ErrorBody {
    ErrorBody::new(
        ErrorCode::General,
        &format!("Could not encode operation result: {error}"),
        Execution::Unknown,
    )
}

fn wire_path(path: PathBuf) -> OpResult<WirePath> {
    WirePath::new(path).map_err(|error| invalid(error.to_string()))
}

fn absolute(path: &WirePath) -> OpResult<&Path> {
    if !path.as_path().is_absolute() {
        return Err(invalid("Template paths must be absolute"));
    }
    Ok(path.as_path())
}

async fn load_template(selector: &TemplateSelector) -> OpResult<templates::Loaded> {
    let path = selector.template_dir.as_ref().map(absolute).transpose()?;
    templates::resolve(selector.name.as_ref().map(|name| name.as_str()), path)
        .await
        .map_err(service_error)
}

fn writable(selector: &TemplateSelector) -> OpResult<(PathBuf, Option<String>)> {
    match (&selector.name, &selector.template_dir) {
        (None, Some(path)) => Ok((absolute(path)?.to_path_buf(), None)),
        (Some(name), None) => {
            let (qualifier, key) = name
                .split_once(':')
                .map_or((None, name.as_str()), |(q, k)| (Some(q), k));
            validate_id(key).map_err(invalid)?;
            if qualifier.is_some_and(|value| value != "user" && value != "builtin") {
                return Err(invalid("Unknown source qualifier"));
            }
            if qualifier == Some("builtin")
                || (qualifier.is_none()
                    && !templates::user_template_exists(key).map_err(service_error)?)
            {
                return Err(invalid(format!(
                    "Builtins are read-only; run orch org template init {key}-copy --from builtin:{key}"
                )));
            }
            Ok((templates::user_root().join(key), Some(key.into())))
        }
        _ => Err(invalid("Select exactly one template name or template_dir")),
    }
}

fn source(source: Source) -> OpResult<TemplateSource> {
    Ok(match source {
        Source::Builtin => TemplateSource::Builtin,
        Source::Legacy => TemplateSource::Legacy,
        Source::User { path } => TemplateSource::User {
            path: wire_path(path)?,
        },
        Source::Directory { path } => TemplateSource::Directory {
            path: wire_path(path)?,
        },
    })
}

async fn org_catalog() -> OpResult<Vec<OrgTemplate>> {
    let mut entries: Vec<_> = orch_domain::ORG_TEMPLATES
        .iter()
        .map(|entry| OrgTemplate {
            key: entry.key.into(),
            name: entry.name.into(),
            description: entry.description.into(),
            agents: entry
                .agents
                .iter()
                .map(|agent| OrgAgentEntry {
                    shop_key: agent.shop_key.into(),
                    name: agent.name.into(),
                })
                .collect(),
            lead_index: entry.lead_index as u64,
        })
        .collect();
    let builtin = templates::builtin("a-team").await.map_err(service_error)?;
    entries.push(OrgTemplate {
        key: "a-team".into(),
        name: "a-team".into(),
        description: builtin.org.description,
        agents: builtin
            .agents
            .iter()
            .map(|agent| OrgAgentEntry {
                shop_key: format!("a-team/{}", agent.name),
                name: agent.name.clone(),
            })
            .collect(),
        lead_index: builtin
            .agents
            .iter()
            .position(|agent| agent.name == builtin.org.lead)
            .ok_or_else(|| invalid("Builtin template lead is missing"))? as u64,
    });
    Ok(entries)
}

fn minimal(name: &str) -> OpResult<Bundle> {
    Ok(Bundle {
        org: OrgDefinition { schema_version: 1, name: name.into(), description: String::new(), lead: "lead".into(), rules: Vec::new(), defaults: None },
        agents: vec![serde_json::from_value(serde_json::json!({"name":"lead","role":"Coordinate the team and report outcomes."})).map_err(json_error)?],
        rules: Default::default(),
    })
}

async fn template_list(root: &Path, include_builtins: bool) -> OpResult<Vec<TemplateListEntry>> {
    let mut entries = Vec::new();
    let mut users = BTreeSet::new();
    match std::fs::read_dir(root) {
        Ok(dirs) => {
            for entry in dirs {
                let entry = entry
                    .map_err(|error| service_error(Error::io("List templates", root, error)))?;
                let name = entry.file_name().to_string_lossy().into_owned();
                if name.starts_with('.') {
                    continue;
                }
                users.insert(name.clone());
                let error = match validate_id(&name) {
                    Err(error) => Some(error),
                    Ok(()) => templates::load_directory(&entry.path(), Some(&name))
                        .await
                        .err()
                        .map(|error| error.to_string()),
                };
                entries.push(TemplateListEntry {
                    name,
                    source: TemplateSource::User {
                        path: wire_path(entry.path())?,
                    },
                    shadowed: false,
                    error,
                });
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(service_error(Error::io("List templates", root, error))),
    }
    if include_builtins {
        for name in std::iter::once("a-team").chain(
            orch_domain::ORG_TEMPLATES
                .iter()
                .map(|template| template.key),
        ) {
            entries.push(TemplateListEntry {
                name: name.into(),
                source: if name == "a-team" {
                    TemplateSource::Builtin
                } else {
                    TemplateSource::Legacy
                },
                shadowed: users.contains(name),
                error: None,
            });
        }
    }
    entries.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(entries)
}

fn normalize_addr(addr: &str) -> orch_core::Result<String> {
    if addr.is_empty() || addr.chars().any(char::is_whitespace) || addr.contains('/') {
        return Err(Error::invalid_arguments(
            "Remote address must be host[:port]",
        ));
    }
    if addr.starts_with('[') {
        return if addr.parse::<std::net::SocketAddr>().is_ok() {
            Ok(addr.into())
        } else {
            Err(Error::invalid_arguments(
                "IPv6 remote addresses require [address]:port",
            ))
        };
    }
    if let Some((host, port)) = addr.split_once(':') {
        if host.is_empty() || port.parse::<u16>().is_err() {
            return Err(Error::invalid_arguments(
                "Remote address must be host[:port]",
            ));
        }
        Ok(addr.into())
    } else {
        Ok(format!("{addr}:7690"))
    }
}

fn offline(entry: RemoteConfig) -> RemoteStatus {
    RemoteStatus {
        alias: entry.alias,
        addr: entry.addr,
        online: false,
        last_seen: None,
        last_error: None,
        peer_name: None,
        instance_id: None,
        os: None,
        agents: Vec::new(),
        peer_version: None,
        link_state: orch_domain::LinkState::Connecting,
        next_retry_at: None,
        attempts: 0,
    }
}

const DEFAULT_FIELDS: &[&str] = &[
    "adapter",
    "model",
    "command",
    "effort",
    "approval_policy",
    "max_turns",
    "timeout_ms",
    "stall_timeout_ms",
    "env",
    "system_prompt",
    "workspace_mode",
    "skills",
];
fn validate_fields(input: &Value, target: &Target) -> OpResult<()> {
    let fields = input
        .as_object()
        .ok_or_else(|| invalid("definition: expected an object"))?;
    for (key, value) in fields {
        let allowed = match target {
            Target::Org => [
                "schema_version",
                "name",
                "lead",
                "description",
                "rules",
                "defaults",
            ]
            .contains(&key.as_str()),
            Target::Agent(_) => {
                ["name", "role", "description", "rules"].contains(&key.as_str())
                    || DEFAULT_FIELDS.contains(&key.as_str())
            }
        };
        if !allowed {
            return Err(invalid(format!("{key}: unknown field")));
        }
        if matches!(target, Target::Org)
            && key == "defaults"
            && let Some(defaults) = value.as_object()
        {
            for field in defaults.keys() {
                if !DEFAULT_FIELDS.contains(&field.as_str()) {
                    return Err(invalid(format!("defaults.{field}: unknown field")));
                }
            }
        }
    }
    Ok(())
}

fn reject_nulls(value: &Value) -> OpResult<()> {
    match value {
        Value::Null => Err(invalid("definition: explicit null is not allowed")),
        Value::Object(fields) => {
            for value in fields.values() {
                reject_nulls(value)?;
            }
            Ok(())
        }
        Value::Array(values) => {
            for value in values {
                reject_nulls(value)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_addresses_validate_before_publication() {
        assert_eq!(normalize_addr("worker").unwrap(), "worker:7690");
        assert_eq!(normalize_addr("worker:55").unwrap(), "worker:55");
        assert_eq!(normalize_addr("[::1]:7690").unwrap(), "[::1]:7690");
        for invalid in ["", "a/b", "a b", "host:abc", "::1", "[::1]"] {
            assert!(normalize_addr(invalid).is_err(), "{invalid}");
        }
    }

    #[test]
    fn template_selection_rejects_relative_paths_and_builtin_writes() {
        let selector = TemplateSelector {
            name: None,
            template_dir: Some(WirePath::new("relative").unwrap()),
        };
        assert!(writable(&selector).is_err());
        let selector = TemplateSelector {
            name: Some("builtin:a-team".try_into().unwrap()),
            template_dir: None,
        };
        assert!(writable(&selector).is_err());
    }

    #[test]
    fn template_patches_reject_unknown_nulls_before_merge() {
        assert!(validate_fields(&serde_json::json!({"surprise": null}), &Target::Org).is_err());
        assert!(
            validate_fields(
                &serde_json::json!({"defaults": {"surprise": null}}),
                &Target::Org
            )
            .is_err()
        );
        assert!(
            validate_fields(
                &serde_json::json!({"role": "new"}),
                &Target::Agent("lead".into())
            )
            .is_ok()
        );
        assert!(reject_nulls(&serde_json::json!({"role": null})).is_err());
    }

    #[tokio::test]
    async fn catalog_contains_the_portable_formation_and_every_legacy_team() {
        let entries = org_catalog().await.unwrap();
        assert_eq!(entries.len(), orch_domain::ORG_TEMPLATES.len() + 1);
        for entry in entries {
            assert!(entry.lead_index < entry.agents.len() as u64);
        }
    }
    #[test]
    fn template_digest_uses_the_same_defaults_as_the_read_projection() {
        let raw = serde_json::json!({"schema_version":1,"name":"one","lead":"lead"});
        let canonical = canonical_definition(raw, &Target::Org).unwrap();
        assert_eq!(canonical["description"], "");
        assert_eq!(canonical["rules"], serde_json::json!([]));
        let read = definition_document(&canonical).unwrap();
        let again =
            definition_document(&canonical_definition(canonical, &Target::Org).unwrap()).unwrap();
        assert_eq!(read.digest, again.digest);
    }

    #[test]
    fn timing_never_fabricates_a_start_or_terminal_end() {
        assert_eq!(timing(None, false, None, 5000).elapsed_ms, None);
        assert_eq!(
            timing(Some(1000), true, Some("malformed"), 5000).elapsed_ms,
            None
        );
        assert_eq!(timing(Some(1000), false, None, 5000).elapsed_ms, Some(4000));
        assert_eq!(timing(Some(1000), false, None, 500).elapsed_ms, Some(0));
    }

    #[tokio::test]
    async fn template_listing_surfaces_invalid_members_and_ignores_dotfiles() {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "orch-daemon-peripheral-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
        ));
        std::fs::create_dir(&path).unwrap();
        struct Cleanup(PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let _cleanup = Cleanup(path.clone());
        std::fs::write(path.join(".ignored"), "not a bundle").unwrap();
        templates::init_bundle(&path.join("valid"), &minimal("valid").unwrap())
            .await
            .unwrap();
        std::fs::create_dir(path.join("broken")).unwrap();
        let entries = template_list(&path, false).await.unwrap();
        assert_eq!(
            entries
                .iter()
                .map(|entry| entry.name.as_str())
                .collect::<Vec<_>>(),
            vec!["broken", "valid"]
        );
        assert!(entries[0].error.is_some());
        assert!(entries[1].error.is_none());
    }
    #[tokio::test]
    async fn snapshot_command_runner_preserves_review_output_and_overflow_failure() {
        use orch_engine::review_runner::{CommandRunner, MAX_OUTPUT_BYTES};
        let runner = SnapshotCommandRunner(std::sync::Arc::new(
            orch_infra::models::SnapshotDiscoveryRuntime::new(orch_infra::process::EnvSnapshot {
                base: vec![("SNAPSHOT_VALUE".into(), "base".into())],
                forced: vec![("SNAPSHOT_VALUE".into(), "trusted".into())],
            }),
        ));
        let temp = tempfile::tempdir().unwrap();
        let timeout = std::time::Duration::from_secs(10);
        let output = runner
            .run(
                "/bin/sh",
                &[
                    "-c",
                    "printf '%s' \"$SNAPSHOT_VALUE\"; printf caller-detail >&2",
                ],
                temp.path(),
                timeout,
            )
            .await;
        assert!(!output.failed);
        assert_eq!(output.stdout, "trusted");
        assert_eq!(output.stderr, "caller-detail");
        let output = runner.run("/bin/sh", &["-c", "/usr/bin/yes o | /usr/bin/head -c 12582912; printf REVIEW_STDERR_CANARY >&2; /usr/bin/yes e | /usr/bin/head -c 12582912 >&2"], temp.path(), timeout).await;
        assert!(
            output.failed,
            "review output overflow must remain a failure"
        );
        assert_eq!(output.stdout.len(), MAX_OUTPUT_BYTES);
        assert_eq!(output.stderr.len(), MAX_OUTPUT_BYTES);
        assert!(output.stderr.contains("REVIEW_STDERR_CANARY"));
    }

    async fn fixture() -> (TestRoot, ServiceHandler, OpContext) {
        let root = TestRoot::new();
        std::fs::create_dir(root.0.join(".orchestry")).unwrap();
        let engine = std::sync::Arc::new(
            orch_engine::build_full_with(
                &root.0,
                orch_engine::EngineSeams {
                    global_config_path: Some(root.0.join("global.yml")),
                    ..Default::default()
                },
            )
            .await
            .unwrap(),
        );
        let coordinator = std::sync::Arc::new(crate::coordinator::ProjectCoordinator::new(
            engine.events.operation_boundary(),
        ));
        let handler = ServiceHandler::new(engine, coordinator);
        let ctx = OpContext::new(orch_proto::handshake::Session {
            caller: orch_proto::handshake::Caller {
                kind: orch_proto::handshake::CallerKind::Cli,
                agent_id: None,
                pid: None,
            },
            mode: orch_proto::handshake::SessionMode::Normal,
        });
        (root, handler, ctx)
    }

    struct TestRoot(PathBuf);
    impl TestRoot {
        fn new() -> Self {
            static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "orch-peripheral-op-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
            ));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for TestRoot {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[tokio::test]
    async fn remote_handlers_validate_and_round_trip_registry_and_listen_preferences() {
        use super::super::Handler;
        let (_root, handler, ctx) = fixture().await;
        let added = Handler::remote_add(
            &handler,
            &ctx,
            RemoteAddArgs {
                alias: "worker".try_into().unwrap(),
                addr: "host".try_into().unwrap(),
            },
        )
        .await
        .unwrap();
        assert_eq!(added.addr, "host:7690");
        assert_eq!(
            Handler::remote_list(&handler, &ctx, RemoteListArgs {})
                .await
                .unwrap(),
            vec![added]
        );
        Handler::remote_listen(
            &handler,
            &ctx,
            RemoteListenArgs {
                enabled: true,
                addr: Some("127.0.0.1:7777".try_into().unwrap()),
            },
        )
        .await
        .unwrap();
        let status = Handler::remote_status(&handler, &ctx, RemoteStatusArgs {})
            .await
            .unwrap();
        assert_eq!(status.listen_configured.as_deref(), Some("127.0.0.1:7777"));
        assert_eq!(status.listen, None, "configured is not an active listener");
        assert!(!status.remotes[0].online);
        assert!(
            Handler::remote_listen(
                &handler,
                &ctx,
                RemoteListenArgs {
                    enabled: false,
                    addr: Some("127.0.0.1:7777".try_into().unwrap())
                }
            )
            .await
            .is_err()
        );
        Handler::remote_remove(
            &handler,
            &ctx,
            RemoteRemoveArgs {
                alias: "worker".try_into().unwrap(),
            },
        )
        .await
        .unwrap();
        assert!(
            Handler::remote_list(&handler, &ctx, RemoteListArgs {})
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn template_handler_round_trip_rejects_an_editor_with_a_stale_base() {
        use super::super::Handler;
        let (root, handler, ctx) = fixture().await;
        let path = WirePath::new(root.0.join("one")).unwrap();
        Handler::org_template_init(
            &handler,
            &ctx,
            OrgTemplateInitArgs {
                name: "one".try_into().unwrap(),
                from: None,
                template_dir: Some(path.clone()),
            },
        )
        .await
        .unwrap();
        let selector = TemplateSelector {
            name: None,
            template_dir: Some(path),
        };
        let base = Handler::org_template_definition_get(
            &handler,
            &ctx,
            OrgTemplateDefinitionGetArgs {
                selector: selector.clone(),
            },
        )
        .await
        .unwrap();
        let patch = |description: &str| DefinitionInput {
            input: DocumentInput {
                document: Some(
                    format!("{{\"description\":\"{description}\"}}")
                        .try_into()
                        .unwrap(),
                ),
                input_transfer: None,
                format: DocumentFormat::Json,
            },
            kind: MutationKind::Patch,
            base_digest: Some(base.digest.clone()),
        };
        let updated = Handler::org_template_definition_apply(
            &handler,
            &ctx,
            OrgTemplateDefinitionApplyArgs {
                selector: selector.clone(),
                definition: patch("fresh"),
            },
        )
        .await
        .unwrap();
        assert_eq!(updated.description, "fresh");
        let error = Handler::org_template_definition_apply(
            &handler,
            &ctx,
            OrgTemplateDefinitionApplyArgs {
                selector: selector.clone(),
                definition: patch("stale"),
            },
        )
        .await
        .unwrap_err();
        assert_eq!(error.code, ErrorCode::Conflict);
        let agents =
            Handler::org_template_agent_list(&handler, &ctx, OrgTemplateAgentListArgs { selector })
                .await
                .unwrap();
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].name, "lead");
    }

    #[tokio::test]
    async fn history_uses_finished_runs_and_preserves_event_text() {
        use super::super::Handler;
        let (_root, handler, ctx) = fixture().await;
        let run = handler
            .engine
            .run_service
            .create(orch_engine::run_service::CreateRunInput {
                task_id: "task".into(),
                agent_id: "agent".into(),
                attempt: 1,
                prompt: "p".into(),
                workspace_path: "/workspace".into(),
            })
            .await
            .unwrap();
        assert!(
            Handler::run_history(&handler, &ctx, RunHistoryArgs {})
                .await
                .unwrap()
                .batches
                .is_empty()
        );
        let mut finished = run.clone();
        finished.status = orch_domain::RunStatus::Succeeded;
        handler.engine.runs.save(&finished).await.unwrap();
        handler
            .engine
            .runs
            .append_event(
                &run.id,
                &orch_domain::RunEvent {
                    timestamp: handler.engine.clock.now_iso(),
                    event_type: orch_domain::RunEventType::AgentOutput,
                    data: Value::String("literal output".into()),
                },
            )
            .await
            .unwrap();
        let history = Handler::run_history(&handler, &ctx, RunHistoryArgs {})
            .await
            .unwrap();
        assert_eq!(history.batches.len(), 1);
        assert_eq!(history.batches[0][0].data, "literal output");
        assert_eq!(history.batches[0][0].run_id, run.id);
    }
    #[tokio::test]
    async fn catalogue_handlers_keep_empty_model_selection_offline_and_return_install_instructions()
    {
        use super::super::Handler;
        let (_root, handler, ctx) = fixture().await;
        let models = Handler::models_catalog(
            &handler,
            &ctx,
            ModelsCatalogArgs {
                adapters: Some(Default::default()),
            },
        )
        .await
        .unwrap();
        assert!(models.is_empty());
        let agents = Handler::catalog_agents(&handler, &ctx, CatalogAgentsArgs {})
            .await
            .unwrap();
        assert_eq!(agents.len(), orch_domain::AGENT_SHOP_TEMPLATES.len());
        let update = Handler::update_check(&handler, &ctx, UpdateCheckArgs {})
            .await
            .unwrap();
        assert_eq!(update.current_version, orch_proto::BINARY_VERSION);
        assert!(update.instructions.contains("brew upgrade r-orch"));
        assert!(update.instructions.contains("no registry query"));
    }

    #[test]
    fn goal_timing_tracks_creation_and_terminal_metadata_without_a_run() {
        let start = "2026-01-01T00:00:00.000Z";
        let goal = orch_domain::Goal {
            id: "goal".into(),
            title: "goal".into(),
            description: String::new(),
            status: orch_domain::GoalStatus::Achieved,
            assignee: None,
            created_at: start.into(),
            updated_at: Some("2026-01-01T00:00:03.000Z".into()),
        };
        let snapshot = derive_timings(&[], &[], &[goal], &Default::default(), 0);
        assert_eq!(
            snapshot.goals["goal"].started_at_ms,
            orch_core::clock::epoch_ms_from_iso(start)
        );
        assert_eq!(snapshot.goals["goal"].elapsed_ms, Some(3000));
    }
}
