//! The one-way importer: whole-source preflight, confined verified second reads,
//! and typed inputs for native unfinished batches. Completion remains unimplemented.
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

/// One bounded, checked source document and its native mapping input.
/// The caller revalidates descriptor-relative bytes before constructing this row.
#[derive(Debug, Clone)]
pub struct ImportRow {
    pub entity: orch_core::store::Entity,
    pub source_sha256: String,
}

/// Immutable input retained until the writer's durability receipt has been drained.
/// Source is the canonical project directory, never a destination filename.
#[derive(Debug, Clone)]
pub struct ImportBatch {
    pub source: String,
    pub rows: Vec<ImportRow>,
}

/// Checked source hashes and, for indexed families, their dense source-order rank.
/// A resume may append after retained rows, but may not silently reorder them.
pub type ImportInventory =
    std::collections::BTreeMap<(orch_core::store::EntityKind, String), (String, Option<u64>)>;
