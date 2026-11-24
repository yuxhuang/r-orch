//! The public Task cursor token and its private encoding.
//!
//! A token is lowercase hex of `version, sort tag, expiry (i64 ms, BE), a
//! 16-byte binding digest, the integer seek keys (i64, BE), the last row's ID`.
//! It holds no snapshot and no filter text: the digest covers the project, the
//! filter and the daemon generation, and the sort tag is compared to the request's
//! by `Continuation::from_decoded`, so a token is refused for any other query,
//! project or generation (R10), and after its expiry.
//!
//! The token is not secret and carries no MAC. Authentication belongs to the
//! caller: the project is the authorized caller's, never the token's. Every
//! decoded component is validated (`Continuation::from_decoded`) and the query
//! binds each value as a parameter. A forged token can therefore only start a
//! walk at another position within the forger's own project, filter and sort,
//! and shows no row that a walk from the start does not. Its expiry claim is
//! bounded by `TTL_MS` on both sides.
use super::super::task_page::{self, Continuation};
use super::super::{Result, SqliteError};
use orch_core::store::{Cursor, Filter, Sort, StoreError};
use rusqlite::types::Value;
use sha2::{Digest, Sha256};

/// How long an issued token resumes a walk.
pub(super) const TTL_MS: i64 = 24 * 60 * 60 * 1000;

const VERSION: u8 = 1;
const DIGEST: usize = 16;
const HEADER: usize = 1 + 1 + 8 + DIGEST;
/// `Cursor` is at most 512 bytes of text and the token is hex.
const MAX_RAW: usize = 256;

const SORTS: [Sort; 6] = [
    Sort::Default,
    Sort::Id,
    Sort::Position,
    Sort::Priority,
    Sort::CreatedAt,
    Sort::UpdatedAt,
];

fn tag(sort: Sort) -> Option<u8> {
    SORTS.iter().position(|s| *s == sort).map(|i| i as u8)
}

/// The generation that issued a token. A restore starts a new one (gate 9), so
/// a token from before it is refused as stale.
pub(in crate::sqlite) struct TaskCursors {
    generation: String,
}

impl TaskCursors {
    pub(in crate::sqlite) fn new(generation: impl Into<String>) -> Self {
        Self {
            generation: generation.into(),
        }
    }

    fn binding(&self, project: &str, filter: &Filter) -> Result<[u8; DIGEST]> {
        let Filter::Tasks { status, goal } = filter else {
            return Err(StoreError::InvalidCursor.into());
        };
        let mut hash = Sha256::new();
        hash.update(b"orch task cursor v1");
        // Length-prefixed fields cannot run into one another.
        let field = |hash: &mut Sha256, tag: u8, bytes: Option<&[u8]>| {
            hash.update([tag, u8::from(bytes.is_some())]);
            let bytes = bytes.unwrap_or_default();
            hash.update((bytes.len() as u64).to_be_bytes());
            hash.update(bytes);
        };
        field(&mut hash, 1, Some(project.as_bytes()));
        field(&mut hash, 2, Some(self.generation.as_bytes()));
        field(
            &mut hash,
            4,
            status.map(|s| s.to_string()).as_deref().map(str::as_bytes),
        );
        field(&mut hash, 5, goal.as_deref().map(str::as_bytes));
        let mut digest = [0; DIGEST];
        digest.copy_from_slice(&hash.finalize()[..DIGEST]);
        Ok(digest)
    }

    /// Encode the continuation that `task_page::query` returned for this query.
    /// An ID too long for 512 bytes of token is refused, never truncated.
    pub(super) fn issue(
        &self,
        project: &str,
        filter: &Filter,
        sort: Sort,
        next: &Continuation,
        now_ms: i64,
    ) -> Result<Cursor> {
        let shape = || SqliteError::Unrepresentable("task continuation shape".into());
        let Some((Value::Text(id), prefix)) = next.seek().split_last() else {
            return Err(shape());
        };
        let mut raw = vec![VERSION, tag(sort).ok_or_else(shape)?];
        raw.extend(now_ms.saturating_add(TTL_MS).to_be_bytes());
        raw.extend(self.binding(project, filter)?);
        for value in prefix {
            let Value::Integer(key) = value else {
                return Err(shape());
            };
            raw.extend(key.to_be_bytes());
        }
        raw.extend(id.as_bytes());
        if raw.len() > MAX_RAW {
            return Err(SqliteError::Unrepresentable(
                "last task ID is too long for a 512-byte cursor".into(),
            ));
        }
        Ok(Cursor::new(hex(&raw))?)
    }

    /// Decode and validate a token for this authorized project and request.
    /// Every refusal is `InvalidCursor`: malformed, another query, another
    /// generation and expired tokens are one class to the caller.
    pub(super) fn resume(
        &self,
        cursor: &Cursor,
        project: &str,
        filter: &Filter,
        sort: Sort,
        now_ms: i64,
    ) -> std::result::Result<Continuation, StoreError> {
        let invalid = || StoreError::InvalidCursor;
        let raw = unhex(cursor.as_str()).ok_or_else(invalid)?;
        if raw.len() < HEADER || raw[0] != VERSION {
            return Err(invalid());
        }
        let decoded_sort = *SORTS.get(usize::from(raw[1])).ok_or_else(invalid)?;
        let expires = i64::from_be_bytes(raw[2..10].try_into().map_err(|_| invalid())?);
        // The claim is unauthenticated: refuse one that outlives any token this
        // generation could have issued.
        if now_ms > expires || i128::from(expires) - i128::from(now_ms) > i128::from(TTL_MS) {
            return Err(invalid());
        }
        let binding = self.binding(project, filter).map_err(|_| invalid())?;
        if raw[10..HEADER] != binding {
            return Err(invalid());
        }
        let keys = task_page::seek_width(decoded_sort)? - 1;
        let tail = raw.get(HEADER..HEADER + keys * 8).ok_or_else(invalid)?;
        let mut values = tail
            .as_chunks::<8>()
            .0
            .iter()
            .map(|key| Value::Integer(i64::from_be_bytes(*key)))
            .collect::<Vec<_>>();
        let id = std::str::from_utf8(&raw[HEADER + keys * 8..]).map_err(|_| invalid())?;
        values.push(Value::Text(id.to_owned()));
        // The digest proved the binding of project and filter; the decoded sort
        // is the token's own and the shared validation compares it to the request.
        Continuation::from_decoded(
            project.to_owned(),
            filter.clone(),
            decoded_sort,
            values,
            project,
            filter,
            sort,
        )
    }
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    bytes
        .iter()
        .flat_map(|b| [DIGITS[usize::from(b >> 4)], DIGITS[usize::from(b & 15)]])
        .map(char::from)
        .collect()
}

/// Lowercase only: one position has one token.
fn unhex(text: &str) -> Option<Vec<u8>> {
    let nibble = |c: u8| match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        _ => None,
    };
    let (pairs, rest) = text.as_bytes().as_chunks::<2>();
    if !rest.is_empty() {
        return None;
    }
    pairs
        .iter()
        .map(|[high, low]| Some(nibble(*high)? << 4 | nibble(*low)?))
        .collect()
}
