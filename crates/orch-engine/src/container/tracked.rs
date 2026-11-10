//! Operation-aware store adapters. Capture old membership before any write.
//! The store contracts and on-disk implementations remain unchanged.

use super::commit::*;
use super::operation::OperationBoundary;
use async_trait::async_trait;
use futures::stream::BoxStream;
use indexmap::IndexMap;
use orch_core::error::Result;
use orch_core::storage::*;
use orch_domain::*;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

pub(super) struct Tracked<T: ?Sized> {
    inner: Arc<T>,
    boundary: Arc<OperationBoundary>,
}
impl<T: ?Sized> Tracked<T> {
    pub(super) fn wrap(inner: Arc<T>, boundary: Arc<OperationBoundary>) -> Arc<Self> {
        Arc::new(Self { inner, boundary })
    }
}

fn change(
    store: CommitStore,
    id: &str,
    old: Result<Option<Membership>>,
    new: Option<Membership>,
) -> CommitChange {
    match old {
        Ok(old) => CommitChange {
            store,
            id: Some(id.to_owned()),
            action: if new.is_none() {
                CommitAction::Deleted
            } else if old.is_none() {
                CommitAction::Created
            } else {
                CommitAction::Updated
            },
            old,
            new,
        },
        Err(_) => CommitChange::store_wide(store),
    }
}
fn task_membership(value: &Task) -> Membership {
    Membership {
        goal_id: value.goal_id.clone(),
        agent_id: value.assignee.clone(),
        recipient_id: None,
    }
}
fn agent_membership(value: &Agent) -> Membership {
    Membership {
        agent_id: Some(value.id.clone()),
        ..Default::default()
    }
}
fn goal_membership(value: &Goal) -> Membership {
    Membership {
        goal_id: Some(value.id.clone()),
        agent_id: value.assignee.clone(),
        recipient_id: None,
    }
}
fn message_membership(value: &Message) -> Membership {
    Membership {
        agent_id: Some(value.from_agent_id.clone()),
        recipient_id: value.to_agent_id.clone(),
        goal_id: None,
    }
}
fn run_membership(value: &Run) -> Membership {
    Membership {
        agent_id: Some(value.agent_id.clone()),
        ..Default::default()
    }
}
fn team_membership(_: &Team) -> Membership {
    Membership::default()
}

#[async_trait]
impl TaskStore for Tracked<dyn TaskStore> {
    async fn list(&self, filter: &TaskFilter) -> Result<Vec<Task>> {
        self.inner.list(filter).await
    }
    async fn get(&self, id: &str) -> Result<Option<Task>> {
        self.inner.get(id).await
    }
    async fn save(&self, task: &Task) -> Result<()> {
        self.boundary
            .store_write(async {
                self.boundary.record_change(change(
                    CommitStore::Tasks,
                    task.id.as_str(),
                    self.inner
                        .get(task.id.as_str())
                        .await
                        .map(|v| v.as_ref().map(task_membership)),
                    Some(task_membership(task)),
                ));
                self.inner.save(task).await
            })
            .await
            .value
    }
    async fn delete(&self, id: &str) -> Result<()> {
        self.boundary
            .store_write(async {
                self.boundary.record_change(change(
                    CommitStore::Tasks,
                    id,
                    self.inner
                        .get(id)
                        .await
                        .map(|v| v.as_ref().map(task_membership)),
                    None,
                ));
                self.inner.delete(id).await
            })
            .await
            .value
    }
}

#[async_trait]
impl AgentStore for Tracked<dyn AgentStore> {
    async fn list(&self) -> Result<Vec<Agent>> {
        self.inner.list().await
    }
    async fn get(&self, id: &str) -> Result<Option<Agent>> {
        self.inner.get(id).await
    }
    async fn get_by_name(&self, name: &str) -> Result<Option<Agent>> {
        self.inner.get_by_name(name).await
    }
    async fn save(&self, agent: &Agent) -> Result<()> {
        self.boundary
            .store_write(async {
                self.boundary.record_change(change(
                    CommitStore::Agents,
                    agent.id.as_str(),
                    self.inner
                        .get(agent.id.as_str())
                        .await
                        .map(|v| v.as_ref().map(agent_membership)),
                    Some(agent_membership(agent)),
                ));
                self.inner.save(agent).await
            })
            .await
            .value
    }
    async fn delete(&self, id: &str) -> Result<()> {
        self.boundary
            .store_write(async {
                self.boundary.record_change(change(
                    CommitStore::Agents,
                    id,
                    self.inner
                        .get(id)
                        .await
                        .map(|v| v.as_ref().map(agent_membership)),
                    None,
                ));
                self.inner.delete(id).await
            })
            .await
            .value
    }
}

#[async_trait]
impl RunStore for Tracked<dyn RunStore> {
    async fn save(&self, run: &Run) -> Result<()> {
        self.boundary
            .store_write(async {
                self.boundary.record_change(change(
                    CommitStore::Runs,
                    run.id.as_str(),
                    self.inner
                        .get(run.id.as_str())
                        .await
                        .map(|v| v.as_ref().map(run_membership)),
                    Some(run_membership(run)),
                ));
                self.inner.save(run).await
            })
            .await
            .value
    }
    async fn get(&self, id: &str) -> Result<Option<Run>> {
        self.inner.get(id).await
    }
    async fn list_all(&self) -> Result<Vec<Run>> {
        self.inner.list_all().await
    }
    async fn list_for_task(&self, task_id: &str) -> Result<Vec<Run>> {
        self.inner.list_for_task(task_id).await
    }
    async fn list_for_agent(&self, agent_id: &str) -> Result<Vec<Run>> {
        self.inner.list_for_agent(agent_id).await
    }
    async fn append_event(&self, run_id: &str, event: &RunEvent) -> Result<()> {
        self.boundary
            .store_write(async {
                self.boundary
                    .record_change(CommitChange::store_wide(CommitStore::Runs));
                self.inner.append_event(run_id, event).await
            })
            .await
            .value
    }
    async fn read_events(&self, run_id: &str) -> Result<Vec<RunEvent>> {
        self.inner.read_events(run_id).await
    }
    async fn read_events_tail(&self, run_id: &str, count: usize) -> Result<Vec<RunEvent>> {
        self.inner.read_events_tail(run_id, count).await
    }
    fn stream_events(
        &self,
        run_id: &str,
        token: CancellationToken,
    ) -> BoxStream<'static, Result<RunEvent>> {
        self.inner.stream_events(run_id, token)
    }
    fn close_run_events(&self, run_id: &str) {
        self.inner.close_run_events(run_id)
    }
}

#[async_trait]
impl StateStore for Tracked<dyn StateStore> {
    async fn read(&self) -> Result<OrchestratorState> {
        self.inner.read().await
    }
    async fn write(&self, state: &OrchestratorState) -> Result<()> {
        self.boundary
            .store_write(async {
                self.boundary
                    .record_change(CommitChange::store_wide(CommitStore::State));
                self.inner.write(state).await
            })
            .await
            .value
    }
}

#[async_trait]
impl ConfigStore for Tracked<dyn ConfigStore> {
    async fn read(&self) -> Result<OrchestratorConfig> {
        self.inner.read().await
    }
    async fn write(&self, config: &OrchestratorConfig) -> Result<()> {
        self.boundary
            .store_write(async {
                self.boundary
                    .record_change(CommitChange::store_wide(CommitStore::Config));
                self.inner.write(config).await
            })
            .await
            .value
    }
    async fn get(&self, key_path: &str) -> Result<Option<serde_json::Value>> {
        self.inner.get(key_path).await
    }
    async fn set(&self, key_path: &str, value: serde_json::Value) -> Result<()> {
        self.boundary
            .store_write(async {
                self.boundary
                    .record_change(CommitChange::store_wide(CommitStore::Config));
                self.inner.set(key_path, value).await
            })
            .await
            .value
    }
}

#[async_trait]
impl ContextStore for Tracked<dyn ContextStore> {
    async fn get(&self, key: &str) -> Result<Option<ContextEntry>> {
        self.inner.get(key).await
    }
    async fn set(&self, key: &str, value: &str, ttl_ms: Option<u64>) -> Result<()> {
        self.boundary
            .store_write(async {
                self.boundary.record_change(change(
                    CommitStore::Context,
                    key,
                    self.inner
                        .get(key)
                        .await
                        .map(|v| v.map(|_| Membership::default())),
                    Some(Membership::default()),
                ));
                self.inner.set(key, value, ttl_ms).await
            })
            .await
            .value
    }
    async fn delete(&self, key: &str) -> Result<()> {
        self.boundary
            .store_write(async {
                self.boundary.record_change(change(
                    CommitStore::Context,
                    key,
                    self.inner
                        .get(key)
                        .await
                        .map(|v| v.map(|_| Membership::default())),
                    None,
                ));
                self.inner.delete(key).await
            })
            .await
            .value
    }
    async fn list(&self) -> Result<Vec<ContextEntry>> {
        self.inner.list().await
    }
    async fn get_all(&self) -> Result<IndexMap<String, String>> {
        self.inner.get_all().await
    }
}

#[async_trait]
impl MessageStore for Tracked<dyn MessageStore> {
    async fn save(&self, message: &Message) -> Result<()> {
        self.boundary
            .store_write(async {
                self.boundary.record_change(change(
                    CommitStore::Messages,
                    message.id.as_str(),
                    self.inner
                        .get(message.id.as_str())
                        .await
                        .map(|v| v.as_ref().map(message_membership)),
                    Some(message_membership(message)),
                ));
                self.inner.save(message).await
            })
            .await
            .value
    }
    async fn get(&self, id: &str) -> Result<Option<Message>> {
        self.inner.get(id).await
    }
    async fn list(&self) -> Result<Vec<Message>> {
        self.inner.list().await
    }
    async fn list_pending(&self, agent_id: &str) -> Result<Vec<Message>> {
        self.inner.list_pending(agent_id).await
    }
    async fn mark_delivered(&self, id: &str) -> Result<()> {
        self.boundary
            .store_write(async {
                let old = self
                    .inner
                    .get(id)
                    .await
                    .map(|v| v.as_ref().map(message_membership));
                let new = old.as_ref().ok().cloned().flatten();
                let mut metadata = change(CommitStore::Messages, id, old, new);
                metadata.action = CommitAction::Updated;
                self.boundary.record_change(metadata);
                self.inner.mark_delivered(id).await
            })
            .await
            .value
    }
    async fn delete(&self, id: &str) -> Result<()> {
        self.boundary
            .store_write(async {
                self.boundary.record_change(change(
                    CommitStore::Messages,
                    id,
                    self.inner
                        .get(id)
                        .await
                        .map(|v| v.as_ref().map(message_membership)),
                    None,
                ));
                self.inner.delete(id).await
            })
            .await
            .value
    }
    async fn purge_expired(&self) -> Result<usize> {
        self.boundary
            .store_write(async {
                let added = self.boundary.note_store(CommitStore::Messages);
                let result = self.inner.purge_expired().await;
                if added && matches!(result, Ok(0)) {
                    self.boundary.discard_noop_store(CommitStore::Messages);
                }
                result
            })
            .await
            .value
    }
}

#[async_trait]
impl GoalStore for Tracked<dyn GoalStore> {
    async fn list(&self, filter: &GoalFilter) -> Result<Vec<Goal>> {
        self.inner.list(filter).await
    }
    async fn get(&self, id: &str) -> Result<Option<Goal>> {
        self.inner.get(id).await
    }
    async fn save(&self, goal: &Goal) -> Result<()> {
        self.boundary
            .store_write(async {
                self.boundary.record_change(change(
                    CommitStore::Goals,
                    goal.id.as_str(),
                    self.inner
                        .get(goal.id.as_str())
                        .await
                        .map(|v| v.as_ref().map(goal_membership)),
                    Some(goal_membership(goal)),
                ));
                self.inner.save(goal).await
            })
            .await
            .value
    }
    async fn delete(&self, id: &str) -> Result<()> {
        self.boundary
            .store_write(async {
                self.boundary.record_change(change(
                    CommitStore::Goals,
                    id,
                    self.inner
                        .get(id)
                        .await
                        .map(|v| v.as_ref().map(goal_membership)),
                    None,
                ));
                self.inner.delete(id).await
            })
            .await
            .value
    }
}

#[async_trait]
impl TeamStore for Tracked<dyn TeamStore> {
    async fn save(&self, team: &Team) -> Result<()> {
        self.boundary
            .store_write(async {
                self.boundary.record_change(change(
                    CommitStore::Teams,
                    team.id.as_str(),
                    self.inner
                        .get(team.id.as_str())
                        .await
                        .map(|v| v.as_ref().map(team_membership)),
                    Some(team_membership(team)),
                ));
                self.inner.save(team).await
            })
            .await
            .value
    }
    async fn get(&self, id: &str) -> Result<Option<Team>> {
        self.inner.get(id).await
    }
    async fn get_by_name(&self, name: &str) -> Result<Option<Team>> {
        self.inner.get_by_name(name).await
    }
    async fn list(&self) -> Result<Vec<Team>> {
        self.inner.list().await
    }
    async fn delete(&self, id: &str) -> Result<()> {
        self.boundary
            .store_write(async {
                self.boundary.record_change(change(
                    CommitStore::Teams,
                    id,
                    self.inner
                        .get(id)
                        .await
                        .map(|v| v.as_ref().map(team_membership)),
                    None,
                ));
                self.inner.delete(id).await
            })
            .await
            .value
    }
}

#[async_trait]
impl GlobalConfigStore for Tracked<dyn GlobalConfigStore> {
    async fn read(&self) -> Result<GlobalConfig> {
        self.inner.read().await
    }
    async fn write(&self, config: &GlobalConfig) -> Result<()> {
        self.boundary
            .store_write(async {
                self.boundary
                    .record_change(CommitChange::store_wide(CommitStore::Config));
                self.inner.write(config).await
            })
            .await
            .value
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    struct FailedPurge;

    #[async_trait]
    impl MessageStore for FailedPurge {
        async fn save(&self, _: &Message) -> Result<()> {
            unreachable!()
        }
        async fn get(&self, _: &str) -> Result<Option<Message>> {
            unreachable!()
        }
        async fn list(&self) -> Result<Vec<Message>> {
            unreachable!()
        }
        async fn list_pending(&self, _: &str) -> Result<Vec<Message>> {
            unreachable!()
        }
        async fn mark_delivered(&self, _: &str) -> Result<()> {
            unreachable!()
        }
        async fn delete(&self, _: &str) -> Result<()> {
            unreachable!()
        }
        async fn purge_expired(&self) -> Result<usize> {
            Err(orch_core::error::Error::other(
                "injected partial purge failure",
            ))
        }
    }

    #[tokio::test]
    async fn failed_purge_retains_the_known_messages_declaration() {
        let boundary = Arc::new(OperationBoundary::new());
        let records = Arc::new(Mutex::new(Vec::new()));
        let sink = records.clone();
        boundary.set_publisher(Some(Arc::new(move |publication| {
            if let Publication::Commit(record) = publication {
                sink.lock().unwrap().push(record.clone());
            }
        })));
        let store = Tracked::<dyn MessageStore>::wrap(Arc::new(FailedPurge), boundary.clone());
        assert!(store.purge_expired().await.is_err());
        let records = records.lock().unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(
            records[0].changes,
            vec![CommitChange::store_wide(CommitStore::Messages)]
        );
        assert!(!records[0].store_set_uncertain);
        assert_eq!(boundary.current_commit_seq(), 1);
    }
}
