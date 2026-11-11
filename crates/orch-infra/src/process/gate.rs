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

/// Spawn admission for library assembly. Defaults to process-wide admission.
#[derive(Clone, Default)]
pub struct SpawnDomain(Domain);

#[derive(Clone, Default)]
enum Domain {
    #[default]
    Process,
    #[cfg(target_os = "macos")]
    Synthetic(Arc<tokio::sync::Semaphore>),
}

impl SpawnDomain {
    /// Isolate admission only for ProcessManagers that never fork. The
    /// fail-closed check covers crate-internal real builders only; external
    /// direct Command builders must never opt in. Linux keeps Process admission.
    pub fn for_synthetic_processes() -> Self {
        #[cfg(target_os = "macos")]
        {
            Self(Domain::Synthetic(Arc::new(tokio::sync::Semaphore::new(1))))
        }
        #[cfg(not(target_os = "macos"))]
        Self::default()
    }

    /// Whether assembly should retain the production admission provider.
    pub fn is_process(&self) -> bool {
        matches!(self.0, Domain::Process)
    }

    /// Reserve before taking any project/state lock. A synthetic reservation
    /// must only reach non-forking ProcessManagers.
    pub async fn reserve(&self, site: SpawnSite) -> SpawnReservation {
        match &self.0 {
            Domain::Process => reserve_spawn(site).await,
            #[cfg(target_os = "macos")]
            Domain::Synthetic(gate) => {
                let notice = Notice::new(site, None);
                let permit = Arc::clone(gate)
                    .acquire_owned()
                    .await
                    .expect("synthetic spawn gate is never closed");
                SpawnReservation {
                    notice,
                    work: None,
                    lease: None,
                    permit: Permit::Synthetic { _permit: permit },
                }
            }
        }
    }
}

#[async_trait::async_trait]
impl orch_core::process::SpawnAdmission for SpawnDomain {
    async fn reserve(&self) -> Box<dyn orch_core::process::SpawnReservation> {
        Box::new(self.reserve(SpawnSite::Launch).await)
    }
}

// Provenance is private: a local permit cannot prove process-wide exclusion.
#[cfg(target_os = "macos")]
enum Permit {
    Process {
        _permit: tokio::sync::OwnedSemaphorePermit,
    },
    Synthetic {
        _permit: tokio::sync::OwnedSemaphorePermit,
    },
}

/// A Send reservation may cross awaits as an ordinary value. Its thread-local
/// handoff exists only during `run`'s synchronous call, never across an await.
pub struct SpawnReservation {
    // Clear the completed stage before another permit owner can enter.
    notice: Notice,
    work: Option<std::sync::Arc<SpawnWork>>,
    lease: Option<work::WorkLease>,
    #[cfg(target_os = "macos")]
    permit: Permit,
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
/// A final launch shares shutdown ownership with diagnostics and preparation.
/// Reservation of the process-wide gate still precedes project/state locks;
/// tracker admission is decided inside the final operation before its writes.
pub struct TrackedSpawnAdmission {
    domain: SpawnDomain,
    work: std::sync::Arc<SpawnWork>,
}
impl TrackedSpawnAdmission {
    pub fn new(domain: SpawnDomain, work: std::sync::Arc<SpawnWork>) -> Self {
        Self { domain, work }
    }
}
#[async_trait::async_trait]
impl orch_core::process::SpawnAdmission for TrackedSpawnAdmission {
    async fn reserve(&self) -> Box<dyn orch_core::process::SpawnReservation> {
        let mut reservation = self.domain.reserve(SpawnSite::Launch).await;
        reservation.work = Some(self.work.clone());
        Box::new(reservation)
    }
    fn is_closed(&self) -> bool {
        self.work.is_sealed()
    }
}

/// The handoff is single-use and tracker-specific. A second process call, or
/// a different manager's tracker, must obtain a new admission in the usual way.
pub(crate) fn take_admitted_work(work: &std::sync::Arc<SpawnWork>) -> Option<work::WorkLease> {
    RESERVED.with(|slot| {
        let mut slot = slot.borrow_mut();
        let reservation = slot.as_mut()?;
        if reservation.lease.as_ref()?.belongs_to(work) {
            reservation.lease.take()
        } else {
            None
        }
    })
}

impl orch_core::process::SpawnReservation for SpawnReservation {
    fn try_admit(&mut self) -> bool {
        let Some(work) = &self.work else {
            return true;
        };
        if self.lease.is_none() {
            self.lease = work.track();
        }
        self.lease.is_some()
    }

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
            permit: Permit::Process { _permit: permit },
            notice: self.0,
            work: None,
            lease: None,
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
    #[cfg(target_os = "macos")]
    assert!(
        matches!(reservation.permit, Permit::Process { .. }),
        "synthetic spawn reservation cannot authorize a real process"
    );
    // The permit stays in this stack frame even if an observer unwinds.
    reservation.notice.enter();
    #[cfg(all(test, target_os = "macos"))]
    tests::at_spawn();
    spawn()
}

#[cfg(test)]
mod capture_tests;
#[cfg(test)]
mod synthetic_tests;
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

#[cfg(test)]
mod tracked_tests;
