//! The one-way importer. Today: read-only parsing and the whole-source `preflight`.
//! Transactions, blobs and the command belong to the slices that follow.
pub(crate) mod document;
pub(crate) mod event;
pub mod preflight;

#[cfg(test)]
mod tests;

#[allow(
    dead_code,
    reason = "Index reconciliation awaits the #146 config and index wiring"
)]
pub(crate) mod index;
