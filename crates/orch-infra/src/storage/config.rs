//! File-based config store.
//!
//! Everything goes through the **raw merged value**, never through a typed
//! `OrchestratorConfig`: the store deep-merges the parsed YAML over
//! `DEFAULT_CONFIG` as plain objects, so unknown keys in a hand-authored
//! `config.yml` survive a `config set` round trip. Deserializing to the typed
//! struct and writing it back would silently delete them.

use async_trait::async_trait;
use orch_core::error::{Error, Result};
use orch_core::storage::ConfigStore;
use orch_domain::{OrchestratorConfig, deep_merge, default_config, get_by_path, set_by_path};
use serde_json::Value;

use super::fs_utils::{owner_only, read_yaml_value, write_yaml_with_mode};
use super::paths::Paths;

#[derive(Debug)]
pub struct FileConfigStore {
    paths: Paths,
}

impl FileConfigStore {
    pub fn new(paths: Paths) -> Self {
        FileConfigStore { paths }
    }

    /// The on-disk file deep-merged over the defaults, as a value tree.
    /// A missing file yields the defaults.
    pub async fn read_merged(&self) -> Result<Value> {
        let defaults = serde_json::to_value(default_config())
            .map_err(|err| Error::other(format!("serialize default config: {err}")))?;

        let path = self.paths.config_path();
        let Some(on_disk) = read_yaml_value(&path).await? else {
            return Ok(defaults);
        };
        // YAML → JSON value so one merge implementation serves both.
        let on_disk: Value = serde_json::to_value(&on_disk)
            .map_err(|err| Error::serde(format!("convert {}", path.display()), &path, err))?;

        // An empty file parses as `null` and is merged as `{}`. Anything that
        // is not a mapping gets the same treatment rather than replacing the
        // whole tree.
        if !on_disk.is_object() {
            return Ok(defaults);
        }
        Ok(deep_merge(&defaults, &on_disk))
    }

    /// Exact authoring data, without materializing defaults or dropping extensions.
    pub async fn read_raw(&self) -> Result<Value> {
        use tokio::io::AsyncReadExt;
        let path = self.paths.config_path();
        let file = match tokio::fs::File::open(&path).await {
            Ok(file) => file,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                return Ok(serde_json::json!({}));
            }
            Err(err) => return Err(Error::io("read raw config", &path, err)),
        };
        let mut bytes = Vec::new();
        file.take(1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .await
            .map_err(|err| Error::io("read raw config", &path, err))?;
        let value = super::fs_utils::parse_definition_yaml_bytes(&path, &bytes).await?;
        if !value.is_object() {
            return Err(Error::invalid_arguments("config: expected an object"));
        }
        Ok(value)
    }

    /// Validate the effective known configuration but persist the raw candidate.
    /// The caller holds the definition lock across its read/merge/write operation.
    pub async fn write_raw(&self, value: &Value, complete: bool) -> Result<()> {
        validate_raw_config(value, complete)?;
        super::fs_utils::check_definition_write_size(value)?;
        self.write_merged(value).await
    }

    async fn write_merged(&self, value: &Value) -> Result<()> {
        // Owner-only: this file carries adapter commands and the names of the
        // environment variables holding API keys (security review C3).
        write_yaml_with_mode(&self.paths.config_path(), value, owner_only()).await
    }
}

/// Present known fields must be well-typed, even where defaults or Option could
/// otherwise hide nulls. Unknown extension keys (including literal nulls) survive.
pub fn validate_raw_config(value: &Value, complete: bool) -> Result<()> {
    if !value.is_object() {
        return Err(Error::invalid_arguments("config: expected an object"));
    }
    let defaults =
        serde_json::to_value(default_config()).map_err(|_| Error::other("serialize defaults"))?;
    fn check_present(value: &Value, schema: &Value, path: &str, complete: bool) -> Result<()> {
        if schema.is_object() {
            let map = value
                .as_object()
                .ok_or_else(|| Error::invalid_arguments(format!("{path}: expected an object")))?;
            for (key, expected) in schema.as_object().expect("object") {
                if let Some(present) = map.get(key) {
                    check_present(present, expected, &format!("{path}.{key}"), complete)?;
                } else if complete {
                    return Err(Error::invalid_arguments(format!(
                        "{path}.{key}: required field"
                    )));
                }
            }
        } else {
            let valid = match schema {
                Value::String(_) => value.is_string(),
                Value::Number(_) => value.as_u64().is_some(),
                _ => false,
            };
            if !valid {
                return Err(Error::invalid_arguments(format!(
                    "{path}: invalid field type"
                )));
            }
        }
        Ok(())
    }
    check_present(value, &defaults, "config", complete)?;
    for (section, fields) in [
        ("project", &["description"][..]),
        (
            "prompt",
            &["template", "system_template", "user_template"][..],
        ),
        ("peer", &["name", "listen"][..]),
    ] {
        if let Some(present) = value.get(section) {
            let map = present.as_object().ok_or_else(|| {
                Error::invalid_arguments(format!("config.{section}: expected an object"))
            })?;
            for field in fields {
                if map.get(*field).is_some_and(|v| !v.is_string()) {
                    return Err(Error::invalid_arguments(format!(
                        "config.{section}.{field}: expected a string"
                    )));
                }
            }
        }
    }
    let effective = deep_merge(&defaults, value);
    let config: OrchestratorConfig =
        serde_json::from_value(if complete { value.clone() } else { effective }).map_err(|_| {
            Error::invalid_arguments(
                "config: missing required field or invalid known field type/enum",
            )
        })?;
    if config.project.name.trim().is_empty() || config.defaults.agent.adapter.trim().is_empty() {
        return Err(Error::invalid_arguments(
            "config.project.name/defaults.agent.adapter: cannot be blank",
        ));
    }
    let agent = &config.defaults.agent;
    let scheduling = &config.scheduling;
    if agent.max_turns == 0
        || agent.timeout_ms == 0
        || agent.stall_timeout_ms == 0
        || config.defaults.task.max_attempts == 0
        || scheduling.max_concurrent_agents == 0
        || scheduling.poll_interval_ms == 0
        || scheduling.retry_base_delay_ms == 0
        || scheduling.retry_max_delay_ms == 0
    {
        return Err(Error::invalid_arguments(
            "config: runtime limits and concurrency must be positive",
        ));
    }
    if !(1..=4).contains(&config.defaults.task.priority) {
        return Err(Error::invalid_arguments(
            "config.defaults.task.priority: expected integer between 1 and 4",
        ));
    }
    Ok(())
}

#[async_trait]
impl ConfigStore for FileConfigStore {
    async fn read(&self) -> Result<OrchestratorConfig> {
        let merged = self.read_merged().await?;
        let path = self.paths.config_path();
        serde_json::from_value(merged)
            .map_err(|err| Error::serde(format!("parse {}", path.display()), &path, err))
    }

    async fn write(&self, config: &OrchestratorConfig) -> Result<()> {
        let value = serde_json::to_value(config)
            .map_err(|err| Error::other(format!("serialize config: {err}")))?;
        self.write_merged(&value).await
    }

    async fn get(&self, key_path: &str) -> Result<Option<Value>> {
        Ok(get_by_path(&self.read_merged().await?, key_path).cloned())
    }

    async fn set(&self, key_path: &str, value: Value) -> Result<()> {
        let mut merged = self.read_merged().await?;
        set_by_path(&mut merged, key_path, value);
        self.write_merged(&merged).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::fs_utils::{atomic_write, clear_ensured_dirs, ensure_dir};
    use orch_domain::{ApprovalPolicy, WorkspaceMode};
    use serde_json::json;

    #[tokio::test]
    async fn raw_mutations_preserve_extensions_and_restore_deleted_defaults() {
        let temp = tempfile::tempdir().unwrap();
        let store = store(temp.path()).await;
        let raw = json!({"project":{"name":"custom"}, "extension":{"null":null,"values":[1,2]}});
        store.write_raw(&raw, false).await.unwrap();
        assert_eq!(store.read_raw().await.unwrap(), raw);
        assert_eq!(
            store.read().await.unwrap().scheduling,
            default_config().scheduling
        );
        assert!(
            store
                .write_raw(&json!({"scheduling":{"max_concurrent_agents":0}}), false)
                .await
                .is_err()
        );
        assert!(
            store
                .write_raw(&json!({"project":{"description":null}}), false)
                .await
                .is_err()
        );
        assert!(store.write_raw(&json!({}), true).await.is_err());
        assert_eq!(store.read_raw().await.unwrap(), raw);
    }

    async fn store(root: &std::path::Path) -> FileConfigStore {
        clear_ensured_dirs();
        let paths = Paths::new(root);
        ensure_dir(&paths.root()).await.unwrap();
        FileConfigStore::new(paths)
    }

    /// Security review C3: the file carries adapter commands and the names of
    /// the environment variables holding API keys, so it must not be readable
    /// by other users of a shared machine.
    #[cfg(unix)]
    #[tokio::test]
    async fn the_written_config_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let store = store(temp.path()).await;
        store.write(&default_config()).await.unwrap();

        let mode = std::fs::metadata(store.paths.config_path())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "config.yml is {mode:o}, not owner-only");
    }

    #[tokio::test]
    async fn a_missing_file_reads_as_the_defaults() {
        let temp = tempfile::tempdir().unwrap();
        let store = store(temp.path()).await;
        let config = store.read().await.unwrap();
        assert_eq!(config, default_config());
    }

    #[tokio::test]
    async fn a_partial_file_merges_over_the_defaults() {
        let temp = tempfile::tempdir().unwrap();
        let store = store(temp.path()).await;
        atomic_write(
            &store.paths.config_path(),
            "project:\n  name: proj\nscheduling:\n  max_concurrent_agents: 2\n",
        )
        .await
        .unwrap();

        let config = store.read().await.unwrap();
        assert_eq!(config.project.name, "proj");
        assert_eq!(config.scheduling.max_concurrent_agents, 2);
        // Untouched keys keep their defaults.
        assert_eq!(config.scheduling.poll_interval_ms, 10_000);
        assert_eq!(config.defaults.agent.adapter, "claude");
        assert_eq!(
            config.defaults.agent.workspace_mode,
            WorkspaceMode::Worktree
        );
    }

    #[tokio::test]
    async fn written_config_matches_the_init_file_shape() {
        let temp = tempfile::tempdir().unwrap();
        let store = store(temp.path()).await;
        let mut config = default_config();
        config.project.name = "proj".into();
        store.write(&config).await.unwrap();

        // As written by `init`.
        assert_eq!(
            tokio::fs::read_to_string(store.paths.config_path())
                .await
                .unwrap(),
            "project:\n  name: proj\ndefaults:\n  agent:\n    adapter: claude\n    approval_policy: auto\n    max_turns: 50\n    timeout_ms: 3600000\n    stall_timeout_ms: 600000\n    workspace_mode: worktree\n    prompt_mode: normal\n  task:\n    max_attempts: 3\n    priority: 3\nscheduling:\n  poll_interval_ms: 10000\n  max_concurrent_agents: 6\n  retry_base_delay_ms: 10000\n  retry_max_delay_ms: 300000\n"
        );
    }

    #[tokio::test]
    async fn get_and_set_use_dot_paths() {
        let temp = tempfile::tempdir().unwrap();
        let store = store(temp.path()).await;
        assert_eq!(
            store.get("defaults.agent.max_turns").await.unwrap(),
            Some(json!(50))
        );
        assert_eq!(
            store.get("project.name").await.unwrap(),
            Some(json!("my-project"))
        );
        assert_eq!(store.get("nope").await.unwrap(), None);
        assert_eq!(
            store.get("defaults.agent.max_turns.deeper").await.unwrap(),
            None
        );

        store
            .set("defaults.agent.approval_policy", json!("manual"))
            .await
            .unwrap();
        assert_eq!(
            store.read().await.unwrap().defaults.agent.approval_policy,
            ApprovalPolicy::Manual
        );
    }

    #[tokio::test]
    async fn set_creates_missing_intermediate_objects() {
        let temp = tempfile::tempdir().unwrap();
        let store = store(temp.path()).await;
        store
            .set("prompt.system_template", json!("custom.md"))
            .await
            .unwrap();
        assert_eq!(
            store
                .read()
                .await
                .unwrap()
                .prompt
                .unwrap()
                .system_template
                .as_deref(),
            Some("custom.md")
        );
    }

    #[tokio::test]
    async fn unknown_keys_survive_a_set_round_trip() {
        let temp = tempfile::tempdir().unwrap();
        let store = store(temp.path()).await;
        atomic_write(
            &store.paths.config_path(),
            "project:\n  name: proj\nmy_extension:\n  k: v\n",
        )
        .await
        .unwrap();

        store.set("project.name", json!("renamed")).await.unwrap();

        let raw = tokio::fs::read_to_string(store.paths.config_path())
            .await
            .unwrap();
        assert!(
            raw.contains("my_extension"),
            "unknown key was dropped:\n{raw}"
        );
        assert_eq!(store.get("my_extension.k").await.unwrap(), Some(json!("v")));
        assert_eq!(
            store.get("project.name").await.unwrap(),
            Some(json!("renamed"))
        );
    }

    #[tokio::test]
    async fn merge_keys_resolve_as_yaml_1_1_defines_them() {
        // Reachable only in a hand-authored file.
        let temp = tempfile::tempdir().unwrap();
        let store = store(temp.path()).await;
        atomic_write(
            &store.paths.config_path(),
            "base: &b\n  max_concurrent_agents: 99\n  poll_interval_ms: 5\nscheduling:\n  <<: *b\n",
        )
        .await
        .unwrap();

        let config = store.read().await.unwrap();
        assert_eq!(config.scheduling.max_concurrent_agents, 99);
        assert_eq!(config.scheduling.poll_interval_ms, 5);
        assert_eq!(
            store.get("scheduling.max_concurrent_agents").await.unwrap(),
            Some(json!(99))
        );
    }

    #[tokio::test]
    async fn an_empty_file_reads_as_the_defaults() {
        let temp = tempfile::tempdir().unwrap();
        let store = store(temp.path()).await;
        atomic_write(&store.paths.config_path(), "").await.unwrap();
        // An empty YAML document is `null`, which deep_merge replaces with —
        // so the result must still be usable.
        assert_eq!(store.read().await.unwrap(), default_config());
    }
}
