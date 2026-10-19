//! Tick-scoped caching wrappers.
//!
//! Each wrapper memoizes `list()` (and `get_by_name` for agents) for the
//! duration of one tick, and clears the whole cache on any `save` or `delete`.
//! `get(id)` is **never** cached: the orchestrator re-reads a single entity
//! precisely when it wants the current value.
//!
//! The invalidation is deliberately coarse — one write drops every cached
//! list, including lists under other filters. A finer scheme would have to
//! reason about which filters a given entity belongs to, and getting that
//! wrong serves stale data to the dispatcher.
//!
//! These wrappers are **not** a general-purpose cache. They are correct only
//! because a tick holds the state lock while it runs, so nothing else mutates
//! the store underneath them. ARCHITECTURE's standing rule applies: a package
//! that needs different caching files an amendment rather than wrapping a
//! wrapper, which is how you end up with two caches of different lifetimes
//! over one store.

use std::sync::Arc;

use async_trait::async_trait;
use orch_core::error::Result;
use orch_core::storage::{AgentStore, GoalFilter, GoalStore, TaskFilter, TaskStore};
use orch_domain::{Agent, Goal, Task};
use tokio::sync::Mutex;

// A sync `Invalidate` trait used to live here. It called `blocking_lock()`,
// which **panics** when called from inside a tokio runtime — which is the only
// place these stores are ever used, so the one safe way to call it was not to.
// It had a single impl and no callers; every real invalidation goes through
// `invalidate_async`. Removed rather than made non-panicking, because a
// `try_lock`-and-defer version would silently skip an invalidation under
// contention, and a cache that *might* still be stale after you cleared it is
// worse than no sync entry point at all.
//
// If something genuinely needs to invalidate from a sync context later, it
// wants `Handle::block_on` at the call site, where the caller can see what it
// is doing.

// ── Tasks ──────────────────────────────────────────────────────────────────

pub struct CachedTaskStore {
    inner: Arc<dyn TaskStore>,
    /// Keyed `"<status>:<goalId>"`, with an empty
    /// segment for an unset dimension — so two filters that differ only in an
    /// unset field share one entry.
    cache: Mutex<std::collections::HashMap<String, Vec<Task>>>,
}

impl std::fmt::Debug for CachedTaskStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CachedTaskStore").finish_non_exhaustive()
    }
}

impl CachedTaskStore {
    pub fn new(inner: Arc<dyn TaskStore>) -> Self {
        CachedTaskStore {
            inner,
            cache: Mutex::new(std::collections::HashMap::new()),
        }
    }

    fn key(filter: &TaskFilter) -> String {
        // `TaskFilter::default()` is the only way to express "no filter at
        // all"; it maps to the same key as a filter with nothing set.
        format!(
            "{}:{}",
            filter.status.map(|s| s.as_str()).unwrap_or(""),
            filter.goal_id.as_deref().unwrap_or("")
        )
    }
}

#[async_trait]
impl TaskStore for CachedTaskStore {
    async fn list(&self, filter: &TaskFilter) -> Result<Vec<Task>> {
        let key = CachedTaskStore::key(filter);
        {
            let cache = self.cache.lock().await;
            if let Some(hit) = cache.get(&key) {
                return Ok(hit.clone());
            }
        }
        let result = self.inner.list(filter).await?;
        self.cache.lock().await.insert(key, result.clone());
        Ok(result)
    }

    /// Never cached.
    async fn get(&self, id: &str) -> Result<Option<Task>> {
        self.inner.get(id).await
    }

    async fn save(&self, task: &Task) -> Result<()> {
        self.inner.save(task).await?;
        self.cache.lock().await.clear();
        Ok(())
    }

    async fn delete(&self, id: &str) -> Result<()> {
        self.inner.delete(id).await?;
        self.cache.lock().await.clear();
        Ok(())
    }
}

impl CachedTaskStore {
    /// Drop every memoized list. Called at the top of a tick.
    pub async fn invalidate_async(&self) {
        self.cache.lock().await.clear();
    }
}

// ── Agents ─────────────────────────────────────────────────────────────────

pub struct CachedAgentStore {
    inner: Arc<dyn AgentStore>,
    state: Mutex<AgentCache>,
}

#[derive(Default)]
struct AgentCache {
    list: Option<Vec<Agent>>,
    /// A negative lookup is cached too: a cached `None` counts
    /// as a hit, so a miss is not re-read either.
    by_name: std::collections::HashMap<String, Option<Agent>>,
}

impl std::fmt::Debug for CachedAgentStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CachedAgentStore").finish_non_exhaustive()
    }
}

impl CachedAgentStore {
    pub fn new(inner: Arc<dyn AgentStore>) -> Self {
        CachedAgentStore {
            inner,
            state: Mutex::new(AgentCache::default()),
        }
    }

    pub async fn invalidate_async(&self) {
        let mut state = self.state.lock().await;
        state.list = None;
        state.by_name.clear();
    }
}

#[async_trait]
impl AgentStore for CachedAgentStore {
    async fn list(&self) -> Result<Vec<Agent>> {
        {
            let state = self.state.lock().await;
            if let Some(hit) = &state.list {
                return Ok(hit.clone());
            }
        }
        let result = self.inner.list().await?;
        self.state.lock().await.list = Some(result.clone());
        Ok(result)
    }

    /// Never cached.
    async fn get(&self, id: &str) -> Result<Option<Agent>> {
        self.inner.get(id).await
    }

    async fn get_by_name(&self, name: &str) -> Result<Option<Agent>> {
        {
            let state = self.state.lock().await;
            if let Some(hit) = state.by_name.get(name) {
                return Ok(hit.clone());
            }
        }
        let result = self.inner.get_by_name(name).await?;
        self.state
            .lock()
            .await
            .by_name
            .insert(name.to_owned(), result.clone());
        Ok(result)
    }

    async fn save(&self, agent: &Agent) -> Result<()> {
        self.inner.save(agent).await?;
        self.invalidate_async().await;
        Ok(())
    }

    async fn delete(&self, id: &str) -> Result<()> {
        self.inner.delete(id).await?;
        self.invalidate_async().await;
        Ok(())
    }
}

// ── Goals ──────────────────────────────────────────────────────────────────

pub struct CachedGoalStore {
    inner: Arc<dyn GoalStore>,
    cache: Mutex<std::collections::HashMap<String, Vec<Goal>>>,
}

impl std::fmt::Debug for CachedGoalStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CachedGoalStore").finish_non_exhaustive()
    }
}

impl CachedGoalStore {
    pub fn new(inner: Arc<dyn GoalStore>) -> Self {
        CachedGoalStore {
            inner,
            cache: Mutex::new(std::collections::HashMap::new()),
        }
    }

    pub async fn invalidate_async(&self) {
        self.cache.lock().await.clear();
    }

    fn key(filter: &GoalFilter) -> String {
        filter
            .status
            .map(|s| s.as_str().to_owned())
            .unwrap_or_else(|| "__all__".to_owned())
    }
}

#[async_trait]
impl GoalStore for CachedGoalStore {
    async fn list(&self, filter: &GoalFilter) -> Result<Vec<Goal>> {
        let key = CachedGoalStore::key(filter);
        {
            let cache = self.cache.lock().await;
            if let Some(hit) = cache.get(&key) {
                return Ok(hit.clone());
            }
        }
        let result = self.inner.list(filter).await?;
        self.cache.lock().await.insert(key, result.clone());
        Ok(result)
    }

    /// Never cached.
    async fn get(&self, id: &str) -> Result<Option<Goal>> {
        self.inner.get(id).await
    }

    async fn save(&self, goal: &Goal) -> Result<()> {
        self.inner.save(goal).await?;
        self.cache.lock().await.clear();
        Ok(())
    }

    async fn delete(&self, id: &str) -> Result<()> {
        self.inner.delete(id).await?;
        self.cache.lock().await.clear();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use orch_core::test_support::{
        AgentStoreCall, FakeAgentStore, FakeGoalStore, FakeTaskStore, GoalStoreCall, TaskStoreCall,
        make_agent, make_task,
    };
    use orch_domain::{GoalStatus, TaskStatus};

    fn task(id: &str, status: TaskStatus) -> Task {
        Task {
            id: id.into(),
            status,
            ..make_task()
        }
    }

    fn goal(id: &str, status: GoalStatus) -> Goal {
        Goal {
            id: id.into(),
            title: "g".into(),
            description: String::new(),
            status,
            assignee: None,
            created_at: "t".into(),
            updated_at: None,
        }
    }

    // ── Tasks ──────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn a_repeated_list_hits_the_inner_store_once() {
        let inner = Arc::new(FakeTaskStore::with_tasks([make_task()]));
        let cached = CachedTaskStore::new(Arc::clone(&inner) as Arc<dyn TaskStore>);

        for _ in 0..5 {
            assert_eq!(cached.list(&TaskFilter::default()).await.unwrap().len(), 1);
        }
        assert_eq!(
            inner
                .calls
                .count_matching(|c| matches!(c, TaskStoreCall::List(_))),
            1,
            "the cache did not hold"
        );
    }

    #[tokio::test]
    async fn each_filter_is_cached_separately() {
        let inner = Arc::new(FakeTaskStore::with_tasks([
            task("tsk_1", TaskStatus::Todo),
            task("tsk_2", TaskStatus::Done),
        ]));
        let cached = CachedTaskStore::new(Arc::clone(&inner) as Arc<dyn TaskStore>);

        assert_eq!(cached.list(&TaskFilter::default()).await.unwrap().len(), 2);
        assert_eq!(
            cached
                .list(&TaskFilter::with_status(TaskStatus::Todo))
                .await
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            cached
                .list(&TaskFilter::with_status(TaskStatus::Done))
                .await
                .unwrap()
                .len(),
            1
        );
        // Three distinct keys, three reads — then no more.
        assert_eq!(
            inner
                .calls
                .count_matching(|c| matches!(c, TaskStoreCall::List(_))),
            3
        );

        cached
            .list(&TaskFilter::with_status(TaskStatus::Todo))
            .await
            .unwrap();
        assert_eq!(
            inner
                .calls
                .count_matching(|c| matches!(c, TaskStoreCall::List(_))),
            3
        );
    }

    #[tokio::test]
    async fn the_cache_key_has_the_expected_shape() {
        assert_eq!(CachedTaskStore::key(&TaskFilter::default()), ":");
        assert_eq!(
            CachedTaskStore::key(&TaskFilter::with_status(TaskStatus::Todo)),
            "todo:"
        );
        assert_eq!(
            CachedTaskStore::key(&TaskFilter::with_goal("goal_1")),
            ":goal_1"
        );
        assert_eq!(
            CachedTaskStore::key(&TaskFilter {
                status: Some(TaskStatus::InProgress),
                goal_id: Some("goal_1".into())
            }),
            "in_progress:goal_1"
        );
    }

    #[tokio::test]
    async fn get_is_never_cached() {
        let inner = Arc::new(FakeTaskStore::with_tasks([make_task()]));
        let cached = CachedTaskStore::new(Arc::clone(&inner) as Arc<dyn TaskStore>);

        for _ in 0..3 {
            cached.get("tsk_test1").await.unwrap();
        }
        assert_eq!(
            inner
                .calls
                .count_matching(|c| matches!(c, TaskStoreCall::Get(_))),
            3
        );
    }

    #[tokio::test]
    async fn a_save_drops_every_cached_list_including_other_filters() {
        let inner = Arc::new(FakeTaskStore::with_tasks([task("tsk_1", TaskStatus::Todo)]));
        let cached = CachedTaskStore::new(Arc::clone(&inner) as Arc<dyn TaskStore>);

        cached.list(&TaskFilter::default()).await.unwrap();
        cached
            .list(&TaskFilter::with_status(TaskStatus::Done))
            .await
            .unwrap();
        let before = inner
            .calls
            .count_matching(|c| matches!(c, TaskStoreCall::List(_)));

        cached.save(&task("tsk_2", TaskStatus::Done)).await.unwrap();

        // Both filters re-read, and the new task is visible through both.
        assert_eq!(cached.list(&TaskFilter::default()).await.unwrap().len(), 2);
        assert_eq!(
            cached
                .list(&TaskFilter::with_status(TaskStatus::Done))
                .await
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            inner
                .calls
                .count_matching(|c| matches!(c, TaskStoreCall::List(_))),
            before + 2
        );
    }

    #[tokio::test]
    async fn a_delete_drops_the_cache_too() {
        let inner = Arc::new(FakeTaskStore::with_tasks([make_task()]));
        let cached = CachedTaskStore::new(Arc::clone(&inner) as Arc<dyn TaskStore>);
        cached.list(&TaskFilter::default()).await.unwrap();

        cached.delete("tsk_test1").await.unwrap();
        assert!(
            cached
                .list(&TaskFilter::default())
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn invalidate_forces_the_next_list_to_re_read() {
        let inner = Arc::new(FakeTaskStore::with_tasks([make_task()]));
        let cached = CachedTaskStore::new(Arc::clone(&inner) as Arc<dyn TaskStore>);
        cached.list(&TaskFilter::default()).await.unwrap();

        // A write that bypasses the wrapper is exactly what invalidate exists
        // for — the next tick must not serve the stale list.
        inner.seed(task("tsk_sneaky", TaskStatus::Todo));
        assert_eq!(cached.list(&TaskFilter::default()).await.unwrap().len(), 1);

        cached.invalidate_async().await;
        assert_eq!(cached.list(&TaskFilter::default()).await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn a_cached_list_is_cloned_so_a_caller_cannot_poison_it() {
        let inner = Arc::new(FakeTaskStore::with_tasks([make_task()]));
        let cached = CachedTaskStore::new(inner as Arc<dyn TaskStore>);

        let mut first = cached.list(&TaskFilter::default()).await.unwrap();
        first[0].status = TaskStatus::Done;
        first.clear();

        let second = cached.list(&TaskFilter::default()).await.unwrap();
        assert_eq!(second.len(), 1);
        assert_eq!(second[0].status, TaskStatus::Todo);
    }

    // ── Agents ─────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn agent_list_and_name_lookups_are_cached_independently() {
        let inner = Arc::new(FakeAgentStore::with_agents([make_agent()]));
        let cached = CachedAgentStore::new(Arc::clone(&inner) as Arc<dyn AgentStore>);

        for _ in 0..3 {
            cached.list().await.unwrap();
            cached.get_by_name("TestAgent").await.unwrap();
        }
        assert_eq!(
            inner
                .calls
                .count_matching(|c| matches!(c, AgentStoreCall::List)),
            1
        );
        assert_eq!(
            inner
                .calls
                .count_matching(|c| matches!(c, AgentStoreCall::GetByName(_))),
            1
        );
    }

    #[tokio::test]
    async fn a_negative_name_lookup_is_cached_as_well() {
        let inner = Arc::new(FakeAgentStore::new());
        let cached = CachedAgentStore::new(Arc::clone(&inner) as Arc<dyn AgentStore>);

        for _ in 0..3 {
            assert!(cached.get_by_name("nope").await.unwrap().is_none());
        }
        // A cached `None` counts as a hit.
        assert_eq!(
            inner
                .calls
                .count_matching(|c| matches!(c, AgentStoreCall::GetByName(_))),
            1
        );
    }

    #[tokio::test]
    async fn an_agent_save_clears_both_caches() {
        let inner = Arc::new(FakeAgentStore::new());
        let cached = CachedAgentStore::new(Arc::clone(&inner) as Arc<dyn AgentStore>);
        assert!(cached.get_by_name("TestAgent").await.unwrap().is_none());
        assert!(cached.list().await.unwrap().is_empty());

        cached.save(&make_agent()).await.unwrap();

        assert!(cached.get_by_name("TestAgent").await.unwrap().is_some());
        assert_eq!(cached.list().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn an_agent_delete_clears_both_caches() {
        let inner = Arc::new(FakeAgentStore::with_agents([make_agent()]));
        let cached = CachedAgentStore::new(inner as Arc<dyn AgentStore>);
        assert!(cached.get_by_name("TestAgent").await.unwrap().is_some());

        cached.delete("agt_test1").await.unwrap();
        assert!(cached.get_by_name("TestAgent").await.unwrap().is_none());
        assert!(cached.list().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn agent_get_is_never_cached() {
        let inner = Arc::new(FakeAgentStore::with_agents([make_agent()]));
        let cached = CachedAgentStore::new(Arc::clone(&inner) as Arc<dyn AgentStore>);
        for _ in 0..3 {
            cached.get("agt_test1").await.unwrap();
        }
        assert_eq!(
            inner
                .calls
                .count_matching(|c| matches!(c, AgentStoreCall::Get(_))),
            3
        );
    }

    #[tokio::test]
    async fn agent_list_order_survives_the_cache() {
        // Last-write order decides tie-broken agent selection, so the cache
        // must not reorder it.
        let inner = Arc::new(FakeAgentStore::new());
        let cached = CachedAgentStore::new(Arc::clone(&inner) as Arc<dyn AgentStore>);
        for id in ["agt_c", "agt_a", "agt_b"] {
            cached
                .save(&Agent {
                    id: id.into(),
                    name: id.into(),
                    ..make_agent()
                })
                .await
                .unwrap();
        }
        let ids: Vec<String> = cached
            .list()
            .await
            .unwrap()
            .into_iter()
            .map(|a| a.id)
            .collect();
        assert_eq!(ids, vec!["agt_c", "agt_a", "agt_b"]);
    }

    // ── Goals ──────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn goal_lists_are_cached_per_status() {
        let inner = Arc::new(FakeGoalStore::with_goals([
            goal("goal_1", GoalStatus::Active),
            goal("goal_2", GoalStatus::Achieved),
        ]));
        let cached = CachedGoalStore::new(Arc::clone(&inner) as Arc<dyn GoalStore>);

        for _ in 0..3 {
            assert_eq!(cached.list(&GoalFilter::default()).await.unwrap().len(), 2);
            assert_eq!(
                cached
                    .list(&GoalFilter::with_status(GoalStatus::Active))
                    .await
                    .unwrap()
                    .len(),
                1
            );
        }
        assert_eq!(
            inner
                .calls
                .count_matching(|c| matches!(c, GoalStoreCall::List(_))),
            2
        );
    }

    #[tokio::test]
    async fn the_goal_cache_key_has_the_expected_shape() {
        assert_eq!(CachedGoalStore::key(&GoalFilter::default()), "__all__");
        assert_eq!(
            CachedGoalStore::key(&GoalFilter::with_status(GoalStatus::Active)),
            "active"
        );
    }

    #[tokio::test]
    async fn a_goal_write_clears_the_cache() {
        let inner = Arc::new(FakeGoalStore::new());
        let cached = CachedGoalStore::new(inner as Arc<dyn GoalStore>);
        assert!(
            cached
                .list(&GoalFilter::default())
                .await
                .unwrap()
                .is_empty()
        );

        cached
            .save(&goal("goal_1", GoalStatus::Active))
            .await
            .unwrap();
        assert_eq!(cached.list(&GoalFilter::default()).await.unwrap().len(), 1);

        cached.delete("goal_1").await.unwrap();
        assert!(
            cached
                .list(&GoalFilter::default())
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn goal_get_is_never_cached() {
        let inner = Arc::new(FakeGoalStore::with_goals([goal(
            "goal_1",
            GoalStatus::Active,
        )]));
        let cached = CachedGoalStore::new(Arc::clone(&inner) as Arc<dyn GoalStore>);
        for _ in 0..3 {
            cached.get("goal_1").await.unwrap();
        }
        assert_eq!(
            inner
                .calls
                .count_matching(|c| matches!(c, GoalStoreCall::Get(_))),
            3
        );
    }

    // ── Shared ─────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn every_wrapper_is_usable_as_the_trait_it_wraps() {
        let tasks: Arc<dyn TaskStore> =
            Arc::new(CachedTaskStore::new(Arc::new(FakeTaskStore::new())));
        let agents: Arc<dyn AgentStore> =
            Arc::new(CachedAgentStore::new(Arc::new(FakeAgentStore::new())));
        let goals: Arc<dyn GoalStore> =
            Arc::new(CachedGoalStore::new(Arc::new(FakeGoalStore::new())));

        assert!(tasks.list(&TaskFilter::default()).await.unwrap().is_empty());
        assert!(agents.list().await.unwrap().is_empty());
        assert!(goals.list(&GoalFilter::default()).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_failing_inner_store_is_not_cached_as_success() {
        let inner = Arc::new(FakeTaskStore::new());
        let cached = CachedTaskStore::new(Arc::clone(&inner) as Arc<dyn TaskStore>);

        inner.faults.fail_once("disk full");
        assert!(cached.list(&TaskFilter::default()).await.is_err());
        // The error was not memoized, so the retry reaches the store.
        assert!(cached.list(&TaskFilter::default()).await.is_ok());
    }
}
