//! Serialize complete synchronous spawns on macOS, whose std pipe setup sets
//! CLOEXEC after creating the descriptors. Another fork in that window could
//! retain a handshake or captured-output writer until an unrelated child exits.

#[cfg(target_os = "macos")]
use std::sync::{Mutex, PoisonError};

#[cfg(target_os = "macos")]
static SPAWN_GATE: Mutex<()> = Mutex::new(());

/// The closure must end at `spawn()`: no await, output read or child wait.
/// Linux creates these descriptors with atomic CLOEXEC and takes no lock.
pub(crate) fn with_spawn_gate<T>(spawn: impl FnOnce() -> T) -> T {
    #[cfg(target_os = "macos")]
    let _guard = {
        #[cfg(test)]
        tests::before_lock(&SPAWN_GATE);
        SPAWN_GATE.lock().unwrap_or_else(PoisonError::into_inner)
    };
    #[cfg(all(test, target_os = "macos"))]
    tests::at_spawn();
    spawn()
}

#[cfg(all(test, target_os = "macos"))]
mod tests;

#[cfg(test)]
mod capture_tests;

#[cfg(all(test, target_os = "linux"))]
mod platform_tests {
    use super::with_spawn_gate;
    use std::sync::mpsc;
    use std::time::Duration;

    #[test]
    fn linux_helper_does_not_serialize_callers() {
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let (finished_tx, finished_rx) = mpsc::channel();
        std::thread::scope(|scope| {
            scope.spawn(move || {
                with_spawn_gate(|| {
                    entered_tx.send(()).unwrap();
                    release_rx.recv_timeout(Duration::from_secs(20)).unwrap();
                })
            });
            entered_rx.recv_timeout(Duration::from_secs(20)).unwrap();
            scope.spawn(move || {
                with_spawn_gate(|| {
                    finished_tx.send(()).unwrap();
                })
            });
            let finished = finished_rx.recv_timeout(Duration::from_secs(20));
            release_tx.send(()).unwrap();
            assert!(
                finished.is_ok(),
                "Linux must not acquire the macOS spawn lock"
            );
        });
    }
}
