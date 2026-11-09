//! Template authoring commands; runtime entities are owned by engine services.
use super::rpc::{Rpc, argument_error, client_error};
use crate::context::{CliContext, EnvFlags};
use crate::document_input::{MutationKind, read_document};
use crate::output::print_table;
use crate::tree::{Cli, DefinitionInput as FileDefinitionInput};
use clap::{Args, Subcommand, ValueEnum};
use orch_core::{Error, Result};
use orch_domain::org_templates::{MAX_DOCUMENT_BYTES, validate_id};
use orch_proto::{
    ops::{Op, args, results},
    path::WirePath,
};
use serde::Serialize;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

#[derive(Debug, Args)]
pub struct TemplateCmd {
    #[command(subcommand)]
    pub command: Option<TemplateSub>,
}
#[derive(Debug, Subcommand)]
pub enum TemplateSub {
    /// Discover user, embedded and legacy templates, including invalid entries
    List,
    /// Inspect an authoring bundle or its effective deployment inputs
    Show {
        #[command(flatten)]
        selector: Selector,
        #[arg(long)]
        resolved: bool,
    },
    /// Create a new template (never overwrite a destination)
    Init {
        name: String,
        #[arg(long)]
        from: Option<String>,
        #[arg(long)]
        template_dir: Option<PathBuf>,
    },
    /// Edit org.yml on a temporary copy, then validate and publish
    Edit {
        #[command(flatten)]
        selector: Selector,
    },
    /// Replace org.yml from a complete JSON or YAML definition
    Update {
        #[command(flatten)]
        selector: Selector,
        #[command(flatten)]
        input: FileDefinitionInput,
    },
    /// Merge a partial JSON or YAML definition into org.yml
    Patch {
        #[command(flatten)]
        selector: Selector,
        #[command(flatten)]
        input: FileDefinitionInput,
    },
    /// Manage individual member definitions
    Agent {
        #[command(subcommand)]
        command: MemberSub,
    },
}
#[derive(Debug, Clone, Args)]
pub struct Selector {
    #[arg(value_name = "NAME", conflicts_with = "template_dir")]
    pub name: Option<String>,
    #[arg(long, value_name = "PATH", required_unless_present = "name")]
    pub template_dir: Option<PathBuf>,
}
impl Selector {
    /// Explicit directories are resolved against the caller's cwd, never the daemon's.
    pub(crate) fn wire(&self, cwd: &Path) -> Result<args::TemplateSelector> {
        Ok(args::TemplateSelector {
            name: self
                .name
                .clone()
                .map(TryInto::try_into)
                .transpose()
                .map_err(argument_error)?,
            template_dir: self
                .template_dir
                .as_deref()
                .map(|path| absolute_path(cwd, path))
                .transpose()?,
        })
    }
}
fn absolute_path(cwd: &Path, path: &Path) -> Result<WirePath> {
    WirePath::new(if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    })
    .map_err(argument_error)
}
#[derive(Clone)]
enum Target {
    Org,
    Agent(String),
}
#[derive(Debug, Args)]
pub struct MemberSelector {
    #[arg(value_name = "NAME", conflicts_with = "template_dir")]
    pub name: Option<String>,
    #[arg(value_name = "AGENT", conflicts_with = "named_agent")]
    pub agent: Option<String>,
    #[arg(
        long = "agent",
        value_name = "AGENT",
        required_unless_present = "agent"
    )]
    pub named_agent: Option<String>,
    #[arg(long, value_name = "PATH", required_unless_present = "name")]
    pub template_dir: Option<PathBuf>,
}
impl MemberSelector {
    fn selector(&self) -> Selector {
        Selector {
            name: self.name.clone(),
            template_dir: self.template_dir.clone(),
        }
    }
    fn target(&self) -> Result<Target> {
        let agent = self
            .named_agent
            .as_ref()
            .or(self.agent.as_ref())
            .ok_or_else(|| Error::invalid_arguments("Select --agent AGENT"))?;
        validate_id(agent).map_err(Error::invalid_arguments)?;
        Ok(Target::Agent(agent.clone()))
    }
}
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum DefinitionFormat {
    Json,
    Yaml,
}
#[derive(Debug, Args)]
pub struct DefinitionInput {
    #[arg(short = 'f', long, value_name = "FILE_OR_DASH")]
    pub file: PathBuf,
    #[arg(long, value_enum)]
    pub input_format: Option<DefinitionFormat>,
}
impl DefinitionInput {
    async fn read(&self) -> Result<Value> {
        read_document(
            &self.file,
            self.input_format.map(|format| match format {
                DefinitionFormat::Json => crate::document_input::InputFormat::Json,
                DefinitionFormat::Yaml => crate::document_input::InputFormat::Yaml,
            }),
        )
        .await
    }
}
#[derive(Debug, Subcommand)]
pub enum MemberSub {
    List {
        #[command(flatten)]
        selector: Selector,
    },
    Add {
        #[command(flatten)]
        selector: Selector,
        #[command(flatten)]
        input: DefinitionInput,
    },
    Edit {
        #[command(flatten)]
        selector: MemberSelector,
    },
    Update {
        #[command(flatten)]
        selector: MemberSelector,
        #[command(flatten)]
        input: FileDefinitionInput,
    },
    Patch {
        #[command(flatten)]
        selector: MemberSelector,
        #[command(flatten)]
        input: FileDefinitionInput,
    },
    Delete {
        #[command(flatten)]
        selector: MemberSelector,
    },
}

pub(crate) fn requires_initialized_project(_args: &[String]) -> bool {
    false
}

pub(crate) async fn dispatch(
    cli: &Cli,
    env: EnvFlags,
    cwd: &Path,
    args: &TemplateCmd,
) -> Result<ExitCode> {
    let ctx = CliContext::resolve(cwd, cli.output(), cli.quiet, cli.no_color, cli.ascii, env);
    let Some(command) = &args.command else {
        return Ok(crate::print_group_help(&["org", "template"]));
    };
    let engine = Rpc::connect(cli, cwd).await?;
    match command {
        TemplateSub::List => return list(&engine, &ctx, env).await,
        TemplateSub::Show { selector, resolved } => {
            let view = engine
                .request(args::OrgTemplateShowArgs {
                    selector: selector.wire(cwd)?,
                    resolved: *resolved,
                })
                .await?;
            emit(&ctx, &template_view(view)?)?;
        }
        TemplateSub::Init {
            name,
            from,
            template_dir,
        } => {
            let path = template_dir
                .as_deref()
                .map(|path| absolute_path(cwd, path))
                .transpose()?;
            let initialized = engine
                .call::<args::OrgTemplateInitArgs>(
                    serde_json::json!({"name":name,"from":from,"template_dir":path}),
                )
                .await?;
            let path = serde_json::to_value(initialized.path.as_path())
                .map_err(|error| Error::other(error.to_string()))?;
            emit(
                &ctx,
                &serde_json::json!({"name":initialized.name,"path":path}),
            )?;
        }
        TemplateSub::Edit { selector } => {
            let selector = selector.wire(cwd)?;
            refuse_builtin_edit(&engine, &selector).await?;
            let base = definition_get(&engine, selector.clone(), &Target::Org).await?;
            let input = edit_document(&base.document).await?;
            let updated = definition_apply(
                &engine,
                selector,
                Target::Org,
                &input,
                MutationKind::Update,
                Some(base.digest),
            )
            .await?;
            emit(&ctx, &updated)?;
        }
        TemplateSub::Update { selector, input } | TemplateSub::Patch { selector, input } => {
            let kind = if matches!(command, TemplateSub::Patch { .. }) {
                MutationKind::Patch
            } else {
                MutationKind::Update
            };
            let updated =
                mutate_definition(&engine, selector.wire(cwd)?, Target::Org, input, kind).await?;
            emit(&ctx, &updated)?;
        }
        TemplateSub::Agent { command } => match command {
            MemberSub::List { selector } => {
                let agents = engine
                    .request(args::OrgTemplateAgentListArgs {
                        selector: selector.wire(cwd)?,
                    })
                    .await?;
                emit(&ctx, &agents)?;
            }
            MemberSub::Add { selector, input } => {
                let value = input.read().await?;
                let selector = selector.wire(cwd)?;
                let agent = document_request(&engine, &value, |input| {
                    args::OrgTemplateAgentAddArgs { selector, input }
                })
                .await
                .map_err(|error| at_source(&input.file, error))?;
                emit(&ctx, &agent)?;
            }
            MemberSub::Edit { selector } => {
                let target = selector.target()?;
                let selector = selector.selector().wire(cwd)?;
                refuse_builtin_edit(&engine, &selector).await?;
                let base = definition_get(&engine, selector.clone(), &target).await?;
                let input = edit_document(&base.document).await?;
                let updated = definition_apply(
                    &engine,
                    selector,
                    target,
                    &input,
                    MutationKind::Update,
                    Some(base.digest),
                )
                .await?;
                emit(&ctx, &updated)?;
            }
            MemberSub::Update { selector, input } | MemberSub::Patch { selector, input } => {
                let kind = if matches!(command, MemberSub::Patch { .. }) {
                    MutationKind::Patch
                } else {
                    MutationKind::Update
                };
                let updated = mutate_definition(
                    &engine,
                    selector.selector().wire(cwd)?,
                    selector.target()?,
                    input,
                    kind,
                )
                .await?;
                emit(&ctx, &updated)?;
            }
            MemberSub::Delete { selector } => {
                let Target::Agent(agent) = selector.target()? else {
                    unreachable!()
                };
                let deleted = engine
                    .request(args::OrgTemplateAgentDeleteArgs {
                        selector: selector.selector().wire(cwd)?,
                        agent: agent.try_into().map_err(argument_error)?,
                    })
                    .await?;
                emit(&ctx, &deleted)?;
            }
        },
    }
    Ok(ExitCode::SUCCESS)
}

/// A builtin is read-only. The daemon refuses its apply, but only after the
/// editor would have opened; refuse first, as the pre-daemon CLI did.
async fn refuse_builtin_edit(engine: &Rpc, selector: &args::TemplateSelector) -> Result<()> {
    let view = engine
        .request(args::OrgTemplateShowArgs {
            selector: selector.clone(),
            resolved: false,
        })
        .await?;
    if !matches!(view.source, results::TemplateSource::Builtin) {
        return Ok(());
    }
    let name = selector.name.as_ref().map_or("", |name| name.as_str());
    let key = name.split_once(':').map_or(name, |(_, key)| key);
    Err(Error::invalid_arguments(format!(
        "Builtins are read-only; run orch org template init {key}-copy --from builtin:{key}"
    )))
}

async fn definition_get(
    engine: &Rpc,
    selector: args::TemplateSelector,
    target: &Target,
) -> Result<results::DefinitionDocument> {
    match target {
        Target::Org => {
            engine
                .request(args::OrgTemplateDefinitionGetArgs { selector })
                .await
        }
        Target::Agent(agent) => {
            engine
                .call::<args::OrgTemplateAgentDefinitionGetArgs>(
                    serde_json::json!({"selector":selector,"agent":agent}),
                )
                .await
        }
    }
}
async fn definition_apply(
    engine: &Rpc,
    selector: args::TemplateSelector,
    target: Target,
    input: &Value,
    kind: MutationKind,
    base_digest: Option<orch_proto::bounded::Id>,
) -> Result<Value> {
    let kind = wire_kind(kind);
    match target {
        Target::Org => serde_json::to_value(
            document_request(engine, input, |input| {
                args::OrgTemplateDefinitionApplyArgs {
                    selector,
                    definition: args::DefinitionInput {
                        input,
                        kind,
                        base_digest,
                    },
                }
            })
            .await?,
        ),
        Target::Agent(agent) => {
            let agent = agent.try_into().map_err(argument_error)?;
            serde_json::to_value(
                document_request(engine, input, |input| {
                    args::OrgTemplateAgentDefinitionApplyArgs {
                        selector,
                        agent,
                        definition: args::DefinitionInput {
                            input,
                            kind,
                            base_digest,
                        },
                    }
                })
                .await?,
            )
        }
    }
    .map_err(|error| Error::other(error.to_string()))
}
async fn mutate_definition(
    engine: &Rpc,
    selector: args::TemplateSelector,
    target: Target,
    input: &FileDefinitionInput,
    kind: MutationKind,
) -> Result<Value> {
    let value = read_document(&input.file, input.input_format).await?;
    definition_apply(engine, selector, target, &value, kind, None)
        .await
        .map_err(|error| at_source(&input.file, error))
}

/// Preserve document provenance in the typed remote error that the CLI's
/// exit boundary renders, rather than only in the core wrapper's context.
pub(crate) fn at_source(file: &Path, error: Error) -> Error {
    if let Some(orch_client::ClientError::Remote(body)) = super::rpc::recover_client_error(&error) {
        let source = if file == Path::new("-") {
            "<stdin>".to_owned()
        } else {
            file.display().to_string()
        };
        let mut annotated = orch_proto::error::ErrorBody::new(
            body.code,
            &format!("{source}: {}", body.message.as_str()),
            body.execution,
        );
        annotated.exit_code = body.exit_code;
        annotated.hint = body.hint.clone();
        return client_error(orch_client::ClientError::Remote(annotated));
    }
    crate::document_input::at_source(file, error)
}

pub(crate) fn wire_kind(kind: MutationKind) -> args::MutationKind {
    match kind {
        MutationKind::Update => args::MutationKind::Update,
        MutationKind::Patch => args::MutationKind::Patch,
    }
}

/// Retain the same socket lane for upload and consumption: transfer handles are
/// connection-local and cannot survive an intervening reconnect.
pub(crate) async fn document_request<A: Op>(
    engine: &Rpc,
    value: &Value,
    make: impl FnOnce(args::DocumentInput) -> A,
) -> Result<A::Output> {
    let document = serde_json::to_string(value).map_err(|error| Error::other(error.to_string()))?;
    if document.len() > MAX_DOCUMENT_BYTES {
        return Err(Error::invalid_arguments("document exceeds 1 MiB"));
    }
    let mut client = engine.lane().await;
    // Reserve space for the envelope and JSON escaping of inline text.
    let input = if document.len() <= 64 * 1024 {
        args::DocumentInput {
            document: Some(document.try_into().map_err(argument_error)?),
            input_transfer: None,
            format: args::DocumentFormat::Json,
        }
    } else {
        let token = client
            .upload(args::TransferPurpose::Document, document.as_bytes())
            .await
            .map_err(client_error)?;
        args::DocumentInput {
            document: None,
            input_transfer: Some(token),
            format: args::DocumentFormat::Json,
        }
    };
    client.request(make(input)).await.map_err(client_error)
}

pub(crate) async fn edit_document(document: &str) -> Result<Value> {
    let value: Value =
        serde_json::from_str(document).map_err(|error| Error::other(error.to_string()))?;
    let initial =
        serde_yaml_ng::to_string(&value).map_err(|error| Error::other(error.to_string()))?;
    let edited = crate::editor::open_in_editor(&initial, ".yml").await?;
    if edited.len() > MAX_DOCUMENT_BYTES {
        return Err(Error::invalid_arguments("<editor>: document exceeds 1 MiB"));
    }
    orch_infra::storage::fs_utils::parse_definition_yaml_bytes(
        Path::new("<editor>"),
        edited.as_bytes(),
    )
    .await
}

/// RPC paths are losslessly tagged; CLI authoring output retains plain paths.
fn source_value(source: results::TemplateSource) -> Result<Value> {
    let (kind, path) = match source {
        results::TemplateSource::Builtin => ("builtin", None),
        results::TemplateSource::Legacy => ("legacy", None),
        results::TemplateSource::User { path } => ("user", Some(path)),
        results::TemplateSource::Directory { path } => ("directory", Some(path)),
    };
    let mut value = serde_json::json!({"kind":kind});
    if let Some(path) = path {
        value["path"] = serde_json::to_value(path.as_path())
            .map_err(|error| Error::other(error.to_string()))?;
    }
    Ok(value)
}
fn template_view(view: results::TemplateView) -> Result<Value> {
    let mut value = serde_json::json!({"source":source_value(view.source)?});
    let bundle =
        serde_json::to_value(view.bundle).map_err(|error| Error::other(error.to_string()))?;
    value
        .as_object_mut()
        .expect("template object")
        .extend(bundle.as_object().expect("bundle object").clone());
    if let Some(agents) = view.resolved_agents {
        value["resolved_agents"] =
            serde_json::to_value(agents).map_err(|error| Error::other(error.to_string()))?;
    }
    Ok(value)
}
pub fn emit<T: Serialize>(ctx: &CliContext, value: &T) -> Result<()> {
    if let Some(format) = ctx.output {
        format.print(value)
    } else {
        let text = serde_yaml_ng::to_string(value).map_err(|e| Error::other(e.to_string()))?;
        print!("{text}");
        Ok(())
    }
}
async fn list(engine: &Rpc, ctx: &CliContext, env: EnvFlags) -> Result<ExitCode> {
    let entries = engine
        .request(args::OrgTemplateListArgs { template_dir: None })
        .await?;
    if let Some(format) = ctx.output {
        let values = entries
            .iter()
            .map(|entry| {
                let mut value =
                    serde_json::to_value(entry).map_err(|error| Error::other(error.to_string()))?;
                value["source"] = source_value(entry.source.clone())?;
                Ok(value)
            })
            .collect::<Result<Vec<_>>>()?;
        format.print(&values)?;
    } else if ctx.quiet {
        for entry in &entries {
            println!("{}", entry.name);
        }
    } else {
        let rows = entries
            .iter()
            .map(|e| {
                vec![
                    e.name.clone(),
                    match &e.source {
                        results::TemplateSource::User { .. } => "user",
                        results::TemplateSource::Builtin => "builtin",
                        _ => "legacy",
                    }
                    .into(),
                    if let Some(error) = &e.error {
                        error.clone()
                    } else if e.shadowed {
                        "shadowed".into()
                    } else {
                        "ok".into()
                    },
                ]
            })
            .map(|row| {
                row.into_iter()
                    .map(|cell| crate::shop_picker::sanitize(&cell))
                    .collect()
            })
            .collect::<Vec<_>>();
        print_table(&ctx.styles(env), &["NAME", "SOURCE", "STATUS"], &rows, 2);
    }
    Ok(if entries.iter().any(|e| e.error.is_some()) {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    })
}
#[cfg(test)]
mod tests {
    use crate::tree::Cli;
    use clap::Parser;

    #[test]
    fn template_preparse_allows_global_flags_before_the_subgroup_only() {
        for flag in ["--json", "--yaml", "--quiet", "--no-color", "--ascii"] {
            let args = vec![flag.to_owned(), "template".into(), "list".into()];
            assert!(
                !crate::commands::org::requires_initialized_project(&args),
                "{flag}"
            );
        }
        assert!(crate::commands::org::requires_initialized_project(&[
            "unknown".into(),
            "template".into()
        ]));
    }

    #[test]
    fn template_commands_parse_without_a_project() {
        for args in [
            vec![
                "orch",
                "org",
                "template",
                "show",
                "builtin:a-team",
                "--resolved",
            ],
            vec![
                "orch",
                "org",
                "template",
                "agent",
                "delete",
                "--template-dir",
                "./team",
                "--agent",
                "worker",
            ],
            vec!["orch", "agent", "shop", "--template", "a-team", "--list"],
        ] {
            assert!(Cli::try_parse_from(args.clone()).is_ok(), "{args:?}");
        }
        assert!(
            Cli::try_parse_from([
                "orch",
                "org",
                "template",
                "patch",
                "a-team",
                "-f",
                "patch.yml"
            ])
            .is_ok()
        );
        for command in ["update", "patch"] {
            assert!(
                Cli::try_parse_from([
                    "orch",
                    "org",
                    "template",
                    "agent",
                    command,
                    "--template-dir",
                    "./team",
                    "--agent",
                    "worker",
                    "-f",
                    "-",
                    "--input-format",
                    "yaml",
                ])
                .is_ok()
            );
        }
    }
}

#[cfg(test)]
mod rpc_tests {
    use super::*;
    use orch_client::{Client, Project};
    use orch_daemon::testing::DaemonHarness;
    use orch_proto::handshake::{Caller, CallerKind, SessionMode};
    use serde_json::json;

    pub(super) async fn fixture() -> (DaemonHarness, Rpc) {
        let harness = DaemonHarness::builder().start().await.unwrap();
        let project = Project::discover(harness.project_root(), None).unwrap();
        let client = Client::from_stream(
            project,
            harness.connect().await.unwrap().into_stream(),
            Caller {
                kind: CallerKind::Cli,
                agent_id: None,
                pid: None,
            },
            SessionMode::Normal,
        )
        .await
        .unwrap();
        let rpc = Rpc::from_client(harness.project_root().to_path_buf(), client);
        (harness, rpc)
    }

    async fn template(rpc: &Rpc, directory: &Path) -> args::TemplateSelector {
        let selector = Selector {
            name: None,
            template_dir: Some(directory.to_path_buf()),
        }
        .wire(Path::new("/"))
        .unwrap();
        rpc.call::<args::OrgTemplateInitArgs>(
            json!({"name":"team","template_dir":selector.template_dir}),
        )
        .await
        .unwrap();
        selector
    }

    #[test]
    fn cli_template_output_keeps_flat_fields_and_plain_paths() {
        let source = results::TemplateSource::Directory {
            path: WirePath::new("/tmp/team").unwrap(),
        };
        assert_eq!(
            source_value(source).unwrap(),
            json!({"kind":"directory","path":"/tmp/team"})
        );
        let bundle = orch_domain::org_templates::Bundle {
            org: serde_json::from_value(json!({"schema_version":1,"name":"team","lead":"lead"}))
                .unwrap(),
            agents: vec![],
            rules: Default::default(),
        };
        let view = template_view(results::TemplateView {
            source: results::TemplateSource::Builtin,
            bundle,
            resolved_agents: None,
        })
        .unwrap();
        assert!(view.get("org").is_some());
        assert!(view.get("bundle").is_none());
        assert!(view.get("resolved_agents").is_none());
    }

    #[test]
    fn explicit_template_paths_are_absolute_and_lossless() {
        use std::os::unix::ffi::OsStringExt;
        let cwd = Path::new("/tmp/caller/subdir");
        let relative = PathBuf::from(std::ffi::OsString::from_vec(b"team-\xff".to_vec()));
        let wire = Selector {
            name: None,
            template_dir: Some(relative.clone()),
        }
        .wire(cwd)
        .unwrap();
        assert_eq!(wire.template_dir.unwrap().as_path(), cwd.join(relative));
        let source = results::TemplateSource::Directory {
            path: WirePath::new(std::ffi::OsString::from_vec(b"/tmp/team-\xff".to_vec())).unwrap(),
        };
        assert!(source_value(source).is_err());
    }

    #[tokio::test]
    async fn config_values_and_global_validation_use_the_daemon() {
        let (harness, rpc) = fixture().await;
        for (raw, expected) in [
            ("5000", json!(5000)),
            ("null", Value::Null),
            ("not json", json!("not json")),
        ] {
            assert_eq!(
                super::super::config::set_value(&rpc, "extension.value", raw)
                    .await
                    .unwrap(),
                expected
            );
            let value = super::super::config::get_value(&rpc, "extension.value")
                .await
                .unwrap();
            assert_eq!(value, Some(expected));
        }
        let ctx = CliContext::new(
            harness.project_root().to_path_buf(),
            None,
            false,
            true,
            false,
            EnvFlags::default(),
        );
        assert_eq!(
            super::super::config::get_value(&rpc, "extension.missing")
                .await
                .unwrap(),
            None
        );
        let original = rpc.request(args::ConfigGlobalShowArgs {}).await.unwrap();
        for (key, value) in [
            ("palette", "invalid"),
            ("activity_filter", "invalid"),
            ("unknown", "value"),
        ] {
            super::super::config::global_set(
                &rpc,
                &ctx,
                &crate::output::Styles::plain(),
                key,
                value,
            )
            .await
            .unwrap();
        }
        assert_eq!(
            rpc.request(args::ConfigGlobalShowArgs {}).await.unwrap(),
            original
        );
        super::super::config::global_set(
            &rpc,
            &ctx,
            &crate::output::Styles::plain(),
            "palette",
            "AMBER",
        )
        .await
        .unwrap();
        assert_eq!(
            rpc.request(args::ConfigGlobalShowArgs {})
                .await
                .unwrap()
                .tui
                .palette
                .as_str(),
            "amber"
        );
        harness.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn legacy_org_deployment_and_catalogue_use_typed_rpc() {
        let (harness, rpc) = fixture().await;
        let ctx = CliContext::new(
            harness.project_root().to_path_buf(),
            None,
            true,
            true,
            false,
            EnvFlags::default(),
        );
        let catalog = rpc.request(args::OrgListArgs {}).await.unwrap();
        assert_eq!(catalog.len(), orch_domain::ORG_TEMPLATES.len() + 1);
        assert_eq!(catalog.last().unwrap().key, "a-team");
        assert_eq!(
            super::super::org::deploy(
                &rpc,
                &ctx,
                &crate::output::Styles::plain(),
                "data-lab",
                Some("Analyze the data")
            )
            .await
            .unwrap(),
            ExitCode::SUCCESS
        );
        let teams = rpc.request(args::TeamListArgs {}).await.unwrap();
        assert_eq!(teams.len(), 1);
        let tasks = rpc.request(args::StatusGetArgs {}).await.unwrap();
        let template = orch_domain::get_org_template_by_key("data-lab").unwrap();
        assert_eq!(tasks.agents.len(), template.agents.len());
        let lead_name = template.agents[template.lead_index].name;
        assert_eq!(
            teams[0].lead_agent_id,
            tasks
                .agents
                .iter()
                .find(|agent| agent.name == lead_name)
                .unwrap()
                .id
        );
        let goals = rpc.call::<args::GoalListArgs>(json!({})).await.unwrap();
        assert_eq!(goals.len(), 1);
        assert_eq!(goals[0].title, "Analyze the data");
        harness.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn config_definition_compare_and_swap_rejects_stale_edit() {
        let (harness, rpc) = fixture().await;
        let base = rpc.request(args::ConfigDefinitionGetArgs {}).await.unwrap();
        let update = json!({"extension":{"first":1}});
        super::super::config::apply_definition(&rpc, &update, MutationKind::Patch, &|error| error)
            .await
            .unwrap();
        let error = document_request(&rpc, &json!({"extension":{"stale":true}}), |input| {
            args::ConfigDefinitionApplyArgs {
                definition: args::DefinitionInput {
                    input,
                    kind: args::MutationKind::Patch,
                    base_digest: Some(base.digest),
                },
            }
        })
        .await
        .unwrap_err();
        assert!(
            matches!(super::super::rpc::recover_client_error(&error), Some(orch_client::ClientError::Remote(body)) if body.code == orch_proto::error::ErrorCode::Conflict),
            "{error:?}"
        );
        let current = rpc
            .call::<args::ConfigGetArgs>(json!({"key":"extension"}))
            .await
            .unwrap();
        assert_eq!(current.value, Some(json!({"first":1})));
        harness.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn template_mutations_validate_and_reject_stale_snapshots() {
        let (harness, rpc) = fixture().await;
        let directory = tempfile::tempdir().unwrap();
        let selector = template(&rpc, &directory.path().join("team")).await;
        let base = definition_get(&rpc, selector.clone(), &Target::Org)
            .await
            .unwrap();
        definition_apply(
            &rpc,
            selector.clone(),
            Target::Org,
            &json!({"description":"changed","defaults":{"skills":["shared"]}}),
            MutationKind::Patch,
            None,
        )
        .await
        .unwrap();
        let error = definition_apply(
            &rpc,
            selector.clone(),
            Target::Org,
            &json!({"description":"stale"}),
            MutationKind::Patch,
            Some(base.digest),
        )
        .await
        .unwrap_err();
        assert!(
            matches!(super::super::rpc::recover_client_error(&error), Some(orch_client::ClientError::Remote(body)) if body.code == orch_proto::error::ErrorCode::Conflict),
            "{error:?}"
        );
        for input in [
            json!({"typo":null}),
            json!({"defaults":{"typo":null}}),
            json!({"lead":null}),
        ] {
            assert!(
                definition_apply(
                    &rpc,
                    selector.clone(),
                    Target::Org,
                    &input,
                    MutationKind::Patch,
                    None
                )
                .await
                .is_err()
            );
        }
        let member = Target::Agent("lead".into());
        definition_apply(
            &rpc,
            selector.clone(),
            member.clone(),
            &json!({"skills":[]}),
            MutationKind::Patch,
            None,
        )
        .await
        .unwrap();
        let updated = definition_apply(
            &rpc,
            selector.clone(),
            member,
            &json!({"skills":null}),
            MutationKind::Patch,
            None,
        )
        .await
        .unwrap();
        assert!(updated.get("skills").is_none());
        let agents = rpc
            .request(args::OrgTemplateShowArgs {
                selector,
                resolved: true,
            })
            .await
            .unwrap()
            .resolved_agents
            .unwrap();
        assert_eq!(agents[0].skills, Some(vec!["shared".into()]));
        harness.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn large_template_documents_use_a_session_local_transfer() {
        let (harness, rpc) = fixture().await;
        let directory = tempfile::tempdir().unwrap();
        let selector = template(&rpc, &directory.path().join("team")).await;
        let description = "quoted \" line\n".repeat(60_000);
        assert!(description.len() > 64 * 1024);
        let updated = definition_apply(
            &rpc,
            selector.clone(),
            Target::Org,
            &json!({"description":description}),
            MutationKind::Patch,
            None,
        )
        .await
        .unwrap();
        assert_eq!(updated["description"], description);
        let shown = rpc
            .request(args::OrgTemplateShowArgs {
                selector,
                resolved: false,
            })
            .await
            .unwrap();
        assert_eq!(shown.bundle.org.description, description);
        harness.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn remote_registry_and_preferences_round_trip_via_rpc() {
        let (harness, rpc) = fixture().await;
        let ctx = CliContext::new(
            harness.project_root().to_path_buf(),
            None,
            false,
            true,
            false,
            EnvFlags::default(),
        );
        let styles = crate::output::Styles::plain();
        for command in [
            crate::tree::RemoteSub::Add {
                alias: "worker".into(),
                addr: "host".into(),
            },
            crate::tree::RemoteSub::Listen {
                state: "on".into(),
                addr: None,
            },
        ] {
            super::super::remote::run(&rpc, &ctx, &styles, &command)
                .await
                .unwrap();
        }
        assert_eq!(
            rpc.request(args::RemoteListArgs {}).await.unwrap()[0].addr,
            "host:7690"
        );
        let status = rpc.request(args::RemoteStatusArgs {}).await.unwrap();
        assert_eq!(
            status.listen_configured.as_deref(),
            Some(super::super::remote::DEFAULT_LISTEN)
        );
        assert!(status.listen.is_none());
        super::super::remote::run(
            &rpc,
            &ctx,
            &styles,
            &crate::tree::RemoteSub::Remove {
                alias: "worker".into(),
            },
        )
        .await
        .unwrap();
        assert!(
            rpc.request(args::RemoteListArgs {})
                .await
                .unwrap()
                .is_empty()
        );
        harness.shutdown().await.unwrap();
    }
}

#[cfg(test)]
mod mutation_tests {
    use super::*;
    use orch_domain::org_templates::Bundle;
    use serde_json::json;
    fn error_text(error: Error) -> String {
        super::super::rpc::recover_client_error(&error)
            .map_or_else(|| error.to_string(), |error| error.render(false))
    }

    async fn template() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("team");
        let (harness, rpc) = rpc_tests::fixture().await;
        let path = absolute_path(Path::new("/"), &root).unwrap();
        rpc.call::<args::OrgTemplateInitArgs>(json!({"name":"team","template_dir":path}))
            .await
            .unwrap();
        harness.shutdown().await.unwrap();
        (dir, root)
    }
    async fn change(
        root: &Path,
        target: Target,
        input: Value,
        kind: MutationKind,
    ) -> Result<Bundle> {
        let (harness, rpc) = rpc_tests::fixture().await;
        let selector = Selector {
            name: None,
            template_dir: Some(root.to_path_buf()),
        }
        .wire(Path::new("/"))?;
        let result = async {
            definition_apply(&rpc, selector.clone(), target, &input, kind, None).await?;
            Ok(rpc
                .request(args::OrgTemplateShowArgs {
                    selector,
                    resolved: false,
                })
                .await?
                .bundle)
        }
        .await;
        harness.shutdown().await.unwrap();
        result
    }
    #[tokio::test]
    async fn skills_empty_overrides_and_patch_null_or_update_omission_inherits() {
        let (_dir, root) = template().await;
        change(
            &root,
            Target::Org,
            json!({"defaults":{"skills":["shared"]}}),
            MutationKind::Patch,
        )
        .await
        .unwrap();
        let member = Target::Agent("lead".into());
        for skills in [json!([]), json!(["local"])] {
            let bundle = change(
                &root,
                member.clone(),
                json!({"skills":skills}),
                MutationKind::Patch,
            )
            .await
            .unwrap();
            assert_eq!(
                serde_json::to_value(&bundle.agents[0]).unwrap()["skills"],
                skills
            );
        }
        let bundle = change(
            &root,
            member.clone(),
            json!({"skills":null}),
            MutationKind::Patch,
        )
        .await
        .unwrap();
        assert!(bundle.agents[0].skills.is_none());
        assert_eq!(
            bundle
                .org
                .defaults
                .as_ref()
                .unwrap()
                .skills
                .as_ref()
                .unwrap(),
            &["shared"]
        );
        change(
            &root,
            member.clone(),
            json!({"skills":[]}),
            MutationKind::Patch,
        )
        .await
        .unwrap();
        let bundle = change(
            &root,
            member,
            json!({"name":"lead","role":"Replaced"}),
            MutationKind::Update,
        )
        .await
        .unwrap();
        assert!(bundle.agents[0].skills.is_none());
    }

    #[tokio::test]
    async fn patches_merge_nested_env_and_remove_defaults_without_storing_null() {
        let (_dir, root) = template().await;
        change(
            &root,
            Target::Org,
            json!({"defaults":{"model":"old","env":{"A":"a","B":"b"}}}),
            MutationKind::Patch,
        )
        .await
        .unwrap();
        let bundle = change(
            &root,
            Target::Org,
            json!({"defaults":{"model":null,"env":{"A":null,"C":"c"}}}),
            MutationKind::Patch,
        )
        .await
        .unwrap();
        let defaults = bundle.org.defaults.unwrap();
        assert!(defaults.model.is_none());
        assert_eq!(
            serde_json::to_value(defaults.env.unwrap()).unwrap(),
            json!({"B":"b","C":"c"})
        );
        let bundle = change(
            &root,
            Target::Org,
            json!({"defaults":null}),
            MutationKind::Patch,
        )
        .await
        .unwrap();
        assert!(bundle.org.defaults.is_none());
    }

    #[tokio::test]
    async fn replacement_repairs_malformed_selected_member_but_not_unrelated_corruption() {
        let (_dir, root) = template().await;
        let path = root.join("agents/lead.yml");
        std::fs::write(&path, "broken: [").unwrap();
        let replacement = json!({"name":"lead","role":"Repaired"});
        change(
            &root,
            Target::Agent("lead".into()),
            replacement.clone(),
            MutationKind::Update,
        )
        .await
        .unwrap();
        std::fs::write(root.join("agents/other.yml"), "broken: [").unwrap();
        let before = std::fs::read(&path).unwrap();
        assert!(
            change(
                &root,
                Target::Agent("lead".into()),
                replacement,
                MutationKind::Update
            )
            .await
            .is_err()
        );
        assert_eq!(std::fs::read(path).unwrap(), before);
    }

    #[tokio::test]
    async fn malformed_root_repair_uses_an_explicit_directory() {
        let (_dir, root) = template().await;
        let path = root.join("org.yml");
        let invalid = b"broken: [";
        std::fs::write(&path, invalid).unwrap();
        let replacement = json!({"schema_version":1,"name":"renamed","lead":"lead"});
        let bundle = change(&root, Target::Org, replacement, MutationKind::Update)
            .await
            .unwrap();
        assert_eq!(bundle.org.name, "renamed");
    }

    #[tokio::test]
    async fn patch_requires_a_parseable_base_and_names_full_update_repair() {
        let (_dir, root) = template().await;
        for (target, path) in [
            (Target::Org, "org.yml"),
            (Target::Agent("lead".into()), "agents/lead.yml"),
        ] {
            let path = root.join(path);
            let old = std::fs::read(&path).unwrap();
            std::fs::write(&path, "broken: [").unwrap();
            let error = change(&root, target, json!({}), MutationKind::Patch)
                .await
                .map_err(error_text)
                .unwrap_err();
            assert!(error.contains("template update"), "{error}");
            assert_eq!(std::fs::read(&path).unwrap(), b"broken: [");
            std::fs::write(path, old).unwrap();
        }
    }

    #[tokio::test]
    async fn identity_changes_and_required_removals_fail_without_writes() {
        let (_dir, root) = template().await;
        for (target, patch, path) in [
            (Target::Org, json!({"name":"renamed"}), "org.yml"),
            (Target::Org, json!({"schema_version":2}), "org.yml"),
            (Target::Org, json!({"name":null}), "org.yml"),
            (Target::Org, json!({"lead":null}), "org.yml"),
            (
                Target::Agent("lead".into()),
                json!({"name":"renamed"}),
                "agents/lead.yml",
            ),
            (
                Target::Agent("lead".into()),
                json!({"role":null}),
                "agents/lead.yml",
            ),
        ] {
            let path = root.join(path);
            let old = std::fs::read(&path).unwrap();
            assert!(
                change(&root, target, patch, MutationKind::Patch)
                    .await
                    .is_err()
            );
            assert_eq!(std::fs::read(path).unwrap(), old);
        }
    }

    #[tokio::test]
    async fn no_op_patch_preserves_comments_and_time_but_still_validates_other_members() {
        let (_dir, root) = template().await;
        let path = root.join("agents/lead.yml");
        let bytes = b"# authored\nname: lead\nrole: Coordinate\n";
        std::fs::write(&path, bytes).unwrap();
        let modified = std::fs::metadata(&path).unwrap().modified().unwrap();
        change(
            &root,
            Target::Agent("lead".into()),
            json!({}),
            MutationKind::Patch,
        )
        .await
        .unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        assert_eq!(
            std::fs::metadata(&path).unwrap().modified().unwrap(),
            modified
        );
        std::fs::write(root.join("agents/other.yml"), "broken: [").unwrap();
        assert!(
            change(
                &root,
                Target::Agent("lead".into()),
                json!({}),
                MutationKind::Patch
            )
            .await
            .is_err()
        );
        assert_eq!(std::fs::read(path).unwrap(), bytes);
    }

    #[tokio::test]
    async fn file_mutation_reuses_input_format_and_attaches_source_to_validation_errors() {
        let (dir, root) = template().await;
        let file = dir.path().join("patch.json");
        std::fs::write(&file, "description: Changed\n").unwrap();
        let selector = Selector {
            name: None,
            template_dir: Some(root.clone()),
        };
        let input = FileDefinitionInput {
            file: file.clone(),
            input_format: Some(crate::document_input::InputFormat::Yaml),
        };
        let (harness, rpc) = rpc_tests::fixture().await;
        let wire = selector.wire(Path::new("/")).unwrap();
        let org = mutate_definition(&rpc, wire.clone(), Target::Org, &input, MutationKind::Patch)
            .await
            .unwrap();
        assert_eq!(org["description"], "Changed");
        let before = std::fs::read(root.join("org.yml")).unwrap();
        std::fs::write(&file, "defaults: {typo: null}\n").unwrap();
        let error = mutate_definition(&rpc, wire, Target::Org, &input, MutationKind::Patch)
            .await
            .map_err(error_text)
            .unwrap_err();
        assert!(
            error.contains(file.to_str().unwrap()) && error.contains("defaults.typo"),
            "{error}"
        );
        assert_eq!(std::fs::read(root.join("org.yml")).unwrap(), before);
        harness.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn concurrent_patches_reread_the_base_after_acquiring_the_lock() {
        let (_dir, root) = template().await;
        let (first, second) = tokio::join!(
            change(
                &root,
                Target::Org,
                json!({"description":"Changed"}),
                MutationKind::Patch
            ),
            change(
                &root,
                Target::Org,
                json!({"defaults":{"skills":["shared"]}}),
                MutationKind::Patch
            ),
        );
        first.unwrap();
        second.unwrap();
        let bundle = change(&root, Target::Org, json!({}), MutationKind::Patch)
            .await
            .unwrap();
        assert_eq!(bundle.org.description, "Changed");
        assert_eq!(bundle.org.defaults.unwrap().skills.unwrap(), ["shared"]);
    }
}
