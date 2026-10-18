//! Template authoring commands; runtime entities are owned by engine services.
use crate::context::{CliContext, EnvFlags};
use crate::document_input::{MutationKind, apply_mutation, read_document, reject_nulls};
use crate::output::print_table;
use crate::tree::{Cli, DefinitionInput as FileDefinitionInput};
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
        let bytes = if json {
            let value = match crate::document_input::parse_json(&self.file, &bytes) {
                Ok(value) => value,
                Err(error) => {
                    // Preserve add's existing diagnostics for invalid input;
                    // this check can only reject, never accept invalid JSON.
                    store::parse_document(&self.file, &bytes).await?;
                    return Err(error);
                }
            };
            // Preserve the JSON value, not its source spelling: UTF-16
            // surrogate escapes are valid JSON but invalid YAML escapes.
            serde_yaml_ng::to_string(&value)
                .map_err(|e| Error::other(e.to_string()))?
                .into_bytes()
        } else {
            bytes
        };
        store::parse_document(&self.file, &bytes).await?;
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
            let input = store::parse_document(Path::new("<editor>"), &edited).await?;
            let bundle =
                publish_definition(locked, Target::Org, &input, MutationKind::Update).await?;
            emit(&ctx, &bundle.org)?;
        }
        TemplateSub::Update { selector, input } | TemplateSub::Patch { selector, input } => {
            let kind = if matches!(command, TemplateSub::Patch { .. }) {
                MutationKind::Patch
            } else {
                MutationKind::Update
            };
            let bundle = mutate_definition(selector, Target::Org, input, kind).await?;
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
                let input = store::parse_document(Path::new("<editor>"), &edited).await?;
                let bundle =
                    publish_definition(locked, target.clone(), &input, MutationKind::Update)
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
            MemberSub::Update { selector, input } | MemberSub::Patch { selector, input } => {
                let kind = if matches!(command, MemberSub::Patch { .. }) {
                    MutationKind::Patch
                } else {
                    MutationKind::Update
                };
                let target = selector.target()?;
                let bundle =
                    mutate_definition(&selector.selector(), target.clone(), input, kind).await?;
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
// Keep allowed paths explicit, as in live agent definitions. In particular an
// unknown null must fail before RFC 7396 can erase it from the candidate.
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
fn definition_candidate(
    current: &Value,
    input: &Value,
    kind: MutationKind,
    target: &Target,
) -> Result<Value> {
    let object = input
        .as_object()
        .ok_or_else(|| Error::invalid_arguments("definition: expected an object"))?;
    for (key, value) in object {
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
            return Err(Error::invalid_arguments(format!("{key}: unknown field")));
        }
        if matches!(target, Target::Org)
            && key == "defaults"
            && let Some(defaults) = value.as_object()
        {
            for field in defaults.keys() {
                if !DEFAULT_FIELDS.contains(&field.as_str()) {
                    return Err(Error::invalid_arguments(format!(
                        "defaults.{field}: unknown field"
                    )));
                }
            }
        }
    }
    if kind == MutationKind::Update {
        reject_nulls(input, "definition")?;
    }
    let candidate = apply_mutation(current, input, kind)?;
    reject_nulls(&candidate, "definition")?;
    Ok(candidate)
}
async fn publish_definition(
    locked: LockedTemplate,
    target: Target,
    input: &Value,
    kind: MutationKind,
) -> Result<Bundle> {
    // Only patch needs an old document. Full replacement repairs a bounded
    // malformed target through the existing raw snapshot overlay.
    let current = if kind == MutationKind::Patch {
        locked.document(&target).await?
    } else {
        Value::Null
    };
    let candidate = definition_candidate(&current, input, kind, &target)?;
    let bytes = serde_yaml_ng::to_string(&candidate).map_err(|e| Error::other(e.to_string()))?;
    locked
        .publish(target, Mutation::Replace(bytes.into_bytes()))
        .await
}
async fn mutate_definition(
    selector: &Selector,
    target: Target,
    input: &FileDefinitionInput,
    kind: MutationKind,
) -> Result<Bundle> {
    let value = read_document(&input.file, input.input_format).await?;
    let (root, expected) = selector.writable()?;
    let locked = LockedTemplate::open(&root, expected.as_deref()).await?;
    publish_definition(locked, target, &value, kind)
        .await
        .map_err(|e| crate::document_input::at_source(&input.file, e))
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
mod mutation_tests {
    use super::*;
    use serde_json::json;

    async fn template() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("team");
        store::init_bundle(&root, &minimal("team").unwrap())
            .await
            .unwrap();
        (dir, root)
    }

    async fn change(
        root: &Path,
        target: Target,
        input: Value,
        kind: MutationKind,
    ) -> Result<Bundle> {
        let locked = LockedTemplate::open(root, None).await?;
        publish_definition(locked, target, &input, kind).await
    }

    #[test]
    fn unknown_null_keys_are_rejected_before_merge() {
        for (target, input, field) in [
            (Target::Org, json!({"typo": null}), "typo"),
            (
                Target::Org,
                json!({"defaults": {"typo": null}}),
                "defaults.typo",
            ),
            (Target::Agent("lead".into()), json!({"typo": null}), "typo"),
        ] {
            let error =
                definition_candidate(&json!({}), &input, MutationKind::Patch, &target).unwrap_err();
            assert!(error.to_string().contains(field), "{error}");
        }
    }

    #[test]
    fn full_replacement_rejects_null_at_every_declared_level() {
        for (target, input) in [
            (Target::Org, json!({"defaults": null})),
            (Target::Org, json!({"defaults": {"model": null}})),
            (Target::Agent("lead".into()), json!({"skills": null})),
            (Target::Agent("lead".into()), json!({"env": {"KEY": null}})),
        ] {
            assert!(
                definition_candidate(&Value::Null, &input, MutationKind::Update, &target).is_err()
            );
        }
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
    async fn malformed_root_repair_uses_the_selector_identity() {
        let (_dir, root) = template().await;
        let path = root.join("org.yml");
        let invalid = b"broken: [";
        std::fs::write(&path, invalid).unwrap();
        let replacement = json!({"schema_version":1,"name":"renamed","lead":"lead"});
        let locked = LockedTemplate::open(&root, Some("team")).await.unwrap();
        assert!(
            publish_definition(locked, Target::Org, &replacement, MutationKind::Update)
                .await
                .is_err()
        );
        assert_eq!(std::fs::read(&path).unwrap(), invalid);
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
                .unwrap_err();
            assert!(error.to_string().contains("template update"), "{error}");
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
        let bundle = mutate_definition(&selector, Target::Org, &input, MutationKind::Patch)
            .await
            .unwrap();
        assert_eq!(bundle.org.description, "Changed");
        let before = std::fs::read(root.join("org.yml")).unwrap();
        std::fs::write(&file, "defaults: {typo: null}\n").unwrap();
        let error = mutate_definition(&selector, Target::Org, &input, MutationKind::Patch)
            .await
            .unwrap_err()
            .to_string();
        assert!(
            error.contains(file.to_str().unwrap()) && error.contains("defaults.typo"),
            "{error}"
        );
        assert_eq!(std::fs::read(root.join("org.yml")).unwrap(), before);
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
        let bundle = store::load_directory(&root, None).await.unwrap();
        assert_eq!(bundle.org.description, "Changed");
        assert_eq!(bundle.org.defaults.unwrap().skills.unwrap(), ["shared"]);
    }
}
