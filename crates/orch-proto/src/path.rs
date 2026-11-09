//! Lossless filesystem paths on the wire (design D4: "raw paths use a lossless
//! tagged-byte encoding; diagnostics escape controls").
//!
//! Unix paths are bytes, not UTF-8. A path is sent as `{"utf8":"…"}` when its
//! bytes are valid UTF-8 and as `{"hex":"…"}` (lowercase hex of the raw bytes)
//! otherwise, so a non-UTF-8 project root survives the round trip unchanged.
//! Paths are for display, hashing and dialing only; project identity is the
//! `(dev, ino)` pair in [`crate::handshake::ProjectId`].

use serde::{Deserialize, Serialize};
use std::ffi::OsString;
use std::fmt;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};

/// Longest path accepted on the wire, in raw bytes (Linux `PATH_MAX`).
pub const MAX_PATH_BYTES: usize = 4096;

#[derive(Clone, PartialEq, Eq, Hash)]
pub struct WirePath(PathBuf);

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
enum Tagged {
    Utf8(String),
    Hex(String),
}

/// A path longer than [`MAX_PATH_BYTES`], or one containing a NUL byte.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PathError {
    #[error("path is {0} bytes, more than the {MAX_PATH_BYTES}-byte limit")]
    TooLong(usize),
    #[error("path contains a NUL byte")]
    Nul,
    #[error("path hex encoding is invalid")]
    BadHex,
}

impl WirePath {
    pub fn new(path: impl Into<PathBuf>) -> Result<Self, PathError> {
        let path = path.into();
        let bytes = path.as_os_str().as_bytes();
        if bytes.len() > MAX_PATH_BYTES {
            return Err(PathError::TooLong(bytes.len()));
        }
        if bytes.contains(&0) {
            return Err(PathError::Nul);
        }
        Ok(Self(path))
    }

    pub fn as_path(&self) -> &Path {
        &self.0
    }

    pub fn into_path_buf(self) -> PathBuf {
        self.0
    }

    /// The path for a human: lossy UTF-8 with control characters escaped, so
    /// a crafted directory name cannot drive the terminal (SE-17).
    pub fn display_escaped(&self) -> String {
        escape_controls(&self.0.to_string_lossy())
    }
}

/// Escape C0/C1 control characters (including ESC and newline) as `\u{..}`.
pub fn escape_controls(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if c.is_control() {
            out.extend(c.escape_unicode());
        } else {
            out.push(c);
        }
    }
    out
}

impl fmt::Debug for WirePath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.0, f)
    }
}

impl fmt::Display for WirePath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.display_escaped())
    }
}

impl Serialize for WirePath {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let bytes = self.0.as_os_str().as_bytes();
        match std::str::from_utf8(bytes) {
            Ok(text) => Tagged::Utf8(text.to_owned()),
            Err(_) => Tagged::Hex(bytes.iter().map(|b| format!("{b:02x}")).collect()),
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for WirePath {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use serde::de::Error;
        let bytes = match Tagged::deserialize(deserializer)? {
            Tagged::Utf8(text) => text.into_bytes(),
            Tagged::Hex(hex) => {
                decode_hex(&hex).ok_or_else(|| D::Error::custom(PathError::BadHex))?
            }
        };
        WirePath::new(OsString::from_vec(bytes)).map_err(D::Error::custom)
    }
}

fn decode_hex(hex: &str) -> Option<Vec<u8>> {
    if !hex.len().is_multiple_of(2) || hex.len() > MAX_PATH_BYTES * 2 {
        return None;
    }
    hex.as_bytes()
        .chunks(2)
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf8_and_raw_paths_round_trip() {
        let utf8 = WirePath::new("/tmp/proj").unwrap();
        let json = serde_json::to_string(&utf8).unwrap();
        assert_eq!(json, r#"{"utf8":"/tmp/proj"}"#);
        assert_eq!(serde_json::from_str::<WirePath>(&json).unwrap(), utf8);

        let raw = WirePath::new(OsString::from_vec(b"/tmp/\xff".to_vec())).unwrap();
        let json = serde_json::to_string(&raw).unwrap();
        assert_eq!(json, r#"{"hex":"2f746d702fff"}"#);
        assert_eq!(serde_json::from_str::<WirePath>(&json).unwrap(), raw);
    }

    #[test]
    fn nul_bad_hex_and_unknown_tags_are_rejected() {
        assert!(serde_json::from_str::<WirePath>(r#"{"hex":"2f00"}"#).is_err());
        assert!(serde_json::from_str::<WirePath>(r#"{"hex":"2"}"#).is_err());
        assert!(serde_json::from_str::<WirePath>(r#"{"raw":"x"}"#).is_err());
    }

    #[test]
    fn display_escapes_terminal_controls() {
        let path = WirePath::new("/tmp/\u{1b}[31mx").unwrap();
        assert_eq!(path.display_escaped(), "/tmp/\\u{1b}[31mx");
    }
}
