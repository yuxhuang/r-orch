//! Independent folded-suite engine. Production tiers call this only after a
//! supported native target admission; fixtures exercise the identical judgments.
use super::{
    hermetic::Hermetic,
    inventory,
    report::{self, Step, Verdict},
    steps::{LogFailure, Runner},
};
use anyhow::{Result, ensure};
use std::collections::BTreeMap;
use std::path::Path;
use std::time::Instant;

pub(super) struct Suite {
    pub steps: Vec<Step>,
    pub logs: BTreeMap<String, Vec<u8>>,
    pub inventory: String,
}
impl Suite {
    fn checked(&mut self, name: &'static str, start: Instant, result: Result<String>) {
        let (verdict, detail) = match result {
            Ok(s) => (Verdict::Pass, s),
            Err(e) => (Verdict::Fail, format!("{e:#}")),
        };
        self.logs
            .insert(format!("steps/{name}.log"), detail.as_bytes().to_vec());
        self.steps.push(Step {
            name,
            verdict,
            exit: Some(if verdict == Verdict::Pass { 0 } else { 1 }),
            elapsed: start.elapsed(),
            detail,
        });
    }
}

pub(super) fn run(repo: &Path, hermetic: &Hermetic, runner: &mut Runner<'_, '_>) -> Suite {
    run_scoped(repo, hermetic, runner, None)
}

fn scope(
    command: &mut std::process::Command,
    selection: Option<&crate::gate_selection::Selection>,
) {
    match selection {
        Some(s) if s.mode == crate::gate_selection::Mode::Packages => {
            for package in &s.packages {
                command.arg("-p").arg(package);
            }
        }
        _ => {
            command.arg("--workspace");
        }
    }
}

fn run_scoped(
    repo: &Path,
    hermetic: &Hermetic,
    runner: &mut Runner<'_, '_>,
    selection: Option<&crate::gate_selection::Selection>,
) -> Suite {
    let mut out = Suite {
        steps: vec![],
        logs: BTreeMap::new(),
        inventory: String::new(),
    };
    let start = Instant::now();
    // Explicitly finish the workspace's public binaries before any test can
    // exec one by path. A pre-existing target/debug/orch is never that proof.
    let mut workspace = hermetic.command(repo);
    workspace
        .args(["build", "--locked", "--manifest-path"])
        .arg(repo.join("Cargo.toml"))
        .arg("--all-targets");
    scope(&mut workspace, selection);
    let workspace_start = runner.statuses().len();
    let workspace_status = runner.logged(&mut workspace, inventory::MAX_LOG);
    let mut build = hermetic.command(repo);
    build
        .args(["test", "--quiet", "--locked", "--manifest-path"])
        .arg(repo.join("Cargo.toml"))
        .args(["--no-run", "--message-format=json"]);
    scope(&mut build, selection);
    let map_start = runner.statuses().len();
    let mapped = match &workspace_status {
        Ok(log) if log.exit.success() => runner.capture_raw(&mut build, inventory::MAX_LOG),
        Ok(log) => Err(anyhow::anyhow!(
            "workspace build exited {}",
            report::code(log.exit)
        )),
        Err(e) => Err(anyhow::anyhow!("workspace build: {e:#}")),
    };
    let build_exit = if workspace_status
        .as_ref()
        .is_ok_and(|log| log.exit.success())
    {
        runner
            .statuses()
            .get(map_start)
            .map(|s| report::code(s.exit))
    } else {
        runner
            .statuses()
            .get(workspace_start)
            .map(|s| report::code(s.exit))
    };
    let built = mapped
        .as_ref()
        .map_err(|e| anyhow::anyhow!("{e:#}"))
        .and_then(|log| {
            ensure!(
                log.exit.success(),
                "test map compilation failed: child exit {}",
                report::code(log.exit)
            );
            Ok(log.stdout.clone())
        });
    let (verdict, detail) = match &built {
        Ok(_) => (Verdict::Pass, "test executable map compiled".into()),
        Err(e) => (Verdict::Fail, format!("{e:#}")),
    };
    let workspace_log = match &workspace_status {
        Ok(log) => log.bytes.clone(),
        Err(e) => e
            .downcast_ref::<LogFailure>()
            .map_or_else(|| format!("{e:#}").into_bytes(), |f| f.bytes.clone()),
    };
    let mut build_log=serde_json::json!({"reason":"gate-build-log","text":String::from_utf8_lossy(&workspace_log)}).to_string().into_bytes();
    build_log.push(b'\n');
    if let Ok(mapped) = &mapped {
        build_log.extend(serde_json::json!({"reason":"gate-map-stderr","text":String::from_utf8_lossy(&mapped.stderr),"exit":report::code(mapped.exit)}).to_string().as_bytes());
        build_log.push(b'\n');
        build_log.extend(&mapped.stdout);
        build_log.push(b'\n');
    }
    match &built {
        Ok(_) => {}
        Err(_) => build_log.extend(
            serde_json::json!({"reason":"gate-build-error","text":detail})
                .to_string()
                .as_bytes(),
        ),
    };
    out.logs.insert("steps/build.log".into(), build_log);
    out.steps.push(Step {
        name: "build",
        verdict,
        exit: build_exit,
        elapsed: start.elapsed(),
        detail,
    });
    let start = Instant::now();
    let mut command = hermetic.command(repo);
    command
        .args(["test", "--locked", "--manifest-path"])
        .arg(repo.join("Cargo.toml"))
        .arg("--no-fail-fast");
    scope(&mut command, selection);
    let status_start = runner.statuses().len();
    let result = if built.is_ok() {
        runner.logged(&mut command, inventory::MAX_LOG)
    } else {
        Err(anyhow::anyhow!("suite did not start: build failed"))
    };
    let (exit, bytes, collection) = match result {
        Ok(log) => (Some(report::code(log.exit)), log.bytes, Ok(())),
        Err(e) => {
            let bytes = e
                .downcast_ref::<LogFailure>()
                .map_or_else(Vec::new, |f| f.bytes.clone());
            let exit = runner
                .statuses()
                .get(status_start)
                .map(|s| report::code(s.exit));
            (exit, bytes, Err(e))
        }
    };
    out.logs.insert("steps/test.log".into(), bytes.clone());
    let judged = (|| -> Result<String> {
        collection?;
        let log = std::str::from_utf8(&bytes)?;
        let map = std::str::from_utf8(built.as_ref().map_err(|e| anyhow::anyhow!("{e:#}"))?)?;
        let rows = inventory::parse(log, map, exit.unwrap_or(1))?;
        out.inventory = inventory::tsv(&rows);
        let failed: Vec<_> = rows
            .iter()
            .filter(|r| r.status == "failed")
            .map(|r| format!("{}::{}", r.target, r.test))
            .collect();
        ensure!(
            exit == Some(0) && failed.is_empty(),
            "suite failed (child exit {:?}): {}",
            exit,
            failed.join(", ")
        );
        hermetic.witness()?;
        Ok(format!(
            "{} passed, 0 failed, {} ignored (same judged sentinel/shim suite)",
            rows.iter().filter(|r| r.status == "ok").count(),
            rows.iter().filter(|r| r.status == "ignored").count()
        ))
    })();
    let (verdict, detail) = match judged {
        Ok(s) => (Verdict::Pass, s),
        Err(e) => (Verdict::Fail, format!("{e:#}")),
    };
    out.steps.push(Step {
        name: "test",
        verdict,
        exit,
        elapsed: start.elapsed(),
        detail,
    });
    let start = Instant::now();
    out.checked("home-hygiene", start, hermetic.hygiene());
    let start = Instant::now();
    out.checked("no-real-cli", start, hermetic.cli());
    out
}

/// Complete step ordering, independently of target admission/publication. This
/// is not exposed as a way around the production native guard.
pub(super) fn run_all(
    repo: &Path,
    real_home: &Path,
    hermetic: &Hermetic,
    runner: &mut Runner<'_, '_>,
) -> Suite {
    run_selected(repo, real_home, hermetic, runner, None)
}

pub(super) fn run_selected(
    repo: &Path,
    real_home: &Path,
    hermetic: &Hermetic,
    runner: &mut Runner<'_, '_>,
    part: Option<super::Part>,
) -> Suite {
    let before = super::real_home::snapshot(real_home);
    if let Err(e) = &before {
        let mut suite = Suite {
            steps: vec![],
            logs: BTreeMap::new(),
            inventory: String::new(),
        };
        suite.checked(
            "real-home",
            Instant::now(),
            Err(anyhow::anyhow!("initial real-home snapshot: {e:#}")),
        );
        return suite;
    }
    let mut suite = if part == Some(super::Part::Static) {
        Suite {
            steps: vec![],
            logs: BTreeMap::new(),
            inventory: String::new(),
        }
    } else {
        run(repo, hermetic, runner)
    };
    suite.steps.truncate(2); // Re-evaluate hygiene after the static commands.
    if part != Some(super::Part::Suite) {
        static_steps(repo, hermetic, runner, &mut suite, false);
    }
    if part == Some(super::Part::Static) {
        return suite;
    }
    let start = Instant::now();
    suite.checked("home-hygiene", start, hermetic.hygiene());
    let start = Instant::now();
    suite.checked("no-real-cli", start, hermetic.cli());
    let start = Instant::now();
    suite.checked(
        "real-home",
        start,
        super::real_home::snapshot(real_home)
            .and_then(|after| super::real_home::compare(&before.unwrap(), &after))
            .map(|(text, _)| text),
    );
    suite
}

fn static_steps(
    repo: &Path,
    hermetic: &Hermetic,
    runner: &mut Runner<'_, '_>,
    suite: &mut Suite,
    fast: bool,
) {
    let names: &[&str] = if fast {
        &["fmt", "clippy", "crate-graph"]
    } else {
        &["clippy", "fmt", "advisories", "crate-graph"]
    };
    for &name in names {
        let start = Instant::now();
        if name == "advisories" && !hermetic.available("cargo-deny") {
            let detail="cargo-deny not installed; required advisories/licenses/bans/sources were not evaluated; cargo install cargo-deny --locked".to_owned();
            suite
                .logs
                .insert(format!("steps/{name}.log"), detail.as_bytes().to_vec());
            suite.steps.push(Step {
                name,
                verdict: Verdict::EnvLimited,
                exit: None,
                elapsed: start.elapsed(),
                detail,
            });
            continue;
        }
        let mut command = if name == "advisories" {
            hermetic.program("cargo-deny")
        } else {
            hermetic.command(repo)
        };
        match name {
            "clippy" => {
                command
                    .args(["clippy", "--locked", "--manifest-path"])
                    .arg(repo.join("Cargo.toml"))
                    .args(["--workspace", "--all-targets", "--", "-D", "warnings"]);
            }
            "fmt" => {
                command
                    .args(["fmt", "--manifest-path"])
                    .arg(repo.join("Cargo.toml"))
                    .args(["--all", "--check"]);
            }
            "advisories" => {
                command
                    .arg("--manifest-path")
                    .arg(repo.join("Cargo.toml"))
                    .args(["check", "advisories", "licenses", "bans", "sources"]);
            }
            "crate-graph" => {
                command
                    .current_dir(repo)
                    .args(["run", "--locked", "--quiet", "--manifest-path"])
                    .arg(repo.join("Cargo.toml"))
                    .args(["-p", "xtask", "--", "crate-graph"]);
            }
            _ => unreachable!(),
        }
        let index = runner.statuses().len();
        let result = runner.logged(&mut command, inventory::MAX_LOG);
        let (exit, bytes, verdict, detail) = match result {
            Ok(log) => {
                let exit = report::code(log.exit);
                let detail = if name == "fmt" {
                    format!(
                        "{} file(s) need reformatting",
                        String::from_utf8_lossy(&log.bytes)
                            .lines()
                            .filter(|s| s.starts_with("Diff in "))
                            .count()
                    )
                } else {
                    format!("child exit {exit}")
                };
                (
                    Some(exit),
                    log.bytes,
                    if exit == 0 {
                        Verdict::Pass
                    } else {
                        Verdict::Fail
                    },
                    detail,
                )
            }
            Err(e) => (
                runner.statuses().get(index).map(|s| report::code(s.exit)),
                e.downcast_ref::<LogFailure>()
                    .map_or_else(Vec::new, |f| f.bytes.clone()),
                Verdict::Fail,
                format!("{e:#}"),
            ),
        };
        suite.logs.insert(format!("steps/{name}.log"), bytes);
        suite.steps.push(Step {
            name,
            verdict,
            exit,
            elapsed: start.elapsed(),
            detail,
        });
    }
}

/// Local iteration uses the same native target, folded suite and judgments.
/// Documentation-only deliberately has no test or witness: it cannot qualify.
pub(super) fn fast_steps(selection: &crate::gate_selection::Selection) -> Vec<&'static str> {
    let mut steps = vec!["fmt", "clippy", "crate-graph"];
    if selection.mode == crate::gate_selection::Mode::DocsOnly {
        steps.push("build");
    } else {
        if needs_witness(selection) {
            steps.push("environment-witness");
        }
        steps.extend(["build", "test", "home-hygiene", "no-real-cli", "real-home"]);
    }
    steps
}
fn needs_witness(s: &crate::gate_selection::Selection) -> bool {
    s.mode == crate::gate_selection::Mode::Packages && !s.packages.iter().any(|p| p == "xtask")
}
pub(super) fn run_fast(
    repo: &Path,
    real_home: &Path,
    h: &Hermetic,
    runner: &mut Runner<'_, '_>,
    selection: &crate::gate_selection::Selection,
) -> Suite {
    let mut out = Suite {
        steps: vec![],
        logs: BTreeMap::new(),
        inventory: String::new(),
    };
    let before = super::real_home::snapshot(real_home);
    if let Err(e) = &before {
        out.checked(
            "real-home",
            Instant::now(),
            Err(anyhow::anyhow!("initial real-home snapshot: {e:#}")),
        );
        return out;
    }
    static_steps(repo, h, runner, &mut out, true);
    if selection.mode == crate::gate_selection::Mode::DocsOnly {
        let start = Instant::now();
        let mut command = h.command(repo);
        command
            .args(["build", "--locked", "--manifest-path"])
            .arg(repo.join("Cargo.toml"))
            .args(["--workspace", "--all-targets"]);
        let index = runner.statuses().len();
        let result = runner.logged(&mut command, inventory::MAX_LOG);
        let (bytes, judged) = match result {
            Ok(log) => (
                log.bytes,
                if log.exit.success() {
                    Ok("documentation-only build; zero suite tests".to_owned())
                } else {
                    Err(anyhow::anyhow!("build exited {}", report::code(log.exit)))
                },
            ),
            Err(e) => (
                e.downcast_ref::<LogFailure>()
                    .map_or_else(Vec::new, |f| f.bytes.clone()),
                Err(e),
            ),
        };
        out.checked("build", start, judged);
        out.steps.last_mut().unwrap().exit =
            runner.statuses().get(index).map(|s| report::code(s.exit));
        out.logs.insert("steps/build.log".into(), bytes);
    } else {
        // Most package closures exclude xtask. Run its witness target unfiltered,
        // including fixture-support tests, but not its entire package suite. Its independent inventory cannot turn an empty
        // selected suite into a nonzero successful one.
        if needs_witness(selection) {
            let start = Instant::now();
            let mut map = h.command(repo);
            map.args(["test", "--quiet", "--locked", "--manifest-path"])
                .arg(repo.join("Cargo.toml"))
                .args([
                    "-p",
                    "xtask",
                    "--test",
                    "gate_witness",
                    "--no-run",
                    "--message-format=json",
                ]);
            let mut bytes = Vec::new();
            let mut child_exit = None;
            let result = (|| -> Result<String> {
                let compiled = runner.capture_raw(&mut map, inventory::MAX_LOG)?;
                child_exit = Some(report::code(compiled.exit));
                out.logs
                    .insert("steps/witness-map.jsonl".into(), compiled.stdout.clone());
                out.logs
                    .insert("steps/witness-map-stderr.log".into(), compiled.stderr);
                ensure!(
                    compiled.exit.success(),
                    "environment witness compilation exited {}",
                    report::code(compiled.exit)
                );
                let mut command = h.command(repo);
                command
                    .args(["test", "--locked", "--manifest-path"])
                    .arg(repo.join("Cargo.toml"))
                    .args([
                        "-p",
                        "xtask",
                        "--test",
                        "gate_witness",
                    ]);
                let log = runner.logged(&mut command, inventory::MAX_LOG)?;
                child_exit = Some(report::code(log.exit));
                bytes = log.bytes;
                let rows = inventory::parse(
                    std::str::from_utf8(&bytes)?,
                    std::str::from_utf8(&compiled.stdout)?,
                    report::code(log.exit),
                )?;
                out.inventory = inventory::tsv(&rows);
                ensure!(
                    log.exit.success()
                        && rows.iter().all(|r| r.status == "ok")
                        && rows.iter().filter(|r| r.test == "gate_environment_witness").count() == 1,
                    "environment witness did not pass exactly once"
                );
                h.witness()?;
                Ok("one independently inventoried environment witness passed".into())
            })();
            if let Err(e) = &result
                && let Some(f) = e.downcast_ref::<LogFailure>()
            {
                bytes = f.bytes.clone();
            }
            out.checked("environment-witness", start, result);
            out.steps.last_mut().unwrap().exit = child_exit;
            out.logs
                .insert("steps/environment-witness.log".into(), bytes);
            if out.steps.last().unwrap().verdict != Verdict::Pass {
                return out;
            }
        }
        let mut selected = run_scoped(repo, h, runner, Some(selection));
        selected.steps.truncate(2); // Hygiene is judged after all selected commands.
        out.steps.extend(selected.steps);
        out.logs.extend(selected.logs);
        out.inventory.push_str(&selected.inventory);
        out.checked("home-hygiene", Instant::now(), h.hygiene());
        out.checked("no-real-cli", Instant::now(), h.cli());
        out.checked(
            "real-home",
            Instant::now(),
            super::real_home::snapshot(real_home)
                .and_then(|after| super::real_home::compare(&before.unwrap(), &after))
                .map(|(text, _)| text),
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gate::{
        files,
        lease::{Lease, Mode, Owned},
        steps::tests::Scratch,
    };
    use std::ffi::OsString;
    use std::os::unix::fs::MetadataExt;
    fn fixture(
        root: &Path,
        case: &str,
    ) -> (std::path::PathBuf, std::path::PathBuf, Hermetic, Lease) {
        let repo = root.join(case);
        std::fs::create_dir(&repo).unwrap();
        files::write(
            &repo.join("Cargo.toml"),
            b"[package]\nname=\"fold-fixture\"\nversion=\"0.1.0\"\nedition=\"2024\"\n[workspace]\n",
        )
        .unwrap();
        files::write(
            &repo.join("rust-toolchain.toml"),
            include_bytes!("../../../rust-toolchain.toml"),
        )
        .unwrap();
        files::write(
            &repo.join("Cargo.lock"),
            b"version = 4\n[[package]]\nname = \"fold-fixture\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        std::fs::create_dir(repo.join("src")).unwrap();
        let action = match case {
            "home" => {
                "std::fs::write(std::path::Path::new(&home).join(\"unexpected\"),b\"bad\").unwrap();"
            }
            "cli" => {
                "let status=std::process::Command::new(\"claude\").arg(\"--fixture\").status().unwrap();assert_eq!(status.code(),Some(127));"
            }
            "failure" => "panic!(\"folded-only fixture failure\");",
            _ => "",
        };
        let source = if case == "compile" {
            String::from("this is a planted Rust compilation error")
        } else if case == "zero" {
            String::from("pub fn no_tests() {}\n")
        } else {
            format!(
                r#"#[test] fn witness() {{
            let home=std::env::var("HOME").unwrap();
            let first=std::env::split_paths(&std::env::var_os("PATH").unwrap()).next().unwrap();
            let dir=std::env::var_os("XTASK_GATE_WITNESS_DIR").unwrap();
            let redirect=if std::env::var_os("ORCHESTRY_GLOBAL_CONFIG_PATH").is_some() {{"set"}} else {{"unset"}};
            std::fs::write(std::path::Path::new(&dir).join("witness.txt"),format!("schema=1\nhome={{home}}\nglobal_config_redirect={{redirect}}\npath_first={{}}\n",first.display())).unwrap();
            {action}
        }}"#
            )
        };
        files::write(&repo.join("src/lib.rs"), source.as_bytes()).unwrap();
        // Private homes use the already installed pinned toolchain, no install,
        // credentials, host cargo config or real HOME. Cargo needs no registry.
        let resources = root.join(format!("resources-{case}"));
        std::fs::create_dir(&resources).unwrap();
        let cargo = resources.join("cargo");
        let rustup = resources.join("rustup");
        let home = resources.join("home");
        for path in [&cargo, &rustup, &home] {
            std::fs::create_dir(path).unwrap();
        }
        std::fs::create_dir(rustup.join("toolchains")).unwrap();
        let output = std::process::Command::new("rustup")
            .args(["which", "rustc"])
            .output()
            .unwrap();
        assert!(output.status.success());
        let compiler = std::path::PathBuf::from(String::from_utf8(output.stdout).unwrap().trim());
        let toolchain = compiler.parent().unwrap().parent().unwrap();
        if case != "toolchain" {
            std::os::unix::fs::symlink(
                toolchain,
                rustup
                    .join("toolchains")
                    .join(toolchain.file_name().unwrap()),
            )
            .unwrap();
        }
        let rustup_bin = std::process::Command::new("/usr/bin/which")
            .arg("rustup")
            .output()
            .unwrap();
        assert!(rustup_bin.status.success());
        let proxy = std::path::PathBuf::from(String::from_utf8(rustup_bin.stdout).unwrap().trim())
            .parent()
            .unwrap()
            .to_path_buf();
        let mut path = std::env::join_paths([
            proxy,
            toolchain.join("bin"),
            "/usr/bin".into(),
            "/bin".into(),
            "/usr/sbin".into(),
            "/sbin".into(),
        ])
        .unwrap();
        if case == "spawn" {
            let empty = resources.join("empty-bin");
            std::fs::create_dir(&empty).unwrap();
            path = empty.into_os_string();
        }
        let env: BTreeMap<OsString, OsString> = [
            ("HOME".into(), home.into_os_string()),
            ("PATH".into(), path),
            ("CARGO_HOME".into(), cargo.into_os_string()),
            ("RUSTUP_HOME".into(), rustup.into_os_string()),
        ]
        .into();
        let repo = repo.canonicalize().unwrap();
        let target = repo.join("target");
        let h = Hermetic::new(&repo, &target, root, env).unwrap();
        let locks = root.join(format!("locks-{case}"));
        std::fs::create_dir(&locks).unwrap();
        let owner = std::fs::metadata(&locks).unwrap().uid();
        let lease = Lease::Owned(
            Owned::acquire(&locks, owner, Mode::Exclusive, std::time::Duration::ZERO).unwrap(),
        );
        (repo, target, h, lease)
    }
    #[test]
    fn actual_folded_suite_judges_failures_and_both_hygiene_checks() {
        let root = Scratch::new();
        for case in [
            "positive",
            "failure",
            "home",
            "cli",
            "zero",
            "toolchain",
            "compile",
            "spawn",
        ] {
            let (repo, _target, h, lease) = fixture(&root.0, case);
            let mut runner = Runner::without_marker(&lease).unwrap();
            if case == "spawn" {
                assert!(
                    runner
                        .run(std::process::Command::new("/bin/sh").args(["-c", "exit 0"]))
                        .unwrap()
                        .success()
                );
            }
            let suite = run(&repo, &h, &mut runner);
            if let Some(directory) = std::env::var_os("XTASK_F45_RETAIN") {
                let directory = std::path::PathBuf::from(directory);
                let retained = files::private_dir(&directory, &format!("folded-{case}")).unwrap();
                for (name, bytes) in &suite.logs {
                    files::write(&retained.join(name.rsplit('/').next().unwrap()), bytes).unwrap();
                }
                files::write(&retained.join("inventory.tsv"), suite.inventory.as_bytes()).unwrap();
                files::write(
                    &retained.join("result.txt"),
                    report::table(&suite.steps).as_bytes(),
                )
                .unwrap();
                files::write(
                    &retained.join("environment.txt"),
                    h.environment().as_bytes(),
                )
                .unwrap();
            }
            let fail = |name| {
                suite.steps.iter().find(|s| s.name == name).unwrap().verdict == Verdict::Fail
            };
            let expected = match case {
                "positive" => (false, false, false),
                "failure" => (true, false, false),
                "home" => (false, true, false),
                "cli" => (false, false, true),
                "zero" | "toolchain" | "compile" | "spawn" => (true, true, true),
                _ => unreachable!(),
            };
            assert_eq!(
                (fail("test"), fail("home-hygiene"), fail("no-real-cli")),
                expected,
                "{case}: {}",
                report::table(&suite.steps)
            );
            if case == "failure" {
                assert_eq!(suite.steps[1].exit, Some(101));
                assert!(suite.inventory.contains("witness\tfailed"));
            }
            if case == "zero" {
                assert!(suite.steps[1].detail.contains("no test executed"));
            }
            if case == "toolchain" {
                assert!(fail("build"));
                assert_eq!(suite.steps[0].exit, Some(1));
                assert!(
                    !String::from_utf8_lossy(&suite.logs["steps/build.log"])
                        .contains("downloading")
                );
            }
            if case == "compile" {
                assert!(fail("build"));
                assert_eq!(suite.steps[0].exit, Some(101));
                assert!(
                    String::from_utf8_lossy(&suite.logs["steps/build.log"]).contains("planted")
                );
            }
            if case == "spawn" {
                assert!(fail("build"));
                assert_eq!(
                    suite.steps[0].exit, None,
                    "no spawned build: never reuse an earlier child status"
                );
                assert!(runner.finished().is_err());
            } else {
                runner.finished().unwrap();
            }
            // No fixture worker remains; only then may scratch be removed.
        }
    }
}
