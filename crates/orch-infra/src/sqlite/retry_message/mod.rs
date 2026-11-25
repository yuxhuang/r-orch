//! SQLite retry mapping (T10, issue 109): `retry_queue` rows behind the retry
//! port, with whole diagnostics stored by R97.
//!
//! The helpers borrow the caller's connection or writer transaction, never
//! commit it, and are not the Send-bound `UnitOfWork` facade: the combined
//! adapter binds the authenticated caller and the failure state around them.
//! No clock is read; deadlines and the time a blob registration is stamped are
//! supplied by the caller. Message and attempt mappings are not here: they need
//! a persisted format that is not decided yet.
use super::SqliteError;
use crate::blob::{BlobError, ReferenceError};

mod retries;
mod text;

#[allow(
    unused_imports,
    reason = "internal retry mapping awaits the combined T10 Storage adapter"
)]
pub(super) use retries::{NamedObject, Retries, RetryReader};

#[cfg(test)]
mod tests;

fn corrupt(detail: &'static str) -> SqliteError {
    SqliteError::RetryMessageCorrupt(detail)
}

/// A publication failure keeps its structure so the writer's policy can class
/// it: an object that could not be written proves no database damage.
fn publish_error(error: BlobError) -> SqliteError {
    match error {
        BlobError::Io(error) => SqliteError::Io(error),
        BlobError::Confined(error) => SqliteError::Confined(error),
        other => SqliteError::Io(std::io::Error::other(other.to_string())),
    }
}

fn text_error(error: text::TextError) -> SqliteError {
    use text::TextError::*;
    match error {
        Publish(error) => publish_error(error),
        Unrepresentable => SqliteError::Unrepresentable(
            "retry diagnostic exceeds the stored document bound".into(),
        ),
        Access(kind) => SqliteError::Io(kind.into()),
        Corrupt(detail) => corrupt(detail),
    }
}

fn reference_error(error: ReferenceError) -> SqliteError {
    match error {
        ReferenceError::Blob(error) => publish_error(error),
        ReferenceError::Sql(error) => SqliteError::Sql(error),
        ReferenceError::MissingOwner { .. }
        | ReferenceError::Metadata
        | ReferenceError::PromptEncoding => corrupt("blob registration"),
    }
}
