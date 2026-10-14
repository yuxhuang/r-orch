//! Guards that keep a test inside its own tempdir.
//!
//! The project root is resolved by **walking up** from a starting directory,
//! so a tempdir with a real ORCH project above it silently becomes a test
//! against that project. On this machine `~/.orchestry` exists, and `cargo
//! test` runs with the cwd inside `$HOME` — so the walk-up reaches it. That is
//! how a test suite ends up writing `tasks/_index.json`, `agents/_index.json`
//! and `state.json` into the developer's own home.
//!
//! Every helper that builds a project directory for a test should call
//! [`assert_hermetic`] on it.

use std::path::Path;

/// The marker directory that makes an ancestor a project.
const ORCHESTRY_DIR: &str = ".orchestry";

/// Panic if `dir` has an `.orchestry` **ancestor**.
///
/// A project *inside* `dir` is fine and is usually the point; what must not
/// exist is one above it, which the walk-up would find first. Panics rather
/// than returning an error so a test helper cannot accidentally ignore it —
/// the failure mode being guarded against is silent.
pub fn assert_hermetic(dir: &Path) {
    let mut current = dir.parent();
    while let Some(path) = current {
        if path.join(ORCHESTRY_DIR).exists() {
            panic!(
                "{} has an `.orchestry` ancestor at {} — the project-root walk-up would \
                 escape this test and operate on that project instead. Point TMPDIR \
                 somewhere without one.",
                dir.display(),
                path.display()
            );
        }
        current = path.parent();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tempdir_with_no_project_above_it_passes() {
        let temp = tempfile::tempdir().unwrap();
        let nested = temp.path().join("a/b");
        std::fs::create_dir_all(&nested).unwrap();
        assert_hermetic(&nested);
    }

    #[test]
    fn a_project_inside_the_directory_is_fine() {
        // Which is the normal case: the test builds a project in its tempdir.
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(temp.path().join(ORCHESTRY_DIR)).unwrap();
        assert_hermetic(temp.path());
    }

    #[test]
    #[should_panic(expected = "has an `.orchestry` ancestor")]
    fn a_project_above_the_directory_panics() {
        let temp = tempfile::tempdir().unwrap();
        let nested = temp.path().join("a/b");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::create_dir_all(temp.path().join(ORCHESTRY_DIR)).unwrap();
        assert_hermetic(&nested);
    }
}
