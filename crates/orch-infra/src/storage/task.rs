//! File-based task store — ported from
//! `src/infrastructure/storage/task-store.ts`.

use async_trait::async_trait;
use orch_core::error::Result;
use orch_core::storage::{TaskFilter, TaskStore};
use orch_domain::{Task, TaskStatus};

use super::fs_utils::{ensure_dir, read_yaml, write_yaml};
use super::index::{IndexManager, ItemFormat, replace_at_end};
use super::paths::Paths;

#[derive(Debug)]
pub struct FileTaskStore {
    paths: Paths,
    index: IndexManager<Task>,
}

impl FileTaskStore {
    pub fn new(paths: Paths) -> Self {
        let index = IndexManager::new(paths.tasks_dir(), ItemFormat::Yaml);
        FileTaskStore { paths, index }
    }
}

/// Dispatch-facing display order (not the domain's state-machine order):
/// `in_progress, retrying, review, todo, done, failed, cancelled`.
fn status_priority(status: TaskStatus) -> u8 {
    match status {
        TaskStatus::InProgress => 0,
        TaskStatus::Retrying => 1,
        TaskStatus::Review => 2,
        TaskStatus::Todo => 3,
        TaskStatus::Done => 4,
        TaskStatus::Failed => 5,
        TaskStatus::Cancelled => 6,
    }
}

#[async_trait]
impl TaskStore for FileTaskStore {
    /// Sorted by status priority, then by `updated_at` **descending**
    /// (newest first).
    ///
    /// `sort_by`, never `sort_unstable_by`: ties must keep index order, which
    /// is last-write order (PARITY.md §14.4). The workspace `clippy.toml`
    /// enforces that.
    async fn list(&self, filter: &TaskFilter) -> Result<Vec<Task>> {
        let mut tasks: Vec<Task> = self
            .index
            .read_index()
            .await?
            .into_iter()
            .filter(|t| filter.matches(t))
            .collect();

        tasks.sort_by(|a, b| {
            status_priority(a.status)
                .cmp(&status_priority(b.status))
                .then_with(|| b.updated_at.cmp(&a.updated_at))
        });
        Ok(tasks)
    }

    /// Reads the individual file, not the index — so a task whose index entry
    /// is stale still reads correctly.
    async fn get(&self, id: &str) -> Result<Option<Task>> {
        read_yaml(&self.paths.task_path(id)?).await
    }

    async fn save(&self, task: &Task) -> Result<()> {
        ensure_dir(&self.paths.tasks_dir()).await?;
        write_yaml(&self.paths.task_path(&task.id)?, task).await?;
        let task = task.clone();
        self.index
            .update_index(move |items| replace_at_end(items, task, |t| &t.id))
            .await
    }

    async fn delete(&self, id: &str) -> Result<()> {
        let path = self.paths.task_path(id)?;
        match tokio::fs::remove_file(&path).await {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => {
                return Err(orch_core::error::Error::io(
                    format!("delete {}", path.display()),
                    &path,
                    err,
                ));
            }
        }
        let id = id.to_owned();
        self.index
            .update_index(move |items| items.into_iter().filter(|t| t.id != id).collect())
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::fs_utils::clear_ensured_dirs;
    use orch_core::test_support::make_task;

    fn store(root: &std::path::Path) -> FileTaskStore {
        clear_ensured_dirs();
        FileTaskStore::new(Paths::new(root))
    }

    fn task(id: &str, status: TaskStatus, updated_at: &str) -> Task {
        Task {
            id: id.into(),
            status,
            updated_at: updated_at.into(),
            ..make_task()
        }
    }

    fn ids(tasks: &[Task]) -> Vec<&str> {
        tasks.iter().map(|t| t.id.as_str()).collect()
    }

    #[tokio::test]
    async fn saves_reads_and_deletes_a_task() {
        let temp = tempfile::tempdir().unwrap();
        let store = store(temp.path());

        assert!(store.get("tsk_test1").await.unwrap().is_none());
        store.save(&make_task()).await.unwrap();
        assert_eq!(
            store.get("tsk_test1").await.unwrap().unwrap().title,
            "Test task"
        );
        assert_eq!(store.list(&TaskFilter::default()).await.unwrap().len(), 1);

        store.delete("tsk_test1").await.unwrap();
        assert!(store.get("tsk_test1").await.unwrap().is_none());
        assert!(store.list(&TaskFilter::default()).await.unwrap().is_empty());
        // Deleting a missing task is a no-op, not an error.
        store.delete("tsk_test1").await.unwrap();
    }

    #[tokio::test]
    async fn writes_one_yaml_file_per_task_plus_the_index() {
        let temp = tempfile::tempdir().unwrap();
        let store = store(temp.path());
        store.save(&make_task()).await.unwrap();

        let mut names = crate::storage::fs_utils::list_files(&store.paths.tasks_dir(), None)
            .await
            .unwrap();
        names.sort();
        assert_eq!(
            names,
            vec!["_index.json".to_owned(), "tsk_test1.yml".to_owned()]
        );
    }

    #[tokio::test]
    async fn list_sorts_by_status_priority_then_newest_first() {
        let temp = tempfile::tempdir().unwrap();
        let store = store(temp.path());
        for t in [
            task("tsk_done", TaskStatus::Done, "2025-01-01T00:00:00Z"),
            task("tsk_todo_old", TaskStatus::Todo, "2025-01-01T00:00:00Z"),
            task("tsk_todo_new", TaskStatus::Todo, "2025-01-02T00:00:00Z"),
            task(
                "tsk_running",
                TaskStatus::InProgress,
                "2025-01-01T00:00:00Z",
            ),
            task("tsk_review", TaskStatus::Review, "2025-01-01T00:00:00Z"),
        ] {
            store.save(&t).await.unwrap();
        }

        assert_eq!(
            ids(&store.list(&TaskFilter::default()).await.unwrap()),
            vec![
                "tsk_running",
                "tsk_review",
                "tsk_todo_new",
                "tsk_todo_old",
                "tsk_done"
            ]
        );
    }

    #[tokio::test]
    async fn list_ties_keep_index_order_which_is_last_write_order() {
        let temp = tempfile::tempdir().unwrap();
        let store = store(temp.path());
        // Identical status and updated_at: the sort must not reorder them.
        for id in ["tsk_a", "tsk_b", "tsk_c"] {
            store
                .save(&task(id, TaskStatus::Todo, "2025-01-01T00:00:00Z"))
                .await
                .unwrap();
        }
        assert_eq!(
            ids(&store.list(&TaskFilter::default()).await.unwrap()),
            vec!["tsk_a", "tsk_b", "tsk_c"]
        );

        // Re-saving `tsk_a` moves it to the end of the index, and the stable
        // sort preserves that.
        store
            .save(&task("tsk_a", TaskStatus::Todo, "2025-01-01T00:00:00Z"))
            .await
            .unwrap();
        assert_eq!(
            ids(&store.list(&TaskFilter::default()).await.unwrap()),
            vec!["tsk_b", "tsk_c", "tsk_a"]
        );
    }

    #[tokio::test]
    async fn list_filters_by_status_and_goal() {
        let temp = tempfile::tempdir().unwrap();
        let store = store(temp.path());
        store
            .save(&task("tsk_1", TaskStatus::Todo, "t"))
            .await
            .unwrap();
        store
            .save(&task("tsk_2", TaskStatus::Done, "t"))
            .await
            .unwrap();
        store
            .save(&Task {
                goal_id: Some("goal_1".into()),
                ..task("tsk_3", TaskStatus::Todo, "t")
            })
            .await
            .unwrap();

        assert_eq!(
            ids(&store
                .list(&TaskFilter::with_status(TaskStatus::Todo))
                .await
                .unwrap()),
            vec!["tsk_1", "tsk_3"]
        );
        assert_eq!(
            ids(&store.list(&TaskFilter::with_goal("goal_1")).await.unwrap()),
            vec!["tsk_3"]
        );
    }

    #[tokio::test]
    async fn a_deleted_index_is_rebuilt_from_the_task_files() {
        let temp = tempfile::tempdir().unwrap();
        let store = store(temp.path());
        store.save(&make_task()).await.unwrap();
        tokio::fs::remove_file(store.index.index_path())
            .await
            .unwrap();

        assert_eq!(store.list(&TaskFilter::default()).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn timestamps_survive_the_yaml_round_trip_as_strings() {
        let temp = tempfile::tempdir().unwrap();
        let store = store(temp.path());
        let t = Task {
            created_at: "2026-10-13T16:37:58.959Z".into(),
            updated_at: "2026-10-13T16:37:58.959Z".into(),
            ..make_task()
        };
        store.save(&t).await.unwrap();

        let raw = tokio::fs::read_to_string(store.paths.task_path("tsk_test1").unwrap())
            .await
            .unwrap();
        assert!(
            raw.contains("created_at: '2026-10-13T16:37:58.959Z'"),
            "{raw}"
        );
        assert_eq!(store.get("tsk_test1").await.unwrap().unwrap(), t);
    }

    #[tokio::test]
    async fn an_invalid_id_is_rejected_rather_than_escaping_the_tasks_dir() {
        let temp = tempfile::tempdir().unwrap();
        let store = store(temp.path());
        assert!(store.get("../../etc/passwd").await.is_err());
        assert!(store.delete("a/b").await.is_err());
    }
}
