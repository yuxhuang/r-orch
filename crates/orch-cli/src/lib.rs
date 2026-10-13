//! WP-5 — the clap command tree, output rendering and serve mode.
//!
//! Module layout follows ARCHITECTURE §3.3. Every module is a stub.

pub mod commands;
pub mod context;
pub mod editor;
pub mod output;
pub mod serve;
pub mod shop_picker;
pub mod update_check;

/// The single entry point behind the `orch`, `orchestry` and `ao` binaries.
///
/// WP-5 replaces this stub with the clap command tree. This is also the single
/// error boundary described in ARCHITECTURE §5.7: match on `orch_core::Error`,
/// print `  \u{2715} <msg>` (plus its hint) to stderr, and exit with
/// `exit_code()`.
pub fn run() -> std::process::ExitCode {
    eprintln!("  \u{2715} r-orch is not implemented yet (WP-5)");
    std::process::ExitCode::from(1)
}
