//! Authored definitions and configuration operations.

use super::{OpContext, ServiceHandler, service_error};
use crate::coordinator::OpResult;
use base64::Engine as _;
use orch_core::Error;
use orch_domain::{CreateAgentInput, GlobalConfig, PromptMode, Task};
use orch_engine::agent_service::{AuthoredAgent, UpdateAgentFields};
use orch_engine::prompt_mode::DefinitionLock;
use orch_engine::task_service::UpdateTaskFields;
use orch_infra::storage::{FileConfigStore, fs_utils};
use orch_proto::bounded::Id;
use orch_proto::error::{ErrorBody, ErrorCode, Execution};
use orch_proto::ops::args::*;
use orch_proto::ops::results::{self, DefinitionDocument};
use serde::de::{DeserializeSeed, MapAccess, SeqAccess, Visitor};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::fmt;
use std::path::Path;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

impl ServiceHandler {
    pub(super) async fn task_definition_get(
        &self,
        _ctx: &OpContext,
        args: TaskDefinitionGetArgs,
    ) -> OpResult<DefinitionDocument> {
        let task = self
            .engine
            .task_service
            .get(args.id.as_str())
            .await
            .map_err(service_error)?;
        definition_document(&task_authored(&task))
    }

    pub(super) async fn task_definition_apply(
        &self,
        ctx: &OpContext,
        args: TaskDefinitionApplyArgs,
    ) -> OpResult<Task> {
        let input = read_document(self, ctx, args.definition.input.clone()).await?;
        let _lock = fs_utils::mutation_lock(&self.engine.paths.root().join(".definition.lock"))
            .await
            .map_err(service_error)?;
        let current = self
            .engine
            .task_service
            .get(args.id.as_str())
            .await
            .map_err(service_error)?;
        let authored = task_authored(&current);
        check_digest(&authored, &args.definition)?;
        let full =
            serde_json::to_value(&current).map_err(|_| invalid("task could not be encoded"))?;
        let candidate = task_candidate(&full, &input, args.definition.kind)?;
        self.engine
            .task_service
            .update_definition(
                args.id.as_str(),
                UpdateTaskFields {
                    title: Some(
                        candidate["title"]
                            .as_str()
                            .expect("validated title")
                            .to_owned(),
                    ),
                    description: Some(
                        candidate["description"]
                            .as_str()
                            .expect("validated description")
                            .to_owned(),
                    ),
                    priority: candidate["priority"].as_i64(),
                    labels: Some(
                        serde_json::from_value(candidate["labels"].clone())
                            .expect("validated labels"),
                    ),
                    attachments: None,
                },
            )
            .await
            .map_err(service_error)
    }

    pub(super) async fn agent_create(
        &self,
        _ctx: &OpContext,
        args: AgentCreateArgs,
    ) -> OpResult<results::AuthoredAgent> {
        let lock = DefinitionLock::acquire(self.engine.paths.project_root())
            .await
            .map_err(service_error)?;
        let input = CreateAgentInput {
            name: args.name.into_inner(),
            adapter: args.adapter.into_inner(),
            role: args.role.map(|v| v.into_inner()),
            command: args.command.map(|v| v.into_inner()),
            model: args.model.map(|v| v.into_inner()),
            effort: args.effort,
            approval_policy: args.approval_policy,
            max_turns: args.max_turns,
            timeout_ms: args.timeout_ms,
            stall_timeout_ms: args.stall_timeout_ms,
            env: args.env.map(|entries| {
                entries
                    .into_inner()
                    .into_iter()
                    .map(|e| (e.name.into_inner(), e.value.into_inner()))
                    .collect()
            }),
            system_prompt: args.system_prompt.map(|v| v.into_inner()),
            workspace_mode: args.workspace_mode,
            skills: args
                .skills
                .map(|v| v.into_inner().into_iter().map(|v| v.into_inner()).collect()),
            prompt_mode: args.prompt_mode,
            role_summary: args.role_summary.map(|v| v.into_inner()),
        };
        self.engine
            .agent_service
            .create_locked(&lock, input)
            .await
            .map(authored_agent)
            .map_err(service_error)
    }

    pub(super) async fn agent_update_fields(
        &self,
        _ctx: &OpContext,
        args: AgentUpdateFieldsArgs,
    ) -> OpResult<results::AuthoredAgent> {
        let lock = DefinitionLock::acquire(self.engine.paths.project_root())
            .await
            .map_err(service_error)?;
        let fields = UpdateAgentFields {
            name: args.name.map(|v| v.into_inner()),
            adapter: args.adapter.map(|v| v.into_inner()),
            role: args.role.map(|v| v.into_inner()),
            command: args.command.map(|v| v.into_inner()),
            model: args.model.map(|p| match p {
                FieldPatch::Set(v) => v.into_inner(),
                FieldPatch::Clear => String::new(),
            }),
            effort: args.effort.map(field_patch),
            approval_policy: args.approval_policy,
            prompt_mode: args.prompt_mode.map(field_patch),
            role_summary: args
                .role_summary
                .map(|p| field_patch(p).map(|v| v.into_inner())),
        };
        self.engine
            .agent_service
            .update_locked(&lock, args.id.as_str(), fields)
            .await
            .map(authored_agent)
            .map_err(service_error)
    }

    pub(super) async fn agent_definition_get(
        &self,
        _ctx: &OpContext,
        args: AgentDefinitionGetArgs,
    ) -> OpResult<DefinitionDocument> {
        let agent = self
            .engine
            .agent_service
            .get(args.id.as_str())
            .await
            .map_err(service_error)?;
        definition_document(&agent_authored(&agent))
    }

    pub(super) async fn agent_definition_apply(
        &self,
        ctx: &OpContext,
        args: AgentDefinitionApplyArgs,
    ) -> OpResult<results::AuthoredAgent> {
        if args.id.as_str().contains('@') {
            return Err(invalid("id: remote agents cannot be mutated"));
        }
        let input = read_document(self, ctx, args.definition.input.clone()).await?;
        let lock = DefinitionLock::acquire(self.engine.paths.project_root())
            .await
            .map_err(service_error)?;
        let current = self
            .engine
            .agent_service
            .get(args.id.as_str())
            .await
            .map_err(service_error)?;
        let authored = agent_authored(&current);
        check_digest(&authored, &args.definition)?;
        let full =
            serde_json::to_value(&current).map_err(|_| invalid("agent could not be encoded"))?;
        let candidate = agent_candidate(&full, &input, args.definition.kind)?;
        let config: orch_domain::AgentConfig = serde_json::from_value(candidate["config"].clone())
            .map_err(|_| invalid("config: invalid definition"))?;
        let mut size_candidate = current;
        size_candidate.name = candidate["name"]
            .as_str()
            .expect("validated name")
            .to_owned();
        size_candidate.adapter = candidate["adapter"]
            .as_str()
            .expect("validated adapter")
            .to_owned();
        size_candidate.role = candidate
            .get("role")
            .and_then(Value::as_str)
            .map(str::to_owned);
        size_candidate.config = config.clone();
        fs_utils::check_definition_write_size(&size_candidate).map_err(service_error)?;
        self.engine
            .agent_service
            .replace_definition_locked(
                &lock,
                args.id.as_str(),
                size_candidate.name,
                size_candidate.adapter,
                size_candidate.role,
                config,
            )
            .await
            .map(authored_agent)
            .map_err(service_error)
    }

    pub(super) async fn config_get(
        &self,
        _ctx: &OpContext,
        args: ConfigGetArgs,
    ) -> OpResult<results::ConfigValue> {
        let value = self
            .engine
            .config_store
            .get(args.key.as_str())
            .await
            .map_err(service_error)?;
        Ok(results::ConfigValue {
            key: args.key.into_inner(),
            value,
        })
    }

    pub(super) async fn config_set(
        &self,
        _ctx: &OpContext,
        args: ConfigSetArgs,
    ) -> OpResult<results::ConfigValue> {
        let parsed = serde_json::from_str(args.value.as_str())
            .unwrap_or_else(|_| Value::String(args.value.into_inner()));
        let _lock = fs_utils::mutation_lock(&self.engine.paths.root().join(".definition.lock"))
            .await
            .map_err(service_error)?;
        let store = FileConfigStore::new(self.engine.paths.clone());
        let mut candidate = store.read_merged().await.map_err(service_error)?;
        orch_domain::set_by_path(&mut candidate, args.key.as_str(), parsed.clone());
        validate_prompt_candidate(self, &candidate).await?;
        self.engine
            .config_store
            .set(args.key.as_str(), parsed.clone())
            .await
            .map_err(service_error)?;
        self.engine.refresh_config().await.map_err(service_error)?;
        Ok(results::ConfigValue {
            key: args.key.into_inner(),
            value: Some(parsed),
        })
    }

    pub(super) async fn config_definition_get(
        &self,
        _ctx: &OpContext,
        _args: ConfigDefinitionGetArgs,
    ) -> OpResult<DefinitionDocument> {
        let raw = FileConfigStore::new(self.engine.paths.clone())
            .read_raw()
            .await
            .map_err(service_error)?;
        definition_document(&raw)
    }

    pub(super) async fn config_definition_apply(
        &self,
        ctx: &OpContext,
        args: ConfigDefinitionApplyArgs,
    ) -> OpResult<DefinitionDocument> {
        let input = read_document(self, ctx, args.definition.input.clone()).await?;
        let _lock = fs_utils::mutation_lock(&self.engine.paths.root().join(".definition.lock"))
            .await
            .map_err(service_error)?;
        let store = FileConfigStore::new(self.engine.paths.clone());
        let current = store.read_raw().await.map_err(service_error)?;
        let candidate = definition_candidate(&current, &input, &args.definition)?;
        let complete = args.definition.kind == MutationKind::Update;
        orch_infra::storage::config::validate_raw_config(&candidate, complete)
            .map_err(service_error)?;
        validate_prompt_candidate(self, &candidate).await?;
        if candidate != current {
            store
                .write_raw(&candidate, complete)
                .await
                .map_err(service_error)?;
        }
        self.engine.refresh_config().await.map_err(service_error)?;
        definition_document(&candidate)
    }

    pub(super) async fn config_global_get(
        &self,
        _ctx: &OpContext,
        args: ConfigGlobalGetArgs,
    ) -> OpResult<results::ConfigValue> {
        let config = self
            .engine
            .global_config_store
            .read()
            .await
            .map_err(service_error)?;
        Ok(results::ConfigValue {
            value: global_value(&config, args.key.as_str()),
            key: args.key.into_inner(),
        })
    }

    pub(super) async fn config_global_set(
        &self,
        _ctx: &OpContext,
        args: ConfigGlobalSetArgs,
    ) -> OpResult<results::ConfigValue> {
        let mut config = self
            .engine
            .global_config_store
            .read()
            .await
            .map_err(service_error)?;
        match args.key.as_str() {
            "activity_filter" => {
                validate_activity_filter(args.value.as_str())?;
                config.tui.activity_filter = args.value.into_inner();
            }
            "palette" => {
                config.tui.palette = args
                    .value
                    .as_str()
                    .to_lowercase()
                    .parse()
                    .map_err(|_| invalid("palette: unknown palette"))?;
            }
            _ => return Err(invalid("unknown global config key")),
        }
        self.engine
            .global_config_store
            .write(&config)
            .await
            .map_err(service_error)?;
        self.engine.refresh_config().await.map_err(service_error)?;
        Ok(results::ConfigValue {
            value: global_value(&config, args.key.as_str()),
            key: args.key.into_inner(),
        })
    }

    pub(super) async fn config_global_show(
        &self,
        _ctx: &OpContext,
        _args: ConfigGlobalShowArgs,
    ) -> OpResult<GlobalConfig> {
        self.engine
            .global_config_store
            .read()
            .await
            .map_err(service_error)
    }

    pub(super) async fn ui_preferences_patch(
        &self,
        _ctx: &OpContext,
        args: UiPreferencesPatchArgs,
    ) -> OpResult<GlobalConfig> {
        let mut config = self
            .engine
            .global_config_store
            .read()
            .await
            .map_err(service_error)?;
        if let Some(value) = args.palette {
            config.tui.palette = value;
        }
        if let Some(value) = args.activity_filter {
            validate_activity_filter(value.as_str())?;
            config.tui.activity_filter = value.into_inner();
        }
        if let Some(patch) = args.notifications {
            if let Some(value) = patch.toast {
                config.tui.notifications.toast = value;
            }
            if let Some(value) = patch.bell {
                config.tui.notifications.bell = value;
            }
        }
        self.engine
            .global_config_store
            .write(&config)
            .await
            .map_err(service_error)?;
        self.engine.refresh_config().await.map_err(service_error)?;
        Ok(config)
    }

    pub(super) async fn config_prompt_mode_get(
        &self,
        _ctx: &OpContext,
        _args: ConfigPromptModeGetArgs,
    ) -> OpResult<PromptMode> {
        let raw = FileConfigStore::new(self.engine.paths.clone())
            .read_raw()
            .await
            .map_err(service_error)?;
        orch_engine::prompt_mode::effective_config_of_raw(&raw)
            .map(|v| v.defaults.agent.prompt_mode)
            .map_err(service_error)
    }

    pub(super) async fn config_prompt_mode_set(
        &self,
        _ctx: &OpContext,
        args: ConfigPromptModeSetArgs,
    ) -> OpResult<PromptMode> {
        let mode = orch_engine::prompt_mode::set_project_prompt_mode(
            self.engine.paths.project_root(),
            self.engine.agents.as_ref(),
            args.mode,
        )
        .await
        .map_err(service_error)?;
        self.engine.refresh_config().await.map_err(service_error)?;
        Ok(mode)
    }

    pub(super) async fn config_prompt_mode_toggle(
        &self,
        _ctx: &OpContext,
        _args: ConfigPromptModeToggleArgs,
    ) -> OpResult<PromptMode> {
        let mode = orch_engine::prompt_mode::toggle_project_prompt_mode(
            self.engine.paths.project_root(),
            self.engine.agents.as_ref(),
        )
        .await
        .map_err(service_error)?;
        self.engine.refresh_config().await.map_err(service_error)?;
        Ok(mode)
    }

    pub(super) async fn state_complete_onboarding(
        &self,
        _ctx: &OpContext,
        _args: StateCompleteOnboardingArgs,
    ) -> OpResult<()> {
        self.engine
            .orchestrator
            .complete_onboarding()
            .await
            .map_err(service_error)
    }

    pub(super) async fn task_attachment_read(
        &self,
        _ctx: &OpContext,
        args: TaskAttachmentReadArgs,
    ) -> OpResult<results::AttachmentBytes> {
        let filename = Path::new(args.filename.as_str());
        if filename.components().count() != 1
            || !matches!(
                filename.components().next(),
                Some(std::path::Component::Normal(_))
            )
        {
            return Err(invalid("attachment filename must be a single file name"));
        }
        self.engine
            .task_service
            .get(args.task_id.as_str())
            .await
            .map_err(service_error)?;
        let path = self
            .engine
            .task_service
            .attachment_path(args.task_id.as_str(), args.filename.as_str())
            .map_err(service_error)?;
        let metadata = tokio::fs::metadata(&path)
            .await
            .map_err(|err| service_error(Error::io("read attachment", &path, err)))?;
        if !metadata.is_file() {
            return Err(invalid("attachment: expected a regular file"));
        }
        // Larger attachment responses use the session's result-chunk stream.
        if metadata.len() > 64 * 1024 * 1024 {
            return Err(invalid("attachment exceeds 64 MiB"));
        }
        let mut bytes = Vec::new();
        tokio::fs::File::open(&path)
            .await
            .map_err(|err| service_error(Error::io("open attachment", &path, err)))?
            .take(64 * 1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .await
            .map_err(|err| service_error(Error::io("read attachment", &path, err)))?;
        if bytes.len() > 64 * 1024 * 1024 {
            return Err(invalid("attachment exceeds 64 MiB"));
        }
        Ok(results::AttachmentBytes {
            filename: args.filename.into_inner(),
            size: bytes.len() as u64,
            data_base64: base64::engine::general_purpose::STANDARD.encode(bytes),
        })
    }

    pub(super) async fn attachment_stage_image(
        &self,
        ctx: &OpContext,
        args: AttachmentStageImageArgs,
    ) -> OpResult<results::StagedAttachment> {
        let bytes = self
            .take_transfer_for(ctx, &args.input_transfer, TransferPurpose::ClipboardImage)
            .await?;
        if !bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
            return Err(invalid("image: expected PNG bytes"));
        }
        let run_directory = self.engine.paths.root().join("run");
        ensure_private_directory(&run_directory).await?;
        let directory = run_directory.join("staged");
        ensure_private_directory(&directory).await?;
        let path = directory.join(format!("{}.png", self.engine.ids.generate("img")));
        let mut file = tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
            .await
            .map_err(|err| service_error(Error::io("stage image", &path, err)))?;
        file.write_all(&bytes)
            .await
            .map_err(|err| service_error(Error::io("write staged image", &path, err)))?;
        file.flush()
            .await
            .map_err(|err| service_error(Error::io("flush staged image", &path, err)))?;
        let path = orch_proto::path::WirePath::new(path)
            .map_err(|_| invalid("staged image path exceeds protocol limit"))?;
        Ok(results::StagedAttachment {
            path,
            bytes: bytes.len() as u64,
        })
    }
}

async fn ensure_private_directory(directory: &Path) -> OpResult<()> {
    use std::os::unix::fs::PermissionsExt;
    if let Err(error) = tokio::fs::create_dir(directory).await
        && error.kind() != std::io::ErrorKind::AlreadyExists
    {
        return Err(service_error(Error::io(
            "create staging directory",
            directory,
            error,
        )));
    }
    let metadata = tokio::fs::symlink_metadata(directory)
        .await
        .map_err(|error| service_error(Error::io("inspect staging directory", directory, error)))?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(invalid("staging directory must be a real directory"));
    }
    tokio::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700))
        .await
        .map_err(|error| service_error(Error::io("set staging permissions", directory, error)))
}

fn field_patch<T>(patch: FieldPatch<T>) -> Option<T> {
    match patch {
        FieldPatch::Set(value) => Some(value),
        FieldPatch::Clear => None,
    }
}

fn authored_agent(authored: AuthoredAgent) -> results::AuthoredAgent {
    results::AuthoredAgent {
        agent: authored.agent,
        diagnostics: authored
            .diagnostics
            .into_iter()
            .map(|d| results::AgentDiagnostic {
                agent_id: d.agent_id,
                agent_name: d.agent_name,
                message: d.message,
            })
            .collect(),
    }
}

fn invalid(message: &str) -> ErrorBody {
    ErrorBody::new(ErrorCode::InvalidArguments, message, Execution::NotExecuted)
}

fn task_authored(task: &Task) -> Value {
    json!({"title":task.title,"description":task.description,"priority":task.priority,"labels":task.labels})
}
fn agent_authored(agent: &orch_domain::Agent) -> Value {
    let mut value = json!({"name":agent.name,"adapter":agent.adapter,"config":agent.config});
    if let Some(role) = &agent.role {
        value["role"] = json!(role);
    }
    value
}

pub(super) fn definition_document(value: &Value) -> OpResult<DefinitionDocument> {
    let document =
        serde_json::to_string(value).map_err(|_| invalid("definition could not be encoded"))?;
    let digest = Id::new(format!("{:x}", Sha256::digest(document.as_bytes())))
        .map_err(|_| invalid("definition digest exceeds protocol limit"))?;
    Ok(DefinitionDocument { document, digest })
}

fn check_digest(current: &Value, definition: &DefinitionInput) -> OpResult<()> {
    if let Some(expected) = &definition.base_digest
        && *expected != definition_document(current)?.digest
    {
        return Err(ErrorBody::new(
            ErrorCode::Conflict,
            "authored definition changed; fetch it again before applying",
            Execution::NotExecuted,
        ));
    }
    Ok(())
}

pub(super) fn definition_candidate(
    current: &Value,
    input: &Value,
    definition: &DefinitionInput,
) -> OpResult<Value> {
    check_digest(current, definition)?;
    apply_mutation(current, input, definition.kind)
}

pub(super) async fn read_document(
    handler: &ServiceHandler,
    ctx: &OpContext,
    input: DocumentInput,
) -> OpResult<Value> {
    let bytes = match (input.document, input.input_transfer) {
        (Some(text), None) => text.into_inner().into_bytes(),
        (None, Some(token)) => handler.take_transfer(ctx, &token).await?,
        _ => return Err(invalid("supply exactly one document or input_transfer")),
    };
    parse_document(input.format, &bytes).await
}

pub(super) async fn parse_document(format: DocumentFormat, bytes: &[u8]) -> OpResult<Value> {
    if bytes.len() > MAX_DOCUMENT_BYTES {
        return Err(invalid("document exceeds 1 MiB"));
    }
    let value = match format {
        DocumentFormat::Json => {
            let mut parser = serde_json::Deserializer::from_slice(bytes);
            let value = StrictValue(0).deserialize(&mut parser).map_err(|_| {
                invalid("invalid JSON (check types, duplicate keys and depth <= 64)")
            })?;
            parser
                .end()
                .map_err(|_| invalid("expected exactly one JSON document"))?;
            value
        }
        DocumentFormat::Yaml => {
            fs_utils::parse_definition_yaml_bytes(Path::new("<request>"), bytes)
                .await
                .map_err(service_error)?
        }
    };
    if !value.is_object() {
        return Err(invalid("document root: expected an object"));
    }
    Ok(value)
}

fn apply_mutation(current: &Value, input: &Value, kind: MutationKind) -> OpResult<Value> {
    if !input.is_object() {
        return Err(invalid("document root: expected an object"));
    }
    check_depth(input, 0)?;
    let candidate = match kind {
        MutationKind::Update => input.clone(),
        MutationKind::Patch => {
            let mut value = current.clone();
            merge_patch(&mut value, input);
            value
        }
    };
    check_depth(&candidate, 0)?;
    Ok(candidate)
}

fn merge_patch(target: &mut Value, patch: &Value) {
    if let Value::Object(patch) = patch {
        if !target.is_object() {
            *target = json!({});
        }
        let target = target.as_object_mut().expect("object assigned");
        for (key, value) in patch {
            if value.is_null() {
                target.remove(key);
            } else {
                merge_patch(target.entry(key.clone()).or_insert(Value::Null), value);
            }
        }
    } else {
        *target = patch.clone();
    }
}

fn writable_input(
    input: &Value,
    current: &Value,
    writable: &[&str],
    readonly: &[&str],
    kind: MutationKind,
) -> OpResult<Value> {
    let map = input
        .as_object()
        .ok_or_else(|| invalid("document root: expected an object"))?;
    let mut out = serde_json::Map::new();
    for (key, value) in map {
        if writable.contains(&key.as_str()) {
            out.insert(key.clone(), value.clone());
        } else if readonly.contains(&key.as_str()) {
            if kind == MutationKind::Patch || current.get(key) != Some(value) {
                return Err(invalid(&format!("{key}: read-only field")));
            }
        } else {
            return Err(invalid(&format!("{key}: unknown field")));
        }
    }
    Ok(Value::Object(out))
}

fn authored_subset(current: &Value, keys: &[&str]) -> Value {
    Value::Object(
        current
            .as_object()
            .expect("serialized entity")
            .iter()
            .filter(|(key, _)| keys.contains(&key.as_str()))
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect(),
    )
}

fn task_candidate(current: &Value, input: &Value, kind: MutationKind) -> OpResult<Value> {
    let writable = &["title", "description", "priority", "labels"];
    let input = writable_input(
        input,
        current,
        writable,
        &[
            "id",
            "status",
            "assignee",
            "depends_on",
            "created_at",
            "updated_at",
            "attempts",
            "max_attempts",
            "workspace_mode",
            "workspace",
            "proof",
            "review_criteria",
            "review_results",
            "scope",
            "feedback",
            "goalId",
            "attachments",
        ],
        kind,
    )?;
    let current = authored_subset(current, writable);
    if kind == MutationKind::Update {
        reject_nulls(&input, "task")?;
    }
    let mut candidate = apply_mutation(&current, &input, kind)?;
    for field in ["title", "description"] {
        if candidate.get(field).and_then(Value::as_str).is_none() {
            return Err(invalid(&format!("{field}: required string")));
        }
    }
    if candidate["title"]
        .as_str()
        .expect("validated title")
        .trim()
        .is_empty()
    {
        return Err(invalid("title: cannot be blank"));
    }
    if !candidate
        .get("priority")
        .and_then(Value::as_i64)
        .is_some_and(|v| (1..=4).contains(&v))
    {
        return Err(invalid("priority: required integer between 1 and 4"));
    }
    if candidate.get("labels").is_none() {
        candidate["labels"] = json!([]);
    }
    if !candidate["labels"]
        .as_array()
        .is_some_and(|items| items.iter().all(Value::is_string))
    {
        return Err(invalid("labels: expected an array of strings"));
    }
    Ok(candidate)
}

fn agent_candidate(current: &Value, input: &Value, kind: MutationKind) -> OpResult<Value> {
    let writable = &["name", "adapter", "role", "config"];
    let input = writable_input(
        input,
        current,
        writable,
        &[
            "id",
            "status",
            "current_task",
            "autonomous",
            "stats",
            "last_error",
            "capabilities",
            "remote",
        ],
        kind,
    )?;
    let current = authored_subset(current, writable);
    if let Some(config) = input.get("config").and_then(Value::as_object) {
        for key in config.keys() {
            if ![
                "command",
                "model",
                "effort",
                "approval_policy",
                "max_turns",
                "timeout_ms",
                "stall_timeout_ms",
                "env",
                "system_prompt",
                "workspace_mode",
                "skills",
                "prompt_mode",
                "role_summary",
            ]
            .contains(&key.as_str())
            {
                return Err(invalid(&format!("config.{key}: unknown field")));
            }
        }
    }
    if kind == MutationKind::Update {
        reject_nulls(&input, "agent")?;
    }
    let candidate = apply_mutation(&current, &input, kind)?;
    for field in ["name", "adapter"] {
        if !candidate
            .get(field)
            .and_then(Value::as_str)
            .is_some_and(|v| !v.trim().is_empty())
        {
            return Err(invalid(&format!("{field}: required nonblank string")));
        }
    }
    if candidate
        .get("role")
        .is_some_and(|v| !v.is_string() && !v.is_null())
    {
        return Err(invalid("role: expected a string"));
    }
    let config = candidate
        .get("config")
        .and_then(Value::as_object)
        .ok_or_else(|| invalid("config: required object"))?;
    reject_nulls(&candidate["config"], "config")?;
    for (key, value) in config {
        let single = json!({key:value});
        serde_json::from_value::<orch_domain::AgentConfig>(single)
            .map_err(|_| invalid(&format!("config.{key}: invalid field type or enum")))?;
    }
    Ok(candidate)
}

fn reject_nulls(value: &Value, path: &str) -> OpResult<()> {
    match value {
        Value::Null => {
            return Err(invalid(&format!(
                "{path}: null is not allowed; omit the field"
            )));
        }
        Value::Object(map) => {
            for (key, value) in map {
                reject_nulls(value, &format!("{path}.{key}"))?;
            }
        }
        Value::Array(items) => {
            for value in items {
                reject_nulls(value, path)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn check_depth(value: &Value, depth: usize) -> OpResult<()> {
    if depth > 64 {
        return Err(invalid("document depth exceeds 64"));
    }
    match value {
        Value::Object(map) => {
            for value in map.values() {
                check_depth(value, depth + 1)?;
            }
        }
        Value::Array(items) => {
            for value in items {
                check_depth(value, depth + 1)?;
            }
        }
        _ => {}
    }
    Ok(())
}

async fn validate_prompt_candidate(handler: &ServiceHandler, candidate: &Value) -> OpResult<()> {
    let typed =
        orch_engine::prompt_mode::effective_config_of_raw(candidate).map_err(service_error)?;
    let agents = handler
        .engine
        .agent_service
        .list()
        .await
        .map_err(service_error)?;
    orch_engine::prompt_mode::validate_project_config_candidate(&typed, &agents)
        .map_err(service_error)
}

fn global_value(config: &GlobalConfig, key: &str) -> Option<Value> {
    match key {
        "activity_filter" => Some(json!(config.tui.activity_filter)),
        "palette" => Some(json!(config.tui.palette.as_str())),
        _ => None,
    }
}

fn validate_activity_filter(value: &str) -> OpResult<()> {
    if !orch_domain::ACTIVITY_FILTER_PRESETS.contains(&value) {
        return Err(invalid("activity_filter: unknown preset"));
    }
    Ok(())
}

struct StrictValue(usize);
impl<'de> DeserializeSeed<'de> for StrictValue {
    type Value = Value;
    fn deserialize<D: serde::Deserializer<'de>>(self, deserializer: D) -> Result<Value, D::Error> {
        if self.0 > 64 {
            return Err(serde::de::Error::custom("document depth exceeds 64"));
        }
        deserializer.deserialize_any(self)
    }
}
impl<'de> Visitor<'de> for StrictValue {
    type Value = Value;
    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a JSON value")
    }
    fn visit_unit<E: serde::de::Error>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }
    fn visit_bool<E: serde::de::Error>(self, v: bool) -> Result<Value, E> {
        Ok(v.into())
    }
    fn visit_i64<E: serde::de::Error>(self, v: i64) -> Result<Value, E> {
        Ok(v.into())
    }
    fn visit_u64<E: serde::de::Error>(self, v: u64) -> Result<Value, E> {
        Ok(v.into())
    }
    fn visit_f64<E: serde::de::Error>(self, v: f64) -> Result<Value, E> {
        serde_json::Number::from_f64(v)
            .map(Value::Number)
            .ok_or_else(|| E::custom("nonfinite number"))
    }
    fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<Value, E> {
        Ok(v.into())
    }
    fn visit_string<E: serde::de::Error>(self, v: String) -> Result<Value, E> {
        Ok(v.into())
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Value, A::Error> {
        let mut values = Vec::new();
        while let Some(value) = seq.next_element_seed(StrictValue(self.0 + 1))? {
            values.push(value);
        }
        Ok(Value::Array(values))
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Value, A::Error> {
        let mut values = serde_json::Map::new();
        while let Some(key) = map.next_key::<String>()? {
            if values.contains_key(&key) {
                return Err(serde::de::Error::custom("duplicate mapping key"));
            }
            values.insert(key, map.next_value_seed(StrictValue(self.0 + 1))?);
        }
        Ok(Value::Object(values))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[cfg(feature = "testing")]
    async fn handler() -> (tempfile::TempDir, ServiceHandler, OpContext) {
        use orch_proto::handshake::{Caller, CallerKind, Session, SessionMode};
        use std::sync::Arc;
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(temp.path().join(".orchestry")).unwrap();
        let engine = Arc::new(
            orch_engine::container::build_full_with(
                temp.path(),
                orch_engine::container::EngineSeams {
                    global_config_path: Some(temp.path().join("global.yml")),
                    ..Default::default()
                },
            )
            .await
            .unwrap(),
        );
        let coordinator = Arc::new(crate::coordinator::ProjectCoordinator::new(
            engine.events.operation_boundary(),
        ));
        let handler = ServiceHandler::new(engine, coordinator);
        let ctx = OpContext::new(Session {
            caller: Caller {
                kind: CallerKind::Cli,
                agent_id: None,
                pid: None,
            },
            mode: SessionMode::Normal,
        });
        (temp, handler, ctx)
    }

    #[cfg(feature = "testing")]
    #[tokio::test]
    async fn authored_task_apply_changes_only_authored_fields_and_rejects_stale_base() {
        let (_temp, handler, ctx) = handler().await;
        let task = handler
            .engine
            .task_service
            .create(orch_domain::CreateTaskInput {
                title: "before".into(),
                ..Default::default()
            })
            .await
            .unwrap();
        let document = handler
            .task_definition_get(
                &ctx,
                TaskDefinitionGetArgs {
                    id: Id::new(task.id.clone()).unwrap(),
                },
            )
            .await
            .unwrap();
        let mut runtime = task.clone();
        runtime.attempts = 2;
        runtime.status = orch_domain::TaskStatus::Cancelled;
        handler.engine.tasks.save(&runtime).await.unwrap();
        let apply = TaskDefinitionApplyArgs {
            id: Id::new(task.id.clone()).unwrap(),
            definition: DefinitionInput {
                input: DocumentInput {
                    document: Some(DocumentText::new(r#"{"title":"after"}"#).unwrap()),
                    input_transfer: None,
                    format: DocumentFormat::Json,
                },
                kind: MutationKind::Patch,
                base_digest: Some(document.digest),
            },
        };
        let updated = handler
            .task_definition_apply(&ctx, apply.clone())
            .await
            .unwrap();
        assert_eq!(updated.title, "after");
        assert_eq!(updated.status, orch_domain::TaskStatus::Cancelled);
        assert_eq!(updated.attempts, 2);
        assert_eq!(updated.created_at, task.created_at);
        let error = handler
            .task_definition_apply(&ctx, apply)
            .await
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::Conflict);
        assert_eq!(
            handler
                .engine
                .task_service
                .get(&task.id)
                .await
                .unwrap()
                .title,
            "after"
        );
    }

    #[cfg(feature = "testing")]
    #[tokio::test]
    async fn authored_agent_apply_validates_keys_and_preserves_runtime_fields() {
        let (_temp, handler, ctx) = handler().await;
        let authored = handler
            .agent_create(
                &ctx,
                serde_json::from_value(json!({"name":"worker","adapter":"shell","command":"true"}))
                    .unwrap(),
            )
            .await
            .unwrap();
        let id = Id::new(authored.agent.id.clone()).unwrap();
        let document = handler
            .agent_definition_get(&ctx, AgentDefinitionGetArgs { id: id.clone() })
            .await
            .unwrap();
        let mut runtime = authored.agent;
        runtime.autonomous = Some(true);
        handler.engine.agents.save(&runtime).await.unwrap();
        let updated = handler
            .agent_definition_apply(
                &ctx,
                AgentDefinitionApplyArgs {
                    id: id.clone(),
                    definition: DefinitionInput {
                        input: DocumentInput {
                            document: Some(DocumentText::new(r#"{"role":"new role"}"#).unwrap()),
                            input_transfer: None,
                            format: DocumentFormat::Json,
                        },
                        kind: MutationKind::Patch,
                        base_digest: Some(document.digest),
                    },
                },
            )
            .await
            .unwrap();
        assert_eq!(updated.agent.role.as_deref(), Some("new role"));
        assert_eq!(updated.agent.autonomous, Some(true));
        let error = handler
            .agent_definition_apply(
                &ctx,
                AgentDefinitionApplyArgs {
                    id: id.clone(),
                    definition: DefinitionInput {
                        input: DocumentInput {
                            document: Some(
                                DocumentText::new(r#"{"config":{"typo":null}}"#).unwrap(),
                            ),
                            input_transfer: None,
                            format: DocumentFormat::Json,
                        },
                        kind: MutationKind::Patch,
                        base_digest: None,
                    },
                },
            )
            .await
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidArguments);
        assert_eq!(
            handler
                .engine
                .agent_service
                .get(id.as_str())
                .await
                .unwrap()
                .role
                .as_deref(),
            Some("new role")
        );
    }

    #[cfg(feature = "testing")]
    #[tokio::test]
    async fn config_and_preference_patches_read_current_values() {
        let (temp, handler, ctx) = handler().await;
        handler
            .config_set(
                &ctx,
                ConfigSetArgs {
                    key: Id::new("defaults.task.max_attempts").unwrap(),
                    value: orch_proto::bounded::Text::new("9").unwrap(),
                },
            )
            .await
            .unwrap();
        let task = handler
            .engine
            .task_service
            .create(orch_domain::CreateTaskInput {
                title: "fresh".into(),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(task.max_attempts, 9);
        handler
            .ui_preferences_patch(
                &ctx,
                UiPreferencesPatchArgs {
                    palette: None,
                    activity_filter: Some(Id::new("all").unwrap()),
                    notifications: Some(NotificationsPatch {
                        toast: Some(false),
                        bell: None,
                    }),
                },
            )
            .await
            .unwrap();
        let preferences = handler
            .ui_preferences_patch(
                &ctx,
                UiPreferencesPatchArgs {
                    palette: None,
                    activity_filter: None,
                    notifications: Some(NotificationsPatch {
                        toast: None,
                        bell: Some(true),
                    }),
                },
            )
            .await
            .unwrap();
        assert!(!preferences.tui.notifications.toast);
        assert!(preferences.tui.notifications.bell);
        assert!(temp.path().join("global.yml").exists());
    }

    #[cfg(feature = "testing")]
    #[tokio::test]
    async fn onboarding_and_prompt_mode_survive_future_operations() {
        let (_temp, handler, ctx) = handler().await;
        handler.engine.orchestrator.daemon_tick(true).await.unwrap();
        handler
            .state_complete_onboarding(&ctx, StateCompleteOnboardingArgs {})
            .await
            .unwrap();
        handler.engine.orchestrator.daemon_tick(true).await.unwrap();
        assert_eq!(
            handler
                .engine
                .state
                .read()
                .await
                .unwrap()
                .onboarding_completed,
            Some(true)
        );
        assert_eq!(
            handler
                .config_prompt_mode_set(
                    &ctx,
                    ConfigPromptModeSetArgs {
                        mode: PromptMode::Eco
                    }
                )
                .await
                .unwrap(),
            PromptMode::Eco
        );
        assert_eq!(
            handler
                .config_prompt_mode_get(&ctx, ConfigPromptModeGetArgs {})
                .await
                .unwrap(),
            PromptMode::Eco
        );
        assert_eq!(
            handler
                .config_prompt_mode_toggle(&ctx, ConfigPromptModeToggleArgs {})
                .await
                .unwrap(),
            PromptMode::Normal
        );
        assert_eq!(
            handler
                .config_prompt_mode_get(&ctx, ConfigPromptModeGetArgs {})
                .await
                .unwrap(),
            PromptMode::Normal
        );
    }

    #[cfg(feature = "testing")]
    #[tokio::test]
    async fn attachment_read_rejects_path_traversal_before_access() {
        let (_temp, handler, ctx) = handler().await;
        for filename in ["../config.yml", "/etc/passwd", ".", "a/b"] {
            let error = handler
                .task_attachment_read(
                    &ctx,
                    TaskAttachmentReadArgs {
                        task_id: Id::new("tsk_missing").unwrap(),
                        filename: Id::new(filename).unwrap(),
                    },
                )
                .await
                .unwrap_err();
            assert_eq!(error.code, ErrorCode::InvalidArguments);
        }
    }

    #[cfg(feature = "testing")]
    #[tokio::test]
    async fn clipboard_staging_consumes_a_sealed_image_transfer_into_a_private_file() {
        use std::os::unix::fs::PermissionsExt;
        let (_temp, handler, ctx) = handler().await;
        let bytes = b"\x89PNG\r\n\x1a\nstaged bytes";
        let transfer = handler
            .transfer_begin(
                &ctx,
                TransferBeginArgs {
                    purpose: TransferPurpose::ClipboardImage,
                    total_bytes: bytes.len() as u64,
                },
            )
            .await
            .unwrap();
        handler
            .transfer_chunk(
                &ctx,
                TransferChunkArgs {
                    token: transfer.token.clone(),
                    n: 0,
                    data_base64: base64::engine::general_purpose::STANDARD
                        .encode(bytes)
                        .try_into()
                        .unwrap(),
                },
            )
            .await
            .unwrap();
        handler
            .transfer_finish(
                &ctx,
                TransferFinishArgs {
                    token: transfer.token.clone(),
                    sha256: Id::new(format!("{:x}", Sha256::digest(bytes))).unwrap(),
                },
            )
            .await
            .unwrap();
        let staged = handler
            .attachment_stage_image(
                &ctx,
                AttachmentStageImageArgs {
                    input_transfer: transfer.token.clone(),
                    format: ImageFormat::Png,
                },
            )
            .await
            .unwrap();
        assert!(staged.path.as_path().is_absolute());
        assert_eq!(tokio::fs::read(staged.path.as_path()).await.unwrap(), bytes);
        assert_eq!(
            std::fs::metadata(staged.path.as_path())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        assert_eq!(
            std::fs::metadata(staged.path.as_path().parent().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert!(
            handler
                .attachment_stage_image(
                    &ctx,
                    AttachmentStageImageArgs {
                        input_transfer: transfer.token,
                        format: ImageFormat::Png
                    }
                )
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn input_rejects_duplicate_keys_and_non_object_roots() {
        for bytes in [br#"{"title":"a","title":"b"}"#.as_slice(), br#"[]"#] {
            assert!(parse_document(DocumentFormat::Json, bytes).await.is_err());
        }
        assert!(
            parse_document(DocumentFormat::Yaml, b"title: a\ntitle: b\n")
                .await
                .is_err()
        );
    }

    #[test]
    fn definition_digest_rejects_an_overlapping_authored_edit() {
        let current = json!({"title":"new", "description":"", "priority":2,"labels":[]});
        let old = definition_document(&json!({"title":"old"})).unwrap();
        let definition = DefinitionInput {
            input: DocumentInput {
                document: None,
                input_transfer: None,
                format: DocumentFormat::Json,
            },
            kind: MutationKind::Patch,
            base_digest: Some(old.digest),
        };
        let error =
            definition_candidate(&current, &json!({"title":"stale"}), &definition).unwrap_err();
        assert_eq!(error.code, ErrorCode::Conflict);
    }

    #[test]
    fn task_definition_rejects_runtime_fields_and_retains_validation() {
        let task = json!({"title":"old", "description":"", "priority":2,"labels":[]});
        assert!(task_candidate(&task, &json!({"status":"done"}), MutationKind::Patch).is_err());
        assert!(task_candidate(&task, &json!({"priority":null}), MutationKind::Patch).is_err());
        let candidate =
            task_candidate(&task, &json!({"title":"new"}), MutationKind::Patch).unwrap();
        assert_eq!(candidate["title"], "new");
        assert_eq!(candidate["priority"], 2);
    }

    #[test]
    fn agent_definition_rejects_unknown_null_before_merge() {
        let current = json!({"name":"a","adapter":"shell","role":"r","config":{}});
        assert!(
            agent_candidate(
                &current,
                &json!({"config":{"typo":null}}),
                MutationKind::Patch
            )
            .is_err()
        );
    }
}
