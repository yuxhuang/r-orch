//! Exercises the port across a real writer-task boundary without any daemon.

use std::sync::Arc;

use async_trait::async_trait;
use orch_core::store::{fake::FakeStorage, *};
use tokio::sync::{mpsc, oneshot};

use crate::v2::Backend;

struct WriteJob {
    caller: Caller,
    lane: Lane,
    work: WriteWork,
    reply: oneshot::Sender<Result<()>>,
}

struct QueuedStorage {
    memory: Arc<FakeStorage>,
    jobs: mpsc::Sender<WriteJob>,
}

struct Worker(tokio::task::JoinHandle<()>);

impl Drop for Worker {
    fn drop(&mut self) {
        self.0.abort();
    }
}

pub fn backend() -> Backend {
    let memory = Arc::new(FakeStorage::default());
    let (jobs, mut inbox) = mpsc::channel::<WriteJob>(16);
    let writer = memory.clone();
    let worker = tokio::spawn(async move {
        while let Some(job) = inbox.recv().await {
            let result = writer.write(job.caller, job.lane, job.work).await;
            let _ = job.reply.send(result);
        }
    });
    Backend {
        storage: Arc::new(QueuedStorage { memory, jobs }),
        _guard: Box::new(Worker(worker)),
    }
}

fn stopped() -> StoreError {
    StoreError::Backend(orch_core::Error::other("test writer stopped"))
}

#[async_trait]
impl Storage for QueuedStorage {
    async fn read(&self, caller: Caller) -> Result<Box<dyn ReadSnapshot>> {
        self.memory.read(caller).await
    }

    async fn write(&self, caller: Caller, lane: Lane, work: WriteWork) -> Result<()> {
        let (reply, result) = oneshot::channel();
        self.jobs
            .send(WriteJob {
                caller,
                lane,
                work,
                reply,
            })
            .await
            .map_err(|_| stopped())?;
        result.await.map_err(|_| stopped())?
    }
}

struct RejectCommit(FakeStorage);

#[async_trait]
impl Storage for RejectCommit {
    async fn read(&self, caller: Caller) -> Result<Box<dyn ReadSnapshot>> {
        self.0.read(caller).await
    }

    async fn write(&self, caller: Caller, lane: Lane, work: WriteWork) -> Result<()> {
        self.0
            .write(
                caller,
                lane,
                Box::new(move |unit| {
                    work(unit)?;
                    Err(stopped())
                }),
            )
            .await
    }
}

#[tokio::test]
async fn typed_result_is_not_returned_when_commit_fails() {
    let storage = RejectCommit(FakeStorage::default());
    let caller = Caller {
        principal: Principal::Owner {
            identity: "owner".into(),
        },
        project: "p".into(),
    };
    let result = storage
        .transaction(caller.clone(), Lane::Entities, |unit| {
            unit.create(Entity::Task(Box::new(crate::contract::task("a"))))?;
            Ok(42_u64)
        })
        .await;
    assert!(matches!(result, Err(StoreError::Backend(_))));
    assert_eq!(
        storage
            .read(caller)
            .await
            .unwrap()
            .get(EntityKind::Task, "a")
            .await
            .unwrap(),
        None
    );
}
