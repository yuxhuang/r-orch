//! Content-addressed blob storage on disk (R58, R59, R97, R101, R102).
//!
//! A blob is named by the SHA-256 of its bytes and lives at `blobs/sha256/ab/cd/<hash>` below the data
//! directory. Every file operation goes through the confined-directory module of [`crate::data_dir`]: a
//! hash column read from a row is parsed to a [`BlobHash`] before it selects a file, and no path string is
//! ever built here.

mod references;
mod store;

use sha2::{Digest, Sha256};
use std::fmt;
use std::str::FromStr;

pub use references::{BlobReferences, ReferenceError};
pub use store::{BlobError, DiskBlobStore};

/// Text up to this many bytes may live in a column (ADR 0017 decision 11, R97); more goes to the blob store.
pub const INLINE_TEXT_MAX_BYTES: usize = 64 * 1024;

/// The largest blob this adapter writes or reads (an import attachment is at most 64 MiB).
pub const MAX_BLOB_BYTES: u64 = 64 * 1024 * 1024;

/// Where a value belongs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Placement {
    /// In the row: UTF-8 text of at most [`INLINE_TEXT_MAX_BYTES`] bytes.
    Inline,
    /// Behind the blob store, the row holding the hash.
    Blob,
}

/// Decides by bytes, not characters: exactly 65,536 bytes of text stay inline, one more byte goes to a
/// blob, and bytes that are not UTF-8 text go to a blob whatever their size.
pub fn placement(bytes: &[u8]) -> Placement {
    if bytes.len() <= INLINE_TEXT_MAX_BYTES && std::str::from_utf8(bytes).is_ok() {
        Placement::Inline
    } else {
        Placement::Blob
    }
}

/// A parsed SHA-256: the only value that may select a blob file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BlobHash([u8; 32]);

/// A value that is not 64 lower-case hex digits.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("not a blob hash: {0}")]
pub struct InvalidHash(String);

impl BlobHash {
    pub fn of(bytes: &[u8]) -> Self {
        Self(Sha256::digest(bytes).into())
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    pub(crate) fn from_digest(digest: [u8; 32]) -> Self {
        Self(digest)
    }
}

impl FromStr for BlobHash {
    type Err = InvalidHash;

    /// Exactly 64 lower-case hex digits; upper case, another length and any other character are refused.
    fn from_str(text: &str) -> Result<Self, InvalidHash> {
        let bytes = text.as_bytes();
        if bytes.len() != 64 {
            return Err(InvalidHash(format!("{} bytes, not 64", bytes.len())));
        }
        let mut out = [0u8; 32];
        for (i, pair) in bytes.as_chunks::<2>().0.iter().enumerate() {
            let digit = |b: u8| match b {
                b'0'..=b'9' => Some(b - b'0'),
                b'a'..=b'f' => Some(b - b'a' + 10),
                _ => None,
            };
            match (digit(pair[0]), digit(pair[1])) {
                (Some(hi), Some(lo)) => out[i] = hi << 4 | lo,
                _ => return Err(InvalidHash("not lower-case hex".into())),
            }
        }
        Ok(Self(out))
    }
}

impl fmt::Display for BlobHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.iter().try_for_each(|b| write!(f, "{b:02x}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_round_trips_lower_case_hex() {
        let hash = BlobHash::of(b"abc");
        assert_eq!(
            hash.to_string(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(hash.to_string().parse::<BlobHash>().unwrap(), hash);
    }

    #[test]
    fn parse_refuses_everything_but_64_lower_case_hex_digits() {
        let good = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        let upper = good.to_uppercase();
        let g = good.replace('b', "g");
        let slash = format!("../{}", &good[3..]);
        let nul = format!("{}\0", &good[..63]);
        let multibyte = format!("{}é", &good[..62]);
        for bad in [
            "",
            "../x",
            &good[..63],
            &format!("{good}0"),
            &upper,
            &g,
            &slash,
            &nul,
            &multibyte,
            " ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015a",
        ] {
            assert!(bad.parse::<BlobHash>().is_err(), "{bad:?}");
        }
    }

    #[test]
    fn placement_threshold_is_in_bytes_at_64_kib() {
        assert_eq!(
            placement(&vec![b'a'; INLINE_TEXT_MAX_BYTES - 1]),
            Placement::Inline
        );
        assert_eq!(
            placement(&vec![b'a'; INLINE_TEXT_MAX_BYTES]),
            Placement::Inline
        );
        assert_eq!(
            placement(&vec![b'a'; INLINE_TEXT_MAX_BYTES + 1]),
            Placement::Blob
        );
        assert_eq!(placement(b""), Placement::Inline);
        // 32,768 two-byte characters are exactly 65,536 bytes; 32,769 are over.
        assert_eq!(placement("é".repeat(32_768).as_bytes()), Placement::Inline);
        assert_eq!(placement("é".repeat(32_769).as_bytes()), Placement::Blob);
    }

    #[test]
    fn non_text_bytes_go_to_a_blob_whatever_their_size() {
        assert_eq!(placement(&[0xff, 0xfe]), Placement::Blob);
        assert_eq!(placement(b"ok\xc3"), Placement::Blob);
    }
}
