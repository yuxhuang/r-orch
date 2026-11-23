//! What the shared scenarios cannot reach: the scope's debug form and states of
//! the reference that only a hand-built value shows.
#![allow(
    clippy::unwrap_used,
    reason = "test helper module; failures terminate the scenario"
)]

use super::*;

#[test]
fn a_scope_does_not_print_its_key() {
    let caller = Caller {
        project: "p".into(),
        principal: Principal::Owner {
            identity: "owner".into(),
        },
    };
    let scope = RequestScope::of(&caller, "super-secret-key-value").unwrap();
    let shown = format!("{scope:?}");
    assert!(!shown.contains("super-secret"), "{shown}");
    assert!(shown.contains("owner"), "{shown}");
}

#[test]
fn the_constants_state_the_spec() {
    assert_eq!(KEY_TTL_MS, 3_600_000);
    assert_eq!(MAX_RESULT_BYTES, 65_536);
    assert_eq!(MAX_ROWS, 1_000);
    assert_eq!((MIN_KEY_BYTES, MAX_KEY_BYTES), (16, 128));
}
