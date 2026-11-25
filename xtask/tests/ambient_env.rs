//! The ambient run's safety net, asserted from inside a test process.
//!
//! `.cargo/config.toml` redirects `ORCHESTRY_GLOBAL_CONFIG_PATH` and
//! `ORCH_DATA_HOME` into the build directory so no cargo-driven test can reach
//! the developer's real `~/.orchestry` (AGENTS.md, non-negotiable 5). That
//! redirect is Cargo's `[env]` table, and a test runner that builds through
//! Cargo but starts the test binaries itself has to apply it as well. This
//! test is what proves it does: it runs under whichever runner the ambient job
//! uses, and fails if the redirect did not arrive.
//!
//! The verification gate is the one context where the variables are absent on
//! purpose: it clears them and gives the suite its own sentinel `HOME`, which
//! its own witness asserts. That case is recognised by the witness directory
//! the gate sets, never by an unset variable alone, so a runner that simply
//! dropped the redirect cannot pass here.

use std::path::{Path, PathBuf};

const CONFIG: &str = include_str!("../../.cargo/config.toml");

fn workspace() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace root")
        .to_path_buf()
}

/// The one `NAME = { value = "...", relative = true }` line for `name`. This
/// reads the committed declaration, so the test cannot drift from it.
fn declared(name: &str) -> PathBuf {
    let prefix = format!("{name} = ");
    let lines: Vec<_> = CONFIG
        .lines()
        .filter(|line| line.starts_with(&prefix))
        .collect();
    assert_eq!(lines.len(), 1, "{name} must be declared exactly once");
    assert!(
        lines[0].contains("relative = true"),
        "{name} must be workspace-relative: {}",
        lines[0]
    );
    let value = lines[0]
        .split_once("value = \"")
        .expect("value")
        .1
        .split('"')
        .next()
        .expect("value end");
    assert!(!value.is_empty() && !value.starts_with('/'));
    workspace().join(value)
}

#[test]
fn the_cargo_env_redirects_reach_this_test_process_unless_the_gate_cleared_them() {
    let sentinel = std::env::var_os("XTASK_GATE_WITNESS_DIR");
    for name in ["ORCHESTRY_GLOBAL_CONFIG_PATH", "ORCH_DATA_HOME"] {
        let expected = declared(name);
        match std::env::var_os(name) {
            Some(value) => assert_eq!(
                Path::new(&value),
                expected,
                "{name} must be the workspace-relative redirect"
            ),
            None => assert!(
                sentinel.is_some(),
                "{name} is unset and this is not a gate run: the test runner did not apply \
                 .cargo/config.toml [env], so cargo-driven tests can reach the real ~/.orchestry"
            ),
        }
    }
}
