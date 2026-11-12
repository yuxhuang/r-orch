//! Remote snapshots as the daemon's peer runtime records them; these tools
//! never connect to a peer or open a listener.
use crate::mcp::registry::{
    CallContext, Class, EmptyArgs, Execution, Registry, ToolMeta, ToolResult, json,
};
use orch_core::Result;
use orch_proto::ops::args as rpc;
use std::sync::Arc;

fn meta(name: &'static str, description: &'static str) -> ToolMeta {
    ToolMeta {
        name,
        description,
        class: Class::Read,
        execution: Execution::Store,
        destructive_hint: false,
        idempotent_hint: true,
        open_world_hint: false,
    }
}
pub(crate) fn register(registry: &mut Registry) -> Result<()> {
    registry.add(
        meta(
            "remote_list",
            "List persisted remote definitions (orch remote list).",
        ),
        list,
    )?;
    registry.add(meta("remote_status", "Read stored remote and live-owner listener snapshots (orch remote status); does not connect or listen."), status)?;
    Ok(())
}
async fn list(cx: Arc<CallContext>, _: EmptyArgs) -> ToolResult {
    json(&cx.request(rpc::RemoteListArgs {}).await?, "remotes")
}
async fn status(cx: Arc<CallContext>, _: EmptyArgs) -> ToolResult {
    json(
        &cx.request(rpc::RemoteStatusArgs {}).await?,
        "remote status",
    )
}

#[cfg(test)]
#[path = "remote_tests.rs"]
mod tests;
