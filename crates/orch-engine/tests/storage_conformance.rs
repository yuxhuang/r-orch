//! File-backend registration. Add a second registration with a factory returning
//! `Stores`; the contract scenarios have no file-backend imports or disk access.

#[path = "storage_contract/mod.rs"]
mod contract;
#[path = "storage_contract/file.rs"]
mod file;

contract::storage_suite!(file_backend, crate::file::backend);
