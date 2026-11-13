//! `orch tui` — connect to the project's daemon, run the dashboard, and show
//! the no-daemon screen whenever there is no daemon to talk to.
//!
//! ```text
//!   connect ──ok──▶ dashboard ──q──────────────▶ exit 0
//!      │               │
//!      │            session lost
//!      ▼               ▼
//!   no-daemon screen ◀─┘   r: connect again    q: exit 7
//! ```
//!
//! The dashboard is a client: opening or closing it changes nothing in the
//! daemon (dispatch stays as it was; running agents are the daemon's). So
//! there is no wind-down on exit any more, and SIGHUP/SIGTERM simply end the
//! draw loop. **No path here starts a daemon** (goal row 3, spec R4).
//! A bare `orch` opens this same dashboard client; initialization is explicit.

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use orch_client::{ClientConfig, ClientError, NoDaemon};
use orch_core::error::{Error, Result};
use orch_proto::handshake::CallerKind;

use super::backend::DaemonBackend;
use super::connect::Connector;
use super::no_daemon::{self, Choice, Screen};

/// The exit status of `q` on the no-daemon screen, and of a non-interactive
/// `orch tui` with no daemon: the NoDaemon code (spec R5).
const NO_DAEMON_EXIT: u8 = 7;

/// `orch tui`. `observe` is the removed `--observe`, refused here so the
/// command line and this entry point agree whichever lands first.
pub async fn run(cwd: PathBuf, version: &str, observe: bool) -> Result<ExitCode> {
    run_with_peer(
        cwd,
        version,
        observe,
        orch_domain::PeerConfig::default(),
        false,
    )
    .await
}

/// The entry point `orch tui [--listen]` routes to. `cwd` is where discovery
/// starts (the project root or any directory below it).
///
/// `--observe` and `--listen` are refused: the dashboard is always a client,
/// and a peer listener is a daemon start option (design D8).
pub async fn run_with_peer(
    cwd: PathBuf,
    version: &str,
    observe: bool,
    peer: orch_domain::PeerConfig,
    explicit_listen: bool,
) -> Result<ExitCode> {
    if observe {
        return Err(Error::invalid_arguments(
            "orch tui --observe was removed: the dashboard is always a client of the project's daemon",
        ));
    }
    if explicit_listen || peer.listen.is_some() {
        return Err(Error::invalid_arguments(
            "--listen is a daemon start option: orch daemon start --listen <addr>",
        ));
    }
    run_as_agent(cwd, version, None).await
}

/// `orch [--agent <id>] tui`: the dashboard as the caller `agent` names (the
/// root `--agent`, design R8), or as the environment's caller
/// (`ORCH_AGENT_ID`) when it is `None`. The identity only rides on the
/// handshake; nothing in this process's environment is changed.
pub async fn run_as_agent(cwd: PathBuf, version: &str, agent: Option<&str>) -> Result<ExitCode> {
    let config = ClientConfig::from_env(cwd.clone())
        .and_then(|config| config.with_agent(agent))
        .map_err(client_error)?;
    let connector = Connector::from_config(config, CallerKind::Tui);
    run_dashboard(connector, cwd, version).await
}

/// The loop in the module diagram, over any [`Connector`].
pub async fn run_dashboard(connector: Connector, cwd: PathBuf, version: &str) -> Result<ExitCode> {
    // SIGHUP (the terminal went away) and SIGTERM end the draw loop and the
    // no-daemon screen the way `q` does. Installed before the first connect,
    // so a signal during it is honoured rather than killing the terminal mode.
    let stop = Arc::new(AtomicBool::new(false));
    let signalled = Arc::new(AtomicBool::new(false));
    watch_termination(Arc::clone(&stop), Arc::clone(&signalled));

    let mut screen: Option<Screen> = None;
    let mut filters = orch_tui::KeywordFilters::default();
    loop {
        let failure =
            match DaemonBackend::launch(connector.clone(), version, cwd.clone(), Arc::clone(&stop))
                .await
            {
                Ok(backend) => {
                    // Leave the screen before the dashboard enters its own.
                    drop(screen.take());
                    let loss = backend.session_loss();
                    let (outcome, retained) =
                        run_draw_loop(backend, Arc::clone(&stop), filters).await?;
                    filters = retained;
                    if signalled.load(Ordering::SeqCst) {
                        return Ok(ExitCode::SUCCESS);
                    }
                    match loss.failure() {
                        // The session went away under the dashboard.
                        Some(rendered) => {
                            stop.store(false, Ordering::SeqCst);
                            rendered
                        }
                        None => return Ok(outcome),
                    }
                }
                Err(error) => match fatal(&error) {
                    Some(exit) => return exit,
                    None => error.render(connector.agent_context()),
                },
            };

        if signalled.load(Ordering::SeqCst) {
            eprintln!("{failure}");
            return Ok(ExitCode::from(NO_DAEMON_EXIT));
        }
        if !Screen::available() {
            // Nothing to show a screen on: say why, with the frozen text,
            // and exit with its code.
            eprintln!("{failure}");
            return Ok(ExitCode::from(NO_DAEMON_EXIT));
        }
        let mut active = match screen.take() {
            Some(active) => active,
            None => Screen::open()
                .map_err(|err| Error::other(format!("cannot show the no-daemon screen: {err}")))?,
        };
        // The key wait blocks, so it runs on the blocking pool; the screen
        // (and the terminal mode it holds) comes back with the answer.
        let asked = {
            let stop = Arc::clone(&stop);
            let failure = failure.clone();
            tokio::task::spawn_blocking(move || {
                let choice = active.ask(&failure, &stop);
                (active, choice)
            })
            .await
            .map_err(|err| Error::other(format!("the no-daemon screen was cancelled: {err}")))?
        };
        let (mut active, choice) = asked;
        let choice =
            choice.map_err(|err| Error::other(format!("the no-daemon screen failed: {err}")))?;
        match choice {
            Choice::Retry => {
                let _ = active.draw(&failure, no_daemon::CONNECTING);
                screen = Some(active);
            }
            Choice::Quit => {
                drop(active);
                // Leave the reason in the scrollback.
                eprintln!("{failure}");
                return Ok(ExitCode::from(NO_DAEMON_EXIT));
            }
        }
    }
}

/// Failures the screen cannot help with: an uninitialized project (exit 3)
/// and invalid input such as an over-long socket path (exit 2) are reported
/// and exit at once, as every other client does.
fn fatal(error: &ClientError) -> Option<Result<ExitCode>> {
    match error {
        ClientError::Connection(NoDaemon::NotInitialized) => {
            Some(Err(Error::Domain(orch_domain::DomainError::NotInitialized)))
        }
        ClientError::InvalidArguments(message) => {
            Some(Err(Error::invalid_arguments(message.clone())))
        }
        _ => None,
    }
}

fn client_error(error: ClientError) -> Error {
    match error {
        ClientError::InvalidArguments(message) => Error::invalid_arguments(message),
        other => Error::other(other.to_string()),
    }
}

/// Run `orch_tui::run_until` on a plain thread and wait for it.
///
/// The draw loop must not run on a runtime thread: `TuiBackend` is
/// synchronous and its methods call `Handle::block_on`, which panics inside a
/// runtime context. A plain `std::thread` has no such context, so the handle
/// the backend carries resolves against the runtime this task is running on.
async fn run_draw_loop(
    backend: DaemonBackend,
    stop: Arc<AtomicBool>,
    mut filters: orch_tui::KeywordFilters,
) -> Result<(ExitCode, orch_tui::KeywordFilters)> {
    let draw = std::thread::Builder::new()
        .name("orch-tui".to_owned())
        .spawn(move || {
            orch_tui::run_until_with_filters(backend, &stop, &mut filters)
                .map(|exit| (exit, filters))
        })
        .map_err(|err| Error::other(format!("cannot start the dashboard thread: {err}")))?;
    // `join` blocks, so it goes to the blocking pool rather than a worker:
    // awaiting here keeps the runtime free to serve the draw thread's calls.
    let outcome = tokio::task::spawn_blocking(move || draw.join())
        .await
        .map_err(|err| Error::other(format!("the dashboard thread was cancelled: {err}")))?;
    match outcome {
        // The guard in `orch_tui::run` restored the terminal before unwinding
        // past it, so it is safe to print.
        Err(panic) => Err(Error::other(format!(
            "the dashboard panicked: {}",
            panic_message(&panic)
        ))),
        Ok(Err(err)) => Err(Error::other(format!("dashboard: {err}"))),
        Ok(Ok((_, filters))) => Ok((ExitCode::SUCCESS, filters)),
    }
}

/// Set `stop` (and `signalled`) when SIGHUP or SIGTERM arrives; a second one
/// exits at once. The dashboard owns no agents any more, so there is no grace
/// to wait out: the first signal is a normal quit.
#[cfg(unix)]
fn watch_termination(stop: Arc<AtomicBool>, signalled: Arc<AtomicBool>) {
    use tokio::signal::unix::{SignalKind, signal};

    let (mut hangup, mut terminate) = match (
        signal(SignalKind::hangup()),
        signal(SignalKind::terminate()),
    ) {
        (Ok(hangup), Ok(terminate)) => (hangup, terminate),
        (Err(err), _) | (_, Err(err)) => {
            tracing::warn!(%err, "cannot install SIGHUP/SIGTERM handlers; they will end the dashboard without restoring the terminal");
            return;
        }
    };
    tokio::spawn(async move {
        tokio::select! {
            _ = hangup.recv() => tracing::info!("SIGHUP: closing the dashboard"),
            _ = terminate.recv() => tracing::info!("SIGTERM: closing the dashboard"),
        }
        signalled.store(true, Ordering::SeqCst);
        stop.store(true, Ordering::SeqCst);

        tokio::select! {
            _ = hangup.recv() => {}
            _ = terminate.recv() => {}
        }
        eprintln!("orch tui: second signal, exiting now");
        std::process::exit(130);
    });
}

#[cfg(not(unix))]
fn watch_termination(_stop: Arc<AtomicBool>, _signalled: Arc<AtomicBool>) {}

fn panic_message(panic: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(msg) = panic.downcast_ref::<&str>() {
        (*msg).to_owned()
    } else if let Some(msg) = panic.downcast_ref::<String>() {
        msg.clone()
    } else {
        "unknown cause".to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn observe_and_listen_are_refused_before_any_connect() {
        let observe = run_with_peer(
            PathBuf::from("/unused"),
            "test",
            true,
            orch_domain::PeerConfig::default(),
            false,
        )
        .await
        .unwrap_err();
        assert!(
            observe.to_string().contains("--observe was removed"),
            "{observe}"
        );

        let listen = run_with_peer(
            PathBuf::from("/unused"),
            "test",
            false,
            orch_domain::PeerConfig {
                name: None,
                listen: Some("127.0.0.1:0".into()),
            },
            true,
        )
        .await
        .unwrap_err();
        assert!(
            listen.to_string().contains("orch daemon start --listen"),
            "{listen}"
        );
    }
}
