//! Every test that needs isolation is named in `.config/nextest.toml`.
//!
//! `cargo test` ran one test binary at a time and let libtest thread the tests
//! inside it. `cargo-nextest` runs **every test in its own process**, which
//! changes two things at once: a process-global `static` lock no longer
//! serialises anything (there is nothing left to share), and two test binaries
//! that each watch the host's process table can now run at the same time. The
//! first is harmless, the second is a flake.
//!
//! The in-crate guards are not removed: `crates/**` is frozen for this change
//! (ADR 0022 decision 4), and they still serialise a plain `cargo test`. What
//! this test does is force every host-wide observation and every process-global
//! lock to be *classified*: either `.config/nextest.toml` names its binary, or
//! the ledger below records why it needs no entry. A new one that is in neither
//! fails here, with the two ways to fix it, rather than becoming an
//! intermittent failure somewhere else.
//!
//! An `Exclusive` or `Group` row states the **hazard**, not the lock. A lock is
//! inert under nextest, so a row that pointed at one would tell the next reader
//! that deleting the lock makes the entry unnecessary — the opposite of true.
//!
//! This is a source scan, like the other guards in `xtask/tests`. It is
//! deliberately over-eager — a self-scoped `/proc/self` read is flagged too —
//! because a pattern that is cheap to classify is cheaper than one that is
//! missed. It fails in both directions: an unclassified hit fails, and a
//! classification whose pattern has gone fails as stale.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

const CONFIG: &str = include_str!("../../.config/nextest.toml");
/// The two other readers of the pass split. The configuration defines it; the
/// gate and the ambient CI job must run exactly the passes it defines.
const GATE: &str = include_str!("../src/gate/nextest.rs");
/// The gate's commands: one `nextest run` per pass and the one listing that is
/// the expected set both are judged against.
const SUITE: &str = include_str!("../src/gate/suite.rs");
const CI: &str = include_str!("../../.github/workflows/ci.yml");
/// This file quotes every pattern it searches for, so it cannot scan itself.
const SELF: &str = "xtask/tests/nextest_isolation.rs";

/// Why a file that matches one of the patterns needs no configuration entry,
/// or which entry covers it and what hazard that entry answers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Verdict {
    /// The owning test binary must be named by an override that gives it
    /// host-wide exclusivity. The string is the hazard, with line numbers.
    Exclusive(&'static str),
    /// The owning test binary must be named by an override that puts it in a
    /// `max-threads` test group. The string is the hazard.
    Group(&'static str),
    /// The owning test binary must be the whole of the alone pass: it runs in
    /// its own `nextest run`, with nothing else on the host, but its own tests
    /// in parallel with each other. The string is the hazard.
    AlonePass(&'static str),
    /// Every observation is scoped to something this test owns — its own pid,
    /// process group, sandbox directory or scene — so a neighbour cannot be
    /// mistaken for its own work.
    Scoped(&'static str),
    /// A process-global `static` whose effect nextest removes by giving each
    /// test its own process. Nothing is shared, so nothing needs serialising.
    PerProcess(&'static str),
    /// Not test code, or the implementation the tests drive rather than a test
    /// that depends on a host-wide answer.
    Production(&'static str),
}

/// Tests that are host-exclusive on their own, inside a binary that is not.
/// The configuration gives each of these a test-level override; anything here
/// must be there and anything there must be here.
///
/// The port-reuse entry is host-exclusive rather than moved into the alone pass on
/// purpose. Its hazard is the same released-and-rebound ephemeral port, and in
/// the alone pass it would compete for ports with the 42 tests that pass runs
/// in parallel — worse than what it had under `cargo test`, where its binary's
/// own mutex meant no sibling ran beside it. Host-wide exclusivity reproduces
/// that, and for one test it costs one test's duration.
const EXCLUSIVE_TESTS: &[(&str, &str)] = &[
    (
        "crates/orch-cli/tests/daemon_nodaemon_matrix.rs",
        "web_answers_503_when_the_daemon_dies_while_serving_and_starts_nothing",
    ),
    // #222: each matrix cold-builds xtask in a private target inside its drain
    // deadline. A group cannot keep unrelated workspace work off the host.
    (
        "xtask/src/gate/target/qualification.rs",
        "gate::target::qualification::production_holder_commands_and_discriminators_on_scratch_filesystem",
    ),
    (
        "xtask/src/gate/target/qualification.rs",
        "gate::target::qualification::production_holder_commands_and_discriminators_on_worktree_filesystem",
    ),
];

/// A helper that binds an ephemeral port, takes the address and **releases**
/// the listener, so the address is rebound later. Its callers race every other
/// process on the host for that port, so their binary must be host-exclusive
/// or the calling test must be listed in `EXCLUSIVE_TESTS`.
const PORT_REUSE_HELPERS: &[&str] = &["free_loopback_port()"];

/// Every file under `crates/` and `xtask/` that matches a pattern below, with
/// its verdict. Keep it sorted by path. A row whose file matches nothing is
/// stale and fails, so a deleted or fixed pattern cannot leave a lie behind.
const LEDGER: &[(&str, Verdict)] = &[
    (
        "crates/orch-cli/src/serve/logger.rs",
        Verdict::Production("reads this process's own /proc/self/status for its log"),
    ),
    (
        "crates/orch-cli/tests/common/daemon.rs",
        Verdict::Scoped(
            "the process-table reads at :315,:319,:570,:574 filter by this \
             fixture's own pgid or socket directory, and the REGISTRY static at \
             :349 is keyed by unique temp roots, which its own comment states \
             concurrent tests never share",
        ),
    ),
    (
        "crates/orch-cli/tests/daemon_admission.rs",
        Verdict::Exclusive(
            "every cell calls assert_only_clients, which is \
             daemon_support/frontends.rs:379 over the unscoped new_orch() of \
             :336: it treats *any* new process whose exe is CARGO_BIN_EXE_orch \
             as an offender, so a concurrent orch-cli test that starts the \
             binary fails it. The static SERIAL at :33 is inert under nextest \
             and is not the reason for this entry",
        ),
    ),
    (
        "crates/orch-cli/tests/daemon_lifecycle.rs",
        Verdict::Scoped(
            "the process_table() read at :663 looks up its own daemon by pid \
             and walks that one process's ancestry; the two new_orch_of uses at \
             :487,:489 filter by this sandbox's ORCH_SOCKET marker",
        ),
    ),
    (
        "crates/orch-cli/tests/daemon_lifecycle_nospawn.rs",
        Verdict::Exclusive(
            "the no-spawn proof compares the whole host process table before \
             against after (process_table() at :81,:106,:129,:301,:328,:345,\
             :357,:359) and requires that no new `orch` process exists at all; \
             the file's own docs at :8-10 and :16-17 say it relies on cargo \
             running test binaries one after another. The static SERIAL at :28 \
             is inert under nextest and is not the reason for this entry",
        ),
    ),
    (
        "crates/orch-cli/tests/daemon_nodaemon_matrix.rs",
        Verdict::Group(
            "its process observations are scene-scoped — assert_only_scene_clients, \
             assert_nothing_left, new_orch_in_scene, and the pid-membership \
             tests at :532,:549,:576-577,:594 — and each Scene gets fresh \
             tempdirs (daemon_support/mod.rs:47-63), so it needs no host-wide \
             exclusivity. What it does need is its own cells not overlapping: \
             :28-29 states the SERIAL mutex at :36 serialises cells within one \
             process, and nextest runs each cell in its own process, so the \
             group restores exactly that and nothing more",
        ),
    ),
    (
        "crates/orch-cli/tests/daemon_skew_two_binaries.rs",
        Verdict::Exclusive(
            "`assert!(watch.new_orch().is_empty())` at :187 is an emptiness \
             assertion over every `orch` process on the host; the comment at \
             :31-33 says the watch sees every such process \"including another \
             test's daemon\". The static LOCK at :35 is inert under nextest and \
             is not the reason for this entry",
        ),
    ),
    (
        "crates/orch-cli/tests/daemon_support/frontends.rs",
        Verdict::Scoped(
            "shared helpers, not a test. The scoped ones filter by scene \
             identity; the unscoped new_orch and assert_only_clients are called \
             only from the binaries this ledger marks Exclusive, and \
             free_loopback_port at :472 releases the ephemeral port it returns \
             — both call-site rules below enforce who may use them",
        ),
    ),
    (
        "crates/orch-cli/tests/daemon_support/mod.rs",
        Verdict::Scoped(
            "defines process_table() (:525) and orch_processes() (:555); the \
             read it performs is filtered by the caller's own pgid",
        ),
    ),
    (
        "crates/orch-cli/tests/fixture_daemon.rs",
        Verdict::Scoped(
            "the process_table() read at :133 is filtered by this fixture's own \
             socket directory, a per-fixture tempdir",
        ),
    ),
    (
        "crates/orch-cli/tests/remote_peers.rs",
        Verdict::AlonePass(
            "`port()` at :700 binds 127.0.0.1:0, takes the address and drops \
             the listener, and a peer rebinds that same address across a \
             restart (:414-425 retries only a confirmed collision, until a \
             deadline) — so it races every other process on the host for one \
             ephemeral port. 36 of its 42 tests reach that helper. Observed \
             2026-11-24: qualification run 1 failed \
             s23b_sigterm_run_watch_while_offline_keeps_the_remote_run with \
             \"Address already in use\". The process-table reads here are \
             themselves pid-scoped (descendant_groups at :571 walks from a pid \
             this test spawned; the lsof and rss reads at :2581,:2934 name one \
             pid with -p); the port space is the hazard. It runs alone rather \
             than host-exclusive because the condition it was green under is \
             \"this binary alone, its own tests in parallel\": its tests then \
             race only each other for a port, as they did when cargo ran one \
             test binary at a time. Host-wide exclusivity would also be \
             correct but costs the sum of its 42 tests",
        ),
    ),
    (
        "crates/orch-cli/tests/stress_single_writer.rs",
        Verdict::Scoped(
            "every process-table use is scoped by std::process::id() \
             (:2027-2033,:2043-2056) or by this sandbox's short_dir; the filter \
             at :1091-1094 only builds a diagnostic message",
        ),
    ),
    (
        "crates/orch-cli/tests/token_eco_standalone_signal.rs",
        Verdict::Scoped("`ps -o stat= -p <pid>` at :229 for pids this test spawned"),
    ),
    (
        "crates/orch-cli/tests/tui_backend.rs",
        Verdict::PerProcess(
            "GLOBAL_CONFIG_TESTS at :35 guards nothing today: its own comment \
             says each project keeps its global.yml inside its own directory",
        ),
    ),
    (
        "crates/orch-cli/tests/yaml_bomb.rs",
        Verdict::Scoped("samples /proc/<its own daemon's pid>/status at :138"),
    ),
    (
        "crates/orch-core/src/test_support/process_group.rs",
        Verdict::Scoped("scans /proc at :44 but keeps only members of a caller-supplied pgid"),
    ),
    (
        "crates/orch-infra/src/process/gate/observation.rs",
        Verdict::PerProcess(
            "the REGISTRY the SERIAL at :315 serialises is a process-global \
             OnceLock (:52-53), so one process per test is strictly more \
             isolated than the lock was",
        ),
    ),
    (
        "crates/orch-infra/src/process/gate/tests.rs",
        Verdict::PerProcess("SCENARIOS at :13 serialises mutations of process-global spawn hooks"),
    ),
    (
        "crates/orch-infra/src/storage/fs_utils.rs",
        Verdict::PerProcess("process-global handle caches and the unit lock that guards them"),
    ),
    (
        "crates/orch-infra/src/storage/lock.rs",
        Verdict::PerProcess("a process-global advisory-lock table and the unit lock over it"),
    ),
    (
        "crates/orch-infra/tests/scripted_process_cleanup.rs",
        Verdict::Scoped(
            "the process-table rows at :38,:375 are filtered by this fixture's own pgid",
        ),
    ),
    (
        "crates/orch-infra/tests/source_budget.rs",
        Verdict::Scoped(
            "each case measures its own child's VmHWM under its own rlimit \
             (:42); the 30 s budget is a wall clock, which a loaded host can \
             miss and which is then diagnosed, never retried",
        ),
    ),
    (
        "crates/orch-infra/tests/token_eco_adapter_probe.rs",
        Verdict::PerProcess(
            "GATE at :521 guards an in-process hazard — a fork on another thread \
             inheriting a fresh pipe write end — which cannot cross processes",
        ),
    ),
    (
        "xtask/src/gate/contention.rs",
        Verdict::Production(
            "the gate's own contention sampler: it is the code under test, and \
             the tests over it assert that foreign activity is *recorded*, not \
             that there is none",
        ),
    ),
    (
        "xtask/src/gate/report.rs",
        Verdict::Production("the gate's own marker diagnostics name lsof as a read-only hint"),
    ),
    (
        "xtask/src/gate/steps.rs",
        Verdict::Production("the gate's own direct-child lifecycle, which the fixtures drive"),
    ),
    (
        "xtask/src/gate/target.rs",
        Verdict::Production("the gate's own marker inspection text"),
    ),
    (
        "xtask/src/gate/target/qualification.rs",
        Verdict::Production("the gate's own holder-command diagnostics"),
    ),
    (
        "xtask/tests/gate_bootstrap.rs",
        Verdict::Scoped("asserts on pids this fixture started, in its own sandbox"),
    ),
    (
        "xtask/tests/gate_lease.rs",
        Verdict::Scoped(
            "every fixture gets its own XTASK_GATE_LEASE_DIR, its own synthetic \
             repository and CARGO_TARGET_DIR, and a /tmp scratch named by pid, \
             nanos and a counter; the process reads name pids it spawned",
        ),
    ),
    (
        "xtask/tests/gate_recovery.rs",
        Verdict::Scoped("the reads at :256,:287,:291 name pids this fixture started"),
    ),
];

/// Helpers whose meaning is host-wide: a call to one sees every `orch` process
/// on the host, so its caller needs host-wide exclusivity.
const UNSCOPED: &[&str] = &[
    "assert_only_clients(",
    ".new_orch()",
    // A fresh emptiness assertion over the host-wide set, however it is spelt.
    "orch_processes(&process_table()).is_empty()",
];

/// Files that define or wrap the unscoped helpers rather than depending on
/// their host-wide meaning.
const UNSCOPED_DEFINITIONS: &[&str] = &[
    "crates/orch-cli/tests/daemon_support/frontends.rs",
    "crates/orch-cli/tests/daemon_support/mod.rs",
];

/// Callers of an unscoped helper that only test membership of a pid they
/// already know, which a neighbour's process cannot change.
const UNSCOPED_MEMBERSHIP_ONLY: &[&str] = &["crates/orch-cli/tests/daemon_nodaemon_matrix.rs"];

fn workspace() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace root")
        .to_path_buf()
}

fn sources(root: &Path) -> Vec<(String, Vec<String>)> {
    fn walk(base: &Path, dir: &Path, out: &mut Vec<(String, Vec<String>)>) {
        let mut entries: Vec<_> = fs::read_dir(dir)
            .expect("readable source directory")
            .map(|e| e.expect("source entry").path())
            .collect();
        entries.sort();
        for path in entries {
            if path.is_dir() {
                walk(base, &path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                let relative = path
                    .strip_prefix(base)
                    .expect("inside the workspace")
                    .to_string_lossy()
                    .into_owned();
                if relative == SELF {
                    continue;
                }
                let text = fs::read_to_string(&path).expect("UTF-8 source");
                out.push((relative, text.lines().map(str::to_owned).collect()));
            }
        }
    }
    let mut out = Vec::new();
    for base in ["crates", "xtask"] {
        walk(root, &root.join(base), &mut out);
    }
    assert!(
        out.len() > 100,
        "source scan found only {} files",
        out.len()
    );
    out
}

/// A `static` item declaration that holds a lock. rustfmt may wrap it, so the
/// type is looked for in a small window rather than on the declaring line.
fn declares_guarded_static(lines: &[String], at: usize) -> bool {
    let line = lines[at].trim_start();
    let declares = line.starts_with("static ")
        || line.starts_with("pub static ")
        || (line.starts_with("pub(") && line.contains(") static "));
    if !declares {
        return false;
    }
    let window = lines[at..lines.len().min(at + 3)].join(" ");
    let declaration = window
        .split_once('=')
        .map_or_else(|| window.clone(), |(before, _)| before.to_owned());
    declaration.contains("Mutex") || declaration.contains("RwLock")
}

/// A read of the host's process table or of `/proc`. This codebase's own idiom
/// is `process_table()` and `orch_processes(`, so those count wherever they are
/// called, not only where they are defined.
fn reads_the_host(line: &str) -> bool {
    [
        "read_dir(\"/proc\")",
        "/bin/ps",
        "/usr/bin/ps",
        "Command::new(\"ps\")",
        "pgrep",
        "lsof",
        "process_table()",
        "orch_processes(",
        "\"/proc",
        "/proc/{",
    ]
    .iter()
    .any(|pattern| line.contains(pattern))
}

/// A bind of an ephemeral port whose listener is **not** kept: the address is
/// taken and released, so something rebinds it later and races the host for it.
/// A bind held in a binding reserves the port instead, which is safe.
fn releases_an_ephemeral_port(lines: &[String], at: usize) -> bool {
    let line = &lines[at];
    if !line.contains("bind(") || !(line.contains(":0\"") || line.contains(", 0)")) {
        return false;
    }
    let window = lines[at.saturating_sub(1)..=at].join(" ");
    !window.contains("let ")
}

fn hits(lines: &[String]) -> Vec<usize> {
    (0..lines.len())
        .filter(|&i| {
            declares_guarded_static(lines, i)
                || reads_the_host(&lines[i])
                || releases_an_ephemeral_port(lines, i)
        })
        .map(|i| i + 1)
        .collect()
}

/// The pass split as the configuration defines it: for each profile that sets
/// a `default-filter`, the filter text and the binaries it names.
fn split() -> BTreeMap<String, (String, BTreeSet<String>)> {
    split_of(CONFIG)
}

fn split_of(config: &str) -> BTreeMap<String, (String, BTreeSet<String>)> {
    let mut out = BTreeMap::new();
    let mut profile: Option<String> = None;
    for raw in config.lines() {
        let line = raw.trim();
        if let Some(rest) = line.strip_prefix("[profile.") {
            profile = Some(
                rest.split_once(']')
                    .expect("a profile header is closed")
                    .0
                    .to_owned(),
            );
            continue;
        }
        // Only a profile table's own filter, never an override's.
        if line.starts_with('[') {
            profile = None;
            continue;
        }
        let Some(rest) = line.strip_prefix("default-filter = ") else {
            continue;
        };
        let name = profile
            .clone()
            .expect("a default-filter belongs to a profile table");
        let filter = rest.trim_matches('\'').to_owned();
        let inner = filter
            .split_once("binary(")
            .expect("a pass filter names its binaries with binary(...)")
            .1
            .split_once(')')
            .expect("binary(...) is closed")
            .0
            .trim_start_matches("/^(")
            .trim_end_matches(")$/");
        let binaries: BTreeSet<String> = inner.split('|').map(str::to_owned).collect();
        for binary in &binaries {
            assert!(
                !binary.is_empty() && binary.chars().all(|c| c.is_alphanumeric() || c == '_'),
                "unsupported binary spelling {binary:?} in the {name} pass filter"
            );
        }
        assert!(
            out.insert(name.clone(), (filter, binaries)).is_none(),
            "{name} sets default-filter twice"
        );
    }
    out
}

/// The binaries that run in a pass of their own, i.e. every profile's filter
/// but the first pass's.
fn alone_binaries() -> BTreeSet<String> {
    let split = split();
    split
        .iter()
        .filter(|(name, _)| *name != "default")
        .flat_map(|(_, (_, binaries))| binaries.clone())
        .collect()
}

/// Binary names an override names, by the kind of isolation it gives them.
fn configured() -> BTreeMap<&'static str, BTreeSet<String>> {
    let mut out: BTreeMap<&'static str, BTreeSet<String>> = BTreeMap::new();
    let mut filter: Option<&str> = None;
    for raw in CONFIG.lines() {
        let line = raw.trim();
        if let Some(rest) = line.strip_prefix("filter = ") {
            filter = Some(rest);
            continue;
        }
        let kind = if line.starts_with("threads-required") {
            "exclusive"
        } else if line.starts_with("test-group") {
            "group"
        } else {
            continue;
        };
        let expression = filter.take().expect("an override sets its filter first");
        let inner = expression
            .split_once("binary(")
            .expect("an override names its binaries with binary(...)")
            .1;
        // A filter that also names one test is a test-level entry: it does not
        // make its whole binary exclusive, and is recorded separately.
        let single = expression.split_once("test(=").map(|(_, rest)| {
            rest.split_once(')')
                .expect("test(=...) is closed")
                .0
                .to_owned()
        });
        let inner = inner
            .split_once(')')
            .expect("binary(...) is closed")
            .0
            .trim_start_matches("/^(")
            .trim_end_matches(")$/");
        for name in inner.split('|') {
            assert!(
                !name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || c == '_'),
                "unsupported binary spelling {name:?}; teach this guard the new form"
            );
            match (&single, kind) {
                (Some(test), "exclusive") => {
                    out.entry("exclusive-test")
                        .or_default()
                        .insert(format!("{name}::{test}"));
                }
                _ => {
                    out.entry(kind).or_default().insert(name.to_owned());
                }
            }
        }
    }
    // A group's threads must really be one: a group that lets two of its tests
    // run at once restores nothing.
    for group in CONFIG.split("[test-groups.").skip(1) {
        let body = group.split("\n[").next().expect("group body");
        assert!(
            body.contains("max-threads = 1"),
            "a test group must be max-threads = 1: {body}"
        );
    }
    assert!(
        out.contains_key("exclusive") && out.contains_key("group"),
        "the configuration lost one of its two isolation forms"
    );
    out
}

/// `crates/<package>/tests/<name>.rs` is the test binary `<name>`. Anything
/// deeper is a shared module and owns no binary of its own.
fn owning_binary(path: &str) -> Option<&str> {
    // Unit tests in this module belong to xtask's binary, not an integration
    // test named qualification. Keep its two matrix overrides test-scoped.
    if path == "xtask/src/gate/target/qualification.rs" {
        return Some("xtask");
    }
    let rest = path.strip_prefix("crates/")?;
    let (_, rest) = rest.split_once('/')?;
    let rest = rest.strip_prefix("tests/")?;
    (!rest.contains('/'))
        .then(|| rest.strip_suffix(".rs"))
        .flatten()
}

#[test]
fn every_isolation_pattern_is_classified_and_the_configuration_matches() {
    let files = sources(&workspace());
    let ledger: BTreeMap<&str, Verdict> = LEDGER.iter().copied().collect();
    assert_eq!(ledger.len(), LEDGER.len(), "duplicate ledger row");
    let config = configured();
    let alone = alone_binaries();
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    for (path, lines) in &files {
        let found = hits(lines);
        if found.is_empty() {
            continue;
        }
        let Some(verdict) = ledger.get(path.as_str()) else {
            panic!(
                "{path}:{found:?} takes a process-global lock or reads the host process table, \
                 and nextest runs every test in its own process. Either name its test binary \
                 in .config/nextest.toml (threads-required for a host-wide observation, a \
                 max-threads = 1 test group for in-binary serialisation, or a pass of its \
                 own to run alone with its own tests in parallel) and add an Exclusive, \
                 Group or AlonePass row to LEDGER in {SELF} stating the hazard, or add a \
                 Scoped/PerProcess/Production row saying why it needs no entry."
            );
        };
        seen.insert(path.as_str());
        let (kind, hazard) = match verdict {
            Verdict::Exclusive(why) => ("exclusive", *why),
            Verdict::Group(why) => ("group", *why),
            Verdict::AlonePass(why) => ("alone", *why),
            Verdict::Scoped(_) | Verdict::PerProcess(_) | Verdict::Production(_) => continue,
        };
        // The hazard, not the lock: a row resting on an inert static would tell
        // the next reader that deleting it removes the need for the entry.
        assert!(
            hazard.len() > 40 && !hazard.contains("static SERIAL guards"),
            "{path}: an {kind} row must state the hazard it answers"
        );
        let binary = owning_binary(path).unwrap_or_else(|| {
            panic!("{path} is a shared module and cannot own a configuration entry")
        });
        if kind == "alone" {
            assert!(
                alone.contains(binary),
                "{path} is classified alone but .config/nextest.toml does not give {binary} \
                 a pass of its own"
            );
            // Running alone and being serialised are different answers; a
            // binary given both would pay for exclusivity it does not need.
            for other in ["exclusive", "group"] {
                assert!(
                    !config
                        .get(other)
                        .is_some_and(|names| names.contains(binary)),
                    "{binary} runs in its own pass and must not also be {other}"
                );
            }
            continue;
        }
        assert!(
            config[kind].contains(binary),
            "{path} is classified {kind} but .config/nextest.toml does not give {binary} \
             that isolation"
        );
    }
    let stale: Vec<_> = ledger.keys().filter(|path| !seen.contains(*path)).collect();
    assert!(
        stale.is_empty(),
        "stale LEDGER rows: these files no longer match any pattern, so the row is a lie: \
         {stale:?}"
    );
    // A binary given a pass of its own must be explained too, or the split
    // could quietly grow a binary nobody classified.
    for name in &alone {
        assert!(
            ledger.iter().any(|(path, verdict)| {
                owning_binary(path) == Some(name.as_str())
                    && matches!(verdict, Verdict::AlonePass(_))
            }),
            "{name} runs in a pass of its own but has no AlonePass LEDGER row explaining it"
        );
    }
    // Every configured binary must be in the ledger, so the configuration
    // cannot carry an entry nobody can explain.
    for (kind, names) in &config {
        // Test-level exclusive entries are explained by EXCLUSIVE_TESTS, which
        // the port-reuse test checks in both directions.
        if *kind == "exclusive-test" {
            continue;
        }
        for name in names {
            assert!(
                ledger.iter().any(|(path, verdict)| {
                    owning_binary(path) == Some(name.as_str())
                        && matches!(
                            (*kind, verdict),
                            ("exclusive", Verdict::Exclusive(_)) | ("group", Verdict::Group(_))
                        )
                }),
                "{kind} entry for {name} has no LEDGER row explaining it"
            );
        }
    }
}

#[test]
fn an_unscoped_process_table_helper_is_only_called_with_host_wide_exclusivity() {
    let files = sources(&workspace());
    let config = configured();
    let mut callers = 0;
    for (path, lines) in &files {
        if UNSCOPED_DEFINITIONS.contains(&path.as_str()) {
            continue;
        }
        let found: Vec<_> = lines
            .iter()
            .enumerate()
            .filter(|(_, line)| UNSCOPED.iter().any(|helper| line.contains(helper)))
            .map(|(n, _)| n + 1)
            .collect();
        if found.is_empty() {
            continue;
        }
        callers += 1;
        if UNSCOPED_MEMBERSHIP_ONLY.contains(&path.as_str()) {
            continue;
        }
        let binary = owning_binary(path)
            .unwrap_or_else(|| panic!("{path}:{found:?} calls an unscoped helper from a module"));
        assert!(
            config["exclusive"].contains(binary),
            "{path}:{found:?} calls an unscoped process-table helper, which sees every orch \
             process on the host, but .config/nextest.toml does not give {binary} \
             threads-required = \"num-test-threads\""
        );
    }
    assert!(callers >= 2, "found only {callers} callers; scan is broken");
}

#[test]
fn a_released_ephemeral_port_is_only_reused_with_host_wide_exclusivity() {
    let files = sources(&workspace());
    let config = configured();
    let exclusive_tests: BTreeMap<&str, &str> = EXCLUSIVE_TESTS.iter().copied().collect();
    let alone = alone_binaries();
    // Every test-level exclusive entry in the configuration is explained here,
    // and every entry here is in the configuration.
    let configured_tests = config.get("exclusive-test").cloned().unwrap_or_default();
    for (path, test) in EXCLUSIVE_TESTS {
        let binary = owning_binary(path).expect("a test-level entry names a binary");
        assert!(
            configured_tests.contains(&format!("{binary}::{test}")),
            "{path}: {test} is listed host-exclusive but the configuration does not give it \
             threads-required at test level"
        );
    }
    assert_eq!(
        configured_tests.len(),
        EXCLUSIVE_TESTS.len(),
        "a test-level exclusive entry in the configuration has no EXCLUSIVE_TESTS row: \
         {configured_tests:?}"
    );
    // And every caller of a release-and-reuse helper is covered one way or the
    // other. The helper's own file defines it and is exempt.
    let mut callers = 0;
    for (path, lines) in &files {
        if UNSCOPED_DEFINITIONS.contains(&path.as_str()) {
            continue;
        }
        let found: Vec<_> = lines
            .iter()
            .enumerate()
            .filter(|(_, line)| PORT_REUSE_HELPERS.iter().any(|h| line.contains(h)))
            .map(|(n, _)| n + 1)
            .collect();
        if found.is_empty() {
            continue;
        }
        callers += 1;
        let binary = owning_binary(path)
            .unwrap_or_else(|| panic!("{path}:{found:?} reuses a released port from a module"));
        assert!(
            config["exclusive"].contains(binary)
                || alone.contains(binary)
                || exclusive_tests.contains_key(path.as_str()),
            "{path}:{found:?} rebinds a released ephemeral port, which races every other \
             process on the host for it, but .config/nextest.toml neither makes {binary} \
             host-exclusive, nor gives it a pass of its own, nor makes the calling test \
             host-exclusive"
        );
    }
    assert!(callers >= 1, "found no caller; the helper scan is broken");
    // And the other direction: a binary in the alone pass must actually have
    // the hazard that earns it one, so the split cannot be used to quarantine
    // an ordinary flake.
    for binary in &alone {
        let (owner, lines) = files
            .iter()
            .find(|(path, _)| owning_binary(path) == Some(binary.as_str()))
            .unwrap_or_else(|| panic!("{binary} is in the alone pass but has no source file"));
        let hazard = (0..lines.len()).any(|i| releases_an_ephemeral_port(lines, i))
            || lines.iter().any(|line| {
                PORT_REUSE_HELPERS.iter().any(|h| line.contains(h))
                    || UNSCOPED.iter().any(|idiom| line.contains(idiom))
            });
        assert!(
            hazard,
            "{owner} is in the alone pass but calls no release-and-reuse port helper and \
             makes no unscoped host-wide observation: a pass of its own is not the remedy"
        );
    }
}

/// The configuration's tables, in order: `(header, body keys and values)`.
/// The preamble before the first header is the table named `""`.
fn tables(config: &str) -> Vec<(String, Vec<String>)> {
    let mut out: Vec<(String, Vec<String>)> = vec![(String::new(), Vec::new())];
    for raw in config.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') {
            out.push((line.to_owned(), Vec::new()));
        } else {
            out.last_mut()
                .expect("a table to hold the line")
                .1
                .push(line.to_owned());
        }
    }
    out
}

/// Which tables may exist and which keys each may carry. With two profiles, a
/// whole-file string match proves nothing: `retries = 3` or a leak-timeout that
/// forgives leaks would satisfy every such check from inside `[profile.alone]`,
/// and the gate forces neither flag on the command line for the ambient run.
/// So the second profile may be nothing but `inherits` plus its half of the
/// split, and every other table is named here with the keys it may set.
fn assert_tables_are_pinned(config: &str) {
    for (header, body) in tables(config) {
        let allowed: &[&str] = match header.as_str() {
            "" => &["nextest-version"],
            "[profile.default]" => &[
                "retries",
                "fail-fast",
                "default-filter",
                "leak-timeout",
                "status-level",
                "final-status-level",
                "failure-output",
                "success-output",
            ],
            // Nothing but the inheritance and its half of the split: every
            // setting the first pass is judged by therefore applies to it too.
            "[profile.alone]" => &["inherits", "default-filter"],
            "[[profile.default.overrides]]" => {
                &["filter", "threads-required", "test-group", "priority"]
            }
            group if group.starts_with("[test-groups.") && group.ends_with(']') => &["max-threads"],
            other => panic!(
                "{other} is not a table this configuration may have: only \
                 [profile.default], [profile.alone], [[profile.default.overrides]] and \
                 [test-groups.*]. A second profile's overrides, or a third profile pulled \
                 in by `inherits`, would carry isolation rules no reader of this guard \
                 expects."
            ),
        };
        for line in &body {
            let key = line
                .split_once(" = ")
                .unwrap_or_else(|| panic!("{header}: {line} is not a `key = value` line"))
                .0;
            assert!(
                allowed.contains(&key),
                "{header} may not set {key}; it may set only {allowed:?}"
            );
        }
        if header == "[profile.alone]" {
            assert_eq!(
                body.len(),
                2,
                "[profile.alone] must be exactly `inherits` and its half of the split, not {body:?}"
            );
            assert!(
                body.contains(&"inherits = \"default\"".to_owned()),
                "[profile.alone] must inherit the first pass's settings, not restate them: {body:?}"
            );
        }
    }
}

#[test]
fn the_configuration_keeps_the_standing_rules() {
    let settings: String = CONFIG
        .lines()
        .filter(|line| !line.trim_start().starts_with('#'))
        .map(|line| format!("{line}\n"))
        .collect();
    // Every whole-file check below is only sound because this pins where a
    // setting may live: see `assert_tables_are_pinned`.
    assert_tables_are_pinned(CONFIG);
    assert!(settings.contains("retries = 0"), "retries must stay 0");
    assert!(
        !settings.contains("terminate-after"),
        "no timeout may terminate a test: a hang must stay a hang"
    );
    // A leak may be reported, never forgiven: nextest kills nothing for it, so
    // failing on one cannot mask a hang.
    assert!(
        !settings.contains("leak-timeout")
            || settings.contains("leak-timeout = { period = \"1s\", result = \"fail\" }"),
        "a leak-timeout must fail the leaking test, never downgrade it"
    );
    // A default filter may exist only as the pass split, and only as two
    // complementary halves: anything else would drop tests from the inventory.
    // The gate judges the union of the passes against an unfiltered listing, so
    // a test the split loses has no result and refuses the run — but that is a
    // run-time refusal, and this is the design rule.
    assert_split_is_complementary(CONFIG);
    assert!(
        settings.contains("nextest-version = { required = \"0.9.152\" }"),
        "the configuration must refuse an older runner than the pin"
    );
    // The pass-path canary tests rest on a passing test's output staying out
    // of the evidence. The gate also forces this on the command line.
    assert!(
        settings.contains("success-output = \"never\""),
        "a passing test's output must stay out of the evidence"
    );
    // A setup or wrapper script can replace the command that runs the tests,
    // and an experimental feature can change what the stream means. Neither
    // belongs in a file the gate trusts to describe isolation only.
    for forbidden in ["scripts", "experimental"] {
        assert!(
            !settings.contains(forbidden),
            "{forbidden} may not appear in the runner configuration: it can \
             replace or reshape the test invocation the gate judges"
        );
    }
}

/// A `default-filter` may exist only as the pass split, and only as two
/// complementary halves: anything else would drop tests from the inventory. The
/// gate judges the union of the passes against an unfiltered listing, so a test
/// the split loses has no result and refuses the run — but that is a run-time
/// refusal, and this is the design rule.
fn assert_split_is_complementary(config: &str) {
    let settings: String = config
        .lines()
        .filter(|line| !line.trim_start().starts_with('#'))
        .map(|line| format!("{line}\n"))
        .collect();
    let split = split_of(config);
    assert_eq!(
        settings.matches("default-filter").count(),
        split.len(),
        "a default-filter outside a profile table is not the pass split"
    );
    assert_eq!(split.len(), 2, "the split is exactly two passes: {split:?}");
    let first = split.get("default").expect("a first pass");
    let rest = split.get("alone").expect("a second pass");
    // The complement is checked as text, so the alone pass's filter has to be a
    // single `binary(...)` atom: `not A & B` parses as `(not A) & B`, so a
    // conjunction here would make `not <filter>` a different set from the
    // complement of `<filter>` and leave tests in neither pass.
    assert!(
        is_binary_atom(&rest.0),
        "the alone pass's filter must be a single binary(...) atom, not {:?}: \
         only then is `not <filter>` its exact complement",
        rest.0
    );
    assert_eq!(
        first.0,
        format!("not {}", rest.0),
        "the two passes' filters must be exact complements, so no test is in neither or both"
    );
    assert_eq!(
        first.1, rest.1,
        "the complements must name the same binaries"
    );
}

/// One `binary(...)` predicate and nothing else: the whole string is the atom,
/// with its parentheses balanced.
fn is_binary_atom(filter: &str) -> bool {
    let Some(inner) = filter.strip_prefix("binary(") else {
        return false;
    };
    let mut depth = 1usize;
    for (at, c) in inner.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    // The closing parenthesis must be the last character, or
                    // something follows the atom.
                    return at + 1 == inner.len();
                }
            }
            _ => {}
        }
    }
    false
}

/// A split that leaves a test out of both passes, or puts one in both, must be
/// rejected here and not left to the gate's union check at run time.
#[test]
fn a_split_that_is_not_a_partition_is_rejected() {
    assert_split_is_complementary(CONFIG);
    for mutant in [
        // Both passes would run remote_peers.
        CONFIG.replacen("not binary(remote_peers)", "binary(remote_peers)", 1),
        // Neither pass would: the first excludes it, the second names another.
        CONFIG.replacen("'binary(remote_peers)'", "'binary(daemon_admission)'", 1),
        // A third filter, which the two-pass gate would never run.
        CONFIG.replacen(
            "[profile.alone]",
            "[profile.third]\ndefault-filter = 'binary(daemon_admission)'\n\n[profile.alone]",
            1,
        ),
        // The split dropped altogether, leaving a filter that only subtracts.
        CONFIG.replacen("default-filter = 'binary(remote_peers)'\n", "", 1),
        // A conjunction in the alone pass: `not A & B` is `(not A) & B`, so
        // `not` of this text is not its complement and tests fall in neither
        // pass, even though both halves still mention the same binary.
        CONFIG.replacen(
            "default-filter = 'binary(remote_peers)'",
            "default-filter = 'binary(remote_peers) & test(=s23b_sigterm_run_watch_while_offline_keeps_the_remote_run)'",
            1,
        ),
    ] {
        assert_ne!(mutant, CONFIG);
        assert!(
            std::panic::catch_unwind(|| assert_split_is_complementary(&mutant)).is_err(),
            "a split that is not a partition must be rejected:\n{mutant}"
        );
    }
}

/// With two profiles, a setting in the wrong one escapes every whole-file
/// check. These are the four ways that happens, and each must be rejected by
/// the table whitelist rather than by anyone noticing.
#[test]
fn a_setting_in_the_second_profile_or_a_third_table_is_rejected() {
    assert_tables_are_pinned(CONFIG);
    for (why, mutant) in [
        // nextest honours a per-profile leak-timeout, so this would let the
        // alone pass forgive a leak. The gate forces no leak-timeout flag.
        (
            "a leak-timeout that forgives leaks, in the second profile",
            CONFIG.replacen(
                "[profile.alone]\ninherits = \"default\"",
                "[profile.alone]\ninherits = \"default\"\nleak-timeout = { period = \"1s\", result = \"pass\" }",
                1,
            ),
        ),
        // The gate forces --retries 0 on both passes, but the ambient CI job
        // does not, so this would retry there.
        (
            "retries in the second profile",
            CONFIG.replacen(
                "[profile.alone]\ninherits = \"default\"",
                "[profile.alone]\ninherits = \"default\"\nretries = 3",
                1,
            ),
        ),
        // Overrides under the second profile: the first pass would then run
        // without the exclusivity its three binaries need.
        (
            "the exclusive overrides moved to the second profile",
            CONFIG.replace("[[profile.default.overrides]]", "[[profile.alone.overrides]]"),
        ),
        // A third profile is never run, but `inherits` makes it reachable and
        // a reader would take its rules for the suite's.
        (
            "a third profile",
            CONFIG.replacen(
                "[profile.alone]",
                "[profile.third]\ninherits = \"alone\"\nretries = 5\n\n[profile.alone]",
                1,
            ),
        ),
    ] {
        assert_ne!(mutant, CONFIG, "{why}");
        assert!(
            std::panic::catch_unwind(|| assert_tables_are_pinned(&mutant)).is_err(),
            "{why} must be rejected:\n{mutant}"
        );
    }
}

/// The split is defined once, in `.config/nextest.toml`. Its two other readers
/// — the gate and the ambient CI job — must run exactly the passes it defines,
/// in the same order, or one of them would silently judge a different suite.
#[test]
fn the_gate_and_the_ambient_job_run_exactly_the_configured_passes() {
    let configured = split();
    let profiles: Vec<&str> = configured.keys().map(String::as_str).collect();
    // The gate's list, in order, parsed from the constant it runs from.
    let line = GATE
        .lines()
        .find_map(|l| {
            l.trim()
                .strip_prefix("pub(super) const PROFILES: &[&str] = &[")
        })
        .expect("the gate declares its passes in one place");
    let gate: Vec<String> = line
        .trim_end_matches("];")
        .split(',')
        .map(|name| name.trim().trim_matches('"').to_owned())
        .filter(|name| !name.is_empty())
        .collect();
    // `default` first: the other passes run alone, so they must come after the
    // pass that runs everything else.
    assert_eq!(
        gate,
        vec!["default".to_owned(), "alone".to_owned()],
        "the gate must run the configured passes, default first"
    );
    assert_eq!(
        gate.iter().map(String::as_str).collect::<BTreeSet<_>>(),
        profiles.iter().copied().collect::<BTreeSet<_>>(),
        "the gate and .config/nextest.toml disagree about which passes exist"
    );
    // The ambient job runs the same passes, in the same order, and no unsplit
    // run beside them.
    let ambient: Vec<String> = CI
        .lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .filter_map(|l| l.split_once("cargo nextest run"))
        .map(|(_, rest)| {
            rest.split_whitespace()
                .skip_while(|word| *word != "--profile")
                .nth(1)
                .unwrap_or("<unsplit>")
                .to_owned()
        })
        .collect();
    assert_eq!(
        ambient, gate,
        "the ambient CI job must run the configured passes, in order, and nothing else"
    );
    // The gate's own listing must be unfiltered, or the expected set would be
    // one pass's half and the union check would prove nothing. Both the
    // constant and the one command that has to carry it: declaring it and not
    // passing it would leave the expected set filtered.
    assert!(
        GATE.contains("pub(super) const UNFILTERED: &str = \"--ignore-default-filter\";"),
        "the gate must take its expected set from an unfiltered listing"
    );
    // Every pass is built by the one command inside the loop over the gate's
    // profile list, so the flags that cannot be left to the configuration are
    // on all of them by construction. A second `nextest run` built elsewhere
    // would escape that, and so would a flag moved out of it.
    assert_eq!(
        SUITE.matches("\"nextest\", \"run\"").count(),
        1,
        "the gate must build its `nextest run` in exactly one place"
    );
    let run = SUITE
        .split_once("for profile in nextest::PROFILES")
        .expect("the gate builds one run per configured pass, in one place")
        .1;
    let run = &run[..run
        .find("\"nextest\", \"list\"")
        .expect("the listing follows the passes")];
    // Whitespace-normalised, so a reformat cannot break this and a renamed or
    // dropped flag still can.
    let run: String = run.split_whitespace().collect::<Vec<_>>().join(" ");
    for forced in [
        "\"--profile\", profile,",
        "\"--retries\", \"0\",",
        "\"--no-tests\", \"fail\",",
        "\"--success-output\", \"never\",",
        "\"--status-level\", \"all\",",
        "\"--user-config-file\", \"none\",",
        "\"--config-file\",",
    ] {
        assert!(
            run.contains(forced),
            "every pass must force {forced:?} on the command line, not inherit it"
        );
    }
    let listing = SUITE
        .split_once("\"nextest\", \"list\"")
        .expect("the gate lists the workspace once")
        .1;
    let listing = &listing[..listing
        .find("\"--message-format\"")
        .expect("the listing asks for a machine-readable format")];
    assert!(
        listing.contains("nextest::UNFILTERED"),
        "the gate's `nextest list` must pass UNFILTERED, or the expected set is one pass's half"
    );
}

/// The detector has to see this codebase's own idioms, not only the shapes it
/// was first written against. Every line here escaped an earlier version.
#[test]
fn the_detector_sees_the_idioms_that_escaped_it() {
    for line in [
        "    assert!(orch_processes(&process_table()).is_empty());",
        "    let table = process_table();",
        "    let out = Command::new(\"ps\").args([\"-axo\", \"pid=\"]).output();",
        "    let out = Command::new(\"/bin/ps\").output();",
        "    let found = pgrep_orch();",
        "    let entries = std::fs::read_dir(\"/proc\")?;",
    ] {
        assert!(reads_the_host(line), "missed a host-wide read: {line}");
    }
    assert!(
        !reads_the_host("    let mut counter = 0; // nothing host-wide here"),
        "false positive on ordinary code"
    );
    // A rustfmt-wrapped static declaration counts; a `&'static` field and a
    // lock-free static do not.
    let wrapped: Vec<String> = [
        "static GATE: std::sync::LazyLock<tokio::sync::RwLock<()>> =",
        "    std::sync::LazyLock::new(|| tokio::sync::RwLock::new(()));",
    ]
    .iter()
    .map(|s| (*s).to_owned())
    .collect();
    assert!(
        declares_guarded_static(&wrapped, 0),
        "missed a wrapped static"
    );
    for line in [
        "    resolved_command: Mutex<&'static str>,",
        "static COUNT: AtomicU64 = AtomicU64::new(0);",
    ] {
        let one: Vec<String> = [line.to_owned()].into();
        assert!(
            !declares_guarded_static(&one, 0),
            "false positive on: {line}"
        );
    }
    // The unscoped call-site rule must see a fresh emptiness assertion too.
    assert!(
        UNSCOPED
            .iter()
            .any(|h| "assert!(orch_processes(&process_table()).is_empty());".contains(h)),
        "the unscoped list must cover a bare host-wide emptiness assertion"
    );
}

/// Pin the exact two expensive matrices and their reservation, so weakening
/// `threads-required` to a group or a fixed count cannot silently pass the
/// ledger's classification checks.
fn assert_holder_matrices_are_exclusive(config: &str) {
    let actual: BTreeSet<Vec<String>> = tables(config)
        .into_iter()
        .filter(|(header, body)| {
            header == "[[profile.default.overrides]]"
                && body
                    .iter()
                    .any(|line| line.contains("production_holder_commands_and_discriminators"))
        })
        .map(|(_, body)| body)
        .collect();
    let expected: BTreeSet<Vec<String>> = ["scratch", "worktree"]
        .into_iter()
        .map(|variant| {
            vec![
                format!("filter = 'package(xtask) & binary(xtask) & test(=gate::target::qualification::production_holder_commands_and_discriminators_on_{variant}_filesystem)'"),
                r#"threads-required = "num-test-threads""#.to_owned(),
                "priority = 100".to_owned(),
            ]
        })
        .collect();
    assert_eq!(
        actual, expected,
        "#222 matrices must each reserve the whole nextest host"
    );
}

#[test]
fn holder_matrices_reserve_the_host_without_serialising_other_xtask_tests() {
    assert_holder_matrices_are_exclusive(CONFIG);
}
