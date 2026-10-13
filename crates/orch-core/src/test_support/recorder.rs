//! Call recording — the replacement for `vi.fn()`'s call assertions.

use std::sync::{Arc, Mutex};

/// A shared, cloneable log of calls made to a test double.
///
/// Cloning a `Recorder` shares the log, so a fake can hand one out and the
/// test still sees everything the fake records.
pub struct Recorder<C> {
    calls: Arc<Mutex<Vec<C>>>,
}

impl<C> Clone for Recorder<C> {
    fn clone(&self) -> Self {
        Recorder {
            calls: Arc::clone(&self.calls),
        }
    }
}

impl<C> Default for Recorder<C> {
    fn default() -> Self {
        Recorder {
            calls: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

impl<C> std::fmt::Debug for Recorder<C>
where
    C: std::fmt::Debug,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("Recorder").field(&*self.lock()).finish()
    }
}

impl<C> Recorder<C> {
    pub fn new() -> Self {
        Recorder::default()
    }

    /// Mutex poisoning is irrelevant here: a panicking test has already
    /// failed, and swallowing it keeps the real assertion failure visible
    /// instead of masking it with a poison error.
    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<C>> {
        self.calls
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub fn record(&self, call: C) {
        self.lock().push(call);
    }

    pub fn len(&self) -> usize {
        self.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn clear(&self) {
        self.lock().clear();
    }
}

impl<C: Clone> Recorder<C> {
    /// Every call, in order.
    pub fn calls(&self) -> Vec<C> {
        self.lock().clone()
    }

    /// The first call, if any.
    pub fn first(&self) -> Option<C> {
        self.lock().first().cloned()
    }

    /// The most recent call, if any.
    pub fn last(&self) -> Option<C> {
        self.lock().last().cloned()
    }

    /// Every call matching a predicate, in order. Use it to project one method
    /// out of a multi-method call enum.
    pub fn matching(&self, predicate: impl Fn(&C) -> bool) -> Vec<C> {
        self.lock()
            .iter()
            .filter(|call| predicate(call))
            .cloned()
            .collect()
    }

    pub fn count_matching(&self, predicate: impl Fn(&C) -> bool) -> usize {
        self.lock().iter().filter(|call| predicate(call)).count()
    }
}

impl<C: Clone + PartialEq> Recorder<C> {
    pub fn contains(&self, call: &C) -> bool {
        self.lock().contains(call)
    }

    pub fn count_of(&self, call: &C) -> usize {
        self.lock()
            .iter()
            .filter(|recorded| *recorded == call)
            .count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Clone, PartialEq)]
    enum Call {
        Get(&'static str),
        Save(&'static str),
    }

    #[test]
    fn records_calls_in_order() {
        let recorder: Recorder<Call> = Recorder::new();
        assert!(recorder.is_empty());

        recorder.record(Call::Get("a"));
        recorder.record(Call::Save("b"));
        recorder.record(Call::Get("a"));

        assert_eq!(recorder.len(), 3);
        assert_eq!(
            recorder.calls(),
            vec![Call::Get("a"), Call::Save("b"), Call::Get("a")]
        );
        assert_eq!(recorder.first(), Some(Call::Get("a")));
        assert_eq!(recorder.last(), Some(Call::Get("a")));
    }

    #[test]
    fn counts_and_contains() {
        let recorder: Recorder<Call> = Recorder::new();
        recorder.record(Call::Get("a"));
        recorder.record(Call::Get("a"));
        recorder.record(Call::Save("b"));

        assert_eq!(recorder.count_of(&Call::Get("a")), 2);
        assert_eq!(recorder.count_of(&Call::Get("z")), 0);
        assert!(recorder.contains(&Call::Save("b")));
        assert!(!recorder.contains(&Call::Save("z")));
    }

    #[test]
    fn projects_one_method_with_a_predicate() {
        let recorder: Recorder<Call> = Recorder::new();
        recorder.record(Call::Get("a"));
        recorder.record(Call::Save("b"));
        recorder.record(Call::Save("c"));

        assert_eq!(recorder.count_matching(|c| matches!(c, Call::Save(_))), 2);
        assert_eq!(
            recorder.matching(|c| matches!(c, Call::Save(_))),
            vec![Call::Save("b"), Call::Save("c")]
        );
    }

    #[test]
    fn clones_share_the_log() {
        let recorder: Recorder<Call> = Recorder::new();
        let handle = recorder.clone();
        handle.record(Call::Get("a"));
        assert_eq!(recorder.len(), 1);

        recorder.clear();
        assert!(handle.is_empty());
    }
}
