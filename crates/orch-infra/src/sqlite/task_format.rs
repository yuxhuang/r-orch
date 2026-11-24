//! Task format v1 building blocks. The task adapter owns SQL reachability and
//! operation savepoints; publishing an object never installs a database reference.
pub(super) mod text;

pub(super) mod codec;
