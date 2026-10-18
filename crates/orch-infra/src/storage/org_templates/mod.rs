//! Bounded, contained directory snapshots and atomic template publication.
//!
//! Safety covers static hostile bundles and cooperating writers. External
//! changes by the same OS user are outside the containment/locking contract.
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use orch_core::{Error, Result};
use orch_domain::org_templates::{
    AgentDefinition, Bundle, MAX_AGENTS, MAX_BUNDLE_BYTES, MAX_DOCUMENT_BYTES, MAX_RULES,
    OrgDefinition, validate_id,
};
use serde::Serialize;
use serde_json::Value;

use super::fs_utils::{atomic_write_with_mode, owner_only, parse_yaml_bytes};

fn invalid(message: impl Into<String>) -> Error {
    Error::invalid_arguments(message)
}
fn io(path: &Path, err: std::io::Error) -> Error {
    Error::io(
        format!("Template I/O at {}: {err}", path.display()),
        path,
        err,
    )
}

/// Apply the existing alias/expansion guard and the stricter template contract.
/// Parser events bound nesting and reject explicit tags before building a tree.
pub async fn parse_document(path: &Path, bytes: &[u8]) -> Result<Value> {
    parse_document_inner(path, bytes, false).await
}

// Identity recovery must not interpret an invalid nullable field elsewhere in
// a parseable org object as permission to change its name/schema. Null is
// accepted only in this read-only old-root projection; candidates stay strict.
async fn parse_document_inner(path: &Path, bytes: &[u8], allow_null: bool) -> Result<Value> {
    use saphyr_parser::{Event, Parser};
    if bytes.is_empty() || bytes.len() > MAX_DOCUMENT_BYTES {
        return Err(invalid(format!(
            "{}: expected 1–{MAX_DOCUMENT_BYTES} bytes",
            path.display()
        )));
    }
    let text = std::str::from_utf8(bytes)
        .map_err(|_| invalid(format!("{}: expected UTF-8", path.display())))?;
    let mut depth = 0usize;
    for event in Parser::new_from_str(text) {
        let (event, _) = event.map_err(|e| invalid(format!("{}: {e}", path.display())))?;
        match event {
            Event::MappingStart(_, tag) | Event::SequenceStart(_, tag) => {
                if tag.is_some() {
                    return Err(invalid(format!(
                        "{}: explicit YAML tags are forbidden",
                        path.display()
                    )));
                }
                depth += 1;
                if depth > 64 {
                    return Err(invalid(format!(
                        "{}: structural depth exceeds 64",
                        path.display()
                    )));
                }
            }
            Event::MappingEnd | Event::SequenceEnd => depth = depth.saturating_sub(1),
            Event::Scalar(_, _, _, Some(_)) => {
                return Err(invalid(format!(
                    "{}: explicit YAML tags are forbidden",
                    path.display()
                )));
            }
            _ => {}
        }
    }
    let yaml = parse_yaml_bytes(path, bytes)
        .await
        .map_err(|err| match err {
            Error::Serde {
                context,
                path,
                source,
            } => Error::Serde {
                context: format!("{context}: {source}"),
                path,
                source,
            },
            err => err,
        })?;
    fn convert(value: serde_yaml_ng::Value, depth: usize, allow_null: bool) -> Result<Value> {
        use serde_yaml_ng::Value as Y;
        if depth > 64 {
            return Err(invalid("Structural depth exceeds 64"));
        }
        Ok(match value {
            Y::Null if allow_null => Value::Null,
            Y::Null => {
                return Err(invalid(
                    "Explicit null is invalid in template definitions; omit the field",
                ));
            }
            Y::Bool(b) => Value::Bool(b),
            Y::String(s) => Value::String(s),
            Y::Number(n) => serde_json::to_value(n).map_err(|_| invalid("Nonfinite number"))?,
            Y::Sequence(items) => Value::Array(
                items
                    .into_iter()
                    .map(|v| convert(v, depth + 1, allow_null))
                    .collect::<Result<_>>()?,
            ),
            Y::Mapping(items) => {
                let mut out = serde_json::Map::new();
                for (key, value) in items {
                    let Y::String(key) = key else {
                        return Err(invalid("Mapping keys must be strings"));
                    };
                    out.insert(key, convert(value, depth + 1, allow_null)?);
                }
                Value::Object(out)
            }
            Y::Tagged(_) => return Err(invalid("Explicit YAML tags are forbidden")),
        })
    }
    // serde_yaml's JSON-compatible representation rejects nonfinite floats
    // through this explicit check rather than serializing them as null.
    fn finite(value: &serde_yaml_ng::Value) -> bool {
        use serde_yaml_ng::Value as Y;
        match value {
            Y::Number(n) => n.as_f64().is_none_or(f64::is_finite),
            Y::Sequence(s) => s.iter().all(finite),
            Y::Mapping(m) => m.iter().all(|(k, v)| finite(k) && finite(v)),
            _ => true,
        }
    }
    if !finite(&yaml) {
        return Err(invalid(format!("{}: nonfinite number", path.display())));
    }
    let value =
        convert(yaml, 0, allow_null).map_err(|e| invalid(format!("{}: {e}", path.display())))?;
    if !value.is_object() {
        return Err(invalid(format!(
            "{}: definition must be an object",
            path.display()
        )));
    }
    Ok(value)
}

pub fn read_bounded(path: &Path) -> Result<Vec<u8>> {
    let meta = std::fs::symlink_metadata(path).map_err(|e| io(path, e))?;
    if !meta.is_file() || meta.file_type().is_symlink() {
        return Err(invalid(format!(
            "{}: expected regular file without symlinks",
            path.display()
        )));
    }
    if meta.len() > MAX_DOCUMENT_BYTES as u64 {
        return Err(invalid(format!("{}: exceeds 1 MiB", path.display())));
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .map_err(|e| io(path, e))?
        .take((MAX_DOCUMENT_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|e| io(path, e))?;
    if bytes.len() > MAX_DOCUMENT_BYTES {
        return Err(invalid(format!("{}: exceeds 1 MiB", path.display())));
    }
    Ok(bytes)
}

#[derive(Debug, Clone)]
pub struct RawSnapshot {
    pub files: BTreeMap<String, Vec<u8>>,
}

impl RawSnapshot {
    pub fn capture(root: &Path) -> Result<Self> {
        directory(root)?;
        let root = std::fs::canonicalize(root).map_err(|e| io(root, e))?;
        let mut files = BTreeMap::new();
        files.insert("org.yml".into(), read_bounded(&root.join("org.yml"))?);
        for (dir, ext, max) in [("agents", "yml", MAX_AGENTS), ("rules", "md", MAX_RULES)] {
            let path = root.join(dir);
            if !path_present(&path)? {
                // Missing rules/ is valid for a template with no rules.
                if dir == "rules" {
                    continue;
                } else {
                    return Err(invalid("Missing agents/ directory"));
                }
            }
            directory(&path)?;
            let mut count = 0;
            for entry in std::fs::read_dir(&path).map_err(|e| io(&path, e))? {
                let entry = entry.map_err(|e| io(&path, e))?;
                let file = entry.path();
                let meta = std::fs::symlink_metadata(&file).map_err(|e| io(&file, e))?;
                if !meta.is_file() || meta.file_type().is_symlink() {
                    return Err(invalid(format!(
                        "{}: nested or nonregular template entry",
                        file.display()
                    )));
                }
                let name = entry
                    .file_name()
                    .into_string()
                    .map_err(|_| invalid("Non-UTF-8 template filename"))?;
                if name.starts_with('.') {
                    continue;
                }
                if dir == "agents" && file.extension().is_some_and(|e| e == "yaml") {
                    return Err(invalid(format!(
                        "{}: rename .yaml member to .yml",
                        file.display()
                    )));
                }
                if !file.extension().is_some_and(|e| e == ext) {
                    continue;
                }
                let stem = file
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .ok_or_else(|| invalid("Invalid filename"))?;
                validate_id(stem).map_err(invalid)?;
                count += 1;
                if count > max {
                    return Err(invalid(format!("Too many {dir}")));
                }
                files.insert(format!("{dir}/{name}"), read_bounded(&file)?);
                check_budget(&files)?;
            }
        }
        check_budget(&files)?;
        Ok(Self { files })
    }

    pub async fn validate(&self, expected_name: Option<&str>) -> Result<Bundle> {
        check_budget(&self.files)?;
        let org: OrgDefinition = serde_json::from_value(
            parse_document(
                Path::new("org.yml"),
                self.files
                    .get("org.yml")
                    .ok_or_else(|| invalid("Missing org.yml"))?,
            )
            .await?,
        )
        .map_err(|e| invalid(format!("org.yml: {e}")))?;
        if expected_name.is_some_and(|name| name != org.name) {
            return Err(invalid("org.yml name must match the user directory key"));
        }
        let mut agents = Vec::new();
        let mut rules = BTreeMap::new();
        for (path, bytes) in &self.files {
            if let Some(stem) = path
                .strip_prefix("agents/")
                .and_then(|p| p.strip_suffix(".yml"))
            {
                let agent: AgentDefinition =
                    serde_json::from_value(parse_document(Path::new(path), bytes).await?)
                        .map_err(|e| invalid(format!("{path}: {e}")))?;
                if agent.name != stem {
                    return Err(invalid(format!("{path}: name must equal filename stem")));
                }
                agents.push(agent);
            } else if let Some(id) = path
                .strip_prefix("rules/")
                .and_then(|p| p.strip_suffix(".md"))
            {
                let text = std::str::from_utf8(bytes)
                    .map_err(|_| invalid(format!("{path}: expected UTF-8")))?;
                rules.insert(id.to_owned(), text.to_owned());
            }
        }
        agents.sort_by(|a, b| a.name.cmp(&b.name));
        let bundle = Bundle { org, agents, rules };
        bundle.validate().map_err(invalid)?;
        Ok(bundle)
    }
}

fn check_budget(files: &BTreeMap<String, Vec<u8>>) -> Result<()> {
    if files.values().any(|b| b.len() > MAX_DOCUMENT_BYTES)
        || files.values().map(Vec::len).sum::<usize>() > MAX_BUNDLE_BYTES
    {
        return Err(invalid("Template exceeds document/bundle byte limits"));
    }
    if files.keys().filter(|p| p.starts_with("agents/")).count() > MAX_AGENTS
        || files.keys().filter(|p| p.starts_with("rules/")).count() > MAX_RULES
    {
        return Err(invalid("Template exceeds agent/rule count limits"));
    }
    Ok(())
}
fn directory(path: &Path) -> Result<()> {
    // Trailing separators and dots must not make the no-follow check follow a link.
    let normalized: PathBuf = path.components().collect();
    let m = std::fs::symlink_metadata(&normalized).map_err(|e| io(path, e))?;
    if !m.is_dir() || m.file_type().is_symlink() {
        return Err(invalid(format!(
            "{}: expected directory without symlinks",
            path.display()
        )));
    }
    Ok(())
}

pub async fn load_directory(root: &Path, expected: Option<&str>) -> Result<Bundle> {
    RawSnapshot::capture(root)?.validate(expected).await
}

pub async fn builtin(name: &str) -> Result<Bundle> {
    if name == "a-team" {
        return RawSnapshot {
            files: orch_domain::org_templates::builtin::A_TEAM
                .iter()
                .map(|(p, s)| (p.to_string(), s.as_bytes().to_vec()))
                .collect(),
        }
        .validate(Some(name))
        .await;
    }
    // Legacy constants are converted only for the new API; their ordinary
    // deployment/model-tier/skill-filter behavior remains on the legacy route.
    let legacy = orch_domain::get_org_template_by_key(name)
        .ok_or_else(|| invalid(format!("Unknown builtin template {name}")))?;
    let mut agents = Vec::new();
    for entry in legacy.agents {
        let shop = orch_domain::get_shop_template_by_key(entry.shop_key)
            .ok_or_else(|| invalid("Missing legacy shop entry"))?;
        agents.push(AgentDefinition {
            name: entry.name.to_ascii_lowercase().replace(' ', "-"),
            description: shop.description.into(),
            role: shop.role.into(),
            adapter: Some("claude".into()),
            approval_policy: Some(shop.approval_policy),
            skills: Some(shop.skills.iter().map(|s| s.to_string()).collect()),
            ..serde_json::from_value(serde_json::json!({"name":"placeholder","role":"placeholder"}))
                .expect("literal definition")
        });
    }
    let lead = agents[legacy.lead_index].name.clone();
    agents.sort_by(|a, b| a.name.cmp(&b.name));
    let bundle = Bundle {
        org: OrgDefinition {
            schema_version: 1,
            name: name.into(),
            description: legacy.description.into(),
            lead,
            rules: vec![],
            defaults: None,
        },
        agents,
        rules: BTreeMap::new(),
    };
    bundle.validate().map_err(invalid)?;
    Ok(bundle)
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Source {
    Builtin,
    Legacy,
    User { path: PathBuf },
    Directory { path: PathBuf },
}
#[derive(Debug, Clone, Serialize)]
pub struct Loaded {
    pub source: Source,
    #[serde(flatten)]
    pub bundle: Bundle,
}
pub fn user_root() -> PathBuf {
    super::global_config::FileGlobalConfigStore::new()
        .path()
        .parent()
        .expect("global config parent")
        .join("templates")
}

fn path_present(path: &Path) -> Result<bool> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(io(path, e)),
    }
}

/// A broken symlink is an invalid existing source, never an absent source.
pub fn user_template_exists(key: &str) -> Result<bool> {
    validate_id(key).map_err(invalid)?;
    path_present(&user_root().join(key))
}

pub async fn resolve(name: Option<&str>, path: Option<&Path>) -> Result<Loaded> {
    match (name, path) {
        (None, Some(path)) => Ok(Loaded {
            source: Source::Directory {
                path: std::fs::canonicalize(path).map_err(|e| io(path, e))?,
            },
            bundle: load_directory(path, None).await?,
        }),
        (Some(name), None) => {
            let (qualifier, key) = name
                .split_once(':')
                .map_or((None, name), |(q, k)| (Some(q), k));
            validate_id(key).map_err(invalid)?;
            if qualifier.is_some_and(|q| q != "builtin" && q != "user") {
                return Err(invalid("Unknown source qualifier; use builtin: or user:"));
            }
            if qualifier == Some("builtin") {
                return Ok(Loaded {
                    source: if key == "a-team" {
                        Source::Builtin
                    } else {
                        Source::Legacy
                    },
                    bundle: builtin(key).await?,
                });
            }
            let path = user_root().join(key);
            let exists = user_template_exists(key)?;
            if qualifier == Some("user") || (qualifier.is_none() && exists) {
                return Ok(Loaded {
                    source: Source::User { path: path.clone() },
                    bundle: load_directory(&path, Some(key)).await?,
                });
            }
            Ok(Loaded {
                source: if key == "a-team" {
                    Source::Builtin
                } else {
                    Source::Legacy
                },
                bundle: builtin(key).await?,
            })
        }
        _ => Err(invalid(
            "Select exactly one template name or --template-dir",
        )),
    }
}

#[derive(Debug, Clone)]
pub enum Target {
    Org,
    Agent(String),
}
impl Target {
    pub fn path(&self) -> Result<String> {
        Ok(match self {
            Self::Org => "org.yml".into(),
            Self::Agent(name) => {
                validate_id(name).map_err(invalid)?;
                format!("agents/{name}.yml")
            }
        })
    }
}
#[derive(Debug)]
pub enum Mutation {
    Add(Vec<u8>),
    Replace(Vec<u8>),
    Delete,
}

async fn lock(path: &Path) -> Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(nix::libc::O_NOFOLLOW);
    }
    let file = options.open(path).map_err(|e| io(path, e))?;
    if !file.metadata().map_err(|e| io(path, e))?.is_file() {
        return Err(invalid("Lock is not a regular file"));
    }
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        if tokio::time::Instant::now() >= deadline {
            return Err(invalid(format!(
                "Timed out after 5 s locking {}",
                path.display()
            )));
        }
        match file.try_lock() {
            Ok(()) => return Ok(file),
            Err(std::fs::TryLockError::WouldBlock) => {
                tokio::time::sleep(Duration::from_millis(5)).await
            }
            Err(std::fs::TryLockError::Error(e)) => return Err(io(path, e)),
        }
    }
}

/// P2 shares this lock/snapshot/overlay seam. Acquire before reading a patch
/// or editor base; publication owns the lock even if its caller is cancelled.
pub struct LockedTemplate {
    root: PathBuf,
    expected: Option<String>,
    pub snapshot: RawSnapshot,
    _lock: std::fs::File,
}
impl LockedTemplate {
    pub async fn open(root: &Path, expected: Option<&str>) -> Result<Self> {
        directory(root)?;
        let root = std::fs::canonicalize(root).map_err(|e| io(root, e))?;
        let guard = lock(&root.join(".template.lock")).await?;
        let snapshot = RawSnapshot::capture(&root)?;
        Ok(Self {
            root,
            expected: expected.map(str::to_owned),
            snapshot,
            _lock: guard,
        })
    }
    pub async fn document(&self, target: &Target) -> Result<Value> {
        let path = target.path()?;
        let bytes = self
            .snapshot
            .files
            .get(&path)
            .ok_or_else(|| invalid(format!("Missing {path}")))?;
        parse_document(Path::new(&path), bytes).await.map_err(|e| {
            invalid(format!(
                "{e}; use template update with a full valid definition to repair"
            ))
        })
    }
    pub async fn publish(self, target: Target, mutation: Mutation) -> Result<Bundle> {
        tokio::spawn(async move { self.publish_inner(target, mutation).await })
            .await
            .map_err(|e| Error::other(format!("Template publication task: {e}")))?
    }
    async fn publish_inner(self, target: Target, mutation: Mutation) -> Result<Bundle> {
        let path = target.path()?;
        let old = self.snapshot.files.get(&path);
        match &mutation {
            Mutation::Add(_) if old.is_some() => {
                return Err(invalid(format!("{path} already exists")));
            }
            Mutation::Replace(_) | Mutation::Delete if old.is_none() => {
                return Err(invalid(format!("Missing {path}")));
            }
            _ => {}
        }
        if matches!(target, Target::Org) && !matches!(mutation, Mutation::Replace(_)) {
            return Err(invalid("Root requires replacement"));
        }
        let mut candidate = self.snapshot.clone();
        match &mutation {
            Mutation::Add(bytes) | Mutation::Replace(bytes) => {
                candidate.files.insert(path.clone(), bytes.clone());
            }
            Mutation::Delete => {
                candidate.files.remove(&path);
            }
        }
        let bundle = candidate.validate(self.expected.as_deref()).await?;
        if matches!(target, Target::Org) {
            // Bounded malformed roots can be repaired. Parseable identity and
            // schema, including an unsupported schema, are never changed.
            if let Some(bytes) = old
                && let Ok(value) = parse_document_inner(Path::new("org.yml"), bytes, true).await
            {
                let next =
                    parse_document(Path::new("org.yml"), &candidate.files["org.yml"]).await?;
                for field in ["name", "schema_version"] {
                    if value.get(field) != next.get(field) {
                        return Err(invalid(format!("{field} is immutable")));
                    }
                }
            }
        }
        if let Mutation::Replace(bytes) = &mutation
            && let Some(old) = old
        {
            // Validate the entire overlaid bundle first, even for a no-op.
            // Compare typed authoring documents so omitted ordinary defaults
            // and show echoes preserve comments/formatting and file times.
            let unchanged = if old == bytes {
                true
            } else if let Ok(value) = parse_document(Path::new(&path), old).await {
                match &target {
                    Target::Org => serde_json::from_value::<OrgDefinition>(value)
                        .is_ok_and(|org| org == bundle.org),
                    Target::Agent(name) => serde_json::from_value::<AgentDefinition>(value)
                        .is_ok_and(|agent| {
                            bundle.agents.iter().any(|a| a.name == *name && *a == agent)
                        }),
                }
            } else {
                false
            };
            if unchanged {
                return Ok(bundle);
            }
        }
        let dest = self.root.join(&path);
        match mutation {
            Mutation::Delete => std::fs::remove_file(&dest).map_err(|e| io(&dest, e))?,
            Mutation::Replace(bytes) => {
                if old != Some(&bytes) {
                    atomic_write_with_mode(
                        &dest,
                        std::str::from_utf8(&bytes).map_err(|_| invalid("Expected UTF-8"))?,
                        owner_only(),
                    )
                    .await?;
                }
            }
            Mutation::Add(bytes) => {
                let mut tmp =
                    tempfile::NamedTempFile::new_in(dest.parent().expect("member parent"))
                        .map_err(|e| io(&dest, e))?;
                tmp.write_all(&bytes).map_err(|e| io(&dest, e))?;
                tmp.as_file().sync_all().map_err(|e| io(&dest, e))?;
                tmp.persist_noclobber(&dest)
                    .map_err(|e| io(&dest, e.error))?;
            }
        }
        sync_directory(dest.parent().expect("target parent"))?;
        Ok(bundle)
    }
}
pub async fn mutate(
    root: &Path,
    expected: Option<&str>,
    target: Target,
    mutation: Mutation,
) -> Result<Bundle> {
    LockedTemplate::open(root, expected)
        .await?
        .publish(target, mutation)
        .await
}

fn sync_directory(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        std::fs::File::open(path)
            .and_then(|f| f.sync_all())
            .map_err(|e| io(path, e))?;
    }
    Ok(())
}

pub async fn init_bundle(destination: &Path, bundle: &Bundle) -> Result<()> {
    bundle.validate().map_err(invalid)?;
    let mut files = BTreeMap::new();
    files.insert(
        "org.yml".into(),
        serde_json::to_vec_pretty(&bundle.org).map_err(|e| Error::other(e.to_string()))?,
    );
    for agent in &bundle.agents {
        files.insert(
            format!("agents/{}.yml", agent.name),
            serde_json::to_vec_pretty(agent).map_err(|e| Error::other(e.to_string()))?,
        );
    }
    for (id, text) in &bundle.rules {
        files.insert(format!("rules/{id}.md"), text.as_bytes().to_vec());
    }
    RawSnapshot {
        files: files.clone(),
    }
    .validate(None)
    .await?;
    let parent = destination
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent).map_err(|e| io(parent, e))?;
    let _lock = lock(&parent.join(".template-init.lock")).await?;
    if std::fs::symlink_metadata(destination).is_ok() {
        return Err(invalid("Template init destination already exists"));
    }
    let staging = tempfile::Builder::new()
        .prefix(".template-init-")
        .tempdir_in(parent)
        .map_err(|e| io(parent, e))?;
    for dir in ["agents", "rules"] {
        std::fs::create_dir(staging.path().join(dir)).map_err(|e| io(parent, e))?;
    }
    for (path, bytes) in files {
        let file = staging.path().join(path);
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut handle = options.open(&file).map_err(|e| io(&file, e))?;
        handle.write_all(&bytes).map_err(|e| io(&file, e))?;
        handle.sync_all().map_err(|e| io(&file, e))?;
    }
    sync_directory(&staging.path().join("agents"))?;
    sync_directory(&staging.path().join("rules"))?;
    sync_directory(staging.path())?;
    // All cooperating init writers hold the stable parent lock. Static
    // destinations (including empty directories/symlinks) are never replaced.
    if std::fs::symlink_metadata(destination).is_ok() {
        return Err(invalid("Template init destination already exists"));
    }
    std::fs::rename(staging.path(), destination).map_err(|e| io(destination, e))?;
    sync_directory(parent)?;
    Ok(())
}

#[cfg(test)]
mod tests;

/// Account for the actual storage encoding, including YAML-1.1 quoting.
/// The caller supplies an idle record with room for the generated identity.
pub fn validate_record_size(record: &orch_domain::Agent) -> Result<()> {
    let yaml = serde_yaml_ng::to_string(record).map_err(|e| Error::other(e.to_string()))?;
    if super::fs_utils::quote_ambiguous_scalars(&yaml).len() > MAX_DOCUMENT_BYTES {
        return Err(invalid(format!(
            "{}: rendered Agent record exceeds 1 MiB",
            record.name
        )));
    }
    Ok(())
}
