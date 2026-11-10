//! Bounded, payload-free observation independent of the runtime polling workers.
use std::collections::BTreeMap;
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

const RECORDS: usize = 128;
const FIRST: Duration = Duration::from_secs(1);
const AGAIN: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpawnSite {
    Launch,
    Workspace,
    Version,
    Discovery,
    Hostname,
    Synchronous,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpawnStage {
    Waiting,
    Entered,
}
#[derive(Clone, Copy, Debug)]
pub struct SpawnObservation {
    pub id: u64,
    pub site: SpawnSite,
    pub stage: SpawnStage,
    pub elapsed: Duration,
}

/// An internal observation port. Production entry points install no custom implementation;
/// the daemon testing harness injects its synchronization observer here.
pub trait SpawnObserver: Send + Sync {
    fn observe(&self, observation: SpawnObservation);
}

struct Record {
    observation: SpawnObservation,
    since: Instant,
    next: Instant,
    dispatch: tracing::Dispatch,
}
#[derive(Default)]
struct State {
    next: u64,
    records: BTreeMap<u64, Record>,
    overflow: [usize; 2],
    overflow_clock: [Option<(Instant, Instant, tracing::Dispatch)>; 2],
    observing: bool,
}
fn registry() -> &'static (Mutex<State>, Condvar) {
    static REGISTRY: OnceLock<(Mutex<State>, Condvar)> = OnceLock::new();
    REGISTRY.get_or_init(Default::default)
}

/// Bounded internal snapshot; contains only generated ids and closed enums.
pub fn pending_spawns() -> Vec<SpawnObservation> {
    let state = registry().0.lock().unwrap_or_else(|e| e.into_inner());
    state
        .records
        .values()
        .map(|record| SpawnObservation {
            elapsed: record.since.elapsed(),
            ..record.observation
        })
        .collect()
}

pub(super) struct Notice {
    id: u64,
    site: SpawnSite,
    observer: Option<Arc<dyn SpawnObserver>>,
    since: Instant,
    recorded: bool,
    stage: usize,
}
impl Notice {
    pub(super) fn new(site: SpawnSite, observer: Option<Arc<dyn SpawnObserver>>) -> Self {
        let (lock, wake) = registry();
        let since = Instant::now();
        let mut state = lock.lock().unwrap_or_else(|e| e.into_inner());
        state.next = state.next.wrapping_add(1);
        let id = state.next;
        let recorded = state.records.len() < RECORDS;
        let event = SpawnObservation {
            id,
            site,
            stage: SpawnStage::Waiting,
            elapsed: Duration::ZERO,
        };
        if recorded {
            state.records.insert(
                id,
                Record {
                    observation: event,
                    since,
                    next: since + FIRST,
                    dispatch: tracing::dispatcher::get_default(Clone::clone),
                },
            );
        } else {
            state.overflow[0] += 1;
            state.overflow_clock[0].get_or_insert_with(|| {
                (
                    since,
                    since + FIRST,
                    tracing::dispatcher::get_default(Clone::clone),
                )
            });
        }
        if !state.observing {
            state.observing = true;
            // A process-wide, short-lived observer; no per-request threads.
            std::thread::Builder::new()
                .name("orch-spawn-log".into())
                .spawn(observe)
                .expect("start spawn observation thread");
        }
        drop(state);
        wake.notify_one();
        let notice = Self {
            id,
            site,
            observer,
            since,
            recorded,
            stage: 0,
        };
        if let Some(observer) = &notice.observer {
            observer.observe(event);
        }
        notice
    }
    pub(super) fn enter(&mut self) {
        self.since = Instant::now();
        let event = SpawnObservation {
            id: self.id,
            site: self.site,
            stage: SpawnStage::Entered,
            elapsed: Duration::ZERO,
        };
        if self.recorded {
            let mut state = registry().0.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(record) = state.records.get_mut(&self.id) {
                record.observation = event;
                record.since = self.since;
                record.next = self.since + FIRST;
            }
        }
        if !self.recorded {
            let mut state = registry().0.lock().unwrap_or_else(|e| e.into_inner());
            state.overflow[0] -= 1;
            if state.overflow[0] == 0 {
                state.overflow_clock[0] = None;
            }
            state.overflow[1] += 1;
            state.overflow_clock[1].get_or_insert_with(|| {
                (
                    self.since,
                    self.since + FIRST,
                    tracing::dispatcher::get_default(Clone::clone),
                )
            });
            self.stage = 1;
        }
        registry().1.notify_one();
        if let Some(observer) = &self.observer {
            observer.observe(event);
        }
    }
}
impl Drop for Notice {
    fn drop(&mut self) {
        let mut state = registry().0.lock().unwrap_or_else(|e| e.into_inner());
        if self.recorded {
            state.records.remove(&self.id);
        } else {
            state.overflow[self.stage] -= 1;
            if state.overflow[self.stage] == 0 {
                state.overflow_clock[self.stage] = None;
            }
        }
        registry().1.notify_one();
    }
}
fn observe() {
    let (lock, wake) = registry();
    loop {
        let mut state = lock.lock().unwrap_or_else(|e| e.into_inner());
        if state.records.is_empty() && state.overflow == [0, 0] {
            state.observing = false;
            return;
        }
        let now = Instant::now();
        let mut due = Vec::new();
        for record in state.records.values_mut() {
            if now >= record.next {
                record.observation.elapsed = now.duration_since(record.since);
                due.push((record.observation, record.dispatch.clone()));
                record.next = now + AGAIN;
            }
        }
        let overflow = state.overflow.iter().sum::<usize>();
        for (index, clock) in state.overflow_clock.iter_mut().enumerate() {
            if let Some((since, next, dispatch)) = clock
                && now >= *next
            {
                due.push((
                    SpawnObservation {
                        id: 0,
                        site: SpawnSite::Synchronous,
                        stage: if index == 0 {
                            SpawnStage::Waiting
                        } else {
                            SpawnStage::Entered
                        },
                        elapsed: now.duration_since(*since),
                    },
                    dispatch.clone(),
                ));
                *next = now + AGAIN;
            }
        }
        if due.is_empty() {
            let next = state
                .records
                .values()
                .map(|r| r.next)
                .chain(
                    state
                        .overflow_clock
                        .iter()
                        .flatten()
                        .map(|(_, next, _)| *next),
                )
                .min()
                .unwrap_or(now + AGAIN);
            drop(
                wake.wait_timeout(state, next.saturating_duration_since(now))
                    .unwrap_or_else(|e| e.into_inner()),
            );
            continue;
        }
        drop(state);
        // Never hold even the observation mutex across a potentially slow log sink.
        for (event, dispatch) in due {
            emit(event, overflow, &dispatch);
        }
    }
}

// Dispatch directly to the captured private-log subscriber. Ambient macro
// callsite/max-level caching is process-global; it must not decide whether an
// independent observer's already-captured subscriber receives this event.
struct SpawnLogSite;
static LOG_SITE: SpawnLogSite = SpawnLogSite;
static LOG_META: tracing::Metadata<'static> = tracing::Metadata::new(
    "process spawn pending",
    "orch_infra::process::spawn",
    tracing::Level::WARN,
    None,
    None,
    None,
    tracing::field::FieldSet::new(
        &[
            "message",
            "spawn_id",
            "site",
            "stage",
            "elapsed_ms",
            "overflow",
        ],
        tracing::callsite::Identifier(&LOG_SITE),
    ),
    tracing::metadata::Kind::EVENT,
);
impl tracing::callsite::Callsite for SpawnLogSite {
    fn set_interest(&self, _: tracing::subscriber::Interest) {}
    fn metadata(&self) -> &tracing::Metadata<'_> {
        &LOG_META
    }
}
fn emit(event: SpawnObservation, overflow: usize, dispatch: &tracing::Dispatch) {
    if dispatch.register_callsite(&LOG_META).is_never() || !dispatch.enabled(&LOG_META) {
        return;
    }
    let fields = LOG_META.fields();
    let names: Vec<_> = fields.iter().collect();
    let message = "process spawn pending";
    let site = tracing::field::debug(event.site);
    let stage = tracing::field::debug(event.stage);
    let elapsed = event.elapsed.as_millis() as u64;
    let values: [(&tracing::field::Field, Option<&dyn tracing::field::Value>); 6] = [
        (&names[0], Some(&message)),
        (&names[1], Some(&event.id)),
        (&names[2], Some(&site)),
        (&names[3], Some(&stage)),
        (&names[4], Some(&elapsed)),
        (&names[5], Some(&overflow)),
    ];
    let values = fields.value_set(&values);
    dispatch.event(&tracing::Event::new(&LOG_META, &values));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use tracing::{
        Event, Subscriber,
        span::{Attributes, Id, Record as Fields},
    };

    static SERIAL: Mutex<()> = Mutex::new(());

    #[derive(Clone)]
    struct Logs(mpsc::Sender<String>);
    impl Subscriber for Logs {
        fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
            true
        }
        fn new_span(&self, _: &Attributes<'_>) -> Id {
            Id::from_u64(1)
        }
        fn record(&self, _: &Id, _: &Fields<'_>) {}
        fn record_follows_from(&self, _: &Id, _: &Id) {}
        fn event(&self, event: &Event<'_>) {
            struct Visitor(String);
            impl tracing::field::Visit for Visitor {
                fn record_debug(
                    &mut self,
                    field: &tracing::field::Field,
                    value: &dyn std::fmt::Debug,
                ) {
                    use std::fmt::Write;
                    write!(&mut self.0, "{}={value:?} ", field.name()).unwrap();
                }
            }
            let mut visitor = Visitor(String::new());
            event.record(&mut visitor);
            let _ = self.0.send(visitor.0);
        }
        fn enter(&self, _: &Id) {}
        fn exit(&self, _: &Id) {}
    }
    #[test]
    fn independent_telemetry_reports_entered_and_clears_on_unwind() {
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let (send, receive) = mpsc::channel();
        let (entered_send, entered_receive) = mpsc::channel();
        let (release_send, release_receive) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            tracing::subscriber::with_default(Logs(send), || {
                let permit =
                    futures::executor::block_on(super::super::reserve_spawn(SpawnSite::Discovery));
                let id = permit.notice.id;
                entered_send.send(id).unwrap();
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    permit.run(|| {
                        super::super::with_spawn_gate(|| {
                            release_receive
                                .recv_timeout(Duration::from_secs(60))
                                .unwrap();
                            panic!("controlled spawn unwind");
                        })
                    });
                }));
                assert!(result.is_err());
                assert!(
                    !pending_spawns().iter().any(|record| record.id == id),
                    "unwind left stale telemetry"
                );
            });
        });
        let id = entered_receive
            .recv_timeout(Duration::from_secs(20))
            .unwrap();
        let event = receive.recv_timeout(Duration::from_secs(20));
        let repeated = event
            .as_ref()
            .ok()
            .map(|_| receive.recv_timeout(Duration::from_secs(20)));
        // Release and join before asserting, including the telemetry-negative path.
        release_send.send(()).unwrap();
        worker.join().unwrap();
        let event = event.expect("independent spawn telemetry missing while worker was parked");
        assert!(
            event.contains(&format!("spawn_id={id}"))
                && event.contains("stage=Entered")
                && event.contains("site=Discovery")
                && event.contains("elapsed_ms="),
            "{event}"
        );
        let repeated = repeated
            .expect("first observation")
            .expect("persistent observation");
        let elapsed = |line: &str| {
            line.split("elapsed_ms=")
                .nth(1)
                .unwrap()
                .split_whitespace()
                .next()
                .unwrap()
                .parse::<u64>()
                .unwrap()
        };
        assert!(
            elapsed(&event) >= 1_000 && elapsed(&repeated) >= elapsed(&event) + 5_000,
            "warning thresholds were not respected: {event}; {repeated}"
        );
        assert!(!event.contains("argv") && !event.contains("HOME") && !event.contains("prompt"));
    }
    #[test]
    fn telemetry_has_a_fixed_record_cap_and_clears_error_and_overflow() {
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let records: Vec<_> = (0..RECORDS + 2)
            .map(|_| Notice::new(SpawnSite::Version, None))
            .collect();
        let state = registry().0.lock().unwrap_or_else(|e| e.into_inner());
        assert!(state.records.len() <= RECORDS);
        assert!(state.overflow[0] >= 2);
        drop(state);
        let ids: Vec<_> = records.iter().map(|record| record.id).collect();
        drop(records);
        assert!(
            !pending_spawns()
                .iter()
                .any(|record| ids.contains(&record.id))
        );
        let permit = futures::executor::block_on(super::super::reserve_spawn(SpawnSite::Launch));
        let id = permit.notice.id;
        let result: std::io::Result<()> = permit
            .run(|| super::super::with_spawn_gate(|| Err(std::io::ErrorKind::NotFound.into())));
        assert!(result.is_err());
        assert!(!pending_spawns().iter().any(|record| record.id == id));
    }
    #[cfg(target_os = "macos")]
    #[test]
    fn waiting_telemetry_is_visible_and_cancellation_clears_it() {
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let (send, receive) = mpsc::channel();
        tracing::subscriber::with_default(Logs(send), || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(async {
                let holder = super::super::reserve_spawn(SpawnSite::Discovery).await;
                let mut waiter = Box::pin(super::super::reserve_spawn(SpawnSite::Launch));
                assert!(futures::poll!(&mut waiter).is_pending());
                let id = pending_spawns()
                    .iter()
                    .find(|record| record.site == SpawnSite::Launch)
                    .unwrap()
                    .id;
                let log = tokio::task::spawn_blocking(move || {
                    let deadline = Instant::now() + Duration::from_secs(20);
                    loop {
                        let line = receive
                            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                            .unwrap();
                        if line.contains(&format!("spawn_id={id}")) {
                            break line;
                        }
                    }
                })
                .await
                .unwrap();
                assert!(log.contains("stage=Waiting"), "{log}");
                drop(waiter);
                assert!(!pending_spawns().iter().any(|record| record.id == id));
                drop(holder);
            });
        });
    }
}
