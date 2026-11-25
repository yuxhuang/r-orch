//! Strict cargo-nextest inventory. Child chatter can cause a false failure,
//! never an inferred pass: the authoritative set of tests is nextest's own
//! listing, every listed test needs exactly one terminal event, and the
//! listing's `status` and `filter-match` fields are checked so no test can be
//! dropped silently by a filter or by a binary that failed its `--list` query.
//!
//! The run's machine-readable stream is `--message-format libtest-json-plus`,
//! which nextest still calls experimental and gates behind
//! `NEXTEST_EXPERIMENTAL_LIBTEST_JSON`. That is acceptable only because the
//! gate refuses any version but the pinned [`VERSION`], so the shape cannot
//! change under a passing gate. The listing
//! (`cargo nextest list --message-format json`), which is what the inventory
//! is judged against, is not experimental.
//!
//! The suite runs in two passes ([`PROFILES`]), because one test binary needs
//! the whole host to itself while its own tests run in parallel; the split is
//! defined once, in `.config/nextest.toml`. The listing is taken **once and
//! unfiltered** (`--ignore-default-filter`), so it stays the complete expected
//! set, and the passes' event streams are judged as one union: a test that no
//! pass ran has no result, and a test two passes ran is a duplicate. Both
//! refuse. Per-suite accounting is per-pass accounting, because the split is by
//! binary and so no suite appears in both passes.

use super::inventory::{self, Row};
use anyhow::{Context, Result, bail, ensure};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

/// The only accepted runner version. A different one fails the gate closed.
pub(super) const VERSION: &str = "0.9.152";
/// Opt-in for `--message-format libtest-json-plus`, set on the one command
/// that needs it so the judged environment record stays what the hermetic
/// root declares.
pub(super) const EXPERIMENT: (&str, &str) = ("NEXTEST_EXPERIMENTAL_LIBTEST_JSON", "1");
/// Pinned structured-output version: a later nextest cannot reshape the stream
/// without also failing the version pin.
pub(super) const FORMAT_VERSION: &str = "0.1";
/// One `nextest run` per profile, in this order. The profiles' complementary
/// `default-filter`s in `.config/nextest.toml` are what the split *is*; this
/// list only says which profiles to run, and `xtask/tests/nextest_isolation.rs`
/// refuses a config, a gate or an ambient CI job that disagree with it.
pub(super) const PROFILES: &[&str] = &["default", "alone"];
/// Makes `nextest list` ignore both profiles' filters, so the expected set is
/// the whole workspace however the passes are split.
pub(super) const UNFILTERED: &str = "--ignore-default-filter";

pub(super) fn install_hint() -> String {
    format!(
        "install the pinned runner with `cargo install cargo-nextest --locked --version {VERSION}`; it is the only suite runner, so there is nothing to fall back to"
    )
}

/// `cargo-nextest 0.9.152 (26ebeda79 2026-11-13)` and nothing else.
pub(super) fn pinned(version_output: &str) -> Result<String> {
    let line = version_output
        .lines()
        .next()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .context("cargo-nextest printed no version")?;
    let rest = line.strip_prefix("cargo-nextest ").with_context(|| {
        format!(
            "unrecognised cargo-nextest version line; {}",
            install_hint()
        )
    })?;
    let found = rest.split_whitespace().next().unwrap_or(rest);
    ensure!(
        found == VERSION,
        "cargo-nextest {found} is not the pinned {VERSION}; {}",
        install_hint()
    );
    Ok(line.to_owned())
}

// ── framing ────────────────────────────────────────────────────────────────
//
// One `steps/test.log` holds everything the inventory is rebuilt from, so a
// receipt verifier needs no extra files and no extra result fields: one leading
// JSON record per pass (its profile, exit status, the number of event lines it
// produced and its operator-facing runner log), then one for the whole separate
// doc-test run with its own status, then the passes' raw event streams in order.
// The line counts are what put each event back with the pass that produced it,
// so a pass's own exit status can be judged against its own stream.

const PASS: &str = "gate-nextest-pass";
const DOC: &str = "gate-doc-tests";

/// One `nextest run`: which profile it was, what it exited with, its captured
/// stderr and its event stream.
pub(super) struct Pass<'a> {
    pub(super) profile: &'a str,
    pub(super) exit: Option<i32>,
    pub(super) events: &'a [u8],
    pub(super) stderr: &'a [u8],
}

/// One pass as the frame gives it back: its exit status and its own events.
pub(super) struct Ran {
    pub(super) exit: i32,
    pub(super) events: String,
}

pub(super) struct Framing {
    /// The passes in pass order, each with the events it produced.
    pub(super) passes: Vec<Ran>,
    pub(super) doc: String,
    pub(super) doc_exit: i32,
}

fn record(
    reason: &str,
    profile: Option<&str>,
    exit: Option<i32>,
    lines: Option<usize>,
    text: &[u8],
) -> Vec<u8> {
    let mut bytes = serde_json::json!({
        "reason": reason,
        "profile": profile,
        "exit": exit,
        "lines": lines,
        "text": String::from_utf8_lossy(text),
    })
    .to_string()
    .into_bytes();
    bytes.push(b'\n');
    bytes
}

/// A pass's events as newline-terminated lines, which is how the frame stores
/// them and how its line count is counted.
fn normalized(events: &[u8]) -> Vec<u8> {
    let mut out = events.to_vec();
    if !out.is_empty() && !out.ends_with(b"\n") {
        out.push(b'\n');
    }
    out
}

pub(super) fn frame(passes: &[Pass<'_>], doc_exit: Option<i32>, doc: &[u8]) -> Vec<u8> {
    let streams: Vec<Vec<u8>> = passes.iter().map(|p| normalized(p.events)).collect();
    let mut out = Vec::new();
    for (pass, stream) in passes.iter().zip(&streams) {
        let lines = stream.iter().filter(|b| **b == b'\n').count();
        out.extend(record(
            PASS,
            Some(pass.profile),
            pass.exit,
            Some(lines),
            pass.stderr,
        ));
    }
    out.extend(record(DOC, None, doc_exit, None, doc));
    for stream in &streams {
        out.extend(stream);
    }
    out
}

/// `(profile, exit, lines, text)`. `lines` is absent on the doc-test record,
/// which carries no event stream.
fn parse_record(line: &str, reason: &str) -> Result<(Option<String>, i32, Option<usize>, String)> {
    let value: Value = serde_json::from_str(line)
        .with_context(|| format!("incomplete inventory: {reason} record"))?;
    ensure!(
        value["reason"] == reason,
        "incomplete inventory: expected the {reason} record"
    );
    let exit = value["exit"]
        .as_i64()
        .and_then(|n| i32::try_from(n).ok())
        .with_context(|| format!("incomplete inventory: {reason} exit unknown"))?;
    let text = value["text"]
        .as_str()
        .with_context(|| format!("incomplete inventory: {reason} text"))?
        .to_owned();
    let profile = value["profile"].as_str().map(str::to_owned);
    let lines = match &value["lines"] {
        Value::Null => None,
        other => Some(
            other
                .as_u64()
                .and_then(|n| usize::try_from(n).ok())
                .filter(|n| *n <= inventory::MAX_LOG)
                .with_context(|| format!("incomplete inventory: {reason} event count"))?,
        ),
    };
    Ok((profile, exit, lines, text))
}

pub(super) fn unframe(log: &str) -> Result<Framing> {
    ensure!(log.len() <= inventory::MAX_LOG, "suite log exceeds budget");
    let mut rest = log;
    let mut counted = Vec::new();
    // The pass records come first, one per profile in `PROFILES`: a log with a
    // pass missing, added, renamed or reordered is not this gate's log.
    for profile in PROFILES {
        let (line, tail) = rest
            .split_once('\n')
            .with_context(|| format!("incomplete inventory: missing {PASS} record"))?;
        rest = tail;
        let (found, exit, lines, _) = parse_record(line, PASS)?;
        ensure!(
            found.as_deref() == Some(*profile),
            "incomplete inventory: expected the {profile} pass record"
        );
        let lines =
            lines.with_context(|| format!("incomplete inventory: {profile} pass event count"))?;
        counted.push((exit, lines));
    }
    let (line, tail) = rest
        .split_once('\n')
        .with_context(|| format!("incomplete inventory: missing {DOC} record"))?;
    let (_, doc_exit, _, doc) = parse_record(line, DOC)?;
    // Each pass takes the number of event lines its own record claims, in pass
    // order, and nothing may be left over: a stream that does not add up is not
    // this gate's log, and a pass's events can no longer drift to another pass.
    let mut rest = tail;
    let mut passes = Vec::new();
    for ((exit, lines), profile) in counted.into_iter().zip(PROFILES) {
        let mut events = String::new();
        for _ in 0..lines {
            let (line, tail) = rest.split_once('\n').with_context(|| {
                format!("incomplete inventory: {profile} pass is missing events it claimed")
            })?;
            events.push_str(line);
            events.push('\n');
            rest = tail;
        }
        passes.push(Ran { exit, events });
    }
    ensure!(
        rest.is_empty(),
        "incomplete inventory: events belong to no nextest pass"
    );
    Ok(Framing {
        passes,
        doc,
        doc_exit,
    })
}

// ── listing ────────────────────────────────────────────────────────────────

struct Suite {
    package: String,
    target: String,
    kind: String,
    /// test name → whether the listing reports it as ignored.
    cases: BTreeMap<String, bool>,
}

fn field(s: &str) -> Result<&str> {
    ensure!(
        !s.is_empty() && !s.chars().any(char::is_control),
        "inconsistent inventory: invalid field"
    );
    Ok(s)
}

/// The listing lives as one extra line in the build log. The cargo executable
/// map in the same file carries no `rust-suites` key and is skipped here,
/// exactly as `inventory::executables` skips this line.
fn listing(map: &str) -> Result<&str> {
    ensure!(
        map.len() <= inventory::MAX_LOG,
        "executable map exceeds budget"
    );
    let mut found = None;
    for line in map.lines().filter(|l| !l.is_empty()) {
        if !line.contains("\"rust-suites\"") {
            continue;
        }
        ensure!(
            found.replace(line).is_none(),
            "inconsistent inventory: duplicate nextest listing"
        );
    }
    found.context("incomplete inventory: no nextest listing")
}

/// `package::binary-name` → suite. That is the prefix nextest puts before `$`
/// in every event name; the listing's own `binary-id` spells a library or
/// binary target differently, so it cannot be used for the lookup.
fn suites(map: &str) -> Result<BTreeMap<String, Suite>> {
    let value: Value = serde_json::from_str(listing(map)?)
        .context("incomplete inventory: nextest listing JSON")?;
    let listed = value["rust-suites"]
        .as_object()
        .context("incomplete inventory: rust-suites")?;
    ensure!(
        !listed.is_empty() && listed.len() <= 4096,
        "nextest listing exceeds count budget"
    );
    let mut out: BTreeMap<String, Suite> = BTreeMap::new();
    let mut total = 0usize;
    for (id, suite) in listed {
        let id = field(id)?;
        ensure!(
            suite["status"] == "listed",
            "incomplete inventory: nextest suite {id} was not listed"
        );
        let kind = field(suite["kind"].as_str().context("nextest suite kind")?)?;
        ensure!(
            ["lib", "bin", "test", "example", "bench", "proc-macro"].contains(&kind),
            "incomplete inventory: unknown nextest suite kind"
        );
        let package = field(
            suite["package-name"]
                .as_str()
                .context("nextest package name")?,
        )?;
        let target = field(
            suite["binary-name"]
                .as_str()
                .context("nextest binary name")?,
        )?;
        let mut cases = BTreeMap::new();
        for (name, case) in suite["testcases"]
            .as_object()
            .context("nextest suite testcases")?
        {
            let ignored = case["ignored"].as_bool().context("nextest case ignored")?;
            // A filter must never take a test out of the inventory silently.
            // `ignored` is the one accepted mismatch; a `default-filter` or a
            // `-E` expression lands here and fails the run instead.
            ensure!(
                if ignored {
                    case["filter-match"]["status"] == "mismatch"
                        && case["filter-match"]["reason"] == "ignored"
                } else {
                    case["filter-match"]["status"] == "matches"
                },
                "incomplete inventory: nextest filtered out {id} {name}"
            );
            ensure!(
                cases.insert(field(name)?.to_owned(), ignored).is_none(),
                "inconsistent inventory: duplicate nextest case"
            );
        }
        total = total
            .checked_add(cases.len())
            .context("inventory count budget")?;
        ensure!(total <= inventory::MAX_TESTS, "inventory count budget");
        ensure!(
            out.insert(
                format!("{package}::{target}"),
                Suite {
                    package: package.to_owned(),
                    target: target.to_owned(),
                    kind: kind.to_owned(),
                    cases,
                },
            )
            .is_none(),
            "inconsistent inventory: ambiguous nextest suite {id}"
        );
    }
    Ok(out)
}

// ── events ─────────────────────────────────────────────────────────────────

fn suite_of<'a>(suites: &'a BTreeMap<String, Suite>, value: &Value) -> Result<(&'a Suite, String)> {
    let nextest = &value["nextest"];
    let package = nextest["crate"].as_str().context("nextest suite crate")?;
    let target = nextest["test_binary"]
        .as_str()
        .context("nextest suite binary")?;
    let key = format!("{package}::{target}");
    let suite = suites
        .get(&key)
        .context("inconsistent inventory: unlisted nextest suite")?;
    ensure!(
        nextest["kind"] == suite.kind.as_str(),
        "inconsistent inventory: nextest suite kind changed"
    );
    Ok((suite, key))
}

/// One `suite` summary's counts, validated and kept for the suite's accounting.
struct Totals {
    passed: u64,
    failed: u64,
    ignored: u64,
}

fn summary(value: &Value) -> Result<Totals> {
    let budget = u64::try_from(inventory::MAX_TESTS).context("inventory count budget")?;
    let mut counts = [0u64; 5];
    for (slot, key) in
        counts
            .iter_mut()
            .zip(["passed", "failed", "ignored", "measured", "filtered_out"])
    {
        *slot = value[key]
            .as_u64()
            .filter(|n| *n <= budget)
            .with_context(|| format!("inconsistent inventory: nextest suite summary {key}"))?;
    }
    ensure!(
        counts[3] == 0,
        "inconsistent inventory: nextest reported a benchmark"
    );
    ensure!(
        value["exec_time"]
            .as_f64()
            .is_some_and(|s| s.is_finite() && s >= 0.0),
        "inconsistent inventory: nextest suite exec_time"
    );
    Ok(Totals {
        passed: counts[0],
        failed: counts[1],
        ignored: counts[2],
    })
}

/// What one pass's own stream said about that pass, for the exit-status check:
/// a pass that reported no suite ran nothing, and a pass that reported a
/// failure must say so in its status.
#[derive(Clone, Default)]
struct Reported {
    suite: bool,
    failure: bool,
}

/// What a suite's events said about it.
#[derive(Default)]
struct Observed {
    announcements: usize,
    summaries: usize,
    passed: u64,
    failed: u64,
}

/// Cargo's compiler artifacts and nextest's listing must name the same test
/// binaries. The runner does not get to supply both sides of the expected set:
/// a binary Cargo built and nextest never listed, or the reverse, refuses the
/// run instead of shrinking the inventory.
fn agree_with_cargo(suites: &BTreeMap<String, Suite>, map: &str) -> Result<()> {
    let listed: BTreeSet<(String, String, String)> = suites
        .values()
        .map(|s| (s.kind.clone(), s.package.clone(), s.target.clone()))
        .collect();
    let built = inventory::targets(map)?;
    let missing: Vec<_> = built.difference(&listed).take(8).collect();
    let extra: Vec<_> = listed.difference(&built).take(8).collect();
    ensure!(
        missing.is_empty() && extra.is_empty(),
        "incomplete inventory: cargo and nextest disagree on the test binaries; \
         cargo built and nextest did not list {missing:?}; nextest listed and cargo did not build {extra:?}"
    );
    Ok(())
}

/// Terminal rows for every test nextest listed, judged against the listing, the
/// suite summaries and every pass's exit status.
///
/// `passes` is every pass in pass order, each with its own exit status and its
/// own stream. Completeness is judged over their **union**, which is what makes
/// the split safe: a listed test that no pass ran has no result below, and one
/// that two passes ran is a duplicate test event, so a filter cannot quietly
/// drop or double a test. Per-suite accounting stays per-pass accounting,
/// because the split is by binary — a suite that somehow ran twice over-counts
/// its summaries and refuses. Each pass's exit status, by contrast, is judged
/// against that pass's own stream alone.
///
/// A test the listing reports as ignored is **not** given a terminal event to
/// prove it: nextest 0.9.152 emits its skip event only sometimes (observed
/// present for all five of orch-infra's ignored tests and absent for a
/// two-test fixture binary), so requiring it would be an intermittent refusal.
/// Its status comes from the listing, which is the binary's own answer to
/// `--list`, and three run-derived checks close the gap: nextest must not have
/// produced a result for it, every summary's `ignored` count must equal the
/// number the listing reports, and the summaries' `passed` plus `failed` must
/// equal the number of tests that were not ignored. Nothing is inferred from an
/// absent event for a test that was supposed to run.
pub(super) fn rows(passes: &[Ran], map: &str) -> Result<Vec<Row>> {
    ensure!(
        passes.len() == PROFILES.len(),
        "incomplete inventory: {} of {} nextest passes",
        passes.len(),
        PROFILES.len()
    );
    let suites = suites(map)?;
    agree_with_cargo(&suites, map)?;
    let mut observed: BTreeMap<&str, Observed> = suites
        .keys()
        .map(|key| (key.as_str(), Observed::default()))
        .collect();
    let mut seen: BTreeMap<(String, String), &'static str> = BTreeMap::new();
    let mut reported = vec![Reported::default(); passes.len()];
    for (pass, state) in passes.iter().zip(&mut reported) {
        for raw in pass.events.lines() {
            if raw.is_empty() {
                continue;
            }
            // Captured child output is indented; an event line never is, so a test
            // cannot print its own result into the stream.
            ensure!(
                raw.trim() == raw,
                "inconsistent inventory: indented nextest event"
            );
            let value: Value =
                serde_json::from_str(raw).context("inconsistent inventory: nextest event JSON")?;
            ensure!(
                value.is_object() && value.get("reason").is_none(),
                "inconsistent inventory: unexpected nextest record"
            );
            let event = value["event"].as_str().context("nextest event")?;
            match value["type"].as_str().context("nextest event type")? {
                "suite" => {
                    state.suite = true;
                    state.failure |= event == "failed";
                    let (suite, key) = suite_of(&suites, &value)?;
                    let ignored = u64::try_from(suite.cases.values().filter(|i| **i).count())
                        .context("inventory count budget")?;
                    let announced =
                        u64::try_from(suite.cases.len()).context("inventory count budget")?;
                    let entry = observed
                        .get_mut(key.as_str())
                        .context("inventory suite entry")?;
                    match event {
                        "started" => {
                            ensure!(
                                value["test_count"].as_u64() == Some(announced),
                                "inconsistent inventory: nextest suite {key} announced a different count"
                            );
                            entry.announcements += 1;
                        }
                        "ok" | "failed" => {
                            let totals = summary(&value)?;
                            ensure!(
                                totals.ignored == ignored,
                                "inconsistent inventory: nextest suite {key} skipped a different number of tests"
                            );
                            ensure!(
                                (event == "ok") == (totals.failed == 0),
                                "inconsistent inventory: nextest suite {key} status"
                            );
                            entry.summaries += 1;
                            entry.passed = entry
                                .passed
                                .checked_add(totals.passed)
                                .context("inventory count budget")?;
                            entry.failed = entry
                                .failed
                                .checked_add(totals.failed)
                                .context("inventory count budget")?;
                        }
                        _ => bail!("inconsistent inventory: nextest suite event"),
                    }
                    ensure!(
                        entry.announcements <= inventory::MAX_TESTS
                            && entry.summaries <= inventory::MAX_TESTS,
                        "inventory count budget"
                    );
                }
                "test" => {
                    let name = value["name"].as_str().context("nextest test name")?;
                    let (key, test) = name
                        .split_once('$')
                        .context("inconsistent inventory: nextest test name")?;
                    let ignored = *suites
                        .get(key)
                        .context("inconsistent inventory: unlisted nextest suite")?
                        .cases
                        .get(test)
                        .context("inconsistent inventory: unlisted nextest test")?;
                    let status = match event {
                        // A start is not evidence of anything; nextest also omits
                        // it for a test reported in a later group.
                        "started" => continue,
                        "ok" => "ok",
                        "failed" => {
                            state.failure = true;
                            "failed"
                        }
                        "ignored" => "ignored",
                        _ => bail!("inconsistent inventory: nextest test event"),
                    };
                    ensure!(
                        (status == "ignored") == ignored,
                        "inconsistent inventory: nextest ran {key} {test} against its listed state"
                    );
                    ensure!(
                        seen.insert((key.to_owned(), test.to_owned()), status)
                            .is_none(),
                        "inconsistent inventory: duplicate nextest test"
                    );
                }
                _ => bail!("inconsistent inventory: nextest event type"),
            }
        }
    }
    let mut rows = Vec::new();
    for (key, suite) in &suites {
        let entry = observed
            .get(key.as_str())
            .context("inventory suite entry")?;
        // A suite with no test runs nothing, and its `status: "listed"` is the
        // completed-executable proof: nextest queried the binary for its tests.
        ensure!(
            suite.cases.is_empty() || (entry.announcements > 0 && entry.summaries > 0),
            "incomplete inventory: nextest suite {key} without a completed result"
        );
        let ran = u64::try_from(suite.cases.values().filter(|i| !**i).count())
            .context("inventory count budget")?;
        ensure!(
            entry.passed.checked_add(entry.failed) == Some(ran),
            "inconsistent inventory: nextest suite {key} ran a different number of tests"
        );
        for (test, ignored) in &suite.cases {
            let status = if *ignored {
                ensure!(
                    seen.get(&(key.clone(), test.clone()))
                        .is_none_or(|s| *s == "ignored"),
                    "inconsistent inventory: nextest {key} {test} is listed as ignored"
                );
                "ignored"
            } else {
                seen.get(&(key.clone(), test.clone()))
                    .copied()
                    .with_context(|| {
                        format!("incomplete inventory: nextest {key} {test} has no result")
                    })?
            };
            rows.push(Row {
                kind: suite.kind.clone(),
                package: suite.package.clone(),
                target: suite.target.clone(),
                test: test.clone(),
                status: status.to_owned(),
            });
        }
    }
    // Every result belonged to a listed test; the loop above refuses any other.
    ensure!(
        seen.len() <= rows.len(),
        "inconsistent inventory: nextest result for an unlisted test"
    );
    // Say the real reason when nothing ran: `--no-tests fail` makes nextest
    // exit 4, and the exit status alone would read as a runner problem. A pass
    // whose filter matched nothing exits 4 too, so this also catches a split
    // that left a pass empty.
    ensure!(
        !passes.iter().all(|p| p.exit == 4),
        "no test executed: nextest found no test to run"
    );
    // Each pass's status is judged against **its own** stream, which is what
    // the frame's per-pass event counts are for. 0 is success and 100 is "some
    // test failed"; no other status describes a complete run (101 is an
    // internal error). A single 4 is accepted because a tier that selects a
    // subset of packages can leave one pass's binaries out of scope entirely —
    // and an empty pass still cannot hide a test, because the completeness loop
    // above judged the passes against the *unfiltered* listing and would have
    // refused every test the empty pass owed. Judging the statuses together
    // would let one pass's failure be answered by the other pass's exit, or a
    // pass that ran tests be passed off as the empty one.
    for ((profile, pass), reported) in PROFILES.iter().zip(passes).zip(&reported) {
        ensure!(
            matches!(pass.exit, 0 | 100 | 4),
            "inconsistent inventory: nextest {profile} pass exit {}",
            pass.exit
        );
        ensure!(
            (pass.exit == 100) == reported.failure,
            "inconsistent inventory: nextest {profile} pass exit {} against its own stream",
            pass.exit
        );
        ensure!(
            (pass.exit == 4) == !reported.suite,
            "inconsistent inventory: nextest {profile} pass exit {} reported {} suite",
            pass.exit,
            if reported.suite { "a" } else { "no" }
        );
    }
    rows.sort();
    Ok(rows)
}

/// The whole judged suite: nextest's own tests plus the separate doc-test run,
/// rebuilt from one framed test log and one build log.
pub(super) fn inventory(log: &str, map: &str) -> Result<Vec<Row>> {
    let framed = unframe(log)?;
    let mut rows = rows(&framed.passes, map)?;
    rows.extend(inventory::parse(
        &framed.doc,
        map,
        framed.doc_exit,
        inventory::Scope::Doc,
    )?);
    rows.sort();
    ensure!(
        rows.windows(2)
            .all(|w| (&w[0].kind, &w[0].package, &w[0].target, &w[0].test)
                != (&w[1].kind, &w[1].package, &w[1].target, &w[1].test)),
        "inconsistent inventory: duplicate test"
    );
    ensure!(
        rows.iter().any(|r| r.status != "ignored"),
        "no test executed"
    );
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(value: Value) -> String {
        format!("{value}\n")
    }

    fn ran(exit: i32, events: &str) -> Ran {
        Ran {
            exit,
            events: events.to_owned(),
        }
    }

    /// The usual fixture shape: the whole stream in the first pass, and a
    /// second pass that matched nothing and so exits 4 with no events.
    fn one(exit: i32, events: &str) -> Vec<Ran> {
        vec![ran(exit, events), ran(4, "")]
    }

    // One library target, which is what a cargo artifact map and a doc-test
    // section both have to agree with.
    const NEXTEST: fn() -> Value =
        || serde_json::json!({"crate":"fixture","test_binary":"fixture","kind":"lib"});

    fn started(count: u64) -> String {
        line(
            serde_json::json!({"type":"suite","event":"started","test_count":count,"nextest":NEXTEST()}),
        )
    }

    fn summary_event(event: &str, passed: u64, failed: u64, ignored: u64) -> String {
        line(
            serde_json::json!({"type":"suite","event":event,"passed":passed,"failed":failed,
            "ignored":ignored,"measured":0,"filtered_out":0,"exec_time":0.1,"nextest":NEXTEST()}),
        )
    }

    fn test_event(event: &str, name: &str) -> String {
        line(
            serde_json::json!({"type":"test","event":event,"name":format!("fixture::fixture${name}"),"exec_time":0.1}),
        )
    }

    fn listed(cases: &[(&str, bool)]) -> String {
        let mut testcases = serde_json::Map::new();
        for (name, ignored) in cases {
            testcases.insert(
                (*name).to_owned(),
                if *ignored {
                    serde_json::json!({"ignored":true,"filter-match":{"status":"mismatch","reason":"ignored"}})
                } else {
                    serde_json::json!({"ignored":false,"filter-match":{"status":"matches"}})
                },
            );
        }
        serde_json::json!({"rust-suites":{"fixture":{
            "package-name":"fixture","binary-name":"fixture","kind":"lib","status":"listed",
            "testcases":Value::Object(testcases)}}})
        .to_string()
    }

    /// Cargo's own compiler artifact for the same target: the independent
    /// expected set the listing has to agree with, and what resolves the
    /// doc-test section.
    fn artifact() -> String {
        serde_json::json!({"reason":"compiler-artifact","package_id":"path+file:///fixture#fixture@0.1.0","profile":{"test":true},"executable":"/fixture/target/test","target":{"kind":["lib"],"name":"fixture"}}).to_string()
    }

    fn with_artifact(listing: &str) -> String {
        format!("{listing}\n{}", artifact())
    }

    /// One test that runs and one the binary reports as ignored.
    fn map() -> String {
        with_artifact(&listed(&[("one", false), ("two", true)]))
    }

    fn events() -> String {
        format!(
            "{}{}{}{}{}",
            started(2),
            test_event("started", "one"),
            test_event("ok", "one"),
            test_event("ignored", "two"),
            summary_event("ok", 1, 0, 1)
        )
    }

    fn doc_log() -> &'static str {
        "Doc-tests fixture\nrunning 1 test\ntest src/lib.rs - (line 1) ... ok\ntest result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s\n"
    }

    #[test]
    fn version_pin_accepts_only_the_pinned_runner() {
        assert_eq!(
            pinned(&format!(
                "cargo-nextest {VERSION} (abc 2026-11-13)\nrelease: {VERSION}\n"
            ))
            .unwrap(),
            format!("cargo-nextest {VERSION} (abc 2026-11-13)")
        );
        for bad in [
            String::new(),
            "\n".to_owned(),
            "cargo-nextest 0.9.151 (abc)".to_owned(),
            "cargo-nextest 0.9.153".to_owned(),
            "nextest 0.9.152".to_owned(),
            format!("cargo-nextest{VERSION}"),
            format!("  cargo-nextest {VERSION}x"),
        ] {
            let e = pinned(&bad).unwrap_err().to_string();
            assert!(
                e.contains("cargo install cargo-nextest") || e.contains("printed no version"),
                "{bad}: {e}"
            );
        }
    }

    #[test]
    fn complete_stream_names_every_listed_test_with_its_listed_status() {
        let rows = rows(&one(0, &events()), &map()).unwrap();
        assert_eq!(
            inventory::tsv(&rows),
            "lib\tfixture\tfixture\tone\tok\nlib\tfixture\tfixture\ttwo\tignored\n"
        );
    }

    /// nextest 0.9.152 does not always emit the skip event, so its absence is
    /// not a refusal; the summaries still have to account for the skip.
    #[test]
    fn a_missing_skip_event_is_accepted_but_the_summary_must_still_account_for_it() {
        let without = format!(
            "{}{}{}",
            started(2),
            test_event("ok", "one"),
            summary_event("ok", 1, 0, 1)
        );
        assert_eq!(
            rows(&one(0, &without), &map()).unwrap(),
            rows(&one(0, &events()), &map()).unwrap()
        );
        for bad in [
            // The summary forgot the skip, or invented one.
            format!(
                "{}{}{}",
                started(2),
                test_event("ok", "one"),
                summary_event("ok", 1, 0, 0)
            ),
            format!(
                "{}{}{}",
                started(2),
                test_event("ok", "one"),
                summary_event("ok", 1, 0, 2)
            ),
            // The summary claims a test that never reported.
            format!(
                "{}{}{}",
                started(2),
                test_event("ok", "one"),
                summary_event("ok", 2, 0, 1)
            ),
            // A benchmark is not supported.
            format!(
                "{}{}{}",
                started(2),
                test_event("ok", "one"),
                summary_event("ok", 1, 0, 1).replace("\"measured\":0", "\"measured\":1")
            ),
            // An `ok` summary with failures, and the reverse.
            format!(
                "{}{}{}",
                started(2),
                test_event("ok", "one"),
                summary_event("ok", 0, 1, 1)
            ),
            format!(
                "{}{}{}",
                started(2),
                test_event("failed", "one"),
                summary_event("failed", 1, 0, 1)
            ),
        ] {
            assert!(rows(&one(0, &bad), &map()).is_err(), "{bad}");
        }
    }

    #[test]
    fn truncated_malformed_and_smuggled_streams_refuse() {
        let good = events();
        for bad in [
            // Truncated before the suite summary, and before any terminal row.
            format!("{}{}", started(2), test_event("ok", "one")),
            started(2),
            String::new(),
            // A listed test that was supposed to run and has no result.
            format!("{}{}", started(2), summary_event("ok", 1, 0, 1)),
            // Malformed JSON and a half-written final line.
            format!("{good}not json\n"),
            format!("{good}{{\"type\":\"test\",\"event\":\"ok\",\"na"),
            // A test or suite the listing never named.
            good.replace("fixture$one", "fixture$three"),
            good.replace("\"test_binary\":\"fixture\"", "\"test_binary\":\"other\""),
            good.replace("\"kind\":\"lib\"", "\"kind\":\"bin\""),
            // Captured child output pretending to be an event.
            format!("{good}    {}", test_event("ok", "one")),
            // A framing record cannot appear in the event stream.
            format!(
                "{good}{}",
                line(serde_json::json!({"reason":PASS,"profile":"default","exit":0,"text":""}))
            ),
            // An announced count that disagrees with the listing.
            good.replace("\"test_count\":2", "\"test_count\":3"),
            // An unknown event or type is chatter, never a pass.
            good.replace("\"event\":\"ok\"", "\"event\":\"flaky\""),
            good.replace("\"type\":\"test\"", "\"type\":\"case\""),
            // A duplicate terminal row.
            format!("{good}{}", test_event("ok", "one")),
            // nextest must not run what the listing calls ignored, and must not
            // skip what it does not.
            format!(
                "{}{}{}{}",
                started(2),
                test_event("ok", "one"),
                test_event("ok", "two"),
                summary_event("ok", 2, 0, 1)
            ),
            format!(
                "{}{}{}{}",
                started(2),
                test_event("ignored", "one"),
                test_event("ignored", "two"),
                summary_event("ok", 0, 0, 1)
            ),
        ] {
            assert!(rows(&one(0, &bad), &map()).is_err(), "{bad}");
        }
        // Only 0, 100 and 4 describe a pass, and each pass's status is judged
        // against its own stream: a status the other pass could have explained
        // is still wrong on this one.
        for (index, passes) in [
            one(1, &good),
            one(101, &good),
            one(-1, &good),
            // Ran a suite and claims it found no test to run.
            one(4, &good),
            // Reported no failure and claims one.
            one(100, &good),
            // The second pass has no stream, so 4 is the only status that
            // describes it — 0 and 100 would be the first pass's answer.
            vec![ran(0, &good), ran(0, "")],
            vec![ran(0, &good), ran(100, "")],
            vec![ran(0, &good), ran(101, "")],
            // A pass too few or too many is not this gate's run.
            vec![ran(0, &good)],
            vec![ran(0, &good), ran(4, ""), ran(4, "")],
        ]
        .into_iter()
        .enumerate()
        {
            assert!(rows(&passes, &map()).is_err(), "case {index}");
        }
        let failed = format!(
            "{}{}{}{}",
            started(2),
            test_event("failed", "one"),
            test_event("ignored", "two"),
            summary_event("failed", 0, 1, 1)
        );
        assert_eq!(
            rows(&one(100, &failed), &map()).unwrap()[0].status,
            "failed"
        );
        assert!(rows(&one(0, &failed), &map()).is_err());
    }

    #[test]
    fn a_listing_that_hides_or_cannot_query_a_test_refuses() {
        for bad in [
            map().replace("\"status\":\"listed\"", "\"status\":\"skipped\""),
            map().replace(
                "\"status\":\"matches\"",
                "\"status\":\"mismatch\",\"reason\":\"default-filter\"",
            ),
            map().replace("\"ignored\":true", "\"ignored\":false"),
            map().replace("rust-suites", "suites"),
            format!("{}\n{}", map(), map()),
            String::new(),
            // A test the stream never reported.
            with_artifact(&listed(&[("one", false), ("two", true), ("three", false)])),
            // A binary cargo built that nextest never listed, and the reverse.
            format!(
                "{}\n{}",
                map(),
                artifact()
                    .replace("\"fixture\"]", "\"other\"]")
                    .replace("\"name\":\"fixture\"", "\"name\":\"other\"")
            ),
            listed(&[("one", false), ("two", true)]),
        ] {
            assert!(rows(&one(0, &events()), &bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_library_and_a_binary_of_one_name_are_ambiguous_and_refuse() {
        // nextest's event names carry only `<package>::<binary-name>`, so a
        // binary target named after its package's library cannot be told apart
        // from it. That must refuse, never guess.
        let two = serde_json::json!({"rust-suites":{
            "fixture":{"package-name":"fixture","binary-name":"fixture","kind":"lib","status":"listed",
                "testcases":{"one":{"ignored":false,"filter-match":{"status":"matches"}}}},
            "fixture::bin/fixture":{"package-name":"fixture","binary-name":"fixture","kind":"bin","status":"listed",
                "testcases":{"two":{"ignored":false,"filter-match":{"status":"matches"}}}}}})
        .to_string();
        assert!(
            rows(&one(0, &events()), &two)
                .unwrap_err()
                .to_string()
                .contains("ambiguous")
        );
    }

    /// The real frame, with the whole stream in the first pass and a second
    /// pass that matched nothing — the shape a one-binary fixture produces.
    fn framed(
        exit: Option<i32>,
        events: &[u8],
        stderr: &[u8],
        doc_exit: Option<i32>,
        doc: &[u8],
    ) -> Vec<u8> {
        frame(
            &[
                Pass {
                    profile: "default",
                    exit,
                    events,
                    stderr,
                },
                Pass {
                    profile: "alone",
                    exit: Some(4),
                    events: b"",
                    stderr: b"",
                },
            ],
            doc_exit,
            doc,
        )
    }

    #[test]
    fn framing_round_trips_and_a_missing_or_unknown_record_refuses() {
        let text = String::from_utf8(framed(
            Some(0),
            events().as_bytes(),
            b"runner chatter\nwith lines\n",
            Some(0),
            doc_log().as_bytes(),
        ))
        .unwrap();
        let read = unframe(&text).unwrap();
        // Each pass gets back exactly the events it produced, by the line count
        // its own record carries.
        assert_eq!(
            read.passes
                .iter()
                .map(|p| (p.exit, p.events.clone()))
                .collect::<Vec<_>>(),
            vec![(0, events()), (4, String::new())]
        );
        assert_eq!(read.doc, doc_log());
        assert_eq!(read.doc_exit, 0);
        let rows = inventory(&text, &map()).unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows.iter().filter(|r| r.kind == "doc").count(), 1);
        for bad in [
            text.lines().skip(1).collect::<Vec<_>>().join("\n"),
            text.replace(DOC, "gate-doc-test"),
            text.replace("\"exit\":0", "\"exit\":null"),
            String::new(),
            // An event count that does not describe the stream: too few leaves
            // events belonging to no pass, too many starves the next pass.
            text.replace("\"lines\":5", "\"lines\":4"),
            text.replace("\"lines\":5", "\"lines\":6"),
            text.replace("\"lines\":5", "\"lines\":null"),
        ] {
            assert!(unframe(&bad).is_err(), "{bad}");
        }
        // Counts that merely swap the two passes' streams still describe the
        // log, so the frame accepts them; the inventory is what refuses, because
        // the pass that claims to have found no test now owns every event.
        let moved = text
            .replace("\"lines\":5", "\"lines\":0")
            .replace("\"lines\":0,\"text\":\"\"", "\"lines\":5,\"text\":\"\"");
        assert_ne!(moved, text);
        unframe(&moved).unwrap();
        let err = inventory(&moved, &map()).unwrap_err().to_string();
        assert!(
            err.contains("against its own stream") || err.contains("suite"),
            "{err}"
        );
        // A failed doc-test run cannot pass while the nextest stream is clean.
        let doc_failed = String::from_utf8(framed(
            Some(0),
            events().as_bytes(),
            b"",
            Some(101),
            doc_log().as_bytes(),
        ))
        .unwrap();
        assert!(inventory(&doc_failed, &map()).is_err());
    }

    /// The pass split is only safe if the union of what the passes ran is
    /// judged against the unfiltered listing. These are the two ways a split
    /// can be wrong, and neither may pass.
    #[test]
    fn a_test_no_pass_ran_or_two_passes_ran_refuses() {
        // A two-test listing where the split lost one of them: the expected set
        // is unfiltered, so the lost test simply has no result.
        let map = with_artifact(&listed(&[("one", false), ("two", false)]));
        // The first pass accounted for both tests but reported only one, and
        // the second pass ran nothing.
        let short = format!(
            "{}{}{}",
            started(2),
            test_event("ok", "one"),
            summary_event("ok", 2, 0, 0)
        );
        let err = rows(&one(0, &short), &map).unwrap_err().to_string();
        assert!(err.contains("two has no result"), "{err}");
        // A pass that dropped the test from its summary as well.
        let err = rows(
            &one(
                0,
                &format!(
                    "{}{}{}",
                    started(2),
                    test_event("ok", "one"),
                    summary_event("ok", 1, 0, 0)
                ),
            ),
            &map,
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("ran a different number"), "{err}");
        // Both passes ran the same tests, which is the other way a split can be
        // wrong. The duplicate test event refuses…
        let whole = format!(
            "{}{}{}{}",
            started(2),
            test_event("ok", "one"),
            test_event("ok", "two"),
            summary_event("ok", 2, 0, 0)
        );
        let err = rows(&[ran(0, &whole), ran(0, &whole)], &map)
            .unwrap_err()
            .to_string();
        assert!(err.contains("duplicate nextest test"), "{err}");
        // …and the doubled suite accounting refuses even with no duplicate
        // event, so a second pass cannot re-run a suite silently.
        let counted_again = format!("{}{}", started(2), summary_event("ok", 2, 0, 0));
        let err = rows(&[ran(0, &whole), ran(0, &counted_again)], &map)
            .unwrap_err()
            .to_string();
        assert!(err.contains("ran a different number"), "{err}");
    }

    /// A pass whose filter matched nothing is only acceptable because the
    /// completeness check above is judged against the unfiltered listing.
    #[test]
    fn an_empty_pass_is_accepted_but_cannot_excuse_a_missing_test() {
        let good = format!(
            "{}{}{}{}",
            started(2),
            test_event("ok", "one"),
            test_event("ignored", "two"),
            summary_event("ok", 1, 0, 1)
        );
        assert_eq!(rows(&one(0, &good), &map()).unwrap().len(), 2);
        // Nothing ran anywhere: say so, rather than reporting an exit status.
        let err = rows(&one(4, ""), &with_artifact(&listed(&[])))
            .unwrap_err()
            .to_string();
        assert!(err.contains("no test executed"), "{err}");
        // The second pass owed a test and ran none.
        let err = rows(
            &one(
                0,
                &format!(
                    "{}{}{}",
                    started(2),
                    test_event("ok", "one"),
                    summary_event("ok", 2, 0, 0)
                ),
            ),
            &with_artifact(&listed(&[("one", false), ("two", false)])),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("two has no result"), "{err}");
    }

    /// The frame names its passes, so a log with a pass missing, renamed or out
    /// of order is not this gate's log.
    #[test]
    fn the_frame_pins_every_pass_by_profile() {
        let text = String::from_utf8(framed(
            Some(0),
            events().as_bytes(),
            b"",
            Some(0),
            doc_log().as_bytes(),
        ))
        .unwrap();
        let lines: Vec<&str> = text.lines().collect();
        for bad in [
            // the alone pass dropped
            [&lines[..1], &lines[2..]].concat().join("\n"),
            // the passes swapped
            [&lines[1..2], &lines[..1], &lines[2..]].concat().join("\n"),
            // a pass renamed
            text.replace("\"alone\"", "\"solo\""),
            // a third pass smuggled in ahead of the doc record
            text.replace(&lines[2].to_owned(), &format!("{}\n{}", lines[1], lines[2])),
        ] {
            let err = match unframe(&bad) {
                Ok(_) => panic!("accepted a reframed log: {bad}"),
                Err(e) => e.to_string(),
            };
            assert!(err.contains("incomplete inventory"), "{bad}: {err}");
        }
    }

    #[test]
    fn an_all_ignored_suite_is_not_a_pass() {
        let only_ignored = listed(&[("two", true)]);
        let stream = format!(
            "{}{}{}",
            started(1),
            test_event("ignored", "two"),
            summary_event("ok", 0, 0, 1)
        );
        let empty_doc = "Doc-tests fixture\nrunning 0 tests\ntest result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0s\n";
        let log = String::from_utf8(framed(
            Some(0),
            stream.as_bytes(),
            b"",
            Some(0),
            empty_doc.as_bytes(),
        ))
        .unwrap();
        assert!(
            inventory(&log, &with_artifact(&only_ignored))
                .unwrap_err()
                .to_string()
                .contains("no test executed")
        );
    }
}
