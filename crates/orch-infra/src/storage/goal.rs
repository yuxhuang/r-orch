//! File-based goal store — ported from
//! `src/infrastructure/storage/goal-store.ts`.

use async_trait::async_trait;
use orch_core::error::{Error, Result};
use orch_core::storage::{GoalFilter, GoalStore};
use orch_domain::{Goal, goal_status_order};

use super::fs_utils::{ensure_dir, read_yaml, write_yaml};
use super::index::{IndexManager, ItemFormat, replace_at_end};
use super::paths::Paths;

#[derive(Debug)]
pub struct FileGoalStore {
    paths: Paths,
    index: IndexManager<Goal>,
}

impl FileGoalStore {
    pub fn new(paths: Paths) -> Self {
        let index = IndexManager::new(paths.goals_dir(), ItemFormat::Yaml);
        FileGoalStore { paths, index }
    }
}

#[async_trait]
impl GoalStore for FileGoalStore {
    /// Sorted by the canonical status order
    /// (`active < paused < achieved < abandoned`), then by `updated_at`
    /// **descending**.
    ///
    /// `sort_by`, never `sort_unstable_by`: ties keep index order, which is
    /// last-write order (PARITY.md §14.4).
    async fn list(&self, filter: &GoalFilter) -> Result<Vec<Goal>> {
        let mut goals: Vec<Goal> = self
            .index
            .read_index()
            .await?
            .into_iter()
            .filter(|g| filter.matches(g))
            .collect();

        goals.sort_by(|a, b| {
            goal_status_order(a.status)
                .cmp(&goal_status_order(b.status))
                .then_with(|| {
                    // `updated_at` is optional on a goal; absent sorts as "".
                    let a_time = a.updated_at.as_deref().unwrap_or("");
                    let b_time = b.updated_at.as_deref().unwrap_or("");
                    b_time.cmp(a_time)
                })
        });
        Ok(goals)
    }

    async fn get(&self, id: &str) -> Result<Option<Goal>> {
        read_yaml(&self.paths.goal_path(id)?).await
    }

    async fn save(&self, goal: &Goal) -> Result<()> {
        ensure_dir(&self.paths.goals_dir()).await?;
        write_yaml(&self.paths.goal_path(&goal.id)?, goal).await?;
        let goal = goal.clone();
        self.index
            .update_index(move |items| replace_at_end(items, goal, |g| &g.id))
            .await
    }

    async fn delete(&self, id: &str) -> Result<()> {
        let path = self.paths.goal_path(id)?;
        match tokio::fs::remove_file(&path).await {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => return Err(Error::io(format!("delete {}", path.display()), &path, err)),
        }
        let id = id.to_owned();
        self.index
            .update_index(move |items| items.into_iter().filter(|g| g.id != id).collect())
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::fs_utils::clear_ensured_dirs;
    use orch_domain::GoalStatus;
    use std::path::Path;

    fn store(root: &Path) -> FileGoalStore {
        clear_ensured_dirs();
        FileGoalStore::new(Paths::new(root))
    }

    fn goal(id: &str, status: GoalStatus, updated_at: Option<&str>) -> Goal {
        Goal {
            id: id.into(),
            title: "Ship v2".into(),
            description: "Get it out".into(),
            status,
            assignee: None,
            created_at: "2026-10-13T16:42:02.793Z".into(),
            updated_at: updated_at.map(str::to_owned),
        }
    }

    fn ids(goals: &[Goal]) -> Vec<&str> {
        goals.iter().map(|g| g.id.as_str()).collect()
    }

    #[tokio::test]
    async fn saves_reads_and_deletes_a_goal() {
        let temp = tempfile::tempdir().unwrap();
        let store = store(temp.path());
        let g = goal(
            "goal_1",
            GoalStatus::Active,
            Some("2026-10-13T16:42:02.793Z"),
        );

        assert!(store.get("goal_1").await.unwrap().is_none());
        store.save(&g).await.unwrap();
        assert_eq!(store.get("goal_1").await.unwrap().unwrap(), g);

        store.delete("goal_1").await.unwrap();
        assert!(store.list(&GoalFilter::default()).await.unwrap().is_empty());
        store.delete("goal_1").await.unwrap();
    }

    #[tokio::test]
    async fn timestamps_are_quoted_so_the_ts_binary_reads_them_as_strings() {
        let temp = tempfile::tempdir().unwrap();
        let store = store(temp.path());
        store
            .save(&goal(
                "goal_1",
                GoalStatus::Active,
                Some("2026-10-13T16:42:02.793Z"),
            ))
            .await
            .unwrap();

        let raw = tokio::fs::read_to_string(store.paths.goal_path("goal_1").unwrap())
            .await
            .unwrap();
        assert!(
            raw.contains("created_at: '2026-10-13T16:42:02.793Z'"),
            "{raw}"
        );
        assert!(
            raw.contains("updated_at: '2026-10-13T16:42:02.793Z'"),
            "{raw}"
        );
    }

    #[tokio::test]
    async fn list_sorts_by_canonical_status_then_newest_first() {
        let temp = tempfile::tempdir().unwrap();
        let store = store(temp.path());
        for g in [
            goal("goal_abandoned", GoalStatus::Abandoned, Some("t1")),
            goal("goal_active_old", GoalStatus::Active, Some("t1")),
            goal("goal_achieved", GoalStatus::Achieved, Some("t1")),
            goal("goal_active_new", GoalStatus::Active, Some("t2")),
            goal("goal_paused", GoalStatus::Paused, Some("t1")),
        ] {
            store.save(&g).await.unwrap();
        }

        assert_eq!(
            ids(&store.list(&GoalFilter::default()).await.unwrap()),
            vec![
                "goal_active_new",
                "goal_active_old",
                "goal_paused",
                "goal_achieved",
                "goal_abandoned"
            ]
        );
    }

    #[tokio::test]
    async fn a_goal_without_updated_at_sorts_last_within_its_status() {
        let temp = tempfile::tempdir().unwrap();
        let store = store(temp.path());
        store
            .save(&goal("goal_none", GoalStatus::Active, None))
            .await
            .unwrap();
        store
            .save(&goal("goal_dated", GoalStatus::Active, Some("t1")))
            .await
            .unwrap();

        assert_eq!(
            ids(&store.list(&GoalFilter::default()).await.unwrap()),
            vec!["goal_dated", "goal_none"]
        );
    }

    #[tokio::test]
    async fn list_ties_keep_last_write_order() {
        let temp = tempfile::tempdir().unwrap();
        let store = store(temp.path());
        for id in ["goal_a", "goal_b", "goal_c"] {
            store
                .save(&goal(id, GoalStatus::Active, Some("t1")))
                .await
                .unwrap();
        }
        assert_eq!(
            ids(&store.list(&GoalFilter::default()).await.unwrap()),
            vec!["goal_a", "goal_b", "goal_c"]
        );

        store
            .save(&goal("goal_a", GoalStatus::Active, Some("t1")))
            .await
            .unwrap();
        assert_eq!(
            ids(&store.list(&GoalFilter::default()).await.unwrap()),
            vec!["goal_b", "goal_c", "goal_a"]
        );
    }

    #[tokio::test]
    async fn list_filters_by_status() {
        let temp = tempfile::tempdir().unwrap();
        let store = store(temp.path());
        store
            .save(&goal("goal_1", GoalStatus::Active, Some("t")))
            .await
            .unwrap();
        store
            .save(&goal("goal_2", GoalStatus::Achieved, Some("t")))
            .await
            .unwrap();

        assert_eq!(
            ids(&store
                .list(&GoalFilter::with_status(GoalStatus::Active))
                .await
                .unwrap()),
            vec!["goal_1"]
        );
        assert!(
            store
                .list(&GoalFilter::with_status(GoalStatus::Abandoned))
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn a_deleted_index_is_rebuilt_from_the_goal_files() {
        let temp = tempfile::tempdir().unwrap();
        let store = store(temp.path());
        store
            .save(&goal("goal_1", GoalStatus::Active, Some("t")))
            .await
            .unwrap();
        tokio::fs::remove_file(store.index.index_path())
            .await
            .unwrap();
        assert_eq!(store.list(&GoalFilter::default()).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn an_invalid_id_is_rejected() {
        let temp = tempfile::tempdir().unwrap();
        let store = store(temp.path());
        assert!(store.get("a/b").await.is_err());
    }
}
