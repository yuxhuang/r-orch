//! The pre-parse routing guard and the Commander-compatible failure mapping
//! (`docs/parity/cli.md` §1.5, §1.6, §3.5, §3.6).
//!
//! Three properties here are L0 and none of them is something clap does on its
//! own:
//!
//! 1. **The container is built before argument parsing.** In an uninitialized
//!    directory every command exits `3` — including `orch <sub> --help`, an
//!    unknown command and an unknown global option — because
//!    `NotInitializedError` is thrown while the container is constructed, which
//!    happens before Commander parses. Only `init`, `doctor` and `update` are
//!    exempt.
//! 2. **`--help`/`--version` with no real subcommand** never builds a
//!    container, so `orch --help` works in an empty directory.
//! 3. **Usage errors exit `1`**, not clap's `2`, and carry Commander's wording.

use std::ffi::OsStr;

use crate::tree::{ALL_KNOWN_COMMANDS, CONTAINER_LESS_COMMANDS};

/// What the argv says before clap looks at it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArgvShape {
    /// The first token that does not start with `-`, so
    /// `orch --json run tsk_1` still routes to `run`.
    pub sub: Option<String>,
    /// `--help`, `-h`, `--version` or `-V` appears anywhere.
    pub is_help_or_version: bool,
    /// `--version` or `-V` specifically: Commander prints the bare version
    /// string, where clap prefixes it with the program name.
    pub wants_version: bool,
    /// `process.argv.length <= 2` — no arguments at all, which is the
    /// first-run path rather than a guarded command.
    pub is_bare: bool,
}

impl ArgvShape {
    pub fn of<I, T>(args: I) -> Self
    where
        I: IntoIterator<Item = T>,
        T: AsRef<OsStr>,
    {
        let mut sub = None;
        let mut is_help_or_version = false;
        let mut wants_version = false;
        let mut count = 0;

        // Skip argv[0]: the binary name is `orch`, `orchestry` or `ao`.
        for arg in args.into_iter().skip(1) {
            count += 1;
            let arg = arg.as_ref().to_string_lossy().into_owned();
            if matches!(arg.as_str(), "--help" | "-h" | "--version" | "-V") {
                is_help_or_version = true;
            }
            if matches!(arg.as_str(), "--version" | "-V") {
                wants_version = true;
            }
            if sub.is_none() && !arg.starts_with('-') {
                sub = Some(arg);
            }
        }

        ArgvShape {
            sub,
            is_help_or_version,
            wants_version,
            is_bare: count == 0,
        }
    }

    /// Whether the first non-flag token names a command the reference knows.
    pub fn has_real_sub(&self) -> bool {
        self.sub
            .as_deref()
            .is_some_and(|sub| ALL_KNOWN_COMMANDS.contains(&sub))
    }

    /// `--help`/`--version` that must be answered without a container.
    pub fn is_help_fast_path(&self) -> bool {
        self.is_help_or_version && !self.has_real_sub()
    }

    /// Whether an uninitialized project is fatal for this invocation.
    ///
    /// `init`, `doctor` and `update` are the exemptions. A **bare** `orch` is
    /// also not fatal: the reference treats it as the first-run experience
    /// (init, then the TUI), which is WP-6's surface — until that lands the
    /// root help is printed instead. `orch --nope`, though, is *not* bare:
    /// it has no subcommand, so the reference builds the full container and
    /// reports `Not initialized` before the usage error (verified in §3.5).
    pub fn requires_initialized_project(&self) -> bool {
        if self.is_bare {
            return false;
        }
        match self.sub.as_deref() {
            None => true,
            Some(sub) => !CONTAINER_LESS_COMMANDS.contains(&sub),
        }
    }
}

/// Map a clap parse failure onto Commander's message and exit code.
///
/// Returns the exit code; the message is already printed. Commander exits `1`
/// for every usage error, where clap exits `2`, and its three documented
/// messages (`docs/parity/cli.md` §3.6) are reproduced verbatim because the
/// harness compares stderr byte for byte. Anything else keeps clap's own
/// rendering, which is help-shaped and therefore L2.
pub fn report_parse_error(err: &clap::Error) -> u8 {
    use clap::error::{ContextKind, ContextValue, ErrorKind};

    match err.kind() {
        ErrorKind::DisplayHelp
        | ErrorKind::DisplayVersion
        | ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand => {
            let _ = err.print();
            return 0;
        }
        ErrorKind::InvalidSubcommand => {
            if let Some(ContextValue::String(name)) = err.get(ContextKind::InvalidSubcommand) {
                eprintln!("error: unknown command '{name}'");
                return 1;
            }
        }
        ErrorKind::UnknownArgument => {
            if let Some(ContextValue::String(name)) = err.get(ContextKind::InvalidArg) {
                eprintln!("error: unknown option '{name}'");
                return 1;
            }
        }
        ErrorKind::MissingRequiredArgument => {
            if let Some(ContextValue::Strings(args)) = err.get(ContextKind::InvalidArg) {
                // Commander reports the first missing option, formatted as it
                // was declared: `required option '--adapter <adapter>' not
                // specified`.
                for arg in args {
                    if arg.starts_with("--") {
                        eprintln!("error: required option '{arg}' not specified");
                        return 1;
                    }
                }
            }
        }
        _ => {}
    }

    let _ = err.print();
    1
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shape(args: &[&str]) -> ArgvShape {
        ArgvShape::of(args)
    }

    #[test]
    fn the_subcommand_is_the_first_non_flag_token() {
        assert_eq!(
            shape(&["orch", "task", "list"]).sub.as_deref(),
            Some("task")
        );
        // Global flags before the subcommand are skipped.
        assert_eq!(
            shape(&["orch", "--json", "run", "tsk_1"]).sub.as_deref(),
            Some("run")
        );
        assert_eq!(shape(&["orch"]).sub, None);
        assert_eq!(shape(&["orch", "--json"]).sub, None);
    }

    #[test]
    fn a_flag_value_can_be_mistaken_for_the_subcommand_exactly_as_in_the_reference() {
        // `--name proj` puts `proj` first. The reference finds the same token
        // and routes nowhere, so an unknown command is reported either way.
        assert_eq!(
            shape(&["orch", "--name", "proj", "init"]).sub.as_deref(),
            Some("proj")
        );
    }

    #[test]
    fn help_and_version_are_detected_anywhere_in_the_argv() {
        for flag in ["--help", "-h", "--version", "-V"] {
            assert!(shape(&["orch", flag]).is_help_or_version, "{flag}");
            assert!(shape(&["orch", "task", flag]).is_help_or_version, "{flag}");
        }
        assert!(!shape(&["orch", "task", "list"]).is_help_or_version);
    }

    #[test]
    fn the_help_fast_path_applies_only_without_a_real_subcommand() {
        assert!(shape(&["orch", "--help"]).is_help_fast_path());
        assert!(shape(&["orch", "-V"]).is_help_fast_path());
        // A real subcommand means the container is built first, which is why
        // `orch logs --help` exits 3 in an uninitialized directory.
        assert!(!shape(&["orch", "logs", "--help"]).is_help_fast_path());
        assert!(!shape(&["orch", "task", "list"]).is_help_fast_path());
        // An unknown command is not a real subcommand.
        assert!(shape(&["orch", "frobnicate", "--help"]).is_help_fast_path());
    }

    #[test]
    fn a_bare_orch_is_the_first_run_path_but_a_stray_flag_is_not() {
        assert!(shape(&["orch"]).is_bare);
        assert!(!shape(&["orch"]).requires_initialized_project());

        // `orch --nope` has no subcommand but is not bare: the reference
        // builds the container and reports `Not initialized` (verified).
        assert!(!shape(&["orch", "--nope"]).is_bare);
        assert!(shape(&["orch", "--nope"]).requires_initialized_project());
        assert!(shape(&["orch", "--json"]).requires_initialized_project());
    }

    #[test]
    fn the_version_flags_are_distinguished_from_help() {
        assert!(shape(&["orch", "--version"]).wants_version);
        assert!(shape(&["orch", "-V"]).wants_version);
        assert!(!shape(&["orch", "--help"]).wants_version);
        assert!(shape(&["orch", "--help"]).is_help_or_version);
    }

    #[test]
    fn only_init_doctor_and_update_run_without_a_project() {
        for exempt in ["init", "doctor", "update"] {
            assert!(
                !shape(&["orch", exempt]).requires_initialized_project(),
                "{exempt}"
            );
        }
        for guarded in [
            "task", "agent", "status", "logs", "config", "context", "msg", "goal", "team", "org",
            "run", "tui", "serve",
        ] {
            assert!(
                shape(&["orch", guarded]).requires_initialized_project(),
                "{guarded}"
            );
        }
        // An unknown command is guarded too: the container is built before
        // parsing, so `orch frobnicate` reports `Not initialized`, not a usage
        // error.
        assert!(shape(&["orch", "frobnicate"]).requires_initialized_project());
        // `orch help task` likewise — `help` is not one of the seventeen.
        assert!(shape(&["orch", "help", "task"]).requires_initialized_project());
        // A bare `orch` is the first-run path, not a guarded command.
        assert!(!shape(&["orch"]).requires_initialized_project());
    }

    #[test]
    fn every_known_command_is_recognized() {
        for name in ALL_KNOWN_COMMANDS {
            assert!(shape(&["orch", name]).has_real_sub(), "{name}");
        }
        assert!(!shape(&["orch", "frobnicate"]).has_real_sub());
        assert!(!shape(&["orch", "help"]).has_real_sub());
    }

    #[test]
    fn a_usage_error_is_exit_one() {
        use crate::tree::Cli;
        use clap::Parser;

        let err = Cli::try_parse_from(["orch", "agent", "add", "a"]).expect_err("--adapter");
        assert_eq!(err.kind(), clap::error::ErrorKind::MissingRequiredArgument);
        assert_eq!(report_parse_error(&err), 1);

        let err = Cli::try_parse_from(["orch", "--nope"]).expect_err("unknown option");
        assert_eq!(report_parse_error(&err), 1);
    }

    #[test]
    fn help_and_version_are_exit_zero() {
        use crate::tree::Cli;
        use clap::Parser;

        let err = Cli::try_parse_from(["orch", "--version"]).expect_err("version exits");
        assert_eq!(report_parse_error(&err), 0);
    }
}
