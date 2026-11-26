//! The inline token: its binding, its expiry and its tuple.
//!
//! A token is lowercase hex of `version, sort tag, expiry (i64 ms, BE), a
//! 16-byte binding digest, the components of the tuple`: an integer as 8 bytes
//! (i64, BE), a text before the identity as one length byte and its bytes, the
//! identity as the rest. It holds no snapshot and asks nothing of the adapter.
//!
//! The digest covers the project, the daemon generation, the family and the
//! filter. The sort tag is compared with the request's sort, so a token is
//! refused for any other query, project or generation, and after its expiry
//! (R10, the owner's rule of 2026-11-26). Like the Task token it is not secret
//! and has no MAC: a forged one can only start a walk at another position of
//! the forger's own project, filter and sort, because the tuple is validated
//! against the request's layout and the page binds each value. Its expiry
//! claim is bounded on both sides.
use super::super::{Result, SqliteError};
use super::layout::{Arm, Component, HEADER, INT_BYTES, Kind, Layout, MAX_RAW, Tuple};
use orch_core::store::{Cursor, Filter, Sort, StoreError};
use sha2::{Digest, Sha256};

/// How long an inline token resumes a walk: 24 hours of the wall clock.
pub(in crate::sqlite) const INLINE_TTL_MS: i64 = 24 * 60 * 60 * 1000;
/// How far a clock may have been set back since issue: 5 minutes. A token is
/// refused as future-dated only when `expires - now` is more than the TTL plus this.
pub(in crate::sqlite) const INLINE_SKEW_MS: i64 = 5 * 60 * 1000;

/// The version byte of an inline token. 2 and 3 are reserved for the retained
/// token and the token by lookup of step 4; every other value is refused.
const INLINE: u8 = 1;
const DIGEST: usize = 16;

const SORTS: [Sort; 8] = [
    Sort::Default,
    Sort::Id,
    Sort::Position,
    Sort::Name,
    Sort::Priority,
    Sort::CreatedAt,
    Sort::UpdatedAt,
    Sort::Attempt,
];

/// The generation that issued a token. A restore and every start take a new one
/// (R41, R77), so a token from before is refused as stale.
pub(in crate::sqlite) struct NonTaskCursors {
    generation: String,
}

impl NonTaskCursors {
    pub(in crate::sqlite) fn new(generation: impl Into<String>) -> Self {
        Self {
            generation: generation.into(),
        }
    }

    /// The byte that says the sort. `None` for a sort no family offers.
    pub(in crate::sqlite) fn sort_tag(sort: Sort) -> Option<u8> {
        SORTS.iter().position(|s| *s == sort).map(|i| i as u8)
    }

    /// The digest of the project, this generation, the family and the filter,
    /// under a domain string of its own. Every field is length-prefixed, so
    /// none runs into another. `Err(InvalidCursor)` for a Task filter.
    pub(in crate::sqlite) fn binding(
        &self,
        project: &str,
        filter: &Filter,
    ) -> Result<[u8; DIGEST]> {
        let mut hash = Sha256::new();
        hash.update(b"orch non-task cursor v1");
        let field = |hash: &mut Sha256, tag: u8, bytes: Option<&[u8]>| {
            hash.update([tag, u8::from(bytes.is_some())]);
            let bytes = bytes.unwrap_or_default();
            hash.update((bytes.len() as u64).to_be_bytes());
            hash.update(bytes);
        };
        field(&mut hash, 1, Some(project.as_bytes()));
        field(&mut hash, 2, Some(self.generation.as_bytes()));
        // The family is the filter's variant: `Messages` and `PendingMessages`
        // are two filters of one family and are told apart by the variant.
        let (family, a, b): (u8, Option<&[u8]>, Option<&[u8]>) = match filter {
            Filter::Tasks { .. } => return Err(StoreError::InvalidCursor.into()),
            Filter::Agents => (1, None, None),
            Filter::Teams => (2, None, None),
            Filter::Runs { task, agent } => (
                3,
                task.as_deref().map(str::as_bytes),
                agent.as_deref().map(str::as_bytes),
            ),
            Filter::Messages { recipient } => (4, recipient.as_deref().map(str::as_bytes), None),
            Filter::PendingMessages { recipient, now } => {
                (5, Some(recipient.as_bytes()), Some(now.as_bytes()))
            }
            Filter::Context => (6, None, None),
            Filter::Config => (7, None, None),
            Filter::Templates => (8, None, None),
            Filter::Goals { status } => (9, status.map(|s| s.as_str().as_bytes()), None),
        };
        field(&mut hash, 3, Some(&[family]));
        field(&mut hash, 4, a);
        field(&mut hash, 5, b);
        Ok(finish(hash))
    }

    /// The inline token for `anchor`, the last row of a page that has more rows
    /// after it, or `None` when its texts do not fit the capacity of its layout
    /// (the retained and lookup arms of step 4). It expires 24 hours after
    /// `now_ms`, the wall clock.
    pub(in crate::sqlite) fn issue_inline(
        &self,
        project: &str,
        filter: &Filter,
        sort: Sort,
        anchor: &Tuple,
        now_ms: i64,
    ) -> Result<Option<Cursor>> {
        let shape = || SqliteError::Unrepresentable("non-task cursor family or sort".into());
        let layout = Layout::of(filter, sort).ok_or_else(shape)?;
        if layout.arm(anchor) != Arm::Inline {
            return Ok(None);
        }
        let raw = self.encode(
            project,
            filter,
            sort,
            &layout,
            anchor,
            now_ms.saturating_add(INLINE_TTL_MS),
        )?;
        debug_assert!(
            raw.len() <= MAX_RAW,
            "the capacity table and the encoder agree"
        );
        Ok(Some(Cursor::new(hex(&raw))?))
    }

    /// The raw token with an explicit expiry. `issue_inline` is the one caller
    /// outside tests, which forge an expiry as an attacker could.
    pub(super) fn encode(
        &self,
        project: &str,
        filter: &Filter,
        sort: Sort,
        layout: &Layout,
        anchor: &Tuple,
        expires_ms: i64,
    ) -> Result<Vec<u8>> {
        let shape = || SqliteError::Unrepresentable("non-task cursor family or sort".into());
        let mut raw = vec![INLINE, Self::sort_tag(sort).ok_or_else(shape)?];
        raw.extend(expires_ms.to_be_bytes());
        raw.extend(self.binding(project, filter)?);
        let last = layout.parts().len() - 1;
        for (index, value) in anchor.components().iter().enumerate() {
            match value {
                Component::Int(v) => raw.extend(v.to_be_bytes()),
                Component::Text(text) if index == last => raw.extend(text.as_bytes()),
                Component::Text(text) => {
                    raw.push(u8::try_from(text.len()).map_err(|_| shape())?);
                    raw.extend(text.as_bytes());
                }
            }
        }
        Ok(raw)
    }

    /// The tuple an inline token holds, for this request. Every refusal is
    /// `InvalidCursor`: malformed, another query, project, generation or sort,
    /// expired or future-dated, and a version this module does not read.
    /// Decided from the token alone; nothing is read.
    pub(in crate::sqlite) fn resume_inline(
        &self,
        cursor: &Cursor,
        project: &str,
        filter: &Filter,
        sort: Sort,
        now_ms: i64,
    ) -> Result<Tuple> {
        let invalid = || SqliteError::from(StoreError::InvalidCursor);
        let raw = unhex(cursor.as_str()).ok_or_else(invalid)?;
        if raw.len() < HEADER || raw[0] != INLINE {
            return Err(invalid());
        }
        if Some(raw[1]) != Self::sort_tag(sort) {
            return Err(invalid());
        }
        let expires = i64::from_be_bytes(raw[2..10].try_into().map_err(|_| invalid())?);
        // `now > expires` expires it. The claim is unauthenticated, so one that
        // outlives any token this generation could have issued, allowing for a
        // clock set back, is refused. Equality passes both checks.
        let ahead = i128::from(expires) - i128::from(now_ms);
        if ahead < 0 || ahead > i128::from(INLINE_TTL_MS + INLINE_SKEW_MS) {
            return Err(invalid());
        }
        if raw[10..HEADER] != self.binding(project, filter).map_err(|_| invalid())? {
            return Err(invalid());
        }
        let layout = Layout::of(filter, sort).ok_or_else(invalid)?;
        let mut rest = &raw[HEADER..];
        let last = layout.parts().len() - 1;
        let mut values = Vec::with_capacity(layout.parts().len());
        for (index, part) in layout.parts().iter().enumerate() {
            if part.kind != Kind::Text {
                let (int, tail) = rest.split_at_checked(INT_BYTES).ok_or_else(invalid)?;
                values.push(Component::Int(i64::from_be_bytes(
                    int.try_into().map_err(|_| invalid())?,
                )));
                rest = tail;
                continue;
            }
            let (bytes, tail) = if index == last {
                (rest, &rest[rest.len()..])
            } else {
                let (length, tail) = rest.split_first().ok_or_else(invalid)?;
                tail.split_at_checked(usize::from(*length))
                    .ok_or_else(invalid)?
            };
            let text = std::str::from_utf8(bytes).map_err(|_| invalid())?;
            values.push(Component::Text(text.to_owned()));
            rest = tail;
        }
        Tuple::new(&layout, values).ok_or_else(invalid)
    }
}

fn finish(hash: Sha256) -> [u8; DIGEST] {
    let mut digest = [0; DIGEST];
    digest.copy_from_slice(&hash.finalize()[..DIGEST]);
    digest
}

pub(super) fn hex(bytes: &[u8]) -> String {
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
