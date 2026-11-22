//! Read-only parsing for the one-way importer. Source confinement, preflight,
//! transactions and reporting belong to the integration layer.
pub(crate) mod document;
pub(crate) mod event;

#[cfg(test)]
mod tests;

pub(crate) mod index;
