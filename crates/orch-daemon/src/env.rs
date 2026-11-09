//! The agent environment snapshot (design D11, spec R21, SE-12).
//!
//! Captured once, at the owner's explicit start, from the starting process's
//! environment; the daemon never calls `set_var`/`remove_var`. Every agent
//! spawn starts from `env_clear()` plus this snapshot, the agent's configured
//! env and the daemon's own routing keys ([`forced`]). Status lists the
//! removed **names**, never values.

use std::ffi::{OsStr, OsString};
use std::path::Path;

/// Names never handed to an agent: identities and routing of whatever
/// started the daemon, web credentials and debugger switches.
pub(crate) const DENYLIST: &[&str] = &[
    "ORCH_AGENT_ID",
    "ORCH_AGENT_NAME",
    "ORCH_TASK_ID",
    "ORCH_TUI_CLIENT",
    "ORCH_WEB_PASSWORD",
    "ORCH_SOCKET",
    "ORCH_PROJECT_ROOT",
    "DEBUG",
    "PWDEBUG",
    crate::detach::DETACHED_CHILD_ENV,
];

/// The sanitized inherited environment and the names it dropped.
#[derive(Debug, Clone, Default)]
pub(crate) struct Snapshot {
    pub(crate) base: Vec<(OsString, OsString)>,
    pub(crate) removed: Vec<String>,
}

pub(crate) fn capture(vars: impl IntoIterator<Item = (OsString, OsString)>) -> Snapshot {
    let mut snapshot = Snapshot::default();
    for (name, value) in vars {
        if denied(&name, &value) {
            let name = name.to_string_lossy().into_owned();
            if !snapshot.removed.contains(&name) {
                snapshot.removed.push(name);
            }
        } else {
            snapshot.base.push((name, value));
        }
    }
    snapshot.removed.sort();
    snapshot
}

fn denied(name: &OsStr, value: &OsStr) -> bool {
    if DENYLIST.iter().any(|denied| name == *denied) {
        return true;
    }
    // NODE_OPTIONS stays unless it opens an inspector.
    name == "NODE_OPTIONS" && value.to_string_lossy().contains("--inspect")
}

/// The routing every agent child receives (D4): the project it belongs to
/// and the endpoint of its daemon, so an inherited shell value cannot
/// redirect it.
pub(crate) fn forced(project_root: &Path, socket: Option<&Path>) -> Vec<(String, String)> {
    let mut forced = vec![(
        orch_proto::discovery::ENV_PROJECT_ROOT.to_owned(),
        project_root.to_string_lossy().into_owned(),
    )];
    if let Some(socket) = socket {
        forced.push((
            orch_proto::discovery::ENV_SOCKET.to_owned(),
            socket.to_string_lossy().into_owned(),
        ));
    }
    forced
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars(pairs: &[(&str, &str)]) -> Vec<(OsString, OsString)> {
        pairs
            .iter()
            .map(|(k, v)| (OsString::from(k), OsString::from(v)))
            .collect()
    }

    #[test]
    fn denylist_removes_identity_routing_and_debug_names_only() {
        let snapshot = capture(vars(&[
            ("PATH", "/bin"),
            ("HOME", "/home/u"),
            ("ORCH_AGENT_ID", "agt_1"),
            ("ORCH_AGENT_NAME", "a"),
            ("ORCH_TASK_ID", "tsk_1"),
            ("ORCH_TUI_CLIENT", "1"),
            ("ORCH_WEB_PASSWORD", "secret-canary"),
            ("ORCH_SOCKET", "/tmp/elsewhere.sock"),
            ("ORCH_PROJECT_ROOT", "/elsewhere"),
            ("DEBUG", "*"),
            ("PWDEBUG", "1"),
            ("NODE_OPTIONS", "--inspect=0.0.0.0:9229"),
            ("ANTHROPIC_API_KEY", "kept-owner-credential"),
        ]));
        let kept: Vec<_> = snapshot
            .base
            .iter()
            .map(|(k, _)| k.to_string_lossy().into_owned())
            .collect();
        assert_eq!(kept, ["PATH", "HOME", "ANTHROPIC_API_KEY"]);
        assert_eq!(
            snapshot.removed,
            [
                "DEBUG",
                "NODE_OPTIONS",
                "ORCH_AGENT_ID",
                "ORCH_AGENT_NAME",
                "ORCH_PROJECT_ROOT",
                "ORCH_SOCKET",
                "ORCH_TASK_ID",
                "ORCH_TUI_CLIENT",
                "ORCH_WEB_PASSWORD",
                "PWDEBUG",
            ]
        );
        // Names only: no value leaks into what status reports.
        assert!(!snapshot.removed.iter().any(|n| n.contains("canary")));
    }

    #[test]
    fn node_options_without_an_inspector_is_kept() {
        let snapshot = capture(vars(&[("NODE_OPTIONS", "--max-old-space-size=4096")]));
        assert_eq!(snapshot.base.len(), 1);
        assert!(snapshot.removed.is_empty());
    }

    #[test]
    fn forced_routing_names_the_project_and_socket() {
        let forced = forced(
            Path::new("/p"),
            Some(Path::new("/p/.orchestry/run/orch.sock")),
        );
        assert_eq!(
            forced,
            [
                ("ORCH_PROJECT_ROOT".to_owned(), "/p".to_owned()),
                (
                    "ORCH_SOCKET".to_owned(),
                    "/p/.orchestry/run/orch.sock".to_owned()
                ),
            ]
        );
    }
}
