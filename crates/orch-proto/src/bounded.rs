//! Length-bounded wire values (spec R20). The bound is part of the type, so a
//! request DTO cannot be decoded with an oversized string or list: the error is
//! raised by serde before any handler sees the value.
//!
//! Frames are already capped at [`crate::frame::MAX_FRAME_BYTES`], so the
//! transient allocation before the check is bounded too.

use serde::de::{self, Deserializer};
use serde::{Deserialize, Serialize, Serializer};
use std::fmt;
use std::ops::Deref;

/// An identifier such as `tsk_…`, `agt_…`, an alias or a key.
pub const MAX_ID_BYTES: usize = 256;
/// A title, name, subject or other one-line field.
pub const MAX_SHORT_TEXT_BYTES: usize = 4 * 1024;
/// A description, message body, role or context value. Larger inputs go
/// through a transfer (design D6) and are checked again by the service.
pub const MAX_TEXT_BYTES: usize = 256 * 1024;
/// A list of ids, labels, scopes or paths.
pub const MAX_LIST_ITEMS: usize = 1024;

/// A UTF-8 string of at most `MAX` bytes.
#[derive(Clone, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BoundedString<const MAX: usize>(String);

/// One-line identifiers: ids, aliases, keys, adapter names.
pub type Id = BoundedString<MAX_ID_BYTES>;
/// Titles, names, subjects.
pub type ShortText = BoundedString<MAX_SHORT_TEXT_BYTES>;
/// Descriptions, bodies, values.
pub type Text = BoundedString<MAX_TEXT_BYTES>;

/// A string longer than its type's bound.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("value is {len} bytes, more than the {max}-byte limit")]
pub struct TooLong {
    pub len: usize,
    pub max: usize,
}

impl<const MAX: usize> BoundedString<MAX> {
    pub fn new(value: impl Into<String>) -> Result<Self, TooLong> {
        let value = value.into();
        if value.len() > MAX {
            return Err(TooLong {
                len: value.len(),
                max: MAX,
            });
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_inner(self) -> String {
        self.0
    }
}

impl<const MAX: usize> Deref for BoundedString<MAX> {
    type Target = str;
    fn deref(&self) -> &str {
        &self.0
    }
}

impl<const MAX: usize> fmt::Debug for BoundedString<MAX> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.0, f)
    }
}

impl<const MAX: usize> TryFrom<String> for BoundedString<MAX> {
    type Error = TooLong;
    fn try_from(value: String) -> Result<Self, TooLong> {
        Self::new(value)
    }
}

impl<const MAX: usize> TryFrom<&str> for BoundedString<MAX> {
    type Error = TooLong;
    fn try_from(value: &str) -> Result<Self, TooLong> {
        Self::new(value)
    }
}

impl<const MAX: usize> Serialize for BoundedString<MAX> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de, const MAX: usize> Deserialize<'de> for BoundedString<MAX> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(de::Error::custom)
    }
}

/// A list of at most `MAX` items.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct BoundedVec<T, const MAX: usize = MAX_LIST_ITEMS>(Vec<T>);

impl<T, const MAX: usize> Default for BoundedVec<T, MAX> {
    fn default() -> Self {
        Self(Vec::new())
    }
}

/// A list with more items than its type's bound.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("list has {len} items, more than the {max}-item limit")]
pub struct TooMany {
    pub len: usize,
    pub max: usize,
}

impl<T, const MAX: usize> BoundedVec<T, MAX> {
    pub fn new(items: Vec<T>) -> Result<Self, TooMany> {
        if items.len() > MAX {
            return Err(TooMany {
                len: items.len(),
                max: MAX,
            });
        }
        Ok(Self(items))
    }

    pub fn into_inner(self) -> Vec<T> {
        self.0
    }
}

impl<T, const MAX: usize> Deref for BoundedVec<T, MAX> {
    type Target = [T];
    fn deref(&self) -> &[T] {
        &self.0
    }
}

impl<T: fmt::Debug, const MAX: usize> fmt::Debug for BoundedVec<T, MAX> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.0, f)
    }
}

impl<T: Serialize, const MAX: usize> Serialize for BoundedVec<T, MAX> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.0.serialize(serializer)
    }
}

impl<'de, T: Deserialize<'de>, const MAX: usize> Deserialize<'de> for BoundedVec<T, MAX> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visitor<T, const MAX: usize>(std::marker::PhantomData<T>);
        impl<'de, T: Deserialize<'de>, const MAX: usize> de::Visitor<'de> for Visitor<T, MAX> {
            type Value = BoundedVec<T, MAX>;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "a list of at most {MAX} items")
            }
            // Stop at MAX + 1 instead of collecting an unbounded list first.
            fn visit_seq<A: de::SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                let mut items = Vec::new();
                while let Some(item) = seq.next_element()? {
                    if items.len() == MAX {
                        return Err(de::Error::custom(format!(
                            "list has more than the {MAX}-item limit"
                        )));
                    }
                    items.push(item);
                }
                Ok(BoundedVec(items))
            }
        }
        deserializer.deserialize_seq(Visitor::<T, MAX>(std::marker::PhantomData))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strings_over_the_bound_do_not_decode() {
        assert!(serde_json::from_str::<BoundedString<3>>("\"abc\"").is_ok());
        assert!(serde_json::from_str::<BoundedString<3>>("\"abcd\"").is_err());
        // Bytes, not chars: "é" is two bytes.
        assert!(serde_json::from_str::<BoundedString<3>>("\"éé\"").is_err());
    }

    #[test]
    fn lists_over_the_bound_do_not_decode() {
        assert!(serde_json::from_str::<BoundedVec<u8, 2>>("[1,2]").is_ok());
        assert!(serde_json::from_str::<BoundedVec<u8, 2>>("[1,2,3]").is_err());
    }
}
