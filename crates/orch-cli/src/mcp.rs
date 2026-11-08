//! `orch mcp`: the project's services as a stdio MCP server.
//!
//! The server runs in-process over the light Engine tier and never takes the
//! orchestrator lock. Startup — flags, the project root, the identity, a first
//! Engine build — happens before anything reaches stdout, so every startup
//! failure is a CLI error on stderr with the CLI's exit code (3 for no
//! project). After that stdout belongs to the protocol.

pub(crate) mod identity;
pub(crate) mod mode;
pub(crate) mod projections;
pub(crate) mod registry;
pub(crate) mod resources;
pub(crate) mod server;
pub(crate) mod system;
mod tools;

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use orch_core::error::{Error, Result};
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
/// server has lost its stdout.
const RUNTIME_SHUTDOWN: Duration = Duration::from_millis(250);

/// Run `orch mcp`. Owns its runtime so the bounded shutdown applies to this
/// command alone.
pub(crate) fn run(args: &McpCmd, styles: &Styles) -> ExitCode {
    let report = |err: &Error| {
        print_error(styles, &err.to_string(), err.hint().as_deref());
        ExitCode::from(u8::try_from(err.exit_code()).unwrap_or(1))
    };

    // Read once: the identity never changes for the life of the server.
    let env_agent = std::env::var(AGENT_ENV).ok();
    let identity = match Identity::resolve(args.agent.as_deref(), env_agent.as_deref()) {
        Ok(identity) => identity,
        Err(err) => return report(&err),
    };
    let mode = Mode::from_flags(args.read_only, args.allow_destructive);
    let root = match std::env::current_dir()
        .map_err(|err| Error::other(format!("cannot read the working directory: {err}")))
        .and_then(|cwd| resolve_root(&cwd, args.project_root.as_deref()))
    {
        Ok(root) => root,
        Err(err) => return report(&err),
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
    let outcome = runtime.block_on(serve(root, identity, mode));
    runtime.shutdown_timeout(RUNTIME_SHUTDOWN);
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => report(&err),
    }
}

/// The project to serve. Without an override, the CLI's own walk up from the
/// working directory. With one, exactly that directory: resolved against the
/// working directory, canonicalized, and required to hold its **own**
/// `.orchestry` — never a parent's. A missing project is `Not initialized`
/// (exit 3); an unreadable path is an I/O error (exit 1).
pub(crate) fn resolve_root(cwd: &Path, project_root: Option<&Path>) -> Result<PathBuf> {
    use orch_infra::storage::ORCHESTRY_DIR;
    use std::io::ErrorKind;

    let not_initialized = || Error::from(orch_domain::DomainError::NotInitialized);
    let Some(requested) = project_root else {
        let root = orch_infra::storage::find_project_root(cwd);
        return if root.join(ORCHESTRY_DIR).is_dir() {
            Ok(root)
        } else {
            Err(not_initialized())
        };
    };
    let joined = cwd.join(requested);
    let root = match std::fs::canonicalize(&joined) {
        Ok(root) => root,
        Err(err) if err.kind() == ErrorKind::NotFound => return Err(not_initialized()),
        Err(err) => return Err(Error::io("cannot resolve --project-root", &joined, err)),
    };
    match std::fs::metadata(root.join(ORCHESTRY_DIR)) {
        Ok(metadata) if metadata.is_dir() => Ok(root),
        Ok(_) => Err(not_initialized()),
        Err(err) if matches!(err.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory) => {
            Err(not_initialized())
        }
        Err(err) => Err(Error::io("cannot read --project-root", &root, err)),
    }
}

/// Every tool this build has, filtered by `mode`.
pub(crate) fn build_registry(mode: Mode, doctor: system::DoctorRunner) -> Result<Registry> {
    let mut registry = Registry::new(mode);
    system::register(&mut registry, doctor)?;
    tools::register_all(&mut registry)?;
    Ok(registry)
}

async fn serve(root: PathBuf, identity: Identity, mode: Mode) -> Result<()> {
    // A first build surfaces a corrupt config as a startup error. Only the
    // project name is kept: every operation builds its own Engine.
    let engine = orch_engine::build_light(root.clone()).await?;
    let settings = server::Settings {
        project_name: engine.config.project.name.clone(),
        project_root: root,
        identity,
        mode,
    };
    drop(engine);

    let registry = build_registry(mode, system::production_doctor())?;
    let stop = CancellationToken::new();
    let server = server::McpServer::new(
        settings,
        server::Seams::production(),
        registry,
        stop.child_token(),
    );
    server::serve_stdio(server, stop).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_root_override_selects_exactly_the_requested_project() {
        let outer = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(outer.path().join(".orchestry")).unwrap();
        let nested = outer.path().join("child");
        std::fs::create_dir_all(&nested).unwrap();
        let canonical_outer = std::fs::canonicalize(outer.path()).unwrap();

        // No override: walk up, as every CLI command does.
        assert_eq!(resolve_root(&nested, None).unwrap(), outer.path());
        // Override relative to the cwd, canonicalized.
        assert_eq!(
            resolve_root(&nested, Some(Path::new(".."))).unwrap(),
            canonical_outer
        );
        // A non-project inside a project is refused, not walked up from.
        let err = resolve_root(outer.path(), Some(Path::new("child"))).expect_err("not a project");
        assert_eq!(err.exit_code(), 3);
        // Missing paths and plain files are not projects either.
        assert_eq!(
            resolve_root(outer.path(), Some(Path::new("nope")))
                .unwrap_err()
                .exit_code(),
            3
        );
        std::fs::write(outer.path().join("file"), "").unwrap();
        assert_eq!(
            resolve_root(outer.path(), Some(Path::new("file")))
                .unwrap_err()
                .exit_code(),
            3
        );
    }

    #[test]
    fn the_full_registry_always_has_status_and_doctor() {
        for mode in [Mode::ReadOnly, Mode::Default, Mode::AllowDestructive] {
            let registry = build_registry(mode, system::production_doctor()).unwrap();
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
}
