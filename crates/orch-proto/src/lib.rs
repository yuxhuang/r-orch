//! The local daemon protocol: what crosses the project socket between the one
//! `orch` process that owns `.orchestry/` and every client (CLI, TUI, web, MCP).
//!
//! Design D1/D5/D6/D7 in the daemon-owned-state design and spec R6/R7/R20 are
//! the contract. This crate holds data and pure codecs only: no stores, no
//! services, no filesystem access and no process execution, so `orch-client`
//! can depend on it without gaining a store capability.
//!
//! - [`handshake`]: the **frozen** hello/welcome/reject envelope. It tolerates
//!   unknown fields so a stop intent and version diagnostics survive skew.
//! - [`frame`]: session frames after the handshake (requests, terminal
//!   `ok`/`err`, `part` streams, `ev` pushes) and their numeric limits.
//! - [`ops`]: one closed, typed request per operation in the D7 catalogue,
//!   its result type and its authoritative [`ops::OpClass`].
//! - [`error`]: the error envelope, exit codes and the uniform NoDaemon text.
//! - [`discovery`]: the `daemon.json` schema 1 record.
//! - [`ndjson`]: the bounded, transport-neutral newline-delimited JSON codec.

// Stricter than the workspace lints, for library code only (integration tests are separate crates and
// `clippy.toml` allows unwrap and print inside `#[cfg(test)]`). See AGENTS.md, "Rust coding standards".
#![deny(
    unreachable_pub,
    clippy::unwrap_used,
    clippy::print_stdout,
    clippy::print_stderr
)]

pub mod bounded;
pub mod discovery;
pub mod error;
pub mod frame;
pub mod handshake;
pub mod ndjson;
pub mod ops;
pub mod path;

/// Re-exported so [`for_each_op!`] can name domain result types from any crate.
#[doc(hidden)]
pub use orch_domain;

/// The local protocol revision. Starts at 1 and is bumped for **every**
/// operation wire or semantic change in the stack (spec R32): equal package
/// semver plus equal `LOCAL_PROTOCOL` is the only compatibility signal, and two
/// builds that differ in neither are deliberately indistinguishable.
#[cfg(not(feature = "skew-seam"))]
pub const LOCAL_PROTOCOL: u32 = 2;

/// Test seam (T2a, spec R32 vector 8.20): a build with `--features skew-seam`
/// takes its protocol revision from `ORCH_SEAM_LOCAL_PROTOCOL` at BUILD time,
/// so two binaries of one tree can disagree. Never enabled by default; without
/// the feature the constant above is 2 and the variable is ignored.
#[cfg(feature = "skew-seam")]
pub const LOCAL_PROTOCOL: u32 = seam_protocol(option_env!("ORCH_SEAM_LOCAL_PROTOCOL"));

#[cfg(feature = "skew-seam")]
const fn seam_protocol(raw: Option<&str>) -> u32 {
    let Some(raw) = raw else { return 2 };
    let bytes = raw.as_bytes();
    assert!(!bytes.is_empty(), "ORCH_SEAM_LOCAL_PROTOCOL is empty");
    let mut value = 0_u32;
    let mut i = 0;
    while i < bytes.len() {
        assert!(
            bytes[i].is_ascii_digit(),
            "ORCH_SEAM_LOCAL_PROTOCOL must be digits"
        );
        value = value * 10 + (bytes[i] - b'0') as u32;
        i += 1;
    }
    assert!(value > 0, "ORCH_SEAM_LOCAL_PROTOCOL must be positive");
    value
}

/// The exact package semver of this build. Ordinary sessions require it to be
/// equal on both sides (spec R6); only the frozen stop intent crosses skew.
pub const BINARY_VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
mod local_protocol_tests {
    use super::LOCAL_PROTOCOL;

    // Without the seam the constant is 2 whatever ORCH_SEAM_LOCAL_PROTOCOL says at
    // build time (run with `ORCH_SEAM_LOCAL_PROTOCOL=2 cargo test -p orch-proto`).
    #[cfg(not(feature = "skew-seam"))]
    #[test]
    fn feature_off_local_protocol_is_two_regardless_of_env() {
        assert_eq!(LOCAL_PROTOCOL, 2);
    }

    #[cfg(feature = "skew-seam")]
    mod seam {
        use super::LOCAL_PROTOCOL;
        use crate::seam_protocol;

        #[test]
        fn feature_on_env_unset_parses_to_two() {
            assert_eq!(seam_protocol(None), 2);
        }

        #[test]
        fn feature_on_env_two_parses_to_two() {
            assert_eq!(seam_protocol(Some("2")), 2);
        }

        #[test]
        fn feature_on_multi_digit_env_parses_positionally() {
            assert_eq!(seam_protocol(Some("10")), 10);
        }

        // Non-numeric, empty, zero and overflowing values panic inside a `const`
        // initialiser, so a seam build with such a value fails to COMPILE; that is
        // checked by hand in the T2a report, not here.
        #[test]
        fn feature_on_constant_follows_the_build_time_env() {
            let expected = match option_env!("ORCH_SEAM_LOCAL_PROTOCOL") {
                None => 2,
                Some(raw) => raw.parse::<u32>().unwrap(),
            };
            assert_eq!(LOCAL_PROTOCOL, expected);
        }
    }
}
