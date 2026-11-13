//! `orch mcp`: a stdio MCP bridge to the project's daemon.
//!
//! The bridge resolves its project, identity and mode before anything
//! reaches stdout, so a bad flag or a missing project is a CLI error on
//! stderr with the CLI's exit code (3 for no project). It does not need a
//! daemon to start: a client's `initialize` fails with the uniform no-daemon
//! message until the owner starts one (`orch daemon start`), and nothing here
//! ever starts one. After startup stdout belongs to the protocol.

pub(crate) mod identity;
pub(crate) mod mode;
pub(crate) mod projections;
pub(crate) mod registry;
pub(crate) mod resources;
pub(crate) mod server;
pub(crate) mod system;
#[cfg(test)]
pub(crate) mod testing;
mod tools;

use std::path::Path;
use std::process::ExitCode;
use std::time::Duration;

use orch_client::{ClientConfig, ClientError};
use orch_core::error::{Error, Result};
use orch_proto::handshake::CallerKind;
use tokio_util::sync::CancellationToken;

use crate::output::{Styles, print_error};
use crate::tree::McpCmd;
use identity::Identity;
use mode::Mode;
use registry::Registry;

/// The environment variable a spawned worker's identity arrives in.
pub(crate) const AGENT_ENV: &str = "ORCH_AGENT_ID";

/// How long the runtime may take to stop after the service has finished.
/// Tokio's stdin read cannot be cancelled, so an unbounded shutdown would
/// hang whenever the client keeps stdin open after a signal or after the
/// bridge has lost its stdout.
const RUNTIME_SHUTDOWN: Duration = Duration::from_millis(250);

/// Run `orch mcp`. Owns its runtime so the bounded shutdown applies to this
/// command alone.
pub(crate) fn run(args: &McpCmd, styles: &Styles) -> ExitCode {
    let report = |err: &Error| {
        print_error(styles, &err.to_string(), err.hint().as_deref());
        ExitCode::from(u8::try_from(err.exit_code()).unwrap_or(1))
    };

    // Read once: the identity never changes for the life of the bridge.
    let env_agent = std::env::var(AGENT_ENV).ok();
    let identity = match Identity::resolve(args.agent.as_deref(), env_agent.as_deref()) {
        Ok(identity) => identity,
        Err(err) => return report(&err),
    };
    let mode = Mode::from_flags(args.read_only, args.allow_destructive);
    let cwd = match std::env::current_dir() {
        Ok(cwd) => cwd,
        Err(err) => {
            return report(&Error::other(format!(
                "cannot read the working directory: {err}"
            )));
        }
    };
    let (settings, config) = match launch(&cwd, args.project_root.as_deref(), identity, mode) {
        Ok(launch) => launch,
        Err(err) => {
            let agent_context =
                std::env::var_os("ORCH_TASK_ID").is_some() || std::env::var_os(AGENT_ENV).is_some();
            eprintln!("{}", err.render(agent_context));
            return ExitCode::from(u8::try_from(err.exit_code()).unwrap_or(1));
        }
    };

    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(err) => {
            return report(&Error::other(format!(
                "failed to start the async runtime: {err}"
            )));
        }
    };
    let outcome = runtime.block_on(serve(settings, config));
    runtime.shutdown_timeout(RUNTIME_SHUTDOWN);
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => report(&err),
    }
}

/// The bridge's fixed settings and the client configuration every call's
/// session uses.
///
/// The project is found as every client finds it — the walk up from the
/// working directory, or with `--project-root` exactly that directory,
/// resolved against the working directory and required to hold its **own**
/// `.orchestry` (never a parent's) — and pinned, so a later call cannot
/// drift to another project. The session is an MCP session in the launch
/// mode, attributed to the resolved identity; it renders failures in the
/// agent variant when an identity was resolved or the process runs inside an
/// agent task (design D5). Only discovery runs here: no daemon is contacted.
pub(crate) fn launch(
    cwd: &Path,
    project_root: Option<&Path>,
    identity: Identity,
    mode: Mode,
) -> std::result::Result<(server::Settings, ClientConfig), ClientError> {
    let mut config = ClientConfig::from_env(cwd.to_path_buf())?;
    if let Some(requested) = project_root {
        config.project_root = Some(cwd.join(requested));
    }
    let root = config.project()?.root;
    config.project_root = Some(root.clone());
    config.caller.agent_id = identity
        .default_agent()
        .map(orch_proto::bounded::Id::new)
        .transpose()
        .map_err(|err| ClientError::InvalidArguments(format!("--agent: {err}")))?;
    // An MCP session with an identity renders the agent variant.
    let config = config
        .with_frontend(CallerKind::Mcp)
        .with_mode(mode.session());
    let settings = server::Settings {
        project_root: root,
        agent_context: config.agent_context,
        identity,
        mode,
    };
    Ok((settings, config))
}

/// Every tool this build has, filtered by `mode`.
pub(crate) fn build_registry(mode: Mode) -> Result<Registry> {
    let mut registry = Registry::new(mode);
    system::register(&mut registry)?;
    tools::register_all(&mut registry)?;
    Ok(registry)
}

async fn serve(settings: server::Settings, config: ClientConfig) -> Result<()> {
    let registry = build_registry(settings.mode)?;
    let server = server::McpServer::new(settings, server::Seams::production(config), registry);
    server::serve_stdio(server, CancellationToken::new()).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::DirBuilderExt;
    use std::path::PathBuf;

    fn root_of(cwd: &Path, requested: Option<&Path>) -> std::result::Result<PathBuf, ClientError> {
        launch(cwd, requested, Identity::of(None), Mode::Default).map(|(settings, config)| {
            assert_eq!(
                config.project_root.as_deref(),
                Some(settings.project_root.as_path())
            );
            settings.project_root
        })
    }

    #[test]
    fn the_root_override_selects_exactly_the_requested_project() {
        let outer = tempfile::tempdir().unwrap();
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(outer.path().join(".orchestry"))
            .unwrap();
        let nested = outer.path().join("child");
        std::fs::create_dir_all(&nested).unwrap();
        let canonical_outer = std::fs::canonicalize(outer.path()).unwrap();

        // No override: walk up, as every CLI command does.
        assert_eq!(root_of(&nested, None).unwrap(), canonical_outer);
        // Override relative to the cwd, canonicalized.
        assert_eq!(
            root_of(&nested, Some(Path::new(".."))).unwrap(),
            canonical_outer
        );
        // A non-project inside a project is refused, not walked up from.
        let err = root_of(outer.path(), Some(Path::new("child"))).expect_err("not a project");
        assert_eq!(err.exit_code(), 3);
        // Missing paths and plain files are not projects either.
        assert_eq!(
            root_of(outer.path(), Some(Path::new("nope")))
                .unwrap_err()
                .exit_code(),
            3
        );
        std::fs::write(outer.path().join("file"), "").unwrap();
        assert_eq!(
            root_of(outer.path(), Some(Path::new("file")))
                .unwrap_err()
                .exit_code(),
            3
        );
    }

    #[test]
    fn the_session_is_an_mcp_session_in_the_launch_mode_with_the_identity() {
        use orch_proto::handshake::SessionMode;
        let project = tempfile::tempdir().unwrap();
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(project.path().join(".orchestry"))
            .unwrap();
        let (settings, config) = launch(
            project.path(),
            None,
            Identity::of(Some("agt_me")),
            Mode::ReadOnly,
        )
        .unwrap();
        assert_eq!(config.caller.kind, CallerKind::Mcp);
        assert_eq!(config.mode, SessionMode::ReadOnly);
        assert_eq!(
            config.caller.agent_id.as_ref().map(|id| id.as_str()),
            Some("agt_me")
        );
        assert!(settings.agent_context && config.agent_context);
        // No .orchestry file was created by startup.
        assert_eq!(
            std::fs::read_dir(project.path().join(".orchestry"))
                .unwrap()
                .count(),
            0
        );
        let err = launch(
            project.path(),
            None,
            Identity::of(Some(&"a".repeat(257))),
            Mode::Default,
        )
        .unwrap_err();
        assert_eq!(err.exit_code(), 2);
    }

    #[test]
    fn the_full_registry_always_has_status_and_doctor() {
        for mode in [Mode::ReadOnly, Mode::Default, Mode::AllowDestructive] {
            let registry = build_registry(mode).unwrap();
            let names: Vec<String> = registry
                .into_router()
                .list_all()
                .into_iter()
                .map(|tool| tool.name.into_owned())
                .collect();
            assert!(names.contains(&"status".to_owned()), "{mode:?}");
            assert!(names.contains(&"doctor".to_owned()), "{mode:?}");
        }
    }

    #[test]
    fn the_catalogue_size_per_mode_is_unchanged() {
        let count = |mode| build_registry(mode).unwrap().into_router().list_all().len();
        assert_eq!(count(Mode::ReadOnly), 18);
        assert_eq!(count(Mode::Default), 45);
        assert_eq!(count(Mode::AllowDestructive), 50);
    }
}
