//! WP-2 — runtime model discovery for the adapter CLIs.

pub mod discovery;
pub mod parsers;

pub use discovery::{
    CommandOutput, DiscoveryRuntime, ModelCatalog, ModelDiscovery, ModelOption,
    SnapshotDiscoveryRuntime, SystemDiscoveryRuntime, fallback_model_options, fallback_model_table,
};
