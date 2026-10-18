//! Template authoring commands; runtime entities are owned by engine services.
use crate::context::{CliContext, EnvFlags};
use crate::output::print_table;
use crate::tree::Cli;
use clap::{Args, Subcommand, ValueEnum};
use orch_core::{Error, Result};
use orch_domain::org_templates::{
    AgentDefinition, Bundle, MAX_DOCUMENT_BYTES, OrgDefinition, validate_id,
};
use orch_infra::storage::org_templates::{
    self as store, Loaded, LockedTemplate, Mutation, Source, Target,
};
use serde::Serialize;
use serde_json::Value;
use std::io::Read;
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
    pub async fn load(&self) -> Result<Loaded> {
        store::resolve(self.name.as_deref(), self.template_dir.as_deref()).await
    }
    pub fn writable(&self) -> Result<(PathBuf, Option<String>)> {
        if let Some(path) = &self.template_dir {
            return Ok((path.clone(), None));
        }
        let name = self
            .name
            .as_deref()
            .ok_or_else(|| Error::invalid_arguments("Select a template"))?;
        let (qualifier, key) = name
            .split_once(':')
            .map_or((None, name), |(q, k)| (Some(q), k));
        validate_id(key).map_err(Error::invalid_arguments)?;
        if qualifier.is_some_and(|q| q != "user" && q != "builtin") {
            return Err(Error::invalid_arguments("Unknown source qualifier"));
        }
        let root = store::user_root().join(key);
        if qualifier == Some("builtin")
            || (qualifier.is_none() && !store::user_template_exists(key)?)
        {
            return Err(Error::invalid_arguments(format!(
                "Builtins are read-only; run orch org template init {key}-copy --from builtin:{key}"
            )));
        }
        Ok((root, Some(key.into())))
    }
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
    async fn read(&self) -> Result<Vec<u8>> {
        let bytes = if self.file == Path::new("-") {
            let mut bytes = Vec::new();
            std::io::stdin()
                .take((MAX_DOCUMENT_BYTES + 1) as u64)
                .read_to_end(&mut bytes)
                .map_err(|e| Error::io("Read <stdin>", "<stdin>", e))?;
            bytes
        } else {
            store::read_bounded(&self.file)?
        };
        let json = match self.input_format {
            Some(DefinitionFormat::Json) => true,
            Some(DefinitionFormat::Yaml) => false,
            None => match self.file.extension().and_then(|e| e.to_str()) {
                Some("json") => true,
                Some("yml" | "yaml") => false,
                _ => matches!(
                    bytes.iter().find(|b| !b.is_ascii_whitespace()),
                    Some(b'{' | b'[')
                ),
            },
        };
        // YAML's strict duplicate-key and depth checks run for JSON too. The
        // selected JSON parser remains final; no parser fallback is attempted.
        store::parse_document(&self.file, &bytes).await?;
        if json {
            serde_json::from_slice::<Value>(&bytes).map_err(|e| {
                Error::invalid_arguments(format!("{}: invalid JSON: {e}", self.file.display()))
            })?;
        }
        Ok(bytes)
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
    match command {
        TemplateSub::List => return list(&ctx, env).await,
        TemplateSub::Show { selector, resolved } => {
            let loaded = selector.load().await?;
            let mut value =
                serde_json::to_value(&loaded).map_err(|e| Error::other(e.to_string()))?;
            if *resolved {
                // Project defaults when present, ordinary defaults otherwise.
                use orch_core::storage::ConfigStore;
                let config = orch_infra::storage::FileConfigStore::new(
                    orch_infra::storage::Paths::new(ctx.project_root.clone()),
                )
                .read()
                .await?;
                let inputs = orch_engine::org_templates::resolve_inputs(
                    &loaded.bundle,
                    &config,
                    None,
                    None,
                    orch_infra::storage::org_templates::validate_record_size,
                )?;
                value["resolved_agents"] =
                    serde_json::to_value(inputs).map_err(|e| Error::other(e.to_string()))?;
            }
            emit(&ctx, &value)?;
        }
        TemplateSub::Init {
            name,
            from,
            template_dir,
        } => {
            validate_id(name).map_err(Error::invalid_arguments)?;
            let mut bundle = if let Some(from) = from {
                store::resolve(Some(from), None).await?.bundle
            } else {
                minimal(name)?
            };
            bundle.org.name = name.clone();
            let destination = template_dir
                .clone()
                .unwrap_or_else(|| store::user_root().join(name));
            store::init_bundle(&destination, &bundle).await?;
            emit(&ctx, &serde_json::json!({"name":name,"path":destination}))?;
        }
        TemplateSub::Edit { selector } => {
            let (root, expected) = selector.writable()?;
            let locked = LockedTemplate::open(&root, expected.as_deref()).await?;
            let old = locked.document(&Target::Org).await?;
            let edited = edit_bytes(&old).await?;
            let bundle = locked
                .publish(Target::Org, Mutation::Replace(edited))
                .await?;
            emit(&ctx, &bundle.org)?;
        }
        TemplateSub::Agent { command } => match command {
            MemberSub::List { selector } => emit(&ctx, &selector.load().await?.bundle.agents)?,
            MemberSub::Add { selector, input } => {
                let bytes = input.read().await?;
                let agent: AgentDefinition =
                    serde_json::from_value(store::parse_document(&input.file, &bytes).await?)
                        .map_err(|e| Error::invalid_arguments(e.to_string()))?;
                let (root, expected) = selector.writable()?;
                store::mutate(
                    &root,
                    expected.as_deref(),
                    Target::Agent(agent.name.clone()),
                    Mutation::Add(bytes),
                )
                .await?;
                emit(&ctx, &agent)?;
            }
            MemberSub::Edit { selector } => {
                let (root, expected) = selector.selector().writable()?;
                let target = selector.target()?;
                let locked = LockedTemplate::open(&root, expected.as_deref()).await?;
                let old = locked.document(&target).await?;
                let edited = edit_bytes(&old).await?;
                let bundle = locked
                    .publish(target.clone(), Mutation::Replace(edited))
                    .await?;
                if let Target::Agent(name) = target {
                    emit(
                        &ctx,
                        bundle
                            .agents
                            .iter()
                            .find(|a| a.name == name)
                            .expect("validated target"),
                    )?;
                }
            }
            MemberSub::Delete { selector } => {
                let (root, expected) = selector.selector().writable()?;
                let target = selector.target()?;
                store::mutate(&root, expected.as_deref(), target.clone(), Mutation::Delete).await?;
                if let Target::Agent(name) = target {
                    emit(&ctx, &serde_json::json!({"deleted":name}))?;
                }
            }
        },
    }
    Ok(ExitCode::SUCCESS)
}
async fn edit_bytes(value: &Value) -> Result<Vec<u8>> {
    let initial = serde_yaml_ng::to_string(value).map_err(|e| Error::other(e.to_string()))?;
    let edited = crate::editor::open_in_editor(&initial, ".yml").await?;
    Ok(edited.into_bytes())
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
fn minimal(name: &str) -> Result<Bundle> {
    Ok(Bundle{org:OrgDefinition{schema_version:1,name:name.into(),description:String::new(),lead:"lead".into(),rules:vec![],defaults:None},agents:vec![serde_json::from_value(serde_json::json!({"name":"lead","role":"Coordinate the team and report outcomes."})).map_err(|e|Error::other(e.to_string()))?],rules:Default::default()})
}

#[derive(Serialize)]
struct ListEntry {
    name: String,
    source: Source,
    shadowed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}
async fn list(ctx: &CliContext, env: EnvFlags) -> Result<ExitCode> {
    let mut entries = Vec::new();
    let mut users = std::collections::BTreeSet::new();
    let root = store::user_root();
    match std::fs::read_dir(&root) {
        Ok(dirs) => {
            for entry in dirs {
                let entry = entry.map_err(|e| Error::io("List templates", &root, e))?;
                let name = entry.file_name().to_string_lossy().into_owned();
                if name.starts_with('.') {
                    continue;
                }
                users.insert(name.clone());
                let error = match validate_id(&name) {
                    Err(e) => Some(e),
                    Ok(()) => store::load_directory(&entry.path(), Some(&name))
                        .await
                        .err()
                        .map(|e| e.to_string()),
                };
                entries.push(ListEntry {
                    name,
                    source: Source::User { path: entry.path() },
                    shadowed: false,
                    error,
                });
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(Error::io("List templates", &root, e)),
    }
    for name in std::iter::once("a-team").chain(orch_domain::ORG_TEMPLATES.iter().map(|t| t.key)) {
        entries.push(ListEntry {
            name: name.into(),
            source: if name == "a-team" {
                Source::Builtin
            } else {
                Source::Legacy
            },
            shadowed: users.contains(name),
            error: None,
        });
    }
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    if let Some(format) = ctx.output {
        format.print(&entries)?;
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
                        Source::User { .. } => "user",
                        Source::Builtin => "builtin",
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
    fn template_commands_parse_without_a_project_and_no_fake_patch_exists() {
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
            .is_err()
        );
    }
}
