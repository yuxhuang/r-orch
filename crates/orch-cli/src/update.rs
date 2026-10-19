//! `orch update` — **amended**: no network, no self-replacement.
//!
//! The command, its `--check` flag and its exit code exist; it reports the
//! running version and how to install an update. There is no background
//! update check, which is why nothing here reads or writes
//! `~/.orchestry/update-check.json`.
//!
//! A CLI that silently downloads and overwrites its own executable as a side
//! effect of `orch task list` is a new supply-chain surface with no channel,
//! no signing and no atomic-replace story.

use crate::output::{Color, Styles};

/// The version reported by `--version`, and the one this command prints.
pub const VERSION: &str = crate::tree::VERSION;

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
    // r-orch ships through Homebrew or a source build, not npm.
    out.push_str("  To install the latest version:\n");
    out.push_str("    Homebrew:  brew upgrade r-orch\n");
    out.push_str("    Source:    git pull && cargo install --path crates/orch-cli\n");
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
            format!(
                "\n  orch update · version {VERSION}\n\
             \n  This build does not update itself.\n\
             \x20 To install the latest version:\n\
             \x20   Homebrew:  brew upgrade r-orch\n\
             \x20   Source:    git pull && cargo install --path crates/orch-cli\n\
             \n"
            )
        );
    }

    #[test]
    fn never_tells_the_user_to_use_npm() {
        // r-orch is not an npm package.
        for check in [false, true] {
            let out = render(&Styles::plain(), check);
            assert!(
                !out.contains("npm"),
                "npm is back in the update text:\n{out}"
            );
        }
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
        // A regression guard for the scope cut: if a future edit reintroduces a registry query or a self-install, it shows up
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
