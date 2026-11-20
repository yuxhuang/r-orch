//! Throwaway storage experiment. Seconds and generation identities are injected.
//! This is deliberately not a production storage port or clock implementation.
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use std::{path::Path, time::Duration};

mod holder;
pub use holder::Holder;

pub const LEASE: i64 = 120;
pub const GIVE_UP: i64 = 90;
pub const RENEW: i64 = 15;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Token {
    pub project: String,
    pub task: String,
    pub agent: String,
    pub run: String,
    pub lineage: String,
    pub epoch: i64,
    pub session: String,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Refusal {
    TaskBusy,
    AgentBusy,
    Superseded,
    Expired,
    Reattach,
    Session,
    NotReady,
    Overflow,
    Injected,
}

#[derive(Debug)]
pub enum Error {
    Refused(Refusal),
    Sql(rusqlite::Error),
}
impl From<rusqlite::Error> for Error {
    fn from(e: rusqlite::Error) -> Self {
        Self::Sql(e)
    }
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for Error {}
pub type Result<T> = std::result::Result<T, Error>;

fn refuse<T>(why: Refusal) -> Result<T> {
    Err(Error::Refused(why))
}
fn deadline(now: i64) -> Result<i64> {
    now.checked_add(LEASE)
        .ok_or(Error::Refused(Refusal::Overflow))
}
fn injected(fail_after: Option<usize>, step: usize) -> Result<()> {
    if fail_after == Some(step) {
        refuse(Refusal::Injected)
    } else {
        Ok(())
    }
}

pub struct Store {
    pub db: Connection,
    session: String,
    lineage: String,
}

impl Store {
    pub fn create(path: &Path) -> Result<Self> {
        let db = Connection::open(path)?;
        db.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL; PRAGMA foreign_keys=ON;",
        )?;
        db.busy_timeout(Duration::from_secs(5))?;
        db.execute_batch(include_str!("schema.sql"))?;
        Self::from_connection(db)
    }

    pub fn connect(path: &Path) -> Result<Self> {
        let db = Connection::open(path)?;
        db.execute_batch("PRAGMA synchronous=NORMAL; PRAGMA foreign_keys=ON;")?;
        db.busy_timeout(Duration::from_secs(5))?;
        Self::from_connection(db)
    }

    fn from_connection(db: Connection) -> Result<Self> {
        let (lineage, session) = db.query_row("SELECT lineage, session FROM daemon", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })?;
        Ok(Self {
            db,
            session,
            lineage,
        })
    }

    pub fn task(&self, project: &str, task: &str) -> Result<()> {
        self.db.execute(
            "INSERT INTO tasks (project,task,status) VALUES (?1,?2,'ready')",
            params![project, task],
        )?;
        self.db
            .execute("INSERT INTO retries VALUES (?1,?2)", params![project, task])?;
        Ok(())
    }

    fn context(tx: &Transaction<'_>, lineage: &str, session: &str) -> Result<()> {
        let matches: bool = tx.query_row(
            "SELECT lineage=?1 AND session=?2 FROM daemon",
            params![lineage, session],
            |r| r.get(0),
        )?;
        if matches {
            Ok(())
        } else {
            refuse(Refusal::Session)
        }
    }

    /// Failpoints are an experiment input: any early return drops/rolls back the transaction.
    pub fn claim(
        &mut self,
        project: &str,
        task: &str,
        agent: &str,
        run: &str,
        now: i64,
        fail_after: Option<usize>,
    ) -> Result<Token> {
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        Self::context(&tx, &self.lineage, &self.session)?;
        Self::reap_in(&tx, now)?;
        // The insert is the arbiter. Preflight SELECTs alone are not a claim.
        tx.execute(
            "INSERT INTO task_epochs VALUES (?1,?2,0) ON CONFLICT DO NOTHING",
            params![project, task],
        )?;
        let previous: i64 = tx.query_row(
            "SELECT epoch FROM task_epochs WHERE project=?1 AND task=?2",
            params![project, task],
            |r| r.get(0),
        )?;
        let epoch = previous
            .checked_add(1)
            .ok_or(Error::Refused(Refusal::Overflow))?;
        tx.execute(
            "UPDATE task_epochs SET epoch=?3 WHERE project=?1 AND task=?2",
            params![project, task, epoch],
        )?;
        injected(fail_after, 1)?;
        let token = Token {
            project: project.into(),
            task: task.into(),
            agent: agent.into(),
            run: run.into(),
            lineage: self.lineage.clone(),
            epoch,
            session: self.session.clone(),
        };
        let insert = tx.execute(
            "INSERT INTO leases VALUES (?1,?2,?3,?4,?5,?6,?7,?8,'active')",
            params![
                project,
                task,
                agent,
                run,
                token.lineage,
                epoch,
                token.session,
                deadline(now)?
            ],
        );
        if let Err(e) = insert {
            if matches!(&e, rusqlite::Error::SqliteFailure(err, _) if err.code == rusqlite::ErrorCode::ConstraintViolation)
            {
                let task_busy: bool = tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM leases WHERE project=?1 AND task=?2)",
                    params![project, task],
                    |r| r.get(0),
                )?;
                return refuse(if task_busy {
                    Refusal::TaskBusy
                } else {
                    Refusal::AgentBusy
                });
            }
            return Err(e.into());
        }
        injected(fail_after, 2)?;
        tx.execute(
            "INSERT INTO runs VALUES (?1,?2,?3,?4,?5,?6,'running')",
            params![project, run, task, agent, token.lineage, epoch],
        )?;
        injected(fail_after, 3)?;
        if tx.execute(
            "UPDATE tasks SET status='in_progress' WHERE project=?1 AND task=?2 AND status='ready'",
            params![project, task],
        )? != 1
        {
            return refuse(Refusal::NotReady);
        }
        injected(fail_after, 4)?;
        tx.execute(
            "DELETE FROM retries WHERE project=?1 AND task=?2",
            params![project, task],
        )?;
        injected(fail_after, 5)?;
        tx.execute(
            "INSERT INTO intents VALUES (?1,?2,'pending')",
            params![project, run],
        )?;
        injected(fail_after, 6)?;
        tx.commit()?;
        Ok(token)
    }

    fn check(tx: &Transaction<'_>, token: &Token, now: i64, recovering: bool) -> Result<()> {
        let (lineage, session): (String, String) =
            tx.query_row("SELECT lineage,session FROM daemon", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })?;
        let epoch: Option<i64> = tx
            .query_row(
                "SELECT epoch FROM task_epochs WHERE project=?1 AND task=?2",
                params![token.project, token.task],
                |r| r.get(0),
            )
            .optional()?;
        if lineage != token.lineage || epoch != Some(token.epoch) {
            return refuse(Refusal::Superseded);
        }
        type Lease = (String, String, String, String, i64, String);
        let lease: Option<Lease> = tx.query_row("SELECT agent,run,lineage,session,deadline,state FROM leases WHERE project=?1 AND task=?2 AND epoch=?3",
            params![token.project,token.task,token.epoch], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?))).optional()?;
        let Some((agent, run, lease_lineage, lease_session, end, state)) = lease else {
            return refuse(Refusal::Expired);
        };
        if agent != token.agent || run != token.run || lease_lineage != token.lineage {
            return refuse(Refusal::Superseded);
        }
        if lease_session != token.session {
            return refuse(Refusal::Session);
        }
        if now >= end {
            return refuse(Refusal::Expired);
        }
        if recovering {
            if state != "recovering" {
                return refuse(Refusal::Reattach);
            }
        } else if state != "active" {
            return refuse(Refusal::Reattach);
        } else if session != token.session {
            return refuse(Refusal::Session);
        }
        Ok(())
    }

    pub fn renew(&mut self, token: &Token, now: i64) -> Result<()> {
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        Self::context(&tx, &self.lineage, &self.session)?;
        Self::check(&tx, token, now, false)?;
        tx.execute(
            "UPDATE leases SET deadline=?3 WHERE project=?1 AND task=?2",
            params![token.project, token.task, deadline(now)?],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn finish(&mut self, token: &Token, now: i64, success: bool) -> Result<()> {
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        Self::context(&tx, &self.lineage, &self.session)?;
        Self::check(&tx, token, now, false)?;
        tx.execute(
            "UPDATE runs SET status=?3 WHERE project=?1 AND run=?2",
            params![
                token.project,
                token.run,
                if success { "succeeded" } else { "failed" }
            ],
        )?;
        tx.execute(
            "UPDATE tasks SET status=?3,attempts=attempts+?4 WHERE project=?1 AND task=?2",
            params![
                token.project,
                token.task,
                if success { "done" } else { "ready" },
                i64::from(!success)
            ],
        )?;
        if !success {
            tx.execute(
                "INSERT INTO retries VALUES (?1,?2)",
                params![token.project, token.task],
            )?;
        }
        tx.execute(
            "DELETE FROM leases WHERE project=?1 AND task=?2",
            params![token.project, token.task],
        )?;
        tx.commit()?;
        Ok(())
    }

    fn lose(tx: &Transaction<'_>, project: &str, task: &str, run: &str) -> Result<()> {
        tx.execute(
            "UPDATE runs SET status='lost' WHERE project=?1 AND run=?2",
            params![project, run],
        )?;
        tx.execute(
            "UPDATE tasks SET status='ready' WHERE project=?1 AND task=?2",
            params![project, task],
        )?;
        tx.execute(
            "INSERT INTO retries VALUES (?1,?2) ON CONFLICT DO NOTHING",
            params![project, task],
        )?;
        tx.execute(
            "UPDATE intents SET state='cancelled' WHERE project=?1 AND run=?2",
            params![project, run],
        )?;
        tx.execute(
            "DELETE FROM leases WHERE project=?1 AND task=?2",
            params![project, task],
        )?;
        Ok(())
    }

    fn reap_in(tx: &Transaction<'_>, now: i64) -> Result<usize> {
        let mut query = tx.prepare("SELECT project,task,run FROM leases WHERE deadline<=?1")?;
        let rows = query
            .query_map([now], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        for (project, task, run) in &rows {
            Self::lose(tx, project, task, run)?;
        }
        Ok(rows.len())
    }

    pub fn reap(&mut self, now: i64) -> Result<usize> {
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        Self::context(&tx, &self.lineage, &self.session)?;
        let count = Self::reap_in(&tx, now)?;
        tx.commit()?;
        Ok(count)
    }

    /// Called exactly once under the exclusive daemon data-directory lock.
    /// A new process clock domain is allowed to start at zero.
    pub fn restart(&mut self, session: &str, now: i64) -> Result<()> {
        if session == self.session {
            return refuse(Refusal::Session);
        }
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        Self::context(&tx, &self.lineage, &self.session)?;
        tx.execute("UPDATE daemon SET session=?1", [session])?;
        tx.execute(
            "UPDATE leases SET state='recovering',deadline=?1",
            [deadline(now)?],
        )?;
        tx.commit()?;
        self.session = session.into();
        Ok(())
    }

    pub fn reattach(&mut self, token: &Token, now: i64) -> Result<Token> {
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        Self::context(&tx, &self.lineage, &self.session)?;
        Self::check(&tx, token, now, true)?;
        tx.execute(
            "UPDATE leases SET state='active',session=?3,deadline=?4 WHERE project=?1 AND task=?2",
            params![token.project, token.task, self.session, deadline(now)?],
        )?;
        tx.commit()?;
        let mut updated = token.clone();
        updated.session = self.session.clone();
        Ok(updated)
    }

    pub fn backup(&self, path: &Path) -> Result<()> {
        self.db.backup(
            rusqlite::MAIN_DB,
            path,
            None::<fn(rusqlite::backup::Progress)>,
        )?;
        Ok(())
    }

    /// Fresh lineage is an external input, never recovered from this snapshot.
    /// Production must generate a collision-resistant identity before enabling work.
    pub fn restore(&mut self, lineage: &str, session: &str) -> Result<()> {
        if lineage == self.lineage {
            return refuse(Refusal::Superseded);
        }
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        Self::context(&tx, &self.lineage, &self.session)?;
        tx.execute_batch(
            "INSERT INTO task_epochs (project,task,epoch)
          SELECT project,task,MAX(epoch) FROM (
            SELECT project,task,epoch FROM task_epochs UNION ALL
            SELECT project,task,epoch FROM runs UNION ALL
            SELECT project,task,epoch FROM leases
          ) GROUP BY project,task
          ON CONFLICT(project,task) DO UPDATE SET epoch=MAX(task_epochs.epoch,excluded.epoch);",
        )?;
        Self::reap_in(&tx, i64::MAX)?;
        tx.execute(
            "UPDATE daemon SET lineage=?1,session=?2",
            params![lineage, session],
        )?;
        tx.commit()?;
        self.lineage = lineage.into();
        self.session = session.into();
        Ok(())
    }
}

#[cfg(test)]
mod tests;
