//! CLI context — resolved project root and global flags.

use std::path::PathBuf;

use crate::output::{StructuredFormat, Styles};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliContext {
    pub project_root: PathBuf,
    /// `--json` or `--yaml`; `None` for human output.
    pub output: Option<StructuredFormat>,
    pub quiet: bool,
    pub no_color: bool,
    pub ascii: bool,
}

/// What the environment says about presentation, read once so tests can pass
/// it explicitly instead of mutating globals.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EnvFlags {
    /// `NO_COLOR` is present at any value.
    pub no_color_env: bool,
    /// `TERM=dumb`.
    pub dumb_term: bool,
    /// stdout is a terminal. `chalk` emits no colour otherwise, and matching
    /// that keeps piped output identical between the two binaries.
    pub stdout_is_tty: bool,
    /// stdin is a terminal. The interactive surfaces — `init`'s adapter
    /// prompt and the agent shop picker — require **both** to be a TTY and
    /// silently take the non-interactive path otherwise.
    pub stdin_is_tty: bool,
}

impl EnvFlags {
    pub fn from_env() -> Self {
        EnvFlags {
            no_color_env: std::env::var_os("NO_COLOR").is_some(),
            dumb_term: std::env::var("TERM").is_ok_and(|term| term == "dumb"),
            stdout_is_tty: std::io::IsTerminal::is_terminal(&std::io::stdout()),
            stdin_is_tty: std::io::IsTerminal::is_terminal(&std::io::stdin()),
        }
    }
}

impl CliContext {
    pub fn new(
        project_root: PathBuf,
        output: Option<StructuredFormat>,
        quiet: bool,
        no_color_flag: bool,
        ascii_flag: bool,
        env: EnvFlags,
    ) -> Self {
        CliContext {
            project_root,
            output,
            quiet,
            no_color: no_color_flag || env.no_color_env,
            ascii: ascii_flag || env.dumb_term,
        }
    }

    /// Resolve the project root by walking up from `start_dir`.
    pub fn resolve(
        start_dir: &std::path::Path,
        output: Option<StructuredFormat>,
        quiet: bool,
        no_color_flag: bool,
        ascii_flag: bool,
        env: EnvFlags,
    ) -> Self {
        CliContext::new(
            project_root(start_dir),
            output,
            quiet,
            no_color_flag,
            ascii_flag,
            env,
        )
    }

    pub fn styles(&self, env: EnvFlags) -> Styles {
        Styles {
            color: !self.no_color && env.stdout_is_tty,
            ascii: self.ascii,
        }
    }
}

/// Root selection is discovery only; project state is read through the socket.
pub fn project_root(start_dir: &std::path::Path) -> PathBuf {
    std::env::var_os("ORCH_PROJECT_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            start_dir
                .ancestors()
                .find(|path| path.join(".orchestry").is_dir())
                .unwrap_or(start_dir)
                .to_path_buf()
        })
}

#[cfg(test)]
pub(crate) fn isolated_root_test(test_name: &str, check: impl FnOnce()) {
    const CHILD: &str = "ORCH_ROOT_TEST_CHILD";
    if std::env::var(CHILD).as_deref() == Ok(test_name) {
        check();
        return;
    }

    // Root discovery intentionally honours ORCH_PROJECT_ROOT. Re-exec only
    // this test with an explicit environment rather than racing other tests
    // by changing the runner's process-global environment.
    let sandbox = tempfile::tempdir().unwrap();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", test_name, "--nocapture"])
        .env_clear()
        .env(CHILD, test_name)
        .env("HOME", sandbox.path())
        .env("TMPDIR", sandbox.path())
        .env(
            "ORCHESTRY_GLOBAL_CONFIG_PATH",
            sandbox.path().join("global.yml"),
        )
        .current_dir(sandbox.path())
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success() && stdout.contains("test result: ok. 1 passed; 0 failed;"),
        "isolated {test_name}: {}\n{stdout}\n{stderr}",
        output.status
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(no_color: bool, dumb: bool, tty: bool) -> EnvFlags {
        EnvFlags {
            no_color_env: no_color,
            dumb_term: dumb,
            stdout_is_tty: tty,
            stdin_is_tty: tty,
        }
    }

    fn context(no_color_flag: bool, ascii_flag: bool, env: EnvFlags) -> CliContext {
        CliContext::new(
            PathBuf::from("/tmp/p"),
            None,
            false,
            no_color_flag,
            ascii_flag,
            env,
        )
    }

    #[test]
    fn no_color_comes_from_the_flag_or_the_environment() {
        assert!(!context(false, false, env(false, false, true)).no_color);
        assert!(context(true, false, env(false, false, true)).no_color);
        assert!(context(false, false, env(true, false, true)).no_color);
    }

    #[test]
    fn ascii_comes_from_the_flag_or_a_dumb_terminal() {
        assert!(!context(false, false, env(false, false, true)).ascii);
        assert!(context(false, true, env(false, false, true)).ascii);
        assert!(context(false, false, env(false, true, true)).ascii);
    }

    #[test]
    fn colour_is_off_whenever_stdout_is_not_a_terminal() {
        // Piped output carries no colour codes.
        let piped = env(false, false, false);
        assert!(!context(false, false, piped).styles(piped).color);

        let tty = env(false, false, true);
        assert!(context(false, false, tty).styles(tty).color);
        assert!(!context(true, false, tty).styles(tty).color);
    }

    #[test]
    fn styles_carry_the_ascii_choice_through() {
        let tty = env(false, false, true);
        assert!(context(false, true, tty).styles(tty).ascii);
    }

    #[test]
    fn resolve_walks_up_to_the_project_root() {
        isolated_root_test(
            "context::tests::resolve_walks_up_to_the_project_root",
            || {
                let temp = tempfile::tempdir().unwrap();
                let root = temp.path().join("proj");
                let nested = root.join("a/b");
                std::fs::create_dir_all(&nested).unwrap();
                std::fs::create_dir_all(root.join(".orchestry")).unwrap();

                let ctx = CliContext::resolve(
                    &nested,
                    Some(StructuredFormat::Yaml),
                    true,
                    false,
                    false,
                    EnvFlags::default(),
                );
                assert_eq!(ctx.project_root, root);
                assert_eq!(ctx.output, Some(StructuredFormat::Yaml));
                assert!(ctx.quiet);
            },
        );
    }

    #[test]
    fn root_discovery_tests_ignore_an_inherited_project_override() {
        use std::os::unix::fs::DirBuilderExt;

        let sandbox = tempfile::tempdir().unwrap();
        let ambient = sandbox.path().join("ambient");
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(ambient.join(".orchestry"))
            .unwrap();

        for test_name in [
            "context::tests::resolve_walks_up_to_the_project_root",
            "mcp::tests::the_root_override_selects_exactly_the_requested_project",
        ] {
            for project_override in [None, Some(&ambient)] {
                let mut command = std::process::Command::new(std::env::current_exe().unwrap());
                command
                    .args(["--exact", test_name, "--nocapture"])
                    .env_clear()
                    .env("HOME", sandbox.path())
                    .env("TMPDIR", sandbox.path())
                    .env(
                        "ORCHESTRY_GLOBAL_CONFIG_PATH",
                        sandbox.path().join("global.yml"),
                    )
                    .current_dir(sandbox.path());
                if let Some(root) = project_override {
                    command.env("ORCH_PROJECT_ROOT", root);
                }
                let output = command.output().unwrap();
                let stdout = String::from_utf8_lossy(&output.stdout);
                let stderr = String::from_utf8_lossy(&output.stderr);
                assert!(
                    output.status.success()
                        && stdout.contains("test result: ok. 1 passed; 0 failed;"),
                    "{test_name} with override {project_override:?}: {}\n{stdout}\n{stderr}",
                    output.status
                );
            }
        }
    }
}
