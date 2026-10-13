//! `orch update` — **amended**: no network, no self-replacement
//! (PARITY.md §11.5, recorded in §13 as out of scope).
//!
//! The reference queries npm for the latest version and, without `--check`,
//! runs `npm install -g @oxgeneral/orch@<latest>` over itself; a
//! cache-only variant of the same check also runs in the background after
//! almost every other command and prints a notice to stderr.
//!
//! None of that is reproduced. The command, its `--check` flag and its exit
//! code are preserved; it reports the running version and how to install an
//! update. The post-command background check is **removed entirely** — which
//! is also why nothing here reads or writes `~/.orchestry/update-check.json`.
//!
//! The reasoning is in PARITY.md §11.5: a CLI that silently downloads and
//! overwrites its own executable as a side effect of `orch task list` is a new
//! supply-chain surface with no channel, no signing and no atomic-replace
//! story, and it is unverifiable by the harness by construction — the oracle
//! queries npm and this would query nothing.

use crate::output::{Color, Styles};

/// The npm package the reference installs itself from, kept because it is what
/// a user has to type.
pub const PACKAGE_NAME: &str = "@oxgeneral/orch";

/// The version reported by `--version`, and the one this command prints.
pub const VERSION: &str = "1.0.34";

/// Render the report. `check` is accepted and makes no difference: with no
/// network there is nothing to check and nothing to install, so both forms
/// print the same thing and exit 0.
pub fn render(styles: &Styles, check: bool) -> String {
    let mut out = String::from("\n");
    out.push_str(&format!(
        "  {} · version {VERSION}\n",
        styles.paint(Color::Amber, "orch update")
    ));
    out.push('\n');
    out.push_str(&format!(
        "  {}\n",
        styles.dim("This build does not update itself.")
    ));
    out.push_str("  To install the latest version:\n");
    out.push_str(&format!("    npm install -g {PACKAGE_NAME}\n"));
    out.push('\n');
    // `--check` is part of the surface; saying so beats pretending the flag
    // changed something.
    if check {
        out.push_str(&format!(
            "  {}\n\n",
            styles.dim("--check: no registry query is made, so there is nothing to report.")
        ));
    }
    out
}

pub fn run(styles: &Styles, check: bool) {
    print!("{}", render(styles, check));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_the_version_and_how_to_install() {
        let out = render(&Styles::plain(), false);
        assert_eq!(
            out,
            "\n  orch update · version 1.0.34\n\
             \n  This build does not update itself.\n\
             \x20 To install the latest version:\n\
             \x20   npm install -g @oxgeneral/orch\n\
             \n"
        );
    }

    #[test]
    fn check_adds_a_line_and_changes_nothing_else() {
        let plain = render(&Styles::plain(), false);
        let checked = render(&Styles::plain(), true);
        assert!(checked.starts_with(plain.trim_end_matches('\n')));
        assert!(checked.contains("--check: no registry query is made"));
    }

    #[test]
    fn the_module_spawns_nothing_and_queries_nothing() {
        // A regression guard for the scope cut (PARITY.md §11.5): if a future
        // edit reintroduces a registry query or a self-install, it shows up
        // here rather than in a security review.
        let body = include_str!("update.rs")
            .split("#[cfg(test)]")
            .next()
            .expect("the module body");
        for forbidden in ["Command::new", "npm view", "reqwest", "TcpStream"] {
            assert!(
                !body.contains(forbidden),
                "{forbidden} is back in update.rs"
            );
        }
    }
}
