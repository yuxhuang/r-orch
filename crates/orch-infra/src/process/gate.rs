//! Complete-spawn exclusion on macOS. Async callers reserve before taking any
//! project/state lock; synchronous callers retain the same exclusion domain.

mod observation;
pub(crate) mod work;

use std::cell::RefCell;
#[cfg(target_os = "macos")]
use std::sync::{Arc, OnceLock};

use observation::Notice;
pub use observation::{SpawnObservation, SpawnObserver, SpawnSite, SpawnStage, pending_spawns};
pub use work::SpawnWork;

#[cfg(target_os = "macos")]
fn gate() -> &'static Arc<tokio::sync::Semaphore> {
    static GATE: OnceLock<Arc<tokio::sync::Semaphore>> = OnceLock::new();
    GATE.get_or_init(|| Arc::new(tokio::sync::Semaphore::new(1)))
}

/// A Send reservation may cross awaits as an ordinary value. Its thread-local
/// handoff exists only during `run`'s synchronous call, never across an await.
pub struct SpawnReservation {
    // Clear the completed stage before another permit owner can enter.
    notice: Notice,
    #[cfg(target_os = "macos")]
    _permit: tokio::sync::OwnedSemaphorePermit,
}

thread_local! {
    static RESERVED: RefCell<Option<SpawnReservation>> = const { RefCell::new(None) };
}

struct ReservationScope(std::marker::PhantomData<std::rc::Rc<()>>);
impl orch_core::process::SpawnScope for ReservationScope {}
impl Drop for ReservationScope {
    fn drop(&mut self) {
        RESERVED.with(|slot| slot.borrow_mut().take());
    }
}
impl SpawnReservation {
    fn enter_scope(self) -> ReservationScope {
        RESERVED.with(|slot| {
            assert!(slot.borrow().is_none(), "nested spawn reservation");
            *slot.borrow_mut() = Some(self);
        });
        ReservationScope(std::marker::PhantomData)
    }
    pub fn run<T>(self, call: impl FnOnce() -> T) -> T {
        let _scope = self.enter_scope();
        call()
    }
}

/// Assembly-only provider for the core final-launch admission port.
#[derive(Default)]
pub struct ProcessSpawnAdmission;
#[async_trait::async_trait]
impl orch_core::process::SpawnAdmission for ProcessSpawnAdmission {
    async fn reserve(&self) -> Box<dyn orch_core::process::SpawnReservation> {
        Box::new(reserve_spawn(SpawnSite::Launch).await)
    }
}
impl orch_core::process::SpawnReservation for SpawnReservation {
    fn enter(self: Box<Self>) -> Box<dyn orch_core::process::SpawnScope> {
        Box::new(self.enter_scope())
    }
}

pub async fn reserve_spawn(site: SpawnSite) -> SpawnReservation {
    reserve_observed(site, None).await
}

pub(crate) struct WaitingSpawn(Notice);

pub(crate) fn observe_spawn(
    site: SpawnSite,
    observer: Option<std::sync::Arc<dyn SpawnObserver>>,
) -> WaitingSpawn {
    WaitingSpawn(Notice::new(site, observer))
}

impl WaitingSpawn {
    pub(crate) async fn reserve(self) -> SpawnReservation {
        #[cfg(all(test, target_os = "macos"))]
        tests::before_lock(gate());
        #[cfg(target_os = "macos")]
        let permit = Arc::clone(gate())
            .acquire_owned()
            .await
            .expect("spawn gate is never closed");
        SpawnReservation {
            #[cfg(target_os = "macos")]
            _permit: permit,
            notice: self.0,
        }
    }
}

pub(crate) async fn reserve_observed(
    site: SpawnSite,
    observer: Option<std::sync::Arc<dyn SpawnObserver>>,
) -> SpawnReservation {
    observe_spawn(site, observer).reserve().await
}

/// End the closure at spawn's actual return/error/unwind, before output or wait.
/// Linux never acquires the macOS exclusion permit.
pub(crate) fn with_spawn_gate<T>(spawn: impl FnOnce() -> T) -> T {
    let reservation = RESERVED.with(|slot| slot.borrow_mut().take());
    let mut reservation = reservation.unwrap_or_else(|| {
        #[cfg(all(test, target_os = "macos"))]
        tests::before_lock(gate());
        futures::executor::block_on(reserve_spawn(SpawnSite::Synchronous))
    });
    // The permit stays in this stack frame even if an observer unwinds.
    reservation.notice.enter();
    #[cfg(all(test, target_os = "macos"))]
    tests::at_spawn();
    spawn()
}

#[cfg(test)]
mod capture_tests;
#[cfg(all(test, target_os = "macos"))]
pub(crate) mod tests;

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
