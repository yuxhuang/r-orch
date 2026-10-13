//! A deterministic id generator, so tests can assert exact ids instead of
//! prefixes (ARCHITECTURE §5.6).

use std::collections::HashMap;
use std::sync::Mutex;

use crate::ids::{ID_LENGTH, IdGenerator};

/// Produces `<prefix>_0000001`, `<prefix>_0000002`, … with a **separate
/// counter per prefix**, so a test can predict `tsk_0000001` without knowing
/// how many agents were created first.
///
/// The suffix is zero-padded to [`ID_LENGTH`], so generated ids satisfy
/// [`crate::ids::is_well_formed_id`] and exercise the same path lengths and
/// filename sanitation as real nanoids.
#[derive(Debug, Default)]
pub struct SeqIdGenerator {
    counters: Mutex<HashMap<String, u64>>,
    start: u64,
}

impl SeqIdGenerator {
    /// First id of every prefix is `_0000001`.
    pub fn new() -> Self {
        SeqIdGenerator::starting_at(1)
    }

    pub fn starting_at(start: u64) -> Self {
        SeqIdGenerator {
            counters: Mutex::new(HashMap::new()),
            start,
        }
    }

    /// How many ids have been handed out for a prefix.
    pub fn issued(&self, prefix: &str) -> u64 {
        let counters = self.counters.lock().unwrap_or_else(|e| e.into_inner());
        counters
            .get(prefix)
            .copied()
            .unwrap_or(self.start)
            .saturating_sub(self.start)
    }
}

impl IdGenerator for SeqIdGenerator {
    fn generate(&self, prefix: &str) -> String {
        let mut counters = self.counters.lock().unwrap_or_else(|e| e.into_inner());
        let next = counters.entry(prefix.to_owned()).or_insert(self.start);
        let value = *next;
        *next += 1;
        format!("{prefix}_{value:0width$}", width = ID_LENGTH)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::is_well_formed_id;

    #[test]
    fn counts_up_from_one() {
        let ids = SeqIdGenerator::new();
        assert_eq!(ids.generate("tsk"), "tsk_0000001");
        assert_eq!(ids.generate("tsk"), "tsk_0000002");
        assert_eq!(ids.generate("tsk"), "tsk_0000003");
    }

    #[test]
    fn each_prefix_has_its_own_counter() {
        let ids = SeqIdGenerator::new();
        assert_eq!(ids.generate("tsk"), "tsk_0000001");
        assert_eq!(ids.generate("agt"), "agt_0000001");
        assert_eq!(ids.generate("tsk"), "tsk_0000002");
        assert_eq!(ids.generate("run"), "run_0000001");
    }

    #[test]
    fn generated_ids_have_the_contract_shape() {
        let ids = SeqIdGenerator::new();
        for prefix in ["tsk", "agt", "run", "goal", "team", "msg"] {
            let id = ids.generate(prefix);
            assert!(is_well_formed_id(&id, prefix), "{id}");
        }
    }

    #[test]
    fn the_start_value_is_configurable() {
        let ids = SeqIdGenerator::starting_at(42);
        assert_eq!(ids.generate("tsk"), "tsk_0000042");
    }

    #[test]
    fn issued_counts_per_prefix() {
        let ids = SeqIdGenerator::new();
        assert_eq!(ids.issued("tsk"), 0);
        ids.generate("tsk");
        ids.generate("tsk");
        ids.generate("agt");
        assert_eq!(ids.issued("tsk"), 2);
        assert_eq!(ids.issued("agt"), 1);
        assert_eq!(ids.issued("run"), 0);
    }

    #[test]
    fn is_usable_behind_a_shared_reference() {
        let ids: std::sync::Arc<dyn IdGenerator> = std::sync::Arc::new(SeqIdGenerator::new());
        assert_eq!(ids.generate("msg"), "msg_0000001");
    }
}
