//! The public Task cursor token and its private encoding.
//!
//! A token is lowercase hex of `version, sort tag, expiry (i64 ms, BE), a
//! 16-byte binding digest, the integer seek keys (i64, BE), the last row's ID`.
//! An ID that does not fit in the 512 bytes of text is carried by reference
//! instead (version 2): `..., the integer seek keys, its byte length (u32, BE),
//! the first 16 bytes of its SHA-256, a fallback, the longest prefix of it that
//! fits`. The resume finds the one row of the authorized project that has that
//! prefix, that length and that digest, in the page's own snapshot, and seeks
//! after its ID.
//!
//! A row deleted between two pages leaves nothing to find, and its ID is not in
//! the token. The fallback is then where the walk goes on, and it is written when
//! the page is issued, from the first row of the next page, `next`, which the
//! page query read to know that there is one. Nothing lies between the last row
//! and `next`, so any position from the last row up to `next` is the same walk.
//! There are three, the first that applies being the one written:
//!
//! - `PastKeys`: `next` has other integer keys. Seek past the keys of the last
//!   row; no ID is needed.
//! - `From`: the keys tie. The last row and `next` share their first `k` bytes,
//!   and `next` has a byte at `k` that the last row does not. Seek from the
//!   shortest prefix of `next` that ends a character past `k`: the token holds
//!   `k` and those bytes, and the first `k` come from the prefix it already
//!   carries. This needs the prefix to reach `k`.
//! - `Successor`: the keys tie and `k` is out of the prefix's reach. Seek from the
//!   row that has the prefix, the length and the digest that the token holds for
//!   `next`, which shares the prefix. If that row is gone too, nothing the token
//!   holds says on which side of the deleted row the surviving rows that share
//!   the prefix lie, and it is refused as `InvalidCursor`, never resumed at a
//!   guessed position.
//!
//! A token holds no snapshot and no filter text: the digest covers the project, the
//! filter and the daemon generation, and the sort tag is compared to the request's
//! by `Continuation::from_decoded_at`, so a token is refused for any other query,
//! project or generation (R10), and after its expiry.
//!
//! The token is not secret and carries no MAC. Authentication belongs to the
//! caller: the project is the authorized caller's, never the token's. Every
//! decoded component is validated (`Continuation::from_decoded_at`) and the query
//! binds each value as a parameter. A forged token can therefore only start a
//! walk at another position within the forger's own project, filter and sort,
//! and shows no row that a walk from the start does not. Its expiry claim is
//! bounded by `TTL_MS` on both sides. A reference whose digest or length is
//! wrong finds no row and so is read as a deleted one: it resumes at its
//! fallback, which is another position of the same kind.
use super::super::task_page::{self, Bound, Continuation};
use super::super::{Result, SqliteError};
use orch_core::store::{Cursor, Filter, Sort, StoreError};
use rusqlite::Transaction;
use rusqlite::types::Value;
use sha2::{Digest, Sha256};

/// How long an issued token resumes a walk.
pub(super) const TTL_MS: i64 = 24 * 60 * 60 * 1000;

/// The last row's ID follows the keys in the token.
const INLINE: u8 = 1;
/// A reference to the last row's ID follows the keys: see the module docs.
const REFERENCE: u8 = 2;
const DIGEST: usize = 16;
const HEADER: usize = 1 + 1 + 8 + DIGEST;
/// `Cursor` is at most 512 bytes of text and the token is hex.
const MAX_RAW: usize = 256;
/// The byte length and the digest of an ID carried by reference, and the byte
/// that says which fallback follows them.
const REFERENCE_FIXED: usize = 4 + DIGEST + 1;
const PAST_KEYS: u8 = 0;
const FROM: u8 = 1;
const SUCCESSOR: u8 = 2;
/// A `From` fallback is `k` (u16, BE), the number of bytes after it, those bytes.
const FROM_FIXED: usize = 2 + 1;
/// A `Successor` fallback is a byte length and a digest, as in the reference.
const SUCCESSOR_LEN: usize = 4 + DIGEST;
/// A prefix is cut at a character boundary, so it may be this much short.
const BOUNDARY_SLACK: usize = 3;

const SORTS: [Sort; 6] = [
    Sort::Default,
    Sort::Id,
    Sort::Position,
    Sort::Priority,
    Sort::CreatedAt,
    Sort::UpdatedAt,
];

/// Where a walk goes on when the row it ended on is gone. See the module docs.
enum Fallback {
    PastKeys,
    /// The bytes of `next` from `k` to the end of its character at `k`; the
    /// first `k` are the deleted row's.
    From {
        k: usize,
        tail: Vec<u8>,
    },
    Successor {
        length: u32,
        digest: [u8; DIGEST],
    },
}

impl Fallback {
    /// The fallback for the last row `id` of a page, whose reference has `room`
    /// bytes after the fixed fields. `next` is the first row of the next page
    /// when it ties with `id` on every integer key, and `None` when it does not.
    fn choose(id: &str, next: Option<&str>, room: usize) -> Option<Self> {
        let Some(next) = next else {
            return Some(Self::PastKeys);
        };
        let k = id
            .bytes()
            .zip(next.bytes())
            .take_while(|(a, b)| a == b)
            .count();
        // `id` sorts before `next`, so `next` has a byte at `k`.
        let end = next.ceil_char_boundary(k + 1);
        let tail = next.as_bytes()[k..end].to_vec();
        let prefix = id.floor_char_boundary(room.checked_sub(FROM_FIXED + tail.len())?);
        if k <= prefix && u16::try_from(k).is_ok() {
            return Some(Self::From { k, tail });
        }
        Some(Self::Successor {
            length: u32::try_from(next.len()).ok()?,
            digest: Sha256::digest(next.as_bytes())[..DIGEST].try_into().ok()?,
        })
    }

    fn write(&self, raw: &mut Vec<u8>) {
        match self {
            Self::PastKeys => raw.push(PAST_KEYS),
            Self::From { k, tail } => {
                raw.push(FROM);
                raw.extend((*k as u16).to_be_bytes());
                raw.push(tail.len() as u8);
                raw.extend(tail);
            }
            Self::Successor { length, digest } => {
                raw.push(SUCCESSOR);
                raw.extend(length.to_be_bytes());
                raw.extend(digest);
            }
        }
    }

    /// The fallback at the start of `bytes` and the rest of them.
    fn read(bytes: &[u8]) -> Option<(Self, &[u8])> {
        let (kind, rest) = bytes.split_first()?;
        match *kind {
            PAST_KEYS => Some((Self::PastKeys, rest)),
            FROM => {
                let (fixed, rest) = rest.split_at_checked(FROM_FIXED)?;
                let k = usize::from(u16::from_be_bytes([fixed[0], fixed[1]]));
                let (tail, rest) = rest.split_at_checked(usize::from(fixed[2]))?;
                Some((
                    Self::From {
                        k,
                        tail: tail.to_vec(),
                    },
                    rest,
                ))
            }
            SUCCESSOR => {
                let (fixed, rest) = rest.split_at_checked(SUCCESSOR_LEN)?;
                Some((
                    Self::Successor {
                        length: u32::from_be_bytes(fixed[..4].try_into().ok()?),
                        digest: fixed[4..].try_into().ok()?,
                    },
                    rest,
                ))
            }
            _ => None,
        }
    }
}

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

    /// Encode the continuation that `task_page::query` returned for this query,
    /// with `following`, the seek tuple of the first row of the next page. An ID
    /// that fits is carried whole; a longer one by reference, so every stored ID
    /// has a token of at most 512 bytes.
    pub(super) fn issue(
        &self,
        project: &str,
        filter: &Filter,
        sort: Sort,
        next: &Continuation,
        following: &[Value],
        now_ms: i64,
    ) -> Result<Cursor> {
        let shape = || SqliteError::Unrepresentable("task continuation shape".into());
        let Some((Value::Text(id), prefix)) = next.seek().split_last() else {
            return Err(shape());
        };
        let mut raw = vec![INLINE, tag(sort).ok_or_else(shape)?];
        raw.extend(now_ms.saturating_add(TTL_MS).to_be_bytes());
        raw.extend(self.binding(project, filter)?);
        for value in prefix {
            let Value::Integer(key) = value else {
                return Err(shape());
            };
            raw.extend(key.to_be_bytes());
        }
        if raw.len() + id.len() <= MAX_RAW {
            raw.extend(id.as_bytes());
        } else {
            raw[0] = REFERENCE;
            let length = u32::try_from(id.len()).map_err(|_| shape())?;
            raw.extend(length.to_be_bytes());
            raw.extend(&Sha256::digest(id.as_bytes())[..DIGEST]);
            // The next row ties with this one on every integer key, or it does not.
            let Some((Value::Text(after), keys)) = following.split_last() else {
                return Err(shape());
            };
            let tied = (keys == prefix).then_some(after.as_str());
            let room = MAX_RAW - raw.len() - 1;
            let fallback = Fallback::choose(id, tied, room).ok_or_else(shape)?;
            fallback.write(&mut raw);
            let room = MAX_RAW - raw.len();
            raw.extend(&id.as_bytes()[..id.floor_char_boundary(room)]);
        }
        Ok(Cursor::new(hex(&raw))?)
    }

    /// Decode and validate a token for this authorized project and request,
    /// resolving an ID carried by reference on `tx`, the page's own snapshot.
    /// Every refusal is `InvalidCursor`: malformed, another query, another
    /// generation, expired and no-longer-found tokens are one class to the caller.
    pub(super) fn resume(
        &self,
        tx: &Transaction<'_>,
        cursor: &Cursor,
        project: &str,
        filter: &Filter,
        sort: Sort,
        now_ms: i64,
    ) -> Result<Continuation> {
        let invalid = || StoreError::InvalidCursor;
        let raw = unhex(cursor.as_str()).ok_or_else(invalid)?;
        if raw.len() < HEADER || ![INLINE, REFERENCE].contains(&raw[0]) {
            return Err(invalid().into());
        }
        let decoded_sort = *SORTS.get(usize::from(raw[1])).ok_or_else(invalid)?;
        let expires = i64::from_be_bytes(raw[2..10].try_into().map_err(|_| invalid())?);
        // The claim is unauthenticated: refuse one that outlives any token this
        // generation could have issued.
        if now_ms > expires || i128::from(expires) - i128::from(now_ms) > i128::from(TTL_MS) {
            return Err(invalid().into());
        }
        let binding = self.binding(project, filter).map_err(|_| invalid())?;
        if raw[10..HEADER] != binding {
            return Err(invalid().into());
        }
        // Before any lookup: another sort's token is refused without reading.
        if decoded_sort != sort {
            return Err(invalid().into());
        }
        let keys = task_page::seek_width(decoded_sort)? - 1;
        let tail = raw.get(HEADER..HEADER + keys * 8).ok_or_else(invalid)?;
        let mut values = tail
            .as_chunks::<8>()
            .0
            .iter()
            .map(|key| Value::Integer(i64::from_be_bytes(*key)))
            .collect::<Vec<_>>();
        let rest = &raw[HEADER + keys * 8..];
        let (bound, id) = if raw[0] == INLINE {
            // One position has one token: an ID that fits is never a reference.
            let id = std::str::from_utf8(rest).map_err(|_| invalid())?;
            (Bound::After, Some(id.to_owned()))
        } else {
            let fits = MAX_RAW - HEADER - keys * 8;
            self.referenced(tx, project, rest, fits)?
        };
        values.extend(id.map(Value::Text));
        // The digest proved the binding of project and filter; the decoded sort
        // is the token's own and the shared validation compares it to the request.
        Ok(Continuation::from_decoded_at(
            project.to_owned(),
            filter.clone(),
            decoded_sort,
            values,
            bound,
            project,
            filter,
            sort,
        )?)
    }

    /// Where `reference` (`length, digest, fallback, prefix`) says to go on in
    /// `project`: after the row it names, or if that row is gone at its fallback,
    /// as the ID to seek from and how. `fits` is how many ID bytes a token with
    /// these keys carries whole. The reference must be one `issue` could have
    /// written: a length that did not fit, and the longest prefix that did. The
    /// scan for a row is bounded to the rows that share a prefix of at least
    /// `fits - 41 - 3` bytes, and holds one ID at a time.
    fn referenced(
        &self,
        tx: &Transaction<'_>,
        project: &str,
        reference: &[u8],
        fits: usize,
    ) -> Result<(Bound, Option<String>)> {
        let invalid = || StoreError::InvalidCursor;
        let (fixed, rest) = reference.split_at_checked(4 + DIGEST).ok_or_else(invalid)?;
        let length = u32::from_be_bytes(fixed[..4].try_into().map_err(|_| invalid())?) as usize;
        let (fallback, prefix) = Fallback::read(rest).ok_or_else(invalid)?;
        let payload = match fallback {
            Fallback::PastKeys => 0,
            Fallback::From { ref tail, .. } => FROM_FIXED + tail.len(),
            Fallback::Successor { .. } => SUCCESSOR_LEN,
        };
        let room = fits
            .checked_sub(REFERENCE_FIXED + payload)
            .ok_or_else(invalid)?;
        let prefix = std::str::from_utf8(prefix).map_err(|_| invalid())?;
        if length <= fits || prefix.len() + BOUNDARY_SLACK < room {
            return Err(invalid().into());
        }
        // Exactly the prefix `issue` cuts, so that one position has one token.
        if prefix.len() > room {
            return Err(invalid().into());
        }
        let issued = |id: &str| id.floor_char_boundary(room) == prefix.len();
        let found = task_page::find_id_with_prefix(tx, project, prefix, length, |id| {
            issued(id) && Sha256::digest(id.as_bytes())[..DIGEST] == fixed[4..]
        })?;
        if let Some(id) = found {
            return Ok((Bound::After, Some(id)));
        }
        match fallback {
            Fallback::PastKeys => Ok((Bound::PastKeys, None)),
            Fallback::From { k, tail } => {
                // The first `k` bytes of the ID are the prefix's; `issue` wrote a
                // prefix that reaches them.
                let head = prefix.as_bytes().get(..k).ok_or_else(invalid)?;
                let id = String::from_utf8([head, &tail].concat()).map_err(|_| invalid())?;
                Ok((Bound::From, Some(id)))
            }
            Fallback::Successor { length, digest } => {
                let length = length as usize;
                // The next row shares the prefix, and is not the row sought.
                let id = task_page::find_id_with_prefix(tx, project, prefix, length, |id| {
                    Sha256::digest(id.as_bytes())[..DIGEST] == digest
                })?
                .ok_or_else(invalid)?;
                Ok((Bound::From, Some(id)))
            }
        }
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
