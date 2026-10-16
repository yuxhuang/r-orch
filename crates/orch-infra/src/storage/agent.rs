//! File-based agent store — ported from
//! `src/infrastructure/storage/agent-store.ts`.

use async_trait::async_trait;
use orch_core::error::{Error, Result};
use orch_core::storage::AgentStore;
use orch_domain::Agent;

use super::fs_utils::{ensure_dir, owner_only, read_yaml, write_yaml_with_mode};
use super::index::{IndexManager, ItemFormat, replace_at_end};
use super::paths::Paths;

#[derive(Debug)]
pub struct FileAgentStore {
    paths: Paths,
    index: IndexManager<Agent>,
}

impl FileAgentStore {
    pub fn new(paths: Paths) -> Self {
        let index = IndexManager::new(paths.agents_dir(), ItemFormat::Yaml);
        FileAgentStore { paths, index }
    }
}

#[async_trait]
impl AgentStore for FileAgentStore {
    /// The index **unsorted** — so the order is last-write order, which is
    /// what `findBestAgent` breaks score ties on (PARITY.md §14.4). Do not add
    /// a sort here.
    async fn list(&self) -> Result<Vec<Agent>> {
        self.index
            .read_index()
            .await?
            .into_iter()
            .map(local_agent)
            .collect()
    }

    async fn get(&self, id: &str) -> Result<Option<Agent>> {
        read_yaml(&self.paths.agent_path(id)?)
            .await?
            .map(local_agent)
            .transpose()
    }

    async fn get_by_name(&self, name: &str) -> Result<Option<Agent>> {
        Ok(self.list().await?.into_iter().find(|a| a.name == name))
    }

    async fn save(&self, agent: &Agent) -> Result<()> {
        super::paths::sanitize_id(&agent.id)?;
        let mut agent = agent.clone();
        agent.capabilities.clear();
        agent.remote = None;
        ensure_dir(&self.paths.agents_dir()).await?;
        // Owner-only: an agent file is the role prompt the agent is run
        // with, i.e. instructions to an AI CLI (security review C3).
        write_yaml_with_mode(&self.paths.agent_path(&agent.id)?, &agent, owner_only()).await?;
        self.index
            .update_index(move |items| replace_at_end(items, agent, |a| &a.id))
            .await
    }

    async fn delete(&self, id: &str) -> Result<()> {
        let path = self.paths.agent_path(id)?;
        match tokio::fs::remove_file(&path).await {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => {
                return Err(Error::io(format!("delete {}", path.display()), &path, err));
            }
        }
        let id = id.to_owned();
        self.index
            .update_index(move |items| items.into_iter().filter(|a| a.id != id).collect())
            .await
    }
}

/// Host capability is a read-time property, including for legacy persisted records.
pub fn host_capability() -> String {
    std::env::var("ORCH_TEST_OS_CAP").unwrap_or_else(|_| std::env::consts::OS.to_owned())
}

fn local_agent(mut agent: Agent) -> Result<Agent> {
    super::paths::sanitize_id(&agent.id)?;
    agent.capabilities = vec![host_capability()];
    agent.remote = None;
    Ok(agent)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::fs_utils::clear_ensured_dirs;
    use orch_core::test_support::make_agent;
    use orch_domain::{AgentConfig, ApprovalPolicy, default_agents};

    fn store(root: &std::path::Path) -> FileAgentStore {
        clear_ensured_dirs();
        FileAgentStore::new(Paths::new(root))
    }

    /// Security review C3: an agent file is the role prompt an AI CLI is run
    /// with, so it is instructions as much as data.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_saved_agent_file_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let store = store(temp.path());
        let saved = agent("agt_1", "nav");
        store.save(&saved).await.unwrap();

        let path = store.paths.agent_path("agt_1").unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            mode,
            0o600,
            "{} is {mode:o}, not owner-only",
            path.display()
        );
    }

    fn agent(id: &str, name: &str) -> Agent {
        Agent {
            id: id.into(),
            name: name.into(),
            ..make_agent()
        }
    }

    fn ids(agents: &[Agent]) -> Vec<&str> {
        agents.iter().map(|a| a.id.as_str()).collect()
    }

    #[tokio::test]
    async fn saves_reads_and_deletes_an_agent() {
        let temp = tempfile::tempdir().unwrap();
        let store = store(temp.path());

        assert!(store.get("agt_test1").await.unwrap().is_none());
        store.save(&make_agent()).await.unwrap();
        assert_eq!(
            store.get("agt_test1").await.unwrap().unwrap().name,
            "TestAgent"
        );
        assert_eq!(
            store.get_by_name("TestAgent").await.unwrap().unwrap().id,
            "agt_test1"
        );
        assert!(store.get_by_name("nope").await.unwrap().is_none());

        store.delete("agt_test1").await.unwrap();
        assert!(store.list().await.unwrap().is_empty());
        store.delete("agt_test1").await.unwrap();
    }

    #[tokio::test]
    async fn list_order_is_last_write_order_and_is_never_sorted() {
        let temp = tempfile::tempdir().unwrap();
        let store = store(temp.path());
        for (id, name) in [("agt_c", "C"), ("agt_a", "A"), ("agt_b", "B")] {
            store.save(&agent(id, name)).await.unwrap();
        }
        // Insertion order, not alphabetical.
        assert_eq!(
            ids(&store.list().await.unwrap()),
            vec!["agt_c", "agt_a", "agt_b"]
        );

        store.save(&agent("agt_c", "C")).await.unwrap();
        assert_eq!(
            ids(&store.list().await.unwrap()),
            vec!["agt_a", "agt_b", "agt_c"]
        );
    }

    #[tokio::test]
    async fn the_bootstrap_agent_round_trips_with_its_multiline_role_intact() {
        let temp = tempfile::tempdir().unwrap();
        let store = store(temp.path());
        let creator = default_agents("claude").into_iter().next().unwrap();
        store.save(&creator).await.unwrap();

        let read_back = store.get("agt_creator").await.unwrap().unwrap();
        assert_eq!(read_back, local_agent(creator).unwrap());
        // The ~3.5 KB role prompt is injected into prompts verbatim.
        assert_eq!(
            read_back.role.as_deref(),
            Some(orch_domain::AGENT_CREATOR_ROLE)
        );
    }

    #[tokio::test]
    async fn an_env_map_round_trips() {
        let temp = tempfile::tempdir().unwrap();
        let store = store(temp.path());
        let a = Agent {
            config: AgentConfig {
                command: Some("echo hi".into()),
                approval_policy: Some(ApprovalPolicy::Auto),
                env: Some(
                    [
                        ("B".to_owned(), "2".to_owned()),
                        ("A".to_owned(), "1".to_owned()),
                    ]
                    .into_iter()
                    .collect(),
                ),
                ..AgentConfig::default()
            },
            ..make_agent()
        };
        store.save(&a).await.unwrap();
        assert_eq!(
            store.get("agt_test1").await.unwrap().unwrap(),
            local_agent(a).unwrap()
        );
    }

    #[tokio::test]
    async fn a_deleted_index_is_rebuilt_from_the_agent_files() {
        let temp = tempfile::tempdir().unwrap();
        let store = store(temp.path());
        store.save(&make_agent()).await.unwrap();
        tokio::fs::remove_file(store.index.index_path())
            .await
            .unwrap();
        assert_eq!(store.list().await.unwrap().len(), 1);
    }
    #[tokio::test]
    async fn capabilities_are_host_derived_and_overlay_fields_never_persist() {
        let temp = tempfile::tempdir().unwrap();
        let store = store(temp.path());
        let mut agent = make_agent();
        agent.capabilities = vec!["fictional-platform".into()];
        agent.remote = Some(orch_domain::RemoteRef {
            alias: "w1".into(),
            online: true,
        });
        store.save(&agent).await.unwrap();
        let saved = tokio::fs::read_to_string(store.paths.agent_path(&agent.id).unwrap())
            .await
            .unwrap();
        assert!(!saved.contains("capabilities:"));
        assert!(!saved.contains("remote:"));
        let index = tokio::fs::read_to_string(store.index.index_path())
            .await
            .unwrap();
        assert!(!index.contains("capabilities"));
        assert!(!index.contains("remote"));
        let read = store.get(&agent.id).await.unwrap().unwrap();
        assert_eq!(read.capabilities, vec![host_capability()]);
        assert!(read.remote.is_none());
        assert_eq!(
            store.list().await.unwrap()[0].capabilities,
            vec![host_capability()]
        );
    }

    #[tokio::test]
    async fn local_store_rejects_remote_ids_on_mutation_and_index_ingest() {
        let temp = tempfile::tempdir().unwrap();
        let store = store(temp.path());
        let mut agent = make_agent();
        agent.id = "agt_test1@w1".into();
        assert!(store.save(&agent).await.is_err());
        assert!(store.delete(&agent.id).await.is_err());
        assert!(store.get(&agent.id).await.is_err());
        super::super::fs_utils::write_json(&store.index.index_path(), &vec![agent])
            .await
            .unwrap();
        assert!(store.list().await.is_err());
    }
}
