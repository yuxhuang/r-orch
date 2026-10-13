//! File-based agent store — ported from
//! `src/infrastructure/storage/agent-store.ts`.

use async_trait::async_trait;
use orch_core::error::{Error, Result};
use orch_core::storage::AgentStore;
use orch_domain::Agent;

use super::fs_utils::{ensure_dir, read_yaml, write_yaml};
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
        self.index.read_index().await
    }

    async fn get(&self, id: &str) -> Result<Option<Agent>> {
        read_yaml(&self.paths.agent_path(id)?).await
    }

    async fn get_by_name(&self, name: &str) -> Result<Option<Agent>> {
        Ok(self.list().await?.into_iter().find(|a| a.name == name))
    }

    async fn save(&self, agent: &Agent) -> Result<()> {
        ensure_dir(&self.paths.agents_dir()).await?;
        write_yaml(&self.paths.agent_path(&agent.id)?, agent).await?;
        let agent = agent.clone();
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
        assert_eq!(read_back, creator);
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
        assert_eq!(store.get("agt_test1").await.unwrap().unwrap(), a);
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
}
