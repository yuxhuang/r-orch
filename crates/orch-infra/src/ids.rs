//! The real [`IdGenerator`]: `<prefix>_<nanoid(7)>` (PARITY.md §6.2).

use orch_core::ids::{ID_LENGTH, IdGenerator};

/// `nanoid::alphabet::SAFE` is the **same 64 characters** as JS nanoid's
/// `urlAlphabet`, so no custom alphabet is needed — only the length and the
/// prefix. Ids are therefore format-compatible with the TypeScript binary's.
#[derive(Debug, Clone, Copy, Default)]
pub struct NanoIdGenerator;

impl NanoIdGenerator {
    pub fn new() -> Self {
        NanoIdGenerator
    }
}

impl IdGenerator for NanoIdGenerator {
    fn generate(&self, prefix: &str) -> String {
        format!("{prefix}_{}", nanoid::nanoid!(ID_LENGTH))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use orch_core::ids::is_well_formed_id;
    use std::collections::HashSet;

    /// V29 — prefix plus 7 characters from the 64-char URL-safe alphabet.
    #[test]
    fn generates_contract_shaped_ids_for_every_prefix() {
        let ids = NanoIdGenerator::new();
        for prefix in ["tsk", "agt", "run", "goal", "team", "msg"] {
            for _ in 0..200 {
                let id = ids.generate(prefix);
                assert!(is_well_formed_id(&id, prefix), "{id}");
                assert_eq!(id.len(), prefix.len() + 1 + ID_LENGTH, "{id}");
            }
        }
    }

    #[test]
    fn the_alphabet_is_exactly_the_js_url_safe_set() {
        // JS nanoid's urlAlphabet, verbatim.
        const URL_ALPHABET: &str =
            "useandom-26T198340PX75pxJACKVERYMINDBUSHWOLF_GQZbfghjklqvwyzrict";
        let js: HashSet<char> = URL_ALPHABET.chars().collect();
        let rust: HashSet<char> = nanoid::alphabet::SAFE.iter().copied().collect();
        assert_eq!(js.len(), 64);
        assert_eq!(rust.len(), 64);
        assert_eq!(js, rust);
    }

    #[test]
    fn ids_are_not_repeated() {
        let ids = NanoIdGenerator::new();
        let generated: HashSet<String> = (0..1000).map(|_| ids.generate("tsk")).collect();
        // 64^7 is ~4.4e12, so a collision in 1000 draws would mean the RNG is
        // not random.
        assert_eq!(generated.len(), 1000);
    }

    #[test]
    fn is_usable_behind_a_shared_reference() {
        let ids: std::sync::Arc<dyn IdGenerator> = std::sync::Arc::new(NanoIdGenerator::new());
        assert!(ids.generate("tsk").starts_with("tsk_"));
    }
}
