//! The id-generator seam (ARCHITECTURE §5.6). Tests cannot control a random
//! generator, so without this seam they could only assert id *prefixes*; with
//! it they can assert exact values.

/// The 64-character URL-safe alphabet shared by JS `nanoid`'s `urlAlphabet`
/// and Rust `nanoid::alphabet::SAFE`.
pub const ID_ALPHABET_LEN: usize = 64;

/// nanoid length used for every generated id.
pub const ID_LENGTH: usize = 7;

pub trait IdGenerator: Send + Sync {
    /// `<prefix>_<nanoid(7)>`. The prefix is passed without its underscore
    /// (`"tsk"`, `"agt"`, `"run"`, `"goal"`, `"team"`, `"msg"`).
    fn generate(&self, prefix: &str) -> String;
}

/// Whether an id has the contract shape: a prefix, `_`, then exactly 7
/// characters from the URL-safe alphabet (V29).
///
/// `agt_creator` is the one id `init` writes as a fixed string rather than
/// generating. It coincidentally has the contract shape —
/// `"creator"` is 7 alphanumeric characters — so this check cannot be used to
/// tell a generated id from the bootstrap one.
pub fn is_well_formed_id(id: &str, prefix: &str) -> bool {
    let Some(suffix) = id
        .strip_prefix(prefix)
        .and_then(|rest| rest.strip_prefix('_'))
    else {
        return false;
    };
    suffix.chars().count() == ID_LENGTH
        && suffix
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_the_reference_ids() {
        // Representative well-formed ids.
        assert!(is_well_formed_id("tsk_O5WBXtX", "tsk"));
        assert!(is_well_formed_id("agt_veWG_kK", "agt"));
        assert!(is_well_formed_id("run_rKQA8ah", "run"));
        assert!(is_well_formed_id("goal_717sF1B", "goal"));
        assert!(is_well_formed_id("team_tmmAHja", "team"));
        assert!(is_well_formed_id("msg_zMOeUPn", "msg"));
    }

    #[test]
    fn rejects_wrong_lengths_prefixes_and_characters() {
        assert!(!is_well_formed_id("tsk_O5WBXt", "tsk"));
        assert!(!is_well_formed_id("tsk_O5WBXtXX", "tsk"));
        assert!(!is_well_formed_id("tsk-O5WBXtX", "tsk"));
        assert!(!is_well_formed_id("agt_O5WBXtX", "tsk"));
        assert!(!is_well_formed_id("tsk_O5WBXt.", "tsk"));
        assert!(!is_well_formed_id("tsk_O5WB/tX", "tsk"));
        assert!(!is_well_formed_id("", "tsk"));
    }

    #[test]
    fn the_fixed_bootstrap_agent_id_also_has_the_contract_shape() {
        // "creator" is 7 alphanumeric characters, so a shape check alone does
        // not distinguish it from a generated id.
        assert!(is_well_formed_id("agt_creator", "agt"));
    }
}
