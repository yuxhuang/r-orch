//! Issue 273: public Storage retry reads retain the leaf's damage checks.
//! Synthetic rows/objects only; no daemon and no external CLI.
use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;

use orch_core::store::retries::{Retry, RetryCursor};
use orch_core::store::{Caller, Lane, Principal, ReadSnapshot, Storage, StorageExt, StoreError};
use orch_infra::blob::BlobHash;
use orch_infra::data_dir::{DataDir, Holder, LockKind};
use orch_infra::sqlite::{
    ConnectionRole, SqliteStorage, StorageOptions, Synchronous, open_connection,
};

fn owner(project: &str) -> Caller {
    Caller {
        project: project.into(),
        principal: Principal::Owner {
            identity: "synthetic".into(),
        },
    }
}

struct Env {
    _root: tempfile::TempDir,
    data: Arc<DataDir>,
    storage: SqliteStorage,
}

impl Env {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let data = Arc::new(
            DataDir::acquire(
                &root.path().join("data"),
                &Holder {
                    kind: LockKind::Daemon,
                    pid: std::process::id(),
                    started_at: "synthetic".into(),
                    binary_version: "test".into(),
                    executable: "/synthetic/orch".into(),
                },
            )
            .unwrap(),
        );
        let storage = SqliteStorage::open(data.clone(), StorageOptions::default()).unwrap();
        Self {
            _root: root,
            data,
            storage,
        }
    }

    fn sql(&self) -> orch_infra::sqlite::ConnectionHandle<'_> {
        open_connection(&self.data, ConnectionRole::Writer, Synchronous::Normal).unwrap()
    }

    async fn put(&self, project: &str, error: &str) {
        let value = Retry {
            task: "object".into(),
            attempt: 1,
            due_at: 10,
            error: error.into(),
        };
        self.storage
            .transaction(owner(project), Lane::Entities, move |unit| {
                unit.put_retry(&value)
            })
            .await
            .unwrap();
    }

    fn assert_refused(&self, result: orch_core::store::Result<()>) {
        assert!(
            matches!(result, Err(StoreError::Backend(ref error)) if error.to_string() == "SQLite storage operation failed"),
            "{result:?}"
        );
        // RetryMessageCorrupt is currently ordinary in writer/failure.rs. The
        // public port deliberately hides its detail; leaf tests pin that detail.
        assert!(self.storage.failure_state().failure().is_none());
    }
}

#[derive(Clone, Copy, Debug)]
enum Read {
    Point,
    Due,
    Page,
    Continuation,
}
const READS: [Read; 4] = [Read::Point, Read::Due, Read::Page, Read::Continuation];

impl Read {
    async fn call(self, snapshot: &dyn ReadSnapshot) -> orch_core::store::Result<()> {
        match self {
            Self::Point => snapshot.read_retry("object").await.map(|_| ()),
            Self::Due => snapshot.read_due_retries(20, 2).await.map(|_| ()),
            Self::Page => snapshot.read_retries(None, 2).await.map(|_| ()),
            Self::Continuation => snapshot
                .read_retries(
                    Some(&RetryCursor {
                        due_at: 0,
                        task: "a".into(),
                    }),
                    2,
                )
                .await
                .map(|_| ()),
        }
    }
}

// A real EACCES is recoverable by itself; mixed-row reads must refuse rather
// than return a partial page. The leaf test pins the structural error precedence
// because the public port hides both errors behind the same refusal.
#[tokio::test]
async fn composed_mixed_retry_pages_refuse_partial_results() {
    for read in [Read::Due, Read::Page, Read::Continuation] {
        for due in [5, 15] {
            let env = Env::new();
            let text = "a".repeat(70_000);
            env.put("p", &text).await;
            let hash = BlobHash::of(text.as_bytes()).to_string();
            let path = env
                ._root
                .path()
                .join("data")
                .join("blobs/sha256")
                .join(&hash[..2])
                .join(&hash[2..4])
                .join(&hash);
            let permissions = std::fs::metadata(&path).unwrap().permissions();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0)).unwrap();
            let snapshot = env.storage.read(owner("p")).await.unwrap();
            let access = snapshot.read_retry("object").await;
            assert!(
                matches!(access, Err(StoreError::Backend(ref error)) if error.to_string() == "SQLite storage operation failed"),
                "{access:?}"
            );
            assert!(env.storage.failure_state().failure().is_none());
            drop(snapshot);
            env.sql().execute(
                r#"INSERT INTO retry_queue(project_id,task_id,attempt,due_at,error) VALUES ('p','damaged',0,?1,'{"v":1,"inline":"ok"}')"#, [due],
            ).unwrap();
            let snapshot = env.storage.read(owner("p")).await.unwrap();
            env.assert_refused(read.call(snapshot.as_ref()).await);
            drop(snapshot);
            let fresh = env.storage.read(owner("p")).await.unwrap();
            assert!(fresh.read_retry("absent").await.unwrap().is_none());
            drop(fresh);
            std::fs::set_permissions(&path, permissions).unwrap();
            env.storage.close().await.unwrap();
        }
    }
}

// Looking up by hash without project_id would read the shared object and pass.
#[tokio::test]
async fn composed_retry_reads_require_registration_in_the_callers_project() {
    for read in READS {
        let env = Env::new();
        let text = "a".repeat(70_000);
        env.put("p", &text).await;
        env.put("other", &text).await;
        env.sql()
            .execute("DELETE FROM blobs WHERE project_id='p'", [])
            .unwrap();
        let other = env.storage.read(owner("other")).await.unwrap();
        assert_eq!(
            other.read_retry("object").await.unwrap().unwrap().error,
            text
        );
        drop(other);
        let snapshot = env.storage.read(owner("p")).await.unwrap();
        env.assert_refused(read.call(snapshot.as_ref()).await);
        drop(snapshot);
        let fresh = env.storage.read(owner("p")).await.unwrap();
        assert!(fresh.read_retry("absent").await.unwrap().is_none());
        drop(fresh);
        env.storage.close().await.unwrap();
    }
}

// Exercise all public query shapes with the leaf size-guard fixture. The leaf
// test distinguishes SQL shielding from the parser bound; the port hides detail.
#[tokio::test]
async fn composed_retry_reads_refuse_oversized_invalid_utf8_documents() {
    for read in READS {
        let env = Env::new();
        env.put("p", "ok").await;
        env.sql()
            .execute(
                "UPDATE retry_queue SET error=CAST(x'80' || zeroblob(4194304) AS TEXT)",
                [],
            )
            .unwrap();
        let snapshot = env.storage.read(owner("p")).await.unwrap();
        env.assert_refused(read.call(snapshot.as_ref()).await);
        drop(snapshot);
        let fresh = env.storage.read(owner("p")).await.unwrap();
        assert!(fresh.read_retry("absent").await.unwrap().is_none());
        drop(fresh);
        env.storage.close().await.unwrap();
    }
}
