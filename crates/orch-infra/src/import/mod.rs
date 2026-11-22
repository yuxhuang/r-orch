//! Read-only parsing for the one-way importer. Source confinement, preflight,
//! transactions and reporting belong to the integration layer.
pub mod document;
pub mod event;

#[cfg(test)]
mod tests;

pub mod index;
