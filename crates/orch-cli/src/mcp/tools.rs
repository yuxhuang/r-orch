//! The tool groups. Each leaf owns its tools' argument types and handlers and
//! exports only `register`; this file only declares and calls them.

mod agent;
mod config;
mod context;
mod goal;
mod logs;
mod msg;
mod remote;
mod task;
mod team;

use super::registry::Registry;

/// Register every group's tools, in a fixed order.
pub(crate) fn register_all(registry: &mut Registry) -> orch_core::Result<()> {
    task::register(registry)?;
    goal::register(registry)?;
    msg::register(registry)?;
    context::register(registry)?;
    agent::register(registry)?;
    team::register(registry)?;
    logs::register(registry)?;
    config::register(registry)?;
    remote::register(registry)?;
    Ok(())
}
