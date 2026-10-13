//! File-based team store — ported from
//! `src/infrastructure/storage/team-store.ts`.
//!
//! **No `_index.json`** — `list` scans the directory, so its order is
//! filesystem enumeration order and is **not contract** (PARITY.md §6.10).
//! Nothing may rely on it.

use async_trait::async_trait;
use orch_core::error::{Error, Result};
use orch_core::storage::TeamStore;
use orch_domain::Team;

use super::fs_utils::{ensure_dir, list_files, read_yaml, write_yaml};
use super::paths::Paths;

#[derive(Debug)]
pub struct FileTeamStore {
    paths: Paths,
}

impl FileTeamStore {
    pub fn new(paths: Paths) -> Self {
        FileTeamStore { paths }
    }
}

#[async_trait]
impl TeamStore for FileTeamStore {
    async fn save(&self, team: &Team) -> Result<()> {
        ensure_dir(&self.paths.teams_dir()).await?;
        write_yaml(&self.paths.team_path(&team.id)?, team).await
    }

    async fn get(&self, id: &str) -> Result<Option<Team>> {
        read_yaml(&self.paths.team_path(id)?).await
    }

    async fn get_by_name(&self, name: &str) -> Result<Option<Team>> {
        Ok(self.list().await?.into_iter().find(|t| t.name == name))
    }

    async fn list(&self) -> Result<Vec<Team>> {
        ensure_dir(&self.paths.teams_dir()).await?;
        let files = list_files(&self.paths.teams_dir(), Some(".yml")).await?;

        let mut teams = Vec::with_capacity(files.len());
        for file in files {
            let id = file.strip_suffix(".yml").unwrap_or(&file);
            // An unreadable file drops out, as the TS `filter(t => t !== null)`
            // does for a null read.
            if let Some(team) = read_yaml(&self.paths.team_path(id)?).await? {
                teams.push(team);
            }
        }
        Ok(teams)
    }

    async fn delete(&self, id: &str) -> Result<()> {
        let path = self.paths.team_path(id)?;
        match tokio::fs::remove_file(&path).await {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(Error::io(format!("delete {}", path.display()), &path, err)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::fs_utils::clear_ensured_dirs;
    use orch_domain::{DEFAULT_TEAM_CONFIG, TeamMember, TeamMemberRole, TeamStatus};
    use std::path::Path;

    fn store(root: &Path) -> FileTeamStore {
        clear_ensured_dirs();
        FileTeamStore::new(Paths::new(root))
    }

    fn team(id: &str, name: &str) -> Team {
        Team {
            id: id.into(),
            name: name.into(),
            description: None,
            status: TeamStatus::Active,
            members: vec![TeamMember {
                agent_id: "agt_creator".into(),
                role: TeamMemberRole::Lead,
                joined_at: "2026-10-13T16:42:11.108Z".into(),
            }],
            task_pool: vec![],
            lead_agent_id: "agt_creator".into(),
            created_at: "2026-10-13T16:42:11.108Z".into(),
            updated_at: "2026-10-13T16:42:11.108Z".into(),
            config: DEFAULT_TEAM_CONFIG,
        }
    }

    #[tokio::test]
    async fn saves_reads_and_deletes_a_team() {
        let temp = tempfile::tempdir().unwrap();
        let store = store(temp.path());
        let t = team("team_1", "squad");

        assert!(store.get("team_1").await.unwrap().is_none());
        store.save(&t).await.unwrap();
        assert_eq!(store.get("team_1").await.unwrap().unwrap(), t);
        assert_eq!(
            store.get_by_name("squad").await.unwrap().unwrap().id,
            "team_1"
        );
        assert!(store.get_by_name("nope").await.unwrap().is_none());

        store.delete("team_1").await.unwrap();
        assert!(store.list().await.unwrap().is_empty());
        store.delete("team_1").await.unwrap();
    }

    #[tokio::test]
    async fn there_is_no_index_file() {
        // PARITY.md §6.10: TeamStore scans its directory.
        let temp = tempfile::tempdir().unwrap();
        let store = store(temp.path());
        store.save(&team("team_1", "squad")).await.unwrap();
        store.list().await.unwrap();

        let files = list_files(&store.paths.teams_dir(), None).await.unwrap();
        assert_eq!(files, vec!["team_1.yml".to_owned()]);
    }

    #[tokio::test]
    async fn lists_every_team_without_depending_on_order() {
        let temp = tempfile::tempdir().unwrap();
        let store = store(temp.path());
        for (id, name) in [("team_c", "C"), ("team_a", "A"), ("team_b", "B")] {
            store.save(&team(id, name)).await.unwrap();
        }

        // Directory order is not contract, so assert on the set.
        let mut ids: Vec<String> = store
            .list()
            .await
            .unwrap()
            .into_iter()
            .map(|t| t.id)
            .collect();
        ids.sort();
        assert_eq!(ids, vec!["team_a", "team_b", "team_c"]);
    }

    #[tokio::test]
    async fn listing_an_absent_teams_directory_is_empty() {
        let temp = tempfile::tempdir().unwrap();
        let store = store(temp.path());
        assert!(store.list().await.unwrap().is_empty());
        assert!(store.get_by_name("squad").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn the_nested_member_timestamp_is_quoted() {
        let temp = tempfile::tempdir().unwrap();
        let store = store(temp.path());
        store.save(&team("team_1", "squad")).await.unwrap();

        let raw = tokio::fs::read_to_string(store.paths.team_path("team_1").unwrap())
            .await
            .unwrap();
        // Inside a sequence item, which is the case a line-based guard can
        // most easily miss.
        assert!(
            raw.contains("joined_at: '2026-10-13T16:42:11.108Z'"),
            "{raw}"
        );
        assert_eq!(
            store.get("team_1").await.unwrap().unwrap(),
            team("team_1", "squad")
        );
    }

    #[tokio::test]
    async fn a_team_config_round_trips_with_its_defaults() {
        let temp = tempfile::tempdir().unwrap();
        let store = store(temp.path());
        store.save(&team("team_1", "squad")).await.unwrap();

        let raw = tokio::fs::read_to_string(store.paths.team_path("team_1").unwrap())
            .await
            .unwrap();
        assert!(raw.contains("auto_claim: true"), "{raw}");
        assert!(raw.contains("message_ttl_ms: 86400000"), "{raw}");
        // `max_concurrent_tasks` is unset, so it is omitted.
        assert!(!raw.contains("max_concurrent_tasks"), "{raw}");
    }

    #[tokio::test]
    async fn an_invalid_id_is_rejected() {
        let temp = tempfile::tempdir().unwrap();
        let store = store(temp.path());
        assert!(store.get("a/b").await.is_err());
        assert!(store.delete("../escape").await.is_err());
    }
}
