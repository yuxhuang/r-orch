//! File-backend registration. Add a second registration with a factory returning
//! `Stores`; the contract scenarios have no file-backend imports or disk access.

#[path = "storage_contract/mod.rs"]
mod contract;
#[path = "storage_contract/file.rs"]
mod file;

contract::storage_suite!(file_backend, crate::file::backend);

#[path = "storage_contract/v2.rs"]
mod v2;
v2::entity_suite!(memory_v2, crate::v2::memory_backend);

#[path = "storage_contract/v2_writer.rs"]
mod v2_writer;
v2::entity_suite!(queued_memory_v2, crate::v2_writer::backend);

#[path = "storage_contract/v2_file.rs"]
mod v2_file;
v2::entity_suite!(file_v2, crate::v2_file::backend);

#[path = "storage_contract/events_v2.rs"]
mod events_v2;
events_v2::event_suite!(memory_events_v2, crate::events_v2::memory_backend);
events_v2::event_suite!(file_events_v2, crate::v2_file::events_backend);

#[path = "storage_contract/v2_operations.rs"]
mod v2_operations;
v2_operations::operational_suite!(memory_operations_v2, crate::v2_operations::memory_backend);
v2_operations::operational_suite!(file_operations_v2, crate::v2_file::operations_backend);

#[path = "storage_contract/leases_v2.rs"]
mod leases_v2;
leases_v2::lease_suite!(memory_leases_v2, crate::v2_operations::memory_backend);
leases_v2::lease_suite!(file_leases_v2, crate::v2_file::operations_backend);

#[path = "storage_contract/intents_v2.rs"]
mod intents_v2;
intents_v2::intent_suite!(memory_intents_v2, crate::v2_operations::memory_backend);
intents_v2::intent_suite!(file_intents_v2, crate::v2_file::operations_backend);

#[path = "storage_contract/request_keys_v2.rs"]
mod request_keys_v2;
request_keys_v2::request_key_suite!(memory_request_keys_v2, crate::v2_operations::memory_backend);
request_keys_v2::request_key_suite!(file_request_keys_v2, crate::v2_file::operations_backend);

#[path = "storage_contract/retries_v2.rs"]
mod retries_v2;
retries_v2::retry_suite!(memory_retries_v2, crate::v2_operations::memory_backend);
retries_v2::retry_suite!(file_retries_v2, crate::v2_file::operations_backend);
