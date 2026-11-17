//! Strict sequential libtest inventory. Child chatter can cause a false failure,
//! never an inferred pass. A completed executable is required even for zero tests.

use anyhow::{Context, Result, bail, ensure};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, VecDeque};

pub(super) const MAX_LOG: usize = 32 * 1024 * 1024;
const MAX_TESTS: usize = 50_000;

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub(super) struct Row {
    pub kind: String,
    pub package: String,
    pub target: String,
    pub test: String,
    pub status: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Executable {
    package: String,
    target: String,
    kind: String,
}

fn field(s: &str) -> Result<&str> {
    ensure!(
        !s.is_empty() && !s.chars().any(char::is_control),
        "inconsistent inventory: invalid field"
    );
    Ok(s)
}

fn terminal(text: &str) -> Option<&'static str> {
    match text {
        "ok" => Some("ok"),
        "FAILED" => Some("failed"),
        s if s == "ignored" || s.starts_with("ignored,") => Some("ignored"),
        _ => None,
    }
}

fn version_output(text: &str) -> bool {
    let Some(version) = text.strip_prefix("orchestry ") else {
        return false;
    };
    let parts: Vec<_> = version.split('.').collect();
    parts.len() == 3
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
}

fn framed_terminal(text: &str) -> Option<&'static str> {
    // This is the complete existing daemon diagnostic, not an arbitrary suffix
    // after a status prefix. Unknown chatter remains an inventory error.
    let status = text.strip_suffix(
        "  ⚠ orchestrator:error (details omitted from daemon stderr; use orch logs --follow)",
    )?;
    match status {
        "ok" => Some("ok"),
        "FAILED" => Some("failed"),
        _ => None,
    }
}

fn package(id: &str) -> Result<String> {
    let (location, fragment) = id
        .rsplit_once('#')
        .context("incomplete inventory: package id")?;
    Ok(field(if let Some((name, _)) = fragment.split_once('@') {
        name
    } else if fragment.starts_with(|c: char| c.is_ascii_digit()) {
        location.rsplit('/').next().context("package name")?
    } else {
        fragment
    })?
    .to_owned())
}

fn executables(map: &str) -> Result<BTreeMap<String, Executable>> {
    ensure!(map.len() <= MAX_LOG, "executable map exceeds budget");
    let mut out = BTreeMap::new();
    for line in map.lines().filter(|l| !l.is_empty()) {
        let v: Value =
            serde_json::from_str(line).context("incomplete inventory: executable JSON")?;
        if v["reason"] != "compiler-artifact" || v["profile"]["test"] != true {
            continue;
        }
        let Some(path) = v["executable"].as_str() else {
            continue;
        };
        let kinds = v["target"]["kind"].as_array().context("artifact kind")?;
        let kind = kinds
            .first()
            .and_then(Value::as_str)
            .context("artifact kind")?;
        ensure!(
            ["lib", "bin", "test", "example", "bench", "proc-macro"].contains(&kind),
            "incomplete inventory: unknown artifact kind"
        );
        let entry = Executable {
            package: package(v["package_id"].as_str().context("package id")?)?,
            target: field(v["target"]["name"].as_str().context("target name")?)?.into(),
            kind: kind.into(),
        };
        ensure!(
            out.insert(field(path)?.to_owned(), entry).is_none(),
            "inconsistent inventory: duplicate executable"
        );
        ensure!(out.len() <= 4096, "executable map exceeds count budget");
    }
    ensure!(!out.is_empty(), "incomplete inventory: no test executables");
    Ok(out)
}

#[derive(Default)]
enum FailureOutput {
    #[default]
    None,
    Captured,
    Names(BTreeSet<String>),
}

struct Section {
    key: String,
    exe: Executable,
    announced: Option<usize>,
    rows: Vec<Row>,
    failures: FailureOutput,
    notices: BTreeSet<String>,
    pending_helpers: VecDeque<usize>,
    helpers: Vec<(usize, usize)>,
    pending_result: Option<String>,
}

impl Section {
    fn new(key: String, exe: Executable) -> Self {
        Self {
            key,
            exe,
            announced: None,
            rows: vec![],
            failures: FailureOutput::None,
            notices: BTreeSet::new(),
            pending_helpers: VecDeque::new(),
            helpers: vec![],
            pending_result: None,
        }
    }

    fn push_result(&mut self, name: &str, status: &str) -> Result<()> {
        self.rows.push(Row {
            kind: self.exe.kind.clone(),
            package: self.exe.package.clone(),
            target: self.exe.target.clone(),
            test: field(name)?.into(),
            status: status.into(),
        });
        Ok(())
    }

    fn reconcile_helpers(&mut self) -> Result<()> {
        ensure!(
            self.pending_helpers.is_empty(),
            "incomplete inventory: nested helper"
        );
        let mut remaining = BTreeMap::new();
        for row in &self.rows {
            let (status, count) = remaining
                .entry(row.test.as_str())
                .or_insert((&row.status, 0));
            ensure!(
                *status == &row.status,
                "inconsistent inventory: conflicting test status"
            );
            *count += 1;
        }
        let mut child = vec![false; self.rows.len()];
        // Filtered single-test helpers may interleave with the outer harness.
        // Exclude only a redundant successful observation within a balanced
        // announcement/summary interval, leaving one identical outer result.
        // A conservative chronological assignment may refuse harder streams;
        // it never invents a missing outer identity or status.
        for &(start, end) in &self.helpers {
            let index = (start..end)
                .find(|&i| {
                    !child[i]
                        && self.rows[i].status == "ok"
                        && remaining[self.rows[i].test.as_str()].1 > 1
                })
                .context("inconsistent inventory: ambiguous or missing nested helper row")?;
            child[index] = true;
            remaining
                .get_mut(self.rows[index].test.as_str())
                .context("helper row")?
                .1 -= 1;
        }
        ensure!(
            remaining.values().all(|(_, n)| *n == 1),
            "inconsistent inventory: duplicate test"
        );
        ensure!(
            self.notices
                .iter()
                .all(|name| remaining.contains_key(name.as_str())),
            "incomplete inventory: running notice without terminal row"
        );
        let mut i = 0;
        self.rows.retain(|_| {
            let keep = !child[i];
            i += 1;
            keep
        });
        Ok(())
    }
}

pub(super) fn parse(log: &str, map: &str, exit: i32) -> Result<Vec<Row>> {
    ensure!(log.len() <= MAX_LOG, "suite log exceeds budget");
    let expected = executables(map)?;
    let mut seen = BTreeSet::new();
    let mut current: Option<Section> = None;
    let mut doc: Option<(String, Executable)> = None;
    let mut rows = Vec::new();
    for raw in log.lines() {
        let line = raw.trim();
        if let Some(s) = current.as_mut()
            && let Some(name) = s.pending_result.take()
        {
            // Only the immediately adjacent physical line can close the frame.
            // Check before all section/failure/helper handling so no boundary,
            // captured stdout or later test can supply a missing terminal.
            ensure!(
                raw == line,
                "inconsistent inventory: split terminal framing"
            );
            let status = terminal(line).context("inconsistent inventory: split terminal")?;
            s.push_result(&name, status)?;
            ensure!(
                rows.len() + s.rows.len() <= MAX_TESTS,
                "inventory count budget"
            );
            continue;
        }
        // Libtest prints captured stdout between two `failures:` headers, then
        // the failed-name list and its outer summary. Treat every result-like
        // line in captured output as data, including cargo/running headers.
        if let Some(s) = current.as_mut() {
            match &mut s.failures {
                FailureOutput::Captured => {
                    if line == "failures:" {
                        s.failures = FailureOutput::Names(BTreeSet::new());
                    }
                    continue;
                }
                FailureOutput::Names(names) if !line.starts_with("test result: ") => {
                    if !line.is_empty() {
                        ensure!(
                            raw.starts_with("    ")
                                && s.rows
                                    .iter()
                                    .any(|r| r.status == "failed" && r.test == line)
                                && names.insert(line.to_owned()),
                            "inconsistent inventory: failed-name list"
                        );
                    }
                    continue;
                }
                _ => {}
            }
        }
        if line.starts_with("Running ") || line.starts_with("Doc-tests ") {
            ensure!(
                current.is_none(),
                "incomplete inventory: executable without completed result"
            );
            let (key, exe) = if let Some(name) = line.strip_prefix("Doc-tests ") {
                let candidates: Vec<_> = expected
                    .values()
                    .filter(|e| {
                        ["lib", "proc-macro"].contains(&e.kind.as_str())
                            && e.target.replace('-', "_") == name
                    })
                    .collect();
                ensure!(
                    candidates.len() == 1,
                    "incomplete inventory: ambiguous or unknown doc target {name}"
                );
                let mut exe = candidates[0].clone();
                exe.kind = "doc".into();
                (format!("doc:{name}"), exe)
            } else {
                let path = line
                    .rsplit_once('(')
                    .and_then(|(_, p)| p.strip_suffix(')'))
                    .context("incomplete inventory: Running path")?;
                (
                    path.to_owned(),
                    expected
                        .get(path)
                        .context("incomplete inventory: unknown executable")?
                        .clone(),
                )
            };
            ensure!(
                !seen.contains(&key),
                "inconsistent inventory: repeated executable {key}"
            );
            doc = (exe.kind == "doc").then(|| (key.clone(), exe.clone()));
            current = Some(Section::new(key, exe));
        } else if let Some(running) = line.strip_prefix("running ") {
            let count: usize = running
                .strip_suffix(" tests")
                .or_else(|| running.strip_suffix(" test"))
                .context("inconsistent inventory: running count")?
                .parse()?;
            ensure!(count <= MAX_TESTS, "inventory count budget");
            if current.is_none() {
                let (key, exe) = doc
                    .as_ref()
                    .context("inconsistent inventory: running without executable")?;
                current = Some(Section::new(key.clone(), exe.clone()));
            }
            let s = current.as_mut().context("inventory section")?;
            if s.announced.is_some() {
                ensure!(
                    s.exe.kind != "doc" && count == 1,
                    "inconsistent inventory: unsupported nested running count"
                );
                s.pending_helpers.push_back(s.rows.len());
                ensure!(
                    s.pending_helpers.len() + s.helpers.len() <= MAX_TESTS,
                    "inventory count budget"
                );
            } else {
                s.announced = Some(count);
            }
        } else if line == "failures:" {
            if let Some(s) = current.as_mut() {
                ensure!(
                    s.pending_helpers.is_empty(),
                    "inconsistent inventory: nested failures"
                );
                s.failures = FailureOutput::Captured;
            }
        } else if let Some(result) = line.strip_prefix("test result: ") {
            let s = current
                .as_mut()
                .context("inconsistent inventory: result without executable")?;
            if let FailureOutput::Names(names) = &s.failures {
                ensure!(
                    *names
                        == s.rows
                            .iter()
                            .filter(|r| r.status == "failed")
                            .map(|r| r.test.clone())
                            .collect(),
                    "inconsistent inventory: failed-name list"
                );
            }
            let words: Vec<_> = result.split_whitespace().collect();
            ensure!(
                words.len() == 15
                    && ["ok.", "FAILED."].contains(&words[0])
                    && words[11..14] == ["out;", "finished", "in"],
                "inconsistent inventory: malformed result"
            );
            let mut counts = [0usize; 5];
            for (i, label) in ["passed;", "failed;", "ignored;", "measured;", "filtered"]
                .iter()
                .enumerate()
            {
                ensure!(
                    words.get(2 + 2 * i) == Some(label),
                    "inconsistent inventory: result fields"
                );
                counts[i] = words[1 + 2 * i].parse()?;
            }
            let seconds: f64 = words[14]
                .strip_suffix('s')
                .context("inconsistent inventory: result duration")?
                .parse()?;
            ensure!(
                seconds.is_finite() && seconds >= 0.0,
                "inconsistent inventory: result duration"
            );
            if counts[4] != 0 {
                ensure!(
                    s.exe.kind != "doc"
                        && matches!(s.failures, FailureOutput::None)
                        && words[0] == "ok."
                        && counts[..4] == [1, 0, 0, 0]
                        && counts[4].checked_add(1) == s.announced,
                    "inconsistent inventory: unsupported nested summary"
                );
                let start = s
                    .pending_helpers
                    .pop_front()
                    .context("inconsistent inventory: nested summary without announcement")?;
                s.helpers.push((start, s.rows.len()));
                continue;
            }
            s.reconcile_helpers()?;
            let count = |status: &str| s.rows.iter().filter(|r| r.status == status).count();
            ensure!(
                counts[0] == count("ok")
                    && counts[1] == count("failed")
                    && counts[2] == count("ignored")
                    && counts[3] == 0
                    && counts[4] == 0
                    && s.announced == Some(s.rows.len()),
                "inconsistent inventory: counts for {}",
                s.key
            );
            ensure!(
                (words[0] == "ok.") == (counts[1] == 0),
                "inconsistent inventory: result status"
            );
            let s = current.take().context("inventory section")?;
            seen.insert(s.key);
            rows.extend(s.rows);
        } else if let Some(test) = line.strip_prefix("test ") {
            let s = current
                .as_mut()
                .context("inconsistent inventory: test without executable")?;
            if let Some(name) = test.strip_suffix(" has been running for over 60 seconds") {
                ensure!(
                    s.announced.is_some()
                        && !s.rows.iter().any(|r| r.test == name)
                        && s.notices.insert(field(name)?.to_owned()),
                    "inconsistent inventory: nonterminal running notice"
                );
                continue;
            }
            let (name, status) = test
                .rsplit_once(" ... ")
                .context("inconsistent inventory: malformed test line")?;
            ensure!(
                s.announced.is_some(),
                "inconsistent inventory: test before running count"
            );
            if let Some(status) = terminal(status).or_else(|| framed_terminal(status)) {
                s.push_result(name, status)?;
            } else {
                ensure!(
                    version_output(status) && s.pending_helpers.is_empty(),
                    "inconsistent inventory: test status"
                );
                s.pending_result = Some(field(name)?.into());
            }
        } else if terminal(line).is_some() {
            bail!("inconsistent inventory: orphan terminal");
        }
        ensure!(
            rows.len() + current.as_ref().map_or(0, |s| s.rows.len()) <= MAX_TESTS,
            "inventory count budget"
        );
    }
    ensure!(
        current.is_none() && expected.keys().all(|key| seen.contains(key)),
        "incomplete inventory: missing completed executable"
    );
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
    ensure!(
        (exit == 0) == rows.iter().all(|r| r.status != "failed"),
        "inconsistent inventory: process exit {exit}"
    );
    // Return failed names too. The caller judges failure, retaining the inventory.
    Ok(rows)
}

pub(super) fn tsv(rows: &[Row]) -> String {
    rows.iter()
        .map(|r| {
            format!(
                "{}\t{}\t{}\t{}\t{}\n",
                r.kind, r.package, r.target, r.test, r.status
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    fn map() -> String {
        serde_json::json!({"reason":"compiler-artifact","package_id":"path+file:///fixture#fixture@0.1.0","profile":{"test":true},"executable":"/fixture/target/test","target":{"kind":["lib"],"name":"fixture"}}).to_string()
    }
    fn log() -> &'static str {
        "Running unittests src/lib.rs (/fixture/target/test)\nrunning 2 tests\ntest one ... ok\ntest two ... ignored\ntest result: ok. 1 passed; 0 failed; 1 ignored; 0 measured; 0 filtered out; finished in 0s\nDoc-tests fixture\nrunning 0 tests\ntest result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0s\n"
    }
    #[test]
    fn accepts_full_inventory_and_keeps_ignored() {
        let rows = parse(log(), &map(), 0).unwrap();
        assert_eq!(rows.len(), 2);
        assert!(tsv(&rows).contains("lib\tfixture\tfixture\tone\tok"));
    }
    #[test]
    fn failed_names_survive_and_status_disagreement_fails() {
        let log = log()
            .replace("one ... ok", "one ... FAILED")
            .replace("ok. 1 passed; 0 failed", "FAILED. 0 passed; 1 failed");
        assert!(
            parse(&log, &map(), 0)
                .unwrap_err()
                .to_string()
                .contains("process exit")
        );
        assert_eq!(parse(&log, &map(), 101).unwrap()[0].status, "failed");
    }
    #[test]
    fn captured_failure_output_cannot_supply_outer_results_or_names() {
        let log = "Running unittests src/lib.rs (/fixture/target/test)\nrunning 2 tests\ntest one ... FAILED\ntest two ... FAILED\n\nfailures:\n\n---- one stdout ----\nRunning forged (/forged)\nrunning 100 tests\ntest forged ... ok\ntest result: ok. 100 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0s\n\n---- two stdout ----\ntest result: not a summary\n\nfailures:\n    one\n    two\n\ntest result: FAILED. 0 passed; 2 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0s\n";
        let rows = parse(log, &map(), 101).unwrap();
        assert_eq!(
            tsv(&rows),
            "lib\tfixture\tfixture\tone\tfailed\nlib\tfixture\tfixture\ttwo\tfailed\n"
        );
        assert!(parse(log, &map(), 0).is_err());
        // A matching list and complete outer result are still required.
        for bad in [
            log.replace("    two\n", "    forged\n"),
            log.replace("    two\n", "    one\n"),
            log.rsplit_once("test result: FAILED.")
                .unwrap()
                .0
                .to_owned(),
        ] {
            assert!(parse(&bad, &map(), 101).is_err());
        }
    }

    #[test]
    fn missing_zero_nested_duplicate_and_spliced_results_refuse() {
        for log in [
            String::new(),
            log().replace("running 2 tests", "running 3 tests"),
            log().replace("test one ... ok", "test one ... ok\ntest one ... ok"),
            log().replace("one ... ok", "one ... child noise ok"),
            log().replace("test result: ok. 1", "noise test result: ok. 1"),
        ] {
            assert!(parse(&log, &map(), 0).is_err(), "{log}");
        }
        let zero = "Running unittests src/lib.rs (/fixture/target/test)\nrunning 0 tests\ntest result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0s\n";
        assert!(
            parse(zero, &map(), 0)
                .unwrap_err()
                .to_string()
                .contains("no test executed")
        );
        assert!(parse(log(), &format!("{}\n{}", map(), map()), 0).is_err());
    }

    fn result(passed: usize, ignored: usize, filtered: usize) -> String {
        format!(
            "test result: ok. {passed} passed; 0 failed; {ignored} ignored; 0 measured; {filtered} filtered out; finished in 0.00s\n"
        )
    }

    fn nested() -> String {
        "Running unittests src/lib.rs (/fixture/target/test)\nrunning 3 tests\nrunning 1 test\nrunning 1 test\ntest one ... ok\n".to_owned()
            + &result(1, 0, 2)
            + "test one ... ok\ntest two ... ok\n"
            + &result(1, 0, 2)
            + "test two ... ok\ntest three ... ignored\n"
            + &result(2, 1, 0)
    }

    #[test]
    fn standard_running_notice_requires_its_later_terminal_row() {
        let notice = "test one has been running for over 60 seconds\n";
        let good = log().replace("test one ... ok", &(notice.to_owned() + "test one ... ok"));
        assert_eq!(
            parse(&good, &map(), 0).unwrap(),
            parse(log(), &map(), 0).unwrap()
        );
        for bad in [
            good.replace("test one ... ok\n", ""),
            good.replace("one has been", "unknown has been"),
            good.replace("60 seconds", "sixty seconds"),
            good.replace(notice, &(notice.to_owned() + notice)),
            good.replace(
                &(notice.to_owned() + "test one ... ok"),
                &("test one ... ok\n".to_owned() + notice),
            ),
        ] {
            assert!(parse(&bad, &map(), 0).is_err(), "{bad}");
        }
    }

    #[test]
    fn concurrent_filtered_helpers_reconcile_only_redundant_observations() {
        let rows = parse(&nested(), &map(), 0).unwrap();
        assert_eq!(
            tsv(&rows),
            "lib\tfixture\tfixture\tone\tok\nlib\tfixture\tfixture\tthree\tignored\nlib\tfixture\tfixture\ttwo\tok\n"
        );
        // The outer row can precede the helper's announcement (orch-infra).
        let earlier = "Running unittests src/lib.rs (/fixture/target/test)\nrunning 2 tests\ntest one ... ok\nrunning 1 test\ntest one ... ok\n".to_owned()
            + &result(1, 0, 1) + "test two ... ignored\n" + &result(1, 1, 0);
        assert_eq!(
            parse(&earlier, &map(), 0).unwrap(),
            parse(log(), &map(), 0).unwrap()
        );
        for bad in [
            nested().replacen("running 1 test\n", "", 1),
            nested().replacen(&result(1, 0, 2), "", 1),
            nested().replacen("test one ... ok\n", "", 1),
            nested().replacen("test one ... ok\n", "test forged ... ok\n", 1),
            nested().replacen("test one ... ok\n", "test one ... FAILED\n", 1),
            nested().replacen("test one ... ok\n", "test one ... ok\ntest one ... ok\n", 1),
            nested().replacen(&result(1, 0, 2), &result(1, 0, 1), 1),
            nested().replacen("running 1 test\n", "running 2 tests\n", 1),
            nested().replacen("test one ... ok\n", "test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 2 filtered out; finished in 0.00s\ntest one ... ok\n", 1),
        ] {
            assert!(parse(&bad, &map(), 0).is_err(), "{bad}");
        }
        assert!(parse(&nested(), &map(), 101).is_err());
    }

    #[test]
    fn doc_target_keeps_multiple_complete_disjoint_groups() {
        let good = log().to_owned()
            + "running 1 test\ntest src/lib.rs - (line 1) ... ok\n"
            + &result(1, 0, 0)
            + "running 1 test\ntest src/lib.rs - (line 2) - compile fail ... ok\n"
            + &result(1, 0, 0);
        let rows = parse(&good, &map(), 0).unwrap();
        assert_eq!(rows.len(), 4);
        assert_eq!(rows.iter().filter(|r| r.kind == "doc").count(), 2);
        for bad in [
            good.replace(
                "src/lib.rs - (line 2) - compile fail",
                "src/lib.rs - (line 1)",
            ),
            good.rsplit_once("test result:").unwrap().0.to_owned(),
            good.replace(
                "running 1 test\ntest src/lib.rs - (line 2)",
                "running 2 tests\ntest src/lib.rs - (line 2)",
            ),
            good.replace(
                "running 1 test\ntest src/lib.rs - (line 2)",
                "Doc-tests fixture\nrunning 1 test\ntest src/lib.rs - (line 2)",
            ),
        ] {
            assert!(parse(&bad, &map(), 0).is_err(), "{bad}");
        }
        let failed = good.replace("(line 1) ... ok", "(line 1) ... FAILED")
            .replacen(&result(1, 0, 0), "test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s\n", 1);
        assert_eq!(
            parse(&failed, &map(), 101)
                .unwrap()
                .iter()
                .filter(|r| r.status == "failed")
                .count(),
            1
        );
        assert!(parse(&failed, &map(), 0).is_err());
    }
}
