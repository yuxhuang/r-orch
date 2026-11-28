//! Native import batches. No completion or cutover is exposed by this slice.
use super::*;
use crate::import::ImportBatch;
use orch_core::store::{Principal, UnitOfWork};
use rusqlite::{OptionalExtension, params};

impl SqliteStorage {
    /// Validate the complete checked source against every retained template before
    /// admitting new rows. Missing files and changed native rows cannot be hidden
    /// by processing a newly added name first. This transaction changes no rows.
    pub async fn check_import_source(
        &self,
        caller: Caller,
        source: String,
        templates: std::collections::BTreeMap<String, String>,
    ) -> store::Result<WriteAttempt> {
        if !matches!(caller.principal, Principal::Owner { .. })
            || !std::path::Path::new(&source).is_absolute()
        {
            return Err(backend("import requires an owner and absolute source"));
        }
        self.retained_write(caller, store::Lane::Entities, None)
            .attempt_unit(Box::new(move |unit| {
                unit.check_import_source(source, templates)
            }))
            .await
    }

    /// Commit native entities, source hashes and unfinished progress together.
    /// This method never marks an import complete or publishes a project pointer.
    /// The offline caller owns the legacy/data guards, preflight, disk floor and
    /// cancellation flag; it must await this method before releasing those guards.
    /// An error or unknown outcome is not permission to replay an admitted batch.
    pub async fn import_batch(
        &self,
        caller: Caller,
        batch: ImportBatch,
    ) -> store::Result<WriteAttempt> {
        if !matches!(caller.principal, Principal::Owner { .. }) {
            return Err(backend("import requires an owner"));
        }
        if batch.rows.is_empty() || batch.rows.len() > 1000 {
            return Err(backend("import batch must contain 1 to 1000 entities"));
        }
        if !std::path::Path::new(&batch.source).is_absolute() {
            return Err(backend("import source must be absolute"));
        }
        for row in &batch.rows {
            if row.source_sha256.len() != 64
                || !row
                    .source_sha256
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            {
                return Err(backend("import source hash must be lowercase SHA-256"));
            }
        }
        self.retained_write(caller, store::Lane::Entities, None)
            .attempt_unit(Box::new(move |unit| unit.apply_import_batch(batch)))
            .await
    }
}

impl Unit<'_, '_> {
    fn check_import_source(
        &mut self,
        source: String,
        templates: std::collections::BTreeMap<String, String>,
    ) -> store::Result<()> {
        use sha2::{Digest, Sha256};
        port(self.ensure_committable())?;
        let result: Result<()> = (|| {
            let project = self.caller().project.clone();
            let expected_key = format!("import:{project}");
            let expected =
                serde_json::json!({"source": source, "status": "unfinished"}).to_string();
            let mut progress = self
                .tx
                .prepare("SELECT key,value FROM daemon_config WHERE key LIKE 'import:%'")?;
            let rows =
                progress.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
            for row in rows {
                let (key, value) = row?;
                if key != expected_key || value != expected {
                    return Err(backend(
                        "import destination belongs to another source or is complete",
                    )
                    .into());
                }
            }
            let mut manifest = self.tx.prepare(
                "SELECT entity,entity_id,source_sha256 FROM import_manifest WHERE project_id=?1",
            )?;
            let rows = manifest.query_map([&project], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })?;
            for row in rows {
                let (family, id, hash) = row?;
                if family != "template" || templates.get(&id) != Some(&hash) {
                    return Err(
                        backend("imported source changed or is missing; resume refused").into(),
                    );
                }
                let native = self.get(EntityKind::Template, &id)?;
                let Some(Record {
                    entity: Entity::Template(template),
                    ..
                }) = native
                else {
                    return Err(
                        backend("imported native template is missing; resume refused").into(),
                    );
                };
                if format!("{:x}", Sha256::digest(template.body.as_bytes())) != hash {
                    return Err(backend("imported native template changed; resume refused").into());
                }
            }
            Ok(())
        })();
        self.observed(result)
    }

    fn apply_import_batch(&mut self, batch: ImportBatch) -> store::Result<()> {
        port(self.ensure_committable())?;
        let result: Result<()> = (|| {
            let project = self.caller().project.clone();
            let key = format!("import:{project}");
            let progress =
                serde_json::json!({"source": batch.source, "status": "unfinished"}).to_string();
            let prior: Option<String> = self
                .tx
                .query_row(
                    "SELECT value FROM daemon_config WHERE key=?1",
                    [&key],
                    |r| r.get(0),
                )
                .optional()?;
            if prior.as_ref().is_some_and(|p| p != &progress) {
                return Err(
                    backend("import destination belongs to another source or is complete").into(),
                );
            }
            for row in batch.rows {
                let family = match row.entity.kind() {
                    EntityKind::Task => "task",
                    EntityKind::Agent => "agent",
                    EntityKind::Goal => "goal",
                    EntityKind::Team => "team",
                    EntityKind::Run => "run",
                    EntityKind::Message => "message",
                    EntityKind::Context => "context",
                    EntityKind::Config => "config",
                    EntityKind::Template => "template",
                };
                let id = row.entity.id().to_owned();
                let previous: Option<String> = self.tx.query_row(
                    "SELECT source_sha256 FROM import_manifest WHERE project_id=?1 AND entity=?2 AND entity_id=?3",
                    params![project, family, id], |r| r.get(0)).optional()?;
                if let Some(hash) = previous {
                    let native = self.get(row.entity.kind(), &id)?;
                    if hash != row.source_sha256 || native.is_none_or(|r| r.entity != row.entity) {
                        return Err(backend(
                            "imported source or native entity changed; resume refused",
                        )
                        .into());
                    }
                    continue;
                }
                self.create(row.entity)?;
                self.tx.execute("INSERT INTO import_manifest(project_id,entity,entity_id,source_sha256,imported_at) VALUES(?1,?2,?3,?4,?5)", params![project,family,id,row.source_sha256,self.now])?;
            }
            self.tx.execute("INSERT INTO daemon_config(key,value,revision) VALUES(?1,?2,1) ON CONFLICT(key) DO UPDATE SET value=excluded.value,revision=daemon_config.revision+1", params![key,progress])?;
            Ok(())
        })();
        self.observed(result)
    }
}

#[cfg(test)]
mod tests {
    use super::super::*;
    use crate::import::{ImportBatch, ImportRow};
    use orch_core::store::{Principal, Storage};

    fn caller() -> Caller {
        Caller {
            project: "synthetic".into(),
            principal: Principal::Owner {
                identity: "local_owner".into(),
            },
        }
    }
    fn row(name: &str, hash: char) -> ImportRow {
        ImportRow {
            entity: Entity::Template(store::TemplateEntry {
                name: name.into(),
                body: "synthetic body".into(),
            }),
            source_sha256: hash.to_string().repeat(64),
        }
    }
    fn batch(rows: Vec<ImportRow>) -> ImportBatch {
        ImportBatch {
            source: "/synthetic/source".into(),
            rows,
        }
    }
    fn manifest_count(data: &DataDir) -> i64 {
        rusqlite::Connection::open_with_flags(
            data.database_path(),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap()
        .query_row("SELECT count(*) FROM import_manifest", [], |r| r.get(0))
        .unwrap()
    }
    #[tokio::test]
    async fn entity_manifest_and_unfinished_state_commit_and_resume_together() {
        let (_root, data) = crate::sqlite::test_data();
        let data = Arc::new(data);
        let storage = SqliteStorage::open(data.clone(), StorageOptions::default()).unwrap();
        let first = storage
            .import_batch(caller(), batch(vec![row("", 'a')]))
            .await
            .unwrap();
        first.result.unwrap();
        assert_eq!(first.outcome, super::super::super::WriteOutcome::Committed);
        assert_eq!(manifest_count(&data), 1);
        let view = storage.read(caller()).await.unwrap();
        let record = view.get(EntityKind::Template, "").await.unwrap().unwrap();
        assert_eq!(record.revision, Revision::ONE);
        drop(view);
        storage
            .import_batch(caller(), batch(vec![row("", 'a')]))
            .await
            .unwrap()
            .result
            .unwrap();
        assert_eq!(manifest_count(&data), 1);
        let conn = rusqlite::Connection::open_with_flags(
            data.database_path(),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        let progress: String = conn
            .query_row(
                "SELECT value FROM daemon_config WHERE key='import:synthetic'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&progress).unwrap()["status"],
            "unfinished"
        );
        drop(conn);
        storage.close().await.unwrap();
    }
    #[tokio::test]
    async fn changed_hash_rolls_back_other_entities_and_manifests() {
        let (_root, data) = crate::sqlite::test_data();
        let data = Arc::new(data);
        let storage = SqliteStorage::open(data.clone(), StorageOptions::default()).unwrap();
        storage
            .import_batch(caller(), batch(vec![row("existing", 'a')]))
            .await
            .unwrap()
            .result
            .unwrap();
        let failed = storage
            .import_batch(
                caller(),
                batch(vec![row("rolled-back", 'b'), row("existing", 'c')]),
            )
            .await
            .unwrap();
        assert!(failed.result.is_err());
        assert_eq!(
            failed.outcome,
            super::super::super::WriteOutcome::RolledBack
        );
        let view = storage.read(caller()).await.unwrap();
        assert!(
            view.get(EntityKind::Template, "rolled-back")
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(manifest_count(&data), 1);
        drop(view);
        storage.close().await.unwrap();
    }
    #[tokio::test]
    async fn failed_first_batch_leaves_no_entities_manifests_or_progress() {
        let (_root, data) = crate::sqlite::test_data();
        let data = Arc::new(data);
        let storage = SqliteStorage::open(data.clone(), StorageOptions::default()).unwrap();
        let failed = storage
            .import_batch(
                caller(),
                batch(vec![row("duplicate", 'a'), row("duplicate", 'b')]),
            )
            .await
            .unwrap();
        assert!(failed.result.is_err());
        assert_eq!(
            failed.outcome,
            super::super::super::WriteOutcome::RolledBack
        );
        let view = storage.read(caller()).await.unwrap();
        assert!(
            view.get(EntityKind::Template, "duplicate")
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(manifest_count(&data), 0);
        let conn = rusqlite::Connection::open_with_flags(
            data.database_path(),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        let count: i64 = conn
            .query_row(
                "SELECT count(*) FROM daemon_config WHERE key='import:synthetic'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 0);
        drop(conn);
        drop(view);
        storage.close().await.unwrap();
    }
    #[tokio::test]
    async fn nonowners_and_invalid_batches_are_refused_before_admission() {
        let (_root, data) = crate::sqlite::test_data();
        let data = Arc::new(data);
        let storage = SqliteStorage::open(data.clone(), StorageOptions::default()).unwrap();
        let mut worker = caller();
        worker.principal = Principal::Worker {
            identity: "local_worker".into(),
        };
        assert!(
            storage
                .import_batch(worker, batch(vec![row("denied", 'a')]))
                .await
                .is_err()
        );
        assert!(storage.import_batch(caller(), batch(vec![])).await.is_err());
        assert!(
            storage
                .import_batch(caller(), batch(vec![row("bad-hash", 'z')]))
                .await
                .is_err()
        );
        assert!(
            storage
                .import_batch(caller(), batch(vec![row("too-many", 'a'); 1001]))
                .await
                .is_err()
        );
        let mut relative = batch(vec![row("relative", 'a')]);
        relative.source = "relative".into();
        assert!(storage.import_batch(caller(), relative).await.is_err());
        assert_eq!(manifest_count(&data), 0);
        storage.close().await.unwrap();
    }
    #[tokio::test]
    async fn another_source_cannot_resume_into_the_destination() {
        let (_root, data) = crate::sqlite::test_data();
        let data = Arc::new(data);
        let storage = SqliteStorage::open(data.clone(), StorageOptions::default()).unwrap();
        storage
            .import_batch(caller(), batch(vec![row("first", 'a')]))
            .await
            .unwrap()
            .result
            .unwrap();
        let mut other = batch(vec![row("second", 'b')]);
        other.source = "/synthetic/other".into();
        assert!(
            storage
                .import_batch(caller(), other)
                .await
                .unwrap()
                .result
                .is_err()
        );
        assert_eq!(manifest_count(&data), 1);
        storage.close().await.unwrap();
    }
}
