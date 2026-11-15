//! Strict sequential libtest inventory. Child chatter can cause a false failure,
//! never an inferred pass. A completed executable is required even for zero tests.

use anyhow::{Context, Result, bail, ensure};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

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

struct Section {
    key: String,
    exe: Executable,
    announced: Option<usize>,
    rows: Vec<Row>,
    failures: bool,
}

pub(super) fn parse(log: &str, map: &str, exit: i32) -> Result<Vec<Row>> {
    ensure!(log.len() <= MAX_LOG, "suite log exceeds budget");
    let expected = executables(map)?;
    let mut seen = BTreeSet::new();
    let mut current: Option<Section> = None;
    let mut rows = Vec::new();
    for raw in log.lines() {
        let line = raw.trim();
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
            current = Some(Section {
                key,
                exe,
                announced: None,
                rows: vec![],
                failures: false,
            });
        } else if line.starts_with("running ")
            && (line.ends_with(" tests") || line.ends_with(" test"))
        {
            let s = current
                .as_mut()
                .context("inconsistent inventory: running without executable")?;
            ensure!(
                s.announced.is_none(),
                "inconsistent inventory: nested running count"
            );
            s.announced = Some(
                line.split_whitespace()
                    .nth(1)
                    .context("running count")?
                    .parse()?,
            );
        } else if line == "failures:" {
            if let Some(s) = current.as_mut() {
                s.failures = true;
            }
        } else if let Some(result) = line.strip_prefix("test result: ") {
            let s = current
                .take()
                .context("inconsistent inventory: result without executable")?;
            let words: Vec<_> = result.split_whitespace().collect();
            ensure!(
                words.len() >= 11 && ["ok.", "FAILED."].contains(&words[0]),
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
            seen.insert(s.key);
            rows.extend(s.rows);
        } else if let Some(test) = line.strip_prefix("test ") {
            let s = current
                .as_mut()
                .context("inconsistent inventory: test without executable")?;
            if s.failures {
                continue;
            }
            let (name, status) = test
                .rsplit_once(" ... ")
                .context("inconsistent inventory: malformed test line")?;
            let status = match status {
                "ok" => "ok",
                "FAILED" => "failed",
                s if s == "ignored" || s.starts_with("ignored,") => "ignored",
                _ => bail!("inconsistent inventory: test status"),
            };
            ensure!(
                s.announced.is_some(),
                "inconsistent inventory: test before running count"
            );
            s.rows.push(Row {
                kind: s.exe.kind.clone(),
                package: s.exe.package.clone(),
                target: s.exe.target.clone(),
                test: field(name)?.into(),
                status: status.into(),
            });
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
}
