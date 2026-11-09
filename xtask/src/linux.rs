//! `cargo xtask linux`: run the Linux checks from macOS in a linux/amd64 container.
//!
//! The image is the official `rust:<channel>-trixie`, with `<channel>` read from
//! `rust-toolchain.toml`, plus tmux and the toolchain's rustfmt and clippy. It
//! is built locally as `orch-linux:<channel>` and never pushed. AGENTS.md
//! ("Linux checks on macOS") says when to use it and how to report it.
//!
//! The container gets the checkout read-only at its own path (a linked
//! worktree's git directory too), a per-checkout volume over `target/`, a
//! shared cargo-registry volume, and no host environment. Commands run as the
//! caller's uid, not root, because the suite has tests that expect a read-only
//! directory to refuse a write, and root ignores that.

use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};

/// Exit code when nothing ran: no Docker or an unusable toolchain pin (report
/// the Linux checks as `env-limited` then), a container already running for
/// this checkout, or `shell` without a terminal.
const CANNOT_RUN: i32 = 2;

const PLATFORM: &str = "linux/amd64";
const LABEL: &str = "org.r-orch.linux=1";
const REGISTRY_VOLUME: &str = "orch-linux-cargo-registry";

#[derive(Debug, Args)]
pub(crate) struct LinuxArgs {
    #[command(subcommand)]
    command: Option<LinuxCommand>,
}

#[derive(Debug, Clone, PartialEq, Eq, Subcommand)]
enum LinuxCommand {
    /// The CI set (the default): Cargo.lock check, fmt, clippy, test, crate-graph.
    Ci,
    /// Run one command in the container, e.g. `run cargo test -p orch-infra`.
    Run {
        #[arg(required = true, trailing_var_arg = true, allow_hyphen_values = true)]
        command: Vec<String>,
    },
    /// An interactive shell in the container (needs a terminal).
    Shell,
    /// Delete this checkout's target volume; `--all` deletes every orch-linux volume.
    Clean {
        #[arg(long)]
        all: bool,
    },
}

/// The checks `cargo xtask linux` runs by default, in CI's order
/// (`.github/workflows/ci.yml`). cargo-deny is not here: deny.toml's
/// `[graph] targets` already selects the Linux target, so the host run sees
/// the Linux graph, and CI always runs it.
const CI_STEPS: &[&str] = &[
    "test -s Cargo.lock && ! grep -qE '^name = \"(terminfo|termwiz)\"$' Cargo.lock",
    "cargo fmt --all --check",
    "cargo clippy --workspace --all-targets -- -D warnings",
    "cargo xtask crate-graph",
    "cargo test --workspace --no-fail-fast",
];

/// A behaviour of the container's platform, not of orch, that breaks tests
/// which pass on Linux CI. The default run probes for each one before the
/// tests and skips only the tests that depend on what it finds, so it follows
/// whatever amd64 emulator Docker uses instead of assuming one. Both were seen
/// under Docker Desktop's amd64 emulation on Apple silicon (engine 29.9.2,
/// 2026-11-07); neither shows in a native linux/arm64 container.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Quirk {
    /// glibc's `posix_spawnp` of a missing binary returns a pid and the child
    /// exits 127, where Linux reports ENOENT. Rust's `Command` uses
    /// `posix_spawn`, so "missing binary" tests see a started child.
    SpawnStartsMissingBinary,
    /// Starting a process writes into `$HOME` (seen: an empty `.cache/…`
    /// directory, even with `XDG_CACHE_HOME` set), so a test that asserts HOME
    /// stays empty fails. `./xtask/gate.sh`'s home-hygiene step cannot pass on
    /// such a platform either.
    ProcessWritesHome,
}

impl Quirk {
    const ALL: [Quirk; 2] = [Quirk::SpawnStartsMissingBinary, Quirk::ProcessWritesHome];

    /// The shell variable its probe sets to 1 when the platform shows it.
    fn var(self) -> &'static str {
        match self {
            Quirk::SpawnStartsMissingBinary => "spawn_quirk",
            Quirk::ProcessWritesHome => "home_quirk",
        }
    }

    /// What the run prints when the probe finds it.
    fn found(self) -> &'static str {
        match self {
            Quirk::SpawnStartsMissingBinary => {
                "spawning a missing binary starts a child here instead of failing"
            }
            Quirk::ProcessWritesHome => "starting a process here writes into HOME ($home_found)",
        }
    }

    /// Shell that sets `var` to 1 when the platform shows the behaviour, else 0.
    /// It tests the platform, never the code under test.
    fn probe(self) -> &'static str {
        match self {
            Quirk::SpawnStartsMissingBinary => SPAWN_PROBE,
            Quirk::ProcessWritesHome => HOME_PROBE,
        }
    }
}

/// `-I` (isolated mode): the probe runs in the checkout, so without it a
/// `sitecustomize.py` there could decide the result.
const SPAWN_PROBE: &str = r#"spawn_quirk=0
if ! command -v python3 >/dev/null; then
  echo "xtask linux: warning: no python3, so the spawn probe cannot run" >&2
elif python3 -I -c '
import os, sys
name = "orch-xtask-no-such-binary"
try:
    pid = os.posix_spawnp(name, [name], {})
except OSError:
    sys.exit(1)
os.waitpid(pid, 0)
'; then
  spawn_quirk=1
fi
"#;

const HOME_PROBE: &str = r#"home_quirk=0
home_found=''
probe_home=$(mktemp -d)
# `env` and `true` are both external programs, so two processes start here.
HOME=$probe_home env true || true
if [ -n "$(ls -A "$probe_home")" ]; then
  home_quirk=1
  home_found=$(cd "$probe_home" && find . -mindepth 1 | sed 's|^\./||' | LC_ALL=C tr -c '[:print:]\n' '?' | paste -s -d ' ' -)
fi
rm -rf "$probe_home"
"#;

/// Tests skipped when the probe finds their behaviour, with the file that
/// holds each. Skipped by exact name; Linux CI runs them all.
const PLATFORM_SKIPS: &[(Quirk, &str, &str)] = &[
    (
        Quirk::SpawnStartsMissingBinary,
        "editor::tests::a_missing_editor_binary_is_an_error",
        "crates/orch-cli/src/editor.rs",
    ),
    (
        Quirk::ProcessWritesHome,
        "listing_showing_and_agent_listing_are_read_only_and_create_nothing_new",
        "crates/orch-cli/tests/org_templates.rs",
    ),
    (
        Quirk::SpawnStartsMissingBinary,
        "review_runner::tests::the_process_runner_reports_a_missing_binary_as_a_failure",
        "crates/orch-engine/src/review_runner.rs",
    ),
    (
        Quirk::SpawnStartsMissingBinary,
        "adapters::util::tests::the_version_probe_reports_a_missing_binary_the_way_node_does",
        "crates/orch-infra/src/adapters/util.rs",
    ),
    (
        Quirk::SpawnStartsMissingBinary,
        "models::discovery::tests::the_real_runner_reports_a_missing_binary",
        "crates/orch-infra/src/models/discovery.rs",
    ),
    (
        Quirk::SpawnStartsMissingBinary,
        "process::manager::tests::a_missing_binary_fails_with_the_ts_message",
        "crates/orch-infra/src/process/manager.rs",
    ),
    (
        Quirk::SpawnStartsMissingBinary,
        "process::spawn::tests::a_spawn_failure_becomes_a_classified_adapter_error",
        "crates/orch-infra/src/process/spawn.rs",
    ),
];

pub(crate) fn run(repo: &Path, args: LinuxArgs) -> Result<()> {
    let command = args.command.unwrap_or(LinuxCommand::Ci);
    let repo = repo
        .canonicalize()
        .with_context(|| format!("resolving {}", repo.display()))?;
    if let Err(reason) = check_mount_path(&repo) {
        cannot_run(&reason);
    }
    let key = volume_key(&repo);

    if !docker_available() {
        cannot_run("Docker is not available (install Docker Desktop and start it)");
    }
    if let LinuxCommand::Clean { all } = command {
        return clean(&repo, &key, all);
    }
    if command == LinuxCommand::Shell && !std::io::stdin().is_terminal() {
        refuse("`shell` needs a terminal; use `cargo xtask linux run <command>` instead");
    }

    let toolchain = std::fs::read_to_string(repo.join("rust-toolchain.toml"))
        .context("reading rust-toolchain.toml")?;
    let channel = toolchain_channel(&toolchain).unwrap_or_else(|reason| cannot_run(&reason));
    let name = format!("orch-linux-{key}");
    if container_exists(&name)? {
        refuse(&format!(
            "container {name} already exists for this checkout (still running, or left behind); wait for it or `docker rm -f {name}`"
        ));
    }
    let others = running_containers()?;
    if !others.is_empty() {
        eprintln!(
            "xtask linux: warning: other orch-linux containers are running ({}); two at once can exhaust Docker's memory",
            others.join(", ")
        );
    }

    let uid = id("-u").unwrap_or_else(|error| cannot_run(&format!("{error:#}")));
    let gid = id("-g").unwrap_or_else(|error| cannot_run(&format!("{error:#}")));
    let base = format!("rust:{channel}-trixie");
    let image = format!("orch-linux:{channel}");
    if let Err(error) = build_image(&base, &image, &channel) {
        cannot_run(&format!(
            "{error:#} (no network, or {base} cannot be pulled for {PLATFORM})"
        ));
    }
    let target_volume = format!("orch-linux-target-{key}");
    for volume in [target_volume.as_str(), REGISTRY_VOLUME] {
        ensure_volume(volume, &repo)?;
    }

    // The volume mount point has to exist in the read-only checkout.
    std::fs::create_dir_all(repo.join("target")).context("creating target/")?;
    let source = describe_source(&repo, &base)?;
    let git_common = git(
        &repo,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )
    .ok()
    .and_then(|common| mountable_git_dir(Path::new(&common)));
    let mut docker = docker_run_args(&DockerRun {
        repo: &repo,
        git_common: git_common.as_deref(),
        target_volume: &target_volume,
        name: &name,
        image: &image,
        tty: tty_flags(&command),
    });
    let (script, extra) = container_script(&command);
    docker.extend(["bash".to_owned(), "-c".to_owned(), ROOT_PHASE.to_owned()]);
    // `source` is an argument, never shell text, whatever it comes to hold.
    docker.extend(["linux".to_owned(), uid, gid, script, source]);
    docker.extend(extra);

    let status = Command::new("docker")
        .args(&docker)
        .status()
        .context("running docker")?;
    exit_with(status)
}

/// Runs as root just long enough to hand the fresh volumes and HOME to the
/// caller's uid, then drops to it with setpriv. `$1` and `$2` are the uid and
/// gid, `$3` the script, and the rest its arguments. `--no-new-privs` keeps a
/// setuid binary in the image from taking the command back to root.
const ROOT_PHASE: &str = r#"set -eu
uid=$1 gid=$2 script=$3; shift 3
install -d -o "$uid" -g "$gid" /home/dev
for dir in "$PWD/target" /usr/local/cargo/registry; do
  [ "$(stat -c %u "$dir")" = "$uid" ] || chown "$uid:$gid" "$dir"
done
exec setpriv --reuid="$uid" --regid="$gid" --clear-groups --no-new-privs -- bash -c "$script" linux "$@""#;

/// The script the container runs as the caller, and the arguments after its
/// first, which is always the description of the source being tested.
fn container_script(command: &LinuxCommand) -> (String, Vec<String>) {
    // Informational only: whether amd64 is emulated changes nothing here. What
    // changes the run is what the probes find. `ARCH` is the xtask binary's, so
    // an x86_64 xtask that is itself emulated would print `native`.
    let host = std::env::consts::ARCH;
    let header = format!(
        "set -euo pipefail\n\
         source=$1\nshift\n\
         arch=$(uname -m)\n\
         if [ \"$arch\" = {host} ]; then platform=\"$arch, native\"\n\
         else platform=\"$arch, emulated on an {host} host\"; fi\n\
         echo \"xtask linux: $platform, $source\"\n"
    );
    match command {
        LinuxCommand::Ci => {
            let mut script = header;
            script.push_str("skips=''\ncount=0\n");
            for quirk in Quirk::ALL {
                let tests: Vec<&str> = PLATFORM_SKIPS
                    .iter()
                    .filter(|(skip, _, _)| *skip == quirk)
                    .map(|(_, test, _)| *test)
                    .collect();
                script.push_str(quirk.probe());
                script.push_str(&format!(
                    "if [ \"${}\" = 1 ]; then\n  echo \"xtask linux: {}; skipping:\"\n",
                    quirk.var(),
                    quirk.found(),
                ));
                for test in &tests {
                    script.push_str(&format!(
                        "  echo '  {test}'\n  skips=\"$skips --skip {test}\"\n"
                    ));
                }
                script.push_str(&format!("  count=$((count + {}))\nfi\n", tests.len()));
            }
            script.push_str(
                "skipped=''\n\
                 if [ \"$count\" -gt 0 ]; then\n  \
                 skipped=\", $count tests skipped for platform behaviour (Linux CI runs them)\"\n  \
                 skips=\"-- --exact$skips\"\n\
                 fi\n",
            );
            for step in CI_STEPS {
                let quoted = step.replace('\'', "'\\''");
                let command = if step.starts_with("cargo test ") {
                    format!("{step} $skips")
                } else {
                    (*step).to_owned()
                };
                script.push_str(&format!(
                    "echo '════ {quoted}'\n{command} || {{ echo '════ FAILED: {quoted}'; exit 1; }}\n"
                ));
            }
            script
                .push_str("echo \"════ linux/amd64 CI set passed$skipped ($platform), $source\"\n");
            (script, Vec::new())
        }
        LinuxCommand::Run { command } => (format!("{header}exec \"$@\"\n"), command.clone()),
        // Not a login shell: Debian's /etc/profile resets PATH and drops cargo.
        LinuxCommand::Shell => (format!("{header}exec bash\n"), Vec::new()),
        LinuxCommand::Clean { .. } => unreachable!("clean never starts a container"),
    }
}

struct DockerRun<'a> {
    repo: &'a Path,
    git_common: Option<&'a Path>,
    target_volume: &'a str,
    name: &'a str,
    image: &'a str,
    tty: &'static [&'static str],
}

/// `docker run …` up to and including the image. `--mount`, never `-v`: on
/// Docker Desktop, `-v <path>:<same path>:ro` under /Volumes mounts nothing.
fn docker_run_args(run: &DockerRun<'_>) -> Vec<String> {
    let repo = run.repo.display().to_string();
    let mut args: Vec<String> = [
        "run",
        "--rm",
        "--init",
        "--platform",
        PLATFORM,
        "--name",
        run.name,
        "--label",
        LABEL,
    ]
    .map(str::to_owned)
    .to_vec();
    args.extend(run.tty.iter().map(|flag| (*flag).to_owned()));
    let mut mounts = vec![format!("type=bind,src={repo},dst={repo},readonly")];
    // A linked worktree's .git is a file naming the main checkout's git
    // directory, so that directory has to be visible at its own path.
    if let Some(common) = run
        .git_common
        .filter(|common| !common.starts_with(run.repo))
    {
        let common = common.display();
        mounts.push(format!("type=bind,src={common},dst={common},readonly"));
    }
    mounts.push(format!(
        "type=volume,src={},dst={repo}/target",
        run.target_volume
    ));
    mounts.push(format!(
        "type=volume,src={REGISTRY_VOLUME},dst=/usr/local/cargo/registry"
    ));
    for mount in mounts {
        args.extend(["--mount".to_owned(), mount]);
    }
    args.extend(["--workdir".to_owned(), repo]);
    // Only these reach the container; nothing from the host environment does.
    for env in ["HOME=/home/dev", "USER=dev", "LANG=C.UTF-8"] {
        args.extend(["--env".to_owned(), env.to_owned()]);
    }
    args.push(run.image.to_owned());
    args
}

fn tty_flags(command: &LinuxCommand) -> &'static [&'static str] {
    if *command == LinuxCommand::Shell {
        &["--interactive", "--tty"]
    } else if std::io::stdin().is_terminal() && std::io::stdout().is_terminal() {
        &["--tty"]
    } else {
        &[]
    }
}

/// The toolchain pin, which is also the image tag. Only an `X.Y.Z` pin has a
/// `rust:X.Y.Z-trixie` image.
fn toolchain_channel(toolchain: &str) -> std::result::Result<String, String> {
    let channel = toolchain
        .lines()
        .filter_map(|line| line.trim().strip_prefix("channel"))
        .filter_map(|rest| rest.trim_start().strip_prefix('='))
        .map(|value| value.trim().trim_matches('"').to_owned())
        .next()
        .ok_or("rust-toolchain.toml has no `channel`")?;
    let parts: Vec<&str> = channel.split('.').collect();
    let numeric = |part: &&str| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit());
    if parts.len() == 3 && parts.iter().all(numeric) {
        Ok(channel)
    } else {
        Err(format!(
            "rust-toolchain.toml channel `{channel}` is not an X.Y.Z version; there is no rust:<channel>-trixie image for it"
        ))
    }
}

/// A stable name for this checkout's target volume: FNV-1a over the path, so
/// two worktrees never share a target directory.
fn volume_key(repo: &Path) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in repo.as_os_str().as_encoded_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

fn dockerfile(base: &str, channel: &str) -> String {
    format!(
        "FROM {base}\n\
         RUN apt-get update \\\n \
         && apt-get install -y --no-install-recommends tmux \\\n \
         && rm -rf /var/lib/apt/lists/*\n\
         RUN rustup component add --toolchain {channel} rustfmt clippy\n"
    )
}

/// Cheap when nothing changed: every layer comes from the build cache. It
/// does not re-pull the base; `docker pull --platform linux/amd64 <base>` does.
fn build_image(base: &str, image: &str, channel: &str) -> Result<()> {
    eprintln!("xtask linux: building {image} from {base} ({PLATFORM})");
    let mut child = Command::new("docker")
        .args([
            "build",
            "--quiet",
            "--platform",
            PLATFORM,
            "--tag",
            image,
            "-",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .context("running docker build")?;
    // Wait even when the write fails: a docker that exits early closes its
    // stdin, and its own status says more than the broken pipe does.
    let written = child
        .stdin
        .take()
        .context("docker build has no stdin")?
        .write_all(dockerfile(base, channel).as_bytes());
    let status = child.wait().context("waiting for docker build")?;
    if !status.success() {
        bail!("docker build of {image} failed ({status})");
    }
    written.context("writing the Dockerfile to docker build")
}

/// The commit, tree and base image the run tests, printed by the container
/// so the PR can cite exactly what passed. The checkout is mounted live, so
/// uncommitted changes are part of the run; `dirty` says so.
fn describe_source(repo: &Path, base: &str) -> Result<String> {
    let commit = git(repo, &["rev-parse", "--short", "HEAD"]).unwrap_or_else(|_| "unknown".into());
    let tree =
        git(repo, &["rev-parse", "--short", "HEAD^{tree}"]).unwrap_or_else(|_| "unknown".into());
    let dirty = match git(repo, &["status", "--porcelain", "--untracked-files=no"]) {
        Ok(status) if status.is_empty() => "no",
        Ok(_) => "yes",
        Err(_) => "unknown",
    };
    let digest = capture(Command::new("docker").args([
        "image",
        "inspect",
        "--format",
        "{{index .RepoDigests 0}}",
        base,
    ]))
    .unwrap_or_else(|_| base.to_owned());
    Ok(format!(
        "commit {commit} tree {tree} dirty {dirty}, {digest}"
    ))
}

fn ensure_volume(volume: &str, repo: &Path) -> Result<()> {
    if Command::new("docker")
        .args(["volume", "inspect", volume])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
    {
        return Ok(());
    }
    let repo_label = format!("org.r-orch.repo={}", repo.display());
    capture(Command::new("docker").args([
        "volume",
        "create",
        "--label",
        LABEL,
        "--label",
        &repo_label,
        volume,
    ]))
    .map(|_| ())
    .with_context(|| format!("creating volume {volume}"))
}

fn clean(repo: &Path, key: &str, all: bool) -> Result<()> {
    let volumes = if all {
        capture(Command::new("docker").args([
            "volume",
            "ls",
            "--quiet",
            "--filter",
            &format!("label={LABEL}"),
        ]))?
        .lines()
        .map(str::to_owned)
        .collect()
    } else {
        vec![format!("orch-linux-target-{key}")]
    };
    let mut failed = false;
    for volume in volumes {
        let exists = Command::new("docker")
            .args(["volume", "inspect", &volume])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success());
        if !exists {
            println!("no volume {volume} ({})", repo.display());
            continue;
        }
        match capture(Command::new("docker").args(["volume", "rm", &volume])) {
            Ok(_) => println!("removed {volume}"),
            Err(error) => {
                eprintln!("xtask linux: could not remove {volume}: {error:#}");
                failed = true;
            }
        }
    }
    if failed {
        bail!(
            "some volumes are still in use; stop their containers first (docker ps --filter label={LABEL})"
        );
    }
    Ok(())
}

/// Any container with this name, running or not: `docker run --name` fails on
/// either.
fn container_exists(name: &str) -> Result<bool> {
    let filter = format!("name=^{name}$");
    Ok(
        !capture(Command::new("docker").args(["ps", "--all", "--quiet", "--filter", &filter]))?
            .is_empty(),
    )
}

/// `--mount` is comma-separated and quote-aware, so a path holding either
/// would split into other fields.
fn check_mount_path(path: &Path) -> std::result::Result<(), String> {
    let text = path.display().to_string();
    if text.contains(',') || text.contains('"') {
        Err(format!(
            "the path {text} contains a comma or a double quote, which docker --mount cannot take"
        ))
    } else {
        Ok(())
    }
}

/// The main checkout's git directory, mounted only when it is one: a `.git`
/// directory next to a `Cargo.toml`. A `.git` file can point anywhere, and the
/// container must not get, say, the home directory.
fn mountable_git_dir(common: &Path) -> Option<PathBuf> {
    let common = common.canonicalize().ok()?;
    let is_checkout = common.file_name().is_some_and(|name| name == ".git")
        && common.is_dir()
        && common
            .parent()
            .is_some_and(|parent| parent.join("Cargo.toml").is_file())
        && check_mount_path(&common).is_ok();
    if is_checkout {
        Some(common)
    } else {
        eprintln!(
            "xtask linux: warning: not mounting git directory {}; git will not work in the container",
            common.display()
        );
        None
    }
}

fn running_containers() -> Result<Vec<String>> {
    let filter = format!("label={LABEL}");
    Ok(
        capture(Command::new("docker").args([
            "ps",
            "--format",
            "{{.Names}}",
            "--filter",
            &filter,
        ]))?
        .lines()
        .map(str::to_owned)
        .collect(),
    )
}

fn docker_available() -> bool {
    Command::new("docker")
        .args(["info", "--format", "{{.ServerVersion}}"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

fn git(repo: &Path, args: &[&str]) -> Result<String> {
    capture(Command::new("git").arg("-C").arg(repo).args(args))
}

fn id(flag: &str) -> Result<String> {
    capture(Command::new("id").arg(flag))
}

/// Runs `command` and returns its trimmed stdout, or an error with its stderr.
fn capture(command: &mut Command) -> Result<String> {
    let output = command
        .output()
        .with_context(|| format!("running {command:?}"))?;
    if !output.status.success() {
        bail!(
            "{command:?} failed ({}): {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

/// A refusal that is not about the environment: nothing ran, so there is
/// nothing to report as env-limited.
fn refuse(reason: &str) -> ! {
    eprintln!("xtask linux: {reason}.");
    std::process::exit(CANNOT_RUN)
}

fn cannot_run(reason: &str) -> ! {
    eprintln!("xtask linux: {reason}. Report the Linux checks as env-limited (Linux CI).");
    std::process::exit(CANNOT_RUN)
}

fn exit_with(status: ExitStatus) -> Result<()> {
    if status.success() {
        return Ok(());
    }
    // A signal has no code; 128 + signal is what a shell would report.
    let code = status.code().unwrap_or_else(|| {
        use std::os::unix::process::ExitStatusExt;
        128 + status.signal().unwrap_or(0)
    });
    std::process::exit(code)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_channel_comes_from_the_toolchain_pin() {
        let pin = "[toolchain]\nchannel = \"1.99.1\"\ncomponents = [\"rustfmt\"]\n";
        assert_eq!(toolchain_channel(pin).unwrap(), "1.99.1");
    }

    #[test]
    fn the_repos_own_pin_is_an_image_tag() {
        let repo = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let pin = std::fs::read_to_string(repo.join("rust-toolchain.toml")).unwrap();
        assert!(toolchain_channel(&pin).is_ok(), "{pin}");
    }

    #[test]
    fn a_pin_without_a_version_is_refused() {
        for pin in [
            "[toolchain]\nchannel = \"stable\"\n",
            "[toolchain]\nchannel = \"1.99\"\n",
            "[toolchain]\nchannel = \"nightly-2026-10-01\"\n",
            "[toolchain]\nchannel = \"1.99.x\"\n",
            "[toolchain]\nprofile = \"minimal\"\n",
        ] {
            assert!(toolchain_channel(pin).is_err(), "{pin}");
        }
    }

    #[test]
    fn each_checkout_gets_its_own_stable_volume() {
        let a = volume_key(Path::new("/work/r-orch"));
        assert_eq!(a, volume_key(Path::new("/work/r-orch")));
        assert_ne!(a, volume_key(Path::new("/work/r-orch/.worktrees/x")));
        assert_eq!(a.len(), 16);
        assert!(a.bytes().all(|b| b.is_ascii_hexdigit()));
    }

    fn args_for(repo: &str, git_common: Option<&str>) -> Vec<String> {
        docker_run_args(&DockerRun {
            repo: Path::new(repo),
            git_common: git_common.map(Path::new),
            target_volume: "orch-linux-target-k",
            name: "orch-linux-k",
            image: "orch-linux:1.99.1",
            tty: &[],
        })
    }

    #[test]
    fn the_checkout_is_read_only_and_target_is_a_volume() {
        let args = args_for("/w/r-orch", Some("/w/r-orch/.git"));
        let mounts: Vec<&String> = args
            .iter()
            .zip(args.iter().skip(1))
            .filter(|(flag, _)| *flag == "--mount")
            .map(|(_, mount)| mount)
            .collect();
        assert_eq!(
            mounts,
            [
                "type=bind,src=/w/r-orch,dst=/w/r-orch,readonly",
                "type=volume,src=orch-linux-target-k,dst=/w/r-orch/target",
                "type=volume,src=orch-linux-cargo-registry,dst=/usr/local/cargo/registry",
            ]
        );
        assert!(!args.iter().any(|arg| arg == "-v" || arg == "--volume"));
        assert_eq!(args.last().map(String::as_str), Some("orch-linux:1.99.1"));
        assert!(args.windows(2).any(|w| w == ["--platform", "linux/amd64"]));
    }

    #[test]
    fn a_linked_worktree_also_mounts_the_main_git_dir() {
        let args = args_for("/w/r-orch/.worktrees/x", Some("/w/r-orch/.git"));
        assert!(args.windows(2).any(|w| w
            == [
                "--mount",
                "type=bind,src=/w/r-orch/.git,dst=/w/r-orch/.git,readonly"
            ]));
    }

    #[test]
    fn only_the_named_variables_reach_the_container() {
        let args = args_for("/w/r-orch", None);
        let envs: Vec<&String> = args
            .iter()
            .zip(args.iter().skip(1))
            .filter(|(flag, _)| *flag == "--env")
            .map(|(_, env)| env)
            .collect();
        assert_eq!(envs, ["HOME=/home/dev", "USER=dev", "LANG=C.UTF-8"]);
    }

    /// The default run must keep up with CI: every command CI runs, other than
    /// toolchain setup, tmux install and cargo-deny, is one of the steps here.
    #[test]
    fn the_ci_steps_cover_the_ci_workflow() {
        let repo = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let ci = std::fs::read_to_string(repo.join(".github/workflows/ci.yml")).unwrap();
        let script = container_script(&LinuxCommand::Ci).0;
        let mut checked = 0;
        for line in ci.lines().map(str::trim) {
            let Some(run) = line.strip_prefix("- run: ") else {
                continue;
            };
            if run.starts_with("rustup ") || run.contains("apt-get") {
                continue;
            }
            assert!(
                script.contains(run),
                "ci.yml runs `{run}`, which cargo xtask linux does not"
            );
            checked += 1;
        }
        assert!(checked >= 4, "found only {checked} cargo steps in ci.yml");
        for line in [
            "test -s Cargo.lock",
            "! grep -qE '^name = \"(terminfo|termwiz)\"$' Cargo.lock",
        ] {
            assert!(
                ci.contains(line),
                "ci.yml no longer has `{line}`; update CI_STEPS"
            );
        }
    }

    #[test]
    fn the_image_adds_tmux_and_the_pinned_components() {
        let file = dockerfile("rust:1.99.1-trixie", "1.99.1");
        assert!(file.starts_with("FROM rust:1.99.1-trixie\n"));
        assert!(file.contains("apt-get install -y --no-install-recommends tmux"));
        assert!(file.contains("rustup component add --toolchain 1.99.1 rustfmt clippy"));
    }

    /// A renamed or removed test would leave a `--skip` that matches nothing,
    /// so each one must still exist where the list says.
    #[test]
    fn every_platform_skip_names_a_test_that_exists() {
        let repo = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        for (_, test, file) in PLATFORM_SKIPS {
            let source = std::fs::read_to_string(repo.join(file)).unwrap();
            let name = test.rsplit("::").next().unwrap();
            assert!(
                source.contains(&format!("fn {name}(")),
                "{test} is not in {file}"
            );
        }
        for quirk in Quirk::ALL {
            assert!(
                PLATFORM_SKIPS.iter().any(|(skip, _, _)| *skip == quirk),
                "{quirk:?} skips nothing"
            );
        }
    }

    /// Each test is skipped only inside its own behaviour's `if`, and nothing
    /// keys on a particular emulator.
    #[test]
    fn skips_follow_the_probed_behaviours() {
        let script = container_script(&LinuxCommand::Ci).0;
        for quirk in Quirk::ALL {
            assert!(script.contains(quirk.probe()), "{quirk:?} probe missing");
            let open = format!("if [ \"${}\" = 1 ]; then\n", quirk.var());
            let start = script.find(&open).expect("the quirk's if");
            let block = &script[start..start + script[start..].find("\nfi\n").unwrap()];
            for (skip, test, _) in PLATFORM_SKIPS {
                assert_eq!(
                    block.contains(&format!("--skip {test}\"")),
                    *skip == quirk,
                    "{test} under {quirk:?}"
                );
            }
        }
        assert!(script.contains("skips=\"-- --exact$skips\""));
        assert!(script.contains("cargo test --workspace --no-fail-fast $skips"));
        let lower = script.to_lowercase();
        for emulator in ["rosetta", "virtualapple", "qemu"] {
            assert!(
                !lower.contains(emulator),
                "the run must not key on {emulator}"
            );
        }
    }

    /// The probes must report what this platform really does, wherever the
    /// test runs: nothing on macOS or Linux CI, both behaviours in an emulated
    /// container. The ground truth is Rust's own `Command`, which is what the
    /// skipped tests use. On a native platform this only checks the `0 0`
    /// case; `cargo xtask linux run cargo test -p xtask` checks the other.
    #[test]
    fn the_probes_agree_with_what_this_platform_does() {
        let spawn_starts = Command::new("orch-xtask-no-such-binary")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map(|mut child| child.wait())
            .is_ok();

        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let home = std::env::temp_dir().join(format!("xtask-probe-{}-{nanos}", std::process::id()));
        std::fs::create_dir(&home).unwrap();
        let status = Command::new("env").arg("true").env("HOME", &home).status();
        let writes_home = std::fs::read_dir(&home).unwrap().next().is_some();
        std::fs::remove_dir_all(&home).unwrap();
        assert!(status.unwrap().success());

        let script = format!(
            "set -euo pipefail\n{SPAWN_PROBE}{HOME_PROBE}echo \"$spawn_quirk $home_quirk\"\n"
        );
        let output = Command::new("bash").args(["-c", &script]).output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let expected = format!("{} {}", u8::from(spawn_starts), u8::from(writes_home));
        assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), expected);
    }

    #[test]
    fn shell_is_not_a_login_shell_and_run_keeps_its_arguments() {
        let shell = container_script(&LinuxCommand::Shell).0;
        assert!(shell.ends_with("exec bash\n"), "{shell}");
        assert!(!shell.contains("bash -l"));
        let (run, args) = container_script(&LinuxCommand::Run {
            command: vec!["cargo".into(), "test".into(), "a b".into()],
        });
        assert!(run.ends_with("exec \"$@\"\n"), "{run}");
        assert_eq!(args, ["cargo", "test", "a b"]);
    }

    #[test]
    fn the_source_is_an_argument_not_script_text() {
        for command in [LinuxCommand::Ci, LinuxCommand::Shell] {
            let script = container_script(&command).0;
            assert!(
                script.starts_with("set -euo pipefail\nsource=$1\nshift\n"),
                "{script}"
            );
        }
    }

    #[test]
    fn the_generated_scripts_are_valid_bash() {
        let run = LinuxCommand::Run {
            command: vec!["true".into()],
        };
        let scripts = [
            ROOT_PHASE.to_owned(),
            container_script(&LinuxCommand::Ci).0,
            container_script(&LinuxCommand::Shell).0,
            container_script(&run).0,
        ];
        for script in scripts {
            let status = Command::new("bash")
                .args(["-n", "-c", &script])
                .status()
                .unwrap();
            assert!(status.success(), "bash -n rejects:\n{script}");
        }
        assert!(ROOT_PHASE.contains("--no-new-privs"));
    }

    #[test]
    fn mount_paths_with_commas_or_quotes_are_refused() {
        assert!(check_mount_path(Path::new("/w/r-orch")).is_ok());
        assert!(check_mount_path(Path::new("/w/with space/r-orch")).is_ok());
        assert!(check_mount_path(Path::new("/w/a,b/r-orch")).is_err());
        assert!(check_mount_path(Path::new("/w/a\"b/r-orch")).is_err());
    }

    #[test]
    fn only_a_checkouts_git_dir_is_mounted() {
        let repo = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        assert_eq!(mountable_git_dir(repo), None, "a checkout is not a git dir");
        assert_eq!(mountable_git_dir(Path::new("/")), None);
        assert_eq!(mountable_git_dir(&repo.join("no-such-dir/.git")), None);
        // A real checkout's git dir, when this one has a directory there
        // (a linked worktree has a .git file instead).
        let git = repo.join(".git");
        if git.is_dir() {
            assert_eq!(mountable_git_dir(&git), Some(git.canonicalize().unwrap()));
        }
    }
}
