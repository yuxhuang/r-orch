//! Init and permission refusals through the real binary and isolated daemons.
mod daemon_support;

use daemon_support::*;
use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::Path;
use std::process::Command;

fn mode(path: &Path) -> u32 {
    fs::symlink_metadata(path).unwrap().mode() & 0o777
}

// The shell, rather than the multithreaded test process, sets the mask.
fn under_umask(cmd: &Command, umask: &str) -> Command {
    let mut wrapped = Command::new("/bin/sh");
    wrapped
        .env_clear()
        .arg("-c")
        .arg(format!("umask {umask}; exec \"$0\" \"$@\""))
        .arg(cmd.get_program())
        .args(cmd.get_args())
        .current_dir(cmd.get_current_dir().unwrap());
    for (key, value) in cmd.get_envs() {
        if let Some(value) = value {
            wrapped.env(key, value);
        }
    }
    std::os::unix::process::CommandExt::process_group(&mut wrapped, 0);
    wrapped
}

fn command(sandbox: &Sandbox, root: &Path) -> Command {
    let bin = sandbox.short_dir().join("bin");
    fs::create_dir_all(&bin).unwrap();
    for name in ["claude", "codex", "opencode", "gemini", "cursor-agent"] {
        let shim = bin.join(name);
        fs::write(
            &shim,
            "#!/bin/sh\nprintf '%s\\n' \"$0 $*\" >> \"$TMPDIR/ai-calls\"\nexit 127\n",
        )
        .unwrap();
        fs::set_permissions(shim, fs::Permissions::from_mode(0o700)).unwrap();
    }
    let mut cmd = orch_command(root, &sandbox.socket(), sandbox.short_dir());
    cmd.env("TMPDIR", sandbox.short_dir())
        .env("PATH", format!("{}:/usr/bin:/bin", bin.display()));
    cmd
}

fn run(sandbox: &Sandbox, root: &Path, args: &[&str], mask: &str) -> std::process::Output {
    let mut cmd = command(sandbox, root);
    cmd.args(args);
    output_with_deadline(under_umask(&cmd, mask), CALL)
}

fn init_start_read_stop(mask: &str) {
    let sandbox = Sandbox::new();
    let root = sandbox.root();
    let out = run(&sandbox, &root, &["init", "--adapter", "shell"], mask);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(mode(&sandbox.state_dir()), 0o700, "init under {mask}");
    let mut cmd = command(&sandbox, &root);
    cmd.args(["daemon", "start"]);
    let mut daemon = DaemonChild::spawn(under_umask(&cmd, mask), sandbox.short_dir().join("d.err"));
    assert!(
        matches!(daemon.outcome(READY), Outcome::Ready(_)),
        "{}",
        daemon.stderr()
    );
    let read = run(&sandbox, &root, &["task", "list"], mask);
    assert!(read.status.success(), "{}", text(&read.stderr));
    let stop = run(&sandbox, &root, &["daemon", "stop"], mask);
    assert!(stop.status.success(), "{}", text(&stop.stderr));
    assert_eq!(daemon.wait_exit(READY).code(), Some(0));
    assert_eq!(mode(&sandbox.state_dir()), 0o700, "after stop under {mask}");
    assert!(!sandbox.short_dir().join("ai-calls").exists());
}

#[test]
fn t1_init_under_000() {
    init_start_read_stop("000");
}
#[test]
fn t1_init_under_002() {
    init_start_read_stop("002");
}
#[test]
fn t1_init_under_022() {
    init_start_read_stop("022");
}
#[test]
fn t1_init_under_077() {
    init_start_read_stop("077");
}

fn writable_project(sandbox: &Sandbox, root: &Path) {
    let init = run(sandbox, root, &["init", "--adapter", "shell"], "022");
    assert!(init.status.success(), "{}", text(&init.stderr));
    fs::set_permissions(root.join(".orchestry"), fs::Permissions::from_mode(0o775)).unwrap();
}

fn quoted(path: &Path) -> String {
    format!("'{}'", path.to_str().unwrap().replace('\'', "'\\''"))
}

fn assert_refusal(sandbox: &Sandbox, root: &Path, args: &[&str]) -> String {
    let out = run(sandbox, root, args, "002");
    let stderr = text(&out.stderr);
    eprintln!("{}: {stderr}", args.join(" "));
    assert_eq!(out.status.code(), Some(2), "{stderr}");
    assert!(
        stderr.contains(root.join(".orchestry").to_str().unwrap()),
        "{stderr}"
    );
    assert!(stderr.contains("775"), "{stderr}");
    assert!(
        stderr.contains(&format!("`chmod 700 {}`", quoted(&root.join(".orchestry")))),
        "{stderr}"
    );
    assert_eq!(mode(&root.join(".orchestry")), 0o775);
    assert!(!sandbox.socket().exists());
    assert!(!root.join(".orchestry/daemon.json").exists());
    stderr
}

#[test]
fn t2_start_refusal_gives_the_owner_command_without_changing_mode() {
    let sandbox = Sandbox::new();
    writable_project(&sandbox, &sandbox.root());
    assert_refusal(&sandbox, &sandbox.root(), &["daemon", "start"]);
}

#[test]
fn t3_client_refusal_gives_the_same_owner_command() {
    let sandbox = Sandbox::new();
    writable_project(&sandbox, &sandbox.root());
    let stderr = assert_refusal(&sandbox, &sandbox.root(), &["task", "list"]);
    assert!(
        stderr.contains("does not prove a daemon is absent"),
        "{stderr}"
    );
}

fn printed_commands_round_trip(sandbox: &Sandbox, root: &Path, stderr: &str) {
    for (prefix, replacement, expected) in [
        ("chmod 700 ", "printf %s ", root.join(".orchestry")),
        ("(cd ", "", root.to_path_buf()),
    ] {
        let advice = stderr
            .split('`')
            .find(|part| part.starts_with(prefix))
            .unwrap();
        let script = if replacement.is_empty() {
            advice.replace("orch daemon start", "printf %s \"$PWD\"")
        } else {
            advice.replacen(prefix, replacement, 1)
        };
        let output = Command::new("/bin/sh")
            .env_clear()
            .env("HOME", root.join("home"))
            .env("TMPDIR", sandbox.short_dir())
            .env("PATH", "/usr/bin:/bin")
            .current_dir(root)
            .args(["-c", &script])
            .output()
            .unwrap();
        assert!(output.status.success(), "{}", text(&output.stderr));
        assert_eq!(output.stdout, expected.as_os_str().as_encoded_bytes());
    }
}

#[test]
fn t4_advice_preserves_hostile_path_bytes_when_executed_by_posix_sh() {
    use std::os::unix::ffi::OsStringExt;
    for name in [
        std::ffi::OsString::from("owner's project; echo injected"),
        std::ffi::OsString::from("line\nnext"),
        std::ffi::OsString::from("trailing\n"),
        std::ffi::OsString::from_vec(b"raw-\xff'\n".to_vec()),
    ] {
        let sandbox = Sandbox::new();
        let root = sandbox.root().join(name);
        match fs::create_dir(&root) {
            Ok(()) => {}
            Err(error)
                if root.to_str().is_none() && error.raw_os_error() == Some(nix::libc::EILSEQ) =>
            {
                eprintln!(
                    "filesystem rejects non-UTF-8 names: {error}; raw-byte quoting is covered by unit tests"
                );
                continue;
            }
            Err(error) => panic!("create hostile project path: {error}"),
        }
        fs::create_dir(root.join("home")).unwrap();
        writable_project(&sandbox, &root);
        for args in [["daemon", "start"], ["task", "list"]] {
            let out = run(&sandbox, &root, &args, "002");
            let stderr = text(&out.stderr);
            assert_eq!(out.status.code(), Some(2), "{stderr}");
            printed_commands_round_trip(&sandbox, &root, &stderr);
            assert_eq!(mode(&root.join(".orchestry")), 0o775);
            assert!(!sandbox.socket().exists());
            assert!(!root.join(".orchestry/daemon.json").exists());
        }
    }
}

#[test]
fn t5_init_creation_failure_names_state_directory_and_preserves_dangling_link() {
    let sandbox = Sandbox::new();
    let root = sandbox.root();
    let state = sandbox.state_dir();
    let target = root.join("missing-state");
    std::os::unix::fs::symlink(&target, &state).unwrap();

    let out = run(&sandbox, &root, &["init", "--adapter", "shell"], "022");
    let stderr = text(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains(state.to_str().unwrap()), "{stderr}");
    assert!(
        fs::symlink_metadata(&state)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(fs::read_link(&state).unwrap(), target);
    assert!(!target.exists());
}
