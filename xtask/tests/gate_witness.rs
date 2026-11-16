//! The same std-only witness is planted in disposable suite workspaces.

#[test]
fn gate_environment_witness() {
    let Some(directory) = std::env::var_os("XTASK_GATE_WITNESS_DIR") else {
        return;
    };
    let home = std::env::var_os("HOME").expect("gate supplies HOME");
    let path = std::env::var_os("PATH").expect("gate supplies PATH");
    let first = std::env::split_paths(&path).next().expect("nonempty PATH");
    let mut record = format!(
        "schema=1\nhome={}\nglobal_config_redirect={}\npath_first={}\n",
        std::path::Path::new(&home).display(),
        if std::env::var_os("ORCHESTRY_GLOBAL_CONFIG_PATH").is_some() {
            "set"
        } else {
            "unset"
        },
        first.display()
    );
    let removed = std::fs::read_to_string(std::path::Path::new(&directory).join("environment.txt"))
        .expect("gate supplies environment names");
    let mut names: Vec<_> = std::env::vars_os()
        .map(|(name, _)| name)
        .filter(|name| {
            let bytes = std::os::unix::ffi::OsStrExt::as_bytes(name.as_os_str());
            bytes.starts_with(b"ORCH_")
                || bytes.starts_with(b"ORCHESTRY_")
                || removed
                    .lines()
                    .any(|line| line.strip_prefix("removed ").map(str::as_bytes) == Some(bytes))
        })
        .collect();
    names.sort();
    for name in names {
        // No values, including credentials/prompts, are ever serialized.
        record.push_str("orch_var=");
        record.push_str(name.to_str().expect("unsupported environment name"));
        record.push('\n');
    }
    let directory = std::path::Path::new(&directory);
    std::fs::create_dir_all(directory).unwrap();
    std::fs::write(directory.join("witness.txt"), record).unwrap();
}

// FIXTURE SUPPORT: not planted in miniature workspaces.
// The assigned seven-file scope deliberately contains this shared std-only
// fixture owner. No production module or substitute verifier is imported.
#[allow(
    dead_code,
    reason = "each black-box binary uses a different subset of fixture operations"
)]
pub mod fixture {
    use std::ffi::{OsStr, OsString};
    use std::fs;
    use std::io::{Read, Write};
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    use std::os::unix::process::CommandExt;
    use std::path::{Path, PathBuf};
    use std::process::{Child, Command, ExitStatus, Stdio};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    pub const PIN: &str = "1.99.1";
    pub const HANDSHAKE_BUDGET: Duration = Duration::from_secs(30);
    pub const COMMAND_BUDGET: Duration = Duration::from_secs(30);
    pub const POLL_INTERVAL: Duration = Duration::from_millis(10);
    const TOOL_NAMES: [&str; 7] = [
        "cargo",
        "rustc",
        "rustdoc",
        "rustfmt",
        "cargo-fmt",
        "cargo-clippy",
        "clippy-driver",
    ];

    // Boundary parser only: global option values must not become subcommands.
    // The no-manifest fixture constraint rules out adding an argv-parser crate.
    pub const CARGO_SUBCOMMAND_PY: &str = r#"
def cargo_subcommand(args):
    values = {'--config', '--manifest-path', '--target-dir', '--color', '--explain', '-Z', '-C'}
    i = 0
    while i < len(args):
        arg = args[i]
        if arg == '--': return None
        if arg in values:
            i += 2
            continue
        if arg.startswith('-') or (i == 0 and arg.startswith('+')):
            i += 1
            continue
        return arg
    return None
"#;

    pub const AGENTS: [&str; 9] = [
        "claude",
        "codex",
        "cursor",
        "cursor-agent",
        "agent",
        "opencode",
        "pi",
        "grok",
        "agy",
    ];
    pub const STEPS: [&str; 9] = [
        "build",
        "test",
        "clippy",
        "fmt",
        "advisories",
        "crate-graph",
        "home-hygiene",
        "no-real-cli",
        "real-home",
    ];
    pub const SESSION: [&str; 12] = [
        "ORCH_AGENT_ID",
        "ORCH_AGENT_NAME",
        "ORCH_TASK_ID",
        "ORCH_SOCKET",
        "ORCH_PROJECT_ROOT",
        "ORCH_TUI_CLIENT",
        "ORCH_WEB_PASSWORD",
        "ORCH_DAEMON_DETACHED_CHILD",
        "ORCHESTRY_TASK_PROMPT",
        "ORCHESTRY_DEBUG",
        "ORCH_TUI_MOUSE",
        "ORCH_REMOTE_ENV_PASSTHROUGH",
    ];

    pub struct Sandbox {
        pub temporary: PathBuf,
        pub root: PathBuf,
    }

    impl Sandbox {
        pub fn new() -> Self {
            let result = Self::filesystem_only();
            result.git(&[
                "init",
                "--quiet",
                "--object-format=sha1",
                "--initial-branch=fixture",
            ]);
            result
        }

        // Retention/path controls need the real owned filesystem layout, but
        // must never start Git, discover installed tools or launch a child.
        pub fn filesystem_only() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let temporary = loop {
                let candidate = Path::new("/tmp").join(format!(
                    "gate-contract-{}-{}-{}",
                    std::process::id(),
                    SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap()
                        .as_nanos(),
                    NEXT.fetch_add(1, Ordering::Relaxed)
                ));
                match fs::DirBuilder::new().mode(0o700).create(&candidate) {
                    Ok(()) => break fs::canonicalize(candidate).unwrap(),
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                    Err(error) => panic!("setup: private fixture: {error}"),
                }
            };
            let result = Self {
                root: temporary.join("repo"),
                temporary,
            };
            for name in [
                "repo",
                "home",
                "tmp",
                "cargo-home",
                "rustup-home",
                "bin",
                "lease",
                "capture",
            ] {
                fs::create_dir(result.temporary.join(name)).unwrap();
            }
            fs::write(result.temporary.join("home/global.yml"), "{}\n").unwrap();
            fs::write(result.temporary.join("fallback-settings.toml"), "").unwrap();
            for agent in AGENTS {
                result.script(
                    &result.temporary.join("bin").join(agent),
                    "#!/bin/sh\nprintf '%s\\n' 'unsafe outer CLI boundary reached' >&2\nexit 126\n",
                );
            }
            result
        }

        pub fn write(&self, path: impl AsRef<Path>, content: impl AsRef<[u8]>) {
            let path = self.root.join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, content).unwrap();
        }

        pub fn script(&self, path: &Path, content: &str) {
            fs::write(path, content).unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
        }

        pub fn command(&self, executable: impl AsRef<OsStr>) -> Command {
            let mut command = Command::new(executable);
            command
                .current_dir(&self.root)
                .env_clear()
                .env("HOME", self.temporary.join("home"))
                .env("TMPDIR", self.temporary.join("tmp"))
                .env("CARGO_HOME", self.temporary.join("cargo-home"))
                .env("RUSTUP_HOME", self.temporary.join("rustup-home"))
                .env("RUSTUP_TOOLCHAIN", PIN)
                .env(
                    "RUSTUP_OVERRIDE_UNIX_FALLBACK_SETTINGS",
                    self.temporary.join("fallback-settings.toml"),
                )
                .env("CARGO_NET_OFFLINE", "true")
                .env("CARGO_BUILD_JOBS", "2")
                .env("RUSTUP_AUTO_INSTALL", "0")
                .env("RUSTUP_NO_UPDATE_CHECK", "1")
                .env("CARGO_TARGET_DIR", self.root.join("target/developer"))
                .env(
                    "ORCHESTRY_GLOBAL_CONFIG_PATH",
                    self.temporary.join("home/global.yml"),
                )
                .env("XTASK_GATE_LEASE_DIR", self.temporary.join("lease"))
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_TERMINAL_PROMPT", "0")
                .env("GIT_AUTHOR_NAME", "Synthetic Fixture")
                .env("GIT_AUTHOR_EMAIL", "fixture@example.invalid")
                .env("GIT_COMMITTER_NAME", "Synthetic Fixture")
                .env("GIT_COMMITTER_EMAIL", "fixture@example.invalid")
                .env("GIT_AUTHOR_DATE", "2000-01-01T00:00:00Z")
                .env("GIT_COMMITTER_DATE", "2000-01-01T00:00:00Z")
                .env("LC_ALL", "C")
                .env("TZ", "UTC")
                .env(
                    "PATH",
                    std::env::join_paths([
                        self.temporary.join("bin"),
                        PathBuf::from("/usr/bin"),
                        PathBuf::from("/bin"),
                        PathBuf::from("/usr/sbin"),
                        PathBuf::from("/sbin"),
                    ])
                    .unwrap(),
                );
            command
        }

        pub fn gate(&self, args: &[&str]) -> Command {
            let mut command = self.command(env!("CARGO_BIN_EXE_xtask"));
            command.arg("gate").args(args);
            command
        }

        pub fn run(&self, command: &mut Command) -> Capture {
            self.spawn(command).finish(COMMAND_BUDGET)
        }

        pub fn spawn(&self, command: &mut Command) -> OwnedChild {
            self.spawn_io(command, false, None)
        }

        pub fn spawn_with_closed_pipes(&self, command: &mut Command) -> OwnedChild {
            self.spawn_io(command, true, None)
        }

        pub fn spawn_with_input(&self, command: &mut Command, input: fs::File) -> OwnedChild {
            self.spawn_io(command, false, Some(input))
        }

        fn spawn_io(
            &self,
            command: &mut Command,
            closed: bool,
            input: Option<fs::File>,
        ) -> OwnedChild {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let serial = NEXT.fetch_add(1, Ordering::Relaxed);
            let stdout = self.temporary.join(format!("capture/{serial}.out"));
            let stderr = self.temporary.join(format!("capture/{serial}.err"));
            command
                .stdin(input.map(Stdio::from).unwrap_or_else(Stdio::null))
                .stdout(fs::File::create(&stdout).unwrap())
                .stderr(fs::File::create(&stderr).unwrap())
                .process_group(0);
            if closed {
                command.stdout(Stdio::piped()).stderr(Stdio::piped());
            }
            let mut child = command.spawn().expect("setup: start owned process");
            if closed {
                // Disconnect actual kernel pipes; no logger mock or production seam.
                drop(child.stdout.take());
                drop(child.stderr.take());
            }
            OwnedChild {
                child,
                stdout,
                stderr,
                reaped: false,
            }
        }

        pub fn git(&self, args: &[&str]) -> String {
            let output = self.run(self.command("/usr/bin/git").args(args));
            output.code(0);
            output.stdout.trim_end().to_owned()
        }

        pub fn blob(&self, bytes: &[u8]) -> String {
            // Input is a private regular file, avoiding unbounded pipe writes.
            let input = self.temporary.join("hash-input");
            fs::write(&input, bytes).unwrap();
            let mut command = self.command("/usr/bin/git");
            command
                .arg("hash-object")
                .arg("--no-filters")
                .arg("--")
                .arg(input);
            let result = self.run(&mut command);
            result.code(0);
            result.stdout.trim().to_owned()
        }

        pub fn commit(&self) -> String {
            self.git(&["add", "-f", "--", "."]);
            self.git(&[
                "-c",
                "commit.gpgsign=false",
                "commit",
                "--quiet",
                "--no-gpg-sign",
                "-m",
                "test: synthetic fixture",
            ]);
            self.git(&["rev-parse", "HEAD"])
        }

        pub fn tools(&self) {
            // Only tool locations/resources are inspected. No account config,
            // credentials, settings, product or AI directories are read or linked.
            let cwd = std::env::current_dir().unwrap();
            let absolute = |path: PathBuf| {
                if path.is_absolute() {
                    path
                } else {
                    cwd.join(path)
                }
            };
            let home = std::env::var_os("HOME")
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
                .map(absolute);
            let explicit_root = |name: &str| {
                std::env::var_os(name).map(|value| {
                    assert!(!value.is_empty(), "setup: empty {name} override");
                    let root = absolute(PathBuf::from(value));
                    assert!(root.is_dir(), "setup: invalid {name} override");
                    root
                })
            };
            let cargo_home =
                explicit_root("CARGO_HOME").or_else(|| home.as_ref().map(|p| p.join(".cargo")));
            let runtime_cargo = std::env::var_os("CARGO").map(PathBuf::from);
            let hints = [runtime_cargo, Some(PathBuf::from(env!("CARGO")))];
            let hinted_root = hints.iter().flatten().find_map(|path| {
                let cargo = fs::canonicalize(absolute(path.clone())).ok()?;
                if cargo.file_name()? != "cargo" {
                    return None;
                }
                let bin = cargo.parent()?;
                let toolchain = bin.parent()?;
                let toolchains = toolchain.parent()?;
                (bin.file_name()? == "bin" && toolchains.file_name()? == "toolchains")
                    .then(|| toolchains.parent().unwrap().to_path_buf())
            });
            let resource = explicit_root("RUSTUP_HOME")
                .or(hinted_root)
                .or_else(|| home.as_ref().map(|p| p.join(".rustup")))
                .expect("setup: no installed-tool root from RUSTUP_HOME, Cargo or HOME");
            assert!(
                resource.join("toolchains").is_dir(),
                "setup: selected Rustup root has no installed toolchains"
            );
            let mut candidates = Vec::new();
            if let Some(root) = cargo_home {
                candidates.push(root.join("bin/rustup"));
            }
            if let Some(path) = std::env::var_os("PATH") {
                candidates.extend(
                    std::env::split_paths(&path)
                        .filter(|p| p.is_absolute())
                        .map(|p| p.join("rustup")),
                );
            }
            let rustup = candidates
                .into_iter()
                .find_map(|candidate| {
                    let canonical = fs::canonicalize(candidate).ok()?;
                    [
                        canonical.clone(),
                        canonical.parent()?.join("../libexec/bin/rustup"),
                    ]
                    .into_iter()
                    .find_map(|path| {
                        let path = fs::canonicalize(path).ok()?;
                        let metadata = fs::metadata(&path).ok()?;
                        if !metadata.is_file() || metadata.permissions().mode() & 0o111 == 0 {
                            return None;
                        }
                        let mut magic = [0; 4];
                        fs::File::open(&path).ok()?.read_exact(&mut magic).ok()?;
                        let native = if cfg!(target_os = "macos") {
                            matches!(
                                magic,
                                [0xcf, 0xfa, 0xed, 0xfe]
                                    | [0xfe, 0xed, 0xfa, 0xcf]
                                    | [0xca, 0xfe, 0xba, 0xbe]
                                    | [0xca, 0xfe, 0xba, 0xbf]
                            )
                        } else {
                            magic == *b"\x7fELF"
                        };
                        native.then_some(path)
                    })
                })
                .expect("setup: native rustup missing; script wrappers are never executed");
            // Resolution uses a disposable toolchains-only view, not host settings.
            let discovery = self.temporary.join("discovery-rustup");
            fs::create_dir(&discovery).unwrap();
            std::os::unix::fs::symlink(resource.join("toolchains"), discovery.join("toolchains"))
                .unwrap();
            let mut version = self.command(&rustup);
            version.arg("--version").env("RUSTUP_HOME", &discovery);
            let output = self.run(&mut version);
            output.code(0);
            assert!(
                output.stdout.starts_with("rustup "),
                "setup: expected native rustup: {}",
                output.text()
            );
            let mut selected: Option<PathBuf> = None;
            for name in TOOL_NAMES {
                let mut command = self.command(&rustup);
                command
                    .args(["which", "--toolchain", PIN, name])
                    .env("RUSTUP_HOME", &discovery);
                let output = self.run(&mut command);
                output.code(0);
                assert_eq!(
                    output.stdout.lines().count(),
                    1,
                    "setup: rustup which output"
                );
                let tool = fs::canonicalize(output.stdout.trim())
                    .expect("setup: installed component path");
                assert_eq!(tool.file_name().unwrap(), name, "setup: wrong component");
                let root = tool.parent().unwrap().parent().unwrap().to_path_buf();
                assert!(
                    root.file_name()
                        .unwrap()
                        .to_string_lossy()
                        .starts_with(&format!("{PIN}-")),
                    "setup: wrong installed pin"
                );
                if let Some(previous) = &selected {
                    assert_eq!(&root, previous, "setup: mixed toolchains");
                }
                selected = Some(root);
            }
            let selected = selected.unwrap();
            let private = self.temporary.join("rustup-home/toolchains");
            fs::create_dir(&private).unwrap();
            std::os::unix::fs::symlink(&selected, private.join(selected.file_name().unwrap()))
                .unwrap();
            for name in TOOL_NAMES.into_iter().chain(["rustup"]) {
                std::os::unix::fs::symlink(&rustup, self.temporary.join("bin").join(name)).unwrap();
            }
            let cargo = self.run(self.command("cargo").arg("--version"));
            cargo.code(0);
            assert!(
                cargo.stdout.starts_with(&format!("cargo {PIN} ")),
                "setup: cargo pin: {}",
                cargo.text()
            );
            let rustc = self.run(self.command("rustc").arg("-vV"));
            rustc.code(0);
            assert!(
                rustc.stdout.lines().any(|l| l == format!("release: {PIN}")),
                "setup: rustc pin"
            );
            let host = rustc
                .stdout
                .lines()
                .find_map(|l| l.strip_prefix("host: "))
                .expect("setup: rustc host");
            assert_eq!(
                selected.file_name().unwrap(),
                OsStr::new(&format!("{PIN}-{host}")),
                "setup: host mismatch"
            );
            let arch = std::env::consts::ARCH;
            assert!(
                host.starts_with(&format!("{arch}-")),
                "setup: nonnative architecture"
            );
            assert!(
                host.contains(if cfg!(target_os = "macos") {
                    "apple-darwin"
                } else {
                    "linux"
                }),
                "setup: nonnative OS"
            );
        }

        pub fn cargo_forwarder(&self) -> PathBuf {
            let directory = self.temporary.join("forward");
            fs::create_dir(&directory).unwrap();
            let forwarded = directory.join("cargo");
            fs::rename(self.temporary.join("bin/cargo"), &forwarded).unwrap();
            forwarded
        }

        pub fn mini(&self, body: &str, witness: bool) {
            self.tools();
            self.write(".gitignore", "/target/\n");
            self.write(
                "Cargo.toml",
                "[workspace]\nresolver = \"3\"\nmembers = [\"fixture\"]\n",
            );
            self.write(
                "Cargo.lock",
                "version = 4\n\n[[package]]\nname = \"fixture\"\nversion = \"0.0.0\"\n",
            );
            self.write("rust-toolchain.toml", "[toolchain]\nchannel = \"1.99.1\"\n");
            self.write(".cargo/config.toml", "[env]\nORCHESTRY_GLOBAL_CONFIG_PATH = { value = \"target/test-global/global.yml\", relative = true }\n");
            self.write(
                "fixture/Cargo.toml",
                "[package]\nname = \"fixture\"\nversion = \"0.0.0\"\nedition = \"2024\"\n",
            );
            self.write("fixture/src/lib.rs", body);
            if witness {
                self.write(
                    "fixture/tests/gate_witness.rs",
                    include_str!("gate_witness.rs")
                        .split("// FIXTURE SUPPORT:")
                        .next()
                        .unwrap(),
                );
            }
            self.commit();
        }

        pub fn result(&self) -> String {
            fs::read_to_string(self.root.join("target/gate-result.txt"))
                .expect("oracle: gate did not retain target/gate-result.txt")
        }

        pub fn suite(&self) -> Capture {
            self.run(&mut self.gate(&["pr", "--part", "suite", "--lease-wait", "0"]))
        }
    }

    impl Default for Sandbox {
        fn default() -> Self {
            Self::new()
        }
    }

    impl Drop for Sandbox {
        fn drop(&mut self) {
            let unwinding = std::thread::panicking();
            if unwinding || self.temporary.join("cleanup-unresolved").exists() {
                // A diagnostic write failure must not panic again during unwind.
                // Retention preserves evidence; it establishes no writer death.
                let reason = if unwinding {
                    "test unwinding"
                } else {
                    "unresolved child lifecycle"
                };
                let _ = writeln!(
                    std::io::stderr(),
                    "fixture retained: {reason} at {}",
                    self.temporary.display()
                );
                return;
            }
            let _ = fs::remove_dir_all(&self.temporary);
        }
    }

    pub struct Capture {
        pub status: ExitStatus,
        pub stdout: String,
        pub stdout_bytes: Vec<u8>,
        pub stderr: String,
        pub stderr_bytes: Vec<u8>,
    }

    impl Capture {
        pub fn code(&self, expected: i32) {
            assert_eq!(self.status.code(), Some(expected), "{}", self.text());
        }
        pub fn text(&self) -> String {
            format!("{}\n{}", self.stdout, self.stderr)
        }
        pub fn contains(&self, text: &str) {
            assert!(
                self.text().contains(text),
                "missing {text:?}: {}",
                self.text()
            );
        }
    }

    // The approved std-only/no-manifest fixture constraint rules out adopting
    // process-wrap here. Child handles pin the direct group; bounded socket
    // workers do NOT establish authoritative death/cleanup for detached leaves.
    // Those lifecycle cases remain safety-held (FS2/I3), regardless of EOF.
    pub struct OwnedChild {
        pub child: Child,
        stdout: PathBuf,
        stderr: PathBuf,
        reaped: bool,
    }

    impl OwnedChild {
        pub fn diagnostic(&self) -> String {
            format!(
                "stdout:\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&fs::read(&self.stdout).unwrap()),
                String::from_utf8_lossy(&fs::read(&self.stderr).unwrap())
            )
        }
        pub fn signal(&mut self, signal: &str) {
            assert!(
                self.child.try_wait().unwrap().is_none(),
                "setup: holder exited before signal"
            );
            assert!(
                self.signal_owned(signal, false),
                "setup: direct owned-child signal failed"
            );
        }
        fn signal_owned(&self, signal: &str, group: bool) -> bool {
            // Caller retains an unreaped direct-child handle. Never call after
            // try_wait returned Some, never use discovered/recovery-file IDs.
            let id = if group {
                format!("-{}", self.child.id())
            } else {
                self.child.id().to_string()
            };
            let Ok(mut sender) = Command::new("/bin/kill")
                .env_clear()
                .args(["-s", signal, "--", &id])
                .spawn()
            else {
                return false;
            };
            let deadline = Instant::now() + Duration::from_secs(2);
            loop {
                match sender.try_wait() {
                    Ok(Some(status)) => return status.success(),
                    Ok(None) if Instant::now() < deadline => {
                        std::thread::park_timeout(POLL_INTERVAL)
                    }
                    _ => {
                        let _ = sender.kill();
                        let cleanup_deadline = Instant::now() + Duration::from_secs(2);
                        while Instant::now() < cleanup_deadline {
                            if matches!(sender.try_wait(), Ok(Some(_))) {
                                return false;
                            }
                            std::thread::park_timeout(POLL_INTERVAL);
                        }
                        eprintln!("fixture cleanup: signal helper could not be joined");
                        return false;
                    }
                }
            }
        }

        pub fn finish(mut self, budget: Duration) -> Capture {
            let deadline = Instant::now() + budget;
            let status = loop {
                if let Some(status) = self.child.try_wait().unwrap() {
                    break status;
                }
                assert!(
                    Instant::now() < deadline,
                    "timeout: owned child did not finish within {budget:?}"
                );
                for path in [&self.stdout, &self.stderr] {
                    assert!(
                        fs::metadata(path).unwrap().len() < 8 * 1024 * 1024,
                        "fixture output bound exceeded"
                    );
                }
                std::thread::park_timeout(POLL_INTERVAL);
            };
            self.reaped = true;
            for path in [&self.stdout, &self.stderr] {
                assert!(
                    fs::metadata(path).unwrap().len() < 8 * 1024 * 1024,
                    "fixture output bound exceeded after child exit"
                );
            }
            let stdout_bytes = fs::read(&self.stdout).unwrap();
            let stderr_bytes = fs::read(&self.stderr).unwrap();
            Capture {
                status,
                stdout: String::from_utf8_lossy(&stdout_bytes).into_owned(),
                stderr: String::from_utf8_lossy(&stderr_bytes).into_owned(),
                stdout_bytes,
                stderr_bytes,
            }
        }
    }

    impl Drop for OwnedChild {
        fn drop(&mut self) {
            if self.reaped {
                return;
            }
            // On abnormal completion retain scratch rather than removing it
            // underneath an unresolved writer. This is containment of files,
            // not a claim that escaped descendants have been stopped (FS2/I3).
            let root = self.stdout.parent().unwrap().parent().unwrap();
            let _ = fs::write(
                root.join("cleanup-unresolved"),
                b"abnormal direct-child completion; descendant authority unresolved\n",
            );
            if matches!(self.child.try_wait(), Ok(None)) {
                let _ = self.signal_owned("KILL", true);
                let _ = self.child.kill();
                let deadline = Instant::now() + Duration::from_secs(2);
                loop {
                    match self.child.try_wait() {
                        Ok(Some(_)) => break,
                        Ok(None) if Instant::now() < deadline => {
                            std::thread::park_timeout(POLL_INTERVAL)
                        }
                        _ => {
                            eprintln!(
                                "fixture cleanup: direct child could not be joined; scratch retained"
                            );
                            break;
                        }
                    }
                }
            }
            // A reaped leader supplies no authority over survivors. No numeric
            // PID/PGID signal follows that branch; independent seam still due.
        }
    }

    pub fn field<'a>(record: &'a str, key: &str) -> &'a str {
        record
            .lines()
            .find_map(|line| line.strip_prefix(&format!("{key}:")))
            .unwrap_or_else(|| panic!("oracle: missing {key}: in {record}"))
            .trim()
    }

    pub fn row<'a>(record: &'a str, step: &str) -> Vec<&'a str> {
        record
            .lines()
            .filter(|line| line.starts_with(&format!("{step}\t")))
            .map(|line| line.split('\t').collect::<Vec<_>>())
            .next()
            .unwrap_or_else(|| panic!("oracle: missing row {step}: {record}"))
    }

    pub fn wait_for(path: &Path, budget: Duration) {
        let deadline = Instant::now() + budget;
        while !path.exists() {
            assert!(
                Instant::now() < deadline,
                "handshake missing: {}",
                path.display()
            );
            std::thread::park_timeout(POLL_INTERVAL);
        }
    }

    pub fn backdate(path: &Path) {
        fs::File::options()
            .read(true)
            .open(path)
            .unwrap()
            .set_times(
                fs::FileTimes::new().set_modified(UNIX_EPOCH + Duration::from_secs(946684800)),
            )
            .unwrap();
    }

    pub fn rust_literal(path: &Path) -> String {
        format!("{:?}", path.to_str().unwrap())
    }
    pub fn os_args(values: &[&str]) -> Vec<OsString> {
        values.iter().map(OsString::from).collect()
    }
}

#[test]
fn filesystem_sandbox_removes_scratch_on_normal_drop() {
    let sandbox = fixture::Sandbox::filesystem_only();
    let temporary = sandbox.temporary.clone();
    std::fs::write(temporary.join("diagnostic"), "normal\n").unwrap();
    drop(sandbox);
    assert!(!temporary.exists());
}

#[test]
fn filesystem_sandbox_retains_diagnostics_while_unwinding() {
    let sandbox = fixture::Sandbox::filesystem_only();
    let temporary = sandbox.temporary.clone();
    std::fs::write(temporary.join("diagnostic"), "panic evidence\n").unwrap();
    let caught = std::panic::catch_unwind(move || {
        let _sandbox = sandbox;
        panic!("controlled filesystem-only unwind");
    });
    assert!(caught.is_err());
    assert_eq!(
        std::fs::read_to_string(temporary.join("diagnostic")).unwrap(),
        "panic evidence\n"
    );
    // This control started no writers; only its retained disposable tree is removed.
    std::fs::remove_dir_all(temporary).unwrap();
}

#[test]
fn filesystem_sandbox_retains_diagnostics_when_cleanup_is_unresolved() {
    let sandbox = fixture::Sandbox::filesystem_only();
    let temporary = sandbox.temporary.clone();
    std::fs::write(temporary.join("diagnostic"), "unresolved evidence\n").unwrap();
    std::fs::write(temporary.join("cleanup-unresolved"), "control\n").unwrap();
    drop(sandbox);
    assert_eq!(
        std::fs::read_to_string(temporary.join("diagnostic")).unwrap(),
        "unresolved evidence\n"
    );
    // The marker is synthetic and no process was started by this control.
    std::fs::remove_dir_all(temporary).unwrap();
}

#[test]
fn supplied_witness_directory_records_home_redirect_path_and_names_without_values() {
    let sandbox = fixture::Sandbox::new();
    let directory = sandbox.temporary.join("witness-record");
    std::fs::create_dir(&directory).unwrap();
    std::fs::write(
        directory.join("environment.txt"),
        "removed XDG_CONFIG_HOME\n",
    )
    .unwrap();
    let module = module_path!().split_once("::").map(|(_, suffix)| suffix);
    let test = module.map_or_else(
        || "gate_environment_witness".to_owned(),
        |prefix| format!("{prefix}::gate_environment_witness"),
    );
    let mut command = sandbox.command(std::env::current_exe().unwrap());
    command
        .args(["--exact", &test, "--test-threads=1"])
        .env("XTASK_GATE_WITNESS_DIR", &directory)
        .env("XDG_CONFIG_HOME", "SECRET_private_xdg")
        .env("ORCH_WEB_PASSWORD", "SECRET_private_password");
    sandbox.run(&mut command).code(0);
    assert_eq!(
        std::fs::read_to_string(directory.join("witness.txt")).unwrap(),
        format!(
            "schema=1\nhome={}\nglobal_config_redirect=set\npath_first={}\norch_var=ORCHESTRY_GLOBAL_CONFIG_PATH\norch_var=ORCH_WEB_PASSWORD\norch_var=XDG_CONFIG_HOME\n",
            sandbox.temporary.join("home").display(),
            sandbox.temporary.join("bin").display()
        )
    );
}

#[test]
fn capture_preserves_non_utf8_output_bytes_and_prints_lossy_diagnostics() {
    let sandbox = fixture::Sandbox::new();
    let output = sandbox.run(sandbox.command("/usr/bin/python3").args([
        "-I",
        "-B",
        "-c",
        "import os; os.write(1, b'out\\xff'); os.write(2, b'err\\xfe')",
    ]));
    output.code(0);
    assert_eq!(
        (
            output.stdout_bytes.as_slice(),
            output.stderr_bytes.as_slice()
        ),
        (b"out\xff".as_slice(), b"err\xfe".as_slice())
    );
    assert!(output.text().contains('\u{fffd}'));
}

#[test]
fn cargo_boundary_recognizes_subcommands_after_global_options_but_not_option_values() {
    let sandbox = fixture::Sandbox::new();
    let cases = [
        (vec!["build"], Some("build")),
        (vec!["--locked", "build", "--workspace"], Some("build")),
        (vec!["+1.99.1", "--config", "build", "test"], Some("test")),
        (vec!["--color=always", "test"], Some("test")),
        (
            vec!["--manifest-path", "build", "metadata"],
            Some("metadata"),
        ),
        (vec!["--", "test"], None),
        (vec!["--config", "test"], None),
        (vec![], None),
    ];
    for (args, expected) in cases {
        let program = format!(
            "import sys, json\n{}\nprint(json.dumps(cargo_subcommand(sys.argv[1:])))",
            fixture::CARGO_SUBCOMMAND_PY
        );
        let output = sandbox.run(
            sandbox
                .command("/usr/bin/python3")
                .args(["-I", "-B", "-c", &program])
                .args(&args),
        );
        output.code(0);
        let actual: Option<String> = serde_json::from_str(output.stdout.trim()).unwrap();
        assert_eq!(actual.as_deref(), expected, "argv={args:?}");
    }
}

#[test]
fn installed_pin_is_discovered_without_a_private_resource_variable() {
    let sandbox = fixture::Sandbox::new();
    sandbox.tools();
    let output = sandbox.run(sandbox.command("cargo").arg("--version"));
    output.code(0);
    assert!(
        output.stdout.starts_with("cargo 1.99.1 "),
        "{}",
        output.text()
    );
    assert!(!sandbox.temporary.join("cargo-home/config.toml").exists());
    assert!(
        !sandbox
            .temporary
            .join("cargo-home/credentials.toml")
            .exists()
    );
    // TV6 control uses a real rustup proxy: empty child home cannot fall back
    // to installed resources, absolute toolchain paths or direct Cargo.
    let empty = sandbox.temporary.join("empty-rustup");
    std::fs::create_dir(&empty).unwrap();
    let output = sandbox.run(
        sandbox
            .command("cargo")
            .arg("--version")
            .env("RUSTUP_HOME", empty),
    );
    assert!(
        !output.status.success(),
        "empty Rustup home was bypassed: {}",
        output.text()
    );
}
